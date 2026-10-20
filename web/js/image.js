import { $, bar, esc, getJSON, post, numOr, rememberFolds } from './dom.js';
import { S, P, escErr } from './state.js';
import { isBusy, setBusy, setIdle, onIdle, redrawBusy } from './busy.js';
import { cancelPost, onPassResult, onViewerResult, setLook, setPost, setViewerImage, showFullSize } from './viewer.js';
import { activeLoop, onJobs, onNotice, onResult, runningJob, soonJobs, stopJob } from './jobs.js';
import { applyWorkItem, loadWorkItems, markGrid } from './workitems.js';
import { loraPicker } from './loras.js';
import { sourceSlot } from './source-slot.js';
import { wireHints } from './hints.js';
import { onRefsChange, references, refsActive, syncRefs } from './refs.js';
import { openGetModels } from './getmodels.js';
import { floatPanels } from './panels.js';
import { partRows } from './parts.js';
import { showError, MEMORY_LABELS } from './memory.js';

// The checkpoint list the server published. Exported because the work-item
// grid looks a name up in it; only this module refills it.
export let models = [];

// Whether Extend can run: a fill-only model with every file present. The Refine
// dialog reads it to enable its Extend group or point at Get models instead.
export const fillReady = () => models.some((m) => m.fill && m.exists);

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

// What every out-of-memory box on this tab can offer besides a retry: the
// Memory select, and the Get models card for a smaller file of this family.
export const oomOptions = () => ({
  memory: 'memory',
  smallerFile: currentModel() ? () => openGetModels(currentModel().family) : null,
  // Opens the References dialog, where a pick is removed.
  references: () => $('refsopen').click(),
});

// One step down on the base size, a quarter fewer pixels per side, snapped to
// 64 like the boxes. Only for a render from noise: a source brings its size.
function smallerSize() {
  for (const id of ['w', 'h']) {
    const v = parseInt($(id).value, 10);
    if (Number.isFinite(v)) $(id).value = Math.max(256, Math.round((v * 0.75) / 64) * 64);
    $(id).dispatchEvent(new Event('input', { bubbles: true }));
  }
}

// ---- The Models fold -------------------------------------------------------
//
// One select per separate file the selected model's family runs with. The
// slots and the files that fit each come from `/models` (`parts`), already
// filtered — which VAE decodes which family is the server's to know, the
// `frame_grid` rule. A slot is `optional` on a bundled checkpoint, where the
// empty choice is the one built into it; otherwise it must name a file.

const baseName = (name) => name.split('/').pop();
const imageParts = partRows({
  el: 'partrows', prefix: 'part', none: 'Built into the checkpoint', onChange: () => syncModelsSummary(),
});
const partSelects = imageParts.selects;

// Redrawn from the selected model. Exported for `applyWorkItem`, which sets the
// model without firing `change` and then puts back the parts the image ran with.
export function syncParts() {
  imageParts.draw(currentModel()?.parts ?? []);
  wireHints($('partrows'));
  syncModelsSummary();
}

// Put back the files a work item ran with, from its profile's absolute paths —
// what a sidecar records.
export function setParts(components) {
  imageParts.set(components);
  syncModelsSummary();
}

const partsPicked = imageParts.picked;

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
  // Only when it isn't Auto: the line says what departs from the plain path.
  if ($('memory').value && $('memory').value !== 'auto') {
    bits.push(`Memory: ${MEMORY_LABELS[$('memory').value] ?? $('memory').value}`);
  }
  const loras = imageLoras.get();
  if (loras.length) {
    bits.push(`LoRA: ${loras.map((l) => baseName(l.name).replace(/\.[^.]+$/, '')).join(', ')}`);
  }
  $('modelsum').textContent = '— ' + [m.name, ...bits].join(' · ');
  // The line is cut to fit, so the whole of it is the title.
  $('modelsum').title = [m.name, ...bits].join('\n');
  // LoRAs patch what is drawn and a memory mode changes how it fits: a
  // departure from the plain path, which a closed panel must not hide.
  $('modelsum').classList.toggle('on', loras.length > 0 || ($('memory').value || 'auto') !== 'auto');
  syncRailDots();
}

// The upscaler's name as the select shows it; FSR when none is picked.
function upscalerName() {
  const sel = $('upscaler');
  return sel.value ? sel.options[sel.selectedIndex]?.textContent ?? sel.value : 'FSR';
}

