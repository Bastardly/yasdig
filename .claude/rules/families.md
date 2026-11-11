---
paths:
  - "src/discover.rs"
  - "web/js/parts.js"
---

# Image model families

## Families

An image model is a **family** plus files, read off tensor names by
`discover::classify` (shared with video). `ImageFamily` and `VideoFamily`
(`src/config.rs`) are separate enums, one classifier. **Nothing is identified by
filename or folder.** `Config::scan_images` walks the models dir recursively, leaving
out the video, LoRA and upscaler dirs — except video-dir *parts* some image slot fits.
Non-runnable files aren't offered; `doctor` says what they are (`Role::Unsupported`,
`Role::Unknown`).

**Bundled or assembled**: `Role::Image { family, bundled }`. Bundled runs as `-m`; a
bare DiT as `--diffusion-model` plus a flag per part, picked by
`discover::image_components` (or the conventional missing path, listed greyed out).
**The flag decides the tensor prefix**, so `base_cmd` asks `Profile::is_bundled`, never
guesses.

**Parts are one table, `ImageFamily::slots()`** (key, label, `fits`, conventional
path), used by `image_components`, `/models` (`Config::part_views`) and
`Config::resolve_part`. Picks arrive on `GenRequest::parts` or `[parts."<name>"]` in
`user/model-settings.toml`; `resolve_part`'s whitelist is membership in the scan.

Things that break silently:

- **Encoder slots fit by kind *and* width.** sd.cpp loads any `--llm` into the
  architecture the diffusion model implies; a same-width wrong encoder renders
  nonsense. `every_slot_is_fillable` pins that encoders don't cross. Qwen3 ships
  0.6B–32B; only the matching width fits (`LlmKind::Qwen3 { dim }`).
- **A text-only GGUF's architecture is in its metadata** (`general.architecture`,
  `Tensors::arch`); llama.cpp writes Qwen3-VL's language half with Qwen3's names. T5:
  umt5 vs T5-XXL differ only in token embedding rows (256384 vs 32128).
- **Qwen3.5 is tested before the Qwen3 arm** (`qwen35` arch, or `linear_attn.`/`ssm_a`):
  MOSS-Image's 9B is 4096 wide like Klein 9B's Qwen3-8B, and a safetensors one has
  `q_norm` too.
- **A tokenizer is a part** (MOSS-Image's `--tokenizer`, `Role::Tokenizer`): `walk`
  reads `.json` files only through `read_tokenizer` (BPE vocabulary size) and lists only
  tokenizers, so config JSONs never become `doctor` rows. The catalog allows a `.json`
  dest only on `slot = "tokenizer"` (`safe_dest`).
- **What a slot accepts is what sd-cli *loads*.** Transformers' nested
  `model.language_model.` Qwen-VL layout is `Unknown` naming the GGUF as the fix.
- **Video-dir parts are shared, its models aren't** — filtered by "some `slots()` row
  fits"; only when the video dir is inside the models dir.
- **Klein 4B/9B are two families; Flux.1 dev/schnell are two** (steps differ;
  `guidance_in`); **so are FLUX.3 Klein/Klein Base** (`ref_time_in`, the distilled
  one's flow map). `LoraBase::fits` lets a Flux LoRA fit both, a FLUX.3 one both
  FLUX.3s — hence `/loras` serves `families`.
- **A distilled file its header can't tell apart** (Iris-3B Turbo) carries its
  settings in the catalog (`File.distilled`): `Catalog::distilled` matches the model at
  its own dest *and* length, and `Config::entry` lays steps/CFG over the recipe and
  `Profile.sigmas` from it. A request's absent `sigmas` keeps the model's.
  `SIGMA_PRESETS` rows are (name, family, the `--sigmas` value).
- **Iris is pixel space**: no VAE slot (so no `--vae`), sd.cpp's marker pair
  (`y_embedder.layer_pool` + `modulation_cores.adaln_img`); its recipe carries
  `weight_type = q8_0` because the release is a 12 GB fp32 file. `--preview proj`
  only logs that it has no projection.
- **Qwen-Image 2.1 is not Qwen-Image**: `txt_in.text_norm` (sd.cpp's marker) is
  tested before the `img_mod` arm, its VAE is `VaeKind::QwenImage21` (Wan's shape at
  `conv2.bias` 64), its encoder `Qwen3Vl { dim: 4096 }`. Its LoRAs' `img_mlp.gate_up`
  is tested before Qwen-Image's `img_mlp` LoRA arm. `sigmas` (`SIGMA_PRESETS`, each
  tied to a family) is request-only, sent by Turbo, on txt2img only (a fixed list
  can't be strength-trimmed).
- **`classify` order**: image models before either VAE branch (a bundled SDXL's VAE
  looks like Wan's); LDM-VAE before Wan VAE; unsupported markers before supported
  families (Hunyuan Video has `double_blocks`, SVD is a UNet); FLUX.3 before every
  Flux arm (it keeps `single_blocks`), as in sd.cpp.
- **UNet split by cross-attention width** (`attn2.to_k`: 768 SD1, 1024 SD2, 2048 SDXL)
  via `inner_dim(layout)` (rectangular; GGUF reverses shapes).
- **`discover::identify` memoizes on `(path, len, mtime)`** — a finishing download is
  the same path with a new length.
- **`Engine::model` resolves per job**, never from a startup registry.
- **`bundled` is its own field.** A bundled SDXL may have a VAE picked (`-m` + `--vae`
  override). `Profile.bundled: Option` read via `is_bundled()`, fallback =
  `components.is_none()` for old sidecars.
  `a_checkpoint_keeps_its_m_when_it_is_handed_a_vae`. (External VAE skips SDXL's fp16
  conv scale — pick the fp16-fix VAE.)
- **`ImageFamily::runnable` needs components *and* a `recipe()`**
  (`a_runnable_dit_has_a_recipe_and_its_parts`).

**Adding a family**: classifier arm (from `get_sd_version`'s marker), variant with
`label`/`as_str` in `ImageFamily::ALL`, rows in `slots()` (plus `Components` field /
flag / `Role` if new), `recipe()`, `flags()`, `runnable()`. **Read real released file
headers first** (HTTP range read of a safetensors header, or a GGUF's first MB) — docs
lie (Flux.2 VAE is diffusers' layout). Then `docs/configuration.md` download list and
the README family table. The escalation ladder needs nothing.

Page: `syncParts` in `image.js`, redrawn by `syncAdvanced`. `genPayload` sends **every**
slot, emptied as `null`. `applyWorkItem` sets the model without `change`, so calls
`syncParts` then `setParts` matched on `data-path` (sidecars record paths, the wire
carries names).
