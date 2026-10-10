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
    id: 'references',
    title: 'References',
    html: `
      <h2>References</h2>
      <p>Pictures the model looks at while it draws, so it can keep a person, a
      piece of clothing or a place from your own images. With a model that takes
      them — <b>Flux.2 Klein</b> for now — a <b>References</b> button appears in
      the Generation section. Pick up to four images there (Choose, Upload, drag
      one in, or paste), then name them in the prompt by number:</p>
      <p><i>"the woman from image 1 wearing the jacket from image 2, walking
      through a market"</i></p>
      <p>Each reference costs about as much as the image itself, so four make a
      generation noticeably slower; large photos are shrunk to the model\u2019s size
      first. The picks stay when you switch to a model that doesn\u2019t use them,
      and come back when you switch back. Reopening a work item puts its
      references back in their slots.</p>
      <p>To change an image you already have, use <b>Edit</b> in the Refine dialog:
      it uses the image itself as reference 1.</p>`,
  },
  {
    id: 'redraw',
    title: 'Refine',
    html: `
      <h2>Refine</h2>
      <p>The one place an image is finished. Press <b>Refine…</b> beside the image.
      The dialog shows the picture at Fit or <b>1:1</b> (hold <b>Space</b> to drag
      it around), with a prompt box, the live preview and the image\u2019s History.</p>
      <h3>Whole image or an area — the mask decides</h3>
      ${table(['', 'What happens'], [
        ['Nothing painted', '<b>Refine image</b>: a light pass over the whole picture, at the Refine pass\u2019s steps and strength. Good for detail and texture.'],
        ['Something painted', '<b>Refine area</b>: only the painted part is redrawn, enlarged to the model\u2019s size first, then blended back in. Every pixel outside it stays exactly as it was.'],
      ])}
      <p><b>Upscale</b> makes the image bigger with no new drawing at all: FSR
      (instant) or an upscaler model from your upscalers folder (slower, sharper).
      Because it draws nothing new it can\u2019t undo a refine, so it can be the last
      step — a bigger file — or come before a refine, which then adds detail at the
      new size. It waits while something is painted, and it is a History step like
      the rest.</p>
      <p><b>Hi-res comes first</b>, from the button above the image: it sets the
      final size and re-draws the whole picture lightly on the way, which would
      partly undo a refine done before it. Enlarge, then refine.</p>
      <p>Drag to paint, right-drag or the <b>Eraser</b> to take paint away. The mask
      shows only while the pointer is over the picture, so moving off it shows the
      result clean. <b>Edge softness</b> is how far past the paint the new pixels
      fade into the old; what you painted is always replaced in full.</p>
      <h3>The prompt</h3>
      <p>Each pass can be asked for something of its own — "sharper fabric
      texture", "an open hand". Blank uses, in order: the Refine pass or Hi-res pass
      prompt from the panel on the left, then the image\u2019s own. (Hi-res uses the
      Hi-res pass panel\u2019s prompt.) The image keeps
      its prompt either way; the History says what each pass was asked.</p>
      <p>A whole-image refine is light (strength 0.2–0.35), so a prompt there brings
      out detail rather than changing what is in the picture. "Make it a red dress"
      is a painted area, or a stronger refine.</p>
      ${table(['Area strength', 'What it does'], [
        ['0.4–0.6', 'Keeps the shape and colours, fixes the detail. The default, 0.5, is here.'],
        ['0.8', 'Replaces what was there, matched to its surroundings.'],
        ['0.9–0.95', 'Starts the area over. For removing or swapping an object.'],
      ])}
      <h3>Editing</h3>
      <p>With a Flux.2 Klein image, <b>Edit</b> changes the whole picture by
      instruction, with no painting: type what to change — "make it night",
      "remove the hat", "turn the jacket red" — and press Edit. The image goes to
      the model as reference 1, so it keeps the picture and changes what you
      asked. Pictures picked under <i>References</i> go along as image 2 onwards.
      It is a History step like any other, so Compare shows what changed and
      Ctrl+Z takes it back. It draws at the model\u2019s own size (1024 for Klein)
      and scales up to fit, so on a large image a <b>Refine image</b> afterwards
      brings the fine detail back.</p>
      <h3>Comparing</h3>
      <p><b>Compare</b> (or <b>C</b>) shows an earlier step on the left and the
      image now on the right, split by a line you drag — or click to move it. Pick
      the earlier step under <i>Compare with</i>; it starts on the one just before.
      Both halves carry the same grade and sharpening, so what differs is what the
      pass did. A smaller earlier step is stretched to the same size, which is how to
      judge an upscale. Painting waits while comparing.</p>
      <h3>Going back</h3>
      <p>Every pass is a step in the <b>History</b>. Click a step to go back to it
      (Ctrl+Z and Ctrl+Shift+Z step through). A new pass from an earlier step
      replaces the ones after it. An image keeps its last twelve steps and always
      its original.</p>
      <h3>In a batch</h3>
      <p><i>After each generation</i> runs Hi-res and Refine on every image, with the
      prompts from the Hi-res pass and Refine pass panels. A <code>{variable}</code>
      in them rolls per image.</p>
      <p>The Outputs tab has no Refine, because a saved output already has its
      sharpening in its pixels; pick it under <i>Image to image</i> to work on it.</p>`,
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
