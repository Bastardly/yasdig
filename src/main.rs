//! yasdig — Python-free Vulkan image generator.
//!
//! A minimal std-only HTTP server fronts two worker threads. The diffusion
//! worker owns the sd.cpp runner and takes its work from the job queue
//! (`jobs.rs`) one image at a time, so sd-cli never runs twice at once (no VRAM
//! thrash). The display worker re-filters, reopens and saves images, so the
//! viewer answers mid-render; the two share one wgpu FSR context behind a lock.
//! Routes:
//!   GET  /            -> static web UI
//!   GET  /templates   -> JSON list of model profiles
//!   POST /generate    -> {prompt, template, negative?, seed?, ...overrides,
//!                         count?, loop?, after?: {hires?, refine?, save}}
//!                        -> {job}, at once. The images arrive through /jobs
//!   GET  /jobs        -> {jobs:[{id, kind, label, done, total, state}],
//!                         latest_seq, notices, progress}
//!   GET  /jobs/latest -> the newest image a job made: {image_base64, width,
//!                         height, seed, image_id, can_hires, timings}
//!   POST /jobs/stop   -> {id}  remove a waiting job, or stop a running one
//!   POST /jobs/skip   -> {id}  cancel the image a run has in flight; the run
//!                        goes on to its next image
//!   POST /upload      -> a PNG as the raw body (the one binary route)
//!                        -> {stem, width, height}, landing in work/output/
//!   POST /img2img     -> {prompt, template, source, ...overrides, count?, ...}
//!                        img2img passes over a work/output/ image, queued like
//!                        /generate's so the same ladder follows them
//!   POST /refine      -> {image_id, ...overrides}   img2img in place, no rescale
//!   POST /redraw     -> {image_id, mask, prompt?, strength?}  redraw the
//!                        painted part; `mask` is a base64 PNG, white = redraw
//!   POST /edit        -> {image_id, prompt, references?, expand?}  change the
//!                        image by instruction, the image itself as reference 1;
//!                        `expand: {left, top, right, bottom}` redraws it bigger
//!   POST /extend      -> {image_id, expand, prompt?, feather?}  grow the canvas
//!                        and draw past its edges with FLUX.1 Fill
//!   POST /upscale     -> {image_id, factor, upscaler?}  enlarge with FSR or an
//!                        upscaler model, no diffusion; a history step
//!   POST /history/goto -> {image_id, index}  step back (or forward) to an
//!                        earlier state of the image; every pass is a step
//!   GET  /history/{id}/{index}.png   a step's thumbnail
//!   POST /history/view -> {image_id, index}  a step as shown now, with today's
//!                        filters, for the before/after compare; changes nothing
//!   POST /hires       -> {image_id, upscale, ...overrides}
//!                        img2img with an enlarge first. Both queue ahead of
//!                        everything not yet running, wait their turn and answer
//!                        in /jobs/latest's shape; both can be run in either order and
//!                        repeated, and both take the escalation knobs — omitted
//!                        means "as the image was generated". `model` names a
//!                        different checkpoint for that one pass
//!   POST /adjust      -> {image_id, sharpness?, clarity?}
//!                        re-runs the display filters only — no diffusion, so
//!                        it's cheap enough to drive from a slider
//!   GET  /workitems   -> every archived generation, newest first, settings and all
//!   GET  /workitems/{id}/thumb.png
//!   POST /workitems/open   -> {id}       reopen one as a live image_id
//!   POST /workitems/delete -> {ids:[..]} remove them from disk
//!   POST /favorites/add    -> {ids:[..]} move them to user/favorite/, so
//!                        "delete all" on the grid can't reach them
//!   POST /favorites/remove -> {ids:[..]} move them back to work/workitems/
//!   GET  /favorites   -> user/favorite/ alone (/workitems lists both, with
//!                        `favorite` saying which shelf each lives on)
//!   GET  /favorites/{id}/thumb.png
//!   POST /favorites/open   -> {id}
//!   POST /favorites/delete -> {ids:[..]}
//!   GET  /loras       -> every file in the LoRA dir, with why an unusable one
//!                        can't be picked. Attach them per request with a
//!                        `loras: [{name, strength}]` on /generate or /img2img
//!   GET  /upscalers   -> every file in the upscaler dir, with its architecture
//!                        and whether it can run. Pick one per generate with
//!                        `upscaler: "<name>"`; `""` is FSR
//!   GET  /outputs     -> every image in work/output/ and the favourites,
//!                        newest first, each with `favorite`
//!   POST /outputs/open -> {stem, model?} open a saved image as a live
//!                        image_id, to adjust and refine; `model` stands in
//!                        when its sidecar names none installed
//!   POST /resize      -> {image_id, factor}  scale it down, a History step
//!   POST /image/model -> {image_id, model}  the model an image's passes run on
//!   POST /outputs/delete -> {stems:[..]} remove them from work/output/
//!   POST /favorite-outputs/add -> {stems:[..]} move them to user/favorite_outputs/
//!   POST /favorite-outputs/remove -> {stems:[..]} move them back to work/output/
//!   GET  /favorite-outputs     -> the same listing, read from there
//!   POST /favorite-outputs/delete -> {stems:[..]}
//!                        (a favourite opens, starts a generation and gets its
//!                        thumbnail through the /outputs routes, by stem)
//!   GET  /video-models -> the [[video]] table, with each family's grid rules
//!   POST /video       -> generate a clip (minutes; watch GET /progress)
//!   GET  /videos      -> every archived clip, newest first
//!   GET  /videos/{id}/thumb.png
//!   GET  /videos/{id}/frames/{n}.png   one frame; the page plays the sequence
//!   POST /videos/enlarge -> {id, upscale} FSR over every frame
//!   POST /videos/delete  -> {ids:[..]}
//!   GET  /outputs/{stem}/thumb.png   generated on first request
//!   POST /save-output      -> {image_id} keep the image on screen
//!   POST /outputs/overwrite -> {image_id} save it over the output it was opened from
//!   POST /png-settings -> raw PNG bytes; the settings it carries ({source, notes, item|params})
//!   GET  /progress    -> what the worker is rendering right now, step by step
//!   GET  /preview/{n}.png  the live preview of the run in flight; `n` is
//!                     the `preview` number /progress and /jobs report
//!   POST /cancel      -> kill the run in flight (the only stop sd-cli offers)
//!   POST /shutdown    -> stop the run, clear what the settings say, and exit
//!
//! The archive listing, the thumbnail, the delete, and both of the last two are
//! answered on the connection thread rather than the GPU worker. The first
//! three only touch the filesystem, and queueing a thumbnail behind a
//! multi-minute diffusion would freeze the grid for the whole render; the last
//! two are *about* the render in flight, so going through the queue would make
//! them arrive only once there was nothing left to report or to stop.

mod area;
mod avi;
mod brush;
mod colorrange;
mod bundle;
mod config;
mod catalog;
mod convert;
mod discover;
mod downloads;
mod edits;
mod foldscale;
mod half;
mod fsr;
mod home;
mod http;
mod jobs;
mod pipeline;
mod pngmeta;
mod redraw;
mod preflight;
mod sd;
mod settings;
mod shutdown;
mod startup;
mod sr;
mod variables;
mod videos;
mod web;
mod workitems;

use anyhow::{Context, Result};
use base64::Engine as _;
use config::{Config, Filters, FiltersPatch, LoraRef};
use pipeline::{
    lock, Engine, EscalateRequest, GenRequest, GenResult, Img2ImgRequest, Retained, VideoRequest,
};
use sd::Monitor;
use settings::Settings;
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use workitems::{Gallery, Shelf};

/// A unit of display-lane work plus a channel to send the result back to the
/// HTTP thread.
///
/// The reply is the finished JSON body rather than a `GenResult`, because not
/// every job produces an image — `SaveOutput` answers with a path. Rendering on
/// the worker costs nothing (it is already holding the only copy of the data) and
/// keeps `submit` indifferent to what the job was.
struct Job {
    kind: JobKind,
    reply: Sender<Result<String>>,
}

/// The display lane: everything that only re-filters, reopens or copies an
/// image. None of it runs sd-cli, so none of it has any business waiting behind
/// a render — which is what it used to do, and why the viewer could not be
/// opened while a batch ran. See [`pipeline::Display`].
enum JobKind {
    /// Re-filter an earlier result — display passes only, no diffusion.
    Adjust { id: u64, patch: FiltersPatch, area: Option<Option<area::Area>> },
    /// Crop the image to a rectangle, as a history step (`Display::crop`).
    Crop { id: u64, rect: (u32, u32, u32, u32) },
    /// Scale the image down (`Display::resize`): Scale's "down" half.
    Resize { id: u64, factor: f32 },
    /// Make the look part of the picture, as a history step
    /// (`Display::apply_look`).
    Apply(u64),
    /// Lay a text layer over the picture as a step (`Display::text`).
    Text { id: u64, layer: Vec<u8>, label: String },
    /// Paint with a brush tool or change its amount (`Display::brush`).
    Brush { id: u64, tool: brush::Tool, amount: Option<f32>, mask: Option<Option<Vec<u8>>>, from: Option<usize> },
    /// Reopen an archived generation as a live result. Needs a worker because
    /// it ends in a `publish`, and because the new `Pending` lives there. The
    /// shelf rides along because the two grids — Current and Favorites — hold
    /// the same ids, so the id alone doesn't say which file to read.
    Open {
        id: String,
        shelf: Shelf,
    },
    /// Reopen a finished image from `work/output/` as a live result in the
    /// Outputs tab's slot (`Slot::Output`; `fallback` the model to refine it
    /// with when its sidecar names none installed). Through a worker for the
    /// same two reasons as `Open`, and unlike every other `/outputs` route,
    /// which only reads the filesystem.
    OpenOutput { stem: String, fallback: Option<String> },
    /// Copy the image on screen into `work/output/`. No GPU, but the only copy
    /// of which file that is lives in the retained images.
    SaveOutput(u64),
    /// Write the image on screen back over the saved file it was opened from
    /// (`Display::overwrite_output`) — the Outputs viewer's *Save changes*.
    OverwriteOutput(u64),
    /// The image on screen as a file for the browser to save
    /// (`Display::download`): `display` with its settings, answered as base64.
    DownloadImage(u64),
    /// Step to another state in an image's history (`Display::goto`). A
    /// `publish`, so the display lane's — it answers mid-render.
    Goto { id: u64, index: usize },
    /// Choose the model an image's passes run on (`Display::set_model`).
    SetModel { id: u64, model: String },
    /// A step as the page would show it, for the before/after compare
    /// (`Display::view_step`). Reads only.
    ViewStep { id: u64, index: usize },
}

/// The diffusion lane's work, as the job queue (`crate::jobs`) carries it.
/// Everything here runs sd-cli or holds the GPU for long enough that two at
/// once would fight over VRAM, so it is serialized and visible in the Jobs
/// dialog.
#[derive(Clone)]
enum Work {
    /// A generate or an image-to-image, one image or many, with what to do
    /// after each. Boxed: it is the large variant by an order of magnitude.
    Run(Box<RunPlan>),
    /// Refine the base image the engine retained from an earlier generation.
    /// The image is pinned while this waits — see `pipeline::Retained::pin`.
    Refine { id: u64, over: EscalateRequest },
    /// Re-diffuse an earlier result at output resolution, optionally enlarging.
    Hires {
        id: u64,
        upscale: f32,
        over: EscalateRequest,
    },
    /// Redraw the painted part of a retained image (`Engine::redraw`). Pinned
    /// while it waits, like the two above.
    Redraw { id: u64, req: pipeline::RedrawRequest },
    /// Change a retained image by instruction (`Engine::edit`). Pinned while
    /// it waits, like the passes above.
    Edit { id: u64, req: pipeline::EditRequest },
    /// Grow a retained image's canvas with FLUX.1 Fill (`Engine::extend`).
    /// Pinned while it waits, like the passes above.
    Extend { id: u64, req: pipeline::ExtendRequest },
    /// Enlarge a retained image with no diffusion (`Engine::upscale`). Pinned
    /// while it waits, like the passes above.
    Upscale {
        id: u64,
        factor: f32,
        upscaler: Option<String>,
    },
    /// Generate a clip. Minutes long rather than seconds, which is what made
    /// `SdRunner::run` stream its output — `GET /progress` is the only thing
    /// that makes this bearable, and it answers on the connection thread while
    /// this job holds the worker.
    Video(VideoRequest),
    /// Requantize a model file. The odd one out: no GPU at all, and the reason
    /// it queues is that it saturates the cores and the disk for minutes and
    /// must not do that underneath a render. Going through the worker is also
    /// what gives it `/progress` and `/cancel` for free.
    Convert(convert::Plan),
    /// FSR over every frame of a finished clip. GPU work, so it belongs to the
    /// worker even though there is no diffusion in it.
    EnlargeClip {
        id: String,
        upscale: f32,
        sharpness: Option<f32>,
        clarity: Option<f32>,
    },
}

impl Work {
    /// The retained image this job pins while it waits, if any. One list for
    /// both places that let a pin go — the worker finishing the job and Stop
    /// removing it — and no `_` arm, so a new kind has to say.
    fn pinned(&self) -> Option<u64> {
        match self {
            Work::Refine { id, .. }
            | Work::Hires { id, .. }
            | Work::Redraw { id, .. }
            | Work::Edit { id, .. }
            | Work::Extend { id, .. }
            | Work::Upscale { id, .. } => Some(*id),
            Work::Run(_) | Work::Video(_) | Work::Convert(_) | Work::EnlargeClip { .. } => None,
        }
    }
}

/// One run: what makes each image, and what happens to it afterwards.
///
/// Everything is captured when the run is queued. That is what a queue means —
/// job A keeps the boxes it was queued with while job B is set up. **The loop
/// is the exception**: there is only ever one, so "read the boxes once per
/// image" still makes sense for it, and the page re-sends them on every change
/// (`Queue::push_loop`, `syncLoop` in image.js).
#[derive(Clone)]
struct RunPlan {
    start: Start,
    after: AfterEach,
    /// The escalation knobs from the same body, for the after-each passes —
    /// what the Refine and Hi-res buttons would send with the panel as it was.
    escalate: EscalateRequest,
}

#[derive(Clone)]
enum Start {
    Generate(GenRequest),
    Img2Img(Img2ImgRequest),
}

/// The *After each generation* fold. Hi-res before refine, the order the
/// buttons are in: hi-res is the pass that enlarges, refine is the finishing
/// touch you run at whatever size you ended up at.
#[derive(Clone, Default, serde::Deserialize)]
struct AfterEach {
    #[serde(default)]
    hires: Option<AfterPass>,
    #[serde(default)]
    refine: Option<AfterPass>,
    #[serde(default)]
    save: bool,
}

#[derive(Clone, Default, serde::Deserialize)]
struct AfterPass {
    /// The checkpoint for this pass; empty or absent is the one that made the
    /// image, as on the buttons.
    #[serde(default)]
    model: Option<String>,
    /// Hi-res only.
    #[serde(default)]
    upscale: Option<f32>,
}

type Jobs = jobs::Queue<Work>;

/// What a connection thread needs to reach the two workers.
#[derive(Clone)]
struct Lanes {
    display: Sender<Job>,
    queue: Jobs,
    retained: Arc<Mutex<Retained>>,
    /// The Get models tab's lane — curl, no GPU, never the job queue.
    downloads: downloads::Downloads,
}

/// The most images one run may ask for. Not a resource limit — the loop is
/// unbounded — but a typo guard: 10000 in the count box is a mistake, and the
/// loop is the way to ask for "many".
const MAX_RUN: u32 = 1000;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let double_clicked = args.len() == 1;
    let result = run(&args);
    // A double-clicked console closes the moment the process exits, taking the
    // one message that said what to fix with it. Windows only: a Linux file
    // manager starts it with no terminal at all, and in a terminal the message
    // stays on screen by itself.
    if let Err(e) = &result {
        if double_clicked && cfg!(windows) {
            eprintln!("[yasdig] {e:#}");
            eprintln!("\nPress Enter to close.");
            let _ = std::io::stdin().read_line(&mut String::new());
            std::process::exit(1);
        }
    }
    result
}

