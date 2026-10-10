import { $ } from './dom.js';
import { sourceSlot } from './source-slot.js';

// Reference images: up to four pictures the model looks at while it draws, the
// prompt naming them "image 1", "image 2" (sd-cli's `-r`, on a family that
// takes them — `references` on /models). They belong to the generation, not
// to the tab: the button sits in the Generation fold, shows only for a model
// that takes them, and the picks live in a small dialog.
//
// Each slot is the same `sourceSlot` *Start from* uses — pick, upload, drop —
// with its own paste turned off: four slots listening would all take one paste,
// so the dialog routes it to the first empty slot instead.

const N = 4;
// Called whenever a pick changes, so the Generation fold's summary can say so.
let changed = () => {};
// The model the button was last drawn for.
let model = null;

const slots = Array.from({ length: N }, (_, i) => sourceSlot({
  prefix: `ref${i + 1}`,
  tab: 'tab-image',
  none: 'empty',
  err: 'refserr',
  paste: false,
  onChange: () => { drawButton(); changed(); },
}));

// The picks, in slot order, gaps closed: "image 1" is the first picked slot.
export const references = () => slots.map((s) => s.get()).filter(Boolean);

// Whether Generate sends them: only to a model that takes them. The picks are
// kept either way, so switching back to Klein brings them back.
export const refsActive = () => !!model?.references;

export function setReferences(stems) {
  slots.forEach((s, i) => s.set(stems[i] ?? null));
}

export const onRefsChange = (fn) => { changed = fn; };

function drawButton() {
  const n = references().length;
  $('refsopen').textContent = n ? `References (${n})` : 'References…';
}

// Shown for a model that takes references, hidden otherwise.
export function syncRefs(m) {
  model = m;
  $('refsopen').hidden = !refsActive();
  if (!refsActive() && $('refsdlg').open) $('refsdlg').close();
  drawButton();
}

export function initRefs() {
  for (const s of slots) s.init();
  $('refsopen').addEventListener('click', () => {
    $('refserr').textContent = '';
    $('refsdlg').showModal();
  });
  $('refsdone').addEventListener('click', () => $('refsdlg').close());
  // A click on the backdrop closes it, as the Jobs dialog's does: the box
  // inside carries the padding, so only the backdrop is the dialog itself.
  $('refsdlg').addEventListener('click', (e) => {
    if (e.target === $('refsdlg')) $('refsdlg').close();
  });
  // A paste while the dialog is up fills the first empty slot.
  document.addEventListener('paste', (e) => {
    if (!$('refsdlg').open) return;
    const file = [...(e.clipboardData?.items || [])]
      .find((i) => i.kind === 'file' && i.type.startsWith('image/'));
    const slot = slots.find((s) => !s.get());
    if (file && slot) {
      e.preventDefault();
      slot.take(file.getAsFile());
    }
  });
  drawButton();
}
