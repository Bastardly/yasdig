# ~~Feature 1 - Preparation for more models~~ — DONE

This is an open-ended definition where I try to explain future plans. The goal here is to prepare for that future by making the architecture more modular. Any suggestions are simply suggestions, and any ideas based on deeper insight would be appreciated. 

Currently the app is build up on two types of models: SDXL based image models and Wan video model. However, the image model structure is a bit of a mess if we want to add more models of either type, or expand with other model families such as:
- LTX for video
- SD1 - for image
- flux, flex, krea etc. for image 
- zImage for image

This concern regards how we structure our model files. Currently, all SDXL models are in the root of the models folder. 

One suggestion could be:

/models
/models/image/SDXL
/models/image/zImage
/models/upscalers
/models/video/wan
/models/video/ltx

Another aspect is the UI.
The image tab is wired to SDXL models. But some models, like zImage, does not take negative prompts. So part of the UI would not make sense.
So one suggestion could be, to add a select box where the user can choose the model type, e.g. SDXL family, and then a definition file handles what fields are available. Same idea could be applied to video on the video tab. 

Another aspect that could be improved is the settings. Right now, we have general settings that applies to all models. It would be cool to be able to have individual settings for each model type - however, this can become rather granular defining all model types. E.g. SD1, SD2, SDXL, SDXL-turbo. So perhaps this would make more sense to do per model? We already have a preset that mimics that, but is not included in the UI.

One more issue could be the image-to-image tab. Perhaps we could take a leaf from the video tab, and use an image loader from e.g. outputs, whether it is image-to-image or text-to-image. Heck, if we do this correctly, perhaps we could even have one generate tab to handle both image and video - but this is just my ideas going haywire. But it would be cool.

Lastly, I do plan on having an image-edit tab, I believe there are both flux and zImage models that can handle edits. Meaning, an AI that can take an image, I could then mark areas in the image I wish to change, add a prompt and then hopefully the model can fix whatever it is. But that's a far away feature yet, but worth keeping in mind.

So, again. Anything is up for discussion.

---

# Feature 1 — the plan (agreed 2026-09-30)

## Progress

- **Phase 1 — classify and scan: DONE** (committed in `1cb130e`). Image files are identified by header
  (`discover::classify` → `Role::Image { family, bundled }`, `Role::ImageVae`, `Role::Unsupported`), the whole
  models tree is scanned (`Config::scan_images`, excluding the video/LoRA/upscaler dirs) with a
  `(path, len, mtime)` memo (`discover::identify`), `ImageFamily` exists in `src/config.rs`, `doctor` tags
  families and names what it won't offer. The LTX file in the root is no longer listed.
- **Phase 2 — components and Z-Image: DONE** (committed in `455b8c7`, verified on the GPU). `Role::Llm(LlmKind::Qwen3 { dim })`;
  `ImageFamily::{runnable, is_unet, recipe, flags}` (Z-Image runnable, turbo recipe 1024², 8 steps, CFG 1, euler;
  `--diffusion-fa` always on DiTs, no `--offload-to-cpu`); `Components { vae, llm, clip_l, t5xxl }` and
  `Profile.family` + `Profile.components` (None = bundled `-m`); `discover::image_components` picks the parts from
  the scan (encoder by width) or names the conventional path; `Config::profile()` layers
  settings → family recipe → template; `base_cmd` emits `--diffusion-model` + parts; `Profile::missing` /
  `require_files` replace the "checkpoint not found" checks; convert offers a DiT as `--diffusion-model`;
  `doctor` shows the parts a DiT was assembled with.
  Known gaps: SDXL LoRAs are not yet filtered out for a Z-Image (phase F); the Qwen-Image VAE still reads as a
  Wan 2.1 VAE (same shape) and needs telling apart when Qwen-Image is added.
- **Phase 3 — capabilities in the page: DONE** (verified). `/models` carries `family`, `family_label`
  and `missing` (paths). The six model selects group by family (`<optgroup>`, only when there is more than one)
  and list a model with missing files *disabled*, with the files in its tooltip. The negative prompt is hidden, with
  a one-line note in its place, at CFG exactly 1 on both tabs (`syncNegative`, hung off `syncCfgPlaceholders`) — sd.cpp only runs the
  unconditioned half when text CFG ≠ image CFG (1), `resolve_guidance` in `src/pipeline/request.cpp`.
  **Dropped from the plan:** per-family size rounding (`size_multiple`). 64 is a valid multiple for every family
  here (DiTs need 16), so generalising `snap64`/`fit_to` would buy at most 48 px of width for churn in tested
  code. `guidance`, `ref_images` and `inpaint` caps are not served yet — nothing uses them; they arrive with
  Flux and the edit tab.
