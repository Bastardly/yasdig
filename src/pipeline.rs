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
use crate::workitems::{self, WorkItem};
use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

static JOB_COUNTER: AtomicU64 = AtomicU64::new(0);

/// How many results stay escalatable. Older ones are evicted (files deleted)
/// so `work/` can't grow without bound.
///
/// Eviction used to lose you the image. It doesn't any more: every generation is
/// archived under `work/workitems/` (see [`crate::workitems`]) and can be
/// reopened, which turns this from a hard limit on what exists into a limit on
/// what is *already warm*. That's why it stays at 8 rather than growing.
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
    /// CFG for the refine and hi-res passes. `None` keeps the profile's setting,
    /// which is itself usually unset and so follows `cfg_scale`.
    pub img2img_cfg_scale: Option<f32>,
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
    /// The archived generation this descends from, so the UI can mark which
    /// thumbnail is on screen. Unrelated to `image_id`: that one addresses the
    /// in-memory cache and restarts at 0, this one outlives the process.
    pub work_id: Option<String>,
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
    /// The archived generation this result descends from, if it has one — set by
    /// `run` and by `open`, and carried through every escalation. Only used for
    /// naming: it groups everything saved out of one generation, and lets the UI
    /// highlight which thumbnail is on screen. `None` if the archive write
    /// failed, which is not a reason to fail the image.
    ///
    /// It is an id, never a path. Nothing in `work/workitems/` is ever opened
    /// through a `Pending` — see [`crate::workitems`].
    work_id: Option<String>,
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
    /// The durable half of `work/`. Written here on a generate, read on an open.
    items: workitems::Store,
    registry: BTreeMap<String, Profile>,
    /// Escalatable results, keyed by job id (ordered, so the oldest evicts first).
    pending: BTreeMap<u64, Pending>,
}

