import { $, esc, post } from './dom.js';
import { S } from './state.js';
import { setBusy } from './busy.js';
import { imageLoras, models, render, syncIcfgPlaceholder } from './image.js';
import { i2Loras, setI2Source, syncI2Note } from './img2img.js';
import { gridDelete } from './griddelete.js';

// The last /workitems listing. It carries each item's full settings, so opening
// one needs no second request.
let workItems = [];

// Marking, the two buttons above the grid and the delete request all live in
// `griddelete.js`, shared with the Outputs gallery — the two places on the page
// that remove a file, and so the two that must not drift apart.
const del = gridDelete({
  grid: 'wigrid',
  mode: 'wimode',
  button: 'widelete',
  all: 'wideleteall',
  route: '/workitems/delete',
  key: 'ids',
  noun: 'work item',
  ids: () => workItems.map((w) => w.id),
  reload: loadWorkItems,
  err: (msg) => { $('err').textContent = msg; },
  onMode: () => markGrid(),
});

export async function loadWorkItems() {
  try {
    const data = await (await fetch('/workitems')).json();
    // A 500 answers with {"error": …}, not a list. Everything below assumes an
    // array, and a TypeError here would take the whole page's wiring with it.
    if (!Array.isArray(data)) throw new Error(data.error || 'unexpected response');
    workItems = data;
  } catch (e) {
    workItems = [];
    $('err').textContent = 'Could not load work items: ' + (e.message || e);
  }
  del.prune(workItems.map((w) => w.id));
  renderWorkItems();
}

function renderWorkItems() {
  const grid = $('wigrid');
  if (!workItems.length) {
    grid.innerHTML = '<p class="sub">No work items yet — generate something.</p>';
    del.sync();
    return;
  }
  // width/height on every thumbnail so the strip reserves its space up front and
  // doesn't reflow, row by row, as the images decode. `loading="lazy"` for the
  // other end of the same problem: a loop left running overnight leaves hundreds
  // of entries here, and there is no reason for the page to hold a decoded copy
  // of each. With the dimensions already declared, deferring them costs nothing.
  grid.innerHTML = workItems.map((w) => `
    <button type="button" data-id="${esc(w.id)}"
      title="${esc(w.prompt)}&#10;${w.width}×${w.height} · seed ${w.seed} · ${esc(w.template)}">
      <img src="/workitems/${esc(w.id)}/thumb.png" alt="" loading="lazy"
        width="${w.thumb_width}" height="${w.thumb_height}"></button>`).join('');
  for (const b of grid.querySelectorAll('button')) {
    b.addEventListener('click', () => clickWorkItem(b.dataset.id));
  }
  markGrid();
}

export function markGrid() {
  for (const b of $('wigrid').querySelectorAll('button')) {
    b.classList.toggle('sel', del.has(b.dataset.id));
    b.classList.toggle('active', b.dataset.id === S.workId);
  }
  del.sync();
}

// In delete mode a click only *marks*: nothing is destroyed until the button
// above the grid is pressed. That makes culling a batch one click per image plus
// one at the end, with the marks visible the whole time — rather than a
// confirmation dialog per image, or a single misclick that can't be taken back.
async function clickWorkItem(id) {
  if (del.toggle(id)) {
    markGrid();
    return;
  }
  // Marking for deletion stays live during a run — it touches nothing the run
  // uses — but opening does not. The buttons are disabled while a run is going;
  // the thumbnails aren't, and an open would queue behind the generation in
  // flight only to have its render replaced by the next image.
  if (S.run) return;
  const item = workItems.find((w) => w.id === id);
  if (!item) return;
  setBusy('go', 'Opening…');
  $('err').textContent = '';
  try {
    applyWorkItem(item);
    render(await post('/workitems/open', { id }));
  } catch (e) {
    $('err').textContent = String(e.message || e);
  } finally {
    setBusy(null);
  }
}

// Put an item's stored settings back in the controls. The server independently
// restores the same profile onto the reopened image, so this is only about what
// the boxes say — but they have to agree, or the next Generate would silently
// differ from the image beside it.
function applyWorkItem(item) {
  const p = item.profile;
  $('prompt').value = item.prompt;
  $('negative').value = item.negative ?? '';
  $('seed').value = item.seed;
  // Assigned directly, and deliberately without firing `change`: that listener
  // is syncAdvanced, which would overwrite every field below with the model's
  // defaults — the exact opposite of restoring this item.
  if (models.some((m) => m.name === item.template)) $('template').value = item.template;
  $('steps').value = p.steps;
  $('cfg').value = p.cfg_scale;
  // Blank stays blank, for the reason syncAdvanced gives: a pre-filled number
  // pins the value and quietly stops it following the CFG box.
  $('icfg').value = p.img2img_cfg_scale ?? '';
  $('sampler').value = p.sampler;
  $('w').value = p.width;
  $('h').value = p.height;
  $('rsteps').value = p.refine_steps;
  $('rstrength').value = p.refine_strength;
  $('hsteps').value = p.hires_steps;
  $('hstrength').value = p.hires_strength;
  // What the image was actually diffused with. The server restores these onto
  // the reopened image independently, so this is only about what the picker
  // says — but a picker that disagreed would send the wrong set on the next
  // Generate, which is the same trap the boxes above are in.
  imageLoras.set(p.loras ?? []);
  // An image-to-image generation remembers what it was diffused from, so
  // reopening one puts the source back rather than quietly forgetting where the
  // picture came from. Only the img2img panel is restored — the model and
  // strength that made it live there, not in this tab's boxes.
  if (item.source) {
    $('i2prompt').value = item.prompt;
    $('i2negative').value = item.negative ?? '';
    $('i2seed').value = item.seed;
    if (models.some((m) => m.name === item.template)) $('i2template').value = item.template;
    $('i2steps').value = p.source_steps;
    $('i2strength').value = p.source_strength;
    $('i2cfg').value = p.cfg_scale;
    $('i2icfg').value = p.img2img_cfg_scale ?? '';
    $('i2sampler').value = p.sampler;
    i2Loras.set(p.loras ?? []);
    setI2Source(item.source);
    syncI2Note();
  }
  // Not the sliders: render() sets those from what the server actually applied.
  syncIcfgPlaceholder();
}

export function initWorkItems() {
  del.init();
  // Independent of the model list: the grid is what survived the last restart.
  loadWorkItems();
}
