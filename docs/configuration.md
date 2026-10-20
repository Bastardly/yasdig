# Configuration

Where yasdig keeps its files, how it finds models, and every setting it reads. The app's own Settings tab edits most of this; this page is for the rest.

## Models

### Get a model

**The Get models tab is the easy way.** It lists one recommended set per family —
the model and every file it needs beside it, with sizes — marks what you already
have, and downloads the rest into the right folders with progress, cancel and
resume. A part you already have under another name is recognised by its header and
not downloaded again. It runs `curl`, which ships with Windows 10 and later and with
most Linux distributions (`doctor` says whether it is found), and it fetches only
the URLs compiled into the program (`src/catalog.toml`) — all from ungated Hugging
Face repositories, so no account or token is needed. **Each model has its own
licence** — some forbid commercial use or are for research only, and a GGUF
repackage carries the original's — so check it before using what you make; every
card links the pages its files come from. Its **Upscalers** cards fetch upscaler models
into the upscaler folder the same way. Downloads run beside renders,
not in their queue. **Help → Intro to models** explains what the files are and where
to find more.

The rest of this section is the manual route, and what to do for a model the tab
doesn't list.

Put model files anywhere under `models/`. Every file is **identified by reading its
header** — the tensor names inside it — so neither the file name nor the folder
matters, and a file that isn't an image model (an LTX video model, a loose VAE) is
left out of the model select and named by `doctor` instead. No config edit is needed
to add a model.

A tidy layout, and the one ComfyUI uses, so an existing ComfyUI `models` folder
works as-is:

```
models/
  checkpoints/         SD 1.x / 2.x / SDXL single-file checkpoints
  diffusion_models/    a model that comes without its text encoder and VAE (Flux, Z-Image, …)
  text_encoders/       their text encoders
  vae/                 their VAEs
  loras/   upscalers/
```

