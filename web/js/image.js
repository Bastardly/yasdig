import { $, bar, esc, post, numOr } from './dom.js';
import { S, P, escErr } from './state.js';
import { setBusy, setIdle, onIdle, redrawBusy } from './busy.js';
import { cancelPost, setPost, setPostStart, setViewerImage, showFullSize } from './viewer.js';
import { onJobs, onNotice, onResult, runningJob, soonJobs, stopJob } from './jobs.js';
import { loadWorkItems, markGrid } from './workitems.js';
import { loraPicker } from './loras.js';
import { sourceSlot } from './source-slot.js';
import { wireHints } from './hints.js';
import { openRedraw, syncRedraw } from './redraw.js';
import { onRefsChange, references, refsActive, syncRefs } from './refs.js';

// The checkpoint list the server published. Exported because the work-item
// grid looks a name up in it; only this module refills it.
export let models = [];

// This tab's attached LoRAs. Exported because `applyWorkItem` has to put a
// reopened generation's back: the grid restores what made an image, and the
// adapters are as much a part of that as the steps and the seed.
export const imageLoras = loraPicker({
  rows: 'loras', add: 'loraadd', family: () => currentModel()?.family ?? null,
  onChange: () => syncSummaries(),
});

// "Start from". Nothing picked is text to image; a picked image makes Generate
// run `/img2img` over it instead of `/generate`. This used to be a tab of its
// own, which duplicated the prompt, model, LoRAs and the whole escalation
// panel for the sake of one image slot — two copies of every control, and a
// LoRA chosen on one tab silently absent on the other. Exported because a
// reopened work item puts its source back.
export const imageSource = sourceSlot({
  prefix: 'src',
  tab: 'tab-image',
  // Nothing to say with nothing picked: the fold's summary already says "off".
  none: '',
  err: 'err',
  // A source arriving opens its fold — a pick, upload, drop, paste or reopened
  // work item. The summary would say "from …" anyway; opening it also puts
  // Strength in front of the person who just asked for a pass over a picture.
  onChange: (stem) => {
    if (stem) $('fold-src').open = true;
    syncMode();
  },
});

// Which controls mean something depends on whether there is a source, so the
// ones that don't are hidden rather than left looking live: base size and the
// output upscale describe a render from noise, and the strength and working
// size describe a pass over a picture. Steps and CFG serve both. `data-when` in
// the markup says which is which.
function syncMode() {
  const source = !!imageSource.get();
  for (const el of $('tab-image').querySelectorAll('[data-when]')) {
    el.hidden = (el.dataset.when === 'source') !== source;
  }
  syncCfgPlaceholders();
  syncSrcNote();
  syncSummaries();
}

// Both this and the pass itself round to 64, and they must agree: this is what
// the picture is scaled to, that is what the latent grid allows.
const WORKING_SIDES = [768, 1024, 1536, 2048, 3072];

function currentModel() {
  return models.find((m) => m.name === $('template').value);
}

// ---- The Models fold -------------------------------------------------------
//
// One select per separate file the selected model's family runs with. The
// slots and the files that fit each come from `/models` (`parts`), already
// filtered — which VAE decodes which family is the server's to know, the
// `frame_grid` rule. A slot is `optional` on a bundled checkpoint, where the
// empty choice is the one built into it; otherwise it must name a file.

const partSelects = () => [...$('partrows').querySelectorAll('select')];
const baseName = (name) => name.split('/').pop();

function partOptions(part) {
  const opts = part.optional
    ? ['<option value="">Built into the checkpoint</option>'] : [];
  for (const o of part.options) {
    opts.push(`<option value="${esc(o.name)}" data-path="${esc(o.path)}">${esc(o.name)}</option>`);
  }
  // What the model holds and nothing on disk offers: a saved choice whose file
  // was deleted, or a required part not downloaded yet. Listed so the select
  // names it rather than silently showing another file.
  if (part.current && !part.options.some((o) => o.name === part.current)) {
    opts.push(`<option value="${esc(part.current)}" data-path="${esc(part.current_path ?? '')}">`
      + `${esc(part.current)} (missing)</option>`);
  }
  return opts.join('');
}

// Redrawn from the selected model. Exported for `applyWorkItem`, which sets the
// model without firing `change` and then puts back the parts the image ran with.
export function syncParts() {
  const parts = currentModel()?.parts ?? [];
  $('partrows').innerHTML = parts.map((p) => `
    <label for="part-${p.slot}">${esc(p.label)}</label>
    <select id="part-${p.slot}" data-slot="${p.slot}">${partOptions(p)}</select>`).join('');
  for (const p of parts) $(`part-${p.slot}`).value = p.current ?? '';
  for (const sel of partSelects()) sel.addEventListener('change', syncModelsSummary);
  wireHints($('partrows'));
  syncModelsSummary();
}

// Put back the files a work item ran with, from its profile's absolute paths —
// what a sidecar records. A path no option carries anymore is added as missing
// rather than dropped, the way `setUpscaler` treats a deleted upscaler.
export function setParts(components) {
  for (const sel of partSelects()) {
    const path = components?.[sel.dataset.slot] ?? null;
    if (!path) {
      if ([...sel.options].some((o) => o.value === '')) sel.value = '';
      continue;
    }
    let opt = [...sel.options].find((o) => o.dataset.path === path);
    if (!opt) {
      opt = new Option(`${path} (missing)`, path);
      opt.dataset.path = path;
      sel.add(opt);
    }
    sel.value = opt.value;
  }
  syncModelsSummary();
}

// Every slot, including an emptied one as `null`: the fold is the complete
// answer to what the model runs with, the same argument `loras` makes.
function partsPicked() {
  return Object.fromEntries(partSelects().map((s) => [s.dataset.slot, s.value || null]));
}

// The summary says what is in the fold, so folding it hides nothing.
function syncModelsSummary() {
  const m = currentModel();
  if (!m) {
    $('modelsum').textContent = '';
    return;
  }
  const bits = partSelects().map((s) => {
    const label = m.parts.find((p) => p.slot === s.dataset.slot)?.label ?? s.dataset.slot;
    return `${label}: ${s.value ? baseName(s.value) : 'built in'}`;
  });
  // The LoRAs live in this fold too, so the line names them — by file name
  // without its extension, the way the rows' own labels read.
  const loras = imageLoras.get();
  if (loras.length) {
    bits.push(`LoRA: ${loras.map((l) => baseName(l.name).replace(/\.[^.]+$/, '')).join(', ')}`);
  }
  $('modelsum').textContent = '— ' + [m.name, ...bits].join(' · ');
  // The line is cut to fit (`.fold > summary`), so the whole of it is the title.
  $('modelsum').title = [m.name, ...bits].join('\n');
}

