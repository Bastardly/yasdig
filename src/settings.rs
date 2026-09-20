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
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// File name inside [`crate::config::Config::user_dir`].
const FILE: &str = "settings.toml";

/// Written above every save. It is the only comment in the file, because the
/// file is rewritten wholesale — anything else put here would be deleted by the
/// next press of Save, which is a worse outcome than not writing it.
const HEADER: &str = "\
# rustimage — your defaults. Written by the Settings tab; edit it by hand if you
# like, but note that pressing Save rewrites the whole file and any comments you
# add here go with it.
#
# These apply to every checkpoint discovered in the models dir. A
# [templates.<name>] block in models.toml overrides whichever of them it names,
# and inherits the rest from here. See README.md for what each field does.

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
    /// Where the hi-res pass's enlarge factor starts (`#runscale`/`#upfactor`).
    /// Not [`Settings::upscale_factor`], which is the FSR enlarge every base
    /// render goes through on its way to the output size.
    pub hires_scale: f32,

    // ---- What was `[defaults]` in models.toml, unchanged in meaning. ----
    pub width: u32,
    pub height: u32,
    pub steps: u32,
    pub cfg_scale: f32,
    /// CFG for the refine and hi-res passes. `None` = follow `cfg_scale`, and it
    /// stays an `Option` all the way onto [`Profile`] so raising the base CFG
    /// doesn't leave the escalations behind at whatever it used to be.
    pub img2img_cfg_scale: Option<f32>,
    pub sampler: String,
    pub upscale_factor: f32,
    pub sharpness: f32,
    pub clarity: f32,
    pub refine_steps: u32,
    pub refine_strength: f32,
    pub hires_steps: u32,
    pub hires_strength: f32,
    pub source_steps: u32,
    pub source_strength: f32,
    pub hires_max_side: u32,
    pub hires_flash_attn: bool,
    pub weight_type: Option<String>,
    pub extra_args: Vec<String>,
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
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PartialSettings {
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub steps: Option<u32>,
    #[serde(default)]
    pub cfg_scale: Option<f32>,
    /// Doubly optional on purpose: absent means "inherit", and an explicit
    /// `img2img_cfg_scale = ...` means "pin this". A template cannot express
    /// "unset it back to following cfg_scale" and doesn't need to — it can name
    /// `cfg_scale` instead.
    #[serde(default)]
    pub img2img_cfg_scale: Option<f32>,
    #[serde(default)]
    pub sampler: Option<String>,
    #[serde(default)]
    pub upscale_factor: Option<f32>,
    #[serde(default)]
    pub sharpness: Option<f32>,
    #[serde(default)]
    pub clarity: Option<f32>,
    #[serde(default)]
    pub refine_steps: Option<u32>,
    #[serde(default)]
    pub refine_strength: Option<f32>,
    #[serde(default)]
    pub hires_steps: Option<u32>,
    #[serde(default)]
    pub hires_strength: Option<f32>,
    #[serde(default)]
    pub source_steps: Option<u32>,
    #[serde(default)]
    pub source_strength: Option<f32>,
    #[serde(default)]
    pub hires_max_side: Option<u32>,
    #[serde(default)]
    pub hires_flash_attn: Option<bool>,
    #[serde(default)]
    pub weight_type: Option<String>,
    #[serde(default)]
    pub extra_args: Option<Vec<String>>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            prompt: "a lovely fluffy crab, cinematic lighting, sharp, in focus".to_string(),
            negative: "watermark, worst quality, low quality".to_string(),
            hires_scale: 2.0,
            // Tuned for standard (non-distilled) SDXL, since that is the common
            // discovered checkpoint. A turbo model wants a template (few steps,
            // CFG 1) or the per-request overrides in the Advanced panel.
            width: 768,
            height: 768,
            steps: 30,
            cfg_scale: 6.0,
            img2img_cfg_scale: None,
            sampler: "euler".to_string(),
            upscale_factor: 2.0,
            // Off. Sharpening is a live slider under the image, not something a
            // render arrives pre-seasoned with — so the first thing on screen is
            // what the model and the upscaler actually produced.
            sharpness: 0.0,
            clarity: 0.0,
            refine_steps: 0,
            refine_strength: 0.30,
            hires_steps: 0,
            // Same denoise as the refine: enough to resolve real detail at the
            // larger size, low enough that the composition doesn't drift.
            hires_strength: 0.30,
            // The Image-to-image tab's pass is a *transform*, not a finish, so it
            // starts far higher than either escalation: at 0.30 a source comes
            // back essentially unchanged and the tab looks broken. 0.55 keeps the
            // composition, the pose and the palette while redrawing the surface
            // in the model's own idiom. Steps are a floor, and `SdRunner::img2img`
            // raises them to `steps * strength` anyway.
            source_steps: 12,
            source_strength: 0.55,
            // Longest side the hi-res pass may diffuse at. SDXL's quality falls
            // off past this and the cost climbs quadratically; beyond it a final
            // FSR upscale covers the rest.
            hires_max_side: 2048,
            hires_flash_attn: true,
            weight_type: None,
            extra_args: Vec::new(),
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
            img2img_cfg_scale: over.img2img_cfg_scale.or(self.img2img_cfg_scale),
            sampler: over.sampler.clone().unwrap_or_else(|| self.sampler.clone()),
            upscale_factor: over.upscale_factor.unwrap_or(self.upscale_factor),
            sharpness: over.sharpness.unwrap_or(self.sharpness),
            clarity: over.clarity.unwrap_or(self.clarity),
            refine_steps: over.refine_steps.unwrap_or(self.refine_steps),
            refine_strength: over.refine_strength.unwrap_or(self.refine_strength),
            hires_steps: over.hires_steps.unwrap_or(self.hires_steps),
            hires_strength: over.hires_strength.unwrap_or(self.hires_strength),
            source_steps: over.source_steps.unwrap_or(self.source_steps),
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
        if !(64..=4096).contains(&self.width) || !(64..=4096).contains(&self.height) {
            bail!(
                "base size is {}×{}, must be between 64 and 4096",
                self.width,
                self.height
            );
        }
        if self.steps == 0 || self.steps > 100 {
            bail!("steps is {}, must be between 1 and 100", self.steps);
        }
        if !(1.0..=MAX_UPSCALE).contains(&self.upscale_factor) {
            bail!(
                "upscale_factor is {}, must be between 1 and {MAX_UPSCALE}",
                self.upscale_factor
            );
        }
        if !(1.0..=MAX_UPSCALE).contains(&self.hires_scale) {
            bail!(
                "hires_scale is {}, must be between 1 and {MAX_UPSCALE}",
                self.hires_scale
            );
        }
        if self.hires_max_side < 64 {
            bail!("hires_max_side is {}, must be 64 or more", self.hires_max_side);
        }
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
            img2img_cfg_scale: self.img2img_cfg_scale,
            sampler: self.sampler.clone(),
            upscale_factor: self.upscale_factor,
            sharpness: self.sharpness,
            clarity: self.clarity,
            refine_steps: self.refine_steps,
            refine_strength: self.refine_strength,
            hires_steps: self.hires_steps,
            hires_strength: self.hires_strength,
            source_steps: self.source_steps,
            source_strength: self.source_strength,
            hires_max_side: self.hires_max_side,
            hires_flash_attn: self.hires_flash_attn,
            weight_type: self.weight_type.clone(),
            extra_args: self.extra_args.clone(),
            // Request-only, like everywhere else a profile is built: a LoRA is
            // reached for per picture, so there is no key for one here.
            loras: Vec::new(),
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
    serde_json::from_value(serde_json::Value::Object(merged))
        .context("reading settings body")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rustimage-settings-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
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
            img2img_cfg_scale: Some(2.5),
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
        assert!(text.starts_with("# rustimage"));
        assert_eq!(Settings::load(&dir).unwrap(), Settings::default());
        std::fs::remove_dir_all(&dir).ok();
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
            img2img_cfg_scale: Some(2.5),
            ..Settings::default()
        };

        let kept = patch(&stored, br#"{"cfg_scale":4.0}"#).unwrap();
        assert_eq!(kept.steps, 12);
        assert_eq!(kept.cfg_scale, 4.0);
        assert_eq!(kept.img2img_cfg_scale, Some(2.5));

        // An explicit null is how the box says "unset", as opposed to "unchanged".
        let cleared = patch(&stored, br#"{"img2img_cfg_scale":null}"#).unwrap();
        assert_eq!(cleared.img2img_cfg_scale, None);
        assert_eq!(cleared.steps, 12);
    }
}
