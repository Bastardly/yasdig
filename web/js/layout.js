import { $, esc, followPointer } from './dom.js';

// ---- Layout boxes (MOSS-Image) ----------------------------------------------
//
// MOSS-Image reads a JSON layout as its prompt: the scene caption (the Prompt
// box) and boxes, each saying what is in it and where — and, for text, the
// exact words. The server writes that JSON (`src/layout.rs`), per pass and at
// that pass's size, since a box is pixels of the canvas being drawn; this page
// only draws the boxes, as fractions of the frame, and sends them.
//
// Where they are drawn: on the stage, which a *Picture | Layout* switch above
// it turns into a frame at the Generation panel's shape, with the last picture
// dimmed under it when its shape matches. Generate lands a picture and the
// stage goes back to it — the switch brings the boxes back over it, to see
// where things landed and move them. The Layout panel lists the boxes, with
// what each is.
//
// Only a family that reads a layout (`layout` on /models) shows any of it; the
// boxes are kept for when one is picked again, and not sent until then, as
// References' picks are.

// The smallest side a box may have, as a fraction of the frame: smaller is a
// click, not a drag, and a box the model can't place.
export const MIN_SIDE = 0.02;
export const LABELS = [['object', 'Object'], ['character', 'Character'], ['text', 'Text']];

const clamp = (v, lo = 0, hi = 1) => Math.min(hi, Math.max(lo, v));

// The box two points span, inside the frame, whichever way it was dragged.
export function rectFrom([ax, ay], [bx, by]) {
  return [clamp(Math.min(ax, bx)), clamp(Math.min(ay, by)), clamp(Math.max(ax, bx)), clamp(Math.max(ay, by))];
}

// `r` moved by (dx, dy), its size kept, stopped at the frame's edges.
export function moved([x0, y0, x1, y1], dx, dy) {
  const w = x1 - x0, h = y1 - y0;
  const nx = clamp(x0 + dx, 0, 1 - w), ny = clamp(y0 + dy, 0, 1 - h);
  return [nx, ny, nx + w, ny + h];
}

// `r` with the corner `handle` ('nw', 'ne', 'sw', 'se') dragged to (x, y),
// never smaller than MIN_SIDE and never turned inside out.
export function resized([x0, y0, x1, y1], handle, x, y) {
  x = clamp(x);
  y = clamp(y);
  if (handle.includes('w')) x0 = Math.min(x, x1 - MIN_SIDE);
  else x1 = Math.max(x, x0 + MIN_SIDE);
  if (handle.includes('n')) y0 = Math.min(y, y1 - MIN_SIDE);
  else y1 = Math.max(y, y0 + MIN_SIDE);
  return [clamp(x0), clamp(y0), clamp(x1), clamp(y1)];
}

// What the server sends the model for `r` on a w×h canvas (`layout::bbox`):
// [y0, x0, y1, x1] in pixels from the centre, near edges floored and far ones
// ceiled. For the box's tooltip, and pinned to the server's numbers by
// layout.test.js.
export function bboxOf([x0, y0, x1, y1], w, h) {
  const px = (v, side) => Math.round(v * side * 1e9) / 1e9;
  const hw = Math.floor(w / 2), hh = Math.floor(h / 2);
  const b = [Math.floor(px(y0, h)) - hh, Math.floor(px(x0, w)) - hw, Math.ceil(px(y1, h)) - hh, Math.ceil(px(x1, w)) - hw];
  b[2] = Math.max(b[2], b[0] + 1);
  b[3] = Math.max(b[3], b[1] + 1);
  return b;
}

// ---- State ------------------------------------------------------------------

let items = [];
let sel = -1;
let enabled = false;
let mode = 'picture';
// Set by `initLayout`: the size the next Generate draws at ([w, h]), the
// picture to dim under the frame ({src, w, h} or null), and what to call after
// any change.
let hooks = { size: () => [1024, 1024], picture: () => null, onChange: () => {} };
const aspect = () => {
  const [w, h] = hooks.size();
  return w > 0 && h > 0 ? w / h : 1;
};

const frame = () => $('lyframe');

function changed() {
  drawBoxes();
  syncSummary();
  hooks.onChange();
}

// What Generate sends: the boxes, to a family that reads them — or nothing.
export function layoutPayload() {
  if (!enabled || !items.length) return null;
  return { items: items.map((it) => ({
    label: it.label,
    caption: it.caption,
    ...(it.label === 'text' ? { text: it.text } : {}),
    rect: it.rect,
  })) };
}

