import { $, esc, post } from './dom.js';

// The job queue, as the page sees it: the Jobs button, its dialog, every tab's
// progress line, and the images a run produces.
//
// One poll of `GET /jobs` feeds all of it. Polled rather than pushed for the
// reason the progress bar always was — there is no channel from the server to
// the page — and it stays cheap: the route answers on the connection thread, so
// it never waits behind the render it reports on. Fast while anything is queued,
// slow while nothing is, because something else (another tab, a reload) can
// queue work this page didn't.
//
// A run's images arrive here and not on the request that queued it: that
// request answered `{job}` at once, since a run can be a loop and nothing about
// it may depend on this page staying open. `latest_seq` moves when the worker
// publishes an image, and only then is the image itself fetched — a megabyte of
// base64 per poll would be the cost of pushing it every time.

const BUSY_MS = 700;
const IDLE_MS = 2500;

// What the dialog calls each kind of job. The server's names are the keys.
const KINDS = {
  generate: 'Generate', img2img: 'Image to image', refine: 'Refine', hires: 'Hi-res', redraw: 'Refine area', upscale: 'Upscale', edit: 'Edit', extend: 'Extend',
  video: 'Video', enlarge: 'Enlarge frames', convert: 'Convert',
};
const isRun = (j) => j.kind === 'generate' || j.kind === 'img2img';

let snap = { jobs: [], latest_seq: 0, notices: [], progress: {} };
// The last image and notice already handed on. `null` until the first poll,
// which adopts what is there: an image the queue last made comes back after a
// reload, but an old failure is not reported a second time.
let seenSeq = null;
let seenNotice = null;
let timer = null;
let polling = false;
let again = false;

const resultHooks = [];
const jobsHooks = [];
const noticeHooks = [];
// The live preview: the frame on screen and the one being fetched, by the
// number `progress.preview` reports.
let previewShown = null;
let previewWanted = null;
let previewUrl = null;
// A run's image landed: `fn(result)`, the same body `/generate` used to answer.
export const onResult = (fn) => resultHooks.push(fn);
// The queue moved: called after every poll.
export const onJobs = (fn) => jobsHooks.push(fn);
// A run failed: `fn(message)`. No request waits on a run, so this is the only
// way its error reaches anybody.
export const onNotice = (fn) => noticeHooks.push(fn);

export const jobCount = () => snap.jobs.length;
export const runningJob = () => snap.jobs.find((j) => j.state !== 'queued') ?? null;
// Whether a pass on some image is running. The Refine dialog's history and the
// viewer's Crop are locked meanwhile: a step back (or a crop) and a pass that
// lands afterwards would disagree about which state is current, and the pass
// would win without saying so.
const PASSES = ['refine', 'hires', 'redraw', 'upscale', 'edit', 'extend'];
export const passRunning = () => PASSES.includes(runningJob()?.kind);

// The loop ("Keep going until stopped"), unless it is ending. There is one at
// a time (`Queue::push_loop`), and it takes the Image tab's edits.
export const activeLoop = () =>
  snap.jobs.find((j) => isRun(j) && j.total === null && j.state !== 'stopping') ?? null;

// What the worker is doing, for the line under each tab's button.
function progressText() {
  const j = runningJob();
  if (!j) return '';
  const p = snap.progress || {};
  const parts = [jobTitle(j)];
  // `steps` is 0 until sd-cli prints its first bar, which is a real state and
  // not worth rendering as '0/0'.
  if (p.busy && p.steps) {
    parts.push(`${p.phase === 'loading' ? 'Loading weights' : 'Step'} ${p.step}/${p.steps}`, p.rate);
  }
  return parts.join(' · ');
}

function jobTitle(j) {
  if (!isRun(j)) return KINDS[j.kind] ?? j.kind;
  return j.total === null ? `Image ${j.done + 1}` : `Image ${j.done + 1}/${j.total}`;
}

