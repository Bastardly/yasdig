import { $, esc, getJSON, post, setPlayIcon } from './dom.js';
import { S } from './state.js';
import { setBusy, setIdle } from './busy.js';
import { soonJobs } from './jobs.js';
import { floatPanels } from './panels.js';
import { upOptions, setUpFactor } from './image.js';
import { FORMATS, available, exportClip, outSize } from './export.js';

// ---- The clip dialog ----------------------------------------------------------
//
// A clip played full size, Refine & adjust's shape: a rail, the player on a
// pane, floating panels. Two kinds of clip open here:
//
// - A clip from the Video tab's grids (`work/videos/{id}/`, or starred in
//   `user/favorite_clips/`): Enlarge makes a new version of it (FSR on every
//   frame of the original, `Engine::enlarge_clip`), History lists the versions
//   with Compare against the original, and Save to output writes the version
//   on screen into `work/output/` — the only way a clip gets there. Shots are
//   added on the Video tab, in the Shots panel, where Generate is.
// - A clip saved to output (`{stem}.avi`), opened from the Outputs tab: it
//   plays and downloads, its frames read out of the AVI
//   (`/outputs/{stem}/frames/{k}.jpg`).
//
// Download encodes the version on screen into a file to share (`export.js`).
//
// The player is this module's own, not the Video stage's. Opening the dialog
// stops the stage (`onOpen`): one clip plays at a time, the one looked at.

let view = null; // { kind: 'clip', clip } | { kind: 'output', stem, info }
let version = 0;
let frames = [];
let base = []; // the original's frames, while Compare is on
let at = 0;
let timer = null;
let split = 0.5;
let seq = 0; // bumped per load: only the newest may draw
let panels = null;
// Told when a clip's versions change, so the Video tab's grid and stage hold
// the clip as it now is.
let onClip = () => {};
let onOpen = () => {};
// The formats this browser can make of the version on screen, and a
// download in progress (its AbortController).
let canMake = new Set(['avi']);
let exporting = null;

const isClip = () => view?.kind === 'clip';
const fps = () => Math.max(1, isClip() ? view.clip.fps : view.info.fps);

function versions() {
  if (!isClip()) return [];
  const c = view.clip;
  return [{ n: 0, label: 'Original', width: c.width, height: c.height },
    ...(c.versions ?? []).map((v) => ({
      ...v,
      label: `Enlarged ${v.factor}×${v.sharpness ? ` · sharpness ${v.sharpness}` : ''}`
        + `${v.clarity ? ` · clarity ${v.clarity}` : ''}`,
    }))];
}

function frameUrl(v, k) {
  if (!isClip()) return `/outputs/${encodeURIComponent(view.stem)}/frames/${k}.jpg`;
  const id = encodeURIComponent(view.clip.id);
  return v ? `/videos/${id}/v${v}/frames/${k}.png` : `/videos/${id}/frames/${k}.png`;
}

function frameCount() {
  return isClip() ? view.clip.frames : view.info.frames;
}

async function decode(v) {
  const n = frameCount();
  const load = (k) => new Promise((resolve) => {
    const img = new Image();
    img.onload = () => resolve(img);
    img.onerror = () => resolve(null);
    img.src = frameUrl(v, k);
  });
  return (await Promise.all(Array.from({ length: n }, (_, k) => load(k)))).filter(Boolean);
}

function stop() {
  clearInterval(timer);
  timer = null;
  $('cbaudio').pause();
  setPlayIcon($('cbplay'), false);
}

function draw(i) {
  if (!frames[i]) return;
  at = i;
  const cv = $('cbcanvas');
  const ctx = cv.getContext('2d');
  ctx.drawImage(frames[i], 0, 0, cv.width, cv.height);
  // Compare: the original on the left of the line, scaled to this version's
  // size, so the same moment is judged side by side.
  if ($('cbcompare').checked && base[i] && version) {
    const x = Math.round(cv.width * split);
    ctx.save();
    ctx.beginPath();
    ctx.rect(0, 0, x, cv.height);
    ctx.clip();
    ctx.drawImage(base[i], 0, 0, cv.width, cv.height);
    ctx.restore();
    ctx.fillStyle = getComputedStyle(cv).getPropertyValue('--state').trim() || '#ddd9d2';
    ctx.fillRect(x - 1, 0, 2, cv.height);
  }
  $('cbscrub').value = i;
  $('cbat').textContent = `${i + 1}/${frames.length} · ${(i / fps()).toFixed(2)}s`;
}

