//! Durable storage for generated images, beside the ephemeral `work/` scratch.
//!
//! ```text
//!   work/workitems/{id}.png         raw, unsharpened, output-resolution
//!   work/workitems/{id}-thumb.png   THUMB_HEIGHT tall, for the grid
//!   work/workitems/{id}.json        the settings that produced it
//!   work/output/{id}-s{step}.png    "Save to output": the filtered image on screen
//!   work/output/{id}-s{step}.json   the same sidecar, with the pass counts filled in
//! ```
//!
//! **Nothing here is ever referenced by a live `Pending`.** A work item is copied
//! *in* when it is generated and copied *out* when it is opened — see the
//! invariant in `CLAUDE.md`. `Pending::remove_files` runs on the first refine of
//! an image and `Scratch::drop` runs on any failed pass, so a `Pending` pointing
//! into this directory would delete the archived original as a side effect of
//! ordinary use. The cost is one extra PNG write per generate (~15 ms at 1536²,
//! against a diffusion measured in seconds), and in exchange the whole existing
//! scratch lifecycle stays exactly as it was.
//!
//! This is also why `purge_pngs` and `MAX_PENDING` in `src/pipeline.rs` are
//! unchanged: both only reach `work/` itself — `purge_pngs` uses a non-recursive
//! `read_dir` and filters on a `.png` extension, so it skips directories — and
//! both are still what keeps the scratch dir bounded. Eviction is merely harmless
//! now, because an evicted image can be reopened from here.

use crate::config::Profile;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Stored thumbnail height. Twice the 200 px the grid displays, so the strip is
/// sharp on a HiDPI screen; ~25 KB each, against ~3 MB for the image itself.
pub const THUMB_HEIGHT: u32 = 400;

/// Longest id we'll look at. Ids are `{unix_millis}-{seed}`, so ~25 characters;
/// the cap is only here to bound what a bad request can make us allocate.
const MAX_ID_LEN: usize = 64;

/// Everything needed to put an image back on screen with the controls that made
/// it. `profile` is the clone `Engine::run` retained — per-request overrides
/// already applied — so reopening an item reproduces its settings exactly,
/// including an `img2img_cfg_scale` that was left unset and therefore follows
/// `cfg_scale` rather than being pinned to the number it happened to resolve to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkItem {
    pub id: String,
    pub created_ms: u128,
    pub prompt: String,
    pub negative: Option<String>,
    pub seed: i64,
    /// Registry key the image was generated from. Kept beside `profile.name` so
    /// the UI can re-select the model without reaching into the profile.
    pub template: String,
    pub width: u32,
    pub height: u32,
    pub thumb_width: u32,
    pub thumb_height: u32,
    /// Escalations this image has been through. Always 0 for a work item (only
    /// generations are archived); meaningful in an `output/` sidecar.
    #[serde(default)]
    pub refine_passes: u32,
    #[serde(default)]
    pub hires_passes: u32,
    #[serde(default)]
    pub step: u32,
    /// Filters baked into an `output/` PNG. Not meaningful for a work item, whose
    /// stored image is deliberately unsharpened.
    #[serde(default)]
    pub sharpness: f32,
    #[serde(default)]
    pub clarity: f32,
    pub profile: Profile,
}

/// A work item id: ASCII digits and `-` only, non-empty, bounded length.
///
/// This is the whole path-traversal guard for `GET /workitems/<id>/thumb.png`,
/// and it is a whitelist rather than a blacklist on purpose — `.`, `/`, `\` and
/// every percent-decoded form of them are simply not in the alphabet, so no
/// amount of `..` cleverness produces a valid id. Keep it that way; if ids ever
/// need letters, widen this rather than reaching for `canonicalize`.
pub fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_ID_LEN
        && s.bytes().all(|b| b.is_ascii_digit() || b == b'-')
}

/// Mint an id for a new work item. Millisecond-resolution timestamps sort
/// lexicographically (they're fixed-width until the year 2286) and a generation
/// takes seconds, so a collision would need two jobs in the same millisecond on a
/// serialized worker. The seed rides along to make the directory browsable.
pub fn new_id(seed: i64) -> String {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{ms}-{}", seed.unsigned_abs())
}

/// The two persistent directories under `work/`. Holds no state beyond its
/// paths, so the GPU worker and the HTTP connection threads each build their own
/// rather than sharing one behind a lock — the worker only ever writes, and a
/// listing that races a write just misses the newest item until the next refresh.
pub struct Store {
    items: PathBuf,
    output: PathBuf,
}

impl Store {
    pub fn new(work_dir: &Path) -> Store {
        let store = Store {
            items: work_dir.join("workitems"),
            output: work_dir.join("output"),
        };
        std::fs::create_dir_all(&store.items).ok();
        std::fs::create_dir_all(&store.output).ok();
        store
    }

    pub fn image_path(&self, id: &str) -> PathBuf {
        self.items.join(format!("{id}.png"))
    }

