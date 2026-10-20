import { $, esc, getJSON, post } from './dom.js';
import { S } from './state.js';
import { setBusy, setIdle } from './busy.js';
import { soonJobs } from './jobs.js';
import { floatPanels } from './panels.js';
import { upOptions, setUpFactor } from './image.js';

// ---- The clip dialog ----------------------------------------------------------
//
// A clip played full size, Refine & adjust's shape: a rail, the player on a
// pane, floating panels. Two kinds of clip open here:
//
// - A clip from the Clips grid (`work/videos/{id}/`): Enlarge makes a new
//   version of it (FSR on every frame of the original, `Engine::enlarge_clip`),
//   History lists the versions with Compare against the original, and Save to
//   output writes the version on screen into `work/output/`.
// - A clip saved to output (`{stem}.avi`), opened from the Outputs tab: it only
//   plays, its frames read out of the AVI (`/outputs/{stem}/frames/{k}.jpg`).
//
// The player is this module's own, not the Video stage's: the stage keeps
// playing what it shows, and closing the dialog leaves it as it was.

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
  $('cbplay').textContent = 'Play';
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
  $('cbplay').textContent = 'Pause';
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
  draw(Math.min(at, frames.length - 1));
  play();
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
  for (const b of $('clipbox').querySelectorAll('.lbrail .railbtn')) b.hidden = !clip;
  $('cbsave').hidden = !clip;
  if (!clip) {
    for (const id of ['panel-cb-enlarge', 'panel-cb-history']) if ($(id).open) $(id).open = false;
  }
  const href = clip
    ? `/videos/${encodeURIComponent(view.clip.id)}${version ? `/v${version}` : ''}/clip.avi`
    : `/outputs/${encodeURIComponent(view.stem)}/clip.avi`;
  $('cbdownload').href = href;
  $('cbdownload').download = `${clip ? view.clip.id : view.stem}${version ? `-v${version}` : ''}.avi`;
  $('sum-cb-enlarge').textContent = `— ${S.upFactor}×`;
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
  if (!$('clipbox').open) $('clipbox').showModal();
  panels.layout();
}

// Open a clip from the Clips grid (or the Video stage), on its newest version.
export function openClipView(clip) {
  view = { kind: 'clip', clip };
  open();
  show((clip.versions ?? []).at(-1)?.n ?? 0);
}

// Open a clip saved to output, from the Outputs tab: it plays, nothing more.
export async function openOutputClip(stem) {
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

export function initClipView({ onClipChange } = {}) {
  if (onClipChange) onClip = onClipChange;
  panels = floatPanels({
    host: $('clipbox'),
    rail: $('clipbox').querySelector('.lbrail'),
    defs: [{ id: 'cb-enlarge' }, { id: 'cb-history', corner: 'bottom-right' }],
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
  $('cbclose').addEventListener('click', () => $('clipbox').close());
  // Closing stops playback and lets the decoded frames go.
  $('clipbox').addEventListener('close', () => {
    stop();
    seq++;
    frames = [];
    base = [];
  });
}
