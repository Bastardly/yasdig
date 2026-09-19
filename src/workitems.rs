//! Durable storage for generated images, beside the ephemeral `work/` scratch.
//!
//! ```text
//!   work/workitems/{id}.png         raw, unsharpened, output-resolution
//!   work/workitems/{id}-thumb.png   THUMB_HEIGHT tall, for the grid
//!   work/workitems/{id}.json        the settings that produced it
//!   work/output/{id}-s{step}.png    "Save to output": the filtered image on screen
//!   work/output/{id}-s{step}.json   the same sidecar, with the pass counts filled in
//!   work/output/{id}-s{step}-thumb.png   for the picker; see `ensure_output_thumb`
//! ```
//!
//! The output directory is the finished work, so it is also what the video tab
//! animates from — hence the thumbnails. It is keyed by *filename stem* rather
//! than by work item id, because one generation can produce several saves and
//! they all share an id.
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
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Makes each in-flight thumbnail write its own temp file. See
/// [`Store::ensure_output_thumb`].
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

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
    /// The `work/output/` stem this image was diffused *from*, for an
    /// image-to-image generation. `None` for a txt2img and for an import, which
    /// has no source of its own. Mirrors `Clip.source`, and is what lets the UI
    /// put the source thumbnail back when a work item is reopened.
    #[serde(default)]
    pub source: Option<String>,
    pub profile: Profile,
}

