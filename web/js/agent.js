import { $, esc, getJSON, post } from './dom.js';
import { floatPanels } from './panels.js';
import { gridMarks, star, wireStars } from './gridmark.js';
import { onJobs, passRunning, soonJobs } from './jobs.js';
import { S } from './state.js';
import { cancelPost, onPassResult, onViewerResult, setLook, setViewerImage, showFullSize } from './viewer.js';
import { openWorkItem } from './workitems.js';
import { upload } from './source-slot.js';
import { renderChat, sessionHtml, stateText } from './chatview.js';
import { openGetModels } from './getmodels.js';

// ---- The Agent tab ------------------------------------------------------------
//
// A chat with a local vision-language model about the pictures you are making.
// A chat starts on the Image tab's model with that model's saved defaults and
// holds only what the agent changes after (`chat.settings`), never the Image
// tab under you. What it may change is Settings → Agent, for every chat. Its
// rounds land in Current like any generation, and a click on one of its
// pictures opens it in Refine & adjust, on this tab (`Slot::Chat`).
//
// The server holds the chat (`work/chats/{id}/chat.json`, `agent::chat`); the
// page draws it and re-reads it as its `rev` moves, polled with the jobs while
// the agent works. A turn's reply streams in from `/chats/{id}/live`.
//
// Workspace like the Video tab: a rail of panels, the conversation on the
// stage in a column whose width is the person's (dragged, remembered), the
// composer pinned under it.

let data = null; // { chat, toolkit } of the chat on the stage
let stageSeq = 0; // bumped by New chat and a pick: a late answer for another chat is dropped
let agentsList = { models: [], driver: true };
let attachments = []; // [{ kind: 'output', id: stem }]
let liveSeq = null;
let wsPanels = null;

const DEFAULT_W = 760;
const WIDTH_KEY = 'yasdig.chat.width';
const LARGE_KEY = 'yasdig.chat.large';

const chatId = () => data?.chat.id ?? null;
const busy = () => ['queued', 'running'].includes(data?.chat.session.state);
const err = (msg) => { $('aerr').textContent = msg ?? ''; };

// ---- Agent models -------------------------------------------------------------

export async function loadAgents() {
  try {
    agentsList = await getJSON('/agents');
  } catch (e) {
    err('Could not list the agent models: ' + (e.message || e));
    return;
  }
  const sel = $('amodel');
  const want = data?.chat.agent ?? agentsList.default ?? sel.value;
  sel.innerHTML = agentsList.models.length
    ? agentsList.models.map((m) => `<option value="${esc(m.name)}">${esc(m.label)}${m.vision ? '' : ' (text only)'}</option>`).join('')
    : '<option value="">None installed</option>';
  if (agentsList.models.some((m) => m.name === want)) sel.value = want;
  syncModelNote();
}

function syncModelNote() {
  const m = agentsList.models.find((x) => x.name === $('amodel').value);
  const lines = [];
  if (!agentsList.driver) lines.push('This build has no agent driver inside it, so the agent can’t run.');
  if (!agentsList.models.length) {
    lines.push(`No agent model yet: <button type="button" class="btn" data-get-agents>Get one</button>`
      + ` <span class="sub">or put a GGUF and its mmproj in a folder of their own under ${esc(agentsList.dir ?? 'models/agents')}.</span>`);
  }
  if (m) {
    lines.push(m.vision ? 'Sees pictures.' : 'Text only: it can’t look at pictures, so no critique or describing.');
    if (m.fits === false) lines.push('Bigger than this GPU’s memory: it may not load.');
    if (m.note) lines.push(esc(m.note));
  }
  $('amodelnote').innerHTML = lines.join(' ');
}

// ---- The chat on the stage ------------------------------------------------------

async function openChat(id) {
  const mine = ++stageSeq;
  if (id !== data?.chat.id) sessionPanel(false);
  err('');
  try {
    const got = await getJSON(`/chats/${encodeURIComponent(id)}`);
    if (mine !== stageSeq) return;
    show(got);
  } catch (e) {
    err(String(e.message || e));
  }
}

function show(got, { scroll = true } = {}) {
  const was = data?.chat;
  data = got;
  liveSeq = null;
  draw(scroll || was?.id !== got.chat.id || was?.messages.length !== got.chat.messages.length);
  // A session the agent just started, or one still going in a chat come back
  // to, opens the panel. A new session is a new goal or a new first round.
  const s = got.chat.session;
  const begun = was?.id === got.chat.id && s.goal
    && (s.goal !== was.session.goal || s.first_round !== was.session.first_round);
  if ((begun || was?.id !== got.chat.id) && ['queued', 'running'].includes(s.state)) sessionPanel(true);
  drawPanels();
  markChats();
}

