//! Durable storage for generated images, beside the ephemeral `work/` scratch.
//!
//! ```text
//!   work/workitems/{id}.png         raw, unsharpened, output-resolution
//!   work/workitems/{id}-thumb.png   THUMB_HEIGHT tall, for the grid
//!   work/workitems/{id}.json        the settings that produced it
//!   user/favorite/{id}.png          the same three files, kept on purpose
//!   user/favorite/{id}-thumb.png
//!   user/favorite/{id}.json
//!   work/output/{id}-s{step}.png    "Save to output": the filtered image on screen
//!   work/output/{id}-s{step}.json   the same sidecar, with the pass counts filled in
//!   work/output/{id}-s{step}-thumb.png   for the picker; see `ensure_output_thumb`
//!   user/favorite_outputs/{stem}.png     the same three files, kept on purpose
//! ```
//!
//! The first two are the same shelf twice over — same trio, same ids, read by
//! the same code through [`Shelf`] — and they differ only in who is allowed to
//! sweep them. See that type.
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
//! It is also what let the scratch cache shrink to one image per place it is
//! shown (`pipeline::Slot`) and move to `work/current/`: an image let go of
//! there can be reopened from here, so nothing more needs to stay warm.

use crate::config::Profile;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
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

/// Which shelf of archived generations a call is about.
///
/// The two hold the same three files under the same ids and are read by the
/// same code; what separates them is **who is allowed to delete them**.
/// `Work` is `work/workitems/`, under the directory this program sweeps —
/// `purge_pngs` at startup, `Delete all` from the page — and a grid of every
/// image ever made is used that way, which is what makes keeping one an
/// explicit act. `Favorites` is `user/favorite/`, beside `user/settings.toml`
/// and under the same rule: nothing that took a decision to create may live
/// under a sweeper.
///
/// A favourite is **moved** here (see [`Store::favorite`]), and the grid's
/// Current listing reads both shelves ([`Store::listing`]), so a starred item
/// stays in the grid while `Delete all` — which goes through `list(Work)` —
/// neither counts nor reaches it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shelf {
    Work,
    Favorites,
}

/// Which gallery of saved images a call is about: [`Shelf`]'s argument again,
/// one directory along. `Output` is `work/output/`, which `Delete all` and the
/// clear-on-close settings empty; `Favorites` is `user/favorite_outputs/`, which
/// nothing touches unasked. A favourite is moved there (see
/// [`Store::favorite_output`]) and still listed on Current
/// ([`Store::output_listing`]).
///
/// Only listing and deleting take one. **Reading by stem doesn't**: a stem names
/// one picture wherever it lives — [`Store::free_stem`] never mints a name either
/// gallery holds — so [`Store::find_output`] can look in both, and every *Start
/// from* slot, the viewer and the thumbnail route work on a favourite unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gallery {
    Output,
    Favorites,
}

/// Everything needed to put an image back on screen with the controls that made
/// it. `profile` is the clone `Engine::run` retained — per-request overrides
/// already applied — so reopening an item reproduces its settings exactly,
/// including a `refine_cfg_scale` that was left unset and therefore follows
/// `cfg_scale` rather than being pinned to the number it happened to resolve to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkItem {
    pub id: String,
    pub created_ms: u128,
    pub prompt: String,
    pub negative: Option<String>,
    /// The prompt as it was *typed*, with its `{variables}` still in it —
    /// `None` when there was nothing to roll, which is also what a sidecar
    /// written before variables existed says. Reopening restores this one where
    /// it exists (`crate::variables::typed`), and the stored `seed` is what
    /// makes that lossless: same seed, same roll, same picture.
    ///
    /// `typed_` rather than the three obvious names, all of which are taken and
    /// mean something else here: `template` is the checkpoint, `raw` is the
    /// unfiltered image, and `source` is the output stem an image was diffused
    /// *from*.
    #[serde(default)]
    pub typed_prompt: Option<String>,
    #[serde(default)]
    pub typed_negative: Option<String>,
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
    favorites: PathBuf,
    output: PathBuf,
    favorite_outputs: PathBuf,
}

impl Store {
    /// Both roots, spelled out. `user/` is a *sibling* of `work/`, not a child,
    /// so deriving one from the other would be a guess about the layout that
    /// only `Config` actually knows.
    pub fn new(work_dir: &Path, user_dir: &Path) -> Store {
        let store = Store {
            items: work_dir.join("workitems"),
            favorites: user_dir.join("favorite"),
            output: work_dir.join("output"),
            favorite_outputs: user_dir.join("favorite_outputs"),
        };
        std::fs::create_dir_all(&store.items).ok();
        std::fs::create_dir_all(&store.favorites).ok();
        std::fs::create_dir_all(&store.output).ok();
        std::fs::create_dir_all(&store.favorite_outputs).ok();
        store
    }

    /// The one place a [`Shelf`] becomes a directory. Every path below goes
    /// through it, so there is no second copy of that decision to disagree.
    fn shelf_dir(&self, shelf: Shelf) -> &Path {
        match shelf {
            Shelf::Work => &self.items,
            Shelf::Favorites => &self.favorites,
        }
    }

    /// [`Store::shelf_dir`] for a [`Gallery`], and the same promise.
    fn gallery_dir(&self, gallery: Gallery) -> &Path {
        match gallery {
            Gallery::Output => &self.output,
            Gallery::Favorites => &self.favorite_outputs,
        }
    }

