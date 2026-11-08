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
import { chooseCastEntry } from './castpicks.js';
import { editSounds } from './sounds.js';
import { wireHints } from './hints.js';
import { choosePicture, upload } from './source-slot.js';
import { linesText, parseLines } from './lines.js';
import { partRows } from './parts.js';
import { loraPicker, turboMemory } from './loras.js';
import { loadModelSettings } from './modelsettings.js';
import { enhance } from './enhance.js';

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
  el: 'vpartrows', prefix: 'vpart', none: 'None', onChange: () => { syncReady(); syncHires(); syncPartNote(); },
});

// LTX-2.5 on the Get models card's bf16 encoder drew a stock scene (a man
// talking to camera) for prompts its q8_0 copy followed, same seed
// (2026-11-07). The Convert tab makes that copy beside it, and discovery
// prefers it; until then, say so.
function syncPartNote() {
  const note = $('vpartnote');
  const sel = $('vpart-llm');
  const path = sel?.selectedOptions[0]?.dataset.path ?? '';
  const bf16 = currentVideoModel()?.family === 'ltx_2_5' && /\.safetensors$/i.test(path);
  note.hidden = !bf16;
  note.textContent = bf16
    ? 'This bf16 text encoder can ignore the prompt on LTX-2.5. Get models fetches a Q4_K_M copy, or convert this one to q8_0 on the '
      + 'Convert tab (about 15 GB, a few minutes); the copy is picked here from then on.'
    : '';
}

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

// Who redraws from the rows when they change (the unbound Cast panel).
const rowWatchers = [];
export const onRowsChange = (fn) => { rowWatchers.push(fn); };

// The rows as the panel holds them now, blank ones too.
export const rowsNow = () => rowEls().map(readRow);

// The storyboard the rows show while a Video chat's film is bound to them
// (`board.js`), or null. Its hooks: `edited()` (a row or Every shot changed),
// `landed(clip)` (a press drew onto the rows), `left()` (the person put
// another clip, or a new one, on the stage), `isBoardClip(id)`,
// `rowExtras(el, k)` (the first-frame buttons of the board's shot k) and
// `framesLabel()`. The rows hold everything; the hooks keep the film in step.
let board = null;
export function setBoard(b) {
  board = b;
  syncShots();
}

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
    cast: rowCast(el, true),
    lines: parseLines(el.querySelector('.shotlines').value),
    sound: el.querySelector('.shotsnd').value.trim(),
    music: el.querySelector('.shotmusic').value.trim(),
    subjects: JSON.parse(el.dataset.subjects || '[]'),
    voices: JSON.parse(el.dataset.voices || '[]'),
  };
}

// ---- The cast on the rows -------------------------------------------------------
//
// A saved cast member on a shot, without a film: their picture as a reference
// named for them (with their look, for H3's subject lines), their voice as a
// sound that is theirs, their name in Who is in it. The rows are the record;
// the Cast panel (`board.js`, unbound) reads it back with `clipCast`.

// ---- Enhance -----------------------------------------------------------------
//
// A shot written out in full by the agent's model for the Video tab's model
// (`enhance.js`): with the clip's look, the shot before when it goes on from
// it, its first frame, who is in it and their looks, its length, and what is
// said and heard — LTX weaves speech into the paragraph, H3 keeps its fields
// and gets a Sound line when it had none.
const ENHANCE_TIP = "The agent's model writes this shot out in full for the picked video model: action, looks, place, camera and, on LTX, every sound. Ctrl+Z in the box undoes it.";
function enhanceRow(row, btn) {
  const m = currentVideoModel();
  const els = rowEls();
  const k = els.indexOf(row);
  const r = readRow(row);
  const fps = m?.forced_fps ?? (parseInt($('vfps').value, 10) || 0);
  const frames = r.frames ?? (m ? alignFrames(m.frames, m.frame_grid) : null);
  const note = row.querySelector('.shotenhanceerr');
  const body = {
    kind: 'video',
    model: $('vmodel').value,
    text: r.text,
    every: $('veverytext').value,
    before: k > 0 && !r.cut ? readRow(els[k - 1]).text : '',
    cut: k > 0 && r.cut,
    picture: sentRow(r, k, takeOf(m)).key || null,
    seconds: fps && frames ? frames / fps : null,
    cast: r.cast,
    looks: (r.subjects ?? []).filter((x) => x.name && x.look).map((x) => [x.name, x.look]),
    lines: r.lines,
    sound: r.sound,
    music: r.music,
  };
  enhance(btn, row.querySelector('textarea'), body, {
    onError: (msg) => { note.textContent = msg; note.hidden = !msg; },
    onDone: (a) => {
      const snd = row.querySelector('.shotsnd');
      if (a.sound && !snd.value.trim()) {
        snd.value = a.sound;
        snd.dispatchEvent(new Event('input', { bubbles: true }));
      }
    },
  });
}

