import { $, esc, post } from './dom.js';
import { S, escErr } from './state.js';
import { setBusy, onIdle } from './busy.js';
import { setPost, setPostStart, showFullSize, setViewerImage, cancelPost } from './viewer.js';
import { gridMarks } from './gridmark.js';

// ---- Outputs ----------------------------------------------------------------
//
// work/output/ as a gallery. Every other tab reads that directory for something
// to start *from*; this one is for looking at what is already finished, and for
// grading it.
//
// The tab is deliberately the grid and nothing else: click a tile and the viewer
// opens on it, arrow keys step through the gallery with the dialog left up. It
// offers no Refine and no Hi-res, and that is the point rather than an omission —
// a saved output is `display`, with its grade and its RCAS already in the pixels,
// so a diffusion over one is exactly what invariant 1 exists to prevent. See
// `Engine::open_output`, which says the same thing from the other end.

// ---- Shared: the output grid ------------------------------------------------

// Everything in work/output/, newest first. `[]` on failure rather than a throw:
// three callers draw a grid from this and an empty one reads correctly as
// "nothing to pick", which is also what a fresh install has.
export async function fetchOutputs() {
  try {
    const list = await (await fetch('/outputs')).json();
    return Array.isArray(list) ? list : [];
  } catch {
    return [];
  }
}

// One tile, drawn the same wherever an output is shown: this tab, and the
// picker dialog behind every "Start from" slot (`source-slot.js`). It was
// written out three times before, which is three chances for the declared height to drift from
// what `.wigrid` actually draws — and when it does, the strip reflows as the
// thumbnails decode.
//
// No `width`/`height` from the server, unlike the work-item grid: an output's
// thumbnail is generated on first request, so there is nothing to report about
// its size until it exists. The aspect from the sidecar reserves the space, and
// a sidecar-less image (`0`) falls back to a square.
export function outputGrid(el, list, { height = 200, empty = '', onPick }) {
  el.innerHTML = list.map((o) => {
    const w = o.height ? Math.round(height * o.width / o.height) : height;
    return `<button type="button" data-stem="${esc(o.stem)}"
      title="${esc(o.prompt || o.stem)}&#10;${o.width}×${o.height} · seed ${o.seed}${
        o.template ? ` · ${esc(o.template)}` : ''}">
      <img src="/outputs/${esc(o.stem)}/thumb.png" alt="" loading="lazy"
        width="${w}" height="${height}"></button>`;
  }).join('') || empty;
  for (const b of el.querySelectorAll('button')) {
    b.addEventListener('click', () => onPick(b.dataset.stem));
  }
}

// ---- The tab ----------------------------------------------------------------

let outputs = [];
// Which tile is open in the viewer, and the only cursor the arrow keys move.
// -1 when nothing has been opened yet.
let index = -1;
// An open is a job on the serialized worker, so a held arrow key has to coalesce
// the way a slider drag does (see runPost): while one is in flight the latest
// target is remembered and fired once, rather than eight of them queueing up and
// then playing back as a slideshow after the key is released.
let opening = false;
let queued = null;

// Marking, the two buttons and the request, shared with the work-item grid.
// These are the two places on the page that remove a file, and a fix to one that
// missed the other would be a fix that only half worked.
// One action, because this gallery only destroys: a favourite is a *work item*
// kept as a workflow, and an output is a finished picture with nowhere further
// to go. See the Outputs section of CLAUDE.md for the same argument about Refine.
const del = gridMarks({
  grid: 'ogrid',
  mode: 'omode',
  ids: () => outputs.map((o) => o.stem),
  reload: loadOutputs,
  err: (msg) => { $('oerr').textContent = msg; },
  onMode: () => markGrid(),
  actions: [{
    value: 'delete',
    button: 'odelete',
    mark: 'sel',
    label: (n) => `Delete ${n} selected`,
    route: () => '/outputs/delete',
    key: 'stems',
    all: {
      button: 'odeleteall',
      noun: 'saved image',
      label: (n) => `Delete all ${n}`,
    },
  }],
});

export async function loadOutputs() {
  // Which image is open, by name rather than by number: a save lands at the
  // head of a newest-first list and shifts every index below it, and a refresh
  // can drop an entry entirely. `-1` if it has gone.
  const open = outputs[index]?.stem;
  outputs = await fetchOutputs();
  index = open ? outputs.findIndex((o) => o.stem === open) : -1;
  del.prune(outputs.map((o) => o.stem));
  outputGrid($('ogrid'), outputs, {
    empty: '<p class="sub">Nothing in work/output/ yet — press "Save to output" '
         + 'under an image on the Image tab, or upload one with Start from.</p>',
    onPick: pick,
  });
  markGrid();
}

// In delete mode a click only *marks*, exactly as it does in the work-item grid:
// nothing goes until the button above the grid is pressed. Otherwise it opens.
function pick(stem) {
  if (del.toggle(stem)) {
    markGrid();
    return;
  }
  openAt(outputs.findIndex((o) => o.stem === stem));
}

