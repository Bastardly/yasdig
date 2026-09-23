# ~~Feature 1 - Split high-res and refine model~~ — DONE
Currently the same setting handles both high-res and refining. It would be nice to be able to split these up in separate models.
For the UI, we should probably have a line between both types. Also, each part should have their own CFG scale, we the default to the origina.

- side note. There is a big, where the refiner model doesn't get saved to the workflow. This would be nice, if this was fixed in this go.

**DONE.** The Advanced panel on both image tabs is now two groups with a rule
between them — *Hi-res pass* and *Refine pass*, hi-res first because that is the
order the buttons above a result are in — and each carries its own steps,
strength, CFG and checkpoint. The server was already per-pass: `/refine` and
`/hires` each build their own `EscalateRequest`, so the single picker only ever
existed because `escalateSettings()` built one object for both buttons. The
checkpoint has moved out of that object and onto each button's own request.

The two CFGs are real per-model settings, `refine_cfg_scale` and
`hires_cfg_scale`, resolved through `Profile::refine_cfg` / `hires_cfg`: the
pass's own knob, then `img2img_cfg_scale`, then `cfg_scale`. That middle rung is
"the default to the original" made literal — a settings file or a `[templates.*]`
that already pins the old shared *Refine/hi-res CFG* keeps running both
escalations at exactly that number. `img2img_cfg_scale` keeps its name and now
governs only the Image-to-image tab's own pass, which is what it has always been
called, so its box left the Image tab and its Settings row moved to the
*Image to image* section.

**The bug is fixed.** `refine_model` and `hires_model` are request-only fields on
`Profile`, exactly like `loras`: no `Defaults`/`Template` key, filled from the
`/generate` and `/img2img` bodies, and therefore archived in the work item's
sidecar. Reopening an item puts both selects back. They are also the one thing
that survives `swap_model`'s wholesale replace — no checkpoint states them, so
there is nothing of the incoming profile's to merge with — and each pass writes
its own choice back, so a saved output records what actually ran.

- Also in this go: **Output upscale** is a box in the Advanced panel rather than
  a settings-only knob. `upscale_factor` reaches `GenRequest` for the first time,
  with a live note under it saying what the base size becomes, and `1` is not a
  small enlarge but EASU skipped entirely — which is how you see the pixels the
  checkpoint actually drew. Image tab only: the Image-to-image pass has no EASU
  stage.
- Worth knowing, since it decides whether two checkpoints are affordable: nothing
  is held between passes. Every pass is a separate `sd-cli` process that loads its
  weights and frees them on exit, so the pipeline already reloads the *same* file
  once per rung. A second model costs page cache, not VRAM.
