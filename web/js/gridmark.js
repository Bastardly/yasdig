import { $, post } from './dom.js';

// ---- Click-to-mark, for a grid of thumbnails --------------------------------
//
// Three grids use this — the work items on the Image tab, the same grid showing
// favourites, and the Outputs gallery — and between them it owns the two things
// on the page that *destroy* a file and the one that keeps one. That is exactly
// the code not to have two copies of: a fix to one that misses the other is a
// fix that only half worked, on the half nobody was looking at.
//
// What it owns: the mode radios, the marked set, the buttons above the grid, and
// the requests. What it deliberately does not own is the grid's markup — each
// tab draws its own tiles and calls `mark()` on them while drawing, because the
// two carry different data attributes and mean different things by "active".
//
// **The actions are a table**, the way `FILTERS`, `FIELDS` and `WEB_FILES` are,
// because there are two of them now and the second arrived a feature later. One
// entry per radio value that isn't "open":
//
//   value    the radio value that makes this action the live one
//   button   id of the button that acts on the marks
//   mark     class put on a marked tile — `sel` is red, `keep` is green
//   label    that button's text, given the number marked
//   route    a *thunk*: the work-item grid's route moves with its shelf
//   key      the body key the ids go under, `ids` or `stems`
//   all      optional { button, noun, label, when } — the "do it to everything"
//            button. Only delete has one, and `when()` is what takes it away on
//            the favourites shelf, where emptying the directory is the one thing
//            the shelf exists to prevent.
//
// One marked set, shared by both actions rather than one each: a single radio
// group means exactly one action is live at a time, and changing mode already
// clears the marks. Two sets would be two overlapping state machines describing
// one radio group.
//
// The shape is a factory rather than a class because there is no inheritance
// here. It builds a closure and a Set and nothing else — every listener and
// every `$()` is inside `init()`, so calling it at a module's top level keeps
// the rule the rest of `web/js/` does.
export function gridMarks({ grid, mode, ids, reload, err, onMode, actions }) {
  const marked = new Set();

  // The radios are a *mode* rather than a modifier key for the reason the
  // README gives: culling twenty images from a batch is twenty clicks and one
  // press at the end, with the marks visible the whole time — not twenty
  // confirmations, and not one misclick that can't be taken back.
  const live = () => {
    const picked = document.querySelector(`input[name=${mode}]:checked`).value;
    return actions.find((a) => a.value === picked) ?? null;
  };

  const allowed = (a) => a.all.when?.() ?? true;

  // Drop marks on things that are no longer there, so a stale id can't ride
  // along in the next request. Called by the owner after it reloads its list.
  function prune(present) {
    for (const id of [...marked]) if (!present.includes(id)) marked.delete(id);
  }

  function sync() {
    const now = live();
    const n = marked.size;
    const total = ids().length;
    for (const a of actions) {
      const on = a === now;
      $(a.button).hidden = !on || n === 0;
      if (on && n) $(a.button).textContent = a.label(n);
      if (!a.all) continue;
      // Only for the live action, only when there is something to act on, and
      // only where the shelf allows it. An always-visible "Delete all" beside a
      // gallery is a misclick waiting for a slow afternoon; the confirm below is
      // the second line of defence, not the first.
      $(a.all.button).hidden = !on || total === 0 || !allowed(a);
      $(a.all.button).textContent = a.all.label(total);
    }
  }

  async function send(action, list) {
    if (!list.length) return;
    const buttons = actions.flatMap((a) => [a.button, a.all?.button]).filter(Boolean);
    for (const b of buttons) $(b).disabled = true;
    try {
      await post(action.route(), { [action.key]: list });
      marked.clear();
      // The image on screen is deliberately left alone if it was one of them:
      // whatever the server is holding for it is a copy in the scratch dir, so
      // the sliders and the escalations keep working. It just has nothing to go
      // back to any more.
      await reload();
    } catch (e) {
      err(String(e.message || e));
    } finally {
      for (const b of buttons) $(b).disabled = false;
    }
  }

  return {
    has: (id) => marked.has(id),
    prune,
    sync,
    // Paint a tile. The factory owns the mark classes because it owns which
    // action is live, and a tile marked green under a red button would be the
    // page lying about what the next press does.
    mark(el, id) {
      const now = live();
      for (const a of actions) el.classList.toggle(a.mark, a === now && marked.has(id));
    },
    // A click in a marking mode marks instead of opening. Returns whether it
    // did, so the caller can stop before doing the other thing.
    toggle(id) {
      if (!live()) return false;
      if (!marked.delete(id)) marked.add(id);
      return true;
    },
    init() {
      for (const a of actions) {
        $(a.button).addEventListener('click', () => send(a, [...marked]));
        if (!a.all) continue;
        // The one place on the page with a confirm box, because it is the one
        // action with nothing in front of it: marking is reversible and visible,
        // "all" is neither. The count is in the question because "delete
        // everything" reads very differently at 3 items and at 300.
        $(a.all.button).addEventListener('click', () => {
          const list = ids();
          if (!list.length || !allowed(a)) return;
          if (!confirm(
            `${a.all.label(list.length)} ${a.all.noun}${list.length === 1 ? '' : 's'}?\n\n`
            + 'This removes the files from disk and cannot be undone.')) return;
          send(a, list);
        });
      }
      for (const r of document.querySelectorAll(`input[name=${mode}]`)) {
        r.addEventListener('change', () => {
          // Changing mode clears the marks. A selection you can't see is one you
          // could act on later without meaning to — and the marks belong to the
          // action you just left, not to the one you arrived at.
          marked.clear();
          // Which action is live decides the hover colour, so the grid carries
          // it as data rather than as one class per action.
          $(grid).dataset.mark = live()?.value ?? '';
          // Re-marking the tiles is the owner's job — it knows what its own
          // `active` means — so the mode change is handed back rather than
          // reached for.
          onMode();
        });
      }
    },
  };
}
