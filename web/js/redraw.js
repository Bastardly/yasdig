import { $, esc, numOr, post } from './dom.js';
import { S } from './state.js';
import { isBusy, onIdle, setBusy, setIdle } from './busy.js';
import { onJobs, runningJob, soonJobs } from './jobs.js';
import { zoomPane } from './viewer.js';
import { oomOptions, refine, takeResult, upscalerOptions, upscalersReady } from './image.js';
import { showError } from './memory.js';
import { references } from './refs.js';

// Redraw: paint over part of the Image tab's result and redraw just that part
// (`POST /redraw`, `Engine::redraw`), in a dialog of its own — the viewer's
// layout, because a large image wants 1:1 — with the live preview and the
// image's history beside it.
//
// The mask is `master`, an offscreen canvas at the image's natural size, white
// where painted: that is what gets sent. `#rdmask` is the view of it, laid over
// `#rdimg` at the image's displayed size rather than wrapping it, so the pane's
// fit/centre rules (`zoomPane`) stay as they are. Display-sized, not natural:
// a 3072² image would otherwise be a second 36 MB canvas for one mask.

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
let pane = null;
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

const open = () => $('redraw').open;

function ensureMaster(w, h) {
  if (master && master.width === w && master.height === h) return;
  master = document.createElement('canvas');
  master.width = w;
  master.height = h;
  painted = false;
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
  const img = $('rdimg');
  el.style.left = `${img.offsetLeft}px`;
  el.style.top = `${img.offsetTop}px`;
  el.style.width = `${img.clientWidth}px`;
  el.style.height = `${img.clientHeight}px`;
}

