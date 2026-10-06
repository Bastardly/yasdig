//! Per-request orchestration, with per-stage timing. Two lanes share it:
//! [`Engine`] runs on the diffusion worker, which takes its work from the job
//! queue (`crate::jobs`) one image at a time, and [`Display`] runs on the
//! display worker — open, adjust, save — so the viewer answers in the middle of
//! a render. They share one FSR device behind a mutex, which the diffusion lane
//! holds only for its own FSR passes and never while sd-cli runs.
//!
//! Generation is deliberately the cheap part — sd.cpp txt2img at base resolution
//! plus an FSR upscale — and quality is escalated afterwards, on demand. Every
//! rung reads the retained *unsharpened* output-resolution image, writes a new
//! one, and RCAS runs once on the way to the browser:
//!
//! ```text
//!   run()     txt2img (base) ─▶ EASU or ESRGAN upscale ────▶ raw ─▶ RCAS ─▶ PNG
//!   img2img() fit a source image ─▶ img2img ───────────────▶ raw ─▶ RCAS ─▶ PNG
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

use crate::config::{Config, Filters, FiltersPatch, LoraRef, Profile};
use crate::fsr::Fsr;
use crate::sd::{Monitor, SdRunner, VideoIo};
use crate::variables;
use crate::videos::{Clip, VideoStore};
use crate::workitems::{self, Shelf, WorkItem};
use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

static JOB_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The one FSR device, shared by both lanes. A mutex rather than a second
/// device: the diffusion lane holds it for its own FSR passes — milliseconds,
/// or seconds for an upscaler model — and never across an sd-cli run, so an
/// edit waits only for a pass it would have had to share the GPU with anyway,
/// and no second device's buffers sit in VRAM beside sd-cli's.
pub type SharedFsr = Arc<Mutex<Fsr>>;

/// Lock without letting one panicked pass poison every later one. The device
/// and its pipelines are still valid after a failed dispatch.
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Where on the page a retained image is shown. **One live image per slot** is
/// the whole eviction rule: storing into a slot lets go of what was there.
///
/// It replaced an eight-entry cache (`MAX_PENDING`) that existed before work
/// items were archived — an evicted image is reopenable from
/// `work/workitems/`, so there is no reason to keep more than what is on
/// screen. Two slots rather than one because both can be on screen in the
/// same minute: a batch image landing must not throw away the output being
/// edited in the viewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// The Image tab's result: a run's images, their escalations, a reopened
    /// work item.
    Image,
    /// The image the Outputs tab's viewer has open.
    Output,
}

struct Live {
    pending: Pending,
    /// `None` once something else took the slot while this was pinned.
    slot: Option<Slot>,
    /// Jobs that still need this image: the run whose hi-res and refine are
    /// still to come, or a Refine queued behind a batch. Evicting a pinned entry
    /// takes its slot and leaves its files until the last pin goes, because the
    /// job would otherwise fail on a file a reopened work item deleted.
    pins: u32,
}

/// Every retained image, shared by the two lanes. Keyed by `image_id`, which
/// an escalation keeps — a refine replaces the entry under the same id.
#[derive(Default)]
pub struct Retained {
    live: BTreeMap<u64, Live>,
}

impl Retained {
    fn get(&self, id: u64, action: &str) -> Result<Pending> {
        match self.live.get(&id) {
            Some(l) => Ok(l.pending.clone()),
            None => bail!(
                "that image is no longer available to {action} — a newer one has \
                 replaced it, or the server restarted. Click it in the work items \
                 below to reopen it, settings and all."
            ),
        }
    }

    /// The slot an entry is in, so an escalation lands back where it was.
    fn slot_of(&self, id: u64) -> Slot {
        self.live.get(&id).and_then(|l| l.slot).unwrap_or(Slot::Image)
    }

    /// Record `entry` as the current state of `id` in `slot`, letting go of
    /// whatever else was there. `pin` adds a hold for the caller — see
    /// [`Live::pins`]; an existing entry's pins carry over.
    fn store(&mut self, id: u64, slot: Slot, entry: Pending, pin: bool) {
        let others: Vec<u64> = self
            .live
            .iter()
            .filter(|(k, l)| **k != id && l.slot == Some(slot))
            .map(|(k, _)| *k)
            .collect();
        for k in others {
            if let Some(l) = self.live.get_mut(&k) {
                l.slot = None;
            }
            self.drop_if_free(k);
        }
        let pins = self.live.get(&id).map_or(0, |l| l.pins) + u32::from(pin);
        self.live.insert(
            id,
            Live {
                pending: entry,
                slot: Some(slot),
                pins,
            },
        );
    }

    /// Write back an adjusted entry — only if it is still the one that was
    /// read. A refine can finish while a slider drag is in flight on the other
    /// lane, and writing the drag back would put the pre-refine image's paths
    /// over the new one's.
    fn update(&mut self, id: u64, entry: Pending) -> bool {
        match self.live.get_mut(&id) {
            Some(l) if l.pending.raw == entry.raw => {
                l.pending = entry;
                true
            }
            _ => false,
        }
    }

    /// Hold `id` for a job. Returns the model that made it — the label the
    /// Jobs dialog shows — or `None` when the image is already gone.
    pub fn pin(&mut self, id: u64) -> Option<String> {
        let l = self.live.get_mut(&id)?;
        l.pins += 1;
        Some(l.pending.profile.name.clone())
    }

    pub fn unpin(&mut self, id: u64) {
        if let Some(l) = self.live.get_mut(&id) {
            l.pins = l.pins.saturating_sub(1);
        }
        self.drop_if_free(id);
    }

    fn drop_if_free(&mut self, id: u64) {
        let free = self
            .live
            .get(&id)
            .is_some_and(|l| l.slot.is_none() && l.pins == 0);
        if free {
            if let Some(l) = self.live.remove(&id) {
                l.pending.remove_files();
            }
        }
    }
}

/// Hard limit on the hi-res upscale factor a request may ask for.
pub const MAX_UPSCALE: f32 = 4.0;

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
    pub scheduler: Option<String>,
    /// Step caching for this pass; `None` keeps the profile's.
    pub cache_mode: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// The FSR (EASU) enlarge every base render goes through on its way to the
    /// output size. `None` keeps the profile's. It is a per-request knob because
    /// it is the one setting that decides what "the image" even is — and because
    /// **1.0 is off**: `Fsr::process` skips EASU entirely there, which is how you
    /// see what the checkpoint actually drew.
    pub upscale_factor: Option<f32>,
    /// What performs that enlarge. `None` keeps the profile's; `Some("")` is FSR,
    /// explicitly — the empty option of a select, which has to be able to say
    /// "not a model" over a default that names one. See [`Profile::upscaler`].
    pub upscaler: Option<String>,
    /// Where the escalation passes start. Applied to the profile before it's
    /// retained, so a later `refine`/`hires` uses what was set at generate time
    /// unless that pass is handed its own overrides — see [`EscalateRequest`].
    pub refine_steps: Option<u32>,
    pub refine_strength: Option<f32>,
    pub refine_cfg_scale: Option<f32>,
    pub hires_steps: Option<u32>,
    pub hires_strength: Option<f32>,
    pub hires_cfg_scale: Option<f32>,
    /// The checkpoints the Refine and Hi-res buttons will run, chosen before the
    /// picture exists. Applied onto the profile and therefore **archived with the
    /// work item**, which is the whole reason they travel with a generate: the
    /// two escalation routes take their own, so the pass itself needs nothing
    /// from here. Reopening an item is what would otherwise lose them.
    pub refine_model: Option<String>,
    pub hires_model: Option<String>,
    /// Display-time post-process, applied once to the finished image.
    /// Cheap enough to tune freely.
    pub sharpness: Option<f32>,
    pub clarity: Option<f32>,
    /// LoRAs for this generation. `None` is "the field wasn't sent", which keeps
    /// whatever the profile has; `Some(vec![])` is "none, explicitly". No
    /// `double_option` needed here, unlike `refine_cfg_scale` on an escalation — an empty list
    /// is spellable, so the two states don't collide the way absent and `null`
    /// do for a number.
    ///
    /// Retained on the profile, so a later refine or hi-res runs with the same
    /// adapters. See [`Profile::loras`].
    pub loras: Option<Vec<LoraRef>>,
    /// The separate files picked in the *Models* fold, slot key to a name
    /// relative to the models dir. An absent slot keeps the model's; `null`
    /// empties it, which is only allowed on a bundled checkpoint (its own copy
    /// is then used). Resolved by [`Config::apply_parts`] and retained on the
    /// profile like `loras`, so refine and hi-res decode with the same VAE.
    pub parts: Option<BTreeMap<String, Option<String>>>,
}

/// One image-to-image generation: a source image the user supplied instead of a
/// txt2img, put through one diffusion pass.
///
/// The same shape as [`GenRequest`] minus the two things a txt2img owns and this
/// doesn't. There is no `width`/`height`, because `SdRunner::img2img` sends no
/// `-W`/`-H` at all — sd.cpp takes the size from the init image, so the size of
/// this pass is decided by `max_side` below and by nothing else. The pass runs the
/// base `steps` at the base `cfg_scale`, exactly as a txt2img would; its one knob
/// of its own is `source_strength`, which decides how far up the schedule those
/// steps start.
#[derive(Debug, Clone)]
pub struct Img2ImgRequest {
    pub prompt: String,
    /// Name of a registry entry (template or discovered model).
    pub template: String,
    /// A `work/output/` stem — a saved image or an upload. Required: this pass
    /// has nothing to diffuse without one.
    pub source: String,
    pub negative: Option<String>,
    /// Negative = pick one; otherwise use as-is.
    pub seed: i64,
    /// Longest side the pass may run at. `None` uses the profile's
    /// `hires_max_side`, which is the same budget question asked of the same
    /// hardware — see [`fit_to`] for what it does to a source bigger than that.
    pub max_side: Option<u32>,
    pub source_strength: Option<f32>,
    pub steps: Option<u32>,
    pub cfg_scale: Option<f32>,
    pub sampler: Option<String>,
    pub scheduler: Option<String>,
    /// Step caching for this pass; `None` keeps the profile's.
    pub cache_mode: Option<String>,
    pub refine_steps: Option<u32>,
    pub refine_strength: Option<f32>,
    pub refine_cfg_scale: Option<f32>,
    pub hires_steps: Option<u32>,
    pub hires_strength: Option<f32>,
    pub hires_cfg_scale: Option<f32>,
    /// See [`GenRequest::refine_model`]. No `upscale_factor` beside them, unlike
    /// that request: this pass has no EASU stage to scale — the pixels already
    /// exist at whatever size they exist at.
    pub refine_model: Option<String>,
    pub hires_model: Option<String>,
    pub sharpness: Option<f32>,
    pub clarity: Option<f32>,
    /// See [`GenRequest::loras`].
    pub loras: Option<Vec<LoraRef>>,
    /// See [`GenRequest::parts`].
    pub parts: Option<BTreeMap<String, Option<String>>>,
}

