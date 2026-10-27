//! Durable storage for generated clips.
//!
//! ```text
//!   work/videos/{id}/frame_000.png  … one PNG per frame, as sd-cli wrote them
//!   work/videos/{id}/thumb.png      first frame, THUMB_HEIGHT tall, for the grid
//!   work/videos/{id}/clip.json      the settings that produced it
//! ```
//!
//! A starred clip is the same folder moved to `user/favorite_clips/{id}/`
//! ([`VideoStore::favorite`]). An id names one clip wherever it lives, so
//! every reader goes through [`VideoStore::dir`], which looks in both; only
//! listing and deleting take a [`Shelf`] — the work items' rule, and for its
//! reason: *Delete all* and Close server's clear reach `work/` alone.
//!
//! A clip is a **directory**, which is the one structural difference from
//! `src/workitems.rs` and the reason this is a separate module rather than a
//! wider `WorkItem`: everything about a still is one file and a sidecar, and
//! `WorkItem`'s field list is about stills.
//!
//! It is also the one thing written *straight into* the archive rather than
//! copied in, which looks like invariant 8 being broken and isn't. That
//! invariant exists because a live `Pending` owns its scratch files and deletes
//! them — `Pending::remove_files` on the first refine, `Scratch::drop` on a
//! failed pass — so an archive entry that doubled as scratch would be destroyed
//! by ordinary use. A clip has no scratch stage: it is written once by one pass
//! and afterwards only ever read. Copying it in would mean copying ~23 MB of
//! frames for nothing. What it does need is the *failure* half of `Scratch`,
//! which is [`ClipDir`].

use crate::config::VideoModel;
use crate::workitems::{valid_id, Shelf, THUMB_HEIGHT};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Filename pattern handed to `sd-cli -o`. The `%03d` is what makes sd-cli write
/// a frame per file instead of muxing a container — see [`crate::sd::SdRunner::video`].
/// Three digits covers 999 frames; sd.cpp implements one chunk window, so the
/// practical ceiling is 81.
const FRAME_PATTERN: &str = "frame_%03d.png";

/// What sd-cli writes instead for a model with a soundtrack (MiniMax H3 with
/// its audio VAE). sd-cli writes audio **only** into a container: with a
/// `%03d` pattern it saves the frames and drops the sound without a word. So
/// such a clip arrives as an MJPEG AVI, and [`VideoStore::unpack`] splits it
/// into the frames everything else reads plus `audio.wav`. The cost is that
/// its frames are sd-cli's JPEGs at quality 90 rather than lossless PNGs.
const CONTAINER: &str = "clip.avi";

/// A clip's soundtrack, beside its frames, when it has one.
const AUDIO: &str = "audio.wav";

/// Everything needed to put a clip back on screen with the controls that made
/// it — the `WorkItem` of the video side.
///
/// `video` is the resolved [`VideoModel`] with per-request overrides already
/// applied, so this *is* the record of what produced the clip, the same property
/// `WorkItem::profile` has for a still.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Clip {
    pub id: String,
    pub created_ms: u128,
    pub prompt: String,
    #[serde(default)]
    pub negative: String,
    /// The prompt and negative as they were *typed*, with their `{variables}`
    /// still in them. `None` when there was nothing to roll. The still side's
    /// pair, for the same reason and read the same way — see
    /// [`crate::workitems::WorkItem::typed_prompt`].
    #[serde(default)]
    pub typed_prompt: Option<String>,
    #[serde(default)]
    pub typed_negative: Option<String>,
    pub seed: i64,
    /// `[[video]]` entry name, beside `video.name` so the UI can re-select the
    /// model without reaching into the model.
    pub model: String,
    /// What the frames on disk actually are, not what was asked for — filled in
    /// by [`VideoStore::finish`] from the files themselves. sd.cpp aligns sizes
    /// up and frame counts down, so asking is the only way to be right.
    pub width: u32,
    pub height: u32,
    pub frames: u32,
    pub fps: u32,
    pub thumb_width: u32,
    pub thumb_height: u32,
    /// The `work/output/` stem the first frame came from. `None` for a clip
    /// generated from the prompt alone.
    #[serde(default)]
    pub source: Option<String>,
    /// Whether the clip has a soundtrack (`audio.wav` beside its frames), so
    /// the player knows to fetch one. Set by [`VideoStore::finish`].
    #[serde(default)]
    pub audio: bool,
    pub video: VideoModel,
    /// The clip's enlarged versions, oldest first — the clip dialog's History.
    /// Each is a folder of frames inside the clip's own (`v{n}/`), so deleting
    /// the clip deletes them and nothing lands in `work/output/` unasked.
    #[serde(default)]
    pub versions: Vec<ClipVersion>,
    /// The Shots panel's *Every shot* box as typed: what goes in front of
    /// every shot's own text ([`shot_prompt`]). `None` when it was empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every: Option<String>,
    /// The clip's shots, first to last; a clip made in one go is one. Never
    /// empty once read ([`Clip::normalize`]).
    #[serde(default)]
    pub segments: Vec<Segment>,
    /// Whether the clip is starred — where it lives, not something stored:
    /// [`VideoStore::load`] sets it.
    #[serde(default, skip_deserializing)]
    pub favorite: bool,
    /// The record before `segments`: every shot was conditioned on
    /// `"{prompt}. {shot}"`, which is why a line spoken in the Prompt was
    /// spoken again in every shot. Read, never written.
    #[serde(default, skip_serializing)]
    pub(crate) shots: Vec<String>,
    #[serde(default, skip_serializing)]
    pub(crate) shot_starts: Vec<u32>,
}

