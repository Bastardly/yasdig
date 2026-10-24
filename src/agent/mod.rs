//! The Agent tab: a local vision-language model that helps with prompts and
//! settings, describes pictures as prompts, and runs a bounded generate →
//! critique → revise loop over a chat's own copy of the Image tab's settings.
//!
//! One turn is one run of the llama.cpp driver ([`crate::llm`]) on the job
//! queue — nothing stays loaded between turns, so sd-cli has the whole card
//! the moment a turn is done. A chat's work is one queue entry, a *leg*, that
//! the worker hands [`step`] a step at a time: a turn, one picture of a round,
//! a pass on a keeper. What the next step is, and what a reply does, is
//! [`session`]'s, decided without a GPU.

pub mod chat;
pub mod context;
pub mod knobs;
pub mod routes;
pub mod schema;
pub mod session;
pub mod skills;
pub mod toolkit;

use crate::agents::{self, AgentModel};
use crate::catalog::Catalog;
use crate::config::Config;
use crate::llm::{self, LlmRunner, Request, Sampling};
use crate::pipeline::{EditRequest, Engine, EscalateRequest, Slot};
use crate::workitems::Shelf;
use anyhow::{anyhow, bail, Context as _, Result};
use chat::{ActionState, ChatStore, ImageRef, Mode, Msg, Next, State};
use session::Step;
use std::path::PathBuf;

/// Tokens a reply may run to. A critique's notes, a row of answers per
/// picture and a reply come to about 600; the rest is room.
const N_PREDICT: u32 = 1024;

pub fn store(cfg: &Config) -> ChatStore {
    ChatStore::new(&cfg.base_dir.join("work"), &cfg.user_dir())
}

/// The agent a chat runs on: the one it names, else the first installed.
pub fn agent_for(cfg: &Config, name: Option<&str>) -> Result<AgentModel> {
    let scan = agents::scan(&cfg.agent_dir());
    if let Some(n) = name {
        if let Some(m) = scan.models.iter().find(|m| m.name == n) {
            return Ok(m.clone());
        }
    }
    scan.models.into_iter().next().ok_or_else(|| {
        anyhow!(
            "no agent model is installed — Get models → Agent models has some, or put a GGUF and its \
             mmproj in a folder of their own under {}",
            cfg.agent_dir().display()
        )
    })
}

/// How a turn runs on `m`: the catalog card's numbers where the file is one
/// of its downloads, the file's own sampling where it says, the defaults else.
pub struct Tuning {
    pub n_ctx: u32,
    pub image_side: u32,
    pub sampling: Sampling,
}

pub fn tuning(cfg: &Config, m: &AgentModel) -> Tuning {
    let card = Catalog::get().agent_for(&cfg.agent_dir(), &m.lm);
    let mut sampling = Sampling::default();
    if let Some(s) = card.and_then(|c| c.sampling) {
        sampling = Sampling { temp: s.temp, top_p: s.top_p, top_k: s.top_k, min_p: s.min_p, seed: 0 };
    } else {
        sampling.temp = m.meta.temp.unwrap_or(sampling.temp);
        sampling.top_p = m.meta.top_p.unwrap_or(sampling.top_p);
        sampling.top_k = m.meta.top_k.unwrap_or(sampling.top_k);
    }
    let n_ctx = card.map_or(8192, |c| c.n_ctx);
    Tuning {
        // Never past what the model was trained to.
        n_ctx: m.n_ctx_train.map_or(n_ctx, |t| n_ctx.min(t as u32)),
        image_side: card.map_or(512, |c| c.image_side),
        sampling,
    }
}

/// A picture shrunk to `side` on its longest edge (never enlarged), as PNG.
pub fn shrink_png(src: &[u8], side: u32) -> Result<Vec<u8>> {
    let img = image::load_from_memory(src).context("reading a picture")?;
    let img = if img.width().max(img.height()) > side {
        img.resize(side, side, image::imageops::FilterType::Lanczos3)
    } else {
        img
    };
    let mut out = Vec::new();
    img.to_rgb8().write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)?;
    Ok(out)
}

