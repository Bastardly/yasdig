//! Where yasdig keeps its files, and making that place on a first run.
//!
//! A release is one file someone downloads and double-clicks, so the folders it
//! needs cannot be a step in a README: they are created here, next to the
//! executable, the first time it starts — and only what is missing, so a second
//! start, an upgrade dropped over the old file, or a folder the person
//! rearranged by hand are all left exactly as they are.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

const CONFIG: &str = "models.toml";
const TEMPLATE: &str = include_str!("default-models.toml");

/// The folders under `models/`, each with the sentence its `README.txt` says.
///
/// ComfyUI's role layout, because it is what every download page assumes. The
/// scan is recursive and reads each file's own tensors, so these are for the
/// person and not for the program: a checkpoint dropped into the wrong one is
/// found just the same. `loras`, `upscalers` and `video` are the exceptions —
/// each is a listing of its own and is left out of the image scan.
const MODEL_DIRS: &[(&str, &str)] = &[
    (
        "checkpoints",
        "Image models that are one file: SD 1.5, SDXL, Turbo, Lightning and the like.\n\
         Drop a .safetensors or .gguf here and reload the page.",
    ),
    (
        "diffusion_models",
        "Image models that come as separate parts (Flux, Z-Image, ...): the main file goes here,\n\
         its VAE into ../vae and its text encoders into ../text_encoders.",
    ),
    ("vae", "VAEs, for models whose VAE is a separate file, or to override a checkpoint's own."),
    ("text_encoders", "Text encoders (CLIP, T5, Qwen) for models that come as separate parts."),
    ("loras", "LoRAs. Subfolders are fine. Each one only works with the model family it was trained on."),
    ("upscalers", "Upscaler models (ESRGAN, SPAN, Compact, SwinIR) for the Output upscale step."),
    ("video", "Video models (Wan 2.x, a nightly feature): transformer, VAE and text encoder."),
];

/// The `models.toml` to start from.
///
/// An explicit path wins, then a `models.toml` in the working directory — what
/// `cargo run` from the repo and every existing install rely on — and otherwise
/// the one beside the executable, created if it isn't there. That last rule is
/// the double-click: a file manager starts a program in `$HOME` on Linux and
/// sometimes in `System32` on Windows, so the working directory says nothing
/// about where the person put the file.
pub fn config_path(explicit: Option<&str>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(PathBuf::from(p));
    }
    let here = PathBuf::from(CONFIG);
    if here.is_file() {
        return Ok(here);
    }
    let exe = std::env::current_exe().context("finding this executable's folder")?;
    // The canonical path, so a symlink to the executable makes its folders
    // where the real file is rather than beside the link.
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    // A build still inside this checkout's `target/` belongs to the checkout,
    // whatever directory it was started from — an IDE's run button or
    // `cargo run` from a subfolder. Without this the exe-folder rule below laid
    // out a fresh, empty home in `target/debug/` and the repo's models vanished.
    // A release copied anywhere else no longer sits under this path.
    let checkout = Path::new(env!("CARGO_MANIFEST_DIR"));
    if exe.starts_with(checkout.join("target")) && checkout.join(CONFIG).is_file() {
        return Ok(checkout.join(CONFIG));
    }
    let root = exe.parent().context("this executable has no folder")?.to_path_buf();
    ensure(&root)?;
    Ok(root.join(CONFIG))
}

/// Create whatever of the layout is missing under `root`. Never overwrites.
pub fn ensure(root: &Path) -> Result<()> {
    let config = root.join(CONFIG);
    if !config.exists() {
        std::fs::write(&config, TEMPLATE).with_context(|| {
            format!(
                "can't create {} — yasdig keeps its models and images in the folder it is in, \
                 so it needs to be somewhere you can write to (not Program Files). Move it to a \
                 folder of its own, such as your Documents or home folder, and start it again",
                config.display()
            )
        })?;
        println!("[yasdig] first run: set up in {}", root.display());
    }
    for (dir, about) in MODEL_DIRS {
        let d = root.join("models").join(dir);
        if !d.exists() {
            std::fs::create_dir_all(&d).with_context(|| format!("creating {}", d.display()))?;
            // Only beside a folder we just made: a README appearing in a folder
            // the person already filled would be noise they didn't ask for.
            std::fs::write(d.join("README.txt"), format!("{about}\n"))
                .with_context(|| format!("writing into {}", d.display()))?;
        }
    }
    for dir in ["user", "work"] {
        let d = root.join(dir);
        std::fs::create_dir_all(&d).with_context(|| format!("creating {}", d.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The promise a re-run makes: a second start, or an upgrade dropped over
    /// the old executable, changes nothing the person has touched.
    #[test]
    fn a_second_run_leaves_everything_alone() {
        let root = std::env::temp_dir().join(format!("yasdig-home-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        ensure(&root).unwrap();
        assert!(root.join("models/checkpoints/README.txt").is_file());
        assert!(root.join("user").is_dir() && root.join("work").is_dir());

        std::fs::write(root.join(CONFIG), "# mine\n").unwrap();
        std::fs::remove_file(root.join("models/loras/README.txt")).unwrap();
        ensure(&root).unwrap();
        assert_eq!(std::fs::read_to_string(root.join(CONFIG)).unwrap(), "# mine\n");
        assert!(!root.join("models/loras/README.txt").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The template is what a first run loads, so it has to parse — and with no
    /// `binary`, which is what makes the bundled sd-cli the one that runs.
    #[test]
    fn the_shipped_config_parses_and_names_no_binary() {
        let cfg: crate::config::Config = toml::from_str(TEMPLATE).unwrap();
        assert!(cfg.sd.binary.is_none());
        assert!(cfg.legacy_server.is_none(), "the port lives in startup.toml now");
    }
}
