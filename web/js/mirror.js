import { wireHints } from './hints.js';

// ---- Mirrored fields: the Image tab's pass settings, shown in the dialog -----
//
// The Refine & adjust dialog's Refine and Hi-res panels show the Image tab's
// Refine pass and Hi-res pass settings — the same values, not copies of them,
// so a change in either place is the one change (`escalateSettings` reads the
// Image tab's controls). A field here is a stand-in for one there:
//
//   <div data-mirror="rsteps rcfg"></div>   two fields in a row
//   <div data-mirror="rprompt" data-label="Prompt for this pass"></div>
//
// Each is built from its source: the source's label (and so its (i) tooltip,
// through `data-help`), a clone of its control with the id `dl-<id>`, and a
// `<prompt-box>` around a prompt. Typing here writes the source and fires the
// same event on it, so everything that listens there — the summaries, the
// loop, `fillPass` refilling a pass when its model changes — hears it. Then
// every field is read back from its source, which picks up whatever those
// listeners changed.
//
// A factory rather than a custom element: a shadow root would cut the fields
// off from style.css and from `wireHints`, and these are ordinary fields.
export function mirrors(root, { onSync } = {}) {
  const pairs = []; // [source, field]

  const labelText = (id) => {
    const l = document.querySelector(`label[for="${id}"]`);
    if (!l) return id;
    // Text nodes only: the (i) `wireHints` put inside it is not the label.
    return [...l.childNodes].filter((n) => n.nodeType === 3).map((n) => n.textContent).join('').trim();
  };

  function field(id, label) {
    const src = document.getElementById(id);
    const ctl = src.cloneNode(true);
    ctl.id = `dl-${id}`;
    pairs.push([src, ctl]);
    const wrap = document.createElement('div');
    const l = document.createElement('label');
    l.htmlFor = ctl.id;
    l.dataset.help = id;
    l.textContent = label ?? labelText(id);
    wrap.append(l);
    if (src.closest('prompt-box')) {
      const box = document.createElement('prompt-box');
      box.append(ctl);
      wrap.append(box);
    } else {
      wrap.append(ctl);
    }
    for (const type of ['input', 'change']) {
      ctl.addEventListener(type, () => {
        src.value = ctl.value;
        src.dispatchEvent(new Event(type, { bubbles: true }));
        sync();
      });
    }
    return wrap;
  }

  // Every field from its source: value, placeholder, and a select's options
  // (the model lists arrive after boot and change with a scan). A field being
  // typed in keeps its caret: its source already says the same thing.
  function sync() {
    for (const [src, ctl] of pairs) {
      if (src.tagName === 'SELECT' && ctl.innerHTML !== src.innerHTML) ctl.innerHTML = src.innerHTML;
      if (ctl.value !== src.value) ctl.value = src.value;
      ctl.placeholder = src.placeholder;
      ctl.disabled = src.disabled;
    }
    onSync?.();
  }

  return {
    init() {
      for (const slot of root.querySelectorAll('[data-mirror]')) {
        const ids = slot.dataset.mirror.split(/\s+/);
        const label = slot.dataset.label;
        if (ids.length > 1) {
          slot.classList.add('row');
          for (const id of ids) slot.append(field(id));
        } else {
          slot.append(...field(ids[0], label).childNodes);
        }
      }
      wireHints(root);
    },
    sync,
  };
}
