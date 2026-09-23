import { $, post, numOr } from './dom.js';
import { S } from './state.js';
import { setBusy, cancelRun, onIdle } from './busy.js';
import { models, effectiveSteps, render, escalateSettings, escalateModels,
         syncCfgPlaceholders } from './image.js';
import { loadWorkItems } from './workitems.js';
import { fetchOutputs, outputGrid } from './outputs.js';
import { loraPicker } from './loras.js';

// ---- Image to image ---------------------------------------------------------
//
// One pass over a picture that already exists, and then the ordinary ladder. The
// tab owns three things and borrows the rest: where the source comes from, the
// upload, and the one request. Everything after the result lands — refine,
// hi-res, the sliders, Save — is the shared machinery, pointed here by `S.pane`.

// The chosen source: a work/output/ stem, never a path. Same shape as the video
// tab's first frame, and the same reason — the page picks from what the server
// published, it never names a file.
let i2source = null;
let i2outputs = [];

// This tab's own picker. A second instance rather than a shared one: the two
// tabs are separate requests with separate model selects, and a LoRA chosen for
// a txt2img has no business riding along on a pass over somebody's photograph.
export const i2Loras = loraPicker({ rows: 'i2loras', add: 'i2loraadd' });

// The long side an upload is capped at before it leaves the browser. Bigger than
// anything worth diffusing (the working-size select tops out at 3072), and
// chosen against the body limit in http.rs: a 4096² PNG is ~25 MB against a
// 32 MB cap, while a 6000 px phone photo would not fit at all.
const MAX_UPLOAD_SIDE = 4096;

// Both this and the img2img pass itself round to 64, and they must agree: this
// is what the picture is scaled to, that is what the latent grid allows.
const WORKING_SIDES = [768, 1024, 1536, 2048, 3072];

function currentI2Model() {
  return models.find((m) => m.name === $('i2template').value);
}

// Pre-fill from the selected model, the same job syncAdvanced does for the other
// tab. The working size starts at the model's hi-res budget: that is the same
// question — how large can this card afford to diffuse — already answered for
// this hardware, so the page doesn't invent a second answer to it.
export function syncI2() {
  const m = currentI2Model();
  if (!m) return;
  $('i2steps').value = m.source_steps;
  $('i2strength').value = m.source_strength;
  $('i2cfg').value = m.cfg_scale;
  $('i2icfg').value = m.img2img_cfg_scale ?? '';
  $('i2sampler').value = m.sampler;
  $('i2rsteps').value = m.refine_steps;
  $('i2rstrength').value = m.refine_strength;
  $('i2hsteps').value = m.hires_steps;
  $('i2hstrength').value = m.hires_strength;
  // Blank stays blank — see `syncAdvanced`.
  $('i2rcfg').value = m.refine_cfg_scale ?? '';
  $('i2hcfg').value = m.hires_cfg_scale ?? '';
  syncCfgPlaceholders();

  const want = parseInt($('i2maxside').value, 10) || m.hires_max_side;
  $('i2maxside').innerHTML = WORKING_SIDES.map((v) =>
    `<option value="${v}"${v === want ? ' selected' : ''}>${v} px${
      v === m.hires_max_side ? ' (model budget)' : ''}</option>`).join('');
  if (!WORKING_SIDES.includes(want)) $('i2maxside').value = m.hires_max_side;

  syncI2Note();
}

export function syncI2Note() {
  const m = currentI2Model();
  if (!m) return;
  // The step floor is `profile.steps * strength`, and `profile.steps` is the
  // model's base step count — which this tab has no box for, because it has no
  // txt2img. So it comes from the model rather than from a field, and passing
  // the CFG here instead would make the whole line quietly wrong.
  const n = parseFloat($('i2steps').value) || 0;
  const s = parseFloat($('i2strength').value) || 0;
  const eff = effectiveSteps(n, s, m.steps);

  const cfgBase = parseFloat($('i2cfg').value);
  const own = parseFloat($('i2icfg').value);
  $('i2icfg').placeholder = Number.isFinite(cfgBase) ? cfgBase : '';
  const cfg = Number.isFinite(own) ? own : cfgBase;

  $('i2note').textContent =
    `Runs ${eff} step${eff === 1 ? '' : 's'} at strength ${s}`
    + (eff > n ? ` (raised from ${n} — the floor is ${Math.ceil(m.steps * s)} at this strength)` : '')
    + (Number.isFinite(cfg) ? ` · CFG ${cfg}` : '')
    + ` · a source over ${$('i2maxside').value} px on its long side is scaled down first`;
}

