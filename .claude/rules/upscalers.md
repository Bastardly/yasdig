---
paths:
  - "src/sr/**/*"
---

# Upscalers

## Upscalers

The initial enlarge in `Engine::run` is EASU or a model (`Profile.upscaler`, `None` =
EASU, names under `Config::upscaler_dir`). An ordinary generation knob.
**`pipeline::enlarge` is a free function** (takes `&Fsr`, `&SdRunner`) so `upscale` runs
without an `Engine` (whose constructor sweeps `work/`).

**Two routes by file type** (`sr::identify`): ESRGAN → sd-cli; Compact, SPAN, SwinIR →
`src/sr/` on the FSR device. DAT and HAT are identified and refused.

sd.cpp `-M upscale` facts:

- **A model it can't build exits 0** and saves the input unchanged. `SdRunner::upscale`
  greps the stderr tail (`run_tail`) and `after_model` refuses a result that didn't grow.
- **RRDBNet only.**
- **Scale is in the weights**, read off the output size.

**Output is `fsr::out_size(input, factor)` whichever ran.** `after_model` shrinks an
overshoot with Lanczos3 and finishes a shortfall with EASU at `factor / scale`
(`an_esrgan_enlarge_lands_on_the_easu_size`).

`sr/weights.rs` reads safetensors and both torch formats (zip with stored storages, and
legacy) via a pickle subset. `list_upscalers` greys out what can't run;
`check_upscaler` refuses it early. Names go through `valid_lora_name`.

`src/sr/` breaks silently:

- **Legacy torch is recognised by all eight magic bytes** (a safetensors header length
  can start `0x80`).
- **Channels-last everywhere** (`[H·W, C]`); conv weights reordered at load to
  `[out][ky][kx][in]` (`gpu::conv_weight`).
- **Swin window partition/shift/mask are index maths in `attn.wgsl`**; `sr-selftest`
  checks against a literal transcription.
- **`Gpu::tensor` flushes before `write_buffer`** (a queued write overtakes unsubmitted
  commands).
- **Tile sized from the device**: `Fsr::new` asks for the adapter's storage-binding
  limit (capped 1 GiB); `plan_tile` shrinks until `Net::peak` fits.

Network check is **`sr::consistency`** (shrink back, compare; working > 30 dB). SPAN's
traps are in its header (`Conv3XC` folded from training weights; SPAB's inplace SiLU).

Page: `loadUpscalers` starts **before** `loadModels`' first `await` (Settings awaits
`upscalersReady`). `upscalerOptions` excludes "(missing)" entries.
