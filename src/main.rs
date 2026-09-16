//! rustimage — Python-free Vulkan image generator.
//!
//! A minimal std-only HTTP server fronts a single GPU worker thread. The worker
//! owns the sd.cpp runner and the wgpu FSR context, so all GPU work is
//! serialized (no VRAM thrash). Routes:
//!   GET  /            -> static web UI
//!   GET  /templates   -> JSON list of model profiles
//!   POST /generate    -> {prompt, template, negative?, seed?, ...overrides}
//!                        -> {image_base64, width, height, seed, image_id,
//!                            can_refine, can_hires, timings}
//!   POST /refine      -> {image_id}            img2img in place, no rescale
//!   POST /hires       -> {image_id, upscale}   img2img with an enlarge first
//!                        both return the same shape as /generate, and both can
//!                        be run in either order and repeated
//!   POST /adjust      -> {image_id, sharpness?, clarity?}
//!                        re-runs the display filters only — no diffusion, so
//!                        it's cheap enough to drive from a slider

mod config;
mod fsr;
mod http;
mod pipeline;
mod preflight;
mod sd;

use anyhow::{Context, Result};
use base64::Engine as _;
use config::Config;
use pipeline::{Engine, GenRequest, GenResult};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;

/// A unit of GPU work plus a channel to send the result back to the HTTP thread.
struct Job {
    kind: JobKind,
    reply: Sender<Result<GenResult>>,
}

enum JobKind {
    Generate(GenRequest),
    /// Refine the base image the engine retained from an earlier generation.
    Refine(u64),
    /// Re-diffuse an earlier result at output resolution, optionally enlarging.
    Hires { id: u64, upscale: f32 },
    /// Re-filter an earlier result — display passes only, no diffusion.
    Adjust {
        id: u64,
        sharpness: Option<f32>,
        clarity: Option<f32>,
    },
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();

    // Subcommands that exercise the FSR (wgpu) path only — no sd.cpp, no server.
    // Useful for verifying the GPU post-process in isolation.
    match args.get(1).map(String::as_str) {
        Some("fsr") => return run_fsr_cli(&args[2..]),
        Some("fsr-selftest") => return run_fsr_selftest(&args[2..]),
        Some("doctor") => return run_doctor(&args[2..]),
        _ => {}
    }

    let config_path = args
        .get(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("models.toml"));

    let cfg = Arc::new(
        Config::load(&config_path)
            .with_context(|| format!("loading {}", config_path.display()))?,
    );

    // Validate the environment before touching the GPU, so missing prerequisites
    // produce clear, actionable instructions instead of a mid-request failure.
    let report = preflight::check(&cfg);
    if !report.print() {
        eprintln!("[rustimage] run `rustimage doctor` after fixing the above to re-check.");
        std::process::exit(1);
    }

    let bind = format!("{}:{}", cfg.server.host, cfg.server.port);
    let web_dir = cfg.base_dir.join("web");

    // Spawn the single GPU worker. It initializes wgpu + sd, reports readiness,
    // then serves jobs one at a time.
    let engine_cfg = (*cfg).clone();
    let (job_tx, job_rx) = mpsc::channel::<Job>();
    let (ready_tx, ready_rx) = mpsc::channel::<Result<String>>();
    std::thread::Builder::new()
        .name("gpu-worker".into())
        .spawn(move || match Engine::new(engine_cfg) {
            Ok(mut engine) => {
                let _ = ready_tx.send(Ok(engine.adapter_name().to_string()));
                for job in job_rx {
                    let result = match job.kind {
                        JobKind::Generate(req) => engine.run(&req),
                        JobKind::Refine(id) => engine.refine(id),
                        JobKind::Hires { id, upscale } => engine.hires(id, upscale),
                        JobKind::Adjust {
                            id,
                            sharpness,
                            clarity,
                        } => engine.adjust(id, sharpness, clarity),
                    };
                    let _ = job.reply.send(result);
                }
            }
            Err(e) => {
                let _ = ready_tx.send(Err(e));
            }
        })
        .context("spawning gpu worker")?;

    match ready_rx.recv().context("gpu worker died during startup")? {
        Ok(name) => {
            println!("[rustimage] GPU worker ready on adapter: {name}");
            println!(
                "[rustimage] GPU post-process: upscale=easu, contrast=clarity, sharpen=rcas"
            );
        }
        Err(e) => {
            eprintln!("[rustimage] startup failed:\n{e:#}");
            std::process::exit(1);
        }
    }

