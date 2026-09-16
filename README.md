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
prompt ──▶ sd-cli (Vulkan, 768px default) ──▶ EASU upscale ──▶ clarity + RCAS ──▶ PNG
                                                    ▲
    on demand, from the buttons above the result:   ├─▶ hi-res   enlarge, then img2img
    (either order, repeatable)                      └─▶ refine   img2img in place, no rescale
```

Both escalations read the *unfiltered* image and write a new one; clarity and RCAS
run once on the way out. So neither filter compounds, and no pass ever diffuses
over pixels an earlier pass already sharpened.

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

All three run on the same AMD GPU sd.cpp uses, with no model weights. `cargo test`
validates the WGSL through naga, since it would otherwise only compile at GPU device
creation — a shader typo would surface as a startup failure on the worker thread.

Each stage is skipped when it would be a no-op — EASU at scale 1.0, contrast at
clarity 0.0 — so `rustimage fsr in.png out.png 1.0 0.5` is an exact sharpen-only pass,
and `rustimage fsr in.png out.png 1.0 0.0 0.0` is a byte-exact copy.

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
| `upscale_factor` | FSR upscale, default `2.0` (768 → 1536) |
| `sharpness` | RCAS sharpness, 0..1, default `0.0` = off (1.0 = RCAS at full strength) — where the slider under the image starts |
| `clarity` | local contrast, -1..1, default `0.0` = off — positive adds mid-frequency depth, negative softens; likewise the slider's starting value |
| `refine_steps` / `refine_strength` | the "Refine image" img2img pass, in place at the current size (0 steps = disabled, and `refine_steps` defaults to 0) |
| `hires_steps` / `hires_strength` | the "Hi-res pass" img2img, after an enlarge (0 steps = disabled, and `hires_steps` defaults to 0) |
| `hires_max_side` | longest side the hi-res pass may diffuse at (default 2048); beyond it, FSR finishes the job. Does not limit refine |
| `hires_flash_attn` | `--diffusion-fa` for the img2img passes when they run large (default true; worth 6× at 2048²) |
| `weight_type` *(opt)* | on-the-fly quantization, e.g. `q8_0`, via sd-cli `--type` |
| `extra_args` *(opt)* | arbitrary flags passed verbatim to sd-cli |

Both denoise strengths default to `0.30` and must be between **0.05 and 0.95** — a
strength of 0 makes the pass a silent no-op, and at exactly 1.0 sd.cpp discards the
init image and the pass degenerates into a plain txt2img. Out-of-range values are
rejected at startup rather than at the first refine.

## Run

```sh
cargo run --release
# [rustimage] 2 model(s) available in /home/you/rustimage/models:
# [rustimage]   • samExtTURBO_v10   [discovered, 6.5 GB]  768x768 → 1536x1536
# [rustimage]   • sd_xl_base_1.0    [discovered, 6.5 GB]  768x768 → 1536x1536
# [rustimage] GPU worker ready on adapter: AMD Radeon RX 7900 XTX (RADV NAVI31)
# [rustimage] GPU post-process: upscale=easu, contrast=clarity, sharpen=rcas
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

### Postprocessing

**Clarity** and **Sharpness** are the two sliders under the image, and they work like a
photo editor rather than a generation setting: drag one and the picture re-filters
under the cursor. There is no diffusion involved — the server re-runs the GPU filters
on the unfiltered image it already retained — so a re-tune costs milliseconds against
the minutes a regenerate would, and it stays lossless however many times you do it.

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

Both sliders reset to the model's values on every new image — a fresh generate, a
refine, a hi-res. That is deliberate: an escalation replaces the pixels, so whatever
sharpening looked right a moment ago is stale by definition, and inheriting it would
hide what the pass actually did under a filter chosen for the previous image. Dialling
it back in costs a drag, not a render.

A different config file can be passed as the first argument:
`cargo run --release -- /path/to/models.toml`.

## Command-line tools

Three subcommands, all of which skip the server. The last two touch only the GPU
post-process — no sd.cpp, no checkpoint, no model weights — which makes them the quick
way to check the Vulkan side in isolation.

```sh
rustimage doctor [models.toml]      # run the startup checks and report, without starting
rustimage fsr in.png out.png [upscale=2.0] [sharpness=0.35] [clarity=0.0]
rustimage fsr-selftest [out.png]    # synthesize a pattern, run FSR, verify dimensions
```

