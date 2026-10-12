import { $, esc, post, rememberFolds } from './dom.js';
import { setUpFactor, loadModels, upscalersReady, upscalerOptions } from './image.js';
import { wireHints } from './hints.js';
import { loadModelSettings } from './modelsettings.js';

// The Settings tab: `user/settings.toml` as an ordinary form.
//
// Every field here is the default for a control that already exists on another
// tab, so the one thing this module must not do is invent a second vocabulary
// for them. A row names the control it is the default *for*, and both the label
// text and the tooltip are taken from that control — the label is read out of
// its `<label>` at build time, and `data-help` hands `wireHints` the same key
// the real control uses. Neither can drift, because neither is written twice.
//
// The three at the bottom have no control to point at: they were only ever
// settable by editing config, and removing `[defaults]` from models.toml is what
// gave them a home. Those carry their own label and their own `HELP` entry.
// There were four — `upscale_factor` has a box on the Image tab now, so it is an
// ordinary row that names it like every other one.
//
// `sharpness` and `clarity` are in the file and deliberately *not* in this form.
// They are where the viewer's two sliders start, which is a per-image live
// control everywhere else on the page — and the Outputs tab starts them at zero
// whatever they say, because a saved output has its grade already in the pixels
// (invariant 1). A default for them reads as a setting that doesn't take, so
// they stay hand-editable and documented in docs/configuration.md instead.

// Blank means "not set", which is a different thing from zero for these — an
// empty CFG follows the knob below it and an empty weight type means no `--type`
// flag at all, while 0 is guidance off — so the field has to be able to say
// nothing.
const BLANK = true;

// Where a section starts. Every section is a fold (`<details class="fold">`),
// like the Image tab's, and they come in the Image tab's order — Prompts,
// Models, Generation, Image to image, Hi-res, Refine — so a default is found
// where its control is. `open: false` starts one folded; what each fold was
// last left at is remembered per browser (`rememberFolds`).
//
// `pane` is the id of the container the rows under it are rendered into, and it
// is why this table is still one table: `markup` is called once per pane and
// skips what isn't in the one it is building, so a section changing column is
// this one word rather than a second list and a second loop.
const SECTION = (title, { open = true, help = null, pane = 'setform' } = {}) =>
  ({ title, open, help, pane });

// A heading inside a section, for rows that belong together but not to a fold
// of their own — the six UNet defaults inside Generation.
const SUB = (sub, help = null) => ({ sub, help });

