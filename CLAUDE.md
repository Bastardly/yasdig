# CLAUDE.md

**yasdig** — *Yet Another Stable Diffusion Image Generator* (rustimage until
2026-10-12). Python-free Vulkan image generator for AMD/Linux. Generate small and cheap
with stable-diffusion.cpp (a subprocess, never FFI), then escalate on demand with
img2img passes and a native FSR post-process in `wgpu`. All sd.cpp work goes through
one job queue and one diffusion worker; the viewer's re-filters run on a display worker
sharing the one FSR device behind a lock (*## The job queue*). The Agent tab runs a
second subprocess the same way — llama.cpp, through our own driver (*## The agent*).

**`README.md` is short and for users** (why escalation, install, what's supported —
and video). **`docs/` is the reference**: `configuration.md`
(folders, model files, `models.toml`/`user/settings.toml` field tables), `guide.md`
(features), `api.md` (CLI and HTTP routes), `building.md`, `architecture.md`. This
file holds only what they don't: the traps.

## Commands

```sh
cargo test                                  # no GPU: naga-validates the WGSL shaders, band maths, DTOs, `fit_to`
cargo run --release                         # server on startup.toml's port (127.0.0.1:8080)
cargo run --release -- doctor               # preflight: binary, checkpoints, web UI
cargo run --release -- fsr-selftest         # full wgpu path, no sd.cpp or checkpoint
cargo run --release -- fsr in.png out.png [scale] [sharpness] [clarity]
cargo run --release -- upscale in.png out.png <model> [factor]  # model enlarge + consistency dB
cargo run --release -- sr-selftest          # `src/sr/` shader ops vs CPU references, matmul TFLOPS
cargo run --release -- fold-scales f.gguf   # fold a scaled-fp8 GGUF's scales into its weights, in place
node --test 'web/js/*.test.js'              # prompt-weight maths, OOM box choices, module graph evaluates.
                                            #   Node's built-in runner; no npm/package.json; runtime needs no Node.
```

Shaders otherwise compile only at GPU device creation, so `cargo test` is what turns a
WGSL typo into a test failure instead of a worker-thread startup failure.

**Run `fsr-selftest` after changing a filter constant.** It asserts dimensions, that
scale 1.0 with all filters at 0 is byte-exact, checks each grade knob (`check_grade`),
and prints per-band movement of planted gratings at 1536² and 3072² (invariant 6).
Band rows are printed, not asserted, because `rcas_radius` is a step function; turning
them into an assertion is the next move. The DFT's own correctness is in `cargo test`.

## Packaging

A release is **one executable** per OS with sd-cli and the page inside it
(`.github/workflows/release.yml` on a `v*` tag), shipped as an **archive holding a
`yasdig/` folder** (the program plus `packaging/README.txt`): a `.zip` on Windows
(Edge's SmartScreen warns far less than for a bare unsigned `.exe`), a `.tar.gz` on
Linux (keeps the executable bit). Double-clicked, it creates its folders beside
itself, starts, opens the browser. Not Tauri (WebKitGTK would be a dependency).
**`cargo build --release` is the whole recipe.** sd.cpp is a gitignored clone at
`./stable-diffusion.cpp/`; `build.rs` builds `sd-cli` into `build-yasdig/` there and a
*release* build embeds it (`build.rs` → `src/bundle.rs`, unpacked to `bin/sd-cli` on
start). Debug builds never embed (link time); a failed sd.cpp build warns in debug,
errors in release. `YASDIG_SD_CLI=<path>` overrides. The repo's `models.toml` names
`build-yasdig/bin/sd-cli`.

`build.rs` traps: it **reruns on `.git/logs/HEAD`, not the source tree** (hand edit to
sd.cpp needs `touch build.rs`); **configure is stamped on success**
(`.yasdig-configured`), not judged by `CMakeCache.txt`.

Five things break silently:

- **Home folder is `home::config_path`**: an argument, else `models.toml` in the CWD,
  else this checkout when the exe is inside its `target/`, else the exe's folder —
  never the CWD alone. `home::ensure` creates only what is missing. **An exe folder
  inside `temp_dir()` is refused** (`in_temp`): double-clicking it inside the zip runs
  it from a folder Windows deletes, models and all.
- **`[sd].binary` wins over the bundle**; `Config::load` unpacks only when it's unset.
- **Unpack is keyed on the stamp `bin/sd-cli.version`** (build.rs's hash), not on
  hashing 93 MB. Binary goes through `.part` + rename, stamp written last.
- **The page is served from `web/` when present, else `WEB_FILES`** (`web::serve`).
  `doctor` checks files on disk only when it reads them.
- **The listener binds before the GPU worker starts** (`Engine::new` sweeps `work/`).
  Port is `startup.toml`'s (`src/startup.rs`). A taken port is asked `GET /instance`;
  `startup::bind`: same folder *and* build → hand over (open browser, exit); any other
  yasdig → `POST /shutdown {"clear":false}` and take the port; non-yasdig → never
  touched, try the next of 20. `"app":"rustimage"` is accepted (transitional).
  **`build_id` is read once, first thing in `run`** (re-read, a rebuilt binary hands
  over to a stale server). **Takeover must send `clear:false`**.

**A master push builds Windows alone** (same `release.yml`, the Linux job skipped,
`publish`/`scan` tag-only): a 14-day workflow artifact to try on a Windows PC, and
`cargo test` on Windows. Never a rolling pre-release — the site's buttons read
`/releases` with pre-releases included.

**Releasing** (`v*` tag → `release.yml`, ~9 min; pushing `master` also deploys the
site when `site/` changed):

1. Commit the work. In `CHANGELOG.md` turn `## Unreleased` into `## X.Y.Z — date`;
   bump `version` in `Cargo.toml` and `yasdig`'s entry in `Cargo.lock`; `cargo build`;
   commit "Release vX.Y.Z"; `git tag -a vX.Y.Z -m "yasdig vX.Y.Z"`.
2. `git push origin master vX.Y.Z`.
3. **Write the release page's notes** — never leave only GitHub's generated
   "Full Changelog" link. The version's CHANGELOG section, plus a link to the full
   changelog, into a file, then **right after the push**
   `gh release create vX.Y.Z --verify-tag --title vX.Y.Z --notes-file <file>`: the
   workflow's publish job finds the release and only uploads the archives to it,
   keeping title and notes. If it already ran, `gh release edit vX.Y.Z --notes-file`.
4. Watch it (`gh run watch`); check both archives are on the release, and — about
   20 minutes later — the *VirusTotal* section in its notes.

**Trust without a certificate**: `publish` also writes `SHA256SUMS` and a build
provenance attestation (`actions/attest-build-provenance`); then the `scan` job
(`.github/workflows/scan.yml`, reusable, also runnable by hand with a tag) uploads
both archives and the program inside each to VirusTotal
(`.github/scripts/virustotal.sh`: bash, curl, jq; secret **`VT_API_KEY`**, Public API
at 4 calls/min) and appends the result to the release notes
(`scan-notes.sh`, between `<!-- yasdig-scan-begin/end -->`, replaced on a re-run, the
notes above kept), plus `scan.json`. It runs after the release is public and never
blocks one.

- **The site parses that notes section**: one `<!-- yasdig-scan {scan.json} -->`
  comment (`scanOf` in `site/index.html`). Change both or neither.
- **The hash shown is GitHub's own `digest` of the asset**, there from upload,
  not the scan's.
- **The README's VirusTotal badge reads `https://yasdig.com/badge.json`** (shields.io
  endpoint format). **shields.io refuses github.com URLs**, so `pages.yml` copies the
  newest release's `badge.json` into the site on every deploy, and `scan.yml` starts
  a deploy when it is done (`actions: write`).

`build.rs` is a table of two native builds (`BUILDS`): sd.cpp's `sd-cli`, and the
agent's `yasdig-llm` from our `llm/` against a gitignored `./llama.cpp/` clone (into
`llama.cpp/build-yasdig/`, `cfg(bundled_llm)`, `YASDIG_LLM`). A missing llama.cpp clone
is a warning even in release — the build then has no agent. `src/bundle.rs` is the same
table (`bundle::SD`, `bundle::LLM`). CI pins `LLAMA_CPP_REF` beside `SD_CPP_REF`.

sd.cpp flags in `build.rs` (`GGML_NATIVE=OFF`, `GGML_OPENMP=OFF`, `/MT`) make it
portable; the workflow fails if sd-cli links anything but the Vulkan loader and libc.
Static libstdc++ is a workflow `LDFLAGS` (Fedora lacks `libstdc++.a`; CI's Ubuntu 22.04
is what travels). Model-folder names shown to the page go through `config::slash_name`
so they read `sdxl/x` on Windows too.

## The four GPU passes

`Fsr::process()` (`src/fsr/mod.rs`) is the single entry point, taking one `Filters`
(`src/config.rs`). A no-op pass is skipped and its textures never allocated. The
`.wgsl` headers are thorough — read them.

- `upscale.wgsl` — **FSR1 EASU**. Skipped at scale 1.0.
- `sharpen.wgsl` — **FSR1 RCAS**. Identity at 0. `FSR_RCAS_DENOISE` deliberately off
  (header has the measurement). **Sharpness 1.0 is the ceiling**: past it `4*lobe + 1`
  goes negative and the filter inverts. More sharpening comes from `rcas_radius`.
- `contrast.wgsl` — **not FSR**: DoG band-pass on luma ("clarity") on a 1/8 plane.
  Identity at 0. **Signed** -1..1 (negative softens).
- `grade.wgsl` — nine grading knobs in **one** pass, in **OKLab** (temperature/tint
  are exactly its chroma axes; header says what they are not). Vibrance is a chroma
  scale faded out by `VIBRANCE_KNEE`. A tenth knob belongs here. Neutral is skipped, since the sRGB/OKLab round trip loses LSBs and
  `fsr-selftest` asserts byte equality. `check_grade` fails a knob that moves nothing
  (the `GradeParams`/shader `Params` copy-paste trap).

**Order is EASU → grade → clarity → RCAS**, and load-bearing: grading after a sharpen
amplifies and clips its halos.

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

## The Image tab's panels

The Image, Video and Settings tabs are **workspaces**, the Refine & adjust dialog's shape
(`.workspace`): a rail (`.wsrail`) of panel toggles, the picture on a **stage**
(`.stage`, `#result`; a click opens Refine & adjust), floating panels
(`panels.js`, `--ws-panel-w`), and a **status card** (`.statuscard`: `#err`/OOM, the
preview, `#prog`, `#loopnote`). Image panels: Prompts (the seed first, then the
prompts and `#refsopen`), Models (LoRAs included, *Save as defaults* in its footer),
Generation (Output upscale/Upscaler last), Image to image, Hi-res pass and Refine pass (each starting with *Run after each
image*, `#opthires`/`#optrefine`), **Info** (`#resultinfo`: size, seed pin, timings,
the rolled prompt), **Current** and **Favorites**. The rail's foot: the seed line
(`#seedline`, `syncSeedLine` — call it after any write to `#seed` that fires no
event), *Save Output* (`#optsave`), *Until stopped* (`#autorun`), Cancel/Stop,
Generate. **No action row**: Hi-res, Refine, Save to output are in the dialog.

**Every panel header says what is inside** (`syncSummaries`, `syncModelsSummary`). A
line turns `.foldstate.on` when the panel departs from the plain path (>1 image or
loop, source picked, a pass ticked to run after each image, a separate Hi-res/Refine
model, LoRAs or a memory mode), and **the rail button's dot mirrors any such line in the panel**,
header or nested fold (`syncRailDots`). Driven by a
delegated `input`/`change` listener on the tab; a redraw that fires no event must call
it (hence `loraPicker`'s `onChange`).

Break silently:

- **The panels kept the folds' ids** (`fold-src`, `modelsbox`, …; `PANEL_DEFS`'s
  `el`), and an element's `open` property opens its panel — so `$('fold-src').open =
  true` on a source pick still works. Open state and position are per browser
  (`yasdig.panel.<id>`); the first time, Prompts and Generation are open and Current
  sits bottom-right.
- **A hidden tab has no size**: `showTab` calls `layoutImageTab`/`layoutVideoTab`.
- **The page stops scrolling on a workspace tab** (`.app:has(> .workspace…)` is the
  window's height less the fixed footer, `--foot-h`); panels scroll their own bodies.
- **The page frame**: `.tabs` is sticky and `.powered` is a fixed footer; every
  `.workspace` has `isolation: isolate`, so its panels' z-indexes (up to 1000) stack
  inside it and never over the top bar. The dialogs are in the top layer.
- **Panels resize vertically** (`resize: vertical`); `panels.js` saves a dragged
  height (`p.h`, from a `ResizeObserver` on the inline height) and a header
  double-click clears it with the position.
- **Settings' panels are built from `FIELDS`** (`markup()` in settings.js, one per
  `SECTION`; `SUB()` a heading inside one, `NOTE()` a grey line; `both` widens the
  panel too — Default prompt, for long variable phrases; `rail:` shortens a button's
  label). Per model is static markup (`#panel-set-per-model`, its Save/Clear in the
  footer), placed by the `PER_MODEL` marker. Only rows with a `key` are settings
  (`rows()` in both modules).
- **Picking a hi-res/refine model refills that fold** (`fillPass` from `passModel`),
  since `swap_model` is wholesale. Blank pass-CFG placeholder follows the picked model.
- **Save as defaults skips a fold holding another model's numbers** (`foreign` in
  `saveModelDefaults`).
