import { $, post } from './dom.js';
import { soonJobs } from './jobs.js';
import { castHtml, castLabel, framesLabel, rowKeyHtml } from './chatview.js';
import { drawShots, freshStage, rowsNow, setBoard, stageClipById } from './video.js';
import { choosePicture, chooseSound, upload } from './source-slot.js';
import { askForSound, playSound } from './sounds.js';

// ---- A film's storyboard, on the Video tab ------------------------------------------
//
// A Video chat's film (`agent::chat::Film`) is not a copy the person sends
// somewhere: its shots are the Shots rows and its cast is the Cast panel,
// while its chat is open in the Assistant panel — *bound*. Both ways:
//
// - film → rows: the chat comes with `board` (`session::board_rows`), the rows
//   as the Shots panel draws them, the cast's pictures and voices filled in.
//   When it changes, the rows are drawn from it — not while the person is
//   typing in one; then once they leave it.
// - rows → film: an edit of the rows or Every shot is written back
//   (`/chats/board`, debounced and only when it differs from what was last
//   written), as the Image tab's controls are into an Image chat. A press that
//   drew onto the rows writes its seeds back, and names the clip.
//
// Generate clip, Extend and Redo are the Video tab's own: the rows are marked
// against the film's clip like any clip's. Opening another clip, or New clip,
// leaves the board (the rows are that clip's); *Back to the storyboard* comes
// back. While the agent works on the chat the rows are locked: its rewrite
// would overtake an edit made meanwhile.

let got = null; // the Video Assistant's chat shown, `{ chat, toolkit, board }`
let show = null; // the Assistant's `show`, for an answer that is the chat
let bound = false;
let applied = null; // the board last drawn into the rows, as JSON
let sent = null; // the rows last written into the film, as JSON
let seenModel = null; // the film's video model last put in #vmodel
let ownClip = null; // a clip just landed on the rows, before the film names it
let stale = false; // a redraw waiting for the person to leave a box
let timer = null;
let pickFor = null; // the cast member an upload is for
let warning = '';

const film = () => got?.chat.film ?? null;
const busy = () => ['queued', 'running'].includes(got?.chat.session.state);
const chatId = () => got?.chat.id ?? null;
const err = (msg) => { $('casterr').textContent = msg ?? ''; };

// What the rows hold, for `/chats/board` (`session::RowIn`).
const payload = () => ({ every: $('veverytext').value, shots: rowsNow() });

// The person is in a box of the Shots panel: a redraw would take it away.
const typing = (root) => root.contains(document.activeElement) && document.activeElement.matches('input, textarea');

const HOOKS = {
  edited() {
    if (!bound || busy()) return;
    clearTimeout(timer);
    timer = setTimeout(flush, 600);
  },
  // A press drew onto the rows: their seeds, and the clip, into the film.
  landed(c) {
    if (!bound) return;
    ownClip = c.id;
    flush({ clip: c.id });
  },
  flush() {
    if (bound) flush();
  },
  left() {
    if (!bound) return;
    bound = false;
    setBoard(null);
    lock();
    drawLine();
  },
  isBoardClip: (id) => bound && !!id && (id === ownClip || id === film()?.clip),
  rowExtras: (k, refs) => (k >= 0 ? rowKeyHtml(got?.board?.shots?.[k], k, refs) : ''),
  framesLabel: () => (bound && !busy() ? framesLabel(film()) : null),
};

// The rows into the film, now. Read before anything else changes them, so a
// leave can call it just before the stage takes another clip's rows.
async function flush(extra = {}) {
  clearTimeout(timer);
  timer = null;
  if (!bound || !got || busy()) return;
  const body = payload();
  const json = JSON.stringify(body);
  if (json === sent && !extra.clip) return;
  sent = json;
  try {
    const g = await post('/chats/board', { id: chatId(), ...body, ...extra });
    warning = g.warning ?? '';
    show?.(g);
  } catch (e) {
    $('verr').textContent = String(e.message || e);
  }
}

