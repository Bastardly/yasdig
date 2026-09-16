//! Thin wrapper around the stable-diffusion.cpp CLI (`sd-cli`). We shell out to
//! the native binary per request — clean process isolation, easy to debug, and
//! it keeps this crate Python-free. All flags are derived from a resolved
//! [`Profile`], with `weight_type`/`extra_args` escape hatches so CLI changes
//! need only a config edit.

use crate::config::{Config, Profile, MIN_STRENGTH};
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct SdRunner<'a> {
    cfg: &'a Config,
}

impl<'a> SdRunner<'a> {
    pub fn new(cfg: &'a Config) -> Self {
        SdRunner { cfg }
    }

    fn binary(&self) -> PathBuf {
        self.cfg.resolve(&self.cfg.sd.binary)
    }

    /// Verify the sd binary exists; returns a friendly error if not.
    pub fn preflight(&self) -> Result<()> {
        let bin = self.binary();
        if !bin.exists() {
            bail!(
                "sd binary not found at {} — build stable-diffusion.cpp \
                 (cmake -B build -DSD_VULKAN=ON && cmake --build build) and set \
                 [sd].binary in models.toml",
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
        big_image: bool,
        label: &str,
    ) -> Result<()> {
        // The base render's CFG over-guides at output resolution: the pass has
        // little freedom left to begin with, and pushing it hard against the
        // prompt spends that freedom on high-frequency artifacts — the same
        // crunch the step floor above attacks from the other side. `img2img_cfg`
        // is the knob for that, and follows `cfg_scale` unless someone set it.
        let cfg = p.img2img_cfg();
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
            if p.hires_flash_attn {
                cmd.arg("--diffusion-fa");
            }
        }
        self.run(cmd, label)
    }

    /// Flags common to both modes.
    ///
    /// `cfg_scale` is passed in rather than read off the profile, because the two
    /// modes want different ones — see [`Profile::img2img_cfg`]. Passing the
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
        cmd.args(["-m", &path_str(&p.model)]);
        cmd.args(["-p", prompt]);
        if let Some(neg) = negative {
            if !neg.is_empty() {
                cmd.args(["-n", neg]);
            }
        }
        cmd.args(["--cfg-scale", &format!("{cfg_scale}")]);
        cmd.args(["--sampling-method", &p.sampler]);
        cmd.args(["-s", &seed.to_string()]);
        cmd.args(["-o", &path_str(out)]);
        if let Some(wt) = &p.weight_type {
            cmd.args(["--type", wt]);
        }
        for a in &p.extra_args {
            cmd.arg(a);
        }
        cmd
    }

    fn run(&self, mut cmd: Command, label: &str) -> Result<()> {
        let output = cmd
            .output()
            .with_context(|| format!("failed to launch sd-cli for {label}"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            bail!(
                "sd-cli {label} exited with {}\n--- stderr ---\n{}\n--- stdout ---\n{}",
                output.status,
                stderr.trim(),
                stdout.trim()
            );
        }
        Ok(())
    }
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().to_string()
}
