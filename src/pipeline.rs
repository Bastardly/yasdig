//! Per-request orchestration, with per-stage timing. Runs on the single GPU
//! worker thread so generation is serialized (one job touches the GPU at a time).
//!
//! Generation is deliberately the cheap part — sd.cpp txt2img at base resolution
//! plus an FSR upscale — and quality is escalated afterwards, on demand:
//!
//! ```text
//!   run()     txt2img (base res) ────────────────────▶ FSR upscale+sharpen
//!   refine()  img2img @ base res (low denoise) ──────▶ FSR upscale+sharpen
//!   hires()   FSR upscale ─▶ img2img @ output res ──▶ FSR upscale (if over budget)
//! ```
//!
//! Each result keeps its intermediates on disk (see [`Pending`]) so the next
//! step up the ladder doesn't re-run the ones below it.

use crate::config::{Config, Profile};
use crate::fsr::Fsr;
use crate::sd::SdRunner;
use anyhow::{bail, Result};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

static JOB_COUNTER: AtomicU64 = AtomicU64::new(0);

/// How many results stay escalatable. Older ones are evicted (files deleted)
/// so `work/` can't grow without bound.
const MAX_PENDING: usize = 8;

/// Hard limit on the hi-res upscale factor a request may ask for.
const MAX_UPSCALE: f32 = 4.0;

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
    pub refined: bool,
    pub hires_passes: u32,
    /// Handle for [`Engine::refine`] / [`Engine::hires`].
    pub image_id: u64,
    pub can_refine: bool,
    pub can_hires: bool,
}

/// The intermediates of one result, kept on disk so quality can be escalated
/// without redoing the cheaper stages. Also holds everything the img2img passes
/// need — notably the profile with the per-request overrides already applied, so
/// a later pass matches the settings the image was generated with rather than
/// whatever the UI happens to show now.
#[derive(Clone)]
struct Pending {
    /// Base-resolution render. `None` once refined or hi-res'd — both supersede
    /// it, and re-refining from the base would throw that work away.
    base: Option<PathBuf>,
    /// The image currently on screen, at output resolution.
    full: PathBuf,
    full_w: u32,
    full_h: u32,
    profile: Profile,
    prompt: String,
    negative: Option<String>,
    seed: i64,
    refined: bool,
    hires_passes: u32,
    /// How many escalations this result has been through. Only used to keep
    /// each step's filenames distinct from the ones it supersedes — otherwise a
    /// pass would read and write the same path.
    step: u32,
}

impl Pending {
    fn can_refine(&self) -> bool {
        self.base.is_some()
    }

    /// A hi-res pass needs room to diffuse: once the image is past the budget
    /// there's no affordable pass left to run on it.
    fn can_hires(&self) -> bool {
        self.profile.hires_steps > 0
            && self.full_w.max(self.full_h) <= self.profile.hires_max_side
    }

    fn remove_files(&self) {
        if let Some(base) = &self.base {
            let _ = std::fs::remove_file(base);
        }
        let _ = std::fs::remove_file(&self.full);
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
        let full = self.work_dir.join(format!("{id}-0-full.png"));

        let sd = SdRunner::new(&self.cfg);
        let neg = req.negative.as_deref();

        // 1) Distilled generation at base resolution.
        let t = Instant::now();
        sd.txt2img(tpl, &req.prompt, neg, seed, &lowres)?;
        let gen_ms = t.elapsed().as_millis();

        // 2) FSR upscale + sharpen — cheap, and enough on its own for a draft.
        //    Quality is escalated afterwards via `refine` and `hires`.
        let t = Instant::now();
        let (w, h) = self
            .fsr
            .process(&lowres, &full, tpl.upscale_factor, tpl.sharpness)?;
        let fsr_ms = t.elapsed().as_millis();

        let png = std::fs::read(&full)?;

        // Retain the intermediates so the next step up the ladder doesn't have
        // to re-run this one. The base render is only worth keeping if the
        // profile can actually refine it.
        let entry = Pending {
            base: if tpl.refine_steps > 0 {
                Some(lowres.clone())
            } else {
                let _ = std::fs::remove_file(&lowres);
                None
            },
            full,
            full_w: w,
            full_h: h,
            profile: profile.clone(),
            prompt: req.prompt.clone(),
            negative: req.negative.clone(),
            seed,
            refined: false,
            hires_passes: 0,
            step: 0,
        };
        let result = self.result(id, &entry, png, Timings { gen_ms, fsr_ms, ..Default::default() });
        self.store(id, entry);
        Ok(result)
    }

