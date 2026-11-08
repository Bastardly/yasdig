import { $, esc, getJSON, post } from './dom.js';

// A note per model file (`user/model-notes.toml`, `src/notes.rs`): the blue
// i beside a model wherever one is picked or listed — the Models tab's
// Installed rows, the Image tab's model select and LoRA rows, the Video tab's
// model select and LoRA rows — opens the one dialog on it.
//
// A note is filed under the file's Installed id (`note_id` on `/models`,
// `/video-models` and `/loras`; `id` on `/installed`), so the page never names a
// path. Every i on the page is a `[data-note]` button; after a save, each one
// whose id it is turns filled, so a note is seen without opening it.
//
// Plain text, saved as it is typed (600 ms after the last key) and on close.

const SAVE_MS = 600;

let notes = {};
let loading = null;
let open = null; // { id, saved } while the dialog shows one
let timer = null;

export function loadNotes() {
  loading ??= getJSON('/model-notes')
    .then((n) => { notes = n ?? {}; markAll(); })
    .catch(() => {})
    .finally(() => { loading = null; });
  return loading;
}

export const hasNote = (id) => !!(id && notes[id]);

// The i's markup, for a module drawing rows. `id` empty → a disabled i (a
// file outside every model folder has no id to file a note under).
export function infoButton(id, label) {
  return `<button type="button" class="btn btn-info${hasNote(id) ? ' has' : ''}" data-note="${esc(id ?? '')}"`
    + ` data-note-label="${esc(label ?? '')}" aria-label="Note on ${esc(label ?? 'this model')}"`
    + ` title="Your note on this model"${id ? '' : ' disabled'}>i</button>`;
}

// Point an existing i at another file — a select changed under it.
export function setInfo(btn, id, label) {
  if (!btn) return;
  btn.dataset.note = id ?? '';
  btn.dataset.noteLabel = label ?? '';
  btn.disabled = !id;
  btn.setAttribute('aria-label', `Note on ${label ?? 'this model'}`);
  btn.classList.toggle('has', hasNote(id));
}

function markAll() {
  for (const b of document.querySelectorAll('[data-note]')) b.classList.toggle('has', hasNote(b.dataset.note));
}

async function save() {
  clearTimeout(timer);
  timer = null;
  if (!open) return;
  const text = $('notetext').value;
  if (text === open.saved) return;
  const id = open.id;
  try {
    await post('/model-notes', { id, text });
    if (text.trim()) notes[id] = text; else delete notes[id];
    if (open?.id === id) open.saved = text;
    $('notemsg').textContent = 'Saved';
    $('noteerr').textContent = '';
    markAll();
  } catch (e) {
    $('noteerr').textContent = String(e.message || e);
  }
}

export async function openNote(id, label) {
  if (!id) return;
  await loadNotes();
  const text = notes[id] ?? '';
  open = { id, saved: text };
  $('notetitle').textContent = label || id;
  $('notefile').textContent = id.replace(/^[a-z]+\//, '');
  $('notetext').value = text;
  $('notemsg').textContent = '';
  $('noteerr').textContent = '';
  $('notedlg').showModal();
  $('notetext').focus();
}

// The note of a file the Models tab just deleted went with it.
export function forget(id) {
  delete notes[id];
  markAll();
}

export function initNotes() {
  // Delegated: rows with an i are redrawn all the time.
  document.addEventListener('click', (e) => {
    const b = e.target.closest('[data-note]');
    if (!b || b.disabled) return;
    e.preventDefault();
    e.stopPropagation();
    openNote(b.dataset.note, b.dataset.noteLabel);
  });
  $('notetext').addEventListener('input', () => {
    $('notemsg').textContent = '';
    clearTimeout(timer);
    timer = setTimeout(save, SAVE_MS);
  });
  $('noteclose').addEventListener('click', () => $('notedlg').close());
  $('notedlg').addEventListener('click', (e) => {
    if (e.target === $('notedlg')) $('notedlg').close();
  });
  // Escape and the button both end here: what is typed is never lost.
  $('notedlg').addEventListener('close', async () => {
    await save();
    open = null;
  });
  loadNotes();
}
