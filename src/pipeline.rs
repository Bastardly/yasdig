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

use crate::area::Area;
use crate::config::{Config, Filters, FiltersPatch, LoraRef, Profile, MAX_STRENGTH, MIN_STRENGTH};
use crate::redraw;
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

    /// Write back a stepped-to entry (`Display::goto`), only if the image is
    /// still the one that was read — `was` is its `raw` then. `update` can't
    /// serve: it matches on the entry's own `raw`, and a step changes exactly
    /// that.
    fn replace(&mut self, id: u64, was: &Path, entry: Pending) -> bool {
        match self.live.get_mut(&id) {
            Some(l) if l.pending.raw == was => {
                l.pending = entry;
                true
            }
            _ => false,
        }
    }

    /// A history step's thumbnail: its PNG bytes, made on first request and
    /// kept beside the step's raw. `None` when there is no such image or step.
    /// Called on a connection thread — a decode and a resize, no GPU.
    pub fn history_thumb(this: &Mutex<Retained>, id: u64, index: usize) -> Option<Vec<u8>> {
        let raw = {
            let r = lock(this);
            r.live.get(&id)?.pending.steps().get(index)?.raw.clone()
        };
        let thumb = thumb_path(&raw);
        if !thumb.exists() {
            let img = image::open(&raw).ok()?;
            // Through a temp name and a rename: two requests for one tile are
            // two threads of this process, and a reader must never see half.
            static N: AtomicU64 = AtomicU64::new(0);
            let tmp = raw.with_extension(format!("thumb{}.part", N.fetch_add(1, Ordering::Relaxed)));
            let written = img
                .thumbnail(256, 256)
                .save_with_format(&tmp, image::ImageFormat::Png)
                .is_ok()
                && std::fs::rename(&tmp, &thumb).is_ok();
            if !written {
                // `.part`, so nothing else would ever sweep it.
                let _ = std::fs::remove_file(&tmp);
                return None;
            }
        }
        std::fs::read(thumb).ok()
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

/// A slider re-filter that lost to a pass finishing on the other lane: the
/// image it was adjusting has been replaced by a newer one under the same id.
/// Typed so the route answers 409 and the page can tell it from "that image is
/// gone" — the image is very much still there, it is just newer.
#[derive(Debug)]
pub struct Superseded;

impl std::fmt::Display for Superseded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("that image was replaced while it was being adjusted")
    }
}

impl std::error::Error for Superseded {}

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
    /// Where the weights live ([`crate::config::MEMORY_MODES`]); `None` keeps
    /// the profile's.
    pub memory: Option<String>,
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
    /// Reference images, as output stems, for a family that takes them. The
    /// same whole-list rule as `loras`: `Some([])` is "none", `None` keeps the
    /// profile's. See [`Profile::references`].
    pub references: Option<Vec<String>>,
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
    /// Where the weights live ([`crate::config::MEMORY_MODES`]); `None` keeps
    /// the profile's.
    pub memory: Option<String>,
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
    /// Where the weights live; `None` keeps the model's.
    pub memory: Option<String>,
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
    /// What this pass is asked for. `None` or blank is the image's own prompt.
    /// Not on `Profile` and not applied by `apply`: it belongs to the pass,
    /// never to the image — `Pending.prompt` keeps describing the picture, and
    /// the step's history label says what the pass was asked (`pass_prompt`).
    pub refine_prompt: Option<String>,
    pub hires_prompt: Option<String>,
    /// Where the weights live for this pass. On an escalation because it
    /// describes how the pass about to run fits, not the image in hand: the
    /// retry after an out-of-memory is exactly this field changing. Applied
    /// after [`Engine::swap_model`], so it survives a swap to another model.
    pub memory: Option<String>,
}

/// What an Edit asks for: an instruction, and any further reference images.
#[derive(Debug, Clone, Default)]
pub struct EditRequest {
    /// What to change — "make it night", "remove the hat". Required: an edit
    /// with no instruction is a re-roll of the same picture.
    pub prompt: Option<String>,
    /// Further references (output stems) after the image itself, which is
    /// always reference 1 — so the prompt names these "image 2" onwards.
    pub references: Vec<String>,
    /// Where the weights live for this pass; `None` keeps the image's.
    pub memory: Option<String>,
    /// Grow the canvas too — "Edit and rescale". The model redraws the whole
    /// picture in the new shape; nothing of the original is pasted back.
    pub expand: Expand,
}

/// What a Redraw asks for: which pixels, and optionally what to put there.
#[derive(Debug, Clone)]
pub struct RedrawRequest {
    /// A PNG at the image's own size; white is "redraw this".
    pub mask_png: Vec<u8>,
    /// What should be there. `None` or blank uses the image's own prompt, which
    /// is the right default for "fix the hand".
    pub prompt: Option<String>,
    /// `None` is [`REDRAW_STRENGTH`].
    pub strength: Option<f32>,
    /// Where the weights live for this pass; `None` keeps the image's.
    pub memory: Option<String>,
    /// Edge softness in image pixels (`redraw::feather`); `None` is
    /// [`REDRAW_SOFTNESS`].
    pub feather: Option<u32>,
}

/// Fixes what was under the mask while keeping its shape and colours — the
/// common case, a hand or a face that is nearly right. 0.8 replaces it, 0.95
/// starts over; those are a choice, not a default.
pub const REDRAW_STRENGTH: f32 = 0.5;

/// Edge softness when a request names none: enough that a strong redraw
/// doesn't leave a seam at 1024², little enough to stay where it was painted.
pub const REDRAW_SOFTNESS: u32 = 16;

/// How far to grow each side of a canvas, in pixels — Extend's, and Edit's
/// when it rescales. One type, so the two can't disagree about what a side or
/// its limit is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(default)]
pub struct Expand {
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
}

impl Expand {
    pub fn amount(&self, side: redraw::Side) -> u32 {
        match side {
            redraw::Side::Left => self.left,
            redraw::Side::Top => self.top,
            redraw::Side::Right => self.right,
            redraw::Side::Bottom => self.bottom,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.left + self.top + self.right + self.bottom == 0
    }

    /// The canvas size after growing a `w`×`h` one.
    pub fn grown(&self, w: u32, h: u32) -> (u32, u32) {
        (w + self.left + self.right, h + self.top + self.bottom)
    }

    /// The sides that grow, in the order they are drawn, each at most
    /// [`redraw::MAX_EXTEND`].
    pub fn sides(&self) -> Result<Vec<(redraw::Side, u32)>> {
        let sides: Vec<_> =
            redraw::Side::ORDER.iter().map(|&s| (s, self.amount(s))).filter(|&(_, px)| px > 0).collect();
        if let Some(&(s, px)) = sides.iter().find(|&&(_, px)| px > redraw::MAX_EXTEND) {
            bail!("{} {px} px is more than the {} px a side may grow by", s.arrow(), redraw::MAX_EXTEND);
        }
        Ok(sides)
    }

    /// "← → 256 px" when the sides grow alike, else "←128 →256".
    pub fn describe(&self) -> String {
        let sides: Vec<_> =
            redraw::Side::ORDER.iter().map(|&s| (s, self.amount(s))).filter(|&(_, px)| px > 0).collect();
        let Some(&(_, first)) = sides.first() else { return String::new() };
        if sides.iter().all(|&(_, px)| px == first) {
            let arrows: Vec<&str> = sides.iter().map(|&(s, _)| s.arrow()).collect();
            format!("{} {first} px", arrows.join(" "))
        } else {
            let each: Vec<String> = sides.iter().map(|&(s, px)| format!("{}{px}", s.arrow())).collect();
            each.join(" ")
        }
    }
}

/// What an Extend asks for: how far to grow each side, and what to draw there.
#[derive(Debug, Clone, Default)]
pub struct ExtendRequest {
    pub expand: Expand,
    /// `None` or blank is [`EXTEND_BACKGROUND_PROMPT`], never the image's own.
    pub prompt: Option<String>,
    /// Edge softness where a strip meets the original; `None` is
    /// [`REDRAW_SOFTNESS`], floored at [`EXTEND_MIN_SOFTNESS`].
    pub feather: Option<u32>,
    /// Where the Fill model's weights live for these passes; `None` keeps its own.
    pub memory: Option<String>,
}

/// What an Extend draws when its prompt box is blank: more of the scene around
/// the picture, in its manner, with no subject named. The image's own prompt
/// names one, and `outpaintingV1` found an ordinary model would draw it again in
/// the strip; FLUX.1 Fill is far less prone to that, but a prompt that asks for
/// no subject can't ask for a second one. A typed prompt is the user's.
pub const EXTEND_BACKGROUND_PROMPT: &str =
    "background scenery continuing the surrounding scene, same art style, same lighting and colours";

/// The least edge softness an Extend's join gets, whatever the slider says.
/// Small on purpose: the model redraws `3·soft + 8` px of the *original* past
/// the strip (`redraw::diffusion_mask`), and a share of a wide strip redrew
/// hundreds of pixels of picture in v1 — a ghost of the subject at the join.
pub const EXTEND_MIN_SOFTNESS: u32 = 24;

/// A pass's memory choice onto the profile it runs with, checked. Blank keeps it.
fn set_memory(profile: &mut Profile, memory: Option<&str>) -> Result<()> {
    if let Some(v) = memory.filter(|v| !v.is_empty()) {
        if !crate::config::MEMORY_MODES.contains(&v) {
            bail!("memory '{v}' is not one of {}", crate::config::MEMORY_MODES.join(", "));
        }
        profile.memory = v.to_string();
    }
    Ok(())
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
        if let Some(v) = self.memory.as_deref().filter(|v| !v.is_empty()) {
            profile.memory = v.to_string();
        }
        profile.validate()
    }
}

