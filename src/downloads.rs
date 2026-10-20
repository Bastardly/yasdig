//! The Get models tab's downloads: `curl` as a subprocess, one file at a time,
//! on a thread of its own.
//!
//! **Why curl and not an HTTP client in here.** A Rust client means a TLS stack
//! compiled into the one-file build, for a feature that runs a few times per
//! install. curl is on every Linux box and ships with Windows 10 and later, and
//! it already does what a 10 GB download needs: Hugging Face's redirect to its
//! CDN (`-L`), retries, the system's proxy settings and resume (`-C -`). It is
//! the decision the project made for sd.cpp, made again: a subprocess, never a
//! linked library.
//!
//! **Its own lane, not the job queue.** A download is minutes of network and no
//! GPU, so it must neither wait behind a render nor hold one up. One worker
//! thread and a FIFO, so two entries sharing a VAE fetch it once and four
//! downloads don't split one connection four ways.
//!
//! Three things break silently:
//!
//! - **A file is written as `<dest>.part` and renamed when its length is
//!   right.** Discovery only reads `.safetensors`/`.gguf`/…, so a half file is
//!   never listed as a model — a truncated header would otherwise read as
//!   `Unknown`, or worse, as a model that fails minutes into a run.
//! - **Progress is the `.part` file's length, read on the connection thread.**
//!   Nothing parses curl's meter, so its format can change under us freely,
//!   and a resumed download shows its true position from the first poll.
//! - **Cancel keeps the `.part`**, so Download resumes from it. Only *Remove
//!   partial* deletes it. A `.part` already as long as the catalog says (killed
//!   between the last byte and the rename) is finished without asking the
//!   server, which would answer a range past the end with a 416.

use crate::catalog::{Catalog, Entry, File, Lora, Upscaler};
use crate::config::{slash_name, Config};
use crate::discover::{self, Found};
use anyhow::{bail, Result};
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

/// One file to fetch. Keyed by its full path everywhere, not by entry: Z-Image
/// and both Flux.1s share one `ae.safetensors`, and a download started from one
/// card is the same download on the others. The full path rather than the
/// catalog's `dest`, because an upscaler's `dest` is relative to the upscaler
/// dir and a model's to the models dir.
#[derive(Clone)]
struct Task {
    key: String,
    dest: PathBuf,
    url: String,
    bytes: u64,
    want: Want,
}

/// What a finished file must turn out to be.
#[derive(Clone)]
enum Want {
    /// The entry it was asked for under, and the file's index in it: checked
    /// against the slot it was meant to fill.
    Part { entry: String, index: usize },
    /// A catalog upscaler: checked against its `arch`.
    Upscaler { id: String },
    /// A speed LoRA a card fetches: checked to be a LoRA for its family.
    Lora { id: String },
}

fn key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[derive(Default)]
struct State {
    queue: VecDeque<Task>,
    running: Option<String>,
    child: Option<Child>,
    cancelled: bool,
    /// Why the last attempt at a dest failed, until it is retried or cleared.
    failed: HashMap<String, String>,
    /// Bumped whenever a file lands, so the page knows to reload its models.
    finished: u64,
}

#[derive(Clone)]
pub struct Downloads(Arc<Inner>);

struct Inner {
    models: PathBuf,
    /// Where a `[[video]]` entry's files go — `Config::video_dir`.
    videos: PathBuf,
    upscalers: PathBuf,
    /// Where a [`Lora`]'s `dest` is — `Config::lora_dir`.
    loras: PathBuf,
    state: Mutex<State>,
    wake: Condvar,
}

impl Downloads {
    pub fn start(cfg: &Config) -> Result<Downloads> {
        let d = Downloads(Arc::new(Inner {
            models: cfg.models_dir(),
            videos: cfg.video_dir(),
            upscalers: cfg.upscaler_dir(),
            loras: cfg.lora_dir(),
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
        }));
        let worker = d.clone();
        std::thread::Builder::new()
            .name("downloads".into())
            .spawn(move || worker.work())?;
        Ok(d)
    }