/// One clip to generate.
///
/// Flat `Option`s over the `[[video]]` entry, the same shape as [`GenRequest`]
/// over a `Profile` and for the same reason: the config entry is where the
/// settings *start*, and the panel is allowed to move them for one run without
/// editing `models.toml`.
///
/// There is no escalation request to go with it. The two things that improve a
/// clip both read its frame sequence, so they act on a finished clip rather than
/// on a live one — see [`Engine::video`].
#[derive(Debug, Clone, Default)]
pub struct VideoRequest {
    pub prompt: String,
    /// Name of a `[[video]]` entry.
    pub model: String,
    /// `None` inherits the model's negative prompt, which for Wan is its own
    /// published one. `Some("")` deliberately means *no* negative.
    pub negative: Option<String>,
    /// Negative = pick one; otherwise use as-is.
    pub seed: i64,
    /// A `work/output/` stem to use as the first frame. `None` is text-to-video.
    pub source: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub frames: Option<u32>,
    pub fps: Option<u32>,
    pub steps: Option<u32>,
    pub cfg_scale: Option<f32>,
    pub high_noise_steps: Option<u32>,
    pub high_noise_cfg_scale: Option<f32>,
    pub flow_shift: Option<f32>,
    pub sampler: Option<String>,
    pub vae_tiling: Option<bool>,
}

/// A finished clip, plus how long it took. The timing is the point on this
/// hardware — it's what tells you whether the next clip is worth starting.
#[derive(Debug, Clone, serde::Serialize)]
pub struct VideoResult {
    pub clip: Clip,
    pub gen_ms: u128,
}

/// What [`Engine::convert`] produced, and how well the estimate held.
///
/// `estimated_bytes` is carried through from the plan so the page can show the
/// prediction next to the outcome. It is the only feedback the estimate ever
/// gets, and it is the reason the number is reported rather than just used.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ConvertResult {
    pub out: PathBuf,
    pub source_bytes: u64,
    pub estimated_bytes: u64,
    pub bytes: u64,
    pub ty: String,
    pub ms: u128,
    /// fp8 scales folded into their weights after sd-cli wrote the file.
    pub folded: usize,
}

/// Where [`Engine::enlarge_clip`] put the frames it wrote.
#[derive(Debug, Clone, serde::Serialize)]
pub struct EnlargeResult {
    pub dir: PathBuf,
    pub frames: u32,
    pub width: u32,
    pub height: u32,
    pub ms: u128,
}

/// Per-pass overrides for `refine` / `hires`, read from the Advanced panel at
/// the moment the button is pressed.
///
/// The retained profile is where an escalation *starts*, not where it is pinned:
/// changing "Refine strength" and pressing Refine has to run at the new
/// strength, or the panel is lying about what the button does. `None` keeps the
/// retained value, so a client that sends none of this behaves exactly as before.
///
/// Only the escalation knobs are here. `steps`, `cfg_scale`, the sampler and the
/// base size describe a diffusion that has already run — there is no way to
/// retroactively apply them to the image in hand, so a stale-looking box is
/// honest for those and a lie for these.
///
/// `template` passes that test too, which is less obvious than it looks: a
/// checkpoint is not a setting of the render that produced the image, it is a
/// setting of the render about to happen. Draft on a Turbo checkpoint, finish on
/// a detailed one.
#[derive(Debug, Clone, Default)]
pub struct EscalateRequest {
    /// A registry entry to run *this pass* with, instead of the one the image
    /// was made with. `None` — and an empty string, which is what an unchosen
    /// select sends — keeps it. Applied by [`Engine::swap_model`] rather than by
    /// `apply` below, because resolving a name needs the registry.
    pub template: Option<String>,
    pub refine_steps: Option<u32>,
    pub refine_strength: Option<f32>,
    pub hires_steps: Option<u32>,
    pub hires_strength: Option<f32>,
    /// The pass's own CFG. Two levels deep on purpose. The outer `None` means
    /// the field wasn't sent; `Some(None)` means it was sent empty, which
    /// *unsets* it so the pass follows `cfg_scale` again. Collapsing the two
    /// would make the box one-way — once a number had been typed there,
    /// clearing it could never restore the link the whole `Option` on `Profile`
    /// exists to preserve. Only one of
    /// these two is ever meaningful per route — `/refine` ignores the hi-res one
    /// and vice versa — but they ride in one struct because the page sends one
    /// panel and the boundary is the route, not the body.
    pub refine_cfg_scale: Option<Option<f32>>,
    pub hires_cfg_scale: Option<Option<f32>>,
}

impl EscalateRequest {
    /// Fold onto the retained profile, then re-check it: the panel can put a
    /// strength or a step count out of range just as `/generate` can.
    fn apply(&self, profile: &mut Profile) -> Result<()> {
        if let Some(v) = self.refine_steps {
            profile.refine_steps = v;
        }
        if let Some(v) = self.refine_strength {
            profile.refine_strength = v;
        }
        if let Some(v) = self.hires_steps {
            profile.hires_steps = v;
        }
        if let Some(v) = self.hires_strength {
            profile.hires_strength = v;
        }
        if let Some(v) = self.refine_cfg_scale {
            profile.refine_cfg_scale = v;
        }
        if let Some(v) = self.hires_cfg_scale {
            profile.hires_cfg_scale = v;
        }
        profile.validate()
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Timings {
    pub gen_ms: u128,
    /// An ESRGAN model's share of the initial enlarge, reduce included. 0 when
    /// EASU did it, which is also what keeps the stage off the readout.
    pub upscale_ms: u128,
    pub fsr_ms: u128,
    pub refine_ms: u128,
    pub hires_ms: u128,
    /// Steps the generate pass's step cache skipped, of `cache_steps`. Both 0
    /// when it skipped none, which is also what an escalation reports — those
    /// run uncached ([`Profile::uncached`]).
    pub cache_skipped: u32,
    pub cache_steps: u32,
}

pub struct GenResult {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub seed: i64,
    pub timings: Timings,
    pub refine_passes: u32,
    pub hires_passes: u32,
    /// Handle for [`Engine::refine`] / [`Engine::hires`] / [`Display::adjust`].
    pub image_id: u64,
    /// The archived generation this descends from, so the UI can mark which
    /// thumbnail is on screen. Unrelated to `image_id`: that one addresses the
    /// in-memory cache and restarts at 0, this one outlives the process.
    pub work_id: Option<String>,
    pub can_refine: bool,
    pub can_hires: bool,
    /// The display filters currently applied, so the UI reflects what the image
    /// actually has rather than what its controls last said.
    pub filters: Filters,
    /// Where those filters *would* start on a new image from the profile this
    /// one was made with: the model's sharpness and clarity, and no grade.
    ///
    /// Reported separately because after an escalation `filters` is not it —
    /// `Filters::restart` carries the grade across, so a viewer that took the
    /// applied values as its reset target would make the last edit the new
    /// neutral and leave no way back. It comes off the retained profile, so it
    /// still describes the model that actually ran even when the escalation
    /// named a different checkpoint.
    pub start: Filters,
    /// The prompt as it was actually conditioned — with every `{variable}`
    /// already drawn. It is the retained one, so a refine or a hi-res reports
    /// the sentence that made the image rather than whatever is in the box now,
    /// and the page can show the roll without hovering the work-item grid.
    pub prompt: String,
}

/// The intermediates of one result, kept on disk so quality can be escalated
/// without redoing the cheaper stages. Also holds everything the img2img passes
/// need — notably the profile with the per-request overrides already applied, so
/// a later pass matches the settings the image was generated with rather than
/// whatever the UI happens to show now. The escalation knobs are the deliberate
/// exception: those describe a pass that hasn't run yet, so `refine`/`hires`
/// take them from the request when it carries them (see [`EscalateRequest`]).
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
    /// `profile`: the profile's two values are where the sliders start, and a
    /// fresh generate, an img2img or a reopen starts there again. `adjust` moves
    /// these and touches nothing else.
    ///
    /// An escalation is the one case that isn't all-or-nothing, and
    /// [`Filters::restart`] holds the rule: **detail resets, colour carries.**
    /// Carrying sharpness and clarity forward was the obvious thing and the
    /// wrong one — a new pass replaces the pixels, so the amount of sharpening
    /// that looked right is stale by definition, and inheriting it hides what
    /// the pass did underneath a filter chosen for the previous image. The eight
    /// grading knobs have no such tie to the pixels, and re-dialling a look on
    /// every rung of the ladder is a reason not to climb it.
    filters: Filters,
    prompt: String,
    negative: Option<String>,
    /// The prompt and negative as they were typed, before `variables::expand`
    /// rolled them. `None` when there was nothing to roll.
    ///
    /// Carried through every escalation so that `save_output` — which builds
    /// its sidecar out of this struct, not out of the request — can record what
    /// a refined and enlarged image was asked for as well as what it was
    /// conditioned with. `prompt` above stays the resolved sentence, because
    /// that is what every pass re-conditions.
    typed_prompt: Option<String>,
    typed_negative: Option<String>,
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
    /// The `work/output/` stem this image was diffused from, if it came in
    /// through [`Engine::img2img`]. Carried through every escalation so the
    /// record of where an image started survives the ladder, and so a reopened
    /// work item can put the source thumbnail back.
    ///
    /// A stem, never a path — for the same reason `work_id` is an id. The file
    /// it names belongs to the user, and `remove_files` would delete it.
    source: Option<String>,
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
    fsr: SharedFsr,
    /// `work/current/`: every file a live result or a pass in flight owns.
    /// Its own folder so `work/`'s root holds only the archives, and so
    /// emptying it — on start and on *Close server* — is one sweep that cannot
    /// reach anything durable.
    scratch: PathBuf,
    /// The durable half of `work/`, plus the favourites shelf under `user/`.
    /// Written on a generate, read on an open; which shelf an open reads is a
    /// [`workitems::Shelf`] the request carries.
    items: workitems::Store,
    /// The clip archive. Unlike `items` this is written *through* rather than
    /// copied into — a clip has no scratch stage; see [`crate::videos`].
    videos: VideoStore,
    /// Shared with the connection threads: progress out, cancel in.
    monitor: Monitor,
    /// Escalatable results, shared with the display lane.
    retained: Arc<Mutex<Retained>>,
}

/// The scratch folder under `work/`. See [`Engine::scratch`].
pub fn scratch_dir(work_dir: &Path) -> PathBuf {
    work_dir.join("current")
}

/// Empty the scratch folder. Run on start — job ids restart at 0, so a
/// leftover would collide with a new job's file — and by *Close server*,
/// which needs no setting for it: nothing in here is anything but scratch.
pub fn clear_scratch(work_dir: &Path) {
    purge_pngs(&scratch_dir(work_dir));
    purge_pngs(&preview_dir(work_dir));
}

/// Where sd-cli writes the live preview of the run in flight (`sd::Monitor`).
/// Inside scratch, and emptied by the runner before every run as well.
pub fn preview_dir(work_dir: &Path) -> PathBuf {
    scratch_dir(work_dir).join("preview")
}

impl Engine {
    pub fn new(cfg: Config, monitor: Monitor) -> Result<Engine> {
        let work_dir = cfg.base_dir.join("work");
        let scratch = scratch_dir(&work_dir);
        std::fs::create_dir_all(&scratch).ok();
        clear_scratch(&work_dir);
        // Scratch used to live in `work/` itself. Swept once more here so an
        // install from before `work/current/` doesn't keep its last eight
        // results' PNGs in the root for ever. Non-recursive and `.png` only,
        // so the archives under `work/` are out of its reach.
        purge_pngs(&work_dir);
        let items = workitems::Store::new(&work_dir, &cfg.user_dir());
        let videos = VideoStore::new(&work_dir);
        let fsr = Arc::new(Mutex::new(Fsr::new()?));
        // Fail fast with a friendly message if the sd binary is missing.
        SdRunner::new(&cfg).preflight()?;
        Ok(Engine {
            cfg,
            fsr,
            scratch,
            items,
            videos,
            monitor,
            retained: Arc::default(),
        })
    }

