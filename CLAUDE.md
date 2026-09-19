# CLAUDE.md

Python-free Vulkan image generator for AMD/Linux. Generate small and cheap with
stable-diffusion.cpp (shelled out as a subprocess, never FFI), then escalate quality
on demand with img2img passes and a native FSR post-process in `wgpu`. All GPU work —
sd.cpp *and* the FSR compute passes — is serialized through one worker thread, so
nothing thrashes VRAM.

**`README.md` is thorough and user-facing: setup, the `models.toml` field table, the
API shapes, the design rationale. Read it for any of those.** This file is only the
part it doesn't cover.

## Commands

```sh
cargo test                                  # no GPU: naga-validates the 3 WGSL shaders, the band-measurement math,
                                            #   the escalation/upload DTOs and `fit_to`
cargo run --release                         # server on 127.0.0.1:8080
cargo run --release -- doctor               # preflight only: binary, checkpoints, web UI
cargo run --release -- fsr-selftest         # full wgpu path; no sd.cpp or checkpoint needed
cargo run --release -- fsr in.png out.png [scale] [sharpness] [clarity]
```

The shaders otherwise only compile at GPU device creation, so `cargo test` is what
turns a WGSL typo into a test failure instead of a startup failure on the worker
thread. `fsr-selftest` and `fsr` exercise the GPU path with no model weights — the
fast loop when touching `src/fsr/`.