    /// The dir an entry's `dest`s are relative to.
    fn root(&self, e: &Entry) -> &Path {
        if e.family.is_video() { &self.0.videos } else { &self.0.models }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Every file of catalog item `id` — a family entry or an upscaler — as
    /// the task that would fetch it. The id is the whole request: what can be
    /// asked for is what the catalog lists.
    fn tasks(&self, id: &str) -> Result<Vec<Task>> {
        let c = Catalog::get();
        if let Some(e) = c.entry(id) {
            let parts = e.file.iter().enumerate().map(|(index, f)| {
                let dest = self.root(e).join(&f.dest);
                Task {
                    key: key(&dest),
                    dest,
                    url: f.url.clone(),
                    bytes: f.bytes,
                    want: Want::Part { entry: e.id.clone(), index },
                }
            });
            let loras = c.loras_of(e).map(|l| {
                let dest = self.0.loras.join(&l.dest);
                Task {
                    key: key(&dest),
                    dest,
                    url: l.url.clone(),
                    bytes: l.bytes,
                    want: Want::Lora { id: l.id.clone() },
                }
            });
            return Ok(parts.chain(loras).collect());
        }
        if let Some(u) = c.upscaler(id) {
            let dest = self.0.upscalers.join(&u.dest);
            return Ok(vec![Task {
                key: key(&dest),
                dest,
                url: u.url.clone(),
                bytes: u.bytes,
                want: Want::Upscaler { id: u.id.clone() },
            }]);
        }
        bail!("no catalog entry named {id:?}")
    }

    /// Queue every file of `id` that nothing on disk fills yet. Answers how many
    /// were queued; 0 is not an error (the entry is already complete).
    pub fn enqueue(&self, cfg: &Config, id: &str) -> Result<usize> {
        let tasks = self.tasks(id)?;
        if !curl_available() {
            bail!("curl was not found on the PATH; install it, or download the files by hand (docs/configuration.md, Get a model)");
        }
        let c = Catalog::get();
        let found = match c.entry(id) {
            Some(e) => scan_for(cfg, e),
            None => Vec::new(),
        };
        let mut st = self.lock();
        let mut n = 0;
        for t in tasks {
            let busy = st.running.as_deref() == Some(&t.key) || st.queue.iter().any(|q| q.key == t.key);
            let had = match &t.want {
                Want::Part { entry, index } => {
                    let e = c.entry(entry).expect("tasks come from the catalog");
                    present(self.root(e), e, &e.file[*index], &found).is_some()
                }
                Want::Upscaler { id } => {
                    upscaler_is(&t.dest, c.upscaler(id).expect("tasks come from the catalog"))
                }
                Want::Lora { id } => lora_is(&t.dest, c.lora(id).expect("tasks come from the catalog")),
            };
            if busy || had {
                continue;
            }
            // A dest that exists but isn't what the slot takes is never
            // overwritten: it is somebody's file, and the card says what it is.
            if t.dest.exists() {
                continue;
            }
            st.failed.remove(&t.key);
            st.queue.push_back(t);
            n += 1;
        }
        drop(st);
        self.0.wake.notify_all();
        Ok(n)
    }

    /// Stop every file of `id`: queued ones are dropped, a running one is
    /// killed. Its `.part` stays, so Download resumes.
    pub fn cancel(&self, id: &str) -> Result<()> {
        let keys: Vec<String> = self.tasks(id)?.into_iter().map(|t| t.key).collect();
        let mut st = self.lock();
        let mine = |k: &str| keys.iter().any(|x| x == k);
        st.queue.retain(|t| !mine(&t.key));
        if st.running.as_deref().is_some_and(mine) {
            st.cancelled = true;
            if let Some(c) = st.child.as_mut() {
                let _ = c.kill();
            }
        }
        Ok(())
    }

    /// Everything, for *Close server*: an orphaned curl would go on writing
    /// gigabytes into a folder nobody is watching.
    pub fn cancel_all(&self) {
        let mut st = self.lock();
        st.queue.clear();
        st.cancelled = true;
        if let Some(c) = st.child.as_mut() {
            let _ = c.kill();
        }
    }

    /// Delete the `.part` files of `id` that nothing is writing.
    pub fn clear(&self, id: &str) -> Result<()> {
        let tasks = self.tasks(id)?;
        let mut st = self.lock();
        for t in tasks {
            let busy = st.running.as_deref() == Some(&t.key) || st.queue.iter().any(|q| q.key == t.key);
            if !busy {
                let _ = std::fs::remove_file(part_of(&t.dest));
                st.failed.remove(&t.key);
            }
        }
        Ok(())
    }

    fn work(&self) {
        loop {
            let task = {
                let mut st = self.lock();
                loop {
                    if let Some(t) = st.queue.pop_front() {
                        st.running = Some(t.key.clone());
                        st.cancelled = false;
                        break t;
                    }
                    st = self.0.wake.wait(st).unwrap_or_else(|p| p.into_inner());
                }
            };
            let result = self.fetch(&task);
            let mut st = self.lock();
            st.running = None;
            st.child = None;
            match result {
                Ok(()) => st.finished += 1,
                Err(_) if st.cancelled => {}
                Err(e) => {
                    eprintln!("[yasdig] download {} failed: {e:#}", task.dest.display());
                    st.failed.insert(task.key.clone(), format!("{e:#}"));
                }
            }
        }
    }

    fn fetch(&self, t: &Task) -> Result<()> {
        let dest = &t.dest;
        let part = part_of(dest);
        if let Some(dir) = dest.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let have = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        if have > t.bytes {
            // Longer than the file can be: not a prefix of it, so not resumable.
            std::fs::remove_file(&part)?;
        }
        if have != t.bytes {
            println!("[yasdig] downloading {} ({})", dest.display(), t.url);
            let mut child = curl_command(&t.url, &part).spawn()?;
            let mut stderr = child.stderr.take().expect("stderr was piped");
            {
                let mut st = self.lock();
                if st.cancelled {
                    let _ = child.kill();
                }
                st.child = Some(child);
            }
            // EOF on stderr is the child exiting, or being killed. The child is
            // taken back out of the lock before `wait`, so a cancel is never
            // blocked behind the wait it is meant to end (`sd::Monitor`'s rule).
            let mut err = String::new();
            let _ = stderr.read_to_string(&mut err);
            let child = self.lock().child.take();
            let status = match child {
                Some(mut c) => c.wait()?,
                None => bail!("curl went missing while it ran"),
            };
            if self.lock().cancelled {
                bail!("cancelled");
            }
            if !status.success() {
                let why = err.trim();
                bail!("curl failed ({status}){}{why}", if why.is_empty() { "" } else { ": " });
            }
        }
        let got = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        if got != t.bytes {
            bail!("got {got} bytes, expected {}; press Download to resume", t.bytes);
        }
        // Checked again here, not only when the download was queued: a file
        // copied in by hand during a multi-GB download is the user's, and a
        // rename would replace it without a word. The `.part` is kept.
        if dest.exists() {
            bail!(
                "{} appeared while this downloaded — it was left alone; the download is kept as {}",
                dest.display(),
                part.display()
            );
        }
        std::fs::rename(&part, dest)?;
        println!("[yasdig] downloaded {}", dest.display());
        // What it *is*, read off its header like every other file: a catalog
        // entry pointing at the wrong file is reported here, on the card, not
        // minutes into a render. The file is kept — it is a whole download, and
        // the card names what it turned out to be.
        match &t.want {
            Want::Part { entry, index } => {
                let entry = Catalog::get().entry(entry).expect("tasks come from the catalog");
                let f = &entry.file[*index];
                let role = discover::identify(dest);
                if !entry.fits(f, dest, &role) {
                    bail!(
                        "downloaded, but it reads as {} — not the {} {} takes",
                        role.describe(),
                        entry.slot_label(f),
                        entry.family.label()
                    );
                }
            }
            Want::Upscaler { id } => {
                let u = Catalog::get().upscaler(id).expect("tasks come from the catalog");
                let arch = upscaler_arch(dest);
                if arch != u.arch {
                    bail!("downloaded, but it reads as {arch}, not the {} the catalog lists", u.arch);
                }
            }
            Want::Lora { id } => {
                let l = Catalog::get().lora(id).expect("tasks come from the catalog");
                if !lora_is(dest, l) {
                    bail!(
                        "downloaded, but it reads as {} — not a LoRA for {}",
                        discover::identify(dest).describe(),
                        l.family.label()
                    );
                }
            }
        }
        Ok(())
    }

    /// The catalog with every file's state, for `GET /catalog`.
    pub fn status(&self, cfg: &Config) -> CatalogView {
        let c = Catalog::get();
        // The video cards only while the Video tab shows: hidden, not removed,
        // like the tab itself.
        let nightly = crate::settings::Settings::load_or_default(&cfg.user_dir()).nightly_video;
        let found = cfg.scan_images();
        let video_found = if nightly { crate::discover::scan(&cfg.video_dir()) } else { Vec::new() };
        let st = self.lock();
        let view = |e: &'static Entry, found: &[Found]| EntryView {
            id: &e.id,
            family: e.family.as_str(),
            family_label: e.family.label(),
            label: &e.label,
            recommended: e.recommended,
            note: &e.note,
            bytes: e.bytes() + c.loras_of(e).map(|l| l.bytes).sum::<u64>(),
            files: e
                .file
                .iter()
                .map(|f| file_state(self.root(e), e, f, found, &st))
                .chain(c.loras_of(e).map(|l| lora_state(&self.0.loras, l, &st)))
                .collect(),
        };
        let entries = c.entry.iter().map(|e| view(e, &found)).collect();
        let videos = if nightly { c.video.iter().map(|e| view(e, &video_found)).collect() } else { Vec::new() };
        let upscalers = Catalog::get()
            .upscaler
            .iter()
            .map(|u| EntryView {
                id: &u.id,
                family: "upscaler",
                family_label: "Upscaler",
                label: &u.label,
                recommended: false,
                note: &u.note,
                bytes: u.bytes,
                files: vec![upscaler_state(&self.0.upscalers, u, &st)],
            })
            .collect();
        CatalogView {
            curl: curl_available(),
            finished: st.finished,
            models_dir: self.0.models.display().to_string(),
            upscaler_dir: self.0.upscalers.display().to_string(),
            video_dir: self.0.videos.display().to_string(),
            entries,
            videos,
            upscalers,
        }
    }
}

