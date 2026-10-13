import { $, esc, post } from './dom.js';
import { S, escErr } from './state.js';
import { setBusy, setIdle, onIdle } from './busy.js';
import { setPost, setPostStart, showFullSize, setViewerImage, cancelPost } from './viewer.js';
import { gridMarks } from './gridmark.js';
import { render } from './image.js';
import { openRedraw } from './redraw.js';

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

// Everything in one gallery, newest first: `outputs` is work/output/,
// `favorite-outputs` is user/favorite_outputs/ — the route's first segment, the
// way the work-item grid's shelf is. `[]` on failure rather than a throw:
// three callers draw a grid from this and an empty one reads correctly as
// "nothing to pick", which is also what a fresh install has.
export async function fetchOutputs(gallery = 'outputs') {
  try {
    const list = await (await fetch(`/${gallery}`)).json();
    return Array.isArray(list) ? list : [];
  } catch {
    return [];
  }
}

// One tile, drawn the same wherever an output is shown, from either gallery.
// The thumbnail is always the `/outputs` route: the server finds a stem in
// whichever gallery holds it, so a favourite needs no route of its own.
//: this tab, and the
// picker dialog behind every "Start from" slot (`source-slot.js`). It was
// written out three times before, which is three chances for the declared height to drift from
// what `.wigrid` actually draws — and when it does, the strip reflows as the
// thumbnails decode.
//
// No `width`/`height` from the server, unlike the work-item grid: an output's
// thumbnail is generated on first request, so there is nothing to report about
// its size until it exists. The aspect from the sidecar reserves the space, and
// a sidecar-less image (`0`) falls back to a square.
// A stem whose picture was saved over this session, and when: the tile's URL
// carries it, so the browser fetches the new thumbnail instead of reusing the
// old one under the same name. The server routes on the path alone.
const edited = new Map();

export function outputGrid(el, list, { height = 200, empty = '', onPick }) {
  el.innerHTML = list.map((o) => {
    const w = o.height ? Math.round(height * o.width / o.height) : height;
    return `<button type="button" data-stem="${esc(o.stem)}"
      title="${esc(o.prompt || o.stem)}&#10;${o.width}×${o.height} · seed ${o.seed}${
        o.template ? ` · ${esc(o.template)}` : ''}">
      <img src="/outputs/${esc(o.stem)}/thumb.png${edited.has(o.stem) ? `?v=${edited.get(o.stem)}` : ''}" alt="" loading="lazy"
        width="${w}" height="${height}"></button>`;
  }).join('') || empty;
  for (const b of el.querySelectorAll('button')) {
    b.addEventListener('click', () => onPick(b.dataset.stem));
  }
}

// ---- The tab ----------------------------------------------------------------

// Which gallery the grid shows, as the first segment of its routes. See
// `Gallery` in `src/workitems.rs`, and `shelf` in workitems.js, which this is
// one directory along.
let gallery = 'outputs';
const onFavorites = () => gallery === 'favorite-outputs';

let outputs = [];
// Which tile is open in the viewer, and the only cursor the arrow keys move.
// -1 when nothing has been opened yet.
let index = -1;
// An open is a job on the serialized display worker, so a held arrow key has to coalesce
// the way a slider drag does (see runPost): while one is in flight the latest
// target is remembered and fired once, rather than eight of them queueing up and
// then playing back as a slideshow after the key is released.
let opening = false;
let queued = null;

// Marking, the buttons and the requests, shared with the work-item grid.
// These are the two places on the page that remove a file, and a fix to one that
// missed the other would be a fix that only half worked. The same two actions
// too: keep (a copy into user/favorite_outputs/) and delete, which follows the
// gallery.
const del = gridMarks({
  grid: 'ogrid',
  mode: 'omode',
  ids: () => outputs.map((o) => o.stem),
  reload: loadOutputs,
  err: (msg) => { $('oerr').textContent = msg; },
  onMode: () => markGrid(),
  actions: [{
    value: 'favorite',
    button: 'ofavorite',
    mark: 'keep',
    label: (n) => `Favorite ${n} selected`,
    route: () => '/favorite-outputs/add',
    key: 'stems',
  }, {
    value: 'delete',
    button: 'odelete',
    mark: 'sel',
    label: (n) => `Delete ${n} selected`,
    route: () => `/${gallery}/delete`,
    key: 'stems',
    all: {
      button: 'odeleteall',
      noun: 'saved image',
      label: (n) => `Delete all ${n}`,
      // Not on Favorites: emptying that in one press is what it exists to
      // prevent. The same rule as the work-item shelf.
      when: () => !onFavorites(),
    },
  }],
});

// Flip which gallery the grid reads — `setShelf` in workitems.js, and for the
// same reasons: the mode goes back to open (marks belong to the gallery they
// were made on, and favouriting a favourite means nothing), and the image open
// in the viewer stays open, since it is a copy in scratch either way.
function setGallery(next) {
  gallery = next;
  $('ocurrent').classList.toggle('active', !onFavorites());
  $('ofavorites').classList.toggle('active', onFavorites());
  $('ofavlabel').hidden = onFavorites();
  $('owhere').textContent = (onFavorites() ? 'user/favorite_outputs/' : 'work/output/')
    + ' — click one to open it in the viewer, then ← / → to step';
  const open = document.querySelector('input[name=omode][value=open]');
  if (!open.checked) {
    open.checked = true;
    open.dispatchEvent(new Event('change'));
  }
  loadOutputs();
}

