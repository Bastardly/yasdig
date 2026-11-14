//! A **Video**: scenes of shots, each shot a clip of its own.
//!
//! ```text
//!   work/videos/{video}/video.json     the order: scenes, their words, the lock
//!   work/videos/{video}/s{n}/          shot n — a clip folder as `videos.rs` has it
//! ```
//!
//! The levels are film's: a **Video** holds **scenes**, a scene holds
//! **shots** (one unbroken camera run; a cut is the join between two), and a
//! shot holds **parts** — its continuations, the `segments` of its clip. A
//! shot is exactly what a clip always was: a run of parts on one model, each
//! going on from the last frame. So everything a clip does (plans, drafts,
//! Extend, Redo, Enlarge, sound) a shot does unchanged, under the id
//! `{video}-s{n}` ([`shot_of`]), which [`VideoStore::dir`] maps into its
//! Video's folder.
//!
//! What the Video adds is the order and what the shots share. A scene's
//! description and negative go into each of its shots' runs (`every`,
//! `negative`), rolled off the scene's own seed so every shot of it rolls the
//! same words. The **lock** is the model, size, fps and Hi-res the rendered
//! shots were made at: those decide whether two shots can be joined, so a
//! render that differs is refused until the person lets it go (the page asks
//! when one of them is changed) — after which the page marks the shots made
//! the old way as outdated (computed, never stored, like a part's done mark).
//!
//! Clips from before Video 2.0 are converted once at start
//! ([`VideoStore::convert_old`]), split at their old cut marks.

use crate::videos::{frames_in, record_lock, Clip, Segment, VideoStore, AUDIO};
use crate::workitems::{valid_id, Shelf, THUMB_HEIGHT};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A Video's record, in its folder.
pub const VIDEO_FILE: &str = "video.json";

/// Shots in one Video. Each is a clip of up to `MAX_SHOTS` parts; this caps
/// the storyboard, not a render.
pub const MAX_VIDEO_SHOTS: usize = 200;

/// `{video}-s{n}` split into the Video's id and the shot's folder name.
/// A Video id is `{ms}-{seed}` ([`crate::workitems::new_id`]), so a third
/// dash-part is unambiguous, and the `s` keeps it apart from an old
/// enlargement's `-x{N}` (`shutdown::is_enlarged_clip`).
pub fn shot_of(id: &str) -> Option<(&str, &str)> {
    let (video, shot) = id.rsplit_once('-')?;
    let n = shot.strip_prefix('s')?;
    (!n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) && video.contains('-')).then_some((video, shot))
}

/// What a join needs every shot to share. Recorded when the first shot is
/// rendered; `None` until then, and again after the person lets it go.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VideoLock {
    pub model: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    #[serde(default)]
    pub hires: bool,
}

impl VideoLock {
    /// What clip `c` was asked to be — the size as typed, not as sd.cpp
    /// rounded it, since that is what the page's boxes hold.
    pub fn of(c: &Clip) -> VideoLock {
        VideoLock {
            model: c.model.clone(),
            width: c.video.width,
            height: c.video.height,
            fps: c.fps,
            hires: c.video.hires,
        }
    }

    /// Why `other` can't join shots made at `self`, in the page's words.
    pub fn differs(&self, other: &VideoLock) -> Option<String> {
        let mut what = Vec::new();
        if self.model != other.model {
            what.push(format!("the model ({})", self.model));
        }
        if (self.width, self.height) != (other.width, other.height) {
            what.push(format!("the size ({}×{})", self.width, self.height));
        }
        if self.fps != other.fps {
            what.push(format!("the frame rate ({} fps)", self.fps));
        }
        if self.hires != other.hires {
            what.push(format!("Hi-res ×2 ({})", if self.hires { "on" } else { "off" }));
        }
        (!what.is_empty()).then(|| what.join(", "))
    }
}

/// A scene: a header, the words its shots share, and its shots in order.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Scene {
    /// The scene's own id inside its Video (`c1`, `c2`, …), so the page can
    /// keep a scene open across a reorder.
    pub id: String,
    #[serde(default)]
    pub title: String,
    /// The scene description: before every part's own words in every shot
    /// of the scene (the old *Every shot*).
    #[serde(default)]
    pub every: String,
    #[serde(default)]
    pub negative: String,
    /// What `every`'s variables roll off, so each shot rolls the same words.
    #[serde(default)]
    pub seed: i64,
    #[serde(default)]
    pub shots: Vec<String>,
}

/// A Video's record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Video {
    pub id: String,
    pub created_ms: u128,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub lock: Option<VideoLock>,
    pub scenes: Vec<Scene>,
    /// Where it lives, not stored: [`VideoStore::load_video`] sets it.
    #[serde(default, skip_deserializing)]
    pub favorite: bool,
    /// Its shots' records, in storyboard order, for the page — filled by
    /// [`VideoStore::load_video_full`], never written.
    #[serde(default, skip_deserializing, skip_serializing_if = "Vec::is_empty")]
    pub clips: Vec<Clip>,
}

impl Video {
    /// Every shot id, in storyboard order.
    pub fn shot_ids(&self) -> impl Iterator<Item = &String> {
        self.scenes.iter().flat_map(|s| s.shots.iter())
    }
}

impl VideoStore {
    fn video_path(&self, id: &str) -> PathBuf {
        self.dir(id).join(VIDEO_FILE)
    }

    fn check_video_id(id: &str) -> Result<()> {
        if !valid_id(id) || shot_of(id).is_some() {
            bail!("'{id}' is not a video id");
        }
        Ok(())
    }