    let listener = TcpListener::bind(&bind).with_context(|| format!("binding {bind}"))?;
    println!("[rustimage] listening on http://{bind}");

    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(_) => continue,
        };
        let job_tx = job_tx.clone();
        let cfg = cfg.clone();
        let web_dir = web_dir.clone();
        std::thread::spawn(move || {
            if let Err(e) = handle_conn(stream, &job_tx, &cfg, &web_dir) {
                eprintln!("[rustimage] connection error: {e:#}");
            }
        });
    }

    Ok(())
}

fn handle_conn(
    stream: std::net::TcpStream,
    job_tx: &Sender<Job>,
    cfg: &Config,
    web_dir: &std::path::Path,
) -> Result<()> {
    let mut conn = http::Connection::new(stream)?;
    let req = match conn.read_request()? {
        Some(r) => r,
        None => return Ok(()),
    };

    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/") | ("GET", "/index.html") => {
            let path = web_dir.join("index.html");
            match std::fs::read(&path) {
                Ok(body) => conn.respond(200, "text/html; charset=utf-8", &body),
                Err(_) => conn.respond(500, "text/plain", b"web/index.html not found"),
            }
        }
        // Built live so newly-dropped checkpoints appear without a restart.
        ("GET", "/templates") | ("GET", "/models") => {
            conn.respond(200, "application/json", build_models_json(cfg).as_bytes())
        }
        ("POST", "/generate") => handle_generate(&mut conn, job_tx, &req.body),
        ("POST", "/refine") => handle_refine(&mut conn, job_tx, &req.body),
        ("POST", "/hires") => handle_hires(&mut conn, job_tx, &req.body),
        ("POST", "/adjust") => handle_adjust(&mut conn, job_tx, &req.body),
        _ => conn.respond(404, "text/plain", b"not found"),
    }
}

fn handle_generate(conn: &mut http::Connection, job_tx: &Sender<Job>, body: &[u8]) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct Dto {
        prompt: String,
        template: String,
        #[serde(default)]
        negative: Option<String>,
        #[serde(default)]
        seed: Option<i64>,
        #[serde(default)]
        steps: Option<u32>,
        #[serde(default)]
        cfg_scale: Option<f32>,
        #[serde(default)]
        img2img_cfg_scale: Option<f32>,
        #[serde(default)]
        sampler: Option<String>,
        #[serde(default)]
        width: Option<u32>,
        #[serde(default)]
        height: Option<u32>,
        #[serde(default)]
        refine_steps: Option<u32>,
        #[serde(default)]
        refine_strength: Option<f32>,
        #[serde(default)]
        hires_steps: Option<u32>,
        #[serde(default)]
        hires_strength: Option<f32>,
        #[serde(default)]
        sharpness: Option<f32>,
        #[serde(default)]
        clarity: Option<f32>,
    }

    let dto: Dto = match parse_json::<Dto>(conn, body)? {
        Some(d) => d,
        None => return Ok(()),
    };

    let req = GenRequest {
        prompt: dto.prompt,
        template: dto.template,
        negative: dto.negative,
        seed: dto.seed.unwrap_or(-1),
        steps: dto.steps,
        cfg_scale: dto.cfg_scale,
        img2img_cfg_scale: dto.img2img_cfg_scale,
        sampler: dto.sampler,
        width: dto.width,
        height: dto.height,
        refine_steps: dto.refine_steps,
        refine_strength: dto.refine_strength,
        hires_steps: dto.hires_steps,
        hires_strength: dto.hires_strength,
        sharpness: dto.sharpness,
        clarity: dto.clarity,
    };

    submit(conn, job_tx, JobKind::Generate(req), 500)
}

/// Escalate the quality of an earlier result, identified by the `image_id` that
/// generation returned. An unknown id means the intermediates are gone (evicted
/// or the server restarted), which is a client-side staleness problem — 400, not
/// 500. The same goes for asking for a pass the image can't take.
fn handle_refine(conn: &mut http::Connection, job_tx: &Sender<Job>, body: &[u8]) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct Dto {
        image_id: u64,
    }

    match parse_json::<Dto>(conn, body)? {
        Some(dto) => submit(conn, job_tx, JobKind::Refine(dto.image_id), 400),
        None => Ok(()),
    }
}

