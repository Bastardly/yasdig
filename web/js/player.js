import { $, esc, ICONS, setPlayIcon, decodeFrames, stillsOf } from './dom.js';
import { openClipView } from './clipview.js';

// ---- The frame player -------------------------------------------------------
//
// The clip on screen, its decoded frames, and where the playhead is. `clip` is
// exported because the video tab reads it — which clip is on screen decides what
// Enlarge acts on and which tile in the grid is marked — but only this module
// ever writes it.
export let clip = null;
let vframes = [];
let vframe = 0;
let vtimer = null;
// A Video plays whole: its generated shots, in storyboard order, one frame
// list. `parts` says where each shot is in it ({c, from, n}); `clip` is the
// shot picked on the storyboard (null when that one isn't generated), which
// is what Enlarge and the Shot panel act on.
let parts = [];
let listKey = '';
// Decoded frames per shot, so moving between shots of one Video is a seek,
// not a reload. Only the Video on the stage is kept.
const decoded = new Map();
const shotKey = (c) => `${c.id}@${c.created_ms}@${c.frames}`;

// The shot the playhead is in.
const partAt = (i) => parts.find((p) => i >= p.from && i < p.from + p.n) ?? parts[0];

// A shot with sound has an <audio> beside the canvas. The frame timer is the
// clock and each shot's soundtrack is put where the frames are whenever
// playback starts, wraps or crosses into the shot — a few seconds don't drift
// far enough between those for a frame-accurate sync to be worth its code.
const sounds = () => [...document.querySelectorAll('#vresult audio[data-shot]')];

function syncSound() {
  const p = partAt(vframe);
  for (const a of sounds()) {
    if (a.dataset.shot !== p?.c.id) { a.pause(); continue; }
    a.currentTime = (vframe - p.from) / Math.max(1, p.c.fps);
    a.play().catch(() => {});
  }
}

export function stopPlayback() {
  clearTimeout(vtimer);
  vtimer = null;
  for (const a of sounds()) a.pause();
  setPlayIcon($('vplay'), false);
}

function drawFrame(i) {
  const cv = $('vcanvas');
  if (!cv || !vframes[i]) return;
  vframe = i;
  cv.getContext('2d').drawImage(vframes[i], 0, 0, cv.width, cv.height);
  const scrub = $('vscrub');
  if (scrub) scrub.value = i;
  const p = partAt(i);
  const at = $('vat');
  if (at && p) {
    const secs = parts.filter((x) => x.from < p.from).reduce((t, x) => t + x.n / (x.c.fps || 1), 0)
      + (i - p.from) / (p.c.fps || 1);
    at.textContent = `${parts.length > 1 ? `${p.label} · ` : ''}${i + 1}/${vframes.length} · ${secs.toFixed(2)}s`;
  }
  for (const m of document.querySelectorAll('#vshotmarks > span')) m.classList.toggle('at', m.dataset.shot === p?.c.id);
}

function togglePlay() {
  if (vtimer) { stopPlayback(); return; }
  if (!vframes.length) return;
  setPlayIcon($('vplay'), true);
  syncSound();
  const tick = () => {
    const was = partAt(vframe);
    const next = (vframe + 1) % vframes.length;
    drawFrame(next);
    if (next === 0 || partAt(next) !== was) syncSound();
    vtimer = setTimeout(tick, 1000 / Math.max(1, partAt(vframe)?.c.fps || 1));
  };
  vtimer = setTimeout(tick, 1000 / Math.max(1, partAt(vframe)?.c.fps || 1));
}

// A shot's frames, decoded once — a narrated one a still per part. A frame
// that won't decode must not hang the whole Video; it is dropped and counted.
function framesOf(c) {
  const k = shotKey(c);
  if (!decoded.has(k)) {
    decoded.set(k, decodeFrames(c.frames,
      (n) => `/videos/${encodeURIComponent(c.id)}/frames/${n}.png`, stillsOf(c)));
  }
  return decoded.get(k);
}

// Every frame, decoded before the first one is shown — playing while
// decoding would show the first pass at whatever rate the disk managed.
async function loadFrames(list, key, startAt) {
  stopPlayback();
  vframes = [];
  for (const k of [...decoded.keys()]) if (!list.some((c) => shotKey(c) === k)) decoded.delete(k);
  const got = await Promise.all(list.map(framesOf));
  // Another Video, or another set of shots, was put on the stage meanwhile.
  if (key !== listKey) return;
  let lost = 0;
  vframes = [];
  parts.forEach((p, j) => {
    const ok = got[j].filter(Boolean);
    lost += p.c.frames - ok.length;
    p.from = vframes.length;
    p.n = ok.length;
    vframes.push(...ok);
  });
  const cv = $('vcanvas');
  if (cv) {
    cv.width = list[0].width;
    cv.height = list[0].height;
  }
  const scrub = $('vscrub');
  if (scrub) scrub.max = Math.max(0, vframes.length - 1);
  drawMarks();
  // Shown, not played: judging starts at the person's moment.
  if (vframes.length) drawFrame(startAt());
  if (lost) $('verr').textContent = `${lost} frame(s) could not be loaded.`;
}

// Under the scrubber, the shots as a strip, each as wide as it is long; a
// click goes to the start of one.
function drawMarks() {
  const el = $('vshotmarks');
  if (!el) return;
  el.hidden = parts.length < 2;
  el.innerHTML = parts.map((p) => `<span data-shot="${esc(p.c.id)}" style="flex-grow:${Math.max(1, p.n)}"
    title="${esc(p.label)}" class="${p.c.id === clip?.id ? 'picked' : ''}"></span>`).join('');
}

