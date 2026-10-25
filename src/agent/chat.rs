//! A chat on disk: `work/chats/{id}/` (Current) or `user/favorite_chats/{id}/`
//! (Favorites), holding `chat.json`, the pictures it shows and a thumbnail.
//!
//! The same shape as `videos::VideoStore`: an id names one chat wherever it
//! lives ([`ChatStore::dir`] looks in both), a favourite is a **move**, and
//! *Delete all* and Close server's clear reach the Current shelf only.
//!
//! **A chat owns copies of every picture it shows** (`img/`, at most
//! [`PREVIEW_SIDE`]), and uploads (`att/`): deleting a work item never breaks
//! a chat, and a turn stages from these into scratch (invariant 8), never from
//! a durable folder.
//!
//! Two threads write a chat — the worker (a turn's reply) and connection
//! threads (the person's edits) — so every write is [`ChatStore::update`]:
//! read, change, write a `.part`, rename, under one process-wide lock.

use crate::workitems::{valid_id, Shelf};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The longest side of a chat's own copy of a picture.
pub const PREVIEW_SIDE: u32 = 512;

static WRITES: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Chat {
    #[serde(default = "one")]
    pub v: u32,
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub created_ms: u64,
    #[serde(default)]
    pub updated_ms: u64,
    /// Moves on every write, so the page knows to re-read and a stale
    /// settings edit is refused (409).
    #[serde(default)]
    pub rev: u64,
    /// The agent model's name (`agents::AgentModel::name`).
    #[serde(default)]
    pub agent: Option<String>,
    /// The chat's copy of the Image tab: a `/generate` body.
    #[serde(default = "empty_object")]
    pub settings: Value,
    /// Moves with every change to `settings`; a batch records the one it ran.
    #[serde(default)]
    pub settings_rev: u64,
    #[serde(default)]
    pub messages: Vec<Msg>,
    #[serde(default)]
    pub changes: Vec<Change>,
    #[serde(default)]
    pub actions: Vec<ActionRec>,
    #[serde(default)]
    pub session: Session,
    /// Where it was read from; never stored.
    #[serde(skip)]
    pub favorite: bool,
}

fn one() -> u32 {
    1
}
fn empty_object() -> Value {
    Value::Object(Map::new())
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// An answer, maybe a settings change and a batch.
    #[default]
    Chat,
    /// The start of a session: checks for the goal, maybe a prompt rewrite.
    Plan,
    /// A look at a batch: checks answered per picture, keepers, what next.
    Critique,
    /// A picture described as a prompt. Never started now (describing is a
    /// chat action); kept so older chats read.
    Describe,
    /// The end of a comparison: which value answered the question, from the
    /// server's count of the checks.
    Verdict,
}

/// A comparison: one round per value of one knob, the same seeds throughout,
/// so the difference between rounds is the value (`session::start_sweep`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Sweep {
    /// The knob's key (`knobs::KNOBS`).
    pub knob: String,
    /// Checked and brought inside its bounds when the comparison began.
    pub values: Vec<Value>,
    /// What the knob held before, put back when the verdict picks nothing.
    pub before: Value,
}

/// A picture a message shows.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImageRef {
    /// `item` (a work item, `id` its work id), `output` (a saved output's
    /// stem) or `attach` (an upload, `id` its file name under `att/`).
    pub kind: String,
    pub id: String,
    /// The chat's own copy under `img/` (or `att/` for an upload).
    pub file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
}

/// One picture's answers to the session's checks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CheckRow {
    /// 1-based, as the agent was shown them.
    pub image: u32,
    pub results: Vec<bool>,
}

