import { $, bar, esc, post, numOr } from './dom.js';
import { S, P, escErr } from './state.js';
import { setBusy, onIdle } from './busy.js';
import { setPost, setPostStart, setViewerImage, showFullSize } from './viewer.js';
import { loadWorkItems, markGrid } from './workitems.js';
import { loraPicker } from './loras.js';
import { sourceSlot } from './source-slot.js';

// The checkpoint list the server published. Exported because the work-item
// grid looks a name up in it; only this module refills it.
export let models = [];

// This tab's attached LoRAs. Exported because `applyWorkItem` has to put a
// reopened generation's back: the grid restores what made an image, and the
// adapters are as much a part of that as the steps and the seed.
export const imageLoras = loraPicker({
  rows: 'loras', add: 'loraadd', family: () => currentModel()?.family ?? null,
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
  none: 'Nothing — text to image. Choose, upload, drop or paste an image to start from it.',
  err: 'err',
  onChange: () => syncMode(),
});

// Which controls mean something depends on whether there is a source, so the
// ones that don't are hidden rather than left looking live: base size, the
// txt2img's own steps and the output upscale describe a render from noise, and
// the strength, steps, working size and img2img CFG describe a pass over a
// picture. `data-when` in the markup says which is which.
function syncMode() {
  const source = !!imageSource.get();
  for (const el of $('tab-image').querySelectorAll('[data-when]')) {
    el.hidden = (el.dataset.when === 'source') !== source;
  }
  syncCfgPlaceholders();
  syncSrcNote();
  syncRunNote();
}

// Both this and the pass itself round to 64, and they must agree: this is what
// the picture is scaled to, that is what the latent grid allows.
const WORKING_SIDES = [768, 1024, 1536, 2048, 3072];

function currentModel() {
  return models.find((m) => m.name === $('template').value);
}

// Pre-fill the Advanced controls from the selected model's defaults.
function syncAdvanced() {
  const m = currentModel();
  if (!m) return;
  // Re-greys the LoRA lists against the new model's family.
  imageLoras.refresh();
  $('steps').value = m.steps;
  $('cfg').value = m.cfg_scale;
  $('sampler').value = m.sampler;
  $('w').value = m.base_width;
  $('h').value = m.base_height;
  $('upscale').value = m.upscale_factor;
  setUpscaler(m.upscaler);
  $('rsteps').value = m.refine_steps;
  $('rstrength').value = m.refine_strength;
  $('hsteps').value = m.hires_steps;
  $('hstrength').value = m.hires_strength;
  // Left empty when the model doesn't set one, rather than pre-filled with the
  // number it resolves to. Pre-filling would look friendlier and would be a lie:
  // it pins the value, so moving the box it follows would silently stop carrying
  // over to this pass.
  $('rcfg').value = m.refine_cfg_scale ?? '';
  $('hcfg').value = m.hires_cfg_scale ?? '';
  setPost({ sharpness: m.sharpness, clarity: m.clarity });
  // The pass over a source. Filled whether or not one is picked, so picking one
  // later finds the model's numbers already there.
  $('srcsteps').value = m.source_steps;
  $('srcstrength').value = m.source_strength;
  $('icfg').value = m.img2img_cfg_scale ?? '';
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
}

