//! What the Get models tab offers: a set of files per family — two where a
//! model ships in two precisions, the first being the default — compiled in
//! from `catalog.toml`.
//!
//! The catalog is the **only** source of a URL the downloader will fetch, and
//! of the path it writes to. The page names an entry by its `id` and nothing
//! else — never a URL, never a destination — so the download routes are a
//! whitelist in the same sense `workitems::valid_id` is: what can be asked for
//! is what is listed here. Embedded rather than read from disk for the same
//! reason, and so a release carries it with no file beside the executable.
//!
//! A file names its `slot`, which is either `"model"` or a key of the family's
//! [`ImageFamily::slots`] row — so whether a file on disk already *is* that part
//! is the same `fits` test the Models fold uses, not a name comparison. That is
//! what makes a Qwen3-4B you already have under another name count, and stops
//! a second 4 GB download of it.
//!
//! Speed LoRAs are a table of their own too, `[[lora]]` ([`Lora`]): a file
//! under the LoRA dir, fetched with the cards that name it in `loras`, carrying
//! the settings it was distilled for ([`Turbo`]) — which `/loras` serves beside
//! the file once it is here, for the Image tab's *Turbo* switch.
//!
//! Video models are a table of their own, `[[video]]`, of the same [`Entry`]
//! shape: a [`VideoFamily`], its [`VideoFamily::slots`] keys, `"high_noise"`
//! for the A14B's second expert, and every `dest` under the **video dir**. Kept
//! apart so the Help tab's family table and the image cards never meet one,
//! and kept out of the image cards' listing.

use crate::config::{ImageFamily, PartSlot, VideoFamily};
use crate::discover::{DitKind, Role};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path};
use std::sync::OnceLock;

const SOURCE: &str = include_str!("catalog.toml");

#[derive(Debug, Deserialize)]
pub struct Catalog {
    #[serde(default)]
    pub entry: Vec<Entry>,
    /// Video models: [`Entry`]s whose family is a [`VideoFamily`].
    #[serde(default)]
    pub video: Vec<Entry>,
    /// Upscaler models, a table of their own: see [`Upscaler`].
    #[serde(default)]
    pub upscaler: Vec<Upscaler>,
    /// Speed LoRAs: see [`Lora`].
    #[serde(default)]
    pub lora: Vec<Lora>,
}

/// A distilled ("turbo") LoRA: a model's few-step version as an adapter. Not an
/// [`Entry`] file, because it lives in [`crate::config::Config::lora_dir`],
/// which may be anywhere, and because it is had only under its own `dest` at its
/// own length — two LoRAs for one family are different adapters, so a name-free
/// "something fits" would be wrong (an [`Upscaler`]'s rule).
#[derive(Debug, Deserialize)]
pub struct Lora {
    pub id: String,
    pub family: ImageFamily,
    pub label: String,
    pub url: String,
    /// Relative to the LoRA dir, `/`-separated — and so the name the page's
    /// LoRA picker and the `<lora:…>` tag carry.
    pub dest: String,
    pub bytes: u64,
    /// Hugging Face's `X-Linked-Etag`, required: the record of which file the
    /// steps and CFG in `turbo` were checked against.
    pub sha256: String,
    pub turbo: Turbo,
}

/// What a speed LoRA was distilled to run at. Applied by the page's *Turbo*
/// switch onto the boxes, never by the server: a LoRA is request-only
/// (`a_model_brings_no_loras_of_its_own`), so its settings are too.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Turbo {
    pub steps: u32,
    pub cfg_scale: f32,
    /// A named fixed schedule it was distilled on (`config::SIGMA_PRESETS`),
    /// sent as `sigmas` with the LoRA. Its length is `steps`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sigmas: Option<String>,
}

/// One upscaler model. Not an [`Entry`]: it has no family and no parts, lives in
/// [`crate::config::Config::upscaler_dir`] rather than the models dir, is often
/// a `.pth`, and is told apart by [`crate::sr::identify`] — so whether it is had
/// is "its `dest` is there and reads as `arch`", never a name-free match (two 4×
/// ESRGANs are different models).
#[derive(Debug, Deserialize)]
pub struct Upscaler {
    pub id: String,
    pub label: String,
    pub note: String,
    /// What [`crate::sr::arch::Arch::name`] calls the file once it is here.
    pub arch: String,
    pub url: String,
    /// Relative to the upscaler dir, `/`-separated.
    pub dest: String,
    pub bytes: u64,
}