fn handle_hires(conn: &mut http::Connection, job_tx: &Sender<Job>, body: &[u8]) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct Dto {
        image_id: u64,
        #[serde(default)]
        upscale: Option<f32>,
    }

    match parse_json::<Dto>(conn, body)? {
        Some(dto) => submit(
            conn,
            job_tx,
            JobKind::Hires {
                id: dto.image_id,
                upscale: dto.upscale.unwrap_or(1.0),
            },
            400,
        ),
        None => Ok(()),
    }
}

/// Re-filter an earlier result. Same staleness contract as the escalations — an
/// unknown id is 400 — but no diffusion runs, so this is the one handler that
/// comes back in milliseconds and can be driven live from the UI's sliders.
/// An omitted field keeps whatever the image currently has.
fn handle_adjust(conn: &mut http::Connection, job_tx: &Sender<Job>, body: &[u8]) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct Dto {
        image_id: u64,
        #[serde(default)]
        sharpness: Option<f32>,
        #[serde(default)]
        clarity: Option<f32>,
    }

    match parse_json::<Dto>(conn, body)? {
        Some(dto) => submit(
            conn,
            job_tx,
            JobKind::Adjust {
                id: dto.image_id,
                sharpness: dto.sharpness,
                clarity: dto.clarity,
            },
            400,
        ),
        None => Ok(()),
    }
}

/// Parse a JSON request body, writing a 400 and returning `None` if it's malformed.
fn parse_json<T: serde::de::DeserializeOwned>(
    conn: &mut http::Connection,
    body: &[u8],
) -> Result<Option<T>> {
    match serde_json::from_slice(body) {
        Ok(v) => Ok(Some(v)),
        Err(e) => {
            conn.respond(
                400,
                "application/json",
                error_json(&format!("bad request: {e}")).as_bytes(),
            )?;
            Ok(None)
        }
    }
}

/// Enqueue GPU work, wait for the worker, and write the JSON response.
/// `err_status` is the status used when the job itself fails.
fn submit(
    conn: &mut http::Connection,
    job_tx: &Sender<Job>,
    kind: JobKind,
    err_status: u16,
) -> Result<()> {
    let (reply_tx, reply_rx) = mpsc::channel();
    if job_tx
        .send(Job {
            kind,
            reply: reply_tx,
        })
        .is_err()
    {
        return conn.respond(
            503,
            "application/json",
            error_json("gpu worker unavailable").as_bytes(),
        );
    }

    let result = reply_rx
        .recv()
        .context("gpu worker dropped the reply channel")?;

    match result {
        Ok(res) => conn.respond(200, "application/json", result_json(&res).as_bytes()),
        Err(e) => conn.respond(
            err_status,
            "application/json",
            error_json(&format!("{e:#}")).as_bytes(),
        ),
    }
}

/// The image plus its metadata as JSON. `can_refine` / `can_hires` say which
/// escalations are still available for `image_id`, which is what the UI keys
/// the buttons above the image off.
fn result_json(res: &GenResult) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(&res.png);
    format!(
        "{{\"image_base64\":\"{}\",\"width\":{},\"height\":{},\"seed\":{},\"refine_passes\":{},\
         \"hires_passes\":{},\"image_id\":{},\"can_refine\":{},\"can_hires\":{},\
         \"sharpness\":{},\"clarity\":{},\
         \"timings\":{{\"gen_ms\":{},\"fsr_ms\":{},\"refine_ms\":{},\"hires_ms\":{}}}}}",
        b64,
        res.width,
        res.height,
        res.seed,
        res.refine_passes,
        res.hires_passes,
        res.image_id,
        res.can_refine,
        res.can_hires,
        res.sharpness,
        res.clarity,
        res.timings.gen_ms,
        res.timings.fsr_ms,
        res.timings.refine_ms,
        res.timings.hires_ms,
    )
}

