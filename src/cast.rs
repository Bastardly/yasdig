//! The cast: characters, places and props saved under a name, to reuse in
//! pictures, clips and storyboards — the film's cast members
//! (`agent::chat::Member`) with a life of their own.
//!
//! One folder per entry under `user/cast/{id}/` (under `user/` because
//! nothing sweeps it: *Close server*'s clear never reaches it):
//!
//! ```text
//! cast.json        the record (`Entry`)
//! picture.png      what it looks like
//! sheet.png        a character's turnaround, on a model that follows references
//! voice.wav        a character's voice
//! thumb.png        the Cast tab's tile
//! tries/{n}.png    the editor's draws not yet chosen (`{n}-sheet.png` beside)
//! ```
//!
//! **An entry copies in, never adopts**: a picture or a voice picked from
//! Outputs is copied into its folder (invariant 8's rule, the other way
//! round — deleting an output never breaks an entry, deleting an entry never
//! deletes an output).
//!
//! **Draw again keeps the tries**: each draw adds `tries/{n}.png`, and *Save*
//! keeps the one chosen and empties `tries/`. A new entry gets its folder at
//! its first draw, as a `draft`, so the tries have a home and survive a
//! reload; *Discard* deletes a draft and only empties a saved entry's tries.
//!
//! **A name is a key**: prompts and storyboards find an entry by it, so two
//! saved entries never share one (case aside).

use crate::agent::chat::MemberKind;
use crate::config::Config;
use crate::http::Connection;
use crate::pipeline::Engine;
use crate::workitems::{valid_id, Shelf};
use crate::{error_json, jobs, parse_json, Lanes, Work};
use anyhow::{anyhow, bail, Context as _, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Characters a name may run to: it goes in prompts, and in a tile's caption.
const MAX_NAME: usize = 60;

/// One lock over every record write: the worker adds a try while the
/// connection thread may be saving or discarding the same entry.
static LOCK: Mutex<()> = Mutex::new(());

/// One draw in the editor, not yet chosen.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Try {
    pub n: u32,
    pub seed: i64,
    /// A turnaround sheet was drawn first and the picture from it.
    #[serde(default)]
    pub sheet: bool,
    pub model: String,
    /// The words it was drawn from.
    #[serde(default)]
    pub prompt: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub kind: MemberKind,
    /// How it looks, in words starting with its name: what a prompt without
    /// its picture is told.
    #[serde(default)]
    pub look: String,
    /// The image model it is drawn with.
    #[serde(default)]
    pub image_model: String,
    /// The seed its picture was drawn at, when drawn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
    /// Never saved: the folder only holds the tries of a new entry.
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub tries: Vec<Try>,
    #[serde(default)]
    pub next_try: u32,
    /// Moves on every write: the page's cache-buster for the pictures.
    #[serde(default)]
    pub rev: u64,
}

/// What the page is shown of an entry: the record, and which files it has.
#[derive(Debug, Serialize)]
pub struct View {
    #[serde(flatten)]
    pub entry: Entry,
    pub picture: bool,
    pub sheet: bool,
    pub voice: bool,
}

/// The fields the editor sends with a draw or a save.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Fields {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub kind: MemberKind,
    #[serde(default)]
    pub look: String,
    #[serde(default)]
    pub image_model: String,
}

/// What the saved picture is.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Pick {
    /// One of the tries.
    Try(u32),
    /// A picture from Outputs (an upload is one too), copied in.
    Output(String),
}

/// What the saved voice is.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum VoicePick {
    /// A sound from Outputs, copied in.
    Output(String),
    Remove,
}

pub struct CastStore {
    root: PathBuf,
}

impl CastStore {
    pub fn new(user_dir: &Path) -> CastStore {
        CastStore { root: user_dir.join("cast") }
    }

    pub fn of(cfg: &Config) -> CastStore {
        CastStore::new(&cfg.user_dir())
    }