export const FIELDS = [
  SECTION('Prompts'),
  { key: 'prompt', page: true, from: 'prompt', kind: 'textarea' },
  { key: 'negative', page: true, from: 'negative', kind: 'text' },

  // Not a default for any control: the word lists behind `{animal}` in a
  // prompt. It is on this tab because it is stored in the same file and saves
  // with the same button — but in the *other* column, because a list that grows
  // without bound has no business between Negative prompt and Steps: twenty
  // variables pushed the whole form off the screen. The right-hand column is
  // also wide enough to put a name, its words and the × on one row.
  SECTION('Prompt variables', { help: 'setvars', pane: 'setvarbox' }),
  { key: 'variables', page: true, kind: 'vars' },

  // The Image tab's Models fold holds the Memory select, so its default is here.
  // Global like the cache mode; a model that needs RAM says so in its own
  // defaults.
  SECTION('Models'),
  { key: 'memory', from: 'memory', kind: 'mirror' },

  SECTION('Generation'),
  // The six knobs a family recipe names. Here they are the defaults for the
  // UNet families only, because that is what they are: every other family's
  // recipe is laid over them (`ImageFamily::recipe`), so a Steps box that
  // claimed to reach a Z-Image would be a setting that appears not to take.
  // Kept rather than removed because they are the one place to change every
  // SDXL at once; a single model is Per model's business.
  SUB('SD 1.x, SD 2.x and SDXL', 'setunet'),
  { key: 'steps', from: 'steps', kind: 'int', min: 1, max: 100 },
  { key: 'cfg_scale', from: 'cfg', kind: 'num', min: 0, step: 0.1 },
  { key: 'sampler', from: 'sampler', kind: 'mirror' },
  { key: 'scheduler', from: 'scheduler', kind: 'mirror' },
  { key: 'width', from: 'w', kind: 'int', min: 64, step: 64 },
  { key: 'height', from: 'h', kind: 'int', min: 64, step: 64 },
  SUB('Every model'),
  // Every value is right for every family (`config::CACHE_MODES`) and no recipe
  // pins it, so it reaches them all — which is why it is out of the UNet rows.
  { key: 'cache_mode', from: 'cachemode', kind: 'mirror' },
  { key: 'upscale_factor', from: 'upscale', kind: 'num', min: 1, max: 4, step: 0.1 },
  { key: 'upscaler', from: 'upscaler', kind: 'upscaler' },

  // The pass over a source runs Steps and CFG scale above; its strength and
  // working size are the two numbers of its own.
  SECTION('Image to image'),
  { key: 'source_strength', from: 'srcstrength', kind: 'num', min: 0.05, max: 0.95, step: 0.05 },
  { key: 'hires_max_side', from: 'srcmaxside', kind: 'int', min: 64, step: 64 },

  SECTION('Hi-res pass'),
  { key: 'hires_steps', from: 'hsteps', kind: 'int', min: 0, max: 100 },
  { key: 'hires_strength', from: 'hstrength', kind: 'num', min: 0.05, max: 0.95, step: 0.05 },
  { key: 'hires_cfg_scale', from: 'hcfg', kind: 'num', min: 0, step: 0.1, blank: BLANK },
  { key: 'hires_scale', page: true, from: 'hiresscale', kind: 'num', min: 1, max: 4, step: 0.1 },

  SECTION('Refine pass'),
  { key: 'refine_steps', from: 'rsteps', kind: 'int', min: 0, max: 100 },
  { key: 'refine_strength', from: 'rstrength', kind: 'num', min: 0.05, max: 0.95, step: 0.05 },
  { key: 'refine_cfg_scale', from: 'rcfg', kind: 'num', min: 0, step: 0.1, blank: BLANK },

  // Not defaults for any control: how this machine runs, so their own labels
  // and help, and here rather than beside a button on the Image tab.
  SECTION('Speed and limits'),
  { key: 'reference_size', page: true, label: 'Reference size', help: 'setrefsize', kind: 'choice',
    options: [[512, '512 px (default)'], [768, '768 px'], [1024, '1024 px'], [0, 'Full (the output’s size)']] },
  { key: 'vae_conv_direct', page: true, label: 'Fast VAE decode', help: 'setvaedirect', kind: 'check' },
  { key: 'gpu_reserve_gb', page: true, label: 'Keep free on the GPU (GB)', help: 'setgpureserve', kind: 'num', min: 0, max: 16, step: 0.5 },
  { key: 'max_image_side', page: true, label: 'Largest image side', help: 'setmaxside', kind: 'int', min: 1024, max: 32768, step: 512 },

  // Not a default for any control either: what *Close server* deletes on the
  // way out (`src/shutdown.rs`). Off unless ticked, and favourites never.
  SECTION('When the server is closed'),
  { key: 'clear_workitems_on_close', page: true, label: 'Clear work items', help: 'setclearwi', kind: 'check' },
  { key: 'clear_outputs_on_close', page: true, label: 'Clear outputs', help: 'setclearout', kind: 'check' },
  { key: 'clear_videos_on_close', page: true, label: 'Clear videos', help: 'setclearvid', kind: 'check' },

  SECTION('Advanced', { open: false }),
  { key: 'hires_flash_attn', label: 'Flash attention', help: 'setflash', kind: 'check' },
  { key: 'weight_type', label: 'Weight type', help: 'setwtype', kind: 'text', blank: BLANK },
  { key: 'extra_args', label: 'Extra sd-cli arguments', help: 'setargs', kind: 'lines' },

  // Features a release ships hidden because they are unfinished. Off by
  // default; ticking one shows it at once (`applyNightly`).
  SECTION('Nightly features', { open: false }),
  { key: 'nightly_video', page: true, label: 'Video tab (work in progress)', help: 'setnightlyvideo', kind: 'check' },
];

