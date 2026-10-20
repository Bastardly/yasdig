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

/// The reference images onto a command: one `-r` each, in the order the
/// prompt names them. Only ever the `refs` a caller hands over — never
/// `Profile::references`, which every pass shares through `base_cmd`: a refine
/// handed the generation's references would redraw toward them rather than
/// finish the picture it was given. A Refine takes them only when asked.
fn push_refs(cmd: &mut Command, p: &Profile, refs: &[PathBuf], ref_side: Option<u32>) {
    for r in refs {
        cmd.args(["-r", &path_str(r)]);
    }
    // sd.cpp resizes every reference itself before encoding it — to the
    // output's area, up to a megapixel, *up* as well as down — so a
    // smaller file on our side would be scaled back. Its own cap is the
    // control (`vae_input_max_pixels`, `docs/edit.md`); naming only that
    // key keeps the model's automatic preset. A hand-written
    // `--ref-image-args` in `extra_args` wins.
    let placed = p.extra_args.iter().any(|a| a == "--ref-image-args");
    if let Some(side) = ref_side.filter(|s| *s > 0 && !refs.is_empty() && !placed) {
        cmd.args(["--ref-image-args", &format!("vae_input_max_pixels={}", side * side)]);
    }
}

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
                "sd binary not found at {} — `yasdig doctor` says why and what to do",
                bin.display()
            );
        }
        Ok(())
    }

    /// text-to-image. Writes a PNG to `out`.
    #[allow(clippy::too_many_arguments)]
    pub fn txt2img(
        &self,
        p: &Profile,
        prompt: &str,
        negative: Option<&str>,
        seed: i64,
        out: &Path,
        refs: &[PathBuf],
        ref_side: Option<u32>,
    ) -> Result<()> {
        let cmd = self.txt2img_cmd(p, prompt, negative, seed, out, refs, ref_side);
        self.run(cmd, "txt2img")
    }

    /// `txt2img`'s command, split out so a test can read it. The reference
    /// images are the `refs` handed in ([`push_refs`]), never the profile's.
    #[allow(clippy::too_many_arguments)]
    fn txt2img_cmd(
        &self,
        p: &Profile,
        prompt: &str,
        negative: Option<&str>,
        seed: i64,
        out: &Path,
        refs: &[PathBuf],
        ref_side: Option<u32>,
    ) -> Command {
        let mut cmd = self.base_cmd(p, prompt, negative, seed, out, p.cfg_scale);
        cmd.args(["-W", &p.width.to_string()]);
        cmd.args(["-H", &p.height.to_string()]);
        cmd.args(["--steps", &p.steps.to_string()]);
        // The generate pass only: an img2img trims its schedule by strength
        // (invariant 2), which a fixed list doesn't survive. A finishing pass
        // keeps the LoRA and runs sd.cpp's own schedule for its steps.
        if !p.sigmas.is_empty() {
            cmd.args(["--sigmas", &p.sigmas]);
        }
        push_refs(&mut cmd, p, refs, ref_side);
        cmd
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
    /// flash attention in the diffusion model. Both matter above ~1536²: measured on a 7900 XTX, a
    /// 2048² pass takes 122 s without flash attention and 20 s with it.
    #[allow(clippy::too_many_arguments)]
    fn img2img_cmd(
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
        refs: &[PathBuf],
        ref_side: Option<u32>,
    ) -> Command {
        let mut cmd = self.base_cmd(p, prompt, negative, seed, out, cfg);
        // sd.cpp derives dimensions from the init image, so no -W/-H here.
        cmd.args(["-i", &path_str(init)]);
        // sd.cpp encodes references whether or not there is an init image
        // (`pipeline/image.cpp`), so an area refine can follow one.
        push_refs(&mut cmd, p, refs, ref_side);
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
            "[yasdig] {label}: {steps} effective steps at strength {strength} \
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
            // A DiT family already runs with it at every size. This lands after
            // `extra_args`, and sd-cli takes the last `--flash-attn`, so a hand-
            // written choice keeps ours out.
            if p.hires_flash_attn && p.family.is_unet() && !crate::config::names_flash_attn(&p.extra_args) {
                cmd.args(crate::config::DIFFUSION_FLASH_ATTN);
            }
        }
        cmd
    }

    /// img2img: one pass over `init`, written to `out`. See [`Self::img2img_cmd`]
    /// for what the arguments do to the command line.
    #[allow(clippy::too_many_arguments)]
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
        refs: &[PathBuf],
        ref_side: Option<u32>,
    ) -> Result<()> {
        let cmd = self.img2img_cmd(
            p, prompt, negative, seed, init, out, strength, steps, cfg, big_image, label, refs, ref_side,
        );
        self.run(cmd, label)
    }

    /// An img2img pass that only redraws where `mask` is white — the Redraw
    /// pass, over a crop the caller has already cut and enlarged (see
    /// [`crate::redraw`]). The same command as [`Self::img2img`] plus
    /// `--mask`, so the step floor and the schedule scaling apply unchanged.
    /// Any model, not just an inpainting checkpoint: sd.cpp blends the
    /// original latent back in outside the mask after every step
    /// (`diffusion_engine.cpp`, `denoise_mask`).
    #[allow(clippy::too_many_arguments)]
    pub fn redraw(
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
        mask: &Path,
        refs: &[PathBuf],
        ref_side: Option<u32>,
    ) -> Result<()> {
        let mut cmd = self.img2img_cmd(
            p, prompt, negative, seed, init, out, strength, steps, cfg, big_image, label, refs, ref_side,
        );
        cmd.args(["--mask", &path_str(mask)]);
        self.run(cmd, label)
    }

    /// One Extend strip: a FLUX.1 Fill pass over `init` with `mask` white where
    /// to draw. `p` is the Fill model's own profile (`Config::fill_model`), so
    /// `base_cmd` brings its parts, its `--guidance 30` and the memory settings.
    ///
    /// Strength 1 and the profile's steps as they are — **not** `img2img_cmd`.
    /// That function scales the schedule by 1/strength and floors the steps for
    /// a *partial* redraw, and keeps strength under 1 because an ordinary model
    /// at 1 throws the init away (invariant 3). A Fill model doesn't: the masked
    /// image and the mask are its input beside the noise, so at 1 it draws the
    /// strip from scratch *knowing* the rest — which is the whole point of it.
    #[allow(clippy::too_many_arguments)]
    pub fn fill(
        &self,
        p: &Profile,
        prompt: &str,
        negative: Option<&str>,
        seed: i64,
        init: &Path,
        mask: &Path,
        out: &Path,
    ) -> Result<()> {
        let cmd = self.fill_cmd(p, prompt, negative, seed, init, mask, out);
        self.run(cmd, "extend")
    }

    #[allow(clippy::too_many_arguments)]
    fn fill_cmd(
        &self,
        p: &Profile,
        prompt: &str,
        negative: Option<&str>,
        seed: i64,
        init: &Path,
        mask: &Path,
        out: &Path,
    ) -> Command {
        let mut cmd = self.base_cmd(p, prompt, negative, seed, out, p.cfg_scale);
        cmd.args(["-i", &path_str(init), "--mask", &path_str(mask)]);
        cmd.args(["--strength", "1", "--steps", &p.steps.max(1).to_string()]);
        cmd
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
        cmd.args(crate::config::memory_args(m.memory_mode(), &m.extra_args));
        cmd.args(crate::config::gpu_reserve_args(self.cfg.settings().gpu_reserve_gb, &m.extra_args));
        if m.vae_tiling {
            cmd.arg("--vae-tiling");
        }
        if m.flash_attn {
            cmd.args(crate::config::DIFFUSION_FLASH_ATTN);
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
    /// `-M upscale` never builds a diffusion context (`examples/cli/main.cpp:889`
    /// hands the init image straight to the upscaler), so there is no checkpoint,
    /// no prompt and no seed. sd.cpp reads `.pth` natively and maps the old
    /// ESRGAN key layout (`src/name_conversion.cpp:1273`), but it only
    /// implements **RRDBNet**: every other architecture goes through
    /// [`crate::sr`] instead, and `pipeline::enlarge` decides which by reading
    /// the file, so only an ESRGAN should ever reach this.
    ///
    /// **A model it can't build is not a failure exit.** `new_upscaler_ctx`
    /// failing is a `LOG_ERROR`, after which sd-cli saves the *input unchanged*
    /// and exits 0 (`main.cpp:966`). So the stderr tail is checked here for the
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
        // `examples/common/common.cpp:2274`), so this does not reach the text
        // encoder as words. Only the positive prompt is scanned, which is why
        // `-n` below is left alone.
        let mut prompt = prompt.to_string();
        for l in &p.loras {
            prompt.push_str(&format!("<lora:{}:{}>", l.name, l.strength));
        }
        cmd.args(["-p", &prompt]);
        // Unconditional, not just when there are tags to resolve, and both
        // halves of that matter. An *empty* `--lora-model-dir` makes sd.cpp skip
        // tag extraction entirely (`common.cpp:2275`) — the `<lora:…>` text then
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
        if let Some(mode) = cache_flag(p) {
            cmd.args(["--cache-mode", mode]);
        }
        // Every pass, not just the generate: a refine that doesn't fit is the
        // same failure as a generate that doesn't. `auto` adds nothing.
        cmd.args(crate::config::memory_args(&p.memory, &p.extra_args));
        let settings = self.cfg.settings();
        cmd.args(crate::config::gpu_reserve_args(settings.gpu_reserve_gb, &p.extra_args));
        // Every pass that decodes — which is all of them. Pinned either way,
        // the rest left to sd.cpp's `auto` probe. Not when `extra_args` names
        // it, by hand: an explicit `--conv-direct` overrides the deprecated
        // `--vae-conv-direct` whichever comes last.
        if !p.extra_args.iter().any(|a| a == "--vae-conv-direct" || a == "--conv-direct") {
            let vae = if settings.vae_conv_direct { "auto,vae=on" } else { "auto,vae=off" };
            cmd.args(["--conv-direct", vae]);
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
        // What the command asks of memory, read off its own arguments so the
        // report says what actually ran. A prompt is what marks a diffusion
        // run — an upscale or a convert leaves the last one's placement alone.
        let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        let params = args
            .iter()
            .position(|a| a == "--params-backend")
            .and_then(|i| args.get(i + 1).cloned());
        // By the default part — a bare `cpu` or `*=cpu` is the RAM mode even
        // with the text encoder released beside it — then by where the model
        // itself lives.
        let mode = match params.as_deref() {
            Some(p) if p.split(',').any(|a| matches!(a.trim(), "cpu" | "*=cpu" | "all=cpu" | "default=cpu")) => "ram",
            Some(p) if p.split(',').any(|a| a.trim() == "diffusion=disk" || a.trim() == "disk") => "disk",
            Some(_) => "custom",
            None if args.iter().any(|a| a == "--offload-to-cpu") => "ram",
            None => crate::config::MEMORY_AUTO,
        };
        let diffusion = args.iter().any(|a| a == "-p");
        if diffusion {
            self.monitor.set_placement(match (&params, mode) {
                (Some(p), _) => describe_params(p),
                (None, "ram") => describe_params("cpu"),
                _ => None,
            });
        }
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .with_context(|| format!("failed to launch sd-cli for {label}"))?;
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");
        let errors = std::thread::spawn(move || drain(stderr, |_| {}));

        self.monitor.begin(label, child);
        // The last progress seen, kept here because `end` clears the monitor's
        // before the exit status is known — and it is what says *when* a run
        // that died ran out of memory.
        let mut last: Option<(Phase, u32, u32)> = None;
        // The last *sampling* step on its own: with the model streamed onto the
        // card a segment at a time, byte-rate bars interleave with the steps,
        // and the last bar overall says "loading" mid-drawing.
        let mut last_step: Option<(u32, u32)> = None;
        let out_tail = drain(stdout, |line| {
            if let Some((step, steps, rate)) = parse_progress(line) {
                self.monitor.advance(label, step, steps, rate);
                let phase = Phase::of_rate(rate);
                if matches!(phase, Phase::Sampling) {
                    last_step = Some((step, steps));
                }
                last = Some((phase, step, steps));
            } else if let Some(spec) = parse_auto_fit(line) {
                if diffusion {
                    self.monitor.set_placement(spec.as_deref().and_then(describe_params));
                }
            } else if let Some(skip) = parse_cache_skip(line) {
                if let Ok(mut s) = self.monitor.0.cache_skip.lock() {
                    *s = Some(skip);
                }
            } else if let Some(ms) = parse_load_ms(line) {
                if let Ok(mut total) = self.monitor.0.load_ms.lock() {
                    *total += ms;
                }
            } else if let Some((stage, ms)) = parse_stage(line) {
                if let Ok(mut s) = self.monitor.0.stages.lock() {
                    s.add(stage, ms);
                }
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
            // sd.cpp's out-of-memory is a warning a few lines before a generic
            // "failed"; the system's is a SIGKILL from the OOM killer with no
            // message at all. Either way the error is typed, so the page can
            // offer the ways out rather than grep a sentence for them.
            let gpu = out_tail.iter().chain(err_tail.iter()).any(|l| is_out_of_memory(l));
            if gpu || killed_by_system(&status) {
                let (phase, step, steps) = oom_phase(out_tail.iter().chain(err_tail.iter()), last, last_step);
                let shortfall = out_tail.iter().chain(err_tail.iter()).rev().find_map(|l| parse_shortfall(l));
                return Err(OutOfMemory {
                    kind: if gpu { "gpu" } else { "ram" },
                    phase,
                    step,
                    steps,
                    placement: self.monitor.placement(),
                    mode,
                    ram_needed: ram_mode_bytes(&args),
                    reserve_gb: args
                        .iter()
                        .position(|a| a == "--max-vram")
                        .and_then(|i| args.get(i + 1))
                        .and_then(|v| v.strip_prefix('-'))
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0.0),
                    lost: out_tail
                        .iter()
                        .chain(err_tail.iter())
                        .any(|l| l.contains("Not enough memory for command submission") || l.contains("ErrorDeviceLost")),
                    need_device_mb: shortfall.map(|s| s.0),
                    need_budget_mb: shortfall.map(|s| s.1),
                    avail_device_mb: shortfall.and_then(|s| s.2),
                    avail_budget_mb: shortfall.and_then(|s| s.3),
                    references: args.iter().filter(|a| *a == "-r").count() as u32,
                    gtt_total: gtt().map(|g| g.0),
                    gtt_free: gtt().map(|g| g.0.saturating_sub(g.1)),
                    encoder_on_card_mb: out_tail.iter().find_map(|l| parse_encoder_on_card(l)),
                    // Read after the child is gone, so its own memory is back.
                    ram_free: available_ram(),
                    label: label.to_string(),
                    log: format!(
                        "--- stderr ---\n{}\n--- stdout (last {TAIL_LINES} lines) ---\n{}",
                        joined(&err_tail),
                        joined(&out_tail)
                    ),
                }
                .into());
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

/// Whether an sd-cli log line is its way of saying the GPU ran out of memory:
/// the model manager's "cannot make enough memory available", or ggml-vulkan's
/// own allocation failure. The run then fails with a generic line, so this is
/// what tells the person the cause.
fn is_out_of_memory(line: &str) -> bool {
    line.contains("cannot make enough memory available")
        || line.contains("ErrorOutOfDeviceMemory")
        || line.contains("failed to allocate")
        // RADV's, on a submission the driver couldn't back; the device is
        // lost right after, which is the line people see.
        || line.contains("Not enough memory for command submission")
}

/// Whether the child was ended by the system rather than by us: SIGKILL with
/// no cancel of ours is Linux's OOM killer, which is how running out of
/// *system* memory looks — no message, just gone.
#[cfg(unix)]
fn killed_by_system(status: &std::process::ExitStatus) -> bool {
    use std::os::unix::process::ExitStatusExt;
    status.signal() == Some(9)
}
#[cfg(not(unix))]
fn killed_by_system(_: &std::process::ExitStatus) -> bool {
    false
}

/// When a run that ran out of memory died: `loading`, `references`, `drawing`
/// or `decoding`, with the last sampling step done.
///
/// sd.cpp's own failure lines first — they name the stage — and only then the
/// last progress bar. The bar alone misleads once a model is streamed onto the
/// card segment by segment: each segment prints a byte-rate bar, so a run that
/// died preparing step 1's weights *looks* like one still loading the model.
fn oom_phase<'a>(
    lines: impl Iterator<Item = &'a String> + Clone,
    last: Option<(Phase, u32, u32)>,
    last_step: Option<(u32, u32)>,
) -> (&'static str, u32, u32) {
    let said = |needles: &[&str]| lines.clone().any(|l| needles.iter().any(|n| l.contains(n)));
    let (s, n) = last_step.unwrap_or((0, 0));
    if said(&["vae decode compute failed", "decode_first_stage failed"]) {
        ("decoding", s, n)
    } else if said(&["failed to encode reference image", "vae encode compute failed"]) {
        ("references", 0, 0)
    } else if said(&["sampling failed", "diffusion model compute failed"]) {
        ("drawing", s, n)
    } else {
        match last {
            None | Some((Phase::Loading, ..)) => ("loading", 0, 0),
            Some((_, s, n)) if s < n => ("drawing", s, n),
            Some((_, s, n)) => ("decoding", s, n),
        }
    }
}

/// A run that ran out of memory, typed so the page can offer what to do about
/// it — the Memory setting, a smaller size, a smaller file — instead of
/// matching words in a sentence. [`oom_json`] finds it in an error's chain.
#[derive(Debug, Clone, serde::Serialize)]
pub struct OutOfMemory {
    /// `gpu`, or `ram` for system memory.
    pub kind: &'static str,
    /// `loading`, `references`, `drawing` or `decoding` — see [`oom_phase`].
    pub phase: &'static str,
    pub step: u32,
    pub steps: u32,
    /// Where the weights were, when anything was off the card.
    pub placement: Option<String>,
    /// The memory mode the command ran with: one of `MEMORY_MODES`, or
    /// `custom` for a placement written into `extra_args`.
    pub mode: &'static str,
    /// What System RAM would hold for this command, in bytes: see
    /// [`ram_mode_bytes`]. The box shows it beside the option and only offers
    /// the option when it fits in `ram_free`.
    pub ram_needed: Option<u64>,
    /// System RAM available once the run had ended (`MemAvailable`), in bytes —
    /// the figure sd.cpp's own planner starts from. `None` off Linux.
    pub ram_free: Option<u64>,
    /// GB the command kept free on the card (`--max-vram -N`), 0 for none.
    pub reserve_gb: f32,
    /// The driver, not sd.cpp, refused the work: RADV's "Not enough memory for
    /// command submission" and a lost device. sd.cpp's own check had passed,
    /// so the card was packed too tight rather than too small — what a reserve
    /// is for.
    pub lost: bool,
    /// sd.cpp's own figures from its "cannot make enough memory available"
    /// line, in MB: what the step needed and what was there, on the device and
    /// in the budget sd.cpp planned with. They tell a full card (device short)
    /// from a budget starved by a reserve (device fine, budget short).
    pub need_device_mb: Option<f64>,
    pub avail_device_mb: Option<f64>,
    pub need_budget_mb: Option<f64>,
    pub avail_budget_mb: Option<f64>,
    /// How many reference images rode along (`-r`). Each is a whole image's
    /// worth of tokens in every step — four of them make a drawing five times
    /// the size the output alone would be.
    pub references: u32,
    /// The GPU's window onto system RAM (amdgpu's GTT), total and free, in
    /// bytes — `None` off amdgpu. This, not free RAM, is what System RAM has to
    /// fit in: sd.cpp keeps RAM-resident weights in the device's *pinned host*
    /// buffer type (`model_manager.cpp`, `params_buffer_type_for`), and the
    /// kernel caps that pool at half of RAM by default. Past it a submission
    /// can't be made resident — RADV's "Not enough memory for command
    /// submission" and a lost device, which a reserve can't fix.
    pub gtt_total: Option<u64>,
    pub gtt_free: Option<u64>,
    /// MB of text encoder sd.cpp kept on the card for the whole run, off its
    /// "total params memory size" line — only needed for the first seconds.
    pub encoder_on_card_mb: Option<f64>,
    #[serde(skip)]
    pub label: String,
    #[serde(skip)]
    pub log: String,
}

impl std::fmt::Display for OutOfMemory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let what = if self.kind == "gpu" { "GPU memory" } else { "system memory (RAM)" };
        let when = match self.phase {
            "drawing" if self.steps > 0 => format!("while drawing (step {} of {})", self.step, self.steps),
            "drawing" => "at the start of drawing".to_string(),
            "references" => "while reading the reference images".to_string(),
            "decoding" => "while turning the result into pixels (the VAE decode)".to_string(),
            _ => "while loading the model".to_string(),
        };
        let fix = match (self.kind, self.phase, self.mode) {
            ("ram", ..) => "Set Memory to Stream from SSD, or use a smaller file of the model.",
            (_, "decoding", _) => "sd.cpp already retried the decode in tiles; a smaller size is what fits.",
            (_, _, "auto") => "Set Memory to System RAM: the weights then wait in RAM and the whole card is free for the work.",
            _ => "Try a smaller size, Stream from SSD, or a smaller file of the model.",
        };
        write!(f, "sd-cli {} ran out of {what} {when}. {fix}\n{}", self.label, self.log)
    }
}

impl std::error::Error for OutOfMemory {}

/// The [`OutOfMemory`] in an error's chain, as JSON for the page — `None` for
/// every other failure.
pub fn oom_json(e: &anyhow::Error) -> Option<serde_json::Value> {
    e.chain()
        .find_map(|c| c.downcast_ref::<OutOfMemory>())
        .and_then(|o| serde_json::to_value(o).ok())
}

/// The bytes System RAM (`*=cpu,te=disk`) holds for a command: every weight
/// file it names except the text encoders, which that mode reads, uses and
/// releases. A bundled checkpoint (`-m`) carries its encoder inside, so it is
/// counted whole — an upper bound, which is the safe side to be wrong on.
/// `None` when the command names no weight file this can size.
fn ram_mode_bytes(args: &[String]) -> Option<u64> {
    const HELD: [&str; 6] = [
        "-m",
        "--diffusion-model",
        "--high-noise-diffusion-model",
        "--vae",
        "--audio-vae",
        "--clip_vision",
    ];
    let sizes: Vec<u64> = args
        .windows(2)
        .filter(|w| HELD.contains(&w[0].as_str()))
        .filter_map(|w| std::fs::metadata(&w[1]).ok().map(|m| m.len()))
        .collect();
    (!sizes.is_empty()).then(|| sizes.iter().sum())
}

/// sd.cpp's model-manager line, as `(need device, need budget, available
/// device, available budget)` in MB. Since f89d9b1 an available figure sd.cpp
/// couldn't measure is the word `unknown` (device) or `unlimited` (budget), and
/// the source location trails the message — so each figure is read by its
/// label, never by counting numbers, which would shift one into another's place.
///
/// ```text
/// [W] model manager cannot make enough memory available on Vulkan0: need 6337.42 MB device / 6151.92 MB budget, available 21786.55 MB device / 5759.43 MB budget - model_manager.cpp:1914
/// ```
fn parse_shortfall(line: &str) -> Option<(f64, f64, Option<f64>, Option<f64>)> {
    let rest = line.split_once("cannot make enough memory available")?.1;
    let (need, avail) = rest.split_once("need ")?.1.split_once(", available ")?;
    // "6337.42 MB device / 6151.92 MB budget" → the number before each label.
    let figures = |half: &str| -> (Option<f64>, Option<f64>) {
        let (dev, bud) = half.split_once(" / ").unwrap_or((half, ""));
        let first = |s: &str| s.split_whitespace().next().and_then(|w| w.parse::<f64>().ok());
        (first(dev), first(bud))
    };
    let (need_dev, need_bud) = figures(need);
    let (avail_dev, avail_bud) = figures(avail);
    Some((need_dev?, need_bud?, avail_dev, avail_bud))
}

/// The text encoders' share of the card, off sd.cpp's summary line:
///
/// ```text
/// total params memory size = 25150.24MB (VRAM 7669.77MB, RAM 17480.47MB): text_encoders 7669.77MB(VRAM), diffusion_model 17316.04MB(RAM), …
/// ```
fn parse_encoder_on_card(line: &str) -> Option<f64> {
    let rest = line.split_once("text_encoders ")?.1;
    let (mb, tail) = rest.split_once("MB(")?;
    tail.starts_with("VRAM").then(|| mb.trim().parse().ok()).flatten()
}

/// The discrete GPU's GTT as `(total, used)` in bytes, from amdgpu's sysfs —
/// the card with the most VRAM, so an integrated GPU beside it isn't the one
/// read. `None` where there is no amdgpu.
pub fn gtt() -> Option<(u64, u64)> {
    let read = |p: PathBuf| -> Option<u64> { std::fs::read_to_string(p).ok()?.trim().parse().ok() };
    std::fs::read_dir("/sys/class/drm")
        .ok()?
        .flatten()
        .map(|e| e.path().join("device"))
        .filter_map(|d| {
            let vram = read(d.join("mem_info_vram_total"))?;
            Some((vram, read(d.join("mem_info_gtt_total"))?, read(d.join("mem_info_gtt_used"))?))
        })
        .max_by_key(|(vram, ..)| *vram)
        .map(|(_, total, used)| (total, used))
}

/// The discrete GPU's VRAM in bytes, from amdgpu's sysfs — the card with the
/// most, as [`gtt`] picks it. `None` where there is no amdgpu (Windows).
pub fn vram() -> Option<u64> {
    let read = |p: PathBuf| -> Option<u64> { std::fs::read_to_string(p).ok()?.trim().parse().ok() };
    std::fs::read_dir("/sys/class/drm")
        .ok()?
        .flatten()
        .filter_map(|e| read(e.path().join("device").join("mem_info_vram_total")))
        .max()
}

/// `MemAvailable` from `/proc/meminfo`, in bytes — what sd.cpp's auto-fit
/// plans RAM with (`backend_fit.cpp`, `available_ram_bytes`). `None` where
/// there is no such file.
pub fn available_ram() -> Option<u64> {
    let info = std::fs::read_to_string("/proc/meminfo").ok()?;
    parse_mem_available(&info)
}

fn parse_mem_available(info: &str) -> Option<u64> {
    let line = info.lines().find(|l| l.starts_with("MemAvailable:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib * 1024)
}

/// auto-fit's decision, off the line `backend_fit.cpp` prints at INFO:
///
/// ```text
/// [INFO ] backend_fit.cpp:474 - auto-fit: --backend "diffusion=vulkan0,te=vulkan0" --params-backend "te=cpu"
/// ```
///
/// `Some(None)` is the line with no params part — everything on the card.
fn parse_auto_fit(line: &str) -> Option<Option<String>> {
    let rest = line.split_once("auto-fit: --backend ")?.1;
    Some(
        rest.split_once("--params-backend \"")
            .and_then(|(_, p)| p.split_once('"'))
            .map(|(p, _)| p.to_string()),
    )
}

/// A `--params-backend` spec in words, or `None` when it puts nothing off the
/// card. The default part (a bare value, or `*=`) is "everything" on its own
/// and "the rest" beside named parts, and comes last: `*=cpu,te=disk` is "text
/// encoder streamed from the SSD, the rest in system RAM".
pub fn describe_params(spec: &str) -> Option<String> {
    let place = |v: &str| match v {
        "cpu" => "in system RAM".to_string(),
        "disk" => "streamed from the SSD".to_string(),
        other => format!("on {other}"),
    };
    let mut named = Vec::new();
    let mut default = None;
    for a in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        match a.split_once('=') {
            Some((k, v)) => {
                let what = match k.to_ascii_lowercase().as_str() {
                    "*" | "all" | "default" => {
                        default = Some(place(v));
                        continue;
                    }
                    "diffusion" | "model" | "unet" | "dit" => "model".to_string(),
                    "te" | "clip" | "llm" | "t5xxl" | "cond" => "text encoder".to_string(),
                    "vae" | "tae" => "VAE".to_string(),
                    _ => k.to_string(),
                };
                named.push(format!("{what} {}", place(v)));
            }
            None => default = Some(place(a)),
        }
    }
    if let Some(d) = default {
        named.push(format!("{} {d}", if named.is_empty() { "everything" } else { "the rest" }));
    }
    (!named.is_empty()).then(|| named.join(", "))
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
    /// sd.cpp's byte-progress bar reports `MB/s`/`GB/s` while loading weights;
    /// the sampler reports `s/it`/`it/s`.
    fn of_rate(rate: &str) -> Phase {
        if rate.ends_with("B/s") {
            Phase::Loading
        } else {
            Phase::Sampling
        }
    }

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
    /// What step caching skipped in the last run, off sd-cli's own log line.
    /// Reset when a run begins, so it never describes the run before.
    cache_skip: Mutex<Option<(u32, u32)>>,
    /// Milliseconds the last run spent loading weights: the sum of sd.cpp's
    /// `loading tensors completed, taking X s` lines, which come once per
    /// part — and once per segment when a model streams. What a model kept
    /// loaded between runs would save, measured rather than guessed.
    load_ms: Mutex<u64>,
    /// Where the last run's time went, stage by stage — see [`Stages`].
    stages: Mutex<Stages>,
    /// Where the last diffusion run kept its weights, when anything was off the
    /// card — see [`describe_params`]. Set from the command's own
    /// `--params-backend`, or from auto-fit's plan line once sd-cli prints it.
    /// Only a run with a prompt sets it, so an ESRGAN enlarge after a generate
    /// leaves the generate's standing.
    placement: Mutex<Option<String>>,
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

    /// `(skipped, total)` steps the last run's step cache skipped, or `None`
    /// when it skipped none — off, too few steps, or a mode that didn't engage.
    pub fn cache_skip(&self) -> Option<(u32, u32)> {
        *self.0.cache_skip.lock().ok()?
    }

    /// Where the last diffusion run kept its weights, or `None` when they were
    /// all on the card.
    pub fn placement(&self) -> Option<String> {
        self.0.placement.lock().ok()?.clone()
    }

    fn set_placement(&self, p: Option<String>) {
        if let Ok(mut held) = self.0.placement.lock() {
            *held = p;
        }
    }

    /// Where the last run's time went, stage by stage.
    pub fn stages(&self) -> Stages {
        self.0.stages.lock().map(|s| *s).unwrap_or_default()
    }

    /// Milliseconds the last run spent loading weights; see `load_ms` above.
    pub fn load_ms(&self) -> u64 {
        self.0.load_ms.lock().map(|m| *m).unwrap_or(0)
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
        if let Ok(mut skip) = self.0.cache_skip.lock() {
            *skip = None;
        }
        if let Ok(mut ms) = self.0.load_ms.lock() {
            *ms = 0;
        }
        if let Ok(mut s) = self.0.stages.lock() {
            *s = Stages::default();
        }
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
                phase: Phase::of_rate(rate),
                step,
                steps,
                rate: rate.to_string(),
            });
        }
    }
}

/// sd-cli's `--cache-mode` for a profile, or `None` for no flag. `adaptive`
/// is one idea sd.cpp implements twice, once per architecture, and a mode on
/// the wrong one is a warning and an uncached run — see
/// [`crate::config::CACHE_MODES`].
fn cache_flag(p: &Profile) -> Option<&'static str> {
    match p.cache_mode.as_str() {
        "spectrum" => Some("spectrum"),
        "adaptive" if p.family.is_unet() => Some("ucache"),
        "adaptive" => Some("easycache"),
        _ => None,
    }
}