// The separate files a clip ran with, by the names `VideoModel` gives them —
// which are also the Video tab's *Models* row keys.
const PARTS = [
  ['diffusion_model', 'Model'], ['high_noise_diffusion_model', 'High-noise model'],
  ['vae', 'VAE'], ['t5xxl', 'Text encoder'], ['llm', 'Text encoder'],
  ['audio_vae', 'Audio VAE'], ['clip_vision', 'CLIP-Vision'],
];
const fileName = (p) => String(p).split(/[\\/]/).pop();

// The Info panel: what is known about the clip on screen, the Image tab's
// panel of the same name. The prompt as it was conditioned only when a
// `{variable}` rolled. A long clip lists its shots as they were conditioned,
// *Every shot* and all; the Shots panel's rows already hold each one's seed.
function renderInfo(c, ms) {
  const v = c.video;
  const secs = (c.frames / Math.max(1, c.fps)).toFixed(2);
  const took = ms
    ? ` · took <b>${(ms / 1000).toFixed(1)} s</b> (${(ms / Math.max(1, c.frames)).toFixed(0)} ms/frame)` : '';
  const rolled = c.typed_prompt && c.prompt !== c.typed_prompt
    ? `<div class="meta">${esc(c.prompt)}</div>` : '';
  const shift = v.flow_shift != null ? ` · shift ${v.flow_shift}` : '';
  const moe = v.high_noise_steps != null ? ` · high-noise ${v.high_noise_steps} steps` : '';
  const files = PARTS.filter(([k]) => v[k])
    .map(([k, label]) => `<tr><td>${label}</td><td><code>${esc(fileName(v[k]))}</code></td></tr>`).join('');
  $('vresultinfo').innerHTML = `
    <div class="meta"><b>${c.width}×${c.height}</b> · ${c.frames} frames @ ${c.fps} fps · ${secs} s
      · seed ${c.seed}${took}</div>
    ${rolled}
    <p class="sub">${esc(c.model)} · ${v.steps} steps · CFG ${v.cfg_scale} · ${esc(v.sampler)}${shift}${moe}
      ${v.vae_tiling ? ' · VAE tiling' : ''}${c.audio ? ' · with sound' : ''}</p>
    ${c.source ? `<p class="sub">First frame: <code>${esc(c.source)}</code></p>` : ''}
    ${(c.segments ?? []).length > 1 ? `<ol class="sub">${c.segments.map((s) =>
      `<li>${s.cut ? '<b>Cut</b> · ' : ''}${esc(s.conditioned)} <span class="val">· from ${(s.start / Math.max(1, c.fps)).toFixed(1)} s
        · ${s.frames} frames · seed ${s.seed}${(s.references ?? []).length ? ` · ${s.references.length} ref(s)` : ''}</span></li>`).join('')}</ol>` : ''}
    <table class="helptable"><tbody>${files}</tbody></table>`;
}

// Nothing on the stage: *New video*, or a Video none of whose shots is made.
export function clearClip() {
  stopPlayback();
  clip = null;
  vframes = [];
  vframe = 0;
  parts = [];
  listKey = '';
  $('vresult').innerHTML = '';
  $('vresultinfo').innerHTML = '<p class="sub">Nothing generated yet.</p>';
}

// One clip on its own — the stage before Video 2.0, and still the way a shot
// that belongs to no Video plays.
export function renderClip(c, ms) {
  renderVideo([{ c, label: 'Shot 1' }], c, ms);
}

// A Video on the stage: `list` its generated shots in order (`{c, label}`),
// `sel` the shot picked (or null), the playhead put at its first frame. The
// same shots again only move the playhead.
export function renderVideo(list, sel, ms, { seek = true } = {}) {
  clip = sel;
  const key = list.map((x) => shotKey(x.c)).join('|');
  const startAt = () => parts.find((p) => p.c.id === sel?.id)?.from ?? 0;
  if (sel) renderInfo(sel, ms);
  if (key === listKey && $('vcanvas')) {
    if (!seek) return;
    stopPlayback();
    drawMarks();
    if (vframes.length) drawFrame(startAt());
    return;
  }
  listKey = key;
  parts = list.map((x) => ({ c: x.c, label: x.label, from: 0, n: 0 }));
  $('vresult').innerHTML = `
    <p class="stagehint">Click to enlarge, compare and save the shot under the playhead</p>
    <canvas id="vcanvas"></canvas>
    <div class="player">
      <button type="button" id="vplay" class="btn btn-icon" title="Play" aria-label="Play">${ICONS.play}</button>
      <input id="vscrub" type="range" min="0" max="0" step="1" value="0">
      <span class="val" id="vat"></span>
    </div>
    <div class="shotmarks" id="vshotmarks" hidden></div>
    ${list.filter((x) => x.c.audio).map((x) => `<audio data-shot="${esc(x.c.id)}" preload="auto"
      src="/videos/${encodeURIComponent(x.c.id)}/audio.wav?${x.c.created_ms}"></audio>`).join('')}`;
  // The canvas opens the clip dialog on the shot under the playhead —
  // Enlarge, History, Save to output — as the Image tab's picture opens
  // Refine & adjust.
  $('vcanvas').addEventListener('click', () => {
    const p = partAt(vframe);
    if (p) openClipView(p.c);
  });
  $('vplay').addEventListener('click', togglePlay);
  $('vscrub').addEventListener('input', (e) => {
    stopPlayback();
    drawFrame(parseInt(e.target.value, 10) || 0);
  });
  $('vshotmarks').addEventListener('click', (e) => {
    const id = e.target.closest('[data-shot]')?.dataset.shot;
    const p = parts.find((x) => x.c.id === id);
    if (!p) return;
    stopPlayback();
    drawFrame(p.from);
  });
  loadFrames(list.map((x) => x.c), key, startAt);
}
