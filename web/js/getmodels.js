import { $, esc, post } from './dom.js';
import { loadModels } from './image.js';

// The Get models tab: the server's catalog (`src/catalog.toml`), one card per
// recommended set, and what each of its files is on this disk.
//
// The page sends a catalog id and nothing else — never a URL, never a path —
// so all it can ask for is what the catalog lists. Whether a file is "had" is
// the server's answer too: a part counts when *any* file on disk fits its slot,
// which is the same test the Models fold uses, so a Qwen3-4B under another name
// is not offered a second time.
//
// Polled once a second while anything is queued or downloading, whichever tab
// is showing: `finished` moving is what reloads the Image tab's model list —
// and with it the upscaler list — so a file that just landed is pickable
// without a reload.
//
// Upscalers are a list of their own on the wire (`upscalers`), drawn after the
// families under one nav button. Separate because Help's family table reads
// `entries` and an upscaler is no family.

let view = null;
let timer = null;
let finished = null;
let pending = null;

const gb = (b) => (b / 1e9).toFixed(b < 1e9 ? 2 : 1) + ' GB';
const active = (f) => f.state === 'queued' || f.state === 'downloading';
const needed = (f) => f.state === 'missing' || f.state === 'partial' || f.state === 'failed';
const fileName = (dest) => dest.split('/').pop();

// One fetch shared by whoever asks in the same moment — this tab and the Help
// topic's family table.
export function fetchCatalog() {
  pending ??= fetch('/catalog').then((r) => r.json()).finally(() => { pending = null; });
  return pending;
}

function fileRow(f) {
  let state;
  switch (f.state) {
    case 'have':
      state = `✓ ${f.name && fileName(f.name) !== fileName(f.dest)
        ? `you have <code>${esc(f.name)}</code>` : 'have it'}`;
      break;
    case 'downloading':
    case 'queued': {
      const pct = f.bytes ? Math.floor((f.got / f.bytes) * 100) : 0;
      state = `<div class="bar"><span class="track"><span class="fill" style="width:${pct}%"></span></span>`
        + `<span class="val">${f.state === 'queued' ? 'queued' : pct + '%'}</span></div>`
        + `<span class="sub">${gb(f.got)} of ${gb(f.bytes)}</span>`;
      break;
    }
    case 'partial':
      state = `paused at ${gb(f.got)} of ${gb(f.bytes)}`;
      break;
    case 'failed':
    case 'wrong':
      state = `<span class="err">${esc(f.why)}</span>`;
      break;
    default:
      state = gb(f.bytes);
  }
  return `<tr><td>${esc(f.slot_label)}</td><td><code>${esc(fileName(f.dest))}</code></td><td>${state}</td></tr>`;
}

// Each repository once, linked by its owner/name — the place its licence is.
// A GGUF repackage carries the original model's licence, which its page names.
function pages(e) {
  const urls = [...new Set(e.files.map((f) => f.page))];
  return urls.map((u) => `<a href="${esc(u)}" target="_blank" rel="noopener noreferrer">`
    + `${esc(u.replace('https://huggingface.co/', ''))}</a>`).join(', ');
}

// Every card, families first — the one list the poll and the buttons care about.
const allEntries = () => [...view.entries, ...(view.upscalers ?? [])];

