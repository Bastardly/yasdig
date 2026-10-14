import { $, esc, numOr, post } from './dom.js';
import { S } from './state.js';
import { isBusy, onIdle, setBusy, setIdle } from './busy.js';
import { onJobs, passRunning, runningJob, soonJobs } from './jobs.js';
import { claimLeft, closeViewer, deliver, onLook, onOpen, passLander, railDot, stepRequest, stepTo } from './viewer.js';
import { fillReady, hiresPass, oomOptions, passSteps, refine, setUpFactor, upOptions, upscalerOptions,
         upscalersReady } from './image.js';
import { openGetModels } from './getmodels.js';
import { showError } from './memory.js';
import { references } from './refs.js';

// The passes of the Refine & adjust dialog (`#lightbox`; viewer.js owns it):
// the Refine panel — paint over part of the picture and redraw just that part
// (`POST /redraw`, `Engine::redraw`), or the whole picture; Edit; Extend — the
// Scale panel's Scale fold, the pass prompt, the model the passes run on,
// History's Compare, and Stop in the status card. viewer.js owns the dialog,
// its pane and History; this module hooks in (`onLook`, `onOpen`,
// `claimLeft`).
//
// The mask is `master`, an offscreen canvas at the image's natural size, white
// where painted: that is what gets sent. `#rdmask` is the view of it, laid over
// `#lbimg` at the image's displayed size rather than wrapping it, so the pane's
// fit/centre rules (`zoomPane`) stay as they are. Display-sized, not natural:
// a 3072² image would otherwise be a second 36 MB canvas for one mask. It
// paints only while the Refine panel and its Refine fold are open — they own
// the pointer then, as Crop, Selection and Brushes do theirs.

let master = null;
// The image the mask belongs to. A different `image_id` clears the mask and the
// prompt — both were about the last picture. A redraw or a step back keeps them,
// so "again" is one click.
let maskFor = null;
let painted = false;
let eraser = false;
// Space is held: a left drag pans instead of painting, as in every paint program.
let space = false;
// The area's own strength. The whole image's is the Refine pass fold's box,
// so the dialog and the fold always agree and *Save as defaults* sees it; the
// one Strength box in the dialog shows whichever the mask says applies.
let areaStrength = 0.5;
// Which of the two the Strength box showed last, so a switch can save it first.
let strengthFor = null;
// The result the dialog is showing: its id, size, history and step.
let shown = null;
let frame = 0;
// Compare: on or off, which step is on the left, where the split is (a
// fraction of the image's width), and an epoch that drops a before-image
// answer a newer request overtook — `cancelPost`'s pattern. `beforeKey` is
// what the image on the left was fetched for, so a redraw of the dialog that
// changed nothing doesn't fetch it again.
let comparing = false;
let cmpWith = null;
let split = 0.5;
let beforeEpoch = 0;
let beforeKey = null;

const open = () => $('lightbox').open;
// The Refine panel is open with its Refine fold: a left drag on the picture
// paints the mask.
const masking = () => $('panel-refine').open && $('rdfold-refine').open;

// A set of ← ↑ ↓ → toggles and an amount: Extend's, and Edit's rescale. One
// factory for both, so the two can't come to disagree about what a side is or
// what it sends. `expand()` is the body's `expand` — the server's `Expand`.
function sideSet(root, onChange) {
  const sides = new Set();
  const amount = root.querySelector('[data-amount]');
  const set = {
    any: () => sides.size > 0,
    expand() {
      const px = Number(amount.value);
      return Object.fromEntries([...sides].map((s) => [s, px]));
    },
    // "1024×1024 → 1280×1024", or '' with nothing picked.
    note(w, h) {
      if (!sides.size) return '';
      const px = Number(amount.value);
      const gw = w + px * (sides.has('left') + sides.has('right'));
      const gh = h + px * (sides.has('top') + sides.has('bottom'));
      return `${w}×${h} → ${gw}×${gh}`;
    },
    // "← → 256 px" for a fold's summary, in the server's drawing order.
    label() {
      const arrows = { left: '←', right: '→', top: '↑', bottom: '↓' };
      const picked = ['left', 'right', 'top', 'bottom'].filter((s) => sides.has(s));
      return picked.length ? `${picked.map((s) => arrows[s]).join(' ')} ${amount.value} px` : '';
    },
    clear() { sides.clear(); set.sync(); },
    sync() {
      for (const b of root.querySelectorAll('[data-side]')) b.classList.toggle('active', sides.has(b.dataset.side));
    },
    disable(off) {
      for (const el of root.querySelectorAll('button, select')) el.disabled = off;
    },
  };
  root.addEventListener('click', (e) => {
    const b = e.target.closest('[data-side]');
    if (!b) return;
    const s = b.dataset.side;
    if (sides.has(s)) sides.delete(s); else sides.add(s);
    set.sync();
    onChange();
  });
  amount.addEventListener('change', onChange);
  return set;
}
let extendSides = null;
let editSides = null;
// The image was opened from Outputs, so the dialog offers its model: an upload
// names none, and a saved picture's maker may not be the one to finish with.
// Set on every open from the pane it opens on.
let pickModel = false;

