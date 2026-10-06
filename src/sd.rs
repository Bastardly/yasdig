//! Thin wrapper around the stable-diffusion.cpp CLI (`sd-cli`). We shell out to
//! the native binary per request — clean process isolation, easy to debug, and
//! it keeps this crate Python-free. All flags are derived from a resolved
//! [`Profile`], with `weight_type`/`extra_args` escape hatches so CLI changes
//! need only a config edit.
//!
//! The child's output is streamed rather than collected at the end, because a
//! pass can run for minutes and it holds the single GPU worker for all of them.
//! [`Monitor`] is the shared handle that makes that visible — one connection
//! thread reads progress off it, another can kill the run through it.

use crate::config::{Components, Config, Profile, VideoModel, MIN_STRENGTH};
use anyhow::{bail, Context, Result};
use std::collections::VecDeque;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Marker in the error a cancelled run returns, so the HTTP layer can tell a
/// Stop from a crash without a second channel for it.
pub const CANCELLED: &str = "cancelled by request";

pub struct SdRunner<'a> {
    cfg: &'a Config,
    /// Where progress goes and where a cancel comes from. Defaults to a
    /// detached one, so the `SdRunner` built for `preflight` needs no plumbing.
    monitor: Monitor,
}

impl<'a> SdRunner<'a> {
    pub fn new(cfg: &'a Config) -> Self {
        SdRunner {
            cfg,
            monitor: Monitor::default(),
        }
    }

    /// The runner the GPU worker uses: the same thing, reporting to a monitor
    /// the connection threads can read.
    pub fn watched(cfg: &'a Config, monitor: &Monitor) -> Self {
        SdRunner {
            cfg,
            monitor: monitor.clone(),
        }
    }

    fn binary(&self) -> PathBuf {
        self.cfg.sd_binary()
    }

    /// Verify the sd binary exists; returns a friendly error if not.
    pub fn preflight(&self) -> Result<()> {
        let bin = self.binary();
        if !bin.exists() {
            bail!(
                "sd binary not found at {} — `rustimage doctor` says why and what to do",
                bin.display()
            );
        }
        Ok(())
    }

    /// text-to-image. Writes a PNG to `out`.
    pub fn txt2img(
        &self,
        p: &Profile,
        prompt: &str,
        negative: Option<&str>,
        seed: i64,
        out: &Path,
    ) -> Result<()> {
        let mut cmd = self.base_cmd(p, prompt, negative, seed, out, p.cfg_scale);
        cmd.args(["-W", &p.width.to_string()]);
        cmd.args(["-H", &p.height.to_string()]);
        cmd.args(["--steps", &p.steps.to_string()]);
        self.run(cmd, "txt2img")
    }

    /// Low-denoise image-to-image. Reads `init`, writes `out`. Drives both the
    /// base-resolution refine and the hi-res pass, which differ only in their
    /// strength/steps and in needing the large-image memory flags.
    ///
    /// `steps` is how many denoising steps the pass should actually run. That is
    /// deliberately not the number handed to `--steps` — see below. It is also a
    /// floor, not an exact count: a pass that starts high up the sigma schedule
    /// needs proportionally more steps to resolve, so `strength` can raise it.
    ///
    /// `cfg` is the guidance this pass runs at, and it is the caller's rather
    /// than read off the profile because the three passes through here resolve it
    /// differently — [`crate::config::Profile::refine_cfg`], `hires_cfg`, and
    /// plain `cfg_scale` for a pass over a *Start from* image. The base render's CFG over-guides at
    /// output resolution whichever pass it is: a low-strength pass has little
    /// freedom to begin with, and pushing it hard against the prompt spends that
    /// freedom on high-frequency artifacts — the same crunch the step floor
    /// attacks from the other side.
    ///
    /// `big_image` adds `--vae-tiling` and (unless the profile disables it)
    /// `--diffusion-fa`. Both matter above ~1536²: measured on a 7900 XTX, a
    /// 2048² pass takes 122 s without flash attention and 20 s with it.
    pub fn img2img(
        &self,
        p: &Profile,
        prompt: &str,
        negative: Option<&str>,
        seed: i64,
        init: &Path,
        out: &Path,
        strength: f32,
        steps: u32,
        cfg: f32,
        big_image: bool,
        label: &str,
    ) -> Result<()> {
        let mut cmd = self.base_cmd(p, prompt, negative, seed, out, cfg);
        // sd.cpp derives dimensions from the init image, so no -W/-H here.
        cmd.args(["-i", &path_str(init)]);
        cmd.args(["--strength", &format!("{strength}")]);
        // Effective steps have to scale with strength, or the pass is simply
        // under-resolved. `strength` decides how far up the sigma schedule the
        // pass starts, so the range those steps must cover grows with it, while
        // the caller's count does not. At 0.9 that means a near-full render in a
        // handful of steps — under-stepped Euler hasn't converged, and what it
        // leaves behind is un-removed noise, which reads as grit. Floor the
        // count at the base render's own step density over the same range.
        let requested_steps = steps;
        let floor = ((p.steps as f32) * strength).ceil() as u32;
        let steps = steps.max(floor).max(1);
        // sd.cpp *trims* an img2img schedule by strength instead of scaling it:
        // it keeps the last `t_enc = steps * strength` steps of a `steps`-long
        // schedule. So `--steps 2 --strength 0.3` gives t_enc = 0 and starts at
        // a sigma near zero — the image comes back untouched. Scale the schedule
        // up by 1/strength so the trim lands on the steps the caller asked for;
        // this is the same compensation sd.cpp applies to its own hires-fix
        // (pipeline/request.cpp, "sd-webui behavior"). The trim keeps one extra
        // sigma, so the pass runs steps + 1 — near enough not to correct for.
        let sched = ((steps as f32) / strength.max(MIN_STRENGTH)).ceil() as u32;
        cmd.args(["--steps", &sched.max(1).to_string()]);
        eprintln!(
            "[rustimage] {label}: {steps} effective steps at strength {strength} \
             (schedule {sched}{}){}",
            // Only when it isn't the obvious thing — same rule as the step note
            // below. A silent line means the pass ran at the base CFG.
            if cfg != p.cfg_scale {
                format!(", cfg {cfg} instead of {}", p.cfg_scale)
            } else {
                String::new()
            },
            if floor > requested_steps {
                format!(" — raised from {requested_steps}, floor is {floor} at this strength")
            } else {
                String::new()
            }
        );
        if big_image {
            cmd.arg("--vae-tiling");
            // Not twice: a DiT family already runs with it at every size.
            if p.hires_flash_attn && !p.family.flags().contains(&"--diffusion-fa") {
                cmd.arg("--diffusion-fa");
            }
        }
        self.run(cmd, label)
    }

