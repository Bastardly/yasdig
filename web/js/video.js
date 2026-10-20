import { $, esc, getJSON, post, numOr, rememberFolds } from './dom.js';
import { floatPanels } from './panels.js';
import { MEMORY_LABELS, showError } from './memory.js';
import { setBusy, setIdle, cancelRun } from './busy.js';
import { soonJobs } from './jobs.js';
import { clip, renderClip } from './player.js';
import { initClipView } from './clipview.js';
import { sourceSlot } from './source-slot.js';
import { partRows } from './parts.js';
import { loraPicker } from './loras.js';
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
// The last /videos listing, and which clips are marked for deletion.
let clips = [];
const vselected = new Set();
// A clip generation in flight. Only ever one — there is no batch here, because
// a single clip already owns the GPU for minutes.
let vrunning = false;

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
  onChange: () => syncVideoSummaries(),
});

// ---- Shots -------------------------------------------------------------------
//
// A long clip is one segment per shot, each made from the last one's final
// frame (`Engine::video`); the Prompt is the scene and a shot what happens in
// it. The rows hold their own text, so a redraw is only for adding or removing.
// `continueFrom` is a clip id the next one starts with — *Continue this clip*.
let continueFrom = null;

function shotList() {
  return [...$('vshots').querySelectorAll('textarea')].map((t) => t.value.trim()).filter(Boolean);
}

function drawShots(list) {
  $('vshots').innerHTML = list.map((text, i) => `
    <div class="shotrow">
      <b>${i + 1}</b>
      <textarea class="short" aria-label="Shot ${i + 1}" placeholder="what happens in this shot">${esc(text)}</textarea>
      <button type="button" class="btn btn-danger" data-del="${i}" title="Remove this shot">&times;</button>
    </div>`).join('');
  syncShots();
}

// What the shots add up to: their count times one segment's length, the
// segment being the Frames box after the model's rounding.
function syncShots() {
  const m = currentVideoModel();
  const rows = $('vshots').querySelectorAll('textarea').length;
  const n = shotList().length;
  const note = $('vshotnote');
  note.classList.remove('err');
  if (!m) { note.textContent = ''; return; }
  const frames = alignFrames(parseInt($('vframes').value, 10), m.frame_grid);
  const fps = m.forced_fps ?? (parseInt($('vfps').value, 10) || 1);
  const seg = frames / fps;
  if ((rows || continueFrom) && !m.chains) {
    note.textContent = `${m.name} can't go on from a clip's last frame, so it makes one clip at a time — `
      + 'TI2V-5B, A14B and H3 can.';
    note.classList.add('err');
  } else if (n > 1 || (n && continueFrom)) {
    note.textContent = `${n} shots × ${seg.toFixed(1)} s ≈ ${(n * seg).toFixed(1)} s`
      + (continueFrom ? ', after the clip it continues' : '');
  } else {
    note.textContent = rows ? 'One shot is one clip; add another to make it longer.' : '';
  }
  $('vcontinue').hidden = !continueFrom;
  if (continueFrom) {
    $('vcontinue').innerHTML = `Continues <code>${esc(continueFrom)}</code> from its last frame `
      + '<button type="button" class="btn" id="vcontinueclear" title="Make a fresh clip instead">Don’t</button>';
    $('vcontinueclear').onclick = () => { continueFrom = null; syncShots(); syncVideoSummaries(); };
  }
}

