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

use crate::catalog::{Catalog, Entry, File};
use crate::config::{slash_name, Config};
use crate::discover::{self, Found};
use anyhow::{bail, Result};
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

/// One file to fetch. Keyed by `dest` everywhere, not by entry: Z-Image and
/// both Flux.1s share one `ae.safetensors`, and a download started from one
/// card is the same download on the others.
#[derive(Clone)]
struct Task {
    dest: String,
    url: String,
    bytes: u64,
    /// The entry it was asked for under, and the file's index in it, so the
    /// finished file can be checked against the slot it was meant to fill.
    entry: String,
    index: usize,
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
    state: Mutex<State>,
    wake: Condvar,
}

impl Downloads {
    pub fn start(cfg: &Config) -> Result<Downloads> {
        let d = Downloads(Arc::new(Inner {
            models: cfg.models_dir(),
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
        }));
        let worker = d.clone();
        std::thread::Builder::new()
            .name("downloads".into())
            .spawn(move || worker.work())?;
        Ok(d)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Queue every file of `id` that nothing on disk fills yet. Answers how many
    /// were queued; 0 is not an error (the entry is already complete).
    pub fn enqueue(&self, cfg: &Config, id: &str) -> Result<usize> {
        let Some(entry) = Catalog::get().entry(id) else {
            bail!("no catalog entry named {id:?}");
        };
        if !curl_available() {
            bail!("curl was not found on the PATH; install it, or download the files by hand (README.md, Get a model)");
        }
        let found = cfg.scan_images();
        let mut st = self.lock();
        let mut n = 0;
        for (index, f) in entry.file.iter().enumerate() {
            let busy = st.running.as_deref() == Some(&f.dest) || st.queue.iter().any(|t| t.dest == f.dest);
            if busy || present(&self.0.models, entry, f, &found).is_some() {
                continue;
            }
            // A dest that exists but isn't what the slot takes is never
            // overwritten: it is somebody's file, and the card says what it is.
            if self.0.models.join(&f.dest).exists() {
                continue;
            }
            st.failed.remove(&f.dest);
            st.queue.push_back(Task {
                dest: f.dest.clone(),
                url: f.url.clone(),
                bytes: f.bytes,
                entry: entry.id.clone(),
                index,
            });
            n += 1;
        }
        drop(st);
        self.0.wake.notify_all();
        Ok(n)
    }

    /// Stop every file of `id`: queued ones are dropped, a running one is
    /// killed. Its `.part` stays, so Download resumes.
    pub fn cancel(&self, id: &str) -> Result<()> {
        let Some(entry) = Catalog::get().entry(id) else {
            bail!("no catalog entry named {id:?}");
        };
        let mut st = self.lock();
        let mine = |dest: &str| entry.file.iter().any(|f| f.dest == dest);
        st.queue.retain(|t| !mine(&t.dest));
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
        let Some(entry) = Catalog::get().entry(id) else {
            bail!("no catalog entry named {id:?}");
        };
        let mut st = self.lock();
        for f in &entry.file {
            let busy = st.running.as_deref() == Some(&f.dest) || st.queue.iter().any(|t| t.dest == f.dest);
            if !busy {
                let _ = std::fs::remove_file(part_path(&self.0.models, &f.dest));
                st.failed.remove(&f.dest);
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
                        st.running = Some(t.dest.clone());
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
                    eprintln!("[rustimage] download {} failed: {e:#}", task.dest);
                    st.failed.insert(task.dest.clone(), format!("{e:#}"));
                }
            }
        }
    }