const MAX_SHOT_REFS = 4; // config::MAX_REFERENCES
const MAX_SHOT_SOUNDS = 3; // pipeline::MAX_REF_AUDIO
const same = (a, b) => (a ?? '').trim().toLowerCase() === (b ?? '').trim().toLowerCase();
const json = (el, k) => JSON.parse(el.dataset[k] || '[]');

// Who is in a row: its chips (`data-cast`), and with `typed` also a name
// still in the box, not yet a chip — so a draft or a press counts it.
const splitNames = (v) => v.split(',').map((x) => x.trim()).filter(Boolean);
function rowCast(el, typed = false) {
  const names = json(el, 'cast');
  if (typed) {
    for (const n of splitNames(el.querySelector('.shotcastadd').value)) {
      if (!names.some((x) => same(x, n))) names.push(n);
    }
  }
  return names;
}
function setRowCast(el, names) {
  el.dataset.cast = JSON.stringify(names);
}

// The typed names become chips; the box empties.
function commitTypedCast(el) {
  const box = el.querySelector('.shotcastadd');
  if (!box.value.trim()) return false;
  setRowCast(el, rowCast(el, true));
  box.value = '';
  return true;
}

// A chip per name: the picture is the row's reference named for them, when
// it has one (a cast member added here, a storyboard's member) and the model
// follows references — on Wan or LTX a thumbnail would say it is used.
function drawRowCast(el) {
  const refs = videoTakes().references ? json(el, 'refs') : [];
  const subjects = json(el, 'subjects');
  const html = rowCast(el).map((name) => {
    const k = subjects.findIndex((sub, j) => refs[j] && same(sub?.name, name));
    const pic = k >= 0 ? `<img src="/outputs/${encodeURIComponent(refs[k])}/thumb.png" alt="">` : '';
    return `<div class="castchip" data-name="${esc(name)}">${pic}<b>${esc(name)}</b>
      <button type="button" class="btn castchipx shotuncast" aria-label="Remove ${esc(name)}"
        title="Take ${esc(name)} off this shot: their name, and the picture and voice added with them">×</button></div>`;
  }).join('');
  const box = el.querySelector('.shotchips');
  if (box.dataset.html !== html) {
    box.innerHTML = html;
    box.dataset.html = html;
  }
}

// `c`: `{stem, name, kind, look, voice}`. Their picture and voice only where
// the model follows them (H3) and the shot doesn't start from a first frame
// (sd.cpp takes one or the other: the frame stays, see `framed`); elsewhere
// the name alone, which the prompt and a storyboard's keyframes use. What
// didn't fit is said in the result.
const framed = (el) => videoTakes().references && !!el.dataset.key;
function castOnRow(el, c) {
  const take = videoTakes();
  if (!take.references || framed(el)) c = { ...c, stem: null, voice: null };
  else if (!take.sounds) c = { ...c, voice: null };
  const refs = json(el, 'refs');
  const subjects = json(el, 'subjects');
  const audio = json(el, 'audio');
  const voices = json(el, 'voices');
  let full = false;
  if (c.stem && !refs.includes(c.stem)) {
    if (refs.length < MAX_SHOT_REFS) {
      refs.push(c.stem);
      subjects[refs.length - 1] = { name: c.name, look: c.look ?? '' };
    } else full = true;
  }
  if (c.voice && !audio.includes(c.voice)) {
    if (audio.length < MAX_SHOT_SOUNDS) {
      audio.push(c.voice);
      voices[audio.length - 1] = c.name;
    } else full = true;
  } else if (c.voice) {
    voices[audio.indexOf(c.voice)] = c.name;
  }
  el.dataset.refs = JSON.stringify(refs);
  el.dataset.subjects = JSON.stringify(refs.map((_, k) => subjects[k] ?? { name: '', look: '' }));
  el.dataset.audio = JSON.stringify(audio);
  el.dataset.voices = JSON.stringify(audio.map((_, k) => voices[k] ?? ''));
  const names = rowCast(el);
  if (!names.some((n) => same(n, c.name))) setRowCast(el, [...names, c.name]);
  return full;
}

function rowsChanged() {
  syncShots();
  syncVideoSummaries();
  keepDraft();
  board?.edited();
}