// The mask at the picture's size. A picture that changed size but kept its
// shape (a Scale, a Hi-res) keeps the mask, scaled with it — that is what lets
// "scale down and try again" retry an area refine. A new shape (Extend, a
// crop) would put it in the wrong place, so it goes.
function ensureMaster(w, h) {
  if (master && master.width === w && master.height === h) return;
  const old = master;
  master = document.createElement('canvas');
  master.width = w;
  master.height = h;
  const sameShape = old && Math.abs(old.width / old.height - w / h) < 0.01;
  if (painted && sameShape) master.getContext('2d').drawImage(old, 0, 0, w, h);
  else painted = false;
}

function clearMask() {
  if (master) master.getContext('2d').clearRect(0, 0, master.width, master.height);
  painted = false;
  draw();
  syncButtons();
}

// Lay an element over the image: same box, in the pane's coordinates, so it
// scrolls with the picture at 1:1. The mask, the before image and the split
// all go through here, so the three can't drift apart.
function lay(el) {
  const img = $('lbimg');
  el.style.left = `${img.offsetLeft}px`;
  el.style.top = `${img.offsetTop}px`;
  el.style.width = `${img.clientWidth}px`;
  el.style.height = `${img.clientHeight}px`;
}

// The mask's view. Display-sized; the CSS blur previews the edge softness at
// the scale the image is shown at.
function place() {
  const img = $('lbimg'), c = $('rdmask');
  const w = img.clientWidth, h = img.clientHeight;
  if (!w) return;
  layCompare();
  if (!master) return;
  lay(c);
  const dpr = window.devicePixelRatio || 1;
  c.width = Math.max(1, Math.round(Math.min(w * dpr, master.width)));
  c.height = Math.max(1, Math.round(Math.min(h * dpr, master.height)));
  const scale = w / master.width;
  c.style.filter = `blur(${(Number($('rpsoft').value) * scale) / 2}px)`;
  draw();
}

// The before image over the left of the split, the line at the split, and a
// label in each top corner. An earlier, smaller step is stretched to the same
// box on purpose: judging an upscale is judging detail at one size.
function layCompare() {
  const show = comparing && !!$('rdbefore').getAttribute('src');
  for (const id of ['rdbefore', 'rdsplit', 'rdtagbefore', 'rdtagnow']) $(id).hidden = !show;
  if (!show) return;
  const img = $('lbimg');
  const w = img.clientWidth, h = img.clientHeight;
  const x = Math.round(w * split);
  lay($('rdbefore'));
  $('rdbefore').style.clipPath = `inset(0 ${w - x}px 0 0)`;
  const line = $('rdsplit');
  line.style.left = `${img.offsetLeft + x}px`;
  line.style.top = `${img.offsetTop}px`;
  line.style.height = `${h}px`;
  const pad = 8;
  $('rdtagbefore').style.left = `${img.offsetLeft + pad}px`;
  $('rdtagbefore').style.top = `${img.offsetTop + pad}px`;
  $('rdtagnow').style.left = `${img.offsetLeft + w - pad}px`;
  $('rdtagnow').style.top = `${img.offsetTop + pad}px`;
}

function draw() {
  if (frame) return;
  frame = requestAnimationFrame(() => {
    frame = 0;
    const c = $('rdmask');
    const ctx = c.getContext('2d');
    ctx.clearRect(0, 0, c.width, c.height);
    if (!painted || !master) return;
    ctx.drawImage(master, 0, 0, c.width, c.height);
    // Keep the mask's shape, take the colour.
    ctx.globalCompositeOperation = 'source-in';
    ctx.fillStyle = getComputedStyle(document.documentElement).getPropertyValue('--mask').trim();
    ctx.fillRect(0, 0, c.width, c.height);
    ctx.globalCompositeOperation = 'source-over';
  });
}

