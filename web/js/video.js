import { $, esc, getJSON, post, numOr, rememberFolds, ICONS, randomSeed } from './dom.js';
import { floatPanels } from './panels.js';
import { MEMORY_LABELS, showError } from './memory.js';
import { cancelRun } from './busy.js';
import { soonJobs } from './jobs.js';
import { clip, renderClip, stopPlayback, clearClip } from './player.js';
import { initClipView, openClipView } from './clipview.js';
import { gridMarks, star, wireStars } from './gridmark.js';
import { shotState, rowMatches, sentRow, ALL } from './shots.js';
import { editRefs } from './refs.js';
import { editSounds } from './sounds.js';
import { wireHints } from './hints.js';
import { choosePicture, upload } from './source-slot.js';
import { linesText, parseLines } from './lines.js';
import { partRows } from './parts.js';
import { loraPicker, turboMemory } from './loras.js';
import { loadModelSettings } from './modelsettings.js';

// ---- Video ------------------------------------------------------------------
//
// A second workflow over the same GPU. Almost nothing is shared with the image
// side except setBusy and the progress poll, which is the point: a clip has
// frames, an fps and a flow shift, and no escalation ladder at all. What it does
// share is the worker, so a clip generating locks the image tab's buttons too —
// there is one GPU and one queue.
//
// The result of a generation is a directory of PNGs, not a container (see
// SdRunner::video for why). So playback is a canvas driven by a timer: every
// frame is fetched once, decoded once, and then the clip scrubs frame-exactly,
// which is what you want from two seconds of footage you are judging rather
// than watching.

let videoModels = [];
// Clip requests in flight — Generates and changes to a clip. Any number: the
// job queue runs them one after another, so Generate clip stays pressable and
// queues the next (New clip, then Generate, while another renders).
let pending = 0;
// Clips with a change in flight (Extend, Redo, ×): one at a time per clip, so
// their rows can't be sent twice. Other clips stay free to change.
const busyClips = new Set();
// Bumped whenever the stage changes hands — a clip opened, *New clip* — so a
// clip that finishes rendering lands on the stage (and in the rows) only if
// the stage is still the one it was started from.
let stageSeq = 0;

function currentVideoModel() {
  return videoModels.find((m) => m.name === $('vmodel').value);
}

// The model's separate files, the Image tab's rows from the same factory. An
// optional part (H3's audio VAE, VACE's CLIP-Vision) can be left out.
const videoParts = partRows({
  el: 'vpartrows', prefix: 'vpart', none: 'None', onChange: () => syncReady(),
});

// The clip's LoRAs: the Image tab's picker, offered what fits this video
// family (`/loras` lists video families too).
const videoLoras = loraPicker({
  rows: 'vloras', add: 'vloraadd',
  family: () => currentVideoModel()?.family ?? null,
  experts: () => !!currentVideoModel()?.is_moe,
  onChange: () => {
    syncVTurboBox();
    syncVideoSummaries();
  },
});

// ---- Turbo -------------------------------------------------------------------
//
// The Image tab's switch (see image.js) over this picker: H3's DMD LoRA. On
// picks it and sets the steps and CFG it was distilled for; off puts the
// model's back. Default on, per model in this browser.
let vTurboSeq = 0;
const vTurboMem = turboMemory('vturbo');
const vTurboOf = (m) => (m ? videoLoras.turboFor(m.family) : undefined);
const vTurboOn = () => !$('vturborow').hidden && $('vturbo').checked;

// Reflects, never applies, as on the Image tab.
function syncVTurboBox() {
  const t = vTurboOf(currentVideoModel());
  $('vturborow').hidden = !t;
  $('vturbo').checked = !!t && videoLoras.has(t.name);
  if (t) $('vturbolabel').textContent = `Turbo — ${t.turbo.steps} steps at CFG ${t.turbo.cfg_scale}`;
}

function setVBox(id, v) {
  $(id).value = v;
  $(id).dispatchEvent(new Event('input', { bubbles: true }));
}

function setVTurbo(on) {
  const m = currentVideoModel();
  const t = vTurboOf(m);
  if (!t) return;
  if (on) {
    videoLoras.add(t.name);
    setVBox('vsteps', t.turbo.steps);
    setVBox('vcfg', t.turbo.cfg_scale);
  } else {
    videoLoras.remove(t.name);
    setVBox('vsteps', m.steps);
    setVBox('vcfg', m.cfg_scale);
  }
}

// ---- Shots -------------------------------------------------------------------
//
// A clip is its shots, first to last: one sd-cli run each (`Engine::video`),
// each but the first going on from the last one's final frame — or a cut,
// which starts from an image (`data-key`, any family that takes a first
// frame) or fresh from its words — on H3 maybe following reference pictures
// and sounds. Shot 1's image is the clip's first frame (`source`). Each row is
// conditioned on its own words with *Every shot* in front, never another
// row's, or a line spoken in shot 1 is spoken again in every shot. On a model
// with sound a row also says who is in it, what is said, heard and scored,
// which the server writes into H3's prompt layout (`h3prompt.rs`). A row
// holds its own state in the DOM: the boxes, and `data-cut`/`data-refs`/
// `data-subjects`/`data-audio`/`data-voices`/`data-key`. The marks are
// `syncShots`'s, against the clip on the stage (`shots.js`).

const rowEls = () => [...$('vshots').querySelectorAll('.shotrow')];

function readRow(el) {
  const seed = parseInt(el.querySelector('.shotseed').value, 10);
  const frames = parseInt(el.querySelector('.shotframes').value, 10);
  return {
    text: el.querySelector('textarea').value,
    seed: Number.isFinite(seed) ? seed : -1,
    frames: Number.isFinite(frames) ? frames : null,
    cut: el.dataset.cut === '1',
    refs: JSON.parse(el.dataset.refs || '[]'),
    audio: JSON.parse(el.dataset.audio || '[]'),
    key: el.dataset.key || null,
    folded: el.classList.contains('folded'),
    cast: el.querySelector('.shotcast').value.split(',').map((x) => x.trim()).filter(Boolean),
    lines: parseLines(el.querySelector('.shotlines').value),
    sound: el.querySelector('.shotsnd').value.trim(),
    music: el.querySelector('.shotmusic').value.trim(),
    subjects: JSON.parse(el.dataset.subjects || '[]'),
    voices: JSON.parse(el.dataset.voices || '[]'),
  };
}

// The rows that are shots — a blank one is not — each with its row index,
// its frames on the grid the server will round them to.
function liveRows() {
  const m = currentVideoModel();
  return rowEls().map((el, i) => {
    const r = readRow(el);
    if (m && r.frames != null) r.frames = alignFrames(r.frames, m.frame_grid);
    return { ...r, i };
  }).filter((r) => r.text.trim());
}

// A drawn clip's shots as rows: the seeds they were drawn with, so Generate
// makes them again unless a seed is reset. Shot 1's image is the clip's.
const rowsOf = (c) => (c.segments ?? []).map((s, k) => ({
  text: s.text, seed: s.seed, frames: s.frames, cut: !!s.cut, refs: s.references ?? [], audio: s.audio ?? [],
  key: (k === 0 ? c.source : s.keyframe) ?? null,
  ...wordsOf(s),
}));

