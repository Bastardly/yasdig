import { followPointer } from './dom.js';

// ---- Floating panels: the Refine & adjust dialog's, and the workspaces' -------
//
// A narrow rail and the picture; each tool is a panel floating over the
// picture, opened from its rail button, dragged by its header to wherever it is
// wanted. The Refine & adjust dialog is built this way, and so are the Image
// and Video tabs (`host` is the dialog or the tab's `.workspace`). Several can
// be open at once — Selection beside Adjust is how a colour pick is used — and
// viewer.js decides which of the dialog's own the pointer.
//
// Each panel's element gets an `open` property that fires `toggle` when it
// changes, as a <details> does, so code written against a fold reads a panel
// the same way.
//
// Remembered per browser, per panel: open or not, and where it was left, as
// fractions of the dialog so it lands in the same place after the window
// changes size. Double-click a header to put the panel back where it starts.
// Storage can be missing or throw (a private window): everything works
// without it, as the markup says.
//
// A factory, regiontool.js's shape: nothing touches the DOM until `init()`.
export function floatPanels({ host, rail, defs }) {
  const GAP = 12;
  const CASCADE = 28;
  const panels = new Map();
  let z = 10;

  const key = (id) => `yasdig.panel.${id}`;
  const load = (id) => {
    try { return JSON.parse(localStorage.getItem(key(id))) ?? {}; } catch { return {}; }
  };
  const save = (p) => {
    try { localStorage.setItem(key(p.id), JSON.stringify({ open: !p.el.hidden, at: p.at })); } catch { /* this session only */ }
  };
  const clamp = (v, lo, hi) => Math.max(lo, Math.min(v, hi));
  const railW = () => rail.offsetWidth;

  // Where a panel starts: tool panels down from the rail's top-right corner,
  // each a little lower than the last so two open never cover each other's
  // header. History is the exception, below.
  const home = (p) => [railW() + GAP + p.index * CASCADE, GAP + p.index * CASCADE];

  // History starts in the bottom-right corner, held by its bottom edge, so a
  // list that grows grows upwards instead of off the screen. Dragged, it is
  // held by its top like the rest.
  function corner(p) {
    const w = host.clientWidth, h = host.clientHeight;
    p.el.style.left = `${Math.max(railW(), w - p.el.offsetWidth - GAP)}px`;
    p.el.style.top = 'auto';
    p.el.style.bottom = `${GAP}px`;
    p.el.style.maxHeight = `${h - 2 * GAP}px`;
  }

  // Put it at (left, top), kept inside the dialog with its header reachable,
  // and no taller than the room below it — its body scrolls instead.
  function put(p, left, top) {
    const w = host.clientWidth, h = host.clientHeight;
    const pw = p.el.offsetWidth;
    left = clamp(left, railW(), Math.max(railW(), w - pw - GAP));
    top = clamp(top, GAP, Math.max(GAP, h - 120));
    p.el.style.left = `${left}px`;
    p.el.style.top = `${top}px`;
    p.el.style.bottom = 'auto';
    p.el.style.maxHeight = `${h - top - GAP}px`;
  }

  function place(p) {
    if (p.el.hidden || !host.clientWidth) return;
    // The size it would have with all the room it wants, for the corner maths.
    p.el.style.maxHeight = '';
    if (p.at) put(p, p.at.x * host.clientWidth, p.at.y * host.clientHeight);
    else if (p.corner) corner(p);
    else put(p, ...home(p));
  }

  function front(p) {
    // Renumbered before it climbs into the status card and the wheel above.
    if (z > 900) {
      z = 10;
      for (const q of [...panels.values()].sort((a, b) => a.el.style.zIndex - b.el.style.zIndex)) q.el.style.zIndex = ++z;
    }
    p.el.style.zIndex = ++z;
  }

  function setOpen(p, on) {
    if (on === !p.el.hidden) return;
    p.el.hidden = !on;
    p.button?.classList.toggle('active', on);
    if (on) {
      place(p);
      front(p);
    }
    save(p);
    p.el.dispatchEvent(new Event('toggle'));
  }

  function drag(p, e) {
    const x0 = e.clientX, y0 = e.clientY;
    const l0 = p.el.offsetLeft, t0 = p.el.offsetTop;
    p.el.classList.add('moving');
    followPointer(p.head, e, (ev) => put(p, l0 + ev.clientX - x0, t0 + ev.clientY - y0), () => {
      p.el.classList.remove('moving');
      p.at = { x: p.el.offsetLeft / host.clientWidth, y: p.el.offsetTop / host.clientHeight };
      save(p);
    });
  }

  return {
    init() {
      defs.forEach((d, index) => {
        // `el` when the panel kept an older id (the Image tab's folds did,
        // so the code that opens one by id still finds it).
        const el = document.getElementById(d.el ?? `panel-${d.id}`);
        const p = {
          id: d.id, el, index, corner: d.corner,
          head: el.querySelector('.fphead'),
          button: rail.querySelector(`[data-panel="${d.id}"]`),
          at: null,
        };
        panels.set(d.id, p);
        Object.defineProperty(el, 'open', { get: () => !el.hidden, set: (v) => setOpen(p, !!v) });
        const was = load(d.id);
        p.at = was.at ?? null;
        el.hidden = !(was.open ?? !!d.open);
        p.button?.classList.toggle('active', !el.hidden);
        p.button?.addEventListener('click', () => setOpen(p, el.hidden));
        el.querySelector('.fpclose').addEventListener('click', () => setOpen(p, false));
        el.addEventListener('pointerdown', () => front(p));
        // The header is the handle, but its buttons stay buttons.
        p.head.addEventListener('pointerdown', (e) => {
          if (e.button !== 0 || e.target.closest('button')) return;
          e.preventDefault();
          drag(p, e);
        });
        p.head.addEventListener('dblclick', (e) => {
          if (e.target.closest('button')) return;
          p.at = null;
          save(p);
          place(p);
        });
      });
      window.addEventListener('resize', () => { for (const p of panels.values()) place(p); });
    },
    // The dialog or the tab was just shown: now there is a size to place against.
    layout() {
      for (const p of panels.values()) {
        if (!p.el.hidden) { place(p); front(p); }
      }
    },
    // A rail button's dot: its panel has something in effect.
    dot(id, on) {
      panels.get(id)?.button?.classList.toggle('on', !!on);
    },
  };
}