// The label of the control this is the default for, without the (i) `wireHints`
// puts inside it. Text nodes only, so it reads the same whether this runs before
// or after that pass.
export function labelOf(id) {
  const el = document.querySelector(`label[for="${id}"]`);
  if (!el) return id;
  return [...el.childNodes]
    .filter((n) => n.nodeType === Node.TEXT_NODE)
    .map((n) => n.textContent)
    .join('')
    .trim();
}

const fieldId = (f) => `set-${f.key}`;

// One variable: its name, its words as a comma-separated line, and a way to
// remove it. The words are one text box rather than a list of them because that
// is how the list is read as well as written — "crab, horse, dog" is the whole
// UI, and spaces inside a word are free.
//
// Source order is the row's order, since `.varrow` places these three in three
// grid columns and nothing is positioned by hand — so it is also the tab order,
// and the × is after the fields it destroys rather than between them.
const varRow = (name = '', words = []) => `
  <div class="varrow">
    <input class="varname" value="${esc(name)}" placeholder="animal"
      aria-label="Variable name">
    <input class="varwords" value="${esc(words.join(', '))}"
      placeholder="crab, horse, dog, cat" aria-label="Words, comma separated">
    <button type="button" class="btn btn-danger" data-vardel
      title="Remove this variable">&times;</button>
  </div>`;

function control(f) {
  const id = fieldId(f);
  const attrs = ['min', 'max', 'step']
    .filter((a) => f[a] !== undefined)
    .map((a) => ` ${a}="${f[a]}"`)
    .join('');
  switch (f.kind) {
    case 'textarea':
      return `<prompt-box><textarea id="${id}"></textarea></prompt-box>`;
    case 'text':
      // The prompt defaults get the weighting shortcut too — they are the text
      // that lands in a prompt box, so `(cat:1.2)` has to be typeable here.
      return f.key === 'negative'
        ? `<prompt-box><input id="${id}"></prompt-box>`
        : `<input id="${id}">`;
    case 'lines':
      return `<textarea id="${id}" class="short" placeholder="one argument per line"></textarea>`;
    case 'check':
      return `<input type="checkbox" id="${id}">`;
    // The rows are drawn by `put` from what the server sent, and read back out
    // of the DOM by `take` — no shadow copy, because nothing here re-renders
    // while you are typing in it. Add and remove are one delegated listener in
    // `initSettings`, for the same reason the dirty check is.
    //
    // The button is above the rows, and a new row is prepended under it: the
    // list is the thing that grows, so a button at its foot walks away from the
    // pointer exactly as the list gets long enough for that to matter.
    case 'vars':
      return `<button type="button" class="btn varadd" data-varadd>Add variable</button>
        <div id="${id}" class="vars"></div>`;
    // The real control's own options, cloned rather than listed again — the
    // sampler list is declared in the markup and this is a view of it.
    case 'mirror':
      return `<select id="${id}">${$(f.from).innerHTML}</select>`;
    // A mirror too, but of a select filled from a fetch, so the options are
    // copied in by `load` once `upscalersReady` says they exist. What is drawn
    // here is only the option that needs no file.
    case 'upscaler':
      return `<select id="${id}"><option value="">FSR (EASU)</option></select>`;
    // A fixed list that belongs to no other control, values numeric.
    case 'choice':
      return `<select id="${id}">${f.options.map(([v, label]) =>
        `<option value="${v}">${esc(label)}</option>`).join('')}</select>`;
    default:
      return `<input type="number" id="${id}"${attrs}>`;
  }
}

