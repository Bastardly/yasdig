import { $, esc } from './dom.js';

// ---- LoRAs ------------------------------------------------------------------
//
// A LoRA is a small adapter that patches a checkpoint's weights: a style, a
// character, a detail slider. sd.cpp takes them inline in the prompt
// (`<lora:name:strength>`) and resolves the names against `--lora-model-dir`;
// this page never writes that spelling, it sends `[{name, strength}]` and lets
// `base_cmd` assemble it. See `Profile::loras`.
//
// A factory, in `gridmark.js`'s shape and for the same reason — two copies of a
// control that builds a request body is two chances for one of them to send a
// subtly different one. One instance today, on the Image tab; the edit tab is
// the likely second.

// The LoRA directory's contents, shared by every picker: one request, every
// consumer, the same argument `outputGrid` makes for the output tile.
let available = [];
// The in-flight fetch, so two pickers initialising in the same tick share one
// request instead of racing two. Cleared by `force`, which is what the Add
// button passes — that is the one moment a stale list matters, and it costs one
// request against a directory listing, so a file dropped into the folder with
// the page open is one click away rather than a reload.
let loading = null;

function fetchLoras(force) {
  if (force || !loading) {
    loading = (async () => {
      try {
        const list = await (await fetch('/loras')).json();
        available = Array.isArray(list) ? list : [];
      } catch {
        available = [];
      }
    })();
  }
  return loading;
}

// Whether a LoRA belongs on a model of `family` (its `/models` wire id).
// The server's rule (`LoraBase::fits`) already evaluated: `families` is every
// family it fits. A LoRA nothing identified lists them all — refusing a working
// file because the scan couldn't read it would be worse than the mismatch — and
// a Wan or Flux.2 dev one lists none, so it fits nothing on this tab.
const fits = (l, family) => !family || l.families.includes(family);

// Everything the picker knows how to render for. An unusable file is kept and
// disabled rather than dropped, matching what the server publishes and why: "my
// LoRA isn't in the list" is a much worse thing to debug than "my LoRA is in the
// list and says why it can't be used". A LoRA for another family is the same
// case, and says which family it is for.
const why = (l, family) => (!l.usable ? l.note : fits(l, family) ? '' : 'for ' + l.base);
const options = (selected, family) => available.map((l) => {
  const no = why(l, family);
  return `
  <option value="${esc(l.name)}"${no ? ' disabled' : ''}${
    l.name === selected ? ' selected' : ''} title="${esc(no)}">${
    esc(l.label)}${no ? ' — ' + esc(no) : ''}</option>`;
}).join('');

// `rows` is the container the rows are drawn into, `add` the button that appends
// one. Both are element ids, so an instance is one object literal per tab.
// `family` returns the selected model's family, read at every draw, so
// `refresh()` after a model change is all it takes to re-grey the lists.
// `onChange` runs after every redraw — an added or removed row; an edit inside
// a row is an ordinary `input` event that bubbles to whoever is listening.
export function loraPicker({ rows, add, family = () => null, onChange = () => {} }) {
  // The rows, in order. Held here rather than read back out of the DOM because a
  // re-render rebuilds the elements and would lose a half-typed strength.
  let picked = [];

  // Add is disabled when it would add nothing: nothing on disk, or nothing on
  // disk for this model. Its click already picked only a fitting LoRA, so an
  // enabled button that did nothing on Z-Image was the one control on the tab
  // that looked broken. The title says which of the two it is.
  function syncAdd(fam) {
    const btn = $(add);
    const any = available.some((l) => l.usable);
    const ok = available.some((l) => l.usable && fits(l, fam));
    btn.disabled = !ok;
    btn.title = ok ? '' : !any ? 'No LoRAs in models/loras/'
      : 'No LoRA in models/loras/ is for this model\'s family';
  }

  // Every redraw is a change to what is picked, or to whether it fits, so it
  // reports once the rows are on screen — the Image tab's fold summary
  // counts them.
  function render() {
    draw();
    onChange();
  }

  function draw() {
    const el = $(rows);
    syncAdd(family());
    if (!available.length) {
      // The same shape as the empty-models message, and it names the folder,
      // because "the picker is empty" and "the picker is looking somewhere
      // else" are indistinguishable from in here. `doctor` prints the path.
      el.innerHTML = '<p class="sub">No LoRAs found — drop .safetensors files '
        + 'into models/loras/. <b>doctor</b> prints the folder it is looking in.</p>';
      return;
    }
    const fam = family();
    // A picked row that stops fitting — the model changed under it — stays,
    // with a line saying why. Dropping it would lose a choice on a select
    // change that may be flipped straight back; keeping it quietly would send
    // it, and the server refuses a LoRA for the wrong family.
    el.innerHTML = picked.map((p, i) => {
      const l = available.find((a) => a.name === p.name);
      const off = l && !fits(l, fam)
        ? `<p class="err">${esc(l.label)} is for ${esc(l.base)} — remove it or pick another for this model.</p>`
        : '';
      return `
      <div class="lorarow">
        <select data-i="${i}" aria-label="LoRA">${options(p.name, fam)}</select>
        <input type="number" data-i="${i}" step="0.05" min="-2" max="2"
          value="${p.strength}" aria-label="Strength" title="Multiplier. 1 is the weight it was trained at.">
        <button type="button" class="btn btn-danger" data-i="${i}"
          title="Remove this LoRA">&times;</button>
      </div>${off}`;
    }).join('');
    for (const sel of el.querySelectorAll('select')) {
      sel.addEventListener('change', () => { picked[Number(sel.dataset.i)].name = sel.value; });
    }
    for (const box of el.querySelectorAll('input')) {
      // Read on every keystroke rather than on blur: `get()` runs the moment
      // Generate is pressed, and a number still being typed in a focused box is
      // exactly when that happens.
      box.addEventListener('input', () => {
        const v = parseFloat(box.value);
        picked[Number(box.dataset.i)].strength = Number.isFinite(v) ? v : 1;
      });
    }
    for (const b of el.querySelectorAll('button')) {
      b.addEventListener('click', () => {
        picked.splice(Number(b.dataset.i), 1);
        render();
      });
    }
  }

  return {
    // Waits for the listing, since a model select fills before it lands and
    // drawing early would show the empty-folder message as if it were true.
    refresh: () => fetchLoras().then(render),

    // What goes in the request body. Empty is a real answer — the server reads
    // `[]` as "none, explicitly" and a missing key as "keep what the profile
    // has", which are different things.
    get: () => picked.map((p) => ({ name: p.name, strength: p.strength })),

    // Put a stored generation's LoRAs back, for reopening a work item. An entry
    // whose file has since been deleted is dropped rather than restored: the
    // request would be refused anyway, and a row naming a file that isn't there
    // is a row that can only produce an error.
    set(list) {
      picked = (list || []).filter((l) => available.some((a) => a.name === l.name && a.usable))
        .map((l) => ({ name: l.name, strength: l.strength }));
      render();
    },

    init() {
      $(add).addEventListener('click', async () => {
        await fetchLoras(true);
        const first = available.find((l) => l.usable && fits(l, family()));
        if (first) picked.push({ name: first.name, strength: 1 });
        render();
      });
      // Async, so the first draw knows what is on disk — the empty-folder
      // message is a real answer and must not be shown before the answer is in.
      // Nothing waits on it: it is one directory listing over the loopback.
      fetchLoras().then(render);
    },
  };
}
