//! The Agent tab's routes. Everything but `/agent/send` is a file read or
//! write and answers on the connection thread; that one queues a leg on the job queue and answer `{job}` at once — the reply arrives
//! in the chat, which the page re-reads as its `rev` moves.

use super::chat::{Chat, ChatStore, ImageRef, Mode, Msg, Next, Phase, State};
use super::{knobs, session, toolkit};
use crate::config::Config;
use crate::http::Connection;
use crate::sd::Monitor;
use crate::workitems::{valid_id, Shelf};
use crate::{error_json, jobs, parse_json, Lanes, Work};
use anyhow::Result;
use serde::Deserialize;
use serde_json::{json, Value};

/// The route for `method path`, or `None` when it isn't one of the agent's.
pub fn route(
    conn: &mut Connection,
    method: &str,
    path: &str,
    body: &[u8],
    cfg: &Config,
    lanes: &Lanes,
    monitor: &Monitor,
) -> Option<Result<()>> {
    Some(match (method, path) {
        ("GET", "/agents") => agents(conn, cfg),
        ("GET", "/chats") => listing(conn, cfg, Shelf::Work),
        ("GET", "/favorite-chats") => listing(conn, cfg, Shelf::Favorites),
        ("POST", "/chats/new") => new_chat(conn, cfg, body),
        ("POST", "/chats/delete") => delete(conn, cfg, body, Shelf::Work),
        ("POST", "/favorite-chats/delete") => delete(conn, cfg, body, Shelf::Favorites),
        ("POST", "/favorite-chats/add") => shelve(conn, cfg, body, true),
        ("POST", "/favorite-chats/remove") => shelve(conn, cfg, body, false),
        ("POST", "/chats/agent") => edit(conn, cfg, body, Edit::Agent),
        ("POST", "/chats/rename") => edit(conn, cfg, body, Edit::Rename),
        ("POST", "/chats/film") => film(conn, cfg, lanes, body),
        ("POST", "/agent/send") => send(conn, cfg, lanes, body),
        ("POST", "/agent/undo") => undo(conn, cfg, body),
        ("POST", "/agent/stop") => stop(conn, cfg, lanes, monitor, body),
        ("GET", p) if p.starts_with("/chats/") => {
            let rest = &p["/chats/".len()..];
            let parts: Vec<&str> = rest.split('/').collect();
            match parts.as_slice() {
                [id] => get_chat(conn, cfg, id),
                [id, "live"] => live(conn, monitor, id),
                [id, "thumb.png"] => file(conn, ChatStore::new(&work(cfg), &cfg.user_dir()).thumb_path(id), valid_id(id)),
                [id, folder @ ("img" | "att"), name] => match super::chat_file(cfg, id, folder, name) {
                    Some(p) => file(conn, p, true),
                    None => file(conn, Default::default(), false),
                },
                _ => return None,
            }
        }
        _ => return None,
    })
}

fn work(cfg: &Config) -> std::path::PathBuf {
    cfg.base_dir.join("work")
}

fn ok(conn: &mut Connection, v: Value) -> Result<()> {
    conn.respond(200, "application/json", v.to_string().as_bytes())
}

fn err(conn: &mut Connection, status: u16, msg: &str) -> Result<()> {
    conn.respond(status, "application/json", error_json(msg).as_bytes())
}

fn file(conn: &mut Connection, path: std::path::PathBuf, valid: bool) -> Result<()> {
    match valid.then(|| std::fs::read(&path).ok()).flatten() {
        Some(b) => conn.respond(200, "image/png", &b),
        None => conn.respond(404, "text/plain", b"not found"),
    }
}

