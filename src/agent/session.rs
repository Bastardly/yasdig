//! The agent's bounded workflow, as pure functions over a [`Chat`]: what the
//! next step is, what a reply does to the chat.
//! The worker (`agent::step`) only carries them out — a turn, an image, a
//! pass — so the decisions are tested here without a GPU.
//!
//! A session runs **plan → round → critique → round → … → done**, at most
//! `rounds` rounds, and a plain chat turn may start the same loop by asking to
//! generate. Each step is one job-queue step, so a button press on the Image
//! tab runs between any two of them.
//!
//! **The agent sizes its own work** from what the person wrote — pictures in
//! a round, rounds in a session — under the caps in code
//! ([`knobs::MAX_BATCH`], [`knobs::MAX_ROUNDS`]). A chat round is one picture
//! unless asked; a session the plan didn't size is [`SESSION_ROUNDS`] of
//! [`SESSION_BATCH`].

use super::chat::{ActionRec, ActionState, Change, ChangeState, Chat, CheckRow, Mode, Msg, Next, State, Sweep};
use super::knobs::{self, Locks, MAX_BATCH, MAX_ROUNDS};
use super::toolkit::Toolkit;
use serde::Deserialize;
use serde_json::{Map, Value};

/// One step of a leg.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Turn(Mode),
    /// Picture `k` (0-based) of round `round`.
    Image { round: u32, k: u32 },
    Escalate(u64),
    End,
}

pub fn next_step(chat: &Chat) -> Step {
    let s = &chat.session;
    match s.next {
        Next::Nothing => Step::End,
        Next::Turn => Step::Turn(s.mode),
        Next::Critique => Step::Turn(Mode::Critique),
        Next::Batch => {
            let drawn = current_batch(chat).map_or(0, |(_, n, _)| n);
            Step::Image { round: s.round, k: drawn }
        }
        Next::Escalate => chat
            .actions
            .iter()
            .find(|a| a.state == ActionState::Queued)
            .map_or(Step::End, |a| Step::Escalate(a.id)),
    }
}

/// The round being drawn: its number, how many of its pictures exist, and
/// how many it will hold.
fn current_batch(chat: &Chat) -> Option<(u32, u32, u32)> {
    chat.messages.iter().rev().find_map(|m| match m {
        Msg::Batch { round, images, size, .. } if *round == chat.session.round => {
            Some((*round, images.len() as u32, whole(*size)))
        }
        _ => None,
    })
}

/// A session's rounds and their size when its plan didn't say.
pub const SESSION_ROUNDS: u32 = 3;
pub const SESSION_BATCH: u32 = 4;

/// A round's size as recorded: 0 in chats from before a round was sized,
/// which drew four.
fn whole(size: u32) -> u32 {
    if size == 0 { SESSION_BATCH } else { size }
}

/// Pictures per round of the session, and its rounds.
fn per_round(chat: &Chat) -> u32 {
    if chat.session.per_round == 0 { SESSION_BATCH } else { chat.session.per_round.min(MAX_BATCH) }
}

pub fn session_limit(chat: &Chat) -> u32 {
    if chat.session.rounds == 0 { SESSION_ROUNDS } else { chat.session.rounds.min(MAX_ROUNDS) }
}

/// How many pictures the current round holds when whole.
pub fn round_size(chat: &Chat) -> u32 {
    current_batch(chat).map_or(per_round(chat), |(_, _, size)| size)
}

/// The newest round, when it is whole and nothing was drawn after it: what a
/// new session judges first instead of drawing again. Its number and size.
pub fn judgeable(chat: &Chat) -> Option<(u32, u32)> {
    let (round, n, size) = current_batch(chat)?;
    (round > 0 && n >= size).then_some((round, size))
}

/// The seed for picture `k` of the round. Every round's seeds are drawn once
/// when it begins (`start_round`) and recorded, so a round can be drawn again.
pub fn seed_for(chat: &Chat, k: u32) -> i64 {
    chat.session.base_seed + k as i64
}

/// How many of its rounds the session has drawn.
pub fn session_rounds(chat: &Chat) -> u32 {
    chat.session.round.saturating_sub(chat.session.first_round)
}

/// Begin the next round: a `Batch` message with no pictures yet. Its seeds are
/// the last round's unless that round's are asked to be new (`fresh`) or there
/// was none — a session with the seed locked keeps them, so a difference
/// between rounds is the change; a chat turn says which it wants.
pub fn start_round(chat: &mut Chat, random_seed: i64) {
    if chat.session.base_seed == 0 || chat.session.fresh {
        chat.session.base_seed = random_seed.max(1);
    }
    let size = match chat.session.count {
        0 => per_round(chat),
        c => c.min(MAX_BATCH),
    };
    chat.session.count = 0;
    chat.session.fresh = false;
    // A comparison's round draws its own value, written before the round
    // records its settings. Only while the session runs: a chat round after
    // it draws what the chat holds.
    // Its value leads the round's line, which otherwise starts with a prompt.
    let mut drew = None;
    if chat.session.mode != Mode::Chat {
        let at = session_rounds(chat) as usize;
        if let Some((k, v)) = chat.session.sweep.as_ref().and_then(|s| Some((knobs::knob(&s.knob)?, s.values.get(at)?.clone()))) {
            knobs::write(&mut chat.settings, k, &v);
            chat.settings_rev += 1;
            drew = Some(knobs::value_label(k.key, &v));
        }
    }
    chat.session.round += 1;
    let n = chat.next_n();
    chat.messages.push(Msg::Batch {
        n,
        ms: super::chat::now_ms(),
        round: chat.session.round,
        settings_rev: chat.settings_rev,
        summary: match drew {
            Some(d) => format!("{d} · {}", summary(&chat.settings)),
            None => summary(&chat.settings),
        },
        size,
        images: Vec::new(),
    });
    chat.session.next = Next::Batch;
}

