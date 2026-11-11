---
paths:
  - "src/cast.rs"
  - "web/js/cast.js"
  - "web/js/casteditor.js"
  - "web/js/castpicks.js"
  - "web/js/castnames.js"
---

# Cast

## Cast

`user/cast/{id}/` (`src/cast.rs`, `web/js/cast.js`, `web/js/casteditor.js`): named
characters, places and props — a film's `Member` with a life of its own (`MemberKind`
reused; the page labels `thing` *Prop*). `cast.json`, `picture.png`, `sheet.png`,
`voice.wav`, `thumb.png`, `tries/`. Under `user/`: nothing sweeps it, so no Favorites
shelf, and Close server's clear never reaches it.

- **An entry copies in, never adopts.** A picture or voice from Outputs is copied into
  its folder (`CastStore::save`); deleting either side never breaks the other.
- **Tries are the only thing a draw writes.** `/cast/draw` adds `tries/{n}.png` on the
  fields it is sent; the record's fields change only on `/cast/save`, which keeps the
  picked try and empties `tries/`. A new entry is a `draft` folder from its first draw
  (`/cast/new`), so tries survive a reload; `discard` deletes a draft, only empties a
  saved one's tries. `next_try` never goes back, so a cached try URL never shows
  another picture.
- **A name is a key**: prompts and storyboards find an entry by it, so `save` refuses a
  name another saved entry has (case aside). Drafts don't count.
- **Every saved entry has a picture** (`save` refuses one without; the page's Save is
  disabled until a try or a picture of the person's own is shown). The picture is
  drawn from the look (`derived_prompt`) — there is no separate picture prompt, by the
  user's call: it was the look said twice.
- **`CastStore::dir` is the only id → path**, through `valid_id`; one `LOCK` over
  record writes (the worker adds a try while the connection thread may save).
- **Drawing is the film's code** (`agent::draw_one`, shared with the storyboard's
  pictures; `SHEET`/`FRONT`/`board_size` are `pub(crate)`): a character on a
  `takes_references` model draws the 2:1 sheet, then the picture with the sheet as a
  reference **by path** (`GenRequest::ref_paths`, staged ahead of `references` and
  counted against `MAX_REFERENCES`, never on the wire or retained). The sheet goes
  through scratch, since sd-cli may only be handed paths under a sandbox root. Kept
  from the work item's raw PNG, not `GenResult.png` (sharpened).
