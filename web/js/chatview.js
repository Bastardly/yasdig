import { esc } from './dom.js';
import { linesText } from './lines.js';

// ---- A chat as HTML ---------------------------------------------------------
//
// The Assistant's message list, drawn from the chat the server sent
// (`GET /chats/{id}`). Pure — strings in, a string out — so `chatview.test.js`
// can hold it to what it shows: every word the model wrote goes through `esc`,
// a change is a card with Undo while it holds,
// a round is a row of its pictures with what the agent made of each — one
// picture alone is drawn large.
//
// The buttons carry what they act on in `data-` attributes; `assistant.js` has one
// delegated listener for the lot.
//
//   chat   the server's chat (`agent::chat::Chat`)
//   live   the reply being written right now, or null

const KNOB_LABEL = {
  prompt: 'Prompt', negative: 'Negative prompt', cfg_scale: 'CFG', steps: 'Steps', size: 'Resolution', model: 'Model',
};

// A knob's value as one line.
export function valueText(key, v) {
  if (v == null) return '—';
  if (key === 'size') return `${v.width ?? '?'}×${v.height ?? '?'}`;
  if (typeof v === 'string') return v.length > 140 ? `“${v.slice(0, 140)}…”` : `“${v}”`;
  return String(v);
}

// What the model wrote, escaped, with its **bold** drawn: small models write
// Markdown whatever they are told, and the stars read as noise.
export function replyHtml(text) {
  return esc(text).replace(/\*\*([^*\n]+)\*\*/g, '<b>$1</b>');
}

const img = (chat, folder, file) => `/chats/${encodeURIComponent(chat.id)}/${folder}/${encodeURIComponent(file)}`;

function picture(chat, ref, { n, badges = '', tools = true } = {}) {
  const folder = ref.kind === 'attach' ? 'att' : 'img';
  const label = n ? `<span class="chatpicno">${n}</span>` : '';
  // A work item opens in Refine & adjust; an output or an upload only shows.
  const open = ref.kind === 'item' ? ` data-open-item="${esc(ref.id)}"` : '';
  // Ask attaches it to the next message, to ask anything about it.
  const ask = tools && (ref.kind === 'item' || ref.kind === 'output')
    ? `<button type="button" class="hint chatpictool" data-ask="${esc(ref.kind)}:${esc(ref.id)}"
        title="Attach this picture to your next message, to ask about it">Ask</button>` : '';
  return `<figure class="chatpic">
    <button type="button" class="chatpicimg"${open} title="${ref.kind === 'item' ? 'Open in Refine &amp; adjust' : ''}${
      ref.seed != null ? `${ref.kind === 'item' ? ' · ' : ''}seed ${ref.seed}` : ''}">
      <img src="${img(chat, folder, ref.file)}" alt="" loading="lazy"></button>
    ${label}${badges}${ask}</figure>`;
}

function changeCard(c) {
  const rows = Object.entries(c.set ?? {}).map(([k, v]) => `<li><b>${esc(KNOB_LABEL[k] ?? k)}</b>
    ${c.before && k in c.before ? `${esc(valueText(k, c.before[k]))} → ` : ''}${esc(valueText(k, v))}</li>`).join('');
  const state = {
    applied: `<button type="button" class="btn" data-undo="${c.id}" title="Put these settings back as they were">Undo</button>`,
    undone: '<span class="sub">Undone</span>',
    // An older chat's proposal nobody answered.
    rejected: '<span class="sub">Set aside</span>',
  }[c.state] ?? '';
  const who = c.by === 'user' ? 'You changed' : c.state === 'rejected' ? 'The agent asked to change' : 'The agent changed';
  return `<div class="chatcard">
    <p class="chatcardhead">${who}</p><ul class="chatchanges">${rows}</ul>
    ${c.reason ? `<p class="sub">${esc(c.reason)}</p>` : ''}
    ${(c.notes ?? []).map((n) => `<p class="sub">${esc(n)}</p>`).join('')}
    <div class="btnrow">${state}</div></div>`;
}

const PASS = { hires: 'Hi-res pass', refine: 'Refine pass', edit: 'Edit' };