// A reopened work item's boxes, or none.
export function setLayout(layout) {
  items = (layout?.items ?? []).map((it) => ({
    label: it.label, caption: it.caption ?? '', text: it.text ?? '', rect: [...it.rect],
  }));
  sel = -1;
  drawRows();
  changed();
}

// The model changed: show the panel, the rail button and the switch only for a
// family that reads layouts. The boxes stay either way.
export function syncLayout(takes) {
  enabled = !!takes;
  const button = document.querySelector('[data-panel="img-layout"]');
  if (button) button.hidden = !enabled;
  $('stagemode').hidden = !enabled;
  if (!enabled) {
    if ($('panel-img-layout').open) $('panel-img-layout').open = false;
    setMode('picture');
  }
  syncSummary();
}

// A picture landed: show it. The switch brings the boxes back.
export function showPicture() {
  setMode('picture');
}

function setMode(m) {
  mode = enabled ? m : 'picture';
  $('layoutstage').hidden = mode !== 'layout';
  for (const b of $('stagemode').querySelectorAll('[data-mode]')) {
    b.classList.toggle('active', b.dataset.mode === mode);
  }
  if (mode === 'layout') placeFrame();
}

function syncSummary() {
  const n = items.length;
  const el = $('sum-layout');
  el.textContent = n ? `— ${n} box${n > 1 ? 'es' : ''}` : '— none';
  el.title = items.map((it, i) => `${i + 1}. ${it.label}: ${it.caption}`).join('\n');
  // Boxes change what the model is told: a departure from the plain path,
  // which the rail's dot carries.
  el.classList.toggle('on', enabled && n > 0);
}

// ---- The frame ----------------------------------------------------------------

// The frame at the Generation panel's shape, as large as the stage allows, and
// the last picture under it when it has that shape (within 1%).
function placeFrame() {
  const host = $('layoutstage');
  const room = (v) => Math.max(0, v - 64);
  const W = room(host.clientWidth), H = room(host.clientHeight);
  const a = aspect();
  let w = W, h = W / a;
  if (h > H) {
    h = H;
    w = H * a;
  }
  frame().style.width = `${Math.round(w)}px`;
  frame().style.height = `${Math.round(h)}px`;
  const pic = hooks.picture();
  const img = $('lyimg');
  img.hidden = !(pic && Math.abs(pic.w / pic.h - a) / a < 0.01);
  if (!img.hidden && img.getAttribute('src') !== pic.src) img.src = pic.src;
  drawBoxes();
}

const pct = (v) => `${(v * 100).toFixed(3)}%`;

function drawBoxes() {
  const f = frame();
  for (const old of f.querySelectorAll('.lybox')) old.remove();
  // The tooltip's pixels, at the size the next Generate draws at.
  const [w, h] = hooks.size();
  items.forEach((it, i) => {
    const [x0, y0, x1, y1] = it.rect;
    const b = document.createElement('div');
    b.className = `lybox ly-${it.label}${i === sel ? ' sel' : ''}`;
    b.dataset.i = i;
    b.style.left = pct(x0);
    b.style.top = pct(y0);
    b.style.width = pct(x1 - x0);
    b.style.height = pct(y1 - y0);
    const label = LABELS.find(([v]) => v === it.label)?.[1] ?? it.label;
    const what = it.label === 'text' && it.text ? `“${it.text}”` : it.caption;
    b.title = `${i + 1}. ${label}: ${it.caption || '(no caption yet)'}`
      + `\nSent as [${bboxOf(it.rect, w, h).join(', ')}] at ${w}×${h}: y0, x0, y1, x1 from the centre`;
    b.innerHTML = `<span class="lytag">${i + 1} · ${esc(label)}</span>`
      + (what ? `<span class="lywhat">${esc(what)}</span>` : '')
      + ['nw', 'ne', 'sw', 'se'].map((h) => `<i class="lyh lyh-${h}" data-h="${h}"></i>`).join('');
    f.appendChild(b);
  });
}

// The pointer as a fraction of the frame.
function at(e) {
  const r = frame().getBoundingClientRect();
  return [(e.clientX - r.left) / r.width, (e.clientY - r.top) / r.height];
}

function select(i, focus = false) {
  sel = i;
  drawBoxes();
  for (const row of $('lyrows').querySelectorAll('.lyrow')) {
    row.classList.toggle('sel', Number(row.dataset.i) === sel);
  }
  if (focus && sel >= 0) {
    $('panel-img-layout').open = true;
    $('lyrows').querySelector(`.lyrow[data-i="${sel}"] [data-k="caption"]`)?.focus();
  }
}

