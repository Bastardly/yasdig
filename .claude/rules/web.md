---
paths:
  - "web/**/*"
  - "src/web.rs"
---

# The page: stylesheet, modules, the prompt box

## The stylesheet

`web/style.css`, everything visual; markup carries classes only. **A rule names a
role, never a value**: colour, type, space, radius and column caps are `:root` tokens
in **`web/tokens.css`** (linked first) — a hex elsewhere means a missing token.
**`tokens.css` is also yasdig.com's**: `site/build.mjs` copies it into the site, so a
renamed or removed token breaks `site/style.css` too (grep it). A token only the site
uses goes in `site/style.css`'s own `:root`. The claude.ai *Design System* artifact
is a copy for browsing — republish it when tokens change. Ink ramp and state colour are neutral (bone)
because they sit beside a graded image. `--fold-title`/`--fold-on` for fold headings.
**Amber (`.btn-run`) is the dock's main button only** (plus Hi-res above a result,
the thumbnails' favourite star as a glyph colour, the Shots strip's round + (asked for), and the Assistant's
`.chatworking` line at the foot of a chat while its job runs — asked for: the header's
grey state line was missed) — in Refine & adjust that is
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
| `jobs.js` | the `/jobs` poll: Jobs button/dialog, progress lines, `onResult`, `onNotice`, `observable` |
| `downloads.js` | the `/downloads` poll: Downloads button/dialog, `downloadsChanged`, `onDownloads` |
| `notes.js` | a model file's note: `#notedlg`, the blue i (`infoButton`, `setInfo`) |
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
| `cast.js` / `casteditor.js` | Cast tab, `castTile`; the cast editor (`openCastEditor`, `KIND_LABEL`) |
| `castpicks.js` / `castnames.js` | the Image tab's Cast row (`castPayload`, `setCastPicks`); which names a prompt uses, pure |
| `video.js` / `player.js` | Video tab; canvas frame player, `renderClip` |
| `shots.js` | which Shots rows the clip on the stage has (`shotState`) |
| `clipview.js` / `export.js` | the clip dialog; Download's WebCodecs encode |
| `mp4mux.js` / `webmmux.js` / `gifenc.js` | the download containers and GIF, pure |
| `convert.js` | the Models tab's Convert panel (`loadConvertModels`, reloaded by `loadInstalled`) |
| `getmodels.js` | Models tab, `openModels` |
| `prompt-box.js` / `prompt-weight.js` | the custom element; pure weighting maths |
| `help.js` | Help tab `TOPICS` — also yasdig.com's `/guide/` pages (`site/build.mjs`; a new `fill` needs a `FILLED` row there) |
| `settings.js` / `modelsettings.js` | Settings form from `FIELDS`; *Per model* card |
| `assistant.js` | the Assistant panel (`mountAssistant`, per tab): chats, the composer, following a turn, the Image tab's controls both ways |
| `board.js` | a Video chat's film as the Shots rows (both ways) and the Cast panel |
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
