import { $, esc } from './dom.js';

// ---- A model's separate files -----------------------------------------------
//
// The *Models* rows: one select per slot a model's family has (VAE, text
// encoder, tokenizer…), from the `parts` that `/models` and `/video-models`
// publish. The Image tab and the Video tab each own one, which is why this is a
// factory: the two differ in where the rows go, what an empty choice means, and
// who listens. An option carries the absolute path as `data-path`, because that
// is what a sidecar records — the wire carries names — so `set` can put back
// the files a picture or a clip ran with.

export function partRows({ el, prefix, none, onChange }) {
  const selects = () => [...$(el).querySelectorAll('select')];

  function options(part) {
    const opts = part.optional ? [`<option value="">${esc(none)}</option>`] : [];
    for (const o of part.options) {
      opts.push(`<option value="${esc(o.name)}" data-path="${esc(o.path)}">${esc(o.name)}</option>`);
    }
    // What the model holds and nothing on disk offers: a saved choice whose
    // file was deleted, or a required part not downloaded yet. Listed so the
    // select names it rather than silently showing another file.
    if (part.current && !part.options.some((o) => o.name === part.current)) {
      opts.push(`<option value="${esc(part.current)}" data-path="${esc(part.current_path ?? '')}">`
        + `${esc(part.current)} (missing)</option>`);
    }
    return opts.join('');
  }

  // Redrawn whenever the model changes, each select on what the model holds.
  function draw(parts) {
    $(el).innerHTML = (parts ?? []).map((p) => `
      <label for="${prefix}-${p.slot}">${esc(p.label)}</label>
      <select id="${prefix}-${p.slot}" data-slot="${p.slot}">${options(p)}</select>`).join('');
    for (const p of parts ?? []) $(`${prefix}-${p.slot}`).value = p.current ?? '';
    if (onChange) for (const sel of selects()) sel.addEventListener('change', onChange);
  }

  // Put back the files a run used, from absolute paths keyed by slot. A path no
  // option carries anymore is added as missing rather than dropped.
  function set(components) {
    for (const sel of selects()) {
      const path = components?.[sel.dataset.slot] ?? null;
      if (!path) {
        if ([...sel.options].some((o) => o.value === '')) sel.value = '';
        continue;
      }
      let opt = [...sel.options].find((o) => o.dataset.path === path);
      if (!opt) {
        opt = new Option(`${path} (missing)`, path);
        opt.dataset.path = path;
        sel.add(opt);
      }
      sel.value = opt.value;
    }
  }

  // Every slot, including an emptied one as `null`: the rows are the complete
  // answer to what the model runs with.
  const picked = () => Object.fromEntries(selects().map((s) => [s.dataset.slot, s.value || null]));

  return { draw, set, picked, selects };
}