// ---- The folds ---------------------------------------------------------------
//
// Every section of the panel is a <details>, and every summary carries a line
// saying what is inside — the rule the Models fold set, applied to all of them,
// so folding a section never hides what Generate is about to do. One function
// writes every line, calling out for the Models one, which `syncParts` also
// redraws. It runs from a delegated `input`/`change` on the column, so a new
// box in a fold needs no listener of its own.

// The checkpoint a pass will run: the one picked for it, or the one generating.
const passModel = (sel) => models.find((m) => m.name === $(sel).value) ?? currentModel();

function syncSummaries() {
  // Before `/models` answers every box is empty, and "× · steps" says nothing.
  if (!currentModel()) return;
  // The line is cut to fit (`.fold > summary`), so the whole of it is the title.
  const say = (id, text) => {
    $(id).textContent = text ? '— ' + text : '';
    $(id).title = text ? text.split(' · ').join('\n') : '';
  };
  const source = imageSource.get();

  // The count rides here now that Images and the loop are in this fold, and in
  // the accent when it is more than one: a folded fold must not hide that the
  // next press starts a batch, or a loop that runs until Stop.
  const n = runTotal();
  // Counted only where they'll be sent: a model that takes them, no source.
  const refs = refsActive() && !source ? references().length : 0;
  say('sum-gen', [
    n === Infinity ? 'until stopped' : n > 1 ? `${n} images` : null,
    source ? null : `${$('w').value}×${$('h').value}`,
    `${$('steps').value} steps`, `CFG ${$('cfg').value}`, $('sampler').value,
    // Auto is the model's own and says nothing worth the space.
    $('scheduler').value === 'auto' ? null : $('scheduler').value,
    $('cachemode').value === 'off' ? null : `${$('cachemode').value} cache`,
    refs ? `${refs} ref${refs > 1 ? 's' : ''}` : null,
  ].filter(Boolean).join(' · '));
  // A step cache trades exactness for time, and references change what is
  // drawn: departures from the plain path a folded section must not hide.
  $('sum-gen').classList.toggle('on', n > 1 || $('cachemode').value !== 'off' || refs > 0);

  const after = [
    $('opthires').checked ? 'hi-res' : null,
    $('optrefine').checked ? 'refine' : null,
    $('optsave').checked ? 'save' : null,
  ].filter(Boolean);
  const sel = $('upscaler');
  const by = sel.value ? sel.options[sel.selectedIndex]?.textContent ?? sel.value : 'FSR';
  say('sum-after', [
    source ? null : `${$('upscale').value}× via ${by}`,
    after.length ? after.join(' → ') : 'nothing after',
  ].filter(Boolean).join(' · '));
  // Each of these spends a pass of GPU time on every image of a run.
  $('sum-after').classList.toggle('on', after.length > 0);

  say('sum-src', source ? `from ${source} · strength ${$('srcstrength').value}` : 'off');
  // In the accent rather than dim, like every summary below that marks a
  // departure from the plain path: a picked source changes what Generate
  // *does*, and read as just more grey small print.
  $('sum-src').classList.toggle('on', !!source);

  // Whole: the summary line cuts it to the width there is, and the title holds
  // all of it.
  say('sum-prompt', $('prompt').value.trim());

  syncModelsSummary();

  for (const [id, steps, sel] of [['sum-hires', 'hsteps', 'hiresmodel'],
                                  ['sum-refine', 'rsteps', 'refmodel']]) {
    const n = parseInt($(steps).value, 10) || 0;
    say(id, n > 0
      ? `${n} steps${id === 'sum-hires' ? ` · ${S.upFactor}×` : ''} · ${$(sel).value || 'same model'}`
      : 'off');
    // Another checkpoint finishing the picture is easy to forget once folded.
    // Compared with the diffusion model rather than tested for being set: a
    // pick that names the generating model is the same model, and stays dim.
    $(id).classList.toggle('on', !!$(sel).value && $(sel).value !== $('template').value);
  }
}

// Open or folded is a per-viewer convenience, so it lives in this browser;
// storage can be missing or throw, and a fold then simply starts as the markup
// has it — Generation, After each, Prompts and Models open, the source
// slot and the two passes folded, since those are reached for rather than used
// every time.
function initFolds() {
  for (const d of $('tab-image').querySelectorAll('details.fold[id]')) {
    const key = `rustimage.fold.${d.id}`;
    try {
      const v = localStorage.getItem(key);
      if (v !== null) d.open = v === '1';
    } catch { /* as the markup has it */ }
    d.addEventListener('toggle', () => {
      try { localStorage.setItem(key, d.open ? '1' : '0'); } catch { /* ignore */ }
    });
  }
}

// Pre-fill the Advanced controls from the selected model's defaults.
function syncAdvanced() {
  const m = currentModel();
  if (!m) return;
  // Re-greys the LoRA lists against the new model's family.
  imageLoras.refresh();
  // The References button is the model's to show (`references` on /models).
  syncRefs(m);
  syncParts();
  $('steps').value = m.steps;
  $('cfg').value = m.cfg_scale;
  $('sampler').value = m.sampler;
  $('scheduler').value = m.scheduler;
  $('cachemode').value = m.cache_mode;
  $('w').value = m.base_width;
  $('h').value = m.base_height;
  $('upscale').value = m.upscale_factor;
  setUpscaler(m.upscaler);
  fillPass('hires');
  fillPass('refine');
  setPost({ sharpness: m.sharpness, clarity: m.clarity });
  // The pass over a source. Filled whether or not one is picked, so picking one
  // later finds the model's number already there. Its steps and CFG are the
  // Steps and CFG scale boxes above.
  $('srcstrength').value = m.source_strength;
  // The working size starts at the model's hi-res budget: that is the same
  // question — how large can this card afford to diffuse — already answered
  // for this hardware, so the page doesn't invent a second answer to it. What
  // was picked survives a model switch.
  const want = parseInt($('srcmaxside').value, 10) || m.hires_max_side;
  $('srcmaxside').innerHTML = WORKING_SIDES.map((v) =>
    `<option value="${v}"${v === want ? ' selected' : ''}>${v} px${
      v === m.hires_max_side ? ' (model budget)' : ''}</option>`).join('');
  if (!WORKING_SIDES.includes(want)) $('srcmaxside').value = m.hires_max_side;
  syncCfgPlaceholders();
  syncOutSize();
  syncSrcNote();
  syncSummaries();
}