    /// Low-denoise img2img on the retained base render, then re-upscale.
    /// Improves the image within SDXL's native resolution range, cheaply.
    pub fn refine(&mut self, id: u64) -> Result<GenResult> {
        let p = self.lookup(id, "refine")?;
        let base = match &p.base {
            Some(b) => b.clone(),
            None => bail!("this image has already been refined"),
        };

        let step = p.step + 1;
        let refined = self.work_dir.join(format!("{id}-{step}-refined.png"));
        let out = self.work_dir.join(format!("{id}-{step}-full.png"));
        let sd = SdRunner::new(&self.cfg);

        let t = Instant::now();
        sd.img2img(
            &p.profile,
            &p.prompt,
            p.negative.as_deref(),
            p.seed,
            &base,
            &refined,
            p.profile.refine_strength,
            p.profile.refine_steps,
            false,
            "img2img refine",
        )?;
        let refine_ms = t.elapsed().as_millis();

        let t = Instant::now();
        let (w, h) = self.fsr.process(
            &refined,
            &out,
            p.profile.upscale_factor,
            p.profile.sharpness,
        )?;
        let fsr_ms = t.elapsed().as_millis();

        let png = std::fs::read(&out)?;
        let _ = std::fs::remove_file(&refined);

        // Only now that it succeeded — a failure above leaves the entry intact
        // so the user can retry. The base render is spent either way.
        p.remove_files();
        let entry = Pending {
            base: None,
            full: out,
            full_w: w,
            full_h: h,
            refined: true,
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
                 affordable pass left to run on it",
                p.full_w,
                p.full_h,
                p.profile.hires_max_side
            );
        }

        let target_w = ((p.full_w as f32) * upscale).round() as u32;
        let target_h = ((p.full_h as f32) * upscale).round() as u32;
        // Largest size the diffusion pass may run at, snapped to a multiple of
        // 64 so the latent dimensions stay clean.
        let budget = p.profile.hires_max_side.min(target_w.max(target_h));
        let diff_scale = budget as f32 / p.full_w.max(p.full_h) as f32;
        let diff_w = snap64(((p.full_w as f32) * diff_scale).round() as u32);
        let diff_h = snap64(((p.full_h as f32) * diff_scale).round() as u32);

        let step = p.step + 1;
        let upscaled = self.work_dir.join(format!("{id}-{step}-up.png"));
        let hires = self.work_dir.join(format!("{id}-{step}-hires.png"));
        let out = self.work_dir.join(format!("{id}-{step}-full.png"));
        let sd = SdRunner::new(&self.cfg);
        let mut fsr_ms = 0u128;

        // 1) Enlarge to the diffusion size, if that's bigger than what we have.
        //    No sharpening: CAS artifacts would be baked in by the diffusion.
        let init = if diff_w > p.full_w {
            let t = Instant::now();
            self.fsr.process(
                &p.full,
                &upscaled,
                diff_w as f32 / p.full_w as f32,
                0.0,
            )?;
            fsr_ms += t.elapsed().as_millis();
            upscaled.clone()
        } else {
            p.full.clone()
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
            true,
            "img2img hi-res",
        )?;
        let hires_ms = t.elapsed().as_millis();

        // 3) Cover any distance the budget wouldn't stretch to.
        let (w, h) = if target_w > diff_w {
            let t = Instant::now();
            let dims = self.fsr.process(
                &hires,
                &out,
                target_w as f32 / diff_w as f32,
                p.profile.sharpness,
            )?;
            fsr_ms += t.elapsed().as_millis();
            let _ = std::fs::remove_file(&hires);
            dims
        } else {
            // Nothing left to upscale; the pass output *is* the result.
            std::fs::rename(&hires, &out)?;
            (diff_w, diff_h)
        };

        let png = std::fs::read(&out)?;
        let _ = std::fs::remove_file(&upscaled);

        p.remove_files();
        let entry = Pending {
            base: None,
            full: out,
            full_w: w,
            full_h: h,
            hires_passes: p.hires_passes + 1,
            step,
            ..p
        };
        let result = self.result(id, &entry, png, Timings { fsr_ms, hires_ms, ..Default::default() });
        self.store(id, entry);
        Ok(result)
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
            width: entry.full_w,
            height: entry.full_h,
            seed: entry.seed,
            timings,
            refined: entry.refined,
            hires_passes: entry.hires_passes,
            image_id: id,
            can_refine: entry.can_refine(),
            can_hires: entry.can_hires(),
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
