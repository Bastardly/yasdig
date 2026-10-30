//! One agent turn: the request file for `yasdig-llm` (the llama.cpp driver in
//! `llm/`), the run, and its framed stdout read back.
//!
//! The driver is a subprocess for the reasons sd-cli is — isolation, and a
//! model file is untrusted C++ input, so it runs in the same sandbox
//! ([`crate::sandbox::confine`]) and on the same [`Monitor`], which is what lets
//! the Jobs dialog's Stop kill a turn like any render. Nothing is kept between
//! turns: every one loads the weights again, which costs a few seconds and buys
//! a GPU that is free for sd-cli the moment the turn is over.
//!
//! The request is a file, not stdin, because the Windows sandbox launcher hands
//! its child a NUL stdin (`sandbox::confine`).

use crate::config::Config;
use crate::sd::{self, Monitor, Phase};
use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// The driver's protocol version, its first stdout line (`@yasdig-llm 1`) and
/// the request's `v`. A driver from another build is refused rather than
/// half-understood.
pub const PROTOCOL: u32 = 1;

/// What one turn is asked to do. Serialized as the request file.
#[derive(Debug, Clone, Serialize)]
pub struct Request {
    pub v: u32,
    pub model: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mmproj: Option<PathBuf>,
    pub n_ctx: u32,
    pub n_predict: u32,
    /// Whether a template with a thinking switch thinks first. Off: the reply
    /// is constrained to a JSON object whose first field is a scratchpad.
    pub thinking: bool,
    pub sampling: Sampling,
    /// The reply's shape; llama.cpp turns it into the grammar it samples under.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json_schema: Option<serde_json::Value>,
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone, Copy, Serialize, serde::Deserialize, PartialEq)]
pub struct Sampling {
    pub temp: f32,
    pub top_p: f32,
    pub top_k: u32,
    pub min_p: f32,
    pub seed: u32,
}

impl Default for Sampling {
    /// For a file the catalog doesn't know: the instruct models' published
    /// middle ground, cooled a little since every reply is a JSON object.
    fn default() -> Self {
        Sampling { temp: 0.6, top_p: 0.95, top_k: 20, min_p: 0.0, seed: 0 }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Message {
    pub role: Role,
    pub content: Vec<Part>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Part {
    Text { text: String },
    /// A PNG staged into `work/current/` — never a durable path (invariant 8),
    /// and on Windows the only place the container may read besides the models.
    Image { path: PathBuf },
}

impl Message {
    pub fn text(role: Role, text: impl Into<String>) -> Self {
        Message { role, content: vec![Part::Text { text: text.into() }] }
    }
}

/// What a finished turn hands back.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Reply {
    /// The reply as the template's own parser reads it: thinking split off.
    pub content: String,
    pub reasoning: String,
    pub stats: Stats,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize, Serialize)]
pub struct Stats {
    #[serde(default)]
    pub n_prompt: u32,
    #[serde(default)]
    pub n_gen: u32,
    #[serde(default)]
    pub load_ms: u64,
    #[serde(default)]
    pub read_ms: u64,
    #[serde(default)]
    pub gen_ms: u64,
    /// `eos`, or `length` when the reply hit `n_predict` — a JSON object cut
    /// off half way, which the caller refuses.
    #[serde(default)]
    pub stop: String,
}

/// The conversation didn't fit the context. Typed, because the caller retries
/// once with less history rather than reporting it.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextOverflow {
    pub need: u32,
    pub have: u32,
}

impl std::fmt::Display for ContextOverflow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the conversation needs {} tokens and the agent's context holds {}",
            self.need, self.have
        )
    }
}

impl std::error::Error for ContextOverflow {}

/// One stdout record.
#[derive(Debug, Clone, PartialEq)]
enum Frame {
    Hello(u32),
    Load(u32, u32),
    Read(u32, u32),
    Text(String),
    Content { content: String, reasoning: String },
    Done(Stats),
    Error { kind: String, msg: String, need: Option<u32>, have: Option<u32> },
}

/// A stdout line as a record, or `None` for anything else — library log lines
/// that reach stdout, a line cut by a kill.
fn parse_frame(line: &str) -> Option<Frame> {
    let (tag, body) = line.split_once(' ')?;
    match tag {
        "@yasdig-llm" => body.trim().parse().ok().map(Frame::Hello),
        "P" => {
            let mut it = body.split_whitespace();
            let what = it.next()?;
            let done = it.next()?.parse().ok()?;
            let total = it.next()?.parse().ok()?;
            match what {
                "load" => Some(Frame::Load(done, total)),
                "read" => Some(Frame::Read(done, total)),
                _ => None,
            }
        }
        "T" => serde_json::from_str::<String>(body).ok().map(Frame::Text),
        "C" => {
            let v: serde_json::Value = serde_json::from_str(body).ok()?;
            Some(Frame::Content {
                content: v.get("content")?.as_str()?.to_string(),
                reasoning: v.get("reasoning").and_then(|r| r.as_str()).unwrap_or("").to_string(),
            })
        }
        "D" => serde_json::from_str(body).ok().map(Frame::Done),
        "E" => {
            let v: serde_json::Value = serde_json::from_str(body).ok()?;
            let num = |k: &str| v.get(k).and_then(|n| n.as_u64()).map(|n| n as u32);
            Some(Frame::Error {
                kind: v.get("kind")?.as_str()?.to_string(),
                msg: v.get("msg").and_then(|m| m.as_str()).unwrap_or("").to_string(),
                need: num("need"),
                have: num("have"),
            })
        }
        _ => None,
    }
}