/// The registry (templates + discovered checkpoints) as JSON for the UI. The
/// per-model defaults (steps/cfg/sampler/size) are included so the UI can
/// pre-fill the advanced controls when a model is selected.
fn build_models_json(cfg: &Config) -> String {
    let items: Vec<String> = cfg
        .registry()
        .values()
        .map(|p| {
            let out_w = ((p.width as f32) * p.upscale_factor).round() as u32;
            let out_h = ((p.height as f32) * p.upscale_factor).round() as u32;
            format!(
                "{{\"name\":{},\"description\":{},\"discovered\":{},\"exists\":{},\
                 \"base_width\":{},\"base_height\":{},\"out_width\":{},\"out_height\":{},\
                 \"steps\":{},\"cfg_scale\":{},\"img2img_cfg_scale\":{},\"sampler\":{},\
                 \"refine_steps\":{},\"refine_strength\":{},\
                 \"hires_steps\":{},\"hires_strength\":{},\"sharpness\":{},\"clarity\":{}}}",
                json_str(&p.name),
                json_str(&p.description),
                p.discovered,
                p.exists,
                p.width,
                p.height,
                out_w,
                out_h,
                p.steps,
                p.cfg_scale,
                // `null`, not the resolved number: the UI has to tell "unset" from
                // "set to exactly the base CFG", because they look the same and
                // behave differently the moment the CFG box moves.
                p.img2img_cfg_scale
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "null".to_string()),
                json_str(&p.sampler),
                p.refine_steps,
                p.refine_strength,
                p.hires_steps,
                p.hires_strength,
                p.sharpness,
                p.clarity,
            )
        })
        .collect();
    format!("[{}]", items.join(","))
}

fn error_json(msg: &str) -> String {
    format!("{{\"error\":{}}}", json_str(msg))
}

/// `rustimage doctor [models.toml]`
/// Run the same startup checks and report, without starting the server.
fn run_doctor(args: &[String]) -> Result<()> {
    let config_path = args
        .get(0)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("models.toml"));
    let cfg = Config::load(&config_path)
        .with_context(|| format!("loading {}", config_path.display()))?;

    let report = preflight::check(&cfg);
    let ok = report.print();
    if ok {
        println!("[rustimage] all checks passed — ready to run.");
        Ok(())
    } else {
        std::process::exit(1);
    }
}

/// `rustimage fsr <in.png> <out.png> [upscale_factor] [sharpness] [clarity]`
/// Runs only the GPU post-process passes on an existing image.
fn run_fsr_cli(args: &[String]) -> Result<()> {
    if args.len() < 2 {
        eprintln!(
            "usage: rustimage fsr <in.png> <out.png> [upscale_factor=2.0] \
             [sharpness=0.35] [clarity=0.0]"
        );
        std::process::exit(2);
    }
    let in_png = PathBuf::from(&args[0]);
    let out_png = PathBuf::from(&args[1]);
    let factor: f32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(2.0);
    let sharp: f32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0.35);
    let clarity: f32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(0.0);

    let fsr = fsr::Fsr::new()?;
    println!("[fsr] adapter: {}", fsr.adapter_name());
    let start = std::time::Instant::now();
    let (w, h) = fsr.process(&in_png, &out_png, factor, sharp, clarity)?;
    println!(
        "[fsr] {} -> {} : {}x{} (x{factor}, sharpness {sharp}, clarity {clarity}) in {} ms",
        in_png.display(),
        out_png.display(),
        w,
        h,
        start.elapsed().as_millis()
    );
    Ok(())
}

/// `rustimage fsr-selftest [out.png]`
/// Exercises the full wgpu path (device init, every compute pass, readback)
/// without sd.cpp or a checkpoint, and checks what the shaders promise. This is
/// what to run after touching a filter constant.
///
/// 1. **Dimensions** — a 2× pass on a 128² pattern comes back 256².
/// 2. **Identity** — at scale 1.0 with both filters at 0 the output is
///    byte-identical to the input. `sharpen.wgsl` and `contrast.wgsl` both
///    claim this in their headers and `README.md` promises it to users; until
///    now nothing checked it.
/// 3. **Band response** — printed, not asserted. See [`report_bands`].
fn run_fsr_selftest(args: &[String]) -> Result<()> {
    let out_png = PathBuf::from(args.get(0).map(String::as_str).unwrap_or("fsr-selftest.png"));

    // Synthesize a 128x128 pattern: checkerboard + diagonal gradient, so both
    // the upscale (smooth ramp) and the sharpen (hard edges) are observable.
    let size = 128u32;
    let mut img = image::RgbaImage::new(size, size);
    for y in 0..size {
        for x in 0..size {
            let checker = ((x / 8) + (y / 8)) % 2 == 0;
            let ramp = ((x + y) as f32 / (2.0 * size as f32) * 255.0) as u8;
            let px = if checker {
                [ramp, 40, 255 - ramp, 255]
            } else {
                [20, ramp, 60, 255]
            };
            img.put_pixel(x, y, image::Rgba(px));
        }
    }
    let in_png = out_png.with_file_name("fsr-selftest-input.png");
    img.save(&in_png)?;

    let fsr = fsr::Fsr::new()?;
    println!("[fsr-selftest] adapter: {}", fsr.adapter_name());

    // 1) Dimensions.
    let (w, h) = fsr.process(&in_png, &out_png, 2.0, 0.4, 0.5)?;
    let expected = (size * 2, size * 2);
    if (w, h) != expected {
        anyhow::bail!("expected {expected:?}, got {:?}", (w, h));
    }
    println!(
        "[fsr-selftest] dimensions OK: {}x{} ({} -> {})",
        w,
        h,
        in_png.display(),
        out_png.display()
    );

    // 2) Identity. Run on the checkerboard rather than something smooth: hard
    //    edges are where RCAS has the most to do, so a pass that is running
    //    when it claims to be skipped shows up here first.
    let ident_png = out_png.with_file_name("fsr-selftest-identity.png");
    fsr.process(&in_png, &ident_png, 1.0, 0.0, 0.0)?;
    check_identity(&img, &ident_png)?;
    println!("[fsr-selftest] identity OK: scale 1.0, sharpness 0, clarity 0 is byte-exact");

    // 3) Bands.
    report_bands(&fsr, &out_png)?;
    Ok(())
}