- **Phase 4 — settings layers: DONE** (verified; a cyclic-import crash found in review was fixed and is now
  guarded by `web/js/modules.test.js`). Layers: `user/settings.toml` → family recipe →
  `[templates.*]` → **`user/model-settings.toml`** (renamed from the planned `user/models.toml`, to not share a
  name with the root config). `settings::ModelSettings` (load/save, `[model."<name>"]` tables of
  `PartialSettings`, now `Serialize`); `settings::update_model` keeps only values that differ from the model's
  base (compared after a typed round trip, so `6.3` vs an f32 doesn't pin); `Config::entries()` / `ModelEntry`
  / `model_base()` split the registry into base + per-model layer. Routes `GET/POST /model-settings`
  (filesystem-only, off the queue). `/models` gains `tuned`; `doctor` tags "tuned" and warns about entries
  naming no model. **Engine now resolves models per job** (`Engine::model`) instead of a registry built at
  startup, which had made new files and saved defaults invisible to runs. Page: "Save as defaults for this
  model" under the Image tab's Advanced rows (sends every box; server drops the unchanged); a *Per model* card
  on the Settings tab (`web/js/modelsettings.js`, rows reused from `settings.js`'s `FIELDS`, placeholders are
  the served base); model selects keep their selection across a rebuild. **Changed from the plan:** the global
  form keeps width/height/steps/CFG/sampler, relabelled *SD 1.x, SD 2.x and SDXL* — they are the one place to
  change every SDXL at once, and other families' recipes already override them. Limitation: this layer can't
  pin FSR over a global ESRGAN upscaler (absent key and FSR are both "no name").
- **Phase 5 — one Image tab: DONE** (verified). The Image-to-image tab is gone (`img2img.js`,
  `#tab-img2img`, its PANES row, `i2go`/`i2prog`). `web/js/source-slot.js` is a factory (Choose… via the shared
  `#picker` dialog, Upload…, drop, paste gated to its tab) used by the Image tab's *Start from* and the Video
  tab's first frame — so a video can now start from an upload. With a source, `data-when` controls swap
  (`syncMode` in `image.js`): base Steps, size and output upscale hide; Strength, Steps, Working size and img2img
  CFG show. `genPayload` returns `[route, body]` — `/img2img` with a source, `/generate` without — so the run
  panel (batch, loop, after-each) works over a source too, walking the seed. Reopening a work item sets or clears
  the slot. No server change. Settings rows moved to the new ids (section renamed *Starting from an image*).
- **Phase 6 — docs: DONE.** CLAUDE.md: new *## Families* and *## Start from* (replacing *The image-to-image
  tab*), a *Per model* subsection under *## Settings*, step 7b in *Adding a per-model knob*, and every stale
  reference to the old tab, `img2img.js`, `#i2go` and the three panes. README: *Get a model* (ComfyUI layout,
  Z-Image download table), *Model families* with the four layers, *Start from*, *Per-model settings*, the
  `/models` fields and `/model-settings` routes, and the roadmap.
- **Left for later** (phase F, in the README roadmap): Flux and Qwen-Image as runnable families, LoRAs
  filtered by family, the edit tab (instruction edit vs masked inpaint, as more *Start from* slots), LTX-2 on
  the Video tab.

## Context

`plan.md`'s Feature 1: the app knows two things, single-file SDXL checkpoints (image) and header-discovered Wan
models (video). The goal is SD1, Flux/Krea/Flex, Z-Image, Qwen-Image, LTX etc. without each one being a
rewrite, and a UI that shows only the controls a model actually uses. An edit tab is further off, but the design
should leave room for it.

**The main finding: the split that matters is single-file vs multi-part models, not image vs video.** Every
image family on the list is run by sd.cpp as `--diffusion-model` + `--vae` + text encoders (`--llm`, `--t5xxl`,
`--clip_l`). That is the shape `discover.rs` already handles for video: read headers, assemble parts by role,
share one encoder across models, name a missing part where it should go. So the core of this feature is to point
that machinery at the image side and make *family* a first-class concept on both sides. Two things already in
the repo show why:
- `models/redgraftLTX25Fast2K_…safetensors` is an **LTX video** model in the image root. `discover_files()`
  (`src/config.rs:1317`) only checks extensions, so it is listed as an image checkpoint today and would fail when run.