/// Where a turn's request file goes: scratch, so start and Close server sweep it.
pub fn request_path(work_dir: &Path) -> PathBuf {
    crate::pipeline::scratch_dir(work_dir).join("agent-request.json")
}

pub struct LlmRunner<'a> {
    cfg: &'a Config,
    monitor: Monitor,
}

impl<'a> LlmRunner<'a> {
    pub fn watched(cfg: &'a Config, monitor: &Monitor) -> Self {
        LlmRunner { cfg, monitor: monitor.clone() }
    }

    pub fn binary(&self) -> PathBuf {
        self.cfg.llm_binary()
    }

    /// Run one turn. `chat` names the conversation the live text
    /// ([`Monitor::live_text`]) belongs to.
    pub fn run(&self, req: &Request, label: &str, chat: &str) -> Result<Reply> {
        let bin = self.binary();
        if !bin.is_file() {
            bail!(
                "the agent's driver isn't here: {}. This build has no agent inside it — \
                 docs/building.md says how to build one, or set [llm].binary in models.toml",
                bin.display()
            );
        }
        // sd-cli's last preview frame would otherwise show while the agent reads,
        // since `Monitor::preview` answers for whatever run is in flight.
        if let Some(dir) = self.monitor.preview_dir() {
            let _ = std::fs::remove_dir_all(dir);
            std::fs::create_dir_all(dir).ok();
        }
        let work = self.cfg.base_dir.join("work");
        let path = request_path(&work);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        std::fs::write(&path, serde_json::to_vec(req)?).with_context(|| format!("writing {}", path.display()))?;

        let mut cmd = Command::new(&bin);
        cmd.arg("--request").arg(&path);
        let mut cmd = crate::sandbox::confine(cmd, &self.cfg.sandbox_roots())?;
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().with_context(|| {
            let hint = if crate::sandbox::enabled() {
                format!(
                    " (it runs without network access; if this system can't set that up, {} \
                     turns it off — `yasdig doctor` says which)",
                    crate::sandbox::TURN_OFF
                )
            } else {
                String::new()
            };
            format!("failed to launch the agent for {label}{hint}")
        })?;
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");
        // As for sd-cli: a loader that finds no GPU inside the Windows container
        // falls back to the CPU and takes forever, so the line stops the turn.
        let blind = Arc::new(AtomicBool::new(false));
        let watch = cfg!(windows) && crate::sandbox::enabled();
        let spot = {
            let (blind, monitor) = (blind.clone(), self.monitor.clone());
            move |line: &str| {
                if watch && line.contains(sd::NO_GPU) && !blind.swap(true, Ordering::SeqCst) {
                    monitor.cancel();
                }
            }
        };
        let errors = {
            let spot = spot.clone();
            std::thread::spawn(move || sd::drain(stderr, spot))
        };

        self.monitor.begin(label, child);
        self.monitor.set_live_text(Some(chat));
        let mut reader = Reader::new(req.n_predict);
        let out_tail = sd::drain(stdout, |line| {
            spot(line);
            if let Some(f) = parse_frame(line) {
                reader.take(f, |phase, step, steps| self.monitor.advance_in(label, phase, step, steps, ""), |t| {
                    self.monitor.push_live_text(t)
                });
            }
        });
        let status = match self.monitor.end() {
            Some(mut child) => child.wait().with_context(|| format!("waiting for the agent ({label})"))?,
            None => bail!("the agent ({label}): the child went missing while it ran"),
        };
        self.monitor.set_live_text(None);
        let err_tail = errors.join().unwrap_or_default();
        let _ = std::fs::remove_file(&path);
        reader.finish(Exit {
            success: status.success(),
            code: status.code(),
            cancelled: self.monitor.was_cancelled(),
            blind: blind.load(Ordering::SeqCst),
            killed: sd::killed_by_system(&status),
            label,
            out: &out_tail,
            err: &err_tail,
        })
    }
}