/// The bytes behind one of a chat's pictures, at their best: a work item's
/// archived (unsharpened) PNG while it exists, else the chat's own copy.
fn picture_bytes(cfg: &Config, store: &ChatStore, chat_id: &str, img: &ImageRef) -> Result<Vec<u8>> {
    let items = crate::workitems::Store::new(&cfg.base_dir.join("work"), &cfg.user_dir());
    if img.kind == "item" {
        for shelf in [Shelf::Work, Shelf::Favorites] {
            let p = items.image_path(shelf, &img.id);
            if let Ok(b) = std::fs::read(&p) {
                return Ok(b);
            }
        }
    }
    if img.kind == "output" {
        if let Some(b) = items.find_output(&img.id).and_then(|p| std::fs::read(p).ok()) {
            return Ok(b);
        }
    }
    let folder = if img.kind == "attach" { "att" } else { "img" };
    let p = store.file(chat_id, folder, &img.file).ok_or_else(|| anyhow!("a picture with a bad name"))?;
    std::fs::read(&p).with_context(|| format!("reading {}", p.display()))
}

/// Pictures a turn sees, copied into scratch at `side` (invariant 8: the
/// driver never reads a durable path, and on Windows `user/` isn't a folder
/// its sandbox may open). Removed when dropped.
struct Staged(Vec<PathBuf>);

