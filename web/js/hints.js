import { $ } from './dom.js';
import { FILTERS } from './viewer.js';

// Every control the page explains, keyed by what the control *is* rather than by
// where it lives: Steps is Steps on three tabs, and three copies of the sentence
// would be three chances for two of them to go stale.
//
// The text says how to *choose* a value rather than restating the field's name,
// and where the answer belongs to the checkpoint it says so in those words —
// that a number is a property of the model and not of image generation is the
// single thing the panel was failing to tell anyone. A few sentences each: this
// is a tooltip, not the README.
// The weighting shortcut belongs to the prompt *box*, not to any one of the six
// controls in one, so it is a string spliced into all three entries rather than
// three sentences free to drift apart — the same rule the comment above states,
// applied one level down.
// The same sentence on both prompt boxes, for the same reason `HELP` is keyed
// by what a control is: a variable works in either of them, and two copies of
// this would be two chances to go stale.
const VARIABLES =
  ' A word in braces is a variable — {animal} draws one of the words you listed '
  + 'for it in Settings, per image, so a batch run varies its subject and not '
  + 'just its seed. Case is ignored, and a name nothing answers to is reported '
  + 'rather than drawn as words.';

const WEIGHTING =
  ' Put the caret in a word or select a phrase and press Ctrl+↑ / Ctrl+↓ (or '
  + 'hold Ctrl and roll the wheel) to weight it in steps of 0.1: the text '
  + 'rewrites itself as (word:1.2), and it disappears again at 1.0.';

