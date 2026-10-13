//! `user/settings.toml` — the defaults a generation starts from, in the one
//! config file the app *writes*.
//!
//! The split between this and `models.toml` is not TOML-versus-anything, it is
//! **who owns the file**:
//!
//!   * `models.toml` is hand-written and mostly comments — what the program
//!     needs in order to start (the sd.cpp binary, the directories, the port,
//!     the per-checkpoint presets, the video models). The app only ever reads
//!     it, which is what lets those comments survive.
//!   * this file is regenerated in full every time Save is pressed on the
//!     Settings tab, so it carries one fixed header and no per-field prose. A
//!     serializer cannot preserve comments, and a settings *form* that silently
//!     deleted the explanation of what it just changed would be worse than no
//!     form.
//!
//! It is read **per call** rather than held (see [`crate::config::Config::registry`]):
//! `Config` is an `Arc` and the GPU worker owns its own clone of it, so a value
//! captured at startup could only be changed by restarting the server. A ~1 KB
//! parse against a multi-second diffusion is not worth a restart, and it is the
//! same rule the LoRA scan already follows.

use crate::config::Profile;
use crate::pipeline::MAX_UPSCALE;
use crate::variables::{self, Variables};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// File name inside [`crate::config::Config::user_dir`].
const FILE: &str = "settings.toml";

/// Written above every save. It is the only comment in the file, because the
/// file is rewritten wholesale — anything else put here would be deleted by the
/// next press of Save, which is a worse outcome than not writing it.
const HEADER: &str = "\
# yasdig — your defaults. Written by the Settings tab; edit it by hand if you
# like, but note that pressing Save rewrites the whole file and any comments you
# add here go with it.
#
# These apply to every model. width, height, steps, cfg_scale, sampler and scheduler reach
# only SD 1.x, 2.x and SDXL: every other family starts from its own defaults for
# those. A [templates.<name>] block in models.toml, then user/model-settings.toml,
# override whichever of these they name. See docs/configuration.md for what each field does.

";

/// Everything the Settings tab edits.
///
/// Three of these are not generation settings at all and have never had a home:
/// `prompt` and `negative` were hardcoded `value=` attributes in `index.html`,
/// and `hires_scale` was `S.upFactor = 2` in the page's state module. They live
/// here rather than on [`Profile`] because none of them is a property of a
/// checkpoint — a model has a house step count, no model has an opinion about
/// what you usually want to draw.
///
/// The rest mirror [`PartialSettings`] field for field, and
/// [`Settings::merged_with`] is what keeps the two honest: it is a struct
/// literal with no `..rest`, so adding a knob to one and forgetting the other is
/// a compile error rather than a setting that silently works for half the
/// models.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    // ---- The page's own defaults. Not per-model; see the doc comment. ----
    /// What the Image tab's prompt box contains at load.
    pub prompt: String,
    /// What both image tabs' negative boxes contain at load.
    pub negative: String,
    /// Where the hi-res pass's enlarge factor starts (`#hiresscale`/`#upfactor`).
    /// Not [`Settings::upscale_factor`], which is the FSR enlarge every base
    /// render goes through on its way to the output size.
    pub hires_scale: f32,

    // ---- What was `[defaults]` in models.toml, unchanged in meaning. ----
    pub width: u32,
    pub height: u32,
    pub steps: u32,
    pub cfg_scale: f32,
    pub sampler: String,
    /// `--scheduler`, or `"auto"` to send none and let sd.cpp pick the model's
    /// own. See [`Profile::scheduler`].
    pub scheduler: String,
    /// Step caching for the generate pass: `off`, `spectrum` or `adaptive`.
    /// See [`Profile::cache_mode`].
    pub cache_mode: String,
    /// Where the weights live: `auto`, `ram` or `disk`. See [`Profile::memory`].
    pub memory: String,
    pub upscale_factor: f32,
    /// What performs that enlarge: `None` is FSR (EASU), a name is an ESRGAN
    /// model under the upscaler dir. See [`Profile::upscaler`].
    pub upscaler: Option<String>,
    pub sharpness: f32,
    pub clarity: f32,
    pub refine_steps: u32,
    pub refine_strength: f32,
    /// CFG for the refine pass alone; `None` follows `cfg_scale`. See [`Profile::refine_cfg`].
    pub refine_cfg_scale: Option<f32>,
    pub hires_steps: u32,
    pub hires_strength: f32,
    /// The same, for the hi-res pass. Declared here rather than beside
    /// `refine_cfg_scale` so each pass's three knobs — steps, strength, CFG —
    /// are written in the file in the order the panel shows them.
    pub hires_cfg_scale: Option<f32>,
    pub source_strength: f32,
    pub hires_max_side: u32,
    pub hires_flash_attn: bool,
    pub weight_type: Option<String>,
    pub extra_args: Vec<String>,

    // ---- What *Close server* clears on the way out. Page-level, like the
    // three at the top: no checkpoint has an opinion about your disk. ----
    /// Empty `work/workitems/` — the Current shelf, never Favorites.
    pub clear_workitems_on_close: bool,
    /// Empty `work/output/`, uploads included — never `user/favorite_outputs/`.
    pub clear_outputs_on_close: bool,
    /// Empty `work/videos/`, and the enlarged frames of each clip.
    pub clear_videos_on_close: bool,

    /// The longest side any enlarge may produce — Upscale, Hi-res, a generate's
    /// output upscale. Page-level: a limit of this machine and this browser, not
    /// of a checkpoint. The GPU's own limit (`Fsr::max_side`, 16384 on a 7900
    /// XTX) is the ceiling above it; this is the lower, chosen one, at 8192 by
    /// default because a bigger PNG crossing to the page as base64 is the thing
    /// that struggles first. `Engine::check_side` is where it is enforced.
    pub max_image_side: u32,
    /// GB of the card kept free of sd.cpp's weights and buffers: `--max-vram
    /// -N` on every diffusion run, in every memory mode. sd.cpp packs the card
    /// to its live free memory minus 512 MiB, and the desktop, the browser and
    /// the driver's own bookkeeping need more slack than that — RADV answers a
    /// submission that doesn't fit with a lost device ("Not enough memory for
    /// command submission"). Page-level, because that slack is this machine's,
    /// not a checkpoint's. 0 is sd.cpp's own behaviour.
    pub gpu_reserve_gb: f32,
    /// The side, in pixels, each reference image is drawn at: sd.cpp's
    /// `vae_input_max_pixels` is this squared. 0 is sd.cpp's own default — a
    /// reference resized to the output's area, up to a megapixel. Each
    /// reference costs attention like the image itself, and attention grows
    /// with the square of the tokens: four references made a Q8 Klein 9B at
    /// 1024² take 153 s against 22 s with none. A likeness, a face or a
    /// garment survives 512 well. Page-level: a speed choice for this machine,
    /// not a model's. Not applied to Edit, whose first reference is the image
    /// being changed.
    pub reference_size: u32,
    /// sd-cli's `--vae-conv-direct`: the VAE's convolutions computed directly
    /// instead of through `ggml_conv_2d`'s im2col, which unrolls the
    /// full-resolution activations into a buffer gigabytes long per layer. The
    /// decode is bound by that traffic, not by the weights — an f16 copy of
    /// Klein's VAE changed nothing, this took its decode from 9.3 s to 0.9 s
    /// (and Krea 2's Wan VAE from 3.3 s to 1.2 s).
    /// Page-level: how this machine computes, not a checkpoint's choice.
    pub vae_conv_direct: bool,
    /// Show the Video tab. The video workflow is unfinished, so a release
    /// hides it unless asked for — Settings → Nightly features. Hidden is not
    /// removed: the routes, the models and the clips on disk are all still
    /// there. Page-level.
    pub nightly_video: bool,
    /// Write the settings that made a picture into the PNG when it is saved to
    /// output: A1111's `parameters` text and yasdig's own record
    /// ([`crate::pngmeta`]). On by default, as in A1111; off for someone who
    /// shares pictures and would rather their prompts didn't travel with them.
    /// Page-level.
    pub embed_png_settings: bool,

    /// The `[variables]` table: `{animal}` in a prompt and the words it can
    /// stand for. See [`crate::variables`].
    ///
    /// It belongs with the three page-level fields at the top — no checkpoint
    /// has an opinion about your word lists, so it is not on
    /// [`PartialSettings`] and not on [`Profile`] — and it is declared **last**
    /// anyway because TOML has no way back out of a table. Serde writes fields
    /// in declaration order, so a scalar after this one makes
    /// `toml::to_string_pretty` fail with `ValueAfterTable`.
    pub variables: Variables,
}