function play() {
  if (timer) { stop(); return; }
  if (!frames.length) return;
  setPlayIcon($('cbplay'), true);
  const a = $('cbaudio');
  if (a.src) {
    a.currentTime = at / fps();
    a.play().catch(() => {});
  }
  timer = setInterval(() => {
    const next = (at + 1) % frames.length;
    draw(next);
    if (next === 0 && a.src) {
      a.currentTime = 0;
      a.play().catch(() => {});
    }
  }, 1000 / fps());
}

// Show version `v`: its frames decoded before the first is drawn, as the
// stage's player does, so the first pass plays at the clip's own rate.
async function show(v) {
  const mine = ++seq;
  stop();
  version = v;
  drawHistory();
  syncRail();
  const got = await decode(v);
  if (mine !== seq) return;
  frames = got;
  if (!frames.length) {
    $('cberr').textContent = 'Its frames could not be loaded.';
    return;
  }
  const cv = $('cbcanvas');
  cv.width = frames[0].naturalWidth;
  cv.height = frames[0].naturalHeight;
  $('cbscrub').max = Math.max(0, frames.length - 1);
  if ($('cbcompare').checked) await loadBase();
  if (mine !== seq) return;
  // Shown, not played — the stage's rule.
  draw(Math.min(at, frames.length - 1));
  probeFormats();
}

async function loadBase() {
  if (base.length || !isClip()) return;
  base = await decode(0);
}

function drawHistory() {
  const list = versions();
  $('cbcmprow').hidden = list.length < 2;
  const id = isClip() ? encodeURIComponent(view.clip.id) : '';
  $('cbhistory').innerHTML = list.map((v) => `
    <li><button type="button" data-v="${v.n}" class="${v.n === version ? 'active' : ''}">
      <img src="${v.n ? `/videos/${id}/v${v.n}/frames/0.png` : `/videos/${id}/thumb.png`}" alt="" loading="lazy">
      <span>${esc(v.label)} · ${v.width}×${v.height}</span>
    </button></li>`).reverse().join('');
  const n = list.length - 1;
  $('sum-cb-history').textContent = n ? `— ${n} version${n > 1 ? 's' : ''}` : '';
  panels?.dot('cb-history', n > 0 && version > 0);
}

function syncRail() {
  const clip = isClip();
  // A saved clip only plays and downloads.
  for (const b of $('clipbox').querySelectorAll('.lbrail .railbtn')) b.hidden = !clip && b.dataset.panel !== 'cb-download';
  $('cbsave').hidden = !clip;
  if (!clip) {
    for (const id of ['panel-cb-enlarge', 'panel-cb-history']) if ($(id).open) $(id).open = false;
  }
  $('sum-cb-enlarge').textContent = `— ${S.upFactor}×`;
}

// ---- Download ----------------------------------------------------------------

const aviUrl = () => (isClip()
  ? `/videos/${encodeURIComponent(view.clip.id)}${version ? `/v${version}` : ''}/clip.avi`
  : `/outputs/${encodeURIComponent(view.stem)}/clip.avi`);
const baseName = () => `${isClip() ? view.clip.id : view.stem}${isClip() && version ? `-v${version}` : ''}`;
const audioUrl = () => $('cbaudio').getAttribute('src') || null;

function remembered() {
  try { return localStorage.getItem('yasdig.clipformat'); } catch { return null; }
}

function picked() {
  return $('cbformats').querySelector('input:checked')?.value ?? 'avi';
}

// The radios for what this browser can make; the last pick if it still can,
// else the first.
function drawFormats() {
  const keep = $('cbformats').querySelector('input:checked')?.value ?? remembered();
  const offer = FORMATS.filter((f) => canMake.has(f.id));
  const want = offer.some((f) => f.id === keep) ? keep : offer[0].id;
  $('cbformats').innerHTML = offer.map((f) => `<label><input type="radio" name="cbformat" value="${f.id}"
    ${f.id === want ? 'checked' : ''}> ${f.label}</label>`).join('');
  syncDownload();
}