// Set up the next clip to go on from `c` — the clip dialog's *Continue this
// clip*: its settings back in the boxes, its shots cleared for new ones.
export function continueClip(c) {
  applyClip(c);
  continueFrom = c.id;
  drawShots(['']);
  $('vfold-prompt').open = true;
  $('vshots').querySelector('textarea')?.focus();
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
  $('vgo').disabled = !!missing.length || vrunning;
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
  const p = $('vprompt').value.trim();
  const shots = shotList().length;
  $('vsum-prompt').textContent = (p ? `— ${p.length > 60 ? p.slice(0, 60) + '…' : p}` : '')
    + (shots > 1 ? ` · ${shots} shots` : '') + (continueFrom ? ' · continues a clip' : '');
  // A long clip is a departure from one clip a Generate: the rail's dot says so.
  $('vsum-prompt').classList.toggle('on', shots > 1 || !!continueFrom);
  const m = $('vmodel').value;
  const picked = partsDeparted();
  const loras = videoLoras.get();
  $('vsum-gen').textContent = m
    ? `— ${m} · ${$('vw').value}×${$('vh').value} · ${$('vframes').value} frames`
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

// The rail's seed line, the Image tab's `syncSeedLine`: amber when pinned.
function syncVSeedLine() {
  const v = $('vseed').value.trim() || '-1';
  const el = $('vseedline');
  el.textContent = `Seed: ${v}`;
  el.title = el.textContent;
  el.classList.toggle('on', v !== '-1');
}

// What the boxes add up to, before anything runs: the frame count after
// rounding, the fps the model will actually use, and the duration that implies.
// The size is snapped in its boxes; a half-typed one is still rounded here.
function syncVideoNote() {
  const m = currentVideoModel();
  if (!m) { $('vgridnote').textContent = ''; return; }
  const w = alignSize(parseInt($('vw').value, 10), m.size_multiple);
  const h = alignSize(parseInt($('vh').value, 10), m.size_multiple);
  const frames = alignFrames(parseInt($('vframes').value, 10), m.frame_grid);
  const fps = m.forced_fps ?? (parseInt($('vfps').value, 10) || 1);
  const asked = [
    parseInt($('vframes').value, 10) !== frames
      ? `${frames} frames (the grid is ${m.frame_grid.multiple}k+${m.frame_grid.offset})` : null,
    m.forced_fps ? `${fps} fps — this model overrides anything else` : null,
  ].filter(Boolean);
  $('vgridnote').textContent =
    `${w}×${h}, ${frames} frames @ ${fps} fps = ${(frames / fps).toFixed(2)} s`
    + (asked.length ? ` · ${asked.join(' · ')}` : '');
}

// Pre-fill from the selected model, the same job syncAdvanced does for an image.
function syncVideo() {
  const m = currentVideoModel();
  if (!m) return;
  $('vnegative').value = m.negative_prompt ?? '';
  videoParts.draw(m.parts);
  // Re-greys the LoRA rows for this family (and shows the expert choice on
  // the pair).
  videoLoras.refresh();
  $('vw').value = m.width;
  $('vh').value = m.height;
  $('vw').step = m.size_multiple;
  $('vh').step = m.size_multiple;
  $('vframes').value = m.frames;
  $('vframes').step = m.frame_grid.multiple;
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
  ['width', 'vw'], ['height', 'vh'], ['frames', 'vframes'], ['fps', 'vfps'],
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
    }
  } catch (e) {
    $('verr').textContent = 'Could not load video models: ' + (e.message || e);
  }
}

// ---- Generating -------------------------------------------------------------

function vidPayload() {
  const seed = parseInt($('vseed').value, 10);
  const m = currentVideoModel();
  return {
    prompt: $('vprompt').value,
    model: $('vmodel').value,
    // Sent even when empty: an emptied box means "no negative", which is not
    // the same as "inherit the model's".
    negative: $('vnegative').value,
    seed: Number.isFinite(seed) ? seed : -1,
    source: vSource.get(),
    width: numOr('vw'),
    height: numOr('vh'),
    frames: numOr('vframes'),
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
    shots: shotList(),
    continue_from: continueFrom,
  };
}

async function startClip() {
  if (vrunning) return;
  vrunning = true;
  $('verr').textContent = '';
  $('vstop').hidden = false;
  $('vstop').disabled = false;
  $('vstop').textContent = 'Stop';
  setBusy('vgo', 'Generating clip…', 'video');
  const shownAtStart = clip?.id;
  try {
    const pending = post('/video', vidPayload());
    soonJobs();
    const data = await pending;
    // Onto the stage unless another clip was opened while this one rendered:
    // then it only joins the grid, and the one being looked at stays.
    if (!clip || clip.id === shownAtStart) renderClip(data.clip, data.gen_ms, data.saved);
    // A long clip cut short says why — a Stop or a failed shot — and is kept.
    if (data.note) $('verr').textContent = data.note;
    // Continuing is for one clip: the next Generate starts afresh.
    continueFrom = null;
    syncShots();
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
    vrunning = false;
    $('vstop').hidden = true;
    setIdle('vgo');
    // Only the button: the boxes keep what made this clip, so the next one
    // starts from it. `syncVideo` here used to put the model's defaults back.
    syncReady();
  }
}