/// The same list as `Option`s: what a `[templates.<name>]` block states about
/// one checkpoint.
///
/// A template overrides only what it names and inherits everything else from
/// [`Settings`], which is what makes the Settings tab mean something for a
/// templated model too. It is also what a preset for a new architecture wants to
/// be — a `[templates.zimage]` should say what is *different* about zImage, not
/// restate your whole workflow.
///
/// It doubles as the reader for the old `[defaults]` block, which is how a first
/// run seeds the settings file from a `models.toml` that was already tuned. See
/// [`Settings::ensure`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PartialSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cfg_scale: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampler: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduler: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upscale_factor: Option<f32>,
    /// Absent inherits, a name pins, the same one-way shape `refine_cfg_scale`
    /// below has: a template can name a model, and has no way to say "back to
    /// FSR" over a setting that chose one — nor needs one, since the select on
    /// the page can.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upscaler: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sharpness: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clarity: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refine_steps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refine_strength: Option<f32>,
    /// Optional, not doubly so: a template says "pin this" or says nothing,
    /// and has `cfg_scale` to reach for when it wants the link back. A
    /// template cannot express "unset it back to following cfg_scale" and
    /// doesn't need to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refine_cfg_scale: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hires_steps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hires_strength: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hires_cfg_scale: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_strength: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hires_max_side: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hires_flash_attn: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_args: Option<Vec<String>>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            // Draws from the two default variables below, so a first Generate
            // already shows what `{name}` does: a batch varies its subject.
            prompt: "a lovely {animal} with {color} hair, cinematic lighting, sharp, in focus"
                .to_string(),
            negative: "watermark, worst quality, low quality".to_string(),
            hires_scale: 2.0,
            // Tuned for standard (non-distilled) SDXL, since that is the common
            // discovered checkpoint. A turbo model wants a template (few steps,
            // CFG 1) or the per-request overrides in the Advanced panel.
            width: 768,
            height: 768,
            steps: 30,
            cfg_scale: 6.0,
            sampler: "euler".to_string(),
            // sd.cpp's own per-model choice, which is not one scheduler: Flux
            // and Flux.2 have schedules of their own, and forcing `discrete`
            // onto them would quietly make them worse.
            scheduler: crate::config::AUTO_SCHEDULER.to_string(),
            // Off until measured: a skipped step is a guess about the model's
            // output, and which mode guesses well on which family is a
            // measurement nobody has made here yet.
            cache_mode: crate::config::CACHE_OFF.to_string(),
            // sd.cpp's auto-fit: no flag at all, which is what every run made
            // before this existed used on the image side.
            memory: crate::config::MEMORY_AUTO.to_string(),
            upscale_factor: 2.0,
            // FSR: free, and needs no file. An ESRGAN model is a download.
            upscaler: None,
            // Off. Sharpening is a live slider under the image, not something a
            // render arrives pre-seasoned with — so the first thing on screen is
            // what the model and the upscaler actually produced.
            sharpness: 0.0,
            clarity: 0.0,
            // On by default: at 0 a fresh install showed both buttons greyed out
            // and the after-each passes ran nothing. Both counts are floors
            // (`SdRunner::img2img` raises them to `steps × strength`), so a
            // small number already adapts — 30-step SDXL at 0.30 runs 9 — and
            // only a few-step distilled model runs exactly this. 4 and 12 are
            // the values tuned on the dev box's own settings.
            refine_steps: 4,
            refine_strength: 0.30,
            // Both unset, which is what makes this change invisible to an
            // existing install: every pass keeps following `cfg_scale`.
            refine_cfg_scale: None,
            hires_cfg_scale: None,
            hires_steps: 12,
            // Same denoise as the refine: enough to resolve real detail at the
            // larger size, low enough that the composition doesn't drift.
            hires_strength: 0.30,
            // The pass over a source image is a *transform*, not a finish, so it
            // starts far higher than either escalation: at 0.30 a source comes
            // back essentially unchanged and the feature looks broken. 0.55 keeps the
            // composition, the pose and the palette while redrawing the surface
            // in the model's own idiom. It runs the model's own `steps` at its own
            // `cfg_scale`; there is no second step count to keep in step.
            source_strength: 0.55,
            // Longest side the hi-res pass may diffuse at. SDXL's quality falls
            // off past this and the cost climbs quadratically; beyond it a final
            // FSR upscale covers the rest.
            hires_max_side: 2048,
            hires_flash_attn: true,
            weight_type: None,
            extra_args: Vec::new(),
            // Off: deleting files is something to ask for, never a default.
            clear_workitems_on_close: false,
            clear_outputs_on_close: false,
            clear_videos_on_close: false,
            max_image_side: DEFAULT_MAX_IMAGE_SIDE,
            // sd.cpp's own packing until someone needs more slack: the
            // out-of-memory box offers it when a run dies of exactly that.
            gpu_reserve_gb: 0.0,
            // A quarter of sd.cpp's megapixel per reference. Chosen by eye on
            // Klein 9B: the likeness held at 512, and 1024² with four
            // references took 33.6 s against 54 s at 768.
            reference_size: 512,
            // Measured 10× on Klein 9B's VAE (9.3 s → 0.9 s) with the same
            // picture; direct convolution computes the same sums.
            vae_conv_direct: true,
            nightly_video: false,
            embed_png_settings: true,
            // What the default prompt names. A file with no `[variables]`
            // table gets these too, which is what keeps that prompt from being
            // a 400; one the page emptied is written as an empty table and
            // stays empty (`emptied_variables_stay_empty`).
            variables: Variables::from([
                ("animal".to_string(), ["cat", "fox", "bear"].map(String::from).to_vec()),
                ("color".to_string(), ["red", "blue", "golden"].map(String::from).to_vec()),
            ]),
        }
    }
}