// What a shot says, hears and is scored with, and who is in it, the
// references and the sounds: as a segment records it, a row reads it and the
// wire sends it.
const wordsOf = (s) => ({
  cast: s.cast ?? [], lines: s.lines ?? [], sound: s.sound ?? '', music: s.music ?? '',
  subjects: s.subjects ?? [], voices: s.voices ?? [],
});

// What a model takes of a row (`sentRow`), from its `/video-models` entry.
const takeOf = (m) => ({ references: !!m?.references, sounds: !!m?.sounds });

// The wire's shape for a row, what a model that takes `take` takes of it; a
// blank Frames box is the model's. Shot 1's image goes as the clip's `source`.
const shotWire = (row, k, take) => {
  const r = sentRow(row, k, take);
  return {
    prompt: r.text, seed: r.seed, frames: r.frames, cut: r.cut, references: r.refs, audio: r.audio,
    keyframe: k > 0 && r.cut ? r.key : null,
    ...wordsOf(r),
  };
};

// Whether a clip's own family can go on from its last frame — by the clip's
// family, since the model it was made with may be listed under another name
// now (or be a [[video]] entry since removed).
const chains = (c) => !!c && videoModels.some((m) => m.family === c.video?.family && m.chains);

// What the clip's own family takes, which is what its shots were sent; with
// none of it installed now, everything (a held reference then reads as a
// change, never as drawn).
const clipTake = (c) => {
  const m = videoModels.find((v) => v.family === c?.video?.family);
  return m ? takeOf(m) : ALL;
};

// The rows against the clip on the stage: which are done, what a press draws.
const shotsNow = () => shotState(liveRows(), clip, chains(clip), clipTake(clip));

// A row folds to its heading line, a panel's way: the fold is the row's own
// (`folded` on a row read back), else what row k was before the redraw.
function drawShots(rows) {
  const was = rowEls().map((el) => el.classList.contains('folded'));
  $('vshots').innerHTML = rows.map((r, k) => {
    const folded = r.folded ?? was[k] ?? false;
    return `
    <div class="shotrow${folded ? ' folded' : ''}" data-cut="${r.cut ? 1 : 0}" data-refs="${esc(JSON.stringify(r.refs ?? []))}"
      data-subjects="${esc(JSON.stringify(r.subjects ?? []))}" data-audio="${esc(JSON.stringify(r.audio ?? []))}"
      data-voices="${esc(JSON.stringify(r.voices ?? []))}" data-key="${esc(r.key ?? '')}">
      <div class="shothead">
        <button type="button" class="shotfold" aria-expanded="${!folded}" title="Fold or unfold this shot">
          <b>Shot ${k + 1}</b></button>
        <span class="shotsum"></span>
        <span class="shotstart" hidden>
          <label data-help="vshotcut">Starts</label>
          <button type="button" class="btn btn-choice" data-start="0">Continue</button>
          <button type="button" class="btn btn-choice" data-start="1">Cut</button>
        </span>
        <button type="button" class="fpclose shotx" aria-label="Remove shot ${k + 1}">&times;</button>
      </div>
      <div class="shotbody">
        <label data-help="vshotseed">Seed</label>
        <div class="withbtn">
          <input class="shotseed" type="number" value="${r.seed ?? -1}" aria-label="Shot ${k + 1} seed">
          <button type="button" class="btn btn-icon shotreseed" title="Back to -1: a new seed each time"
            aria-label="Reset shot ${k + 1} seed to -1">${ICONS.reset}</button>
          <button type="button" class="btn btn-icon shotroll" title="A random seed, kept"
            aria-label="Random seed for shot ${k + 1}">${ICONS.dice}</button>
        </div>
        <label data-help="vshotframes">Frames</label>
        <input class="shotframes" type="number" value="${r.frames ?? ''}" aria-label="Shot ${k + 1} frames">
        <label data-help="vprompt">Prompt</label>
        <prompt-box><textarea class="short" aria-label="Shot ${k + 1} prompt"
          placeholder="${k ? 'what happens next: an action, where the camera is' : 'the crab lifts a claw, slow camera push in'}">${esc(r.text ?? '')}</textarea></prompt-box>
        <div class="shotsoundrows" hidden>
          <label data-help="vshotcast">Who is in it</label>
          <input type="text" class="shotcast" value="${esc((r.cast ?? []).join(', '))}" placeholder="Mara, Leo"
            aria-label="Who is in shot ${k + 1}">
          <label data-help="vshotlines">Spoken</label>
          <textarea class="short shotlines" aria-label="Spoken in shot ${k + 1}"
            placeholder="One line each — Mara: Who's there?  Leo (voiceover): I never left.">${esc(linesText(r.lines ?? []))}</textarea>
          <label data-help="vshotsound">Sound</label>
          <input type="text" class="shotsnd" value="${esc(r.sound ?? '')}" placeholder="rain on the roof, footsteps — empty is silence"
            aria-label="Sound in shot ${k + 1}">
          <label data-help="vshotmusic">Music</label>
          <input type="text" class="shotmusic" value="${esc(r.music ?? '')}" placeholder="only the audience hears it — empty is none"
            aria-label="Music in shot ${k + 1}">
        </div>
        <div class="btnrow shotfrom">
          <button type="button" class="btn shotkey" hidden title="The image this shot starts from">Start from image…</button>
          <button type="button" class="btn shotkeyup" hidden title="An image from this computer">Upload…</button>
          <img class="shotkeythumb" alt="" hidden>
          <button type="button" class="btn shotkeyclear" hidden>Clear</button>
          <button type="button" class="btn shotrefs" hidden>References…</button>
          <button type="button" class="btn shotsounds" hidden>Sounds…</button>
        </div>
        <p class="sub shotwhy" hidden>A continue goes on from the last frame; references and sounds go on a cut.</p>
      </div>
    </div>`;
  }).join('');
  wireHints($('vshots'));
  frameSteps();
  syncShots();
}

// ---- Unsent edits, per clip ----------------------------------------------------
//
// Rows added or changed on a clip and not drawn yet are the clip's draft: kept
// when another clip is opened and put back when it is opened again — and in
// this browser's storage, so a reload keeps them too. Only what differs from
// the clip is kept; a draft that matches it is dropped. Per browser on purpose:
// it is a convenience over the record, never part of it.
const DRAFTS_KEY = 'yasdig.clipdrafts';
const MAX_DRAFTS = 200;
let drafts = null;
let draftTimer = null;

function draftMap() {
  if (!drafts) {
    try {
      drafts = new Map(Object.entries(JSON.parse(localStorage.getItem(DRAFTS_KEY) || '{}')));
    } catch {
      drafts = new Map();
    }
  }
  return drafts;
}

function storeDrafts() {
  const map = draftMap();
  // Oldest first, as inserted: a long-forgotten clip's edits go first.
  while (map.size > MAX_DRAFTS) map.delete(map.keys().next().value);
  try { localStorage.setItem(DRAFTS_KEY, JSON.stringify(Object.fromEntries(map))); } catch { /* a convenience */ }
}

// Whether the panel holds just what clip `c` is: its shots, its Every shot,
// its negative. Blank rows are not shots.
function matchesClip(c, rows, every, negative) {
  const segs = c.segments ?? [];
  return rows.length === segs.length
    && rows.every((r, k) => rowMatches(r, segs[k], k, c.source))
    && every.trim() === (c.every ?? '').trim()
    && negative === (c.typed_negative ?? c.negative ?? '');
}

