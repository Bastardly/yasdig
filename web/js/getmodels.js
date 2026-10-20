import { $, esc, getJSON, post } from './dom.js';
import { loadModels } from './image.js';
import { loadVideoModels } from './video.js';

// The Get models tab: the server's catalog (`src/catalog.toml`), one card per
// set of files — a family may have two, in different precisions — and what
// each of its files is on this disk. A recommended card says so, on the card
// and on its family's nav button; the server decides which are, against this
// GPU's VRAM (`Catalog::recommended`). The cards stay in catalog order.
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
// The cards come in categories — image models, video models, upscalers — each a
// list of its own on the wire (`entries`, `videos`, `upscalers`) and a group of
// its own in the nav, because one flat list stopped being readable. Separate
// on the wire too, because Help's family table reads `entries` and neither a
// video model nor an upscaler belongs in it. `videos` is empty unless the
// nightly Video tab is on.

let view = null;
let timer = null;
let finished = null;
let pending = null;

const gb = (b) => (b / 1e9).toFixed(b < 1e9 ? 2 : 1) + ' GB';
// A GPU's memory as it is sold: 24 GiB is a "24 GB" card.
const vramGb = (b) => `${Math.round(b / 2 ** 30)} GB`;
const active = (f) => f.state === 'queued' || f.state === 'downloading';
const needed = (f) => f.state === 'missing' || f.state === 'partial' || f.state === 'failed';
const fileName = (dest) => dest.split('/').pop();

// One fetch shared by whoever asks in the same moment — this tab and the Help
// topic's family table.
export function fetchCatalog() {
  pending ??= getJSON('/catalog').finally(() => { pending = null; });
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

// Every card, in the order drawn — the one list the poll and the buttons care
// about.
const allEntries = () => [...view.entries, ...(view.videos ?? []), ...(view.upscalers ?? [])];

// A family's first card, once each: the nav buttons, and the anchor
// `openGetModels` scrolls to.
export const firstPerFamily = (list) =>
  list.filter((e, i) => list.findIndex((o) => o.family === e.family) === i);

// What the tab shows, by category: a nav heading and a run of cards each.
// `where` is what a finished card says to do next.
const categories = () => [
  { id: 'image', title: 'Image models', entries: view.entries,
    where: 'Everything is here. Pick it in the Image tab’s model list.' },
  { id: 'video', title: 'Video models', entries: view.videos ?? [],
    where: 'Everything is here. Pick it in the Video tab’s model list.' },
  { id: 'upscaler', title: 'Upscalers', entries: view.upscalers ?? [],
    where: 'Everything is here. Pick it under Output upscale, or in Refine & adjust’s Scale.' },
].filter((c) => c.entries.length);

function card(e, i, where, first) {
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
  // A family with two cards anchors on the first of them, like the upscalers.
  const anchor = upscaler ? (i === 0 ? 'id="gm-upscaler"' : '') : (first ? `id="gm-${e.family}"` : '');
  const badge = e.recommended ? '<span class="gmbadge">Recommended</span>' : '';
  // More than the GPU holds: it still runs, with part of it in system RAM.
  const big = view.vram && e.card_gb && e.card_gb * 2 ** 30 > view.vram
    ? `<p class="sub">Needs about ${e.card_gb} GB on the GPU, more than this one’s ${vramGb(view.vram)}:`
      + ' part of it will sit in system RAM, which is slower.</p>' : '';
  return `<div class="card gmcard" ${anchor} data-entry="${esc(e.id)}">
    <div class="gmhead"><b>${esc(e.label)}</b>${badge}<span class="sub">${esc(e.family_label)} · ${gb(e.bytes)}</span></div>
    <p class="sub">${esc(e.note)}</p>${big}
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
  if (view.videos?.length) $('gmdir').innerHTML += ` Video models go in <code>${esc(view.video_dir)}</code>.`;
  // What the recommendations were picked for, when the server could read it.
  if (view.vram) $('gmdir').innerHTML += ` <b>Recommended</b> is picked for this GPU’s ${vramGb(view.vram)}.`;
  // The upscalers are one button: they have no families to tell apart.
  // A family is recommended when any of its cards is.
  const recommended = (fam) => view.entries.some((e) => e.family === fam && e.recommended);
  const navButton = (e) =>
    `<button type="button" class="btn btn-choice" data-family="${e.family}">${esc(e.family_label)}`
    + `${recommended(e.family) ? ' <span class="gmbadge">Recommended</span>' : ''}</button>`;
  $('gmnav').innerHTML = categories().map((c) => `<span class="grouphead">${c.title}</span>`
    + (c.id === 'upscaler'
      ? '<button type="button" class="btn btn-choice" data-family="upscaler">Upscalers</button>'
      : firstPerFamily(c.entries).map(navButton).join(''))).join('');
  $('gmlist').innerHTML = categories().map((c) => {
    const firsts = new Set(firstPerFamily(c.entries));
    return `<h2 class="gmgroup">${c.title}</h2>`
      + c.entries.map((e, i) => card(e, i, c.where, firsts.has(e))).join('');
  }).join('');
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
  if (finished !== null && view.finished !== finished) {
    loadModels({ refill: false });
    if (view.videos?.length) loadVideoModels({ refill: false });
  }
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
