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
  setting. Still blocked: upstream's conditioning cache (`e6281b6`, 2026-09-23,
  `--conditioning-cache-size`) is in-process only, so a fresh sd-cli per pass
  never hits it. The memory half shipped instead: System RAM releases the text
  encoder after the prompt (`*=cpu,te=disk`).
- **A persistent `sd-server` instead of an sd-cli per pass.** Gets the
  conditioning cache and skips reloading the model every image — likely the
  biggest speed win left — at the price of models resident between runs, and a
  rework of the queue's cancel, progress and preview handling. Still a
  subprocess, so within the project's rules.
- **Bump sd.cpp** (81 commits past 59c23bc as of 2026-10-10): flash attention
  honoured in LLM text encoders (#1987), GPU-memory report and LLM-encoding
  fixes (#2020), VAE tiling retries limited to allocation failures (#2019).
  Re-transcribe `SAMPLERS`/`SCHEDULERS` and recheck the auto-fit line format.

## 1. Find faces automatically, with no Python 

sd.cpp's ADetailer needs a Python script to convert its YOLOv8 detector.
rustimage already reads PyTorch weights itself (`src/sr/weights.rs`) and runs
whole networks on the GPU through wgpu (`src/sr/`). A native face/hand detector
would give Redraw a **Find faces** button that paints the mask for you. It's a big
build, and the one that plays most to what makes this project different: nothing
else in the AMD/Linux space does it without Python.

## 2. Extend the canvas (outpainting) — parked on `outpaintingV1`

Built and tried, then parked. What it taught: an ordinary checkpoint (SDXL, Krea 2,
Klein) at high strength treats a masked strip as a fresh canvas and paints the
prompt into it — a second subject — however much picture it is shown. A picture-
heavy crop, a join pass and a background prompt each helped, none enough. The real
fix is a model built for it: **Flux.1 Fill** or an **SDXL inpainting** checkpoint,
plus a model picker for the pass. The branch has the working machinery to put one
behind.

## 3. Image editing in the Redraw dialog — Klein done 2026-10-08

Already the next planned feature (Kontext, Qwen-Image-Edit, Klein / Krea 2 edit
LoRAs). The Redraw dialog is its natural home: prompt, image, live preview and
history are all there. "Make it night" or "remove the hat" with no painting, and
undo covers edits too.

## 3b. Reference images ("put this person / this jacket in the picture") — Klein done 2026-10-08

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

## 7. Model downloader — done 2026-10-10

Built: the **Get models** tab (`src/catalog.toml`, curl as a subprocess, resume,
parts recognised under any name) and Help → *Intro to models*. Next: more entries
(SD 1.5, an SDXL fine-tune, Qwen-Image-Edit once it is a family) and TAESD
preview files once those are wanted.


For the AMD community this may matter more than any feature. The app already knows
exactly which file a greyed-out model is missing and where it belongs. A *Get
models* screen that downloads the right file to the right folder would turn the
README's download list into one click.

## 8. Search the work items

Every sidecar already records the prompt, model, LoRAs and seed. A search box over
the grid ("crab", "Klein", "detail LoRA") keeps a long archive useful.

## Order

Done: 3c, #4, #7, and #3/#3b on Klein (a References button in Generation; Edit in the
Refine dialog). Parked: #2 on `outpaintingV1`. Next candidates: Qwen-Image-Edit and
FLUX.1 Kontext as families (they reuse the reference plumbing), or #5 comparison
grids.

# Powered by Stable-diffusion.cpp and Vulkan
Remove the text "Vulkan · stable-diffusion.cpp + native FSR upscale — no Python." and create a
footer with the text Powered by Stable-diffusion.cpp and Vulkan with a link to each product.

# Handling licence
This project will be released under Apache 2.0. stable-diffusion.cpp  is MIT and we should mention that all downloaded models have different licences.

# Small image menu restyle
Move models segment above generation. It feels unnatural that it's all the way down there. I was wrong in suggesting that in the first place.

Currently the summery text is white, which makes it blend into the rest of the UI
So I think the summery text should have:     color: var(--accent-hi); and the 
.foldstate.on should use the color var(--go); instead.

Give prompt textareas a default height of 220px

# Hide video and convert tab
Currently, since I'm planning on releasing the app soon, I decided to focus on the image generation workflow.
In this regard, the video and convert tab feels like an odd side note, so I think we should hide the tabs for now.
We should make them a setting under "Nightly features" at the bottom, with default off. This will allow us to work with them more easily. The if people decides to try them anyway, the were warned in the setting that these features are WIP.