export async function loadOutputs() {
  // Which image is open, by name rather than by number: a save lands at the
  // head of a newest-first list and shifts every index below it, and a refresh
  // can drop an entry entirely. `-1` if it has gone.
  const open = outputs[index]?.stem;
  // The gallery can be flipped mid-flight, and the loser of that race must not
  // draw one gallery's tiles under the other's buttons.
  const from = gallery;
  const list = await fetchOutputs(from);
  if (from !== gallery) return;
  outputs = list;
  index = open ? outputs.findIndex((o) => o.stem === open) : -1;
  del.prune(outputs.map((o) => o.stem));
  outputGrid($('ogrid'), outputs, {
    empty: onFavorites()
      ? '<p class="sub">No favorites yet — pick "Click to favorite" on Current, '
        + 'mark some images and keep them.</p>'
      : '<p class="sub">Nothing in work/output/ yet — press "Save to output" '
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
  // No check for a run on the Image tab: opening is on the display lane and
  // the viewer's image has a slot of its own (`Slot::Output`), so a run
  // landing images meanwhile neither delays this nor replaces it.
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
  // Not while the next picture opens: `index` already names it, and the save
  // would be of the one still retained. `setIdle('lbsave')` re-enables it
  // through the `onIdle` hook below.
  $('lbsavechanges').disabled = true;
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
    setIdle('lbsave');
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

// Keep the edit as a new file beside the original. `free_stem` suffixes the
// name, so this never touches the picture it was opened from — that is
// Save changes' job, below.
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
    setIdle('lbsave');
  }
}

// Save the edit over the picture it was opened from (`/outputs/overwrite`).
// Safe to press twice: the viewer holds a copy of the file as it was opened and
// the sliders re-filter that, so a second save replaces the first rather than
// grading on top of it — and Reset, then Save changes, puts the original back.
// That copy goes when another picture is opened, which is what the line says.
async function saveChanges() {
  if (S.imageId === null) return;
  const stem = outputs[index]?.stem;
  setBusy('lbsavechanges', 'Saving…');
  $('lberr').textContent = '';
  try {
    await post('/outputs/overwrite', { image_id: S.imageId });
    if (stem) edited.set(stem, Date.now());
    $('lbsaved').textContent = `saved over ${stem} — Reset and save again to undo, until you move on`;
    await loadOutputs();
  } catch (e) {
    $('lberr').textContent = String(e.message || e);
  } finally {
    setIdle('lbsavechanges');
  }
}

// Into the Refine dialog: the saved file becomes the Image tab's result, so
// refining, extending and editing work on it as on any generation. The Image
// tab's model stands in for one the sidecar doesn't name (an upload). The tab
// switch comes first — `showTab` empties the pane it leaves and restores the
// Image tab's last picture, which `render` then replaces.
async function toImage() {
  const stem = outputs[index]?.stem;
  if (!stem) return;
  setBusy('lbrefine', 'Opening…');
  $('lberr').textContent = '';
  try {
    const data = await post('/outputs/to-image', { stem, model: $('template').value || null });
    $('lightbox').close();
    document.querySelector('.tab[data-tab="image"]')?.click();
    render(data);
    openRedraw(data, { fromOutput: true });
  } catch (e) {
    $('lberr').textContent = String(e.message || e);
  } finally {
    setIdle('lbrefine');
  }
}

// Declarations only above this line; everything that touches the DOM is here.
export function initOutputs() {
  del.init();
  $('orefresh').addEventListener('click', loadOutputs);
  $('ocurrent').addEventListener('click', () => setGallery('outputs'));
  $('ofavorites').addEventListener('click', () => setGallery('favorite-outputs'));
  $('lbsave').addEventListener('click', saveEdit);
  $('lbsavechanges').addEventListener('click', saveChanges);
  $('lbrefine').addEventListener('click', toImage);

  // On `document`, because a modal <dialog>'s keydown still bubbles out of it —
  // and because the same listener then covers the grid behind it.
  //
  // The guard that matters is the last one: an <input type=range> consumes Left
  // and Right natively, and that is the right behaviour for it. So a focused
  // slider keeps its arrows and everything else gets the gallery's. Clicking the
  // picture is how you hand them back.
  document.addEventListener('keydown', (e) => {
    if (S.pane !== 'outputs') return;
    if (!$('lightbox').open) return;
    if (e.key !== 'ArrowLeft' && e.key !== 'ArrowRight') return;
    if (/^(INPUT|TEXTAREA|SELECT)$/.test(e.target?.tagName)) return;
    e.preventDefault();
    step(e.key === 'ArrowRight' ? 1 : -1);
  });

  // Save is only pressable once something is open, which IDLE_LABELS can't know.
  onIdle(() => {
    $('lbsave').disabled = S.imageId === null;
    $('lbsavechanges').disabled = S.imageId === null;
    $('lbrefine').disabled = S.imageId === null;
  });

  loadOutputs();
}