/// An entry's family: an image one under `[[entry]]`, a video one under
/// `[[video]]` (`parse` holds each table to its kind). Untagged, since the two
/// enums' wire names don't overlap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum Family {
    Image(ImageFamily),
    Video(VideoFamily),
}

impl Family {
    pub fn slots(self) -> &'static [PartSlot] {
        match self {
            Family::Image(f) => f.slots(),
            Family::Video(f) => f.slots(),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Family::Image(f) => f.label(),
            Family::Video(f) => f.label(),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Family::Image(f) => f.as_str(),
            Family::Video(f) => f.as_str(),
        }
    }

    pub fn is_video(self) -> bool {
        matches!(self, Family::Video(_))
    }
}

#[derive(Debug, Deserialize)]
pub struct Entry {
    pub id: String,
    pub family: Family,
    pub label: String,
    /// A card a newcomer should start from, marked on the tab and its family
    /// in the Image tab's model list — where it fits ([`Catalog::recommended`]).
    #[serde(default)]
    pub recommended: bool,
    /// Roughly what the set occupies on the GPU, in GB: what a recommendation
    /// is checked against.
    pub card_gb: Option<f32>,
    /// Recommended only on a GPU with less VRAM than this: the pick for a card
    /// the others don't fit.
    pub recommend_below_gb: Option<f32>,
    pub note: String,
    pub file: Vec<File>,
    /// [`Lora`] ids fetched with this card, shown as rows of it.
    #[serde(default)]
    pub loras: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct File {
    pub slot: String,
    pub url: String,
    /// Relative to the models dir, `/`-separated.
    pub dest: String,
    pub bytes: u64,
    /// Hugging Face's `X-Linked-Etag`. Required where entries share a `dest`
    /// from different URLs — the proof the copies are one file. Not checked
    /// after a download (hashing gigabytes per file is minutes); the length is.
    pub sha256: Option<String>,
    /// On a `model` file only: the settings this exact file was distilled to
    /// run at, where its header can't say it is the distilled one (Iris-3B
    /// Turbo has every tensor of the base). Applied to the model found at
    /// this `dest` at this length ([`Catalog::distilled`]) as a layer over the
    /// family's recipe.
    pub distilled: Option<Turbo>,
}

impl Catalog {
    /// The embedded catalog, parsed once. `catalog_is_consistent` is what makes
    /// the `expect` safe: a malformed `catalog.toml` fails `cargo test`, never a
    /// start.
    pub fn get() -> &'static Catalog {
        static CATALOG: OnceLock<Catalog> = OnceLock::new();
        CATALOG.get_or_init(|| parse(SOURCE).expect("src/catalog.toml is checked by cargo test"))
    }

    /// An image or a video entry.
    pub fn entry(&self, id: &str) -> Option<&Entry> {
        self.entry.iter().chain(&self.video).find(|e| e.id == id)
    }

    pub fn upscaler(&self, id: &str) -> Option<&Upscaler> {
        self.upscaler.iter().find(|u| u.id == id)
    }

    pub fn lora(&self, id: &str) -> Option<&Lora> {
        self.lora.iter().find(|l| l.id == id)
    }