// An agent message is the big one by far, and the common one: boxing it would
// buy nothing for a list of a few dozen and cost every match a deref.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum Msg {
    User {
        n: u32,
        #[serde(default)]
        ms: u64,
        text: String,
        #[serde(default)]
        images: Vec<ImageRef>,
    },
    Agent {
        n: u32,
        #[serde(default)]
        ms: u64,
        mode: Mode,
        #[serde(default)]
        reply: String,
        /// The model's scratchpad: what earlier turns are shortened to.
        #[serde(default)]
        notes: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        checks: Vec<CheckRow>,
        /// The pictures it picked, 1-based in the batch it saw.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        keepers: Vec<u32>,
        /// The pictures every check said yes to, by the server's count.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        passed: Vec<u32>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        changes: Vec<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action: Option<u64>,
        /// A Describe turn's prompt.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        negative: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        skills: Vec<String>,
        #[serde(default)]
        stats: crate::llm::Stats,
    },
    Batch {
        n: u32,
        #[serde(default)]
        ms: u64,
        round: u32,
        settings_rev: u64,
        /// What the round ran with, in a line — what a later turn is told
        /// instead of being shown the pictures again.
        #[serde(default)]
        summary: String,
        /// How many pictures the round holds when whole; 0 in chats written
        /// before a round could be sized, which meant the default four
        /// (`session::SESSION_BATCH`).
        #[serde(default)]
        size: u32,
        images: Vec<ImageRef>,
    },
    /// A line from yasdig itself: stopped, a failure, a clamp.
    Note {
        n: u32,
        #[serde(default)]
        ms: u64,
        text: String,
    },
}