/// How the child ended, for [`Reader::finish`].
struct Exit<'a> {
    success: bool,
    code: Option<i32>,
    cancelled: bool,
    blind: bool,
    killed: bool,
    label: &'a str,
    out: &'a VecDeque<String>,
    err: &'a VecDeque<String>,
}

/// The records of one turn, folded as they arrive. Apart from the runner so
/// the decisions — what a missing `D` means, which failure wins — are tested
/// without a child.
struct Reader {
    n_predict: u32,
    hello: Option<u32>,
    written: u32,
    text: String,
    content: Option<(String, String)>,
    done: Option<Stats>,
    error: Option<Frame>,
}

impl Reader {
    fn new(n_predict: u32) -> Self {
        Reader { n_predict, hello: None, written: 0, text: String::new(), content: None, done: None, error: None }
    }

    fn take(&mut self, f: Frame, mut progress: impl FnMut(Phase, u32, u32), mut text: impl FnMut(&str)) {
        match f {
            Frame::Hello(v) => self.hello = Some(v),
            Frame::Load(d, t) => progress(Phase::Loading, d, t),
            Frame::Read(d, t) => progress(Phase::Reading, d, t),
            Frame::Text(t) => {
                self.written += 1;
                self.text.push_str(&t);
                text(&t);
                progress(Phase::Writing, self.written, self.n_predict);
            }
            Frame::Content { content, reasoning } => self.content = Some((content, reasoning)),
            Frame::Done(s) => self.done = Some(s),
            e @ Frame::Error { .. } => self.error = Some(e),
        }
    }

    fn finish(self, x: Exit) -> Result<Reply> {
        let label = x.label;
        let logs = || {
            format!(
                "--- stderr ---\n{}\n--- stdout (last {} lines) ---\n{}",
                sd::joined(x.err),
                sd::TAIL_LINES,
                sd::joined(x.out)
            )
        };
        if x.blind {
            bail!(
                "the agent ({label}): no GPU is visible from inside the sandbox (\"{}\"), so it was \
                 stopped before it fell back to the CPU. {} runs it without the sandbox.",
                sd::NO_GPU,
                crate::sandbox::TURN_OFF
            );
        }
        if !x.success && x.cancelled {
            bail!("the agent ({label}) {}", sd::CANCELLED);
        }
        if let Some(v) = self.hello {
            if v != PROTOCOL {
                bail!(
                    "the agent's driver speaks protocol {v} and this yasdig speaks {PROTOCOL} — \
                     [llm].binary points at a driver from another build"
                );
            }
        }
        let gpu_oom = x.out.iter().chain(x.err.iter()).any(|l| sd::is_out_of_memory(l));
        if let Some(Frame::Error { kind, msg, need, have }) = &self.error {
            if kind == "context" {
                return Err(ContextOverflow { need: need.unwrap_or(0), have: have.unwrap_or(0) }.into());
            }
            if kind == "oom" || (kind == "load" && gpu_oom) {
                bail!(out_of_memory(label, "gpu"));
            }
            bail!("the agent ({label}): {msg}\n{}", logs());
        }
        if x.killed {
            bail!(out_of_memory(label, "ram"));
        }
        if !x.success {
            if gpu_oom {
                bail!(out_of_memory(label, "gpu"));
            }
            let code = x.code.map_or("a signal".to_string(), sd::exit_code_text);
            bail!("the agent ({label}) exited with {code}\n{}", logs());
        }
        let Some(stats) = self.done else {
            bail!("the agent ({label}) ended without finishing its reply\n{}", logs());
        };
        let (content, reasoning) = self.content.unwrap_or((self.text, String::new()));
        Ok(Reply { content, reasoning, stats })
    }
}