    /// The speed LoRAs a card fetches with it.
    pub fn loras_of<'a>(&'a self, e: &'a Entry) -> impl Iterator<Item = &'a Lora> + 'a {
        e.loras.iter().filter_map(|id| self.lora(id))
    }

    /// Whether `e` is a recommendation on a GPU with `vram` bytes: curated,
    /// and fitting it. With the VRAM unknown (no amdgpu sysfs — Windows), the
    /// curated cards for a large GPU, since nothing says otherwise.
    pub fn recommended(e: &Entry, vram: Option<u64>) -> bool {
        // In GiB, the unit a card is sold by: a "24 GB" card is 24 GiB.
        let gb = vram.map(|b| b as f32 / (1u64 << 30) as f32);
        e.recommended
            && match (gb, e.card_gb, e.recommend_below_gb) {
                (None, _, below) => below.is_none(),
                (Some(have), need, below) => need.is_none_or(|n| n <= have) && below.is_none_or(|b| have < b),
            }
    }

    /// Whether some recommended card is of this family — what the Image tab's
    /// model list marks. By family, since the installed file may be any
    /// quantization of it.
    pub fn recommends(&self, family: ImageFamily, vram: Option<u64>) -> bool {
        self.entry.iter().any(|e| e.family == Family::Image(family) && Catalog::recommended(e, vram))
    }

    /// The settings a model file at `path` was distilled to run at: a catalog
    /// `model` file with `distilled`, at its own dest under `models` and its
    /// own length — the file the download wrote. A hint, never a refusal: a
    /// renamed copy just starts at the family's recipe.
    pub fn distilled(&self, models: &Path, path: &Path) -> Option<&Turbo> {
        let len = std::fs::metadata(path).ok()?.len();
        self.entry
            .iter()
            .flat_map(|e| &e.file)
            .find(|f| f.slot == "model" && f.bytes == len && models.join(&f.dest) == path)?
            .distilled
            .as_ref()
    }

    /// Whether `family` has more than one card — two precisions of one model,
    /// whose model files must then be told apart by name (`downloads::present`).
    pub fn has_several(&self, family: Family) -> bool {
        self.entry.iter().chain(&self.video).filter(|e| e.family == family).count() > 1
    }
}

/// The repository page a file comes from — where its licence is. Derived from
/// the URL rather than stored, so the two can't disagree; `parse` has already
/// checked the URL is a `huggingface.co/{owner}/{repo}/resolve/…`.
fn repo_page(url: &str) -> String {
    let rest = url.trim_start_matches("https://huggingface.co/");
    let repo = rest.split("/resolve/").next().unwrap_or(rest);
    format!("https://huggingface.co/{repo}")
}

fn hf_resolve(url: &str) -> bool {
    url.starts_with("https://huggingface.co/") && url.contains("/resolve/")
}

impl File {
    pub fn page(&self) -> String {
        repo_page(&self.url)
    }
}

impl Upscaler {
    pub fn page(&self) -> String {
        repo_page(&self.url)
    }
}

impl Lora {
    pub fn page(&self) -> String {
        repo_page(&self.url)
    }
}

impl Entry {
    pub fn bytes(&self) -> u64 {
        self.file.iter().map(|f| f.bytes).sum()
    }

    /// The `slots()` row a file fills, or `None` for the model itself.
    pub fn slot(&self, f: &File) -> Option<&'static PartSlot> {
        self.family.slots().iter().find(|s| s.key == f.slot)
    }

    pub fn slot_label(&self, f: &File) -> &'static str {
        match (self.slot(f), f.slot.as_str()) {
            (Some(s), _) => s.label,
            (None, "high_noise") => "High-noise model",
            (None, _) => "Model",
        }
    }

    /// Whether a discovered file can stand in for `f` — for a part the slot's
    /// own test, for the model any model of this family. The second is
    /// deliberately loose: someone with a Klein 9B in another quantization
    /// wants its encoder, not a second copy of the transformer. The path is
    /// for the A14B, whose two experts only their names tell apart.
    pub fn fits(&self, f: &File, path: &Path, role: &Role) -> bool {
        if let Some(s) = self.slot(f) {
            return (s.fits)(role);
        }
        match self.family {
            Family::Image(fam) => matches!(role, Role::Image { family, .. } if *family == fam),
            Family::Video(fam) => video_model_fits(fam, &f.slot, path, role),
        }
    }
}

/// [`Entry::fits`] for a video model's transformer: the [`DitKind`] discovery
/// would make this family of, and for the A14B the half its name says.
fn video_model_fits(family: VideoFamily, slot: &str, path: &Path, role: &Role) -> bool {
    let Role::Dit(kind) = role else {
        return false;
    };
    let half = crate::discover::noise_marker(path);
    match (family, kind, slot) {
        (VideoFamily::Wan22Ti2v5b, DitKind::Wan22Ti2v5b, "model") => true,
        (VideoFamily::Wan22A14b, DitKind::Wan22A14b, "model") => half == Some(false),
        (VideoFamily::Wan22A14b, DitKind::Wan22A14b, "high_noise") => half == Some(true),
        (VideoFamily::Wan21Vace, DitKind::WanVace { .. }, "model") => true,
        (VideoFamily::WanDense, DitKind::WanOther { .. }, "model") => true,
        (VideoFamily::WanDense, DitKind::Wan22A14b, "model") => half.is_none(),
        (VideoFamily::MinimaxH3, DitKind::MinimaxH3, "model") => true,
        _ => false,
    }
}

