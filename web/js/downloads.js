import { $, esc, post } from './dom.js';
import { observable } from './jobs.js';

// The top bar's Downloads button and its dialog: every file the server is
// fetching, waiting to fetch, or failed to fetch, whichever tab is showing.
//
// One poll of `GET /downloads`, which reads only `.part` lengths on the
// server (no model scan), so it can run every second while anything moves.
// Slow while nothing does, because a download started in another browser tab
// should show here too.
//
// `downloadsChanged` moves when a file finishes (the server's `finished`
// count): the model lists subscribe and reload, so a file that just landed is
// pickable without a reload. `onDownloads` hands every poll's summary on, for
// the Models tab's cards.

const BUSY_MS = 1000;
const IDLE_MS = 5000;
const POLL_TIMEOUT_MS = 8000;

export const downloadsChanged = observable(null);

let snap = { files: [], got: 0, bytes: 0, finished: null };
let timer = null;
let polling = false;
let again = false;
let halted = false;
const hooks = [];

export const onDownloads = (fn) => hooks.push(fn);
const active = (f) => f.state === 'queued' || f.state === 'downloading';
export const anyActive = () => snap.files.some(active);

const gb = (b) => (b / 1e9).toFixed(b < 1e9 ? 2 : 1) + ' GB';

function drawButton() {
  const b = $('dlbtn');
  const live = snap.files.filter(active);
  const failed = snap.files.length - live.length;
  b.hidden = snap.files.length === 0;
  const parts = [];
  if (live.length) {
    const pct = snap.bytes ? Math.floor((snap.got / snap.bytes) * 100) : 0;
    parts.push(`${pct}%`, `${gb(Math.max(0, snap.bytes - snap.got))} left`);
  }
  if (failed) parts.push(`${failed} failed`);
  b.textContent = `Downloads ${parts.join(' · ')}`;
  b.classList.toggle('failed', failed > 0 && !live.length);
  b.title = 'What the Models tab is downloading';
}

function row(f) {
  const pct = f.bytes ? Math.floor((f.got / f.bytes) * 100) : 0;
  const where = `data-entry="${esc(f.entry)}" data-file="${f.file}"`;
  if (f.state === 'failed') {
    return `<li class="jobrow failed">
      <div class="jobinfo"><b>${esc(f.label)}</b>
        <div class="err">${esc(f.why ?? 'Failed')}</div></div>
      <button type="button" class="btn" data-act="start" ${where}>Retry</button>
      <button type="button" class="btn" data-act="dismiss" ${where}>Dismiss</button>
    </li>`;
  }
  const state = f.state === 'queued' ? 'Queued' : `${pct}%`;
  return `<li class="jobrow ${f.state === 'downloading' ? 'running' : 'queued'}">
    <div class="jobinfo"><b>${esc(f.label)}</b>
      <div class="bar"><span class="track"><span class="fill" style="width:${pct}%"></span></span>
        <span class="val">${state}</span></div>
      <div class="sub">${gb(f.got)} of ${gb(f.bytes)}</div></div>
    <button type="button" class="btn btn-danger" data-act="cancel" ${where}>Cancel</button>
  </li>`;
}

function drawList() {
  $('dllist').innerHTML = snap.files.length
    ? snap.files.map(row).join('')
    : '<li class="sub">Nothing is downloading.</li>';
}

export async function pollDownloads() {
  if (halted) return;
  if (polling) { again = true; return; }
  polling = true;
  clearTimeout(timer);
  try {
    const got = await (await fetch('/downloads', { signal: AbortSignal.timeout(POLL_TIMEOUT_MS) })).json();
    if (!Array.isArray(got.files)) return;
    snap = got;
    // The first answer is the baseline: nothing finished since this page
    // loaded its model lists.
    if (downloadsChanged.value === null) downloadsChanged.value = snap.finished;
    else downloadsChanged.set(snap.finished);
    drawButton();
    if ($('dldlg').open) drawList();
    for (const fn of hooks) {
      try { fn(snap); } catch (e) { console.error('[yasdig] a downloads hook failed', e); }
    }
  } catch {
    // Quiet, like the jobs poll: `alive.js` is what reports a server gone.
  } finally {
    polling = false;
    if (!halted) timer = setTimeout(pollDownloads, anyActive() ? BUSY_MS : IDLE_MS);
    if (again) { again = false; pollDownloads(); }
  }
}

// After a Download press: the button should show now, not in up to IDLE_MS.
export const soonDownloads = () => setTimeout(pollDownloads, 150);
export function stopDownloads() { halted = true; clearTimeout(timer); }

export function initDownloads() {
  $('dlbtn').addEventListener('click', () => {
    drawList();
    $('dlerr').textContent = '';
    $('dldlg').showModal();
  });
  $('dlclose').addEventListener('click', () => $('dldlg').close());
  $('dldlg').addEventListener('click', (e) => {
    if (e.target === $('dldlg')) $('dldlg').close();
  });
  // Delegated: the rows are redrawn on every poll. A cancel keeps the `.part`,
  // so it asks nothing — Download on the card picks up where it stopped.
  $('dllist').addEventListener('click', async (e) => {
    const b = e.target.closest('[data-act]');
    if (!b) return;
    b.disabled = true;
    $('dlerr').textContent = '';
    const body = { entry: b.dataset.entry };
    if (b.dataset.act !== 'start') body.file = Number(b.dataset.file);
    try {
      await post(`/downloads/${b.dataset.act}`, body);
    } catch (err) {
      $('dlerr').textContent = String(err.message || err);
    }
    pollDownloads();
  });
  pollDownloads();
}