/// Whether the leg's next step is a picture of a round that hasn't begun —
/// none drawn yet, or the last one full — so the worker must begin it first.
pub fn needs_round(chat: &Chat) -> bool {
    chat.session.next == Next::Batch && current_batch(chat).is_none_or(|(_, n, size)| n >= size)
}

/// A round's settings in a line, for later turns that don't see its pictures.
pub fn summary(settings: &Value) -> String {
    let get = |k: &str| settings.get(k).filter(|v| !v.is_null());
    let mut parts = Vec::new();
    if let Some(p) = get("prompt").and_then(|v| v.as_str()) {
        let short: String = p.chars().take(160).collect();
        parts.push(format!("prompt \"{short}{}\"", if short.len() < p.len() { "…" } else { "" }));
    }
    if let Some(n) = get("negative").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        parts.push(format!("negative \"{}\"", n.chars().take(80).collect::<String>()));
    }
    for (k, label) in [("cfg_scale", "CFG"), ("steps", "steps")] {
        if let Some(v) = get(k) {
            parts.push(format!("{label} {v}"));
        }
    }
    if let (Some(w), Some(h)) = (get("width"), get("height")) {
        parts.push(format!("{w}×{h}"));
    }
    if let Some(m) = get("template").and_then(|v| v.as_str()) {
        parts.push(format!("model {m}"));
    }
    parts.join(", ")
}

/// The agent's words for what round `round` tries: the reason of a change it
/// made just before, else what it told the person. Empty when it said nothing
/// (a round drawn on the person's own settings).
pub fn round_reason(chat: &Chat, round: u32) -> String {
    let at = chat.messages.iter().position(|m| matches!(m, Msg::Batch { round: r, .. } if *r == round));
    let Some(at) = at else { return String::new() };
    chat.messages[..at]
        .iter()
        .rev()
        .take_while(|m| !matches!(m, Msg::Batch { .. }))
        .find_map(|m| match m {
            Msg::Agent { changes, reply, .. } => {
                let reason = chat
                    .changes
                    .iter()
                    .filter(|c| changes.contains(&c.id) && c.state == ChangeState::Applied)
                    .map(|c| c.reason.trim())
                    .find(|r| !r.is_empty());
                Some(reason.unwrap_or(reply.trim()).to_string())
            }
            _ => None,
        })
        .unwrap_or_default()
}

/// A picture of the round landed.
pub fn image_done(chat: &mut Chat, img: super::chat::ImageRef) {
    let round = chat.session.round;
    // A round a chat turn asked for is shown and left to the person, even
    // with a session's checks still on the chat: the person is steering now.
    let judge = !chat.session.checks.is_empty() && chat.session.mode != Mode::Chat;
    for m in chat.messages.iter_mut().rev() {
        if let Msg::Batch { round: r, images, size, .. } = m {
            if *r == round {
                images.push(img);
                let size = whole(*size);
                if images.len() as u32 >= size {
                    chat.session.next = if judge { Next::Critique } else { Next::Nothing };
                }
                return;
            }
        }
    }
}

