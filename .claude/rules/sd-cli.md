---
paths:
  - "src/sd.rs"
  - "web/js/memory.js"
  - "web/js/help.js"
---

# Driving sd-cli: memory, preview, caching, samplers

## Memory

`Profile::memory` (`config::MEMORY_MODES`: `auto`, `ram`, `disk`),
`VideoModel::memory_mode`, `config::memory_args`; typed `sd::OutOfMemory`;
`web/js/memory.js`. A generation knob, and on `EscalateRequest`, `EditRequest`,
`RedrawRequest`, `VideoRequest` (it describes how the next pass fits). sd.cpp's
`--auto-fit` is default (`auto`); `ram` = weights in RAM, whole card for compute;
`disk` streams.

Break silently:

- **Any explicit `--params-backend` turns auto-fit off**, and `--offload-to-cpu` is
  one. `auto` = no flag; never emit the alias.
- **A placement in `extra_args` wins** (`memory_args` returns nothing;
  `memory_modes_are_one_params_backend_or_none`).
- **`[[video]]`'s old `offload_to_cpu` is read, never written**; `memory_mode()`
  decides; the page gets `memory_mode`.
- **The error is typed.** `run_tail` returns `OutOfMemory` (kind, phase, step,
  placement, mode); `oom_json` finds it through `.context()`; rides as `oom` on a
  `Notice` / `error_body`. System OOM is a SIGKILL — `killed_by_system` checks *after*
  the cancel check (`running_out_of_memory_is_a_typed_error_with_its_phase`).
- **`ram` is `*=cpu,te=disk`** (encoder released after use). `run_tail` reads the mode
  off the default part.
- **Phase is `run_tail`'s own last progress** (`end()` clears the monitor first). On
  decode sd.cpp already tiled, so only a smaller size is offered (a pass in the
  dialog: *Scale down to 0.75× and try again*). `oom_phase` reads
  failure lines before progress bars.
- **Load and stage times are parsed** (`parse_load_ms` → `Timings::load_ms`,
  `parse_stage` → `sd::Stages`), reset by `begin`, read straight after the pass. Don't
  read "generating N latent images"/"hires fix" (double counting). Loading is ~15% of
  a run — why a persistent `sd-server` is shelved.
- **Placement is only set by a run with `-p`** (an ESRGAN run mustn't wipe it); lands
  on `Timings::weights` (so `Timings` isn't `Copy`).

**System RAM is offered only when it fits**: `ram_needed` (`ram_mode_bytes`) vs
`ram_free` (`MemAvailable` after the child exits); `ramFits` keeps sd.cpp's headroom
(max of 2 GiB, 10%). **Its real limit is the GTT**: RAM-resident params are pinned host
memory amdgpu caps at the GTT (half of RAM by default). `sd::gtt()` reads
`mem_info_gtt_*`; `gttShort` greys System RAM out and puts Stream from SSD first.
`memory.js` keeps `options`/`ramFits` pure for `memory.test.js`.

**"Device lost" is a reserve, not a mode**: `Settings::gpu_reserve_gb` →
`config::gpu_reserve_args` → `--max-vram -N` on every run (page-level, `PAGE_ONLY`).
The box offers a 3 GB reserve on a lost device and **saves it at once**, then calls
`adoptSaved` so Settings' snapshot moves. **Offered once, never stepped up.**
`parse_shortfall` reads the need/budget line (by label); `budgetStarved` offers *Stop
keeping the card free*; `parse_encoder_on_card` and `-r` count are named in the box.

**Reopening does not restore Memory** (it describes this machine now, not the picture).

Page: `showError(el, msg, oom, opts)` is every sink. Buttons set a control; *Try again*
is the caller's `retry`. `post()` puts `e.oom` on the error.

## Live preview

`--preview proj` on every txt2img/img2img (`base_cmd`) into `work/current/preview/`,
served as `GET /preview/{n}.png`. `Monitor::with_preview` holds the dir. Video too,
in its own shape (below).

- **Numbered files plus an `IEND` test** (sd-cli writes in place). `newest_complete`
  takes the newest complete `p-NNNN.png` and prunes older ones only.
- **The dir is emptied in `run_tail` before spawn**, not in `Monitor::begin`.
- **`Monitor::preview` is `None` whenever `progress` is.** The page swaps a blob only
  once fetched.
- **Video is one file, `preview.avi`** (`VIDEO_PREVIEW`; sd-cli fills no `%d` for a
  video preview and rewrites it in place each step). `Monitor::video_preview` parses
  it per (length, mtime) with `avi::read`, keeps the last whole version when a read
  lands mid-write, and bumps `seq`; `/jobs` `progress.video_preview {seq, frames,
  fps}`, frames at `/preview/video/{seq}/{k}.jpg` (the AVI's own JPEGs). The page
  loops them on `#vlivepreview` (`drawVideoPreview`), swapping only whole versions.

## Step caching

`Profile::cache_mode` → `--cache-mode`, generate and source pass only. Off by default
until measured per family.

- **Wrong-architecture mode is a warning and an uncached render**, so values are
  `config::CACHE_MODES` (`off`, `spectrum`, `adaptive`); `cache_flag` maps `adaptive` to
  `ucache`/`easycache` by `ImageFamily::is_unet`. New modes must fit every family.
- **Refine/Hi-res run on `Profile::uncached()`** (a clone).
- **The skip line is the proof**: `parse_cache_skip`, read straight after the txt2img.
- **Video not wired yet.**

## Samplers, schedulers and the Help tab

`config::SAMPLERS`/`SCHEDULERS` transcribe sd-cli's tables; `check_sampling` refuses
anything else. The page's lists are `#sampler`/`#scheduler` markup
(`every_sampler_and_scheduler_option_is_one_sd_cpp_takes`). **Re-transcribe when sd.cpp
is bumped.**

**The clone is at `b13a482`** (bumped 2026-10-18; CI builds `SD_CPP_REF` in
`release.yml` — bump both). sd.cpp references here name
functions/files; line numbers drift — search by name. Log format is `[I] msg -
file:line`; parsers match contents and have fixtures in both formats; unmeasured
memory prints `unknown`/`unlimited` (hence `parse_shortfall` reads by label).

- **Flash attention and direct convolution are `auto` per module** upstream
  (benchmarked at load); `--diffusion-fa`/`--vae-conv-direct` are deprecated and
  warn. We emit **one** `--flash-attn auto,diffusion=on` (`DIFFUSION_FLASH_ATTN`,
  DiT `flags()`, big img2img on a UNet, video) and **one** `--conv-direct
  auto,vae=on|off` (`Settings::vae_conv_direct`) — sd-cli takes the last of each,
  so ours stays out when `extra_args` names either spelling (`names_flash_attn`).

- **Auto is the absence of `--scheduler`** (sd.cpp's `default_scheduler` picks per
  model/sampler).
- **`ays` only has SD 1.5/SDXL tables**; `check_sampling` takes the family.
- **`ltx2`, `logit_normal`, `llada_image` are left out** until their families exist.

Help tab (`help.js`): a `TOPICS` table (id, title, HTML, optional `fill()`). Tooltips
point to Help rather than repeating it.
