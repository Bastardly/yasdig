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

use super::chat::{Chat, Film, MemberKind, Mode, Msg};
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
    /// The pictures shown as images, already staged into scratch, in order:
    /// the session's reference first (`reference` of them), then the turn's own.
    pub images: Vec<PathBuf>,
    pub reference: usize,
    /// The session is a comparison the server runs: its turns only judge.
    pub sweep: bool,
    pub skills: &'a str,
    /// The checks a critique answers.
    pub checks: &'a [String],
    pub goal: &'a str,
    pub round: u32,
    /// Messages kept from the end of the history; `usize::MAX` for all.
    pub history: usize,
    /// The chat's storyboard, when it has one.
    pub film: Option<&'a Film>,
}

/// A storyboard as text: what a chat turn is told the film is, and what a
/// Story turn rewrites.
pub fn board_text(f: &Film) -> String {
    let mut s = String::new();
    if !f.title.is_empty() {
        s.push_str(&format!("Title: {}\n", f.title));
    }
    s.push_str(&format!("Every shot: {}\nCast:", if f.every.is_empty() { "(nothing)" } else { &f.every }));
    for m in &f.cast {
        let kind = match m.kind {
            MemberKind::Character => "character",
            MemberKind::Place => "place",
            MemberKind::Thing => "thing",
        };
        let picture = match &m.picture {
            Some(p) if p.kind != "item" => "the person's picture",
            Some(_) => "a picture drawn",
            None => "no picture yet",
        };
        s.push_str(&format!("\n- {} ({kind}, {picture}): {}", m.name, m.look));
    }
    s.push_str("\nShots:");
    for (k, shot) in f.shots.iter().enumerate() {
        let how = if k == 0 { "first" } else if shot.cut { "cut" } else { "goes on" };
        let who = if shot.cast.is_empty() { String::new() } else { format!(" [{}]", shot.cast.join(", ")) };
        s.push_str(&format!("\n{}. ({how}) {}{who}", k + 1, shot.text));
    }
    s
}