    /// The display lane, sharing this engine's FSR device and retained images.
    pub fn display(&self) -> Display {
        Display {
            fsr: self.fsr.clone(),
            scratch: self.scratch.clone(),
            items: workitems::Store::new(&self.cfg.base_dir.join("work"), &self.cfg.user_dir()),
            retained: self.retained.clone(),
        }
    }

    /// For the HTTP side: pinning an image a queued Refine will need.
    pub fn retained(&self) -> Arc<Mutex<Retained>> {
        self.retained.clone()
    }

    fn fsr(&self) -> MutexGuard<'_, Fsr> {
        lock(&self.fsr)
    }

    /// A runner wired to this engine's monitor. Every pass goes through here, so
    /// there is one place that decides a render is observable and cancellable
    /// rather than three that have to remember to be.
    /// Requantize a model file. No GPU, no `Pending`, no scratch — see
    /// [`crate::sd::SdRunner::convert`] for why it holds the worker anyway.
    ///
    /// The output is checked for size afterwards rather than trusted: a
    /// conversion that ran out of disk halfway leaves a short file and exits
    /// non-zero, but a *cancelled* one leaves a short file and is reported as a
    /// stop, and either way what is on disk now is a partial model that
    /// discovery would happily list. So a failure removes it.
    pub fn convert(&mut self, plan: &crate::convert::Plan) -> Result<ConvertResult> {
        let t = Instant::now();
        if let Err(e) = self.sd().convert(plan) {
            let _ = std::fs::remove_file(&plan.out);
            return Err(e);
        }
        // sd.cpp copies a scaled-fp8 file's per-tensor scales across unfolded,
        // and the raw-magnitude matmul overflows Vulkan's fp16 accumulators
        // (`foldscale`). A file nothing was scaled in is untouched.
        let fold = match crate::foldscale::fold(&plan.out) {
            Ok(r) => r,
            Err(e) => {
                let _ = std::fs::remove_file(&plan.out);
                return Err(e.context("folding the fp8 scales into the converted file"));
            }
        };
        let bytes = std::fs::metadata(&plan.out)
            .with_context(|| {
                format!(
                    "sd-cli reported success but wrote nothing to {}",
                    plan.out.display()
                )
            })?
            .len();
        Ok(ConvertResult {
            out: plan.out.clone(),
            source_bytes: plan.source_bytes,
            estimated_bytes: plan.estimated_bytes,
            bytes,
            ty: plan.ty.clone(),
            ms: t.elapsed().as_millis(),
            folded: fold.folded,
        })
    }

