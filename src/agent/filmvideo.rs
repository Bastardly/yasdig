//! A film's Video (`Film.video`): the storyboard is the board.
//!
//! A Video chat's film used to keep its own copy of the shots, synced both
//! ways with one clip's rows on the Video tab (`board.js`), and every edit
//! made on one side while the other was busy was lost. Now the film writes
//! its own Video — scenes, shots, each shot's parts — and reads it back:
//!
//! - [`push`] after every change the agent makes (a story turn, a part
//!   written out in full, a first frame or cast picture drawn): a shot not
//!   generated is written as its plan, a generated one as its draft, so its
//!   frames stay and the storyboard marks it changed.
//! - [`pull`] before every turn and board press: the person's storyboard
//!   edits (words, order, shots added or deleted, scene descriptions) are
//!   what the agent sees, matched to the film's parts by shot id.
//!
//! Only one side writes at a time: the Video tab holds the film's Video
//! inert while the chat's job runs, and reloads it when the job ends.

use super::chat::{ChatStore, Film, FilmScene, FilmShot, ImageRef};
use super::session::{self, RowIn};
use crate::config::Config;
use crate::story::{Scene, Video};
use crate::videos::Clip;
use anyhow::{bail, Context as _, Result};

/// A Video shot's rows as the Shot panel has them: its draft's, else its
/// parts — part 1's picture is the shot's first frame (`source`).
pub fn rows_of(c: &Clip) -> Vec<RowIn> {
    if let Some(rows) = c.draft.as_ref().and_then(|d| d.get("rows")) {
        if let Ok(rows) = serde_json::from_value::<Vec<RowIn>>(rows.clone()) {
            return rows;
        }
    }
    c.segments
        .iter()
        .enumerate()
        .map(|(j, s)| RowIn {
            text: s.text.clone(),
            seed: s.seed,
            frames: (s.frames > 0).then_some(s.frames),
            cast: s.words.cast.clone(),
            lines: s.words.lines.clone(),
            sound: s.words.sound.clone(),
            music: s.words.music.clone(),
            key: if j == 0 { c.source.clone() } else { None },
            refs: s.references.clone(),
            audio: s.audio.clone(),
            key_ref: None,
        })
        .collect()
}

/// The film read back from its Video: scenes as the storyboard has them,
/// and every shot's parts in storyboard order — matched to the film's own by
/// shot id, so a part keeps its story words, first frame and picks while it
/// says the same (`session::row_shot`). A shot the person added is new
/// parts; one they deleted is gone. A Video that is gone frees the film to
/// make a new one. `keys` is the chat copy of a first frame the board
/// didn't draw, per stem (`keep_keys`). Answers the names the cast doesn't
/// have, which the parts leave out.
pub fn read_back(f: &mut Film, v: &Video, keys: &[(String, ImageRef)]) -> Vec<String> {
    let names: Vec<String> = f.cast.iter().map(|m| m.name.clone()).collect();
    let mut unknown = Vec::new();
    let mut shots: Vec<FilmShot> = Vec::new();
    // A film from before its Video was written by `push` (an old chat's
    // clip, converted shot by shot at its cuts) has no ids: matched by place.
    let by_place = f.shots.iter().all(|s| s.shot.is_none());
    let groups = f.groups();
    let mut n = 0;
    for (si, sc) in v.scenes.iter().enumerate() {
        for sid in &sc.shots {
            let Some(c) = v.clips.iter().find(|c| &c.id == sid) else { continue };
            let olds: Vec<&FilmShot> = if by_place {
                groups.get(n).map_or(Vec::new(), |g| f.shots[g.clone()].iter().collect())
            } else {
                f.shots.iter().filter(|s| s.shot.as_deref() == Some(sid.as_str())).collect()
            };
            n += 1;
            let mut rows: Vec<RowIn> = rows_of(c).into_iter().filter(|r| !r.text.trim().is_empty()).collect();
            // A shot with no words yet (the person's + Shot) stays in the film
            // as one empty part, so the agent is shown it and can fill it. Its
            // first row is kept as it is: a first frame picked before any words.
            let empty = rows.is_empty();
            if empty {
                rows.push(rows_of(c).into_iter().next().unwrap_or_else(|| serde_json::from_str("{}").expect("a row's fields all have defaults")));
            }
            for (j, row) in rows.iter().enumerate() {
                let mut row = row.clone();
                if let Some((_, r)) = row.key.as_ref().and_then(|k| keys.iter().find(|(s, _)| s == k)) {
                    row.key_ref = Some(r.clone());
                }
                let first = shots.is_empty();
                let mut part = session::row_shot(f, &names, &row, j == 0, first, olds.get(j).copied(), &mut unknown);
                part.scene = si;
                part.shot = Some(sid.clone());
                part.drawn = c.rendered;
                part.enhance = olds.get(j).is_some_and(|o| o.enhance && o.text == part.text);
                if empty {
                    part.brief.clear();
                }
                shots.push(part);
            }
        }
    }
    f.scenes = v.scenes.iter().map(|s| FilmScene { id: s.id.clone(), title: s.title.clone(), every: s.every.clone() }).collect();
    if !v.title.trim().is_empty() {
        f.title = v.title.clone();
    }
    f.shots = shots;
    unknown
}