/// The message for an agent that didn't fit. Plain words, not the sd OOM box:
/// the ways out are a smaller agent model or a shorter context, and neither is
/// a button.
fn out_of_memory(label: &str, kind: &str) -> String {
    let what = if kind == "ram" { "system memory" } else { "graphics memory" };
    format!(
        "the agent ({label}) ran out of {what}. Pick a smaller agent model beside the Assistant's \
         Send button, or one whose file is a smaller quantization."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exit<'a>(out: &'a VecDeque<String>, err: &'a VecDeque<String>) -> Exit<'a> {
        Exit { success: true, code: Some(0), cancelled: false, blind: false, killed: false, label: "chat", out, err }
    }

    fn read(lines: &[&str]) -> Reader {
        let mut r = Reader::new(100);
        for l in lines {
            if let Some(f) = parse_frame(l) {
                r.take(f, |_, _, _| {}, |_| {});
            }
        }
        r
    }

    #[test]
    fn records_parse_and_anything_else_is_ignored() {
        assert_eq!(parse_frame("@yasdig-llm 1"), Some(Frame::Hello(1)));
        assert_eq!(parse_frame("P load 40 100"), Some(Frame::Load(40, 100)));
        assert_eq!(parse_frame("P read 2 5"), Some(Frame::Read(2, 5)));
        assert_eq!(parse_frame(r#"T "a\nb \"c\" é""#), Some(Frame::Text("a\nb \"c\" é".into())));
        assert_eq!(parse_frame("load_tensors: offloading 36 layers"), None);
        assert_eq!(parse_frame("T \"cut off"), None);
        assert_eq!(parse_frame("P warp 1 2"), None);
        assert_eq!(
            parse_frame(r#"E {"kind":"context","msg":"x","need":9000,"have":8192}"#),
            Some(Frame::Error { kind: "context".into(), msg: "x".into(), need: Some(9000), have: Some(8192) })
        );
    }

    #[test]
    fn a_finished_turn_hands_back_the_templates_parse() {
        let (o, e) = (VecDeque::new(), VecDeque::new());
        let r = read(&[
            "@yasdig-llm 1",
            r#"T "{\"notes\":"#,
            r#"T "\"x\"}""#,
            r#"C {"content":"{\"notes\":\"x\"}","reasoning":""}"#,
            r#"D {"n_prompt":10,"n_gen":2,"load_ms":1,"read_ms":2,"gen_ms":3,"stop":"eos"}"#,
        ]);
        let reply = r.finish(exit(&o, &e)).unwrap();
        assert_eq!(reply.content, r#"{"notes":"x"}"#);
        assert_eq!(reply.stats.n_gen, 2);
        assert_eq!(reply.stats.stop, "eos");
    }

    #[test]
    fn exit_zero_without_done_is_a_failure() {
        let (o, e) = (VecDeque::new(), VecDeque::new());
        let err = read(&["@yasdig-llm 1", r#"T "{""#]).finish(exit(&o, &e)).unwrap_err();
        assert!(err.to_string().contains("without finishing"), "{err}");
    }

    #[test]
    fn a_context_overflow_is_typed_so_the_caller_can_retry() {
        let (o, e) = (VecDeque::new(), VecDeque::new());
        let r = read(&["@yasdig-llm 1", r#"E {"kind":"context","msg":"x","need":9000,"have":8192}"#]);
        let err = r.finish(Exit { success: false, code: Some(4), ..exit(&o, &e) }).unwrap_err();
        assert_eq!(err.downcast_ref::<ContextOverflow>(), Some(&ContextOverflow { need: 9000, have: 8192 }));
    }

    #[test]
    fn stop_wins_over_whatever_the_kill_left_behind() {
        let (o, e) = (VecDeque::new(), VecDeque::new());
        let err = read(&["@yasdig-llm 1"])
            .finish(Exit { success: false, code: None, cancelled: true, ..exit(&o, &e) })
            .unwrap_err();
        assert!(err.to_string().ends_with(sd::CANCELLED), "{err}");
    }

    #[test]
    fn running_out_of_memory_says_what_to_pick_instead() {
        let o = VecDeque::new();
        let e: VecDeque<String> = ["ggml_vulkan: ErrorOutOfDeviceMemory".to_string()].into();
        let r = read(&["@yasdig-llm 1", r#"E {"kind":"load","msg":"could not load"}"#]);
        let err = r.finish(Exit { success: false, code: Some(3), ..exit(&o, &e) }).unwrap_err();
        assert!(err.to_string().contains("graphics memory"), "{err}");
        let err = read(&[]).finish(Exit { success: false, code: None, killed: true, ..exit(&o, &o) }).unwrap_err();
        assert!(err.to_string().contains("system memory"), "{err}");
    }

    #[test]
    fn a_driver_from_another_build_is_refused() {
        let (o, e) = (VecDeque::new(), VecDeque::new());
        let r = read(&["@yasdig-llm 2", r#"D {"stop":"eos"}"#]);
        assert!(r.finish(exit(&o, &e)).unwrap_err().to_string().contains("protocol 2"));
    }

    #[test]
    fn the_request_file_is_the_drivers_shape() {
        let req = Request {
            v: PROTOCOL,
            model: "/m/a.gguf".into(),
            mmproj: None,
            n_ctx: 8192,
            n_predict: 512,
            thinking: false,
            sampling: Sampling::default(),
            json_schema: Some(serde_json::json!({"type": "object"})),
            messages: vec![
                Message::text(Role::System, "be brief"),
                Message { role: Role::User, content: vec![Part::Image { path: "/w/a.png".into() }] },
            ],
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["v"], 1);
        assert!(v.get("mmproj").is_none());
        assert_eq!(v["messages"][0]["role"], "system");
        assert_eq!(v["messages"][0]["content"][0], serde_json::json!({"type": "text", "text": "be brief"}));
        assert_eq!(v["messages"][1]["content"][0], serde_json::json!({"type": "image", "path": "/w/a.png"}));
    }
}