fn run(args: &[String]) -> Result<()> {
    // First, before a minute of preflight and GPU start in which `cargo` may
    // already be rebuilding the file — see `startup::build_id`.
    startup::build_id();

    // Subcommands that run without the server. `fsr`, `fsr-selftest` and
    // `sr-selftest` exercise the wgpu path alone; `upscale` adds sd-cli for an
    // ESRGAN but still needs no checkpoint.
    match args.get(1).map(String::as_str) {
        Some("fsr") => return run_fsr_cli(&args[2..]),
        Some("fsr-selftest") => return run_fsr_selftest(&args[2..]),
        Some("sr-selftest") => return run_sr_selftest(),
        Some("doctor") => return run_doctor(&args[2..]),
        Some("upscale") => return run_upscale_cli(&args[2..]),
        Some("fold-scales") => return run_fold_scales(&args[2..]),
        _ => {}
    }

    let config_path = home::config_path(args.get(1).map(String::as_str))?;

    let cfg = Arc::new(
        Config::load(&config_path)
            .with_context(|| format!("loading {}", config_path.display()))?,
    );

    // Settings for models that are gone are set aside, and brought back for
    // ones that returned, before anything reads them. Said once, here, rather
    // than as a warning on every start that nobody can act on but by hand.
    let registry = cfg.registry();
    match settings::ModelSettings::reconcile(&cfg.user_dir(), |n| registry.contains_key(n)) {
        Ok((parked, restored)) => {
            let removed = settings::ModelSettings::parked_path(&cfg.user_dir());
            for name in parked {
                println!(
                    "[yasdig] warning: model '{name}' no longer exists, so its saved settings \
                     were removed (kept in {} — they come back if the model does)",
                    removed.display()
                );
            }
            for name in restored {
                println!("[yasdig] model '{name}' is back — its saved settings are restored");
            }
        }
        Err(e) => eprintln!("[yasdig] warning: couldn't tidy per-model settings: {e:#}"),
    }

    // Validate the environment before touching the GPU, so missing prerequisites
    // produce clear, actionable instructions instead of a mid-request failure.
    let report = preflight::check(&cfg);
    if !report.print() {
        anyhow::bail!("fix the above and start yasdig again (`yasdig doctor` re-checks)");
    }

    // Before the GPU worker, not after it: `Engine::new` sweeps `work/`, and a
    // second copy of *this folder* started by a second double-click would do
    // that under the feet of the first one's render. `startup::bind` is the
    // rule for a taken port: hand over to this same folder and build, close any
    // other yasdig and take its port, and step past anything else.
    let startup = startup::load(&cfg.base_dir, cfg.legacy_server.as_ref())?;
    let me = startup::Instance {
        home: cfg.base_dir.display().to_string(),
        build: startup::build_id().to_string(),
    };
    let (listener, bind) = match startup::bind(&startup.server, &me)? {
        startup::Bound::Listening(l, bind) => (l, bind),
        startup::Bound::HandedOver(bind) => {
            let url = format!("http://{bind}/");
            println!("[yasdig] already running at {url} — opening it");
            open_browser(&url);
            return Ok(());
        }
    };
    let url = format!("http://{bind}/");
    // A checkout's `web/` when there is one, so edits show on reload; the
    // copy built in otherwise. See `web::serve`.
    let web_dir = Some(cfg.base_dir.join("web")).filter(|d| d.is_dir());

    // Shared with every connection thread: the worker publishes progress into
    // it, a Stop reaches the running child through it, and its runs write the
    // live preview `GET /preview/{n}.png` serves.
    let monitor = Monitor::with_preview(pipeline::preview_dir(&cfg.base_dir.join("work")));

    // The diffusion worker. It initializes wgpu + sd, reports readiness, then
    // takes work from the job queue one image at a time. The display worker is
    // started from what it hands back — the same FSR device and the same
    // retained images — so the two lanes share one GPU context.
    let engine_cfg = (*cfg).clone();
    let engine_monitor = monitor.clone();
    let queue = Jobs::new();
    let worker_queue = queue.clone();
    let (ready_tx, ready_rx) = mpsc::channel::<Result<(String, pipeline::Display, Arc<Mutex<Retained>>)>>();
    std::thread::Builder::new()
        .name("gpu-worker".into())
        .spawn(move || match Engine::new(engine_cfg, engine_monitor) {
            Ok(mut engine) => {
                let saver = engine.display();
                let _ = ready_tx.send(Ok((engine.adapter_name(), engine.display(), engine.retained())));
                loop {
                    let ticket = worker_queue.next();
                    let id = ticket.id;
                    // A panic here used to end the thread with the job still
                    // marked running: its request waited for ever, and so did
                    // everything queued behind it. Caught, it fails that one
                    // job and the worker takes the next — a wgpu validation
                    // panic on an oversized upscale was the case that found
                    // it. A pinned image the job held stays pinned (its files
                    // go with the next start), which is the price of not
                    // trusting state a panic left half-written.
                    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        diffuse(&mut engine, &saver, &worker_queue, ticket)
                    }));
                    if let Err(payload) = caught {
                        let why = payload
                            .downcast_ref::<String>()
                            .cloned()
                            .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
                            .unwrap_or_else(|| "unknown cause".into());
                        eprintln!("[yasdig] GPU worker caught a panic in job {id}: {why}");
                        worker_queue.abort(id, format!("the GPU worker failed on this job: {why}"));
                    }
                }
            }
            Err(e) => {
                let _ = ready_tx.send(Err(e));
            }
        })
        .context("spawning gpu worker")?;

    let (display, retained) = match ready_rx.recv().context("gpu worker died during startup")? {
        Ok((name, display, retained)) => {
            println!("[yasdig] GPU worker ready on adapter: {name}");
            println!(
                "[yasdig] GPU post-process: upscale=easu, grade=oklab, contrast=clarity, \
                 sharpen=rcas"
            );
            (display, retained)
        }
        Err(e) => return Err(e.context("startup failed")),
    };

    let (display_tx, display_rx) = mpsc::channel::<Job>();
    std::thread::Builder::new()
        .name("display-worker".into())
        .spawn(move || {
            let mut display = display;
            for job in display_rx {
                let result = match job.kind {
                    JobKind::Adjust { id, patch, area } => {
                        display.adjust(id, &patch, area).map(|r| result_json(&r))
                    }
                    JobKind::Crop { id, rect } => display.crop(id, rect).map(|r| result_json(&r)),
                    JobKind::Resize { id, factor } => display.resize(id, factor).map(|r| result_json(&r)),
                    JobKind::Apply(id) => display.apply_look(id).map(|r| result_json(&r)),
                    JobKind::Text { id, layer, label } => display.text(id, &layer, &label).map(|r| result_json(&r)),
                    JobKind::Brush { id, tool, amount, mask, from } => {
                        display.brush(id, tool, amount, mask, from).map(|r| result_json(&r))
                    }
                    JobKind::Open { id, shelf } => {
                        display.open(&id, shelf).map(|r| result_json(&r))
                    }
                    JobKind::OpenOutput { stem, fallback } => {
                        display.open_output(&stem, fallback.as_deref()).map(|r| result_json(&r))
                    }
                    JobKind::SaveOutput(id) => display.save_output(id).map(|p| saved_json(&p)),
                    JobKind::OverwriteOutput(id) => display.overwrite_output(id).map(|p| saved_json(&p)),
                    JobKind::DownloadImage(id) => display.download(id).map(|(png, name)| {
                        serde_json::json!({
                            "image_base64": base64::engine::general_purpose::STANDARD.encode(&png),
                            "name": name,
                        })
                        .to_string()
                    }),
                    JobKind::Goto { id, index } => display.goto(id, index).map(|r| result_json(&r)),
                    JobKind::SetModel { id, model } => display.set_model(id, &model).map(|r| result_json(&r)),
                    JobKind::ViewStep { id, index } => display.view_step(id, index).map(|(png, w, h)| {
                        format!(
                            "{{\"image_base64\":\"{}\",\"width\":{w},\"height\":{h}}}",
                            base64::engine::general_purpose::STANDARD.encode(&png)
                        )
                    }),
                };
                let _ = job.reply.send(result);
            }
        })
        .context("spawning display worker")?;
    let lanes = Lanes {
        display: display_tx,
        queue,
        retained,
        downloads: downloads::Downloads::start(&cfg).context("spawning the download worker")?,
    };

    println!("[yasdig] listening on {url}");
    if std::env::var_os("YASDIG_NO_BROWSER").is_none() {
        open_browser(&url);
    }
    println!("[yasdig] to stop: Close server on the page, Ctrl+C, or close this window");

    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(_) => continue,
        };
        let lanes = lanes.clone();
        let cfg = cfg.clone();
        let web_dir = web_dir.clone();
        let monitor = monitor.clone();
        let bound = startup.server.host.clone();
        std::thread::spawn(move || {
            if let Err(e) = handle_conn(stream, &lanes, &cfg, web_dir.as_deref(), &monitor, &bound) {
                eprintln!("[yasdig] connection error: {e:#}");
            }
        });
    }

    Ok(())
}

/// Run one ticket from the job queue on the diffusion worker, and settle it
/// with the queue: one image of a run, or a whole indivisible job.
fn diffuse(engine: &mut Engine, saver: &pipeline::Display, queue: &Jobs, t: jobs::Ticket<Work>) {
    // A pass on a retained image: published as the latest result, its pin let
    // go before the request is answered, whatever the outcome.
    let pinned = t.work.pinned();
    let retained = engine.retained();
    let finish = |r: Result<GenResult>| {
        // Only the Image tab's picture is "the latest result": a pass on one
        // opened from Outputs answers its own request and nothing else, or
        // the Image tab would pick it up as its own.
        let r = r.map(|r| match lock(&retained).slot_of(r.image_id) {
            pipeline::Slot::Image => latest(queue, &r),
            _ => result_json(&r),
        });
        if let Some(id) = pinned {
            lock(&retained).unpin(id);
        }
        queue.finish(t.id, r);
    };
    match t.work {
        Work::Run(plan) => match run_image(engine, saver, queue, &plan, t.done) {
            Ok(()) => {
                queue.image_done(t.id);
            }
            // *Cancel generation* killed this image on purpose: the run goes
            // on to its next one, with no notice — the person asked for it.
            Err(_) if queue.skipped(t.id) => {
                queue.image_done(t.id);
            }
            Err(e) => queue.fail_run(t.id, format!("{e:#}"), sd::oom_json(&e)),
        },
        Work::Refine { id, over } => finish(engine.refine(id, &over)),
        Work::Hires { id, upscale, over } => finish(engine.hires(id, upscale, &over)),
        Work::Edit { id, req } => finish(engine.edit(id, &req)),
        Work::Upscale { id, factor, upscaler } => finish(engine.upscale(id, factor, upscaler)),
        Work::Redraw { id, req } => finish(engine.redraw(id, &req)),
        Work::Extend { id, req } => finish(engine.extend(id, &req)),
        Work::Video(req) => queue.finish(t.id, engine.video(&req).and_then(|r| json_body(&r))),
        Work::Convert(plan) => {
            queue.finish(t.id, engine.convert(&plan).and_then(|r| json_body(&r)))
        }
        Work::EnlargeClip {
            id,
            upscale,
            sharpness,
            clarity,
        } => queue.finish(
            t.id,
            engine
                .enlarge_clip(
                    &id,
                    upscale,
                    &Filters::starting_at(sharpness.unwrap_or(0.0), clarity.unwrap_or(0.0)),
                )
                .and_then(|r| json_body(&r)),
        ),
    }
}

/// Render an image result and publish it as the latest one, so the page shows
/// it wherever it was when the job was queued — including after a reload.
fn latest(queue: &Jobs, r: &GenResult) -> String {
    let json = result_json(r);
    queue.set_latest(json.clone());
    json
}

/// One image of a run, start to finish: the same calls in the same order the
/// page's run loop used to make — generate, then hi-res, refine and save as the
/// *After each* fold asked. Each stage is published as it lands, as the page
/// used to render it.
///
/// The image is pinned from its generate to its last pass, so a work item
/// reopened on the display lane meanwhile can take the Image slot without
/// deleting the file the hi-res is about to read.
fn run_image(
    engine: &mut Engine,
    saver: &pipeline::Display,
    queue: &Jobs,
    plan: &RunPlan,
    done: u32,
) -> Result<()> {
    let first = match &plan.start {
        Start::Generate(req) => {
            let mut req = req.clone();
            req.seed = jobs::walk_seed(req.seed, done);
            engine.run(&req, true)?
        }
        Start::Img2Img(req) => {
            let mut req = req.clone();
            req.seed = jobs::walk_seed(req.seed, done);
            engine.img2img(&req, true)?
        }
    };
    let id = first.image_id;
    latest(queue, &first);
    let rest = (|| -> Result<()> {
        if let Some(h) = &plan.after.hires {
            let over = EscalateRequest {
                template: h.model.clone(),
                ..plan.escalate.clone()
            };
            let r = engine.hires(id, h.upscale.unwrap_or(2.0), &over)?;
            latest(queue, &r);
        }
        if let Some(rf) = &plan.after.refine {
            let over = EscalateRequest {
                template: rf.model.clone(),
                ..plan.escalate.clone()
            };
            let r = engine.refine(id, &over)?;
            latest(queue, &r);
        }
        if plan.after.save {
            saver.save_output(id)?;
        }
        Ok(())
    })();
    lock(&engine.retained()).unpin(id);
    rest
}

/// Show the page in the person's own browser. A double-click has no other way
/// to find it, so this is the one thing a first run must not leave to a README.
/// Not waited on — `xdg-open` can block until the browser exits — and a
/// failure only costs the person a click, so it prints the address and moves on.
fn open_browser(url: &str) {
    let spawned = if cfg!(windows) {
        // `start`'s first quoted argument is a window title, hence the "".
        std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(url).spawn()
    };
    if spawned.is_err() {
        println!("[yasdig] open {url} in your browser");
    }
}

fn handle_conn(
    stream: std::net::TcpStream,
    lanes: &Lanes,
    cfg: &Config,
    web_dir: Option<&std::path::Path>,
    monitor: &Monitor,
    bound: &str,
) -> Result<()> {
    let mut conn = http::Connection::new(stream)?;
    let req = match conn.read_request()? {
        Some(r) => r,
        None => return Ok(()),
    };
    if !http::allowed(&req.method, req.origin.as_deref(), req.host.as_deref(), bound) {
        eprintln!(
            "[yasdig] refused {} {} from origin {:?}, host {:?}",
            req.method, req.path, req.origin, req.host
        );
        return conn.respond(
            403,
            "application/json",
            error_json("refused: this request came from another website").as_bytes(),
        );
    }

    // A handler that panics — a model file the parsers didn't expect, say —
    // must still answer, or the page waits on a request that will never end.
    // The connection is this request's alone, so nothing else sees its state.
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        route(&mut conn, req, lanes, cfg, web_dir, monitor)
    }));
    match caught {
        Ok(r) => r,
        Err(panic) => {
            let msg = panic
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| panic.downcast_ref::<&str>().copied())
                .unwrap_or("unknown panic");
            conn.respond(
                500,
                "application/json",
                error_json(&format!("internal error: {msg}")).as_bytes(),
            )
        }
    }
}

fn route(
    conn: &mut http::Connection,
    req: http::Request,
    lanes: &Lanes,
    cfg: &Config,
    web_dir: Option<&std::path::Path>,
    monitor: &Monitor,
) -> Result<()> {
    // The page and everything it loads, resolved through one whitelist —
    // `web::WEB_FILES`. Ahead of the routes rather than an arm among them
    // because there are sixteen names and one answer for all of them. The client
    // never names a file that isn't in that table, which is the same rule as
    // `workitems::valid_id` and the reason there is no generic static handler to
    // traverse out of.
    if req.method == "GET" {
        if let Some(file) = web::lookup(&req.path) {
            return web::serve(conn, web_dir, file);
        }
    }

    match (req.method.as_str(), req.path.as_str()) {
        // Built live so newly-dropped checkpoints appear without a restart.
        ("GET", "/templates") | ("GET", "/models") => {
            conn.respond(200, "application/json", build_models_json(cfg).as_bytes())
        }
        // Filesystem-only, so they answer here rather than through the job
        // queue — the worker is serialized, and a form submit queued behind a
        // render would appear to hang for the length of the render.
        ("GET", "/settings") => handle_get_settings(conn, cfg),
        ("POST", "/settings") => handle_save_settings(conn, cfg, &req.body),
        ("GET", "/model-settings") => handle_get_model_settings(conn, cfg),
        ("POST", "/model-settings") => handle_save_model_settings(conn, cfg, &req.body),
        ("POST", "/generate") => handle_generate(conn, lanes, &req.body),
        ("POST", "/img2img") => handle_img2img(conn, cfg, lanes, &req.body),
        // The one route with a binary body, and the one `http.rs` gives a large
        // limit to. Filesystem-only, so it stays off the queue — an upload must
        // not wait out the render it is meant to replace.
        ("POST", "/upload") => handle_upload(conn, cfg, &req.body),
        ("POST", "/png-settings") => handle_png_settings(conn, cfg, &req.body),
        ("POST", "/refine") => handle_refine(conn, lanes, &req.body),
        ("POST", "/hires") => handle_hires(conn, lanes, &req.body),
        ("POST", "/redraw") => handle_redraw(conn, lanes, &req.body),
        ("POST", "/upscale") => handle_upscale(conn, lanes, &req.body),
        ("POST", "/edit") => handle_edit(conn, lanes, &req.body),
        ("POST", "/extend") => handle_extend(conn, lanes, &req.body),
        ("POST", "/history/goto") => handle_goto(conn, lanes, &req.body),
        ("POST", "/crop") => handle_crop(conn, lanes, &req.body),
        ("POST", "/resize") => handle_resize(conn, lanes, &req.body),
        ("POST", "/apply") => handle_apply(conn, lanes, &req.body),
        ("POST", "/text") => handle_text(conn, lanes, &req.body),
        ("POST", "/brush") => handle_brush(conn, lanes, &req.body),
        // A brush tool's mask, for the page to paint on again.
        ("GET", p) if p.starts_with("/brush/") && p.ends_with(".png") => {
            match brush_path(p).and_then(|(id, tool)| pipeline::Retained::brush_mask(&lanes.retained, id, tool)) {
                Some(png) => conn.respond_with(200, "image/png", "Cache-Control: no-store\r\n", &png),
                None => conn.respond(404, "text/plain", b"not found"),
            }
        }
        ("POST", "/history/view") => handle_view_step(conn, lanes, &req.body),
        // A history step's thumbnail. Off the queue like every thumbnail; both
        // numbers are the whole whitelist.
        ("GET", p) if p.starts_with("/history/") && p.ends_with(".png") => {
            match history_path(p).and_then(|(id, i)| {
                pipeline::Retained::history_thumb(&lanes.retained, id, i)
            }) {
                Some(png) => conn.respond_with(200, "image/png", "Cache-Control: no-store\r\n", &png),
                None => conn.respond(404, "text/plain", b"not found"),
            }
        }
        ("POST", "/adjust") => handle_adjust(conn, lanes, &req.body),
        ("POST", "/save-output") => handle_save_output(conn, lanes, &req.body, false),
        ("POST", "/outputs/overwrite") => handle_save_output(conn, lanes, &req.body, true),
        ("POST", "/download-image") => handle_download_image(conn, lanes, &req.body),

        // Progress and Stop, both on the connection thread and both for the
        // same reason: they are *about* the job the worker is running, so
        // queueing them behind it would mean a progress bar that only updates
        // once the render it reports on has finished, and a Stop button that
        // can't be pressed until there is nothing left to stop.
        // Which folder this server is, for a second start deciding whether to
        // hand over to it or take the next port (`instance_home`). On the
        // connection thread so it answers mid-render.
        ("GET", "/instance") => conn.respond(
            200,
            "application/json",
            format!(
                "{{\"app\":\"yasdig\",\"home\":{},\"build\":{}}}",
                json_str(&cfg.base_dir.display().to_string()),
                json_str(startup::build_id())
            )
            .as_bytes(),
        ),
        ("GET", "/progress") => conn.respond(
            200,
            "application/json",
            progress_json(monitor).as_bytes(),
        ),
        // The newest live preview, by the number `/progress` and `/jobs` report.
        // A number is the whole whitelist; a 404 is normal, since an older
        // frame is pruned as soon as a newer one is complete.
        ("GET", p) if p.starts_with("/preview/") && p.ends_with(".png") => {
            let n = p.trim_start_matches("/preview/").trim_end_matches(".png");
            match n.parse::<u32>().ok().and_then(|n| monitor.preview_png(n)) {
                Some(png) => conn.respond_with(200, "image/png", "Cache-Control: no-store\r\n", &png),
                None => conn.respond(404, "text/plain", b"not found"),
            }
        }
        // The queue, beside them for the same reason: the Jobs dialog is about
        // what the worker is doing and what waits behind it.
        ("GET", "/jobs") => handle_jobs(conn, &lanes.queue, monitor),
        ("GET", "/jobs/latest") => match lanes.queue.latest() {
            Some((_, json)) => conn.respond(200, "application/json", json.as_bytes()),
            None => conn.respond(404, "application/json", error_json("no image yet").as_bytes()),
        },
        ("POST", "/jobs/stop") => handle_stop_job(conn, lanes, monitor, &req.body),
        ("POST", "/jobs/skip") => handle_skip_job(conn, lanes, monitor, &req.body),
        ("POST", "/cancel") => conn.respond(
            200,
            "application/json",
            format!("{{\"cancelled\":{}}}", monitor.cancel()).as_bytes(),
        ),
        ("POST", "/shutdown") => {
            lanes.downloads.cancel_all();
            handle_shutdown(conn, cfg, monitor, &req.body)
        }

        // The archive. List, thumbnail and delete are answered right here rather
        // than through `submit` — they never touch the GPU, and the worker is
        // serialized, so going through it would leave the grid dead for the
        // length of whatever render is in flight.
        ("GET", "/workitems") => handle_list_workitems(conn, cfg, Shelf::Work),
        // `http.rs` strips the query string before routing, so the id has to be
        // a path segment — hence a guard arm rather than a literal.
        ("GET", p) if p.starts_with("/workitems/") && p.ends_with("/thumb.png") => {
            let id = p
                .trim_start_matches("/workitems/")
                .trim_end_matches("/thumb.png");
            handle_thumb(conn, cfg, Shelf::Work, id)
        }
        ("POST", "/workitems/open") => {
            handle_open_workitem(conn, lanes, Shelf::Work, &req.body)
        }
        ("POST", "/workitems/delete") => {
            handle_delete_workitems(conn, cfg, Shelf::Work, &req.body)
        }

        // The same four routes over `user/favorite/`, which is the same three
        // files under the same ids — so each one is the handler above with a
        // different `Shelf`. The shelf is a *path segment* rather than a query
        // parameter because `http.rs` strips the query string before routing,
        // which is the same constraint that already puts an id in the path.
        ("GET", "/favorites") => handle_list_workitems(conn, cfg, Shelf::Favorites),
        ("GET", p) if p.starts_with("/favorites/") && p.ends_with("/thumb.png") => {
            let id = p
                .trim_start_matches("/favorites/")
                .trim_end_matches("/thumb.png");
            handle_thumb(conn, cfg, Shelf::Favorites, id)
        }
        ("POST", "/favorites/open") => {
            handle_open_workitem(conn, lanes, Shelf::Favorites, &req.body)
        }
        ("POST", "/favorites/delete") => {
            handle_delete_workitems(conn, cfg, Shelf::Favorites, &req.body)
        }
        ("POST", "/favorites/add") => handle_favorite(conn, cfg, &req.body, true),
        ("POST", "/favorites/remove") => handle_favorite(conn, cfg, &req.body, false),

        // `work/output/` — the finished images, which is what the video tab
        // animates from. Same connection-thread family as the archive above, and
        // the same guard-arm shape for the same reason.
        // Built live, like `/models`: a video model is tens of gigabytes across
        // several files, so "it finished downloading" is something to notice
        // without a restart.
        ("GET", "/convert-models") => handle_convert_models(conn, cfg),
        ("POST", "/convert") => handle_convert(conn, cfg, lanes, &req.body),
        // Filesystem only — one header seek per file in the LoRA dir — so it
        // answers here rather than queueing behind whatever is rendering. Built
        // live for the reason `/models` is: dropping a file into a folder should
        // not need a restart to be noticed.
        ("GET", "/loras") => handle_list_loras(conn, cfg),
        // Same shape and reason, minus the header read — see `UpscalerFile`.
        ("GET", "/upscalers") => handle_list_upscalers(conn, cfg),
        // The Get models tab. Its own lane (`downloads`), so all four answer
        // here: a download is network, not GPU, and must neither wait behind a
        // render nor hold one up. The body names a catalog id and nothing else.
        // The third-party notices a release has to carry inside itself — the
        // repo's THIRD_PARTY_LICENSES.md, compiled in, read by Help → About &
        // licences. One copy: the file is the source, this serves it.
        ("GET", "/licences") => conn.respond(
            200,
            "text/plain; charset=utf-8",
            include_str!("../THIRD_PARTY_LICENSES.md").as_bytes(),
        ),
        ("GET", "/catalog") => match serde_json::to_vec(&lanes.downloads.status(cfg)) {
            Ok(body) => conn.respond(200, "application/json", &body),
            Err(e) => conn.respond(500, "application/json", error_json(&format!("{e}")).as_bytes()),
        },
        ("POST", p) if p.starts_with("/downloads/") => handle_download(conn, cfg, lanes, p, &req.body),
        ("GET", "/video-models") => handle_video_models(conn, cfg),
        ("POST", "/video") => handle_video(conn, lanes, &req.body),
        ("POST", "/videos/enlarge") => handle_enlarge_clip(conn, lanes, &req.body),
        ("GET", "/videos") => handle_list_clips(conn, cfg),
        // Two guard arms rather than one, because a clip serves both a
        // thumbnail and its frames and only the frame route has a number in it.
        ("GET", p) if p.starts_with("/videos/") && p.ends_with("/thumb.png") => {
            let id = p
                .trim_start_matches("/videos/")
                .trim_end_matches("/thumb.png");
            handle_clip_file(conn, cfg, id, None)
        }
        ("GET", p) if p.starts_with("/videos/") && p.contains("/frames/") => {
            match clip_frame(p) {
                Some((id, n)) => handle_clip_file(conn, cfg, id, Some(n)),
                None => conn.respond(404, "text/plain", b"not found"),
            }
        }
        ("GET", p) if p.starts_with("/videos/") && p.ends_with("/audio.wav") => {
            handle_clip_media(conn, cfg, p.trim_start_matches("/videos/").trim_end_matches("/audio.wav"), false)
        }
        ("GET", p) if p.starts_with("/videos/") && p.ends_with("/clip.avi") => {
            handle_clip_media(conn, cfg, p.trim_start_matches("/videos/").trim_end_matches("/clip.avi"), true)
        }
        ("POST", "/videos/delete") => handle_delete_clips(conn, cfg, &req.body),
        ("GET", "/outputs") => handle_list_outputs(conn, cfg, Gallery::Output),
        ("POST", "/outputs/open") => handle_open_output(conn, lanes, &req.body),
        ("POST", "/image/model") => handle_set_model(conn, lanes, &req.body),
        ("POST", "/outputs/delete") => {
            handle_delete_outputs(conn, cfg, Gallery::Output, &req.body)
        }
        // The kept half of the gallery. Only a listing, an add and a delete:
        // opening one, its thumbnail and starting from it all go by stem through
        // the `/outputs` routes above, which look in both (`Store::find_output`).
        ("GET", "/favorite-outputs") => handle_list_outputs(conn, cfg, Gallery::Favorites),
        ("POST", "/favorite-outputs/add") => handle_favorite_outputs(conn, cfg, &req.body, true),
        ("POST", "/favorite-outputs/remove") => {
            handle_favorite_outputs(conn, cfg, &req.body, false)
        }
        ("POST", "/favorite-outputs/delete") => {
            handle_delete_outputs(conn, cfg, Gallery::Favorites, &req.body)
        }
        ("GET", p) if p.starts_with("/outputs/") && p.ends_with("/thumb.png") => {
            let stem = p
                .trim_start_matches("/outputs/")
                .trim_end_matches("/thumb.png");
            handle_output_thumb(conn, cfg, stem)
        }

        _ => conn.respond(404, "text/plain", b"not found"),
    }
}