    /// Video generation, which sd.cpp calls `vid_gen`.
    ///
    /// Writes a **PNG sequence** to `out_pattern`, which must carry a `%0Nd`
    /// specifier — sd-cli's output path is what selects the form, and a pattern
    /// takes an early branch that writes one file per frame (`main.cpp`,
    /// `format_specifier_regex`). Without a specifier it muxes a single
    /// container instead, and the two are exclusive: there is one `-o`.
    ///
    /// Frames are the right choice here even though a `.webm` would play in the
    /// page with no help from us, and it comes down to what happens *next*.
    /// Enlarging a clip with FSR and refining one with a VACE pass both consume
    /// frames — `--control-video` takes a directory of them — and this binary
    /// has no video decoder to get them back out of a container (`image` is
    /// compiled `features = ["png"]`, and sd-cli's muxer is internal to sd-cli
    /// with no mode that runs it over existing images). Ask for the container
    /// and both escalations would need the clip generated a second time, at
    /// minutes a go. Frames are also lossless, which is the same argument
    /// invariant 1 makes for `Pending.raw`: what gets filtered should not
    /// already be compression artifacts. The price is disk — ~700 KB a frame at
    /// 832×480, so ~23 MB for a 33-frame clip.
    ///
    /// What the pass reads and writes travels in [`VideoIo`].
    pub fn video(
        &self,
        m: &VideoModel,
        prompt: &str,
        negative: &str,
        seed: i64,
        io: &VideoIo,
    ) -> Result<()> {
        let cmd = self.video_cmd(m, prompt, negative, seed, io);
        let label = if io.init.is_some() { "i2v" } else { "t2v" };
        self.run(cmd, label)
    }

    /// The argv for [`SdRunner::video`], built separately so it can be asserted
    /// on without a GPU. This is the function a new `family` extends, and the
    /// flags it emits are the only thing that distinguishes one from another.
    fn video_cmd(
        &self,
        m: &VideoModel,
        prompt: &str,
        negative: &str,
        seed: i64,
        io: &VideoIo,
    ) -> Command {
        let mut cmd = Command::new(self.binary());
        // Not `cfg.sd.mode`: that one is the image mode, and it is configurable
        // because old builds spelled it `txt2img`. `vid_gen` has only ever had
        // one spelling.
        cmd.args(["-M", "vid_gen"]);
        cmd.args(["--diffusion-model", &path_str(&m.diffusion_model)]);
        if let Some(high) = &m.high_noise_diffusion_model {
            cmd.args(["--high-noise-diffusion-model", &path_str(high)]);
        }
        cmd.args(["--vae", &path_str(&m.vae)]);
        for (flag, path) in [
            ("--t5xxl", &m.t5xxl),
            ("--llm", &m.llm),
            ("--audio-vae", &m.audio_vae),
            ("--clip_vision", &m.clip_vision),
        ] {
            if let Some(p) = path {
                cmd.args([flag, &path_str(p)]);
            }
        }

        // LoRAs ride inline in the prompt, which is sd.cpp's own spelling for
        // them; `|high_noise|` picks the expert. Config holds the names so this
        // stays a property of the model rather than something typed into the
        // prompt box and lost on the next generate.
        let mut p = prompt.to_string();
        for lora in &m.loras {
            p.push_str(&format!("<lora:{lora}>"));
        }
        cmd.args(["-p", &p]);
        if !negative.is_empty() {
            cmd.args(["-n", negative]);
        }
        if let Some(dir) = &m.lora_dir {
            cmd.args(["--lora-model-dir", &path_str(dir)]);
        }

        // Rounded here as well as in the UI, because a request can arrive from
        // anywhere. sd.cpp would align these itself and warn, but then the
        // sidecar would record a size the clip doesn't have.
        let (w, h, frames) = m.aligned();
        cmd.args(["-W", &w.to_string()]);
        cmd.args(["-H", &h.to_string()]);
        cmd.args(["--video-frames", &frames.to_string()]);
        cmd.args(["--fps", &m.effective_fps().to_string()]);
        cmd.args(["--steps", &m.steps.to_string()]);
        cmd.args(["--cfg-scale", &format!("{}", m.cfg_scale)]);
        cmd.args(["--sampling-method", &m.sampler]);
        if let Some(shift) = m.flow_shift {
            cmd.args(["--flow-shift", &format!("{shift}")]);
        }

        // The second expert's half of the schedule. Only for the MoE pair —
        // emitting these for a dense checkpoint would be asking sd.cpp to split
        // a model that has nothing to split.
        if m.family.is_moe() {
            if let Some(steps) = m.high_noise_steps {
                cmd.args(["--high-noise-steps", &steps.to_string()]);
            } else if let Some(b) = m.moe_boundary {
                // Only honoured while `--high-noise-steps` is -1, which is
                // exactly this branch (sd.cpp's own flag help says so).
                cmd.args(["--moe-boundary", &format!("{b}")]);
            }
            cmd.args(["--high-noise-cfg-scale", &format!("{}", m.high_noise_cfg())]);
            cmd.args(["--high-noise-sampling-method", &m.sampler]);
        }
        if let Some(s) = m.vace_strength {
            cmd.args(["--vace-strength", &format!("{s}")]);
        }

        cmd.args(["-s", &seed.to_string()]);
        cmd.args(["-o", &path_str(io.out)]);
        if let Some(i) = io.init {
            cmd.args(["-i", &path_str(i)]);
        }
        if let Some(c) = io.control {
            cmd.args(["--control-video", &path_str(c)]);
        }
        if m.offload_to_cpu {
            cmd.arg("--offload-to-cpu");
        }
        if m.vae_tiling {
            cmd.arg("--vae-tiling");
        }
        if m.flash_attn {
            cmd.arg("--diffusion-fa");
        }
        if let Some(wt) = &m.weight_type {
            cmd.args(["--type", wt]);
        }
        for a in &m.extra_args {
            cmd.arg(a);
        }
        cmd
    }

    /// Requantize a model file (`-M convert`), writing a GGUF beside it.
    ///
    /// No GPU is involved — the work is dequantize-then-quantize on the CPU,
    /// streamed a tensor at a time (`src/convert.cpp:169`) so a 15 GB source
    /// never has to be resident. It still runs on the GPU worker, for two
    /// reasons that have nothing to do with the GPU: it saturates the cores and
    /// the disk for several minutes, which is exactly what must not happen
    /// underneath a render on a 30 GB box; and going through the worker is what
    /// gives it [`Monitor`], so `GET /progress` and `POST /cancel` work on it
    /// with no new plumbing. That second one is free rather than lucky —
    /// conversion reports through `pretty_bytes_progress`, the same
    /// `print_progress_line` the weight loader uses, so [`parse_progress`]
    /// already reads it and [`Phase`] already classifies it as loading.
    pub fn convert(&self, plan: &crate::convert::Plan) -> Result<()> {
        let cmd = self.convert_cmd(plan);
        self.run(cmd, "convert")
    }

    /// Enlarge `input` into `output` with an ESRGAN model, at whatever scale the
    /// model was trained for.
    ///
    /// `-M upscale` never builds a diffusion context (`examples/cli/main.cpp:915`
    /// hands the init image straight to the upscaler), so there is no checkpoint,
    /// no prompt and no seed. sd.cpp reads `.pth` natively and maps the old
    /// ESRGAN key layout (`src/name_conversion.cpp:1273`), but it only
    /// implements **RRDBNet**: every other architecture goes through
    /// [`crate::sr`] instead, and `pipeline::enlarge` decides which by reading
    /// the file, so only an ESRGAN should ever reach this.
    ///
    /// **A model it can't build is not a failure exit.** `new_upscaler_ctx`
    /// failing is a `LOG_ERROR`, after which sd-cli saves the *input unchanged*
    /// and exits 0 (`main.cpp:991`). So the stderr tail is checked here for the
    /// two lines that path prints, and the caller still checks the size of what
    /// came back — the structural half of the same guard, which also catches a
    /// 1× model that ran fine and enlarged nothing.
    pub fn upscale(&self, model: &Path, input: &Path, output: &Path) -> Result<()> {
        let mut cmd = Command::new(self.binary());
        cmd.args(["-M", "upscale"]);
        cmd.args(["-i", &path_str(input)]);
        cmd.args(["-o", &path_str(output)]);
        cmd.args(["--upscale-model", &path_str(model)]);
        cmd.args(["--upscale-tile-size", &UPSCALE_TILE.to_string()]);
        let err_tail = self.run_tail(cmd, "upscale")?;
        if err_tail
            .iter()
            .any(|l| l.contains("new_upscaler_ctx failed") || l.contains("upscale failed"))
        {
            bail!(
                "sd-cli could not run {} as an ESRGAN upscaler, though its tensor \
                 names say it is one — the file may be damaged.\n--- stderr ---\n{}",
                model.display(),
                joined(&err_tail)
            );
        }
        Ok(())
    }

