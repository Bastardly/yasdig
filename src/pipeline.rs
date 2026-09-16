//! Per-request orchestration, with per-stage timing. Runs on the single GPU
//! worker thread so generation is serialized (one job touches the GPU at a time).
//!
//! Generation is deliberately the cheap part — sd.cpp txt2img at base resolution
//! plus an FSR upscale — and quality is escalated afterwards, on demand. Every
//! rung reads the retained *unsharpened* output-resolution image, writes a new
//! one, and RCAS runs once on the way to the browser:
//!
//! ```text
//!   run()     txt2img (base) ─▶ EASU upscale ──────────────▶ raw ─▶ RCAS ─▶ PNG
//!   refine()  img2img @ current res, no rescale ──────────▶ raw ─▶ RCAS ─▶ PNG
//!   hires()   EASU enlarge ─▶ img2img ─▶ EASU (if over budget) ─▶ raw ─▶ RCAS ─▶ PNG
//!   adjust()  (no diffusion at all) ───────────────────────────▶ raw ─▶ RCAS ─▶ PNG
//! ```
//!
//! Three consequences worth stating, because all three used to be otherwise. The
//! ladder is free-form: `refine` runs on whatever is on screen, so it can follow
//! a hi-res pass and can repeat. Sharpening never compounds — no pass ever
//! diffuses over pixels another pass has already sharpened. And because `raw` is
//! still exactly what the last diffusion produced, the display filters can be
//! re-run on it for free: that is `adjust`, and it is why they are a slider
//! rather than a reason to regenerate.