**`fsr-selftest` is what to run after changing a filter constant.** It asserts the
dimensions and that scale 1.0 with both filters at 0 is byte-exact (claimed by two
shader headers and by `README.md`, and unchecked until it wasn't), then plants four
gratings at wavelengths proportional to the image and prints how much each filter moved
each band, at 1536² and 3072². Those two sizes are invariant 6 below with a number on
it — same setting, RCAS radius 1 vs 2 — and the band rows are how you catch a "clarity"
edit that has quietly become a high-pass. They are printed rather than asserted because
`rcas_radius` is a step function and nobody has yet seen the spread to bound it; turning
them into an assertion once there are numbers is the obvious next move. The CPU half of
the measurement — that the DFT reports a planted amplitude correctly and doesn't leak
between bins — is in `cargo test`, since a plausible-looking table that says nothing
true is the failure worth guarding against.

## The three GPU passes

`Fsr::process()` in `src/fsr/mod.rs` is the single entry point for all three. Each is
skipped, and its textures never allocated, when it would be a no-op. The header
comment in each `.wgsl` is thorough; open the file rather than guessing.

- `src/fsr/upscale.wgsl` — **FSR1 EASU**, a port of `FsrEasuF`. Skipped at scale 1.0.
- `src/fsr/sharpen.wgsl` — **FSR1 RCAS**, a port of `FsrRcasF`. Exact identity at
  sharpness 0.0. Its `FSR_RCAS_DENOISE` term is deliberately off; the header explains
  why and quotes the measurement. Don't re-enable it without measuring again.
  **Sharpness 1.0 is the ceiling, not a soft cap** — it's the reference filter at
  0 stops, and a lobe past the limiter inverts the filter rather than sharpening
  harder (`4*lobe + 1` goes negative). More apparent sharpening has to come from the
  tap distance (`rcas_radius`), not the lobe.
- `src/fsr/contrast.wgsl` — **not FSR.** A difference-of-Gaussians band-pass on luma
  ("clarity"), computed on a 1/8 plane. Exact identity at clarity 0.0. The knob is
  **signed**, -1..1: negative subtracts the band and softens.

## The image-to-image tab

`Engine::img2img` (`src/pipeline.rs`) is `run` with the txt2img replaced by a source
image, and `open`'s copy-in rule bolted to the front. Three things about it.

**The source is a `work/output/` stem, never a path**, resolved by
`Engine::output_source` — the same helper `Engine::video` uses for a first frame, and
the same contract: the client picks from what `GET /outputs` published, and
`workitems::valid_id` is the whitelist that makes that safe. One helper rather than
two copies of the check.

**It is copied into scratch, never adopted.** That file belongs to the user, and a
`Pending` pointing at it would have `remove_files()` delete it on the first refine —
invariant 8's failure in a new place. The copy is `{id}-0-src.png` under `Scratch`, so
a failed pass takes it with it.

**`fit_to` only ever shrinks, and only the width is snapped to 64.** Snapping both
axes was the obvious thing and is wrong: 5000×2000 into a 1536 budget gives 1536×576
rather than 1536×614, a 6.7% change of aspect that nothing downstream reports. One
snapped axis is also what `hires` does, where a single scale factor drives both. A
source already inside the budget comes back untouched, which is what lets the caller
`fs::copy` the bytes instead of resampling them — what feeds a diffusion pass should
not already be a generation of loss. The size ceiling is not politeness: a phone photo
is 4032×3024 and a pass at that size is an OOM on a 24 GB card.

Uploads (`POST /upload`) land in `work/output/` through `Store::import`, which is why
the tab needed no new listing, thumbnail or picker route — that directory is already
*the* pool of images you can start from, and an import is animatable as well as
diffusable. The stem is minted (`new_id`), never taken from the filename: `valid_id`
is alphanumerics and `-`, so almost no real filename is even expressible, and
generating the name is what keeps the guard a whitelist rather than a sanitizer.

The body is **raw PNG bytes**, not base64 in JSON — `Request.body` is a `Vec<u8>` that
is never UTF-8 validated, so this costs nothing and saves a third of the wire.
`http.rs` picks the limit per path (`body_limit`, an exact match — a prefix would hand
32 MiB to anything starting with `/upload`), and over-limit now *answers* 413 instead
of bailing, which used to close the socket with no response at all. **PNG only in
Rust, on purpose**: the page re-encodes through a `<canvas>` first, so JPEG/WebP/AVIF
all work and `image` stays compiled `features = ["png"]`.

## The two image panes

`web/index.html` has two tabs that put an image on screen, and everything around the
image — the action row, Save, the viewer — is one set of functions pointed at whichever
is showing. That is `PANES` and `P(k)`.

The module state (`imageId`, `currentSrc`, `workId`) stays **single**, because only one
tab is visible and the worker is serialized, so there is only ever one live result to
have; two copies would be two ways to be stale. The price is that `showTab` must
`clearPane` the tab it leaves — otherwise a picture sits there looking live beside
Refine and Hi-res buttons that now address a different image, which would work and
would be wrong. It also means the ids `render()` writes (`refine`, `hires`, `saveout`,
`upfactor`, `viewadjust`) exist in at most one pane at a time, which is why
`IDLE_LABELS` and `setUpFactor` need no second set of them.

A pane is down to three keys — `result`, `err`, `escmodel` — and the two it lost say
what the viewer bought. The sliders and their timing readout used to be duplicated per
pane; there is one dialog, so there is now one pair, addressed by plain id. `setPost`,
`syncPostLabels`, `runPost` and `setBusy` reach them with `$()` rather than `P()`, and
adding a third slider is a change in one place rather than two.

## The viewer

`<dialog id="lightbox">` is where post-processing happens, and that is a correction
rather than a move. Sharpness is RCAS — a filter over single pixels — and clarity is a
band whose width is proportional to the image (invariant 6). Both were being judged on
the result column's fitted preview, a few hundred pixels wide, which is why the feature
read as broken while `/adjust` was doing exactly what it claimed.

Three decisions in it are load-bearing:

- **Closing is the `×` and Escape, nothing else.** The old viewer dismissed on any
  click, which is right for something you only look at and hostile for something you
  edit in: one stray click on the picture and the picture is gone. Don't put the
  click-to-close listener back.
- **Fit and 1:1 are both needed**, because the two filters ask different questions — a
  look across the whole frame, and a pixel. 1:1 centres its scroll (`centreView`)
  rather than landing in a corner, and the centring runs twice: once inline and once on
  the image's `load`. A fresh `data:` URL has no dimensions to measure when
  `showFullSize` returns, and re-opening on the src already loaded fires no `load` at
  all, so neither half alone is enough. `lbCentre` is the flag that keeps a *re-filter*
  from re-centring — the whole point of panning to a detail is that dragging Sharpness
  leaves it under the cursor.
- **`setResultImage` writes both copies.** The image is on screen twice while the
  dialog is open, and `render()` keeps the big one in step too: a modal dialog stops
  clicks, not a batch run that was already in flight.

Panning is scroll offsets, not a transform, so the wheel, a trackpad and the scrollbars
keep working with no code. `.lbview` needs `min-width: 0` — a flex item's default
minimum is its content, so a 3072 px image would otherwise refuse to shrink its pane
and shove the panel off the left edge — and the image centres with `margin: auto`
rather than by centring the container, which clips the overflowing left edge and makes
it unreachable by scrolling.

The panel is a fixed 300 px and scrolls on its own because it is a stack that is
expected to grow; new sliders go in it, not under the image.

## Invariants

These break silently rather than loudly.

1. **Nothing ever diffuses over sharpened pixels.** `Pending.raw` (`src/pipeline.rs`)
   is the unsharpened output-resolution image; every escalation reads it, and RCAS is
   applied exactly once, in `Engine::publish()`. A new pass that read `display`
   instead would compound sharpening along the whole ladder. `Engine::adjust` — the
   live sharpness/clarity sliders — rests on this directly: re-filtering `raw` is
   lossless however many times a drag does it. Break the invariant and the sliders
   silently become destructive.
2. **sd.cpp *trims* an img2img schedule by strength, it doesn't scale it** — it keeps
   the last `steps × strength` of a `steps`-long schedule. So `SdRunner::img2img`
   (`src/sd.rs:102`) hands it `steps / strength`. Drop that and `--steps 2
   --strength 0.3` gives `t_enc = 0`: the image comes back untouched.
3. **`MAX_STRENGTH` is 0.95, not 1.0** (`src/config.rs:227`). sd.cpp only trims
   `if (strength < 1.f)`, so at exactly 1.0 the init image is discarded and the pass
   degenerates into a plain txt2img.
4. **`*_steps` is a floor, not an exact count.** Raised to `profile.steps × strength`
   in `img2img`, because strength decides how far up the sigma schedule the pass
   starts. Under-stepping a high-strength pass leaves un-removed noise, which reads as
   grit.
5. **Dimensions come back from `publish()`; they don't carry over.** sd.cpp rounds an
   init image to what the latent grid allows, so a pass can return a size it wasn't
   given. `snap64()` exists for the same reason.
6. **Both display filters size their band from the image, not the pixel grid.**
   `clarity_sigmas` and `rcas_radius` (`src/fsr/mod.rs`) are both proportional to the
   longest side, so a setting survives a hi-res pass. A fixed-footprint kernel silently
   grades a finer and finer band as the image grows — which is a look-wrong bug, not a
   crash, and it was only caught by someone saying sharpening "felt lost" at 3072.
   `fsr-selftest` measures this now, and `check_bands` asserts it for clarity, which
   holds to within a few percent across the range. **RCAS holds it in steps, not
   smoothly**, and is deliberately excluded from that assertion: `rcas_radius` is a whole
   number of pixels, so it is exact where a size ratio matches a step and drifts between
   them — measured, the fine band lifts 3.245× at 1536², 2.481× at 2048² (same radius,
   24% weaker) and 3.245× again at 3072². Don't read the equal numbers at 1536 and 3072
   as proof: those sizes double the radius along with the image, so they agree by
   arithmetic. Compare sizes *within* a plateau (768..2303, or 2304..3839) to see real
   behaviour. Fixing the sawtooth means fractional taps, which trades away the limiter
   guarantee — see `sharpen.wgsl`'s header before going there.
7. **`hires_max_side` gates the hi-res pass but deliberately *not* refine.** Hi-res
   can fall back to diffusing smaller and letting FSR cover the rest; refine has no
   such fallback, so gating it would just remove the button in the state where it
   helps most. `Pending::can_refine` documents this — it is not a bug to fix.
8. **`work/workitems/` is never referenced by a live `Pending`.** Every generation is
   archived there (`src/workitems.rs`); `Engine::run` *copies* `raw` in and
   `Engine::open` *copies* it back out into the usual `{id}-0-raw.png` scratch name. A
   `Pending` pointing straight at the archive would have its entry deleted by
   `Pending::remove_files` the first time it was refined, or by `Scratch::drop` if a
   pass failed — the feature quietly destroying the thing it exists to keep. One extra
   PNG write per generate (~15 ms at 1536²) is the price of the whole existing scratch
   lifecycle staying untouched.

   Which is why `purge_pngs` and `MAX_PENDING` are **unchanged**, despite reading like
   the cleanup to remove. Both only reach `work/` itself — `purge_pngs` is a
   non-recursive `read_dir` filtered on a `.png` extension, so it skips directories —
   and both still bound the scratch dir. What changed is that eviction is now harmless:
   an evicted image is reopenable rather than lost, so the 8-entry cache is a limit on
   what is already warm, not on what exists. `Engine::lookup`'s error says so.

   `Pending.work_id` is an id, never a path, for the same reason. It exists only to
   name saves (`work/output/{work_id}-s{step}.png`) and to let the UI mark which
   thumbnail is on screen. **`Pending.source` is a stem for the same reason again** —
   it names an image in `work/output/` that the user either saved or uploaded, and
   `Engine::img2img` copies it into scratch precisely so `remove_files` can't reach
   it. The one rule: nothing a `Pending` owns may live in a durable directory.

9. **Filter a clip's frames; never re-diffuse them.** `Engine::enlarge_clip` runs
   `Fsr::process` over each frame and that is safe *by construction*: EASU, RCAS and
   clarity are deterministic functions of the pixels, so two near-identical frames
   produce two near-identical outputs and a fixed kernel cannot flicker. Invariant 6
   covers the size change with it — both filters size their band from the image, so a
   sharpness setting survives the enlargement.

   Running the existing `refine` per frame would compile, run, and look plausible on any
   single frame. Every frame would draw its own noise and invent its own detail with no
   knowledge of its neighbours; adjacent frames diverge structurally and the eye
   integrates that as boiling or crawling texture. It is the signature artifact of
   per-frame SD upscaling and it is only visible *in motion*, which is why it belongs
   here rather than in a code review. Refining a clip properly is video-native: a second
   `-M vid_gen` pass with a VACE checkpoint and `--control-video`, which takes a
   directory of frames — precisely what pass 1 wrote, so no decode and no ffmpeg. That
   is a `family` entry and a second `SdRunner::video` call, not this function with a
   sampler bolted on.

   `work/videos/` is also the one archive written *through* rather than copied into,
   which looks like invariant 8 and isn't. That invariant exists because a live
   `Pending` deletes its own scratch files; a clip has no scratch stage, being written
   once by one pass and afterwards only read. What it does need is the failure half, and
   that is `videos::ClipDir` — a guard that sweeps a half-written directory unless
   `keep()` is reached. Cancelling is the *common* path here, not the rare one.

## Adding a per-model knob

The most repeated change here. Follow `img2img_cfg_scale` end to end for a *generation*
knob, or `sharpness`/`clarity` for a *display* one — the two are not interchangeable, see
the note below the table.

| # | file | what |
|---|------|------|
| 1 | `src/config.rs` | field on **both** `Defaults` and `Template`, then on `Profile`, then wired in **both** arms of `Config::registry()` |
| 2 | `src/config.rs` | range-check in `Profile::validate()` |
| 3 | `src/sd.rs` | consume it in `base_cmd` / `txt2img` / `img2img` |
| 4 | `src/pipeline.rs` | `Option<T>` on `GenRequest`, applied onto the profile in `Engine::run()` **before** `profile.validate()` — and, for an *escalation* knob, on `EscalateRequest` + its `apply()` as well |
| 5 | `src/main.rs` | field on the `/generate` `Dto`, plus `build_models_json` so the UI can pre-fill it — and, for a *display* knob, the `/adjust` `Dto` and `result_json` too; for an escalation knob, `EscalateDto` and `overrides()` |
| 6 | `web/index.html` | input in the Advanced panel; `syncAdvanced()` and `applyWorkItem()` pre-fill it, `generate()` reads it — an escalation knob goes in `escalateSettings()`, which `generate()` spreads, so it is read once and sent by all three routes |
| 7 | `models.toml` + `README.md` | commented default, and a row in the field table |

The trap: **`Defaults` and `Template` are two separate structs with the same field
list**, and `registry()` builds each in its own arm. Adding to one and not the other
compiles cleanly and silently ignores the knob for half the models.

**A *video* knob is a shorter path and the trap doesn't apply**: there is one struct,
`VideoModel`, because a `[[video]]` entry has no `[defaults]`/`[templates]` split to keep
in step. Video *is* discovered, but an explicit entry replaces a discovered model rather
than merging with it, so there is still only one place a field lives. Add the field there
(with `#[serde(default)]` unless it is genuinely required), give it a value in
`VideoModel::discovered()` — a struct literal, so forgetting it is a compile error rather
than a knob that silently only works for half the models — range-check it in
`VideoModel::validate()`, consume it in `SdRunner::video_cmd`, add an `Option<T>` to
`VideoRequest` applied onto the clone in `Engine::video`, a field on the `/video` `Dto`,
and an input plus a line in `syncVideo()`/`applyClip()`. A knob whose right value depends
on the architecture belongs in `VideoFamily::recipe()` rather than in a `default_*`
function. Anything the page must *derive* from `family` goes on `VideoModelDto` instead
of being reimplemented in JavaScript — see `frame_grid` below.

A knob that *defaults to another knob* stays `Option<T>` all the way onto `Profile` and
resolves through a method — `img2img_cfg_scale` / `Profile::img2img_cfg()`. Resolving it
to a number at load, or pinning it in `Engine::run` when the request omits it, both
compile and both quietly sever the link: raising `cfg_scale` would leave the escalations
at whatever the base CFG used to be. For the same reason `build_models_json` emits `null`
rather than the resolved value — the UI has to be able to tell "unset" from "set to the
same number", because they diverge the moment the other box moves.

Per-request overrides are applied onto a *cloned* profile, which is then retained in
`Pending` — so a later refine/hi-res uses the settings the image was generated with,
not whatever the UI shows now. Keep that property. It is what makes a generation
reproducible from its sidecar, and it is the only reason `open` can restore an image
faithfully.

The retained profile is the **starting point** for an escalation, though, not a pin:
`/refine` and `/hires` take an `EscalateRequest` (`src/pipeline.rs`) and fold it on top
before the pass runs. Only the knobs that describe a pass which *hasn't happened yet* —
`refine_steps`, `refine_strength`, `hires_steps`, `hires_strength`,
`img2img_cfg_scale`, and **`template`**. `steps`, `cfg_scale`, `sampler` and the base
size are settings of a txt2img that already ran, so there is nothing a new value could
do to the image in hand; they stay retained, and the next *generate* picks them up. Get
that boundary wrong in either direction and the Advanced panel starts lying: honour a
base knob and the image claims settings it wasn't made with, ignore an escalation knob
and pressing the button after editing the box quietly does the old thing.

`template` passes that test and is worth stating separately, because it looks like a
base setting and isn't: a *checkpoint* is a property of the pass about to run. It is
applied by `Engine::swap_model`, not by `EscalateRequest::apply`, because resolving a
name needs the registry — and it must run **before** `apply`, since the swap replaces
the whole profile and would otherwise discard the boxes folded onto it.

The swap is **wholesale**, and that is the decision, not an oversight. The named
model brings its own `refine_*`, `hires_*`, `img2img_cfg_scale`, `hires_max_side`,
`weight_type`, `extra_args` and slider start points, and the request's explicit knobs
land on top — which is consistent, because `syncAdvanced()` already refills those
boxes from whatever model is selected. A field-wise merge is the `Defaults`/`Template`
trap from the top of this file wearing a hat: every knob would have to become an
`Option` so "unset" could be told from "set to the default". The replaced profile is
what the new `Pending` retains, so `can_refine`, `can_hires` and where the sliders
reset all describe the model that actually ran — which is what keeps a saved sidecar
true, and also means a swap can remove the Hi-res button (new model, `hires_steps = 0`)
or declare the image past budget (smaller `hires_max_side`).

On the wire the field is **`model`, not `template`**, and that is load-bearing:
`genPayload()` spreads `escalateSettings()` into the `/generate` body, where
`template` is the model doing the txt2img. A second field by that name would silently
clobber it — `genPayload` destructures `model` back out, and
`an_escalation_may_name_a_different_checkpoint` pins both halves.

`EscalateRequest.img2img_cfg_scale` is `Option<Option<f32>>` for the reason the
"defaults to another knob" paragraph above gives: absent = keep, `null` = unset it so
the pass follows `cfg_scale` again, a
number = pin. Serde collapses `null` into `None`, so the DTO needs
`deserialize_with = "double_option"` — without it an emptied box can never be cleared,
only overwritten. `escalate_body_tells_an_empty_cfg_box_from_a_missing_one` guards both
that and the `#[serde(flatten)]` on the hi-res body, and needs no GPU.

The two **display** filters are the deliberate exception, and the distinction is easy
to collapse by accident. `profile.sharpness`/`.clarity` are where the sliders *start*;
`Pending.sharpness`/`.clarity` are where they are *now*. `adjust` writes the second
pair only, and `run`/`refine`/`hires` all reset it from the first — so every new image
arrives unfiltered. Putting the live value back on the profile would compile fine and
silently restore the old inherit-forever behaviour.

## Conventions

- Comments explain *why*, usually with a measured number (flash attention is 6× at
  2048²; clarity costs ~1 ms at 3072²). That density is the house style, not excess —
  match it, and measure before claiming.
- `anyhow` throughout. Errors are user-facing strings with the fix in them; see
  `SdRunner::preflight` and `Engine::lookup`.
- std-only HTTP by design (`src/http.rs`). Don't reach for a web framework.
  - `http.rs` **strips the query string before routing**, so an id has to be a path
    segment (`GET /workitems/{id}/thumb.png`, a guard arm in `handle_conn`) or a POST
    body. `workitems::valid_id` is the traversal guard and is a *whitelist* — digits and
    `-`, so `.` and `/` aren't in the alphabet at all. Widen it rather than replacing it
    with `canonicalize`.
  - `Job.reply` carries the **finished JSON body**, not a `GenResult` — not every job
    produces an image (`SaveOutput` answers with a path). The worker renders it; that's
    why `result_json` is called in the worker match rather than in `submit`.
  - Routes that only touch the filesystem (`/workitems` list, the thumbnail,
    `/workitems/delete`) are answered **on the connection thread**, not through the job
    queue. The worker is serialized, so queueing a thumbnail behind a diffusion would
    freeze the grid for the length of the render. The worker only ever *writes* to that
    directory, so a listing that races a write just misses the newest item.
  - `/progress` and `/cancel` are on the connection thread for a sharper version of the
    same reason: they are *about* the job the worker is running. Through the queue, a
    progress bar would first update once the render it reports on had finished, and Stop
    would become pressable only when there was nothing left to stop.
  - The video routes split the same way. `/video` and `/videos/enlarge` are GPU work and
    go through the queue; `/video-models`, `/videos`, `/videos/{id}/thumb.png`,
    `/videos/{id}/frames/{n}.png`, `/videos/delete`, `/outputs` and
    `/outputs/{stem}/thumb.png` are filesystem-only and answer on the connection thread.
    That matters more here than it does for a still: a clip holds the worker for
    *minutes*, and the frame player fetching 33 PNGs is exactly what must not queue
    behind it.
  - `/outputs/{stem}/thumb.png` is the one thumbnail route that *generates*. Output PNGs
    are full resolution (up to 3072²) and were written long before there were
    thumbnails, so `ensure_output_thumb` makes one on first request and caches it. It
    writes to a counter-suffixed temp name and renames — two requests for the same tile
    are two threads of *this* process, so a pid in the name would have given them the
    same file.
- **`SdRunner::run` streams the child's output; it must not go back to
  `Command::output()`.** Three things about that are load-bearing and all three break
  silently:
  - Progress is on **stdout**, not stderr. sd.cpp sends `SD_LOG_ERROR` to stderr and
    everything else — including the bars — to stdout (`examples/common/log.cpp`,
    `out_stream`). Both are read anyway, since a failure message wants the informational
    tail too, and stderr gets its own thread because a pipe nobody drains fills up and
    deadlocks the child mid-render.
  - `drain` splits on `\r` **and** `\n`, which is why it isn't `BufRead::lines`. sd.cpp
    ends each bar with a carriage return so it can overwrite it in place, and only the
    final step of a pass gets a newline — read by lines, a 20-step render delivers its
    whole progress history in one blob after the last step, which is exactly when it
    stops being progress.
  - `Monitor` parks the `Child` in a `Mutex` for the length of the run so `cancel` can
    reach it, and `run` takes it back out *before* `wait`. Waiting while holding that
    lock would hold it against the very cancel it is waiting for.
  - `rate` is sd.cpp's own text, kept verbatim. It already picks its unit (`s/it` below
    one iteration per second, `it/s` above; `MB/s`/`GB/s` while loading weights), and
    `Phase` is derived from that suffix. Parsing it into a number means a second
    implementation of the same decision, free to disagree with what the terminal says.