// The markup for one container. Everything not in that pane is skipped, the
// `<details>` bookkeeping included — `folded` is only ever set by a section this
// pane actually drew, so the fold can't be opened in one container and closed in
// the other.
function markup(pane) {
  const out = [];
  let inFold = false;
  let active = 'setform';   // anything declared before the first section
  for (const f of FIELDS) {
    if (f.title) {
      if (inFold && active === pane) out.push('</details>');
      inFold = false;
      active = f.pane;
      if (active !== pane) continue;
      const id = `setfold-${f.title.toLowerCase().replace(/[^a-z0-9]+/g, '-')}`;
      out.push(`<details class="fold" id="${id}"${f.open ? ' open' : ''}><summary>${
        f.help ? `<label data-help="${f.help}">${f.title}</label>` : f.title}</summary>`);
      inFold = true;
      continue;
    }
    if (active !== pane) continue;
    if (f.sub) {
      out.push(`<label class="grouphead"${f.help ? ` data-help="${f.help}"` : ''}>${f.sub}</label>`);
      continue;
    }
    // The only field with no label of its own: the section heading above it is
    // the label, there being exactly one field under it, and "Prompt variables"
    // followed by "Variables" says it twice.
    if (f.kind === 'vars') {
      out.push(control(f));
      continue;
    }
    const label = f.label ?? labelOf(f.from);
    const help = f.help ?? f.from;
    if (f.kind === 'check') {
      // A checkbox labels itself from the right, like every other one on the
      // page — `.checks` is the rule that stops `label` being a block here.
      out.push(`<div class="checks"><label data-help="${help}">`
        + `${control(f)} ${label}</label></div>`);
    } else {
      out.push(`<label for="${fieldId(f)}" data-help="${help}">${label}</label>`);
      out.push(control(f));
    }
  }
  if (inFold) out.push('</details>');
  return out.join('\n');
}

// ---- Reading and writing the form ------------------------------------------

function put(f, v) {
  const el = $(fieldId(f));
  if (f.kind === 'check') {
    el.checked = !!v;
    return;
  }
  if (f.kind === 'vars') {
    // In the order the server sent them, which is alphabetical: the table is a
    // `BTreeMap`, so the file has a stable order and a save that changed
    // nothing produces no diff. A row added at the top therefore moves to where
    // its name belongs on the next save.
    el.innerHTML = Object.entries(v ?? {}).map(([name, words]) =>
      varRow(name, words)).join('');
    return;
  }
  if (f.kind === 'lines') {
    el.value = (v ?? []).join('\n');
    return;
  }
  // A select silently falls back to its first option when the stored value
  // isn't one of them — so a sampler this
  // build doesn't list, would be displayed as something else and then *saved*
  // as it. Carry the stray value as an option of its own instead: the form then
  // shows what is really stored, and changing it is a deliberate act.
  if (el.tagName === 'SELECT' && v != null
      && ![...el.options].some((o) => o.value === String(v))) {
    el.add(new Option(`${v} (from the file)`, String(v)));
  }
  el.value = v ?? '';
}

function take(f) {
  const el = $(fieldId(f));
  if (f.kind === 'check') return el.checked;
  if (f.kind === 'vars') {
    const out = {};
    for (const row of el.querySelectorAll('.varrow')) {
      // A row whose name is still empty is one that was just added, not an
      // answer — skipping it is what stops a half-typed variable from blocking
      // Save. The server normalises too; this is only so that what is sent and
      // what comes back agree, and the dirty check compares them.
      const name = row.querySelector('.varname').value.trim().toLowerCase();
      if (!name) continue;
      out[name] = row.querySelector('.varwords').value
        .split(',').map((w) => w.trim()).filter(Boolean);
    }
    return out;
  }
  if (f.kind === 'lines') {
    return el.value.split('\n').map((s) => s.trim()).filter(Boolean);
  }
  // The empty option is FSR, which the file spells as no key at all.
  if (f.kind === 'upscaler') return $(fieldId(f)).value || null;
  if (f.kind === 'choice') return Number(el.value);
  if (f.kind === 'textarea' || f.kind === 'text' || f.kind === 'mirror') {
    const s = el.value;
    return f.blank ? (s.trim() || null) : s;
  }
  const raw = el.value.trim();
  // An empty number box is `null` where the field admits one and the stored
  // value where it doesn't, because sending `0` for an emptied Steps box would
  // be the form inventing an answer the user didn't give.
  if (!raw) return f.blank ? null : undefined;
  const n = parseFloat(raw);
  return Number.isFinite(n) ? n : undefined;
}