function markGrid() {
  const stem = outputs[index]?.stem;
  for (const b of $('ogrid').querySelectorAll('button')) {
    del.mark(b, b.dataset.stem);
    // The image the viewer is on. Bone rather than red — the two marks mean
    // opposite things and land on the same tiles.
    b.classList.toggle('active', b.dataset.stem === stem);
  }
  del.sync();
}

// Open one in the viewer, live: the server mints a fresh `image_id` for it, which
// is the only thing that makes the sliders work — `/adjust` addresses a retained
// `Pending`, never a filename.
async function openAt(i) {
  if (i < 0 || i >= outputs.length) return;
  // A batch or loop on the Image tab owns the worker *and* `setBusy`: opening
  // here would end with a `setBusy(null)` that re-enables Generate in the middle
  // of the run it belongs to.
  if (S.run) return;
  if (opening) { queued = i; return; }
  // A debounced or in-flight `/adjust` belongs to the image being left. Without
  // this, letting go of a slider and pressing → immediately lands the old
  // image's re-filter on top of the new one.
  cancelPost();
  opening = true;
  const wasOpen = $('lightbox').open;
  index = i;
  markGrid();
  $('oerr').textContent = '';
  setBusy('lbsave', 'Opening…');
  try {
    const data = await post('/outputs/open', { stem: outputs[i].stem });
    S.imageId = data.image_id;
    S.currentSrc = `data:image/png;base64,${data.image_base64}`;
    // Both of these are `Filters::NONE` for an output — the picture already
    // carries whatever was applied when it was saved, so 0 means "as saved" and
    // Reset means "back to the file on disk".
    setPostStart(data.filters_start ?? data.filters);
    setPost(data.filters);
    $('postms').textContent = '';
    // `showFullSize` clears this, but swapping the image inside an open dialog
    // doesn't — so the previous image's failure would sit under the new one.
    $('lberr').textContent = '';
    $('lbsaved').textContent = `${outputs[i].stem} · ${data.width}×${data.height}`;
    // Opening the dialog and swapping the image it holds are different things,
    // and only the first may call showModal. Both re-centre, because what
    // arrived is a different picture rather than a re-filter of this one.
    if (wasOpen) setViewerImage(S.currentSrc); else showFullSize(S.currentSrc);
  } catch (e) {
    S.imageId = null;
    const msg = String(e.message || e);
    $('oerr').textContent = msg;
    if ($('lightbox').open) $('lberr').textContent = msg;
  } finally {
    opening = false;
    setBusy(null);
    if (queued !== null) { const next = queued; queued = null; openAt(next); }
  }
}

// Clamped rather than wrapped: running off the end of a gallery and reappearing
// at the other one loses your place in it.
function step(d) {
  const i = Math.min(outputs.length - 1, Math.max(0, index + d));
  // `index` is already the in-flight target while one is opening, so a second
  // press computes the one after it and coalesces rather than repeating.
  if (i !== index) openAt(i);
}

// Keep the edit. A new file beside the original, never over it: the pre-edit
// pixels exist nowhere else — an output can descend from a refine, and those are
// not archived — so `free_stem` suffixing the name is the whole safety net.
async function saveEdit() {
  if (S.imageId === null) return;
  setBusy('lbsave', 'Saving…');
  $('lberr').textContent = '';
  try {
    const data = await post('/save-output', { image_id: S.imageId });
    $('lbsaved').textContent = `saved → ${data.path}`;
    // The edit is now a tile of its own, at the head of the grid. `loadOutputs`
    // keeps the image being edited open across the refresh.
    await loadOutputs();
  } catch (e) {
    escErr(String(e.message || e));
    $('lberr').textContent = String(e.message || e);
  } finally {
    setBusy(null);
  }
}

// Declarations only above this line; everything that touches the DOM is here.
export function initOutputs() {
  del.init();
  $('orefresh').addEventListener('click', loadOutputs);
  $('lbsave').addEventListener('click', saveEdit);

  // On `document`, because a modal <dialog>'s keydown still bubbles out of it —
  // and because the same listener then covers the grid behind it.
  //
  // The guard that matters is the last one: an <input type=range> consumes Left
  // and Right natively, and that is the right behaviour for it. So a focused
  // slider keeps its arrows and everything else gets the gallery's. Clicking the
  // picture is how you hand them back.
  document.addEventListener('keydown', (e) => {
    if (S.pane !== 'outputs' || S.run) return;
    if (!$('lightbox').open) return;
    if (e.key !== 'ArrowLeft' && e.key !== 'ArrowRight') return;
    if (/^(INPUT|TEXTAREA|SELECT)$/.test(e.target?.tagName)) return;
    e.preventDefault();
    step(e.key === 'ArrowRight' ? 1 : -1);
  });

  // Save is only pressable once something is open, which IDLE_LABELS can't know.
  onIdle(() => { $('lbsave').disabled = S.imageId === null; });

  loadOutputs();
}