- `models/misc/vae/ae.safetensors` is the Flux VAE, used by Flux, Chroma *and* Z-Image. Folders per family would
  need a copy for each.

Decisions taken (all four are the recommended options):
1. **Folders by role, family read from the header.** ComfyUI's layout across the whole models dir. The folder
   names help people browse; the program ignores them. Today's flat layout and `models/video/` keep working.
2. **Settings: built-in family defaults, then the user's per-model overrides.** Global Settings shrinks to the
   knobs that apply to every family.
3. **Merge txt2img and img2img** into one Image tab with a "Start from" slot. Video keeps its own tab but reuses
   the same parts.
4. **Z-Image is the first new family**, the proof that the design works.

## Target design

### A. One scan, one classifier, a family on everything
- `discover::scan(models_dir)` runs over the **whole** models tree, once. The existing `video_dir`,
  `lora_dir` and `upscaler_dir` remain overrides that get scanned too when they point outside the tree.
- Extend `discover::classify` (`src/discover.rs:461`) with the image discriminators, transcribed from sd.cpp's
  `get_sd_version` (`~/code/stable-diffusion.cpp/src/model_loader.cpp:~495-605`), the same way the Wan ones were:
  SD1, SD2, SDXL (UNet: `input_blocks` + `label_emb` / `conditioner.embedders.1`), Flux / Flux2 / Chroma, Z-Image
  (`cap_embedder.0.weight`), Qwen-Image, LTX (`adaln_single.emb.timestep_embedder`). Add roles for the new
  encoders (Qwen3 LLM, Qwen2.5-VL, T5-XXL, CLIP-L/G) and the Flux, Qwen-Image and SD VAEs.
- **Whether a file is a full checkpoint (`-m`) or a bare diffusion model** is also read from the header: are VAE
  and text-encoder tensors present (`first_stage_model.`, `conditioner.` / `cond_stage_model.`)? A Flux
  "all-in-one" file then works through `-m`, exactly like SDXL.
- New `ImageFamily` enum in `src/config.rs`, next to `VideoFamily` and in its shape: `as_str`, `recipe()`,
  `size_multiple()`, `native_size()`, plus `caps()`. Separate enums because their recipes are different shapes;
  one classifier feeds both.
- A new `discover::build_images` assembles image models the way `build` assembles video ones: a bare DiT claims the
  VAE and encoders its family needs, and a missing part gets the conventional path inside its role folder
  (`vae/ae.safetensors`, `text_encoders/qwen_3_4b.safetensors`), greyed out in the UI.
- **Cost:** `registry()` runs per request. A header read of ~20 SDXL files is milliseconds, but add a
  `(path, len, mtime) → Role` memo in a `Mutex<HashMap>` so the whole-tree scan never re-parses unchanged files.
  It must stay keyed on mtime so a newly finished download still shows up without a restart.

### B. Profile gets a family and components
- `Profile` gains `family: ImageFamily` and `components: Components { vae, clip_l, clip_g, t5xxl, llm, llm_vision }`,
  all `Option<PathBuf>` with `#[serde(default)]`. `model` keeps its meaning (the primary file) so old sidecars load.
  `ImageFamily::default()` is `Sdxl`, which is exactly what every existing sidecar was.
- `SdRunner::base_cmd` (`src/sd.rs:412`): a checkpoint emits `-m` as today; a bare DiT emits
  `--diffusion-model` plus each present component. The family's recipe carries its own memory flags
  (`--offload-to-cpu`, `--diffusion-fa`, `--clip-on-cpu`), because on this box RAM runs out before VRAM does.
- `snap64` / `fit_to` take `family.size_multiple()` (SD 64, the DiTs 16) instead of a hard-coded 64.
- Escalations across families keep working as they are: every rung is pixels in, pixels out, so refining a
  Z-Image picture on an SDXL checkpoint is legitimate. `swap_model` already clears LoRAs, which becomes more
  obviously correct now.

### C. Settings layers
The resolution chain becomes (each step is one `merged_with`, no `..rest`, so the compile-error guarantee holds):

