import { $, esc, post, numOr } from './dom.js';
import { showError } from './memory.js';
import { S } from './state.js';
import { setBusy, setIdle, cancelRun } from './busy.js';
import { soonJobs } from './jobs.js';
import { clip, renderClip } from './player.js';
import { sourceSlot } from './source-slot.js';

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

// Round the way the server will. Both rules come from /video-models rather than
// being written out here, because `video_cmd` rounds the same numbers to run the
// model and the duration below is only honest if the two agree exactly.
const alignSize = (v, mult) => Math.max(mult, Math.ceil((v || 0) / mult) * mult);

function alignFrames(v, g) {
  const f = Math.max(v || 0, g.min);
  if (g.round_up) return f + (((g.offset - f) % g.multiple) + g.multiple) % g.multiple;
  return Math.max(g.min, Math.floor((f - g.offset) / g.multiple) * g.multiple + g.offset);
}

// What the boxes add up to, before anything runs: the size and frame count after
// rounding, the fps the model will actually use, and the duration that implies.
function syncVideoNote() {
  const m = currentVideoModel();
  if (!m) { $('vgridnote').textContent = ''; return; }
  const w = alignSize(parseInt($('vw').value, 10), m.size_multiple);
  const h = alignSize(parseInt($('vh').value, 10), m.size_multiple);
  const frames = alignFrames(parseInt($('vframes').value, 10), m.frame_grid);
  const fps = m.forced_fps ?? (parseInt($('vfps').value, 10) || 1);
  const asked = [
    parseInt($('vw').value, 10) !== w || parseInt($('vh').value, 10) !== h
      ? `rounded to ${w}×${h} (multiples of ${m.size_multiple})` : null,
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
  // A video model is several files and tens of gigabytes, so "not downloaded
  // yet" is a normal state to be in for an hour. Say which file is missing.
  $('vmodelnote').textContent = m.missing.length
    ? `Not ready — missing ${m.missing.length} file(s): ${m.missing.join(', ')}`
    : (m.description || '');
  $('vgo').disabled = !!m.missing.length || vrunning;
  syncVideoNote();
}

async function loadVideoModels() {
  try {
    const data = await (await fetch('/video-models')).json();
    if (!Array.isArray(data)) throw new Error(data.error || 'unexpected response');
    videoModels = data;
    const sel = $('vmodel');
    sel.innerHTML = '';
    for (const m of videoModels) {
      const opt = document.createElement('option');
      opt.value = m.name;
      opt.textContent = `${m.name}${m.exists ? '' : ' — not downloaded'}`;
      sel.appendChild(opt);
    }
    if (!videoModels.length) {
      $('verr').textContent =
        'No video models configured. Add a [[video]] entry to models.toml — '
        + 'the comments there list what each family needs.';
      $('vgo').disabled = true;
      return;
    }
    syncVideo();
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
  try {
    const pending = post('/video', vidPayload());
    soonJobs();
    const data = await pending;
    renderClip(data.clip, data.gen_ms);
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
    syncVideo();
  }
}

const cancelClip = () => cancelRun('vstop', 'video');

export async function enlargeClip() {
  if (!clip) return;
  setBusy('venlarge', 'Enlarging…', 'enlarge');
  $('verr').textContent = '';
  soonJobs();
  try {
    const data = await post('/videos/enlarge', {
      id: clip.id,
      upscale: S.upFactor,
      sharpness: parseFloat($('vsharp').value) || 0,
      clarity: parseFloat($('vclarity').value) || 0,
    });
    $('vsaved').textContent =
      `${data.frames} frames at ${data.width}×${data.height} → ${data.dir} (${data.ms} ms)`;
  } catch (e) {
    $('verr').textContent = String(e.message || e);
  } finally {
    setIdle('venlarge');
  }
}

// ---- The clip grid ----------------------------------------------------------

const vDeleteMode = () =>
  document.querySelector('input[name=vmode]:checked').value === 'delete';

async function loadClips() {
  try {
    const data = await (await fetch('/videos')).json();
    if (!Array.isArray(data)) throw new Error(data.error || 'unexpected response');
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
  if (vrunning) return;
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
  if (videoModels.some((m) => m.name === c.model)) $('vmodel').value = c.model;
  // Deliberately not firing `change`: that listener is syncVideo, which would
  // overwrite everything below with the model's defaults.
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
  syncVideoNote();
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

export function initVideo() {
  // The Image tab's sampler list, groups and all: one list in the markup, and
  // `config::SAMPLERS` checks it. First, before anything sets a value on it.
  $('vsampler').innerHTML = $('sampler').innerHTML;
  $('vgo').addEventListener('click', startClip);
  $('vstop').addEventListener('click', cancelClip);
  vSource.init();
  $('pickclose').addEventListener('click', () => $('picker').close());
  $('vdelete').addEventListener('click', deleteClips);
  $('vmodel').addEventListener('change', syncVideo);
  for (const id of ['vw', 'vh', 'vframes', 'vfps']) {
    $(id).addEventListener('input', syncVideoNote);
  }
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

  loadVideoModels();
  loadClips();
}
