# Architecture notes

How the pieces fit, for anyone working on the code. `CLAUDE.md` holds the detailed invariants.


- **Single GPU worker thread.** Both sd.cpp and the wgpu FSR passes use the GPU, so all
  jobs are serialized through one worker (an mpsc queue) to avoid VRAM thrash. HTTP
  connection threads enqueue a job and wait for the reply.
- **Shell-out, not FFI.** The server runs `sd-cli` as a subprocess per request — clean
  isolation, easy debugging, and it keeps everything Python-free.
- **The agent is a second subprocess, run the same way.** `yasdig-llm` (`llm/`, built
  against llama.cpp) takes a request file — the conversation, the pictures as paths in
  `work/current/`, the reply's JSON schema — answers once and exits, in the same sandbox
  as sd-cli. llama.cpp's own CLIs don't fit: `llama-mtmd-cli` takes no conversation, and
  `llama-cli` talks to an in-process server over a loopback socket the sandbox refuses.
  Nothing stays loaded between turns, so the GPU is the image model's whenever the agent
  isn't writing; a chat's work is one queue entry handed to the worker a step at a time.
- **sd-cli has no network** (Linux and Windows). It parses downloaded model files in
  C++, so a crafted file exploiting a loader bug is the one way a model could act.
  On Linux a seccomp filter installed between fork and exec (`src/sandbox.rs`)
  refuses it every internet socket, for itself and anything it starts. On Windows it
  runs in an AppContainer with no capabilities — no internet, LAN or loopback — and
  can open only the folders yasdig grants it (`work/`, the model folders, sd-cli's
  own); yasdig re-runs itself as a small launcher (`yasdig sandbox-run`) to start it
  there, and the first run checks with `curl.exe` that a connection from inside is
  refused. `[sandbox] enabled = false` in `startup.toml` (or `YASDIG_NO_SANDBOX=1`)
  turns it off; `doctor` says whether it's on.
- **Layout:**
  ```
  src/main.rs      server + routing + single GPU worker + CLI subcommands
  src/http.rs      minimal std-only HTTP/1.1
  src/config.rs    models.toml -> typed profiles (templates + discovery), image families
  src/discover.rs  identify model files by their tensor headers, image and video
  src/convert.rs   requantize a model file (-M convert) — types, size estimate, slots
  src/foldscale.rs fold a scaled-fp8 GGUF's per-tensor scales into its weights
  src/preflight.rs startup environment checks, shared with `doctor`
  src/sd.rs        sd-cli subprocess wrapper (txt2img + img2img + vid_gen)
  src/llm.rs       the agent's llama.cpp driver as a subprocess: one turn, framed stdout
  src/agents.rs    the agent's models: GGUF headers read, model and projector paired by folder
  src/agent/       the Agent tab: chats (chat.rs), the knob table and permissions (knobs.rs,
                   toolkit.rs), the reply grammar (schema.rs), what a turn is shown
                   (context.rs), the bounded workflow (session.rs), skills, routes
  llm/             yasdig-llm.cpp: one chat turn over llama.cpp's libraries, built by build.rs
  src/sandbox.rs   keeps sd-cli off the network: seccomp (Linux), AppContainer (Windows)
  src/settings.rs  user/settings.toml and user/model-settings.toml
  src/pipeline.rs  orchestrate sd -> fsr -> escalations, with timings + result cache
  src/workitems.rs durable storage: work/workitems/ + work/output/
  src/edits.rs     a work item's kept Refine & adjust edits (<id>.edits/)
  src/videos.rs    durable storage for clips: work/videos/
  src/fsr/         wgpu compute (upscale.wgsl = EASU, grade.wgsl = colour,
                   contrast.wgsl = clarity, sharpen.wgsl = RCAS)
  web/index.html   frontend: the markup, and one <script type="module">
  web/style.css    the design tokens and every rule built on them
  web/js/          the frontend, one ES module per area — app.js is the entry
  src/web.rs       the whitelist of files the page is made of, and the route
  models.toml      config + model profiles + the [[video]] overrides
  models/video/    video components, scanned recursively and identified by header
  user/settings.toml  the defaults every generation starts from
  user/model-settings.toml  what individual models do differently
  user/favorite/   work items you kept — nothing here sweeps user/
  user/favorite_outputs/  saved images you kept
  src/shutdown.rs  what Close server clears on the way out
  work/            scratch, cleared at startup
  work/workitems/  every generation + its settings — kept; <id>.edits/ holds an
                   edited one's History steps and look (src/edits.rs)
  work/output/     what you pressed "Save to output" on — kept
  work/videos/     one directory per clip: its frames, a thumbnail, its settings
  ```
- **The archive is copy-in, copy-out.** Nothing in `work/workitems/` is ever opened in
  place: a generation is copied in, and reopening one copies it back out to a scratch
  name. An escalation deletes the files of the step it supersedes, so an archive entry
  used directly would be destroyed by the first refine run on it.
- **`work/videos/` is the exception, and for a reason that doesn't undermine the rule.**
  A clip is written straight into the archive because it has no scratch stage: one pass
  writes it and nothing afterwards does anything but read it, so there is no escalation
  to delete it out from under itself — and copying 23 MB of frames would buy nothing.
  What it does need is the *failure* half, and that is a guard which sweeps a
  half-written directory unless the pass reaches the end. Cancelling mid-clip is the
  common path here, not the rare one.