fn parse(src: &str) -> Result<Catalog> {
    let c: Catalog = toml::from_str(src).context("parsing src/catalog.toml")?;
    let all: Vec<&Entry> = c.entry.iter().chain(&c.video).collect();
    for (i, e) in all.iter().enumerate() {
        if all[..i].iter().any(|o| o.id == e.id) {
            bail!("catalog: id {:?} is listed twice", e.id);
        }
        match e.family {
            Family::Image(f) if !f.runnable() => bail!("catalog: {} is a family this build doesn't run", e.id),
            Family::Image(_) if c.video.iter().any(|v| v.id == e.id) => {
                bail!("catalog: {} is under [[video]] with an image family", e.id)
            }
            Family::Video(_) if c.entry.iter().any(|v| v.id == e.id) => {
                bail!("catalog: {} is under [[entry]] with a video family", e.id)
            }
            _ => {}
        }
        if e.file.iter().filter(|f| f.slot == "model").count() != 1 {
            bail!("catalog: {} needs exactly one file with slot = \"model\"", e.id);
        }
        // The second expert, exactly for the pair that has one.
        let moe = matches!(e.family, Family::Video(f) if f.is_moe());
        if e.file.iter().filter(|f| f.slot == "high_noise").count() != usize::from(moe) {
            bail!("catalog: {} needs {} file with slot = \"high_noise\"", e.id, if moe { "exactly one" } else { "no" });
        }
        for f in &e.file {
            if !matches!(f.slot.as_str(), "model" | "high_noise") && e.slot(f).is_none() {
                bail!("catalog: {} names slot {:?}, which {} has no row for", e.id, f.slot, e.family.label());
            }
            if !safe_dest(&f.dest, &f.slot) {
                bail!("catalog: {}'s dest {:?} must be a plain relative path", e.id, f.dest);
            }
            if !hf_resolve(&f.url) {
                bail!("catalog: {}'s url must be a huggingface.co resolve link: {}", e.id, f.url);
            }
            if f.bytes == 0 {
                bail!("catalog: {}'s {} has no size", e.id, f.dest);
            }
            if let Some(d) = &f.distilled {
                let fam = match e.family {
                    Family::Image(fam) if f.slot == "model" => fam,
                    _ => bail!("catalog: {}'s distilled settings belong on an image model file", e.id),
                };
                if d.sigmas.as_deref().is_some_and(|s| !crate::config::SIGMA_PRESETS.iter().any(|(n, f, _)| *n == s && *f == fam)) {
                    bail!("catalog: {}'s distilled sigmas is no {} preset", e.id, fam.label());
                }
            }
        }
    }
    // One destination is one file. Entries sharing a VAE may each fetch it
    // from their own model's repo — the card links it as where the licence is
    // — but then both copies must carry the same `sha256`, or a resume could
    // splice two different files and the second card would read "wrong". Per
    // table, since the two tables' dests are under different dirs.
    for table in [&c.entry, &c.video] {
        check_shared_dests(table)?;
    }
    check_loras(&c)?;
    check_upscalers(c)
}

fn check_loras(c: &Catalog) -> Result<()> {
    for (i, l) in c.lora.iter().enumerate() {
        if c.entry(&l.id).is_some() || c.lora[..i].iter().any(|o| o.id == l.id || o.dest == l.dest) {
            bail!("catalog: lora {:?} is listed twice", l.id);
        }
        if !l.family.runnable() {
            bail!("catalog: lora {} is for a family this build doesn't run", l.id);
        }
        // A LoRA's name is a `<lora:…>` tag, so `valid_lora_name` is the rule,
        // and sd.cpp's LoRA loader reads safetensors only.
        let p = Path::new(&l.dest);
        if !(crate::config::valid_lora_name(&l.dest)
            && p.components().all(|c| matches!(c, Component::Normal(_)))
            && p.extension().and_then(|x| x.to_str()) == Some("safetensors"))
        {
            bail!("catalog: lora {}'s dest {:?} must be a plain .safetensors name", l.id, l.dest);
        }
        if !hf_resolve(&l.url) {
            bail!("catalog: lora {}'s url must be a huggingface.co resolve link: {}", l.id, l.url);
        }
        if l.bytes == 0 || l.turbo.steps == 0 || l.sha256.len() != 64 {
            bail!("catalog: lora {} needs a size, a sha256 and its steps", l.id);
        }
        if let Some(s) = &l.turbo.sigmas {
            if !crate::config::SIGMA_PRESETS.iter().any(|(n, f, _)| *n == s && *f == l.family) {
                bail!("catalog: lora {}'s sigmas {s:?} is no {} preset", l.id, l.family.label());
            }
        }
    }
    for e in c.entry.iter().chain(&c.video) {
        for id in &e.loras {
            match c.lora(id) {
                Some(l) if e.family == Family::Image(l.family) => {}
                Some(_) => bail!("catalog: {} fetches lora {id}, which is for another family", e.id),
                None => bail!("catalog: {} fetches lora {id}, which isn't listed", e.id),
            }
        }
    }
    Ok(())
}