/// One shot of a clip: what was typed for it, what sd-cli was conditioned on,
/// its seed and length, how it began, and the frame it begins at in the
/// clip's one frame sequence.
///
/// The page's row for a shot holds `text`, `seed`, `frames`, `cut`,
/// `references` and `keyframe`; a row that still matches all six is a shot
/// that is done.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    /// As typed, `{variables}` and all.
    pub text: String,
    pub conditioned: String,
    pub seed: i64,
    pub start: u32,
    /// What sd-cli rendered, aligned to the family's grid — a shot that goes
    /// on from the last one adds one fewer to the clip (the seam frame).
    /// 0 in a record from before shots had their own length: [`Clip::normalize`]
    /// fills it with the model's, which every shot then used.
    #[serde(default)]
    pub frames: u32,
    /// A shot that starts fresh instead of going on from the last frame — a
    /// cut, which is the only way an H3 shot can take references: sd.cpp
    /// refuses its references beside a start frame.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cut: bool,
    /// `work/output/` stems, as for a still's references.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<String>,
    /// A cut that starts from a picture: a `work/output/` stem, sd-cli's `-i`
    /// for this shot instead of the clip's last frame. How a Wan clip changes
    /// scene, since it has no references.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyframe: Option<String>,
    /// Sounds H3 follows (`--ref-audio`): `work/output/` `.wav` stems, a
    /// voice or a sound for this shot. Where references may be.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audio: Vec<String>,
}

/// What a shot is conditioned on: the *Every shot* text in front of its own.
/// Never another shot's text — an action or a line of dialogue in shot 1 must
/// happen once.
pub fn shot_prompt(every: &str, text: &str) -> String {
    match (every.trim(), text.trim()) {
        ("", t) => t.to_string(),
        (e, "") => e.to_string(),
        (e, t) => format!("{e}. {t}"),
    }
}

impl Clip {
    /// A clip as one shot, for a record that names none: made in one go, or
    /// read from before shots were recorded.
    pub fn single_segment(&self) -> Segment {
        Segment {
            text: self.typed_prompt.clone().unwrap_or_else(|| self.prompt.clone()),
            conditioned: self.prompt.clone(),
            seed: self.seed,
            start: 0,
            frames: self.video.aligned().2,
            cut: false,
            references: Vec::new(),
            keyframe: None,
            audio: Vec::new(),
        }
    }

    /// Bring an older record into the `segments` shape. Its `prompt` was the
    /// scene every shot was conditioned with, which is now what *Every shot*
    /// holds; a one-go clip that was extended had a blank first shot, meaning
    /// "the Prompt alone", and keeps its Prompt as shot 1.
    fn normalize(&mut self) {
        if !self.segments.is_empty() {
            self.shots.clear();
            self.shot_starts.clear();
            let frames = self.video.aligned().2;
            for s in self.segments.iter_mut().filter(|s| s.frames == 0) {
                s.frames = frames;
            }
            return;
        }
        let frames = self.video.aligned().2;
        let shots = std::mem::take(&mut self.shots);
        let starts = std::mem::take(&mut self.shot_starts);
        let start = |k: usize| starts.get(k).copied().unwrap_or(0);
        if shots.is_empty() {
            self.segments = vec![self.single_segment()];
        } else if shots[0].is_empty() {
            let mut segs = vec![self.single_segment()];
            for (k, s) in shots.iter().enumerate().skip(1) {
                segs.push(Segment {
                    text: s.clone(),
                    conditioned: format!("{}. {s}", self.prompt),
                    seed: self.seed,
                    start: start(k),
                    frames,
                    cut: false,
                    references: Vec::new(),
                    keyframe: None,
                    audio: Vec::new(),
                });
            }
            self.segments = segs;
        } else {
            let scene = self.typed_prompt.take().unwrap_or_else(|| self.prompt.clone());
            self.segments = shots
                .iter()
                .enumerate()
                .map(|(k, s)| Segment {
                    text: s.clone(),
                    conditioned: format!("{}. {s}", self.prompt),
                    seed: self.seed.wrapping_add(k as i64),
                    start: start(k),
                    frames,
                    cut: false,
                    references: Vec::new(),
                    keyframe: None,
                    audio: Vec::new(),
                })
                .collect();
            self.prompt = shots[0].clone();
            self.every = Some(scene);
        }
    }
}

/// One enlarged version of a clip: what made it, and its frames' size.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipVersion {
    /// Its folder, `v{n}`; 1 is the first enlarge. The original is version 0.
    pub n: u32,
    pub factor: f32,
    pub sharpness: f32,
    pub clarity: f32,
    pub width: u32,
    pub height: u32,
}

// No `duration()` to match [`crate::config::VideoModel::duration`]: that one
// exists because a *requested* frame count has to be rounded to the family's
// grid first, and the page must not do that arithmetic twice. `Clip::frames` is
// already what is on disk, so its duration is a division the page can do.

/// The `frame_*.png` files directly in `dir`, sorted — the names are
/// zero-padded, so name order is frame order.
fn frames_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("frame_") && n.ends_with(".png"))
        })
        .collect();
    out.sort();
    out
}

