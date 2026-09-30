import { $ } from './dom.js';
import { fetchOutputs, outputGrid } from './outputs.js';

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
// - **Choose…** opens the shared `#picker` dialog over `GET /outputs`.
// - **Upload…**, a drop on the zone, or a paste while the tab is showing sends
//   the file to `POST /upload`, which puts it in `work/output/` under a minted
//   name and answers with that stem. So an upload is also in the picker next
//   time, and in the Outputs tab.
// - A reopened work item puts back the stem it was made from.
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
async function upload(file) {
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

// `tab` is the tab's container id, for the paste guard; `none` is what the name
// line says with nothing picked; `err` is where a failed upload is reported;
// `onChange(stem | null)` runs after every change, including a clear.
export function sourceSlot({ prefix, tab, none, err, onChange = () => {} }) {
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
  async function openPicker() {
    const grid = $('pickgrid');
    grid.innerHTML = '<p class="sub">Loading…</p>';
    $('picker').showModal();
    outputGrid(grid, await fetchOutputs(), {
      height: 140,
      empty: '<p class="sub">Nothing in work/output/ yet — save an image, or '
           + 'upload one with the button beside Choose.</p>',
      onPick: (picked) => {
        set(picked);
        $('picker').close();
      },
    });
  }

  function init() {
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
    document.addEventListener('paste', (e) => {
      if ($(tab).hidden) return;
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

  return { init, set, get: () => stem };
}
