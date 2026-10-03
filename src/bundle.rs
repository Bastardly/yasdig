//! The stable-diffusion.cpp binary this executable carries, and getting it onto
//! disk where it can be run.
//!
//! sd.cpp is still a subprocess and never FFI — the bytes only travel inside
//! us. A release is built with `RUSTIMAGE_SD_CLI` set (see `build.rs`), and on
//! start [`ensure`] writes them to `bin/sd-cli` beside `models.toml`, where
//! [`crate::config::Config::sd_binary`] finds them.
//!
//! **It is written once per build, not once per start.** `bin/sd-cli.version`
//! holds the hash `build.rs` computed; a start that finds it matching touches
//! nothing, so the 93 MB write is paid on first run and after an upgrade, and
//! never otherwise. Comparing the stamp rather than the file's own bytes is
//! deliberate: hashing 93 MB on every start would cost what the stamp saves.
//! The cost is that a hand-replaced `bin/sd-cli` with an untouched stamp is
//! trusted — and someone doing that wants `[sd].binary` instead.

use anyhow::Result;
#[cfg(bundled_sd)]
use anyhow::Context;
use std::path::{Path, PathBuf};

const NAME: &str = if cfg!(windows) { "sd-cli.exe" } else { "sd-cli" };

#[cfg(bundled_sd)]
const BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/sd-cli"));
#[cfg(bundled_sd)]
const HASH: &str = include_str!(concat!(env!("OUT_DIR"), "/sd-cli.hash"));

/// Whether this build carries an sd-cli at all. `doctor` says which build it
/// is looking at, because "binary not found" means something different for each.
pub const BUNDLED: bool = cfg!(bundled_sd);

/// Where the bundled binary lives under `base`, whether or not it is there yet.
pub fn path(base: &Path) -> PathBuf {
    base.join("bin").join(NAME)
}

/// Write the bundled binary to [`path`] unless the stamp says it already is.
/// A no-op in a build that carries none, so a checkout with no `[sd].binary`
/// gets `doctor`'s "not found" with the build instructions, not an error here.
#[cfg(bundled_sd)]
pub fn ensure(base: &Path) -> Result<()> {
    let bin = path(base);
    let stamp = bin.with_file_name("sd-cli.version");
    if bin.is_file() && std::fs::read_to_string(&stamp).is_ok_and(|s| s.trim() == HASH) {
        return Ok(());
    }
    let dir = bin.parent().unwrap();
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    println!("[rustimage] unpacking stable-diffusion.cpp to {}", bin.display());

    // Through a temp name and a rename, so a start that dies halfway (a full
    // disk, a closed console) leaves the old binary or none, never a truncated
    // one that the stamp would then vouch for. The stamp goes last for the
    // same reason.
    let tmp = bin.with_file_name(format!("{NAME}.part"));
    std::fs::write(&tmp, BYTES).with_context(|| format!("writing {}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&tmp, &bin).with_context(|| {
        format!(
            "replacing {} — is another copy of rustimage, or a render, still running?",
            bin.display()
        )
    })?;
    std::fs::write(&stamp, HASH).with_context(|| format!("writing {}", stamp.display()))?;
    Ok(())
}

#[cfg(not(bundled_sd))]
pub fn ensure(_base: &Path) -> Result<()> {
    Ok(())
}