/// A work item id or output stem: ASCII alphanumerics and `-` only, non-empty,
/// bounded length.
///
/// This is the whole path-traversal guard for `GET /workitems/<id>/thumb.png`
/// and `GET /outputs/<stem>/thumb.png`, and it is a whitelist rather than a
/// blacklist on purpose — `.`, `/`, `\` and every percent-decoded form of them
/// are simply not in the alphabet, so no amount of `..` cleverness produces a
/// valid id. Keep it that way: widen the alphabet rather than reaching for
/// `canonicalize`.
///
/// Letters are in it because an output stem is `{id}-s{step}`, which has an `s`
/// in the middle. That is the only reason, and it costs nothing — a letter is no
/// more dangerous than a digit when the two characters that matter are still
/// outside the set.
pub fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_ID_LEN
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
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

        let img =
            image::open(raw).with_context(|| format!("loading {} for archiving", raw.display()))?;
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

    /// Remove a saved image from `work/output/`: the PNG, its sidecar and its
    /// thumbnail. The same shape as [`Store::delete`] and for the same reason —
    /// a leftover sidecar or thumbnail resurrects the entry as a broken tile on
    /// the next refresh.
    ///
    /// Nothing else is touched. A live `Pending` opened from this image holds a
    /// *copy* in the scratch dir (invariant 8), so deleting the file out from
    /// under the viewer leaves the picture on screen working — it just has
    /// nowhere to go back to.
    pub fn delete_output(&self, stem: &str) -> Result<()> {
        if !valid_id(stem) {
            bail!("'{stem}' is not an output name");
        }
        for p in [
            self.output_path(stem),
            self.output_thumb_path(stem),
            self.output_sidecar_path(stem),
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
        let stem = self.free_stem(&format!("{}-s{}", item.id, item.step));
        let png = self.output.join(format!("{stem}.png"));
        std::fs::copy(display, &png).with_context(|| format!("saving to {}", png.display()))?;
        write_sidecar(&self.output.join(format!("{stem}.json")), item)?;
        // Best-effort: a save that produced the PNG the user asked for has done
        // its job, and a missing thumbnail is something `ensure_output_thumb`
        // fixes on the first request for it anyway. Failing the save over the
        // grid's convenience would be the wrong trade.
        if let Err(e) = self.ensure_output_thumb(&stem) {
            eprintln!("[rustimage] thumbnail for {stem} deferred: {e:#}");
        }
        Ok(png)
    }

    /// The first `{base}`, `{base}-2`, `{base}-3`… whose PNG doesn't exist yet.
    ///
    /// Shared by `save_output` and `import` because both mint a name that is
    /// *almost* unique and must never overwrite: step counts restart at 0 when a
    /// work item is reopened, and two imports can land in the same millisecond.
    fn free_stem(&self, base: &str) -> String {
        let mut stem = base.to_string();
        for n in 2.. {
            if !self.output.join(format!("{stem}.png")).exists() {
                break;
            }
            stem = format!("{base}-{n}");
        }
        stem
    }

    /// Take a PNG the user supplied and put it in `work/output/`, returning its
    /// stem.
    ///
    /// `work/output/` rather than a directory of its own, because it is already
    /// *the* pool of images you can start from — it is what the video tab's
    /// first-frame picker reads. Landing an import there means the listing, the
    /// thumbnail route and every existing picker work on it with no new plumbing,
    /// and an imported photo can be animated as readily as diffused.
    ///
    /// The uploaded file's own name is never used: `valid_id` is alphanumerics
    /// and `-`, so almost no real filename is even expressible, and minting the
    /// stem is what keeps the traversal guard a whitelist. `width`/`height` are
    /// the caller's, already read from the decoded image — this writes the
    /// original bytes rather than a re-encode, so nothing is lost on the way in.
    pub fn import(&self, png: &[u8], width: u32, height: u32) -> Result<String> {
        std::fs::create_dir_all(&self.output)
            .with_context(|| format!("creating {}", self.output.display()))?;
        let stem = self.free_stem(&format!("{}-import", new_id(0)));
        let path = self.output.join(format!("{stem}.png"));
        std::fs::write(&path, png).with_context(|| format!("writing {}", path.display()))?;

        let item = WorkItem {
            id: stem.clone(),
            created_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
            prompt: String::new(),
            negative: None,
            seed: -1,
            // Not a registry key, and deliberately not one that could ever be
            // mistaken for one — the parentheses aren't in any model name. The
            // grid shows it verbatim, which is the honest answer to "what made
            // this": nothing here did.
            template: "(imported)".to_string(),
            width,
            height,
            thumb_width: 0,
            thumb_height: 0,
            refine_passes: 0,
            hires_passes: 0,
            step: 0,
            sharpness: 0.0,
            clarity: 0.0,
            source: None,
            profile: Profile::default(),
        };
        write_sidecar(&self.output.join(format!("{stem}.json")), &item)?;
        if let Err(e) = self.ensure_output_thumb(&stem) {
            eprintln!("[rustimage] thumbnail for {stem} deferred: {e:#}");
        }
        Ok(stem)
    }

    /// `work/output/` itself, for the one caller that writes a whole
    /// subdirectory into it rather than a single image.
    pub fn output_dir(&self) -> &Path {
        &self.output
    }

    pub fn output_path(&self, stem: &str) -> PathBuf {
        self.output.join(format!("{stem}.png"))
    }

    pub fn output_thumb_path(&self, stem: &str) -> PathBuf {
        self.output.join(format!("{stem}-thumb.png"))
    }

    fn output_sidecar_path(&self, stem: &str) -> PathBuf {
        self.output.join(format!("{stem}.json"))
    }

    /// The sidecar beside an output PNG, or `None`.
    ///
    /// `None` is a normal answer, not a failure: a PNG copied into
    /// `work/output/` by hand has no sidecar, and `list_outputs` deliberately
    /// lists it anyway — the file on disk is the thing the user saved, and a
    /// listing that hid it would be lying. Callers supply their own defaults.
    pub fn load_output(&self, stem: &str) -> Option<WorkItem> {
        std::fs::read(self.output_sidecar_path(stem))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
    }

    /// Make sure `{stem}-thumb.png` exists, and return it.
    ///
    /// Generate-on-first-request rather than only-on-save, because the whole
    /// point of the picker is the images already in `work/output/` — everything
    /// saved before thumbnails existed has to appear too. After the first request
    /// it's a cached file and this is a stat.
    ///
    /// An output PNG is full resolution, up to 3072², so this is ~100 ms of
    /// decode and resize. That's why it isn't done inline in the listing: the
    /// grid asks for the tiles it is actually showing, in parallel, one
    /// connection thread each.
    pub fn ensure_output_thumb(&self, stem: &str) -> Result<PathBuf> {
        if !valid_id(stem) {
            bail!("'{stem}' is not an output name");
        }
        let thumb = self.output_thumb_path(stem);
        if thumb.is_file() {
            return Ok(thumb);
        }
        let src = self.output_path(stem);
        let img = image::open(&src)
            .with_context(|| format!("loading {} for a thumbnail", src.display()))?;
        let tw = ((img.width() as f32) * (THUMB_HEIGHT as f32) / (img.height().max(1) as f32))
            .round() as u32;
        let small = img.thumbnail(tw.max(1), THUMB_HEIGHT);
        // Written under a unique name and renamed into place, because two
        // requests for the same tile are two connection threads, and interleaved
        // writes to one PNG produce a file that decodes to garbage rather than an
        // error. Rename within a directory is atomic, so the loser of the race
        // simply replaces identical bytes.
        //
        // The counter is what makes the name unique, not the pid: both racing
        // writers are *this* process, so a pid would have given them the same
        // temp file and re-created the problem one step earlier.
        let tmp = self.output.join(format!(
            "{stem}-thumb.{}.tmp",
            TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        // `save` picks the encoder from the extension, and this name has none
        // it would recognise, so the format is named.
        small
            .save_with_format(&tmp, image::ImageFormat::Png)
            .with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &thumb).with_context(|| format!("publishing {}", thumb.display()))?;
        Ok(thumb)
    }

    /// Everything in `work/output/`, newest first.
    ///
    /// Keyed by stem, and the sidecar is read for the rest — an output PNG whose
    /// sidecar is missing is still listed, because the file on disk is the thing
    /// the user saved and a picker that hides it would be lying. Such an entry
    /// just has no prompt to show.
    pub fn list_outputs(&self) -> Vec<Output> {
        let Ok(entries) = std::fs::read_dir(&self.output) else {
            return Vec::new();
        };
        let mut out: Vec<Output> = entries
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let path = e.path();
                if path.extension()?.to_str()? != "png" {
                    return None;
                }
                let stem = path.file_stem()?.to_str()?;
                // `{stem}-thumb.png` is a companion, not an entry — the same
                // trap the work item listing avoids by filtering on `.json`.
                if stem.ends_with("-thumb") || !valid_id(stem) {
                    return None;
                }
                Some(self.output_entry(stem, &path))
            })
            .collect();
        // Newest first. Stems begin with the generation's millisecond timestamp,
        // but several saves share one, so the file's own mtime is what orders
        // them — it is when *this* image was saved.
        out.sort_by(|a, b| b.created_ms.cmp(&a.created_ms).then(b.stem.cmp(&a.stem)));
        out
    }

    fn output_entry(&self, stem: &str, png: &Path) -> Output {
        let item = self.load_output(stem);
        let created_ms = std::fs::metadata(png)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis())
            .unwrap_or(0);
        Output {
            stem: stem.to_string(),
            created_ms,
            width: item.as_ref().map(|i| i.width).unwrap_or(0),
            height: item.as_ref().map(|i| i.height).unwrap_or(0),
            prompt: item.as_ref().map(|i| i.prompt.clone()).unwrap_or_default(),
            seed: item.as_ref().map(|i| i.seed).unwrap_or(0),
            template: item
                .as_ref()
                .map(|i| i.template.clone())
                .unwrap_or_default(),
        }
    }
}

