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
cargo test                                  # no GPU: naga-validates the 3 WGSL shaders + the band-measurement math
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
   thumbnail is on screen.

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
`img2img_cfg_scale`. `steps`, `cfg_scale`, `sampler` and the base size are settings of a
txt2img that already ran, so there is nothing a new value could do to the image in hand;
they stay retained, and the next *generate* picks them up. Get that boundary wrong in
either direction and the Advanced panel starts lying: honour a base knob and the image
claims settings it wasn't made with, ignore an escalation knob and pressing the button
after editing the box quietly does the old thing.

`EscalateRequest.img2img_cfg_scale` is `Option<Option<f32>>` for the reason two
paragraphs up: absent = keep, `null` = unset it so the pass follows `cfg_scale` again, a
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
- JSON out is hand-rolled `format!` for the two long-standing shapes (`result_json`,
  `build_models_json`) and `serde_json` for anything backed by a real struct (work item
  sidecars, `GET /workitems`). `serde_json` was already a dependency for parsing; don't
  hand-roll a 22-field round-trip, and don't churn the two that exist.
- **Batch and loop generation are a client loop, not a job.** `startRun()` in
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
- No Python anywhere in build or runtime. That is the project's whole pitch.
