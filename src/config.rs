//! Loads `models.toml` into strongly-typed config.
//!
//! Two ways to get a selectable model:
//!   * **Discovery** — any checkpoint dropped in the models dir is auto-listed
//!     and runs with the `[defaults]` profile. Zero config.
//!   * **Templates** — an explicit `[templates.<name>]` block is a tuned preset
//!     (e.g. Turbo settings) that overrides the defaults for one checkpoint.
//!
//! Both are merged into a single name→[`Profile`] registry the rest of the app
//! uses.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const MODEL_EXTS: &[&str] = &["safetensors", "ckpt", "gguf"];

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub sd: SdConfig,
    #[serde(default)]
    pub server: ServerConfig,
    /// Explicit tuned presets, keyed by name. Optional — discovery covers the
    /// common case.
    #[serde(default)]
    pub templates: BTreeMap<String, Template>,
    /// Generation/FSR settings applied to auto-discovered checkpoints.
    #[serde(default)]
    pub defaults: Defaults,
    /// Directory the config was loaded from; all relative paths resolve against it.
    #[serde(skip)]
    pub base_dir: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SdConfig {
    /// Path to the compiled stable-diffusion.cpp binary (`sd-cli` or `sd`).
    pub binary: String,
    /// Value for the `-M/--mode` flag. Newer sd.cpp: "img_gen"; older: "txt2img".
    #[serde(default = "default_mode")]
    pub mode: String,
    /// Directory scanned for checkpoints. Default "models".
    #[serde(default = "default_models_dir")]
    pub models_dir: String,
}

fn default_mode() -> String {
    "img_gen".to_string()
}
fn default_models_dir() -> String {
    "models".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            host: default_host(),
            port: default_port(),
        }
    }
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}
fn default_port() -> u16 {
    8080
}

/// Tunable generation + FSR settings. Shared shape between `[defaults]` and each
/// `[templates.*]` block (a template just also carries a model + description).
#[derive(Debug, Clone, Deserialize)]
pub struct Defaults {
    #[serde(default = "default_width")]
    pub width: u32,
    #[serde(default = "default_height")]
    pub height: u32,
    #[serde(default = "default_steps")]
    pub steps: u32,
    #[serde(default = "default_cfg")]
    pub cfg_scale: f32,
    #[serde(default = "default_sampler")]
    pub sampler: String,
    #[serde(default = "default_upscale")]
    pub upscale_factor: f32,
    #[serde(default = "default_sharpness")]
    pub sharpness: f32,
    #[serde(default)]
    pub clarity: f32,
    #[serde(default)]
    pub refine_steps: u32,
    #[serde(default = "default_refine_strength")]
    pub refine_strength: f32,
    #[serde(default)]
    pub hires_steps: u32,
    #[serde(default = "default_hires_strength")]
    pub hires_strength: f32,
    #[serde(default = "default_hires_max_side")]
    pub hires_max_side: u32,
    #[serde(default = "default_true")]
    pub hires_flash_attn: bool,
    #[serde(default)]
    pub weight_type: Option<String>,
    #[serde(default)]
    pub extra_args: Vec<String>,
}

