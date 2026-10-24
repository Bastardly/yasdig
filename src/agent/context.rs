//! What one turn is shown: the system text (skills, the toolkit, the reply's
//! rules) and the conversation, rebuilt from the chat every time.
//!
//! **Only the newest pictures are pictures.** An image costs a few hundred
//! tokens and a 4B model's context is a few thousand, so earlier rounds reach a
//! turn as a line each — what was drawn with what, what the agent noted, which
//! it kept — and only the batch being judged, or the person's latest upload, is
//! sent as images. Older turns are their `notes` and `reply`.
//!
//! Roles alternate strictly (Mistral's and Gemma's templates refuse two user
//! messages in a row), so neighbours of one role are merged, and the list
//! always ends on the person's side for the reply to follow.

use super::chat::{Chat, Mode, Msg};
use super::schema;
use super::toolkit::Toolkit;
use crate::llm::{Message, Part, Role};
use std::path::PathBuf;

/// A rough token count for text: the Qwen and Gemma tokenizers average a
/// little over three characters per token on English prose.
pub fn text_tokens(s: &str) -> usize {
    s.len() / 3 + 1
}

/// What a picture costs at `side` on its longest edge: Qwen-VL's 16-pixel
/// patches merged 2×2 make (side/32)² tokens, 256 at 512; Gemma's and
/// Pixtral's land near the same. A floor of 256 keeps the estimate honest for
/// projectors that tile.
pub fn image_tokens(side: u32) -> usize {
    (((side / 32) * (side / 32)) as usize).max(256)
}

/// What the turn is about, past the conversation itself.
pub struct Turn<'a> {
    pub mode: Mode,
    /// The pictures shown as images, already staged into scratch, in order.
    pub images: Vec<PathBuf>,
    pub skills: &'a str,
    /// The checks a critique answers.
    pub checks: &'a [String],
    pub goal: &'a str,
    pub round: u32,
    /// Messages kept from the end of the history; `usize::MAX` for all.
    pub history: usize,
}

/// The system message: who the agent is, what it may change and how to reply.
pub fn system(t: &Toolkit, turn: &Turn) -> String {
    let mut s = String::new();
    s.push_str(turn.skills);
    if let Some(label) = t.family_label {
        s.push_str(&format!("\n\nThe image model is a {label} model."));
    }
    let offered: Vec<_> = t.offered_knobs().collect();
    if turn.mode != Mode::Describe {
        if offered.is_empty() {
            s.push_str("\n\nYou may not change any settings in this chat; advise only.");
        } else {
            s.push_str(&format!(
                "\n\nSettings you may change (at most {} per reply, in `change`):",
                t.max_changes
            ));
            for k in &offered {
                let range = match (k.kind, k.min, k.max) {
                    ("int" | "float" | "size", Some(a), Some(b)) => format!(" Range {a}–{b}."),
                    ("model", ..) => format!(" One of: {}.", k.allowed.join(", ")),
                    _ => String::new(),
                };
                s.push_str(&format!("\n- {}: {} Now: {}.{range}", k.key, k.about, k.value));
            }
        }
        for k in t.knobs.iter().filter(|k| k.locked.is_some() && k.perm != super::knobs::Perm::Off) {
            s.push_str(&format!("\n- {} is fixed: {}.", k.label, k.locked.as_deref().unwrap_or("")));
        }
        if matches!(turn.mode, Mode::Chat | Mode::Plan) {
            s.push_str(if schema::writes_prompt(t) {
                "\n\nThe prompt you write in `change.prompt` is exactly what is drawn. Write the whole \
                 prompt there, never in `reply`; the reply says in a sentence what you are drawing."
            } else {
                "\n\nThe prompt is not yours to change; pictures are drawn from the current one."
            });
        }
        let passes: Vec<_> = t.offered_actions().collect();
        if turn.mode == Mode::Critique && !passes.is_empty() {
            s.push_str("\n\nPasses you may run on one keeper (action \"escalate\"):");
            for a in passes {
                s.push_str(&format!("\n- {}: {}", a.key, a.about));
            }
        }
    }
    s.push_str(&format!(
        "\n\nReply with one JSON object. Write `notes` first: your own short reasoning (under {} \
         characters), which you will see again next turn. `reply` is what the person reads.",
        schema::NOTES_CHARS
    ));
    s
}

