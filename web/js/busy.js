import { $ } from './dom.js';
import { onJobs, runningKind, stopKind } from './jobs.js';

// What each button says at rest, so a button that was waiting can be put back.
export const IDLE_LABELS = {
  go: 'Generate', refine: 'Refine image', hires: 'Hi-res pass',
  saveout: 'Save to output', lbsave: 'Save as new output',
  vgo: 'Generate clip', venlarge: 'Enlarge frames',
  cgo: 'Convert',
};

// What else has to be re-synced when a button stops waiting. `IDLE_LABELS` can
// say what a button reads at rest; it cannot say whether the button should be
// *pressable*, which for Save depends on whether anything is open. A list, for
// the same reason IDLE_LABELS is one — and it is what keeps this module free of
// imports from the tabs that would otherwise import it back.
const idleHooks = [];
export const onIdle = (fn) => idleHooks.push(fn);

// Buttons waiting on their own request: id -> { label, kind }.
//
// This used to lock *every* GPU button on the page at once, because there was
// one worker and the page was its only scheduler. The job queue is the
// scheduler now, so a button only waits on what it asked for: Generate stays
// pressable during a run (it queues the next one), and the viewer's sliders
// stay live (they are on the display lane). `kind` is the job this request
// became, so the button can say "Queued…" until the worker reaches it.
const waiting = new Map();

function draw(id) {
  const b = $(id);
  const w = waiting.get(id);
  if (!b || !w) return;
  b.disabled = true;
  b.textContent = w.kind && runningKind() !== w.kind ? 'Queued…' : w.label;
}

export function setBusy(id, label, kind = null) {
  waiting.set(id, { label, kind });
  draw(id);
}

export function setIdle(id) {
  waiting.delete(id);
  const b = $(id);
  if (b) {
    b.disabled = false;
    if (IDLE_LABELS[id]) b.textContent = IDLE_LABELS[id];
  }
  for (const fn of idleHooks) fn();
}

// For render(), which rebuilds the result's action row: a Refine still waiting
// must not come back as a fresh, pressable button.
export function redrawBusy() {
  for (const id of waiting.keys()) draw(id);
}

// Stop this tab's job. A clip or a conversion is one job, so stopping a running
// one kills it — its partial output is swept — and a queued one is simply
// taken off the queue; either way the request waiting on it fails, and the tab
// reports that.
export async function cancelRun(stopId, kind) {
  $(stopId).disabled = true;
  $(stopId).textContent = 'Stopping…';
  await stopKind(kind);
}

export function initBusy() {
  onJobs(() => { for (const id of waiting.keys()) draw(id); });
}
