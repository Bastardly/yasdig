//! Thin wrapper around the stable-diffusion.cpp CLI (`sd-cli`). We shell out to
//! the native binary per request — clean process isolation, easy to debug, and
//! it keeps this crate Python-free. All flags are derived from a resolved
//! [`Profile`], with `weight_type`/`extra_args` escape hatches so CLI changes
//! need only a config edit.

use crate::config::{Config, Profile};
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
        let mut cmd = self.base_cmd(p, prompt, negative, seed, out);
        cmd.args(["-W", &p.width.to_string()]);
        cmd.args(["-H", &p.height.to_string()]);
        cmd.args(["--steps", &p.steps.to_string()]);
        self.run(cmd, "txt2img")
    }

    /// image-to-image low-denoise refine (the "High quality" toggle). Reads
    /// `init`, writes `out`, using the profile's refine_* params.
    pub fn img2img_refine(
        &self,
        p: &Profile,
        prompt: &str,
        negative: Option<&str>,
        seed: i64,
        init: &Path,
        out: &Path,
    ) -> Result<()> {
        let mut cmd = self.base_cmd(p, prompt, negative, seed, out);
        // sd.cpp derives dimensions from the init image, so no -W/-H here.
        cmd.args(["-i", &path_str(init)]);
        cmd.args(["--strength", &format!("{}", p.refine_strength)]);
        cmd.args(["--steps", &p.refine_steps.max(1).to_string()]);
        self.run(cmd, "img2img refine")
    }

    /// Flags common to both modes.
    fn base_cmd(
        &self,
        p: &Profile,
        prompt: &str,
        negative: Option<&str>,
        seed: i64,
        out: &Path,
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
        cmd.args(["--cfg-scale", &format!("{}", p.cfg_scale)]);
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