const HELP = [
  { ids: ['prompt', 'vprompt'],
    text: 'What you want to see. Subject first, then style, lighting and lens; '
        + 'commas separate ideas and the earlier words carry more weight. A list '
        + 'of tags works as well as a sentence — these models were trained on both.'
        + VARIABLES + WEIGHTING },
  { ids: ['negative'],
    text: 'What to steer away from. It works on flaws that keep turning up — '
        + 'watermark, extra fingers, blurry — rather than on objects you simply '
        + 'did not ask for, which are already absent. It only works when CFG is '
        + 'not 1: turbo models run at 1 and ignore it, which is why the box is hidden there.'
        + VARIABLES + WEIGHTING },
  { ids: ['vnegative'],
    text: 'What to steer away from. Pre-filled with the model’s own published '
        + 'negative prompt, which for Wan is 400 characters of Chinese nobody '
        + 'should have to retype. Emptying the box means no negative at all.'
        + WEIGHTING },
  { ids: ['template', 'vmodel'],
    text: 'Which model does the work. Every model has its own house style '
        + 'and its own sensible steps, CFG and size — picking one refills the '
        + 'boxes below with the values it was measured at, and on the Image tab '
        + 'shows the separate files it runs with (a VAE, a text encoder).' },
  { ids: ['part-vae', 'pmpart-vae'],
    text: 'Turns the finished latent into pixels. A checkpoint that carries its '
        + 'own uses it when this says "built into the checkpoint"; pick a file to '
        + 'replace it — the usual reason on SDXL is the fp16-fix VAE, since the '
        + 'original overflows at half precision. Only VAEs this model can decode '
        + 'are listed.' },
  { ids: ['part-llm', 'pmpart-llm'],
    text: 'The language model that reads your prompt for this model. Z-Image is '
        + 'trained against Qwen3 4B and nothing else will do, so only files of '
        + 'that size are listed.' },
  { ids: ['seed', 'vseed'],
    text: 'The random noise the picture is grown from, as a number. The same seed '
        + 'with the same settings gives the same image, so it is how you keep a '
        + 'result and change one thing about it. -1 draws a new one every time.' },
  { ids: ['steps', 'vsteps'],
    text: 'How many passes the sampler takes to turn noise into an image. More '
        + 'costs proportionally more time and stops helping after a point — and '
        + 'the right number belongs to the checkpoint: turbo and LCM models are '
        + 'trained to land in 1–4, a standard SDXL wants around 20. On the Image '
        + 'tab a pass over a picked image runs these too: all of them, with '
        + 'Strength deciding how far up the schedule they start.' },
  { ids: ['cfg', 'vcfg'],
    text: 'How hard the model is pushed towards your prompt. Low is loose and '
        + 'soft, high follows the words but goes harsh and over-saturated; 5–7 '
        + 'suits a standard SDXL. Turbo and LCM checkpoints are trained for CFG 1 '
        + 'and scorch at anything higher. On the Image tab it is '
        + 'also the CFG of a pass over a picked image.' },
  { ids: ['sampler', 'vsampler'],
    text: 'The algorithm that removes the noise. They differ mostly in how '
        + 'quickly they settle: euler is the safe default, dpm++2m converges in '
        + 'fewer steps, and lcm belongs to LCM and turbo checkpoints and looks '
        + 'wrong on anything else.' },
  { ids: ['loras'],
    text: 'Small files that patch the checkpoint’s weights — a style, a '
        + 'character, a 4-step distillation. They come from models/loras/, and '
        + 'each one has to have been trained for the base model you picked above '
        + '(an SDXL LoRA does nothing to an SD1.5) — if it doesn’t match, the '
        + 'image just comes back unchanged. Strength is the multiplier: 1 is what '
        + 'it was trained at, 0.5 is a hint of it, and negative subtracts. They '
        + 'carry into Refine and Hi-res, and a different Refine/hi-res model '
        + 'clears them, since a LoRA belongs to one base.' },
  { ids: ['w', 'h'],
    text: 'The size the model actually paints at, before any upscale. Diffusion '
        + 'models only compose correctly near the size they were trained on — '
        + '1024×1024 for SDXL, 512 for SD1.5 — and drifting far from it gives '
        + 'duplicated heads and stretched bodies. Go bigger with the hi-res pass, '
        + 'not here. Multiples of 64.' },
  { ids: ['rsteps'],
    text: 'Refine re-diffuses the finished image at the same size, which sharpens '
        + 'and fills in detail the first pass only suggested. This is how many '
        + 'steps it gets; 0 removes the button.' },
  { ids: ['rstrength'],
    text: 'How far into the noise the refine pass starts, and so how much it is '
        + 'allowed to change. Around 0.3 tightens what is already there; past '
        + '0.6 it starts redrawing the subject. 0.95 is the ceiling because at '
        + '1.0 the pass would throw your image away and start over.' },
  { ids: ['hsteps'],
    text: 'The hi-res pass enlarges the image and re-diffuses it, which is where '
        + 'real detail at the bigger size comes from — an upscale alone can only '
        + 'interpolate what is there. This is its step count; 0 removes the button.' },
  { ids: ['hstrength'],
    text: 'How far the hi-res pass may move the enlarged image. Low (~0.3) keeps '
        + 'the composition and adds texture; high invents new detail and can lose '
        + 'the likeness of what you had.' },
  { ids: ['rcfg'],
    text: 'CFG for the refine pass alone. Empty follows the CFG scale box — '
        + 'or, with a model picked for this pass, that model\'s own CFG. A lower '
        + 'value here is the common trick: an image that already exists needs '
        + 'less pushing than one being invented.' },
  { ids: ['hcfg'],
    text: 'CFG for the hi-res pass alone. Empty follows the CFG scale box — '
        + 'or, with a model picked for this pass, that model\'s own CFG. It is a '
        + 'separate number from the refine one because the two passes work on different pixels — hi-res is inventing detail '
        + 'at a new size, refine is tightening what is already there.' },
  { ids: ['refmodel', 'hiresmodel'],
    text: 'The checkpoint this one pass uses, which need not be the one that '
        + 'drew the image: draft on a fast turbo model, finish on a detailed '
        + 'one. Picking it fills the boxes under it with its own steps, strength '
        + 'and CFG, which you can then change. Costs no VRAM to '
        + 'name a second — every pass is a separate sd-cli process that loads '
        + 'and frees its own weights — only the disk read if the file has fallen '
        + 'out of the page cache.' },
  { ids: ['upscale'],
    text: 'How much this render is enlarged by FSR on its way to the output '
        + 'size — it is why a 768 model is listed as 768×768 → 1536×1536. Not a '
        + 'diffusion pass: it invents nothing, and it is fast and '
        + 'lossless-looking up to about 2×. Set it to 1 and the pass is skipped '
        + 'entirely, which is how you see what the checkpoint actually drew; to '
        + 'add real detail at the larger size, use the hi-res pass instead.' },
  { ids: ['upscaler'],
    text: 'What does that enlarge. FSR is instant and only redistributes the '
        + 'pixels the checkpoint drew. A model (a .pth or .safetensors in '
        + 'models/upscalers/) draws new fine texture instead. ESRGAN (4x-UltraSharp, '
        + 'RealESRGAN_x4plus) and Compact take seconds, SPAN about the same, SwinIR '
        + 'longer; HAT and DAT are listed but not supported. Models are fixed at '
        + '2× or 4×; a 4× model asked for 2× is enlarged 4× and then reduced, which '
        + 'is the normal way to use one. The size that comes out is the same either way.' },
  { ids: ['count'],
    text: 'How many images this run makes. Each is a separate request, so results '
        + 'appear as they land and Stop only has to wait for the one in flight.' },
  { ids: ['hiresscale'],
    text: 'How much the hi-res pass enlarges, when it runs, from 1 to 4 — 2.5 is '
        + 'fine. The same factor as the box above a finished image, which does not '
        + 'exist yet before the first render — hence a second copy of it here.' },
  { ids: ['autorun'],
    text: 'Ignore the count and keep drawing new seeds until you press Stop. For '
        + 'leaving a prompt running and picking from the gallery afterwards.' },
  { ids: ['aftereach'],
    text: 'What happens to each image once it is drawn, without you pressing the '
        + 'buttons. All three are off by default: a run that quietly refined and '
        + 'saved everything would be spending GPU time nobody asked for.' },
  { ids: ['opthires'],
    text: 'Run the hi-res pass on every image of the run. Roughly doubles the '
        + 'time per image, and is what makes an unattended run produce finished '
        + 'pictures rather than drafts.' },
  { ids: ['optrefine'],
    text: 'Run refine on every image of the run. With the hi-res pass on too, it '
        + 'runs after it — refining the bigger picture, not the draft.' },
  { ids: ['optsave'],
    text: 'Copy every image of the run into work/output/. Everything is kept in '
        + 'the gallery either way; this is the folder to point other programs at, '
        + 'and the pool Start from and the video tab\'s first frame pick from.' },
  { ids: ['source'],
    text: 'Nothing, to draw from noise — or a picture to start from instead: '
        + 'Choose picks one you saved, and Upload, a drop or a paste brings one '
        + 'in (it lands in work/output/ beside your saves). With a picture, '
        + 'Generate redraws it with your prompt, and Strength decides how much. '
        + 'The original is copied, never worked on in place.' },
  { ids: ['srcstrength'],
    text: 'How far the model may travel from your source image. 0.2 is a retouch, '
        + '0.5 a reinterpretation, and 0.95 keeps little more than the layout. It '
        + 'stops there because at 1.0 the source is discarded entirely and this '
        + 'becomes a plain text-to-image.' },
  { ids: ['srcmaxside'],
    text: 'The longest side the source is scaled to before diffusing. A phone '
        + 'photo is 4032 px wide and would run out of VRAM at full size. Images '
        + 'are only ever shrunk, never enlarged, and the aspect ratio is kept. '
        + 'It is the same budget the hi-res pass works to: that pass diffuses no '
        + 'larger than this and covers anything beyond it with an FSR upscale.' },
  { ids: ['vfirstframe'],
    text: 'The still the clip animates, chosen from work/output/. Leave it empty '
        + 'and the same model runs as plain text-to-video; set it and the first '
        + 'frame is yours, which is by far the most reliable way to control what '
        + 'the clip contains.' },
  { ids: ['vw', 'vh'],
    text: 'The clip’s frame size. Wan publishes 832×480 and 1280×720, and going '
        + 'far past what a model was trained on costs VRAM quickly for little '
        + 'gain. Rounded up to what the model’s latent grid allows.' },
  { ids: ['vframes'],
    text: 'How many frames to generate — divided by fps, that is the length. The '
        + 'model’s latent grid only permits certain counts (Wan works in 4n+1), '
        + 'so the number is rounded for you and the line below says what you will '
        + 'actually get.' },
  { ids: ['vfps'],
    text: 'How fast the finished frames are played back. It does not change how '
        + 'much motion is generated, only how long the same frames take to show. '
        + 'Some families publish a fixed rate, and the box then follows it.' },
  { ids: ['vhsteps'],
    text: 'The A14B models are a pair of experts: a high-noise one that blocks '
        + 'out the motion and composition, then a low-noise one that finishes the '
        + 'picture. This is the first one’s step count; the Steps box above is '
        + 'the second’s.' },
  { ids: ['vhcfg'],
    text: 'CFG for the high-noise expert, which is where the composition and the '
        + 'motion are decided — so it is often set higher than the finishing '
        + 'pass, where a hard push only adds harshness.' },
  { ids: ['vshift'],
    text: 'Flow shift moves where the schedule spends its time. Higher spends '
        + 'longer at the noisy end, which stabilises large motion and camera '
        + 'moves; lower keeps fine texture. Wan’s own recommendation is around 5 '
        + '— move it a step at a time.' },
  { ids: ['vtiling'],
    text: 'Decode the frames in tiles instead of all at once. The VAE is the '
        + 'memory-hungry end of a clip, so this is the first thing to reach for '
        + 'when the render finishes and the *decode* runs out of VRAM. It costs '
        + 'time, not quality.' },
  { ids: ['vsharp'],
    text: 'Sharpening applied to every enlarged frame. It is a fixed kernel — the '
        + 'same function of the same pixels every time — so it cannot flicker '
        + 'between frames the way a per-frame diffusion would. 0 is off.' },
  { ids: ['vclarity'],
    text: 'Local contrast on every enlarged frame: a wide band, so it reads as '
        + 'depth rather than as edge sharpening. Negative softens. Fixed kernel '
        + 'again, so it stays stable in motion.' },
  { ids: ['csource'],
    text: 'The file to requantize. This list is what was found in your models '
        + 'folder, and each file’s role — transformer, VAE, text encoder — is '
        + 'read out of its header rather than guessed from its name, which is '
        + 'what stops a 15 GB text encoder being converted as a transformer.' },
  { ids: ['ctype'],
    text: 'The weight type to write. Smaller is not only smaller: Vulkan cannot '
        + 'multiply fp8 at all, so an fp8 file is cast to BF16 while it runs and '
        + 'needs about twice its size in memory. q4_K is the first type on this '
        + 'list the GPU can use directly.' },

  { ids: ['setvars'],
    text: 'Named lists of words that a prompt can draw from: call one "animal", '
        + 'give it "crab, horse, dog", and {animal} in any prompt becomes one of '
        + 'them — a different one per image, so Generate 20 is twenty subjects. '
        + 'Separate the words with commas; spaces inside a word are fine. Braces '
        + 'rather than [brackets] because [cat] already means something to the '
        + 'sampler: it de-emphasises the word. The draw follows the seed, so the '
        + 'same seed still gives the same picture.' },

  // The three settings with no control anywhere else to be the default *for*.
  // They were only ever reachable by editing models.toml by hand, which is what
  // moving `[defaults]` into a form had to stop being the answer to. There were
  // four: `upscale_factor` has a box on the Image tab now, so its sentence is up
  // with the rest of that panel's under `upscale`.
  { ids: ['setunet'],
    text: 'Where every SD 1.x, SD 2.x and SDXL checkpoint starts. Other families '
        + 'start from their own defaults instead — a Z-Image turbo wants 8 steps '
        + 'at CFG 1, and 25 steps at CFG 6 would be a wrong answer for it — so '
        + 'these do not reach them. To change one model, use Per model.' },
  { ids: ['setpermodel'],
    text: 'Where one model starts, when that should differ from the rest: a '
        + 'turbo SDXL at 6 steps and CFG 1, say, which its file cannot tell us. '
        + 'An empty box starts where its placeholder says, and a box set to the '
        + 'same number is dropped on save, so changing the settings above still '
        + 'reaches everything this model has no opinion about. Saved to '
        + 'user/model-settings.toml. The Image tab’s “Save as defaults for this '
        + 'model” writes here too.' },
  { ids: ['setclearwi'],
    text: 'When you press Close server, delete every work item on the Current '
        + 'shelf. Favorites are never touched. Only Close server does this — '
        + 'closing the window or pressing Ctrl+C leaves everything in place.' },
  { ids: ['setclearout'],
    text: 'When you press Close server, delete every image in work/output/ — '
        + 'saved images and uploads alike. Favorite outputs are never touched. '
        + 'Only Close server does this.' },
  { ids: ['setclearvid'],
    text: 'When you press Close server, delete every clip on the Video tab, and '
        + 'the enlarged frames made from them. Only Close server does this.' },
  { ids: ['setflash'],
    text: 'Pass --diffusion-fa to the refine and hi-res passes when they run '
        + 'large. Worth about 6× at 2048² on this card (122 s → 20 s), so leave '
        + 'it on unless your GPU or driver cannot do it.' },
  { ids: ['setwtype'],
    text: 'Quantize the weights as they load, e.g. q8_0 or q4_K — sd-cli’s '
        + '--type. It trades a little quality for memory and, on a backend that '
        + 'cannot multiply the stored type, for speed. Leave it empty to run the '
        + 'file as it is on disk. The Convert tab does the same thing once, '
        + 'permanently, instead of on every load.' },
  { ids: ['setargs'],
    text: 'Flags passed straight to sd-cli, one per line, after everything '
        + 'rustimage sets. The escape hatch for a model that needs something '
        + 'this page has no box for — and the place a wrong flag shows up as a '
        + 'failed render rather than as a message, since nothing here checks '
        + 'them.' },
];