/// Where a run's time went, off sd.cpp's own per-stage lines. Each stage
/// includes loading its own weights — sd.cpp loads lazily, on first use — so
/// `Timings::load_ms` beside it is the share of all of them that was reading.
/// What it is for: the stage that dominates is the part worth quantising, or
/// caching, or giving a smaller reference size.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize)]
pub struct Stages {
    /// Encoding images into latents: references, or a pass's source image.
    pub encode_ms: u64,
    /// The text encoder reading the prompt.
    pub text_ms: u64,
    /// The diffusion steps — every sampling pass, summed.
    pub draw_ms: u64,
    /// The VAE turning the result into pixels.
    pub decode_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Stage {
    Encode,
    Text,
    Draw,
    Decode,
}

impl Stages {
    fn add(&mut self, stage: Stage, ms: u64) {
        match stage {
            Stage::Encode => self.encode_ms += ms,
            Stage::Text => self.text_ms += ms,
            Stage::Draw => self.draw_ms += ms,
            Stage::Decode => self.decode_ms += ms,
        }
    }
}

/// A stage and its milliseconds, off the lines `src/pipeline/image.cpp`
/// prints — "taking 1.90s" on the image side, "taking 812 ms" on the video
/// side — in either log format.
fn parse_stage(line: &str) -> Option<(Stage, u64)> {
    let stage = [
        ("encode_first_stage completed, taking ", Stage::Encode),
        ("get_learned_condition completed, taking ", Stage::Text),
        ("sampling completed, taking ", Stage::Draw),
        ("decode_first_stage completed, taking ", Stage::Decode),
    ];
    let (rest, stage) = stage
        .iter()
        .find_map(|(marker, s)| line.split_once(marker).map(|(_, r)| (r, *s)))?;
    let rest = rest.trim_start();
    let num_end = rest.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(rest.len());
    let n: f64 = rest[..num_end].parse().ok()?;
    let ms = if rest[num_end..].trim_start().starts_with("ms") { n } else { n * 1000.0 };
    Some((stage, ms.round() as u64))
}

/// Milliseconds out of sd.cpp's per-load line, in either log format:
///
/// ```text
/// [I] loading tensors completed, taking 1.71s (read: 1.28s, memcpy: 0.00s, …) - model_loader.cpp:1385
/// ```
fn parse_load_ms(line: &str) -> Option<u64> {
    let rest = line.split_once("loading tensors completed, taking ")?.1;
    let secs: f64 = rest.split_once('s')?.0.trim().parse().ok()?;
    Some((secs * 1000.0).round() as u64)
}

/// `(skipped, total)` out of the line every cache mode logs when it engaged:
///
/// ```text
/// [INFO ] sample-cache.cpp:318 - UCache skipped 9/30 steps (1.43x estimated speedup)
/// ```
///
/// That line is the only proof a mode did anything — one that didn't engage
/// says so in a warning, or not at all.
fn parse_cache_skip(line: &str) -> Option<(u32, u32)> {
    let rest = line.split_once(" skipped ")?.1;
    let (counts, tail) = rest.split_once(' ')?;
    if !tail.starts_with("steps") {
        return None;
    }
    let (skipped, total) = counts.split_once('/')?;
    Some((skipped.parse().ok()?, total.parse().ok()?))
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

    /// `base` joined with `rest` the way the code joins them — `\` on Windows,
    /// so an expected path is spelled the way the command will spell it.
    fn joined(base: &str, rest: &str) -> String {
        Path::new(base).join(rest).display().to_string()
    }

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
        assert_eq!(flag(&a, "--diffusion-model"), Some(joined("/models", "low.gguf").as_str()));
        assert_eq!(
            flag(&a, "--high-noise-diffusion-model"),
            Some(joined("/models", "high.gguf").as_str())
        );
        assert_eq!(
            flag(&a, "--vae"),
            Some(joined("/models", "wan_2.1_vae.safetensors").as_str())
        );
        assert_eq!(flag(&a, "--t5xxl"), Some(joined("/models", "umt5.gguf").as_str()));
        assert_eq!(flag(&a, "-p"), Some("a lovely cat"));
        assert_eq!(flag(&a, "-n"), Some("blurry"));
        assert_eq!(flag(&a, "-i"), Some("/work/first.png"));
        assert_eq!(flag(&a, "-s"), Some("42"));
        assert_eq!(flag(&a, "-o"), Some("/work/videos/1/frame_%03d.png"));
        assert_eq!(flag(&a, "--video-frames"), Some("33"));
        assert_eq!(flag(&a, "--fps"), Some("16"));
        assert_eq!(flag(&a, "--flow-shift"), Some("3"));
        // `ram`, spelled as what sd.cpp turns `--offload-to-cpu` into, and
        // never the alias as well.
        assert_eq!(flag(&a, "--params-backend"), Some("*=cpu,te=disk"));
        assert!(!a.contains(&"--offload-to-cpu".to_string()));
        assert_eq!(flag(&a, "--flash-attn"), Some("auto,diffusion=on"));
        assert!(!a.iter().any(|x| x == "--diffusion-fa"), "deprecated upstream");
        // On by default: the decode is where a clip runs out of memory.
        assert!(a.contains(&"--vae-tiling".to_string()));
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
        assert!(!a.iter().any(|x| x == "--diffusion-model" || x == "--flash-attn"));

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
        assert_eq!(a.iter().filter(|x| *x == "--flash-attn").count(), 1);
        assert_eq!(flag(&a, "--flash-attn"), Some("auto,diffusion=on"));
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

    /// `adaptive` is one idea sd.cpp implements once per architecture, and the
    /// wrong one is a warning and an uncached run — so the family decides.
    #[test]
    fn a_step_cache_names_the_mode_the_family_has() {
        let cfg: Config = toml::from_str("[sd]\nbinary = \"/opt/sd-cli\"\n").unwrap();
        let cmd = |family, mode: &str| {
            let p = Profile {
                model: PathBuf::from("/models/m.safetensors"),
                family,
                cache_mode: mode.into(),
                ..Default::default()
            };
            let a = argv(&SdRunner::new(&cfg).base_cmd(&p, "a cat", None, 1, Path::new("o.png"), 1.0));
            flag(&a, "--cache-mode").map(str::to_string)
        };
        use crate::config::ImageFamily::{Flux, Sdxl, ZImage};
        assert_eq!(cmd(Sdxl, "off"), None);
        assert_eq!(cmd(Sdxl, "spectrum").as_deref(), Some("spectrum"));
        assert_eq!(cmd(Flux, "spectrum").as_deref(), Some("spectrum"));
        assert_eq!(cmd(Sdxl, "adaptive").as_deref(), Some("ucache"));
        assert_eq!(cmd(ZImage, "adaptive").as_deref(), Some("easycache"));
    }

    /// Each stage line in both formats and both units; "hires sampling" and
    /// "generating … latent images" are not a second count of the drawing.
    #[test]
    fn the_stage_lines_are_read_and_summed() {
        let lines = [
            "[INFO   ] image.cpp:391  - encode_first_stage completed, taking 2.01s",
            "[I] get_learned_condition completed, taking 1.90s - image.cpp:529",
            "[I] sampling completed, taking 12.25s - image.cpp:899",
            "[I] generating 1 latent images completed, taking 12.30s - image.cpp:911",
            "[I] decode_first_stage completed, taking 2.10s - image.cpp:624",
            "[I] encode_first_stage completed, taking 812 ms - video.cpp:821",
        ];
        let mut s = Stages::default();
        for l in lines {
            if let Some((stage, ms)) = parse_stage(l) {
                s.add(stage, ms);
            }
        }
        assert_eq!(s, Stages { encode_ms: 2822, text_ms: 1900, draw_ms: 12250, decode_ms: 2100 });
        assert_eq!(parse_stage("[I] hires fix completed, taking 3.00s"), None);
    }

    /// The load line in both formats, and nothing that only looks like it.
    #[test]
    fn the_load_line_is_read_in_both_formats() {
        assert_eq!(parse_load_ms("[INFO   ] model_loader.cpp:1302 - loading tensors completed, taking 0.22s (read: 0.03s, memcpy: 0.00s, convert: 0.00s, copy_to_backend: 0.02s)"), Some(220));
        assert_eq!(parse_load_ms("[I] loading tensors completed, taking 1.71s (read: 1.28s, memcpy: 0.00s) - model_loader.cpp:1385"), Some(1710));
        assert_eq!(parse_load_ms("  |####| 4/4 - 1.85GB/s"), None);
        assert_eq!(parse_load_ms("[I] loading tensors from model.gguf"), None);
    }

    /// Every mode logs the same shape when it skipped something; that line is
    /// the only proof it engaged, so nothing else may be read as one.
    #[test]
    fn the_skip_line_is_read_and_nothing_else_is() {
        for line in [
            "[INFO ] sample-cache.cpp:318 - UCache skipped 9/30 steps (1.43x estimated speedup)",
            "[INFO ] sample-cache.cpp:299 - EasyCache skipped 9/30 steps (1.43x estimated speedup)",
            "[INFO ] sample-cache.cpp:353 - Spectrum skipped 9/30 steps (1.43x estimated speedup)",
            "[INFO ] sample-cache.cpp:323 - UCache skipped 9/30 steps",
            // f89d9b1's format: short level, location trailing.
            "[I] UCache skipped 9/30 steps (1.43x estimated speedup) - sample-cache.cpp:338",
            "[I] EasyCache skipped 9/30 steps - sample-cache.cpp:324",
        ] {
            assert_eq!(parse_cache_skip(line), Some((9, 30)), "{line}");
        }
        for line in [
            "  |=====>        | 3/20 - 1.23s/it",
            "[INFO ] sample-cache.cpp:309 - EasyCache completed without skipping steps",
            "[WARN ] sample-cache.cpp:75 - UCache requested but not supported for this model type",
            "[INFO ] a prompt that skipped 3/4 beats",
        ] {
            assert_eq!(parse_cache_skip(line), None, "{line}");
        }
    }

    /// The model manager's warning and ggml's allocation failures read as out
    /// of memory; an ordinary failure line does not.
    /// `auto` is the absence of a flag — any explicit `--params-backend` turns
    /// sd.cpp's auto-fit off — and the other two are one `--params-backend`
    /// each, never beside `--offload-to-cpu`. A placement written into
    /// `extra_args` is the more specific choice and suppresses ours.
    #[test]
    fn memory_modes_are_one_params_backend_or_none() {
        let cfg: Config = toml::from_str("[sd]\nbinary = \"/opt/sd-cli\"\n").unwrap();
        let cmd = |memory: &str, extra: &[&str]| {
            let p = Profile {
                model: PathBuf::from("/models/sdxl.safetensors"),
                memory: memory.into(),
                extra_args: extra.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            };
            argv(&SdRunner::new(&cfg).base_cmd(&p, "a cat", None, 1, Path::new("out.png"), 1.0))
        };
        let count = |a: &[String]| a.iter().filter(|x| *x == "--params-backend").count();
        let auto = cmd("auto", &[]);
        assert_eq!(count(&auto), 0);
        assert!(!auto.iter().any(|x| x == "--offload-to-cpu" || x == "--auto-fit"));
        let ram = cmd("ram", &[]);
        // Everything in RAM but the text encoder, which is loaded, used once and
        // released — held in RAM it would sit there through every step.
        assert_eq!((count(&ram), flag(&ram, "--params-backend")), (1, Some("*=cpu,te=disk")));
        let disk = cmd("disk", &[]);
        assert_eq!(flag(&disk, "--params-backend"), Some("diffusion=disk,te=disk"));
        assert_eq!(count(&disk), 1);
        let hand = cmd("ram", &["--params-backend", "te=cpu"]);
        assert_eq!((count(&hand), flag(&hand, "--params-backend")), (1, Some("te=cpu")));
        assert_eq!(count(&cmd("disk", &["--offload-to-cpu"])), 0);
        // Fast VAE decode is on by default, in the current spelling, and left
        // out when extra_args names it by hand in either spelling.
        let direct = |a: &[String]| {
            a.iter().filter(|x| *x == "--conv-direct" || *x == "--vae-conv-direct").count()
        };
        assert_eq!((direct(&auto), flag(&auto, "--conv-direct")), (1, Some("auto,vae=on")));
        assert_eq!(direct(&cmd("auto", &["--vae-conv-direct"])), 1);
        assert_eq!(flag(&cmd("auto", &["--conv-direct", "off"]), "--conv-direct"), Some("off"));
    }

    /// auto-fit's own line, in `backend_fit.cpp:474`'s format, read into words.
    #[test]
    fn the_auto_fit_line_says_where_the_weights_went() {
        let both = "[INFO ] backend_fit.cpp:474 - auto-fit: --backend \"diffusion=vulkan0,te=vulkan0,vae=vulkan0\" --params-backend \"te=cpu,vae=disk\"";
        assert_eq!(parse_auto_fit(both), Some(Some("te=cpu,vae=disk".into())));
        let fits = "[INFO ] backend_fit.cpp:474 - auto-fit: --backend \"diffusion=vulkan0,te=vulkan0\"";
        assert_eq!(parse_auto_fit(fits), Some(None));
        assert_eq!(parse_auto_fit("[INFO ] auto-fit plan:"), None);
        // f89d9b1's format, with the location after the message.
        assert_eq!(
            parse_auto_fit("[I] auto-fit: --backend \"diffusion=vulkan0,te=vulkan0\" --params-backend \"te=cpu\" - backend_fit.cpp:474"),
            Some(Some("te=cpu".into()))
        );
        assert_eq!(
            parse_auto_fit("[I] auto-fit: --backend \"diffusion=vulkan0\" - backend_fit.cpp:474"),
            Some(None)
        );
        assert_eq!(
            describe_params("te=cpu,vae=disk").as_deref(),
            Some("text encoder in system RAM, VAE streamed from the SSD")
        );
        assert_eq!(describe_params("cpu").as_deref(), Some("everything in system RAM"));
        assert_eq!(
            describe_params("*=cpu,te=disk").as_deref(),
            Some("text encoder streamed from the SSD, the rest in system RAM")
        );
        assert_eq!(
            describe_params("diffusion=disk,te=disk").as_deref(),
            Some("model streamed from the SSD, text encoder streamed from the SSD")
        );
        assert_eq!(describe_params("diffusion=vulkan1").as_deref(), Some("model on vulkan1"));
        assert_eq!(describe_params(""), None);
    }

    /// The whole path through a real child: sd.cpp-shaped progress, then the
    /// two ways a run dies of memory. The error is typed, carries when it
    /// happened and what the command asked for, and `oom_json` finds it.
    #[cfg(unix)]
    #[test]
    fn running_out_of_memory_is_a_typed_error_with_its_phase() {
        let cfg: Config = toml::from_str("[sd]\nbinary = \"/opt/sd-cli\"\n").unwrap();
        let runner = SdRunner::new(&cfg);
        let run = |script: &str, args: &[&str]| {
            let mut cmd = Command::new("sh");
            cmd.args(["-c", script, "sh"]).args(args);
            let e = runner.run_tail(cmd, "generate").unwrap_err();
            let oom = e.chain().find_map(|c| c.downcast_ref::<OutOfMemory>()).cloned();
            (e, oom)
        };
        // System memory: killed mid-sampling, with no message.
        let (e, oom) = run(
            "printf '  |=>  | 3/20 - 1.00s/it\\n'; kill -9 $$",
            &["-p", "x", "--params-backend", "*=cpu,te=disk"],
        );
        let oom = oom.expect("a SIGKILL is the OOM killer");
        assert_eq!((oom.kind, oom.phase, oom.step, oom.steps, oom.mode), ("ram", "drawing", 3, 20, "ram"));
        assert_eq!(oom.placement.as_deref(), Some("text encoder streamed from the SSD, the rest in system RAM"));
        // The SSD mode by where the model lives, and a hand-written spec as custom.
        let (_, oom) = run("kill -9 $$", &["-p", "x", "--params-backend", "diffusion=disk,te=disk"]);
        assert_eq!(oom.unwrap().mode, "disk");
        let (_, oom) = run("kill -9 $$", &["-p", "x", "--params-backend", "te=cpu"]);
        assert_eq!(oom.unwrap().mode, "custom");
        let json = oom_json(&e).unwrap();
        assert_eq!(json["kind"], "ram");
        assert!(json.get("log").is_none(), "the log rides in the message, not twice");
        // GPU memory after the last step: the decode.
        let (_, oom) = run(
            "printf '  |====| 20/20 - 1.00s/it\\n'; echo 'ggml_vulkan: Device memory allocation failed: ErrorOutOfDeviceMemory' >&2; exit 1",
            &["-p", "x"],
        );
        let oom = oom.unwrap();
        assert_eq!((oom.kind, oom.phase, oom.mode), ("gpu", "decoding", "auto"));
        assert!(oom.to_string().contains("VAE decode"));
        // Streamed segment by segment: byte-rate bars after the last step, then
        // sd.cpp's own sampling failure. That is drawing, not loading — the
        // run this was found on read "while loading the model".
        let (_, oom) = run(
            "printf '  |=>  | 1/4 - 1.00s/it\\n  |####| 4/4 - 1.85GB/s\\n'; \
             echo 'model manager cannot make enough memory available on Vulkan0' ; \
             echo '[ERROR  ] diffusion_engine.cpp:2532 - Diffusion model sampling failed' >&2; exit 1",
            &["-p", "x"],
        );
        let oom = oom.unwrap();
        assert_eq!((oom.phase, oom.step, oom.steps), ("drawing", 1, 4));
        let (_, oom) = run(
            "printf '  |####| 4/4 - 1.85GB/s\\n'; echo 'cannot make enough memory available'; \
             echo 'Diffusion model sampling failed' >&2; exit 1",
            &["-p", "x"],
        );
        assert!(oom.unwrap().to_string().contains("at the start of drawing"));
        let (_, oom) = run(
            "echo 'ErrorOutOfDeviceMemory'; echo 'failed to encode reference image 2' >&2; exit 1",
            &["-p", "x"],
        );
        assert_eq!(oom.unwrap().phase, "references");
        // The driver refusing a submission sd.cpp's own check let through: a
        // lost device, with the reserve the command ran with read back.
        let (_, oom) = run(
            "echo 'radv/amdgpu: Not enough memory for command submission.' >&2; \
             echo 'Diffusion model sampling failed' >&2; exit 134",
            &["-p", "x", "--max-vram", "-3"],
        );
        let oom = oom.unwrap();
        assert!(oom.lost);
        assert_eq!((oom.kind, oom.phase, oom.reserve_gb), ("gpu", "drawing", 3.0));
        // A budget starved by a reserve: the card had 21.8 GB, the budget 5.8.
        // The figures, the encoder left on the card and the references ride along.
        let (_, oom) = run(
            "echo '[INFO ] diffusion_engine.cpp:1237 - total params memory size = 25150.24MB (VRAM 7669.77MB, RAM 17480.47MB): text_encoders 7669.77MB(VRAM), diffusion_model 17316.04MB(RAM)'; \
             echo '[WARN ] model_manager.cpp:1762 - model manager cannot make enough memory available on Vulkan0: need 6337.42 MB device / 6151.92 MB budget, available 21786.55 MB device / 5759.43 MB budget'; \
             echo 'Diffusion model sampling failed' >&2; exit 1",
            &["-p", "x", "-r", "a.png", "-r", "b.png", "--max-vram", "-16"],
        );
        let oom = oom.unwrap();
        assert_eq!((oom.need_device_mb, oom.avail_device_mb), (Some(6337.42), Some(21786.55)));
        assert_eq!((oom.need_budget_mb, oom.avail_budget_mb), (Some(6151.92), Some(5759.43)));
        assert_eq!((oom.references, oom.reserve_gb, oom.encoder_on_card_mb), (2, 16.0, Some(7669.77)));
        assert_eq!(parse_encoder_on_card("text_encoders 7669.77MB(RAM), x"), None);
        // f89d9b1's format: short level, location trailing, and a figure sd.cpp
        // couldn't measure as a word — read by label, so nothing shifts.
        assert_eq!(
            parse_shortfall("[W] model manager cannot make enough memory available on Vulkan0: need 6337.42 MB device / 6151.92 MB budget, available 21786.55 MB device / 5759.43 MB budget - model_manager.cpp:1914"),
            Some((6337.42, 6151.92, Some(21786.55), Some(5759.43)))
        );
        assert_eq!(
            parse_shortfall("[W] model manager cannot make enough memory available on Vulkan0: need 1254.49 MB device / 6333.66 MB budget, available unknown device / unlimited budget - model_manager.cpp:1914"),
            Some((1254.49, 6333.66, None, None))
        );
        // Before any step: loading. And an ordinary crash is not one.
        let (_, oom) = run("echo 'cannot make enough memory available' ; exit 1", &[]);
        assert_eq!(oom.unwrap().phase, "loading");
        let (e, oom) = run("echo 'generate failed'; exit 1", &[]);
        assert!(oom.is_none() && oom_json(&e).is_none());
    }

    /// System RAM's figure counts the files it holds and not the text
    /// encoders it releases; MemAvailable is read in bytes.
    #[test]
    fn ram_needed_counts_held_weights_but_not_the_text_encoder() {
        let dir = std::env::temp_dir().join(format!("yasdig-ramneed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = |name: &str, len: usize| {
            let p = dir.join(name);
            std::fs::write(&p, vec![0u8; len]).unwrap();
            p.to_string_lossy().into_owned()
        };
        let (model, vae, llm) = (file("m.gguf", 1000), file("v.safetensors", 30), file("te.gguf", 500));
        let args: Vec<String> =
            ["-M", "img_gen", "--diffusion-model", &model, "--vae", &vae, "--llm", &llm, "-p", "x"]
                .iter()
                .map(|s| s.to_string())
                .collect();
        assert_eq!(ram_mode_bytes(&args), Some(1030));
        assert_eq!(ram_mode_bytes(&["-p".into(), "x".into()]), None);
        assert_eq!(
            parse_mem_available("MemTotal:  31000000 kB\nMemAvailable:   26000000 kB\n"),
            Some(26_000_000 * 1024)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn out_of_memory_is_recognised() {
        assert!(is_out_of_memory(
            "[WARN   ] model_manager.cpp:1762 - model manager cannot make enough memory available on Vulkan0: need 7441.11 MB"
        ));
        assert!(is_out_of_memory("ggml_vulkan: Device memory allocation failed: ErrorOutOfDeviceMemory"));
        assert!(is_out_of_memory("radv: Not enough memory for command submission."));
        assert!(!is_out_of_memory("[ERROR  ] main.cpp:965  - generate failed"));
    }

    /// References are `-r`, one each in order, and only the ones a command is
    /// handed: the img2img passes share `base_cmd`, and a refine must never
    /// pick up the generation's from the profile.
    #[test]
    fn a_turbo_schedule_reaches_the_generate_pass_only() {
        let cfg: Config = toml::from_str("[sd]\nbinary = \"/opt/sd-cli\"\n").unwrap();
        let p = Profile {
            model: PathBuf::from("/models/q21.gguf"),
            family: crate::config::ImageFamily::QwenImage21,
            sigmas: "qwen-image-2.1-turbo".into(),
            ..Default::default()
        };
        p.validate().unwrap();
        let r = SdRunner::new(&cfg);
        let a = argv(&r.txt2img_cmd(&p, "x", None, 1, Path::new("o.png"), &[], None));
        assert_eq!(flag(&a, "--sigmas"), Some("qwen-image-2.1-turbo"));
        let pass = argv(&r.img2img_cmd(&p, "x", None, 1, Path::new("i.png"), Path::new("o.png"), 0.3, 10, 1.0, false, "x", &[], None));
        assert!(!pass.iter().any(|x| x == "--sigmas"), "a strength-trimmed pass can't take a fixed list");
        let plain = Profile { sigmas: String::new(), ..p.clone() };
        assert!(!argv(&r.txt2img_cmd(&plain, "x", None, 1, Path::new("o.png"), &[], None)).iter().any(|x| x == "--sigmas"));
        // A preset is its own family's; anything else is refused.
        assert!(Profile { family: crate::config::ImageFamily::MossImage, ..p.clone() }.validate().is_err());
        assert!(Profile { sigmas: "1.0,0.5,0.0".into(), ..p }.validate().is_err());
    }

    #[test]
    fn a_pass_carries_only_the_references_it_is_handed() {
        let cfg: Config = toml::from_str("[sd]\nbinary = \"/opt/sd-cli\"\n").unwrap();
        let p = Profile {
            model: PathBuf::from("/models/klein.safetensors"),
            family: crate::config::ImageFamily::Flux2Klein4b,
            references: vec!["a".into(), "b".into()],
            ..Default::default()
        };
        let r = SdRunner::new(&cfg);
        let refs = [PathBuf::from("/s/ref1.png"), PathBuf::from("/s/ref2.png")];
        let a = argv(&r.txt2img_cmd(&p, "x", None, 1, Path::new("o.png"), &refs, Some(768)));
        let given: Vec<&str> = a.windows(2).filter(|w| w[0] == "-r").map(|w| w[1].as_str()).collect();
        assert_eq!(given, ["/s/ref1.png", "/s/ref2.png"]);
        // The reference size is sd.cpp's own cap, squared — its resize would
        // scale a smaller file back up — and only with references to cap.
        assert_eq!(flag(&a, "--ref-image-args"), Some("vae_input_max_pixels=589824"));
        let none = argv(&r.txt2img_cmd(&p, "x", None, 1, Path::new("o.png"), &[], Some(768)));
        assert!(!none.iter().any(|x| x == "-r" || x == "--ref-image-args"));
        // Edit passes none (its first reference is the image being changed),
        // 0 is sd.cpp's own default, and a hand-written one wins.
        for side in [None, Some(0)] {
            let a = argv(&r.txt2img_cmd(&p, "x", None, 1, Path::new("o.png"), &refs, side));
            assert!(!a.iter().any(|x| x == "--ref-image-args"), "{side:?}");
        }
        let hand = Profile { extra_args: vec!["--ref-image-args".into(), "preset=flux2".into()], ..p.clone() };
        let a = argv(&r.txt2img_cmd(&hand, "x", None, 1, Path::new("o.png"), &refs, Some(768)));
        assert_eq!(a.iter().filter(|x| *x == "--ref-image-args").count(), 1);
        let pass = argv(&r.img2img_cmd(&p, "x", None, 1, Path::new("i.png"), Path::new("o.png"), 0.3, 10, 1.0, false, "x", &[], None));
        assert!(!pass.iter().any(|x| x == "-r"), "the profile's references must not reach a pass");
        // A Refine asked to follow references gets them, capped the same way.
        let asked = argv(&r.img2img_cmd(
            &p, "x", None, 1, Path::new("i.png"), Path::new("o.png"), 0.3, 10, 1.0, false, "x", &refs[..1], Some(768),
        ));
        let given: Vec<&str> = asked.windows(2).filter(|w| w[0] == "-r").map(|w| w[1].as_str()).collect();
        assert_eq!(given, ["/s/ref1.png"]);
        assert_eq!(flag(&asked, "--ref-image-args"), Some("vae_input_max_pixels=589824"));
    }

    /// An Extend strip is a Fill pass at strength 1 on the Fill model's own
    /// steps and guidance: no schedule scaling, no step floor, the mask and the
    /// grown canvas, and never the references an ordinary txt2img would carry.
    #[test]
    fn a_fill_pass_is_full_strength_on_the_fill_models_own_settings() {
        let cfg: Config = toml::from_str("[sd]\nbinary = \"/opt/sd-cli\"\n").unwrap();
        let p = Profile {
            model: PathBuf::from("/models/flux1-fill-dev-Q8_0.gguf"),
            family: crate::config::ImageFamily::FluxFill,
            bundled: Some(false),
            steps: 28,
            cfg_scale: 1.0,
            references: vec!["a".into()],
            ..Default::default()
        };
        let a = argv(&SdRunner::new(&cfg).fill_cmd(&p, "a meadow", None, 7, Path::new("c.png"), Path::new("m.png"), Path::new("o.png")));
        assert_eq!(flag(&a, "--diffusion-model"), Some("/models/flux1-fill-dev-Q8_0.gguf"));
        assert_eq!(flag(&a, "--strength"), Some("1"));
        assert_eq!(flag(&a, "--steps"), Some("28"));
        assert_eq!(flag(&a, "--guidance"), Some("30"));
        assert_eq!(flag(&a, "--cfg-scale"), Some("1"));
        assert_eq!(flag(&a, "-i"), Some("c.png"));
        assert_eq!(flag(&a, "--mask"), Some("m.png"));
        assert!(!a.iter().any(|x| x == "-r"), "a fill pass never carries references");
    }

    /// Redraw is img2img plus `--mask` and nothing else — the step floor and
    /// schedule scaling are the same function's, so they can't drift apart.
    #[test]
    fn a_redraw_is_an_img2img_with_a_mask() {
        let cfg: Config = toml::from_str("[sd]\nbinary = \"/opt/sd-cli\"\n").unwrap();
        let p = Profile { model: PathBuf::from("/models/m.safetensors"), ..Default::default() };
        let r = SdRunner::new(&cfg);
        let base = argv(&r.img2img_cmd(&p, "a hand", None, 7, Path::new("i.png"), Path::new("o.png"), 0.8, 20, 6.0, false, "x", &[], None));
        let mut masked = r.img2img_cmd(&p, "a hand", None, 7, Path::new("i.png"), Path::new("o.png"), 0.8, 20, 6.0, false, "x", &[], None);
        masked.args(["--mask", "m.png"]);
        let masked = argv(&masked);
        assert_eq!(&masked[..base.len()], &base[..]);
        assert_eq!(flag(&masked, "--mask"), Some("m.png"));
        assert!(!base.iter().any(|a| a == "--mask"));
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
        assert_eq!(flag(&a, "--lora-model-dir"), Some(joined("/models", "loras").as_str()));
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
        let dir = std::env::temp_dir().join(format!("yasdig-preview-{}", std::process::id()));
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
        assert_eq!(
            flag(&a, "--preview-path"),
            Some(joined("/work/current/preview", "p-%04d.png").as_str())
        );
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