- JSON out is hand-rolled `format!` for the two long-standing shapes (`result_json`,
  `build_models_json`) and `serde_json` for anything backed by a real struct (work item
  sidecars, `GET /workitems`). `serde_json` was already a dependency for parsing; don't
  hand-roll a 22-field round-trip, and don't churn the two that exist.
- **Batch and loop generation are a client loop, not a job.** The Image-to-image tab
  is deliberately single-shot and has no run panel: the run panel's value is drawing
  N *different* pictures from one prompt, and with a fixed source image the only
  variation left is the seed. `startRun()` in
  `web/index.html` calls `/generate`, then `/hires`, `/refine` and `/save-output` per
  image; there is no batch route and there shouldn't be. The worker is a single blocking
  consumer (`for job in job_rx`), so one job that owned it for a whole run would leave
  `/adjust`, the grid and every other client dead for the length of it, and cancelling it
  would need a second channel into that loop. Looping over the existing routes costs one
  round trip per image against a multi-second diffusion, and is what makes each result
  appear as it lands and Stop only have to wait for the image in flight. The stated cost
  is in `README.md`: the loop lives in the page, so closing the tab ends it after the
  current image.
  - The automated passes go through the same `escalateSettings()` the buttons use, and
    the boxes are read once per image rather than captured at the start. A run must not
    grow its own copy of the knob list — that is how the panel starts lying again.
  - `#stop` is deliberately **not** in `IDLE_LABELS`, which is why `setBusy` never
    disables it. A stop button that greys out for the length of the thing it stops is
    furniture.