    /// An entry's folder. The only place an id becomes a path, and only a
    /// whitelisted one does.
    pub fn dir(&self, id: &str) -> Result<PathBuf> {
        if !valid_id(id) {
            bail!("'{id}' is not a cast entry");
        }
        Ok(self.root.join(id))
    }

    fn record(&self, id: &str) -> Result<PathBuf> {
        Ok(self.dir(id)?.join("cast.json"))
    }

    pub fn load(&self, id: &str) -> Result<Entry> {
        let path = self.record(id)?;
        let text = std::fs::read_to_string(&path).map_err(|_| anyhow!("that cast entry is gone"))?;
        let mut e: Entry = serde_json::from_str(&text).with_context(|| format!("reading {}", path.display()))?;
        e.id = id.to_string();
        Ok(e)
    }

    /// Every entry, saved ones and drafts, by name.
    pub fn list(&self) -> Vec<View> {
        let Ok(dirs) = std::fs::read_dir(&self.root) else { return Vec::new() };
        let mut out: Vec<View> = dirs
            .flatten()
            .filter_map(|d| d.file_name().to_str().map(str::to_string))
            .filter_map(|id| self.load(&id).ok())
            .map(|e| self.view(e))
            .collect();
        out.sort_by(|a, b| {
            (a.entry.draft, a.entry.name.to_lowercase(), &a.entry.id).cmp(&(b.entry.draft, b.entry.name.to_lowercase(), &b.entry.id))
        });
        out
    }

    pub fn view(&self, entry: Entry) -> View {
        let has = |f: &str| self.dir(&entry.id).map(|d| d.join(f).is_file()).unwrap_or(false);
        View { picture: has("picture.png"), sheet: has("sheet.png"), voice: has("voice.wav"), entry }
    }