- **Hi-res scale is a number box; mirroring selects grow an option** (`upOptions`,
  `setUpFactor`) since a select shows its first option on no match. Clamp on `change`.

## The dock

Convert's and Settings' columns end in `<div class="dock">` (`position: sticky;
bottom: 0`): error box, progress, `.runstop`, amber button. A dock with nothing
showing hides itself by CSS. The workspaces and the Refine & adjust dialog have a
rail foot and a status card instead (same rules for the preview and the caps); every
status card gets a × (`initStatusCards` in app.js) that empties its `.err`/`.sub`,
shown by CSS only while one holds something.

- **It must stay the column's last child, opaque.** Page scrolls, card is the
  containing block.
- **Preview fold hides by CSS** (`.previewfold:not(:has(img:not([hidden])))`);
  `jobs.js` only toggles `img.livepreview`.
- **Growth is capped**: error box 40vh, preview `min(--preview-h, 35vh)` — or an OOM
  log pushes the button off-screen.

## Layout boxes

MOSS-Image's JSON layout prompt (`src/layout.rs`, `web/js/layout.js`). The model card's
compiler (`skills/moss-image-prompt-enhancer/scripts/compile_prompt.py`) is the spec
`layout::compose` follows byte for byte (compact JSON, key order, floor near / ceil far,
`abs_centralized` `yxyx`); its `references/layout.md` cup is the fixture, in both
test files.

- **Boxes are stored as 0..1 fractions and composed per sd-cli call at that call's
  size** (`layout::conditioned`: txt2img at `tpl.width/height`, the source pass at the
  fitted size, Refine at `raw_w/raw_h`, Hi-res at the init's size). The JSON is never
  stored as the prompt; `Pending.prompt` stays the caption.
- **Redraw and Extend get the caption alone** (crops). Edit doesn't reach MOSS.
- **`Profile.layout`, request-only like `references`**: refused off MOSS by
  `Profile::validate` (`takes_layout`), lifted across `swap_model` to another MOSS
  only. In `EmbeddedProfile`; an invalid one read from a PNG is dropped with a note.
- **Variables roll inside boxes at compose time**, off the image's seed, so every pass
  rolls the same words; the profile keeps them typed.
- **MOSS skips sd.cpp's `(word:1.2)` parser** (`prompt_attn_range {0,0}`), which is
  why JSON's brackets survive. A family that parses weights would mangle it.
- Page: the stage's *Picture | Layout* switch (`#stagemode`) and `#layoutstage` are
  **siblings of `#result`**, absolutely placed beside the rail (`render()` rewrites
  `#result`, and its `:empty` placeholder must keep working). `render()` calls
  `showPicture()`. `layoutPayload()` is `null` unless the family takes layouts.

## Image to image

The *Image to image* fold (code says "Start from"): a picked source makes Generate run
`/img2img`. `web/js/source-slot.js` is the factory; the Video first frame is a second
instance. `Engine::img2img` is `run` with a source instead of txt2img.

- **The source is a `work/output/` stem**, resolved by `Engine::output_source` (shared
  with video), guarded by `workitems::valid_id`.
- **Copied into scratch** (`{id}-0-src.png`), never adopted — invariant 8.
- **`fit_to` only shrinks, and snaps only the width to 64** (snapping both distorts
  aspect up to 6.7%). Inside the budget it's untouched so the caller can `fs::copy`.
- **Uploads** (`POST /upload`) land in `work/output/` via `Store::import`, stem minted
  by `new_id`. Body is raw PNG bytes; `http.rs` `body_limit` is an exact path match;
  over-limit answers 413. **PNG only in Rust** — the page re-encodes via `<canvas>`.

Page:

- **`data-when` swaps the panel; `syncMode` is the only writer.** Base size/Output
  upscale are `text`; Strength/Working size are `source`; Steps and CFG serve both.
  The slot's `onChange` calls `syncMode` and opens the fold.
- **`genPayload` returns `[route, body]`**, so batch/loop/after-each work over a source.
- **A reopened work item always sets the slot** — to its source or to nothing.
- **The fold summary says `from …` whenever a source is picked.**

Paste is a document listener per slot, gated to its visible tab.

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

## Upscalers

The initial enlarge in `Engine::run` is EASU or a model (`Profile.upscaler`, `None` =
EASU, names under `Config::upscaler_dir`). An ordinary generation knob.
**`pipeline::enlarge` is a free function** (takes `&Fsr`, `&SdRunner`) so `upscale` runs
without an `Engine` (whose constructor sweeps `work/`).

**Two routes by file type** (`sr::identify`): ESRGAN → sd-cli; Compact, SPAN, SwinIR →
`src/sr/` on the FSR device. DAT and HAT are identified and refused.

sd.cpp `-M upscale` facts:

- **A model it can't build exits 0** and saves the input unchanged. `SdRunner::upscale`
  greps the stderr tail (`run_tail`) and `after_model` refuses a result that didn't grow.
- **RRDBNet only.**
- **Scale is in the weights**, read off the output size.

**Output is `fsr::out_size(input, factor)` whichever ran.** `after_model` shrinks an
overshoot with Lanczos3 and finishes a shortfall with EASU at `factor / scale`
(`an_esrgan_enlarge_lands_on_the_easu_size`).

`sr/weights.rs` reads safetensors and both torch formats (zip with stored storages, and
legacy) via a pickle subset. `list_upscalers` greys out what can't run;
`check_upscaler` refuses it early. Names go through `valid_lora_name`.

`src/sr/` breaks silently:

- **Legacy torch is recognised by all eight magic bytes** (a safetensors header length
  can start `0x80`).
- **Channels-last everywhere** (`[H·W, C]`); conv weights reordered at load to
  `[out][ky][kx][in]` (`gpu::conv_weight`).
- **Swin window partition/shift/mask are index maths in `attn.wgsl`**; `sr-selftest`
  checks against a literal transcription.
- **`Gpu::tensor` flushes before `write_buffer`** (a queued write overtakes unsubmitted
  commands).
- **Tile sized from the device**: `Fsr::new` asks for the adapter's storage-binding
  limit (capped 1 GiB); `plan_tile` shrinks until `Net::peak` fits.

Network check is **`sr::consistency`** (shrink back, compare; working > 30 dB). SPAN's
traps are in its header (`Conv3XC` folded from training weights; SPAB's inplace SiLU).

Page: `loadUpscalers` starts **before** `loadModels`' first `await` (Settings awaits
`upscalersReady`). `upscalerOptions` excludes "(missing)" entries.

## Favourites

`user/favorite/` holds the same three files as `work/workitems/`, same ids, via
`workitems::Shelf`. **A favourite is a move** (`Store::favorite`/`unfavorite`, copy
then delete), and **Current lists both shelves** (`Store::listing`,
`Store::output_listing`, `favorite` on each entry), so *Delete all* — which goes
through the one-directory `list`/`list_outputs`, as Close server's clear does — never
counts or reaches a starred item while the grid still shows it. Under `user/` because
nothing sweeps it.

- **`Shelf` → directory only in `Store::shelf_dir`**; all path/load/list/delete take a
  shelf. `save` doesn't (generations archive to Current).
- **`delete` touches one shelf** (`a_favourite_survives_deleting_the_original`).
- **The shelf is a path segment** (`/favorites`, `/favorites/{id}/thumb.png`,
  `/favorites/open|delete|add`); `JobKind::Open` carries one.
- **Un-star is `/favorites/remove` (moves back); `/favorites/delete` deletes.**
- **An id on both shelves** (kept by the copying build) lists once, as the
  favourite. `copy_picture` keeps mtime (the output gallery sorts by it).
- **Reopening a favourite is copy-in/copy-out** (`Display::open_in` takes a `Shelf`).
- **`list`/`list_outputs` stay one directory**; only the routes' listings merge.

Page: **two grids, two panels** — `shelfGrid` instances in `workitems.js`, Current
(`/workitems`, which lists starred items too) and Favorites (`/favorites`), each with
its own mode radios, marks and Delete; *Delete all* is Current's only.
`loadWorkItems`/`markGrid` drive both; each drops responses that lost a race. **Favouriting is the tile's star** (`star`/`wireStars` in `gridmark.js`):

- **The star is a sibling of the tile's button inside `.tile`**, never a child
  (nested buttons, and the click would open the picture); handler stops the event
  too. Grid code addresses tiles as `button[data-id]`/`[data-stem]`, not `button`.
- **A starred tile reads from its own shelf** (`home(w)` in `workitems.js`; outputs
  find a stem in either gallery). `outputGrid` draws stars only when passed `star`.
- **`gridMarks`' `ids()` is what the actions may reach** — on Current it leaves
  favourites out, so they can't be marked (`locked`) or counted by *Delete all*.

`gridmark.js` holds the actions table (`value`, `button`, `mark`, `label`, `route`
thunk, `key`, optional `all`), today delete alone, with one shared marked `Set`.
*Delete all* shows in every mode; `all.when()` hides it on favourites.

### Favourite outputs

`user/favorite_outputs/` (`workitems::Gallery`, `Store::favorite_output`, a move).
**A stem names one picture wherever it lives**: `Store::free_stem` refuses a name either
gallery holds, so `Store::find_output` searches both and `output_source`,
`open_output`, `load_output` and the thumbnail read through it.

- **`free_stem` must keep checking both.**
- **`delete_output` takes a `Gallery`** and touches only it.

## Downloads

Get models tab: `src/catalog.toml` (compiled in), `src/catalog.rs`, `src/downloads.rs`,
`GET /catalog`, `POST /downloads/{start,cancel,clear}`, `web/js/getmodels.js`. A set
per runnable family, each file keyed by its `slots()` key (or `"model"`); **a family
may have two cards** (MOSS INT8 + BF16), so its anchor, nav button and Help's family
row take its *first* card (`firstPerFamily`) — keep the default first in the toml.
A card's model is had **only at its own dest** when its family has several cards
(`has_several` in `downloads::present`), or the BF16 card reads as had beside the INT8.
`recommended = true` is curated; `Catalog::recommended` adds "fits the GPU"
(`card_gb` ≤ `sd::vram()` in GiB, `recommend_below_gb` for the small-card pick; VRAM
unknown → the curated large-GPU set). `/catalog` serves the result per card plus
`vram`; `/models` per family. Cards stay in catalog order. Help's *Intro to models* draws from `/catalog`. **curl is a subprocess** (no TLS stack in the
binary); `doctor` warns when missing.

- **The catalog is the only source of a URL or destination**; routes take an `id`.
  `parse` refuses non-`huggingface.co/…/resolve/` URLs, non-`Normal` dests not ending
  `.safetensors`/`.gguf`, unknown slots. A shared file comes from each card's own repo;
  shared `dest`s from different URLs must carry one `sha256`.
- **URLs and `bytes` were HEAD-checked by hand** (2026-10-10). Re-check when an entry
  changes; avoid gated repos.
- **`<dest>.part` until the length is right, then renamed.** Progress = `.part` length.
  A full-length `.part` finishes without asking the server (would 416).
- **Present = a scanned file fits the slot**, not "dest exists". A `dest` holding
  something else is `wrong`, never overwritten. State keyed by `dest`.
- **After the rename the file is `identify`'d** against its slot; kept even if wrong.
- **Own lane, never the job queue**; *Close server* calls `cancel_all`. Cancel keeps
  the `.part`.

**Upscalers are a table of their own** (`[[upscaler]]`, `catalog::Upscaler`): no
family, `dest` relative to `Config::upscaler_dir`, `.pth`/`.safetensors`, and "have" =
that `dest` exists and `sr::identify` names its `arch` — never a name-free match (two
4× ESRGANs are different models). `downloads::Want` picks the post-download check;
state is keyed by full path, since the two tables have different roots. On the wire
they are `upscalers`, beside `entries`, so Help's family table never meets one.

**Video cards are `[[video]]`** (`catalog::Family::Video`, the same `Entry`): `dest`
under `Config::video_dir`, `slot` a `VideoFamily::slots` key, `"model"` or the A14B's
`"high_noise"` (`parse` requires it exactly for `is_moe`). `Entry::fits` takes the path,
since only `noise_marker` tells the experts apart. "Have" scans the video dir alone —
what discovery assembles from.

Cards link source repos (`File::page`). **No licence names in the catalog**, on purpose.
Page: cards and nav are grouped by category (`categories()` in `getmodels.js`: image,
video, upscalers); `finished` moving triggers `loadModels()` (and `loadVideoModels()`);
`#getparts` opens the family's card (`openGetModels`).

## Settings in the PNG