// Record the panel as clip `c`'s draft, or drop the draft when there is
// nothing unsent. `rows` defaults to the panel's.
function setDraft(c, rows = rowEls().map(readRow), every = $('veverytext').value, negative = $('vnegative').value) {
  clearTimeout(draftTimer);
  const map = draftMap();
  const m = currentVideoModel();
  const live = rows.filter((r) => String(r.text ?? '').trim())
    .map((r) => ({ ...r, frames: m && r.frames != null ? alignFrames(r.frames, m.frame_grid) : r.frames }));
  map.delete(c.id);
  if (!matchesClip(c, live, every, negative)) map.set(c.id, { rows, every, negative });
  storeDrafts();
}

const keepDraft = () => { if (clip) setDraft(clip); };

// Typing saves a moment later, so a reload loses at most that moment.
function keepDraftSoon() {
  clearTimeout(draftTimer);
  draftTimer = setTimeout(keepDraft, 400);
}

// A row for + Add shot: the seed and length of the row above it, so a new
// shot starts as its neighbour was drawn (the dice or Reset vary it).
function addShot() {
  const els = rowEls();
  const last = els.length ? readRow(els.at(-1)) : null;
  drawShots([...rowEls().map(readRow), {
    text: '', seed: last?.seed ?? -1, frames: last?.frames ?? currentVideoModel()?.frames ?? null,
  }]);
  rowEls().at(-1)?.querySelector('textarea').focus();
  syncVideoSummaries();
}

// The clip's length as the rows say it: a shot that goes on from the last
// one adds one frame fewer (the seam is its first frame, the last one's
// final frame).
function rowsSeconds(m) {
  const fps = m.forced_fps ?? (parseInt($('vfps').value, 10) || 1);
  let frames = 0;
  for (const [j, r] of liveRows().entries()) {
    const f = r.frames ?? alignFrames(m.frames, m.frame_grid);
    frames += j > 0 && !r.cut ? f - 1 : f;
  }
  return frames / fps;
}

// The marks, each row's start switch, references button and ×, the footer's
// press, and what the shots add up to.
function syncShots() {
  const m = currentVideoModel();
  const live = liveRows();
  const st = shotState(live, clip, chains(clip), clipTake(clip));
  const doneRows = new Set(live.slice(0, st.done).map((r) => r.i));
  const lastDone = st.dropLast >= 0 ? live[st.dropLast]?.i : -1;
  const els = rowEls();
  const refsOk = !!m?.references;
  const keyOk = !!m?.chains;
  // A change to this clip is in flight: nothing more on it until it lands.
  const busy = !!clip && busyClips.has(clip.id);
  els.forEach((el, i) => {
    el.classList.toggle('done', doneRows.has(i));
    // What a folded row still says: the start of its prompt.
    el.querySelector('.shotsum').textContent = el.querySelector('textarea').value.trim();
    const cut = el.dataset.cut === '1' && i > 0;
    const start = el.querySelector('.shotstart');
    start.hidden = i === 0 || !keyOk;
    for (const b of start.querySelectorAll('[data-start]')) b.classList.toggle('active', (b.dataset.start === '1') === cut);
    // References on a cut, or on shot 1, never beside its image: H3 can't
    // follow them and go on from a frame at once. A row may hold both (a
    // storyboard's does): on H3 its references win and the image waits in
    // `data-key` for Wan, which in turn holds the references unseen
    // (`sentRow` sends what the model takes).
    const refs = el.querySelector('.shotrefs');
    const n = JSON.parse(el.dataset.refs || '[]').length;
    const a = JSON.parse(el.dataset.audio || '[]').length;
    const key = el.dataset.key || '';
    const followed = (refsOk ? n : 0) + (m?.sounds ? a : 0);
    // Shot 1 and a cut start fresh: from an image, or on H3 from references.
    const fresh = i === 0 || cut;
    refs.hidden = !refsOk || !fresh || (!!key && !followed);
    refs.textContent = n ? `References (${n})` : 'References…';
    // Sounds go where references go, on an H3 with its audio VAE.
    const sounds = el.querySelector('.shotsounds');
    sounds.hidden = !m?.sounds || !fresh || (!!key && !followed);
    sounds.textContent = a ? `Sounds (${a})` : 'Sounds…';
    el.querySelector('.shotwhy').hidden = !refsOk || fresh;
    el.querySelector('.shotsoundrows').hidden = !m?.sounds;
    // Shot 1's image is the clip's first frame; a cut's is how Wan cuts.
    const keyBtn = el.querySelector('.shotkey');
    keyBtn.hidden = !fresh || (i > 0 && !keyOk) || followed > 0;
    keyBtn.textContent = key ? 'Another image…' : 'Start from image…';
    el.querySelector('.shotkeyup').hidden = keyBtn.hidden;
    const thumb = el.querySelector('.shotkeythumb');
    thumb.hidden = keyBtn.hidden || !key;
    if (key) thumb.src = `/outputs/${encodeURIComponent(key)}/thumb.png`;
    el.querySelector('.shotkeyclear').hidden = thumb.hidden;
    // A done shot stays: the shots after it start from its frames. Only the
    // clip's last one can be cut off, and the cut leaves its text in the row.
    const x = el.querySelector('.shotx');
    x.hidden = (doneRows.has(i) && (i !== lastDone || busy)) || els.length === 1;
    x.title = i === lastDone ? 'Cut this shot off the clip — its text stays, to draw it again' : 'Remove this shot';
  });
  const foot = st.action === 'extend' || st.action === 'redo';
  $('vextendfoot').hidden = !foot || busy;
  const n = st.pending.length;
  $('vextend').textContent = st.action === 'redo'
    ? (n ? `Redo from shot ${st.done + 1}` : `Cut from shot ${st.done + 1}`)
    : `Extend clip — ${n} shot${n === 1 ? '' : 's'}`;
  $('vextend').title = st.action === 'redo'
    ? `Keep shots 1–${st.done} of the clip on screen and draw the rest again, from shot ${st.done}’s last frame`
    : 'Draw the new shots onto the clip on screen, from its last frame';
  const note = $('vshotnote');
  note.classList.remove('err');
  if (!m) { note.textContent = ''; return; }
  if (busy) {
    note.textContent = 'A change to this clip is queued or drawing; its shots can be changed again when it lands.';
  } else if (live.length > 1 && !m.chains) {
    note.textContent = `${m.name} can't go on from a clip's last frame, so it makes one clip at a time — `
      + 'TI2V-5B, A14B and H3 can.';
    note.classList.add('err');
  } else if (st.action === 'redo') {
    note.textContent = `Shot ${st.done + 1} differs from the clip on screen. ${$('vextend').textContent} keeps the `
      + `${st.done} before it, on the clip's own model and settings; Generate clip makes every row again.`;
  } else if (st.action === 'extend') {
    note.textContent = `Extend clip draws ${n} more onto the clip on screen, from its last frame, `
      + 'with its own model and settings.';
  } else {
    note.textContent = `${live.length || 1} shot${live.length > 1 ? 's' : ''} ≈ ${rowsSeconds(m).toFixed(1)} s`;
  }
}