`Settings::default()` → `family.recipe()` (a `PartialSettings`) → user global `settings.toml` (family-agnostic
fields only) → `[templates.*]` in `models.toml` (hand-written, read-only) → **`user/models.toml`** per-model
overrides (written by the UI).

- **Family-specific knobs leave the global form**: width, height, steps, cfg_scale, sampler, refine/hires steps and
  CFGs, source steps. They stay on `PartialSettings`, so recipes, templates and per-model files can set them.
  Global keeps prompt, negative, hires_scale, variables, upscale_factor, upscaler, sharpness/clarity,
  hires_max_side (a hardware budget), hires_flash_attn, and the strengths (a workflow preference). Otherwise
  CLAUDE.md's own rule applies: a global Steps box that silently does nothing for Z-Image is "a setting that
  appears not to take".
- **Migration:** the SDXL recipe *is* today's `Settings::default()` numbers. Family-specific values already in an
  existing `settings.toml` keep applying to the UNet families (SD1/SD2/SDXL) only, as a legacy overlay, so this
  install changes nothing on upgrade. Documented in the header comment and README.
- `user/models.toml`: `[model."<registry name>"]` tables of `PartialSettings`, regenerated on save like
  `settings.toml`. Written by a **"Save as defaults for this model"** button in the Image tab's Advanced panel and
  editable in a new *Per model* section of the Settings tab (pick a model: family defaults shown as placeholders,
  overrides as values, a clear button per field). This is also what tells SDXL and SDXL-turbo apart, which no
  header can.

### D. Capabilities served, never derived in JS
Following the `frame_grid` rule: `/models` gains `family`, `components` (for greying out, with the missing path
named) and `caps`. The rule each capability follows:
- **Negative prompt:** live iff `cfg_scale > 1`, a general rule, not a per-family table. It covers Z-Image turbo,
  SDXL-turbo and Flux in one line. The box greys out with "ignored at CFG 1" and comes back the moment CFG is raised
  (Z-Image base at CFG 5 does use one).
- **`guidance`:** Flux-type distilled guidance (`--guidance`), shown only for families that take it.
- **`size_multiple`, `native_size`:** drive the size inputs' step and the notes.
- **`ref_images`, `inpaint`:** reserved for the edit tab, served from day one and ignored by the page for now.
The model `<select>` groups options by family (`<optgroup>`) and never lists a video family.

### E. One Image tab with a "Start from" slot
- New `web/js/source-slot.js`, a factory in the shape of `loras.js`/`gridmark.js`: *None* / *Outputs picker* (the
  existing `outputGrid`) / *Upload* (the existing `POST /upload`, canvas re-encode). It owns the working-size
  control that lives on the img2img tab today.
- With a source picked, **Generate** posts to `/img2img` (unchanged server route) with the source strength and steps;
  with none, to `/generate`. The Image tab's result pane, escalations and Save then serve both. The run panel stays:
  in a merged tab it costs nothing, and a batch of seeds over one source is legitimate.
- The Video tab's first-frame picker is rebuilt on the same factory.
- `img2img.js`, its pane row in `PANES`, `#tab-img2img` and `#i2go` go. `setBusy`/`IDLE_LABELS` lose one entry.
  `WEB_FILES` swaps the row.
- Later, the edit tab is two more slot kinds on the same factory: *Mask* (a canvas painter, sent as a PNG for
  `--mask`) and *References* (sent as `-r`). Both are copied into scratch under invariant 8 like the source already
  is. There are two kinds of edit and they are worth keeping apart: **instruction edit** (Kontext, Qwen-Image-Edit,
  Flux2: reference image + "make the sky red", no mask) and **masked inpaint** (`--mask`). The caps say which a
  model does.

### F. Small follow-ons worth doing in the same sweep
- **The LoRA picker filters by family.** A LoRA's key names say which base it patches (SDXL has `lora_te2_` /
  `input_blocks`; Flux has `double_blocks`). Mismatched ones are shown greyed with "for SDXL", the same
  listed-not-hidden rule as today.
- `doctor` prints the image models by family with the parts each is missing, as it already does for video.
- **LTX** becomes a `VideoFamily` entry (`--llm` Gemma, `--audio-vae`, `--embeddings-connectors`) once A is in.
  It is a video-side follow-up, not part of this plan.

## Phases (each lands green on its own)