function actionCard(chat, a) {
  const what = `${PASS[a.kind] ?? a.kind}${a.instruction ? `: ${esc(a.instruction)}` : ''}`;
  const state = {
    queued: '<span class="sub">Waiting its turn</span>',
    done: a.result ? picture(chat, a.result, { tools: false }) : '<span class="sub">Done</span>',
    rejected: '<span class="sub">Not run</span>',
    failed: `<span class="sub">Failed: ${esc(a.error ?? '')}</span>`,
  }[a.state] ?? '';
  return `<div class="chatcard">
    <p class="chatcardhead">${a.state === 'rejected' ? 'The agent asked to run' : 'The agent ran'} ${what}</p>
    <div class="btnrow">${state}</div></div>`;
}

// One answer row per picture: ✓/✗ for each check, in the checks' order.
function checkMarks(row) {
  return row ? `<span class="chatchecks">${row.results.map((r) => (r ? '✓' : '✗')).join('')}</span>` : '';
}

function batchPictures(chat, m, judged) {
  return m.images.map((ref, i) => {
    const n = i + 1;
    const row = judged?.checks?.find((c) => c.image === n);
    const kept = judged?.keepers?.includes(n);
    const passed = judged?.passed?.includes(n);
    const badges = checkMarks(row)
      + (kept ? '<span class="chatbadge keep" title="The agent picked it">Keeper</span>' : '')
      + (passed ? '<span class="chatbadge pass" title="Every check said yes">All checks</span>' : '');
    return picture(chat, ref, { n, badges });
  }).join('');
}

// A session the agent wants to run, waiting for the person: nothing is drawn
// until Start (`/agent/session`), and a new message drops it.
function proposalCard(p) {
  const what = p.sweep
    ? `Compare ${esc(KNOB_LABEL[p.sweep.knob] ?? p.sweep.knob)}: ${p.sweep.values.map((v) => esc(valueText(p.sweep.knob, v))).join(', ')}`
      + ` — ${p.per_round} picture${p.per_round === 1 ? '' : 's'} each, on the same seeds, every one judged.`
    : 'Draw rounds, judge each against yes/no checks and change the prompt or settings until the pictures are right.';
  return `<div class="chatcard">
    <p class="chatcardhead">${p.sweep ? 'Run this comparison?' : 'Run this session?'}</p>
    <p class="chatprompt">${esc(p.goal)}</p><p class="sub">${what}</p>
    <div class="btnrow">
      <button type="button" class="btn" data-proposal="start">Start</button>
      <button type="button" class="btn" data-proposal="no">No thanks</button>
    </div></div>`;
}

function agentMessage(chat, m) {
  const changes = (m.changes ?? []).map((id) => chat.changes.find((c) => c.id === id)).filter(Boolean);
  const action = m.action != null ? chat.actions.find((a) => a.id === m.action) : null;
  const describe = m.prompt ? `<div class="chatcard">
      <p class="chatcardhead">Prompt</p><p class="chatprompt">${esc(m.prompt)}</p>
      ${m.negative ? `<p class="sub">Negative: ${esc(m.negative)}</p>` : ''}
      <div class="btnrow">
        <button type="button" class="btn" data-copy-prompt="${m.n}">Copy to the Image tab</button>
      </div></div>` : '';
  const checks = m.mode === 'plan' && chat.session.checks.length ? `<ol class="chatchecklist">${
    chat.session.checks.map((c) => `<li>${esc(c)}</li>`).join('')}</ol>` : '';
  // A storyboard is long; the Story panel holds it.
  const board = m.mode === 'story' && chat.film ? `<div class="chatcard">
      <p class="chatcardhead">Storyboard${chat.film.title ? `: ${esc(chat.film.title)}` : ''}</p>
      <p class="sub">${chat.film.cast.length} in the cast · ${chat.film.shots.length} shot${chat.film.shots.length === 1 ? '' : 's'}</p>
      ${castPics(chat)}
      <div class="btnrow"><button type="button" class="btn" data-open-story>Open the storyboard</button></div></div>` : '';
  const notes = m.notes ? `<details class="chatnotes"><summary>Notes</summary><p>${esc(m.notes)}</p></details>` : '';
  const stats = m.stats?.n_gen ? `<span class="sub chatstats">${m.stats.n_prompt} read · ${m.stats.n_gen} written · ${
    ((m.stats.load_ms + m.stats.read_ms + m.stats.gen_ms) / 1000).toFixed(1)} s</span>` : '';
  return `<div class="chatmsg agent" data-n="${m.n}">
    ${m.reply ? `<p class="chattext">${replyHtml(m.reply)}</p>` : ''}
    ${checks}${describe}${board}${changes.map(changeCard).join('')}${action ? actionCard(chat, action) : ''}
    ${chat.session.proposal?.n === m.n ? proposalCard(chat.session.proposal) : ''}
    <div class="chatfoot">${notes}${stats}</div></div>`;
}

