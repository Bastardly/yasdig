//! Loads `models.toml` into strongly-typed config.
//!
//! The *defaults* a model runs with are not in here — they live in
//! `user/settings.toml` ([`crate::settings`]), because that is the file the
//! Settings tab writes. This file is what the program needs in order to start,
//! and is read-only to the app so its comments survive.
//!
//! Two ways to get a selectable model:
//!   * **Discovery** — any checkpoint dropped in the models dir is auto-listed
//!     and runs on the settings alone. Zero config.
//!   * **Templates** — an explicit `[templates.<name>]` block states what is
//!     *different* about one checkpoint (a Turbo model's 4 steps and CFG 1) and
//!     inherits everything it doesn't name from the settings.
//!
//! Both are merged into a single name→[`Profile`] registry the rest of the app
//! uses, through one code path: settings → the template's overlay → `Profile`.

use crate::settings::{PartialSettings, Settings};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub sd: SdConfig,
    #[serde(default)]
    pub server: ServerConfig,
    /// Explicit tuned presets, keyed by name. Optional — discovery covers the
    /// common case.
    #[serde(default)]
    pub templates: BTreeMap<String, Template>,
    /// The old `[defaults]` block, kept for one job only: seeding
    /// `user/settings.toml` on a first run so an install that had tuned it
    /// doesn't quietly revert to the built-ins. Once that file exists this is
    /// ignored entirely, and `preflight` says so rather than leaving the two to
    /// disagree in silence.
    #[serde(default, rename = "defaults")]
    pub legacy_defaults: Option<PartialSettings>,
    /// Video model *overrides*. Most models come from scanning the video dir
    /// (see [`crate::discover`]); an entry here names components that haven't
    /// been downloaded yet, pins measured settings, or answers the one question
    /// a file's header can't — which half of an expert pair it is. An entry
    /// replaces a discovered model rather than patching it; see
    /// [`Config::video_models`].
    #[serde(default)]
    pub video: Vec<VideoModel>,
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
    /// Directory scanned for video model components. Defaults to `video/`
    /// inside [`SdConfig::models_dir`], so moving the models dir moves both.
    ///
    /// The image scan (`Config::scan_images`) walks the rest of `models/` and
    /// leaves this subtree out: a transformer here is the Video tab's, and its
    /// encoders are assembled by `discover::build`, not offered as checkpoints.
    #[serde(default)]
    pub video_dir: Option<String>,
    /// Directory scanned for LoRAs. Defaults to `loras/` inside
    /// [`SdConfig::models_dir`], so moving the models dir moves this too.
    ///
    /// Its own directory, which the image scan leaves out, so the LoRA listing
    /// is the one place an adapter is described. The classifier would refuse a
    /// LoRA as a checkpoint anyway; the exclusion is about which listing
    /// explains the file, not about keeping it out of the select.
    #[serde(default)]
    pub lora_dir: Option<String>,
    /// Directory holding ESRGAN upscaler models. Defaults to `upscalers/` inside
    /// [`SdConfig::models_dir`], for the reason [`SdConfig::lora_dir`] gives.
    #[serde(default)]
    pub upscaler_dir: Option<String>,
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

/// A tuned preset for one checkpoint: which file it is, and what is different
/// about it.
///
/// **It overrides only what it names.** Everything else comes from
/// `user/settings.toml`, which is what makes the Settings tab mean something
/// for a templated model too — and what a preset for a new architecture wants
/// to be, since a zImage or Krea entry has opinions about steps and CFG and
/// none at all about your hi-res strength. [`Settings::merged_with`] is the one
/// place that merge happens.
#[derive(Debug, Clone, Deserialize)]
pub struct Template {
    #[serde(default)]
    pub description: Option<String>,
    /// The one field a template must state, because it is the only thing
    /// discovery cannot work out for itself: which file this preset is *for*.
    pub model: String,
    /// Everything else, all optional. Flattened, so the block stays flat in
    /// TOML and reads exactly as `[defaults]` used to.
    #[serde(flatten)]
    pub over: PartialSettings,
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

/// One LoRA applied to one generation.
///
/// A struct rather than the video side's `"name:1"` string, and the difference
/// is what the two are for: a `[[video]]` entry *pins* its LoRAs in
/// `models.toml`, so a single string a human types is the right shape there.
/// These are picked per picture in the UI and round-trip through a work item's
/// sidecar, so two fields the page can bind to beat one it would have to split
/// on a colon — a colon the name is forbidden to contain anyway, see
/// [`valid_lora_name`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoraRef {
    /// Path relative to [`Config::lora_dir`], **extension included** —
    /// `detail.safetensors`, or `sdxl/detail.safetensors`.
    ///
    /// The extension is in it so resolution is one `join` and one `exists`.
    /// sd.cpp will happily take a bare stem and try four extensions
    /// (`common.cpp:2280`), but then the name in a sidecar no longer identifies
    /// a file, and a `.gguf` quietly standing in for a deleted `.safetensors`
    /// is not something a record of what generated an image should allow.
    pub name: String,
    /// Multiplier. 1.0 is the weight the adapter was trained at.
    pub strength: f32,
}

/// Lowest and highest LoRA multiplier accepted.
///
/// The floor is negative on purpose: subtracting a LoRA is a real technique, and
/// sd.cpp parses the number with `stof` and applies whatever it gets. The
/// ceiling is where an adapter stops adding its subject and starts tearing the
/// image apart — there is no hard limit in the format, so this is a guard
/// against a typo (a stray `10`), not a claim about the math.
pub const LORA_STRENGTH_MIN: f32 = -2.0;
pub const LORA_STRENGTH_MAX: f32 = 2.0;

/// Is this a name [`Config::lora_path`] may join onto the LoRA directory?
///
/// A whitelist, the same principle as `workitems::valid_id` — but a separate
/// function rather than a widened one, because the two guard different
/// alphabets for different reasons. An id is minted by us and so can be digits
/// and `-`; a LoRA name is a *filename the user chose*, so it has to allow `.`,
/// spaces, and `/` for the `sdxl/`, `flux/` subfolders people actually keep
/// these in. Widening `valid_id` to cover this would quietly widen the archive
/// routes with it.
///
/// So the check is structural instead of alphabetic: every path component must
/// be `Component::Normal`, which rejects `..`, a leading `/` and a bare `.` in
/// one test. On top of that, `:` `<` `>` are rejected because sd.cpp's tag regex
/// is `<lora:([^:>]+):([^>]+)>` (`common.cpp:2248`) — a name containing one
/// wouldn't be escaped, it would silently re-cut the tag somewhere else.
///
/// It also guards [`Profile::upscaler`], which is the same kind of thing — a
/// filename the user chose, relative to a models subdirectory — reaching
/// `--upscale-model` rather than a tag. There the `:` `<` `>` rule is stricter
/// than an argv needs, and costs nothing: a second whitelist for the same
/// question would be a second place to get the traversal check wrong.
pub fn valid_lora_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 200 {
        return false;
    }
    if name.contains([':', '<', '>']) {
        return false;
    }
    let p = Path::new(name);
    p.components()
        .all(|c| matches!(c, std::path::Component::Normal(_)))
}

/// One file in the LoRA directory, as `GET /loras` publishes it.
///
/// Unusable entries are listed rather than dropped, for the reason `doctor`
/// greys out a half-downloaded video model instead of hiding it: "my LoRA isn't
/// in the list" is a much worse thing to debug than "my LoRA is in the list and
/// says why it can't be used".
#[derive(Debug, Clone, Serialize)]
pub struct LoraFile {
    /// What a [`LoraRef`] puts in `name`.
    pub name: String,
    /// What the picker shows: the same path without its extension.
    pub label: String,
    pub bytes: u64,
    pub usable: bool,
    /// Empty when `usable`; otherwise why not, in one phrase.
    pub note: String,
    /// The base it was trained against, as [`ImageFamily::as_str`] — the
    /// spelling `/models` uses for a model's `family`, so the page compares two
    /// served strings rather than deciding anything itself. `None` for a base
    /// that is no image family (a Wan or Krea 2 LoRA) *and* for one nothing
    /// identified; `base` tells those apart.
    pub family: Option<&'static str>,
    /// The base as a name — "SDXL", "Krea 2" — or empty when unknown, which is
    /// what makes a LoRA offered for every model. See
    /// [`crate::discover::LoraBase::fits`], the rule this is the wire form of.
    pub base: &'static str,
}

/// One file in the upscaler directory, as `GET /upscalers` publishes it.
///
/// `arch` is read from the file's tensor names by [`crate::sr::identify`] — a
/// safetensors header, or the pickle of a `.pth`, never the weights — so the
/// select can grey out a HAT or a DAT *before* anyone waits for a render to find
/// out. It is not the whole guard: a file can name the right tensors and hold
/// garbage, which is what the size check in `enlarge` and `sr::consistency`
/// are for.
#[derive(Debug, Clone, Serialize)]
pub struct UpscalerFile {
    /// What [`Profile::upscaler`] holds: the path relative to
    /// [`Config::upscaler_dir`], extension included.
    pub name: String,
    /// What the select shows: the same path without its extension.
    pub label: String,
    pub bytes: u64,
    /// `ESRGAN`, `SwinIR-M`, `HAT`, `unknown` … — see [`crate::sr::arch::Arch::name`].
    pub arch: String,
    /// Runnable, by sd-cli (ESRGAN) or by [`crate::sr`] (everything else).
    pub supported: bool,
}

/// A fully-resolved, ready-to-run model entry (from a template or discovery).
///
/// Serializable because a work item stores its profile verbatim (see
/// `src/workitems.rs`): the per-request overrides are already applied by the
/// time `Engine::run` retains it, so the stored copy *is* the record of what
/// generated that image. Every field is `#[serde(default)]` so a sidecar written
/// before a knob was added still loads — a work item that fails to parse is one
/// the user can no longer reopen, which is a worse outcome than a defaulted knob.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
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
    /// What performs that enlarge. `None` is FSR's EASU; a name is an ESRGAN
    /// model under [`Config::upscaler_dir`], extension included — the same
    /// contract as [`LoraRef::name`]. A sidecar written before this existed
    /// reads as `None`, which is what it ran with.
    pub upscaler: Option<String>,
    pub sharpness: f32,
    /// Local-contrast ("clarity") amount, -1..1. 0.0 = off; negative softens.
    pub clarity: f32,
    pub refine_steps: u32,
    pub refine_strength: f32,
    /// CFG for the refine pass alone. `None` follows `cfg_scale`, so raising
    /// the base CFG carries the pass with it — read it through [`Profile::refine_cfg`]
    /// rather than unwrapping it by hand.
    pub refine_cfg_scale: Option<f32>,
    pub hires_steps: u32,
    pub hires_strength: f32,
    /// The same, for the hi-res pass. Two knobs rather than one because the two
    /// passes run at different sizes on different pixels: a hi-res enlarge is
    /// inventing detail at the new resolution and a refine is tightening what is
    /// already there, and the guidance that suits one over-pushes the other.
    pub hires_cfg_scale: Option<f32>,
    /// Which checkpoint the **Refine** button runs, instead of the one that made
    /// the image: draft on a turbo model, finish on a detailed one. `None` is
    /// "the one that made the image", which is what an unchosen select sends.
    ///
    /// **Request-only**, exactly like [`Profile::loras`] — there is no key for
    /// it on `Defaults` or `Template`, so both arms of [`Config::registry`]
    /// build it empty and the `Defaults`/`Template` trap at the top of
    /// `CLAUDE.md` has no second struct to catch anyone with. It is on `Profile`
    /// because the profile is what a `Pending` retains and what a sidecar
    /// stores, and a work item that didn't name its finishing checkpoint
    /// couldn't restore the picker when it was reopened.
    pub refine_model: Option<String>,
    /// The same, for **Hi-res**. Separate from `refine_model` because the whole
    /// point of the pair is that the two passes need not agree.
    pub hires_model: Option<String>,
    /// The strength of the single pass over a *Start from* image — the one
    /// knob of its own it has: it runs `steps` at `cfg_scale`, as the txt2img
    /// would. Not an escalation: nothing gates on it, and `hires_max_side`
    /// doesn't bound it — it bounds an *enlarge*, and this pass never enlarges.
    pub source_strength: f32,
    /// Longest side the hi-res pass may *enlarge* to before diffusing; past it
    /// the pass diffuses as large as it can afford and FSR covers the rest.
    /// Refine is not under this ceiling — it has no such fallback, so gating it
    /// would just remove the option.
    pub hires_max_side: u32,
    pub hires_flash_attn: bool,
    pub weight_type: Option<String>,
    pub extra_args: Vec<String>,
    /// LoRAs this generation runs with. Applied by [`crate::sd::SdRunner`]'s
    /// `base_cmd`, so the refine and hi-res passes over the same image inherit
    /// them — which is the point: a finishing pass that dropped the LoRA would
    /// redraw the surface in the base model's idiom and undo the look.
    ///
    /// **Request-only.** Unlike every other field here there is no `loras` key
    /// on `Defaults` or `Template`, so both arms of [`Config::registry`] build
    /// this empty and it is only ever filled from a `/generate` or `/img2img`
    /// body. That is what keeps this knob clear of the `Defaults`/`Template`
    /// trap at the top of `CLAUDE.md` — there is no second struct to forget.
    /// It is on `Profile` anyway because the profile is what a `Pending`
    /// retains and what a sidecar stores, and an image whose record doesn't
    /// name its LoRAs isn't reproducible.
    pub loras: Vec<LoraRef>,
    /// Which architecture `model` is. A sidecar written before families existed
    /// reads as SDXL, which is what every one of them ran.
    pub family: ImageFamily,
    /// The separate files a bare diffusion model runs with, or `None` for a
    /// bundled checkpoint that carries its own — the difference between
    /// sd.cpp's `-m` and `--diffusion-model` plus a flag per component. `None`
    /// is also what an old sidecar reads as, and it was right: those were all
    /// single-file checkpoints.
    ///
    /// Identity, like `model`: filled by [`Config::registry`] from the scan and
    /// the per-model file, then overridden per request by a part picked in the
    /// *Models* fold (`GenRequest::parts`), and carried whole through
    /// `swap_model`.
    pub components: Option<Components>,
    /// Whether `model` runs as `-m`. Read through [`Profile::is_bundled`],
    /// which is where the fallback for older sidecars lives.
    pub bundled: Option<bool>,
    /// True if this came from a folder scan rather than an explicit template.
    pub discovered: bool,
    /// True if the checkpoint file actually exists on disk.
    pub exists: bool,
}

/// Only reachable through `#[serde(default)]` on the struct above — nothing
/// builds a `Profile` from nothing, since a real one always comes from
/// `Config::registry()`. It exists so a work item's sidecar survives a knob
/// being added.
///
/// The **built-in** settings, deliberately, not the user's: this fills the gaps
/// in a sidecar written before a field existed, and that record has to describe
/// the image it belongs to. Reading `user/settings.toml` here would let a
/// setting changed today rewrite what an old generation claims it ran with.
impl Default for Profile {
    fn default() -> Self {
        Settings::default().as_profile("")
    }
}