// One stroke, press to release, drawn into the master as round segments. The
// brush is in screen pixels and converted, so it is the same size on screen in
// Fit and at 1:1. A right drag erases whatever the Eraser says.
function stroke(e) {
  // A pan (Space, middle button) was taken by the pane, which ran first — or
  // was refused because the pane is fitted, and is still not a stroke.
  if (e.defaultPrevented || space) return;
  // Comparing, the pointer moves the split instead: the mask is hidden, and a
  // stroke nobody can see is a stroke nobody meant.
  if (comparing) {
    if (e.button === 0) dragSplit(e);
    return;
  }
  if (!masking() || !master || (e.button !== 0 && e.button !== 2)) return;
  e.preventDefault();
  const img = $('lbimg'), view = $('lbview'), c = $('rdmask');
  const erase = eraser || e.button === 2;
  const ctx = master.getContext('2d');
  const at = (ev) => {
    const r = img.getBoundingClientRect();
    const k = master.width / r.width;
    return [(ev.clientX - r.left) * k, (ev.clientY - r.top) * (master.height / r.height), k];
  };
  let [lx, ly, k] = at(e);
  const seg = (x, y) => {
    ctx.globalCompositeOperation = erase ? 'destination-out' : 'source-over';
    ctx.strokeStyle = '#fff';
    ctx.lineCap = 'round';
    ctx.lineJoin = 'round';
    ctx.lineWidth = Number($('rpbrush').value) * k;
    ctx.beginPath();
    ctx.moveTo(lx, ly);
    ctx.lineTo(x, y);
    ctx.stroke();
    ctx.globalCompositeOperation = 'source-over';
    lx = x; ly = y;
    if (!erase) painted = true;
    draw();
  };
  seg(lx + 0.01, ly);
  c.classList.add('stroking');
  const move = (ev) => { const [x, y] = at(ev); seg(x, y); };
  const up = () => {
    c.classList.remove('stroking');
    view.removeEventListener('pointermove', move);
    view.removeEventListener('pointerup', up);
    view.removeEventListener('pointercancel', up);
    syncButtons();
  };
  view.setPointerCapture(e.pointerId);
  view.addEventListener('pointermove', move);
  view.addEventListener('pointerup', up);
  view.addEventListener('pointercancel', up);
}

// The split follows the pointer from the press — a click jumps it there, a
// drag carries it — clamped to the image.
function dragSplit(e) {
  e.preventDefault();
  const view = $('lbview');
  const at = (ev) => {
    const r = $('lbimg').getBoundingClientRect();
    split = Math.min(1, Math.max(0, (ev.clientX - r.left) / r.width));
    layCompare();
  };
  at(e);
  const up = () => {
    view.removeEventListener('pointermove', at);
    view.removeEventListener('pointerup', up);
    view.removeEventListener('pointercancel', up);
  };
  view.setPointerCapture(e.pointerId);
  view.addEventListener('pointermove', at);
  view.addEventListener('pointerup', up);
  view.addEventListener('pointercancel', up);
}

// Compare needs a second step; with one, there is nothing to put beside it.
const canCompare = () => (shown?.history?.length ?? 0) > 1;

function setCompare(on) {
  comparing = on && canCompare();
  $('rdcompare').classList.toggle('active', comparing);
  $('lbview').classList.toggle('comparing', comparing);
  syncMasking();
  if (comparing) fetchBefore();
  layCompare();
}

// The step on the left, as the page would show it now — the server runs it
// through `publish` with today's filters (`/history/view`), so a sharpened
// "now" is never measured against an unsharpened "before".
async function fetchBefore() {
  if (!comparing || !shown || cmpWith === null) return;
  const key = `${shown.image_id}:${shown.at}:${shown.step}:${cmpWith}`;
  if (key === beforeKey) { layCompare(); return; }
  // Hidden until the new one is in hand: the old left half beside a new right
  // half would be a comparison of the wrong pair, however briefly.
  $('rdbefore').removeAttribute('src');
  layCompare();
  const epoch = ++beforeEpoch;
  try {
    const data = await post('/history/view', { image_id: shown.image_id, index: cmpWith });
    if (epoch !== beforeEpoch) return;
    beforeKey = key;
    $('rdbefore').src = `data:image/png;base64,${data.image_base64}`;
    $('rdtagbefore').textContent = `Before: ${shown.history[cmpWith]}`;
    $('rdtagnow').textContent = `Now: ${shown.history[shown.at]}`;
    layCompare();
  } catch (e) {
    if (epoch !== beforeEpoch) return;
    $('lberr').textContent = String(e.message || e);
    setCompare(false);
  }
}