// The model's missing files, less any a pick replaced: a Wan VAE that only
// exists under models/vae/ is a missing part until it is picked in its row.
function missingNow(m) {
  const swapped = new Set();
  for (const s of videoParts.selects()) {
    const p = m.parts.find((x) => x.slot === s.dataset.slot);
    if (p?.current_path && s.value !== (p.current ?? '')) swapped.add(p.current_path);
  }
  return m.missing.filter((f) => !swapped.has(f));
}

// A video model is several files and tens of gigabytes, so "not downloaded
// yet" is a normal state to be in for an hour. Say which file is missing.
function syncReady() {
  const m = currentVideoModel();
  if (!m) return;
  const missing = missingNow(m);
  $('vmodelnote').textContent = missing.length
    ? `Not ready — missing ${missing.length} file(s): ${missing.join(', ')}`
    : (m.description || '');
  $('vgo').disabled = !!missing.length;
  // Pressed again while one runs, it queues another.
  $('vgo').textContent = pending ? 'Queue another clip' : 'Generate clip';
}

// Which rows hold something other than what discovery picked — said in the
// Generation line, since a clip made with another VAE looks different.
function partsDeparted() {
  const m = currentVideoModel();
  return videoParts.selects().filter((s) => {
    const p = m?.parts.find((x) => x.slot === s.dataset.slot);
    return p && (s.value || null) !== (p.auto ?? null);
  }).map((s) => m.parts.find((x) => x.slot === s.dataset.slot).label);
}

// Each fold's summary says what is inside, the Image tab's rule, so a folded
// section still reads. Run from a delegated listener on the tab, and after
// anything that sets the boxes without an event (`syncVideo`, `applyClip`).
function syncVideoSummaries() {
  const live = liveRows();
  const p = live[0]?.text.trim() ?? '';
  const m = currentVideoModel();
  $('vsum-prompt').textContent = (live.length > 1 ? `— ${live.length} shots` : '— 1 shot')
    + (m ? ` · ${rowsSeconds(m).toFixed(1)} s` : '')
    + (p ? ` · ${p.length > 60 ? p.slice(0, 60) + '…' : p}` : '');
  // A long clip is a departure from one clip a Generate: the rail's dot says so.
  $('vsum-prompt').classList.toggle('on', live.length > 1);
  const name = $('vmodel').value;
  const picked = partsDeparted();
  const loras = videoLoras.get();
  $('vsum-gen').textContent = name
    ? `— ${name} · ${$('vw').value}×${$('vh').value} · ${$('vsteps').value} steps`
      + (picked.length ? ` · own ${picked.join(', ')}` : '')
      + (loras.length ? ` · LoRA: ${loras.map((l) => l.name.split('/').pop().replace(/\.[^.]+$/, '')).join(', ')}` : '') : '';
  $('vsum-gen').classList.toggle('on', picked.length > 0 || loras.length > 0);
  $('vsum-adv').textContent = `— Memory: ${MEMORY_LABELS[$('vmemory').value] ?? $('vmemory').value}`
    + ($('vtiling').checked ? '' : ' · VAE tiling off');
}

// Round the way the server will. Both rules come from /video-models rather than
// being written out here, because `video_cmd` rounds the same numbers to run the
// model and the duration below is only honest if the two agree exactly.
const alignSize = (v, mult) => Math.max(mult, Math.ceil((v || 0) / mult) * mult);

function alignFrames(v, g) {
  const f = Math.max(v || 0, g.min);
  if (g.round_up) return f + (((g.offset - f) % g.multiple) + g.multiple) % g.multiple;
  return Math.max(g.min, Math.floor((f - g.offset) / g.multiple) * g.multiple + g.offset);
}

// A size box, snapped on `change` the way the server rounds it — up, to the
// model's multiple — so the boxes show what will run rather than a note
// beside them explaining the difference.
function snapSize(id) {
  const m = currentVideoModel();
  const v = parseInt($(id).value, 10);
  if (!m || !Number.isFinite(v)) return;
  const snapped = alignSize(v, m.size_multiple);
  if (snapped !== v) $(id).value = snapped;
}

// What the boxes add up to, before anything runs: the size after rounding,
// the fps the model will actually use, and the frame grid each shot's length
// is rounded to. The size is snapped in its boxes; a half-typed one is still
// rounded here.
function syncVideoNote() {
  const m = currentVideoModel();
  if (!m) { $('vgridnote').textContent = ''; return; }
  const w = alignSize(parseInt($('vw').value, 10), m.size_multiple);
  const h = alignSize(parseInt($('vh').value, 10), m.size_multiple);
  const fps = m.forced_fps ?? (parseInt($('vfps').value, 10) || 1);
  $('vgridnote').textContent = `${w}×${h} @ ${fps} fps`
    + (m.forced_fps ? ' — this model overrides anything else' : '')
    + ` · a shot's frames are ${m.frame_grid.multiple}k+${m.frame_grid.offset}`;
}

// Every row's Frames on the model's grid, a blank one at the model's length —
// after a model change (H3's grid is not Wan's) or a typed number.
function snapShotFrames() {
  const m = currentVideoModel();
  if (!m) return;
  for (const box of $('vshots').querySelectorAll('.shotframes')) {
    const v = parseInt(box.value, 10);
    box.value = alignFrames(Number.isFinite(v) ? v : m.frames, m.frame_grid);
  }
  frameSteps();
}

// The arrows step from grid point to grid point. A number box steps from its
// `min`, so `min` must be on the grid too: with H3's 17k+5 and a min of 1, a
// step down from 39 landed on 35, which the snap rounds back *up* to 39 — the
// down arrow did nothing.
function frameSteps() {
  const g = currentVideoModel()?.frame_grid;
  if (!g) return;
  const min = g.offset >= g.min ? g.offset : g.offset + Math.ceil((g.min - g.offset) / g.multiple) * g.multiple;
  for (const box of $('vshots').querySelectorAll('.shotframes')) {
    box.min = min;
    box.step = g.multiple;
  }
}

// Pre-fill from the selected model, the same job syncAdvanced does for an image.
function syncVideo() {
  const m = currentVideoModel();
  if (!m) return;
  $('vnegative').value = m.negative_prompt ?? '';
  videoParts.draw(m.parts);
  // Re-greys the LoRA rows for this family (and shows the expert choice on
  // the pair), then sets Turbo to this model's default. After the boxes
  // below are filled, since it overwrites steps and CFG.
  const seq = ++vTurboSeq;
  videoLoras.refresh().then(() => {
    if (seq !== vTurboSeq || currentVideoModel() !== m) return;
    videoLoras.dropTurbos(m.family);
    if (vTurboOf(m) && vTurboMem.wanted(m.name)) setVTurbo(true);
    syncVTurboBox();
  });
  $('vw').value = m.width;
  $('vh').value = m.height;
  $('vw').step = m.size_multiple;
  $('vh').step = m.size_multiple;
  snapShotFrames();
  $('vfps').value = m.forced_fps ?? m.fps;
  $('vfps').disabled = !!m.forced_fps;
  $('vsteps').value = m.steps;
  $('vcfg').value = m.cfg_scale;
  $('vsampler').value = m.sampler;
  $('vshift').value = m.flow_shift ?? '';
  $('vtiling').checked = !!m.vae_tiling;
  $('vmemory').value = m.memory_mode ?? 'ram';
  // The second expert's knobs only exist for the pair.
  $('vmoe').hidden = !m.is_moe;
  $('vhsteps').value = m.high_noise_steps ?? '';
  // Blank rather than pre-filled, for the reason the image tab's CFG box gives:
  // a number here pins it and quietly stops it following the CFG above.
  $('vhcfg').value = m.high_noise_cfg_scale ?? '';
  $('vhcfg').placeholder = m.cfg_scale;
  $('vclearmodel').hidden = !m.tuned;
  syncReady();
  syncVideoNote();
  syncShots();
  syncVideoSummaries();
}