// Who the rows' references name, in first-seen order: `{name, stem, shots,
// voice}` — the clip's cast as the unbound Cast panel shows it.
export function clipCast() {
  const out = [];
  rowEls().forEach((el, i) => {
    const refs = json(el, 'refs');
    const audio = json(el, 'audio');
    const voices = json(el, 'voices');
    json(el, 'subjects').forEach((sub, k) => {
      if (!sub?.name?.trim() || !refs[k]) return;
      let m = out.find((x) => same(x.name, sub.name));
      if (!m) out.push(m = { name: sub.name.trim(), stem: refs[k], shots: [], voice: false });
      if (!m.shots.includes(i)) m.shots.push(i);
      if (voices.some((v, j) => same(v, sub.name) && audio[j])) m.voice = true;
    });
  });
  return out;
}

// A saved entry as the rows hold it: its picture's and voice's output copies
// (`/cast/output`), `{stem, voice, name, kind, look}`.
export async function castEntry(id) {
  const e = await getJSON(`/cast/${encodeURIComponent(id)}`);
  const { stem } = await post('/cast/output', { id, which: 'picture' });
  const voice = e.voice && e.kind === 'character' ? (await post('/cast/output', { id, which: 'voice' })).stem : null;
  return { stem, voice, name: e.name.trim(), kind: e.kind, look: e.look };
}

// A shot's *Add cast…*, on a shot that starts fresh (shot 1, a cut; a
// continue has no cast): the member as `castOnRow` puts them.
function addCastToRow(el) {
  const names = rowCast(el, true);
  chooseCastEntry(async (id) => {
    const note = $('vshotnote');
    try {
      const c = await castEntry(id);
      const i = rowEls().indexOf(el);
      if (i < 0) return;
      commitTypedCast(el);
      if (i > 0 && el.dataset.cut !== '1') return;
      if (castOnRow(el, c)) {
        rowsChanged();
        note.textContent = `Shot ${i + 1} had no room for all of ${c.name}: a shot follows at most 4 pictures and 3 sounds.`;
        note.classList.add('err');
        return;
      }
      rowsChanged();
      if (framed(el) && c.stem) {
        note.textContent = `Shot ${i + 1} starts from its first frame, so ${c.name} is named in it, not pictured. `
          + 'Clear the frame for the shot to follow their picture.';
        note.classList.remove('err');
      }
    } catch (ex) {
      rowsChanged();
      note.textContent = String(ex.message || ex);
      note.classList.add('err');
    }
  }, (e) => names.some((n) => same(n, e.name)), 'Everyone in the cast is in this shot already.');
}

// What the shown model does with a cast member: references (H3), sounds.
export const videoTakes = () => takeOf(currentVideoModel());

// A saved cast member onto every shot that starts fresh (shot 1 and each
// cut): where references and sounds go. `{full, framed}`: the shot numbers
// that had no room, and those that start from a first frame, so name them
// without their picture.
export function addCastToShots(c) {
  const out = { full: [], framed: [] };
  rowEls().forEach((el, i) => {
    if (i > 0 && el.dataset.cut !== '1') return;
    if (framed(el) && c.stem) out.framed.push(i + 1);
    if (castOnRow(el, c)) out.full.push(i + 1);
  });
  rowsChanged();
  return out;
}