// The rail's seed line: what the next Generate uses, read-only. Amber when
// pinned (not -1), since every image then repeats it. Called from
// `syncSummaries`, and after the writes that fire no event (a reopened work
// item, Info's seed pin).
export function syncSeedLine() {
  const v = $('seed').value.trim() || '-1';
  const el = $('seedline');
  el.textContent = `Seed: ${v}`;
  el.title = el.textContent;
  el.classList.toggle('on', v !== '-1');
}

// ---- The workspace ------------------------------------------------------------
//
// The tab is a rail, the picture on a stage, and floating panels over it
// (`panels.js`), the Refine & adjust dialog's shape. The panels kept the
// folds' ids, so code that opens one (`$('fold-src').open = true`) still does.
let wsPanels = null;
const PANEL_DEFS = [
  { id: 'img-prompt', el: 'fold-prompt', open: true },
  { id: 'img-models', el: 'modelsbox' },
  { id: 'img-gen', el: 'fold-gen', open: true },
  { id: 'img-src', el: 'fold-src' },
  { id: 'img-hires', el: 'fold-hires' },
  { id: 'img-refine', el: 'fold-refine' },
  { id: 'img-info' },
  { id: 'img-current', open: true, corner: 'bottom-right' },
  { id: 'img-favorites' },
];

// A rail button's dot: a line in its panel is in the accent, so the panel
// departs from the plain path (a batch, an After-each pass, a source, a LoRA, a
// pass's own model) — the header's, or a fold's inside it.
function syncRailDots() {
  if (!wsPanels) return;
  for (const d of PANEL_DEFS) {
    const el = $(d.el ?? `panel-${d.id}`);
    wsPanels.dot(d.id, !!el.querySelector('.foldstate.on'));
  }
}

// The tab was shown: its panels can be placed now that it has a size.
export const layoutImageTab = () => wsPanels?.layout();

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
// A model of the listing by name, for the dialog's pass gating (redraw.js).
export const modelNamed = (name) => models.find((m) => m.name === name);

// The steps a press of that pass will run with: the fold's box, which
// `escalateSettings()` sends and the server folds over the image's profile
// before it checks. An empty box sends nothing, so the profile's own count
// stands — the picked model's here, the nearest thing the page holds.
export function passSteps(which) {
  const n = parseInt($(which === 'hires' ? 'hsteps' : 'rsteps').value, 10);
  if (Number.isFinite(n)) return n;
  return passModel(which === 'hires' ? 'hiresmodel' : 'refmodel')?.[`${which}_steps`] ?? 0;
}

function syncSummaries() {
  // Everything that changes a summary is something a running loop should
  // hear, so this is also where the loop is told.
  syncLoop();
  syncSeedLine();
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
    source ? null : `${$('upscale').value}× via ${upscalerName()}`,
  ].filter(Boolean).join(' · '));
  // A step cache trades exactness for time, and references change what is
  // drawn: departures from the plain path a folded section must not hide.
  $('sum-gen').classList.toggle('on', n > 1 || $('cachemode').value !== 'off' || refs > 0);

  say('sum-src', source ? `from ${source} · strength ${$('srcstrength').value}` : 'off');
  // In the accent rather than dim, like every summary below that marks a
  // departure from the plain path: a picked source changes what Generate
  // *does*, and read as just more grey small print.
  $('sum-src').classList.toggle('on', !!source);

  // Whole: the summary line cuts it to the width there is, and the title holds
  // all of it.
  say('sum-prompt', $('prompt').value.trim());

  syncModelsSummary();

  for (const [id, steps, sel, opt] of [['sum-hires', 'hsteps', 'hiresmodel', 'opthires'],
                                       ['sum-refine', 'rsteps', 'refmodel', 'optrefine']]) {
    const n = parseInt($(steps).value, 10) || 0;
    const each = $(opt).checked;
    say(id, n > 0
      ? [each ? 'after each' : null, `${n} steps`, id === 'sum-hires' ? `${S.upFactor}×` : null,
        $(sel).value || 'same model'].filter(Boolean).join(' · ')
      : 'off');
    // In the accent when it runs on every image of a run (GPU time on each),
    // or on another checkpoint, which is easy to forget once closed. Compared
    // with the diffusion model rather than tested for being set: a pick that
    // names the generating model is the same model, and stays dim.
    $(id).classList.toggle('on', each || (!!$(sel).value && $(sel).value !== $('template').value));
  }
  syncRailDots();
}