- **Video models are discovered by reading their headers, never their filenames**
  (`src/discover.rs`). A checkpoint is one file, so the image side can identify it by
  dropping it in the models dir. A video model is three to five files — transformer, VAE,
  text encoder, sometimes a *second* transformer — whose roles are not in their names, so
  the scan opens each one instead: safetensors keeps a JSON tensor index behind a `u64`
  length, GGUF a typed key/value block, and the tensor names in it name the architecture.
  One seek and a few hundred KB against an 8-20 GB file.

  The discriminators are lifted from sd.cpp's own `ModelLoader::get_sd_version`
  (`src/model_loader.cpp:495-605`), because the only classification worth having is the
  one the binary we shell out to will agree with. **Read shapes off square tensors only**
  (`Tensors::square_dim`): safetensors stores a shape in PyTorch order and GGUF stores it
  reversed, so a rectangular tensor means picking an end per format and being wrong in
  one of them. A model's width is in any attention projection, which is `[dim, dim]`.

  The case that justifies the whole approach is in the models dir: a file named
  `minimax_h3Comfy.safetensors` is a Qwen3-VL-32B *text encoder*. Filename matching would
  have slotted 15 GB in as a transformer and failed minutes into a run.

  - **`[[video]]` did not go away, it changed job.** It names files that aren't
    downloaded yet (so the listing stays a checklist), pins measured settings, and
    settles the one question no header can answer. An entry **replaces** a discovered
    model rather than patching it, matching on `name` or on naming the same
    `diffusion_model`. Replacing wholesale is what keeps `VideoModel` a single struct — a
    field-wise merge would need every knob to be `Option` so "unset" could be told from
    "set to the default", which is the `Defaults`/`Template` trap from the top of this
    file. What an entry omits comes from `VideoFamily::recipe`, not from the discovered
    copy.
  - **Which half of a MoE pair a file is cannot be read from it.** The two experts have
    identical tensor names and identical shapes and differ only in their weights, so
    `noise_marker` falls back to the filename — the only place the information exists.
    When the filename says nothing either, the file becomes a `wan_dense` model that runs
    alone, **not** a guess. That variant exists for exactly this: repackagers ship single
    14B files with no marker, sd.cpp runs a lone expert quite happily, and the
    alternatives were to guess which half it was or to refuse to list a runnable file.
  - A missing component is not an error at startup: a half-downloaded 20 GB model is the
    normal state of one for an hour, so `doctor` and the model select list it greyed out
    *with the filename it is waiting for*. Discovery keeps that property — a model whose
    VAE isn't there yet points at the conventional name **inside the role directory it
    belongs in**, so the path printed is also an instruction. Download one expert of a
    pair and it names the other one exactly.
  - The role subdirectories (`diffusion_models/`, `vae/`, `text_encoders/`, `loras/`,
    `clip_vision/`) are ComfyUI's layout, which is what every download instruction
    assumes. **The scan is recursive, so they are for the human, not the program** — a
    flat folder discovers exactly the same models. They exist because the encoder is
    *shared*: one umt5 serves every Wan family, so a folder per model would mean copying
    7 GB per model or a tree of symlinks.
  - **`family` is the load-bearing field.** It decides which flags `video_cmd` emits
    (the A14B pair needs a whole second `--high-noise-*` set; H3 needs `--llm` and
    `--audio-vae`), which grid sizes and frame counts round to, and whether fps is the
    user's to choose. Keeping it explicit from day one is what makes a new architecture a
    few lines of config rather than a refactor — `minimax_h3` is already a variant, and
    is unrunnable here only on memory grounds.
  - **The two grids round in opposite directions**, which is why `align_frames` isn't one
    formula. Wan goes through `((f - 1) / 4) + 1` latent frames and back, and integer
    division truncates, so 34 frames runs as 33 — *down* to `4k+1`. H3 has its own
    branch, `while (frames % 17 != 5) ++frames` — *up* to `17k+5`, floor 5. Sizes always
    round **up**, to `vae_scale_factor × 2`: 32 for the TI2V-5B and H3, 16 for every
    other Wan. All of it is in sd.cpp's `src/pipeline/diffusion_engine.cpp` and
    `src/pipeline/request.cpp`; sd.cpp aligns and *warns*, so getting the direction wrong
    here doesn't fail, it just makes the sidecar describe a clip that doesn't exist.
  - The page is handed those rules as data (`VideoModelDto::frame_grid`,
    `size_multiple`, `forced_fps`) rather than deriving them from `family`. It rounds the
    frame count to show a duration and `video_cmd` rounds it to run the model; two
    implementations of `4k+1` that disagree by one frame is exactly how the box above the
    button starts lying. `the_frame_grid_describes_the_rounding` takes `align_frames` as
    the truth and asserts the published grid explains every answer it gives.
  - **A clip is a PNG sequence, not a container**, and `-o` cannot be both — a format
    specifier in the output path takes an early branch in sd-cli that writes one file per
    frame, and without one it muxes a single `.avi`/`.webm`/`.webp`. Frames win because
    of what happens next: both escalations consume frames (`Fsr::process` per frame,
    `--control-video` for a VACE pass) and this binary has no video decoder to get them
    back out of a container — `image` is compiled `features = ["png"]` and sd-cli's muxer
    is internal to sd-cli, with no mode that runs it over existing images. A container
    would mean regenerating the clip, at minutes a go, to do anything with it. Frames are
    also lossless, which is invariant 1's argument again: what gets filtered should not
    already be compression artifacts. The price is disk, ~700 KB a frame at 832×480, so
    ~23 MB for a 33-frame clip. Playback is therefore a `<canvas>` and a timer rather
    than a `<video>`, which also buys frame-exact scrubbing.
  - The video Stop is `POST /cancel` and **kills the child**, unlike the image tab's
    `#stop`, which lets the image in flight finish and then ends the run. There is only
    ever one clip in flight, so stopping it can only mean killing it. A cancelled run's
    error ends in `sd::CANCELLED`, which the page reports as "Stopped" rather than as a
    wall of sd-cli stderr.
