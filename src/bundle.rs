//! The native binaries this executable carries — stable-diffusion.cpp's
//! `sd-cli`, and the agent's llama.cpp driver `yasdig-llm` — and getting them
//! onto disk where they can be run.
//!
//! Both are still subprocesses and never FFI — the bytes only travel inside
//! us. A release is built with the clones beside it (see `build.rs`), and on
//! start [`ensure`] writes each to `bin/<name>` beside `models.toml`, where
//! [`crate::config::Config::sd_binary`] and [`crate::config::Config::llm_binary`]
//! find them.
//!
//! **Each is written once per build, not once per start.** `bin/<name>.version`
//! holds the hash `build.rs` computed; a start that finds it matching touches
//! nothing, so the 93 MB write is paid on first run and after an upgrade, and
//! never otherwise. Comparing the stamp rather than the file's own bytes is
//! deliberate: hashing 93 MB on every start would cost what the stamp saves.
//! The cost is that a hand-replaced `bin/sd-cli` with an untouched stamp is
//! trusted — and someone doing that wants `[sd].binary` instead.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// One binary: its file name under `bin/`, and the bytes and hash when this
/// build carries it.
pub struct Bundled {
    /// The name without `.exe` — the stamp is `<stem>.version`.
    stem: &'static str,
    /// What the unpack line calls it.
    what: &'static str,
    carried: Option<(&'static [u8], &'static str)>,
}

pub const SD: Bundled = Bundled {
    stem: "sd-cli",
    what: "stable-diffusion.cpp",
    #[cfg(bundled_sd)]
    carried: Some((
        include_bytes!(concat!(env!("OUT_DIR"), "/sd-cli")),
        include_str!(concat!(env!("OUT_DIR"), "/sd-cli.hash")),
    )),
    #[cfg(not(bundled_sd))]
    carried: None,
};

pub const LLM: Bundled = Bundled {
    stem: "yasdig-llm",
    what: "the agent's llama.cpp driver",
    #[cfg(bundled_llm)]
    carried: Some((
        include_bytes!(concat!(env!("OUT_DIR"), "/yasdig-llm")),
        include_str!(concat!(env!("OUT_DIR"), "/yasdig-llm.hash")),
    )),
    #[cfg(not(bundled_llm))]
    carried: None,
};

impl Bundled {
    /// Whether this build carries the binary at all. `doctor` says which build
    /// it is looking at, because "binary not found" means something different
    /// for each.
    pub const fn bundled(&self) -> bool {
        self.carried.is_some()
    }

    fn name(&self) -> String {
        if cfg!(windows) {
            format!("{}.exe", self.stem)
        } else {
            self.stem.to_string()
        }
    }

    /// Where the bundled binary lives under `base`, whether or not it is there yet.
    pub fn path(&self, base: &Path) -> PathBuf {
        base.join("bin").join(self.name())
    }

    /// Write the bundled binary to [`Bundled::path`] unless the stamp says it
    /// already is. A no-op in a build that carries none, so a checkout with no
    /// `[sd].binary` gets `doctor`'s "not found" with the build instructions,
    /// not an error here.
    pub fn ensure(&self, base: &Path) -> Result<()> {
        let Some((bytes, hash)) = self.carried else {
            return Ok(());
        };
        let bin = self.path(base);
        let stamp = bin.with_file_name(format!("{}.version", self.stem));
        if bin.is_file() && std::fs::read_to_string(&stamp).is_ok_and(|s| s.trim() == hash) {
            return Ok(());
        }
        let dir = bin.parent().unwrap();
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        println!("[yasdig] unpacking {} to {}", self.what, bin.display());

        // Through a temp name and a rename, so a start that dies halfway (a full
        // disk, a closed console) leaves the old binary or none, never a truncated
        // one that the stamp would then vouch for. The stamp goes last for the
        // same reason.
        let tmp = bin.with_file_name(format!("{}.part", self.name()));
        std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
        }
        std::fs::rename(&tmp, &bin).with_context(|| {
            format!(
                "replacing {} — is another copy of yasdig, or a render, still running?",
                bin.display()
            )
        })?;
        std::fs::write(&stamp, hash).with_context(|| format!("writing {}", stamp.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_binary_has_its_own_name_and_stamp_under_bin() {
        let base = Path::new("/x");
        assert_ne!(SD.path(base), LLM.path(base));
        assert!(SD.path(base).starts_with("/x/bin"));
        assert!(LLM.path(base).ends_with(if cfg!(windows) { "yasdig-llm.exe" } else { "yasdig-llm" }));
    }
}
