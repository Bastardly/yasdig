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
/// picture and a reply come to about 600; the rest is room. A storyboard is
/// a cast's looks and prompts and up to twenty shots: several times that.
fn n_predict(mode: Mode) -> u32 {
    if mode == Mode::Story { 3072 } else { 1024 }
}

pub fn store(cfg: &Config) -> ChatStore {
    ChatStore::new(&cfg.base_dir.join("work"), &cfg.user_dir())
}

/// The agent a chat runs on: the one it names, else Settings' default, else
/// the first installed.
pub fn agent_for(cfg: &Config, name: Option<&str>) -> Result<AgentModel> {
    let scan = agents::scan(&cfg.agent_dir());
    let default = crate::settings::Settings::load_or_default(&cfg.user_dir()).agent.agent;
    for n in [name, default.as_deref()].into_iter().flatten() {
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

/// Files a turn sees that aren't the chat's pictures — a shot's frames —
/// staged after them the same way.
fn stage_files(cfg: &Config, chat_id: &str, files: &[PathBuf], side: u32, out: &mut Staged) -> Result<()> {
    let dir = crate::pipeline::scratch_dir(&cfg.base_dir.join("work"));
    let at = out.0.len();
    for (k, f) in files.iter().enumerate() {
        let png = shrink_png(&std::fs::read(f).with_context(|| format!("reading {}", f.display()))?, side)?;
        let p = dir.join(format!("agent-{chat_id}-{}.png", at + k));
        std::fs::write(&p, png)?;
        out.0.push(p);
    }
    Ok(())
}

/// The frames a take is judged on: the last of the shot before when this
/// one goes on from it, then this one's first, middle and last.
fn take_frames(cfg: &Config, f: &chat::Film) -> Result<Vec<PathBuf>> {
    let id = f.clip.as_deref().ok_or_else(|| anyhow!("the film has no clip yet"))?;
    let videos = crate::videos::VideoStore::new(&cfg.base_dir.join("work"), &cfg.user_dir());
    let clip = videos.load(id)?;
    let k = f.shot;
    let seg = clip.segments.get(k).ok_or_else(|| anyhow!("the clip has no shot {}", k + 1))?;
    let end = clip.segments.get(k + 1).map_or(clip.frames, |s| s.start).max(seg.start + 1);
    let mut out = Vec::new();
    if k > 0 && !seg.cut && seg.start > 0 {
        out.push(videos.frame_path(id, seg.start - 1));
    }
    for n in [seg.start, (seg.start + end - 1) / 2, end - 1] {
        out.push(videos.frame_path(id, n));
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
    let step_now = session::next_step(&chat);
    let result = match step_now {
        Step::End => {
            store.update(id, |c| {
                c.session.state = session::finished(c);
                c.session.job = None;
                Ok(())
            })?;
            return Ok(false);
        }
        Step::Turn(mode) => {
            queue.relabel(job, label(match mode {
                Mode::Chat => "reply",
                Mode::Plan => "planning",
                Mode::Critique if chat.session.sweep.is_some() => "comparing",
                Mode::Critique => "looking at the round",
                Mode::Describe => "describing",
                Mode::Verdict => "writing the answer",
                Mode::Story => "writing the storyboard",
                Mode::Take => "watching the shot",
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
        Step::Shot(k) => {
            let n = chat.film.as_ref().map_or(0, |f| f.shots.len());
            queue.relabel(job, label(&format!("filming shot {} of {n}", k + 1)));
            shoot(engine, &store, &chat, k)
        }
        Step::CastPicture(_) | Step::CastSheet(_) | Step::Keyframe(_) => {
            queue.relabel(job, label(&board_label(&chat, step_now)));
            board_picture(engine, &store, &chat, step_now)
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
    // A storyboard is written for the video model as much as the image one.
    let video_family = tk.video.as_ref().map(|v| v.family).filter(|_| mode == Mode::Story);
    let families: Vec<&str> = tk.family.into_iter().chain(video_family).collect();
    let (skill_text, used, _dropped) = skills::select(&all_skills, &families, mode);

    // What it looks at: a session's reference first (the plan and every
    // critique judge against it), then the round to judge, or what the
    // newest message attached.
    let reference: Vec<ImageRef> = match mode {
        Mode::Plan | Mode::Critique => chat.session.reference.clone(),
        // A take: the cast as they should look.
        Mode::Take => chat
            .film
            .as_ref()
            .map(|f| session::take_cast(f, f.shot).iter().filter_map(|m| m.picture.clone()).collect())
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    // And the shot's frames, from the clip.
    let frames = match (mode, &chat.film) {
        (Mode::Take, Some(f)) => take_frames(&cfg, f)?,
        _ => Vec::new(),
    };
    let take_checks = chat.film.as_ref().map(|f| session::take_checks(f, f.shot)).unwrap_or_default();
    let checks: &[String] = if mode == Mode::Take { &take_checks } else { &chat.session.checks };
    let own: Vec<ImageRef> = match mode {
        Mode::Critique => chat
            .messages
            .iter()
            .rev()
            .find_map(|m| match m {
                Msg::Batch { round, images, .. } if *round == chat.session.round => Some(images.clone()),
                _ => None,
            })
            .unwrap_or_default(),
        Mode::Chat | Mode::Describe | Mode::Story => chat
            .messages
            .iter()
            .rev()
            .find_map(|m| match m {
                Msg::User { images, .. } => Some(images.clone()),
                _ => None,
            })
            .unwrap_or_default(),
        Mode::Plan | Mode::Verdict | Mode::Take => Vec::new(),
    };
    let images: Vec<ImageRef> = reference.iter().cloned().chain(own.iter().cloned()).collect();
    if (!images.is_empty() || !frames.is_empty()) && !model.vision() {
        bail!(
            "{} has no vision projector, so it can't see pictures — pick one that can beside the message box",
            model.label
        );
    }
    if mode == Mode::Describe && images.is_empty() {
        bail!("there is no picture to describe — attach one first");
    }
    // A plan that will judge the round just drawn sees no pictures yet, but
    // its schema knows the round is there (no prompt is required of it).
    let sweep = chat.session.sweep.as_ref().filter(|_| matches!(mode, Mode::Plan | Mode::Critique | Mode::Verdict));
    let shown = match mode {
        Mode::Plan => session::judgeable(chat).map_or(0, |(_, size)| size),
        Mode::Verdict => sweep.map_or(0, |s| s.values.len() as u32),
        Mode::Take => frames.len() as u32,
        _ => own.len() as u32,
    };
    let mut tk = tk;
    tk.vision = model.vision();
    let schema = schema::output(mode, &tk, shown, checks.len() as u32, sweep.is_some());
    let n_predict = n_predict(mode);
    let budget = (tune.n_ctx.saturating_sub(n_predict) as usize).saturating_sub(64);

    let runner = LlmRunner::watched(&cfg, engine.monitor());
    let mut side = tune.image_side;
    let mut history_cap = usize::MAX;
    let reply = loop {
        let mut staged = stage(&cfg, store, &chat.id, &images, side)?;
        stage_files(&cfg, &chat.id, &frames, side, &mut staged)?;
        let base = context::Turn {
            mode,
            images: staged.0.clone(),
            reference: reference.len(),
            sweep: sweep.is_some(),
            skills: &skill_text,
            checks,
            goal: &chat.session.goal,
            round: chat.session.round,
            history: history_cap,
            film: chat.film.as_ref(),
        };
        let keep = context::fit_history(chat, &tk, &base, side, budget).min(history_cap);
        let turn = context::Turn { history: keep, ..base.clone() };
        let req = Request {
            v: llm::PROTOCOL,
            model: model.lm.clone(),
            mmproj: model.mmproj.clone(),
            n_ctx: tune.n_ctx,
            n_predict,
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
        bail!("the agent's reply ran past {n_predict} tokens and was cut off");
    }
    let parsed = session::parse_reply(&reply.content)?;
    let stats = reply.stats.clone();
    store.update(&chat.id, |c| {
        // Re-resolved on the chat as it is now: the person may have changed
        // Settings → Agent while the turn ran.
        let mut tk = toolkit::resolve(&cfg, c);
        tk.vision = model.vision();
        let locks = tk.locks.clone();
        session::apply_reply(c, mode, parsed, &tk, &locks, used.clone(), stats.clone());
        Ok(())
    })
}

/// Picture `k` of round `round`: the chat's settings through `/generate`'s
/// own conversion, into the agent's slot.
fn picture(engine: &mut Engine, store: &ChatStore, chat: &chat::Chat, round: u32, k: u32) -> Result<()> {
    let mut settings = chat.settings.clone();
    with_turbo(engine.cfg(), &mut settings);
    // A chat whose prompt the agent may not write draws the model's empty one.
    if let Some(o) = settings.as_object_mut() {
        o.entry("prompt").or_insert("".into());
    }
    let mut dto: crate::GenerateDto =
        serde_json::from_value(settings).context("the chat's settings aren't a generate request")?;
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

/// The queue's line for a picture of the board.
fn board_label(chat: &chat::Chat, step: Step) -> String {
    let Some(f) = &chat.film else { return "the storyboard".into() };
    match step {
        Step::CastPicture(k) => format!("cast {}/{}: {}", k + 1, f.cast.len(), f.cast.get(k).map_or("", |m| m.name.as_str())),
        Step::CastSheet(k) => format!("turnaround {}/{}: {}", k + 1, f.cast.len(), f.cast.get(k).map_or("", |m| m.name.as_str())),
        Step::Keyframe(k) => format!("keyframe of shot {}", k + 1),
        _ => "the storyboard".into(),
    }
}

/// The film's video model as the board describes it: its size and length.
fn film_video(cfg: &Config, film: &chat::Film) -> Option<toolkit::VideoView> {
    cfg.video_models().into_iter().find(|m| m.name == film.video_model).map(|m| toolkit::VideoView::of(&m))
}

/// Film shot `k`: shot 1 makes the clip — a retake of it a new one, the
/// take it replaces deleted — and a later one extends it, or redoes itself
/// (`extend_clip` keeps the shots before). On the Video tab's model with its
/// saved defaults, and its Turbo LoRA when installed, as the tab's switch is
/// on by default; the agent picks no size, steps or length.
fn shoot(engine: &mut Engine, store: &ChatStore, chat: &chat::Chat, k: usize) -> Result<()> {
    let cfg = engine.cfg().clone();
    let f = chat.film.as_ref().ok_or_else(|| anyhow!("the chat has no storyboard"))?;
    let seed = (rand_seed() & 0x7fff_ffff) as i64;
    let spec = session::shot_spec(f, k, seed);
    let every = (!f.every.is_empty()).then(|| f.every.clone());
    let r = match (k, &f.clip) {
        (0, old) => {
            let mut req = crate::pipeline::VideoRequest { model: f.video_model.clone(), shots: vec![spec], every, ..Default::default() };
            if !f.references {
                req.source = f.shots[0].key_stem.clone();
            }
            with_video_turbo(&cfg, &mut req);
            let r = engine.video(&req)?;
            if let Some(old) = old.as_ref().filter(|o| **o != r.clip.id) {
                let videos = crate::videos::VideoStore::new(&cfg.base_dir.join("work"), &cfg.user_dir());
                if let Err(e) = videos.delete(Shelf::Work, old) {
                    eprintln!("[yasdig] agent: could not delete the take {old} was replaced by {}: {e:#}", r.clip.id);
                }
            }
            r
        }
        (_, Some(id)) => engine.extend_clip(&crate::pipeline::ExtendClipRequest {
            id: id.clone(),
            keep: k,
            shots: vec![spec],
            every,
            negative: None,
        })?,
        (_, None) => bail!("shot {} has no clip to go on from", k + 1),
    };
    // A retake that failed leaves the clip shorter, with the reason.
    if r.clip.segments.len() <= k {
        bail!("{}", r.note.unwrap_or_else(|| format!("shot {} wasn't filmed", k + 1)));
    }
    store.update(&chat.id, |c| {
        session::shot_done(c, k, seed, &r.clip.id);
        if let Some(n) = &r.note {
            c.note(n.clone());
        }
        Ok(())
    })
}

/// The video model's catalog Turbo LoRA, when installed, at the steps and
/// CFG it was distilled to.
fn with_video_turbo(cfg: &Config, req: &mut crate::pipeline::VideoRequest) {
    let Some(family) = cfg.video_models().into_iter().find(|m| m.name == req.model).map(|m| m.family) else { return };
    let turbo = Catalog::get().lora.iter().find(|l| {
        l.family == crate::catalog::Family::Video(family)
            && std::fs::metadata(cfg.lora_dir().join(&l.dest)).is_ok_and(|m| m.is_file() && m.len() == l.bytes)
    });
    if let Some(l) = turbo {
        req.loras = Some(vec![crate::config::VideoLora { name: l.dest.clone(), strength: 1.0, expert: Default::default() }]);
        req.steps = Some(l.turbo.steps);
        req.cfg_scale = Some(l.turbo.cfg_scale);
    }
}

/// How long one of the film's shots runs, at most, in seconds.
pub fn shot_seconds(cfg: &Config, film: &chat::Film) -> f32 {
    film_video(cfg, film).map_or(0.0, |v| v.shot_seconds())
}

/// A cast member's picture or a shot's keyframe: the person's own saved to
/// Outputs, or one drawn and saved there — a video shot's references and
/// first frames are output stems.
fn board_picture(engine: &mut Engine, store: &ChatStore, chat: &chat::Chat, step: Step) -> Result<()> {
    let cfg = engine.cfg().clone();
    let film = chat.film.as_ref().ok_or_else(|| anyhow!("the chat has no storyboard"))?;
    let video = film_video(&cfg, film);
    let secs = video.as_ref().map_or(0.0, toolkit::VideoView::shot_seconds);
    let settings = board_settings(chat, film);
    let ms = chat::now_ms();
    let (picture, stem) = match step {
        Step::CastPicture(k) => {
            let m = film.cast.get(k).ok_or_else(|| anyhow!("that cast member is gone"))?;
            match (&m.picture, &m.sheet_stem) {
                (Some(p), _) => (p.clone(), picture_output(&cfg, store, &chat.id, p)?),
                // Drawn from its own sheet, so the two are one person.
                (None, Some(sheet)) => {
                    let pic = Pic {
                        prompt: with_look(&format!("{FRONT} {}", m.look), &film.every),
                        size: None,
                        refs: vec![sheet.clone()],
                        file: format!("c{ms}-{k}.png"),
                        reason: format!("cast: {}", m.name),
                    };
                    draw(engine, store, chat, &settings, &pic)?
                }
                (None, None) => {
                    let words = if m.prompt.is_empty() { &m.look } else { &m.prompt };
                    let pic = Pic {
                        prompt: with_look(words, &film.every),
                        size: None,
                        refs: Vec::new(),
                        file: format!("c{ms}-{k}.png"),
                        reason: format!("cast: {}", m.name),
                    };
                    draw(engine, store, chat, &settings, &pic)?
                }
            }
        }
        Step::CastSheet(k) => {
            let m = film.cast.get(k).ok_or_else(|| anyhow!("that cast member is gone"))?;
            // The person's picture is who the sheet shows; else the look is.
            let refs: Vec<String> = m.stem.clone().filter(|_| m.theirs()).into_iter().collect();
            let who = if refs.is_empty() { m.look.clone() } else { format!("The person in the reference picture. {}", m.look) };
            let pic = Pic {
                prompt: with_look(&format!("{SHEET} {who}"), &film.every),
                size: board_size(&cfg, &settings, 2.0),
                refs,
                file: format!("t{ms}-{k}.png"),
                reason: format!("turnaround: {}", m.name),
            };
            draw(engine, store, chat, &settings, &pic)?
        }
        Step::Keyframe(k) => {
            let shot = film.shots.get(k).ok_or_else(|| anyhow!("that shot is gone"))?;
            let v = video.as_ref().ok_or_else(|| anyhow!("the film's video model '{}' isn't installed", film.video_model))?;
            // The cast's pictures as references where the image model takes
            // them — a character's sheet, which shows every side — and their
            // looks in words either way.
            let refs: Vec<String> = if film.sheets {
                shot.cast
                    .iter()
                    .filter_map(|n| {
                        let m = film.member(n)?;
                        m.sheet_stem.clone().or_else(|| m.stem.clone())
                    })
                    .take(crate::config::MAX_REFERENCES)
                    .collect()
            } else {
                Vec::new()
            };
            let mut prompt = with_look(&shot.text, &film.every);
            for n in &shot.cast {
                if let Some(m) = film.member(n).filter(|m| !m.look.is_empty()) {
                    prompt.push_str(&format!(" {}: {}", m.name, m.look));
                }
            }
            let pic = Pic {
                prompt,
                size: board_size(&cfg, &settings, v.width as f64 / v.height.max(1) as f64),
                refs,
                file: format!("k{ms}-{k}.png"),
                reason: format!("keyframe of shot {}", k + 1),
            };
            draw(engine, store, chat, &settings, &pic)?
        }
        _ => bail!("not a picture of the storyboard"),
    };
    store.update(&chat.id, |c| {
        session::board_picture_done(c, step, picture.clone(), stem.clone(), secs);
        Ok(())
    })
}

/// A character's turnaround: the angles the person asked for, one figure
/// each, so every later picture has a side to follow.
const SHEET: &str = "Character turnaround sheet: the same person shown four times side by side on one plain light grey \
                     background, full body and standing — front view, three-quarter view, side view in profile, back \
                     view. The same face, hair and clothes in every view, even light, no text.";

/// A front picture drawn from that sheet.
const FRONT: &str = "The person in the reference picture, alone, seen from the front, full body and standing, on a plain \
                     light grey background.";

/// A prompt with the film's look after it.
fn with_look(text: &str, every: &str) -> String {
    match (text.trim(), every.trim()) {
        (t, "") => t.to_string(),
        ("", e) => e.to_string(),
        (t, e) => format!("{}. {e}", t.trim_end_matches('.')),
    }
}

/// The settings a board's pictures are drawn on: the chat's, on the board's
/// image model when it has its own (the switch drops the other model's
/// steps, size and LoRAs, as a chat's model change does).
fn board_settings(chat: &chat::Chat, film: &chat::Film) -> serde_json::Value {
    let mut settings = chat.settings.clone();
    if !film.image_model.is_empty() {
        if let Some(k) = knobs::knob("model") {
            knobs::write(&mut settings, k, &serde_json::Value::String(film.image_model.clone()));
        }
    }
    settings
}

/// A picture's size at `aspect` (width over height): the image model's own
/// pixel count, so it draws at the size it knows. A clip fits a keyframe
/// to its own size.
fn board_size(cfg: &Config, settings: &serde_json::Value, aspect: f64) -> Option<(u32, u32)> {
    let template = settings.get("template").and_then(|t| t.as_str()).unwrap_or("");
    let p = cfg.registry().get(template).cloned()?;
    let get = |k: &str, d: u32| settings.get(k).and_then(|v| v.as_u64()).map_or(d, |v| v as u32);
    let area = (get("width", p.width) as f64) * (get("height", p.height) as f64);
    let snap = |x: f64| ((x / 64.0).round() as u32).max(4) * 64;
    let w = snap((area * aspect).sqrt());
    Some((w, snap(area / w as f64)))
}

/// One of the board's pictures: what it is drawn from, and where it is kept.
struct Pic {
    prompt: String,
    size: Option<(u32, u32)>,
    /// Output stems.
    refs: Vec<String>,
    /// Its name under the chat's `img/`.
    file: String,
    /// What the work item's Info says it was for.
    reason: String,
}

/// One picture on `settings`, kept in the chat and saved to Outputs.
/// Returns it and its stem.
fn draw(engine: &mut Engine, store: &ChatStore, chat: &chat::Chat, settings: &serde_json::Value, pic: &Pic) -> Result<(ImageRef, String)> {
    let mut settings = settings.clone();
    with_turbo(engine.cfg(), &mut settings);
    // A new chat's settings are its model alone, with no prompt yet.
    if let Some(o) = settings.as_object_mut() {
        o.insert("prompt".into(), pic.prompt.clone().into());
    }
    let mut dto: crate::GenerateDto =
        serde_json::from_value(settings).context("the chat's settings aren't a generate request")?;
    dto.seed = Some((rand_seed() & 0x7fff_ffff) as i64);
    dto.count = Some(1);
    dto.looping = false;
    dto.update_only = false;
    dto.after = Default::default();
    if let Some((w, h)) = pic.size {
        dto.width = Some(w);
        dto.height = Some(h);
    }
    if !pic.refs.is_empty() {
        dto.references = Some(pic.refs.clone());
    }
    let (plan, _) = dto.run_plan();
    let crate::Start::Generate(req) = plan.start else {
        bail!("a storyboard picture is a generate");
    };
    let r = engine.run_in(&req, false, Slot::Agent)?;
    let work_id = r.work_id.clone().ok_or_else(|| anyhow!("the picture wasn't archived"))?;
    let items = crate::workitems::Store::new(&engine.cfg().base_dir.join("work"), &engine.cfg().user_dir());
    let trace = crate::workitems::AgentTrace { chat: chat.id.clone(), title: chat.title.clone(), round: 0, reason: pic.reason.clone() };
    if let Err(e) = items.set_agent(&work_id, trace) {
        eprintln!("[yasdig] agent: could not mark work item {work_id} as the agent's: {e:#}");
    }
    keep_picture(store, &chat.id, &pic.file, &r.png)?;
    let picture = ImageRef { kind: "item".into(), id: work_id.clone(), file: pic.file.clone(), seed: Some(r.seed) };
    let stem = picture_output(engine.cfg(), store, &chat.id, &picture)?;
    Ok((picture, stem))
}

/// A chat's picture as an output, saved there if it isn't one: its stem.
fn picture_output(cfg: &Config, store: &ChatStore, chat_id: &str, p: &ImageRef) -> Result<String> {
    let items = crate::workitems::Store::new(&cfg.base_dir.join("work"), &cfg.user_dir());
    match p.kind.as_str() {
        "output" => Ok(p.id.clone()),
        "item" => {
            let shelf = if items.image_path(Shelf::Work, &p.id).is_file() { Shelf::Work } else { Shelf::Favorites };
            let item = items.load(shelf, &p.id).with_context(|| format!("work item {} is gone", p.id))?;
            let embed = crate::settings::Settings::load_or_default(&cfg.user_dir()).embed_png_settings;
            let path = items.save_output(&items.image_path(shelf, &p.id), &item, embed)?;
            Ok(path.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_string())
        }
        _ => {
            // An upload from before uploads were outputs: the chat's copy.
            let bytes = picture_bytes(cfg, store, chat_id, p)?;
            let img = image::load_from_memory(&bytes).context("reading a picture")?;
            items.import(&bytes, img.width(), img.height())
        }
    }
}

/// A chat that names no LoRAs draws with its model's catalog Turbo LoRA when
/// it is installed, at the steps, CFG and schedule it was distilled to — as
/// the Image tab does with Turbo on, its default. Like that switch it only
/// sets them: steps or CFG the agent has set are kept (`toolkit::locks_for`
/// tells it what the LoRA was tuned for, and locks nothing), and a fixed
/// schedule goes only with the LoRA's own steps. A chat begun on the Image
/// tab's whole body (before chats started from the model alone) carries its
/// own `loras` and is left as it is.
fn with_turbo(cfg: &Config, settings: &mut serde_json::Value) {
    let Some(obj) = settings.as_object_mut() else { return };
    if obj.contains_key("loras") {
        return;
    }
    let template = obj.get("template").and_then(|t| t.as_str()).unwrap_or("");
    let Some(family) = cfg.registry().get(template).map(|p| p.family) else { return };
    let Some(l) = toolkit::turbo_lora(cfg, family) else { return };
    obj.insert("loras".into(), serde_json::json!([{ "name": l.dest, "strength": 1.0 }]));
    let own_steps = !obj.contains_key("steps");
    obj.entry("steps").or_insert(l.turbo.steps.into());
    obj.entry("cfg_scale").or_insert(serde_json::json!(l.turbo.cfg_scale));
    if let (Some(s), true) = (&l.turbo.sigmas, own_steps) {
        obj.insert("sigmas".into(), s.clone().into());
    }
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
                    "The {} pass is done. It is a History step on the picture: click the picture to open it.",
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

#[cfg(test)]
mod tests {
    /// A new chat holds only its model, so a round is the model's saved
    /// defaults (`Engine::run`'s registry profile) plus what the agent set.
    #[test]
    fn a_round_of_a_new_chat_overrides_nothing_it_was_not_told_to() {
        let dto: crate::GenerateDto = serde_json::from_value(serde_json::json!({"template": "sdxl", "prompt": "a fox"})).unwrap();
        let (plan, _) = dto.run_plan();
        let crate::Start::Generate(req) = plan.start else { panic!("a generate") };
        assert_eq!(req.template, "sdxl");
        assert_eq!((req.steps, req.cfg_scale, req.width, req.height), (None, None, None, None));
        assert!(req.loras.is_none(), "no LoRA list: the profile's, and Turbo's to add");
    }
}