/// `work/videos/` and `user/favorite_clips/`. Holds nothing but its paths,
/// like `workitems::Store`, so the GPU worker and each connection thread build
/// their own.
pub struct VideoStore {
    root: PathBuf,
    favorites: PathBuf,
}

impl VideoStore {
    /// Both roots, spelled out, for `Store::new`'s reason: `user/` is a
    /// sibling of `work/` that only `Config` knows.
    pub fn new(work_dir: &Path, user_dir: &Path) -> VideoStore {
        let store = VideoStore {
            root: work_dir.join("videos"),
            favorites: user_dir.join("favorite_clips"),
        };
        std::fs::create_dir_all(&store.root).ok();
        std::fs::create_dir_all(&store.favorites).ok();
        store
    }

    /// The one place a [`Shelf`] becomes a directory.
    fn shelf_dir(&self, shelf: Shelf) -> &Path {
        match shelf {
            Shelf::Work => &self.root,
            Shelf::Favorites => &self.favorites,
        }
    }

    /// Where clip `id` is: starred, or in `work/videos/` — which is also where
    /// a clip not made yet goes, so [`VideoStore::prepare`] needs nothing else.
    pub fn dir(&self, id: &str) -> PathBuf {
        let fav = self.favorites.join(id);
        if fav.is_dir() {
            fav
        } else {
            self.root.join(id)
        }
    }

    pub fn thumb_path(&self, id: &str) -> PathBuf {
        self.dir(id).join("thumb.png")
    }

    fn sidecar_path(&self, id: &str) -> PathBuf {
        self.dir(id).join("clip.json")
    }

    /// Where a frame landed. Matches [`FRAME_PATTERN`] — sd-cli numbers from 0.
    pub fn frame_path(&self, id: &str, n: u32) -> PathBuf {
        self.dir(id).join(format!("frame_{n:03}.png"))
    }

    /// The soundtrack, for a clip that has one.
    pub fn audio_path(&self, id: &str) -> Option<PathBuf> {
        Some(self.dir(id).join(AUDIO)).filter(|p| p.is_file())
    }

    /// Make the directory and hand back the `-o` to generate into — the frame
    /// pattern, or for a clip with sound ([`CONTAINER`]) one AVI — plus the
    /// guard that removes it again if the pass doesn't finish.
    pub fn prepare(&self, id: &str, sound: bool) -> Result<(PathBuf, ClipDir)> {
        if !valid_id(id) {
            bail!("'{id}' is not a clip id");
        }
        let dir = self.dir(id);
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let out = dir.join(if sound { CONTAINER } else { FRAME_PATTERN });
        Ok((out, ClipDir { dir, keep: false }))
    }

    /// Where segment `k` of a long clip renders before it joins the clip — a
    /// folder inside the clip's own, so the clip's guard sweeps it — and the
    /// `-o` to hand sd-cli there.
    pub fn prepare_segment(&self, id: &str, k: usize, sound: bool) -> Result<PathBuf> {
        let dir = self.dir(id).join(format!("seg{k}"));
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        Ok(dir.join(if sound { CONTAINER } else { FRAME_PATTERN }))
    }

    /// Join segment `k` onto the clip: its frames after the clip's, **less its
    /// first** when it goes on from the clip (`seam`), which is the clip's last
    /// frame sd-cli was started from; its sound after the clip's, less that
    /// frame's worth. A cut keeps every frame. Answers the frame the segment's
    /// own begin at. The segment's folder goes.
    pub fn append_segment(&self, id: &str, k: usize, fps: u32, seam: bool) -> Result<u32> {
        let seg = self.dir(id).join(format!("seg{k}"));
        let container = seg.join(CONTAINER);
        let mut audio = None;
        if container.is_file() {
            let avi = crate::avi::read(&container)?;
            for (n, jpeg) in avi.frames.iter().enumerate() {
                image::load_from_memory_with_format(jpeg, image::ImageFormat::Jpeg)
                    .with_context(|| format!("decoding frame {n} of shot {}", k + 1))?
                    .save(seg.join(format!("frame_{n:03}.png")))?;
            }
            audio = avi.audio;
        }
        let start = self.frames(id).len() as u32;
        let skip = usize::from(seam);
        for (i, f) in frames_in(&seg).iter().enumerate().skip(skip) {
            let to = self.frame_path(id, start + (i - skip) as u32);
            std::fs::rename(f, &to).with_context(|| format!("joining shot {} into the clip", k + 1))?;
        }
        if let Some(mut more) = audio {
            let path = self.dir(id).join(AUDIO);
            let mut joined = crate::avi::read_wav(&path)?;
            // One frame of samples, the one dropped above.
            let skip = skip * (more.sample_rate / fps.max(1)) as usize * more.channels as usize * 2;
            more.pcm.drain(..skip.min(more.pcm.len()));
            joined.pcm.extend_from_slice(&more.pcm);
            crate::avi::write_wav(&path, &joined)?;
        }
        std::fs::remove_dir_all(&seg).with_context(|| format!("removing {}", seg.display()))?;
        Ok(start)
    }

