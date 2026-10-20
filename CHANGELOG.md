# Changelog

What changed in each release of yasdig. Each release's executables are on the
[Releases page](https://github.com/Bastardly/yasdig/releases).

## Unreleased

### New

- **MOSS-Image.** OpenMOSS's 5B model, Base and SFT, with two Get models cards for the
  SFT: Comfy-Org's official INT8 file (5.4 GB, which sd.cpp runs as INT8) and the BF16
  original (10.2 GB). The Convert tab lists an INT8 file but won't convert it, since
  its weights are stored rotated. It renders 50 steps at CFG 3, as its model card does. It needs a fourth
  file beside the model, encoder and VAE: Qwen3.5's `tokenizer.json`, which the card
  fetches and the Models panel lists as *Tokenizer*. Its Qwen3.5-9B encoder is never
  offered to Flux.2 Klein 9B, or Klein's Qwen3-8B to it, although both are 4096 wide.
- **Get models is grouped**: image models, video models and upscalers each under a
  heading, in the list and in its menu.
- **Recommended models.** FLUX.3 Klein, MOSS-Image and Krea 2 are marked
  *Recommended* on their Get models cards, which come first, and their families lead
  the Image tab's model list.
- **Video tab (nightly)**:
  - **Video models in Get models**: Wan 2.2 TI2V-5B, the Wan 2.2 I2V-A14B pair, and
    MiniMax H3.
  - **MiniMax H3 runs**, with its own VAE (it was handed Wan's) and its sound. Its
    recipe is sd.cpp's (CFG 1), and it streams from the SSD because its 18 GB text
    encoder doesn't fit in System RAM's share. Check its licence before you use it.
  - **Pick a model's parts** (VAE, text encoder, H3's audio VAE) like on the Image
    tab. A reopened clip puts back the parts it was made with.
  - **Clips are saved as a video file** (`.avi`, with sound when the model makes it)
    in the output folder, beside the frames. *Settings → Save clips as* switches back
    to frames only. The player has a *Download* button and plays H3's sound, and
    *Enlarge* follows the same setting.
  - **An Info panel**, like the Image tab's: the clip's size, frames, seed (click
    to reuse it), how long it took, its settings, the files it ran with and where
    it was saved.
  - **VAE tiling is on by default**, since a clip is likelier to run out of memory in
    the VAE decode than anywhere else. Turn it off under Advanced for a faster decode
    when the card has room.

## 0.2.4 — 2026-10-18

### New

- **FLUX.3 Klein and FLUX.3 Klein Base.** Black Forest Labs' new models, recognised
  from the file like every other family: Klein draws in 16 steps at CFG 1, the base
  in 20 at CFG 3.5. Both take reference images and can **Edit**. They use the Flux.2
  VAE and Krea 2's Qwen3-VL-4B text encoder, so if you have either already, the Get
  models card only fetches the model (6.2 GB at Q8).
- **FLUX.3 LoRAs** are told apart from Flux.1 and Flux.2 ones, and fit both FLUX.3
  models.

### Changed

- **stable-diffusion.cpp updated** to `b13a482` (2026-10-18). It now picks flash
  attention and direct convolution for each part of the model by measuring them on
  your GPU. yasdig still pins the two it measured (flash attention in the diffusion
  model, direct convolution in the VAE) and leaves the rest to that test, using the
  new `--flash-attn`/`--conv-direct` flags instead of the deprecated ones.

## 0.2.3 — 2026-10-18

### New

- **A work item remembers its Refine & adjust edits.** Switch to another item and
  back, or restart, and its History, sliders, selection and brushes are as you left
  them: every step is saved in a folder beside the item, which moves with it when
  starred and goes when it is deleted.
- **Edit is its own panel** in Refine & adjust, between Refine and Hi-res, shown for
  models that take references (Flux.2 Klein).
- **References from Refine & adjust**: the Edit and Refine panels open the References
  dialog. Refine can now follow the picks too (tick *Use with Refine*), whole picture
  or a painted area — "the jacket from image 1".
- A *Support yasdig on Ko-fi* link in the app's footer, the README and yasdig.com.

### Fixed

- A pass that finished after you had switched to another work item replaced the
  picture on screen with its own; it now lands in its own item's History.

## 0.2.2 — 2026-10-17

### New

- **Text on the picture**: a Text panel in Refine & adjust, with the fonts this
  computer has, bold and italic, size, line spacing, alignment, colour, opacity, an
  outline and a shadow. Drag it into place; *Add text* makes it a History step, laid
  over the picture as it looks, so its colour is exactly the one picked.

### Changed

- **Refine & adjust has a panel per pass**: Refine (with Edit), Hi-res and Extend,
  each with its own *Prompt for this pass* last and its buttons at the foot. Refine
  and Hi-res show the Image tab's own settings for those passes — model, steps, CFG,
  strength — and changing them in either place changes both. Edit has its own
  instruction box.
- **Brushes show the result, not a red overlay**, and the brush is two rings: its
  size, and the full-strength core inside the softness. The Refine mask's brush shows
  the same pair: the stroke, and how far Edge softness fades out around it.
- **Boxes step aside while you adjust**: with the pointer on a panel, the selection
  box, crop box and mask fade, so a vignette's soft edge is what you see.
- **Output upscale starts at 1** on a new install: a first render comes out at the
  size it was drawn.
- **Per model** shows its empty boxes as `default: …`, so they no longer look
  disabled.

## 0.2.1 — 2026-10-15

### Changed

- **The top bar stays put** when a page scrolls, and nothing but a dialog covers it.
  The credits are a thin footer fixed to the window's bottom, and the Image, Video
  and Settings tabs never scroll.
- **Panels can be made taller or shorter** by their bottom-right corner, and remember
  it.
- **After each generation is split up**: *Run after each image* is at the top of the
  Hi-res pass and Refine pass panels, *Save Output* is in the rail above *Until
  stopped*, and Output upscale and Upscaler stay in Generation.
- **The rail shows the seed** the next Generate uses, in amber when it is pinned.
- **Settings is a workspace** like the Image tab: eight sections in the rail —
  Default prompt (with the prompt variables, in a panel that also widens), Generation,
  Image to image, Hi-res pass, Refine pass, Per model, General and Nightly — with Save
  at the rail's foot. A variable's words can be one phrase per line. Generation notes
  which defaults only reach SD 1.x, 2.x and SDXL, and Nightly warns that it is a work
  in progress.
- **Refine & adjust** says *Press Esc to close* at its foot.
- **Every release is checked in the open**: VirusTotal scans the archives and the
  program inside each, the release notes link the reports, and every release carries
  `SHA256SUMS` and a GitHub build attestation. On [yasdig.com](https://yasdig.com)
  both download buttons now open a dialog with the version, VirusTotal's result, the
  SHA-256 (with a copy button) and the attestation.

## 0.2.0 — 2026-10-14

### New

- **Scale** replaces Upscale in the editor: smaller (0.25×–0.75×, or an exact width)
  is instant, bigger is FSR or an upscaler model as before. When a pass runs out of
  memory, its box offers **Scale down to 0.75× and try again**.

### Changed

- **One dialog, Refine & adjust.** Post-processing and Refine are now one dialog with
  one History — and Compare now works on adjustments too. A narrow rail opens
  floating panels over the picture — Refine, Scale, Adjust, Brushes, Selection,
  History — that you can drag anywhere and that stay where you put them; a dot on a
  rail button shows its panel has something in effect, and a panel's actions stay
  at its foot, the main one in amber — *Apply adjustments* is History's, since it
  adds a step. Whichever of Refine, Crop, Selection or Brushes is open owns the
  pointer; with none open, a drag pans.
- **The Image and Video tabs are workspaces** like the editor: a rail, the picture
  filling the window, and floating panels for every section (Prompts, Models,
  Generation, …), each remembering where you left it. Work items are two panels,
  **Current** and **Favorites**; the picture's size, seed and timings are an **Info**
  panel; *After each generation* is part of the Generation panel, which starts with the
  seed. Generate, Stop and *Until stopped* (was "Keep going until stopped") are at the
  rail's foot. The action row is gone:
  **Hi-res** is in Refine & adjust's Refine panel (and works on Outputs too), and
  **Save to output** in its rail.
- **Outputs open in the same dialog**, and Refine, Hi-res, Edit, Extend and Scale work there:
  *Save changes* writes the result over the file. *Refine & extend…* is gone.

### Fixed

- The Refine dialog's *Cancel generation* button did nothing.

## 0.1.8 — 2026-10-14

### New

- **Download from the viewer.** A *Download* button saves the picture as you see it,
  full size and with every adjustment, to your browser's downloads — with its
  settings in the PNG, like *Save to output*.

### Changed

- **The viewer's buttons**: *Download* comes first, *Apply adjustments* is the amber
  button, and on the Outputs tab *Save changes* is neutral. Leaving an output with an
  edit that isn't saved — closing, ←/→ or *Refine & extend…* — now asks first.

## 0.1.7 — 2026-10-14

### New

- **Pick an area by colour.** A colour wheel over the picture selects hues, and two
  bars select lightness and saturation, each with a soft fade at its edges. The
  area's sliders then change only those colours — anywhere, or inside a rectangle or
  ellipse — and the brushes can be limited to them too. A grey preview shows what is
  selected while you drag. A **Colour feather** slider softens the selection's edges
  in the picture, so strong changes don't leave hard outlines.
- **Restore brush.** Paint an earlier History step back in: refine the whole picture,
  then take back the parts you liked better before. Your sliders apply to the
  combined picture, and *Apply adjustments* makes it a new step.

## 0.1.6 — 2026-10-14

### Changed

- **Releases are archives**: `yasdig-windows-x86_64.zip` and
  `yasdig-linux-x86_64.tar.gz`, each a `yasdig` folder holding the program and a short
  README. Edge warns far less about downloading a zip than a bare program, and on
  Linux there is no `chmod` step any more. Started from inside the zip without
  extracting, yasdig now stops and says so, instead of making its home in a folder
  Windows deletes.

### New

- **Brushes in the viewer**: Dodge, Burn, Saturate, Desaturate, Blur and Sharpen,
  painted where you want them with a soft or hard brush. Each tool's strength stays
  adjustable after you paint, until *Apply adjustments* makes the strokes part of
  the picture.

## 0.1.5 — 2026-10-13

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