// The panels, before anything reads `.open` on one. Open or closed and where
// each was left is a per-viewer convenience, so it lives in this browser
// (`panels.js`); the first time, Prompts and Generation are open, and Current
// sits in the bottom-right corner. The folds still nested in a panel (the
// preview) remember their own state.
function initFolds() {
  wsPanels = floatPanels({
    host: $('tab-image'),
    rail: $('tab-image').querySelector('.wsrail'),
    defs: PANEL_DEFS,
  });
  wsPanels.init();
  // The tab the page opens on, so it has its size already.
  wsPanels.layout();
  rememberFolds($('tab-image'));
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
  $('memory').value = m.memory ?? 'auto';
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
  ['memory', 'memory', 'text'],
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

// `refill: false` keeps the Generation boxes as they are unless the selected
// model itself changed: a download landing in the background must not reset
// a Steps value somebody is in the middle of tuning. A Settings save refills,
// since new defaults are the point of it.
export async function loadModels({ refill = true } = {}) {
  const before = $('template').value;
  try {
    // Started before the first `await`, so `upscalersReady` is this fetch by the
    // time `initSettings` — later in the same boot tick — waits on it.
    const upscalers = loadUpscalers();
    models = await getJSON('/models');
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
      // A fill-only model (FLUX.1 Fill) is never something to generate, refine or
      // hi-res with; it is what Extend draws with, found by the server itself.
      const pickable = models.filter((m) => !m.fill);
      const families = [...new Set(pickable.map((m) => m.family_label))].sort();
      for (const family of families) {
        const group = families.length > 1 ? document.createElement('optgroup') : sel;
        if (group !== sel) {
          group.label = family;
          sel.appendChild(group);
        }
        for (const m of pickable.filter((x) => x.family_label === family)) {
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
            opt.title = 'Waiting for:\n' + m.missing.join('\n')
              + '\n\nThe Get models tab can download them.';
          }
          group.appendChild(opt);
        }
      }
      if ([...sel.options].some((o) => o.value === keep && !o.disabled)) sel.value = keep;
    }
    // What a first run looks like: the folders exist, nothing is in them. The
    // server rescans per request, so a reload is all it takes after the copy.
    if (!models.some((m) => !m.fill)) {
      $('err').textContent = 'No image models yet. The Get models tab downloads one, or put a '
        + '.safetensors or .gguf model into models/checkpoints (in the folder yasdig is in) '
        + 'and reload this page.';
    } else if ($('err').textContent.startsWith('No image models yet')) {
      $('err').textContent = '';
    }
    // A greyed-out model is a family whose parts the Get models tab can fetch.
    // The link goes to the first such family; the tab lists the rest.
    const waiting = models.filter((m) => !m.exists && !m.fill);
    $('getparts').hidden = !waiting.length;
    if (waiting.length) {
      $('getparts').innerHTML = `${waiting.length === 1 ? 'One model is' : `${waiting.length} models are`} `
        + `waiting for files (${waiting.map((m) => esc(m.name)).join(', ')}). `
        + '<button type="button" class="btn">Get the missing files…</button>';
      $('getparts').querySelector('button').onclick = () => openGetModels(waiting[0].family);
    }
    if (refill || $('template').value !== before) syncAdvanced();
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
  for (const id of ['hiresscale', 'dl-hiresscale', 'vfactor']) {
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
// Where the time went inside sd.cpp, stage by stage — the part that dominates is
// the part worth quantising (the text encoder, the model) or shrinking (the
// references). Each stage includes loading its own weights; the last figure is
// how much of all of them that was.
function stageLine(t) {
  const s = t.stages ?? {};
  const sec = (ms) => `${(ms / 1000).toFixed(1)} s`;
  // Each stage named with the file that does it, since that file is the lever:
  // what to quantise, or swap, when its stage dominates.
  const parts = [
    ['Prompt (text encoder)', s.text_ms], ['Images in (VAE)', s.encode_ms],
    ['Drawing (model)', s.draw_ms], ['Decode (VAE)', s.decode_ms],
  ].filter(([, ms]) => ms > 0).map(([name, ms]) => `${name} ${sec(ms)}`);
  if (!parts.length && !t.load_ms) return '';
  const load = t.load_ms ? ` — of which loading weights ${sec(t.load_ms)}` : '';
  return `<p class="sub">${parts.join(' · ')}${load}</p>`;
}

export function render(data) {
  // A different picture from the one under the sliders: a re-filter of the
  // old one still in flight must not land on top of it. A run's image can
  // arrive mid-drag now that the sliders stay live during one.
  if (data.image_id !== S.imageId) cancelPost();
  // The same picture at the same size (a refine, an adjustment baked, a step
  // back) keeps the dialog's view; anything else is re-centred.
  const same = data.image_id === S.imageId && lastImage?.width === data.width && lastImage?.height === data.height;
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
  const cached = (t.cache_skipped
    ? `<p class="sub">Step cache skipped ${t.cache_skipped} of ${t.cache_steps} steps</p>` : '')
    // Where the weights were when any were off the card — what explains a
    // slow pass before it becomes one that ran out of memory.
    + (t.weights ? `<p class="sub">Weights: ${esc(t.weights)}</p>` : '')
    + stageLine(t);

  S.imageId = data.image_id;
  // The seed box is left exactly as typed. Writing the drawn seed back into it
  // turns "-1" into a one-shot: you ask for random, get a number, and every
  // generate after that silently repeats the same image until you notice and
  // retype -1. The seed used is reported below and is one click from the box,
  // which covers the reason it was ever written back.
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

  // The picture on the stage; what is known about it in the Info panel. Every
  // pass on it — Hi-res, Refine, Edit, Extend, Scale — and Save to output are
  // in the Refine & adjust dialog, which a click on the picture opens.
  P('result').innerHTML = `<img src="${S.currentSrc}" alt="result" title="Click to refine and adjust">`;
  P('info').innerHTML = `
    <div class="meta"><b>${data.width}×${data.height}</b> · seed
      <b id="seedval" class="pin" title="Put this seed in the box, to generate it again">${data.seed}</b>
      · total <b>${total} ms</b>${tags}</div>
    ${rolled}
    <div class="bars">${bars}</div>${cached}
    `;
  P('result').querySelector('img').addEventListener('click', () => showFullSize(S.currentSrc));
  // The dialog can already be open when this runs: a pass from it, a step
  // back, or a batch image landing. A new state of the same picture keeps the
  // view where it was; a different picture is re-centred.
  setViewerImage(S.currentSrc, same);
  // Opt-in reproducibility: pinning the seed is a thing you choose, not
  // something that happens to the box you just typed in.
  $('seedval').addEventListener('click', () => {
    $('seed').value = data.seed;
    syncSeedLine();
  });

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
  // `setLook` also restores the area and what the Crop fold says.
  setLook(data);
  $('postms').textContent = '';
  // A pass still waiting on its request keeps saying so.
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

// `land` is where the answer goes: `takeResult` for the Image tab's own
// picture, the dialog's `passLander()` for one opened from Outputs.
async function escalate(which, path, payload, label, kind, err = escErr, land = takeResult) {
  if (S.imageId === null) return;
  const onImage = S.pane === 'image';
  setBusy(which, label, kind);
  err('');
  soonJobs();
  try {
    try {
      land(await post(path, { image_id: S.imageId, ...payload }));
    } catch (e) {
      // Only the Image tab's picture has a work item to reopen.
      const base = onImage && lastImage && !lastImage.refine_passes && !lastImage.hires_passes;
      const again = stale(e) && base && S.workId ? await reopen(S.workId) : null;
      if (!again) throw e;
      render(again);
      land(await post(path, { image_id: S.imageId, ...payload }));
    }
  } catch (e) {
    const msg = String(e.message || e);
    // *Cancel generation* on this pass: nothing went wrong, and the image
    // under the buttons is still the one the pass started from.
    // Out of memory: the retry is this same pass, with the Memory select as
    // it stands by then — which is what the box's buttons change.
    err(/cancelled by request/.test(msg) ? 'Cancelled. The image is as it was.' : msg, e.oom, {
      ...oomOptions(),
      retry: () => escalate(which, path, { ...payload, memory: $('memory').value || null }, label, kind, err, land),
    });
    // The image is gone, so the dialog's buttons go dead (no `imageId`);
    // the picture stays on screen. Any other failure is retryable — the
    // server kept its intermediates.
    if (stale(e) && onImage) S.imageId = null;
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
    // Every pass: a Refine that ran out of memory is retried with this changed.
    memory: $('memory').value || null,
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
// Refine is the dialog's whole-image pass. The dialog's prompt wins over the
// fold's when typed (the fold's is already in `escalateSettings()`), and `err`
// is where a failure goes: the dialog is modal, so `#rerr` behind it is a line
// nobody reads. On Outputs there is no Refine pass fold's model: the dialog's
// *Refine with* has already set the image's.
// `extra` is the dialog's own: the references a ticked Refine follows.
export const refine = (prompt, err, land, extra = {}) => escalate('redrawgo', '/refine', {
  // The Refine pass panel's model, which the dialog's Refine panel shows too,
  // so it is the one picked on Outputs as well.
  model: $('refmodel').value || null, ...escalateSettings(),
  ...(prompt ? { refine_prompt: prompt } : {}), ...extra,
}, 'Refining…', 'refine', err, land);
// Hi-res, from the dialog's Refine panel: enlarge by the shared factor and
// re-diffuse at the new size, with the Hi-res pass panel's settings and model.
export const hiresPass = (err, land) => escalate('rdhires', '/hires', {
  model: $('hiresmodel').value || null, upscale: S.upFactor,
  ...escalateSettings(),
}, 'Hi-res pass…', 'hires', err, land);

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

// A run's whole request: the panel as it is now, how many, and what after.
// One builder for Generate and for the loop's live edits, so the two can't
// disagree about what a run is.
function runBody(n) {
  const [route, body] = genPayload();
  return [route, {
    ...body,
    count: n === Infinity ? null : n,
    loop: n === Infinity,
    after: afterEach(),
  }];
}

async function startRun() {
  $('err').textContent = '';
  const n = runTotal();
  const [route, body] = runBody(n);
  $('go').disabled = true;
  try {
    const r = await post(route, body);
    // Start with a loop already running updates that loop (one at a time,
    // `Queue::push_loop`) rather than queuing a second.
    if (n === Infinity) {
      lastLoop = JSON.stringify([route, body]);
      if (r.updated) loopUpdated();
    }
    soonJobs();
  } catch (e) {
    $('err').textContent = String(e.message || e);
  } finally {
    $('go').disabled = false;
  }
}

// ---- The loop follows the panel ---------------------------------------------
//
// "Until stopped" draws each image from the Image tab as it is
// *now*: change the prompt to "lovely cat" and the next image is a cat. A batch
// keeps what it was queued with; the loop is the one run that listens, and
// there is only ever one (`Queue::push_loop`). The image in flight finishes as
// it started.
//
// Debounced, so a word typed is one request, not one per key; deduplicated, so
// opening a fold sends nothing; and skipped on a blank prompt, because clearing
// the box on the way to a new one would fail the next image, and a failed
// image ends the run. `update_only`, because an edit landing after the loop
// ended must not start another.
const LOOP_DEBOUNCE_MS = 800;
let loopTimer = null;
// The body the loop last got, as JSON.
let lastLoop = null;
// The loop image count from which the note is stale: the in-flight image is
// the old settings', the one after it is the first with the new ones.
let loopNoteUntil = null;

function syncLoop() {
  clearTimeout(loopTimer);
  loopTimer = setTimeout(updateLoop, LOOP_DEBOUNCE_MS);
}

async function updateLoop() {
  if (!activeLoop() || !currentModel() || !$('prompt').value.trim()) return;
  const [route, body] = runBody(Infinity);
  const key = JSON.stringify([route, body]);
  if (key === lastLoop) return;
  lastLoop = key;
  try {
    const r = await post(route, { ...body, update_only: true });
    if (r.updated) loopUpdated();
    soonJobs();
  } catch (e) {
    // Not sent, so the same edit may be tried again.
    lastLoop = null;
    $('err').textContent = String(e.message || e);
  }
}

function loopUpdated() {
  const j = activeLoop();
  loopNoteUntil = j ? j.done + 2 : null;
  $('loopnote').textContent = 'Loop updated — the next image uses it.';
}

// After every poll: the note goes once the loop has drawn an image with the
// new settings, or has ended.
function syncLoopNote() {
  if (!$('loopnote').textContent) return;
  const j = activeLoop();
  if (!j || (loopNoteUntil !== null && j.done >= loopNoteUntil)) {
    $('loopnote').textContent = '';
    loopNoteUntil = null;
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
// The Refine & adjust dialog's *Save to output* (its rail, on the Image tab):
// what is on screen, look and all.
export async function saveOutput() {
  if (S.imageId === null) return;
  setBusy('saveout', 'Saving…');
  $('lberr').textContent = '';
  try {
    const data = await post('/save-output', { image_id: S.imageId });
    $('lbsaved').textContent = `saved → ${data.path}`;
  } catch (e) {
    $('lberr').textContent = String(e.message || e);
  } finally {
    setIdle('saveout');
  }
}

// ---- Settings from a PNG --------------------------------------------------
//
// The server reads the file (`POST /png-settings`, `src/pngmeta.rs`) and
// matches it against what is installed; this only applies the answer. The
// original file's bytes are sent — `/upload`'s canvas re-encode would strip
// the text chunks this is for.

// An A1111-style answer names only some settings: each box it doesn't name
// keeps what it has. The model goes first, through `change`, because
// `syncAdvanced` refills every box from the model's defaults — the PNG's
// numbers land on top of that.
function applyParams(p) {
  if (p.template && p.template !== $('template').value) {
    $('template').value = p.template;
    $('template').dispatchEvent(new Event('change', { bubbles: true }));
  }
  $('prompt').value = p.prompt;
  if (p.negative !== undefined) $('negative').value = p.negative;
  const set = (id, v) => { if (v !== undefined && v !== null) $(id).value = v; };
  set('seed', p.seed);
  set('steps', p.steps);
  set('cfg', p.cfg_scale);
  set('sampler', p.sampler);
  set('scheduler', p.scheduler);
  set('w', p.width);
  set('h', p.height);
  imageLoras.set(p.loras ?? []);
  syncCfgPlaceholders();
  syncOutSize();
}

async function loadPngSettings(file) {
  const note = $('pngnote');
  note.hidden = false;
  note.textContent = `Reading ${file.name}…`;
  try {
    const resp = await fetch('/png-settings', { method: 'POST', body: await file.arrayBuffer() });
    const data = await resp.json();
    if (!resp.ok || data.error) throw new Error(data.error || `HTTP ${resp.status}`);
    if (data.item) applyWorkItem(data.item);
    else if (data.params) applyParams(data.params);
    const from = { yasdig: 'yasdig', a1111: 'A1111-style' }[data.source];
    const head = from ? `Settings from ${file.name} (${from})` : `Nothing loaded from ${file.name}`;
    note.textContent = [head, ...data.notes].join(' — ');
    // The summaries follow the boxes through the column's `input` listener,
    // which assigning `.value` never fires.
    $('prompt').dispatchEvent(new Event('input', { bubbles: true }));
  } catch (e) {
    note.textContent = `Couldn't read ${file.name}: ${e.message || e}`;
  }
}

function initPngSettings() {
  $('pngload').addEventListener('click', () => $('pngfile').click());
  $('pngfile').addEventListener('change', () => {
    const f = $('pngfile').files[0];
    if (f) loadPngSettings(f);
    $('pngfile').value = '';
  });
  // A dropped file on the prompt box. `dragover` has to be cancelled for a file
  // or the browser opens it in place of the page; a text drag is left to the
  // textarea, which inserts it as it always has.
  const box = $('prompt');
  const hasFile = (e) => [...(e.dataTransfer?.types ?? [])].includes('Files');
  box.addEventListener('dragover', (e) => { if (hasFile(e)) e.preventDefault(); });
  box.addEventListener('drop', (e) => {
    const f = e.dataTransfer?.files?.[0];
    if (!f) return;
    e.preventDefault();
    if (f.type === 'image/png' || f.name.toLowerCase().endsWith('.png')) loadPngSettings(f);
    else {
      $('pngnote').hidden = false;
      $('pngnote').textContent = `${f.name} isn't a PNG — only PNG files carry settings.`;
    }
  });
}

export function initImage() {
  initPngSettings();
  imageLoras.init();
  onRefsChange(syncSummaries);
  initFolds();
  $('go').addEventListener('click', startRun);
  $('saveout').addEventListener('click', saveOutput);
  syncSeedLine();
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
  // Every panel's summary follows its boxes, from one listener on the tab.
  for (const ev of ['input', 'change']) {
    $('tab-image').addEventListener(ev, syncSummaries);
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
  // A crop, a scale or a step back in the dialog is a new state of this image;
  // a pass's answer goes through `takeResult`, as a run's does.
  onViewerResult('image', render);
  onPassResult('image', takeResult);
  onJobs(syncLoopNote);
  // A run that failed has no request waiting on it; its message comes here.
  onNotice((msg, oom) => {
    if (/cancelled by request/.test(msg)) {
      $('err').textContent = 'Stopped. The image in flight was not kept.';
      return;
    }
    showError($('err'), msg, oom, { ...oomOptions(), retry: () => $('go').click(), smaller: smallerSize });
  });

  loadModels();
  // A label for controls that start with a value, so it can't wait for the
  // first edit to say what it does.
  syncGoLabel();
}
