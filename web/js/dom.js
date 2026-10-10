// The four things every other module needs and nothing else does: how to find an
// element, how to escape text into one, how to ask the server something, and how
// to read a number out of a box that may be empty. No state, no imports.

// Every control on the page has a unique id and is addressed by it — see the
// module map in CLAUDE.md for why there are no shadow roots to hide one.
export const $ = (id) => document.getElementById(id);

// The grid renders user-supplied prompts into attributes, which nothing else on
// this page does — every other interpolation is a number from the server.
export const esc = (s) => String(s ?? '').replace(/[&<>"]/g,
  (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));

export function bar(name, ms, max) {
  const pct = max > 0 ? Math.round((ms / max) * 100) : 0;
  return `<div class="bar"><span class="name">${name}</span>
    <span class="track"><span class="fill" style="width:${pct}%"></span></span>
    <span class="val">${ms} ms</span></div>`;
}

export async function post(path, payload) {
  const resp = await fetch(path, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(payload),
  });
  const data = await resp.json();
  if (!resp.ok || data.error) {
    const err = new Error(data.error || ('HTTP ' + resp.status));
    err.status = resp.status;
    // A pass that ran out of memory says so in a typed field (`sd::oom_json`),
    // which `memory.js` turns into the ways out.
    err.oom = data.oom ?? null;
    throw err;
  }
  return data;
}

// A blank box means "unset", which the server reads as "leave the retained value
// alone" — so an empty Advanced panel never overrides anything.
export const numOr = (id) => {
  const v = parseFloat($(id).value);
  return Number.isFinite(v) ? v : null;
};

