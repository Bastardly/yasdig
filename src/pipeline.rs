//! Per-request orchestration: sd.cpp txt2img -> FSR upscale/sharpen -> optional
//! img2img refine, with per-stage timing. Runs on the single GPU worker thread
//! so generation is serialized (one job touches the GPU at a time).
//!
//! When a result is produced *without* the refine pass, the base-resolution PNG
//! is retained (see [`Pending`]) so the UI can ask for a refine after the fact
//! without re-running txt2img.

use crate::config::{Config, Profile};
use crate::fsr::Fsr;
use crate::sd::SdRunner;
use anyhow::{bail, Result};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

static JOB_COUNTER: AtomicU64 = AtomicU64::new(0);

/// How many base images stay refinable. Older ones are evicted (file deleted)
/// so `work/` can't grow without bound.
const MAX_PENDING: usize = 8;

#[derive(Debug, Clone)]
pub struct GenRequest {
    pub prompt: String,
    /// Name of a registry entry (template or discovered model).
    pub template: String,
    pub negative: Option<String>,
    /// Negative = pick one; otherwise use as-is.
    pub seed: i64,
    pub quality: bool,
    /// Optional per-request overrides (so one discovered model works for both
    /// Turbo and standard checkpoints without editing config).
    pub steps: Option<u32>,
    pub cfg_scale: Option<f32>,
    pub sampler: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

#[derive(Debug, Clone, Copy)]
pub struct Timings {
    pub gen_ms: u128,
    pub fsr_ms: u128,
    pub refine_ms: u128,
}

pub struct GenResult {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub seed: i64,
    pub timings: Timings,
    pub refined: bool,
    /// Handle for [`Engine::refine`] when a refinable base image was retained.
    /// `None` when this result is already refined, or the model can't refine.
    pub refine_id: Option<u64>,
}

/// A base-resolution render kept on disk so it can be refined after the fact.
/// Holds everything `SdRunner::img2img_refine` needs — notably the profile with
/// the per-request overrides already applied, so a later refine matches the
/// settings the image was generated with, not whatever the UI shows now.
#[derive(Clone)]
struct Pending {
    base: PathBuf,
    profile: Profile,
    prompt: String,
    negative: Option<String>,
    seed: i64,
}

pub struct Engine {
    cfg: Config,
    fsr: Fsr,
    work_dir: PathBuf,
    registry: BTreeMap<String, Profile>,
    /// Refinable base images, keyed by job id (ordered, so the oldest evicts first).
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
        let lowres = self.work_dir.join(format!("{id}-lowres.png"));
        let refined_base = self.work_dir.join(format!("{id}-refined.png"));
        let fsr_out = self.work_dir.join(format!("{id}-fsr.png"));

        let sd = SdRunner::new(&self.cfg);
        let neg = req.negative.as_deref();

        // 1) Distilled generation at base resolution.
        let t = Instant::now();
        sd.txt2img(tpl, &req.prompt, neg, seed, &lowres)?;
        let gen_ms = t.elapsed().as_millis();

        // 2) Optional low-denoise refine — done BEFORE upscaling, at the base
        //    resolution. Refining after FSR would run SDXL img2img at the
        //    upscaled size (e.g. 2048²), which exceeds GPU VRAM. Refining first
        //    keeps diffusion in SDXL's native range and still improves the image
        //    the cheap FSR pass then upscales.
        let mut refine_ms = 0u128;
        let mut refined = false;
        let fsr_input = if req.quality && tpl.refine_steps > 0 {
            let t = Instant::now();
            sd.img2img_refine(tpl, &req.prompt, neg, seed, &lowres, &refined_base)?;
            refine_ms = t.elapsed().as_millis();
            refined = true;
            refined_base.clone()
        } else {
            lowres.clone()
        };

        // 3) FSR upscale + sharpen (the cheap "shortcut" instead of a second
        //    high-res diffusion pass).
        let t = Instant::now();
        let (w, h) = self
            .fsr
            .process(&fsr_input, &fsr_out, tpl.upscale_factor, tpl.sharpness)?;
        let fsr_ms = t.elapsed().as_millis();

        let png = std::fs::read(&fsr_out)?;

        // Best-effort cleanup of intermediates.
        let _ = std::fs::remove_file(&fsr_out);
        if refined {
            let _ = std::fs::remove_file(&refined_base);
        }

        // Keep the base render around if a later refine is still possible, so
        // "Refine image" costs an img2img + FSR instead of a full re-generate.
        let refine_id = if !refined && tpl.refine_steps > 0 {
            self.pending.insert(
                id,
                Pending {
                    base: lowres,
                    profile: profile.clone(),
                    prompt: req.prompt.clone(),
                    negative: req.negative.clone(),
                    seed,
                },
            );
            while self.pending.len() > MAX_PENDING {
                if let Some((_, old)) = self.pending.pop_first() {
                    let _ = std::fs::remove_file(&old.base);
                }
            }
            Some(id)
        } else {
            let _ = std::fs::remove_file(&lowres);
            None
        };

        Ok(GenResult {
            png,
            width: w,
            height: h,
            seed,
            timings: Timings {
                gen_ms,
                fsr_ms,
                refine_ms,
            },
            refined,
            refine_id,
        })
    }

    /// Run the img2img refine pass on a retained base image, then re-upscale.
    /// Same work `run` would have done with `quality: true`, minus the txt2img.
    pub fn refine(&mut self, id: u64) -> Result<GenResult> {
        let p = match self.pending.get(&id) {
            Some(p) => p.clone(),
            None => bail!(
                "that image is no longer available to refine — it was evicted or \
                 the server restarted. Generate it again (seed is preserved) to refine."
            ),
        };

        let refined_base = self.work_dir.join(format!("{id}-refined.png"));
        let fsr_out = self.work_dir.join(format!("{id}-fsr.png"));
        let sd = SdRunner::new(&self.cfg);

        let t = Instant::now();
        sd.img2img_refine(
            &p.profile,
            &p.prompt,
            p.negative.as_deref(),
            p.seed,
            &p.base,
            &refined_base,
        )?;
        let refine_ms = t.elapsed().as_millis();

        let t = Instant::now();
        let (w, h) = self.fsr.process(
            &refined_base,
            &fsr_out,
            p.profile.upscale_factor,
            p.profile.sharpness,
        )?;
        let fsr_ms = t.elapsed().as_millis();

        let png = std::fs::read(&fsr_out)?;

        // Only now that it succeeded: an image refines once, so drop the base
        // image along with the intermediates. A failure above leaves the entry
        // in place so the user can retry.
        self.pending.remove(&id);
        let _ = std::fs::remove_file(&p.base);
        let _ = std::fs::remove_file(&refined_base);
        let _ = std::fs::remove_file(&fsr_out);

        Ok(GenResult {
            png,
            width: w,
            height: h,
            seed: p.seed,
            timings: Timings {
                gen_ms: 0,
                fsr_ms,
                refine_ms,
            },
            refined: true,
            refine_id: None,
        })
    }
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