fn agents(conn: &mut Connection, cfg: &Config) -> Result<()> {
    let scan = crate::agents::scan(&cfg.agent_dir());
    let vram = crate::sd::vram();
    let models: Vec<Value> = scan
        .models
        .iter()
        .map(|m| {
            let tune = super::tuning(cfg, m);
            json!({
                "name": m.name,
                "label": m.label,
                "arch": m.arch,
                "vision": m.vision(),
                "bytes": m.bytes,
                // The files plus a context's worth: room on the card or not.
                "fits": vram.map(|v| m.bytes + (1u64 << 30) <= v),
                "n_ctx": tune.n_ctx,
                "note": m.note,
            })
        })
        .collect();
    let driver = cfg.llm_binary().is_file();
    // What Settings → Agent draws its whitelist from.
    let knobs: Vec<Value> = knobs::KNOBS.iter().map(|k| json!({ "key": k.key, "label": k.label, "help": k.help })).collect();
    let actions: Vec<Value> = knobs::ACTIONS.iter().map(|a| json!({ "key": a.key, "label": a.label, "help": a.help })).collect();
    ok(conn, json!({
        "models": models,
        "driver": driver,
        "vram": vram,
        "dir": cfg.agent_dir().display().to_string(),
        "knobs": knobs,
        "actions": actions,
        "default": super::agent_for(cfg, None).ok().map(|m| m.name),
        "max_batch": knobs::MAX_BATCH,
        "max_rounds": knobs::MAX_ROUNDS,
    }))
}

fn summary(c: &Chat) -> Value {
    json!({
        "id": c.id,
        "title": if c.title.is_empty() { "New chat" } else { &c.title },
        "updated_ms": c.updated_ms,
        "created_ms": c.created_ms,
        "favorite": c.favorite,
        "state": c.session.state,
        "thumb": c.newest_image().is_some(),
    })
}

fn listing(conn: &mut Connection, cfg: &Config, shelf: Shelf) -> Result<()> {
    let list: Vec<Value> = super::store(cfg).listing(shelf).iter().map(summary).collect();
    ok(conn, Value::Array(list))
}

fn chat_json(cfg: &Config, c: &Chat) -> Value {
    json!({ "chat": c, "toolkit": toolkit::resolve(cfg, c), "favorite": c.favorite })
}

fn get_chat(conn: &mut Connection, cfg: &Config, id: &str) -> Result<()> {
    match super::store(cfg).load(id) {
        Ok(c) => ok(conn, chat_json(cfg, &c)),
        Err(e) => err(conn, 404, &format!("{e:#}")),
    }
}

/// The reply being written, while a turn of this chat runs.
fn live(conn: &mut Connection, monitor: &Monitor, id: &str) -> Result<()> {
    let p = monitor.snapshot();
    match monitor.live_text(id) {
        Some(t) => ok(conn, json!({
            "seq": t.seq,
            "text": t.text,
            "phase": p.as_ref().map(|p| p.phase.as_str()),
            "step": p.as_ref().map(|p| p.step),
            "steps": p.as_ref().map(|p| p.steps),
        })),
        None => ok(conn, json!({ "seq": null })),
    }
}

fn new_chat(conn: &mut Connection, cfg: &Config, body: &[u8]) -> Result<()> {
    #[derive(Deserialize)]
    struct Dto {
        /// The Image tab's model. The chat starts from its saved defaults
        /// (the registry's profile) and holds only what the agent sets after.
        template: String,
        #[serde(default)]
        agent: Option<String>,
        /// The Video tab's model: what a film is shot on.
        #[serde(default)]
        video: Option<String>,
    }
    let Some(dto) = parse_json::<Dto>(conn, body)? else { return Ok(()) };
    if !cfg.registry().contains_key(&dto.template) {
        return err(conn, 400, &format!("'{}' isn't an installed model", dto.template));
    }
    let agent = dto.agent.or_else(|| super::agent_for(cfg, None).ok().map(|m| m.name));
    let mut chat = Chat::new(super::chat::new_id(), json!({ "template": dto.template }), agent);
    chat.video = dto.video.filter(|v| !v.is_empty());
    let store = super::store(cfg);
    match store.create(&chat) {
        Ok(()) => ok(conn, chat_json(cfg, &chat)),
        Err(e) => err(conn, 500, &format!("{e:#}")),
    }
}