export function setI2Source(stem) {
  i2source = stem;
  $('i2srcthumb').hidden = !stem;
  if (stem) $('i2srcthumb').src = `/outputs/${encodeURIComponent(stem)}/thumb.png`;
  $('i2clear').hidden = !stem;
  $('i2srcname').textContent = stem
    ? stem
    : 'Drop an image here, paste one, or pick one below.';
  $('i2go').disabled = !stem || !!S.run;
  markI2Grid();
}

export async function loadI2Outputs() {
  i2outputs = await fetchOutputs();
  // The tile itself comes from `outputs.js`, which is the one place it is
  // written now — this grid, the Outputs tab and the video tab's picker all draw
  // the same thing, and the declared height has to match what `.wigrid` draws or
  // the strip reflows as the images decode.
  outputGrid($('i2grid'), i2outputs, {
    empty: '<p class="sub">Nothing in work/output/ yet — drop an image above, '
         + 'or save one from the Image tab.</p>',
    onPick: setI2Source,
  });
  markI2Grid();
}

function markI2Grid() {
  for (const b of $('i2grid').querySelectorAll('button')) {
    b.classList.toggle('active', b.dataset.stem === i2source);
  }
}

// Whatever the browser can decode becomes a PNG on the way out.
//
// This is what keeps `image` compiled with the `png` feature alone: the format
// list tracks the browser's rather than a crate's, so JPEG, WebP and AVIF all
// work without this binary growing a decoder it would then own. The cap is here
// rather than on the server for the same practical reason the whole route has a
// raised body limit — 25 MB of phone photo is not worth uploading to scale it
// down at the other end.
async function uploadFile(file) {
  if (!file || !file.type.startsWith('image/')) {
    $('i2err').textContent = 'That is not an image.';
    return;
  }
  $('i2err').textContent = '';
  $('i2srcname').textContent = 'Reading…';
  try {
    const bmp = await createImageBitmap(file);
    const scale = Math.min(1, MAX_UPLOAD_SIDE / Math.max(bmp.width, bmp.height));
    const w = Math.max(1, Math.round(bmp.width * scale));
    const h = Math.max(1, Math.round(bmp.height * scale));
    const cv = document.createElement('canvas');
    cv.width = w;
    cv.height = h;
    cv.getContext('2d').drawImage(bmp, 0, 0, w, h);
    bmp.close?.();
    const blob = await new Promise((ok) => cv.toBlob(ok, 'image/png'));
    if (!blob) throw new Error('the browser could not encode that image');

    // Raw bytes, not JSON: /upload is the one route with a binary body, which is
    // a third smaller on the wire and needs no decoder at either end.
    const resp = await fetch('/upload', {
      method: 'POST',
      headers: { 'Content-Type': 'image/png' },
      body: blob,
    });
    const data = await resp.json();
    if (!resp.ok || data.error) throw new Error(data.error || ('HTTP ' + resp.status));
    await loadI2Outputs();
    setI2Source(data.stem);
  } catch (e) {
    setI2Source(i2source);
    $('i2err').textContent = 'Could not import that image: ' + (e.message || e);
  }
}

