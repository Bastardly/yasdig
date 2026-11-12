import { $, esc, getJSON, post } from './dom.js';
import { FIELDS, labelOf } from './settings.js';
import { loadModels, upscalersReady, upscalerOptions } from './image.js';
import { wireHints } from './hints.js';
import { loadVideoModels } from './video.js';

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

// A video model's boxes: what the Video tab's *Save as defaults* writes
// (`settings::VIDEO_KNOBS`), labelled and explained as the Video tab's own
// controls are (`from`). `only` names the family flag a row needs (`moe`,
// `ltx`, `narrated`); `diffusion` rows are left off narrated stills, which
// keep only size, fps, a language and a voice.
const VROWS = [
  { key: 'width', from: 'vw', min: 64 },
  { key: 'height', from: 'vh', min: 64 },
  { key: 'frames', label: 'Frames (a new shot’s length)', min: 1, diffusion: true },
  { key: 'fps', from: 'vfps', min: 1, max: 60 },
  { key: 'steps', from: 'vsteps', min: 1, max: 100, diffusion: true },
  { key: 'cfg_scale', from: 'vcfg', step: 0.1, min: 0, diffusion: true },
  { key: 'sampler', from: 'vsampler', kind: 'mirror', diffusion: true },
  { key: 'flow_shift', from: 'vshift', step: 0.5, min: 0, diffusion: true },
  { key: 'high_noise_steps', from: 'vhsteps', min: 0, max: 100, only: 'moe' },
  { key: 'high_noise_cfg_scale', from: 'vhcfg', step: 0.1, min: 0, only: 'moe' },
  { key: 'hires', label: 'Hi-res ×2', help: 'vhires', kind: 'check', only: 'ltx' },
  { key: 'vae_tiling', label: 'VAE tiling', help: 'vtiling', kind: 'check', diffusion: true },
  { key: 'memory', from: 'vmemory', kind: 'mirror', diffusion: true },
  { key: 'negative_prompt', from: 'vnegative', kind: 'para', diffusion: true },
  { key: 'language', from: 'vlang', kind: 'lang', only: 'narrated' },
  { key: 'narrator', label: 'Default voice (a sound’s name in Outputs)', help: 'vnarrator', kind: 'text', only: 'narrated' },
];
const vidOf = (f) => `pmv-${f.key}`;
const applies = (f, m) => (f.only ? !!m[f.only] : !(f.diffusion && m.narrated));

// The listing as the server last sent it — image models and video models, one
// list — and the model on screen as its select value: `i:` or `v:` and the
// name, since a name may be both.
let listing = [];
let current = null;
const keyOf = (m) => `${m.video ? 'v' : 'i'}:${m.name}`;
const shownModel = () => listing.find((m) => keyOf(m) === current);

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

function control(f, id = idOf(f)) {
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
      return `<select id="${id}"><option value=""></option>${f.from.startsWith('v') ? '' : $(f.from).innerHTML}</select>`;
    // Its options are copied in once `upscalersReady` says they exist. The
    // empty value here is "no override", not FSR — so FSR can't be pinned over
    // a global upscaler from this layer, which the file format can't express
    // either (an absent key and FSR are both "no name").
    case 'upscaler':
      return `<select id="${id}"><option value=""></option></select>`;
    case 'lines':
    case 'para':
      return `<textarea id="${id}" class="short"></textarea>`;
    // Filled per model: a mirror's source may fill late (the samplers), and
    // a narrated model's languages are its own.
    case 'lang':
      return `<select id="${id}"><option value=""></option></select>`;
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
  const vhtml = VROWS.map((f) => {
    const help = f.help ?? f.from;
    return `<div data-vrow="${f.key}"><label for="${vidOf(f)}"${help ? ` data-help="${help}"` : ''}>${
      esc(f.label ?? labelOf(f.from))}</label>${control(f, vidOf(f))}</div>`;
  }).join('\n');
  return `<label for="pmmodel">Model</label>
    <select id="pmmodel"></select>
    <p class="sub" id="pmnote"></p>
    <div id="pmparts"></div>
    <div id="pmrows">${html}</div>
    <div id="pmvrows" hidden>${vhtml}</div>`;
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

// One box, set to this model's override (`v`, `undefined` for none), its
// empty state saying what that falls back to.
function put(el, f, v, fallback) {
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
    // Said as a default, like the selects' "— default: … —": a bare number
    // in a grey box read as a disabled field.
    el.placeholder = fallback === '' ? '' : `default: ${fallback}`;
    el.value = v ?? '';
  }
}

// A box's value as the server wants it, or `null` for "no override".
function read(el, f) {
  const raw = el.value.trim();
  if (raw === '') return null;
  if (f.kind === 'check') return raw === 'true';
  if (f.kind === 'lines') return raw.split('\n').map((x) => x.trim()).filter(Boolean);
  if (['mirror', 'upscaler', 'text', 'para', 'lang'].includes(f.kind)) return raw;
  const n = parseFloat(raw);
  return Number.isFinite(n) ? n : null;
}

const saved = (m) => Object.keys(m.over).length + Object.keys(m.parts_over).length;

