import { $, esc, getJSON, post } from './dom.js';
import { FIELDS, labelOf } from './settings.js';
import { loadModels, upscalersReady, upscalerOptions } from './image.js';
import { wireHints } from './hints.js';

// The Settings tab's *Per model* section: `user/model-settings.toml` as a form.
//
// One model at a time. Every box is an **override**: empty means "start where
// this model would anyway", and the placeholder says what that is — the
// server's answer (`GET /model-settings` serves each model's `base`), not one
// re-derived here, since the layering (settings, family recipe, template) is
// the server's to know. A box set to the same number as its placeholder is
// dropped on save for the same reason: the file keeps only real differences,
// so a model stays linked to the global form for everything else.
//
// The rows are `settings.js`'s own `FIELDS`, filtered to the knobs a model can
// override, so a label, a tooltip and a range are declared once for both forms.
// Not in that table's form itself because it is a different file with a
// different save, and one Save button writing two files would make "Unsaved
// changes" mean two things.

// Page-level rows, which no model has an opinion about, are marked `page: true`
// on the `FIELDS` row itself. This used to be a second list kept here by hand;
// it fell behind the server's `PAGE_ONLY` (max_image_side and the rows after
// it), every save then carried a key the server refuses for a model, and
// saving per model failed for every model. `fields_mark_exactly_the_page_only_keys`
// now checks the marks against the server's list.
// A function, not a constant, and that is load-bearing: `settings.js` imports
// this module back, so reading `FIELDS` at the top level runs before that
// module has declared it — a ReferenceError that takes the whole module graph,
// and every tab, down with it. The CLAUDE.md rule "a module's top level
// declares and nothing else" is exactly this.
const rows = () => FIELDS.filter((f) => f.key && !f.page);

const idOf = (f) => `pm-${f.key}`;

// The listing as the server last sent it, and the model on screen.
let listing = [];
let current = null;

// What an empty box falls back to, as words. `null` in a base is a knob that
// follows another one (the per-pass CFGs), which is what the global form's
// empty box means too.
function shown(f, v) {
  if (v === null || v === undefined) return f.key.endsWith('cfg_scale') ? 'follows CFG' : '';
  if (f.kind === 'lines') return v.length ? v.join(' ') : 'none';
  if (f.kind === 'check') return v ? 'on' : 'off';
  if (f.kind === 'upscaler') return v || 'FSR (EASU)';
  return String(v);
}

function control(f) {
  const id = idOf(f);
  const attrs = ['min', 'max', 'step']
    .filter((a) => f[a] !== undefined)
    .map((a) => ` ${a}="${f[a]}"`)
    .join('');
  switch (f.kind) {
    // Three states, because this layer can say "on", "off" or nothing.
    case 'check':
      return `<select id="${id}"><option value=""></option>
        <option value="true">on</option><option value="false">off</option></select>`;
    case 'mirror':
      return `<select id="${id}"><option value=""></option>${$(f.from).innerHTML}</select>`;
    // Its options are copied in once `upscalersReady` says they exist. The
    // empty value here is "no override", not FSR — so FSR can't be pinned over
    // a global upscaler from this layer, which the file format can't express
    // either (an absent key and FSR are both "no name").
    case 'upscaler':
      return `<select id="${id}"><option value=""></option></select>`;
    case 'lines':
      return `<textarea id="${id}" class="short"></textarea>`;
    case 'text':
      return `<input id="${id}">`;
    default:
      return `<input type="number" id="${id}"${attrs}>`;
  }
}

function markup() {
  const html = rows().map((f) => {
    const label = f.label ?? labelOf(f.from);
    const help = f.help ?? f.from;
    return `<label for="${idOf(f)}" data-help="${help}">${label}</label>${control(f)}`;
  }).join('\n');
  // The heading and the Save / Clear buttons are the panel's own
  // (`#panel-set-per-model` in index.html): its header and pinned footer.
  return `<label for="pmmodel">Model</label>
    <select id="pmmodel"></select>
    <p class="sub" id="pmnote"></p>
    <div id="pmparts"></div>
    <div id="pmrows">${html}</div>`;
}

// ---- Reading and writing ---------------------------------------------------

// The model's slots — VAE, text encoder — as selects. Empty is "no choice
// saved", and its option says what that falls back to: the file the scan picks,
// or on a bundled checkpoint the one built into it. Redrawn per model, since
// the slots are the family's.
function fillParts(m) {
  $('pmparts').innerHTML = m.parts.map((p) => {
    const fallback = p.auto ?? (p.optional ? 'built into the checkpoint' : 'none on disk');
    const saved = m.parts_over[p.slot];
    const opts = [`<option value="">— default: ${esc(fallback)} —</option>`,
      ...p.options.map((o) => `<option value="${esc(o.name)}">${esc(o.name)}</option>`)];
    if (saved && !p.options.some((o) => o.name === saved)) {
      opts.push(`<option value="${esc(saved)}">${esc(saved)} (missing)</option>`);
    }
    return `<label for="pmpart-${p.slot}">${esc(p.label)}</label>
      <select id="pmpart-${p.slot}" data-slot="${p.slot}">${opts.join('')}</select>`;
  }).join('');
  for (const p of m.parts) $(`pmpart-${p.slot}`).value = m.parts_over[p.slot] ?? '';
  wireHints($('pmparts'));
}