#[derive(Deserialize)]
struct Ids {
    ids: Vec<String>,
}

fn delete(conn: &mut Connection, cfg: &Config, body: &[u8], shelf: Shelf) -> Result<()> {
    let Some(dto) = parse_json::<Ids>(conn, body)? else { return Ok(()) };
    let store = super::store(cfg);
    let mut failed = Vec::new();
    for id in &dto.ids {
        // Not out from under the worker: a leg mid-turn would write it back.
        if store.load(id).is_ok_and(|c| c.session.busy()) {
            failed.push(format!("{id}: the agent is working on it — stop it first"));
            continue;
        }
        if let Err(e) = store.delete(shelf, id) {
            failed.push(format!("{id}: {e:#}"));
        }
    }
    if failed.is_empty() {
        ok(conn, json!({ "deleted": true }))
    } else {
        err(conn, 409, &failed.join("; "))
    }
}

fn shelve(conn: &mut Connection, cfg: &Config, body: &[u8], keep: bool) -> Result<()> {
    let Some(dto) = parse_json::<Ids>(conn, body)? else { return Ok(()) };
    let store = super::store(cfg);
    let mut failed = Vec::new();
    for id in &dto.ids {
        if store.load(id).is_ok_and(|c| c.session.busy()) {
            failed.push(format!("{id}: the agent is working on it — stop it first"));
            continue;
        }
        let moved = if keep { store.favorite(id) } else { store.unfavorite(id) };
        if let Err(e) = moved {
            failed.push(format!("{id}: {e:#}"));
        }
    }
    if failed.is_empty() {
        ok(conn, json!({ "moved": true }))
    } else {
        err(conn, 409, &failed.join("; "))
    }
}

enum Edit {
    Agent,
    Rename,
}

fn edit(conn: &mut Connection, cfg: &Config, body: &[u8], what: Edit) -> Result<()> {
    #[derive(Deserialize)]
    struct Dto {
        id: String,
        #[serde(default)]
        agent: Option<String>,
        #[serde(default)]
        title: Option<String>,
    }
    let Some(dto) = parse_json::<Dto>(conn, body)? else { return Ok(()) };
    let store = super::store(cfg);
    let result = store.update(&dto.id, |c| {
        match what {
            Edit::Agent => {
                let name = dto.agent.clone().ok_or_else(|| anyhow::anyhow!("no agent"))?;
                if !crate::agents::scan(&cfg.agent_dir()).models.iter().any(|m| m.name == name) {
                    anyhow::bail!("'{name}' isn't an installed agent model");
                }
                c.agent = Some(name);
            }
            Edit::Rename => {
                let t: String = dto.title.clone().unwrap_or_default().trim().chars().take(80).collect();
                c.title = t;
            }
        }
        Ok(())
    });
    match result {
        Ok(()) => get_chat(conn, cfg, &dto.id),
        Err(e) => err(conn, 400, &format!("{e:#}")),
    }
}