/// The files a bare diffusion model is handed beside itself, one per sd.cpp
/// flag. Only what the runnable families use: Z-Image takes `vae` and `llm`,
/// and Flux will take `clip_l` and `t5xxl`.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Components {
    pub vae: Option<PathBuf>,
    pub llm: Option<PathBuf>,
    pub clip_l: Option<PathBuf>,
    pub t5xxl: Option<PathBuf>,
}

impl Components {
    /// The file in slot `key` — a [`PartSlot::key`], which is the field name.
    pub fn get(&self, key: &str) -> Option<&PathBuf> {
        match key {
            "vae" => self.vae.as_ref(),
            "llm" => self.llm.as_ref(),
            "clip_l" => self.clip_l.as_ref(),
            "t5xxl" => self.t5xxl.as_ref(),
            _ => None,
        }
    }

    /// Fill or empty slot `key`. A key that is no slot does nothing — every
    /// caller has it from [`ImageFamily::slots`].
    pub fn set(&mut self, key: &str, path: Option<PathBuf>) {
        let slot = match key {
            "vae" => &mut self.vae,
            "llm" => &mut self.llm,
            "clip_l" => &mut self.clip_l,
            "t5xxl" => &mut self.t5xxl,
            _ => return,
        };
        *slot = path;
    }

    /// Each present component with the sd.cpp flag that loads it.
    pub fn flags(&self) -> Vec<(&'static str, &Path)> {
        [
            ("--vae", &self.vae),
            ("--llm", &self.llm),
            ("--clip_l", &self.clip_l),
            ("--t5xxl", &self.t5xxl),
        ]
        .into_iter()
        .filter_map(|(flag, p)| p.as_deref().map(|p| (flag, p)))
        .collect()
    }
}

/// One separate file a family runs with: what the Image tab's *Models* fold
/// offers a select for, and what `discover::image_components` picks when
/// nobody has.
///
/// One table for both, so the file the scan picks by itself and the files the
/// page offers instead can't disagree about what fits. Whether a slot must be
/// filled is not in here: it is optional exactly when the model is bundled and
/// so carries its own copy — see [`Profile::is_bundled`].
#[derive(Debug)]
pub struct PartSlot {
    /// The [`Components`] field, and the key on the wire and in
    /// `user/model-settings.toml`.
    pub key: &'static str,
    pub label: &'static str,
    /// Whether a discovered file can fill it — the check that keeps a Flux VAE
    /// off an SDXL, which would decode to noise rather than fail.
    pub fits: fn(&crate::discover::Role) -> bool,
    /// Where it conventionally lives under the models dir, named when nothing
    /// is on disk so the missing path is also the instruction.
    pub dir: &'static str,
    pub file: &'static str,
}

impl Profile {
    /// Whether `model` carries its own VAE and text encoder, and so runs as
    /// `-m` — with any [`Components`] then *overriding* its own, which sd.cpp
    /// allows because a tensor loaded later replaces one of the same name
    /// (`model_loader.cpp:171`, the external VAE loading after the checkpoint).
    ///
    /// Stored since a bundled checkpoint can have a VAE picked for it, so
    /// "has components" stopped meaning "is assembled". The fallback reads
    /// every sidecar written before then correctly: no components was a
    /// bundled checkpoint, and some was a Z-Image.
    pub fn is_bundled(&self) -> bool {
        self.bundled.unwrap_or(self.components.is_none())
    }

    /// Whether a slot may be left empty: on a bundled checkpoint, which then
    /// uses its own copy, and on any UNet — every one of those is bundled, and
    /// a bare one was never assembled at all. The one rule both the fold's
    /// "built in" option and [`Config::apply_parts`] follow.
    pub fn parts_optional(&self) -> bool {
        self.is_bundled() || self.family.is_unet()
    }

    /// Put `path` in slot `key`, or empty it. A bundled model left with
    /// nothing picked goes back to `None`, what it had before anyone chose.
    pub fn set_part(&mut self, key: &str, path: Option<PathBuf>) {
        let bundled = self.is_bundled();
        self.bundled = Some(bundled);
        let mut parts = self.components.take().unwrap_or_default();
        parts.set(key, path);
        self.components = if bundled && parts == Components::default() {
            None
        } else {
            Some(parts)
        };
    }

    /// Every file this profile runs with that isn't on disk: the model, and
    /// each component. Empty means runnable.
    ///
    /// A component that was never found is named at its conventional path (see
    /// `discover::image_components`), so this list is also where to put it.
    pub fn missing(&self) -> Vec<PathBuf> {
        let mut files = vec![self.model.as_path()];
        if let Some(c) = &self.components {
            files.extend(c.flags().into_iter().map(|(_, p)| p));
        }
        files
            .into_iter()
            .filter(|p| !p.is_file())
            .map(Path::to_path_buf)
            .collect()
    }

    /// Fail, naming every missing file, before a pass that would need them.
    ///
    /// One sentence for all the callers that used to say "checkpoint not found"
    /// with only the model's path — which was the whole story for a single-file
    /// checkpoint and is half of it for a Z-Image whose text encoder isn't
    /// downloaded yet.
    pub fn require_files(&self) -> Result<()> {
        let missing = self.missing();
        if missing.is_empty() {
            return Ok(());
        }
        let list: Vec<String> = missing.iter().map(|p| format!("  {}", p.display())).collect();
        bail!(
            "'{}' can't run — {} not on disk:\n{}",
            self.name,
            if missing.len() == 1 { "this file is" } else { "these files are" },
            list.join("\n")
        )
    }

    /// The CFG the refine pass runs at: its own knob, or the base CFG.
    ///
    /// Unset follows `cfg_scale` rather than resolving to a number at load, so
    /// raising the base CFG on a discovered model doesn't quietly leave the
    /// escalation behind at whatever it used to be. The single place the
    /// fallback lives — nothing else should unwrap the field itself.
    pub fn refine_cfg(&self) -> f32 {
        self.refine_cfg_scale.unwrap_or(self.cfg_scale)
    }

    /// The CFG the hi-res pass runs at. See [`Profile::refine_cfg`].
    pub fn hires_cfg(&self) -> f32 {
        self.hires_cfg_scale.unwrap_or(self.cfg_scale)
    }

    /// Reject denoise strengths that would make a pass a no-op (or blow up the
    /// compensated schedule). Called once per resolved profile at load, and
    /// again in the pipeline after per-request overrides are applied.
    pub fn validate(&self) -> Result<()> {
        for (field, v) in [
            ("refine_strength", self.refine_strength),
            ("hires_strength", self.hires_strength),
            ("source_strength", self.source_strength),
        ] {
            if !(MIN_STRENGTH..=MAX_STRENGTH).contains(&v) {
                bail!(
                    "'{}': {field} is {v}, must be between {MIN_STRENGTH} and {MAX_STRENGTH}",
                    self.name
                );
            }
        }
        // 0.0 is meaningful here — it's guidance off — so the floor is 0 rather
        // than a minimum. Only nonsense is rejected. `cfg_scale` goes through the
        // same check: it was unvalidated, so a negative one from the UI reached
        // sd.cpp untouched.
        for (field, v) in [
            ("cfg_scale", self.cfg_scale),
            ("refine_cfg_scale", self.refine_cfg()),
            ("hires_cfg_scale", self.hires_cfg()),
        ] {
            if !v.is_finite() || v < 0.0 {
                bail!("'{}': {field} is {v}, must be 0 or greater", self.name);
            }
        }
        // The name guard is here as well as at the route because this profile is
        // about to become a command line: a name that re-cuts sd.cpp's tag regex
        // would not fail, it would apply some *other* file.
        for l in &self.loras {
            if !valid_lora_name(&l.name) {
                bail!("'{}': '{}' is not a LoRA name", self.name, l.name);
            }
            if !l.strength.is_finite()
                || !(LORA_STRENGTH_MIN..=LORA_STRENGTH_MAX).contains(&l.strength)
            {
                bail!(
                    "'{}': LoRA '{}' has strength {}, must be between \
                     {LORA_STRENGTH_MIN} and {LORA_STRENGTH_MAX}",
                    self.name,
                    l.name,
                    l.strength
                );
            }
        }
        // Same reason as the LoRA names: this is one step from being a path
        // handed to sd-cli.
        if let Some(u) = &self.upscaler {
            if !valid_lora_name(u) {
                bail!("'{}': '{u}' is not an upscaler file name", self.name);
            }
        }
        Filters::starting_at(self.sharpness, self.clarity).validate(&self.name)
    }
}

/// Every display filter on one image, as one value.
///
/// Ten knobs travel together through `publish`, `Pending`, `/adjust` and the
/// result JSON, so they are one struct rather than ten parameters: adding the
/// eleventh is a field here, a row in [`Filters::TABLE`], a line in `grade.wgsl`
/// and a row in the page's `FILTERS` list — not a signature edited in six files.
///
/// **Only the first two are per-model.** `sharpness` and `clarity` are on
/// `Defaults`/`Template` because a checkpoint has a house look for them; the
/// eight grading knobs always start neutral, because no checkpoint has an
/// opinion about your hue. That is also why this struct isn't the one the config
/// deserializes — see [`Profile`].
///
/// Neutral is all-zero, which is what `Default` derives and what every
/// `Fsr::process` call that isn't `Engine::publish` passes ([`Filters::NONE`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Filters {
    /// RCAS amount, 0..1. Unsigned: there is no meaningful direction to run a
    /// sharpen backwards in.
    pub sharpness: f32,
    /// Difference-of-Gaussians local contrast, -1..1. Negative softens the band.
    pub clarity: f32,
    // The eight below are the grade pass: per-pixel, -1..1, 0 = identity, and
    // all of them are one dispatch together whatever they are set to.
    /// Perceptual lightness, ±1.5 stops at the ends.
    pub lightness: f32,
    /// Tone curve slope about mid-grey.
    pub contrast: f32,
    /// Lift or recover the top of the range only.
    pub highlights: f32,
    /// Open or crush the bottom of the range only.
    pub shadows: f32,
    /// Chroma scale. -1 is exactly greyscale, +1 is double.
    pub saturation: f32,
    /// Hue rotation, -1..1 = ±180°.
    pub hue: f32,
    /// Blue↔yellow shift.
    pub temperature: f32,
    /// Green↔magenta shift.
    pub tint: f32,
}

impl Filters {
    /// Every filter off. `Fsr::process` with this is a byte-exact identity at
    /// scale 1.0 — all three optional passes are skipped — which
    /// `fsr-selftest` asserts.
    pub const NONE: Filters = Filters {
        sharpness: 0.0,
        clarity: 0.0,
        lightness: 0.0,
        contrast: 0.0,
        highlights: 0.0,
        shadows: 0.0,
        saturation: 0.0,
        hue: 0.0,
        temperature: 0.0,
        tint: 0.0,
    };

    /// Where a fresh image's sliders start: the model's two display settings,
    /// and no grade. Every new generation, refine, hi-res and reopen goes
    /// through here or through [`Filters::restart`].
    pub fn starting_at(sharpness: f32, clarity: f32) -> Filters {
        Filters {
            sharpness,
            clarity,
            ..Filters::NONE
        }
    }

    /// What this image's filters become when an escalation replaces its pixels:
    /// **detail resets, colour carries.**
    ///
    /// `sharpness` and `clarity` size their band from the image (invariant 6),
    /// so the numbers that looked right are stale the moment a pass changes the
    /// resolution or redraws the detail — and inheriting them hides what the
    /// pass did underneath a filter chosen for the previous picture. A hue
    /// rotation is the same rotation at any size, and losing a grade on every
    /// rung of the ladder is a reason not to press the button.
    ///
    /// Safe under invariant 1 either way: `raw` is never graded, so carrying the
    /// numbers re-filters the unsharpened image rather than compounding onto an
    /// already-graded one.
    pub fn restart(&self, sharpness: f32, clarity: f32) -> Filters {
        Filters {
            sharpness,
            clarity,
            ..*self
        }
    }

    /// Name, value and lower bound of each knob. The upper bound is 1.0 for all
    /// ten. One table so that a new knob is range-checked by existing code.
    fn table(&self) -> [(&'static str, f32, f32); 10] {
        [
            ("sharpness", self.sharpness, 0.0),
            ("clarity", self.clarity, -1.0),
            ("lightness", self.lightness, -1.0),
            ("contrast", self.contrast, -1.0),
            ("highlights", self.highlights, -1.0),
            ("shadows", self.shadows, -1.0),
            ("saturation", self.saturation, -1.0),
            ("hue", self.hue, -1.0),
            ("temperature", self.temperature, -1.0),
            ("tint", self.tint, -1.0),
        ]
    }

    /// Does the grade pass have anything to do? All eight at 0 is an exact
    /// identity, so the pass is skipped and its texture never allocated — which
    /// is what keeps a neutral `process` byte-exact.
    pub fn grades(&self) -> bool {
        self.table()[2..].iter().any(|(_, v, _)| *v != 0.0)
    }

    /// Range-check every knob.
    ///
    /// Not folded into [`Profile::validate`] because `Engine::adjust` checks
    /// values that aren't on a profile at all: the profile carries where the
    /// sliders *start*, and the live positions live on the result they apply to.
    ///
    /// Every shader clamps internally, but an out-of-range value is a typo worth
    /// reporting rather than silently flooring.
    pub fn validate(&self, name: &str) -> Result<()> {
        for (field, v, lo) in self.table() {
            if !(lo..=1.0).contains(&v) {
                bail!("'{name}': {field} is {v}, must be between {lo} and 1.0");
            }
        }
        Ok(())
    }
}

/// A `/adjust` body: every knob optional, absent meaning "leave that one alone".
///
/// The route has always had those semantics and keeps them, so adjusting one
/// filter stays a one-field request — the page sends all ten because it has all
/// ten on screen, but a `curl` shouldn't have to. Mirrors
/// [`crate::pipeline::EscalateRequest::apply`].
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(default)]
pub struct FiltersPatch {
    pub sharpness: Option<f32>,
    pub clarity: Option<f32>,
    pub lightness: Option<f32>,
    pub contrast: Option<f32>,
    pub highlights: Option<f32>,
    pub shadows: Option<f32>,
    pub saturation: Option<f32>,
    pub hue: Option<f32>,
    pub temperature: Option<f32>,
    pub tint: Option<f32>,
}