// The dialog's third column. "(4/8)" counts the image in flight, so a run's
// first image reads 1/8 rather than 0/8.
function status(j) {
  if (j.state === 'stopping') return 'Stopping after this image';
  if (j.state === 'queued') {
    if (!isRun(j)) return 'Queued';
    return j.total === null ? 'Queued · until stopped' : `Queued · ${j.total} image${j.total > 1 ? 's' : ''}`;
  }
  if (!isRun(j)) return 'Running';
  return j.total === null
    ? `Running until stopped · ${j.done} done`
    : `(${j.done + 1}/${j.total})`;
}

function drawButton() {
  const b = $('jobsbtn');
  const n = snap.jobs.length;
  b.hidden = n === 0;
  // A loop never ends by itself, and while one is in the queue it is the
  // thing most worth knowing about it.
  const loop = snap.jobs.some((j) => isRun(j) && j.total === null);
  b.innerHTML = `Jobs (${n})`
    + (loop ? ' <span class="jobs-loop" aria-label="keeps going until stopped">!</span>' : '');
  b.title = loop ? 'A job in the queue keeps going until it is stopped' : 'What is queued on the GPU';
}

function drawList() {
  const list = $('jobslist');
  if (!snap.jobs.length) {
    list.innerHTML = '<li class="sub">Nothing is queued.</li>';
    return;
  }
  const line = progressText();
  list.innerHTML = snap.jobs.map((j) => `
    <li class="jobrow ${j.state}">
      <div class="jobinfo">
        <b>${esc(KINDS[j.kind] ?? j.kind)}</b> <span class="meta">${esc(j.label)}</span>
        <div class="sub">${esc(status(j))}</div>
        ${j.state !== 'queued' && line ? `<div class="sub">${esc(line)}</div>` : ''}
      </div>
      <button type="button" class="btn btn-danger" data-stop="${j.id}"
        ${j.state === 'stopping' ? 'disabled' : ''}>${j.state === 'queued' ? 'Remove' : 'Stop'}</button>
    </li>`).join('');
}

// The thumbnail under the progress line, while sd-cli samples. Fetched as a
// blob rather than by pointing `src` at the route: an older frame is deleted as
// soon as a newer one is complete, so a re-request could 404 and blank it. The
// shown frame only changes once the next one is in hand.
// Every `img.livepreview` on the page shows it — the run panel's and the
// Redraw dialog's — from the one fetch.
async function drawPreview() {
  const imgs = [...document.querySelectorAll('img.livepreview')];
  const show = (url) => { for (const i of imgs) { i.src = url; i.hidden = false; } };
  const live = () => snap.progress?.preview;
  const n = live();
  if (typeof n !== 'number') {
    for (const i of imgs) i.hidden = true;
    previewShown = previewWanted = null;
    return;
  }
  if (n === previewShown || n === previewWanted) return;
  previewWanted = n;
  try {
    const res = await fetch(`/preview/${n}.png`);
    if (!res.ok) return; // pruned already; the next poll names a newer one
    const blob = await res.blob();
    // The run may have ended, or a newer frame overtaken this one, meanwhile.
    if (previewWanted !== n || typeof live() !== 'number') return;
    if (previewUrl) URL.revokeObjectURL(previewUrl);
    previewUrl = URL.createObjectURL(blob);
    show(previewUrl);
    previewShown = n;
  } catch {
    // A dropped fetch is the next poll's to retry.
  } finally {
    if (previewWanted === n) previewWanted = null;
  }
}

async function deliverLatest(seq) {
  try {
    const data = await (await fetch('/jobs/latest')).json();
    if (data.error) return;
    seenSeq = seq;
    for (const fn of resultHooks) fn(data);
  } catch {
    // Picked up on the next poll: `seenSeq` didn't move.
  }
}

