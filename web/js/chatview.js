import { esc } from './dom.js';

// ---- A chat as HTML ---------------------------------------------------------
//
// The Agent tab's message list, drawn from the chat the server sent
// (`GET /chats/{id}`). Pure — strings in, a string out — so `chatview.test.js`
// can hold it to what it shows: every word the model wrote goes through `esc`,
// a change is a card with Undo while it holds,
// a round is a row of its pictures with what the agent made of each — one
// picture alone is drawn large.
//
// The buttons carry what they act on in `data-` attributes; `agent.js` has one
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
  const notes = m.notes ? `<details class="chatnotes"><summary>Notes</summary><p>${esc(m.notes)}</p></details>` : '';
  const stats = m.stats?.n_gen ? `<span class="sub chatstats">${m.stats.n_prompt} read · ${m.stats.n_gen} written · ${
    ((m.stats.load_ms + m.stats.read_ms + m.stats.gen_ms) / 1000).toFixed(1)} s</span>` : '';
  return `<div class="chatmsg agent" data-n="${m.n}">
    ${m.reply ? `<p class="chattext">${replyHtml(m.reply)}</p>` : ''}
    ${checks}${describe}${changes.map(changeCard).join('')}${action ? actionCard(chat, action) : ''}
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
    return '<p class="sub">No session yet. Ask the agent to get something right or improve a round, and it runs one: it writes checks, draws a round, judges it and tries again, as many rounds as you ask for (three when you don’t say). Ask it to find something out (“which CFG works best?”) and it compares: one round per value on the same seeds, then counts which passed.</p>';
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
  const round = s.goal ? ` · round ${sessionRound(s)} of ${sessionLimit(s)}` : '';
  return {
    idle: '', queued: 'Waiting for the GPU', running: `Working${round}`,
    done: `Done${round}`, stopped: 'Stopped', failed: 'Stopped on an error', interrupted: 'Interrupted',
  }[s.state] ?? '';
}