export function renderMessage(chat, m, i) {
  switch (m.role) {
    case 'user':
      return `<div class="chatmsg user" data-n="${m.n}">
        ${m.images?.length ? `<div class="chatpics">${m.images.map((r) => picture(chat, r)).join('')}</div>` : ''}
        ${m.text ? `<p class="chattext">${esc(m.text)}</p>` : ''}</div>`;
    case 'agent':
      return agentMessage(chat, m);
    case 'batch': {
      // The critique of this round is the next agent message after it.
      const judged = chat.messages.slice(i + 1).find((x) => x.role === 'agent' && x.mode === 'critique');
      const empty = !m.images.length ? '<p class="sub">Drawing…</p>' : '';
      return `<div class="chatmsg batch" data-n="${m.n}">
        <p class="chatcardhead">Round ${m.round}<span class="sub"> · ${esc(m.summary)}</span></p>
        <div class="chatpics${m.images.length === 1 ? ' large' : ''}">${batchPictures(chat, m, judged)}</div>${empty}</div>`;
    }
    case 'clip':
      return `<div class="chatmsg agent" data-n="${m.n}"><div class="chatcard">
        <p class="chatcardhead">The film${m.title ? `: ${esc(m.title)}` : ''}</p>
        <img class="chatclip" src="/videos/${encodeURIComponent(m.id)}/thumb.png" alt="">
        <div class="btnrow"><button type="button" class="btn" data-open-clip="${esc(m.id)}">Open in the Video tab</button></div>
      </div></div>`;
    case 'note':
      return `<p class="chatnote${m.text.includes('\n') ? ' table' : ''}" data-n="${m.n}">${esc(m.text)}</p>`;
    default:
      return '';
  }
}

export function renderChat(chat, live = null) {
  const msgs = chat.messages.map((m, i) => renderMessage(chat, m, i)).join('');
  const writing = live?.text != null
    ? `<div class="chatmsg agent live"><p class="chattext">${replyHtml(liveReply(live.text)) || '…'}</p></div>` : '';
  return msgs + writing;
}

// What to show of a reply still being written: it is JSON, and the person
// wants the `reply` field, so that is read out of the partial text as soon as
// it starts, and the notes stand in until then.
export function liveReply(text) {
  for (const key of ['reply', 'notes', 'prompt']) {
    const at = text.indexOf(`"${key}"`);
    if (at < 0) continue;
    const open = text.indexOf('"', text.indexOf(':', at) + 1);
    if (open < 0) continue;
    let out = '';
    for (let i = open + 1; i < text.length; i++) {
      const c = text[i];
      if (c === '\\') { out += { n: '\n', t: '\t', '"': '"', '\\': '\\' }[text[i + 1]] ?? ''; i++; continue; }
      if (c === '"') return out;
      out += c;
    }
    return out;
  }
  return '';
}

// The Session panel: the goal, what a comparison draws, the picture to
// match, the checks and where it is.
export function sessionHtml(chat) {
  const s = chat.session;
  if (!s.goal) {
    return '<p class="sub">No session yet. Ask the agent to get something right or improve a round, and it offers one; once you press Start it writes checks, draws a round, judges it and tries again, as many rounds as you ask for (three when you don’t say). Ask it to find something out (“which CFG works best?”) and it offers a comparison: one round per value on the same seeds, then counts which passed.</p>';
  }
  const sw = s.sweep;
  const sweep = sw ? `<p><b>Comparing ${esc(KNOB_LABEL[sw.knob] ?? sw.knob)}</b>: ${
    sw.values.map((v) => esc(valueText(sw.knob, v))).join(' · ')}</p>` : '';
  const refs = (s.reference ?? []).map((r) => `<img class="chatref" src="${
    img(chat, r.kind === 'attach' ? 'att' : 'img', r.file)}" alt="The picture to match" title="The picture to match">`).join('');
  return `<p><b>Goal</b>: ${esc(s.goal)}</p>${sweep}
    ${refs ? `<p class="sub">To match</p><div class="chatrefs">${refs}</div>` : ''}
    ${s.checks.length ? `<p class="sub">Checks</p><ol class="chatchecklist">${s.checks.map((c) => `<li>${esc(c)}</li>`).join('')}</ol>` : ''}
    <p class="sub">Round ${sessionRound(s)} of ${sessionLimit(s)} · ${esc(stateText(chat) || 'idle')}</p>`;
}