// The mask's view. Display-sized; the CSS blur previews the edge softness at
// the scale the image is shown at.
function place() {
  const img = $('rdimg'), c = $('rdmask');
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
  const img = $('rdimg');
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
  if (!master || (e.button !== 0 && e.button !== 2)) return;
  e.preventDefault();
  const img = $('rdimg'), view = $('rdview'), c = $('rdmask');
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
  const view = $('rdview');
  const at = (ev) => {
    const r = $('rdimg').getBoundingClientRect();
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
  $('rdview').classList.toggle('comparing', comparing);
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
    $('rperr').textContent = String(e.message || e);
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

// Whether a pass on some image is running. The history is locked meanwhile: a
// step back and a pass that lands afterwards would disagree about which state
// is current, and the pass would win without saying so.
const PASSES = ['refine', 'hires', 'redraw', 'upscale', 'edit'];
const passRunning = () => PASSES.includes(runningJob()?.kind);

// What the dialog's buttons say and allow, from the mask and the result on
// screen. Painted is "Refine area" (the redraw pass); nothing painted is
// "Refine image" (the refine pass) — the mask decides, so there is no mode to
// remember. A button waiting on its own request is setBusy's until it's back.
function syncButtons() {
  $('rperase').classList.toggle('active', eraser);
  const live = S.imageId !== null && !!shown;
  syncStrength();
  if (!isBusy('redrawgo')) {
    $('redrawgo').textContent = painted ? 'Refine area' : 'Refine image';
    $('redrawgo').disabled = !live || (!painted && !shown.can_refine);
    $('redrawgo').title = !painted && live && !shown.can_refine
      ? 'This model has no refine pass (Refine steps is 0)' : '';
  }
  // Edit: only for a model that takes references (the image itself is the
  // first), whole-image like Upscale, and it needs an instruction to follow.
  $('rdedit').hidden = !(live && shown.can_edit);
  if (!isBusy('rdedit')) {
    const said = $('rpprompt').value.trim();
    $('rdedit').disabled = !live || painted || !said;
    $('rdedit').title = painted ? 'Clear the mask to edit the whole image'
      : !said ? 'Type what to change in the prompt box' : '';
  }
  // Upscale is whole-image and changes the size the mask was painted at, so it
  // waits while something is painted, as a refine of the whole image would.
  if (!isBusy('rdupscale')) {
    $('rdupscale').disabled = !live || painted;
    $('rdupscale').title = painted ? 'Clear the mask to upscale the whole image' : '';
  }
  if (live) {
    // The size the server will land on: `fsr::out_size`, whichever upscaler.
    const f = Number($('rdfactor').value);
    $('rdupnote').textContent =
      `${shown.width}×${shown.height} → ${Math.round(shown.width * f)}×${Math.round(shown.height * f)}`;
  }
  // Blank uses: the area its image's own prompt; the whole image the Refine
  // pass fold's prompt when there is one.
  const fold = $('rprompt').value.trim();
  $('rpprompt').placeholder = painted || !fold
    ? "blank uses this image's own prompt"
    : `blank uses the Refine pass prompt: ${fold}`;
  $('rdcancel').hidden = !passRunning();
  for (const b of $('rdhistory').querySelectorAll('button')) b.disabled = passRunning();
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

const dialogErr = (m, oom = null, opts = {}) => showError($('rperr'), m, oom, opts);

// A pass in this dialog that failed: plain text, or the out-of-memory box
// whose *Try again* runs `retry` — the same pass, with the Memory select as it
// stands by then.
function passErr(e, retry) {
  const msg = String(e.message || e);
  if (/cancelled by request/.test(msg)) {
    $('rperr').textContent = 'Cancelled. The image is as it was.';
    return;
  }
  showError($('rperr'), msg, e.oom, { ...oomOptions(), retry });
}

// Bigger, with no diffusion. Through the queue like the passes — a model runs
// on the GPU, an ESRGAN in sd-cli — and back as an ordinary result.
async function upscale() {
  if (S.imageId === null || painted) return;
  $('rperr').textContent = '';
  setBusy('rdupscale', 'Upscaling…', 'upscale');
  soonJobs();
  try {
    takeResult(await post('/upscale', {
      image_id: S.imageId,
      factor: Number($('rdfactor').value),
      upscaler: $('rdupscaler').value || null,
    }));
  } catch (e) {
    const msg = String(e.message || e);
    $('rperr').textContent = /cancelled by request/.test(msg)
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
  $('rperr').textContent = '';
  setBusy('rdedit', 'Editing…', 'edit');
  soonJobs();
  try {
    takeResult(await post('/edit', {
      image_id: S.imageId, prompt: said, references: references(), memory: $('memory').value || null,
    }));
  } catch (e) {
    passErr(e, edit);
  } finally {
    setIdle('rdedit');
    syncButtons();
  }
}

// The whole-image refine goes through image.js's `escalate`, which keeps the
// stale-image reopen and the busy label; this only adds the dialog's prompt.
async function refineImage() {
  await refine($('rpprompt').value.trim() || null, dialogErr);
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
  $('rperr').textContent = '';
  setBusy('redrawgo', 'Refining area…', 'redraw');
  soonJobs();
  try {
    takeResult(await post('/redraw', {
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

async function goto(index) {
  if (!shown || index < 0 || index >= shown.history.length || index === shown.at) return;
  if (passRunning()) return;
  $('rperr').textContent = '';
  try {
    takeResult(await post('/history/goto', { image_id: shown.image_id, index }));
  } catch (e) {
    $('rperr').textContent = String(e.message || e);
  }
}

function drawHistory() {
  if (!shown) return;
  const steps = shown.history ?? [];
  $('rdhistory').innerHTML = steps.map((label, i) => `
    <li><button type="button" data-step="${i}" class="${i === shown.at ? 'active' : ''}">
      <img src="/history/${shown.image_id}/${i}.png" alt="" loading="lazy">
      <span>${i + 1}. ${esc(label)}</span>
    </button></li>`).reverse().join('');
  drawCompare();
  syncButtons();
}

// Called by render() with every result the Image tab shows: a new image, a
// redraw, a refine, a step back. The dialog follows it while open; the mask goes
// only with a different image or a different size (a hi-res pass).
export function syncRedraw(data) {
  if (data.image_id !== maskFor) {
    maskFor = data.image_id;
    $('rpprompt').value = '';
    if (master) clearMask();
  }
  const fresh = !shown || shown.image_id !== data.image_id;
  ensureMaster(data.width, data.height);
  shown = data;
  if (!open()) return;
  // The same image redrawn keeps the view where it was — the point of panning
  // to a hand is to see the new one land there.
  if (fresh) pane.expectNew(); else pane.keepView();
  $('rdimg').src = S.currentSrc;
  drawHistory();
}

export function openRedraw(data) {
  shown = null;
  // The viewer's sliders may have moved since: the left side is fetched again.
  beforeKey = null;
  fillUpscalers();
  // Re-read the Strength box from scratch: the fold's may have changed since.
  strengthFor = null;
  syncRedraw(data);
  $('rperr').textContent = '';
  $('redraw').showModal();
  pane.expectNew();
  $('rdimg').src = S.currentSrc;
  pane.shown();
  drawHistory();
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
  pane = zoomPane({
    view: $('rdview'), img: $('rdimg'), fit: $('rdfit'), full: $('rdfull'),
    pans: (e) => e.button === 1 || (e.button === 0 && space),
  });
  // After the pane's own listener, so a pan it took is `defaultPrevented` here.
  $('rdview').addEventListener('pointerdown', stroke);
  $('rdview').addEventListener('contextmenu', (e) => e.preventDefault());
  new ResizeObserver(place).observe($('rdimg'));
  $('rdimg').addEventListener('load', place);

  // Space to pan and Ctrl+Z to step back — but never from inside the prompt,
  // where both already mean something.
  const typing = (e) => /^(TEXTAREA|INPUT)$/.test(e.target.tagName);
  document.addEventListener('keydown', (e) => {
    if (!open() || typing(e)) return;
    if (e.code === 'Space') {
      space = true;
      $('rdview').classList.add('panning');
      e.preventDefault();
    } else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'z' && shown) {
      e.preventDefault();
      goto(shown.at + (e.shiftKey ? 1 : -1));
    } else if (e.key.toLowerCase() === 'c' && !e.ctrlKey && !e.metaKey && !e.altKey) {
      e.preventDefault();
      setCompare(!comparing);
    }
  });
  document.addEventListener('keyup', (e) => {
    if (e.code !== 'Space') return;
    space = false;
    $('rdview').classList.remove('panning');
  });

  $('rdhistory').addEventListener('click', (e) => {
    const b = e.target.closest('[data-step]');
    if (b && !b.disabled) goto(Number(b.dataset.step));
  });
  $('rdcompare').addEventListener('click', () => setCompare(!comparing));
  $('rdcmp').addEventListener('change', () => { cmpWith = Number($('rdcmp').value); fetchBefore(); });
  $('rperase').addEventListener('click', () => { eraser = !eraser; syncButtons(); });
  $('rpclear').addEventListener('click', clearMask);
  $('redrawgo').addEventListener('click', () => (painted ? redraw() : refineImage()));
  $('rpstrength').addEventListener('input', takeStrength);
  $('rdupscale').addEventListener('click', upscale);
  $('rdedit').addEventListener('click', edit);
  // Edit waits for an instruction; the button follows the box as it's typed.
  $('rpprompt').addEventListener('input', syncButtons);
  $('rdfactor').addEventListener('change', syncButtons);
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
    if (!j || !PASSES.includes(j.kind)) return;
    // Straight to the route rather than `stopJob`, which keeps its failures
    // quiet: an FSR upscale can't be stopped part-way, and the server says so.
    try {
      await post('/jobs/stop', { id: j.id });
    } catch (e) {
      $('rperr').textContent = String(e.message || e);
    }
    soonJobs();
  });
  $('rdclose').addEventListener('click', () => $('redraw').close());
  $('rpsoft').addEventListener('input', () => { syncVals(); place(); });
  $('rpbrush').addEventListener('input', syncVals);
  syncVals();
  onJobs(() => { if (open()) syncButtons(); });
}
