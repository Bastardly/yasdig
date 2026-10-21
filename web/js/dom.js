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

// Line icons for the buttons that are only an icon, drawn in the button's
// text colour. Such a button carries `aria-label` and `title`: the picture
// says nothing to a screen reader. Static markup names one with `data-icon`
// (`fillIcons`, at boot); built markup interpolates `ICONS` itself.
const svg = (body) => `<svg class="icon" viewBox="0 0 24 24" aria-hidden="true" fill="none"
  stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">${body}</svg>`;
const pip = (x, y) => `<circle cx="${x}" cy="${y}" r="1.4" fill="currentColor" stroke="none"/>`;
export const ICONS = {
  // Back to -1: a circling arrow.
  reset: svg('<path d="M3 12a9 9 0 1 0 2.6-6.4"/><path d="M3 3.5V9h5.5"/>'),
  // A random seed, kept: a die showing five.
  dice: svg(`<rect x="3.5" y="3.5" width="17" height="17" rx="3.5"/>${pip(8.5, 8.5)}${pip(15.5, 8.5)}${pip(12, 12)}${pip(8.5, 15.5)}${pip(15.5, 15.5)}`),
  play: svg('<path d="M8 5v14l11-7z" fill="currentColor"/>'),
  pause: svg('<path d="M8.5 5v14M15.5 5v14" stroke-width="3"/>'),
};

export function fillIcons(root = document) {
  for (const el of root.querySelectorAll('[data-icon]')) el.innerHTML = ICONS[el.dataset.icon] ?? '';
}

// A play/pause toggle shows what a press does.
export function setPlayIcon(btn, playing) {
  if (!btn) return;
  btn.innerHTML = ICONS[playing ? 'pause' : 'play'];
  btn.setAttribute('aria-label', playing ? 'Pause' : 'Play');
  btn.title = playing ? 'Pause' : 'Play';
}

// A seed for the dice button: one sd.cpp takes, kept in the box.
export const randomSeed = () => Math.floor(Math.random() * 2 ** 31);

export function bar(name, ms, max) {
  const pct = max > 0 ? Math.round((ms / max) * 100) : 0;
  return `<div class="bar"><span class="name">${name}</span>
    <span class="track"><span class="fill" style="width:${pct}%"></span></span>
    <span class="val">${ms} ms</span></div>`;
}

export async function post(path, payload) {
  return answer(await fetch(path, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(payload),
  }));
}

// A GET of a JSON route, held to the same rule as `post`: a failure answers
// `{error}` with a 4xx/5xx, and that is a throw here, never a value a caller
// would then index as a list. Not for `/settings`, which answers a broken file
// with 200, its built-ins and an `error` beside them.
export async function getJSON(path) {
  return answer(await fetch(path));
}

async function answer(resp) {
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

// Every <details class="fold" id="…"> under `root` remembers whether it was open,
// per fold and per browser (`yasdig.fold.<id>`). Without a stored value a fold
// starts as the markup has it, so `open` there is the default. Every access in a
// try/catch: storage can be missing or throw, and a fold then simply starts as
// written. Shared by every tab with folds, so they all behave alike.
export function rememberFolds(root) {
  for (const d of root.querySelectorAll('details.fold[id]')) {
    const key = `yasdig.fold.${d.id}`;
    try {
      const v = localStorage.getItem(key);
      if (v !== null) d.open = v === '1';
    } catch { /* as the markup has it */ }
    d.addEventListener('toggle', () => {
      try { localStorage.setItem(key, d.open ? '1' : '0'); } catch { /* ignore */ }
    });
  }
}


// Track a press to its end, for a drag that must always finish: the button let
// go (on `el`, which holds the capture, or anywhere on the page should the
// capture have been lost), a cancelled pointer, or the capture taken away —
// say by `el` hiding under the pointer. `end` runs once, whichever came first.
export function followPointer(el, e, move, end) {
  let done = false;
  const off = () => {
    if (done) return;
    done = true;
    el.removeEventListener('pointermove', move);
    el.removeEventListener('lostpointercapture', off);
    for (const t of ['pointerup', 'pointercancel']) window.removeEventListener(t, off, true);
    end();
  };
  el.addEventListener('pointermove', move);
  el.addEventListener('lostpointercapture', off);
  for (const t of ['pointerup', 'pointercancel']) window.addEventListener(t, off, true);
  try { el.setPointerCapture(e.pointerId); } catch { /* the window listeners still end it */ }
}