// What the pick makes, and the boxes that apply to it.
function syncDownload() {
  const fmt = picked();
  const f = FORMATS.find((x) => x.id === fmt);
  $('cbformatnote').textContent = f.note;
  $('cbencrow').hidden = fmt === 'avi';
  $('cbsize').parentElement.hidden = fmt === 'gif';
  $('sum-cb-download').textContent = `— ${f.label}`;
  const missing = FORMATS.filter((x) => !canMake.has(x.id)).map((x) => x.label);
  if (!exporting) {
    const cv = $('cbcanvas');
    const [w, h] = fmt === 'avi' ? [cv.width, cv.height] : outSize(fmt, cv.width, cv.height, parseFloat($('cbscale').value) || 1);
    $('cbdlnote').textContent = (cv.width ? `${w}×${h}. ` : '')
      + (missing.length ? `This browser can't make ${missing.join(' or ')} — another browser may.` : '');
  }
}

// Ask the browser once a version's size is known.
async function probeFormats() {
  const mine = seq;
  const cv = $('cbcanvas');
  let got = new Set(['avi', 'gif']);
  try {
    got = await available(cv.width, cv.height, fps());
  } catch { /* GIF and AVI need no encoder */ }
  if (mine !== seq) return;
  canMake = got;
  drawFormats();
}

function saveBlob(blob, name) {
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = name;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 60000);
}

async function download() {
  const fmt = picked();
  try { localStorage.setItem('yasdig.clipformat', fmt); } catch { /* a convenience */ }
  const f = FORMATS.find((x) => x.id === fmt);
  if (fmt === 'avi') {
    const a = document.createElement('a');
    a.href = aviUrl();
    a.download = `${baseName()}.avi`;
    document.body.appendChild(a);
    a.click();
    a.remove();
    return;
  }
  const v = version;
  exporting = new AbortController();
  $('cbdlgo').disabled = true;
  $('cbdlstop').hidden = false;
  $('cberr').textContent = '';
  $('cbdlnote').textContent = 'Starting…';
  try {
    const { blob, note } = await exportClip({
      fmt,
      frameUrl: (k) => frameUrl(v, k),
      frames: frames.length || frameCount(),
      fps: fps(),
      width: $('cbcanvas').width,
      height: $('cbcanvas').height,
      // A GIF has no sound to carry.
      audioUrl: fmt === 'gif' ? null : audioUrl(),
      scale: parseFloat($('cbscale').value) || 1,
      size: $('cbsize').value,
      signal: exporting.signal,
      onProgress: (done, total) => { $('cbdlnote').textContent = `Encoding frame ${done} of ${total}…`; },
    });
    saveBlob(blob, `${baseName()}.${f.ext}`);
    $('cbdlnote').textContent = `${f.label}, ${(blob.size / 1e6).toFixed(1)} MB.${note ? ` ${note}` : ''}`;
  } catch (e) {
    if (e?.name === 'AbortError') $('cbdlnote').textContent = 'Cancelled.';
    else { $('cbdlnote').textContent = ''; $('cberr').textContent = `Download: ${e.message || e}`; }
  } finally {
    exporting = null;
    $('cbdlgo').disabled = false;
    $('cbdlstop').hidden = true;
  }
}

function sound() {
  const a = $('cbaudio');
  const has = isClip() ? view.clip.audio : view.info.audio;
  const src = !has ? '' : isClip()
    ? `/videos/${encodeURIComponent(view.clip.id)}/audio.wav`
    : `/outputs/${encodeURIComponent(view.stem)}/audio.wav`;
  if (src) a.src = src; else a.removeAttribute('src');
}

function open() {
  base = [];
  at = 0;
  $('cberr').textContent = '';
  $('cbprog').textContent = '';
  $('cbcompare').checked = false;
  sound();
  onOpen();
  if (!$('clipbox').open) $('clipbox').showModal();
  panels.layout();
}

// Open a clip from the Clips grid (or the Video stage), on its newest version.
export function openClipView(clip) {
  view = { kind: 'clip', clip };
  open();
  show((clip.versions ?? []).at(-1)?.n ?? 0);
}

