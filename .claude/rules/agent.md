---
paths:
  - "src/agent/**/*"
  - "src/llm.rs"
  - "src/agents.rs"
  - "llm/**/*"
  - "web/js/assistant.js"
  - "web/js/board.js"
  - "web/js/chatview.js"
  - "web/js/agentperms.js"
  - "web/js/enhance.js"
---

# The agent

## The agent

The Assistant (`src/agent/`, `src/llm.rs`, `src/agents.rs`, `llm/yasdig-llm.cpp`,
`web/js/assistant.js`): a chat with a local VLM, a panel in the tab it works on —
`mountAssistant` once on the Image tab, once on the Video tab (there is no Agent tab,
checked 2026-10-28: a chat's own copy of the tab's settings and board was what every
handoff lost things between). A turn is one run of our driver on the job queue; nothing
stays loaded.

**The driver, not llama.cpp's CLIs** (checked at v0.8.0; it also speaks narrated
stills, `video.md`): `llama-mtmd-cli` is one-shot
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
- **A turn's first try reserves `n_predict(mode)`, not the grammar's worst case**
  (`schema::max_tokens`: a chat draw can run to ~1,600 tokens, a 20-shot story past
  5,000): the driver refuses `prompt + n_predict > n_ctx` up front, so every token
  reserved is history lost. A reply that stops on `length` is run once more with
  `max_tokens` (capped at half the context, history refitted); Enhance re-asks once.
- **`Msg::Agent.did` is the reply's `action` word**, replayed in `context::messages`
  between `notes` and `reply` (the order a reply is written in). `action` is the id of
  an escalation, an older field.
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

- **A chat is one tab's** (`Chat.context: Pane`, `image` by serde default; `ChatStore::load`
  makes one with a film `video`). Each panel lists only its own (`/chats` carries
  `context`). Only a Video chat films (`toolkit::resolve` sets `film_off` on an Image one).
  **The person doesn't know our words**: a 4 s "clip", "shot" and "4 seconds of…" were
  each answered "I can't make video clips" (2026-11-14) — the old film line said so.
  `context::FILM_ASKS` makes anything that moves a film (ask only picture-or-video),
  the "film" reply is capped at `FILM_REPLY_CHARS` (Gemma wrote the whole board into an
  800-char reply), `general.md` says "image and video generator", and `film.md` keeps
  to the length asked. Three `agent-check` cases hold the three phrasings.
- **An Image chat's settings are the Image tab's controls**, both ways: the page writes
  them in (`/chats/new`/`/agent/send` `settings`, and `/chats/settings` debounced on any
  edit of the tab outside the panel, while the agent works too — the person's edit wins),
  through `Chat::take_settings`, which is no write when they are the same
  (`same_settings`: the page's `7` is the agent's `7.0`). The page puts the agent's writes
  back when `settings_rev` moves (`follows` → `applyAssistantSettings`, the `knobs::KNOBS`
  keys only, model first), and skips what it just sent itself. Rounds draw from noise
  (`assistantSettings` leaves *Image to image*'s source out), into `Slot::Image`,
  published as the latest (`agent::picture`), so they land on the stage like Generate's.
- **A chat's settings are a `/generate` body** (`GenerateDto`, `chat.settings` as JSON):
  a Video chat's (and an old one's) hold only what the agent set — `/chats/new` makes
  `{template}`, so a round (through `GenerateDto::run_plan` like the button) is that
  model's registry profile plus the agent's keys. `with_turbo` adds the catalog Turbo LoRA (steps/CFG/sigmas)
  when there is no `loras` key — `toolkit::locks_for` reads the same absence. A model
  switch (`knobs::write`) removes steps/CFG/size/sampler/LoRAs/parts, keeps the prompts.
- **The agent is shown every knob at the value the next round draws with**
  (`toolkit::shown`: the chat's, `with_turbo`'s, then the registry profile's). The
  chat's settings hold only what was set in it; shown as null, CFG was guessed at 7.
- **A session ends when stopped or overtaken** (`session::close_session`, from every
  stop path and `send`): a round cut short is closed at what it holds — left open, the
  next chat draw filled it at the session's size — and a comparison's knob goes back.
- **Board edits are `session::edit_board`** (tested); the route only keeps the picture,
  looks up the model and queues. Errors show in the Cast panel (`#casterr`).
- **The rules are Settings', read per turn** (`knobs::Rules`, `[agent]`; `toolkit::resolve`
  loads them): a whitelist of keys, no ask-first for a change, no bounds but `hard_bounds`. A chat
  carries none. An old `chat.json`'s `pending`/`awaiting` read as `rejected`/`idle` (serde
  aliases); an old `[agent]` of tables reads as the defaults (`key_list`).