/// *Close server*: the way out of a release started by a double-click, which
/// has no console to close.
///
/// On the connection thread beside `/cancel`, never through the queue — a
/// render would hold it for minutes, and "close" that waits for the thing it is
/// closing is the Stop-button mistake again. In order:
///
/// 1. **Kill the child first.** The process exiting would leave sd-cli running
///    on its own, holding the VRAM, with no server left to stop it. Then empty
///    `work/current/`, which is scratch and needs no setting to say so.
/// 2. Clear what `user/settings.toml` says to ([`shutdown::clear`]). The *saved*
///    file, which is what the page's confirm box quotes.
/// 3. Answer, so the page can say it has stopped rather than seeing a dropped
///    connection it can't tell from a crash.
/// 4. Exit. That frees the port and closes a Windows console window. It never
///    reaches `main`'s "press Enter" wait, which is for errors.
///
/// A worker in the middle of a job is not waited for: with its child killed
/// the job is failing anyway, and nothing it would write is worth keeping a
/// server open for.
///
/// `{"clear": false}` skips step 2. That is a newer start taking the port back
/// (`startup::bind`): nobody pressed *Close server*, so nothing the settings
/// tie to that button may happen. An empty body — the button — clears.
fn handle_shutdown(
    conn: &mut http::Connection,
    cfg: &Config,
    monitor: &Monitor,
    body: &[u8],
) -> Result<()> {
    let clear = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["clear"].as_bool())
        // Deleting is asked for, never assumed: the page always says which it
        // wants, and a body that doesn't say is not a request to delete files.
        .unwrap_or(false);
    monitor.cancel();
    // Always, whatever the settings say: `work/current/` is scratch, and the
    // queue it served is gone with this process anyway.
    pipeline::clear_scratch(&cfg.base_dir.join("work"));
    let cleared = if clear {
        let c = shutdown::clear(cfg);
        println!(
            "[yasdig] closed from the page (cleared {} work items, {} outputs, {} clips)",
            c.workitems, c.outputs, c.videos
        );
        c
    } else {
        println!("[yasdig] a newer start took over this port — closing");
        shutdown::Cleared::default()
    };
    let body = serde_json::json!({ "closed": true, "cleared": cleared });
    let _ = conn.respond(200, "application/json", body.to_string().as_bytes());
    std::process::exit(0);
}

/// The archive, for a connection thread. Cheap to build — it is two `PathBuf`s
/// and a `create_dir_all` — so each request makes its own rather than sharing
/// the engine's behind a lock.
fn store(cfg: &Config) -> workitems::Store {
    workitems::Store::new(&cfg.base_dir.join("work"), &cfg.user_dir())
}

fn handle_list_workitems(
    conn: &mut http::Connection,
    cfg: &Config,
    shelf: Shelf,
) -> Result<()> {
    let items = store(cfg).listing(shelf);
    match serde_json::to_vec(&items) {
        Ok(body) => conn.respond(200, "application/json", &body),
        Err(e) => conn.respond(
            500,
            "application/json",
            error_json(&format!("listing work items: {e}")).as_bytes(),
        ),
    }
}

/// A 404 covers both "not a work item id" and "no such work item", because from
/// the browser's side they are the same thing: a thumbnail that isn't there.
/// [`workitems::valid_id`] is the traversal guard — it is a whitelist of digits
/// and `-`, so nothing that reaches `read` can name a path outside the archive.
fn handle_thumb(conn: &mut http::Connection, cfg: &Config, shelf: Shelf, id: &str) -> Result<()> {
    if !workitems::valid_id(id) {
        return conn.respond(404, "text/plain", b"not found");
    }
    match std::fs::read(store(cfg).thumb_path(shelf, id)) {
        Ok(body) => conn.respond(200, "image/png", &body),
        Err(_) => conn.respond(404, "text/plain", b"not found"),
    }
}

/// Split `/videos/{id}/frames/{n}.png` into its two parts.
///
/// Its own function so it can be tested, because the interesting case is the one
/// that must *not* happen: a frame number that doesn't parse has to be `None`
/// all the way out. Returning a `None` frame index instead would fall into
/// [`handle_clip_file`]'s thumbnail branch, and `/frames/nonsense.png` would
/// quietly answer 200 with the poster frame.
///
/// The id is not validated here — [`handle_clip_file`] does that with
/// [`workitems::valid_id`], which is the traversal guard and should stay the
/// single place that decision is made.
fn clip_frame(path: &str) -> Option<(&str, u32)> {
    let (id, frame) = path.trim_start_matches("/videos/").split_once("/frames/")?;
    let n = frame.strip_suffix(".png")?.parse::<u32>().ok()?;
    Some((id, n))
}

/// The clip archive, for a connection thread. Same reasoning as [`store`].
fn video_store(cfg: &Config) -> videos::VideoStore {
    videos::VideoStore::new(&cfg.base_dir.join("work"))
}

/// A `[[video]]` entry as the page needs it: the entry itself, plus the things
/// the page would otherwise have to re-derive from `family`.
///
/// The grid rules are served rather than reimplemented in JavaScript because
/// the page rounds the frame count to show a duration, and `video_cmd` rounds it
/// to run the model. Two implementations of the same arithmetic is how those
/// start disagreeing.
#[derive(serde::Serialize)]
struct VideoModelDto {
    #[serde(flatten)]
    model: config::VideoModel,
    family: &'static str,
    size_multiple: u32,
    frame_grid: config::FrameGrid,
    /// `Some(n)` when the model overrides fps whatever it is asked for, so the
    /// page can show the box as fixed rather than let it lie.
    forced_fps: Option<u32>,
    /// True for the mixture-of-experts pair: the page shows the second set of
    /// step/CFG boxes only when they mean something.
    is_moe: bool,
    /// The memory mode the model runs with, resolved — `memory` beside it is
    /// the raw entry, which an old `offload_to_cpu` key leaves unset.
    memory_mode: String,
    /// Which component files aren't on disk. A greyed-out entry that says *what*
    /// is missing is the download list the config comments promise.
    missing: Vec<String>,
    /// Seconds, from the rounded frame count and the fps that will really be
    /// used — the number the page shows before anything has run.
    duration: f32,
    /// The separate files, for the Video tab's *Models* rows — the image
    /// side's `parts`, from [`config::VideoFamily::slots`].
    parts: Vec<config::PartView>,
}

/// Everything in the LoRA dir, usable or not.
///
/// The unusable ones are listed with their reason rather than filtered out, the
/// same call `doctor` makes for a half-downloaded video model: "my LoRA isn't in
/// the list" is a much worse thing to debug than "my LoRA is in the list and
/// The Settings tab's two routes.
///
/// A *broken* `user/settings.toml` answers 200 with the built-in settings and an
/// `error` beside them rather than a 500, because the page has two jobs here and
/// only one of them has failed: it still has to fill the form with something
/// editable, and saving that form is how the file gets repaired. A 500 would
/// leave an empty form and no way out but a text editor. Generation meanwhile
/// falls back the same way — see `Settings::load_or_default`.
fn handle_get_settings(conn: &mut http::Connection, cfg: &Config) -> Result<()> {
    let (settings, note) = match Settings::load(&cfg.user_dir()) {
        Ok(s) => (s, None),
        Err(e) => (Settings::default(), Some(format!("{e:#}"))),
    };
    match settings::to_json(&settings, note.as_deref()) {
        Ok(body) => conn.respond(200, "application/json", body.as_bytes()),
        Err(e) => conn.respond(
            500,
            "application/json",
            error_json(&format!("{e:#}")).as_bytes(),
        ),
    }
}

/// Validate, then write. In that order, so a form with a bad number in it leaves
/// the file exactly as it found it — the error comes back as text the tab shows
/// beside the Save button.
fn handle_save_settings(conn: &mut http::Connection, cfg: &Config, body: &[u8]) -> Result<()> {
    // Over what is stored, not over the built-ins — see `settings::patch`. A
    // file that won't parse patches the built-ins instead, which is the only
    // thing a repair could sensibly start from.
    let stored = Settings::load(&cfg.user_dir()).unwrap_or_default();
    let result = settings::patch(&stored, body)
        .and_then(|s| {
            s.validate()?;
            Ok(s)
        })
        .and_then(|s| {
            s.save(&cfg.user_dir())?;
            Ok(s)
        });
    match result {
        Ok(s) => match settings::to_json(&s, None) {
            Ok(b) => conn.respond(200, "application/json", b.as_bytes()),
            Err(e) => conn.respond(
                500,
                "application/json",
                error_json(&format!("{e:#}")).as_bytes(),
            ),
        },
        Err(e) => conn.respond(
            400,
            "application/json",
            error_json(&format!("{e:#}")).as_bytes(),
        ),
    }
}

/// Every model's slots, for the Settings tab's *Per model* card — the same
/// views `/models` serves, keyed by name.
fn part_views(cfg: &Config) -> std::collections::BTreeMap<String, Vec<config::PartView>> {
    let scan = cfg.scan_images();
    cfg.registry()
        .into_iter()
        .map(|(name, p)| {
            let views = cfg.part_views(&p, &scan);
            (name, views)
        })
        .collect()
}

/// `GET /model-settings`. A file that won't parse answers 200 with no
/// overrides and the error beside them, for the reason `/settings` does: the
/// form still has to be editable, and saving it is the repair.
fn handle_get_model_settings(conn: &mut http::Connection, cfg: &Config) -> Result<()> {
    let (tuned, note) = match settings::ModelSettings::load(&cfg.user_dir()) {
        Ok(t) => (t, None),
        Err(e) => (
            settings::ModelSettings::default(),
            Some(format!("{e:#} — showing no per-model settings; saving will rewrite the file")),
        ),
    };
    match settings::models_json(&cfg.entries(), &tuned, &part_views(cfg), note.as_deref()) {
        Ok(b) => conn.respond(200, "application/json", b.as_bytes()),
        Err(e) => conn.respond(500, "application/json", error_json(&format!("{e:#}")).as_bytes()),
    }
}

/// `POST /model-settings`: `{"model": name, "values": {knob: value | null}}`.
///
/// Filesystem only, like `/settings`, and for the same reason off the queue.
/// Answers with the whole listing, so the page redraws from what was stored
/// rather than from what it sent — the server drops every value equal to the
/// model's base, and the form should show that.
fn handle_save_model_settings(
    conn: &mut http::Connection,
    cfg: &Config,
    body: &[u8],
) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct Dto {
        model: String,
        #[serde(default)]
        values: serde_json::Map<String, serde_json::Value>,
        /// Slot key to a name under the models dir, or `null` to forget it.
        #[serde(default)]
        parts: serde_json::Map<String, serde_json::Value>,
    }
    let result = (|| -> Result<String> {
        let dto: Dto = serde_json::from_slice(body).context("parsing model settings body")?;
        let base = cfg
            .model_base(&dto.model)
            .with_context(|| format!("no model named '{}'", dto.model))?;
        // Over a broken file, nothing: the same repair rule as `/settings`.
        let mut tuned = settings::ModelSettings::load(&cfg.user_dir()).unwrap_or_default();
        let current = tuned.model.get(&dto.model).cloned().unwrap_or_default();
        let next = settings::update_model(&current, &base, &dto.values)?;
        // An entry with nothing left in it is removed rather than written as an
        // empty table, so clearing every box leaves no trace of the model.
        if next == settings::PartialSettings::default() {
            tuned.model.remove(&dto.model);
        } else {
            tuned.model.insert(dto.model.clone(), next);
        }
        let saved = tuned.parts.get(&dto.model).cloned().unwrap_or_default();
        let parts = cfg.update_parts(&dto.model, &saved, &dto.parts)?;
        if parts.is_empty() {
            tuned.parts.remove(&dto.model);
        } else {
            tuned.parts.insert(dto.model.clone(), parts);
        }
        tuned.save(&cfg.user_dir())?;
        settings::models_json(&cfg.entries(), &tuned, &part_views(cfg), None)
    })();
    match result {
        Ok(b) => conn.respond(200, "application/json", b.as_bytes()),
        Err(e) => conn.respond(400, "application/json", error_json(&format!("{e:#}")).as_bytes()),
    }
}

/// says why".
fn handle_list_loras(conn: &mut http::Connection, cfg: &Config) -> Result<()> {
    match serde_json::to_vec(&cfg.list_loras()) {
        Ok(body) => conn.respond(200, "application/json", &body),
        Err(e) => conn.respond(
            500,
            "application/json",
            error_json(&format!("listing LoRAs: {e}")).as_bytes(),
        ),
    }
}

/// `GET /upscalers`. Filesystem only, like `/loras`.
fn handle_list_upscalers(conn: &mut http::Connection, cfg: &Config) -> Result<()> {
    match serde_json::to_vec(&cfg.list_upscalers()) {
        Ok(body) => conn.respond(200, "application/json", &body),
        Err(e) => conn.respond(
            500,
            "application/json",
            error_json(&format!("listing upscalers: {e}")).as_bytes(),
        ),
    }
}

fn handle_download(
    conn: &mut http::Connection,
    cfg: &Config,
    lanes: &Lanes,
    path: &str,
    body: &[u8],
) -> Result<()> {
    let entry = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["entry"].as_str().map(str::to_string))
        .unwrap_or_default();
    let d = &lanes.downloads;
    let r = match path {
        "/downloads/start" => d.enqueue(cfg, &entry).map(|n| format!("{{\"queued\":{n}}}")),
        "/downloads/cancel" => d.cancel(&entry).map(|()| "{}".into()),
        "/downloads/clear" => d.clear(&entry).map(|()| "{}".into()),
        _ => return conn.respond(404, "text/plain", b"not found"),
    };
    match r {
        Ok(body) => conn.respond(200, "application/json", body.as_bytes()),
        Err(e) => conn.respond(400, "application/json", error_json(&format!("{e:#}")).as_bytes()),
    }
}

