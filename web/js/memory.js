import { esc } from './dom.js';

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

function headline(o) {
  const what = o.kind === 'gpu' ? 'GPU memory' : 'system memory (RAM)';
  const when = o.phase === 'drawing' ? `while drawing (step ${o.step} of ${o.steps})`
    : o.phase === 'decoding' ? 'while turning the result into pixels'
      : 'while loading the model';
  return `Ran out of ${what} ${when}.`;
}

// The options that fit this failure, best first. Each is [label, why, act].
function options(o, opts) {
  const out = [];
  const setMemory = (v) => () => {
    const sel = document.getElementById(opts.memory);
    if (!sel) return;
    sel.value = v;
    sel.dispatchEvent(new Event('change', { bubbles: true }));
  };
  if (o.kind === 'gpu' && o.phase !== 'decoding' && o.mode === 'auto') {
    out.push(['Keep weights in system RAM',
      'The whole card is then free for the work. A little slower.', setMemory('ram')]);
  }
  if (o.kind === 'ram' || (o.phase !== 'decoding' && o.mode === 'ram')) {
    out.push(['Stream from SSD',
      'Reads the model from its file as it goes. The least memory; slower.', setMemory('disk')]);
  }
  if (o.kind === 'gpu' && opts.smaller) {
    out.push(['Smaller size', 'Memory grows with the picture’s area.', opts.smaller]);
  }
  if (opts.smallerFile) {
    out.push(['Get a smaller file…',
      'A Q6 or Q4 GGUF of the same model needs far less. The lasting fix.', opts.smallerFile]);
  }
  return out;
}

// Show `msg` in `el` — as plain text, or as the box when `oom` is set.
// `opts`: { memory: id of the Memory select, retry, smaller, smallerFile }.
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
    <div class="oomopts">${opt.map(([label, why], i) => `
      <button type="button" class="btn btn-choice" data-opt="${i}">${esc(label)}</button>
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
    if (!o) return;
    o[2]();
    for (const x of el.querySelectorAll('[data-opt]')) x.classList.toggle('active', x === b);
  });
}