impl Drop for Staged {
    fn drop(&mut self) {
        for p in &self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

fn stage(cfg: &Config, store: &ChatStore, chat_id: &str, imgs: &[ImageRef], side: u32) -> Result<Staged> {
    let dir = crate::pipeline::scratch_dir(&cfg.base_dir.join("work"));
    std::fs::create_dir_all(&dir)?;
    let mut out = Staged(Vec::new());
    for (k, img) in imgs.iter().enumerate() {
        let png = shrink_png(&picture_bytes(cfg, store, chat_id, img)?, side)?;
        let p = dir.join(format!("agent-{chat_id}-{k}.png"));
        std::fs::write(&p, png)?;
        out.0.push(p);
    }
    Ok(out)
}

/// The bytes of a picture a message is about to attach: a work item's or an
/// output's, at full size, for [`keep_picture`].
pub fn attach_bytes(cfg: &Config, kind: &str, id: &str) -> Result<Vec<u8>> {
    if !crate::workitems::valid_id(id) {
        bail!("'{id}' is not a picture id");
    }
    let items = crate::workitems::Store::new(&cfg.base_dir.join("work"), &cfg.user_dir());
    let path = match kind {
        "item" => [Shelf::Work, Shelf::Favorites].into_iter().map(|s| items.image_path(s, id)).find(|p| p.is_file()),
        "output" => items.find_output(id),
        _ => bail!("a picture is a work item or an output, not '{kind}'"),
    };
    let path = path.ok_or_else(|| anyhow!("that picture is gone"))?;
    std::fs::read(&path).with_context(|| format!("reading {}", path.display()))
}

/// Keep a picture in the chat: its own copy under `img/`, and the tile.
pub fn keep_picture(store: &ChatStore, chat_id: &str, name: &str, png: &[u8]) -> Result<()> {
    let dir = store.dir(chat_id);
    std::fs::create_dir_all(dir.join("img"))?;
    std::fs::write(dir.join("img").join(name), shrink_png(png, chat::PREVIEW_SIDE)?)?;
    std::fs::write(store.thumb_path(chat_id), shrink_png(png, 256)?)?;
    Ok(())
}

/// One step of chat `id`'s leg, for the worker. `Ok(true)` goes on (the
/// queue puts the leg back, behind anything waiting), `Ok(false)` ends it.
pub fn step(engine: &mut Engine, queue: &crate::Jobs, id: &str, job: u64) -> Result<bool> {
    let cfg = engine.cfg().clone();
    let store = store(&cfg);
    if queue.stopping(job) {
        end_stopped(&store, id);
        return Ok(false);
    }
    // A round that hasn't begun is begun here, where its seed is drawn.
    let mut chat = store.update(id, |c| {
        if session::needs_round(c) {
            session::start_round(c, (rand_seed() & 0x7fff_ffff) as i64);
        }
        c.session.state = State::Running;
        c.session.job = Some(job);
        Ok(c.clone())
    })?;
    let label = |what: &str| {
        let title = if chat.title.is_empty() { "Agent".to_string() } else { format!("Agent · {}", chat.title) };
        format!("{title} · {what}")
    };
    let result = match session::next_step(&chat) {
        Step::End => {
            store.update(id, |c| {
                c.session.state = if c.pending() { State::Awaiting } else { session::finished(c) };
                c.session.job = None;
                Ok(())
            })?;
            return Ok(false);
        }
        Step::Turn(mode) => {
            queue.relabel(job, label(match mode {
                Mode::Chat => "reply",
                Mode::Plan => "planning",
                Mode::Critique => "looking at the round",
                Mode::Describe => "describing",
            }));
            turn(engine, &store, &mut chat, mode)
        }
        Step::Image { round, k } => {
            queue.relabel(job, label(&format!("round {round}, picture {}/{}", k + 1, session::round_size(&chat))));
            picture(engine, &store, &chat, round, k)
        }
        Step::Escalate(action) => {
            queue.relabel(job, label("finishing a keeper"));
            escalate(engine, &store, &chat, action)
        }
    };
    match result {
        Ok(()) => {
            let c = store.load(id)?;
            let more = session::next_step(&c) != Step::End && !queue.stopping(job);
            if !more {
                store.update(id, |c| {
                    c.session.state = if queue.stopping(job) {
                        State::Stopped
                    } else if c.pending() {
                        State::Awaiting
                    } else {
                        session::finished(c)
                    };
                    c.session.job = None;
                    Ok(())
                })?;
            }
            Ok(more)
        }
        Err(e) if queue.stopping(job) || e.to_string().ends_with(crate::sd::CANCELLED) => {
            end_stopped(&store, id);
            Ok(false)
        }
        Err(e) => {
            let msg = format!("{e:#}");
            let _ = store.update(id, |c| {
                c.session.state = State::Failed;
                c.session.job = None;
                c.session.error = Some(msg.clone());
                c.note(format!("Stopped on an error: {msg}"));
                Ok(())
            });
            Err(e)
        }
    }
}

fn end_stopped(store: &ChatStore, id: &str) {
    let _ = store.update(id, |c| {
        c.session.state = State::Stopped;
        c.session.job = None;
        c.note("Stopped.");
        Ok(())
    });
}

fn rand_seed() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(chat::now_ms() as u128);
    h.finish()
}

/// One turn of the agent.
fn turn(engine: &mut Engine, store: &ChatStore, chat: &mut chat::Chat, mode: Mode) -> Result<()> {
    let cfg = engine.cfg().clone();
    let model = agent_for(&cfg, chat.agent.as_deref())?;
    let tune = tuning(&cfg, &model);
    let tk = toolkit::resolve(&cfg, chat);
    let (all_skills, _) = skills::load(&user_skills_dir(&cfg));
    let (skill_text, used, _dropped) = skills::select(&all_skills, tk.family, mode);

    // What it looks at: the round to judge, or what the newest message attached.
    let images: Vec<ImageRef> = match mode {
        Mode::Critique => chat
            .messages
            .iter()
            .rev()
            .find_map(|m| match m {
                Msg::Batch { round, images, .. } if *round == chat.session.round => Some(images.clone()),
                _ => None,
            })
            .unwrap_or_default(),
        Mode::Chat | Mode::Describe => chat
            .messages
            .iter()
            .rev()
            .find_map(|m| match m {
                Msg::User { images, .. } => Some(images.clone()),
                _ => None,
            })
            .unwrap_or_default(),
        Mode::Plan => Vec::new(),
    };
    if !images.is_empty() && !model.vision() {
        bail!(
            "{} has no vision projector, so it can't see pictures — pick an agent that can in the Model panel",
            model.label
        );
    }
    if mode == Mode::Describe && images.is_empty() {
        bail!("there is no picture to describe — attach one first");
    }
    // A plan that will judge the round just drawn sees no pictures yet, but
    // its schema knows the round is there (no prompt is required of it).
    let shown = match mode {
        Mode::Plan => session::judgeable(chat).map_or(0, |(_, size)| size),
        _ => images.len() as u32,
    };
    let schema = schema::output(mode, &tk, shown, chat.session.checks.len() as u32);
    let budget = (tune.n_ctx.saturating_sub(N_PREDICT) as usize).saturating_sub(64);

    let runner = LlmRunner::watched(&cfg, engine.monitor());
    let mut side = tune.image_side;
    let mut history_cap = usize::MAX;
    let reply = loop {
        let staged = stage(&cfg, store, &chat.id, &images, side)?;
        let base = context::Turn {
            mode,
            images: staged.0.clone(),
            skills: &skill_text,
            checks: &chat.session.checks,
            goal: &chat.session.goal,
            round: chat.session.round,
            history: history_cap,
        };
        let keep = context::fit_history(chat, &tk, &base, side, budget).min(history_cap);
        let turn = context::Turn { history: keep, ..base.clone() };
        let req = Request {
            v: llm::PROTOCOL,
            model: model.lm.clone(),
            mmproj: model.mmproj.clone(),
            n_ctx: tune.n_ctx,
            n_predict: N_PREDICT,
            thinking: false,
            sampling: Sampling { seed: (rand_seed() & 0xffff_ffff) as u32, ..tune.sampling },
            json_schema: Some(schema.clone()),
            messages: context::messages(chat, &tk, &turn),
        };
        match runner.run(&req, &model.label, &chat.id) {
            Ok(r) => break r,
            // Once, with half the history and smaller pictures: the estimate
            // is an estimate, and the template's own wrapping varies.
            Err(e) if e.downcast_ref::<llm::ContextOverflow>().is_some() && history_cap == usize::MAX => {
                history_cap = (keep / 2).max(1);
                side = (side * 3 / 4).max(224);
            }
            Err(e) => return Err(e),
        }
    };
    if reply.stats.stop == "length" {
        bail!("the agent's reply ran past {N_PREDICT} tokens and was cut off");
    }
    let parsed = session::parse_reply(&reply.content)?;
    let stats = reply.stats.clone();
    store.update(&chat.id, |c| {
        // Re-resolved on the chat as it is now: the person may have changed
        // its settings or permissions while the turn ran.
        let tk = toolkit::resolve(&cfg, c);
        let locks = tk.locks.clone();
        session::apply_reply(c, mode, parsed, &tk, &locks, used.clone(), stats.clone());
        Ok(())
    })
}

/// Picture `k` of round `round`: the chat's settings through `/generate`'s
/// own conversion, into the agent's slot.
fn picture(engine: &mut Engine, store: &ChatStore, chat: &chat::Chat, round: u32, k: u32) -> Result<()> {
    let mut dto: crate::GenerateDto =
        serde_json::from_value(chat.settings.clone()).context("the chat's settings aren't a generate request")?;
    dto.seed = Some(session::seed_for(chat, k));
    dto.count = Some(1);
    dto.looping = false;
    dto.update_only = false;
    dto.after = Default::default();
    let (plan, _) = dto.run_plan();
    let crate::Start::Generate(req) = plan.start else {
        bail!("an agent round is a generate");
    };
    let r = engine.run_in(&req, false, Slot::Agent)?;
    let work_id = r.work_id.clone().ok_or_else(|| anyhow!("the picture wasn't archived"))?;
    let trace = crate::workitems::AgentTrace {
        chat: chat.id.clone(),
        title: chat.title.clone(),
        round,
        reason: session::round_reason(chat, round),
    };
    // The trace is a courtesy: a picture that lost it is still the picture.
    let items = crate::workitems::Store::new(&engine.cfg().base_dir.join("work"), &engine.cfg().user_dir());
    if let Err(e) = items.set_agent(&work_id, trace) {
        eprintln!("[yasdig] agent: could not mark work item {work_id} as the agent's: {e:#}");
    }
    let file = format!("r{round}-{k}.png");
    keep_picture(store, &chat.id, &file, &r.png)?;
    store.update(&chat.id, |c| {
        session::image_done(c, ImageRef { kind: "item".into(), id: work_id.clone(), file: file.clone(), seed: Some(r.seed) });
        Ok(())
    })
}

/// The pass a critique asked for, on its keeper.
fn escalate(engine: &mut Engine, store: &ChatStore, chat: &chat::Chat, action: u64) -> Result<()> {
    let a = chat.actions.iter().find(|a| a.id == action).cloned().ok_or_else(|| anyhow!("that pass is gone"))?;
    let mut run = || -> Result<crate::pipeline::GenResult> {
        let mut display = engine.display();
        let items = crate::workitems::Store::new(&engine.cfg().base_dir.join("work"), &engine.cfg().user_dir());
        // It may have been starred since.
        let shelf = if items.image_path(Shelf::Work, &a.item).is_file() { Shelf::Work } else { Shelf::Favorites };
        let opened = display.open_in(&a.item, shelf, Slot::Agent)?;
        let id = opened.image_id;
        match a.kind.as_str() {
            "hires" => engine.hires(id, 2.0, &EscalateRequest::default()),
            "refine" => engine.refine(id, &EscalateRequest::default()),
            "edit" => engine.edit(id, &EditRequest { prompt: a.instruction.clone(), ..Default::default() }),
            other => bail!("'{other}' is not a pass the agent runs"),
        }
    };
    match run() {
        Ok(r) => {
            let file = format!("x{action}.png");
            keep_picture(store, &chat.id, &file, &r.png)?;
            store.update(&chat.id, |c| {
                if let Some(x) = c.actions.iter_mut().find(|x| x.id == action) {
                    x.state = ActionState::Done;
                    x.result = Some(ImageRef { kind: "item".into(), id: x.item.clone(), file: file.clone(), seed: None });
                }
                if !c.actions.iter().any(|x| x.state == ActionState::Queued) {
                    c.session.next = Next::Nothing;
                }
                c.note(format!(
                    "The {} pass is done. It is a History step on the picture: open it from Current on the Image tab.",
                    a.kind
                ));
                Ok(())
            })
        }
        Err(e) => {
            let msg = format!("{e:#}");
            store.update(&chat.id, |c| {
                if let Some(x) = c.actions.iter_mut().find(|x| x.id == action) {
                    x.state = ActionState::Failed;
                    x.error = Some(msg.clone());
                }
                Ok(())
            })?;
            Err(e)
        }
    }
}

/// `yasdig llm-selftest`: one turn outside the server — the driver, the
/// sandbox, the GPU and the reply grammar, with nothing else in the way.
/// With a picture, the agent describes it; without, it answers in text.
pub fn selftest(cfg: &Config, name: Option<&str>, image: Option<&std::path::Path>) -> Result<()> {
    let model = agent_for(cfg, name)?;
    let tune = tuning(cfg, &model);
    println!(
        "[llm-selftest] {} ({}, {}) with {}",
        model.label,
        model.name,
        if model.vision() { "sees pictures" } else { "text only" },
        cfg.llm_binary().display()
    );
    let mut parts = Vec::new();
    let mut staged = Staged(Vec::new());
    if let Some(img) = image {
        if !model.vision() {
            bail!("{} has no vision projector to look at {}", model.label, img.display());
        }
        let png = shrink_png(&std::fs::read(img).with_context(|| format!("reading {}", img.display()))?, tune.image_side)?;
        let dir = crate::pipeline::scratch_dir(&cfg.base_dir.join("work"));
        std::fs::create_dir_all(&dir)?;
        let p = dir.join("agent-selftest.png");
        std::fs::write(&p, png)?;
        staged.0.push(p.clone());
        parts.push(llm::Part::Image { path: p });
        parts.push(llm::Part::Text { text: "Describe this picture in two sentences.".into() });
    } else {
        parts.push(llm::Part::Text { text: "In one sentence: what is a seed in image generation?".into() });
    }
    let schema = serde_json::json!({
        "type": "object",
        "properties": { "notes": { "type": "string", "maxLength": 300 }, "reply": { "type": "string", "maxLength": 600 } },
        "required": ["notes", "reply"],
        "additionalProperties": false
    });
    let req = Request {
        v: llm::PROTOCOL,
        model: model.lm.clone(),
        mmproj: model.mmproj.clone(),
        n_ctx: tune.n_ctx.min(4096),
        n_predict: 512,
        thinking: false,
        sampling: tune.sampling,
        json_schema: Some(schema),
        messages: vec![
            llm::Message::text(llm::Role::System, "You are a helpful assistant. Reply with one JSON object."),
            llm::Message { role: llm::Role::User, content: parts },
        ],
    };
    let monitor = crate::sd::Monitor::default();
    let t = std::time::Instant::now();
    let reply = LlmRunner::watched(cfg, &monitor).run(&req, "selftest", "selftest")?;
    let parsed = session::parse_reply(&reply.content);
    println!("[llm-selftest] reply: {}", reply.content);
    match parsed {
        Ok(r) => println!("[llm-selftest] parsed: {}", r.reply),
        Err(e) => println!("[llm-selftest] NOT the asked-for JSON: {e:#}"),
    }
    let s = &reply.stats;
    println!(
        "[llm-selftest] {} prompt + {} written tokens; load {} ms, read {} ms, write {} ms ({:.1} tok/s); {} s in all; stop: {}",
        s.n_prompt,
        s.n_gen,
        s.load_ms,
        s.read_ms,
        s.gen_ms,
        s.n_gen as f64 / (s.gen_ms.max(1) as f64 / 1000.0),
        t.elapsed().as_secs(),
        s.stop
    );
    Ok(())
}

/// The path of a chat-held file a route serves, or `None` for a bad name.
pub fn chat_file(cfg: &Config, id: &str, folder: &str, name: &str) -> Option<PathBuf> {
    store(cfg).file(id, folder, name)
}

pub fn user_skills_dir(cfg: &Config) -> PathBuf {
    cfg.user_dir().join("skills")
}
