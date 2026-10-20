import { $, esc, getJSON, post } from './dom.js';
import { S } from './state.js';
import { holdTurbo, imageLoras, imageSource, models, render, setParts, setUpscaler, syncCfgPlaceholders,
         syncOutSize, syncParts, syncSeedLine } from './image.js';
import { gridMarks, star, wireStars } from './gridmark.js';
import { setReferences } from './refs.js';
import { setLayout } from './layout.js';

// Two shelves of the same thing, each its own panel and grid: Current is
// `work/workitems/` (which lists starred items too, from user/favorite/),
// Favorites is `user/favorite/` alone. The shelf is the first segment of every
// route, so the two differ only in that string. `work/workitems/` is what this
// program sweeps (`Delete all`, and `purge_pngs` at startup); `user/favorite/`
// is what it never touches unasked. See `Shelf` in `src/workitems.rs`.
//
// A factory, gridMarks' shape: closures only, until `init()`.
function shelfGrid({ shelf, grid, mode, del, all, empty }) {
  // The last listing. It carries each item's full settings, so opening one
  // needs no second request.
  let items = [];
  // Bumped per `load`; only the newest answer may draw.
  let seq = 0;
  const onFavorites = shelf === 'favorites';

  // Marking, the buttons above the grid and both requests live in
  // `gridmark.js`, shared with the Outputs gallery — the places on the page
  // that remove a file, and so the ones that must not drift apart.
  const marks = gridMarks({
    grid,
    mode,
    // On Current, only what lives there: a starred item is in user/favorite/,
    // out of reach of a delete here until it is un-starred.
    ids: () => items.filter((w) => onFavorites || !w.favorite).map((w) => w.id),
    reload: loadWorkItems,
    err: (msg) => { $('err').textContent = msg; },
    onMode: () => mark(),
    // Keeping one is the tile's star (`wireStars` below), not a mode here.
    actions: [{
      value: 'delete',
      button: del,
      mark: 'sel',
      label: (n) => `Delete ${n} selected`,
      // On Favorites this removes the kept copy and nothing under `work/`.
      route: () => `/${shelf}/delete`,
      key: 'ids',
      // Not on Favorites: emptying that directory in one press is the exact
      // thing it exists to be safe from; the marked delete is still there for
      // taking one back out.
      ...(all ? {
        all: {
          button: all,
          noun: 'work item',
          label: (n) => (n ? `Delete all ${n} unstarred` : 'Delete all unstarred'),
        },
      } : {}),
    }],
  });

  async function load() {
    // Every image of a batch reloads the grids, so several of these can be in
    // flight; only the newest may draw, or an older answer landing last leaves
    // the newest item missing until the next image.
    const mine = ++seq;
    let got = [];
    try {
      const data = await getJSON(`/${shelf}`);
      // Everything below assumes an array, and a TypeError here would take
      // the whole page's wiring with it.
      if (!Array.isArray(data)) throw new Error('unexpected response');
      got = data;
    } catch (e) {
      $('err').textContent = 'Could not load work items: ' + (e.message || e);
    }
    if (mine !== seq) return;
    items = got;
    marks.prune(items.map((w) => w.id));
    draw();
  }

  function draw() {
    const el = $(grid);
    if (!items.length) {
      el.innerHTML = `<p class="sub">${empty}</p>`;
      marks.sync();
      return;
    }
    // width/height on every thumbnail so the strip reserves its space up front
    // and doesn't reflow, row by row, as the images decode. `loading="lazy"`
    // for the other end of the same problem: a loop left running overnight
    // leaves hundreds of entries here, and there is no reason for the page to
    // hold a decoded copy of each. With the dimensions declared, deferring them
    // costs nothing.
    el.innerHTML = items.map((w) => `<span class="tile">
      <button type="button" data-id="${esc(w.id)}"
        title="${esc(w.prompt)}&#10;${w.width}×${w.height} · seed ${w.seed} · ${esc(w.template)}">
        <img src="/${home(w)}/${esc(w.id)}/thumb.png" alt="" loading="lazy"
          width="${w.thumb_width}" height="${w.thumb_height}"></button>${star(w.favorite)}</span>`).join('');
    for (const b of el.querySelectorAll('button[data-id]')) {
      b.addEventListener('click', () => click(b.dataset.id));
    }
    // Starring moves the item to user/favorite/ and un-starring moves it
    // back; either way it stays in Current's grid, and both grids reload.
    // `/favorites/delete` is the other thing, and only *Click to delete* on
    // Favorites sends it.
    wireStars(el, {
      of: (btn) => {
        const id = btn.previousElementSibling.dataset.id;
        const w = items.find((x) => x.id === id);
        return { favorite: !!w?.favorite, body: { ids: [id] } };
      },
      add: '/favorites/add',
      remove: '/favorites/remove',
      reload: loadWorkItems,
      err: (msg) => { $('err').textContent = msg; },
    });
    mark();
  }

  function mark() {
    for (const b of $(grid).querySelectorAll('button[data-id]')) {
      marks.mark(b, b.dataset.id);
      b.classList.toggle('active', b.dataset.id === S.workId);
    }
    marks.sync();
  }

  // In delete mode a click only *marks*: nothing is destroyed until the
  // button above the grid is pressed. That makes culling a batch one click per
  // image plus one at the end, with the marks visible the whole time — rather
  // than a confirmation per image, or a misclick that can't be taken back.
  function click(id) {
    if (marks.toggle(id)) {
      mark();
      return;
    }
    const item = items.find((w) => w.id === id);
    if (item) openItem(item);
  }

  return { init: () => marks.init(), load, mark };
}

