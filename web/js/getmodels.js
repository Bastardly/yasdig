import { $, esc, getJSON, post } from './dom.js';
import { floatPanels } from './panels.js';
import { loadModels } from './image.js';
import { loadVideoModels } from './video.js';
import { loadAgents } from './assistant.js';
import { downloadsChanged, onDownloads, soonDownloads } from './downloads.js';

// The Models tab: the server's catalog (`src/catalog.toml`), one card per
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
// Downloads are `downloads.js`'s poll, which reads no disk: a file finishing
// (`downloadsChanged`) reloads the Image, Video and agent model lists — and
// with them the upscaler list — so a file that just landed is pickable without
// a reload; the cards (`/catalog`, which scans) are refetched only while this
// tab shows, once a second while anything moves.
//
// The cards come in categories — image models, video models, upscalers, agent
// models — each a list of its own on the wire (`entries`, `videos`,
// `upscalers`, `agents`): Help's family table reads `entries` and neither a
// video model nor an upscaler belongs in it. The tab is a workspace: the rail
// has a heading per category and a button per family, each family's cards a
// floating panel (`panels.js`), so two can be read side by side. Rail and
// panels are built once, from the first answer — the catalog is compiled in,
// so its families change only with the server, and `alive.js` asks for a
// reload then — and a poll rewrites only the panels' bodies, so open state,
// position and scroll stay.

let view = null;
let pending = null;
let wsPanels = null;
// `GET /installed`: what is on disk, grouped (`src/inventory.rs`). Fetched on
// entering the tab and when a download lands, never on the progress poll — it
// walks every model folder.
let installed = null;

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

// The categories whose cards have no families to tell apart: one rail button
// and one panel.
const SINGLE = { upscaler: 'Upscalers', agent: 'Agent models' };

// A family's first card, once each: the nav buttons, and the anchor
// `openModels` scrolls to.
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
  { id: 'agent', title: 'Agent models', entries: view.agents ?? [],
    where: 'Everything is here. Pick it beside the Assistant panel’s Send button, on the Image or Video tab.' },
].filter((c) => c.entries.length);

function card(e, where) {
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
  const badge = e.recommended ? '<span class="gmbadge">Recommended</span>' : '';
  // More than the GPU holds: it still runs, with part of it in system RAM.
  const big = view.vram && e.card_gb && e.card_gb * 2 ** 30 > view.vram
    ? `<p class="sub">Needs about ${e.card_gb} GB on the GPU, more than this one’s ${vramGb(view.vram)}:`
      + ' part of it will sit in system RAM, which is slower.</p>' : '';
  return `<div class="gmcard" data-entry="${esc(e.id)}">
    <div class="gmhead"><b>${esc(e.label)}</b>${badge}<span class="sub">${esc(e.family_label)} · ${gb(e.bytes)}</span></div>
    <p class="sub">${esc(e.note)}</p>${big}
    <p class="sub">Licence: each file has its own — see ${pages(e)}.</p>
    <table class="helptable gmfiles"><tbody>${e.files.map(fileRow).join('')}</tbody></table>
    ${done ? `<p class="sub">${where}</p>` : ''}
    <div class="btnrow">${buttons}</div>
    <div class="err" data-err></div>
  </div>`;
}

// A rail button and a panel each: a family's cards (its first card's family,
// in catalog order), or a whole category with no families to tell apart.
function groups() {
  return categories().map((c) => ({
    ...c,
    panels: SINGLE[c.id]
      ? [{ id: c.id, label: SINGLE[c.id], entries: c.entries }]
      : firstPerFamily(c.entries).map((f) => ({
        id: f.family, label: f.family_label, entries: c.entries.filter((e) => e.family === f.family),
      })),
  }));
}

// A family is recommended when any of its cards is — video ones too. The mark
// sits in place; the rail is never reordered by it.
const isRecommended = (g) => g.entries.some((e) => e.recommended);