    /// The record alone.
    pub fn load_video(&self, id: &str) -> Result<Video> {
        Self::check_video_id(id)?;
        let path = self.video_path(id);
        let bytes = std::fs::read(&path).with_context(|| format!("that video is gone ({})", path.display()))?;
        let mut v: Video = serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))?;
        v.favorite = path.starts_with(&self.favorites);
        Ok(v)
    }

    /// The record with its shots' clips, as the page lists it. A shot whose
    /// record won't load is left out of `clips`, never out of the order.
    pub fn load_video_full(&self, id: &str) -> Result<Video> {
        let mut v = self.load_video(id)?;
        v.clips = v.shot_ids().filter_map(|s| self.load(s).ok()).collect();
        Ok(v)
    }

    /// `video.json` through a `.part` and a rename. Callers hold the record lock.
    fn write_video(&self, v: &Video) -> Result<()> {
        let mut v = v.clone();
        v.clips.clear();
        let path = self.video_path(&v.id);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let part = path.with_extension("json.part");
        std::fs::write(&part, serde_json::to_vec_pretty(&v)?).with_context(|| format!("writing {}", part.display()))?;
        std::fs::rename(&part, &path).with_context(|| format!("writing {}", path.display()))
    }

    /// A new Video's record. Refused over one that exists.
    pub fn create_video(&self, v: &Video) -> Result<()> {
        Self::check_video_id(&v.id)?;
        let _g = record_lock();
        if self.video_path(&v.id).exists() {
            bail!("video '{}' exists already", v.id);
        }
        self.write_video(v)
    }

    /// Read, change and write a Video's record as one step, so the worker
    /// taking a lock and the page saving the order never undo each other.
    pub fn update_video(&self, id: &str, f: impl FnOnce(&mut Video) -> Result<()>) -> Result<Video> {
        let _g = record_lock();
        let mut v = self.load_video(id)?;
        f(&mut v)?;
        self.write_video(&v)?;
        Ok(v)
    }

    /// The Videos on one shelf, newest first, each with its shots' clips.
    pub fn list_videos(&self, shelf: Shelf) -> Vec<Video> {
        let Ok(entries) = std::fs::read_dir(self.shelf_dir(shelf)) else {
            return Vec::new();
        };
        let mut out: Vec<Video> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().join(VIDEO_FILE).is_file())
            .filter_map(|e| self.load_video_full(e.file_name().to_str()?).ok())
            .collect();
        out.sort_by(|a, b| b.created_ms.cmp(&a.created_ms));
        out
    }

    /// What a grid shows: Current is both shelves, Favorites its own.
    pub fn video_listing(&self, shelf: Shelf) -> Vec<Video> {
        if shelf == Shelf::Favorites {
            return self.list_videos(Shelf::Favorites);
        }
        let mut out = self.list_videos(Shelf::Favorites);
        let starred: std::collections::HashSet<String> = out.iter().map(|v| v.id.clone()).collect();
        out.extend(self.list_videos(Shelf::Work).into_iter().filter(|v| !starred.contains(&v.id)));
        out.sort_by(|a, b| b.created_ms.cmp(&a.created_ms));
        out
    }

    /// A shot id in Video `v` that no shot has, listed or on disk.
    pub fn new_shot_id(&self, v: &Video) -> String {
        let num = |s: &str| shot_of(s).and_then(|(_, n)| n[1..].parse::<u32>().ok());
        let listed = v.shot_ids().filter_map(|s| num(s)).max().unwrap_or(0);
        let on_disk = std::fs::read_dir(self.dir(&v.id))
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter_map(|e| e.file_name().to_str()?.strip_prefix('s')?.parse::<u32>().ok())
                    .max()
                    .unwrap_or(0)
            })
            .unwrap_or(0);
        format!("{}-s{}", v.id, listed.max(on_disk) + 1)
    }

    /// Remove a shot: out of its scene, and its folder. Answers the Video.
    pub fn delete_shot(&self, id: &str) -> Result<Video> {
        let Some((vid, _)) = shot_of(id) else { bail!("'{id}' is not a shot") };
        let v = self.update_video(vid, |v| {
            for s in &mut v.scenes {
                s.shots.retain(|x| x != id);
            }
            Ok(())
        })?;
        let dir = self.dir(id);
        match std::fs::remove_dir_all(&dir) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(e).with_context(|| format!("deleting {}", dir.display()))?
            }
            _ => {}
        }
        Ok(v)
    }

    /// Refuse a render of shot `id` that its Video's other shots can't be
    /// joined to. A shot that isn't in a Video (none are, but the store still
    /// reads plain clips) passes.
    pub fn check_lock(&self, id: &str, want: &VideoLock) -> Result<()> {
        let Some((vid, _)) = shot_of(id) else { return Ok(()) };
        let v = self.load_video(vid)?;
        if let Some(why) = v.lock.as_ref().and_then(|l| l.differs(want)) {
            bail!(
                "this video's shots are made with {why}. To change that, change it in Generation and say yes \
                 to generating them again — the shots made the old way are then marked outdated"
            );
        }
        Ok(())
    }

    /// After a shot renders: its Video takes the lock if it had none.
    pub fn take_lock(&self, id: &str, lock: VideoLock) -> Result<()> {
        let Some((vid, _)) = shot_of(id) else { return Ok(()) };
        self.update_video(vid, |v| {
            v.lock.get_or_insert(lock);
            Ok(())
        })
        .map(|_| ())
    }

    /// A shot drawn again lands under a new id (`fresh`) so a Stop or a
    /// failure leaves the old one whole; on success it takes the old one's
    /// place in its scene and the old folder goes.
    pub fn replace_shot(&self, old: &str, fresh: &str) -> Result<()> {
        let Some((vid, _)) = shot_of(old) else { bail!("'{old}' is not a shot") };
        // First, the ids it stands for, so a film still finds its parts. The
        // fresh record is on disk (`finish` wrote it); an old one that won't
        // read still has its id carried.
        let mut now = self.load(fresh).context("reading the shot drawn again")?;
        now.replaces = self.load(old).map(|w| w.replaces).unwrap_or_default();
        now.replaces.push(old.to_string());
        self.save(&now)?;
        self.update_video(vid, |v| {
            for s in &mut v.scenes {
                for x in &mut s.shots {
                    if x == old {
                        *x = fresh.to_string();
                    }
                }
            }
            Ok(())
        })?;
        let _ = std::fs::remove_dir_all(self.dir(old));
        Ok(())
    }

    /// Old clip folders still not converted — a conversion that failed. The
    /// page says so, since a folder without `video.json` is in no grid.
    pub fn unconverted(&self) -> Vec<String> {
        [Shelf::Work, Shelf::Favorites]
            .iter()
            .filter_map(|&shelf| std::fs::read_dir(self.shelf_dir(shelf)).ok())
            .flat_map(|rd| rd.filter_map(|e| e.ok()))
            .filter(|e| e.path().join("clip.json").is_file() && !e.path().join(VIDEO_FILE).exists())
            .filter_map(|e| e.file_name().to_str().map(str::to_string))
            .collect()
    }

    /// Clips from before Video 2.0, on both shelves, each made a Video of one
    /// scene whose shots are the old clip's runs between cut marks. Built
    /// beside the old folder and swapped in by renames, so a crash at any
    /// point leaves either the old clip (converted again next start) or the
    /// new Video — never neither. The Video keeps the clip's id.
    pub fn convert_old(&self) {
        for shelf in [Shelf::Work, Shelf::Favorites] {
            let root = self.shelf_dir(shelf).to_path_buf();
            recover(&root);
            let Ok(entries) = std::fs::read_dir(&root) else { continue };
            let ids: Vec<String> = entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().join("clip.json").is_file() && !e.path().join(VIDEO_FILE).exists())
                .filter_map(|e| e.file_name().to_str().map(str::to_string))
                .filter(|id| valid_id(id) && shot_of(id).is_none())
                .collect();
            for id in ids {
                match convert_one(&root, &id) {
                    Ok(n) => eprintln!("[yasdig] clip {id} is now a video of {n} shot(s)"),
                    Err(e) => eprintln!("[yasdig] clip {id} was left as it was: {e:#}"),
                }
            }
        }
    }
}

