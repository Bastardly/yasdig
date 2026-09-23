# CLAUDE.md

Python-free Vulkan image generator for AMD/Linux. Generate small and cheap with
stable-diffusion.cpp (shelled out as a subprocess, never FFI), then escalate quality
on demand with img2img passes and a native FSR post-process in `wgpu`. All GPU work —
sd.cpp *and* the FSR compute passes — is serialized through one worker thread, so
nothing thrashes VRAM.

**`README.md` is thorough and user-facing: setup, the `models.toml` field table, the
API shapes, the design rationale. Read it for any of those.** This file is only the
part it doesn't cover.

## Commands

```sh
cargo test                                  # no GPU: naga-validates the 4 WGSL shaders, the band-measurement math,
                                            #   the escalation/upload DTOs and `fit_to`
cargo run --release                         # server on 127.0.0.1:8080
cargo run --release -- doctor               # preflight only: binary, checkpoints, web UI
cargo run --release -- fsr-selftest         # full wgpu path; no sd.cpp or checkpoint needed
cargo run --release -- fsr in.png out.png [scale] [sharpness] [clarity]

node --test 'web/js/*.test.js'              # the one JS unit test: the prompt-weighting string maths.
                                            #   Node's built-in runner — no npm, no package.json, no
                                            #   build step, and nothing at runtime needs Node.
```

The shaders otherwise only compile at GPU device creation, so `cargo test` is what
turns a WGSL typo into a test failure instead of a startup failure on the worker
thread. `fsr-selftest` and `fsr` exercise the GPU path with no model weights — the
fast loop when touching `src/fsr/`.

