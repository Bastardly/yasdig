---
paths:
  - "src/settings.rs"
  - "src/variables.rs"
  - "web/js/settings.js"
  - "web/js/modelsettings.js"
---

# Settings, prompt variables, adding a knob

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
- **Video models are in the same card** (`GET /model-settings`' `video`: every
  `video_models_untuned` model with its `base` over `VIDEO_KNOBS`, `memory` as
  `memory_mode()`, `moe`/`ltx`/`narrated`, `languages`, parts): the Model select's
  values are `i:`/`v:` + name (a name may be both), video groups after image ones.
  A video model swaps `#pmrows` for `#pmvrows` (`VROWS` in modelsettings.js, rows
  hidden by `applies`) and saves `{kind: "video"}` — which answers `{tuned}`, so the
  card re-reads the listing.
- **`upscaler: None` means both FSR and unset** — documented, not fixed.
  **`scheduler` uses `"auto"` as a value** (`config::AUTO_SCHEDULER`), so DiT recipes
  can pin Auto over a global `karras`.

Page: `settings.js` builds the form from `FIELDS`; a row names the control it defaults,
and takes label (`labelOf()`) and tooltip (`data-help`) from it. Only
`hires_flash_attn`, `weight_type`, `extra_args` carry their own label/`HELP`. **Delegated
listeners are rooted at `#tab-settings`** (the form is panels, not one container).
**`sharpness`/`clarity` are in the file, not the
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