    /// The argv for [`SdRunner::convert`].
    ///
    /// `plan.slot` is the load-bearing part and it is chosen by the server from
    /// the file's discovered role, never by the client: each component flag
    /// prefixes every tensor name it loads (`--diffusion-model` prepends
    /// `model.diffusion_model.`, `--vae` prepends `vae.`), so the wrong flag
    /// produces a valid file full of misnamed tensors rather than an error. See
    /// [`crate::convert`].
    fn convert_cmd(&self, plan: &crate::convert::Plan) -> Command {
        let mut cmd = Command::new(self.binary());
        cmd.args(["-M", "convert"]);
        cmd.args([plan.slot, &path_str(&plan.source)]);
        cmd.args(["--type", &plan.ty]);
        cmd.args(["-o", &path_str(&plan.out)]);
        // Single-threaded on purpose, and this is not a conservative default —
        // sd.cpp's streaming converter is not thread-safe. It runs `n_threads`
        // workers that each call `ModelLoader::load_tensor` on **one shared
        // loader** (`src/convert.cpp:244`), and that call re-enters
        // `load_tensors` → `process_model_files`, which mutates the loader's own
        // `file_data` with no lock held. Left at its default (physical cores,
        // 12 here) it aborts inside glibc — `double free or corruption (out)`
        // and `tcache_thread_shutdown(): unaligned tcache chunk` — on roughly
        // one run in three, reproduced at the CLI with these exact arguments.
        //
        // A crash would be survivable, since `Engine::convert` removes the
        // partial file and the run is retryable. The reason for -t 1 is that the
        // corruption is in the *loader's* bookkeeping while other threads are
        // reading through the mappings it is re-making, so the same race can put
        // wrong bytes in the output instead of aborting. A silently wrong 8 GB
        // model that loads and generates slightly worse video is the failure
        // this project can least afford to ship.
        //
        // The price is measured and small: 254 MB of bf16 VAE converted in
        // 2.07 s at -t 1 against 0.53 s at -t 4, so ~123 MB/s. Quantizing is
        // more work per byte than widening, but a 15 GB transformer is still
        // minutes, once, for a file that is then used for months. Revisit when
        // upstream takes a lock around that call.
        cmd.args(["-t", "1"]);
        cmd
    }

    /// Flags common to both image modes.
    ///
    /// `cfg_scale` is passed in rather than read off the profile, because the two
    /// modes want different ones — see [`Profile::refine_cfg`]. Passing the
    /// value keeps this function from having to know which mode it's serving.
    fn base_cmd(
        &self,
        p: &Profile,
        prompt: &str,
        negative: Option<&str>,
        seed: i64,
        out: &Path,
        cfg_scale: f32,
    ) -> Command {
        let mut cmd = Command::new(self.binary());
        cmd.args(["-M", &self.cfg.sd.mode]);
        // A bundled checkpoint is one `-m`. A bare DiT is `--diffusion-model`
        // plus a flag per separate file — the flag also decides the prefix
        // sd.cpp loads its tensors under, so a DiT through `-m` doesn't fail to
        // start, it fails to find its own weights.
        //
        // The parts go on either way: on a bundled checkpoint a `--vae` is an
        // override, which sd.cpp honours because it loads the VAE after the
        // checkpoint and a later tensor replaces one of the same name.
        let flag = if p.is_bundled() { "-m" } else { "--diffusion-model" };
        cmd.args([flag, &path_str(&p.model)]);
        for (flag, path) in p.components.iter().flat_map(Components::flags) {
            cmd.args([flag, &path_str(path)]);
        }
        cmd.args(p.family.flags());
        // LoRAs ride inline in the prompt, which is sd.cpp's own spelling for
        // them — the same shape `video_cmd` uses below. The tags are pulled back
        // out before the text is conditioned (`extract_and_remove_lora`,
        // `examples/common/common.cpp:2243`), so this does not reach the text
        // encoder as words. Only the positive prompt is scanned, which is why
        // `-n` below is left alone.
        let mut prompt = prompt.to_string();
        for l in &p.loras {
            prompt.push_str(&format!("<lora:{}:{}>", l.name, l.strength));
        }
        cmd.args(["-p", &prompt]);
        // Unconditional, not just when there are tags to resolve, and both
        // halves of that matter. An *empty* `--lora-model-dir` makes sd.cpp skip
        // tag extraction entirely (`common.cpp:2245`) — the `<lora:…>` text then
        // stays in the prompt and is conditioned as literal words — and its
        // default is `"."`, the process CWD. Sending it always means a
        // `<lora:name:1>` typed by hand into the prompt box resolves against the
        // same folder the picker offers, rather than against wherever the server
        // happens to have been started from.
        cmd.args(["--lora-model-dir", &path_str(&self.cfg.lora_dir())]);
        if let Some(neg) = negative {
            if !neg.is_empty() {
                cmd.args(["-n", neg]);
            }
        }
        cmd.args(["--cfg-scale", &format!("{cfg_scale}")]);
        cmd.args(["--sampling-method", &p.sampler]);
        // Auto is no flag at all: sd.cpp's `default_scheduler` then picks per
        // model (`flux`/`flux2` for those families, `lcm` for the lcm and tcd
        // samplers, `discrete` otherwise), which no single value here could.
        if p.scheduler != crate::config::AUTO_SCHEDULER {
            cmd.args(["--scheduler", &p.scheduler]);
        }
        cmd.args(["-s", &seed.to_string()]);
        cmd.args(["-o", &path_str(out)]);
        // The live preview: `proj` is a fixed latent-to-RGB matrix per VAE
        // (`StableDiffusionGGML::preview_image`), so it needs no file and covers
        // every family this build runs. Numbered rather than one overwritten
        // file, because sd-cli writes in place — see `newest_complete`.
        if let Some(dir) = self.monitor.preview_dir() {
            cmd.args(["--preview", "proj", "--preview-interval", "1"]);
            cmd.args(["--preview-path", &path_str(&dir.join("p-%04d.png"))]);
        }
        if let Some(wt) = &p.weight_type {
            cmd.args(["--type", wt]);
        }
        for a in &p.extra_args {
            cmd.arg(a);
        }
        cmd
    }

    /// Launch `sd-cli` and watch it run.
    ///
    /// Deliberately not `Command::output()`, which is what this was: that hands
    /// back nothing until the process exits. Fine for a 4-step SDXL at 8 s, and
    /// useless for a video clip that owns the single GPU worker for minutes
    /// while every other route waits behind it. Streaming the output is what
    /// gives `GET /progress` something to report and `POST /cancel` something to
    /// kill.
    ///
    /// Progress comes off **stdout**, not stderr: sd.cpp routes `SD_LOG_ERROR`
    /// to stderr and everything else — bars included — to stdout
    /// (`examples/common/log.cpp`, `out_stream`). Both are read, because a
    /// failure message wants the informational tail as well as the error line,
    /// and stderr gets its own thread: a pipe nobody drains fills up and
    /// deadlocks the child half way through a render.
    fn run(&self, cmd: Command, label: &str) -> Result<()> {
        self.run_tail(cmd, label).map(|_| ())
    }

