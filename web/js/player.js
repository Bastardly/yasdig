import { $, esc, ICONS, setPlayIcon } from './dom.js';
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

// A clip with sound (MiniMax H3) has an <audio> beside the canvas. The frame
// timer is the clock and the soundtrack is put where the frames are whenever
// playback starts or wraps — two seconds of footage don't drift far enough
// between those for a frame-accurate sync to be worth its code.
const sound = () => $('vaudio');

function syncSound() {
  const a = sound();
  if (!a) return;
  a.currentTime = vframe / Math.max(1, clip.fps);
  a.play().catch(() => {});
}

export function stopPlayback() {
  clearInterval(vtimer);
  vtimer = null;
  sound()?.pause();
  setPlayIcon($('vplay'), false);
}

function drawFrame(i) {
  const cv = $('vcanvas');
  if (!cv || !vframes[i]) return;
  vframe = i;
  cv.getContext('2d').drawImage(vframes[i], 0, 0);
  const scrub = $('vscrub');
  if (scrub) scrub.value = i;
  const at = $('vat');
  if (at) {
    at.textContent =
      `${i + 1}/${vframes.length} · ${(i / (clip.fps || 1)).toFixed(2)}s`;
  }
}

function togglePlay() {
  if (vtimer) { stopPlayback(); return; }
  if (!vframes.length) return;
  setPlayIcon($('vplay'), true);
  syncSound();
  vtimer = setInterval(() => {
    const next = (vframe + 1) % vframes.length;
    drawFrame(next);
    if (next === 0) syncSound();
  }, 1000 / Math.max(1, clip.fps));
}

// Every frame, decoded before the first one is shown. A clip is 33 PNGs off
// localhost, so this is a fraction of a second — and decoding as we play would
// show the first pass through at whatever rate the disk managed rather than at
// the clip's own fps.
async function loadFrames(c) {
  stopPlayback();
  vframes = [];
  const load = (n) => new Promise((resolve) => {
    const img = new Image();
    img.onload = () => resolve(img);
    // A frame that won't decode must not hang the whole clip; it is dropped and
    // the count below says so.
    img.onerror = () => resolve(null);
    img.src = `/videos/${encodeURIComponent(c.id)}/frames/${n}.png`;
  });
  const loaded = await Promise.all(
    Array.from({ length: c.frames }, (_, n) => load(n)));
  // Another clip was opened while these decoded: its own load owns the
  // canvas, and these frames playing under its title would be the wrong clip.
  if (c !== clip) return;
  vframes = loaded.filter(Boolean);
  const cv = $('vcanvas');
  if (cv) {
    cv.width = c.width;
    cv.height = c.height;
  }
  const scrub = $('vscrub');
  if (scrub) scrub.max = Math.max(0, vframes.length - 1);
  // Shown, not played: a clip that lands, or is opened, waits for Play —
  // judging it starts from its first frame, at the user's moment.
  if (vframes.length) drawFrame(0);
  if (vframes.length !== c.frames) {
    $('verr').textContent =
      `${c.frames - vframes.length} of ${c.frames} frames could not be loaded.`;
  }
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

// Nothing on the stage: *New clip*. The clip itself stays in its grid.
export function clearClip() {
  stopPlayback();
  clip = null;
  vframes = [];
  vframe = 0;
  $('vresult').innerHTML = '';
  $('vresultinfo').innerHTML = '<p class="sub">No clip yet.</p>';
}

// The clip, rebuilt per clip: the player on the stage and what is known about
// it in the Info panel; the clip dialog does the rest. `ms` is only there for
// a clip that was just generated — opening one from the grid has no timing to
// report, and inventing one would be worse than leaving it out.
export function renderClip(c, ms) {
  clip = c;
  $('vresult').innerHTML = `
    <canvas id="vcanvas"></canvas>
    <div class="player">
      <button type="button" id="vplay" class="btn btn-icon" title="Play" aria-label="Play">${ICONS.play}</button>
      <input id="vscrub" type="range" min="0" max="0" step="1" value="0">
      <span class="val" id="vat"></span>
    </div>
    ${c.audio ? `<audio id="vaudio" preload="auto" src="/videos/${encodeURIComponent(c.id)}/audio.wav"></audio>` : ''}`;
  renderInfo(c, ms);
  // The canvas opens the clip dialog — Enlarge, History, Save to output — as
  // the Image tab's picture opens Refine & adjust.
  $('vcanvas').title = 'Click to enlarge, compare and save';
  $('vcanvas').addEventListener('click', () => openClipView(clip));
  $('vplay').addEventListener('click', togglePlay);
  $('vscrub').addEventListener('input', (e) => {
    stopPlayback();
    drawFrame(parseInt(e.target.value, 10) || 0);
  });
  loadFrames(c);
}