#[derive(Debug, Clone, Default)]
pub struct Timings {
    pub gen_ms: u128,
    /// An ESRGAN model's share of the initial enlarge, reduce included. 0 when
    /// EASU did it, which is also what keeps the stage off the readout.
    pub upscale_ms: u128,
    pub fsr_ms: u128,
    pub refine_ms: u128,
    pub hires_ms: u128,
    pub redraw_ms: u128,
    /// Steps the generate pass's step cache skipped, of `cache_steps`. Both 0
    /// when it skipped none, which is also what an escalation reports — those
    /// run uncached ([`Profile::uncached`]).
    pub cache_skipped: u32,
    pub cache_steps: u32,
    /// Where the pass's weights were when any were off the card ("text
    /// encoder in system RAM"), read straight after its sd-cli run like the
    /// cache numbers. It explains a slow pass before it becomes a failed one.
    pub weights: Option<String>,
    /// Of the pass's time, how much went to loading weights (sd.cpp's own
    /// "loading tensors completed" lines, summed) — the share a model kept
    /// loaded between runs would save.
    pub load_ms: u64,
    /// Where the pass's time went, stage by stage (`sd::Stages`).
    pub stages: crate::sd::Stages,
}

pub struct GenResult {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub seed: i64,
    pub timings: Timings,
    pub refine_passes: u32,
    pub hires_passes: u32,
    /// How many passes this image is past its generate — every escalation,
    /// a redraw included. The page's "already on screen" test needs it: a
    /// redraw changes no pass count and no size, so without it a redrawn
    /// image would compare equal to the one it replaced and never be drawn.
    pub step: u32,
    /// What each step of this image's history was ("Generated", "Refine", …),
    /// oldest first, and which one is on screen. See `Display::goto`.
    pub history: Vec<String>,
    pub at: usize,
    /// Handle for [`Engine::refine`] / [`Engine::hires`] / [`Display::adjust`].
    pub image_id: u64,
    /// The archived generation this descends from, so the UI can mark which
    /// thumbnail is on screen. Unrelated to `image_id`: that one addresses the
    /// in-memory cache and restarts at 0, this one outlives the process.
    pub work_id: Option<String>,
    /// Whether this image is still inside the hi-res budget — the size half of
    /// [`Pending::can_hires`] only. Neither pass's step count is in here: a
    /// press sends the fold's boxes, which override the retained profile before
    /// the server checks, so only the page knows whether the press will have
    /// steps. A fresh install's profile says 0 for both, and gating the buttons
    /// on that hid passes the fold was set to run.
    pub can_hires: bool,
    /// The model this step's passes run on — the registry name. The Refine
    /// dialog's model picker shows it for a picture opened from Outputs.
    pub model: String,
    /// Whether the Refine dialog's Edit applies: the model that made this step
    /// takes reference images, so the image itself can be one.
    pub can_edit: bool,
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
    /// The area with a look of its own, if the image has one, so a reopened
    /// viewer shows it again.
    pub area: Option<Area>,
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
    /// the pass did underneath a filter chosen for the previous image. The nine
    /// grading knobs have no such tie to the pixels, and re-dialling a look on
    /// every rung of the ladder is a reason not to climb it.
    filters: Filters,
    /// An area with a look of its own, over `filters` (`crate::area`). The
    /// look, like `filters`: carried across a pass with its detail reset
    /// (`Area::restart`), mapped by a crop, never in a `Snapshot`.
    area: Option<Area>,
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
    /// The saved image the Outputs viewer opened this from, so *Save changes*
    /// can write back over exactly that file (`Display::overwrite_output`).
    /// Set only by `open_output` into `Slot::Output`; every pass starts a new
    /// picture and leaves it `None`. A stem, never a path, for `source`'s reason.
    opened_from: Option<String>,
    refine_passes: u32,
    hires_passes: u32,
    /// How many escalations this result has been through. Only used to keep
    /// each step's filenames distinct from the ones it supersedes — otherwise a
    /// pass would read and write the same path.
    step: u32,
    /// Every state this image has been in, oldest first, so a bad pass can be
    /// stepped back from (`Display::goto`). Empty means "just the current
    /// state", which is what every fresh `Pending` starts as. Scratch, like
    /// everything a `Pending` owns: never in a sidecar.
    history: Vec<Snapshot>,
    /// Which of `history` the live fields are.
    at: usize,
}

/// One state in a [`Pending`]'s history: enough to make it the live image
/// again. Not the filters — those are the user's look and stay where they are
/// when stepping (invariant 1's carry, the same as `Filters::restart`'s).
#[derive(Debug, Clone)]
struct Snapshot {
    raw: PathBuf,
    /// A Hi-res on another checkpoint swaps the profile, and stepping back
    /// past it has to swap it back: `can_refine`, `can_hires` and where the
    /// sliders reset all describe the model that made *this* state.
    profile: Profile,
    refine_passes: u32,
    hires_passes: u32,
    step: u32,
    /// What made it: "Generated", "Refine", "Hi-res 2×", "Redraw: a red scarf".
    label: String,
}

/// How many states an image keeps. Each is one full-size PNG in scratch, so
/// it is a disk cap rather than a memory one; past it the oldest *after the
/// original* goes, since the original is the one people go back to.
const MAX_HISTORY: usize = 12;

impl Snapshot {
    fn of(p: &Pending, label: String) -> Snapshot {
        Snapshot {
            raw: p.raw.clone(),
            profile: p.profile.clone(),
            refine_passes: p.refine_passes,
            hires_passes: p.hires_passes,
            step: p.step,
            label,
        }
    }

    fn remove_files(&self) {
        let _ = std::fs::remove_file(&self.raw);
        let _ = std::fs::remove_file(thumb_path(&self.raw));
    }
}

/// Where a history step's thumbnail lives: beside its raw, so whatever deletes
/// the one deletes the other. Made on first request (`history_thumb`).
fn thumb_path(raw: &Path) -> PathBuf {
    raw.with_extension("thumb.png")
}

/// The prompt a finishing pass runs with: its own, when one was typed, with
/// any `{variable}` rolled off the image's seed — or the image's own prompt,
/// already rolled when it was generated. Never written back: the image keeps
/// describing itself, and only the step's label says what this pass was asked.
fn pass_prompt(own: Option<&str>, image: &str, vars: &variables::Variables, seed: i64) -> Result<String> {
    match own.map(str::trim) {
        Some(text) if !text.is_empty() => variables::expand(text, vars, seed),
        _ => Ok(image.to_string()),
    }
}

/// A history step's label: what made it, and what it was asked for when that
/// differs from the image's own prompt — cut short, since it is a list row.
fn step_label(what: &str, own: Option<&str>) -> String {
    match own.map(str::trim) {
        Some(text) if !text.is_empty() => {
            let short: String = text.chars().take(40).collect();
            let more = if text.chars().count() > 40 { "…" } else { "" };
            format!("{what}: {short}{more}")
        }
        _ => what.to_string(),
    }
}