export async function pollJobs() {
  if (polling) { again = true; return; }
  polling = true;
  clearTimeout(timer);
  try {
    const got = await (await fetch('/jobs')).json();
    // An `{error}` body adopted as the snapshot used to throw in `finally`
    // below, and the poll was never scheduled again until a reload.
    if (!Array.isArray(got.jobs) || !Array.isArray(got.notices)) return;
    snap = got;
    const first = seenSeq === null;
    if (first) {
      seenNotice = Math.max(0, ...snap.notices.map((n) => n.seq));
      seenSeq = 0;
    }
    for (const n of snap.notices) {
      if (n.seq > seenNotice) {
        seenNotice = n.seq;
        for (const fn of noticeHooks) fn(n.message, n.oom ?? null);
      }
    }
    if (snap.latest_seq > seenSeq) await deliverLatest(snap.latest_seq);
    const text = progressText();
    // All three tabs' readouts: they report the same worker, and only one of
    // them is ever visible.
    for (const id of ['prog', 'vprog', 'cprog', 'rdprog']) $(id).textContent = text;
    drawPreview();
    drawButton();
    if ($('jobsdlg').open) drawList();
    for (const fn of jobsHooks) fn(snap);
  } catch {
    // A dropped poll needs no reporting — the next one is a moment away, and
    // the request that actually matters reports its own failure.
  } finally {
    polling = false;
    timer = setTimeout(pollJobs, snap.jobs.length ? BUSY_MS : IDLE_MS);
    if (again) { again = false; pollJobs(); }
  }
}

// After queueing something: the button should appear now, not in up to
// IDLE_MS. Delayed a touch for a request still on its way to the queue.
export const soonJobs = () => setTimeout(pollJobs, 150);

// Stop or remove, by id. `confirm` is the dialog's; the Stop buttons beside
// Generate, the clip and the conversion never asked and still don't.
export async function stopJob(id, ask = false) {
  const j = snap.jobs.find((x) => x.id === id);
  if (!j) return;
  if (ask) {
    const what = `${KINDS[j.kind] ?? j.kind} · ${j.label}`;
    const msg = j.state === 'queued'
      ? `Remove "${what}" from the queue?`
      : isRun(j)
        ? `Stop "${what}"?\n\nThe image being made now is finished first, with its after-each passes.`
        : `Stop "${what}" now?\n\nWhat it has made so far is thrown away.`;
    if (!confirm(msg)) return;
  }
  $('jobserr').textContent = '';
  try {
    await post('/jobs/stop', { id });
  } catch (e) {
    // Most often "already finished", which the next poll makes plain.
    if (ask) $('jobserr').textContent = String(e.message || e);
  }
  pollJobs();
}

// The Stop beside a tab's own button: that tab's job, wherever it is in the
// queue. The page waits on at most one of each kind, so the kind finds it; the
// running one first, since that is the one a Stop is usually about.
export function stopKind(kind) {
  const mine = snap.jobs.filter((j) => j.kind === kind);
  const j = mine.find((x) => x.state !== 'queued') ?? mine[mine.length - 1];
  if (j) return stopJob(j.id);
  return Promise.resolve();
}

// Whether the worker is on a job of this kind right now — so a button waiting
// on its own request can say "Queued…" until its turn comes.
export const runningKind = () => runningJob()?.kind ?? null;

export function initJobs() {
  $('jobsbtn').addEventListener('click', () => {
    drawList();
    $('jobserr').textContent = '';
    $('jobsdlg').showModal();
  });
  $('jobsclose').addEventListener('click', () => $('jobsdlg').close());
  // The box inside carries all the padding, so the dialog element itself is
  // the target only of a click on its backdrop. Modal already keeps such a
  // click from reaching anything behind it, which is what `inert` would buy.
  $('jobsdlg').addEventListener('click', (e) => {
    if (e.target === $('jobsdlg')) $('jobsdlg').close();
  });
  // Delegated: the rows are redrawn on every poll.
  $('jobslist').addEventListener('click', (e) => {
    const b = e.target.closest('[data-stop]');
    if (b) stopJob(Number(b.dataset.stop), true);
  });
  pollJobs();
}
