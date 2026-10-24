import { $, esc, getJSON } from './dom.js';
import { wireHints } from './hints.js';

// ---- Settings → Agent: what the agent may do ------------------------------------
//
// The `[agent]` table (`agent::knobs::Rules`): one whitelist for every chat — a
// checkbox per setting it may change and per pass it may run — and the agent a
// new chat starts with. The rows come from `GET /agents` (the server's knob
// table), so the page invents none of them; until that answers, the stored
// value is carried through unchanged, as `kind: 'vars'` carries its rows.
//
// `drawRules` writes the markup, `readRules` reads it back in the server's own
// spelling (keys in order, sets sorted), so a save that changed nothing compares
// equal in Settings' dirty check.

let table = null; // `/agents`: { knobs, actions, models, max_batch, max_rounds }
let loading = null;

// The table, fetched once. Never rejects: without it the stored value is
// carried through.
export function loadRuleTable() {
  loading ??= getJSON('/agents').then((t) => { table = t; }, () => {});
  return loading;
}

export function drawRules(el, rules) {
  el.dataset.value = JSON.stringify(rules ?? null);
  if (!table) {
    el.innerHTML = '<p class="sub">What the agent may change couldn’t be read; it is kept as it is.</p>';
    return;
  }
  const r = rules ?? { knobs: [], actions: [] };
  const box = (attr, key, label, help, on) => `<div class="checks"><label data-help="${esc(help)}">`
    + `<input type="checkbox" ${attr}="${esc(key)}"${on ? ' checked' : ''}> ${esc(label)}</label></div>`;
  const knobs = table.knobs.map((k) => box('data-rule-knob', k.key, k.label, k.help, r.knobs?.includes(k.key))).join('');
  const passes = table.actions.map((a) => box('data-rule-action', a.key, a.label, a.help, r.actions?.includes(a.key))).join('');
  const models = table.models ?? [];
  const known = !r.agent || models.some((m) => m.name === r.agent);
  const options = [`<option value="">The first installed</option>`]
    .concat(models.map((m) => `<option value="${esc(m.name)}">${esc(m.label)}${m.vision ? '' : ' (text only)'}</option>`))
    .concat(known ? [] : [`<option value="${esc(r.agent)}">${esc(r.agent)} (not installed)</option>`]);
  el.innerHTML = `<label class="grouphead" data-help="agentknobs">Settings it may change</label>${knobs}
    <label class="grouphead" data-help="agentpasses">Passes it may run on a keeper</label>${passes}
    <label for="set-agent-default" data-help="setagent">Agent for new chats</label>
    <select id="set-agent-default">${options.join('')}</select>
    <p class="sub">It decides how many pictures and rounds from what you write, up to ${
      table.max_batch} pictures a round and ${table.max_rounds} rounds in a session.</p>`;
  $('set-agent-default').value = r.agent ?? '';
  wireHints(el);
}

export function readRules(el) {
  if (!table || !el.querySelector('[data-rule-knob]')) return JSON.parse(el.dataset.value || 'null') ?? undefined;
  const ticked = (attr) => [...el.querySelectorAll(`[${attr}]`)].filter((c) => c.checked)
    .map((c) => c.getAttribute(attr)).sort();
  const out = {};
  const agent = $('set-agent-default').value;
  if (agent) out.agent = agent;
  out.knobs = ticked('data-rule-knob');
  out.actions = ticked('data-rule-action');
  return out;
}
