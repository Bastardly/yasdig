import { $, bar, esc, post, numOr } from './dom.js';
import { S, P, escErr } from './state.js';
import { setBusy, onIdle } from './busy.js';
import { setPost, setPostStart, setViewerImage, showFullSize } from './viewer.js';
import { loadWorkItems, markGrid } from './workitems.js';
import { loadI2Outputs, syncI2 } from './img2img.js';
import { loraPicker } from './loras.js';

// The checkpoint list the server published. Exported because the img2img tab and
// the work-item grid both look a name up in it; only this module refills it.
export let models = [];

// This tab's attached LoRAs. Exported because `applyWorkItem` has to put a
// reopened generation's back: the grid restores what made an image, and the
// adapters are as much a part of that as the steps and the seed.
export const imageLoras = loraPicker({ rows: 'loras', add: 'loraadd' });

function currentModel() {
  return models.find((m) => m.name === $('template').value);
}

// Pre-fill the Advanced controls from the selected model's defaults.
function syncAdvanced() {
  const m = currentModel();
  if (!m) return;
  $('steps').value = m.steps;
  $('cfg').value = m.cfg_scale;
  // Left empty when the model doesn't set one, rather than pre-filled with the
  // number it resolves to. Pre-filling would look friendlier and would be a lie:
  // it pins the value, so moving the CFG box afterwards would silently stop
  // carrying over to the escalations.
  $('icfg').value = m.img2img_cfg_scale ?? '';
  $('sampler').value = m.sampler;
  $('w').value = m.base_width;
  $('h').value = m.base_height;
  $('rsteps').value = m.refine_steps;
  $('rstrength').value = m.refine_strength;
  $('hsteps').value = m.hires_steps;
  $('hstrength').value = m.hires_strength;
  setPost({ sharpness: m.sharpness, clarity: m.clarity });
  syncIcfgPlaceholder();
}

// Mirror the step floor the server applies (see SdRunner::img2img): a pass that
// starts further up the noise schedule needs proportionally more steps, so the
// count in the box is a minimum. Showing the real number keeps the extra time a
// higher strength costs from being a surprise.
export function effectiveSteps(steps, strength, base) {
  if (!(steps > 0)) return 0;
  return Math.max(steps, Math.ceil(base * strength));
}

// A blank Refine/hi-res CFG box means "whatever the CFG box says", which is a
// thing the field has to admit to rather than decide quietly — so the number it
// resolves to goes in the placeholder, live, because the base CFG it follows is
// one field away and editable.
export function syncIcfgPlaceholder() {
  const base = parseFloat($('cfg').value);
  $('icfg').placeholder = Number.isFinite(base) ? base : '';
}

export async function loadModels() {
  try {
    models = await (await fetch('/models')).json();
    // Four selects off one list: the two that choose what generates, and the two
    // that choose what escalates. The escalation pair leads with a blank option
    // meaning "whatever made the image", which is what the server reads an empty
    // string as.
    for (const [id, blank] of [['template', null], ['i2template', null],
                               ['escmodel', '— same as generated —'],
                               ['i2escmodel', '— same as generated —']]) {
      const sel = $(id);
      sel.innerHTML = '';
      if (blank) {
        const none = document.createElement('option');
        none.value = '';
        none.textContent = blank;
        sel.appendChild(none);
      }
      for (const m of models) {
        const opt = document.createElement('option');
        opt.value = m.name;
        const tag = m.discovered ? 'discovered' : 'preset';
        opt.textContent = blank
          // An escalation never uses a model's base size, so quoting it there
          // would be a number that means nothing about what the button will do.
          ? `${m.name} [${tag}]`
          : `${m.name} [${tag}] — ${m.out_width}×${m.out_height}`;
        sel.appendChild(opt);
      }
    }
    if (!models.length) {
      $('err').textContent = 'No models found. Drop a .safetensors into the models/ folder.';
    }
    syncAdvanced();
    syncI2();
  } catch (e) {
    $('err').textContent = 'Could not load models: ' + e;
  }
}

export const UPSCALES = [[1, '1× (same size)'], [1.5, '1.5×'], [2, '2×'], [3, '3×'], [4, '4×']];

// One factor, three places to set it: the select above the image, the one in the
// run panel — which has to exist too, because the first is rebuilt by every
// render and so isn't there before the first image — and the one over a clip's
// frames. All three are views of `upFactor`; this keeps them saying the same
// thing, which matters because two of them are on tabs you can't see at once.
export const upOptions = () => UPSCALES.map(([v, label]) =>
  `<option value="${v}"${v === S.upFactor ? ' selected' : ''}>${label}</option>`).join('');

export function setUpFactor(v) {
  S.upFactor = v;
  for (const id of ['upfactor', 'runscale', 'vfactor']) {
    if ($(id) && parseFloat($(id).value) !== v) $(id).value = v;
  }
  syncRunNote();
}