// ---- The Story panel ------------------------------------------------------------
//
// A film's storyboard (`agent::chat::Film`): its look, the cast with their
// pictures, the shots. Every field is the person's to edit until the film is
// shot; an edit is posted as it is made (`assistant.js`, `/chats/film`), and a
// picture dropped or a keyframe's shot reworded is drawn again by the agent.
// `locked` while the agent works on the chat.

const KIND = { character: 'Character', place: 'Place', thing: 'Thing' };
const PHASE = { board: 'Drawing its pictures', ready: 'Ready for the Video tab' };

const thumb = (chat, ref, alt) => `<img src="${img(chat, ref.kind === 'attach' ? 'att' : 'img', ref.file)}" alt="${esc(alt)}">`;

// Whether member `m` gets a turnaround sheet (`Film::wants_sheet`).
const wantsSheet = (f, m) => f.sheets && m.kind === 'character';

function memberCard(chat, m, k, lock) {
  const theirs = m.picture && m.picture.kind !== 'item';
  const sheet = m.sheet ? `<div class="storysheet" title="${esc(m.name)} from every side">${thumb(chat, m.sheet, `${m.name}, turnaround`)}</div>`
    : '';
  const pic = m.picture ? thumb(chat, m.picture, m.name)
    : `<span class="sub">${chat.film.phase === 'board' ? 'Drawn next' : 'No picture'}</span>`;
  return `<div class="storymember">
    <div class="storypic" title="${theirs ? 'Your picture' : 'Drawn by the agent'}">${pic}</div>
    <div class="storyfields">
      <input type="text" value="${esc(m.name)}" data-film-member="${k}" data-field="name" aria-label="Name"${lock}>
      <span class="sub">${esc(KIND[m.kind] ?? m.kind)}</span>
      <textarea rows="2" data-film-member="${k}" data-field="look" aria-label="How ${esc(m.name)} looks"${lock}>${esc(m.look)}</textarea>
      <div class="btnrow">
        <button type="button" class="btn" data-film-pick="${k}" title="Use a picture from Outputs"${lock}>Choose…</button>
        <button type="button" class="btn" data-film-upload="${k}" title="Use a picture from this computer"${lock}>Upload…</button>
        <button type="button" class="btn" data-film-redraw="${k}" title="Let the agent draw this one from its look"${lock}>${
          theirs ? 'Draw one' : 'Draw again'}</button>
      </div>${voiceRow(chat.film, m, k, lock)}
    </div>${sheet}</div>`;
}

// On H3, a character's voice: a sound H3 follows on the shots that start
// from the cast's pictures (a continue can't take one).
function voiceRow(f, m, k, lock) {
  if (!f.references || m.kind !== 'character') return '';
  const has = !!m.voice;
  return `<div class="btnrow storyvoice" title="A voice ${esc(m.name)} speaks in, on the shots that start fresh from the cast">
      <span class="sub">Voice: ${has ? esc(m.voice) : 'H3 picks one'}</span>
      ${has ? `<button type="button" class="btn" data-film-voiceplay="${esc(m.voice)}">Play</button>` : ''}
      <button type="button" class="btn" data-film-voicepick="${k}"${lock}>Choose…</button>
      <button type="button" class="btn" data-film-voiceup="${k}"${lock}>Upload…</button>
      ${has ? `<button type="button" class="btn" data-film-novoice="${k}"${lock}>Remove</button>` : ''}
    </div>`;
}

// The pictures shot k's keyframe is drawn with, as `Film::cast_marks`.
function castMarks(f, s) {
  return s.cast.map((n) => {
    const m = f.cast.find((c) => c.name === n);
    return m ? `${m.stem ?? ''}|${m.sheet_stem ?? ''}` : '';
  });
}

// Shot k has no keyframe to start from: none drawn, or one drawn with a
// cast picture that has changed since (`Film::keyframe_missing`).
export function keyMissing(f, s, k) {
  if (f.references || s.no_key || !(k === 0 || s.cut)) return false;
  return !s.key_stem || (Array.isArray(s.key_cast) && s.key_cast.join('\n') !== castMarks(f, s).join('\n'));
}