    fn sd(&self) -> SdRunner<'_> {
        SdRunner::watched(&self.cfg, &self.monitor)
    }

    /// Resolve a `work/output/` stem to the file it names.
    ///
    /// The client never sends a path — it picks a stem the server published
    /// through `GET /outputs`, and `valid_id` is the whitelist that makes that
    /// safe (digits, letters and `-`, so `.` and `/` aren't in the alphabet).
    /// Shared by [`Engine::video`], [`Engine::img2img`] and
    /// [`Display::open_output`]: all three start from a finished image, and a
    /// second copy of this check is a second place for it to be gotten wrong.
    fn output_source(&self, stem: &str) -> Result<PathBuf> {
        output_source(&self.items, stem)
    }

    /// Point a pending result at a different checkpoint before an escalation
    /// runs on it.
    ///
    /// The swap is **wholesale**: the named entry's profile replaces the
    /// retained one entirely, bringing its own `refine_*`, `hires_*`,
    /// `hires_max_side`, `weight_type`, `extra_args` and
    /// slider starting points with it. `EscalateRequest::apply` then folds the
    /// panel's explicit boxes on top, which is the right order — the page
    /// refills those boxes from the selected model, so what it sends is already
    /// that model's numbers unless the user typed over them.
    ///
    /// A field-wise merge was the other option and is the trap from the top of
    /// CLAUDE.md wearing a different hat: every knob would have to become an
    /// `Option` so "unset" could be told from "set to the default", and half of
    /// them would silently keep the old model's value.
    ///
    /// The replaced profile is what gets retained in the new `Pending`, so
    /// `can_refine`, `can_hires` and where the sliders reset all describe the
    /// model that actually ran. That is the honest answer and it is what keeps a
    /// saved sidecar true.
    fn swap_model(&self, p: &mut Pending, over: &EscalateRequest) -> Result<()> {
        // An empty string is what an unchosen `<select>` sends, and it means the
        // same as not sending the field at all.
        let Some(name) = over.template.as_deref().filter(|s| !s.is_empty()) else {
            return Ok(());
        };
        if name == p.profile.name {
            return Ok(());
        }
        let next = self.model(name)?;
        next.require_files()?;
        eprintln!(
            "[rustimage] escalating with '{}' instead of '{}'",
            next.name, p.profile.name
        );
        // The two escalation checkpoints survive the replace, and that is not the
        // field-wise merge this doc comment just argued against: they are
        // request-only, so `next` always has them `None` and there is nothing of
        // the incoming checkpoint's to merge with. They say what the user means
        // to finish with, which no checkpoint has an opinion about — and a saved
        // output's sidecar has to keep saying it after the swap that used them.
        let (refine_model, hires_model) =
            (p.profile.refine_model.clone(), p.profile.hires_model.clone());
        p.profile = next.clone();
        p.profile.refine_model = refine_model;
        p.profile.hires_model = hires_model;
        Ok(())
    }

    /// One model, resolved **now**.
    ///
    /// This used to read a registry built once in [`Engine::new`], which quietly
    /// broke the promise the rest of the program makes about it: `/models` is
    /// served live, so a checkpoint dropped in while the server ran appeared in
    /// the select and then failed here as an unknown model, and a model's saved
    /// defaults never reached a run. Resolving per job is a few header reads
    /// that `discover`'s memo answers from memory, against a multi-second
    /// diffusion — the same trade `Config::registry` documents.
    fn model(&self, name: &str) -> Result<Profile> {
        let mut reg = self.cfg.registry();
        match reg.remove(name) {
            Some(p) => Ok(p),
            None => bail!(
                "unknown model '{name}' — available: {}",
                reg.keys().cloned().collect::<Vec<_>>().join(", ")
            ),
        }
    }

    pub fn adapter_name(&self) -> String {
        self.fsr().adapter_name().to_string()
    }

    pub fn run(&mut self, req: &GenRequest, pin: bool) -> Result<GenResult> {
        let mut profile = self.model(&req.template)?;
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
        if let Some(v) = &req.scheduler {
            if !v.is_empty() {
                profile.scheduler = v.clone();
            }
        }
        if let Some(v) = &req.cache_mode {
            if !v.is_empty() {
                profile.cache_mode = v.clone();
            }
        }
        if let Some(v) = req.width {
            profile.width = v;
        }
        if let Some(v) = req.height {
            profile.height = v;
        }
        // Bounded by `validate()` below, which already holds it to
        // `1.0..=MAX_UPSCALE` — 1.0 being the legal bottom rather than a special
        // case, because that is FSR skipped.
        if let Some(v) = req.upscale_factor {
            profile.upscale_factor = v;
        }
        // Absent keeps the default; present-and-empty is FSR. `named` is what
        // turns the second into `None`, the same rule the checkpoint pickers use.
        if req.upscaler.is_some() {
            profile.upscaler = named(&req.upscaler);
        }
        if let Some(v) = req.refine_steps {
            profile.refine_steps = v;
        }
        if let Some(v) = req.refine_strength {
            profile.refine_strength = v;
        }
        if let Some(v) = req.refine_cfg_scale {
            profile.refine_cfg_scale = Some(v);
        }
        if let Some(v) = req.hires_steps {
            profile.hires_steps = v;
        }
        if let Some(v) = req.hires_strength {
            profile.hires_strength = v;
        }
        if let Some(v) = req.hires_cfg_scale {
            profile.hires_cfg_scale = Some(v);
        }
        // Retained rather than acted on: this generate runs neither pass. It is
        // here so the archived work item names the checkpoints that were chosen
        // to finish it, and reopening the item can put them back in the pickers.
        // An empty string is what an unchosen select sends and means the same as
        // not sending the field, the same rule `swap_model` applies.
        profile.refine_model = named(&req.refine_model);
        profile.hires_model = named(&req.hires_model);
        if let Some(v) = req.sharpness {
            profile.sharpness = v;
        }
        if let Some(v) = req.clarity {
            profile.clarity = v;
        }
        // Whole-list replacement rather than a merge: the picker on the page is
        // the complete answer to "what is attached", so an empty list has to be
        // able to mean none. See [`GenRequest::loras`].
        if let Some(v) = &req.loras {
            profile.loras = v.clone();
        }
        if let Some(parts) = &req.parts {
            self.cfg.apply_parts(&mut profile, parts)?;
        }
        // The overrides can push the denoise strengths out of range even though
        // the profile was fine at load, and this profile is what the later
        // refine/hi-res passes run with.
        profile.validate()?;
        // Before anything expensive: sd.cpp would warn and carry on without a
        // LoRA it can't find, which is two minutes spent on the wrong picture.
        self.cfg.check_loras(&profile.loras, profile.family)?;
        // Resolved here for the same reason: a deleted model file would
        // otherwise fail only after the render it was going to enlarge.
        let upscaler = self.cfg.check_upscaler(&profile.upscaler)?;
        let tpl = &profile;

        profile.require_files()?;
        if req.prompt.trim().is_empty() {
            bail!("prompt is empty");
        }

        let seed = if req.seed < 0 { random_seed() } else { req.seed };
        // Variables are drawn here rather than in the page: the prompt box has to
        // keep `{animal}` for the next image of a run, so what ran and what was
        // typed are different strings — and this is the side that records what
        // ran, into the work item and into the `Pending` every escalation reads.
        // The roll follows `seed`, so the promise the seed box makes still holds.
        // See [`crate::variables`].
        let vars = self.cfg.settings().variables;
        let prompt = variables::expand(&req.prompt, &vars, seed)?;
        let negative = req
            .negative
            .as_deref()
            .map(|n| variables::expand(n, &vars, seed))
            .transpose()?;
        // The other half of what this image is: the sentence as typed, kept
        // only where the roll changed it. Reopening the work item puts this one
        // back in the box rather than the animal it drew, and the seed beside it
        // makes that lossless — see [`crate::variables::typed`].
        let typed_prompt = variables::typed(&req.prompt, &prompt);
        let typed_negative = req
            .negative
            .as_deref()
            .zip(negative.as_deref())
            .and_then(|(src, out)| variables::typed(src, out));
        let id = JOB_COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut scratch = Scratch::default();
        let lowres = scratch.path(self.scratch.join(format!("{id}-0-base.png")));
        let raw = scratch.path(self.scratch.join(format!("{id}-0-raw.png")));
        let display = scratch.path(self.scratch.join(format!("{id}-0-full.png")));

        let sd = self.sd();
        let neg = negative.as_deref();

        // 1) Distilled generation at base resolution.
        let t = Instant::now();
        sd.txt2img(tpl, &prompt, neg, seed, &lowres)?;
        let gen_ms = t.elapsed().as_millis();
        // Read now: an ESRGAN enlarge below is another sd-cli run, which resets it.
        let (cache_skipped, cache_steps) = self.monitor.cache_skip().unwrap_or_default();

        // 2) Upscale, unsharpened — EASU by default, which is cheap and enough
        //    on its own for a draft, or an ESRGAN model. Quality is escalated
        //    afterwards via `refine` and `hires`, both of which read this image
        //    rather than the sharpened one.
        let t = Instant::now();
        let upscale_ms = self.enlarge(
            &lowres,
            &raw,
            tpl.upscale_factor,
            upscaler.as_deref(),
        )?;
        // Nothing reads the base render once it has been upscaled — every
        // escalation works at output resolution now — so it stays in `scratch`
        // and goes when this returns, whichever way it returns.

        // 3) Sharpen for display.
        let filters = Filters::starting_at(tpl.sharpness, tpl.clarity);
        let (png, w, h) = publish(&self.fsr, &raw, &display, &filters)?;
        let fsr_ms = t.elapsed().as_millis() - upscale_ms;

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
            prompt: prompt.clone(),
            negative: negative.clone(),
            typed_prompt: typed_prompt.clone(),
            typed_negative: typed_negative.clone(),
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
            // A txt2img has no source image by definition.
            source: None,
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
            filters,
            profile: profile.clone(),
            prompt: prompt.clone(),
            negative: negative.clone(),
            typed_prompt,
            typed_negative,
            seed,
            work_id,
            source: None,
            refine_passes: 0,
            hires_passes: 0,
            step: 0,
        };
        let result = entry.result(id, png, Timings {
            gen_ms,
            upscale_ms,
            fsr_ms,
            cache_skipped,
            cache_steps,
            ..Default::default()
        });
        // `pin` is a run holding this image for the hi-res and refine still to
        // come — see [`Live::pins`].
        lock(&self.retained).store(id, Slot::Image, entry, pin);
        Ok(result)
    }

    /// One img2img pass over an image the user supplied, producing a result that
    /// joins the ordinary ladder: refine, hi-res, the sliders and Save all work
    /// on it afterwards exactly as they do on a generation.
    ///
    /// Structurally this is `run` with the txt2img swapped for a source image,
    /// and `open`'s copy-in rule bolted to the front. Both halves matter:
    ///
    /// * The source lives in `work/output/` and belongs to the user. It is
    ///   **copied** into scratch, never adopted — a `Pending` pointing at it
    ///   would have `remove_files` delete the user's own image on the first
    ///   refine, which is invariant 8's failure in a new place.
    /// * There is no EASU stage. `run` generates small and upscales because that
    ///   is cheap; here the pixels already exist at whatever size they exist at,
    ///   and the only sizing question is whether that size is affordable — see
    ///   [`fit_to`].
    pub fn img2img(&mut self, req: &Img2ImgRequest, pin: bool) -> Result<GenResult> {
        let mut profile = self.model(&req.template)?;
        // The same fold as `run`, minus width/height: this pass sends no -W/-H,
        // so a base size would be a box that does nothing.
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
        if let Some(v) = &req.scheduler {
            if !v.is_empty() {
                profile.scheduler = v.clone();
            }
        }
        if let Some(v) = &req.cache_mode {
            if !v.is_empty() {
                profile.cache_mode = v.clone();
            }
        }
        if let Some(v) = req.source_strength {
            profile.source_strength = v;
        }
        if let Some(v) = req.refine_steps {
            profile.refine_steps = v;
        }
        if let Some(v) = req.refine_strength {
            profile.refine_strength = v;
        }
        if let Some(v) = req.refine_cfg_scale {
            profile.refine_cfg_scale = Some(v);
        }
        if let Some(v) = req.hires_steps {
            profile.hires_steps = v;
        }
        if let Some(v) = req.hires_strength {
            profile.hires_strength = v;
        }
        if let Some(v) = req.hires_cfg_scale {
            profile.hires_cfg_scale = Some(v);
        }
        // See `run`: retained, not acted on.
        profile.refine_model = named(&req.refine_model);
        profile.hires_model = named(&req.hires_model);
        if let Some(v) = req.sharpness {
            profile.sharpness = v;
        }
        if let Some(v) = req.clarity {
            profile.clarity = v;
        }
        if let Some(v) = &req.loras {
            profile.loras = v.clone();
        }
        if let Some(parts) = &req.parts {
            self.cfg.apply_parts(&mut profile, parts)?;
        }
        profile.validate()?;
        self.cfg.check_loras(&profile.loras, profile.family)?;

        profile.require_files()?;
        if req.prompt.trim().is_empty() {
            bail!("prompt is empty");
        }

        let stored = self.output_source(&req.source)?;
        let seed = if req.seed < 0 { random_seed() } else { req.seed };
        // The same roll as a txt2img's, and for the same reasons — see `run`.
        let vars = self.cfg.settings().variables;
        let prompt = variables::expand(&req.prompt, &vars, seed)?;
        let negative = req
            .negative
            .as_deref()
            .map(|n| variables::expand(n, &vars, seed))
            .transpose()?;
        // The typed pair, exactly as in `run` — and it matters here too, because
        // an img2img generation is reopened through the same grid.
        let typed_prompt = variables::typed(&req.prompt, &prompt);
        let typed_negative = req
            .negative
            .as_deref()
            .zip(negative.as_deref())
            .and_then(|(src, out)| variables::typed(src, out));
        let id = JOB_COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut scratch = Scratch::default();
        let src = scratch.path(self.scratch.join(format!("{id}-0-src.png")));
        let raw = scratch.path(self.scratch.join(format!("{id}-0-raw.png")));
        let display = scratch.path(self.scratch.join(format!("{id}-0-full.png")));

        // 1) Bring the source in at a size this card can diffuse at. A phone
        //    photo is 4032x3024 and a pass at that size is an out-of-memory on a
        //    24 GB card, so this is not optional politeness — a tab with no
        //    ceiling here is unusable on its most obvious input.
        let t = Instant::now();
        let budget = req.max_side.unwrap_or(profile.hires_max_side);
        let (sw, sh) = image::image_dimensions(&stored)
            .with_context(|| format!("reading {}", stored.display()))?;
        let (fw, fh) = fit_to((sw, sh), budget);
        if (fw, fh) == (sw, sh) {
            // Byte-for-byte, and no re-encode: whatever is fed to a diffusion
            // pass should not already be a generation of loss.
            std::fs::copy(&stored, &src)
                .with_context(|| format!("copying {}", stored.display()))?;
        } else if fw < sw {
            // Minification. Deliberately not EASU: it is a magnifier, and run
            // backwards it aliases. Lanczos3 is the standard answer and this
            // runs once per generation, against a diffusion measured in seconds.
            let img = image::open(&stored)
                .with_context(|| format!("loading {}", stored.display()))?;
            image::imageops::resize(&img, fw, fh, image::imageops::FilterType::Lanczos3)
                .save(&src)
                .with_context(|| format!("writing {}", src.display()))?;
        } else {
            // Magnification, which is EASU's whole job — and unsharpened, like
            // everything that feeds a diffusion pass.
            self.fsr()
                .process(&stored, &src, fw as f32 / sw as f32, &Filters::NONE)?;
        }
        let fsr_ms_in = t.elapsed().as_millis();

        // 2) The pass itself. `big_image` is asked of the size it will actually
        //    run at, which after step 1 is the source's size and not the
        //    original's.
        let t = Instant::now();
        self.sd().img2img(
            &profile,
            &prompt,
            negative.as_deref(),
            seed,
            &src,
            &raw,
            profile.source_strength,
            profile.steps,
            profile.cfg_scale,
            fw.max(fh) > BIG_IMAGE_SIDE,
            "img2img source",
        )?;
        let gen_ms = t.elapsed().as_millis();
        let (cache_skipped, cache_steps) = self.monitor.cache_skip().unwrap_or_default();

        // 3) Sharpen for display. Dimensions come from here, not from `fit_to`:
        //    sd.cpp rounds an init image to the latent grid and only warns.
        let t = Instant::now();
        let filters = Filters::starting_at(profile.sharpness, profile.clarity);
        let (png, w, h) = publish(&self.fsr, &raw, &display, &filters)?;
        let fsr_ms = fsr_ms_in + t.elapsed().as_millis();

        scratch.keep(&raw);
        scratch.keep(&display);
        // `src` is not kept: nothing reads it again, and the image it was copied
        // from is still sitting in `work/output/` where the user put it.

        let mut item = WorkItem {
            id: workitems::new_id(seed),
            created_ms: now_ms(),
            prompt: prompt.clone(),
            negative: negative.clone(),
            typed_prompt: typed_prompt.clone(),
            typed_negative: typed_negative.clone(),
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
            source: Some(req.source.clone()),
            profile: profile.clone(),
        };
        let work_id = match self.items.save(&raw, &mut item) {
            Ok(()) => Some(item.id),
            Err(e) => {
                eprintln!("[rustimage] could not archive work item: {e:#}");
                None
            }
        };

        let entry = Pending {
            raw,
            raw_w: w,
            raw_h: h,
            display,
            filters,
            profile,
            prompt: prompt.clone(),
            negative: negative.clone(),
            typed_prompt,
            typed_negative,
            seed,
            work_id,
            source: Some(req.source.clone()),
            refine_passes: 0,
            hires_passes: 0,
            step: 0,
        };
        let result = entry.result(id, png, Timings {
            gen_ms,
            fsr_ms,
            cache_skipped,
            cache_steps,
            ..Default::default()
        });
        lock(&self.retained).store(id, Slot::Image, entry, pin);
        Ok(result)
    }

    /// Generate a clip.
    ///
    /// Deliberately not an escalation ladder. A still is cheap to make and
    /// expensive to perfect, so the image side generates small and climbs; a
    /// clip is expensive to make at all, and the two things that would improve
    /// one — FSR over the frames, a VACE pass over the frames — both read the
    /// frame sequence this writes, so they are separate actions on a finished
    /// clip rather than rungs held in memory. That is why there is no
    /// `VideoPending` and no eviction: the archive *is* the state.
    ///
    /// `req.source` names an image in `work/output/`, which becomes the first
    /// frame. Leaving it out runs the same checkpoint as plain text-to-video.
    pub fn video(&mut self, req: &VideoRequest) -> Result<VideoResult> {
        let mut m = self.cfg.video_model(&req.model)?;
        // Per-request overrides onto a *clone*, which is then stored in the
        // clip's sidecar — the same property `Engine::run` gives a still, and
        // the reason a clip can be reproduced from its own record.
        if let Some(v) = req.width {
            m.width = v;
        }
        if let Some(v) = req.height {
            m.height = v;
        }
        if let Some(v) = req.frames {
            m.frames = v;
        }
        if let Some(v) = req.fps {
            m.fps = v;
        }
        if let Some(v) = req.steps {
            m.steps = v;
        }
        if let Some(v) = req.cfg_scale {
            m.cfg_scale = v;
        }
        // Stays an `Option`, like `refine_cfg_scale`: unset means the expert
        // follows `cfg_scale`, and pinning it here would sever that.
        if let Some(v) = req.high_noise_cfg_scale {
            m.high_noise_cfg_scale = Some(v);
        }
        if let Some(v) = req.high_noise_steps {
            m.high_noise_steps = Some(v);
        }
        if let Some(v) = req.flow_shift {
            m.flow_shift = Some(v);
        }
        if let Some(v) = &req.sampler {
            if !v.is_empty() {
                m.sampler = v.clone();
            }
        }
        if let Some(v) = req.vae_tiling {
            m.vae_tiling = v;
        }
        m.validate()?;

        if !m.exists {
            let missing: Vec<String> =
                m.missing().iter().map(|p| p.display().to_string()).collect();
            bail!(
                "video model '{}' is missing {} of its files:\n  {}",
                m.name,
                missing.len(),
                missing.join("\n  ")
            );
        }
        if req.prompt.trim().is_empty() {
            bail!("prompt is empty");
        }

        // The first frame, if there is one. Taken from `work/output/` rather
        // than the work item archive because that is the finished work — the
        // images that were worth keeping are the ones worth animating.
        let init = match &req.source {
            Some(stem) => Some(self.output_source(stem)?),
            None => None,
        };
        // An explicit negative wins, including an explicitly empty one; absent
        // inherits the model's, which for Wan is its own published prompt.
        let negative = req
            .negative
            .clone()
            .unwrap_or_else(|| m.negative_prompt.clone());

        let seed = if req.seed < 0 { random_seed() } else { req.seed };
        // The same roll as a still's, over both prompts — see `run`. The
        // negative has already picked up the model's published one by here, and
        // that is plain text with no braces in it.
        let vars = self.cfg.settings().variables;
        let prompt = variables::expand(&req.prompt, &vars, seed)?;
        let rolled = variables::expand(&negative, &vars, seed)?;
        // Both typed halves are taken before the shadowing below, which is the
        // whole reason the rolled negative gets a name of its own here.
        let typed_prompt = variables::typed(&req.prompt, &prompt);
        let typed_negative = variables::typed(&negative, &rolled);
        let negative = rolled;
        let id = workitems::new_id(seed);
        let (pattern, mut dir) = self.videos.prepare(&id)?;

        let t = Instant::now();
        self.sd().video(
            &m,
            &prompt,
            &negative,
            seed,
            &VideoIo {
                init: init.as_deref(),
                control: None,
                out: &pattern,
            },
        )?;
        let gen_ms = t.elapsed().as_millis();

        let (_, _, frames) = m.aligned();
        let mut clip = Clip {
            id,
            created_ms: now_ms(),
            prompt,
            negative,
            typed_prompt,
            typed_negative,
            seed,
            model: req.model.clone(),
            // Placeholders: `finish` replaces all three from the frames on disk,
            // because sd.cpp aligns a size up and a frame count down and only
            // warns about it.
            width: 0,
            height: 0,
            frames,
            fps: m.effective_fps(),
            thumb_width: 0,
            thumb_height: 0,
            source: req.source.clone(),
            video: m,
        };
        self.videos.finish(&mut clip)?;
        // Past every `?`, so the guard stops being a sweeper. Everything before
        // this point leaves no half-written clip behind — which is the ordinary
        // path, not the rare one, because that is what Stop does.
        dir.keep();
        Ok(VideoResult { clip, gen_ms })
    }

    /// Run every frame of a finished clip through the FSR passes and write the
    /// result into `work/output/{id}-x{factor}/`.
    ///
    /// This is the clip's analogue of the hi-res button, and it is the reason
    /// the generator writes frames rather than a container. Two properties make
    /// it work on video, and they are worth stating because "AI upscaling" is
    /// assumed to shimmer:
    ///
    /// * **A fixed kernel cannot flicker.** EASU, RCAS and clarity are
    ///   deterministic functions of the pixels, so two near-identical frames
    ///   produce two near-identical outputs. Stability is by construction.
    /// * **Invariant 6 covers the size change**: both display filters size their
    ///   band from the image, so a sharpness setting means the same thing after
    ///   the enlargement as before it.
    ///
    /// What must *not* happen here is re-diffusion. A per-frame img2img draws
    /// its own noise and invents its own detail with no knowledge of its
    /// neighbours; adjacent frames then diverge structurally and the eye reads
    /// that as boiling. Refining a clip properly is a video-native pass —
    /// `--control-video` over this same frame directory — not this function
    /// with a sampler bolted on.
    ///
    /// Output goes to a *subdirectory* rather than flat into `work/output/`,
    /// which the picker lists: 33 near-identical tiles would bury the images
    /// someone actually wants to animate, and a folder of frames is what an
    /// editor wants anyway.
    pub fn enlarge_clip(
        &mut self,
        id: &str,
        factor: f32,
        filters: &Filters,
    ) -> Result<EnlargeResult> {
        let clip = self.videos.load(id)?;
        if !(1.0..=MAX_UPSCALE).contains(&factor) {
            bail!("upscale factor must be between 1.0 and {MAX_UPSCALE}");
        }
        filters.validate(&clip.id)?;
        let frames = self.videos.frames(id);
        if frames.is_empty() {
            bail!("clip '{id}' has no frames on disk");
        }

        let name = format!("{id}-x{}", (factor * 100.0).round() as u32);
        let out_dir = self.items.output_dir().join(&name);
        std::fs::create_dir_all(&out_dir)
            .with_context(|| format!("creating {}", out_dir.display()))?;

        let t = Instant::now();
        let mut size = (0, 0);
        for (i, frame) in frames.iter().enumerate() {
            let dest = out_dir.join(format!("frame_{i:03}.png"));
            size = self.fsr().process(frame, &dest, factor, filters)?;
        }
        Ok(EnlargeResult {
            dir: out_dir,
            frames: frames.len() as u32,
            width: size.0,
            height: size.1,
            ms: t.elapsed().as_millis(),
        })
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
    pub fn refine(&mut self, id: u64, over: &EscalateRequest) -> Result<GenResult> {
        let mut p = lock(&self.retained).get(id, "refine")?;
        // Model first, then the boxes on top of it: a swap replaces the whole
        // profile, so applying the overrides the other way round would throw
        // them away. Both before `can_refine`, so the check reads the settings
        // this pass will actually run with rather than the ones it inherited.
        self.swap_model(&mut p, over)?;
        over.apply(&mut p.profile)?;
        // What this pass is about to run, written back so a `save-output` sidecar
        // records it. Only when the request names one: an unchosen select sends
        // an empty string and changes nothing, which is the same rule
        // `swap_model` applies to the swap itself.
        if let Some(name) = named(&over.template) {
            p.profile.refine_model = Some(name);
        }
        // The generate checked these, but that was possibly hours ago on a
        // `Pending` that has been sitting in the cache since. A file deleted in
        // between is the same silent drop arriving late. Note a model swap has
        // already emptied the list by this point, since it replaces the whole
        // profile — correct, because a LoRA is trained against one base.
        self.cfg.check_loras(&p.profile.loras, p.profile.family)?;
        if !p.can_refine() {
            bail!("'{}' has no refine pass configured (refine_steps = 0)", p.profile.name);
        }

        let step = p.step + 1;
        let mut scratch = Scratch::default();
        let raw = scratch.path(self.scratch.join(format!("{id}-{step}-raw.png")));
        let display = scratch.path(self.scratch.join(format!("{id}-{step}-full.png")));
        let sd = self.sd();

        // Uncached: a finishing pass is the quality rung (`Profile::uncached`).
        let t = Instant::now();
        sd.img2img(
            &p.profile.uncached(),
            &p.prompt,
            p.negative.as_deref(),
            p.seed,
            &p.raw,
            &raw,
            p.profile.refine_strength,
            p.profile.refine_steps,
            p.profile.refine_cfg(),
            p.big_image(),
            "img2img refine",
        )?;
        let refine_ms = t.elapsed().as_millis();

        let t = Instant::now();
        // Dimensions come back from here rather than carrying over: sd.cpp
        // rounds an init image to what the latent grid allows, so the pass can
        // return a slightly different size than it was given.
        let filters = p.filters.restart(p.profile.sharpness, p.profile.clarity);
        let (png, w, h) = publish(&self.fsr, &raw, &display, &filters)?;
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
            // Detail back to the model's starting point, the grade kept — see
            // `Filters::restart`.
            filters,
            refine_passes: p.refine_passes + 1,
            step,
            ..p
        };
        let result = entry.result(id, png, Timings { fsr_ms, refine_ms, ..Default::default() });
        self.keep(id, entry);
        Ok(result)
    }

    /// The hi-res pass: diffuse at the output resolution to add detail the FSR
    /// upscale can't invent, optionally enlarging on the way.
    ///
    /// Diffusion cost climbs quadratically with pixel count and SDXL's quality
    /// falls off past `hires_max_side`, while FSR is nearly free at any size —
    /// so an upscale beyond the budget diffuses as large as it can afford, then
    /// covers the remaining distance with a plain FSR pass.
    pub fn hires(&mut self, id: u64, upscale: f32, over: &EscalateRequest) -> Result<GenResult> {
        if !(1.0..=MAX_UPSCALE).contains(&upscale) {
            bail!("upscale must be between 1 and {MAX_UPSCALE}");
        }
        let mut p = lock(&self.retained).get(id, "run a hi-res pass on")?;
        // See `refine`: the swap replaces the profile, so it has to come first.
        // Note this can also move `hires_max_side`, and therefore whether the
        // pass is affordable at all — `can_hires` below is asked of the model
        // that will run it.
        self.swap_model(&mut p, over)?;
        over.apply(&mut p.profile)?;
        // See `refine`.
        if let Some(name) = named(&over.template) {
            p.profile.hires_model = Some(name);
        }
        self.cfg.check_loras(&p.profile.loras, p.profile.family)?;
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
        let upscaled = scratch.path(self.scratch.join(format!("{id}-{step}-up.png")));
        let hires = scratch.path(self.scratch.join(format!("{id}-{step}-hires.png")));
        let raw = scratch.path(self.scratch.join(format!("{id}-{step}-raw.png")));
        let display = scratch.path(self.scratch.join(format!("{id}-{step}-full.png")));
        let sd = self.sd();
        let mut fsr_ms = 0u128;

        // 1) Enlarge to the diffusion size, if that's bigger than what we have.
        //    Unsharpened, like everything that feeds a diffusion pass.
        let init = if diff_w > p.raw_w {
            let t = Instant::now();
            self.fsr().process(
                &p.raw,
                &upscaled,
                diff_w as f32 / p.raw_w as f32,
                &Filters::NONE,
            )?;
            fsr_ms += t.elapsed().as_millis();
            upscaled.clone()
        } else {
            p.raw.clone()
        };

        // 2) The detail pass itself.
        let t = Instant::now();
        // Uncached, as refine is: see `Profile::uncached`.
        sd.img2img(
            &p.profile.uncached(),
            &p.prompt,
            p.negative.as_deref(),
            p.seed,
            &init,
            &hires,
            p.profile.hires_strength,
            p.profile.hires_steps,
            p.profile.hires_cfg(),
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
        self.fsr().process(&hires, &raw, scale, &Filters::NONE)?;
        // `hires` and `upscaled` have both been consumed; the guard drops them
        // on the way out, and would have dropped them just the same had the
        // pass died between here and the top of the function.

        // 4) Sharpen for display.
        let filters = p.filters.restart(p.profile.sharpness, p.profile.clarity);
        let (png, w, h) = publish(&self.fsr, &raw, &display, &filters)?;
        fsr_ms += t.elapsed().as_millis();

        scratch.keep(&raw);
        scratch.keep(&display);
        p.remove_files();
        let entry = Pending {
            raw,
            raw_w: w,
            raw_h: h,
            display,
            // Detail back to the model's starting point, the grade kept — see
            // `Filters::restart`.
            filters,
            hires_passes: p.hires_passes + 1,
            step,
            ..p
        };
        let result = entry.result(id, png, Timings { fsr_ms, hires_ms, ..Default::default() });
        self.keep(id, entry);
        Ok(result)
    }

    /// See [`enlarge`], which this is wired to the engine's own FSR device and
    /// monitored runner — so a model pass shows on the progress bar and Stop
    /// can kill it, like every other sd-cli run.
    fn enlarge(&self, src: &Path, dst: &Path, factor: f32, model: Option<&Path>) -> Result<u128> {
        enlarge(&self.fsr, &self.sd(), src, dst, factor, model)
    }

    /// Put an escalation's result back where its image was shown.
    fn keep(&self, id: u64, entry: Pending) {
        let mut r = lock(&self.retained);
        let slot = r.slot_of(id);
        r.store(id, slot, entry, false);
    }
}