const cancelClip = () => cancelRun('vstop', 'video');

// ---- The clip grid ----------------------------------------------------------

const vDeleteMode = () =>
  document.querySelector('input[name=vmode]:checked').value === 'delete';

async function loadClips() {
  try {
    const data = await getJSON('/videos');
    if (!Array.isArray(data)) throw new Error('unexpected response');
    clips = data;
  } catch (e) {
    clips = [];
    $('verr').textContent = 'Could not load clips: ' + (e.message || e);
  }
  for (const id of [...vselected]) {
    if (!clips.some((c) => c.id === id)) vselected.delete(id);
  }
  renderClips();
}

function renderClips() {
  const grid = $('vgrid');
  if (!clips.length) {
    grid.innerHTML = '<p class="sub">No clips yet.</p>';
    syncVDelete();
    return;
  }
  grid.innerHTML = clips.map((c) => `
    <button type="button" data-id="${esc(c.id)}"
      title="${esc(c.prompt)}&#10;${c.width}×${c.height} · ${c.frames} frames @ ${c.fps} fps · seed ${c.seed}">
      <img src="/videos/${esc(c.id)}/thumb.png" alt="" loading="lazy"
        width="${c.thumb_width}" height="${c.thumb_height}"></button>`).join('');
  for (const b of grid.querySelectorAll('button')) {
    b.addEventListener('click', () => clickClip(b.dataset.id));
  }
  markClips();
}

function markClips() {
  for (const b of $('vgrid').querySelectorAll('button')) {
    b.classList.toggle('sel', vselected.has(b.dataset.id));
    b.classList.toggle('active', !!clip && b.dataset.id === clip.id);
  }
  syncVDelete();
}

function syncVDelete() {
  $('vdelete').hidden = vselected.size === 0;
  $('vdelete').textContent = `Delete ${vselected.size} selected`;
}

// Opening a clip needs no server round trip at all: the frames are already on
// disk under /videos/{id}/frames/, so this is the player pointed at them. The
// settings come back from the listing, which carries each clip's whole sidecar.
function clickClip(id) {
  if (vDeleteMode()) {
    if (!vselected.delete(id)) vselected.add(id);
    markClips();
    return;
  }
  // Allowed while a clip renders: opening one only reads disk, and setting up
  // the next job from another clip is exactly what the wait is for.
  const c = clips.find((x) => x.id === id);
  if (!c) return;
  $('verr').textContent = '';
  applyClip(c);
  renderClip(c);
  markClips();
}

// Put a clip's stored settings back in the controls, so the next Generate starts
// from what made the clip on screen rather than from whatever was last typed.
function applyClip(c) {
  const v = c.video;
  // The typed prompt where the roll made one — see `applyWorkItem`. The seed
  // below restores the roll with it.
  $('vprompt').value = c.typed_prompt ?? c.prompt;
  $('vnegative').value = c.typed_negative ?? c.negative ?? '';
  $('vseed').value = c.seed;
  syncVSeedLine();
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
  $('vframes').value = v.frames;
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
  vSource.set(c.source);
  // Its shots, to make it again or to edit; a reopen is not a continuation.
  continueFrom = null;
  drawShots(c.shots ?? []);
  // Its picked LoRAs; a clip from before picks has none.
  videoLoras.set(v.picked_loras ?? []);
  syncVideoNote();
  syncVideoSummaries();
}

async function deleteClips() {
  const ids = [...vselected];
  if (!ids.length) return;
  $('vdelete').disabled = true;
  try {
    await post('/videos/delete', { ids });
    vselected.clear();
    await loadClips();
  } catch (e) {
    $('verr').textContent = String(e.message || e);
  } finally {
    $('vdelete').disabled = false;
  }
}