fn check_shared_dests(table: &[Entry]) -> Result<()> {
    for f in table.iter().flat_map(|e| &e.file) {
        for o in table.iter().flat_map(|o| &o.file) {
            if o.dest != f.dest {
                continue;
            }
            if o.bytes != f.bytes {
                bail!("catalog: {} is listed with two different sizes", f.dest);
            }
            if o.url != f.url && (f.sha256.is_none() || o.sha256 != f.sha256) {
                bail!("catalog: {} comes from two URLs without one sha256 proving they are one file", f.dest);
            }
        }
    }
    Ok(())
}

fn check_upscalers(c: Catalog) -> Result<Catalog> {
    for (i, u) in c.upscaler.iter().enumerate() {
        if c.entry(&u.id).is_some() || c.lora(&u.id).is_some() || c.upscaler[..i].iter().any(|o| o.id == u.id) {
            bail!("catalog: id {:?} is listed twice", u.id);
        }
        if !safe_upscaler_dest(&u.dest) {
            bail!("catalog: {}'s dest {:?} must be a plain .pth or .safetensors name", u.id, u.dest);
        }
        if !hf_resolve(&u.url) {
            bail!("catalog: {}'s url must be a huggingface.co resolve link: {}", u.id, u.url);
        }
        if u.bytes == 0 {
            bail!("catalog: {} has no size", u.id);
        }
        if c.upscaler[..i].iter().any(|o| o.dest == u.dest) {
            bail!("catalog: {} is listed twice", u.dest);
        }
    }
    Ok(c)
}

/// An upscaler's destination: [`safe_dest`]'s rule with the extensions
/// [`crate::config::Config::list_upscalers`] lists and sd-cli or `sr` can read.
/// `.gguf` is left out: no upscaler ships as one.
fn safe_upscaler_dest(dest: &str) -> bool {
    let p = Path::new(dest);
    crate::config::valid_lora_name(dest)
        && p.components().all(|c| matches!(c, Component::Normal(_)))
        && matches!(p.extension().and_then(|x| x.to_str()), Some("pth" | "safetensors"))
}