// The boxes *Save as defaults* writes, as [wire key, box, kind]: what
// `settings::VIDEO_KNOBS` takes. A blank number box with a meaning — shift and
// the high-noise CFG follow the model when blank — is `null`, which forgets it.
const VIDEO_KNOBS = [
  ['width', 'vw'], ['height', 'vh'], ['frames', null, 'shot1'], ['fps', 'vfps'],
  ['steps', 'vsteps'], ['cfg_scale', 'vcfg'], ['sampler', 'vsampler', 'text'],
  ['flow_shift', 'vshift', 'blank'], ['high_noise_steps', 'vhsteps', 'moe'],
  ['high_noise_cfg_scale', 'vhcfg', 'moe'], ['vae_tiling', 'vtiling', 'check'],
  ['memory', 'vmemory', 'text'], ['negative_prompt', 'vnegative', 'text'],
];

async function saveVideoDefaults(clear) {
  const m = currentVideoModel();
  if (!m) return;
  const values = {};
  for (const [key, id, kind] of VIDEO_KNOBS) {
    if (clear) { values[key] = null; continue; }
    if (kind === 'moe' && !m.is_moe) continue;
    // Turbo's steps and CFG are its LoRA's: saved, unticking would put them back.
    if ((key === 'steps' || key === 'cfg_scale') && vTurboOn() && !clear) continue;
    // A new row's length: the first shot's.
    if (kind === 'shot1') {
      const f = rowEls()[0] && readRow(rowEls()[0]).frames;
      if (f) values[key] = f;
      continue;
    }
    if (kind === 'check') { values[key] = $(id).checked; continue; }
    if (kind === 'text') { values[key] = $(id).value; continue; }
    if (kind === 'blank' && $(id).value.trim() === '') { values[key] = null; continue; }
    const n = parseFloat($(id).value);
    if (Number.isFinite(n)) values[key] = n;
  }
  // The rows too; the server keeps only a file that isn't discovery's pick.
  const parts = clear
    ? Object.fromEntries(m.parts.map((p) => [p.slot, null]))
    : videoParts.picked();
  $('vsavemodelmsg').textContent = 'Saving…';
  try {
    const data = await post('/model-settings', { kind: 'video', model: m.name, values, parts });
    $('vsavemodelmsg').textContent = clear
      ? `Cleared — ${m.name} starts from its own defaults again.`
      : data.tuned ? `Saved — ${m.name} now opens with these.`
        : 'Nothing to save: every box is where this model starts anyway.';
    // Clearing puts the boxes back where the model starts; a save leaves them.
    await loadVideoModels({ refill: clear });
    // Settings → Per model lists what each video model keeps.
    loadModelSettings();
  } catch (e) {
    $('vsavemodelmsg').textContent = String(e.message || e);
  }
}

// `refill: false` keeps the boxes as they are unless the selected model itself
// changed — the Image tab's rule: a download landing in the background must not
// reset a clip being set up.
export async function loadVideoModels({ refill = true } = {}) {
  const before = $('vmodel').value;
  try {
    const data = await getJSON('/video-models');
    if (!Array.isArray(data)) throw new Error('unexpected response');
    videoModels = data;
    const sel = $('vmodel');
    sel.innerHTML = '';
    for (const m of videoModels) {
      const opt = document.createElement('option');
      opt.value = m.name;
      // "tuned": it opens with saved defaults of its own — the Image tab's tag.
      opt.textContent = `${m.name}${m.tuned ? ' [tuned]' : ''}${m.exists ? '' : ' — not downloaded'}`;
      sel.appendChild(opt);
    }
    // A rebuilt select falls back to its first option: put the pick back.
    if (videoModels.some((m) => m.name === before)) sel.value = before;
    if (!videoModels.length) {
      $('verr').textContent =
        'No video models configured. Add a [[video]] entry to models.toml — '
        + 'the comments there list what each family needs.';
      $('vgo').disabled = true;
      return;
    }
    if (refill || sel.value !== before) syncVideo();
    else {
      $('vclearmodel').hidden = !currentVideoModel()?.tuned;
      syncReady();
      // A download may have brought a speed LoRA: Turbo shows once it is here.
      videoLoras.reload();
    }
  } catch (e) {
    $('verr').textContent = 'Could not load video models: ' + (e.message || e);
  }
}

// ---- Generating -------------------------------------------------------------

function vidPayload(rows) {
  const m = currentVideoModel();
  return {
    model: $('vmodel').value,
    // Sent even when empty: an emptied box means "no negative", which is not
    // the same as "inherit the model's".
    negative: $('vnegative').value,
    // Shot 1's image: the clip's first frame.
    source: rows[0] ? sentRow(rows[0], 0, takeOf(m)).key : null,
    width: numOr('vw'),
    height: numOr('vh'),
    fps: numOr('vfps'),
    steps: numOr('vsteps'),
    cfg_scale: numOr('vcfg'),
    high_noise_steps: m && m.is_moe ? numOr('vhsteps') : null,
    high_noise_cfg_scale: m && m.is_moe ? numOr('vhcfg') : null,
    flow_shift: numOr('vshift'),
    sampler: $('vsampler').value || null,
    vae_tiling: $('vtiling').checked,
    memory: $('vmemory').value || null,
    parts: videoParts.picked(),
    // Always sent, `[]` included: the picker is the whole answer.
    loras: videoLoras.get(),
    shots: rows.map((r, k) => shotWire(r, k, takeOf(m))),
    every: $('veverytext').value,
  };
}

// After a press: the rows the clip now has, with the seeds they were drawn
// with, then any sent that it didn't get to (a Stop, a failed shot) — kept
// for another try.
function landRows(c, sent) {
  drawShots([...rowsOf(c), ...sent.slice((c.segments ?? []).length)]);
  syncVideoSummaries();
  setDraft(c);
}

// A clip request starts or ends: Stop shows while any is in flight (it
// stops the one running; the queue goes on to the next).
function began() {
  pending++;
  $('vstop').hidden = false;
  $('vstop').disabled = false;
  $('vstop').textContent = 'Stop';
  syncReady();
  syncShots();
}

function ended() {
  pending--;
  $('vstop').hidden = pending === 0;
  $('vstop').disabled = false;
  $('vstop').textContent = 'Stop';
  // Only the button: the boxes keep what made this clip, so the next one
  // starts from it. `syncVideo` here used to put the model's defaults back.
  syncReady();
  syncShots();
}