    /// Cut a clip back to its first `keep` frames — *Undo* of an added shot —
    /// and its sound to the same length.
    pub fn truncate(&self, id: &str, keep: u32, fps: u32) -> Result<()> {
        for f in self.frames(id).iter().skip(keep as usize) {
            std::fs::remove_file(f).with_context(|| format!("removing {}", f.display()))?;
        }
        let path = self.dir(id).join(AUDIO);
        if path.is_file() {
            let mut a = crate::avi::read_wav(&path)?;
            let bytes = (keep as u64 * a.sample_rate as u64 / fps.max(1) as u64) as usize * a.channels as usize * 2;
            a.pcm.truncate(bytes);
            crate::avi::write_wav(&path, &a)?;
        }
        Ok(())
    }

    /// Everything made *from* a clip's frames, let go when its frames change:
    /// sd-cli's container, the cached download, and the enlarged versions,
    /// which no longer cover the clip. Answers how many versions went.
    pub fn invalidate(&self, clip: &mut Clip) -> usize {
        self.drop_container(&clip.id);
        let _ = std::fs::remove_file(self.dir(&clip.id).join("download.avi"));
        let n = clip.versions.len();
        for v in clip.versions.drain(..) {
            let _ = std::fs::remove_dir_all(self.version_root(&clip.id, v.n));
        }
        n
    }

    /// A long clip's joined frames are the clip; sd-cli's container of its
    /// first shot alone must not stand in for it on *Download*.
    pub fn drop_container(&self, id: &str) {
        let _ = std::fs::remove_file(self.dir(id).join(CONTAINER));
    }

    /// Split the AVI sd-cli wrote for a clip with sound into numbered PNG
    /// frames and `audio.wav`, so playback, *Enlarge* and [`VideoStore::finish`]
    /// see what they see for every other clip. The AVI is kept: it is the
    /// soundtrack's own container, and [`VideoStore::export`] hands it out as
    /// it is rather than re-encoding its JPEGs a second time.
    pub fn unpack(&self, id: &str) -> Result<()> {
        let avi = crate::avi::read(&self.dir(id).join(CONTAINER))?;
        for (n, jpeg) in avi.frames.iter().enumerate() {
            let img = image::load_from_memory_with_format(jpeg, image::ImageFormat::Jpeg)
                .with_context(|| format!("decoding frame {n} of the clip sd-cli wrote"))?;
            img.save(self.frame_path(id, n as u32))
                .with_context(|| format!("writing frame {n}"))?;
        }
        if let Some(audio) = &avi.audio {
            crate::avi::write_wav(&self.dir(id).join(AUDIO), audio)?;
        }
        Ok(())
    }

    /// A video file of the clip for the player's *Download*: sd-cli's own AVI
    /// when it wrote one, or one encoded once into the clip's folder — which
    /// [`VideoStore::delete`] and the sweep take with the rest of it.
    pub fn download_file(&self, clip: &Clip, version: u32) -> Result<PathBuf> {
        let own = self.dir(&clip.id).join(CONTAINER);
        if version == 0 && own.is_file() {
            return Ok(own);
        }
        let path = self.version_root(&clip.id, version).join("download.avi");
        if !path.is_file() {
            self.export(clip, version, &path)?;
        }
        Ok(path)
    }

    /// The folder a version's frames are in: the clip's own for the original
    /// (0), `v{n}/` inside it for an enlarge.
    pub fn version_root(&self, id: &str, version: u32) -> PathBuf {
        match version {
            0 => self.dir(id),
            n => self.dir(id).join(format!("v{n}")),
        }
    }

    /// Frame `k` of a version, by the [`FRAME_PATTERN`] name.
    pub fn version_frame(&self, id: &str, version: u32, k: u32) -> PathBuf {
        self.version_root(id, version).join(format!("frame_{k:03}.png"))
    }

    /// Every frame of a version, in order.
    pub fn version_frames(&self, id: &str, version: u32) -> Vec<PathBuf> {
        frames_in(&self.version_root(id, version))
    }

    /// Rewrite the clip's record — after an enlarge adds a version.
    pub fn save(&self, clip: &Clip) -> Result<()> {
        let json = serde_json::to_vec_pretty(clip).context("serializing clip")?;
        let path = self.sidecar_path(&clip.id);
        std::fs::write(&path, json).with_context(|| format!("writing {}", path.display()))
    }

    /// The clip as one video file at `dest`: sd-cli's own AVI for a clip that
    /// came as one, otherwise its frames (and soundtrack) encoded by
    /// [`crate::avi::write`].
    pub fn export(&self, clip: &Clip, version: u32, dest: &Path) -> Result<()> {
        let own = self.dir(&clip.id).join(CONTAINER);
        if version == 0 && own.is_file() {
            std::fs::copy(&own, dest).with_context(|| format!("writing {}", dest.display()))?;
            return Ok(());
        }
        let frames = self.version_frames(&clip.id, version);
        if frames.is_empty() {
            bail!("clip '{}' has no version {version}", clip.id);
        }
        let audio = match self.audio_path(&clip.id) {
            Some(p) => Some(crate::avi::read_wav(&p)?),
            None => None,
        };
        crate::avi::write(dest, &frames, clip.fps, audio.as_ref())
    }

    /// Every frame of a clip, in order.
    ///
    /// Read off the filesystem rather than counted from `clip.frames`, because
    /// this is also what tells us what `clip.frames` should say. Sorting is by
    /// name, which is why the pattern is zero-padded.
    pub fn frames(&self, id: &str) -> Vec<PathBuf> {
        frames_in(&self.dir(id))
    }

