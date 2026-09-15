//! rustimage — Python-free Vulkan image generator.
//!
//! A minimal std-only HTTP server fronts a single GPU worker thread. The worker
//! owns the sd.cpp runner and the wgpu FSR context, so all GPU work is
//! serialized (no VRAM thrash). Routes:
//!   GET  /            -> static web UI
//!   GET  /templates   -> JSON list of model profiles
//!   POST /generate    -> {prompt, template, negative?, seed?, quality?}
//!                        -> {image_base64, width, height, seed, refine_id, timings}
//!   POST /refine      -> {refine_id}  (refine a previous unrefined result)
//!                        -> same shape, with refine_id null

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
    /// Refine a base image the engine retained from an earlier generation.
    Refine(u64),
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
            println!("[rustimage] FSR post-process: upscale=lanczos2, sharpen=cas (interim FSR path)");
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
        quality: Option<bool>,
        #[serde(default)]
        steps: Option<u32>,
        #[serde(default)]
        cfg_scale: Option<f32>,
        #[serde(default)]
        sampler: Option<String>,
        #[serde(default)]
        width: Option<u32>,
        #[serde(default)]
        height: Option<u32>,
    }

    let dto: Dto = match serde_json::from_slice(body) {
        Ok(d) => d,
        Err(e) => {
            return conn.respond(
                400,
                "application/json",
                error_json(&format!("bad request: {e}")).as_bytes(),
            );
        }
    };

    let req = GenRequest {
        prompt: dto.prompt,
        template: dto.template,
        negative: dto.negative,
        seed: dto.seed.unwrap_or(-1),
        quality: dto.quality.unwrap_or(false),
        steps: dto.steps,
        cfg_scale: dto.cfg_scale,
        sampler: dto.sampler,
        width: dto.width,
        height: dto.height,
    };

    submit(conn, job_tx, JobKind::Generate(req), 500)
}

/// Refine an earlier unrefined result, identified by the `refine_id` that
/// generation returned. An unknown id means the base image is gone (evicted or
/// the server restarted), which is a client-side staleness problem — 400, not 500.
fn handle_refine(conn: &mut http::Connection, job_tx: &Sender<Job>, body: &[u8]) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct Dto {
        refine_id: u64,
    }

    let dto: Dto = match serde_json::from_slice(body) {
        Ok(d) => d,
        Err(e) => {
            return conn.respond(
                400,
                "application/json",
                error_json(&format!("bad request: {e}")).as_bytes(),
            );
        }
    };

    submit(conn, job_tx, JobKind::Refine(dto.refine_id), 400)
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

/// The image plus its metadata as JSON. `refine_id` is null when the result
/// can't be refined further, which is what the UI keys the Refine button off.
fn result_json(res: &GenResult) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(&res.png);
    let refine_id = match res.refine_id {
        Some(id) => id.to_string(),
        None => "null".to_string(),
    };
    format!(
        "{{\"image_base64\":\"{}\",\"width\":{},\"height\":{},\"seed\":{},\"refined\":{},\
         \"refine_id\":{},\"timings\":{{\"gen_ms\":{},\"fsr_ms\":{},\"refine_ms\":{}}}}}",
        b64,
        res.width,
        res.height,
        res.seed,
        res.refined,
        refine_id,
        res.timings.gen_ms,
        res.timings.fsr_ms,
        res.timings.refine_ms,
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
                 \"steps\":{},\"cfg_scale\":{},\"sampler\":{},\"can_refine\":{}}}",
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
                json_str(&p.sampler),
                p.refine_steps > 0,
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

/// `rustimage fsr <in.png> <out.png> [upscale_factor] [sharpness]`
/// Runs only the FSR upscale+sharpen passes on an existing image.
fn run_fsr_cli(args: &[String]) -> Result<()> {
    if args.len() < 2 {
        eprintln!("usage: rustimage fsr <in.png> <out.png> [upscale_factor=2.0] [sharpness=0.35]");
        std::process::exit(2);
    }
    let in_png = PathBuf::from(&args[0]);
    let out_png = PathBuf::from(&args[1]);
    let factor: f32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(2.0);
    let sharp: f32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0.35);

    let fsr = fsr::Fsr::new()?;
    println!("[fsr] adapter: {}", fsr.adapter_name());
    let start = std::time::Instant::now();
    let (w, h) = fsr.process(&in_png, &out_png, factor, sharp)?;
    println!(
        "[fsr] {} -> {} : {}x{} (x{factor}, sharpness {sharp}) in {} ms",
        in_png.display(),
        out_png.display(),
        w,
        h,
        start.elapsed().as_millis()
    );
    Ok(())
}

/// `rustimage fsr-selftest [out.png]`
/// Synthesizes a test pattern, runs FSR, and verifies the output dimensions.
/// Exercises the full wgpu path (device init, both compute passes, readback)
/// without needing sd.cpp or a checkpoint.
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
    let (w, h) = fsr.process(&in_png, &out_png, 2.0, 0.4)?;
    let expected = (size * 2, size * 2);
    if (w, h) != expected {
        anyhow::bail!("expected {expected:?}, got {:?}", (w, h));
    }
    println!(
        "[fsr-selftest] OK: {}x{} ({} -> {})",
        w,
        h,
        in_png.display(),
        out_png.display()
    );
    Ok(())
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