fn handle_video_models(conn: &mut http::Connection, cfg: &Config) -> Result<()> {
    let scan = cfg.video_scan();
    let models: Vec<VideoModelDto> = cfg
        .video_models()
        .into_iter()
        .map(|m| VideoModelDto {
            parts: cfg.video_part_views(&m, &scan),
            family: m.family.as_str(),
            size_multiple: m.family.size_multiple(),
            frame_grid: m.family.frame_grid(),
            forced_fps: m.family.forced_fps(),
            is_moe: m.family.is_moe(),
            memory_mode: m.memory_mode().to_string(),
            missing: m.missing().iter().map(|p| p.display().to_string()).collect(),
            duration: m.duration(),
            model: m,
        })
        .collect();
    match serde_json::to_vec(&models) {
        Ok(body) => conn.respond(200, "application/json", &body),
        Err(e) => conn.respond(
            500,
            "application/json",
            error_json(&format!("listing video models: {e}")).as_bytes(),
        ),
    }
}

fn handle_video(conn: &mut http::Connection, lanes: &Lanes, body: &[u8]) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct Dto {
        prompt: String,
        model: String,
        #[serde(default)]
        negative: Option<String>,
        #[serde(default)]
        seed: Option<i64>,
        /// A `work/output/` stem. Absent or empty runs the same model as plain
        /// text-to-video, which is what clearing the picker means.
        #[serde(default)]
        source: Option<String>,
        #[serde(default)]
        width: Option<u32>,
        #[serde(default)]
        height: Option<u32>,
        #[serde(default)]
        frames: Option<u32>,
        #[serde(default)]
        fps: Option<u32>,
        #[serde(default)]
        steps: Option<u32>,
        #[serde(default)]
        cfg_scale: Option<f32>,
        #[serde(default)]
        high_noise_steps: Option<u32>,
        #[serde(default)]
        high_noise_cfg_scale: Option<f32>,
        #[serde(default)]
        flow_shift: Option<f32>,
        #[serde(default)]
        sampler: Option<String>,
        #[serde(default)]
        vae_tiling: Option<bool>,
        #[serde(default)]
        memory: Option<String>,
        #[serde(default)]
        parts: Option<std::collections::BTreeMap<String, Option<String>>>,
    }

    let dto: Dto = match parse_json::<Dto>(conn, body)? {
        Some(d) => d,
        None => return Ok(()),
    };

    let req = VideoRequest {
        prompt: dto.prompt,
        model: dto.model,
        negative: dto.negative,
        seed: dto.seed.unwrap_or(-1),
        source: dto.source.filter(|s| !s.is_empty()),
        width: dto.width,
        height: dto.height,
        frames: dto.frames,
        fps: dto.fps,
        steps: dto.steps,
        cfg_scale: dto.cfg_scale,
        high_noise_steps: dto.high_noise_steps,
        high_noise_cfg_scale: dto.high_noise_cfg_scale,
        flow_shift: dto.flow_shift,
        sampler: dto.sampler,
        vae_tiling: dto.vae_tiling,
        memory: dto.memory,
        parts: dto.parts,
    };

    let label = req.model.clone();
    enqueue_wait(conn, lanes, jobs::Kind::Video, label, Work::Video(req), false, 500)
}

fn handle_enlarge_clip(
    conn: &mut http::Connection,
    lanes: &Lanes,
    body: &[u8],
) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct Dto {
        id: String,
        #[serde(default)]
        upscale: Option<f32>,
        #[serde(default)]
        sharpness: Option<f32>,
        #[serde(default)]
        clarity: Option<f32>,
    }

    match parse_json::<Dto>(conn, body)? {
        Some(dto) => enqueue_wait(
            conn,
            lanes,
            jobs::Kind::Enlarge,
            format!("clip {}", dto.id),
            Work::EnlargeClip {
                id: dto.id,
                upscale: dto.upscale.unwrap_or(2.0),
                sharpness: dto.sharpness,
                clarity: dto.clarity,
            },
            false,
            400,
        ),
        None => Ok(()),
    }
}

/// The convert tab's candidate list. Filesystem-only — it reads a header per
/// file, which is a seek and a few hundred KB — so it answers on the connection
/// thread like the other listings. Queueing it would mean the tab couldn't
/// populate while a conversion was running, which is precisely when someone
/// looks at it.
fn handle_convert_models(conn: &mut http::Connection, cfg: &Config) -> Result<()> {
    let list = convert::listing(cfg);
    match serde_json::to_vec(&list) {
        Ok(body) => conn.respond(200, "application/json", &body),
        Err(e) => conn.respond(
            500,
            "application/json",
            error_json(&format!("listing convertible models: {e}")).as_bytes(),
        ),
    }
}

/// Start a conversion.
///
/// The plan is built **here**, on the connection thread, not in the worker: it
/// is the validation step, and a bad type or an output that already exists
/// should come back as a 400 immediately rather than after waiting out whatever
/// render is in flight. It is also where the client's `source` is checked
/// against the published candidate list — the client chooses from what the
/// server offered, it never names a path.
fn handle_convert(
    conn: &mut http::Connection,
    cfg: &Config,
    lanes: &Lanes,
    body: &[u8],
) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct Dto {
        source: String,
        #[serde(rename = "type")]
        ty: String,
    }

    let Some(dto) = parse_json::<Dto>(conn, body)? else {
        return Ok(());
    };
    match convert::plan(cfg, &dto.source, &dto.ty) {
        Ok(plan) => {
            let label = plan
                .source
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            enqueue_wait(conn, lanes, jobs::Kind::Convert, label, Work::Convert(plan), false, 400)
        }
        Err(e) => conn.respond(
            400,
            "application/json",
            error_json(&format!("{e:#}")).as_bytes(),
        ),
    }
}

fn handle_list_clips(conn: &mut http::Connection, cfg: &Config) -> Result<()> {
    let clips = video_store(cfg).list();
    match serde_json::to_vec(&clips) {
        Ok(body) => conn.respond(200, "application/json", &body),
        Err(e) => conn.respond(
            500,
            "application/json",
            error_json(&format!("listing clips: {e}")).as_bytes(),
        ),
    }
}

/// A clip's thumbnail (`frame` = `None`) or one of its frames.
///
/// One function for both because the guard is the same and it is the only thing
/// that matters: [`workitems::valid_id`] on the id, and a parsed `u32` for the
/// frame, so neither path segment can name anything outside the clip directory.
/// A 404 covers "bad id", "no such clip" and "no such frame" alike — from the
/// browser's side they are one thing, an image that isn't there.
fn handle_clip_file(
    conn: &mut http::Connection,
    cfg: &Config,
    id: &str,
    frame: Option<u32>,
) -> Result<()> {
    if !workitems::valid_id(id) {
        return conn.respond(404, "text/plain", b"not found");
    }
    let store = video_store(cfg);
    let path = match frame {
        Some(n) => store.frame_path(id, n),
        None => store.thumb_path(id),
    };
    match std::fs::read(path) {
        Ok(body) => conn.respond(200, "image/png", &body),
        Err(_) => conn.respond(404, "text/plain", b"not found"),
    }
}

/// A clip's soundtrack (for the player), or the clip as one video file (its
/// *Download*). The file is the one [`videos::VideoStore::download_file`]
/// names, encoded on first request — a second or so of JPEG encoding, on the
/// connection thread like every other file route.
fn handle_clip_media(conn: &mut http::Connection, cfg: &Config, id: &str, video: bool) -> Result<()> {
    let store = video_store(cfg);
    let Ok(clip) = store.load(id) else {
        return conn.respond(404, "text/plain", b"not found");
    };
    let path = if video {
        match store.download_file(&clip) {
            Ok(p) => p,
            Err(e) => return conn.respond(500, "application/json", error_json(&format!("{e:#}")).as_bytes()),
        }
    } else {
        match store.audio_path(id) {
            Some(p) => p,
            None => return conn.respond(404, "text/plain", b"not found"),
        }
    };
    match std::fs::read(&path) {
        Ok(body) if video => conn.respond_with(
            200,
            "video/x-msvideo",
            &format!("Content-Disposition: attachment; filename=\"{id}.avi\"\r\n"),
            &body,
        ),
        Ok(body) => conn.respond(200, "audio/wav", &body),
        Err(_) => conn.respond(404, "text/plain", b"not found"),
    }
}

fn handle_delete_clips(conn: &mut http::Connection, cfg: &Config, body: &[u8]) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct Dto {
        ids: Vec<String>,
    }

    let dto: Dto = match parse_json::<Dto>(conn, body)? {
        Some(d) => d,
        None => return Ok(()),
    };
    let store = video_store(cfg);
    let mut failed = Vec::new();
    for id in &dto.ids {
        if let Err(e) = store.delete(id) {
            failed.push(format!("{id}: {e:#}"));
        }
    }
    if failed.is_empty() {
        conn.respond(200, "application/json", br#"{"deleted":true}"#)
    } else {
        conn.respond(
            500,
            "application/json",
            error_json(&failed.join("; ")).as_bytes(),
        )
    }
}

fn handle_list_outputs(conn: &mut http::Connection, cfg: &Config, gallery: Gallery) -> Result<()> {
    let items = store(cfg).output_listing(gallery);
    match serde_json::to_vec(&items) {
        Ok(body) => conn.respond(200, "application/json", &body),
        Err(e) => conn.respond(
            500,
            "application/json",
            error_json(&format!("listing outputs: {e}")).as_bytes(),
        ),
    }
}

/// Like [`handle_thumb`], except the file may not exist yet: everything saved
/// before thumbnails existed gets one on its first request. That makes this the
/// one thumbnail route that can take ~100 ms — an output PNG is full resolution.
/// It is still on the connection thread, since it's a decode and a resize rather
/// than GPU work, and the grid asks for its tiles in parallel.
fn handle_output_thumb(conn: &mut http::Connection, cfg: &Config, stem: &str) -> Result<()> {
    let body = store(cfg)
        .ensure_output_thumb(stem)
        .and_then(|p| Ok(std::fs::read(p)?));
    match body {
        Ok(body) => conn.respond(200, "image/png", &body),
        Err(_) => conn.respond(404, "text/plain", b"not found"),
    }
}

fn handle_open_workitem(
    conn: &mut http::Connection,
    lanes: &Lanes,
    shelf: Shelf,
    body: &[u8],
) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct Dto {
        id: String,
    }

    match parse_json::<Dto>(conn, body)? {
        // 400, same staleness contract as the escalations: an id that isn't
        // there means the UI's copy of the grid is stale, not that we broke.
        Some(dto) => submit(conn, &lanes.display, JobKind::Open { id: dto.id, shelf }, 400),
        None => Ok(()),
    }
}

/// Open a saved image so the viewer can filter it. A `work/output/` stem rather
/// than a path, the same contract the img2img source and the video first frame
/// use: the client picks from what `GET /outputs` published, and
/// `workitems::valid_id` — reached through `Engine::output_source` — is the
/// whitelist that makes that safe.
///
/// 400 for a stem that isn't there, matching the rest: it means the page's copy
/// of the grid is stale, not that we broke.
fn handle_open_output(
    conn: &mut http::Connection,
    lanes: &Lanes,
    body: &[u8],
) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct Dto {
        stem: String,
        /// The Image tab's model, for a picture whose sidecar names none
        /// installed (`Display::open_output`).
        #[serde(default)]
        model: Option<String>,
    }

    match parse_json::<Dto>(conn, body)? {
        Some(dto) => submit(
            conn,
            &lanes.display,
            JobKind::OpenOutput { stem: dto.stem, fallback: dto.model },
            400,
        ),
        None => Ok(()),
    }
}

/// Delete is a list, not a single id: the UI selects a set and then deletes it in
/// one go, which is what keeps culling a batch render from being a click per
/// image with a confirmation on each.
fn handle_delete_workitems(
    conn: &mut http::Connection,
    cfg: &Config,
    shelf: Shelf,
    body: &[u8],
) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct Dto {
        ids: Vec<String>,
    }

    let Some(dto) = parse_json::<Dto>(conn, body)? else {
        return Ok(());
    };
    let store = store(cfg);
    let mut deleted = 0usize;
    for id in &dto.ids {
        match store.delete(shelf, id) {
            Ok(()) => deleted += 1,
            Err(e) => eprintln!("[yasdig] {e:#}"),
        }
    }
    conn.respond(
        200,
        "application/json",
        format!("{{\"deleted\":{deleted}}}").as_bytes(),
    )
}

/// Star (`keep`) or un-star work items: move them to `user/favorite/`, or back
/// to `work/workitems/` ([`Store::favorite`], [`Store::unfavorite`]).
///
/// A list rather than one id, for the reason `/workitems/delete` is one, and
/// answered on the connection thread beside it, because it is a few file
/// copies and touches no GPU. Un-starring is `remove`, not `/favorites/delete`:
/// that one deletes the files, this one puts them back where `Delete all` can
/// reach them.
///
/// Failures are logged and skipped, exactly as the delete loop does: an id the
/// grid has that the disk hasn't means the page's copy is stale, and taking the
/// whole request down over it would lose the ones that did work.
fn handle_favorite(
    conn: &mut http::Connection,
    cfg: &Config,
    body: &[u8],
    keep: bool,
) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct Dto {
        ids: Vec<String>,
    }

    let Some(dto) = parse_json::<Dto>(conn, body)? else {
        return Ok(());
    };
    let store = store(cfg);
    let mut favorited = 0usize;
    for id in &dto.ids {
        let moved = if keep { store.favorite(id) } else { store.unfavorite(id) };
        match moved {
            Ok(()) => favorited += 1,
            Err(e) => eprintln!("[yasdig] {e:#}"),
        }
    }
    conn.respond(
        200,
        "application/json",
        format!("{{\"favorited\":{favorited}}}").as_bytes(),
    )
}

/// Remove saved images. The filesystem half of the Outputs tab, and so answered
/// on the connection thread beside `/workitems/delete` rather than through the
/// queue — the worker only ever *writes* to that directory.
///
/// A list, not a single stem, for the reason the work-item delete is one: the
/// page marks a set and then removes it in one go. "Delete all" is that same
/// request with every stem in it rather than a route of its own, which is what
/// keeps `valid_id` the only guard there is — a route that deleted a directory
/// wholesale would need a different kind of check entirely.
fn handle_delete_outputs(
    conn: &mut http::Connection,
    cfg: &Config,
    gallery: Gallery,
    body: &[u8],
) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct Dto {
        stems: Vec<String>,
    }

    let Some(dto) = parse_json::<Dto>(conn, body)? else {
        return Ok(());
    };
    let store = store(cfg);
    let mut deleted = 0usize;
    for stem in &dto.stems {
        match store.delete_output(gallery, stem) {
            Ok(()) => deleted += 1,
            Err(e) => eprintln!("[yasdig] {e:#}"),
        }
    }
    conn.respond(
        200,
        "application/json",
        format!("{{\"deleted\":{deleted}}}").as_bytes(),
    )
}

/// Star or un-star saved images: [`handle_favorite`] for the Outputs tab, and
/// the same shape for the same reasons — a list, answered here, a failure
/// logged and skipped.
fn handle_favorite_outputs(
    conn: &mut http::Connection,
    cfg: &Config,
    body: &[u8],
    keep: bool,
) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct Dto {
        stems: Vec<String>,
    }

    let Some(dto) = parse_json::<Dto>(conn, body)? else {
        return Ok(());
    };
    let store = store(cfg);
    let mut favorited = 0usize;
    for stem in &dto.stems {
        let moved = if keep { store.favorite_output(stem) } else { store.unfavorite_output(stem) };
        match moved {
            Ok(()) => favorited += 1,
            Err(e) => eprintln!("[yasdig] {e:#}"),
        }
    }
    conn.respond(
        200,
        "application/json",
        format!("{{\"favorited\":{favorited}}}").as_bytes(),
    )
}

/// `/save-output` mints a new file; `/outputs/overwrite` writes back over the
/// one the viewer opened. Same body, same lane.
fn handle_save_output(
    conn: &mut http::Connection,
    lanes: &Lanes,
    body: &[u8],
    overwrite: bool,
) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct Dto {
        image_id: u64,
    }

    match parse_json::<Dto>(conn, body)? {
        Some(dto) => {
            let kind = if overwrite {
                JobKind::OverwriteOutput(dto.image_id)
            } else {
                JobKind::SaveOutput(dto.image_id)
            };
            submit(conn, &lanes.display, kind, 400)
        }
        None => Ok(()),
    }
}

/// `/download-image`: the picture on screen as a file, with a name for it. A
/// POST through the display lane like `/save-output`, since only the retained
/// image knows which file is on screen. (Not `/downloads/…`: those fetch models.)
fn handle_download_image(conn: &mut http::Connection, lanes: &Lanes, body: &[u8]) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct Dto {
        image_id: u64,
    }

    match parse_json::<Dto>(conn, body)? {
        Some(dto) => submit(conn, &lanes.display, JobKind::DownloadImage(dto.image_id), 400),
        None => Ok(()),
    }
}

fn handle_generate(conn: &mut http::Connection, lanes: &Lanes, body: &[u8]) -> Result<()> {
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
        sampler: Option<String>,
        #[serde(default)]
        scheduler: Option<String>,
        cache_mode: Option<String>,
        #[serde(default)]
        memory: Option<String>,
        #[serde(default)]
        width: Option<u32>,
        #[serde(default)]
        height: Option<u32>,
        #[serde(default)]
        upscale_factor: Option<f32>,
        /// An ESRGAN file under the upscaler dir, or `""` for FSR. Absent keeps
        /// the default. See [`GenRequest::upscaler`].
        #[serde(default)]
        upscaler: Option<String>,
        #[serde(default)]
        refine_steps: Option<u32>,
        #[serde(default)]
        refine_strength: Option<f32>,
        #[serde(default)]
        refine_cfg_scale: Option<f32>,
        #[serde(default)]
        hires_steps: Option<u32>,
        #[serde(default)]
        hires_strength: Option<f32>,
        #[serde(default)]
        hires_cfg_scale: Option<f32>,
        /// The after-each passes' own prompts; see `EscalateRequest`.
        #[serde(default)]
        refine_prompt: Option<String>,
        #[serde(default)]
        hires_prompt: Option<String>,
        /// The two escalation checkpoints, as the *generate* body spells them.
        /// They name no pass this request runs; they are archived with the work
        /// item so reopening it restores the pickers. `/refine` and `/hires` take
        /// theirs as `model` instead, because there it is the pass about to run.
        #[serde(default)]
        refine_model: Option<String>,
        #[serde(default)]
        hires_model: Option<String>,
        #[serde(default)]
        sharpness: Option<f32>,
        #[serde(default)]
        clarity: Option<f32>,
        /// Absent keeps the profile's list, `[]` clears it. The page always
        /// sends the picker's full contents, so in practice it is always one or
        /// the other and never a merge.
        #[serde(default)]
        loras: Option<Vec<LoraRef>>,
        /// Reference images (output stems), in the order the prompt names
        /// them. Always sent for a family that takes them; `[]` is none.
        #[serde(default)]
        references: Option<Vec<String>>,
        /// See `GenRequest::parts`. A `null` inside the map is kept as `None`
        /// — it means "empty this slot" — so no `double_option` is needed.
        #[serde(default)]
        parts: Option<std::collections::BTreeMap<String, Option<String>>>,
        /// How many images, and whether to keep going until stopped. One
        /// request is one queued run, whatever its length — see `crate::jobs`.
        #[serde(default)]
        count: Option<u32>,
        #[serde(default, rename = "loop")]
        looping: bool,
        /// With `loop`: only update the running loop, never start one. The
        /// page's live edits send it (`syncLoop`).
        #[serde(default)]
        update_only: bool,
        /// The *After each generation* fold.
        #[serde(default)]
        after: AfterEach,
    }

    let dto: Dto = match parse_json::<Dto>(conn, body)? {
        Some(d) => d,
        None => return Ok(()),
    };

    let mut escalate = run_escalation(
        dto.refine_steps,
        dto.refine_strength,
        dto.refine_cfg_scale,
        dto.hires_steps,
        dto.hires_strength,
        dto.hires_cfg_scale,
    );
    // The fold's pass prompts, for the after-each passes. `{variables}` roll
    // per image, off each image's own seed (`Engine::pass_prompt`).
    escalate.refine_prompt = dto.refine_prompt.clone();
    escalate.hires_prompt = dto.hires_prompt.clone();
    let total = (!dto.looping).then(|| dto.count.unwrap_or(1));
    let after = dto.after;

    let req = GenRequest {
        prompt: dto.prompt,
        template: dto.template,
        negative: dto.negative,
        seed: dto.seed.unwrap_or(-1),
        steps: dto.steps,
        cfg_scale: dto.cfg_scale,
        sampler: dto.sampler,
        scheduler: dto.scheduler,
        cache_mode: dto.cache_mode,
        memory: dto.memory,
        width: dto.width,
        height: dto.height,
        upscale_factor: dto.upscale_factor,
        upscaler: dto.upscaler,
        refine_steps: dto.refine_steps,
        refine_strength: dto.refine_strength,
        refine_cfg_scale: dto.refine_cfg_scale,
        hires_steps: dto.hires_steps,
        hires_strength: dto.hires_strength,
        hires_cfg_scale: dto.hires_cfg_scale,
        refine_model: dto.refine_model,
        hires_model: dto.hires_model,
        sharpness: dto.sharpness,
        clarity: dto.clarity,
        loras: dto.loras,
        references: dto.references,
        parts: dto.parts,
    };

    let label = req.template.clone();
    let update_only = dto.update_only;
    queue_run(conn, lanes, jobs::Kind::Generate, label, total, update_only, RunPlan {
        start: Start::Generate(req),
        after,
        escalate,
    })
}