/// A reply as the schema shapes it. Every field optional: the server reads
/// what it can and the grammar is not trusted to have delivered all of it.
#[derive(Debug, Default, Deserialize)]
pub struct ReplyJson {
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub reply: String,
    #[serde(default)]
    pub change: Option<Map<String, Value>>,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub action: Option<String>,
    /// Plan: how many rounds the session runs.
    #[serde(default)]
    pub rounds: Option<u32>,
    /// How many pictures to draw (a plan: per round), and whether to keep
    /// the last round's seeds.
    #[serde(default)]
    pub count: Option<u32>,
    #[serde(default)]
    pub seeds: Option<String>,
    /// Plan: the questions. Critique: one row of answers per picture.
    #[serde(default)]
    pub checks: Option<Value>,
    #[serde(default)]
    pub keepers: Vec<u32>,
    #[serde(default)]
    pub escalate: Option<EscalateJson>,
    #[serde(default)]
    pub prompt: Option<String>,
    /// Chat, action "improve" or "compare": what the pictures must show, or
    /// what is being found out.
    #[serde(default)]
    pub goal: Option<String>,
    /// Chat, action "compare": the knob, and its values as written.
    #[serde(default)]
    pub vary: Option<String>,
    #[serde(default)]
    pub values: Option<Vec<Value>>,
    /// Verdict: the compared value that answered best, from 1.
    #[serde(default)]
    pub best: Option<u32>,
    #[serde(default)]
    pub negative: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct EscalateJson {
    pub image: u32,
    pub pass: String,
    #[serde(default)]
    pub instruction: Option<String>,
}

/// Read a reply. The template's parser already split off any thinking, so
/// what is left should be the object; a reply wrapped in a code fence or with
/// words around it is read from its outermost braces.
pub fn parse_reply(text: &str) -> anyhow::Result<ReplyJson> {
    let t = text.trim();
    if let Ok(r) = serde_json::from_str(t) {
        return Ok(r);
    }
    let (a, b) = (t.find('{'), t.rfind('}'));
    match (a, b) {
        (Some(a), Some(b)) if a < b => serde_json::from_str(&t[a..=b])
            .map_err(|e| anyhow::anyhow!("the agent's reply isn't the JSON it was asked for ({e})")),
        _ => anyhow::bail!("the agent's reply isn't the JSON it was asked for"),
    }
}

/// Start a session on `goal`: its own checks (the plan turn writes them),
/// its rounds counted from here. Round numbers go on, so none is reused. The
/// plan decides whether it judges the round just drawn or draws afresh. The
/// pictures attached to the message it answers are what it is judged against.
pub fn begin(chat: &mut Chat, goal: &str) {
    chat.session.goal = goal.to_string();
    chat.session.checks.clear();
    chat.session.first_round = chat.session.round;
    chat.session.rounds = 0;
    chat.session.per_round = 0;
    chat.session.sweep = None;
    chat.session.reference = chat
        .messages
        .iter()
        .rev()
        .find_map(|m| match m {
            Msg::User { images, .. } => Some(images.clone()),
            _ => None,
        })
        .unwrap_or_default();
    chat.session.mode = Mode::Plan;
}

/// A comparison from a "compare" reply: each value read, checked as a change
/// to it would be (whitelist, locks, bounds) and kept once. `None`, with the
/// reasons in `notes`, unless two values are left.
fn sweep_from(chat: &Chat, r: &ReplyJson, t: &Toolkit, locks: &Locks, notes: &mut Vec<String>) -> Option<Sweep> {
    let Some(key) = r.vary.as_deref().filter(|k| knobs::COMPARABLE.contains(k)) else {
        notes.push("The agent wanted to compare but named no setting it can compare.".into());
        return None;
    };
    let k = knobs::knob(key)?;
    let models = t.model_names();
    let mut values = Vec::new();
    for raw in r.values.iter().flatten() {
        let v = knobs::parse_value(key, raw);
        if let Some(v) = knobs::check(key, &v, &t.rules, locks, &models, notes) {
            if !values.contains(&v) {
                values.push(v);
            }
        }
    }
    values.truncate(MAX_ROUNDS as usize);
    if values.len() < 2 {
        notes.push(format!("A comparison needs two values of {} the agent may use; it had {}.", k.label, values.len()));
        return None;
    }
    Some(Sweep { knob: key.to_string(), values, before: knobs::value_of(&chat.settings, k) })
}

/// The critique of round `round`: its answer rows.
fn critique_of(chat: &Chat, round: u32) -> Option<&[CheckRow]> {
    let at = chat.messages.iter().position(|m| matches!(m, Msg::Batch { round: r, .. } if *r == round))?;
    chat.messages[at + 1..].iter().find_map(|m| match m {
        Msg::Agent { mode: Mode::Critique, checks, .. } => Some(checks.as_slice()),
        _ => None,
    })
}

/// A comparison's results, counted by the server from the critiques'
/// answers: the checks, then a numbered line per value. What the verdict
/// turn is shown and what the person reads, whatever the model makes of it.
pub fn sweep_table(chat: &Chat) -> String {
    let Some(sw) = &chat.session.sweep else { return String::new() };
    let label = knobs::knob(&sw.knob).map_or(sw.knob.as_str(), |k| k.label);
    let mut lines = vec![format!("Comparing {label}, the same seeds each time. Checks:")];
    for (i, c) in chat.session.checks.iter().enumerate() {
        lines.push(format!("  {}. {c}", i + 1));
    }
    for (i, v) in sw.values.iter().enumerate() {
        let what = knobs::value_label(&sw.knob, v);
        let Some(rows) = critique_of(chat, chat.session.first_round + 1 + i as u32) else {
            lines.push(format!("{}. {what}: not drawn", i + 1));
            continue;
        };
        let want = chat.session.checks.len();
        let all = rows.iter().filter(|r| r.results.len() == want && r.results.iter().all(|b| *b)).count();
        let per: Vec<String> = (0..want)
            .map(|c| format!("{} {}/{}", c + 1, rows.iter().filter(|r| r.results.get(c) == Some(&true)).count(), rows.len()))
            .collect();
        lines.push(format!("{}. {what}: {all} of {} passed every check ({})", i + 1, rows.len(), per.join(", ")));
    }
    lines.join("\n")
}

/// Record a reply on the chat and decide what comes next.
pub fn apply_reply(chat: &mut Chat, mode: Mode, mut r: ReplyJson, t: &Toolkit, locks: &Locks, skills: Vec<String>, stats: crate::llm::Stats) {
    let n = chat.next_n();
    let mut change_ids = Vec::new();
    let mut notes_out = Vec::new();
    // A verdict puts the knob back as it was before the comparison, then sets
    // the best value as the agent's change, so Undo returns to the start.
    if mode == Mode::Verdict {
        if let Some(sw) = chat.session.sweep.clone() {
            if let Some(k) = knobs::knob(&sw.knob) {
                knobs::write(&mut chat.settings, k, &sw.before);
                chat.settings_rev += 1;
            }
            match r.best.and_then(|b| sw.values.get((b as usize).checked_sub(1)?)) {
                Some(v) => {
                    r.change = Some(Map::from_iter([(sw.knob.clone(), v.clone())]));
                    if r.reason.trim().is_empty() {
                        r.reason = format!("the comparison's best: {}", knobs::value_label(&sw.knob, v));
                    }
                }
                None => notes_out.push("The comparison picked no value, so the setting is back to what it was.".into()),
            }
        }
    }
    // Settings: whatever is on the whitelist, at once.
    if let Some(change) = r.change.as_ref().filter(|c| !c.is_empty()) {
        let applied = knobs::apply(&mut chat.settings, change, &t.rules, locks, &t.model_names(), &|m| t.locks_of(m).cloned());
        notes_out.extend(applied.notes.iter().filter(|n| !n.contains("isn't the agent's to change")).cloned());
        if !applied.set.is_empty() {
            chat.settings_rev += 1;
            let id = chat.next_change_id();
            chat.changes.push(Change {
                id,
                by: "agent".into(),
                state: ChangeState::Applied,
                set: applied.set,
                before: applied.before,
                reason: r.reason.clone(),
                msg: n,
                notes: Vec::new(),
            });
            change_ids.push(id);
        }
    }

    let batch = current_batch(chat).map_or(0, |(_, n, _)| n);
    let mut checks_rows = Vec::new();
    let mut passed = Vec::new();
    let mut keepers = Vec::new();
    let mut action_id = None;
    let mut table = false;
    match mode {
        Mode::Plan if chat.session.sweep.is_some() => {
            chat.session.checks = plan_checks(&r);
            // The server sized it: a round per value, the same seeds.
            chat.session.rounds = chat.session.sweep.as_ref().map_or(0, |s| s.values.len() as u32);
            chat.session.fresh = true;
            chat.session.next = Next::Batch;
        }
        Mode::Plan => {
            chat.session.checks = plan_checks(&r);
            // How long and how wide, as the goal said; unsaid is the default.
            chat.session.rounds = r.rounds.map_or(0, |n| n.clamp(1, MAX_ROUNDS));
            chat.session.per_round = r.count.map_or(0, |n| n.clamp(1, MAX_BATCH));
            // A round just drawn is judged first, unless the plan rewrote the
            // prompt (the change just made): then those pictures answer another goal.
            let rewrote = chat.changes.iter().any(|c| change_ids.contains(&c.id) && c.set.contains_key("prompt"));
            match judgeable(chat) {
                Some((round, _)) if !rewrote => {
                    chat.session.first_round = round - 1;
                    chat.session.next = Next::Critique;
                }
                _ => {
                    chat.session.fresh = true;
                    chat.session.next = Next::Batch;
                }
            }
        }
        Mode::Critique => {
            let rows: Vec<Vec<bool>> = r
                .checks
                .as_ref()
                .and_then(|c| serde_json::from_value(c.clone()).ok())
                .unwrap_or_default();
            let want = chat.session.checks.len().max(1);
            for (i, row) in rows.into_iter().take(batch as usize).enumerate() {
                let row: Vec<bool> = row.into_iter().take(want).collect();
                // The server's own ranking: every check yes. Kept beside the
                // model's keepers, since a small model flatters what it drew.
                if row.len() == want && row.iter().all(|b| *b) {
                    passed.push(i as u32 + 1);
                }
                checks_rows.push(CheckRow { image: i as u32 + 1, results: row });
            }
            keepers = r.keepers.iter().copied().filter(|k| (1..=batch).contains(k)).collect();
            keepers.dedup();
            let action = r.action.as_deref().unwrap_or("done");
            chat.session.next = match action {
                // A comparison's rounds are the server's: the next value, the
                // same seeds; after the last, the counts and a verdict.
                _ if chat.session.sweep.is_some() && session_rounds(chat) < session_limit(chat) => Next::Batch,
                _ if chat.session.sweep.is_some() => {
                    table = true;
                    chat.session.mode = Mode::Verdict;
                    Next::Turn
                }
                "generate" if session_rounds(chat) < session_limit(chat) => {
                    // The same seeds unless it asks for new ones, so a difference
                    // between rounds is the change.
                    chat.session.fresh = r.seeds.as_deref() == Some("new");
                    chat.session.count = r.count.map_or(0, |n| n.clamp(1, MAX_BATCH));
                    Next::Batch
                }
                "generate" => {
                    notes_out.push(format!("That was round {} of {}, so the session stops here.", session_rounds(chat), session_limit(chat)));
                    Next::Nothing
                }
                "escalate" => match escalation(chat, r.escalate.as_ref(), t, n) {
                    Ok(id) => {
                        action_id = Some(id);
                        Next::Escalate
                    }
                    Err(why) => {
                        notes_out.push(why);
                        Next::Nothing
                    }
                },
                _ => Next::Nothing,
            };
        }
        Mode::Chat => {
            chat.session.next = match r.action.as_deref() {
                // Each older name too, for a reply from before the rename.
                Some("draw" | "generate") => {
                    // One picture unless the person asked for more.
                    chat.session.count = r.count.unwrap_or(1).clamp(1, MAX_BATCH);
                    // New seeds unless asked to keep the composition.
                    chat.session.fresh = r.seeds.as_deref() != Some("same");
                    Next::Batch
                }
                // The leg goes on to the plan turn, which writes the checks
                // and sizes the session.
                Some("improve" | "session") => match r.goal.as_deref().map(str::trim).filter(|g| !g.is_empty()) {
                    Some(goal) => {
                        begin(chat, goal);
                        Next::Turn
                    }
                    None => {
                        notes_out.push("The agent wanted to run a session but gave it no goal.".into());
                        Next::Nothing
                    }
                },
                Some("compare") => {
                    let goal = r.goal.as_deref().map(str::trim).filter(|g| !g.is_empty()).unwrap_or("a comparison").to_string();
                    // Its change is in already: a switch of model brings that model's locks.
                    let model = chat.settings.get("template").and_then(|v| v.as_str()).unwrap_or("");
                    let locks = t.locks_of(model).filter(|_| change_ids.iter().any(|id| {
                        chat.changes.iter().any(|c| c.id == *id && c.set.contains_key("model"))
                    })).unwrap_or(locks);
                    match sweep_from(chat, &r, t, locks, &mut notes_out) {
                        Some(sw) => {
                            begin(chat, &goal);
                            chat.session.per_round = r.count.unwrap_or(2).clamp(1, MAX_BATCH);
                            chat.session.rounds = sw.values.len() as u32;
                            chat.session.sweep = Some(sw);
                            Next::Turn
                        }
                        None => Next::Nothing,
                    }
                }
                // "describe" writes its prompt on the message, below.
                _ => Next::Nothing,
            };
        }
        Mode::Describe | Mode::Verdict => chat.session.next = Next::Nothing,
    }

    chat.messages.push(Msg::Agent {
        n,
        ms: super::chat::now_ms(),
        mode,
        reply: r.reply.trim().to_string(),
        notes: r.notes.trim().to_string(),
        checks: checks_rows,
        keepers,
        passed,
        changes: change_ids,
        action: action_id,
        prompt: r.prompt.map(|p| p.trim().to_string()).filter(|p| !p.is_empty()),
        negative: r.negative.map(|p| p.trim().to_string()).filter(|p| !p.is_empty()),
        skills,
        stats,
    });
    for note in notes_out {
        chat.note(note);
    }
    if table {
        let t = sweep_table(chat);
        chat.note(t);
    }
    // A new round is begun by the worker (it draws the seed), so a reply only
    // says "a round next"; the round itself starts when the leg goes on.
}

/// A plan's checks, trimmed, the empty ones dropped.
fn plan_checks(r: &ReplyJson) -> Vec<String> {
    r.checks
        .as_ref()
        .and_then(|c| c.as_array())
        .map(|a| a.iter().filter_map(|q| q.as_str().map(|s| s.trim().to_string())).filter(|s| !s.is_empty()).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .take(super::schema::MAX_CHECKS as usize)
        .collect()
}

/// The pass a critique asked for, queued.
fn escalation(chat: &mut Chat, e: Option<&EscalateJson>, t: &Toolkit, msg: u32) -> Result<u64, String> {
    let e = e.ok_or("the agent asked for a pass without saying which picture")?;
    let a = t
        .actions
        .iter()
        .find(|a| a.key == e.pass)
        .filter(|a| a.offered())
        .ok_or_else(|| format!("the agent asked for a {} pass, which it may not run", e.pass))?;
    let item = chat
        .messages
        .iter()
        .rev()
        .find_map(|m| match m {
            Msg::Batch { round, images, .. } if *round == chat.session.round => images.get(e.image.checked_sub(1)? as usize).cloned(),
            _ => None,
        })
        .ok_or_else(|| format!("there is no picture {} in this round", e.image))?;
    let instruction = e.instruction.as_ref().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    if a.key == "edit" && instruction.is_none() {
        return Err("the agent asked for an Edit without saying what to change".into());
    }
    let id = chat.next_action_id();
    chat.actions.push(ActionRec {
        id,
        kind: a.key.to_string(),
        item: item.id,
        instruction,
        state: ActionState::Queued,
        result: None,
        msg,
        error: None,
    });
    Ok(id)
}

/// How a leg ends when nothing is next: a session is done, a chat is idle.
pub fn finished(chat: &Chat) -> State {
    if chat.session.goal.is_empty() { State::Idle } else { State::Done }
}

/// Undo one applied change: put each key back, provided it still holds what
/// the change set. Recorded as a change of the person's, so it can be undone
/// in turn.
pub fn undo(chat: &mut Chat, id: u64) -> Result<u64, String> {
    let c = chat
        .changes
        .iter()
        .find(|c| c.id == id && c.state == ChangeState::Applied)
        .cloned()
        .ok_or("that change isn't applied")?;
    for (key, after) in &c.set {
        let k = knobs::knob(key).ok_or("an unknown setting")?;
        if &knobs::value_of(&chat.settings, k) != after {
            return Err(format!("{} has changed since, so it was left as it is", k.label));
        }
    }
    for (key, before) in &c.before {
        if let Some(k) = knobs::knob(key) {
            knobs::write(&mut chat.settings, k, before);
        }
    }
    if let Some(orig) = chat.changes.iter_mut().find(|x| x.id == id) {
        orig.state = ChangeState::Undone;
    }
    chat.settings_rev += 1;
    let new = chat.next_change_id();
    chat.changes.push(Change {
        id: new,
        by: "user".into(),
        state: ChangeState::Applied,
        set: c.before.clone(),
        before: c.set.clone(),
        reason: format!("undid change {id}"),
        msg: chat.next_n().saturating_sub(1),
        notes: Vec::new(),
    });
    Ok(new)
}

#[cfg(test)]
mod tests {
    use super::super::chat::ImageRef;
    use super::super::knobs::Rules;
    use super::super::toolkit::{build, ModelChoice};
    use super::*;
    use crate::config::ImageFamily;
    use serde_json::json;

    fn chat() -> Chat {
        let mut c = Chat::new("1-0".into(), json!({"prompt": "a fox", "cfg_scale": 6.0}), None);
        c.session.per_round = 2;
        c.session.rounds = 2;
        c.session.goal = "a fox in a red hat".into();
        c.session.next = Next::Turn;
        c.session.mode = Mode::Plan;
        c
    }

    fn tk(c: &Chat) -> Toolkit {
        tk_under(c, &Rules::default())
    }

    fn tk_under(c: &Chat, rules: &Rules) -> Toolkit {
        build(rules, &c.settings, &Locks::default(), vec![], Some(ImageFamily::Sdxl))
    }

    fn img(id: &str) -> ImageRef {
        ImageRef { kind: "item".into(), id: id.into(), file: format!("{id}.png"), seed: Some(1) }
    }

    fn reply(v: Value) -> ReplyJson {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn a_session_runs_plan_rounds_and_critiques_then_stops_at_its_limit() {
        let mut c = chat();
        assert_eq!(next_step(&c), Step::Turn(Mode::Plan));
        let t = tk(&c);
        apply_reply(&mut c, Mode::Plan, reply(json!({"notes": "", "checks": ["Is there a hat?", "Is it red?"], "rounds": 2, "count": 2, "reply": "ok"})), &t, &Locks::default(), vec![], Default::default());
        assert_eq!(c.session.checks.len(), 2);
        assert_eq!(c.session.next, Next::Batch);
        start_round(&mut c, 100);
        assert_eq!(next_step(&c), Step::Image { round: 1, k: 0 });
        assert_eq!(seed_for(&c, 1), 101, "the seed is locked by default");
        image_done(&mut c, img("a"));
        assert_eq!(next_step(&c), Step::Image { round: 1, k: 1 });
        image_done(&mut c, img("b"));
        assert_eq!(next_step(&c), Step::Turn(Mode::Critique));
        let r = reply(json!({"notes": "", "checks": [[true, false], [true, true]], "keepers": [2, 9], "action": "generate", "reply": "",
                             "change": {"prompt": "a fox in a bright red hat"}}));
        apply_reply(&mut c, Mode::Critique, r, &t, &Locks::default(), vec![], Default::default());
        let Some(Msg::Agent { passed, keepers, .. }) = c.messages.last().cloned() else { panic!() };
        assert_eq!(passed, [2]);
        assert_eq!(keepers, [2], "a keeper outside the batch is dropped");
        assert_eq!(c.settings["prompt"], json!("a fox in a bright red hat"));
        assert_eq!(c.session.next, Next::Batch);
        start_round(&mut c, 999);
        assert_eq!(seed_for(&c, 0), 100, "the same seeds every round");
        image_done(&mut c, img("c"));
        image_done(&mut c, img("d"));
        let r = reply(json!({"notes": "", "checks": [[true, true], [true, true]], "keepers": [1], "action": "generate", "reply": ""}));
        apply_reply(&mut c, Mode::Critique, r, &t, &Locks::default(), vec![], Default::default());
        assert_eq!(c.session.next, Next::Nothing, "round 2 of 2 was the last");
        assert_eq!(next_step(&c), Step::End);
    }

    #[test]
    fn a_chat_draws_as_many_as_asked_and_keeps_seeds_only_when_asked() {
        let mut c = chat();
        c.session.goal.clear();
        c.session.mode = Mode::Chat;
        let t = tk(&c);
        let draw = |count: u32, seeds: &str| reply(json!({"notes": "", "action": "generate", "count": count, "seeds": seeds,
                                                       "change": {"prompt": "a cat"}, "reason": "", "reply": "a cat"}));
        apply_reply(&mut c, Mode::Chat, draw(1, "new"), &t, &Locks::default(), vec![], Default::default());
        start_round(&mut c, 100);
        assert_eq!(round_size(&c), 1, "one asked for is one drawn");
        image_done(&mut c, img("a"));
        assert_eq!(next_step(&c), Step::End);
        apply_reply(&mut c, Mode::Chat, draw(99, "same"), &t, &Locks::default(), vec![], Default::default());
        start_round(&mut c, 200);
        assert_eq!((round_size(&c), seed_for(&c, 0)), (MAX_BATCH, 100), "clamped to the cap; the same seeds");
        for k in ["b", "c", "d", "e", "e2", "e3", "e4", "e5"] {
            image_done(&mut c, img(k));
        }
        apply_reply(&mut c, Mode::Chat, draw(2, "new"), &t, &Locks::default(), vec![], Default::default());
        start_round(&mut c, 300);
        assert_eq!((c.session.round, seed_for(&c, 0)), (3, 300), "rounds count on; new seeds");
        let unsaid = reply(json!({"notes": "", "action": "generate", "change": {"prompt": "a cat"}, "reply": ""}));
        for k in ["f", "g"] {
            image_done(&mut c, img(k));
        }
        apply_reply(&mut c, Mode::Chat, unsaid, &t, &Locks::default(), vec![], Default::default());
        start_round(&mut c, 400);
        assert_eq!(round_size(&c), 1, "one picture when no number was said");
    }

    #[test]
    fn a_chat_reply_starts_a_session_on_its_own_goal_and_the_plan_turn_follows() {
        let mut c = chat();
        c.session.goal.clear();
        c.session.checks = vec!["an old check".into()];
        c.session.mode = Mode::Chat;
        let t = tk(&c);
        let start = |goal: &str| reply(json!({"notes": "", "action": "session", "goal": goal, "reply": "On it."}));
        apply_reply(&mut c, Mode::Chat, start("  "), &t, &Locks::default(), vec![], Default::default());
        assert_eq!(next_step(&c), Step::End, "no goal, no session");
        assert!(matches!(c.messages.last(), Some(Msg::Note { .. })));
        apply_reply(&mut c, Mode::Chat, start("A fox wearing a red hat"), &t, &Locks::default(), vec![], Default::default());
        assert_eq!(c.session.goal, "A fox wearing a red hat");
        assert!(c.session.checks.is_empty(), "the plan writes its own");
        assert_eq!(next_step(&c), Step::Turn(Mode::Plan));
    }

    #[test]
    fn a_comparison_draws_a_round_per_value_on_the_same_seeds_then_counts_and_picks() {
        let mut c = chat();
        c.session.goal.clear();
        c.session.mode = Mode::Chat;
        let t = tk(&c);
        let go = |c: &mut Chat, mode: Mode, v: Value| apply_reply(c, mode, reply(v), &t, &Locks::default(), vec![], Default::default());
        go(&mut c, Mode::Chat, json!({"notes": "a comparison", "action": "compare", "goal": "Which CFG keeps the hat red?",
                                      "vary": "cfg_scale", "values": ["4", "9", "4", "x"], "count": 2, "reply": "Let's see."}));
        let sw = c.session.sweep.clone().expect("a comparison");
        assert_eq!(sw.values, [json!(4.0), json!(9.0)], "read as numbers, once each; the unreadable dropped");
        assert_eq!(sw.before, json!(6.0));
        assert_eq!(next_step(&c), Step::Turn(Mode::Plan));
        go(&mut c, Mode::Plan, json!({"notes": "", "checks": ["Is the hat red?", "Is it a fox?"], "reply": ""}));
        assert_eq!((c.session.rounds, round_size(&c)), (2, 2));
        for (i, (seed, answers)) in [(100, json!([[true, true], [true, true]])), (555, json!([[false, true], [true, true]]))].into_iter().enumerate() {
            assert!(needs_round(&c));
            start_round(&mut c, seed);
            assert_eq!(c.settings["cfg_scale"], sw.values[i], "round {i} draws its value");
            assert_eq!(seed_for(&c, 0), 100, "the same seeds every round");
            let Some(Msg::Batch { summary, .. }) = c.messages.last() else { panic!("a round") };
            assert!(summary.starts_with(&format!("CFG scale {} · ", [4, 9][i])), "{summary}");
            image_done(&mut c, img("a"));
            image_done(&mut c, img("b"));
            assert_eq!(next_step(&c), Step::Turn(Mode::Critique));
            go(&mut c, Mode::Critique, json!({"notes": "", "checks": answers, "keepers": [], "reply": ""}));
        }
        assert_eq!(next_step(&c), Step::Turn(Mode::Verdict));
        let Some(Msg::Note { text, .. }) = c.messages.last().cloned() else { panic!("the counts are a note") };
        assert!(text.contains("1. CFG scale 4: 2 of 2 passed every check (1 2/2, 2 2/2)"), "{text}");
        assert!(text.contains("2. CFG scale 9: 1 of 2 passed every check (1 1/2, 2 2/2)"), "{text}");
        go(&mut c, Mode::Verdict, json!({"notes": "", "best": 1, "reply": "CFG 4: every picture passed."}));
        assert_eq!(c.settings["cfg_scale"], json!(4.0));
        assert_eq!(next_step(&c), Step::End);
        let change = c.changes.last().unwrap().id;
        undo(&mut c, change).unwrap();
        assert_eq!(c.settings["cfg_scale"], json!(6.0), "Undo returns to before the comparison");
    }

    #[test]
    fn a_comparison_of_a_locked_setting_or_one_value_draws_nothing() {
        let mut c = chat();
        c.session.goal.clear();
        c.session.mode = Mode::Chat;
        let t = tk(&c);
        let ask = json!({"notes": "", "action": "compare", "goal": "best CFG", "vary": "cfg_scale", "values": ["3", "7"], "count": 2, "reply": ""});
        apply_reply(&mut c, Mode::Chat, reply(ask), &t, &Locks::of(true, false, None), vec![], Default::default());
        assert_eq!(next_step(&c), Step::End);
        assert!(c.session.sweep.is_none() && c.session.goal.is_empty());
        let notes: Vec<String> = c.messages.iter().filter_map(|m| match m { Msg::Note { text, .. } => Some(text.clone()), _ => None }).collect();
        assert!(notes.iter().any(|n| n.contains("locked")) && notes.iter().any(|n| n.contains("needs two values")), "{notes:?}");
    }

    #[test]
    fn a_comparison_may_switch_to_a_model_that_takes_what_this_one_locks() {
        let mut c = chat();
        c.settings = json!({"prompt": "a cat", "template": "klein"});
        c.session.goal.clear();
        c.session.mode = Mode::Chat;
        let mut t = build(&Rules::default(), &c.settings, &Locks::of(true, false, None), vec![
            ModelChoice { name: "klein".into(), family: "flux2-klein-4b", label: "FLUX.2 Klein 4B", locks: Locks::of(true, false, None) },
            ModelChoice { name: "moss".into(), family: "moss_image", label: "MOSS-Image", locks: Locks::default() },
        ], Some(ImageFamily::Sdxl));
        t.locks = Locks::of(true, false, None);
        let ask = |model: &str| reply(json!({"notes": "", "action": "compare", "change": {"model": model}, "reason": "",
            "goal": "best CFG", "vary": "cfg_scale", "values": ["3", "5", "7"], "count": 2, "reply": ""}));
        apply_reply(&mut c, Mode::Chat, ask("moss"), &t, &t.locks.clone(), vec![], Default::default());
        assert_eq!(c.settings["template"], json!("moss"));
        assert_eq!(c.session.sweep.as_ref().map(|s| s.values.len()), Some(3), "{:?}", c.messages);
        assert_eq!(next_step(&c), Step::Turn(Mode::Plan));
        // Staying on a model without CFG, there is nothing to compare.
        let mut c2 = chat();
        c2.settings = json!({"prompt": "a cat", "template": "klein"});
        c2.session.goal.clear();
        c2.session.mode = Mode::Chat;
        apply_reply(&mut c2, Mode::Chat, ask("klein"), &t, &t.locks.clone(), vec![], Default::default());
        assert!(c2.session.sweep.is_none());
    }

    #[test]
    fn a_verdict_that_picks_nothing_puts_the_setting_back() {
        let mut c = chat();
        c.session.sweep = Some(Sweep { knob: "cfg_scale".into(), values: vec![json!(3.0), json!(9.0)], before: json!(6.0) });
        c.settings["cfg_scale"] = json!(9.0);
        let t = tk(&c);
        apply_reply(&mut c, Mode::Verdict, reply(json!({"notes": "", "reply": "Hard to say."})), &t, &Locks::default(), vec![], Default::default());
        assert_eq!(c.settings["cfg_scale"], json!(6.0));
    }

    #[test]
    fn a_session_is_judged_against_the_pictures_its_message_attached() {
        let mut c = chat();
        c.session.goal.clear();
        c.session.mode = Mode::Chat;
        c.messages.push(Msg::User { n: c.next_n(), ms: 0, text: "find the best prompt for this".into(), images: vec![img("ref")] });
        let t = tk(&c);
        apply_reply(&mut c, Mode::Chat, reply(json!({"notes": "", "action": "improve", "goal": "Match the picture", "reply": ""})), &t, &Locks::default(), vec![], Default::default());
        assert_eq!(c.session.reference, [img("ref")]);
        assert_eq!(next_step(&c), Step::Turn(Mode::Plan));
    }

    #[test]
    fn a_chat_reply_describing_a_picture_keeps_the_prompt_and_ends() {
        let mut c = chat();
        c.session.goal.clear();
        c.session.mode = Mode::Chat;
        let t = tk(&c);
        let r = reply(json!({"notes": "", "action": "describe", "prompt": "a red fox ", "negative": "blurry", "reply": "Here it is."}));
        apply_reply(&mut c, Mode::Chat, r, &t, &Locks::default(), vec![], Default::default());
        let Some(Msg::Agent { prompt, negative, .. }) = c.messages.last().cloned() else { panic!() };
        assert_eq!((prompt.as_deref(), negative.as_deref()), (Some("a red fox"), Some("blurry")));
        assert_eq!(next_step(&c), Step::End);
    }

    #[test]
    fn a_round_is_explained_by_the_change_that_led_to_it_else_the_reply() {
        let mut c = chat();
        c.session.goal.clear();
        c.session.mode = Mode::Chat;
        let t = tk(&c);
        let said = |reason: &str, prompt: &str| reply(json!({"notes": "", "action": "generate", "count": 1, "seeds": "new",
            "change": {"prompt": prompt}, "reason": reason, "reply": "Here is a cat."}));
        apply_reply(&mut c, Mode::Chat, said("black fur, as asked", "a black cat"), &t, &Locks::default(), vec![], Default::default());
        start_round(&mut c, 100);
        image_done(&mut c, img("a"));
        assert_eq!(round_reason(&c, 1), "black fur, as asked");
        // The same prompt again is no change, so the reply is the explanation.
        apply_reply(&mut c, Mode::Chat, said("", "a black cat"), &t, &Locks::default(), vec![], Default::default());
        start_round(&mut c, 200);
        assert_eq!(round_reason(&c, 2), "Here is a cat.");
        assert_eq!(round_reason(&c, 9), "", "no such round");
    }

    #[test]
    fn a_session_after_a_round_judges_that_round_first() {
        let mut c = chat();
        c.session.goal.clear();
        c.session.mode = Mode::Chat;
        start_round(&mut c, 100);
        image_done(&mut c, img("a"));
        image_done(&mut c, img("b"));
        // As '/agent/send' begins a session.
        c.session.goal = "pick the best fox".into();
        c.session.mode = Mode::Plan;
        c.session.first_round = c.session.round;
        let t = tk(&c);
        apply_reply(&mut c, Mode::Plan, reply(json!({"notes": "", "checks": ["Is it a fox?"], "change": {}, "reason": "", "reply": ""})), &t, &Locks::default(), vec![], Default::default());
        assert_eq!(next_step(&c), Step::Turn(Mode::Critique), "the round on screen is judged, not redrawn");
        assert_eq!(session_rounds(&c), 1, "and counts as the session's first");
        let r = reply(json!({"notes": "", "checks": [[true], [false]], "keepers": [1], "action": "generate", "change": {}, "reason": "", "reply": ""}));
        apply_reply(&mut c, Mode::Critique, r, &t, &Locks::default(), vec![], Default::default());
        start_round(&mut c, 999);
        assert_eq!((c.session.round, seed_for(&c, 0)), (2, 100), "round 2, the judged round's seeds");

        // A plan that rewrites the prompt draws afresh instead.
        let mut c = chat();
        c.session.mode = Mode::Chat;
        start_round(&mut c, 100);
        image_done(&mut c, img("a"));
        image_done(&mut c, img("b"));
        c.session.mode = Mode::Plan;
        c.session.first_round = c.session.round;
        apply_reply(&mut c, Mode::Plan, reply(json!({"notes": "", "checks": ["Is it a dog?"], "change": {"prompt": "a dog"}, "reason": "", "reply": ""})), &t, &Locks::default(), vec![], Default::default());
        assert_eq!(c.session.next, Next::Batch);
        start_round(&mut c, 500);
        assert_eq!(seed_for(&c, 0), 500);
    }

    #[test]
    fn a_plan_sizes_its_session_inside_the_caps() {
        let mut c = chat();
        c.session.rounds = 0;
        c.session.per_round = 0;
        let t = tk(&c);
        let plan = |rounds: u32, count: u32| reply(json!({"notes": "", "checks": ["Is it a fox?"], "rounds": rounds, "count": count,
                                                        "change": {"prompt": "a fox"}, "reason": "", "reply": ""}));
        apply_reply(&mut c, Mode::Plan, plan(2, 3), &t, &Locks::default(), vec![], Default::default());
        start_round(&mut c, 100);
        assert_eq!((session_limit(&c), round_size(&c)), (2, 3), "two rounds of three, as asked");
        apply_reply(&mut c, Mode::Plan, plan(50, 50), &t, &Locks::default(), vec![], Default::default());
        assert_eq!((session_limit(&c), per_round(&c)), (MAX_ROUNDS, MAX_BATCH), "held to the caps");
        let unsaid = reply(json!({"notes": "", "checks": ["Is it a fox?"], "change": {"prompt": "a fox"}, "reason": "", "reply": ""}));
        apply_reply(&mut c, Mode::Plan, unsaid, &t, &Locks::default(), vec![], Default::default());
        assert_eq!((session_limit(&c), per_round(&c)), (SESSION_ROUNDS, SESSION_BATCH));
    }

    #[test]
    fn an_escalation_needs_its_pass_switched_on() {
        let mut c = chat();
        start_round(&mut c, 1);
        image_done(&mut c, img("a"));
        image_done(&mut c, img("b"));
        let r = || reply(json!({"notes": "", "checks": [[true],[true]], "keepers": [1], "action": "escalate", "escalate": {"image": 1, "pass": "hires"}, "reply": ""}));
        let t = tk(&c);
        apply_reply(&mut c, Mode::Critique, r(), &t, &Locks::default(), vec![], Default::default());
        assert_eq!(c.session.next, Next::Nothing, "hires is off by default");
        assert!(c.actions.is_empty());
        let mut rules = Rules::default();
        rules.actions.insert("hires".into());
        let t = tk_under(&c, &rules);
        apply_reply(&mut c, Mode::Critique, r(), &t, &Locks::default(), vec![], Default::default());
        let id = c.actions[0].id;
        assert_eq!(c.actions[0].item, "a");
        assert_eq!(next_step(&c), Step::Escalate(id));
    }

    #[test]
    fn undo_puts_back_only_what_is_still_as_the_change_left_it() {
        let mut c = chat();
        c.session.mode = Mode::Chat;
        let t = tk(&c);
        apply_reply(&mut c, Mode::Chat, reply(json!({"notes": "", "reply": "", "action": "none", "change": {"cfg_scale": 7.0}})), &t, &Locks::default(), vec![], Default::default());
        let id = c.changes[0].id;
        let undo_id = undo(&mut c, id).unwrap();
        assert_eq!(c.settings["cfg_scale"], json!(6.0));
        // Undoing the undo redoes.
        undo(&mut c, undo_id).unwrap();
        assert_eq!(c.settings["cfg_scale"], json!(7.0));
        // Changed since by hand: refused.
        let last = c.changes.last().unwrap().id;
        c.settings["cfg_scale"] = json!(3.0);
        assert!(undo(&mut c, last).is_err());
    }

    #[test]
    fn a_reply_in_a_code_fence_still_reads() {
        assert_eq!(parse_reply("```json\n{\"notes\":\"a\",\"reply\":\"b\"}\n```").unwrap().reply, "b");
        assert!(parse_reply("no json here").is_err());
    }
}
