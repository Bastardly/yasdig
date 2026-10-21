//! `startup.toml`: where the server listens, and taking that port back.
//!
//! Its own file beside `models.toml` rather than a table inside it, because it
//! is the one setting a person reaches for *before* the app is any use to them
//! — "8080 is my other thing" — and a file with that name and a handful of lines
//! answers it without reading eighty lines about models. Created on the first
//! start of every home, a checkout's included (and gitignored there: a port is
//! the person's, not the repo's). It used to be `[server]` in `models.toml`;
//! a home that still has one is seeded from it, once.
//!
//! The port is a **promise**, not a starting point. A start that finds it taken
//! asks what holds it (`GET /instance`) and acts on the answer — see [`bind`].

use anyhow::{Context, Result};
use serde::Deserialize;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const FILE: &str = "startup.toml";

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Startup {
    #[serde(default)]
    pub server: Server,
    #[serde(default)]
    pub sandbox: Sandbox,
}

/// `[sandbox]`: whether sd-cli runs without network access (`crate::sandbox`).
/// A start-up setting because it describes this machine — whether its kernel,
/// or its GPU driver inside an AppContainer, can run it — not a generation.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Sandbox {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

impl Default for Sandbox {
    fn default() -> Self {
        Sandbox { enabled: true }
    }
}

fn default_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Server {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
}

impl Default for Server {
    fn default() -> Self {
        Server { host: default_host(), port: default_port() }
    }
}

pub fn default_host() -> String {
    "127.0.0.1".to_string()
}
pub fn default_port() -> u16 {
    8080
}

fn template(s: &Server) -> String {
    format!(
        "# How yasdig starts. Read once, when it starts: edit, then restart it.\n\
         \n\
         [server]\n\
         # 127.0.0.1 keeps the page to this computer. \"0.0.0.0\" opens it to\n\
         # your network — there is no password, so only on one you trust.\n\
         host = {host:?}\n\
         \n\
         # The port the page is on: http://127.0.0.1:<port>/\n\
         # If another yasdig is using it (an older build, or another folder),\n\
         # that one is closed and this one takes over. If some other program\n\
         # has it, yasdig leaves it alone and uses the next free port.\n\
         port = {port}\n\
         \n\
         [sandbox]\n\
         # sd-cli, the program that reads model files and draws, runs with no\n\
         # network access, so a booby-trapped model file couldn't send anything\n\
         # anywhere. Set false only if it can't run that way on this computer\n\
         # (`yasdig doctor` says so).\n\
         enabled = true\n",
        host = s.host,
        port = s.port,
    )
}

pub fn path(root: &Path) -> PathBuf {
    root.join(FILE)
}

/// `[sandbox] enabled = false`, read without creating the file: every entry
/// point loads `Config`, not only the server, and `doctor` and `upscale` run
/// sd-cli too. A file that doesn't parse leaves the sandbox on; the server's
/// own `load` reports the error.
pub fn sandbox_off(root: &Path) -> bool {
    std::fs::read_to_string(path(root))
        .ok()
        .and_then(|t| toml::from_str::<Startup>(&t).ok())
        .is_some_and(|s| !s.sandbox.enabled)
}

/// Read `startup.toml`, writing it first if it isn't there — from the old
/// `[server]` in `models.toml` when there was one, so a port someone moved
/// stays moved. Never overwrites.
pub fn load(root: &Path, legacy: Option<&Server>) -> Result<Startup> {
    let p = path(root);
    if !p.exists() {
        let seed = legacy.cloned().unwrap_or_default();
        std::fs::write(&p, template(&seed)).with_context(|| format!("creating {}", p.display()))?;
        if legacy.is_some() {
            println!("[yasdig] moved [server] from models.toml into {}", p.display());
        }
    }
    let text = std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", p.display()))
}

/// What this process is, as `GET /instance` reports it: the folder it serves
/// and which build of the executable it is.
///
/// The build is the executable's size and modification time, read at start —
/// not a hash (100 MB of reading per start) and not the crate version (which
/// every `cargo build` of the day shares). It is what tells "the same thing,
/// double-clicked twice" from "a rebuild while the old one still runs", which
/// were one case before and made `cargo run` keep handing you the stale server.
///
/// **Read once and kept**: asked again later, a server whose file `cargo` has
/// since rebuilt would report the *new* build, and be handed over to as if it
/// were current — the exact case this exists to catch. `main` calls it before
/// anything else can have changed the file.
pub fn build_id() -> &'static str {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(read_build_id)
}

fn read_build_id() -> String {
    std::env::current_exe()
        .and_then(std::fs::metadata)
        .map(|m| {
            let t = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_millis());
            format!("{}-{t}", m.len())
        })
        .unwrap_or_default()
}

/// A yasdig answering on a port.
#[derive(Debug, PartialEq)]
pub struct Instance {
    pub home: String,
    /// Empty from a build that predates the field.
    pub build: String,
}

/// What a start does about the port.
pub enum Bound {
    Listening(TcpListener, String),
    /// This folder, this build, already running: open the browser on it and end.
    HandedOver(String),
}

