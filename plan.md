# ~~Feature - Clean up image tab menu ui~~ — DONE

Currently, the menu is a bit messy and overwhelming, and the workflow has a few dents as well. 
Also, there are some minor tweaks that the elevate the UX by quite a bit.

Currently, we have a <details> with Summery on models only. I wish to expand on that to allow the user to filter out the large amount of settings by collapsing the segment. 

I'll mark the beginning and of each "details" as <collapse name></collapse name> (Where name is the summery) as how I wish to use this. In the code, just use the <details> and summary like before.

So here is how I imagine the layout of menu to be - based on labels and button names:

Generate button
Images, Seed
Keep going until stopped
<collapse Generation>
    Base width, Swap, Base height // (Swap is a new button that shows a swap icon or "<>" to swap width and height)
    Steps, CFG scale
    Sampler
</collapse Generation>

<collapse After each generation>
   After each image
   Output upscale, Upscaler
   Hi-res pass, Refine, Save to output
</collapse After each generation>

<collapse Image to image>
    // Clear current text, and remove the "Start from" header
    Selected Image // The image user has chosen or uploaded
    Choose image, Upload image, Clear image (When selected image is prensent)
    img2img steps, img2img CFG // Note, if steps and CFG could be set from the models's settings, that would be great.
    Strength, 
    Working size
</collapse Image to image>

<collapse Prompts>
  Prompt
  Negative Prompt (Optional)
</collapse Prompts>
<collapse Models>
    // Same as now
</collapse Models>

<collapse Loras>
    // The add loras buttons
</collapse Loras>

<collapse Hi-res pass>
    Hi-res model
    Hi-res steps, Hi-res CFG // Note, if steps and CFG could be set from the selected hi-res models's settings, that would be great.
    Hi-res strength, Hi-res scale // If we could make Hi-res scale into an input type number, and clamp it were needed, that would be great - see "Output upscale" for reference
</collapse Hi-res pass>

<collapse refine pass>
    Refine model
    Refine steps, Refine CFG // Note, if steps and CFG could be set from the selected refine models's settings, that would
    Refine strength
</collapse refine pass>

Save as default for this model

## Progress

- **Done.** The controls column is folds (`<details class="fold">`), each summary saying what is
  inside; open state remembered per fold, per browser. Images + Seed share a row; Swap (⇄) between
  base width and height; Output upscale moved into *After each generation*.
- *Image to image* replaces the *Start from* heading: thumbnail above its buttons (the Video tab's
  first-frame slot shares the layout), opens itself when a source arrives, summary says `from …`.
- **img2img Steps and img2img CFG removed**, end to end (`source_steps`, `img2img_cfg_scale`): a pass
  over a source runs the Generation fold's Steps and CFG scale. The refine/hi-res CFG chain is now
  pass CFG → `cfg_scale`. Old keys in settings files and sidecars are ignored and dropped on the next Save.
- Picking a Hi-res / Refine model refills that fold's steps, strength and CFG from the model;
  *Save as defaults* skips a pass whose fold holds another model's numbers.
- Hi-res scale is a number box (1–4, clamped on change); the selects that mirror it add an option
  for an unlisted value like 2.5.
- Follow-up: the *After each image* heading and the run summary line (`#runnote`) are gone; Image to
  image sits below Prompts; LoRAs moved into the Models fold (named in its summary); a picked source
  shows its summary in `--accent-hi`.
