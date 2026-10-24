import { $, esc, getJSON, post } from './dom.js';
import { floatPanels } from './panels.js';
import { gridMarks, star, wireStars } from './gridmark.js';
import { onJobs, soonJobs } from './jobs.js';
import { chatSettings, useSettings } from './image.js';
import { openWorkItem } from './workitems.js';
import { upload } from './source-slot.js';
import { renderChat, sessionRound, stateText } from './chatview.js';
import { drawPerms, readPerms } from './agentperms.js';
import { wireHints } from './hints.js';
import { openGetModels } from './getmodels.js';

// ---- The Agent tab ------------------------------------------------------------
//
// A chat with a local vision-language model about the pictures you are making.
// The chat keeps its own copy of the Image tab's settings (taken when it
// starts, *Copy from Image tab* to take it again) and the agent changes that
// copy, never the Image tab under you; *Open on Image tab* brings it back by
// your press. Its rounds land in Current like any generation.
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
  const want = data?.chat.agent ?? sel.value;
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
  $('adescribe').disabled = !m?.vision || !attachments.length;
}

// ---- The chat on the stage ------------------------------------------------------

async function openChat(id) {
  const mine = ++stageSeq;
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
  $('astop').hidden = !b && data?.chat.session.state !== 'awaiting';
  $('asend').disabled = b;
  $('asession').disabled = b;
  $('adescribe').disabled = b || !attachments.length
    || !agentsList.models.find((m) => m.name === $('amodel').value)?.vision;
}

// ---- Panels -----------------------------------------------------------------------

function drawPanels() {
  if (!data) {
    $('asettings').innerHTML = '<p class="sub">Start a chat to give it settings.</p>';
    $('aperms').innerHTML = '<p class="sub">Start a chat to set what its agent may do.</p>';
    $('asessionbody').innerHTML = '<p class="sub">No chat yet.</p>';
    return;
  }
  drawSettings();
  drawPerms($('aperms'), data.toolkit);
  wireHints($('tab-agent'));
  const s = data.chat.session;
  $('asessionbody').innerHTML = s.goal
    ? `<p><b>Goal</b>: ${esc(s.goal)}</p>
       ${s.checks.length ? `<p class="sub">Checks</p><ol class="chatchecklist">${s.checks.map((c) => `<li>${esc(c)}</li>`).join('')}</ol>` : ''}
       <p class="sub">Round ${sessionRound(s)} of ${data.chat.permissions.rounds} · ${esc(stateText(data.chat) || 'idle')}</p>`
    : '<p class="sub">No session yet: write a goal in the box and press Run a session. The agent then writes checks, draws a round, judges it, and tries again, up to the rounds in Permissions.</p>';
  if (data.chat.agent && $('amodel').value !== data.chat.agent
      && agentsList.models.some((m) => m.name === data.chat.agent)) {
    $('amodel').value = data.chat.agent;
    syncModelNote();
  }
}

// The chat's copy of the settings, as a small form: what the agent's knobs are.
function drawSettings() {
  const s = data.chat.settings;
  const tk = data.toolkit;
  const opt = (v, cur) => `<option value="${esc(v)}"${v === cur ? ' selected' : ''}>${esc(v)}</option>`;
  const num = (id, label, v, extra = '') => `<label for="${id}">${label}</label>
    <input type="number" id="${id}" value="${v ?? ''}" ${extra} placeholder="model’s own">`;
  const locked = (key) => tk.locks?.[key] ? `<p class="sub">${esc(tk.locks[key])}</p>` : '';
  $('asettings').innerHTML = `
    <label for="as-template">Model</label>
    <select id="as-template">${tk.models.map((m) => opt(m.name, s.template)).join('')}${
      tk.models.some((m) => m.name === s.template) ? '' : opt(s.template ?? '', s.template)}</select>
    <label for="as-prompt">Prompt</label>
    <prompt-box><textarea id="as-prompt" class="short">${esc(s.prompt ?? '')}</textarea></prompt-box>
    <label for="as-negative">Negative prompt</label>
    <prompt-box><textarea id="as-negative" class="short">${esc(s.negative ?? '')}</textarea></prompt-box>
    ${locked('negative')}
    ${num('as-cfg', 'CFG scale', s.cfg_scale, 'step="0.1" min="0" max="30"')}${locked('cfg_scale')}
    ${num('as-steps', 'Steps', s.steps, 'min="1" max="100"')}${locked('steps')}
    <div class="row">
      <div>${num('as-w', 'Width', s.width, 'step="64" min="64" max="4096"')}</div>
      <div>${num('as-h', 'Height', s.height, 'step="64" min="64" max="4096"')}</div>
    </div>`;
}

function readSettings() {
  const s = { ...data.chat.settings };
  const n = (id) => { const v = parseFloat($(id).value); return Number.isFinite(v) ? v : null; };
  s.template = $('as-template').value;
  s.prompt = $('as-prompt').value;
  s.negative = $('as-negative').value || null;
  s.cfg_scale = n('as-cfg');
  s.steps = n('as-steps');
  s.width = n('as-w');
  s.height = n('as-h');
  return s;
}

async function saveSettings(settings) {
  if (!data) return;
  try {
    show(await post('/chats/settings', { id: chatId(), rev: data.chat.rev, settings }), { scroll: false });
  } catch (e) {
    err(String(e.message || e));
    // A 409 is the agent having changed them meanwhile: show what is there now.
    if (/changed since/.test(e.message)) openChat(chatId());
  }
}

