//! The Agent tab's routes. Everything but `/agent/send` and `/agent/approve`
//! is a file read or write and answers on the connection thread; those two
//! queue a leg on the job queue and answer `{job}` at once — the reply arrives
//! in the chat, which the page re-reads as its `rev` moves.

use super::chat::{Chat, ChatStore, ImageRef, Mode, Msg, Next, State};
use super::{session, toolkit};
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
        ("POST", "/chats/settings") => edit(conn, cfg, body, Edit::Settings),
        ("POST", "/chats/permissions") => edit(conn, cfg, body, Edit::Permissions),
        ("POST", "/chats/agent") => edit(conn, cfg, body, Edit::Agent),
        ("POST", "/chats/rename") => edit(conn, cfg, body, Edit::Rename),
        ("POST", "/agent/send") => send(conn, cfg, lanes, body),
        ("POST", "/agent/approve") => approve(conn, cfg, lanes, body),
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
    ok(conn, json!({ "models": models, "driver": driver, "vram": vram, "dir": cfg.agent_dir().display().to_string() }))
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
        /// The Image tab's `/generate` body, as the page builds it.
        settings: Value,
        #[serde(default)]
        agent: Option<String>,
    }
    let Some(dto) = parse_json::<Dto>(conn, body)? else { return Ok(()) };
    if !dto.settings.is_object() {
        return err(conn, 400, "settings must be a /generate body");
    }
    let mut settings = dto.settings;
    clean_settings(&mut settings);
    if let Err(e) = serde_json::from_value::<crate::GenerateDto>(settings.clone()) {
        return err(conn, 400, &format!("those settings aren't a generate request: {e}"));
    }
    let perms = crate::settings::Settings::load_or_default(&cfg.user_dir()).agent;
    let agent = dto.agent.or_else(|| super::agent_for(cfg, None).ok().map(|m| m.name));
    let chat = Chat::new(super::chat::new_id(), settings, perms, agent);
    let store = super::store(cfg);
    match store.create(&chat) {
        Ok(()) => ok(conn, chat_json(cfg, &chat)),
        Err(e) => err(conn, 500, &format!("{e:#}")),
    }
}

