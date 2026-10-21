import { $ } from './dom.js';
import { sourceSlot } from './source-slot.js';

// Reference images: up to four pictures the model looks at while it draws, the
// prompt naming them "image 1", "image 2" (sd-cli's `-r`, on a family that
// takes them — `references` on /models). They belong to the generation, not
// to the tab: the button sits in the Prompts panel, shows only for a model
// that takes them, and the picks live in a small dialog.
//
// One dialog, more than one owner: the Image tab's set lives in the slots,
// and a Video shot borrows them (`editRefs`) — its own picks go in, the
// Image tab's come back when the dialog closes, without a change event, so
// nothing on the Image tab hears another tab's pictures.
//
// Each slot is the same `sourceSlot` *Start from* uses — pick, upload, drop —
// with its own paste turned off: four slots listening would all take one paste,
// so the dialog routes it to the first empty slot instead.

const N = 4;
// Called whenever a pick changes: the Generation fold's summary, and the
// Refine & adjust dialog's buttons and dots.
const listeners = [];
const changed = () => listeners.forEach((fn) => fn());
// The model the button was last drawn for.
let model = null;
// A borrower's `onDone(stems)` while the dialog holds its picks, and the
// Image tab's set to put back.
let borrower = null;
let parked = [];

const slots = Array.from({ length: N }, (_, i) => sourceSlot({
  prefix: `ref${i + 1}`,
  tab: 'tab-image',
  none: 'empty',
  err: 'refserr',
  paste: false,
  onChange: () => { if (!borrower) { drawButton(); changed(); } },
}));

// The picks, in slot order, gaps closed: "image 1" is the first picked slot.
export const references = () => slots.map((s) => s.get()).filter(Boolean);

// Whether Generate sends them: only to a model that takes them. The picks are
// kept either way, so switching back to Klein brings them back.
export const refsActive = () => !!model?.references;

export function setReferences(stems) {
  slots.forEach((s, i) => s.set(stems[i] ?? null));
}

// The dialog over someone else's picks — a Video shot's: `stems` in the
// slots, `onDone(stems)` when it closes. The default words are the Image
// tab's; a borrower says what its pictures do.
export function editRefs(stems, onDone, note) {
  parked = references();
  borrower = onDone;
  setReferences(stems);
  $('refsnote').textContent = note;
  openRefs();
}

function giveBack() {
  if (!borrower) return;
  const done = borrower;
  const picked = references();
  setReferences(parked);
  borrower = null;
  $('refsnote').textContent = IMAGE_NOTE;
  done(picked);
}

export const onRefsChange = (fn) => { listeners.push(fn); };

// The Generation panel's button and the Refine & adjust dialog's
// (`data-refs-open`, in Refine and Edit) all say how many are picked.
function drawButton() {
  const n = references().length;
  for (const b of [$('refsopen'), ...document.querySelectorAll('[data-refs-open]')]) {
    b.textContent = n ? `References (${n})` : 'References…';
  }
}

// The dialog, from any of those buttons. Modal, so over Refine & adjust it
// stacks above it in the top layer, and Escape closes it first.
export function openRefs() {
  $('refserr').textContent = '';
  $('refsdlg').showModal();
}

// Shown for a model that takes references, hidden otherwise.
export function syncRefs(m) {
  model = m;
  $('refsopen').hidden = !refsActive();
  if (!refsActive() && !borrower && $('refsdlg').open) $('refsdlg').close();
  drawButton();
}

// The dialog's words for the Image tab, kept in the markup.
let IMAGE_NOTE = '';

export function initRefs() {
  IMAGE_NOTE = $('refsnote').textContent;
  $('refsdlg').addEventListener('close', giveBack);
  for (const s of slots) s.init();
  $('refsopen').addEventListener('click', openRefs);
  for (const b of document.querySelectorAll('[data-refs-open]')) b.addEventListener('click', openRefs);
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