    pub fn thumb_path(&self, id: &str) -> PathBuf {
        self.items.join(format!("{id}-thumb.png"))
    }

    fn sidecar_path(&self, id: &str) -> PathBuf {
        self.items.join(format!("{id}.json"))
    }

    /// Archive a generation: the unsharpened image, a thumbnail, and the
    /// settings. `item.thumb_*` are filled in here, since only this function
    /// knows what the thumbnailer produced.
    ///
    /// `raw` is *copied*, not moved — the live `Pending` still owns it.
    pub fn save(&self, raw: &Path, item: &mut WorkItem) -> Result<()> {
        if !valid_id(&item.id) {
            bail!("refusing to save work item with invalid id '{}'", item.id);
        }
        std::fs::create_dir_all(&self.items)
            .with_context(|| format!("creating {}", self.items.display()))?;

        let img = image::open(raw)
            .with_context(|| format!("loading {} for archiving", raw.display()))?;
        let (w, h) = (img.width(), img.height());
        // Width from the height, not the other way round: the grid fixes height
        // and lets width fall where it may, so that's the axis to be exact on.
        let tw = ((w as f32) * (THUMB_HEIGHT as f32) / (h.max(1) as f32)).round() as u32;
        let thumb = img.thumbnail(tw.max(1), THUMB_HEIGHT);
        item.thumb_width = thumb.width();
        item.thumb_height = thumb.height();

        std::fs::copy(raw, self.image_path(&item.id))
            .with_context(|| format!("archiving {}", raw.display()))?;
        thumb
            .save(self.thumb_path(&item.id))
            .context("writing work item thumbnail")?;
        write_sidecar(&self.sidecar_path(&item.id), item)
    }

    pub fn load(&self, id: &str) -> Result<WorkItem> {
        if !valid_id(id) {
            bail!("'{id}' is not a work item id");
        }
        let path = self.sidecar_path(id);
        let bytes = std::fs::read(&path)
            .with_context(|| format!("that work item is gone ({})", path.display()))?;
        let mut item: WorkItem = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing {}", path.display()))?;
        // The stored flag says whether the checkpoint was there when the image
        // was made, which is not a question about now. Re-ask it, so a profile
        // whose model has since been deleted reports honestly.
        item.profile.exists = item.profile.model.is_file();
        Ok(item)
    }

    /// Every archived item, newest first. Unreadable or unparseable sidecars are
    /// skipped rather than failing the listing: one bad file shouldn't cost the
    /// user the whole grid.
    pub fn list(&self) -> Vec<WorkItem> {
        let Ok(entries) = std::fs::read_dir(&self.items) else {
            return Vec::new();
        };
        let mut out: Vec<WorkItem> = entries
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let path = e.path();
                if path.extension()?.to_str()? != "json" {
                    return None;
                }
                let item: WorkItem = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
                // The image is the item; a sidecar without one is a half-written
                // save and would render as a broken thumbnail.
                self.image_path(&item.id).is_file().then_some(item)
            })
            .collect();
        out.sort_by(|a, b| b.created_ms.cmp(&a.created_ms));
        out
    }

    /// Remove an item and both of its companions. Missing files are not an
    /// error — the point is that afterwards it's gone.
    pub fn delete(&self, id: &str) -> Result<()> {
        if !valid_id(id) {
            bail!("'{id}' is not a work item id");
        }
        for p in [
            self.image_path(id),
            self.thumb_path(id),
            self.sidecar_path(id),
        ] {
            match std::fs::remove_file(&p) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e).with_context(|| format!("deleting {}", p.display())),
            }
        }
        Ok(())
    }

    /// Keep the image currently on screen. Saves `display` rather than the
    /// unsharpened `raw`: this is the "I want this one" button, and what the user
    /// means by *this one* includes wherever they left the two sliders.
    ///
    /// Named by origin and step, so repeated refines of the same image land
    /// beside each other in the order they were made — and suffixed if that name
    /// is taken. Step counts restart at 0 every time a work item is reopened, so
    /// two separate sessions of refining the same original both reach `-s1`;
    /// without the suffix the second save would silently overwrite the first,
    /// which is the one thing this button must never do.
    pub fn save_output(&self, display: &Path, item: &WorkItem) -> Result<PathBuf> {
        std::fs::create_dir_all(&self.output)
            .with_context(|| format!("creating {}", self.output.display()))?;
        let base = format!("{}-s{}", item.id, item.step);
        let mut stem = base.clone();
        for n in 2.. {
            if !self.output.join(format!("{stem}.png")).exists() {
                break;
            }
            stem = format!("{base}-{n}");
        }
        let png = self.output.join(format!("{stem}.png"));
        std::fs::copy(display, &png)
            .with_context(|| format!("saving to {}", png.display()))?;
        write_sidecar(&self.output.join(format!("{stem}.json")), item)?;
        Ok(png)
    }
}