impl FiltersPatch {
    pub fn apply(&self, f: &mut Filters) {
        for (dst, src) in [
            (&mut f.sharpness, self.sharpness),
            (&mut f.clarity, self.clarity),
            (&mut f.lightness, self.lightness),
            (&mut f.contrast, self.contrast),
            (&mut f.highlights, self.highlights),
            (&mut f.shadows, self.shadows),
            (&mut f.saturation, self.saturation),
            (&mut f.hue, self.hue),
            (&mut f.temperature, self.temperature),
            (&mut f.tint, self.tint),
        ] {
            if let Some(v) = src {
                *dst = v;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Image families
// ---------------------------------------------------------------------------

/// Which image architecture a model is, read off its tensor names by
/// [`crate::discover::classify`].
///
/// The image side's counterpart to [`VideoFamily`], and a separate enum on
/// purpose: the two share a classifier but not a recipe — a video family has an
/// fps and a frame grid, an image family has neither. What a family decides is
/// the same kind of thing on both sides, though: which files sd.cpp has to be
/// handed, what the model arrives set to, and which controls on the page mean
/// anything.
///
/// Only the families the classifier can name are here. An architecture sd.cpp
/// knows and this build doesn't list — SD3, Flux 2, Chroma, LTX — is a
/// [`crate::discover::Role::Unsupported`] rather than a variant, so `doctor` can
/// say what it is without the registry having to decide what to do with it.
///
/// `Default` is SDXL because that is what every checkpoint this program ran
/// before it had families was, which is what an old work-item sidecar needs to
/// read as.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, Deserialize)]
pub enum ImageFamily {
    #[serde(rename = "sd1")]
    Sd1,
    #[serde(rename = "sd2")]
    Sd2,
    #[default]
    #[serde(rename = "sdxl")]
    Sdxl,
    /// Flux.1 dev/schnell, and the Krea and Flex finetunes that keep its
    /// tensor layout. CLIP-L + T5-XXL + the 16-channel `ae` VAE.
    #[serde(rename = "flux")]
    Flux,
    /// Z-Image (Tongyi). One Qwen3-4B as the text encoder, and Flux's `ae` VAE.
    #[serde(rename = "z_image")]
    ZImage,
    /// Qwen-Image. Qwen2.5-VL-7B as the text encoder, and its own Wan-shaped
    /// VAE.
    #[serde(rename = "qwen_image")]
    QwenImage,
}

impl ImageFamily {
    /// Every variant, for the tests that have to hold across all of them. The
    /// same tripwire arrangement as [`VideoFamily::ALL`].
    #[cfg(test)]
    pub const ALL: [ImageFamily; 6] = [
        ImageFamily::Sd1,
        ImageFamily::Sd2,
        ImageFamily::Sdxl,
        ImageFamily::Flux,
        ImageFamily::ZImage,
        ImageFamily::QwenImage,
    ];

    /// Can the Image tab run this family yet?
    ///
    /// A family turns true once it has both halves of what a DiT needs: its
    /// components (`discover::image_components` knows which VAE and encoder to
    /// hand it) and a [`ImageFamily::recipe`], because `user/settings.toml`'s
    /// SDXL-tuned steps and CFG are a wrong answer for every DiT here. Until
    /// then it is identified, named by `doctor`, and kept out of the select
    /// rather than offered and then failing minutes in.
    pub fn runnable(self) -> bool {
        match self {
            ImageFamily::Sd1 | ImageFamily::Sd2 | ImageFamily::Sdxl | ImageFamily::ZImage => true,
            ImageFamily::Flux | ImageFamily::QwenImage => false,
        }
    }

    /// The UNet families — the ones every setting in `user/settings.toml` was
    /// tuned for, and so the ones that take it with no recipe laid over it.
    pub fn is_unet(self) -> bool {
        matches!(self, ImageFamily::Sd1 | ImageFamily::Sd2 | ImageFamily::Sdxl)
    }

    /// What a model of this family arrives set to, laid **over** the user's
    /// settings and under a `[templates.*]` entry.
    ///
    /// Empty for the UNet families: the user's settings *are* their recipe —
    /// that is what those numbers were tuned against, and an install that
    /// upgrades into this keeps generating exactly what it did.
    ///
    /// A DiT's recipe names only the knobs that are about the architecture:
    /// size, steps, CFG and sampler. The workflow knobs — strengths, the
    /// escalation step floors, the upscaler, the hi-res budget — stay the
    /// user's. The escalation CFGs are deliberately *not* pinned: unset, they
    /// fall through to this `cfg_scale`, so a turbo model's refine runs at the
    /// CFG 1 it needs, and raising CFG for a base model carries them along.
    ///
    /// Z-Image's is the **turbo**'s, from sd.cpp's `docs/z_image.md` — the
    /// release people actually run. A header can't tell turbo from base (same
    /// tensors, different training), so the base model wants a template: about
    /// 28 steps at CFG 4-5.
    pub fn recipe(self) -> PartialSettings {
        match self {
            ImageFamily::Sd1 | ImageFamily::Sd2 | ImageFamily::Sdxl => PartialSettings::default(),
            ImageFamily::ZImage => PartialSettings {
                width: Some(1024),
                height: Some(1024),
                steps: Some(8),
                cfg_scale: Some(1.0),
                sampler: Some("euler".into()),
                ..PartialSettings::default()
            },
            // Not runnable yet, so nothing reads these; a recipe arrives with
            // the family.
            ImageFamily::Flux | ImageFamily::QwenImage => PartialSettings::default(),
        }
    }

    /// Flags this family always runs with, whatever the settings say.
    ///
    /// Flash attention on every DiT: `hires_flash_attn` only turns it on for
    /// large passes because that is where it paid off on SDXL (122 s → 20 s at
    /// 2048²), but a DiT attends over every token of the image in every block,
    /// so it is the difference at 1024² too — and every example in sd.cpp's
    /// DiT docs passes it. No `--offload-to-cpu`: a Z-Image turbo is 12.3 GB of
    /// DiT and 8 GB of Qwen3, which fits a 24 GB card, while offloading would
    /// hold all 20 GB in system RAM, the thing this box runs out of first.
    pub fn flags(self) -> &'static [&'static str] {
        if self.is_unet() {
            &[]
        } else {
            &["--diffusion-fa"]
        }
    }

    /// The separate files this family runs with, in the order the page shows
    /// them. Adding a family's parts is a row here.
    ///
    /// The UNet families list their VAE only: every checkpoint of theirs is
    /// bundled, so the slot is an override, and the VAE is the one part people
    /// actually swap — SDXL's own overflows in fp16, which is what the
    /// fp16-fix VAE exists for. Flux (VAE, `clip_l`, `t5xxl`) and Qwen-Image
    /// get rows when they become runnable.
    pub fn slots(self) -> &'static [PartSlot] {
        use crate::discover::{ImageVaeKind, LlmKind, Role};
        const SD_VAE: PartSlot = PartSlot {
            key: "vae",
            label: "VAE",
            fits: |r| matches!(r, Role::ImageVae(ImageVaeKind::Sd)),
            dir: "vae",
            file: "sdxl_vae.safetensors",
        };
        const FLUX_VAE: PartSlot = PartSlot {
            key: "vae",
            label: "VAE",
            fits: |r| matches!(r, Role::ImageVae(ImageVaeKind::Flux)),
            dir: "vae",
            file: "ae.safetensors",
        };
        const QWEN3_4B: PartSlot = PartSlot {
            key: "llm",
            label: "Text encoder",
            fits: |r| matches!(r, Role::Llm(LlmKind::Qwen3 { dim: 2560 })),
            dir: "text_encoders",
            file: "qwen_3_4b.safetensors",
        };
        match self {
            ImageFamily::Sd1 | ImageFamily::Sd2 | ImageFamily::Sdxl => &[SD_VAE],
            ImageFamily::ZImage => &[FLUX_VAE, QWEN3_4B],
            ImageFamily::Flux | ImageFamily::QwenImage => &[],
        }
    }

    /// Human-readable name, for `doctor` and the page's model groups.
    pub fn label(self) -> &'static str {
        match self {
            ImageFamily::Sd1 => "SD 1.x",
            ImageFamily::Sd2 => "SD 2.x",
            ImageFamily::Sdxl => "SDXL",
            ImageFamily::Flux => "Flux",
            ImageFamily::ZImage => "Z-Image",
            ImageFamily::QwenImage => "Qwen-Image",
        }
    }

    /// Stable string for the wire and for config, matching the serde spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            ImageFamily::Sd1 => "sd1",
            ImageFamily::Sd2 => "sd2",
            ImageFamily::Sdxl => "sdxl",
            ImageFamily::Flux => "flux",
            ImageFamily::ZImage => "z_image",
            ImageFamily::QwenImage => "qwen_image",
        }
    }
}

// ---------------------------------------------------------------------------
// Video models
// ---------------------------------------------------------------------------

/// Which video architecture a model is.
///
/// The load-bearing field, because it decides three things at once: which flags
/// the command builder emits (the A14B runs a second checkpoint and so needs a
/// whole `--high-noise-*` set, H3 needs `--llm` and `--audio-vae`), which grid
/// the UI has to round to, and whether fps is the user's to choose at all.
///
/// [`crate::discover`] reads it off a transformer's tensor names, and a
/// `[[video]]` entry can state it outright. Keeping it a named value rather than
/// a set of scattered conditionals is what makes a new architecture a few lines
/// rather than a refactor.
///
/// The wire spelling is pinned per variant rather than derived by
/// `rename_all`, which splits `Wan22Ti2v5b` into `wan22_ti2v5b` and would put
/// `models.toml` and [`VideoFamily::as_str`] one letter apart.
/// `family_names_match_their_wire_spelling` keeps the two in step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, Deserialize)]
pub enum VideoFamily {
    /// Wan 2.2 TI2V 5B. One dense checkpoint, and the only Wan that takes
    /// `wan2.2_vae` instead of `wan_2.1_vae` — which is also why its size grid
    /// is twice as coarse as the rest of the family's.
    #[serde(rename = "wan22_ti2v_5b")]
    Wan22Ti2v5b,
    /// Wan 2.2 I2V/T2V A14B: a mixture-of-experts *pair*, high-noise expert
    /// early in the schedule and low-noise late. Only one is resident per step,
    /// so the card isn't the limit — but `--offload-to-cpu` holds both, so
    /// system RAM is.
    #[serde(rename = "wan22_a14b")]
    Wan22A14b,
    /// Wan 2.1 VACE. The frames-in/frames-out family: this is the one that can
    /// refine a finished clip, because it sees every frame at once.
    #[serde(rename = "wan21_vace")]
    Wan21Vace,
    /// Any single dense Wan transformer on the 2.1 VAE — a Wan 2.1 T2V, or one
    /// expert of an A14B run on its own.
    ///
    /// It exists because the last case is real and common: repackagers ship a
    /// single 14B file with no `LowNoise`/`HighNoise` in the name, and that is
    /// the one thing about a mixture-of-experts checkpoint a header cannot
    /// settle — the two halves have identical tensor names and identical
    /// shapes. Without this variant the only honest options would be to guess
    /// which half it is or to refuse to list a perfectly runnable file. sd.cpp
    /// runs a lone expert for the whole schedule quite happily; it is only the
    /// *pair* that needs the `--high-noise-*` flags, and this family doesn't
    /// emit them.
    #[serde(rename = "wan_dense")]
    WanDense,
    /// MiniMax H3. Listed, not runnable here — a 19.9 GB DiT plus a 14.6 GB
    /// Qwen3-VL-32B text encoder at Q4_K_M doesn't fit 24 GB of VRAM or 30 GB
    /// of RAM. It exists as a variant so the day the quantization or the
    /// hardware changes is a `models.toml` edit and nothing more.
    #[serde(rename = "minimax_h3")]
    MinimaxH3,
}

impl VideoFamily {
    /// Every variant, for the tests that have to hold across all of them.
    ///
    /// Rust can't enumerate an enum, so this is a hand-written list and could
    /// fall behind — `all_families_are_listed` is the tripwire, and every
    /// `match` in this impl is exhaustive, so a new variant cannot be added
    /// without at least being *considered* everywhere it matters.
    #[cfg(test)]
    pub const ALL: [VideoFamily; 5] = [
        VideoFamily::Wan22Ti2v5b,
        VideoFamily::Wan22A14b,
        VideoFamily::WanDense,
        VideoFamily::Wan21Vace,
        VideoFamily::MinimaxH3,
    ];

    /// Multiple both dimensions must be. sd.cpp computes it as
    /// `vae_scale_factor * diffusion_model_down_factor` (`align_image_size`,
    /// `src/pipeline/request.cpp`); every video DiT has a down factor of 2, so
    /// this is twice the VAE's spatial factor — 8 for `wan_2.1_vae`, 16 for the
    /// TI2V-5B and H3 VAEs (`get_scale_factor`, `src/model/vae/vae.hpp`).
    pub fn size_multiple(self) -> u32 {
        match self {
            VideoFamily::Wan22Ti2v5b | VideoFamily::MinimaxH3 => 32,
            VideoFamily::Wan22A14b | VideoFamily::Wan21Vace | VideoFamily::WanDense => 16,
        }
    }

    /// Round a dimension the way sd.cpp will. It aligns **up**, warns, and
    /// carries on, so matching it here only means the size on screen is the size
    /// that ran — get the direction wrong and the UI reports a resolution the
    /// clip doesn't have.
    pub fn align_size(self, v: u32) -> u32 {
        let m = self.size_multiple();
        v.max(m).div_ceil(m) * m
    }

    /// Round a frame count the way sd.cpp will — and the two families round in
    /// **opposite directions**, which is the whole reason this isn't one
    /// formula. Wan goes through `((f - 1) / 4) + 1` latent frames and back, and
    /// integer division truncates, so 34 frames comes back 33: it rounds *down*
    /// to `4k+1`. H3 has its own branch, `while (frames % 17 != 5) ++frames`,
    /// which rounds *up* to `17k+5` off a floor of 5. Both in
    /// `align_video_frames`, `src/pipeline/diffusion_engine.cpp`.
    pub fn align_frames(self, frames: u32) -> u32 {
        match self {
            VideoFamily::MinimaxH3 => {
                let f = frames.max(5);
                f + (5 + 17 - f % 17) % 17
            }
            _ => (frames.max(1) - 1) / 4 * 4 + 1,
        }
    }

    /// The frame grid as data, so the page can round exactly the way
    /// [`VideoFamily::align_frames`] does.
    ///
    /// Served rather than re-derived in JavaScript on purpose: the rounded
    /// count decides the duration shown next to the box, and a second
    /// implementation of `4k+1` that disagrees by one frame is precisely how a
    /// UI starts lying about the clip it is about to make.
    /// `the_frame_grid_describes_the_rounding` is what keeps the two honest.
    pub fn frame_grid(self) -> FrameGrid {
        match self {
            VideoFamily::MinimaxH3 => FrameGrid {
                multiple: 17,
                offset: 5,
                min: 5,
                round_up: true,
            },
            _ => FrameGrid {
                multiple: 4,
                offset: 1,
                min: 1,
                round_up: false,
            },
        }
    }

    /// The fps the model will use regardless of what was asked, or `None` when
    /// it's a free choice. sd.cpp overrides and warns rather than failing
    /// (`GenerationRequest`, `src/pipeline/request.cpp`), so a UI that doesn't
    /// know this quietly mislabels the clip's duration.
    pub fn forced_fps(self) -> Option<u32> {
        match self {
            VideoFamily::MinimaxH3 => Some(24),
            _ => None,
        }
    }