// The *Compare with* select: every step but the one on screen. The pick is
// kept while it still names another step; otherwise the one just before the
// current, or the next when the current is the original.
function drawCompare() {
  const steps = shown?.history ?? [];
  const ok = canCompare();
  $('rdcmprow').hidden = !ok;
  $('rdcompare').disabled = !ok;
  if (!ok) { if (comparing) setCompare(false); return; }
  if (cmpWith === null || cmpWith === shown.at || cmpWith >= steps.length) {
    cmpWith = shown.at > 0 ? shown.at - 1 : 1;
  }
  $('rdcmp').innerHTML = steps.map((label, i) => (i === shown.at ? ''
    : `<option value="${i}"${i === cmpWith ? ' selected' : ''}>${i + 1}. ${esc(label)}</option>`)).join('');
  if (comparing) fetchBefore();
}

// `passRunning` (jobs.js): the history is locked while a pass runs.

// What the dialog's buttons say and allow, from the mask and the result on
// screen. Painted is "Refine area" (the redraw pass); nothing painted is
// "Refine image" (the refine pass) — the mask decides, so there is no mode to
// remember. A button waiting on its own request is setBusy's until it's back.
function syncButtons() {
  $('rperase').classList.toggle('active', eraser);
  // A painted mask is the Refine panel's "something in effect".
  railDot('refine', painted);
  const live = S.imageId !== null && !!shown;
  syncStrength();
  if (!isBusy('redrawgo')) {
    $('redrawgo').textContent = painted ? 'Refine area' : 'Refine image';
    // The fold's steps, not the image's profile: that box is what the press
    // sends, and it overrides the profile before the server checks.
    const noSteps = passSteps('refine') <= 0;
    $('redrawgo').disabled = !live || (!painted && noSteps);
    $('redrawgo').title = !painted && live && noSteps
      ? 'Refine steps is 0 — set it in the Refine pass fold' : '';
  }
  // Hi-res: on the Hi-res pass panel's steps (a profile can say 0 while the
  // panel says otherwise), and only while the picture is inside the hi-res
  // budget (`can_hires`, Settings' hi-res max side).
  const hf = S.upFactor;
  if (!isBusy('rdhires')) {
    const steps = passSteps('hires') > 0;
    $('rdhires').disabled = !live || !shown.can_hires || !steps;
    $('rdhires').title = !live ? '' : !shown.can_hires ? 'Already at the hi-res size limit (Settings → Hi-res max side)'
      : !steps ? 'Hi-res steps is 0 — set it in the Hi-res pass panel' : '';
  }
  $('rdhiresnote').textContent = live
    ? `${shown.width}×${shown.height} → ${Math.round(shown.width * hf)}×${Math.round(shown.height * hf)}` : '';
  // Edit: only for a model that takes references (the image itself is the
  // first), whole-image like Upscale, and it needs an instruction to follow.
  $('rdfold-edit').hidden = !(live && shown.can_edit);
  $('rdedit').hidden = $('rdfold-edit').hidden;
  if (!isBusy('rdedit')) {
    const said = $('rpprompt').value.trim();
    $('rdedit').textContent = editSides.any() ? 'Edit and rescale' : 'Edit';
    $('rdedit').disabled = !live || painted || !said;
    $('rdedit').title = painted ? 'Clear the Refine fold’s mask to edit the whole image'
      : !said ? 'Type what to change in the prompt box' : '';
  }
  $('rdeditnote').textContent = live ? editSides.note(shown.width, shown.height) : '';
  // Extend: FLUX.1 Fill when one is installed, nothing to pick. Without one the
  // group is greyed out and says where to get it. Whole-image and a new size,
  // so it waits while something is painted, like Upscale.
  const fill = fillReady();
  $('rdneedsfill').hidden = fill;
  extendSides.disable(!fill);
  if (!isBusy('rdextendgo')) {
    $('rdextendgo').disabled = !live || !fill || painted || !extendSides.any();
    $('rdextendgo').title = !fill ? 'Needs a FLUX.1 Fill model (Get models)'
      : painted ? 'Clear the Refine fold’s mask to extend the canvas'
        : !extendSides.any() ? 'Pick a side to extend' : '';
  }
  $('rdextendnote').textContent = live ? extendSides.note(shown.width, shown.height) : '';
  // Scale: the mask scales with the picture (`ensureMaster`), so it needn't
  // wait for one. Down is a step like a crop, locked while a pass runs.
  const f = scaleFactor();
  const to = live && f ? scaledSize(f) : null;
  const byWidth = $('rdfactor').value === 'width';
  $('rdwidth').hidden = !byWidth;
  $('rdupscaler').hidden = byWidth || !(f > 1);
  if (!isBusy('rdupscale')) {
    $('rdupscale').textContent = !f || f === 1 ? 'Scale' : f < 1 ? 'Scale down' : 'Scale up';
    $('rdupscale').disabled = !to || f === 1 || (f < 1 && passRunning());
    $('rdupscale').title = f < 1 && passRunning() ? 'A pass is running on the image — scale when it lands' : '';
  }
  $('rdupnote').textContent = to ? `${shown.width}×${shown.height} → ${to[0]}×${to[1]}` : '';
  // Blank uses: the area its image's own prompt; the whole image the Refine
  // pass fold's prompt when there is one.
  // Extend shares the box but not the rule: its blank continues the background,
  // since the image's own prompt names a subject it would draw again.
  const fold = $('rprompt').value.trim();
  $('rpprompt').placeholder = painted
    ? "blank uses this image's own prompt"
    : !fold
      ? "blank: Refine uses this image's own prompt; Extend continues the background"
      : `blank: Refine uses the Refine pass prompt (${fold}); Extend continues the background`;
  syncDialogSummaries();
  $('rdmodel').disabled = passRunning();
  $('rdcancel').hidden = !passRunning();
}