/// Assert an output is byte-for-byte its input.
///
/// Reports *how* it differs rather than only that it does: a one-bit drift
/// across every pixel and a whole pass running by mistake are different bugs
/// and want different fixes.
fn check_identity(src: &image::RgbaImage, out_png: &std::path::Path) -> Result<()> {
    let got = image::open(out_png)
        .with_context(|| format!("loading {}", out_png.display()))?
        .to_rgba8();
    if got.dimensions() != src.dimensions() {
        anyhow::bail!(
            "identity: {:?} in, {:?} out — a pass that should have been skipped resized it",
            src.dimensions(),
            got.dimensions()
        );
    }
    let (mut max_diff, mut differing) = (0i32, 0usize);
    for (a, b) in src.pixels().zip(got.pixels()) {
        let d = (0..3)
            .map(|c| (a.0[c] as i32 - b.0[c] as i32).abs())
            .max()
            .unwrap_or(0);
        if d > 0 {
            differing += 1;
            max_diff = max_diff.max(d);
        }
    }
    if differing > 0 {
        anyhow::bail!(
            "identity: {differing} of {} pixels differ, by up to {max_diff}/255. At scale \
             1.0 with both filters at 0, EASU and the clarity passes are supposed to be \
             skipped outright and RCAS to collapse to e / 1.0 — see the headers in \
             sharpen.wgsl and contrast.wgsl, and the promise in README.md",
            src.width() as usize * src.height() as usize
        );
    }
    Ok(())
}