/// The person's edit of a storyboard, before it is filmed: one member's or
/// one shot's fields. A change may leave a picture missing — one dropped to
/// be drawn again, a keyframe's shot reworded; `draw` queues the leg that
/// draws whatever is.
fn film(conn: &mut Connection, cfg: &Config, lanes: &Lanes, body: &[u8]) -> Result<()> {
    #[derive(Deserialize)]
    struct Pic {
        kind: String,
        id: String,
    }
    #[derive(Deserialize)]
    struct Dto {
        id: String,
        #[serde(default)]
        member: Option<usize>,
        #[serde(default)]
        shot: Option<usize>,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        look: Option<String>,
        /// A picture of the person's (an output), or `null` to draw one again.
        #[serde(default, deserialize_with = "crate::double_option")]
        picture: Option<Option<Pic>>,
        #[serde(default)]
        text: Option<String>,
        #[serde(default)]
        cast: Option<Vec<String>>,
        #[serde(default)]
        cut: Option<bool>,
        /// Shot `shot`'s first frame drawn again (a click on it).
        #[serde(default)]
        redraw_keyframe: bool,
        /// Shot `shot` starts from its words (`true`), or a first frame again.
        #[serde(default)]
        no_keyframe: Option<bool>,
        /// H3's: shot `shot`'s spoken lines, its sound and its score.
        #[serde(default)]
        lines: Option<Vec<super::chat::Line>>,
        #[serde(default)]
        sound: Option<String>,
        #[serde(default)]
        music: Option<String>,
        /// A character's voice: a sound output's stem, or `null` for none.
        #[serde(default, deserialize_with = "crate::double_option")]
        voice: Option<Option<String>>,
        #[serde(default)]
        every: Option<String>,
        /// Draw what the board is missing (*Draw the cast*).
        #[serde(default)]
        draw: bool,
        /// The image model the board's pictures are drawn with.
        #[serde(default)]
        image_model: Option<String>,
        /// The video model it is shot on.
        #[serde(default)]
        video_model: Option<String>,
        /// Film it (*Film*), or go on filming after a stop.
        #[serde(default)]
        go: bool,
    }
    let Some(dto) = parse_json::<Dto>(conn, body)? else { return Ok(()) };
    let store = super::store(cfg);
    // A picked picture is copied into the chat before the write, as a
    // message's are.
    let kept = match dto.picture.as_ref() {
        Some(Some(p)) => {
            if p.kind != "output" {
                return err(conn, 400, "a cast member's picture is an output: choose or upload one");
            }
            let bytes = match super::attach_bytes(cfg, &p.kind, &p.id) {
                Ok(b) => b,
                Err(e) => return err(conn, 400, &format!("{e:#}")),
            };
            let file = format!("p{}.png", super::chat::now_ms());
            if let Err(e) = super::keep_picture(&store, &dto.id, &file, &bytes) {
                return err(conn, 500, &format!("{e:#}"));
            }
            Some(ImageRef { kind: p.kind.clone(), id: p.id.clone(), file, seed: None })
        }
        _ => None,
    };
    if let Some(Some(stem)) = &dto.voice {
        if crate::workitems::Store::new(&cfg.base_dir.join("work"), &cfg.user_dir()).find_audio_output(stem).is_none() {
            return err(conn, 400, &format!("no saved sound '{stem}': choose or upload one"));
        }
    }
    // At the model it is about to be shot on, when that changes.
    let shot_seconds = store.load(&dto.id).ok().and_then(|c| c.film).map_or(0.0, |mut f| {
        if let Some(v) = &dto.video_model {
            f.video_model = v.clone();
        }
        super::shot_seconds(cfg, &f)
    });
    let models = toolkit::runnable(cfg, &cfg.registry());
    // Each shot is looked at when the chat's agent can see.
    let watch = dto.go
        && store.load(&dto.id).ok().and_then(|c| super::agent_for(cfg, c.agent.as_deref()).ok()).is_some_and(|m| m.vision());
    let retakes = crate::settings::Settings::load_or_default(&cfg.user_dir()).agent.film_retakes;
    let r = store.update(&dto.id, |c| {
        if c.session.busy() {
            anyhow::bail!("the agent is working on this chat — wait, or stop it first");
        }
        if dto.go {
            session::go(c, watch, retakes).map_err(|e| anyhow::anyhow!(e))?;
            c.session.state = State::Queued;
            c.session.job = None;
            c.session.error = None;
            return Ok(Some(c.clone()));
        }
        let f = c.film.as_mut().ok_or_else(|| anyhow::anyhow!("this chat has no storyboard"))?;
        if !matches!(f.phase, Phase::Board | Phase::Ready) {
            anyhow::bail!("this film is being shot or is done; ask the agent for a new storyboard to change it");
        }
        if let Some(e) = &dto.every {
            f.every = e.trim().to_string();
        }
        // Its cuts follow the cast on H3 and start from keyframes on Wan, so
        // the switch may leave keyframes to draw (or ones no longer needed,
        // which stay).
        if let Some(name) = &dto.video_model {
            let v = toolkit::resolve(cfg, &Chat { video: Some(name.clone()), ..c.clone() })
                .video
                .filter(|v| v.chains)
                .ok_or_else(|| anyhow::anyhow!("'{name}' isn't an installed video model that can film shot after shot"))?;
            let f = c.film.as_mut().expect("checked above");
            f.video_model = v.name.clone();
            f.references = v.references;
            c.video = Some(v.name);
        }
        // A new model draws what is missing from here on; what is drawn stays.
        let image_model = match &dto.image_model {
            Some(name) => {
                let m = models.iter().find(|m| &m.name == name).ok_or_else(|| anyhow::anyhow!("'{name}' isn't an installed model"))?;
                Some((m.name.clone(), m.references))
            }
            None => None,
        };
        let edit = session::BoardEdit {
            image_model,
            member: dto.member,
            name: dto.name.clone(),
            look: dto.look.clone(),
            picture: dto.picture.as_ref().map(|p| p.as_ref().map(|_| kept.clone().expect("kept above"))),
            shot: dto.shot,
            text: dto.text.clone(),
            cast: dto.cast.clone(),
            cut: dto.cut,
            redraw_keyframe: dto.redraw_keyframe,
            no_keyframe: dto.no_keyframe,
            lines: dto.lines.clone(),
            sound: dto.sound.clone(),
            music: dto.music.clone(),
            voice: dto.voice.clone(),
        };
        session::edit_board(c, &edit).map_err(|e| anyhow::anyhow!(e))?;
        if dto.draw {
            session::board_press(c, shot_seconds);
        } else {
            session::board_advance(c, shot_seconds, false);
        }
        let draws = c.session.next == Next::Film;
        if draws {
            c.session.state = State::Queued;
            c.session.job = None;
            c.session.error = None;
        }
        Ok(draws.then(|| c.clone()))
    });
    match r {
        Ok(Some(c)) => {
            enqueue(lanes, &store, &c)?;
            get_chat(conn, cfg, &dto.id)
        }
        Ok(None) => get_chat(conn, cfg, &dto.id),
        Err(e) => err(conn, 409, &format!("{e:#}")),
    }
}