    /// Write `e` whole: `.part`, then a rename, the `rev` moved.
    fn write(&self, e: &mut Entry) -> Result<()> {
        let dir = self.dir(&e.id)?;
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        e.rev += 1;
        let path = dir.join("cast.json");
        let part = dir.join("cast.json.part");
        std::fs::write(&part, serde_json::to_vec_pretty(e)?).with_context(|| format!("writing {}", part.display()))?;
        std::fs::rename(&part, &path).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    /// Change entry `id` under the lock, and write it.
    pub fn update<T>(&self, id: &str, f: impl FnOnce(&mut Entry) -> Result<T>) -> Result<(Entry, T)> {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut e = self.load(id)?;
        let t = f(&mut e)?;
        self.write(&mut e)?;
        Ok((e, t))
    }

    /// A new draft holding `fields`, its folder made.
    pub fn create(&self, fields: &Fields) -> Result<Entry> {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut n = 0u32;
        let id = loop {
            let id = format!("c{}{}", crate::agent::chat::now_ms(), if n == 0 { String::new() } else { format!("-{n}") });
            if !self.root.join(&id).exists() {
                break id;
            }
            n += 1;
        };
        let mut e = Entry { id, draft: true, ..Default::default() };
        set_fields(&mut e, fields);
        self.write(&mut e)?;
        Ok(e)
    }

    pub fn try_path(&self, id: &str, n: u32, sheet: bool) -> Result<PathBuf> {
        Ok(self.dir(id)?.join("tries").join(if sheet { format!("{n}-sheet.png") } else { format!("{n}.png") }))
    }

    /// A draw's pictures into `tries/`, recorded on the entry.
    pub fn add_try(&self, id: &str, picture: &[u8], sheet: Option<&[u8]>, seed: i64, model: &str, prompt: &str) -> Result<Entry> {
        let (e, ()) = self.update(id, |e| {
            let n = e.next_try.max(e.tries.iter().map(|t| t.n + 1).max().unwrap_or(0));
            let dir = self.dir(id)?.join("tries");
            std::fs::create_dir_all(&dir)?;
            std::fs::write(self.try_path(id, n, false)?, picture)?;
            if let Some(s) = sheet {
                std::fs::write(self.try_path(id, n, true)?, s)?;
            }
            e.tries.push(Try { n, seed, sheet: sheet.is_some(), model: model.to_string(), prompt: prompt.to_string() });
            e.next_try = n + 1;
            Ok(())
        })?;
        Ok(e)
    }

    /// Commit the editor: the fields, the picture chosen (`None` keeps the
    /// one there), the voice (`None` keeps it), and the tries emptied.
    pub fn save(
        &self,
        id: &str,
        fields: &Fields,
        pick: Option<&Pick>,
        voice: Option<&VoicePick>,
        outputs: &crate::workitems::Store,
    ) -> Result<Entry> {
        let name = fields.name.trim();
        if name.is_empty() {
            bail!("give it a name: prompts find it by its name");
        }
        if name.chars().count() > MAX_NAME || name.contains(['\n', '\r']) {
            bail!("a name is one line of at most {MAX_NAME} characters");
        }
        let taken = self.list().into_iter().any(|v| {
            v.entry.id != id && !v.entry.draft && v.entry.name.trim().to_lowercase() == name.to_lowercase()
        });
        if taken {
            bail!("the cast already has a “{name}”: prompts find an entry by its name, so each needs its own");
        }
        let dir = self.dir(id)?;
        let (e, ()) = self.update(id, |e| {
            set_fields(e, fields);
            match pick {
                None => {}
                Some(Pick::Try(n)) => {
                    let t = e.tries.iter().find(|t| t.n == *n).cloned().ok_or_else(|| anyhow!("that try is gone"))?;
                    std::fs::copy(self.try_path(id, t.n, false)?, dir.join("picture.png")).context("keeping the picture")?;
                    if t.sheet {
                        std::fs::copy(self.try_path(id, t.n, true)?, dir.join("sheet.png")).context("keeping the turnaround")?;
                    } else {
                        remove(&dir.join("sheet.png"))?;
                    }
                    e.seed = Some(t.seed);
                    e.image_model = t.model.clone();
                }
                Some(Pick::Output(stem)) => {
                    let src = outputs.find_output(stem).ok_or_else(|| anyhow!("output '{stem}' is gone"))?;
                    std::fs::copy(&src, dir.join("picture.png")).context("copying the picture in")?;
                    // A sheet was of the picture it replaces.
                    remove(&dir.join("sheet.png"))?;
                    e.seed = None;
                }
            }
            match voice {
                None => {}
                Some(VoicePick::Output(stem)) => {
                    let src = outputs.find_audio_output(stem).ok_or_else(|| anyhow!("sound '{stem}' is gone"))?;
                    std::fs::copy(&src, dir.join("voice.wav")).context("copying the voice in")?;
                }
                Some(VoicePick::Remove) => remove(&dir.join("voice.wav"))?,
            }
            // Every entry has a picture: it is what pictures of it follow,
            // and what the Cast tab shows.
            if !dir.join("picture.png").is_file() {
                bail!("draw it first (or choose a picture of your own): an entry is saved with its picture");
            }
            if pick.is_some() {
                write_thumb(&dir.join("picture.png"), &dir.join("thumb.png"))?;
            }
            clear_tries(&dir, e)?;
            e.draft = false;
            Ok(())
        })?;
        Ok(e)
    }

    /// Close the editor without saving: a draft goes, a saved entry loses
    /// only its tries.
    pub fn discard(&self, id: &str) -> Result<()> {
        let e = self.load(id)?;
        if e.draft {
            return self.delete(id);
        }
        let dir = self.dir(id)?;
        self.update(id, |e| clear_tries(&dir, e))?;
        Ok(())
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        let dir = self.dir(id)?;
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("deleting {}", dir.display())),
        }
    }
}

fn set_fields(e: &mut Entry, f: &Fields) {
    e.name = f.name.trim().to_string();
    e.kind = f.kind;
    e.look = f.look.trim().to_string();
    e.image_model = f.image_model.clone();
}

