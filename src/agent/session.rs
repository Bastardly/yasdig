//! The agent's bounded workflow, as pure functions over a [`Chat`]: what the
//! next step is, what a reply does to the chat, what an approval resumes.
//! The worker (`agent::step`) only carries them out — a turn, an image, a
//! pass — so the decisions are tested here without a GPU.
//!
//! A session runs **plan → round → critique → round → … → done**, at most
//! `rounds` rounds, and a plain chat turn may start the same loop by asking to
//! generate. Each step is one job-queue step, so a button press on the Image
//! tab runs between any two of them. Anything set to *ask first* ends the leg
//! as `awaiting`: nothing holds the queue while the person decides.

use super::chat::{ActionRec, ActionState, Change, ChangeState, Chat, CheckRow, Mode, Msg, Next, State};
use super::knobs::{self, Locks, Perm};
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
        _ if chat.pending() => Step::End,
        Next::Turn => Step::Turn(s.mode),
        Next::Critique => Step::Turn(Mode::Critique),
        Next::Batch => {
            let drawn = current_batch(chat).map_or(0, |(_, n)| n);
            Step::Image { round: s.round, k: drawn }
        }
        Next::Escalate => chat
            .actions
            .iter()
            .find(|a| a.state == ActionState::Queued)
            .map_or(Step::End, |a| Step::Escalate(a.id)),
    }
}

/// The round being drawn, and how many of its pictures exist.
fn current_batch(chat: &Chat) -> Option<(u32, u32)> {
    chat.messages.iter().rev().find_map(|m| match m {
        Msg::Batch { round, images, .. } if *round == chat.session.round => Some((*round, images.len() as u32)),
        _ => None,
    })
}

/// The seed for picture `k` of a round: the same every round with the seed
/// locked, so a difference between rounds is the change; drawn afresh
/// otherwise (`-1`).
pub fn seed_for(chat: &Chat, k: u32) -> i64 {
    if chat.permissions.seed_lock { chat.session.base_seed + k as i64 } else { -1 }
}

/// Begin the next round: a `Batch` message with no pictures yet.
pub fn start_round(chat: &mut Chat, base_seed: i64) {
    if chat.session.round == 0 && chat.session.base_seed == 0 {
        // The chat's own seed when one is set, so a session can be asked to
        // keep a composition the person liked.
        chat.session.base_seed = chat.settings.get("seed").and_then(|v| v.as_i64()).filter(|s| *s >= 0).unwrap_or(base_seed);
    }
    chat.session.round += 1;
    let n = chat.next_n();
    chat.messages.push(Msg::Batch {
        n,
        ms: super::chat::now_ms(),
        round: chat.session.round,
        settings_rev: chat.settings_rev,
        summary: summary(&chat.settings),
        images: Vec::new(),
    });
    chat.session.next = Next::Batch;
}