**`fsr-selftest` is what to run after changing a filter constant.** It asserts the
dimensions and that scale 1.0 with every filter at 0 is byte-exact (claimed by three
shader headers and by `README.md`, and unchecked until it wasn't), checks the grade
pass knob by knob (`check_grade`), then plants four
gratings at wavelengths proportional to the image and prints how much each filter moved
each band, at 1536² and 3072². Those two sizes are invariant 6 below with a number on
it — same setting, RCAS radius 1 vs 2 — and the band rows are how you catch a "clarity"
edit that has quietly become a high-pass. They are printed rather than asserted because
`rcas_radius` is a step function and nobody has yet seen the spread to bound it; turning
them into an assertion once there are numbers is the obvious next move. The CPU half of
the measurement — that the DFT reports a planted amplitude correctly and doesn't leak
between bins — is in `cargo test`, since a plausible-looking table that says nothing
true is the failure worth guarding against.

## The four GPU passes

`Fsr::process()` in `src/fsr/mod.rs` is the single entry point for all four, and takes
them as one `Filters` (`src/config.rs`) rather than as a widening list of floats. Each
pass is skipped, and its textures never allocated, when it would be a no-op. The header
comment in each `.wgsl` is thorough; open the file rather than guessing.

- `src/fsr/upscale.wgsl` — **FSR1 EASU**, a port of `FsrEasuF`. Skipped at scale 1.0.
- `src/fsr/sharpen.wgsl` — **FSR1 RCAS**, a port of `FsrRcasF`. Exact identity at
  sharpness 0.0. Its `FSR_RCAS_DENOISE` term is deliberately off; the header explains
  why and quotes the measurement. Don't re-enable it without measuring again.
  **Sharpness 1.0 is the ceiling, not a soft cap** — it's the reference filter at
  0 stops, and a lobe past the limiter inverts the filter rather than sharpening
  harder (`4*lobe + 1` goes negative). More apparent sharpening has to come from the
  tap distance (`rcas_radius`), not the lobe.
- `src/fsr/contrast.wgsl` — **not FSR.** A difference-of-Gaussians band-pass on luma
  ("clarity"), computed on a 1/8 plane. Exact identity at clarity 0.0. The knob is
  **signed**, -1..1: negative subtracts the band and softens.
- `src/fsr/grade.wgsl` — **not FSR either.** Eight colour-grading knobs — lightness,
  contrast, highlights, shadows, saturation, hue, temperature, tint — in **one** pass,
  because every one of them reads a single texel and writes a single texel. They are
  one dispatch together whatever they are set to (too cheap to measure at 3072²), so
  the ninth knob costs nothing to add and belongs here rather than in a pass of its own.

  It works in **OKLab**, which is what keeps it one shader: in that space each knob is
  a named operation on three numbers, and the two white-balance knobs are *exactly* its
  two chroma axes. The header says what temperature and tint are and are not (chroma
  shifts, not a von Kries adaptation) and where to go if that reads wrong.

  **Order is EASU → grade → clarity → RCAS.** A photo editor's order, and load-bearing:
  clarity's band and RCAS's limiter both act on contrast the grade has just changed, and
  grading after a sharpen amplifies its halos and then clips them.

  Neutral is skipped rather than relied on: the sRGB/OKLab round trip loses a
  least-significant bit here and there, and `fsr-selftest` asserts byte equality.
  `check_grade` in `src/main.rs` is what makes the eight knobs safe to extend — it runs
  each at 1.0 and fails if it moved nothing, which is the copy-paste that `GradeParams`
  and the shader's `Params` invite.

## The image-to-image tab

`Engine::img2img` (`src/pipeline.rs`) is `run` with the txt2img replaced by a source
image, and `open`'s copy-in rule bolted to the front. Three things about it.

**The source is a `work/output/` stem, never a path**, resolved by
`Engine::output_source` — the same helper `Engine::video` uses for a first frame, and
the same contract: the client picks from what `GET /outputs` published, and
`workitems::valid_id` is the whitelist that makes that safe. One helper rather than
two copies of the check.

**It is copied into scratch, never adopted.** That file belongs to the user, and a
`Pending` pointing at it would have `remove_files()` delete it on the first refine —
invariant 8's failure in a new place. The copy is `{id}-0-src.png` under `Scratch`, so
a failed pass takes it with it.

**`fit_to` only ever shrinks, and only the width is snapped to 64.** Snapping both
axes was the obvious thing and is wrong: 5000×2000 into a 1536 budget gives 1536×576
rather than 1536×614, a 6.7% change of aspect that nothing downstream reports. One
snapped axis is also what `hires` does, where a single scale factor drives both. A
source already inside the budget comes back untouched, which is what lets the caller
`fs::copy` the bytes instead of resampling them — what feeds a diffusion pass should
not already be a generation of loss. The size ceiling is not politeness: a phone photo
is 4032×3024 and a pass at that size is an OOM on a 24 GB card.

Uploads (`POST /upload`) land in `work/output/` through `Store::import`, which is why
the tab needed no new listing, thumbnail or picker route — that directory is already
*the* pool of images you can start from, and an import is animatable as well as
diffusable. The stem is minted (`new_id`), never taken from the filename: `valid_id`
is alphanumerics and `-`, so almost no real filename is even expressible, and
generating the name is what keeps the guard a whitelist rather than a sanitizer.

The body is **raw PNG bytes**, not base64 in JSON — `Request.body` is a `Vec<u8>` that
is never UTF-8 validated, so this costs nothing and saves a third of the wire.
`http.rs` picks the limit per path (`body_limit`, an exact match — a prefix would hand
32 MiB to anything starting with `/upload`), and over-limit now *answers* 413 instead
of bailing, which used to close the socket with no response at all. **PNG only in
Rust, on purpose**: the page re-encodes through a `<canvas>` first, so JPEG/WebP/AVIF
all work and `image` stays compiled `features = ["png"]`.

## LoRAs

`models/loras/` (`Config::lora_dir`, an `SdConfig` option defaulting inside the
models dir — the same shape as `video_dir`, and for the same reason: the
checkpoint scan there is non-recursive and file-only, so a subdirectory is
already invisible to it). `GET /loras` lists it through `discover::scan`, and
`base_cmd` is where a picked one becomes a command line.

**Everything here is shaped by three facts about `extract_and_remove_lora`**
(`stable-diffusion.cpp examples/common/common.cpp:2243`), which is sd.cpp's own
tag parser:

- **An empty `--lora-model-dir` skips tag extraction entirely** (line 2245). The
  `<lora:…>` text then stays in the prompt and is conditioned as words. So
  `base_cmd` emits the flag **unconditionally**, not only when something is
  attached — which also means a tag typed into the prompt box by hand resolves
  against the folder the picker offers rather than against the process CWD,
  which is sd.cpp's `"."` default.
- **A LoRA it can't find is a `LOG_WARN` and a dropped tag** (line 2289). The
  render succeeds, takes its usual two minutes, and comes back as a picture that
  simply doesn't have the adapter in it. `Config::check_loras` is the answer, and
  it is called from all four places a profile reaches `SdRunner` — `run`,
  `img2img`, `refine` and `hires` — because a file deleted between the generate
  and the Refine is the same failure arriving later.
- **The tag regex is `<lora:([^:>]+):([^>]+)>`** (line 2248), so `:` `<` `>` in a
  name don't get escaped, they silently re-cut the tag somewhere else.
  `valid_lora_name` rejects them, and is a **second whitelist beside
  `workitems::valid_id` rather than a widening of it**: an id is minted by us and
  so can be digits and `-`, while this is a filename the user chose and has to
  allow `.`, spaces and `/`. Widening `valid_id` would quietly widen the archive
  routes with it. The path half of the check is structural — every
  `Path::components()` must be `Component::Normal`, which rejects `..`, a leading
  `/` and a bare `.` in one test.

**`Profile.loras` is the only knob on `Profile` with no `Defaults`/`Template`
pair**, and that is deliberate: a LoRA is reached for per picture, so it arrives
on the request and both arms of `registry()` build the field empty. That is what
keeps this change clear of the trap at the top of this file — there is no second
struct to forget. `a_model_brings_no_loras_of_its_own` pins it, and is the test
that fails if somebody adds the config key later without reading this.

It is on `Profile` at all for two reasons, and both are load-bearing:

- **`base_cmd` is shared by `txt2img` and `img2img`**, so refine and hi-res
  inherit the adapters with no code. That is the correct default rather than a
  convenience: a finishing pass that dropped the LoRA would redraw the surface in
  the base model's idiom and undo the look.
- **The profile is what a `Pending` retains and what a sidecar stores**, so
  reopening a work item restores the picker and an image's record names what
  actually made it.

The consequence to keep in mind is that `swap_model` replaces the profile
**wholesale**, so naming a different checkpoint for a pass clears the LoRAs.
That is right — a LoRA is trained against one base — and it is why `loras` is
deliberately *not* on `EscalateRequest`: the boundary test in "Adding a per-model
knob" would let it in, but a second picker on the page would then need its own
rule for how a `Vec` interacts with a wholesale swap.

On the page, `loras.js` is a factory in `gridmark.js`'s shape and for the same
reason — two copies of a control that builds a request body is two chances for
one of them to send a different one. Two things in it break silently:

- **The module-level `available` list is shared; the picked rows are not.** One
  fetch feeds both tabs (`loading` memoizes the in-flight promise so two `init()`
  calls in one tick don't race two requests), but a LoRA chosen for a txt2img has
  no business riding along on a pass over somebody's photograph, so each instance
  owns its own `picked`.
- **`loras` is always sent, including empty.** The server reads `[]` as "none,
  explicitly" and a missing key as "keep whatever the profile has"; the picker on
  screen is the complete answer to the question, so it must be able to say none.
  Dropping the key when the list is empty would make a removed row unremovable.

## Favourites

`user/favorite/` holds the same three files as `work/workitems/` — PNG,
thumbnail, sidecar — under the same ids, read by the same code through
`workitems::Shelf`. The Image tab's grid has a **Current / Favorites** toggle
that is nothing but which shelf its routes name.

**The point is `Delete all`.** A grid of every generation ever made grows
without bound, so the way it actually gets used is to empty it, and that took
the good ones with the rest. A favourite is therefore a **copy, never a move**
(`Store::favorite`): the original stays in the grid, `Delete all` stays the
blunt instrument it is, and what survives it is what was chosen. A move would
make favouriting a way of *hiding* an image from the grid it was just found in,
and would leave the same "empty it and lose things" problem one directory along.

That is also why it is under `user/` — the directory nothing here sweeps, beside
`settings.toml` — rather than a third directory under `work/`. `purge_pngs` is
non-recursive over `work/` itself, so it could not reach a `work/favorite/`
either; the argument is about what the rule *says*, not about what the current
code happens to touch.

Five things break silently:

- **`Shelf` is the only place a shelf becomes a directory.** Every path goes
  through `Store::shelf_dir`, and `image_path`, `thumb_path`, `sidecar_path`,
  `load`, `list` and `delete` all take one — so a caller cannot forget which
  archive it meant. `save` deliberately does not: a generation is archived, a
  favourite is chosen.
- **`delete` is one shelf, and that is the whole guarantee.** A delete that
  reached both would destroy the favourites a moment after they were made,
  which is the failure this feature exists to prevent and would look exactly
  like it working. `a_favourite_survives_deleting_the_original` is that under
  test, in both directions.
- **The shelf is a path segment, not a query parameter.** `http.rs` strips the
  query string before routing — the same constraint that already puts an id in
  the path — so the routes are `/favorites`, `/favorites/{id}/thumb.png`,
  `/favorites/open`, `/favorites/delete` and `/favorites/add`, each one line
  delegating to the work-item handler with a different `Shelf`. `JobKind::Open`
  carries one for the same reason: the two grids hold the same ids, so an id
  alone doesn't say which file to read.
- **There is no `unfavorite` route.** `POST /favorites/delete` already is one,
  and a second name for removing a file is a second place to get `valid_id`
  wrong. The page reaches it through the ordinary delete mode, which is why
  *Click to delete* stays offered on the favourites shelf while *Delete all*
  does not.
- **Reopening a favourite is the same copy-in, copy-out.** `Engine::open` takes
  a `Shelf` and changes nothing else, so invariant 8 holds unchanged — and more
  sharply here, since a `Pending` pointing into `user/favorite/` would have
  `remove_files` delete a file the user explicitly asked to keep.

On the page, the shelf is one module-level string in `workitems.js` that is the
first segment of every route the grid uses, so the toggle has almost nothing to
do: `setShelf` lights the right `.btn-choice`, hides the *Click to favorite*
radio (favouriting a favourite means nothing) and reloads. `loadWorkItems`
captures the shelf across its `await` and drops a response that lost a race
against a flip. A generate reloads the grid and deliberately does **not** switch
back to Current — the toggle stays where it was put, which is the only rule that
stays predictable.

The two actions are a table in `gridmark.js` (`value`, `button`, `mark`,
`label`, `route`, `key`, and an optional `all`), the way `FILTERS` and `FIELDS`
are tables, because the second one arrived a feature after the first. One marked
`Set` is shared between them rather than one each: a single radio group means
exactly one action is live at a time, and changing mode already clears the marks.
`route` is a thunk because the work-item grid's route moves with the shelf, and
`all.when()` is what takes **Delete all** away on the favourites shelf.

## The Outputs tab

`work/output/` as a gallery rather than as a picker. `web/js/outputs.js` and
`Engine::open_output` (`src/pipeline.rs`), and the whole tab is the grid: clicking a
tile opens the **viewer** on it full size with the ten sliders live, and ← / → step
through the gallery with the dialog left up.

**There is no result column, no Refine and no Hi-res, and that is the design.** A
saved output is `display`, not `raw` — its grade and its RCAS are already in the
pixels — so diffusing over one is precisely what invariant 1 exists to prevent.
Grading it *further* is fine and is what the tab is for; escalating it is not, and
the way to escalate a saved image is the Image-to-image tab, which starts a real
pass from it.

That also makes this the one place the sliders are **not** the lossless re-filter
they are everywhere else. `/adjust` elsewhere re-runs the passes over a retained
unfiltered `raw`; here `raw` *is* the filtered file, so a drag stacks on what is
already baked in. Three things follow, and all three are in `open_output`:

- **The filters start at `Filters::NONE`**, not at the model's values. 0 means "as
  saved". Starting at `profile.sharpness` would sharpen an already sharpened picture
  the instant it appeared.
- **The retained profile's `sharpness`/`clarity` are zeroed too**, because
  `Engine::result` derives `start` — where **Reset** goes — from them. They are the
  one pair on a restored profile that must not be kept, and they look exactly like a
  pair worth keeping.
- **A missing sidecar is an answer, not an error.** `list_outputs` deliberately lists
  a PNG dropped into the directory by hand, so this has to be able to open one:
  `Profile::default()`, no prompt, and the sliders work the same.

The image is copied into scratch like every other reopen — invariant 8, in its third
place. `work_id` and `step` come back from the sidecar so a save lands as
`{id}-s{step}-2.png`, grouped with the generation it came from; `free_stem` is what
stops that name being the file being edited. **Save is never an overwrite**: the
pre-edit pixels exist nowhere else, since an output can descend from a refine and
those are not archived.

Four things in it break silently:

- **Deleting is `gridmark.js`, shared with the work-item grid.** Those are the
  two places on the page that remove a file, and a fix to one that missed the other
  would be a fix that only half worked — on the half nobody was looking at. The
  factory owns the mode radios, the marked set, the buttons and the requests; each
  tab still draws its own tiles and calls `mark()` on them, because the two carry
  different data attributes and mean different things by `active`. It builds a
  closure and a `Set` and nothing else, so calling it at module top level keeps the
  declare-only rule. This tab declares **one** action and the work-item grid two —
  see *## Favourites*.

  **`Delete all` is the same request with every id in it**, not a route of its own —
  which is what keeps `valid_id` the only guard there is. A route that emptied a
  directory wholesale would need a different kind of check entirely. It is also the
  one `confirm()` on the page, and it only appears in delete mode: marking is visible
  and reversible, "all" is neither.
- **`outputGrid` is the one tile.** It was written out three times — this grid, the
  img2img source strip, the video tab's first-frame picker — and the declared height
  has to match what `.wigrid` draws or the strip reflows as the thumbnails decode.
  One function with a `height`, an `empty` and an `onPick`.
- **The arrow keys yield to a focused range input.** An `<input type=range>` consumes
  Left and Right natively and should: the key handler checks
  `e.target.tagName` and returns for one. So a focused slider keeps its arrows, and
  clicking the picture is how you hand them back. The listener is on `document`
  because a modal `<dialog>`'s keydown bubbles out of it.
- **An open is a job on the serialized worker, so a held arrow key coalesces** the
  way a slider drag does — one in flight, the latest target remembered, fired once.
  Without that a two-second key repeat queues a slideshow that plays back after you
  let go. `cancelPost` (`viewer.js`) is the other half: it bumps an epoch that
  `runPost` compares across its `await`, so the re-filter of the image you just left
  can't land on top of the one that replaced it.

## The stylesheet

`web/style.css`, served through `web::WEB_FILES` like everything else the page
loads. Everything about how the app looks is in that one file; the markup carries
classes and nothing else. (The `<link>` is relative, but the page no longer opens
from `file://` at all — module scripts are blocked there by CORS. Run the server.)

**A rule names a role, never a value.** Colour, type, space, radius and the column
caps are all tokens on `:root`, so a hex anywhere else means the token you wanted is
missing — add it rather than the hex. The measurements keep the names the `.layout`
comment argues about (`--col-result-max`, `--panel-w`, `--thumb-h`), and they are at
the values that comment was written for.

**The ink ramp is neutral on purpose.** The controls sit beside a 1024 px render
that the user is grading with eight colour knobs, and a blue-black page — which is
what `#0e1116` was — casts on it. The same argument decides the *state* colour:
active tab, active thumbnail, every `accent-color`, the progress fill and the focus
ring are bone rather than a hue, because every one of those marks lands on or beside
an image. A photo tool's selection border is light grey for this reason.

**The action colour marks the one thing a controls panel is for: amber is the
button at the head of the column, and nothing else.** Neutral is every other
click; red stops or destroys something. On four of the five panels that button
also spends GPU time, and the colour doubles as a warning about it — `Enlarge
frames` and `Close` used to be the same blue, and a click here can cost minutes.
Settings' Save costs nothing and is amber anyway, because one panel wearing a
different colour for its primary action reads as a different *kind* of control
rather than as a cheaper one. That is `.btn-run`, `.btn` and `.btn-danger`, plus
`.btn-choice` for a set where the highlighted one is where you are.

Three things in it break silently:

- **A bare `<button>` is unstyled, and a class carries the look.** The element used
  to be painted as a full-width green action button, which meant nine rules whose
  only job was to undo it — every tab, thumbnail, zoom control and icon on the page.
  The cost of the reset is that a button which *forgets* its class is invisible
  chrome, so the rule is: every `<button>` carries `.btn` or `.hint`, except the
  thumbnail grids (where the 2 px border *is* the chrome) and `#lbclose`.
- **`:disabled` is declared after every `:hover`.** Same specificity, so source order
  is the only thing deciding what a greyed-out button under the pointer looks like.
- **`--col-*` cannot be used in the media query.** Custom properties aren't allowed
  there, so the 899 px breakpoint is a literal that has to be kept in step by hand.

`.tab` carries no style at all — it is how `showTab` finds the five tab buttons, and
a rule with no declarations would be worse than none. Don't delete it for looking
unused. And `doctor` checks for both files under `web/`, because an unstyled page
reads as a broken one rather than as a missing one.

## The script

`web/js/`, one ES module per area, no build step and no framework — the browser
resolves the imports itself. `index.html` loads exactly one of them:
`<script type="module" src="js/app.js">`.

| file | holds |
|------|-------|
| `dom.js` | `$`, `esc`, `post`, `numOr`, `bar`. No state, no imports |
| `state.js` | `S`, `PANES`, `P`, `clearPane`, `escErr` |
| `busy.js` | `IDLE_LABELS`, `setBusy`, `onIdle`, the progress poll, `cancelRun` |
| `hints.js` | `HELP`, `HINTS`, `wireHints`, the tooltip bubble |
| `viewer.js` | `FILTERS`, the sliders, `/adjust`, fit/1:1, panning |
| `image.js` | the Image tab: models, Advanced, `render`, escalations, runs, Save |
| `workitems.js` | the work-item grid and reopening one |
| `gridmark.js` | click-to-mark: the marks, the action table, the buttons, the requests |
| `img2img.js` | the Image-to-image tab, including the upload |
| `loras.js` | the LoRA picker, one instance per image tab |
| `outputs.js` | `fetchOutputs`, `outputGrid`, and the Outputs tab |
| `video.js` | the Video tab: models, generate, the clip grid, the first-frame picker |
| `player.js` | the canvas frame player and `renderClip` |
| `convert.js` | the Convert tab |
| `prompt-box.js` | the one custom element, and the weighting shortcut on it |
| `prompt-weight.js` | `adjust`: the string maths behind Ctrl+↑/↓. Pure, and the one file with a test |
| `settings.js` | the Settings tab: the field table, and the form generated from it |
| `app.js` | `showTab`, and the boot order |

Four things in it break silently:

- **A module's top level declares and nothing else.** Every `addEventListener`,
  every `innerHTML =` and every first fetch is inside that module's exported
  `init*()`, and `app.js` calls them in order. That is what makes the cycles in
  the graph harmless — `video.js` and `player.js` import each other, and so do
  `image.js`, `workitems.js` and `img2img.js`, because the tabs genuinely do call
  into each other. A cycle only bites when a module *reads* a cyclic import while
  the other is still evaluating, and nothing here does. Put a `$(...)` at file
  scope and that stops being true.
- **The six shared values live on one object, `S`.** Not six exported `let`s: an
  imported binding is read-only at the importer, so `imageId = null` in another
  file is a `TypeError` rather than an assignment. The single-state rule under
  *The three image panes* is unchanged; this is only how it is spelled now. Anything
  only one module writes stays a plain `let` in that module and is exported if
  others read it (`models`, `clip`).
- **`setBusy` reaches every tab, and gets there through `onIdle`.** It disables
  `#go`, `#i2go`, `#vgo`, `#cgo`, `#lbsave`, all ten sliders and three upscale
  selects at once, because there is one GPU queue. What a button says at rest is the owning
  tab's to decide, so `image.js` and `img2img.js` register a callback rather than
  `busy.js` importing them back — which is also what keeps that module importable
  from everywhere.
- **`web::WEB_FILES` is both the route table and the `doctor` list**, and the
  content type in it is load-bearing: a browser refuses to execute a
  `type="module"` script served as anything but a JavaScript MIME type, and the
  failure is a console error behind a blank page. Adding a module means adding a
  row. It is still a whitelist — the name is matched whole, the same rule as
  `workitems::valid_id` — which is why there is no generic static handler.

`<prompt-box>` (`web/js/prompt-box.js`) is the only custom element, and it
**wraps** its `<textarea>`/`<input>` rather than replacing it: the control inside
keeps the id, so `$('prompt').value`, `<label for>` and `wireHints` need nothing
forwarded. It is where the features around a prompt go, and there are six of them
on the page, so writing one three times is what it exists to prevent. Light DOM on
purpose. See *## The prompt box* below for the one feature on it.

**The tabs are deliberately *not* components.** Shadow DOM is the only thing a
custom element offers that a module doesn't, and `setBusy` above is why this page
can't have it — five shadow roots would turn one function into a broadcast plus a
registry, and `style.css` would need re-plumbing into each root. Without a shadow
root a custom element is an `init()` with ceremony. `showTab` clearing the pane it
leaves is the same argument from the other end: these tabs are one app with five
faces, not five independent things.

## Settings

`user/settings.toml` is the defaults every generation starts from, and
`src/settings.rs` owns it. The split from `models.toml` is **who writes the
file**, not the format — both are TOML, one parser:

- `models.toml` is hand-written and mostly comments (the `[[video]]` block alone
  is 80 lines of them). The app only ever reads it, which is what lets those
  survive. It holds what the program needs in order to *start*: the binary, the
  directories, the port, `[templates.*]`, `[[video]]`.
- `user/settings.toml` is regenerated in full on every Save, so it carries one
  fixed header and nothing else. A settings *form* that deleted the explanation
  of what it just changed would be worse than no form.

It is `user/` and not `work/` because `work/` is scratch this program deletes
from — `purge_pngs`, `Delete all` — and nothing that took a decision to create
may live under a sweeper. `user/favorite/` is the other half of that rule; see
*## Favourites*.

Five things break silently:

- **`registry()` re-reads the file on every call**, and must keep doing so.
  `Config` is an `Arc` and the GPU worker holds its *own* clone (`main.rs`), so a
  value cached at load could only be changed by restarting the server — which is
  not what a Save button means. A ~1 KB parse against a multi-second diffusion,
  the same rule the LoRA scan already follows.
- **`Settings::merged_with` and `as_profile` have no `..rest`**, on purpose. See
  *Adding a per-model knob* — that is what makes the two-struct pair safe.
- **`POST /settings` is a patch over what is stored, not a whole document**
  (`settings::patch`). The form leaves out any box it can't read a number from,
  so absent has to mean *keep*; deserializing the body on its own would make it
  mean *reset to the built-in*, and an emptied Steps box would quietly put 30
  back. An explicit `null` still clears a field — that is how the Refine/hi-res
  CFG box says "unset" rather than "unchanged".
- **A broken file answers 200, not 500.** `GET /settings` returns the built-ins
  with an `error` spliced in beside them, because the tab has two jobs and only
  one has failed: it still has to fill an editable form, and saving that form is
  the repair. A 500 would leave an empty form and no way out but a text editor.
  `Settings::load_or_default` is the same decision for generation.
- **`Profile::default()` reads the *built-in* settings, never the user's.** It
  fills the gaps in a work-item sidecar written before a field existed, and that
  record has to describe the image it belongs to — reading the live file would
  let a setting changed today rewrite what an old generation claims it ran with.

On the page, `web/js/settings.js` generates the form from one table, the way
`viewer.js` builds ten sliders from `FILTERS`. A row names the *control it is the
default for*, and takes both the label text and the tooltip from it —
`labelOf()` reads the real `<label>`, and `data-help="<that id>"` hands
`wireHints` the key the real control uses. So there is no second vocabulary for
the same knobs and no second copy of any sentence, which is the rule `HELP`
already states seen from the other end. Only the three with no control anywhere
else (`hires_flash_attn`, `weight_type`, `extra_args`) carry their own label and
their own `HELP` entry. There were four: `upscale_factor` has an *Output upscale*
box on the Image tab now, so it is an ordinary row that names it. Note
`img2img_cfg_scale`'s row points at `i2icfg` and sits in the *Image to image*
section, because after the refine/hi-res split that is the only pass it governs
outright — the other two fall through to it.

The *Prompt variables* section is the one part of that form which is not a
default for a control at all, and the one field with no label of its own — its
section heading carries the `data-help`. See *## Prompt variables*.

It is also **the one section rendered into a different container**, `#setvarbox`
in the tab's right-hand column, because the list grows without bound and in the
controls column it pushed Steps, the sizes and the whole Advanced fold off the
screen. That is one word on its `SECTION` row — `pane` — and `markup(pane)` is
called once per container and skips what isn't in the one it is building, so
`FIELDS` is still one table and `fill`/`body`/the dirty check are untouched: they
address a control by an id that is unique to the *document*, not to a pane. The
consequence to keep in mind is that **the delegated listeners are rooted at
`#tab-settings`, not at `#setform`** — the add/remove clicks and the `input` that
marks the form dirty both have to span two panes, and a listener left on the form
would silently stop seeing half of them. The *Add variable* button is above the
rows and prepends, for the reason the `vars` case says.

**`sharpness` and `clarity` are in the file and deliberately not in the form.**
They are where the viewer's two sliders *start*, and that is a live per-image
control everywhere else on the page — the Outputs tab starts them at zero
whatever the settings say, because a saved output has its grade already in the
pixels (invariant 1). So a default for them is a setting that appears not to
take, which is worse than not offering it. Hand-editable, and in the README
table. The eight grading knobs were never per-model at all.

**Unsaved edits are marked on the tab, not just under the button.** This is the
one form on the page you can walk away from mid-edit — every other panel either
acts immediately or is consumed by the button beside it — so `markDirty()`
toggles `.unsaved` on the Settings tab button as well as on `#setmsg`, and arms
a `beforeunload` while there is something to lose. The tab's mark is the words
"Unsaved changes" through a `::after`, not a dot: it has to be readable from
whichever tab you walked away to, and a symbol you must already know the meaning
of isn't. The tab row wraps, so the width is free. It compares
`JSON.stringify(body())` against the last settled snapshot rather than watching
fields, which asks the only question worth asking: would saving now change
anything? `settle()` is the one place that snapshot is taken, and it takes it
from what the *server* returned, so a value it normalised doesn't read as a
pending edit.

Three more on that module, all about order and timing:

- **It is last in `boot()`.** After `initImage()`, because it applies the stored
  hi-res scale through `setUpFactor`, which addresses a select that fills the
  options of — and therefore after `initHints()`, which is why it is the one
  caller that passes a root to `wireHints`.
- **The prompt and negative defaults are only written into a box that still
  holds what the last apply put there.** The fetch is async, and a default
  arriving after someone has started typing must not take the sentence away from
  them. That is also why `index.html` no longer ships those two strings in a
  `value=`: the box starts empty, so "untouched" is a state the module can
  recognise.
- **A stored value a `<select>` doesn't offer is added as an option rather than
  ignored.** A select silently shows its first option when nothing matches, so a
  hand-edited `hires_scale = 2.5` would be displayed as 1× and then *saved* as
  1 by the next press of the button.

## Prompt variables

`{animal}` in a prompt, the `[variables]` table in `user/settings.toml` behind
it, and `src/variables.rs`. A variable is a name and a list of words; every
placeholder is replaced by one of that variable's words on the way to sd.cpp,
so a batch run varies its subject rather than only its seed.

**Braces, not `[brackets]`, and that is the whole design.** `[x]` is sd.cpp's
÷1.1 down-weight (`parse_prompt_attention`, `stable-diffusion.cpp
src/core/util.cpp:816` — the same function *## The prompt box* below is built
around), so with square brackets `[animl]` is a *legitimate* prompt meaning
"the word animl, de-emphasised" and a typo could never be reported. `{` and `}`
fall inside that parser's plain-text run (`[^\\()\[\]:B]{1,1024}`) and mean
nothing to it, which is what lets an unknown name be a 400 naming the variables
that do exist. `a_down_weighted_word_is_untouched` pins the other half.

**Expansion is on the server, unlike the weighting shortcut.** That one is
client-side because it *rewrites the box you are looking at*; this one must not,
because the box has to keep `{animal}` for the next image of a run. So what ran
and what was typed are different strings, and the server is the side that
records what ran: `WorkItem.prompt`, `Clip.prompt` and `Pending.prompt` get the
resolved sentence, and an escalation re-uses that retained prompt rather than
rolling a different animal into a refine. `Engine::run`, `Engine::img2img` and
`Engine::video` each call `variables::expand` once, right after the seed is
drawn, over both the prompt and the negative.

**Both strings are archived, which is what makes a work item a workflow rather
than a picture.** `typed_prompt`/`typed_negative` sit beside them on `WorkItem`,
`Clip` and `Pending`, and `variables::typed` is the one place that decides what
goes in them: the original, and **only where the roll changed it**. So `None`
means "there was nothing to roll" for a prompt with no braces in it *and* for
every sidecar written before this existed — one meaning, no version check, and
the page reads `typed_prompt ?? prompt` (`applyWorkItem`, `applyClip`).

Reopening restores the typed one, and that is lossless rather than a trade:
`applyWorkItem` puts the seed back on the next line, the roll follows the seed,
so `{animal}` plus that seed *is* the picture you clicked — pressing Generate
reproduces it and clearing the seed re-rolls it. The rolled sentence stays
visible under the image, because `render()` already prints `data.prompt`
whenever it differs from the box and now it does.

They are on `Pending` — seven literals, none with a `..rest`, so the compiler
names every site — for one reason: `Engine::save_output` builds its sidecar out
of the `Pending`, not out of the request. Without them a refined and enlarged
image saved to `work/output/` would record what it was conditioned with and lose
what it was asked for.

Five things break silently:

- **The roll comes from the seed**, and must keep doing so. The seed box
  promises that the same number gives the same picture and the reported seed is
  clickable *because* of that promise; a roll off the clock would quietly end
  it. `pick` mixes the seed with the occurrence's ordinal, so two writes of
  `{color}` in one prompt disagree with each other while the whole prompt stays
  reproducible, and a batch — which walks the seed, `genPayload(index)` — still
  varies image to image.
- **A replacement is never rescanned.** Words are appended to the output, so a
  word containing `{color}` lands in the prompt as those characters. One pass,
  no recursion, nothing that can loop.
- **Only a well-formed name is a placeholder.** `{a nice day}`, an unclosed
  brace and a bare `{}` are copied through, so braces stay typeable. What is
  *not* copied through is a well-formed name nothing answers to — that is an
  error, and it is the entire reason the syntax changed.
- **`Settings.variables` is declared last on the struct and has to stay
  there.** TOML has no way back out of a table and serde writes fields in
  declaration order, so a scalar declared after it makes
  `toml::to_string_pretty` fail with `ValueAfterTable` and Save stops working.
  `variables_round_trip_through_the_file` is that constraint under test. It is
  otherwise one of the page-level settings like `prompt` — not on
  `PartialSettings`, not on `Profile`, since no checkpoint has an opinion about
  your word lists.
- **Two rows with the same name is the one error only the page can catch.**
  `take()` builds an object and JSON collapses duplicate keys, so by the time
  the body is written one row is already lost — hence `duplicateName()` in
  `settings.js` *and* the case-collision check in `variables::normalize`, which
  is what guards the file a text editor left.

On the page it is one row in `settings.js`'s `FIELDS` with `kind: 'vars'`, so
dirty tracking, the patch semantics and Save need no special case: `markDirty`
already asks whether `body()` still matches the file. The rows hold their own
state in the DOM — `put` draws them, `take` reads them back, and add/remove is
a delegated listener that appends or detaches a single node — because nothing
re-renders the list while you are typing in it. That is the opposite of
`loras.js`, which keeps a shadow array precisely because it *does* re-render.

## The prompt box

Ctrl+↑ / Ctrl+↓ and Ctrl+wheel weight the word under the caret or the selected
phrase, rewriting the text as `(word:1.2)` — ComfyUI's and Forge's syntax, and the
one `stable-diffusion.cpp` actually parses. It is on `<prompt-box>`
(`web/js/prompt-box.js`), which is why it lands on all six prompt controls across
three tabs with no markup change and no per-tab code, and it is **the whole
feature** — nothing on the Rust side knows about it. A prompt already travels
verbatim from `$('prompt').value` to `cmd.args(["-p", …])` with no shell in
between, so parens and colons were always inert on the way out, and a weighted
prompt round-trips through a sidecar as the text it is.

The string maths is `web/js/prompt-weight.js`, split out and **pure** — no DOM, no
imports, the same contract as `dom.js`. That split is what buys the test:
`web/js/prompt-weight.test.js` runs under `node --test 'web/js/*.test.js'` with no
npm, no `package.json` and no build step, and it is deliberately *not* in
`WEB_FILES`, so the browser can't fetch it. `the_table_lists_every_file_under_web`
skips `*.test.js` for that reason rather than the table listing it.

**Everything here is shaped by `parse_prompt_attention`**
(`stable-diffusion.cpp src/core/util.cpp:816`), which is a port of A1111's:

- **Nesting multiplies, it does not replace** (`util.cpp:806-815`:
  `(((house:1.3)))` is 1.573). So when the target *is* an existing group, `adjust`
  retunes that group's number; wrapping a second one round it would silently push a
  1.2 to 1.32. The rule deciding which is one line — the group's inner text, minus
  any `:w` tail, must equal the target exactly — and it is what correctly makes
  Ctrl+↑ on one word of `(a red crab:1.2)` nest, because pointing at one word of
  three is a request to weight that word.
- **`(x)` is ×1.1 and `[x]` is ×1/1.1** (`util.cpp:821-822`), so a hand-typed
  `[cat]` has to be *recognised*. Nesting it into `([cat]:1.1)` cancels to exactly
  1.0 and reads as the key having done nothing. It is read as 0.9, snapped to the
  step grid — a tenth of one step, in a number being dragged.
- **`\(` is the only escape that works** (`util.cpp:892`; `\)` and `\]` leak the
  backslash into the tokens, upstream's bug). So `escaped()` must be consulted
  before treating a paren as a bracket, and we never emit an escape ourselves.

Four things break silently:

- **`{ passive: false }` on the wheel listener.** Without it `preventDefault()` may
  be ignored and Ctrl+wheel zooms the page instead of weighting the word.
- **The wheel yields when the field isn't focused.** An unfocused field has a stale
  `selectionStart`, usually 0, so rolling over a box you aren't editing would weight
  its first word. Unfocused, the gesture stays browser zoom — the same shape as the
  Outputs tab's arrow keys yielding to a focused range input.
- **The write goes through `document.execCommand('insertText', …)`, not
  `field.value =`.** Assigning `value` wipes the field's native undo stack, so one
  Ctrl+Z after a few nudges throws the whole prompt away. `insertText` records one
  undo entry per adjustment and fires `input` on its own. `setRangeText` is the
  fallback for the day it stops working, and loses only the history.
- **`adjust` returns `null`, and the caller must then not `preventDefault()`.** A
  caret with a delimiter on both sides has no word to weight, and Ctrl+↑ there
  should do whatever the browser does with it rather than being swallowed.

Two smaller ones, both pinned by tests: the weight is snapped with
`Math.round(w * 10) / 10` because `1.1 + 0.1` is `1.2000000000000002`, and `|| 0`
after it kills the negative zero that prints as `-0.0`. It is **unclamped** by
choice — 0 and negatives are reachable, as in ComfyUI. And the returned selection is
the group's *inner* text, never the brackets, which is what makes a held key
idempotent: every step leaves the selection equal to the inner text again, so the
edit branch keeps being the one taken.

## The three image panes

The page has three tabs that put an image on screen, and everything around the
image — the action row, Save, the viewer — is one set of functions pointed at whichever
is showing. That is `PANES` and `P(k)`.

**The third row is shorter than the other two, and deliberately.** `outputs` carries
an `err` and nothing else: that tab has no result block, because its image only ever
exists in the viewer, and neither escalation select, because it offers no escalation. The
two places that read `result` reach it with `?.` for exactly that row —
`setResultImage` in `viewer.js` and `clearPane` here. Giving the tab an empty hidden
`<div id="oresult">` to keep the shape square was the other option and is worse: a
pane key that exists so a lookup won't throw is a key that says nothing true.

The module state (`imageId`, `currentSrc`, `workId`) stays **single**, because only one
tab is visible and the worker is serialized, so there is only ever one live result to
have; two copies would be two ways to be stale. The price is that `showTab` must
`clearPane` the tab it leaves — otherwise a picture sits there looking live beside
Refine and Hi-res buttons that now address a different image, which would work and
would be wrong. It also means the ids `render()` writes (`refine`, `hires`, `saveout`,
`upfactor`, `viewadjust`) exist in at most one pane at a time, which is why
`IDLE_LABELS` and `setUpFactor` need no second set of them.

A pane is down to four keys — `result`, `err`, `refmodel`, `hiresmodel` — and the two it lost say
what the viewer bought. The sliders and their timing readout used to be duplicated per
pane; there is one dialog, so there is now one set, addressed by plain id. `setPost`,
`syncPostLabels`, `runPost` and `setBusy` reach them with `$()` rather than `P()`, and
adding a slider is a change in one place rather than two.

## The tooltips

Every control's label carries an `(i)`, and the text behind it is in **`HELP`** — one
table, keyed by what a control *is* rather than by where it lives, because Steps is
Steps on three tabs and three copies of the sentence are two chances to go stale. The
viewer's sliders are the exception and carry their own `help:` on the `FILTERS` row,
which is the same rule seen from the other side: a slider is declared in one place, and
number eleven should be one line in one list. `HINTS` is the id → text index both end up
in.

`wireHints()` injects the icons rather than the markup containing fifty-odd of them; the
key is the control's id, from `for` or from the input a checkbox label wraps, with
`data-help` for the three headings that label a group instead of a field. It is
idempotent and runs twice — once at load, and once at the end of `renderClip`, whose two
boxes are the only labelled controls built after load.

Three things in it break silently:

- **The icon is inside its `<label>`, so its click handler must `preventDefault()`.** A
  click anywhere in a label activates the labelled control — without that line, asking
  what "Keep going until stopped" means switches it on.
- **The bubble is a `popover`.** Not decoration: the top layer is the only place it can
  be drawn in the two places it has to work. The viewer is a modal `<dialog>`, which
  renders above everything outside it, and `.lbside` is `overflow-y: auto`, which clips
  anything inside it. An ordinary positioned `div` works everywhere except exactly where
  the sliders are.
- **The icon is a `<button>` carrying `.hint` and *not* `.btn`**, which is the one
  place that combination is right: this is a 15 px circle inside a label, not a
  control in a row. See the stylesheet section for why a bare button has no look of
  its own to undo.

What the text is for: the panel asks a beginner for a sampler, a CFG scale and a flow
shift, and the right answer for the first two *belongs to the checkpoint* — 4 steps and
CFG 1 on a turbo model, 20 and 5 on a standard SDXL. Where that is true the sentence
says so. Keep them to a few sentences and keep them about how to *choose*; this is a
tooltip, not the README.

## The viewer

`<dialog id="lightbox">` is where post-processing happens, and that is a correction
rather than a move. Sharpness is RCAS — a filter over single pixels — and clarity is a
band whose width is proportional to the image (invariant 6). Both were being judged on
the result column's fitted preview, a few hundred pixels wide, which is why the feature
read as broken while `/adjust` was doing exactly what it claimed.

**The ten sliders are generated from one table, `FILTERS`.** It carries the id, the
label, the odd override (sharpness starts at 0, hue prints degrees), and it is read by
the markup, `setPost`, `syncPostLabels`, `setBusy`, the `/adjust` body and the input
listeners — so a new slider is a row here and a field on the server's `Filters`, not a
block of markup plus five matching edits. The ids are the **wire names**, which is what
lets the request and the response be built by looping instead of by naming ten keys
twice. Don't hand-write a slider into the panel; the next one after it will be
hand-written too, and then the list has stopped being the truth.

The label sits **above** its track rather than beside it. Beside was right when there
were two of these under a full-width image; in a 300 px column a 72 px label was a
quarter of the travel.

Five more decisions in it are load-bearing:

- **Closing is the `×` and Escape, nothing else.** The old viewer dismissed on any
  click, which is right for something you only look at and hostile for something you
  edit in: one stray click on the picture and the picture is gone. Don't put the
  click-to-close listener back.
- **Fit, 1:1 and Reset are `.btn-choice`, the class the tab row wears.** They share
  that rule rather than copying it, because they are the same kind of control: a set
  where the highlighted one is where you are. Amber is reserved for the buttons that
  spend GPU time, so nothing in this row can be mistaken for an action — which is why
  `setFit` toggles `active` rather than a class of its own.
- **Fit and 1:1 are both needed**, because the two filters ask different questions — a
  look across the whole frame, and a pixel. 1:1 centres its scroll (`centreView`)
  rather than landing in a corner, and the centring runs twice: once inline and once on
  the image's `load`. A fresh `data:` URL has no dimensions to measure when
  `showFullSize` returns, and re-opening on the src already loaded fires no `load` at
  all, so neither half alone is enough. `lbCentre` is the flag that keeps a *re-filter*
  from re-centring — the whole point of panning to a detail is that dragging Sharpness
  leaves it under the cursor.
- **`setResultImage` writes both copies.** The image is on screen twice while the
  dialog is open, and `render()` keeps the big one in step too: a modal dialog stops
  clicks, not a batch run that was already in flight. Its write to the small copy is
  `P('result')?.`, because the Outputs pane has no small copy at all.
- **`cancelPost` is what makes a *different* image safe to put under the sliders.**
  It clears the debounce and bumps an epoch that `runPost` compares across its
  `await`, so a re-filter of the image being left can't land on the one replacing
  it. The Outputs tab's arrow keys are the only caller, and without it letting go of
  a slider and pressing → immediately reads as the arrow key having done nothing.

Panning is scroll offsets, not a transform, so the wheel, a trackpad and the scrollbars
keep working with no code. `.lbview` needs `min-width: 0` — a flex item's default
minimum is its content, so a 3072 px image would otherwise refuse to shrink its pane
and shove the panel off the left edge — and the image centres with `margin: auto`
rather than by centring the container, which clips the overflowing left edge and makes
it unreachable by scrolling.

The panel is a fixed 300 px and scrolls on its own because it is a stack that is
expected to grow; new sliders go in it, not under the image.

**Reset goes to `filters_start`, which is neither zero nor what is applied.** Not zero,
because "off" is not where two of the ten live: a fresh image arrives at the model's
sharpness, and resetting it to 0 would leave it softer than it was generated. And not
`filters`, because after a Refine or Hi-res those carry the grade across (invariant 1) —
taking them as the reset target makes every edit the next image's neutral, and there is
then no way back to an ungraded picture. `GenResult.start` is the profile's two values
with no grade, computed from the *retained* profile so it still describes the model that
ran after a checkpoint swap. `render()` stores it in `postStart`; double-clicking one
slider sends that one home, the only way back to an exact value with a pointer.

## Invariants

These break silently rather than loudly.

1. **Nothing ever diffuses over sharpened — or graded — pixels.** `Pending.raw`
   (`src/pipeline.rs`) is the unfiltered output-resolution image; every escalation
   reads it, and the display passes run exactly once, in `Engine::publish()`. A new
   pass that read `display` instead would compound sharpening along the whole ladder.
   `Engine::adjust` — the live sliders — rests on this directly: re-filtering `raw` is
   lossless however many times a drag does it. Break the invariant and the sliders
   silently become destructive.

   It is also what makes the grade safe to **carry across an escalation**
   (`Filters::restart`): the new pass diffuses the ungraded `raw`, so the grade is
   re-applied to the new pixels rather than stacked on top of itself. Sharpness and
   clarity reset there instead — they size their band from the image (invariant 6), so
   a number picked for the previous pixels is stale by definition. Detail resets,
   colour carries; that asymmetry is deliberate and the obvious tidy-up is to make it
   "consistent", which throws away a look on every rung of the ladder.
2. **sd.cpp *trims* an img2img schedule by strength, it doesn't scale it** — it keeps
   the last `steps × strength` of a `steps`-long schedule. So `SdRunner::img2img`
   (`src/sd.rs:102`) hands it `steps / strength`. Drop that and `--steps 2
   --strength 0.3` gives `t_enc = 0`: the image comes back untouched.
3. **`MAX_STRENGTH` is 0.95, not 1.0** (`src/config.rs:227`). sd.cpp only trims
   `if (strength < 1.f)`, so at exactly 1.0 the init image is discarded and the pass
   degenerates into a plain txt2img.
4. **`*_steps` is a floor, not an exact count.** Raised to `profile.steps × strength`
   in `img2img`, because strength decides how far up the sigma schedule the pass
   starts. Under-stepping a high-strength pass leaves un-removed noise, which reads as
   grit.
5. **Dimensions come back from `publish()`; they don't carry over.** sd.cpp rounds an
   init image to what the latent grid allows, so a pass can return a size it wasn't
   given. `snap64()` exists for the same reason.
6. **Both display filters size their band from the image, not the pixel grid.**
   `clarity_sigmas` and `rcas_radius` (`src/fsr/mod.rs`) are both proportional to the
   longest side, so a setting survives a hi-res pass. A fixed-footprint kernel silently
   grades a finer and finer band as the image grows — which is a look-wrong bug, not a
   crash, and it was only caught by someone saying sharpening "felt lost" at 3072.
   `fsr-selftest` measures this now, and `check_bands` asserts it for clarity, which
   holds to within a few percent across the range. **RCAS holds it in steps, not
   smoothly**, and is deliberately excluded from that assertion: `rcas_radius` is a whole
   number of pixels, so it is exact where a size ratio matches a step and drifts between
   them — measured, the fine band lifts 3.245× at 1536², 2.481× at 2048² (same radius,
   24% weaker) and 3.245× again at 3072². Don't read the equal numbers at 1536 and 3072
   as proof: those sizes double the radius along with the image, so they agree by
   arithmetic. Compare sizes *within* a plateau (768..2303, or 2304..3839) to see real
   behaviour. Fixing the sawtooth means fractional taps, which trades away the limiter
   guarantee — see `sharpen.wgsl`'s header before going there.
7. **`hires_max_side` gates the hi-res pass but deliberately *not* refine.** Hi-res
   can fall back to diffusing smaller and letting FSR cover the rest; refine has no
   such fallback, so gating it would just remove the button in the state where it
   helps most. `Pending::can_refine` documents this — it is not a bug to fix.
8. **`work/workitems/` is never referenced by a live `Pending`.** Every generation is
   archived there (`src/workitems.rs`); `Engine::run` *copies* `raw` in and
   `Engine::open` *copies* it back out into the usual `{id}-0-raw.png` scratch name. A
   `Pending` pointing straight at the archive would have its entry deleted by
   `Pending::remove_files` the first time it was refined, or by `Scratch::drop` if a
   pass failed — the feature quietly destroying the thing it exists to keep. One extra
   PNG write per generate (~15 ms at 1536²) is the price of the whole existing scratch
   lifecycle staying untouched.

   Which is why `purge_pngs` and `MAX_PENDING` are **unchanged**, despite reading like
   the cleanup to remove. Both only reach `work/` itself — `purge_pngs` is a
   non-recursive `read_dir` filtered on a `.png` extension, so it skips directories —
   and both still bound the scratch dir. What changed is that eviction is now harmless:
   an evicted image is reopenable rather than lost, so the 8-entry cache is a limit on
   what is already warm, not on what exists. `Engine::lookup`'s error says so.

   `Pending.work_id` is an id, never a path, for the same reason. It exists only to
   name saves (`work/output/{work_id}-s{step}.png`) and to let the UI mark which
   thumbnail is on screen. **`Pending.source` is a stem for the same reason again** —
   it names an image in `work/output/` that the user either saved or uploaded, and
   `Engine::img2img` copies it into scratch precisely so `remove_files` can't reach
   it. The one rule: nothing a `Pending` owns may live in a durable directory.

9. **Filter a clip's frames; never re-diffuse them.** `Engine::enlarge_clip` runs
   `Fsr::process` over each frame and that is safe *by construction*: EASU, RCAS and
   clarity are deterministic functions of the pixels, so two near-identical frames
   produce two near-identical outputs and a fixed kernel cannot flicker. Invariant 6
   covers the size change with it — both filters size their band from the image, so a
   sharpness setting survives the enlargement.

   Running the existing `refine` per frame would compile, run, and look plausible on any
   single frame. Every frame would draw its own noise and invent its own detail with no
   knowledge of its neighbours; adjacent frames diverge structurally and the eye
   integrates that as boiling or crawling texture. It is the signature artifact of
   per-frame SD upscaling and it is only visible *in motion*, which is why it belongs
   here rather than in a code review. Refining a clip properly is video-native: a second
   `-M vid_gen` pass with a VACE checkpoint and `--control-video`, which takes a
   directory of frames — precisely what pass 1 wrote, so no decode and no ffmpeg. That
   is a `family` entry and a second `SdRunner::video` call, not this function with a
   sampler bolted on.

   `work/videos/` is also the one archive written *through* rather than copied into,
   which looks like invariant 8 and isn't. That invariant exists because a live
   `Pending` deletes its own scratch files; a clip has no scratch stage, being written
   once by one pass and afterwards only read. What it does need is the failure half, and
   that is `videos::ClipDir` — a guard that sweeps a half-written directory unless
   `keep()` is reached. Cancelling is the *common* path here, not the rare one.

## Adding a per-model knob

The most repeated change here. Follow `img2img_cfg_scale` end to end for a *generation*
knob, or `sharpness`/`clarity` for a *display* one — the two are not interchangeable, see
the note below the table.

| # | file | what |
|---|------|------|
| 1 | `src/settings.rs` | field on **both** `Settings` and `PartialSettings`, then a line in `Settings::merged_with` and one in `Settings::as_profile` — both are struct literals with no `..rest`, so a miss here is a compile error |
| 1b | `src/config.rs` | field on `Profile` (what `as_profile` above fills) |
| 2 | `src/config.rs` | range-check in `Profile::validate()`, or in `Settings::validate` for a bound only the defaults have |
| 3 | `src/sd.rs` | consume it in `base_cmd` / `txt2img` / `img2img` |
| 4 | `src/pipeline.rs` | `Option<T>` on `GenRequest`, applied onto the profile in `Engine::run()` **before** `profile.validate()` — and, for an *escalation* knob, on `EscalateRequest` + its `apply()` as well |
| 5 | `src/main.rs` | field on the `/generate` `Dto`, plus `build_models_json` so the UI can pre-fill it — and, for a *display* knob, the `/adjust` `Dto` and `result_json` too; for an escalation knob, `EscalateDto` and `overrides()` |
| 6 | `web/index.html` + `web/js/image.js` (or `img2img.js`, `video.js`) | input in the Advanced panel; `syncAdvanced()` and `applyWorkItem()` pre-fill it, `generate()` reads it — an escalation knob goes in `escalateSettings()`, which `generate()` spreads, so it is read once and sent by all three routes |
| 7 | `src/settings.rs` + `web/js/settings.js` + `README.md` | default in `Settings::default()`, a row in `FIELDS` so it is editable, and a row in the `user/settings.toml` field table |

The trap this used to have — two structs with one field list, built in two arms of
`registry()`, so adding to one and not the other compiled cleanly and ignored the
knob for half the models — **is now a build failure**, and that is the only reason
the pair is still two structs. `Settings` and `PartialSettings` are joined by
`Settings::merged_with`, and `Settings`/`Profile` by `Settings::as_profile`; both
are spelled-out literals with no `..rest`, so a field added to one end and not the
other has nowhere to hide. Don't "tidy" either into a `..Default::default()` — that
is exactly the tidy-up that puts the trap back. `registry()` now has one arm, not
two: a discovered checkpoint is a template that states only where the file is.

Step 7 is also different now: a new knob's default goes in `Settings::default()`
and its documentation in the `user/settings.toml` table, **not** in `models.toml`
— that file no longer carries defaults at all. A knob that should be editable
rather than only hand-typed needs a row in `web/js/settings.js`'s `FIELDS`, which
is one line if a control for it already exists on another tab.

**A *video* knob is a shorter path and the trap doesn't apply**: there is one struct,
`VideoModel`, because a `[[video]]` entry has no settings/`[templates]` split to keep
in step. Video *is* discovered, but an explicit entry replaces a discovered model rather
than merging with it, so there is still only one place a field lives. Add the field there
(with `#[serde(default)]` unless it is genuinely required), give it a value in
`VideoModel::discovered()` — a struct literal, so forgetting it is a compile error rather
than a knob that silently only works for half the models — range-check it in
`VideoModel::validate()`, consume it in `SdRunner::video_cmd`, add an `Option<T>` to
`VideoRequest` applied onto the clone in `Engine::video`, a field on the `/video` `Dto`,
and an input plus a line in `syncVideo()`/`applyClip()`. A knob whose right value depends
on the architecture belongs in `VideoFamily::recipe()` rather than in a `default_*`
function. Anything the page must *derive* from `family` goes on `VideoModelDto` instead
of being reimplemented in JavaScript — see `frame_grid` below.

The CFG is that pattern three deep — `refine_cfg_scale` → `img2img_cfg_scale` →
`cfg_scale`, through `Profile::refine_cfg`, and the same for hi-res. The middle rung is
not decoration: it is what makes the split invisible to an install that already pinned
the old shared *Refine/hi-res CFG*, which keeps running both passes at that number until
one of the two is set. `each_pass_falls_through_to_the_knob_below_it` is that under
test, including that `Some(0.0)` is guidance off rather than unset. `SdRunner::img2img`
takes the resolved number as an argument rather than asking the profile, because the
three passes through it resolve it differently and a function that picked one of them
would be right for two callers out of three.

A knob that *defaults to another knob* stays `Option<T>` all the way onto `Profile` and
resolves through a method — `img2img_cfg_scale` / `Profile::img2img_cfg()`. Resolving it
to a number at load, or pinning it in `Engine::run` when the request omits it, both
compile and both quietly sever the link: raising `cfg_scale` would leave the escalations
at whatever the base CFG used to be. For the same reason `build_models_json` emits `null`
rather than the resolved value — the UI has to be able to tell "unset" from "set to the
same number", because they diverge the moment the other box moves.

Per-request overrides are applied onto a *cloned* profile, which is then retained in
`Pending` — so a later refine/hi-res uses the settings the image was generated with,
not whatever the UI shows now. Keep that property. It is what makes a generation
reproducible from its sidecar, and it is the only reason `open` can restore an image
faithfully.

The retained profile is the **starting point** for an escalation, though, not a pin:
`/refine` and `/hires` take an `EscalateRequest` (`src/pipeline.rs`) and fold it on top
before the pass runs. Only the knobs that describe a pass which *hasn't happened yet* —
`refine_steps`, `refine_strength`, `refine_cfg_scale`, `hires_steps`,
`hires_strength`, `hires_cfg_scale`, `img2img_cfg_scale`, and **`template`**. `steps`, `cfg_scale`, `sampler` and the base
size are settings of a txt2img that already ran, so there is nothing a new value could
do to the image in hand; they stay retained, and the next *generate* picks them up. Get
that boundary wrong in either direction and the Advanced panel starts lying: honour a
base knob and the image claims settings it wasn't made with, ignore an escalation knob
and pressing the button after editing the box quietly does the old thing.

`template` passes that test and is worth stating separately, because it looks like a
base setting and isn't: a *checkpoint* is a property of the pass about to run. It is
applied by `Engine::swap_model`, not by `EscalateRequest::apply`, because resolving a
name needs the registry — and it must run **before** `apply`, since the swap replaces
the whole profile and would otherwise discard the boxes folded onto it.

The swap is **wholesale**, and that is the decision, not an oversight. The named
model brings its own `refine_*`, `hires_*`, `img2img_cfg_scale`, `hires_max_side`,
`weight_type`, `extra_args` and slider start points, and the request's explicit knobs
land on top — which is consistent, because `syncAdvanced()` already refills those
boxes from whatever model is selected. A field-wise merge is the `Defaults`/`Template`
trap from the top of this file wearing a hat: every knob would have to become an
`Option` so "unset" could be told from "set to the default". The replaced profile is
what the new `Pending` retains, so `can_refine`, `can_hires` and where the sliders
reset all describe the model that actually ran — which is what keeps a saved sidecar
true, and also means a swap can remove the Hi-res button (new model, `hires_steps = 0`)
or declare the image past budget (smaller `hires_max_side`).

**`refine_model` and `hires_model` are the one exception to that replace**, and it
is not a field-wise merge sneaking back in. They are request-only — neither
`registry()` arm sets one, so the incoming profile always has them `None` and
there is nothing of the checkpoint's to merge with. They say what the *user*
means to finish with, which no checkpoint has an opinion about, so `swap_model`
lifts them off the old profile and puts them back on the new one, and then the
pass writes its own choice over its own field. Without that, refining twice would
leave a saved output's sidecar claiming no finishing checkpoint was ever named.

They are on `Profile` at all for the reason `loras` is (*## LoRAs*): the profile
is what a `Pending` retains and what a sidecar stores, so it is the only place
that makes a reopened work item restore the two pickers. **That is what the
`/generate` and `/img2img` bodies carry them for** — neither request runs either
pass. Before this they were dropped on the way out of `escalateSettings()`, which
is exactly why the choice used to survive nothing.

On the wire an *escalation* names it **`model`, not `template`**, and that is
load-bearing: `template` already means the model doing the txt2img, and `/refine`
and `/hires` each run exactly one pass, so one field is the whole answer there. A
*generate* spells the same choice as the pair above instead, because it is
recording two intentions rather than naming one pass.
`an_escalation_may_name_a_different_checkpoint` pins the escalation half, and
`escalateModels()` in `image.js` is the page's.

`EscalateRequest.img2img_cfg_scale` is `Option<Option<f32>>` for the reason the
"defaults to another knob" paragraph above gives: absent = keep, `null` = unset it so
the pass follows `cfg_scale` again, a
number = pin. Serde collapses `null` into `None`, so the DTO needs
`deserialize_with = "double_option"` — without it an emptied box can never be cleared,
only overwritten. `escalate_body_tells_an_empty_cfg_box_from_a_missing_one` guards both
that and the `#[serde(flatten)]` on the hi-res body, and needs no GPU.

The **display** filters are the deliberate exception, and the distinction is easy to
collapse by accident. `profile.sharpness`/`.clarity` are where the sliders *start*;
`Pending.filters` is where all ten of them are *now*. `adjust` writes the second only,
and `run`/`img2img`/`open` reset it from the first — so every new image arrives
unfiltered. Putting a live value back on the profile would compile fine and silently
restore the old inherit-forever behaviour. (`refine`/`hires` are the half-exception:
`Filters::restart`, invariant 1.)

**`loras` is not per-model either**, and for a different reason than the grading
knobs below: it *could* have been, and was deliberately left off `Defaults` and
`Template` because a LoRA is chosen per picture rather than being a property of a
checkpoint. It lands on `Profile` straight from the request. See *## LoRAs*.

**The eight grading knobs are not per-model at all**, and that is the reason the table
above doesn't apply to them: they live only on `Filters`, so adding the eleventh is a
field there, a row in `Filters::table`, a line in `grade.wgsl` plus its `GradeParams`,
and a row in the page's `FILTERS` — no `Defaults`/`Template` pair to keep in step, no
`registry()` arm, no `models.toml`, no field-table row. A checkpoint has a house
sharpness; no checkpoint has an opinion about your hue. If one ever needs a start point,
that is the full seven-step path above, and the trap at the top of this file applies
again.

## Conventions

- Comments explain *why*, usually with a measured number (flash attention is 6× at
  2048²; clarity costs ~1 ms at 3072²). That density is the house style, not excess —
  match it, and measure before claiming.
- `anyhow` throughout. Errors are user-facing strings with the fix in them; see
  `SdRunner::preflight` and `Engine::lookup`.
- std-only HTTP by design (`src/http.rs`). Don't reach for a web framework.
  - `http.rs` **strips the query string before routing**, so an id has to be a path
    segment (`GET /workitems/{id}/thumb.png`, a guard arm in `handle_conn`) or a POST
    body. `workitems::valid_id` is the traversal guard and is a *whitelist* — digits and
    `-`, so `.` and `/` aren't in the alphabet at all. Widen it rather than replacing it
    with `canonicalize`.
  - `Job.reply` carries the **finished JSON body**, not a `GenResult` — not every job
    produces an image (`SaveOutput` answers with a path). The worker renders it; that's
    why `result_json` is called in the worker match rather than in `submit`.
  - Routes that only touch the filesystem (`/workitems` list, the thumbnail,
    `/workitems/delete`) are answered **on the connection thread**, not through the job
    queue. The worker is serialized, so queueing a thumbnail behind a diffusion would
    freeze the grid for the length of the render. The worker only ever *writes* to that
    directory, so a listing that races a write just misses the newest item.
  - `/progress` and `/cancel` are on the connection thread for a sharper version of the
    same reason: they are *about* the job the worker is running. Through the queue, a
    progress bar would first update once the render it reports on had finished, and Stop
    would become pressable only when there was nothing left to stop.
  - The video routes split the same way. `/video` and `/videos/enlarge` are GPU work and
    go through the queue; `/video-models`, `/videos`, `/videos/{id}/thumb.png`,
    `/videos/{id}/frames/{n}.png`, `/videos/delete`, `/outputs` and
    `/outputs/{stem}/thumb.png` are filesystem-only and answer on the connection thread.
    That matters more here than it does for a still: a clip holds the worker for
    *minutes*, and the frame player fetching 33 PNGs is exactly what must not queue
    behind it.
  - `/outputs/{stem}/thumb.png` is the one thumbnail route that *generates*. Output PNGs
    are full resolution (up to 3072²) and were written long before there were
    thumbnails, so `ensure_output_thumb` makes one on first request and caches it. It
    writes to a counter-suffixed temp name and renames — two requests for the same tile
    are two threads of *this* process, so a pid in the name would have given them the
    same file.
- **`SdRunner::run` streams the child's output; it must not go back to
  `Command::output()`.** Three things about that are load-bearing and all three break
  silently:
  - Progress is on **stdout**, not stderr. sd.cpp sends `SD_LOG_ERROR` to stderr and
    everything else — including the bars — to stdout (`examples/common/log.cpp`,
    `out_stream`). Both are read anyway, since a failure message wants the informational
    tail too, and stderr gets its own thread because a pipe nobody drains fills up and
    deadlocks the child mid-render.
  - `drain` splits on `\r` **and** `\n`, which is why it isn't `BufRead::lines`. sd.cpp
    ends each bar with a carriage return so it can overwrite it in place, and only the
    final step of a pass gets a newline — read by lines, a 20-step render delivers its
    whole progress history in one blob after the last step, which is exactly when it
    stops being progress.
  - `Monitor` parks the `Child` in a `Mutex` for the length of the run so `cancel` can
    reach it, and `run` takes it back out *before* `wait`. Waiting while holding that
    lock would hold it against the very cancel it is waiting for.
  - `rate` is sd.cpp's own text, kept verbatim. It already picks its unit (`s/it` below
    one iteration per second, `it/s` above; `MB/s`/`GB/s` while loading weights), and
    `Phase` is derived from that suffix. Parsing it into a number means a second
    implementation of the same decision, free to disagree with what the terminal says.
- JSON out is hand-rolled `format!` for the two long-standing shapes (`result_json`,
  `build_models_json`) and `serde_json` for anything backed by a real struct (work item
  sidecars, `GET /workitems`). `serde_json` was already a dependency for parsing; don't
  hand-roll a 22-field round-trip, and don't churn the two that exist.
- **Batch and loop generation are a client loop, not a job.** The Image-to-image tab
  is deliberately single-shot and has no run panel: the run panel's value is drawing
  N *different* pictures from one prompt, and with a fixed source image the only
  variation left is the seed. `startRun()` in
  `web/js/image.js` calls `/generate`, then `/hires`, `/refine` and `/save-output` per
  image; there is no batch route and there shouldn't be. The worker is a single blocking
  consumer (`for job in job_rx`), so one job that owned it for a whole run would leave
  `/adjust`, the grid and every other client dead for the length of it, and cancelling it
  would need a second channel into that loop. Looping over the existing routes costs one
  round trip per image against a multi-second diffusion, and is what makes each result
  appear as it lands and Stop only have to wait for the image in flight. The stated cost
  is in `README.md`: the loop lives in the page, so closing the tab ends it after the
  current image.
  - The automated passes go through the same `escalateSettings()` the buttons use, and
    the boxes are read once per image rather than captured at the start. A run must not
    grow its own copy of the knob list — that is how the panel starts lying again.
  - `#stop` is deliberately **not** in `IDLE_LABELS`, which is why `setBusy` never
    disables it. A stop button that greys out for the length of the thing it stops is
    furniture.
- **Video models are discovered by reading their headers, never their filenames**
  (`src/discover.rs`). A checkpoint is one file, so the image side can identify it by
  dropping it in the models dir. A video model is three to five files — transformer, VAE,
  text encoder, sometimes a *second* transformer — whose roles are not in their names, so
  the scan opens each one instead: safetensors keeps a JSON tensor index behind a `u64`
  length, GGUF a typed key/value block, and the tensor names in it name the architecture.
  One seek and a few hundred KB against an 8-20 GB file.

  The discriminators are lifted from sd.cpp's own `ModelLoader::get_sd_version`
  (`src/model_loader.cpp:495-605`), because the only classification worth having is the
  one the binary we shell out to will agree with. **Read shapes off square tensors only**
  (`Tensors::square_dim`): safetensors stores a shape in PyTorch order and GGUF stores it
  reversed, so a rectangular tensor means picking an end per format and being wrong in
  one of them. A model's width is in any attention projection, which is `[dim, dim]`.

  The case that justifies the whole approach is in the models dir: a file named
  `minimax_h3Comfy.safetensors` is a Qwen3-VL-32B *text encoder*. Filename matching would
  have slotted 15 GB in as a transformer and failed minutes into a run.

  - **`[[video]]` did not go away, it changed job.** It names files that aren't
    downloaded yet (so the listing stays a checklist), pins measured settings, and
    settles the one question no header can answer. An entry **replaces** a discovered
    model rather than patching it, matching on `name` or on naming the same
    `diffusion_model`. Replacing wholesale is what keeps `VideoModel` a single struct — a
    field-wise merge would need every knob to be `Option` so "unset" could be told from
    "set to the default", which is the `Defaults`/`Template` trap from the top of this
    file. What an entry omits comes from `VideoFamily::recipe`, not from the discovered
    copy.
  - **Which half of a MoE pair a file is cannot be read from it.** The two experts have
    identical tensor names and identical shapes and differ only in their weights, so
    `noise_marker` falls back to the filename — the only place the information exists.
    When the filename says nothing either, the file becomes a `wan_dense` model that runs
    alone, **not** a guess. That variant exists for exactly this: repackagers ship single
    14B files with no marker, sd.cpp runs a lone expert quite happily, and the
    alternatives were to guess which half it was or to refuse to list a runnable file.
  - A missing component is not an error at startup: a half-downloaded 20 GB model is the
    normal state of one for an hour, so `doctor` and the model select list it greyed out
    *with the filename it is waiting for*. Discovery keeps that property — a model whose
    VAE isn't there yet points at the conventional name **inside the role directory it
    belongs in**, so the path printed is also an instruction. Download one expert of a
    pair and it names the other one exactly.
  - The role subdirectories (`diffusion_models/`, `vae/`, `text_encoders/`, `loras/`,
    `clip_vision/`) are ComfyUI's layout, which is what every download instruction
    assumes. **The scan is recursive, so they are for the human, not the program** — a
    flat folder discovers exactly the same models. They exist because the encoder is
    *shared*: one umt5 serves every Wan family, so a folder per model would mean copying
    7 GB per model or a tree of symlinks.
  - **`family` is the load-bearing field.** It decides which flags `video_cmd` emits
    (the A14B pair needs a whole second `--high-noise-*` set; H3 needs `--llm` and
    `--audio-vae`), which grid sizes and frame counts round to, and whether fps is the
    user's to choose. Keeping it explicit from day one is what makes a new architecture a
    few lines of config rather than a refactor — `minimax_h3` is already a variant, and
    is unrunnable here only on memory grounds.
  - **The two grids round in opposite directions**, which is why `align_frames` isn't one
    formula. Wan goes through `((f - 1) / 4) + 1` latent frames and back, and integer
    division truncates, so 34 frames runs as 33 — *down* to `4k+1`. H3 has its own
    branch, `while (frames % 17 != 5) ++frames` — *up* to `17k+5`, floor 5. Sizes always
    round **up**, to `vae_scale_factor × 2`: 32 for the TI2V-5B and H3, 16 for every
    other Wan. All of it is in sd.cpp's `src/pipeline/diffusion_engine.cpp` and
    `src/pipeline/request.cpp`; sd.cpp aligns and *warns*, so getting the direction wrong
    here doesn't fail, it just makes the sidecar describe a clip that doesn't exist.
  - The page is handed those rules as data (`VideoModelDto::frame_grid`,
    `size_multiple`, `forced_fps`) rather than deriving them from `family`. It rounds the
    frame count to show a duration and `video_cmd` rounds it to run the model; two
    implementations of `4k+1` that disagree by one frame is exactly how the box above the
    button starts lying. `the_frame_grid_describes_the_rounding` takes `align_frames` as
    the truth and asserts the published grid explains every answer it gives.
  - **A clip is a PNG sequence, not a container**, and `-o` cannot be both — a format
    specifier in the output path takes an early branch in sd-cli that writes one file per
    frame, and without one it muxes a single `.avi`/`.webm`/`.webp`. Frames win because
    of what happens next: both escalations consume frames (`Fsr::process` per frame,
    `--control-video` for a VACE pass) and this binary has no video decoder to get them
    back out of a container — `image` is compiled `features = ["png"]` and sd-cli's muxer
    is internal to sd-cli, with no mode that runs it over existing images. A container
    would mean regenerating the clip, at minutes a go, to do anything with it. Frames are
    also lossless, which is invariant 1's argument again: what gets filtered should not
    already be compression artifacts. The price is disk, ~700 KB a frame at 832×480, so
    ~23 MB for a 33-frame clip. Playback is therefore a `<canvas>` and a timer rather
    than a `<video>`, which also buys frame-exact scrubbing.
  - The video Stop is `POST /cancel` and **kills the child**, unlike the image tab's
    `#stop`, which lets the image in flight finish and then ends the run. There is only
    ever one clip in flight, so stopping it can only mean killing it. A cancelled run's
    error ends in `sd::CANCELLED`, which the page reports as "Stopped" rather than as a
    wall of sd-cli stderr.
- **A file's weight type on disk is not what it costs to run**, which is why there is a
  Convert tab (`src/convert.rs`, `-M convert`). `Linear::forward`
  (`model/common/ggml_block.hpp:211`) asks the backend whether it can matmul the stored
  type and, when it can't, puts a `ggml_cast` in the *compute graph* for every linear
  layer — so an fp8 checkpoint on a Vulkan device (all of which report no fp8) executes
  as BF16 and needs about twice its file size. `q4_K` is not a downgrade from fp8 there;
  it is the first type in the list the backend can actually multiply.
  - **The component flag renames the tensors.** `convert_with_components`
    (`sd.cpp src/convert.cpp:354`) takes a path per slot and prefixes every tensor name
    by which slot it arrived in: `--diffusion-model` → `model.diffusion_model.`, `--vae`
    → `vae.`, `--t5xxl` → `text_encoders.t5xxl.transformer.`, `-m` → nothing. The wrong
    flag writes a *valid* file full of misnamed tensors that fails minutes into a later
    run. `Slot::for_role` is that mapping and it is derived from the discovered role, so
    the client never picks it. Roles with no slot at all (`QwenVl` needs `--llm`,
    `ClipVision` needs `--clip_vision`, neither being a parameter of that function) are
    left off the list rather than squeezed through a neighbouring flag. Prefixing is
    idempotent (`model_loader.cpp:292`), so a converted file round-trips.
  - **`-t 1` is load-bearing, not caution.** sd.cpp's streaming converter runs a worker
    per core, all calling `ModelLoader::load_tensor` on one shared loader
    (`convert.cpp:244`), which re-enters `load_tensors` → `process_model_files` and
    mutates that loader's `file_data` unlocked. At the default it aborts in glibc on
    roughly one run in three. The crash is survivable — `Engine::convert` removes the
    partial file — but the race is in the loader's bookkeeping *while other threads read
    through the mappings it is remaking*, so it can also write wrong bytes and exit
    cleanly. Measured cost of serializing: 254 MB in 2.07 s against 0.53 s at four
    threads. Revisit if upstream takes a lock.
  - **A shape does not always survive a conversion, and the VAE is where that bit.**
    GGUF has four dimensions, so exporting a 5-D Conv3d weight folds the trailing axes:
    `conv2.weight` `[16, 16, 1, 1, 1]` comes back `[1, 1, 1, 256]`, element count kept
    and the width gone. So the Wan VAE's `z_dim` is read off `conv2.bias` — one
    dimension, therefore immune to both that and the PyTorch/ggml order flip — with
    `square_dim` on the weight kept only as a fallback. `Tensors::vector_len` is for
    exactly this, and `a_vae_survives_the_trip_through_gguf` is the regression. Biases
    are never quantized (`model_loader.cpp:1517`), so they are always there to read.
  - The size shown before the button is `estimate`, which transcribes
    `ModelLoader::tensor_should_be_converted` (`model_loader.cpp:1509`) rather than
    assuming everything is quantized — a transformer keeps every bias, norm scale and
    embedding at full width, which is enough to matter when the question is "does this
    fit in 24 GB". It lands within 0.01% of a real conversion. The **type table is
    served**, not written into the page, for the reason `frame_grid` is: a second copy in
    JavaScript is free to disagree with the one the CLI validates against.
- No Python anywhere in build or runtime. That is the project's whole pitch.