// Scale's factor: the select's, or the Width box's over the picture's width.
function scaleFactor() {
  if ($('rdfactor').value !== 'width') return Number($('rdfactor').value);
  const w = Number($('rdwidth').value);
  return shown && w > 0 ? w / shown.width : null;
}

// Where the server lands: down, each side snapped to 8 (`resize_size`); up,
// `fsr::out_size`, whichever upscaler.
function scaledSize(f) {
  if (f < 1) return [Math.floor(Math.round(shown.width * f) / 8) * 8, Math.floor(Math.round(shown.height * f) / 8) * 8];
  return [Math.round(shown.width * f), Math.round(shown.height * f)];
}

// Every fold's summary says what is inside, the Image tab's rule, so a folded
// tool still reads: what Refine will do, a rescale picked, the upscale, the
// sides Extend will grow (or that it can't), how many steps there are.
function syncDialogSummaries() {
  const put = (id, text) => { $(id).textContent = text ? `— ${text}` : ''; };
  const strength = $('rpstrength').value;
  put('rdsum-refine', `${painted ? 'area painted' : 'whole image'} · ${strength}`);
  put('rdsum-hires', `${S.upFactor}×`);
  const grown = shown ? editSides.note(shown.width, shown.height).split('→ ')[1] : '';
  put('rdsum-edit', grown ? `rescale → ${grown}` : '');
  const f = scaleFactor();
  const up = f > 1 ? $('rdupscaler').selectedOptions[0]?.textContent ?? '' : '';
  const to = shown && f ? scaledSize(f) : null;
  put('rdsum-upscale', to ? `${to[0]}×${to[1]}${up ? ` · ${up}` : ''}` : '');
  put('rdsum-extend', fillReady() ? extendSides.label() : 'needs FLUX.1 Fill');
}

// The Strength box shows the number for what the button will do. On a switch
// it is labelled first and its value swapped, never reinterpreted: an area at
// 0.5 must not become a whole-image refine at 0.5.
function syncStrength() {
  const mode = painted ? 'area' : 'image';
  if (mode === strengthFor) return;
  strengthFor = mode;
  $('rpstrengthlabel').textContent = painted ? 'Strength (area)' : 'Strength (whole image)';
  $('rpstrength').value = painted ? areaStrength : $('rstrength').value;
}