// The shelf an item's files are on, as its routes' first segment: Current
// lists both, and a starred item is read from user/favorite/.
const home = (w) => (w.favorite ? 'favorites' : 'workitems');

const grids = [
  shelfGrid({
    shelf: 'workitems', grid: 'wigrid', mode: 'wimode', del: 'widelete', all: 'wideleteall',
    empty: 'No work items yet — generate something.',
  }),
  shelfGrid({
    shelf: 'favorites', grid: 'figrid', mode: 'fimode', del: 'fidelete',
    empty: 'No favorites yet — star a work item to keep it.',
  }),
];

// Both shelves: a generate, a star or a delete can change either. A generate
// adds to Current; it never opens or closes a panel.
export function loadWorkItems() {
  return Promise.all(grids.map((g) => g.load()));
}

// The open item, framed in whichever grids show it.
export function markGrid() {
  for (const g of grids) g.mark();
}

// Opening works during a run too: it is on the display lane, so it never waits
// for the render in flight. The run's next image will replace it on screen when
// it lands, as a run's images always replaced each other — but only on screen:
// the run holds its own image (`Retained::pin`), and this one stays one click
// away in the grid.
async function openItem(item) {
  if (opening) return;
  opening = true;
  $('err').textContent = '';
  try {
    applyWorkItem(item);
    render(await post(`/${home(item)}/open`, { id: item.id }));
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
// Also what a PNG carrying yasdig's own record restores through
// (`loadPngSettings` in image.js): the same shape, minus the paths and stems
// that only mean something on the machine that made it.
export function applyWorkItem(item) {
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
  syncSeedLine();
  // Assigned directly, and deliberately without firing `change`: that listener
  // is syncAdvanced, which would overwrite every field below with the model's
  // defaults — the exact opposite of restoring this item.
  const known = models.some((m) => m.name === item.template);
  if (known) $('template').value = item.template;
  // Its LoRAs say whether Turbo was on, so no default may land over them.
  holdTurbo();
  // Its boxes, or none: a reopened image is drawn from what it was made with.
  setLayout(p.layout);
  // The fold's slots are the model's, so they are redrawn for it — and then set
  // to what this image ran with, which a sidecar records as absolute paths. Not
  // when its model is gone: the select kept another one, and that model's slots
  // have no business holding a different family's files.
  syncParts();
  if (known) setParts(p.components);
  $('steps').value = p.steps;
  $('cfg').value = p.cfg_scale;
  $('sampler').value = p.sampler;
  // A sidecar from before the knob ran with sd.cpp's own choice, which is Auto.
  $('scheduler').value = p.scheduler ?? 'auto';
  // And from before step caching, with none: off is what it ran with.
  $('cachemode').value = p.cache_mode ?? 'off';
  // Memory is deliberately *not* restored: it says how a run fits on this
  // machine now, not what the picture is made of. Restoring it turned a
  // reopened Stream-from-SSD item into a silent mode switch for every run after.
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
  // The pictures it was drawn with, back in their numbered slots. A sidecar
  // from before references existed has none, which is what it ran with.
  setReferences(p.references ?? []);
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
  for (const g of grids) g.init();
  // Independent of the model list: the grids are what survived the last restart.
  loadWorkItems();
}