/// Finish or undo a conversion a crash interrupted: a half-built `.convert`
/// goes; an `.old` goes once its Video is in place, else it is put back.
fn recover(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else { return };
    let names: Vec<String> = entries.filter_map(|e| e.ok()?.file_name().to_str().map(str::to_string)).collect();
    for n in names.iter().filter(|n| n.ends_with(".convert")) {
        let _ = std::fs::remove_dir_all(root.join(n));
    }
    for n in names.iter().filter(|n| n.ends_with(".old")) {
        let base = root.join(n.trim_end_matches(".old"));
        if base.join(VIDEO_FILE).is_file() {
            let _ = std::fs::remove_dir_all(root.join(n));
        } else if !base.exists() {
            let _ = std::fs::rename(root.join(n), &base);
        }
    }
}

/// A hard link where the filesystem allows one, else a copy.
fn link_or_copy(from: &Path, to: &Path) -> Result<()> {
    if std::fs::hard_link(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to).with_context(|| format!("copying {} to {}", from.display(), to.display()))?;
    Ok(())
}

/// The runs of `segments` between cut marks, as index ranges.
fn runs(segments: &[Segment]) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut from = 0;
    for (k, s) in segments.iter().enumerate().skip(1) {
        if s.cut {
            out.push(from..k);
            from = k;
        }
    }
    out.push(from..segments.len());
    out
}