// Everything of `name` off every shot (or just `rows`): its references, its
// voice, its name.
export function removeCastFromShots(name, rows = rowEls()) {
  for (const el of rows) {
    const refs = json(el, 'refs');
    const subjects = json(el, 'subjects');
    const audio = json(el, 'audio');
    const voices = json(el, 'voices');
    const keep = refs.map((_, k) => !same(subjects[k]?.name, name));
    el.dataset.refs = JSON.stringify(refs.filter((_, k) => keep[k]));
    el.dataset.subjects = JSON.stringify(subjects.filter((_, k) => keep[k]));
    const keepA = audio.map((_, k) => !same(voices[k], name));
    el.dataset.audio = JSON.stringify(audio.filter((_, k) => keepA[k]));
    el.dataset.voices = JSON.stringify(voices.filter((_, k) => keepA[k]));
    setRowCast(el, rowCast(el).filter((n) => !same(n, name)));
  }
  rowsChanged();
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
export function drawShots(rows) {
  const was = rowEls().map((el) => el.classList.contains('folded'));
  $('vshots').innerHTML = rows.map((r, k) => {
    const folded = r.folded ?? was[k] ?? false;
    return `
    <div class="shotrow${folded ? ' folded' : ''}" data-cut="${r.cut ? 1 : 0}" data-refs="${esc(JSON.stringify(r.refs ?? []))}"
      data-subjects="${esc(JSON.stringify(r.subjects ?? []))}" data-audio="${esc(JSON.stringify(r.audio ?? []))}"
      data-voices="${esc(JSON.stringify(r.voices ?? []))}" data-key="${esc(r.key ?? '')}"
      data-cast="${esc(JSON.stringify(r.cast ?? []))}">
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
        <div class="btnrow shotenhancerow">
          <button type="button" class="btn shotenhance" title="${ENHANCE_TIP}" data-title="${ENHANCE_TIP}">Enhance</button>
          <span class="sub shotenhanceerr" hidden></span>
        </div>
        <div class="shotcastrow" hidden>
          <label data-help="vshotcast">Who is in it</label>
          <div class="castchips shotchips"></div>
          <div class="withbtn">
            <input type="text" class="shotcastadd" placeholder="Type a name, or Add cast…"
              aria-label="Add someone to shot ${k + 1}">
            <button type="button" class="btn shotcastpick" title="Someone from the Cast tab: on H3 their picture and voice come too">Add cast…</button>
          </div>
        </div>
        <div class="shotsoundrows" hidden>
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
        <label class="shotkeylabel" hidden>Start from image</label>
        <div class="btnrow shotfrom">
          <img class="shotkeythumb" alt="" hidden>
          <button type="button" class="btn shotkey" hidden title="The image this shot starts from: from Outputs, Favorites or the cast">Choose…</button>
          <button type="button" class="btn shotkeyup" hidden title="An image from this computer">Upload…</button>
          <button type="button" class="btn shotkeyclear" hidden>Clear</button>
          <span class="shotboard"></span>
        </div>
        <div class="btnrow shotfollow">
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
// when another clip is opened and put back when it is opened again. Only what
// differs from the clip is kept; a draft that matches it is dropped. On the
// server, beside the clip (`draft.json`, `POST /videos/draft`), so a reload or
// another browser has them too; `drafts` is what this page last knew of them.
// They used to be this browser's `localStorage` (`OLD_DRAFTS_KEY`), moved up
// once by `adoptOldDrafts`.
const OLD_DRAFTS_KEY = 'yasdig.clipdrafts';
const drafts = new Map();
// Drafts this page wrote: the listing may be older than the write, so its
// copy of these never replaces ours.
const draftsMine = new Set();
let draftTimer = null;
let oldDraftsAdopted = false;

// The listing's drafts, for clips this page hasn't written one for.
function takeDrafts(items) {
  for (const c of items) {
    if (draftsMine.has(c.id)) continue;
    if (c.draft) drafts.set(c.id, c.draft);
    else drafts.delete(c.id);
  }
}

function writeDraft(id, d, opts = {}) {
  const was = drafts.get(id) ?? null;
  if (JSON.stringify(was) === JSON.stringify(d)) return;
  draftsMine.add(id);
  if (d) drafts.set(id, d);
  else drafts.delete(id);
  post('/videos/draft', { id, draft: d }, opts)
    .catch((e) => console.warn('[yasdig] a clip draft was not kept', e));
}

// This browser's old drafts, onto the server once, for clips that have none
// there; then the old key goes.
function adoptOldDrafts(items) {
  if (oldDraftsAdopted) return;
  oldDraftsAdopted = true;
  let old = {};
  try { old = JSON.parse(localStorage.getItem(OLD_DRAFTS_KEY) || '{}'); } catch { return; }
  const known = new Set(items.map((c) => c.id));
  for (const [id, d] of Object.entries(old)) {
    if (known.has(id) && !drafts.has(id)) writeDraft(id, d);
  }
  try { localStorage.removeItem(OLD_DRAFTS_KEY); } catch { /* it stays, harmlessly */ }
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
function setDraft(c, rows = rowEls().map(readRow), every = $('veverytext').value, negative = $('vnegative').value, opts = {}) {
  // A storyboard's clip keeps none: the film is its draft. A plan is saved
  // whole instead (`savePlan`).
  if (board?.isBoardClip(c.id) || c.rendered === false) return;
  clearTimeout(draftTimer);
  const m = currentVideoModel();
  const live = rows.filter((r) => String(r.text ?? '').trim())
    .map((r) => ({ ...r, frames: m && r.frames != null ? alignFrames(r.frames, m.frame_grid) : r.frames }));
  writeDraft(c.id, matchesClip(c, live, every, negative) ? null : { rows, every, negative }, opts);
}

// What the stage holds is kept: a plan saved, a clip's draft written.
function keepDraft(opts = {}) {
  if (plan) savePlan({ quiet: true, ...opts });
  else if (clip) setDraft(clip, undefined, undefined, undefined, opts);
}

// Typing saves a moment later, so a reload loses at most that moment.
function keepDraftSoon() {
  clearTimeout(draftTimer);
  draftTimer = setTimeout(keepDraft, 400);
}

// ---- Saved, not generated ------------------------------------------------------
//
// *Save* keeps the whole clip as the Shots panel and the settings have it —
// a record with no frames (`POST /videos/plan`, `rendered: false`), listed in
// Current with a "Not generated" tile. While one is on the stage (`plan`, with
// `clip` null: there is nothing to play) every change saves itself, and
// Generate renders into it, so it keeps its id, its place and its star.
let plan = null;
let planSent = null;
let planSaving = false;

function planBody() {
  return { ...vidPayload(liveRows()), id: plan?.id ?? null };
}

// `quiet`: an autosave, which says nothing and skips a panel with no shots.
async function savePlan({ quiet = false, keepalive = false } = {}) {
  clearTimeout(draftTimer);
  if (!liveRows().length) {
    if (!quiet) showError($('verr'), 'Say what happens in shot 1.');
    return;
  }
  if (plan && busyClips.has(plan.id)) return;
  // A first save still on its way has no id to save over yet: a second would
  // make a second clip.
  if (!plan && planSaving) return;
  const body = planBody();
  const json = JSON.stringify(body);
  if (quiet && json === planSent) return;
  planSent = json;
  const seq = stageSeq;
  const first = !plan;
  if (first) planSaving = true;
  try {
    const data = await post('/videos/plan', body, { keepalive });
    // A New clip saved for the first time becomes the plan on the stage —
    // unless the stage moved on while it was on its way.
    if (seq === stageSeq) {
      plan = data.clip;
      if (!quiet) $('vsentnote').textContent = 'Saved, not generated. Changes save as you make them.';
      drawPlanInfo();
    }
    loadClips();
  } catch (e) {
    planSent = null;
    if (!quiet) showError($('verr'), String(e.message || e));
    else console.warn('[yasdig] a saved clip was not updated', e);
  } finally {
    if (first) planSaving = false;
  }
}

function drawPlanInfo() {
  if (!plan) return;
  $('vresultinfo').innerHTML = `<p class="sub">Saved, not generated yet — ${plan.segments.length} shot(s).
    Generate draws it into this clip.</p>`;
}

// A plan onto the stage: its settings and rows into the panel, nothing to play.
function stagePlan(c) {
  keepDraft();
  stageSeq++;
  $('verr').textContent = '';
  clearClip();
  applyClip(c);
  plan = c;
  planSent = JSON.stringify(planBody());
  drawPlanInfo();
  markClips();
  syncShots();
}

// A row for + Add shot: the seed and length of the row above it, so a new
// shot starts as its neighbour was drawn (the dice or Reset vary it).
function addShot() {
  const els = rowEls();
  const last = els.length ? readRow(els.at(-1)) : null;
  drawShots([...rowEls().map(readRow), {
    text: '', seed: last?.seed ?? -1, frames: last?.frames ?? currentVideoModel()?.frames ?? null,
  }]);
  const row = rowEls().at(-1);
  row?.querySelector('textarea').focus({ preventScroll: true });
  row?.scrollIntoView({ block: 'nearest', inline: 'end', behavior: 'smooth' });
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
    // Shot 1 and every cut start something the shots after them go on from:
    // a shade brighter, so where each run begins shows at a glance.
    el.classList.toggle('fresh', i === 0 || (el.dataset.cut === '1' && keyOk));
    // What a folded row still says: the start of its prompt.
    el.querySelector('.shotsum').textContent = el.querySelector('textarea').value.trim();
    const cut = el.dataset.cut === '1' && i > 0;
    const start = el.querySelector('.shotstart');
    start.hidden = i === 0 || !keyOk;
    for (const b of start.querySelectorAll('[data-start]')) b.classList.toggle('active', (b.dataset.start === '1') === cut);
    // References on a cut, or on shot 1, never beside its image: H3 can't
    // follow them and go on from a frame at once, so each hides the other's
    // buttons and a cast member added beside a frame is a name (`framed`). A
    // row from before may still hold both: `sentRow` sends what the model
    // takes, the references on H3.
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
    // Who is in it decides a storyboard's references and first frames on any
    // model; off a board, only H3 reads it. Never on a continue: it starts
    // from the last frame, whoever that shows.
    el.querySelector('.shotcastrow').hidden = (!m?.sounds && !board) || !fresh;
    drawRowCast(el);
    // Shot 1's image is the clip's first frame; a cut's is how Wan cuts.
    const keyBtn = el.querySelector('.shotkey');
    keyBtn.hidden = !fresh || (i > 0 && !keyOk) || followed > 0;
    el.querySelector('.shotkeylabel').hidden = keyBtn.hidden;
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
    const extras = el.querySelector('.shotboard');
    extras.innerHTML = board?.rowExtras(live.findIndex((r) => r.i === i), refsOk) ?? '';
  });
  const foot = st.action === 'extend' || st.action === 'redo';
  $('vextend').hidden = !foot || busy;
  const frames = board?.framesLabel() ?? null;
  $('vframes').hidden = !frames;
  if (frames) $('vframes').textContent = frames;
  $('vextendfoot').hidden = $('vextend').hidden && $('vframes').hidden;
  for (const fn of rowWatchers) fn();
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

// LTX's *Hi-res ×2*: only with a latent upscaler in its parts, and the
// note says the size the clip comes out at.
function syncHires() {
  const m = currentVideoModel();
  $('vhiresrow').hidden = !m?.ltx;
  if (!m?.ltx) return;
  const has = !!videoParts.picked().latent_upscaler;
  $('vhires').disabled = !has;
  if (!has) $('vhires').checked = false;
  const w = numOr('vw'), h = numOr('vh');
  $('vhiresnote').textContent = !has
    ? 'Needs the latent upscaler (Get models has it on the LTX cards).'
    : $('vhires').checked && w && h
      ? `Drawn at ${w}×${h}, refined at ${w * 2}×${h * 2} — the clip's size.`
      : 'Draws at this size, then enlarges ×2 and refines it in the same run.';
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
  // Never `.on`: the rail's dot reads every line in the panel, and these
  // are settings, not a departure.
  const every = $('veverytext').value.trim();
  const neg = $('vnegative').value.trim();
  const sumEvery = (every ? `— ${every.length > 50 ? every.slice(0, 50) + '…' : every}` : '— nothing')
    + (neg ? ' · a negative' : ' · no negative');
  $('vsum-every').textContent = sumEvery;
  $('vsum-every').title = every;
  // A long clip is a departure from one clip a Generate: the rail's dot says so.
  $('vsum-prompt').classList.toggle('on', live.length > 1);
  const name = $('vmodel').value;
  const picked = partsDeparted();
  const loras = videoLoras.get();
  $('vsum-gen').textContent = name
    ? `— ${name} · ${$('vw').value}×${$('vh').value}${!$('vhiresrow').hidden && $('vhires').checked ? ' · Hi-res ×2' : ''} · ${$('vsteps').value} steps`
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
  syncPartNote();
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
  $('vhires').checked = !!m.hires;
  syncHires();
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
  ['hires', 'vhires', 'ltx'],
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
    if (kind === 'ltx') { if (m.ltx) values[key] = $(id).checked; continue; }
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
    hires: m && m.ltx ? $('vhires').checked : null,
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
  // Named before the draft: a storyboard's clip keeps none.
  board?.landed(c);
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
  // A plan on the stage is saved as it stands, then drawn into.
  let into = plan?.id ?? null;
  let saved = null;
  if (into && busyClips.has(into)) return;
  if (into) await savePlan({ quiet: true });
  const body = { ...vidPayload(sent), id: into };
  // Anything else is saved first, as a plan of its own drawn into, so the
  // clip is in Current from the press — "Generating…" while it renders, its
  // frames when done, and kept as not generated if it fails or is stopped.
  // Not staged: the render lands on the stage as before.
  if (!into) {
    try {
      saved = (await post('/videos/plan', body)).clip;
      into = saved.id;
      body.id = into;
    } catch (e) {
      console.warn('[yasdig] the clip could not be saved before generating; generating anyway', e);
    }
  }
  if (into) busyClips.add(into);
  if (!plan && into) loadClips();
  began();
  try {
    const req = post('/video', body);
    soonJobs();
    const data = await req;
    // Onto the stage unless another clip was opened, or a new one started,
    // while this one rendered: then it only joins the grid, and the stage and
    // the rows being set up stay.
    if (stageSeq === stageAtStart) {
      plan = null;
      renderClip(data.clip, data.gen_ms);
      landRows(data.clip, sent);
    }
    // A long clip cut short says why — a Stop or a failed shot — and is kept.
    if (data.note) $('verr').textContent = data.note;
    loadClips();
  } catch (e) {
    const msg = String(e.message || e);
    // Still on the stage, the clip saved for this press becomes the plan
    // there, so Try again (or Generate) draws into it rather than saving a
    // second one beside it.
    if (saved && stageSeq === stageAtStart && !plan) {
      plan = saved;
      planSent = JSON.stringify(planBody());
      drawPlanInfo();
      markClips();
    }
    // The marker SdRunner::run puts in a killed pass's error. A clip the user
    // stopped is not a failure, and reporting a stack of sd-cli stderr for it
    // would read as one.
    showError($('verr'), msg.includes('cancelled by request')
      ? `Stopped. The partial frames are swept${into ? '; the clip stays in Current, not generated' : ''}.`
      : msg.includes('removed from the job queue')
        ? 'Removed from the queue before it started.'
        : msg, e.oom, { memory: 'vmemory', retry: startClip });
  } finally {
    if (into) busyClips.delete(into);
    ended();
    // The tile stops saying Generating…, whichever way it ended.
    loadClips();
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

  // Answers the listing, for `loadClips`' drafts: `[]` when this load lost a race.
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
    if (mine !== seq) return [];
    items = got;
    marks.prune(items.map((c) => c.id));
    draw();
    return items;
  }

  function draw() {
    const el = $(grid);
    if (!items.length) {
      el.innerHTML = `<p class="sub">${empty}</p>`;
      marks.sync();
      return;
    }
    el.innerHTML = items.map((c) => (c.rendered === false ? planTile(c) : `<span class="tile">
      <button type="button" data-id="${esc(c.id)}"
        title="${esc(c.prompt)}&#10;${c.width}×${c.height} · ${c.frames} frames @ ${c.fps} fps · ${(c.segments ?? []).length || 1} shot(s) · seed ${c.seed}&#10;Click to show it; click again to open it (Enlarge, History, Save to output)">
        <img src="/videos/${esc(c.id)}/thumb.png" alt="" loading="lazy"
          width="${c.thumb_width}" height="${c.thumb_height}"></button>${star(c.favorite)}</span>`)).join('');
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
      b.classList.toggle('active', b.dataset.id === (clip ?? plan)?.id);
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

// A clip saved and not generated: its first picture if it has one, else a
// card of the same height, both marked.
function planTile(c) {
  const pic = c.thumb_width
    ? `<img src="/videos/${esc(c.id)}/thumb.png?${c.created_ms}" alt="" loading="lazy" width="${c.thumb_width}" height="${c.thumb_height}">`
    : `<span class="plancard">${esc(c.prompt).slice(0, 80)}</span>`;
  return `<span class="tile plantile">
    <button type="button" data-id="${esc(c.id)}"
      title="${esc(c.prompt)}&#10;Saved, not generated · ${(c.segments ?? []).length} shot(s) · ${esc(c.model)}&#10;Click to open it; Generate draws it">
      ${pic}<span class="planmark">${busyClips.has(c.id) ? 'Generating…' : 'Not generated'}</span></button>${star(c.favorite)}</span>`;
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
async function loadClips() {
  const got = await Promise.all(grids.map((g) => g.load()));
  const items = got.flat();
  takeDrafts(items);
  adoptOldDrafts(items);
  // The plan on the stage, as the server now has it (a star moves it).
  if (plan) plan = items.find((c) => c.id === plan.id) ?? plan;
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
  if (c.rendered === false) {
    board?.flush();
    stagePlan(c);
    board?.left();
    return;
  }
  // The board's pending edits are read before the rows change; it is left
  // after, since its clip keeps no draft and the staging must still know it.
  board?.flush();
  stageClip(c);
  board?.left();
}

// A clip onto the stage, its rows into the panel. For a storyboard's own
// clip too (`board.js`), which then draws the board's rows over them.
export function stageClip(c) {
  keepDraft();
  plan = null;
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
  board?.flush();
  freshStage();
  board?.left();
  rowEls()[0]?.querySelector('textarea').focus();
}

// An empty stage and one empty row: *New clip*'s, and a storyboard's before
// it has a clip (`board.js`), which then draws its rows.
export function freshStage() {
  keepDraft();
  plan = null;
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
  syncPartNote();
  syncReady();
  $('vw').value = v.width;
  $('vh').value = v.height;
  $('vfps').value = v.fps;
  $('vsteps').value = v.steps;
  $('vcfg').value = v.cfg_scale;
  $('vsampler').value = v.sampler;
  $('vshift').value = v.flow_shift ?? '';
  $('vtiling').checked = !!v.vae_tiling;
  $('vhires').checked = !!v.hires;
  syncHires();
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
  const d = board?.isBoardClip(c.id) || c.rendered === false ? null : drafts.get(c.id);
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

// A clip by id onto the stage, for a storyboard opened with its clip
// (`board.js`): false when it is gone.
export async function stageClipById(id) {
  await loadClips();
  const c = grids.map((g) => g.find(id)).find(Boolean);
  if (c && clip?.id !== c.id) stageClip(c);
  return !!c;
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
  { id: 'vid-prompt', el: 'vfold-prompt', open: true, both: true },
  { id: 'vid-gen', el: 'vfold-gen', open: true },
  { id: 'vid-adv', el: 'vfold-adv' },
  { id: 'vid-info' },
  { id: 'vid-current', open: true, corner: 'bottom-right' },
  { id: 'vid-favorites' },
  { id: 'vid-assist', both: true },
  { id: 'vid-cast', both: true },
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
  $('vsaveclip').addEventListener('click', () => savePlan());
  // A plan saves itself on any change on the tab — its settings are part of
  // it, not only the rows.
  for (const ev of ['input', 'change']) $('tab-video').addEventListener(ev, () => { if (plan) keepDraftSoon(); });
  // A clip a chat made, opened from it: the grids learn of it
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
    } else if (e.target.closest('.shotenhance')) {
      enhanceRow(row, e.target.closest('.shotenhance'));
      return;
    } else if (e.target.closest('.shotreseed')) {
      row.querySelector('.shotseed').value = -1;
    } else if (e.target.closest('.shotroll')) {
      row.querySelector('.shotseed').value = randomSeed();
    } else if (e.target.closest('[data-start]')) {
      row.dataset.cut = e.target.closest('[data-start]').dataset.start;
      // A shot that goes on from a frame can't follow references, start
      // from a picture of its own, or name a cast: it has whoever the frame
      // shows.
      if (row.dataset.cut === '0') {
        for (const k of ['refs', 'subjects', 'audio', 'voices', 'cast']) row.dataset[k] = '[]';
        row.dataset.key = '';
        row.querySelector('.shotcastadd').value = '';
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
    } else if (e.target.closest('.shotcastpick')) {
      addCastToRow(row);
      return;
    } else if (e.target.closest('.shotuncast')) {
      removeCastFromShots(e.target.closest('.castchip').dataset.name, [row]);
      return;
    } else if (e.target.closest('.shotrefs')) {
      // Each picture's name, its look kept from the storyboard while the
      // picture stays.
      const was = JSON.parse(row.dataset.subjects || '[]');
      const had = JSON.parse(row.dataset.refs || '[]');
      editRefs(had, (stems, names, cast = []) => {
        row.dataset.refs = JSON.stringify(stems);
        row.dataset.subjects = JSON.stringify(stems.map((stem, k) => ({
          name: names[k] ?? '', look: cast.find((c) => c.stem === stem)?.look ?? was[had.indexOf(stem)]?.look ?? '',
        })));
        // A cast member picked here brings their voice and their name.
        for (const c of cast) castOnRow(row, { ...c, name: names[stems.indexOf(c.stem)] || c.name });
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
    // A typed name becomes a chip when the box is left (or Enter, below).
    if (e.target.classList.contains('shotcastadd')) commitTypedCast(e.target.closest('.shotrow'));
    syncShots();
    syncVideoSummaries();
  });
  $('vshots').addEventListener('keydown', (e) => {
    if (e.key !== 'Enter' || !e.target.classList.contains('shotcastadd')) return;
    e.preventDefault();
    if (commitTypedCast(e.target.closest('.shotrow'))) rowsChanged();
  });
  // Every shot counts as much as the rows: what a done shot was drawn with.
  $('tab-video').addEventListener('input', syncShots);
  // The Shots panel's edits are the stage clip's draft.
  for (const ev of ['input', 'change', 'click']) $('vfold-prompt').addEventListener(ev, keepDraftSoon);
  // And a bound storyboard's: the rows are its shots.
  for (const ev of ['input', 'change', 'click']) $('vfold-prompt').addEventListener(ev, () => board?.edited());
  window.addEventListener('pagehide', () => keepDraft({ keepalive: true }));
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
  $('vmodel').addEventListener('change', syncVideo);
  $('vturbo').addEventListener('change', () => {
    const m = currentVideoModel();
    if (m) vTurboMem.remember(m.name, $('vturbo').checked);
    setVTurbo($('vturbo').checked);
  });
  for (const id of ['vw', 'vh', 'vfps']) {
    $(id).addEventListener('input', syncVideoNote);
  }
  for (const id of ['vw', 'vh', 'vhires']) {
    $(id).addEventListener(id === 'vhires' ? 'change' : 'input', syncHires);
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