function draw(scroll, live = null) {
  const log = $('chatlog');
  if (!data) {
    log.innerHTML = '';
    $('atitle').value = '';
    $('astate').textContent = '';
    syncButtons();
    return;
  }
  const atBottom = log.scrollHeight - log.scrollTop - log.clientHeight < 40;
  log.innerHTML = renderChat(data.chat, live);
  if (scroll || atBottom) log.scrollTop = log.scrollHeight;
  if (document.activeElement !== $('atitle')) $('atitle').value = data.chat.title;
  $('astate').textContent = stateText(data.chat);
  syncButtons();
}

function syncButtons() {
  const b = busy();
  $('astop').hidden = !b;
  $('asend').disabled = b;
}

// ---- Panels -----------------------------------------------------------------------

// The Session panel has no rail button: it opens when the agent starts a
// session, or on coming to a chat whose session is still going, and closes
// when the stage moves to another chat or a new one, so what it shows is
// always that chat's. Its × closes it as any panel's does.
function sessionPanel(on) {
  $('panel-agent-session').open = on;
}

function drawPanels() {
  if (!data) {
    $('asessionbody').innerHTML = '<p class="sub">No chat yet.</p>';
    return;
  }
  $('asessionbody').innerHTML = sessionHtml(data.chat);
  if (data.chat.agent && $('amodel').value !== data.chat.agent
      && agentsList.models.some((m) => m.name === data.chat.agent)) {
    $('amodel').value = data.chat.agent;
    syncModelNote();
  }
}

// ---- Sending ------------------------------------------------------------------------

async function ensureChat() {
  if (data) return data.chat.id;
  // The Image tab's model, which brings its own saved defaults.
  const got = await post('/chats/new', { template: $('template').value, agent: $('amodel').value || null });
  stageSeq++;
  show(got);
  loadChats();
  return got.chat.id;
}

// Every message is a chat turn: the agent decides whether it talks, draws,
// describes an attached picture or starts a session.
async function send() {
  err('');
  const text = $('atext').value.trim();
  if (!text && !attachments.length) return;
  try {
    const id = await ensureChat();
    await post('/agent/send', { chat: id, text, images: attachments.map(({ kind, id }) => ({ kind, id })) });
    $('atext').value = '';
    attachments = [];
    drawChips();
    soonJobs();
    await openChat(id);
    loadChats();
  } catch (e) {
    err(String(e.message || e));
  }
}

// ---- Attachments ------------------------------------------------------------------------

// A picture already in the chat, attached to the next message so the person
// can ask about it. Its chip shows the chat's own copy (`src`).
function ask(kind, id, src) {
  if (attachments.some((a) => a.kind === kind && a.id === id)) return;
  attachments.push({ kind, id, src });
  if (attachments.length > 4) attachments = attachments.slice(-4);
  drawChips();
  $('atext').focus();
}

function drawChips() {
  $('achips').innerHTML = attachments.map((a, i) => `<span class="chip">
    <img src="${a.src ? esc(a.src) : `/outputs/${encodeURIComponent(a.id)}/thumb.png`}" alt="">
    <button type="button" class="hint" data-unattach="${i}" aria-label="Remove">&times;</button></span>`).join('');
  syncButtons();
}

async function attachFile(file) {
  err('');
  try {
    attachments.push({ kind: 'output', id: await upload(file) });
    if (attachments.length > 4) attachments = attachments.slice(-4);
    drawChips();
  } catch (e) {
    err(String(e.message || e));
  }
}

// ---- Current and Favorites ----------------------------------------------------------------