1. **Classify and scan.** A only, plus `ImageFamily` with just `Sdxl`, `Sd1`, `Sd2`, `ZImage`, `Flux`, `QwenImage`,
   `Unsupported(String)`. `registry()` lists only image families it can run, and `doctor` shows the rest. Fixes
   the LTX-in-root bug. No UI change beyond the LTX file disappearing from the select.
   Files: `src/discover.rs`, `src/config.rs` (`discover_files` → scan-based, `registry`), `src/preflight.rs`.
2. **Components and Z-Image.** B + the Z-Image recipe (8 steps, CFG 1, 1024², euler, `--diffusion-fa`,
   `--offload-to-cpu`) + `ImageFamily::recipe()` slotted into `Config::profile()` (`src/config.rs:1702`).
   Files: `src/config.rs`, `src/sd.rs`, `src/pipeline.rs` (`snap64`/`fit_to`), `src/settings.rs`.
3. **Capabilities in the page.** D. Files: `src/main.rs` (`build_models_json`), `web/js/image.js`,
   `web/js/img2img.js` (still exists at this point), `web/js/hints.js`.
4. **Settings layers.** C: the global form shrinks, `user/models.toml` appears, *Save as defaults for this model*,
   the Settings tab's *Per model* section, legacy overlay migration.
   Files: `src/settings.rs` (a new `ModelSettings` load/save/patch in the `Settings` pattern), `src/config.rs`,
   `src/main.rs` (routes `GET/POST /model-settings`), `web/js/settings.js`, `web/js/image.js`, `web/index.html`.
5. **Merge the image tabs.** E. Files: new `web/js/source-slot.js`, `web/js/image.js`, `web/js/video.js`,
   `web/js/state.js`, `web/js/busy.js`, `web/js/app.js`, `web/index.html`, `src/web.rs`, delete `web/js/img2img.js`.
6. **Docs.** CLAUDE.md: the *Adding a per-model knob* table gains the recipe step; a new *## Families* section
   says "adding a family = classifier arm + enum variant + recipe + component flags + caps"; *## The
   image-to-image tab* is rewritten as the source slot. README: folder layout, the new field layering, Z-Image
   setup. `plan.md` is marked done the way the previous features were.

Each phase is its own commit on `newArch`. Phases 1–2 are the architecture; 3–5 are what you see; after 2,
Flux / Qwen-Image are each a classifier arm, a recipe and a caps row.

## Reuse, don't rebuild
- `discover::read_tensors`, `Tensors::has/square_dim/vector_len`, the `Found`/`Role` model, and `build`'s
  "missing part named where it goes" pattern (`src/discover.rs:714`).
- `VideoFamily::recipe()` / `FamilyRecipe` as the template for `ImageFamily::recipe()`.
- `Settings::merged_with` for every new layer; `settings::patch` semantics for `user/models.toml`.
- `outputGrid`, `POST /upload`, `Engine::output_source`, `fit_to` for the source slot.
- `valid_lora_name` for any user-chosen path that reaches a component flag.

## Verification
I run: `cargo build`, `cargo test`, `cargo clippy`, `node --test 'web/js/*.test.js'`. (No `cargo fmt`, see
memory.) New unit tests, no GPU needed:
- A classifier test per family, from synthetic tensor lists (the existing `t(...)` helper), including "an LTX
  file is not an image model" and "an SDXL checkpoint with a bundled VAE is `-m`, a bare Z-Image DiT is not".
- `build_images`: a Z-Image DiT with `ae.safetensors` and no Qwen3 is listed with its missing encoder named in
  `text_encoders/`.
- `base_cmd` argv for a checkpoint and for a component model, asserted like `video_cmd`.
- Layering: recipe < global < template < per-model, and the legacy overlay reaching SDXL but not Z-Image.
- An old sidecar with no `family` loads as SDXL and reopens unchanged.

You run (checklist handed over at each phase):
- `cargo run --release -- doctor`: families listed, LTX file shown as video/unsupported, not as an image checkpoint.
- Existing SDXL generate / refine / hi-res / reopen are unchanged (phase 1–2 regression).
- Z-Image: drop the DiT in `diffusion_models/` and Qwen3-4B in `text_encoders/`, generate at defaults, then refine
  it on an SDXL checkpoint.
- Negative box greys out at CFG 1 and returns at CFG 5.
- Per-model save survives a restart, and a change to global Settings still reaches it for the global fields.
- Merged tab: Start from an output and from an upload, then escalate and save; the video first-frame picker still
  works.