/// What a chat's copy of the settings never carries: how many to run and
/// what to do after each — a round is the agent's to size, and its passes
/// are the agent's to ask for.
fn clean_settings(s: &mut Value) {
    if let Some(o) = s.as_object_mut() {
        for k in ["count", "loop", "update_only", "after", "seed"] {
            o.remove(k);
        }
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
    Settings,
    Permissions,
    Agent,
    Rename,
}

fn edit(conn: &mut Connection, cfg: &Config, body: &[u8], what: Edit) -> Result<()> {
    #[derive(Deserialize)]
    struct Dto {
        id: String,
        /// The `rev` the page last read; a settings edit over a newer one is
        /// refused, since the agent may have changed them meanwhile.
        #[serde(default)]
        rev: Option<u64>,
        #[serde(default)]
        settings: Option<Value>,
        #[serde(default)]
        permissions: Option<super::knobs::Permissions>,
        #[serde(default)]
        agent: Option<String>,
        #[serde(default)]
        title: Option<String>,
    }
    let Some(dto) = parse_json::<Dto>(conn, body)? else { return Ok(()) };
    let store = super::store(cfg);
    let result = store.update(&dto.id, |c| {
        match what {
            Edit::Settings => {
                if dto.rev.is_some_and(|r| r != c.rev) {
                    anyhow::bail!("conflict: the chat changed since — reload it");
                }
                let mut s = dto.settings.clone().ok_or_else(|| anyhow::anyhow!("no settings"))?;
                clean_settings(&mut s);
                serde_json::from_value::<crate::GenerateDto>(s.clone())
                    .map_err(|e| anyhow::anyhow!("those settings aren't a generate request: {e}"))?;
                c.settings = s;
                c.settings_rev += 1;
            }
            Edit::Permissions => {
                let p = dto.permissions.clone().ok_or_else(|| anyhow::anyhow!("no permissions"))?;
                p.validate()?;
                c.permissions = p;
            }
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
        Err(e) if e.to_string().starts_with("conflict") => err(conn, 409, &format!("{e:#}")),
        Err(e) => err(conn, 400, &format!("{e:#}")),
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
        /// `chat`, `session` (the text is the goal) or `describe`.
        #[serde(default)]
        mode: Option<String>,
    }
    let Some(dto) = parse_json::<Dto>(conn, body)? else { return Ok(()) };
    let mode = match dto.mode.as_deref().unwrap_or("chat") {
        "chat" => Mode::Chat,
        "session" => Mode::Plan,
        "describe" => Mode::Describe,
        other => return err(conn, 400, &format!("'{other}' is not a way to send (chat, session, describe)")),
    };
    let text = dto.text.trim().to_string();
    if text.is_empty() && dto.images.is_empty() {
        return err(conn, 400, "write something, or attach a picture");
    }
    if mode == Mode::Plan && text.is_empty() {
        return err(conn, 400, "a session needs a goal: say what the picture should show");
    }
    if mode == Mode::Describe && dto.images.is_empty() {
        return err(conn, 400, "attach the picture to describe");
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
        // A new message answers what was waiting: a proposal left unanswered
        // is set aside, or it would hold the new turn back.
        let mut dropped = 0;
        for ch in c.changes.iter_mut().filter(|ch| ch.state == super::chat::ChangeState::Pending) {
            ch.state = super::chat::ChangeState::Rejected;
            dropped += 1;
        }
        for a in c.actions.iter_mut().filter(|a| a.state == super::chat::ActionState::Pending) {
            a.state = super::chat::ActionState::Rejected;
            dropped += 1;
        }
        if dropped > 0 {
            c.note(format!("{dropped} proposal(s) left unanswered were set aside."));
        }
        let n = c.next_n();
        c.messages.push(Msg::User { n, ms: super::chat::now_ms(), text: text.clone(), images: refs.clone() });
        if mode == Mode::Plan {
            // A new session: a new goal and its own checks. Its rounds count
            // from here; round numbers go on, so none is reused. The plan
            // decides whether it judges the round just drawn or draws afresh.
            c.session.goal = text.clone();
            c.session.checks.clear();
            c.session.first_round = c.session.round;
        }
        c.session.mode = mode;
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

fn approve(conn: &mut Connection, cfg: &Config, lanes: &Lanes, body: &[u8]) -> Result<()> {
    #[derive(Deserialize)]
    struct Answer {
        id: u64,
        approve: bool,
    }
    #[derive(Deserialize)]
    struct Dto {
        chat: String,
        #[serde(default)]
        changes: Vec<Answer>,
        #[serde(default)]
        actions: Vec<Answer>,
    }
    let Some(dto) = parse_json::<Dto>(conn, body)? else { return Ok(()) };
    let store = super::store(cfg);
    let changes: Vec<(u64, bool)> = dto.changes.iter().map(|a| (a.id, a.approve)).collect();
    let actions: Vec<(u64, bool)> = dto.actions.iter().map(|a| (a.id, a.approve)).collect();
    let go_on = store.update(&dto.chat, |c| {
        if c.session.busy() {
            anyhow::bail!("the agent is working on this chat");
        }
        let tk = toolkit::resolve(cfg, c);
        let go = session::approve(c, &changes, &actions, &tk.locks, &tk.model_names());
        if go {
            c.session.state = State::Queued;
            c.session.job = None;
        }
        Ok(go.then(|| c.clone()))
    });
    match go_on {
        Ok(Some(c)) => {
            let job = enqueue(lanes, &store, &c)?;
            ok(conn, json!({ "job": job }))
        }
        Ok(None) => ok(conn, json!({ "job": null })),
        Err(e) => err(conn, 409, &format!("{e:#}")),
    }
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
            if c.session.busy() || c.session.state == State::Awaiting {
                c.session.state = State::Stopped;
                c.session.job = None;
                c.note("Stopped.");
            }
            Ok(())
        });
    }
    ok(conn, json!({ "stopped": true }))
}