/// The first frames in a Video's shots that the film didn't draw (the
/// person's picks), to copy into the chat as a picked picture is.
fn picked_keys(f: &Film, v: &Video) -> Vec<String> {
    let ours: Vec<&str> = f.shots.iter().filter_map(|s| s.key_stem.as_deref()).collect();
    let mut out = Vec::new();
    for c in &v.clips {
        for r in rows_of(c) {
            if let Some(k) = r.key.filter(|k| !k.is_empty() && !ours.contains(&k.as_str()) && !out.contains(k)) {
                out.push(k);
            }
        }
    }
    out
}

/// Each picked first frame copied into the chat (`img/`), as `/chats/board`
/// used to: the board shows it, and it survives the output's deletion.
fn keep_keys(cfg: &Config, store: &ChatStore, id: &str, stems: &[String]) -> Vec<(String, ImageRef)> {
    let mut out = Vec::new();
    for stem in stems {
        let Ok(bytes) = super::attach_bytes(cfg, "output", stem) else { continue };
        let file = format!("k{}-{}.png", super::chat::now_ms(), out.len());
        if super::keep_picture(store, id, &file, &bytes).is_ok() {
            out.push((stem.clone(), ImageRef { kind: "output".into(), id: stem.clone(), file, seed: None }));
        }
    }
    out
}

/// [`read_back`] for chat `id`, when it has a film with a Video.
pub fn pull(cfg: &Config, store: &ChatStore, id: &str) -> Result<()> {
    let chat = store.load(id)?;
    let Some(vid) = chat.film.as_ref().and_then(|f| f.video.clone()) else { return Ok(()) };
    let videos = crate::video_store(cfg);
    if !gone(&videos, &vid) {
        let v = videos.load_video_full(&vid).context("reading the film's video")?;
        let picked = chat.film.as_ref().map(|f| picked_keys(f, &v)).unwrap_or_default();
        let keys = keep_keys(cfg, store, id, &picked);
        store.update(id, |c| {
            if let Some(f) = c.film.as_mut() {
                read_back(f, &v, &keys);
            }
            Ok(())
        })?;
        return Ok(());
    }
    {
        // Deleted, or cleared by Close server: the next write makes another.
        store.update(id, |c| {
            if let Some(f) = c.film.as_mut() {
                f.video = None;
                f.drop.clear();
                for s in &mut f.shots {
                    s.shot = None;
                    s.drawn = false;
                }
                for s in &mut f.scenes {
                    s.id.clear();
                }
            }
            Ok(())
        })?;
    }
    Ok(())
}

/// Whether Video `vid` is gone (deleted, or cleared by Close server) — not
/// merely unreadable, which is an error, never a reason to make another.
fn gone(videos: &crate::videos::VideoStore, vid: &str) -> bool {
    !videos.dir(vid).join("video.json").is_file()
}