// Open a clip saved to output, from the Outputs tab. Saved as `{id}` or
// `{id}-v{n}`, so while the clip it came from is still on the Video tab it
// opens as that clip — Enlarge and History and all — on that version;
// otherwise the file only plays.
export async function openOutputClip(stem) {
  const [, id, v] = stem.match(/^(.*?)(?:-v(\d+))?$/);
  try {
    const clips = await getJSON('/videos');
    const c = Array.isArray(clips) ? clips.find((x) => x.id === id) : null;
    if (c) {
      view = { kind: 'clip', clip: c };
      open();
      const n = Number(v ?? 0);
      show((c.versions ?? []).some((x) => x.n === n) ? n : 0);
      return;
    }
  } catch { /* the file alone, below */ }
  try {
    const info = await getJSON(`/outputs/${encodeURIComponent(stem)}/video.json`);
    view = { kind: 'output', stem, info };
  } catch (e) {
    $('oerr').textContent = `Could not open ${stem}: ${e.message || e}`;
    return;
  }
  open();
  show(0);
}

async function enlarge() {
  if (!isClip()) return;
  setBusy('cbenlarge', 'Enlarging…', 'enlarge');
  $('cberr').textContent = '';
  soonJobs();
  try {
    const data = await post('/videos/enlarge', {
      id: view.clip.id,
      upscale: parseFloat($('cbfactor').value) || S.upFactor,
      sharpness: parseFloat($('cbsharp').value) || 0,
      clarity: parseFloat($('cbclarity').value) || 0,
    });
    view.clip = data.clip;
    onClip(data.clip);
    $('cbprog').textContent = `Version ${data.version}: ${data.clip.frames} frames in ${(data.ms / 1000).toFixed(1)} s`;
    show(data.version);
  } catch (e) {
    $('cberr').textContent = String(e.message || e);
  } finally {
    setIdle('cbenlarge');
  }
}

async function save() {
  if (!isClip()) return;
  setBusy('cbsave', 'Saving…');
  $('cberr').textContent = '';
  try {
    const data = await post('/videos/save', { id: view.clip.id, version });
    $('cbprog').textContent = `Saved → ${data.saved}`;
  } catch (e) {
    $('cberr').textContent = String(e.message || e);
  } finally {
    setIdle('cbsave');
  }
}

// Compare's line follows a drag on the picture.
function dragSplit(e) {
  if (!$('cbcompare').checked || !version) return;
  const move = (ev) => {
    const r = $('cbcanvas').getBoundingClientRect();
    split = Math.min(1, Math.max(0, (ev.clientX - r.left) / r.width));
    draw(at);
  };
  move(e);
  const cv = $('cbcanvas');
  cv.setPointerCapture?.(e.pointerId);
  const up = () => {
    cv.removeEventListener('pointermove', move);
    cv.removeEventListener('pointerup', up);
  };
  cv.addEventListener('pointermove', move);
  cv.addEventListener('pointerup', up);
}

export function initClipView({ onClipChange, onOpen: opened } = {}) {
  if (onClipChange) onClip = onClipChange;
  if (opened) onOpen = opened;
  panels = floatPanels({
    host: $('clipbox'),
    rail: $('clipbox').querySelector('.lbrail'),
    defs: [{ id: 'cb-enlarge' }, { id: 'cb-history', corner: 'bottom-right' }, { id: 'cb-download' }],
  });
  panels.init();
  $('cbfactor').innerHTML = upOptions();
  $('cbfactor').value = S.upFactor;
  $('cbfactor').addEventListener('change', (e) => {
    setUpFactor(parseFloat(e.target.value));
    syncRail();
  });
  $('cbplay').addEventListener('click', play);
  $('cbscrub').addEventListener('input', (e) => {
    stop();
    draw(parseInt(e.target.value, 10) || 0);
  });
  $('cbhistory').addEventListener('click', (e) => {
    const b = e.target.closest('[data-v]');
    if (b) show(Number(b.dataset.v));
  });
  $('cbcompare').addEventListener('change', async () => {
    if ($('cbcompare').checked) await loadBase();
    draw(at);
  });
  $('cbcanvas').addEventListener('pointerdown', dragSplit);
  $('cbenlarge').addEventListener('click', enlarge);
  $('cbsave').addEventListener('click', save);
  $('cbdlgo').addEventListener('click', download);
  $('cbdlstop').addEventListener('click', () => exporting?.abort());
  $('cbformats').addEventListener('change', syncDownload);
  $('cbscale').addEventListener('change', syncDownload);
  $('cbclose').addEventListener('click', () => $('clipbox').close());
  // Closing stops playback and lets the decoded frames go.
  $('clipbox').addEventListener('close', () => {
    exporting?.abort();
    stop();
    seq++;
    frames = [];
    base = [];
  });
}
