import { $, esc, post } from './dom.js';
import { S } from './state.js';
import { imageLoras, imageSource, models, render, setParts, setUpscaler, syncCfgPlaceholders,
         syncOutSize, syncParts } from './image.js';
import { gridMarks } from './gridmark.js';

// The last listing. It carries each item's full settings, so opening one needs
// no second request.
let workItems = [];

// Which shelf the grid is showing, as the first segment of its routes — the two
// are the same three files under the same ids, so the only difference between
// Current and Favorites is this string. `work/workitems/` is what this program
// sweeps (`Delete all`, and `purge_pngs` at startup); `user/favorite/` is what
// it never touches unasked. See `Shelf` in `src/workitems.rs`.
let shelf = 'workitems';
const onFavorites = () => shelf === 'favorites';

// Marking, the buttons above the grid and both requests live in `gridmark.js`,
// shared with the Outputs gallery — the places on the page that remove a file,
// and so the ones that must not drift apart.
const marks = gridMarks({
  grid: 'wigrid',
  mode: 'wimode',
  ids: () => workItems.map((w) => w.id),
  reload: loadWorkItems,
  err: (msg) => { $('err').textContent = msg; },
  onMode: () => markGrid(),
  actions: [{
    // Keeping one. A copy rather than a move, so the original stays in the grid
    // and `Delete all` stays the blunt instrument it is — which is the whole
    // point: the favourites are what survives it.
    value: 'favorite',
    button: 'wifavorite',
    mark: 'keep',
    label: (n) => `Favorite ${n} selected`,
    route: () => '/favorites/add',
    key: 'ids',
  }, {
    value: 'delete',
    button: 'widelete',
    mark: 'sel',
    label: (n) => `Delete ${n} selected`,
    // Follows the shelf, which is why the route is a thunk: on Favorites this
    // removes the kept copy and nothing under `work/`.
    route: () => `/${shelf}/delete`,
    key: 'ids',
    all: {
      button: 'wideleteall',
      noun: 'work item',
      label: (n) => `Delete all ${n}`,
      // Not on the favourites shelf. Emptying that directory in one press is
      // the exact thing it exists to be safe from; the marked delete above is
      // still there for taking one back out.
      when: () => !onFavorites(),
    },
  }],
});

// Flip which directory the grid reads. Everything else follows from `shelf`, so
// this only has to reset the things that don't: the mode (marks belong to the
// shelf they were made on, and "Click to favorite" isn't offered here), and
// which of the two choice buttons is lit.
function setShelf(next) {
  shelf = next;
  $('wicurrent').classList.toggle('active', !onFavorites());
  $('wifavorites').classList.toggle('active', onFavorites());
  // Favouriting a favourite means nothing, so the radio goes away — and with it
  // the mode, if that is where we were standing. Assigning `checked` fires no
  // `change`, so the marks are cleared by hand through the same path.
  $('wifavlabel').hidden = onFavorites();
  const open = document.querySelector('input[name=wimode][value=open]');
  if (!open.checked) {
    open.checked = true;
    open.dispatchEvent(new Event('change'));
  }
  loadWorkItems();
}

export async function loadWorkItems() {
  // Whichever shelf is showing. A generate reloads this too and deliberately
  // does *not* switch you back to Current: the toggle is where you left it,
  // which is the only rule that stays predictable, and the new item is one
  // click away.
  const from = shelf;
  let items = [];
  try {
    const data = await (await fetch(`/${from}`)).json();
    // A 500 answers with {"error": …}, not a list. Everything below assumes an
    // array, and a TypeError here would take the whole page's wiring with it.
    if (!Array.isArray(data)) throw new Error(data.error || 'unexpected response');
    items = data;
  } catch (e) {
    $('err').textContent = 'Could not load work items: ' + (e.message || e);
  }
  // The shelf can be flipped mid-flight, and the loser of that race must not
  // land at all — not even on `workItems`, which is what a click reads to find
  // the settings to restore. Drawing one shelf's tiles over another's list is
  // the version of this bug that looks like nothing until a tile is clicked.
  if (from !== shelf) return;
  workItems = items;
  marks.prune(workItems.map((w) => w.id));
  renderWorkItems();
}

