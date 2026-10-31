//! Builds the two native binaries yasdig runs — stable-diffusion.cpp's `sd-cli`
//! and the agent's llama.cpp driver `yasdig-llm` — and bakes them into a
//! release build.
//!
//! The point is that `cargo build --release` is the whole recipe. With a
//! `stable-diffusion.cpp/` checkout in the repo (gitignored — see docs/building.md),
//! this script configures and builds its `sd-cli` target into
//! `stable-diffusion.cpp/build-yasdig/`, and a **release** build then carries
//! the bytes (`cfg(bundled_sd)`, read by `src/bundle.rs`). The same goes for a
//! `llama.cpp/` checkout: the CMake project configured is our own `llm/`, which
//! pulls llama.cpp in as a library, into `llama.cpp/build-yasdig/`
//! (`cfg(bundled_llm)`).
//!
//! A debug build builds both too, so a fresh clone's `cargo run` has them to
//! run (the repo's `models.toml` points `[sd].binary` and `[llm].binary` at
//! them), but never embeds them: 93 MB in every test binary is link time paid on
//! every `cargo test` for nothing. For the same reason a failed native build is
//! a warning in debug — the Rust half still builds and tests — and an error in
//! release, where an executable without it inside is not the thing that was
//! asked for. A *missing* llama.cpp checkout is a warning even in release: the
//! agent is optional, and a release without it says so on its Agent tab.
//!
//! `YASDIG_SD_CLI=<path>` / `YASDIG_LLM=<path>` still override all of this with
//! a binary built elsewhere, which is then embedded whatever the profile.
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

const BUILD_DIR: &str = "build-yasdig";

/// One native binary and how to make it.
struct Native {
    /// The binary's (and its cmake target's) name, and the file in `OUT_DIR`.
    name: &'static str,
    /// What messages call the project.
    what: &'static str,
    /// The checkout, relative to the manifest — rerun on its `.git/logs/HEAD`.
    clone: &'static str,
    /// The CMake source directory, relative to the manifest.
    cmake_src: &'static str,
    /// Our own sources beside the checkout, rerun on when they change.
    own: &'static [&'static str],
    env: &'static str,
    cfg: &'static str,
    flags: &'static [&'static str],
    /// A missing checkout fails a release (sd-cli) or only warns (the agent).
    required: bool,
}

const BUILDS: [Native; 2] = [
    Native {
        name: "sd-cli",
        what: "stable-diffusion.cpp",
        clone: "stable-diffusion.cpp",
        cmake_src: "stable-diffusion.cpp",
        own: &[],
        env: "YASDIG_SD_CLI",
        cfg: "bundled_sd",
        flags: &["-DSD_VULKAN=ON", "-DSD_SERVER_BUILD_FRONTEND=OFF"],
        required: true,
    },
    Native {
        name: "yasdig-llm",
        what: "llama.cpp",
        clone: "llama.cpp",
        cmake_src: "llm",
        own: &["llm/CMakeLists.txt", "llm/yasdig-llm.cpp"],
        env: "YASDIG_LLM",
        cfg: "bundled_llm",
        // llm/CMakeLists.txt switches llama.cpp's server, tools and TLS off.
        flags: &["-DGGML_VULKAN=ON"],
        required: false,
    },
];

fn main() {
    let release = std::env::var("PROFILE").as_deref() == Ok("release");
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    for n in &BUILDS {
        native(n, &manifest, release);
    }
}

fn native(n: &Native, manifest: &Path, release: bool) {
    println!("cargo::rustc-check-cfg=cfg({})", n.cfg);
    println!("cargo::rerun-if-env-changed={}", n.env);

    let (bin, embed) = if let Some(p) = std::env::var_os(n.env) {
        (PathBuf::from(p), true)
    } else {
        let clone = manifest.join(n.clone);
        // Rerun when the checkout moves to another commit — `logs/HEAD` gets a
        // line on every checkout, pull and commit — or appears at all. Not on
        // the source tree: walking 1 GB of ggml per `cargo build` is the cost
        // this exists to avoid. Edit the clone itself and `touch build.rs`.
        // Our own driver sources are small, so those are watched directly.
        println!("cargo::rerun-if-changed={}", clone.join(".git/logs/HEAD").display());
        println!("cargo::rerun-if-changed={}", clone.join("CMakeLists.txt").display());
        for f in n.own {
            println!("cargo::rerun-if-changed={}", manifest.join(f).display());
        }
        if !clone.join("CMakeLists.txt").is_file() {
            if release {
                let need = if n.required {
                    format!("this release has no {} inside it and will need a binary path", n.name)
                } else {
                    format!("this release has no {} inside it, so it has no agent", n.name)
                };
                println!("cargo::warning=no {}/ in the repo, so {need}. docs/building.md has the clone.", n.clone);
            }
            return;
        }
        // cmake here would build for the machine running cargo, not for the
        // target — a Linux binary inside a Windows exe. A cross build (in
        // practice `cargo check --target x86_64-pc-windows-gnu`, the cheap
        // Windows gate) is for the Rust half; the real one is CI's, on Windows.
        if std::env::var("TARGET") != std::env::var("HOST") {
            println!("cargo::warning=cross build: {} not built or embedded", n.name);
            return;
        }
        match build(n, &manifest.join(n.cmake_src), &clone.join(BUILD_DIR)) {
            Ok(bin) => (bin, release),
            Err(e) if release => panic!("{e}"),
            Err(e) => {
                println!("cargo::warning={}", e.replace('\n', " "));
                return;
            }
        }
    };
    println!("cargo::rerun-if-changed={}", bin.display());
    if embed {
        embed_binary(n, &bin);
    }
}