/// The display lane: everything that only re-filters, reopens or copies an
/// image, on a thread of its own so none of it waits behind a render. It
/// shares the engine's FSR device and its [`Retained`] — the sliders work on
/// the image a run just produced — and owns no sd-cli at all, which is what
/// makes it safe to run beside one.
pub struct Display {
    fsr: SharedFsr,
    scratch: PathBuf,
    items: workitems::Store,
    retained: Arc<Mutex<Retained>>,
}

impl Display {
    /// Re-run the display filters on an existing result. No diffusion: this is
    /// [`publish`] again on the retained `raw`, which is what makes it a
    /// millisecond operation and therefore something a slider can drive.
    ///
    /// The escalations are the expensive rungs of the ladder; this is the free
    /// one. It is only possible because no pass ever diffuses over sharpened
    /// pixels — `raw` is still exactly what the last diffusion produced, so
    /// re-filtering it is lossless no matter how many times it is done.
    ///
    /// A [`FiltersPatch`], so an absent knob keeps its current value — matching
    /// how overrides work everywhere else here, and keeping a one-filter request
    /// a one-field body. The change applies to this image only, and only until
    /// the next pass — see [`Pending::filters`].
    pub fn adjust(&mut self, id: u64, patch: &FiltersPatch) -> Result<GenResult> {
        let mut p = lock(&self.retained).get(id, "adjust")?;
        patch.apply(&mut p.filters);
        // Same bounds the config goes through, from the same place.
        p.filters.validate(&p.profile.name)?;

        let t = Instant::now();
        // Straight back over the same `display` path, and `step` doesn't move:
        // `display` is only ever an output — no pass reads it — so there is no
        // new file to make, and a drag can't churn the work dir or push older
        // results out of the cache.
        //
        // Deliberately no `Scratch` here, unlike the three passes above. This
        // writes over a file the retained `Pending` already owns, so putting it
        // under the guard would mean a failed drag deleting a live image.
        let (png, w, h) = publish(&self.fsr, &p.raw, &p.display, &p.filters)?;
        let fsr_ms = t.elapsed().as_millis();

        let entry = Pending {
            raw_w: w,
            raw_h: h,
            ..p
        };
        let result = entry.result(id, png, Timings { fsr_ms, ..Default::default() });
        // Only over the entry it read. A pass on the other lane may have
        // replaced it meanwhile, and the drag belongs to the image it replaced.
        if !lock(&self.retained).update(id, entry) {
            bail!("that image was replaced while it was being adjusted");
        }
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
    pub fn open(&mut self, work_id: &str, shelf: Shelf) -> Result<GenResult> {
        let item = self.items.load(shelf, work_id)?;
        let stored = self.items.image_path(shelf, &item.id);

        let id = JOB_COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut scratch = Scratch::default();
        let raw = scratch.path(self.scratch.join(format!("{id}-0-raw.png")));
        let display = scratch.path(self.scratch.join(format!("{id}-0-full.png")));

        let t = Instant::now();
        std::fs::copy(&stored, &raw)
            .with_context(|| format!("reopening {}", stored.display()))?;
        // Filters start where the model says again, exactly as a fresh generate
        // does — the archived image is unsharpened, so there is nothing to undo.
        let filters = Filters::starting_at(item.profile.sharpness, item.profile.clarity);
        let (png, w, h) = publish(&self.fsr, &raw, &display, &filters)?;
        let fsr_ms = t.elapsed().as_millis();

        scratch.keep(&raw);
        scratch.keep(&display);

        let entry = Pending {
            raw,
            raw_w: w,
            raw_h: h,
            display,
            filters,
            profile: item.profile,
            prompt: item.prompt,
            negative: item.negative,
            typed_prompt: item.typed_prompt,
            typed_negative: item.typed_negative,
            seed: item.seed,
            work_id: Some(item.id),
            // Restored from the sidecar, so reopening an image-to-image
            // generation puts its source back rather than quietly forgetting
            // where the picture came from.
            source: item.source,
            refine_passes: item.refine_passes,
            hires_passes: item.hires_passes,
            step: 0,
        };
        let result = entry.result(id, png, Timings { fsr_ms, ..Default::default() });
        lock(&self.retained).store(id, Slot::Image, entry, false);
        Ok(result)
    }

    /// Bring a finished image in `work/output/` back as a live result, so the
    /// viewer's sliders can work on it. The Outputs tab's one route.
    ///
    /// A near-twin of [`Display::open`] — copy in, `publish`, mint a `Pending` —
    /// and it is copied into scratch for exactly the same reason: that file
    /// belongs to the user, and a `Pending` pointing at it would have
    /// `remove_files` delete it. Invariant 8, in the third place it applies.
    ///
    /// Three things differ from reopening a work item, and all three come from
    /// the same fact: **`work/output/` holds `display`, not `raw`.** A saved
    /// image already has its grade and its RCAS baked in.
    ///
    /// 1. The filters start at [`Filters::NONE`], not at the model's values.
    ///    The picture already carries whatever was applied when it was saved, so
    ///    0 means "as saved" — starting at `profile.sharpness` would sharpen an
    ///    already sharpened image the moment it appeared.
    /// 2. The retained profile's `sharpness`/`clarity` are zeroed for the same
    ///    reason, because [`Engine::result`] derives `start` — where **Reset**
    ///    goes — from them. They look like values worth keeping and are the one
    ///    pair that must not be.
    /// 3. This is therefore *not* the lossless re-filter the sliders are
    ///    elsewhere (invariant 1): `raw` here is a filtered image, so a drag
    ///    stacks on what is already there. That is inherent to editing a
    ///    finished file, and it is why the tab offers no escalation — a
    ///    diffusion over these pixels is the thing invariant 1 exists to
    ///    prevent.
    ///
    /// A missing sidecar is not an error: `list_outputs` lists a PNG dropped in
    /// by hand, so this has to be able to open one. It gets `Profile::default()`
    /// and no prompt, which is all the sliders need.
    pub fn open_output(&mut self, stem: &str) -> Result<GenResult> {
        let stored = output_source(&self.items, stem)?;
        let item = self.items.load_output(stem);

        let id = JOB_COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut scratch = Scratch::default();
        let raw = scratch.path(self.scratch.join(format!("{id}-0-raw.png")));
        let display = scratch.path(self.scratch.join(format!("{id}-0-full.png")));

        let t = Instant::now();
        std::fs::copy(&stored, &raw)
            .with_context(|| format!("opening {}", stored.display()))?;
        let filters = Filters::NONE;
        let (png, w, h) = publish(&self.fsr, &raw, &display, &filters)?;
        let fsr_ms = t.elapsed().as_millis();

        scratch.keep(&raw);
        scratch.keep(&display);

        let mut profile = item.as_ref().map(|i| i.profile.clone()).unwrap_or_default();
        // See (2) above: these two are where the sliders start, and for this one
        // image "where they start" is off.
        profile.sharpness = 0.0;
        profile.clarity = 0.0;

        let entry = Pending {
            raw,
            raw_w: w,
            raw_h: h,
            display,
            filters,
            profile,
            prompt: item.as_ref().map(|i| i.prompt.clone()).unwrap_or_default(),
            negative: item.as_ref().and_then(|i| i.negative.clone()),
            typed_prompt: item.as_ref().and_then(|i| i.typed_prompt.clone()),
            typed_negative: item.as_ref().and_then(|i| i.typed_negative.clone()),
            seed: item.as_ref().map(|i| i.seed).unwrap_or(-1),
            // Kept so a save out of this tab lands as `{id}-s{step}-2.png`,
            // grouped with the generation it came from — which is what
            // `save_output` means by naming a file after its origin. `free_stem`
            // is what stops it overwriting the image being edited.
            work_id: item.as_ref().map(|i| i.id.clone()),
            source: item.as_ref().and_then(|i| i.source.clone()),
            refine_passes: item.as_ref().map(|i| i.refine_passes).unwrap_or(0),
            hires_passes: item.as_ref().map(|i| i.hires_passes).unwrap_or(0),
            step: item.as_ref().map(|i| i.step).unwrap_or(0),
        };
        let result = entry.result(id, png, Timings { fsr_ms, ..Default::default() });
        lock(&self.retained).store(id, Slot::Output, entry, false);
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
        let p = lock(&self.retained).get(id, "save")?;
        let item = WorkItem {
            // Named by origin, so every save out of one generation groups
            // together; the job id only stands in when the archive write failed.
            id: p.work_id.clone().unwrap_or_else(|| id.to_string()),
            created_ms: now_ms(),
            prompt: p.prompt.clone(),
            negative: p.negative.clone(),
            typed_prompt: p.typed_prompt.clone(),
            typed_negative: p.typed_negative.clone(),
            seed: p.seed,
            template: p.profile.name.clone(),
            width: p.raw_w,
            height: p.raw_h,
            thumb_width: 0,
            thumb_height: 0,
            refine_passes: p.refine_passes,
            hires_passes: p.hires_passes,
            step: p.step,
            sharpness: p.filters.sharpness,
            clarity: p.filters.clarity,
            source: p.source.clone(),
            profile: p.profile.clone(),
        };
        self.items.save_output(&p.display, &item)
    }
}

/// See [`Engine::output_source`]; a free function so the display lane's
/// [`Display::open_output`] runs the same check.
fn output_source(items: &workitems::Store, stem: &str) -> Result<PathBuf> {
    if !workitems::valid_id(stem) {
        bail!("'{stem}' is not an output name");
    }
    // Either gallery: a favourite is a source like any other, and a stem
    // names one picture wherever it lives (`workitems::Gallery`).
    items
        .find_output(stem)
        .with_context(|| format!("no saved image '{stem}' in work/output/ or your favourites"))
}

/// The one place the display filters are applied. Puts the retained raw
/// image through the grade, clarity and RCAS passes into `display` and
/// returns the PNG bytes for the response.
///
/// Scale 1.0, so [`Fsr::process`] skips EASU, and each of the other three is
/// skipped when its knobs are neutral — microseconds against the diffusion
/// it follows. At [`Filters::NONE`] it is a byte-exact copy.
///
/// It also reports the true dimensions, which is how each pass learns what
/// sd.cpp actually produced rather than what it was asked for.
fn publish(
    fsr: &Mutex<Fsr>,
    raw: &Path,
    display: &Path,
    filters: &Filters,
) -> Result<(Vec<u8>, u32, u32)> {
    let (w, h) = lock(fsr).process(raw, display, 1.0, filters)?;
    let png = std::fs::read(display)?;
    Ok((png, w, h))
}

impl Pending {
    fn result(&self, id: u64, png: Vec<u8>, timings: Timings) -> GenResult {
        GenResult {
            png,
            width: self.raw_w,
            height: self.raw_h,
            seed: self.seed,
            timings,
            refine_passes: self.refine_passes,
            hires_passes: self.hires_passes,
            image_id: id,
            work_id: self.work_id.clone(),
            can_refine: self.can_refine(),
            can_hires: self.can_hires(),
            filters: self.filters,
            start: Filters::starting_at(self.profile.sharpness, self.profile.clarity),
            prompt: self.prompt.clone(),
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

/// The size an img2img pass should run a source image at, given the longest side
/// it can afford.
///
/// Only ever *shrinks*, and only when the source is over budget. A source
/// already inside it comes back untouched, which is what lets the caller skip
/// the resize entirely and copy the bytes — a re-encode of an image that needed
/// no resizing is a generation of loss for nothing.
///
/// **Only the width is snapped to 64**, and the height follows from it — the
/// same asymmetry `hires` has, where one scale factor drives both axes. Snapping
/// both was the obvious thing and is wrong: 5000×2000 into a 1536 budget snaps
/// to 1536×576 instead of 1536×614, a 6.7% change of aspect that reads as a
/// subtly squashed picture and that nothing downstream would report. The height
/// is left to sd.cpp, which aligns an init image to the latent grid regardless
/// and only warns — which is why `publish` is where the true size comes from
/// (invariant 5).
///
/// Enlarging is deliberately not done here even when the source is small — a
/// budget is a ceiling, not a target, and stretching a 512² thumbnail to 2048²
/// before diffusing it is a decision for the caller to ask for, not one to
/// inherit from a limit.
fn fit_to(src: (u32, u32), max_side: u32) -> (u32, u32) {
    let (w, h) = src;
    let long = w.max(h);
    if long == 0 || max_side == 0 || long <= max_side {
        return (w, h);
    }
    let scale = max_side as f32 / long as f32;
    let fw = snap64(((w as f32) * scale).round() as u32);
    // From the *snapped* width, not from the scale, so the two axes can't
    // disagree about which rounding they followed.
    let fh = ((h as f32) * (fw as f32 / w.max(1) as f32)).round() as u32;
    (fw, fh.max(1))
}

/// Enlarge `src` into `dst` by `factor`, unsharpened, with EASU or with an
/// upscaler model. Returns the milliseconds the model's share took — 0 when
/// EASU did it all.
///
/// A model runs by one of two routes, decided by what the file *is*
/// ([`crate::sr::identify`]): an ESRGAN through sd-cli, which has its own
/// loader and kernels for exactly that network, and anything else through
/// [`crate::sr`] on the FSR device. Everything after the model is the same
/// for both.
///
/// The one place an enlarge is chosen, so it is the helper for every pass
/// that makes pixels bigger. A free function rather than only a method so the
/// `upscale` subcommand can run it without an `Engine`, whose constructor
/// sweeps `work/`. Only `Engine::run` calls it today; hi-res's two EASU
/// steps and `enlarge_clip` are the obvious next callers, and every model
/// here is a deterministic function of the pixels, so a clip may use one
/// (invariant 9).
///
/// **The output is `fsr::out_size(src, factor)` whichever upscaler ran.** An
/// upscaler model enlarges by the fixed scale it was trained at, and the
/// difference is covered afterwards (see [`after_model`]): a model that
/// overshot is reduced with Lanczos, one that fell short is finished by
/// EASU. So the size the page quoted before the click stays true.
///
/// `model` is ignored at a factor of 1: that is "no upscale", and a
/// 4× pass reduced back to 1× is minutes spent on a resample.
pub fn enlarge(
    fsr: &Mutex<Fsr>,
    sd: &SdRunner,
    src: &Path,
    dst: &Path,
    factor: f32,
    model: Option<&Path>,
) -> Result<u128> {
    // The lock is taken per GPU step, never around the whole call: an ESRGAN
    // runs in sd-cli, and holding the device across that would stall every
    // slider on the display lane for the length of it.
    let Some(model) = model.filter(|_| factor > 1.0) else {
        lock(fsr).process(src, dst, factor, &Filters::NONE)?;
        return Ok(0);
    };
    let t = Instant::now();
    let input = image::image_dimensions(src)
        .with_context(|| format!("reading the size of {}", src.display()))?;
    // Beside `dst` so it shares its job id and its folder; a `.png` extension
    // so `purge_pngs` reaches it after a crash. Its own guard, since it never outlives this.
    let mut scratch = Scratch::default();
    let big = scratch.path(dst.with_extension("model.png"));
    if crate::sr::is_esrgan(model)? {
        sd.upscale(model, src, &big)?;
    } else {
        let r = {
            let fsr = lock(fsr);
            crate::sr::upscale(fsr.device(), fsr.queue(), model, src, &big)?
        };
        // One line per run: the tile is chosen from the device's buffer limit,
        // so it is worth being able to see what it came out as.
        eprintln!(
            "[rustimage] {} {}× in {} tile(s) of {} px, {} ms",
            r.arch, r.scale, r.tiles, r.tile, r.ms
        );
    }
    let got = image::image_dimensions(&big).with_context(|| {
        format!("the upscaler reported success but wrote no image to {}", big.display())
    })?;
    match after_model(input, got, factor)
        .with_context(|| format!("upscaling with {}", model.display()))?
    {
        AfterModel::Keep => {
            std::fs::rename(&big, dst)
                .with_context(|| format!("moving the upscale to {}", dst.display()))?;
        }
        AfterModel::Reduce(w, h) => reduce(&big, dst, w, h)?,
        AfterModel::Easu(rest) => {
            let ms = t.elapsed().as_millis();
            lock(fsr).process(&big, dst, rest, &Filters::NONE)?;
            return Ok(ms);
        }
    }
    Ok(t.elapsed().as_millis())
}

/// What a model's result still needs to reach `fsr::out_size(input, factor)`.
#[derive(Debug, Clone, Copy, PartialEq)]
enum AfterModel {
    /// The model's scale was the factor asked for.
    Keep,
    /// It overshot — the common case, a 4× model asked for 2× — so resample
    /// down to exactly this size.
    Reduce(u32, u32),
    /// It fell short, a 2× model asked for 3×; EASU covers the rest at this
    /// factor, the same move hi-res makes past its budget.
    Easu(f32),
}

/// Decide [`AfterModel`] from what the model actually returned, never from a
/// scale read off a filename.
///
/// **An image that didn't get bigger is an error**, not a `Keep`. That is what
/// sd-cli hands back when it could not build an ESRGAN at all — it logs, saves
/// the input unchanged and exits 0 — and it is also what a 1× restoration
/// model does by design. Neither is an upscale, and passing either through
/// would be a picture that silently skipped the step it was asked for.
fn after_model(input: (u32, u32), got: (u32, u32), factor: f32) -> Result<AfterModel> {
    if got.0 <= input.0 || got.1 <= input.1 {
        bail!(
            "the model returned {}×{} for a {}×{} image — it enlarged nothing. \
             A 1× restoration model is not an upscaler, and neither is a file sd-cli \
             could not build as an ESRGAN",
            got.0,
            got.1,
            input.0,
            input.1
        );
    }
    let target = crate::fsr::out_size(input, factor);
    Ok(if got == target {
        AfterModel::Keep
    } else if got.0 >= target.0 && got.1 >= target.1 {
        AfterModel::Reduce(target.0, target.1)
    } else {
        // `factor / scale` rather than `target / got`: the latter would size the
        // height off the width's rounding. This keeps EASU computing the same
        // `round(side × factor)` per axis it would have computed from `input`.
        AfterModel::Easu(factor / (got.0 as f32 / input.0 as f32))
    })
}

/// Lanczos3 `src` down to exactly `w`×`h`, on the CPU.
///
/// Not EASU, which is an upsampler — handed a factor below 1 it point-samples
/// and aliases. Lanczos from 4× is supersampling in its own right: the model's
/// own grid noise averages out on the way down. CPU because it runs once per
/// generate, on an image the model took seconds to make; the `image` crate's
/// resize is in its core, so this needs no new feature. Not yet timed at
/// 4096² → 2048².
fn reduce(src: &Path, dst: &Path, w: u32, h: u32) -> Result<()> {
    let img = image::open(src).with_context(|| format!("loading {}", src.display()))?;
    img.resize_exact(w, h, image::imageops::FilterType::Lanczos3)
        .save_with_format(dst, image::ImageFormat::Png)
        .with_context(|| format!("writing {}", dst.display()))
}

/// Round down to a multiple of 64 (never below 64) — SDXL wants clean latent
/// dimensions, and FSR's float scaling would otherwise hand it odd sizes.
fn snap64(v: u32) -> u32 {
    (v / 64).max(1) * 64
}

/// A model name a request actually gave, or `None`.
///
/// An unchosen `<select>` posts the empty string, which means the same thing as
/// leaving the field out — so the two collapse here, once, rather than at each of
/// the four places that read one. Keeping them apart would give "" a meaning of
/// its own that nothing on the page can express.
fn named(v: &Option<String>) -> Option<String> {
    v.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
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

#[cfg(test)]
mod tests {
    use super::{after_model, fit_to, AfterModel, Pending, Retained, Slot};
    use crate::config::{Filters, Profile};
    use crate::fsr::out_size;
    use std::path::Path;

    /// A retained image whose two files exist, so eviction has something to
    /// delete and the tests something to look for.
    fn live(dir: &Path, name: &str) -> Pending {
        let raw = dir.join(format!("{name}-raw.png"));
        let display = dir.join(format!("{name}-full.png"));
        std::fs::write(&raw, b"r").unwrap();
        std::fs::write(&display, b"d").unwrap();
        Pending {
            raw,
            raw_w: 1,
            raw_h: 1,
            display,
            profile: Profile::default(),
            filters: Filters::NONE,
            prompt: String::new(),
            negative: None,
            typed_prompt: None,
            typed_negative: None,
            seed: 0,
            work_id: None,
            source: None,
            refine_passes: 0,
            hires_passes: 0,
            step: 0,
        }
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("rustimage-slot-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// One live image per slot: the next one in lets the last one go, files
    /// and all — and only in its own slot, so a batch image landing leaves the
    /// output open in the viewer alone.
    #[test]
    fn storing_into_a_slot_lets_go_of_what_was_there() {
        let dir = scratch("evict");
        let mut r = Retained::default();
        let a = live(&dir, "a");
        let out = live(&dir, "out");
        r.store(1, Slot::Image, a.clone(), false);
        r.store(2, Slot::Output, out.clone(), false);
        r.store(3, Slot::Image, live(&dir, "b"), false);
        assert!(r.get(1, "t").is_err());
        assert!(!a.raw.exists() && !a.display.exists());
        assert!(r.get(2, "t").is_ok() && out.raw.exists());
        assert!(r.get(3, "t").is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A run's image, or one a queued Refine is waiting for, loses its slot to
    /// a reopened work item but keeps its files until the job lets go.
    #[test]
    fn a_pinned_image_outlives_its_slot_until_unpinned() {
        let dir = scratch("pin");
        let mut r = Retained::default();
        let a = live(&dir, "a");
        r.store(1, Slot::Image, a.clone(), false);
        assert!(r.pin(1).is_some());
        r.store(2, Slot::Image, live(&dir, "b"), false);
        assert!(r.get(1, "t").is_ok() && a.raw.exists());
        r.unpin(1);
        assert!(r.get(1, "t").is_err() && !a.raw.exists());
        assert!(r.pin(1).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A slider drag that read the image before a refine replaced it must not
    /// write the old paths back over the new ones.
    #[test]
    fn an_adjust_never_writes_over_a_newer_pass() {
        let dir = scratch("update");
        let mut r = Retained::default();
        let before = live(&dir, "a");
        r.store(1, Slot::Image, before.clone(), false);
        r.store(1, Slot::Image, live(&dir, "a2"), false);
        assert!(!r.update(1, before));
        assert!(!r.update(9, live(&dir, "c")));
        let now = r.get(1, "t").unwrap();
        assert!(r.update(1, now));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Whichever upscaler runs, the image comes out the size EASU would have
    /// made — which is the size the page quoted before the click, and what
    /// `preflight` budgets for. Walked over every factor the box allows and
    /// both model scales sd.cpp can build, at sizes nowhere near round.
    #[test]
    fn an_esrgan_enlarge_lands_on_the_easu_size() {
        for input in [(1024u32, 1024u32), (768, 1344), (832, 1216), (1000, 1373), (97, 61)] {
            for s in [2u32, 4] {
                let got = (input.0 * s, input.1 * s);
                for tenths in 11..=40 {
                    let f = tenths as f32 / 10.0;
                    let want = out_size(input, f);
                    let landed = match after_model(input, got, f).unwrap() {
                        AfterModel::Keep => got,
                        AfterModel::Reduce(w, h) => (w, h),
                        AfterModel::Easu(rest) => out_size(got, rest),
                    };
                    assert_eq!(landed, want, "{input:?} at {f} through a {s}x model");
                }
            }
        }
        // And the common cases take the expected road.
        let i = (1024, 1024);
        assert_eq!(after_model(i, (4096, 4096), 4.0).unwrap(), AfterModel::Keep);
        assert_eq!(after_model(i, (4096, 4096), 2.0).unwrap(), AfterModel::Reduce(2048, 2048));
        assert!(matches!(after_model(i, (2048, 2048), 3.0).unwrap(), AfterModel::Easu(_)));
    }

    /// sd-cli saves the input unchanged and exits 0 when it can't build the
    /// model, so the size is the only thing that says the step didn't happen.
    /// A 1× restoration model looks identical, and is refused for the same
    /// reason: it is not an upscaler.
    #[test]
    fn an_upscale_that_enlarged_nothing_is_an_error() {
        assert!(after_model((1024, 1024), (1024, 1024), 2.0).is_err());
        assert!(after_model((1024, 768), (2048, 768), 2.0).is_err());
    }

    /// *Start from*'s one piece of arithmetic, and the only part of
    /// that pass decidable without a GPU or a checkpoint.
    ///
    /// It matters because the failure is quiet in both directions: fit a source
    /// too large and a phone photo takes the card out of memory minutes in, fit
    /// one that was already fine and every import is silently re-encoded and
    /// re-sampled for nothing.
    #[test]
    fn a_source_image_is_only_shrunk_and_only_when_it_has_to_be() {
        // Inside the budget: returned unchanged, which is what lets the caller
        // copy the bytes instead of resampling them. Includes sizes that are
        // nowhere near a multiple of 64 — being over the budget is the only
        // reason to touch an image, not being untidy.
        for src in [(768, 768), (2048, 1024), (1000, 1373), (2048, 2048), (1, 1)] {
            assert_eq!(fit_to(src, 2048), src, "{src:?} is already affordable");
        }

        // Over it: the long side lands on the budget, snapped down to a clean
        // latent width, and the short side follows the aspect.
        let (w, h) = fit_to((4032, 3024), 2048);
        assert_eq!((w, h), (2048, 1536));
        assert_eq!(w % 64, 0);
        assert!(w.max(h) <= 2048);

        // Portrait: the *long* side is the one that meets the budget, not the
        // width. Getting this backwards fits a tall image to 2048 wide and
        // diffuses 2730 px of height on a budget that said 2048.
        let (w, h) = fit_to((3024, 4032), 2048);
        assert!(w.max(h) <= 2048, "{w}x{h}");
        assert_eq!(h, 2048);

        // Aspect survives to within the snap, which is up to 64 px on the short
        // side of an extreme ratio and much less than that on anything real.
        for (sw, sh) in [(6000u32, 4000u32), (5000, 2000), (3000, 5200), (4096, 4096)] {
            let (w, h) = fit_to((sw, sh), 1536);
            assert!(w.max(h) <= 1536, "{sw}x{sh} -> {w}x{h}");
            assert_eq!(w % 64, 0, "{sw}x{sh} -> {w}x{h}");
            // Tight, because only one axis is snapped. This is the assertion
            // that fails if anyone ever "tidies up" the height as well.
            let want = (sw as f32) / (sh as f32);
            let got = (w as f32) / (h as f32);
            assert!(
                (want - got).abs() / want < 0.002,
                "{sw}x{sh} -> {w}x{h}: aspect moved from {want} to {got}"
            );
        }

        // Never enlarges. A budget is a ceiling, and a 512² thumbnail stretched
        // to 2048² before diffusing is a decision, not something to inherit
        // from a limit.
        assert_eq!(fit_to((512, 512), 3072), (512, 512));

        // Degenerate inputs come back untouched rather than panicking or
        // dividing by zero — `max_side` of 0 is what an empty box would send.
        assert_eq!(fit_to((0, 0), 2048), (0, 0));
        assert_eq!(fit_to((4032, 3024), 0), (4032, 3024));
    }
}