/// `next` with the history `prev` leaves it: every step after the one `prev`
/// was on is deleted — a new pass from an earlier step is a new branch, as in
/// any undo stack — and `next` goes on top. The image `prev` showed stays, as
/// a step; only its `display`, which `publish` can make again, is deleted.
fn advance(prev: &Pending, mut next: Pending, label: String) -> Pending {
    let mut h = prev.steps();
    let at = prev.at.min(h.len() - 1);
    for s in h.drain(at + 1..) {
        s.remove_files();
    }
    if prev.display != next.display {
        let _ = std::fs::remove_file(&prev.display);
    }
    h.push(Snapshot::of(&next, label));
    while h.len() > MAX_HISTORY {
        h.remove(1).remove_files();
    }
    next.at = h.len() - 1;
    next.history = h;
    next
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
        self.profile.hires_steps > 0 && self.hires_fits()
    }

    /// The size half of [`Pending::can_hires`], which is all the page is told:
    /// it reads the steps off its own boxes (see [`GenResult::can_hires`]).
    fn hires_fits(&self) -> bool {
        self.raw_w.max(self.raw_h) <= self.profile.hires_max_side
    }

    /// Whether an img2img pass at this size needs the large-image memory flags.
    fn big_image(&self) -> bool {
        self.raw_w.max(self.raw_h) > BIG_IMAGE_SIDE
    }

    /// The history, with the current state in it — which a fresh `Pending`'s
    /// empty one leaves implicit.
    fn steps(&self) -> Vec<Snapshot> {
        if self.history.is_empty() {
            vec![Snapshot::of(self, "Generated".into())]
        } else {
            self.history.clone()
        }
    }

    /// Step `index` of the history, or `None` past its end. The one place
    /// `goto` and `view_step` read a step from, so the two can't disagree
    /// about what index 2 is.
    fn step(&self, index: usize) -> Option<Snapshot> {
        self.steps().into_iter().nth(index)
    }

    /// Everything this image owns: the display, and every step's raw and
    /// thumbnail — the current one among them.
    fn remove_files(&self) {
        let _ = std::fs::remove_file(&self.display);
        for s in self.steps() {
            s.remove_files();
        }
        let _ = std::fs::remove_file(&self.raw);
        let _ = std::fs::remove_file(thumb_path(&self.raw));
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
            cfg: self.cfg.clone(),
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
        if plan.native {
            // A VAE to f16, which sd.cpp's converter can't do (see `half`). No
            // scales to fold: nothing in it is fp8.
            let r = crate::half::safetensors_to_f16(&plan.source, &plan.out)?;
            eprintln!(
                "[yasdig] convert: {} of {} tensors to f16, the rest copied as they were",
                r.halved, r.tensors
            );
            return Ok(ConvertResult {
                out: plan.out.clone(),
                source_bytes: plan.source_bytes,
                estimated_bytes: plan.estimated_bytes,
                bytes: std::fs::metadata(&plan.out)?.len(),
                ty: plan.ty.clone(),
                ms: t.elapsed().as_millis(),
                folded: 0,
            });
        }
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
            "[yasdig] escalating with '{}' instead of '{}'",
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
            // The one door generate, Hi-res and Refine resolve a model through,
            // so a fill-only model is refused here once: from noise, or over a
            // whole picture, it has nothing to fill. Extend finds it itself
            // (`Config::fill_model`).
            Some(p) if p.family.fill_only() => bail!(
                "'{name}' is a {} model — it extends pictures (Refine… → Extend) rather than \
                 generating them",
                p.family.label()
            ),
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
        if let Some(v) = &req.memory {
            if !v.is_empty() {
                profile.memory = v.clone();
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
        if let Some(v) = &req.references {
            profile.references = v.clone();
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

        // Before the render rather than at the enlarge: a 4096 base at 4× is
        // past any limit, and two minutes of txt2img shouldn't find that out.
        let (ow, oh) = crate::fsr::out_size((tpl.width, tpl.height), tpl.upscale_factor);
        self.check_side(ow, oh)?;

        // The references, copied into scratch — never adopted, invariant 8 —
        // and shrunk to the model's own size when bigger: each costs attention
        // tokens like the image does, so a phone photo would be several images'
        // worth of memory and time. Resolved before the render, so a deleted
        // output fails now rather than after it.
        let mut refs = Vec::new();
        for (i, stem) in tpl.references.iter().enumerate() {
            let src = self.output_source(stem)?;
            let dst = scratch.path(self.scratch.join(format!("{id}-0-ref{}.png", i + 1)));
            stage_reference(&src, &dst, tpl.width.max(tpl.height))?;
            refs.push(dst);
        }

        // 1) Distilled generation at base resolution.
        let t = Instant::now();
        let ref_side = Some(self.cfg.settings().reference_size);
        sd.txt2img(tpl, &prompt, neg, seed, &lowres, &refs, ref_side)?;
        let gen_ms = t.elapsed().as_millis();
        // Read now: an ESRGAN enlarge below is another sd-cli run, which resets it.
        let (cache_skipped, cache_steps) = self.monitor.cache_skip().unwrap_or_default();
        let load_ms = self.monitor.load_ms();
        let stages = self.monitor.stages();

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
        let (png, w, h) = publish(&self.fsr, &raw, &display, &filters, None)?;
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
                eprintln!("[yasdig] could not archive work item: {e:#}");
                None
            }
        };

        // Retain the intermediates so the next step up the ladder doesn't have
        // to re-run this one.
        let entry = Pending {
            history: Vec::new(),
            at: 0,
            raw,
            raw_w: w,
            raw_h: h,
            display,
            filters,
            area: None,
            profile: profile.clone(),
            prompt: prompt.clone(),
            negative: negative.clone(),
            typed_prompt,
            typed_negative,
            seed,
            work_id,
            source: None,
            opened_from: None,
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
            weights: self.monitor.placement(),
            load_ms,
            stages,
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
        if let Some(v) = &req.memory {
            if !v.is_empty() {
                profile.memory = v.clone();
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
        let load_ms = self.monitor.load_ms();
        let stages = self.monitor.stages();

        // 3) Sharpen for display. Dimensions come from here, not from `fit_to`:
        //    sd.cpp rounds an init image to the latent grid and only warns.
        let t = Instant::now();
        let filters = Filters::starting_at(profile.sharpness, profile.clarity);
        let (png, w, h) = publish(&self.fsr, &raw, &display, &filters, None)?;
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
                eprintln!("[yasdig] could not archive work item: {e:#}");
                None
            }
        };

        let entry = Pending {
            history: Vec::new(),
            at: 0,
            raw,
            raw_w: w,
            raw_h: h,
            display,
            filters,
            area: None,
            profile,
            prompt: prompt.clone(),
            negative: negative.clone(),
            typed_prompt,
            typed_negative,
            seed,
            work_id,
            source: Some(req.source.clone()),
            opened_from: None,
            refine_passes: 0,
            hires_passes: 0,
            step: 0,
        };
        let result = entry.result(id, png, Timings {
            gen_ms,
            fsr_ms,
            cache_skipped,
            cache_steps,
            weights: self.monitor.placement(),
            load_ms,
            stages,
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
        if let Some(v) = req.memory.as_deref().filter(|v| !v.is_empty()) {
            m.memory = Some(v.to_string());
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
        // Unique per pass, not just per step: after stepping back to step 1,
        // the next pass is step 2 again, and `{id}-{step}` would overwrite the
        // old step 2's file and then delete it as a discarded branch.
        let n = JOB_COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut scratch = Scratch::default();
        let raw = scratch.path(self.scratch.join(format!("{id}-{step}-{n}-raw.png")));
        let display = scratch.path(self.scratch.join(format!("{id}-{step}-{n}-full.png")));
        let sd = self.sd();

        let vars = self.cfg.settings().variables;
        let prompt = pass_prompt(over.refine_prompt.as_deref(), &p.prompt, &vars, p.seed)?;

        // Uncached: a finishing pass is the quality rung (`Profile::uncached`).
        let t = Instant::now();
        sd.img2img(
            &p.profile.uncached(),
            &prompt,
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
        let area = p.area.as_ref().map(Area::restart);
        let (png, w, h) = publish(&self.fsr, &raw, &display, &filters, area.as_ref())?;
        let fsr_ms = t.elapsed().as_millis();

        scratch.keep(&raw);
        scratch.keep(&display);
        // Only now that it succeeded — a failure above leaves the entry intact
        // so the user can retry, and takes this pass's half-written files with
        // it rather than stranding them beside the ones still in use.
        let prev = p.clone();
        let entry = Pending {
            raw,
            raw_w: w,
            raw_h: h,
            display,
            // Detail back to the model's starting point, the grade kept — see
            // `Filters::restart`.
            filters,
            area,
            refine_passes: p.refine_passes + 1,
            step,
            ..p
        };
        let entry = advance(&prev, entry, step_label("Refine", over.refine_prompt.as_deref()));
        let result = entry.result(id, png, Timings { fsr_ms, refine_ms, weights: self.monitor.placement(), load_ms: self.monitor.load_ms(), stages: self.monitor.stages(), ..Default::default() });
        self.keep(id, entry);
        Ok(result)
    }

    /// Redraw only the painted part of the current image — see
    /// [`crate::redraw`] for why it is a crop, enlarged, diffused, shrunk and
    /// composited rather than a masked pass over the whole frame.
    ///
    /// An escalation like `refine`: it reads `raw`, never `display`
    /// (invariant 1), runs uncached, and its result becomes the image, so
    /// Refine, Hi-res, Save and another Redraw follow it with no special case.
    /// The prompt is the pass's own and is not written back: `Pending.prompt`
    /// keeps describing the picture.
    pub fn redraw(&mut self, id: u64, req: &RedrawRequest) -> Result<GenResult> {
        let mut p = lock(&self.retained).get(id, "redraw")?;
        set_memory(&mut p.profile, req.memory.as_deref())?;
        self.cfg.check_loras(&p.profile.loras, p.profile.family)?;
        let strength = req.strength.unwrap_or(REDRAW_STRENGTH);
        if !(MIN_STRENGTH..=MAX_STRENGTH).contains(&strength) {
            bail!("redraw strength must be between {MIN_STRENGTH} and {MAX_STRENGTH}");
        }

        let mut base = image::open(&p.raw)
            .with_context(|| format!("reading {}", p.raw.display()))?
            .to_rgb8();
        let mask = image::load_from_memory(&req.mask_png)
            .context("the mask is not a PNG")?
            .to_luma8();
        // Refused rather than resized: a mismatch means the page painted on a
        // different image than the one this id now names.
        if mask.dimensions() != base.dimensions() {
            bail!(
                "the mask is {}×{} but the image is {}×{} — reopen the image and paint again",
                mask.width(),
                mask.height(),
                base.width(),
                base.height()
            );
        }
        let painted = redraw::mask_bbox(&mask).context("paint the area to redraw first")?;
        let soft = req.feather.unwrap_or(REDRAW_SOFTNESS);
        if soft > redraw::MAX_SOFTNESS {
            bail!("edge softness must be at most {} pixels", redraw::MAX_SOFTNESS);
        }
        let crop = redraw::context_rect(painted, base.width(), base.height(), soft);
        let target = p.profile.width.max(p.profile.height);
        let (ww, wh) = redraw::work_size(crop.w, crop.h, target);

        let step = p.step + 1;
        // Unique per pass, not just per step: after stepping back to step 1,
        // the next pass is step 2 again, and `{id}-{step}` would overwrite the
        // old step 2's file and then delete it as a discarded branch.
        let n = JOB_COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut scratch = Scratch::default();
        // Three working files, never kept: `Scratch` sweeps them on success
        // and on failure alike.
        let init = scratch.path(self.scratch.join(format!("{id}-{step}-{n}-rp-init.png")));
        let sd_mask = scratch.path(self.scratch.join(format!("{id}-{step}-{n}-rp-mask.png")));
        let out = scratch.path(self.scratch.join(format!("{id}-{step}-{n}-rp-out.png")));
        let raw = scratch.path(self.scratch.join(format!("{id}-{step}-{n}-raw.png")));
        let display = scratch.path(self.scratch.join(format!("{id}-{step}-{n}-full.png")));

        use image::imageops::{crop_imm, resize, FilterType};
        let crop_mask = crop_imm(&mask, crop.x, crop.y, crop.w, crop.h).to_image();
        resize(&crop_imm(&base, crop.x, crop.y, crop.w, crop.h).to_image(), ww, wh, FilterType::Lanczos3)
            .save(&init)
            .with_context(|| format!("writing {}", init.display()))?;
        // Grown past the whole fade at the crop's own scale, then taken to the
        // work size: the blend below must only ever mix in pixels the model
        // really drew (`redraw::diffusion_mask`). Hard-edged, since sd.cpp
        // binarizes it anyway and the soft edge is the composite's job.
        let grown = redraw::diffusion_mask(&crop_mask, soft);
        redraw::binarize(&resize(&grown, ww, wh, FilterType::Triangle))
            .save(&sd_mask)
            .with_context(|| format!("writing {}", sd_mask.display()))?;

        // A step's worth further along the seed, so pressing Redraw again on
        // the same mask draws a new variant rather than the same one. The roll
        // of any `{variable}` follows it, as everywhere else.
        let seed = p.seed.wrapping_add(step as i64);
        let vars = self.cfg.settings().variables;
        let prompt = pass_prompt(req.prompt.as_deref(), &p.prompt, &vars, seed)?;

        let t = Instant::now();
        self.sd().redraw(
            &p.profile.uncached(),
            &prompt,
            p.negative.as_deref(),
            seed,
            &init,
            &out,
            strength,
            p.profile.steps,
            p.profile.cfg_scale,
            ww.max(wh) > BIG_IMAGE_SIDE,
            "img2img redraw",
            &sd_mask,
        )?;
        let redraw_ms = t.elapsed().as_millis();

        let t = Instant::now();
        let patch = image::open(&out)
            .with_context(|| format!("reading {}", out.display()))?
            .to_rgb8();
        let patch = resize(&patch, crop.w, crop.h, FilterType::Lanczos3);
        let alpha = redraw::feather(&crop_mask, soft);
        redraw::composite(&mut base, &patch, &alpha, (crop.x, crop.y));
        base.save(&raw).with_context(|| format!("writing {}", raw.display()))?;
        let filters = p.filters.restart(p.profile.sharpness, p.profile.clarity);
        let area = p.area.as_ref().map(Area::restart);
        let (png, w, h) = publish(&self.fsr, &raw, &display, &filters, area.as_ref())?;
        let fsr_ms = t.elapsed().as_millis();

        scratch.keep(&raw);
        scratch.keep(&display);
        let prev = p.clone();
        let entry = Pending {
            raw,
            raw_w: w,
            raw_h: h,
            display,
            filters,
            area,
            step,
            ..p
        };
        // "Area" is what the page calls a redraw: the dialog's Refine with
        // something painted.
        let entry = advance(&prev, entry, step_label("Area", req.prompt.as_deref()));
        let result = entry.result(id, png, Timings { fsr_ms, redraw_ms, weights: self.monitor.placement(), load_ms: self.monitor.load_ms(), stages: self.monitor.stages(), ..Default::default() });
        self.keep(id, entry);
        Ok(result)
    }

    /// Change the whole picture by instruction ("make it night", "remove the
    /// hat"), with no painting: a txt2img on the image's own model with **the
    /// image itself as reference 1**, any further references after it, and the
    /// instruction as the prompt. A model that takes references reads that as
    /// "this picture, changed so" — which is what an edit checkpoint is, and
    /// what Flux.2 Klein does natively.
    ///
    /// Run at the image's own shape, its long side capped at `hires_max_side`
    /// (the size a pass may diffuse at), both axes snapped to the latent grid
    /// and the result resized back to the image's exact size. The whole
    /// picture is the result — nothing is composited — so it is a history step
    /// like any pass, and Compare shows what changed.
    /// Grow the canvas on the asked-for sides and draw beyond them with FLUX.1
    /// Fill (`Config::fill_model`) — on any image, whatever model made it, since
    /// Fill only needs the picture and a mask. One side at a time (left and
    /// right, then top and bottom, `redraw::Side::ORDER`): a later strip spans
    /// the earlier ones' new edges, which is what draws the corners, and each
    /// strip gets the model's full size. Every original pixel the fade never
    /// reaches stays the original's bytes (`redraw::composite`), and the image
    /// keeps its own profile, so a Refine afterwards runs on the model that made
    /// it. A history step like every pass.
    pub fn extend(&mut self, id: u64, req: &ExtendRequest) -> Result<GenResult> {
        let mut fill = self.cfg.fill_model().context(
            "Extend needs a FLUX.1 Fill model — download one on the Get models tab",
        )?;
        set_memory(&mut fill, req.memory.as_deref())?;
        fill.validate()?;
        let p = lock(&self.retained).get(id, "extend")?;
        let soft = req.feather.unwrap_or(REDRAW_SOFTNESS);
        if soft > redraw::MAX_SOFTNESS {
            bail!("edge softness must be at most {} pixels", redraw::MAX_SOFTNESS);
        }
        let sides = req.expand.sides()?;
        if sides.is_empty() {
            bail!("pick at least one side to extend");
        }
        let (grown_w, grown_h) = req.expand.grown(p.raw_w, p.raw_h);
        self.check_side(grown_w, grown_h)?;

        let mut base = image::open(&p.raw)
            .with_context(|| format!("reading {}", p.raw.display()))?
            .to_rgb8();
        let step = p.step + 1;
        let n = JOB_COUNTER.fetch_add(1, Ordering::Relaxed);
        let seed = p.seed.wrapping_add(step as i64);
        let vars = self.cfg.settings().variables;
        let prompt = pass_prompt(req.prompt.as_deref(), EXTEND_BACKGROUND_PROMPT, &vars, seed)?;
        let soft = soft.max(EXTEND_MIN_SOFTNESS);
        // The Fill model's own size — what it was trained at, and what each
        // strip's crop is taken to.
        let target = fill.width.max(fill.height);

        use image::imageops::{crop_imm, resize, FilterType};
        let mut scratch = Scratch::default();
        let t = Instant::now();
        for (k, &(side, px)) in sides.iter().enumerate() {
            let (grown, mask) = redraw::grow(&base, side, px);
            base = grown;
            let crop = redraw::extend_crop(base.dimensions(), side, px, target);
            let (ww, wh) = redraw::work_size(crop.w, crop.h, target);
            let init = scratch.path(self.scratch.join(format!("{id}-{step}-{n}-{k}-ext-init.png")));
            let sd_mask = scratch.path(self.scratch.join(format!("{id}-{step}-{n}-{k}-ext-mask.png")));
            let out = scratch.path(self.scratch.join(format!("{id}-{step}-{n}-{k}-ext-out.png")));
            let crop_mask = crop_imm(&mask, crop.x, crop.y, crop.w, crop.h).to_image();
            resize(&crop_imm(&base, crop.x, crop.y, crop.w, crop.h).to_image(), ww, wh, FilterType::Lanczos3)
                .save(&init)
                .with_context(|| format!("writing {}", init.display()))?;
            // Past the whole fade, as the area pass does, so the blend only ever
            // mixes the original with pixels the model really drew.
            let grown_mask = redraw::diffusion_mask(&crop_mask, soft);
            redraw::binarize(&resize(&grown_mask, ww, wh, FilterType::Triangle))
                .save(&sd_mask)
                .with_context(|| format!("writing {}", sd_mask.display()))?;
            // Each strip its own noise, still reproducible from the step. No
            // negative: Fill runs at CFG 1, where sd.cpp never evaluates one.
            self.sd().fill(&fill, &prompt, None, seed.wrapping_add(k as i64), &init, &sd_mask, &out)?;
            let patch = image::open(&out)
                .with_context(|| format!("reading {}", out.display()))?
                .to_rgb8();
            let patch = resize(&patch, crop.w, crop.h, FilterType::Lanczos3);
            let alpha = redraw::feather(&crop_mask, soft);
            redraw::composite(&mut base, &patch, &alpha, (crop.x, crop.y));
        }
        let redraw_ms = t.elapsed().as_millis();

        let t = Instant::now();
        let raw = scratch.path(self.scratch.join(format!("{id}-{step}-{n}-raw.png")));
        let display = scratch.path(self.scratch.join(format!("{id}-{step}-{n}-full.png")));
        base.save(&raw).with_context(|| format!("writing {}", raw.display()))?;
        // A new size, so the detail filters restart for it; the grade carries.
        let filters = p.filters.restart(p.profile.sharpness, p.profile.clarity);
        let area = p.area.as_ref().map(Area::restart);
        let (png, w, h) = publish(&self.fsr, &raw, &display, &filters, area.as_ref())?;
        let fsr_ms = t.elapsed().as_millis();
        scratch.keep(&raw);
        scratch.keep(&display);

        let what = format!("Extend {}", req.expand.describe());
        let prev = p.clone();
        let entry = Pending { raw, raw_w: w, raw_h: h, display, filters, area, step, ..p };
        let entry = advance(&prev, entry, step_label(&what, req.prompt.as_deref()));
        let result = entry.result(id, png, Timings {
            fsr_ms,
            redraw_ms,
            weights: self.monitor.placement(),
            load_ms: self.monitor.load_ms(),
            stages: self.monitor.stages(),
            ..Default::default()
        });
        self.keep(id, entry);
        Ok(result)
    }

    pub fn edit(&mut self, id: u64, req: &EditRequest) -> Result<GenResult> {
        let mut p = lock(&self.retained).get(id, "edit")?;
        set_memory(&mut p.profile, req.memory.as_deref())?;
        if !p.profile.family.takes_references() {
            bail!(
                "Edit needs a model that takes reference images (Flux.2 Klein); this \
                 image was made with a {} model",
                p.profile.family.label()
            );
        }
        let instruction = req
            .prompt
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .context("type what to change — \"make it night\", \"remove the hat\"")?;
        if req.references.len() + 1 > crate::config::MAX_REFERENCES {
            bail!(
                "an edit takes at most {} more references — the image itself is the first",
                crate::config::MAX_REFERENCES - 1
            );
        }
        if let Some(bad) = req.references.iter().find(|r| !workitems::valid_id(r)) {
            bail!("'{bad}' is not an output image");
        }
        self.cfg.check_loras(&p.profile.loras, p.profile.family)?;
        req.expand.sides()?;
        // The shape the result comes back at: the image's own, or grown.
        let (tw, th) = req.expand.grown(p.raw_w, p.raw_h);
        self.check_side(tw, th)?;

        let step = p.step + 1;
        let n = JOB_COUNTER.fetch_add(1, Ordering::Relaxed);
        let seed = p.seed.wrapping_add(step as i64);
        let vars = self.cfg.settings().variables;
        let prompt = variables::expand(instruction, &vars, seed)?;

        let mut scratch = Scratch::default();
        let base = p.profile.width.max(p.profile.height);
        let mut refs = Vec::new();
        let first = scratch.path(self.scratch.join(format!("{id}-{step}-{n}-ref1.png")));
        stage_reference(&p.raw, &first, base)?;
        refs.push(first);
        for (i, stem) in req.references.iter().enumerate() {
            let src = self.output_source(stem)?;
            let dst = scratch.path(self.scratch.join(format!("{id}-{step}-{n}-ref{}.png", i + 2)));
            stage_reference(&src, &dst, base)?;
            refs.push(dst);
        }

        // The image's own shape at the model's own size — never bigger. An
        // edit is a txt2img with the image as a reference, so its cost is the
        // render's tokens *plus* the reference's: at the 2048 a generate's 2×
        // Output upscale makes, Klein 9B needed ~20k tokens a step and ran out
        // of memory on a 24 GB card one segment short, at 19 s/it. At the model's
        // size it is a quarter of that and what the model was trained at; the
        // result is scaled back to the image's size, and a Refine afterwards
        // brings a large image's detail back. Rescaling only changes the shape
        // drawn at: the reference is still the image as it is, and the model
        // re-imagines it to fill the new frame.
        let (ew, eh) = edit_size(tw, th, base);
        let profile = Profile { width: ew, height: eh, ..p.profile.uncached() };
        let out = scratch.path(self.scratch.join(format!("{id}-{step}-{n}-edit.png")));
        let t = Instant::now();
        // No reference cap: reference 1 is the image being changed, and drawing
        // it smaller would blur exactly what the edit has to keep.
        self.sd().txt2img(&profile, &prompt, p.negative.as_deref(), seed, &out, &refs, None)?;
        let gen_ms = t.elapsed().as_millis();

        let t = Instant::now();
        let img = image::open(&out).with_context(|| format!("reading {}", out.display()))?;
        let img = if (img.width(), img.height()) == (tw, th) {
            img
        } else {
            img.resize_exact(tw, th, image::imageops::FilterType::Lanczos3)
        };
        let raw = scratch.path(self.scratch.join(format!("{id}-{step}-{n}-raw.png")));
        let display = scratch.path(self.scratch.join(format!("{id}-{step}-{n}-full.png")));
        img.to_rgb8().save(&raw).with_context(|| format!("writing {}", raw.display()))?;
        let filters = p.filters.restart(p.profile.sharpness, p.profile.clarity);
        let area = p.area.as_ref().map(Area::restart);
        let (png, w, h) = publish(&self.fsr, &raw, &display, &filters, area.as_ref())?;
        let fsr_ms = t.elapsed().as_millis();

        scratch.keep(&raw);
        scratch.keep(&display);
        let prev = p.clone();
        let entry = Pending {
            raw,
            raw_w: w,
            raw_h: h,
            display,
            filters,
            area,
            step,
            ..p
        };
        let what = if req.expand.is_empty() {
            "Edit".to_string()
        } else {
            format!("Edit and rescale {}", req.expand.describe())
        };
        let entry = advance(&prev, entry, step_label(&what, Some(instruction)));
        let result = entry.result(id, png, Timings { gen_ms, fsr_ms, weights: self.monitor.placement(), load_ms: self.monitor.load_ms(), stages: self.monitor.stages(), ..Default::default() });
        self.keep(id, entry);
        Ok(result)
    }

    /// Enlarge the image with no diffusion at all: FSR's EASU, or an upscaler
    /// model (`upscaler` names one under `Config::upscaler_dir`, `None` is
    /// EASU) — the same `enlarge` a generate's first stage uses, pointed at the
    /// image in hand. A step in its history like any pass.
    ///
    /// Unlike Hi-res it re-draws nothing, so it can't undo a refine: it can be
    /// the last step (a bigger file) or come before a refine (detail added at
    /// the new size). Reads `raw` and publishes through `publish`, so the
    /// display filters still run once, over the enlarged pixels (invariant 1),
    /// with the detail pair restarted for the new size (invariant 6).
    pub fn upscale(&mut self, id: u64, factor: f32, upscaler: Option<String>) -> Result<GenResult> {
        if !(factor > 1.0 && factor <= MAX_UPSCALE) {
            bail!("upscale must be more than 1× and at most {MAX_UPSCALE}×");
        }
        let p = lock(&self.retained).get(id, "upscale")?;
        let model = self.cfg.check_upscaler(&upscaler)?;
        // Before the work, not after: a model pass can run for minutes and only
        // then meet the post-process's limit. Upscaling an upscale doubles fast.
        let (w, h) = crate::fsr::out_size((p.raw_w, p.raw_h), factor);
        self.check_side(w, h)?;
        let step = p.step + 1;
        let n = JOB_COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut scratch = Scratch::default();
        let raw = scratch.path(self.scratch.join(format!("{id}-{step}-{n}-raw.png")));
        let display = scratch.path(self.scratch.join(format!("{id}-{step}-{n}-full.png")));

        let t = Instant::now();
        self.enlarge(&p.raw, &raw, factor, model.as_deref())?;
        let upscale_ms = t.elapsed().as_millis();

        let t = Instant::now();
        let filters = p.filters.restart(p.profile.sharpness, p.profile.clarity);
        let area = p.area.as_ref().map(Area::restart);
        let (png, w, h) = publish(&self.fsr, &raw, &display, &filters, area.as_ref())?;
        let fsr_ms = t.elapsed().as_millis();

        scratch.keep(&raw);
        scratch.keep(&display);
        let prev = p.clone();
        let entry = Pending {
            raw,
            raw_w: w,
            raw_h: h,
            display,
            filters,
            area,
            step,
            ..p
        };
        // "Upscale 2× · FSR", "Upscale 4× · 4x-UltraSharp": which one ran is
        // what tells two upscale steps apart in the list.
        let by = upscaler
            .as_deref()
            .map(|n| Path::new(n).file_stem().map_or(n.to_string(), |s| s.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "FSR".into());
        let entry = advance(&prev, entry, format!("Upscale {factor}× · {by}"));
        let result = entry.result(id, png, Timings { fsr_ms, upscale_ms, ..Default::default() });
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
        self.check_side(target_w, target_h)?;
        // Largest size the diffusion pass may run at, snapped to a multiple of
        // 64 so the latent dimensions stay clean. Only the width is snapped:
        // FSR scales both axes by one factor, so the height follows from it.
        let budget = p.profile.hires_max_side.min(target_w.max(target_h));
        let diff_scale = budget as f32 / p.raw_w.max(p.raw_h) as f32;
        let diff_w = snap64(((p.raw_w as f32) * diff_scale).round() as u32);

        let step = p.step + 1;
        // Unique per pass, not just per step: after stepping back to step 1,
        // the next pass is step 2 again, and `{id}-{step}` would overwrite the
        // old step 2's file and then delete it as a discarded branch.
        let n = JOB_COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut scratch = Scratch::default();
        let upscaled = scratch.path(self.scratch.join(format!("{id}-{step}-{n}-up.png")));
        let hires = scratch.path(self.scratch.join(format!("{id}-{step}-{n}-hires.png")));
        let raw = scratch.path(self.scratch.join(format!("{id}-{step}-{n}-raw.png")));
        let display = scratch.path(self.scratch.join(format!("{id}-{step}-{n}-full.png")));
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

        // 2) The detail pass itself, with its own prompt when one was typed.
        //    Uncached, as refine is: see `Profile::uncached`.
        let vars = self.cfg.settings().variables;
        let prompt = pass_prompt(over.hires_prompt.as_deref(), &p.prompt, &vars, p.seed)?;
        let t = Instant::now();
        sd.img2img(
            &p.profile.uncached(),
            &prompt,
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
        let area = p.area.as_ref().map(Area::restart);
        let (png, w, h) = publish(&self.fsr, &raw, &display, &filters, area.as_ref())?;
        fsr_ms += t.elapsed().as_millis();

        scratch.keep(&raw);
        scratch.keep(&display);
        let prev = p.clone();
        let entry = Pending {
            raw,
            raw_w: w,
            raw_h: h,
            display,
            // Detail back to the model's starting point, the grade kept — see
            // `Filters::restart`.
            filters,
            area,
            hires_passes: p.hires_passes + 1,
            step,
            ..p
        };
        let entry = advance(&prev, entry, step_label(&format!("Hi-res {upscale}×"), over.hires_prompt.as_deref()));
        let result = entry.result(id, png, Timings { fsr_ms, hires_ms, weights: self.monitor.placement(), load_ms: self.monitor.load_ms(), stages: self.monitor.stages(), ..Default::default() });
        self.keep(id, entry);
        Ok(result)
    }

    /// Refuse a size past the largest image allowed, *before* any work: the
    /// chosen limit (`Settings::max_image_side`, read per job like every
    /// setting) under the GPU's own (`Fsr::max_side`). The message names the
    /// setting, so the way to go bigger is in the error, and says when the GPU
    /// is the one in the way, since no setting gets past that.
    fn check_side(&self, w: u32, h: u32) -> Result<()> {
        let gpu = lock(&self.fsr).max_side();
        let chosen = self.cfg.settings().max_image_side;
        if w.max(h) <= chosen.min(gpu) {
            return Ok(());
        }
        if w.max(h) <= gpu {
            bail!(
                "{w}×{h} is past the largest image size, {chosen} px on a side. Raise \
                 \"Largest image side\" in Settings → Image (this GPU allows up to {gpu}), \
                 or make it smaller. Very large images can be slow to show in the page."
            );
        }
        bail!(
            "{w}×{h} is past this GPU's limit of {gpu} px on a side, which no setting can \
             raise — make it smaller, or start from a smaller step in the history."
        )
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
    cfg: Config,
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
    ///
    /// `area` is the area's whole new state: `None` keeps it, `Some(None)`
    /// removes it.
    pub fn adjust(&mut self, id: u64, patch: &FiltersPatch, area: Option<Option<Area>>) -> Result<GenResult> {
        let mut p = lock(&self.retained).get(id, "adjust")?;
        patch.apply(&mut p.filters);
        // Same bounds the config goes through, from the same place.
        p.filters.validate(&p.profile.name)?;
        if let Some(area) = area {
            if let Some(a) = &area {
                a.validate()?;
            }
            p.area = area;
        }

        let t = Instant::now();
        // Straight back over the same `display` path, and `step` doesn't move:
        // `display` is only ever an output — no pass reads it — so there is no
        // new file to make, and a drag can't churn the work dir or push older
        // results out of the cache.
        //
        // Deliberately no `Scratch` here, unlike the three passes above. This
        // writes over a file the retained `Pending` already owns, so putting it
        // under the guard would mean a failed drag deleting a live image.
        let (png, w, h) = publish(&self.fsr, &p.raw, &p.display, &p.filters, p.area.as_ref())?;
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
            return Err(Superseded.into());
        }
        Ok(result)
    }

    /// Make step `index` of an image's history the live image again: the way
    /// back from a pass that went wrong. No diffusion — `publish` over that
    /// step's raw with the filters as they are now — so it is the display lane's
    /// and answers mid-render, like the sliders. Nothing is deleted: stepping
    /// forward again works until a new pass starts a new branch (`advance`).
    pub fn goto(&mut self, id: u64, index: usize) -> Result<GenResult> {
        let p = lock(&self.retained).get(id, "step through")?;
        let history = p.steps();
        let s = p.step(index).with_context(|| format!("this image has no step {index}"))?;
        let t = Instant::now();
        let (png, w, h) = publish(&self.fsr, &s.raw, &p.display, &p.filters, p.area.as_ref())?;
        let fsr_ms = t.elapsed().as_millis();
        let was = p.raw.clone();
        let entry = Pending {
            raw: s.raw,
            raw_w: w,
            raw_h: h,
            profile: s.profile,
            refine_passes: s.refine_passes,
            hires_passes: s.hires_passes,
            step: s.step,
            history,
            at: index,
            ..p
        };
        let result = entry.result(id, png, Timings { fsr_ms, ..Default::default() });
        if !lock(&self.retained).replace(id, &was, entry) {
            bail!("that image changed while stepping through its history — try again");
        }
        Ok(result)
    }

    /// Crop the image to `rect` — `(x, y, width, height)` in its own pixels —
    /// as a history step, so the Refine dialog's History and the viewer's
    /// *Undo crop* step back from it. On `raw`, not a display filter: every
    /// pass reads `raw` (invariant 1), so a crop that lived only in the display
    /// would be undone by the next Refine, and "crop to the face, then Hi-res"
    /// is the point. No diffusion, so the display lane's, like `goto`.
    ///
    /// The filters stay as they are — it is the same picture, smaller — and
    /// the area is mapped into the new frame (`Area::crop`). `opened_from` is
    /// kept, unlike after a pass, so the Outputs viewer's *Save changes*
    /// writes the crop back over the file it opened.
    pub fn crop(&mut self, id: u64, rect: (u32, u32, u32, u32)) -> Result<GenResult> {
        let p = lock(&self.retained).get(id, "crop")?;
        let (x, y, w, h) = crop_rect(rect, p.raw_w, p.raw_h)?;
        let step = p.step + 1;
        let n = JOB_COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut scratch = Scratch::default();
        let raw = scratch.path(self.scratch.join(format!("{id}-{step}-{n}-raw.png")));
        let display = scratch.path(self.scratch.join(format!("{id}-{step}-{n}-full.png")));
        let img = image::open(&p.raw).with_context(|| format!("loading {}", p.raw.display()))?;
        img.crop_imm(x, y, w, h)
            .save(&raw)
            .with_context(|| format!("writing {}", raw.display()))?;
        let area = p.area.and_then(|a| a.crop((x, y, w, h), p.raw_w, p.raw_h));

        let t = Instant::now();
        let (png, out_w, out_h) = publish(&self.fsr, &raw, &display, &p.filters, area.as_ref())?;
        let fsr_ms = t.elapsed().as_millis();

        let was = p.raw.clone();
        let prev = p.clone();
        let entry = Pending {
            raw: raw.clone(),
            raw_w: out_w,
            raw_h: out_h,
            display: display.clone(),
            area,
            step,
            ..p
        };
        let entry = advance(&prev, entry, format!("Crop {w}×{h}"));
        let result = entry.result(id, png, Timings { fsr_ms, ..Default::default() });
        // Only over the image it read, as `goto`: a pass that landed meanwhile
        // owns the image now, and the crop was of the one it replaced. The new
        // files stay under `scratch` until then, so a refusal removes them.
        if !lock(&self.retained).replace(id, &was, entry) {
            return Err(Superseded.into());
        }
        scratch.keep(&raw);
        scratch.keep(&display);
        Ok(result)
    }

    /// *Apply adjustments*: make the image as it is shown — grade, detail and
    /// area all in it — the image itself, as a history step, and start the
    /// look over from nothing on it. The filters go to `NONE`, the area to
    /// `None`, and the profile's slider start points to 0, as `open_output`
    /// does for a picture whose look is already in its pixels.
    ///
    /// **The one place a look reaches `raw`**, and only because it was asked
    /// for: a Refine or Hi-res after it works on the adjusted picture, as
    /// *Refine & extend* on a saved output does. Without it invariant 1 holds;
    /// with it, the step before is the untouched picture to go back to.
    pub fn apply_look(&mut self, id: u64) -> Result<GenResult> {
        let p = lock(&self.retained).get(id, "apply adjustments to")?;
        let acting_area = p.area.filter(Area::acts);
        if p.filters == Filters::NONE && acting_area.is_none() {
            bail!("nothing to apply — every slider is at 0");
        }
        let label = adjust_label(&p.filters, acting_area.as_ref());
        let step = p.step + 1;
        let n = JOB_COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut scratch = Scratch::default();
        let raw = scratch.path(self.scratch.join(format!("{id}-{step}-{n}-raw.png")));
        let display = scratch.path(self.scratch.join(format!("{id}-{step}-{n}-full.png")));
        std::fs::copy(&p.display, &raw).with_context(|| format!("copying {}", p.display.display()))?;

        let t = Instant::now();
        // Neutral, so the published picture is the applied one to the byte.
        let (png, w, h) = publish(&self.fsr, &raw, &display, &Filters::NONE, None)?;
        let fsr_ms = t.elapsed().as_millis();

        let was = p.raw.clone();
        let prev = p.clone();
        let mut profile = p.profile.clone();
        profile.sharpness = 0.0;
        profile.clarity = 0.0;
        let entry = Pending {
            raw: raw.clone(),
            raw_w: w,
            raw_h: h,
            display: display.clone(),
            filters: Filters::NONE,
            area: None,
            profile,
            step,
            ..p
        };
        let entry = advance(&prev, entry, label);
        let result = entry.result(id, png, Timings { fsr_ms, ..Default::default() });
        if !lock(&self.retained).replace(id, &was, entry) {
            return Err(Superseded.into());
        }
        scratch.keep(&raw);
        scratch.keep(&display);
        Ok(result)
    }

    /// Make `name` the model the image's passes run on — the Refine dialog's
    /// picker for a picture opened from Outputs, whose maker is unknown (an
    /// upload) or only a guess. Replaces the profile wholesale, as
    /// `Engine::swap_model` does, so Refine, Refine area, Hi-res and Edit all
    /// follow it with nothing of their own; a fill-only model is refused here
    /// as `Engine::model` refuses it. The slider start points are kept: for an
    /// output they are zero, its look already being in the pixels, and a new
    /// model's house sharpness must not turn that into a sharpen. No pixels
    /// change, so the published file is answered as it is.
    pub fn set_model(&mut self, id: u64, name: &str) -> Result<GenResult> {
        let p = lock(&self.retained).get(id, "change the model of")?;
        let mut reg = self.cfg.registry();
        let mut next = match reg.remove(name) {
            Some(m) if m.family.fill_only() => {
                bail!("'{name}' is a {} model — it extends pictures rather than refining them", m.family.label())
            }
            Some(m) => m,
            None => bail!("unknown model '{name}'"),
        };
        next.require_files()?;
        next.sharpness = p.profile.sharpness;
        next.clarity = p.profile.clarity;
        next.refine_model = p.profile.refine_model.clone();
        next.hires_model = p.profile.hires_model.clone();
        let png = std::fs::read(&p.display).with_context(|| format!("reading {}", p.display.display()))?;
        let entry = Pending { profile: next, ..p };
        let result = entry.result(id, png, Timings::default());
        if !lock(&self.retained).update(id, entry) {
            bail!("that image changed while picking its model — try again");
        }
        Ok(result)
    }

    /// Step `index` as the page would show it, for the Refine dialog's
    /// before/after compare — and nothing else: no write to `Retained`, no
    /// history move, the live image untouched. Through `publish` with the
    /// filters on screen *now*, never the raw file: a before shown unsharpened
    /// beside a sharpened now would rig every comparison. Display lane, so it
    /// answers mid-render like the sliders.
    pub fn view_step(&mut self, id: u64, index: usize) -> Result<(Vec<u8>, u32, u32)> {
        let p = lock(&self.retained).get(id, "compare")?;
        let s = p.step(index).with_context(|| format!("this image has no step {index}"))?;
        static N: AtomicU64 = AtomicU64::new(0);
        let tmp = self
            .scratch
            .join(format!("{id}-view-{}.png", N.fetch_add(1, Ordering::Relaxed)));
        let shown = publish(&self.fsr, &s.raw, &tmp, &p.filters, p.area.as_ref());
        let _ = std::fs::remove_file(&tmp);
        shown
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
        // The validated id, never the sidecar's own `id` field: a copied-in
        // sidecar whose field disagrees with its file name would open another
        // item's picture.
        let stored = self.items.image_path(shelf, work_id);

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
        let (png, w, h) = publish(&self.fsr, &raw, &display, &filters, None)?;
        let fsr_ms = t.elapsed().as_millis();

        scratch.keep(&raw);
        scratch.keep(&display);

        let entry = Pending {
            history: Vec::new(),
            at: 0,
            raw,
            raw_w: w,
            raw_h: h,
            display,
            filters,
            area: None,
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
            opened_from: None,
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
    ///
    /// `Slot::Image` is the Outputs viewer's *Refine & extend…*: the same
    /// picture made the Image tab's result, so the Refine dialog can work on
    /// it. That is a diffusion over finished pixels, chosen — the same choice
    /// as picking it as *Image to image*'s source — and (1) still holds: the
    /// filters start at none, so nothing already in it is applied twice. There
    /// the profile has to name a model a Refine can run, which an upload's
    /// default doesn't and a sidecar's may no longer; `fallback` (the Image
    /// tab's selected model) stands in then.
    pub fn open_output(&mut self, stem: &str, slot: Slot, fallback: Option<&str>) -> Result<GenResult> {
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
        let (png, w, h) = publish(&self.fsr, &raw, &display, &filters, None)?;
        let fsr_ms = t.elapsed().as_millis();

        scratch.keep(&raw);
        scratch.keep(&display);

        let mut profile = item.as_ref().map(|i| i.profile.clone()).unwrap_or_default();
        if slot == Slot::Image {
            let mut reg = self.cfg.registry();
            if !reg.contains_key(&profile.name) {
                let name = fallback.filter(|n| !n.is_empty()).context(
                    "pick a model on the Image tab first — this picture names none that is installed",
                )?;
                profile = reg
                    .remove(name)
                    .filter(|p| !p.family.fill_only())
                    .with_context(|| format!("unknown model '{name}'"))?;
            }
        }
        // See (2) above: these two are where the sliders start, and for this one
        // image "where they start" is off.
        profile.sharpness = 0.0;
        profile.clarity = 0.0;

        let mut entry = Pending {
            history: Vec::new(),
            at: 0,
            raw,
            raw_w: w,
            raw_h: h,
            display,
            filters,
            area: None,
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
            // Only the Outputs viewer's own slot saves back over the file;
            // *Refine & extend…* (`Slot::Image`) makes a new picture of it.
            opened_from: (slot == Slot::Output).then(|| stem.to_string()),
            refine_passes: item.as_ref().map(|i| i.refine_passes).unwrap_or(0),
            hires_passes: item.as_ref().map(|i| i.hires_passes).unwrap_or(0),
            step: item.as_ref().map(|i| i.step).unwrap_or(0),
        };
        // Its first step is the saved picture, not a generation: the implicit
        // label would say "Generated" in the viewer's History.
        entry.history = vec![Snapshot::of(&entry, "Opened".into())];
        let result = entry.result(id, png, Timings { fsr_ms, ..Default::default() });
        lock(&self.retained).store(id, slot, entry, false);
        Ok(result)
    }

    /// Save the image on screen back over the output it was opened from — the
    /// Outputs viewer's *Save changes*. Only an image `open_output` put in
    /// `Slot::Output` knows that file (`Pending::opened_from`); anything else is
    /// refused rather than guessed at. Why it is safe to do: see
    /// [`workitems::Store::overwrite_output`].
    pub fn overwrite_output(&self, id: u64) -> Result<PathBuf> {
        let p = lock(&self.retained).get(id, "save")?;
        let stem = p
            .opened_from
            .as_deref()
            .context("this image wasn't opened from Outputs, so there is no file to save over")?;
        self.items.overwrite_output(stem, &p.display, self.cfg.settings().embed_png_settings)
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
        self.items.save_output(&p.display, &item, self.cfg.settings().embed_png_settings)
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
    area: Option<&Area>,
) -> Result<(Vec<u8>, u32, u32)> {
    let (w, h) = match area.filter(|a| a.acts()) {
        None => lock(fsr).process(raw, display, 1.0, filters)?,
        // Two looks and a blend: the whole picture's, then the area's on top
        // of it, mixed by the area's weight (`area::composite`). In memory, so
        // neither look round-trips through a PNG; only the blend is written.
        Some(a) => {
            let img = image::open(raw)
                .with_context(|| format!("loading {}", raw.display()))?
                .to_rgba8();
            let (mut base, local) = {
                let fsr = lock(fsr);
                let base = fsr.filter(&img, filters)?;
                let local = fsr.filter(&base, &a.filters)?;
                (base, local)
            };
            crate::area::composite(&mut base, &local, a);
            base.save(display)
                .with_context(|| format!("saving {}", display.display()))?;
            base.dimensions()
        }
    };
    let png = std::fs::read(display)?;
    Ok((png, w, h))
}

impl Pending {
    fn result(&self, id: u64, png: Vec<u8>, timings: Timings) -> GenResult {
        let steps = self.steps();
        GenResult {
            step: self.step,
            history: steps.into_iter().map(|s| s.label).collect(),
            at: self.at,
            png,
            width: self.raw_w,
            height: self.raw_h,
            seed: self.seed,
            timings,
            refine_passes: self.refine_passes,
            hires_passes: self.hires_passes,
            image_id: id,
            work_id: self.work_id.clone(),
            can_hires: self.hires_fits(),
            model: self.profile.name.clone(),
            can_edit: self.profile.family.takes_references(),
            filters: self.filters,
            start: Filters::starting_at(self.profile.sharpness, self.profile.clarity),
            area: self.area,
            prompt: self.prompt.clone(),
        }
    }
}

/// An applied look's history label: the knobs it moved, by name, or a count
/// past three, and whether an area went in with it.
fn adjust_label(filters: &Filters, area: Option<&Area>) -> String {
    let moved = filters.changed();
    let mut text = if moved.len() > 3 {
        format!("{} changes", moved.len())
    } else {
        moved
            .iter()
            .map(|(name, v)| {
                let mut c = name.chars();
                let first = c.next().map(|f| f.to_uppercase().collect::<String>()).unwrap_or_default();
                format!("{first}{} {v:+.2}", c.as_str())
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    if area.is_some() {
        text = if text.is_empty() { "area".into() } else { format!("{text} + area") };
    }
    format!("Adjust: {text}")
}

/// The smallest crop: below a model's smallest working size a Refine or
/// Hi-res of it would be enlarging a thumbnail.
const MIN_CROP: u32 = 64;

/// A crop request as it will run: width and height snapped *down* to a
/// multiple of 8, so a later Refine or Hi-res latent covers the picture
/// exactly, then checked to lie inside the picture and to change something.
fn crop_rect((x, y, w, h): (u32, u32, u32, u32), full_w: u32, full_h: u32) -> Result<(u32, u32, u32, u32)> {
    let (w, h) = (w / 8 * 8, h / 8 * 8);
    if w < MIN_CROP || h < MIN_CROP {
        bail!("a crop must be at least {MIN_CROP}×{MIN_CROP} pixels");
    }
    if x.checked_add(w).is_none_or(|r| r > full_w) || y.checked_add(h).is_none_or(|b| b > full_h) {
        bail!("the crop must lie inside the picture ({full_w}×{full_h})");
    }
    if (w, h) == (full_w, full_h) {
        bail!("that crop is the whole picture");
    }
    Ok((x, y, w, h))
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
    // so `purge_pngs` reaches it after a crash. Its own guard, since it never
    // outlives this. Process id and a counter in the name: from the `upscale`
    // subcommand `dst` is the user's own path, and `out.model.png` beside it
    // could be a file of theirs that the guard would then delete.
    let mut scratch = Scratch::default();
    let n = JOB_COUNTER.fetch_add(1, Ordering::Relaxed);
    let big = scratch.path(dst.with_extension(format!("{}-{n}.model.png", std::process::id())));
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
            "[yasdig] {} {}× in {} tile(s) of {} px, {} ms",
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

/// The size an edit draws a `w`×`h` result at: that shape with its long side
/// at most the model's `base`, each axis snapped to 64. Never bigger — see
/// `Engine::edit`.
fn edit_size(w: u32, h: u32, base: u32) -> (u32, u32) {
    let long = w.max(h);
    let k = long.min(base) as f32 / long as f32;
    (snap64(((w as f32) * k).round() as u32), snap64(((h as f32) * k).round() as u32))
}

/// A reference image, copied into scratch for sd-cli's `-r` — never handed
/// over in place (invariant 8) — and shrunk to `max_side` when bigger. Each
/// reference costs attention tokens like the image itself, so an unshrunk
/// phone photo would be several images' worth of memory and time; one already
/// small enough is copied byte for byte.
fn stage_reference(src: &Path, dst: &Path, max_side: u32) -> Result<()> {
    let img = image::open(src).with_context(|| format!("reading {}", src.display()))?;
    let (fw, fh) = fit_to((img.width(), img.height()), max_side);
    if (fw, fh) == (img.width(), img.height()) {
        std::fs::copy(src, dst).with_context(|| format!("copying {}", src.display()))?;
    } else {
        img.resize_exact(fw, fh, image::imageops::FilterType::Lanczos3)
            .save(dst)
            .with_context(|| format!("writing {}", dst.display()))?;
    }
    Ok(())
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
    use super::{
        adjust_label, crop_rect,
        advance, after_model, edit_size, fit_to, pass_prompt, step_label, AfterModel, Expand, Pending, Retained,
        Slot, MAX_HISTORY,
    };
    use crate::config::{Filters, Profile};
    use crate::fsr::out_size;
    use std::path::Path;

    /// The sides are drawn left and right first, so a later strip spans their
    /// new edges and draws the corners; past the limit is refused, and the
    /// step's name says what grew.
    #[test]
    fn an_expansion_grows_in_side_order_and_names_itself() {
        use crate::redraw::{Side, MAX_EXTEND};
        let e = Expand { left: 256, top: 0, right: 256, bottom: 128 };
        assert_eq!(e.grown(1024, 1024), (1536, 1152));
        let order: Vec<Side> = e.sides().unwrap().iter().map(|&(s, _)| s).collect();
        assert_eq!(order, [Side::Left, Side::Right, Side::Bottom]);
        assert_eq!(e.describe(), "←256 →256 ↓128");
        assert_eq!(Expand { left: 256, right: 256, ..Default::default() }.describe(), "← → 256 px");
        assert!(Expand::default().is_empty() && Expand::default().sides().unwrap().is_empty());
        assert!(Expand { top: MAX_EXTEND + 1, ..Default::default() }.sides().is_err());
    }

    /// An edit draws the target's shape at the model's size, never bigger —
    /// rescaling changes the shape, not the cost.
    #[test]
    fn an_edit_draws_its_target_shape_at_the_models_size() {
        assert_eq!(edit_size(1024, 1024, 1024), (1024, 1024));
        // 1024² grown 256 each side: 1536×1024 drawn at 1024 long.
        assert_eq!(edit_size(1536, 1024, 1024), (1024, 640));
        // A small image is drawn at its own size, snapped.
        assert_eq!(edit_size(800, 600, 1024), (768, 576));
        // A 2× upscale of 1024² is drawn at 1024², as before rescaling existed.
        assert_eq!(edit_size(2048, 2048, 1024), (1024, 1024));
    }

    /// A retained image whose two files exist, so eviction has something to
    /// delete and the tests something to look for.
    #[test]
    fn an_applied_look_is_labelled_by_what_it_moved() {
        let f = Filters { lightness: 0.2, vibrance: 0.3, ..Filters::NONE };
        assert_eq!(adjust_label(&f, None), "Adjust: Lightness +0.20, Vibrance +0.30");
        let many = Filters { lightness: 0.1, contrast: 0.1, shadows: -0.1, hue: 0.1, ..Filters::NONE };
        assert_eq!(adjust_label(&many, None), "Adjust: 4 changes");
        let a = crate::area::Area {
            shape: crate::area::Shape::Rect,
            x0: 0.1,
            y0: 0.1,
            x1: 0.5,
            y1: 0.5,
            feather: 0.0,
            invert: false,
            filters: Filters { tint: 0.2, ..Filters::NONE },
        };
        assert_eq!(adjust_label(&Filters::NONE, Some(&a)), "Adjust: area");
        assert_eq!(adjust_label(&f, Some(&a)), "Adjust: Lightness +0.20, Vibrance +0.30 + area");
    }

    #[test]
    fn a_crop_snaps_to_eight_and_stays_inside_the_picture() {
        assert_eq!(crop_rect((10, 20, 515, 300), 1024, 1024).unwrap(), (10, 20, 512, 296));
        assert!(crop_rect((0, 0, 70, 63), 1024, 1024).is_err(), "too small once snapped");
        assert!(crop_rect((600, 0, 512, 512), 1024, 1024).is_err(), "past the right edge");
        assert!(crop_rect((u32::MAX, 0, 512, 512), 1024, 1024).is_err(), "no overflow");
        assert!(crop_rect((0, 0, 1024, 1024), 1024, 1024).is_err(), "the whole picture");
        // A side that isn't a multiple of 8 can still be kept whole on the other axis.
        assert_eq!(crop_rect((0, 0, 1000, 700), 1000, 768).unwrap(), (0, 0, 1000, 696));
    }

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
            area: None,
            prompt: String::new(),
            negative: None,
            typed_prompt: None,
            typed_negative: None,
            seed: 0,
            work_id: None,
            source: None,
            opened_from: None,
            refine_passes: 0,
            hires_passes: 0,
            step: 0,
            history: Vec::new(),
            at: 0,
        }
    }

    /// `prev` after a pass that wrote `name`: what `refine` does, minus sd-cli.
    fn pass(dir: &Path, prev: &Pending, name: &str) -> Pending {
        let next = Pending { step: prev.step + 1, ..live(dir, name) };
        advance(prev, next, name.into())
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("yasdig-slot-{tag}-{}", std::process::id()));
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

    /// `goto` and `view_step` read a step through one door: index 0 of a fresh
    /// image is the image itself, every step of a history is its own raw, and
    /// past the end is nothing.
    #[test]
    fn a_step_is_read_from_one_place() {
        let dir = scratch("step");
        let g = live(&dir, "g");
        assert_eq!(g.step(0).map(|s| s.raw), Some(g.raw.clone()));
        assert!(g.step(1).is_none());
        let a = pass(&dir, &g, "a");
        let b = pass(&dir, &a, "b");
        let raws: Vec<_> = (0..3).map(|i| b.step(i).unwrap().raw).collect();
        assert_eq!(raws, [g.raw.clone(), a.raw.clone(), b.raw.clone()]);
        assert!(b.step(3).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A pass's prompt is its own when typed, rolled off the seed it was given,
    /// and the image's own otherwise — blank and whitespace included.
    #[test]
    fn a_pass_prompt_falls_back_to_the_images_own() {
        let mut vars = crate::variables::Variables::new();
        vars.insert("color".into(), vec!["red".into(), "blue".into(), "green".into()]);
        let image = "a crab in a red hat";
        assert_eq!(pass_prompt(None, image, &vars, 1).unwrap(), image);
        assert_eq!(pass_prompt(Some("   "), image, &vars, 1).unwrap(), image);
        assert_eq!(pass_prompt(Some(" sharper fabric "), image, &vars, 1).unwrap(), "sharper fabric");
        let rolled = pass_prompt(Some("a {color} scarf"), image, &vars, 5).unwrap();
        assert!(!rolled.contains('{'), "{rolled}");
        assert_eq!(rolled, pass_prompt(Some("a {color} scarf"), image, &vars, 5).unwrap(), "the seed decides");
        assert!(pass_prompt(Some("{nope}"), image, &vars, 1).is_err(), "an unknown name is an error, as everywhere");
    }

    #[test]
    fn a_step_label_says_what_the_pass_was_asked() {
        assert_eq!(step_label("Refine", None), "Refine");
        assert_eq!(step_label("Refine", Some("  ")), "Refine");
        assert_eq!(step_label("Area", Some("an open hand")), "Area: an open hand");
        let long = "x".repeat(60);
        assert_eq!(step_label("Hi-res 2×", Some(&long)), format!("Hi-res 2×: {}…", "x".repeat(40)));
    }

    /// Every pass is a step: the image it replaced stays, and stepping back
    /// then taking a new pass drops the branch that was stepped back from.
    #[test]
    fn a_new_pass_after_stepping_back_drops_the_old_branch() {
        let dir = scratch("history");
        let g = live(&dir, "g");
        let fingers = pass(&dir, &g, "fingers");
        let hat = pass(&dir, &fingers, "hat");
        assert_eq!(hat.history.len(), 3);
        assert_eq!(hat.at, 2);
        assert!(g.raw.exists() && fingers.raw.exists() && hat.raw.exists());
        assert!(!g.display.exists() && !fingers.display.exists(), "displays are remade, not kept");

        // Back to the fingers (what `goto` writes), then a better hat.
        let back = Pending { at: 1, ..fingers.clone() };
        let back = Pending { history: hat.history.clone(), ..back };
        let hat2 = pass(&dir, &back, "hat2");
        let labels: Vec<&str> = hat2.history.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, ["Generated", "fingers", "hat2"]);
        assert!(!hat.raw.exists(), "the bad hat's step is gone");
        assert!(fingers.raw.exists() && g.raw.exists());

        hat2.remove_files();
        assert!(!g.raw.exists() && !fingers.raw.exists() && !hat2.raw.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn history_keeps_the_original_and_drops_the_oldest_after_it() {
        let dir = scratch("cap");
        let g = live(&dir, "g");
        let mut p = g.clone();
        let mut made = Vec::new();
        for i in 0..MAX_HISTORY + 3 {
            p = pass(&dir, &p, &format!("s{i}"));
            made.push(p.raw.clone());
        }
        assert_eq!(p.history.len(), MAX_HISTORY);
        assert_eq!(p.history[0].label, "Generated");
        assert!(g.raw.exists(), "the original stays");
        // Fifteen passes and the original is sixteen: the four oldest after
        // the original go.
        assert!(!made[0].exists() && !made[3].exists() && made[4].exists());
        assert_eq!(p.at, MAX_HISTORY - 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A step back writes only over the image it read; a pass that landed
    /// meanwhile wins.
    #[test]
    fn a_step_never_writes_over_a_newer_pass() {
        let dir = scratch("replace");
        let mut r = Retained::default();
        let a = live(&dir, "a");
        r.store(1, Slot::Image, a.clone(), false);
        let b = pass(&dir, &a, "b");
        let stepped = Pending { at: 0, ..b.clone() };
        r.store(1, Slot::Image, b.clone(), false);
        assert!(r.replace(1, &b.raw, stepped.clone()));
        assert!(!r.replace(1, &a.raw, stepped), "read a, but b is live");
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
