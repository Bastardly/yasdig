import { $, esc } from './dom.js';
import { S } from './state.js';
import { wireHints } from './hints.js';
import { upOptions, setUpFactor } from './image.js';
import { enlargeClip } from './video.js';

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
  const b = $('vplay');
  if (b) b.textContent = 'Play';
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
  $('vplay').textContent = 'Pause';
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
  if (vframes.length) {
    drawFrame(0);
    togglePlay();
  }
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
// panel of the same name. The prompt as it was conditioned only when it isn't
// what the box holds — a rolled `{variable}`, or a clip reopened from the grid.
function renderInfo(c, ms, saved) {
  const v = c.video;
  const secs = (c.frames / Math.max(1, c.fps)).toFixed(2);
  const took = ms
    ? ` · took <b>${(ms / 1000).toFixed(1)} s</b> (${(ms / Math.max(1, c.frames)).toFixed(0)} ms/frame)` : '';
  const rolled = c.prompt && c.prompt !== $('vprompt').value
    ? `<div class="meta">${esc(c.prompt)}</div>` : '';
  const shift = v.flow_shift != null ? ` · shift ${v.flow_shift}` : '';
  const moe = v.high_noise_steps != null ? ` · high-noise ${v.high_noise_steps} steps` : '';
  const files = PARTS.filter(([k]) => v[k])
    .map(([k, label]) => `<tr><td>${label}</td><td><code>${esc(fileName(v[k]))}</code></td></tr>`).join('');
  $('vresultinfo').innerHTML = `
    <div class="meta"><b>${c.width}×${c.height}</b> · ${c.frames} frames @ ${c.fps} fps · ${secs} s
      · seed <b id="vseedval" class="pin" title="Put this seed in the box, to generate it again">${c.seed}</b>${took}</div>
    ${rolled}
    <p class="sub">${esc(c.model)} · ${v.steps} steps · CFG ${v.cfg_scale} · ${esc(v.sampler)}${shift}${moe}
      ${v.vae_tiling ? ' · VAE tiling' : ''}${c.audio ? ' · with sound' : ''}</p>
    ${c.source ? `<p class="sub">First frame: <code>${esc(c.source)}</code></p>` : ''}
    <table class="helptable"><tbody>${files}</tbody></table>
    ${saved ? `<p class="sub">Saved <code>${esc(saved)}</code></p>` : ''}`;
  // Opt-in, as on the Image tab: the box keeps what was typed until this is
  // clicked.
  $('vseedval').addEventListener('click', () => { $('vseed').value = c.seed; });
}

// The clip, rebuilt per clip: the player on the stage, what is known about it
// in the Info panel and Enlarge frames in the Clip panel. `ms` and `saved` are
// only there for a clip that was just generated — opening one from the grid
// has no timing to report, and inventing one would be worse than leaving it out.
export function renderClip(c, ms, saved) {
  clip = c;
  $('vresult').innerHTML = `
    <canvas id="vcanvas"></canvas>
    <div class="player">
      <button type="button" id="vplay" class="btn">Play</button>
      <input id="vscrub" type="range" min="0" max="0" step="1" value="0">
      <span class="val" id="vat"></span>
      <a id="vdownload" class="btn" href="/videos/${encodeURIComponent(c.id)}/clip.avi" download="${esc(c.id)}.avi"
        title="The clip as one .avi file${c.audio ? ', with its sound' : ''}">Download</a>
    </div>
    ${c.audio ? `<audio id="vaudio" preload="auto" src="/videos/${encodeURIComponent(c.id)}/audio.wav"></audio>` : ''}`;
  renderInfo(c, ms, saved);
  $('vclipinfo').innerHTML = `
    <div class="actions">
      <select id="vfactor" title="FSR enlarge every frame">${upOptions()}</select>
      <button type="button" id="venlarge" class="btn btn-run">Enlarge frames</button>
    </div>
    <div class="row">
      <div>
        <label for="vsharp">Sharpness</label>
        <input id="vsharp" type="number" step="0.05" min="0" max="1" value="0">
      </div>
      <div>
        <label for="vclarity">Clarity</label>
        <input id="vclarity" type="number" step="0.05" min="-1" max="1" value="0">
      </div>
    </div>
    <!-- Their own boxes rather than the Image tab's sliders. Those live under
         the still on the other tab, so reading them here would apply a filter
         the user can neither see nor set — and every clip would inherit
         whatever the last image was left at. -->
    <p class="sub" id="venlargenote">
      Writes the filtered clip to work/output/, as a video file or a folder of
      frames — Settings → Save clips as. A fixed kernel can't flicker, so the
      frames stay stable — unlike a per-frame diffusion, which invents new
      detail each frame and boils.</p>
    <div id="vsaved" class="meta"></div>`;
  $('vfactor').value = S.upFactor;
  $('vfactor').addEventListener('change', (e) => setUpFactor(parseFloat(e.target.value)));
  $('vplay').addEventListener('click', togglePlay);
  $('vscrub').addEventListener('input', (e) => {
    stopPlayback();
    drawFrame(parseInt(e.target.value, 10) || 0);
  });
  $('venlarge').addEventListener('click', enlargeClip);
  // The only labelled controls on the page built after load, so the only place
  // that has to ask for its (i) icons itself.
  wireHints($('vclipinfo'));
  loadFrames(c);
}