/// Queue a leg for the chat, its state set first so the page never sees it
/// idle in between.
fn enqueue(lanes: &Lanes, store: &ChatStore, c: &Chat) -> Result<u64> {
    let label = if c.title.is_empty() { "Agent".to_string() } else { format!("Agent · {}", c.title) };
    let job = lanes.queue.push(jobs::Kind::Agent, label, None, Work::Agent { chat: c.id.clone() }, None, false);
    store.update(&c.id, |c| {
        if c.session.job.is_none() {
            c.session.job = Some(job);
        }
        Ok(())
    })?;
    Ok(job)
}

fn send(conn: &mut Connection, cfg: &Config, lanes: &Lanes, body: &[u8]) -> Result<()> {
    #[derive(Deserialize)]
    struct Pic {
        kind: String,
        id: String,
    }
    #[derive(Deserialize)]
    struct Dto {
        chat: String,
        #[serde(default)]
        text: String,
        #[serde(default)]
        images: Vec<Pic>,
        /// The Video tab's model now, which a film from here on is shot on.
        #[serde(default)]
        video: Option<String>,
    }
    // Every message is a chat turn: the agent decides whether it talks,
    // draws, describes a picture or starts a session (`session::apply_reply`).
    // An older page's `mode` is an unknown field, ignored.
    let Some(dto) = parse_json::<Dto>(conn, body)? else { return Ok(()) };
    let text = dto.text.trim().to_string();
    if text.is_empty() && dto.images.is_empty() {
        return err(conn, 400, "write something, or attach a picture");
    }
    if dto.images.len() > 4 {
        return err(conn, 400, "at most four pictures in one message");
    }
    let store = super::store(cfg);
    // The pictures are copied into the chat now, so the message keeps them
    // whatever happens to the originals.
    let n = match store.load(&dto.chat) {
        Ok(c) if c.session.busy() => return err(conn, 409, "the agent is still working on this chat — stop it, or wait"),
        Ok(c) => c.next_n(),
        Err(e) => return err(conn, 404, &format!("{e:#}")),
    };
    let mut refs = Vec::new();
    for (k, p) in dto.images.iter().enumerate() {
        let bytes = match super::attach_bytes(cfg, &p.kind, &p.id) {
            Ok(b) => b,
            Err(e) => return err(conn, 400, &format!("{e:#}")),
        };
        let name = format!("u{n}-{k}.png");
        if let Err(e) = super::keep_picture(&store, &dto.chat, &name, &bytes) {
            return err(conn, 500, &format!("{e:#}"));
        }
        refs.push(ImageRef { kind: p.kind.clone(), id: p.id.clone(), file: name, seed: None });
    }
    let started = store.update(&dto.chat, |c| {
        if c.agent.is_none() {
            c.agent = super::agent_for(cfg, None).ok().map(|m| m.name);
        }
        c.name_from(&text);
        if let Some(v) = dto.video.as_ref().filter(|v| !v.is_empty()) {
            c.video = Some(v.clone());
        }
        // What the person says now overtakes a session left stopped or done.
        session::close_session(c);
        let n = c.next_n();
        c.messages.push(Msg::User { n, ms: super::chat::now_ms(), text: text.clone(), images: refs.clone() });
        c.session.mode = Mode::Chat;
        c.session.next = Next::Turn;
        c.session.state = State::Queued;
        c.session.job = None;
        c.session.error = None;
        Ok(c.clone())
    });
    let c = match started {
        Ok(c) => c,
        Err(e) => return err(conn, 500, &format!("{e:#}")),
    };
    let job = enqueue(lanes, &store, &c)?;
    ok(conn, json!({ "job": job }))
}