`src/pngmeta.rs` writes two text chunks into every output `Store::save_output` and
`overwrite_output` write, and into the viewer's *Download* (`Display::download`,
`POST /download-image`, the same `workitems::output_bytes`): A1111's `parameters` text and a `yasdig` JSON record.
`POST /png-settings` reads a dropped PNG back. Gated by `Settings::embed_png_settings`
(on, page-level). Not written into the work-item archive or the live display, since
*Save to output* is the "keep this one" button.

- **The record is `pngmeta::Embedded`, curated, never the sidecar.** A `Profile` holds
  absolute paths (`model`, `components`, `extra_args`), and a PNG is a file people post
  (`the_record_never_carries_a_path`). A new Image-tab knob that should travel goes in
  `EmbeddedProfile` too.
- **Read on the server**, so the A1111 sampler/scheduler table is one table
  (`every_mapped_name_is_one_sd_cpp_takes`), and matched against what is installed: a
  missing model or LoRA becomes a `note`, never a setting the next Generate is refused
  over.
- **Chunks go before the first `IDAT`** (PIL-based readers stop collecting text
  there), and ours are replaced on re-save rather than stacked. Only `tEXt` and
  uncompressed `iTXt` are read, which keeps it dependency-free.
- **The page sends the original file's bytes**, not through `/upload`'s canvas
  re-encode, which strips the chunks. A yasdig answer goes through `applyWorkItem`; an
  A1111 one through `applyParams`, which sets only the fields it names, model first
  (its `change` refills every box).

## Memory

`Profile::memory` (`config::MEMORY_MODES`: `auto`, `ram`, `disk`),
`VideoModel::memory_mode`, `config::memory_args`; typed `sd::OutOfMemory`;
`web/js/memory.js`. A generation knob, and on `EscalateRequest`, `EditRequest`,
`RedrawRequest`, `VideoRequest` (it describes how the next pass fits). sd.cpp's
`--auto-fit` is default (`auto`); `ram` = weights in RAM, whole card for compute;
`disk` streams.

Break silently:

- **Any explicit `--params-backend` turns auto-fit off**, and `--offload-to-cpu` is
  one. `auto` = no flag; never emit the alias.
- **A placement in `extra_args` wins** (`memory_args` returns nothing;
  `memory_modes_are_one_params_backend_or_none`).
- **`[[video]]`'s old `offload_to_cpu` is read, never written**; `memory_mode()`
  decides; the page gets `memory_mode`.
- **The error is typed.** `run_tail` returns `OutOfMemory` (kind, phase, step,
  placement, mode); `oom_json` finds it through `.context()`; rides as `oom` on a
  `Notice` / `error_body`. System OOM is a SIGKILL — `killed_by_system` checks *after*
  the cancel check (`running_out_of_memory_is_a_typed_error_with_its_phase`).
- **`ram` is `*=cpu,te=disk`** (encoder released after use). `run_tail` reads the mode
  off the default part.
- **Phase is `run_tail`'s own last progress** (`end()` clears the monitor first). On
  decode sd.cpp already tiled, so only a smaller size is offered (a pass in the
  dialog: *Scale down to 0.75× and try again*). `oom_phase` reads
  failure lines before progress bars.
- **Load and stage times are parsed** (`parse_load_ms` → `Timings::load_ms`,
  `parse_stage` → `sd::Stages`), reset by `begin`, read straight after the pass. Don't
  read "generating N latent images"/"hires fix" (double counting). Loading is ~15% of
  a run — why a persistent `sd-server` is shelved.
- **Placement is only set by a run with `-p`** (an ESRGAN run mustn't wipe it); lands
  on `Timings::weights` (so `Timings` isn't `Copy`).

**System RAM is offered only when it fits**: `ram_needed` (`ram_mode_bytes`) vs
`ram_free` (`MemAvailable` after the child exits); `ramFits` keeps sd.cpp's headroom
(max of 2 GiB, 10%). **Its real limit is the GTT**: RAM-resident params are pinned host
memory amdgpu caps at the GTT (half of RAM by default). `sd::gtt()` reads
`mem_info_gtt_*`; `gttShort` greys System RAM out and puts Stream from SSD first.
`memory.js` keeps `options`/`ramFits` pure for `memory.test.js`.

**"Device lost" is a reserve, not a mode**: `Settings::gpu_reserve_gb` →
`config::gpu_reserve_args` → `--max-vram -N` on every run (page-level, `PAGE_ONLY`).
The box offers a 3 GB reserve on a lost device and **saves it at once**, then calls
`adoptSaved` so Settings' snapshot moves. **Offered once, never stepped up.**
`parse_shortfall` reads the need/budget line (by label); `budgetStarved` offers *Stop
keeping the card free*; `parse_encoder_on_card` and `-r` count are named in the box.

**Reopening does not restore Memory** (it describes this machine now, not the picture).

Page: `showError(el, msg, oom, opts)` is every sink. Buttons set a control; *Try again*
is the caller's `retry`. `post()` puts `e.oom` on the error.

## Close server

`POST /shutdown` (`handle_shutdown`, `src/shutdown.rs`), on the connection thread, in
order: **kill the child**, clear, answer, `process::exit(0)`. The clear reads the
*saved* `user/settings.toml` (the confirm quotes the same file); deletes only through
`Store::delete`/`delete_output`/`VideoStore::delete`, never favourites, never
`remove_dir_all`. Always empties `work/current/` (`pipeline::clear_scratch`); the
confirm counts dropped queued jobs. The `clear_*_on_close` fields are page-level.
**Page-level keys are marked `page: true` on their `FIELDS` row** so the *Per model*
card omits them (`fields_mark_exactly_the_page_only_keys`). The page then shows a
"stopped" note (`window.close()` is ignored); Settings' `beforeunload` stands down.

## The agent

The Agent tab (`src/agent/`, `src/llm.rs`, `src/agents.rs`, `llm/yasdig-llm.cpp`,
`web/js/agent.js`): a chat with a local VLM over its own settings, started from the
Image tab's model. A turn is one run of our driver on the job queue; nothing stays loaded.

**The driver, not llama.cpp's CLIs** (checked at v0.7.0): `llama-mtmd-cli` is one-shot
only with both `-p` and `--image` and takes no conversation; `llama-cli` talks to an
in-process llama-server over loopback TCP, which seccomp and the AppContainer refuse.
`yasdig-llm` is mtmd-cli's evaluation loop with the history from a **request file**
(never stdin: the Windows launcher hands the child NUL), `common_chat_session` for the
template, the JSON-schema grammar and the thinking split.

- **Stdout is framed** (`@yasdig-llm 1`, `P load|read`, `T "<json>"`, `C`, `D`, `E`);
  any other line is a library log and ignored (`llm::parse_frame`). `PROTOCOL` is checked
  both ends. Exit 0 without a `D` is a failure; `E context` is the typed
  `ContextOverflow`, retried once with half the history and smaller pictures.
- **Bumping llama.cpp**: re-read `tools/mtmd/mtmd-cli.cpp` against the driver;
  `common_chat_session` (ctor, `apply_sampling`, `finish`), `common_chat_templates_inputs`
  (`json_schema`, `enable_thinking`), `mtmd_tokenize`, `mtmd_helper_eval_chunk_single`,
  `llama_process`; the CMake options `llm/CMakeLists.txt` forces (`LLAMA_BUILD_MTMD`,
  `LLAMA_OPENSSL`, `LLAMA_SUBPROCESS`, `MTMD_VIDEO`) and the targets `llama-common`, `mtmd`.
  `docs/building.md`'s clone tag and `release.yml` together.
- **`serde_json`'s `preserve_order` is load-bearing**: llama.cpp's grammar follows the
  schema's property order, and `notes` (the scratchpad) must come first.

Models (`agents::scan`): GGUF metadata only (`general.architecture` = `clip` is a
projector; an LM needs a chat template). **Paired by folder** — one LM and one projector
in a folder are one agent — because `clip.vision.projection_dim` is the LM width for
Qwen-VL and Gemma but 14336 beside Ministral's 4096; several per folder pair by width
only when unambiguous. **`agent_dir` is excluded from `scan_images`**, or a Qwen agent is
offered as a text encoder. Catalog `[[agent]]` cards: each in its own folder
(`check_agents`), sampling/`image_side`/`n_ctx` matched to the file by dest + length
(`Catalog::agent_for`). No content goals in notes or docs.

Break silently:

- **A chat's settings are a `/generate` body holding only what the agent set**
  (`GenerateDto`, `chat.settings` as JSON): `/chats/new` makes `{template}`, so a round
  (through `GenerateDto::run_plan` like the button) is that model's registry profile
  plus the agent's keys. `with_turbo` adds the catalog Turbo LoRA (steps/CFG/sigmas)
  when there is no `loras` key — `toolkit::locks_for` reads the same absence. A model
  switch (`knobs::write`) removes steps/CFG/size/sampler/LoRAs/parts, keeps the prompts.
- **The rules are Settings', read per turn** (`knobs::Rules`, `[agent]`; `toolkit::resolve`
  loads them): a whitelist of keys, no ask-first, no bounds but `hard_bounds`. A chat
  carries none. An old `chat.json`'s `pending`/`awaiting` read as `rejected`/`idle` (serde
  aliases); an old `[agent]` of tables reads as the defaults (`key_list`).
- **One Send; the agent picks the action.** `/agent/send` is always a chat turn, whose
  schema branches are named for the request — `talk`, `draw`, `describe` (only when the
  turn shows pictures), `improve` (a `goal`) and `compare` (`vary`, `values`, `count`;
  absent when no `knobs::COMPARABLE` key is offered). `apply_reply` still reads the old
  `none`/`generate`/`session`. An improve calls `session::begin` (goal, checks, rounds and
  sweep cleared, `first_round`, `reference` = the newest user message's pictures,
  `mode = Plan`) and `Next::Turn`. `Mode::Describe` is never started; kept so old chats read.
- **A comparison is the server's, not the model's** (`Session.sweep: Sweep {knob, values,
  before}`): `sweep_from` checks every value through `knobs::check` (whitelist, locks,
  `hard_bounds`; strings parsed per knob — the grammar can't type array items) and needs
  two; `rounds` = values. `start_round` writes `values[k]` (`knobs::write`) and leads the
  round's summary with it; seeds are kept. In a sweep the plan turn writes checks only,
  a critique answers checks/keepers only, and after the last one `sweep_table` is posted
  as a note and `Mode::Verdict` (`best`) runs: `before` is written back, then `best` lands
  as an ordinary undoable change — so Undo returns to before the comparison.
- **`Session.reference` is staged first** in plan and critique turns (`Turn.reference`
  counts them), so "Picture 1" stays the round's first picture.
- **The agent sizes its work**: chat `count` (1 unless said), a plan's `rounds`/`count`
  (`Session.rounds`/`per_round`, 0 = `SESSION_ROUNDS`/`SESSION_BATCH`), a critique's
  `count`/`seeds`; `knobs::MAX_BATCH`/`MAX_ROUNDS` are the only caps.
- **`Slot::Agent`**: rounds and passes never take the Image tab's picture. `open_in`
  into `Slot::Agent` leaves a **live** entry where it is (it may be on the Image tab).
- **`Slot::Chat` is the Agent tab's viewer** (`/workitems/open {pane: "agent"}`,
  `JobKind::Open.slot`), apart from `Slot::Agent` so a round never evicts the picture
  looked at; it takes a live entry out of `Agent` but leaves one in `Image`
  (`stays_where_it_is`). Page: `PANES.agent`, `#lightbox.onagent` hides the mirrored pass
  model/steps/CFG, and `escalateSettings`/`passPick`/`passSteps` send none on that pane,
  so a pass runs on the picture's own profile.
- **A leg is `Kind::Agent`, `total: None`**, stepped like a loop (`is_stepped`), never
  the loop (`replace_loop` needs `is_run`). Stop marks `stopping` **and** kills;
  `agent::step` checks `Queue::stopping`. A panic marks the chat failed in the worker
  loop; `ChatStore::settle_all` on start turns a busy chat `interrupted`.
- **Every chat write is `ChatStore::update`** (one lock, `.part` + rename, `rev` moves);
  the worker re-reads at each step, so the person's edits mid-turn survive. Timestamps
  are `u64`: serde_json can't read a `u128` inside the internally-tagged `Msg`.
- **The knob table is the one source** (`knobs::KNOBS`): the panel, the schema and
  `apply` read it; `agent_knobs_agree_with_fields_and_validate` pins it to `FIELDS` and
  `Profile::validate` (`MAX_CFG` is in `config.rs`, `settings.js` and the inputs).
  `apply` never trusts the grammar (no float ranges, no property count): unknown, off or
  locked keys are dropped with a note, numbers clamped to `hard_bounds`.