- **Each try is a work item** in Current, like a storyboard picture (`run_in`,
  `Slot::Agent`, so it never takes the Image tab's picture).
- `Work::CastDraw` / `Kind::Cast` answer on their request (`enqueue_wait`); the editor's
  Draw becomes Stop (`stopKind('cast')`), and Discard stops a draw before deleting its
  folder.

**On the Image tab** (`web/js/castpicks.js`, the Prompts panel's Cast row):
`Profile.cast: Vec<CastPick {id, name, words, picture}>`, request-only like
`references`, on `GenRequest`/`Img2ImgRequest` and both DTOs.

- **Composed on the server** (`cast::resolve` → `cast::compose`, in `Engine::run` and
  `img2img` right after `variables::expand`): the words picks' looks
  (`cast::look_paragraph`, shared with the storyboard's `keyframe_prompt`) lead the
  rolled prompt, a paragraph each. `Pending.prompt`/the work item hold the composed
  text, so passes and sidecars see what ran; `typed_prompt` (variables' mechanism —
  it differs, so it is kept) is what goes back in the box.
- **Picture picks are references by path** (a character's `sheet.png`, else
  `picture.png`), staged ahead of `ref_paths` and `references`; `Profile::validate`
  counts them into `MAX_REFERENCES` and refuses them off a references family.
  `img2img` clears every picture flag (no references there); an agent model switch
  (`knobs::write`) does too.
- **`resolve` fills `name`** onto the retained picks. The PNG record carries picks
  **without ids** (`EmbeddedProfile::of`); `pngmeta::read` finds each by name in
  `Install.cast`. **One missing → none**: chips cleared and `typed_prompt` dropped, so
  the box gets the composed prompt, every look written out. `applyWorkItem` does the
  same when `setCastPicks` finds an id gone. Never chips beside a composed prompt (a
  look twice) or a short prompt with no chips (a look lost).
- **Names in the prompt are offered, never picked** (`castnames.js`, pure, tested):
  whole word, case kept, so a member named "Rain" isn't found in "rain".
- The row's chip changes fire a bubbling `change` on `#castchips` for the tab's
  delegated listener (summaries, loop, an Image chat's settings); `syncSummaries`
  calls `syncCastPicks` (Picture ticks shown only while `refsActive()` and no source).

**In clips and films** (what can only name an output stem):

- **`CastStore::output_stem` is a cache** (`Entry.stem`/`sheet_stem`/`voice_stem`,
  `POST /cast/output`): reused while either gallery has it, re-imported when deleted,
  dropped by `save` when the file it copies changes. The Image tab never goes through
  it (paths), so Outputs fills only with what a clip or film used.
- **The `#picker`'s Cast source** (`source-slot.js`) turns a pick into that stem, so
  every slot takes a cast member unchanged; the cast editor's own Choose… leaves it
  out (`{cast: false}`).
- **A film member from the cast is the person's** (`agent::cast_member`: output
  copies, pictures kept in the chat's `img/`, `kind: "output"`), so it is never
  redrawn; `Member.cast_id` names the entry. *Add from cast…* is `/chats/film
  {from_cast}` → `BoardEdit.add` (name unique in the film, `MAX_CAST`).
- **A story turn reuses the cast by name, only as asked**: `Toolkit.library` (saved
  entries with a picture, Video chats only, `serde(skip)`) is cut by
  `toolkit::offered` to the ones the person's messages name (whole word, as spelled),
  the film already has, or all on "my cast" — the whole list had a camel-and-parrot
  film pull in unrelated characters — and listed in the turn (`library_text`, capped); `apply_story` gives a name match (case aside) the entry's spelling, kind,
  look and `cast_id`, and keeps an old drawn picture only for the same entry. The
  files arrive after the turn (`adopt_cast`, end of `turn`), only where the member has
  none. film.md is at the skill budget, so the instruction is in the turn's text.
- *Save to cast…* on a member opens the editor prefilled (`picture`, `sheet`,
  `voice` stems; `Pick::Outputs` keeps the turnaround).
- **A Cast pick carries `extra`**: the picker calls `onPick(stem, {cast, voice})`
  (`voice` a character's voice, also an output copy). `sourceSlot`'s `onCast`, the
  References dialog's `castPicks` → `editRefs`' third `onDone` argument → `castOnRow`
  (video.js) add the voice to the shot's sounds as that name's, and the name to *Who
  is in it*; Sounds fills *Voice of*.
- **A shot's *Who is in it* is chips** (`data-cast` on the row, `rowCast`/
  `drawRowCast`; a name still in the box counts via `rowCast(el, true)`). Its
  *Add cast…* (`addCastToRow`, `castEntry`) is `castOnRow`; a chip's × is
  `removeCastFromShots(name, [row])`. **A continue has no cast** (it starts from the
  last frame, whoever that shows): the row hides it, Continue clears it, `sentRow`
  sends none and `rowMatches` ignores an old clip's.
- **Unbound, the Cast panel is the rows** (`clipCast`, read off references'
  `subjects`; no state of its own): *Add from cast…* → `addCastToShots` (shot 1 and
  every cut), Remove → `removeCastFromShots` by name. Redrawn on `onRowsChange`
  (end of `syncShots`), only when its HTML moved.

**Made by the agent** (`action: "cast"`, `member {name, kind, look}`, gated by
`Toolkit::casts`: `Rules.cast` and an image model):

- **`set_cast` is pure; the entry is made by the draw** (`Next::Cast` →
  `Step::CastTry` → `agent::cast_try`): `ChatCast.id` empty until the first draw
  creates a draft, bound before drawing (a failed draw leaves no second draft).
  Unbound, a `Toolkit.library` name match binds the saved entry. A discarded draft
  is recreated by the next draw.
- **A bound entry is kept for the same name only** (case aside): another name is
  another member, or a request to make Carl was drawn over Elias.
- **`View.drawing`** (`cast::DRAWING`, in memory, a guard in `draw`): the editor
  waits ("Creating Elias, please wait…", `cewait`, re-read on `queueChanged`), and
  closing it never discards the draft under the draw. `discard`/`delete` wait up to
  5 s for a draw to end (the editor stops its own first), then refuse.
- **The agent never saves**; a try is a `Msg::Cast` (image `kind: "cast"`, a copy in
  `img/`) and the page opens the editor **bound** (`opts.chat`) on a try that arrives
  while the chat is open (`followCast`), never on coming back. Bound, the chat's words
  win over the entry's and the person's edits go back through `/chats/cast` (no
  `cast_rev` move, so no reopen over typing).
- **A change turn sees the newest try** when nothing is attached and the agent model
  has vision (`try_shown`, `Turn.cast_shown`), and the member's words in the system
  text.

Page: the editor holds `cur = {id, entry, pick, voice, chat}` and sends nothing but a draw
until Save; `pick` is `{try}`, `{output}`, `{outputs: {picture, sheet}}` or `null`
(keep), the wire shape of `cast::Pick`. Refine & adjust's *Save as cast…* (`#lbcast`) goes through
`/download-image` and `/upload`, so the entry copies an output in like any other.