fn remove(p: &Path) -> Result<()> {
    match std::fs::remove_file(p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("deleting {}", p.display())),
    }
}

fn clear_tries(dir: &Path, e: &mut Entry) -> Result<()> {
    let tries = dir.join("tries");
    match std::fs::remove_dir_all(&tries) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err).with_context(|| format!("deleting {}", tries.display())),
    }
    e.tries.clear();
    Ok(())
}

/// The tile: the picture at the grids' height.
fn write_thumb(src: &Path, dst: &Path) -> Result<()> {
    let img = image::open(src).with_context(|| format!("reading {}", src.display()))?;
    let h = crate::workitems::THUMB_HEIGHT;
    let img = if img.height() > h { img.resize(u32::MAX, h, image::imageops::FilterType::Lanczos3) } else { img };
    img.to_rgb8().save_with_format(dst, image::ImageFormat::Png).with_context(|| format!("writing {}", dst.display()))
}

/// The words a picture is drawn from: the look, framed so the subject is alone and whole — what every later picture
/// it is a reference for can follow. The film skill's `prompt` rule, as words.
pub fn derived_prompt(kind: MemberKind, look: &str) -> String {
    let look = look.trim().trim_end_matches('.');
    let frame = match kind {
        MemberKind::Character => "Alone, full body and standing, facing the viewer, on a plain light grey background.",
        MemberKind::Place => "A wide view, with nobody in it.",
        MemberKind::Thing => "The object alone and whole, on a plain light grey background.",
    };
    if look.is_empty() { frame.to_string() } else { format!("{look}. {frame}") }
}

/// The shape it is drawn at, width over height: a person standing is tall,
/// a place wide.
fn aspect(kind: MemberKind) -> f64 {
    match kind {
        MemberKind::Character => 0.75,
        MemberKind::Place => 1.5,
        MemberKind::Thing => 1.0,
    }
}

/// A draw in the editor, for the worker.
#[derive(Debug, Clone, Deserialize)]
pub struct DrawRequest {
    pub id: String,
    #[serde(flatten)]
    pub fields: Fields,
    /// A character on a model that follows references: a turnaround sheet
    /// first, and the picture drawn from it, so the two are one person.
    #[serde(default)]
    pub turnaround: bool,
}

/// One try, drawn and kept in the entry's `tries/`: the entry as it is now.
pub fn draw(engine: &mut Engine, req: &DrawRequest) -> Result<String> {
    let cfg = engine.cfg().clone();
    let store = CastStore::of(&cfg);
    store.load(&req.id)?;
    let f = &req.fields;
    let model = f.image_model.trim();
    let profile = cfg
        .registry()
        .get(model)
        .cloned()
        .ok_or_else(|| anyhow!("pick a model to draw with ('{model}' isn't installed)"))?;
    if f.look.trim().is_empty() {
        bail!("describe it first: it is drawn from its look");
    }
    let settings = json!({ "template": model });
    let size = crate::agent::board_size(&cfg, &settings, aspect(f.kind));
    // Kept at full size, from the work item's archived (unsharpened) PNG.
    let items = crate::workitems::Store::new(&cfg.base_dir.join("work"), &cfg.user_dir());
    let bytes = |r: &crate::pipeline::GenResult| -> Vec<u8> {
        r.work_id
            .as_ref()
            .and_then(|w| std::fs::read(items.image_path(Shelf::Work, w)).ok())
            .unwrap_or_else(|| r.png.clone())
    };
    let sheet = req.turnaround && f.kind == MemberKind::Character && profile.family.takes_references();
    let (picture, sheet_png, seed, words) = if sheet {
        let who = f.look.trim();
        let sheet_size = crate::agent::board_size(&cfg, &settings, 2.0);
        let s = crate::agent::draw_one(engine, &settings, &format!("{} {who}", crate::agent::SHEET), sheet_size, &[], Vec::new())?;
        let sheet_png = bytes(&s);
        // The picture follows the sheet, which has to be a file to be one.
        let staged = crate::pipeline::scratch_dir(&cfg.base_dir.join("work")).join(format!("cast-{}-sheet.png", req.id));
        std::fs::write(&staged, &sheet_png)?;
        let front = crate::agent::draw_one(engine, &settings, &format!("{} {who}", crate::agent::FRONT), size, &[], vec![staged.clone()]);
        let _ = std::fs::remove_file(&staged);
        let front = front?;
        (bytes(&front), Some(sheet_png), front.seed, who.to_string())
    } else {
        let words = derived_prompt(f.kind, &f.look);
        let r = crate::agent::draw_one(engine, &settings, &words, size, &[], Vec::new())?;
        (bytes(&r), None, r.seed, words)
    };
    let e = store.add_try(&req.id, &picture, sheet_png.as_deref(), seed, model, &words)?;
    Ok(serde_json::to_string(&store.view(e))?)
}