/// The escalation knobs a generate body carries, as the after-each passes
/// send them — what the Refine and Hi-res buttons would send with the panel as
/// it stood. The two CFGs are *sent*, `null` included: the page always sends
/// them, and an empty box unsets the pass's CFG, exactly as on the buttons.
fn run_escalation(
    refine_steps: Option<u32>,
    refine_strength: Option<f32>,
    refine_cfg_scale: Option<f32>,
    hires_steps: Option<u32>,
    hires_strength: Option<f32>,
    hires_cfg_scale: Option<f32>,
) -> EscalateRequest {
    EscalateRequest {
        template: None,
        refine_steps,
        refine_strength,
        hires_steps,
        hires_strength,
        refine_cfg_scale: Some(refine_cfg_scale),
        hires_cfg_scale: Some(hires_cfg_scale),
        // Set by the caller off the same body: they are strings, and passing
        // them through here would take this to eight positional arguments.
        refine_prompt: None,
        hires_prompt: None,
        // The after-each passes run with the generate's own, already on the
        // retained profile.
        memory: None,
        // Never: an after-each refine finishes the picture, and the
        // generation's references would pull it toward them.
        references: Vec::new(),
    }
}

/// Queue a run and answer at once with its id. The images arrive through
/// `GET /jobs` and `/jobs/latest`, not on this request — a run can be a loop,
/// and nothing about it should depend on this connection staying open.
///
/// What the generate itself would reject — an unknown model, an empty prompt —
/// is found by the worker and comes back as a notice, the same way a failure on
/// image 7 of 8 does. Checking it twice, here and there, would be two copies of
/// the rules.
fn queue_run(
    conn: &mut http::Connection,
    lanes: &Lanes,
    kind: jobs::Kind,
    label: String,
    total: Option<u32>,
    update_only: bool,
    plan: RunPlan,
) -> Result<()> {
    if total.is_some_and(|n| n == 0 || n > MAX_RUN) {
        return conn.respond(
            400,
            "application/json",
            error_json(&format!(
                "a run is 1 to {MAX_RUN} images — for more, use Keep going until stopped"
            ))
            .as_bytes(),
        );
    }
    let work = Work::Run(Box::new(plan));
    // A loop request while a loop exists is that loop's new settings, from its
    // next image on (`Queue::push_loop`): there is one loop, and it follows
    // the page the way the page's own loop once did.
    let (id, updated) = match total {
        Some(_) => (lanes.queue.push(kind, label, total, work, None, false), false),
        None if update_only => match lanes.queue.update_loop(kind, label, work) {
            Some(id) => (id, true),
            None => return conn.respond(200, "application/json", b"{\"job\":null,\"updated\":false}"),
        },
        None => lanes.queue.push_loop(kind, label, work),
    };
    conn.respond(
        200,
        "application/json",
        format!("{{\"job\":{id},\"updated\":{updated}}}").as_bytes(),
    )
}

/// One img2img pass over an image already in `work/output/` — a saved result or
/// something the user uploaded.
///
/// The source is validated **here**, on the connection thread, rather than in
/// the worker: a stale pick is a client mistake and deserves an immediate 400
/// rather than one that arrives after whatever render is in flight has finished.
/// Same split as `/convert`. The pass itself queues, and a failure in it is a
/// 500 like `/generate`'s, because by then the request was well-formed.
fn handle_img2img(
    conn: &mut http::Connection,
    cfg: &Config,
    lanes: &Lanes,
    body: &[u8],
) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct Dto {
        prompt: String,
        template: String,
        source: String,
        #[serde(default)]
        negative: Option<String>,
        #[serde(default)]
        seed: Option<i64>,
        #[serde(default)]
        max_side: Option<u32>,
        #[serde(default)]
        source_strength: Option<f32>,
        #[serde(default)]
        steps: Option<u32>,
        #[serde(default)]
        cfg_scale: Option<f32>,
        #[serde(default)]
        sampler: Option<String>,
        #[serde(default)]
        scheduler: Option<String>,
        cache_mode: Option<String>,
        #[serde(default)]
        memory: Option<String>,
        #[serde(default)]
        refine_steps: Option<u32>,
        #[serde(default)]
        refine_strength: Option<f32>,
        #[serde(default)]
        refine_cfg_scale: Option<f32>,
        #[serde(default)]
        hires_steps: Option<u32>,
        #[serde(default)]
        hires_strength: Option<f32>,
        #[serde(default)]
        hires_cfg_scale: Option<f32>,
        /// The after-each passes' own prompts; see `EscalateRequest`.
        #[serde(default)]
        refine_prompt: Option<String>,
        #[serde(default)]
        hires_prompt: Option<String>,
        /// See `/generate`'s. No `upscale_factor` beside them: this pass has no
        /// EASU stage to scale.
        #[serde(default)]
        refine_model: Option<String>,
        #[serde(default)]
        hires_model: Option<String>,
        #[serde(default)]
        sharpness: Option<f32>,
        #[serde(default)]
        clarity: Option<f32>,
        /// See `/generate`'s.
        #[serde(default)]
        loras: Option<Vec<LoraRef>>,
        /// See `GenRequest::parts`. A `null` inside the map is kept as `None`
        /// — it means "empty this slot" — so no `double_option` is needed.
        #[serde(default)]
        parts: Option<std::collections::BTreeMap<String, Option<String>>>,
        /// How many images, and whether to keep going until stopped. One
        /// request is one queued run, whatever its length — see `crate::jobs`.
        #[serde(default)]
        count: Option<u32>,
        #[serde(default, rename = "loop")]
        looping: bool,
        /// With `loop`: only update the running loop, never start one. The
        /// page's live edits send it (`syncLoop`).
        #[serde(default)]
        update_only: bool,
        /// The *After each generation* fold.
        #[serde(default)]
        after: AfterEach,
    }

    let dto: Dto = match parse_json::<Dto>(conn, body)? {
        Some(d) => d,
        None => return Ok(()),
    };

    if store(cfg).find_output(&dto.source).is_none() {
        return conn.respond(
            400,
            "application/json",
            error_json(&format!(
                "no saved image '{}' to start from — pick one from the grid or \
                 upload a new one",
                dto.source
            ))
            .as_bytes(),
        );
    }


    let mut escalate = run_escalation(
        dto.refine_steps,
        dto.refine_strength,
        dto.refine_cfg_scale,
        dto.hires_steps,
        dto.hires_strength,
        dto.hires_cfg_scale,
    );
    // The fold's pass prompts, for the after-each passes. `{variables}` roll
    // per image, off each image's own seed (`Engine::pass_prompt`).
    escalate.refine_prompt = dto.refine_prompt.clone();
    escalate.hires_prompt = dto.hires_prompt.clone();
    let total = (!dto.looping).then(|| dto.count.unwrap_or(1));
    let after = dto.after;

    let req = Img2ImgRequest {
        prompt: dto.prompt,
        template: dto.template,
        source: dto.source,
        negative: dto.negative,
        seed: dto.seed.unwrap_or(-1),
        max_side: dto.max_side,
        source_strength: dto.source_strength,
        steps: dto.steps,
        cfg_scale: dto.cfg_scale,
        sampler: dto.sampler,
        scheduler: dto.scheduler,
        cache_mode: dto.cache_mode,
        memory: dto.memory,
        refine_steps: dto.refine_steps,
        refine_strength: dto.refine_strength,
        refine_cfg_scale: dto.refine_cfg_scale,
        hires_steps: dto.hires_steps,
        hires_strength: dto.hires_strength,
        hires_cfg_scale: dto.hires_cfg_scale,
        refine_model: dto.refine_model,
        hires_model: dto.hires_model,
        sharpness: dto.sharpness,
        clarity: dto.clarity,
        loras: dto.loras,
        parts: dto.parts,
    };

    let label = req.template.clone();
    let update_only = dto.update_only;
    queue_run(conn, lanes, jobs::Kind::Img2img, label, total, update_only, RunPlan {
        start: Start::Img2Img(req),
        after,
        escalate,
    })
}

/// Take a PNG from the browser and put it in `work/output/`.
///
/// The one route with a binary body. `Request.body` is already a `Vec<u8>` and is
/// never UTF-8 validated, so the PNG arrives as-is — no multipart parser, no
/// base64, and a third fewer bytes on the wire than a JSON envelope. `http.rs`
/// gives this path, and only this path, a large body limit.
///
/// **PNG only, and the page is what makes that reasonable.** `image` is compiled
/// with the `png` feature alone, which is a deliberate line — this binary has no
/// business growing a JPEG decoder. The page runs whatever the user picked
/// through a `<canvas>` first, so JPEG, WebP, AVIF and anything else the browser
/// can decode all arrive here as PNG, and the format support tracks the browser's
/// rather than a crate's feature list.
///
/// Answered on the connection thread: a decode and two file writes, which must
/// not queue behind a render the user is uploading an image in order to replace.
fn handle_upload(conn: &mut http::Connection, cfg: &Config, body: &[u8]) -> Result<()> {
    // Decoded rather than trusted, for two reasons: the dimensions go in the
    // sidecar, and a body that isn't a PNG should fail now rather than minutes
    // into a diffusion that can't read its own init image.
    let img = match image::load_from_memory_with_format(body, image::ImageFormat::Png) {
        Ok(img) => img,
        Err(e) => {
            return conn.respond(
                400,
                "application/json",
                error_json(&format!("that isn't a PNG this build can read: {e}")).as_bytes(),
            )
        }
    };
    let (w, h) = (img.width(), img.height());
    drop(img);

    match store(cfg).import(body, w, h) {
        Ok(stem) => conn.respond(
            200,
            "application/json",
            format!("{{\"stem\":{},\"width\":{w},\"height\":{h}}}", json_str(&stem)).as_bytes(),
        ),
        Err(e) => conn.respond(
            500,
            "application/json",
            error_json(&format!("{e:#}")).as_bytes(),
        ),
    }
}

/// The settings a dropped PNG carries (`pngmeta::read`), matched against what is
/// installed. Raw PNG bytes, like `/upload` — but the original file's, since the
/// page's `<canvas>` re-encode would strip the very chunks this reads. Answered
/// here, not through a lane: it only reads the body and the model lists.
fn handle_png_settings(conn: &mut http::Connection, cfg: &Config, body: &[u8]) -> Result<()> {
    let install = pngmeta::Install {
        models: cfg.registry().into_keys().collect(),
        loras: cfg.list_loras().into_iter().filter(|l| l.usable).map(|l| l.name).collect(),
        upscalers: cfg.list_upscalers().into_iter().filter(|u| u.supported).map(|u| u.name).collect(),
    };
    match pngmeta::read(body, &install) {
        Ok(loaded) => conn.respond(200, "application/json", &serde_json::to_vec(&loaded)?),
        Err(e) => conn.respond(
            400,
            "application/json",
            error_json(&format!("{e:#} — only PNG files carry settings")).as_bytes(),
        ),
    }
}

/// The body `/refine` and `/hires` share: which earlier result to escalate, plus
/// the knobs that pass runs with. An unknown `image_id` means the intermediates
/// are gone (a newer image took its slot, or the server restarted), which is a
/// client-side staleness problem — 400, not 500. The same goes for asking for a pass the image can't
/// take.
///
/// Every knob is optional and falls back to the settings the retained image was
/// made with, so a bare `{image_id}` behaves exactly as it always did. Sending
/// them is what keeps the Advanced panel honest after a generate: edit "Refine
/// strength", press Refine, and the pass runs at what the box says.
#[derive(serde::Deserialize)]
struct EscalateDto {
    image_id: u64,
    /// Which checkpoint runs *this* pass. Named `model` rather than `template`
    /// on purpose: the page spreads this same object into the `/generate` body,
    /// where `template` already means the model doing the txt2img, and a second
    /// field by that name would silently clobber it.
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    refine_steps: Option<u32>,
    #[serde(default)]
    refine_strength: Option<f32>,
    #[serde(default)]
    hires_steps: Option<u32>,
    #[serde(default)]
    hires_strength: Option<f32>,
    /// The pass's own CFG. `deserialize_with` rather than a plain
    /// `Option<Option<f32>>`: serde folds an explicit `null` into `None`
    /// otherwise, and here "sent empty" and "not sent" have to mean different
    /// things — empty unsets it so the pass follows `cfg_scale` again. Only the
    /// one belonging to this route does anything; the page sends one panel.
    #[serde(default, deserialize_with = "double_option")]
    refine_cfg_scale: Option<Option<f32>>,
    #[serde(default, deserialize_with = "double_option")]
    hires_cfg_scale: Option<Option<f32>>,
    /// The pass's own prompt; absent, `null` and blank all mean the image's
    /// own. Only the one belonging to this route does anything.
    #[serde(default)]
    refine_prompt: Option<String>,
    #[serde(default)]
    hires_prompt: Option<String>,
    /// Where the weights live for this pass; absent keeps the image's.
    #[serde(default)]
    memory: Option<String>,
    /// Reference images (output stems) for a Refine to follow; absent is none.
    #[serde(default)]
    references: Vec<String>,
}

impl EscalateDto {
    fn overrides(&self) -> EscalateRequest {
        EscalateRequest {
            template: self.model.clone(),
            refine_steps: self.refine_steps,
            refine_strength: self.refine_strength,
            hires_steps: self.hires_steps,
            hires_strength: self.hires_strength,
            refine_cfg_scale: self.refine_cfg_scale,
            hires_cfg_scale: self.hires_cfg_scale,
            refine_prompt: self.refine_prompt.clone(),
            hires_prompt: self.hires_prompt.clone(),
            memory: self.memory.clone(),
            references: self.references.clone(),
        }
    }
}

fn double_option<'de, D, T>(de: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    serde::Deserialize::deserialize(de).map(Some)
}

fn handle_refine(conn: &mut http::Connection, lanes: &Lanes, body: &[u8]) -> Result<()> {
    match parse_json::<EscalateDto>(conn, body)? {
        Some(dto) => escalate(
            conn,
            lanes,
            jobs::Kind::Refine,
            dto.image_id,
            dto.model.clone(),
            Work::Refine {
                id: dto.image_id,
                over: dto.overrides(),
            },
        ),
        None => Ok(()),
    }
}

/// `POST /redraw {image_id, mask, prompt?, strength?, references?}`: redraw the painted
/// part of the image in hand. `mask` is a base64 PNG at the image's size, white
/// where to redraw — base64 in JSON rather than a raw body like `/upload`,
/// because it travels with three other fields, and a mask is mostly one colour
/// so its PNG is small. Queued and waited on like `/refine`.
fn handle_redraw(conn: &mut http::Connection, lanes: &Lanes, body: &[u8]) -> Result<()> {
    let Some(dto) = parse_json::<RedrawDto>(conn, body)? else {
        return Ok(());
    };
    let mask_png = match dto.mask_png() {
        Ok(m) => m,
        Err(e) => {
            return conn.respond(400, "application/json", error_json(&format!("{e:#}")).as_bytes())
        }
    };
    escalate(
        conn,
        lanes,
        jobs::Kind::Redraw,
        dto.image_id,
        None,
        Work::Redraw {
            id: dto.image_id,
            req: pipeline::RedrawRequest {
                mask_png,
                prompt: dto.prompt,
                strength: dto.strength,
                feather: dto.feather,
                memory: dto.memory,
                references: dto.references,
            },
        },
    )
}

#[derive(serde::Deserialize)]
struct RedrawDto {
    image_id: u64,
    mask: String,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    strength: Option<f32>,
    #[serde(default)]
    feather: Option<u32>,
    #[serde(default)]
    memory: Option<String>,
    /// Reference images to follow, as on `/refine`; absent is none.
    #[serde(default)]
    references: Vec<String>,
}

impl RedrawDto {
    /// The mask's PNG bytes. Accepts a `data:` URL as well, since that is what
    /// a canvas hands the page, and stripping it there would be one more place
    /// to get the prefix wrong.
    fn mask_png(&self) -> Result<Vec<u8>> {
        data_url_bytes(&self.mask)
    }
}

/// A `data:image/png;base64,…` URL (or bare base64) as bytes: what a canvas
/// hands the page, for Redraw's mask and a brush's.
fn data_url_bytes(url: &str) -> Result<Vec<u8>> {
    let b64 = url.split_once("base64,").map_or(url, |(_, b)| b);
    base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .context("the mask is not valid base64")
}

/// `POST /edit {image_id, prompt, references?, expand?}`: change the image by
/// instruction, the image itself as reference 1 — and with `expand`
/// (`{left, top, right, bottom}` px), redraw it in that grown shape. Queued and waited on like
/// `/refine`.
fn handle_edit(conn: &mut http::Connection, lanes: &Lanes, body: &[u8]) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct Dto {
        image_id: u64,
        #[serde(default)]
        prompt: Option<String>,
        #[serde(default)]
        references: Vec<String>,
        #[serde(default)]
        memory: Option<String>,
        #[serde(default)]
        expand: pipeline::Expand,
    }
    let Some(dto) = parse_json::<Dto>(conn, body)? else {
        return Ok(());
    };
    let id = dto.image_id;
    let req = pipeline::EditRequest {
        prompt: dto.prompt,
        references: dto.references,
        memory: dto.memory,
        expand: dto.expand,
    };
    escalate(conn, lanes, jobs::Kind::Edit, id, None, Work::Edit { id, req })
}

