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

export function stopPlayback() {
  clearInterval(vtimer);
  vtimer = null;
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
  vtimer = setInterval(() => {
    drawFrame((vframe + 1) % vframes.length);
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

// The result block, rebuilt per clip. `ms` is only there for a clip that was
// just generated — opening one from the grid has no timing to report, and
// inventing one would be worse than leaving it out.
export function renderClip(c, ms) {
  clip = c;
  const secs = (c.frames / Math.max(1, c.fps)).toFixed(2);
  const took = ms ? ` · took ${(ms / 1000).toFixed(1)}s (${(ms / Math.max(1, c.frames)).toFixed(0)} ms/frame)` : '';
  $('vresult').innerHTML = `
    <canvas id="vcanvas"></canvas>
    <div class="player">
      <button type="button" id="vplay" class="btn">Play</button>
      <input id="vscrub" type="range" min="0" max="0" step="1" value="0">
      <span class="val" id="vat"></span>
    </div>
    <div class="meta spaced">
      <b>${c.width}×${c.height}</b> · ${c.frames} frames @ ${c.fps} fps · ${secs}s
      · seed <b>${c.seed}</b> · ${esc(c.model)}${took}
    </div>
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
      Writes a filtered PNG sequence to work/output/. A fixed kernel can't
      flicker, so the frames stay stable — unlike a per-frame diffusion, which
      invents new detail each frame and boils.</p>
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
  wireHints($('vresult'));
  loadFrames(c);
}

