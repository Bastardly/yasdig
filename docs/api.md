# Command line and HTTP API

yasdig is a local web server; the page is one client of these routes, and anything else on your machine can be another.

## Command-line tools

Six subcommands, all of which skip the server. `llm-selftest` runs one agent turn —
the driver, its sandbox, the GPU and the reply grammar — with an agent model (its name,
or `-` for the first installed) and, given a picture, asks it to describe it; it prints
the reply, whether it was the asked-for JSON, and the load, read and write times. `upscale` runs the initial enlarge
exactly as a generate would, with a model — a name in the upscaler dir or a path — and
needs no checkpoint (sd-cli only for an ESRGAN), so it is the fast way to try a new
upscaler file; at a whole-number factor it also prints the consistency check described
under [Upscalers](guide.md#upscalers). The last three touch only the GPU — no sd.cpp, no
checkpoint, no model weights — which makes them the quick way to check the Vulkan side
in isolation.

```sh
yasdig doctor [models.toml]      # run the startup checks and report, without starting
yasdig upscale in.png out.png <model> [factor=2.0] [models.toml]  # the model enlarge alone
yasdig fold-scales <file.gguf>...  # fold a scaled-fp8 GGUF's scales into its weights
yasdig llm-selftest [agent|-] [picture.png] [models.toml]  # one agent turn, no server
yasdig fsr in.png out.png [upscale=2.0] [sharpness=0.35] [clarity=0.0]  # no colour knobs: UI only
yasdig fsr-selftest [out.png]    # synthesize patterns and check the filters
yasdig sr-selftest               # the upscaler shaders against CPU references, plus speed
```

From a source checkout, put them after `--`: `cargo run --release -- doctor`.

Each post-process stage is skipped when it would be a no-op — EASU at scale 1.0, the
grade with all nine knobs at 0, clarity at 0.0 — so `yasdig fsr in.png out.png 1.0 0.5`
is an exact sharpen-only pass, and `yasdig fsr in.png out.png 1.0 0.0 0.0` is a
byte-exact copy. That is also what **Output upscale** means by `1`: not a small enlarge,
but no EASU pass at all, which is how you see the pixels the checkpoint actually drew.

`fsr-selftest` verifies the output dimensions, asserts that every filter at 0 really is
a byte-exact copy (the promise above), checks each of the nine colour knobs
one at a time — that it reaches the GPU at all, and that lightness, contrast, saturation
and temperature run in the direction they claim — and then measures how much each detail
filter moves each spatial frequency, at three image sizes, because those two size their
band from the image rather than the pixel grid and that is the claim worth checking. It
needs a Vulkan device and nothing else.


## API

- `GET /` — the web UI.
- `GET /templates` (alias: `GET /models`) — JSON list of the registry, templates and
  discovered checkpoints alike. Built fresh on every call, so a newly-dropped checkpoint
  appears without a restart. Each entry carries `name`, `description`, `discovered`,
  `exists`, `family` (`sd1`, `sd2`, `sdxl`, `z_image`) and `family_label`, `tuned` (it
  has per-model settings), `missing` (every file standing between it and a run, at the
  path it is expected at — empty when `exists`), `parts` (one entry per separate file
  the family runs with: `slot` — `vae`, `llm` —, `label`, `optional` — may it be left
  empty, true on a single-file checkpoint —, `current` and `current_path` — what the
  model runs with now, as a name relative to the models dir and as the path a work
  item's sidecar records —, `auto` — what the scan picks with nothing chosen, `null` on
  a checkpoint, meaning its own — and `options`, every file that fits the slot as
  `{name, path}`, already filtered for the family), `base_width` / `base_height`, `out_width` / `out_height`, and the profile's
  `steps`, `cfg_scale`, `sampler`, `scheduler`, `upscale_factor`, `upscaler`, `refine_steps`,
  `refine_strength`, `refine_cfg_scale`, `hires_steps`, `hires_strength`, `hires_cfg_scale`,
  `source_strength`, `hires_max_side`, `sharpness`, `clarity` — which is
  what lets the UI pre-fill its controls when you pick a model. `hires_max_side` is
  published because *Image to image*'s working-size control starts there, and a
  second answer to "how large can this card afford to diffuse" computed in JavaScript
  would be a second answer free to disagree. The two pass CFGs are
  `null` when the profile doesn't set one; that is deliberately distinguishable from the
  same number as `cfg_scale`, because unset *follows* the base CFG rather than matching
  it once.
- `GET /model-settings` — every model's per-model layer: `{"models": [{name,
  family_label, base, over, parts, parts_over}]}`, where `base` is what the model starts at *without* its
  own settings (the Per model card's placeholders), `over` is what
  `user/model-settings.toml` holds for it, `parts` is the same slot list `/models`
  serves and `parts_over` the files saved for it (slot → name). A broken file answers 200 with an `error`.
- `POST /model-settings` — `{"model": name, "values": {knob: value | null}, "parts":
  {slot: name | null}}`, both optional. `parts` names a file relative to the models dir,
  which must be one the scan found and one that fits the slot; a name equal to what the
  scan would pick anyway is not stored. A missing
  key is left alone, `null` removes that setting, a value sets it — and then everything
  equal to `base` is dropped, so only real differences are stored. Any setting a model
  can have (not `prompt`, `negative`, `hires_scale` or `variables`); an unknown key is
  a 400 rather than a silent no-op. Answers with the same shape as the GET.
- `POST /generate` — **queues a run** and answers `{ "job": 7 }` at once; the images
  arrive through `GET /jobs` and `GET /jobs/latest` below. Only `prompt` and
  `template` are required:
  ```json
  { "prompt": "…", "template": "sdxl-turbo", "negative": null, "seed": -1 }
  ```
  `references` — output stems, at most 4, for a model that takes reference images
  (Flux.2 and FLUX.3 Klein) — are named in the prompt as image 1, image 2; `[]` is none, and a
  model that doesn't take them refuses any.
  The run itself: `count` images (default 1, at most 1000), or `"loop": true` to keep
  going until stopped; a pinned `seed` advances by one per image. There is one loop at
  a time: a `"loop": true` request while one is queued or running replaces its
  settings from its next image (same job, count kept) and answers
  `{ "job": id, "updated": true }`. With `"update_only": true` it never starts a loop,
  and answers `{ "job": null, "updated": false }` when there is none. `after` is the
  *After each generation* fold — each pass `null` to skip it, `model` empty or absent
  for the checkpoint that made the image, and the escalation knobs below apply to
  both passes:
  ```json
  { "count": 4, "loop": false,
    "after": { "hires": { "upscale": 2, "model": null },
               "refine": { "model": "sd_xl_base_1.0" }, "save": true } }
  ```
  Anything the generate itself refuses — an unknown model, an empty prompt — is found
  when the run reaches the worker and is reported as a `notice` on `/jobs`.
  Every generation setting may also be overridden per-request; each is optional, and
  omitting one uses the model's profile value:
  ```json
  { "steps": 20, "cfg_scale": 5.0, "sampler": "euler", "scheduler": "auto",
    "width": 768, "height": 768, "upscale_factor": 2.0, "upscaler": "4x-UltraSharp.pth",
    "refine_steps": 2, "refine_strength": 0.3, "hires_steps": 8, "hires_strength": 0.3,
    "sharpness": 0.35, "clarity": 0.0,
    "loras": [{ "name": "detail.safetensors", "strength": 0.8 }],
    "parts": { "vae": "vae/sdxl_vae.safetensors" } }
  ```
  `parts` is what the **Models** fold picked: slot → a file relative to the models
  dir, or `null` to empty the slot (only on a single-file checkpoint, which then uses
  its own). An absent slot keeps the model's. A name the scan didn't find, or a file
  that doesn't fit the slot — a Flux VAE for an SDXL — is refused before anything
  runs. Retained with the image, like `loras`. `/img2img` takes it too.
  `upscaler` is a file name in the upscaler dir, or `""` for FSR — absent keeps the
  profile's. A name that isn't on disk is a `500` naming what is.
  `loras` is the one list here, and absent is not the same as empty: omitting the key
  keeps whatever the profile has, `[]` clears it. `name` is the path relative to the
  LoRA dir, extension included; `strength` is any finite number (1 is as trained,
  negative subtracts). A name that isn't on disk is a
  `500` with the available names in it rather than a render that quietly lacks it. They
  are retained with the image, so `/refine` and `/hires` run the same adapters — and
  they are *not* an escalation knob, so a `model` swap in one of those bodies replaces
  them along with the rest of the profile.
  The overrides are applied to the profile *before* it is retained with the image, so a
  later `/refine` or `/hires` starts from the settings that generated it — the four
  escalation knobs can then be moved per pass, see below. Omitting
  `refine_cfg_scale` or `hires_cfg_scale` leaves it *unset* rather than pinning it, so
  that pass keeps following whatever `cfg_scale` this request used. `sharpness` and
  `clarity` are the exception, because they aren't generation settings: they set the
  image's starting filters, `/adjust` moves them from there, and every later pass resets
  them. The web UI leaves both out for that reason. The nine colour knobs aren't here
  at all — they are not per-model, so they start at 0 on every image and only `/adjust`
  sets them. `seed` defaults to `-1` (pick one).
  Each image, as `GET /jobs/latest` serves it (and as `/refine`, `/hires` and the
  reopen routes answer):
  ```json
  { "image_base64": "…", "width": 1536, "height": 1536, "seed": 12345,
    "refine_passes": 0, "hires_passes": 0,
    "image_id": 3, "work_id": "1758104523000-12345",
    "can_hires": true,
    "filters": { "sharpness": 0.35, "clarity": 0.0, "lightness": 0.0, "contrast": 0.0,
                 "highlights": 0.0, "shadows": 0.0, "saturation": 0.0, "hue": 0.0,
                 "temperature": 0.0, "tint": 0.0 },
    "filters_start": { "sharpness": 0.35, "clarity": 0.0, "…": 0.0 },
    "prompt": "A lovely crab wearing a blue hat",
    "timings": { "gen_ms": 0, "upscale_ms": 0, "fsr_ms": 0, "refine_ms": 0, "hires_ms": 0 } }
  ```
  `upscale_ms` is an ESRGAN model's share of the enlarge, reduce included, and `0` when
  EASU did it. `timings.lora_note`, when present, is a LoRA sd.cpp warned about — part of it
  not applied, or tensors skipped for the wrong shape — in words, then sd.cpp's counts.
  Only sd.cpp's own warnings make one; a suspicion goes to the server's console. `image_id` addresses the retained intermediates; `can_hires` says whether that
  image is still inside the hi-res budget (whether a pass has steps is the request's
  `refine_steps`/`hires_steps`, so the client decides that); `filters` reports all ten display filters as
  actually applied, which is what the UI's sliders reflect, and `filters_start` reports
  where they would start on a new image from the same model — the two differ after an
  escalation, which carries the colour grade across, and `filters_start` is what the
  viewer's **Reset** goes back to. `work_id` is the archived
  generation this descends from — it outlives the process, unlike `image_id`, and is
  `null` only if the archive write failed. `prompt` is the text that was actually
  conditioned, with every [`{variable}`](guide.md#prompt-variables) already drawn — it is
  what the work item records, and what a later `/refine` or `/hires` of this image
  runs with.
- `POST /png-settings` — a PNG's bytes, as-is (like `/upload`, but the original file,
  since a canvas re-encode strips text chunks). Answers the settings it carries,
  matched against what is installed: `{source, notes, item?, params?}`. `source` is
  `yasdig` (`item`, the shape a work item restores from: prompt, negative, typed
  prompts, seed, template and a `profile` of the Image tab's knobs — no paths),
  `a1111` (`params`: only the fields its `parameters` text named — prompt, negative,
  seed, steps, cfg_scale, sampler, scheduler, width, height, template, loras), or
  `none`. `notes` says what couldn't be applied: a model or LoRA not installed, a
  sampler sd.cpp hasn't got, a ComfyUI graph. A 400 for a body that isn't a PNG.
  Answered on the connection thread, with the 32 MiB body limit.
- `POST /upload` — a route with a **binary body**: a PNG, sent as-is with
  `Content-Type: image/png`. No multipart, no base64 — `Request.body` is bytes
  already, and skipping the envelope is a third off the wire. It lands in
  `work/output/` under a minted stem and answers
  `{ "stem": "…", "width": 1024, "height": 768 }`. The stem is minted rather than
  taken from the filename because `valid_id` — the traversal guard for every
  `/outputs/{stem}` route — is a whitelist of alphanumerics and `-`, which almost
  no real filename satisfies. PNG only: the page re-encodes whatever you gave it in
  a canvas first, so the format support tracks your browser's rather than a Rust
  crate's feature list. Answered on the connection thread, and it is the one path
  `http.rs` gives a large body limit (32 MiB; anything over is a `413`).
- `POST /upload-audio` — a **binary body** too: a 16-bit PCM WAV (`Content-Type:
  audio/wav`), kept in `work/output/` as `{stem}.wav` for an H3 shot to follow.
  2–15 seconds (MiniMax's limit for one reference sound), else a 400 saying so.
  Answers `{ "stem": "…-sound", "seconds": 4.2 }`. The page decodes any format the
  browser plays and sends 32 kHz stereo. It is listed by `/outputs` with
  `audio: true` and `seconds`, served at `GET /outputs/{stem}/sound.wav`, and deleted
  and starred like a picture. Same 32 MiB limit.
- `POST /img2img` — one img2img pass over an image already in `work/output/`.
  `prompt`, `template` and `source` are required; the rest are the same per-request
  overrides `/generate` takes, minus `width`/`height` (sd.cpp derives the size from
  the init image) — `steps` and `cfg_scale` are this pass's own — plus:
  ```json
  { "prompt": "…", "template": "sdxl-turbo", "source": "1758104523000-0-import",
    "source_strength": 0.55, "max_side": 2048 }
  ```
  `max_side` is a ceiling — a source already inside it is used untouched, and a
  larger one is scaled down (Lanczos3) first; omitted, it is the model's
  `hires_max_side`. The source is validated on the connection thread, so a stale
  pick is an immediate `400` rather than one that arrives after the render in
  flight. Queued like `/generate` — `count`, `loop` and `after` included — so the
  escalations follow it unchanged, and each result is archived as a work item with
  the source stem recorded on it.
- `GET /jobs` — the queue, running entry first:
  ```json
  { "jobs": [{ "id": 7, "kind": "generate", "label": "sdxl-turbo", "done": 1,
               "total": 4, "state": "running", "skipping": false }],
    "latest_seq": 12, "notices": [{ "seq": 9, "message": "…" }],
    "progress": { "busy": true, "phase": "sampling", "step": 7, "steps": 20, "rate": "1.23s/it" } }
  ```
  `kind` is `generate`, `img2img`, `refine`, `hires`, `video`, `enlarge` or `convert`;
  `total` is `null` for a loop; `state` is `running`, `stopping` or `queued`;
  `skipping` is true while the image in flight is being cancelled by `/jobs/skip`.
  `latest_seq` moves whenever a job publishes an image — fetch it from
  `GET /jobs/latest`. `notices` are the last few failed runs, which have no request
  of their own to fail. Answered on the connection thread.
- `GET /jobs/latest` — the newest image any job made, in the shape above; `404` before
  the first.
- `POST /jobs/stop` — `{ "id": 7 }`. A waiting job is removed (a request waiting on it
  gets an error); a running run finishes the image in flight and ends; any other
  running job is killed. Answers `{ "stopped": "removed" | "stopping" | "cancelled" }`.
- `POST /jobs/skip` — `{ "id": 7 }`, the *Cancel generation* button. Kills the image a
  running run has in flight and goes on to the run's next image; the cancelled image
  counts, so a pinned seed walks past it. Answers `{ "skipped": true }`, or `400` for a
  job that isn't a running run, or in the moment between two passes of one image.
- `POST /refine` — `{ "image_id": 3 }`. img2img over the retained image at its current
  size, no rescale. Optional `refine_prompt` (`/hires`: `hires_prompt`) is what the
  pass is asked for; blank or absent uses the image's own prompt, which the image
  keeps either way. `/generate` and `/img2img` take the same two for their
  after-each passes. Optional `references` (output stems, at most 4) are pictures the
  pass follows, on a model that takes them; refused otherwise. Never sent by an
  after-each pass.
- `POST /redraw` — `{ "image_id": 3, "mask": "<base64 PNG or data: URL>", "prompt":
  "a red scarf", "strength": 0.5, "feather": 16 }`. Redraws where the mask is white.
  The mask must be the image's own size; the rest is optional (blank prompt = the
  image's own, strength 0.5, `feather` — edge softness in image pixels, 0–128 —
  16), and `references` as on `/refine`. Queued ahead of everything not yet running, like `/refine`, and answers in its
  shape, with `timings.redraw_ms`.
- `POST /edit` — `{ "image_id": 3, "prompt": "make it night", "references": ["…"] }`.
  Changes the image by instruction on a model that takes references: the image itself
  is reference 1, `references` (output stems, at most 3) follow it. `prompt` is
  required. `"expand": {"left": 256, "right": 256}` (any of `left`/`top`/`right`/
  `bottom`, each at most 512) redraws it in that grown shape — *Edit and rescale*. A
  history step, queued and answered like `/refine`.
- `POST /extend` — `{ "image_id": 3, "expand": {"left": 256, "right": 256},
  "prompt": null, "feather": 16 }`. Grows the canvas on those sides (each at most 512)
  and draws past the old edges with the installed FLUX.1 Fill model; 400 naming Get
  models when there is none. Blank prompt = continue the background, `feather` as
  `/redraw`'s (floored at 24 here). A history step, queued and answered like
  `/refine`, with `timings.redraw_ms`.
- `POST /upscale` — `{ "image_id": 3, "factor": 2, "upscaler": "4x-UltraSharp.pth" }`.
  Enlarges the retained image with no diffusion: FSR when `upscaler` is absent, `null`
  or empty, otherwise that model from the upscalers dir. `factor` is more than 1 and
  at most 4; the size is `fsr::out_size` whichever ran. A history step, queued and
  answered like `/refine`, with `timings.upscale_ms`.
- `POST /history/goto` — `{ "image_id": 3, "index": 1 }`. Make that step of the
  image's history the live one. Every result carries `history` (each step's label,
  oldest first) and `at` (the step on screen). A `publish`, no diffusion — answered on
  the display lane, so it never waits behind a render.
- `POST /history/view` — `{ "image_id": 3, "index": 0 }`. That step as the page would
  show it now — through the display filters currently applied — as `{ image_base64,
  width, height }`. Changes nothing; the before half of Compare.
- `GET /history/{image_id}/{index}.png` — a step's 256 px thumbnail, made on first
  request.
- `POST /hires` — `{ "image_id": 3, "upscale": 2 }`. FSR-upscale toward
  `upscale ×` the current size (1–4, default 1), img2img at up to `hires_max_side`, then
  a final FSR upscale if the target is beyond that budget.

  Both queue ahead of everything not yet running — so during a batch they run between
  two of its images — wait their turn, and answer in `/jobs/latest`'s shape. They can
  be chained in either order, any
  number of times; `/hires` stops being available once the image is past
  `hires_max_side`, `/refine` does not.

  Both also accept the knobs that describe the pass itself, which override what the
  image was generated with — this is what lets you render once and then try the same
  image at several refine strengths:
  ```json
  { "model": "sd_xl_base_1.0", "refine_steps": 4, "refine_strength": 0.35,
    "hires_steps": 8, "hires_strength": 0.3, "refine_cfg_scale": 2.5 }
  ```
  `model` runs the pass on a *different* checkpoint — a registry name from
  `/models`, or absent/empty for the one the image was made with. It belongs here
  for the same reason the others do: it describes a pass that hasn't happened yet.
  The swap is wholesale, so the named model's refine/hi-res steps, strengths,
  CFGs, `hires_max_side`, `weight_type` and `extra_args` all come with it,
  and the explicit fields in this body are then applied on top. The profile the
  pass actually ran with is what is retained, so `can_hires` and the
  filter starting points in the reply describe the model that ran.

  Each is optional and an omitted one keeps the retained value, so a bare
  `{ "image_id": 3 }` behaves exactly as it always has. The two pass CFGs are
  the fields where sending `null` differs from leaving it out: `null` *unsets* it, so the
  pass goes back to following `cfg_scale`. The base settings — `steps`, `cfg_scale`,
  `sampler`, `width`, `height` — are deliberately not accepted here: they describe a
  txt2img that has already run, and nothing they could say would change the image in
  hand. Change those and press **Generate**.
- `POST /adjust` — `{ "image_id": 3, "sharpness": 0.5, "saturation": -0.2 }`. Re-runs
  the display filters on the retained image. Every filter named in the `filters` object
  above is accepted here, flat; all are optional, and an omitted one keeps its current
  value, so adjusting one knob is a one-field request. No diffusion runs, so this
  returns in milliseconds — it is what the viewer's sliders call, and it is safe to call
  repeatedly because the image it filters is always the unfiltered one. The new values
  belong to that image and last until the next pass over it — `/refine` and `/hires`
  reset `sharpness` and `clarity` to the model's values and carry the nine colour knobs
  across. Same response shape as `/generate`, with the time reported in `fsr_ms`.
  `area` sets the image's one area with a look of its own: `{ "shape": "rect" |
  "ellipse" | "whole", "x0", "y0", "x1", "y1" (fractions of the frame), "feather"
  (0..0.5 of the short side), "invert", "filters": { …the same keys… }, "colors",
  "brushes" }`, applied over the whole picture's filters inside the selection —
  shape × colour. `colors` is `{ "hue", "lightness", "saturation", "feather" }`, each
  channel `null` (any) or `{ "a", "b", "c", "d" }`: fully selected from b to c, easing
  (smoothstep) to nothing at a and d, in OKLCh — hue in degrees (unwrapped: `a` in 0..360, the rest may run past
  360), lightness and saturation 0..100. `feather` (0..1, default 0) blurs the colour
  selection in the picture, σ up to 2% of the short side. `"brushes": true` limits the brushes to the
  selection. Absent keeps it, `null` removes it. Every result
  carries `area` (or `null`).
- `POST /brush` — `{ "image_id": 3, "tool": "dodge", "amount": 0.4, "mask":
  "data:image/png;base64,…" }`. Paints with one brush tool (`blur`, `sharpen`, `dodge`,
  `burn`, `saturate`, `desaturate`, `restore`): `mask` is the tool's whole mask at the
  image's size, its alpha the weight; `"mask": null` clears the tool; no `mask` sets
  only the `amount` (0..1). `restore` paints a History step back in, before the
  filters: `from` names the step (default the one before; not the one on screen), and
  a new step drops its strokes. Results carry `restore_from`. Display lane; same response shape as `/adjust`. Every result
  carries `brushes`, `{ tool: amount }` for each tool holding strokes, and
  `GET /brush/{image_id}/{tool}.png` serves a tool's mask back.
- `POST /apply` — `{ "image_id": 3 }`. *Apply adjustments*: the image as shown (filters
  and area in it) becomes the image itself, as a history step labelled with what
  moved; the filters and area reset to none and the slider start points to 0. Passes
  after it work on the adjusted picture. `400` when nothing is set; display lane,
  `409` when a pass replaced the image first.
- `POST /crop` — `{ "image_id": 3, "x": 64, "y": 0, "width": 768, "height": 768 }`, in
  the image's pixels. Crops the retained image as a history step (so `/history/goto`
  undoes it and a later pass works on the crop). Width and height are rounded down to
  a multiple of 8, must be at least 64, inside the picture, and not all of it. The
  filters are kept and an area is mapped into the new frame. Display lane, like
  `/adjust`; `409` when a pass replaced the image first.
- `POST /text` — `{ "image_id": 3, "layer": "data:image/png;base64,…", "label": "Hello" }`.
  The Refine & adjust dialog's Text panel: `layer` is a transparent PNG at the
  picture's size, laid over the picture *as it looks* (the filters, area and brushes
  baked first, as `/apply` does) as a history step named `Text: "…"` from `label`'s
  first 40 characters. The look starts again from zero. Display lane; `409` when a
  pass replaced the image first.
- `POST /resize` — `{ "image_id": 3, "factor": 0.5 }`. Scales the retained image
  *down* (Lanczos, no diffusion) as a history step; `factor` is between 0 and 1, and
  each side is rounded down to a multiple of 8 and must stay at least 64. Up is
  `/upscale`. The detail filters restart as after an upscale; an area is kept (it is in
  fractions) and brush masks are resized when read. Display lane, like `/crop`; `409`
  when a pass replaced the image first.
- `POST /save-output` — `{ "image_id": 3 }`. Copies the image on screen, filters and
  all, into `work/output/`. Returns `{ "path": "work/output/…png" }`.
- `POST /outputs/overwrite` — `{ "image_id": 3 }`, the Outputs viewer's *Save changes*:
  writes the image on screen back over the saved picture it was opened from, in
  `work/output/` and in your favourites if it is there too, keeping the file's date so
  the grid keeps its order. Only for an image opened through `/outputs/open` (a 400
  otherwise). Returns `{ "path": … }`.
- `POST /download-image` — `{ "image_id": 3 }`, the viewer's *Download*: the image on
  screen as `/save-output` would write it (settings in the PNG when
  `embed_png_settings` is on), but written nowhere. Returns `{ "image_base64", "name" }`,
  the name `yasdig-<stem>.png` — the file's own stem for a picture opened from Outputs.
- `GET /workitems` — every archived generation, newest first. Each carries `id`,
  `created_ms`, `prompt`, `negative`, `seed`, `template`, `width` / `height`,
  `thumb_width` / `thumb_height`, `source` (the `work/output/` stem an
  image-to-image generation was diffused from, `null` for a txt2img), and a
  `profile` object holding the full resolved
  settings the image was generated with — per-request overrides already applied, and
  the pass CFGs still `null` if they were left unset. That is what lets the UI
  restore an item's controls without a second request. Lists both
  shelves: a favourite is still a work item, and `favorite: true` says it lives in
  `user/favorite/` (its star is filled, and its routes are `/favorites/…`). An id on
  both shelves is listed once, as the favourite.
- `GET /workitems/{id}/thumb.png` — the thumbnail. An id is digits and `-` only;
  anything else is `404`.
- `POST /workitems/open` — `{ "id": "1758104523000-12345" }`. Copies the archived image
  back into the working set and returns the `/generate` shape with a fresh `image_id`,
  so the escalations and the sliders work on it immediately. No diffusion runs.
  `"pane": "agent"` opens it in the Agent tab's viewer instead (`Slot::Chat`), which
  takes neither the Image tab's picture nor a round the agent is drawing.
- `POST /workitems/delete` — `{ "ids": ["…", "…"] }`. Returns `{ "deleted": 2 }`.
- `POST /favorites/add` — `{ "ids": ["…", "…"] }`. Moves those work items — image,
  thumbnail and sidecar — into `user/favorite/`, out of reach of `Delete all` and the
  clear on close. Returns `{ "favorited": 2 }`. They stay in `GET /workitems`.
- `POST /favorites/remove` — the same body. Moves them back to `work/workitems/`.
- `GET /favorites`, `GET /favorites/{id}/thumb.png`, `POST /favorites/open`,
  `POST /favorites/delete` — `/favorites` lists that shelf alone; the other three are
  the routes above over `user/favorite/`. Same shapes, same ids, same guard.
  `delete` deletes the files; un-starring is `remove`.
- `POST /favorite-outputs/add` — `{ "stems": ["…"] }`. Moves those saved images into
  `user/favorite_outputs/`, keeping their stems and file times. Returns
  `{ "favorited": 1 }`. `POST /favorite-outputs/remove` moves them back.
  `GET /favorite-outputs` lists that gallery alone in the `/outputs` shape, and
  `POST /favorite-outputs/delete` deletes from there only. There is no open or thumbnail route of their own: a stem names one
  picture in either gallery, so `/outputs/open`, `/outputs/{stem}/thumb.png` and a
  `source` on `/img2img` or `/video` find a favourite by stem as well.
- `GET /progress` — what the worker is doing right now:
  `{ "busy": true, "label": "hi-res", "phase": "sampling", "step": 7, "steps": 20,
  "rate": "1.23s/it" }`, or `{ "busy": false }` when nothing is running. `phase` is
  `loading` while sd-cli reads weights and `sampling` once it is denoising — worth
  distinguishing, because a model that streams from RAM can spend minutes on the first
  and "step 0 of 20" is the wrong thing to show for all of them. `rate` is sd-cli's own
  text, unit and all. While a txt2img or img2img pass samples, the busy body also
  carries `"preview": 7` — the newest complete live preview — or `null` before the
  first one; `GET /jobs` embeds the same object as its `progress`.
- `GET /preview/{n}.png` — that preview, as a PNG at 1/8 of the render's size. Older
  frames are deleted as newer ones complete, so a 404 for an `n` that is no longer the
  newest is normal; ask for the number the last poll reported.
- `POST /cancel` — stops the run in flight, returning `{ "cancelled": true }` if there
  was one. This kills the `sd-cli` process, which is the only cancellation it offers;
  the pass was writing to a scratch file that is cleaned up on failure either way, so
  the image on screen is never a half-written one. The cancelled request itself comes
  back as an error whose message ends in `cancelled by request`.
- `POST /shutdown {clear}` — the *Close server* button. Stops the run in flight, and
  with `"clear": true` deletes what the three `clear_*_on_close` settings in the *saved*
  `user/settings.toml` ask for (absent or anything else deletes nothing), answers `{ "closed": true, "cleared": { "workitems": 0, "outputs": 3, "videos": 0 } }`
  and exits, freeing the port.
- `GET /loras` — every file in the LoRA dir, recursive: `name` (what a request puts in
  `loras[].name`), `label` (the same without its extension, for a picker), `bytes`,
  `usable`, `note` — why not, when `usable` is false — and what it was trained for:
  `base`, as a name (`SDXL`, `Flux.1`), and `families`, every family it fits in the
  spelling `/models` uses for a model's (`sdxl`, `flux`, `flux_schnell`, …). A list
  rather than one family because the rule is more than equality: a Flux.1 LoRA fits
  dev and schnell. `families` empty is a LoRA for something that is no image family
  here (a Wan or Flux.2 dev one) and fits no model; `base` empty is a LoRA nothing
  identified, and `families` then lists them all. Unusable entries are listed
  rather than filtered out, because "my LoRA isn't in the list" is a much worse thing to
  debug than "my LoRA is in the list and says why". Filesystem only: it answers on the
  connection thread, and costs one header read per file.
- `GET /upscalers` — every model file in the upscaler dir: `name` (what `/generate`
  puts in `upscaler`), `label`, `bytes`, `arch` (`ESRGAN`, `Compact`, `SPAN`,
  `SwinIR-M`, `HAT`, `unknown`, …) and `supported`. The architecture is read from the
  tensor names — a safetensors header, or a `.pth`'s pickle — never the weights, so it
  answers on the connection thread. See [Upscalers](guide.md#upscalers).
- `GET /outputs` — every image in `work/output/`, newest first: `stem`, `created_ms`,
  `width` / `height`, `prompt`, `seed`, `template`, and `favorite`. Lists both
  galleries, as `/workitems` does: `favorite: true` is an image in
  `user/favorite_outputs/`. Uploads are in here too, with an
  empty prompt and `template: "(imported)"` — one pool, because at the moment you are
  choosing a source image the difference is not interesting. Keyed by filename **stem**, not by
  work item id — several saves come out of one generation (`{id}-s{step}`, plus a `-2`
  suffix when that name is taken), so only the filename is unique. A PNG whose sidecar
  has gone missing is still listed with zeroed dimensions and an empty prompt: the file
  on disk is the thing you saved, and a picker that hid it would be lying.
- `GET /outputs/{stem}/thumb.png` — generated on the first request and cached, because
  output PNGs are full resolution and everything saved before thumbnails existed has to
  appear too.
- `POST /outputs/delete` — `{ "stems": ["…", "…"] }`. Returns `{ "deleted": 2 }`.
  Removes each PNG with its sidecar and thumbnail. A list rather than a single stem
  for the reason the work-item delete is one, and "delete all" is this request with
  every stem in it rather than a route of its own — which is what keeps the id
  whitelist the only guard the route needs.
- `POST /outputs/open` — `{ "stem": "1758104523000-12345-s0", "model": "sdxl-base" }`.
  Copies a saved image back into the working set and returns the `/generate` shape with
  a fresh `image_id`, so the sliders and the passes (`/refine`, `/redraw`, `/edit`,
  `/extend`, `/upscale`, `/resize`) work on it. No diffusion runs to open it. `model`
  (optional) stands in when the sidecar names no installed model (an upload); without
  one it still opens, and `/image/model` can pick one. Its passes answer their own
  request and are never published as the Image tab's latest result.
  Unlike the other `/outputs` routes this one goes through the GPU queue, because it
  ends in a filter pass. `filters` and `filters_start` both come back neutral: the file
  already carries whatever was applied when it was saved, so 0 means "as saved". A PNG
  with no sidecar opens too, with default settings.
- `POST /image/model` — `{ "image_id": 3, "model": "sdxl-base" }`. Makes that the model
  the image's passes run on (the profile is replaced, the pixels untouched) and answers
  in the `/generate` shape, `model` included. Not a fill-only model.
- `GET /video-models` — every video model: the ones found by scanning `models/video/`
  plus the `[[video]]` table, built fresh per call so a finished download shows up
  without a restart. Each entry is the config verbatim plus what the
  page would otherwise have to re-derive from `family`: `size_multiple`, `frame_grid`
  (`{multiple, offset, min, round_up}`), `forced_fps`, `is_moe`, `missing` (the component
  files not on disk), `duration`, and `parts`: one row per separate file its family runs
  with (`VideoFamily::slots`), in `/models`' `parts` shape — `slot`, `label`, `optional`,
  `current`, `auto` and `options`, every file under the models dir that fits. Served rather than reimplemented in the page on
  purpose — the page rounds the frame count to show a duration and the command builder
  rounds it to run the model, and two implementations of `4k+1` are how those start
  disagreeing.
- `GET /convert-models` — `{types, files}` for the Convert tab. `types` is the weight
  types `--type` accepts, each with its real bits-per-weight (`q4_K` is 4.5, `q6_K` is
  6.5625 — not the round numbers the names suggest); `files` is every model file either
  models dir holds that `-M convert` can be pointed at, each with the role read from its
  header, the `sd-cli` flag that role implies, its dominant stored type, and a predicted
  output size **per type**. Served rather than computed in the page for the same reason
  `frame_grid` is: the rule for what gets quantized is sd.cpp's, and a second copy of it
  in JavaScript would be a second copy free to disagree.
- `POST /convert` — `{source, type}`. `source` must be one of the ids `/convert-models`
  published; that is the traversal guard, and it is a whitelist in the same spirit as
  `workitems::valid_id` — the client chooses from what the server offered, it never names
  a path. Validation happens on the connection thread so a bad type or an existing output
  comes back as a 400 immediately rather than after waiting out whatever render is in
  flight. The conversion itself queues on the worker: no GPU is involved, but it
  saturates the cores and the disk for minutes and must not do that underneath a render.
  Going through the worker is also what gives it `/progress` and `/cancel` for free.
- `GET /catalog` — the Get models tab: `{curl, finished, models_dir, upscaler_dir,
  video_dir, entries, videos, upscalers}`. Each
  entry is a recommended set from `src/catalog.toml` (`id`, `family`, `family_label`,
  `label`, `note`, `bytes`) with its `files`, each carrying `slot`, `slot_label`, `dest`
  (under the models dir), `bytes`, `page` (its Hugging Face repository, where its
  licence is — derived from the URL) and a `state`: `have` (with `name`, the file that fills
  it — any file whose header fits the slot, not only `dest`), `missing`, `queued`,
  `downloading` or `partial` (with `got`, the `.part` file's length), `failed` (with
  `why`), or `wrong` (`dest` exists and is something else, which is never overwritten).
  `finished` counts files landed since start; the page reloads its models when it moves.
  `upscalers` has the same shape, one file each, with `family` `"upscaler"`, `slot_label`
  the architecture (`ESRGAN`, `Compact`, `SPAN`), `dest` under the upscaler dir, and
  `have` only when that file is there and reads as that architecture. `videos` is the video models' sets in the `entries` shape, `dest`
  under the video dir, `slot` a `VideoFamily::slots` key, `"model"` or the A14B's
  `"high_noise"`; a part counts as had only from the video dir, which is where discovery
  assembles a video model from.
- `POST /downloads/start`, `/downloads/cancel`, `/downloads/clear` — `{entry}`, a catalog
  id and nothing else: the page never names a URL or a path. Start queues the entry's
  files that nothing on disk fills yet; cancel drops queued ones and kills a running
  curl, **keeping** its `.part` so start resumes it; clear deletes the `.part` files
  nothing is writing. Answered on the connection thread — downloads have their own lane,
  not the GPU queue. An unknown id is a 400.
- `POST /video` — `model` and `shots` (at least one) are required; everything else
  optional:
  ```json
  { "model": "wan2.2-ti2v-5b", "negative": null,
    "source": "1758104523000-12345-s3",
    "width": 832, "height": 480, "fps": 24,
    "steps": 20, "cfg_scale": 6.0, "high_noise_steps": 8, "high_noise_cfg_scale": 3.5,
    "flow_shift": 3.0, "sampler": "euler", "vae_tiling": true,
    "parts": { "vae": "video/vae/wan2.2_vae.safetensors", "audio_vae": null },
    "shots": [
      { "prompt": "she turns to the door", "seed": -1, "frames": 33 },
      { "prompt": "she walks out", "seed": 1234, "frames": 49 },
      { "prompt": "she comes back in", "seed": -1, "cut": true, "references": ["1758104523000-12345-s3"] }
    ],
    "every": "a red-haired woman, a rainy street" }
  ```
  One sd-cli run per shot, each with its own `seed` (-1 or absent picks one) and
  `frames` (absent is the model's; rounded to the family's grid). Each shot after the
  first is rendered from the previous one's last frame (only on families that chain:
  TI2V-5B, A14B, H3), up to 20 in all — or, with `cut: true`, starts fresh: from a
  `keyframe` (a `work/output/` stem; any family that chains; a cut without one starts from its words),
  or, on MiniMax H3, from its words and up to four `references` (`work/output/` stems;
  `references: true` on `/video-models`). Never both, and references never beside a first
  frame: sd.cpp refuses H3's references beside a keyframe, so shot 1 takes them only with
  no `source`. A shot may also carry up to three `audio` stems (sounds from
  `/upload-audio`), one `--ref-audio` each, where `references` may go and on a model with
  its audio VAE (`sounds: true` on `/video-models`); 15 s in all. Shot 1's starting picture is `source`, never a `keyframe`. Every shot is conditioned on `"{every}. {its own text}"` — never on
  another shot's text — and `every`'s variables roll once, off shot 1's seed. An empty
  shot is refused by its number.
  `parts` picks the model's separate files, by name under the models dir as `/video-models`
  lists them; `null` leaves out an optional one (H3's audio VAE, VACE's CLIP-Vision), and
  an absent key keeps the model's own.
  `source` is a `work/output/` stem to use as the first frame; absent or empty runs plain
  text-to-video. `negative` absent inherits the model's (Wan's own published one);
  `negative: ""` means no negative at all. The overrides land on a *clone* of the config
  entry, which is then stored in the clip's sidecar, so a clip is reproducible from its
  own record. Takes **minutes** — poll `/progress`. Response:
  ```json
  { "clip": { "id": "…", "frames": 33, "fps": 24, "width": 832, "height": 480, "audio": false, … },
    "gen_ms": 214000, "note": null }
  ```
  `clip.segments` is one record per shot, `{text, conditioned, seed, start, frames,
  cut?, references?, keyframe?, audio?}` (typed text, what sd-cli was told, its seed, its first frame in the
  clip, how many frames sd-cli made — a continuing shot adds one fewer, its first being
  the seam — and whether it was a cut, with references or from a picture); `clip.prompt`/`clip.seed` are
  shot 1's; `clip.every` the typed
  *Every shot*. `note` says why a long clip has fewer shots than asked for (a Stop, or a
  shot that failed after others), and which LoRA sd.cpp left out (as `timings.lora_note`). Nothing is written to `work/output/` — that is
  `/videos/save`. A model with a soundtrack (MiniMax H3 with its audio VAE) is generated
  into an AVI by sd-cli — the only way it keeps the sound — and split into the frames
  and `audio.wav`, so `clip.audio` is true.
  `clip.width`, `height` and `frames` are measured from the frames on disk rather than
  echoed from the request, because sd.cpp aligns a size up and a frame count down and
  only warns about it.
- `GET /videos` — Current: every clip, newest first, starred ones included with
  `favorite: true`; each carries its full sidecar including the resolved model under
  `video`.
- `GET /favorite-clips` — the starred clips alone (`user/favorite_clips/`).
- `POST /favorite-clips/add`, `/favorite-clips/remove` — `{ "ids": ["…"] }`. Star or
  un-star: each moves the clip's folder between `work/videos/` and
  `user/favorite_clips/`. Its id and every `/videos/{id}/…` URL stay the same.
- `POST /favorite-clips/delete` — `{ "ids": ["…"] }`, from the favourites only.
- `GET /videos/{id}/thumb.png` — the first frame, `THUMB_HEIGHT` tall.
- `GET /videos/{id}/frames/{n}.png` — one frame, numbered from 0. The page fetches all of
  them and plays the sequence on a canvas. A non-numeric `{n}` is `404`, not the
  thumbnail.
- `GET /videos/{id}/audio.wav` — the clip's soundtrack, for one that has one.
- `GET /videos/{id}/clip.avi` — the clip as one video file, as an attachment: sd-cli's
  own AVI for a clip with sound, otherwise its frames encoded on first request and kept
  in the clip's folder.
- `POST /videos/enlarge` — `{ "id": "…", "upscale": 2.0, "sharpness": 0.0, "clarity": 0.0 }`.
  FSR over every frame of the original into a new version inside the clip
  (`{id}/v{n}/`, served at `/videos/{id}/v{n}/frames/{k}.png`). Returns
  `{ "clip": {…}, "version": 1, "ms": 1820 }`. Filters only — it never re-diffuses,
  for the reason under "Video" above.
- `POST /videos/extend` — `{ "id": "…", "keep": 2, "shots": [{ "prompt": "…", "seed": -1,
  "frames": 33 }], "every": "…", "negative": "…" }` changes the clip in place: its first
  `keep` shots are kept (at least 1 — changing shot 1 is a new clip), the rest cut, then
  `shots` are drawn after them, in `/video`'s shape, on the model and settings the clip
  recorded. `keep` = the clip's shot count adds shots (*Extend clip*), a smaller one
  redraws from shot `keep + 1` (*Redo from shot k*), and a smaller one with no `shots`
  cuts (the × on the last shot). Every row is checked before a frame is cut. Queued;
  answers like `/video`, `note` saying what a Stop or a failure kept. Enlarged versions
  are removed, since they no longer cover the clip.
- `POST /videos/save` — `{ "id": "…", "version": 0 }`. The version into `work/output/`,
  as `{id}[-v{n}].avi` (or a frames folder with `video_save = "frames"`). Answers
  `{ "saved": "…" }`.
- `POST /videos/delete` — `{ "ids": ["…"] }`. Removes each clip's whole directory from
  `work/videos/`; a starred clip is out of its reach.

### The agent

The Agent tab's chats (`work/chats/{id}/`, starred ones in `user/favorite_chats/`). Every
route but `/agent/send` (and a `/chats/film` edit that leaves a picture to draw) reads or
writes files and answers at once; that one queues a *leg* on the job queue (kind `agent`) and answers `{ "job": id }` — the reply arrives in
the chat, whose `rev` moves on every write.

- `GET /agents` — `{ models: [{name, label, arch, vision, bytes, fits, n_ctx, note}],
  driver, vram, dir, default, knobs: [{key, label, help}], actions: [{key, label, help}],
  max_batch, max_rounds }`: the agent models in the agents dir, paired by folder, whether
  this build has the driver, the agent a new chat starts with, and the table Settings →
  Agent draws its whitelist from (`[agent]` in `user/settings.toml`).
- `GET /chats` / `GET /favorite-chats` — `[{id, title, updated_ms, created_ms, favorite,
  state, thumb}]`, newest change first; Current lists starred ones too.
- `POST /chats/new` — `{ template, agent?, video? }`: a chat on that model, whose rounds
  start from its saved defaults (and its catalog Turbo LoRA when installed). `agent`
  absent is Settings' default; `video` is the Video tab's model, which a film is shot on.
  Answers `{ chat, toolkit }`.
- `GET /chats/{id}` — `{ chat, toolkit }`: the chat as stored, and the knob table
  resolved for it under Settings → Agent — each knob's `allowed`, limits, `locked` reason
  and value, the passes, the models it could pick, `locks`, the film's `video` model
  `{name, family, label, chains, references, width, height, frames, fps}` and
  `film_off`, why no film can be made in it (`null` when one can).
- `GET /chats/{id}/live` — the reply being written while a turn runs: `{ seq, text,
  phase, step, steps }`, or `{ seq: null }`.
- `GET /chats/{id}/thumb.png`, `GET /chats/{id}/img/{name}.png` — the chat's own copies
  of its pictures; names are `[a-z0-9-]+.png`.
- `POST /chats/agent` — `{ id, agent }`; `POST /chats/rename` — `{ id, title }`.
- `POST /agent/send` — `{ chat, text, images: [{kind: "item"|"output", id}], video? }`: a
  chat turn, whose reply may talk, draw, describe an attached picture as a prompt, start a
  session (a plan turn follows in the same job), or write a film's storyboard (a story
  turn follows in the same job; its pictures wait for `/chats/film {draw: true}`). `video` is the Video tab's model now. At most four pictures, copied into the
  chat. `409` while the agent is working on the chat.
- `POST /chats/film` — `{ id, every?, member?, name?, look?, picture?, shot?, text?,
  cast?, cut?, lines?, sound?, music?, voice?, redraw_keyframe?, no_keyframe?, draw?, image_model?, video_model?, go? }`: an edit of the storyboard before it is filmed — the look every shot
  shares, cast member `member`'s name, look or picture (`{kind: "output", id}`, or `null`
  to have it drawn again), shot `shot`'s words, cast (names) or cut. A reworded shot's
  keyframe is dropped, as is one sent with `redraw_keyframe: true` (a click on it in the
  Story panel); `no_keyframe: true` starts shot `shot` from its words with none drawn
  (`false` draws one again); on H3, `lines` (`[{who, words, language?, voiceover?}]`, `who` a
  cast name or `narrator`), `sound` and `music` are what shot `shot` says, sounds like and
  is scored with (empty sound is silence, empty music none), and `voice` cast member
  `member`'s voice (a sound output's stem, or `null` for none) — sent with each shot they
  speak in that starts fresh from the cast's pictures, as H3's voice-timbre reference;
  `image_model` is the installed model the pictures still missing are
  drawn with (one that takes references gives characters a turnaround sheet). `video_model` is the installed video model it is shot on (the Video tab's when the board was written); on one whose cuts start from keyframes, the switch may leave keyframes to draw. `draw: true` (the Story panel's *Draw the cast*, then *Draw the first frames*) queues
  a leg that draws what the cast is missing or, with the cast whole, the first frames
  missing or drawn with a cast picture since changed; `go: true` (*Film*) one that films it — shot 1 a new
  clip in the Video tab's Current, each later shot an extend of it, each looked at by the
  agent when it can see and filmed once more if a check fails — or goes on after a stop. `409` while the
  agent is working on the chat, or once the film is being shot.
- `POST /agent/undo` — `{ chat, change }`: put an applied change's settings back, if they
  still hold what it set (else `409`). Recorded as a change of yours.
- `POST /agent/stop` — `{ chat }`: end the chat's leg, killing a reply or a render in flight.
- `POST /chats/delete` / `POST /favorite-chats/delete` — `{ ids }`, one shelf each.
  `/favorite-chats/add` and `/favorite-chats/remove` move chats between the two. A chat
  the agent is working on is refused (`409`).

The archive listing, the thumbnail and the delete never touch the GPU, so they answer
immediately even while a render is in flight. `/progress` and `/cancel` do the same for
a sharper reason: they are *about* the render in flight, so answering them through the
job queue would mean progress that arrives only once the render has finished, and a Stop
that becomes possible only when there is nothing left to stop. The whole video family
splits the same way: `/video`, `/videos/extend` and `/videos/enlarge` are GPU work and queue, and
everything else about a clip is filesystem-only and doesn't — which matters most here,
since a clip holds the worker for minutes and the frame player is fetching 33 PNGs.

Errors are `{"error": "…"}`. Anything addressed by `image_id` fails with `400` — the id
has expired (only the newest image per place it is shown stays warm, in
`work/current/`, which is cleared on restart), or that pass isn't available for the
image; either way it's client-side staleness, and the image is left intact to retry.
Since every generation is archived, an expired id is recovered by reopening the work
item rather than by regenerating — the page does that by itself for an image with no
escalation in it. Malformed JSON is `400`, and `503` means the GPU worker is gone.
`POST /adjust` answers `409` when a pass on the same image finished while the re-filter
ran: the image is still there, just newer, and the page drops the stale answer.

Every route refuses with `403` a request from another website: a non-GET whose `Origin`
is present and isn't `http://` plus its own `Host`, or any request whose `Host` isn't a
loopback name or `startup.toml`'s host (not checked when bound to `0.0.0.0`). Clients
that send no `Origin` — curl, scripts — are unaffected. A request line or header line
over 8 KB, more than 100 headers, or a request not fully sent within 30 seconds is
dropped; a body over its route's limit answers `413`.