// A shot whose first frame is stale: drawn with a cast picture since changed.
function shotState(f, s, k) {
  return s.key_stem && keyMissing(f, s, k) ? '<span class="sub">Its cast changed: drawn again</span>' : '';
}

// A shot's first frame: a click draws it again (after a confirm) while
// the board can change; removed, the shot starts from its words.
function keyColumn(chat, s, k, lock) {
  const f = chat.film;
  if (s.no_key && !f.references && (k === 0 || s.cut)) {
    return `<div class="storykey"><span class="sub">Starts from its words</span>${lock ? ''
      : `<button type="button" class="btn" data-film-addkey="${k}">Draw a first frame</button>`}</div>`;
  }
  if (!s.keyframe) return '';
  const pic = thumb(chat, s.keyframe, `Shot ${k + 1}`);
  if (lock) return `<div class="storykey" title="The picture shot ${k + 1} starts from">${pic}</div>`;
  return `<div class="storykey"><button type="button" class="storykeypic" data-film-key="${k}" title="The picture shot ${
    k + 1} starts from — click to draw it again">${pic}</button><button type="button" class="btn" data-film-nokey="${
    k}" title="Start shot ${k + 1} from its words">Remove first frame</button></div>`;
}

function shotRow(chat, s, k, lock) {
  const how = k === 0 ? '<span class="sub">First shot</span>'
    : `<select data-film-shot="${k}" data-field="cut" aria-label="How shot ${k + 1} begins"${lock}>
        <option value="0"${s.cut ? '' : ' selected'}>Continue</option><option value="1"${s.cut ? ' selected' : ''}>Cut</option></select>`;
  const key = keyColumn(chat, s, k, lock);
  return `<li class="storyshot">
    <div class="storyshothead"><b>${k + 1}</b>${how}${shotState(chat.film, s, k)}</div>
    <div class="storyshotbody">${key}<div class="storyfields">
      <textarea rows="2" data-film-shot="${k}" data-field="text" aria-label="Shot ${k + 1}"${lock}>${esc(s.text)}</textarea>
      <input type="text" value="${esc(s.cast.join(', '))}" data-film-shot="${k}" data-field="cast"
        placeholder="Who is in it" aria-label="Who is in shot ${k + 1}"${lock}>${chat.film.references ? soundFields(s, k, lock) : ''}
    </div></div></li>`;
}

// H3 films sound: what is said, what is heard, and the score — written into
// its prompt by the server (`h3prompt.rs`).
function soundFields(s, k, lock) {
  return `
      <textarea rows="2" data-film-shot="${k}" data-field="lines" aria-label="Spoken in shot ${k + 1}"
        placeholder="Spoken, one line each — Mara: Who's there?  Leo (voiceover): I never left."${lock}>${esc(linesText(s.lines))}</textarea>
      <input type="text" value="${esc(s.sound ?? '')}" data-film-shot="${k}" data-field="sound"
        placeholder="Sound: rain on the roof, footsteps — empty is silence" aria-label="Sound in shot ${k + 1}"${lock}>
      <input type="text" value="${esc(s.music ?? '')}" data-film-shot="${k}" data-field="music"
        placeholder="Music only the audience hears — empty is none" aria-label="Music in shot ${k + 1}"${lock}>`;
}

// The cast's pictures in a row, named, for the storyboard's card.
function castPics(chat) {
  // A character's turnaround says more than its front picture.
  const drawn = chat.film.cast.filter((m) => m.sheet || m.picture);
  return drawn.length ? `<div class="chatpics">${drawn.map((m) => `<figure class="chatpic" title="${esc(m.name)}">${
    thumb(chat, m.sheet ?? m.picture, m.name)}<span class="chatpicno">${esc(m.name)}</span></figure>`).join('')}</div>` : '';
}

// The Story panel's main action, `{ label, body }` for `/chats/film`, or
// null without a board: its shots sent to the Video tab (`{send: true}`),
// where the clip is made — whenever the board changes, with whatever
// pictures it has. Drawing them is the panel's other buttons, and optional.
export function mainAction(film) {
  return film ? { label: 'Send to Video tab', body: { send: true } } : null;
}

// *Draw the cast*, under the cast, while one of them has no picture (or a
// character on a references model no turnaround); null once they all do.
export function castLabel(film) {
  if (!film) return null;
  const done = (m) => m.stem && (!wantsSheet(film, m) || m.sheet_stem);
  if (film.cast.every(done)) return null;
  return film.cast.some(done) ? 'Draw the rest of the cast' : 'Draw the cast';
}