/// Whether two shots' rows say the same: every field the Shot panel holds.
fn same_rows(a: &[RowIn], b: &[RowIn]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| {
            x.text.trim() == y.text.trim()
                && x.seed == y.seed
                && x.frames == y.frames
                && x.cast == y.cast
                && x.lines == y.lines
                && x.sound.trim() == y.sound.trim()
                && x.music.trim() == y.music.trim()
                && x.key.as_deref().filter(|k| !k.is_empty()) == y.key.as_deref().filter(|k| !k.is_empty())
                && x.refs == y.refs
                && x.audio == y.audio
        })
}

/// What [`write`] did that the chat should say: generated shots the story
/// left out, kept.
#[derive(Debug, Default, PartialEq)]
pub struct Written {
    pub video: String,
    pub kept: Vec<String>,
}

/// The film written into its Video, made first when it has none: a scene per
/// film scene (a locked one keeps its description and negative — its
/// generated shots were made with them), a shot per group of parts, planned
/// when not generated and drafted when its rows differ. Shots the story left
/// out are removed only when listed in `Film.drop` and still not generated;
/// any other stays where it was. Answers the film with the Video's scene and
/// shot ids filled in, and the generated shots it kept.
pub fn write(cfg: &Config, settings: Option<&serde_json::Value>, f: &Film) -> Result<(Film, Written)> {
    let videos = crate::video_store(cfg);
    let items = crate::store(cfg);
    let Some(m) = cfg.video_models().into_iter().find(|m| m.name == f.video_model) else {
        bail!("the film's video model '{}' isn't installed", f.video_model);
    };
    if f.shots.is_empty() {
        bail!("the film has no shots");
    }
    // The Video, made when there is none (or it was deleted).
    let mut v = match f.video.as_deref().filter(|id| !gone(&videos, id)) {
        Some(id) => videos.load_video_full(id).context("reading the film's video")?,
        None => {
            let seed = crate::pipeline::random_seed();
            let v = Video {
                id: crate::workitems::new_id(seed),
                created_ms: crate::pipeline::now_ms(),
                title: f.title.clone(),
                lock: None,
                scenes: Vec::new(),
                favorite: false,
                clips: Vec::new(),
            };
            videos.create_video(&v)?;
            v
        }
    };
    // The Generation panel as the person last sent it — only when it was
    // for the model the shots are made with (a story may pick another; its
    // steps and size would be the wrong model's), else that model's own
    // defaults. Once a shot is generated, the lock's model, size, fps and
    // Hi-res.
    let model = v.lock.as_ref().map_or(f.video_model.clone(), |l| l.model.clone());
    let mut body = settings
        .filter(|s| s.get("model").and_then(|x| x.as_str()) == Some(model.as_str()))
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    body["model"] = serde_json::json!(model);
    body["shots"] = serde_json::json!([]);
    let dto: crate::VideoDto = serde_json::from_value(body).context("reading the Video tab's settings")?;
    let mut base = dto.request();
    base.into = None;
    base.source = None;
    if let Some(l) = &v.lock {
        base.model = l.model.clone();
        base.width = Some(l.width);
        base.height = Some(l.height);
        base.fps = Some(l.fps);
        base.hires = Some(l.hires);
    }

    let rendered = |id: &str| v.clips.iter().any(|c| c.id == id && c.rendered);
    let film_scenes: Vec<FilmScene> = if f.scenes.is_empty() {
        vec![FilmScene { id: String::new(), title: String::new(), every: f.every.clone() }]
    } else {
        f.scenes.clone()
    };
    let mut out = f.clone();
    let mut scenes: Vec<Scene> = Vec::new();
    for (si, fs) in film_scenes.iter().enumerate() {
        let had = v.scenes.iter().find(|s| !fs.id.is_empty() && s.id == fs.id);
        let locked = had.is_some_and(|s| s.shots.iter().any(|x| rendered(x)));
        let id = match had {
            Some(s) => s.id.clone(),
            None => {
                let mut n = scenes.len() + 1;
                while v.scenes.iter().chain(scenes.iter()).any(|s| s.id == format!("c{n}")) {
                    n += 1;
                }
                format!("c{n}")
            }
        };
        scenes.push(Scene {
            id: id.clone(),
            title: fs.title.clone(),
            every: if locked { had.map_or(String::new(), |s| s.every.clone()) } else { fs.every.clone() },
            negative: had.map_or_else(|| m.negative_prompt.clone(), |s| s.negative.clone()),
            seed: had.map_or_else(crate::pipeline::random_seed, |s| s.seed),
            shots: Vec::new(),
        });
        if let Some(s) = out.scenes.get_mut(si) {
            s.id = id;
        } else if f.scenes.is_empty() {
            out.scenes.push(FilmScene { id, ..fs.clone() });
        }
    }

    // A shot per group of parts, in its scene, under the id it had.
    let listed: Vec<String> = v.shot_ids().cloned().collect();
    for g in f.groups() {
        let si = f.shots[g.start].scene.min(scenes.len() - 1);
        let had = f.shots[g.start].shot.clone().filter(|id| listed.contains(id));
        let sid = match had {
            Some(id) => id,
            // Past every id listed or on disk: each new plan is written
            // before the next id is drawn.
            None => videos.new_shot_id(&v),
        };
        let mut specs = Vec::new();
        let mut rows = Vec::new();
        for k in g.clone() {
            let (mut spec, _) = session::shot_spec(f, k);
            spec.cut = false;
            spec.keyframe = None;
            specs.push(spec);
            rows.push(session::row_json(f, k));
        }
        let key = session::shot_spec(f, g.start).1;
        let scene = &scenes[si];
        match v.clips.iter().find(|c| c.id == sid) {
            // Generated: its frames stay. Left alone while the film says
            // what the shot's rows (its draft's, else its parts) say — so a
            // draft of the person's is never dropped — else drafted, or the
            // draft dropped when the film is back to what was generated.
            Some(c) if c.rendered => {
                let ours: Vec<RowIn> = rows.iter().filter_map(|r| serde_json::from_value(r.clone()).ok()).collect();
                let now: Vec<RowIn> = rows_of(c).into_iter().filter(|r| !r.text.trim().is_empty()).collect();
                if !same_rows(&now, &ours) {
                    let made = rows_of(&Clip { draft: None, ..c.clone() });
                    // The person's own flow shift for the shot rides along.
                    let shift = c.draft.as_ref().and_then(|d| d.get("shift")).cloned().filter(|x| !x.is_null());
                    let draft = (!same_rows(&made, &ours) || shift.is_some()).then(|| {
                        let mut d = serde_json::json!({ "rows": rows });
                        if let Some(x) = shift {
                            d["shift"] = x;
                        }
                        d
                    });
                    videos.save_draft(&sid, draft.as_ref())?;
                }
            }
            had => {
                let mut req = base.clone();
                req.flow_shift_own = had.and_then(|c| c.flow_shift_own);
                req.shots = specs;
                req.source = key;
                req.every = Some(scene.every.clone());
                req.negative = Some(scene.negative.clone());
                req.every_seed = Some(scene.seed);
                req.into = Some(sid.clone());
                crate::pipeline::plan_clip(cfg, &items, &videos, &req)
                    .with_context(|| format!("writing shot {} of the film", sid))?;
            }
        }
        scenes[si].shots.push(sid.clone());
        for k in g {
            out.shots[k].shot = Some(sid.clone());
            out.shots[k].scene = si;
        }
    }

    // What the film doesn't have: a plan the story dropped goes; the rest
    // stay where they were — after the shot they followed, in their scene —
    // and a generated one is said (`push` then reads it into the film). A
    // shot the person added with no words yet is one of these, silently.
    let placed: Vec<String> = scenes.iter().flat_map(|s| s.shots.clone()).collect();
    let mut kept = Vec::new();
    for old in &v.scenes {
        for (j, sid) in old.shots.iter().enumerate().filter(|(_, s)| !placed.contains(s)) {
            if f.drop.contains(sid) && !rendered(sid) {
                let _ = std::fs::remove_dir_all(videos.dir(sid));
                continue;
            }
            if rendered(sid) {
                kept.push(sid.clone());
            }
            let home = match scenes.iter().position(|s| s.id == old.id) {
                Some(i) => i,
                None => {
                    scenes.push(Scene { shots: Vec::new(), ..old.clone() });
                    scenes.len() - 1
                }
            };
            let at = old.shots[..j].iter().rev().find_map(|b| scenes[home].shots.iter().position(|x| x == b)).map_or(0, |i| i + 1);
            scenes[home].shots.insert(at, sid.clone());
        }
    }
    let title = f.title.clone();
    v = videos.update_video(&v.id, |rec| {
        rec.scenes = scenes.clone();
        if !title.trim().is_empty() {
            rec.title = title.clone();
        }
        Ok(())
    })?;
    out.video = Some(v.id.clone());
    out.drop.clear();
    Ok((out, Written { video: v.id, kept }))
}

