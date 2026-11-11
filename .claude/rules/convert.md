---
paths:
  - "src/convert.rs"
  - "src/foldscale.rs"
  - "src/half.rs"
  - "web/js/convert.js"
---

# Convert

- **Convert** (`src/convert.rs`, `-M convert`; the Models tab's `#panel-gm-convert`,
static markup, its rail button in the Tools fold after Installed (`build()`) — file, type, the
button; role/estimate/dest are the selects' `title`s): a file's on-disk type isn't its run
cost — Vulkan has no fp8 matmul, so `Linear::forward` casts to BF16 in-graph; `q4_K`
is the first type actually multiplied.
- **The component flag renames tensors** (`--diffusion-model` →
  `model.diffusion_model.`, `--vae` → `vae.`, `--t5xxl` → `text_encoders.t5xxl.transformer.`,
  `-m` → none). `Slot::for_role` derives it from the role; roles with no slot
  (`--llm`, `--clip_vision`) aren't listed.
- **VAE f16 saves memory, not decode time.** `Settings::vae_conv_direct` (on,
  page-level) is the decode speedup (Klein 9B VAE 9.3 s → 0.9 s).
- **Safetensors VAE → f16 is native** (`Plan::native`, `half::safetensors_to_f16`),
  since sd.cpp's converter aborts on the Flux.2 VAE's i32 tensor
  (`f32_to_f16_rounds_to_nearest_even`). List grouped by `Candidate::group`.
- **`-t 1` is load-bearing** — sd.cpp's multithreaded converter races the loader's
  `file_data` (aborts ~1 in 3, could write wrong bytes). Revisit if upstream locks.
- **GGUF folds 5-D shapes**, so the Wan VAE's `z_dim` is read off `conv2.bias`
  (`Tensors::vector_len`; `a_vae_survives_the_trip_through_gguf`).
- **Scaled-fp8 scales must be folded** (`foldscale::fold` after `Engine::convert`;
  `fold-scales` for old files; `doctor` names them) — else matmuls run 250–11 000×
  too large and render as a mesh. **Block field offsets in `foldscale::layout` are
  ggml's `block_*` structs** (`a_scaled_fp8_file_folds_into_its_weights`); each
  weight/scale pair is written under a journal.
- `estimate` transcribes `tensor_should_be_converted` (within 0.01%). The type table
  is served, not copied into the page.