    /// True for the mixture-of-experts pair — the only family that loads two
    /// checkpoints, and so the only one with a second set of sampler knobs.
    pub fn is_moe(self) -> bool {
        matches!(self, VideoFamily::Wan22A14b)
    }

    /// The settings a model of this family should arrive with.
    ///
    /// These are the numbers every example for that family in sd.cpp's
    /// `docs/wan.md` uses, and they exist as code because a *discovered* model
    /// has no `models.toml` entry to read them from. The table in `models.toml`
    /// now overrides these rather than restating them, which is the same
    /// relationship `[templates.<name>]` has to `user/settings.toml` on the image
    /// side.
    ///
    /// They are a documented starting point, not a measurement on any
    /// particular card — the thing to change once a run has been timed.
    pub fn recipe(self) -> FamilyRecipe {
        match self {
            // The 5B is trained at 24 fps and is the one family that says so;
            // `forced_fps` stays None for it because sd.cpp doesn't override it,
            // it just performs best there.
            VideoFamily::Wan22Ti2v5b => FamilyRecipe {
                fps: 24,
                steps: 20,
                cfg_scale: 6.0,
                high_noise_steps: None,
                flow_shift: Some(3.0),
            },
            // The pair splits its schedule: 8 steps of high-noise then 10 of
            // low, at the lower CFG the MoE recipe wants.
            VideoFamily::Wan22A14b => FamilyRecipe {
                fps: 16,
                steps: 10,
                cfg_scale: 3.5,
                high_noise_steps: Some(8),
                flow_shift: Some(3.0),
            },
            // A lone Wan transformer wants the full 20-step schedule, since
            // there is no second expert to hand the rest of it to.
            VideoFamily::Wan21Vace | VideoFamily::WanDense => FamilyRecipe {
                fps: 16,
                steps: 20,
                cfg_scale: 6.0,
                high_noise_steps: None,
                flow_shift: Some(3.0),
            },
            VideoFamily::MinimaxH3 => FamilyRecipe {
                fps: 24,
                steps: 20,
                cfg_scale: 6.0,
                high_noise_steps: None,
                flow_shift: None,
            },
        }
    }

    /// Stable string for the UI. Matches the `family` spelling in `models.toml`,
    /// because the page rounds with these rules too and has to name them.
    pub fn as_str(self) -> &'static str {
        match self {
            VideoFamily::Wan22Ti2v5b => "wan22_ti2v_5b",
            VideoFamily::Wan22A14b => "wan22_a14b",
            VideoFamily::Wan21Vace => "wan21_vace",
            VideoFamily::WanDense => "wan_dense",
            VideoFamily::MinimaxH3 => "minimax_h3",
        }
    }
}

/// Do two paths name the same file?
///
/// Canonicalized when both exist, so `models/video/x.gguf` and an absolute path
/// to the same file match and a `[[video]]` entry really does shadow the
/// discovered copy. Falls back to comparing the paths, which is the right answer
/// for a component that hasn't been downloaded yet and so can't be canonicalized
/// at all.
fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// The sampler settings a family runs at by default. See
/// [`VideoFamily::recipe`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FamilyRecipe {
    pub fps: u32,
    pub steps: u32,
    pub cfg_scale: f32,
    pub high_noise_steps: Option<u32>,
    pub flow_shift: Option<f32>,
}

/// Legal frame counts, as `multiple * k + offset` no lower than `min`, rounded
/// in the direction `round_up` names. See [`VideoFamily::frame_grid`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct FrameGrid {
    pub multiple: u32,
    pub offset: u32,
    pub min: u32,
    pub round_up: bool,
}

/// One `[[video]]` entry: a named set of component files plus the settings it
/// runs at.
///
/// There is no settings/`[templates]` split here, and so no second struct to
/// keep in step — the trap described under "Adding a per-model knob" in
/// `CLAUDE.md` doesn't apply to this table. A video model is three to five files
/// whose roles can't be read off a filename, so there is nothing to discover,
/// and nothing to default *for*. Every knob lives here once and
/// [`Config::video_models`] only resolves the paths.
///
/// Serializable because a clip's sidecar stores its model verbatim — the same
/// record [`Profile`] is for a still. `name`, `family`, `diffusion_model` and
/// `vae` are required, since those are what a `models.toml` entry is *for* and a
/// missing one should say so by name; everything else defaults, so a sidecar
/// written before a knob existed still loads.
#[derive(Debug, Clone, serde::Serialize, Deserialize)]
pub struct VideoModel {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub family: VideoFamily,
    /// The (low-noise, for a MoE pair) transformer.
    pub diffusion_model: PathBuf,
    /// The high-noise expert. Required for [`VideoFamily::Wan22A14b`] and
    /// meaningless for anything else; [`VideoModel::validate`] says so.
    #[serde(default)]
    pub high_noise_diffusion_model: Option<PathBuf>,
    pub vae: PathBuf,
    /// umt5 text encoder — every Wan. H3 uses `llm` instead.
    #[serde(default)]
    pub t5xxl: Option<PathBuf>,
    /// H3's Qwen3-VL text encoder (`--llm`).
    #[serde(default)]
    pub llm: Option<PathBuf>,
    /// H3's separate audio VAE (`--audio-vae`).
    #[serde(default)]
    pub audio_vae: Option<PathBuf>,
    /// CLIP-Vision tower, which Wan 2.1 I2V and FLF2V want and Wan 2.2 doesn't.
    #[serde(default)]
    pub clip_vision: Option<PathBuf>,
    #[serde(default = "default_video_width")]
    pub width: u32,
    #[serde(default = "default_video_height")]
    pub height: u32,
    #[serde(default = "default_video_frames")]
    pub frames: u32,
    #[serde(default = "default_video_fps")]
    pub fps: u32,
    #[serde(default = "default_video_steps")]
    pub steps: u32,
    #[serde(default = "default_video_cfg")]
    pub cfg_scale: f32,
    #[serde(default = "default_sampler")]
    pub sampler: String,
    /// Timestep shift for flow-matching models. `None` leaves sd.cpp to pick,
    /// but every example in `docs/wan.md` passes 3.0 — so an entry that means to
    /// follow the documented recipe should say 3.0 rather than say nothing.
    #[serde(default)]
    pub flow_shift: Option<f32>,
    /// Steps for the high-noise expert. `None` hands sd.cpp `-1`, which makes it
    /// split the schedule itself at `moe_boundary` below.
    #[serde(default)]
    pub high_noise_steps: Option<u32>,
    /// CFG for the high-noise expert. `None` follows `cfg_scale`, the same
    /// deliberate indirection as [`Profile::refine_cfg_scale`]: resolving it at
    /// load would quietly leave the expert behind the next time `cfg_scale`
    /// moved.
    #[serde(default)]
    pub high_noise_cfg_scale: Option<f32>,
    /// Timestep where the schedule hands over from the high-noise expert to the
    /// low-noise one. sd.cpp only honours it **when `--high-noise-steps` is -1**
    /// (its own flag help says so), so setting both means this one does nothing.
    #[serde(default)]
    pub moe_boundary: Option<f32>,
    /// VACE conditioning strength, for the frames-in/frames-out refine pass.
    #[serde(default)]
    pub vace_strength: Option<f32>,
    /// Wan's own published negative prompt — see [`default_video_negative`].
    #[serde(default = "default_video_negative")]
    pub negative_prompt: String,
    /// LoRAs to apply, as sd.cpp's inline spelling without the angle brackets:
    /// `"name:1"`, or `"|high_noise|name:1"` to target the high-noise expert.
    /// They are assembled into the prompt by the command builder, so this stays
    /// per-model config instead of something typed into the prompt box.
    #[serde(default)]
    pub loras: Vec<String>,
    /// Directory the names above resolve in (`--lora-model-dir`). Defaults to
    /// the models dir.
    #[serde(default)]
    pub lora_dir: Option<PathBuf>,
    /// `--offload-to-cpu`. Defaults **on**: it is what makes the A14B pair
    /// possible at all on a 24 GB card, and every example in `docs/wan.md`
    /// passes it. Turning it off is a speed-for-memory trade, not a fix.
    #[serde(default = "default_true")]
    pub offload_to_cpu: bool,
    /// `--vae-tiling`. Off by default because it costs time, but it is the
    /// *first* thing to turn on if a clip dies in the decode — Wan's VAEs are
    /// the memory-hungry part of the pipeline, not the transformer.
    #[serde(default)]
    pub vae_tiling: bool,
    /// `--diffusion-fa`. On, like every documented Wan invocation.
    #[serde(default = "default_true")]
    pub flash_attn: bool,
    #[serde(default)]
    pub weight_type: Option<String>,
    /// Escape hatch, same as the image side. The OOM escalations that aren't
    /// worth their own field live here: `["--auto-fit", "off"]`, `["--rng",
    /// "cpu"]`, `["--max-vram", "20"]`.
    #[serde(default)]
    pub extra_args: Vec<String>,
    /// Filled in by [`Config::video_models`]: whether every component file this
    /// entry names is actually on disk. A video model is several files, so
    /// "missing" is the normal state while weights are still downloading, and
    /// the UI has to be able to grey it out rather than fail at run time.
    #[serde(default)]
    pub exists: bool,
    /// True when this came from scanning the video dir rather than from a
    /// `[[video]]` entry. Shown in `doctor` and in the UI, because "why is this
    /// model listed" has two different answers and only one of them is a line
    /// someone can go and edit.
    #[serde(default)]
    pub discovered: bool,
}

// The two the video table shares with the image side's settings. They stayed
// here when the rest moved to `settings.rs` because a `[[video]]` entry has no
// `user/settings.toml` layer under it — there is one struct, so an omitted key
// falls to a built-in rather than to something the user set.
fn default_sampler() -> String {
    "euler".to_string()
}
fn default_true() -> bool {
    true
}

fn default_video_width() -> u32 {
    832
}
fn default_video_height() -> u32 {
    480
}
/// 33 frames is the count every `docs/wan.md` example uses, and it is already on
/// the `4k+1` grid: ~2 s at 16 fps, ~1.4 s at 24.
fn default_video_frames() -> u32 {
    33
}
/// Wan 2.1 and the A14B are trained at 16; the TI2V-5B is trained at 24, so its
/// entry sets its own. There is no family-aware default here on purpose —
/// `forced_fps` is for what the model *overrides*, and guessing what it merely
/// prefers would make two places disagree.
fn default_video_fps() -> u32 {
    16
}
fn default_video_steps() -> u32 {
    20
}
fn default_video_cfg() -> f32 {
    6.0
}

/// Wan's published negative prompt, verbatim from every example in
/// `docs/wan.md`.
///
/// It defaults in code rather than in `models.toml` because it is the model's
/// own recommendation and not an opinion of ours — a 400-character Chinese
/// string is something to inherit, not to retype, and an entry that wants
/// something else can simply say so.
fn default_video_negative() -> String {
    "色调艳丽，过曝，静态，细节模糊不清，字幕，风格，作品，画作，画面，静止，整体发灰，\
     最差质量，低质量，JPEG压缩残留，丑陋的，残缺的，多余的手指，画得不好的手部，\
     画得不好的脸部，畸形的，毁容的，形态畸形的肢体，手指融合，静止不动的画面，\
     杂乱的背景，三条腿，背景人很多，倒着走"
        .to_string()
}

impl VideoModel {
    /// A model built from a file the scanner identified, carrying its family's
    /// [`recipe`](VideoFamily::recipe) and nothing else.
    ///
    /// The components are filled in by the caller, which is the only place that
    /// knows what else was in the folder. `vae` starts as the empty path
    /// deliberately: every family needs one, so a caller that forgets produces a
    /// model that reads as missing a component rather than one that silently
    /// runs without it.
    pub fn discovered(name: String, family: VideoFamily, diffusion_model: PathBuf) -> VideoModel {
        let r = family.recipe();
        VideoModel {
            name,
            description: None,
            family,
            diffusion_model,
            high_noise_diffusion_model: None,
            vae: PathBuf::new(),
            t5xxl: None,
            llm: None,
            audio_vae: None,
            clip_vision: None,
            width: default_video_width(),
            height: default_video_height(),
            frames: default_video_frames(),
            fps: r.fps,
            steps: r.steps,
            cfg_scale: r.cfg_scale,
            sampler: default_sampler(),
            flow_shift: r.flow_shift,
            high_noise_steps: r.high_noise_steps,
            high_noise_cfg_scale: None,
            moe_boundary: None,
            vace_strength: None,
            negative_prompt: default_video_negative(),
            loras: Vec::new(),
            lora_dir: None,
            offload_to_cpu: true,
            vae_tiling: false,
            flash_attn: true,
            weight_type: None,
            extra_args: Vec::new(),
            exists: false,
            discovered: true,
        }
    }

    /// The CFG the high-noise expert runs at. Follows `cfg_scale` when unset —
    /// see [`Profile::refine_cfg`] for why this stays a method rather than
    /// being resolved at load.
    pub fn high_noise_cfg(&self) -> f32 {
        self.high_noise_cfg_scale.unwrap_or(self.cfg_scale)
    }

    /// Size and frame count after rounding to the family's grid — the numbers
    /// that will actually run.
    pub fn aligned(&self) -> (u32, u32, u32) {
        (
            self.family.align_size(self.width),
            self.family.align_size(self.height),
            self.family.align_frames(self.frames),
        )
    }

    /// The fps the clip will have, after any override the family forces.
    pub fn effective_fps(&self) -> u32 {
        self.family.forced_fps().unwrap_or(self.fps.max(1))
    }

    /// Clip length in seconds, from the numbers that will really run.
    pub fn duration(&self) -> f32 {
        let (_, _, frames) = self.aligned();
        frames as f32 / self.effective_fps() as f32
    }

    /// Reject an entry that can't run, at load rather than minutes into a job.
    ///
    /// The family-specific component checks are the point: a missing high-noise
    /// expert makes an A14B entry silently generate with one expert for the
    /// whole schedule, which is not an error sd.cpp reports and not something
    /// the result announces either — it just looks worse than it should.
    pub fn validate(&self) -> Result<()> {
        if self.name.is_empty() {
            bail!("a [[video]] entry has an empty name");
        }
        let n = &self.name;
        if self.family.is_moe() && self.high_noise_diffusion_model.is_none() {
            bail!(
                "video '{n}': family {} is a mixture-of-experts pair and needs \
                 high_noise_diffusion_model as well as diffusion_model",
                self.family.as_str()
            );
        }
        match self.family {
            VideoFamily::MinimaxH3 => {
                if self.llm.is_none() {
                    bail!("video '{n}': family minimax_h3 needs llm (its Qwen3-VL text encoder)");
                }
            }
            _ => {
                if self.t5xxl.is_none() {
                    bail!(
                        "video '{n}': family {} needs t5xxl (the umt5 text encoder)",
                        self.family.as_str()
                    );
                }
            }
        }
        if self.frames == 0 {
            bail!("video '{n}': frames is 0");
        }
        if self.width == 0 || self.height == 0 {
            bail!("video '{n}': width/height is 0");
        }
        if self.fps == 0 {
            bail!("video '{n}': fps is 0");
        }
        for (field, v) in [
            ("cfg_scale", self.cfg_scale),
            ("high_noise_cfg_scale", self.high_noise_cfg()),
        ] {
            if !v.is_finite() || v < 0.0 {
                bail!("video '{n}': {field} is {v}, must be 0 or greater");
            }
        }
        // 0.875 is sd.cpp's own default, and the useful range is the middle of
        // the schedule; 0 or 1 would mean one expert does everything, which is
        // what leaving out the other checkpoint is for.
        if let Some(b) = self.moe_boundary {
            if !(0.0..=1.0).contains(&b) {
                bail!("video '{n}': moe_boundary is {b}, must be between 0 and 1");
            }
        }
        if let Some(s) = self.vace_strength {
            if !(0.0..=1.0).contains(&s) {
                bail!("video '{n}': vace_strength is {s}, must be between 0 and 1");
            }
        }
        Ok(())
    }