/// The conversation for one turn.
pub fn messages(chat: &Chat, t: &Toolkit, turn: &Turn) -> Vec<Message> {
    let mut out = vec![Message::text(Role::System, system(t, turn))];
    let history = &chat.messages;
    // The newest user message is the one this turn answers in chat and
    // describe modes; in plan and critique the last message is written here.
    let answering = matches!(turn.mode, Mode::Chat | Mode::Describe);
    let last_user = history.iter().rposition(|m| matches!(m, Msg::User { .. }));
    let keep_from = history.len().saturating_sub(turn.history);
    for (i, m) in history.iter().enumerate().skip(keep_from) {
        let is_current = answering && Some(i) == last_user;
        match m {
            Msg::User { text, images, .. } => {
                let mut parts = Vec::new();
                if is_current {
                    for (k, p) in turn.images.iter().enumerate() {
                        if turn.images.len() > 1 {
                            parts.push(Part::Text { text: format!("Picture {}:", k + 1) });
                        }
                        parts.push(Part::Image { path: p.clone() });
                    }
                } else if !images.is_empty() {
                    parts.push(Part::Text { text: format!("[{} picture(s) attached earlier]", images.len()) });
                }
                if is_current && turn.mode == Mode::Describe {
                    let family = t.family_label.unwrap_or("the current");
                    parts.push(Part::Text {
                        text: format!("Describe this picture as a prompt for a {family} model. {text}").trim().to_string(),
                    });
                } else if !text.is_empty() {
                    parts.push(Part::Text { text: text.clone() });
                }
                push(&mut out, Role::User, parts);
            }
            Msg::Agent { notes, reply, prompt, .. } => {
                let mut v = serde_json::json!({ "notes": notes, "reply": reply });
                if let Some(p) = prompt {
                    v["prompt"] = serde_json::Value::String(p.clone());
                }
                push(&mut out, Role::Assistant, vec![Part::Text { text: v.to_string() }]);
            }
            Msg::Batch { round, summary, images, .. } => {
                let text = format!("[yasdig] Round {round}: {} picture(s) drawn — {summary}", images.len());
                push(&mut out, Role::User, vec![Part::Text { text }]);
            }
            Msg::Note { text, .. } => {
                push(&mut out, Role::User, vec![Part::Text { text: format!("[yasdig] {text}") }]);
            }
        }
    }
    match turn.mode {
        Mode::Plan => {
            let prompt = chat.settings.get("prompt").and_then(|p| p.as_str()).unwrap_or("");
            push(
                &mut out,
                Role::User,
                vec![Part::Text {
                    text: format!(
                        "Goal: {}\nThe prompt now: {prompt}\nWrite the checks for this goal, and change the prompt \
                         if it doesn't ask for everything the goal does.",
                        turn.goal
                    ),
                }],
            );
        }
        Mode::Critique => {
            let mut parts = vec![Part::Text {
                text: format!(
                    "Round {}: the {} picture(s) just drawn, in order. Goal: {}",
                    turn.round,
                    turn.images.len(),
                    turn.goal
                ),
            }];
            for (k, p) in turn.images.iter().enumerate() {
                parts.push(Part::Text { text: format!("Picture {}:", k + 1) });
                parts.push(Part::Image { path: p.clone() });
            }
            let checks: Vec<String> = turn.checks.iter().enumerate().map(|(i, c)| format!("{}. {c}", i + 1)).collect();
            parts.push(Part::Text {
                text: format!("Answer each check for each picture, in order:\n{}", checks.join("\n")),
            });
            push(&mut out, Role::User, parts);
        }
        Mode::Chat | Mode::Describe => {}
    }
    // The reply must follow the person's side.
    if out.last().is_some_and(|m| m.role != Role::User) {
        push(&mut out, Role::User, vec![Part::Text { text: "Go on.".into() }]);
    }
    out
}

/// Append, merging into the previous message when the role repeats.
fn push(out: &mut Vec<Message>, role: Role, parts: Vec<Part>) {
    if parts.is_empty() {
        return;
    }
    match out.last_mut() {
        Some(last) if last.role == role && role != Role::System => {
            last.content.push(Part::Text { text: "\n".into() });
            last.content.extend(parts);
        }
        _ => out.push(Message { role, content: parts }),
    }
}

/// An estimate of what the messages cost, in tokens.
pub fn estimate(msgs: &[Message], image_side: u32) -> usize {
    msgs.iter()
        .flat_map(|m| &m.content)
        .map(|p| match p {
            Part::Text { text } => text_tokens(text),
            Part::Image { .. } => image_tokens(image_side),
        })
        .sum::<usize>()
        // Each message's template wrapping: role tags, turn markers.
        + msgs.len() * 8
}

