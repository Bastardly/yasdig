import { $, esc, post } from './dom.js';
import { fetchOutputs, outputGrid } from './outputs.js';
import { castTile, fetchCast } from './cast.js';

// "Start from": an image a generation begins with instead of noise. The Image
// tab's source and the Video tab's first frame are both this, so it is written
// once — the pattern `loras.js` and `gridmark.js` follow, and for their reason:
// two copies of a control that ends up in a request body are two chances for
// one of them to send something different.
//
// The source is a `work/output/` stem, never a path — the page picks from what
// the server published, and `workitems::valid_id` is the whitelist the server
// holds it to. There are three ways to get one, and they all end in a stem:
//
// - **Choose…** opens the shared `#picker` dialog over `GET /outputs`, or over
//   `GET /favorite-outputs` — a favourite is a stem like any other.
// - **Upload…**, a drop on the zone, or a paste while the tab is showing sends
//   the file to `POST /upload`, which puts it in `work/output/` under a minted
//   name and answers with that stem. So an upload is also in the picker next
//   time, and in the Outputs tab.
// - A reopened work item puts back the stem it was made from.
// - The picker's **Cast** source: a saved entry's picture (or voice) is
//   handed over as an output copy (`POST /cast/output`, made once and
//   reused), so every slot that takes a stem takes a cast member too.
//
// The markup is `index.html`'s — the ids are `${prefix}drop`, `thumb`, `name`,
// `pick`, `upload`, `clear` and `file` — and this module only wires it, so the
// look stays in the stylesheet with everything else.

// The long side an upload is capped at before it leaves the browser. Bigger than
// anything worth diffusing (the working-size select tops out at 3072), and
// chosen against the body limit in http.rs: a 4096² PNG is ~25 MB against a
// 32 MB cap, while a 6000 px phone photo would not fit at all.
const MAX_UPLOAD_SIDE = 4096;

// Whatever the browser can decode becomes a PNG on the way out.
//
// This is what keeps `image` compiled with the `png` feature alone: the format
// list tracks the browser's rather than a crate's, so JPEG, WebP and AVIF all
// work without this binary growing a decoder it would then own. The cap is here
// rather than on the server for the same practical reason the whole route has a
// raised body limit — 25 MB of phone photo is not worth uploading to scale it
// down at the other end.
export async function upload(file) {
  if (!file || !file.type.startsWith('image/')) throw new Error('that is not an image');
  const bmp = await createImageBitmap(file);
  const scale = Math.min(1, MAX_UPLOAD_SIDE / Math.max(bmp.width, bmp.height));
  const w = Math.max(1, Math.round(bmp.width * scale));
  const h = Math.max(1, Math.round(bmp.height * scale));
  const cv = document.createElement('canvas');
  cv.width = w;
  cv.height = h;
  cv.getContext('2d').drawImage(bmp, 0, 0, w, h);
  bmp.close?.();
  const blob = await new Promise((ok) => cv.toBlob(ok, 'image/png'));
  if (!blob) throw new Error('the browser could not encode that image');

  // Raw bytes, not JSON: /upload is the one route with a binary body, which is
  // a third smaller on the wire and needs no decoder at either end.
  const resp = await fetch('/upload', {
    method: 'POST',
    headers: { 'Content-Type': 'image/png' },
    body: blob,
  });
  const data = await resp.json();
  if (!resp.ok || data.error) throw new Error(data.error || ('HTTP ' + resp.status));
  return data.stem;
}

// ---- The picker dialog -----------------------------------------------------
//
// One `#picker` for every slot, so its state is here rather than per slot: who
// opened it (`pickFor`, that slot's `set`) and which gallery it shows. The
// gallery is remembered between opens — whoever went to Favorites once is
// likely to go there again. The toggle's listeners go on once, from whichever
// slot initialises first.
let pickFor = null;
let pickGallery = 'outputs';
// Pictures, or — for a shot's sounds and a voice — sounds.
let pickSounds = false;
// The cast editor's own Choose… leaves the Cast source out.
let pickCast = true;
let pickerWired = false;

