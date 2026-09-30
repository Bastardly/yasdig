import { $, post } from './dom.js';
import { setFiltersBusy } from './viewer.js';

// The video buttons are in here too, and that is the important part: the GPU
// worker is serialized, so a clip generating has to lock the image tab's
// Generate as surely as it locks its own.
export const IDLE_LABELS = {
  go: 'Generate', refine: 'Refine image', hires: 'Hi-res pass',
  saveout: 'Save to output', lbsave: 'Save as new output',
  vgo: 'Generate clip', venlarge: 'Enlarge frames',
  cgo: 'Convert',
};

// What else has to be re-synced when the worker frees up. `IDLE_LABELS` can say
// what a button reads at rest; it cannot say whether the button should be
// *pressable*, which for Generate depends on the run panel. A list, for the same reason IDLE_LABELS is one —
// and it is what keeps this module free of imports from the two tabs that would
// otherwise import it back.
const idleHooks = [];
export const onIdle = (fn) => idleHooks.push(fn);

// The GPU worker is serialized, so every action locks out together; only the
// one that started the work says what it's doing.
export function setBusy(active, label) {
  for (const [id, idle] of Object.entries(IDLE_LABELS)) {
    const b = $(id);
    if (b) {
      b.disabled = !!active;
      b.textContent = id === active ? label : idle;
    }
  }
  // What a button says and whether it is pressable at rest is the owning tab's
  // to decide, so every path back to idle goes through one place: this one.
  if (!active) for (const fn of idleHooks) fn();
  // Both views of the upscale factor, not just the one above the image: they
  // are one value, so leaving the panel's copy live during a pass would let the
  // pair disagree about a number the next image is about to read.
  for (const id of ['upfactor', 'hiresscale', 'vfactor']) {
    if ($(id)) $(id).disabled = !!active;
  }
  setFiltersBusy(active);
  // Called again for each stage of a run, so the poll is started once and left
  // alone rather than restarted per stage.
  if (active) {
    if (!progTimer) progTimer = setInterval(pollProgress, PROGRESS_MS);
  } else {
    clearInterval(progTimer);
    progTimer = null;
    for (const id of ['prog', 'vprog', 'cprog']) $(id).textContent = '';
  }
}

// How often to ask what the worker is doing. Polled rather than pushed because
// there is no channel from the server to the page, and it stays cheap: /progress
// is answered on the connection thread, so it never queues behind the render it
// reports on, and one small GET per second against a multi-second step is
// nothing.
const PROGRESS_MS = 700;
let progTimer = null;

async function pollProgress() {
  try {
    const p = await (await fetch('/progress')).json();
    // `steps` is 0 until sd-cli prints its first bar, which is a real state and
    // not worth rendering as '0/0'.
    const text = p.busy && p.steps
      ? `${p.phase === 'loading' ? 'Loading weights' : 'Step'} ${p.step}/${p.steps} · ${p.rate}`
      : '';
    // Written to both tabs' readouts rather than to whichever is showing: they
    // report the same worker, and only one of them is ever visible.
    for (const id of ['prog', 'vprog', 'cprog']) $(id).textContent = text;
  } catch {
    // A dropped poll needs no reporting — the next one is PROGRESS_MS away, and
    // the request that actually matters reports its own failure.
  }
}

// Kill the running pass. Unlike the image tab's Stop — which lets the image in
// flight finish and then ends the run — there is only ever one clip in flight,
// so "stop" can only mean "kill it". The half-written frames go with it:
// ClipDir sweeps the directory when the pass doesn't reach `keep`.
export async function cancelRun(stopId, errId) {
  $(stopId).disabled = true;
  $(stopId).textContent = 'Stopping…';
  try {
    await post('/cancel', {});
  } catch (e) {
    $(errId).textContent = String(e.message || e);
  }
}