use crate::config::{Config, Profile};
use crate::fsr::Fsr;
use crate::sd::SdRunner;
use anyhow::{bail, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

static JOB_COUNTER: AtomicU64 = AtomicU64::new(0);

/// How many results stay escalatable. Older ones are evicted (files deleted)
/// so `work/` can't grow without bound.
const MAX_PENDING: usize = 8;

/// Hard limit on the hi-res upscale factor a request may ask for.
const MAX_UPSCALE: f32 = 4.0;

/// Above this longest side an img2img pass gets `--vae-tiling` (and flash
/// attention), which is what keeps a large pass inside VRAM. Below it the
/// tiling is pure cost — it has its own faint seam artifacts to pay for.
const BIG_IMAGE_SIDE: u32 = 1024;

#[derive(Debug, Clone)]
pub struct GenRequest {
    pub prompt: String,
    /// Name of a registry entry (template or discovered model).
    pub template: String,
    pub negative: Option<String>,
    /// Negative = pick one; otherwise use as-is.
    pub seed: i64,
    /// Optional per-request overrides (so one discovered model works for both
    /// Turbo and standard checkpoints without editing config).
    pub steps: Option<u32>,
    pub cfg_scale: Option<f32>,
    pub sampler: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Overrides for the escalation passes. Applied to the profile before it's
    /// retained, so a later `refine`/`hires` uses what was set at generate time.
    pub refine_steps: Option<u32>,
    pub refine_strength: Option<f32>,
    pub hires_steps: Option<u32>,
    pub hires_strength: Option<f32>,
    /// Display-time post-process, applied once to the finished image.
    /// Cheap enough to tune freely.
    pub sharpness: Option<f32>,
    pub clarity: Option<f32>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Timings {
    pub gen_ms: u128,
    pub fsr_ms: u128,
    pub refine_ms: u128,
    pub hires_ms: u128,
}

pub struct GenResult {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub seed: i64,
    pub timings: Timings,
    pub refine_passes: u32,
    pub hires_passes: u32,
    /// Handle for [`Engine::refine`] / [`Engine::hires`] / [`Engine::adjust`].
    pub image_id: u64,
    pub can_refine: bool,
    pub can_hires: bool,
    /// The display filters currently applied, so the UI reflects what the image
    /// actually has rather than what its controls last said.
    pub sharpness: f32,
    pub clarity: f32,
}

/// The intermediates of one result, kept on disk so quality can be escalated
/// without redoing the cheaper stages. Also holds everything the img2img passes
/// need — notably the profile with the per-request overrides already applied, so
/// a later pass matches the settings the image was generated with rather than
/// whatever the UI happens to show now.
#[derive(Clone)]
struct Pending {
    /// Output-resolution image with **no RCAS applied**. This is what every
    /// escalation reads: sharpening is a display filter, so no pass ever
    /// diffuses over pixels an earlier pass already sharpened.
    raw: PathBuf,
    raw_w: u32,
    raw_h: u32,
    /// The sharpened PNG that went to the browser — `raw` put through RCAS.
    /// Retained so re-sharpening at a new setting needn't redo the diffusion.
    display: PathBuf,
    profile: Profile,
    /// The display filters as they stand on *this* image. Deliberately not on
    /// `profile`: the profile's values are where the sliders start, and every
    /// new image — a fresh generate, a refine, a hi-res — starts there again.
    /// `adjust` moves these two and touches nothing else.
    ///
    /// Carrying them forward instead was the obvious thing and the wrong one.
    /// An escalation replaces the pixels, so the amount of sharpening that
    /// looked right is stale by definition, and inheriting it hides what the
    /// pass actually did underneath a filter chosen for the previous image.
    sharpness: f32,
    clarity: f32,
    prompt: String,
    negative: Option<String>,
    seed: i64,
    refine_passes: u32,
    hires_passes: u32,
    /// How many escalations this result has been through. Only used to keep
    /// each step's filenames distinct from the ones it supersedes — otherwise a
    /// pass would read and write the same path.
    step: u32,
}

impl Pending {
    /// Refine is deliberately *not* under `hires_max_side`. That ceiling exists
    /// because the hi-res pass has somewhere to fall back to — it can diffuse
    /// smaller and let FSR cover the rest. Refine has no such fallback: it runs
    /// at the size the image already is, or not at all. Gating it on the budget
    /// removed the button in exactly the state where it helps most, which is an
    /// image FSR has stretched past its last diffusion.
    ///
    /// The cost of that is real — a pass at 3072² is not cheap, and on a tight
    /// card it can run out of VRAM — but it's one click, the size is on screen
    /// above the button, and a failed pass leaves the image intact to retry.
    fn can_refine(&self) -> bool {
        self.profile.refine_steps > 0
    }

    /// A hi-res pass needs room to diffuse: once the image is past the budget
    /// there's no affordable pass left to run on it.
    fn can_hires(&self) -> bool {
        self.profile.hires_steps > 0
            && self.raw_w.max(self.raw_h) <= self.profile.hires_max_side
    }

    /// Whether an img2img pass at this size needs the large-image memory flags.
    fn big_image(&self) -> bool {
        self.raw_w.max(self.raw_h) > BIG_IMAGE_SIDE
    }

    fn remove_files(&self) {
        let _ = std::fs::remove_file(&self.raw);
        let _ = std::fs::remove_file(&self.display);
    }
}

pub struct Engine {
    cfg: Config,
    fsr: Fsr,
    work_dir: PathBuf,
    registry: BTreeMap<String, Profile>,
    /// Escalatable results, keyed by job id (ordered, so the oldest evicts first).
    pending: BTreeMap<u64, Pending>,
}

impl Engine {
    pub fn new(cfg: Config) -> Result<Engine> {
        let work_dir = cfg.base_dir.join("work");
        std::fs::create_dir_all(&work_dir).ok();
        // Job ids restart at 0 each run, so clear any PNGs a previous run left
        // behind rather than letting a new job collide with them.
        purge_pngs(&work_dir);
        let fsr = Fsr::new()?;
        // Fail fast with a friendly message if the sd binary is missing.
        SdRunner::new(&cfg).preflight()?;
        let registry = cfg.registry();
        Ok(Engine {
            cfg,
            fsr,
            work_dir,
            registry,
            pending: BTreeMap::new(),
        })
    }

    pub fn adapter_name(&self) -> &str {
        self.fsr.adapter_name()
    }

    pub fn run(&mut self, req: &GenRequest) -> Result<GenResult> {
        let mut profile = match self.registry.get(&req.template) {
            Some(p) => p.clone(),
            None => bail!("unknown model '{}'", req.template),
        };
        // Apply per-request overrides onto the profile.
        if let Some(v) = req.steps {
            profile.steps = v;
        }
        if let Some(v) = req.cfg_scale {
            profile.cfg_scale = v;
        }
        if let Some(v) = &req.sampler {
            if !v.is_empty() {
                profile.sampler = v.clone();
            }
        }
        if let Some(v) = req.width {
            profile.width = v;
        }
        if let Some(v) = req.height {
            profile.height = v;
        }
        if let Some(v) = req.refine_steps {
            profile.refine_steps = v;
        }
        if let Some(v) = req.refine_strength {
            profile.refine_strength = v;
        }
        if let Some(v) = req.hires_steps {
            profile.hires_steps = v;
        }
        if let Some(v) = req.hires_strength {
            profile.hires_strength = v;
        }
        if let Some(v) = req.sharpness {
            profile.sharpness = v;
        }
        if let Some(v) = req.clarity {
            profile.clarity = v;
        }
        // The overrides can push the denoise strengths out of range even though
        // the profile was fine at load, and this profile is what the later
        // refine/hi-res passes run with.
        profile.validate()?;
        let tpl = &profile;

        if !profile.exists {
            bail!(
                "checkpoint for '{}' not found: {}",
                profile.name,
                profile.model.display()
            );
        }
        if req.prompt.trim().is_empty() {
            bail!("prompt is empty");
        }

        let seed = if req.seed < 0 { random_seed() } else { req.seed };
        let id = JOB_COUNTER.fetch_add(1, Ordering::Relaxed);
        let lowres = self.work_dir.join(format!("{id}-0-base.png"));
        let raw = self.work_dir.join(format!("{id}-0-raw.png"));
        let display = self.work_dir.join(format!("{id}-0-full.png"));

        let sd = SdRunner::new(&self.cfg);
        let neg = req.negative.as_deref();

        // 1) Distilled generation at base resolution.
        let t = Instant::now();
        sd.txt2img(tpl, &req.prompt, neg, seed, &lowres)?;
        let gen_ms = t.elapsed().as_millis();

        // 2) EASU upscale, unsharpened — cheap, and enough on its own for a
        //    draft. Quality is escalated afterwards via `refine` and `hires`,
        //    both of which read this image rather than the sharpened one.
        let t = Instant::now();
        self.fsr
            .process(&lowres, &raw, tpl.upscale_factor, 0.0, 0.0)?;
        // Nothing reads the base render once it has been upscaled: every
        // escalation works at output resolution now.
        let _ = std::fs::remove_file(&lowres);

        // 3) Sharpen for display.
        let (png, w, h) = self.publish(&raw, &display, tpl.sharpness, tpl.clarity)?;
        let fsr_ms = t.elapsed().as_millis();

        // Retain the intermediates so the next step up the ladder doesn't have
        // to re-run this one.
        let entry = Pending {
            raw,
            raw_w: w,
            raw_h: h,
            display,
            sharpness: profile.sharpness,
            clarity: profile.clarity,
            profile: profile.clone(),
            prompt: req.prompt.clone(),
            negative: req.negative.clone(),
            seed,
            refine_passes: 0,
            hires_passes: 0,
            step: 0,
        };
        let result = self.result(id, &entry, png, Timings { gen_ms, fsr_ms, ..Default::default() });
        self.store(id, entry);
        Ok(result)
    }

    /// Low-denoise img2img on the current image, at its current resolution and
    /// with no rescale of any kind. That makes it the finishing rung: it can
    /// follow a hi-res pass, it can repeat, and unlike the hi-res pass it has no
    /// size ceiling (see [`Pending::can_refine`]).
    ///
    /// It used to run on the retained base render and re-upscale afterwards,
    /// which is why it read as soft. Every img2img round-trips through the VAE,
    /// which costs fine detail no matter how low the strength; doing that at
    /// base resolution and then stretching the result 2× means the loss lands on
    /// detail while only the structural gain survives the upscale. At output
    /// resolution there is nothing downstream to smear what the pass adds.
    pub fn refine(&mut self, id: u64) -> Result<GenResult> {
        let p = self.lookup(id, "refine")?;
        if !p.can_refine() {
            bail!("'{}' has no refine pass configured (refine_steps = 0)", p.profile.name);
        }

        let step = p.step + 1;
        let raw = self.work_dir.join(format!("{id}-{step}-raw.png"));
        let display = self.work_dir.join(format!("{id}-{step}-full.png"));
        let sd = SdRunner::new(&self.cfg);

        let t = Instant::now();
        sd.img2img(
            &p.profile,
            &p.prompt,
            p.negative.as_deref(),
            p.seed,
            &p.raw,
            &raw,
            p.profile.refine_strength,
            p.profile.refine_steps,
            p.big_image(),
            "img2img refine",
        )?;
        let refine_ms = t.elapsed().as_millis();

        let t = Instant::now();
        // Dimensions come back from here rather than carrying over: sd.cpp
        // rounds an init image to what the latent grid allows, so the pass can
        // return a slightly different size than it was given.
        let (png, w, h) = self.publish(&raw, &display, p.profile.sharpness, p.profile.clarity)?;
        let fsr_ms = t.elapsed().as_millis();

        // Only now that it succeeded — a failure above leaves the entry intact
        // so the user can retry.
        p.remove_files();
        let entry = Pending {
            raw,
            raw_w: w,
            raw_h: h,
            display,
            // Back to the model's starting point — see `Pending::sharpness`.
            sharpness: p.profile.sharpness,
            clarity: p.profile.clarity,
            refine_passes: p.refine_passes + 1,
            step,
            ..p
        };
        let result = self.result(id, &entry, png, Timings { fsr_ms, refine_ms, ..Default::default() });
        self.store(id, entry);
        Ok(result)
    }

    /// The hi-res pass: diffuse at the output resolution to add detail the FSR
    /// upscale can't invent, optionally enlarging on the way.
    ///
    /// Diffusion cost climbs quadratically with pixel count and SDXL's quality
    /// falls off past `hires_max_side`, while FSR is nearly free at any size —
    /// so an upscale beyond the budget diffuses as large as it can afford, then
    /// covers the remaining distance with a plain FSR pass.
    pub fn hires(&mut self, id: u64, upscale: f32) -> Result<GenResult> {
        if !(1.0..=MAX_UPSCALE).contains(&upscale) {
            bail!("upscale must be between 1 and {MAX_UPSCALE}");
        }
        let p = self.lookup(id, "run a hi-res pass on")?;
        if p.profile.hires_steps == 0 {
            bail!("'{}' has no hi-res pass configured (hires_steps = 0)", p.profile.name);
        }
        if !p.can_hires() {
            bail!(
                "{}×{} is already past the hi-res budget of {} px — there's no \
                 affordable enlarge left to run on it. Refine still works at this \
                 size.",
                p.raw_w,
                p.raw_h,
                p.profile.hires_max_side
            );
        }

        let target_w = ((p.raw_w as f32) * upscale).round() as u32;
        let target_h = ((p.raw_h as f32) * upscale).round() as u32;
        // Largest size the diffusion pass may run at, snapped to a multiple of
        // 64 so the latent dimensions stay clean. Only the width is snapped:
        // FSR scales both axes by one factor, so the height follows from it.
        let budget = p.profile.hires_max_side.min(target_w.max(target_h));
        let diff_scale = budget as f32 / p.raw_w.max(p.raw_h) as f32;
        let diff_w = snap64(((p.raw_w as f32) * diff_scale).round() as u32);

        let step = p.step + 1;
        let upscaled = self.work_dir.join(format!("{id}-{step}-up.png"));
        let hires = self.work_dir.join(format!("{id}-{step}-hires.png"));
        let raw = self.work_dir.join(format!("{id}-{step}-raw.png"));
        let display = self.work_dir.join(format!("{id}-{step}-full.png"));
        let sd = SdRunner::new(&self.cfg);
        let mut fsr_ms = 0u128;

        // 1) Enlarge to the diffusion size, if that's bigger than what we have.
        //    Unsharpened, like everything that feeds a diffusion pass.
        let init = if diff_w > p.raw_w {
            let t = Instant::now();
            self.fsr.process(
                &p.raw,
                &upscaled,
                diff_w as f32 / p.raw_w as f32,
                0.0,
                0.0,
            )?;
            fsr_ms += t.elapsed().as_millis();
            upscaled.clone()
        } else {
            p.raw.clone()
        };

        // 2) The detail pass itself.
        let t = Instant::now();
        sd.img2img(
            &p.profile,
            &p.prompt,
            p.negative.as_deref(),
            p.seed,
            &init,
            &hires,
            p.profile.hires_strength,
            p.profile.hires_steps,
            // `budget` is the longest side the diffusion actually runs at, which
            // is what decides whether it needs the large-image flags — not the
            // size we started from.
            budget > BIG_IMAGE_SIDE,
            "img2img hi-res",
        )?;
        let hires_ms = t.elapsed().as_millis();

        // 3) Cover any distance the budget wouldn't stretch to, still
        //    unsharpened — this is the new retained image, and a later pass may
        //    diffuse over it.
        let t = Instant::now();
        let scale = if target_w > diff_w {
            target_w as f32 / diff_w as f32
        } else {
            1.0
        };
        self.fsr.process(&hires, &raw, scale, 0.0, 0.0)?;
        let _ = std::fs::remove_file(&hires);
        let _ = std::fs::remove_file(&upscaled);

        // 4) Sharpen for display.
        let (png, w, h) = self.publish(&raw, &display, p.profile.sharpness, p.profile.clarity)?;
        fsr_ms += t.elapsed().as_millis();

        p.remove_files();
        let entry = Pending {
            raw,
            raw_w: w,
            raw_h: h,
            display,
            // Back to the model's starting point — see `Pending::sharpness`.
            sharpness: p.profile.sharpness,
            clarity: p.profile.clarity,
            hires_passes: p.hires_passes + 1,
            step,
            ..p
        };
        let result = self.result(id, &entry, png, Timings { fsr_ms, hires_ms, ..Default::default() });
        self.store(id, entry);
        Ok(result)
    }

    /// Re-run the display filters on an existing result. No diffusion: this is
    /// [`Engine::publish`] again on the retained `raw`, which is what makes it a
    /// millisecond operation and therefore something a slider can drive.
    ///
    /// The escalations are the expensive rungs of the ladder; this is the free
    /// one. It is only possible because no pass ever diffuses over sharpened
    /// pixels — `raw` is still exactly what the last diffusion produced, so
    /// re-filtering it is lossless no matter how many times it is done.
    ///
    /// `None` keeps the current value, matching how overrides work everywhere
    /// else here. The change applies to this image only, and only until the next
    /// pass — see [`Pending::sharpness`].
    pub fn adjust(
        &mut self,
        id: u64,
        sharpness: Option<f32>,
        clarity: Option<f32>,
    ) -> Result<GenResult> {
        let mut p = self.lookup(id, "adjust")?;
        p.sharpness = sharpness.unwrap_or(p.sharpness);
        p.clarity = clarity.unwrap_or(p.clarity);
        // Same bounds the config goes through, from the same place.
        crate::config::validate_filters(&p.profile.name, p.sharpness, p.clarity)?;

        let t = Instant::now();
        // Straight back over the same `display` path, and `step` doesn't move:
        // `display` is only ever an output — no pass reads it — so there is no
        // new file to make, and a drag can't churn the work dir or push older
        // results out of the cache.
        let (png, w, h) = self.publish(&p.raw, &p.display, p.sharpness, p.clarity)?;
        let fsr_ms = t.elapsed().as_millis();

        let entry = Pending {
            raw_w: w,
            raw_h: h,
            ..p
        };
        let result = self.result(id, &entry, png, Timings { fsr_ms, ..Default::default() });
        self.store(id, entry);
        Ok(result)
    }

    /// The one place RCAS is applied. Sharpens the retained raw image into
    /// `display` and returns the PNG bytes for the response.
    ///
    /// Scale 1.0, so [`Fsr::process`] skips EASU and this is a sharpen-only
    /// pass — two compute passes plus a PNG round-trip, microseconds against the
    /// diffusion it follows. At `sharpness` 0.0 it is an exact identity.
    ///
    /// It also reports the true dimensions, which is how each pass learns what
    /// sd.cpp actually produced rather than what it was asked for.
    fn publish(
        &self,
        raw: &Path,
        display: &Path,
        sharpness: f32,
        clarity: f32,
    ) -> Result<(Vec<u8>, u32, u32)> {
        let (w, h) = self.fsr.process(raw, display, 1.0, sharpness, clarity)?;
        let png = std::fs::read(display)?;
        Ok((png, w, h))
    }

    fn lookup(&self, id: u64, action: &str) -> Result<Pending> {
        match self.pending.get(&id) {
            Some(p) => Ok(p.clone()),
            None => bail!(
                "that image is no longer available to {action} — it was evicted or \
                 the server restarted. Generate it again (seed is preserved) to retry."
            ),
        }
    }

    /// Record an entry as the current state of `id`, evicting the oldest
    /// results (and their files) once the cache is over budget.
    fn store(&mut self, id: u64, entry: Pending) {
        self.pending.insert(id, entry);
        while self.pending.len() > MAX_PENDING {
            if let Some((_, old)) = self.pending.pop_first() {
                old.remove_files();
            }
        }
    }

    fn result(&self, id: u64, entry: &Pending, png: Vec<u8>, timings: Timings) -> GenResult {
        GenResult {
            png,
            width: entry.raw_w,
            height: entry.raw_h,
            seed: entry.seed,
            timings,
            refine_passes: entry.refine_passes,
            hires_passes: entry.hires_passes,
            image_id: id,
            can_refine: entry.can_refine(),
            can_hires: entry.can_hires(),
            sharpness: entry.sharpness,
            clarity: entry.clarity,
        }
    }
}

/// Round down to a multiple of 64 (never below 64) — SDXL wants clean latent
/// dimensions, and FSR's float scaling would otherwise hand it odd sizes.
fn snap64(v: u32) -> u32 {
    (v / 64).max(1) * 64
}

/// Best-effort removal of every PNG in `dir` (leftovers from a previous run).
fn purge_pngs(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for path in entries.filter_map(|e| e.ok().map(|e| e.path())) {
        let is_png = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("png"))
            .unwrap_or(false);
        if is_png {
            let _ = std::fs::remove_file(&path);
        }
    }
}

fn random_seed() -> i64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    let ctr = JOB_COUNTER.load(Ordering::Relaxed);
    ((nanos ^ (ctr.wrapping_mul(2654435761))) & 0x7fff_ffff) as i64
}