    /// Close out a finished clip: measure what sd-cli produced, write the
    /// thumbnail and the sidecar.
    ///
    /// `clip.width`/`height`/`frames` are *overwritten* here from the files on
    /// disk. That is the video half of "dimensions come back from `publish()`,
    /// they don't carry over" — sd.cpp aligns a size up and a frame count down
    /// and only warns about it, so the request is not a reliable record of the
    /// result.
    pub fn finish(&self, clip: &mut Clip) -> Result<()> {
        let frames = self.frames(&clip.id);
        if frames.is_empty() {
            bail!(
                "sd-cli wrote no frames to {} — the pass reported success but produced nothing",
                self.dir(&clip.id).display()
            );
        }
        let first = image::open(&frames[0])
            .with_context(|| format!("loading {} as the first frame", frames[0].display()))?;
        clip.width = first.width();
        clip.height = first.height();
        clip.frames = frames.len() as u32;
        clip.audio = self.audio_path(&clip.id).is_some();

        // Width from the height, as on the image side: the grid fixes height.
        let tw = ((first.width() as f32) * (THUMB_HEIGHT as f32) / (first.height().max(1) as f32))
            .round() as u32;
        let thumb = first.thumbnail(tw.max(1), THUMB_HEIGHT);
        clip.thumb_width = thumb.width();
        clip.thumb_height = thumb.height();
        thumb
            .save(self.thumb_path(&clip.id))
            .context("writing clip thumbnail")?;

        let json = serde_json::to_vec_pretty(clip).context("serializing clip")?;
        let path = self.sidecar_path(&clip.id);
        std::fs::write(&path, json).with_context(|| format!("writing {}", path.display()))
    }

    pub fn load(&self, id: &str) -> Result<Clip> {
        if !valid_id(id) {
            bail!("'{id}' is not a clip id");
        }
        let path = self.sidecar_path(id);
        let bytes = std::fs::read(&path)
            .with_context(|| format!("that clip is gone ({})", path.display()))?;
        let mut clip: Clip =
            serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))?;
        clip.normalize();
        clip.favorite = path.starts_with(&self.favorites);
        Ok(clip)
    }

    /// The clips on one shelf, newest first. A directory without a readable
    /// `clip.json` is skipped — that's a run that died part way through, and one
    /// of them shouldn't cost the user the whole grid.
    pub fn list(&self, shelf: Shelf) -> Vec<Clip> {
        let Ok(entries) = std::fs::read_dir(self.shelf_dir(shelf)) else {
            return Vec::new();
        };
        let mut out: Vec<Clip> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .filter_map(|e| {
                let id = e.file_name().to_str()?.to_string();
                let clip = self.load(&id).ok()?;
                // The frames are the clip; a sidecar with none is a half-written
                // save and would render as a broken tile.
                (!self.frames(&id).is_empty()).then_some(clip)
            })
            .collect();
        out.sort_by(|a, b| b.created_ms.cmp(&a.created_ms));
        out
    }

    /// What a grid shows: Current is both shelves (a starred clip stays where
    /// it was found, flagged `favorite`), Favorites its own alone.
    pub fn listing(&self, shelf: Shelf) -> Vec<Clip> {
        if shelf == Shelf::Favorites {
            return self.list(Shelf::Favorites);
        }
        let mut out = self.list(Shelf::Favorites);
        let starred: std::collections::HashSet<String> = out.iter().map(|c| c.id.clone()).collect();
        out.extend(self.list(Shelf::Work).into_iter().filter(|c| !starred.contains(&c.id)));
        out.sort_by(|a, b| b.created_ms.cmp(&a.created_ms));
        out
    }

    /// Remove a clip from one shelf, frames and all. Missing is not an error —
    /// the point is that afterwards it's gone. One shelf only: a delete that
    /// reached both would take the favourites with the rest.
    pub fn delete(&self, shelf: Shelf, id: &str) -> Result<()> {
        if !valid_id(id) {
            bail!("'{id}' is not a clip id");
        }
        let dir = self.shelf_dir(shelf).join(id);
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("deleting {}", dir.display())),
        }
    }

    /// Star a clip: move its folder into `user/favorite_clips/`, out of reach
    /// of *Delete all* and Close server's clear. Already starred is success.
    pub fn favorite(&self, id: &str) -> Result<()> {
        self.shelve(id, Shelf::Work, Shelf::Favorites)
    }

    /// Un-star: back into `work/videos/`. Never a delete.
    pub fn unfavorite(&self, id: &str) -> Result<()> {
        self.shelve(id, Shelf::Favorites, Shelf::Work)
    }

    fn shelve(&self, id: &str, from: Shelf, to: Shelf) -> Result<()> {
        if !valid_id(id) {
            bail!("'{id}' is not a clip id");
        }
        let (src, dest) = (self.shelf_dir(from).join(id), self.shelf_dir(to).join(id));
        if !src.is_dir() {
            if dest.is_dir() {
                return Ok(());
            }
            bail!("that clip is gone ({})", src.display());
        }
        if dest.exists() {
            bail!("{} is already there", dest.display());
        }
        // A rename where `work/` and `user/` share a filesystem, which is the
        // ordinary layout; otherwise copied, then the original removed — a
        // crash between the two leaves it on both shelves, listed once.
        if std::fs::rename(&src, &dest).is_ok() {
            return Ok(());
        }
        copy_tree(&src, &dest).with_context(|| format!("copying {} to {}", src.display(), dest.display()))?;
        std::fs::remove_dir_all(&src).with_context(|| format!("removing {}", src.display()))
    }
}