#[derive(Serialize)]
pub struct CatalogView {
    curl: bool,
    finished: u64,
    models_dir: String,
    upscaler_dir: String,
    video_dir: String,
    entries: Vec<EntryView>,
    /// Video models, empty unless the Video tab is on (nightly). Their own
    /// list for the reason `upscalers` has one.
    videos: Vec<EntryView>,
    /// A list of their own, so the Help tab's family table — which reads
    /// `entries` — never meets one.
    upscalers: Vec<EntryView>,
}

#[derive(Serialize)]
struct EntryView {
    id: &'static str,
    family: &'static str,
    family_label: &'static str,
    label: &'static str,
    recommended: bool,
    note: &'static str,
    bytes: u64,
    files: Vec<FileView>,
}

#[derive(Serialize, Debug, PartialEq)]
struct FileView {
    slot: String,
    slot_label: String,
    dest: String,
    bytes: u64,
    /// The Hugging Face repository it comes from, which is where its licence is.
    page: String,
    /// `have`, `missing`, `queued`, `downloading`, `partial`, `failed` or
    /// `wrong` (the dest exists and is something else).
    state: &'static str,
    /// Bytes on disk so far, for the three states with a `.part`.
    got: u64,
    /// The file that fills it, for `have`, which may not be `dest`.
    name: Option<String>,
    why: Option<String>,
}

