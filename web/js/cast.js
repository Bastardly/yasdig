import { $, esc, getJSON } from './dom.js';
import { gridMarks } from './gridmark.js';
import { KIND_LABEL, onCastChanged, openCastEditor } from './casteditor.js';

// ---- The Cast tab ---------------------------------------------------------------
//
// user/cast/ as a grid (`src/cast.rs`): one tile per entry, its picture and
// name. A click opens the cast editor; *Click to delete* marks, as on every
// grid (`gridmark.js`). One shelf and no star: nothing sweeps user/cast/, so
// everything here is already kept. The kind buttons filter what is shown.

let entries = [];
let kind = ''; // '' = all
let loadSeq = 0;

// Every entry, saved ones and drafts, by name. `[]` on failure: an empty
// grid reads as "nothing yet", which is also what a fresh install has.
export async function fetchCast() {
  try {
    const list = await getJSON('/cast');
    return Array.isArray(list) ? list : [];
  } catch {
    return [];
  }
}

const shown = () => entries.filter((e) => !kind || e.kind === kind);

// One tile: the picture (or the name's first letter, for a draft not drawn
// yet) and the name with its kind under it. `data-id` like the work items'.
export function castTile(e, height = 200) {
  const tip = esc(e.look || e.name || 'Not named yet');
  const pic = e.picture
    ? `<img src="/cast/${esc(e.id)}/thumb.png?v=${e.rev}" alt="" loading="lazy" height="${height}">`
    : `<span class="castletter" style="width:${Math.round(height * 0.75)}px;height:${height}px" aria-hidden="true">${
      esc((e.name || '?').trim().charAt(0).toUpperCase())}</span>`;
  return `<button type="button" class="casttile" data-id="${esc(e.id)}" title="${tip}">${pic}
    <span class="castcap"><b>${esc(e.name || 'Not named yet')}</b>
    <span class="sub">${KIND_LABEL[e.kind] ?? e.kind}${e.draft ? ' · not saved' : ''}</span></span></button>`;
}

const del = gridMarks({
  grid: 'ctgrid',
  mode: 'ctmode',
  ids: () => shown().map((e) => e.id),
  reload: loadCast,
  err: (msg) => { $('cterr').textContent = msg; },
  onMode: () => markGrid(),
  actions: [{
    value: 'delete',
    button: 'ctdelete',
    mark: 'sel',
    label: (n) => `Delete ${n} selected`,
    route: () => '/cast/delete',
    key: 'ids',
  }],
});

function markGrid() {
  for (const b of $('ctgrid').querySelectorAll('button[data-id]')) del.mark(b, b.dataset.id);
  del.sync();
}

function drawGrid() {
  const list = shown();
  $('ctgrid').innerHTML = list.map((e) => castTile(e)).join('')
    || `<p class="sub">${entries.length ? 'None of this kind yet.'
      : 'No cast yet. New cast… makes one: a name, how they look, and a picture drawn from it.'}</p>`;
  del.prune(list.map((e) => e.id));
  markGrid();
}

export async function loadCast() {
  const seq = ++loadSeq;
  const list = await fetchCast();
  if (seq !== loadSeq) return;
  entries = list;
  drawGrid();
}

function setKind(k) {
  kind = k;
  for (const b of $('ctkinds').querySelectorAll('button')) b.classList.toggle('active', b.dataset.kind === k);
  drawGrid();
}

export function initCast() {
  del.init();
  $('ctkinds').addEventListener('click', (e) => {
    const b = e.target.closest('button[data-kind]');
    if (b) setKind(b.dataset.kind);
  });
  $('ctgrid').addEventListener('click', (e) => {
    const b = e.target.closest('button[data-id]');
    if (!b) return;
    if (del.toggle(b.dataset.id)) {
      markGrid();
      return;
    }
    openCastEditor(b.dataset.id);
  });
  $('ctrefresh').addEventListener('click', loadCast);
  $('ctnew').addEventListener('click', () => openCastEditor(null, { kind: kind || 'character' }));
  onCastChanged(() => {
    if (!$('tab-cast').hidden) loadCast();
  });
}
