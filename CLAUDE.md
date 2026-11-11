# CLAUDE.md

**yasdig** — *Yet Another Stable Diffusion Image Generator* (rustimage until
2026-10-12). Python-free Vulkan image generator for AMD/Linux. Generate small and cheap
with stable-diffusion.cpp (a subprocess, never FFI), then escalate on demand with
img2img passes and a native FSR post-process in `wgpu`. All sd.cpp work goes through
one job queue and one diffusion worker; the viewer's re-filters run on a display worker
sharing the one FSR device behind a lock. The Assistant panels run a second subprocess
the same way — llama.cpp, through our own driver.

**`README.md` is short and for users** (why escalation, install, what's supported —
and video). **`docs/` is the reference**: `configuration.md`
(folders, model files, `models.toml`/`user/settings.toml` field tables), `guide.md`
(features), `api.md` (CLI and HTTP routes), `building.md`, `architecture.md`.

**The traps live in `.claude/rules/`, one file per area** (*## Where the traps are*).
This file holds only what every session needs: commands, the invariants, the
cross-cutting conventions, and the index.

## Commands

```sh
cargo test                                  # no GPU: naga-validates the WGSL shaders, band maths, DTOs, `fit_to`
cargo run --release                         # server on startup.toml's port (127.0.0.1:8080)
cargo run --release -- doctor               # preflight: binary, checkpoints, web UI
cargo run --release -- fsr-selftest         # full wgpu path, no sd.cpp or checkpoint
cargo run --release -- fsr in.png out.png [scale] [sharpness] [clarity]
cargo run --release -- upscale in.png out.png <model> [factor]  # model enlarge + consistency dB
cargo run --release -- sr-selftest          # `src/sr/` shader ops vs CPU references, matmul TFLOPS
cargo run --release -- agent-check [agent]  # nine scored agent turns (`agent/check.rs`): run before and after a skill edit
cargo run --release -- fold-scales f.gguf   # fold a scaled-fp8 GGUF's scales into its weights, in place
node --test 'web/js/*.test.js'              # prompt-weight maths, OOM box choices, module graph evaluates.
                                            #   Node's built-in runner; no npm/package.json; runtime needs no Node.
```

Shaders otherwise compile only at GPU device creation, so `cargo test` is what turns a
WGSL typo into a test failure instead of a worker-thread startup failure.

**Run `fsr-selftest` after changing a filter constant.** It asserts dimensions, that
scale 1.0 with all filters at 0 is byte-exact, checks each grade knob (`check_grade`),
and prints per-band movement of planted gratings at 1536² and 3072² (invariant 6).
Band rows are printed, not asserted, because `rcas_radius` is a step function; turning
them into an assertion is the next move. The DFT's own correctness is in `cargo test`.

## Where the traps are

Each `.claude/rules/*.md` loads by itself when a file its `paths:` names is read or
edited. **A grep doesn't load one, and the hub files (`main.rs`, `pipeline.rs`,
`config.rs`) load none** — so before working in an area, Read its file.
A new trap goes in the area's file, not here; a new source file goes in its `paths:`.