/// A destination is relative, made only of plain names, and is a model file
/// the scan will read — `valid_lora_name`'s rule plus an extension check, so a
/// finished download can't land somewhere discovery won't look. A `.part` name
/// is refused outright: it is the downloader's in-flight spelling.
///
/// The `tokenizer` slot is the one place `.json` is allowed: the scan reads a
/// JSON file only to see whether it is a tokenizer, so nothing else would be
/// found after the download.
fn safe_dest(dest: &str, slot: &str) -> bool {
    let p = Path::new(dest);
    let ext = p.extension().and_then(|x| x.to_str());
    crate::config::valid_lora_name(dest)
        && p.components().all(|c| matches!(c, Component::Normal(_)))
        && match slot {
            "tokenizer" => ext == Some("json"),
            _ => matches!(ext, Some("safetensors" | "gguf")),
        }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_consistent() {
        let c = parse(SOURCE).unwrap();
        assert!(!c.entry.is_empty());
        // Every runnable family has something to download: the tab is where a
        // newcomer starts, and a family missing from it reads as unsupported.
        for fam in ImageFamily::ALL.iter().filter(|f| f.runnable()) {
            if matches!(fam, ImageFamily::Sd1 | ImageFamily::Sd2) {
                continue; // superseded; docs/configuration.md's manual list still covers them
            }
            assert!(c.entry.iter().any(|e| e.family == Family::Image(*fam)), "{} has no catalog entry", fam.label());
        }
        // Every image card says what it needs on the GPU, which is what a
        // recommendation is checked against.
        for e in &c.entry {
            assert!(e.card_gb.is_some_and(|g| g > 0.0), "{} has no card_gb", e.id);
        }
        // A DiT entry carries every part its family needs, so one Download
        // makes it runnable.
        for e in &c.entry {
            for s in e.family.slots() {
                let bundled = matches!(e.family, Family::Image(f) if f.is_unet());
                if !bundled {
                    assert!(e.file.iter().any(|f| f.slot == s.key), "{} lacks its {}", e.id, s.label);
                }
            }
        }
        // Every upscaler names an architecture this build runs — a listed
        // download that the select then greys out would be a broken promise.
        assert!(!c.upscaler.is_empty());
        let runnable = ["ESRGAN", "Compact", "SPAN", "SwinIR-S", "SwinIR-M", "SwinIR-L"];
        for u in &c.upscaler {
            assert!(runnable.contains(&u.arch.as_str()), "{} names arch {:?}", u.id, u.arch);
        }
    }

    /// A video card carries every part its family can't run without, so one
    /// Download makes it runnable, and only its own kind of family.
    #[test]
    fn the_video_cards_are_complete() {
        let c = parse(SOURCE).unwrap();
        assert!(!c.video.is_empty());
        for e in &c.video {
            assert!(e.family.is_video(), "{}", e.id);
            for s in e.family.slots().iter().filter(|s| !s.optional) {
                assert!(e.file.iter().any(|f| f.slot == s.key), "{} lacks its {}", e.id, s.label);
            }
        }
        // An image family under [[video]] is refused, and the reverse.
        let wrong = "[[video]]\nid = \"v\"\nfamily = \"z_image\"\nlabel = \"v\"\nnote = \"\"\n[[video.file]]\nslot = \"model\"\nurl = \"https://huggingface.co/a/b/resolve/main/m.gguf\"\ndest = \"m.gguf\"\nbytes = 1\n";
        assert!(parse(wrong).is_err());
        assert!(parse(&wrong.replace("z_image", "wan22_ti2v_5b").replace("[[video", "[[entry")).is_err());
        assert!(parse(&wrong.replace("z_image", "wan22_ti2v_5b")).is_ok());
    }

    /// The A14B's two experts have the same tensors; only the name says which
    /// slot a file fills.
    #[test]
    fn each_expert_fits_only_its_own_slot() {
        let c = Catalog::get();
        let pair = c.video.iter().find(|e| e.family == Family::Video(VideoFamily::Wan22A14b)).unwrap();
        let slot = |k: &str| pair.file.iter().find(|f| f.slot == k).unwrap();
        let role = Role::Dit(DitKind::Wan22A14b);
        let low = Path::new("/m/Wan2.2-I2V-A14B-LowNoise-Q4_K_M.gguf");
        let high = Path::new("/m/Wan2.2-I2V-A14B-HighNoise-Q4_K_M.gguf");
        assert!(pair.fits(slot("model"), low, &role));
        assert!(!pair.fits(slot("model"), high, &role));
        assert!(pair.fits(slot("high_noise"), high, &role));
        assert!(!pair.fits(slot("high_noise"), Path::new("/m/unmarked.gguf"), &role));
    }

    #[test]
    fn a_bad_upscaler_is_refused() {
        let up = "[[upscaler]]\nid = \"u\"\nlabel = \"u\"\nnote = \"\"\narch = \"SPAN\"\nurl = \"https://huggingface.co/a/b/resolve/main/u.pth\"\ndest = \"u.pth\"\nbytes = 1\n";
        assert!(parse(up).is_ok());
        for bad in [
            up.replace("dest = \"u.pth\"", "dest = \"../u.pth\""),
            up.replace("dest = \"u.pth\"", "dest = \"u.gguf\""),
            up.replace("dest = \"u.pth\"", "dest = \"u.pth.part\""),
            up.replace("https://huggingface.co/a/b/resolve", "https://example.com/a/b/resolve"),
            up.replace("bytes = 1", "bytes = 0"),
            format!("{up}{up}"),
        ] {
            assert!(parse(&bad).is_err(), "accepted:\n{bad}");
        }
    }

    #[test]
    fn a_bad_catalog_is_refused() {
        let base = |file: &str| {
            format!(
                "[[entry]]\nid = \"x\"\nfamily = \"z_image\"\nlabel = \"x\"\nnote = \"\"\n{file}"
            )
        };
        let model = "[[entry.file]]\nslot = \"model\"\nurl = \"https://huggingface.co/a/b/resolve/main/m.gguf\"\ndest = \"diffusion_models/m.gguf\"\nbytes = 1\n";
        assert!(parse(&base(model)).is_ok());
        for bad in [
            model.replace("diffusion_models/m.gguf", "../m.gguf"),
            model.replace("diffusion_models/m.gguf", "/etc/m.gguf"),
            model.replace("diffusion_models/m.gguf", "m.gguf.part"),
            model.replace("https://huggingface.co/a/b/resolve", "http://example.com/a/b/resolve"),
            model.replace("slot = \"model\"", "slot = \"clip_l\""),
            model.replace("bytes = 1", "bytes = 0"),
        ] {
            assert!(parse(&base(&bad)).is_err(), "accepted:\n{bad}");
        }
        // Two entries sharing a dest from two repos: only with one hash.
        let two = |sha_a: &str, sha_b: &str| {
            let vae = |repo: &str, sha: &str| {
                format!("[[entry.file]]\nslot = \"vae\"\nurl = \"https://huggingface.co/{repo}/resolve/main/ae.safetensors\"\ndest = \"vae/ae.safetensors\"\nbytes = 5\n{sha}")
            };
            let a = base(&format!("{model}{}", vae("a/one", sha_a)));
            let b = base(&format!("{}{}", model.replace("m.gguf", "n.gguf"), vae("b/two", sha_b))).replace("id = \"x\"", "id = \"y\"");
            parse(&format!("{a}{b}"))
        };
        assert!(two("sha256 = \"ab\"\n", "sha256 = \"ab\"\n").is_ok());
        assert!(two("sha256 = \"ab\"\n", "sha256 = \"cd\"\n").is_err());
        assert!(two("", "").is_err());
    }

    /// A recommendation fits the GPU: the 21 GB BF16 MOSS is never one, the
    /// INT8 on a 24 GB card is and on a 12 GB one isn't, and Iris only below
    /// 12 GB — and with the VRAM unknown, the curated large-GPU set.
    #[test]
    fn a_recommendation_fits_the_gpu() {
        let c = Catalog::get();
        let e = |id: &str| c.entry(id).unwrap();
        let gb = |g: u64| Some(g << 30);
        assert!(Catalog::recommended(e("moss-image-sft-int8"), gb(24)));
        assert!(!Catalog::recommended(e("moss-image-sft-int8"), gb(12)));
        assert!(!Catalog::recommended(e("moss-image-sft-bf16"), gb(24)));
        assert!(!Catalog::recommended(e("iris-3b-turbo"), gb(24)));
        assert!(Catalog::recommended(e("iris-3b-turbo"), gb(8)));
        assert!(!Catalog::recommended(e("iris-3b"), gb(8)), "the base is 200 passes a picture");
        assert!(Catalog::recommended(e("flux3-klein-q8"), None));
        assert!(!Catalog::recommended(e("iris-3b-turbo"), None));
        assert!(c.has_several(Family::Image(ImageFamily::MossImage)));
        assert!(!c.has_several(Family::Image(ImageFamily::Krea2)));
    }

    /// Iris-3B Turbo has the base's every tensor, so its 4 steps ride on the
    /// file: at the card's own dest and length, and nowhere else.
    #[test]
    fn a_distilled_file_is_known_by_its_dest_and_length() {
        let dir = std::env::temp_dir().join(format!("yasdig-distilled-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("diffusion_models")).unwrap();
        let c = Catalog::get();
        let at = dir.join("diffusion_models/iris-3b-turbo.safetensors");
        // Sparse: the length is what is compared, and costs nothing.
        std::fs::File::create(&at).unwrap().set_len(5975089200).unwrap();
        let d = c.distilled(&dir, &at).expect("the Turbo at its dest");
        assert_eq!((d.steps, d.cfg_scale, d.sigmas.as_deref()), (4, 1.0, Some("iris-turbo")));
        assert_eq!(crate::config::sigma_arg("iris-turbo"), Some("1.0,0.938,0.833,0.625,0.0"));
        std::fs::File::create(&at).unwrap().set_len(5).unwrap();
        assert!(c.distilled(&dir, &at).is_none(), "another length is another file");
        let base = dir.join("diffusion_models/iris-3b.safetensors");
        std::fs::File::create(&base).unwrap().set_len(11950112680).unwrap();
        assert!(c.distilled(&dir, &base).is_none(), "the base runs its recipe");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A speed LoRA is a plain `.safetensors` name a `<lora:…>` tag can carry,
    /// and a card fetches only LoRAs listed for its own family.
    #[test]
    fn a_speed_lora_belongs_to_its_cards_family() {
        let c = Catalog::get();
        let moss = c.entry.iter().filter(|e| e.family == Family::Image(ImageFamily::MossImage));
        for e in moss {
            assert_eq!(c.loras_of(e).count(), 1, "{} fetches its turbo LoRA", e.id);
        }
        let lora = "[[lora]]\nid = \"l\"\nfamily = \"z_image\"\nlabel = \"l\"\nurl = \"https://huggingface.co/a/b/resolve/main/l.safetensors\"\ndest = \"l.safetensors\"\nbytes = 1\nsha256 = \"0000000000000000000000000000000000000000000000000000000000000000\"\nturbo = { steps = 8, cfg_scale = 1.0 }\n";
        assert!(parse(lora).is_ok());
        let card = |family: &str| {
            format!("[[entry]]\nid = \"x\"\nfamily = \"{family}\"\nlabel = \"x\"\nnote = \"\"\nloras = [\"l\"]\n[[entry.file]]\nslot = \"model\"\nurl = \"https://huggingface.co/a/b/resolve/main/m.gguf\"\ndest = \"m.gguf\"\nbytes = 1\n")
        };
        assert!(parse(&format!("{}{lora}", card("z_image"))).is_ok());
        assert!(parse(&format!("{}{lora}", card("krea2"))).is_err());
        assert!(parse(&card("z_image")).is_err(), "names a LoRA nobody lists");
        for bad in [
            lora.replace("l.safetensors\"\nbytes", "../l.safetensors\"\nbytes"),
            lora.replace("dest = \"l.safetensors\"", "dest = \"l.gguf\""),
            lora.replace("dest = \"l.safetensors\"", "dest = \"a:b.safetensors\""),
            lora.replace("steps = 8", "steps = 0"),
            format!("{lora}{lora}"),
        ] {
            assert!(parse(&bad).is_err(), "accepted:\n{bad}");
        }
    }

    #[test]
    fn a_part_fits_by_role_not_by_name() {
        use crate::discover::LlmKind;
        let c = Catalog::get();
        let klein9 = c.entry.iter().find(|e| e.family == Family::Image(ImageFamily::Flux2Klein9b)).unwrap();
        let llm = klein9.file.iter().find(|f| f.slot == "llm").unwrap();
        assert!(klein9.fits(llm, Path::new("x"), &Role::Llm(LlmKind::Qwen3 { dim: 4096 })));
        // The 4B is the encoder people reach for by mistake.
        assert!(!klein9.fits(llm, Path::new("x"), &Role::Llm(LlmKind::Qwen3 { dim: 2560 })));
        let model = klein9.file.iter().find(|f| f.slot == "model").unwrap();
        assert!(klein9.fits(model, Path::new("x"), &Role::Image { family: ImageFamily::Flux2Klein9b, bundled: false }));
        assert!(!klein9.fits(model, Path::new("x"), &Role::Image { family: ImageFamily::Flux2Klein4b, bundled: false }));
    }

    #[test]
    fn a_files_page_is_its_repository() {
        let c = Catalog::get();
        let files = c.entry.iter().flat_map(|e| &e.file).map(|f| (f.page(), &f.url));
        let ups = c.upscaler.iter().map(|u| (u.page(), &u.url));
        for (page, url) in files.chain(ups) {
            assert!(!page.contains("/resolve/") && page.matches('/').count() == 4, "{page}");
            assert!(url.starts_with(&format!("{page}/resolve/")), "{page}");
        }
    }
}
