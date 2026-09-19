# rustimage

A Python-free image generator for AMD GPUs on Linux. It generates **small and cheap**
with [stable-diffusion.cpp](https://github.com/leejet/stable-diffusion.cpp) on the
**Vulkan** backend, then does the expensive-looking part on the GPU: a **native AMD
FidelityFX Super Resolution 1.0 upscale + sharpen** as compute shaders via
[`wgpu`](https://github.com/gfx-rs/wgpu) — no ML upscaler, and no Python anywhere in
the build or runtime.

The base render is one diffusion pass at 768² (the default; distilled checkpoints like
SDXL-Turbo want a template with 1–4 steps and CFG 1). Anything better than that is
**escalated on demand**, so you only pay for it on the images worth keeping.

```
prompt ──▶ sd-cli (Vulkan, 768px default) ──▶ EASU upscale ──▶ grade + clarity + RCAS ──▶ PNG
your image ──▶ fit to budget ──▶ img2img ─────────▶ ▲
                                                    ├─▶ hi-res   enlarge, then img2img
    on demand, from the buttons above the result:   └─▶ refine   img2img in place, no rescale
    (either order, repeatable, and each may name its own checkpoint)
```

Both escalations read the *unfiltered* image and write a new one; the grade, clarity
and RCAS run once on the way out. So no filter compounds, and no pass ever diffuses
over pixels an earlier pass already sharpened or graded.

The **Image to image** tab is the second way in: drop, paste or pick a picture and
run one diffusion pass over it, after which it climbs the same ladder. And because
a checkpoint describes the pass *about to* run rather than the one that already
did, each rung can name its own — draft on a Turbo model, finish on a detailed one.
See [Image to image](#image-to-image).

There is a **Video** tab too, over the same subprocess and the same GPU queue: Wan 2.2
through sd.cpp's `vid_gen` mode, animating either a prompt or one of your finished
images. A clip comes back as a PNG sequence, which is what lets the same FSR passes
enlarge it frame by frame — a fixed kernel can't flicker, so that stays stable where
per-frame diffusion boils. See [Video](#video).

A tiny std-only HTTP server glues it together and serves a single static HTML page.

## Why not FidelityFX-CLI?

The plan originally called for shelling out to AMD's `FidelityFX-CLI`. That tool is
**Windows-only** (it uses the Windows Imaging Component + Direct3D 11), so on Linux the
post-process is implemented directly as Vulkan compute shaders through `wgpu`:

- `src/fsr/upscale.wgsl` — **FSR1 EASU** (Edge-Adaptive Spatial Upsampling), a port of
  `FsrEasuF`. It fits a local edge direction from luma gradients over a 12-tap
  neighbourhood and stretches a Lanczos-2-shaped kernel along that edge, so it doesn't
  filter across edges the way a fixed isotropic kernel does.
- `src/fsr/sharpen.wgsl` — **FSR1 RCAS** (Robust Contrast Adaptive Sharpening), a port of
  `FsrRcasF`: the post-upscale variant, with the ring-derived limiter that rules out
  ringing by construction. RCAS's optional noise-detection term is deliberately not
  enabled — it scales the lobe down exactly where a diffusion render has fine detail.
  One deviation from the reference: the cross is sampled further out on a large image,
  so the filter tracks the picture rather than the pixel grid.
- `src/fsr/contrast.wgsl` — not FSR: a local-contrast ("clarity") unsharp mask between
  the two, on brightness only, and signed — negative softens the band instead of
  boosting it. It moves the mid frequencies that read as contrast and
  depth while leaving the fine band alone. It exists because a low-strength img2img
  enters the noise schedule at low sigma and so only ever rebuilds fine texture — depth
  is the one thing the refine pass structurally cannot add, and raising strength to
  reach it re-composes the picture instead.

- `src/fsr/grade.wgsl` — not FSR either: eight colour knobs (lightness, contrast,
  highlights, shadows, saturation, hue, temperature, tint) in a single pass, ahead of
  the other two. Every one of them is a per-pixel transform, so they cost one dispatch
  together however many you have moved. It works in OKLab, where each knob is one named
  operation and where "warmer" and "greener" are literally the two colour axes.

All four run on the same AMD GPU sd.cpp uses, with no model weights. `cargo test`
validates the WGSL through naga, since it would otherwise only compile at GPU device
creation — a shader typo would surface as a startup failure on the worker thread.

Each stage is skipped when it would be a no-op — EASU at scale 1.0, the grade with all
eight knobs at 0, contrast at clarity 0.0 — so `rustimage fsr in.png out.png 1.0 0.5`
is an exact sharpen-only pass, and `rustimage fsr in.png out.png 1.0 0.0 0.0` is a
byte-exact copy.

## Requirements

Verified target: AMD Radeon RX 7900 XTX (RADV), Vulkan 1.4, Fedora. Any Vulkan-capable
GPU should work.

- Rust (stable) — for building this crate.
- A Vulkan driver (Fedora: `mesa-vulkan-drivers`, already present with RADV).

## One-time setup

### 1. Build stable-diffusion.cpp (Vulkan)

Its Vulkan backend (ggml-vulkan) compiles GLSL compute shaders at build time, so it needs
CMake, a GLSL compiler, and the SPIR-V / Vulkan dev packages:

```sh
sudo dnf install cmake glslc vulkan-loader-devel \
                 spirv-headers-devel spirv-tools-devel glslang-devel

git clone --recursive https://github.com/leejet/stable-diffusion.cpp
cd stable-diffusion.cpp
cmake -B build -DSD_VULKAN=ON
cmake --build build -j
```

This produces the CLI binary (`build/bin/sd-cli` on recent checkouts, `sd` on older ones).

> If configure fails with *"Could not find a package configuration file provided by
> SPIRV-Headers"*, you're missing `spirv-headers-devel` / `spirv-tools-devel` — install
> them and re-run `cmake -B build ...` (delete `build/` first).
>
> A `pnpm not found; frontend build disabled` warning is harmless — it only skips sd.cpp's
> own example web UI, which rustimage doesn't use.

> Note: `rustimage` itself does **not** need `glslc` — its FSR shaders are WGSL, compiled
> in-process by `wgpu`/naga. `glslc` is only needed to build sd.cpp.

### 2. Get a checkpoint

Download a single-file `.safetensors` / `.ckpt` / `.gguf` (no conversion needed) into
`models/`. That's the whole step — every checkpoint in that folder is **auto-discovered**
and listed in the UI, so no config edit is needed to add a model. Distilled checkpoints
(SDXL-Turbo, SD-Turbo, LCM) give the fastest results, but want a template to set their
low step count and CFG 1 — see below.

### 3. Smoke-test sd.cpp on its own

Before involving Rust, confirm generation works:

```sh
./build/bin/sd-cli -M img_gen -m models/sdxl-turbo.safetensors \
  -p "a lovely cat" --steps 4 --cfg-scale 1 --sampling-method euler -o test.png
```

## Configure

There are two ways to get a selectable model, and the first needs no config at all:

- **Discovery** — any checkpoint in the models dir is auto-listed and runs with the
  `[defaults]` profile. Nothing to edit.
- **Templates** — a `[templates.<name>]` block is a tuned preset that overrides the
  defaults for one specific checkpoint (matched by its `model` path). Use it for models
  that need non-default settings, e.g. a Turbo checkpoint at 4 steps and CFG 1.

Either way, **adding a model never needs a code change.**

Edit `models.toml`:

- `[sd].binary` — path to your `sd-cli` (or `sd`) binary.
- `[sd].mode` — `img_gen` for recent sd.cpp; `txt2img` for older builds.
- `[sd].models_dir` — folder scanned for checkpoints (default `models`).
- `[server].host` / `[server].port` — default `127.0.0.1` and `8080`.
- `[defaults]` — the generation + FSR settings applied to every **discovered**
  checkpoint. Tuned for standard (non-distilled) SDXL.
- `[templates.<name>]` — a tuned preset, as above. Takes the same fields as
  `[defaults]`, plus `model` and an optional `description`.

Settings fields, shared between `[defaults]` and `[templates.*]` except where noted:

| field | meaning |
|-------|---------|
| `model` | path to the checkpoint, relative to project root (**templates only**; a discovered model is its own file) |
| `description` | optional label shown in the UI (templates only) |
| `width` / `height` | base generation size, default `768` each (small = fast) |
| `steps`, `cfg_scale`, `sampler` | sd.cpp params; defaults `20`, `5.0`, `euler` (Turbo → few steps, cfg 1) |
| `img2img_cfg_scale` *(opt)* | CFG for the refine and hi-res passes only; unset (the default) follows `cfg_scale`. `0` is legal and means guidance off |
| `upscale_factor` | FSR upscale, default `2.0` (768 → 1536) |
| `sharpness` | RCAS sharpness, 0..1, default `0.0` = off (1.0 = RCAS at full strength) — where the viewer's slider starts. The only display filter here besides `clarity`: the eight colour knobs are not per-model and always start neutral |
| `clarity` | local contrast, -1..1, default `0.0` = off — positive adds mid-frequency depth, negative softens; likewise the slider's starting value |
| `refine_steps` / `refine_strength` | the "Refine image" img2img pass, in place at the current size (0 steps = disabled, and `refine_steps` defaults to 0) |
| `source_steps` / `source_strength` | the **Image to image** tab's one pass over a source image you supply. Defaults `12` and `0.55` — far higher than the two escalations, because this one is asked to make a picture *out of* one rather than to finish one it made |
| `hires_steps` / `hires_strength` | the "Hi-res pass" img2img, after an enlarge (0 steps = disabled, and `hires_steps` defaults to 0) |
| `hires_max_side` | longest side the hi-res pass may diffuse at (default 2048); beyond it, FSR finishes the job. Does not limit refine |
| `hires_flash_attn` | `--diffusion-fa` for the img2img passes when they run large (default true; worth 6× at 2048²) |
| `weight_type` *(opt)* | on-the-fly quantization, e.g. `q8_0`, via sd-cli `--type` |
| `extra_args` *(opt)* | arbitrary flags passed verbatim to sd-cli |

All three denoise strengths must be between **0.05 and 0.95** — a
strength of 0 makes the pass a silent no-op, and at exactly 1.0 sd.cpp discards the
init image and the pass degenerates into a plain txt2img. Out-of-range values are
rejected at startup rather than at the first refine.

### Video models

**Drop the files in `models/video/` and they are identified by what is inside them.** A
video model is three to five files — transformer, VAE, text encoder, sometimes a *second*
transformer — whose roles are not in their names, so rustimage doesn't read the names. It
reads the header: both safetensors and GGUF carry a tensor index at the front of the
file, and the tensor names in it identify the architecture outright. That is one seek and
a few hundred KB against files of 8 to 20 GB — identifying 37 GB across three files takes
7 ms — so the folder is rescanned on every listing and a download that just finished
shows up without a restart.

It sorts out which of the two Wan VAEs a file is (they differ by latent width, and using
the wrong one produces garbage rather than an error), which text encoder is which, and
which family a transformer belongs to. The discriminators are lifted from
stable-diffusion.cpp's own detection code, so the answer agrees with the binary that will
load it.

Why it isn't filename matching, with a real example: a file downloaded as
`minimax_h3Comfy.safetensors` is a **Qwen3-VL-32B text encoder**, not a MiniMax H3
transformer. The name is wrong; the tensors aren't.

Suggested layout — ComfyUI's, which is what every download instruction assumes. The scan
is **recursive**, so this is for your benefit, not the program's; a flat folder discovers
exactly the same models:

```
models/video/diffusion_models/   the transformers
models/video/vae/                wan_2.1_vae / wan2.2_vae
models/video/text_encoders/      umt5-xxl — shared by every Wan family
models/video/loras/              e.g. the lightx2v 4-step pair
models/video/clip_vision/        Wan 2.1 I2V/FLF2V only
```

One folder per *role* rather than one per model, because the encoder is shared: a single
7 GB umt5 serves every Wan you own, and a folder per model would mean copying it or
maintaining a tree of symlinks.

`doctor` prints what it found, what each model is still missing, and any file it could
not place.

#### When you still want a `[[video]]` entry

Three cases, and settings you have measured is only one of them:

- **Naming files you haven't downloaded yet.** A model with missing components is
  *listed and greyed out*, not hidden, and both `doctor` and the model select name the
  file they're waiting for — so the table doubles as a download checklist. The two
  entries shipped in `models.toml` do exactly this.
- **Pinning settings** once you have timed a model on your own card.
- **Saying which half of a mixture-of-experts pair a file is.** This is the one thing no
  header can answer: the high- and low-noise experts of an A14B have identical tensor
  names and identical shapes and differ only in their weights. If the filename says
  `LowNoise`/`HighNoise` the two are paired automatically; if it says nothing, the file
  is listed as a `wan_dense` model that runs on its own, which is a thing sd.cpp does
  quite happily. Pairing it is then a decision you make, with information the program
  doesn't have.

An entry **replaces** a discovered model rather than patching it, matching by `name` or
by naming the same `diffusion_model` file. So an entry is complete in itself, and what it
leaves out comes from the family's built-in recipe rather than from the discovered copy.

```toml
[[video]]
name        = "wan2.2-ti2v-5b"
family      = "wan22_ti2v_5b"
diffusion_model = "models/video/diffusion_models/wan2.2_ti2v_5B_fp16.safetensors"
vae             = "models/video/vae/wan2.2_vae.safetensors"   # NOT wan_2.1_vae
t5xxl           = "models/video/text_encoders/umt5-xxl-encoder-Q8_0.gguf"
width = 832
height = 480
frames = 33
fps = 24
steps = 20
cfg_scale = 6.0
flow_shift = 3.0
```

| field | meaning |
|-------|---------|
| `name` | required; what the UI shows and what `POST /video` takes |
| `family` | required; see the table below — it decides the flags, the grids and whether fps is yours |
| `diffusion_model` | required; the transformer (the *low-noise* one, for a MoE pair) |
| `high_noise_diffusion_model` | the second expert. Required for `wan22_a14b`, meaningless elsewhere |
| `vae` | required. `wan2.2_vae` for the TI2V-5B, `wan_2.1_vae` for everything else Wan |
| `t5xxl` | the umt5 text encoder; required for every Wan family |
| `llm` / `audio_vae` | H3's Qwen3-VL encoder and separate audio VAE |
| `clip_vision` | CLIP-Vision tower — Wan 2.1 I2V/FLF2V wants it, Wan 2.2 doesn't |
| `width` / `height` | default `832`×`480`; rounded **up** to the family's multiple |
| `frames` | default `33`; rounded to the family's grid (Wan **down**, H3 **up**) |
| `fps` | default `16`; ignored by any family with a forced rate |
| `steps`, `cfg_scale`, `sampler` | defaults `20`, `6.0`, `euler` |
| `flow_shift` *(opt)* | timestep shift. Unset lets sd.cpp choose, but every documented Wan run passes `3.0` |
| `high_noise_steps` *(opt)* | steps for the high-noise expert. Unset hands sd.cpp `-1`, which splits the schedule itself at `moe_boundary` |
| `high_noise_cfg_scale` *(opt)* | unset follows `cfg_scale`, the same live link `img2img_cfg_scale` has |
| `moe_boundary` *(opt)* | handover timestep, default `0.875`. **Only honoured while `high_noise_steps` is unset** — sd.cpp's own flag help says so |
| `vace_strength` *(opt)* | VACE conditioning strength, for the frames-in/frames-out family |
| `negative_prompt` | defaults to Wan's own published negative prompt, so you never retype 400 characters of Chinese |
| `loras` *(opt)* | sd.cpp's inline names without the brackets: `"name:1"`, or `"\|high_noise\|name:1"` to target an expert |
| `lora_dir` *(opt)* | `--lora-model-dir` |
| `offload_to_cpu` | default **true**; it is what makes the A14B pair possible at all |
| `vae_tiling` | default false. The *first* thing to turn on if a clip dies in the decode — Wan's VAE is the memory-hungry end |
| `flash_attn` | `--diffusion-fa`, default true |
| `weight_type` / `extra_args` | as on the image side. `["--auto-fit", "off"]` and `["--rng", "cpu"]` live here |

| `family` | shape | size grid | frame grid | native fps |
|----------|-------|-----------|------------|------------|
| `wan22_ti2v_5b` | one dense checkpoint; the only Wan taking `wan2.2_vae` | 32 | `4k+1`, rounds down | 24 |
| `wan22_a14b` | mixture-of-experts *pair*, high-noise early and low-noise late | 16 | `4k+1`, rounds down | 16 |
| `wan_dense` | any single dense Wan on the 2.1 VAE — a 2.1 T2V, or one A14B expert run alone | 16 | `4k+1`, rounds down | 16 |
| `wan21_vace` | frames in, frames out — the pass that refines a finished clip | 16 | `4k+1`, rounds down | 16 |
| `minimax_h3` | present so it's a config edit later, not runnable on 24 GB / 30 GB | 32 | `17k+5`, rounds **up**, floor 5 | forced 24 |

The rounding is sd.cpp's, not ours, and it *warns and carries on* rather than failing —
so rustimage rounds first and shows you the result, and the line under the boxes says
what will actually run and how long it will be. Sizes are `vae_scale_factor × 2`. Frame
counts go through the latent frame count and back, and because that division truncates,
**34 Wan frames run as 33**: asking for more can give you fewer.

Two notes on memory, against a 24 GB card and 30 GB of RAM. Only one A14B expert is
resident per step, so the *card* is not the limit. `offload_to_cpu` holds **both**, so
system RAM is: 14B at Q4_K_M is roughly 8 GB an expert, which puts the pair plus a ~7 GB
encoder at around 23 GB — it fits 30 GB, but not by much. Q8_0 (~15 GB an expert) does
not fit at all. Those are arithmetic, not measurements; peak RSS during a run is the
number that settles it, and if 23 GB crowds the machine the levers are Q3_K_M on the
experts and a smaller encoder, in that order.

## Run

```sh
cargo run --release
# [rustimage] 2 model(s) available in /home/you/rustimage/models:
# [rustimage]   • samExtTURBO_v10   [discovered, 6.5 GB]  768x768 → 1536x1536
# [rustimage]   • sd_xl_base_1.0    [discovered, 6.5 GB]  768x768 → 1536x1536
# [rustimage] GPU worker ready on adapter: AMD Radeon RX 7900 XTX (RADV NAVI31)
# [rustimage] GPU post-process: upscale=easu, grade=oklab, contrast=clarity, sharpen=rcas
# [rustimage] listening on http://127.0.0.1:8080
```

Startup runs a preflight first: it checks the sd.cpp binary, lists the checkpoints it
found, and warns about templates pointing at files that aren't there. If something is
missing it prints copy-pasteable fix instructions and exits rather than failing later,
mid-request — `doctor` below runs the same checks without starting the server.

Open <http://127.0.0.1:8080>, pick a model, type a prompt, Generate. The result shows
output resolution, seed, and per-stage timings (generate / fsr / refine / hi-res).
`-1` in the seed box means "draw a new one each time" and stays that way; click the
seed under the image to put it in the box when you want that image back.

Every generation setting is editable per-request from the controls beside the prompt —
steps, CFG, sampler, base size, and both escalation pairs. They
pre-fill from the selected model and are sent with the request, so one discovered
checkpoint works for both Turbo and standard settings without touching config. The
settings a request was generated with are retained with the image, so a later refine or
hi-res uses those rather than whatever the panel shows by then.

Every control has a small **(i)** beside its label, with a sentence or two on what the
setting does and how to choose a value — and, where that is the honest answer, on the
fact that the value belongs to the *checkpoint* rather than to image generation: a
turbo model is trained to land in 1–4 steps at CFG 1, where a standard SDXL wants
around 20 at 5–7. The same icons are on the image-to-image, video and convert tabs and
on the viewer's ten sliders.

Quality is escalated *after* the fact, from the buttons above the result — so you only
pay for it on images worth keeping. The server retains each result's intermediates, so
no step ever recomputes the ones below it:

- **Hi-res pass** — enlarge by 1× to 4×, then img2img at that size (`hires_*`). This is
  what adds detail the EASU upscale can't invent. Past `hires_max_side` the pass
  diffuses as large as its budget allows and covers the rest with a final FSR upscale,
  so a 4× request stays affordable.
- **Refine image** — img2img over the current image, at whatever size it currently is,
  with no rescale (`refine_*`). Usually tuned with fewer steps and a lower strength than
  hi-res, which makes it the cheaper rung and the finishing one: run it *after* a hi-res
  pass, and repeat it if it helps.

For both, `*_steps` is a **floor** on the denoising steps that run, not an exact count.
Strength decides how far up the noise schedule a pass starts, so a higher strength has
more range to cover and the count is raised to at least `steps × strength`. Too few
steps for the strength doesn't reinterpret less — it leaves un-removed noise behind,
which is what reads as grit. The UI shows the number each pass will actually run.

Guidance is the other half of that. **Refine/hi-res CFG** (`img2img_cfg_scale`) sets the
CFG for those two passes alone. The value that suits a 768² base render over-guides at
output resolution: a low-strength pass has little freedom to begin with, and pushing it
hard against the prompt spends that freedom on high-frequency artifacts — the same crunch
under-stepping produces, arriving from the other direction. If a pass looks harsh, try
~2–3 here before reaching for more steps. Leave the box blank and it follows the CFG
above, which is the default and matches how the passes have always run; the note under
the controls always states the number that will actually be used.

Order is free and both repeat. The hi-res button disappears once the image is past
`hires_max_side`; refine is not under that ceiling, because it doesn't rescale and so
has nowhere to fall back to — and an image FSR has stretched beyond its last diffusion
is the one most worth refining. Either button is hidden if its pass is set to 0 steps.

That does mean refine will diffuse at whatever size you're at, 3072² included. Keep an
eye on VRAM; if the pass fails, the image is left intact so you can retry.

Refine deliberately does **not** go back to the base render. Every img2img round-trips
through the VAE, which costs fine detail no matter how low the strength; paying that at
base resolution and then stretching the result 2× spends the loss on detail while only
the structural gain survives the upscale. That is what made the old refine pass read as
soft. At output resolution there is nothing downstream to smear what it adds.

### Work items

Every image you generate is kept, in `work/workitems/`: the unsharpened
output-resolution PNG, a thumbnail, and a JSON sidecar with the settings that produced
it. They survive a restart, and they show up as a panel of thumbnails — a third column
beside the controls and the result on a wide screen, wrapping underneath them when
there isn't room.

Click one and it comes back **live** — the controls repopulate with that image's
settings, the picture goes back on screen, and Refine, Hi-res and the viewer's sliders work on
it again. Because the panel has been restored to what made the image, an escalation runs
with those settings unless you change them first, in which case it runs with what the
boxes say. That is the point: render a batch, pick the one worth the money, and escalate that
one. The server keeps the last 8 results warm in memory, but an image that has fallen
out of that window reopens from disk just the same.

Flip the toggle above the grid to **Click to delete** and a click *marks* a thumbnail
instead — red frame, red cast — and a `Delete N selected` button appears. Nothing is
removed until you press it, so culling twenty images from a batch is twenty clicks and
one confirmation rather than twenty confirmations. Switching back to **Click to open**
clears the marks.

Refined and hi-res results are deliberately **not** archived: those are the expensive
rungs and most of them are experiments. **Save to output**, above the image, writes the
picture currently on screen — filters and all — to `work/output/`, with its own sidecar.
It is available for a plain generate too, and you can press it after each of several
refines to keep every step.

Everything else in `work/` is scratch and is still cleared at startup.

### Image to image

The second tab starts from a picture instead of a prompt. Drop a file on the zone
at the top, paste one, or click a tile in **Source images** — which is everything
in `work/output/`: what you have saved from the Image tab, and what you have
dropped in. An import is listed beside the rest because at the moment you are
choosing something to diffuse, the difference isn't interesting.

Anything the browser can open works — JPEG, WebP, AVIF — because the page
re-encodes it to PNG in a canvas before uploading. Nothing in the binary decodes
anything but PNG, and that stays true.

**Strength** is the knob that matters. It decides whether this is a retouch or a
re-render: 0.3 barely moves the picture, the default 0.55 keeps the composition,
the pose and the palette while redrawing the surface in the model's own idiom, and
0.7 and up treats your image as a rough sketch. **Working size** is a ceiling, not
a target — a source already smaller than it is left alone, and a bigger one is
scaled down first. It starts at the model's `hires_max_side` because that is the
same question already answered for this card: a 4000 px phone photo diffused at
full size is an out-of-memory, not a better picture.

The result then joins the ordinary ladder. Refine, Hi-res, the viewer's sliders
and Save to output all work on it exactly as they do on a generation, and it is archived
as a work item like one — so it comes back from the grid on the Image tab, source
image and all.

### Escalating with a different model

**Refine/hi-res model**, in the Advanced panel of both tabs, runs that one pass on
a checkpoint other than the one the image was made with. Leave it on *same as
generated* and nothing changes.

It is there because a checkpoint is a setting of the pass *about to run*, not of
the one that already ran — which is the same test `steps` and `cfg_scale` fail,
and why those stay retained. Draft fast on a Turbo checkpoint and finish on a
detailed one; or take something the Image-to-image tab produced and hi-res it
through the model that suits the subject.

The swap is **wholesale**: the chosen model brings its own refine and hi-res
steps, strengths, img2img CFG, hi-res budget and slider starting points, and the
boxes in the panel are then applied on top — which is what you'd expect, since
those boxes already refill themselves from whichever model is selected. It also
means the buttons re-derive from the new model, so picking one with
`hires_steps = 0` removes the Hi-res button for that image, and one with a
smaller `hires_max_side` may say the image is already past its budget.

### Batch and loop

The panel above the Generate button is about the *run* rather than the image. Set
**Images** to a number and the button says `Generate 4`; the images are produced one at
a time and each appears as it lands, so you can watch the batch fill the work items
strip rather than waiting for the lot. Tick **Keep going until stopped** and the button
becomes `Start loop generation`: it ignores the count and keeps going until you press
**Stop**, which finishes the image in flight and then ends the run. Stop stays clickable
the whole time — it is the one control a busy run doesn't disable.

While anything is rendering, the line under the buttons counts the steps as sd-cli
reports them — `Step 7/20 · 1.23s/it`, or `Loading weights 18/50 · 412.00MB/s` before
the denoising starts. It is polled from `GET /progress`, which is answered off the job
queue, so it keeps moving during a render rather than arriving once the render is done.

A pinned seed **advances by one per image**, so a batch of eight isn't eight copies of
the same picture; the first image is exactly the seed you pinned. `-1` keeps drawing a
fresh seed per image, as always. The line under the panel states what the button will do
before you press it, seed range included.

**After each image** adds three optional steps, all off by default, run in this order:

- **Hi-res pass** — at the factor in the panel's **Hi-res scale**, which is the same
  value as the select above the image.
- **Refine** — after the hi-res pass, for the reason the buttons are in that order.
- **Save to output** — writes the finished image to `work/output/`, so a run can fill a
  folder without a click per image.

They apply to a run of one just as much as to a batch, which is how you queue a single
generate-and-refine and walk away. Both passes read the same Advanced panel the buttons
do, so a run uses whatever the boxes say when it starts.

Any failure stops the run and reports it beside the controls — whatever killed image 3
would kill image 4 as well. Everything generated up to that point is still in the grid.

Two things worth knowing before leaving one unattended. **The loop runs in the page**:
closing the tab stops it after the current image (which is also the quickest way to stop
a runaway one). And every generation is archived, so an overnight loop writes a few MB
per image into `work/workitems/` — delete what you don't want from the grid.

### Video

The **Video** tab is a second workflow over the same GPU — and it really is the same
GPU: starting a clip greys out the Image tab's buttons too, because the worker is
serialized and there is one queue.

Pick a model, write a prompt, and press **Generate clip**. The negative prompt box comes
pre-filled with Wan's own published negative prompt; emptying it means *no* negative
rather than "use the default". The line under the size, frame and fps boxes states what
will actually run once those numbers are rounded to the model's grids, and how many
seconds that is — so `36` frames at 24 fps reads back as `33 frames @ 24 fps = 1.38 s`
before anything has started.

**First frame** is how you animate a still. It picks from `work/output/` — the images you
pressed *Save to output* on, i.e. the finished work rather than the raw archive. Clear it
and the same model runs as plain text-to-video. (Outputs saved before this existed get a
thumbnail generated on the first request for it, so an old folder fills in as you scroll.)

While it runs, the same `Step 7/20 · 1.23s/it` readout as the image tab counts along —
which matters much more here, because a clip is minutes rather than seconds. **Stop**
kills it, unlike the Image tab's Stop, which finishes the image in flight first: there is
only ever one clip in flight, so stopping it can only mean killing it. Nothing is kept;
the partial frames are swept.

A finished clip plays in a canvas with a scrubber, frame by frame. That is because a
clip **is** a PNG sequence on disk — `work/videos/{id}/frame_000.png` and friends — not a
video file. sd-cli's `-o` can write either a frame sequence or one muxed container, and
frames win: both of the things you'd want to do to a clip afterwards consume frames, and
rustimage has no video decoder to get them back out of a container. Frames are also
lossless, and they're what an editor wants. The cost is disk — about 700 KB a frame at
832×480, so ~23 MB for a 33-frame clip. Delete what you don't want from the Clips grid.

**Enlarge frames** runs every frame through the same FSR passes the image side uses and
writes the result to `work/output/{id}-x{factor}/`. A subdirectory, so 33 near-identical
tiles don't bury the picker. Two things make this work on video where per-frame "AI
upscaling" doesn't:

- **A fixed kernel cannot flicker.** EASU, RCAS, clarity and the colour grade are
  deterministic functions of the pixels, so two near-identical frames produce two
  near-identical outputs. Stability is by construction, not by luck.
- The filters **size their band from the image**, so a sharpness setting means the same
  thing after the enlargement as before it.

What rustimage deliberately does *not* do is re-diffuse frames. Running the refine pass
per frame would work on any single frame and boil in motion: each frame draws its own
noise and invents its own detail with no knowledge of its neighbours. Refining a clip
properly is video-native — a second pass with a VACE checkpoint and `--control-video`,
which takes a directory of frames, exactly what the first pass wrote. The plumbing for
that is a `family` entry away; it isn't wired to a button yet.

Not in this version: audio (Wan S2V is a separate checkpoint), clips longer than one
chunk window (sd.cpp implements one), and a downloadable single-file video.

### Convert

The third tab requantizes a model file you already have, instead of downloading a
different copy of it. Pick a file, pick a weight type, press Convert; the result is
written **beside the original** as a `.gguf`, so it is discovered on the next start with
no config edit. Nothing is deleted.

This exists because **a file's type on disk is not what it costs to run**. sd.cpp asks
the backend whether it can multiply the stored type and, when it can't, inserts a cast
into the compute graph for every linear layer in the model
(`src/model/common/ggml_block.hpp:211`). Every Vulkan device today reports no fp8 — so a
15 GB fp8 Wan checkpoint executes as BF16 and behaves like a file twice its size. On a
24 GB card that is the difference between running and `ErrorDeviceLost`. Converting it
to `q4_K` is not a downgrade from fp8; it is the first entry in that list the backend
can actually multiply.

The list of files comes from the same header scan the video tab uses, which matters for
more than convenience: `-M convert` takes **a separate flag per component** and prefixes
every tensor name according to which one you used — `--diffusion-model` prepends
`model.diffusion_model.`, `--vae` prepends `vae.`. Convert a VAE through the diffusion
slot and you get a perfectly valid file whose every tensor is misnamed, which fails
minutes into a later run rather than at conversion time. The page never picks that flag;
the server derives it from what the file turned out to be. Components with no `-M
convert` flag at all — an H3 LLM, a CLIP-Vision tower — are left off the list rather than
offered with a footnote, and LoRAs are excluded because quantizing a low-rank delta is
how you lose the effect you downloaded it for.

The size shown before you press the button is an estimate, computed by transcribing
sd.cpp's own rule for which tensors get quantized (`ModelLoader::tensor_should_be_converted`)
over the tensor index in the file's header. A surprising amount of a transformer is left
alone — every bias, every norm scale, every embedding — so an estimate that assumed
otherwise would under-predict by enough to matter. Measured against real conversions it
lands within 0.01%.

**It runs single-threaded on purpose.** sd.cpp's streaming converter starts one worker
per physical core and has them all call `ModelLoader::load_tensor` on a single shared
loader, which re-enters `load_tensors` → `process_model_files` and mutates that loader's
own state with no lock. At its default thread count it aborts inside glibc — `double
free or corruption (out)` — on roughly one run in three. A crash would be survivable, but
the corruption is in the loader's bookkeeping while other threads read through the
mappings it is remaking, so the same race can put wrong bytes in the output instead of
aborting, and a silently-wrong 8 GB model is the worst thing this could ship. The price
is measured: 254 MB of VAE takes 2.07 s serialized against 0.53 s at four threads.

### Postprocessing

Ten sliders live in the **viewer** — press **View & adjust**, or click the picture.
They work like a photo editor rather than a generation setting: drag one and the image
re-filters under the cursor. There is no diffusion involved — the server re-runs the GPU
filters on the unfiltered image it already retained — so a re-tune costs milliseconds
against the minutes a regenerate would, and it stays lossless however many times you do
it.

| | |
|---|---|
| **Tone** | **Lightness** overall brightness, about ±1.5 stops. **Contrast** steepens or flattens the curve about mid-grey. **Highlights** and **Shadows** move one end of the range and leave the other alone — pull Highlights down to recover a blown sky, push Shadows up to open the dark half. |
| **Colour** | **Saturation** (-1 is exactly greyscale, +1 is double). **Temperature** blue↔yellow and **Tint** green↔magenta, scaled so black stays black. **Hue** rotates every colour, ±180°. |
| **Detail** | **Clarity** and **Sharpness**, described below. |

They are in the viewer because that is the only place you can see what they do. The two
detail filters work at or near the pixel level, and the result column shows a fitted
preview a few hundred pixels across; a sharpness setting chosen there is a guess. The
viewer opens with the whole frame fitted, which is the right view for a grade and for
clarity, and **1:1** switches to true pixels, which is the right view for sharpness —
drag the image to move around it. Only the **×** and Escape close it, so a stray click
while you are working can't dismiss the thing you are working on. **Reset** puts every
slider back to where this image started, and a double-click does the same for one.

The eight colour knobs are one GPU pass between the upscale and the detail filters, so
moving eight of them costs exactly what moving one does. They are all 0 by default and
0 is exactly off — the pass is skipped outright rather than run as an identity, so an
ungraded image is byte-for-byte what came out of the upscaler.

**Clarity** runs **-1 to 1**. Positive is a wide unsharp mask on brightness: it moves
the mid frequencies that read as contrast and depth, and leaves fine detail alone.
Reach for it when the image looks flat. Depth is the one thing refine *can't* give you
— a low-strength pass only ever touches fine texture, and raising strength to reach the
coarse structure re-composes the picture instead. Negative runs the same filter
backwards, subtracting that band instead of adding it, which flattens mid-scale
structure into something softer — the same move as negative Clarity in a photo editor,
and the right one when a render came out overcooked.

**Sharpness** is FSR's RCAS, working the fine band; its limiter rules out ringing, so
the top of the range is usable. If the image looks gritty, sharpness is amplifying it —
try more clarity and less sharpness. 1.0 is the reference filter at full strength and
there is nothing above it: FSR parameterizes sharpness as `exp2(-stops)`, and a lobe
bigger than the limiter permits doesn't sharpen harder, it inverts the filter.

Both filters size their working band from the **image**, not from the pixel grid, so a
setting means the same thing before and after a hi-res pass — clarity's radii are a
fixed fraction of the longest side, and RCAS samples its cross further out on a large
image (one pixel at 1536, two at 3072). Without that, a filter with a fixed footprint
quietly loses its grip as the picture grows: at 3072 a 3×3 kernel is grading a band
three times finer, relative to the subject, than the same kernel at 1024.

0.0 is off for both, and off is exact: neither filter touches a pixel at zero. Both
default to off, so the first image you see is what the model and the upscaler actually
produced — everything on top of it is a choice you made and can take back.

Clarity and Sharpness reset to the model's values on every new image, including after a
**Refine** or **Hi-res**. That is deliberate: an escalation replaces the pixels, so
whatever sharpening looked right a moment ago is stale by definition, and inheriting it
would hide what the pass actually did under a filter chosen for the previous image.
Dialling it back in costs a drag, not a render.

**The eight colour sliders survive Refine and Hi-res**, which is the other half of the
same argument: +0.2 saturation means the same thing at any size, so there is nothing
stale about it, and losing a grade every time you climb a rung would be a reason not to
climb it. It is re-applied to the new pixels rather than stacked on them — the escalation
diffuses the ungraded image, as everything here does. A fresh generate, an
image-to-image, or reopening a work item all start from neutral again.

A different config file can be passed as the first argument:
`cargo run --release -- /path/to/models.toml`.

## Command-line tools

Three subcommands, all of which skip the server. The last two touch only the GPU
post-process — no sd.cpp, no checkpoint, no model weights — which makes them the quick
way to check the Vulkan side in isolation.

```sh
rustimage doctor [models.toml]      # run the startup checks and report, without starting
rustimage fsr in.png out.png [upscale=2.0] [sharpness=0.35] [clarity=0.0]  # no colour knobs: UI only
rustimage fsr-selftest [out.png]    # synthesize patterns and check the filters
```

From a source checkout, put them after `--`: `cargo run --release -- doctor`.

`fsr-selftest` verifies the output dimensions, asserts that every filter at 0 really is
a byte-exact copy (the promise made further up), checks each of the eight colour knobs
one at a time — that it reaches the GPU at all, and that lightness, contrast, saturation
and temperature run in the direction they claim — and then measures how much each detail
filter moves each spatial frequency, at three image sizes, because those two size their
band from the image rather than the pixel grid and that is the claim worth checking. It
needs a Vulkan device and nothing else.

## API

- `GET /` — the web UI.
- `GET /templates` (alias: `GET /models`) — JSON list of the registry, templates and
  discovered checkpoints alike. Built fresh on every call, so a newly-dropped checkpoint
  appears without a restart. Each entry carries `name`, `description`, `discovered`,
  `exists`, `base_width` / `base_height`, `out_width` / `out_height`, and the profile's
  `steps`, `cfg_scale`, `img2img_cfg_scale`, `sampler`, `refine_steps`,
  `refine_strength`, `hires_steps`, `hires_strength`, `source_steps`,
  `source_strength`, `hires_max_side`, `sharpness`, `clarity` — which is
  what lets the UI pre-fill its controls when you pick a model. `hires_max_side` is
  published because the Image-to-image tab's working-size control starts there, and a
  second answer to "how large can this card afford to diffuse" computed in JavaScript
  would be a second answer free to disagree. `img2img_cfg_scale` is
  `null` when the profile doesn't set one; that is deliberately distinguishable from the
  same number as `cfg_scale`, because unset *follows* the base CFG rather than matching
  it once.
- `POST /generate` — only `prompt` and `template` are required:
  ```json
  { "prompt": "…", "template": "sdxl-turbo", "negative": null, "seed": -1 }
  ```
  Every generation setting may also be overridden per-request; each is optional, and
  omitting one uses the model's profile value:
  ```json
  { "steps": 20, "cfg_scale": 5.0, "img2img_cfg_scale": 2.5, "sampler": "euler",
    "width": 768, "height": 768,
    "refine_steps": 2, "refine_strength": 0.3, "hires_steps": 8, "hires_strength": 0.3,
    "sharpness": 0.35, "clarity": 0.0 }
  ```
  The overrides are applied to the profile *before* it is retained with the image, so a
  later `/refine` or `/hires` starts from the settings that generated it — the four
  escalation knobs can then be moved per pass, see below. Omitting
  `img2img_cfg_scale` leaves it *unset* rather than pinning it, so the escalations keep
  following whatever `cfg_scale` this request used. `sharpness` and
  `clarity` are the exception, because they aren't generation settings: they set the
  image's starting filters, `/adjust` moves them from there, and every later pass resets
  them. The web UI leaves both out for that reason. The eight colour knobs aren't here
  at all — they are not per-model, so they start at 0 on every image and only `/adjust`
  sets them. `seed` defaults to `-1` (pick one).
  Response:
  ```json
  { "image_base64": "…", "width": 1536, "height": 1536, "seed": 12345,
    "refine_passes": 0, "hires_passes": 0,
    "image_id": 3, "work_id": "1758104523000-12345",
    "can_refine": true, "can_hires": true,
    "filters": { "sharpness": 0.35, "clarity": 0.0, "lightness": 0.0, "contrast": 0.0,
                 "highlights": 0.0, "shadows": 0.0, "saturation": 0.0, "hue": 0.0,
                 "temperature": 0.0, "tint": 0.0 },
    "filters_start": { "sharpness": 0.35, "clarity": 0.0, "…": 0.0 },
    "timings": { "gen_ms": 0, "fsr_ms": 0, "refine_ms": 0, "hires_ms": 0 } }
  ```
  `image_id` addresses the retained intermediates; `can_refine` / `can_hires` say which
  escalations that image can still take; `filters` reports all ten display filters as
  actually applied, which is what the UI's sliders reflect, and `filters_start` reports
  where they would start on a new image from the same model — the two differ after an
  escalation, which carries the colour grade across, and `filters_start` is what the
  viewer's **Reset** goes back to. `work_id` is the archived
  generation this descends from — it outlives the process, unlike `image_id`, and is
  `null` only if the archive write failed.
- `POST /upload` — the one route with a **binary body**: a PNG, sent as-is with
  `Content-Type: image/png`. No multipart, no base64 — `Request.body` is bytes
  already, and skipping the envelope is a third off the wire. It lands in
  `work/output/` under a minted stem and answers
  `{ "stem": "…", "width": 1024, "height": 768 }`. The stem is minted rather than
  taken from the filename because `valid_id` — the traversal guard for every
  `/outputs/{stem}` route — is a whitelist of alphanumerics and `-`, which almost
  no real filename satisfies. PNG only: the page re-encodes whatever you gave it in
  a canvas first, so the format support tracks your browser's rather than a Rust
  crate's feature list. Answered on the connection thread, and it is the one path
  `http.rs` gives a large body limit (32 MiB; anything over is a `413`).
- `POST /img2img` — one img2img pass over an image already in `work/output/`.
  `prompt`, `template` and `source` are required; the rest are the same per-request
  overrides `/generate` takes, minus `width`/`height` (sd.cpp derives the size from
  the init image), plus:
  ```json
  { "prompt": "…", "template": "sdxl-turbo", "source": "1758104523000-0-import",
    "source_steps": 12, "source_strength": 0.55, "max_side": 2048 }
  ```
  `max_side` is a ceiling — a source already inside it is used untouched, and a
  larger one is scaled down (Lanczos3) first; omitted, it is the model's
  `hires_max_side`. The source is validated on the connection thread, so a stale
  pick is an immediate `400` rather than one that arrives after the render in
  flight. Answers in `/generate`'s shape, so the escalations follow it unchanged,
  and the result is archived as a work item with the source stem recorded on it.
- `POST /refine` — `{ "image_id": 3 }`. img2img over the retained image at its current
  size, no rescale.
- `POST /hires` — `{ "image_id": 3, "upscale": 2 }`. FSR-upscale toward
  `upscale ×` the current size (1–4, default 1), img2img at up to `hires_max_side`, then
  a final FSR upscale if the target is beyond that budget.

  Both return the same shape as `/generate` and can be chained in either order, any
  number of times; `/hires` stops being available once the image is past
  `hires_max_side`, `/refine` does not.

  Both also accept the knobs that describe the pass itself, which override what the
  image was generated with — this is what lets you render once and then try the same
  image at several refine strengths:
  ```json
  { "model": "sd_xl_base_1.0", "refine_steps": 4, "refine_strength": 0.35,
    "hires_steps": 8, "hires_strength": 0.3, "img2img_cfg_scale": 2.5 }
  ```
  `model` runs the pass on a *different* checkpoint — a registry name from
  `/models`, or absent/empty for the one the image was made with. It belongs here
  for the same reason the others do: it describes a pass that hasn't happened yet.
  The swap is wholesale, so the named model's refine/hi-res steps, strengths,
  img2img CFG, `hires_max_side`, `weight_type` and `extra_args` all come with it,
  and the explicit fields in this body are then applied on top. The profile the
  pass actually ran with is what is retained, so `can_refine`, `can_hires` and the
  filter starting points in the reply describe the model that ran.

  Each is optional and an omitted one keeps the retained value, so a bare
  `{ "image_id": 3 }` behaves exactly as it always has. `img2img_cfg_scale` is the one
  field where sending `null` differs from leaving it out: `null` *unsets* it, so the
  pass goes back to following `cfg_scale`. The base settings — `steps`, `cfg_scale`,
  `sampler`, `width`, `height` — are deliberately not accepted here: they describe a
  txt2img that has already run, and nothing they could say would change the image in
  hand. Change those and press **Generate**.
- `POST /adjust` — `{ "image_id": 3, "sharpness": 0.5, "saturation": -0.2 }`. Re-runs
  the display filters on the retained image. Every filter named in the `filters` object
  above is accepted here, flat; all are optional, and an omitted one keeps its current
  value, so adjusting one knob is a one-field request. No diffusion runs, so this
  returns in milliseconds — it is what the viewer's sliders call, and it is safe to call
  repeatedly because the image it filters is always the unfiltered one. The new values
  belong to that image and last until the next pass over it — `/refine` and `/hires`
  reset `sharpness` and `clarity` to the model's values and carry the eight colour knobs
  across. Same response shape as `/generate`, with the time reported in `fsr_ms`.
- `POST /save-output` — `{ "image_id": 3 }`. Copies the image on screen, filters and
  all, into `work/output/`. Returns `{ "path": "work/output/…png" }`.
- `GET /workitems` — every archived generation, newest first. Each carries `id`,
  `created_ms`, `prompt`, `negative`, `seed`, `template`, `width` / `height`,
  `thumb_width` / `thumb_height`, `source` (the `work/output/` stem an
  image-to-image generation was diffused from, `null` for a txt2img), and a
  `profile` object holding the full resolved
  settings the image was generated with — per-request overrides already applied, and
  `img2img_cfg_scale` still `null` if it was left unset. That is what lets the UI
  restore an item's controls without a second request.
- `GET /workitems/{id}/thumb.png` — the thumbnail. An id is digits and `-` only;
  anything else is `404`.
- `POST /workitems/open` — `{ "id": "1758104523000-12345" }`. Copies the archived image
  back into the working set and returns the `/generate` shape with a fresh `image_id`,
  so the escalations and the sliders work on it immediately. No diffusion runs.
- `POST /workitems/delete` — `{ "ids": ["…", "…"] }`. Returns `{ "deleted": 2 }`.
- `GET /progress` — what the worker is doing right now:
  `{ "busy": true, "label": "hi-res", "phase": "sampling", "step": 7, "steps": 20,
  "rate": "1.23s/it" }`, or `{ "busy": false }` when nothing is running. `phase` is
  `loading` while sd-cli reads weights and `sampling` once it is denoising — worth
  distinguishing, because a model that streams from RAM can spend minutes on the first
  and "step 0 of 20" is the wrong thing to show for all of them. `rate` is sd-cli's own
  text, unit and all.
- `POST /cancel` — stops the run in flight, returning `{ "cancelled": true }` if there
  was one. This kills the `sd-cli` process, which is the only cancellation it offers;
  the pass was writing to a scratch file that is cleaned up on failure either way, so
  the image on screen is never a half-written one. The cancelled request itself comes
  back as an error whose message ends in `cancelled by request`.
- `GET /outputs` — every image in `work/output/`, newest first: `stem`, `created_ms`,
  `width` / `height`, `prompt`, `seed`, `template`. Uploads are in here too, with an
  empty prompt and `template: "(imported)"` — one pool, because at the moment you are
  choosing a source image the difference is not interesting. Keyed by filename **stem**, not by
  work item id — several saves come out of one generation (`{id}-s{step}`, plus a `-2`
  suffix when that name is taken), so only the filename is unique. A PNG whose sidecar
  has gone missing is still listed with zeroed dimensions and an empty prompt: the file
  on disk is the thing you saved, and a picker that hid it would be lying.
- `GET /outputs/{stem}/thumb.png` — generated on the first request and cached, because
  output PNGs are full resolution and everything saved before thumbnails existed has to
  appear too.
- `GET /video-models` — every video model: the ones found by scanning `models/video/`
  plus the `[[video]]` table, built fresh per call so a finished download shows up
  without a restart. Each entry is the config verbatim plus what the
  page would otherwise have to re-derive from `family`: `size_multiple`, `frame_grid`
  (`{multiple, offset, min, round_up}`), `forced_fps`, `is_moe`, `missing` (the component
  files not on disk) and `duration`. Served rather than reimplemented in the page on
  purpose — the page rounds the frame count to show a duration and the command builder
  rounds it to run the model, and two implementations of `4k+1` are how those start
  disagreeing.
- `GET /convert-models` — `{types, files}` for the Convert tab. `types` is the weight
  types `--type` accepts, each with its real bits-per-weight (`q4_K` is 4.5, `q6_K` is
  6.5625 — not the round numbers the names suggest); `files` is every model file either
  models dir holds that `-M convert` can be pointed at, each with the role read from its
  header, the `sd-cli` flag that role implies, its dominant stored type, and a predicted
  output size **per type**. Served rather than computed in the page for the same reason
  `frame_grid` is: the rule for what gets quantized is sd.cpp's, and a second copy of it
  in JavaScript would be a second copy free to disagree.
- `POST /convert` — `{source, type}`. `source` must be one of the ids `/convert-models`
  published; that is the traversal guard, and it is a whitelist in the same spirit as
  `workitems::valid_id` — the client chooses from what the server offered, it never names
  a path. Validation happens on the connection thread so a bad type or an existing output
  comes back as a 400 immediately rather than after waiting out whatever render is in
  flight. The conversion itself queues on the worker: no GPU is involved, but it
  saturates the cores and the disk for minutes and must not do that underneath a render.
  Going through the worker is also what gives it `/progress` and `/cancel` for free.
- `POST /video` — `prompt` and `model` are required; everything else optional:
  ```json
  { "prompt": "…", "model": "wan2.2-ti2v-5b", "negative": null, "seed": -1,
    "source": "1758104523000-12345-s3",
    "width": 832, "height": 480, "frames": 33, "fps": 24,
    "steps": 20, "cfg_scale": 6.0, "high_noise_steps": 8, "high_noise_cfg_scale": 3.5,
    "flow_shift": 3.0, "sampler": "euler", "vae_tiling": false }
  ```
  `source` is a `work/output/` stem to use as the first frame; absent or empty runs plain
  text-to-video. `negative` absent inherits the model's (Wan's own published one);
  `negative: ""` means no negative at all. The overrides land on a *clone* of the config
  entry, which is then stored in the clip's sidecar, so a clip is reproducible from its
  own record. Takes **minutes** — poll `/progress`. Response:
  ```json
  { "clip": { "id": "…", "frames": 33, "fps": 24, "width": 832, "height": 480, … },
    "gen_ms": 214000 }
  ```
  `clip.width`, `height` and `frames` are measured from the frames on disk rather than
  echoed from the request, because sd.cpp aligns a size up and a frame count down and
  only warns about it.
- `GET /videos` — every archived clip, newest first; each carries its full sidecar
  including the resolved model under `video`.
- `GET /videos/{id}/thumb.png` — the first frame, `THUMB_HEIGHT` tall.
- `GET /videos/{id}/frames/{n}.png` — one frame, numbered from 0. The page fetches all of
  them and plays the sequence on a canvas. A non-numeric `{n}` is `404`, not the
  thumbnail.
- `POST /videos/enlarge` — `{ "id": "…", "upscale": 2.0, "sharpness": 0.0, "clarity": 0.0 }`.
  FSR over every frame into `work/output/{id}-x{factor}/`. Returns
  `{ "dir": "…", "frames": 33, "width": 1664, "height": 960, "ms": 1820 }`. Filters only
  — it never re-diffuses, for the reason under "Video" above.
- `POST /videos/delete` — `{ "ids": ["…"] }`. Removes each clip's whole directory.

The archive listing, the thumbnail and the delete never touch the GPU, so they answer
immediately even while a render is in flight. `/progress` and `/cancel` do the same for
a sharper reason: they are *about* the render in flight, so answering them through the
job queue would mean progress that arrives only once the render has finished, and a Stop
that becomes possible only when there is nothing left to stop. The whole video family
splits the same way: `/video` and `/videos/enlarge` are GPU work and queue, and
everything else about a clip is filesystem-only and doesn't — which matters most here,
since a clip holds the worker for minutes and the frame player is fetching 33 PNGs.

Errors are `{"error": "…"}`. Anything addressed by `image_id` fails with `400` — the id
has expired (only the last 8 results stay warm, and the scratch half of `work/` is
cleared on restart), or that pass isn't available for the image; either way it's
client-side staleness, and the image is left intact to retry. Since every generation is
archived, an expired id is now recovered by reopening the work item rather than by
regenerating. A failed `/generate` is `500`, malformed JSON is `400`, and `503` means
the GPU worker is gone.

## Architecture notes

- **Single GPU worker thread.** Both sd.cpp and the wgpu FSR passes use the GPU, so all
  jobs are serialized through one worker (an mpsc queue) to avoid VRAM thrash. HTTP
  connection threads enqueue a job and wait for the reply.
- **Shell-out, not FFI.** The server runs `sd-cli` as a subprocess per request — clean
  isolation, easy debugging, and it keeps everything Python-free.
- **Layout:**
  ```
  src/main.rs      server + routing + single GPU worker + CLI subcommands
  src/http.rs      minimal std-only HTTP/1.1
  src/config.rs    models.toml -> typed profiles (templates + discovery)
  src/discover.rs  identify video model files by their tensor headers
  src/convert.rs   requantize a model file (-M convert) — types, size estimate, slots
  src/preflight.rs startup environment checks, shared with `doctor`
  src/sd.rs        sd-cli subprocess wrapper (txt2img + img2img + vid_gen)
  src/pipeline.rs  orchestrate sd -> fsr -> escalations, with timings + result cache
  src/workitems.rs durable storage: work/workitems/ + work/output/
  src/videos.rs    durable storage for clips: work/videos/
  src/fsr/         wgpu compute (upscale.wgsl = EASU, grade.wgsl = colour,
                   contrast.wgsl = clarity, sharpen.wgsl = RCAS)
  web/index.html   frontend
  models.toml      config + model profiles + the [[video]] overrides
  models/video/    video components, scanned recursively and identified by header
  work/            scratch, cleared at startup
  work/workitems/  every generation + its settings — kept
  work/output/     what you pressed "Save to output" on — kept
  work/videos/     one directory per clip: its frames, a thumbnail, its settings
  ```
- **The archive is copy-in, copy-out.** Nothing in `work/workitems/` is ever opened in
  place: a generation is copied in, and reopening one copies it back out to a scratch
  name. An escalation deletes the files of the step it supersedes, so an archive entry
  used directly would be destroyed by the first refine run on it.
- **`work/videos/` is the exception, and for a reason that doesn't undermine the rule.**
  A clip is written straight into the archive because it has no scratch stage: one pass
  writes it and nothing afterwards does anything but read it, so there is no escalation
  to delete it out from under itself — and copying 23 MB of frames would buy nothing.
  What it does need is the *failure* half, and that is a guard which sweeps a
  half-written directory unless the pass reaches the end. Cancelling mid-clip is the
  common path here, not the rare one.

## Roadmap

- Keep the model resident via the sd.cpp C API (FFI) instead of per-request subprocess,
  if warm-start latency matters.
- **Refine a clip with a VACE pass.** `--control-video` takes a directory of frames,
  which is exactly what a generation already writes, so this is a `family` entry and a
  second `SdRunner::video` call — no decode, no ffmpeg, and the only honest way to add
  detail to a finished clip.
- **A downloadable single-file clip.** The cheapest route that keeps an encoder out of
  Rust is the browser's own `VideoEncoder` (WebCodecs): Chrome and Firefox ship VP9/AV1
  encoders, and hand-rolled WebM is a few hundred lines of JavaScript. sd-cli can mux one
  itself, but not in the same run that writes the frames, and the frames are what the
  escalations need.
- **Ken Burns from a still.** EASU samples with sub-pixel accuracy, so a pan and zoom
  across any `work/output/` image is a scale and an offset in a shader that is already
  written and already tested — motion at zero diffusion cost and with no model at all.
