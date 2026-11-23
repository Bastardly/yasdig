//! The Assistant panels' routes. Everything but `/agent/send` is a file read or
//! write and answers on the connection thread; that one queues a leg on the job queue and answer `{job}` at once — the reply arrives
//! in the chat, which the page re-reads as its `rev` moves.

use super::chat::{Chat, ChatStore, ImageRef, Pane, Mode, Msg, Next, State};
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
        ("POST", "/chats/settings") => settings(conn, cfg, body),
        ("POST", "/chats/film") => film(conn, cfg, lanes, body),
        ("POST", "/chats/board") => board(conn, cfg, body),
        ("POST", "/chats/cast") => chat_cast(conn, cfg, body),
        ("POST", "/agent/send") => send(conn, cfg, lanes, body),
        ("POST", "/enhance") => enhance(conn, cfg, lanes, body),
        ("POST", "/agent/undo") => undo(conn, cfg, body),
        ("POST", "/agent/stop") => stop(conn, cfg, lanes, monitor, body),
        ("POST", "/agent/session") => answer_proposal(conn, cfg, lanes, body),
        ("POST", "/agent/film-model") => answer_film_model(conn, cfg, lanes, body),
        ("POST", "/agent/again") => again(conn, cfg, lanes, body),
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
        "context": c.context,
    })
}

fn listing(conn: &mut Connection, cfg: &Config, shelf: Shelf) -> Result<()> {
    let list: Vec<Value> = super::store(cfg).listing(shelf).iter().map(summary).collect();
    ok(conn, Value::Array(list))
}

/// Whether what the page sent with a Video chat's request is the film's own
/// Video's — its Generation panel and model — rather than another Video's
/// open on the tab: the film has no Video yet, or `video_id` is it. Only
/// then are they the film's; otherwise its Video's own record stands.
fn from_film_video(c: &Chat, video_id: Option<&str>) -> bool {
    match c.film.as_ref().and_then(|f| f.video.as_deref()) {
        None => true,
        Some(v) => video_id == Some(v),
    }
}

/// The page's Generation panel onto a Video chat, by [`from_film_video`].
fn take_video_settings(c: &mut Chat, video_id: Option<&str>, settings: Option<Value>) {
    c.video_settings = if from_film_video(c, video_id) { settings.filter(Value::is_object) } else { None };
}

/// Video `vid` as the film of chat `c`, which has none: empty but for the
/// Video, so the turn's `filmvideo::pull` reads every shot back as the
/// person's (none is the film's), and `Ready`, its pictures being whatever
/// the person gave it. On `view`, the model it is shot on.
fn adopt_video(c: &mut Chat, vid: &str, view: Option<&toolkit::VideoView>) {
    c.film = Some(super::chat::Film { video: Some(vid.to_string()), phase: super::chat::Phase::Ready, ..Default::default() });
    if let Some(v) = view {
        session::set_video(c, v);
    }
}

fn chat_json(cfg: &Config, c: &Chat) -> Value {
    let t = toolkit::resolve(cfg, c);
    json!({ "chat": c, "toolkit": t, "favorite": c.favorite })
}