// `clipGrid`'s shape (video.js): Current lists both shelves, a star moves a
// chat to `user/favorite_chats/`, and Delete all reaches the unstarred only.
function chatGrid({ list, del, grid, mode, delBtn, all, empty }) {
  let items = [];
  let seq = 0;
  const onFavorites = list === '/favorite-chats';
  const marks = gridMarks({
    grid,
    mode,
    ids: () => items.filter((c) => onFavorites || !c.favorite).map((c) => c.id),
    reload: loadChats,
    err,
    onMode: () => mark(),
    actions: [{
      value: 'delete',
      button: delBtn,
      mark: 'sel',
      label: (n) => `Delete ${n} selected`,
      route: () => del,
      key: 'ids',
      ...(all ? { all: { button: all, noun: 'chat', label: (n) => (n ? `Delete all ${n} unstarred` : 'Delete all unstarred') } } : {}),
    }],
  });

  async function load() {
    const mine = ++seq;
    let got = [];
    try {
      const d = await getJSON(list);
      if (Array.isArray(d)) got = d;
    } catch (e) {
      err('Could not load chats: ' + (e.message || e));
    }
    if (mine !== seq) return;
    items = got;
    marks.prune(items.map((c) => c.id));
    draw();
  }

  function draw() {
    const el = $(grid);
    if (!items.length) {
      el.innerHTML = `<p class="sub">${empty}</p>`;
      marks.sync();
      return;
    }
    el.innerHTML = items.map((c) => `<span class="tile chattile">
      <button type="button" data-id="${esc(c.id)}" title="${esc(c.title)}">${
        c.thumb ? `<img src="/chats/${esc(c.id)}/thumb.png?${c.updated_ms}" alt="" loading="lazy">` : ''}
        <span class="chattiletitle">${esc(c.title)}</span></button>${star(c.favorite)}</span>`).join('');
    for (const b of el.querySelectorAll('button[data-id]')) b.addEventListener('click', () => click(b.dataset.id));
    wireStars(el, {
      of: (btn) => {
        const id = btn.previousElementSibling.dataset.id;
        return { favorite: !!items.find((x) => x.id === id)?.favorite, body: { ids: [id] } };
      },
      add: '/favorite-chats/add',
      remove: '/favorite-chats/remove',
      reload: loadChats,
      err,
    });
    mark();
  }

  function mark() {
    for (const b of $(grid).querySelectorAll('button[data-id]')) {
      marks.mark(b, b.dataset.id);
      b.classList.toggle('active', b.dataset.id === chatId());
    }
    marks.sync();
  }

  function click(id) {
    if (marks.toggle(id)) {
      mark();
      return;
    }
    openChat(id);
  }

  return { init: () => marks.init(), load, mark };
}

const grids = [
  chatGrid({ list: '/chats', del: '/chats/delete', grid: 'agrid', mode: 'amode', delBtn: 'adelete', all: 'adeleteall',
    empty: 'No chats yet — write to the agent below.' }),
  chatGrid({ list: '/favorite-chats', del: '/favorite-chats/delete', grid: 'afgrid', mode: 'afmode', delBtn: 'afdelete',
    empty: 'No favorites yet — star a chat to keep it.' }),
];

function loadChats() {
  return Promise.all(grids.map((g) => g.load()));
}

function markChats() {
  for (const g of grids) g.mark();
}

// *New chat*: an empty stage. The chat itself is made on the first send, on
// the Image tab's model as it is then.
function newChat() {
  stageSeq++;
  sessionPanel(false);
  data = null;
  attachments = [];
  drawChips();
  draw(true);
  drawPanels();
  markChats();
  err('');
  $('atext').focus();
}

// ---- Following a chat the agent is working on --------------------------------------------

async function follow() {
  if (!data || !busy()) return;
  const id = chatId();
  const mine = stageSeq;
  try {
    const live = await getJSON(`/chats/${encodeURIComponent(id)}/live`);
    if (mine !== stageSeq) return;
    if (live.seq != null && live.seq !== liveSeq) {
      liveSeq = live.seq;
      draw(false, live);
    }
    const got = await getJSON(`/chats/${encodeURIComponent(id)}`);
    if (mine !== stageSeq) return;
    if (got.chat.rev !== data.chat.rev) {
      const ended = !['queued', 'running'].includes(got.chat.session.state);
      show(got, { scroll: true });
      if (ended) loadChats();
    }
  } catch { /* the next poll is a moment away */ }
}

// ---- The column's width -------------------------------------------------------------------

function setWidth(w) {
  $('chatcol').style.setProperty('--chat-w', `${Math.round(w)}px`);
}

function initGrip() {
  try {
    const w = parseFloat(localStorage.getItem(WIDTH_KEY));
    if (Number.isFinite(w)) setWidth(w);
  } catch { /* the default */ }
  const grip = $('chatgrip');
  grip.addEventListener('pointerdown', (e) => {
    e.preventDefault();
    grip.setPointerCapture(e.pointerId);
    const col = $('chatcol');
    const start = e.clientX;
    const w0 = col.getBoundingClientRect().width;
    // The column is centred, so a pull on its right edge grows both sides.
    const move = (ev) => setWidth(w0 + 2 * (ev.clientX - start));
    const up = () => {
      grip.removeEventListener('pointermove', move);
      try { localStorage.setItem(WIDTH_KEY, String(col.getBoundingClientRect().width)); } catch { /* fine */ }
    };
    grip.addEventListener('pointermove', move);
    grip.addEventListener('pointerup', up, { once: true });
    grip.addEventListener('lostpointercapture', up, { once: true });
  });
  grip.addEventListener('dblclick', () => {
    setWidth(DEFAULT_W);
    try { localStorage.removeItem(WIDTH_KEY); } catch { /* fine */ }
  });
}