fn undo(conn: &mut Connection, cfg: &Config, body: &[u8]) -> Result<()> {
    #[derive(Deserialize)]
    struct Dto {
        chat: String,
        change: u64,
    }
    let Some(dto) = parse_json::<Dto>(conn, body)? else { return Ok(()) };
    let r = super::store(cfg).update(&dto.chat, |c| {
        if c.session.busy() {
            anyhow::bail!("the agent is working on this chat — undo once it is done");
        }
        session::undo(c, dto.change).map_err(|e| anyhow::anyhow!(e))
    });
    match r {
        Ok(_) => get_chat(conn, cfg, &dto.chat),
        Err(e) => err(conn, 409, &format!("{e:#}")),
    }
}

fn stop(conn: &mut Connection, cfg: &Config, lanes: &Lanes, monitor: &Monitor, body: &[u8]) -> Result<()> {
    #[derive(Deserialize)]
    struct Dto {
        chat: String,
    }
    let Some(dto) = parse_json::<Dto>(conn, body)? else { return Ok(()) };
    let store = super::store(cfg);
    let job = match store.load(&dto.chat) {
        Ok(c) => c.session.job,
        Err(e) => return err(conn, 404, &format!("{e:#}")),
    };
    let stopped = job.map(|j| lanes.queue.stop(j, || monitor.cancel()));
    // Waiting, or nothing behind it: the leg is gone, and the chat says so.
    // Running: the worker ends it at once and writes "Stopped" itself.
    if !matches!(stopped, Some(jobs::Stopped::Stopping)) {
        let _ = store.update(&dto.chat, |c| {
            if c.session.busy() {
                c.session.state = State::Stopped;
                c.session.job = None;
                session::close_session(c);
                c.note("Stopped.");
            }
            Ok(())
        });
    }
    ok(conn, json!({ "stopped": true }))
}
