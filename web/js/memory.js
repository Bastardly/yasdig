import { esc, post } from './dom.js';
import { adoptSaved } from './settings.js';

// The out-of-memory box: what a run that ran out of memory shows in place of
// its error text, with the ways out as buttons.
//
// The server types the failure (`sd::OutOfMemory`) and sends it as `oom` beside
// the message — on a run's notice and on a waiting request's error body — so
// this never matches words in a sentence. What it knows: `kind` (gpu or ram),
// `phase` (loading, drawing, decoding), the step, where auto-fit had put the
// weights, and the memory mode the command ran with.
//
// A button sets a control and nothing else: the Memory select, or the size
// box. *Try again* re-runs exactly what failed — the caller hands that in — so a
// Refine that ran out of memory retries the Refine, not a generate. Nothing is
// saved behind anyone's back; the hint names *Save as defaults for this model*.

export const MEMORY_LABELS = {
  auto: 'Auto',
  ram: 'System RAM',
  disk: 'Stream from SSD',
};

export function headline(o) {
  const what = o.kind === 'gpu' ? 'GPU memory' : 'system memory (RAM)';
  const when = o.phase === 'drawing'
    ? (o.steps ? `while drawing (step ${o.step} of ${o.steps})` : 'at the start of drawing')
    : o.phase === 'decoding' ? 'while turning the result into pixels'
      : o.phase === 'references' ? 'while reading the reference images'
        : 'while loading the model';
  const refs = o.references > 0
    ? `, with ${o.references} reference image${o.references > 1 ? 's' : ''}` : '';
  return `Ran out of ${what} ${when}${refs}.`;
}

const GB = (b) => (b / 1e9).toFixed(1) + ' GB';

// Whether System RAM's weights fit in what is free, with the headroom sd.cpp's
// own planner keeps back: the larger of 2 GiB and a tenth (`backend_fit.cpp`).
// Unknown either way counts as fitting — the option then says what it can.
export function ramFits(o) {
  return !gttShort(o) && (o.ram_needed == null || o.ram_free == null
    || o.ram_needed <= o.ram_free - Math.max(2 * 2 ** 30, o.ram_free / 10));
}

// The GPU reaches system RAM through a window (amdgpu's GTT) the kernel caps
// at half of RAM by default, and sd.cpp keeps RAM-resident weights in it. A
// model bigger than that can't run from System RAM however much RAM is free —
// the driver refuses the submission and the device is lost. 17.3 GB of fp16
// Klein 9B against a 15.2 GiB GTT is the run that found it.
export function gttShort(o) {
  return o.ram_needed != null && o.gtt_free != null && o.ram_needed > o.gtt_free;
}

// "Needs about 17.6 GB of RAM; 26.0 GB is free." — whichever halves are known.
function ramLine(o) {
  if (o.ram_needed == null) return '';
  if (gttShort(o)) {
    return `Needs ${GB(o.ram_needed)}; the card can reach only ${GB(o.gtt_free)} of RAM (GTT).`;
  }
  return `Needs ${GB(o.ram_needed)} of RAM`
    + (o.ram_free != null ? `; ${GB(o.ram_free)} free.` : '.');
}

// The options that fit this failure, best first. Each is [label, why, act],
// with an act of null for one shown greyed out, to say why it isn't offered.
// The budget sd.cpp planned with was what ran short, not the card: the device
// had room for the step and the budget didn't — which is a reserve set too high.
export function budgetStarved(o) {
  return o.need_device_mb != null && o.avail_device_mb != null
    && o.avail_device_mb >= o.need_device_mb
    && o.avail_budget_mb != null && o.need_budget_mb != null
    && o.avail_budget_mb < o.need_budget_mb;
}

