import { $, esc, numOr, post } from './dom.js';
import { S } from './state.js';
import { setBusy, setIdle } from './busy.js';
import { onJobs, runningJob, soonJobs, stopJob } from './jobs.js';
import { zoomPane } from './viewer.js';
import { takeResult } from './image.js';

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
// A redraw request is out: the button is setBusy's until it comes back.
let waiting = false;
// The result the dialog is showing: its id, size, history and step.
let shown = null;
let pane = null;
let frame = 0;

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

// Lay the mask's view over the image: same box, in the pane's coordinates, so
// it scrolls with the picture at 1:1. The CSS blur previews the edge softness
// at the scale the image is displayed.
function place() {
  const img = $('rdimg'), c = $('rdmask');
  const w = img.clientWidth, h = img.clientHeight;
  if (!w || !master) return;
  c.style.left = `${img.offsetLeft}px`;
  c.style.top = `${img.offsetTop}px`;
  c.style.width = `${w}px`;
  c.style.height = `${h}px`;
  const dpr = window.devicePixelRatio || 1;
  c.width = Math.max(1, Math.round(Math.min(w * dpr, master.width)));
  c.height = Math.max(1, Math.round(Math.min(h * dpr, master.height)));
  const scale = w / master.width;
  c.style.filter = `blur(${(Number($('rpsoft').value) * scale) / 2}px)`;
  draw();
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
  if (e.defaultPrevented || space || !master || (e.button !== 0 && e.button !== 2)) return;
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

// Whether a pass on some image is running. The history is locked meanwhile: a
// step back and a pass that lands afterwards would disagree about which state
// is current, and the pass would win without saying so.
const passRunning = () => ['refine', 'hires', 'redraw'].includes(runningJob()?.kind);

function syncButtons() {
  $('rperase').classList.toggle('active', eraser);
  if (!waiting) $('redrawgo').disabled = !painted || S.imageId === null;
  const j = runningJob();
  $('rdcancel').hidden = !(j && j.kind === 'redraw');
  for (const b of $('rdhistory').querySelectorAll('button')) b.disabled = passRunning();
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
  waiting = true;
  setBusy('redrawgo', 'Redrawing…', 'redraw');
  soonJobs();
  try {
    takeResult(await post('/redraw', {
      image_id: S.imageId,
      mask: maskPng(),
      prompt: $('rpprompt').value.trim() || null,
      strength: numOr('rpstrength'),
      feather: Number($('rpsoft').value),
    }));
  } catch (e) {
    const msg = String(e.message || e);
    $('rperr').textContent = /cancelled by request/.test(msg)
      ? 'Cancelled. The image is as it was.' : msg;
  } finally {
    setIdle('redrawgo');
    waiting = false;
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
  syncRedraw(data);
  $('rperr').textContent = '';
  $('redraw').showModal();
  pane.expectNew();
  $('rdimg').src = S.currentSrc;
  pane.shown();
  drawHistory();
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
  $('rperase').addEventListener('click', () => { eraser = !eraser; syncButtons(); });
  $('rpclear').addEventListener('click', clearMask);
  $('redrawgo').addEventListener('click', redraw);
  $('rdcancel').addEventListener('click', () => {
    const j = runningJob();
    if (j && j.kind === 'redraw') stopJob(j.id);
  });
  $('rdclose').addEventListener('click', () => $('redraw').close());
  $('rpsoft').addEventListener('input', () => { syncVals(); place(); });
  $('rpbrush').addEventListener('input', syncVals);
  syncVals();
  onJobs(() => { if (open()) syncButtons(); });
}