function card(e, i) {
  const left = e.files.filter(needed).reduce((s, f) => s + f.bytes - f.got, 0);
  const busy = e.files.some(active);
  const partial = !busy && e.files.some((f) => f.got > 0 && (f.state === 'partial' || f.state === 'failed'));
  const done = e.files.every((f) => f.state === 'have');
  const buttons = [
    !busy && left > 0
      ? `<button type="button" class="btn btn-run" data-act="start" ${view.curl ? '' : 'disabled'}>`
        + `${partial ? 'Resume' : 'Download'} ${gb(left)}</button>` : '',
    busy ? '<button type="button" class="btn btn-danger" data-act="cancel">Cancel</button>' : '',
    partial ? '<button type="button" class="btn" data-act="clear">Remove partial</button>' : '',
  ].join('');
  const upscaler = e.family === 'upscaler';
  // The upscalers share one anchor, on the first of them.
  const anchor = upscaler ? (i === 0 ? 'id="gm-upscaler"' : '') : `id="gm-${e.family}"`;
  const where = upscaler
    ? 'Everything is here. Pick it under Output upscale, or in the Refine dialog’s Upscale.'
    : 'Everything is here. Pick it in the Image tab’s model list.';
  return `<div class="card gmcard" ${anchor} data-entry="${esc(e.id)}">
    <div class="gmhead"><b>${esc(e.label)}</b><span class="sub">${esc(e.family_label)} · ${gb(e.bytes)}</span></div>
    <p class="sub">${esc(e.note)}</p>
    <p class="sub">Licence: each file has its own — see ${pages(e)}.</p>
    <table class="helptable gmfiles"><tbody>${e.files.map(fileRow).join('')}</tbody></table>
    ${done ? `<p class="sub">${where}</p>` : ''}
    <div class="btnrow">${buttons}</div>
    <div class="err" data-err></div>
  </div>`;
}

function draw() {
  if (!view) return;
  $('gmdir').innerHTML = `Saved under <code>${esc(view.models_dir)}</code>, in the folder for each kind of file`
    + ` — upscalers in <code>${esc(view.upscaler_dir)}</code>.`;
  $('gmerr').textContent = view.curl ? ''
    : 'curl was not found, so nothing can be downloaded from here. Install curl, or fetch the files '
      + 'by hand (docs/configuration.md, Get a model) into the folders each card names.';
  const ups = view.upscalers ?? [];
  $('gmnav').innerHTML = view.entries.map((e) =>
    `<button type="button" class="btn btn-choice" data-family="${e.family}">${esc(e.family_label)}</button>`).join('')
    + (ups.length ? '<button type="button" class="btn btn-choice" data-family="upscaler">Upscalers</button>' : '');
  $('gmlist').innerHTML = view.entries.map((e) => card(e)).join('') + ups.map(card).join('');
}

async function refresh() {
  clearTimeout(timer);
  timer = null;
  try {
    view = await fetchCatalog();
  } catch (e) {
    $('gmerr').textContent = 'Could not read the catalog: ' + e;
    return;
  }
  // The first answer only sets the baseline; a later one that moved means a
  // file landed, so the model list may have a new runnable entry.
  if (finished !== null && view.finished !== finished) loadModels();
  finished = view.finished;
  draw();
  if (allEntries().some((e) => e.files.some(active))) timer = setTimeout(refresh, 1000);
}

// Open the tab on one family's card. For the Image tab's "Get the missing
// files…" and the Help topic's table.
export function openGetModels(family) {
  document.querySelector('.tab[data-tab="getmodels"]')?.click();
  refresh().then(() => $(`gm-${family}`)?.scrollIntoView({ behavior: 'smooth', block: 'start' }));
}

export function initGetModels() {
  document.querySelector('.tab[data-tab="getmodels"]')?.addEventListener('click', refresh);
  $('gmnav').addEventListener('click', (e) => {
    const b = e.target.closest('[data-family]');
    if (b) $(`gm-${b.dataset.family}`)?.scrollIntoView({ behavior: 'smooth', block: 'start' });
  });
  $('gmlist').addEventListener('click', async (e) => {
    const b = e.target.closest('[data-act]');
    if (!b) return;
    const box = b.closest('[data-entry]');
    b.disabled = true;
    try {
      await post(`/downloads/${b.dataset.act}`, { entry: box.dataset.entry });
    } catch (err) {
      box.querySelector('[data-err]').textContent = String(err.message || err);
      b.disabled = false;
      return;
    }
    refresh();
  });
  // A download started before a reload is still running on the server; the
  // first poll picks it up and keeps polling.
  refresh();
}