/// Convert clip folder `root/id` into a Video in place. Answers its shot count.
fn convert_one(root: &Path, id: &str) -> Result<usize> {
    let dir = root.join(id);
    let mut clip: Clip = serde_json::from_slice(&std::fs::read(dir.join("clip.json"))?).context("reading clip.json")?;
    clip.normalize();
    let draft: Option<serde_json::Value> =
        std::fs::read(dir.join("draft.json")).ok().and_then(|b| serde_json::from_slice(&b).ok());
    let runs = runs(&clip.segments);
    let tmp = root.join(format!("{id}.convert"));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;

    let frames = frames_in(&dir);
    let audio = std::fs::read(dir.join(AUDIO)).ok().and_then(|b| crate::avi::parse_wav(&b).ok());
    let total = frames.len() as u32;
    let starts: Vec<u32> = runs.iter().map(|r| clip.segments.get(r.start).map_or(0, |s| s.start)).collect();
    let draft_rows = draft.as_ref().map(|d| split_rows(d, runs.len()));

    let mut shots = Vec::new();
    for (r, run) in runs.iter().enumerate() {
        let n = r + 1;
        let sid = format!("{id}-s{n}");
        let sdir = tmp.join(format!("s{n}"));
        std::fs::create_dir_all(&sdir)?;
        let mut segs: Vec<Segment> = clip.segments[run.clone()].to_vec();
        let (from, to) = if clip.rendered {
            (starts[r].min(total), starts.get(r + 1).copied().unwrap_or(total).min(total))
        } else {
            (0, 0)
        };
        for s in &mut segs {
            s.start = s.start.saturating_sub(from);
            s.cut = false;
        }
        let key = if r == 0 { clip.source.clone() } else { segs.first_mut().and_then(|s| s.keyframe.take()) };
        if let Some(s) = segs.first_mut() {
            s.keyframe = None;
        }
        for (i, f) in frames[from as usize..to.max(from) as usize].iter().enumerate() {
            link_or_copy(f, &sdir.join(format!("frame_{i:03}.png")))?;
        }
        let mut has_audio = false;
        if let (Some(a), true) = (&audio, clip.rendered) {
            let at = |frame: u32| {
                let samples = frame as u64 * a.sample_rate as u64 / clip.fps.max(1) as u64;
                ((samples * a.channels as u64 * 2) as usize).min(a.pcm.len())
            };
            let end = if r + 1 == runs.len() { a.pcm.len() } else { at(to) };
            let piece = crate::avi::Audio {
                sample_rate: a.sample_rate,
                channels: a.channels,
                pcm: a.pcm[at(from).min(end)..end].to_vec(),
            };
            crate::avi::write_wav(&sdir.join(AUDIO), &piece)?;
            has_audio = true;
        }
        if runs.len() == 1 && dir.join("clip.avi").is_file() {
            link_or_copy(&dir.join("clip.avi"), &sdir.join("clip.avi"))?;
        }
        let (mut tw, mut th) = (0, 0);
        if clip.rendered && to > from {
            let first = image::open(sdir.join("frame_000.png")).context("reading a shot's first frame")?;
            let w = ((first.width() as f32) * (THUMB_HEIGHT as f32) / (first.height().max(1) as f32)).round() as u32;
            let t = first.thumbnail(w.max(1), THUMB_HEIGHT);
            (tw, th) = (t.width(), t.height());
            t.save(sdir.join("thumb.png")).context("writing a shot's thumbnail")?;
        } else if r == 0 && dir.join("thumb.png").is_file() && clip.thumb_width > 0 {
            link_or_copy(&dir.join("thumb.png"), &sdir.join("thumb.png"))?;
            (tw, th) = (clip.thumb_width, clip.thumb_height);
        }
        let first_seg = segs.first();
        let shot = Clip {
            id: sid.clone(),
            prompt: first_seg.map(|s| s.text.trim().to_string()).unwrap_or_default(),
            seed: first_seg.map_or(clip.seed, |s| s.seed),
            typed_prompt: if r == 0 { clip.typed_prompt.clone() } else { None },
            frames: if clip.rendered { to.saturating_sub(from) } else { segs.iter().map(|s| s.frames).sum() },
            thumb_width: tw,
            thumb_height: th,
            source: key,
            audio: has_audio,
            versions: Vec::new(),
            segments: segs,
            draft: None,
            ..clip.clone()
        };
        std::fs::write(sdir.join("clip.json"), serde_json::to_vec_pretty(&shot)?)?;
        if let Some(rows) = draft_rows.as_ref().and_then(|d| d.get(r)).filter(|rows| !rows.is_empty()) {
            std::fs::write(sdir.join("draft.json"), serde_json::to_vec(&serde_json::json!({ "rows": rows }))?)?;
        }
        shots.push(sid);
    }
    let video = Video {
        id: id.to_string(),
        created_ms: clip.created_ms,
        title: String::new(),
        lock: clip.rendered.then(|| VideoLock::of(&clip)),
        scenes: vec![Scene {
            id: "c1".into(),
            title: String::new(),
            every: clip.every.clone().unwrap_or_default(),
            negative: clip.typed_negative.clone().unwrap_or_else(|| clip.negative.clone()),
            seed: clip.seed,
            shots,
        }],
        favorite: false,
        clips: Vec::new(),
    };
    std::fs::write(tmp.join(VIDEO_FILE), serde_json::to_vec_pretty(&video)?)?;
    let old = root.join(format!("{id}.old"));
    std::fs::rename(&dir, &old).context("moving the old clip aside")?;
    if let Err(e) = std::fs::rename(&tmp, &dir) {
        let _ = std::fs::rename(&old, &dir);
        return Err(e).context("putting the video in place");
    }
    let _ = std::fs::remove_dir_all(&old);
    Ok(runs.len())
}

/// A draft's rows (`{rows: [...]}`, each maybe `cut`) split at the cut marks
/// into `n` shots' rows; runs past the last shot join it.
fn split_rows(draft: &serde_json::Value, n: usize) -> Vec<Vec<serde_json::Value>> {
    let mut out: Vec<Vec<serde_json::Value>> = vec![Vec::new()];
    for (k, row) in draft.get("rows").and_then(|r| r.as_array()).into_iter().flatten().enumerate() {
        let cut = row.get("cut").and_then(|c| c.as_bool()).unwrap_or(false);
        if k > 0 && cut && out.len() < n.max(1) {
            out.push(Vec::new());
        }
        let mut row = row.clone();
        if let Some(o) = row.as_object_mut() {
            o.remove("cut");
        }
        out.last_mut().expect("never empty").push(row);
    }
    out
}

// ---- The join ----------------------------------------------------------------

/// A Video as one picture and one soundtrack: its generated shots in
/// storyboard order. What *Download* and *Save to output* write; the page's
/// players read the same shots frame by frame and `joined.wav` for the sound.
pub struct Joined {
    pub fps: u32,
    pub frames: Vec<PathBuf>,
    pub audio: Option<crate::avi::Audio>,
}