// ---- Routes --------------------------------------------------------------------

/// The route for `method path`, or `None` when it isn't the cast's. Only a
/// draw goes through the job queue; the rest are file reads and writes on
/// the connection thread.
pub fn route(conn: &mut Connection, method: &str, path: &str, body: &[u8], cfg: &Config, lanes: &Lanes) -> Option<Result<()>> {
    let store = CastStore::of(cfg);
    Some(match (method, path) {
        ("GET", "/cast") => ok(conn, json!(store.list())),
        ("POST", "/cast/new") => create(conn, &store, body),
        ("POST", "/cast/draw") => draw_route(conn, &store, lanes, body),
        ("POST", "/cast/save") => save(conn, cfg, &store, body),
        ("POST", "/cast/discard") => discard(conn, &store, body),
        ("POST", "/cast/delete") => delete(conn, &store, body),
        ("GET", p) if p.starts_with("/cast/") => {
            let parts: Vec<&str> = p["/cast/".len()..].split('/').collect();
            match parts.as_slice() {
                [id] => match store.load(id) {
                    Ok(e) => ok(conn, json!(store.view(e))),
                    Err(e) => err(conn, 404, &format!("{e:#}")),
                },
                [id, f @ ("picture.png" | "sheet.png" | "thumb.png" | "voice.wav")] => {
                    media(conn, store.dir(id).ok().map(|d| d.join(f)))
                }
                [id, "tries", f] => {
                    let try_file = f.strip_suffix(".png").and_then(|s| {
                        let (n, sheet) = match s.strip_suffix("-sheet") {
                            Some(n) => (n, true),
                            None => (s, false),
                        };
                        n.parse::<u32>().ok().map(|n| (n, sheet))
                    });
                    media(conn, try_file.and_then(|(n, sheet)| store.try_path(id, n, sheet).ok()))
                }
                _ => return None,
            }
        }
        _ => return None,
    })
}

fn ok(conn: &mut Connection, v: Value) -> Result<()> {
    conn.respond(200, "application/json", v.to_string().as_bytes())
}

fn err(conn: &mut Connection, status: u16, msg: &str) -> Result<()> {
    conn.respond(status, "application/json", error_json(msg).as_bytes())
}

fn media(conn: &mut Connection, path: Option<PathBuf>) -> Result<()> {
    let ty = match path.as_ref().and_then(|p| p.extension()).and_then(|e| e.to_str()) {
        Some("wav") => "audio/wav",
        _ => "image/png",
    };
    match path.and_then(|p| std::fs::read(p).ok()) {
        Some(b) => conn.respond(200, ty, &b),
        None => conn.respond(404, "text/plain", b"not found"),
    }
}

fn create(conn: &mut Connection, store: &CastStore, body: &[u8]) -> Result<()> {
    let Some(f) = parse_json::<Fields>(conn, body)? else { return Ok(()) };
    match store.create(&f) {
        Ok(e) => ok(conn, json!(store.view(e))),
        Err(e) => err(conn, 500, &format!("{e:#}")),
    }
}