/// `POST /extend {image_id, expand: {left?, top?, right?, bottom?}, prompt?,
/// feather?, memory?}`: grow the image in hand on the sides given, in pixels, and draw
/// past the old edges with FLUX.1 Fill. Queued and waited on like `/refine`.
fn handle_extend(conn: &mut http::Connection, lanes: &Lanes, body: &[u8]) -> Result<()> {
    let Some(dto) = parse_json::<ExtendDto>(conn, body)? else {
        return Ok(());
    };
    let id = dto.image_id;
    escalate(conn, lanes, jobs::Kind::Extend, id, None, Work::Extend { id, req: dto.request() })
}

#[derive(serde::Deserialize)]
struct ExtendDto {
    image_id: u64,
    #[serde(default)]
    expand: pipeline::Expand,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    feather: Option<u32>,
    #[serde(default)]
    memory: Option<String>,
}

impl ExtendDto {
    fn request(self) -> pipeline::ExtendRequest {
        pipeline::ExtendRequest {
            expand: self.expand,
            prompt: self.prompt,
            feather: self.feather,
            memory: self.memory,
        }
    }
}

/// `POST /upscale {image_id, factor, upscaler?}`: enlarge the image in hand
/// with FSR (`upscaler` absent, `null` or empty) or a model from the upscalers
/// folder. Queued and waited on like `/refine`; no diffusion.
fn handle_upscale(conn: &mut http::Connection, lanes: &Lanes, body: &[u8]) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct Dto {
        image_id: u64,
        factor: f32,
        #[serde(default)]
        upscaler: Option<String>,
    }
    let Some(dto) = parse_json::<Dto>(conn, body)? else {
        return Ok(());
    };
    escalate(
        conn,
        lanes,
        jobs::Kind::Upscale,
        dto.image_id,
        None,
        Work::Upscale {
            id: dto.image_id,
            factor: dto.factor,
            upscaler: dto.upscaler.filter(|u| !u.is_empty()),
        },
    )
}

/// `POST /history/goto {image_id, index}`: make a step of the image's history
/// the live one. The display lane's, so it never waits behind a render.
fn handle_goto(conn: &mut http::Connection, lanes: &Lanes, body: &[u8]) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct Dto {
        image_id: u64,
        index: usize,
    }
    match parse_json::<Dto>(conn, body)? {
        Some(dto) => submit(
            conn,
            &lanes.display,
            JobKind::Goto {
                id: dto.image_id,
                index: dto.index,
            },
            400,
        ),
        None => Ok(()),
    }
}

/// `POST /image/model {image_id, model}`: make `model` the one the image's
/// passes run on — the Refine dialog's picker for a picture opened from
/// Outputs. The display lane's: it changes no pixels.
fn handle_set_model(conn: &mut http::Connection, lanes: &Lanes, body: &[u8]) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct Dto {
        image_id: u64,
        model: String,
    }
    match parse_json::<Dto>(conn, body)? {
        Some(dto) => submit(conn, &lanes.display, JobKind::SetModel { id: dto.image_id, model: dto.model }, 400),
        None => Ok(()),
    }
}

/// `POST /history/view {image_id, index}`: a step as the page would show it,
/// with today's filters, for the compare slider. Changes nothing.
fn handle_view_step(conn: &mut http::Connection, lanes: &Lanes, body: &[u8]) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct Dto {
        image_id: u64,
        index: usize,
    }
    match parse_json::<Dto>(conn, body)? {
        Some(dto) => submit(
            conn,
            &lanes.display,
            JobKind::ViewStep {
                id: dto.image_id,
                index: dto.index,
            },
            400,
        ),
        None => Ok(()),
    }
}

/// `/history/{image_id}/{index}.png` → the two numbers, or `None` for any
/// path that isn't exactly that.
fn history_path(path: &str) -> Option<(u64, usize)> {
    let rest = path.strip_prefix("/history/")?.strip_suffix(".png")?;
    let (id, index) = rest.split_once('/')?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(id) || !digits(index) {
        return None;
    }
    Some((id.parse().ok()?, index.parse().ok()?))
}

/// `/hires` is `/refine` plus the enlarge factor. Flattened rather than a sixth
/// field on `EscalateDto`, so refine can't be sent an upscale it would ignore.
#[derive(serde::Deserialize)]
struct HiresDto {
    #[serde(flatten)]
    esc: EscalateDto,
    #[serde(default)]
    upscale: Option<f32>,
}

fn handle_hires(conn: &mut http::Connection, lanes: &Lanes, body: &[u8]) -> Result<()> {
    match parse_json::<HiresDto>(conn, body)? {
        Some(dto) => escalate(
            conn,
            lanes,
            jobs::Kind::Hires,
            dto.esc.image_id,
            dto.esc.model.clone(),
            Work::Hires {
                id: dto.esc.image_id,
                upscale: dto.upscale.unwrap_or(1.0),
                over: dto.esc.overrides(),
            },
        ),
        None => Ok(()),
    }
}

/// Re-filter an earlier result. Same staleness contract as the escalations — an
/// unknown id is 400 — but no diffusion runs, so this is the one handler that
/// comes back in milliseconds and can be driven live from the UI's sliders.
/// An omitted field keeps whatever the image currently has.
fn handle_adjust(conn: &mut http::Connection, lanes: &Lanes, body: &[u8]) -> Result<()> {
    // Flattened rather than nested under a `filters` key, so the body stays the
    // flat `{"image_id":1,"sharpness":0.5}` it has always been and simply gains
    // eight more optional fields.
    #[derive(serde::Deserialize)]
    struct Dto {
        image_id: u64,
        #[serde(flatten)]
        patch: FiltersPatch,
        /// The area's whole state: absent keeps it, `null` removes it.
        #[serde(default, deserialize_with = "double_option")]
        area: Option<Option<area::Area>>,
    }

    match parse_json::<Dto>(conn, body)? {
        Some(dto) => submit(
            conn,
            &lanes.display,
            JobKind::Adjust {
                id: dto.image_id,
                patch: dto.patch,
                area: dto.area,
            },
            400,
        ),
        None => Ok(()),
    }
}

/// `POST /brush {image_id, tool, amount?, mask?}`: paint with one brush tool
/// (`mask` a PNG data URL of the whole mask), clear it (`mask: null`), or —
/// with no `mask` — set only how strong it is. Display lane.
fn handle_brush(conn: &mut http::Connection, lanes: &Lanes, body: &[u8]) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct Dto {
        image_id: u64,
        tool: String,
        #[serde(default)]
        amount: Option<f32>,
        #[serde(default, deserialize_with = "double_option")]
        mask: Option<Option<String>>,
        /// Restore's: the History step it paints back in.
        #[serde(default)]
        from: Option<usize>,
    }
    let Some(d) = parse_json::<Dto>(conn, body)? else {
        return Ok(());
    };
    let Some(tool) = brush::Tool::parse(&d.tool) else {
        return conn.respond(400, "application/json", error_json(&format!("no brush called '{}'", d.tool)).as_bytes());
    };
    let mask = match d.mask {
        None => None,
        Some(None) => Some(None),
        Some(Some(url)) => match data_url_bytes(&url) {
            Ok(png) => Some(Some(png)),
            Err(e) => return conn.respond(400, "application/json", error_json(&format!("{e:#}")).as_bytes()),
        },
    };
    submit(conn, &lanes.display, JobKind::Brush { id: d.image_id, tool, amount: d.amount, mask, from: d.from }, 400)
}

/// `/brush/{image_id}/{tool}.png` → the id and the tool, or `None`.
fn brush_path(path: &str) -> Option<(u64, brush::Tool)> {
    let (id, tool) = path.strip_prefix("/brush/")?.strip_suffix(".png")?.split_once('/')?;
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((id.parse().ok()?, brush::Tool::parse(tool)?))
}

/// `POST /apply {image_id}`: *Apply adjustments* — the look becomes the
/// picture, as a history step (`Display::apply_look`). Display lane.
fn handle_apply(conn: &mut http::Connection, lanes: &Lanes, body: &[u8]) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct Dto {
        image_id: u64,
    }
    match parse_json::<Dto>(conn, body)? {
        Some(d) => submit(conn, &lanes.display, JobKind::Apply(d.image_id), 400),
        None => Ok(()),
    }
}

/// `POST /text {image_id, layer, label}`: the dialog's Text panel — `layer`
/// a transparent PNG (data URL) the page drew at the picture's size, laid over
/// the picture as it looks, as a step (`Display::text`). Display lane.
fn handle_text(conn: &mut http::Connection, lanes: &Lanes, body: &[u8]) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct Dto {
        image_id: u64,
        layer: String,
        #[serde(default)]
        label: String,
    }
    let Some(d) = parse_json::<Dto>(conn, body)? else { return Ok(()) };
    let layer = match data_url_bytes(&d.layer) {
        Ok(b) => b,
        Err(e) => return conn.respond(400, "application/json", error_body(&e).as_bytes()),
    };
    submit(conn, &lanes.display, JobKind::Text { id: d.image_id, layer, label: d.label }, 400)
}

/// `POST /crop {image_id, x, y, width, height}`: crop the image, in its own
/// pixels, as a history step (`Display::crop`). The display lane's: no
/// diffusion, so it answers mid-render like the sliders.
fn handle_crop(conn: &mut http::Connection, lanes: &Lanes, body: &[u8]) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct Dto {
        image_id: u64,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    }
    match parse_json::<Dto>(conn, body)? {
        Some(d) => submit(
            conn,
            &lanes.display,
            JobKind::Crop { id: d.image_id, rect: (d.x, d.y, d.width, d.height) },
            400,
        ),
        None => Ok(()),
    }
}

fn handle_resize(conn: &mut http::Connection, lanes: &Lanes, body: &[u8]) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct Dto {
        image_id: u64,
        factor: f32,
    }
    match parse_json::<Dto>(conn, body)? {
        Some(d) => submit(conn, &lanes.display, JobKind::Resize { id: d.image_id, factor: d.factor }, 400),
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

/// A Refine or Hi-res button press: pin the image so a run landing meanwhile
/// can't delete what this pass will read, queue it ahead of everything that
/// isn't running, and wait for it. 400 when the image is already gone — the
/// page's copy is stale, which is the contract these routes always had.
fn escalate(
    conn: &mut http::Connection,
    lanes: &Lanes,
    kind: jobs::Kind,
    id: u64,
    model: Option<String>,
    work: Work,
) -> Result<()> {
    let Some(made_with) = lock(&lanes.retained).pin(id) else {
        return conn.respond(
            400,
            "application/json",
            error_json(
                "that image is no longer available — a newer one has replaced it, or the \
                 server restarted. Click it in the work items below to reopen it.",
            )
            .as_bytes(),
        );
    };
    let label = model.filter(|m| !m.is_empty()).unwrap_or(made_with);
    enqueue_wait(conn, lanes, kind, label, work, true, 400)
}

/// Queue diffusion-lane work, wait for its turn and its result, and write the
/// JSON response. `err_status` is the status used when the job itself fails.
///
/// The connection waits for as long as the queue ahead of it takes. That is
/// fine for a connection thread, and it keeps these routes request/response —
/// the clip, the conversion and the escalated image come back on the request
/// that asked for them, as they always did. A page that goes away meanwhile
/// loses only the answer: the clip is archived and the image is the latest.
fn enqueue_wait(
    conn: &mut http::Connection,
    lanes: &Lanes,
    kind: jobs::Kind,
    label: String,
    work: Work,
    priority: bool,
    err_status: u16,
) -> Result<()> {
    let (reply_tx, reply_rx) = mpsc::channel();
    lanes.queue.push(kind, label, None, work, Some(reply_tx), priority);
    let result = reply_rx
        .recv()
        .context("gpu worker dropped the reply channel")?;
    match result {
        Ok(json) => conn.respond(200, "application/json", json.as_bytes()),
        Err(e) if e.downcast_ref::<pipeline::Superseded>().is_some() => {
            conn.respond(409, "application/json", error_body(&e).as_bytes())
        }
        Err(e) => conn.respond(err_status, "application/json", error_body(&e).as_bytes()),
    }
}

/// `GET /jobs`: the queue, and what the running job is doing right now — one
/// poll for the Jobs button, its dialog and every tab's progress line.
fn handle_jobs(conn: &mut http::Connection, queue: &Jobs, monitor: &Monitor) -> Result<()> {
    let mut body = serde_json::to_value(queue.snapshot())?;
    body["progress"] = serde_json::from_str(&progress_json(monitor))?;
    conn.respond(200, "application/json", body.to_string().as_bytes())
}

/// `POST /jobs/stop {id}`: the Jobs dialog's one button. Waiting, it is
/// Remove; running, a run finishes the image in flight (its passes and its save
/// included — dropping a refined image one call short of saving it is not what
/// anybody means by stop), and anything else is killed, as the video tab's
/// Stop always did.
fn handle_stop_job(
    conn: &mut http::Connection,
    lanes: &Lanes,
    monitor: &Monitor,
    body: &[u8],
) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct Dto {
        id: u64,
    }
    let Some(dto) = parse_json::<Dto>(conn, body)? else {
        return Ok(());
    };
    let what = match lanes.queue.stop(dto.id, || monitor.cancel()) {
        jobs::Stopped::Removed(work) => {
            // A queued button press was holding its image.
            if let Some(id) = work.pinned() {
                lock(&lanes.retained).unpin(id);
            }
            "removed"
        }
        jobs::Stopped::Stopping => "stopping",
        jobs::Stopped::Cancel(killed) => {
            // Only an sd-cli run can be killed. FSR and the `sr` models run on
            // this process's own GPU device, a pass at a time, with nothing to
            // stop part-way — say so, rather than answer "cancelled" and let it
            // finish anyway.
            if !killed {
                return conn.respond(
                    400,
                    "application/json",
                    error_json("this pass runs on the GPU directly and can't be stopped part-way — it will finish on its own in a moment").as_bytes(),
                );
            }
            "cancelled"
        }
        jobs::Stopped::Unknown => {
            return conn.respond(
                400,
                "application/json",
                error_json("that job has already finished").as_bytes(),
            )
        }
    };
    conn.respond(
        200,
        "application/json",
        format!("{{\"stopped\":\"{what}\"}}").as_bytes(),
    )
}

/// `POST /jobs/skip {id}`: *Cancel generation*. Kills the image a run has in
/// flight and lets the run go on to its next one — the button for a preview
/// whose composition is already wrong. Marked before the kill, so the worker
/// can never see the failure without the mark; taken back when there was no
/// child to kill (between two sd-cli passes), so it can't swallow a real
/// failure later.
fn handle_skip_job(
    conn: &mut http::Connection,
    lanes: &Lanes,
    monitor: &Monitor,
    body: &[u8],
) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct Dto {
        id: u64,
    }
    let Some(dto) = parse_json::<Dto>(conn, body)? else {
        return Ok(());
    };
    let msg = match lanes.queue.skip(dto.id, || monitor.cancel()) {
        jobs::Skip::Killed => return conn.respond(200, "application/json", b"{\"skipped\":true}"),
        jobs::Skip::NotInFlight => "that job has no image in flight to cancel",
        jobs::Skip::BetweenPasses => "between two passes — try again in a moment",
    };
    conn.respond(400, "application/json", error_json(msg).as_bytes())
}

/// Hand display-lane work to its worker, wait, and write the JSON response.
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
        Ok(json) => conn.respond(200, "application/json", json.as_bytes()),
        Err(e) if e.downcast_ref::<pipeline::Superseded>().is_some() => {
            conn.respond(409, "application/json", error_body(&e).as_bytes())
        }
        Err(e) => conn.respond(err_status, "application/json", error_body(&e).as_bytes()),
    }
}

/// The image plus its metadata as JSON. `can_hires` says whether `image_id` is
/// still inside the hi-res budget; whether either pass has steps is the page's
/// to read off its fold boxes, since those are what a press sends.
fn result_json(res: &GenResult) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(&res.png);
    // The filters go out as one nested object, serialized from the struct that
    // owns them rather than as ten more holes in this format string — the
    // convention's "serde_json for anything backed by a real struct", and the
    // thing that keeps slider eleven from touching this function at all.
    let filters = serde_json::to_string(&res.filters).unwrap_or_else(|_| "{}".to_string());
    let start = serde_json::to_string(&res.start).unwrap_or_else(|_| "{}".to_string());
    let area = serde_json::to_string(&res.area).unwrap_or_else(|_| "null".to_string());
    let brushes = serde_json::to_string(&res.brushes).unwrap_or_else(|_| "{}".to_string());
    let restore_from = res.restore_from.map_or("null".to_string(), |f| f.to_string());
    format!(
        "{{\"image_base64\":\"{}\",\"width\":{},\"height\":{},\"seed\":{},\"refine_passes\":{},\
         \"hires_passes\":{},\"step\":{},\"history\":{},\"at\":{},\"image_id\":{},\"work_id\":{},\"can_hires\":{},\"model\":{},\"can_edit\":{},\
         \"filters\":{},\"filters_start\":{},\"area\":{},\"brushes\":{},\"restore_from\":{},\"prompt\":{},\
         \"timings\":{{\"gen_ms\":{},\"upscale_ms\":{},\"fsr_ms\":{},\"refine_ms\":{},\"hires_ms\":{},\"redraw_ms\":{},\"cache_skipped\":{},\"cache_steps\":{},\"weights\":{},\"load_ms\":{},\"stages\":{}}}}}",
        b64,
        res.width,
        res.height,
        res.seed,
        res.refine_passes,
        res.hires_passes,
        res.step,
        serde_json::to_string(&res.history).unwrap_or_else(|_| "[]".to_string()),
        res.at,
        res.image_id,
        // `null` when the archive write failed, which is the only case there
        // isn't one. The UI uses it to mark which thumbnail is on screen.
        res.work_id
            .as_deref()
            .map(json_str)
            .unwrap_or_else(|| "null".to_string()),
        res.can_hires,
        json_str(&res.model),
        res.can_edit,
        filters,
        start,
        area,
        brushes,
        restore_from,
        // What was actually conditioned, so the page can show a `{variable}`
        // prompt's roll under the image.
        json_str(&res.prompt),
        res.timings.gen_ms,
        res.timings.upscale_ms,
        res.timings.fsr_ms,
        res.timings.refine_ms,
        res.timings.hires_ms,
        res.timings.redraw_ms,
        res.timings.cache_skipped,
        res.timings.cache_steps,
        res.timings.weights.as_deref().map_or("null".to_string(), json_str),
        res.timings.load_ms,
        serde_json::to_string(&res.timings.stages).unwrap_or_else(|_| "null".into()),
    )
}

/// Where a "Save to output" landed. Reported relative to the project root when
/// it can be, since that's how the user refers to `work/output/` themselves.
/// `serde_json` for a real struct, per the convention: the two hand-rolled
/// shapes are the long-standing ones, and a clip's sidecar is a 15-field
/// round-trip that has no business being assembled with `format!`.
fn json_body<T: serde::Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).context("serializing the response")
}

fn saved_json(path: &std::path::Path) -> String {
    let shown = path
        .strip_prefix(std::env::current_dir().unwrap_or_default())
        .unwrap_or(path);
    format!("{{\"path\":{}}}", json_str(&shown.display().to_string()))
}