// Info first, image last: on a narrow screen the metadata and the action
// buttons stay visible without scrolling past a tall image.
export function render(data) {
  const t = data.timings;
  const stages = [['generate', t.gen_ms], ['fsr', t.fsr_ms],
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
  const box = $(S.pane === 'img2img' ? 'i2prompt' : 'prompt');
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
    $(S.pane === 'image' ? 'seed' : 'i2seed').value = data.seed;
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
// Read from whichever pane is showing, because both tabs end in the same two
// buttons. There is deliberately one function rather than a copy per tab: a run
// that grew its own list of knobs is how the panel starts lying about what the
// button will do.
export const escalateSettings = () => {
  const p = (id) => (S.pane === 'image' ? id : 'i2' + id);
  return {
    // A checkpoint, unlike everything else here, isn't a number in a box — but
    // it passes the same test: it describes the pass about to run. Empty means
    // "the one that made the image", which is what an unchosen select sends.
    model: P('escmodel').value || null,
    refine_steps: numOr(p('rsteps')),
    refine_strength: numOr(p('rstrength')),
    hires_steps: numOr(p('hsteps')),
    hires_strength: numOr(p('hstrength')),
    // Here null is a value, not an omission: clearing the box unsets the override
    // so the passes follow the CFG box again. Sending nothing would pin whatever
    // was last set, which is the one thing this field must never do.
    img2img_cfg_scale: numOr(S.pane === 'image' ? 'icfg' : 'i2icfg'),
  };
};

const refine = () => escalate('refine', '/refine', escalateSettings(), 'Refining…');
const hires = () => escalate('hires', '/hires',
  { upscale: parseFloat($('upfactor').value), ...escalateSettings() }, 'Hi-res pass…');

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
  $('runnote').textContent =
    (n === Infinity ? 'Generates until you press Stop'
      : `Generates ${n} image${n > 1 ? 's' : ''}`)
    + (after.length ? ` · after each: ${after.join(' → ')}` : '')
    + (walks ? ` · seeds ${seed}–${n === Infinity ? '…' : seed + n - 1}` : '');
}

// One image's request: exactly what a single Generate has always sent, plus the
// seed walk. A pinned seed advances by one per image, because a batch at a fixed
// seed is otherwise N copies of the same picture; image 0 is still precisely the
// seed in the box, which is the reason to pin one. -1 stays -1 and the server
// draws a fresh one each time.
function genPayload(index) {
  const seed = parseInt($('seed').value, 10);
  return {
    prompt: $('prompt').value,
    template: $('template').value,
    negative: $('negative').value || null,
    seed: Number.isFinite(seed) && seed >= 0 ? seed + index : -1,
    steps: numOr('steps'),
    cfg_scale: numOr('cfg'),
    sampler: $('sampler').value || null,
    width: numOr('w'),
    height: numOr('h'),
    // The same five the escalations send, from the same place — they set where
    // this image's passes start, and `/refine` and `/hires` can move them
    // afterwards. `img2img_cfg_scale` is among them, and null for an empty box
    // still means "leave it unset" here: the passes keep following cfg_scale
    // rather than getting pinned to whatever it resolves to today.
    //
    // `model` is dropped rather than spread, and this is the one place it
    // matters: it is the escalations' checkpoint, and `template` above is this
    // generation's. Sent as-is it would arrive at /generate as a stray key —
    // harmless today, and a name collision waiting to be renamed into a bug.
    ...(({ model, ...rest }) => rest)(escalateSettings()),
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
      render(await post('/generate', genPayload(S.run.done)));
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
          { image_id: S.imageId, upscale: S.upFactor, ...escalateSettings() }));
      }
      if ($('optrefine').checked) {
        setBusy('go', at('Refining'));
        render(await post('/refine', { image_id: S.imageId, ...escalateSettings() }));
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
    // work/output/ is also the img2img tab's source pool, so what was just
    // saved is now something to start from.
    loadI2Outputs();
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
  // The run panel's view of the shared upscale factor. Built here rather than in
  // the markup so there is one list of factors on the page.
  $('runscale').innerHTML = upOptions();
  $('runscale').addEventListener('change', (e) => setUpFactor(parseFloat(e.target.value)));
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
  // What an empty Refine/hi-res CFG box resolves to is whatever the box above it
  // says, so the placeholder has to follow that field as it is typed in.
  $('cfg').addEventListener('input', syncIcfgPlaceholder);

  // `IDLE_LABELS.go` is only a fallback: what the button says at rest depends on
  // the run panel, which that map can't know.
  onIdle(syncGoLabel);

  loadModels();
  // Both are labels for controls that start with a value, so neither can wait for
  // the first edit to say what it does.
  syncGoLabel();
  syncRunNote();
}