/// `src` into `dest`, folders and all — a clip's versions are folders.
pub(crate) fn copy_tree(src: &Path, dest: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let to = dest.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_tree(&e.path(), &to)?;
        } else {
            std::fs::copy(e.path(), &to)?;
        }
    }
    Ok(())
}

/// Removes a half-written clip directory unless [`ClipDir::keep`] is called.
///
/// The same job `Scratch` does for an image pass, and it matters more here: a
/// clip that dies at frame 20 of 33 leaves twenty PNGs and no sidecar, which
/// `list` would skip forever while the disk kept them. A cancelled run is the
/// common case, not the rare one — that's what the Stop button does.
pub struct ClipDir {
    dir: PathBuf,
    keep: bool,
}

impl ClipDir {
    pub fn keep(&mut self) {
        self.keep = true;
    }

    /// A guard over any folder this module writes: an enlarge's version.
    pub fn at(dir: PathBuf) -> ClipDir {
        ClipDir { dir, keep: false }
    }
}

impl Drop for ClipDir {
    fn drop(&mut self) {
        if !self.keep {
            if let Err(e) = std::fs::remove_dir_all(&self.dir) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    eprintln!("[yasdig] leaving {} behind: {e}", self.dir.display());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{VideoFamily, VideoModel};

    fn model() -> VideoModel {
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
        let cfg: crate::config::Config = toml::from_str(src).unwrap();
        cfg.video.into_iter().next().unwrap()
    }

    fn clip(id: &str, created_ms: u128) -> Clip {
        Clip {
            id: id.to_string(),
            created_ms,
            prompt: "a lovely fluffy crab, walking".to_string(),
            negative: String::new(),
            // Rolled, so the sidecar round-trip below carries the typed half.
            typed_prompt: Some("a lovely fluffy {animal}, walking".to_string()),
            typed_negative: None,
            seed: 7,
            model: "5b".to_string(),
            // Deliberately wrong: `finish` must replace these from the files.
            width: 1,
            height: 1,
            frames: 999,
            fps: 24,
            thumb_width: 0,
            thumb_height: 0,
            source: Some("100-7-s0".to_string()),
            audio: false,
            video: model(),
            versions: Vec::new(),
            every: None,
            segments: Vec::new(),
            favorite: false,
            shots: Vec::new(),
            shot_starts: Vec::new(),
        }
    }

    /// A clip with sound comes from sd-cli as one AVI and leaves as frames,
    /// `audio.wav`, and the AVI itself for its download.
    #[test]
    fn a_clip_with_sound_is_unpacked_into_frames_and_a_soundtrack() {
        let dir = std::env::temp_dir().join(format!("yasdig-vid-snd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = VideoStore::new(&dir, &dir);
        let (out, mut guard) = store.prepare("100-7", true).unwrap();
        assert!(out.ends_with(CONTAINER));
        // What sd-cli would have written: frames and a soundtrack in one AVI.
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        write_frames(&src, 3, 64, 32);
        let frames: Vec<PathBuf> = (0..3).map(|i| src.join(format!("frame_{i:03}.png"))).collect();
        let audio = crate::avi::Audio { sample_rate: 32_000, channels: 2, pcm: vec![1, 2, 3, 4] };
        crate::avi::write(&out, &frames, 24, Some(&audio)).unwrap();

        store.unpack("100-7").unwrap();
        let mut c = clip("100-7", 100);
        store.finish(&mut c).unwrap();
        guard.keep();
        assert_eq!((c.width, c.height, c.frames), (64, 32, 3));
        assert!(c.audio);
        assert_eq!(crate::avi::read_wav(&store.audio_path("100-7").unwrap()).unwrap(), audio);
        // The download is sd-cli's own file, not a second encode of its JPEGs.
        assert_eq!(store.download_file(&c, 0).unwrap(), out);
        std::fs::remove_dir_all(&dir).ok();
    }

    fn write_frames(dir: &Path, n: u32, w: u32, h: u32) {
        for i in 0..n {
            image::RgbaImage::from_pixel(w, h, image::Rgba([i as u8, 2, 3, 255]))
                .save(dir.join(format!("frame_{i:03}.png")))
                .unwrap();
        }
    }

    #[test]
    fn a_clip_is_measured_from_its_frames() {
        let dir = std::env::temp_dir().join(format!("yasdig-vid-{}", std::process::id()));
        let store = VideoStore::new(&dir, &dir);

        let (pattern, mut guard) = store.prepare("100-7", false).unwrap();
        assert!(pattern.ends_with("frame_%03d.png"), "{}", pattern.display());
        write_frames(&store.dir("100-7"), 5, 300, 100);

        let mut c = clip("100-7", 100);
        store.finish(&mut c).unwrap();
        guard.keep();

        // The request said 999 frames at 1x1; the files say otherwise, and the
        // files win.
        assert_eq!((c.width, c.height, c.frames), (300, 100, 5));
        assert_eq!(c.thumb_height, THUMB_HEIGHT);
        assert_eq!(c.thumb_width, THUMB_HEIGHT * 3);
        assert!(store.thumb_path("100-7").is_file());

        let loaded = store.load("100-7").unwrap();
        assert_eq!(loaded.prompt, c.prompt);
        assert_eq!(loaded.typed_prompt, c.typed_prompt);
        assert_eq!(loaded.frames, 5);
        assert_eq!(loaded.source.as_deref(), Some("100-7-s0"));
        assert_eq!(loaded.video.family, VideoFamily::Wan22Ti2v5b);

        // Frames come back in order, which is what the zero padding is for.
        let frames = store.frames("100-7");
        assert_eq!(frames.len(), 5);
        assert_eq!(frames[0], store.frame_path("100-7", 0));
        assert_eq!(frames[4], store.frame_path("100-7", 4));
        // The thumbnail is not a frame.
        assert!(!frames.iter().any(|p| p.ends_with("thumb.png")));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn listing_skips_runs_that_never_finished() {
        let dir = std::env::temp_dir().join(format!("yasdig-vid2-{}", std::process::id()));
        let store = VideoStore::new(&dir, &dir);

        for (id, ms) in [("100-7", 100u128), ("200-7", 200)] {
            let (_, mut guard) = store.prepare(id, false).unwrap();
            write_frames(&store.dir(id), 2, 8, 8);
            let mut c = clip(id, ms);
            store.finish(&mut c).unwrap();
            guard.keep();
        }
        // Frames but no sidecar: a pass that died at frame 1.
        let (_, mut orphan) = store.prepare("300-7", false).unwrap();
        write_frames(&store.dir("300-7"), 1, 8, 8);
        orphan.keep();
        // A sidecar but no frames: the other half of the same accident.
        let (_, mut empty) = store.prepare("400-7", false).unwrap();
        let mut c = clip("400-7", 400);
        c.frames = 0;
        std::fs::write(
            store.dir("400-7").join("clip.json"),
            serde_json::to_vec(&c).unwrap(),
        )
        .unwrap();
        empty.keep();

        let ids: Vec<String> = store.list(Shelf::Work).into_iter().map(|c| c.id).collect();
        assert_eq!(ids, ["200-7", "100-7"], "newest first, finished only");

        store.delete(Shelf::Work, "100-7").unwrap();
        assert!(!store.dir("100-7").exists());
        assert_eq!(store.list(Shelf::Work).len(), 1);
        // Deleting what isn't there is a no-op.
        store.delete(Shelf::Work, "100-7").unwrap();
        assert!(store.load("100-7").is_err());
        assert!(store.delete(Shelf::Work, "../work").is_err());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The guard is what keeps a cancelled clip from leaving 20 PNGs and no
    /// sidecar on a disk that is already nearly full. Stop makes this the
    /// common path, not a rare one.
    #[test]
    fn an_unfinished_clip_directory_removes_itself() {
        let dir = std::env::temp_dir().join(format!("yasdig-vid3-{}", std::process::id()));
        let store = VideoStore::new(&dir, &dir);
        {
            let (_, _guard) = store.prepare("100-7", false).unwrap();
            write_frames(&store.dir("100-7"), 3, 8, 8);
            assert_eq!(store.frames("100-7").len(), 3);
        }
        assert!(
            !store.dir("100-7").exists(),
            "the guard should have swept it"
        );
        assert!(store.list(Shelf::Work).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `finish` must not write a sidecar for a directory with nothing in it —
    /// that is the state `list` skips, so a clip that reported success and
    /// produced no frames has to be an error the user sees.
    #[test]
    fn finishing_an_empty_clip_is_an_error() {
        let dir = std::env::temp_dir().join(format!("yasdig-vid4-{}", std::process::id()));
        let store = VideoStore::new(&dir, &dir);
        let (_, _guard) = store.prepare("100-7", false).unwrap();
        let err = store
            .finish(&mut clip("100-7", 100))
            .unwrap_err()
            .to_string();
        assert!(err.contains("no frames"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A long clip's next shot starts from the clip's last frame, which sd-cli
    /// draws again as the shot's first: joined, that one is dropped and the
    /// rest follow on, numbered after the clip's.
    #[test]
    fn a_shot_joins_the_clip_without_repeating_its_seam() {
        let dir = std::env::temp_dir().join(format!("yasdig-vid-shots-{}", std::process::id()));
        let store = VideoStore::new(&dir, &dir);
        let (_, mut guard) = store.prepare("100-7", false).unwrap();
        let shade = |p: &Path, v: u8| image::GrayImage::from_pixel(4, 4, image::Luma([v])).save(p).unwrap();
        for i in 0..3u8 {
            shade(&store.frame_path("100-7", i as u32), i);
        }
        let seg = store.prepare_segment("100-7", 1, false).unwrap();
        for i in 0..3u8 {
            shade(&seg.with_file_name(format!("frame_{i:03}.png")), 100 + i);
        }
        assert_eq!(store.append_segment("100-7", 1, 24, true).unwrap(), 3);
        let frames = store.frames("100-7");
        assert_eq!(frames.len(), 5, "3 + 3 less the repeated seam");
        let px = |p: &Path| image::open(p).unwrap().to_luma8().get_pixel(0, 0)[0];
        assert_eq!(frames.iter().map(|f| px(f)).collect::<Vec<_>>(), [0, 1, 2, 101, 102]);
        assert!(!store.dir("100-7").join("seg1").exists());

        // Undo of that shot: back to the clip's own three frames.
        store.truncate("100-7", 3, 24).unwrap();
        assert_eq!(store.frames("100-7").len(), 3);
        assert!(store.append_segment("100-7", 1, 24, true).is_err(), "the segment is gone");
        let seg = store.prepare_segment("100-7", 1, false).unwrap();
        for i in 0..3u8 {
            shade(&seg.with_file_name(format!("frame_{i:03}.png")), 100 + i);
        }
        store.append_segment("100-7", 1, 24, true).unwrap();
        assert_eq!(store.frames("100-7").len(), 5);

        // A cut starts fresh, so it has no seam to drop: all three join.
        store.truncate("100-7", 3, 24).unwrap();
        let seg = store.prepare_segment("100-7", 1, false).unwrap();
        for i in 0..3u8 {
            shade(&seg.with_file_name(format!("frame_{i:03}.png")), 100 + i);
        }
        store.append_segment("100-7", 1, 24, false).unwrap();
        let frames = store.frames("100-7");
        assert_eq!(frames.iter().map(|f| px(f)).collect::<Vec<_>>(), [0, 1, 2, 100, 101, 102]);
        guard.keep();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The extend bug: a line spoken in the Prompt was spoken again in every
    /// shot, because every shot was `"{prompt}. {shot}"`. Now a shot is its own
    /// text, with only the *Every shot* box in front.
    #[test]
    fn a_shot_is_conditioned_on_its_own_text_and_every_shot() {
        assert_eq!(shot_prompt("", "she says hello"), "she says hello");
        assert_eq!(shot_prompt("a red-haired woman, film grain", "she waves"), "a red-haired woman, film grain. she waves");
        assert_eq!(shot_prompt("  a beach ", ""), "a beach");
    }

    /// Records from before `segments` read as shots: a long clip's old Prompt
    /// was the scene every shot carried, so it becomes *Every shot*; an
    /// extended one-go clip keeps its Prompt as shot 1.
    #[test]
    fn an_older_record_reads_as_segments() {
        let mut one = clip("100-7", 1);
        one.normalize();
        assert_eq!(one.segments.len(), 1);
        assert_eq!(one.segments[0].text, "a lovely fluffy {animal}, walking");
        assert_eq!(one.segments[0].conditioned, "a lovely fluffy crab, walking");
        assert!(one.every.is_none());

        let mut long = clip("100-7", 1);
        long.typed_prompt = None;
        long.prompt = "a beach at dusk".into();
        long.shots = vec!["a crab walks".into(), "it lifts a claw".into()];
        long.shot_starts = vec![0, 33];
        long.normalize();
        assert_eq!(long.every.as_deref(), Some("a beach at dusk"));
        assert_eq!(long.prompt, "a crab walks");
        let texts: Vec<_> = long.segments.iter().map(|s| (s.text.as_str(), s.start, s.seed)).collect();
        assert_eq!(texts, [("a crab walks", 0, 7), ("it lifts a claw", 33, 8)]);
        assert_eq!(long.segments[1].conditioned, "a beach at dusk. it lifts a claw");
        // Every old shot ran the model's own length.
        let frames = long.video.aligned().2;
        assert!(long.segments.iter().all(|s| s.frames == frames && !s.cut), "{:?}", long.segments);

        let mut grown = clip("100-7", 1);
        grown.shots = vec![String::new(), "it turns".into()];
        grown.shot_starts = vec![0, 33];
        grown.normalize();
        assert!(grown.every.is_none());
        assert_eq!(grown.segments[0].text, "a lovely fluffy {animal}, walking");
        assert_eq!((grown.segments[1].text.as_str(), grown.segments[1].start), ("it turns", 33));

        // And the old fields are never written back.
        let json = serde_json::to_string(&grown).unwrap();
        assert!(!json.contains("shot_starts"), "{json}");
    }

    /// A star moves the clip to `user/favorite_clips/`: it is still read by
    /// id, still listed on Current, and out of reach of a delete there.
    #[test]
    fn a_favourite_clip_survives_deleting_current() {
        let dir = std::env::temp_dir().join(format!("yasdig-vid-fav-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = VideoStore::new(&dir.join("work"), &dir.join("user"));
        for (id, ms) in [("100-7", 100u128), ("200-7", 200)] {
            let (_, mut guard) = store.prepare(id, false).unwrap();
            write_frames(&store.dir(id), 2, 8, 8);
            store.finish(&mut clip(id, ms)).unwrap();
            guard.keep();
        }
        store.favorite("100-7").unwrap();
        store.favorite("100-7").unwrap();
        assert!(store.dir("100-7").starts_with(dir.join("user")));
        assert!(store.load("100-7").unwrap().favorite);
        assert_eq!(store.frames("100-7").len(), 2);

        let current: Vec<_> = store.listing(Shelf::Work).into_iter().map(|c| (c.id, c.favorite)).collect();
        assert_eq!(current, [("200-7".to_string(), false), ("100-7".to_string(), true)]);
        assert_eq!(store.listing(Shelf::Favorites).len(), 1);
        // What *Delete all* and Close server's clear go through.
        assert_eq!(store.list(Shelf::Work).len(), 1);

        store.delete(Shelf::Work, "100-7").unwrap();
        assert!(store.load("100-7").is_ok(), "a delete on Current never reaches a favourite");

        store.unfavorite("100-7").unwrap();
        assert!(!store.load("100-7").unwrap().favorite);
        assert_eq!(store.list(Shelf::Favorites).len(), 0);
        std::fs::remove_dir_all(&dir).ok();
    }
}