function renderWorkItems() {
  const grid = $('wigrid');
  if (!workItems.length) {
    grid.innerHTML = `<p class="sub">${onFavorites()
      ? 'No favorites yet — mark some work items and keep them.'
      : 'No work items yet — generate something.'}</p>`;
    marks.sync();
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
      <img src="/${shelf}/${esc(w.id)}/thumb.png" alt="" loading="lazy"
        width="${w.thumb_width}" height="${w.thumb_height}"></button>`).join('');
  for (const b of grid.querySelectorAll('button')) {
    b.addEventListener('click', () => clickWorkItem(b.dataset.id));
  }
  markGrid();
}

export function markGrid() {
  for (const b of $('wigrid').querySelectorAll('button')) {
    marks.mark(b, b.dataset.id);
    b.classList.toggle('active', b.dataset.id === S.workId);
  }
  marks.sync();
}

// In delete mode a click only *marks*: nothing is destroyed until the button
// above the grid is pressed. That makes culling a batch one click per image plus
// one at the end, with the marks visible the whole time — rather than a
// confirmation dialog per image, or a single misclick that can't be taken back.
async function clickWorkItem(id) {
  if (marks.toggle(id)) {
    markGrid();
    return;
  }
  // Opening works during a run too: it is on the display lane, so it never
  // waits for the render in flight. The run's next image will replace it on
  // screen when it lands, as a run's images always replaced each other — but
  // only on screen: the run holds its own image (`Retained::pin`), and this one
  // stays one click away in the grid.
  if (opening) return;
  const item = workItems.find((w) => w.id === id);
  if (!item) return;
  opening = true;
  $('err').textContent = '';
  try {
    applyWorkItem(item);
    render(await post(`/${shelf}/open`, { id }));
  } catch (e) {
    $('err').textContent = String(e.message || e);
  } finally {
    opening = false;
  }
}

// One open at a time: a double click would otherwise open the item twice, the
// second evicting the first.
let opening = false;

// Put an item's stored settings back in the controls. The server independently
// restores the same profile onto the reopened image, so this is only about what
// the boxes say — but they have to agree, or the next Generate would silently
// differ from the image beside it.
function applyWorkItem(item) {
  const p = item.profile;
  // The prompt as it was typed, where the roll made those two different things:
  // `{animal}` goes back in the box, not the crab it drew. Lossless because the
  // seed goes back with it two lines down — same seed, same roll — so pressing
  // Generate reproduces this image and clearing the seed re-rolls it. A sidecar
  // written before variables existed has no typed copy, and neither has a prompt
  // with nothing in it to roll; both mean "the stored one is what was typed".
  $('prompt').value = item.typed_prompt ?? item.prompt;
  $('negative').value = item.typed_negative ?? item.negative ?? '';
  $('seed').value = item.seed;
  // Assigned directly, and deliberately without firing `change`: that listener
  // is syncAdvanced, which would overwrite every field below with the model's
  // defaults — the exact opposite of restoring this item.
  const known = models.some((m) => m.name === item.template);
  if (known) $('template').value = item.template;
  // The fold's slots are the model's, so they are redrawn for it — and then set
  // to what this image ran with, which a sidecar records as absolute paths. Not
  // when its model is gone: the select kept another one, and that model's slots
  // have no business holding a different family's files.
  syncParts();
  if (known) setParts(p.components);
  $('steps').value = p.steps;
  $('cfg').value = p.cfg_scale;
  $('sampler').value = p.sampler;
  $('w').value = p.width;
  $('h').value = p.height;
  // `?? ''` rather than a fallback number: a sidecar written before this box
  // existed has no factor, and inventing one would claim the image was made at
  // it. An empty box sends nothing and the model's own factor runs.
  $('upscale').value = p.upscale_factor ?? '';
  // A sidecar from before the select existed has no key, and ran with FSR —
  // which is what `null` selects, so there is nothing to invent here.
  setUpscaler(p.upscaler);
  $('rsteps').value = p.refine_steps;
  $('rstrength').value = p.refine_strength;
  $('hsteps').value = p.hires_steps;
  $('hstrength').value = p.hires_strength;
  // Blank stays blank, for the reason fillPass gives: a pre-filled number
  // pins the value and quietly stops it following the box below it.
  $('rcfg').value = p.refine_cfg_scale ?? '';
  $('hcfg').value = p.hires_cfg_scale ?? '';
  // The checkpoints this image was going to be finished with. Guarded the way
  // `template` is: a select silently shows its first option when nothing
  // matches, so a model deleted since would read as "— same as generated —"
  // being *chosen* rather than as the choice having gone.
  setModel('refmodel', p.refine_model);
  setModel('hiresmodel', p.hires_model);
  // What the image was actually diffused with. The server restores these onto
  // the reopened image independently, so this is only about what the picker
  // says — but a picker that disagreed would send the wrong set on the next
  // Generate, which is the same trap the boxes above are in.
  imageLoras.set(p.loras ?? []);
  // The source pass's strength, and the source itself. Set either way: reopening an
  // image made from noise clears the slot, because what made it is "nothing",
  // and leaving a source picked from before would turn the next Generate into
  // a pass over that instead. Setting the slot runs syncMode, which shows the
  // right half of the panel and refreshes the notes.
  $('srcstrength').value = p.source_strength;
  imageSource.set(item.source ?? null);
  // Not the sliders: render() sets those from what the server actually applied.
  syncCfgPlaceholders();
  syncOutSize();
}

// Put a stored escalation checkpoint back in its picker, or leave the picker at
// "— same as generated —". `null` is the ordinary answer: it is what an image
// generated without one stored, and what every sidecar written before these two
// fields existed says.
function setModel(id, name) {
  $(id).value = name && models.some((m) => m.name === name) ? name : '';
}

export function initWorkItems() {
  marks.init();
  $('wicurrent').addEventListener('click', () => setShelf('workitems'));
  $('wifavorites').addEventListener('click', () => setShelf('favorites'));
  // Independent of the model list: the grid is what survived the last restart.
  loadWorkItems();
}