impl Settings {
    /// These settings with a template's stated overrides laid on top.
    ///
    /// Deliberately a struct literal with **no `..self`**: the whole point of
    /// splitting [`PartialSettings`] out is that the two lists cannot drift, and
    /// a `..` would turn a forgotten field into a knob that quietly only works
    /// for discovered models. That is the trap `CLAUDE.md` opens with, and this
    /// is what turns it into a build failure.
    ///
    /// The three page-level fields are not in the overlay and so pass straight
    /// through — a checkpoint has no opinion about your default prompt.
    pub fn merged_with(&self, over: &PartialSettings) -> Settings {
        Settings {
            prompt: self.prompt.clone(),
            negative: self.negative.clone(),
            hires_scale: self.hires_scale,
            width: over.width.unwrap_or(self.width),
            height: over.height.unwrap_or(self.height),
            steps: over.steps.unwrap_or(self.steps),
            cfg_scale: over.cfg_scale.unwrap_or(self.cfg_scale),
            sampler: over.sampler.clone().unwrap_or_else(|| self.sampler.clone()),
            scheduler: over.scheduler.clone().unwrap_or_else(|| self.scheduler.clone()),
            cache_mode: over.cache_mode.clone().unwrap_or_else(|| self.cache_mode.clone()),
            memory: over.memory.clone().unwrap_or_else(|| self.memory.clone()),
            upscale_factor: over.upscale_factor.unwrap_or(self.upscale_factor),
            upscaler: over.upscaler.clone().or_else(|| self.upscaler.clone()),
            sharpness: over.sharpness.unwrap_or(self.sharpness),
            clarity: over.clarity.unwrap_or(self.clarity),
            refine_steps: over.refine_steps.unwrap_or(self.refine_steps),
            refine_strength: over.refine_strength.unwrap_or(self.refine_strength),
            refine_cfg_scale: over.refine_cfg_scale.or(self.refine_cfg_scale),
            hires_steps: over.hires_steps.unwrap_or(self.hires_steps),
            hires_strength: over.hires_strength.unwrap_or(self.hires_strength),
            hires_cfg_scale: over.hires_cfg_scale.or(self.hires_cfg_scale),
            source_strength: over.source_strength.unwrap_or(self.source_strength),
            hires_max_side: over.hires_max_side.unwrap_or(self.hires_max_side),
            hires_flash_attn: over.hires_flash_attn.unwrap_or(self.hires_flash_attn),
            weight_type: over
                .weight_type
                .clone()
                .or_else(|| self.weight_type.clone()),
            extra_args: over
                .extra_args
                .clone()
                .unwrap_or_else(|| self.extra_args.clone()),
            clear_workitems_on_close: self.clear_workitems_on_close,
            clear_outputs_on_close: self.clear_outputs_on_close,
            clear_videos_on_close: self.clear_videos_on_close,
            max_image_side: self.max_image_side,
            gpu_reserve_gb: self.gpu_reserve_gb,
            reference_size: self.reference_size,
            vae_conv_direct: self.vae_conv_direct,
            nightly_video: self.nightly_video,
            embed_png_settings: self.embed_png_settings,
            variables: self.variables.clone(),
        }
    }

    /// Where the file lives, given [`crate::config::Config::user_dir`].
    pub fn path(user_dir: &Path) -> PathBuf {
        user_dir.join(FILE)
    }