const panelHtml = (id, label) => `
  <section class="fpanel" id="panel-gm-${id}" aria-label="${esc(label)}" hidden>
    <header class="fphead"><b>${esc(label)}</b><span class="foldstate" data-gmstate="${id}"></span><button type="button" class="fpclose" aria-label="Close ${esc(label)}">&times;</button></header>
    <div class="fpbody" data-gmbody="${id}"></div>
  </section>`;

function build() {
  const gs = groups();
  const inv = installed.groups;
  $('gmrail').innerHTML = '<p class="railtitle">Installed</p>'
    + inv.map((g) => `<button type="button" class="btn btn-choice railbtn" data-panel="gm-installed-${g.id}">`
      + `<span>${esc(g.title)}</span><span class="meta" data-gmcount="${g.id}"></span></button>`).join('')
    + gs.map((c) => `<p class="railtitle">${c.title}</p>`
    + c.panels.map((g) => `<button type="button" class="btn btn-choice railbtn" data-panel="gm-${g.id}">`
      + `<span>${esc(g.label)}</span>`
      + `${isRecommended(g) ? '<span class="gmbadge">Recommended</span>' : ''}</button>`).join('')).join('');
  $('tab-models').insertAdjacentHTML('beforeend',
    inv.map((g) => panelHtml(`installed-${g.id}`, g.title)).join('')
    + gs.flatMap((c) => c.panels).map((g) => panelHtml(g.id, g.label)).join(''));
  // Open the first time: the family picked for this GPU, else the first.
  const image = gs.find((c) => c.id === 'image')?.panels ?? [];
  const first = (image.find(isRecommended) ?? image[0])?.id;
  wsPanels = floatPanels({
    host: $('tab-models'),
    rail: $('gmrail'),
    // Installed cascades from the top, the families a little further in, so
    // the two opened the first time never sit on each other.
    defs: [
      ...inv.map((g, i) => ({ id: `gm-installed-${g.id}`, open: g.id === 'checkpoints', both: true, stack: i })),
      ...gs.flatMap((c) => c.panels.map((g, i) => ({ id: `gm-${g.id}`, open: g.id === first, both: true, stack: 3 + (i % 6) }))),
    ],
  });
  wsPanels.init();
  if (shown()) wsPanels.layout();
}

// One file on disk: what it is, what it serves, how big.
function installedRow(it) {
  const fams = it.families.map((f) => `<span class="gmchip">${esc(f)}</span>`).join('');
  return `<li class="gmitem" data-item="${esc(it.id)}">
    <div class="gmiteminfo"><b>${esc(it.label)}</b> <span class="meta">${gb(it.bytes)}</span>
      <div class="sub">${esc(it.what)}</div>
      ${it.name !== it.label ? `<div class="meta"><code>${esc(it.name)}</code></div>` : ''}
      ${fams ? `<div class="gmchips">${fams}</div>` : ''}</div>
  </li>`;
}

function drawInstalled() {
  if (!installed || !wsPanels) return;
  for (const g of installed.groups) {
    const items = installed.items.filter((i) => i.group === g.id);
    const size = items.reduce((s, i) => s + i.bytes, 0);
    const body = document.querySelector(`[data-gmbody="installed-${g.id}"]`);
    const html = items.length
      ? `<ul class="gmitems">${items.map(installedRow).join('')}</ul>`
      : '<p class="sub">None here.</p>';
    if (body.dataset.html !== html) { body.innerHTML = html; body.dataset.html = html; }
    document.querySelector(`[data-gmstate="installed-${g.id}"]`).textContent = items.length ? `${items.length} · ${gb(size)}` : '';
    document.querySelector(`[data-gmcount="${g.id}"]`).textContent = items.length ? String(items.length) : '';
    // An empty group has nothing to show; Other especially.
    document.querySelector(`[data-panel="gm-installed-${g.id}"]`).hidden = !items.length;
  }
}

export async function loadInstalled() {
  try {
    installed = await getJSON('/installed');
  } catch (e) {
    $('gmerr').textContent = 'Could not list the installed models: ' + e;
    // The family panels still build; Installed has no groups this time.
    installed ??= { groups: [], items: [] };
    return;
  }
  if (view) draw();
}

