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

## 1. Find faces automatically, with no Python (I don't think this is needed)

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

## 3c. Prompted refinement, all in one dialog

Give Refine and Hi-res a prompt of their own, and make the Redraw dialog the one
place for refining an image.

**It's possible, and cheap on the server.** Refine and Hi-res are img2img passes
that currently re-use the image's own prompt (`p.prompt` in `Engine::refine` /
`Engine::hires`). An optional `prompt` on `EscalateRequest` does the job: blank
means the image's own, as Redraw does now. Like Redraw's, it belongs to the pass,
not the image: `Pending.prompt` keeps describing the picture, the history label
shows what the pass was asked for ("Refine: sharper fabric"), and `{variables}`
expand with the pass's seed.

**The dialog becomes Refine.**
- The Redraw button in the action row becomes **Refine…**, and the plain Refine
  button above the image goes away.
- In the dialog, one prompt box and one button. **Nothing painted** refines the
  whole image at the Refine strength and steps. **Something painted** redraws just
  that area. The button says which ("Refine image" / "Refine area"), so the mask
  decides and there's no mode to remember.
- Hi-res moves in too: the scale and a **Hi-res** button in the same panel, using
  the same prompt box. That gives every finishing pass the live preview, Cancel
  generation and History.
- The Hi-res button and scale select above the image could then go as well, which
  leaves the action row as Save, View & adjust and Refine…. Worth deciding when it's
  built: Hi-res without the dialog is one click today.

**Bulk runs keep their prompts in the main menu.** The *After each generation*
fold gets an optional **Refine prompt** and **Hi-res prompt** (blank = the image's
own). They're captured at queue time like everything else in a run, so a batch of
eight gets the same finishing instruction, with `{variables}` rolled per image.
They also belong in *Save as defaults for this model*.

**One thing to say in the Help text:** a prompt far from what's in the picture does
little at low strength, and at high strength it starts changing the composition.
Refine's usual 0.2–0.35 suits "more detail in the fabric" more than "make it a red
dress"; that one is a Redraw with a mask, or a stronger refine.

## 4. Before/after compare

A split slider in the viewer or the Redraw dialog, comparing the current step with
any earlier one. History already keeps every step, so this is mostly page work,
and it's exactly what judging a redraw or a refine needs.

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

## Picks

- **Next for fun:** #2, outpainting.
- **The one most worth building:** #1, native face finding.