/// The Video tab's model a film is shot on (`session::set_video`): it
/// decides whether a shot's part 1 follows the cast's pictures (H3) or
/// starts from a first frame. The board itself is the film's Video
/// (`filmvideo`), edited in the storyboard. Answers the chat.
fn board(conn: &mut Connection, cfg: &Config, body: &[u8]) -> Result<()> {
    #[derive(Deserialize)]
    struct Dto {
        id: String,
        #[serde(default)]
        video: Option<String>,
        /// The Generation panel, and the Video open on the tab it is of.
        #[serde(default)]
        settings: Option<Value>,
        #[serde(default)]
        video_id: Option<String>,
    }
    let Some(dto) = parse_json::<Dto>(conn, body)? else { return Ok(()) };
    let store = super::store(cfg);
    let film = match store.load(&dto.id) {
        Ok(c) => c.film,
        Err(e) => return err(conn, 404, &format!("{e:#}")),
    };
    let Some(film) = film else { return err(conn, 404, "this chat has no storyboard") };
    let shot_seconds = super::shot_seconds(cfg, &film);
    let to = match dto.video.as_deref() {
        Some(name) => match cfg.video_models().iter().find(|m| m.name == name && m.exists) {
            Some(m) => Some(toolkit::VideoView::of(m)),
            None => return err(conn, 400, &format!("'{name}' isn't an installed video model")),
        },
        None => None,
    };
    let r = store.update(&dto.id, |c| {
        if c.session.busy() {
            anyhow::bail!("the agent is working on this chat — wait, or stop it first");
        }
        if let Some(v) = &to {
            session::set_video(c, v);
        }
        take_video_settings(c, dto.video_id.as_deref(), dto.settings.clone());
        session::board_advance(c, shot_seconds, false);
        Ok(c.clone())
    });
    match r {
        Ok(c) => {
            // The parts' references follow the model: written to the Video.
            let warning = super::filmvideo::push(cfg, &store, &dto.id).err().map(|e| format!("{e:#}")).unwrap_or_default();
            let c = store.load(&dto.id).unwrap_or(c);
            let mut v = chat_json(cfg, &c);
            v["warning"] = json!(warning);
            ok(conn, v)
        }
        Err(e) => err(conn, 409, &format!("{e:#}")),
    }
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
        /// The panel it is begun in.
        #[serde(default)]
        context: Pane,
        /// An Image chat's: the Image tab's controls, which it starts from.
        #[serde(default)]
        settings: Option<Value>,
    }
    let Some(dto) = parse_json::<Dto>(conn, body)? else { return Ok(()) };
    if !cfg.registry().contains_key(&dto.template) {
        return err(conn, 400, &format!("'{}' isn't an installed model", dto.template));
    }
    let agent = dto.agent.or_else(|| super::agent_for(cfg, None).ok().map(|m| m.name));
    let mut chat = Chat::new(super::chat::new_id(), json!({ "template": dto.template }), agent);
    chat.video = dto.video.filter(|v| !v.is_empty());
    chat.context = dto.context;
    match dto.settings {
        Some(v) if dto.context == Pane::Image => {
            chat.take_settings(v);
        }
        // A Video chat's: the Generation panel, which its film's Video is made with.
        Some(v) if dto.context == Pane::Video && v.is_object() => chat.video_settings = Some(v),
        _ => {}
    }
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

/// The Image tab's controls into an Image chat, as the person changes them —
/// while the agent works too: their edit wins, and the next round or turn
/// draws with it (the worker re-reads the chat at every step).
fn settings(conn: &mut Connection, cfg: &Config, body: &[u8]) -> Result<()> {
    #[derive(Deserialize)]
    struct Dto {
        id: String,
        settings: Value,
    }
    let Some(dto) = parse_json::<Dto>(conn, body)? else { return Ok(()) };
    if !dto.settings.is_object() {
        return err(conn, 400, "settings are a /generate body");
    }
    let r = super::store(cfg).update(&dto.id, |c| {
        if c.context != Pane::Image {
            anyhow::bail!("only an Image chat follows the Image tab");
        }
        Ok(c.take_settings(dto.settings.clone()))
    });
    match r {
        Ok(_) => ok(conn, json!({ "ok": true })),
        Err(e) => err(conn, 409, &format!("{e:#}")),
    }
}