async function startClip() {
  const sent = liveRows();
  $('verr').textContent = '';
  if (!sent.length) {
    showError($('verr'), 'Say what happens in shot 1.');
    return;
  }
  const stageAtStart = stageSeq;
  const body = vidPayload(sent);
  began();
  try {
    const req = post('/video', body);
    soonJobs();
    const data = await req;
    // Onto the stage unless another clip was opened, or a new one started,
    // while this one rendered: then it only joins the grid, and the stage and
    // the rows being set up stay.
    if (stageSeq === stageAtStart) {
      renderClip(data.clip, data.gen_ms);
      landRows(data.clip, sent);
    }
    // A long clip cut short says why — a Stop or a failed shot — and is kept.
    if (data.note) $('verr').textContent = data.note;
    loadClips();
  } catch (e) {
    const msg = String(e.message || e);
    // The marker SdRunner::run puts in a killed pass's error. A clip the user
    // stopped is not a failure, and reporting a stack of sd-cli stderr for it
    // would read as one.
    showError($('verr'), msg.includes('cancelled by request')
      ? 'Stopped. Nothing was kept — the partial frames are swept.'
      : msg.includes('removed from the job queue')
        ? 'Removed from the queue before it started.'
        : msg, e.oom, { memory: 'vmemory', retry: startClip });
  } finally {
    ended();
  }
}

// *Extend clip* and *Redo from shot k*: the clip on the stage changed in
// place — its first `keep` shots kept, the pending rows drawn after them, on
// the clip's own model and settings, so the join is seamless; only the rows,
// *Every shot* and the negative come from the panel. A shot that fails or is
// stopped after others keeps those, as a long clip's does.
async function extendClip() {
  const c = clip;
  const live = liveRows();
  const st = shotState(live, c, chains(c), clipTake(c));
  if (!c || busyClips.has(c.id) || (st.action !== 'extend' && st.action !== 'redo')) return;
  const keep = st.action === 'redo' ? st.done : c.segments.length;
  await runOnClip({
    id: c.id,
    keep,
    // On the clip's own model, so what it takes.
    shots: st.pending.map((r, j) => shotWire(r, keep + j, clipTake(c))),
    every: $('veverytext').value,
    negative: $('vnegative').value,
  }, [...live.slice(0, keep), ...st.pending]);
}

// The × on the clip's last shot: cut off, its text left in the row, so it
// can be drawn again with a press of Extend.
function cutLastShot() {
  if (!clip || busyClips.has(clip.id)) return;
  runOnClip({ id: clip.id, keep: clip.segments.length - 1, shots: [] }, liveRows());
}

// One `/videos/extend` on a clip, queued like a Generate, with Stop and
// errors where a Generate has them. The clip that comes back replaces it
// everywhere, and its rows if it is still on the stage.
async function runOnClip(body, rows) {
  $('verr').textContent = '';
  busyClips.add(body.id);
  began();
  try {
    const req = post('/videos/extend', body);
    soonJobs();
    const data = await req;
    clipChanged(data.clip);
    if (clip?.id === data.clip.id) landRows(data.clip, rows);
    // Off the stage, its draft becomes what was drawn plus what wasn't
    // reached, so opening it again shows the shots as they now are.
    else setDraft(data.clip, [...rowsOf(data.clip), ...rows.slice(data.clip.segments.length)], body.every, body.negative);
    if (data.note) $('verr').textContent = data.note;
  } catch (e) {
    const msg = String(e.message || e);
    showError($('verr'), msg.includes('cancelled by request')
      ? 'Stopped. The clip is as it was before this shot.'
      : msg.includes('removed from the job queue')
        ? 'Removed from the queue before it started.'
        : msg, e.oom, { memory: 'vmemory' });
  } finally {
    busyClips.delete(body.id);
    ended();
  }
}

// A clip's record changed — shots added or cut, an enlarge: the stage plays
// it as it now is, the grids hold it, the rows are marked against it.
function clipChanged(c) {
  if (clip?.id === c.id) renderClip(c);
  loadClips();
  syncShots();
}

const cancelClip = () => cancelRun('vstop', 'video');

// ---- The clip grids ----------------------------------------------------------
//
// The Image tab's two shelves, for clips: Current is `work/videos/` (and
// lists starred clips too, from `user/favorite_clips/`), Favorites the starred
// alone. A star moves a clip between the two (`VideoStore::favorite`); its id
// stays, and so does every URL under `/videos/{id}/`. `shelfGrid` in
// `workitems.js` is the same shape for stills.
function clipGrid({ list, del, grid, mode, delBtn, all, empty }) {
  let items = [];
  let seq = 0;
  const onFavorites = list === '/favorite-clips';
  const marks = gridMarks({
    grid,
    mode,
    // On Current, only what lives there: a starred clip is out of reach of a
    // delete here until it is un-starred.
    ids: () => items.filter((c) => onFavorites || !c.favorite).map((c) => c.id),
    reload: loadClips,
    err: (msg) => { $('verr').textContent = msg; },
    onMode: () => mark(),
    actions: [{
      value: 'delete',
      button: delBtn,
      mark: 'sel',
      label: (n) => `Delete ${n} selected`,
      route: () => del,
      key: 'ids',
      ...(all ? { all: { button: all, noun: 'clip', label: (n) => (n ? `Delete all ${n} unstarred` : 'Delete all unstarred') } } : {}),
    }],
  });

  async function load() {
    const mine = ++seq;
    let got = [];
    try {
      const data = await getJSON(list);
      if (!Array.isArray(data)) throw new Error('unexpected response');
      got = data;
    } catch (e) {
      $('verr').textContent = 'Could not load clips: ' + (e.message || e);
    }
    if (mine !== seq) return;
    items = got;
    marks.prune(items.map((c) => c.id));
    draw();
  }

  function draw() {
    const el = $(grid);
    if (!items.length) {
      el.innerHTML = `<p class="sub">${empty}</p>`;
      marks.sync();
      return;
    }
    el.innerHTML = items.map((c) => `<span class="tile">
      <button type="button" data-id="${esc(c.id)}"
        title="${esc(c.prompt)}&#10;${c.width}×${c.height} · ${c.frames} frames @ ${c.fps} fps · ${(c.segments ?? []).length || 1} shot(s) · seed ${c.seed}&#10;Click to show it; click again to open it (Enlarge, History, Save to output)">
        <img src="/videos/${esc(c.id)}/thumb.png" alt="" loading="lazy"
          width="${c.thumb_width}" height="${c.thumb_height}"></button>${star(c.favorite)}</span>`).join('');
    for (const b of el.querySelectorAll('button[data-id]')) {
      b.addEventListener('click', () => click(b.dataset.id));
    }
    wireStars(el, {
      of: (btn) => {
        const id = btn.previousElementSibling.dataset.id;
        return { favorite: !!items.find((x) => x.id === id)?.favorite, body: { ids: [id] } };
      },
      add: '/favorite-clips/add',
      remove: '/favorite-clips/remove',
      reload: loadClips,
      err: (msg) => { $('verr').textContent = msg; },
    });
    mark();
  }

  function mark() {
    for (const b of $(grid).querySelectorAll('button[data-id]')) {
      marks.mark(b, b.dataset.id);
      b.classList.toggle('active', !!clip && b.dataset.id === clip.id);
    }
    marks.sync();
  }

  function click(id) {
    if (marks.toggle(id)) {
      mark();
      return;
    }
    const c = items.find((x) => x.id === id);
    if (c) clickClip(c);
  }

  return { init: () => marks.init(), load, mark, find: (id) => items.find((c) => c.id === id) };
}