// Every round's pictures at the column's width, not only a round of one. A way
// of looking, so per browser, like the width.
function initLarge() {
  const box = $('alarge');
  try { box.checked = localStorage.getItem(LARGE_KEY) === '1'; } catch { /* off */ }
  const apply = () => $('chatlog').classList.toggle('large', box.checked);
  apply();
  box.addEventListener('change', () => {
    apply();
    try { localStorage.setItem(LARGE_KEY, box.checked ? '1' : '0'); } catch { /* fine */ }
  });
}

// ---- The log's buttons ------------------------------------------------------------------------

async function onLogClick(e) {
  const t = e.target.closest('button');
  if (!t || !data) return;
  const id = chatId();
  const d = t.dataset;
  try {
    if (d.undo) {
      show(await post('/agent/undo', { chat: id, change: Number(d.undo) }), { scroll: false });
    } else if (d.openItem) {
      openItem(d.openItem);
    } else if (d.ask) {
      const [kind, pid] = d.ask.split(':');
      ask(kind, pid, t.closest('.chatpic')?.querySelector('img')?.getAttribute('src'));
    } else if (d.copyPrompt) {
      const m = data.chat.messages.find((x) => x.n === Number(d.copyPrompt));
      if (!m?.prompt) return;
      $('prompt').value = m.prompt;
      if (m.negative) $('negative').value = m.negative;
      $('prompt').dispatchEvent(new Event('input', { bubbles: true }));
      document.querySelector('.tab[data-tab="image"]')?.click();
    }
  } catch (ex) {
    err(String(ex.message || ex));
  }
}

// ---- A chat's pictures in Refine & adjust -------------------------------------------------------
//
// A click on one of the chat's pictures opens it in the dialog on this tab, as
// Outputs opens a file (`outputs.js`), into a slot of its own on the server
// (`Slot::Chat`): a session drawing its next round never takes the picture
// being looked at, and one also on the Image tab stays there. ←/→ step through
// the chat's pictures in order; *Open on Image tab* reopens it there.

let pics = []; // the chat's work items, in order, as the dialog steps through them
let at = -1;
let opening = false;
let queuedAt = null;
let shownSize = '';

// Every work item the chat shows, once each, in the order shown.
function chatPictures() {
  const ids = [];
  for (const m of data?.chat.messages ?? []) {
    for (const r of m.images ?? []) if (r.kind === 'item' && !ids.includes(r.id)) ids.push(r.id);
  }
  return ids;
}

// The dialog's caption: where the picture is among the chat's, and its size.
function caption(got) {
  return `${at + 1} of ${pics.length} · ${got.width}×${got.height}`;
}

function landed(got) {
  const same = got.image_id === S.imageId && shownSize === `${got.width}×${got.height}`;
  S.imageId = got.image_id;
  S.currentSrc = `data:image/png;base64,${got.image_base64}`;
  S.workId = got.work_id ?? null;
  shownSize = `${got.width}×${got.height}`;
  setLook(got);
  $('lbsaved').textContent = caption(got);
  return same;
}

async function openPicture(i) {
  if (i < 0 || i >= pics.length) return;
  if (opening) { queuedAt = i; return; }
  // A re-filter still on its way belongs to the picture being left.
  cancelPost();
  opening = true;
  at = i;
  const wasOpen = $('lightbox').open;
  err('');
  try {
    // A picture starred since is on the other shelf.
    const id = pics[i];
    const got = await post('/workitems/open', { id, pane: 'agent' })
      .catch(() => post('/favorites/open', { id, pane: 'agent' }));
    landed(got);
    $('lberr').textContent = '';
    if (wasOpen) setViewerImage(S.currentSrc); else showFullSize(S.currentSrc);
  } catch (e) {
    S.imageId = null;
    const msg = String(e.message || e);
    err(msg);
    if ($('lightbox').open) $('lberr').textContent = msg;
  } finally {
    opening = false;
    if (queuedAt !== null) { const next = queuedAt; queuedAt = null; openPicture(next); }
  }
}

function openItem(id) {
  pics = chatPictures();
  const i = pics.indexOf(id);
  if (i >= 0) openPicture(i);
}