- **Locks** (`toolkit::locks_for`): a `Catalog::distilled` file locks steps and CFG;
  `ImageFamily::cfg_locked` (recipe CFG 1) locks CFG; a CFG lock locks the negative. **A
  Turbo LoRA locks nothing** (`Locks.turbo` is advice: what it was tuned for), as the
  Image tab's Turbo only sets steps/CFG — MOSS's DMD LoRA takes a real CFG. `with_turbo`
  fills steps/CFG only where the chat has none, and its sigmas only with its own steps.
  **Locks are the model's, not the chat's**: with Model allowed, a locked knob is still
  in the `change` schema and `compare`'s `vary`; `knobs::apply` writes `model` first and
  checks the rest against `locks_of` the new one (`ModelChoice.locks`, from `runnable`),
  and a comparison that switched checks its values the same way. Without it, "best CFG?"
  on Flux had only the model to vary and wrote `name:3` as model values.
- **Pictures a turn sees are staged into scratch** at the agent's `image_side` and
  removed after (invariant 8; `user/` isn't a sandbox root on Windows). A chat keeps its
  own copies under `img/`, so deleting a work item never breaks it.
- **Only the newest pictures are images** (`context::messages`); older rounds are their
  summary line, older turns their `notes`/`reply`. Roles alternate (merged), ending on
  the user side — Mistral's and Gemma's templates refuse otherwise.
- **`Settings.agent` is a table declared before `[variables]`** (`ValueAfterTable`), plain
  values only. Page-level; its `FIELDS` row (`kind: 'agent'`) is drawn by `agentperms.js`
  from `GET /agents`' table and read back in the server's order (sets sorted), or the
  dirty check never settles.
- **Skills** (`agent::skills`): built-ins compiled in, `user/skills/*.md` re-read per
  turn, an `id` match replaces; `BUDGET` drops whole skills, lowest priority first.
- **A film** (`Chat.film: Film`, `session::apply_story`): a chat turn's `film` action
  hands over to a `Mode::Story` turn (its own schema, `n_predict` 3072), whose board is
  **rewritten whole**, never patched; what survives is matched by the server — a member
  keeps its picture by name and an unchanged look (or when it is the person's), a shot
  its keyframe when its words, cast and cut are unchanged. **Nothing is drawn until the
  person presses *Draw the cast*** (`/chats/film {draw}`, `board_advance(…, draw)`), so
  the looks can be read first. **Board pictures run on `Film.image_model`**
  (`board_settings`: the chat's settings switched as a model change switches them; the
  agent's `image_model` pick, else the chat's). On one that `takes_references`
  (`Film.sheets`) a character gets a **turnaround sheet** (`CastSheet`, 2:1): a drawn
  one's sheet first and its front picture drawn from it, the person's picture first
  and the sheet from it — so both are one person. Keyframes reference sheets; video
  references stay the single front picture (`stem`). `Next::Film` then draws what is missing (`board_step`: the cast first, since keyframes use their pictures; a
  keyframe for shot 1 and every cut unless `Film.references`, H3's), and
  `board_advance` sets `Phase::Ready` and ends the leg. **Every board picture is also an
  output** (`stem`, `key_stem`): a shot's references and first frames are output stems.
  The video model is `Chat.video`, the Video tab's as the person last sent; `film_off`
  says why none can be made (`Rules.film`, no image model, no video model, one that
  doesn't `chains()`). A new chat's settings have no `prompt` key, which `GenerateDto`
  requires: a board picture inserts its own, a round an empty one. A film's turns get the video family's skills too
  (`skills::select` takes both families' names, which never collide).
- **No `data-help` on a `<button>`**: `wireHints` would nest a button in it.

## The job queue

`src/jobs.rs` (`Queue<T>`, generic for GPU-free tests), `Work`/`diffuse` in `main.rs`,
`web/js/jobs.js`. Every diffusion-lane job (run, Refine/Hi-res press, clip, clip
enlarge, conversion) is an entry, stoppable in the Jobs dialog. The display lane
(`pipeline::Display`: open, adjust, open-output, save) is separate.

**A run is handed out one image at a time** (`Queue::next`, `run_image`, `image_done`):

- **A button press runs at the next image boundary** (`priority`: after the running
  entry and any waiting priority entries).
- **A loop gives way**: after each image, if anything waits, it goes to the back.
- **Stop**: a waiting entry is removed (request answered, pin released); a running run
  is marked `stopping` and ends at the image boundary; other sd-cli work is killed
  (`monitor.cancel`); FSR/`sr` work answers 400 (it will finish).
- **Cancel generation** (`POST /jobs/skip`): `Queue::skip` marks *before* the kill;
  `diffuse` reads `Queue::skipped` and calls `image_done`, not `fail_run`. The skip
  counts toward `done`. `unskip` when no child was found. Refine/Hi-res presses use
  `/jobs/stop`.

Break silently:

- **A worker panic fails the job, never the thread**: `catch_unwind` + `Queue::abort`.
  `Fsr::process` checks sizes (`check_fits`, `max_side` = adapter's texture limit) and
  runs in a wgpu error scope (the uncaptured handler panics). `Settings::max_image_side`
  (default 8192, page-level) is checked by `Engine::check_side` before any work.
- **A run answers `{job}` at once; images come via `/jobs/latest`** (`set_latest`,
  `latest_seq`). Refine, Hi-res, Video, Convert, Enlarge answer on their own request
  (`enqueue_wait`) and also publish; `takeResult` skips one already shown.
- **Everything a run needs is captured at queue time — except the loop.** There is
  one (`Queue::push_loop`); a loop request updates it (kind, label, work; id and
  `done` kept), and the page re-sends the panel on change (`syncLoop` from
  `syncSummaries`: debounced, deduplicated, skipped on a blank prompt, `update_only`
  so a stale edit never restarts an ended loop).
- **A run's failure has no request**: `fail_run` ends it with a `Notice`; the page's
  first poll adopts existing notices.
- **`/generate` validates only the count**; everything else surfaces as a notice.

The display lane shares FSR as `Arc<Mutex<Fsr>>`; the diffusion lane locks per FSR
step, never across sd-cli (`enlarge` doesn't lock around ESRGAN). `Retained::update`
writes back only over the entry it read; an `/adjust` that loses is
`pipeline::Superseded` → 409, which the page ignores (never null `S.imageId` on it).
**Stop/skip kill under the queue lock** (`Queue::stop`/`skip` take the kill): checking
then killing could hit the next image's sd-cli. **`Work::pinned()` is the one list of
pinning kinds** — no `_` arm.

## Live preview

`--preview proj` on every txt2img/img2img (`base_cmd`) into `work/current/preview/`,
served as `GET /preview/{n}.png`. `Monitor::with_preview` holds the dir. Video too,
in its own shape (below).

- **Numbered files plus an `IEND` test** (sd-cli writes in place). `newest_complete`
  takes the newest complete `p-NNNN.png` and prunes older ones only.
- **The dir is emptied in `run_tail` before spawn**, not in `Monitor::begin`.
- **`Monitor::preview` is `None` whenever `progress` is.** The page swaps a blob only
  once fetched.
- **Video is one file, `preview.avi`** (`VIDEO_PREVIEW`; sd-cli fills no `%d` for a
  video preview and rewrites it in place each step). `Monitor::video_preview` parses
  it per (length, mtime) with `avi::read`, keeps the last whole version when a read
  lands mid-write, and bumps `seq`; `/jobs` `progress.video_preview {seq, frames,
  fps}`, frames at `/preview/video/{seq}/{k}.jpg` (the AVI's own JPEGs). The page
  loops them on `#vlivepreview` (`drawVideoPreview`), swapping only whole versions.

## Redraw

**One dialog, *Refine & adjust*** (`#lightbox`): `viewer.js` owns it (open/close,
pane, History, the look) and `web/js/redraw.js` is its passes (Refine,
Edit, Extend, Scale, the pass prompt, *Refine with*, Compare), hooked in through
`onLook`/`onOpen`/`claimLeft` so viewer.js never imports it. Code says `redraw` for the
area pass: `src/redraw.rs` (pure geometry/compositing), `Engine::redraw`, `POST
/redraw`, `SdRunner::redraw` (`img2img_cmd` + `--mask`). The mask decides the Refine
panel's main button (empty → `/refine`, painted → `/redraw`). **A panel with actions
pins them in a footer** (`.fpfoot`, under the scrolling body): the others above, the
main one last and amber — Refine: Eraser · Clear, *Refine*; Edit: *Edit*; Hi-res: *Hi-res*; Extend: *Extend*; Scale: *Apply crop* (live
only while the Crop fold's box is up), *Scale*; History: *Apply adjustments* (it adds
a step). **Each pass panel ends with its own *Prompt for this pass***: Refine's is
`#rprompt`, Hi-res's `#hprompt` (both mirrored), Extend's `#rdextendprompt`; Edit has
its own instruction box (`#rdeditprompt`) so it never becomes the refine prompt.
**The Refine and Hi-res panels' settings are the Image tab's controls, mirrored**
(`web/js/mirror.js`: `data-mirror="rsteps rcfg"` builds `dl-<id>` fields from the
source's label and control; a write sets the source and fires its event, then every
field re-reads — so `fillPass`, the summaries and the loop hear it). `hiresPass` and
`refine` read `#hiresmodel`/`#refmodel` directly, so Outputs uses them too. *Save to output* is in
the dialog's rail on the Image pane (`saveOutput`). Whole-image passes go through
`image.js`'s `escalate` (an error sink and a `land`). Strength box: whole-image value *is* `#rstrength`; area has its own
(`syncStrength`).

- **Scale** (the fold `#rdfold-upscale`): down is `Display::resize`, `POST /resize
  {factor}` — display lane, Lanczos on `raw`, snapped to 8 (`resize_size`), a step
  through `stepRequest` like a crop; up is `Engine::upscale`, `POST /upscale`, through
  the queue. Both restart the detail filters. The mask scales with the picture when
  the shape holds (`ensureMaster`), so a pass's OOM box can offer **Scale down to
  0.75× and try again** (`scaleDown` in `dialogErr`).
- **A pass's answer goes to the pane it was pressed on** (`passLander()` taken at
  the press → `onPassResult`: `takeResult` on Image, the Outputs handler only onto the
  same `image_id` still open), never to whichever tab shows when it lands.

**Each pass has its own prompt** — `EscalateRequest.refine_prompt`/`hires_prompt`,
redraw's `prompt` — via `pass_prompt` (blank → the image's own, variables rolled off
its seed), named by `step_label`. Not on `Profile`, never in `Pending.prompt`/sidecar,
page-level. `#rprompt`/`#hprompt` ride in `escalateSettings()`; the dialog box wins.
`--mask` works on every family (`denoise_mask`).

- **Composited in pixel space**: `redraw::composite` via `redraw::feather`; pixels
  beyond the fade are the original bytes (`untouched_pixels_are_the_original_bytes`).
- **The fade goes outward, sd-cli redraws past it**: `feather` is 255 over the mask;
  `diffusion_mask` dilates `3·soft + 8` (`the_model_redraws_past_the_whole_fade`);
  `context_rect` grows the crop by the same reach.
- **The crop is enlarged to the model's size** (`work_size`, both axes snapped — fine
  since the patch is resized back exactly).
- **`GenResult.step` is in `sameImage`**, or a redraw compares equal and is dropped.
- **Seed is `p.seed + step`.**
- **The mask view lies over the image** (display-sized; master mask natural-sized). It
  paints only while the Refine panel is open (`#lbview.masking`); its pointer handler
  skips what the pane took as a pan.
- **Outputs open in the same dialog and their passes run there**, in `Slot::Output`
  (`/outputs/open {stem, model?}`, the page's model as `fallback` when the sidecar's
  isn't installed; with none it still opens). `diffuse` publishes only `Slot::Image`
  results as latest, or an Output's refine would land on the Image tab.
  `opened_from` **survives every step**, so *Save changes* writes what is shown. ←/→
  are locked while a pass runs. *Refine with* (`pickModel`, on Outputs) →
  `POST /image/model` → `Display::set_model` (wholesale swap, slider starts kept — zero
  for outputs). `GenResult.model` shows it.

### Extend

`Engine::extend`, `POST /extend`, drawn with **FLUX.1 Fill** (`ImageFamily::FluxFill`,
`img_in` 384 wide); `Config::fill_model()` enables it. **A fill-only family never
generates**: `ImageFamily::fill_only`, hidden from selects/`#getparts`, refused by
`Engine::model`.

- **`SdRunner::fill`, never `img2img`**: `--strength 1`, `--steps p.steps`; `--guidance
  30` in `flags()` (`a_fill_pass_is_full_strength_on_the_fill_models_own_settings`).
- **Same compositing as Redraw.**
- **One side at a time** (`Side::ORDER`), never a ring.
- **Crop is `redraw::extend_crop`** (strip + enough picture to be roughly square).
- **Blend floor `EXTEND_MIN_SOFTNESS`** (24 px), not a share of the strip.
- **Blank prompt = `EXTEND_BACKGROUND_PROMPT`**, not the image's.
- **The image keeps its own profile**; Fill's is local to the pass.

Strip is pre-filled by `redraw::grow`. No seam pass (add one as low-strength Fill if
seams appear). **Edit and rescale** is separate: `EditRequest.expand` (same
`pipeline::Expand`) draws the grown shape at base size (`edit_size`), nothing pasted
back. Both side sets are `sideSet` instances, cleared on a new image. Compare uses
`object-fit: contain`.

Dialog layout: a **rail** (`.lbrail`) of panel toggles — Refine, Edit (its button
hidden without `can_edit`), Hi-res, Extend,
Scale, Adjust, Brushes, Selection, Text, History — with Fit/1:1, Download and the saves at
its foot; the pane; and **floating panels** (`web/js/panels.js`, `.fpanel`) over it.
Refine: *Refine with* (Outputs), model/steps/CFG, strength, edge softness and brush,
the References row (`refineTakesRefs`), the prompt. Edit: the instruction, References,
the rescale sides. Hi-res: model/steps/CFG/strength/scale, the
prompt. Extend: sides, the prompt. Scale: folds Scale and Crop. Adjust: Reset, Tone/Colour/Detail. Brushes. Selection (the area).
History with Compare. A **status card** (`.statuscard`, pane's lower left, hidden by
CSS when empty) holds `#lberr`/OOM, the preview, progress and Stop.

- **A panel element has an `open` property that fires `toggle`** (`panels.js`), as a
  `<details>` does — the exclusion code reads panels and folds alike. Panels
  remembered open are opened before anyone listens; `announcePanels` re-fires.
- **Placed only once the dialog is open** (`panels.layout()` in `showFullSize`; a
  closed dialog has no size). Position remembered per panel as fractions of the
  dialog (`yasdig.panel.<id>`); History, unmoved, is held by its bottom edge.
- **z-order**: panels count up from 10 (renumbered past 900), the status card 1000,
  the colour wheel 1001, `#lbclose` 1002.
- **Rail dots** (`railDot`) are the folds' old overview: set from `viewerSummaries`,
  `drawHistory` and redraw's `syncButtons`.
- **Text is drawn by the page and baked by the server** (`web/js/texttool.js`,
  `POST /text {image_id, layer, label}`, `Display::text`): the browser has the fonts
  and shapes any script; one `paint` draws the preview and the full-size transparent
  PNG. `/text` is **Apply's step with the layer on top** (`Display::bake`, shared with
  `apply_look`; `overlay_onto`): the look is baked first, so the text keeps its exact
  colour. Fonts offered are the ones installed, found by measuring
  (`installedFonts`) — `document.fonts.check()` says yes to any name.
- **`.overpanel`** on `#lightbox` while the pointer is on a panel or the rail: the
  region box, the mask and the brush ring fade, so a slider's effect on a soft edge
  is what you see. **Not the text preview**: it is what the Text panel edits.
- **Brushes draw no mask on screen** (`brushpaint.js`): the effect is what is looked
  at. Two rings — the size, and the full-strength core at `size × (1 − softness)`.
  The Refine mask's brush has the same pair (`placeRing` in redraw.js), the other
  way round: the core is the stroke, the ring is `brush + 2 × Edge softness` (its
  fade goes outward), scaled from the picture's pixels like the mask's blur.

`zoomPane` holds fit/centre rules (`expectNew`, `keepView`; `setViewerImage(src, keep)`
keeps the view for the same picture at the same size).

### History

`Pending.history` (`Snapshot`s), `Pending.at`, `Display::goto`, `POST /history/goto`,
`GET /history/{id}/{index}.png`. Lives in scratch, and **every write-back of an edited
work item is a save** to `{shelf}/{id}.edits/` (`Retained::persist` from `store`,
`update`, `replace`; `crate::edits`): step raws, brush masks, `edits.json` with `at`,
each step's profile/label, filters, area and brush amounts. `Display::open_in` reuses a
live entry for the item (on screen or pinned by a pass), else copies the folder in
(`reopen_edits`); a folder it can't read opens the original. Moves and goes with the
item (`Store::favorite`/`delete`), so Delete all and Close server's clear reach it.

- **Folder names carry a per-run prefix** (`edits::name_for`): scratch names repeat
  across runs (job ids restart at 0) and a file already in the folder is never
  rewritten. Saving writes files, then the record via rename, then drops stale files.
- **Not saved**: an Outputs entry (`opened_from`), or one with nothing edited
  (`Pending::edited`) and no folder yet. Page-only state (the painted mask, the
  Edit/Extend boxes, the Text box) isn't kept.
- **A late pass never takes a slot back** (`Retained::put_back`, from
  `Engine::keep`): its image, displaced while it ran, is updated and saved in place
  (`a_late_pass_never_takes_the_slot_back`). It used to fall back to `Slot::Image`
  and evict the picture on screen.

- **`advance` keeps the replaced image as a step**; a pass from an earlier step drops
  later ones; `MAX_HISTORY` (12) drops the oldest after the original.
- **Pass file names carry a unique `n`** (`{id}-{step}-{n}-raw.png`).
- **`remove_files` deletes every step's files.**
- **`goto` writes back via `Retained::replace(id, was, …)`**, not `update`. The page
  locks the list during a pass.
- **Filters aren't in a `Snapshot`**; the step's profile is.
- **Compare's before half goes through `publish` with current filters**
  (`Display::view_step`, `POST /history/view`, via `Pending::step`). One `lay()` places
  before image, split line and mask; `beforeKey` + an epoch prevent stale fetches.

## References

sd-cli `-r` on families that take them (`ImageFamily::takes_references`: the Flux.2
and FLUX.3 Kleins).
**References** button in Prompts (shown from `references` on `/models`), and the
dialog's Edit and Refine panels (`data-refs-open`, `openRefs`), open
`#refsdlg`, four `sourceSlot`s (`web/js/refs.js`). **One dialog, borrowed**: a Video cut
shot opens it with `editRefs(stems, onDone, note)` — the Image tab's picks are parked
and put back on `close` with no change event (`borrower`), so nothing on the Image tab
(summaries, the loop) hears another tab's pictures. `Profile.references` = output stems,
request-only like `loras`. **Edit** in the Refine & adjust dialog (`Engine::edit`, `POST /edit`)
uses the image as reference 1 and the prompt as instruction, at the image's shape **at
the model's base size, never bigger**, resized back.

- **`-r` comes only from the refs a command is handed** (`sd::push_refs`), never from
  `Profile::references`, which every pass shares through `base_cmd`
  (`a_pass_carries_only_the_references_it_is_handed`). A Refine/area refine gets
  them only when the panel's *Use with Refine* is ticked (`EscalateRequest`/
  `RedrawRequest.references`, staged by `Engine::pass_refs`); after-each never.
- **Copied into scratch and fitted** (`stage_reference`).
- **The family gate is the server's** (`Profile::validate`: family, `MAX_REFERENCES`
  = 4, `valid_id`).
- **`references` sent whole to a model that takes them, not at all otherwise.**
- **Reference size**: sd.cpp resizes refs itself; `Settings::reference_size` (512,
  page-level) → `--ref-image-args vae_input_max_pixels=N²` (4 refs on Klein 9B: 153 s
  full, 33.6 s at 512). **Edit passes `None`.**
- **Slots' own paste is off** (`paste: false`); the dialog routes a paste to the first
  empty slot.

## Step caching

`Profile::cache_mode` → `--cache-mode`, generate and source pass only. Off by default
until measured per family.

- **Wrong-architecture mode is a warning and an uncached render**, so values are
  `config::CACHE_MODES` (`off`, `spectrum`, `adaptive`); `cache_flag` maps `adaptive` to
  `ucache`/`easycache` by `ImageFamily::is_unet`. New modes must fit every family.
- **Refine/Hi-res run on `Profile::uncached()`** (a clone).
- **The skip line is the proof**: `parse_cache_skip`, read straight after the txt2img.
- **Video not wired yet.**

## Samplers, schedulers and the Help tab

`config::SAMPLERS`/`SCHEDULERS` transcribe sd-cli's tables; `check_sampling` refuses
anything else. The page's lists are `#sampler`/`#scheduler` markup
(`every_sampler_and_scheduler_option_is_one_sd_cpp_takes`). **Re-transcribe when sd.cpp
is bumped.**

**The clone is at `b13a482`** (bumped 2026-10-18; CI builds `SD_CPP_REF` in
`release.yml` — bump both). sd.cpp references here name
functions/files; line numbers drift — search by name. Log format is `[I] msg -
file:line`; parsers match contents and have fixtures in both formats; unmeasured
memory prints `unknown`/`unlimited` (hence `parse_shortfall` reads by label).

- **Flash attention and direct convolution are `auto` per module** upstream
  (benchmarked at load); `--diffusion-fa`/`--vae-conv-direct` are deprecated and
  warn. We emit **one** `--flash-attn auto,diffusion=on` (`DIFFUSION_FLASH_ATTN`,
  DiT `flags()`, big img2img on a UNet, video) and **one** `--conv-direct
  auto,vae=on|off` (`Settings::vae_conv_direct`) — sd-cli takes the last of each,
  so ours stays out when `extra_args` names either spelling (`names_flash_attn`).

- **Auto is the absence of `--scheduler`** (sd.cpp's `default_scheduler` picks per
  model/sampler).
- **`ays` only has SD 1.5/SDXL tables**; `check_sampling` takes the family.
- **`ltx2`, `logit_normal`, `llada_image` are left out** until their families exist.

Help tab (`help.js`): a `TOPICS` table (id, title, HTML, optional `fill()`). Tooltips
point to Help rather than repeating it.

## The Outputs tab

`work/output/` as a gallery (`web/js/outputs.js`, `Display::open_output`). Click opens
the Refine & adjust dialog; ←/→ step. **No Hi-res here** (it is the Image tab's action
row); the dialog's passes run on the opened file — a diffusion over finished pixels,
chosen, like picking it as a source (see *Redraw*). Sliders here stack on baked-in
pixels, so in `open_output`:

- **Filters start at `Filters::NONE`.**
- **The retained profile's `sharpness`/`clarity` are zeroed** (Reset's `start` derives
  from them).
- **A missing sidecar is `Profile::default()`**, not an error.

Copied into scratch (invariant 8). `work_id`/`step` restored so *Save as new* lands
as `{id}-s{step}-2.png`. **Save changes overwrites, and is safe because of that copy**
(`Display::overwrite_output` → `Store::overwrite_output`): the sliders re-filter the
copy taken at open, so a second save never stacks the grade and Reset + Save restores
the original while the viewer stays on it. Only `Pending::opened_from` (set by
`open_output` into `Slot::Output`) names the file. It writes **every gallery holding
the stem** (one stem, one picture), keeps the mtime (grid order), rebuilds each tile,
and leaves the sidecar; the page cache-busts the tile (`edited` in `outputs.js`).

- **Deleting is `gridmark.js`**, shared with the work-item grid.
  **Delete all = the same request with every id.** One of four `confirm()`s
  (with Close server, Jobs Stop/Remove, and the Outputs viewer's unsaved edit).
- **`outputGrid` is the one tile** (`height`, `empty`, `onPick`, optional `star`); height must match
  `.wigrid`.
- **Arrow keys yield to a focused range input**; listener on `document`.
- **Opens coalesce like a slider drag**; `cancelPost` bumps the epoch `runPost` checks.

## The stylesheet

`web/style.css`, everything visual; markup carries classes only. **A rule names a
role, never a value**: colour, type, space, radius and column caps are `:root` tokens —
a hex elsewhere means a missing token. Ink ramp and state colour are neutral (bone)
because they sit beside a graded image. `--fold-title`/`--fold-on` for fold headings.
**Amber (`.btn-run`) is the dock's main button only** (plus Hi-res above a result,
and the thumbnails' favourite star as a glyph colour) — in Refine & adjust that is
each panel footer's main action (History's is *Apply adjustments*) and, from the
Image tab, the rail's *Save to output*;
`.btn` neutral, `.btn-danger` red, `.btn-choice` for "where you are" sets.

- **A bare `<button>` is unstyled**: every button carries `.btn` or `.hint`, except
  thumbnail grids, `#lbclose`, a panel's `.fpclose` and a Video shot's fold heading
  (`.shotfold`).
- **`:disabled` is declared after every `:hover`.**
- **`--col-*` can't be used in the media query** — the 899 px breakpoint is a literal.

`.tab` has no style — `showTab` finds tabs by it; don't delete. `doctor` checks both
web files.

## The script

`web/js/`, one ES module per area, no build step, no framework; `index.html` loads
only `js/app.js`.

| file | holds |
|------|-------|
| `dom.js` | `$`, `esc`, `post`, `numOr`, `bar`, `rememberFolds`, `ICONS`/`fillIcons`/`setPlayIcon`, `randomSeed`. No state, no imports |
| `state.js` | `S`, `PANES`, `P`, `clearPane`, `escErr` |
| `busy.js` | `IDLE_LABELS`, `setBusy`/`setIdle`, `onIdle`, `cancelRun` |
| `jobs.js` | the `/jobs` poll: Jobs button/dialog, progress lines, `onResult`, `onNotice` |
| `hints.js` | `HELP`, `HINTS`, `wireHints`, the tooltip bubble |
| `viewer.js` | the Refine & adjust dialog: open/close, `FILTERS`, sliders, `/adjust`, History, `zoomPane` |
| `panels.js` | floating panels (the dialog's and the workspaces'): open, drag, remember, rail dots |
| `mirror.js` | the dialog's stand-ins for the Image tab's pass controls (`data-mirror`) |
| `texttool.js` | the Text panel: installed fonts, the preview, the full-size layer |
| `image.js` | Image tab: its panels, models, `render` (stage + Info), escalations, runs, Save |
| `workitems.js` | the Current and Favorites grids, reopening |
| `gridmark.js` | click-to-mark actions |
| `source-slot.js` | source-image slot factory, upload/drop/paste |
| `loras.js` | LoRA picker factory |
| `memory.js` | `showError` and the OOM box |
| `redraw.js` | the dialog's passes: the Refine panel, Scale, the mask, Compare |
| `refs.js` | References button and dialog (borrowed by a Video cut shot, `editRefs`) |
| `outputs.js` | `fetchOutputs`, `outputGrid`, Outputs tab |
| `video.js` / `player.js` | Video tab; canvas frame player, `renderClip` |
| `shots.js` | which Shots rows the clip on the stage has (`shotState`) |
| `clipview.js` / `export.js` | the clip dialog; Download's WebCodecs encode |
| `mp4mux.js` / `webmmux.js` / `gifenc.js` | the download containers and GIF, pure |
| `convert.js` | Convert tab |
| `getmodels.js` | Get models tab, `openGetModels` |
| `prompt-box.js` / `prompt-weight.js` | the custom element; pure weighting maths |
| `help.js` | Help tab `TOPICS` |
| `settings.js` / `modelsettings.js` | Settings form from `FIELDS`; *Per model* card |
| `agent.js` | Agent tab: chats, the composer, panels, following a turn |
| `chatview.js` / `agentperms.js` | a chat as HTML, pure; Settings → Agent's whitelist |
| `app.js` | `showTab`, boot order |

Break silently:

- **A module's top level declares only.** Listeners, `innerHTML`, first fetches go in
  exported `init*()` called by `app.js`. Cycles exist (`video`↔`player`,
  `image`↔`workitems`, `settings`↔`modelsettings`) and only bite if read at load —
  `modules.test.js` catches it.
- **Shared mutable values live on `S`** (imported bindings are read-only). Single-writer
  state stays a module `let`.
- **`setBusy` locks one button, not the page**; it says "Queued…" until the worker
  reaches its `kind`. `render()` calls `redrawBusy()`.
- **`web::WEB_FILES` is the route table and the `doctor` list**; content type matters
  (modules need a JS MIME type). New module = new row. `*.test.js` is deliberately not
  in it (`the_table_lists_every_file_under_web` skips them).

`<prompt-box>` is the only custom element; it **wraps** its textarea/input (id stays
on the inner control), light DOM. Tabs are deliberately not components (shadow DOM
would break `setBusy`/the poll/`style.css`). `showTab` clears the pane it leaves.

## Website, licences

- **Video left nightly**: `nightly_video` is gone from `Settings` (an old file
  with it still loads — no `deny_unknown_fields` — and the key drops on the next save).
  A future unfinished feature can bring the section back.
- **Website**: `site/` (yasdig.com), deployed by `.github/workflows/pages.yml`, kept
  out of `docs/`. Short and download-first; tokens hand-copied from `web/style.css`.
  Buttons repoint via `api.github.com/…/releases` (not `releases/latest`, which skips
  pre-releases); failure leaves the Releases link. **Both buttons open a dialog
  first** (`#win-dialog`, `#linux-dialog`, `askFirst`): how to start it, then a
  *Checked* block (`checked()`): version, VirusTotal's verdicts, the SHA-256 with a
  copy button, and — when `api.github.com/…/attestations/<digest>` has one — the
  build attestation. Asset names
  (`yasdig-windows-x86_64.zip`, `yasdig-linux-x86_64.tar.gz`; the regexes still match
  the bare files older releases carry) must match `release.yml`. The Windows dialog
  also says what SmartScreen will show and why; a dialog opens only once the script
  found the file, and its own link downloads.
- **Licences**: Apache-2.0. `THIRD_PARTY_LICENSES.md` (sd.cpp, ggml and their bundled
  libs verbatim, plus the crate table from `cargo tree -e normal` × `cargo metadata`)
  is compiled in and served at `GET /licences`. **Regenerate on an sd.cpp or llama.cpp bump or a new
  crate.**

## Settings

`user/settings.toml` (defaults every generation starts from), owned by
`src/settings.rs`. `models.toml` is hand-written, read-only to the app (binary, dirs,
`[templates.*]`, `[[video]]`); `user/settings.toml` is regenerated whole on Save with a
fixed header. `user/` because `work/` gets swept.

- **`registry()` re-reads the file every call** (the worker holds its own `Config`
  clone).
- **`Settings::merged_with` and `as_profile` have no `..rest`** (see the knob table).
- **`POST /settings` is a patch** (`settings::patch`): absent = keep, `null` = clear.
- **A broken file answers 200** with built-ins plus `error`; `load_or_default` likewise.
- **`Profile::default()` reads the built-in settings**, never the user's.

### Per model

Four layers of `Settings::merged_with` (`Config::entries`, `Config::registry`):
1. `user/settings.toml`; 2. `ImageFamily::recipe()` (empty for SD1/2/SDXL — hence the
global *SD 1.x, SD 2.x and SDXL* section); 3. `[templates.*]` in `models.toml`;
4. `user/model-settings.toml` (`settings::ModelSettings`, `[model."<name>"]` of
`PartialSettings`, plus sibling `[parts."<name>"]` via `Config::update_parts`).

- **`update_model` stores only what differs from layers 1–3** (`Config::model_base`),
  compared after a typed round trip (f32 noise).
- **Unknown and page-only keys are refused**, not dropped.
- **Entries key on registry name** (file stem). Each start **parks** orphans in
  `user/model-settings.removed.toml` and restores them when the model returns
  (`ModelSettings::reconcile`); `doctor` only reports.
- **`upscaler: None` means both FSR and unset** — documented, not fixed.
  **`scheduler` uses `"auto"` as a value** (`config::AUTO_SCHEDULER`), so DiT recipes
  can pin Auto over a global `karras`.

Page: `settings.js` builds the form from `FIELDS`; a row names the control it defaults,
and takes label (`labelOf()`) and tooltip (`data-help`) from it. Only
`hires_flash_attn`, `weight_type`, `extra_args` carry their own label/`HELP`. The
*Prompt variables* section renders into `#setvarbox` (`pane` on its `SECTION` row;
`markup(pane)` per container) — so **delegated listeners are rooted at
`#tab-settings`, not `#setform`**. **`sharpness`/`clarity` are in the file, not the
form** (live per-image controls).

**Unsaved edits mark the tab** (`.unsaved`, "Unsaved changes" via `::after`) and arm
`beforeunload`; dirty = `JSON.stringify(body())` vs the snapshot `settle()` takes from
the server's answer.

- **Last in `boot()`** (after `initImage()`/`initHints()`; passes a root to
  `wireHints`).
- **Prompt/negative defaults only overwrite a box still holding the last applied
  value**; `index.html` ships them empty.
- **A stored value a `<select>` lacks is added as an option.**

## Prompt variables

`{animal}` in a prompt, `[variables]` in `user/settings.toml`, `src/variables.rs`.
**Braces, because `[x]` is sd.cpp's down-weight** — so an unknown `{name}` can be a 400
(`a_down_weighted_word_is_untouched`). **Expanded on the server**
(`variables::expand`, once in `Engine::run`/`img2img`/`video` after the seed, over
prompt and negative); `WorkItem`/`Clip`/`Pending.prompt` get the resolved sentence.
`typed_prompt`/`typed_negative` (via `variables::typed`) store the original only where
the roll changed it; the page reads `typed_prompt ?? prompt`. Reopening restores the
typed text plus the seed, which reproduces the roll. `Pending` carries them so
`Display::save_output`'s sidecar has them.

- **The roll comes from the seed** (`pick` mixes seed and occurrence ordinal).
- **Replacements are never rescanned.**
- **Only a well-formed name is a placeholder**; a well-formed unknown name is an error.
- **`Settings.variables` is declared last** (TOML `ValueAfterTable`;
  `variables_round_trip_through_the_file`). Page-level, not on `Profile`.
- **Duplicate names**: `duplicateName()` on the page and the case check in
  `variables::normalize`.

Page: one `FIELDS` row, `kind: 'vars'`; rows hold state in the DOM (`put`/`take`).

## The prompt box

Ctrl+↑/↓ and Ctrl+wheel weight the word/selection as `(word:1.2)` — entirely in
`<prompt-box>`; maths in pure `prompt-weight.js` (tested by `prompt-weight.test.js`).
Shaped by sd.cpp's `parse_prompt_attention` (`src/core/util.cpp`):

- **Nesting multiplies** — if the target *is* a group (inner text minus `:w` equals the
  target), retune it; otherwise nest.
- **`(x)` ×1.1, `[x]` ÷1.1** — `[cat]` is recognised as 0.9.
- **`\(` is the only working escape**; check `escaped()`; never emit escapes.

Break silently:

- **`{ passive: false }` on the wheel listener.**
- **The wheel yields when unfocused** (stale `selectionStart`).
- **Write via `execCommand('insertText')`** to keep undo; `setRangeText` fallback.
- **`adjust` returns `null` → don't `preventDefault()`.**

Snap with `Math.round(w * 10) / 10` then `|| 0` (negative zero). Unclamped. The returned
selection is the group's inner text, so a held key stays idempotent.

## Viewer, panes and tooltips

**Panes**: Image and Outputs share the dialog via `PANES` and `P(k)`. Image's
`result` is the stage, `info` the Info panel; the `outputs` row has only `err`;
`setResultImage` and `clearPane` use `?.` for `result`. Module state (`imageId`, `currentSrc`, `workId`) is single, so **`showTab`
must `clearPane` the tab it leaves**; returning to Image calls `restoreImage`
(`lastImage`; Outputs has its own `Slot::Output`). A pane's keys: `result`, `err`,
`refmodel`, `hiresmodel`; sliders are addressed by plain id.

**Tooltips**: `HELP` keyed by what a control *is*; viewer sliders carry `help:` on
`FILTERS`; `HINTS` is the id → text index. `wireHints()` injects icons (key from `for`,
the wrapped input, or `data-help`); idempotent, run at load and after `renderClip`.
- **The icon is inside its `<label>`, so its click must `preventDefault()`.**
- **The bubble is a `popover`** (top layer is the only thing above a modal dialog and
  outside a panel body's overflow clip).
- **The icon is `.hint`, not `.btn`.**
Tooltip text: a few sentences on how to *choose*, saying when the answer belongs to the
checkpoint.

**Viewer** (`<dialog id="lightbox">`, the Refine & adjust dialog's *Adjust* half and
its owner): post-processing is judged full size here.
**The eleven sliders come from `FILTERS`** (id = wire name; read by markup, `setPost`,
`syncPostLabels`, `setBusy`, the `/adjust` body, listeners) — never hand-write one.
Label above the track; the Adjust panel holds a fold per `FILTERS` `group`
(`viewerSummaries`, `rememberFolds`).
- **Two looks, one set of sliders**: `target` says whether they show the whole
  picture's filters or the area's; the other set is held (`parked` / `area.filters`).
  `/adjust` always sends both (`area: null` removes). `setLook(data)` is how a result
  reaches the viewer (filters, start, area, history) — not `setPost`, which is the
  whole picture's values only.
- **Crop is a history step on `raw`** (`Display::crop`, display lane, `advance` +
  `Retained::replace` → `Superseded`), never a filter: passes read `raw`. Snapped down
  to 8, ≥ 64. Keeps `opened_from` so Outputs' *Save changes* writes it. Locked while a
  pass runs (`passRunning` in jobs.js).
- **The area is composited in `publish`** (`crate::area`): global filters, the area's
  on top (`Fsr::filter`, in memory), `area::composite` by an analytic weight. Pixels
  at weight 0 are the base's bytes. Normalised to the frame, so it survives a pass;
  `Area::crop` maps it on a crop; `Area::restart` at every `Filters::restart` site;
  not in a `Snapshot` (an undo of a crop doesn't map it back).
- **An area selects by shape × colour** (`Area::weights`; `Shape::Whole` is no
  shape): the colour range is OKLCh, the grade's own space (`crate::colorrange`,
  hue unwrapped `a` in 0..360, greys never hue-selected), read off the picture **after
  the whole-picture look**. `colorrange.rs` and `web/js/colorrange.js` are the same
  maths — **one fixture table pins both** (`the_fixture_table_the_page_checks_too`,
  `colorrange.test.js`); change both or neither. Ramps are smoothstep (a linear
  fade's corner shows as a line under a strong look). `ColorRange.feather` blurs the
  colour weight only (`brush::blur_plane`), never the shape's — it has its own edge. `Area.brushes` multiplies brush
  weights by the selection (`brush::apply`'s `limit`). The wheel (`colorwheel.js`)
  lives in the dialog, hides on the panel's `pointerenter`, and takes Escape first
  (`cancel` prevented); its live preview runs the JS maths on the displayed image.
  **A press ends through `follow`** (pointerup on window, cancel, *or*
  `lostpointercapture` — hiding drops the capture), and `previewRange(null)` cancels
  a queued frame, or the grey preview comes back after the release.
- **`regiontool.js`** is the box for both: one at a time (opening one fold closes the
  other), laid over the image like Redraw's mask; its pointerdown stops the pane's
  pan, so the middle button and Space+drag pan instead.
- **Brushes** (`crate::brush`, `brushpaint.js`): a mask file plus an amount per tool
  on `Pending.brushes`, run in `publish` after the area (`brush::apply`; Blur on the
  CPU, the rest `Fsr::filter`), blended by `area::composite_by` — **one blend** with
  the area's. Masks are pixels: `crop` crops them, anything else resizes on read.
  Scratch-owned (`remove_files`; a replaced mask is deleted). `/brush` has the upload
  body limit; its answer lands like a slider's (`setResultImage`, never re-centres),
  serialized on `brushChain`, which `stepRequest` also waits for. **Five things own
  the pointer and exclude each other**: the Text panel (a drag moves the text), the
  Refine panel's mask, the Crop fold's box (only in an open Scale panel), the Selection
  panel's area box, the Brushes panel. With none, a left drag pans (`pans` asks
  `leftTaken`, which redraw.js claims for the mask and Compare's split). Adjust,
  Scale and History never take it, so they stay open beside.
- **Restore is the brush that isn't a look**: it paints History step `from` back in,
  raw into raw, **before** the filters (`brush::restore`, `Pending::restore_source`,
  `publish`'s `restore`), so a grade has no seam. `brush::apply` skips it. Keyed to a
  step, so **`advance` drops it** (Apply bakes first); a jump keeps it, and standing on
  `from` restores nothing. Another-size step is Lanczos-scaled.
- **Crop, Scale down, Apply, Text and History jumps go through `stepRequest`** (one at a
  time, after `settlePost` lets a pending `/adjust` land) and answer through the
  pane's handler (`onViewerResult`: `render` on Image, the Outputs viewer's own). All
  are locked while a pass runs (`passRunning`). Ctrl+Z steps through `stepTo`.
- **One History list** (`#lbhistory`), with Compare (redraw.js) above it.
  `open_output` seeds its first step as "Opened".
- **Closing is `×` and Escape only.**
- **Fit/1:1/Reset are `.btn-choice`**; `setFit` toggles `active`.
- **1:1 centres inline and again on `load`** (a fresh `data:` URL has no size yet) —
  `zoomPane`'s `expectNew`; a re-filter calls `keepView` so it never re-centres.
- **`setResultImage` writes both copies** (small one via `P('result')?.`).
- **`cancelPost`** bumps the epoch so a stale re-filter can't land on a new image.
Panning is scroll offsets; `.lbview` needs `min-width: 0`; the image centres by
`margin: auto`.

**Reset goes to `filters_start`** (`GenResult.start`: the retained profile's
sharpness/clarity, no grade), stored as `postStart`; double-click one slider resets it.

## Invariants

1. **Nothing diffuses over sharpened or graded pixels.** `Pending.raw` is the
   unfiltered image; every escalation reads it; display passes run once, in
   `pipeline::publish()`. This makes `Display::adjust` lossless and lets the grade
   **carry across an escalation** (`Filters::restart`) while sharpness/clarity reset
   (they're sized from the image). That asymmetry is deliberate. **One exception,
   asked for**: the viewer's *Apply adjustments* (`Display::apply_look`) copies
   `display` to a new `raw` step and zeroes the look and its start points; passes
   after it work on the adjusted picture, and History holds the untouched step.
2. **sd.cpp trims an img2img schedule by strength**, so `SdRunner::img2img` passes
   `steps / strength` (else `--steps 2 --strength 0.3` changes nothing).
3. **`MAX_STRENGTH` is 0.95** (`src/config.rs`): at 1.0 sd.cpp discards the init image.
4. **`*_steps` is a floor**, raised to `profile.steps × strength` (under-stepping
   leaves grit).
5. **Dimensions come back from `publish()`**; sd.cpp may round. `snap64()` likewise.
6. **Both display filters size their band from the image** (`clarity_sigmas`,
   `rcas_radius` ∝ longest side). `check_bands` asserts it for clarity. **RCAS holds it
   in steps** (whole-pixel radius: fine band 3.245× at 1536², 2.481× at 2048², 3.245×
   at 3072² — 1536 vs 3072 agree by arithmetic; compare within a plateau,
   768..2303 or 2304..3839). Fixing it means fractional taps, which costs the limiter
   guarantee (see `sharpen.wgsl`).
7. **`hires_max_side` gates hi-res but not refine** (`Pending::can_refine`) — not a bug.
   **The buttons are gated on the fold's steps, not the profile's**: a press sends
   `#hsteps`/`#rsteps`, which override the retained profile before the server checks,
   and a profile can say 0. So `GenResult.can_hires` is the size budget
   only (`Pending::hires_fits`) and the page reads steps with `passSteps`.
8. **Nothing a `Pending` owns lives in a durable directory.** `Engine::run` copies
   `raw` into `work/workitems/`; `Display::open_in` copies it back out to
   `{id}-0-raw.png`; sources, references, outputs and favourites are likewise copied
   in. Otherwise `Pending::remove_files` or `Scratch::drop` deletes the user's file.
   Scratch is `work/current/` (`pipeline::scratch_dir`), emptied on start and Close,
   holding one live image per slot (`Slot::Image`, `Slot::Output` in `Retained`). An
   escalated image exists only in scratch until saved, so jobs **pin** it
   (`Retained::pin`). `Pending.work_id` and `Pending.source` are ids/stems, never paths.
   **A work item's kept edits** (`{shelf}/{id}.edits/`, `src/edits.rs`) obey the same
   rule both ways: steps and masks are hard-linked (or copied) *out* of scratch, and
   `Display::open_in` copies them back *in* under fresh names.
9. **Filter a clip's frames; never re-diffuse them.** `Engine::enlarge_clip` runs
   `Fsr::process` per frame (deterministic, no flicker). Per-frame `refine` would boil
   in motion; refining a clip means a VACE `--control-video` pass. `work/videos/` is
   written through (no scratch stage); `videos::ClipDir` sweeps a half-written dir
   unless `keep()` is reached.

## Adding a per-model knob

Follow `refine_cfg_scale` for a *generation* knob, `sharpness`/`clarity` for a
*display* one.

| # | file | what |
|---|------|------|
| 1 | `src/settings.rs` | field on **both** `Settings` and `PartialSettings`, a line in `Settings::merged_with` and in `Settings::as_profile` (no `..rest`, so a miss is a compile error) |
| 1b | `src/config.rs` | field on `Profile` |
| 2 | `src/config.rs` | range-check in `Profile::validate()` (or `Settings::validate`) |
| 3 | `src/sd.rs` | consume in `base_cmd` / `txt2img` / `img2img` |
| 4 | `src/pipeline.rs` | `Option<T>` on `GenRequest`, applied in `Engine::run()` **before** `profile.validate()`; escalation knobs also on `EscalateRequest` + `apply()` |
| 5 | `src/main.rs` | `/generate` `Dto` field, `build_models_json`; display knobs also `/adjust` `Dto` + `result_json`; escalation knobs `EscalateDto` + `overrides()` |
| 6 | `web/index.html` + `web/js/image.js` | input in its fold; `syncAdvanced()`/`applyWorkItem()` fill it, `generate()` reads it; escalation knobs go in `escalateSettings()` |
| 7 | `src/settings.rs` + `web/js/settings.js` + `docs/configuration.md` | default in `Settings::default()`, a `FIELDS` row, a row in the settings field table |
| 7b | `src/config.rs` + `web/js/image.js` | architecture-dependent → a value in each DiT `recipe()`; Image-tab box → a `MODEL_KNOBS` row |

**Never "tidy" `merged_with`/`as_profile` into `..Default::default()`** — the
compile errors are what keep `Settings`/`PartialSettings`/`Profile` in step. Defaults
go in `Settings::default()`, not `models.toml`.

**A knob that defaults to another stays `Option<T>` onto `Profile`** and resolves
through a method (`Profile::refine_cfg()` → `cfg_scale`; same for hi-res;
`each_pass_falls_through_to_the_knob_below_it`, where `Some(0.0)` is guidance off).
`build_models_json` emits `null`, not the resolved value. `SdRunner::img2img` takes the
resolved CFG as an argument.

**Per-request overrides apply to a cloned profile retained in `Pending`**, so later
passes use what the image was made with. **Escalations fold an `EscalateRequest` on
top** — only knobs for a pass that hasn't happened: `refine_steps`/`_strength`/
`_cfg_scale`, `hires_steps`/`_strength`/`_cfg_scale`, and the checkpoint. Base knobs
(steps, CFG, sampler, size) stay retained.

**A checkpoint for a pass** is applied by `Engine::swap_model` **before** `apply`, and
the swap is **wholesale**: the new model brings its own `refine_*`, `hires_*`,
`hires_max_side`, `weight_type`, `extra_args`, slider starts, and drops LoRAs. The
request's knobs land on top (the page refills fold boxes via `fillPass`). The swapped
profile is retained, so `can_hires`/reset points describe what ran.
**`refine_model`/`hires_model` are the exception**: request-only, lifted across the
swap so sidecars and reopened items keep both pickers; `/generate` and `/img2img` carry
them for that reason. On the wire an escalation names it **`model`**, not `template`
(`an_escalation_may_name_a_different_checkpoint`; `escalateModels()` on the page).

`EscalateRequest.refine_cfg_scale`/`hires_cfg_scale` are `Option<Option<f32>>` with
`deserialize_with = "double_option"` (absent = keep, `null` = unset, number = pin);
`escalate_body_tells_an_empty_cfg_box_from_a_missing_one` also guards the
`#[serde(flatten)]` on the hi-res body.

**Display filters**: `profile.sharpness`/`clarity` are slider *start* points;
`Pending.filters` is where all ten are now. `adjust` writes only the latter;
`run`/`img2img`/`open` reset it from the profile (`refine`/`hires` use
`Filters::restart`). **The nine grading knobs aren't per-model**: a field on
`Filters`, a row in `Filters::table`, a line in `grade.wgsl` + `GradeParams`, a row in
the page's `FILTERS`.

**A video knob** has one struct, `VideoModel` (an explicit `[[video]]` entry replaces a
discovered one): field (`#[serde(default)]`), value in `VideoModel::discovered()`,
`VideoModel::validate()`, `SdRunner::video_cmd`, `Option<T>` on `VideoRequest` applied
in `Engine::video`, `/video` `Dto` field, input + `syncVideo()`/`applyClip()`.
Architecture-dependent values go in `VideoFamily::recipe()`; anything the page derives
from `family` goes on `VideoModelDto`.

## Conventions

- Comments explain *why*, usually with a measured number. Match that density; measure
  before claiming.
- `anyhow` throughout; errors are user-facing with the fix in them (`SdRunner::preflight`,
  `Engine::lookup`).
- std-only HTTP (`src/http.rs`), no framework.
  - **`http::allowed` is the only guard against other websites** (no login): a non-GET
    with a foreign `Origin`, or a non-loopback `Host` (DNS rebinding), is a 403.
    Absent `Origin` passes — the takeover and every older build send none, and send
    `Host: localhost` with no port, so `Host` is compared by name only. A destructive
    default must stay off: `/shutdown` clears only on an explicit `"clear": true`.
  - **The read timeout covers the request only** (`set_read_timeout(None)` once the
    body is in); a minutes-long render waits on a channel, not the socket. The 413
    drains the body, or the reset swallows it.
  - **A handler panic answers 500** (`catch_unwind` in `handle_conn`); model-file
    parsers still must not panic — `identify` memoizes only successes, so a panicking
    file breaks every `/models`.
  - **The query string is stripped before routing**, so ids are path segments or POST
    body fields. `workitems::valid_id` is a whitelist (alphanumerics and `-`) — widen
    it, don't replace it with `canonicalize`.
  - `Job.reply` carries the finished JSON body (`result_json` called in the worker).
  - Filesystem-only routes (lists, thumbnails, deletes, `/video-models`, `/videos…`,
    frames, `/outputs…`) and `/progress`, `/cancel` answer on the connection thread —
    never behind a render.
  - `/outputs/{stem}/thumb.png` generates on first request (`ensure_output_thumb`),
    via a counter-suffixed temp name and rename.
- **Every sd-cli spawn is `SdRunner::run_tail`'s, which confines it**
  (`sandbox::confine`, after the arguments are read; the builders stay plain, so
  their `argv` tests hold on every OS). `curl` for downloads is deliberately outside.
  Off by `startup.toml`'s `[sandbox] enabled = false` (`startup::sandbox_off`, read
  without creating the file and set in `Config::load`, since `doctor` and `upscale`
  run sd-cli too) or `YASDIG_NO_SANDBOX=1`; messages name the file
  (`sandbox::TURN_OFF`).
  - **Linux**: seccomp in `pre_exec` (namespaces fail on Ubuntu's AppArmor). The
    filter's jump offsets are checked by a BPF interpreter in its tests — renumber
    them with it.
  - **Windows**: an AppContainer (`yasdig.sd-cli`, zero capabilities). `confine`
    rewrites the command into `yasdig.exe sandbox-run <sd-cli> <args…>`, since std
    can't pass process attributes on stable; **the `sandbox::HELPER` dispatch must
    stay first in `main()`**, before `build_id`. The launcher re-quotes the
    arguments (`quote_into`, round-tripped by a test-only MSVC splitter), passes
    only its std handles (stdin is NUL — a console handle can't go on the handle
    list), and holds sd-cli in a kill-on-close job, so Stop's kill of the launcher
    takes sd-cli too.
  - **The container opens only what is granted**: `Config::sandbox_roots` (`work/`,
    every model folder — writable, Convert writes beside its source — and sd-cli's
    folder), as inheritable ACEs added once. **A path sd-cli is handed must be under
    one**: the `upscale` CLI stages an ESRGAN's files in scratch.
  - **Enforcement is the firewall's filtering engine**, which a third-party firewall
    can replace, so the first spawn of a run checks it (`sandbox::probe`: `curl.exe`
    inside against our loopback port; a connection that *arrives* refuses every
    run). `NO_GPU` (`ggml_vulkan: No devices found.`) under the container stops the
    run, or sd.cpp would quietly fall back to the CPU.
- **`SdRunner::run` streams; never go back to `Command::output()`.**
  - Progress is on **stdout**; stderr gets its own draining thread (deadlock otherwise).
  - `drain` splits on `\r` **and** `\n`.
  - `Monitor` parks the `Child` in a `Mutex`; `run` takes it out *before* `wait`.
  - `rate` is sd.cpp's text verbatim; `Phase` derives from its unit suffix.
- JSON out: hand-rolled `format!` for `result_json` and `build_models_json` (don't
  churn), `serde_json` for anything struct-backed.
- **Batch/loop are a queued job** (*## The job queue*). `#stop` is not in
  `IDLE_LABELS`; the Jobs poll drives it.
- **Video**:
  - **Discovered by reading headers** (`src/discover.rs`), discriminators from sd.cpp's
    `ModelLoader::get_sd_version`. **Read shapes off square tensors only**
    (`Tensors::square_dim`) — PyTorch vs GGUF order.
  - **`[[video]]` entries replace a discovered model** (by `name` or same
    `diffusion_model`); omitted fields come from `VideoFamily::recipe`.
  - **Parts are `VideoFamily::slots()`** (`PartSlot`, `dir`/`file` under the video dir,
    `optional` for H3's audio VAE and VACE's CLIP-Vision): `discover::video_components`
    fills a model, `Config::video_part_views`/`apply_video_parts` serve and apply picks.
    Names are relative to the **models dir** like image parts, so `resolve_slot` is
    shared; `video_scan` adds the image scan's files (Krea's Wan VAE in `models/vae/`).
    Picks are per run and restored from `Clip.video` (keys = field names); no saved
    defaults, like every video knob. Page rows are `parts.js`, shared with the Image tab.
  - **H3 has its own VAE** (`VaeKind::MinimaxH3`, tested before the Wan VAE arm: its
    convolutional encoder has `conv2`) and `Role::AudioVae`. Recipe: CFG 1, `disk`
    memory (`FamilyRecipe::memory`; the 18 GB encoder is past the GTT).
  - **Sound only survives in a container**: sd-cli drops audio with a `%03d` `-o`. A
    model with `audio_vae` generates into `clip.avi` (`VideoStore::prepare(id, true)`),
    `unpack` splits it into frames + `audio.wav`; the AVI is kept for download.
  - **Nothing reaches `work/output/` unasked**: only `/videos/save` writes a clip there,
    as `Settings::video_save` says (`file` default, `crate::avi` MJPEG + PCM at q95 |
    `frames`, page-level). Close server's clip clear leaves saved outputs to the outputs
    switch (it still sweeps old builds' `{id}-x{N}` enlargements, `is_enlarged_clip`).
  - **MoE expert halves are told apart by filename only** (`noise_marker`); no marker →
    `wan_dense`, not a guess.
  - A missing component is listed greyed out with the conventional path in its role
    folder. Role subfolders are ComfyUI's, for humans; the scan is recursive.
  - **`family` decides flags, grids and fps.** Grids round in opposite directions
    (`align_frames`): Wan down to `4k+1`, H3 up to `17k+5`; sizes up to
    `vae_scale_factor × 2`. The page gets `frame_grid`, `size_multiple`, `forced_fps`
    on `VideoModelDto` (`the_frame_grid_describes_the_rounding`).
  - **A clip is a PNG sequence**, not a container (no decoder here; escalations need
    frames). Playback is `<canvas>`.
  - Video Stop is `POST /cancel` and kills; error ends in `sd::CANCELLED` → "Stopped".
  - **Clips queue**: Generate clip is never locked (`pending` counts requests in
    flight; it reads *Queue another clip*), each request waits on the job queue. Only
    a change to one clip is one at a time (`busyClips`: its footer and × hide), so a
    clip's rows are never sent twice.
  - **Unsent edits are a per-clip draft** (`setDraft`/`keepDraft`, `localStorage`
    `yasdig.clipdrafts`): saved on the Shots panel's input (debounced), on switching
    clip or New clip, on `pagehide`; restored by `applyClip`; dropped when the panel
    matches the clip (`matchesClip`, via `rowMatches`). A change landing off the stage
    rewrites that clip's draft to what was drawn plus the rows it didn't reach.
  - **A finished clip never refills the boxes**: `startClip`'s `finally` calls
    `syncReady()`, not `syncVideo()` (which puts the model's defaults back), and
    `loadVideoModels({refill:false})` after a download. Only a model `change` refills.
  - **Clips open while one renders** (`clickClip` has no busy gate); a clip landing
    while another is open joins the grid without taking the stage. "Another" is
    `stageSeq`, bumped by `clickClip` and *New clip* (`#vnew`, `newClip`: `clearClip`
    and one empty row; model and settings stay), not a clip id — a null stage after
    New clip must not take a late clip, or its rows would overwrite the new ones.
  - **Current and Favorites** (`clipGrid` in video.js, `shelfGrid`'s shape): a star
    moves the clip's folder to `user/favorite_clips/` (`VideoStore::favorite`, rename
    or copy-then-delete). **An id names one clip wherever it lives**: every reader goes
    through `VideoStore::dir`, which looks in both, so no `/videos/{id}/…` URL changes;
    only `list`/`delete` take a `Shelf`, and *Delete all*/Close server's clear use
    `Shelf::Work`. `load` sets `Clip.favorite` from where it was read.
  - **No autoplay, one player**: `loadFrames` and the dialog's `show` draw frame 0
    only; opening the dialog calls the stage's `stopPlayback` (`initClipView`'s
    `onOpen`), and `showTab` stops it off the tab.
  - The size boxes snap up to `size_multiple` on `change` (`snapSize`); each shot's
    Frames to the frame grid (`snapShotFrames`, also on a model change). **A Frames
    box's `min` is on the grid** (`frameSteps`): a number box steps from its `min`,
    and H3's round-up snap undid every step down from an off-grid one. There is no
    clip seed: each shot has its own.
  - **The clip dialog** (`#clipbox`, `web/js/clipview.js`, its own player): Enlarge,
    History, Download, Save to output — no shots. Opened from the stage's canvas, or an
    Outputs ▶ tile (`openOutputClip`: play and Download only). **Enlarges are versions inside the clip**
    (`work/videos/{id}/v{n}/`, `Clip.versions`, always from the original's frames),
    served at `/videos/{id}/v{n}/frames/{k}.png` and `…/v{n}/clip.avi`
    (`clip_version` parses `{id}` or `{id}/v{n}`; `v0` is refused). *Save to output*
    is `POST /videos/save {id, version}` → `{id}[-v{n}].avi` (or a frames folder).
  - **Per-model video defaults** are `[video."<name>"]` (`settings::VideoKnobs`,
    typed, every field optional) and `[video_parts."<name>"]` in
    `user/model-settings.toml`, apart from the image tables (a name may be both).
    `Config::video_models` lays them over `video_models_untuned`, which is what a
    save compares against (`update_video_model`: typed round trip first, `memory`
    against `memory_mode()`); a saved part is kept only when it isn't discovery's.
    `POST /model-settings {kind: "video"}`; `reconcile` takes a video `exists` too.
  - **A clip is shots** (`VideoRequest.shots: Vec<ShotSpec {text, seed, frames, cut,
    references}>`, at least one, plus `every`): one sd-cli run per shot in one job,
    each with its own seed and `align_frames`'d length (`render_first`/`render_shot`
    clone the model with `frames` set), each conditioned on
    **`videos::shot_prompt(every, its own text)`** — never `"{prompt}. {shot}"`, which
    made a line spoken in the Prompt be spoken in every shot. A shot's words roll off
    its own seed, `every` once off shot 1's. **`plan_shots`** (a free fn, tested)
    checks and rolls every row before a frame is drawn or cut. Recorded as
    `Clip.segments` (`{text, conditioned, seed, start, frames, cut, references}`, never
    empty; `Clip::normalize` reads the old `shots`/`shot_starts` with the old prompt as
    `every`, never writes them, and fills a missing `frames` with the model's — what
    every old shot ran at). `Clip.prompt`/`seed` are shot 1's. Shot 1 renders into the
    clip; every other into `seg{k}/` from the clip's last frame, then `append_segment`
    drops its first frame (the seam, redrawn from `-i`) and its sound's — unless it is
    a **cut** (`seam: false`). Only `VideoFamily::chains()` continue (5B, A14B, H3 —
    `-i` is a first frame there; VACE reads it as a reference, T2V ignores it).
    Failure or Stop at shot k>1 keeps 1..k-1 with `VideoResult.note` (`cut_short`). A
    multi-shot clip drops sd-cli's `clip.avi` (it is shot 1 alone).
  - **References are H3's alone** (`VideoFamily::takes_references`,
    `references` on `/video-models`): sd.cpp refuses H3's Ref2VA beside a keyframe, so
    a shot with references has no `-i` (`VideoIo.refs` → one `-r` each, staged into
    scratch by `stage_shot_refs`, removed after). Shot 1 takes them only with no
    first frame. The page's ⓘ for it is the shared `vshotcut`.
  - **A cut from a picture** (`ShotSpec.keyframe`/`Segment.keyframe`, an output
    stem; the row's `data-key`) is any `chains()` family's: `render_shot`'s `-i` is
    the keyframe instead of the last frame. Never on shot 1 (that is `source`), a
    continue, or beside references. **A cut on Wan needs one** — its only way to
    change scene; an H3 cut may have neither (fresh from words).
  - **Extend, Redo and the × are one request** (`Engine::extend_clip`,
    `ExtendClipRequest`, `POST /videos/extend {id, keep, shots, every?, negative?}`,
    `Work::ExtendClip` on the queue): keep the first `keep` segments (≥ 1; shot 1 is
    Generate's), `truncate` the rest, draw `shots` after them on `clip.video` as
    recorded. A failure before any shot answers the error only when nothing was cut;
    after a cut it answers the shorter clip with a note. **Done is computed, never
    stored** (`shots.js` `shotState`/`rowMatches`, `shots.test.js`): row k is done when
    `segments[k]` has its text, seed, frames, cut and references; `action` is
    `extend`/`redo`/`generate`/`none`. Blank rows are not shots (`liveRows` keeps the
    row index for the marks). After a press the rows are redrawn from the clip
    (`landRows`: the seeds drawn, plus any sent rows it didn't reach). Any change
    `invalidate`s what was made from the frames (container, cached download, enlarged
    versions) and re-`finish`es. *Save to output* always rewrites.
  - **Download is the browser's** (`web/js/export.js`, WebCodecs): frames fetched one at
    a time → `VideoEncoder` (Baseline H.264 first, so no reordering; `ctts` v1 if an
    encoder reorders anyway) → `mp4mux.js`/`webmmux.js`, sound via
    `OfflineAudioContext` at 48 kHz → Opus; GIF is `gifenc.js` (one median-cut palette
    for the clip, so no shimmer). `available()` offers only what `isConfigSupported`
    says; the AVI is a plain link. The muxers and GIF are pure, read back by
    `mux.test.js`/`gifenc.test.js`. No codec in the binary on purpose.
  - **A saved clip is an output**: `list_outputs` lists `{stem}.avi` with
    `video: true`; `find_output` stays PNG-only (it resolves image sources), and
    `find_clip_output` finds the clip; `free_stem` checks both; delete, star
    (`copy_picture`'s `media`) and the tile (`write_output_thumb` decodes the first
    JPEG) handle either. Frames come out of the AVI (`/outputs/{stem}/frames/{k}.jpg`,
    one parsed file cached, `OUTPUT_CLIP`). The source picker filters `video`, and
    Refine & adjust's ←/→ steps over them.
- **Convert tab** (`src/convert.rs`, `-M convert`): a file's on-disk type isn't its run
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
- No Python anywhere in build or runtime.