function draw() {
  if (!view || !installed) return;
  $('gmdir').innerHTML = `Saved under <code>${esc(view.models_dir)}</code>, in the folder for each kind of file`
    + ` — upscalers in <code>${esc(view.upscaler_dir)}</code>.`;
  $('gmerr').textContent = view.curl ? ''
    : 'curl was not found, so nothing can be downloaded from here. Install curl, or fetch the files '
      + 'by hand (docs/configuration.md, Get a model) into the folders each card names.';
  if (view.videos?.length) $('gmdir').innerHTML += ` Video models go in <code>${esc(view.video_dir)}</code>.`;
  if (view.agents?.length) $('gmdir').innerHTML += ` Agent models go in <code>${esc(view.agent_dir)}</code>.`;
  // What the recommendations were picked for, when the server could read it.
  if (view.vram) $('gmdir').innerHTML += ` <b>Recommended</b> is picked for this GPU’s ${vramGb(view.vram)}.`;
  if (!wsPanels) build();
  for (const c of groups()) {
    for (const g of c.panels) {
      const body = document.querySelector(`[data-gmbody="${g.id}"]`);
      if (!body) continue;
      const html = g.entries.map((e) => card(e, c.where)).join('');
      // Only when it moved: a rewrite would drop a pressed button's focus.
      if (body.dataset.html !== html) { body.innerHTML = html; body.dataset.html = html; }
      const busy = g.entries.some((e) => e.files.some(active));
      const had = g.entries.some((e) => e.files.every((f) => f.state === 'have'));
      document.querySelector(`[data-gmstate="${g.id}"]`).textContent = busy ? 'downloading' : had ? 'have it' : '';
      wsPanels.dot(`gm-${g.id}`, busy);
    }
  }
  drawInstalled();
}

const shown = () => !$('tab-models').hidden;

async function refresh() {
  try {
    // The rail is built from both, so the first time waits for both.
    if (!installed) await loadInstalled();
    view = await fetchCatalog();
  } catch (e) {
    $('gmerr').textContent = 'Could not read the catalog: ' + e;
    return;
  }
  draw();
}

// Open the tab on one family's panel, in front. For the Image tab's "Get the
// missing files…", the Assistant's agent link and the Help topic's table;
// `upscaler` and `agent` name those categories' panels.
export function openModels(family) {
  document.querySelector('.tab[data-tab="models"]')?.click();
  refresh().then(() => wsPanels?.show(`gm-${family}`));
}

// `showTab`: a hidden tab has no size to place panels against.
export const layoutModelsTab = () => wsPanels?.layout();

export function initModels() {
  document.querySelector('.tab[data-tab="models"]')?.addEventListener('click', () => {
    loadInstalled();
    refresh();
  });
  // Delegated to the tab: the panels are built after the first fetch.
  $('tab-models').addEventListener('click', async (e) => {
    const b = e.target.closest('[data-act]');
    if (!b) return;
    const box = b.closest('[data-entry]');
    b.disabled = true;
    try {
      await post(`/downloads/${b.dataset.act}`, { entry: box.dataset.entry });
      soonDownloads();
    } catch (err) {
      box.querySelector('[data-err]').textContent = String(err.message || err);
      b.disabled = false;
      return;
    }
    refresh();
  });
  // A file landed: the model may have a new runnable entry.
  downloadsChanged.subscribe(() => {
    loadModels({ refill: false });
    loadVideoModels({ refill: false });
    loadAgents();
    loadInstalled();
    if (shown()) refresh();
  });
  // Progress on the cards, only where they are seen.
  // One more after the last stops, so a cancel from the Downloads dialog
  // shows on its card.
  let moving = false;
  onDownloads((d) => {
    const now = d.files.some(active);
    if (shown() && (now || moving)) refresh();
    moving = now;
  });
  refresh();
}
