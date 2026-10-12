//! Durable storage for generated clips.
//!
//! ```text
//!   work/videos/{id}/frame_000.png  … one PNG per frame, as sd-cli wrote them
//!   work/videos/{id}/thumb.png      first frame, THUMB_HEIGHT tall, for the grid
//!   work/videos/{id}/clip.json      the settings that produced it
//! ```
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
use crate::workitems::{valid_id, THUMB_HEIGHT};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Filename pattern handed to `sd-cli -o`. The `%03d` is what makes sd-cli write
/// a frame per file instead of muxing a container — see [`crate::sd::SdRunner::video`].
/// Three digits covers 999 frames; sd.cpp implements one chunk window, so the
/// practical ceiling is 81.
const FRAME_PATTERN: &str = "frame_%03d.png";

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
    pub video: VideoModel,
}

// No `duration()` to match [`crate::config::VideoModel::duration`]: that one
// exists because a *requested* frame count has to be rounded to the family's
// grid first, and the page must not do that arithmetic twice. `Clip::frames` is
// already what is on disk, so its duration is a division the page can do.

/// `work/videos/`. Holds nothing but its path, like `workitems::Store`, so the
/// GPU worker and each connection thread build their own.
pub struct VideoStore {
    root: PathBuf,
}

impl VideoStore {
    pub fn new(work_dir: &Path) -> VideoStore {
        let store = VideoStore {
            root: work_dir.join("videos"),
        };
        std::fs::create_dir_all(&store.root).ok();
        store
    }

    pub fn dir(&self, id: &str) -> PathBuf {
        self.root.join(id)
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

    /// Make the directory and hand back the `-o` pattern to generate into, plus
    /// the guard that removes it again if the pass doesn't finish.
    pub fn prepare(&self, id: &str) -> Result<(PathBuf, ClipDir)> {
        if !valid_id(id) {
            bail!("'{id}' is not a clip id");
        }
        let dir = self.dir(id);
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        Ok((dir.join(FRAME_PATTERN), ClipDir { dir, keep: false }))
    }

    /// Every frame of a clip, in order.
    ///
    /// Read off the filesystem rather than counted from `clip.frames`, because
    /// this is also what tells us what `clip.frames` should say. Sorting is by
    /// name, which is why the pattern is zero-padded.
    pub fn frames(&self, id: &str) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(self.dir(id)) else {
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
        serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))
    }

    /// Every archived clip, newest first. A directory without a readable
    /// `clip.json` is skipped — that's a run that died part way through, and one
    /// of them shouldn't cost the user the whole grid.
    pub fn list(&self) -> Vec<Clip> {
        let Ok(entries) = std::fs::read_dir(&self.root) else {
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

    /// Remove a clip, frames and all. Missing is not an error — the point is
    /// that afterwards it's gone.
    pub fn delete(&self, id: &str) -> Result<()> {
        if !valid_id(id) {
            bail!("'{id}' is not a clip id");
        }
        let dir = self.dir(id);
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("deleting {}", dir.display())),
        }
    }
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
            video: model(),
        }
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
        let store = VideoStore::new(&dir);

        let (pattern, mut guard) = store.prepare("100-7").unwrap();
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
        let store = VideoStore::new(&dir);

        for (id, ms) in [("100-7", 100u128), ("200-7", 200)] {
            let (_, mut guard) = store.prepare(id).unwrap();
            write_frames(&store.dir(id), 2, 8, 8);
            let mut c = clip(id, ms);
            store.finish(&mut c).unwrap();
            guard.keep();
        }
        // Frames but no sidecar: a pass that died at frame 1.
        let (_, mut orphan) = store.prepare("300-7").unwrap();
        write_frames(&store.dir("300-7"), 1, 8, 8);
        orphan.keep();
        // A sidecar but no frames: the other half of the same accident.
        let (_, mut empty) = store.prepare("400-7").unwrap();
        let mut c = clip("400-7", 400);
        c.frames = 0;
        std::fs::write(
            store.dir("400-7").join("clip.json"),
            serde_json::to_vec(&c).unwrap(),
        )
        .unwrap();
        empty.keep();

        let ids: Vec<String> = store.list().into_iter().map(|c| c.id).collect();
        assert_eq!(ids, ["200-7", "100-7"], "newest first, finished only");

        store.delete("100-7").unwrap();
        assert!(!store.dir("100-7").exists());
        assert_eq!(store.list().len(), 1);
        // Deleting what isn't there is a no-op.
        store.delete("100-7").unwrap();
        assert!(store.load("100-7").is_err());
        assert!(store.delete("../work").is_err());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The guard is what keeps a cancelled clip from leaving 20 PNGs and no
    /// sidecar on a disk that is already nearly full. Stop makes this the
    /// common path, not a rare one.
    #[test]
    fn an_unfinished_clip_directory_removes_itself() {
        let dir = std::env::temp_dir().join(format!("yasdig-vid3-{}", std::process::id()));
        let store = VideoStore::new(&dir);
        {
            let (_, _guard) = store.prepare("100-7").unwrap();
            write_frames(&store.dir("100-7"), 3, 8, 8);
            assert_eq!(store.frames("100-7").len(), 3);
        }
        assert!(
            !store.dir("100-7").exists(),
            "the guard should have swept it"
        );
        assert!(store.list().is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `finish` must not write a sidecar for a directory with nothing in it —
    /// that is the state `list` skips, so a clip that reported success and
    /// produced no frames has to be an error the user sees.
    #[test]
    fn finishing_an_empty_clip_is_an_error() {
        let dir = std::env::temp_dir().join(format!("yasdig-vid4-{}", std::process::id()));
        let store = VideoStore::new(&dir);
        let (_, _guard) = store.prepare("100-7").unwrap();
        let err = store
            .finish(&mut clip("100-7", 100))
            .unwrap_err()
            .to_string();
        assert!(err.contains("no frames"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