// What the pass over a source will actually do, in a line: the step floor
// (`SdRunner::img2img` raises the count to `profile.steps × strength`), the CFG
// it resolves to, and the working-size ceiling. `m.steps` rather than the Steps
// box, because that box is the txt2img's and is hidden here — the floor is the
// model's base step count.
export function syncSrcNote() {
  const m = currentModel();
  if (!m || !imageSource.get()) return;
  const n = parseFloat($('srcsteps').value) || 0;
  const s = parseFloat($('srcstrength').value) || 0;
  const eff = effectiveSteps(n, s, m.steps);
  const base = parseFloat($('cfg').value);
  const own = parseFloat($('icfg').value);
  const cfg = Number.isFinite(own) ? own : base;
  $('srcnote').textContent =
    `Runs ${eff} step${eff === 1 ? '' : 's'} at strength ${s}`
    + (eff > n ? ` (raised from ${n} — the floor is ${Math.ceil(m.steps * s)} at this strength)` : '')
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

// Mirror the step floor the server applies (see SdRunner::img2img): a pass that
// starts further up the noise schedule needs proportionally more steps, so the
// count in the box is a minimum. Showing the real number keeps the extra time a
// higher strength costs from being a surprise.
export function effectiveSteps(steps, strength, base) {
  if (!(steps > 0)) return 0;
  return Math.max(steps, Math.ceil(base * strength));
}

// A blank per-pass CFG box means "whatever the box below it says", which is a
// thing the field has to admit to rather than decide quietly — so the number it
// resolves to goes in the placeholder, live, because the base CFG it ends up
// following is one field away and editable.
//
// The chain is the server's (`Profile::refine_cfg`): this pass's box, then
// `img2img_cfg_scale`, then the base CFG. That middle rung is the img2img CFG
// box — but only a generate *from a source* sends it, so from noise it is the
// model's value that the image retains, and that is what is read.
export function syncCfgPlaceholders() {
  const num = (id) => {
    const v = parseFloat($(id).value);
    return Number.isFinite(v) ? v : null;
  };
  const source = !!imageSource.get();
  const base = num('cfg');
  const mid = source ? num('icfg') : currentModel()?.img2img_cfg_scale;
  for (const id of ['rcfg', 'hcfg']) $(id).placeholder = mid ?? base ?? '';
  $('icfg').placeholder = base ?? '';

  // Here because this already runs everywhere a CFG box changes — typed, a
  // model switch, a reopened work item, a source picked or cleared. A pass
  // over a source runs at its own CFG, which falls through to the base box.
  syncNegative('negative', 'negnote', source ? num('icfg') ?? base : base);
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
  ['width', 'w'], ['height', 'h'],
  ['upscale_factor', 'upscale'], ['upscaler', 'upscaler', 'choice'],
  ['hires_steps', 'hsteps'], ['hires_strength', 'hstrength'], ['hires_cfg_scale', 'hcfg', 'blank'],
  ['refine_steps', 'rsteps'], ['refine_strength', 'rstrength'], ['refine_cfg_scale', 'rcfg', 'blank'],
  // The pass over a source. Sent with no source picked too: the boxes still
  // hold the model's numbers, so they drop out as unchanged.
  ['source_steps', 'srcsteps'], ['source_strength', 'srcstrength'],
  ['img2img_cfg_scale', 'icfg', 'blank'],
];

async function saveModelDefaults() {
  const m = currentModel();
  if (!m) return;
  const values = {};
  for (const [key, id, kind] of MODEL_KNOBS) {
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
    await post('/model-settings', { model: m.name, values });
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
    if (!models.length) {
      $('err').textContent = 'No models found. Drop a .safetensors into the models/ folder.';
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
export const upOptions = () => UPSCALES.map(([v, label]) =>
  `<option value="${v}"${v === S.upFactor ? ' selected' : ''}>${label}</option>`).join('');

export function setUpFactor(v) {
  S.upFactor = v;
  for (const id of ['upfactor', 'hiresscale', 'vfactor']) {
    if ($(id) && parseFloat($(id).value) !== v) $(id).value = v;
  }
  syncRunNote();
}

// Info first, image last: on a narrow screen the metadata and the action
// buttons stay visible without scrolling past a tall image.
export function render(data) {
  const t = data.timings;
  const stages = [['generate', t.gen_ms], ['upscale', t.upscale_ms ?? 0], ['fsr', t.fsr_ms],
                  ['refine', t.refine_ms], ['hi-res', t.hires_ms]];
  const total = stages.reduce((sum, [, ms]) => sum + ms, 0);
  const max = Math.max(...stages.map(([, ms]) => ms), 1);
  const bars = stages.filter(([, ms]) => ms > 0)
    .map(([name, ms]) => bar(name, ms, max)).join('');

  S.imageId = data.image_id;
  // The seed box is left exactly as typed. Writing the drawn seed back into it
  // turns "-1" into a one-shot: you ask for random, get a number, and every
  // generate after that silently repeats the same image until you notice and
  // retype -1. The seed used is reported below and is one click from the box,
  // which covers the reason it was ever written back.
  // Hi-res first: it's the pass that enlarges, and refine is the finishing
  // touch you run on whatever size you ended up at.
  let actions = '';
  if (data.can_hires) {
    // From `upFactor`, not a hardcoded 2: this select is destroyed and rebuilt
    // on every render, so the choice has to live outside it to survive one.
    actions += `<select id="upfactor" title="Upscale before the hi-res pass">${upOptions()}</select>
      <button id="hires" class="btn btn-run">Hi-res pass</button>`;
  }
  if (data.can_refine) {
    actions += '<button id="refine" class="btn btn-run">Refine image</button>';
  }
  // Always offered, including for a plain generate — a base render can be worth
  // keeping too. Only generations are archived automatically; this is how a
  // refined or hi-res image gets kept.
  actions += '<button id="saveout" class="btn">Save to output</button>';
  // The sliders are behind this button, so it has to be a button. Clicking the
  // image opens the same viewer, but an affordance nobody can see is a feature
  // nobody finds — and the sliders used to be visible furniture.
  actions += '<button id="viewadjust" class="btn">View &amp; adjust</button>';

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
    <div class="bars">${bars}</div>
    `;

  P('result').querySelector('img').addEventListener('click', () => showFullSize(S.currentSrc));
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
  if ($('refine')) $('refine').addEventListener('click', refine);
  if ($('hires')) $('hires').addEventListener('click', hires);
  if ($('saveout')) $('saveout').addEventListener('click', saveOutput);
  if ($('upfactor')) {
    $('upfactor').addEventListener('change', (e) => setUpFactor(parseFloat(e.target.value)));
  }

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
}

async function escalate(which, path, payload, label) {
  if (S.imageId === null) return;
  setBusy(which, label);
  escErr('');
  try {
    render(await post(path, { image_id: S.imageId, ...payload }));
  } catch (e) {
    escErr(String(e.message || e));
    // 400 = the pass isn't available any more (evicted, or the server
    // restarted), so the controls are dead; keep the image on screen. Any
    // other failure is retryable — the server kept its intermediates.
    if (e.status === 400) {
      S.imageId = null;
      const row = document.querySelector('.actions');
      if (row) row.remove();
    }
  } finally {
    setBusy(null);
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

// Each button sends its own checkpoint. Empty means "the one that made the
// image", which is what an unchosen select sends and what the server reads an
// empty string as.
const refine = () => escalate('refine', '/refine',
  { model: P('refmodel').value || null, ...escalateSettings() }, 'Refining…');
const hires = () => escalate('hires', '/hires',
  { model: P('hiresmodel').value || null, upscale: parseFloat($('upfactor').value),
    ...escalateSettings() }, 'Hi-res pass…');

// ---- Runs -------------------------------------------------------------------
//
// A run is one or more generations, plus whatever the "After each image" boxes
// ask for. It is driven from this page rather than by a server-side batch job on
// purpose. The GPU worker is a single blocking consumer, so one job that held it
// for a whole run would leave /adjust, the grid and every other client dead for
// the length of it, with nothing to cancel it by. Looping over the routes that
// already exist costs one round trip per image — nothing against a diffusion —
// and buys three things: each result on screen as it lands, a stop that only has
// to wait for the image in flight, and no new server code at all.
//
// The price is that the loop lives in the page: close the tab and it ends after
// the current image. For a local tool with the browser open that is a fair
// trade, and it does mean a runaway loop is always one tab-close from stopping.

const runTotal = () =>
  $('autorun').checked ? Infinity : Math.max(1, parseInt($('count').value, 10) || 1);

// The button does three different things, so it says three different things.
function syncGoLabel() {
  const n = runTotal();
  $('go').textContent =
    n === Infinity ? 'Start loop generation' : n > 1 ? `Generate ${n}` : 'Generate';
}

// State what the button will do, in a line, before it is pressed. Four controls
// interact here, and working out what they add up to isn't the user's job.
function syncRunNote() {
  const n = runTotal();
  const after = [
    $('opthires').checked ? `hi-res ${S.upFactor}×` : null,
    $('optrefine').checked ? 'refine' : null,
    $('optsave').checked ? 'save to output' : null,
  ].filter(Boolean);
  const seed = parseInt($('seed').value, 10);
  const walks = Number.isFinite(seed) && seed >= 0 && n > 1;
  const source = imageSource.get();
  $('runnote').textContent =
    (n === Infinity ? 'Generates until you press Stop'
      : `Generates ${n} image${n > 1 ? 's' : ''}`)
    + (source ? ` from ${source}` : '')
    + (after.length ? ` · after each: ${after.join(' → ')}` : '')
    + (walks ? ` · seeds ${seed}–${n === Infinity ? '…' : seed + n - 1}` : '');
}

// One image's request, as `[route, body]`: from noise it is `/generate`, from a
// source `/img2img`, and the two share everything that isn't about where the
// picture starts. Plus the seed walk: a pinned seed advances by one per image,
// because a batch at a fixed seed is otherwise N copies of the same picture;
// image 0 is still precisely the seed in the box, which is the reason to pin
// one. -1 stays -1 and the server draws a fresh one each time. A batch over one
// source is therefore N seeds of one transform, which is a real use — pick the
// best of eight.
function genPayload(index) {
  const seed = parseInt($('seed').value, 10);
  const common = {
    prompt: $('prompt').value,
    template: $('template').value,
    negative: $('negative').value || null,
    seed: Number.isFinite(seed) && seed >= 0 ? seed + index : -1,
    cfg_scale: numOr('cfg'),
    sampler: $('sampler').value || null,
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
      source_steps: numOr('srcsteps'),
      source_strength: numOr('srcstrength'),
      // The pass's own CFG; null follows `cfg_scale`, the same chain as ever.
      img2img_cfg_scale: numOr('icfg'),
    }];
  }
  return ['/generate', {
    ...common,
    steps: numOr('steps'),
    width: numOr('w'),
    height: numOr('h'),
    // The FSR enlarge on the way to the output size. An empty box sends nothing
    // and the model's own factor runs; 1 is a real answer and means no upscale.
    upscale_factor: numOr('upscale'),
    // Always sent: `""` is FSR, explicitly, which has to be sayable over a
    // saved default that names a model.
    upscaler: $('upscaler').value,
  }];
}

async function startRun() {
  if (S.run) return;
  S.run = { stop: false, done: 0, total: runTotal() };
  $('err').textContent = '';
  syncStopButton();
  try {
    while (!S.run.stop && S.run.done < S.run.total) {
      // Progress goes in the button that started it — the one control on the
      // page that is certainly still where the user is looking.
      const n = S.run.done + 1;
      const at = (what) =>
        `${what} ${n}${S.run.total === Infinity ? '' : '/' + S.run.total}…`;

      setBusy('go', at('Generating'));
      render(await post(...genPayload(S.run.done)));
      // Only a generate creates a work item — refine and hi-res escalate the one
      // that's already there, and adjust doesn't touch the disk at all. Per
      // image rather than once at the end, so the strip fills as the run goes.
      loadWorkItems();

      // Hi-res before refine, the order the buttons are in and for the reason
      // they're in it: hi-res is the pass that enlarges, refine is the finishing
      // touch you run at whatever size you ended up at. Both read the Advanced
      // panel through the same escalateSettings() the buttons use, so an
      // automated pass and a manual one run identically.
      if ($('opthires').checked) {
        setBusy('go', at('Hi-res'));
        render(await post('/hires',
          { image_id: S.imageId, upscale: S.upFactor,
            model: P('hiresmodel').value || null, ...escalateSettings() }));
      }
      if ($('optrefine').checked) {
        setBusy('go', at('Refining'));
        render(await post('/refine',
          { image_id: S.imageId, model: P('refmodel').value || null,
            ...escalateSettings() }));
      }
      if ($('optsave').checked) {
        setBusy('go', at('Saving'));
        const saved = await post('/save-output', { image_id: S.imageId });
        $('saved').textContent = `saved → ${saved.path}`;
      }
      S.run.done += 1;
    }
  } catch (e) {
    // Deliberately not escalate()'s behaviour of reporting beside the image and
    // carrying on: a failure here is the run's failure. Whatever killed image 3
    // kills image 4 as well — VRAM, a missing checkpoint, a pass the model
    // hasn't got — and a hundred repeats of it help nobody. Everything already
    // generated is in the grid, and the message says what to change.
    $('err').textContent = String(e.message || e);
  } finally {
    S.run = null;
    setBusy(null);
    syncStopButton();
  }
}

// Stop after the image in flight has finished its whole pipeline. The hi-res,
// the refine and the save are that image, and dropping a refined one a single
// call short of saving it is not what anybody means by "stop".
function stopRun() {
  if (!S.run) return;
  S.run.stop = true;
  $('stop').disabled = true;
  $('stop').textContent = 'Stopping after this image…';
}

// Shown only while there is something to stop *between* — a single image is over
// when it's over. setBusy never touches it, so it stays clickable for the whole
// run, which is the entire point of it.
function syncStopButton() {
  const show = !!S.run && S.run.total > 1;
  $('stop').hidden = !show;
  if (show) {
    $('stop').disabled = false;
    $('stop').textContent = 'Stop';
  }
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
    setBusy(null);
  }
}

export function initImage() {
  imageLoras.init();
  $('go').addEventListener('click', startRun);
  $('stop').addEventListener('click', stopRun);
  // The Advanced panel's view of the shared upscale factor. Built here rather
  // than in the markup so there is one list of factors on the page.
  $('hiresscale').innerHTML = upOptions();
  $('hiresscale').addEventListener('change', (e) => setUpFactor(parseFloat(e.target.value)));
  // 'input' covers the checkboxes too, and keeps the count responsive per keystroke
  // rather than on blur — the button's own label is the feedback.
  for (const id of ['count', 'autorun']) {
    $(id).addEventListener('input', () => { syncGoLabel(); syncRunNote(); });
  }
  // The seed is in here because a pinned one walks across a batch, and the note is
  // where that's stated.
  for (const id of ['opthires', 'optrefine', 'optsave', 'seed']) {
    $(id).addEventListener('input', syncRunNote);
  }
  // Each model carries its own defaults, so re-fill the controls on switch.
  $('template').addEventListener('change', syncAdvanced);
  $('savemodel').addEventListener('click', saveModelDefaults);
  // A note about the model that was selected is stale once another one is.
  $('template').addEventListener('change', () => { $('savemodelmsg').textContent = ''; });
  // What an empty per-pass CFG box resolves to is whatever the chain under it
  // says, so the placeholders have to follow that field as it is typed in —
  // and with a source, the img2img CFG is the middle of that chain.
  for (const id of ['cfg', 'icfg']) $(id).addEventListener('input', syncCfgPlaceholders);
  // The line under the source's boxes says what they add up to.
  for (const id of ['srcsteps', 'srcstrength', 'srcmaxside', 'cfg', 'icfg']) {
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

  loadModels();
  // Both are labels for controls that start with a value, so neither can wait for
  // the first edit to say what it does.
  syncGoLabel();
  syncRunNote();
}
