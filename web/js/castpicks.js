import { $, esc } from './dom.js';
import { castTile, fetchCast } from './cast.js';
import { onCastChanged, openCastEditor } from './casteditor.js';
import { namedIn } from './castnames.js';

// ---- The Image tab's cast row -------------------------------------------------
//
// Under the prompts, in the Prompts panel: the saved cast members this
// prompt is about, as chips. Each says what of it is sent (`config::CastPick`):
//
// - Words: its look leads the prompt, a paragraph of its own — composed on
//   the server (`cast::compose`), so the box keeps the short sentence and a
//   reopen puts the chips back beside it.
// - Picture: its picture (a character's turnaround) is a reference, on a
//   model that takes them and only from noise; the tick hides elsewhere.
//
// The names in the prompt are plain words ("Elias lights a cigar"); a name
// the prompt uses that isn't picked is offered, never added unasked.

let library = []; // the saved cast, `/cast` minus drafts
let loaded = false;
let picks = []; // [{ id, words, picture }], in the order their looks lead
let pictures = () => false; // whether a Picture tick would be sent now
let references = () => 0; // how many reference slots the References dialog holds

const entry = (id) => library.find((e) => e.id === id) ?? null;

// The chips' change, for the tab's delegated listener (the summaries, the
// loop, an Image chat's settings).
const announce = () => $('castchips').dispatchEvent(new Event('change', { bubbles: true }));

// What `/generate` (`withPictures`) or `/img2img` sends: always, `[]` too —
// the row is the whole answer, as the LoRA picker is.
export function castPayload(withPictures) {
  return picks
    .filter((p) => entry(p.id))
    .map((p) => ({ id: p.id, words: p.words, picture: withPictures && p.picture }));
}

// The names picked, for the Prompts panel's line.
export const castNames = () => picks.map((p) => entry(p.id)?.name).filter(Boolean);

// Put a reopened image's picks back (a sidecar's or a PNG's `profile.cast`).
// False when one is no longer in the cast: the caller then shows the prompt
// that ran, every look written out, rather than the short one without its
// chips — and sets no chips, so no look comes twice.
export function setCastPicks(list = []) {
  const want = (list ?? []).map((c) => ({ id: c.id, words: !!c.words, picture: !!c.picture }));
  if (loaded && want.some((p) => !entry(p.id))) {
    picks = [];
    drawChips();
    return false;
  }
  picks = want;
  drawChips();
  return true;
}

function add(id) {
  if (picks.some((p) => p.id === id)) return;
  picks.push({ id, words: true, picture: true });
  drawChips();
  announce();
}

function drawChips() {
  $('castchips').innerHTML = picks.map((p) => {
    const e = entry(p.id);
    const name = e?.name ?? 'Loading…';
    const thumb = e?.picture ? `<img src="/cast/${esc(e.id)}/thumb.png?v=${e.rev}" alt="">` : '';
    return `<div class="castchip" data-id="${esc(p.id)}" title="${esc(e?.look ?? '')}">${thumb}
      <b>${esc(name)}</b>
      <label title="Its look leads the prompt, as a paragraph of its own"><input type="checkbox" data-words${p.words ? ' checked' : ''}> Words</label>
      <label data-pic title="Its picture is one of the references the model follows${e?.sheet ? ' (its turnaround sheet: every side)' : ''}"><input type="checkbox" data-picture${
        p.picture ? ' checked' : ''}> Picture</label>
      <button type="button" class="btn castchipx" data-remove aria-label="Remove ${esc(name)}" title="Remove">×</button></div>`;
  }).join('');
  syncCastPicks();
}

// What depends on the model and the prompt, not on the picks: the Picture
// ticks, the reference count, the names offered. Cheap; the summaries call it.
export function syncCastPicks() {
  const pic = pictures();
  for (const l of $('castchips').querySelectorAll('[data-pic]')) l.hidden = !pic;
  const n = pic ? picks.filter((p) => p.picture && entry(p.id)).length : 0;
  const total = n + references();
  const note = $('castnote');
  note.hidden = total <= 4;
  note.textContent = `${total} pictures to follow, references included: a model takes at most 4. Untick a Picture or clear a reference.`;
  const offer = namedIn($('prompt').value, library, picks.map((p) => p.id));
  const s = $('castsuggest');
  s.hidden = !offer.length;
  s.innerHTML = offer.map((e) => `<button type="button" class="btn" data-offer="${esc(e.id)}"
    title="${esc(e.look)}">Add ${esc(e.name)}</button>`).join(' ') + (offer.length ? ' <span class="sub">— in your cast</span>' : '');
}

async function loadLibrary() {
  library = (await fetchCast()).filter((e) => !e.draft);
  loaded = true;
  drawChips();
}

function openPicker() {
  const left = library.filter((e) => !picks.some((p) => p.id === e.id));
  $('castpickgrid').innerHTML = left.map((e) => castTile(e, 140)).join('')
    || `<p class="sub">${library.length ? 'Every cast member is in the prompt already.'
      : 'No cast yet. New cast… makes one: a name, how they look, and a picture.'}</p>`;
  $('castpickdlg').showModal();
}

// `opts.pictures()`: a Picture tick would be sent (a model that takes
// references, no source picked). `opts.references()`: the References
// dialog's count, which shares the model's four slots.
export function initCastPicks(opts) {
  pictures = opts.pictures;
  references = opts.references;
  $('castadd').addEventListener('click', openPicker);
  $('castpickclose').addEventListener('click', () => $('castpickdlg').close());
  $('castpickdlg').addEventListener('click', (e) => {
    if (e.target === $('castpickdlg')) $('castpickdlg').close();
  });
  $('castpickgrid').addEventListener('click', (e) => {
    const b = e.target.closest('button[data-id]');
    if (!b) return;
    $('castpickdlg').close();
    add(b.dataset.id);
  });
  // A new one made from here goes straight into the prompt.
  $('castpicknew').addEventListener('click', () => {
    $('castpickdlg').close();
    openCastEditor(null, {}, async (saved) => {
      await loadLibrary();
      add(saved.id);
    });
  });
  $('castchips').addEventListener('change', (e) => {
    const chip = e.target.closest('.castchip');
    const p = chip && picks.find((x) => x.id === chip.dataset.id);
    if (!p) return;
    if (e.target.matches('[data-words]')) p.words = e.target.checked;
    if (e.target.matches('[data-picture]')) p.picture = e.target.checked;
    syncCastPicks();
  });
  $('castchips').addEventListener('click', (e) => {
    const b = e.target.closest('[data-remove]');
    if (!b) return;
    picks = picks.filter((p) => p.id !== b.closest('.castchip').dataset.id);
    drawChips();
    announce();
  });
  $('castsuggest').addEventListener('click', (e) => {
    const b = e.target.closest('[data-offer]');
    if (b) add(b.dataset.offer);
  });
  $('prompt').addEventListener('input', syncCastPicks);
  onCastChanged(loadLibrary);
  loadLibrary();
}