// The Cast source: entries with a picture (or, for sounds, a voice). A pick
// becomes an output stem before it reaches the slot.
async function drawCastPicker(grid, sounds) {
  const list = (await fetchCast()).filter((e) => !e.draft && (sounds ? e.voice : e.picture));
  if (pickGallery !== 'cast' || sounds !== pickSounds) return;
  grid.innerHTML = list.map((e) => castTile(e, 140)).join('')
    || `<p class="sub">${sounds ? 'No cast member has a voice yet — give one a voice in the cast editor.'
      : 'No cast yet — New cast… on the Cast tab makes one.'}</p>`;
  grid.onclick = async (ev) => {
    const b = ev.target.closest('button[data-id]');
    if (!b) return;
    const e = list.find((x) => x.id === b.dataset.id);
    try {
      const { stem } = await post('/cast/output', { id: e.id, which: sounds ? 'voice' : 'picture' });
      // A character's picture brings its voice: a slot that can use one (a
      // shot's references) adds it as their sound.
      const voice = !sounds && e.voice && e.kind === 'character'
        ? (await post('/cast/output', { id: e.id, which: 'voice' })).stem : null;
      pickFor?.(stem, { cast: { id: e.id, name: e.name.trim(), kind: e.kind, look: e.look }, voice });
      $('picker').close();
    } catch (e) {
      grid.insertAdjacentHTML('afterbegin', `<p class="err">${esc(e.message)}</p>`);
    }
  };
}

async function drawPicker() {
  const grid = $('pickgrid');
  if (!pickCast && pickGallery === 'cast') pickGallery = 'outputs';
  const fav = pickGallery === 'favorite-outputs';
  $('pickcurrent').classList.toggle('active', pickGallery === 'outputs');
  $('pickfavorites').classList.toggle('active', fav);
  $('pickcast').classList.toggle('active', pickGallery === 'cast');
  $('pickcast').hidden = !pickCast;
  grid.innerHTML = '<p class="sub">Loading…</p>';
  grid.onclick = null;
  const from = pickGallery;
  const sounds = pickSounds;
  if (from === 'cast') return drawCastPicker(grid, sounds);
  // Pictures only, or sounds only: a saved clip is never either.
  const list = (await fetchOutputs(from)).filter((o) => !o.video && !!o.audio === sounds);
  if (from !== pickGallery || sounds !== pickSounds) return;
  outputGrid(grid, list, {
    height: 140,
    empty: sounds
      ? '<p class="sub">No sounds yet — upload one with the button beside Choose.</p>'
      : fav
      ? '<p class="sub">No favorite outputs yet — keep some on the Outputs tab.</p>'
      : '<p class="sub">Nothing in work/output/ yet — save an image, or '
        + 'upload one with the button beside Choose.</p>',
    onPick: (picked) => {
      pickFor?.(picked);
      $('picker').close();
    },
  });
}

function wirePicker() {
  if (pickerWired) return;
  pickerWired = true;
  for (const [btn, g] of [['pickcurrent', 'outputs'], ['pickfavorites', 'favorite-outputs'], ['pickcast', 'cast']]) {
    $(btn).addEventListener('click', () => { pickGallery = g; drawPicker(); });
  }
}

// Every open goes through here and says which kind it wants: the kind is
// module state, and a slot that left it as the last opener did showed only
// sounds after a voice was chosen.
function openPickerFor(onPick, sounds, cast = true) {
  wirePicker();
  pickFor = onPick;
  pickSounds = sounds;
  pickCast = cast;
  $('picktitle').textContent = sounds ? 'Pick a sound' : 'Pick a picture';
  $('picker').showModal();
  drawPicker();
}