/// One shot's part in a join: its name for an error, its frame count, its
/// sound (`None` when it is silent).
pub struct JoinPart {
    pub name: String,
    pub frames: u32,
    pub audio: Option<crate::avi::Audio>,
}

/// The shots' sounds as one track. Each shot gets exactly the samples its
/// frames last, measured from the start of the Video
/// (`frames so far × rate / fps`), so a shot whose WAV runs short or long is
/// padded or cut rather than pushing every later shot out of sync; a silent
/// shot is silence. `None` when no shot has sound.
pub fn join_audio(parts: &[JoinPart], fps: u32) -> Result<Option<crate::avi::Audio>> {
    let Some(first) = parts.iter().find_map(|p| p.audio.as_ref()) else {
        return Ok(None);
    };
    let (rate, channels) = (first.sample_rate, first.channels);
    let block = channels as usize * 2;
    let fps = fps.max(1) as u64;
    let mut pcm = Vec::new();
    let mut done = 0u64;
    for p in parts {
        if let Some(a) = &p.audio {
            if (a.sample_rate, a.channels) != (rate, channels) {
                bail!(
                    "{}'s sound is {} Hz, {} channel(s) and the video's is {rate} Hz, {channels} — \
                     generate {} again to join it",
                    p.name, a.sample_rate, a.channels, p.name
                );
            }
        }
        done += p.frames as u64;
        let want = (done * rate as u64 / fps) as usize * block;
        let at = pcm.len();
        if let Some(a) = &p.audio {
            pcm.extend_from_slice(&a.pcm[..(want - at).min(a.pcm.len()) / block * block]);
        }
        pcm.resize(want, 0);
    }
    Ok(Some(crate::avi::Audio { sample_rate: rate, channels, pcm }))
}

impl VideoStore {
    /// Video `vid` joined: version 0 is the shots as generated, `n` their
    /// enlarged version `n` — which every generated shot must have, or the
    /// sizes would differ. Refused, naming the shots, when they can't be one
    /// picture: another size or frame rate (an outdated shot, made before the
    /// lock was let go) or sound of another rate.
    pub fn joined(&self, vid: &str, version: u32) -> Result<Joined> {
        let v = self.load_video_full(vid)?;
        let names = shot_names(&v);
        let shots: Vec<&Clip> = v.clips.iter().filter(|c| c.rendered && c.frames > 0).collect();
        if shots.is_empty() {
            bail!("nothing in this video is generated yet");
        }
        let name = |c: &Clip| names.iter().find(|(id, _)| *id == c.id).map_or_else(|| c.id.clone(), |(_, n)| n.clone());
        let size = |c: &Clip| match version {
            0 => Some((c.width, c.height)),
            n => c.versions.iter().find(|x| x.n == n).map(|x| (x.width, x.height)),
        };
        let missing: Vec<String> = shots.iter().filter(|c| size(c).is_none()).map(|c| name(c)).collect();
        if !missing.is_empty() {
            bail!(
                "version {version} doesn't cover {} — Enlarge them in it first",
                missing.join(", ")
            );
        }
        // Measured against a shot made at the lock, when one is: an outdated
        // shot first in the order must not make the rest look wrong.
        let anchor = v.lock.as_ref().and_then(|l| shots.iter().find(|c| VideoLock::of(c) == *l)).unwrap_or(&shots[0]);
        let (width, height) = size(anchor).unwrap_or_default();
        let fps = anchor.fps;
        let odd: Vec<String> = shots
            .iter()
            .filter(|c| size(c) != Some((width, height)) || c.fps != fps)
            .map(|c| {
                let (w, h) = size(c).unwrap_or_default();
                format!("{} ({w}×{h}, {} fps)", name(c), c.fps)
            })
            .collect();
        if !odd.is_empty() {
            bail!(
                "these shots can't join a {width}×{height}, {fps} fps video: {} — generate them again",
                odd.join(", ")
            );
        }
        let mut frames = Vec::new();
        let mut parts = Vec::new();
        for c in &shots {
            let mine = self.version_frames(&c.id, version);
            if mine.is_empty() {
                bail!("{} has no frames on disk", name(c));
            }
            let audio = match (c.audio, self.audio_path(&c.id)) {
                (true, Some(p)) => Some(crate::avi::read_wav(&p)?),
                _ => None,
            };
            parts.push(JoinPart { name: name(c), frames: mine.len() as u32, audio });
            frames.extend(mine);
        }
        let audio = join_audio(&parts, fps)?;
        Ok(Joined { fps, frames, audio })
    }
}