// ---- Sending ------------------------------------------------------------------------

async function ensureChat() {
  if (data) return data.chat.id;
  const got = await post('/chats/new', { settings: chatSettings(), agent: $('amodel').value || null });
  stageSeq++;
  show(got);
  loadChats();
  return got.chat.id;
}

async function send(mode) {
  err('');
  const text = $('atext').value.trim();
  if (!text && !attachments.length) return;
  try {
    const id = await ensureChat();
    await post('/agent/send', { chat: id, text, images: attachments.map(({ kind, id }) => ({ kind, id })), mode });
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

// A picture already in the chat, described as a prompt.
async function describe(kind, id) {
  if (!data || busy()) return;
  try {
    await post('/agent/send', { chat: chatId(), text: '', images: [{ kind, id }], mode: 'describe' });
    soonJobs();
    openChat(chatId());
  } catch (e) {
    err(String(e.message || e));
  }
}

// ---- Attachments ------------------------------------------------------------------------

function drawChips() {
  $('achips').innerHTML = attachments.map((a, i) => `<span class="chip">
    <img src="/outputs/${encodeURIComponent(a.id)}/thumb.png" alt="">
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

// *New chat*: an empty stage. The chat itself is made on the first send, from
// the Image tab's settings as they are then.
function newChat() {
  stageSeq++;
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

// ---- The log's buttons ------------------------------------------------------------------------

async function onLogClick(e) {
  const t = e.target.closest('button');
  if (!t || !data) return;
  const id = chatId();
  const d = t.dataset;
  try {
    if (d.approveChange || d.rejectChange) {
      const cid = Number(d.approveChange ?? d.rejectChange);
      const r = await post('/agent/approve', { chat: id, changes: [{ id: cid, approve: !!d.approveChange }] });
      if (r.job != null) soonJobs();
      openChat(id);
    } else if (d.approveAction || d.rejectAction) {
      const aid = Number(d.approveAction ?? d.rejectAction);
      const r = await post('/agent/approve', { chat: id, actions: [{ id: aid, approve: !!d.approveAction }] });
      if (r.job != null) soonJobs();
      openChat(id);
    } else if (d.undo) {
      show(await post('/agent/undo', { chat: id, change: Number(d.undo) }), { scroll: false });
    } else if (d.openItem) {
      await openWorkItem(d.openItem);
    } else if (d.describe) {
      const [kind, pid] = d.describe.split(':');
      describe(kind, pid);
    } else if (d.usePrompt || d.copyPrompt) {
      const m = data.chat.messages.find((x) => x.n === Number(d.usePrompt ?? d.copyPrompt));
      if (!m?.prompt) return;
      if (d.usePrompt) {
        saveSettings({ ...data.chat.settings, prompt: m.prompt, ...(m.negative ? { negative: m.negative } : {}) });
      } else {
        $('prompt').value = m.prompt;
        if (m.negative) $('negative').value = m.negative;
        $('prompt').dispatchEvent(new Event('input', { bubbles: true }));
        document.querySelector('.tab[data-tab="image"]')?.click();
      }
    }
  } catch (ex) {
    err(String(ex.message || ex));
  }
}

// ---- Wiring -------------------------------------------------------------------------------------

const PANEL_DEFS = [
  { id: 'agent-model', open: true },
  { id: 'agent-settings' },
  { id: 'agent-perms' },
  { id: 'agent-session' },
  { id: 'agent-current', open: true, corner: 'bottom-right' },
  { id: 'agent-favorites' },
];

export const layoutAgentTab = () => wsPanels?.layout();

export function initAgent() {
  wsPanels = floatPanels({ host: $('tab-agent'), rail: $('tab-agent').querySelector('.wsrail'), defs: PANEL_DEFS });
  wsPanels.init();
  for (const g of grids) g.init();
  initGrip();
  $('anew').addEventListener('click', newChat);
  // The Image tab's Info names the chat a picture came from (`render` in image.js).
  document.addEventListener('yasdig:openchat', (e) => {
    document.querySelector('.tab[data-tab="agent"]')?.click();
    openChat(e.detail);
  });
  $('asend').addEventListener('click', () => send('chat'));
  $('asession').addEventListener('click', () => send('session'));
  $('adescribe').addEventListener('click', () => send('describe'));
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
      if (!busy()) send('chat');
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
  $('asettingssave').addEventListener('click', () => data && saveSettings(readSettings()));
  $('asettingscopy').addEventListener('click', () => {
    if (!data) return;
    const s = chatSettings();
    saveSettings(s);
  });
  $('asettingsuse').addEventListener('click', () => {
    if (!data) return;
    useSettings(data.chat.settings);
    document.querySelector('.tab[data-tab="image"]')?.click();
  });
  $('apermssave').addEventListener('click', async () => {
    if (!data) return;
    try { show(await post('/chats/permissions', { id: chatId(), permissions: readPerms($('aperms')) }), { scroll: false }); } catch (e) { err(String(e.message || e)); }
  });
  $('apermsdefault').addEventListener('click', async () => {
    if (!data) return;
    try {
      await post('/settings', { agent: readPerms($('aperms')) });
      $('apermsnote').textContent = 'New chats will start with these.';
    } catch (e) { err(String(e.message || e)); }
  });
  onJobs(follow);
  drawPanels();
  loadAgents();
  loadChats();
}
