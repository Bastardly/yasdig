import { $, esc, getJSON, post } from './dom.js';
import { onJobs, queueChanged, soonJobs } from './jobs.js';
import { openWorkItem } from './workitems.js';
import { choosePicture, upload } from './source-slot.js';
import { renderChat, sessionHtml, stateText } from './chatview.js';
import { flushBoard, follow as followBoard, showBoard } from './board.js';
import { openGetModels } from './getmodels.js';
import { wireHints } from './hints.js';

// ---- The Assistant panel ------------------------------------------------------
//
// A chat with a local vision-language model, in the tab it works on: a panel
// of the Image tab's workspace and one of the Video tab's (`mountAssistant`,
// once each). A chat belongs to one (`chat.context`), and each panel lists
// only its own.
//
// On the Image tab the controls *are* the chat's settings: what the person
// changes there is written into the chat (`/chats/settings`, debounced) and
// the agent works from it from its next round or turn; what the agent
// changes lands in the controls (`apply`). Its pictures land on the stage, as
// Generate's do. What it may change is Settings → Agent, for every chat. On
// the Video tab a chat makes films: its storyboard is the Shots rows and the
// Cast panel while the chat is open (`board.js`).
//
// The server holds the chat (`work/chats/{id}/chat.json`, `agent::chat`); the
// panel draws it and re-reads it as its `rev` moves, polled with the jobs
// while the agent works. A turn's reply streams in from `/chats/{id}/live`.

const LARGE_KEY = 'yasdig.chat.large';
const mounts = {}; // pane → mount, for a chat opened from elsewhere
let agentsList = { models: [], driver: true };

// The panel's insides; its element is the workspace's `panel-…-assist`.
const MARKUP = `
  <div class="fpbody chatcol">
    <div class="chathead">
      <select class="achats" aria-label="Chat" title="Your chats on this tab, newest first"></select>
      <button type="button" class="btn astar" title="Keep this chat in Favorites">&#9734;</button>
      <button type="button" class="btn btn-danger adel" title="Delete this chat">Delete</button>
    </div>
    <div class="chathead">
      <input class="chattitle atitle" placeholder="New chat" aria-label="Chat title">
      <span class="sub astate"></span>
    </div>
    <div class="chatlog alog"></div>
    <div class="checks"><label data-help="alarge"><input type="checkbox" class="alarge"> Large pictures</label></div>
  </div>
  <div class="fpfoot composer">
    <div class="err aerr"></div>
    <div class="chips achips"></div>
    <prompt-box><textarea class="short atext"></textarea></prompt-box>
    <div class="btnrow composerrow">
      <button type="button" class="btn apick" title="Attach a picture from Outputs or Favorites">Choose…</button>
      <button type="button" class="btn aupload" title="Attach a picture from your computer (or paste or drop one)">Upload…</button>
      <input type="file" class="afile" accept="image/*" multiple hidden>
      <select class="composeragent amodel" aria-label="Agent"
        title="The agent this chat talks to. Settings → Agent sets the one new chats start with, and what it may change."></select>
      <span class="grow"></span>
      <button type="button" class="btn btn-danger astop" hidden>Stop</button>
      <button type="button" class="btn btn-run asend" title="Send (Enter)">Send</button>
    </div>
    <p class="sub amodelnote"></p>
  </div>`;

const WORDS = {
  image: {
    empty: 'Ask for a prompt or a picture, attach a picture and ask about it, or say what you want got right — '
      + 'the agent works with this tab’s settings, and you see them change.',
    placeholder: 'A fox in a red knitted hat, snowy forest — keep trying until the hat is red',
  },
  video: {
    empty: 'Ask for a short film: the agent writes its shots into the Shots panel and its cast into the '
      + 'Cast panel, which you change as you like, then Generate clip.',
    placeholder: 'A one-minute film: two friends meet in the rain at a bus stop',
  },
};

export async function loadAgents() {
  try {
    agentsList = await getJSON('/agents');
  } catch (e) {
    for (const m of Object.values(mounts)) m.err('Could not list the agent models: ' + (e.message || e));
    return;
  }
  for (const m of Object.values(mounts)) m.agentsLoaded();
}