/// Whether the leg's next step is a picture of a round that hasn't begun —
/// none drawn yet, or the last one full — so the worker must begin it first.
pub fn needs_round(chat: &Chat) -> bool {
    chat.session.next == Next::Batch
        && current_batch(chat).is_none_or(|(_, n)| n >= chat.permissions.batch)
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

/// A picture of the round landed.
pub fn image_done(chat: &mut Chat, img: super::chat::ImageRef) {
    let round = chat.session.round;
    let batch = chat.permissions.batch;
    for m in chat.messages.iter_mut().rev() {
        if let Msg::Batch { round: r, images, .. } = m {
            if *r == round {
                images.push(img);
                if images.len() as u32 >= batch {
                    // A round a chat turn asked for, with no checks to judge it
                    // by, is shown and left to the person.
                    chat.session.next = if chat.session.checks.is_empty() { Next::Nothing } else { Next::Critique };
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
    /// Plan: the questions. Critique: one row of answers per picture.
    #[serde(default)]
    pub checks: Option<Value>,
    #[serde(default)]
    pub keepers: Vec<u32>,
    #[serde(default)]
    pub escalate: Option<EscalateJson>,
    #[serde(default)]
    pub prompt: Option<String>,
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

/// What a reply did, for the worker.
#[derive(Debug, Default, PartialEq)]
pub struct Outcome {
    /// Something waits for the person.
    pub awaiting: bool,
}

/// Record a reply on the chat and decide what comes next.
pub fn apply_reply(chat: &mut Chat, mode: Mode, r: ReplyJson, t: &Toolkit, locks: &Locks, skills: Vec<String>, stats: crate::llm::Stats) -> Outcome {
    let n = chat.next_n();
    let mut change_ids = Vec::new();
    let mut notes_out = Vec::new();
    // Settings: auto knobs now, ask knobs as one pending change.
    if let Some(change) = r.change.as_ref().filter(|c| !c.is_empty()) {
        let models = t.model_names();
        let auto = knobs::apply(&mut chat.settings, change, &chat.permissions, locks, &models, &[Perm::Auto]);
        notes_out.extend(auto.notes.iter().filter(|n| !n.contains("isn't the agent's to change")).cloned());
        if !auto.set.is_empty() {
            chat.settings_rev += 1;
            let id = chat.next_change_id();
            chat.changes.push(Change {
                id,
                by: "agent".into(),
                state: ChangeState::Applied,
                set: auto.set.clone(),
                before: auto.before,
                reason: r.reason.clone(),
                msg: n,
                notes: Vec::new(),
            });
            change_ids.push(id);
        }
        // What is left that the person asked to be asked about.
        let asked: Map<String, Value> = change
            .iter()
            .filter(|(k, _)| !auto.set.contains_key(*k))
            .filter(|(k, _)| knobs::knob(k).is_some_and(|kn| chat.permissions.knob(kn).perm == Perm::Ask))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if !asked.is_empty() {
            // A dry run says what it would become, so the card shows the
            // clamped value and Approve applies exactly that.
            let mut probe = chat.settings.clone();
            let would = knobs::apply(&mut probe, &asked, &chat.permissions, locks, &models, &[Perm::Ask]);
            if !would.set.is_empty() {
                let id = chat.next_change_id();
                chat.changes.push(Change {
                    id,
                    by: "agent".into(),
                    state: ChangeState::Pending,
                    set: would.set,
                    before: would.before,
                    reason: r.reason.clone(),
                    msg: n,
                    notes: would.notes,
                });
                change_ids.push(id);
            }
        }
    }

    let batch = current_batch(chat).map_or(0, |(_, n)| n);
    let mut checks_rows = Vec::new();
    let mut passed = Vec::new();
    let mut keepers = Vec::new();
    let mut action_id = None;
    match mode {
        Mode::Plan => {
            let qs: Vec<String> = r
                .checks
                .as_ref()
                .and_then(|c| c.as_array())
                .map(|a| a.iter().filter_map(|q| q.as_str().map(|s| s.trim().to_string())).filter(|s| !s.is_empty()).collect())
                .unwrap_or_default();
            chat.session.checks = qs.into_iter().take(super::schema::MAX_CHECKS as usize).collect();
            chat.session.next = Next::Batch;
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
                "generate" if chat.session.round < chat.permissions.rounds => Next::Batch,
                "generate" => {
                    notes_out.push(format!("That was round {} of {}, so the session stops here.", chat.session.round, chat.permissions.rounds));
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
                Some("generate") => Next::Batch,
                _ => Next::Nothing,
            };
        }
        Mode::Describe => chat.session.next = Next::Nothing,
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
    // A new round is begun by the worker (it draws the seed), so a reply only
    // says "a round next"; the round itself starts when the leg goes on.
    let awaiting = chat.pending();
    if awaiting {
        chat.session.state = State::Awaiting;
    }
    Outcome { awaiting }
}

/// The pass a critique asked for, as a queued or pending action.
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
        state: if a.perm == Perm::Auto { ActionState::Queued } else { ActionState::Pending },
        result: None,
        msg,
        error: None,
    });
    Ok(id)
}

/// The person's answers to what waited. Returns whether the leg should go on.
pub fn approve(
    chat: &mut Chat,
    changes: &[(u64, bool)],
    actions: &[(u64, bool)],
    locks: &Locks,
    models: &[String],
) -> bool {
    for (id, yes) in changes {
        let Some(i) = chat.changes.iter().position(|c| c.id == *id && c.state == ChangeState::Pending) else { continue };
        if *yes {
            let set = chat.changes[i].set.clone();
            let applied = knobs::apply(&mut chat.settings, &set, &chat.permissions, locks, models, &[Perm::Ask, Perm::Auto]);
            let c = &mut chat.changes[i];
            c.before = applied.before;
            c.set = applied.set;
            c.notes.extend(applied.notes);
            c.state = ChangeState::Applied;
            chat.settings_rev += 1;
        } else {
            chat.changes[i].state = ChangeState::Rejected;
        }
    }
    for (id, yes) in actions {
        if let Some(a) = chat.actions.iter_mut().find(|a| a.id == *id && a.state == ActionState::Pending) {
            a.state = if *yes { ActionState::Queued } else { ActionState::Rejected };
        }
    }
    if chat.session.next == Next::Escalate && !chat.actions.iter().any(|a| a.state == ActionState::Queued) {
        chat.session.next = Next::Nothing;
    }
    let go_on = !chat.pending() && chat.session.next != Next::Nothing;
    if !chat.pending() && chat.session.state == State::Awaiting {
        chat.session.state = if go_on { State::Idle } else { finished(chat) };
    }
    go_on
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
    use super::super::knobs::Permissions;
    use super::super::toolkit::build;
    use super::*;
    use crate::config::ImageFamily;
    use serde_json::json;

    fn chat() -> Chat {
        let mut c = Chat::new("1-0".into(), json!({"prompt": "a fox", "cfg_scale": 6.0}), Permissions::default(), None);
        c.permissions.batch = 2;
        c.permissions.rounds = 2;
        c.session.goal = "a fox in a red hat".into();
        c.session.next = Next::Turn;
        c.session.mode = Mode::Plan;
        c
    }

    fn tk(c: &Chat) -> Toolkit {
        build(&c.permissions, &c.settings, &Locks::default(), vec![], Some(ImageFamily::Sdxl))
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
        apply_reply(&mut c, Mode::Plan, reply(json!({"notes": "", "checks": ["Is there a hat?", "Is it red?"], "reply": "ok"})), &t, &Locks::default(), vec![], Default::default());
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
    fn ask_first_pauses_and_an_approval_resumes() {
        let mut c = chat();
        c.permissions.knobs.get_mut("prompt").unwrap().perm = Perm::Ask;
        c.session.mode = Mode::Chat;
        let t = tk(&c);
        let out = apply_reply(&mut c, Mode::Chat, reply(json!({"notes": "", "reply": "x", "action": "generate", "change": {"prompt": "a dog"}})), &t, &Locks::default(), vec![], Default::default());
        assert!(out.awaiting);
        assert_eq!(c.session.state, State::Awaiting);
        assert_eq!(c.settings["prompt"], json!("a fox"), "not applied yet");
        assert_eq!(next_step(&c), Step::End, "nothing runs while it waits");
        let id = c.changes.iter().find(|x| x.state == ChangeState::Pending).unwrap().id;
        assert!(approve(&mut c, &[(id, true)], &[], &Locks::default(), &[]));
        assert_eq!(c.settings["prompt"], json!("a dog"));
        assert_eq!(c.session.next, Next::Batch);
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
        c.permissions.actions.insert("hires".into(), Perm::Auto);
        let t = tk(&c);
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