/// Configure once, then build the one target. A no-op build is a couple of
/// seconds; the first is several minutes, most of it ggml-vulkan's shaders.
fn build(n: &Native, src: &Path, build: &Path) -> Result<PathBuf, String> {
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    let windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");

    // A stamp written after a configure *succeeds*, not the cache: a failed
    // configure leaves a CMakeCache.txt behind, and trusting it turned one
    // missing package into a build that failed with "no Makefile" forever.
    let configured = build.join(".yasdig-configured");
    if !configured.is_file() {
        let mut cfg = Command::new("cmake");
        cfg.arg("-S").arg(src).arg("-B").arg(build).args([
            "-DCMAKE_BUILD_TYPE=Release",
            "-DGGML_NATIVE=OFF",
            "-DGGML_OPENMP=OFF",
        ]);
        cfg.args(n.flags);
        if msvc {
            cfg.args([
                "-DCMAKE_POLICY_DEFAULT_CMP0091=NEW",
                "-DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded",
                "-DCMAKE_CXX_FLAGS=/EHsc /bigobj",
            ]);
        } else if windows {
            // MinGW (MSYS2's UCRT64): CMake's default generator on Windows is
            // Visual Studio's, and a static link keeps libstdc++/winpthread
            // DLLs out — the AppContainer can't reach C:\msys64 to load them.
            cfg.args([
                "-G",
                "Ninja",
                "-DCMAKE_EXE_LINKER_FLAGS=-static",
                "-DCMAKE_CXX_FLAGS=-Wa,-mbig-obj",
            ]);
        }
        run(cfg, "configuring", n.what)?;
        std::fs::write(&configured, "").map_err(|e| e.to_string())?;
    }

    let jobs = std::thread::available_parallelism().map_or(4, |n| n.get());
    let mut b = Command::new("cmake");
    b.arg("--build").arg(build).args(["--config", "Release", "--target", n.name]);
    b.arg("--parallel").arg(jobs.to_string());
    run(b, "building", n.what)?;

    // Single-config generators (Makefiles, Ninja) put it in bin/; Visual
    // Studio's multi-config one in bin/Release/.
    let exe = if windows { format!("{}.exe", n.name) } else { n.name.to_string() };
    [build.join("bin").join(&exe), build.join("bin/Release").join(&exe)]
        .into_iter()
        .find(|p| p.is_file())
        .ok_or_else(|| format!("{} built, but no {exe} under {}", n.what, build.display()))
}

fn run(mut cmd: Command, step: &str, what: &str) -> Result<(), String> {
    match cmd.status() {
        Ok(s) if s.success() => Ok(()),
        Ok(_) => Err(format!(
            "{step} {what} failed — the cmake output is above. It needs cmake, \
             a C++ compiler, glslc and the Vulkan headers; docs/building.md \
             lists the packages"
        )),
        Err(e) => Err(format!(
            "{step} {what}: can't run cmake ({e}). Install cmake, glslc and the \
             Vulkan headers — docs/building.md lists the packages"
        )),
    }
}

/// Copy the binary into `OUT_DIR` with a hash beside it. FNV-1a, written here
/// rather than pulled in as a crate: it only has to tell one build of a binary
/// from another, so a start can skip rewriting 93 MB that `bin/` already holds.
fn embed_binary(n: &Native, bin: &Path) {
    let bytes = std::fs::read(bin).unwrap_or_else(|e| panic!("reading {}: {e}", bin.display()));
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    std::fs::write(out.join(n.name), &bytes).unwrap();

    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in &bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    std::fs::write(out.join(format!("{}.hash", n.name)), format!("{h:016x}-{}", bytes.len())).unwrap();
    println!("cargo::rustc-cfg={}", n.cfg);
}