- **One Send; the agent picks the action.** `/agent/send` is always a chat turn, whose
  schema branches are named for the request — `talk`, `draw`, `describe` (only when the
  turn shows pictures), `improve` (a `goal`) and `compare` (`vary`, `values`, `count`;
  absent when no `knobs::COMPARABLE` key is offered). `apply_reply` still reads the old
  `none`/`generate`/`session`. **A session always asks first**: an improve or a
  compare only stores `Session.proposal` (`Proposal {n, goal, sweep, per_round, rounds}`,
  drawn as a card under message `n`) and ends the leg; `POST /agent/session {start}` →
  `start_proposal` calls `session::begin` (goal, checks, rounds and sweep cleared,
  `first_round`, `reference` = the newest user message's pictures, `mode = Plan`), re-reads
  a sweep's `before` (the knob may have moved while it waited), `Next::Turn`, and queues;
  `decline_proposal` notes "Not started.". `close_session` drops it, so a new message does
  too. A compare's model switch is applied at once either way (undoable). `Mode::Describe` is never started; kept so old chats read.
- **A comparison is the server's, not the model's** (`Session.sweep: Sweep {knob, values,
  before}`): `sweep_from` checks every value through `knobs::check` (whitelist, locks,
  `hard_bounds`; strings parsed per knob — the grammar can't type array items) and needs
  two; `rounds` = values. `start_round` writes `values[k]` (`knobs::write`) and leads the
  round's summary with it; seeds are kept. In a sweep the plan turn writes checks only,
  a critique answers checks/keepers only, and after the last one `sweep_table` is posted
  as a note and `Mode::Verdict` (`best`) runs: `before` is written back, then `best` lands
  as an ordinary undoable change — so Undo returns to before the comparison.
- **A proposal is more than Start**: an improve carries the prompt its first round
  would draw (`Proposal.prompt`, the schema's `prompt` on that branch). Start sets it
  as an undoable change and marks `Session.redraw_first` (the plan turn then draws
  rather than judging the old prompt's round — `judgeable` is `None` while it is set);
  *Draw one* (`/agent/session {draw_one}` → `session::draw_proposal`) sets it and
  draws once as a chat draw, no session. The card's sizes (`rounds`, `count` →
  `start_proposal_sized`) set `Session.sized`, which the plan turn's own sizes never
  overwrite. The card's number boxes are the only thing typed into the log, so
  `draw()` in assistant.js carries their values across a redraw.
- **Again is the server's** (`POST /agent/again` → `session::draw_again`):
  `close_session`, then a chat draw on new seeds with no turn. The page offers it under
  the newest round only, never while the chat's job runs.
- **The agent is told the prompt variables** (`Toolkit.variables`, `serde(skip)`,
  from Settings per turn; `context::variables_text`, capped): names and a few words in
  Chat and Plan when it may write the prompt; a critique's pictures are labelled with
  what each rolled (`variables::picks`, off the Batch's seeds — the same maths as
  `expand`, pinned by a test). **A `{name}` the agent invents would fail the round**
  (unknown name → error), so `session::known_variables` → `variables::unbrace_unknown`
  strips its braces with a note, on a change's prompt/negative and a proposal's prompt.
- **`Session.reference` is staged first** in plan and critique turns (`Turn.reference`
  counts them), so "Picture 1" stays the round's first picture.
- **The agent sizes its work**: chat `count` (1 unless said), a plan's `rounds`/`count`
  (`Session.rounds`/`per_round`, 0 = `SESSION_ROUNDS`/`SESSION_BATCH`), a critique's
  `count`/`seeds`; `knobs::MAX_BATCH`/`MAX_ROUNDS` are the only caps.
- **`Slot::Agent`**: a film's board pictures and the passes on a keeper never take the
  Image tab's picture. `open_in` into `Slot::Agent` leaves a **live** entry where it is
  (it may be on the Image tab). A chat's picture clicked opens on the Image tab
  (`openWorkItem`).
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
- **Enhance** (`agent::enhance`, `POST /enhance`, `Work::Enhance`, `Kind::Enhance`,
  `web/js/enhance.js`): one schema-held call (`{prompt, sound?, spoken?}`) per box, with the
  family's `modes: [enhance]` guide (`skills::select_enhance`: `Skill.enhance` guides
  plus the family's mode-free style notes; `select` never gives a chat turn one). The
  page writes the answer with `insertText` (undo). H3 keeps names (its `<Subject N>`
  match) and its fields; `sound` and `spoken` (H3's Spoken lines — its prompt is what is seen, so a
  line the words asked for had nowhere else to go) each fill only an empty field. **A part sees its
  neighbours** (`enhance::part_neighbours` for a film, `shots.js` `partNeighbours` on
  the Video tab — **one rule, one fixture table pins both**
  (`the_fixture_table_the_page_checks_too`, `shots.test.js`): the scene's opening part,
  the shot's part 1, the part before — a shot's part 1 cuts from the shot before, "the
  same film", not "nothing carried over"), shed by `trimmed` to fit the
  context and dropped on a `ContextOverflow` retry; `then_continues` (the next shot goes
  on from this one) asks it to end with every face in view — a continue knows only the
  last frame, so a face that left it comes back a stranger's (film.md says the same); *Enhance every part*
  (`enhanceAll`) runs the rows in order. **A story's new or changed
  shots are enhanced after the turn** (`FilmShot.enhance` → `Next::Enhance` →
  `Step::Enhance`, `session::shot_enhanced`): the story's words move to
  `FilmShot.brief`, which `board_text`, `apply_story`'s match (`words()`) and
  `keyframe_prompt` use. `row_shot` keeps `brief` only while the part's text is the
  enhanced one; an unchanged shot is never re-enhanced (the person's edit stands). A
  failure clears every flag with a note; a Stop is a Stop.
- **A film** (`Chat.film: Film`, `session::apply_story`): a chat turn's `film` action
  hands over to a `Mode::Story` turn (its own schema, `n_predict` 3072): **scenes ›
  shots › parts** (`SceneJson`/`CameraJson`/`ShotJson`; caps `MAX_SCENES`,
  `MAX_SCENE_SHOTS`, `MAX_PARTS`, `MAX_FILM_SHOTS`). **`Film.shots` stays a flat list of
  parts**: `cut` starts a shot (`Film::groups`), `scene` indexes `Film.scenes` (each its
  own `every`: `scene_every`), `shot` is the Video shot's id, `drawn` that it is
  generated. **The server keeps the person's edits, not the model** (bug 4,
  2026-11-14: "shot 1" was answered as "Shot s2", and a part put first cost every later
  part its seed): `board_text` names each shot as the storyboard does ("Scene 1 · Shot 2
  (id s6)", `context::shot_number`), with its parts' lengths, H3's lines/sound/music,
  and **(the person's)** on a part `FilmShot.theirs` marks (`read_back` sets it on words
  the film didn't write; an empty shot the person added is theirs). A shot is `keep`
  (`context::short_id`: `s3`) or its parts; **a part may be `{"keep": true}`** (the next
  shown part not yet placed, whole), and a written part **matches by its words**
  (`context::same_words` against `shown_words`, one line, cut at `SHOWN_WORDS` 400 with
  "…" — a long hand-written part can be written back as shown), wherever it now stands:
  a match keeps its full text, brief, seed, picks, its length while `seconds` is the one
  shown, and lines/sound/music given back unchanged or not at all; a first frame stays
  only at the shot's first place. A scene matches by its `id` (`c1`), so its negative
  and seed stay. **Left out of a reply, only a plan the agent wrote and nobody touched
  goes** (`Film.drop`, said "Removed Scene 1 · Shot 3…"); a generated shot or one with
  a part of the person's stays (said); a part of theirs a rewritten shot dropped is
  named. Notes use the storyboard's numbers. **A part that names someone not in the part before starts
  a new shot** (`recut` against the story's own names, then a later part's `cast` is
  emptied): a part carries only the last frame, so they would be drawn from words.
  **Only a `character` does** — a place or thing coming into view goes on (a sausage
  typed `character` split a one-shot film, 2026-11-14; `film.md` now says what `kind`
  is). The sheet/front prompts say "character", not "person" (a dog was "the person").
  **Nothing is drawn unasked, and
  drawing is optional**: an undrawn member is words, an undrawn first frame a shot from
  words. *Draw the cast* (Cast footer) and *Draw the first frames* (the Shot footer,
  `#vframes`, on the film's Video) are `/chats/film {draw, frames}` → `board_press_for`;
  part 1's *Draw again* and *Draw a first frame* (`rowKeyHtml`, `{shot_id}` → the
  shot's first part) keep `board_press`'s automatic pick.
  **A cast press stops at the cast**: keyframes need a press of their own
  (`Film.frames`, set only when the cast is whole;
  `draw_step` reads it, `board_step` doesn't), so redrawing a member never redraws every
  frame they are in each time. **A keyframe records the cast pictures it was drawn
  with** (`FilmShot.key_cast`, `Film::cast_marks`); one that differs now is missing
  (`keyframe_missing`, `None` = an old board, never stale), so Film waits for it. **Board pictures run on `Film.image_model`**
  (`board_settings`: the chat's settings switched as a model change switches them; the
  agent's `image_model` pick, else the chat's). On one that `takes_references`
  (`Film.sheets`) a character gets a **turnaround sheet** (`CastSheet`, 2:1): a drawn
  one's sheet first and its front picture drawn from it, the person's picture first
  and the sheet from it — so both are one person. Keyframes reference sheets; video
  references stay the single front picture (`stem`). `Next::Film` then draws what is missing (`board_step`: the cast first, since keyframes use their pictures; a
  keyframe for each shot's part 1 unless `Film.references`, H3's, drawn on its scene's
  `every`), and
  `board_advance` sets `Phase::Ready` and ends the leg. **Every board picture is also an
  output** (`stem`, `key_stem`): a shot's references and first frames are output stems.
  The video model is `Chat.video`, the Video tab's as the person last sent. **The
  person picks it, never the agent** (asked for 2026-11-14; the silent swap to the first
  model that chains is gone). When the tab's model can't make a film (narrated stills,
  VACE, T2V, none) and some installed one can, `Toolkit.film_pick` lists them and the
  chat schema's "film" carries a `video_model` enum — the agent's *recommendation*
  (context says why: speech and sound → H3). The "film" then sets `Session.pick`
  (`film_pick`, a `VideoPick` under that message) and stops; the card's buttons post
  `/agent/film-model` (`pick_film_model`: `set_video`, a note, the story turn queued).
  Models in their usual order, the recommendation tagged in place
  (`recommend-without-reordering`); a new message drops it (`close_session`), as a
  proposal. **What a model can do is `VideoModel::features`** (`config::VideoFeatures`:
  `continues`, `references`, `sounds`, `audio`, `narrated`, `films`, `about`), the one
  source for `VideoView` and the page's `/video-models` `features` — the card's words are
  its `about`. `film_off` says why none can be made (`Rules.film`, no image model, the
  tab's model; Gemma once turned a bare refusal into "I only generate still images", so
  with a model to pick the agent is never told it can't). A new chat's settings have no `prompt` key, which `GenerateDto`
  requires: a board picture inserts its own, a round an empty one. A film's turns get the video family's skills too
  (`skills::select` takes both families' names, which never collide).
- **A chat's film writes its own Video** (`agent::filmvideo`, `Film.video`, read from
  an old chat's `clip`): the storyboard is the board — no copy on the page (a copy synced
  both ways lost every edit made while the other side was busy). **`push`** (`write`)
  after a story turn, an Enhance and every board picture (`step`), and after a
  `/chats/film` edit that draws nothing: a Video made when there is none (`create_video`),
  a scene per film scene (**a locked one — any generated shot — keeps its `every` and
  negative**: its shots were made with them), a plan per not-generated shot
  (`plan_clip` with `into`, carrying the shot's own `flow_shift_own`), a **draft** for a
  generated one whose rows differ from the film's — **every field** (`same_rows`), so a
  draft of the person's (a new length, a sound) is never dropped as "the same"; the
  draft keeps `shift`. A shot the film left out is deleted only when in `Film.drop` and
  still a plan; any other stays where it was (after the shot it followed), and a
  generated one is said — once: **`push` reads the Video back** (`pull`) after
  writing, so it is the film's again. New ids are `new_shot_id` (numeric; each plan is
  on disk before the next is drawn). **What a shot is written with**, weakest first:
  the model's defaults; the shot's own recorded settings (`recorded_settings`, its
  picked parts and memory too; a new shot takes the Video's first shot's); the page's
  Generation panel (`Chat::video_settings`, sent with `/chats/new`, `/agent/send`,
  `/chats/film`, `/chats/board` beside `video_id`) **only when sent from the film's own
  Video** (`routes::from_film_video`; from another Video it is cleared, never kept
  stale) and for the model the shots are made with; the lock's model/size/fps/Hi-res
  over all. **A plan that already says what the film says (rows, scene words, model)
  is not rewritten**, so a press never puts back what was set on it. The chat's model
  is the film's Video's lock when it has one (`toolkit::resolve`), and `/agent/send`
  changes `chat.video` only from the film's Video — another Video open on the tab is
  not the film's. A shot drawn again carries `Clip.replaces` (every id it stood for),
  which `read_back` matches parts through.
  **A Video chat with no film adopts the Video open when it sends** (`routes::adopt_video`:
  an empty `Ready` film naming it, on its lock's model): the turn's `pull` reads every
  shot back as the person's. Without it a hand-made Video was never shown to the
  agent ("change shot 2" → "please share the storyboard", 2026-11-14).
  **Written then read back, a film is itself** on Wan and on H3
  (`a_film_written_and_read_back_is_unchanged`): keep it so.
  **`pull`** (`read_back`) before every turn and every `/chats/film`: the film becomes
  the Video's scenes and shots in storyboard order, each shot's rows (`rows_of`: its
  draft's, else its parts with `source` as part 1's key) through `session::row_shot`,
  matched to the film's parts of the same shot id (or one it `replaces`) by words, else
  by place; a first frame the film
  didn't draw is the person's, copied into the chat (`keep_keys`). A film with no ids
  (an old chat's, `Film.clip` read as `video`) is matched by place. A Video whose
  `video.json` is gone clears `Film.video` and the ids and the next push makes
  another; one that won't load is an error, never a second Video (`gone`). **One writer at a
  time**: while the chat's job runs the film's Video is inert on the page
  (`holdVideo`: the Storyboard, Scene and Shot bodies and the Shot footer), and when
  it ends `board.js` `follow` restages it from disk (`followVideo`: the page's
  `draftsMine` for its shots forgotten, `stageShot(…, {keep: false})` so the stale
  panels aren't saved over the write), letting go of the hold only after. While held,
  `keepDraft`/`savePlan`/`setDraft` are no-ops (`holding`) — a Generation change
  autosaves the open plan otherwise. `follow` records the Video as seen only when
  the job has ended: the first film's Video is named mid-job and must still open. Before a send or a film press the open shot and
  a storyboard edit still waiting are saved (`flushBoard`: `saveStage` and
  `flushStory`, awaited). No `rev` on `video.json`: `update_video`'s
  `RECORD` lock, `/videos/story`'s shot-set check and the inert page cover it.
  `/chats/board {id, video}` is only `session::set_video` (`Film.references`, so what
  *Draw the first frames* draws follows the model), then a push. `FilmShot.seed`/`frames`
  are the part's, a story's `seconds` the first `frames`
  (`VideoView::frames_for`: never under `toolkit::action_seconds`, the words' floor,
  capped at `VideoView::longest`, Settings' `storyboard_shot_seconds` (5) on the nearest
  grid point — the tab's Frames is where a new row starts, 1.4 s on LTX, and once
  capped every shot) (a reworded part −1;
  `FilmShot`'s `Default` is by hand for that). The story schema has no `video_model`
  (the person's pick); `apply_story` keeps an unchanged part's seed, length and picks.
- **The agent never films** (checked 2026-10-28; it used to shoot and judge each take).
  **A film's parts are written for the film's model** (`shot_spec` on
  `Film.references`, rewritten when `set_video` moves it); with no film, *Add cast…* adds
  pictures and voices only on a model that follows them (`castOnRow`), the name
  elsewhere. **`shots.js` `sentRow` still picks what is sent** (references/sounds on
  H3, the image only beside neither; Wan the image), the guard for old drafts. `shotState`
  compares `sentRow` for the clip's own family (`clipTake`), so held references
  aren't a change on a Wan clip; `matchesClip` (the draft) compares the raw row. Nothing is queued. The board stays editable (`Phase` is
  `Board`/`Ready`; old `shooting`/`done` and `next: shoot|take` read through serde
  aliases; `Mode::Take` only so old messages read). The Lines/sound/music are plain
  fields; the H3 layout is the engine's (`video.md`). A board's `video_model` is
  the Video tab's (`set_video`) or the film-model card's.
- **No `data-help` on a `<button>`**: `wireHints` would nest a button in it.