    pub fn image_path(&self, shelf: Shelf, id: &str) -> PathBuf {
        self.shelf_dir(shelf).join(format!("{id}.png"))
    }

    pub fn thumb_path(&self, shelf: Shelf, id: &str) -> PathBuf {
        self.shelf_dir(shelf).join(format!("{id}-thumb.png"))
    }

    fn sidecar_path(&self, shelf: Shelf, id: &str) -> PathBuf {
        self.shelf_dir(shelf).join(format!("{id}.json"))
    }

    /// Archive a generation: the unsharpened image, a thumbnail, and the
    /// settings. `item.thumb_*` are filled in here, since only this function
    /// knows what the thumbnailer produced.
    ///
    /// `raw` is *copied*, not moved — the live `Pending` still owns it.
    ///
    /// Always [`Shelf::Work`]: a generation is archived, a favourite is chosen.
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

        std::fs::copy(raw, self.image_path(Shelf::Work, &item.id))
            .with_context(|| format!("archiving {}", raw.display()))?;
        thumb
            .save(self.thumb_path(Shelf::Work, &item.id))
            .context("writing work item thumbnail")?;
        write_sidecar(&self.sidecar_path(Shelf::Work, &item.id), item)
    }

    pub fn load(&self, shelf: Shelf, id: &str) -> Result<WorkItem> {
        if !valid_id(id) {
            bail!("'{id}' is not a work item id");
        }
        let path = self.sidecar_path(shelf, id);
        let bytes = std::fs::read(&path)
            .with_context(|| format!("that work item is gone ({})", path.display()))?;
        let mut item: WorkItem = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing {}", path.display()))?;
        // The stored flag says whether the checkpoint was there when the image
        // was made, which is not a question about now. Re-ask it, so a profile
        // whose model has since been deleted reports honestly.
        item.profile.exists = item.profile.missing().is_empty();
        Ok(item)
    }

    /// Every archived item, newest first. Unreadable or unparseable sidecars are
    /// skipped rather than failing the listing: one bad file shouldn't cost the
    /// user the whole grid.
    pub fn list(&self, shelf: Shelf) -> Vec<WorkItem> {
        let Ok(entries) = std::fs::read_dir(self.shelf_dir(shelf)) else {
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
                self.image_path(shelf, &item.id).is_file().then_some(item)
            })
            .collect();
        out.sort_by(|a, b| b.created_ms.cmp(&a.created_ms));
        out
    }

    /// [`Store::list`] as the grid wants it. Current is *both* shelves: a
    /// favourite lives in `user/favorite/` and is still shown among the work
    /// items, starred. Favorites is that shelf alone. An id on both shelves (a
    /// favourite kept by a build that copied rather than moved) is listed
    /// once, as the favourite.
    pub fn listing(&self, shelf: Shelf) -> Vec<Listed> {
        let favorites = self.list(Shelf::Favorites);
        let kept: HashSet<String> = favorites.iter().map(|i| i.id.clone()).collect();
        let mut out: Vec<Listed> = favorites
            .into_iter()
            .map(|item| Listed { item, favorite: true })
            .collect();
        if shelf == Shelf::Work {
            out.extend(
                self.list(Shelf::Work)
                    .into_iter()
                    .filter(|i| !kept.contains(&i.id))
                    .map(|item| Listed { item, favorite: false }),
            );
            out.sort_by(|a, b| b.item.created_ms.cmp(&a.item.created_ms));
        }
        out
    }

    /// Remove an item and both of its companions. Missing files are not an
    /// error — the point is that afterwards it's gone.
    /// One shelf only. `Delete all` on the grid is this in a loop, and the
    /// favourites shelf has to be out of its reach or the feature does nothing.
    pub fn delete(&self, shelf: Shelf, id: &str) -> Result<()> {
        if !valid_id(id) {
            bail!("'{id}' is not a work item id");
        }
        for p in [
            self.image_path(shelf, id),
            self.thumb_path(shelf, id),
            self.sidecar_path(shelf, id),
        ] {
            match std::fs::remove_file(&p) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e).with_context(|| format!("deleting {}", p.display())),
            }
        }
        Ok(())
    }

    /// Star a work item: **move** its three files onto the favourites shelf.
    ///
    /// A move, so `Delete all` — which empties `work/workitems/` — can't reach
    /// it, and so its count is only what it would delete. The Current grid
    /// still shows it ([`Store::listing`] reads both shelves), so starring
    /// never hides an image from the grid it was found in.
    ///
    /// Copy first, then remove the original: a crash between the two leaves the
    /// item on both shelves, which the listing shows once. Already starred is
    /// success. A missing image or sidecar is an error — there is nothing to
    /// keep. A missing *thumbnail* is not: an entry old enough to lack one is
    /// still worth keeping.
    pub fn favorite(&self, id: &str) -> Result<()> {
        if !valid_id(id) {
            bail!("'{id}' is not a work item id");
        }
        let (work, fav) = (self.shelf_dir(Shelf::Work), self.shelf_dir(Shelf::Favorites));
        if !self.image_path(Shelf::Work, id).is_file()
            && self.image_path(Shelf::Favorites, id).is_file()
        {
            return Ok(());
        }
        copy_picture(work, fav, id, true)?;
        self.delete(Shelf::Work, id)
    }

    /// Un-star: move a favourite back to `work/workitems/`, where `Delete all`
    /// can reach it again. Never a delete — that is `/favorites/delete`. An
    /// item already on both shelves just loses its favourite copy.
    pub fn unfavorite(&self, id: &str) -> Result<()> {
        if !valid_id(id) {
            bail!("'{id}' is not a work item id");
        }
        if !self.image_path(Shelf::Favorites, id).is_file() {
            return Ok(());
        }
        if !self.image_path(Shelf::Work, id).is_file() {
            copy_picture(self.shelf_dir(Shelf::Favorites), self.shelf_dir(Shelf::Work), id, true)?;
        }
        self.delete(Shelf::Favorites, id)
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
    ///
    /// One gallery only, for the reason [`Store::delete`] is one shelf: a
    /// delete that reached both would take the favourites with the rest.
    pub fn delete_output(&self, gallery: Gallery, stem: &str) -> Result<()> {
        if !valid_id(stem) {
            bail!("'{stem}' is not an output name");
        }
        let dir = self.gallery_dir(gallery);
        for p in [
            dir.join(format!("{stem}.png")),
            dir.join(format!("{stem}-thumb.png")),
            dir.join(format!("{stem}.json")),
        ] {
            match std::fs::remove_file(&p) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e).with_context(|| format!("deleting {}", p.display())),
            }
        }
        Ok(())
    }

    /// Star a saved image: move its files into `user/favorite_outputs/`.
    /// [`Store::favorite`] one directory along, and for its reasons. A missing
    /// sidecar is fine here, unlike a work item's: a PNG dropped into
    /// `work/output/` by hand is listed, so it can be kept. The stem doesn't
    /// change — stems are unique across both galleries (`free_stem`).
    pub fn favorite_output(&self, stem: &str) -> Result<()> {
        if !valid_id(stem) {
            bail!("'{stem}' is not an output name");
        }
        let (out, fav) = (self.gallery_dir(Gallery::Output), self.gallery_dir(Gallery::Favorites));
        if !self.output_path(stem).is_file() && fav.join(format!("{stem}.png")).is_file() {
            return Ok(());
        }
        copy_picture(out, fav, stem, false)?;
        self.delete_output(Gallery::Output, stem)
    }

    /// Un-star a saved image: back into `work/output/`. [`Store::unfavorite`]
    /// for the Outputs tab.
    pub fn unfavorite_output(&self, stem: &str) -> Result<()> {
        if !valid_id(stem) {
            bail!("'{stem}' is not an output name");
        }
        let (out, fav) = (self.gallery_dir(Gallery::Output), self.gallery_dir(Gallery::Favorites));
        if !fav.join(format!("{stem}.png")).is_file() {
            return Ok(());
        }
        if !self.output_path(stem).is_file() {
            copy_picture(fav, out, stem, false)?;
        }
        self.delete_output(Gallery::Favorites, stem)
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
    ///
    /// With `embed`, the PNG carries the settings that made it
    /// ([`crate::pngmeta`]) — `Settings::embed_png_settings`.
    pub fn save_output(&self, display: &Path, item: &WorkItem, embed: bool) -> Result<PathBuf> {
        std::fs::create_dir_all(&self.output)
            .with_context(|| format!("creating {}", self.output.display()))?;
        let stem = self.free_stem(&format!("{}-s{}", item.id, item.step));
        let png = self.output.join(format!("{stem}.png"));
        let bytes = output_bytes(display, embed.then_some(item))?;
        std::fs::write(&png, bytes).with_context(|| format!("saving to {}", png.display()))?;
        write_sidecar(&self.output.join(format!("{stem}.json")), item)?;
        // Best-effort: a save that produced the PNG the user asked for has done
        // its job, and a missing thumbnail is something `ensure_output_thumb`
        // fixes on the first request for it anyway. Failing the save over the
        // grid's convenience would be the wrong trade.
        if let Err(e) = self.ensure_output_thumb(&stem) {
            eprintln!("[yasdig] thumbnail for {stem} deferred: {e:#}");
        }
        Ok(png)
    }

    /// The first `{base}`, `{base}-2`, `{base}-3`… whose PNG exists in neither
    /// gallery.
    ///
    /// Both, not just `work/output/`: that is what lets a stem name one picture
    /// wherever it lives (see [`Gallery`]). Checking only the gallery being
    /// written would let a refine saved after its favourited original was
    /// deleted take the original's name, and a *Start from* naming that stem
    /// would then mean two different pictures.
    ///
    /// Shared by `save_output` and `import` because both mint a name that is
    /// *almost* unique and must never overwrite: step counts restart at 0 when a
    /// work item is reopened, and two imports can land in the same millisecond.
    fn free_stem(&self, base: &str) -> String {
        let mut stem = base.to_string();
        for n in 2.. {
            if self.find_output(&stem).is_none() {
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
        // `create_new`, retried: uploads answer on connection threads, so two
        // can pick the same free stem at once, and a plain write would let the
        // second replace the first under one name.
        let base = format!("{}-import", new_id(0));
        let (stem, path, mut file) = loop {
            let stem = self.free_stem(&base);
            let path = self.output.join(format!("{stem}.png"));
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(f) => break (stem, path, f),
                // Another upload took it between `free_stem` and here; the
                // next `free_stem` sees its file and moves on.
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e).with_context(|| format!("creating {}", path.display())),
            }
        };
        std::io::Write::write_all(&mut file, png).with_context(|| format!("writing {}", path.display()))?;

        let item = WorkItem {
            id: stem.clone(),
            created_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
            prompt: String::new(),
            negative: None,
            // An import has no prompt at all, so it has no typed one either.
            typed_prompt: None,
            typed_negative: None,
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
            eprintln!("[yasdig] thumbnail for {stem} deferred: {e:#}");
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

    #[cfg(test)]
    fn output_thumb_path(&self, stem: &str) -> PathBuf {
        self.output.join(format!("{stem}-thumb.png"))
    }

    /// The saved image a stem names, from whichever gallery holds it —
    /// `work/output/` first, though by [`Store::free_stem`] a stem in both is
    /// the same picture twice. Every reader by stem goes through this, which is
    /// what makes a favourite work as a *Start from*, in the viewer and in the
    /// picker with no routes of its own. `None` for a stem that isn't valid.
    pub fn find_output(&self, stem: &str) -> Option<PathBuf> {
        if !valid_id(stem) {
            return None;
        }
        [&self.output, &self.favorite_outputs]
            .into_iter()
            .map(|d| d.join(format!("{stem}.png")))
            .find(|p| p.is_file())
    }

    /// The sidecar beside an output PNG, or `None`.
    ///
    /// `None` is a normal answer, not a failure: a PNG copied into
    /// `work/output/` by hand has no sidecar, and `list_outputs` deliberately
    /// lists it anyway — the file on disk is the thing the user saved, and a
    /// listing that hid it would be lying. Callers supply their own defaults.
    pub fn load_output(&self, stem: &str) -> Option<WorkItem> {
        std::fs::read(self.find_output(stem)?.with_extension("json"))
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
        // Beside whichever copy was found, so a favourite whose original has
        // been deleted still gets — and keeps — a tile.
        let Some(src) = self.find_output(stem) else {
            bail!("no saved image '{stem}'");
        };
        let dir = src.parent().unwrap_or(&self.output);
        let thumb = dir.join(format!("{stem}-thumb.png"));
        if thumb.is_file() {
            return Ok(thumb);
        }
        write_output_thumb(&src, stem)
    }

    /// Write the edited pixels in `display` over the saved image `stem`, in
    /// every gallery that holds it — the viewer's *Save changes*.
    ///
    /// Safe where it looks dangerous, because of what the viewer holds: a
    /// scratch *copy* of the file taken when it was opened (invariant 8), which
    /// the sliders re-filter. So saving twice never stacks the grade, and Reset
    /// then Save puts the original pixels back while the viewer stays on it.
    ///
    /// Both galleries, because a stem names one picture wherever it lives
    /// ([`Store::free_stem`]): updating only `work/output/` would leave the
    /// favourite showing the old picture under the same name. The file's mtime
    /// is put back, since the grid is ordered by it and an edit shouldn't move
    /// a picture to the front. Through a temp name and a rename, so a failure
    /// halfway leaves the old file whole. The sidecar is left alone: it still
    /// says how the picture was made.
    pub fn overwrite_output(&self, stem: &str, display: &Path, embed: bool) -> Result<PathBuf> {
        if !valid_id(stem) {
            bail!("'{stem}' is not an output name");
        }
        let copies: Vec<PathBuf> = [&self.output, &self.favorite_outputs]
            .into_iter()
            .map(|d| d.join(format!("{stem}.png")))
            .filter(|p| p.is_file())
            .collect();
        if copies.is_empty() {
            bail!("'{stem}' is no longer in work/output/ or your favourites — use Save as new output");
        }
        // The settings from the sidecar it was saved with; a PNG dropped in by
        // hand has none and is written plain.
        let item = if embed { self.load_output(stem) } else { None };
        let bytes = output_bytes(display, item.as_ref())?;
        for png in &copies {
            let dir = png.parent().unwrap_or(&self.output);
            let mtime = std::fs::metadata(png).and_then(|m| m.modified()).ok();
            let tmp = dir.join(format!("{stem}.{}.tmp", TMP_COUNTER.fetch_add(1, Ordering::Relaxed)));
            std::fs::write(&tmp, &bytes).with_context(|| format!("writing {}", tmp.display()))?;
            if let Some(t) = mtime {
                // Best-effort: a picture saved but moved to the front of the
                // grid is a small surprise, not a failed save.
                let _ = std::fs::File::options().write(true).open(&tmp).and_then(|f| f.set_modified(t));
            }
            if let Err(e) = std::fs::rename(&tmp, png) {
                let _ = std::fs::remove_file(&tmp);
                return Err(e).with_context(|| format!("saving over {}", png.display()));
            }
            let _ = std::fs::remove_file(dir.join(format!("{stem}-thumb.png")));
            if let Err(e) = write_output_thumb(png, stem) {
                eprintln!("[yasdig] thumbnail for {stem} deferred: {e:#}");
            }
        }
        Ok(copies[0].clone())
    }
}

/// The bytes an output is written with: `display`, with the settings of `item`
/// embedded when there is one. Embedding failing (a PNG it can't parse) costs
/// the metadata, never the save.
pub fn output_bytes(display: &Path, item: Option<&WorkItem>) -> Result<Vec<u8>> {
    let bytes = std::fs::read(display).with_context(|| format!("reading {}", display.display()))?;
    Ok(match item {
        Some(item) => crate::pngmeta::embed(&bytes, item).unwrap_or_else(|e| {
            eprintln!("[yasdig] saving without settings in the PNG: {e:#}");
            bytes
        }),
        None => bytes,
    })
}

/// Make `{stem}-thumb.png` beside the output PNG `src`. Shared by
/// [`Store::ensure_output_thumb`] and [`Store::overwrite_output`], which needs
/// a fresh one beside each copy it wrote.
fn write_output_thumb(src: &Path, stem: &str) -> Result<PathBuf> {
    let dir = src.parent().context("an output has a folder")?;
    let thumb = dir.join(format!("{stem}-thumb.png"));
    let img = image::open(src)
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
    let tmp = dir.join(format!(
        "{stem}-thumb.{}.tmp",
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    // `save` picks the encoder from the extension, and this name has none
    // it would recognise, so the format is named.
    small
        .save_with_format(&tmp, image::ImageFormat::Png)
        .with_context(|| format!("writing {}", tmp.display()))?;
    // On Windows the loser's rename can fail outright instead, because the
    // winner's file may already be open for the response — and a file open
    // there can't be replaced. The race was for identical bytes, so a tile
    // that exists by now is the answer either way.
    if let Err(e) = std::fs::rename(&tmp, &thumb) {
        let _ = std::fs::remove_file(&tmp);
        if !thumb.is_file() {
            return Err(e).with_context(|| format!("publishing {}", thumb.display()));
        }
    }
    Ok(thumb)
}

impl Store {
    /// Everything in `work/output/`, newest first.
    ///
    /// Keyed by stem, and the sidecar is read for the rest — an output PNG whose
    /// sidecar is missing is still listed, because the file on disk is the thing
    /// the user saved and a picker that hides it would be lying. Such an entry
    /// just has no prompt to show.
    pub fn list_outputs(&self, gallery: Gallery) -> Vec<Output> {
        let Ok(entries) = std::fs::read_dir(self.gallery_dir(gallery)) else {
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
                let mut entry = self.output_entry(stem, &path);
                entry.favorite = gallery == Gallery::Favorites;
                Some(entry)
            })
            .collect();
        sort_outputs(&mut out);
        out
    }

    /// [`Store::list_outputs`] as the gallery shows it: Current is both
    /// galleries, the favourites starred — [`Store::listing`] one directory
    /// along. `list_outputs` stays one directory, since Close server's clear
    /// counts and deletes through it.
    pub fn output_listing(&self, gallery: Gallery) -> Vec<Output> {
        let mut out = self.list_outputs(Gallery::Favorites);
        if gallery == Gallery::Output {
            let kept: HashSet<String> = out.iter().map(|o| o.stem.clone()).collect();
            out.extend(
                self.list_outputs(Gallery::Output)
                    .into_iter()
                    .filter(|o| !kept.contains(&o.stem)),
            );
            sort_outputs(&mut out);
        }
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
            favorite: false,
        }
    }
}

/// Newest first. Stems begin with the generation's millisecond timestamp, but
/// several saves share one, so the file's own mtime is what orders them — it is
/// when *this* image was saved.
fn sort_outputs(out: &mut [Output]) {
    out.sort_by(|a, b| b.created_ms.cmp(&a.created_ms).then(b.stem.cmp(&a.stem)));
}

/// Copy one picture's files between two directories of the same layout:
/// `{stem}.png`, `{stem}.json` (required when `sidecar` is), and
/// `{stem}-thumb.png` when there is one. The modification time comes along,
/// since the output gallery is ordered by it and starring an image must not
/// move it to the front.
fn copy_picture(from: &Path, to: &Path, stem: &str, sidecar: bool) -> Result<()> {
    std::fs::create_dir_all(to).with_context(|| format!("creating {}", to.display()))?;
    for (name, required) in [
        (format!("{stem}.png"), true),
        (format!("{stem}.json"), sidecar),
        (format!("{stem}-thumb.png"), false),
    ] {
        let src = from.join(&name);
        if !required && !src.is_file() {
            continue;
        }
        let dst = to.join(&name);
        std::fs::copy(&src, &dst).with_context(|| format!("keeping {}", src.display()))?;
        if let Ok(t) = std::fs::metadata(&src).and_then(|m| m.modified()) {
            let _ = std::fs::File::options().write(true).open(&dst).and_then(|f| f.set_modified(t));
        }
    }
    Ok(())
}

/// A work item as the grid lists it. Not fields on [`WorkItem`], because that
/// struct is the sidecar and neither fact is stored: both are read off the two
/// shelves at listing time.
#[derive(Debug, Serialize)]
pub struct Listed {
    #[serde(flatten)]
    pub item: WorkItem,
    /// It lives on the favourites shelf — the tile's star is filled, and its
    /// routes (open, thumbnail) are `/favorites/…`.
    pub favorite: bool,
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
    /// It lives in `user/favorite_outputs/` — the tile's star is filled.
    pub favorite: bool,
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
            // Rolled, so every round-trip below carries the typed half too.
            typed_prompt: Some("a lovely fluffy {animal}".to_string()),
            typed_negative: None,
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
        let dir = std::env::temp_dir().join(format!("yasdig-wi-{}", new_id(0)));
        let store = Store::new(&dir, &dir.join("user"));

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
        let list = store.list(Shelf::Work);
        assert_eq!(
            list.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
            ["200-7", "100-7"]
        );

        let loaded = store.load(Shelf::Work, "100-7").unwrap();
        assert_eq!(loaded.prompt, a.prompt);
        assert_eq!(loaded.width, 320);

        // Reopening must not consume the archive — this is the invariant.
        assert!(store.image_path(Shelf::Work, "100-7").is_file());

        store.delete(Shelf::Work, "100-7").unwrap();
        for p in [
            store.image_path(Shelf::Work, "100-7"),
            store.thumb_path(Shelf::Work, "100-7"),
            store.sidecar_path(Shelf::Work, "100-7"),
        ] {
            assert!(!p.exists(), "{} survived the delete", p.display());
        }
        assert_eq!(store.list(Shelf::Work).len(), 1);
        // Deleting what isn't there is a no-op, not an error.
        store.delete(Shelf::Work, "100-7").unwrap();
        assert!(store.load(Shelf::Work, "100-7").is_err());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Saving the same origin and step twice must not overwrite the first — see
    /// `save_output`. Reopening a work item restarts `step` at 0, so this is an
    /// ordinary sequence, not a corner case.
    #[test]
    fn save_output_never_overwrites() {
        let dir = std::env::temp_dir().join(format!("yasdig-out-{}", new_id(1)));
        let store = Store::new(&dir, &dir.join("user"));
        let src = dir.join("src.png");
        image::RgbaImage::from_pixel(8, 8, image::Rgba([1, 2, 3, 255]))
            .save(&src)
            .unwrap();

        let it = item("300-7", 300);
        let first = store.save_output(&src, &it, false).unwrap();
        let second = store.save_output(&src, &it, false).unwrap();
        let third = store.save_output(&src, &it, false).unwrap();

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

    /// *Save changes*: the new pixels land over every copy of the stem, the
    /// grid keeps its order (mtime kept), each copy gets a fresh tile, and the
    /// sidecar still describes the picture.
    #[test]
    fn overwrite_output_replaces_every_copy_in_place() {
        let dir = std::env::temp_dir().join(format!("yasdig-over-{}", new_id(3)));
        let store = Store::new(&dir, &dir.join("user"));
        let old = dir.join("old.png");
        let new = dir.join("new.png");
        image::RgbaImage::from_pixel(8, 8, image::Rgba([1, 2, 3, 255])).save(&old).unwrap();
        image::RgbaImage::from_pixel(8, 8, image::Rgba([9, 9, 9, 255])).save(&new).unwrap();
        let it = item("400-1", 400);
        let saved = store.save_output(&old, &it, false).unwrap();
        let stem = saved.file_stem().unwrap().to_str().unwrap().to_string();
        // On both galleries, as a favourite kept by a build that copied is.
        copy_picture(&store.output, &store.favorite_outputs, &stem, false).unwrap();
        let fav = dir.join("user").join("favorite_outputs").join(format!("{stem}.png"));
        assert!(fav.is_file(), "{}", fav.display());

        let when = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        std::fs::File::options().write(true).open(&saved).unwrap().set_modified(when).unwrap();
        let thumb_before = std::fs::read(store.output_thumb_path(&stem)).unwrap();

        store.overwrite_output(&stem, &new, false).unwrap();
        for png in [&saved, &fav] {
            assert_eq!(std::fs::read(png).unwrap(), std::fs::read(&new).unwrap(), "{}", png.display());
        }
        assert_eq!(std::fs::metadata(&saved).unwrap().modified().unwrap(), when);
        assert_ne!(std::fs::read(store.output_thumb_path(&stem)).unwrap(), thumb_before);
        assert!(saved.with_extension("json").is_file());
        assert_eq!(store.list_outputs(Gallery::Output).len(), 1);

        assert!(store.overwrite_output("../x", &new, false).is_err());
        assert!(store.overwrite_output("999-gone", &new, false).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The picker's half of the store: a listing keyed by stem, and thumbnails
    /// that appear for images saved long before thumbnails existed.
    #[test]
    fn outputs_list_by_stem_and_grow_thumbnails() {
        let dir = std::env::temp_dir().join(format!("yasdig-pick-{}", new_id(2)));
        let store = Store::new(&dir, &dir.join("user"));
        let src = dir.join("src.png");
        image::RgbaImage::from_pixel(300, 100, image::Rgba([9, 9, 9, 255]))
            .save(&src)
            .unwrap();

        let mut it = item("400-7", 400);
        it.width = 300;
        it.height = 100;
        let first = store.save_output(&src, &it, false).unwrap();
        let second = store.save_output(&src, &it, false).unwrap();

        // Saving writes the thumbnail now, so the grid is warm immediately.
        assert!(store.output_thumb_path("400-7-s0").is_file());

        // An image saved before thumbnails existed: PNG and sidecar, no thumb.
        std::fs::copy(&src, store.output_path("100-7-s2")).unwrap();
        assert!(!store.output_thumb_path("100-7-s2").is_file());

        let list = store.list_outputs(Gallery::Output);
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
            .list_outputs(Gallery::Output)
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
        assert!(!store.list_outputs(Gallery::Output).iter().any(|o| o.stem.contains(".tmp")));

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
        let dir = std::env::temp_dir().join(format!("yasdig-del-{}", new_id(6)));
        let store = Store::new(&dir, &dir.join("user"));
        let src = dir.join("src.png");
        image::RgbaImage::from_pixel(64, 32, image::Rgba([4, 5, 6, 255]))
            .save(&src)
            .unwrap();

        let it = item("600-7", 600);
        store.save_output(&src, &it, false).unwrap();
        assert!(store.output_path("600-7-s0").is_file());
        assert!(store.output_thumb_path("600-7-s0").is_file());

        store.delete_output(Gallery::Output, "600-7-s0").unwrap();
        assert!(!store.output_path("600-7-s0").exists());
        assert!(!store.output_thumb_path("600-7-s0").exists());
        assert!(store.load_output("600-7-s0").is_none());
        assert!(store.list_outputs(Gallery::Output).is_empty());

        // Deleting what is already gone is how "delete all" behaves when two
        // tabs are open on the same directory, so it is not an error.
        store.delete_output(Gallery::Output, "600-7-s0").unwrap();
        // The traversal guard is the same whitelist as everywhere else.
        assert!(store.delete_output(Gallery::Output, "../models.toml").is_err());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// What the Outputs tab reopens an image *with*. `list_outputs` drops the
    /// profile on the way into an `Output`, so the sidecar has to be readable
    /// whole — `Display::open_output` restores the settings from it, and a
    /// sidecar that came back empty would silently reopen every saved image as
    /// `Profile::default()`.
    ///
    /// The `None` half matters just as much: a PNG dropped into the directory by
    /// hand is listed and therefore openable, so that path has to be an answer
    /// rather than an error.
    #[test]
    fn an_output_sidecar_comes_back_whole() {
        let dir = std::env::temp_dir().join(format!("yasdig-side-{}", new_id(5)));
        let store = Store::new(&dir, &dir.join("user"));
        let src = dir.join("src.png");
        image::RgbaImage::from_pixel(64, 64, image::Rgba([1, 2, 3, 255]))
            .save(&src)
            .unwrap();

        let mut it = item("500-7", 500);
        it.step = 2;
        it.refine_passes = 1;
        it.sharpness = 0.4;
        it.profile.name = "someturbo".to_string();
        store.save_output(&src, &it, false).unwrap();

        let back = store
            .load_output("500-7-s2")
            .expect("the sidecar it just wrote");
        assert_eq!(back.id, "500-7");
        assert_eq!(back.step, 2);
        assert_eq!(back.refine_passes, 1);
        assert_eq!(back.sharpness, 0.4);
        assert_eq!(back.profile.name, "someturbo");
        assert_eq!(back.prompt, it.prompt);
        // The prompt as typed rides along, so a save out of a rolled generation
        // records what was asked for as well as what was conditioned.
        assert_eq!(back.typed_prompt, it.typed_prompt);

        // Dropped in by hand: listed, openable, and with nothing to say.
        std::fs::copy(&src, store.output_path("888-1-s0")).unwrap();
        assert!(store.load_output("888-1-s0").is_none());

        std::fs::remove_dir_all(&dir).ok();
    }


    /// Every sidecar on disk before variables existed has no `typed_prompt` key
    /// in it, and there are hundreds of them in a real `work/workitems/`. The
    /// field is `#[serde(default)]` for exactly that, and `None` there means the
    /// same thing it means for a prompt with nothing to roll — the stored prompt
    /// *is* what was typed — which is why the page needs no version check, only
    /// `typed_prompt ?? prompt`.
    #[test]
    fn a_sidecar_written_before_variables_still_loads() {
        let json = r#"{
            "id": "500-7", "created_ms": 500,
            "prompt": "a lovely fluffy crab", "negative": null, "seed": 7,
            "template": "test", "width": 320, "height": 160,
            "thumb_width": 0, "thumb_height": 0,
            "profile": null
        }"#
        .replace("\"profile\": null", &format!(
            "\"profile\": {}",
            serde_json::to_string(&Profile::default()).unwrap()
        ));
        let back: WorkItem = serde_json::from_str(&json).expect("an old sidecar");
        assert_eq!(back.prompt, "a lovely fluffy crab");
        assert_eq!(back.typed_prompt, None);
        assert_eq!(back.typed_negative, None);
    }

    /// A favourite lives on its own shelf, out of `Delete all`'s reach, and is
    /// still listed among the work items, starred. Un-starring puts it back.
    /// The silent failures: a favourite missing from Current, a favourite
    /// counted by `Delete all` (which deletes through `list(Shelf::Work)`), and
    /// an item on both shelves shown twice.
    #[test]
    fn a_favourite_moves_to_its_shelf_and_is_still_listed_with_the_work() {
        let dir = std::env::temp_dir().join(format!("yasdig-fav-{}", new_id(7)));
        let store = Store::new(&dir, &dir.join("user"));
        let src = dir.join("src.png");
        image::RgbaImage::from_pixel(64, 32, image::Rgba([7, 7, 7, 255]))
            .save(&src)
            .unwrap();
        let listed = |shelf| -> Vec<(String, bool)> {
            store.listing(shelf).into_iter().map(|l| (l.item.id, l.favorite)).collect()
        };

        let mut keep = item("700-7", 700);
        let mut other = item("800-7", 800);
        store.save(&src, &mut keep).unwrap();
        store.save(&src, &mut other).unwrap();

        store.favorite("700-7").unwrap();
        for p in [
            store.image_path(Shelf::Favorites, "700-7"),
            store.thumb_path(Shelf::Favorites, "700-7"),
            store.sidecar_path(Shelf::Favorites, "700-7"),
        ] {
            assert!(p.is_file(), "{} was not kept", p.display());
        }
        // Moved: what `Delete all` and Close server's clear go through no
        // longer has it, while the grid still shows it, starred.
        assert!(!store.image_path(Shelf::Work, "700-7").exists());
        assert_eq!(
            store.list(Shelf::Work).iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
            ["800-7"]
        );
        assert_eq!(listed(Shelf::Work), [("800-7".into(), false), ("700-7".into(), true)]);
        assert_eq!(listed(Shelf::Favorites), [("700-7".into(), true)]);
        // Starring twice is still one favourite.
        store.favorite("700-7").unwrap();
        assert_eq!(listed(Shelf::Favorites).len(), 1);

        for it in store.list(Shelf::Work) {
            store.delete(Shelf::Work, &it.id).unwrap();
        }
        assert_eq!(listed(Shelf::Work), [("700-7".into(), true)]);
        // The sidecar came over whole, so a favourite reopens as the workflow
        // it was — braces and all — rather than as a picture.
        let back = store.load(Shelf::Favorites, "700-7").unwrap();
        assert_eq!(back.typed_prompt, keep.typed_prompt);
        assert_eq!(back.seed, keep.seed);

        // Un-starring is a move back, never a delete.
        store.unfavorite("700-7").unwrap();
        assert!(store.list(Shelf::Favorites).is_empty());
        assert_eq!(store.load(Shelf::Work, "700-7").unwrap().prompt, keep.prompt);
        assert_eq!(listed(Shelf::Work), [("700-7".into(), false)]);

        // A favourite kept by a build that copied: on both shelves, listed
        // once, and un-starring drops only the favourite copy.
        store.favorite("700-7").unwrap();
        copy_picture(&store.favorites, &store.items, "700-7", true).unwrap();
        assert_eq!(listed(Shelf::Work), [("700-7".into(), true)]);
        store.unfavorite("700-7").unwrap();
        assert_eq!(listed(Shelf::Work), [("700-7".into(), false)]);
        assert!(store.list(Shelf::Favorites).is_empty());

        // Nothing to keep is an error, not a silent success that would leave a
        // tile which never appears. The traversal guard is the same whitelist
        // as everywhere else.
        assert!(store.favorite("900-7").is_err());
        assert!(store.favorite("../models.toml").is_err());
        assert!(store.unfavorite("../models.toml").is_err());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The same for saved images, plus two things only a gallery has: its
    /// order is the files' mtime, which starring must not reset, and a stem
    /// names one picture wherever it lives.
    #[test]
    fn a_favourite_output_moves_and_keeps_its_place_and_name() {
        let dir = std::env::temp_dir().join(format!("yasdig-favout-{}", new_id(4)));
        let store = Store::new(&dir, &dir.join("user"));
        let src = dir.join("src.png");
        image::RgbaImage::from_pixel(64, 32, image::Rgba([7, 7, 7, 255]))
            .save(&src)
            .unwrap();
        let it = item("900-7", 900);
        let saved = store.save_output(&src, &it, false).unwrap();
        assert!(saved.ends_with("900-7-s0.png"));
        // A newer save beside it, so the order has something to keep.
        std::thread::sleep(std::time::Duration::from_millis(20));
        let newer = store.save_output(&src, &item("901-7", 901), false).unwrap();
        assert!(newer.ends_with("901-7-s0.png"));
        let order = |g| -> Vec<(String, bool)> {
            store.output_listing(g).into_iter().map(|o| (o.stem, o.favorite)).collect()
        };

        store.favorite_output("900-7-s0").unwrap();
        let fav = dir.join("user/favorite_outputs/900-7-s0.png");
        assert!(fav.is_file() && fav.with_extension("json").is_file());
        assert!(!store.output_path("900-7-s0").exists());
        // Out of what the clear deletes through, still in the gallery, starred
        // and in the place it was.
        assert_eq!(store.list_outputs(Gallery::Output).len(), 1);
        assert_eq!(
            order(Gallery::Output),
            [("901-7-s0".into(), false), ("900-7-s0".into(), true)]
        );
        assert_eq!(order(Gallery::Favorites), [("900-7-s0".into(), true)]);
        // Reading by stem finds it there — sidecar, thumbnail and all.
        assert_eq!(store.find_output("900-7-s0").as_deref(), Some(fav.as_path()));
        assert_eq!(store.load_output("900-7-s0").unwrap().seed, it.seed);
        let thumb = store.ensure_output_thumb("900-7-s0").unwrap();
        assert!(thumb.starts_with(dir.join("user/favorite_outputs")));

        // The next save of the same step must not reuse the favourite's name,
        // or one stem would mean two pictures.
        let next = store.save_output(&src, &it, false).unwrap();
        assert!(next.ends_with("900-7-s0-2.png"), "{}", next.display());
        store.delete_output(Gallery::Output, "900-7-s0-2").unwrap();

        // Un-starring moves it back, still in its place.
        store.unfavorite_output("900-7-s0").unwrap();
        assert!(store.list_outputs(Gallery::Favorites).is_empty());
        assert!(store.output_path("900-7-s0").is_file());
        assert_eq!(
            order(Gallery::Output),
            [("901-7-s0".into(), false), ("900-7-s0".into(), false)]
        );

        assert!(store.favorite_output("nope-s0").is_err());
        assert!(store.favorite_output("../models.toml").is_err());
        assert!(store.find_output("../models.toml").is_none());

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
        let dir = std::env::temp_dir().join(format!("yasdig-imp-{}", new_id(3)));
        let store = Store::new(&dir, &dir.join("user"));
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
            .list_outputs(Gallery::Output)
            .into_iter()
            .find(|o| o.stem == a)
            .expect("an import is listed as a source image");
        assert_eq!((entry.width, entry.height), (120, 60));
        assert_eq!(entry.template, "(imported)");
        assert!(!valid_id(&entry.template));

        std::fs::remove_dir_all(&dir).ok();
    }
}