function fill() {
  const m = shownModel();
  if (!m) return;
  fillParts(m);
  $('pmrows').hidden = !!m.video;
  $('pmvrows').hidden = !m.video;
  const n = saved(m);
  $('pmnote').textContent = `${m.video ? 'Video · ' : ''}${m.family_label}. `
    + (n ? `${n} setting${n === 1 ? '' : 's'} saved for this model; ` : 'Nothing saved for this model; ')
    + 'an empty box uses the default it shows.'
    + (m.video ? ' The Video tab’s Save as defaults writes these too.' : '');
  if (m.video) fillVideo(m);
  else for (const f of rows()) put($(idOf(f)), f, m.over[f.key], shown(f, m.base[f.key]));
  $('pmmsg').textContent = '';
}

// A video model's rows: only the ones its family has, each select's options
// taken now (the Video tab fills its sampler list late; a narrated model's
// languages are its own).
function fillVideo(m) {
  for (const f of VROWS) {
    const row = document.querySelector(`[data-vrow="${f.key}"]`);
    row.hidden = !applies(f, m);
    if (row.hidden) continue;
    const el = $(vidOf(f));
    if (f.kind === 'mirror') el.innerHTML = `<option value=""></option>${$(f.from).innerHTML}`;
    if (f.kind === 'lang') {
      el.innerHTML = '<option value=""></option>'
        + (m.languages ?? []).map(([code, name]) => `<option value="${esc(code)}">${esc(name)}</option>`).join('');
    }
    let base = m.base[f.key];
    if (f.kind === 'lang') base = (m.languages ?? []).find(([c]) => c === (base ?? 'en'))?.[1] ?? base ?? 'English';
    if (f.kind === 'mirror' && base != null) base = [...$(f.from).options].find((o) => o.value === base)?.textContent ?? base;
    let fallback = shown(f, base);
    if (f.key === 'flow_shift' && fallback === '') fallback = 'sd.cpp’s own';
    if (f.key === 'narrator' && fallback === '') fallback = 'the voice model’s own';
    // Wan's published negative runs to 400 characters of Chinese.
    if (f.kind === 'para' && fallback.length > 60) fallback = `${fallback.slice(0, 60)}…`;
    put(el, f, m.over[f.key], fallback);
  }
}

// Every row, as the server wants it: a value, or `null` for "no override".
// Sending all of them is what makes an emptied box a removal.
function values() {
  return Object.fromEntries(rows().map((f) => [f.key, read($(idOf(f)), f)]));
}

// A video model's rows that apply to it — the others aren't its knobs.
function videoValues(m) {
  return Object.fromEntries(VROWS.filter((f) => applies(f, m)).map((f) => [f.key, read($(vidOf(f)), f)]));
}

function fillSelect() {
  const sel = $('pmmodel');
  // Image families first, then video, each group by its name.
  const groups = [...new Set(listing.map((m) => `${m.video ? 'v' : 'i'}${m.family_label}`))].sort();
  sel.innerHTML = groups.map((g) => {
    const video = g[0] === 'v';
    const fam = g.slice(1);
    return `<optgroup label="${esc(video ? `Video · ${fam}` : fam)}">${
      listing.filter((m) => !!m.video === video && m.family_label === fam).map((m) =>
        `<option value="${esc(keyOf(m))}">${esc(m.name)}${saved(m) ? ' (tuned)' : ''}</option>`).join('')}</optgroup>`;
  }).join('');
  if (!shownModel()) current = listing[0] ? keyOf(listing[0]) : null;
  if (current) sel.value = current;
}

function take(data) {
  listing = [...(data.models ?? []), ...(data.video ?? []).map((v) => ({ ...v, video: true }))];
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

async function save(body, parts, clear = false) {
  const m = shownModel();
  if (!m) return;
  $('pmerr').textContent = '';
  $('pmmsg').textContent = 'Saving…';
  try {
    if (m.video) {
      // The Video tab's own save: it answers `{tuned}`, not the listing.
      const r = await post('/model-settings', { kind: 'video', model: m.name, values: body, parts });
      await loadModelSettings();
      $('pmmsg').textContent = clear ? 'Cleared.'
        : r.tuned ? 'Saved.' : 'Nothing to save: every box is where this model starts anyway.';
      // The Video tab opens a model with its saved defaults.
      loadVideoModels({ refill: false });
      return;
    }
    // Redrawn from the reply: the server drops what equals the default, and
    // the form should show that rather than what was typed.
    take(await post('/model-settings', { model: m.name, values: body, parts }));
    $('pmmsg').textContent = 'Saved.';
    // The Image tab's pre-fill and the select's "tuned" tag come from /models.
    loadModels();
  } catch (e) {
    $('pmmsg').textContent = '';
    $('pmerr').textContent = String(e.message || e);
  }
}

const noParts = () => Object.fromEntries([...$('pmparts').querySelectorAll('select')].map((x) => [x.dataset.slot, null]));

export function initModelSettings() {
  $('pmbox').innerHTML = markup();
  wireHints($('pmbox'));
  $('pmmodel').addEventListener('change', () => {
    current = $('pmmodel').value;
    fill();
  });
  $('pmsave').addEventListener('click', () => {
    const m = shownModel();
    if (m) save(m.video ? videoValues(m) : values(), partValues());
  });
  $('pmclear').addEventListener('click', () => {
    const m = shownModel();
    if (!m) return;
    save(Object.fromEntries((m.video ? VROWS : rows()).map((f) => [f.key, null])), noParts(), true);
  });
  for (const id of ['pmrows', 'pmvrows', 'pmparts']) {
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