impl Default for Defaults {
    fn default() -> Self {
        Defaults {
            width: default_width(),
            height: default_height(),
            steps: default_steps(),
            cfg_scale: default_cfg(),
            sampler: default_sampler(),
            upscale_factor: default_upscale(),
            sharpness: default_sharpness(),
            clarity: 0.0,
            refine_steps: 0,
            refine_strength: default_refine_strength(),
            hires_steps: 0,
            hires_strength: default_hires_strength(),
            hires_max_side: default_hires_max_side(),
            hires_flash_attn: true,
            weight_type: None,
            extra_args: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Template {
    #[serde(default)]
    pub description: Option<String>,
    pub model: String,
    pub width: u32,
    pub height: u32,
    pub steps: u32,
    pub cfg_scale: f32,
    #[serde(default = "default_sampler")]
    pub sampler: String,
    #[serde(default = "default_upscale")]
    pub upscale_factor: f32,
    #[serde(default = "default_sharpness")]
    pub sharpness: f32,
    #[serde(default)]
    pub clarity: f32,
    #[serde(default)]
    pub refine_steps: u32,
    #[serde(default = "default_refine_strength")]
    pub refine_strength: f32,
    #[serde(default)]
    pub hires_steps: u32,
    #[serde(default = "default_hires_strength")]
    pub hires_strength: f32,
    #[serde(default = "default_hires_max_side")]
    pub hires_max_side: u32,
    #[serde(default = "default_true")]
    pub hires_flash_attn: bool,
    #[serde(default)]
    pub weight_type: Option<String>,
    #[serde(default)]
    pub extra_args: Vec<String>,
}

// Defaults tuned for standard (non-distilled) SDXL, since that's the common
// discovered checkpoint. Turbo/LCM models want a template override (few steps,
// cfg 1) — or use the per-request overrides in the UI.
fn default_width() -> u32 {
    768
}
fn default_height() -> u32 {
    768
}
fn default_steps() -> u32 {
    20
}
fn default_cfg() -> f32 {
    5.0
}
fn default_sampler() -> String {
    "euler".to_string()
}
fn default_upscale() -> f32 {
    2.0
}
fn default_sharpness() -> f32 {
    0.35
}
fn default_refine_strength() -> f32 {
    0.30
}
// Same denoise as the base-res refine: enough to resolve real detail at the
// larger size, low enough that the composition doesn't drift.
fn default_hires_strength() -> f32 {
    0.30
}
/// Longest side the hi-res diffusion pass may run at. SDXL's quality falls off
/// past this, and the cost climbs quadratically; anything beyond it is reached
/// with a final (cheap) FSR upscale instead.
fn default_hires_max_side() -> u32 {
    2048
}
fn default_true() -> bool {
    true
}

/// Lowest img2img denoise strength we accept. sd.cpp trims an img2img schedule
/// by strength, so we scale the schedule up by `1 / strength` to compensate
/// (see `SdRunner::img2img`) — a floor keeps that from exploding, and a
/// strength of 0 would make the pass a silent no-op.
pub const MIN_STRENGTH: f32 = 0.05;

/// Highest img2img denoise strength we accept. sd.cpp only trims the schedule
/// `if (strength < 1.f)` (`src/pipeline/image.cpp`), so at exactly 1.0 the trim
/// is skipped and the init image is discarded — the pass degenerates into a
/// plain txt2img, which is never what either escalation button wants.
pub const MAX_STRENGTH: f32 = 0.95;

/// A fully-resolved, ready-to-run model entry (from a template or discovery).
#[derive(Debug, Clone)]
pub struct Profile {
    pub name: String,
    pub description: String,
    pub model: PathBuf,
    pub width: u32,
    pub height: u32,
    pub steps: u32,
    pub cfg_scale: f32,
    pub sampler: String,
    pub upscale_factor: f32,
    pub sharpness: f32,
    /// Local-contrast ("clarity") amount, 0..1. 0.0 = off.
    pub clarity: f32,
    pub refine_steps: u32,
    pub refine_strength: f32,
    pub hires_steps: u32,
    pub hires_strength: f32,
    /// Longest side the hi-res pass may *enlarge* to before diffusing; past it
    /// the pass diffuses as large as it can afford and FSR covers the rest.
    /// Refine is not under this ceiling — it has no such fallback, so gating it
    /// would just remove the option.
    pub hires_max_side: u32,
    pub hires_flash_attn: bool,
    pub weight_type: Option<String>,
    pub extra_args: Vec<String>,
    /// True if this came from a folder scan rather than an explicit template.
    pub discovered: bool,
    /// True if the checkpoint file actually exists on disk.
    pub exists: bool,
}

impl Profile {
    /// Reject denoise strengths that would make a pass a no-op (or blow up the
    /// compensated schedule). Called once per resolved profile at load, and
    /// again in the pipeline after per-request overrides are applied.
    pub fn validate(&self) -> Result<()> {
        for (field, v) in [
            ("refine_strength", self.refine_strength),
            ("hires_strength", self.hires_strength),
        ] {
            if !(MIN_STRENGTH..=MAX_STRENGTH).contains(&v) {
                bail!(
                    "'{}': {field} is {v}, must be between {MIN_STRENGTH} and {MAX_STRENGTH}",
                    self.name
                );
            }
        }
        // Both shaders clamp internally, but an out-of-range value here is a
        // typo worth reporting rather than silently flooring.
        for (field, v) in [("sharpness", self.sharpness), ("clarity", self.clarity)] {
            if !(0.0..=1.0).contains(&v) {
                bail!("'{}': {field} is {v}, must be between 0.0 and 1.0", self.name);
            }
        }
        Ok(())
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let mut cfg: Config =
            toml::from_str(&text).with_context(|| format!("parsing config {}", path.display()))?;
        // Directory containing the config; a relative "models.toml" has an empty
        // parent, so fall back to ".", then make it absolute for clear messages.
        let dir = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => PathBuf::from("."),
        };
        cfg.base_dir = std::fs::canonicalize(&dir).unwrap_or(dir);

        // Catch unusable denoise settings at startup rather than on the first
        // refine, where they'd surface as a pass that silently does nothing.
        for p in cfg.registry().values() {
            p.validate()
                .with_context(|| format!("in {}", path.display()))?;
        }
        Ok(cfg)
    }

    /// Resolve a possibly-relative path against the config's directory.
    pub fn resolve(&self, p: &str) -> PathBuf {
        let pb = PathBuf::from(p);
        if pb.is_absolute() {
            pb
        } else {
            self.base_dir.join(pb)
        }
    }

    pub fn models_dir(&self) -> PathBuf {
        self.resolve(&self.sd.models_dir)
    }

    /// Scan the models dir for checkpoint files (non-recursive), sorted by name.
    pub fn discover_files(&self) -> Vec<PathBuf> {
        let dir = self.models_dir();
        let mut files: Vec<PathBuf> = match std::fs::read_dir(&dir) {
            Ok(rd) => rd
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.is_file())
                .filter(|p| {
                    p.extension()
                        .and_then(|e| e.to_str())
                        .map(|e| MODEL_EXTS.contains(&e.to_ascii_lowercase().as_str()))
                        .unwrap_or(false)
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        files.sort();
        files
    }

    /// Build the name→Profile registry: explicit templates first, then any
    /// discovered checkpoint not already referenced by a template.
    pub fn registry(&self) -> BTreeMap<String, Profile> {
        let mut reg: BTreeMap<String, Profile> = BTreeMap::new();

        for (name, t) in &self.templates {
            let model = self.resolve(&t.model);
            let exists = model.exists();
            reg.insert(
                name.clone(),
                Profile {
                    name: name.clone(),
                    description: t
                        .description
                        .clone()
                        .unwrap_or_else(|| format!("preset for {}", t.model)),
                    exists,
                    model,
                    width: t.width,
                    height: t.height,
                    steps: t.steps,
                    cfg_scale: t.cfg_scale,
                    sampler: t.sampler.clone(),
                    upscale_factor: t.upscale_factor,
                    sharpness: t.sharpness,
                    clarity: t.clarity,
                    refine_steps: t.refine_steps,
                    refine_strength: t.refine_strength,
                    hires_steps: t.hires_steps,
                    hires_strength: t.hires_strength,
                    hires_max_side: t.hires_max_side,
                    hires_flash_attn: t.hires_flash_attn,
                    weight_type: t.weight_type.clone(),
                    extra_args: t.extra_args.clone(),
                    discovered: false,
                },
            );
        }

        // Paths already covered by a template shouldn't be re-listed.
        let covered: std::collections::HashSet<PathBuf> =
            reg.values().map(|p| p.model.clone()).collect();

        let d = &self.defaults;
        for file in self.discover_files() {
            if covered.contains(&file) {
                continue;
            }
            let stem = file
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("model")
                .to_string();
            let name = unique_key(&reg, stem);
            reg.insert(
                name.clone(),
                Profile {
                    description: format!(
                        "discovered: {}",
                        file.file_name().and_then(|n| n.to_str()).unwrap_or("?")
                    ),
                    name,
                    model: file,
                    width: d.width,
                    height: d.height,
                    steps: d.steps,
                    cfg_scale: d.cfg_scale,
                    sampler: d.sampler.clone(),
                    upscale_factor: d.upscale_factor,
                    sharpness: d.sharpness,
                    clarity: d.clarity,
                    refine_steps: d.refine_steps,
                    refine_strength: d.refine_strength,
                    hires_steps: d.hires_steps,
                    hires_strength: d.hires_strength,
                    hires_max_side: d.hires_max_side,
                    hires_flash_attn: d.hires_flash_attn,
                    weight_type: d.weight_type.clone(),
                    extra_args: d.extra_args.clone(),
                    discovered: true,
                    exists: true,
                },
            );
        }

        reg
    }
}

/// Return `base`, or `base-2`, `base-3`, … if already present.
fn unique_key(reg: &BTreeMap<String, Profile>, base: String) -> String {
    if !reg.contains_key(&base) {
        return base;
    }
    let mut n = 2;
    loop {
        let candidate = format!("{base}-{n}");
        if !reg.contains_key(&candidate) {
            return candidate;
        }
        n += 1;
    }
}