| before touching | read `.claude/rules/…` | loads with |
|-----------------|------------------------|------------|
| a release, `build.rs`, the bundle, the home folder, port takeover, CI, the VirusTotal scan, `site/`, licences | `packaging.md` | `build.rs`, `src/{bundle,home,startup}.rs`, `.github/`, `site/` |
| **bumping sd.cpp** (`SD_CPP_REF`, patches, sampler tables, log parsers) | `packaging.md` + `sd-cli.md` | — |
| **bumping llama.cpp** (`LLAMA_CPP_REF`, the driver's API list) | `packaging.md` + `agent.md` | — |
| the four FSR passes, filter constants | `fsr.md` | `src/fsr/` |
| **adding a family**, `classify`, part slots, encoders, bundled vs assembled | `families.md` | `src/discover.rs`, `parts.js` |
| the Image tab's panels, the dock, Image to image, MOSS layout boxes | `image-tab.md` | `image.js`, `panels.js`, `source-slot.js`, `layout.*` |
| LoRAs (image and video), Turbo | `loras.md` | `loras.js`, `src/discover.rs` |
| upscalers, `src/sr/` networks | `upscalers.md` | `src/sr/` |
| the cast library, cast picks, cast in clips/films, the agent's `cast` action | `cast.md` | `src/cast.rs`, `cast*.js` |
| favourites, the Outputs tab, work-item shelves, PNG text chunks | `gallery.md` | `workitems.*`, `pngmeta.rs`, `gridmark.js`, `outputs.js` |
| the catalog, downloads, Installed, model notes, the Models tab | `downloads.md` | `catalog.*`, `downloads.*`, `inventory.rs`, `notes.*`, `getmodels.js` |
| memory modes and OOM, live preview, step caching, samplers/schedulers, the Help tab | `sd-cli.md` | `src/sd.rs`, `memory.js`, `help.js` |
| the job queue, Stop/skip, loops, Close server | `jobs.md` | `jobs.*`, `shutdown.rs`, `busy.js`, `alive.js` |
| the Assistant: driver, chats, sessions, knobs, skills, Enhance, films and the board | `agent.md` | `src/agent/`, `llm.rs`, `agents.rs`, `llm/`, `assistant.js`, `board.js` |
| Refine & adjust: Redraw, Extend, Edit, Scale, History, References, sliders, area, brushes, text, panes, tooltips | `viewer.md` | `viewer.js`, `redraw.js`, `refs.js`, `redraw.rs`, `area.rs`, `brush.rs`, `edits.rs` |
| any page work: stylesheet tokens, the module table and its rules, `<prompt-box>` | `web.md` | anything under `web/`, `src/web.rs` |
| settings layers, per-model settings, prompt variables, **adding a per-model or video knob** | `settings.md` | `settings.rs`, `variables.rs`, `settings.js`, `modelsettings.js` |
| the sd-cli sandbox (seccomp, Landlock, AppContainer) | `sandbox.md` | `src/sandbox.rs` |
| video: discovery, LTX/H3/Wan, shots, clips, plans, drafts, sounds, narrated stills, export | `video.md` | `videos.rs`, `narration.rs`, `voices.rs`, `avi.rs`, `h3prompt.rs`, `video.js`, `shots.js`, … |
| Convert, scaled-fp8 folding | `convert.md` | `convert.rs`, `foldscale.rs`, `half.rs`, `convert.js` |

## Invariants

1. **Nothing diffuses over sharpened or graded pixels.** `Pending.raw` is the
   unfiltered image; every escalation reads it; display passes run once, in
   `pipeline::publish()`. This makes `Display::adjust` lossless and lets the grade
   **carry across an escalation** (`Filters::restart`) while sharpness/clarity reset
   (they're sized from the image). That asymmetry is deliberate. **One exception,
   asked for**: the viewer's *Apply adjustments* (`Display::apply_look`) copies
   `display` to a new `raw` step and zeroes the look and its start points; passes
   after it work on the adjusted picture, and History holds the untouched step.
2. **sd.cpp trims an img2img schedule by strength**, so `SdRunner::img2img` passes
   `steps / strength` (else `--steps 2 --strength 0.3` changes nothing).
3. **`MAX_STRENGTH` is 0.95** (`src/config.rs`): at 1.0 sd.cpp discards the init image.
4. **`*_steps` is a floor**, raised to `profile.steps × strength` (under-stepping
   leaves grit).
5. **Dimensions come back from `publish()`**; sd.cpp may round. `snap64()` likewise.
6. **Both display filters size their band from the image** (`clarity_sigmas`,
   `rcas_radius` ∝ longest side). `check_bands` asserts it for clarity. **RCAS holds it
   in steps** (whole-pixel radius: fine band 3.245× at 1536², 2.481× at 2048², 3.245×
   at 3072² — 1536 vs 3072 agree by arithmetic; compare within a plateau,
   768..2303 or 2304..3839). Fixing it means fractional taps, which costs the limiter
   guarantee (see `sharpen.wgsl`).
7. **`hires_max_side` gates hi-res but not refine** (`Pending::can_refine`) — not a bug.
   **The buttons are gated on the fold's steps, not the profile's**: a press sends
   `#hsteps`/`#rsteps`, which override the retained profile before the server checks,
   and a profile can say 0. So `GenResult.can_hires` is the size budget
   only (`Pending::hires_fits`) and the page reads steps with `passSteps`.
8. **Nothing a `Pending` owns lives in a durable directory.** `Engine::run` copies
   `raw` into `work/workitems/`; `Display::open_in` copies it back out to
   `{id}-0-raw.png`; sources, references, outputs and favourites are likewise copied
   in. Otherwise `Pending::remove_files` or `Scratch::drop` deletes the user's file.
   Scratch is `work/current/` (`pipeline::scratch_dir`), emptied on start and Close,
   holding one live image per slot (`Slot::Image`, `Slot::Output` in `Retained`). An
   escalated image exists only in scratch until saved, so jobs **pin** it
   (`Retained::pin`). `Pending.work_id` and `Pending.source` are ids/stems, never paths.
   **A work item's kept edits** (`{shelf}/{id}.edits/`, `src/edits.rs`) obey the same
   rule both ways: steps and masks are hard-linked (or copied) *out* of scratch, and
   `Display::open_in` copies them back *in* under fresh names.
9. **Filter a clip's frames; never re-diffuse them.** `Engine::enlarge_clip` runs
   `Fsr::process` per frame (deterministic, no flicker). Per-frame `refine` would boil
   in motion; refining a clip means a VACE `--control-video` pass. **The one
   exception is LTX's enlarge** (`enlarge_clip_ltx`, `ClipVersion.ltx`): a shot's
   frames as `--control-video` beside `--hires` are the first pass of one LTX run
   (our sd.cpp patch), so the refine sees every frame at once and can't boil. Per
   shot, on its `conditioned` words; frames padded *up* to 8k+1 (`ltx_frames`; the
   patch holds the last), sides to 32 (`ltx_side`), cut and Lanczos'd back to 2×
   after; a distilled model refines from the strength down its own grid
   (`ltx_refine_sigmas`). The clip's sound stays; LTX's is dropped. `work/videos/` is
   written through (no scratch stage); `videos::ClipDir` sweeps a half-written dir
   unless `keep()` is reached.

## Conventions

- Comments explain *why*, usually with a measured number. Match that density; measure
  before claiming.
- `anyhow` throughout; errors are user-facing with the fix in them (`SdRunner::preflight`,
  `Engine::lookup`).
- std-only HTTP (`src/http.rs`), no framework.
  - **`http::allowed` is the only guard against other websites** (no login): a non-GET
    with a foreign `Origin`, or a non-loopback `Host` (DNS rebinding), is a 403.
    Absent `Origin` passes — the takeover and every older build send none, and send
    `Host: localhost` with no port, so `Host` is compared by name only. A destructive
    default must stay off: `/shutdown` clears only on an explicit `"clear": true`.
    **Bound to every interface, `Host` must be a name public DNS can't hold** (an IP
    literal, one label, `.local`) or one of `startup.toml`'s `[server] names`
    (`http::host_allowed`); the 403 says which name to add.
  - **The page carries a CSP** (`web::HEADERS`: `script-src 'self'`), the net under
    the hand-written `esc`s. So **no inline script, `on*=` attribute or `javascript:`
    URL anywhere**, in `index.html` or in a string a module builds — it would silently
    not run (`the_pages_headers_are_whole_lines_and_the_page_fits_its_policy` covers
    the markup only). Styles keep `'unsafe-inline'`. Every answer is `nosniff` and
    `Cross-Origin-Resource-Policy: same-origin`.
  - **The read timeout covers the request only** (`set_read_timeout(None)` once the
    body is in); a minutes-long render waits on a channel, not the socket. The 413
    drains the body, or the reset swallows it.
  - **A handler panic answers 500** (`catch_unwind` in `handle_conn`); model-file
    parsers still must not panic — `identify` memoizes only successes, so a panicking
    file breaks every `/models`.
  - **The query string is stripped before routing**, so ids are path segments or POST
    body fields. `workitems::valid_id` is a whitelist (alphanumerics and `-`) — widen
    it, don't replace it with `canonicalize`.
  - `Job.reply` carries the finished JSON body (`result_json` called in the worker).
  - Filesystem-only routes (lists, thumbnails, deletes, `/video-models`, `/videos…`,
    frames, `/outputs…`) and `/progress`, `/cancel` answer on the connection thread —
    never behind a render.
  - `/outputs/{stem}/thumb.png` generates on first request (`ensure_output_thumb`),
    via a counter-suffixed temp name and rename.
- **Every sd-cli spawn is `SdRunner::run_tail`'s, which confines it**
  (`sandbox::confine`). **A path sd-cli is handed, or a place a child writes, must be
  under a `Config::sandbox_roots` root** (`work/`, the model folders, sd-cli's folder)
  on both OSes; `curl` for downloads is deliberately outside. The rest is
  `.claude/rules/sandbox.md`.
- **`SdRunner::run` streams; never go back to `Command::output()`.**
  - Progress is on **stdout**; stderr gets its own draining thread (deadlock otherwise).
  - `drain` splits on `\r` **and** `\n`.
  - `Monitor` parks the `Child` in a `Mutex`; `run` takes it out *before* `wait`.
  - `rate` is sd.cpp's text verbatim; `Phase` derives from its unit suffix.
- JSON out: hand-rolled `format!` for `result_json` and `build_models_json` (don't
  churn), `serde_json` for anything struct-backed.
- **Batch/loop are a queued job** (`.claude/rules/jobs.md`). `#stop` is not in
  `IDLE_LABELS`; the Jobs poll drives it.
- **Video and Convert have their own files** (`.claude/rules/video.md`, `convert.md`).
- No Python anywhere in build or runtime.