function takeStrength() {
  const v = $('rpstrength').value;
  if (strengthFor === 'area') {
    areaStrength = Number(v);
  } else {
    // Into the fold, through an `input` event, so its summary and *Save as
    // defaults* hear it the way they hear a typed number.
    $('rstrength').value = v;
    $('rstrength').dispatchEvent(new Event('input', { bubbles: true }));
  }
}

// Every failure in this half lands in the dock's box. Out of memory, it also
// offers to scale the picture down and run the pass again on it: memory grows
// with the picture's area, and the server says as much on a decode failure.
const dialogErr = (m, oom = null, opts = {}) => showError($('lberr'), m, oom, {
  ...opts,
  scaleDown: opts.retry ? async () => { if (await scaleDown(0.75)) opts.retry(); } : null,
});

// A pass in this dialog that failed: plain text, or the out-of-memory box
// whose *Try again* runs `retry` — the same pass, with the Memory select as it
// stands by then.
function passErr(e, retry) {
  const msg = String(e.message || e);
  if (/cancelled by request/.test(msg)) {
    $('lberr').textContent = 'Cancelled. The image is as it was.';
    return;
  }
  dialogErr(msg, e.oom, { ...oomOptions(), retry });
}

// Smaller: a step like a crop, on the display lane (`/resize`), answered
// through the pane's own handler. Says whether it landed.
const scaleDown = (factor) => stepRequest('/resize', { factor });

// Scale. Down is `scaleDown`; up is no diffusion either, but through the queue
// like the passes — a model runs on the GPU, an ESRGAN in sd-cli — and back as
// an ordinary result.
async function scale() {
  const f = scaleFactor();
  if (S.imageId === null || !f || f === 1) return;
  if (f < 1) {
    setBusy('rdupscale', 'Scaling…');
    try { await scaleDown(f); } finally { setIdle('rdupscale'); syncButtons(); }
    return;
  }
  $('lberr').textContent = '';
  setBusy('rdupscale', 'Upscaling…', 'upscale');
  soonJobs();
  const land = passLander();
  try {
    land(await post('/upscale', {
      image_id: S.imageId,
      factor: f,
      upscaler: $('rdupscaler').value || null,
    }));
  } catch (e) {
    const msg = String(e.message || e);
    $('lberr').textContent = /cancelled by request/.test(msg)
      ? 'Cancelled. The image is as it was.' : msg;
  } finally {
    setIdle('rdupscale');
    syncButtons();
  }
}

// Change the whole picture by instruction: the image itself is reference 1 on
// the server, and any picks in the References dialog follow it — so the prompt
// names those "image 2" onwards.
async function edit() {
  const said = $('rpprompt').value.trim();
  if (S.imageId === null || painted || !said) return;
  $('lberr').textContent = '';
  setBusy('rdedit', editSides.any() ? 'Editing and rescaling…' : 'Editing…', 'edit');
  soonJobs();
  const land = passLander();
  try {
    land(await post('/edit', {
      image_id: S.imageId, prompt: said, references: references(), memory: $('memory').value || null,
      expand: editSides.expand(),
    }));
  } catch (e) {
    passErr(e, edit);
  } finally {
    setIdle('rdedit');
    syncButtons();
  }
}

// Grow the canvas on the chosen sides and draw beyond them with FLUX.1 Fill,
// with this dialog's prompt (blank continues the background) and softness.
async function extend() {
  if (S.imageId === null || painted || !extendSides.any() || !fillReady()) return;
  $('lberr').textContent = '';
  setBusy('rdextendgo', 'Extending…', 'extend');
  soonJobs();
  const land = passLander();
  try {
    land(await post('/extend', {
      image_id: S.imageId,
      expand: extendSides.expand(),
      prompt: $('rpprompt').value.trim() || null,
      feather: Number($('rpsoft').value),
      memory: $('memory').value || null,
    }));
  } catch (e) {
    passErr(e, extend);
  } finally {
    setIdle('rdextendgo');
    syncButtons();
  }
}

// The whole-image refine goes through image.js's `escalate`, which keeps the
// stale-image reopen and the busy label; this only adds the dialog's prompt.
async function refineImage() {
  await refine($('rpprompt').value.trim() || null, dialogErr, passLander());
  syncButtons();
}

