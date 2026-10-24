import { esc } from './dom.js';

// ---- The Permissions panel ----------------------------------------------------
//
// One row per knob the agent could change, from the toolkit the server sent
// (`agent::toolkit`) — so the rows, their bounds and their locks are the
// server's, and the page invents none of them. Each row: Off / Ask first /
// Auto, the person's bounds inside the hard ones, and for the model the list
// it may pick from. A row the chat's model locks says why instead.
//
// `drawPerms` writes the markup; `readPerms` reads it back as the
// `Permissions` object `/chats/permissions` and `/settings {agent}` take.

const PERMS = [['off', 'Off'], ['ask', 'Ask first'], ['auto', 'Auto']];

const select = (name, v) => `<select data-perm="${esc(name)}">${PERMS.map(([p, l]) =>
  `<option value="${p}"${p === v ? ' selected' : ''}>${l}</option>`).join('')}</select>`;

function bounds(k) {
  if (!['int', 'float', 'size'].includes(k.kind)) return '';
  const step = k.kind === 'float' ? '0.1' : k.kind === 'size' ? '64' : '1';
  const box = (which, v) => `<input type="number" data-bound="${which}" step="${step}"
    min="${k.hard_min}" max="${k.hard_max}" value="${v ?? ''}" aria-label="${which === 'min' ? 'Lowest' : 'Highest'} ${esc(k.label)}">`;
  return `<span class="permbounds">${box('min', k.min)}<span class="sub">to</span>${box('max', k.max)}</span>`;
}

function allowed(k, models) {
  if (k.kind !== 'model') return '';
  if (!models.length) return '<p class="sub">No models are installed.</p>';
  return `<div class="permmodels">${models.map((m) => `<label><input type="checkbox" data-allow="${esc(m.name)}"${
    k.allowed.includes(m.name) ? ' checked' : ''}> ${esc(m.name)} <span class="sub">${esc(m.label)}</span></label>`).join('')}</div>`;
}

export function drawPerms(el, tk) {
  const knobs = tk.knobs.map((k) => `<div class="permrow" data-knob="${esc(k.key)}">
      <label data-help="${esc(k.help)}">${esc(k.label)}</label>${select(k.key, k.perm)}
      ${bounds(k)}${allowed(k, tk.models)}
      ${k.locked && k.kind !== 'model' ? `<p class="sub">Locked: ${esc(k.locked)}</p>` : ''}</div>`).join('');
  const actions = tk.actions.map((a) => `<div class="permrow" data-action="${esc(a.key)}">
      <label data-help="${esc(a.help)}">${esc(a.label)}</label>${select(a.key, a.perm)}
      ${a.unavailable ? `<p class="sub">${esc(a.unavailable)}</p>` : ''}</div>`).join('');
  el.innerHTML = `<label class="grouphead" data-help="agentknobs">Settings it may change</label>${knobs}
    <label class="grouphead" data-help="agentpasses">Passes it may run on a keeper</label>${actions}
    <label class="grouphead">Rounds</label>
    <div class="checks"><label data-help="agentseedlock"><input type="checkbox" data-opt="seed_lock"${
      tk.seed_lock ? ' checked' : ''}> Same seeds every round</label></div>
    <label data-help="agentbatch">Pictures per round</label>
    <input type="number" data-opt="batch" min="1" max="8" value="${tk.batch}">
    <label data-help="agentrounds">Rounds at most</label>
    <input type="number" data-opt="rounds" min="1" max="10" value="${tk.rounds}">
    <label data-help="agentmaxchanges">Changes per reply</label>
    <input type="number" data-opt="max_changes" min="1" max="2" value="${tk.max_changes}">`;
}

export function readPerms(el) {
  const num = (input) => {
    const v = parseFloat(input?.value);
    return Number.isFinite(v) ? v : null;
  };
  const knobs = {};
  for (const row of el.querySelectorAll('[data-knob]')) {
    const k = { perm: row.querySelector('[data-perm]').value };
    const lo = num(row.querySelector('[data-bound="min"]'));
    const hi = num(row.querySelector('[data-bound="max"]'));
    if (lo != null) k.min = lo;
    if (hi != null) k.max = hi;
    const allow = [...row.querySelectorAll('[data-allow]')].filter((c) => c.checked).map((c) => c.dataset.allow);
    if (allow.length) k.allowed = allow;
    knobs[row.dataset.knob] = k;
  }
  const actions = {};
  for (const row of el.querySelectorAll('[data-action]')) actions[row.dataset.action] = row.querySelector('[data-perm]').value;
  const opt = (name) => el.querySelector(`[data-opt="${name}"]`);
  const int = (name, lo, hi, d) => Math.min(hi, Math.max(lo, Math.round(num(opt(name)) ?? d)));
  return {
    seed_lock: opt('seed_lock').checked,
    batch: int('batch', 1, 8, 4),
    rounds: int('rounds', 1, 10, 3),
    max_changes: int('max_changes', 1, 2, 2),
    actions,
    knobs,
  };
}