// Fill one escalation's three boxes from the checkpoint that will run it: the
// one picked in its fold, or the generating model with nothing picked. The
// server swaps the whole profile for the picked model's (`swap_model`) and
// folds these boxes on top, so filling them from the generating model — which
// is what this did before the pickers moved into the folds — sent that model's
// numbers to run on another.
//
// The CFG is left empty when the model doesn't set one, rather than pre-filled
// with the number it resolves to. Pre-filling would look friendlier and would
// be a lie: it pins the value, so moving the box it follows would silently stop
// carrying over to this pass.
function fillPass(which) {
  const [steps, strength, cfg, sel] = which === 'hires'
    ? ['hsteps', 'hstrength', 'hcfg', 'hiresmodel']
    : ['rsteps', 'rstrength', 'rcfg', 'refmodel'];
  const m = passModel(sel);
  if (!m) return;
  $(steps).value = m[`${which}_steps`];
  $(strength).value = m[`${which}_strength`];
  $(cfg).value = m[`${which}_cfg_scale`] ?? '';
}

// What the pass over a source will actually do, in a line. It runs the Steps
// box's count — all of them, with strength deciding how far up the schedule
// they start — at the CFG scale box, and a source bigger than the working size
// is scaled down first.
export function syncSrcNote() {
  if (!currentModel() || !imageSource.get()) return;
  const n = parseInt($('steps').value, 10) || 0;
  const s = parseFloat($('srcstrength').value) || 0;
  const cfg = parseFloat($('cfg').value);
  $('srcnote').textContent =
    `Runs ${n} step${n === 1 ? '' : 's'} at strength ${s}`
    + (Number.isFinite(cfg) ? ` · CFG ${cfg}` : '')
    + ` · a source over ${$('srcmaxside').value} px on its long side is scaled down first`;
}

// What the base size becomes after the enlarge, and what does it. The model
// select already quotes this sum for the model's own factor; this box is what
// moves the right-hand half of it, so the answer has to follow the box rather
// than the model. 1 is not a small upscale — nothing runs there, whichever
// upscaler is picked. The size is the same for every upscaler: a model's output
// is reduced or finished by EASU to land exactly here (`pipeline::enlarge`).
export function syncOutSize() {
  const f = parseFloat($('upscale').value);
  const w = parseInt($('w').value, 10);
  const h = parseInt($('h').value, 10);
  if (!Number.isFinite(f) || !Number.isFinite(w) || !Number.isFinite(h)) {
    $('outsize').textContent = '';
    return;
  }
  const sel = $('upscaler');
  const by = sel.value ? sel.options[sel.selectedIndex].textContent : 'FSR';
  $('outsize').textContent = f === 1
    ? `${w} × ${h} — no upscale`
    : `${w} × ${h} → ${Math.round(w * f)} × ${Math.round(h * f)} via ${by}`;
}

// A blank per-pass CFG box means "whatever the CFG it follows says", which is a
// thing the field has to admit to rather than decide quietly — so the number it
// resolves to goes in the placeholder, live, because the CFG it ends up
// following is often one field away and editable.
//
// The chain is the server's (`Profile::refine_cfg`): this pass's box, then the
// profile's `cfg_scale`. Which profile that is depends on the pass's model: the
// generating one retains the CFG scale box, while a model picked for the pass
// replaces the profile wholesale and brings its own.
export function syncCfgPlaceholders() {
  const v = parseFloat($('cfg').value);
  const base = Number.isFinite(v) ? v : null;
  for (const [id, sel] of [['rcfg', 'refmodel'], ['hcfg', 'hiresmodel']]) {
    const own = $(sel).value && passModel(sel);
    $(id).placeholder = (own ? own.cfg_scale : base) ?? '';
  }

  // Here because this already runs everywhere a CFG box changes — typed, a
  // model switch, a reopened work item, a source picked or cleared.
  syncNegative('negative', 'negnote', base);
}

// A negative prompt does nothing at CFG exactly 1. sd.cpp only evaluates the
// unconditioned half when the text CFG differs from the image CFG, which is 1
// unless an edit model sets it (`resolve_guidance`,
// `src/pipeline/request.cpp:274`) — so the rule is "≠ 1", not "> 1", and it is
// about the number rather than the family: SDXL-Turbo and a Z-Image turbo are
// both at 1, and a Z-Image base at CFG 5 uses its negative.
//
// The field is hidden, not cleared: what is in it is still sent, and comes back
// into force the moment CFG moves off 1. The note stays in its place, because a
// field that vanishes with no word about why reads as a bug.
function syncNegative(box, note, cfg) {
  const idle = cfg === 1;
  document.querySelector(`label[for="${box}"]`).hidden = idle;
  $(box).closest('prompt-box').hidden = idle;
  $(note).hidden = !idle;
}

// The files `GET /upscalers` lists, as the Upscaler select's options, behind an
// FSR option that needs no file. Each says its architecture, and one the server
// can't run (a HAT, a DAT) is listed *disabled* rather than hidden: it is in the
// folder, and a select that silently left it out would read as the file not
// having been found. A promise that settles once the
// select is filled, because two things must wait for it rather than race it:
// `syncAdvanced`, which sets the select's value, and the Settings form, whose
// row mirrors these options the way its sampler row mirrors the sampler's.
// Refetched with the model list, so a file dropped into the folder shows up
// after a Settings save as a checkpoint would.
export let upscalersReady = Promise.resolve();
// The options as the folder has them — without a "(missing)" one `setUpscaler`
// may have added for a reopened image, which is that image's business and not a
// choice for Settings to offer.
export let upscalerOptions = '<option value="">FSR (EASU)</option>';

export function loadUpscalers() {
  upscalersReady = (async () => {
    let list = [];
    try {
      const r = await fetch('/upscalers');
      if (r.ok) list = await r.json();
    } catch {
      // An empty list leaves FSR, which is the whole feature minus the option.
    }
    const sel = $('upscaler');
    const was = sel.value;
    upscalerOptions = '<option value="">FSR (EASU)</option>'
      + list.map((u) => (u.supported
        ? `<option value="${esc(u.name)}">${esc(u.label)} (${esc(u.arch)})</option>`
        : `<option value="${esc(u.name)}" disabled>${esc(u.label)} (${esc(u.arch)}, not supported)</option>`))
        .join('');
    sel.innerHTML = upscalerOptions;
    setUpscaler(was);
  })();
  return upscalersReady;
}