/// What the worker is doing, or `{"busy":false}` when it is idle.
///
/// Hand-rolled like the other two long-standing shapes. Neither string needs
/// escaping and both are ours: `label` is one of a handful of literals this file
/// passes to `SdRunner`, and `rate` is sd.cpp's own `%.2f` plus a unit.
/// `preview` is the newest live frame's number for `GET /preview/{n}.png`, or
/// `null` while the run has none (loading, or a mode that writes none).
fn progress_json(monitor: &Monitor) -> String {
    progress_body(monitor.snapshot().as_ref(), monitor.preview())
}

fn progress_body(p: Option<&sd::Progress>, preview: Option<u32>) -> String {
    match p {
        Some(p) => format!(
            "{{\"busy\":true,\"label\":{},\"phase\":\"{}\",\"step\":{},\"steps\":{},\"rate\":{},\"preview\":{}}}",
            json_str(&p.label),
            p.phase.as_str(),
            p.step,
            p.steps,
            json_str(&p.rate),
            preview.map_or("null".to_string(), |n| n.to_string())
        ),
        None => "{\"busy\":false}".to_string(),
    }
}
/// An optional number as JSON: the number itself, or `null`.
///
/// Three fields need this and all three mean the same thing by an absent value —
/// "follows the knob below it" — so the page must be able to tell that from a
/// number that happens to match today. See `build_models_json`.
fn opt_num(v: Option<f32>) -> String {
    v.map(|v| v.to_string()).unwrap_or_else(|| "null".to_string())
}

/// The registry (templates + discovered checkpoints) as JSON for the UI. The
/// per-model defaults (steps/cfg/sampler/size) are included so the UI can
/// pre-fill the advanced controls when a model is selected.
fn build_models_json(cfg: &Config) -> String {
    // Which models have saved defaults of their own, so the page can say so
    // beside the select rather than leave a changed number unexplained.
    let tuned = settings::ModelSettings::load_or_default(&cfg.user_dir());
    // Every header here is memoized, so a second walk is a directory listing.
    let scan = cfg.scan_images();
    let items: Vec<String> = cfg
        .registry()
        .values()
        .map(|p| {
            let out_w = ((p.width as f32) * p.upscale_factor).round() as u32;
            let out_h = ((p.height as f32) * p.upscale_factor).round() as u32;
            // Every file standing between this model and a run, at the path it
            // is expected at, so a greyed-out option can say what it is waiting
            // for rather than only that it is waiting.
            let missing: Vec<String> =
                p.missing().iter().map(|f| json_str(&f.display().to_string())).collect();
            // The *Models* fold's slots, each with the files that fit it —
            // filtered here, so the page never decides what fits a family.
            // serde_json because it is a real struct, per the house rule.
            let parts = serde_json::to_string(&cfg.part_views(p, &scan))
                .unwrap_or_else(|_| "[]".into());
            format!(
                "{{\"name\":{},\"description\":{},\"discovered\":{},\"exists\":{},\
                 \"family\":{},\"family_label\":{},\"references\":{},\"fill\":{},\"tuned\":{},\"missing\":[{}],\"parts\":{},\
                 \"base_width\":{},\"base_height\":{},\"out_width\":{},\"out_height\":{},\
                 \"steps\":{},\"cfg_scale\":{},\"sampler\":{},\"scheduler\":{},\"cache_mode\":{},\"memory\":{},\
                 \"upscale_factor\":{},\"upscaler\":{},\
                 \"refine_steps\":{},\"refine_strength\":{},\"refine_cfg_scale\":{},\
                 \"hires_steps\":{},\"hires_strength\":{},\"hires_cfg_scale\":{},\
                 \"source_strength\":{},\"hires_max_side\":{},\
                 \"sharpness\":{},\"clarity\":{}}}",
                json_str(&p.name),
                json_str(&p.description),
                p.discovered,
                p.exists,
                json_str(p.family.as_str()),
                json_str(p.family.label()),
                // Whether the References button shows: the server's rule,
                // served, so the page holds no copy of it.
                p.family.takes_references(),
                p.family.fill_only(),
                tuned.model.contains_key(&p.name) || tuned.parts.contains_key(&p.name),
                missing.join(","),
                parts,
                p.width,
                p.height,
                out_w,
                out_h,
                p.steps,
                p.cfg_scale,
                json_str(&p.sampler),
                json_str(&p.scheduler),
                json_str(&p.cache_mode),
                json_str(&p.memory),
                // The panel's Output upscale box starts here. It is the one
                // generation knob that changes what "the image" is, so the page
                // needs the number and not just the `out_*` above, which is that
                // number already spent.
                p.upscale_factor,
                // `null` is FSR. What the Upscaler select starts at.
                p.upscaler
                    .as_deref()
                    .map(json_str)
                    .unwrap_or_else(|| "null".to_string()),
                p.refine_steps,
                p.refine_strength,
                // `null`, not the resolved number: the UI has to tell "unset"
                // from "set to exactly the base CFG", because they look the same
                // and diverge the moment the CFG box moves.
                opt_num(p.refine_cfg_scale),
                p.hires_steps,
                p.hires_strength,
                opt_num(p.hires_cfg_scale),
                p.source_strength,
                // Published because *Start from*'s working-size control
                // starts there: it is the same budget question — how large can
                // this card afford to diffuse — asked of the same hardware, so
                // the page has no business inventing a second answer to it.
                p.hires_max_side,
                p.sharpness,
                p.clarity,
            )
        })
        .collect();
    format!("[{}]", items.join(","))
}

/// A failed job's body: `{error}`, plus `oom` when it ran out of memory, so the
/// page can offer the Memory setting and the other ways out.
fn error_body(e: &anyhow::Error) -> String {
    let mut v = serde_json::json!({ "error": format!("{e:#}") });
    if let Some(oom) = sd::oom_json(e) {
        v["oom"] = oom;
    }
    v.to_string()
}

fn error_json(msg: &str) -> String {
    format!("{{\"error\":{}}}", json_str(msg))
}

/// `yasdig doctor [models.toml]`
/// Run the same startup checks and report, without starting the server.
fn run_doctor(args: &[String]) -> Result<()> {
    let config_path = home::config_path(args.get(0).map(String::as_str))?;
    let cfg = Config::load(&config_path)
        .with_context(|| format!("loading {}", config_path.display()))?;

    let report = preflight::check(&cfg);
    let ok = report.print();
    if ok {
        println!("[yasdig] all checks passed — ready to run.");
        Ok(())
    } else {
        std::process::exit(1);
    }
}

/// `yasdig fold-scales <file.gguf>...`
///
/// The repair for a GGUF converted from a scaled-fp8 checkpoint before
/// `Engine::convert` folded the scales itself — see `foldscale`. In place and
/// in seconds to minutes, where re-converting a 14B at `-t 1` takes hours. No
/// config, no sd-cli, no GPU.
fn run_fold_scales(args: &[String]) -> Result<()> {
    if args.is_empty() {
        eprintln!("usage: yasdig fold-scales <file.gguf>...");
        std::process::exit(2);
    }
    for a in args {
        let path = PathBuf::from(a);
        let start = std::time::Instant::now();
        let r = foldscale::fold(&path)?;
        if r.recovered {
            println!("[fold] {}: undid an interrupted fold first", path.display());
        }
        println!(
            "[fold] {}: folded {}, already folded {}, left alone {} — {} ms",
            path.display(),
            r.folded,
            r.already,
            r.skipped.len(),
            start.elapsed().as_millis()
        );
        for s in &r.skipped {
            println!("  left alone: {s}");
        }
    }
    Ok(())
}

/// `yasdig upscale <in.png> <out.png> <model> [factor] [models.toml]`
///
/// The initial enlarge on its own, exactly as a generate runs it: an ESRGAN
/// model through `sd-cli -M upscale`, then the Lanczos reduce or EASU remainder
/// that lands it on the size EASU would have made. No checkpoint and no server,
/// so it is the fast loop for trying a new upscaler file — and the place to see
/// that a file sd.cpp can't build is refused rather than passed through.
///
/// `model` is a name inside the upscaler dir, as the page's select sends it, or
/// a path to a file anywhere.
fn run_upscale_cli(args: &[String]) -> Result<()> {
    if args.len() < 3 {
        eprintln!(
            "usage: yasdig upscale <in.png> <out.png> <model> [factor=2.0] [models.toml]"
        );
        std::process::exit(2);
    }
    let in_png = PathBuf::from(&args[0]);
    let out_png = PathBuf::from(&args[1]);
    let factor: f32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(2.0);
    if !(1.0..=pipeline::MAX_UPSCALE).contains(&factor) {
        anyhow::bail!("factor must be between 1 and {}", pipeline::MAX_UPSCALE);
    }
    let config_path = home::config_path(args.get(4).map(String::as_str))?;
    let cfg = Config::load(&config_path)
        .with_context(|| format!("loading {}", config_path.display()))?;
    let model = match PathBuf::from(&args[2]) {
        p if p.is_file() => p,
        _ => cfg
            .check_upscaler(&Some(args[2].clone()))?
            .expect("a name was given"),
    };

    let arch = sr::identify(&model)?;
    println!(
        "[upscale] {}: {} at {}",
        model.display(),
        arch.name(),
        arch.scale().map(|s| format!("{s}×")).unwrap_or_else(|| "unknown scale".into())
    );
    let runner = sd::SdRunner::new(&cfg);
    // sd-cli only runs the ESRGANs; the rest never touch it, so a missing
    // binary is no reason to refuse them.
    if matches!(arch, sr::arch::Arch::Esrgan { .. }) {
        runner.preflight()?;
    }
    // A mutex because `enlarge` is the server's helper too, where the device
    // is shared between two lanes; here nobody else holds it.
    let fsr = std::sync::Mutex::new(fsr::Fsr::new()?);
    let start = std::time::Instant::now();
    let model_ms = pipeline::enlarge(&fsr, &runner, &in_png, &out_png, factor, Some(&model))?;
    let (w, h) = image::image_dimensions(&out_png)
        .with_context(|| format!("reading {}", out_png.display()))?;
    println!(
        "[upscale] {} -> {} : {w}x{h} (x{factor}) — model {model_ms} ms, total {} ms",
        in_png.display(),
        out_png.display(),
        start.elapsed().as_millis()
    );
    // The round trip, when the factor is a whole number: shrink the result
    // back and compare it with what went in. See `sr::consistency`.
    if factor.fract() == 0.0 {
        let input = image::open(&in_png)?.to_rgb8();
        let output = image::open(&out_png)?.to_rgb8();
        let db = sr::consistency(&input, &output, factor as u32);
        println!(
            "[upscale] consistency: {db:.1} dB {}",
            if db >= 30.0 {
                "(ok — shrunk back, it is the input)"
            } else if db >= 25.0 {
                "(low — a heavily stylising model can land here; look at it)"
            } else {
                "(FAIL — this is not the input enlarged; the network is mis-wired or the file is wrong)"
            }
        );
    }
    Ok(())
}

/// `yasdig sr-selftest`: every `sr` shader op against a CPU transcription of
/// the PyTorch op, plus a matmul throughput. No model file needed.
fn run_sr_selftest() -> Result<()> {
    let fsr = fsr::Fsr::new()?;
    println!("[sr-selftest] adapter: {}", fsr.adapter_name());
    println!(
        "[sr-selftest] storage binding limit: {} MiB",
        fsr.device().limits().max_storage_buffer_binding_size >> 20
    );
    sr::selftest(fsr.device(), fsr.queue())?;
    println!("[sr-selftest] all ops agree with the reference");
    Ok(())
}