// Clamped, as Outputs is; not while a pass runs on the picture open.
function stepPicture(d) {
  const i = at + d;
  if (i >= 0 && i < pics.length && !passRunning()) openPicture(i);
}

function initViewerPane() {
  document.addEventListener('keydown', (e) => {
    if (S.pane !== 'agent' || !$('lightbox').open) return;
    if (e.key !== 'ArrowLeft' && e.key !== 'ArrowRight') return;
    if (/^(INPUT|TEXTAREA|SELECT)$/.test(e.target?.tagName)) return;
    e.preventDefault();
    stepPicture(e.key === 'ArrowRight' ? 1 : -1);
  });
  // A crop, a step back, a pass: a new state of the same picture.
  const land = (got) => setViewerImage(S.currentSrc, landed(got));
  onViewerResult('agent', land);
  onPassResult('agent', (got, id) => {
    if (S.pane === 'agent' && $('lightbox').open && S.imageId === id) land(got);
  });
  $('lbtoimage').addEventListener('click', async () => {
    const id = pics[at];
    if (!id) return;
    try { await openWorkItem(id); } catch (e) { $('lberr').textContent = String(e.message || e); }
  });
}

// ---- Wiring -------------------------------------------------------------------------------------

const PANEL_DEFS = [
  { id: 'agent-session' },
  { id: 'agent-current', open: true, corner: 'bottom-right' },
  { id: 'agent-favorites' },
];

export const layoutAgentTab = () => wsPanels?.layout();

export function initAgent() {
  wsPanels = floatPanels({ host: $('tab-agent'), rail: $('tab-agent').querySelector('.wsrail'), defs: PANEL_DEFS });
  wsPanels.init();
  sessionPanel(false);
  for (const g of grids) g.init();
  initGrip();
  initLarge();
  $('anew').addEventListener('click', newChat);
  // The Image tab's Info names the chat a picture came from (`render` in image.js).
  document.addEventListener('yasdig:openchat', (e) => {
    document.querySelector('.tab[data-tab="agent"]')?.click();
    openChat(e.detail);
  });
  $('asend').addEventListener('click', send);
  $('astop').addEventListener('click', async () => {
    if (!data) return;
    try {
      await post('/agent/stop', { chat: chatId() });
      soonJobs();
      openChat(chatId());
    } catch (e) { err(String(e.message || e)); }
  });
  $('atext').addEventListener('keydown', (e) => {
    // Enter sends, as a chat does; Shift+Enter is a new line.
    if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      if (!busy()) send();
    }
  });
  $('aattach').addEventListener('click', () => $('afile').click());
  $('afile').addEventListener('change', async () => {
    for (const f of $('afile').files) await attachFile(f);
    $('afile').value = '';
  });
  $('achips').addEventListener('click', (e) => {
    const b = e.target.closest('[data-unattach]');
    if (!b) return;
    attachments.splice(Number(b.dataset.unattach), 1);
    drawChips();
  });
  // A picture pasted or dropped on the tab is an attachment.
  document.addEventListener('paste', (e) => {
    if ($('tab-agent').hidden) return;
    const f = [...(e.clipboardData?.files ?? [])].find((x) => x.type.startsWith('image/'));
    if (f) {
      e.preventDefault();
      attachFile(f);
    }
  });
  const composer = $('acomposer');
  composer.addEventListener('dragover', (e) => { if ([...(e.dataTransfer?.types ?? [])].includes('Files')) e.preventDefault(); });
  composer.addEventListener('drop', (e) => {
    const files = [...(e.dataTransfer?.files ?? [])].filter((f) => f.type.startsWith('image/'));
    if (!files.length) return;
    e.preventDefault();
    files.forEach(attachFile);
  });
  $('chatlog').addEventListener('click', onLogClick);
  $('amodelnote').addEventListener('click', (e) => {
    if (e.target.closest('[data-get-agents]')) openGetModels('agent');
  });
  $('amodel').addEventListener('change', async () => {
    syncModelNote();
    if (!data) return;
    try { show(await post('/chats/agent', { id: chatId(), agent: $('amodel').value }), { scroll: false }); } catch (e) { err(String(e.message || e)); }
  });
  $('atitle').addEventListener('change', async () => {
    if (!data) return;
    try {
      show(await post('/chats/rename', { id: chatId(), title: $('atitle').value }), { scroll: false });
      loadChats();
    } catch (e) { err(String(e.message || e)); }
  });
  initViewerPane();
  onJobs(follow);
  drawPanels();
  loadAgents();
  loadChats();
}
