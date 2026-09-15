//! Startup preflight checks. Instead of a cryptic failure deep in a request,
//! we validate the environment up front (sd.cpp binary, checkpoints, web UI),
//! list the models found in the models dir, and print specific, copy-pasteable
//! fix instructions for anything missing.

use crate::config::Config;

pub struct Report {
    /// Fatal problems — the app cannot usefully serve requests.
    pub errors: Vec<String>,
    /// Non-fatal problems — worth flagging but the app can still run.
    pub warnings: Vec<String>,
    /// Informational lines (e.g. the discovered model list).
    pub info: Vec<String>,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.errors.is_empty()
    }

    /// Print the report. Returns true if it's safe to continue (no fatal errors).
    pub fn print(&self) -> bool {
        for i in &self.info {
            println!("[rustimage] {i}");
        }
        for w in &self.warnings {
            eprintln!("[rustimage] warning:\n{}\n", indent(w));
        }
        if !self.errors.is_empty() {
            eprintln!("[rustimage] cannot start — the following must be fixed:\n");
            for e in &self.errors {
                eprintln!("{}\n", indent(e));
            }
        }
        self.ok()
    }
}

fn indent(s: &str) -> String {
    s.lines()
        .map(|l| format!("  {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn human_size(bytes: u64) -> String {
    const U: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

pub fn check(cfg: &Config) -> Report {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let mut info = Vec::new();

    // 1) sd.cpp binary.
    let bin = cfg.resolve(&cfg.sd.binary);
    if !bin.exists() {
        errors.push(format!(
            "stable-diffusion.cpp binary not found:\n  {}\n\n\
             Build it (cmake, glslc and the SPIR-V/Vulkan dev packages are needed):\n\
             \x20   cd {parent}\n\
             \x20   git clone --recursive https://github.com/leejet/stable-diffusion.cpp\n\
             \x20   cd stable-diffusion.cpp && cmake -B build -DSD_VULKAN=ON && cmake --build build -j\n\n\
             Then point [sd].binary in models.toml at the result\n\
             (build/bin/sd-cli on recent builds, or build/bin/sd on older ones).",
            bin.display(),
            parent = cfg.base_dir.parent().unwrap_or(&cfg.base_dir).display(),
        ));
    } else if bin.is_dir() {
        errors.push(format!(
            "[sd].binary points at a directory, not an executable:\n  {}",
            bin.display()
        ));
    }

    // 2) Model registry (explicit templates + discovered checkpoints).
    let registry = cfg.registry();
    let usable: Vec<_> = registry.values().filter(|p| p.exists).collect();
    let missing: Vec<_> = registry.values().filter(|p| !p.exists).collect();
    let models_dir = cfg.models_dir();

    if usable.is_empty() {
        errors.push(format!(
            "no usable checkpoints.\n\n\
             Drop a .safetensors / .ckpt / .gguf file into:\n  {}\n\
             and it will be auto-detected (single-file safetensors load directly —\n\
             no conversion needed). Distilled models (SDXL-Turbo, SD-Turbo, LCM) give\n\
             the fastest results.",
            models_dir.display()
        ));
    } else {
        info.push(format!(
            "{} model(s) available in {}:",
            usable.len(),
            models_dir.display()
        ));
        for p in &usable {
            let size = std::fs::metadata(&p.model)
                .map(|m| human_size(m.len()))
                .unwrap_or_else(|_| "?".into());
            let tag = if p.discovered { "discovered" } else { "preset" };
            info.push(format!(
                "  • {:<28} [{}, {}]  {}x{} → {}x{}",
                p.name,
                tag,
                size,
                p.width,
                p.height,
                (p.width as f32 * p.upscale_factor) as u32,
                (p.height as f32 * p.upscale_factor) as u32,
            ));
        }
    }

    // Templates that point at a file that isn't there are worth flagging.
    for p in &missing {
        warnings.push(format!(
            "template '{}' references a checkpoint that doesn't exist:\n  {}",
            p.name,
            p.model.display()
        ));
    }

    // 3) Web UI (API still works without it, so this is only a warning).
    let index = cfg.base_dir.join("web").join("index.html");
    if !index.exists() {
        warnings.push(format!(
            "web UI not found:\n  {}\nThe JSON API still works, but the browser UI won't load.",
            index.display()
        ));
    }

    Report {
        errors,
        warnings,
        info,
    }
}