// Select a stored upscaler name, adding it if the folder no longer has it. A
// select shows its first option when nothing matches, which here would display
// FSR for an image that was made with a model — and then *send* FSR on the next
// generate. Shown as missing instead, so the generate fails with the server's
// message naming what is there, rather than quietly running something else.
export function setUpscaler(name) {
  const sel = $('upscaler');
  const v = name ?? '';
  if (v && ![...sel.options].some((o) => o.value === v)) {
    sel.add(new Option(`${v} (missing)`, v));
  }
  sel.value = v;
  syncOutSize();
}

// The boxes "Save as defaults for this model" writes: every control in the
// Advanced panel that is a model's setting, as [wire key, id, kind]. Not the
// sliders — those are per image — and not the escalation models, which are a
// choice per picture rather than something a checkpoint starts at.
//
// Every one is sent, and the server drops whatever equals where the model would
// start anyway (`settings::update_model`), so the file keeps only the boxes that
// were actually changed and a model stays linked to the Settings tab for the
// rest. A blank CFG box is `null`, which removes an override: it means "follow
// the box below", the same thing it means when generating.
const MODEL_KNOBS = [
  ['steps', 'steps'], ['cfg_scale', 'cfg'], ['sampler', 'sampler', 'text'],
  ['scheduler', 'scheduler', 'text'],
  ['cache_mode', 'cachemode', 'text'],
  ['width', 'w'], ['height', 'h'],
  ['upscale_factor', 'upscale'], ['upscaler', 'upscaler', 'choice'],
  ['hires_steps', 'hsteps'], ['hires_strength', 'hstrength'], ['hires_cfg_scale', 'hcfg', 'blank'],
  ['refine_steps', 'rsteps'], ['refine_strength', 'rstrength'], ['refine_cfg_scale', 'rcfg', 'blank'],
  // The pass over a source. Sent with no source picked too: the box still
  // holds the model's number, so it drops out as unchanged.
  ['source_strength', 'srcstrength'],
];

async function saveModelDefaults() {
  const m = currentModel();
  if (!m) return;
  // A pass with another checkpoint picked for it has that checkpoint's numbers
  // in its boxes (`fillPass`), and they are not this model's to keep — saving
  // them would quietly retune it with another model's escalation.
  const foreign = (key) =>
    (key.startsWith('hires_') && passModel('hiresmodel') !== m)
    || (key.startsWith('refine_') && passModel('refmodel') !== m);
  const values = {};
  for (const [key, id, kind] of MODEL_KNOBS) {
    if (foreign(key)) continue;
    const raw = $(id).value.trim();
    if (kind === 'text') values[key] = raw;
    // The empty option is FSR, which is "no upscaler named" — so it clears.
    else if (kind === 'choice') values[key] = raw || null;
    else if (raw === '') {
      // A blank number box says nothing, except where blank has a meaning.
      if (kind === 'blank') values[key] = null;
    } else {
      const n = parseFloat(raw);
      if (Number.isFinite(n)) values[key] = n;
    }
  }
  $('savemodelmsg').textContent = 'Saving…';
  try {
    // The parts too: the server keeps only a choice that differs from what
    // the scan would pick, so sending every slot pins nothing by accident.
    await post('/model-settings', { model: m.name, values, parts: partsPicked() });
    $('savemodelmsg').textContent = `Saved — ${m.name} now opens with these. `
      + 'Settings → Per model lists what differs from its defaults.';
    // The select's "tuned" tag and every pre-fill come from `/models`.
    loadModels();
  } catch (e) {
    $('savemodelmsg').textContent = String(e.message || e);
  }
}

export async function loadModels() {
  try {
    // Started before the first `await`, so `upscalersReady` is this fetch by the
    // time `initSettings` — later in the same boot tick — waits on it.
    const upscalers = loadUpscalers();
    models = await (await fetch('/models')).json();
    await upscalers;
    // Three selects off one list: the one that chooses what generates, and one
    // per escalation — refine and hi-res pick their own, because the two passes
    // run at different sizes on different pixels and there is no reason one
    // checkpoint should be right for both. The escalation two lead with a
    // blank option meaning "whatever made the image", which is what the server
    // reads an empty string as.
    for (const [id, blank] of [['template', null],
                               ['refmodel', '— same as generated —'],
                               ['hiresmodel', '— same as generated —']]) {
      const sel = $(id);
      // Rebuilt after a save as well as at boot, and a rebuilt select falls
      // back to its first option — so saving a model's defaults, or the
      // Settings form, used to switch the Image tab to whichever model sorts
      // first. What was selected is put back if it is still selectable.
      const keep = sel.value;
      sel.innerHTML = '';
      if (blank) {
        const none = document.createElement('option');
        none.value = '';
        none.textContent = blank;
        sel.appendChild(none);
      }
      // Grouped by family once there is more than one, because the family is
      // what decides whether a model's numbers mean anything next to another's:
      // 8 steps is a Z-Image's whole render and a third of an SDXL's.
      const families = [...new Set(models.map((m) => m.family_label))].sort();
      for (const family of families) {
        const group = families.length > 1 ? document.createElement('optgroup') : sel;
        if (group !== sel) {
          group.label = family;
          sel.appendChild(group);
        }
        for (const m of models.filter((x) => x.family_label === family)) {
          const opt = document.createElement('option');
          opt.value = m.name;
          // "tuned": it opens with saved defaults of its own, which is the
          // answer to "why does this one start at 12 steps".
          const tag = (m.discovered ? 'discovered' : 'preset') + (m.tuned ? ', tuned' : '');
          opt.textContent = blank
            // An escalation never uses a model's base size, so quoting it there
            // would be a number that means nothing about what the button will do.
            ? `${m.name} [${tag}]`
            : `${m.name} [${tag}] — ${m.out_width}×${m.out_height}`;
          // Listed rather than hidden, the rule `doctor` follows: "my model
          // isn't there" is worse to debug than "it's there, and here is the
          // file it is waiting for". Disabled only stops *picking* it; a
          // reopened work item can still put it back on screen.
          if (!m.exists) {
            opt.disabled = true;
            opt.textContent += ' (missing files)';
            opt.title = 'Waiting for:\n' + m.missing.join('\n');
          }
          group.appendChild(opt);
        }
      }
      if ([...sel.options].some((o) => o.value === keep && !o.disabled)) sel.value = keep;
    }
    // What a first run looks like: the folders exist, nothing is in them. The
    // server rescans per request, so a reload is all it takes after the copy.
    if (!models.length) {
      $('err').textContent = 'No image models yet. Put a .safetensors or .gguf model into '
        + 'models/checkpoints (in the folder rustimage is in), then reload this page.';
    }
    syncAdvanced();
  } catch (e) {
    $('err').textContent = 'Could not load models: ' + e;
  }
}