/// The file that already fills `f`, if any: its own `dest` first, then any
/// scanned file the slot's test accepts.
fn present(root: &Path, e: &Entry, f: &File, found: &[Found]) -> Option<PathBuf> {
    let dest = root.join(&f.dest);
    if dest.exists() {
        return e.fits(f, &dest, &discover::identify(&dest)).then_some(dest);
    }
    found.iter().find(|x| e.fits(f, &x.path, &x.role)).map(|x| x.path.clone())
}

/// The files that may already fill an entry's parts: the image scan for an
/// image entry, and for a video one the video dir alone — what discovery
/// assembles a video model from, so a part counted as had is one the model
/// will actually use.
fn scan_for(cfg: &Config, e: &Entry) -> Vec<Found> {
    if e.family.is_video() { discover::scan(&cfg.video_dir()) } else { cfg.scan_images() }
}

fn file_state(models: &Path, e: &'static Entry, f: &'static File, found: &[Found], st: &State) -> FileView {
    let k = key(&models.join(&f.dest));
    let part = part_of(&models.join(&f.dest));
    let got = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    let mut v = FileView {
        slot: f.slot.clone(),
        slot_label: e.slot_label(f).to_string(),
        dest: f.dest.clone(),
        bytes: f.bytes,
        page: f.page(),
        state: "missing",
        got: 0,
        name: None,
        why: None,
    };
    if st.running.as_deref() == Some(&k) {
        v.state = "downloading";
        v.got = got;
    } else if st.queue.iter().any(|t| t.key == k) {
        v.state = "queued";
        v.got = got;
    } else if let Some(p) = present(models, e, f, found) {
        v.state = "have";
        v.name = Some(slash_name(models, &p));
    } else if models.join(&f.dest).exists() {
        v.state = "wrong";
        v.why = Some(format!("{} is there, and reads as {}", f.dest, discover::identify(&models.join(&f.dest)).describe()));
    } else if let Some(why) = st.failed.get(&k) {
        v.state = "failed";
        v.got = got;
        v.why = Some(why.clone());
    } else if got > 0 {
        v.state = "partial";
        v.got = got;
    }
    v
}

