# rustimage

A Python-free image generator for AMD GPUs on Linux. It generates **small and
distilled** with [stable-diffusion.cpp](https://github.com/leejet/stable-diffusion.cpp)
on the **Vulkan** backend (SDXL-Turbo, 1–4 steps, ~512px), then does the expensive-looking
part cheaply: a **native AMD FidelityFX-style upscale + sharpen** as GPU compute shaders
via [`wgpu`](https://github.com/gfx-rs/wgpu) — no second diffusion pass, no ML upscaler,
no Python anywhere in the build or runtime.

```
prompt ──▶ sd-cli (Vulkan, distilled, 512px) ──▶ FSR upscale+sharpen (wgpu) ──▶ PNG
                                                        │
        on demand, from the buttons above the result:   ├─▶ refine   img2img at base resolution
                                                        └─▶ hi-res   img2img at output resolution (±upscale)
```

Generation stays cheap; quality is escalated afterwards, only on the images worth it.

A tiny std-only HTTP server glues it together and serves a single static HTML page.

## Why not FidelityFX-CLI?

The plan originally called for shelling out to AMD's `FidelityFX-CLI`. That tool is
**Windows-only** (it uses the Windows Imaging Component + Direct3D 11), so on Linux the
post-process is implemented directly as Vulkan compute shaders through `wgpu`:

- `src/fsr/upscale.wgsl` — spatial upscale (the EASU slot). **Currently a Lanczos-2
  resample** as an interim while the full FSR1 EASU directional kernel is ported. The
  server logs `upscale=lanczos2` at startup so the active path is explicit.
- `src/fsr/sharpen.wgsl` — **AMD FidelityFX CAS** (Contrast Adaptive Sharpening), a
  faithful port of the non-scaling kernel.

Both run on the same AMD GPU sd.cpp uses, with no model weights.

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

### 2. Get a distilled checkpoint

Download a single-file `.safetensors` (no conversion needed) into `models/`, e.g.
SDXL-Turbo. Then point `models.toml` at it (see below).

### 3. Smoke-test sd.cpp on its own

Before involving Rust, confirm generation works:

```sh
./build/bin/sd-cli -M img_gen -m models/sdxl-turbo.safetensors \
  -p "a lovely cat" --steps 4 --cfg-scale 1 --sampling-method euler -o test.png
```

## Configure

Edit `models.toml`:

- `[sd].binary` — path to your `sd-cli` (or `sd`) binary.
- `[sd].mode` — `img_gen` for recent sd.cpp; `txt2img` for older builds.
- `[templates.<name>]` — one **model profile** per entry. Each drives both the sd-cli
  command line and the FSR post-process. **Adding a model = a new block + a checkpoint
  file. No code changes.**

Key template fields:

| field | meaning |
|-------|---------|
| `model` | path to the `.safetensors` (relative to project root) |
| `width` / `height` | base generation size (small = fast) |
| `steps`, `cfg_scale`, `sampler` | sd.cpp params (Turbo → few steps, cfg 1, `euler`) |
| `upscale_factor` | FSR upscale (e.g. `2.0`: 512 → 1024) |
| `sharpness` | CAS sharpness, 0..1 |
| `refine_steps` / `refine_strength` | the "Refine image" img2img pass, at base res (0 steps = disabled) |
| `hires_steps` / `hires_strength` | the "Hi-res pass" img2img, at output res (0 steps = disabled) |
| `hires_max_side` | longest side the hi-res pass may diffuse at (default 2048); beyond it, FSR finishes the job |
| `hires_flash_attn` | `--diffusion-fa` for the hi-res pass (default true; worth 6× at 2048²) |
| `weight_type` *(opt)* | on-the-fly quantization, e.g. `q8_0`, via sd-cli `--type` |
| `extra_args` *(opt)* | arbitrary flags passed verbatim to sd-cli |

## Run

```sh
cargo run --release
# [rustimage] GPU worker ready on adapter: AMD Radeon RX 7900 XTX (RADV NAVI31)
# [rustimage] FSR post-process: upscale=lanczos2, sharpen=cas (interim FSR path)
# [rustimage] listening on http://127.0.0.1:8080
```

Open <http://127.0.0.1:8080>, pick a model, type a prompt, Generate. The result shows
output resolution, seed, and per-stage timings (generate / fsr / refine).

Quality is escalated *after* the fact, from the buttons above the result — so you only
pay for it on images worth keeping. The server retains each result's intermediates, so
no step ever recomputes the ones below it:

- **Refine image** — img2img at base resolution (`refine_*`). Cheap; improves the image
  inside SDXL's native range before the FSR upscale.
- **Hi-res pass** — img2img at the *output* resolution (`hires_*`), with an upscale
  factor from 1× to 4×. This is what adds detail the Lanczos+CAS upscale can't invent.
  Past `hires_max_side` the pass diffuses as large as its budget allows and covers the
  rest with a final FSR upscale, so a 4× request stays affordable.

A button only appears when that pass is still available: refining is gone once the image
has been refined or hi-res'd, and the hi-res pass is gone once the image is larger than
`hires_max_side`.

A different config file can be passed as the first argument:
`cargo run --release -- /path/to/models.toml`.

## API

- `GET /` — the web UI.
- `GET /templates` — JSON list of configured profiles.
- `POST /generate` — request:
  ```json
  { "prompt": "…", "template": "sdxl-turbo", "negative": null, "seed": -1 }
  ```
  response:
  ```json
  { "image_base64": "…", "width": 1536, "height": 1536, "seed": 12345,
    "refined": false, "hires_passes": 0,
    "image_id": 3, "can_refine": true, "can_hires": true,
    "timings": { "gen_ms": 0, "fsr_ms": 0, "refine_ms": 0, "hires_ms": 0 } }
  ```
  `image_id` addresses the retained intermediates; `can_refine` / `can_hires` say which
  escalations that image can still take.
- `POST /refine` — `{ "image_id": 3 }`. img2img at base resolution, then re-upscale.
- `POST /hires` — `{ "image_id": 3, "upscale": 2 }`. FSR-upscale toward
  `upscale ×` the current size (1–4), img2img at up to `hires_max_side`, then a final
  FSR upscale if the target is beyond that budget.

  Both return the same shape as `/generate` and can be chained (refine, then hi-res).
  `400` if the id has expired — only the last 8 results are kept, and `work/` is cleared
  on restart — or if that pass isn't available for the image.

## Architecture notes

- **Single GPU worker thread.** Both sd.cpp and the wgpu FSR passes use the GPU, so all
  jobs are serialized through one worker (an mpsc queue) to avoid VRAM thrash. HTTP
  connection threads enqueue a job and wait for the reply.
- **Shell-out, not FFI.** The server runs `sd-cli` as a subprocess per request — clean
  isolation, easy debugging, and it keeps everything Python-free.
- **Layout:**
  ```
  src/main.rs      server + routing + single GPU worker
  src/http.rs      minimal std-only HTTP/1.1
  src/config.rs    models.toml -> typed profiles
  src/sd.rs        sd-cli subprocess wrapper (txt2img + img2img)
  src/pipeline.rs  orchestrate sd -> fsr -> refine, with timings + refine cache
  src/fsr/         wgpu compute (upscale.wgsl + sharpen.wgsl)
  web/index.html   frontend
  models.toml      config + model profiles
  ```

## Roadmap

- Port the full FSR1 **EASU** directional-upscale kernel into `upscale.wgsl` (replacing
  the Lanczos interim); RCAS/CAS in `sharpen.wgsl` stays as-is.
- Keep the model resident via the sd.cpp C API (FFI) instead of per-request subprocess,
  if warm-start latency matters.