// id → text. Two tables declare a control — `HELP` for the form fields, and
// `FILTERS` for the viewer's sliders, which carry their own — and this is the
// one index both end up in, so `wireHints` has a single thing to look in.
const HINTS = new Map();
for (const h of HELP) for (const id of h.ids) HINTS.set(id, h.text);
for (const f of FILTERS) if (f.help) HINTS.set(f.id, f.help);

// Put an (i) after every label that has something to say, rather than writing
// fifty-odd of them into the markup by hand. The key is the control's id — from
// `for`, or from the input a checkbox label wraps — so the tables above are
// keyed by the same names as everything else on the page. `data-help` is for the
// few headings that label a group rather than a single control.
//
// Idempotent, because it runs again over markup built after load (`renderClip`).
export function wireHints(root = document) {
  for (const label of root.querySelectorAll('label')) {
    if (label.querySelector('.hint')) continue;
    const key = label.dataset.help || label.htmlFor
      || label.querySelector('input, select, textarea')?.id;
    const text = HINTS.get(key);
    if (!text) continue;
    const b = document.createElement('button');
    b.type = 'button';   // the default is submit, and these are in no form by luck
    b.className = 'hint';
    b.textContent = 'i';
    b.setAttribute('aria-label', `What is ${label.textContent.trim()}?`);
    b.dataset.tip = text;
    b.addEventListener('pointerenter', () => showTip(b));
    b.addEventListener('pointerleave', hideTip);
    b.addEventListener('focus', () => showTip(b));
    b.addEventListener('blur', hideTip);
    b.addEventListener('click', (e) => {
      // Inside a <label>, a click anywhere activates the labelled control — so
      // without this, asking what "Keep going until stopped" means switches it on.
      e.preventDefault();
      showTip(b);
    });
    label.appendChild(b);
  }
}