// A chat opened from elsewhere (the Image tab's Info names the chat a picture
// came from): its own tab's panel shows it.
async function openAnywhere(id) {
  try {
    const got = await getJSON(`/chats/${encodeURIComponent(id)}`);
    const m = mounts[got.chat.context] ?? mounts.image;
    document.querySelector(`.tab[data-tab="${m.pane}"]`)?.click();
    m.reveal(got);
  } catch (e) {
    mounts.image?.err(String(e.message || e));
  }
}

// `pane`: 'image' or 'video'. `panel`: the panel element. `settings()`: the
// tab's controls as a `/generate` body (Image only); `apply(settings)` puts an
// agent's change into them; `edits`: the element whose input/change events are
// the person's edits of them.
export function mountAssistant({ pane, panel, settings = null, apply = null, edits = null }) {
  panel.insertAdjacentHTML('beforeend', MARKUP);
  wireHints(panel);
  const q = (c) => panel.querySelector(`.${c}`);
  const log = q('alog');
  log.dataset.empty = WORDS[pane].empty;
  q('atext').placeholder = WORDS[pane].placeholder;

  let data = null; // { chat, toolkit } of the chat shown
  let seq = 0; // bumped by New chat and a pick: a late answer for another chat is dropped
  let attachments = []; // [{ kind: 'output', id: stem, src? }]
  let liveSeq = null;
  let chats = []; // this tab's, both shelves
  let appliedRev = null; // the chat's `settings_rev` the controls were last set from
  let sentJSON = null; // the controls as last written into the chat

  const chatId = () => data?.chat.id ?? null;
  const busy = () => ['queued', 'running'].includes(data?.chat.session.state);
  const err = (msg) => { q('aerr').textContent = msg ?? ''; };
  const onImage = pane === 'image';
  const story = pane === 'video';

  // ---- Agent models -------------------------------------------------------------

  function agentsLoaded() {
    const sel = q('amodel');
    const want = data?.chat.agent ?? agentsList.default ?? sel.value;
    sel.innerHTML = agentsList.models.length
      ? agentsList.models.map((m) => `<option value="${esc(m.name)}">${esc(m.label)}${m.vision ? '' : ' (text only)'}</option>`).join('')
      : '<option value="">None installed</option>';
    if (agentsList.models.some((m) => m.name === want)) sel.value = want;
    syncModelNote();
  }

  function syncModelNote() {
    const m = agentsList.models.find((x) => x.name === q('amodel').value);
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
    q('amodelnote').innerHTML = lines.join(' ');
  }

  // ---- The chat shown --------------------------------------------------------------

  async function openChat(id) {
    const mine = ++seq;
    if (id !== data?.chat.id) sessionPanel(false);
    err('');
    try {
      const got = await getJSON(`/chats/${encodeURIComponent(id)}`);
      if (mine !== seq) return;
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
    drawList();
    follows(got.chat);
  }

  function draw(scroll, live = null) {
    if (!data) {
      log.innerHTML = '';
      q('atitle').value = '';
      q('astate').textContent = '';
      syncButtons();
      return;
    }
    const atBottom = log.scrollHeight - log.scrollTop - log.clientHeight < 40;
    log.innerHTML = renderChat(data.chat, live);
    if (scroll || atBottom) log.scrollTop = log.scrollHeight;
    if (document.activeElement !== q('atitle')) q('atitle').value = data.chat.title;
    q('astate').textContent = stateText(data.chat);
    syncButtons();
  }

  function syncButtons() {
    const b = busy();
    q('astop').hidden = !b;
    q('asend').disabled = b;
    q('adel').hidden = !data;
    q('astar').hidden = !data;
    q('astar').innerHTML = data?.favorite ? '&#9733;' : '&#9734;';
    q('astar').title = data?.favorite ? 'In Favorites — click to move it back' : 'Keep this chat in Favorites';
  }

  // ---- The tab's controls, both ways (Image) ------------------------------------------

  // The agent's writes into the controls: when the chat's settings moved on
  // since they were last set from it (a change, an Undo, a session's round),
  // or on coming to the chat. What was just written in by the person is no
  // change of the agent's, so nothing moves under them.
  function follows(chat) {
    if (!onImage || chat.context !== 'image' || chat.settings_rev === appliedRev) return;
    appliedRev = chat.settings_rev;
    const now = JSON.stringify(chat.settings);
    if (now === sentJSON) return;
    apply(chat.settings);
    sentJSON = JSON.stringify(settings());
  }

  // The person's edit, into the chat on screen — debounced and only when it
  // differs from what the chat was last given, so a drag is one write.
  let editTimer = null;
  function edited(e) {
    if (!data || data.chat.context !== 'image' || panel.contains(e.target)) return;
    clearTimeout(editTimer);
    editTimer = setTimeout(async () => {
      if (!data) return;
      const body = settings();
      const s = JSON.stringify(body);
      if (s === sentJSON) return;
      sentJSON = s;
      try {
        await post('/chats/settings', { id: chatId(), settings: body });
      } catch (ex) {
        err(String(ex.message || ex));
      }
    }, 600);
  }

  // ---- The Session panel, and the Video tab's board ----------------------------------------

  // The Session panel (Image) has no rail button: it opens when the agent
  // starts a session, or on coming to a chat whose session is still going, and
  // closes when the panel moves to another chat or a new one, so what it shows
  // is always that chat's. Its × closes it as any panel's does.
  function sessionPanel(on) {
    if (onImage) $('panel-img-session').open = on;
  }

  function drawPanels() {
    if (onImage) $('asessionbody').innerHTML = data ? sessionHtml(data.chat) : '<p class="sub">No chat yet.</p>';
    // The film is the Shots rows and the Cast panel (`board.js`).
    if (story) followBoard(data, (g) => show(g, { scroll: false }));
    if (data?.chat.agent && q('amodel').value !== data.chat.agent
        && agentsList.models.some((m) => m.name === data.chat.agent)) {
      q('amodel').value = data.chat.agent;
      syncModelNote();
    }
  }

  // ---- Sending ------------------------------------------------------------------------

  // The Video tab's model, or null before its list has loaded.
  const videoModel = () => $('vmodel').value || null;

  async function ensureChat() {
    if (data) return data.chat.id;
    // The Image tab's model, which brings its own saved defaults, and on the
    // Image tab its controls whole. And the Video tab's, which a film is shot on.
    const body = onImage ? settings() : null;
    const got = await post('/chats/new', {
      template: $('template').value, agent: q('amodel').value || null, video: videoModel(), context: pane, settings: body,
    });
    seq++;
    sentJSON = body ? JSON.stringify(body) : null;
    appliedRev = got.chat.settings_rev;
    show(got);
    loadChats();
    return got.chat.id;
  }

  // Every message is a chat turn: the agent decides whether it talks, draws,
  // describes an attached picture or proposes a session, which waits for the
  // person's Start.
  async function send() {
    err('');
    const text = q('atext').value.trim();
    if (!text && !attachments.length) return;
    try {
      const id = await ensureChat();
      clearTimeout(editTimer);
      if (story) await flushBoard();
      const body = onImage ? settings() : null;
      if (body) sentJSON = JSON.stringify(body);
      await post('/agent/send', {
        chat: id, text, images: attachments.map(({ kind, id }) => ({ kind, id })), video: videoModel(), settings: body,
      });
      q('atext').value = '';
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
    q('atext').focus();
  }

  function drawChips() {
    q('achips').innerHTML = attachments.map((a, i) => `<span class="chip">
      <img src="${a.src ? esc(a.src) : `/outputs/${encodeURIComponent(a.id)}/thumb.png`}" alt="">
      <button type="button" class="hint" data-unattach="${i}" aria-label="Remove">&times;</button></span>`).join('');
    syncButtons();
  }

  // An output by its stem, chosen or just uploaded; the server finds it in
  // either gallery.
  function attachOutput(stem) {
    if (attachments.some((a) => a.kind === 'output' && a.id === stem)) return;
    attachments.push({ kind: 'output', id: stem });
    if (attachments.length > 4) attachments = attachments.slice(-4);
    drawChips();
  }

  async function attachFile(file) {
    err('');
    try {
      attachOutput(await upload(file));
    } catch (e) {
      err(String(e.message || e));
    }
  }

  // ---- The chat list ------------------------------------------------------------------------

  // This tab's chats, Favorites and Current alike (`/chats` lists both,
  // starred ones flagged), newest first; *New chat* on top.
  async function loadChats() {
    try {
      const d = await getJSON('/chats');
      chats = Array.isArray(d) ? d.filter((c) => (c.context ?? 'image') === pane) : [];
    } catch (e) {
      err('Could not load chats: ' + (e.message || e));
    }
    drawList();
  }

  function drawList() {
    const sel = q('achats');
    const here = chats.some((c) => c.id === chatId()) || !data ? chats
      : [{ id: data.chat.id, title: data.chat.title || 'New chat', favorite: data.favorite }, ...chats];
    sel.innerHTML = '<option value="">New chat</option>'
      + here.map((c) => `<option value="${esc(c.id)}">${c.favorite ? '★ ' : ''}${esc(c.title)}</option>`).join('');
    sel.value = chatId() ?? '';
  }

  // *New chat*: an empty panel. The chat itself is made on the first send, on
  // the tab's settings as they are then.
  function newChat() {
    seq++;
    sessionPanel(false);
    data = null;
    appliedRev = null;
    sentJSON = null;
    attachments = [];
    drawChips();
    draw(true);
    drawPanels();
    drawList();
    err('');
    q('atext').focus();
  }

  async function star() {
    if (!data) return;
    try {
      await post(data.favorite ? '/favorite-chats/remove' : '/favorite-chats/add', { ids: [chatId()] });
      await openChat(chatId());
      loadChats();
    } catch (e) {
      err(String(e.message || e));
    }
  }

  async function del() {
    if (!data || !confirm(`Delete the chat “${data.chat.title || 'New chat'}”? Its pictures stay in Current.`)) return;
    try {
      await post(data.favorite ? '/favorite-chats/delete' : '/chats/delete', { ids: [chatId()] });
      newChat();
      loadChats();
    } catch (e) {
      err(String(e.message || e));
    }
  }

  // ---- Following a chat the agent is working on --------------------------------------------

  // The reply as it is written, every poll while this chat is busy.
  async function follow() {
    if (!data || !busy()) return;
    const id = chatId();
    const mine = seq;
    try {
      const live = await getJSON(`/chats/${encodeURIComponent(id)}/live`);
      if (mine !== seq) return;
      if (live.seq != null && live.seq !== liveSeq) {
        liveSeq = live.seq;
        draw(false, live);
      }
    } catch { /* the next poll is a moment away */ }
  }

  // The chat itself, whenever the queue moves (`queueChanged`): a step began
  // or ended, a picture landed. Not gated on what this panel last saw — the
  // panel's idea of whether the chat is busy is what goes stale — so a leg
  // started anywhere is followed, and its writes reach the controls.
  async function refresh() {
    if (!data) return;
    const id = chatId();
    const mine = seq;
    try {
      const got = await getJSON(`/chats/${encodeURIComponent(id)}`);
      if (mine !== seq) return;
      if (got.chat.rev !== data.chat.rev) show(got, { scroll: true });
    } catch { /* the next change, or the next poll, tries again */ }
    loadChats();
  }

  // Every round's pictures at the panel's width, not only a round of one. A
  // way of looking, so per browser.
  function initLarge() {
    const box = q('alarge');
    try { box.checked = localStorage.getItem(LARGE_KEY) === '1'; } catch { /* off */ }
    const set = () => log.classList.toggle('large', box.checked);
    set();
    box.addEventListener('change', () => {
      set();
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
      } else if (d.proposal === 'start') {
        t.disabled = true;
        await post('/agent/session', { chat: id, start: true });
        soonJobs();
        await openChat(id);
      } else if (d.proposal === 'no') {
        show(await post('/agent/session', { chat: id, start: false }), { scroll: false });
      } else if (d.openClip) {
        // The Video tab opens it (`video.js`).
        document.dispatchEvent(new CustomEvent('yasdig:openclip', { detail: d.openClip }));
      } else if (d.openStory != null) {
        showBoard();
      } else if (d.openItem) {
        // On the Image tab's stage, in Refine & adjust, like any work item.
        await openWorkItem(d.openItem);
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

  // ---- Wiring -------------------------------------------------------------------------------

  initLarge();
  if (onImage && edits) {
    edits.addEventListener('input', edited);
    edits.addEventListener('change', edited);
  }
  q('achats').addEventListener('change', () => {
    const id = q('achats').value;
    if (id) openChat(id);
    else newChat();
  });
  q('astar').addEventListener('click', star);
  q('adel').addEventListener('click', del);
  q('asend').addEventListener('click', send);
  q('astop').addEventListener('click', async () => {
    if (!data) return;
    try {
      await post('/agent/stop', { chat: chatId() });
      soonJobs();
      openChat(chatId());
    } catch (e) { err(String(e.message || e)); }
  });
  q('atext').addEventListener('keydown', (e) => {
    // Enter sends, as a chat does; Shift+Enter is a new line.
    if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      if (!busy()) send();
    }
  });
  q('apick').addEventListener('click', () => choosePicture(attachOutput));
  q('aupload').addEventListener('click', () => q('afile').click());
  q('afile').addEventListener('change', async () => {
    for (const f of q('afile').files) await attachFile(f);
    q('afile').value = '';
  });
  q('achips').addEventListener('click', (e) => {
    const b = e.target.closest('[data-unattach]');
    if (!b) return;
    attachments.splice(Number(b.dataset.unattach), 1);
    drawChips();
  });
  // A picture pasted while the composer has focus, or dropped on it, is an
  // attachment; elsewhere on the tab a paste is the tab's own.
  const composer = panel.querySelector('.composer');
  composer.addEventListener('paste', (e) => {
    const f = [...(e.clipboardData?.files ?? [])].find((x) => x.type.startsWith('image/'));
    if (f) {
      e.preventDefault();
      e.stopPropagation();
      attachFile(f);
    }
  });
  composer.addEventListener('dragover', (e) => { if ([...(e.dataTransfer?.types ?? [])].includes('Files')) e.preventDefault(); });
  composer.addEventListener('drop', (e) => {
    const files = [...(e.dataTransfer?.files ?? [])].filter((f) => f.type.startsWith('image/'));
    if (!files.length) return;
    e.preventDefault();
    e.stopPropagation();
    files.forEach(attachFile);
  });
  log.addEventListener('click', onLogClick);
  q('amodelnote').addEventListener('click', (e) => {
    if (e.target.closest('[data-get-agents]')) openGetModels('agent');
  });
  q('amodel').addEventListener('change', async () => {
    syncModelNote();
    if (!data) return;
    try { show(await post('/chats/agent', { id: chatId(), agent: q('amodel').value }), { scroll: false }); } catch (e) { err(String(e.message || e)); }
  });
  q('atitle').addEventListener('change', async () => {
    if (!data) return;
    try {
      show(await post('/chats/rename', { id: chatId(), title: q('atitle').value }), { scroll: false });
      loadChats();
    } catch (e) { err(String(e.message || e)); }
  });
  onJobs(follow);
  queueChanged.subscribe(refresh);
  drawPanels();
  syncButtons();
  loadChats();

  const m = {
    pane,
    err,
    agentsLoaded,
    reveal(got) {
      panel.open = true;
      seq++;
      show(got);
    },
  };
  mounts[pane] = m;
  return m;
}

// Once, after both mounts: the agent list, and a chat opened from elsewhere.
export function initAssistants() {
  document.addEventListener('yasdig:openchat', (e) => openAnywhere(e.detail));
  loadAgents();
}
