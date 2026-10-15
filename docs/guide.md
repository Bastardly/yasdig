# User guide

Every part of the app, tab by tab. The in-app **Help** tab covers the same ground more briefly.

## Settings

The **Settings** tab is `user/settings.toml` as a workspace like the Image tab's: a
rail of sections, each a panel you open, move and resize — the values every model
starts from, and what the prompt boxes contain when the page loads. In
*Generation*, Steps, CFG, sampler, scheduler and size carry a note that they only
reach SD 1.x, SD 2.x and SDXL — other families start from their own (see
[Model families](configuration.md#model-families)). **Save settings** is at the
rail's foot.
Press **Save settings** and it is written and takes effect immediately — the
model list and the Image tab's boxes refill themselves, with no restart and no
reload.

The sections, in the rail's order: **Default prompt** (the prompt boxes' starting
text, and *Prompt variables*), **Generation**, **Image to image**, **Hi-res pass**,
**Refine pass**, **Per model**, **General** (Models, Speed and limits, Saved images,
When the server is closed, Advanced) and **Nightly**. The Default prompt panel can be
made wider as well as taller, by its corner, since a variable's words can be whole
phrases — one per line or separated by commas.

*Prompt variables* is the one part that is not a set of defaults. It is the word lists behind
`{animal}` in a prompt, and [Prompt variables](#prompt-variables) below says what
they do. Everything else is the default for a control on one of the other tabs,
and carries that control's own label and its own **(i)**, so "Refine strength"
here is the same words and the same explanation as "Refine strength" there. The
sections are named after what they belong to, which is why *Steps* appears
twice — once for the Image tab and once under *Starting from an image*, where it
is a different pass. The three under **Advanced** are the ones with no control
anywhere else: flash attention, the weight type and extra sd-cli arguments.
**Nightly** holds features still in the works, which can change or break between
releases.

Edits you haven't saved are marked on the Settings tab itself, so walking away
mid-change says so from wherever you walked to, and reloading the page asks
first.

`sharpness` and `clarity` are in the file but **not** in the form. They are only
where the viewer's sliders start, which is a live per-image control everywhere
else — and the Outputs tab starts them at zero whatever they say, because a
saved output already has its grade in the pixels. Edit them by hand if you want
a different starting point.

A bad value is rejected before anything is written, so the file is never left
worse than it was found; the reason appears under the Save button. If you break
the file by hand, the tab says what is wrong and fills the form with the
built-in defaults — saving it is the repair. Generation meanwhile falls back to
those same built-ins rather than refusing to run.

Nothing here is retroactive. An image already made keeps the settings it was
made with, which is what lets reopening a work item restore it faithfully.

### Per-model settings

A file can't say whether it is an SDXL or an SDXL-Turbo, or a Z-Image turbo or its
base model — same tensors, different training — so the difference is yours to say,
once per model:

- On the Image tab, set the boxes the way that model wants them and press
  **Save as defaults for this model**. It opens that way from then on, and is tagged
  *tuned* in the select.
- Or on the Settings tab, the **Per model** panel: pick a model, and each box shows
  its own setting — or is empty, with the value it will start at as the
  placeholder. Empty a box to drop that setting; **Clear all** drops them all.

Only what differs from where the model would start anyway is kept, in
`user/model-settings.toml`. So changing a global setting still reaches every model
that has no opinion of its own about it — the tuned turbo keeps its 6 steps, and
picks up your new refine strength. This is also the place for a model's own
**Extra sd-cli arguments**, such as `--offload-to-cpu` for a model that doesn't fit,
and for the files it runs with — a VAE picked in the **Models** panel is saved with the
rest, under a table of its own:

```toml
[model."juggernautXL"]
steps = 30

[parts."juggernautXL"]
vae = "vae/sdxl_vae.safetensors"   # relative to the models dir
```

A saved file that has since been deleted greys the model out naming it, rather than
running on something else. Picking the file the scan would have picked anyway saves
nothing.

A model is remembered by its name, which for a discovered one is its file name
without the extension. When a model is gone, the next start moves its settings to
`user/model-settings.removed.toml` and says so, and moves them back if the file returns
— so taking a model out for a while costs nothing. Renamed a file? Rename its entry in
`user/model-settings.toml` to match (before starting yasdig) and it keeps them.

## Prompt variables

A **variable** is a name and a list of words, defined under *Prompt variables* on
the Settings tab. Write it in braces anywhere in a prompt and each image draws
one of the words:

```
animal = crab, horse, dog, cat
color  = red, blue, yellow

"A lovely {animal} wearing a {color} hat"   →   "A lovely crab wearing a blue hat"
```

Which makes a batch run vary its subject rather than only its seed: **Generate
20** with `{animal}` in the box is twenty different pictures, not twenty seeds
of one. Write the same variable twice and it is drawn twice — `a {color} hat and
a {color} coat`.

Separate the words with commas. Spaces inside a word are fine (`glorious gopher,
pretty python`), and a variable is deleted by removing its row.

Five things are worth knowing:

* **Braces, not `[brackets]`.** `[cat]` already means something to the sampler —
  it de-emphasises the word by 1.1× — so `[animal]` could never be told apart
  from a typo. With braces a name nothing answers to is an error naming the
  variables you *do* have, instead of the word `{animl}` quietly appearing in
  the picture.
* **Case is ignored.** `{Animal}`, `{ANIMAL}` and `{animal}` all find `animal`.
* **The draw follows the seed.** The same seed and the same settings still give
  the same image, prompt included — so pinning a seed and pressing Generate
  twice gives the same words both times, and clicking a reported seed to
  reproduce a picture still works. A batch walks the seed, so it still varies.
* **The box keeps the braces.** What actually ran is recorded with the image —
  it is in the work item, on its thumbnail's tooltip, and on the line under the
  result — and a Refine or Hi-res of that image re-uses it rather than drawing a
  different animal. Only a new generation rolls again.
* **Reopening a work item puts the braces back.** Both sentences are kept, so
  clicking a thumbnail restores `a lovely {animal}` rather than the crab it
  drew — which is what makes a saved generation a workflow you can run again
  instead of one picture. Nothing is lost by it: the seed comes back at the same
  time and the roll follows the seed, so pressing **Generate** reproduces that
  exact image, and clearing the seed to `-1` draws a new animal. The sentence
  that actually ran stays on the line under the picture.

Anything that is not a well-formed name is left alone, so `{a nice day}` and a
stray `{` stay exactly as typed.

## Prompt weighting

Any prompt box takes the weighting syntax ComfyUI and Forge use, and will write it
for you. Put the caret in a word — or select a phrase with the mouse — and press
**Ctrl+↑** or **Ctrl+↓**, or hold Ctrl and roll the wheel:

```
a lovely fluffy crab           Ctrl+↑ on "crab"  →  a lovely fluffy (crab:1.1)
a lovely fluffy (crab:1.1)     Ctrl+↑            →  a lovely fluffy (crab:1.2)
a lovely fluffy (crab:1.1)     Ctrl+↓            →  a lovely fluffy crab
```

Each step is 0.1, and **1.0 removes the wrapper** rather than writing `(crab:1.0)`,
so the round trip always ends back at the words you typed. The number is unclamped —
0 and negatives are reachable, as in ComfyUI — and it is snapped to a tenth, so a
weight hand-typed as `1.25` lands on `1.4` the first time you nudge it.

It works on every prompt box, positive and negative. A weighted prompt is
stored verbatim with the image, so reopening a work item brings the weights back.

Three things worth knowing, all of them properties of sd.cpp's parser rather than of
this UI:

- **Nesting multiplies.** `(((house:1.3)))` conditions at 1.573. So pointing at one
  word of a weighted phrase nests deliberately — Ctrl+↑ on `crab` inside
  `(a red crab:1.2)` gives `(a red (crab:1.1):1.2)`, which is 1.32 on the crab and
  1.2 on the rest. Pointing at the whole phrase retunes the 1.2 instead.
- **`(word)` is ×1.1 and `[word]` is ×1/1.1**, the A1111 shorthands. Both are
  recognised, so a hand-typed `[word]` is retuned rather than wrapped; it reads as
  0.9, since 0.909 is not a number to show someone mid-drag.
- **`[from:to:when]` prompt scheduling is not supported by sd.cpp at all**, and fails
  quietly rather than loudly — the whole thing is conditioned as literal text at 0.909.
  Don't reach for it.

Ctrl+Z walks the edits back one nudge at a time; the shortcut writes through the
field's own undo stack rather than replacing its contents.

## Settings inside a PNG

Every image you save to output carries the settings that made it, written into the
PNG the way A1111 and Forge write theirs. Civitai, A1111's *PNG Info* and any other
tool that reads those files see the prompt, negative prompt, model, seed, steps,
sampler, CFG and size, with each LoRA as a `<lora:name:weight>` tag on the prompt.
yasdig also writes its own record beside it, holding the rest of the Image tab:
the refine and hi-res settings, the prompt as you typed it before its
`{variables}` rolled, and the LoRA picker. No file path is ever written.

**Load settings from a PNG…** under the prompts, or dropping a PNG on the prompt box,
puts them back. A yasdig PNG restores everything, so Generate makes the same
picture again. A PNG from A1111, Forge or Civitai fills what it names and leaves
every other box alone. A seed from another program gives a close picture rather than
the same one. A model or LoRA you haven't installed is named in the line under the
button instead of being picked. A ComfyUI PNG holds a node graph rather than
settings, so it loads nothing and says so.

Settings → General → *Saved images* turns the writing off, if you share pictures and would
rather your prompts didn't travel with them.

## The Image tab

The Image tab is a workspace: a rail down the left, the latest picture filling the rest,
and **panels** that float over it. Each rail button opens one — **Prompts**, **Models**,
**Generation** (the seed at its top, the output size and upscaler at its foot),
**Image to image**, **Hi-res pass**, **Refine pass**,
**Info** (the picture's size, seed, timings and the prompt as it was drawn), **Current**
and **Favorites** (the work items). Open as many as you like and drag each by its title
bar to where you want it; it stays there next time, and a double-click on the title bar
puts it back. Each panel's title bar says what is inside, in amber when it departs from
a plain generate (a batch, a source, a LoRA, an extra pass), and the rail button then
carries a dot so a closed panel still says so. A panel can be made taller or shorter
by its bottom-right corner, and remembers that too. The rail's foot shows the seed
the next Generate uses (in amber when it is pinned rather than -1), then **Save
Output** and **Until stopped**, Stop and **Generate**; progress, the live preview and any error appear in a card at the lower
left of the picture.

Click the picture to open **Refine & adjust** — Hi-res, Refine, Edit, Extend, Scale,
the sliders, and *Save to output* are all there (see *Refine*). The Video tab is laid
out the same way.

## Work items

Every image you generate is kept, in `work/workitems/`: the unsharpened
output-resolution PNG, a thumbnail, and a JSON sidecar with the settings that produced
it. They survive a restart, and they show up as thumbnails in the **Current** panel;
starred ones are also in **Favorites** (see below), each panel a rail button away.

Click one and it comes back **live** — the controls repopulate with that image's
settings, the picture goes back on screen, and Refine, Hi-res and the sliders work on
it again. Because the panel has been restored to what made the image, an escalation runs
with those settings unless you change them first, in which case it runs with what the
boxes say. That is the point: render a batch, pick the one worth the money, and escalate that
one. The server keeps the last 8 results warm in memory, but an image that has fallen
out of that window reopens from disk just the same.

Flip the toggle above the grid to **Click to delete** and a click *marks* a thumbnail
instead — red frame, red cast — and a `Delete N selected` button appears. Nothing is
removed until you press it, so culling twenty images from a batch is twenty clicks and
one confirmation rather than twenty confirmations. Switching back to **Click to open**
clears the marks.

**Delete all N unstarred** is always beside them, and is the one button on the grid that asks
first: marking is visible and reversible, and emptying the directory is neither. The
Outputs tab has the same controls, working the same way.

### Favorites

Which is the problem: a grid of every image you have ever made is used by emptying
it, and that takes the good ones with the rest. So every thumbnail has a **star** in
its top-right corner (it shows when you hover; a filled one always shows). Click it
and that work item **moves** to `user/favorite/`; click it again and it moves back.
Clicking the star never opens the image. A starred item stays where it was in the
grid, but it is out of reach: `Delete all` doesn't count it or delete it, and *Click
to delete* can't mark it. So `Delete all` stays the blunt instrument it is, and what
survives it is what you starred.

`user/` rather than `work/` on purpose. `work/` is the directory this program
deletes from — it is cleared of scratch at startup and emptied on request — and
nothing that took a decision to create should live under a sweeper. The same reason
`settings.toml` is there.

**Current** shows everything, starred or not; **Favorites** shows only the starred
ones. A favourite opens exactly as a work item does: the
controls repopulate, the picture comes back, Refine and Hi-res and the sliders work
on it. With the prompt restored as you *typed* it — braces and all, see
[Prompt variables](#prompt-variables) — that is a saved workflow rather than a saved
picture.

On the favourites shelf every star is filled, and **Delete all** is gone, since
emptying that directory in one press is the exact thing it exists to be safe from.
Un-starring a tile moves it back among the ordinary work items. **Click to delete**
on this shelf deletes a favourite outright.

Refined and hi-res results are deliberately **not** archived: those are the expensive
rungs and most of them are experiments. **Save to output**, in Refine & adjust's rail, writes the
picture currently on screen — filters and all — to `work/output/`, with its own sidecar.
It is available for a plain generate too, and you can press it after each of several
refines to keep every step.

Everything else in `work/` is scratch and is still cleared at startup.

## References

With a model that takes reference images — Flux.2 Klein 4B and 9B for now — a
**References** button appears in the Generation section. It opens a small dialog with
four numbered slots (Choose, Upload, drag in or paste); the prompt names them by
number: *"the woman from image 1 wearing the jacket from image 2, in a market"*. They
reach sd-cli as `-r`, on the generate only — Refine and Hi-res finish the picture, they
don't redraw it toward a reference. Large pictures are shrunk to the model's size
first, since each reference costs about as much as the image. Picks survive switching
to a model that doesn't use them, and a reopened work item puts its references back.

## Image to image

The Image tab can start from a picture instead of from noise. The **Image to image**
panel holds the slot: **Choose…** picks one of your outputs (everything in
`work/output/` — what you have saved, and what you have uploaded), **Upload…** takes
a file, and dropping or pasting an image onto the tab does the same. An upload lands
in `work/output/` beside your saves, so it is in the picker next time, in the Outputs
tab. **Clear** goes back to text to image.

Anything the browser can open works — JPEG, WebP, AVIF — because the page
re-encodes it to PNG in a canvas before uploading. Nothing in the binary decodes
anything but PNG, and that stays true.

With a picture picked, the panel opens and changes to match: base size and
Output upscale hide, because they describe a render from noise, and two boxes appear
under the picture. The pass runs the Generation panel's **Steps** and **CFG scale** —
all of the steps, with Strength deciding how far up the schedule they start — so a
model's own numbers apply with no second set to keep in step. The panel's header says
*from …* whenever a picture is picked, so a folded slot never quietly turns Generate
into a pass over a picture.

**Strength** is the knob that matters. It decides whether this is a retouch or a
re-render: 0.3 barely moves the picture, the default 0.55 keeps the composition,
the pose and the palette while redrawing the surface in the model's own idiom, and
0.7 and up treats your image as a rough sketch. **Working size** is a ceiling, not
a target — a source already smaller than it is left alone, and a bigger one is
scaled down first. It starts at the model's `hires_max_side` because that is the
same question already answered for this card: a 4000 px phone photo diffused at
full size is an out-of-memory, not a better picture.

Everything else is the Image tab as usual: the same prompt, model, LoRAs and
escalation settings, and the same run panel — *Generate 8* over one picture is eight
seeds of the same transform, a good way to pick the best of several. The result joins
the ordinary ladder (Hi-res, Refine & adjust, Save), and is archived as a work item
with its source, so reopening it puts the picture back in the slot. Reopening one
made from noise clears the slot.

## LoRAs

A LoRA is a small file that patches a checkpoint's weights: a style, a character, a
4-step distillation. Drop them in `models/loras/` — subfolders are fine and are
scanned, so `models/loras/sdxl/` and `models/loras/flux/` work — and they appear in
the **LoRAs** block under the model select on the Image tab. Press **+ Add LoRA**,
pick one, set a strength, add another if you want. `doctor` prints the folder it is
looking in and what it found there, which is the first thing to check if the list is
empty.

**Strength** is the multiplier. 1 is the weight the adapter was trained at, 0.5 is a
hint of it, and negative subtracts — a real technique. There is no limit either way:
slider LoRAs are often published for values like -13.

**A LoRA has to match the family of the model it patches**, and the picker enforces
it. An SDXL LoRA does nothing to a Z-Image, and stable-diffusion.cpp won't say so: it
renders, and the picture comes back as though you hadn't attached it. So each LoRA is
identified the way a checkpoint is — by the names of the weights inside it, never by
its filename or its metadata, which is often wrong (one popular SDXL LoRA calls itself
SD 1.5 in its own header) — and one for another family is listed greyed out with what
it is *for*. Switch the model and the list re-greys; a LoRA already picked that no
longer fits stays on its row with a line saying why, and Generate refuses it rather
than running without it. A LoRA nothing could identify is offered for every model —
refusing a file that works would be worse. `doctor` prints each LoRA's family.

The Flux LoRAs are the case where the names say least: Flux.1 and every Flux.2 share
`double_blocks`, so a Flux LoRA is placed by how wide the model it patches is (Klein 9B
and Flux.2 dev are wider) and, for the two the same width, by which blocks it touches —
Flux.1 has 19 double blocks, Klein 4B five. One that only touches blocks both have is
offered for both rather than guessed. A Flux.1 LoRA fits dev and schnell alike.

What you pick travels with the image. **Refine** and **Hi-res** run the same
adapters, because they are passes over that picture and one that dropped the LoRA
would redraw the surface in the base model's idiom and undo the look — so there is
one picker, not one per pass — unlike the checkpoint, which does get one each.
Naming a different model for a pass does clear them, which is also right: a LoRA
belongs to the base it was trained against.
Reopening a generation from the work-item grid puts its LoRAs back with the rest of
its settings, and they are in its sidecar, so what made an image stays readable.

A LoRA you name but that isn't on disk is an **error**, not a silent omission. That
is a deliberate difference from stable-diffusion.cpp, which warns on its own stdout
and renders happily without it — two minutes spent on a picture that isn't the one
you asked for, with nothing saying so.

You can also still type `<lora:name:0.8>` into the prompt box by hand; it resolves
against the same folder. That route keeps sd.cpp's silent-drop behaviour, since
nothing on this side is looking at it.

## Upscalers

Every render is enlarged on its way to the output size, and the **Upscaler** select
beside *Output upscale*, at the foot of the Generation panel, picks what does it. **FSR (EASU)** is the
default: milliseconds, no file, and it only redistributes the pixels the checkpoint
drew. A **model** draws new fine texture instead — hair, fabric, skin — at the cost of
seconds. **Get models → Upscalers** downloads four that run here — 4x-UltraSharp and
4xNomosWebPhoto (ESRGAN), Real-ESRGAN general x4v3 (Compact, fast) and 2xNomosUni SPAN
(a fast 2×) — or drop one in `models/upscalers/` (subfolders work) and it appears in
the select with its architecture beside it; `doctor` prints the folder, what each file is, and
whether it can run.

`.pth`, `.pt` and `.safetensors` all load directly — no conversion and no Python. The
architecture is read from the file's tensor names, never from its filename, and decides
how it runs:

| architecture | runs on | examples | speed |
|---|---|---|---|
| **ESRGAN** (RRDBNet) | sd-cli (`-M upscale`) | `4x-UltraSharp`, `RealESRGAN_x4plus`, `x4plus_anime_6B`, `4x_NMKD-Siax_200k`, `4x_foolhardy_Remacri`, `4xNomos8kSC`, most `4x_*.pth` | seconds |
| **Compact** (SRVGG) | yasdig's own shaders | `realesr-general-x4v3`, `realesr-animevideov3`, most "compact" releases | fast |
| **SPAN** | yasdig's own shaders | `4xNomos8k_span_otf_*`, most `*_span*` files on OpenModelDB | fast |
| **SwinIR** | yasdig's own shaders | `003_realSR_BSRGAN_DFOWMFC_s64w8_SwinIR-L_x4_GAN`, the classical and lightweight releases | slowest — measure it |
| DAT, HAT, others | — | listed greyed out, *not supported* | |

**If you want the best result for the least fuss, pick an ESRGAN.** It is the most
widely trained family by far, and sd.cpp's kernels for it are mature. The other three
run on hand-written compute shaders on the same Vulkan device the post-process uses —
sd.cpp can't run them, and adding them there would mean a patched sd.cpp — and every
buffer they use is freed when the enlarge finishes, so the next diffusion pass has the
whole card. DAT and HAT are transformer upscalers too heavy to be worth it here: minutes
per image. A file that isn't supported is shown in the select but can't be picked, and
a generate that names one anyway is refused *before* it renders.

sd-cli on its own saves your image unenlarged and reports success when it can't build a
model; that is caught and is an **error**.

A model enlarges by the fixed scale it was trained at, 2× or 4×, and the output is
brought to exactly the size the *Output upscale* box asks for: a 4× model at 2× is run
at 4× and reduced with Lanczos (the usual way to use one, and supersampling in its own
right), and a 2× model at 3× is finished by EASU. So the size quoted under the box is
the size you get, whichever upscaler you pick. Set the box to 1 and nothing runs at all.

The choice is saved with the image, so reopening a work item restores it, and the
Settings tab has a default for it. It applies to the first enlarge only; the hi-res
pass and *Enlarge frames* still use EASU.

**Checking a new model file**: `yasdig upscale in.png out.png <file> 4`, at the
model's own scale. Besides the time, it prints a **consistency** figure — the result
shrunk back by 4 and compared with the input, in dB. An upscaler that works lands above
30 dB, because shrinking its output gives back what went in; one that is broken, or a
file that isn't what its tensor names claim, lands far below. `yasdig sr-selftest`
checks the shader operations themselves against reference implementations, and prints
how fast the GPU runs them.

## Outputs

The third tab is `work/output/` as a gallery: everything you have saved, and everything
you have dropped in, at whatever size it was saved at. It is the one tab with no
controls — click a thumbnail and the **Refine & adjust** dialog (see *Refine*) opens on
it full size, and **←** / **→** step to the previous and next image with the dialog
left up. A focused slider keeps its own arrow keys, so click the picture to hand them
back to the gallery.

**Save changes**, at the bottom of the panel, writes what is on screen back over the
picture — the sliders, a crop, a Refine or an Extend alike — and the tile stays where it
was in the grid. The viewer holds the picture as it was when you opened it and the sliders
work on that, so saving twice doesn't grade on top of the first save, and **Reset**
then **Save changes** puts the original back — until you open another picture.
**Save as new output**, above it, keeps the original and saves the edit as a new file
beside it. Closing the dialog or stepping with **←** / **→** with an edit that isn't
saved asks first, so an edit isn't lost by accident. While a pass runs on the picture,
**←** / **→** wait for it.

The same **Click to open / Click to delete** toggle and stars the work-item grid has
are on this one, and behave identically: a click marks, `Delete N
selected` removes the set, and **Delete all N unstarred** deletes everything that isn't
starred, behind a confirmation. Deleting the image the viewer is showing is safe — the server is holding
a copy of it in its scratch dir, so the sliders and Save keep working; it just has
nothing to go back to.

**Current** and **Favorites** flip the gallery, as they do for work items. The star
**moves** an image to `user/favorite_outputs/`, so `Delete all` and the clear-on-close
settings can't reach it, and it stays in Current's grid; un-starring moves it back.
On Favorites, *Delete all* is gone. A favourite opens in the same dialog,
and can be picked as an *Image to image* source — the picker
behind **Choose…** has the same Current / Favorites toggle. A new save never takes
the name of a favourite, so one name always means one picture.

Refine, Edit, Extend and Scale work here too, on the opened file: it already has its
grade and its sharpening in the pixels, so the sliders start at zero, and a pass over
it is a choice you make, like picking it as a source. **Refine with**, at the top of the
panel, is the model the passes run on, starting at the one that made the picture (or,
for an upload, the one picked on the Image tab). Hi-res stays on the Image tab; to
start a real pass from a saved picture at a strength you choose, pick it in the
**Image to image** panel.

## Refine

A click on the picture — or a picture opened on the Outputs tab — opens **Refine &
adjust**, the one place an image is finished: the picture, nearly full screen, with a
narrow **rail** down the left. Each rail button opens a floating panel over the
picture: **Refine** (the pass prompt, Refine, Hi-res, Edit, Extend), **Scale** (Scale
and Crop), **Adjust** (Tone, Colour, Detail — see *Postprocessing*), **Brushes**,
**Selection** (the area and its colours) and **History**. A panel's buttons stay at
its foot however far you scroll it, the main one in amber. Open as many as you like;
drag one by its title bar to put it anywhere, and it stays there next time
(double-click the title bar to put it back). A dot on a rail button says its panel has
something in effect — a painted mask, a moved slider, brush strokes — even while it is
closed. The rail's foot has Fit / **1:1**, **Download** and, from the Image tab, **Save
to output**; **Apply adjustments** is History's main button. Progress, the live
preview, Stop and any error appear in a card at the lower left.

**Hi-res** (the Refine panel's *Hi-res* fold and button) enlarges the picture and adds
detail at the new size, with the Hi-res pass panel's steps, strength and model — do
that first, since it re-draws the whole picture lightly and would partly undo a refine
made before it.

The picture under the pointer belongs to whichever of the Refine fold, Crop, Selection
or Brushes is open — opening one closes the others. With none open, a drag pans at
1:1; the middle button or Space+drag always pans.

What the Refine fold's button does depends on the mask:

- **Nothing painted — Refine image.** A light img2img pass over the whole picture, at
  the Refine pass's steps and strength: detail and texture rather than new content.
- **Something painted — Refine area.** Only the painted part is redrawn. It is cut
  out with a margin, enlarged to the model's own size, redrawn, shrunk back and blended
  in, so small things come back *sharper* and every pixel outside keeps its exact
  value. It works on every model, not just inpainting checkpoints. **Strength** for
  an area is its own (0.5 keeps the shape and fixes the detail, 0.8 replaces it, 0.95
  starts over), and **Edge softness** is how far past the paint the new pixels fade
  into the old.

- **Scale** makes it smaller or bigger with no diffusion. Smaller (0.25×–0.75×, or
  **Width…** for an exact width) is instant, rounded down to a multiple of 8 pixels.
  Bigger (1.5×–4×) is FSR or any runnable model from `models/upscalers/` (the same
  choice as *Output upscale*). It draws nothing new, so unlike Hi-res it can't undo a
  refine — make it the last step for a bigger file, or scale and then refine to add
  detail at the new size. A painted mask scales with the picture. When a pass runs
  out of memory, the box offers **Scale down to 0.75× and try again**.

Each pass can be given its own prompt ("sharper fabric texture", "an open hand").
Blank uses the Refine pass prompt from the panel on the left, then the image's own
(Hi-res uses the Hi-res pass panel's); the image keeps its prompt either way, and the History says what each
step was asked. Right-drag erases, and the mask only shows while the pointer is over
the picture.

**Compare** (or **C**) puts an earlier step on the left and the image now on the right,
split by a line you drag; *Compare with* picks the step. Both halves carry the same
grade and sharpening, and a smaller earlier step is stretched to the same box, so what
you see is what the pass changed.

**Edit** (Flux.2 Klein images) changes the whole picture by instruction, with no
painting: type what to change — "make it night", "remove the hat" — and press Edit.
The image goes to the model as reference 1 and any References picks follow it, so the
picture is kept and what you asked is changed. It runs at the image's shape at the model's
own size (1024 for Klein) and is scaled back up — an edit costs the render's tokens
plus the reference's, and at 2048 that ran Klein 9B out of memory — so a Refine image
afterwards brings a large image's detail back. A History step like the rest.

**Rescale with Edit**, under the Edit button, gives it a new shape: pick sides
(← ↑ ↓ →) and an amount and the button becomes **Edit and rescale**. The model redraws
the whole picture to fill the bigger frame — "the same room, wider". It is a
re-imagining, nothing pasted back, so faces and small detail can change; when the
picture must stay exactly as it is, Extend.

**Extend** grows the canvas on the sides you pick (128-512 px each) and draws what lies
beyond them, on any image — SDXL, Klein, an upload. It uses **FLUX.1 Fill**, a model
trained to continue a picture from a mask, and turns on by itself once one is installed
(Get models → FLUX.1 Fill, about 12.7 GB at Q8 plus Flux.1's three shared parts);
without one the group is greyed out with a link to it. Each side is its own Fill pass
at the Fill model's full size — left and right first, then top and bottom across the
new width, which draws the corners — and only the strips are drawn: every pixel you had
keeps its exact value beyond the Edge softness blend. A blank prompt continues the
background; the image's own prompt names the subject and would draw it again in the
strip, so type one only to put something particular there. The image keeps its own
model for any Refine afterwards. FLUX.1 Fill dev is under the FLUX.1 [dev] non-commercial
licence; check the card's link.

Every pass — Refine, Refine area, Hi-res, Edit, Extend, Scale — is a step in the History, as are a crop and an Apply, with a thumbnail
each. Click one to go back (or Ctrl+Z / Ctrl+Shift+Z): fixed the fingers, ruined the
hat, step back to the fingers and try the hat again. A new pass from an earlier step
replaces the ones after it. An image keeps its last twelve steps and always its
original; history is scratch, gone with the image or the server. Compare shows an
earlier step of another shape (before an Extend) whole inside the frame.

For a batch, *Run after each image* in the Hi-res pass and Refine pass panels runs them on every image
with the prompts from the Hi-res pass and Refine pass panels; a `{variable}` in them
rolls per image.

## Escalating with a different model

**Hi-res model** and **Refine model**, at the head of the Hi-res pass and Refine pass panels, run
that one pass on a checkpoint other than the one the image was made with. Leave
either on *same as generated* and that pass doesn't change. Picking one fills its
fold's steps, strength and CFG from that model's own settings, since the pass runs
on them; the boxes can then be changed as usual.

Two pickers rather than one because the two passes are not the same job: hi-res
is inventing detail at a size nothing has drawn yet, refine is tightening what is
already there. Draft fast on a Turbo checkpoint, enlarge on one that holds
structure, finish on one that draws surfaces well.

Naming a second checkpoint costs no memory. Every pass is a separate `sd-cli`
process that loads its weights and frees them when it exits — the pipeline
already reloads the *same* file once per rung — so the only cost of a second one
is re-reading it from disk when it has fallen out of the page cache.

They are there because a checkpoint is a setting of the pass *about to run*, not
of the one that already ran — which is the same test `steps` and `cfg_scale`
fail, and why those stay retained. The pair is stored with the generation too, so
reopening a work item brings back what you meant to finish it with along with
everything else.

The swap is **wholesale**: the chosen model brings its own refine and hi-res
steps, strengths, CFG, hi-res budget and slider starting points, and the
boxes in the panel are then applied on top — which is what you'd expect, since
picking a model for a pass refills that pass's boxes from it. The two
model choices themselves survive it, being no checkpoint's to state. It also
means the buttons re-derive from the new model: picking one with
`hires_steps = 0` puts 0 in the Hi-res steps box, which greys out the Hi-res
button, and one with a smaller `hires_max_side` may say the image is already
past its budget.

## Batch and loop

The Generation panel is about the *run* rather than the image. The Generate button is
in the bar pinned to the bottom of the window — with the seed, Cancel and Stop, the
progress line and the live preview above it — so it is in reach however far you have
scrolled the settings, and a job starting or finishing never moves the prompt you are
typing in. Set **Images** to a number and
the button says `Generate 4`; the images are produced one at a time and each
appears as it lands, so you can watch the batch fill the work items strip rather
than waiting for the lot. Tick **Until stopped** (at the rail's foot) and the button
becomes `Start loop generation`: it ignores the count and keeps going until you press
**Stop job**, which finishes the image in flight and then ends the run (it only shows
for more than one image). A loop **follows the panel**: change the prompt, the model or
any setting while it runs and the next image uses it ("Loop updated" under the
button); the image already drawing finishes as it started. There is one loop at a
time — starting a loop while one runs just hands it the panel at once. **Cancel generation**, beside it, throws the image in flight
away and moves on to the next one in the run — a single image simply ends; on a Refine
or Hi-res pass it cancels that pass and leaves the image as it was.

Generate **queues** the run on the server, and stays pressable: press it again — with
other settings, another model — and that run waits its turn behind this one. While
anything is queued, a green **Jobs (n)** button sits at the end of the tab row; it
opens a list of what is running and waiting, with a **Stop** or **Remove** on each (both
ask first). A red **!** on it means a loop is in there. A loop never hogs the queue:
after each of its images, anything queued behind it runs first. **Refine** and
**Hi-res** pressed during a batch run between two of its images rather than after the
whole batch. Conversions queue the same way.

The run belongs to the server, not the page: switch tabs, edit outputs in the viewer
(which never waits on the queue) or reload, and it carries on — the Image tab shows its
newest image when you come back.

While anything is rendering, the line in that bar counts the steps as sd-cli
reports them — `Image 2/4 · Step 7/20 · 1.23s/it`, or `Loading weights 18/50 ·
412.00MB/s` before the denoising starts. It is polled from `GET /jobs`, which is
answered off the job queue, so it keeps moving during a render rather than arriving
once the render is done.

Under it, a **live preview** shows the picture forming, updated every step — enough
to see a wrong composition, a wrong subject or a LoRA doing nothing a few steps in,
and press **Cancel generation** instead of waiting out the render. It is sd-cli's `--preview proj`:
a fixed projection of the latent to colour, so it costs no extra download and works
for every image family, but it is 1/8 of the render's size and shows composition and
colour rather than detail. Generate, *Image to image*, Refine and Hi-res all show one.

A pinned seed **advances by one per image**, so a batch of eight isn't eight copies of
the same picture; the first image is exactly the seed you pinned. `-1` keeps drawing a
fresh seed per image, as always.

Three optional steps run on each image as it lands, all off by default, in this
order:

- **Hi-res pass** — *Run after each image* at the top of the Hi-res pass panel, at
  that panel's **Hi-res scale** (1 to 4, so 2.5 is fine).
- **Refine** — the same box in the Refine pass panel; after the hi-res pass, for the
  reason the buttons are in that order.
- **Save Output** — in the rail, above *Until stopped*: writes the finished image to
  `work/output/`, so a run can fill a folder without a click per image.

They apply to a run of one just as much as to a batch, which is how you queue a single
generate-and-refine and walk away. Both passes read the same boxes the buttons
do, and a run takes them as they are **when you press Generate** — so you can queue a
run, change the boxes and queue a different one.

Any failure stops the run and reports it beside the controls — whatever killed image 3
would kill image 4 as well. Everything generated up to that point is still in the grid.

Two things worth knowing before leaving one unattended. **The queue lives in the
server's memory**: closing the tab doesn't stop it, but *Close server* (or quitting
yasdig) drops whatever is still queued. And every generation is archived, so an
overnight loop writes a few MB per image into `work/workitems/` — delete what you
don't want from the grid. Only the newest image per place it is shown is kept warm, in
`work/current/`, which is emptied on start and on *Close server*: a refined or hi-res
image you want to keep is one **Save to output** away (or tick **Save Output** in the rail).


## Convert

The third tab requantizes a model file you already have, instead of downloading a
different copy of it. Pick a file, pick a weight type, press Convert; the result is
written **beside the original** as a `.gguf`, so it is discovered on the next start with
no config edit. Nothing is deleted.

This exists because **a file's type on disk is not what it costs to run**. sd.cpp asks
the backend whether it can multiply the stored type and, when it can't, inserts a cast
into the compute graph for every linear layer in the model
(`src/model/common/ggml_block.hpp:211`). Every Vulkan device today reports no fp8 — so a
15 GB fp8 checkpoint executes as BF16 and behaves like a file twice its size. On a
24 GB card that is the difference between running and `ErrorDeviceLost`. Converting it
to `q4_K` is not a downgrade from fp8; it is the first entry in that list the backend
can actually multiply.

The list of files comes from the same header scan that finds models, which matters for
more than convenience: `-M convert` takes **a separate flag per component** and prefixes
every tensor name according to which one you used — `--diffusion-model` prepends
`model.diffusion_model.`, `--vae` prepends `vae.`. Convert a VAE through the diffusion
slot and you get a perfectly valid file whose every tensor is misnamed, which fails
minutes into a later run rather than at conversion time. The page never picks that flag;
the server derives it from what the file turned out to be. Components with no `-M
convert` flag at all — a Qwen language model, a CLIP-Vision tower — are left off the list rather than
offered with a footnote, and LoRAs are excluded because quantizing a low-rank delta is
how you lose the effect you downloaded it for.

The size shown before you press the button is an estimate, computed by transcribing
sd.cpp's own rule for which tensors get quantized (`ModelLoader::tensor_should_be_converted`)
over the tensor index in the file's header. A surprising amount of a transformer is left
alone — every bias, every norm scale, every embedding — so an estimate that assumed
otherwise would under-predict by enough to matter. Measured against real conversions it
lands within 0.01%.

**A scaled-fp8 file has its scales folded in afterwards.** ComfyUI's "fp8_scaled"
checkpoints store each weight as raw fp8 numbers (up to ±448) next to one multiplier that
sd.cpp applies *after* the matrix multiply, and `-M convert` copies that layout across
unchanged. Each multiply then adds up values hundreds to thousands of times their real
size, which overflows on Vulkan and comes out as a mesh over the picture that gets worse
every step. So once sd-cli finishes, the converter rewrites the per-block multipliers in
place and sets each scale to 1 (`src/foldscale.rs`). The result says how many it folded.
A file converted before this existed is repaired with `yasdig fold-scales <file.gguf>`,
in seconds to minutes rather than another conversion; `doctor` names any file that needs it.

**It runs single-threaded on purpose.** sd.cpp's streaming converter starts one worker
per physical core and has them all call `ModelLoader::load_tensor` on a single shared
loader, which re-enters `load_tensors` → `process_model_files` and mutates that loader's
own state with no lock. At its default thread count it aborts inside glibc — `double
free or corruption (out)` — on roughly one run in three. A crash would be survivable, but
the corruption is in the loader's bookkeeping while other threads read through the
mappings it is remaking, so the same race can put wrong bytes in the output instead of
aborting, and a silently-wrong 8 GB model is the worst thing this could ship. The price
is measured: 254 MB of VAE takes 2.07 s serialized against 0.53 s at four threads.

## Postprocessing

Eleven sliders live in the **Adjust** panel of the **Refine & adjust** dialog — click
the picture.
They work like a photo editor rather than a generation setting: drag one and the image
re-filters under the cursor. There is no diffusion involved — the server re-runs the GPU
filters on the unfiltered image it already retained — so a re-tune costs milliseconds
against the minutes a regenerate would, and it stays lossless however many times you do
it.

| | |
|---|---|
| **Tone** | **Lightness** overall brightness, about ±1.5 stops. **Contrast** steepens or flattens the curve about mid-grey. **Highlights** and **Shadows** move one end of the range and leave the other alone — pull Highlights down to recover a blown sky, push Shadows up to open the dark half. |
| **Colour** | **Saturation** (-1 is exactly greyscale, +1 is double). **Vibrance** is saturation for the dull colours only: muted ones gain most, vivid ones barely move. **Temperature** blue↔yellow and **Tint** green↔magenta, scaled so black stays black. **Hue** rotates every colour, ±180°. |
| **Detail** | **Clarity** and **Sharpness**, described below. |

They are in the dialog because that is the only place you can see what they do. The two
detail filters work at or near the pixel level, and the result column shows a fitted
preview a few hundred pixels across; a sharpness setting chosen there is a guess. The
dialog opens with the whole frame fitted, which is the right view for a grade and for
clarity, and **1:1** switches to true pixels, which is the right view for sharpness —
drag the image to move around it. Only the **×** and Escape close it, so a stray click
while you are working can't dismiss the thing you are working on. **Reset** puts every
slider back to where this image started (and removes an area), and a double-click does
the same for one.

The **Adjust** panel holds folds: **Tone**, **Colour** and **Detail** hold the
sliders, then **Crop** and **Area**. Folded, each says what is set inside it.

The nine colour knobs are one GPU pass between the upscale and the detail filters, so
moving nine of them costs exactly what moving one does. They are all 0 by default and
0 is exactly off — the pass is skipped outright rather than run as an identity, so an
ungraded image is byte-for-byte what came out of the upscaler.

**Clarity** runs **-1 to 1**. Positive is a wide unsharp mask on brightness: it moves
the mid frequencies that read as contrast and depth, and leaves fine detail alone.
Reach for it when the image looks flat. Depth is the one thing refine *can't* give you
— a low-strength pass only ever touches fine texture, and raising strength to reach the
coarse structure re-composes the picture instead. Negative runs the same filter
backwards, subtracting that band instead of adding it, which flattens mid-scale
structure into something softer — the same move as negative Clarity in a photo editor,
and the right one when a render came out overcooked.

**Sharpness** is FSR's RCAS, working the fine band; its limiter rules out ringing, so
the top of the range is usable. If the image looks gritty, sharpness is amplifying it —
try more clarity and less sharpness. 1.0 is the reference filter at full strength and
there is nothing above it: FSR parameterizes sharpness as `exp2(-stops)`, and a lobe
bigger than the limiter permits doesn't sharpen harder, it inverts the filter.

Both filters size their working band from the **image**, not from the pixel grid, so a
setting means the same thing before and after a hi-res pass — clarity's radii are a
fixed fraction of the longest side, and RCAS samples its cross further out on a large
image (one pixel at 1536, two at 3072). Without that, a filter with a fixed footprint
quietly loses its grip as the picture grows: at 3072 a 3×3 kernel is grading a band
three times finer, relative to the subject, than the same kernel at 1024.

0.0 is off for both, and off is exact: neither filter touches a pixel at zero. Both
default to off, so the first image you see is what the model and the upscaler actually
produced — everything on top of it is a choice you made and can take back.

**The Outputs tab is the exception to all of this, and it is worth knowing which way.**
Everywhere else the sliders re-filter an unfiltered image the server kept, so they are
lossless however many times you drag them, and they start at the model's values. On the
Outputs tab the file *is* the filtered image — it is what you saved — so all ten start
at 0, which there means "as saved", and **Reset** goes back to the file on disk rather
than to a model's numbers. What you drag stacks on what was baked in. That is inherent
to editing a finished picture, and it is why that tab offers no re-render: a second
grade on top of a first is a look, while a diffusion over a sharpened image is a
mistake.

Clarity and Sharpness reset to the model's values on every new image, including after a
**Refine** or **Hi-res**. That is deliberate: an escalation replaces the pixels, so
whatever sharpening looked right a moment ago is stale by definition, and inheriting it
would hide what the pass actually did under a filter chosen for the previous image.
Dialling it back in costs a drag, not a render.

**The nine colour sliders survive Refine and Hi-res**, which is the other half of the
same argument: +0.2 saturation means the same thing at any size, so there is nothing
stale about it, and losing a grade every time you climb a rung would be a reason not to
climb it. It is re-applied to the new pixels rather than stacked on them — the escalation
diffuses the ungraded image, as everything here does. A fresh generate — from noise
or from a picture — or reopening a work item all start from neutral again.

A different config file can be passed as the first argument:
`cargo run --release -- /path/to/models.toml`.

### Area: pick by colour

An area can select by **colour** as well as by shape. **Whole** means no shape at
all — every pixel of the selected colours — while **Rectangle** and **Ellipse** limit
it to the shape too. **Colours…**, beside *Whole image* and *Area*, opens a wheel over
the picture (making a whole-picture area first if there is none):

- **The rim is every hue.** Click it to start a range there. The band inside it shows
  what is selected — white fully, black not at all — and the four dots set it: the
  two filled ones are where full selection starts and ends, the two hollow ones where
  the fade reaches nothing. A dot pushes the ones it runs into, and they spring back
  if you drag it away again before letting go. Drag the white part to move the whole range. A red range
  can run across the top, from magenta to orange.
- **Lightness and Saturation** are the two bars in the middle, with the same dots —
  "only the shadows", "only the strong colours".
- **Feather**, the track under the bars, is the Colour feather slider (below) — here
  too, so you can watch its grey preview while you drag it.
- **Double-click a dot** to go back to "any" for that channel (or 0 for Feather).
- **Drag the dark middle** to move the wheel anywhere on screen; it opens where you
  left it next time. Double-click the dark middle to put it back over the picture.

While you hold a dot, the picture turns into a grey map of what is selected. Move
back to the panel and the wheel gets out of the way: the area's sliders now change
only those colours — deepen just the blue of a sky, warm just the skin. **Invert**
selects everything else, and **Brushes only paint inside the selection** limits the
brushes the same way: dodge across the whole frame and only the selected colours
lighten. Greys never count as a hue, so a hue range doesn't pick them.

**Colour feather** softens the selection in the picture itself. The fade between the
dots softens it by colour, but where a sky meets a roof the colour changes in a pixel,
and an extreme change shows that pixel as a hard edge (or as speckle, where the colour
is noisy). Feather blurs the selection there, up to 2% of the picture's short side.
The shape's own soft edge is **Shape feather**.

### Crop

Open the **Crop** fold (in the Scale panel) and a box appears over the picture: drag its corners or edges,
drag inside it to move it, or drag anywhere else on the picture to draw a new one.
**Aspect** locks it to a shape (Original keeps the picture's own), and the
**⟲** button beside it turns that shape between landscape and portrait; the size under it
is what you will get, rounded down to a multiple of 8 pixels. **Apply crop** crops;
closing the fold or **Cancel** leaves the picture alone. At 1:1, pan with the middle
button or Space+drag while the box is up.

A crop is a step in the image's history, not a filter: it shows in **History**, which
steps back from it, and a Refine or Hi-res after it
works on the cropped picture — crop to the face, then Hi-res it. The sliders keep
their values. On the Outputs tab, **Save changes** writes the crop over the file.

### Area

**Area** gives one part of the picture a look of its own. In the **Selection** panel, press
**Area**: an ellipse appears, and the sliders now adjust only inside it — on top of the
whole picture's look, so 0 means "as the rest". **Whole image** points the sliders back.
Drag the shape like a crop box; choose **Rectangle** or **Ellipse**, set **Feather**
for how soft its edge is (the dashed lines show where the blend starts and ends), and
**Invert** to adjust everything *but* the shape — darkening the edges around a
subject, for instance. **Remove area** drops it.

The area follows the picture through a Refine or Hi-res, keeps its colour settings and
resets its detail ones, as the whole picture's do. A crop moves it with the picture.

### Brushes

The **Brushes** panel paints an effect exactly where you want it: **Dodge** lightens,
**Burn** darkens, **Saturate** and **Desaturate** add and take away colour, **Blur**
softens, **Sharpen** crisps edges. Pick a tool and paint on the picture; the effect
appears when you let go. **Amount** sets how strong the chosen tool is, and moving it
changes every stroke of that tool at once. **Size** and **Softness** shape the brush
(the ring shows it). Paint with the right button, or tick **Erase**, to take paint
back off; **Clear this tool** and **Clear all** start over. The painted mask shows in
red while the pointer is over the picture. At 1:1, pan with the middle button or
Space+drag.

Strokes stay live until you apply: a crop keeps them on the same pixels, a Hi-res
carries them to the bigger picture, and Reset clears them with the rest of the look.

**Restore** paints an earlier step back in — refine the whole picture, then take back
the parts you liked better before. Pick **Restore**, choose the step under **Restore
from** (the step before, unless you say otherwise), and paint: those pixels return
from that step, with the brush's soft edge, and **Amount** is how much. Your sliders
apply to the combined picture, so there is no seam in the grade. **Apply adjustments**
makes the combination a new step. Restore strokes belong to the history they were
painted against, so a new step (a refine, a crop, an Apply) drops them; jumping
through History keeps them. A step of another size — before a Hi-res — is scaled to
fit, so it comes back softer; it works best between refines. If you know *before*
refining which part should change, **Refine area** in the Refine fold redraws only
what you paint instead.

### Download

**Download**, at the foot of the panel, saves the picture as you see it —
full size, every slider, area, brush and crop included — to your browser's downloads
folder, as `yasdig-….png`. Like *Save to output*, it carries the settings it was made
with when *Settings in the PNG* is on, so dropping it back on yasdig restores them.
Nothing is written to `work/output/`.

### Apply and History

The sliders, the area and a crop are all live until you decide otherwise.
**Apply adjustments**, at the foot of the History panel, makes the look part of the
picture: the picture doesn't change on screen, but every slider goes back to 0 and the
next change starts from the adjusted picture. Each Apply, like each crop, is a step in
**History**, labelled with what it changed — *Adjust: Lightness +0.30,
Vibrance +0.20*. Click any step to go back to it, or forward again; adjusting and
applying from an earlier step starts a new branch from there, as an undo does.

Until you apply, a Refine or Hi-res works on the picture as generated and your sliders
carry over to the result, as they always have. After you apply, they work on the
adjusted picture — that is the point of applying — and the step before the Apply in
History is the untouched one to go back to.
