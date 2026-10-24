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
    text: 'The language model that reads your prompt for this model. Each family '
        + 'is trained against exactly one — Qwen3 4B for Z-Image and Flux.2 Klein 4B, '
        + 'Qwen3 8B for Klein 9B, Qwen3-VL 4B for Krea 2, Qwen2.5-VL 7B for '
        + 'Qwen-Image — and another one loads without complaint and conditions '
        + 'on nonsense, so only the one that fits is listed. A Q8 GGUF of it '
        + 'is indistinguishable in practice and half the size.' },
  { ids: ['part-clip_l', 'pmpart-clip_l'],
    text: 'Flux.1\'s short text encoder: CLIP-L, which reads the gist of the '
        + 'prompt. There is one file for it and it is small, so this rarely '
        + 'needs changing.' },
  { ids: ['part-t5xxl', 'pmpart-t5xxl'],
    text: 'Flux.1\'s long text encoder, T5-XXL, which reads the detail of the '
        + 'prompt and any text to render. The fp16 file is 9.5 GB, and beside a '
        + 'Q8 Flux (12 GB) that is tight on a 24 GB card — a Q8 GGUF of it '
        + '(about 5 GB) is the fix, at no visible cost.' },
  { ids: ['seed'],
    text: 'The random noise the picture is grown from, as a number. The same seed '
        + 'with the same settings gives the same image, so it is how you keep a '
        + 'result and change one thing about it. -1 draws a new one every time; '
        + 'the circling arrow puts -1 back, and the die picks one at random and '
        + 'keeps it.' },
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
    text: 'The method that turns noise into the picture, step by step. Keep '
        + 'the one the model fills in unless you have a reason: few-step and '
        + 'turbo models are trained for one sampler and look wrong on others. '
        + 'The Help tab compares them.' },
  { ids: ['scheduler'],
    text: 'Where the steps are spent between full noise and none. Auto lets '
        + 'sd.cpp pick the model’s own, which is the right answer for Flux and '
        + 'Flux.2. The Help tab compares them.' },
  { ids: ['rdeditprompt'],
    text: 'What Edit should change, as an instruction — "make it night", "remove '
        + 'the hat". The picture itself goes to the model as image 1, and any '
        + 'References follow it. Kept apart from the Refine prompt, so it never '
        + 'becomes what every refine after it is asked for.' },
  { ids: ['rdextendprompt'],
    text: 'What to draw in the new strips. Blank continues the background, which is '
        + 'usually what you want: the image\u2019s own prompt names its subject, and '
        + 'Fill would draw it again at the edge.' },
  { ids: ['rprompt', 'hprompt'],
    text: 'Optional: what this pass is asked for, instead of the image\u2019s own '
        + 'prompt — the same box here and in Refine & adjust, and what Run after '
        + 'each image uses. A refine is a light pass, so it suits detail ("sharper '
        + 'fabric") more than a different object; over a painted area, say what '
        + 'should appear there — "a red scarf", "an open hand".' },
  { ids: ['rpstrength'],
    text: 'How much of the picture the pass may change. The whole image follows the '
        + 'Refine pass fold (a light 0.2–0.35 adds detail). An area keeps its own: '
        + '0.5, the default, keeps its shape and fixes the detail; 0.8 replaces it; '
        + '0.95 starts that area over.' },
  { ids: ['rpsoft'],
    text: 'How far past what you painted the new pixels fade into the old. '
        + 'What you painted is always replaced in full; this is the blend around '
        + 'it. Raise it when a strong redraw leaves a visible edge, lower it to '
        + 'keep the change tight. 0 is a hard edge.' },
  { ids: ['rdmodel'],
    text: 'The model this picture\u2019s Refine, Refine area, Hi-res and Edit run '
        + 'on. A saved picture starts at the model that made it; an upload at the '
        + 'one picked on the Image tab. Extend always uses FLUX.1 Fill.' },
  { ids: ['extend'],
    text: 'Makes the canvas bigger on the sides you pick and draws what lies '
        + 'beyond them, on any image. The picture you have stays exactly as it is; '
        + 'only the new strips are drawn, blended in over Edge softness. Blank '
        + 'prompt continues the background — type one to put something there. '
        + 'Uses FLUX.1 Fill, from the Get models tab.' },
  { ids: ['rescale'],
    text: 'Edit with a new shape: pick sides and the model redraws the whole '
        + 'picture to fill the bigger frame, following your instruction. Unlike '
        + 'Extend nothing is kept pixel for pixel — it is a re-imagining, so faces '
        + 'and small detail can change. Leave it unpicked for an ordinary Edit.' },
  { ids: ['rdcmp'],
    text: 'Which step Compare puts on the left. It starts on the step before the '
        + 'one on screen; pick the first to see how far the image has come.' },
  { ids: ['rpbrush'],
    text: 'Brush size on screen, so it feels the same in the column, fitted and at '
        + '1:1. Right-drag erases; Space+drag pans. Paint over all of what should '
        + 'change — Edge softness blends it into the picture beyond that.' },
  { ids: ['memory', 'vmemory'],
    text: 'Where the model’s weights wait while it draws. Auto lets sd.cpp decide '
        + 'by free memory — the card first. System RAM keeps them in RAM and moves '
        + 'them to the card as needed, leaving the whole card for the work: the '
        + 'fix when a big model runs out of GPU memory. It also lets go of the text '
        + 'encoder once the prompt is read. Stream from SSD re-reads '
        + 'them from the file, for when RAM runs out; slower. The Help tab says more.' },
  { ids: ['cachemode'],
    text: 'Skips denoising steps whose result can be predicted, so Generate '
        + 'finishes sooner, at some cost in exactness. Refine and Hi-res always '
        + 'run every step. Leave it Off for few-step models, which have nothing '
        + 'to skip; the Help tab says what the two modes do.' },
  { ids: ['layout'],
    text: 'MOSS-Image can be told where things go. Each box is placed on the '
        + 'picture and says what is in it: an Object, a Character, or Text — whose '
        + 'exact words it letters there. The Prompt stays the description of the '
        + 'whole scene. Boxes are kept as shares of the frame, so they follow a '
        + 'change of size, and every Refine and Hi-res pass is told them again at '
        + 'its own size. The model card calls this experimental: it guides, it '
        + 'doesn’t trace.' },
  { ids: ['vshots'],
    text: 'A clip, one shot at a time: each shot is its own sd-cli run with its '
        + 'own seed, length and words, and each after the first goes on from the '
        + 'last one’s final frame — so say only what happens in it; an action or a '
        + 'line in shot 1 happens once. Shots the clip on screen already has are '
        + 'ticked green. Change one’s seed, length or words and Redo from shot k '
        + 'draws it and the ones after it again, keeping those before; add a row '
        + 'and Extend clip draws it onto the end. Cheap to try at a few steps — '
        + 'then raise the steps and Generate clip makes every shot again with the '
        + 'same seeds. TI2V-5B, A14B and H3 can do it.' },
  { ids: ['vshotseed'],
    text: 'The shot’s seed. -1 rolls a new one, and the box then shows the seed '
        + 'the shot was drawn with, so Generate clip draws it the same way again '
        + '— at more steps, say. A new shot starts with the seed of the one above. '
        + 'The circling arrow puts -1 back; the die picks a seed at random and '
        + 'keeps it, to try the shot another way.' },
  { ids: ['vshotframes'],
    text: 'How long the shot is, in frames — divided by fps, that is its length. '
        + 'The model’s latent grid only permits certain counts (Wan works in 4k+1, '
        + 'H3 in 17k+5), so the number is rounded for you. A shot that goes on from '
        + 'the last one adds one frame fewer: its first is the last one’s final '
        + 'frame. A longer shot costs more memory, so a long clip is more shots.' },
  { ids: ['vshotcut'],
    text: 'How the shot begins. Continue draws on from the last shot’s final '
        + 'frame: seamless, but one frame is all that carries over, so a face that '
        + 'leaves and comes back is drawn anew and may not match. Cut starts a new '
        + 'camera shot from nothing but its words and, if you pick them, reference '
        + 'pictures — the dwarf’s face, his coat — which H3 keeps; it does not join '
        + 'the frame before. Only MiniMax H3 takes references, and never beside a '
        + 'starting frame, which is why a shot with them is a cut.' },
  { ids: ['cbformat'],
    text: 'What to save the version on screen as, made here in the browser, so '
        + 'only what this browser can encode is offered. MP4 plays everywhere — '
        + 'Discord, phones — and WebM nearly everywhere; AV1 is smallest, for '
        + 'newer players. Their sound is Opus, which some Apple players skip. GIF '
        + 'loops on its own in a chat, with no sound. Under 10 MB fits Discord '
        + 'without Nitro; a long clip then looks soft, and half size helps. The '
        + 'AVI is the file yasdig keeps, large and untouched.' },
  { ids: ['vevery'],
    text: 'Words that go in front of every shot, the first included: who and where '
        + '— the characters, the place, the look. One frame is all that carries '
        + 'across a seam, so a face that turns away is drawn again from these. '
        + 'Keep actions and dialogue out: they belong to a shot. Its {variables} '
        + 'roll once, so a rolled character is the same one in every shot.' },
  { ids: ['vloras'],
    text: 'LoRAs for the video model, from models/loras/ — only the ones for this '
        + 'model’s size are offered (a TI2V-5B LoRA does nothing to a 14B). On the '
        + 'Wan 2.2 A14B pair, each says which expert it patches: the low- and '
        + 'high-noise LoRAs published for it are one file each, and a Wan 2.1 14B '
        + 'LoRA goes on both. Strength 1 is what it was trained at.' },
  { ids: ['turbo'],
    text: 'A LoRA that teaches this model to finish in a few steps, from its Get '
        + 'models card. On, it renders in the few steps at CFG 1 the label says, '
        + 'instead of the model’s dozens at real CFG — several times faster, with '
        + 'a little less fine detail and variety. '
        + 'Off puts the model’s own steps and CFG back. The choice is remembered '
        + 'per model in this browser.' },
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
  { ids: ['opthires'],
    text: 'Run this hi-res pass on every image of a run, as it lands, with the '
        + 'settings below. Off by default: it roughly doubles the time per image, '
        + 'and is what makes an unattended run produce finished pictures rather '
        + 'than drafts.' },
  { ids: ['optrefine'],
    text: 'Run this refine pass on every image of a run, as it lands, with the '
        + 'settings below. Off by default. With the hi-res pass on too, it runs '
        + 'after it — refining the bigger picture, not the draft.' },
  { ids: ['optsave'],
    text: 'Copy every image of a run into work/output/ (the Outputs tab) as it '
        + 'lands, after any hi-res or refine pass ticked. Everything is kept in '
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
  { ids: ['cbsharp'],
    text: 'Sharpening applied to every enlarged frame. It is a fixed kernel — the '
        + 'same function of the same pixels every time — so it cannot flicker '
        + 'between frames the way a per-frame diffusion would. 0 is off.' },
  { ids: ['cbclarity'],
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
    text: 'When you press Close server, delete every unstarred clip on the Video '
        + 'tab, enlarged versions and all. Starred clips are kept, and so is what you '
        + 'saved to output. Only Close server does this.' },
  { ids: ['setclearchats'],
    text: 'When you press Close server, delete every unstarred chat on the Agent '
        + 'tab. Starred chats are kept, and so are the pictures its rounds made — '
        + 'those are in Current on the Image tab. Only Close server does this.' },
  { ids: ['setagent'],
    text: 'What a new chat lets its agent change, how far, and how long a session '
        + 'runs. Each chat keeps its own copy, so changing this leaves existing '
        + 'chats as they are. Set it on the Agent tab’s Permissions panel, with '
        + 'Save as default for new chats.' },
  { ids: ['amodel'],
    text: 'The language model the agent thinks with. One that sees pictures can '
        + 'judge a round and describe a picture; a text-only one can only talk. '
        + 'It loads for each reply and lets go of the GPU after, so it never '
        + 'shares the card with an image model — a bigger one is slower to load, '
        + 'not slower to draw with.' },
  { ids: ['agentknobs'],
    text: 'Off: the agent can’t touch it. Ask first: it proposes a change and '
        + 'waits for you. Auto: it changes it and says why. The numbers are the '
        + 'range it must stay in. A model distilled to run without CFG locks CFG '
        + 'and the negative prompt, whatever this says.' },
  { ids: ['agentpasses'],
    text: 'What the agent may do to a picture it likes: a Hi-res or Refine pass, '
        + 'or an Edit by instruction on a model that can. The result is a History '
        + 'step on that picture, so nothing is lost if it goes wrong.' },
  { ids: ['agentseedlock'],
    text: 'In a session, picture 1 of every round starts from the same seed, picture 2 from the '
        + 'next, and so on — so a difference between rounds is the change the '
        + 'agent made, not luck. Off, every picture is a new composition.' },
  { ids: ['agentbatch'],
    text: 'Pictures drawn before the agent looks. More gives it more to choose '
        + 'from for one load of its model; each one costs a render. In plain chat '
        + 'the agent draws one picture unless you ask for more, up to this.' },
  { ids: ['agentrounds'],
    text: 'The most rounds a session runs before it stops by itself.' },
  { ids: ['agentmaxchanges'],
    text: 'How many settings one reply may change. Small models judge better '
        + 'when one thing changes at a time.' },
  { ids: ['setembedpng'],
    text: 'Writes the prompt, negative prompt, model, seed, steps, sampler and CFG '
        + 'into each image you save to output, in the format A1111, Forge and Civitai '
        + 'read — and the rest of the Image tab’s settings for yasdig. Dropping such a '
        + 'PNG on the prompt box loads them back. Turn it off if you share pictures '
        + 'and would rather your prompts did not travel with them. No file paths are '
        + 'ever written.' },
  { ids: ['setvideosave'],
    text: 'What the clip dialog’s Save to output writes into the output folder. A '
        + 'video file is an .avi that any video player opens (with sound, for a model '
        + 'that makes it). Frames only writes the PNG frames into a folder, one file '
        + 'each, which is what an editor wants. A clip only reaches the output folder '
        + 'when you save it.' },
  { ids: ['setvaedirect'],
    text: 'Computes the VAE — the step that turns the result into pixels — without '
        + 'first unrolling the whole image into a huge temporary buffer. Same picture, '
        + 'much faster: Klein 9B’s decode went from 9.3 s to 0.9 s, Krea 2’s from 3.3 s to '
        + '1.2 s. Turn it off only to '
        + 'compare.' },
  { ids: ['setrefsize'],
    text: 'How big each reference image is drawn. Every reference costs as much work '
        + 'as an extra picture, so this is the biggest speed knob when references are '
        + 'used. Measured on a Klein 9B at 1024 with four references: 153 s at full size, '
        + '54 s at 768, 34 s at 512 (22 s with none). Edit '
        + 'always keeps the image it changes at full size.' },
  { ids: ['setgpureserve'],
    text: 'How much of the graphics card to keep free while a model draws. 0 lets '
        + 'stable-diffusion.cpp fill it to the brim; the desktop, the browser and the '
        + 'driver then sometimes have too little, and the run stops with “device lost”. '
        + '2–4 GB is usually enough. For this machine, every model.' },
  { ids: ['setmaxside'],
    text: 'The longest side Upscale, Hi-res or Output upscale may make, in pixels. '
        + 'Past it you get a message instead of an image. 8192 by default: a bigger '
        + 'image can be slow to show in the page. Your GPU sets the real maximum. '
        },
  { ids: ['setflash'],
    text: 'Use flash attention in the refine and hi-res passes when they run '
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
        + 'yasdig sets. The escape hatch for a model that needs something '
        + 'this page has no box for — and the place a wrong flag shows up as a '
        + 'failed render rather than as a message, since nothing here checks '
        + 'them.' },
];

// id → text. Two tables declare a control — `HELP` for the form fields, and
// `FILTERS` for the viewer's sliders, which carry their own — and this is the
// one index both end up in, so `wireHints` has a single thing to look in.
//
// Built on first use rather than at load: `FILTERS` is another module's, and
// reading it at this module's top level only worked while the import cycle
// happened to evaluate `viewer.js` first.
let HINTS = null;
function hintFor(key) {
  if (!HINTS) {
    HINTS = new Map();
    for (const h of HELP) for (const id of h.ids) HINTS.set(id, h.text);
    for (const f of FILTERS) if (f.help) HINTS.set(f.id, f.help);
  }
  return HINTS.get(key);
}

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
    const text = hintFor(key);
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
      // without this, asking what "Until stopped" means switches it on.
      e.preventDefault();
      showTip(b);
    });
    label.appendChild(b);
  }
}

// The bubble is a `popover`, which is what puts it in the browser's top layer,
// and that is the only reason it works in the two places it must: the viewer is
// a modal <dialog>, so an ordinary fixed element renders *behind* the backdrop,
// and a panel's `.fpbody` is `overflow-y: auto`, so an ordinary absolute one is clipped by
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