/// One saved image in `work/output/`, as the picker needs it.
///
/// The stem is the identity, not `WorkItem::id`: several saves come out of one
/// generation (`{id}-s{step}`, plus a `-2` suffix when that name is taken), so
/// the id is shared and only the filename is unique.
///
/// `width`/`height` are the image's real size, so the grid can lay out tiles
/// before their thumbnails have loaded. They're 0 when the sidecar is missing,
/// which the page treats as "unknown aspect" rather than as an empty tile.
#[derive(Debug, Clone, Serialize)]
pub struct Output {
    pub stem: String,
    pub created_ms: u128,
    pub width: u32,
    pub height: u32,
    pub prompt: String,
    pub seed: i64,
    pub template: String,
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
        // Output stems, which is why letters are in the alphabet at all.
        assert!(valid_id("1758104523000-12345-s3"));
        assert!(valid_id("1758104523000-12345-s3-2"));

        assert!(!valid_id(""));
        assert!(!valid_id(".."));
        assert!(!valid_id("../../models.toml"));
        assert!(!valid_id("a/b"));
        assert!(!valid_id("12.png"));
        assert!(!valid_id("12 34"));
        assert!(!valid_id(&"1".repeat(MAX_ID_LEN + 1)));
        // Widening to letters must not have let a separator back in. These are
        // the only characters that matter.
        for bad in [
            "..", "a.b", "a/b", "a\\b", "%2e%2e", "a%2fb", "a\0b", "a:b", "~", "a b",
        ] {
            assert!(!valid_id(bad), "{bad:?} must not be a valid id");
        }
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
            prompt: "a lovely fluffy crab".to_string(),
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
            source: None,
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

