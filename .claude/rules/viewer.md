---
paths:
  - "web/js/viewer.js"
  - "web/js/redraw.js"
  - "web/js/refs.js"
  - "web/js/mirror.js"
  - "web/js/texttool.js"
  - "web/js/regiontool.js"
  - "web/js/brushpaint.js"
  - "web/js/colorwheel.js"
  - "web/js/colorrange.js"
  - "web/js/hints.js"
  - "src/redraw.rs"
  - "src/area.rs"
  - "src/brush.rs"
  - "src/colorrange.rs"
  - "src/edits.rs"
---

# The Refine & adjust dialog

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