// What the server reads: black, white where painted, at the image's own size.
function maskPng() {
  const c = document.createElement('canvas');
  c.width = master.width;
  c.height = master.height;
  const ctx = c.getContext('2d');
  ctx.fillStyle = '#000';
  ctx.fillRect(0, 0, c.width, c.height);
  ctx.drawImage(master, 0, 0);
  return c.toDataURL('image/png');
}

async function redraw() {
  if (!painted || S.imageId === null || !master) return;
  $('lberr').textContent = '';
  setBusy('redrawgo', 'Refining area…', 'redraw');
  soonJobs();
  const land = passLander();
  try {
    land(await post('/redraw', {
      image_id: S.imageId,
      mask: maskPng(),
      prompt: $('rpprompt').value.trim() || null,
      strength: numOr('rpstrength'),
      feather: Number($('rpsoft').value),
      memory: $('memory').value || null,
    }));
  } catch (e) {
    passErr(e, redraw);
  } finally {
    setIdle('redrawgo');
    syncButtons();
  }
}

// The *Refine with* row: the Image tab's own model list (the generating
// select's options, so the two can't disagree about what is pickable), set to
// the model the image's passes run on now.
function drawModel() {
  $('rdmodelrow').hidden = !pickModel;
  if (!pickModel || !shown) return;
  const sel = $('rdmodel');
  sel.innerHTML = $('template').innerHTML;
  sel.value = shown.model;
}

// Picking one changes no pixels; the server swaps the image's profile and
// answers with what the passes can now do (Refine, Hi-res, Edit).
async function setModel() {
  if (!shown) return;
  $('lberr').textContent = '';
  try {
    deliver(await post('/image/model', { image_id: shown.image_id, model: $('rdmodel').value }));
  } catch (e) {
    $('lberr').textContent = String(e.message || e);
    $('rdmodel').value = shown.model;
  }
}

function drawDialog() {
  if (!shown) return;
  drawModel();
  drawCompare();
  syncButtons();
}

// Every state the dialog is handed (viewer.js's `onLook`): a new image, a pass,
// a step back, a crop, a scale. The mask goes with a different image or a new
// shape (`ensureMaster`).
function syncRedraw(data) {
  if (data.image_id !== maskFor) {
    maskFor = data.image_id;
    $('rpprompt').value = '';
    // The sides too: a rescale picked for the last picture would turn this
    // one's Edit into "Edit and rescale" unasked.
    extendSides?.clear();
    editSides?.clear();
    pickModel = false;
    if (master) clearMask();
  }
  ensureMaster(data.width, data.height);
  shown = data;
  if (open()) drawDialog();
}

// The dialog is opening (viewer.js's `onOpen`), on the state `onLook` handed.
function prepareOpen() {
  pickModel = S.pane === 'outputs';
  // The shared factor, as the Hi-res pass panel last set it.
  $('upfactor').innerHTML = upOptions();
  // The sliders may have moved since: Compare's left side is fetched again.
  beforeKey = null;
  fillUpscalers();
  // Re-read the Strength box from scratch: the fold's may have changed since.
  strengthFor = null;
  syncMasking();
  drawDialog();
}

// The pane says a left drag paints only while the Refine fold has the pointer.
function syncMasking() {
  $('lbview').classList.toggle('masking', masking() && !comparing);
}

// The upscalers the page knows (FSR plus every runnable model), kept on the
// one picked when it is still offered. Filled on open, since the listing
// arrives after boot and a model can be added while the server runs.
async function fillUpscalers() {
  await upscalersReady;
  const sel = $('rdupscaler');
  const keep = sel.value;
  sel.innerHTML = upscalerOptions;
  if ([...sel.options].some((o) => o.value === keep && !o.disabled)) sel.value = keep;
}

function syncVals() {
  $('rpsoftval').textContent = `${$('rpsoft').value} px`;
  $('rpbrushval').textContent = `${$('rpbrush').value} px`;
}