/// Each shot's name as the storyboard says it: "Shot 2", or "Scene 1 · Shot
/// 2" once there is more than one scene.
fn shot_names(v: &Video) -> Vec<(String, String)> {
    let many = v.scenes.len() > 1;
    v.scenes
        .iter()
        .enumerate()
        .flat_map(|(si, s)| {
            s.shots.iter().enumerate().map(move |(k, id)| {
                let n = if many { format!("Scene {} · Shot {}", si + 1, k + 1) } else { format!("Shot {}", k + 1) };
                (id.clone(), n)
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shot_id_is_a_video_id_and_a_numbered_s() {
        assert_eq!(shot_of("1758104523000-42-s3"), Some(("1758104523000-42", "s3")));
        assert_eq!(shot_of("1758104523000-42"), None, "a video");
        assert_eq!(shot_of("1758104523000-42-x200"), None, "an old enlargement");
        assert_eq!(shot_of("1758104523000-42-s"), None);
        assert_eq!(shot_of("1758104523000-s2"), None, "no video id before it");
    }

    #[test]
    fn a_lock_says_what_differs() {
        let a = VideoLock { model: "m".into(), width: 832, height: 480, fps: 24, hires: false };
        assert_eq!(a.differs(&a.clone()), None);
        let b = VideoLock { width: 640, fps: 16, ..a.clone() };
        let why = a.differs(&b).unwrap();
        assert!(why.contains("832×480") && why.contains("24 fps"), "{why}");
    }

    #[test]
    fn draft_rows_split_at_their_cut_marks() {
        let d = serde_json::json!({ "rows": [
            { "text": "a" }, { "text": "b" }, { "text": "c", "cut": true }, { "text": "d", "cut": true },
        ]});
        let got = split_rows(&d, 2);
        assert_eq!(got.len(), 2, "a run past the last shot joins it");
        assert_eq!(got[0].len(), 2);
        assert_eq!(got[1].len(), 2);
        assert!(got[1][0].get("cut").is_none(), "no cut marks inside a shot");
    }

    fn png(path: &Path, shade: u8) {
        image::RgbImage::from_pixel(8, 4, image::Rgb([shade, shade, shade])).save(path).unwrap();
    }

    fn seg(text: &str, start: u32, frames: u32, cut: bool, keyframe: Option<&str>) -> Segment {
        Segment {
            text: text.into(),
            conditioned: text.into(),
            seed: 1,
            start,
            frames,
            cut,
            references: Vec::new(),
            keyframe: keyframe.map(str::to_string),
            audio: Vec::new(),
            words: Default::default(),
        }
    }

    /// An old clip of three parts with a cut before the third becomes a
    /// Video of two shots: frames renumbered from 0 in each, the sound split
    /// at the cut, the cut's picture the second shot's first frame, the
    /// draft's rows split the same way, and nothing of the old folder left.
    #[test]
    fn an_old_clip_is_split_at_its_cuts() {
        let dir = std::env::temp_dir().join(format!("yasdig-story-convert-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = VideoStore::new(&dir, &dir.join("user"));
        let id = "500-7";
        let cdir = store.root.join(id);
        std::fs::create_dir_all(&cdir).unwrap();
        for n in 0..10u32 {
            png(&cdir.join(format!("frame_{n:03}.png")), n as u8 * 20);
        }
        // 24 fps, 2400 Hz mono: 100 samples a frame, 200 bytes.
        let pcm: Vec<u8> = (0..10u32).flat_map(|f| std::iter::repeat(f as u8).take(200)).collect();
        crate::avi::write_wav(&cdir.join(AUDIO), &crate::avi::Audio { sample_rate: 2400, channels: 1, pcm }).unwrap();
        let mut c = crate::videos::tests::clip(id, 500);
        c.fps = 24;
        c.every = Some("a harbour at dawn".into());
        c.segments = vec![seg("one", 0, 4, false, None), seg("two", 4, 3, false, None), seg("three", 7, 3, true, Some("200-9"))];
        std::fs::write(cdir.join("clip.json"), serde_json::to_vec(&c).unwrap()).unwrap();
        std::fs::write(
            cdir.join("draft.json"),
            br#"{"rows":[{"text":"one"},{"text":"two"},{"text":"three!","cut":true}],"every":"x","negative":""}"#,
        )
        .unwrap();

        store.convert_old();

        let v = store.load_video_full(id).unwrap();
        assert_eq!(v.scenes.len(), 1);
        assert_eq!(v.scenes[0].shots, vec![format!("{id}-s1"), format!("{id}-s2")]);
        assert_eq!(v.scenes[0].every, "a harbour at dawn");
        assert!(v.lock.is_some(), "a rendered clip's shots are locked to what made them");
        assert_eq!(v.clips.len(), 2);
        let (s1, s2) = (&v.clips[0], &v.clips[1]);
        assert_eq!(store.frames(&s1.id).len(), 7);
        assert_eq!(store.frames(&s2.id).len(), 3);
        assert_eq!((s1.segments.len(), s2.segments.len()), (2, 1));
        assert_eq!(s2.segments[0].start, 0, "rebased");
        assert!(s2.segments.iter().all(|s| !s.cut && s.keyframe.is_none()));
        assert_eq!(s2.source.as_deref(), Some("200-9"), "the cut's picture is the shot's first frame");
        assert_eq!(s1.source, c.source);
        let first = image::open(store.frame_path(&s2.id, 0)).unwrap().to_rgb8();
        assert_eq!(first.get_pixel(0, 0)[0], 140, "shot 2 starts at old frame 7");
        let a1 = crate::avi::read_wav(&store.dir(&s1.id).join(AUDIO)).unwrap();
        let a2 = crate::avi::read_wav(&store.dir(&s2.id).join(AUDIO)).unwrap();
        assert_eq!((a1.pcm.len(), a2.pcm.len()), (1400, 600));
        assert_eq!(a2.pcm[0], 7);
        let d = s2.draft.as_ref().unwrap();
        assert_eq!(d["rows"][0]["text"], "three!");
        let names: Vec<String> =
            std::fs::read_dir(&store.root).unwrap().filter_map(|e| e.ok()?.file_name().into_string().ok()).collect();
        assert_eq!(names, vec![id.to_string()], "no .old or .convert left behind");

        // Run again: nothing to do, nothing changes.
        store.convert_old();
        assert_eq!(store.load_video_full(id).unwrap().clips.len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A crash between the two renames leaves `.old` and `.convert`; the next
    /// start puts the old clip back and converts it again.
    #[test]
    fn a_conversion_cut_short_is_done_again() {
        let dir = std::env::temp_dir().join(format!("yasdig-story-recover-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = VideoStore::new(&dir, &dir.join("user"));
        let id = "600-7";
        let old = store.root.join(format!("{id}.old"));
        std::fs::create_dir_all(&old).unwrap();
        png(&old.join("frame_000.png"), 0);
        let mut c = crate::videos::tests::clip(id, 600);
        c.segments = vec![seg("one", 0, 1, false, None)];
        std::fs::write(old.join("clip.json"), serde_json::to_vec(&c).unwrap()).unwrap();
        std::fs::create_dir_all(store.root.join(format!("{id}.convert"))).unwrap();

        store.convert_old();

        let v = store.load_video_full(id).unwrap();
        assert_eq!(v.clips.len(), 1);
        assert!(!old.exists() && !store.root.join(format!("{id}.convert")).exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A plan (no frames) splits by its rows' cut marks too.
    #[test]
    fn an_old_plan_is_split_too() {
        let dir = std::env::temp_dir().join(format!("yasdig-story-plan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = VideoStore::new(&dir, &dir.join("user"));
        let id = "700-7";
        std::fs::create_dir_all(store.root.join(id)).unwrap();
        let mut c = crate::videos::tests::clip(id, 700);
        c.rendered = false;
        c.segments = vec![seg("one", 0, 33, false, None), seg("two", 0, 33, true, None)];
        std::fs::write(store.root.join(id).join("clip.json"), serde_json::to_vec(&c).unwrap()).unwrap();

        store.convert_old();

        let v = store.load_video_full(id).unwrap();
        assert!(v.lock.is_none(), "nothing rendered, nothing locked");
        assert_eq!(v.clips.len(), 2);
        assert!(v.clips.iter().all(|c| !c.rendered && c.segments.len() == 1));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A Video's shots live in its folder under their own ids; a new id never
    /// reuses one on disk; the lock refuses a render that wouldn't join and
    /// is taken by the first; a deleted shot leaves its scene and the disk.
    #[test]
    fn a_videos_shots_lock_and_go() {
        let dir = std::env::temp_dir().join(format!("yasdig-story-flow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = VideoStore::new(&dir, &dir.join("user"));
        let id = "800-7";
        let v = Video {
            id: id.into(),
            created_ms: 800,
            title: String::new(),
            lock: None,
            scenes: vec![Scene {
                id: "c1".into(),
                title: String::new(),
                every: String::new(),
                negative: String::new(),
                seed: 3,
                shots: vec![format!("{id}-s1")],
            }],
            favorite: false,
            clips: Vec::new(),
        };
        store.create_video(&v).unwrap();
        assert!(store.create_video(&v).is_err(), "never over one that exists");
        assert_eq!(store.dir(&format!("{id}-s1")), store.root.join(id).join("s1"));
        std::fs::create_dir_all(store.root.join(id).join("s4")).unwrap();
        assert_eq!(store.new_shot_id(&v), format!("{id}-s5"), "past every folder on disk");

        let shot = format!("{id}-s1");
        let a = VideoLock { model: "m".into(), width: 832, height: 480, fps: 16, hires: false };
        store.check_lock(&shot, &a).unwrap();
        store.take_lock(&shot, a.clone()).unwrap();
        let b = VideoLock { width: 640, ..a.clone() };
        assert!(store.check_lock(&shot, &b).unwrap_err().to_string().contains("832×480"));
        store.take_lock(&shot, b.clone()).unwrap();
        assert_eq!(store.load_video(id).unwrap().lock, Some(a), "the first lock holds until let go");

        store.update_video(id, |v| {
            v.scenes[0].shots.push(format!("{id}-s4"));
            Ok(())
        })
        .unwrap();
        let mut s4 = crate::videos::tests::clip(&format!("{id}-s4"), 1);
        s4.replaces = vec![format!("{id}-s2")];
        store.save(&s4).unwrap();
        std::fs::create_dir_all(store.root.join(id).join("s5")).unwrap();
        store.save(&crate::videos::tests::clip(&format!("{id}-s5"), 1)).unwrap();
        store.replace_shot(&format!("{id}-s4"), &format!("{id}-s5")).unwrap();
        assert_eq!(store.load_video(id).unwrap().scenes[0].shots, vec![shot.clone(), format!("{id}-s5")]);
        assert_eq!(store.load(&format!("{id}-s5")).unwrap().replaces, [format!("{id}-s2"), format!("{id}-s4")], "what it stands for, oldest first");
        assert!(!store.root.join(id).join("s4").exists(), "the old shot's folder goes");
        let v = store.delete_shot(&format!("{id}-s5")).unwrap();
        assert_eq!(v.scenes[0].shots, vec![shot]);
        assert_eq!(store.list_videos(Shelf::Work).len(), 1);
        store.favorite(id).unwrap();
        assert!(store.load_video(id).unwrap().favorite, "a star moves the Video whole");
        assert!(store.dir(&format!("{id}-s1")).starts_with(&store.favorites));
        std::fs::remove_dir_all(&dir).ok();
    }

    fn mono(rate: u32, samples: &[u8]) -> crate::avi::Audio {
        // One byte value per sample, both bytes of it, so a sample's origin
        // can be read back.
        crate::avi::Audio { sample_rate: rate, channels: 1, pcm: samples.iter().flat_map(|&s| [s, s]).collect() }
    }

    fn part(name: &str, frames: u32, audio: Option<crate::avi::Audio>) -> JoinPart {
        JoinPart { name: name.into(), frames, audio }
    }

    /// Each shot's sound is put at its own frames: a silent shot is silence,
    /// a short WAV is padded and a long one cut, so the shot after starts on
    /// time either way.
    #[test]
    fn a_joined_sound_keeps_every_shot_on_its_frames() {
        // 10 fps at 100 Hz: 10 samples a frame.
        let parts = [
            part("Shot 1", 2, None),
            part("Shot 2", 2, Some(mono(100, &[2; 15]))), // 5 short
            part("Shot 3", 1, Some(mono(100, &[3; 14]))), // 4 long
            part("Shot 4", 1, Some(mono(100, &[4; 10]))),
        ];
        let a = join_audio(&parts, 10).unwrap().unwrap();
        let s: Vec<u8> = a.pcm.chunks(2).map(|c| c[0]).collect();
        assert_eq!(s.len(), 60);
        assert!(s[..20].iter().all(|&x| x == 0), "a silent shot is silence");
        assert!(s[20..35].iter().all(|&x| x == 2) && s[35..40].iter().all(|&x| x == 0), "padded");
        assert!(s[40..50].iter().all(|&x| x == 3), "cut to its frame");
        assert!(s[50..].iter().all(|&x| x == 4), "and the next starts on time");
        assert!(join_audio(&[part("Shot 1", 3, None)], 10).unwrap().is_none(), "no sound, no track");
    }

    /// Sample positions come from the frames so far, not a per-frame
    /// rounding, so a rate that doesn't divide never drifts.
    #[test]
    fn a_joined_sound_does_not_drift_on_an_odd_rate() {
        let parts: Vec<JoinPart> = (0..7).map(|k| part(&format!("Shot {k}"), 1, Some(mono(100, &[k as u8; 20])))).collect();
        let a = join_audio(&parts, 7).unwrap().unwrap();
        assert_eq!(a.pcm.len() / 2, 100, "seven frames at 7 fps are one second");
    }

    #[test]
    fn a_joined_sound_refuses_another_rate() {
        let parts = [part("Shot 1", 1, Some(mono(100, &[1; 10]))), part("Shot 2", 1, Some(mono(200, &[1; 20])))];
        let e = join_audio(&parts, 10).unwrap_err().to_string();
        assert!(e.contains("Shot 2") && e.contains("200 Hz"), "{e}");
    }

    /// A Video of two generated shots and one not: the join is the two, in
    /// storyboard order; a shot at another size is refused by name, and so
    /// is a version a shot doesn't have.
    #[test]
    fn a_video_joins_its_generated_shots_in_order() {
        let dir = std::env::temp_dir().join(format!("yasdig-story-join-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = VideoStore::new(&dir, &dir.join("user"));
        let id = "900-7";
        let shots: Vec<String> = (1..=3).map(|n| format!("{id}-s{n}")).collect();
        let v = Video {
            id: id.into(),
            created_ms: 900,
            title: String::new(),
            lock: None,
            scenes: vec![Scene {
                id: "c1".into(),
                title: String::new(),
                every: String::new(),
                negative: String::new(),
                seed: 3,
                // Storyboard order is s2 first.
                shots: vec![shots[1].clone(), shots[0].clone(), shots[2].clone()],
            }],
            favorite: false,
            clips: Vec::new(),
        };
        store.create_video(&v).unwrap();
        let make = |sid: &str, frames: u32, shade: u8, rendered: bool| {
            let d = store.dir(sid);
            std::fs::create_dir_all(&d).unwrap();
            let mut c = crate::videos::tests::clip(sid, 900);
            (c.width, c.height, c.fps, c.frames, c.rendered) = (8, 4, 10, frames, rendered);
            if rendered {
                for n in 0..frames {
                    png(&d.join(format!("frame_{n:03}.png")), shade);
                }
            }
            std::fs::write(d.join("clip.json"), serde_json::to_vec(&c).unwrap()).unwrap();
        };
        make(&shots[0], 2, 10, true);
        make(&shots[1], 3, 20, true);
        make(&shots[2], 0, 0, false);
        // The second in order has sound; the first is padded with silence.
        crate::avi::write_wav(&store.dir(&shots[0]).join(AUDIO), &mono(100, &[9; 20])).unwrap();
        let mut c = store.load(&shots[0]).unwrap();
        c.audio = true;
        store.save(&c).unwrap();

        let j = store.joined(id, 0).unwrap();
        assert_eq!((j.fps, j.frames.len()), (10, 5));
        assert!(j.frames[0].starts_with(store.dir(&shots[1])), "storyboard order, not shot number");
        let a = j.audio.unwrap();
        assert_eq!(a.pcm.len(), 50 * 2);
        assert!(a.pcm[..60].iter().all(|&b| b == 0) && a.pcm[60..].iter().all(|&b| b == 9));

        let e = store.joined(id, 1).err().unwrap().to_string();
        assert!(e.contains("version 1") && e.contains("Shot 1") && e.contains("Shot 2"), "{e}");

        let mut c = store.load(&shots[1]).unwrap();
        c.width = 16;
        store.save(&c).unwrap();
        let e = store.joined(id, 0).err().unwrap().to_string();
        assert!(e.contains("Shot 2") && e.contains("16×4"), "{e}");
        // With a lock, the shot made another way is the one named, wherever
        // it stands: here Shot 1, made before the lock was let go.
        let mut c = store.load(&shots[1]).unwrap();
        c.video.width = 16;
        store.save(&c).unwrap();
        let lock = VideoLock::of(&store.load(&shots[0]).unwrap());
        store.update_video(id, |v| {
            v.lock = Some(lock.clone());
            Ok(())
        })
        .unwrap();
        let e = store.joined(id, 0).err().unwrap().to_string();
        assert!(e.contains("8×4") && e.contains("Shot 1 (16×4"), "{e}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