const grids = [
  clipGrid({
    list: '/videos', del: '/videos/delete', grid: 'vgrid', mode: 'vmode', delBtn: 'vdelete', all: 'vdeleteall',
    empty: 'No clips yet — generate one.',
  }),
  clipGrid({
    list: '/favorite-clips', del: '/favorite-clips/delete', grid: 'vfgrid', mode: 'vfmode', delBtn: 'vfdelete',
    empty: 'No favorites yet — star a clip to keep it.',
  }),
];

// Both shelves: a generate, a star, a delete or an extend can change either.
function loadClips() {
  return Promise.all(grids.map((g) => g.load()));
}

function markClips() {
  for (const g of grids) g.mark();
}

// Opening a clip needs no server round trip at all: the frames are already on
// disk under /videos/{id}/frames/, so this is the player pointed at them. The
// settings come back from the listing, which carries each clip's whole sidecar.
// Allowed while a clip renders: opening one only reads disk, and setting up
// the next job from another clip is exactly what the wait is for.
function clickClip(c) {
  // A second click on the clip already on the stage opens the clip dialog —
  // Enlarge, History, Save to output — as a click on the stage does.
  if (clip?.id === c.id) {
    openClipView(clip);
    return;
  }
  keepDraft();
  stageSeq++;
  $('verr').textContent = '';
  applyClip(c);
  renderClip(c);
  markClips();
  syncShots();
}

// *New clip*: an empty stage and one empty shot, so the next Generate makes
// a clip of its own rather than a redo of the one that was showing — which
// stays in Current. The model and its settings stay too: they are how clips
// are made here, not part of the one left behind. Allowed mid-render; that
// clip then only joins the grid.
function newClip() {
  keepDraft();
  stageSeq++;
  clearClip();
  markClips();
  $('verr').textContent = '';
  $('veverytext').value = '';
  $('vsentnote').textContent = '';
  // A seed of its own from the start, so the first render can be drawn again.
  drawShots([{ text: '', seed: randomSeed(), frames: currentVideoModel()?.frames ?? null, folded: false }]);
  snapShotFrames();
  syncVideoSummaries();
  syncRailDots();
  rowEls()[0]?.querySelector('textarea').focus();
}

// Put a clip's stored settings back in the controls, so the next Generate starts
// from what made the clip on screen rather than from whatever was last typed.
function applyClip(c) {
  const v = c.video;
  // Its own LoRAs and steps win over a model change's pending Turbo default.
  vTurboSeq++;
  // The typed negative where the roll made one — see `applyWorkItem`. Each
  // row's seed restores its roll.
  $('vnegative').value = c.typed_negative ?? c.negative ?? '';
  if (videoModels.some((m) => m.name === c.model)) $('vmodel').value = c.model;
  // Deliberately not firing `change`: that listener is syncVideo, which would
  // overwrite everything below with the model's defaults. The rows are redrawn
  // for the model, then set to the files the clip ran with — `c.video` keys its
  // paths by the slot names.
  videoParts.draw(currentVideoModel()?.parts);
  videoParts.set(c.video);
  syncReady();
  $('vw').value = v.width;
  $('vh').value = v.height;
  $('vfps').value = v.fps;
  $('vsteps').value = v.steps;
  $('vcfg').value = v.cfg_scale;
  $('vsampler').value = v.sampler;
  $('vshift').value = v.flow_shift ?? '';
  $('vtiling').checked = !!v.vae_tiling;
  // Memory is not restored, for the image tab's reason: it is how a run fits
  // on this machine now, not part of the clip.
  $('vmoe').hidden = !(currentVideoModel() || {}).is_moe;
  $('vhsteps').value = v.high_noise_steps ?? '';
  $('vhcfg').value = v.high_noise_cfg_scale ?? '';
  $('vsentnote').textContent = '';
  // Its shots, with their seeds and lengths — to extend, to redo from one,
  // to make again, or to edit.
  $('veverytext').value = c.every ?? '';
  drawShots(rowsOf(c));
  // What was added or changed on it and not drawn yet.
  const d = draftMap().get(c.id);
  if (d) {
    $('veverytext').value = d.every ?? '';
    $('vnegative').value = d.negative ?? $('vnegative').value;
    drawShots(d.rows);
  }
  // Its picked LoRAs; a clip from before picks has none.
  videoLoras.set(v.picked_loras ?? []);
  syncVideoNote();
  syncVideoSummaries();
}

// ---- From the Agent tab ------------------------------------------------------

// A storyboard's shots, sent from the Agent tab (`/chats/film {send}`), as a
// new clip: the one on screen keeps its draft. Nothing is drawn until
// Generate clip.
export function openShots({ every, source, shots, note }) {
  newClip();
  $('veverytext').value = every ?? '';
  const frames = currentVideoModel()?.frames ?? null;
  drawShots(shots.map((r, k) => ({ ...r, frames: r.frames ?? frames, key: k === 0 ? source ?? null : r.key, folded: false })));
  snapShotFrames();
  $('vsentnote').textContent = note || '';
  document.querySelector('.tab[data-tab="video"]')?.click();
  $('vfold-prompt').open = true;
  syncVideoSummaries();
  syncRailDots();
}

// A row's starting image from this computer: the row whose Upload was pressed.
let uploadFor = null;

async function takeImage(row, file) {
  if (!row || !file) return;
  $('verr').textContent = '';
  try {
    row.dataset.key = await upload(file);
  } catch (e) {
    $('verr').textContent = 'Could not import that image: ' + (e.message || e);
  }
  syncShots();
  syncVideoSummaries();
  keepDraft();
}

// The tab is a workspace like the Image tab's: its panels kept the folds' ids,
// and the first time Prompts and Generation are open, Current in the corner.
let wsPanels = null;
const PANEL_DEFS = [
  { id: 'vid-prompt', el: 'vfold-prompt', open: true },
  { id: 'vid-gen', el: 'vfold-gen', open: true },
  { id: 'vid-adv', el: 'vfold-adv' },
  { id: 'vid-info' },
  { id: 'vid-current', open: true, corner: 'bottom-right' },
  { id: 'vid-favorites' },
];

// A rail button's dot is its panel header's line in the accent.
function syncRailDots() {
  if (!wsPanels) return;
  for (const d of PANEL_DEFS) {
    wsPanels.dot(d.id, !!$(d.el ?? `panel-${d.id}`).querySelector('.fphead .foldstate.on'));
  }
}

// The tab was shown: its panels can be placed now that it has a size.
// Shown: the panels are placed, and the grids re-read, since clips are made
// off this tab too (the agent films into Current).
export function layoutVideoTab() {
  wsPanels?.layout();
  loadClips();
}

