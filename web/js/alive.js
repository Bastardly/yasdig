// Is the server there? A heartbeat on `GET /health` every two seconds, and a
// banner while it doesn't answer.
//
// Every other request on the page fails quietly on purpose (the jobs poll
// drops a miss, a button reports its own error), which left a page whose
// server had died looking exactly like an idle one. This is the one place that
// says so.
//
// Two misses in a row, not one: `/health` answers on the connection thread,
// which a slow listing can hold for a moment, and a banner that flickered on a
// busy second would teach people to ignore it.
//
// The banner is a `popover`, so it sits in the top layer above an open modal
// dialog (Refine & adjust, the clip dialog) — the place someone is most likely
// to be when a long render's server goes away. It is re-shown on every missed
// beat, which puts it back on top of a dialog opened since.
import { $ } from './dom.js';
import { soonJobs } from './jobs.js';

const BEAT_MS = 2000;
const TIMEOUT_MS = 1500;
const MISSES = 2;

let timer = null;
let misses = 0;
let down = false;
let build = null;   // the build this page's modules came from
let stopped = false;

export const isAlive = () => !down;

async function beat() {
  let ok = false;
  let got = null;
  try {
    const r = await fetch('/health', { signal: AbortSignal.timeout(TIMEOUT_MS), cache: 'no-store' });
    if (r.ok) { got = await r.json(); ok = got?.ok === true; }
  } catch { /* a miss */ }
  if (stopped) return;
  if (ok) {
    misses = 0;
    if (build === null) build = got.build ?? '';
    const restarted = got.build && build && got.build !== build;
    if (down || restarted) show(restarted ? 'restarted' : null);
    down = false;
  } else if (++misses >= MISSES) {
    down = true;
    show('down');
  }
  timer = setTimeout(beat, BEAT_MS);
}

// `state`: 'down', 'restarted' (back, but a different binary: the modules
// in this tab may not match it), or null (back, nothing to say).
function show(state) {
  const box = $('serverdown');
  if (!state) {
    if (box.matches(':popover-open')) box.hidePopover();
    // The jobs poll kept missing while we were down; catch it up now rather
    // than in up to its idle interval.
    soonJobs();
    return;
  }
  const restarted = state === 'restarted';
  $('serverdowntext').textContent = restarted
    ? 'yasdig was restarted. Reload this page so it matches the server.'
    : "yasdig isn't answering. The server stopped or was closed. Start yasdig again, and this page reconnects on its own.";
  $('serverdownreload').hidden = !restarted;
  box.classList.toggle('restarted', restarted);
  // Hidden and shown again so it goes back on top of anything opened since.
  if (box.matches(':popover-open')) box.hidePopover();
  box.showPopover();
}

// Close server's own tab: it says "stopped" in the page itself, and a banner
// saying the server isn't answering would only argue with it.
export function stopAlive() {
  stopped = true;
  clearTimeout(timer);
  const box = $('serverdown');
  if (box.matches(':popover-open')) box.hidePopover();
}

export function initAlive() {
  $('serverdownreload').addEventListener('click', () => location.reload());
  beat();
}
