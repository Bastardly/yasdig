//! What the Get models tab offers: one recommended set of files per family,
//! compiled in from `catalog.toml`.
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

use crate::config::{ImageFamily, PartSlot};
use crate::discover::Role;
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::{Component, Path};
use std::sync::OnceLock;

const SOURCE: &str = include_str!("catalog.toml");

#[derive(Debug, Deserialize)]
pub struct Catalog {
    #[serde(default)]
    pub entry: Vec<Entry>,
    /// Upscaler models, a table of their own: see [`Upscaler`].
    #[serde(default)]
    pub upscaler: Vec<Upscaler>,
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

#[derive(Debug, Deserialize)]
pub struct Entry {
    pub id: String,
    pub family: ImageFamily,
    pub label: String,
    pub note: String,
    pub file: Vec<File>,
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
}

impl Catalog {
    /// The embedded catalog, parsed once. `catalog_is_consistent` is what makes
    /// the `expect` safe: a malformed `catalog.toml` fails `cargo test`, never a
    /// start.
    pub fn get() -> &'static Catalog {
        static CATALOG: OnceLock<Catalog> = OnceLock::new();
        CATALOG.get_or_init(|| parse(SOURCE).expect("src/catalog.toml is checked by cargo test"))
    }

    pub fn entry(&self, id: &str) -> Option<&Entry> {
        self.entry.iter().find(|e| e.id == id)
    }

    pub fn upscaler(&self, id: &str) -> Option<&Upscaler> {
        self.upscaler.iter().find(|u| u.id == id)
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

impl Entry {
    pub fn bytes(&self) -> u64 {
        self.file.iter().map(|f| f.bytes).sum()
    }

    /// The `slots()` row a file fills, or `None` for the model itself.
    pub fn slot(&self, f: &File) -> Option<&'static PartSlot> {
        self.family.slots().iter().find(|s| s.key == f.slot)
    }

    pub fn slot_label(&self, f: &File) -> &'static str {
        match self.slot(f) {
            Some(s) => s.label,
            None => "Model",
        }
    }

    /// Whether a discovered file can stand in for `f` — for a part the slot's
    /// own test, for the model any image model of this family. The second is
    /// deliberately loose: someone with a Klein 9B in another quantization
    /// wants its encoder, not a second copy of the transformer.
    pub fn fits(&self, f: &File, role: &Role) -> bool {
        match self.slot(f) {
            Some(s) => (s.fits)(role),
            None => matches!(role, Role::Image { family, .. } if *family == self.family),
        }
    }
}

fn parse(src: &str) -> Result<Catalog> {
    let c: Catalog = toml::from_str(src).context("parsing src/catalog.toml")?;
    for (i, e) in c.entry.iter().enumerate() {
        if c.entry[..i].iter().any(|o| o.id == e.id) {
            bail!("catalog: id {:?} is listed twice", e.id);
        }
        if !e.family.runnable() {
            bail!("catalog: {} is a family this build doesn't run", e.id);
        }
        if e.file.iter().filter(|f| f.slot == "model").count() != 1 {
            bail!("catalog: {} needs exactly one file with slot = \"model\"", e.id);
        }
        for f in &e.file {
            if f.slot != "model" && e.slot(f).is_none() {
                bail!("catalog: {} names slot {:?}, which {} has no row for", e.id, f.slot, e.family.label());
            }
            if !safe_dest(&f.dest) {
                bail!("catalog: {}'s dest {:?} must be a plain relative path", e.id, f.dest);
            }
            if !hf_resolve(&f.url) {
                bail!("catalog: {}'s url must be a huggingface.co resolve link: {}", e.id, f.url);
            }
            if f.bytes == 0 {
                bail!("catalog: {}'s {} has no size", e.id, f.dest);
            }
        }
    }
    // One destination is one file. Entries sharing a VAE may each fetch it
    // from their own model's repo — the card links it as where the licence is
    // — but then both copies must carry the same `sha256`, or a resume could
    // splice two different files and the second card would read "wrong".
    for f in c.entry.iter().flat_map(|e| &e.file) {
        for o in c.entry.iter().flat_map(|o| &o.file) {
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
    for (i, u) in c.upscaler.iter().enumerate() {
        if c.entry.iter().any(|e| e.id == u.id) || c.upscaler[..i].iter().any(|o| o.id == u.id) {
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
fn safe_dest(dest: &str) -> bool {
    let p = Path::new(dest);
    crate::config::valid_lora_name(dest)
        && p.components().all(|c| matches!(c, Component::Normal(_)))
        && matches!(p.extension().and_then(|x| x.to_str()), Some("safetensors" | "gguf"))
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
            assert!(c.entry.iter().any(|e| e.family == *fam), "{} has no catalog entry", fam.label());
        }
        // A DiT entry carries every part its family needs, so one Download
        // makes it runnable.
        for e in &c.entry {
            for s in e.family.slots() {
                let bundled = matches!(e.family, ImageFamily::Sd1 | ImageFamily::Sd2 | ImageFamily::Sdxl);
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

    #[test]
    fn a_part_fits_by_role_not_by_name() {
        use crate::discover::LlmKind;
        let c = Catalog::get();
        let klein9 = c.entry.iter().find(|e| e.family == ImageFamily::Flux2Klein9b).unwrap();
        let llm = klein9.file.iter().find(|f| f.slot == "llm").unwrap();
        assert!(klein9.fits(llm, &Role::Llm(LlmKind::Qwen3 { dim: 4096 })));
        // The 4B is the encoder people reach for by mistake.
        assert!(!klein9.fits(llm, &Role::Llm(LlmKind::Qwen3 { dim: 2560 })));
        let model = klein9.file.iter().find(|f| f.slot == "model").unwrap();
        assert!(klein9.fits(model, &Role::Image { family: ImageFamily::Flux2Klein9b, bundled: false }));
        assert!(!klein9.fits(model, &Role::Image { family: ImageFamily::Flux2Klein4b, bundled: false }));
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