// Every slot, `null` for the empty choice — which forgets a saved one.
const partValues = () => Object.fromEntries(
  [...$('pmparts').querySelectorAll('select')].map((s) => [s.dataset.slot, s.value || null]));

function fill() {
  const m = listing.find((x) => x.name === current);
  if (!m) return;
  fillParts(m);
  const n = Object.keys(m.over).length + Object.keys(m.parts_over).length;
  $('pmnote').textContent = `${m.family_label}. `
    + (n ? `${n} setting${n === 1 ? '' : 's'} saved for this model; ` : 'Nothing saved for this model; ')
    + 'an empty box starts where the placeholder says.';
  for (const f of rows()) {
    const el = $(idOf(f));
    const v = m.over[f.key];
    const fallback = shown(f, m.base[f.key]);
    if (el.tagName === 'SELECT') {
      el.options[0].textContent = `— default: ${fallback} —`;
      // A stored value the select doesn't list (a removed upscaler, a sampler
      // this build hasn't got) would show as the blank default, and the next
      // Save would then delete it. Carried as an option of its own, as the
      // Settings form does — and dropped again when another model is shown,
      // since this select is shared by all of them.
      for (const o of [...el.options]) if (o.dataset.stray) o.remove();
      if (v !== undefined && ![...el.options].some((o) => o.value === String(v))) {
        const o = new Option(`${v} (from the file)`, String(v));
        o.dataset.stray = '1';
        el.add(o);
      }
      el.value = v === undefined ? '' : String(v);
    } else if (f.kind === 'lines') {
      el.placeholder = `default: ${fallback}`;
      el.value = (v ?? []).join('\n');
    } else {
      el.placeholder = fallback;
      el.value = v ?? '';
    }
  }
  $('pmmsg').textContent = '';
}

// Every row, as the server wants it: a value, or `null` for "no override".
// Sending all of them is what makes an emptied box a removal.
function values() {
  const out = {};
  for (const f of rows()) {
    const raw = $(idOf(f)).value.trim();
    if (raw === '') {
      out[f.key] = null;
    } else if (f.kind === 'check') {
      out[f.key] = raw === 'true';
    } else if (f.kind === 'lines') {
      out[f.key] = raw.split('\n').map((s) => s.trim()).filter(Boolean);
    } else if (['mirror', 'upscaler', 'text'].includes(f.kind)) {
      out[f.key] = raw;
    } else {
      const n = parseFloat(raw);
      out[f.key] = Number.isFinite(n) ? n : null;
    }
  }
  return out;
}

function fillSelect() {
  const sel = $('pmmodel');
  const families = [...new Set(listing.map((m) => m.family_label))].sort();
  sel.innerHTML = families.map((fam) => `<optgroup label="${esc(fam)}">${
    listing.filter((m) => m.family_label === fam).map((m) =>
      `<option value="${esc(m.name)}">${esc(m.name)}${
        Object.keys(m.over).length + Object.keys(m.parts_over).length ? ' (tuned)' : ''
      }</option>`).join('')}</optgroup>`).join('');
  if (!listing.some((m) => m.name === current)) current = listing[0]?.name ?? null;
  if (current) sel.value = current;
}

function take(data) {
  listing = data.models ?? [];
  fillSelect();
  fill();
  $('pmerr').textContent = data.error ?? '';
}

// Exported because a global Save moves every model's base, so the placeholders
// here are stale the moment one lands.
export async function loadModelSettings() {
  try {
    take(await getJSON('/model-settings'));
  } catch (e) {
    $('pmerr').textContent = 'Could not read per-model settings: ' + e;
  }
}

async function save(body, parts) {
  $('pmerr').textContent = '';
  $('pmmsg').textContent = 'Saving…';
  try {
    // Redrawn from the reply: the server drops what equals the default, and
    // the form should show that rather than what was typed.
    take(await post('/model-settings', { model: current, values: body, parts }));
    $('pmmsg').textContent = 'Saved.';
    // The Image tab's pre-fill and the select's "tuned" tag come from /models.
    loadModels();
  } catch (e) {
    $('pmmsg').textContent = '';
    $('pmerr').textContent = String(e.message || e);
  }
}

export function initModelSettings() {
  $('pmbox').innerHTML = markup();
  wireHints($('pmbox'));
  $('pmmodel').addEventListener('change', () => {
    current = $('pmmodel').value;
    fill();
  });
  $('pmsave').addEventListener('click', () => save(values(), partValues()));
  $('pmclear').addEventListener('click', () =>
    save(Object.fromEntries(rows().map((f) => [f.key, null])),
      Object.fromEntries([...$('pmparts').querySelectorAll('select')]
        .map((s) => [s.dataset.slot, null]))));
  for (const id of ['pmrows', 'pmparts']) {
    $(id).addEventListener('input', () => { $('pmmsg').textContent = 'Unsaved changes'; });
  }
  // The upscaler options exist once their fetch has landed; copied in behind
  // the "default" option, which `fill` relabels.
  upscalersReady.then(() => {
    $(idOf({ key: 'upscaler' })).insertAdjacentHTML('beforeend',
      upscalerOptions.replace('<option value="">FSR (EASU)</option>', ''));
    loadModelSettings();
  });
}
