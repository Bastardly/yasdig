---
paths:
  - "web/js/loras.js"
  - "src/discover.rs"
---

# LoRAs

## LoRAs

`models/loras/` (`Config::lora_dir`), excluded from the image scan. `GET /loras` via
`discover::scan`; `base_cmd` builds the command line. Shaped by sd.cpp's
`extract_and_remove_lora` (`examples/common/common.cpp`):

- **Empty `--lora-model-dir` skips tag extraction** (tags conditioned as words), so
  `base_cmd` emits it **unconditionally**.
- **The page gets only what sd.cpp warns about LoRAs** (`lora_report`): its own
  `Only (a / b) … applied` (end of a runtime run) and `incompatible … skipped`, worded
  first, its counts after (`Timings::lora_note` / `VideoResult.note`). **Inference goes
  to the console** (`[yasdig] … possible LoRA problem`): no count line for a tag (at
  runtime a part left with no tensors drops it unannounced), or merge counts short.
  The inference was on the page in v0.5.2 and fired on MOSS's working DMD LoRA:
  unquantized models merge lazily, one count per staging group against the whole
  file, and a line logged mid-bar arrives glued to the bar (no newline until the last
  step), which `glued` now splits before any parser sees it.
- **A missing LoRA is a `LOG_WARN` and a dropped tag.** `Config::check_loras` is called
  from all four places a profile reaches `SdRunner` — `run`, `img2img`, `refine`,
  `hires`.
- **Tag regex `<lora:([^:>]+):([^>]+)>`**: `valid_lora_name` rejects `:` `<` `>`, and is
  a second whitelist beside `valid_id` (don't widen `valid_id`). Path check: every
  component `Component::Normal`.

**Wrong-family LoRA is silent too**, so `Role::Lora` carries a `LoraBase`
(`discover::lora_base`, off tensor names) and `check_loras` refuses a mismatch:

- **Never the metadata** (`ss_base_model_version` lies).
- **UNet split** by `attn2.to_k` width; self-attn-only LoRAs fall back to "second text
  encoder → SDXL, else CLIP width".
- **Each marker has two spellings** (dotted and kohya's `lora_unet_…`); separator
  markers are tested twice.
- **`Unknown` fits everything; `Other` (Wan, Flux.2 dev) fits nothing.**
- **Flux LoRAs** (`discover::flux_lora`): block down-projection width first (6144
  Flux.2 dev, 4096 Klein 9B); at 3072, past double 4/single 19 or per-block modulation
  → Flux.1, `*_stream_modulation` → Klein, else `Unknown`. `block_index` must skip
  `single_transformer_blocks` when looking for double blocks. **FLUX.3 is tested
  before it** (`txt_mode_blocks`/`content_mode_blocks`): it has `single_blocks` and
  `*_stream_modulations` too.

Wire: `/loras` entries carry `families` and `base`; `fits()` in `loras.js` is
membership only.

**Video LoRAs**: `VideoModel.picked_loras: Vec<VideoLora>` (request-only, the
Video tab's picker; `loras: Vec<String>` stays the hand-written `[[video]]` pins).
`LoraBase::Wan(width)` off an attention down-projection (3072 5B, 5120 the 14Bs,
1536 the 1.3B) and `fits_video`; `/loras` `families` carries video wire names too.
`VideoLora::tags` puts an A14B pick on its expert (`<lora:|high_noise|…>` is
sd.cpp's high-noise spelling, a plain tag the low-noise model). `video_cmd` always
sends `--lora-model-dir` (`m.lora_dir` or `Config::lora_dir`); `check_video_loras`
in `Engine::video` shares `check_lora_files` with the image side.
**H3 LoRAs** are `LoraBase::MinimaxH3` (a `token_refiner` path segment — Hunyuan's is
`individual_token_refiner`). sd.cpp places a DiT LoRA's tensors only under a prefix it
knows (`has_dit_prefix`), so a bare-named one is `Role::Unknown` with the reason, never
offered. Pruned H3 bases take an 8-wide `adaln_proj` input (`adaln_t_table`), so
only a LoRA made for pruned bases patches their time modulation; the catalog's DMD is one.

**Speed LoRAs / Turbo**: catalog `[[lora]]` (`catalog::Lora`, `dest` under
`Config::lora_dir`, had only at its own dest *and* length — `lora_is`), fetched with
cards listing it in `loras`. `/loras` adds `turbo {steps, cfg_scale}` to the file at
that dest and length. The page's *Turbo* switch (`image.js`) is a **view over the
picker**: ticked = the LoRA is picked (`syncTurboBox` reflects on every redraw, never
applies); ticking sets steps/CFG, unticking puts the model's back. Default on, per
model in `localStorage` — never server-side (`Profile.loras` stays request-only).
`syncAdvanced` applies the default after the LoRA listing lands, guarded by
`turboSeq`, which `applyWorkItem` bumps (`holdTurbo`) so a reopened item's steps
win. *Save as defaults* skips steps/CFG while Turbo is on. A `[[lora]]`'s `family` is
a `catalog::Family`, so a video family has one too: the Video tab's `#vturbo` is the
same view (`syncVideo`/`vTurboSeq`, bumped by `applyClip`; memory `turboMemory`).

**`Profile.loras` is request-only** — no per-model config; `as_profile` builds it empty
(`a_model_brings_no_loras_of_its_own`). It's on `Profile` because `base_cmd` is shared
by every pass (finishing passes keep the look) and because the profile is what a
`Pending`/sidecar retains. `swap_model` replaces it wholesale (right: a LoRA fits one
base), which is why `loras` is not on `EscalateRequest`.

`loras.js` is a factory (one instance):

- **`available` is shared, `picked` per instance**; `loading` memoizes the fetch.
- **`loras` is always sent, `[]` included** (missing = keep profile's).
- **A picked row that stops fitting stays**, with a note. `refresh()` awaits the
  listing fetch.
