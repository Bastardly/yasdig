---
paths:
  - "src/workitems.rs"
  - "src/pngmeta.rs"
  - "web/js/workitems.js"
  - "web/js/gridmark.js"
  - "web/js/outputs.js"
---

# Favourites, Outputs, settings in the PNG

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
- **`delete` touches one shelf.**
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

## The Outputs tab

`work/output/` as a gallery (`web/js/outputs.js`, `Display::open_output`). Click opens
the Refine & adjust dialog; ←/→ step. **No Hi-res here** (it is the Image tab's action
row); the dialog's passes run on the opened file — a diffusion over finished pixels,
chosen, like picking it as a source (see *Redraw* in `viewer.md`). Sliders here stack on baked-in
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
  **Delete all = the same request with every id.** One of eight `confirm()`s
  (with Close server, Jobs Stop/Remove, the Outputs viewer's unsaved edit, redrawing a film's first frame,
  deleting the Assistant's chat — the panel has no click-to-delete grid — closing the cast editor
  with tries unsaved, and deleting a model file in Installed, which names what uses it).
- **`outputGrid` is the one tile** (`height`, `empty`, `onPick`, optional `star`); height must match
  `.wigrid`.
- **Arrow keys yield to a focused range input**; listener on `document`.
- **Opens coalesce like a slider drag**; `cancelPost` bumps the epoch `runPost` checks.

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
  re-encode (which strips the chunks from any PNG it redraws). A yasdig answer goes through `applyWorkItem`; an
  A1111 one through `applyParams`, which sets only the fields it names, model first
  (its `change` refills every box).