/// A speed LoRA is had when its own `dest` is there, whole, and reads as a
/// LoRA its family takes — not when some other LoRA fits (see [`Lora`]).
fn lora_is(dest: &Path, l: &Lora) -> bool {
    std::fs::metadata(dest).is_ok_and(|m| m.is_file() && m.len() == l.bytes)
        && matches!(discover::identify(dest), discover::Role::Lora(b) if b.fits(l.family))
}

/// [`file_state`] for a card's speed LoRA, under the LoRA dir.
fn lora_state(dir: &Path, l: &'static Lora, st: &State) -> FileView {
    let dest = dir.join(&l.dest);
    let k = key(&dest);
    let got = std::fs::metadata(part_of(&dest)).map(|m| m.len()).unwrap_or(0);
    let mut v = FileView {
        slot: "lora".to_string(),
        slot_label: format!("Turbo LoRA ({} steps)", l.turbo.steps),
        dest: l.dest.clone(),
        bytes: l.bytes,
        page: l.page(),
        state: "missing",
        got: 0,
        name: None,
        why: None,
    };
    if st.running.as_deref() == Some(&k) {
        v.state = "downloading";
        v.got = got;
    } else if st.queue.iter().any(|t| t.key == k) {
        v.state = "queued";
        v.got = got;
    } else if lora_is(&dest, l) {
        v.state = "have";
        v.name = Some(l.dest.clone());
    } else if dest.exists() {
        v.state = "wrong";
        v.why = Some(format!("{} is there, and reads as {}", l.dest, discover::identify(&dest).describe()));
    } else if let Some(why) = st.failed.get(&k) {
        v.state = "failed";
        v.got = got;
        v.why = Some(why.clone());
    } else if got > 0 {
        v.state = "partial";
        v.got = got;
    }
    v
}

/// What `sr::identify` calls a file, as `Arch::name` spells it.
fn upscaler_arch(path: &Path) -> String {
    crate::sr::identify(path).unwrap_or(crate::sr::arch::Arch::Unknown).name()
}

/// An upscaler is had when its own `dest` is there and reads as its `arch`.
fn upscaler_is(dest: &Path, u: &Upscaler) -> bool {
    dest.is_file() && upscaler_arch(dest) == u.arch
}