// ---- The first frame --------------------------------------------------------

// The Image tab's "Start from" slot, from the same factory. Its pick, upload,
// drop and paste all end in a `work/output/` stem, which is what `/video` takes.
export const vSource = sourceSlot({
  prefix: 'vsrc',
  tab: 'tab-video',
  none: 'None — text to video. Choose, upload, drop or paste an image to animate it.',
  err: 'verr',
});

// The tab is a workspace like the Image tab's: its panels kept the folds' ids,
// and the first time Prompts and Generation are open, Clips in the corner.
let wsPanels = null;
const PANEL_DEFS = [
  { id: 'vid-prompt', el: 'vfold-prompt', open: true },
  { id: 'vid-gen', el: 'vfold-gen', open: true },
  { id: 'vid-adv', el: 'vfold-adv' },
  { id: 'vid-info' },
  { id: 'vid-clips', open: true, corner: 'bottom-right' },
];

// A rail button's dot is its panel header's line in the accent.
function syncRailDots() {
  if (!wsPanels) return;
  for (const d of PANEL_DEFS) {
    wsPanels.dot(d.id, !!$(d.el ?? `panel-${d.id}`).querySelector('.fphead .foldstate.on'));
  }
}

// The tab was shown: its panels can be placed now that it has a size.
export const layoutVideoTab = () => wsPanels?.layout();

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
  $('vsavemodel').addEventListener('click', () => saveVideoDefaults(false));
  $('vshotadd').addEventListener('click', () => {
    const list = [...$('vshots').querySelectorAll('textarea')].map((t) => t.value);
    drawShots([...list, '']);
    [...$('vshots').querySelectorAll('textarea')].at(-1)?.focus();
    syncVideoSummaries();
  });
  $('vshots').addEventListener('click', (e) => {
    const b = e.target.closest('[data-del]');
    if (!b) return;
    const list = [...$('vshots').querySelectorAll('textarea')].map((t) => t.value);
    list.splice(Number(b.dataset.del), 1);
    drawShots(list);
    syncVideoSummaries();
  });
  $('vshots').addEventListener('input', syncShots);
  for (const id of ['vframes', 'vfps']) $(id).addEventListener('input', syncShots);
  $('vclearmodel').addEventListener('click', () => saveVideoDefaults(true));
  $('vstop').addEventListener('click', cancelClip);
  vSource.init();
  videoLoras.init();
  $('pickclose').addEventListener('click', () => $('picker').close());
  $('vdelete').addEventListener('click', deleteClips);
  $('vmodel').addEventListener('change', syncVideo);
  for (const id of ['vw', 'vh', 'vframes', 'vfps']) {
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
  $('vseedrandom').addEventListener('click', () => {
    $('vseed').value = -1;
    $('vseed').dispatchEvent(new Event('input', { bubbles: true }));
  });
  $('vseed').addEventListener('input', syncVSeedLine);
  syncVSeedLine();
  for (const r of document.querySelectorAll('input[name=vmode]')) {
    r.addEventListener('change', () => {
      if (!vDeleteMode()) vselected.clear();
      // The mark the hover rule reads. `data-mark` rather than a class,
      // because `gridmark.js` has two actions to name and one vocabulary for
      // both grids is the point of it.
      $('vgrid').dataset.mark = vDeleteMode() ? 'delete' : '';
      markClips();
    });
  }

  // An enlarge in the clip dialog adds a version: the grid's copy of the
  // clip, and the stage's, are the record it changed.
  initClipView({
    // Only a family that chains can go on from a clip's last frame.
    canContinue: (c) => !!videoModels.find((m) => m.name === c.model)?.chains,
    onContinue: continueClip,
    onClipChange: (c) => {
      const i = clips.findIndex((x) => x.id === c.id);
      if (i >= 0) clips[i] = c;
      // The stage plays it as it now is: a shot added or taken back changes
      // its frames, an enlarge its versions.
      if (clip?.id === c.id) renderClip(c);
      renderClips();
    },
  });

  loadVideoModels();
  loadClips();
}