/// The longest history that fits `budget` tokens, as a [`Turn::history`].
pub fn fit_history(chat: &Chat, t: &Toolkit, turn: &Turn, image_side: u32, budget: usize) -> usize {
    let mut keep = chat.messages.len();
    loop {
        let probe = Turn { history: keep, ..turn.clone() };
        if keep == 0 || estimate(&messages(chat, t, &probe), image_side) <= budget {
            return keep;
        }
        keep -= 1;
    }
}

impl Clone for Turn<'_> {
    fn clone(&self) -> Self {
        Turn { images: self.images.clone(), ..*self }
    }
}

#[cfg(test)]
mod tests {
    use super::super::chat::ImageRef;
    use super::super::knobs::{Locks, Permissions};
    use super::super::toolkit::build;
    use super::*;
    use crate::config::ImageFamily;

    fn chat() -> Chat {
        let mut c = Chat::new("1-0".into(), serde_json::json!({"prompt": "a fox"}), Permissions::default(), None);
        let img = |id: &str| ImageRef { kind: "item".into(), id: id.into(), file: format!("{id}.png"), seed: Some(1) };
        c.messages = vec![
            Msg::User { n: 0, ms: 0, text: "a fox in a hat".into(), images: vec![] },
            Msg::Agent { n: 1, ms: 0, mode: Mode::Plan, reply: "On it.".into(), notes: "need hat".into(), checks: vec![],
                         keepers: vec![], passed: vec![], changes: vec![], action: None, prompt: None, negative: None,
                         skills: vec![], stats: Default::default() },
            Msg::Batch { n: 2, ms: 0, round: 1, settings_rev: 0, summary: "prompt 'a fox', CFG 6".into(), images: vec![img("a"), img("b")] },
            Msg::Note { n: 3, ms: 0, text: "Stopped.".into() },
            Msg::User { n: 4, ms: 0, text: "try again".into(), images: vec![] },
        ];
        c
    }

    fn toolkit() -> Toolkit {
        build(&Permissions::default(), &serde_json::json!({"prompt": "a fox"}), &Locks::default(), vec![], Some(ImageFamily::Sdxl))
    }

    fn turn(mode: Mode, images: Vec<PathBuf>) -> Turn<'static> {
        Turn { mode, images, skills: "SKILLS", checks: &[], goal: "a fox in a red hat", round: 2, history: usize::MAX }
    }

    #[test]
    fn roles_alternate_and_end_on_the_persons_side() {
        let m = messages(&chat(), &toolkit(), &turn(Mode::Chat, vec![]));
        let roles: Vec<Role> = m.iter().map(|m| m.role).collect();
        assert_eq!(roles, [Role::System, Role::User, Role::Assistant, Role::User]);
        // The batch line, the note and the new message were merged.
        let last = &m[3].content;
        let text: String = last.iter().filter_map(|p| match p { Part::Text { text } => Some(text.as_str()), _ => None }).collect();
        assert!(text.contains("Round 1") && text.contains("Stopped.") && text.contains("try again"), "{text}");
    }

    #[test]
    fn only_the_batch_being_judged_is_sent_as_pictures() {
        let checks = ["Is there a hat?".to_string()];
        let t = Turn { checks: &checks, ..turn(Mode::Critique, vec!["/w/a.png".into(), "/w/b.png".into()]) };
        let m = messages(&chat(), &toolkit(), &t);
        let images: usize = m.iter().flat_map(|m| &m.content).filter(|p| matches!(p, Part::Image { .. })).count();
        assert_eq!(images, 2);
        let last: String = m.last().unwrap().content.iter().filter_map(|p| match p { Part::Text { text } => Some(text.as_str()), _ => None }).collect();
        assert!(last.contains("1. Is there a hat?"));
    }

    #[test]
    fn a_tight_budget_drops_the_oldest_history_first() {
        let c = chat();
        let t = toolkit();
        let tn = turn(Mode::Chat, vec![]);
        let all = estimate(&messages(&c, &t, &tn), 512);
        let keep = fit_history(&c, &t, &tn, 512, all - 5);
        assert!(keep < c.messages.len());
        assert_eq!(fit_history(&c, &t, &tn, 512, all), c.messages.len());
    }

    #[test]
    fn the_system_text_names_only_what_may_change() {
        let s = system(&toolkit(), &turn(Mode::Chat, vec![]));
        assert!(s.contains("- prompt:") && s.contains("- cfg_scale:") && s.contains("Range 1–8"));
        assert!(!s.contains("- steps:"), "steps is off by default");
    }
}