fn draw_route(conn: &mut Connection, store: &CastStore, lanes: &Lanes, body: &[u8]) -> Result<()> {
    let Some(req) = parse_json::<DrawRequest>(conn, body)? else { return Ok(()) };
    if let Err(e) = store.load(&req.id) {
        return err(conn, 404, &format!("{e:#}"));
    }
    let label = if req.fields.name.trim().is_empty() { "cast".to_string() } else { format!("cast: {}", req.fields.name.trim()) };
    crate::enqueue_wait(conn, lanes, jobs::Kind::Cast, label, Work::CastDraw(req), false, 400)
}

fn save(conn: &mut Connection, cfg: &Config, store: &CastStore, body: &[u8]) -> Result<()> {
    #[derive(Deserialize)]
    struct Dto {
        /// None: a new entry, saved without a draw (a picture of the
        /// person's own, or words alone).
        #[serde(default)]
        id: Option<String>,
        #[serde(flatten)]
        fields: Fields,
        #[serde(default)]
        picture: Option<Pick>,
        #[serde(default)]
        voice: Option<VoicePick>,
    }
    let Some(dto) = parse_json::<Dto>(conn, body)? else { return Ok(()) };
    let outputs = crate::workitems::Store::new(&cfg.base_dir.join("work"), &cfg.user_dir());
    let result = (|| -> Result<Entry> {
        let (id, fresh) = match &dto.id {
            Some(id) => (id.clone(), false),
            None => (store.create(&dto.fields)?.id, true),
        };
        let r = store.save(&id, &dto.fields, dto.picture.as_ref(), dto.voice.as_ref(), &outputs);
        // A new one that couldn't be saved leaves no draft behind.
        if r.is_err() && fresh {
            let _ = store.delete(&id);
        }
        r
    })();
    match result {
        Ok(e) => ok(conn, json!(store.view(e))),
        Err(e) => err(conn, 400, &format!("{e:#}")),
    }
}

#[derive(Deserialize)]
struct IdDto {
    id: String,
}

fn discard(conn: &mut Connection, store: &CastStore, body: &[u8]) -> Result<()> {
    let Some(dto) = parse_json::<IdDto>(conn, body)? else { return Ok(()) };
    match store.discard(&dto.id) {
        Ok(()) => ok(conn, json!({ "ok": true })),
        Err(e) => err(conn, 400, &format!("{e:#}")),
    }
}