export function initVideo() {
  // The Image tab's sampler list, groups and all: one list in the markup, and
  // `config::SAMPLERS` checks it. First, before anything sets a value on it.
  $('vsampler').innerHTML = $('sampler').innerHTML;
  wsPanels = floatPanels({ host: $('tab-video'), rail: $('tab-video').querySelector('.wsrail'), defs: PANEL_DEFS });
  wsPanels.init();
  rememberFolds($('tab-video'));
  for (const ev of ['input', 'change']) $('tab-video').addEventListener(ev, syncRailDots);
  for (const ev of ['input', 'change']) $('tab-video').addEventListener(ev, syncVideoSummaries);
  $('vgo').addEventListener('click', startClip);
  $('vnew').addEventListener('click', newClip);
  // A clip the Agent tab made, opened from its chat: the grids learn of it
  // first, since it landed while this tab wasn't looking.
  document.addEventListener('yasdig:openclip', async (e) => {
    document.querySelector('.tab[data-tab="video"]')?.click();
    await loadClips();
    const c = grids.map((g) => g.find(e.detail)).find(Boolean);
    if (c) clickClip(c);
    else $('verr').textContent = 'That clip is gone: it was deleted.';
  });
  $('vsavemodel').addEventListener('click', () => saveVideoDefaults(false));
  $('vshotadd').addEventListener('click', addShot);
  $('vshots').addEventListener('click', (e) => {
    const row = e.target.closest('.shotrow');
    if (!row) return;
    const i = rowEls().indexOf(row);
    if (e.target.closest('.shotfold')) {
      const folded = row.classList.toggle('folded');
      e.target.closest('.shotfold').setAttribute('aria-expanded', String(!folded));
      return;
    }
    if (e.target.closest('.shotx')) {
      // The clip's last shot is cut off the clip; any other row only leaves
      // the panel.
      const st = shotsNow();
      if (st.dropLast >= 0 && liveRows()[st.dropLast]?.i === i) { cutLastShot(); return; }
      const rows = rowEls().map(readRow);
      rows.splice(i, 1);
      drawShots(rows);
    } else if (e.target.closest('.shotreseed')) {
      row.querySelector('.shotseed').value = -1;
    } else if (e.target.closest('.shotroll')) {
      row.querySelector('.shotseed').value = randomSeed();
    } else if (e.target.closest('[data-start]')) {
      row.dataset.cut = e.target.closest('[data-start]').dataset.start;
      // A shot that goes on from a frame can't follow references, or start
      // from a picture of its own.
      if (row.dataset.cut === '0') {
        for (const k of ['refs', 'subjects', 'audio', 'voices']) row.dataset[k] = '[]';
        row.dataset.key = '';
      }
    } else if (e.target.closest('.shotkeyup')) {
      uploadFor = row;
      $('vkeyfile').click();
      return;
    } else if (e.target.closest('.shotkey')) {
      choosePicture((stem) => {
        row.dataset.key = stem;
        syncShots();
        syncVideoSummaries();
        keepDraft();
      });
      return;
    } else if (e.target.closest('.shotkeyclear')) {
      row.dataset.key = '';
    } else if (e.target.closest('.shotrefs')) {
      // Each picture's name, its look kept from the storyboard while the
      // picture stays.
      const was = JSON.parse(row.dataset.subjects || '[]');
      const had = JSON.parse(row.dataset.refs || '[]');
      editRefs(had, (stems, names) => {
        row.dataset.refs = JSON.stringify(stems);
        row.dataset.subjects = JSON.stringify(stems.map((stem, k) => ({
          name: names[k] ?? '', look: was[had.indexOf(stem)]?.look ?? '',
        })));
        syncShots();
        syncVideoSummaries();
        keepDraft();
      }, `Pictures shot ${i + 1} follows: a face, a costume, a place — MiniMax H3 keeps who and what is in `
        + 'them. The shot starts fresh, a new camera shot, since H3 cannot follow references and go on from '
        + 'a frame at once. Name who each one is, as the shot\'s words and lines do.', had.map((_, k) => was[k]?.name ?? ''));
      return;
    } else if (e.target.closest('.shotsounds')) {
      editSounds(JSON.parse(row.dataset.audio || '[]'), (stems, voices) => {
        row.dataset.audio = JSON.stringify(stems);
        row.dataset.voices = JSON.stringify(voices);
        syncShots();
        syncVideoSummaries();
        keepDraft();
      }, `Sounds shot ${i + 1} follows, up to three and 15 seconds in all: a voice to speak in, a sound to `
        + 'match. Give a voice the name its speaker has in Spoken, or name it in the prompt as <Audio 1> — '
        + '"the woman speaks in the voice of <Audio 1>". Like references, they start the shot fresh.',
      JSON.parse(row.dataset.voices || '[]'));
      return;
    } else return;
    syncShots();
    syncVideoSummaries();
  });
  $('vshots').addEventListener('change', (e) => {
    if (e.target.classList.contains('shotframes')) snapShotFrames();
    syncShots();
    syncVideoSummaries();
  });
  // Every shot counts as much as the rows: what a done shot was drawn with.
  $('tab-video').addEventListener('input', syncShots);
  // The Shots panel's edits are the stage clip's draft.
  for (const ev of ['input', 'change', 'click']) $('vfold-prompt').addEventListener(ev, keepDraftSoon);
  window.addEventListener('pagehide', keepDraft);
  $('vextend').addEventListener('click', extendClip);
  $('vclearmodel').addEventListener('click', () => saveVideoDefaults(true));
  $('vstop').addEventListener('click', cancelClip);
  $('vkeyfile').addEventListener('change', (e) => {
    takeImage(uploadFor, e.target.files[0]);
    // Cleared so re-picking the same file fires `change` again.
    e.target.value = '';
  });
  // An image dropped on a row's buttons starts that shot; a paste on the
  // tab, shot 1 — the first frame, as the old slot took it.
  $('vshots').addEventListener('dragover', (e) => { if (e.target.closest('.shotfrom')) e.preventDefault(); });
  $('vshots').addEventListener('drop', (e) => {
    const from = e.target.closest('.shotfrom');
    if (!from || from.querySelector('.shotkey').hidden) return;
    e.preventDefault();
    takeImage(from.closest('.shotrow'), e.dataTransfer?.files?.[0]);
  });
  document.addEventListener('paste', (e) => {
    if ($('tab-video').hidden || document.querySelector('dialog[open]')) return;
    if (/^(INPUT|TEXTAREA)$/.test(document.activeElement?.tagName)) return;
    const row = rowEls()[0];
    if (!row || row.querySelector('.shotkey').hidden) return;
    const file = [...(e.clipboardData?.items || [])].find((x) => x.kind === 'file' && x.type.startsWith('image/'));
    if (file) {
      e.preventDefault();
      takeImage(row, file.getAsFile());
    }
  });
  videoLoras.init();
  $('pickclose').addEventListener('click', () => $('picker').close());
  for (const g of grids) g.init();
  // A sent board's note spoke of the model it was sent to.
  $('vmodel').addEventListener('change', () => { $('vsentnote').textContent = ''; syncVideo(); });
  $('vturbo').addEventListener('change', () => {
    const m = currentVideoModel();
    if (m) vTurboMem.remember(m.name, $('vturbo').checked);
    setVTurbo($('vturbo').checked);
  });
  for (const id of ['vw', 'vh', 'vfps']) {
    $(id).addEventListener('input', syncVideoNote);
  }
  for (const id of ['vw', 'vh']) {
    $(id).addEventListener('change', () => {
      snapSize(id);
      syncVideoNote();
    });
  }
  $('vswapwh').addEventListener('click', () => {
    [$('vw').value, $('vh').value] = [$('vh').value, $('vw').value];
    $('vw').dispatchEvent(new Event('input', { bubbles: true }));
  });
  drawShots([{ text: '', seed: randomSeed(), frames: null }]);
  // An enlarge in the clip dialog adds a version: the grid's copy of the
  // clip, and the stage's, are the record it changed.
  initClipView({
    onClipChange: clipChanged,
    // One clip plays at a time: the dialog's player is the one looked at.
    onOpen: stopPlayback,
  });

  loadVideoModels();
  loadClips();
}