/// The system message: who the agent is, what it may change and how to reply.
pub fn system(t: &Toolkit, turn: &Turn) -> String {
    let mut s = String::new();
    s.push_str(turn.skills);
    if let Some(label) = t.family_label {
        s.push_str(&format!("\n\nThe image model is a {label} model."));
    }
    let offered: Vec<_> = t.offered_knobs().collect();
    if turn.sweep && matches!(turn.mode, Mode::Plan | Mode::Critique | Mode::Verdict) {
        s.push_str(
            "\n\nThis session is a comparison: yasdig draws one round per value with the same seeds, \
             and you judge. Change nothing; answer what is asked.",
        );
    } else if turn.mode == Mode::Story {
        if schema::board_models(t).is_some() {
            let names: Vec<String> = t
                .models
                .iter()
                .map(|m| format!("{} ({}{})", m.name, m.label, if m.references { ", follows reference pictures" } else { "" }))
                .collect();
            s.push_str(&format!(
                "\n\nThe cast's pictures and the keyframes are drawn with `image_model`, one of: {}. Pick one that \
                 follows reference pictures when there is one: a character then gets a turnaround sheet (front, \
                 three-quarter, side, back) and every later picture of them follows it. If the person names a \
                 model, use it.",
                names.join(", ")
            ));
        }
        if let Some(v) = &t.video {
            s.push_str(&format!(
                "\n\nThe film is shot with {}, a {} video model: {}×{}, each shot at most {:.1} s ({} frames at {} fps).",
                v.name,
                v.label,
                v.width,
                v.height,
                v.shot_seconds(),
                v.frames,
                v.fps
            ));
        }
    } else if turn.mode != Mode::Describe {
        if offered.is_empty() {
            s.push_str("\n\nYou may not change any settings in this chat; advise only.");
        } else {
            s.push_str("\n\nSettings you may change, in `change`:");
            for k in &offered {
                let range = match (k.kind, k.min, k.max) {
                    ("int" | "float" | "size", Some(a), Some(b)) => format!(" Range {a}–{b}."),
                    ("model", ..) => {
                        // Which take no CFG, so a request for CFG picks one that does.
                        let names: Vec<String> = t
                            .models
                            .iter()
                            .map(|m| format!("{} ({}{})", m.name, m.label, if m.locks.cfg_scale.is_some() { ", no CFG" } else { "" }))
                            .collect();
                        format!(" One of: {}. If the person names a model, use it.", names.join(", "))
                    }
                    _ => String::new(),
                };
                s.push_str(&format!("\n- {}: {} Now: {}.{range}", k.key, k.about, k.value));
            }
        }
        let switch = t.knob("model").is_some_and(|m| m.offered());
        for k in t.knobs.iter().filter(|k| k.locked.is_some() && k.allowed) {
            s.push_str(&format!("\n- {} is fixed on this model: {}.", k.label, k.locked.as_deref().unwrap_or("")));
            if switch {
                s.push_str(" Another model may take it: set `model` in the same change, or compare on it.");
            }
        }
        if let Some(turbo) = &t.locks.turbo {
            s.push_str(&format!(
                "\n- The speed LoRA {turbo} is picked. Keep steps and CFG there unless the person asks; \
                 both can still be changed."
            ));
        }
        if matches!(turn.mode, Mode::Chat | Mode::Plan) {
            s.push_str(if schema::writes_prompt(t) {
                "\n\nThe prompt you write in `change.prompt` is exactly what is drawn. Write the whole \
                 prompt there, never in `reply`; the reply says in a sentence what you are drawing."
            } else {
                "\n\nThe prompt is not yours to change; pictures are drawn from the current one."
            });
            if turn.mode == Mode::Chat {
                s.push_str(&format!(
                    "\n\nWhat you do this turn is your `action`, chosen from what the person asked. Start \
                     `notes` by naming it: talk, a drawing, a description, an improvement or a comparison.\n\
                     - \"talk\": answer, and change settings if asked. \"Give me a prompt for a castle\", \
                     \"what does CFG do?\"\n\
                     - \"draw\": draw once. `count` is how many pictures the person asked for (1 to {}; 1 \
                     when they didn't say). `seeds` \"same\" keeps the last round's compositions, to try a \
                     change on the same pictures; \"new\" draws different ones. \"Draw a fox\", \"three more\", \
                     \"now with a blue sky\".",
                    super::knobs::MAX_BATCH
                ));
                if turn.images.len() > turn.reference {
                    s.push_str(
                        "\n- \"describe\": write the attached picture as a prompt for this model, in `prompt`. \
                         \"Turn this into a prompt\", \"what would draw this?\"",
                    );
                }
                s.push_str(
                    "\n- \"improve\": a session that draws rounds, judges each against yes/no checks and changes \
                     the prompt or settings until the pictures are right. \"Keep trying until the hat is red\", \
                     \"make these better\", \"find the best prompt for this picture\" (an attached picture is \
                     what the rounds are judged against). `goal`: one sentence of what the pictures must show.",
                );
                if let Some(why) = &t.film_off {
                    s.push_str(&format!(
                        "\n- A film can't be made in this chat: {why}. If the person asks for one, say so and what would fix it."
                    ));
                } else {
                    s.push_str(
                        "\n- \"film\": make a short movie from the person's story, idea or pictures: a storyboard \
                         of its cast and shots, their pictures drawn, then it waits for the person's go. \"Make a \
                         film of this story\", \"a short movie about a lighthouse keeper\". With a storyboard \
                         already, \"film\" rewrites it: \"make shot 2 at night\", \"add a dog\". The \
                         storyboard is written next, so `reply` only says in a sentence that you are writing it.",
                    );
                }
                if t.offered_knobs().any(|k| super::knobs::COMPARABLE.contains(&k.key)) {
                    s.push_str(
                        "\n- \"compare\": find out which value of one setting works best: the same seeds drawn \
                         once per value, every picture judged, then the answer. \"What is the best CFG?\" → \
                         `vary` cfg_scale, `values` [\"3\", \"5\", \"7\", \"9\"]; \"which model draws hands \
                         better?\" → `vary` model, a value per model; two wordings → `vary` prompt. `count` is \
                         pictures per value, 2 when the person didn't say. `goal`: the question, in one sentence.",
                    );
                }
            }
        }
        let passes: Vec<_> = t.offered_actions().collect();
        if turn.mode == Mode::Critique && !passes.is_empty() {
            s.push_str("\n\nPasses you may run on one keeper (action \"escalate\"):");
            for a in passes {
                s.push_str(&format!("\n- {}: {}", a.key, a.about));
            }
        }
        if turn.mode == Mode::Critique {
            s.push_str(
                "\n\nTo draw another round, set `action` to \"generate\": `count` is its size (this round's, \
                 unless the person asked otherwise), and `seeds` \"same\" keeps these compositions so the \
                 difference is your change; \"new\" draws different ones.",
            );
        }
    }
    if let Some(f) = turn.film.filter(|_| turn.mode == Mode::Chat) {
        s.push_str(&format!("\n\nThis chat's storyboard:\n{}", board_text(f)));
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
    let answering = matches!(turn.mode, Mode::Chat | Mode::Describe | Mode::Story);
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
    let (refs, own) = turn.images.split_at(turn.reference.min(turn.images.len()));
    let reference_parts = |label: &str| -> Vec<Part> {
        let mut parts = Vec::new();
        if !refs.is_empty() {
            parts.push(Part::Text { text: label.to_string() });
            parts.extend(refs.iter().map(|p| Part::Image { path: p.clone() }));
        }
        parts
    };
    let sweep = chat.session.sweep.as_ref().filter(|_| turn.sweep);
    match turn.mode {
        Mode::Plan if sweep.is_some() => {
            let sw = sweep.expect("matched");
            let label = super::knobs::knob(&sw.knob).map_or(sw.knob.as_str(), |k| k.label);
            let values: Vec<String> = sw.values.iter().map(|v| super::knobs::value_label(&sw.knob, v)).collect();
            let mut parts = reference_parts("The picture to match:");
            parts.push(Part::Text {
                text: format!(
                    "Question: {}\nThis is a comparison of {label}: {}, the same seeds each time. Write the checks: \
                     what to look for in every picture to tell the values apart for this question.",
                    turn.goal,
                    values.join("; ")
                ),
            });
            push(&mut out, Role::User, parts);
        }
        Mode::Plan => {
            let prompt = chat.settings.get("prompt").and_then(|p| p.as_str()).unwrap_or("");
            let next = match super::session::judgeable(chat) {
                Some((round, size)) => format!(
                    "The {size} picture(s) of round {round}, drawn with this prompt, will be judged against your \
                     checks next. Change the prompt only if those pictures can't meet the goal; a changed prompt \
                     means a new round instead."
                ),
                None => "Change the prompt if it doesn't ask for everything the goal does.".to_string(),
            };
            let mut parts = reference_parts(
                "The picture to match: the checks are what it shows, and the prompt should draw something like it.",
            );
            parts.push(Part::Text {
                    text: format!(
                        "Goal: {}\nThe prompt now: {prompt}\nWrite the checks for this goal. {next}\nSet `rounds` and \
                         `count` (pictures per round) from the person's words: \"try 2 rounds of 2\" is 2 and 2. \
                         When they don't say, 3 rounds of 4. At most {} rounds of {}.",
                        turn.goal,
                        super::knobs::MAX_ROUNDS,
                        super::knobs::MAX_BATCH
                    ),
            });
            push(&mut out, Role::User, parts);
        }
        Mode::Critique => {
            let mut parts = reference_parts("Reference, the picture to match:");
            // In a comparison, which value this round drew.
            let value = sweep
                .and_then(|sw| {
                    let at = (super::session::session_rounds(chat) as usize).checked_sub(1)?;
                    Some(format!(" It drew {}.", super::knobs::value_label(&sw.knob, sw.values.get(at)?)))
                })
                .unwrap_or_default();
            parts.push(Part::Text {
                text: format!(
                    "Round {} (the session's {} of {}): the {} picture(s) just drawn, in order.{value} Goal: {}",
                    turn.round,
                    super::session::session_rounds(chat),
                    super::session::session_limit(chat),
                    own.len(),
                    turn.goal
                ),
            });
            for (k, p) in own.iter().enumerate() {
                parts.push(Part::Text { text: format!("Picture {}:", k + 1) });
                parts.push(Part::Image { path: p.clone() });
            }
            let checks: Vec<String> = turn.checks.iter().enumerate().map(|(i, c)| format!("{}. {c}", i + 1)).collect();
            parts.push(Part::Text {
                text: format!("Answer each check for each picture, in order:\n{}", checks.join("\n")),
            });
            push(&mut out, Role::User, parts);
        }
        Mode::Verdict => {
            let n = sweep.map_or(0, |s| s.values.len());
            push(
                &mut out,
                Role::User,
                vec![Part::Text {
                    text: format!(
                        "{}\n\nQuestion: {}\nWhich value answers it best? `best` is its number (1 to {n}).",
                        super::session::sweep_table(chat),
                        turn.goal
                    ),
                }],
            );
        }
        Mode::Story => {
            let refs = t.video.as_ref().is_some_and(|v| v.references);
            let secs = t.video.as_ref().map_or(5.0, |v| v.shot_seconds());
            let mut text = String::from("Write the storyboard for a short film, as the person asked.");
            if let Some(f) = turn.film {
                text.push_str(&format!(
                    "\nThe storyboard now:\n{}\nWrite it again whole with the person's change. Keep every name, \
                     look and shot they didn't ask to change, word for word.",
                    board_text(f)
                ));
            }
            text.push_str(&format!(
                "\nOne shot is one action, and runs about {:.0} s. When the person says how long the film is, \
                 that many seconds is that many shots of it (\"10 seconds\" is {} shots). When they say neither, \
                 4 to 6 shots; at most {}.\n\
                 `cut` true starts a new scene: {} false goes on from the last frame of the shot before, so the \
                 same people and place carry on. Shot 1 is never a cut.\n\
                 `cast`: each person, place or thing that must look the same in more than one shot, with its `look` \
                 in words and a `prompt` for its picture: that one subject alone, whole, on a plain background, in \
                 the film's look. A shot's `cast` names who is in it, at most {}.\n\
                 `every`: the film's look in a few words (style, light, colour), put in front of every shot.\n\
                 A shot's `text` says what happens, in the present tense, naming who is in it.",
                secs,
                (10.0 / secs.max(0.1)).round().max(1.0),
                crate::pipeline::MAX_SHOTS,
                if refs {
                    "it is drawn from the pictures of the cast it names, so name everyone in it."
                } else {
                    "it starts from a picture drawn first, with the cast it names."
                },
                crate::config::MAX_REFERENCES
            ));
            let attached = own.len();
            if attached > 0 {
                text.push_str(&format!(
                    "\nThe person attached {attached} picture(s): a member's `picture` n makes picture n its picture \
                     (\"this is Mara\"), and its `look` describes it; 0 draws one from its `prompt`."
                ));
            }
            push(&mut out, Role::User, vec![Part::Text { text }]);
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
    use super::super::knobs::{Locks, Rules};
    use super::super::toolkit::build;
    use super::*;
    use crate::config::ImageFamily;

    fn chat() -> Chat {
        let mut c = Chat::new("1-0".into(), serde_json::json!({"prompt": "a fox"}), None);
        let img = |id: &str| ImageRef { kind: "item".into(), id: id.into(), file: format!("{id}.png"), seed: Some(1) };
        c.messages = vec![
            Msg::User { n: 0, ms: 0, text: "a fox in a hat".into(), images: vec![] },
            Msg::Agent { n: 1, ms: 0, mode: Mode::Plan, reply: "On it.".into(), notes: "need hat".into(), checks: vec![],
                         keepers: vec![], passed: vec![], changes: vec![], action: None, prompt: None, negative: None,
                         skills: vec![], stats: Default::default() },
            Msg::Batch { n: 2, ms: 0, round: 1, settings_rev: 0, summary: "prompt 'a fox', CFG 6".into(), size: 2, images: vec![img("a"), img("b")] },
            Msg::Note { n: 3, ms: 0, text: "Stopped.".into() },
            Msg::User { n: 4, ms: 0, text: "try again".into(), images: vec![] },
        ];
        c
    }

    fn toolkit() -> Toolkit {
        build(&Rules::default(), &serde_json::json!({"prompt": "a fox"}), &Locks::default(), vec![], Some(ImageFamily::Sdxl))
    }

    fn turn(mode: Mode, images: Vec<PathBuf>) -> Turn<'static> {
        Turn { mode, images, reference: 0, sweep: false, skills: "SKILLS", checks: &[], goal: "a fox in a red hat", round: 2, history: usize::MAX, film: None }
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
        assert!(s.contains("- prompt:") && s.contains("- cfg_scale:") && s.contains("Range 0–"));
        assert!(!s.contains("- steps:"), "steps is off by default");
    }

    #[test]
    fn a_chat_turn_is_told_how_to_improve_and_compare_with_examples() {
        let blind = system(&toolkit(), &turn(Mode::Chat, vec![]));
        assert!(blind.contains("\"improve\"") && blind.contains("\"compare\"") && !blind.contains("\"describe\""));
        assert!(blind.contains("What is the best CFG?") && blind.contains("Start `notes` by naming it"));
        let seeing = system(&toolkit(), &turn(Mode::Chat, vec!["/w/a.png".into()]));
        assert!(seeing.contains("\"describe\""));
    }

    #[test]
    fn a_critique_sees_the_reference_before_the_round() {
        let checks = ["Is there a hat?".to_string()];
        let t = Turn {
            checks: &checks,
            reference: 1,
            ..turn(Mode::Critique, vec!["/w/ref.png".into(), "/w/a.png".into(), "/w/b.png".into()])
        };
        let m = messages(&chat(), &toolkit(), &t);
        let last = &m.last().unwrap().content;
        let first_image = last.iter().position(|p| matches!(p, Part::Image { path } if path.ends_with("ref.png"))).unwrap();
        let labelled = last[..first_image].iter().any(|p| matches!(p, Part::Text { text } if text.starts_with("Reference")));
        assert!(labelled, "the reference comes first, named");
        let text: String = last.iter().filter_map(|p| match p { Part::Text { text } => Some(text.as_str()), _ => None }).collect();
        assert!(text.contains("the 2 picture(s) just drawn") && text.contains("Picture 2:") && !text.contains("Picture 3:"));
    }
}