export const UPSCALES = [[1, '1× (same size)'], [1.5, '1.5×'], [2, '2×'], [3, '3×'], [4, '4×']];

// One factor, three places to set it: the select above the image, the one in the
// Hi-res pass group — which has to exist too, because the first is rebuilt by
// every render and so isn't there before the first image — and the one over a
// clip's frames. All three are views of `upFactor`; this keeps them saying the
// same thing, which matters because two of them are on tabs you can't see at
// once.
//
// The Hi-res scale box is a number, so the factor can be one the list doesn't
// offer — 2.5, say. The two selects then get an option for it: a select
// silently shows its first option when nothing matches, which would display
// 1× while the pass ran 2.5×.
const factorLabel = (v) => UPSCALES.find(([x]) => x === v)?.[1] ?? `${v}×`;
export const upOptions = () => {
  const list = UPSCALES.some(([x]) => x === S.upFactor)
    ? UPSCALES : [...UPSCALES, [S.upFactor, factorLabel(S.upFactor)]].sort((a, b) => a[0] - b[0]);
  return list.map(([v, label]) =>
    `<option value="${v}"${v === S.upFactor ? ' selected' : ''}>${label}</option>`).join('');
};

export function setUpFactor(v) {
  S.upFactor = v;
  for (const id of ['upfactor', 'hiresscale', 'vfactor']) {
    const el = $(id);
    if (!el || parseFloat(el.value) === v) continue;
    if (el.tagName === 'SELECT' && ![...el.options].some((o) => parseFloat(o.value) === v)) {
      el.innerHTML = upOptions();
    }
    el.value = v;
  }
  syncSummaries();
}

// The box's own rule, the server's (`pipeline::MAX_UPSCALE`): 1 to 4, and a
// tenth is as fine as it is worth being. Applied on `change`, not per
// keystroke, so typing "2.5" isn't clamped at "2." on the way.
function takeHiresScale() {
  const v = parseFloat($('hiresscale').value);
  const f = Number.isFinite(v) ? Math.round(Math.min(4, Math.max(1, v)) * 10) / 10 : S.upFactor;
  $('hiresscale').value = f;
  setUpFactor(f);
}

// Info first, image last: on a narrow screen the metadata and the action
// buttons stay visible without scrolling past a tall image.
export function render(data) {
  // A different picture from the one under the sliders: a re-filter of the
  // old one still in flight must not land on top of it. A run's image can
  // arrive mid-drag now that the sliders stay live during one.
  if (data.image_id !== S.imageId) cancelPost();
  lastImage = data;
  const t = data.timings;
  const stages = [['generate', t.gen_ms], ['upscale', t.upscale_ms ?? 0], ['fsr', t.fsr_ms],
                  ['refine', t.refine_ms], ['hi-res', t.hires_ms], ['area', t.redraw_ms ?? 0]];
  const total = stages.reduce((sum, [, ms]) => sum + ms, 0);
  const max = Math.max(...stages.map(([, ms]) => ms), 1);
  const bars = stages.filter(([, ms]) => ms > 0)
    .map(([name, ms]) => bar(name, ms, max)).join('');
  // The only proof a step cache engaged: sd-cli logs what it skipped, and a
  // mode that did nothing logs nothing, so no line is the honest answer.
  const cached = t.cache_skipped
    ? `<p class="sub">Step cache skipped ${t.cache_skipped} of ${t.cache_steps} steps</p>` : '';

  S.imageId = data.image_id;
  // The seed box is left exactly as typed. Writing the drawn seed back into it
  // turns "-1" into a one-shot: you ask for random, get a number, and every
  // generate after that silently repeats the same image until you notice and
  // retype -1. The seed used is reported below and is one click from the box,
  // which covers the reason it was ever written back.
  // Hi-res first: it's the pass that enlarges, and refine is the finishing
  // touch you run on whatever size you ended up at.
  // Hi-res first: it sets the final size, and refining comes after it — at the
  // size the image ends up at. Refine itself lives in the Refine dialog
  // (redraw.js), with the prompt, the mask, the live preview and the history.
  let actions = '';
  if (data.can_hires) {
    // From `upFactor`, not a hardcoded 2: this select is destroyed and rebuilt
    // on every render, so the choice has to live outside it to survive one.
    actions += `<select id="upfactor" title="Upscale before the hi-res pass">${upOptions()}</select>
      <button id="hires" class="btn btn-run">Hi-res pass</button>`;
  }
  // Always offered, including for a plain generate — a base render can be worth
  // keeping too. Only generations are archived automatically; this is how a
  // refined or hi-res image gets kept.
  actions += '<button id="saveout" class="btn">Save to output</button>';
  // The sliders are behind this button, so it has to be a button. Clicking the
  // image opens the same viewer, but an affordance nobody can see is a feature
  // nobody finds — and the sliders used to be visible furniture.
  actions += '<button id="viewadjust" class="btn">View &amp; adjust</button>';
  // Neutral: opening the dialog spends nothing; its own buttons do.
  actions += '<button id="redrawopen" class="btn">Refine…</button>';

  // The prompt as it was actually conditioned. Shown only when it differs from
  // the box, which happens for exactly three reasons: a `{variable}` was drawn —
  // and the box has to keep the braces for the next image, so this is the only
  // place the roll is visible without hovering the grid — or a work item was
  // reopened, which puts those braces back in the box and so shows the roll here
  // for as long as the image is up, or this is an escalation of an image made
  // from a prompt that has since been retyped.
  const box = $('prompt');
  const rolled = data.prompt && data.prompt !== box.value
    ? `<div class="meta">${esc(data.prompt)}</div>` : '';

  const times = n => n > 1 ? ` ×${n}` : '';
  const tags = (data.refine_passes ? ` · refined${times(data.refine_passes)}` : '')
    + (data.hires_passes ? ` · hi-res${times(data.hires_passes)}` : '');
  S.currentSrc = `data:image/png;base64,${data.image_base64}`;

  P('result').innerHTML = `

    ${actions ? `<div class="actions">${actions}</div>` : ''}
    <div id="saved" class="meta"></div>
    <div id="rerr" class="err"></div>
    <img src="${S.currentSrc}" alt="result" title="Click to open the viewer and post-process">
    <div class="meta"><b>${data.width}×${data.height}</b> · seed
      <b id="seedval" class="pin" title="Put this seed in the box, to generate it again">${data.seed}</b>
      · total <b>${total} ms</b>${tags}</div>
    ${rolled}
    <div class="bars">${bars}</div>${cached}
    `;

  P('result').querySelector('img').addEventListener('click', () => showFullSize(S.currentSrc));
  $('redrawopen').addEventListener('click', () => openRedraw(data));
  if ($('hires')) $('hires').addEventListener('click', hires);
  if ($('upfactor')) {
    $('upfactor').addEventListener('change', (e) => setUpFactor(parseFloat(e.target.value)));
  }
  // The Redraw dialog follows the image on screen while it is open: a redraw,
  // a step back, or a batch image landing.
  syncRedraw(data);
  $('viewadjust').addEventListener('click', () => showFullSize(S.currentSrc));
  // The viewer can already be open when this runs: a modal dialog stops clicks,
  // not a batch that was started before it was opened. So the big copy is kept in
  // step here too, and re-centred, since it is a different image.
  setViewerImage(S.currentSrc);
  // Opt-in reproducibility: pinning the seed is a thing you choose, not
  // something that happens to the box you just typed in.
  $('seedval').addEventListener('click', () => {
    $('seed').value = data.seed;
  });
  if ($('saveout')) $('saveout').addEventListener('click', saveOutput);

  // Which archived generation this descends from, so the grid can show it.
  // Survives a refine or hi-res: those don't create a work item, they escalate
  // the one that's already on screen.
  S.workId = data.work_id ?? null;
  markGrid();

  // What the server actually applied — after a generate the model's two start
  // points and no grade, after an escalation the grade that was carried and the
  // detail filters back at those start points.
  //
  // Reset goes to `filters_start`, which is a different thing and has to be:
  // that is the model's starting point, while `filters` after a Refine includes
  // the grade carried over from the last edit. Taking the applied values as the
  // reset target would make each edit the next image's neutral, with no way
  // back to an ungraded picture.
  setPostStart(data.filters_start ?? data.filters);
  setPost(data.filters);
  $('postms').textContent = '';
  // The action row was just rebuilt; a Refine still waiting on its request
  // keeps saying so.
  redrawBusy();
}