    /// The picker's half of the store: a listing keyed by stem, and thumbnails
    /// that appear for images saved long before thumbnails existed.
    #[test]
    fn outputs_list_by_stem_and_grow_thumbnails() {
        let dir = std::env::temp_dir().join(format!("rustimage-pick-{}", new_id(2)));
        let store = Store::new(&dir);
        let src = dir.join("src.png");
        image::RgbaImage::from_pixel(300, 100, image::Rgba([9, 9, 9, 255]))
            .save(&src)
            .unwrap();

        let mut it = item("400-7", 400);
        it.width = 300;
        it.height = 100;
        let first = store.save_output(&src, &it).unwrap();
        let second = store.save_output(&src, &it).unwrap();

        // Saving writes the thumbnail now, so the grid is warm immediately.
        assert!(store.output_thumb_path("400-7-s0").is_file());

        // An image saved before thumbnails existed: PNG and sidecar, no thumb.
        std::fs::copy(&src, store.output_path("100-7-s2")).unwrap();
        assert!(!store.output_thumb_path("100-7-s2").is_file());

        let list = store.list_outputs();
        let stems: Vec<&str> = list.iter().map(|o| o.stem.as_str()).collect();
        assert_eq!(stems.len(), 3, "{stems:?}");
        // Companion thumbnails are not entries — the trap this listing exists to
        // avoid, since they sit in the same directory with the same extension.
        assert!(!stems.iter().any(|s| s.ends_with("-thumb")), "{stems:?}");
        assert!(stems.contains(&"400-7-s0") && stems.contains(&"400-7-s0-2"));

        // Both saves came out of one work item, so the stem is the only thing
        // telling them apart.
        assert!(first.file_stem() != second.file_stem());

        let entry = list.iter().find(|o| o.stem == "400-7-s0").unwrap();
        assert_eq!((entry.width, entry.height), (300, 100));
        assert_eq!(entry.prompt, it.prompt);

        // A sidecar-less save is still listed; it just has nothing to say.
        std::fs::copy(&src, store.output_path("999-7-s0")).unwrap();
        let bare = store
            .list_outputs()
            .into_iter()
            .find(|o| o.stem == "999-7-s0")
            .expect("a PNG with no sidecar is still the user's image");
        assert_eq!((bare.width, bare.height), (0, 0));
        assert!(bare.prompt.is_empty());

        // Generate-on-request, then cache.
        let thumb = store.ensure_output_thumb("100-7-s2").unwrap();
        assert!(thumb.is_file());
        let dims = image::open(&thumb).unwrap();
        assert_eq!(dims.height(), THUMB_HEIGHT);
        assert_eq!(dims.width(), THUMB_HEIGHT * 3);
        let again = store.ensure_output_thumb("100-7-s2").unwrap();
        assert_eq!(thumb, again);
        // No temp file left behind by the rename.
        assert!(!store.list_outputs().iter().any(|o| o.stem.contains(".tmp")));

        assert!(store.ensure_output_thumb("../etc").is_err());
        assert!(store.ensure_output_thumb("nope-s0").is_err());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Deleting a saved image has to take all three files with it. A sidecar or
    /// a thumbnail left behind is the same failure the work-item delete guards
    /// against — `list_outputs` keys on the PNG, so a leftover companion is
    /// invisible until `ensure_output_thumb` serves a stale picture for the next
    /// image that mints the same stem.
    #[test]
    fn deleting_an_output_takes_its_companions() {
        let dir = std::env::temp_dir().join(format!("rustimage-del-{}", new_id(6)));
        let store = Store::new(&dir);
        let src = dir.join("src.png");
        image::RgbaImage::from_pixel(64, 32, image::Rgba([4, 5, 6, 255]))
            .save(&src)
            .unwrap();

        let it = item("600-7", 600);
        store.save_output(&src, &it).unwrap();
        assert!(store.output_path("600-7-s0").is_file());
        assert!(store.output_thumb_path("600-7-s0").is_file());

        store.delete_output("600-7-s0").unwrap();
        assert!(!store.output_path("600-7-s0").exists());
        assert!(!store.output_thumb_path("600-7-s0").exists());
        assert!(store.load_output("600-7-s0").is_none());
        assert!(store.list_outputs().is_empty());

        // Deleting what is already gone is how "delete all" behaves when two
        // tabs are open on the same directory, so it is not an error.
        store.delete_output("600-7-s0").unwrap();
        // The traversal guard is the same whitelist as everywhere else.
        assert!(store.delete_output("../models.toml").is_err());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// What the Outputs tab reopens an image *with*. `list_outputs` drops the
    /// profile on the way into an `Output`, so the sidecar has to be readable
    /// whole — `Engine::open_output` restores the settings from it, and a
    /// sidecar that came back empty would silently reopen every saved image as
    /// `Profile::default()`.
    ///
    /// The `None` half matters just as much: a PNG dropped into the directory by
    /// hand is listed and therefore openable, so that path has to be an answer
    /// rather than an error.
    #[test]
    fn an_output_sidecar_comes_back_whole() {
        let dir = std::env::temp_dir().join(format!("rustimage-side-{}", new_id(5)));
        let store = Store::new(&dir);
        let src = dir.join("src.png");
        image::RgbaImage::from_pixel(64, 64, image::Rgba([1, 2, 3, 255]))
            .save(&src)
            .unwrap();

        let mut it = item("500-7", 500);
        it.step = 2;
        it.refine_passes = 1;
        it.sharpness = 0.4;
        it.profile.name = "someturbo".to_string();
        store.save_output(&src, &it).unwrap();

        let back = store
            .load_output("500-7-s2")
            .expect("the sidecar it just wrote");
        assert_eq!(back.id, "500-7");
        assert_eq!(back.step, 2);
        assert_eq!(back.refine_passes, 1);
        assert_eq!(back.sharpness, 0.4);
        assert_eq!(back.profile.name, "someturbo");
        assert_eq!(back.prompt, it.prompt);

        // Dropped in by hand: listed, openable, and with nothing to say.
        std::fs::copy(&src, store.output_path("888-1-s0")).unwrap();
        assert!(store.load_output("888-1-s0").is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// An import is the one image whose name the *user* chose, and the answer is
    /// that it doesn't get to: the stem is minted here, because `valid_id` is the
    /// whole traversal guard for `/outputs/{stem}/thumb.png` and a filename that
    /// went anywhere near it would have to be sanitized rather than generated.
    ///
    /// Two imports in the same millisecond mint the same base, which is why this
    /// goes through the same collision loop as a save. Rare, and entirely
    /// reachable by dropping a folder of images onto the page at once.
    #[test]
    fn an_import_mints_its_own_name_and_never_overwrites() {
        let dir = std::env::temp_dir().join(format!("rustimage-imp-{}", new_id(3)));
        let store = Store::new(&dir);
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(120, 60, image::Rgba([3, 4, 5, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();

        let a = store.import(&png, 120, 60).unwrap();
        let b = store.import(&png, 120, 60).unwrap();
        assert!(valid_id(&a), "{a}");
        assert!(valid_id(&b), "{b}");
        assert_ne!(a, b, "a second import must not land on the first");
        assert!(store.output_path(&a).is_file());
        assert!(store.output_path(&b).is_file());

        // The bytes go in untouched — what feeds a diffusion pass should not
        // already be a re-encode.
        assert_eq!(std::fs::read(store.output_path(&a)).unwrap(), png);

        // It appears in the picker like anything else in work/output/, with its
        // real dimensions and an origin that can't be mistaken for a model name.
        let entry = store
            .list_outputs()
            .into_iter()
            .find(|o| o.stem == a)
            .expect("an import is listed as a source image");
        assert_eq!((entry.width, entry.height), (120, 60));
        assert_eq!(entry.template, "(imported)");
        assert!(!valid_id(&entry.template));

        std::fs::remove_dir_all(&dir).ok();
    }
}