    /// Read it, or say why not. A missing file is an error here — callers that
    /// can carry on without one use [`Settings::load_or_default`].
    pub fn load(user_dir: &Path) -> Result<Settings> {
        let path = Self::path(user_dir);
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    /// Read it, falling back to the built-ins.
    ///
    /// This is what `registry()` calls, and it cannot return a `Result` without
    /// making every caller of `registry()` fallible. The failure is not
    /// swallowed: `GET /settings` reports the parse error to the Settings tab,
    /// and `doctor` reports it at startup — the two places somebody looks when
    /// the numbers are not what they set.
    pub fn load_or_default(user_dir: &Path) -> Settings {
        Self::load(user_dir).unwrap_or_default()
    }

    /// Create the file if it isn't there, seeded from the old `[defaults]` block
    /// when `models.toml` still has one. Returns the path if it wrote.
    ///
    /// Seeding matters more than it looks: the shipped `models.toml` set
    /// `upscale_factor = 2.0` where the built-in is 1.5 and
    /// `refine_strength = 0.05` where the built-in is 0.30, so a clean break
    /// would have silently retuned an existing install on upgrade.
    pub fn ensure(user_dir: &Path, seed: Option<&PartialSettings>) -> Result<Option<PathBuf>> {
        let path = Self::path(user_dir);
        if path.exists() {
            return Ok(None);
        }
        let settings = match seed {
            Some(over) => Settings::default().merged_with(over),
            None => Settings::default(),
        };
        settings.save(user_dir)?;
        Ok(Some(path))
    }

    /// Write it, whole, through a temp file and a rename.
    ///
    /// Renaming rather than truncating in place is what keeps a crash or a full
    /// disk from leaving half a settings file behind — the same reason
    /// `ensure_output_thumb` does it. The counter-free temp name is safe because
    /// the only writer is a `POST /settings`, and two of those racing would
    /// leave one of the two complete files rather than a blend.
    pub fn save(&self, user_dir: &Path) -> Result<()> {
        std::fs::create_dir_all(user_dir)
            .with_context(|| format!("creating {}", user_dir.display()))?;
        let body = format!(
            "{HEADER}{}",
            toml::to_string_pretty(self).context("serializing settings")?
        );
        let path = Self::path(user_dir);
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, body).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    /// Reject anything that would make a pass a no-op, blow up a schedule, or
    /// fail deep inside a render.
    ///
    /// Most of it is [`Profile::validate`] doing the work over a throwaway
    /// profile, on purpose: the strength floors, the CFG rule and the filter
    /// ranges already live there and are already what every generation is
    /// checked against. A second copy here would be free to disagree with the
    /// one that actually gates a run.
    pub fn validate(&self) -> Result<()> {
        // Base size, steps, upscale factor and hi-res budget are checked by
        // `Profile::validate` at the end, which a `/generate` goes through too.
        if !(1.0..=MAX_UPSCALE).contains(&self.hires_scale) {
            bail!(
                "hires_scale is {}, must be between 1 and {MAX_UPSCALE}",
                self.hires_scale
            );
        }
        // The GPU's own limit is checked where it is known, at the enlarge
        // (`Engine::check_side`); this only refuses what no GPU allows.
        if !(1024..=MAX_IMAGE_SIDE_CEILING).contains(&self.max_image_side) {
            bail!(
                "max_image_side is {}, must be between 1024 and {MAX_IMAGE_SIDE_CEILING}",
                self.max_image_side
            );
        }
        if self.reference_size != 0 && !(256..=2048).contains(&self.reference_size) {
            bail!(
                "reference_size is {}, must be 0 (sd.cpp's own) or between 256 and 2048",
                self.reference_size
            );
        }
        if !(0.0..=MAX_GPU_RESERVE_GB).contains(&self.gpu_reserve_gb) {
            bail!(
                "gpu_reserve_gb is {}, must be between 0 and {MAX_GPU_RESERVE_GB}",
                self.gpu_reserve_gb
            );
        }
        variables::validate(&self.variables)?;
        self.as_profile("settings").validate()
    }

    /// These settings as a nameless profile, for [`Settings::validate`] and for
    /// the registry to fill in a model's identity on top of.
    ///
    /// Spelled out rather than `..Default::default()` for the same reason
    /// [`Settings::merged_with`] is: [`Profile`] is the other struct carrying
    /// this field list, and a `..` here would let the two drift.
    pub fn as_profile(&self, name: &str) -> Profile {
        Profile {
            name: name.to_string(),
            description: String::new(),
            model: PathBuf::new(),
            width: self.width,
            height: self.height,
            steps: self.steps,
            cfg_scale: self.cfg_scale,
            sampler: self.sampler.clone(),
            scheduler: self.scheduler.clone(),
            cache_mode: self.cache_mode.clone(),
            memory: self.memory.clone(),
            upscale_factor: self.upscale_factor,
            upscaler: self.upscaler.clone(),
            sharpness: self.sharpness,
            clarity: self.clarity,
            refine_steps: self.refine_steps,
            refine_strength: self.refine_strength,
            refine_cfg_scale: self.refine_cfg_scale,
            hires_steps: self.hires_steps,
            hires_strength: self.hires_strength,
            hires_cfg_scale: self.hires_cfg_scale,
            // Request-only, like `loras` below: which checkpoint finishes a
            // picture is chosen per picture, so no checkpoint states it and
            // there is no key for one here.
            refine_model: None,
            hires_model: None,
            source_strength: self.source_strength,
            hires_max_side: self.hires_max_side,
            hires_flash_attn: self.hires_flash_attn,
            weight_type: self.weight_type.clone(),
            extra_args: self.extra_args.clone(),
            // Request-only, like everywhere else a profile is built: a LoRA is
            // reached for per picture, so there is no key for one here.
            loras: Vec::new(),
            // Request-only too, and for the same reason: references are picked
            // per picture.
            references: Vec::new(),
            // Identity, like `model` above: the registry fills these from what
            // the file is. SDXL and bundled is what a profile with no file
            // behind it has always meant.
            family: crate::config::ImageFamily::default(),
            components: None,
            bundled: None,
            discovered: false,
            exists: false,
        }
    }
}

/// What `GET /settings` publishes.
///
/// `serde_json` rather than the hand-rolled `format!` the two long-standing
/// shapes use, which is the rule in `CLAUDE.md`: this is backed by a real struct
/// and a 22-field round-trip is not worth hand-writing twice.
///
/// `note` is spliced in as an `error` key rather than wrapping the settings in
/// an envelope: the form binds to them by field name, and `{"settings":{…}}`
/// would put every field one level deeper for the sake of one optional string.
pub fn to_json(s: &Settings, note: Option<&str>) -> Result<String> {
    let body = serde_json::to_string(s).context("serializing settings")?;
    Ok(match note {
        Some(msg) => format!(
            "{{{}:{},{}",
            serde_json::to_string("error").expect("a string literal"),
            serde_json::to_string(msg).context("serializing the settings error")?,
            // `Settings` always serializes as a non-empty object, so there is a
            // `{` to strip and a field to separate from with the comma above.
            body.strip_prefix('{').unwrap_or(&body)
        ),
        None => body,
    })
}

/// Apply a `POST /settings` body on top of what is stored.
///
/// A patch rather than a whole document, and that is load-bearing: the form
/// leaves out any box it cannot read a number from, so "absent" has to mean
/// **keep what is there**. Deserializing the body on its own would make it mean
/// "reset to the built-in" — so an emptied Steps box would quietly put 30 back,
/// which is the form inventing an answer nobody gave. It is also what lets a
/// page that predates a knob save without wiping it.
///
/// An explicit `null` still clears a field, which is how the Refine/hi-res CFG
/// box and Weight type say "unset" as opposed to "unchanged".
pub fn patch(base: &Settings, body: &[u8]) -> Result<Settings> {
    let over: serde_json::Map<String, serde_json::Value> =
        serde_json::from_slice(body).context("parsing settings body")?;
    let mut merged = match serde_json::to_value(base).context("reading current settings")? {
        serde_json::Value::Object(m) => m,
        // `Settings` is a struct, so it is always an object.
        other => bail!("settings are not an object: {other}"),
    };
    merged.extend(over);
    let mut settings: Settings = serde_json::from_value(serde_json::Value::Object(merged))
        .context("reading settings body")?;
    // Canonicalised on the way in rather than on the way out, so what is
    // validated, what is written and what the form gets back are the same
    // thing: names lowercased, words trimmed, blanks dropped.
    settings.variables = variables::normalize(&settings.variables)?;
    Ok(settings)
}

/// File name of the per-model overrides, inside the same user dir.
///
/// Not `models.toml`, which is what the plan first called it: the root one is
/// the hand-written startup config, and two files of one name with two
/// different owners is a mistake waiting to be made in a text editor.
const MODELS_FILE: &str = "model-settings.toml";
/// Where [`ModelSettings::reconcile`] parks the entries of a model that is gone.
const PARKED_FILE: &str = "model-settings.removed.toml";

const PARKED_HEADER: &str = "\
# yasdig — per-model defaults for models that weren't there at the last start.
# Kept rather than deleted: a model moved out for a while gets its settings back
# on the first start that finds it again. Delete an entry, or the file, to drop
# them for good. Rewritten whole on every start that moves something.

";

const MODELS_HEADER: &str = "\
# yasdig — per-model defaults. Written by the Image tab's \"Save as defaults
# for this model\" and the Settings tab's Per model section; pressing either
# rewrites the whole file.
#
# Each [model.\"<name>\"] table overrides one model and nothing else, and only
# what differs from where that model would otherwise start is kept — so a
# change to your settings, or to the model's family defaults, still reaches
# every knob not named here.

";

/// `user/model-settings.toml`: what one model starts at, where that differs
/// from what it would start at anyway.
///
/// The last layer in [`crate::config::Config::registry`]: settings, the
/// family's recipe, a `[templates.*]` entry, then this. It exists because a
/// header can't tell an SDXL from an SDXL-Turbo, or a Z-Image turbo from its
/// base — same tensors, different training — so the difference has to be
/// something a person says, and saying it should not need a text editor.
///
/// Keyed by registry name, which for a discovered model is its file stem. A
/// renamed file therefore loses its entry, and every start parks entries that
/// no longer match a model in `user/model-settings.removed.toml` — see
/// [`ModelSettings::reconcile`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelSettings {
    pub model: std::collections::BTreeMap<String, PartialSettings>,
    /// The separate files picked for a model in place of the ones the scan
    /// would choose, `[parts."<name>"]`: slot key (`vae`, `llm`) to a path
    /// relative to the models dir. A table beside `model` rather than inside
    /// each entry, so an entry stays a plain [`PartialSettings`] and
    /// [`update_model`] never sees a key it has to refuse. Declared after
    /// `model`, which TOML doesn't care about here — both are tables.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub parts: std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
}