    /// [`SdRunner::run`], handing back the stderr tail of a run that succeeded —
    /// for the one mode, `upscale`, whose failures don't always reach the exit
    /// status.
    fn run_tail(&self, mut cmd: Command, label: &str) -> Result<VecDeque<String>> {
        // Before the spawn, not in `Monitor::begin`, which runs after it: a
        // child that wrote its first preview in that gap would lose it. Every
        // run clears it, so one that passes no `--preview` (video, convert,
        // upscale) leaves nothing behind that could read as its own.
        if let Some(dir) = self.monitor.preview_dir() {
            let _ = std::fs::remove_dir_all(dir);
            std::fs::create_dir_all(dir).ok();
        }
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .with_context(|| format!("failed to launch sd-cli for {label}"))?;
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");
        let errors = std::thread::spawn(move || drain(stderr, |_| {}));

        self.monitor.begin(label, child);
        let out_tail = drain(stdout, |line| {
            if let Some((step, steps, rate)) = parse_progress(line) {
                self.monitor.advance(label, step, steps, rate);
            }
        });
        // Taken back out before waiting, since `wait` blocks and the lock it
        // would otherwise hold is the one `cancel` needs. Clearing it here also
        // means a cancel arriving late can't land on a child that has exited.
        let status = match self.monitor.end() {
            // EOF on stdout means the child is already on its way out, so this
            // doesn't wait for anything in practice.
            Some(mut child) => child
                .wait()
                .with_context(|| format!("waiting for sd-cli {label}"))?,
            None => bail!("sd-cli {label}: the child went missing while it ran"),
        };
        let err_tail = errors.join().unwrap_or_default();
        if !status.success() {
            // Checked before the crash message, because a killed process exits
            // on a signal and reporting the user's own Stop as a crash would be
            // a lie — but checked *inside* the failure branch, because a cancel
            // that lands after the pass already succeeded cancelled nothing.
            if self.monitor.was_cancelled() {
                bail!("sd-cli {label} {CANCELLED}");
            }
            bail!(
                "sd-cli {label} exited with {status}\n--- stderr ---\n{}\n--- stdout (last {TAIL_LINES} lines) ---\n{}",
                joined(&err_tail),
                joined(&out_tail),
            );
        }
        Ok(err_tail)
    }
}

/// What one `vid_gen` pass reads and writes.
///
/// Grouped rather than passed as three more positional arguments, because
/// `Option<&Path>, Option<&Path>, &Path` at a call site says nothing about which
/// is the first frame, which is the control directory and which is the output.
pub struct VideoIo<'a> {
    /// The first frame, for image-to-video. `None` runs the same checkpoint as
    /// plain text-to-video.
    pub init: Option<&'a Path>,
    /// A *directory* of frames for a VACE pass (`--control-video`) — which is
    /// exactly what a previous pass's `-o` pattern produced, so refining a clip
    /// needs no decode and no ffmpeg.
    pub control: Option<&'a Path>,
    /// The `-o` path. Must carry a `%0Nd` specifier; see [`SdRunner::video`].
    pub out: &'a Path,
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().to_string()
}

/// The kept tail as one block, for an error message.
fn joined(lines: &VecDeque<String>) -> String {
    lines
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join("\n")
}

/// What phase a running `sd-cli` is in. The two are told apart by the unit
/// sd.cpp prints: its byte-progress bar reports `MB/s`/`GB/s` while loading
/// weights, and its sampler reports `s/it`/`it/s`.
///
/// Loading is worth reporting separately rather than folding into one bar:
/// for a model that streams from RAM it is a large and *silent* share of the
/// wall clock, and "stuck at 0/20" is the wrong thing to show for two minutes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Loading,
    Sampling,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Loading => "loading",
            Phase::Sampling => "sampling",
        }
    }
}

/// A snapshot of what the child process is doing, for `GET /progress`.
///
/// `rate` is sd.cpp's own text, kept verbatim rather than parsed into a number.
/// It already picks its unit — `s/it` under one iteration per second and `it/s`
/// above, `MB/s` under a gigabyte and `GB/s` above — and re-deriving that here
/// would be a second implementation of the same decision, free to disagree with
/// what the terminal says.
#[derive(Debug, Clone)]
pub struct Progress {
    pub label: String,
    pub phase: Phase,
    pub step: u32,
    pub steps: u32,
    pub rate: String,
}

/// Shared handle on the one child process the GPU worker has in flight.
///
/// Two jobs: let a connection thread read progress without queueing behind the
/// render it is reporting on, and let one kill it. Cheap to clone — everything
/// is behind the same `Arc`.
#[derive(Clone, Default)]
pub struct Monitor(Arc<MonitorInner>);

#[derive(Default)]
struct MonitorInner {
    progress: Mutex<Option<Progress>>,
    /// The child, parked here for the length of the run so `cancel` can reach
    /// it. `run` takes it back out before waiting, because `wait` blocks and a
    /// lock held across it would be a lock held against the very cancel it is
    /// waiting for.
    child: Mutex<Option<Child>>,
    cancelled: AtomicBool,
    /// Where sd-cli writes its per-step previews. `None` on a detached
    /// monitor, which then asks for no previews at all — nobody would read them.
    preview: Option<PathBuf>,
}

impl Monitor {
    /// The worker's monitor, whose runs write a live preview into `dir`. The
    /// dir is the monitor's rather than the runner's because both ends need
    /// it: the runner names it to sd-cli, a connection thread reads it back.
    pub fn with_preview(dir: PathBuf) -> Self {
        Monitor(Arc::new(MonitorInner {
            preview: Some(dir),
            ..Default::default()
        }))
    }

    pub fn preview_dir(&self) -> Option<&Path> {
        self.0.preview.as_deref()
    }

    /// The number of the newest complete preview, while a run is in flight.
    /// `None` between runs, so a finished or stopped run's last frame is never
    /// shown as if it were live.
    pub fn preview(&self) -> Option<u32> {
        self.snapshot()?;
        newest_complete(self.preview_dir()?)
    }

    /// The bytes of preview `n`, for `GET /preview/{n}.png`. The number is the
    /// whole whitelist: the name is built here, never taken from the request.
    pub fn preview_png(&self, n: u32) -> Option<Vec<u8>> {
        std::fs::read(self.preview_dir()?.join(preview_name(n))).ok()
    }

    /// What to show while a job runs, or `None` when nothing is running.
    pub fn snapshot(&self) -> Option<Progress> {
        self.0.progress.lock().ok()?.clone()
    }

    /// Kill the run in flight. Returns whether there was one.
    ///
    /// Killing the process is the only cancellation sd-cli offers — there is no
    /// protocol to ask it to stop — and it is safe here because every pass
    /// writes to a scratch file that `Scratch::drop` already cleans up after a
    /// failure. A half-written PNG is never the live image.
    pub fn cancel(&self) -> bool {
        let Ok(mut held) = self.0.child.lock() else {
            return false;
        };
        match held.as_mut() {
            Some(child) => {
                self.0.cancelled.store(true, Ordering::SeqCst);
                child.kill().is_ok()
            }
            None => false,
        }
    }