    fn fetch(&self, t: &Task) -> Result<()> {
        let dest = self.0.models.join(&t.dest);
        let part = part_path(&self.0.models, &t.dest);
        if let Some(dir) = dest.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let have = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        if have > t.bytes {
            // Longer than the file can be: not a prefix of it, so not resumable.
            std::fs::remove_file(&part)?;
        }
        if have != t.bytes {
            println!("[rustimage] downloading {} ({})", t.dest, t.url);
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
        std::fs::rename(&part, &dest)?;
        println!("[rustimage] downloaded {}", t.dest);
        // What it *is*, read off its header like every other file: a catalog
        // entry pointing at the wrong file is reported here, on the card, not
        // minutes into a render. The file is kept — it is a whole download, and
        // the card names what it turned out to be.
        let entry = Catalog::get().entry(&t.entry).expect("tasks come from the catalog");
        let f = &entry.file[t.index];
        let role = discover::identify(&dest);
        if !entry.fits(f, &role) {
            bail!(
                "downloaded, but it reads as {} — not the {} {} takes",
                role.describe(),
                entry.slot_label(f),
                entry.family.label()
            );
        }
        Ok(())
    }

    /// The catalog with every file's state, for `GET /catalog`.
    pub fn status(&self, cfg: &Config) -> CatalogView {
        let found = cfg.scan_images();
        let st = self.lock();
        let entries = Catalog::get()
            .entry
            .iter()
            .map(|e| EntryView {
                id: &e.id,
                family: e.family.as_str(),
                family_label: e.family.label(),
                label: &e.label,
                note: &e.note,
                bytes: e.bytes(),
                files: e
                    .file
                    .iter()
                    .map(|f| file_state(&self.0.models, e, f, &found, &st))
                    .collect(),
            })
            .collect();
        CatalogView {
            curl: curl_available(),
            finished: st.finished,
            models_dir: self.0.models.display().to_string(),
            entries,
        }
    }
}

#[derive(Serialize)]
pub struct CatalogView {
    curl: bool,
    finished: u64,
    models_dir: String,
    entries: Vec<EntryView>,
}

#[derive(Serialize)]
struct EntryView {
    id: &'static str,
    family: &'static str,
    family_label: &'static str,
    label: &'static str,
    note: &'static str,
    bytes: u64,
    files: Vec<FileView>,
}

#[derive(Serialize, Debug, PartialEq)]
struct FileView {
    slot: String,
    slot_label: &'static str,
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
fn present(models: &Path, e: &Entry, f: &File, found: &[Found]) -> Option<PathBuf> {
    let dest = models.join(&f.dest);
    if dest.exists() {
        return e.fits(f, &discover::identify(&dest)).then_some(dest);
    }
    found.iter().find(|x| e.fits(f, &x.role)).map(|x| x.path.clone())
}

fn file_state(models: &Path, e: &'static Entry, f: &'static File, found: &[Found], st: &State) -> FileView {
    let part = part_path(models, &f.dest);
    let got = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    let mut v = FileView {
        slot: f.slot.clone(),
        slot_label: e.slot_label(f),
        dest: f.dest.clone(),
        bytes: f.bytes,
        page: f.page(),
        state: "missing",
        got: 0,
        name: None,
        why: None,
    };
    if st.running.as_deref() == Some(&f.dest) {
        v.state = "downloading";
        v.got = got;
    } else if st.queue.iter().any(|t| t.dest == f.dest) {
        v.state = "queued";
        v.got = got;
    } else if let Some(p) = present(models, e, f, found) {
        v.state = "have";
        v.name = Some(slash_name(models, &p));
    } else if models.join(&f.dest).exists() {
        v.state = "wrong";
        v.why = Some(format!("{} is there, and reads as {}", f.dest, discover::identify(&models.join(&f.dest)).describe()));
    } else if let Some(why) = st.failed.get(&f.dest) {
        v.state = "failed";
        v.got = got;
        v.why = Some(why.clone());
    } else if got > 0 {
        v.state = "partial";
        v.got = got;
    }
    v
}

fn part_path(models: &Path, dest: &str) -> PathBuf {
    let mut p = models.join(dest).into_os_string();
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
        let dir = std::env::temp_dir().join(format!("rustimage-dl-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn klein9() -> &'static Entry {
        Catalog::get().entry.iter().find(|e| e.family == ImageFamily::Flux2Klein9b).unwrap()
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
        let part = part_path(dir.as_path(), &f.dest);
        std::fs::create_dir_all(part.parent().unwrap()).unwrap();
        std::fs::write(&part, vec![0u8; 1234]).unwrap();
        let mut st = State::default();
        let v = file_state(dir.as_path(), e, f, &[], &st);
        assert_eq!((v.state, v.got), ("partial", 1234));
        st.running = Some(f.dest.clone());
        let v = file_state(dir.as_path(), e, f, &[], &st);
        assert_eq!((v.state, v.got), ("downloading", 1234));
        st.running = None;
        st.failed.insert(f.dest.clone(), "curl failed".into());
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
        let part = part_path(Path::new("/m"), &f.dest);
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
        let t = Task {
            dest: "vae/test.safetensors".into(),
            url: format!("file://{}", src.display()),
            bytes: body.len() as u64,
            entry: e.id.clone(),
            index,
        };
        let part = part_path(&models, &t.dest);
        std::fs::create_dir_all(part.parent().unwrap()).unwrap();
        std::fs::write(&part, &body[..40_000]).unwrap();
        let d = Downloads(Arc::new(Inner {
            models: models.clone(),
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
        }));
        let err = d.fetch(&t).unwrap_err().to_string();
        assert!(err.contains("reads as"), "{err}");
        assert!(!part.exists());
        assert_eq!(std::fs::read(models.join(&t.dest)).unwrap(), body);
    }

    #[test]
    fn an_unknown_entry_is_refused() {
        let d = Downloads(Arc::new(Inner {
            models: PathBuf::from("/nonexistent"),
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
        }));
        assert!(d.cancel("../../etc/passwd").is_err());
        assert!(d.clear("https://example.com/x.gguf").is_err());
    }
}
