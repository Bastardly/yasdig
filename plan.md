# ~~Feature - Allow VAE + misc selection~~ — DONE
Currently we are using an existing VAE, but it would be nice to be able to select one from the list, preferbly one that is filtered to that specific model family (If possible)

Note, that when implement Flux etc. We may need to be able to select other models like text encoders etc. So it would be nice to have an option to choose these models under the "model" section. Perhaps rename the "Model" label to "Checkpoint" or "Diffusion model".

Then based on what model family you choose, the required support fields shows up like VAE, Text Encoder etc... Vae may be optional?

Also, could we make each model selection field full width, place them under a "Models" <details> with summary as we've done under settings -> advanced. (Not the high-res and refine model selection.) We may need to do this more. But that's another feature.

Also, could we move seed under steps and CFG scale? E.g. next to the sampler select?

## Progress

- **Done.** `ImageFamily::slots()` is the one table of parts (SD/SDXL: VAE, optional; Z-Image: VAE + Qwen3-4B
  text encoder, required); `Profile.bundled` so a checkpoint keeps `-m` with a `--vae` override;
  `GenRequest::parts` / `/img2img`, resolved by `Config::resolve_part` against the scan; `/models` serves each
  slot's fitting files; `[parts."<name>"]` in `user/model-settings.toml` (a sibling table, not a key in the
  entry); the Image tab's **Models** fold (*Diffusion model* + a select per slot, summary line, open state
  remembered per browser), Seed beside Sampler, the Per model card's part selects, reopening restores them.
- Also: **+ Add LoRA** is disabled, with a title saying why, when no LoRA fits the model.
- Left for later: text-encoder slots for SDXL (CLIP-L/G) and a bare SDXL UNet, which has never been assembled;
  `--taesd` as an optional preview-decoder slot.
