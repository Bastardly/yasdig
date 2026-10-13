# Changelog

What changed in each release of yasdig. Each release's executables are on the
[Releases page](https://github.com/Bastardly/yasdig/releases).

## Unreleased

### New

- **Vibrance** in the viewer's Colour sliders: saturation for the dull colours only,
  so muted colours gain the most and vivid ones barely move.
- **Crop** in the viewer. Drag a box, lock it to an aspect if you like, and apply. It
  is a step in the image's history, so it can be undone, and a Refine or Hi-res
  afterwards works on the cropped picture.
- **Area adjustments.** Give one part of the picture its own look: a rectangle or an
  ellipse with a soft edge (or everything outside it), with its own set of the
  viewer's sliders on top of the whole picture's.
- **Apply and History in the viewer.** *Apply adjustments* makes the current look
  part of the picture and starts the sliders from zero on it, as a step in a
  **History** you can jump back and forward through — the same list as the Refine
  dialog's. Crops are steps there too.
- **The viewer's panel is folds** — Tone, Colour, Detail, Crop, Area, History — each
  saying what is set inside it when folded.

### Changed

- **Keep going until stopped follows your edits.** Change the prompt, the model or any
  other setting while a loop runs, and the next image uses it; the image already
  drawing finishes as it started. There is one loop at a time: starting a loop while
  one runs updates it instead of queuing a second.

## 0.1.4 — 2026-10-13

### Changed

- **Favourite with one click.** Every thumbnail in Work items and Outputs has a star
  in its corner; click it to keep the image as a favourite, click it again to let it
  go. It replaces the *Click to favorite* mode.
- **Favourites stay in the grid.** A starred image is still shown under Current, but
  it lives with the favourites: *Delete all* neither counts nor deletes it, and
  un-starring moves it back among the rest. Favourites kept by an earlier version
  show once, starred.
- **Delete all is always shown** on Current, whichever mode the grid is in, and reads
  *Delete all N unstarred*, so it says what it will take. It still asks before deleting
  anything, and still never appears on Favorites.

## 0.1.3 — 2026-10-13

### Changed

- **LoRA strength is no longer limited to -2..2.** Any number goes; some LoRAs are
  made to be used at -13 or beyond.

### Fixed

- **Other websites can no longer reach the server.** A page open in another tab
  could send it requests, including *Close server* with its clear-on-close
  settings. Requests from anything but yasdig's own page are now refused, and
  *Close server* only deletes anything when the page explicitly asks it to.
- **A damaged or hostile model file no longer breaks the model list.** A truncated
  or malformed upscaler (`.pth`, `.safetensors`) or GGUF used to crash the request
  that read it, every time. It is now listed as unreadable.
- **Too-large uploads say so** instead of the connection dropping.
- **Dragging a grade slider during a Refine** no longer leaves the image unusable
  afterwards.
- **Stop and Cancel generation** can no longer kill the next job's render by
  arriving at the wrong moment.
- **Converting a checkpoint with a picked VAE** no longer writes a broken file.
- **A finished download or a Settings save** no longer resets the Generation boxes.
- **Loading settings from a PNG** made on another install now falls back, with a
  note, for a sampler, scheduler or upscaler this one doesn't have.
- Sizes, steps and the upscale factor are checked on every generation, not only
  when saved in Settings.
- Smaller fixes: two uploads in the same millisecond no longer overwrite each
  other, a download never replaces a file already at its destination, *Clear
  videos* no longer matches a folder you named like `holiday-x2`, and several
  lists on the page can no longer be drawn over by an older answer.

## 0.1.2 — 2026-10-13

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

### Build

- The release build now pins its third-party GitHub Actions to exact commits,
  checks the Vulkan SDK downloads against their published checksums, and runs with
  a read-only token except for the step that attaches the executables.

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
