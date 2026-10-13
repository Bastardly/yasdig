# Changelog

What changed in each release of yasdig. Each release's executables are on the
[Releases page](https://github.com/Bastardly/yasdig/releases).

## Unreleased

### New

- **Settings inside saved PNGs.** Every image saved to output carries the settings
  that made it, in the format A1111, Forge and Civitai read: prompt, negative
  prompt, model, seed, steps, sampler, CFG, size and LoRAs. yasdig also writes its
  own record of the rest of the Image tab. No file paths are ever written, and
  Settings → *Saved images* turns it off.
- **Load settings from a PNG.** Drop a PNG on the prompt box, or use *Load settings
  from a PNG…*, to fill the controls from it. A yasdig image restores everything; an
  image from A1111, Forge or Civitai fills what it names. Anything you haven't
  installed is named instead of picked.

## 0.1.1 — 2026-10-13

### New

- **Upscalers on the Get models tab.** A new *Upscalers* section downloads four
  upscaler models in one click each, into the upscaler folder:
  - **4x-UltraSharp** — crisp, detailed 4×, the common pick for generated images.
  - **4xNomosWebPhoto** — 4×, trained on real photos; for photographs and uploads.
  - **Real-ESRGAN general x4v3** — small and fast 4×.
  - **2xNomosUni SPAN** — a fast 2×, for when 4× is more than the picture needs.

  They show up under *Output upscale* and in the Refine dialog's *Upscale* as soon
  as they land. All four are ordinary model files run on Vulkan, so they work on any
  GPU yasdig runs on.
- **Save changes in the Outputs viewer.** Grading a saved picture no longer has to
  make a copy: *Save changes* writes the edit back over it, and over its favourite
  copy if it has one. The picture keeps its place in the grid. Saving twice doesn't
  grade on top of the first save, and *Reset* then *Save changes* puts the original
  back until you open another picture. *Save as new output* is still there for
  keeping both.
- **A default prompt that shows off prompt variables**: a fresh install starts from
  *a lovely {animal} with {color} hair, cinematic lighting, sharp, in focus*, with
  `animal` (cat, fox, bear) and `color` (red, blue, golden) in Settings, so a batch
  varies its subject from the first Generate.

### Fixed

- **The Hi-res button and Refine dialog's *Refine image* were unavailable on a fresh
  install**, even though the same passes ran fine as *After each generation*. They
  now follow the step counts in the Hi-res and Refine folds — what pressing them
  actually runs with — and grey out with an explanation only when that count is 0.
- **Refine and Hi-res now have step counts out of the box** (4 and 12, changeable in
  Settings). They were 0, which turned both passes off until set by hand. Both are
  minimums: a model with many steps runs more.

## 0.1.0 — 2026-10-13

The first public release: one executable for Linux and Windows, with
stable-diffusion.cpp built in.