const rows = () => FIELDS.filter((f) => !f.title && !f.sub);

function fill(s) {
  for (const f of rows()) put(f, s[f.key]);
}

function body() {
  const out = {};
  for (const f of rows()) {
    const v = take(f);
    // `undefined` is "the box says nothing usable"; leaving the key out lets the
    // server keep what it has rather than writing a guess over it.
    if (v !== undefined) out[f.key] = v;
  }
  return out;
}

// ---- What the rest of the page does with them -------------------------------

// The three settings that are not per-model. They seed controls on other tabs,
// so they are applied once at load and again after a save.
//
// The prompt boxes are only written while they still hold what the last apply
// put there — which at boot is the empty string, since the markup no longer
// ships a default in a `value=`. The fetch is async, and a default arriving
// after someone has started typing must not take the sentence away from them.
const SEEDS = [['prompt', ['prompt']], ['negative', ['negative']]];
const applied = { prompt: '', negative: '' };

function applyPageDefaults(s) {
  for (const [key, ids] of SEEDS) {
    for (const id of ids) {
      if ($(id).value === applied[key]) $(id).value = s[key] ?? '';
    }
    applied[key] = s[key] ?? '';
  }
  // Only the box's starting point, so it is safe to reapply after a save: the
  // factor the user picked for the image in hand is the same control.
  if (s.hires_scale) setUpFactor(s.hires_scale);
}

// What a nightly feature's setting shows or hides, applied from what the server
// stored — on load and after every save, so ticking it is enough. The Video tab
// and the *Clear videos* row go together: a setting for clips you can't make
// would be a question nobody can answer. Leaving the tab while it hides is the
// one move it makes for you.
function applyNightly(s) {
  const on = !!s.nightly_video;
  const tab = document.querySelector('.tab[data-tab="video"]');
  if (tab) {
    if (!on && tab.classList.contains('active')) {
      document.querySelector('.tab[data-tab="image"]')?.click();
    }
    tab.hidden = !on;
  }
  const clear = $(fieldId(FIELDS.find((f) => f.key === 'clear_videos_on_close')));
  if (clear) clear.closest('.checks').hidden = !on;
}

// ---- Unsaved changes --------------------------------------------------------

// What the form looked like the last time it agreed with the file. Compared as
// JSON rather than field by field because `body()` already reduces the form to
// exactly the values that would be sent — so this asks the only question worth
// asking: would saving now change anything?
let saved = null;

// Settings are the one form on the page you can walk away from mid-edit: every
// other panel either acts immediately or is consumed by the button next to it.
// So the mark goes on the tab as well as under the button — a note visible only
// on the tab you have left is not a note. The tab spells it out rather than
// carrying a dot; the stylesheet's `.tab.unsaved` rule is that half.
function markDirty() {
  const dirty = saved !== null && JSON.stringify(body()) !== saved;
  $('setmsg').textContent = dirty ? 'Unsaved changes' : '';
  $('setmsg').classList.toggle('unsaved', dirty);
  document.querySelector('.tab[data-tab="settings"]').classList.toggle('unsaved', dirty);
  return dirty;
}

// A value something else on the page has already saved — the out-of-memory
// box's "keep N GB of the card free". Written into its control *and* into the
// last-saved snapshot, so the next Save can't quietly put the old number back
// and the form doesn't call it an unsaved edit. Any edit in progress elsewhere
// in the form is left as it is.
export function adoptSaved(key, value) {
  const f = rows().find((r) => r.key === key);
  if (!f || saved === null) return;
  put(f, value);
  const snap = JSON.parse(saved);
  snap[key] = take(f);
  saved = JSON.stringify(snap);
  markDirty();
}

function settle(s) {
  fill(s);
  applyPageDefaults(s);
  applyNightly(s);
  saved = JSON.stringify(body());
  markDirty();
}

// ---- Load and save ----------------------------------------------------------

