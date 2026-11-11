---
paths:
  - "web/js/image.js"
  - "web/js/panels.js"
  - "web/js/source-slot.js"
  - "web/js/layout.js"
  - "src/layout.rs"
---

# The Image tab

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
- **Shots is a sideways strip** (`.shotstrip`, a card per shot, `.fresh` on shot 1 and
  each cut; the round amber + is `position: sticky` at the strip's right edge; Every
  shot and the negative fold as `#vfold-every`, capped at `--shot-common-w`) and resizes
  both ways — `fpboth` in markup is overwritten by `PANEL_DEFS`' `both`.
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

Settings' columns end in `<div class="dock">` (`position: sticky;
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

## Image to image

The *Image to image* fold (code says "Start from"): a picked source makes Generate run
`/img2img`. `web/js/source-slot.js` is the factory (the References dialog's slots
are instances too). `Engine::img2img` is `run` with a source instead of txt2img.

- **The source is a `work/output/` stem**, resolved by `Engine::output_source` (shared
  with video), guarded by `workitems::valid_id`.
- **Copied into scratch** (`{id}-0-src.png`), never adopted — invariant 8.
- **`fit_to` only shrinks, and snaps only the width to 64** (snapping both distorts
  aspect up to 6.7%). Inside the budget it's untouched so the caller can `fs::copy`.
- **Uploads** (`POST /upload`) land in `work/output/` via `Store::import`, stem minted
  by `new_id`. Body is raw PNG bytes; `http.rs` `body_limit` is an exact path match;
  over-limit answers 413. **PNG only in Rust** — the page re-encodes via `<canvas>`,
  except a PNG under the side and byte caps, sent as it is so its settings chunks
  survive; `handle_upload` strips ours (`pngmeta::strip`) when
  `embed_png_settings` is off.

Page:

- **`data-when` swaps the panel; `syncMode` is the only writer.** Base size/Output
  upscale are `text`; Strength/Working size are `source`; Steps and CFG serve both.
  The slot's `onChange` calls `syncMode` and opens the fold.
- **`genPayload` returns `[route, body]`**, so batch/loop/after-each work over a source.
- **A reopened work item always sets the slot** — to its source or to nothing.
- **The fold summary says `from …` whenever a source is picked.**

Paste is a document listener per slot, gated to its visible tab.

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
