//! Builds stable-diffusion.cpp's `sd-cli`, and bakes it into a release build.
//!
//! The point is that `cargo build --release` is the whole recipe. With a
//! `stable-diffusion.cpp/` checkout in the repo (gitignored — see README.md),
//! this script configures and builds its `sd-cli` target into
//! `stable-diffusion.cpp/build-yasdig/`, and a **release** build then carries
//! the bytes (`cfg(bundled_sd)`, read by `src/bundle.rs`).
//!
//! A debug build builds sd-cli too, so a fresh clone's `cargo run` has one to
//! run (the repo's `models.toml` points `[sd].binary` at it), but never embeds
//! it: 93 MB in every test binary is link time paid on every `cargo test` for
//! nothing. For the same reason a failed sd.cpp build is a warning in debug —
//! the Rust half still builds and tests — and an error in release, where an
//! executable without sd-cli inside is not the thing that was asked for.
//!
//! `YASDIG_SD_CLI=<path>` still overrides all of this with a binary built
//! elsewhere, and is then embedded whatever the profile.
//!
//! The cmake flags are the release's, on purpose, so a local release build is
//! the file CI would make: `GGML_NATIVE=OFF` (a native build dies with "illegal
//! instruction" on an older CPU), `GGML_OPENMP=OFF` (libgomp/vcomp140 aren't on
//! every system), and on Windows a static C++ runtime (no VC++ redistributable).
//! Linux's static libstdc++ is CI's business, through `LDFLAGS`: Fedora doesn't
//! ship the static library, and a binary built on a new distro needs that
//! distro's glibc anyway, so portability comes from CI's older Ubuntu. Its own `build-yasdig/` directory
//! rather than `build/`, so a native build made by hand next to it is left alone.

use std::path::{Path, PathBuf};
use std::process::Command;

const SD_DIR: &str = "stable-diffusion.cpp";
const BUILD_DIR: &str = "build-yasdig";

fn main() {
    println!("cargo::rustc-check-cfg=cfg(bundled_sd)");
    println!("cargo::rerun-if-env-changed=YASDIG_SD_CLI");

    let release = std::env::var("PROFILE").as_deref() == Ok("release");
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());

    let (cli, embed) = if let Some(p) = std::env::var_os("YASDIG_SD_CLI") {
        (PathBuf::from(p), true)
    } else {
        let src = manifest.join(SD_DIR);
        // Rerun when the checkout moves to another commit — `logs/HEAD` gets a
        // line on every checkout, pull and commit — or appears at all. Not on
        // the source tree: walking 1 GB of ggml per `cargo build` is the cost
        // this exists to avoid. Edit sd.cpp itself and `touch build.rs`.
        println!("cargo::rerun-if-changed={}", src.join(".git/logs/HEAD").display());
        println!("cargo::rerun-if-changed={}", src.join("CMakeLists.txt").display());
        if !src.join("CMakeLists.txt").is_file() {
            if release {
                println!(
                    "cargo::warning=no {SD_DIR}/ in the repo, so this release has no sd-cli inside \
                     it and will need [sd].binary. README.md, \"Building from source\", has the clone."
                );
            }
            return;
        }
        // cmake here would build for the machine running cargo, not for the
        // target — a Linux sd-cli inside a Windows exe. A cross build (in
        // practice `cargo check --target x86_64-pc-windows-gnu`, the cheap
        // Windows gate) is for the Rust half; the real one is CI's, on Windows.
        if std::env::var("TARGET") != std::env::var("HOST") {
            println!("cargo::warning=cross build: sd-cli not built or embedded");
            return;
        }
        match build_sd_cli(&src) {
            Ok(cli) => (cli, release),
            Err(e) if release => panic!("{e}"),
            Err(e) => {
                println!("cargo::warning={}", e.replace('\n', " "));
                return;
            }
        }
    };
    println!("cargo::rerun-if-changed={}", cli.display());
    if embed {
        embed_sd_cli(&cli);
    }
}

/// Configure once, then build the one target. A no-op build is a couple of
/// seconds; the first is several minutes, most of it ggml-vulkan's shaders.
fn build_sd_cli(src: &Path) -> Result<PathBuf, String> {
    let build = src.join(BUILD_DIR);
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    let windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");

    // A stamp written after a configure *succeeds*, not the cache: a failed
    // configure leaves a CMakeCache.txt behind, and trusting it turned one
    // missing package into a build that failed with "no Makefile" forever.
    let configured = build.join(".yasdig-configured");
    if !configured.is_file() {
        let mut cfg = Command::new("cmake");
        cfg.arg("-S").arg(src).arg("-B").arg(&build).args([
            "-DCMAKE_BUILD_TYPE=Release",
            "-DSD_VULKAN=ON",
            "-DGGML_NATIVE=OFF",
            "-DGGML_OPENMP=OFF",
            "-DSD_SERVER_BUILD_FRONTEND=OFF",
        ]);
        if msvc {
            cfg.args([
                "-DCMAKE_POLICY_DEFAULT_CMP0091=NEW",
                "-DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded",
                "-DCMAKE_CXX_FLAGS=/EHsc /bigobj",
            ]);
        }
        run(cfg, "configuring")?;
        std::fs::write(&configured, "").map_err(|e| e.to_string())?;
    }

    let jobs = std::thread::available_parallelism().map_or(4, |n| n.get());
    let mut b = Command::new("cmake");
    b.arg("--build").arg(&build).args(["--config", "Release", "--target", "sd-cli"]);
    b.arg("--parallel").arg(jobs.to_string());
    run(b, "building")?;

    // Single-config generators (Makefiles, Ninja) put it in bin/; Visual
    // Studio's multi-config one in bin/Release/.
    let exe = if windows { "sd-cli.exe" } else { "sd-cli" };
    [build.join("bin").join(exe), build.join("bin/Release").join(exe)]
        .into_iter()
        .find(|p| p.is_file())
        .ok_or_else(|| format!("sd.cpp built, but no {exe} under {}", build.display()))
}

fn run(mut cmd: Command, what: &str) -> Result<(), String> {
    match cmd.status() {
        Ok(s) if s.success() => Ok(()),
        Ok(_) => Err(format!(
            "{what} stable-diffusion.cpp failed — the cmake output is above. It needs cmake, \
             a C++ compiler, glslc and the Vulkan headers; README.md, \"Building from \
             source\", lists the packages"
        )),
        Err(e) => Err(format!(
            "{what} stable-diffusion.cpp: can't run cmake ({e}). Install cmake, glslc and the \
             Vulkan headers — README.md, \"Building from source\", lists the packages"
        )),
    }
}

/// Copy the binary into `OUT_DIR` with a hash beside it. FNV-1a, written here
/// rather than pulled in as a crate: it only has to tell one build of sd-cli
/// from another, so a start can skip rewriting 93 MB that `bin/` already holds.
fn embed_sd_cli(cli: &Path) {
    let bytes = std::fs::read(cli).unwrap_or_else(|e| panic!("reading {}: {e}", cli.display()));
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    std::fs::write(out.join("sd-cli"), &bytes).unwrap();

    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in &bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    std::fs::write(out.join("sd-cli.hash"), format!("{h:016x}-{}", bytes.len())).unwrap();
    println!("cargo::rustc-cfg=bundled_sd");
}