// *Draw the first frames*, above Send, while a shot that starts from one
// has none or a stale one. It draws any of the cast still missing first.
export function framesLabel(film) {
  return film?.shots.some((s, k) => keyMissing(film, s, k)) ? 'Draw the first frames' : null;
}

//   models  the toolkit's runnable models, for the board's image model
export function storyHtml(chat, { busy = false, off = null, models = [] } = {}) {
  const f = chat.film;
  if (!f && off) return `<p class="sub">No film can be made in this chat now: ${esc(off)}.</p>`;
  if (!f) {
    return '<p class="sub">No film yet. Tell the agent a story, an idea or who is in it, and ask for a film: it writes a storyboard for the Video tab’s model, and draws the cast and the pictures the shots start from when you press for them. Send to Video tab puts the shots there, where you generate the clip.</p>';
  }
  const editable = !busy;
  const lock = editable ? '' : ' disabled';
  const secs = f.shots.length;
  return `<p>${f.title ? `<b>${esc(f.title)}</b> · ` : ''}<span class="sub">${esc(PHASE[f.phase] ?? f.phase)} · ${
      secs} shot${secs === 1 ? '' : 's'} for ${esc(f.video_model)}</span></p>
    <label class="grouphead">Every shot</label>
    <textarea rows="2" data-film-every aria-label="Every shot"${lock}>${esc(f.every)}</textarea>
    ${pictureModel(f, models, lock, chat.settings?.template)}
    <label class="grouphead">Cast</label>
    ${f.cast.length ? `<div class="storycast">${f.cast.map((m, k) => memberCard(chat, m, k, lock)).join('')}</div>`
      : '<p class="sub">No one the shots must keep the same.</p>'}
    ${castLabel(f) && editable ? `<div class="btnrow"><button type="button" class="btn" data-film-drawcast
      title="A picture of each in the cast, from their look: what the first frames are drawn with, and on H3 what the shots follow. Optional">${
      esc(castLabel(f))}</button></div>` : ''}
    <label class="grouphead">Shots</label>
    <ol class="storyshots">${f.shots.map((s, k) => shotRow(chat, s, k, lock)).join('')}</ol>`;
}

// The model the cast and keyframes are drawn with. One that follows
// reference pictures gives characters a turnaround sheet.
function pictureModel(f, models, lock, chatModel) {
  if (!models.length) return '';
  // Empty is the chat's own (a board from before boards had a model).
  const name = f.image_model || chatModel;
  const opts = models.map((m) => `<option value="${esc(m.name)}"${m.name === name ? ' selected' : ''}>${esc(m.name)} · ${
    esc(m.label)}${m.references ? ' (turnarounds)' : ''}</option>`);
  if (name && !models.some((m) => m.name === name)) opts.unshift(`<option selected>${esc(name)} (not installed)</option>`);
  return `<label class="grouphead" for="astorymodel">Pictures drawn with</label>
    <select id="astorymodel" data-film-model${lock}>${opts.join('')}</select>
    <p class="sub">${f.sheets ? 'Characters get a turnaround sheet — front, three-quarter, side, back — and a front picture drawn from it.'
      : 'This model can’t follow a reference picture, so each character gets one picture. A Klein can draw turnarounds.'}</p>`;
}

// How many of its rounds the session has drawn: round numbers count on
// through the whole chat, a session from where it began.
export function sessionRound(s) {
  return Math.max(0, s.round - (s.first_round ?? 0));
}

// The rounds the session may draw: the plan's, else three (`session::SESSION_ROUNDS`).
export function sessionLimit(s) {
  return s.rounds || 3;
}

// The header's line: what the agent is doing, or waiting for.
export function stateText(chat) {
  const s = chat.session;
  const f = chat.film;
  if (s.state === 'running' && f && s.next === 'film') {
    return f.cast.some((m) => !m.stem) ? 'Drawing the cast' : 'Drawing the first frames';
  }
  if (s.state === 'running' && s.next === 'turn' && s.mode === 'story') return 'Writing the storyboard';
  const round = s.goal ? ` · round ${sessionRound(s)} of ${sessionLimit(s)}` : '';
  return {
    idle: '', queued: 'Waiting for the GPU', running: `Working${round}`,
    done: `Done${round}`, stopped: 'Stopped', failed: 'Stopped on an error', interrupted: 'Interrupted',
  }[s.state] ?? '';
}