export function initRedraw() {
  extendSides = sideSet($('rdextendsides'), syncButtons);
  editSides = sideSet($('rdeditsides'), syncButtons);
  onLook(syncRedraw);
  onOpen(prepareOpen);
  // A plain left drag is the mask's while its fold is open, Compare's split
  // while comparing; Space hands it back to the pan.
  claimLeft(() => !space && (comparing || masking()));
  $('rdfold-refine').addEventListener('toggle', syncMasking);
  $('panel-refine').addEventListener('toggle', syncMasking);
  $('rdextendgo').addEventListener('click', extend);
  $('rdmodel').addEventListener('change', setModel);
  $('rpstrength').addEventListener('input', syncDialogSummaries);
  $('rdupscaler').addEventListener('change', syncDialogSummaries);
  $('rdgetfill').addEventListener('click', () => { if (closeViewer()) openGetModels('flux_fill'); });
  // After the pane's own listener (viewer.js's initViewer runs first), so a
  // pan it took is `defaultPrevented` here.
  $('lbview').addEventListener('pointerdown', stroke);
  $('lbview').addEventListener('contextmenu', (e) => e.preventDefault());
  new ResizeObserver(place).observe($('lbimg'));
  $('lbimg').addEventListener('load', place);

  // Space to pan and Ctrl+Z to step back — but never from inside the prompt,
  // where both already mean something.
  const typing = (e) => /^(TEXTAREA|INPUT)$/.test(e.target.tagName);
  document.addEventListener('keydown', (e) => {
    if (!open() || typing(e)) return;
    if (e.code === 'Space') {
      space = true;
      $('lbview').classList.add('panning');
      e.preventDefault();
    } else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'z' && shown) {
      e.preventDefault();
      stepTo(shown.at + (e.shiftKey ? 1 : -1));
    } else if (e.key.toLowerCase() === 'c' && !e.ctrlKey && !e.metaKey && !e.altKey) {
      e.preventDefault();
      setCompare(!comparing);
    }
  });
  document.addEventListener('keyup', (e) => {
    if (e.code !== 'Space') return;
    space = false;
    $('lbview').classList.remove('panning');
  });

  $('rdcompare').addEventListener('click', () => setCompare(!comparing));
  $('rdcmp').addEventListener('change', () => { cmpWith = Number($('rdcmp').value); fetchBefore(); });
  $('rperase').addEventListener('click', () => { eraser = !eraser; syncButtons(); });
  $('rpclear').addEventListener('click', clearMask);
  $('redrawgo').addEventListener('click', () => (painted ? redraw() : refineImage()));
  $('rpstrength').addEventListener('input', takeStrength);
  $('rdupscale').addEventListener('click', scale);
  $('rdhires').addEventListener('click', async () => {
    await hiresPass(dialogErr, passLander());
    syncButtons();
  });
  $('upfactor').addEventListener('change', (e) => { setUpFactor(parseFloat(e.target.value)); syncButtons(); });
  $('rdedit').addEventListener('click', edit);
  // Edit waits for an instruction; the button follows the box as it's typed.
  $('rpprompt').addEventListener('input', syncButtons);
  $('rdfactor').addEventListener('change', () => {
    // Width… starts at the picture's own, so the box says what it changes.
    if ($('rdfactor').value === 'width' && shown && !$('rdwidth').value) $('rdwidth').value = shown.width;
    syncButtons();
  });
  $('rdwidth').addEventListener('input', syncButtons);
  // The fold's strength or prompt edited with the dialog shut: picked up on
  // the next open, since `syncStrength` only re-reads on a switch.
  $('rstrength').addEventListener('input', () => {
    if (strengthFor === 'image' && document.activeElement !== $('rpstrength')) {
      $('rpstrength').value = $('rstrength').value;
    }
  });
  // setIdle puts back a fixed label; this button's depends on the mask.
  onIdle(() => { if (open()) syncButtons(); });
  // Whatever pass the button is shown for: the same test decides both, so the
  // button can't again appear for a pass it doesn't stop. Each is one sd-cli
  // run, so stopping it is killing it (`Stopped::Cancel`).
  $('rdcancel').addEventListener('click', async () => {
    const j = runningJob();
    if (!j || !passRunning()) return;
    // Straight to the route rather than `stopJob`, which keeps its failures
    // quiet: an FSR upscale can't be stopped part-way, and the server says so.
    try {
      await post('/jobs/stop', { id: j.id });
    } catch (e) {
      $('lberr').textContent = String(e.message || e);
    }
    soonJobs();
  });
  $('rpsoft').addEventListener('input', () => { syncVals(); place(); });
  $('rpbrush').addEventListener('input', syncVals);
  syncVals();
  onJobs(() => { if (open()) syncButtons(); });
}