/// `yasdig fsr <in.png> <out.png> [upscale_factor] [sharpness] [clarity]`
/// Runs only the GPU post-process passes on an existing image.
fn run_fsr_cli(args: &[String]) -> Result<()> {
    if args.len() < 2 {
        eprintln!(
            "usage: yasdig fsr <in.png> <out.png> [upscale_factor=2.0] \
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
    // Two positional filters and no more: the nine grading knobs are a UI
    // thing, and a command line with ten trailing floats on it is unusable.
    let (w, h) = fsr.process(&in_png, &out_png, factor, &Filters::starting_at(sharp, clarity))?;
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

/// `yasdig fsr-selftest [out.png]`
/// Exercises the full wgpu path (device init, every compute pass, readback)
/// without sd.cpp or a checkpoint, and checks what the shaders promise. This is
/// what to run after touching a filter constant.
///
/// 1. **Dimensions** — a 2× pass on a 128² pattern comes back 256².
/// 2. **Identity** — at scale 1.0 with every filter at 0 the output is
///    byte-identical to the input. `sharpen.wgsl`, `contrast.wgsl` and
///    `grade.wgsl` all claim this in their headers and `docs/api.md` promises
///    it to users; until now nothing checked it.
/// 3. **The grade** — that each of the nine knobs is wired at all, and that
///    the four with an unambiguous direction move that way. See [`check_grade`].
/// 4. **Band response** — printed, then asserted. See [`report_bands`].
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
    let (w, h) = fsr.process(&in_png, &out_png, 2.0, &Filters::starting_at(0.4, 0.5))?;
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
    fsr.process(&in_png, &ident_png, 1.0, &Filters::NONE)?;
    check_identity(&img, &ident_png)?;
    println!("[fsr-selftest] identity OK: scale 1.0, every filter at 0 is byte-exact");

    // 3) The grade.
    check_grade(&fsr, &img, &in_png, &out_png)?;

    // 4) Bands.
    report_bands(&fsr, &out_png)?;
    Ok(())
}

/// Check the grade pass: that every knob is *connected*, and that the ones with
/// an unarguable direction move that way.
///
/// The realistic regression here is not bad maths, it is a knob wired to the
/// wrong field or left out of `GradeParams` — eight floats going into one
/// uniform, where a copy-paste is invisible and the symptom is one slider that
/// does nothing or does its neighbour's job. So every knob is run at +1 and has
/// to change *something*, and the four whose direction is not a matter of taste
/// are measured: lightness up, contrast spreading, saturation at -1 landing
/// exactly on grey, and temperature moving blue towards yellow.
///
/// Hue and tint are only checked for being connected. A hue rotation of 180°
/// pushes colours out of gamut, where the clamp makes it non-invertible, so
/// "rotate twice and compare" would assert the clamp rather than the rotation —
/// the honest check for those two is a pair of eyes on the slider.
fn check_grade(
    fsr: &fsr::Fsr,
    src: &image::RgbaImage,
    in_png: &std::path::Path,
    near: &std::path::Path,
) -> Result<()> {
    let out_png = near.with_file_name("fsr-selftest-grade.png");
    let graded = |f: Filters| -> Result<image::RgbaImage> {
        fsr.process(in_png, &out_png, 1.0, &f)?;
        Ok(image::open(&out_png)?.to_rgba8())
    };
    // Rec. 601 luma on the stored (sRGB-encoded) values. Not a colour science
    // claim — just a stable scalar to compare two images by.
    let luma = |p: &image::Rgba<u8>| {
        0.299 * p.0[0] as f32 + 0.587 * p.0[1] as f32 + 0.114 * p.0[2] as f32
    };
    let mean = |img: &image::RgbaImage, f: &dyn Fn(&image::Rgba<u8>) -> f32| {
        img.pixels().map(f).sum::<f32>() / img.pixels().len() as f32
    };
    let spread = |img: &image::RgbaImage| {
        let m = mean(img, &luma);
        (img.pixels().map(|p| (luma(p) - m).powi(2)).sum::<f32>() / img.pixels().len() as f32)
            .sqrt()
    };
    let blue_minus_red = |p: &image::Rgba<u8>| p.0[2] as f32 - p.0[0] as f32;

    // Every knob at +1, and the count of pixels it moved. Zero is the failure.
    type Knob = (&'static str, fn(&mut Filters));
    let knobs: [Knob; 9] = [
        ("lightness", |f| f.lightness = 1.0),
        ("contrast", |f| f.contrast = 1.0),
        ("highlights", |f| f.highlights = 1.0),
        ("shadows", |f| f.shadows = 1.0),
        ("saturation", |f| f.saturation = 1.0),
        ("vibrance", |f| f.vibrance = 1.0),
        ("hue", |f| f.hue = 1.0),
        ("temperature", |f| f.temperature = 1.0),
        ("tint", |f| f.tint = 1.0),
    ];
    let mut moved = Vec::new();
    for (name, set) in knobs {
        let mut f = Filters::NONE;
        set(&mut f);
        let got = graded(f)?;
        let n = src
            .pixels()
            .zip(got.pixels())
            .filter(|(a, b)| a.0[..3] != b.0[..3])
            .count();
        if n == 0 {
            anyhow::bail!(
                "grade: '{name}' at 1.0 changed nothing. The pass ran (the other knobs \
                 move it), so the value is not reaching the shader — check that \
                 GradeParams and `Params` in grade.wgsl still list the same fields in \
                 the same order."
            );
        }
        moved.push((name, n));
    }
    let total = src.pixels().len();
    let cells: String = moved
        .iter()
        .map(|(n, c)| format!("{n} {:.0}%", 100.0 * *c as f32 / total as f32))
        .collect::<Vec<_>>()
        .join(", ");
    println!("[fsr-selftest] grade: pixels moved at +1.0 — {cells}");

    // Direction, for the four that have one.
    let base_luma = mean(src, &luma);
    let up = mean(&graded(Filters { lightness: 1.0, ..Filters::NONE })?, &luma);
    let down = mean(&graded(Filters { lightness: -1.0, ..Filters::NONE })?, &luma);
    if !(down < base_luma && base_luma < up) {
        anyhow::bail!("grade: lightness runs backwards — {down:.1} / {base_luma:.1} / {up:.1}");
    }
    let harder = spread(&graded(Filters { contrast: 1.0, ..Filters::NONE })?);
    let softer = spread(&graded(Filters { contrast: -1.0, ..Filters::NONE })?);
    if !(softer < spread(src) && spread(src) < harder) {
        anyhow::bail!(
            "grade: contrast runs backwards — luma spread {softer:.1} / {:.1} / {harder:.1}",
            spread(src)
        );
    }
    let grey = graded(Filters { saturation: -1.0, ..Filters::NONE })?;
    let worst = grey
        .pixels()
        .map(|p| {
            let (r, g, b) = (p.0[0] as i32, p.0[1] as i32, p.0[2] as i32);
            (r - g).abs().max((g - b).abs())
        })
        .max()
        .unwrap_or(0);
    if worst > 1 {
        anyhow::bail!(
            "grade: saturation -1.0 left {worst}/255 of colour in the worst pixel. It \
             should land exactly on a = b = 0, which is exactly neutral."
        );
    }
    let warm = mean(&graded(Filters { temperature: 1.0, ..Filters::NONE })?, &blue_minus_red);
    let cool = mean(&graded(Filters { temperature: -1.0, ..Filters::NONE })?, &blue_minus_red);
    if !(warm < mean(src, &blue_minus_red) && mean(src, &blue_minus_red) < cool) {
        anyhow::bail!(
            "grade: temperature runs backwards — blue-red {warm:.1} warm / {:.1} neutral / \
             {cool:.1} cool. Positive is meant to be warmer.",
            mean(src, &blue_minus_red)
        );
    }
    println!(
        "[fsr-selftest] grade OK: lightness {base_luma:.1} -> {up:.1}/{down:.1}, contrast \
         spread {:.1} -> {harder:.1}/{softer:.1}, saturation -1 is neutral to {worst}/255, \
         temperature blue-red {:.1} -> {warm:.1}/{cool:.1}",
        spread(src),
        mean(src, &blue_minus_red)
    );
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
             sharpen.wgsl and contrast.wgsl, and the promise in docs/api.md",
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
            fsr.process(&in_png, &out_png, 1.0, &Filters::starting_at(sharpness, clarity))?;
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
    use super::{clip_frame, history_path, workitems, EscalateDto, ExtendDto, HiresDto, RedrawDto};

    /// A failed job's body carries `oom` exactly when the failure was the typed
    /// out-of-memory, wrapped in context or not — and nothing for any other.
    #[test]
    fn an_error_body_says_oom_only_for_running_out_of_memory() {
        let oom = crate::sd::OutOfMemory {
            kind: "gpu",
            phase: "drawing",
            step: 7,
            steps: 20,
            placement: None,
            mode: "auto",
            ram_needed: Some(17_600_000_000),
            ram_free: None,
            reserve_gb: 0.0,
            lost: false,
            need_device_mb: None,
            avail_device_mb: None,
            need_budget_mb: None,
            avail_budget_mb: None,
            references: 0,
            gtt_total: None,
            gtt_free: None,
            encoder_on_card_mb: None,
            label: "generate".into(),
            log: String::new(),
        };
        let e = anyhow::Error::new(oom).context("refine");
        let v: serde_json::Value = serde_json::from_str(&super::error_body(&e)).unwrap();
        assert_eq!(v["oom"]["phase"], "drawing");
        assert_eq!(v["oom"]["step"], 7);
        assert!(v["error"].as_str().unwrap().contains("ran out of GPU memory while drawing (step 7 of 20)"));
        let plain: serde_json::Value =
            serde_json::from_str(&super::error_body(&anyhow::anyhow!("no such model"))).unwrap();
        assert!(plain.get("oom").is_none());
        assert_eq!(plain["error"], "no such model");
    }

    /// The history thumbnail's guard: two whole numbers or nothing.
    #[test]
    fn a_brush_mask_path_is_an_id_and_a_tool() {
        assert_eq!(super::brush_path("/brush/3/dodge.png"), Some((3, crate::brush::Tool::Dodge)));
        assert_eq!(super::brush_path("/brush/3/restore.png"), Some((3, crate::brush::Tool::Restore)));
        for bad in ["/brush/3/smudge.png", "/brush/a/blur.png", "/brush//blur.png", "/brush/3/../blur.png", "/brush/3.png"] {
            assert_eq!(super::brush_path(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_history_path_is_two_numbers() {
        assert_eq!(history_path("/history/3/0.png"), Some((3, 0)));
        assert_eq!(history_path("/history/12/11.png"), Some((12, 11)));
        for bad in ["/history/3.png", "/history/a/0.png", "/history/3/-1.png", "/history//0.png", "/history/3/0/1.png", "/history/3/../0.png"] {
            assert_eq!(history_path(bad), None, "{bad}");
        }
    }

    /// The mask arrives as a canvas's `data:` URL or as bare base64; both are
    /// the PNG's bytes. `prompt` and `strength` are optional.
    #[test]
    fn a_redraw_body_carries_its_mask_either_way() {
        let png = b"\x89PNG\r\n";
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(png);
        let body = format!("{{\"image_id\":3,\"mask\":\"data:image/png;base64,{b64}\"}}");
        let dto: RedrawDto = serde_json::from_str(&body).unwrap();
        assert_eq!(dto.mask_png().unwrap(), png);
        assert_eq!((dto.prompt, dto.strength), (None, None));

        let body = format!("{{\"image_id\":3,\"mask\":\"{b64}\",\"prompt\":\"a hat\",\"strength\":0.5}}");
        let dto: RedrawDto = serde_json::from_str(&body).unwrap();
        assert_eq!(dto.mask_png().unwrap(), png);
        assert_eq!(dto.prompt.as_deref(), Some("a hat"));

        let bad: RedrawDto = serde_json::from_str("{\"image_id\":3,\"mask\":\"!!\"}").unwrap();
        assert!(bad.mask_png().is_err());
    }

    /// An Extend names its sides under `expand`, each optional; a side left
    /// out doesn't grow, and the rest of the body is the area pass's.
    #[test]
    fn an_extend_body_names_its_sides() {
        let dto: ExtendDto =
            serde_json::from_str(r#"{"image_id":3,"expand":{"left":256,"right":256},"feather":40}"#).unwrap();
        let req = dto.request();
        assert_eq!(req.expand, crate::pipeline::Expand { left: 256, top: 0, right: 256, bottom: 0 });
        assert_eq!((req.prompt, req.feather, req.memory), (None, Some(40), None));
        let bare: ExtendDto = serde_json::from_str(r#"{"image_id":3}"#).unwrap();
        assert!(bare.request().expand.is_empty());
    }

    /// The page tells "no preview yet" from "preview 0" by `null`, so the
    /// field has to be there on every busy body and be a real JSON value.
    #[test]
    fn progress_carries_the_preview_number_or_null() {
        let p = crate::sd::Progress {
            label: "txt2img".into(),
            phase: crate::sd::Phase::Sampling,
            step: 3,
            steps: 20,
            rate: "1.23s/it".into(),
        };
        let with: serde_json::Value = serde_json::from_str(&super::progress_body(Some(&p), Some(7))).unwrap();
        assert_eq!(with["preview"], 7);
        let without: serde_json::Value = serde_json::from_str(&super::progress_body(Some(&p), None)).unwrap();
        assert!(without["preview"].is_null());
        assert_eq!(super::progress_body(None, Some(7)), "{\"busy\":false}");
    }

    /// The frame route's guard. The case worth pinning is the rejection: a
    /// segment that isn't a number must not come back as "no frame", because
    /// that is how `/frames/nonsense.png` would end up answering 200 with the
    /// clip's poster frame.
    #[test]
    fn a_frame_path_needs_a_number_or_nothing() {
        assert_eq!(clip_frame("/videos/100-7/frames/0.png"), Some(("100-7", 0)));
        assert_eq!(clip_frame("/videos/100-7/frames/32.png"), Some(("100-7", 32)));

        for bad in [
            "/videos/100-7/frames/nonsense.png",
            "/videos/100-7/frames/.png",
            "/videos/100-7/frames/-1.png",
            "/videos/100-7/frames/1.5.png",
            "/videos/100-7/frames/0.png.png",
            "/videos/100-7/frames/0",
            "/videos/100-7/frames/../../../etc/passwd.png",
            "/videos/100-7/thumb.png",
        ] {
            assert_eq!(clip_frame(bad), None, "{bad} must not resolve to a frame");
        }

        // A traversal attempt in the *id* is not this function's job — it parses
        // the shape and `handle_clip_file` applies `valid_id`. Stated as a test
        // so the division of labour is on the record rather than assumed.
        assert_eq!(
            clip_frame("/videos/../../secrets/frames/0.png"),
            Some(("../../secrets", 0))
        );
        assert!(!workitems::valid_id("../../secrets"));
    }

    /// An escalation body has three states per knob, and two of them look the
    /// same in JSON unless the deserializer is told otherwise: absent means
    /// "keep what the image was made with", `null` on a pass's CFG means
    /// "unset it so the pass follows cfg_scale again", and a number pins it.
    /// Serde folds `null` into `None` by default, which would make an emptied
    /// box unclearable — and `/hires` routes the whole struct through
    /// `#[serde(flatten)]`'s buffered content deserializer, which is where a
    /// regression here would hide. No GPU needed to catch either.
    /// A pass's own prompt: absent and `null` both leave the image's own, and
    /// the hi-res one rides through the flatten like every other knob.
    #[test]
    fn an_escalation_carries_its_own_prompt() {
        let bare: EscalateDto = serde_json::from_str(r#"{"image_id":7}"#).unwrap();
        assert_eq!(bare.overrides().refine_prompt, None);
        let null: EscalateDto =
            serde_json::from_str(r#"{"image_id":7,"refine_prompt":null}"#).unwrap();
        assert_eq!(null.overrides().refine_prompt, None);
        let set: EscalateDto =
            serde_json::from_str(r#"{"image_id":7,"refine_prompt":"sharper fabric"}"#).unwrap();
        assert_eq!(set.overrides().refine_prompt.as_deref(), Some("sharper fabric"));
        let h: HiresDto =
            serde_json::from_str(r#"{"image_id":7,"upscale":2.0,"hires_prompt":"crisp"}"#).unwrap();
        assert_eq!(h.esc.overrides().hires_prompt.as_deref(), Some("crisp"));
    }

    #[test]
    fn escalate_body_tells_an_empty_cfg_box_from_a_missing_one() {
        let bare: EscalateDto = serde_json::from_str(r#"{"image_id":7}"#).unwrap();
        assert_eq!(bare.image_id, 7);
        assert_eq!(bare.overrides().refine_cfg_scale, None);
        assert_eq!(bare.overrides().refine_strength, None);
        // References only when the Refine panel's tick sends them.
        assert!(bare.overrides().references.is_empty());
        let r: EscalateDto = serde_json::from_str(r#"{"image_id":7,"references":["a-1"]}"#).unwrap();
        assert_eq!(r.overrides().references, ["a-1"]);
        let d: RedrawDto = serde_json::from_str(r#"{"image_id":7,"mask":""}"#).unwrap();
        assert!(d.references.is_empty());

        let cleared: EscalateDto =
            serde_json::from_str(r#"{"image_id":7,"refine_cfg_scale":null}"#).unwrap();
        assert_eq!(cleared.overrides().refine_cfg_scale, Some(None));

        let set: EscalateDto = serde_json::from_str(
            r#"{"image_id":7,"refine_cfg_scale":8.5,"refine_steps":12,"refine_strength":0.35}"#,
        )
        .unwrap();
        let o = set.overrides();
        assert_eq!(o.refine_cfg_scale, Some(Some(8.5)));
        assert_eq!(o.refine_steps, Some(12));
        assert_eq!(o.refine_strength, Some(0.35));

        // The same three states, seen through the flatten.
        let h: HiresDto = serde_json::from_str(
            r#"{"image_id":7,"upscale":2.0,"hires_cfg_scale":null,"hires_strength":0.5}"#,
        )
        .unwrap();
        assert_eq!(h.upscale, Some(2.0));
        assert_eq!(h.esc.overrides().hires_cfg_scale, Some(None));
        assert_eq!(h.esc.overrides().hires_strength, Some(0.5));

        let h2: HiresDto = serde_json::from_str(r#"{"image_id":7,"upscale":2.0}"#).unwrap();
        assert_eq!(h2.esc.overrides().hires_cfg_scale, None);

        // The two per-pass boxes are the same three states again, and each is a
        // separate `deserialize_with` that a copy-paste can leave pointing at the
        // wrong field — which compiles, and reads as the box doing nothing.
        let pair: EscalateDto = serde_json::from_str(
            r#"{"image_id":7,"refine_cfg_scale":3.0,"hires_cfg_scale":null}"#,
        )
        .unwrap();
        assert_eq!(pair.overrides().refine_cfg_scale, Some(Some(3.0)));
        assert_eq!(pair.overrides().hires_cfg_scale, Some(None));
        assert_eq!(bare.overrides().refine_cfg_scale, None);
        assert_eq!(bare.overrides().hires_cfg_scale, None);

        let hp: HiresDto = serde_json::from_str(
            r#"{"image_id":7,"upscale":2.0,"hires_cfg_scale":2.5}"#,
        )
        .unwrap();
        assert_eq!(hp.esc.overrides().hires_cfg_scale, Some(Some(2.5)));
        assert_eq!(hp.esc.overrides().refine_cfg_scale, None);
    }

    /// The per-pass model swap has the same three states as the CFG box above,
    /// and the middle one is the one a `<select>` actually sends: an unchosen
    /// option is the empty string, not an absent field. `Engine::swap_model`
    /// treats that as "keep the model the image was made with", so the two have
    /// to survive the trip as themselves rather than being folded together here.
    ///
    /// Through `HiresDto`'s flatten as well, for the reason the test above
    /// states: that is where a serde regression hides.
    #[test]
    fn an_escalation_may_name_a_different_checkpoint() {
        let bare: EscalateDto = serde_json::from_str(r#"{"image_id":7}"#).unwrap();
        assert_eq!(bare.overrides().template, None);

        let empty: EscalateDto = serde_json::from_str(r#"{"image_id":7,"model":""}"#).unwrap();
        assert_eq!(empty.overrides().template.as_deref(), Some(""));

        let named: EscalateDto =
            serde_json::from_str(r#"{"image_id":7,"model":"sd_xl_base_1.0"}"#).unwrap();
        assert_eq!(named.overrides().template.as_deref(), Some("sd_xl_base_1.0"));

        let h: HiresDto =
            serde_json::from_str(r#"{"image_id":7,"upscale":2.0,"model":"hebe_v10"}"#).unwrap();
        assert_eq!(h.esc.overrides().template.as_deref(), Some("hebe_v10"));
        assert_eq!(h.upscale, Some(2.0));

        // `template` is *not* the name on the wire, and this is why: the page
        // spreads the escalation knobs into the `/generate` body, where
        // `template` is the model doing the txt2img. A field by that name here
        // would be read by neither struct as the other one expects.
        let wrong: EscalateDto =
            serde_json::from_str(r#"{"image_id":7,"template":"sd_xl_base_1.0"}"#).unwrap();
        assert_eq!(wrong.overrides().template, None);
    }

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

    /// `/models` is hand-rolled `format!`, so a field added out of step with
    /// its argument — which this change nearly shipped — is valid Rust and
    /// broken JSON, or worse, valid JSON with one value under another's name.
    /// Parsed here, with a model of each shape.
    #[test]
    fn the_models_listing_is_json_with_every_field_where_it_belongs() {
        let root = crate::config::fresh_test_root("yasdig-models-json");
        let models = root.join("models");
        std::fs::create_dir_all(&models).unwrap();
        let write = |name: &str, tensors: &str| {
            let json = format!("{{{tensors}}}");
            let mut buf = (json.len() as u64).to_le_bytes().to_vec();
            buf.extend_from_slice(json.as_bytes());
            std::fs::write(models.join(name), buf).unwrap();
        };
        write(
            "xl.safetensors",
            r#""input_blocks.4.1.transformer_blocks.0.attn2.to_k.weight":{"dtype":"F16","shape":[640,2048],"data_offsets":[0,0]}"#,
        );
        write(
            "z.safetensors",
            r#""cap_embedder.0.weight":{"dtype":"F16","shape":[3840,2560],"data_offsets":[0,0]}"#,
        );
        std::fs::write(root.join("models.toml"), "[sd]\nbinary = \"sd-cli\"\n").unwrap();
        let cfg = crate::config::Config::load(&root.join("models.toml")).unwrap();

        let list: serde_json::Value = serde_json::from_str(&super::build_models_json(&cfg)).unwrap();
        let by = |n: &str| list.as_array().unwrap().iter().find(|m| m["name"] == n).unwrap().clone();
        let (xl, z) = (by("xl"), by("z"));
        assert_eq!((xl["family"].as_str(), xl["exists"].as_bool()), (Some("sdxl"), Some(true)));
        assert_eq!(xl["missing"], serde_json::json!([]));
        assert_eq!(xl["tuned"], serde_json::json!(false));
        assert_eq!((z["family_label"].as_str(), z["steps"].as_u64()), (Some("Z-Image"), Some(8)));
        // No VAE and no encoder: both named, and the model not runnable.
        assert_eq!(z["exists"], serde_json::json!(false));
        assert_eq!(z["missing"].as_array().unwrap().len(), 2);

        // Two VAEs of different widths: each slot offers only the one that fits.
        std::fs::create_dir_all(models.join("vae")).unwrap();
        let vae = |name: &str, z2: u32| {
            write(
                &format!("vae/{name}"),
                &format!(
                    r#""decoder.conv_in.weight":{{"dtype":"F16","shape":[512,4,3,3],"data_offsets":[0,0]}},
                       "decoder.mid.block_1.conv1.weight":{{"dtype":"F16","shape":[512,512,3,3],"data_offsets":[0,0]}},
                       "encoder.conv_out.bias":{{"dtype":"F16","shape":[{z2}],"data_offsets":[0,0]}}"#
                ),
            )
        };
        vae("sdxl_vae.safetensors", 8);
        vae("ae.safetensors", 32);
        let list: serde_json::Value = serde_json::from_str(&super::build_models_json(&cfg)).unwrap();
        let by = |n: &str| list.as_array().unwrap().iter().find(|m| m["name"] == n).unwrap().clone();
        let (xl, z) = (by("xl"), by("z"));
        let slot = |m: &serde_json::Value, i: usize| m["parts"][i].clone();
        assert_eq!(slot(&xl, 0)["slot"], "vae");
        assert_eq!(slot(&xl, 0)["optional"], true);
        assert_eq!(slot(&xl, 0)["current"], serde_json::Value::Null);
        assert_eq!(slot(&xl, 0)["options"][0]["name"], "vae/sdxl_vae.safetensors");
        assert_eq!(slot(&xl, 0)["options"].as_array().unwrap().len(), 1);
        assert_eq!(slot(&z, 0)["optional"], false);
        assert_eq!(slot(&z, 0)["current"], "vae/ae.safetensors");
        assert_eq!(slot(&z, 1)["slot"], "llm");

        // And a request naming the wrong one is refused before anything runs.
        let mut p = cfg.registry().remove("xl").unwrap();
        let pick = |n: &str| [("vae".to_string(), Some(n.to_string()))].into_iter().collect();
        assert!(cfg.apply_parts(&mut p, &pick("vae/ae.safetensors")).is_err());
        assert!(cfg.apply_parts(&mut p, &pick("../models.toml")).is_err());
        cfg.apply_parts(&mut p, &pick("vae/sdxl_vae.safetensors")).unwrap();
        assert_eq!(p.components.unwrap().vae, Some(models.join("vae/sdxl_vae.safetensors")));
        // A Z-Image has no VAE of its own, so emptying the slot is refused.
        let mut zp = cfg.registry().remove("z").unwrap();
        let clear = [("vae".to_string(), None)].into_iter().collect();
        assert!(cfg.apply_parts(&mut zp, &clear).is_err());

        // Saving: the scan's own pick is not a choice worth storing, a real
        // one is, and `null` forgets it.
        let body = |j: &str| serde_json::from_str::<serde_json::Map<_, _>>(j).unwrap();
        let none = Default::default();
        assert!(cfg.update_parts("z", &none, &body(r#"{"vae":"vae/ae.safetensors"}"#)).unwrap().is_empty());
        let kept = cfg.update_parts("xl", &none, &body(r#"{"vae":"vae/sdxl_vae.safetensors"}"#)).unwrap();
        assert_eq!(kept.get("vae").map(String::as_str), Some("vae/sdxl_vae.safetensors"));
        assert!(cfg.update_parts("xl", &kept, &body(r#"{"vae":null}"#)).unwrap().is_empty());
        assert!(cfg.update_parts("xl", &none, &body(r#"{"llm":"vae/ae.safetensors"}"#)).is_err());

        // A saved part reaches the registry, and once its file is gone the
        // model waits for it rather than quietly running on something else.
        let mut tuned = crate::settings::ModelSettings::default();
        tuned.parts.insert("xl".into(), kept);
        tuned.save(&cfg.user_dir()).unwrap();
        let sdxl_vae = models.join("vae/sdxl_vae.safetensors");
        let p = cfg.registry().remove("xl").unwrap();
        assert_eq!(p.components.and_then(|c| c.vae), Some(sdxl_vae.clone()));
        std::fs::remove_file(&sdxl_vae).unwrap();
        let p = cfg.registry().remove("xl").unwrap();
        assert_eq!(p.missing(), vec![sdxl_vae]);

        std::fs::remove_dir_all(&root).ok();
    }
}