**SD 1.x, SD 2.x, SDXL** — one `.safetensors` / `.ckpt` / `.gguf` file, no
conversion needed. Distilled checkpoints (SDXL-Turbo, LCM) are the fastest, but want
few steps and CFG 1: set that once with *Save as defaults for this model* (see
[Per-model settings](guide.md#per-model-settings)).

**Z-Image** — three files, which are found and put together for you:

| file | where to get it |
|---|---|
| the model — Z-Image-Turbo | [safetensors](https://huggingface.co/Comfy-Org/z_image_turbo/tree/main/split_files/diffusion_models) or [GGUF](https://huggingface.co/leejet/Z-Image-Turbo-GGUF/tree/main) |
| the text encoder — Qwen3-4B | [safetensors](https://huggingface.co/Comfy-Org/z_image_turbo/tree/main/split_files/text_encoders) or [GGUF](https://huggingface.co/unsloth/Qwen3-4B-Instruct-2507-GGUF/tree/main) |
| the VAE — Flux's `ae.safetensors` | [FLUX.1-schnell](https://huggingface.co/second-state/FLUX.1-schnell-GGUF/tree/main) (Black Forest Labs' own repo asks you to log in; this copy is the same file) |

With only some of them, the model is listed greyed out and `doctor` prints the path
it is waiting for. The bf16 set is ~20 GB and fits a 24 GB card; a GGUF quantization
is the answer on a smaller one. The turbo runs at 8 steps and CFG 1, which is what it
arrives set to; the base Z-Image wants ~28 steps at CFG 4-5, which is a per-model
setting.

The other families follow the same pattern. The sizes below are the picks that fit a
**24 GB card with the model and its encoders resident together**. The model and the
text encoder are both on the card for the whole render, so it is their sum that has to
fit, and a smaller quantization of either is the lever. An fp8 file is a false
economy on Vulkan: no Vulkan device can multiply fp8, so sd.cpp casts it to bf16 and
it needs about twice its size (the [Convert](guide.md#convert) tab turns one into a GGUF).

**Flux.1** (dev, schnell, Krea dev, Kontext) — about 18.5 GB at Q8:

| file | where to get it |
|---|---|
| the model | dev [GGUF](https://huggingface.co/leejet/FLUX.1-dev-gguf/tree/main) (`q8_0`, 12.8 GB), schnell [GGUF](https://huggingface.co/leejet/FLUX.1-schnell-gguf/tree/main), Kontext [GGUF](https://huggingface.co/QuantStack/FLUX.1-Kontext-dev-GGUF/tree/main) |
| CLIP-L | [`clip_l.safetensors`](https://huggingface.co/comfyanonymous/flux_text_encoders/tree/main) |
| T5-XXL | [GGUF](https://huggingface.co/city96/t5-v1_1-xxl-encoder-gguf/tree/main) (`Q8_0`, 5 GB). The fp16 file (9.5 GB) works too, beside a Q4 model |
| the VAE — Flux's `ae.safetensors` | the same one Z-Image uses |

**Flux.2 Klein** — the 4B is about 12 GB, the 9B about 19 GB at Q8. Both are fast
enough to be worth trying first, and both can edit — which the Image tab will be able
to use once it has an edit mode:

| file | where to get it |
|---|---|
| the model | 4B [GGUF](https://huggingface.co/leejet/FLUX.2-klein-4B-GGUF/tree/main) or [safetensors](https://huggingface.co/black-forest-labs/FLUX.2-klein-4B/tree/main); 9B [GGUF](https://huggingface.co/leejet/FLUX.2-klein-9B-GGUF/tree/main) (`Q8_0`, 9.9 GB) |
| the text encoder | 4B: Qwen3-4B, the same file as Z-Image's. 9B: Qwen3-8B, [GGUF](https://huggingface.co/unsloth/Qwen3-8B-GGUF/tree/main) (`Q8_0`) |
| the VAE — Flux.2's | [`flux2-vae.safetensors`](https://huggingface.co/Comfy-Org/vae-text-encorder-for-flux-klein-4b/tree/main/split_files/vae) |

**FLUX.3 Klein** — about 11 GB at Q8. Klein is the distilled release (16 steps at
CFG 1); Klein Base is the undistilled one (20 steps at CFG 3.5), for finetuning and
LoRAs. `doctor` tells them apart. Both take reference images and can edit. Black
Forest Labs' own repos are gated, so these links are leejet's GGUFs:

| file | where to get it |
|---|---|
| the model | Klein [GGUF](https://huggingface.co/leejet/flux-3-image-klein-GGUF/tree/main) (`Q8_0`, 6.2 GB); Klein Base [GGUF](https://huggingface.co/leejet/flux-3-image-klein-base-GGUF/tree/main) (`Q8_0`, 6.1 GB) |
| the text encoder — Qwen3-VL-4B | the same file as Krea 2's, below |
| the VAE — Flux.2's | the same file as Flux.2 Klein's, above |

**Krea 2** — about 15 GB:

| file | where to get it |
|---|---|
| the model — Krea 2 Turbo | [GGUF](https://huggingface.co/realrebelai/KREA-2_GGUFs/tree/main/TURBO) (`Q6_K`, 10.5 GB) |
| the text encoder — Qwen3-VL-4B | [GGUF](https://huggingface.co/Qwen/Qwen3-VL-4B-Instruct-GGUF/tree/main) (`Q8_0`, 4.3 GB; not the `mmproj` file). **Not** Comfy's `qwen3vl_4b_bf16.safetensors`, though sd.cpp's own Krea 2 doc links it: it nests the language half under `model.language_model.`, which sd.cpp can't load, and `doctor` says so |
| the VAE | [`qwen_image_vae.safetensors`](https://huggingface.co/Comfy-Org/Krea-2/tree/main/vae) |

**Qwen-Image** — about 17 GB with these picks; it is a 20B model, the largest here:

| file | where to get it |
|---|---|
| the model | [GGUF](https://huggingface.co/QuantStack/Qwen-Image-GGUF/tree/main) (`Q4_K_S`, 12.1 GB) |
| the text encoder — Qwen2.5-VL-7B | [GGUF](https://huggingface.co/mradermacher/Qwen2.5-VL-7B-Instruct-GGUF/tree/main) (`Q4_K_M`, 4.7 GB) |
| the VAE | [`qwen_image_vae.safetensors`](https://huggingface.co/Comfy-Org/Qwen-Image_ComfyUI/tree/main/split_files/vae) — the same file as Krea 2's |

**Qwen-Image 2.1** — about 18 GB with these picks. A 7B model of its own, not a
Qwen-Image finetune: none of Qwen-Image's files fit it. It renders 40 steps at CFG 6,
or 8 at CFG 1 with the turbo LoRA (the Get models card fetches it, and the Models
panel's **Turbo** switch adds its fixed `--sigmas qwen-image-2.1-turbo` schedule). It
was trained around 2048²; here that is the Hi-res pass's size. Its licence is Qwen's
research licence — read it on the model's page before you use it:

| file | where to get it |
|---|---|
| the model | [GGUF](https://huggingface.co/leejet/Qwen-Image-2.1-GGUF/tree/main) (`Q8_0`, 7.7 GB) |
| the text encoder — Qwen3-VL-8B | [GGUF](https://huggingface.co/Qwen/Qwen3-VL-8B-Instruct-GGUF/tree/main) (`Q8_0`, 8.7 GB; not the `mmproj` file) |
| the VAE | [`qwen_image_2.1_vae_bf16.safetensors`](https://huggingface.co/Comfy-Org/Qwen-Image-2.1/tree/main/vae) — its own; the Wan VAEs don't decode it |
| the turbo LoRA (optional) | [`qwen_image_2.1_turbo_lora_avg_rank_178_bf16.safetensors`](https://huggingface.co/Comfy-Org/Qwen-Image-2.1/tree/main/loras), into `models/loras/` |

**Iris-3B** — about 8 GB. A 3B model that draws in pixel space, so it has no VAE: the
model and its text encoder are all it needs. Its recipe loads it as q8_0
(`weight_type`). Two releases with identical tensors: **Turbo** (`turbo/`, BF16,
6 GB), 4 steps at CFG 1 on the σ list of its `distill.yaml`, and the base (fp32,
12 GB), 100 steps at CFG 3 — 200 network passes a picture, so minutes. A header can't
tell them apart, so the Turbo's settings come with the file the Get models card
downloads (at its own name and size); a Turbo saved under another name starts at the
base's 100 steps — set 4 steps at CFG 1 and *Save as defaults*, though its fixed
schedule only comes with the card's file:

| file | where to get it |
|---|---|
| the model | Turbo: [`turbo/model.safetensors`](https://huggingface.co/speridlabs/iris-3b/tree/main/turbo) (BF16, 6 GB); base: the root [`model.safetensors`](https://huggingface.co/speridlabs/iris-3b/tree/main) (fp32, 12 GB) |
| the text encoder — Qwen3-VL-4B | the same file as Krea 2's, above |

**MOSS-Image** (Base and SFT, which `doctor` can't tell apart: the same tensors) —
about 16 GB with the INT8 model, about 21 GB with BF16. It runs 50 steps at CFG 3, its model card's settings, so a picture
takes longer than the 5B size suggests. It is the one family with a fourth file: sd.cpp
requires Qwen3.5's own `tokenizer.json`, which is recognised by its contents like the
rest and can go anywhere under `models/`:

| file | where to get it |
|---|---|
| the model | SFT or Base from [Comfy-Org](https://huggingface.co/Comfy-Org/MOSS-Image/tree/main/diffusion_models): `int8_convrot` (5.4 GB, the Get models default — sd.cpp multiplies it as INT8, and the Convert tab leaves it alone) or `bf16` (10.2 GB); or leejet's [GGUF](https://huggingface.co/leejet/MOSS-Image-SFT-GGUF/tree/main) (`Q8_0`, 5.7 GB) |
| the text encoder — Qwen3.5-9B | [GGUF](https://huggingface.co/unsloth/Qwen3.5-9B-GGUF/tree/main) (`Q8_0`, 9.5 GB; not an `mmproj` file) |
| the tokenizer | [`tokenizer.json`](https://huggingface.co/OpenMOSS-Team/MOSS-Image-Base/tree/main/tokenizer) |
| the VAE — Flux.2's | the same file as Flux.2 Klein's, above |

If a model runs out of memory, the error says which memory and when, and offers the
ways out as buttons — chiefly the **Memory** setting in the *Models* fold: *System RAM*
keeps the weights in RAM and leaves the card for the work, *Stream from SSD* re-reads
them from the file when RAM is what ran out. *Save as defaults for this model* keeps the
choice for that model only. Help → Memory explains the three.

### Smoke-test sd.cpp on its own

If a generation fails and you want to rule yasdig out, run sd-cli by hand:

```sh
stable-diffusion.cpp/build-yasdig/bin/sd-cli -M img_gen -m models/checkpoints/sdxl-turbo.safetensors \
  -p "a lovely cat" --steps 4 --cfg-scale 1 --sampling-method euler -o test.png
```


## Configure

**Three files, and the difference between them is who writes them.**

| file | holds | written by |
|---|---|---|
| `startup.toml` | the port and address the page is on | you, by hand; created on first start |
| `models.toml` | what the program needs in order to start: the sd.cpp binary, the directories, per-checkpoint presets | you, by hand |
| `user/settings.toml` | your defaults: steps, CFG, size, the refine and hi-res settings, the default prompt | the **Settings tab**, on every save |
| `user/model-settings.toml` | what one model does differently — a turbo's 6 steps, say | *Save as defaults for this model* and the Settings tab's **Per model** card |

`user/settings.toml` is created on first run, seeded from a `[defaults]` block in
`models.toml` if you still have one. It is rewritten in full every time you press
Save, so it carries no comments — which is exactly why it is a separate file from
the one above, where the comments are most of the content.

There are two ways to get a selectable model, and the first needs no config at
all:

- **Discovery** — any model in the models dir is auto-listed and runs on your
  settings, laid under its family's defaults (see below). Nothing to edit.
- **Templates** — a `[templates.<name>]` block says what is *different* about
  one specific checkpoint (matched by its `model` path) and inherits everything
  it doesn't name. A Turbo preset is three lines: 4 steps, CFG 1, and the file.

Either way, **adding a model never needs a code change.**

### Model families

A model's **family** is read from its file, and it decides two things: which files
sd.cpp is handed, and what the model starts at.

The families, their files and their starting numbers are the table under
[Supported models](../README.md#whats-supported). The SD families start at *your* settings — the
*SD 1.x, SD 2.x and SDXL* section of the Settings tab — and every other family at a
recipe of its own (size, steps, CFG, sampler; euler throughout), from sd.cpp's docs or
the model's card. An architecture this build doesn't run is named by `doctor` ("…
which this build doesn't run yet") rather than offered and then failing minutes into a
render. The model select groups models by family.

Flux.1 dev and schnell are separate families because the file says which it is — dev
carries the distilled guidance and schnell doesn't — and their right step counts are
20 and 4. A Flux LoRA fits both.

**Choosing the separate files.** The Image tab's **Models** fold holds the
*Diffusion model* select and, under it, one select per file that model's family runs
with — the VAE for SD and SDXL; the VAE, CLIP-L and T5-XXL for Flux.1; the VAE and
the text encoder for the rest. Each lists only the files that fit, identified by what
is inside them like everything else here: a Flux VAE is never offered to an SDXL, whose
latents it can't decode, and a text encoder is offered only to the family trained
against it, by kind *and* size — Klein 4B and Krea 2 both want a 4B Qwen, and a
Qwen3-VL in Klein's slot would load without complaint and condition on nonsense.

- On a single-file checkpoint the VAE defaults to **Built into the checkpoint**.
  Picking a file replaces it — the usual reason on SDXL is
  [the fp16-fix VAE](https://huggingface.co/madebyollin/sdxl-vae-fp16-fix), since the
  original overflows at half precision. Drop it in `models/vae/`.
- On a model assembled from parts, each slot starts at the file the scan found and
  can't be emptied, because the model has nothing of its own to fall back on.

The choice travels with the image — Refine and Hi-res decode with the same VAE, a
reopened work item puts it back — and **Save as defaults for this model** remembers
it (see [Per-model settings](guide.md#per-model-settings)). The fold's summary line says what
is picked, so it can stay folded.

**Where a model's numbers come from**, each layer overriding the one before only
where it says something:

1. `user/settings.toml` — the Settings tab.
2. The family's defaults — size, steps, CFG and sampler, for every family except SD
   and SDXL (whose defaults are your settings).
3. A `[templates.<name>]` block in `models.toml`, if you wrote one.
4. `user/model-settings.toml` — see [Per-model settings](guide.md#per-model-settings).

### `models.toml`

- `[sd].binary` — path to your `sd-cli` (or `sd`) binary. Leave it out to use the one
  built into a release executable (unpacked to `bin/`). A checkout sets it.
- `[sd]` and every key in it are optional; the file a release writes on its first run
  sets only the server address.
- `[sd].mode` — `img_gen` for recent sd.cpp; `txt2img` for older builds.
- `[sd].models_dir` — folder scanned for checkpoints (default `models`).
  Recursive, and identified by header rather than extension; the LoRA and
  upscaler folders below are left out, since each has a listing of its own.
- `[sd].lora_dir` — folder scanned for LoRAs (default `loras/` inside the models dir).
  This one *is* recursive, so `models/loras/sdxl/` works. See [LoRAs](guide.md#loras).
- `[sd].upscaler_dir` — folder of ESRGAN upscaler models (default `upscalers/` inside
  the models dir). See [Upscalers](guide.md#upscalers).
- The port and address are not here any more: they are in `startup.toml`, beside this
  file (see [Run](#run)). A `[server]` block left over from before is copied there on
  the first start and ignored after that; `doctor` says so.
- `[templates.<name>]` — a preset for one checkpoint. `model` is the only
  required field; everything else is any of the settings below except
  `prompt`, `negative` and `hires_scale`, which are not properties of a
  checkpoint. `description` sets the label in the UI.

### `user/settings.toml`

Edited from the Settings tab, or by hand. These are the values every model starts
from, and what a template inherits whatever it doesn't name. Tuned for standard
(non-distilled) SDXL — which is why `width`, `height`, `steps`, `cfg_scale`,
`sampler` and `scheduler` reach SD 1.x, 2.x and SDXL only: every other family starts from its own
(see [Model families](#model-families)).

| field | meaning |
|-------|---------|
| `prompt` / `negative` | what the prompt boxes contain when the page loads. The default prompt is `a lovely {animal} with {color} hair, cinematic lighting, sharp, in focus`, drawing from the default variables below. Not per-model — a checkpoint has a house step count, none has an opinion about what you usually want to draw |
| `width` / `height` | base generation size, default `768` each (small = fast) |
| `steps`, `cfg_scale`, `sampler` | sd.cpp params; defaults `30`, `6.0`, `euler` (Turbo → few steps, cfg 1). `sampler` is any of sd.cpp's 21 (`config::SAMPLERS`) |
| `scheduler` | sd.cpp's `--scheduler`, default `auto`: no flag, so sd.cpp picks the model's own (`flux`/`flux2` for those families, `discrete` for SD and SDXL). `ays` is refused outside SD 1.5 and SDXL, the only models it has tables for. The Help tab compares them |
| `memory` | Where the weights live while a pass runs: `auto` (default — sd.cpp's auto-fit places each part on the card, then in RAM, then on disk, by free memory), `ram` (`--params-backend "*=cpu,te=disk"`: every weight in RAM, leaving the card for the work — the fix for a big model out of GPU memory — except the text encoder, which is read from its file, used for the prompt and released, so it holds no RAM while the picture is drawn. sd.cpp keeps those weights in the GPU's pinned host memory, amdgpu's **GTT**, which the kernel caps at half of RAM by default: a model bigger than that — a 17 GB fp16 9B on a 30 GB machine — can't run this way however much RAM is free, and fails with a lost device. The out-of-memory box checks the GTT and offers `disk` instead) or `disk` (`--params-backend diffusion=disk,te=disk`: re-read from the file, the least RAM, slower). Every pass uses it, Refine and Hi-res included. Per model through *Save as defaults for this model*; a `--params-backend` in `extra_args` wins over it |
| `gpu_reserve_gb` | GB of the graphics card kept free while a model draws: `--max-vram -N` on every run, in every Memory mode. Default `0`, sd.cpp's own packing (free memory minus 512 MiB). When the desktop, browser and driver need more slack, RADV stops a run with *"Not enough memory for command submission"* and a lost device; the out-of-memory box offers 3 GB then, and saves it here. Page-level: it describes the machine, not a model |
| `reference_size` | The side, in pixels, each reference image is drawn at: `--ref-image-args vae_input_max_pixels=N²` on a generate with references. Default `512` (the likeness held there on Klein 9B); `0` is sd.cpp's own (each reference resized to the output's area, up to a megapixel — *up* as well as down, so shrinking the files ourselves would be undone). The biggest speed knob with references: on a Q8 Klein 9B at 1024² with four references, 153 s full-size, 54 s at 768 and 33.6 s at 512 (22 s with none). Not applied to Edit, whose first reference is the image being changed. Page-level |
| `vae_conv_direct` | sd-cli's `--conv-direct auto,vae=on` (off: `vae=off`), default **on**: the VAE's convolutions computed directly instead of through an im2col buffer gigabytes long. Klein 9B's decode went from 9.3 s to 0.9 s and Krea 2's from 3.3 s to 1.2 s, the same picture. Page-level |
| `embed_png_settings` | Write the settings that made a picture into the PNG when it is saved to output: A1111's `parameters` text, which Civitai, A1111 and Forge read, plus yasdig's own record of the Image tab's settings. Default **on**; no file paths are ever written. Page-level |
| `video_save` | What a finished clip leaves in `work/output/`: `"file"` (default), an `{id}.avi` (MJPEG, with the soundtrack for MiniMax H3) beside the frames, or `"frames"`, the PNG frames in `work/videos/{id}/` only. *Save to output* in the clip dialog writes a version the same way (`{id}-v{N}.avi` or a frames folder). Page-level |
| `cache_mode` | Step caching for the generate pass: `off` (default), `spectrum` or `adaptive` — UCache on SD 1.x, SD 2.x and SDXL, EasyCache on the DiT families, chosen by the server since each only works on its own kind. Refine and Hi-res always run uncached. The result says how many steps it skipped |
| `upscale_factor` | the enlarge every base render goes through, default `1` (no enlarge: the picture comes out at the size it was drawn) — not a diffusion pass. `2` makes a 768 render 1536. Also a box at the foot of the Image tab's *Generation* panel |
| `upscaler` *(opt)* | what does that enlarge: unset (the default) is FSR's EASU; a file name under the upscaler dir, extension included, is an ESRGAN model. See [Upscalers](guide.md#upscalers) |
| `hires_scale` | where the hi-res pass's enlarge factor starts, default `2.0`, 1 to 4. The select above a finished image and the *Hi-res scale* box in the Hi-res pass fold are the same setting |
| `sharpness` | RCAS sharpness, 0..1, default `0.0` = off (1.0 = RCAS at full strength) — where the viewer's slider starts. The only display filter here besides `clarity`: the nine colour knobs are not per-model and always start neutral |
| `clarity` | local contrast, -1..1, default `0.0` = off — positive adds mid-frequency depth, negative softens; likewise the slider's starting value |
| `refine_steps` / `refine_strength` | the "Refine image" img2img pass, in place at the current size. Defaults `4` / `0.30`; steps is a floor, raised to `steps × strength`, and `0` turns the pass off |
| `refine_cfg_scale` *(opt)* | CFG for the refine pass alone; unset follows `cfg_scale` |
| `source_strength` | the one pass over an [*Image to image*](guide.md#image-to-image) source. Default `0.55` — far higher than the two escalations, because this one is asked to make a picture *out of* one rather than to finish one it made. The pass runs `steps` at `cfg_scale`, like the txt2img would |
| `hires_steps` / `hires_strength` | the "Hi-res pass" img2img, after an enlarge. Defaults `12` / `0.30`; a floor like `refine_steps`, and `0` turns the pass off |
| `hires_cfg_scale` *(opt)* | the same, for the hi-res pass. Two knobs rather than one because the two passes work on different pixels at different sizes |
| `hires_max_side` | longest side the hi-res pass may diffuse at (default 2048); beyond it, FSR finishes the job. Does not limit refine. Also where *Image to image*'s working size starts |
| `hires_flash_attn` | `--flash-attn auto,diffusion=on` for the img2img passes when they run large (default true; worth 6× at 2048²) |
| `weight_type` *(opt)* | on-the-fly quantization, e.g. `q8_0`, via sd-cli `--type` |
| `extra_args` *(opt)* | arbitrary flags passed verbatim to sd-cli |
| `clear_workitems_on_close` | when **Close server** is pressed, delete every work item on the Current shelf. Default `false`. Favorites are never touched |
| `clear_outputs_on_close` | the same for `work/output/` — saved images and uploads. Default `false`. Favorite outputs are never touched |
| `clear_videos_on_close` | the same for `work/videos/`, and the enlarged frames made from each clip. Default `false` |
| `max_image_side` | the longest side Upscale, Hi-res or a generate's output upscale may produce (default 8192, 1024–32768). Past it, the pass is refused with a message naming this setting and the GPU's own limit, which nothing can raise (16384 on a 7900 XTX). Larger images are slow to show in the page |
| `[variables]` | named word lists a prompt draws from — `animal = ["crab", "horse"]` makes `{animal}` in any prompt one of them. Defaults: `animal = ["cat", "fox", "bear"]`, `color = ["red", "blue", "golden"]`; a file with no table gets these, and a table emptied in Settings stays empty. See [Prompt variables](guide.md#prompt-variables) |

All three denoise strengths must be between **0.05 and 0.95** — a
strength of 0 makes the pass a silent no-op, and at exactly 1.0 sd.cpp discards the
init image and the pass degenerates into a plain txt2img. Out-of-range values are
rejected before they are written, so a bad number in the form leaves the file as
it was.

Nothing here is retroactive: an image already made keeps the settings it was made
with, which is what lets reopening a work item restore it faithfully.


## Run

```sh
cargo run --release
# [yasdig] settings: /home/you/yasdig/user/settings.toml
# [yasdig] 3 model(s) available in /home/you/yasdig/models:
# [yasdig]   • samExtTURBO_v10   [sdxl, discovered, tuned, 6.5 GB]  768x768 → 1536x1536  · 6 steps, cfg 1
# [yasdig]   • sd_xl_base_1.0    [sdxl, discovered, 6.5 GB]  768x768 → 1536x1536  · 30 steps, cfg 6
# [yasdig]   • z_image_turbo     [z_image, discovered, 11.5 GB]  1024x1024 → 2048x2048  · 8 steps, cfg 1
# [yasdig]       --vae   vae/ae.safetensors
# [yasdig]       --llm   text_encoders/qwen_3_4b.safetensors
# [yasdig] GPU worker ready on adapter: AMD Radeon RX 7900 XTX (RADV NAVI31)
# [yasdig] GPU post-process: upscale=easu, grade=oklab, contrast=clarity, sharpen=rcas
# [yasdig] listening on http://127.0.0.1:8080
```

Startup runs a preflight first: it checks the sd.cpp binary, creates
`user/settings.toml` if it isn't there yet, lists the checkpoints it found with the
steps and CFG they resolve to — which is how you see what a template inherited —
and warns about templates pointing at files that aren't there. If something is
missing it prints copy-pasteable fix instructions and exits rather than failing later,
mid-request — `doctor` below runs the same checks without starting the server. No
models at all is a warning rather than a stop: a fresh folder has none, and one
dropped in while the server runs shows up on the next reload.

The browser opens on the page by itself (set `YASDIG_NO_BROWSER=1` to stop that).

The port is in `startup.toml`, beside `models.toml` — created on the first start with
`port = 8080`; edit it and restart to move. That port is kept, not just tried first:

| holding the port | what a start does |
|---|---|
| nothing | listens there |
| this same folder and build | opens the browser on it and ends — a second double-click |
| any other yasdig — another folder, or an older build of this one | asks it to close (without its clear-on-close), then takes the port |
| any other program | leaves it alone, and uses the next free port, saying which |

Closing another yasdig goes through its own `/shutdown`, so it works the same on
Linux and Windows, and nothing that isn't yasdig is ever closed. A yasdig built
before this existed has no such route and is stepped past like any other program. With no
`models.toml` argument it uses the one in the current directory, then the checkout a
`target/` build belongs to, and otherwise the one beside the executable, creating that folder's layout on the way.

Open <http://127.0.0.1:8080>, pick a model, type a prompt, Generate. The result shows
output resolution, seed, and per-stage timings (generate / upscale / fsr / refine / hi-res —
the upscale bar only when an ESRGAN model ran).
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
around 20 at 5–7. The same icons are on the Convert tab and on the viewer's ten
sliders.

The interface is static files — `web/index.html`, `web/style.css` and a folder of ES
modules under `web/js/` — with no build step, no framework and nothing fetched from
the network, so editing one and reloading is the whole development loop. The modules
are plain `import`/`export` that the browser resolves itself; `web/js/app.js` is the
entry point and `src/web.rs` holds the list of files the server will serve, which is
also the list `doctor` checks for. Colour in it means something: a button is **amber**
when pressing it spends GPU time, neutral when the click is free, and red when it stops or
deletes something. Everything else is a neutral grey, because the panel sits beside a
picture you are judging and chrome with a hue in it biases that judgement.

Quality is escalated *after* the fact, from the Refine & adjust dialog — so you only
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

Guidance is the other half of that, and each pass has its own box: **Hi-res CFG**
(`hires_cfg_scale`) and **Refine CFG** (`refine_cfg_scale`). The value that suits a 768²
base render over-guides at output resolution: a low-strength pass has little freedom to
begin with, and pushing it hard against the prompt spends that freedom on
high-frequency artifacts — the same crunch under-stepping produces, arriving from the
other direction. If a pass looks harsh, try ~2–3 there before reaching for more steps.

Leave a box blank and it follows the CFG scale box — or, when a model is picked for the
pass, that model's own CFG, since the pass then runs on its settings. The placeholder always shows what an empty box currently resolves to.

**Each pass picks its own checkpoint too.** *Hi-res model* and *Refine model* are
separate selects, both defaulting to *— same as generated —*: draft on a fast turbo
model, enlarge on one that holds structure, finish on a detailed one. A named checkpoint
brings its own steps, strengths, CFG and slider start points with it, and replaces the
LoRAs — a LoRA is trained against one base. Naming a second model costs no VRAM: every
pass is a separate `sd-cli` process that loads and frees its own weights, so the only
cost is re-reading the file if it has fallen out of the page cache.

Order is free and both repeat. The hi-res button disappears once the image is past
`hires_max_side`; refine is not under that ceiling, because it doesn't rescale and so
has nowhere to fall back to — and an image FSR has stretched beyond its last diffusion
is the one most worth refining. Either button is greyed out while its fold's steps box
says 0.

That does mean refine will diffuse at whatever size you're at, 3072² included. Keep an
eye on VRAM; if the pass fails, the image is left intact so you can retry.

Refine deliberately does **not** go back to the base render. Every img2img round-trips
through the VAE, which costs fine detail no matter how low the strength; paying that at
base resolution and then stretching the result 2× spends the loss on detail while only
the structural gain survives the upscale. That is what made the old refine pass read as
soft. At output resolution there is nothing downstream to smear what it adds.