async function startI2() {
  if (S.run || !i2source) return;
  S.run = { stop: false, done: 0, total: 1 };
  $('i2err').textContent = '';
  // Stop here kills the child, like the video tab's and unlike the Image tab's:
  // there is only ever one pass in flight on this tab, so stopping it can only
  // mean stopping *it*. A cancelled pass leaves the source image untouched —
  // it was only ever read.
  $('i2stop').hidden = false;
  $('i2stop').disabled = false;
  $('i2stop').textContent = 'Stop';
  setBusy('i2go', 'Running…');
  try {
    const seed = parseInt($('i2seed').value, 10);
    render(await post('/img2img', {
      prompt: $('i2prompt').value,
      template: $('i2template').value,
      source: i2source,
      negative: $('i2negative').value || null,
      seed: Number.isFinite(seed) ? seed : -1,
      max_side: numOr('i2maxside'),
      source_steps: numOr('i2steps'),
      source_strength: numOr('i2strength'),
      cfg_scale: numOr('i2cfg'),
      // This tab's own pass. It used to arrive through escalateSettings(), which
      // no longer carries it: after the split `img2img_cfg_scale` governs only
      // the pass this button runs, plus being what the two per-pass boxes fall
      // through to.
      img2img_cfg_scale: numOr('i2icfg'),
      sampler: $('i2sampler').value || null,
      // The escalation knobs travel with the generation here too, so a later
      // Refine starts from what this panel says rather than from the model's
      // defaults. The two checkpoints ride along for the same reason they do on
      // /generate: so reopening the work item restores the pickers.
      ...escalateSettings(),
      ...escalateModels(),
      // See genPayload: always sent, so an empty picker means none.
      loras: i2Loras.get(),
    }));
    // The result is archived like any other generation, so it belongs in the
    // other tab's grid immediately.
    loadWorkItems();
  } catch (e) {
    const msg = String(e.message || e);
    // sd-cli's own stderr tail is the wrong thing to show for a deliberate stop.
    $('i2err').textContent = msg.includes('cancelled by request') ? 'Stopped.' : msg;
  } finally {
    S.run = null;
    $('i2stop').hidden = true;
    setBusy(null);
    setI2Source(i2source);
  }
}

export function initI2() {
  // ---- Image to image wiring --------------------------------------------------
  i2Loras.init();
  $('i2go').addEventListener('click', startI2);
  $('i2stop').addEventListener('click', () => cancelRun('i2stop', 'i2err'));
  $('i2browse').addEventListener('click', () => $('i2file').click());
  $('i2clear').addEventListener('click', () => setI2Source(null));
  $('i2refresh').addEventListener('click', loadI2Outputs);
  $('i2file').addEventListener('change', (e) => {
    uploadFile(e.target.files[0]);
    // Cleared so re-picking the same file fires `change` again.
    e.target.value = '';
  });
  $('i2template').addEventListener('change', syncI2);
  for (const id of ['i2steps', 'i2strength', 'i2cfg', 'i2icfg', 'i2maxside']) {
    $(id).addEventListener('input', syncI2Note);
  }
  // Both boxes the per-pass CFGs fall through to on this tab, so what an empty
  // one resolves to has to follow them as they are typed in.
  for (const id of ['i2cfg', 'i2icfg']) {
    $(id).addEventListener('input', syncCfgPlaceholders);
  }
  // The whole zone, not the button — dragging a file onto a 90×30 target is a game
  // of skill. `dragover` must be cancelled or the browser navigates to the file.
  for (const ev of ['dragenter', 'dragover']) {
    $('i2drop').addEventListener(ev, (e) => {
      e.preventDefault();
      $('i2drop').classList.add('over');
    });
  }
  for (const ev of ['dragleave', 'drop']) {
    $('i2drop').addEventListener(ev, () => $('i2drop').classList.remove('over'));
  }
  $('i2drop').addEventListener('drop', (e) => {
    e.preventDefault();
    uploadFile(e.dataTransfer?.files?.[0]);
  });
  // Paste is on the document because there is nothing sensible to focus first, so
  // it checks the tab itself — and stays out of the way of pasting a prompt.
  document.addEventListener('paste', (e) => {
    if ($('tab-img2img').hidden) return;
    if (/^(INPUT|TEXTAREA)$/.test(document.activeElement?.tagName)) return;
    const file = [...(e.clipboardData?.items || [])]
      .find((i) => i.kind === 'file' && i.type.startsWith('image/'));
    if (file) {
      e.preventDefault();
      uploadFile(file.getAsFile());
    }
  });

  // "Run img2img" is only pressable once there is something to run it on, which
  // IDLE_LABELS can't know.
  onIdle(() => { $('i2go').disabled = !i2source; });

  // The source grid and the "nothing picked yet" state of the zone above it.
  loadI2Outputs();
  setI2Source(null);
}