/// [`file_state`] for an upscaler: the same states, with "have" meaning its
/// `dest` and nothing else (see [`Upscaler`]).
fn upscaler_state(dir: &Path, u: &'static Upscaler, st: &State) -> FileView {
    let dest = dir.join(&u.dest);
    let k = key(&dest);
    let got = std::fs::metadata(part_of(&dest)).map(|m| m.len()).unwrap_or(0);
    let mut v = FileView {
        slot: "upscaler".to_string(),
        slot_label: u.arch.clone(),
        dest: u.dest.clone(),
        bytes: u.bytes,
        page: u.page(),
        state: "missing",
        got: 0,
        name: None,
        why: None,
    };
    if st.running.as_deref() == Some(&k) {
        v.state = "downloading";
        v.got = got;
    } else if st.queue.iter().any(|t| t.key == k) {
        v.state = "queued";
        v.got = got;
    } else if dest.is_file() {
        let arch = upscaler_arch(&dest);
        if arch == u.arch {
            v.state = "have";
            v.name = Some(u.dest.clone());
        } else {
            v.state = "wrong";
            v.why = Some(format!("{} is there, and reads as {arch}", u.dest));
        }
    } else if let Some(why) = st.failed.get(&k) {
        v.state = "failed";
        v.got = got;
        v.why = Some(why.clone());
    } else if got > 0 {
        v.state = "partial";
        v.got = got;
    }
    v
}

fn part_of(dest: &Path) -> PathBuf {
    let mut p = dest.as_os_str().to_owned();
    p.push(".part");
    PathBuf::from(p)
}

/// The whole command line. `-sS` keeps stderr to errors only, which is what
/// lands on the card when it fails; `--fail` makes a 404 or 401 an exit code
/// rather than an HTML page saved under a model's name.
fn curl_command(url: &str, part: &Path) -> Command {
    let mut cmd = Command::new("curl");
    cmd.args(["-L", "--fail", "-sS", "--retry", "3", "--retry-delay", "5", "-C", "-", "-o"])
        .arg(part)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    cmd
}