// The picker for anyone else who wants a stem: the Assistant's composer.
// `onPick(stem, extra)`: `extra` is `{cast: {id, name, kind, look}, voice}`
// for a pick from the Cast source (`voice` a character's voice, as an output
// stem, or null), absent otherwise.
// `{cast: false}` leaves the Cast source out (the cast editor's own).
export function choosePicture(onPick, { cast = true } = {}) {
  openPickerFor(onPick, false, cast);
}

// The same picker over the sounds (`sounds.js`).
export function chooseSound(onPick, { cast = true } = {}) {
  openPickerFor(onPick, true, cast);
}

// `tab` is the tab's container id, for the paste guard; `none` is what the name
// line says with nothing picked; `err` is where a failed upload is reported;
// `onChange(stem | null)` runs after every change, including a clear.
// `paste: false` leaves pasting to the caller — the References dialog has four
// slots and routes a paste to the first empty one rather than all four.
// `onCast(stem, extra)` runs after a pick from the Cast source (`extra` as
// `choosePicture` gives it).
export function sourceSlot({ prefix, tab, none, err, onChange = () => {}, onCast = null, paste = true }) {
  const id = (part) => `${prefix}${part}`;
  let stem = null;

  function set(next) {
    stem = next || null;
    $(id('thumb')).hidden = !stem;
    if (stem) $(id('thumb')).src = `/outputs/${encodeURIComponent(stem)}/thumb.png`;
    $(id('clear')).hidden = !stem;
    $(id('name')).textContent = stem ?? none;
    onChange(stem);
  }

  async function take(file) {
    $(err).textContent = '';
    $(id('name')).textContent = 'Reading…';
    try {
      set(await upload(file));
    } catch (e) {
      set(stem);
      $(err).textContent = 'Could not import that image: ' + (e.message || e);
    }
  }

  // The same tile as the Outputs tab, from the same function — only the height
  // differs, because this one is a dialog rather than a panel.
  // The stem is all a slot keeps, whichever gallery it came from.
  function openPicker() {
    openPickerFor((picked, extra) => {
      set(picked);
      if (extra?.cast) onCast?.(picked, extra);
    }, false);
  }

  function init() {
    wirePicker();
    $(id('pick')).addEventListener('click', openPicker);
    $(id('upload')).addEventListener('click', () => $(id('file')).click());
    $(id('clear')).addEventListener('click', () => set(null));
    $(id('file')).addEventListener('change', (e) => {
      take(e.target.files[0]);
      // Cleared so re-picking the same file fires `change` again.
      e.target.value = '';
    });
    // The whole zone, not the button — dragging a file onto a 90×30 target is
    // a game of skill. `dragover` must be cancelled or the browser navigates to
    // the file.
    const zone = $(id('drop'));
    for (const ev of ['dragenter', 'dragover']) {
      zone.addEventListener(ev, (e) => {
        e.preventDefault();
        zone.classList.add('over');
      });
    }
    for (const ev of ['dragleave', 'drop']) {
      zone.addEventListener(ev, () => zone.classList.remove('over'));
    }
    zone.addEventListener('drop', (e) => {
      e.preventDefault();
      take(e.dataTransfer?.files?.[0]);
    });
    // On the document because there is nothing sensible to focus first, so it
    // checks its own tab is the one showing — two slots, two listeners, at most
    // one of them live — and stays out of the way of pasting into a prompt.
    if (paste) document.addEventListener('paste', (e) => {
      // Not while a dialog is up: a paste there belongs to the dialog (the
      // References one routes its own), not to the slot behind it.
      if ($(tab).hidden || document.querySelector('dialog[open]')) return;
      if (/^(INPUT|TEXTAREA)$/.test(document.activeElement?.tagName)) return;
      const file = [...(e.clipboardData?.items || [])]
        .find((i) => i.kind === 'file' && i.type.startsWith('image/'));
      if (file) {
        e.preventDefault();
        take(file.getAsFile());
      }
    });
    set(null);
  }

  return { init, set, take, get: () => stem };
}