impl ModelSettings {
    pub fn path(user_dir: &Path) -> PathBuf {
        user_dir.join(MODELS_FILE)
    }

    /// Read it. A missing file is an empty one — the ordinary state, since
    /// nothing is written until somebody saves a model's defaults.
    pub fn load(user_dir: &Path) -> Result<ModelSettings> {
        Self::read(&Self::path(user_dir))
    }

    fn read(path: &Path) -> Result<ModelSettings> {
        if !path.exists() {
            return Ok(ModelSettings::default());
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn parked_path(user_dir: &Path) -> PathBuf {
        user_dir.join(PARKED_FILE)
    }

    /// Move the entries of models that no longer exist out of the live file,
    /// and back in for models that exist again. Run once per server start.
    ///
    /// **Parked, not deleted**, in `user/model-settings.removed.toml`. An entry
    /// is keyed by file stem, so "the model is gone" is indistinguishable from
    /// "the file is in another folder for an afternoon" — which is exactly what
    /// happens when you try a release in a fresh folder. Deleting would make
    /// that afternoon cost every tuned model its settings; parking makes it
    /// cost nothing, because the first start that finds the file again puts
    /// the entry back. An entry already live wins over a parked one of the same
    /// name: it was saved more recently.
    ///
    /// Returns (parked, restored) names, for the start-up log.
    pub fn reconcile(
        user_dir: &Path,
        exists: impl Fn(&str) -> bool,
    ) -> Result<(Vec<String>, Vec<String>)> {
        let mut live = Self::load(user_dir)?;
        let parked_path = Self::parked_path(user_dir);
        let mut parked = Self::read(&parked_path)?;

        let gone: std::collections::BTreeSet<String> =
            live.model.keys().chain(live.parts.keys()).filter(|n| !exists(n)).cloned().collect();
        let back: std::collections::BTreeSet<String> =
            parked.model.keys().chain(parked.parts.keys()).filter(|n| exists(n)).cloned().collect();
        if gone.is_empty() && back.is_empty() {
            return Ok((vec![], vec![]));
        }
        for name in &gone {
            if let Some(e) = live.model.remove(name) {
                parked.model.insert(name.clone(), e);
            }
            if let Some(p) = live.parts.remove(name) {
                parked.parts.insert(name.clone(), p);
            }
        }
        for name in &back {
            if let Some(e) = parked.model.remove(name) {
                live.model.entry(name.clone()).or_insert(e);
            }
            if let Some(p) = parked.parts.remove(name) {
                live.parts.entry(name.clone()).or_insert(p);
            }
        }
        // The parked file first: if the second write fails, an entry is in
        // both files rather than in neither.
        if parked == ModelSettings::default() {
            let _ = std::fs::remove_file(&parked_path);
        } else {
            parked.write(&parked_path, PARKED_HEADER)?;
        }
        live.save(user_dir)?;
        Ok((gone.into_iter().collect(), back.into_iter().collect()))
    }

    /// Read it, or nothing. The same decision [`Settings::load_or_default`]
    /// makes for the same reason, and reported in the same two places: the
    /// Settings tab and `doctor`.
    pub fn load_or_default(user_dir: &Path) -> ModelSettings {
        Self::load(user_dir).unwrap_or_default()
    }

    /// Write it whole, through a rename, as [`Settings::save`] does.
    pub fn save(&self, user_dir: &Path) -> Result<()> {
        self.write(&Self::path(user_dir), MODELS_HEADER)
    }

    fn write(&self, path: &Path, header: &str) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let body = format!(
            "{header}{}",
            toml::to_string_pretty(self).context("serializing model settings")?
        );
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, body).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }
}

/// Fold a `POST /model-settings` body into one model's overrides.
///
/// `values` maps a knob to a value or to `null`. A key that is absent is left
/// alone, `null` removes the override, and a value sets it — the patch rules
/// [`patch`] uses, for the reason it gives.
///
/// Then **everything equal to `base` is dropped**, `base` being what the model
/// starts at without this layer. That is what makes "Save as defaults for this
/// model" safe to press with the whole Advanced panel in the body: only the
/// boxes that differ become pins, so raising a global setting later still
/// reaches every knob this model has no opinion about. Pinning all of them
/// would quietly cut the model off from the Settings tab.
///
/// Compared after a round trip through the typed struct, not as raw JSON: the
/// page sends `6.3` and an `f32` serializes as `6.300000190734863`, which would
/// otherwise never compare equal and would pin every CFG it touched.
pub fn update_model(
    over: &PartialSettings,
    base: &Settings,
    values: &serde_json::Map<String, serde_json::Value>,
) -> Result<PartialSettings> {
    let base_v = match serde_json::to_value(base).context("reading the model's defaults")? {
        serde_json::Value::Object(m) => m,
        other => bail!("settings are not an object: {other}"),
    };
    let mut m = match serde_json::to_value(over).context("reading the model's overrides")? {
        serde_json::Value::Object(m) => m,
        other => bail!("overrides are not an object: {other}"),
    };
    for (k, v) in values {
        // Only the per-model knobs. `Settings` also carries the page's own
        // defaults (prompt, variables), and serde would drop an unknown key
        // silently — so a typo would read as saved and do nothing.
        if !base_v.contains_key(k) || PAGE_ONLY.contains(&k.as_str()) {
            bail!("'{k}' is not a setting a model can override");
        }
        if v.is_null() {
            m.remove(k);
        } else {
            m.insert(k.clone(), v.clone());
        }
    }
    let typed: PartialSettings = serde_json::from_value(serde_json::Value::Object(m))
        .context("reading the model's settings")?;
    let mut m = match serde_json::to_value(&typed)? {
        serde_json::Value::Object(m) => m,
        other => bail!("overrides are not an object: {other}"),
    };
    m.retain(|k, v| base_v.get(k) != Some(v));
    let out: PartialSettings = serde_json::from_value(serde_json::Value::Object(m))?;
    base.merged_with(&out).validate()?;
    Ok(out)
}

