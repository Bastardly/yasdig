import { $, post } from './dom.js';

// ---- Click-to-delete, for a grid of thumbnails -------------------------------
//
// Two grids have this now — the work items on the Image tab and the Outputs
// gallery — and they are the two places on the page that destroy a file. That is
// exactly the code not to have two copies of: a fix to one that misses the other
// is a fix that only half worked, on the half nobody was looking at.
//
// What it owns: the mode radios, the marked set, the two buttons above the grid,
// and the two requests. What it deliberately does not own is the grid's markup —
// each tab draws its own tiles and calls `has()` while marking them, because the
// two carry different data attributes and mean different things by "active".
//
// The shape is a factory rather than a class because there is no inheritance
// here and two instances are all there will ever be. It builds a closure and a
// Set and nothing else — every listener and every `$()` is inside `init()`, so
// calling it at a module's top level keeps the rule the rest of `web/js/` does.
export function gridDelete({ grid, mode, button, all, route, key, noun, ids, reload, err, onMode }) {
  const marked = new Set();

  // The radios are a *mode* rather than a modifier key for the reason the
  // README gives: culling twenty images from a batch is twenty clicks and one
  // press at the end, with the marks visible the whole time — not twenty
  // confirmations, and not one misclick that can't be taken back.
  const deleting = () =>
    document.querySelector(`input[name=${mode}]:checked`).value === 'delete';

  // Drop marks on things that are no longer there, so a stale id can't ride
  // along in the next request. Called by the owner after it reloads its list.
  function prune(present) {
    for (const id of [...marked]) if (!present.includes(id)) marked.delete(id);
  }

  function sync() {
    const n = marked.size;
    $(button).hidden = n === 0;
    $(button).textContent = `Delete ${n} selected`;
    // Only in delete mode, and only when there is something to delete. An
    // always-visible "Delete all" beside a gallery is a misclick waiting for a
    // slow afternoon; the confirm below is the second line of defence, not the
    // first.
    const total = ids().length;
    $(all).hidden = !deleting() || total === 0;
    $(all).textContent = `Delete all ${total}`;
  }

  async function send(list) {
    if (!list.length) return;
    $(button).disabled = true;
    $(all).disabled = true;
    try {
      await post(route, { [key]: list });
      marked.clear();
      // The image on screen is deliberately left alone if it was one of them:
      // whatever the server is holding for it is a copy in the scratch dir, so
      // the sliders and the escalations keep working. It just has nothing to go
      // back to any more.
      await reload();
    } catch (e) {
      err(String(e.message || e));
    } finally {
      $(button).disabled = false;
      $(all).disabled = false;
    }
  }

  return {
    has: (id) => marked.has(id),
    prune,
    sync,
    // A click in delete mode marks instead of opening. Returns whether it did,
    // so the caller can stop before doing the other thing.
    toggle(id) {
      if (!deleting()) return false;
      if (!marked.delete(id)) marked.add(id);
      return true;
    },
    init() {
      $(button).addEventListener('click', () => send([...marked]));
      // The one place on the page with a confirm box, because it is the one
      // action with nothing in front of it: marking is reversible and visible,
      // "all" is neither. The count is in the question because "delete
      // everything" reads very differently at 3 items and at 300.
      $(all).addEventListener('click', () => {
        const list = ids();
        if (!list.length) return;
        if (!confirm(
          `Delete all ${list.length} ${noun}${list.length === 1 ? '' : 's'}?\n\n`
          + 'This removes the files from disk and cannot be undone.')) return;
        send(list);
      });
      for (const r of document.querySelectorAll(`input[name=${mode}]`)) {
        r.addEventListener('change', () => {
          // Leaving delete mode clears the marks. A selection you can't see is
          // one you could act on later without meaning to.
          if (!deleting()) marked.clear();
          $(grid).classList.toggle('deleting', deleting());
          // Re-marking the tiles is the owner's job — it knows what its own
          // `active` means — so the mode change is handed back rather than
          // reached for.
          onMode();
        });
      }
    },
  };
}