impl Msg {
    pub fn n(&self) -> u32 {
        match self {
            Msg::User { n, .. } | Msg::Agent { n, .. } | Msg::Batch { n, .. } | Msg::Note { n, .. } => *n,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeState {
    #[default]
    Applied,
    /// Set aside; also what an ask-first proposal of an older chat reads as.
    #[serde(alias = "pending")]
    Rejected,
    Undone,
}

/// A settings change: the agent's, or the person's undo.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Change {
    pub id: u64,
    /// `agent` or `user`.
    pub by: String,
    pub state: ChangeState,
    /// Knob key → value (`knobs::value_of`'s spelling).
    pub set: Map<String, Value>,
    #[serde(default)]
    pub before: Map<String, Value>,
    #[serde(default)]
    pub reason: String,
    /// The message it came with.
    #[serde(default)]
    pub msg: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActionState {
    #[default]
    Queued,
    Done,
    #[serde(alias = "pending")]
    Rejected,
    Failed,
}

/// A pass the agent asked for on one of its pictures.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActionRec {
    pub id: u64,
    /// `hires`, `refine` or `edit`.
    pub kind: String,
    /// The work item it runs on.
    pub item: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruction: Option<String>,
    pub state: ActionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<ImageRef>,
    #[serde(default)]
    pub msg: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// Also what an older chat paused on an ask-first proposal reads as.
    #[default]
    #[serde(alias = "awaiting")]
    Idle,
    Queued,
    Running,
    Done,
    Stopped,
    Failed,
    /// It was queued or running when the server stopped.
    Interrupted,
}

/// What the agent does next, when its leg resumes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Next {
    /// Answer the newest message.
    #[default]
    Turn,
    /// Draw the next round.
    Batch,
    /// Look at the round just drawn.
    Critique,
    /// Run the queued pass.
    Escalate,
    Nothing,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Session {
    pub state: State,
    /// The mode the next turn runs in.
    #[serde(default)]
    pub mode: Mode,
    #[serde(default)]
    pub next: Next,
    /// A session's goal, in the person's words; empty for plain chat.
    #[serde(default)]
    pub goal: String,
    /// Yes/no questions the pictures are judged by, from the plan turn.
    #[serde(default)]
    pub checks: Vec<String>,
    /// Rounds drawn in this chat so far — chat rounds and session rounds
    /// alike, so a round's number is never reused.
    #[serde(default)]
    pub round: u32,
    /// The rounds drawn before this session began; `round - first_round` is
    /// how many of its `rounds` the session has used.
    #[serde(default)]
    pub first_round: u32,
    /// The seed picture 1 of the latest round started from; picture k is
    /// `base_seed + k`.
    #[serde(default)]
    pub base_seed: i64,
    /// The next round's size; 0 is the session's `per_round`.
    #[serde(default)]
    pub count: u32,
    /// Rounds this session may draw (the plan's, from the goal); 0 is
    /// `session::SESSION_ROUNDS`.
    #[serde(default)]
    pub rounds: u32,
    /// Pictures per round of this session; 0 is `session::SESSION_BATCH`.
    #[serde(default)]
    pub per_round: u32,
    /// The next round draws new seeds rather than the last round's.
    #[serde(default)]
    pub fresh: bool,
    /// A comparison, when this session is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sweep: Option<Sweep>,
    /// The pictures the person attached when the session began: what the
    /// plan and every critique judge against.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reference: Vec<ImageRef>,
    /// The job carrying the session now, while queued or running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Session {
    /// Whether a job is carrying the chat.
    pub fn busy(&self) -> bool {
        matches!(self.state, State::Queued | State::Running)
    }
}

impl Chat {
    pub fn new(id: String, settings: Value, agent: Option<String>) -> Chat {
        let ms = now_ms();
        Chat {
            v: 1,
            id,
            title: String::new(),
            created_ms: ms,
            updated_ms: ms,
            rev: 0,
            agent,
            settings,
            settings_rev: 0,
            messages: Vec::new(),
            changes: Vec::new(),
            actions: Vec::new(),
            session: Session::default(),
            favorite: false,
        }
    }

    pub fn next_n(&self) -> u32 {
        self.messages.iter().map(Msg::n).max().map_or(0, |n| n + 1)
    }

    pub fn next_change_id(&self) -> u64 {
        self.changes.iter().map(|c| c.id).max().map_or(1, |n| n + 1)
    }

    pub fn next_action_id(&self) -> u64 {
        self.actions.iter().map(|a| a.id).max().map_or(1, |n| n + 1)
    }

    pub fn note(&mut self, text: impl Into<String>) {
        let n = self.next_n();
        self.messages.push(Msg::Note { n, ms: now_ms(), text: text.into() });
    }

    /// A title from the first thing the person said, if there is none yet.
    pub fn name_from(&mut self, text: &str) {
        if self.title.is_empty() {
            let t: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
            self.title = t.chars().take(60).collect();
        }
    }

    /// The newest picture the chat shows, for its tile.
    pub fn newest_image(&self) -> Option<&ImageRef> {
        self.messages.iter().rev().find_map(|m| match m {
            Msg::Batch { images, .. } | Msg::User { images, .. } => images.last(),
            _ => None,
        })
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A chat's id: `{ms}-{counter}`, so two in the same millisecond differ.
pub fn new_id() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    format!("{}-{}", now_ms(), N.fetch_add(1, Ordering::Relaxed))
}

/// A name of a file a chat holds: `[a-z0-9-]+.png`, nothing else — the
/// whitelist the routes that serve them use.
pub fn valid_file(name: &str) -> bool {
    name.strip_suffix(".png").is_some_and(|stem| {
        !stem.is_empty() && stem.len() <= 64 && stem.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    })
}

pub struct ChatStore {
    root: PathBuf,
    favorites: PathBuf,
}

impl ChatStore {
    pub fn new(work_dir: &Path, user_dir: &Path) -> ChatStore {
        let s = ChatStore { root: work_dir.join("chats"), favorites: user_dir.join("favorite_chats") };
        std::fs::create_dir_all(&s.root).ok();
        std::fs::create_dir_all(&s.favorites).ok();
        s
    }

    fn shelf_dir(&self, shelf: Shelf) -> &Path {
        match shelf {
            Shelf::Work => &self.root,
            Shelf::Favorites => &self.favorites,
        }
    }

    /// Where chat `id` is: starred, or in `work/chats/` (also where a new one goes).
    pub fn dir(&self, id: &str) -> PathBuf {
        let fav = self.favorites.join(id);
        if fav.is_dir() {
            fav
        } else {
            self.root.join(id)
        }
    }

    /// A picture the chat holds: `img/` for its own copies, `att/` for uploads.
    pub fn file(&self, id: &str, folder: &str, name: &str) -> Option<PathBuf> {
        (valid_id(id) && valid_file(name) && matches!(folder, "img" | "att"))
            .then(|| self.dir(id).join(folder).join(name))
    }

    pub fn thumb_path(&self, id: &str) -> PathBuf {
        self.dir(id).join("thumb.png")
    }

    pub fn load(&self, id: &str) -> Result<Chat> {
        if !valid_id(id) {
            bail!("'{id}' is not a chat id");
        }
        let path = self.dir(id).join("chat.json");
        let bytes = std::fs::read(&path).with_context(|| format!("that chat is gone ({})", path.display()))?;
        let mut chat: Chat = serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))?;
        chat.favorite = path.starts_with(&self.favorites);
        Ok(chat)
    }

    /// Write a new chat. Refuses an id already taken.
    pub fn create(&self, chat: &Chat) -> Result<()> {
        if !valid_id(&chat.id) {
            bail!("'{}' is not a chat id", chat.id);
        }
        let _w = WRITES.lock().unwrap_or_else(|p| p.into_inner());
        let dir = self.dir(&chat.id);
        if dir.exists() {
            bail!("a chat called {} already exists", chat.id);
        }
        std::fs::create_dir_all(dir.join("img"))?;
        std::fs::create_dir_all(dir.join("att"))?;
        self.write(chat)
    }

    /// Change a chat: read it, hand it to `f`, write it back with `rev`
    /// moved. What `f` returns is passed on.
    pub fn update<T>(&self, id: &str, f: impl FnOnce(&mut Chat) -> Result<T>) -> Result<T> {
        let _w = WRITES.lock().unwrap_or_else(|p| p.into_inner());
        let mut chat = self.load(id)?;
        let out = f(&mut chat)?;
        chat.rev += 1;
        chat.updated_ms = now_ms();
        self.write(&chat)?;
        Ok(out)
    }

    fn write(&self, chat: &Chat) -> Result<()> {
        let dir = self.dir(&chat.id);
        let path = dir.join("chat.json");
        let tmp = dir.join("chat.json.part");
        std::fs::write(&tmp, serde_json::to_vec_pretty(chat)?).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))
    }

    /// The chats on one shelf, newest change first. One that can't be read is
    /// skipped, not fatal to the grid.
    pub fn list(&self, shelf: Shelf) -> Vec<Chat> {
        let Ok(entries) = std::fs::read_dir(self.shelf_dir(shelf)) else {
            return Vec::new();
        };
        let mut out: Vec<Chat> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .filter_map(|e| self.load(e.file_name().to_str()?).ok())
            .collect();
        out.sort_by(|a, b| b.updated_ms.cmp(&a.updated_ms));
        out
    }

    /// Current is both shelves (a starred chat flagged `favorite`); Favorites
    /// its own alone — `VideoStore::listing`'s rule.
    pub fn listing(&self, shelf: Shelf) -> Vec<Chat> {
        if shelf == Shelf::Favorites {
            return self.list(Shelf::Favorites);
        }
        let mut out = self.list(Shelf::Favorites);
        let starred: std::collections::HashSet<String> = out.iter().map(|c| c.id.clone()).collect();
        out.extend(self.list(Shelf::Work).into_iter().filter(|c| !starred.contains(&c.id)));
        out.sort_by(|a, b| b.updated_ms.cmp(&a.updated_ms));
        out
    }

    /// Remove a chat from one shelf. Missing is not an error.
    pub fn delete(&self, shelf: Shelf, id: &str) -> Result<()> {
        if !valid_id(id) {
            bail!("'{id}' is not a chat id");
        }
        let _w = WRITES.lock().unwrap_or_else(|p| p.into_inner());
        let dir = self.shelf_dir(shelf).join(id);
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("deleting {}", dir.display())),
        }
    }

    pub fn favorite(&self, id: &str) -> Result<()> {
        self.shelve(id, Shelf::Work, Shelf::Favorites)
    }

    pub fn unfavorite(&self, id: &str) -> Result<()> {
        self.shelve(id, Shelf::Favorites, Shelf::Work)
    }

    fn shelve(&self, id: &str, from: Shelf, to: Shelf) -> Result<()> {
        if !valid_id(id) {
            bail!("'{id}' is not a chat id");
        }
        let _w = WRITES.lock().unwrap_or_else(|p| p.into_inner());
        let (src, dest) = (self.shelf_dir(from).join(id), self.shelf_dir(to).join(id));
        if !src.is_dir() {
            if dest.is_dir() {
                return Ok(());
            }
            bail!("that chat is gone ({})", src.display());
        }
        if dest.exists() {
            bail!("{} is already there", dest.display());
        }
        if std::fs::rename(&src, &dest).is_ok() {
            return Ok(());
        }
        crate::videos::copy_tree(&src, &dest)
            .with_context(|| format!("copying {} to {}", src.display(), dest.display()))?;
        std::fs::remove_dir_all(&src).with_context(|| format!("removing {}", src.display()))
    }

    /// Settle chats a stopped server left mid-turn: a session `queued` or
    /// `running` with no job behind it is `interrupted`. Called on start,
    /// when no job can be carrying one.
    pub fn settle_all(&self) {
        for shelf in [Shelf::Work, Shelf::Favorites] {
            for c in self.list(shelf) {
                if c.session.busy() {
                    let _ = self.update(&c.id, |c| {
                        c.session.state = State::Interrupted;
                        c.session.job = None;
                        c.note("The server stopped while the agent was working. Send again to go on.");
                        Ok(())
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(name: &str) -> (ChatStore, PathBuf) {
        let d = std::env::temp_dir().join(format!("yasdig-chats-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        (ChatStore::new(&d.join("work"), &d.join("user")), d)
    }

    fn chat(id: &str) -> Chat {
        Chat::new(id.into(), serde_json::json!({"prompt": "a cat", "template": "sdxl"}), None)
    }

    #[test]
    fn a_chat_round_trips_and_every_write_moves_its_rev() {
        let (s, _d) = store("rt");
        s.create(&chat("1-0")).unwrap();
        s.update("1-0", |c| {
            c.name_from("  a red   hat on a fox ");
            c.note("hello");
            Ok(())
        })
        .unwrap();
        let c = s.load("1-0").unwrap();
        assert_eq!(c.title, "a red hat on a fox");
        assert_eq!(c.rev, 1);
        assert!(matches!(&c.messages[0], Msg::Note { text, .. } if text == "hello"));
        assert!(s.create(&chat("1-0")).is_err(), "an id is never written over");
    }

    #[test]
    fn a_favourite_moves_and_a_delete_reaches_one_shelf() {
        let (s, _d) = store("fav");
        s.create(&chat("2-0")).unwrap();
        s.create(&chat("3-0")).unwrap();
        s.favorite("2-0").unwrap();
        assert!(s.load("2-0").unwrap().favorite);
        assert_eq!(s.listing(Shelf::Work).len(), 2, "Current lists both");
        assert_eq!(s.list(Shelf::Work).len(), 1, "Delete all sees only its own");
        s.delete(Shelf::Work, "2-0").unwrap();
        assert!(s.load("2-0").is_ok(), "the favourite survives a Current delete");
        s.unfavorite("2-0").unwrap();
        assert!(!s.load("2-0").unwrap().favorite);
    }

    #[test]
    fn a_chat_left_mid_turn_is_interrupted_on_start() {
        let (s, _d) = store("settle");
        let mut c = chat("4-0");
        c.session.state = State::Running;
        c.session.job = Some(7);
        s.create(&c).unwrap();
        s.settle_all();
        let c = s.load("4-0").unwrap();
        assert_eq!(c.session.state, State::Interrupted);
        assert_eq!(c.session.job, None);
    }

    /// A chat written while ask-first existed: its own permissions, a
    /// proposal and a pass waiting, the session paused on them.
    #[test]
    fn a_chat_from_before_the_whitelist_still_reads() {
        let old = serde_json::json!({
            "id": "5-0", "settings": {"prompt": "a cat"},
            "permissions": {"batch": 4, "rounds": 3, "knobs": {"prompt": {"perm": "ask"}}},
            "changes": [{"id": 1, "by": "agent", "state": "pending", "set": {"prompt": "a dog"}}],
            "actions": [{"id": 1, "kind": "hires", "item": "a", "state": "pending"}],
            "session": {"state": "awaiting"},
        });
        let c: Chat = serde_json::from_value(old).unwrap();
        assert_eq!(c.changes[0].state, ChangeState::Rejected);
        assert_eq!(c.actions[0].state, ActionState::Rejected);
        assert_eq!(c.session.state, State::Idle);
    }

    #[test]
    fn only_a_plain_png_name_is_a_chat_file() {
        assert!(valid_file("b2-0.png"));
        for bad in ["../x.png", "a.PNG", "x.json", ".png", "a/b.png", "A.png"] {
            assert!(!valid_file(bad), "{bad}");
        }
        let (s, _d) = store("file");
        assert!(s.file("1-0", "img", "a.png").is_some());
        assert!(s.file("1-0", "..", "a.png").is_none());
        assert!(s.file("../1", "img", "a.png").is_none());
    }
}
