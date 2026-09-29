# ~~Feature 1 - Split high-res and refine model~~ — DONE
Currently the same setting handles both high-res and refining. It would be nice to be able to split these up in separate models.
For the UI, we should probably have a line between both types. Also, each part should have their own CFG scale, we the default to the origina.

- side note. There is a big, where the refiner model doesn't get saved to the workflow. This would be nice, if this was fixed in this go.

**DONE.** The Advanced panel on both image tabs is now two groups with a rule
between them — *Hi-res pass* and *Refine pass*, hi-res first because that is the
order the buttons above a result are in — and each carries its own steps,
strength, CFG and checkpoint. The server was already per-pass: `/refine` and
`/hires` each build their own `EscalateRequest`, so the single picker only ever
existed because `escalateSettings()` built one object for both buttons. The
checkpoint has moved out of that object and onto each button's own request.

The two CFGs are real per-model settings, `refine_cfg_scale` and
`hires_cfg_scale`, resolved through `Profile::refine_cfg` / `hires_cfg`: the
pass's own knob, then `img2img_cfg_scale`, then `cfg_scale`. That middle rung is
"the default to the original" made literal — a settings file or a `[templates.*]`
that already pins the old shared *Refine/hi-res CFG* keeps running both
escalations at exactly that number. `img2img_cfg_scale` keeps its name and now
governs only the Image-to-image tab's own pass, which is what it has always been
called, so its box left the Image tab and its Settings row moved to the
*Image to image* section.

**The bug is fixed.** `refine_model` and `hires_model` are request-only fields on
`Profile`, exactly like `loras`: no `Defaults`/`Template` key, filled from the
`/generate` and `/img2img` bodies, and therefore archived in the work item's
sidecar. Reopening an item puts both selects back. They are also the one thing
that survives `swap_model`'s wholesale replace — no checkpoint states them, so
there is nothing of the incoming profile's to merge with — and each pass writes
its own choice back, so a saved output records what actually ran.

- Also in this go: **Output upscale** is a box in the Advanced panel rather than
  a settings-only knob. `upscale_factor` reaches `GenRequest` for the first time,
  with a live note under it saying what the base size becomes, and `1` is not a
  small enlarge but EASU skipped entirely — which is how you see the pixels the
  checkpoint actually drew. Image tab only: the Image-to-image pass has no EASU
  stage.
- Worth knowing, since it decides whether two checkpoints are affordable: nothing
  is held between passes. Every pass is a separate `sd-cli` process that loads its
  weights and frees them on exit, so the pipeline already reloads the *same* file
  once per rung. A second model costs page cache, not VRAM.

# Feature 2 - Post-generate image quality: the options

**Item 1 is built, for the initial upscale:** an *Upscaler* select beside *Output
upscale* picks EASU or any ESRGAN file in `models/upscalers/`, through
`pipeline::enlarge`. Hi-res and *Enlarge frames* are still on EASU and are the
next callers of that helper. ADetailer is parked. See CLAUDE.md *## Upscalers*.

The survey as it was written, so the findings survive the session. The question
was what is left in FSR upscaling, whether antialiasing applies, and what else the
GPU could do for the picture between the generate and the save.

## Two answers that close a question rather than open work

**FSR 1.0 is complete here.** `upscale.wgsl` is EASU and `sharpen.wgsl` is RCAS,
which is the whole specification — there is no third pass being skipped. FSR 2 and
3 are *temporal*: they need motion vectors, a depth buffer, a jittered projection
and the previous frame's history, and a single still render has none of them. FSR 4
is a neural network with weights, which puts it in the same box as ESRGAN below
rather than in this one. Of the three unused helpers in `ffx_fsr1.h` — TEPD, SRTM,
LFGA — only TEPD buys anything, and what it points at is item 2.

**Antialiasing does not apply**, and adding it would make things worse. There is no
rasterizer, no polygon edge and no sub-pixel coverage being point-sampled: MSAA
needs geometry, TAA needs motion vectors, and FXAA and SMAA are edge-detect-and-blur
filters built to hide stair-steps on a rendered frame — pointed at a diffusion image
they read every high-contrast texture edge as a jaggy and soften it, which is
precisely what RCAS and clarity exist to undo. The aliasing that *is* in these
pictures was baked in upstream, by the VAE decoding an 8× downsampled latent: fabric
weave, railings, text and hair are undersampled at generation time, and no filter
removes that without removing the pattern with it. The one antialiasing technique
that genuinely applies is supersampling — generate bigger, resample down — which is
item 3.

## Four candidates, ranked

1. **A Real-ESRGAN rung.** The only one of these that *adds* detail rather than
   redistributing what is there. `sd-cli -M upscale -i in.png -o out.png
   --upscale-model …` never constructs a diffusion context at all
   (`stable-diffusion.cpp examples/cli/main.cpp:915` hands the init image straight
   to the upscaler), so it is a subprocess pass with no checkpoint load — exactly
   the shape `refine` and `hires` already have. `--upscale-tile-size` (default 128)
   bounds VRAM. The models are fixed 4×, which is why item 3 pairs with it.
2. **Carry the filter chain at 16-bit, and dither once on the way out.** `mid`,
   `graded`, `clarity` and `dst` in `Fsr::process` are all `Rgba8Unorm`
   (`src/fsr/mod.rs:399-413`, `:505`, `:579`), so EASU → grade → clarity → RCAS
   rounds to 8 bits four times. The grade is the worst of them: `grade.wgsl`'s own
   header already admits the sRGB/OKLab round trip loses a least-significant bit,
   and a shadows or contrast move on a sky is where that shows up as a step. About
   50 MB more intermediate VRAM at 3072² and no time cost; `fsr-selftest`'s byte-
   exact identity assertion is the guard that it stayed honest.
3. **A downscale pass.** A box/Lanczos reduce, so the output can be *smaller* than
   what was diffused — the supersampling above. Blocked today by two things, both
   deliberate: `upscale_factor` is floored at 1.0 (`MAX_UPSCALE`,
   `src/pipeline.rs:49`), and EASU is an upsampler that would undersample and alias
   harder if handed a factor below 1. Generate at 1536 and resample to 1024 and the
   VAE's grid noise and any moiré average out.

   **Settled since:** a downscale only pays after a pass that adds *real* detail at
   the larger size — ESRGAN, or a hi-res diffusion. FSR up then Lanczos down is two
   resamples of the same information and gains nothing, so it is not an option.
   The useful form is a **final output size** decoupled from the diffusion size, as
   a post-processing feature of its own. The Lanczos reduce it needs now exists
   (`pipeline::reduce`, from item 1).
4. **ADetailer.** `-M adetailer` runs a YOLOv8 detector over a finished image and
   inpaints each crop (`docs/adetailer.md`) — the answer to the small-face
   complaint, and the biggest perceptual win here on portraits. The caveat to weigh:
   the detector has to be converted once, offline, by sd.cpp's
   `scripts/convert_yolov8_to_safetensors.py`, which is **Python**. Nothing Python
   at runtime, but a Python line in the setup instructions of a project whose pitch
   is that there isn't one.

## One more, listed and not recommended

sd.cpp's own `--hires` fix can enlarge in *latent* space — `--hires-upscaler` takes
`Latent`, `Latent (bicubic antialiased)` and friends, or a model under
`--hires-upscalers-dir`. That does something this ladder structurally cannot: our
hi-res rung decodes to PNG, runs EASU, re-encodes and diffuses, so there is a VAE
round trip per rung and theirs has none. The price is that it all happens inside one
process, so taking it means giving up per-pass checkpoints, per-pass progress and
cancel for that rung — which is most of what the ladder is for.
