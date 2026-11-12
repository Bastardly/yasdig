---
paths:
  - "src/videos.rs"
  - "src/story.rs"
  - "src/narration.rs"
  - "src/voices.rs"
  - "src/avi.rs"
  - "src/h3prompt.rs"
  - "web/js/video.js"
  - "web/js/player.js"
  - "web/js/shots.js"
  - "web/js/storyboard.js"
  - "web/js/clipview.js"
  - "web/js/export.js"
  - "web/js/mp4mux.js"
  - "web/js/webmmux.js"
  - "web/js/gifenc.js"
  - "web/js/sounds.js"
  - "web/js/wavenc.js"
  - "web/js/lines.js"
---

# Video

## Video 2.0: Video › Scene › Shot › Part

**A shot is what a clip always was** (one model, parts going on from the last frame);
a **Video** (`src/story.rs`, `work/videos/{vid}/video.json`) orders **scenes** (header,
description = the old *Every shot*, negative, a `seed` its `every` rolls off —
`VideoRequest.every_seed`) of **shots**, each a clip folder at `{vid}/s{n}/` under the
id **`{vid}-s{n}`** (`story::shot_of`; `VideoStore::dir` maps it, so every
`/videos/{id}/…` route, Extend, Enlarge, plan and draft work on a shot unchanged).
Below, "clip" means a shot unless it says Video. A cut is the join between shots; parts
don't carry `cut` now (read for old records), except narrated stills, whose `sentRow`
still makes each part a cut from its own picture (step 6 makes it one picture per shot). Names in the UI: Storyboard, Shot
(the old Shots panel, rows labelled *Part k*), Scene.

- **Model, size, fps and Hi-res are the Video's** (the Generation panel), so shots
  join. `Video.lock` is taken by the first render (`take_lock`); `Engine::video`
  refuses a shot that differs (`check_lock`) until `POST /videos/story {unlock}` clears
  it. Steps/CFG/LoRAs/memory stay free. **Outdated is computed** (`shots.js`
  `shotOutdated`: recorded model/size/fps/hires vs the panel, `every`/negative vs the
  scene), never stored. A scene's words lock in the page only (`sceneLocked`).
- **A drawn shot drawn again renders under a new id** (`new_shot_id`) and
  `replace_shot` swaps it into the scene on success, so a Stop keeps the old one.
- **Routes**: `/videos/new {plan, title, scene}`, `/videos/shot {id, scene, at, plan}`,
  `/videos/shot/delete {id}`, `/videos/story {id, title, scenes, unlock}` (refused
  unless the shots named are exactly the Video's), all answering `{video}` with its
  shots' `clips`. `/videos` and `/favorite-clips` list Videos; star/delete act on the
  Video folder. Every record write is `update_video` under `RECORD`.
- **Old clips are converted at start** (`VideoStore::convert_old`): split at cut marks,
  frames renumbered (hard links), `audio.wav` split at `start × rate / fps`, drafts
  split by their rows' `cut`; built in `{id}.convert`, swapped by renames (`recover`
  finishes or undoes a crash). The Video keeps the old id.
