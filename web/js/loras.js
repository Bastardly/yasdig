import { $, esc } from './dom.js';

// ---- LoRAs ------------------------------------------------------------------
//
// A LoRA is a small adapter that patches a checkpoint's weights: a style, a
// character, a detail slider. sd.cpp takes them inline in the prompt
// (`<lora:name:strength>`) and resolves the names against `--lora-model-dir`;
// this page never writes that spelling, it sends `[{name, strength}]` and lets
// `base_cmd` assemble it. See `Profile::loras`.
//
// Two tabs have a picker and they are instances of the same factory, in
// `griddelete.js`'s shape and for the same reason — two copies of a control that
// builds a request body is two chances for one of them to send a subtly
// different one.

// The LoRA directory's contents, shared by both pickers: one request, two
// consumers, the same argument `outputGrid` makes for the output tile.
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

// Everything the picker knows how to render for. An unusable file is kept and
// disabled rather than dropped, matching what the server publishes and why: "my
// LoRA isn't in the list" is a much worse thing to debug than "my LoRA is in the
// list and says why it can't be used".
const options = (selected) => available.map((l) => `
  <option value="${esc(l.name)}"${l.usable ? '' : ' disabled'}${
    l.name === selected ? ' selected' : ''} title="${esc(l.note)}">${
    esc(l.label)}${l.usable ? '' : ' — ' + esc(l.note)}</option>`).join('');

// `rows` is the container the rows are drawn into, `add` the button that appends
// one. Both are element ids, so an instance is one object literal per tab.
export function loraPicker({ rows, add }) {
  // The rows, in order. Held here rather than read back out of the DOM because a
  // re-render rebuilds the elements and would lose a half-typed strength.
  let picked = [];

  function render() {
    const el = $(rows);
    if (!available.length) {
      // The same shape as the empty-models message, and it names the folder,
      // because "the picker is empty" and "the picker is looking somewhere
      // else" are indistinguishable from in here. `doctor` prints the path.
      el.innerHTML = '<p class="sub">No LoRAs found — drop .safetensors files '
        + 'into models/loras/. <b>doctor</b> prints the folder it is looking in.</p>';
      return;
    }
    el.innerHTML = picked.map((p, i) => `
      <div class="lorarow">
        <select data-i="${i}" aria-label="LoRA">${options(p.name)}</select>
        <input type="number" data-i="${i}" step="0.05" min="-2" max="2"
          value="${p.strength}" aria-label="Strength" title="Multiplier. 1 is the weight it was trained at.">
        <button type="button" class="btn btn-danger" data-i="${i}"
          title="Remove this LoRA">&times;</button>
      </div>`).join('');
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
        const first = available.find((l) => l.usable);
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