/// Measure how much each filter moves each spatial frequency, at two sizes.
///
/// The gratings' wavelengths are fractions of the width rather than fixed pixel
/// counts, because that is exactly what `clarity_sigmas` and `rcas_radius`
/// claim to hold constant (CLAUDE.md invariant 6): both size their band from
/// the image, so one setting should move the same bands at 1536 and at 3072.
/// Running both sizes is what puts a number on that — it was previously only
/// ever caught by someone saying sharpening "felt lost" at 3072.
///
/// The table is printed in full and then handed to [`check_bands`], which
/// asserts the properties behind it rather than the values in it. Reading the
/// numbers is still the point — they are how you see a filter change do what
/// you meant — but a regression that inverts a band or flattens the knob now
/// fails the command instead of waiting to be noticed.
fn report_bands(fsr: &fsr::Fsr, near: &std::path::Path) -> Result<()> {
    let mut results: Vec<BandRow> = Vec::new();
    // 2048 is here because 1536 and 3072 alone don't test RCAS at all: the size
    // doubles and `rcas_radius` doubles with it, so the cross sees an identical
    // `radius / wavelength` and returns identical numbers by arithmetic rather
    // than by the invariant holding. 2048 sits inside the radius-1 plateau
    // (which runs 768..2303), so 1536 → 2048 is the case where the image grows
    // and the tap distance doesn't — the drift the step function can't remove.
    // All three are multiples of 512, so every wavelength stays a whole number.
    for size in [1536u32, 2048, 3072] {
        // tone, depth, detail, grit — 1536/384/48/3 px at the smaller size.
        let waves: Vec<u32> = [1u32, 4, 32, 512].iter().map(|d| size / d).collect();
        let (img, protect_min) = grating(size, &waves);
        let in_png = near.with_file_name(format!("fsr-selftest-grating-{size}.png"));
        img.save(&in_png)?;
        let out_png = near.with_file_name(format!("fsr-selftest-bands-{size}.png"));
        let before = band_amplitudes(&img, &waves);

        let (wide, narrow) = fsr::clarity_sigmas(size, size);
        println!(
            "\n[fsr-selftest] bands at {size}² — rcas radius {}, clarity sigma \
             {wide:.2}/{narrow:.2} (low-res px), midtone protect ≥ {protect_min:.3}",
            fsr::rcas_radius(size, size)
        );
        if wide >= 16.0 {
            println!(
                "  NOTE: the wide lobe is at its 16.0 clamp, which it reaches at 3200 px. \
                 Past that\n        clarity stops scaling with the image and only RCAS \
                 still holds invariant 6."
            );
        }
        let head: String = waves
            .iter()
            .zip(BANDS)
            .map(|(w, n)| format!("{:>12}", format!("{n} {w}px")))
            .collect();
        println!("  {:<15}{head}{:>7}", "", "ms");
        for (label, sharpness, clarity) in [
            ("clarity +0.6", 0.0, 0.6),
            ("clarity -0.6", 0.0, -0.6),
            ("sharpness 1.0", 1.0, 0.0),
        ] {
            let t = std::time::Instant::now();
            fsr.process(&in_png, &out_png, 1.0, sharpness, clarity)?;
            let ms = t.elapsed().as_millis();
            let after = band_amplitudes(&image::open(&out_png)?.to_rgba8(), &waves);
            let gains: Vec<f32> = before.iter().zip(&after).map(|(b, a)| a / b).collect();
            let cells: String = gains
                .iter()
                .map(|g| format!("{:>12}", format!("{g:.3}×")))
                .collect();
            println!("  {label:<15}{cells}{ms:>7}");
            results.push((size, label, gains));
        }
    }
    check_bands(&results)?;
    println!("\n[fsr-selftest] band shape OK: see check_bands for what that does and doesn't cover");
    Ok(())
}