- **Page**: `storyboard.js` (board panel, Scene panel, ↑↓ moves across scene edges;
  **the Shot, Scene and Generation panels have no rail button** — only the storyboard
  opens them (Generation by `#vopengen` under the title, beside the lock and Unlock),
  so an unsaved video still draws a placeholder Scene 1 · Shot 1 to click;
  **Advanced is a fold at the foot of Generation** (`#vfold-adv`, a `<details>`, no
  panel), and **Cast's rail button shows only on a model with `references`**
  (`syncCastRail`; nothing else uses it while films are off);
  hooks from `video.js`), `video.js` stages one shot (`stageShot`; `applySettings` only
  when a Video is opened from the grid). A new video is saved by a press
  (`ensureVideo`), never by typing. *Generate video*/*scene* queue every shot whose
  mark isn't done (`pressShot`: `/video` or `/videos/extend`) **one at a time** (`videoRun`: a
  dozen requests waiting at once would take every connection the poll and Stop need);
  the rail's Stop ends the run and stops every video job (`stopAllKind`). Drafts are `{rows}`.
- **The Assistant's films are paused** (`toolkit::FILMS_PAUSED`, `board.js`
  `FILMS_PAUSED`): its flat rows don't fit; it comes back on the storyboard.
- **The stage plays the Video whole** (`showVideo` → player.js `renderVideo`): every
  generated shot in storyboard order as one frame list (`parts`), the playhead at the
  picked shot, decoded frames cached per shot (`decoded`, keyed by id+created+frames),
  each shot's own `audio.wav` re-synced as the playhead crosses into it, a strip of
  shots under the scrubber (`#vshotmarks`). Same shots again = a seek; `seek: false`
  (a refresh after a render) leaves the playhead. The canvas opens the clip dialog on
  the shot under the playhead.
- **Not yet**: downloading/saving/enlarging the joined Video (each is per shot today), Enhance's neighbours across shots, narrated stills as one picture per shot.

## Clips (shots)

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
- **LTX-2.3 and 2.5 are two families** (`VideoFamily::Ltx23`/`Ltx25`, `is_ltx`;
  `discover::ltx`, ahead of every VAE arm: its audio VAE has the LDM layout, its
  video VAE Wan's, its Gemma 4 reads as a Qwen3). DiT: `adaln_single` +
  `audio_patchify_proj` (without the latter it is PixArt), 2.5 by
  `keyframes_abs_pos_embedding`. **VAEs say their release only in metadata**
  (`__metadata__.model_version`, `Tensors::version`); `None` fits both
  (`LtxVersion::takes`). 2.5's diffusion-decoder VAE (`decoder.diff_blocks`) is
  `Unsupported` naming the `-conv` file. 2.3's encoder is a stock Gemma 3 plus
  `--embeddings-connectors`; 2.5's is Lightricks' Gemma 4 *with*
  `text_embedding_projection` (`Gemma4Ltx`) — a stock Gemma 4 is `Unknown`.
  Frames `8k+1` down, sizes 32. **2.5's bf16 encoder ignores prompts**: it drew a
  man talking to camera where its q8_0 copy drew the prompt, same seed
  (2026-11-07). The card fetches Rootport's Q4_K_M (`LTX25_LLM.file`);
  Convert takes a bf16 through `-m` (`Slot::for_role`), and `video_components`
  prefers a GGUF beside it after the named file; `#vpartnote` says so while
  the bf16 is picked. LTX prompts go
  out in ASCII punctuation (`sd::ascii_punctuation`), a precaution only — it
  was blamed first, but the vocabularies agree on ’ and —. **The two upscalers are identical and unversioned**:
  `video_components` prefers the file named as the slot's `file`, and a card
  counts its upscaler as had only at its own dest (`downloads::present`).
- **LTX hi-res is in the one run** (`VideoModel.hires`, `--hires
  --hires-upscalers-dir <parent> --hires-upscaler <stem>`: sd-cli refuses a path
  or an extension there); `-W/-H` are the first pass's, the clip comes out ×2
  (`finish` reads it off the frames; `plan_clip` doubles it). **The distilled files
  are the catalog's** (`Catalog::video_distilled`, dest + length, in
  `video_models_untuned`): steps/CFG and `VIDEO_SIGMA_PRESETS` (`--sigmas` +
  `--hires-sigmas`). `--sigmas` goes only while `steps` is its length.
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
- **Shots queue**: Generate is never locked (`pending` counts requests in
  flight), each request waits on the job queue. Only
  a change to one clip is one at a time (`busyClips`: its footer and × hide), so a
  clip's rows are never sent twice.
- **Unsent edits are a per-clip draft on the server** (`setDraft`/`keepDraft` →
  `POST /videos/draft`, `draft.json` beside `clip.json` — never inside it, so a draft
  written mid-render can't race the worker's record rewrite): saved on the Shots
  panel's input (debounced), on switching clip or New clip, on `pagehide`
  (`keepalive`); restored by `applyClip` from `drafts` (the listing's, except ids this
  page wrote — `draftsMine`); dropped when the panel matches the clip (`matchesClip`,
  via `rowMatches`). A change landing off the stage rewrites that clip's draft to what
  was drawn plus the rows it didn't reach. Old `localStorage` `yasdig.clipdrafts` are
  moved up once (`adoptOldDrafts`).
- **A clip may be a plan** (`Clip.rendered == false`, `pipeline::plan_clip`,
  `POST /videos/plan`, on the connection thread): the rows as typed (no roll, no
  file checks), no frames, listed (`list` skips frameless dirs only when rendered).
  On the stage it is `plan` with player `clip` null, so `shotState` says `generate`;
  any tab change autosaves it; Generate sends its `id` (`VideoRequest.into`) and
  `prepare_plan`'s guard sweeps only what the run wrote (`sweep_render`), keeping the
  plan on failure. `save_plan` refuses over a rendered clip, under `RECORD`, the one
  lock on `clip.json` writes. Extend, Enlarge and Save to output refuse a plan.
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
  `POST /model-settings {kind: "video"}` (the Video tab's *Save as defaults* and
  Settings → Per model, which lists every video model); `reconcile` takes a video
  `exists` too.
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
- **Sounds are outputs**: `work/output/{stem}.wav` (`Store::import_audio`, from
  `POST /upload-audio`: 16-bit PCM only, the page decodes and resamples to 32 kHz
  stereo, `wavenc.js`; `find_audio_output`; listed `audio: true` + `seconds`;
  `free_stem`, `media`, delete and star take `.wav`). A shot's `audio` is one
  `--ref-audio` each, staged into scratch; `plan_shots` puts them where references
  may go, H3 with an audio VAE, ≤ `MAX_REF_AUDIO`; `Engine::check_sounds` the
  lengths (`REF_AUDIO_SECONDS`, MiniMax's: 2–15 s each, 15 s in all — sd.cpp checks
  none). **H3's fl2va and ref2va files have the same header and length** (checked
  2026-10-27), so both are one family and either is offered references and sounds;
  the catalog has a card for each.
- **An H3 shot with any `ShotWords` filled is sent in MiniMax's prompt layout**
  (`crate::h3prompt::compose`, from `plan_shots`; the guide is
  `skills/h3-prompt-writing/references/{base,ref}-en.txt` in
  github.com/MiniMax-AI/MiniMax-H3): `integrated_multimodal_description` /
  `overall_soundscape` / `non_diegetic_music` (`N/A` when empty), the I2VA
  first-frame line first when the shot has an `-i`, Ref2VA's six sections with each
  reference a `<Subject N>` (`subjects`, parallel to `references`; sd.cpp presents
  them as `<Picture N>`) and each sound an `<Audio N>` (`voices`, parallel to
  `audio`: `<Audio k> is the voice-timbre reference for <Subject j> (Sx).`). Speaker
  IDs by first line across the clip — `plan_shots` gets the kept `before` segments,
  so an Extend keeps `(S1)`. `<d>[Lang] …</d>` is ours (`clean_lines` strips a
  model's). **Silence is written too** (`with_lines`): the last on-screen speaker
  closes their lips, the shot's `cast` with no line stay silent, "Nothing more is
  said"; no lines is "No one speaks" — time left unaccounted for, H3 fills with
  invented speech. **All fields empty sends the text as typed**: wrapping prose in
  the layout would make its soundscape `N/A`, i.e. silence. `every` goes inside.
  H3's encoder takes the prompt whole (`prompt_attn_range {0,0}`), so `(S1)` and
  `[Shot 1]` survive. Page: the rows' *Who is in it* / *Spoken* / *Sound* / *Music*
  (`m.sounds`; *Who is in it* on any model while a storyboard is bound), names in the borrowed
  References/Sounds dialogs (`editRefs(…, names)`, `editSounds(…, voices)`).
- **References are H3's alone** (`VideoFamily::takes_references`,
  `references` on `/video-models`): sd.cpp refuses H3's Ref2VA beside a keyframe, so
  a shot with references has no `-i` (`VideoIo.refs` → one `-r` each, staged into
  scratch by `stage_shot_refs`, removed after). Shot 1 takes them only with no
  first frame. The page's ⓘ for it is the shared `vshotcut`.
- **A cut from a picture** (`ShotSpec.keyframe`/`Segment.keyframe`, an output
  stem; the row's `data-key`) is any `chains()` family's: `render_shot`'s `-i` is
  the keyframe instead of the last frame. Never on shot 1 (that is `source`), a
  continue, or beside references. **Shot 1's row holds the clip's first frame** in
  its own `data-key` (*Start from image*, no Generation-panel slot): `vidPayload`
  sends it as `source`, `rowsOf` puts `c.source` back, `rowMatches` takes it as
  the 4th argument. A cut with neither starts fresh from its words,
  on any family (as shot 1 with no first frame); a film's shot does when the
  person removes its first frame (`FilmShot.no_key`).
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
- **Narrated stills** (`VideoFamily::Narrated`, `src/narration.rs`, `src/voices.rs`):
  a shot is a picture held while a cloned voice reads its words. No sd-cli: the
  driver's **`"task": "speak"`** (`llm::SpeakRequest`, `LlmRunner::speak`, `PROTOCOL`
  2 — both ends; `Phase::Speaking`) runs Qwen3-TTS through mtmd's `gen_audio`, one
  WAV per piece. One model per `voices::scan` pair in `models/voices/` (projector
  `clip.has_gen_audio_encoder` beside any backbone — Chatterbox Turbo's is a plain
  `gpt2`; `TTS_ARCHES` only names TTS-only backbones for `agents::classify` to
  refuse), `[[voice]]` cards (`check_pairs`, `Want::Voice`; Chatterbox Turbo first —
  the user compared all three by ear 2026-11-09: Qwen3-TTS's llama.cpp clone is
  speaker-embedding only, timbre without delivery, and reads flat). **Languages are
  the file's** (`general.languages`, `GgufMeta.languages`; none listed =
  `narration::QWEN_LANGUAGES`), served per model, refused at speak time when the
  model doesn't list it (Turbo would read German as English); `#vlangpick` hides at
  one. **Sampling is the file's**: the driver sets llama-tts's TTS defaults (repeat
  penalty 1.05 over everything) and `common_init_from_params` lays
  `general.sampling.*` over whatever the request sent (no `user_sampling_config`
  bits) — Chatterbox's 1.2 penalty and top_k 1000 are what was heard. A piece with no
  speaker reads in the model's first predefined voice (Chatterbox needs one); pieces
  go out with `n_predict` `narration::MAX_PIECE_FRAMES` (the driver's 512 is 20 s at
  Chatterbox's 25 Hz). Multilingual's guidance sequence is llama.cpp's own
  (`classifier_free_guidance` doubles `n_seq_max` at init). `plan_narrated`: shot 1's
  picture is `source`, the rest `keyframe`, `cut` = k>0, ≤1 `audio`, `every`/negative
  unused, `frames` 0 until spoken. `Engine::narrate`: one speak run for every new
  shot (a shot's own `audio`, else `VideoModel.narrator`, the panel's *Default voice*
  — no carrying over from the shot before; `/videos/extend` sends the panel's
  `narrator`/`language` so a Redo reads as shown; pieces ≤300 chars at sentence ends,
  `narration::pieces`), `narration::lay` pads each shot to whole frames **measured
  from the clip's start** (rounding never accumulates), the still fitted *contain* on
  black as RGB (`fit_still`; canvas uploads are RGBA), frame 1 written, the rest
  **hard links** (`hold_frame`, copy fallback) — Enlarge's FSR path processes each
  segment's first frame and links the rest too. `chains()` is true (Extend/Redo
  work) but `VideoView` and skills leave it out of films and Enhance. Page:
  `narrateRow` relabels the row, `sentRow` with `take.narrated` sends `frames: null`
  (`rowMatches` skips it), `.unnarrated` + `#tab-video.narrated` hide the diffusion
  controls, `#vlang`; both players decode **one still per segment**
  (`decodeFrames`/`stillsOf` in dom.js) — 720 frames would be 720 decodes.
- **`frames_in` sorts by number**: `frame_%03d` is three digits, so frame 1000 sorted
  before 101 (`frames_are_ordered_by_number_past_a_thousand`).