export function options(o, opts) {
  const out = [];
  const fits = ramFits(o);
  const setMemory = (v) => () => {
    const sel = document.getElementById(opts.memory);
    if (!sel) return;
    sel.value = v;
    sel.dispatchEvent(new Event('change', { bubbles: true }));
  };
  const setReserve = (n) => async () => {
    await post('/settings', { gpu_reserve_gb: n });
    adoptSaved('gpu_reserve_gb', n);
  };
  // A reserve that starved the budget is the whole story: lower it, and offer
  // nothing that asks sd.cpp to plan with even less. The run this was found on
  // had 21.8 GB free on the card and a 5.8 GB budget.
  if (o.kind === 'gpu' && (o.reserve_gb ?? 0) > 0 && budgetStarved(o)) {
    out.push(['Stop keeping the card free',
      `The card had ${GB(o.avail_device_mb * 1e6)} free, but the ${o.reserve_gb} GB reserve left only `
        + `${GB(o.avail_budget_mb * 1e6)}. Sets the reserve to 0.`,
      setReserve(0)]);
  }
  if (o.kind === 'gpu' && o.phase !== 'decoding' && o.mode === 'auto') {
    // The text encoder Auto left on the card is the clearest win: System RAM
    // lets go of it after the prompt. Otherwise, when the model was already off
    // the card, this changes less — and says so.
    const enc = o.encoder_on_card_mb;
    const offCard = /model (in system RAM|streamed)/.test(o.placement ?? '');
    const why = enc
      ? `Frees the ${GB(enc * 1e6)} text encoder Auto left on the card. `
      : offCard
        ? 'The model was already off the card; this frees little more. '
        : 'Takes the weights off the card. A little slower. ';
    out.push(fits
      ? ['Keep weights in system RAM', why + ramLine(o), setMemory('ram')]
      : ['Keep weights in system RAM',
        `Won’t fit. ${ramLine(o)}`,
        null]);
  }
  // Bigger than the GTT: the SSD is the way, and it goes first — in Auto too,
  // whose plan puts the model "in RAM" without knowing the window's size.
  const offCardRam = /model in system RAM/.test(o.placement ?? '') || o.mode === 'ram';
  if (o.kind === 'gpu' && o.mode !== 'disk' && offCardRam && gttShort(o)) {
    out.unshift(['Stream from SSD',
      `Too big for the card’s ${GB(o.gtt_free)} window onto RAM (needs ${GB(o.ram_needed)}). `
        + 'Loads from the file instead. Slower.',
      setMemory('disk')]);
  } else if (o.kind === 'ram' || (o.phase !== 'decoding' && (o.mode === 'ram' || !fits))) {
    out.push(['Stream from SSD',
      'Loads from the file as it goes. Almost no RAM; slower.', setMemory('disk')]);
  }
  // The driver refused work sd.cpp's own check had passed: the card packed too
  // tight. Offered once, at 3 GB, and only for that — a bigger reserve starves
  // the budget instead (see above), so this never steps up.
  // Not when the GTT is what's short: no reserve makes the window bigger.
  if (o.kind === 'gpu' && o.lost && !(o.reserve_gb > 0) && !gttShort(o)) {
    out.unshift(['Keep 3 GB of the card free',
      'The driver ran out of room. Saved in Settings, for every model.', setReserve(3)]);
  }
  if (o.kind === 'gpu' && o.references > 0 && opts.references) {
    out.push(['Fewer reference images',
      `${o.references} references make the drawing ${o.references + 1} images’ worth of work. `
        + 'A smaller Reference size in Settings makes each cheaper.', opts.references]);
  }
  if (o.kind === 'gpu' && opts.smaller) {
    out.push(['Smaller size', 'Memory grows with the picture’s area.', opts.smaller]);
  }
  if (opts.smallerFile) {
    out.push(['Get a smaller file…',
      'A Q8 or Q4 GGUF needs far less. The lasting fix.', opts.smallerFile]);
  }
  return out;
}

// Show `msg` in `el` — as plain text, or as the box when `oom` is set.
// `opts`: { memory: id of the Memory select, retry, smaller, smallerFile, references }.
export function showError(el, msg, oom, opts = {}) {
  if (!el) return;
  if (!oom) {
    el.textContent = msg;
    return;
  }
  const opt = options(oom, opts);
  el.innerHTML = `<div class="oombox">
    <b>${esc(headline(oom))}</b>
    ${oom.placement ? `<p class="sub">Weights were: ${esc(oom.placement)}.</p>` : ''}
    ${oom.need_device_mb != null && oom.avail_device_mb != null
      ? `<p class="sub">Needed ${GB(oom.need_device_mb * 1e6)}; the card had ${GB(oom.avail_device_mb * 1e6)} free${
        oom.avail_budget_mb != null ? ` (budget ${GB(oom.avail_budget_mb * 1e6)})` : ''}.</p>` : ''}
    ${oom.kind === 'ram' && oom.ram_free != null
      ? `<p class="sub">System RAM free now: ${GB(oom.ram_free)}.</p>` : ''}
    <div class="oomopts">${opt.map(([label, why], i) => `
      <button type="button" class="btn btn-choice" data-opt="${i}"${opt[i][2] ? '' : ' disabled'}>${esc(label)}</button>
      <span class="sub">${esc(why)}</span>`).join('')}</div>
    ${opts.retry ? '<div class="btnrow"><button type="button" class="btn btn-run" data-retry>Try again</button></div>' : ''}
    <p class="sub">A choice here is for this run. <i>Save as defaults for this model</i>
    keeps it.</p>
    <details><summary class="sub">Details</summary><pre class="oomlog">${esc(msg)}</pre></details>
  </div>`;
  el.querySelector('.oombox').addEventListener('click', (e) => {
    const b = e.target.closest('button');
    if (!b) return;
    if (b.dataset.retry !== undefined) {
      el.textContent = '';
      opts.retry();
      return;
    }
    const o = opt[Number(b.dataset.opt)];
    if (!o || !o[2]) return;
    Promise.resolve(o[2]()).then(() => {
      for (const x of el.querySelectorAll('[data-opt]')) x.classList.toggle('active', x === b);
    }, (err) => { b.textContent = 'Could not save: ' + (err.message || err); });
  });
}