/// One row of the band table: image size, the filter setting that produced it,
/// and the gain measured for each of [`BANDS`], in that order.
type BandRow = (u32, &'static str, Vec<f32>);

const BANDS: [&str; 4] = ["tone", "depth", "detail", "grit"];
const TONE: usize = 0;
const DEPTH: usize = 1;
const DETAIL: usize = 2;
const GRIT: usize = 3;

/// The properties the numbers have to keep, as distinct from the numbers.
///
/// Each check guards a way a filter could silently stop being what it claims;
/// none of them pins a value that is free to move. Tolerances are roughly 2×
/// the spread measured on a 7900 XTX on 2026-09-16, so an edit has room to
/// change the look without tripping them.
///
/// **RCAS is deliberately not checked for scale invariance**, though clarity is.
/// `rcas_radius` is a whole number of pixels, so it tracks the image in steps:
/// measured, the 3 px band at 1536² lifts 3.245×, and at 2048² — same radius,
/// bigger picture — the 4 px band lifts 2.481×, a 24% drift, while 3072² returns
/// to exactly 3.245× because the radius stepped up with the size. That sawtooth
/// is the design, not a regression, and asserting it away would be asserting
/// something false. Removing it means a fractional tap distance, which costs the
/// limiter its "ringing excluded by construction" guarantee — a separate,
/// measured decision, per `sharpen.wgsl`'s header.
fn check_bands(results: &[BandRow]) -> Result<()> {
    let mut bad: Vec<String> = Vec::new();
    let spread = |label: &str, band: usize| -> (f32, f32) {
        // Compared on `|gain - 1|` — the amount of band actually moved — so a
        // negative-clarity row is measured like a positive one instead of
        // showing a huge relative spread merely for landing near zero.
        let moved = results
            .iter()
            .filter(|(_, l, _)| *l == label)
            .map(|(_, _, g)| (g[band] - 1.0).abs());
        moved.fold((f32::MAX, 0.0f32), |(lo, hi), v| (lo.min(v), hi.max(v)))
    };

    for (size, label, g) in results {
        if label.starts_with("clarity") {
            // The whole reason it is a difference of two Gaussians rather than
            // `src - blur`: both ends of the spectrum come through untouched.
            for band in [TONE, GRIT] {
                if (g[band] - 1.0).abs() > 0.05 {
                    bad.push(format!(
                        "{size}² {label}: {} moved {:.3}×. Clarity passes the middle and \
                         rejects both ends — a lifted grit band means it has turned back \
                         into a high-pass, which is what contrast.wgsl's header warns about",
                        BANDS[band], g[band]
                    ));
                }
            }
        } else {
            // RCAS is the opposite shape: it may only reach the fine end.
            for (band, tol) in [(TONE, 0.02), (DEPTH, 0.02), (DETAIL, 0.10)] {
                if (g[band] - 1.0).abs() > tol {
                    bad.push(format!(
                        "{size}² {label}: {} moved {:.3}× (tolerance {tol}). RCAS is a \
                         fine-band sharpener; reaching this far down means the cross is \
                         sampling wider than intended",
                        BANDS[band], g[band]
                    ));
                }
            }
            if g[GRIT] < 2.0 {
                bad.push(format!(
                    "{size}² {label}: grit only {:.3}×. At the top of the knob the \
                     sharpening should be unmistakable — 1.0 is the reference filter at \
                     0 stops, so anything this weak means the lobe or the limiter broke",
                    g[GRIT]
                ));
            }
        }
    }

    // Scale invariance, clarity only — `clarity_sigmas` is smoothly proportional
    // to the image, so the same setting must move the same bands at every size.
    // Only depth and detail: tone and grit sit near zero by design, where a
    // ratio is noise and the absolute check above is the meaningful one.
    for label in ["clarity +0.6", "clarity -0.6"] {
        for band in [DEPTH, DETAIL] {
            let (lo, hi) = spread(label, band);
            if hi > lo * 1.30 {
                bad.push(format!(
                    "{label}: the {} band moves by {lo:.3} at one size and {hi:.3} at \
                     another. clarity_sigmas is proportional to the longest side, so a \
                     setting is supposed to mean the same thing at every size — see \
                     CLAUDE.md invariant 6",
                    BANDS[band]
                ));
            }
        }
    }

    if bad.is_empty() {
        return Ok(());
    }
    anyhow::bail!("band checks failed:\n  - {}", bad.join("\n  - "));
}

/// Mid-grey plus one sinusoid per wavelength, varying in x only — so a
/// separable blur along y does nothing and each band reads straight off a
/// column mean.
///
/// Amplitudes are deliberately small (0.03 each, 0.12 summed). `contrast.wgsl`
/// fades its effect toward black and white, so a big swing would be measuring
/// that fade as much as the filter; at this amplitude the protection term stays
/// within a few percent of 1, and the returned minimum says exactly how far.
fn grating(size: u32, waves: &[u32]) -> (image::RgbaImage, f32) {
    const AMP: f32 = 0.03;
    let mut protect_min = 1.0f32;
    let row: Vec<u8> = (0..size)
        .map(|x| {
            let l = 0.5
                + waves
                    .iter()
                    .map(|w| AMP * (std::f32::consts::TAU * x as f32 / *w as f32).sin())
                    .sum::<f32>();
            let t = (2.0 * l - 1.0).abs();
            protect_min = protect_min.min(1.0 - t * t);
            (l.clamp(0.0, 1.0) * 255.0).round() as u8
        })
        .collect();
    let mut img = image::RgbaImage::new(size, size);
    for y in 0..size {
        for x in 0..size {
            let v = row[x as usize];
            img.put_pixel(x, y, image::Rgba([v, v, v, 255]));
        }
    }
    (img, protect_min)
}

/// Single-bin DFT of the image's column means, one bin per wavelength.
///
/// Averaging down the rows first drops 8-bit quantisation noise by √height,
/// which at 1536 rows puts it far below the 0.03 amplitude being measured.
/// Every wavelength divides the width exactly, so each bin lands on a whole
/// number of cycles and no band leaks into its neighbour — which the old
/// hand-measured table's 400 px and 40 px, on a 1536 px image, did not.
fn band_amplitudes(img: &image::RgbaImage, waves: &[u32]) -> Vec<f32> {
    let (w, h) = img.dimensions();
    let mut means = vec![0.0f64; w as usize];
    for y in 0..h {
        for x in 0..w {
            means[x as usize] += img.get_pixel(x, y).0[1] as f64 / 255.0;
        }
    }
    for m in &mut means {
        *m /= h as f64;
    }
    waves
        .iter()
        .map(|lambda| {
            let (mut re, mut im) = (0.0f64, 0.0f64);
            for (x, v) in means.iter().enumerate() {
                let phase = std::f64::consts::TAU * x as f64 / *lambda as f64;
                re += v * phase.cos();
                im += v * phase.sin();
            }
            (2.0 * re.hypot(im) / w as f64) as f32
        })
        .collect()
}

/// Minimal JSON string escaper.
fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::{band_amplitudes, check_bands, grating, BandRow, DEPTH, DETAIL, GRIT};

    /// The measurement harness, measured against a signal whose answer is
    /// known: every planted band must read back at the amplitude it was
    /// planted at, and no band may pick up its neighbours.
    ///
    /// Worth a test precisely because `fsr-selftest` prints these numbers
    /// without judging them. A DFT that is out by a factor of two, or that
    /// leaks between bins, produces a table that looks entirely plausible and
    /// says nothing true — and needs no GPU to catch, which is what this file's
    /// other test is for.
    /// `check_bands` against the real table `fsr-selftest` printed on a
    /// 7900 XTX on 2026-09-16 — the run the tolerances were drawn from. It has
    /// to pass, and each way a filter could break has to fail it, or the
    /// tolerances are decoration.
    ///
    /// Pinned here rather than left to the GPU command so the thresholds can be
    /// retuned on any machine: `cargo test` says whether a bound still admits
    /// the known-good data and still rejects the known-bad shapes.
    #[test]
    fn band_checks_accept_measured_and_reject_regressions() {
        // size, label, [tone, depth, detail, grit]
        let measured: Vec<BandRow> = vec![
            (1536, "clarity +0.6", vec![1.013, 1.402, 1.608, 0.997]),
            (1536, "clarity -0.6", vec![0.987, 0.598, 0.392, 1.003]),
            (1536, "sharpness 1.0", vec![1.000, 1.000, 1.017, 3.245]),
            (2048, "clarity +0.6", vec![1.022, 1.412, 1.654, 0.996]),
            (2048, "clarity -0.6", vec![0.978, 0.588, 0.346, 1.004]),
            (2048, "sharpness 1.0", vec![0.999, 1.001, 1.009, 2.481]),
            (3072, "clarity +0.6", vec![1.018, 1.406, 1.681, 0.996]),
            (3072, "clarity -0.6", vec![0.982, 0.594, 0.319, 1.004]),
            (3072, "sharpness 1.0", vec![1.000, 1.000, 1.017, 3.245]),
        ];
        check_bands(&measured).expect("the measured table must pass");

        // The RCAS plateau drift is in that data (3.245 / 2.481 / 3.245) and
        // must not trip anything — it is the design, and checking it would be
        // checking something false.

        let broken = |f: &dyn Fn(&mut Vec<BandRow>)| {
            let mut m = measured.clone();
            f(&mut m);
            check_bands(&m).unwrap_err().to_string()
        };

        // Clarity turned back into a high-pass: it lifts the grit band.
        let e = broken(&|m| m[0].2[GRIT] = 1.4);
        assert!(e.contains("grit"), "{e}");

        // Clarity stopped scaling with the image — invariant 6 gone.
        let e = broken(&|m| m[6].2[DETAIL] = 3.0);
        assert!(e.contains("invariant 6"), "{e}");

        // RCAS reaching down into the structural bands.
        let e = broken(&|m| m[2].2[DEPTH] = 1.5);
        assert!(e.contains("fine-band sharpener"), "{e}");

        // The knob gone limp — a limiter or lobe mistake.
        let e = broken(&|m| m[2].2[GRIT] = 1.2);
        assert!(e.contains("unmistakable"), "{e}");
    }

    #[test]
    fn grating_amplitudes_round_trip() {
        let size = 1536u32;
        let waves: Vec<u32> = [1u32, 4, 32, 512].iter().map(|d| size / d).collect();
        let (img, protect_min) = grating(size, &waves);

        // 8-bit quantisation is the only error here, and averaging 1536 rows of
        // an identical column leaves it as a plain rounding residue.
        for (w, a) in waves.iter().zip(band_amplitudes(&img, &waves)) {
            assert!(
                (a - 0.03).abs() < 0.002,
                "{w}px band planted at 0.030, measured {a:.4}"
            );
        }

        // A band that isn't there must measure as absent, or the bins leak and
        // every number in the table is a blend of its neighbours.
        let absent = size / 128;
        assert!(
            band_amplitudes(&img, &[absent])[0] < 0.002,
            "unplanted {absent}px band should read ~0"
        );

        // The amplitudes are chosen to keep contrast.wgsl's midtone protection
        // near 1, so the printed gains are the filter and not the fade.
        assert!(protect_min > 0.9, "protect floor {protect_min:.3} is too low");
    }
}