From a source checkout, put them after `--`: `cargo run --release -- doctor`.

## API

- `GET /` — the web UI.
- `GET /templates` (alias: `GET /models`) — JSON list of the registry, templates and
  discovered checkpoints alike. Built fresh on every call, so a newly-dropped checkpoint
  appears without a restart. Each entry carries `name`, `description`, `discovered`,
  `exists`, `base_width` / `base_height`, `out_width` / `out_height`, and the profile's
  `steps`, `cfg_scale`, `sampler`, `refine_steps`, `refine_strength`, `hires_steps`,
  `hires_strength`, `sharpness`, `clarity` — which is what lets the UI pre-fill its
  controls when you pick a model.
- `POST /generate` — only `prompt` and `template` are required:
  ```json
  { "prompt": "…", "template": "sdxl-turbo", "negative": null, "seed": -1 }
  ```
  Every generation setting may also be overridden per-request; each is optional, and
  omitting one uses the model's profile value:
  ```json
  { "steps": 20, "cfg_scale": 5.0, "sampler": "euler", "width": 768, "height": 768,
    "refine_steps": 2, "refine_strength": 0.3, "hires_steps": 8, "hires_strength": 0.3,
    "sharpness": 0.35, "clarity": 0.0 }
  ```
  The overrides are applied to the profile *before* it is retained with the image, so a
  later `/refine` or `/hires` runs with the settings that generated it. `sharpness` and
  `clarity` are the exception, because they aren't generation settings: they set the
  image's starting filters, `/adjust` moves them from there, and every later pass resets
  them. The web UI leaves both out for that reason. `seed` defaults to `-1` (pick one).
  Response:
  ```json
  { "image_base64": "…", "width": 1536, "height": 1536, "seed": 12345,
    "refine_passes": 0, "hires_passes": 0,
    "image_id": 3, "can_refine": true, "can_hires": true,
    "sharpness": 0.35, "clarity": 0.0,
    "timings": { "gen_ms": 0, "fsr_ms": 0, "refine_ms": 0, "hires_ms": 0 } }
  ```
  `image_id` addresses the retained intermediates; `can_refine` / `can_hires` say which
  escalations that image can still take; `sharpness` / `clarity` report the filters
  actually applied, which is what the UI's sliders reflect.
- `POST /refine` — `{ "image_id": 3 }`. img2img over the retained image at its current
  size, no rescale.
- `POST /hires` — `{ "image_id": 3, "upscale": 2 }`. FSR-upscale toward
  `upscale ×` the current size (1–4, default 1), img2img at up to `hires_max_side`, then
  a final FSR upscale if the target is beyond that budget.

  Both return the same shape as `/generate` and can be chained in either order, any
  number of times; `/hires` stops being available once the image is past
  `hires_max_side`, `/refine` does not.
- `POST /adjust` — `{ "image_id": 3, "sharpness": 0.5, "clarity": 0.3 }`. Re-runs the
  display filters on the retained image; both fields are optional, and an omitted one
  keeps its current value. No diffusion runs, so this returns in milliseconds — it is
  what the sliders under the image call, and it is safe to call repeatedly because the
  image it filters is always the unfiltered one. The new values belong to that image
  and last until the next pass over it — `/refine` and `/hires` both reset the filters
  to the model's values. Same response shape as `/generate`, with the time reported in
  `fsr_ms`.

Errors are `{"error": "…"}`. Anything addressed by `image_id` fails with `400` — the id
has expired (only the last 8 results are kept, and `work/` is cleared on restart), or
that pass isn't available for the image; either way it's client-side staleness, and the
image is left intact to retry. A failed `/generate` is `500`, malformed JSON is `400`, and `503` means the GPU
worker is gone.

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
  src/preflight.rs startup environment checks, shared with `doctor`
  src/sd.rs        sd-cli subprocess wrapper (txt2img + img2img)
  src/pipeline.rs  orchestrate sd -> fsr -> escalations, with timings + result cache
  src/fsr/         wgpu compute (upscale.wgsl = EASU, contrast.wgsl = clarity,
                   sharpen.wgsl = RCAS)
  web/index.html   frontend
  models.toml      config + model profiles
  ```

## Roadmap

- Keep the model resident via the sd.cpp C API (FFI) instead of per-request subprocess,
  if warm-start latency matters.