fn delete(conn: &mut Connection, store: &CastStore, body: &[u8]) -> Result<()> {
    #[derive(Deserialize)]
    struct Dto {
        ids: Vec<String>,
    }
    let Some(dto) = parse_json::<Dto>(conn, body)? else { return Ok(()) };
    for id in &dto.ids {
        if let Err(e) = store.delete(id) {
            return err(conn, 400, &format!("{e:#}"));
        }
    }
    ok(conn, json!({ "deleted": dto.ids.len() }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(w, h, image::Rgb([200, 100, 50]));
        let mut out = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
        out
    }

    fn temp() -> PathBuf {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("yasdig-cast-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn fields(name: &str) -> Fields {
        Fields { name: name.into(), look: format!("{name} is a weathered cowboy."), ..Default::default() }
    }

    #[test]
    fn saving_keeps_the_chosen_try_and_empties_the_rest() {
        let d = temp();
        let store = CastStore::new(&d.join("user"));
        let outputs = crate::workitems::Store::new(&d.join("work"), &d.join("user"));
        let e = store.create(&fields("Elias")).unwrap();
        assert!(e.draft);
        store.add_try(&e.id, &png(8, 12), None, 1, "m", "p").unwrap();
        store.add_try(&e.id, &png(12, 8), Some(&png(16, 8)), 2, "m", "p").unwrap();
        let saved = store.save(&e.id, &fields("Elias"), Some(&Pick::Try(1)), None, &outputs).unwrap();
        assert!(!saved.draft);
        assert!(saved.tries.is_empty());
        assert_eq!(saved.seed, Some(2));
        let dir = store.dir(&e.id).unwrap();
        assert_eq!(image::open(dir.join("picture.png")).unwrap().width(), 12);
        assert!(dir.join("sheet.png").is_file());
        assert!(dir.join("thumb.png").is_file());
        assert!(!dir.join("tries").exists());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn discarding_drops_a_draft_but_only_a_saved_entrys_tries() {
        let d = temp();
        let store = CastStore::new(&d.join("user"));
        let outputs = crate::workitems::Store::new(&d.join("work"), &d.join("user"));
        let draft = store.create(&fields("Sally")).unwrap();
        store.discard(&draft.id).unwrap();
        assert!(!store.dir(&draft.id).unwrap().exists());

        let e = store.create(&fields("Elias")).unwrap();
        store.add_try(&e.id, &png(8, 8), None, 1, "m", "p").unwrap();
        store.save(&e.id, &fields("Elias"), Some(&Pick::Try(0)), None, &outputs).unwrap();
        store.add_try(&e.id, &png(4, 4), None, 2, "m", "p").unwrap();
        store.discard(&e.id).unwrap();
        let kept = store.load(&e.id).unwrap();
        assert!(kept.tries.is_empty());
        assert_eq!(image::open(store.dir(&e.id).unwrap().join("picture.png")).unwrap().width(), 8);
        // Numbering goes on, so a cached try URL never shows another picture.
        assert_eq!(store.add_try(&e.id, &png(4, 4), None, 3, "m", "p").unwrap().tries[0].n, 2);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn two_saved_entries_never_share_a_name() {
        let d = temp();
        let store = CastStore::new(&d.join("user"));
        let outputs = crate::workitems::Store::new(&d.join("work"), &d.join("user"));
        let a = store.create(&fields("Elias")).unwrap();
        store.add_try(&a.id, &png(8, 8), None, 1, "m", "p").unwrap();
        store.save(&a.id, &fields("Elias"), Some(&Pick::Try(0)), None, &outputs).unwrap();
        let b = store.create(&fields("elias ")).unwrap();
        store.add_try(&b.id, &png(8, 8), None, 1, "m", "p").unwrap();
        let e = store.save(&b.id, &fields("elias "), Some(&Pick::Try(0)), None, &outputs).unwrap_err();
        assert!(format!("{e:#}").contains("already has"), "{e:#}");
        // Saving the same entry again under its own name is fine.
        store.save(&a.id, &fields("Elias"), None, None, &outputs).unwrap();
        assert!(store.save(&b.id, &fields(""), None, None, &outputs).is_err());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn an_entry_is_never_saved_without_a_picture() {
        let d = temp();
        let store = CastStore::new(&d.join("user"));
        let outputs = crate::workitems::Store::new(&d.join("work"), &d.join("user"));
        let e = store.create(&fields("Elias")).unwrap();
        let err = store.save(&e.id, &fields("Elias"), None, None, &outputs).unwrap_err();
        assert!(format!("{err:#}").contains("draw it first"), "{err:#}");
        assert!(store.load(&e.id).unwrap().draft);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn an_id_that_is_not_whitelisted_never_becomes_a_path() {
        let store = CastStore::new(Path::new("/nowhere"));
        assert!(store.dir("../etc").is_err());
        assert!(store.dir("a/b").is_err());
        assert!(store.dir("c123-1").is_ok());
    }

    #[test]
    fn the_picture_is_drawn_from_the_look_framed_for_its_kind() {
        let p = derived_prompt(MemberKind::Character, "Elias is a weathered cowboy.");
        assert!(p.starts_with("Elias is a weathered cowboy. Alone"), "{p}");
        assert!(derived_prompt(MemberKind::Place, "Dark ruins").contains("nobody"));
        assert!(!derived_prompt(MemberKind::Thing, "").starts_with('.'));
    }
}
