# Future ideas

Ideas collected on 2026-10-06, roughly from most exciting to most practical. None
of them is started. Where sd.cpp's support hasn't been checked yet, the idea says
so.

## Open follow-ups from the 2026-10-06 session

- **Sharper live previews (TAESD).** sd.cpp's `--preview tae` decodes each step
  with a tiny autoencoder, giving near full-size frames instead of `proj`'s 1/8
  colour approximation. It needs a small extra download per family (taesd,
  taesdxl, taef1, taew2_1, …) and a way to recognise those files.
- **Step caching for video.** Wan is a DiT, so Spectrum and Adaptive (EasyCache)
  both apply. It's waiting until video renders again. The work is a `cache_mode`
  field on `VideoModel`, its value in `VideoModel::discovered()`, and the flag
  mapping in `SdRunner::video_cmd`.
- **Making a cache mode a family default.** Measure Off / Spectrum / Adaptive per
  family. Where one is a free win, set it in that family's `recipe()`.
- **Text-encoder cache in `work/cache/`**, cleared by *Close server* with no
  setting. Blocked: sd-cli can't save or load conditioning. Check again on every
  sd.cpp bump.

## 1. Find faces automatically, with no Python 

sd.cpp's ADetailer needs a Python script to convert its YOLOv8 detector.
rustimage already reads PyTorch weights itself (`src/sr/weights.rs`) and runs
whole networks on the GPU through wgpu (`src/sr/`). A native face/hand detector
would give Redraw a **Find faces** button that paints the mask for you. It's a big
build, and the one that plays most to what makes this project different: nothing
else in the AMD/Linux space does it without Python.

## 2. Extend the canvas (outpainting)

Mostly Redraw machinery: grow the canvas, make the new strip the mask, and redraw
it so the picture continues. An *Extend ← → ↑ ↓* row in the Redraw dialog, with
history and the live preview for free. Probably the most fun per hour.

## 3. Image editing in the Redraw dialog

Already the next planned feature (Kontext, Qwen-Image-Edit, Klein / Krea 2 edit
LoRAs). The Redraw dialog is its natural home: prompt, image, live preview and
history are all there. "Make it night" or "remove the hat" with no painting, and
undo covers edits too.

## 3b. Reference images ("put this person / this jacket in the picture")

sd-cli's `-r` / `--ref-image` takes one or more reference images beside the prompt:
"the woman from image 1 wearing the jacket from image 2, in a market". It's the
same plumbing as image editing (an edit is the current image as reference 1), so
the two are one feature.

- **Flux.2 Klein 4B / 9B**: already a runnable family here, so this is `-r` plus
  UI.
- **Qwen-Image-Edit 2509 / 2511**: multi-reference is its signature use. A new
  family with Qwen-Image's tensors, so it needs a per-model flag; 2509+ wants
  `--llm_vision`, and 2511 wants `--model-args qwen_image_zero_cond_t=true`.
- **FLUX.1 Kontext** takes one reference. **Krea 2** works through edit LoRAs via
  `--ref-image-args`.
- **SDXL**: IP-Adapter (Plus; needs CLIP-Vision ViT-H plus the adapter file) for a
  looser likeness, or PhotoMaker v1 for faces. PhotoMaker v2 and PuLID need a
  Python step, so they're out.

UI: a few numbered reference slots on the Image tab from the `sourceSlot` factory
(upload, paste, drop, pick from Outputs), so the prompt can say "image 1".

## 3c. Prompted refinement, all in one dialog — done 2026-10-08

Built. Refine and Hi-res take a prompt of their own, and the Redraw dialog became
the **Refine** dialog: nothing painted refines the whole image, something painted
refines that area. Hi-res stays above the image, since it sets the final size and
comes before refining. The action row is Hi-res, Save, View & adjust and Refine…. The Refine pass and Hi-res pass folds have optional prompts for bulk
runs. Unlike this note first said, those prompts are **not** per-model: a prompt is
the page's, like the main prompt, so *Save as defaults* doesn't store them.

## 4. Before/after compare — done 2026-10-08

Built in the Refine dialog: **Compare** (or **C**) splits the picture between an
earlier step and now, with a line you drag. *Compare with* picks the step. Both
halves carry the current grade and sharpening, and a smaller step is stretched to
the same box, so an upscale can be judged too.

## 5. Comparison grids (X/Y plots)

Vary one or two settings across a batch (CFG × sampler, LoRA strength, seeds) and
get a labelled grid of results. The job queue and prompt variables already do most
of it. It's the fastest way to learn what a new model likes, and good Help-tab
material.

## 6. Sketch or pose to image (ControlNet)

sd.cpp has `--control-net` and `--control-image`. The Redraw canvas could double as
a sketch pad: draw rough shapes, generate from them. Not yet checked which
ControlNet models the current sd.cpp build runs well, so this starts with a
header-reading spike, like the families did.

## 7. Model downloader

For the AMD community this may matter more than any feature. The app already knows
exactly which file a greyed-out model is missing and where it belongs. A *Get
models* screen that downloads the right file to the right folder would turn the
README's download list into one click.

## 8. Search the work items

Every sidecar already records the prompt, model, LoRAs and seed. A search box over
the grid ("crab", "Klein", "detail LoRA") keeps a long archive useful.

## Order

3c and #4 are done. Next: **#2 outpainting**, then **#3/#3b editing and
references on Klein**.