// The Image tab's result as it was last drawn, so coming back from another
// tab puts it back — `showTab` clears the pane it leaves, and before this the
// picture was simply gone. Also what a run's image is compared against, since
// the escalation that produced it may have rendered it already.
let lastImage = null;

export function restoreImage() {
  if (lastImage && S.imageId === null) render(lastImage);
}

// `step` because a redraw moves nothing else: same id, same passes, same size.
const sameImage = (a, b) => !!a && !!b && a.image_id === b.image_id && a.step === b.step
  && a.refine_passes === b.refine_passes && a.hires_passes === b.hires_passes
  && a.width === b.width && a.height === b.height;

// A run's image landed (or a pass's, which was also answered on its own
// request). Drawn only on the Image tab: on Outputs it would replace the image
// in the viewer, which belongs to that tab. It is kept, and comes back with the
// tab.
//
// Also where an escalation's own answer goes, for the same reason: it can wait
// behind a batch for minutes now, and the tab it was pressed on may not be the
// one showing when it lands.
export function takeResult(data) {
  if (sameImage(data, lastImage) && S.imageId === data.image_id) return;
  loadWorkItems();
  if (S.pane === 'image') render(data); else lastImage = data;
}

// The server answered that the image is gone. One with no escalation in it is
// exactly its archived work item, so it can be reopened and the pass retried
// with nobody the wiser; an escalated one existed only in scratch, and the
// message says how to get its base back.
const stale = (e) => e.status === 400 && /no longer available/.test(String(e.message));

async function reopen(workId) {
  for (const shelf of ['workitems', 'favorites']) {
    try { return await post(`/${shelf}/open`, { id: workId }); } catch { /* the other shelf */ }
  }
  return null;
}

async function escalate(which, path, payload, label, kind, err = escErr) {
  if (S.imageId === null) return;
  setBusy(which, label, kind);
  err('');
  soonJobs();
  try {
    try {
      takeResult(await post(path, { image_id: S.imageId, ...payload }));
    } catch (e) {
      const base = lastImage && !lastImage.refine_passes && !lastImage.hires_passes;
      const again = stale(e) && base && S.workId ? await reopen(S.workId) : null;
      if (!again) throw e;
      render(again);
      takeResult(await post(path, { image_id: S.imageId, ...payload }));
    }
  } catch (e) {
    const msg = String(e.message || e);
    // *Cancel generation* on this pass: nothing went wrong, and the image
    // under the buttons is still the one the pass started from.
    err(/cancelled by request/.test(msg) ? 'Cancelled. The image is as it was.' : msg);
    // The image is gone, so the controls are dead; keep the picture on
    // screen. Any other failure is retryable — the server kept its
    // intermediates.
    if (stale(e)) {
      S.imageId = null;
      const row = document.querySelector('.actions');
      if (row) row.remove();
    }
  } finally {
    setIdle(which);
  }
}

// The escalation knobs go with every refine/hi-res, not just with the generate
// that preceded them. The server starts from the profile the image was made with
// and folds these on top, so editing "Refine strength" and pressing Refine runs
// at the new number rather than silently re-using the old one.
//
// Only these six: steps, CFG, sampler and base size describe the txt2img that
// has already run, and there is nothing a new value could do to the image in
// hand. They stay as they are, and a later generate picks them up.
//
// One function, read by the two buttons, the run loop and both generate
// bodies: a run that grew its own list of knobs is how the panel starts lying
// about what the button will do.
//
// The checkpoint is *not* in here, and that is the split: it is per pass, so it
// rides on the one request that runs that pass rather than in the object both
// share. Which is also why nothing strips a stray `model` key out of this any
// more on the way to /generate.
export const escalateSettings = () => {
  return {
    refine_steps: numOr('rsteps'),
    refine_strength: numOr('rstrength'),
    hires_steps: numOr('hsteps'),
    hires_strength: numOr('hstrength'),
    // Here null is a value, not an omission: clearing the box unsets the override
    // so the pass follows the knob below it again. Sending nothing would pin
    // whatever was last set, which is the one thing these must never do.
    refine_cfg_scale: numOr('rcfg'),
    hires_cfg_scale: numOr('hcfg'),
    // Each pass's own prompt: blank is the image's own, on the server. Read by
    // a run's after-each passes, and by the dialog when its own box is blank.
    refine_prompt: $('rprompt').value.trim() || null,
    hires_prompt: $('hprompt').value.trim() || null,
  };
};