function onPointerDown(e) {
  if (e.button !== 0) return;
  e.preventDefault();
  const handle = e.target.closest('[data-h]');
  const box = e.target.closest('.lybox');
  const p0 = at(e);
  if (box) {
    const i = Number(box.dataset.i);
    select(i);
    const start = [...items[i].rect];
    followPointer(frame(), e, (ev) => {
      const p = at(ev);
      items[i].rect = handle ? resized(start, handle.dataset.h, ...p) : moved(start, p[0] - p0[0], p[1] - p0[1]);
      drawBoxes();
    }, changed);
    return;
  }
  // On the frame itself: a new box, drawn by the drag. Too small to be one
  // when let go (a click) and it is dropped — a click deselects.
  const it = { label: 'object', caption: '', text: '', rect: rectFrom(p0, p0) };
  items.push(it);
  const i = items.length - 1;
  sel = i;
  followPointer(frame(), e, (ev) => {
    it.rect = rectFrom(p0, at(ev));
    drawBoxes();
  }, () => {
    const [x0, y0, x1, y1] = it.rect;
    if (x1 - x0 < MIN_SIDE || y1 - y0 < MIN_SIDE) {
      items.splice(i, 1);
      sel = -1;
      changed();
      return;
    }
    drawRows();
    changed();
    select(i, true);
  });
}

// ---- The panel's rows -----------------------------------------------------------

function drawRows() {
  const el = $('lyrows');
  if (!items.length) {
    el.innerHTML = '<p class="sub">No boxes yet. Switch the picture to <b>Layout</b> and drag on it to draw one.</p>';
    return;
  }
  el.innerHTML = items.map((it, i) => `
    <div class="lyrow${i === sel ? ' sel' : ''}" data-i="${i}">
      <div class="lyrowhead">
        <b>${i + 1}</b>
        <select data-k="label" aria-label="What box ${i + 1} is">${LABELS.map(([v, l]) =>
          `<option value="${v}"${v === it.label ? ' selected' : ''}>${l}</option>`).join('')}</select>
        <button type="button" class="btn btn-danger" data-del title="Remove this box">&times;</button>
      </div>
      <input type="text" data-k="caption" value="${esc(it.caption)}"
        placeholder="${it.label === 'text' ? 'How it looks: large white serif title' : 'What is in it'}"
        aria-label="Caption of box ${i + 1}">
      <input type="text" data-k="text" value="${esc(it.text)}" placeholder="The exact words"
        aria-label="Words in box ${i + 1}"${it.label === 'text' ? '' : ' hidden'}>
    </div>`).join('');
}

function onRowInput(e) {
  const row = e.target.closest('.lyrow');
  const k = e.target.dataset.k;
  if (!row || !k) return;
  const it = items[Number(row.dataset.i)];
  it[k] = e.target.value;
  // A label changes which boxes the row has; typing doesn't, and redrawing
  // the rows would take the focus out of the box being typed in.
  if (k === 'label') drawRows();
  changed();
}

export function initLayout({ size, picture, onChange }) {
  hooks = { size, picture, onChange };
  drawRows();
  syncSummary();
  $('stagemode').addEventListener('click', (e) => {
    const b = e.target.closest('[data-mode]');
    if (b) setMode(b.dataset.mode);
  });
  frame().addEventListener('pointerdown', onPointerDown);
  $('lyrows').addEventListener('input', onRowInput);
  $('lyrows').addEventListener('change', onRowInput);
  $('lyrows').addEventListener('focusin', (e) => {
    const row = e.target.closest('.lyrow');
    if (row && Number(row.dataset.i) !== sel) select(Number(row.dataset.i));
  });
  $('lyrows').addEventListener('click', (e) => {
    const del = e.target.closest('[data-del]');
    if (!del) return;
    items.splice(Number(del.closest('.lyrow').dataset.i), 1);
    sel = -1;
    drawRows();
    changed();
  });
  $('lyclear').addEventListener('click', () => setLayout(null));
  // Delete removes the selected box, unless a box is being typed in.
  document.addEventListener('keydown', (e) => {
    if (mode !== 'layout' || sel < 0 || $('tab-image').hidden) return;
    if (e.key !== 'Delete' || e.target.closest('input, textarea, select')) return;
    items.splice(sel, 1);
    sel = -1;
    drawRows();
    changed();
  });
  new ResizeObserver(() => {
    if (mode === 'layout') placeFrame();
  }).observe($('layoutstage'));
}

// The Generation panel's size changed: the frame follows.
export function relayout() {
  if (mode === 'layout') placeFrame();
}