- **A file's weight type on disk is not what it costs to run**, which is why there is a
  Convert tab (`src/convert.rs`, `-M convert`). `Linear::forward`
  (`model/common/ggml_block.hpp:211`) asks the backend whether it can matmul the stored
  type and, when it can't, puts a `ggml_cast` in the *compute graph* for every linear
  layer — so an fp8 checkpoint on a Vulkan device (all of which report no fp8) executes
  as BF16 and needs about twice its file size. `q4_K` is not a downgrade from fp8 there;
  it is the first type in the list the backend can actually multiply.
  - **The component flag renames the tensors.** `convert_with_components`
    (`sd.cpp src/convert.cpp:354`) takes a path per slot and prefixes every tensor name
    by which slot it arrived in: `--diffusion-model` → `model.diffusion_model.`, `--vae`
    → `vae.`, `--t5xxl` → `text_encoders.t5xxl.transformer.`, `-m` → nothing. The wrong
    flag writes a *valid* file full of misnamed tensors that fails minutes into a later
    run. `Slot::for_role` is that mapping and it is derived from the discovered role, so
    the client never picks it. Roles with no slot at all (`QwenVl` needs `--llm`,
    `ClipVision` needs `--clip_vision`, neither being a parameter of that function) are
    left off the list rather than squeezed through a neighbouring flag. Prefixing is
    idempotent (`model_loader.cpp:292`), so a converted file round-trips.
  - **`-t 1` is load-bearing, not caution.** sd.cpp's streaming converter runs a worker
    per core, all calling `ModelLoader::load_tensor` on one shared loader
    (`convert.cpp:244`), which re-enters `load_tensors` → `process_model_files` and
    mutates that loader's `file_data` unlocked. At the default it aborts in glibc on
    roughly one run in three. The crash is survivable — `Engine::convert` removes the
    partial file — but the race is in the loader's bookkeeping *while other threads read
    through the mappings it is remaking*, so it can also write wrong bytes and exit
    cleanly. Measured cost of serializing: 254 MB in 2.07 s against 0.53 s at four
    threads. Revisit if upstream takes a lock.
  - **A shape does not always survive a conversion, and the VAE is where that bit.**
    GGUF has four dimensions, so exporting a 5-D Conv3d weight folds the trailing axes:
    `conv2.weight` `[16, 16, 1, 1, 1]` comes back `[1, 1, 1, 256]`, element count kept
    and the width gone. So the Wan VAE's `z_dim` is read off `conv2.bias` — one
    dimension, therefore immune to both that and the PyTorch/ggml order flip — with
    `square_dim` on the weight kept only as a fallback. `Tensors::vector_len` is for
    exactly this, and `a_vae_survives_the_trip_through_gguf` is the regression. Biases
    are never quantized (`model_loader.cpp:1517`), so they are always there to read.
  - The size shown before the button is `estimate`, which transcribes
    `ModelLoader::tensor_should_be_converted` (`model_loader.cpp:1509`) rather than
    assuming everything is quantized — a transformer keeps every bias, norm scale and
    embedding at full width, which is enough to matter when the question is "does this
    fit in 24 GB". It lands within 0.01% of a real conversion. The **type table is
    served**, not written into the page, for the reason `frame_grid` is: a second copy in
    JavaScript is free to disagree with the one the CLI validates against.
- No Python anywhere in build or runtime. That is the project's whole pitch.
