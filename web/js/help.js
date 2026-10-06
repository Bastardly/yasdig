import { $, esc } from './dom.js';

// The Help tab: a menu of topics on the left, one topic on the right.
//
// A topic is a row in `TOPICS`, and that is the whole of adding one — the menu
// and the page are drawn from it. Written as HTML strings rather than fetched
// files because they are short, and a file per topic would be a `WEB_FILES`
// row per topic.
//
// The tooltips (`hints.js`) say what a control *is*; this is where choosing
// between its values goes. They point here rather than repeating it, for the
// rule `HELP` already states: one copy of each sentence.

// A name/use table, the shape every comparison here takes. The first column is
// a value you can pick, so it is set as code — except where it names models.
const table = (head, rows, code = true) => `
  <table class="helptable">
    <thead><tr>${head.map((h) => `<th>${h}</th>`).join('')}</tr></thead>
    <tbody>${rows.map(([name, ...rest]) => `<tr><td>${code ? `<code>${name}</code>` : name}</td>${
      rest.map((c) => `<td>${c}</td>`).join('')}</tr>`).join('')}</tbody>
  </table>`;

export const TOPICS = [
  {
    id: 'samplers',
    title: 'Samplers',
    html: `
      <h2>Samplers</h2>
      <p>The sampler is how each step removes noise. At enough steps most give a
      similar picture; what changes is the <b>look</b> (crisp, soft, textured),
      how <b>few steps</b> they need, and whether the same seed gives the
      <b>same picture</b>.</p>
      <p><b>Start with what the model fills in.</b> Turbo, Lightning, LCM and the
      newer models (Flux, Z-Image, Qwen-Image) are trained for one sampler and
      look wrong on others.</p>

      <h3>Good starting points</h3>
      ${table(['Model', 'Sampler'], [
        ['Realistic and photo SDXL', '<code>dpm++2m_sde</code>, 30 steps — or <code>dpm++2m</code>, 25 steps. With <code>karras</code>'],
        ['Anime and illustration SDXL', '<code>euler_a</code>, 25–30 steps'],
        ['SD 1.5', '<code>dpm++2m</code> + <code>karras</code>, 20–25 steps'],
        ['SDXL Turbo, Lightning, Hyper', '<code>euler</code>, the steps on the model’s card (4–8), CFG 1–2'],
        ['Anything with an LCM LoRA', '<code>lcm</code>, 4–8 steps, CFG 1–2'],
        ['Flux, Krea 2, Qwen-Image, Z-Image', '<code>euler</code>'],
        ['Quick drafts on SDXL', '<code>ipndm</code>, 12–15 steps'],
      ], false)}

      <h3>Repeatable</h3>
      <p>Same seed, same picture. More steps refine the image rather than change it.</p>
      ${table(['Sampler', 'What you’ll see'], [
        ['euler', 'Clean and neutral. The safe first try on any model.'],
        ['dpm++2m', 'Crisp and detailed by 20–25 steps. The usual pick for realistic SDXL and SD 1.5.'],
        ['dpm++2mv2', 'Like dpm++2m, a little cleaner at low step counts.'],
        ['ipndm, ipndm_v', 'Holds together at 10–15 steps, where euler is still blurry. Good for drafts.'],
        ['heun, dpm2', 'Slightly cleaner edges than euler. <b>Twice as slow</b> per step — use half the steps.'],
        ['res_multistep', 'Sharper than euler at the same steps. Worth a try when a Flux or Qwen-Image looks soft.'],
        ['res_2s', 'As above, a bit sharper again. <b>Twice as slow</b> per step.'],
        ['ddim_trailing', 'Only when a model’s card asks for DDIM.'],
        ['lms', 'An older method. No reason to pick it today.'],
      ])}

      <h3>Varying</h3>
      <p>Names ending in <code>_a</code> or <code>sde</code> add a little fresh noise each
      step. The picture keeps changing as you add steps, and seeds differ more from
      each other.</p>
      ${table(['Sampler', 'What you’ll see'], [
        ['euler_a', 'Softer and smoother, a painterly look. Loved for anime and illustration; can blur fine detail in photos.'],
        ['dpm++2m_sde', 'More fine texture: skin pores, fabric, film grain. Needs 25+ steps. Great for realistic portraits.'],
        ['dpm++2m_sde_bt', 'Like dpm++2m_sde, a slightly different texture. Try both.'],
        ['er_sde', 'Textured like the sde ones, but already good at 15–20 steps.'],
        ['dpm++2s_a', 'Detailed with a soft finish. <b>Twice as slow</b> per step.'],
      ])}

      <h3>Few-step</h3>
      <p>Only for models or LoRAs trained for them; on a normal model they look washed out.</p>
      ${table(['Sampler', 'What you’ll see'], [
        ['lcm', 'Smooth, a bit waxy. For LCM models and LoRAs at 4–8 steps, CFG 1–2.'],
        ['tcd', 'Like lcm with more detail kept. For TCD LoRAs at 4–8 steps.'],
      ])}

      <h3>Experimental</h3>
      ${table(['Sampler', 'What you’ll see'], [
        ['euler_cfg_pp, euler_a_cfg_pp', 'Variants that need a very low CFG (about 1–2). At a normal CFG they burn the colours.'],
        ['euler_ge', 'An Euler variant for experimenting; close to euler in practice.'],
      ])}`,
  },
  {
    id: 'schedulers',
    title: 'Schedulers',
    html: `
      <h2>Schedulers</h2>
      <p>The scheduler decides how much noise is left after each step — where the
      steps are spent. Spending more of them near the end, where little noise is
      left, tends to give cleaner fine detail.</p>
      <p><b>Leave it on Auto unless you have a reason.</b> Auto is not one
      scheduler: sd.cpp picks per model — Flux and Flux.2 get schedules of their
      own, <code>lcm</code> and <code>tcd</code> get <code>lcm</code>,
      <code>ddim_trailing</code> gets <code>simple</code>, and everything else gets
      <code>discrete</code>. Forcing <code>discrete</code> onto a Flux makes it
      worse.</p>
      ${table(['Scheduler', 'What it does'], [
        ['Auto', 'The model’s own choice, as above. The default.'],
        ['discrete', 'The noise levels the model was trained on, evenly spaced. What Auto picks for SD and SDXL.'],
        ['karras', 'Packs more steps into the low-noise end, for cleaner fine detail. The classic partner for DPM++ samplers.'],
        ['exponential', 'Evenly spaced on a log scale. Similar in spirit to karras, a different spread.'],
        ['sgm_uniform', 'Uniform spacing that ends exactly at zero noise. Common with distilled, few-step models.'],
        ['simple', 'A plain even pick from the model’s noise levels.'],
        ['ays', '“Align Your Steps”: schedules tuned for very few steps. <b>SD 1.5 and SDXL only</b> — refused for other models.'],
        ['lcm', 'For LCM-distilled models. Auto picks it for the <code>lcm</code> and <code>tcd</code> samplers.'],
      ])}

      <h3>More</h3>
      ${table(['Scheduler', 'What it does'], [
        ['gits', 'Precomputed few-step schedules, like ays.'],
        ['smoothstep', 'An S-curve: slow at both ends, quick in the middle.'],
        ['kl_optimal', 'Spacing derived to minimise error per step; good at low step counts.'],
        ['beta', 'Spends steps at both ends, little in the middle. A common choice in ComfyUI workflows.'],
        ['bong_tangent', 'A tangent-shaped curve with a similar both-ends shape. For experimenting.'],
        ['flux, flux2', 'Flux’s and Flux.2’s own schedules — what Auto already picks for them.'],
      ])}

      <h3>Good starting pairs</h3>
      ${table(['Model', 'Sampler + scheduler'], [
        ['SDXL, SD 1.5', '<code>dpm++2m</code> + <code>karras</code>, 25–30 steps'],
        ['Turbo, Lightning, Z-Image', '<code>euler</code> + Auto, at the model’s own steps'],
        ['LCM LoRA', '<code>lcm</code> + Auto, 4–8 steps, CFG 1–2'],
        ['Flux, Krea 2, Qwen-Image', '<code>euler</code> + Auto'],
      ], false)}`,
  },
  {
    id: 'redraw',
    title: 'Redraw',
    html: `
      <h2>Redraw</h2>
      <p>Keeps an image that is almost right. Paint over what should change — a hand,
      a face, a stray object — and press <b>Redraw</b>: only that area is redrawn,
      and every pixel outside it stays exactly as it was.</p>
      <h3>How</h3>
      <p>Press <b>Redraw…</b> beside the image. Drag to paint; right-drag, or the
      <b>Eraser</b>, takes paint away. Switch to <b>1:1</b> for detail and hold
      <b>Space</b> to drag the picture around. The mask shows only while the pointer
      is over the picture, so moving off it shows you the result clean. Type what
      should be there, or leave the box blank to redraw from the image\u2019s own
      prompt. The live preview shows the area forming; <i>Cancel generation</i>
      stops it and leaves the image as it was.</p>
      ${table(['Strength', 'What it does'], [
        ['0.4–0.6', 'Keeps the shape and colours, fixes the detail. The default, 0.5, is here.'],
        ['0.8', 'Replaces what was there, matched to its surroundings.'],
        ['0.9–0.95', 'Starts the area over. For removing or swapping an object.'],
      ])}
      <p><b>Edge softness</b> is how far past the paint the new pixels fade into the
      old. What you painted is always replaced in full. Raise it when a strong
      redraw leaves an edge; 0 is a hard one.</p>
      <h3>Going back</h3>
      <p>Every pass on an image — Redraw, Refine, Hi-res — is a step in its
      <b>History</b>, in the dialog. Click a step to go back to it (Ctrl+Z and
      Ctrl+Shift+Z step through). Fixed the fingers, then ruined the hat? Click the
      fingers step and redraw the hat again: the bad hat is replaced, the fingers
      are kept. An image keeps its last twelve steps and always its original.</p>
      <h3>Why small things come back sharper</h3>
      <p>The painted area and a margin around it are cut out and enlarged to the
      model\u2019s own size before they are redrawn, then shrunk back in. A face 80
      pixels wide is drawn at 1024, not at 80 — which is why a redraw can fix
      what a whole re-roll can\u2019t. The Outputs tab has no Redraw, because a
      saved output already has its sharpening in its pixels; pick it under
      <i>Image to image</i> to work on it.</p>`,
  },
  {
    id: 'stepcache',
    title: 'Step caching',
    html: `
      <h2>Step caching</h2>
      <p>Near the end of a render, one step often changes the picture very little.
      A step cache notices and skips the model for that step, reusing or
      predicting its output instead. Generate finishes sooner; the cost is that
      a skipped step is an estimate, not the real thing.</p>
      ${table(['Setting', 'What it does'], [
        ['Off', 'Every step is computed. The default.'],
        ['Spectrum', 'Fits a curve through the steps so far and predicts the next ones. Works on every model.'],
        ['Adaptive', 'Reuses the last result while the input has barely moved. UCache on SD 1.x, SD 2.x and SDXL, EasyCache on the others — picked for you, since each only works on its own kind of model.'],
      ])}
      <p><b>Only Generate is cached.</b> Refine and Hi-res always run every step:
      they are what you press for the best version of an image you have
      already chosen, so a cheap first draft is where the shortcut belongs.</p>
      <p><b>How to tell it did something:</b> under the timings, a line says how
      many steps it skipped. No line means it skipped none — a few-step model
      has no room to, which is a real answer rather than a fault.</p>
      <p>Try it with a fixed seed, Off against on, and compare both the time and
      the picture. The live preview shows a composition going wrong early, and
      <i>Cancel generation</i> throws that one away.</p>`,
  },
];

const KEY = 'rustimage.help.topic';

function show(id) {
  const topic = TOPICS.find((t) => t.id === id) ?? TOPICS[0];
  for (const b of document.querySelectorAll('#helpnav [data-topic]')) {
    b.classList.toggle('active', b.dataset.topic === topic.id);
  }
  $('helpdoc').innerHTML = topic.html;
  // Per browser, every access in a try/catch — the rule `initFolds` follows.
  try { localStorage.setItem(KEY, topic.id); } catch { /* ignore */ }
}

export function initHelp() {
  $('helpnav').innerHTML = TOPICS.map((t) =>
    `<button type="button" class="btn btn-choice" data-topic="${t.id}">${esc(t.title)}</button>`).join('');
  $('helpnav').addEventListener('click', (e) => {
    const b = e.target.closest('[data-topic]');
    if (b) show(b.dataset.topic);
  });
  let last = null;
  try { last = localStorage.getItem(KEY); } catch { /* ignore */ }
  show(last);
}