impl Engine {
    pub fn new(cfg: Config) -> Result<Engine> {
        let work_dir = cfg.base_dir.join("work");
        std::fs::create_dir_all(&work_dir).ok();
        // Job ids restart at 0 each run, so clear any PNGs a previous run left
        // behind rather than letting a new job collide with them. Still exactly
        // right with the archive in place: this is non-recursive and matches on
        // a `.png` extension, so `work/workitems/` and `work/output/` are not
        // reachable from it — only the scratch it was always meant to clear.
        purge_pngs(&work_dir);
        let items = workitems::Store::new(&work_dir);
        let fsr = Fsr::new()?;
        // Fail fast with a friendly message if the sd binary is missing.
        SdRunner::new(&cfg).preflight()?;
        let registry = cfg.registry();
        Ok(Engine {
            cfg,
            fsr,
            work_dir,
            items,
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
        // Stays an `Option` on the profile: an absent override leaves it absent,
        // so a request that only raises `cfg_scale` raises what the escalations
        // run at too. Pinning it here would silently break that.
        if let Some(v) = req.img2img_cfg_scale {
            profile.img2img_cfg_scale = Some(v);
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
        let mut scratch = Scratch::default();
        let lowres = scratch.path(self.work_dir.join(format!("{id}-0-base.png")));
        let raw = scratch.path(self.work_dir.join(format!("{id}-0-raw.png")));
        let display = scratch.path(self.work_dir.join(format!("{id}-0-full.png")));

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
        // Nothing reads the base render once it has been upscaled — every
        // escalation works at output resolution now — so it stays in `scratch`
        // and goes when this returns, whichever way it returns.

        // 3) Sharpen for display.
        let (png, w, h) = self.publish(&raw, &display, tpl.sharpness, tpl.clarity)?;
        let fsr_ms = t.elapsed().as_millis();

        // Past every `?`: these two are the result, and the `Pending` below owns
        // them. Anything the guard still holds is an intermediate.
        scratch.keep(&raw);
        scratch.keep(&display);

        // Archive the generation. A *copy* of `raw`, deliberately: the `Pending`
        // below still owns the original, and the first refine will delete it.
        // The unsharpened image is what gets stored, for the same reason every
        // escalation reads it — it's what the diffusion actually produced, and
        // the display filters are re-applied on the way out.
        //
        // Failing here does not fail the generation. The image on screen is worth
        // more than its archive entry, and the alternative is throwing away a
        // render that already cost minutes because a disk was full.
        let mut item = WorkItem {
            id: workitems::new_id(seed),
            created_ms: now_ms(),
            prompt: req.prompt.clone(),
            negative: req.negative.clone(),
            seed,
            template: req.template.clone(),
            width: w,
            height: h,
            thumb_width: 0,
            thumb_height: 0,
            refine_passes: 0,
            hires_passes: 0,
            step: 0,
            sharpness: profile.sharpness,
            clarity: profile.clarity,
            profile: profile.clone(),
        };
        let work_id = match self.items.save(&raw, &mut item) {
            Ok(()) => Some(item.id),
            Err(e) => {
                eprintln!("[rustimage] could not archive work item: {e:#}");
                None
            }
        };

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
            work_id,
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
        let mut scratch = Scratch::default();
        let raw = scratch.path(self.work_dir.join(format!("{id}-{step}-raw.png")));
        let display = scratch.path(self.work_dir.join(format!("{id}-{step}-full.png")));
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

        scratch.keep(&raw);
        scratch.keep(&display);
        // Only now that it succeeded — a failure above leaves the entry intact
        // so the user can retry, and takes this pass's half-written files with
        // it rather than stranding them beside the ones still in use.
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
        let mut scratch = Scratch::default();
        let upscaled = scratch.path(self.work_dir.join(format!("{id}-{step}-up.png")));
        let hires = scratch.path(self.work_dir.join(format!("{id}-{step}-hires.png")));
        let raw = scratch.path(self.work_dir.join(format!("{id}-{step}-raw.png")));
        let display = scratch.path(self.work_dir.join(format!("{id}-{step}-full.png")));
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
        // `hires` and `upscaled` have both been consumed; the guard drops them
        // on the way out, and would have dropped them just the same had the
        // pass died between here and the top of the function.

        // 4) Sharpen for display.
        let (png, w, h) = self.publish(&raw, &display, p.profile.sharpness, p.profile.clarity)?;
        fsr_ms += t.elapsed().as_millis();

        scratch.keep(&raw);
        scratch.keep(&display);
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
        //
        // Deliberately no `Scratch` here, unlike the three passes above. This
        // writes over a file the retained `Pending` already owns, so putting it
        // under the guard would mean a failed drag deleting a live image.
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

    /// Bring an archived generation back as a live result: a fresh `image_id`
    /// with its own `Pending`, so refine, hi-res and the sliders all work on it
    /// immediately. No diffusion runs — this is a file copy and a `publish`.
    ///
    /// The stored image is **copied** into the scratch dir rather than adopted in
    /// place. A `Pending` pointing at `work/workitems/` would have its archive
    /// entry deleted by `remove_files` the first time it was refined, which is
    /// the one way this feature can quietly destroy the thing it exists to keep.
    ///
    /// The profile comes back verbatim from the sidecar, so the passes run with
    /// the settings that made the image rather than whatever the UI shows now —
    /// the same property a live `Pending` has, extended across a restart.
    pub fn open(&mut self, work_id: &str) -> Result<GenResult> {
        let item = self.items.load(work_id)?;
        let stored = self.items.image_path(&item.id);

        let id = JOB_COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut scratch = Scratch::default();
        let raw = scratch.path(self.work_dir.join(format!("{id}-0-raw.png")));
        let display = scratch.path(self.work_dir.join(format!("{id}-0-full.png")));

        let t = Instant::now();
        std::fs::copy(&stored, &raw)
            .with_context(|| format!("reopening {}", stored.display()))?;
        // Filters start where the model says again, exactly as a fresh generate
        // does — the archived image is unsharpened, so there is nothing to undo.
        let (png, w, h) = self.publish(
            &raw,
            &display,
            item.profile.sharpness,
            item.profile.clarity,
        )?;
        let fsr_ms = t.elapsed().as_millis();

        scratch.keep(&raw);
        scratch.keep(&display);

        let entry = Pending {
            raw,
            raw_w: w,
            raw_h: h,
            display,
            sharpness: item.profile.sharpness,
            clarity: item.profile.clarity,
            profile: item.profile,
            prompt: item.prompt,
            negative: item.negative,
            seed: item.seed,
            work_id: Some(item.id),
            refine_passes: item.refine_passes,
            hires_passes: item.hires_passes,
            step: 0,
        };
        let result = self.result(id, &entry, png, Timings { fsr_ms, ..Default::default() });
        self.store(id, entry);
        Ok(result)
    }

    /// Copy the image currently on screen into `work/output/`, and return where
    /// it landed so the UI can say so.
    ///
    /// `display`, not `raw`: this is the keep-this-one button, and what the user
    /// means by *this one* includes wherever the two sliders were left. The
    /// unsharpened version is already archived for every generation, so nothing
    /// is lost by saving the filtered one here.
    pub fn save_output(&self, id: u64) -> Result<PathBuf> {
        let p = self.lookup(id, "save")?;
        let item = WorkItem {
            // Named by origin, so every save out of one generation groups
            // together; the job id only stands in when the archive write failed.
            id: p.work_id.clone().unwrap_or_else(|| id.to_string()),
            created_ms: now_ms(),
            prompt: p.prompt.clone(),
            negative: p.negative.clone(),
            seed: p.seed,
            template: p.profile.name.clone(),
            width: p.raw_w,
            height: p.raw_h,
            thumb_width: 0,
            thumb_height: 0,
            refine_passes: p.refine_passes,
            hires_passes: p.hires_passes,
            step: p.step,
            sharpness: p.sharpness,
            clarity: p.clarity,
            profile: p.profile.clone(),
        };
        self.items.save_output(&p.display, &item)
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
                 the server restarted. Click it in the work items below to reopen \
                 it, settings and all."
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
            work_id: entry.work_id.clone(),
            can_refine: entry.can_refine(),
            can_hires: entry.can_hires(),
            sharpness: entry.sharpness,
            clarity: entry.clarity,
        }
    }
}

/// Files a pass created that shouldn't outlive it. Anything still listed when
/// this drops is deleted, so an early `?` cleans up after itself.
///
/// Every pass writes under fresh names and adopts them only once the whole
/// thing has succeeded — until then nothing else references them. So an
/// `sd-cli` that died partway (VRAM, most often) used to leave a half-written
/// PNG behind that no `Pending` pointed at and only `purge_pngs` cleared, at
/// the next restart.
///
/// The genuine intermediates go in here too, not just the failure cases.
/// `lowres`, `upscaled` and `hires` are transient on the success path as well,
/// so letting the guard remove them covers both paths with one rule instead of
/// one `remove_file` for success and nothing at all for failure.
#[derive(Default)]
struct Scratch(Vec<PathBuf>);

impl Scratch {
    /// Register a path the pass is about to write, and hand it straight back.
    fn path(&mut self, p: PathBuf) -> PathBuf {
        self.0.push(p.clone());
        p
    }

    /// Promote a file out of the set — a `Pending` owns it from here on.
    fn keep(&mut self, p: &Path) {
        self.0.retain(|q| q != p);
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        for p in &self.0 {
            let _ = std::fs::remove_file(p);
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

/// Wall-clock milliseconds, for work item ids and their `created_ms`. Only ever
/// compared against other values from here, so a clock that steps backwards
/// misorders the grid and nothing worse.
fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn random_seed() -> i64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    let ctr = JOB_COUNTER.load(Ordering::Relaxed);
    ((nanos ^ (ctr.wrapping_mul(2654435761))) & 0x7fff_ffff) as i64
}
