import { $, esc } from './dom.js';
import { fetchCatalog, openGetModels } from './getmodels.js';

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
//
// A row may carry `fill(el)`, run each time the topic is shown, for a part that
// is the server's to know. *Intro to models* uses it for its family table, so
// the parts each family needs come from the same `/catalog` the Get models tab
// draws, not from a second copy written here.

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
    id: 'models',
    title: 'Intro to models',
    html: `
      <h2>Intro to models</h2>
      <p>A <b>model</b> is what draws the picture. It is a big file of learned
      numbers — 2 to 20 GB — and each one has its own look and its own idea of
      good settings. The <b>Get models</b> tab downloads a working set for each
      kind this app runs, so this page is for when you want to know what you are
      looking at, or go looking for more.</p>

      <h3>One file, or several</h3>
      <p>An <b>SDXL or SD 1.5 checkpoint</b> is one file with everything inside.
      Drop it in <code>models/checkpoints</code> and pick it.</p>
      <p>The newer models — <b>Flux, Klein, Z-Image, Krea 2, Qwen-Image</b> — come as
      a <b>diffusion model</b> plus separate <b>parts</b>:</p>
      ${table(['Part', 'What it does'], [
        ['Text encoder', 'Reads your prompt and turns it into something the model understands. Often a whole language model (Qwen3, T5), and often as large as the model itself.'],
        ['VAE', 'Turns the model’s compressed result into pixels at the end. Small, but it has to be the right one: the wrong VAE gives coloured noise.'],
      ], false)}
      <p>The parts must match the model <b>exactly</b>. A text encoder of the
      wrong size loads without complaint and quietly ignores your prompt. rustimage
      reads every file to find out what it is, so file names and folders don’t
      matter — but it can only pick from what you have. Each model’s parts are in
      the <i>Models</i> fold on the Image tab.</p>

      <h3>What each family needs</h3>
      <div id="helpfamilies"><p class="sub">Reading the catalog…</p></div>

      <h3>File types</h3>
      ${table(['Ending', 'What it is'], [
        ['.safetensors', 'The usual format. Often full precision (fp16 or bf16) — the largest version of a model.'],
        ['.gguf', 'A packed format that can be <b>quantized</b>: stored in fewer bits, so smaller and lighter on memory.'],
        ['.ckpt, .pt, .pth', 'Older formats. Fine for upscalers; prefer .safetensors for anything else.'],
      ])}
      <p><b>Quantization</b> is the <code>Q8_0</code>, <code>Q6_K</code>,
      <code>Q4_K</code> in a GGUF’s name: the bits per number. <b>Q8</b> looks the
      same as the original at half the size. <b>Q6</b> and <b>Q5</b> are very close.
      <b>Q4</b> is about a quarter of the size and starts to lose fine detail — the
      way to fit a 20B model on one card.</p>
      <p><b>Avoid fp8 files</b> on this app. AMD cards can’t compute in fp8, so an
      fp8 file runs at twice its size. The <b>Convert</b> tab turns one into a
      GGUF that runs at its real size.</p>

      <h3>Where to find more</h3>
      <p><b>Hugging Face</b> (<code>huggingface.co</code>) is where models are
      published: the original releases, and GGUF versions by people like
      <code>city96</code>, <code>leejet</code>, <code>unsloth</code> and
      <code>QuantStack</code>. A repository holds many files; open its
      <i>Files</i> tab and download <b>one</b> — the quantization that fits your
      card — not the whole thing. Some repositories ask you to log in and accept a
      licence first.</p>
      <p><b>Civitai</b> (<code>civitai.com</code>) is the community’s shelf:
      thousands of fine-tuned checkpoints and <b>LoRAs</b>, mostly SDXL, Pony,
      Illustrious and SD 1.5. Check the <i>Base model</i> on a page before
      downloading. Some downloads need a free account.</p>
      <p><b>Check the licence.</b> Every model, checkpoint and LoRA comes with
      its own terms, and they differ a lot: some allow anything, some forbid
      commercial use of the images, some are for research only, and some ask
      you to accept terms before downloading. The licence is on the model’s page
      (on Hugging Face, in the model card and the <i>License</i> tag; on Civitai,
      under the model’s permissions). A quantized copy — a GGUF someone else made —
      is still under the original model’s licence. That includes the files the
      Get models tab downloads; each card links their pages.</p>
      <p>A <b>LoRA</b> is a small add-on (10–300 MB) that teaches a model a style,
      a character or a detail. It only works on the family it was trained for —
      an SDXL LoRA does nothing on Flux — and the LoRA picker greys out the ones
      that don’t fit. They go in <code>models/loras</code>.</p>

      <h3>Where files go</h3>
      ${table(['Folder', 'What goes in it'], [
        ['models/checkpoints', 'One-file models: SDXL, SD 1.5, and their fine-tunes'],
        ['models/diffusion_models', 'Diffusion models that need parts: Flux, Klein, Z-Image, Krea 2, Qwen-Image'],
        ['models/text_encoders', 'Text encoders: Qwen, T5, CLIP-L'],
        ['models/vae', 'VAEs'],
        ['models/loras', 'LoRAs'],
        ['models/upscalers', 'Upscaler models (ESRGAN, SPAN, SwinIR…)'],
      ])}
      <p>These are the folders ComfyUI uses, so its download instructions work
      here. They are for you: rustimage reads every file under
      <code>models/</code>, so a model is found wherever it is — except that
      LoRAs and upscalers must be in their own folders.</p>`,
    async fill(el) {
      const box = el.querySelector('#helpfamilies');
      let cat;
      try {
        cat = await fetchCatalog();
      } catch (e) {
        box.innerHTML = `<p class="sub">Could not read the catalog: ${esc(String(e))}</p>`;
        return;
      }
      if (!box.isConnected) return;
      const gb = (b) => (b / 1e9).toFixed(1) + ' GB';
      const parts = (e) => e.files.map((f) => f.slot_label).join(' + ');
      // Done reads green and needs no button; missing reads amber, the colour
      // that says something is still to be done, with the button under it.
      const state = (e) => {
        const n = e.files.filter((f) => f.state !== 'have').length;
        return n
          ? `<span class="famstate missing">${n} file${n > 1 ? 's' : ''} missing</span>`
            + `<br><button type="button" class="btn spaced" data-family="${e.family}">Get it</button>`
          : '<span class="famstate done">✓ all here</span>';
      };
      box.innerHTML = table(['Family', 'Needs', 'Recommended set', ''], cat.entries.map((e) => [
        esc(e.family_label),
        esc(parts(e)),
        `${esc(e.label)}, ${gb(e.bytes)}<br><span class="sub">${esc(e.note)}</span>`,
        state(e),
      ]), false) + '<p class="sub">SD 1.5 and SD 2 checkpoints also run: one file each, from Civitai.</p>';
      box.addEventListener('click', (ev) => {
        const b = ev.target.closest('[data-family]');
        if (b) openGetModels(b.dataset.family);
      });
    },
  },
  {
    id: 'about',
    title: 'About & licences',
    html: `
      <h2>About & licences</h2>
      <p><b>rustimage</b> is free software under the
      <a href="https://www.apache.org/licenses/LICENSE-2.0" target="_blank" rel="noopener noreferrer">Apache License 2.0</a>.</p>
      <p>It is powered by
      <a href="https://github.com/leejet/stable-diffusion.cpp" target="_blank" rel="noopener noreferrer">stable-diffusion.cpp</a>
      (MIT licence), which this program carries inside itself and runs to draw every
      picture, and by <a href="https://www.vulkan.org/" target="_blank" rel="noopener noreferrer">Vulkan</a>,
      which your graphics driver provides.</p>
      <h3>Models</h3>
      <p>Models are not part of rustimage. <b>Every model has its own licence</b>, set by
      the people who made it: some allow anything, some forbid commercial use of what
      you make. Check a model’s page before using its pictures — each card on the
      <i>Get models</i> tab links the pages its files come from.</p>
      <h3>Third-party notices</h3>
      <p>The components built into this program, and the licence notices they ask to
      travel with every copy:</p>
      <pre class="licences" id="licencetext">Loading…</pre>`,
    async fill(el) {
      const box = el.querySelector('#licencetext');
      try {
        const text = await (await fetch('/licences')).text();
        if (box.isConnected) box.textContent = text;
      } catch (e) {
        box.textContent = `Could not load the notices (${e}). They are also in THIRD_PARTY_LICENSES.md beside the source.`;
      }
    },
  },
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
      <p>The one place an image is finished. Press <b>Refine…</b> beside the image, or click the image.
      The dialog shows the picture at Fit or <b>1:1</b> (hold <b>Space</b> to drag
      it around), with a prompt box at the top, a section per tool (Refine, Edit,
      Upscale, Extend, History) that folds away, and the live preview and the
      Refine button at the bottom.</p>
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
      <p><b>Rescale with Edit</b> does the same with a new shape: pick sides (← ↑ ↓ →)
      and an amount, and the button becomes <b>Edit and rescale</b>. The model
      redraws the whole picture to fill the bigger frame — "the same room, wider",
      "show the rest of the table". It is a re-imagining: nothing is pasted back,
      so for a picture that must stay exactly as it is, use Extend.</p>
      <h3>Extending</h3>
      <p><b>Extend</b> makes the canvas bigger on the sides you pick and draws what
      lies beyond them. It works on any image, whatever made it, and keeps every
      pixel you already have — only the new strips are drawn, and Edge softness
      blends the join. Each side is its own pass at the drawing model’s full size,
      left and right first, so picking all four draws the corners too.</p>
      <p>It uses <b>FLUX.1 Fill</b>, a model trained for exactly this, and turns on
      by itself once one is installed (Get models → FLUX.1 Fill). With a blank
      prompt it continues the background; the image’s own prompt would name
      the subject, and Fill would draw it again in the strip. Type a prompt to put
      something particular there — "a window", "more of the beach".</p>
      <h3>Comparing</h3>
      <p><b>Compare</b> (or <b>C</b>) shows an earlier step on the left and the
      image now on the right, split by a line you drag — or click to move it. Pick
      the earlier step under <i>Compare with</i>; it starts on the one just before.
      Both halves carry the same grade and sharpening, so what differs is what the
      pass did. A smaller earlier step of the same shape is stretched to the same
      size, which is how to judge an upscale; one of another shape (before an
      Extend) is shown whole inside the frame. Painting waits while comparing.</p>
      <h3>Going back</h3>
      <p>Every pass is a step in the <b>History</b>. Click a step to go back to it
      (Ctrl+Z and Ctrl+Shift+Z step through). A new pass from an earlier step
      replaces the ones after it. An image keeps its last twelve steps and always
      its original.</p>
      <h3>In a batch</h3>
      <p><i>After each generation</i> runs Hi-res and Refine on every image, with the
      prompts from the Hi-res pass and Refine pass panels. A <code>{variable}</code>
      in them rolls per image.</p>
      <h3>Saved pictures and uploads</h3>
      <p>In the Outputs viewer, <b>Refine &amp; extend…</b> opens the picture here
      as the current image, with this dialog open: refine, extend, edit or
      upscale it like a fresh one. It opens the saved file, which already has its
      sharpening and grade in it, so the sliders start at zero; save slider changes
      first if you want them kept. <b>Refine with</b>, at the top of the dialog,
      picks the model its passes run on: it starts at the model that made the
      picture, or for an upload the one picked on the Image tab.</p>`,
  },
  {
    id: 'memory',
    title: 'Memory',
    html: `
      <h2>Memory</h2>
      <p>A run needs two kinds of memory. <b>GPU memory</b> (VRAM, 24 GB on a
      7900 XTX) holds the work of drawing — the picture-in-progress, attention —
      and whatever weights are on the card. <b>System memory</b> (RAM) holds the
      weights that aren’t. A big model with its text encoder can be 20 GB or more,
      so where those weights live is what decides whether a run fits.</p>

      <h3>The Memory setting</h3>
      <p>In the <i>Models</i> fold, per model (<i>Save as defaults for this
      model</i> keeps it), and on the Video tab.</p>
      ${table(['Setting', 'What it does'], [
        ['Auto', 'stable-diffusion.cpp measures free memory and places each part: on the card first, then in RAM, then read from the SSD. The default, and right for anything that fits.'],
        ['System RAM', 'Every weight waits in RAM and moves to the card a piece at a time, so the whole card is free for the drawing. <b>The fix when a big model runs out of GPU memory.</b> The text encoder is read from its file, used for the prompt and then let go, so it doesn’t hold RAM while the picture is drawn. Needs RAM about the size of the model; a little slower.'],
        ['Stream from SSD', 'The model and its text encoder are re-read from their files as they’re needed. The least memory of the three — for when RAM is what runs out. Slower. It only reads, so it doesn’t wear the drive.'],
      ], false)}
      <p><b>System RAM has a second limit.</b> The graphics card reaches system RAM
      through a window the Linux driver sizes at half your RAM by default (the
      <i>GTT</i> — about 15 GB on a 30 GB machine). Weights kept in RAM have to
      fit in that window, however much RAM is free. A full-precision 9B model
      (17 GB) doesn’t, and the run stops with “device lost”; the error box then
      offers Stream from SSD, which goes from the file straight to the card. The
      window can be made larger with a kernel setting (<code>ttm.pages_limit</code>),
      or a Q8 file of the model fits it as it is.</p>
      <p>Auto guesses how much room the drawing will need and fills the rest of
      the card with weights. A 9B model at two megapixels can need more than
      the guess, which is when System RAM helps.</p>

      <h3>Can the text encoder’s result be kept?</h3>
      <p>Not between runs. stable-diffusion.cpp can only remember a prompt’s
      encoding inside one running process, and rustimage starts a fresh one for
      every pass. What it does instead, in System RAM and Stream from SSD, is
      let go of the encoder as soon as it has read the prompt — the encoder is
      the second-largest file in most sets, and it is only needed for the first
      second or two.</p>

      <h3>When a run runs out</h3>
      <p>The error says which memory ran out and when — loading the model,
      drawing (with the step), or turning the result into pixels — and offers
      what fits: <b>System RAM</b>, <b>Stream from SSD</b>, a <b>smaller
      size</b>, or a <b>smaller file</b>. Pick one, then <b>Try again</b> re-runs
      exactly what failed. Nothing is saved until you press <i>Save as
      defaults</i>.</p>
      <p>System RAM comes with its figure — <i>needs about 17.6 GB of RAM;
      26.0 GB is free</i> — counted from the model’s own files. When it won’t fit
      with room to spare, it is shown greyed out and Stream from SSD is offered
      instead: a full-precision 9B can need more RAM than a 32 GB machine has
      free.</p>
      <p><b>“Device lost”</b> is a different failure: stable-diffusion.cpp’s own
      check said the work fits, and the graphics driver still had no room for
      it, because the desktop and the browser use the card too. The box then
      offers to <b>keep 3 GB of the card free</b>. That is a setting for this
      machine, not a model, so it is saved at once (Settings → <i>Keep free on
      the GPU</i>) and applies to every run after.</p>
      <p>Running out <b>while turning the result into pixels</b> is the VAE.
      stable-diffusion.cpp already retries that in tiles by itself, so what is
      left is a smaller size.</p>
      <p>Running out of <b>system memory</b> looks different: the system stops the
      run with no message, and rustimage says so. Close what you don’t need, or
      use Stream from SSD.</p>

      <h3>The lasting fix: a smaller file</h3>
      <p>A <b>Q8</b> GGUF needs about half the memory of the original, <b>Q6</b>
      a bit less again, and <b>Q4</b> about a quarter — with Q8 indistinguishable
      and Q4 starting to lose fine detail. <b>fp8 files run at twice their
      size</b> on AMD cards. The <b>Get models</b> tab has GGUF sets for every
      family, and the <b>Convert</b> tab turns a file you have into one.</p>

      <h3>After a run</h3>
      <p>When any weights weren’t on the card, a line under the timings says
      where they were — <i>text encoder in system RAM</i>, <i>model streamed from
      the SSD</i>. That explains a slow run before it becomes a failed one.</p>`,
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

// The menu's order: these first, in this order, then every other topic by
// title — so a topic added later lands in its alphabetical place with no edit
// here, and `TOPICS` can stay in whatever order reads best in the source.
const PINNED = ['about', 'models'];
export function menuOrder() {
  const pinned = PINNED.map((id) => TOPICS.find((t) => t.id === id)).filter(Boolean);
  const rest = TOPICS.filter((t) => !PINNED.includes(t.id))
    .sort((a, b) => a.title.localeCompare(b.title));
  return [...pinned, ...rest];
}

function show(id) {
  const topic = TOPICS.find((t) => t.id === id) ?? menuOrder()[0];
  for (const b of document.querySelectorAll('#helpnav [data-topic]')) {
    b.classList.toggle('active', b.dataset.topic === topic.id);
  }
  $('helpdoc').innerHTML = topic.html;
  topic.fill?.($('helpdoc'));
  // Per browser, every access in a try/catch — the rule `rememberFolds` follows.
  try { localStorage.setItem(KEY, topic.id); } catch { /* ignore */ }
}

// Open the Help tab on one topic — for the footer's Licences link.
export function openHelp(id) {
  document.querySelector('.tab[data-tab="help"]')?.click();
  show(id);
}

export function initHelp() {
  $('helpnav').innerHTML = menuOrder().map((t) =>
    `<button type="button" class="btn btn-choice" data-topic="${t.id}">${esc(t.title)}</button>`).join('');
  $('helpnav').addEventListener('click', (e) => {
    const b = e.target.closest('[data-topic]');
    if (b) show(b.dataset.topic);
  });
  let last = null;
  try { last = localStorage.getItem(KEY); } catch { /* ignore */ }
  show(last);
}