fn write_sidecar(path: &Path, item: &WorkItem) -> Result<()> {
    let json = serde_json::to_vec_pretty(item).context("serializing work item")?;
    std::fs::write(path, json).with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `valid_id` is the only thing standing between a URL path segment and a
    /// `std::fs::read`, so it gets a test even though the rest of this module
    /// needs a filesystem to exercise.
    #[test]
    fn valid_id_is_a_whitelist() {
        assert!(valid_id("1758104523000-12345"));
        assert!(valid_id("0"));

        assert!(!valid_id(""));
        assert!(!valid_id(".."));
        assert!(!valid_id("../../models.toml"));
        assert!(!valid_id("a/b"));
        assert!(!valid_id("12.png"));
        assert!(!valid_id("12 34"));
        assert!(!valid_id("abc"));
        assert!(!valid_id(&"1".repeat(MAX_ID_LEN + 1)));
    }

    #[test]
    fn new_id_is_valid_and_carries_the_seed() {
        let id = new_id(12345);
        assert!(valid_id(&id), "{id}");
        assert!(id.ends_with("-12345"), "{id}");
        // A negative seed would otherwise produce a second '-' — still in the
        // alphabet, but it would make the id ambiguous to read.
        assert!(valid_id(&new_id(-7)));
    }

    fn item(id: &str, created_ms: u128) -> WorkItem {
        WorkItem {
            id: id.to_string(),
            created_ms,
            prompt: "a lovely robot crab".to_string(),
            negative: None,
            seed: 7,
            template: "test".to_string(),
            width: 320,
            height: 160,
            thumb_width: 0,
            thumb_height: 0,
            refine_passes: 0,
            hires_passes: 0,
            step: 0,
            sharpness: 0.0,
            clarity: 0.0,
            profile: Profile::default(),
        }
    }

    /// The whole lifecycle on a real filesystem. No GPU and no sd.cpp: this is a
    /// PNG round-trip and a directory listing, which is exactly the part that
    /// fails silently — a thumbnail with the wrong aspect, a listing that also
    /// picks up `-thumb.png`, or a delete that leaves the sidecar behind and so
    /// resurrects the item as a broken tile on the next refresh.
    #[test]
    fn save_list_reopen_delete() {
        let dir = std::env::temp_dir().join(format!("rustimage-wi-{}", new_id(0)));
        let store = Store::new(&dir);

        // A 320x160 source, so a wrong-way-round thumbnail is obvious.
        let src = dir.join("src.png");
        image::RgbaImage::from_pixel(320, 160, image::Rgba([10, 20, 30, 255]))
            .save(&src)
            .unwrap();

        let mut a = item("100-7", 100);
        let mut b = item("200-7", 200);
        store.save(&src, &mut a).unwrap();
        store.save(&src, &mut b).unwrap();

        // Height is pinned, width follows the aspect ratio.
        assert_eq!(a.thumb_height, THUMB_HEIGHT);
        assert_eq!(a.thumb_width, THUMB_HEIGHT * 2);

        // Newest first, and the two companion files are not mistaken for items.
        let list = store.list();
        assert_eq!(
            list.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
            ["200-7", "100-7"]
        );

        let loaded = store.load("100-7").unwrap();
        assert_eq!(loaded.prompt, a.prompt);
        assert_eq!(loaded.width, 320);

        // Reopening must not consume the archive — this is the invariant.
        assert!(store.image_path("100-7").is_file());

        store.delete("100-7").unwrap();
        for p in [
            store.image_path("100-7"),
            store.thumb_path("100-7"),
            store.sidecar_path("100-7"),
        ] {
            assert!(!p.exists(), "{} survived the delete", p.display());
        }
        assert_eq!(store.list().len(), 1);
        // Deleting what isn't there is a no-op, not an error.
        store.delete("100-7").unwrap();
        assert!(store.load("100-7").is_err());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Saving the same origin and step twice must not overwrite the first — see
    /// `save_output`. Reopening a work item restarts `step` at 0, so this is an
    /// ordinary sequence, not a corner case.
    #[test]
    fn save_output_never_overwrites() {
        let dir = std::env::temp_dir().join(format!("rustimage-out-{}", new_id(1)));
        let store = Store::new(&dir);
        let src = dir.join("src.png");
        image::RgbaImage::from_pixel(8, 8, image::Rgba([1, 2, 3, 255]))
            .save(&src)
            .unwrap();

        let it = item("300-7", 300);
        let first = store.save_output(&src, &it).unwrap();
        let second = store.save_output(&src, &it).unwrap();
        let third = store.save_output(&src, &it).unwrap();

        assert_ne!(first, second);
        assert_ne!(second, third);
        assert!(first.ends_with("300-7-s0.png"), "{}", first.display());
        assert!(second.ends_with("300-7-s0-2.png"), "{}", second.display());
        assert!(third.ends_with("300-7-s0-3.png"), "{}", third.display());
        for p in [&first, &second, &third] {
            assert!(p.is_file());
            assert!(p.with_extension("json").is_file());
        }

        std::fs::remove_dir_all(&dir).ok();
    }
}