async function load() {
  try {
    const s = await (await fetch('/settings')).json();
    // Before `settle`, which `put`s the stored name — into a select that has to
    // offer it by then, or `put` would add it as a stray from the file.
    await upscalersReady;
    $('set-upscaler').innerHTML = upscalerOptions;
    settle(s);
    // A file that won't parse still answers 200 with the built-ins, so the form
    // is editable and saving it is the repair. See `handle_get_settings`.
    $('seterr').textContent = s.error ?? '';
  } catch (e) {
    $('seterr').textContent = 'Could not read settings: ' + e;
  }
}

// Two rows with the same name, which is the one error the server cannot see:
// `take` builds an object and JSON collapses duplicate keys silently, so by the
// time the body is written one of the two rows has already been lost.
// Everything else about the table is checked on the server as well, against the
// file a text editor could have left.
function duplicateName() {
  const seen = new Set();
  for (const box of $('set-variables').querySelectorAll('.varname')) {
    const name = box.value.trim().toLowerCase();
    if (!name) continue;
    if (seen.has(name)) return name;
    seen.add(name);
  }
  return null;
}

async function save() {
  const dup = duplicateName();
  if (dup) {
    $('seterr').textContent = `'${dup}' is defined twice — variable names `
      + 'ignore case, so two spellings of one name are one variable.';
    return;
  }
  $('seterr').textContent = '';
  $('setmsg').classList.remove('unsaved');
  $('setmsg').textContent = 'Saving…';
  try {
    // Filled from what came *back*, not from what went out: the server is what
    // decides what is stored, and a field it normalised has to show up here.
    settle(await post('/settings', body()));
    $('setmsg').textContent = 'Saved.';
    // The Advanced panel on the Image tab is filled from the model list, and the
    // model list is built from these numbers — so it is stale the moment this
    // returns. Rebuilt rather than reloaded: `/models` is served live.
    loadModels();
    // And every model's base moved with it, which is what the Per model
    // section's placeholders show.
    loadModelSettings();
  } catch (e) {
    $('setmsg').textContent = '';
    markDirty();
    $('seterr').textContent = String(e.message || e);
  }
}

export function initSettings() {
  // Built synchronously so `wireHints` finds these labels in its first pass;
  // the values arrive from the fetch below, which cannot land before boot ends.
  //
  // Two containers, one table: the variables are in the other column and
  // everything else is the form. `fill`, `body` and the dirty check don't know
  // that — they address a control by its id, which is unique to the document
  // rather than to a pane.
  $('setform').innerHTML = markup('setform');
  $('setvarbox').innerHTML = markup('setvarbox');
  rememberFolds($('tab-settings'));
  wireHints($('tab-settings'));
  $('setsave').addEventListener('click', save);
  // Delegated to the *tab*, not to the form, because the panes are two elements
  // and one listener on their common parent is the whole reason the split cost
  // nothing here. A field added to FIELDS is still watched with no second edit,
  // in whichever pane it lands. `input` covers typing and the checkbox;
  // `change` catches a select in the browsers that don't fire `input` for one.
  for (const ev of ['input', 'change']) {
    $('tab-settings').addEventListener(ev, markDirty);
  }
  // Adding and removing a variable row, delegated for the same two reasons: one
  // listener that survives the rows being redrawn by a save, and one that spans
  // both panes. A row is inserted or detached rather than the list being
  // re-rendered, so every other row keeps exactly what is typed in it.
  $('tab-settings').addEventListener('click', (e) => {
    const del = e.target.closest('[data-vardel]');
    const add = e.target.closest('[data-varadd]');
    if (!del && !add) return;
    if (del) {
      del.closest('.varrow').remove();
    } else {
      // Under the button that was just pressed, which is above the list.
      $('set-variables').insertAdjacentHTML('afterbegin', varRow());
      $('set-variables').firstElementChild.querySelector('.varname').focus();
    }
    markDirty();
  });
  // The page holds the only copy of an unsaved edit, so a reload would take it
  // with no way back. Only armed while there is something to lose — and not
  // once Close server has replaced the page, when there is no server left to
  // save to and the form itself is gone.
  window.addEventListener('beforeunload', (e) => {
    if (!$('tab-settings')) return;
    if (markDirty()) e.preventDefault();
  });
  load();
}