    fn begin(&self, label: &str, child: Child) {
        // Cleared here rather than in `cancel`, so a cancel that lands in the
        // gap between two jobs can't kill the next one before it starts.
        self.0.cancelled.store(false, Ordering::SeqCst);
        if let Ok(mut held) = self.0.child.lock() {
            *held = Some(child);
        }
        if let Ok(mut p) = self.0.progress.lock() {
            *p = Some(Progress {
                label: label.to_string(),
                phase: Phase::Loading,
                step: 0,
                steps: 0,
                rate: String::new(),
            });
        }
    }

    fn end(&self) -> Option<Child> {
        if let Ok(mut p) = self.0.progress.lock() {
            *p = None;
        }
        self.0.child.lock().ok()?.take()
    }

    fn was_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::SeqCst)
    }

    fn advance(&self, label: &str, step: u32, steps: u32, rate: &str) {
        if let Ok(mut p) = self.0.progress.lock() {
            *p = Some(Progress {
                label: label.to_string(),
                phase: if rate.ends_with("B/s") {
                    Phase::Loading
                } else {
                    Phase::Sampling
                },
                step,
                steps,
                rate: rate.to_string(),
            });
        }
    }
}

/// What sd-cli's `p-%04d.png` makes of preview `n` (`format_frame_idx`).
fn preview_name(n: u32) -> String {
    format!("p-{n:04}.png")
}

/// The last twelve bytes of every finished PNG: the empty `IEND` chunk, its
/// length, type and CRC.
const PNG_END: [u8; 12] = [0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82];

/// The highest-numbered preview in `dir` that is complete, deleting the ones
/// before it.
///
/// sd-cli writes each preview in place — no temp name, no rename — so the
/// newest file can be half written when this looks, and an overwritten single
/// file would be half written exactly as often as it was read. Numbered files
/// plus the `IEND` test make that a fall-back to the previous frame instead of
/// a broken image, and don't depend on whether sd-cli prints its progress bar
/// before or after the callback. Pruning keeps a 50-step run to two files; the
/// one being written is never older than the one returned, so it is never
/// deleted under the child.
pub fn newest_complete(dir: &Path) -> Option<u32> {
    let mut found: Vec<u32> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| {
            let name = e.ok()?.file_name();
            let digits = name.to_str()?.strip_prefix("p-")?.strip_suffix(".png")?;
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            digits.parse().ok()
        })
        .collect();
    found.sort_unstable_by(|a, b| b.cmp(a));
    let newest = found.iter().copied().find(|&n| is_complete(&dir.join(preview_name(n))))?;
    for &n in found.iter().filter(|&&n| n < newest) {
        let _ = std::fs::remove_file(dir.join(preview_name(n)));
    }
    Some(newest)
}