/// Bind the configured port, taking it back from another yasdig if one has
/// it.
///
/// - **Free**: listen.
/// - **This folder and this build**: hand over. That is a second double-click,
///   and closing the first would kill whatever it is rendering.
/// - **Any other yasdig** — another folder, or this one's older build: ask
///   it to close (`POST /shutdown` with `clear: false`, so its clear-on-close
///   settings don't fire on a takeover nobody pressed a button for), wait for
///   the port, listen. Through the app's own route, not by finding and killing a
///   process, so it is the same on Linux and Windows and can only ever reach a
///   yasdig. A build too old to have that route is stepped past.
/// - **Anything else**: never touched — it is somebody's — and the next free
///   port of 20 is used instead, as before.
pub fn bind(server: &Server, me: &Instance) -> Result<Bound> {
    const TRIES: u16 = 20;
    for port in (server.port..=u16::MAX).take(TRIES as usize) {
        let addr = format!("{}:{port}", server.host);
        let err = match TcpListener::bind(&addr) {
            Ok(l) => return Ok(Bound::Listening(l, addr)),
            Err(e) => e,
        };
        if err.kind() != std::io::ErrorKind::AddrInUse {
            return Err(err).with_context(|| {
                format!("can't listen on {addr} — check {FILE}")
            });
        }
        match instance(&addr) {
            Some(other) if other == *me => return Ok(Bound::HandedOver(addr)),
            Some(other) => {
                let what = if other.home == me.home {
                    "an older build of this yasdig".to_string()
                } else {
                    format!("the yasdig in {}", other.home)
                };
                println!("[yasdig] port {port} is held by {what} — closing it");
                if close(&addr) {
                    if let Some(l) = rebind(&addr) {
                        return Ok(Bound::Listening(l, addr));
                    }
                }
                println!("[yasdig] it didn't let go of port {port}; trying the next one");
            }
            None => {
                println!(
                    "[yasdig] port {port} is used by another program; leaving it be and \
                     trying {}",
                    port.saturating_add(1)
                );
            }
        }
    }
    anyhow::bail!(
        "ports {} to {} are all in use — set another `port` in {FILE}",
        server.port,
        server.port.saturating_add(TRIES - 1),
    )
}

/// One HTTP/1.0 request to a local server, the answer's body if it was a 200.
fn request(addr: &str, req: &str) -> Option<String> {
    let probe = || -> std::io::Result<String> {
        let sock = std::net::ToSocketAddrs::to_socket_addrs(addr)?
            .next()
            .ok_or(std::io::ErrorKind::NotFound)?;
        let mut s = TcpStream::connect_timeout(&sock, Duration::from_secs(2))?;
        s.set_read_timeout(Some(Duration::from_secs(5)))?;
        s.write_all(req.as_bytes())?;
        let mut body = String::new();
        s.read_to_string(&mut body)?;
        Ok(body)
    };
    let reply = probe().ok()?;
    let (head, body) = reply.split_once("\r\n\r\n")?;
    head.starts_with("HTTP/1.1 200").then(|| body.to_string())
}

/// The yasdig holding `addr`, or `None` when what holds it isn't one.
pub fn instance(addr: &str) -> Option<Instance> {
    let body = request(addr, "GET /instance HTTP/1.0\r\nHost: localhost\r\n\r\n")?;
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    // "rustimage" is this program before its rename (2026-10-12): a server of
    // that build still holding the port is ours to take over, not a stranger to
    // step past. Transitional — drop it once no such build can be running.
    if v["app"] != "yasdig" && v["app"] != "rustimage" {
        return None;
    }
    Some(Instance {
        home: v["home"].as_str()?.to_string(),
        build: v["build"].as_str().unwrap_or_default().to_string(),
    })
}

fn close(addr: &str) -> bool {
    let body = r#"{"clear":false}"#;
    let req = format!(
        "POST /shutdown HTTP/1.0\r\nHost: localhost\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    request(addr, &req).is_some()
}

/// The old server answers and then exits, so the port frees a moment later.
fn rebind(addr: &str) -> Option<TcpListener> {
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(l) = TcpListener::bind(addr) {
            return Some(l);
        }
        if Instant::now() > until {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_is_made_once_and_seeded_from_models_toml() {
        let root = std::env::temp_dir().join(format!("yasdig-startup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let old = Server { host: "127.0.0.1".into(), port: 9123 };
        assert_eq!(load(&root, Some(&old)).unwrap().server, old);

        // A second start never rewrites it, whatever models.toml says now.
        std::fs::write(path(&root), "[server]\nport = 9200\n").unwrap();
        let s = load(&root, Some(&old)).unwrap().server;
        assert_eq!((s.host.as_str(), s.port), ("127.0.0.1", 9200));

        // And the shipped text parses to the defaults.
        std::fs::remove_file(path(&root)).unwrap();
        assert_eq!(load(&root, None).unwrap().server, Server::default());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_sandbox_is_on_unless_the_file_says_false() {
        let root = std::env::temp_dir().join(format!("yasdig-sandbox-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        // No file, the shipped file, an older file without the table, a
        // broken one: all on.
        assert!(!sandbox_off(&root));
        assert!(load(&root, None).unwrap().sandbox.enabled);
        assert!(!sandbox_off(&root));
        std::fs::write(path(&root), "[server]\nport = 9200\n").unwrap();
        assert!(!sandbox_off(&root));
        std::fs::write(path(&root), "[sandbox]\nenabled = nope\n").unwrap();
        assert!(!sandbox_off(&root));
        std::fs::write(path(&root), "[server]\nport = 9200\n\n[sandbox]\nenabled = false\n").unwrap();
        assert!(sandbox_off(&root));
        std::fs::remove_dir_all(&root).ok();
    }
}
