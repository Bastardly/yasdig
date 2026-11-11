---
paths:
  - "src/catalog.rs"
  - "src/catalog.toml"
  - "src/downloads.rs"
  - "src/inventory.rs"
  - "src/notes.rs"
  - "web/js/getmodels.js"
  - "web/js/downloads.js"
  - "web/js/notes.js"
---

# The Models tab: downloads, the catalog, Installed

## Downloads

Models tab: `src/catalog.toml` (compiled in), `src/catalog.rs`, `src/downloads.rs`,
`GET /catalog`, `POST /downloads/{start,cancel,clear}`, `web/js/getmodels.js`. A set
per runnable family, each file keyed by its `slots()` key (or `"model"`); **a family
may have two cards** (MOSS INT8 + BF16), so its panel, rail button and Help's family
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
- **`File.replaces`**: an earlier card's file that still fits the slot but doesn't
  count as had, so the card offers its successor (LTX-2.5's bf16 encoder).
- **Own lane, never the job queue**; *Close server* calls `cancel_all`. Cancel keeps
  the `.part`.
- **`PARALLEL` (3) workers, one FIFO, one key per dest path**: `State.running` is a
  map of `Run {task, child, cancelled}` — each `fetch` parks and takes back its own
  child, so a cancel never kills another worker's curl. `State::busy` (running *or*
  queued) is what keeps a shared file to one fetch. `GET /downloads`
  (`Downloads::summary`) reads only `.part` lengths, never a scan; `Task.label`/
  `entry`/`index` exist for it and for `/downloads/cancel {entry, file}`.

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
**Installed** (`src/inventory.rs`, `GET /installed`, the rail's first section): one
list from `video_scan` (which holds the image scan, so a shared VAE is one row),
`list_loras`, `list_upscalers`, `agents::scan` — no header read of its own. **An id is
`<root>/<rel>`** (`id_of`, most specific root first, since the others sit inside
`models/`); `resolve` takes only a known root and `Normal` components, and `find` is
the whitelist (in a fresh `list`). `group_of` is by role alone (unusable LoRAs and
upscalers are `other`); `families_of` is every family with a slot that fits.
Fetched on entering the tab and on `downloadsChanged`, never on the progress poll.
Built with the rail, so the first `refresh` waits for both answers.

- **Delete is `inventory::delete`** (`POST /installed/delete`, connection thread):
  `find` in a fresh list, refused while *any* job is in the queue (which job reads
  which file is in each profile; Stop is the clear answer) or `Downloads::busy`.
  Files and `.part` only, never a folder; `PermissionDenied` is "in use" (Windows).
- **Settings follow by path, not name** (`ModelSettings::follow_delete`, tested):
  registry names are stems made unique in scan order, so deleting `a/model` makes
  `b/model-2` into `model` — `delete` maps name → path before and after, parks the
  deleted model's tables *now* (before a survivor takes the name), moves each
  survivor's, and drops part pins equal to the file's models-dir name.
- **Notes** (`src/notes.rs`, `user/model-notes.toml`, one `LOCK`, `.tmp` + rename)
  are keyed by Installed id; a gone file's note is kept, only a delete removes it.
  `note_id` rides on `/models` (hand-rolled JSON), `/video-models` (`VideoModelDto`)
  and `/loras` (a `Value` patch, so `LoraFile` stays the config type). The page:
  `notes.js` — every i is a `[data-note]` `.btn.btn-info` (blue `--info`, `.has`
  filled), one document click listener, `setInfo` re-points one (`syncParts`,
  `syncReady`, a LoRA row's select). **A LoRA row's × handler is `button[data-i]`**,
  or it would remove the row on an i click.

**The top bar's Downloads** (`downloads.js`, `#dlbtn`/`#dldlg`) is the only download
poll: 1 s while active, 5 s idle (another browser tab's download). `downloadsChanged`
(the `observable` from `jobs.js`, on `finished`; the first answer is a silent
baseline) reloads the model lists; `onDownloads` refetches `/catalog` — which scans —
only while the Models tab shows, plus once after activity stops (a dialog cancel).
A failure stays listed until retried or `POST /downloads/dismiss`.

Page: the tab is a **workspace**: `#gmrail` has a `details.fold` per section (Installed, then each category; `rememberFolds`, `gmfold-<id>`; `openModels` opens the fold holding its button)
(`categories()` in `getmodels.js`) and a button per family, `panel-gm-<family>` (or
`gm-upscaler`/`gm-agent`, `SINGLE`) a floating panel of its cards. **Built once from the
first `/catalog`** (`build()`; the catalog is compiled in), then a poll rewrites only
the bodies (`data-gmbody`, skipped when unchanged), so position/scroll/focus stay;
`stack` keeps two dozen panels from cascading off the window. The Recommended mark is
per panel over *every* category (it used to read `view.entries` only, missing video).
`openModels(family)` is `wsPanels.show` (`panels.js`: open *and* raise) — `#getparts`,
Help, the Assistant and Fill's link call it.