// The two escalation checkpoints, as a *generate* body spells them. They name no
// pass this request runs — they are stored on the image so reopening it restores
// the pickers, which is the one thing a work item used to lose. The escalation
// routes take theirs as `model` instead, because there it is the pass about to
// run and there is exactly one of them.
export const escalateModels = () => ({
  refine_model: P('refmodel').value || null,
  hires_model: P('hiresmodel').value || null,
});

// Each pass sends its own checkpoint: empty means "the one that made the
// image", which is what an unchosen select sends.
//
// Refine is the Refine dialog's whole-image pass. The dialog's prompt wins over
// the fold's when typed (the fold's is already in `escalateSettings()`), and
// `err` is where a failure goes: the dialog is modal, so `#rerr` behind it is a
// line nobody reads.
export const refine = (prompt, err) => escalate('redrawgo', '/refine', {
  model: P('refmodel').value || null, ...escalateSettings(),
  ...(prompt ? { refine_prompt: prompt } : {}),
}, 'Refining…', 'refine', err);
// Hi-res is the action row's, with the Hi-res fold's prompt.
const hires = () => escalate('hires', '/hires', {
  model: P('hiresmodel').value || null, upscale: parseFloat($('upfactor').value),
  ...escalateSettings(),
}, 'Hi-res pass…', 'hires');

// ---- Runs -------------------------------------------------------------------
//
// A run is one or more generations, plus whatever the "After each image" boxes
// ask for. Pressing Generate queues it on the server (`crate::jobs`) and that is
// all this page does: the server makes the images one at a time, publishes each
// stage as it lands, and the Jobs poll hands them to `takeResult`. So the run
// survives a reload or a trip to another tab, and Generate stays pressable —
// pressing it again queues the next run behind this one.
//
// Everything the run needs is captured now, in the body. That is what a queue
// means: job A keeps the boxes it was queued with while job B is being set up.

const runTotal = () =>
  $('autorun').checked ? Infinity : Math.max(1, parseInt($('count').value, 10) || 1);

// The button does three different things, so it says three different things.
function syncGoLabel() {
  const n = runTotal();
  $('go').textContent =
    n === Infinity ? 'Start loop generation' : n > 1 ? `Generate ${n}` : 'Generate';
}

// One image's request, as `[route, body]`: from noise it is `/generate`, from a
// source `/img2img`, and the two share everything that isn't about where the
// picture starts. The seed in the box goes as typed; the server walks it by
// one per image of a pinned-seed run, so image 0 is still precisely that seed.
// A batch over one source is therefore N seeds of one transform, which is a
// real use — pick the best of eight.
function genPayload() {
  const seed = parseInt($('seed').value, 10);
  const common = {
    prompt: $('prompt').value,
    template: $('template').value,
    negative: $('negative').value || null,
    seed: Number.isFinite(seed) && seed >= 0 ? seed : -1,
    // Both routes: a pass over a source runs these too.
    steps: numOr('steps'),
    cfg_scale: numOr('cfg'),
    sampler: $('sampler').value || null,
    scheduler: $('scheduler').value || null,
    // Only the generate pass (or the pass over a source) uses it: Refine and
    // Hi-res run uncached on the server whatever this says.
    cache_mode: $('cachemode').value || null,
    // The same six the escalations send, from the same place — they set where
    // this image's passes start, and `/refine` and `/hires` can move them
    // afterwards. The two CFGs are among them, and null for an empty box still
    // means "leave it unset" here: the passes keep following the knob below
    // rather than getting pinned to whatever it resolves to today.
    ...escalateSettings(),
    // Retained on the image rather than acted on — this request runs neither
    // pass. It is what makes a reopened work item come back with its finishing
    // checkpoints in the pickers. Spread separately from the knobs above because
    // the two escalation routes spell the same choice as `model`, singular.
    ...escalateModels(),
    // Always sent, including empty — the server reads `[]` as "none" and a
    // missing key as "keep whatever the profile has", and the picker on screen
    // is the complete answer to the question, so it must be able to say none.
    // Retained on the image, so a later Refine or Hi-res runs with the same
    // adapters without a second picker to set them in.
    loras: imageLoras.get(),
    // The Models fold. Retained like `loras`, so Refine and Hi-res decode with
    // the VAE this image was made with.
    parts: partsPicked(),
    // Not sharpness/clarity: those are post-process, not generation settings,
    // so a new image starts from the model's values rather than inheriting
    // wherever the last one's sliders were left.
  };
  const source = imageSource.get();
  if (source) {
    return ['/img2img', {
      ...common,
      source,
      max_side: numOr('srcmaxside'),
      source_strength: numOr('srcstrength'),
    }];
  }
  return ['/generate', {
    ...common,
    width: numOr('w'),
    height: numOr('h'),
    // The FSR enlarge on the way to the output size. An empty box sends nothing
    // and the model's own factor runs; 1 is a real answer and means no upscale.
    upscale_factor: numOr('upscale'),
    // Always sent: `""` is FSR, explicitly, which has to be sayable over a
    // saved default that names a model.
    upscaler: $('upscaler').value,
    // To a model that takes them, always, `[]` included — the picker is the
    // whole answer, as for `loras`. To one that doesn't, not at all: the picks
    // wait in the dialog for a model that does. A pass over a source doesn't
    // take them (its start image is already the picture it works from).
    ...(refsActive() ? { references: references() } : {}),
  }];
}

// The *After each generation* fold, as the run will carry it. The checkpoint
// selects are read from the Image tab by id: Generate only exists there.
function afterEach() {
  return {
    hires: $('opthires').checked
      ? { upscale: S.upFactor, model: $('hiresmodel').value || null } : null,
    refine: $('optrefine').checked ? { model: $('refmodel').value || null } : null,
    save: $('optsave').checked,
  };
}

async function startRun() {
  $('err').textContent = '';
  const n = runTotal();
  const [route, body] = genPayload();
  $('go').disabled = true;
  try {
    await post(route, {
      ...body,
      count: n === Infinity ? null : n,
      loop: n === Infinity,
      after: afterEach(),
    });
    soonJobs();
  } catch (e) {
    $('err').textContent = String(e.message || e);
  } finally {
    $('go').disabled = false;
  }
}