fn is_complete(path: &Path) -> bool {
    use std::io::{Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut tail = [0u8; 12];
    f.seek(SeekFrom::End(-12)).is_ok() && f.read_exact(&mut tail).is_ok() && tail == PNG_END
}

/// Pull `step`, `steps` and the rate text out of one of sd.cpp's progress bars:
///
/// ```text
///   |=====>                                            | 3/20 - 1.23s/it
/// ```
///
/// Returns `None` for anything else, which is most of what arrives — the same
/// stream carries every `[INFO]` line sd.cpp logs.
fn parse_progress(line: &str) -> Option<(u32, u32, &str)> {
    // The bar itself contains no '|' past its closing one, so the last is the
    // end of the bar — and a log line with no bar at all fails here.
    let tail = line.rsplit_once('|')?.1;
    let (counts, rate) = tail.split_once('-')?;
    let (step, steps) = counts.trim().split_once('/')?;
    let rate = rate.trim();
    // Every line is followed by an erase-to-end-of-line, since sd.cpp overwrites
    // its own bar in place.
    let rate = match rate.find('\u{1b}') {
        Some(i) => rate[..i].trim_end(),
        None => rate,
    };
    Some((step.parse().ok()?, steps.parse().ok()?, rate))
}

/// Longest run of output kept for an error message. sd.cpp is chatty under
/// `-v` and a video job runs for minutes, so the tail is bounded — the failure
/// is always at the end.
const TAIL_LINES: usize = 40;

/// `--upscale-tile-size` for an ESRGAN pass: the side of the square the model
/// runs over at a time, which is what bounds its VRAM. sd.cpp's own default,
/// kept until it has been measured on the 7900 XTX — a larger tile means fewer
/// seams and fewer dispatches, and an RRDB at 128² is nowhere near 24 GB.
const UPSCALE_TILE: u32 = 128;

/// Read `sd-cli`'s stdout to EOF, feeding progress lines to `on_progress` as
/// they arrive and keeping the last [`TAIL_LINES`] for a failure message.
///
/// Splits on `\r` *and* `\n`, which is the whole reason this isn't
/// `BufRead::lines`: sd.cpp ends every progress bar with a carriage return so it
/// can overwrite it in place, and only the final step of a pass gets a newline.
/// Read by lines, a 20-step render would deliver its progress in one blob after
/// the last step — exactly when it stops being progress.
fn drain(stream: impl Read, mut on_progress: impl FnMut(&str)) -> VecDeque<String> {
    let mut reader = BufReader::new(stream);
    let mut tail: VecDeque<String> = VecDeque::with_capacity(TAIL_LINES);
    let mut line = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut push = |tail: &mut VecDeque<String>, line: &mut Vec<u8>| {
        if line.is_empty() {
            return;
        }
        let text = String::from_utf8_lossy(line).trim_end().to_string();
        line.clear();
        if text.is_empty() {
            return;
        }
        on_progress(&text);
        if tail.len() == TAIL_LINES {
            tail.pop_front();
        }
        tail.push_back(text);
    };
    loop {
        match reader.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                for &b in &chunk[..n] {
                    if b == b'\r' || b == b'\n' {
                        push(&mut tail, &mut line);
                    } else {
                        line.push(b);
                    }
                }
            }
        }
    }
    push(&mut tail, &mut line);
    tail
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A config and a video model, from the same TOML the user writes. Parsing
    /// it rather than building the struct by hand is deliberate: it means these
    /// tests would also catch a field that stopped being readable from
    /// `models.toml`.
    fn video_fixture(extra: &str) -> (Config, VideoModel) {
        let src = format!(
            r#"
[sd]
binary = "/opt/sd-cli"

[[video]]
name = "a14b"
family = "wan22_a14b"
diffusion_model = "low.gguf"
high_noise_diffusion_model = "high.gguf"
vae = "wan_2.1_vae.safetensors"
t5xxl = "umt5.gguf"
width = 832
height = 480
frames = 33
fps = 16
steps = 10
cfg_scale = 3.5
high_noise_steps = 8
flow_shift = 3.0
{extra}
"#
        );
        let mut cfg: Config = toml::from_str(&src).expect("fixture should parse");
        cfg.base_dir = PathBuf::from("/models");
        let m = cfg.video_models().remove(0);
        (cfg, m)
    }

    fn argv(cmd: &Command) -> Vec<String> {
        cmd.get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect()
    }

    /// The value that follows `flag`, or `None` if the flag isn't there.
    fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
        let i = args.iter().position(|a| a == name)?;
        args.get(i + 1).map(|s| s.as_str())
    }

    #[test]
    fn a_video_command_names_every_component() {
        let (cfg, m) = video_fixture("");
        let runner = SdRunner::new(&cfg);
        let cmd = runner.video_cmd(
            &m,
            "a lovely cat",
            "blurry",
            42,
            &VideoIo {
                init: Some(Path::new("/work/first.png")),
                control: None,
                out: Path::new("/work/videos/1/frame_%03d.png"),
            },
        );
        let a = argv(&cmd);

        // `vid_gen` is the literal, not `[sd].mode` — that one says `img_gen`.
        assert_eq!(flag(&a, "-M"), Some("vid_gen"));
        assert_eq!(flag(&a, "--diffusion-model"), Some("/models/low.gguf"));
        assert_eq!(
            flag(&a, "--high-noise-diffusion-model"),
            Some("/models/high.gguf")
        );
        assert_eq!(
            flag(&a, "--vae"),
            Some("/models/wan_2.1_vae.safetensors")
        );
        assert_eq!(flag(&a, "--t5xxl"), Some("/models/umt5.gguf"));
        assert_eq!(flag(&a, "-p"), Some("a lovely cat"));
        assert_eq!(flag(&a, "-n"), Some("blurry"));
        assert_eq!(flag(&a, "-i"), Some("/work/first.png"));
        assert_eq!(flag(&a, "-s"), Some("42"));
        assert_eq!(flag(&a, "-o"), Some("/work/videos/1/frame_%03d.png"));
        assert_eq!(flag(&a, "--video-frames"), Some("33"));
        assert_eq!(flag(&a, "--fps"), Some("16"));
        assert_eq!(flag(&a, "--flow-shift"), Some("3"));
        assert!(a.contains(&"--offload-to-cpu".to_string()));
        assert!(a.contains(&"--diffusion-fa".to_string()));
        // Off by default, and the OOM escalation that costs time.
        assert!(!a.contains(&"--vae-tiling".to_string()));
        // Nothing this model doesn't have.
        assert!(flag(&a, "--llm").is_none());
        assert!(flag(&a, "--clip_vision").is_none());
        assert!(flag(&a, "--control-video").is_none());
    }

    /// The MoE pair is the whole reason `family` exists, so the second expert's
    /// flags are the thing most worth pinning: emit them for a dense checkpoint
    /// and sd.cpp is being asked to split a model with nothing to split.
    #[test]
    fn only_the_moe_pair_gets_the_high_noise_flags() {
        let (cfg, m) = video_fixture("");
        let a = argv(&SdRunner::new(&cfg).video_cmd(
            &m,
            "p",
            "",
            1,
            &VideoIo {
                init: None,
                control: None,
                out: Path::new("o_%03d.png"),
            },
        ));
        assert_eq!(flag(&a, "--high-noise-steps"), Some("8"));
        // Unset follows cfg_scale rather than sd.cpp's own 7.0 default.
        assert_eq!(flag(&a, "--high-noise-cfg-scale"), Some("3.5"));
        assert_eq!(flag(&a, "--high-noise-sampling-method"), Some("euler"));
        // An explicit step count makes `--moe-boundary` dead, so it isn't sent.
        assert!(flag(&a, "--moe-boundary").is_none());

        let (cfg, mut m) = video_fixture("");
        m.family = crate::config::VideoFamily::Wan22Ti2v5b;
        m.high_noise_diffusion_model = None;
        let a = argv(&SdRunner::new(&cfg).video_cmd(
            &m,
            "p",
            "",
            1,
            &VideoIo {
                init: None,
                control: None,
                out: Path::new("o_%03d.png"),
            },
        ));
        for dead in [
            "--high-noise-steps",
            "--high-noise-cfg-scale",
            "--high-noise-sampling-method",
            "--high-noise-diffusion-model",
        ] {
            assert!(flag(&a, dead).is_none(), "{dead} has no meaning here");
        }
    }

    #[test]
    fn the_moe_boundary_is_sent_only_when_it_would_be_honoured() {
        let (cfg, mut m) = video_fixture("");
        m.high_noise_steps = None;
        m.moe_boundary = Some(0.9);
        let a = argv(&SdRunner::new(&cfg).video_cmd(
            &m,
            "p",
            "",
            1,
            &VideoIo {
                init: None,
                control: None,
                out: Path::new("o_%03d.png"),
            },
        ));
        assert!(flag(&a, "--high-noise-steps").is_none());
        assert_eq!(flag(&a, "--moe-boundary"), Some("0.9"));
    }

    /// Sizes and frame counts go out already on the family's grid, so the
    /// sidecar records what ran. sd.cpp would round these itself and warn.
    #[test]
    fn sizes_and_frames_go_out_pre_rounded() {
        let (cfg, mut m) = video_fixture("");
        m.width = 500;
        m.height = 481;
        m.frames = 36;
        let a = argv(&SdRunner::new(&cfg).video_cmd(
            &m,
            "p",
            "",
            1,
            &VideoIo {
                init: None,
                control: None,
                out: Path::new("o_%03d.png"),
            },
        ));
        // 16 is the A14B's grid, and frames round *down* to 4k+1.
        assert_eq!(flag(&a, "-W"), Some("512"));
        assert_eq!(flag(&a, "-H"), Some("496"));
        assert_eq!(flag(&a, "--video-frames"), Some("33"));

        // H3 forces 24 fps whatever was asked, so the command must say 24 —
        // otherwise the sidecar and the clip disagree about its duration.
        let (cfg, mut m) = video_fixture("");
        m.family = crate::config::VideoFamily::MinimaxH3;
        m.fps = 8;
        m.frames = 6;
        let a = argv(&SdRunner::new(&cfg).video_cmd(
            &m,
            "p",
            "",
            1,
            &VideoIo {
                init: None,
                control: None,
                out: Path::new("o_%03d.png"),
            },
        ));
        assert_eq!(flag(&a, "--fps"), Some("24"));
        assert_eq!(flag(&a, "--video-frames"), Some("22"));
    }

    /// The image path's half of the same thing, through `base_cmd` — so it also
    /// covers the refine and hi-res passes, which build their command there.
    ///
    /// The second assertion is the one worth having: `--lora-model-dir` goes out
    /// even with nothing attached, because an *empty* one makes sd.cpp skip tag
    /// extraction altogether and a `<lora:…>` typed into the prompt box would
    /// then be conditioned as words.
    #[test]
    fn image_loras_ride_in_the_prompt() {
        let cfg: Config = toml::from_str(
            r#"
[sd]
binary = "/opt/sd-cli"
"#,
        )
        .expect("fixture should parse");
        let mut p = Profile {
            model: PathBuf::from("/models/sdxl.safetensors"),
            ..Default::default()
        };

        let bare = argv(&SdRunner::new(&cfg).base_cmd(
            &p,
            "a lovely cat",
            None,
            1,
            Path::new("out.png"),
            6.0,
        ));
        assert_eq!(flag(&bare, "-p"), Some("a lovely cat"));
        assert!(bare.iter().any(|a| a == "--lora-model-dir"));

        p.loras = vec![
            crate::config::LoraRef {
                name: "detail.safetensors".into(),
                strength: 0.8,
            },
            crate::config::LoraRef {
                name: "sdxl/style.safetensors".into(),
                strength: -0.5,
            },
        ];
        let a = argv(&SdRunner::new(&cfg).base_cmd(
            &p,
            "a lovely cat",
            None,
            1,
            Path::new("out.png"),
            6.0,
        ));
        assert_eq!(
            flag(&a, "-p"),
            Some("a lovely cat<lora:detail.safetensors:0.8><lora:sdxl/style.safetensors:-0.5>")
        );
    }

    /// A bundled checkpoint is one `-m`; a DiT is `--diffusion-model` plus a
    /// flag per part and its family's own flags. And `-m` must not appear for
    /// a DiT: sd.cpp would load its tensors under the wrong prefix.
    #[test]
    fn a_dit_is_handed_its_parts_and_a_checkpoint_is_not() {
        let cfg: Config = toml::from_str("[sd]\nbinary = \"/opt/sd-cli\"\n").unwrap();
        let cmd = |p: &Profile| {
            argv(&SdRunner::new(&cfg).base_cmd(p, "a cat", None, 1, Path::new("out.png"), 1.0))
        };

        let sdxl = Profile {
            model: PathBuf::from("/models/sdxl.safetensors"),
            ..Default::default()
        };
        let a = cmd(&sdxl);
        assert_eq!(flag(&a, "-m"), Some("/models/sdxl.safetensors"));
        assert!(!a.iter().any(|x| x == "--diffusion-model" || x == "--diffusion-fa"));

        let z = Profile {
            model: PathBuf::from("/models/zimage.safetensors"),
            family: crate::config::ImageFamily::ZImage,
            components: Some(crate::config::Components {
                vae: Some(PathBuf::from("/models/vae/ae.safetensors")),
                llm: Some(PathBuf::from("/models/text_encoders/qwen3.safetensors")),
                ..Default::default()
            }),
            ..Default::default()
        };
        let a = cmd(&z);
        assert_eq!(flag(&a, "--diffusion-model"), Some("/models/zimage.safetensors"));
        assert_eq!(flag(&a, "--vae"), Some("/models/vae/ae.safetensors"));
        assert_eq!(flag(&a, "--llm"), Some("/models/text_encoders/qwen3.safetensors"));
        assert!(!a.iter().any(|x| x == "-m" || x == "--t5xxl" || x == "--clip_l"));
        assert_eq!(a.iter().filter(|x| *x == "--diffusion-fa").count(), 1);
    }

    /// A VAE picked for a bundled checkpoint is an override on `-m`, not a
    /// reason to run it as a bare diffusion model — which would load the UNet
    /// under the wrong prefix and find none of its weights.
    #[test]
    fn a_checkpoint_keeps_its_m_when_it_is_handed_a_vae() {
        let cfg: Config = toml::from_str("[sd]\nbinary = \"/opt/sd-cli\"\n").unwrap();
        let mut sdxl = Profile {
            model: PathBuf::from("/models/sdxl.safetensors"),
            bundled: Some(true),
            ..Default::default()
        };
        sdxl.set_part("vae", Some(PathBuf::from("/models/vae/sdxl_vae.safetensors")));
        let a = argv(&SdRunner::new(&cfg).base_cmd(&sdxl, "a cat", None, 1, Path::new("o.png"), 1.0));
        assert_eq!(flag(&a, "-m"), Some("/models/sdxl.safetensors"));
        assert_eq!(flag(&a, "--vae"), Some("/models/vae/sdxl_vae.safetensors"));
        assert!(!a.iter().any(|x| x == "--diffusion-model"));

        // Emptied again, it is the checkpoint as it was.
        sdxl.set_part("vae", None);
        assert_eq!(sdxl.components, None);
        assert!(sdxl.is_bundled());
    }

    /// Auto is the absence of the flag, not a value of it: sd.cpp would refuse
    /// `--scheduler auto`, and with no flag it picks Flux's own schedule for a
    /// Flux, which no single name sent from here could.
    #[test]
    fn auto_sends_no_scheduler_and_a_name_sends_itself() {
        let cfg: Config = toml::from_str("[sd]\nbinary = \"/opt/sd-cli\"\n").unwrap();
        let mut p = Profile { model: PathBuf::from("/models/m.safetensors"), ..Default::default() };
        assert_eq!(p.scheduler, crate::config::AUTO_SCHEDULER);
        let a = argv(&SdRunner::new(&cfg).base_cmd(&p, "a cat", None, 1, Path::new("o.png"), 1.0));
        assert!(!a.iter().any(|x| x == "--scheduler"));
        p.scheduler = "karras".into();
        let a = argv(&SdRunner::new(&cfg).base_cmd(&p, "a cat", None, 1, Path::new("o.png"), 1.0));
        assert_eq!(flag(&a, "--scheduler"), Some("karras"));
    }

    /// LoRAs are inline in the prompt, which is sd.cpp's spelling — and the
    /// `|high_noise|` form is how one targets an expert.
    #[test]
    fn loras_are_appended_to_the_prompt() {
        let (cfg, m) = video_fixture(
            r#"loras = ["low4:1", "|high_noise|high4:1"]
lora_dir = "loras""#,
        );
        let a = argv(&SdRunner::new(&cfg).video_cmd(
            &m,
            "a lovely cat",
            "",
            1,
            &VideoIo {
                init: None,
                control: None,
                out: Path::new("o_%03d.png"),
            },
        ));
        assert_eq!(
            flag(&a, "-p"),
            Some("a lovely cat<lora:low4:1><lora:|high_noise|high4:1>")
        );
        assert_eq!(flag(&a, "--lora-model-dir"), Some("/models/loras"));
        // An empty negative is omitted rather than sent as "".
        assert!(flag(&a, "-n").is_none());
    }

    /// A VACE refine: frames in, frames out, and the input directory is what
    /// the first pass already wrote.
    #[test]
    fn a_control_video_is_a_directory_of_frames() {
        let (cfg, mut m) = video_fixture("");
        m.family = crate::config::VideoFamily::Wan21Vace;
        m.high_noise_diffusion_model = None;
        m.vace_strength = Some(0.5);
        let a = argv(&SdRunner::new(&cfg).video_cmd(
            &m,
            "p",
            "",
            1,
            &VideoIo {
                init: None,
                control: Some(Path::new("/work/videos/7")),
                out: Path::new("/work/videos/8/frame_%03d.png"),
            },
        ));
        assert_eq!(flag(&a, "--control-video"), Some("/work/videos/7"));
        assert_eq!(flag(&a, "--vace-strength"), Some("0.5"));
    }

    /// The exact shape `print_progress_line` produces (sd.cpp
    /// `src/core/util.cpp`): a 50-cell bar, the counts, and a rate whose unit
    /// flips at one iteration per second. The trailing escape is sd.cpp erasing
    /// the rest of its own line.
    #[test]
    fn parses_a_sampler_bar() {
        let bar = "  |=====>                                           | 3/20 - 1.23s/it\u{1b}[K";
        assert_eq!(parse_progress(bar), Some((3, 20, "1.23s/it")));
    }

    #[test]
    fn parses_a_fast_sampler_bar() {
        let bar = "  |==================================================| 4/4 - 2.50it/s";
        assert_eq!(parse_progress(bar), Some((4, 4, "2.50it/s")));
    }

    /// Weight loading uses the same shape with a different bar character and a
    /// byte rate. `Phase` is derived from that unit, so it has to survive.
    #[test]
    fn parses_a_loading_bar() {
        let bar = "  |##################                                | 18/50 - 412.00MB/s";
        let (step, steps, rate) = parse_progress(bar).expect("a bar");
        assert_eq!((step, steps), (18, 50));
        assert!(rate.ends_with("B/s"), "{rate} should read as loading");
    }

    /// Everything else on the stream is log output, and most of it contains
    /// both a '-' and a '/'. None of it may parse as progress.
    #[test]
    fn ignores_log_lines() {
        for line in [
            "[INFO ] stable-diffusion.cpp:1234 - loading model from models/sd_xl_base_1.0.safetensors",
            "[INFO ] model.cpp:900  - sampling completed, taking 8.42s",
            "  |==================================================| bogus",
            "",
            "|",
        ] {
            assert_eq!(parse_progress(line), None, "{line:?} should not parse");
        }
    }

    /// A `\r`-separated stream has to arrive as separate lines, or progress
    /// isn't progress. This is the case `BufRead::lines` gets wrong.
    #[test]
    fn drain_splits_on_carriage_returns() {
        let stream = "  |=>  | 1/3 - 1.00s/it\u{1b}[K\r  |==> | 2/3 - 1.00s/it\u{1b}[K\r  |====| 3/3 - 1.00s/it\n";
        let mut seen = Vec::new();
        let tail = drain(stream.as_bytes(), |line| {
            if let Some((step, steps, _)) = parse_progress(line) {
                seen.push((step, steps));
            }
        });
        assert_eq!(seen, vec![(1, 3), (2, 3), (3, 3)]);
        assert_eq!(tail.len(), 3);
    }

    /// The tail is for error messages, so it must be the *last* lines and must
    /// not grow without bound over a long run.
    #[test]
    fn drain_keeps_only_the_last_lines() {
        let mut stream = String::new();
        for i in 0..(TAIL_LINES * 3) {
            stream.push_str(&format!("line {i}\n"));
        }
        let tail = drain(stream.as_bytes(), |_| {});
        assert_eq!(tail.len(), TAIL_LINES);
        assert_eq!(
            tail.back().map(String::as_str),
            Some(format!("line {}", TAIL_LINES * 3 - 1).as_str())
        );
    }

    /// The newest preview may be half written; the one before it is what to
    /// show, and everything older goes. Names that aren't sd-cli's are left
    /// alone and never read as a frame.
    #[test]
    fn the_newest_complete_preview_wins_and_older_ones_go() {
        let dir = std::env::temp_dir().join(format!("rustimage-preview-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(newest_complete(&dir), None, "a missing dir is no preview");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(newest_complete(&dir), None, "an empty dir is no preview");

        let png = |body: &[u8]| [b"\x89PNG".as_slice(), body, &PNG_END].concat();
        for n in [0, 1, 2] {
            std::fs::write(dir.join(preview_name(n)), png(b"frame")).unwrap();
        }
        std::fs::write(dir.join(preview_name(3)), b"\x89PNG half").unwrap();
        std::fs::write(dir.join("p-.png"), png(b"")).unwrap();
        std::fs::write(dir.join("p-9999.png.part"), png(b"")).unwrap();
        std::fs::write(dir.join("other.png"), png(b"")).unwrap();

        assert_eq!(newest_complete(&dir), Some(2));
        assert!(!dir.join(preview_name(0)).exists() && !dir.join(preview_name(1)).exists());
        assert!(dir.join(preview_name(3)).exists(), "the one being written stays");
        assert!(dir.join("other.png").exists());

        // The child finishes writing it: now that one is the frame.
        std::fs::write(dir.join(preview_name(3)), png(b"frame")).unwrap();
        assert_eq!(newest_complete(&dir), Some(3));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Only the worker's monitor asks for previews, and only while a run is
    /// in flight does it report one.
    #[test]
    fn previews_are_asked_for_by_the_worker_alone() {
        let cfg: Config = toml::from_str("[sd]\nbinary = \"/opt/sd-cli\"\n").unwrap();
        let p = Profile {
            model: PathBuf::from("/models/sdxl.safetensors"),
            ..Default::default()
        };
        let detached = argv(&SdRunner::new(&cfg).base_cmd(&p, "a cat", None, 1, Path::new("o.png"), 1.0));
        assert!(!detached.iter().any(|a| a.starts_with("--preview")));

        let m = Monitor::with_preview(PathBuf::from("/work/current/preview"));
        let a = argv(&SdRunner::watched(&cfg, &m).base_cmd(&p, "a cat", None, 1, Path::new("o.png"), 1.0));
        assert_eq!(flag(&a, "--preview"), Some("proj"));
        assert_eq!(flag(&a, "--preview-path"), Some("/work/current/preview/p-%04d.png"));
        assert_eq!(m.preview(), None, "nothing running, nothing to show");
    }

    /// `Phase` is derived from the unit sd.cpp chose, so that derivation is a
    /// behaviour and not an implementation detail.
    #[test]
    fn phase_follows_the_rate_unit() {
        let m = Monitor::default();
        m.advance("txt2img", 1, 2, "412.00MB/s");
        assert_eq!(m.snapshot().expect("a run").phase, Phase::Loading);
        m.advance("txt2img", 1, 2, "1.23s/it");
        assert_eq!(m.snapshot().expect("a run").phase, Phase::Sampling);
    }

    /// Nothing running is the common case — the button is pressable whenever the
    /// UI feels like it, so this must not panic or claim a kill it didn't make.
    #[test]
    fn cancel_is_a_no_op_when_nothing_is_running() {
        let m = Monitor::default();
        assert!(!m.cancel());
        assert!(m.snapshot().is_none());
    }

    /// The whole contract of Stop: a registered child actually dies, the monitor
    /// says so, and it comes back idle. Skipped rather than failed if `sleep`
    /// isn't on the box, since the fixture isn't the thing under test.
    #[test]
    fn cancel_kills_the_registered_child() {
        let Ok(child) = Command::new("sleep").arg("30").spawn() else {
            return;
        };
        let m = Monitor::default();
        m.begin("fixture", child);
        assert!(m.snapshot().is_some(), "a run should be visible");
        assert!(m.cancel(), "there was a child to kill");
        assert!(m.was_cancelled());
        let mut child = m.end().expect("the child comes back to be reaped");
        assert!(
            !child.wait().expect("reaping").success(),
            "a killed process is not a success"
        );
        assert!(m.snapshot().is_none(), "idle once the run is over");
    }

    /// The closest thing to the real path that runs without a GPU: a child
    /// process writing sd.cpp-shaped bars down a real pipe, drained into a real
    /// monitor. The in-memory tests above can't catch a mistake in the piping
    /// itself, and on the GPU that mistake looks like a progress bar that never
    /// moves.
    #[test]
    fn a_child_process_drives_the_monitor() {
        let script = r"printf '  |=>  | 1/3 - 1.00s/it\033[K\r'; \
                       printf '  |==> | 2/3 - 1.00s/it\033[K\r'; \
                       printf '  |====| 3/3 - 0.90s/it\033[K\n'";
        let Ok(mut child) = Command::new("sh")
            .args(["-c", script])
            .stdout(Stdio::piped())
            .spawn()
        else {
            return;
        };
        let stdout = child.stdout.take().expect("stdout was piped");
        let m = Monitor::default();
        let seen = std::cell::RefCell::new(Vec::new());
        let tail = drain(stdout, |line| {
            if let Some((step, steps, rate)) = parse_progress(line) {
                seen.borrow_mut().push(step);
                m.advance("fixture", step, steps, rate);
            }
        });
        assert!(child.wait().expect("reaping").success());
        assert_eq!(*seen.borrow(), vec![1, 2, 3], "every bar should arrive");
        assert_eq!(tail.len(), 3);
        let p = m.snapshot().expect("a run in flight");
        assert_eq!((p.step, p.steps), (3, 3));
        assert_eq!(p.phase, Phase::Sampling);
        assert_eq!(p.rate, "0.90s/it");
    }
}