    /// Every component file this entry names, for the existence check and for
    /// `doctor`.
    pub fn components(&self) -> Vec<&PathBuf> {
        [
            Some(&self.diffusion_model),
            self.high_noise_diffusion_model.as_ref(),
            Some(&self.vae),
            self.t5xxl.as_ref(),
            self.llm.as_ref(),
            self.audio_vae.as_ref(),
            self.clip_vision.as_ref(),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    /// The components that aren't on disk — what the UI shows instead of a
    /// bare "missing".
    pub fn missing(&self) -> Vec<PathBuf> {
        self.components()
            .into_iter()
            .filter(|p| !p.exists())
            .cloned()
            .collect()
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

        // Before the registry, because the registry reads it. A first run seeds
        // it from the old `[defaults]` block so an install that had tuned that
        // doesn't silently revert — the shipped file set `upscale_factor = 2.0`
        // where the built-in is 1.5.
        Settings::ensure(&cfg.user_dir(), cfg.legacy_defaults.as_ref())?;

        // Catch unusable denoise settings at startup rather than on the first
        // refine, where they'd surface as a pass that silently does nothing.
        for p in cfg.registry().values() {
            p.validate()
                .with_context(|| format!("in {}", path.display()))?;
        }
        // Same for the video table, plus the check discovery makes unnecessary
        // on the image side: two entries with one name would make the UI's
        // select ambiguous and `video_model` return whichever came first.
        let mut seen = std::collections::HashSet::new();
        for m in &cfg.video {
            m.validate()
                .with_context(|| format!("in {}", path.display()))?;
            if !seen.insert(&m.name) {
                bail!(
                    "in {}: two [[video]] entries are both named '{}'",
                    path.display(),
                    m.name
                );
            }
        }
        Ok(cfg)
    }

    /// Resolve a possibly-relative path against the config's directory.
    pub fn resolve(&self, p: &str) -> PathBuf {
        self.resolve_path(Path::new(p))
    }

    pub fn resolve_path(&self, p: &Path) -> PathBuf {
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.base_dir.join(p)
        }
    }

    pub fn models_dir(&self) -> PathBuf {
        self.resolve(&self.sd.models_dir)
    }

    /// Everything that belongs to the person rather than to the install:
    /// `user/settings.toml` today, favourites next.
    ///
    /// Not configurable, and beside `work/` rather than inside it on purpose —
    /// `work/` is scratch this program deletes from (`purge_pngs`, `Delete
    /// all`), and nothing that took a decision to create may live somewhere
    /// with a sweeper pointed at it.
    pub fn user_dir(&self) -> PathBuf {
        self.base_dir.join("user")
    }

    /// The user's settings, read from disk now.
    ///
    /// Read per call rather than held, and every caller must keep doing that:
    /// this `Config` is inside an `Arc` that the GPU worker owns its own clone
    /// of, so a value captured at startup could only be changed by restarting
    /// the server — which is not what a Save button means. A ~1 KB parse
    /// against a multi-second diffusion, the same rule `registry` and the LoRA
    /// scan already follow.
    pub fn settings(&self) -> Settings {
        Settings::load_or_default(&self.user_dir())
    }

    /// Every model file under the models dir that belongs to the image side,
    /// identified by its header — for [`Config::registry`] and `doctor`.
    ///
    /// Recursive, so `checkpoints/`, `diffusion_models/sdxl/` or a flat folder
    /// all work and the layout is the person's to choose. The three directories
    /// with scans of their own are left out: the video dir (whose transformers
    /// are the Video tab's, and whose encoders `discover::build` assembles), and
    /// the LoRA and upscaler dirs, which `list_loras` and `list_upscalers`
    /// already describe.
    pub fn scan_images(&self) -> Vec<crate::discover::Found> {
        crate::discover::scan_excluding(
            &self.models_dir(),
            &[self.video_dir(), self.lora_dir(), self.upscaler_dir()],
        )
    }

    /// Directory holding video model components.
    pub fn video_dir(&self) -> PathBuf {
        match &self.sd.video_dir {
            Some(d) => self.resolve(d),
            None => self.models_dir().join("video"),
        }
    }

    /// Directory holding LoRAs, for both image tabs.
    pub fn lora_dir(&self) -> PathBuf {
        match &self.sd.lora_dir {
            Some(d) => self.resolve(d),
            None => self.models_dir().join("loras"),
        }
    }

    /// Directory holding ESRGAN upscaler models.
    pub fn upscaler_dir(&self) -> PathBuf {
        match &self.sd.upscaler_dir {
            Some(d) => self.resolve(d),
            None => self.models_dir().join("upscalers"),
        }
    }

    /// Every model-shaped file in the upscaler dir, for `GET /upscalers` and
    /// `doctor`, each identified by [`crate::sr::identify`]. Recomputed per
    /// call, like [`Config::list_loras`]: reading a header or a `.pth`'s pickle
    /// is milliseconds.
    ///
    /// Recursive to one level, so an `esrgan/` or `anime/` subfolder works the
    /// way `loras/sdxl/` does, and bounded because nobody nests these deeply.
    pub fn list_upscalers(&self) -> Vec<UpscalerFile> {
        fn walk(root: &Path, dir: &Path, depth: usize, out: &mut Vec<UpscalerFile>) {
            let Ok(rd) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in rd.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if depth < 2 {
                        walk(root, &path, depth + 1, out);
                    }
                    continue;
                }
                let ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_ascii_lowercase())
                    .unwrap_or_default();
                if !matches!(ext.as_str(), "pth" | "pt" | "safetensors" | "gguf") {
                    continue;
                }
                let name = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned();
                // A name the whitelist refuses could never be selected, so it
                // is not offered rather than offered and then rejected.
                if !valid_lora_name(&name) {
                    continue;
                }
                let label = match name.rsplit_once('.') {
                    Some((stem, _)) => stem.to_string(),
                    None => name.clone(),
                };
                let bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
                // A file that can't even be parsed is listed as unknown rather
                // than hidden: it is in the folder, and the select saying so is
                // how its owner finds out.
                let arch = crate::sr::identify(&path).unwrap_or(crate::sr::arch::Arch::Unknown);
                out.push(UpscalerFile {
                    name,
                    label,
                    bytes,
                    supported: arch.supported(),
                    arch: arch.name(),
                });
            }
        }
        let dir = self.upscaler_dir();
        let mut out = Vec::new();
        walk(&dir, &dir, 0, &mut out);
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Resolve the upscaler a profile names, or say what is there instead.
    ///
    /// `None` is FSR and needs nothing on disk. Called before the txt2img, so a
    /// deleted file costs a millisecond rather than a render — the same reason
    /// [`Config::check_loras`] runs first.
    pub fn check_upscaler(&self, name: &Option<String>) -> Result<Option<PathBuf>> {
        let Some(name) = name else {
            return Ok(None);
        };
        if !valid_lora_name(name) {
            bail!("'{name}' is not an upscaler file name");
        }
        let dir = self.upscaler_dir();
        let path = dir.join(name);
        if !path.is_file() {
            let known: Vec<String> =
                self.list_upscalers().into_iter().map(|f| f.name).collect();
            bail!(
                "no upscaler named '{name}' in {} — available: {}",
                dir.display(),
                if known.is_empty() {
                    "nothing (the folder is empty; FSR needs no file)".to_string()
                } else {
                    known.join(", ")
                }
            );
        }
        // Refused here, before the txt2img, for the reason the missing-file
        // case above is: a HAT would otherwise fail after the render it was
        // meant to enlarge.
        let arch = crate::sr::identify(&path)
            .with_context(|| format!("reading upscaler '{name}'"))?;
        if !arch.supported() {
            bail!(
                "upscaler '{name}' is {} — not an architecture this can run. \
                 ESRGAN, Compact, SwinIR and SPAN are",
                match arch {
                    crate::sr::arch::Arch::Unknown => "not a recognised upscaler".to_string(),
                    a => format!("a {}", a.name()),
                }
            );
        }
        Ok(Some(path))
    }

    /// Everything in the LoRA dir, for `GET /loras` and for `doctor`.
    ///
    /// Recursive and header-read, through the same [`crate::discover::scan`] the
    /// video dir uses — so `loras/sdxl/`, `loras/flux/` work, and a checkpoint
    /// dropped in here by mistake is identified as one rather than offered as an
    /// adapter. Recomputed per call, like [`Config::registry`]: a file appearing
    /// in a folder is not worth a restart to notice.
    pub fn list_loras(&self) -> Vec<LoraFile> {
        let dir = self.lora_dir();
        crate::discover::scan(&dir)
            .into_iter()
            .map(|f| {
                let name = f
                    .path
                    .strip_prefix(&dir)
                    .unwrap_or(&f.path)
                    .to_string_lossy()
                    .into_owned();
                let label = match name.rsplit_once('.') {
                    Some((stem, _)) => stem.to_string(),
                    None => name.clone(),
                };
                let bytes = std::fs::metadata(&f.path).map(|m| m.len()).unwrap_or(0);
                let note = if !valid_lora_name(&name) {
                    // Only reachable by a filename containing ':' '<' or '>',
                    // since everything else here came off a directory walk.
                    "the name has a character sd.cpp's <lora:…> tag can't carry".to_string()
                } else {
                    match &f.role {
                        crate::discover::Role::Lora(_) => String::new(),
                        crate::discover::Role::Unknown(why) => why.clone(),
                        other => format!("looks like a {}, not a LoRA", other.describe()),
                    }
                };
                let base = match f.role {
                    crate::discover::Role::Lora(b) => b,
                    _ => crate::discover::LoraBase::Unknown,
                };
                LoraFile {
                    name,
                    label,
                    bytes,
                    usable: note.is_empty(),
                    note,
                    family: match base {
                        crate::discover::LoraBase::Image(f) => Some(f.as_str()),
                        _ => None,
                    },
                    base: base.label(),
                }
            })
            .collect()
    }

    /// Resolve every LoRA a profile names, or say which one is missing.
    ///
    /// This exists because of what sd.cpp does without it: a `<lora:…>` tag
    /// whose file it can't find is a `LOG_WARN` and a *dropped tag*
    /// (`examples/common/common.cpp:2289`). The render then succeeds, takes the
    /// usual two minutes, and comes back as a picture that simply doesn't have
    /// the LoRA in it — with nothing on the wire saying so. Failing here instead
    /// costs a millisecond and turns a silent wrong answer into a message that
    /// names what is actually on disk.
    ///
    /// Called from every entry point that is about to hand a profile to
    /// `SdRunner`, escalations included: a file deleted between the generate and
    /// the Refine is the same failure arriving later.
    ///
    /// It also refuses a LoRA trained for another family, which sd.cpp takes
    /// just as quietly: the tensor names don't match, the misses are a log line,
    /// and the render comes back without the LoRA in it. The picker greys those
    /// out, so this is for what reaches a run another way — a reopened work
    /// item, a hand-written request.
    pub fn check_loras(&self, refs: &[LoraRef], family: ImageFamily) -> Result<()> {
        if refs.is_empty() {
            return Ok(());
        }
        let dir = self.lora_dir();
        for l in refs {
            if !valid_lora_name(&l.name) {
                bail!("'{}' is not a LoRA name", l.name);
            }
            if !dir.join(&l.name).exists() {
                let known: Vec<String> = self
                    .list_loras()
                    .into_iter()
                    .filter(|f| f.usable)
                    .map(|f| f.name)
                    .collect();
                bail!(
                    "no LoRA named '{}' in {} — available: {}",
                    l.name,
                    dir.display(),
                    if known.is_empty() {
                        "nothing (the folder is empty)".to_string()
                    } else {
                        known.join(", ")
                    }
                );
            }
            match crate::discover::identify(&dir.join(&l.name)) {
                crate::discover::Role::Lora(base) if !base.fits(family) => bail!(
                    "'{}' is a LoRA for {}, and this is a {} model — its weights would \
                     not match, and sd.cpp would render without it",
                    l.name,
                    base.label(),
                    family.label()
                ),
                crate::discover::Role::Lora(_) => {}
                other => bail!("'{}' looks like a {}, not a LoRA", l.name, other.describe()),
            }
        }
        Ok(())
    }

    /// Identify everything in the video dir. See [`crate::discover`].
    pub fn discover_video(&self) -> crate::discover::Discovery {
        let dir = self.video_dir();
        let found = crate::discover::scan(&dir);
        crate::discover::build(&dir, &found)
    }

    /// Every video model: the ones found by scanning the video dir, plus the
    /// `[[video]]` table, with paths resolved and `exists` filled in.
    ///
    /// **An explicit entry replaces a discovered one; it does not patch it.** A
    /// `[[video]]` entry matches by `name`, or by naming the same
    /// `diffusion_model` file. Replacing wholesale is what keeps `VideoModel` a
    /// single struct: a field-by-field merge would need every knob to be an
    /// `Option` so "unset" could be told from "set to the same value as the
    /// default", which is exactly the `Defaults`/`Template` trap that
    /// `CLAUDE.md` warns about on the image side. An entry that wants to change
    /// one number still only writes that one line — the rest comes from
    /// [`VideoFamily::recipe`], not from the discovered model.
    ///
    /// Recomputed per call rather than cached, like [`Config::registry`]. A
    /// video model is 20-30 GB across several files, so "half of it is still
    /// downloading" is a normal state for it to be in, and a restart to notice
    /// the rest arrived would be a poor way to find that out. The scan costs one
    /// header read per file, not a hash of 20 GB.
    pub fn video_models(&self) -> Vec<VideoModel> {
        let explicit: Vec<VideoModel> = self.video.iter().map(|m| self.resolve_model(m)).collect();
        let mut out = explicit.clone();
        for m in self.discover_video().models {
            let shadowed = explicit
                .iter()
                .any(|e| e.name == m.name || same_file(&e.diffusion_model, &m.diffusion_model));
            if !shadowed {
                out.push(self.resolve_model(&m));
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Resolve one entry's component paths and check they're on disk.
    fn resolve_model(&self, m: &VideoModel) -> VideoModel {
        let mut m = m.clone();
        let resolve = |p: &Path| self.resolve_path(p);
        m.diffusion_model = resolve(&m.diffusion_model);
        m.vae = resolve(&m.vae);
        for p in [
            &mut m.high_noise_diffusion_model,
            &mut m.t5xxl,
            &mut m.llm,
            &mut m.audio_vae,
            &mut m.clip_vision,
            &mut m.lora_dir,
        ]
        .into_iter()
        .flatten()
        {
            *p = resolve(p);
        }
        m.exists = m.missing().is_empty();
        m
    }

    /// Look up one video model by name, resolved. The error names what is
    /// available, since the usual cause is a stale name in a saved clip.
    pub fn video_model(&self, name: &str) -> Result<VideoModel> {
        let models = self.video_models();
        if let Some(m) = models.iter().find(|m| m.name == name) {
            return Ok(m.clone());
        }
        let known: Vec<&str> = models.iter().map(|m| m.name.as_str()).collect();
        if known.is_empty() {
            bail!(
                "no video model '{name}' — nothing in {} was recognised as one, and models.toml \
                 has no [[video]] entries. `doctor` lists what was found there.",
                self.video_dir().display()
            );
        }
        bail!("no video model '{name}' — available: {}", known.join(", "))
    }

    /// Build the name→Profile registry: explicit templates first, then any
    /// discovered checkpoint not already referenced by a template, each with
    /// the user's per-model overrides laid on last.
    pub fn registry(&self) -> BTreeMap<String, Profile> {
        let tuned = crate::settings::ModelSettings::load_or_default(&self.user_dir());
        // Scanned again after `entries` did, which costs a directory walk: the
        // headers are memoized by `discover::identify`.
        let scan = self.scan_images();
        self.entries()
            .into_iter()
            .map(|e| {
                let settings = match tuned.model.get(&e.name) {
                    Some(over) => e.base.merged_with(over),
                    None => e.base,
                };
                let mut p = Profile {
                    description: e.description,
                    model: e.model,
                    discovered: e.discovered,
                    family: e.family,
                    components: e.components,
                    bundled: Some(e.bundled),
                    ..settings.as_profile(&e.name)
                };
                // The parts somebody picked for this model. One that no longer
                // resolves — deleted, or hand-edited into something that
                // doesn't fit — is kept as a path when the file is simply gone,
                // so the model greys out naming it, and dropped otherwise;
                // `doctor` says which.
                for (key, name) in tuned.parts.get(&e.name).into_iter().flatten() {
                    if let Some(path) = self.saved_part(p.family, key, name, &scan) {
                        p.set_part(key, Some(path));
                    }
                }
                // Read off the disk rather than passed in, because a discovered
                // file exists by definition and a templated one may name a
                // checkpoint that was never downloaded — and a DiT may be here
                // without its text encoder. Both are greyed out in the UI rather
                // than hidden, the rule `doctor` follows for a half-downloaded
                // video model.
                p.exists = p.missing().is_empty();
                (e.name, p)
            })
            .collect()
    }

    /// A part named relative to the models dir, as the scan found it, if it
    /// fills `key` on a `family` model — or an error listing what would.
    ///
    /// **Membership in the scan is the whitelist**: the name is compared with
    /// what a directory walk produced, so `..` or an absolute path can't name
    /// anything that isn't already listed, and the role check is what refuses
    /// a Flux VAE on an SDXL. Stronger than [`valid_lora_name`], which only
    /// knows about paths.
    pub fn resolve_part(
        &self,
        family: ImageFamily,
        key: &str,
        name: &str,
        scan: &[crate::discover::Found],
    ) -> Result<PathBuf> {
        let Some(slot) = family.slots().iter().find(|s| s.key == key) else {
            bail!("a {} model has no '{key}' to choose", family.label());
        };
        let path = self.models_dir().join(name);
        match scan.iter().find(|f| f.path == path) {
            Some(f) if (slot.fits)(&f.role) => Ok(path),
            Some(f) => bail!(
                "'{name}' is a {}, not a {} for a {} model",
                f.role.describe(),
                slot.label,
                family.label()
            ),
            None => {
                let fitting: Vec<String> = self.part_options(slot, scan).into_iter().map(|o| o.name).collect();
                bail!(
                    "no {} named '{name}' under {} — available: {}",
                    slot.label,
                    self.models_dir().display(),
                    if fitting.is_empty() { "none".to_string() } else { fitting.join(", ") }
                )
            }
        }
    }

    /// Lay a request's part choices onto a profile. See [`GenRequest::parts`].
    ///
    /// [`GenRequest::parts`]: crate::pipeline::GenRequest::parts
    pub fn apply_parts(&self, p: &mut Profile, parts: &BTreeMap<String, Option<String>>) -> Result<()> {
        let scan = self.scan_images();
        for (key, name) in parts {
            match name {
                Some(name) => {
                    let path = self.resolve_part(p.family, key, name, &scan)?;
                    p.set_part(key, Some(path));
                }
                None if p.parts_optional() => p.set_part(key, None),
                None => {
                    let label = p.family.slots().iter().find(|s| s.key == key).map_or(key.as_str(), |s| s.label);
                    bail!("a {} model has no {label} of its own — pick one", p.family.label());
                }
            }
        }
        Ok(())
    }

    /// Fold a `POST /model-settings` body's `parts` into one model's saved
    /// choices: a name picks, `null` forgets.
    ///
    /// **A choice equal to what the scan picks anyway is not stored**, the rule
    /// [`crate::settings::update_model`] has for numbers and for its reason:
    /// the Image tab's *Save as defaults* sends every slot, and pinning the
    /// automatic pick would stop the model following a better file dropped in
    /// later. On a bundled checkpoint that pick is "its own", so any VAE named
    /// is a real choice.
    pub fn update_parts(
        &self,
        model: &str,
        saved: &BTreeMap<String, String>,
        values: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<BTreeMap<String, String>> {
        let e = self
            .entries()
            .into_iter()
            .find(|e| e.name == model)
            .with_context(|| format!("no model named '{model}'"))?;
        let scan = self.scan_images();
        let mut out = saved.clone();
        for (key, v) in values {
            match v {
                serde_json::Value::Null => {
                    out.remove(key);
                }
                serde_json::Value::String(name) => {
                    let path = self.resolve_part(e.family, key, name, &scan)?;
                    let auto = e.components.as_ref().and_then(|c| c.get(key));
                    if auto == Some(&path) {
                        out.remove(key);
                    } else {
                        out.insert(key.clone(), name.clone());
                    }
                }
                other => bail!("'{key}' should name a file, not {other}"),
            }
        }
        Ok(out)
    }

    /// A part from `user/model-settings.toml`: resolved like a request's, or
    /// the bare path when the file is missing, so the model lists what it is
    /// waiting for. `None` for a name that exists and doesn't fit.
    fn saved_part(
        &self,
        family: ImageFamily,
        key: &str,
        name: &str,
        scan: &[crate::discover::Found],
    ) -> Option<PathBuf> {
        if let Ok(path) = self.resolve_part(family, key, name, scan) {
            return Some(path);
        }
        let path = self.models_dir().join(name);
        let known = family.slots().iter().any(|s| s.key == key);
        (known && valid_lora_name(name) && !path.exists()).then_some(path)
    }

    /// What the page names a part by: its path under the models dir, or the
    /// whole path for one outside it (a template may point anywhere).
    pub fn part_name(&self, path: &Path) -> String {
        path.strip_prefix(self.models_dir())
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned()
    }

    /// Every file on disk that fits `slot`, for its select.
    pub fn part_options(&self, slot: &PartSlot, scan: &[crate::discover::Found]) -> Vec<PartOption> {
        let mut out: Vec<PartOption> = scan
            .iter()
            .filter(|f| (slot.fits)(&f.role))
            .map(|f| PartOption { name: self.part_name(&f.path), path: f.path.clone() })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// A profile's slots as `/models` publishes them: what each holds now,
    /// what the scan would hold with nothing picked, and what else fits.
    pub fn part_views(&self, p: &Profile, scan: &[crate::discover::Found]) -> Vec<PartView> {
        let auto = crate::discover::image_components(p.family, p.is_bundled(), scan, &self.models_dir());
        p.family
            .slots()
            .iter()
            .map(|slot| {
                let name = |c: Option<&Components>| c.and_then(|c| c.get(slot.key)).map(|x| self.part_name(x));
                PartView {
                    slot: slot.key,
                    label: slot.label,
                    optional: p.parts_optional(),
                    current: name(p.components.as_ref()),
                    current_path: p.components.as_ref().and_then(|c| c.get(slot.key)).cloned(),
                    auto: name(auto.as_ref()),
                    options: self.part_options(slot, scan),
                }
            })
            .collect()
    }

    /// What one model starts at *without* the user's per-model overrides — the
    /// thing [`crate::settings::update_model`] compares against, so only a
    /// real difference is stored. `None` for a name that isn't a model.
    pub fn model_base(&self, name: &str) -> Option<Settings> {
        self.entries().into_iter().find(|e| e.name == name).map(|e| e.base)
    }

    /// Every model, resolved up to but not including the per-model layer.
    ///
    /// A discovered checkpoint is not a second kind of model, it is a template
    /// that states only where the file is — which is why both arms go through
    /// one `entry()` and not an arm each. Two arms is how the
    /// `[defaults]`/`[templates]` pair used to drift.
    pub fn entries(&self) -> Vec<ModelEntry> {
        let settings = self.settings();
        // One scan serves both arms: a template's file is identified from it
        // too, and so are the VAE and encoder a bare DiT is assembled with.
        let scan = self.scan_images();
        let mut out: Vec<ModelEntry> = Vec::new();

        for (name, t) in &self.templates {
            let model = self.resolve(&t.model);
            let description = t
                .description
                .clone()
                .unwrap_or_else(|| format!("preset for {}", t.model));
            out.push(self.entry(&settings, &scan, &t.over, name.clone(), description, model, false));
        }

        // Paths already covered by a template shouldn't be re-listed.
        let covered: std::collections::HashSet<PathBuf> =
            out.iter().map(|e| e.model.clone()).collect();

        // **Listed by what the file is, not by its extension.** This used to be
        // every `.safetensors`/`.ckpt`/`.gguf` in the models root, which offered
        // an LTX-2 video transformer as an SDXL checkpoint that then failed at
        // load. A file that isn't a runnable image family is left out and named
        // by `doctor` instead — see [`ImageFamily::runnable`].
        let empty = PartialSettings::default();
        for f in &scan {
            let crate::discover::Role::Image { family, .. } = f.role else {
                continue;
            };
            if !family.runnable() || covered.contains(&f.path) {
                continue;
            }
            let stem = f
                .path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("model")
                .to_string();
            let name = unique_key(&out, stem);
            let description = format!(
                "discovered: {}",
                f.path.file_name().and_then(|n| n.to_str()).unwrap_or("?")
            );
            out.push(self.entry(&settings, &scan, &empty, name, description, f.path.clone(), true));
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// One model up to its template: the settings, its family's recipe, the
    /// template's overlay, and the things that are the model's own identity.
    ///
    /// The layers, each one [`Settings::merged_with`]: the user's settings,
    /// then [`ImageFamily::recipe`] (empty for the UNet families, whose recipe
    /// the settings already are), then the template. A recipe sits *over* the
    /// settings because what it names — size, steps, CFG, sampler — is about
    /// the architecture, and an SDXL-tuned 25 steps at CFG 6 is a wrong answer
    /// for a turbo DiT; a template sits over both because it is somebody's
    /// decision about one file. [`Config::registry`] adds the last layer.
    #[allow(clippy::too_many_arguments)]
    fn entry(
        &self,
        settings: &Settings,
        scan: &[crate::discover::Found],
        over: &PartialSettings,
        name: String,
        description: String,
        model: PathBuf,
        discovered: bool,
    ) -> ModelEntry {
        // What the file is: from the scan when it is under the models dir, read
        // directly when a template names one elsewhere, and SDXL-and-bundled —
        // what every model was before families — when it isn't on disk at all.
        let role = match scan.iter().find(|f| f.path == model) {
            Some(f) => f.role.clone(),
            None if model.is_file() => crate::discover::identify(&model),
            None => crate::discover::Role::Unknown(String::new()),
        };
        let (family, bundled) = match role {
            crate::discover::Role::Image { family, bundled } => (family, bundled),
            _ => (ImageFamily::default(), true),
        };
        let components =
            crate::discover::image_components(family, bundled, scan, &self.models_dir());
        ModelEntry {
            base: settings.merged_with(&family.recipe()).merged_with(over),
            name,
            description,
            model,
            discovered,
            family,
            bundled,
            components,
        }
    }
}

/// One model as [`Config::entries`] resolves it: its identity, and the
/// settings it starts at before the user's per-model overrides.
#[derive(Debug, Clone)]
pub struct ModelEntry {
    pub name: String,
    pub description: String,
    pub model: PathBuf,
    pub discovered: bool,
    pub family: ImageFamily,
    pub bundled: bool,
    pub components: Option<Components>,
    pub base: Settings,
}

/// A file that can fill a slot: `name` is what a request and
/// `user/model-settings.toml` carry, `path` is what a work item's sidecar
/// records, so a reopened image can find its option again.
#[derive(Debug, Clone, Serialize)]
pub struct PartOption {
    pub name: String,
    pub path: PathBuf,
}

/// One slot of one model, for the *Models* fold. `optional` is whether it may
/// be left empty — true for a bundled checkpoint, whose own copy is then used,
/// and for every UNet family.
/// `auto` is what the scan would pick with nothing chosen (`None` on a bundled
/// model: its own), so the page can say what "no choice" means.
#[derive(Debug, Clone, Serialize)]
pub struct PartView {
    pub slot: &'static str,
    pub label: &'static str,
    pub optional: bool,
    pub current: Option<String>,
    pub current_path: Option<PathBuf>,
    pub auto: Option<String>,
    pub options: Vec<PartOption>,
}

/// Return `base`, or `base-2`, `base-3`, … if already present.
fn unique_key(taken: &[ModelEntry], base: String) -> String {
    let used = |n: &str| taken.iter().any(|e| e.name == n);
    if !used(&base) {
        return base;
    }
    let mut n = 2;
    loop {
        let candidate = format!("{base}-{n}");
        if !used(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(toml_src: &str) -> Result<Config> {
        let mut cfg: Config = toml::from_str(toml_src)?;
        cfg.base_dir = PathBuf::from("/tmp");
        for m in &cfg.video {
            m.validate()?;
        }
        Ok(cfg)
    }

    const MINIMAL: &str = r#"
[sd]
binary = "sd-cli"

[[video]]
name = "5b"
family = "wan22_ti2v_5b"
diffusion_model = "v/ti2v.safetensors"
vae = "v/wan2.2_vae.safetensors"
t5xxl = "v/umt5.gguf"
"#;

    /// Writes a safetensors file holding nothing but a header, which is all
    /// discovery reads. Enough to be identified; 200 bytes on disk.
    fn fake_model(path: &std::path::Path, tensors: &[(&str, &str)]) {
        let body: Vec<String> = tensors
            .iter()
            .map(|(n, shape)| {
                format!(r#""{n}":{{"dtype":"F16","shape":{shape},"data_offsets":[0,2]}}"#)
            })
            .collect();
        let json = format!("{{{}}}", body.join(","));
        let mut buf = (json.len() as u64).to_le_bytes().to_vec();
        buf.extend_from_slice(json.as_bytes());
        buf.extend_from_slice(&[0u8; 2]);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, buf).unwrap();
    }

    /// A folder of components becomes a model with no `[[video]]` entry at all,
    /// and an entry that names the same transformer replaces it rather than
    /// producing a second listing.
    #[test]
    fn discovery_finds_a_model_and_an_entry_shadows_it() {
        // Distinct from the other temp dirs in this file: `cargo test` runs the
        // suite in one process, so a pid alone is not a unique name.
        let root =
            std::env::temp_dir().join(format!("rustimage-cfg-discover-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let video = root.join("models").join("video");
        let dit = video.join("diffusion_models").join("wan5b.safetensors");
        fake_model(
            &dit,
            &[
                ("blocks.0.cross_attn.norm_k.weight", "[3072]"),
                ("blocks.0.self_attn.q.weight", "[3072,3072]"),
            ],
        );
        fake_model(
            &video.join("vae").join("wan2.2_vae.safetensors"),
            &[
                ("decoder.conv1.weight", "[160]"),
                ("conv2.weight", "[48,48,1,1,1]"),
            ],
        );
        fake_model(
            &video.join("text_encoders").join("umt5.safetensors"),
            &[(
                "encoder.block.0.layer.0.SelfAttention.q.weight",
                "[4096,4096]",
            )],
        );

        // No [[video]] table whatsoever.
        let bare = "[sd]\nbinary = \"sd-cli\"\n";
        std::fs::write(root.join("models.toml"), bare).unwrap();
        let cfg = Config::load(&root.join("models.toml")).unwrap();
        let found = cfg.video_models();
        assert_eq!(found.len(), 1, "one transformer, one model");
        assert_eq!(found[0].family, VideoFamily::Wan22Ti2v5b);
        assert_eq!(found[0].name, "wan5b");
        assert!(found[0].discovered);
        // The shared components were attached without anyone naming them.
        assert!(found[0].exists, "missing: {:?}", found[0].missing());
        // The family recipe supplied the settings a [[video]] entry would have.
        assert_eq!((found[0].fps, found[0].steps), (24, 20));

        // Now an entry naming the same file. One model, tuned, not discovered.
        let with_entry = format!(
            "{bare}\n[[video]]\nname = \"tuned\"\nfamily = \"wan22_ti2v_5b\"\n\
             diffusion_model = \"{}\"\nvae = \"{}\"\nt5xxl = \"{}\"\nsteps = 4\n",
            dit.display(),
            video.join("vae").join("wan2.2_vae.safetensors").display(),
            video
                .join("text_encoders")
                .join("umt5.safetensors")
                .display(),
        );
        std::fs::write(root.join("models.toml"), with_entry).unwrap();
        let cfg = Config::load(&root.join("models.toml")).unwrap();
        let found = cfg.video_models();
        assert_eq!(found.len(), 1, "the entry replaces the discovered copy");
        assert_eq!(found[0].name, "tuned");
        assert!(!found[0].discovered);
        assert_eq!(found[0].steps, 4);

        std::fs::remove_dir_all(&root).ok();
    }

    /// The image select lists what a file *is*: an LTX transformer in the root,
    /// a loose VAE, and anything under the video and LoRA dirs are all
    /// `.safetensors` and none of them is a checkpoint. A subfolder is found.
    #[test]
    fn the_image_registry_lists_checkpoints_by_header_not_by_extension() {
        let root =
            std::env::temp_dir().join(format!("rustimage-cfg-images-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let models = root.join("models");
        let sdxl = [
            ("model.diffusion_model.input_blocks.0.0.weight", "[320,4,3,3]"),
            (
                "model.diffusion_model.input_blocks.4.1.transformer_blocks.0.attn2.to_k.weight",
                "[640,2048]",
            ),
        ];
        fake_model(&models.join("xl.safetensors"), &sdxl);
        fake_model(&models.join("checkpoints").join("nested.safetensors"), &sdxl);
        fake_model(
            &models.join("ltx.safetensors"),
            &[("model.diffusion_model.adaln_single.emb.timestep_embedder.linear_1.bias", "[4096]")],
        );
        fake_model(
            &models.join("vae").join("ae.safetensors"),
            &[
                ("decoder.conv_in.weight", "[512,16,3,3]"),
                ("decoder.mid.block_1.conv2.bias", "[512]"),
                ("encoder.conv_out.bias", "[32]"),
            ],
        );
        // A checkpoint-shaped file in the LoRA dir is the LoRA listing's to
        // explain, not a second copy in the select.
        fake_model(&models.join("loras").join("stray.safetensors"), &sdxl);

        std::fs::write(root.join("models.toml"), "[sd]\nbinary = \"sd-cli\"\n").unwrap();
        let cfg = Config::load(&root.join("models.toml")).unwrap();
        let names: Vec<String> = cfg.registry().into_keys().collect();
        assert_eq!(names, ["nested", "xl"]);

        std::fs::remove_dir_all(&root).ok();
    }

    /// A bare Z-Image in the tree is assembled from the VAE and encoder beside
    /// it, arrives on its recipe rather than on the SDXL-tuned settings, and a
    /// template still has the last word. Without its encoder it is listed and
    /// not runnable, naming the file it is waiting for.
    #[test]
    fn a_z_image_is_assembled_and_arrives_on_its_recipe() {
        let root =
            std::env::temp_dir().join(format!("rustimage-cfg-zimage-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let models = root.join("models");
        fake_model(
            &models.join("zimage.safetensors"),
            &[("cap_embedder.0.weight", "[3840,2560]")],
        );
        fake_model(
            &models.join("vae").join("ae.safetensors"),
            &[
                ("decoder.conv_in.weight", "[512,16,3,3]"),
                ("decoder.mid.block_1.conv2.bias", "[512]"),
                ("encoder.conv_out.bias", "[32]"),
            ],
        );
        let toml = "[sd]\nbinary = \"sd-cli\"\n";
        std::fs::write(root.join("models.toml"), toml).unwrap();

        let cfg = Config::load(&root.join("models.toml")).unwrap();
        let p = cfg.registry().remove("zimage").unwrap();
        assert_eq!(p.family, ImageFamily::ZImage);
        assert_eq!((p.width, p.steps, p.cfg_scale), (1024, 8, 1.0));
        assert!(!p.exists, "no encoder yet");
        assert_eq!(p.missing(), [models.join("text_encoders").join("qwen_3_4b.safetensors")]);

        fake_model(
            &models.join("te").join("qwen3.safetensors"),
            &[
                ("model.embed_tokens.weight", "[151936,2560]"),
                ("model.layers.0.self_attn.q_norm.weight", "[128]"),
                ("model.norm.weight", "[2560]"),
            ],
        );
        let with_template = format!(
            "{toml}\n[templates.zbase]\nmodel = \"{}\"\nsteps = 28\n",
            models.join("zimage.safetensors").display()
        );
        std::fs::write(root.join("models.toml"), with_template).unwrap();
        let cfg = Config::load(&root.join("models.toml")).unwrap();
        let reg = cfg.registry();
        assert!(!reg.contains_key("zimage"), "the template covers the file");
        let p = &reg["zbase"];
        assert!(p.exists, "missing: {:?}", p.missing());
        assert_eq!((p.width, p.steps), (1024, 28), "recipe under, template over");
        let parts = p.components.as_ref().unwrap();
        assert_eq!(parts.llm, Some(models.join("te").join("qwen3.safetensors")));

        // The user's per-model layer is last, over the template — and the base
        // the page diffs against is everything *but* it.
        let mut tuned = crate::settings::ModelSettings::default();
        tuned.model.insert(
            "zbase".into(),
            PartialSettings {
                steps: Some(5),
                ..PartialSettings::default()
            },
        );
        tuned.save(&cfg.user_dir()).unwrap();
        assert_eq!(cfg.registry()["zbase"].steps, 5);
        assert_eq!(cfg.model_base("zbase").unwrap().steps, 28);

        std::fs::remove_dir_all(&root).ok();
    }

    /// A sidecar from before families: no `family`, no `components`. It must
    /// read as what it was — an SDXL run through `-m`.
    #[test]
    fn an_old_sidecar_profile_is_a_bundled_sdxl() {
        let p: Profile = serde_json::from_str(r#"{"name":"old","model":"/m/x.safetensors"}"#).unwrap();
        assert_eq!(p.family, ImageFamily::Sdxl);
        assert_eq!(p.components, None);
    }

    #[test]
    fn image_family_names_match_their_wire_spelling() {
        for f in ImageFamily::ALL {
            let json = serde_json::to_string(&f).unwrap();
            assert_eq!(json, format!("\"{}\"", f.as_str()));
        }
    }

    /// The guard is structural, so these are the shapes that would otherwise be
    /// joined onto the LoRA directory or re-cut sd.cpp's tag regex.
    #[test]
    fn a_lora_name_cannot_escape_its_directory() {
        for bad in [
            "",
            "../secrets.safetensors",
            "sdxl/../../etc/passwd",
            "/etc/passwd",
            "./detail.safetensors",
            "de:tail.safetensors",
            "detail>.safetensors",
            "<detail.safetensors",
        ] {
            assert!(!valid_lora_name(bad), "{bad:?} should be rejected");
        }
        for good in [
            "detail.safetensors",
            "sdxl/style.safetensors",
            "a b c.gguf",
            "sdxl/character/someone-v2.safetensors",
        ] {
            assert!(valid_lora_name(good), "{good:?} should be accepted");
        }
    }

    /// A strength is range-checked on the profile, because by then it has come
    /// off the wire and is one step from being a command line.
    #[test]
    fn a_lora_strength_is_range_checked() {
        let mut p = Profile {
            name: "t".into(),
            ..Default::default()
        };
        p.loras = vec![LoraRef {
            name: "detail.safetensors".into(),
            strength: 1.0,
        }];
        assert!(p.validate().is_ok());

        // Negative is legal — subtracting a LoRA is a real technique.
        p.loras[0].strength = -0.75;
        assert!(p.validate().is_ok());

        for bad in [3.0, -9.0, f32::NAN] {
            p.loras[0].strength = bad;
            assert!(p.validate().is_err(), "{bad} should be rejected");
        }

        p.loras[0].strength = 1.0;
        p.loras[0].name = "../elsewhere.safetensors".into();
        assert!(p.validate().is_err());
    }

    /// Moving the models dir moves the LoRAs with it, and an explicit
    /// `lora_dir` resolves against the config's directory like every other path.
    #[test]
    fn the_lora_dir_defaults_inside_the_models_dir() {
        let cfg = parse(MINIMAL).expect("minimal config should parse");
        assert_eq!(cfg.lora_dir(), cfg.models_dir().join("loras"));

        let cfg = parse(
            r#"
[sd]
binary = "sd-cli"
models_dir = "weights"
lora_dir = "adapters"
"#,
        )
        .expect("config should parse");
        assert_eq!(cfg.lora_dir(), PathBuf::from("/tmp/adapters"));
    }

    /// The upscaler folder is the LoRA folder's shape: listed by extension,
    /// subfolders included, and a name resolves or says what is there.
    #[test]
    fn an_upscaler_resolves_in_its_folder_or_names_what_is() {
        let base = std::env::temp_dir().join(format!("rustimage-cfg-up-{}", std::process::id()));
        let dir = base.join("models").join("upscalers");
        std::fs::create_dir_all(dir.join("anime")).unwrap();
        // A header naming an RRDB block is all identification reads. The
        // extension is not what decides the format — the magic is — so a
        // safetensors body under `.pth` is a fair stand-in.
        let esrgan = {
            let h = r#"{"body.0.rdb1.conv1.weight":{"dtype":"F32","shape":[1],"data_offsets":[0,4]},"conv_up2.weight":{"dtype":"F32","shape":[1],"data_offsets":[4,8]}}"#;
            let mut b = (h.len() as u64).to_le_bytes().to_vec();
            b.extend(h.as_bytes());
            b.extend([0u8; 8]);
            b
        };
        std::fs::write(dir.join("4x-UltraSharp.pth"), &esrgan).unwrap();
        std::fs::write(dir.join("anime").join("x4plus_anime.safetensors"), &esrgan).unwrap();
        std::fs::write(dir.join("junk.pt"), b"x").unwrap();
        std::fs::write(dir.join("readme.txt"), b"x").unwrap();
        let mut cfg = parse(MINIMAL).unwrap();
        cfg.base_dir = base.clone();
        assert_eq!(cfg.upscaler_dir(), dir);

        let listed = cfg.list_upscalers();
        let names: Vec<&str> = listed.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["4x-UltraSharp.pth", "anime/x4plus_anime.safetensors", "junk.pt"]);
        assert_eq!(listed[0].arch, "ESRGAN");
        assert!(listed[0].supported);
        // Unreadable is listed — it is in the folder — and marked, not hidden.
        assert_eq!((listed[2].arch.as_str(), listed[2].supported), ("unknown", false));
        let err = cfg.check_upscaler(&Some("junk.pt".into())).unwrap_err();
        assert!(format!("{err:#}").contains("junk.pt"), "{err:#}");

        // FSR needs no file.
        assert_eq!(cfg.check_upscaler(&None).unwrap(), None);
        assert_eq!(
            cfg.check_upscaler(&Some("4x-UltraSharp.pth".into())).unwrap(),
            Some(dir.join("4x-UltraSharp.pth"))
        );
        let err = cfg.check_upscaler(&Some("gone.pth".into())).unwrap_err().to_string();
        assert!(err.contains("4x-UltraSharp.pth"), "{err}");
        // Out of the folder is refused before the disk is asked.
        for bad in ["../x.pth", "/etc/passwd", "."] {
            assert!(cfg.check_upscaler(&Some(bad.into())).is_err(), "{bad}");
            let p = Profile {
                upscaler: Some(bad.into()),
                ..Default::default()
            };
            assert!(p.validate().is_err(), "{bad}");
        }
        std::fs::remove_dir_all(&base).ok();
    }

    /// A sidecar written before the knob existed ran with EASU, and has to say
    /// so when it is read back.
    #[test]
    fn an_old_sidecar_profile_upscaled_with_fsr() {
        let p: Profile = serde_json::from_str(r#"{"name":"old","upscale_factor":2.0}"#).unwrap();
        assert_eq!(p.upscaler, None);
    }

    /// A registry entry never carries LoRAs: they only ever arrive on a request.
    /// If this ever fails, a `loras` key has been added to the settings or
    /// `[templates.*]` and the note on [`Profile::loras`] has stopped being true.
    #[test]
    fn a_model_brings_no_loras_of_its_own() {
        let cfg = parse(MINIMAL).expect("minimal config should parse");
        for p in cfg.registry().values() {
            assert!(p.loras.is_empty(), "'{}' arrived with LoRAs", p.name);
        }
    }

    /// Neither escalation checkpoint is ever a property of a model, for the same
    /// reason a LoRA isn't: which one finishes a picture is chosen per picture.
    /// This fails the day somebody adds a `refine_model` key to the settings or
    /// to `[templates.*]`, which is exactly when the note on
    /// [`Profile::refine_model`] would have stopped being true.
    #[test]
    fn a_model_brings_no_escalation_checkpoints_of_its_own() {
        let cfg = parse(MINIMAL).expect("minimal config should parse");
        for p in cfg.registry().values() {
            assert_eq!(p.refine_model, None, "'{}' named a refine model", p.name);
            assert_eq!(p.hires_model, None, "'{}' named a hi-res model", p.name);
        }
    }

    /// The CFG chain: each pass's own knob, then the base CFG. Resolving either
    /// knob to a value at load compiles and quietly severs the link, so raising
    /// `cfg_scale` would leave the escalations behind at the old number.
    #[test]
    fn each_pass_falls_through_to_the_knob_below_it() {
        let mut p = Profile {
            cfg_scale: 6.0,
            ..Profile::default()
        };
        // Nothing set: every pass follows the base CFG.
        assert_eq!(p.refine_cfg(), 6.0);
        assert_eq!(p.hires_cfg(), 6.0);

        // One pass pinned; the other is untouched, which is the feature.
        p.refine_cfg_scale = Some(3.0);
        assert_eq!(p.refine_cfg(), 3.0);
        assert_eq!(p.hires_cfg(), 6.0);

        // Raising the base moves whatever isn't pinned, and only that.
        p.cfg_scale = 7.0;
        assert_eq!(p.hires_cfg(), 7.0);
        assert_eq!(p.refine_cfg(), 3.0);

        // 0 is guidance off, not "unset" — the one value an `unwrap_or` over a
        // number rather than an `Option` would swallow.
        p.hires_cfg_scale = Some(0.0);
        assert_eq!(p.hires_cfg(), 0.0);
        assert!(p.validate().is_ok(), "0 is a legal CFG");

        p.refine_cfg_scale = Some(-1.0);
        assert!(p.validate().is_err(), "a negative CFG must be rejected");
    }

    #[test]
    fn a_minimal_video_entry_parses_and_inherits() {
        let cfg = parse(MINIMAL).expect("minimal entry should parse");
        let m = &cfg.video[0];
        assert_eq!(m.family, VideoFamily::Wan22Ti2v5b);
        assert_eq!((m.width, m.height, m.frames), (832, 480, 33));
        assert!(m.offload_to_cpu && m.flash_attn && !m.vae_tiling);
        // The negative prompt is inherited rather than retyped, which is the
        // whole reason it defaults in code.
        assert!(m.negative_prompt.contains("最差质量"));
        assert!(!m.negative_prompt.contains(' '));
    }

    #[test]
    fn video_paths_resolve_against_the_config_dir() {
        let cfg = parse(MINIMAL).unwrap();
        let m = &cfg.video_models()[0];
        assert_eq!(m.diffusion_model, PathBuf::from("/tmp/v/ti2v.safetensors"));
        assert_eq!(m.t5xxl.as_deref(), Some(Path::new("/tmp/v/umt5.gguf")));
        // Nothing is on disk under /tmp/v, so every component is missing and
        // the entry knows it.
        assert!(!m.exists);
        assert_eq!(m.missing().len(), 3);
    }

    #[test]
    fn an_unknown_family_is_rejected_by_name() {
        let src = MINIMAL.replace("wan22_ti2v_5b", "wan_9000");
        let err = parse(&src).unwrap_err().to_string();
        assert!(err.contains("wan_9000"), "{err}");
    }

    #[test]
    fn a_moe_entry_needs_both_experts() {
        let src = MINIMAL.replace("wan22_ti2v_5b", "wan22_a14b");
        let err = parse(&src).unwrap_err().to_string();
        assert!(err.contains("high_noise_diffusion_model"), "{err}");

        let src = format!("{src}high_noise_diffusion_model = \"v/high.gguf\"\n");
        let cfg = parse(&src).expect("both experts named should validate");
        assert!(cfg.video[0].family.is_moe());
    }

    #[test]
    fn a_wan_entry_needs_a_text_encoder() {
        let src = MINIMAL.replace("t5xxl = \"v/umt5.gguf\"\n", "");
        let err = parse(&src).unwrap_err().to_string();
        assert!(err.contains("t5xxl"), "{err}");
    }

    #[test]
    fn two_entries_cannot_share_a_name() {
        // Through `load`, since that's where the uniqueness check lives.
        let dir = std::env::temp_dir().join(format!("rustimage-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("models.toml");
        let second = &MINIMAL[MINIMAL.find("[[video]]").unwrap()..];
        std::fs::write(&path, format!("{MINIMAL}\n{second}")).unwrap();
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(err.contains("both named '5b'"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The numbers here are sd.cpp's, read off `align_image_size` and
    /// `get_scale_factor`: `wan_2.1_vae` is 8× spatial and the TI2V-5B VAE is
    /// 16×, both times a DiT down factor of 2.
    #[test]
    fn size_grids_are_per_family() {
        assert_eq!(VideoFamily::Wan22Ti2v5b.size_multiple(), 32);
        assert_eq!(VideoFamily::MinimaxH3.size_multiple(), 32);
        assert_eq!(VideoFamily::Wan22A14b.size_multiple(), 16);
        assert_eq!(VideoFamily::Wan21Vace.size_multiple(), 16);

        // Aligned up, like sd.cpp — and 480/832 are on both grids already,
        // which is why every documented example uses them.
        for f in [VideoFamily::Wan22Ti2v5b, VideoFamily::Wan22A14b] {
            assert_eq!(f.align_size(480), 480);
            assert_eq!(f.align_size(832), 832);
        }
        assert_eq!(VideoFamily::Wan22A14b.align_size(500), 512);
        assert_eq!(VideoFamily::Wan22Ti2v5b.align_size(500), 512);
        // The one case where the coarser grid shows: 496 is a legal A14B width
        // and gets rounded again for the 5B.
        assert_eq!(VideoFamily::Wan22A14b.align_size(496), 496);
        assert_eq!(VideoFamily::Wan22Ti2v5b.align_size(496), 512);
        // Never zero, whatever comes in.
        assert_eq!(VideoFamily::Wan22A14b.align_size(0), 16);
    }

    /// The direction is the point: Wan truncates its way *down* to `4k+1`
    /// through the latent count, H3 walks *up* to `17k+5`.
    #[test]
    fn frame_grids_round_in_opposite_directions() {
        let wan = VideoFamily::Wan22A14b;
        assert_eq!(wan.align_frames(33), 33);
        assert_eq!(wan.align_frames(34), 33);
        assert_eq!(wan.align_frames(36), 33);
        assert_eq!(wan.align_frames(37), 37);
        assert_eq!(wan.align_frames(81), 81);
        assert_eq!(wan.align_frames(1), 1);
        assert_eq!(wan.align_frames(0), 1);
        // 4 frames is 1 latent frame, which decodes back to a single image —
        // exactly what sd.cpp does with it.
        assert_eq!(wan.align_frames(4), 1);

        let h3 = VideoFamily::MinimaxH3;
        assert_eq!(h3.align_frames(0), 5);
        assert_eq!(h3.align_frames(5), 5);
        assert_eq!(h3.align_frames(6), 22);
        assert_eq!(h3.align_frames(22), 22);
        assert_eq!(h3.align_frames(23), 39);
        assert_eq!(h3.align_frames(56), 56);
        for f in 1..200u32 {
            let a = h3.align_frames(f);
            assert!(a >= f.max(5) && a % 17 == 5, "{f} -> {a}");
        }
    }

    /// Every variant has to be in `ALL`, or the two tests below quietly stop
    /// covering the one that was added last.
    #[test]
    fn all_families_are_listed() {
        let mut names: Vec<&str> = VideoFamily::ALL.iter().map(|f| f.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(
            names.len(),
            VideoFamily::ALL.len(),
            "two families share a wire name"
        );
    }

    /// `as_str` is what the UI rounds by, `Deserialize` is what `models.toml`
    /// is written in. They are two hand-written lists of the same names, so
    /// they can drift; this is the test that says so.
    #[test]
    fn family_names_match_their_wire_spelling() {
        for f in VideoFamily::ALL {
            let src = MINIMAL.replace("wan22_ti2v_5b", f.as_str());
            let parsed: Config = toml::from_str(&src)
                .unwrap_or_else(|e| panic!("{} is not a parseable family: {e}", f.as_str()));
            assert_eq!(parsed.video[0].family, f);
        }
    }

    /// The grid the page is handed has to describe the rounding the command
    /// builder performs — not approximately, exactly. Checked by taking
    /// `align_frames` as the truth and asserting the published grid explains
    /// every one of its answers.
    #[test]
    fn the_frame_grid_describes_the_rounding() {
        for f in VideoFamily::ALL {
            let g = f.frame_grid();
            for want in 0..200u32 {
                let got = f.align_frames(want);
                assert!(got >= g.min, "{:?}: {want} -> {got} is below the floor", f);
                assert_eq!(
                    got % g.multiple,
                    g.offset % g.multiple,
                    "{:?}: {want} -> {got} is off the grid",
                    f
                );
                if g.round_up {
                    assert!(
                        got >= want.max(g.min),
                        "{:?}: {want} -> {got} rounded down",
                        f
                    );
                    // And it is the *nearest* legal count in that direction.
                    assert!(
                        got < want.max(g.min) + g.multiple,
                        "{:?}: {want} -> {got}",
                        f
                    );
                } else if want > g.min {
                    assert!(got <= want, "{:?}: {want} -> {got} rounded up", f);
                    assert!(got + g.multiple > want, "{:?}: {want} -> {got}", f);
                }
            }
        }
    }

    #[test]
    fn fps_is_forced_only_where_sd_cpp_forces_it() {
        assert_eq!(VideoFamily::MinimaxH3.forced_fps(), Some(24));
        assert_eq!(VideoFamily::Wan22A14b.forced_fps(), None);
        assert_eq!(VideoFamily::Wan22Ti2v5b.forced_fps(), None);
    }

    #[test]
    fn duration_uses_the_numbers_that_will_really_run() {
        let mut m = parse(MINIMAL).unwrap().video.remove(0);
        m.frames = 36;
        m.fps = 24;
        // 36 rounds down to 33, not up to 37 — so 1.375 s, not 1.5.
        assert_eq!(m.aligned(), (832, 480, 33));
        assert!((m.duration() - 33.0 / 24.0).abs() < 1e-6);

        m.family = VideoFamily::MinimaxH3;
        m.fps = 8;
        // H3 ignores the requested fps entirely, so the duration must not
        // believe it.
        assert_eq!(m.effective_fps(), 24);
        assert!((m.duration() - 39.0 / 24.0).abs() < 1e-6);
    }

    #[test]
    fn the_high_noise_cfg_follows_cfg_scale_until_it_is_set() {
        let mut m = parse(MINIMAL).unwrap().video.remove(0);
        m.cfg_scale = 3.5;
        assert_eq!(m.high_noise_cfg(), 3.5);
        m.cfg_scale = 5.0;
        assert_eq!(m.high_noise_cfg(), 5.0, "unset must not pin the old value");
        m.high_noise_cfg_scale = Some(1.0);
        assert_eq!(m.high_noise_cfg(), 1.0);
    }

    /// An absent knob keeps what the image has. This is the whole contract of
    /// `/adjust`: the page sends all ten because it has all ten on screen, and
    /// anything else may send one.
    #[test]
    fn a_patch_moves_only_the_knobs_it_names() {
        let mut f = Filters {
            sharpness: 0.35,
            saturation: 0.4,
            hue: -0.2,
            ..Filters::NONE
        };
        let patch: FiltersPatch = serde_json::from_str(r#"{"saturation":-1.0}"#).unwrap();
        patch.apply(&mut f);
        assert_eq!(f.saturation, -1.0);
        assert_eq!(f.sharpness, 0.35, "an absent knob must not be reset to 0");
        assert_eq!(f.hue, -0.2);
    }

    /// Every knob is range-checked, not just the two that predate the grade —
    /// the trap being a new field added to the struct and forgotten in the
    /// table, which validates clean and reaches the shader as anything at all.
    #[test]
    fn every_filter_is_range_checked() {
        assert!(Filters::NONE.validate("m").is_ok());
        for (name, set) in [
            (
                "sharpness",
                (|f: &mut Filters, v| f.sharpness = v) as fn(&mut Filters, f32),
            ),
            ("clarity", |f, v| f.clarity = v),
            ("lightness", |f, v| f.lightness = v),
            ("contrast", |f, v| f.contrast = v),
            ("highlights", |f, v| f.highlights = v),
            ("shadows", |f, v| f.shadows = v),
            ("saturation", |f, v| f.saturation = v),
            ("hue", |f, v| f.hue = v),
            ("temperature", |f, v| f.temperature = v),
            ("tint", |f, v| f.tint = v),
        ] {
            let mut f = Filters::NONE;
            set(&mut f, 1.5);
            let err = f.validate("m").unwrap_err().to_string();
            assert!(err.contains(name), "1.5 in {name} was accepted: {err}");
            // Sharpness is the one knob with no negative half — RCAS has no
            // direction to run backwards in.
            let mut f = Filters::NONE;
            set(&mut f, -0.5);
            assert_eq!(
                f.validate("m").is_err(),
                name == "sharpness",
                "{name} disagrees with its own lower bound"
            );
        }
    }

    /// The pass has to be skipped at neutral, because the sRGB/OKLab round trip
    /// is not byte-exact and `fsr-selftest` asserts byte equality.
    #[test]
    fn a_neutral_grade_is_no_work_at_all() {
        assert!(!Filters::NONE.grades());
        assert!(
            !Filters::starting_at(1.0, -1.0).grades(),
            "detail is not a grade"
        );
        assert!(Filters {
            tint: -0.01,
            ..Filters::NONE
        }
        .grades());
    }

    /// Detail resets, colour carries. Both halves, because the rule is an
    /// asymmetry and the obvious tidy-up is to make it consistent.
    ///
    /// The third assertion is the one that had to be learned: what an escalation
    /// carries is *not* where the sliders start any more, so the two have to be
    /// reported separately or the viewer's Reset makes each edit the next
    /// image's neutral and there is no way back to an ungraded picture.
    #[test]
    fn an_escalation_keeps_the_grade_and_drops_the_detail() {
        let live = Filters {
            sharpness: 0.9,
            clarity: 0.5,
            saturation: 0.3,
            temperature: -0.4,
            ..Filters::NONE
        };
        let next = live.restart(0.35, 0.0);
        assert_eq!((next.sharpness, next.clarity), (0.35, 0.0));
        assert_eq!((next.saturation, next.temperature), (0.3, -0.4));
        assert_ne!(
            next,
            Filters::starting_at(0.35, 0.0),
            "a carried grade is not a starting point — GenResult.start is the one \
             the viewer resets to"
        );
    }
}