// The running run, if it is an image run of more than one: what *Stop job*
// under Generate addresses. The Jobs dialog stops anything; this is the shortcut
// for the common case, and it asks nothing, as it never did.
const isRunKind = (j) => j.kind === 'generate' || j.kind === 'img2img';
const stoppableRun = () => {
  const j = runningJob();
  return j && isRunKind(j) && j.total !== 1 ? j : null;
};

// What *Cancel generation* addresses: whatever is drawing the live preview —
// an image of a run, or a Refine, Hi-res or Redraw press.
const cancellable = () => {
  const j = runningJob();
  return j && (isRunKind(j) || ['refine', 'hires', 'redraw', 'upscale', 'edit'].includes(j.kind)) ? j : null;
};

// Stop after the image in flight has finished its whole pipeline. The hi-res,
// the refine and the save are that image, and dropping a refined one a single
// call short of saving it is not what anybody means by "stop".
function stopRun() {
  const j = stoppableRun();
  if (j) stopJob(j.id);
}

// Throw the image in flight away — for a preview whose composition is already
// wrong. A run goes on to its next image (`/jobs/skip`); a Refine or Hi-res is
// one pass, so cancelling it is stopping it.
async function cancelImage() {
  const j = cancellable();
  if (!j) return;
  if (!isRunKind(j)) return stopJob(j.id);
  $('cancelgen').disabled = true;
  try {
    await post('/jobs/skip', { id: j.id });
  } catch (e) {
    // Most often the gap between two passes; the poll re-enables the button.
    escErr(String(e.message || e));
  }
  soonJobs();
}

// Both driven by the Jobs poll, so they follow a run this page didn't start,
// and one that outlived a reload. *Stop job* only while there is something to
// stop *between* — a single image is over when it's over, and *Cancel
// generation* already ends it.
function syncStopButton() {
  const c = cancellable();
  $('cancelgen').hidden = !c;
  if (c) {
    $('cancelgen').disabled = !!c.skipping;
    $('cancelgen').textContent = c.skipping ? 'Cancelling…' : 'Cancel generation';
  }
  const j = stoppableRun();
  $('stop').hidden = !j;
  if (!j) return;
  const stopping = j.state === 'stopping';
  $('stop').disabled = stopping;
  $('stop').textContent = stopping ? 'Stopping after this image…' : 'Stop job';
}

// Keep the image on screen. Everything generated is archived automatically, but
// a refine or hi-res is not — escalations are the expensive rungs, and most of
// them are experiments. This is how one stops being an experiment.
async function saveOutput() {
  if (S.imageId === null) return;
  setBusy('saveout', 'Saving…');
  escErr('');
  try {
    const data = await post('/save-output', { image_id: S.imageId });
    $('saved').textContent = `saved → ${data.path}`;
  } catch (e) {
    escErr(String(e.message || e));
  } finally {
    setIdle('saveout');
  }
}

export function initImage() {
  imageLoras.init();
  onRefsChange(syncSummaries);
  initFolds();
  $('go').addEventListener('click', startRun);
  $('stop').addEventListener('click', stopRun);
  $('cancelgen').addEventListener('click', cancelImage);
  // The Hi-res fold's view of the shared upscale factor.
  $('hiresscale').value = S.upFactor;
  $('hiresscale').addEventListener('change', takeHiresScale);
  // Portrait for landscape. Through `input` events rather than straight to the
  // notes, so everything that listens to the two boxes hears it.
  $('swapwh').addEventListener('click', () => {
    [$('w').value, $('h').value] = [$('h').value, $('w').value];
    for (const id of ['w', 'h']) $(id).dispatchEvent(new Event('input', { bubbles: true }));
  });
  // Back to a random seed. Through an `input` event, the way Swap does it, so
  // anything listening to the box hears it.
  $('seedrandom').addEventListener('click', () => {
    $('seed').value = -1;
    $('seed').dispatchEvent(new Event('input', { bubbles: true }));
  });
  // Every fold's summary follows its boxes, from one listener on the column.
  for (const ev of ['input', 'change']) {
    $('tab-image').querySelector('.col-controls').addEventListener(ev, syncSummaries);
  }
  // Each pass's boxes follow the checkpoint picked for it.
  for (const [sel, which] of [['hiresmodel', 'hires'], ['refmodel', 'refine']]) {
    $(sel).addEventListener('change', () => { fillPass(which); syncCfgPlaceholders(); });
  }
  // 'input' covers the checkboxes too, and keeps the count responsive per keystroke
  // rather than on blur — the button's own label is the feedback.
  for (const id of ['count', 'autorun']) {
    $(id).addEventListener('input', syncGoLabel);
  }
  // Each model carries its own defaults, so re-fill the controls on switch.
  $('template').addEventListener('change', syncAdvanced);
  $('savemodel').addEventListener('click', saveModelDefaults);
  // A note about the model that was selected is stale once another one is.
  $('template').addEventListener('change', () => { $('savemodelmsg').textContent = ''; });
  // What an empty per-pass CFG box resolves to is whatever the chain under it
  // says, so the placeholders have to follow that field as it is typed in.
  $('cfg').addEventListener('input', syncCfgPlaceholders);
  // The line under the source's boxes says what they add up to, and two of the
  // numbers in it are the Generation fold's.
  for (const id of ['steps', 'srcstrength', 'srcmaxside', 'cfg']) {
    $(id).addEventListener('input', syncSrcNote);
  }
  imageSource.init();
  // Three boxes decide the output size and the note under them states it.
  for (const id of ['upscale', 'w', 'h']) {
    $(id).addEventListener('input', syncOutSize);
  }
  // And one select says what does the enlarging, which the note names.
  $('upscaler').addEventListener('change', syncOutSize);

  // `IDLE_LABELS.go` is only a fallback: what the button says at rest depends on
  // the run panel, which that map can't know.
  onIdle(syncGoLabel);
  onResult(takeResult);
  onJobs(syncStopButton);
  // A run that failed has no request waiting on it; its message comes here.
  onNotice((msg) => {
    $('err').textContent = /cancelled by request/.test(msg)
      ? 'Stopped. The image in flight was not kept.' : msg;
  });

  loadModels();
  // A label for controls that start with a value, so it can't wait for the
  // first edit to say what it does.
  syncGoLabel();
}