// The bubble is a `popover`, which is what puts it in the browser's top layer,
// and that is the only reason it works in the two places it must: the viewer is
// a modal <dialog>, so an ordinary fixed element renders *behind* the backdrop,
// and `.lbside` is `overflow-y: auto`, so an ordinary absolute one is clipped by
// the panel the sliders live in. Plain `popover` rather than `popover="manual"`
// also buys Escape and click-outside dismissal, and makes a second icon's text
// replace the first rather than stack on top of it.
function showTip(btn) {
  const tip = $('tip');
  tip.textContent = btn.dataset.tip;
  // Measured after it is shown, because a closed popover has no size — and off
  // at the origin first, so the measurement is of the wrapped text rather than
  // of text squeezed against the right edge of the window.
  tip.style.left = '0px';
  tip.style.top = '0px';
  if (!tip.matches(':popover-open')) tip.showPopover();
  const a = btn.getBoundingClientRect();
  const t = tip.getBoundingClientRect();
  const room = window.innerHeight - a.bottom - t.height - 8;
  tip.style.left = `${Math.max(8, Math.min(a.left - 6, window.innerWidth - t.width - 8))}px`;
  tip.style.top = `${room > 0 ? a.bottom + 8 : Math.max(8, a.top - t.height - 8)}px`;
}

function hideTip() {
  const tip = $('tip');
  if (tip.matches(':popover-open')) tip.hidePopover();
}

export function initHints() {
  // Capturing, so a scroll of the slider panel or the work-item strip counts and
  // not just the window's: the bubble is positioned against a rectangle that has
  // just moved, and a tooltip that drifts away from its icon is worse than none.
  document.addEventListener('scroll', hideTip, true);

  // After the sliders' markup, so the viewer's panel is covered by the same pass.
  wireHints();
}