/// [`write`] for chat `id`, then the Video read back into the film
/// ([`pull`]): the ids it gave, and a generated shot the story left out is
/// the film's again — so it is said once, and the next write places it. Runs
/// where nothing else writes the film meanwhile: the worker's own step, or a
/// route refused while the chat is busy.
pub fn push(cfg: &Config, store: &ChatStore, id: &str) -> Result<()> {
    let chat = store.load(id)?;
    let Some(f) = chat.film.as_ref().filter(|f| !f.shots.is_empty()) else { return Ok(()) };
    let (film, w) = write(cfg, chat.video_settings.as_ref(), f)?;
    store.update(id, |c| {
        if let Some(cur) = c.film.as_mut() {
            cur.video = film.video.clone();
            cur.drop.clear();
        }
        if !w.kept.is_empty() {
            let one = w.kept.len() == 1;
            c.note(format!(
                "{} generated shot{} the story left out {} kept in the video, where {} {} — delete {} in the \
                 storyboard if you don't want {}.",
                w.kept.len(),
                if one { "" } else { "s" },
                if one { "is" } else { "are" },
                if one { "it" } else { "they" },
                if one { "was" } else { "were" },
                if one { "it" } else { "them" },
                if one { "it" } else { "them" },
            ));
        }
        Ok(())
    })?;
    pull(cfg, store, id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::videos::Segment;

    fn shot(id: &str, rendered: bool, parts: &[&str]) -> Clip {
        let mut c = crate::videos::tests::clip(id, 1);
        c.rendered = rendered;
        c.source = None;
        c.segments = parts.iter().map(|t| Segment { text: t.to_string(), seed: 5, frames: 33, ..Default::default() }).collect();
        c
    }

    fn scene(id: &str, every: &str, shots: &[&str]) -> Scene {
        Scene { id: id.into(), title: id.to_uppercase(), every: every.into(), negative: String::new(), seed: 1, shots: shots.iter().map(|s| s.to_string()).collect() }
    }

    /// The storyboard is what the film becomes: shots in storyboard order,
    /// a generated shot's draft over its parts, a deleted shot gone, and a
    /// part that says what the story's part said keeps its story words.
    #[test]
    fn the_film_reads_back_its_video_by_shot_id() {
        let mut f = Film {
            shots: vec![
                FilmShot { text: "Mara opens the door, in full.".into(), brief: "Mara opens the door.".into(), shot: Some("v-s1".into()), ..Default::default() },
                FilmShot { text: "Gone.".into(), cut: true, shot: Some("v-s9".into()), ..Default::default() },
            ],
            ..Default::default()
        };
        let mut s2 = shot("v-s2", true, &["Leo waits."]);
        s2.draft = Some(serde_json::json!({ "rows": [{ "text": "Leo waits in the rain." }, { "text": "The bus comes." }] }));
        let v = Video {
            id: "v".into(),
            created_ms: 1,
            title: "Rain".into(),
            lock: None,
            scenes: vec![scene("c1", "warm", &["v-s1"]), scene("c2", "grey", &["v-s2"])],
            favorite: false,
            clips: vec![shot("v-s1", false, &["Mara opens the door, in full.", "She steps out."]), s2],
        };
        read_back(&mut f, &v, &[]);
        assert_eq!(f.groups(), [0..2, 2..4]);
        let ids: Vec<Option<&str>> = f.shots.iter().map(|s| s.shot.as_deref()).collect();
        assert_eq!(ids, [Some("v-s1"), Some("v-s1"), Some("v-s2"), Some("v-s2")]);
        assert_eq!(f.shots[0].brief, "Mara opens the door.", "the story's words stand behind an untouched part");
        assert_eq!((f.shots[2].text.as_str(), f.shots[2].drawn, f.shots[2].scene), ("Leo waits in the rain.", true, 1));
        assert_eq!((f.scene_every(1), f.title.as_str()), ("grey", "Rain"));
    }

    #[test]
    fn a_shot_the_person_left_empty_is_shown_to_the_agent() {
        let mut f = Film { shots: vec![part("Mara waits.", 0, true)], ..Default::default() };
        f.shots[0].shot = Some("v-s1".into());
        let v = Video {
            id: "v".into(),
            created_ms: 1,
            title: String::new(),
            lock: None,
            scenes: vec![scene("c1", "warm", &["v-s1", "v-s8"])],
            favorite: false,
            clips: vec![shot("v-s1", false, &["Mara waits."]), shot("v-s8", false, &[""])],
        };
        read_back(&mut f, &v, &[]);
        assert_eq!(f.groups(), [0..1, 1..2], "the empty shot is a shot of its own");
        assert_eq!(f.shots[1].shot.as_deref(), Some("v-s8"));
        assert!(!f.needs_keyframe(1), "no words, nothing to draw a first frame from");
        let board = super::super::context::board_text(&f);
        assert!(board.contains("Shot s8:\n  - (empty"), "{board}");
    }

    fn cfg(dir: &std::path::Path) -> Config {
        let src = r#"
[sd]
binary = "sd-cli"

[[video]]
name = "5b"
family = "wan22_ti2v_5b"
diffusion_model = "d.safetensors"
vae = "v.safetensors"
t5xxl = "t.gguf"
"#;
        let mut c: Config = toml::from_str(src).unwrap();
        c.base_dir = dir.to_path_buf();
        c
    }

    fn part(text: &str, scene: usize, cut: bool) -> FilmShot {
        FilmShot { text: text.into(), scene, cut, seed: 3, ..Default::default() }
    }

    /// A film's first write makes its Video: a scene per scene, a planned
    /// shot per group of parts. A later write leaves a generated shot's
    /// frames and drafts its change, and removes only a plan the story
    /// dropped — a generated shot it left out stays.
    #[test]
    fn a_film_writes_its_own_video_and_never_loses_a_generated_shot() {
        let dir = std::env::temp_dir().join(format!("yasdig-filmvideo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = cfg(&dir);
        let mut f = Film {
            title: "Rain".into(),
            video_model: "5b".into(),
            scenes: vec![
                FilmScene { id: String::new(), title: "Home".into(), every: "warm".into() },
                FilmScene { id: String::new(), title: "Stop".into(), every: "grey".into() },
            ],
            shots: vec![part("Mara opens the door.", 0, false), part("She steps out.", 0, false), part("She looks back.", 0, true), part("Leo waits.", 1, true)],
            ..Default::default()
        };
        let (out, w) = write(&cfg, None, &f).unwrap();
        let videos = crate::video_store(&cfg);
        let v = videos.load_video_full(&w.video).unwrap();
        let shots: Vec<usize> = v.scenes.iter().map(|s| s.shots.len()).collect();
        assert_eq!(shots, [2, 1]);
        assert_eq!((v.scenes[0].every.as_str(), v.title.as_str()), ("warm", "Rain"));
        let first = v.clips.iter().find(|c| c.id == v.scenes[0].shots[0]).unwrap();
        assert_eq!((first.segments.len(), first.rendered), (2, false));
        assert_eq!(out.shots[1].shot, out.shots[0].shot, "a shot's parts share its id");
        assert!(out.scenes.iter().all(|s| !s.id.is_empty()));

        // Shot 1 generated (its record says so); shot 2 dropped by the story,
        // shot 3 left out without being dropped is kept anyway as generated.
        let s1 = out.shots[0].shot.clone().unwrap();
        let s2 = out.shots[2].shot.clone().unwrap();
        let s3 = out.shots[3].shot.clone().unwrap();
        for id in [&s1, &s3] {
            let path = videos.dir(id).join("clip.json");
            let mut rec: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            rec["rendered"] = serde_json::json!(true);
            std::fs::write(&path, serde_json::to_vec(&rec).unwrap()).unwrap();
        }
        f = out;
        f.shots.truncate(2);
        f.shots[1].text = "She runs out.".into();
        f.drop = vec![s2.clone(), s3.clone()];
        f.scenes[0].every = "cold".into();
        let (_, w2) = write(&cfg, None, &f).unwrap();
        let v = videos.load_video_full(&w.video).unwrap();
        assert!(!videos.dir(&s2).exists(), "a dropped plan goes");
        assert_eq!(w2.kept, std::slice::from_ref(&s3), "a generated shot stays");
        assert_eq!(v.scenes[1].shots, [s3]);
        let c1 = v.clips.iter().find(|c| c.id == s1).unwrap();
        assert_eq!(c1.segments[1].text, "She steps out.", "its frames' words stand");
        assert_eq!(c1.draft.as_ref().unwrap()["rows"][1]["text"], "She runs out.");
        assert_eq!(v.scenes[0].every, "warm", "a locked scene keeps its description");
        std::fs::remove_dir_all(&dir).ok();
    }

    fn tmp(name: &str) -> (std::path::PathBuf, Config) {
        let dir = std::env::temp_dir().join(format!("yasdig-filmvideo-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let c = cfg(&dir);
        (dir, c)
    }

    fn generated(cfg: &Config, id: &str) {
        let path = crate::video_store(cfg).dir(id).join("clip.json");
        let mut rec: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        rec["rendered"] = serde_json::json!(true);
        std::fs::write(&path, serde_json::to_vec(&rec).unwrap()).unwrap();
    }

    fn drawn(stem: &str) -> Option<ImageRef> {
        Some(ImageRef { kind: "item".into(), id: "w1".into(), file: format!("{stem}.png"), seed: None })
    }

    /// A film with its cast, two scenes and three shots, first frames drawn.
    fn film() -> Film {
        let member = |name: &str, stem: &str| super::super::chat::Member {
            name: name.into(),
            stem: Some(stem.into()),
            voice: Some(format!("{stem}-voice")),
            ..Default::default()
        };
        let mut shots = vec![
            FilmShot { cast: vec!["Mara".into()], key_stem: Some("k1".into()), keyframe: drawn("k1"), key_cast: Some(vec!["m|".into()]), ..part("Mara opens the door.", 0, false) },
            FilmShot { frames: Some(49), ..part("She steps out.", 0, false) },
            FilmShot { cast: vec!["Leo".into()], brief: "Leo waits.".into(), frames: Some(33), ..part("Leo waits at the stop, in full.", 1, true) },
        ];
        shots[0].frames = Some(81);
        Film {
            title: "Rain".into(),
            video_model: "5b".into(),
            cast: vec![member("Mara", "m"), member("Leo", "l")],
            scenes: vec![
                FilmScene { id: String::new(), title: "Home".into(), every: "warm".into() },
                FilmScene { id: String::new(), title: "Stop".into(), every: "grey".into() },
            ],
            shots,
            ..Default::default()
        }
    }

    /// What `push` does after `write`: the Video read back into the film.
    fn round(cfg: &Config, f: &Film) -> Film {
        let (out, w) = write(cfg, None, f).unwrap();
        let v = crate::video_store(cfg).load_video_full(&w.video).unwrap();
        let mut back = out.clone();
        read_back(&mut back, &v, &[]);
        back
    }

    /// Written and read back, a film is itself: its first frames drawn, its
    /// story words, lengths and seeds — on Wan, and on H3, where part 1
    /// carries the cast's pictures and a drawn first frame waits for a
    /// switch back rather than reading as cleared.
    #[test]
    fn a_film_written_and_read_back_is_unchanged() {
        for h3 in [false, true] {
            let (dir, cfg) = tmp(if h3 { "rt-h3" } else { "rt-wan" });
            let mut f = film();
            f.references = h3;
            let once = round(&cfg, &f);
            let twice = round(&cfg, &once);
            assert_eq!(once.shots.len(), 3);
            for (a, b) in f.shots.iter().zip(&once.shots) {
                assert_eq!((&a.text, &a.brief, a.frames, &a.key_stem, &a.keyframe, a.no_key, a.cut, a.scene), (&b.text, &b.brief, b.frames, &b.key_stem, &b.keyframe, b.no_key, b.cut, b.scene), "h3 {h3}");
                assert_eq!((&a.references, &a.audio, &a.cast), (&b.references, &b.audio, &b.cast), "h3 {h3}: the cast's own come back out");
            }
            assert_eq!(once, twice, "h3 {h3}: a second round changes nothing");
            std::fs::remove_dir_all(&dir).ok();
        }
    }

    /// A draft of the person's on a generated shot — here only a new length
    /// — survives the film reading it in and writing again.
    #[test]
    fn a_persons_draft_survives_a_pull_and_a_push() {
        let (dir, cfg) = tmp("draft");
        let videos = crate::video_store(&cfg);
        let f = round(&cfg, &film());
        let s1 = f.shots[0].shot.clone().unwrap();
        generated(&cfg, &s1);
        let v = videos.load_video_full(f.video.as_deref().unwrap()).unwrap();
        let mut rows: Vec<serde_json::Value> =
            rows_of(v.clips.iter().find(|c| c.id == s1).unwrap()).iter().map(|r| serde_json::to_value(r).unwrap()).collect();
        rows[1]["frames"] = serde_json::json!(33);
        videos.save_draft(&s1, Some(&serde_json::json!({ "rows": rows, "shift": 7.0 }))).unwrap();
        let v = videos.load_video_full(f.video.as_deref().unwrap()).unwrap();
        let mut back = f.clone();
        read_back(&mut back, &v, &[]);
        assert_eq!(back.shots[1].frames, Some(33));
        write(&cfg, None, &back).unwrap();
        let c = videos.load(&s1).unwrap();
        let d = c.draft.expect("the draft stays");
        assert_eq!((d["rows"][1]["frames"].clone(), d["shift"].clone()), (serde_json::json!(33), serde_json::json!(7.0)));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A shot added early in a long Video takes an id past every other, so
    /// it never lands on one (`s13` sorts before `s3` as text).
    #[test]
    fn a_new_shot_in_a_long_video_takes_a_new_id() {
        let (dir, cfg) = tmp("ids");
        let mut f = film();
        f.scenes.truncate(1);
        f.shots = (0..12).map(|n| part(&format!("Shot {n}."), 0, n > 0)).collect();
        let mut f = round(&cfg, &f);
        f.shots.insert(1, part("A new one.", 0, true));
        let f = round(&cfg, &f);
        let mut ids: Vec<String> = f.shots.iter().filter_map(|s| s.shot.clone()).collect();
        assert_eq!(ids.len(), 13);
        assert!(ids[1].ends_with("-s13"), "{ids:?}");
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 13, "every shot its own");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// An old chat's film names its converted clip and no shot ids: read
    /// back by place, it keeps its story words and first frames.
    #[test]
    fn an_old_films_parts_are_matched_by_place() {
        let (dir, cfg) = tmp("old");
        let f = film();
        let (_, w) = write(&cfg, None, &f).unwrap();
        let v = crate::video_store(&cfg).load_video_full(&w.video).unwrap();
        let mut old = f.clone();
        old.video = Some(w.video.clone());
        read_back(&mut old, &v, &[]);
        assert_eq!(old.shots[2].brief, "Leo waits.");
        assert_eq!(old.shots[0].keyframe, drawn("k1"));
        assert!(old.shots.iter().all(|s| s.shot.is_some()));
        std::fs::remove_dir_all(&dir).ok();
    }
}