// The Video Assistant's chat, as it was just shown (or null). `showFn` is how
// an answer that is the chat reaches the panel again.
export function follow(g, showFn) {
  show = showFn;
  const was = got;
  got = g && g.chat.context === 'video' ? g : null;
  drawCast();
  const f = film();
  if (!f) {
    if (bound) HOOKS.left();
    drawLine();
    return;
  }
  // A film just written, or a chat with one opened: the rows and the cast.
  if (got.chat.id !== was?.chat.id || !was?.chat.film) {
    bind(true);
    return;
  }
  if (bound) {
    redraw();
    // The marks and the footer read the board too: a cast just drawn moves
    // *Draw the first frames* though no row changed.
    setBoard(HOOKS);
  }
  lock();
  drawLine();
}

// The board onto the rows: its model first, its clip on the stage (or a new
// one), then its shots.
async function bind(open = false) {
  const id = chatId();
  const f = film();
  if (!f) return;
  bound = true;
  applied = null;
  ownClip = null;
  seenModel = null;
  setBoard(HOOKS);
  modelFrom(f);
  const staged = f.clip ? await stageClipById(f.clip) : false;
  if (chatId() !== id || !bound) return;
  if (!staged) freshStage();
  redraw(true);
  lock();
  drawLine();
  if (open) {
    $('vfold-prompt').open = true;
    $('panel-vid-cast').open = true;
  }
}

// The film's video model into #vmodel when it moved (the agent picked one,
// or the board was opened): the board is written for it.
function modelFrom(f) {
  if (!f.video_model || f.video_model === seenModel) return;
  seenModel = f.video_model;
  const sel = $('vmodel');
  if (sel.value === f.video_model || ![...sel.options].some((o) => o.value === f.video_model)) return;
  sel.value = f.video_model;
  sel.dispatchEvent(new Event('change', { bubbles: true }));
}

function redraw(force = false) {
  const b = got?.board;
  if (!b) return;
  modelFrom(film());
  const json = JSON.stringify(b);
  if (!force && json === applied) {
    note();
    return;
  }
  if (typing($('vfold-prompt'))) {
    stale = true;
    return;
  }
  stale = false;
  applied = json;
  drawShots(b.shots);
  $('veverytext').value = b.every ?? '';
  sent = JSON.stringify(payload());
  note();
}

function note() {
  $('vsentnote').textContent = [got?.board?.note, warning].filter(Boolean).join(' ');
}

// The agent is working on the chat: the rows wait for it.
function lock() {
  const on = bound && busy();
  for (const id of ['vshots', 'vshotadd', 'veverytext']) $(id).inert = on;
}

// The Shots panel's first line: whose rows these are, or the way back.
function drawLine() {
  const f = film();
  const line = $('vboardline');
  line.hidden = !f;
  if (!f) return;
  const title = f.title ? `“${f.title}”` : 'the storyboard';
  $('vboardtext').textContent = bound
    ? `These shots are ${title}, from the Assistant’s chat: what you change here, it sees.${busy() ? ' The agent is working on it; the shots open again when it is done.' : ''}`
    : `The Assistant’s chat has ${title}.`;
  $('vboardback').hidden = bound;
}

// ---- The Cast panel -------------------------------------------------------------

function drawCast() {
  const f = film();
  const label = !busy() && castLabel(f);
  $('castdraw').hidden = !label;
  if (label) $('castdraw').textContent = label;
  const body = $('castbody');
  if (typing(body)) {
    body.dataset.stale = '1';
    return;
  }
  delete body.dataset.stale;
  body.innerHTML = castHtml(got?.chat ?? null, { busy: busy(), off: got?.toolkit.film_off, models: got?.toolkit.models ?? [] });
}

// One edit of the board (`/chats/film`): the cast, a draw press. Pending row
// edits go first, so the press works on the rows as they are.
async function edit(fields) {
  if (!got) return;
  err('');
  await flush();
  try {
    show?.(await post('/chats/film', { id: chatId(), ...fields }));
    soonJobs();
  } catch (e) {
    err(String(e.message || e));
    drawCast();
  }
}