/// Whether `curl` runs at all, asked once.
pub fn curl_available() -> bool {
    static FOUND: OnceLock<bool> = OnceLock::new();
    *FOUND.get_or_init(|| {
        Command::new("curl")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ImageFamily;
    use crate::discover::{ImageVaeKind, LlmKind, Role};

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("yasdig-dl-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn klein9() -> &'static Entry {
        Catalog::get().entry.iter().find(|e| e.family == crate::catalog::Family::Image(ImageFamily::Flux2Klein9b)).unwrap()
    }

    fn file(e: &'static Entry, slot: &str) -> &'static File {
        e.file.iter().find(|f| f.slot == slot).unwrap()
    }

    #[test]
    fn a_part_under_another_name_counts_as_had() {
        let dir = tmpdir("had");
        let e = klein9();
        let found = vec![
            Found { path: dir.as_path().join("te/my-qwen3-8b.gguf"), role: Role::Llm(LlmKind::Qwen3 { dim: 4096 }) },
            Found { path: dir.as_path().join("vae/other.safetensors"), role: Role::ImageVae(ImageVaeKind::Sd) },
        ];
        let st = State::default();
        let llm = file_state(dir.as_path(), e, file(e, "llm"), &found, &st);
        assert_eq!(llm.state, "have");
        assert_eq!(llm.name.as_deref(), Some("te/my-qwen3-8b.gguf"));
        // An SD VAE is not a Flux.2 VAE, whatever it is called.
        assert_eq!(file_state(dir.as_path(), e, file(e, "vae"), &found, &st).state, "missing");
    }

    #[test]
    fn a_part_file_is_progress_and_the_queue_wins() {
        let dir = tmpdir("part");
        let e = klein9();
        let f = file(e, "llm");
        let part = part_of(&dir.join(&f.dest));
        std::fs::create_dir_all(part.parent().unwrap()).unwrap();
        std::fs::write(&part, vec![0u8; 1234]).unwrap();
        let mut st = State::default();
        let v = file_state(dir.as_path(), e, f, &[], &st);
        assert_eq!((v.state, v.got), ("partial", 1234));
        st.running = Some(key(&dir.join(&f.dest)));
        let v = file_state(dir.as_path(), e, f, &[], &st);
        assert_eq!((v.state, v.got), ("downloading", 1234));
        st.running = None;
        st.failed.insert(key(&dir.join(&f.dest)), "curl failed".into());
        assert_eq!(file_state(dir.as_path(), e, f, &[], &st).state, "failed");
    }

    #[test]
    fn a_dest_holding_something_else_is_wrong_not_had() {
        let dir = tmpdir("wrong");
        let e = klein9();
        let f = file(e, "llm");
        let dest = dir.as_path().join(&f.dest);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(&dest, b"not a model").unwrap();
        let v = file_state(dir.as_path(), e, f, &[], &State::default());
        assert_eq!(v.state, "wrong");
    }

    #[test]
    fn curl_resumes_into_the_part_file() {
        let f = file(klein9(), "llm");
        let part = part_of(&Path::new("/m").join(&f.dest));
        assert!(part.to_string_lossy().ends_with(".gguf.part"));
        let cmd = curl_command(&f.url, &part);
        let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        for want in ["-L", "--fail"] {
            assert!(args.iter().any(|a| a == want), "{want} missing: {args:?}");
        }
        let c = args.iter().position(|a| a == "-C").unwrap();
        assert_eq!(args[c + 1], "-");
        let o = args.iter().position(|a| a == "-o").unwrap();
        assert_eq!(Path::new(&args[o + 1]), part);
        assert_eq!(args.last().unwrap(), &f.url);
    }

    /// The whole path through curl, offline: a `file://` source, a `.part`
    /// holding its first half, and a result that isn't the part it claims to be.
    /// Resume must finish the file rather than restart or double it, and the
    /// role check must name what arrived.
    #[test]
    fn a_download_resumes_renames_and_checks_its_role() {
        if !curl_available() {
            return;
        }
        let dir = tmpdir("fetch");
        let src = dir.join("source.bin");
        let body: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&src, &body).unwrap();
        let models = dir.join("models");
        let e = klein9();
        let index = e.file.iter().position(|f| f.slot == "vae").unwrap();
        let dest = models.join("vae/test.safetensors");
        let t = Task {
            key: key(&dest),
            dest: dest.clone(),
            url: format!("file://{}", src.display()),
            bytes: body.len() as u64,
            want: Want::Part { entry: e.id.clone(), index },
        };
        let part = part_of(&dest);
        std::fs::create_dir_all(part.parent().unwrap()).unwrap();
        std::fs::write(&part, &body[..40_000]).unwrap();
        let d = downloads(&models, &dir.join("upscalers"));
        let err = d.fetch(&t).unwrap_err().to_string();
        assert!(err.contains("reads as"), "{err}");
        assert!(!part.exists());
        assert_eq!(std::fs::read(&dest).unwrap(), body);

        // An upscaler is checked against its arch the same way.
        let u = &Catalog::get().upscaler[0];
        let dest = dir.join("upscalers").join(&u.dest);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        let t = Task {
            key: key(&dest),
            dest: dest.clone(),
            url: format!("file://{}", src.display()),
            bytes: body.len() as u64,
            want: Want::Upscaler { id: u.id.clone() },
        };
        let err = d.fetch(&t).unwrap_err().to_string();
        assert!(err.contains("reads as unknown"), "{err}");
        assert!(dest.is_file());
    }

    fn downloads(models: &Path, upscalers: &Path) -> Downloads {
        Downloads(Arc::new(Inner {
            models: models.to_path_buf(),
            videos: models.join("video"),
            upscalers: upscalers.to_path_buf(),
            loras: models.join("loras"),
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
        }))
    }

    #[test]
    fn an_unknown_entry_is_refused() {
        let d = downloads(Path::new("/nonexistent"), Path::new("/nonexistent/upscalers"));
        assert!(d.cancel("../../etc/passwd").is_err());
        assert!(d.clear("https://example.com/x.gguf").is_err());
        // An upscaler id is a catalog id like any other.
        let u = &Catalog::get().upscaler[0];
        assert_eq!(d.tasks(&u.id).unwrap()[0].dest, Path::new("/nonexistent/upscalers").join(&u.dest));
    }

    /// An upscaler is had only under its own name and arch: a file there that
    /// reads as something else is `wrong`, never `have`, and is never queued over.
    #[test]
    fn an_upscaler_is_had_by_its_dest_and_arch() {
        let dir = tmpdir("up");
        let u = &Catalog::get().upscaler[0];
        let st = State::default();
        assert_eq!(upscaler_state(&dir, u, &st).state, "missing");
        std::fs::write(dir.join(&u.dest), b"not weights").unwrap();
        let v = upscaler_state(&dir, u, &st);
        assert_eq!(v.state, "wrong");
        assert!(v.why.unwrap().contains("unknown"));
    }
}
