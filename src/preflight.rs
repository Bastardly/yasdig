//! Startup preflight checks. Instead of a cryptic failure deep in a request,
//! we validate the environment up front (sd.cpp binary, checkpoints, web UI),
//! list the models found in the models dir, and print specific, copy-pasteable
//! fix instructions for anything missing.

use crate::config::Config;
use crate::discover::Role;
use crate::settings::Settings;

/// One line naming what a shared component is, for the `doctor` listing.
///
/// A LoRA gets an extra clause here that [`Role::describe`] deliberately
/// doesn't: this listing is the video dir's, and both tabs pick LoRAs from the
/// LoRA folder alone, so one here is only reachable by being named in config.
fn describe_role(role: &Role) -> String {
    match role {
        Role::Lora(_) => "LoRA — move it to the LoRA folder to pick it on the Video tab".into(),
        other => other.describe(),
    }
}

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
            println!("[yasdig] {i}");
        }
        for w in &self.warnings {
            eprintln!("[yasdig] warning:\n{}\n", indent(w));
        }
        if !self.errors.is_empty() {
            eprintln!("[yasdig] cannot start — the following must be fixed:\n");
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

/// Every `.gguf` under `dir`, as deep as the model scan goes.
fn gguf_files(dir: &std::path::Path, depth: usize, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() && depth < 6 {
            gguf_files(&p, depth + 1, out);
        } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("gguf")) {
            out.push(p);
        }
    }
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
    let bin = cfg.sd_binary();
    if !bin.exists() && cfg.sd.binary.is_none() {
        // Only reachable from a source build: a release carries its own and
        // `Config::load` has already written it out.
        errors.push(format!(
            "this build of yasdig has no stable-diffusion.cpp inside it, and\n\
             models.toml names none. Either build one with YASDIG_SD_CLI set\n\
             (see docs/building.md), or build sd.cpp and set [sd].binary:\n  {}",
            bin.display()
        ));
    } else if !bin.exists() {
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
    } else {
        // Which of the two it is, because "my build isn't being used" and "the
        // bundled one is stale" are both questions this line answers.
        let whose = match (&cfg.sd.binary, crate::bundle::BUNDLED) {
            (Some(_), _) => "[sd].binary",
            (None, true) => "built in",
            (None, false) => "left in bin/ by an earlier build",
        };
        info.push(format!("stable-diffusion.cpp ({whose}): {}", bin.display()));
    }

    // 1b) The settings file. `Config::load` creates it if it is missing, so by
    // the time this runs the only interesting cases are "it is there" and "it is
    // there and unreadable" — and the second has to be loud, because generation
    // silently falls back to the built-ins when it happens (see
    // `Settings::load_or_default`) and the symptom is otherwise just numbers
    // that aren't the ones you set.
    let settings_path = Settings::path(&cfg.user_dir());
    match Settings::load(&cfg.user_dir()) {
        Ok(s) => {
            info.push(format!("settings: {}", settings_path.display()));
            // Named rather than counted: the one question this answers is "why
            // does {animal} say it isn't a variable", and the answer is nearly
            // always that it is saved under another name.
            if !s.variables.is_empty() {
                info.push(format!(
                    "prompt variables: {}",
                    s.variables
                        .iter()
                        .map(|(k, v)| {
                            let n = v.len();
                            format!("{{{k}}} ({n} word{})", if n == 1 { "" } else { "s" })
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            // A table a text editor put out of range is not fatal — everything
            // without a `{}` in it still renders — but every prompt that names
            // the broken variable fails, so it has to be said once at startup.
            if let Err(e) = crate::variables::validate(&s.variables) {
                warnings.push(format!(
                    "the [variables] table in\n  {}\nis unusable: {e:#}\n\n\
                     A prompt naming a broken variable will fail until it is\n\
                     fixed; everything without a {{}} in it still renders.\n\
                     The Settings tab edits the table and saving repairs it.",
                    settings_path.display()
                ));
            }
        }
        Err(e) => warnings.push(format!(
            "{e:#}\n\n\
             The built-in defaults are in use until it is fixed. Saving from the\n\
             Settings tab rewrites the file and repairs it."
        )),
    }
    // The old block is read exactly once, to seed that file on a first run. If
    // both exist it is dead weight, and saying nothing would leave two files
    // that look like they disagree.
    if cfg.legacy_defaults.is_some() && settings_path.exists() {
        warnings.push(format!(
            "models.toml still has a [defaults] block, and it is now ignored.\n\
             Those settings live in\n  {}\n\
             and are edited from the Settings tab. Delete the block so the two\n\
             can't look like they disagree.",
            settings_path.display()
        ));
    }

    // The same for the old [server]: `startup.toml` was seeded from it on the
    // first start after the move, and reads nothing from it since.
    if cfg.legacy_server.is_some() && crate::startup::path(&cfg.base_dir).exists() {
        warnings.push(format!(
            "models.toml still has a [server] block, and it is now ignored.\n\
             The port and address live in\n  {}\n\
             Delete the block so the two can't look like they disagree.",
            crate::startup::path(&cfg.base_dir).display()
        ));
    }

    // 2) Model registry (explicit templates + discovered checkpoints).
    let registry = cfg.registry();
    // The per-model layer, read once here for the tags below and for the
    // check that every entry in it still names a model.
    let tuned = match crate::settings::ModelSettings::load(&cfg.user_dir()) {
        Ok(t) => t,
        Err(e) => {
            warnings.push(format!(
                "{e:#}\n\n\
                 No per-model settings are in use until it is fixed. Saving a model's\n\
                 defaults rewrites the file and repairs it."
            ));
            crate::settings::ModelSettings::default()
        }
    };
    // Keyed by name, and a discovered model's name is its file stem — so a
    // renamed or deleted file leaves an entry that applies to nothing. A server
    // start sets those aside (`ModelSettings::reconcile`) and says so; `doctor`
    // only reports, so it says what the next start will do.
    let saved: std::collections::BTreeSet<&String> = tuned.model.keys().chain(tuned.parts.keys()).collect();
    for name in saved.into_iter().filter(|n| !registry.contains_key(*n)) {
        warnings.push(format!(
            "model '{name}' no longer exists; the next start moves its saved settings to {}\n\
             (renamed the file? rename the entry instead, and it keeps them)",
            crate::settings::ModelSettings::parked_path(&cfg.user_dir()).display()
        ));
    }
    // What each image file was identified as, so a checkpoint's line can name
    // its family and the files the registry left out can be accounted for. The
    // memo in `discover` means this second scan re-reads nothing.
    let image_scan = cfg.scan_images();
    let family_of = |path: &std::path::Path| {
        image_scan.iter().find(|f| f.path == path).and_then(|f| match f.role {
            Role::Image { family, .. } => Some(family),
            _ => None,
        })
    };
    let usable: Vec<_> = registry.values().filter(|p| p.exists).collect();
    let missing: Vec<_> = registry.values().filter(|p| !p.exists).collect();
    let models_dir = cfg.models_dir();

    // A warning, not a refusal: a first run has an empty models folder by
    // definition, and the registry is rescanned on every request, so a file
    // dropped in while the server runs shows up on the next reload. Refusing
    // to start would make the very first double-click end in an error.
    if usable.is_empty() {
        warnings.push(format!(
            "no image models yet.\n\n\
             Drop a .safetensors / .gguf file into:\n  {}\n\
             and it will be auto-detected (single-file safetensors load directly —\n\
             no conversion needed). Distilled models (SDXL-Turbo, SD-Turbo, LCM) give\n\
             the fastest results.",
            models_dir.join("checkpoints").display()
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
            let tuned_here = tuned.model.contains_key(&p.name) || tuned.parts.contains_key(&p.name);
            let tag = match (p.discovered, tuned_here) {
                (true, false) => "discovered",
                (true, true) => "discovered, tuned",
                (false, false) => "preset",
                (false, true) => "preset, tuned",
            };
            // Steps and CFG are printed because a template now *inherits* what
            // it doesn't name: these are the resolved numbers, so a preset that
            // states only `steps` still shows what it will actually run with.
            // A template naming a file that isn't where discovery looks has no
            // scan entry, so its family goes unsaid rather than guessed.
            let family = family_of(&p.model).map(|f| f.as_str()).unwrap_or("?");
            info.push(format!(
                "  • {:<28} [{}, {}, {}]  {}x{} → {}x{}  · {} steps, cfg {}",
                p.name,
                family,
                tag,
                size,
                p.width,
                p.height,
                (p.width as f32 * p.upscale_factor) as u32,
                (p.height as f32 * p.upscale_factor) as u32,
                p.steps,
                p.cfg_scale,
            ));
            // Which VAE and encoder a DiT was assembled with. Picked, not
            // named in config, so this is the only place the choice is shown.
            if let Some(parts) = &p.components {
                for (flag, path) in parts.flags() {
                    info.push(format!(
                        "      {flag:<7} {}",
                        path.strip_prefix(&models_dir).unwrap_or(path).display()
                    ));
                }
            }
        }
    }

    // Templates that point at a file that isn't there are worth flagging.
    // Either a template naming a file that was never downloaded, or a DiT
    // whose VAE or text encoder isn't here yet. Every missing file is printed,
    // at the path it is expected at, so the warning is the download list.
    for p in &missing {
        let files: Vec<String> = p.missing().iter().map(|f| format!("  {}", f.display())).collect();
        warnings.push(format!(
            "{} '{}' ({}) is listed but can't run until these exist:\n{}",
            if p.discovered { "model" } else { "template" },
            p.name,
            p.family.label(),
            files.join("\n")
        ));
    }

    // 2a) Image-side files the select doesn't offer, each with what it is. The
    // registry now lists by header rather than by extension, so without this a
    // file that used to appear in the select would silently stop appearing.
    //
    // Split by how much it matters. A recognised family this build can't run
    // yet, and a component (a VAE, an encoder) waiting for the model that uses
    // it, are normal and are info. An architecture we don't do, or a file
    // nothing matched, is a warning — it is several GB that either is a bad
    // download or is in the wrong folder, and the person believes it works.
    let mut parked = Vec::new();
    for f in &image_scan {
        let name = f
            .path
            .strip_prefix(&models_dir)
            .unwrap_or(&f.path)
            .display()
            .to_string();
        match &f.role {
            Role::Image { family, .. } if family.runnable() => {}
            Role::Unsupported(_) | Role::Unknown(_) => warnings.push(format!(
                "not offered in the Image tab:\n  {}\n  {}",
                f.path.display(),
                f.role.describe()
            )),
            // A Wan transformer is the Video tab's, and it only looks for them
            // in its own directory.
            Role::Dit(_) => warnings.push(format!(
                "a video model outside the video dir, so neither tab lists it:\n  {}\n  \
                 move it under {}",
                f.path.display(),
                cfg.video_dir().display()
            )),
            other => parked.push(format!("  ○ {:<44} {}", name, other.describe())),
        }
    }
    if !parked.is_empty() {
        info.push(format!(
            "{} other image-side file(s) — components, or families this build doesn't run yet:",
            parked.len()
        ));
        info.extend(parked);
    }

    // 2b) LoRAs. Never an error and never a warning: an empty folder is the
    // normal state of one, and the reason this is printed at all is that "the
    // picker is empty" and "the picker is looking somewhere else" are
    // indistinguishable from the browser. The path is the answer to both.
    let loras = cfg.list_loras();
    let usable_loras = loras.iter().filter(|l| l.usable).count();
    info.push(format!(
        "{usable_loras} LoRA(s) in {}",
        cfg.lora_dir().display()
    ));
    for l in &loras {
        info.push(format!(
            "  {} {:<44} {}",
            if l.usable { "·" } else { "○" },
            l.name,
            if l.usable && l.base.is_empty() {
                format!("{}, base not recognised — offered for every model", human_size(l.bytes))
            } else if l.usable {
                format!("{}, for {}", human_size(l.bytes), l.base)
            } else {
                l.note.clone()
            },
        ));
    }

    // 2c) Upscalers, on the same terms: an empty folder is normal, since FSR
    // needs no file, and the path is the answer to "why is the select empty".
    let upscalers = cfg.list_upscalers();
    info.push(format!(
        "{} upscaler(s) in {} — FSR is always available",
        upscalers.len(),
        cfg.upscaler_dir().display()
    ));
    for u in &upscalers {
        info.push(format!(
            "  · {:<44} {:<10} {}{}",
            u.name,
            u.arch,
            human_size(u.bytes),
            if u.supported { "" } else { "  (not supported)" }
        ));
    }

    // 3) Video models: whatever the video dir was identified as, plus the
    // `[[video]]` table.
    //
    // Never an error, even with nothing on disk. The image side must have a
    // usable checkpoint to be worth starting at all; video is an extra tab, and
    // a half-downloaded 20 GB model is the normal state of one for an hour at a
    // time. So the report is a download checklist: it says which component file
    // is still missing, which is exactly what the config comments promise.
    let clips = cfg.video_models();
    let scan = cfg.discover_video();
    if !clips.is_empty() {
        let ready = clips.iter().filter(|m| m.exists).count();
        info.push(format!("{ready}/{} video model(s) ready:", clips.len()));
        for m in &clips {
            let (w, h, frames) = m.aligned();
            info.push(format!(
                "  {} {:<24} [{}]  {w}x{h}, {frames} frames @ {} fps ({:.1}s){}",
                if m.exists { "•" } else { "○" },
                m.name,
                m.family.as_str(),
                m.effective_fps(),
                m.duration(),
                if m.discovered { "" } else { "  (models.toml)" },
            ));
            for path in m.missing() {
                info.push(format!("      missing: {}", path.display()));
            }
        }
    }
    // The shared parts, so a folder of eight files that produced two models
    // doesn't look like it lost six of them.
    if !scan.components.is_empty() {
        info.push(format!(
            "{} shared video component(s) in {}:",
            scan.components.len(),
            cfg.video_dir().display()
        ));
        for c in &scan.components {
            info.push(format!(
                "  · {:<44} {}",
                c.path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                describe_role(&c.role),
            ));
        }
    }
    // And the ones nothing could be made of. A warning rather than info: an
    // unplaceable 15 GB file in the models dir is either a bad download or
    // something the user believes is working, and both are worth saying out
    // loud once.
    for u in &scan.unrecognized {
        warnings.push(format!(
            "not recognised as a video model component:\n  {}\n  {}",
            u.path.display(),
            u.why
        ));
    }

    // 3b) GGUFs converted from scaled fp8 before the convert folded the scales.
    // They load and render, as a mesh that worsens every step, so the file is
    // named with the command that repairs it (`foldscale`).
    let mut ggufs = Vec::new();
    gguf_files(&models_dir, 0, &mut ggufs);
    if cfg.video_dir().strip_prefix(&models_dir).is_err() {
        gguf_files(&cfg.video_dir(), 0, &mut ggufs);
    }
    for p in &ggufs {
        if let Ok(n @ 1..) = crate::foldscale::pending(p) {
            warnings.push(format!(
                "{n} fp8 scale(s) in this GGUF were never folded into its weights, which\n\
                 overflows on Vulkan and renders as a mesh or mosaic:\n  {}\n\
                 repair it in place with:\n  yasdig fold-scales \"{}\"",
                p.display(),
                p.display()
            ));
        }
    }

    // 4) Web UI (API still works without it, so this is only a warning).
    // Every file the page is made of, read off the same table the route serves
    // from — a missing stylesheet or module doesn't read as a missing UI, it
    // reads as a broken one, and one absent module blanks the whole page.
    // Only when there is a `web/` to serve from: without one the page is the
    // copy built into this executable, which can't be missing a file.
    let web = cfg.base_dir.join("web");
    if !web.is_dir() {
        info.push("web UI: built in".into());
    }
    let missing: Vec<_> = crate::web::WEB_FILES
        .iter()
        .filter(|_| web.is_dir())
        .map(|f| web.join(f.name))
        .filter(|path| !path.exists())
        .collect();
    if !missing.is_empty() {
        let list: Vec<String> = missing.iter().map(|p| format!("  {}", p.display())).collect();
        warnings.push(format!(
            "web UI file{} not found:\n{}\nThe JSON API still works; the browser UI won't load, or will load broken.",
            if missing.len() == 1 { "" } else { "s" },
            list.join("\n"),
        ));
    }

    // curl is what the Get models tab downloads with (`downloads`). A warning,
    // not an error: every model can still be fetched by hand.
    if crate::downloads::curl_available() {
        info.push("curl: found (the Get models tab can download)".into());
    } else {
        warnings.push(
            "curl was not found on the PATH, so the Get models tab can't download.\n\
             Install it (it ships with Windows 10 and later and with most Linux\n\
             distributions), or fetch the files by hand — docs/configuration.md, Get a model."
                .into(),
        );
    }

    Report {
        errors,
        warnings,
        info,
    }
}