/// The person's edits in the cast editor bound to the chat: the member's
/// words as the agent will next see them. Only onto the entry the chat is
/// bound to (`cast`, its id); no `cast_rev` move, so the editor isn't
/// opened again over the person's own typing.
fn chat_cast(conn: &mut Connection, cfg: &Config, body: &[u8]) -> Result<()> {
    #[derive(Deserialize)]
    struct Dto {
        id: String,
        cast: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        kind: super::chat::MemberKind,
        #[serde(default)]
        look: String,
    }
    let Some(dto) = parse_json::<Dto>(conn, body)? else { return Ok(()) };
    let r = super::store(cfg).update(&dto.id, |c| {
        let Some(cc) = c.cast.as_mut().filter(|cc| cc.id == dto.cast) else {
            anyhow::bail!("this chat isn't making that cast member");
        };
        cc.name = dto.name.trim().to_string();
        cc.kind = dto.kind;
        cc.look = dto.look.trim().to_string();
        Ok(())
    });
    match r {
        Ok(()) => ok(conn, json!({ "ok": true })),
        Err(e) => err(conn, 409, &format!("{e:#}")),
    }
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
        /// The Video's shot (`{video}-s3`) whose part 1 `shot` means,
        /// in place of an index into the film's parts.
        #[serde(default)]
        shot_id: Option<String>,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        look: Option<String>,
        #[serde(default)]
        speaks: Option<String>,
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
        /// Draw what the board is missing: with `frames` false the cast
        /// (*Draw the cast*), true the first frames too (*Draw the first
        /// frames*), absent whichever is next.
        #[serde(default)]
        draw: bool,
        #[serde(default)]
        frames: Option<bool>,
        /// The image model the board's pictures are drawn with.
        #[serde(default)]
        image_model: Option<String>,
        /// A saved cast entry's id, added to the film's cast (*Add from cast…*).
        #[serde(default)]
        from_cast: Option<String>,
        /// The Generation panel, and the Video open on the tab it is of: the
        /// film's Video is written with it only when that is the one.
        #[serde(default)]
        settings: Option<Value>,
        #[serde(default)]
        video_id: Option<String>,
    }
    let Some(mut dto) = parse_json::<Dto>(conn, body)? else { return Ok(()) };
    let store = super::store(cfg);
    // The storyboard as it is now, so an edit here lands on it, not on the
    // film's older copy; and the panel it is made with, when it is the film's.
    if store.load(&dto.id).is_ok_and(|c| !c.session.busy()) {
        let _ = store.update(&dto.id, |c| {
            if !c.session.busy() {
                take_video_settings(c, dto.video_id.as_deref(), dto.settings.clone());
            }
            Ok(())
        });
        if let Err(e) = super::filmvideo::pull(cfg, &store, &dto.id) {
            return err(conn, 500, &format!("reading the film's video: {e:#}"));
        }
    }
    if let Some(sid) = &dto.shot_id {
        let at = store.load(&dto.id).ok().and_then(|c| c.film).and_then(|f| f.shots.iter().position(|s| s.shot.as_deref() == Some(sid.as_str())));
        match at {
            Some(k) => dto.shot = Some(k),
            None => return err(conn, 404, &format!("shot {sid} isn't in this chat's film")),
        }
    }
    let add = match &dto.from_cast {
        Some(id) => match super::cast_member(cfg, &store, &dto.id, id) {
            Ok(m) => Some(m),
            Err(e) => return err(conn, 400, &format!("{e:#}")),
        },
        None => None,
    };
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
    let shot_seconds = store.load(&dto.id).ok().and_then(|c| c.film).map_or(0.0, |f| super::shot_seconds(cfg, &f));
    let models = toolkit::runnable(cfg, &cfg.registry());
    let r = store.update(&dto.id, |c| {
        if c.session.busy() {
            anyhow::bail!("the agent is working on this chat — wait, or stop it first");
        }
        let f = c.film.as_mut().ok_or_else(|| anyhow::anyhow!("this chat has no storyboard"))?;
        if let Some(e) = &dto.every {
            f.every = e.trim().to_string();
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
            speaks: dto.speaks.clone(),
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
            add: add.clone(),
        };
        session::edit_board(c, &edit).map_err(|e| anyhow::anyhow!(e))?;
        if dto.draw {
            match dto.frames {
                Some(frames) => session::board_press_for(c, shot_seconds, frames),
                None => session::board_press(c, shot_seconds),
            }
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
        Ok(None) => {
            // Nothing to draw: the edit is written into the Video now.
            if let Err(e) = super::filmvideo::push(cfg, &store, &dto.id) {
                return err(conn, 500, &format!("writing the film's video: {e:#}"));
            }
            get_chat(conn, cfg, &dto.id)
        }
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

/// *Enhance*: a prompt rewritten in full by the agent's model, on the queue
/// (it holds the GPU) as a button press, answering `{prompt, sound?}`.
fn enhance(conn: &mut Connection, cfg: &Config, lanes: &Lanes, body: &[u8]) -> Result<()> {
    let Some(req) = parse_json::<super::enhance::Request>(conn, body)? else { return Ok(()) };
    if req.text.trim().is_empty() {
        return err(conn, 400, "there is nothing to enhance — write a few words first");
    }
    if let Err(e) = super::enhance::target(cfg, &req) {
        return err(conn, 400, &format!("{e:#}"));
    }
    if req.picture.as_deref().is_some_and(|p| !valid_id(p)) {
        return err(conn, 400, "a bad picture name");
    }
    let label = if req.kind == "video" { "Enhance a shot" } else { "Enhance the prompt" };
    crate::enqueue_wait(conn, lanes, jobs::Kind::Enhance, label.into(), Work::Enhance(Box::new(req)), true, 400)
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
        /// An Image chat's: the Image tab's controls as they are now. A
        /// Video chat's: the Generation panel (`vidPayload` without shots).
        #[serde(default)]
        settings: Option<Value>,
        /// The Video open on the tab, whose panel `settings` is.
        #[serde(default)]
        video_id: Option<String>,
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
        // Said while a session runs: it joins the session (`Session::steers`).
        Ok(c) if c.session.steerable() => {
            if !dto.images.is_empty() {
                return err(conn, 409, "while the session runs, say what to change in words — attach pictures once it ends, or stop it");
            }
            let r = store.update(&dto.chat, |c| {
                if !c.session.steerable() {
                    anyhow::bail!("the session has just ended — send it again");
                }
                let n = c.next_n();
                c.messages.push(Msg::User { n, ms: super::chat::now_ms(), text: text.clone(), images: Vec::new() });
                c.session.steers.push(text.clone());
                Ok(())
            });
            return match r {
                Ok(()) => ok(conn, json!({ "steered": true })),
                Err(e) => err(conn, 409, &format!("{e:#}")),
            };
        }
        Ok(c) if c.session.busy() && c.session.sweep.is_some() => {
            return err(conn, 409, "a comparison judges every value by the same checks, so it can't take more on the way — stop it, or wait")
        }
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
    // A Video chat with no film yet, sent with a saved Video open, takes
    // that Video as its film (`adopt`): the person made it by hand and asks
    // about "shot 2", so the turn is shown it, read back as theirs.
    let adopt = dto.video_id.as_deref().and_then(|vid| {
        let v = crate::video_store(cfg).load_video(vid).ok()?;
        let model = v.lock.as_ref().map(|l| l.model.clone()).or_else(|| dto.video.clone()).filter(|m| !m.is_empty());
        let view = model.and_then(|name| cfg.video_models().iter().find(|m| m.name == name && m.exists).map(toolkit::VideoView::of));
        Some((vid.to_string(), view))
    });
    let started = store.update(&dto.chat, |c| {
        if c.context == Pane::Video && c.film.is_none() {
            if let Some((vid, view)) = &adopt {
                adopt_video(c, vid, view.as_ref());
            }
        }
        if c.agent.is_none() {
            c.agent = super::agent_for(cfg, None).ok().map(|m| m.name);
        }
        c.name_from(&text);
        if c.context == Pane::Image {
            if let Some(v) = dto.settings.clone() {
                c.take_settings(v);
            }
        } else if from_film_video(c, dto.video_id.as_deref()) {
            if let Some(v) = dto.video.as_ref().filter(|v| !v.is_empty()) {
                c.video = Some(v.clone());
            }
            c.video_settings = dto.settings.clone().filter(Value::is_object);
        } else {
            // Sent from another Video: its panel isn't the film's.
            c.video_settings = None;
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

/// A button on the film-model card: the chat's video model from now on,
/// and the story turn the "film" asked for, queued on it.
fn answer_film_model(conn: &mut Connection, cfg: &Config, lanes: &Lanes, body: &[u8]) -> Result<()> {
    #[derive(Deserialize)]
    struct Dto {
        chat: String,
        video: String,
    }
    let Some(dto) = parse_json::<Dto>(conn, body)? else { return Ok(()) };
    let Some(view) = cfg.video_models().iter().find(|m| m.name == dto.video && m.exists).map(super::toolkit::VideoView::of) else {
        return err(conn, 400, &format!("the video model '{}' isn't installed whole", dto.video));
    };
    let store = super::store(cfg);
    let r = store.update(&dto.chat, |c| {
        if c.session.busy() {
            anyhow::bail!("the agent is working on this chat — stop it, or wait");
        }
        session::pick_film_model(c, &dto.video, &view).map_err(|e| anyhow::anyhow!(e))?;
        c.session.state = State::Queued;
        c.session.job = None;
        c.session.error = None;
        Ok(c.clone())
    });
    let c = match r {
        Ok(c) => c,
        Err(e) => return err(conn, 409, &format!("{e:#}")),
    };
    let job = enqueue(lanes, &store, &c)?;
    ok(conn, json!({ "job": job }))
}

/// *Start* or *No thanks* on the session the agent proposed.
fn answer_proposal(conn: &mut Connection, cfg: &Config, lanes: &Lanes, body: &[u8]) -> Result<()> {
    #[derive(Deserialize)]
    struct Dto {
        chat: String,
        start: bool,
        /// *Draw one*: the proposal's first prompt, once, instead of the session.
        #[serde(default)]
        draw_one: bool,
        /// The card's sizes, where the person set them.
        #[serde(default)]
        rounds: Option<u32>,
        #[serde(default)]
        count: Option<u32>,
    }
    let Some(dto) = parse_json::<Dto>(conn, body)? else { return Ok(()) };
    let store = super::store(cfg);
    let r = store.update(&dto.chat, |c| {
        if c.session.busy() {
            anyhow::bail!("the agent is working on this chat — stop it, or wait");
        }
        if !dto.start {
            return session::decline_proposal(c).map_err(|e| anyhow::anyhow!(e)).map(|_| c.clone());
        }
        if dto.draw_one {
            session::draw_proposal(c).map_err(|e| anyhow::anyhow!(e))?;
        } else {
            session::start_proposal_sized(c, dto.rounds, dto.count).map_err(|e| anyhow::anyhow!(e))?;
        }
        c.session.state = State::Queued;
        c.session.job = None;
        c.session.error = None;
        Ok(c.clone())
    });
    let c = match r {
        Ok(c) => c,
        Err(e) => return err(conn, 409, &format!("{e:#}")),
    };
    if !dto.start {
        return get_chat(conn, cfg, &dto.chat);
    }
    let job = enqueue(lanes, &store, &c)?;
    ok(conn, json!({ "job": job }))
}

/// *Again* under a round (`session::draw_again`): queued like a turn, but
/// its leg starts at the pictures.
fn again(conn: &mut Connection, cfg: &Config, lanes: &Lanes, body: &[u8]) -> Result<()> {
    #[derive(Deserialize)]
    struct Dto {
        chat: String,
        #[serde(default)]
        count: Option<u32>,
    }
    let Some(dto) = parse_json::<Dto>(conn, body)? else { return Ok(()) };
    let store = super::store(cfg);
    let r = store.update(&dto.chat, |c| {
        if c.session.busy() {
            anyhow::bail!("the agent is working on this chat — stop it, or wait");
        }
        session::draw_again(c, dto.count.unwrap_or(1)).map_err(|e| anyhow::anyhow!(e))?;
        c.session.state = State::Queued;
        c.session.job = None;
        c.session.error = None;
        Ok(c.clone())
    });
    let c = match r {
        Ok(c) => c,
        Err(e) => return err(conn, 409, &format!("{e:#}")),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The page's panel is a film's only when sent from its own Video, or
    /// before it has one: another Video's panel clears what was kept, so the
    /// film's Video's own record stands.
    #[test]
    fn a_film_takes_the_panel_only_from_its_own_video() {
        let mut c = Chat::new("1-0".into(), json!({}), None);
        c.context = Pane::Video;
        let panel = json!({ "model": "5b", "steps": 9 });
        take_video_settings(&mut c, Some("other"), Some(panel.clone()));
        assert_eq!(c.video_settings, Some(panel.clone()), "no Video yet: the panel it will be made with");
        c.film = Some(super::super::chat::Film { video: Some("v".into()), ..Default::default() });
        take_video_settings(&mut c, Some("other"), Some(panel.clone()));
        assert_eq!(c.video_settings, None, "another Video's panel");
        take_video_settings(&mut c, Some("v"), Some(panel.clone()));
        assert_eq!(c.video_settings, Some(panel));
    }

    /// A Video chat with no film takes the open Video as one, ready (no
    /// pictures to draw first), and its panel is then that film's.
    #[test]
    fn a_chat_with_no_film_adopts_the_open_video() {
        let mut c = Chat::new("1-0".into(), json!({}), None);
        c.context = Pane::Video;
        adopt_video(&mut c, "v", None);
        let f = c.film.as_ref().unwrap();
        assert_eq!((f.video.as_deref(), f.phase, f.shots.len()), (Some("v"), super::super::chat::Phase::Ready, 0));
        assert!(from_film_video(&c, Some("v")));
    }
}