// Before a message is sent: an edit still waiting goes first, or the turn
// (which locks the rows) would overtake it.
export async function flushBoard() {
  await flush();
}

// From the chat's storyboard card: back onto the rows, the panels open.
export function showBoard() {
  if (!film()) return;
  if (!bound) bind(true);
  else {
    $('vfold-prompt').open = true;
    $('panel-vid-cast').open = true;
  }
}

export function initBoard() {
  const body = $('castbody');
  body.addEventListener('change', (e) => {
    const t = e.target;
    if (t.matches('[data-film-model]')) edit({ image_model: t.value });
    else if (t.dataset.filmMember != null) edit({ member: Number(t.dataset.filmMember), [t.dataset.field]: t.value });
  });
  body.addEventListener('focusout', () => {
    if (body.dataset.stale) setTimeout(drawCast, 0);
  });
  body.addEventListener('click', (e) => {
    const b = e.target.closest('button');
    if (!b) return;
    const d = b.dataset;
    const picture = (k) => (stem) => edit({ member: k, picture: { kind: 'output', id: stem } });
    if (d.filmPick != null) choosePicture(picture(Number(d.filmPick)));
    // Drawn on the same press, as a person pressing it means.
    else if (d.filmRedraw != null) edit({ member: Number(d.filmRedraw), picture: null, draw: true });
    else if (d.filmUpload != null) {
      pickFor = Number(d.filmUpload);
      $('castfile').click();
    } else if (d.filmVoiceplay != null) playSound(d.filmVoiceplay);
    else if (d.filmVoicepick != null) {
      const k = Number(d.filmVoicepick);
      chooseSound((stem) => edit({ member: k, voice: stem }));
    } else if (d.filmVoiceup != null) {
      const k = Number(d.filmVoiceup);
      askForSound((s) => edit({ member: k, voice: s.stem }), err);
    } else if (d.filmNovoice != null) edit({ member: Number(d.filmNovoice), voice: null });
  });
  $('castfile').addEventListener('change', async () => {
    const f = $('castfile').files[0];
    $('castfile').value = '';
    if (!f || pickFor == null) return;
    try {
      await edit({ member: pickFor, picture: { kind: 'output', id: await upload(f) } });
    } catch (ex) {
      err(String(ex.message || ex));
    }
  });
  $('castdraw').addEventListener('click', () => edit({ draw: true, frames: false }));
  // The rows' first-frame buttons (`rowKeyHtml`) and the Shots footer's.
  $('vshots').addEventListener('click', (e) => {
    const b = e.target.closest('[data-board-redraw], [data-board-addkey]');
    if (!b || !bound) return;
    if (b.dataset.boardRedraw != null) {
      const k = Number(b.dataset.boardRedraw);
      if (confirm(`Draw shot ${k + 1}'s first frame again? The one there now is replaced.`)) {
        edit({ shot: k, redraw_keyframe: true, draw: true });
      }
    } else edit({ shot: Number(b.dataset.boardAddkey), no_keyframe: false, draw: true });
  });
  $('vframes').addEventListener('click', () => edit({ draw: true, frames: true }));
  $('vboardback').addEventListener('click', () => bind(true));
  $('vfold-prompt').addEventListener('focusout', () => {
    if (stale) setTimeout(() => redraw(), 0);
  });
  // The tab's model is the one the board is shot on: H3 follows the cast's
  // pictures, the rest start a cut from a first frame.
  $('vmodel').addEventListener('change', async () => {
    if (!bound || busy() || !$('vmodel').value || $('vmodel').value === film()?.video_model) return;
    seenModel = $('vmodel').value;
    try {
      show?.(await post('/chats/board', { id: chatId(), video: $('vmodel').value }));
    } catch (e) {
      $('verr').textContent = String(e.message || e);
    }
  });
}