/// [`Settings::max_image_side`] out of the box. wgpu's own default texture
/// limit, and about where a result stops displaying comfortably in the page.
pub const DEFAULT_MAX_IMAGE_SIDE: u32 = 8192;

/// No GPU wgpu runs on goes past this (Vulkan's largest `maxImageDimension2D`
/// in practice is 16384, some report 32768), so a bigger number is a typo.
pub const MAX_IMAGE_SIDE_CEILING: u32 = 32768;

/// The fields of [`Settings`] that are the page's rather than a model's.
const PAGE_ONLY: &[&str] = &[
    "prompt",
    "negative",
    "hires_scale",
    "variables",
    "clear_workitems_on_close",
    "clear_outputs_on_close",
    "clear_videos_on_close",
    "max_image_side",
    "gpu_reserve_gb",
    "reference_size",
    "vae_conv_direct",
    "nightly_video",
    "embed_png_settings",
];

/// The most of the card [`Settings::gpu_reserve_gb`] may keep free. Past this
/// a 24 GB card has too little left to draw with, and a smaller one none.
pub const MAX_GPU_RESERVE_GB: f32 = 16.0;

/// `GET /model-settings`: every model's base and overrides.
///
/// The base is served so the Settings tab can show what an *empty* box means
/// — its placeholder — without the page re-deriving the layering, which is the
/// `frame_grid` rule: one implementation of a rule, on the side that runs it.
pub fn models_json(
    entries: &[crate::config::ModelEntry],
    tuned: &ModelSettings,
    parts: &std::collections::BTreeMap<String, Vec<crate::config::PartView>>,
    note: Option<&str>,
) -> Result<String> {
    let models: Vec<serde_json::Value> = entries
        .iter()
        .map(|e| {
            let mut base = match serde_json::to_value(&e.base)? {
                serde_json::Value::Object(m) => m,
                other => bail!("settings are not an object: {other}"),
            };
            base.retain(|k, _| !PAGE_ONLY.contains(&k.as_str()));
            Ok(serde_json::json!({
                "name": e.name,
                "family_label": e.family.label(),
                "base": base,
                "over": tuned.model.get(&e.name).cloned().unwrap_or_default(),
                // The slots, as `/models` serves them, and which of them are
                // saved choices rather than the scan's own pick.
                "parts": parts.get(&e.name).cloned().unwrap_or_default(),
                "parts_over": tuned.parts.get(&e.name).cloned().unwrap_or_default(),
            }))
        })
        .collect::<Result<_>>()?;
    let mut body = serde_json::json!({ "models": models });
    if let Some(msg) = note {
        body["error"] = serde_json::Value::String(msg.to_string());
    }
    Ok(body.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Settings page marks a row `page: true` exactly when the server
    /// refuses that key for a model. The per-model card hides marked rows and
    /// sends every other one, so a missing mark makes every per-model save
    /// fail — which a hand-kept second list did, from `max_image_side` on.
    #[test]
    fn fields_mark_exactly_the_page_only_keys() {
        let page = crate::web::lookup("/js/settings.js").expect("settings.js is served");
        let js = std::str::from_utf8(page.bytes).unwrap();
        let mut seen = 0;
        for line in js.lines() {
            let Some(rest) = line.split_once("{ key: '").map(|(_, r)| r) else { continue };
            let key = rest.split('\'').next().unwrap();
            let marked = line.contains("page: true");
            assert_eq!(marked, PAGE_ONLY.contains(&key), "settings.js row {key}: page mark disagrees with PAGE_ONLY");
            seen += 1;
        }
        assert!(seen > 20, "read the FIELDS rows ({seen})");
        for key in PAGE_ONLY {
            assert!(js.contains(&format!("{{ key: '{key}', page: true")), "{key} has no row marked page: true");
        }
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "yasdig-settings-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A model moved out for one start and back for the next loses nothing:
    /// its entry leaves the live file, waits in the parked one, and returns.
    /// And a live entry beats a parked one of the same name, being newer.
    #[test]
    fn a_missing_model_s_settings_are_parked_and_come_back() {
        let dir = tmpdir("reconcile");
        let mut s = ModelSettings::default();
        let tuned = PartialSettings { steps: Some(12), ..Default::default() };
        s.model.insert("kept".into(), tuned.clone());
        s.model.insert("moved".into(), tuned.clone());
        s.parts.insert("moved".into(), [("vae".into(), "vae/x.safetensors".into())].into());
        s.save(&dir).unwrap();

        let (parked, back) = ModelSettings::reconcile(&dir, |n| n == "kept").unwrap();
        assert_eq!((parked, back), (vec!["moved".to_string()], vec![]));
        let live = ModelSettings::load(&dir).unwrap();
        assert!(live.model.contains_key("kept") && !live.model.contains_key("moved"));
        assert!(live.parts.is_empty());

        // Nothing to move: the files are left alone.
        assert_eq!(ModelSettings::reconcile(&dir, |n| n == "kept").unwrap(), (vec![], vec![]));

        let (parked, back) = ModelSettings::reconcile(&dir, |_| true).unwrap();
        assert_eq!((parked, back), (vec![], vec!["moved".to_string()]));
        assert_eq!(ModelSettings::load(&dir).unwrap(), s);
        assert!(!ModelSettings::parked_path(&dir).exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The whole point of the overlay: a preset for a new architecture says what
    /// is different about it and inherits the rest, so the Settings tab means
    /// something for a templated model too.
    #[test]
    fn a_template_only_overrides_what_it_names() {
        let base = Settings {
            width: 1024,
            refine_strength: 0.12,
            sampler: "dpm++2m".to_string(),
            ..Settings::default()
        };

        let over: PartialSettings = toml::from_str("steps = 8\ncfg_scale = 1.0\n").unwrap();
        let m = base.merged_with(&over);

        assert_eq!(m.steps, 8);
        assert_eq!(m.cfg_scale, 1.0);
        // Everything it didn't name comes from the settings, not from the
        // built-ins — 30 steps and "euler" are what a `..Default` would leak.
        assert_eq!(m.width, 1024);
        assert_eq!(m.refine_strength, 0.12);
        assert_eq!(m.sampler, "dpm++2m");
    }

    /// A file written before the scheduler existed reads as Auto — what those
    /// images ran with — and a model can pin Auto back over a global choice,
    /// which an `Option` (the `upscaler` shape) could not say.
    /// The largest image is the page's, not a model's: it round-trips through
    /// the file, is checked for sense, and a per-model entry may not set it.
    #[test]
    fn the_largest_image_side_is_a_page_setting() {
        let old: Settings = toml::from_str("steps = 20\n").unwrap();
        assert_eq!(old.max_image_side, DEFAULT_MAX_IMAGE_SIDE);
        let big = Settings { max_image_side: 16384, ..Settings::default() };
        let back: Settings = toml::from_str(&toml::to_string_pretty(&big).unwrap()).unwrap();
        assert_eq!(back.max_image_side, 16384);
        assert!(big.validate().is_ok());
        assert!(Settings { max_image_side: 512, ..Settings::default() }.validate().is_err());
        assert!(Settings { max_image_side: 65536, ..Settings::default() }.validate().is_err());
        assert!(PAGE_ONLY.contains(&"max_image_side"));
        // The card reserve: page-level, off by default, bounded, round-trips.
        assert!(PAGE_ONLY.contains(&"gpu_reserve_gb"));
        assert_eq!(Settings::default().gpu_reserve_gb, 0.0);
        let r = Settings { gpu_reserve_gb: 3.0, ..Settings::default() };
        assert_eq!(toml::from_str::<Settings>(&toml::to_string_pretty(&r).unwrap()).unwrap().gpu_reserve_gb, 3.0);
        assert!(Settings { gpu_reserve_gb: -1.0, ..Settings::default() }.validate().is_err());
        assert!(Settings { gpu_reserve_gb: 20.0, ..Settings::default() }.validate().is_err());
        // The reference size: page-level, 512 by default, 0 or a sane side.
        assert!(PAGE_ONLY.contains(&"reference_size"));
        assert_eq!(Settings::default().reference_size, 512);
        assert!(Settings { reference_size: 0, ..Settings::default() }.validate().is_ok());
        assert!(Settings { reference_size: 100, ..Settings::default() }.validate().is_err());
        assert!(PAGE_ONLY.contains(&"vae_conv_direct") && Settings::default().vae_conv_direct);
        // Video is a nightly feature: off unless asked for, page-level.
        assert!(PAGE_ONLY.contains(&"nightly_video") && !Settings::default().nightly_video);
        assert!(PAGE_ONLY.contains(&"embed_png_settings") && Settings::default().embed_png_settings);
        let on = Settings { nightly_video: true, ..Settings::default() };
        assert!(toml::from_str::<Settings>(&toml::to_string_pretty(&on).unwrap()).unwrap().nightly_video);
    }

    #[test]
    fn the_step_cache_defaults_off_and_a_model_can_pin_it_back_off() {
        let old: Settings = toml::from_str("steps = 20\n").unwrap();
        assert_eq!(old.cache_mode, crate::config::CACHE_OFF);

        let global = Settings { cache_mode: "spectrum".into(), ..Settings::default() };
        let back: Settings = toml::from_str(&toml::to_string_pretty(&global).unwrap()).unwrap();
        assert_eq!(back.cache_mode, "spectrum");
        assert_eq!(global.as_profile("m").cache_mode, "spectrum");

        let pin: PartialSettings = toml::from_str("cache_mode = \"off\"\n").unwrap();
        assert_eq!(global.merged_with(&pin).cache_mode, "off");
        assert_eq!(global.merged_with(&PartialSettings::default()).cache_mode, "spectrum");
    }

    #[test]
    fn the_scheduler_defaults_to_auto_and_auto_can_be_pinned() {
        let old: Settings = toml::from_str("steps = 20\nsampler = \"euler\"\n").unwrap();
        assert_eq!(old.scheduler, crate::config::AUTO_SCHEDULER);

        let global = Settings { scheduler: "karras".into(), ..Settings::default() };
        let back: Settings = toml::from_str(&toml::to_string_pretty(&global).unwrap()).unwrap();
        assert_eq!(back.scheduler, "karras");

        let pin: PartialSettings = toml::from_str("scheduler = \"auto\"\n").unwrap();
        assert_eq!(global.merged_with(&pin).scheduler, "auto");
        assert_eq!(global.merged_with(&PartialSettings::default()).scheduler, "karras");
    }

    /// An empty overlay has to be exactly the settings, because that is the
    /// discovered-checkpoint path — every model with no template goes through
    /// `merged_with` too, and a field the merge forgot would show up here.
    #[test]
    fn an_empty_overlay_changes_nothing() {
        let base = Settings::default();
        assert_eq!(base.merged_with(&PartialSettings::default()), base);
    }

    #[test]
    fn settings_round_trip_through_the_file() {
        let dir = tmpdir("roundtrip");
        let s = Settings {
            prompt: "a crab # not a comment".to_string(),
            steps: 7,
            refine_cfg_scale: Some(2.5),
            extra_args: vec!["--diffusion-fa".to_string()],
            ..Settings::default()
        };
        s.save(&dir).unwrap();
        assert_eq!(Settings::load(&dir).unwrap(), s);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The upgrade path. `models.toml` shipped `upscale_factor = 2.0` where the
    /// built-in is 1.5, so seeding is what stops an existing install being
    /// silently retuned the first time it runs a build with this in it.
    #[test]
    fn a_missing_file_is_seeded_from_the_old_defaults_block() {
        let dir = tmpdir("seed");
        let old: PartialSettings =
            toml::from_str("upscale_factor = 2.0\nrefine_strength = 0.05\n").unwrap();
        assert!(Settings::ensure(&dir, Some(&old)).unwrap().is_some());

        let s = Settings::load(&dir).unwrap();
        assert_eq!(s.upscale_factor, 2.0);
        assert_eq!(s.refine_strength, 0.05);
        // Untouched keys still come from the built-ins.
        assert_eq!(s.steps, Settings::default().steps);

        // Second call is a no-op: the file is the truth once it exists.
        assert!(Settings::ensure(&dir, Some(&old)).unwrap().is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Validation runs before the write, so a form with a bad number in it
    /// cannot leave the file worse than it found it.
    #[test]
    fn bad_settings_are_rejected() {
        for bad in [
            Settings { refine_strength: 1.5, ..Settings::default() },
            Settings { width: 0, ..Settings::default() },
            Settings { hires_scale: 9.0, ..Settings::default() },
            Settings { cfg_scale: -1.0, ..Settings::default() },
            Settings { steps: 0, ..Settings::default() },
        ] {
            assert!(bad.validate().is_err(), "{bad:?} should not validate");
        }
        assert!(Settings::default().validate().is_ok());
    }

    /// The header is a comment, so it must survive a round trip as *nothing*.
    /// If it ever stopped parsing, every save would produce a file the next
    /// start refuses.
    #[test]
    fn the_header_is_only_a_comment() {
        let dir = tmpdir("header");
        Settings::default().save(&dir).unwrap();
        let text = std::fs::read_to_string(Settings::path(&dir)).unwrap();
        assert!(text.starts_with("# yasdig"));
        assert_eq!(Settings::load(&dir).unwrap(), Settings::default());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The `[variables]` table has to survive the trip, and this is also the
    /// test that fails if a scalar field is ever declared after it: TOML has no
    /// way back out of a table, so `to_string_pretty` would refuse to write the
    /// file at all.
    #[test]
    fn variables_round_trip_through_the_file() {
        let dir = tmpdir("variables");
        let s = Settings {
            steps: 9,
            variables: Variables::from([
                ("animal".to_string(), vec!["crab".to_string(), "horse".to_string()]),
                ("color".to_string(), vec!["red".to_string()]),
            ]),
            ..Settings::default()
        };
        s.save(&dir).unwrap();
        let text = std::fs::read_to_string(Settings::path(&dir)).unwrap();
        assert!(text.contains("[variables]"), "{text}");
        // Every value before the one table, which is the constraint above.
        assert!(text.find("[variables]") > text.find("steps = 9"), "{text}");
        assert_eq!(Settings::load(&dir).unwrap(), s);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A file with no table gets the defaults, which the default prompt needs.
    #[test]
    fn a_missing_variables_table_is_the_defaults() {
        let s: Settings = toml::from_str("steps = 12\n").unwrap();
        assert_eq!(s.variables, Settings::default().variables);
        assert!(s.prompt.contains("{animal}") && s.variables.contains_key("animal"));
        // And a table the page sent as `{}` clears the one that was stored.
        let stored = Settings {
            variables: Variables::from([("animal".to_string(), vec!["crab".to_string()])]),
            ..Settings::default()
        };
        let cleared = patch(&stored, br#"{"variables":{}}"#).unwrap();
        assert!(cleared.variables.is_empty());
    }

    /// Emptied on the page and saved, the table must not come back as the
    /// defaults on the next load — it has to be written, empty.
    #[test]
    fn emptied_variables_stay_empty() {
        let s = Settings { variables: Variables::new(), ..Settings::default() };
        let back: Settings = toml::from_str(&toml::to_string_pretty(&s).unwrap()).unwrap();
        assert!(back.variables.is_empty());
    }

    /// The page sends JSON; the disk holds TOML. Both have to describe the same
    /// struct or a save would rewrite fields the form never showed.
    #[test]
    fn the_wire_shape_and_the_file_shape_agree() {
        let s = Settings {
            negative: "blurry".to_string(),
            weight_type: Some("q8_0".to_string()),
            ..Settings::default()
        };
        let json = to_json(&s, None).unwrap();
        assert_eq!(patch(&Settings::default(), json.as_bytes()).unwrap(), s);
    }

    /// A broken settings file answers 200 with the built-ins *and* the reason,
    /// so the tab can fill an editable form and saving it is the repair. The
    /// splice is string surgery, so it is worth a test: a stray brace here
    /// would give the page a parse error instead of a message.
    #[test]
    fn a_read_error_rides_along_with_the_settings() {
        let s = Settings::default();
        let json = to_json(&s, Some("parsing settings.toml: expected \" at line 3")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(v["error"].as_str().unwrap().contains("line 3"));
        assert_eq!(v["steps"], s.steps);
        // And the ordinary answer carries no such key at all.
        let plain: serde_json::Value =
            serde_json::from_str(&to_json(&s, None).unwrap()).unwrap();
        assert!(plain.get("error").is_none());
    }

    /// The reason the save is a patch. A box the form couldn't read is left out
    /// of the body, and leaving it out has to mean "as it was" — reading the
    /// body as a whole document would put the built-in back instead.
    #[test]
    fn a_missing_key_keeps_the_stored_value_and_null_clears_it() {
        let stored = Settings {
            steps: 12,
            refine_cfg_scale: Some(2.5),
            ..Settings::default()
        };

        let kept = patch(&stored, br#"{"cfg_scale":4.0}"#).unwrap();
        assert_eq!(kept.steps, 12);
        assert_eq!(kept.cfg_scale, 4.0);
        assert_eq!(kept.refine_cfg_scale, Some(2.5));

        // An explicit null is how the box says "unset", as opposed to "unchanged".
        let cleared = patch(&stored, br#"{"refine_cfg_scale":null}"#).unwrap();
        assert_eq!(cleared.refine_cfg_scale, None);
        assert_eq!(cleared.steps, 12);
    }

    fn values(json: &str) -> serde_json::Map<String, serde_json::Value> {
        serde_json::from_str(json).unwrap()
    }

    /// The Image tab sends every box, and only the ones that differ from where
    /// the model starts become pins — including a CFG the page sends as `6.3`,
    /// which as an `f32` serializes as 6.300000190734863 and would never
    /// compare equal as raw JSON.
    #[test]
    fn a_model_keeps_only_what_differs_from_its_base() {
        let base = Settings {
            steps: 25,
            cfg_scale: 6.3,
            ..Settings::default()
        };
        let over = update_model(
            &PartialSettings::default(),
            &base,
            &values(r#"{"steps":12,"cfg_scale":6.3,"sampler":"euler","refine_cfg_scale":null}"#),
        )
        .unwrap();
        assert_eq!(
            over,
            PartialSettings {
                steps: Some(12),
                ..PartialSettings::default()
            }
        );
        // A later save that puts the box back to the base removes the pin, and
        // an absent key leaves the others alone.
        let over = update_model(
            &PartialSettings {
                steps: Some(12),
                cfg_scale: Some(1.0),
                ..PartialSettings::default()
            },
            &base,
            &values(r#"{"steps":25}"#),
        )
        .unwrap();
        assert_eq!((over.steps, over.cfg_scale), (None, Some(1.0)));
        // `null` clears.
        let over = update_model(&over, &base, &values(r#"{"cfg_scale":null}"#)).unwrap();
        assert_eq!(over, PartialSettings::default());
    }

    #[test]
    fn a_model_override_is_checked_like_a_setting() {
        let base = Settings::default();
        let none = PartialSettings::default();
        for bad in [
            r#"{"steps":0}"#,
            r#"{"refine_strength":1.0}"#,
            r#"{"stpes":12}"#,
            r#"{"prompt":"a cat"}"#,
            r#"{"variables":{}}"#,
        ] {
            assert!(update_model(&none, &base, &values(bad)).is_err(), "{bad}");
        }
    }

    /// Model names are file stems — dots, spaces, capitals — and each is a
    /// quoted table key. The file must read back what it wrote.
    #[test]
    fn model_settings_round_trip_through_the_file() {
        let dir = tmpdir("models");
        let mut m = ModelSettings::default();
        m.model.insert(
            "sd_xl_base_1.0".into(),
            PartialSettings {
                steps: Some(6),
                cfg_scale: Some(1.0),
                extra_args: Some(vec!["--offload-to-cpu".into()]),
                ..PartialSettings::default()
            },
        );
        m.model.insert(
            "hand 5.5".into(),
            PartialSettings {
                sampler: Some("dpm++2m".into()),
                ..PartialSettings::default()
            },
        );
        // A saved part, for a model with no saved numbers at all — the table
        // stands on its own.
        m.parts.insert(
            "juggernaut".into(),
            [("vae".to_string(), "vae/sdxl_vae.safetensors".to_string())].into_iter().collect(),
        );
        m.save(&dir).unwrap();
        assert_eq!(ModelSettings::load(&dir).unwrap(), m);
        // No file is no overrides, not an error.
        std::fs::remove_file(ModelSettings::path(&dir)).unwrap();
        assert_eq!(ModelSettings::load(&dir).unwrap(), ModelSettings::default());
    }
}
