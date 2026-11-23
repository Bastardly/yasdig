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
    /// The cast member the chat is making, and whether the turn's picture is
    /// its newest try (shown when the person attached none).
    pub cast: Option<&'a super::chat::ChatCast>,
    pub cast_shown: bool,
}

/// A storyboard as text: what a chat turn is told the film is, and what a
/// Story turn rewrites.
pub fn board_text(f: &Film, fps: Option<u32>) -> String {
    let mut s = String::new();
    if !f.title.is_empty() {
        s.push_str(&format!("Title: {}\n", f.title));
    }
    s.push_str("Cast:");
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
        if !m.speaks.trim().is_empty() {
            s.push_str(&format!(" Speaks: {}", m.speaks.trim()));
        }
    }
    // Scenes, each shot under the number the storyboard shows it by and its
    // id (what `keep` names), each part a line.
    let groups = f.groups();
    let scenes = f.scenes.len().max(1);
    for si in 0..scenes {
        let mine: Vec<&std::ops::Range<usize>> = groups.iter().filter(|g| f.shots[g.start].scene.min(scenes - 1) == si).collect();
        let sc = f.scenes.get(si);
        let title = sc.map_or("", |c| c.title.as_str());
        let id = sc.map_or("", |c| c.id.as_str());
        let locked = mine.iter().any(|g| f.shots[g.start].drawn);
        s.push_str(&format!(
            "\nScene {}{}{}{}",
            si + 1,
            if title.is_empty() { String::new() } else { format!(": {title}") },
            if id.is_empty() { String::new() } else { format!(" (id {id})") },
            if locked { " (locked: a shot in it is generated, so its description stays)" } else { "" }
        ));
        let every = f.scene_every(si);
        s.push_str(&format!("\n  Description: {}", if every.is_empty() { "(nothing)" } else { every }));
        for (k, g) in mine.iter().enumerate() {
            let first = &f.shots[g.start];
            let name = shot_number(si, k, scenes);
            let id = first.shot.as_deref().map_or(String::from("new"), short_id);
            let done = if first.drawn { ", generated" } else { "" };
            s.push_str(&format!("\n  {name} (id {id}{done}):"));
            for k in (*g).clone() {
                let part = &f.shots[k];
                let who = if part.cast.is_empty() { String::new() } else { format!(" [{}]", part.cast.join(", ")) };
                let words = shown_words(part);
                // A shot the person added and left empty: the agent may fill it.
                let words = if words.is_empty() { "(empty: the person added this shot; give it parts or keep it)".to_string() } else { words };
                let len = part.frames.zip(fps).map_or(String::new(), |(n, r)| format!(" ({:.1} s)", n as f32 / r.max(1) as f32));
                let theirs = if part.theirs && !part.text.trim().is_empty() { " (the person's)" } else { "" };
                s.push_str(&format!("\n  - {words}{who}{len}{theirs}"));
                if !part.lines.is_empty() {
                    let said: Vec<String> = part.lines.iter().map(|l| format!("{}: \"{}\"", l.who, l.words)).collect();
                    s.push_str(&format!("\n    lines: {}", said.join(" ")));
                }
                if !part.sound.is_empty() {
                    s.push_str(&format!("\n    sound: {}", part.sound));
                }
                if !part.music.is_empty() {
                    s.push_str(&format!("\n    music: {}", part.music));
                }
            }
        }
    }
    s
}

/// How the storyboard names the `k`th shot of scene `si` (`storyboard.js`
/// `shotName`): "Scene 2 · Shot 1", or "Shot 1" in a one-scene film — the
/// person's words for it.
pub fn shot_number(si: usize, k: usize, scenes: usize) -> String {
    if scenes > 1 { format!("Scene {} · Shot {}", si + 1, k + 1) } else { format!("Shot {}", k + 1) }
}

/// The storyboard's name for the shot part `k` of `f` is in.
pub fn shot_name_of(f: &Film, k: usize) -> String {
    let groups = f.groups();
    let scenes = f.scenes.len().max(1);
    let Some(gi) = groups.iter().position(|g| g.contains(&k)) else { return format!("Shot {}", k + 1) };
    let si = f.shots[groups[gi].start].scene.min(scenes - 1);
    let n = groups[..gi].iter().filter(|g| f.shots[g.start].scene.min(scenes - 1) == si).count();
    shot_number(si, n, scenes)
}

/// Characters of a part's words a storyboard shows at most: a part the
/// person wrote out at length (an enhanced paragraph runs to 2400) would
/// crowd the board, and could never be written back whole under the
/// reply's cap. A reply that gives the words as shown keeps the part.
pub const SHOWN_WORDS: usize = 400;

/// A part's words as a storyboard shows them: the story's own words, else
/// its text, on one line, cut at [`SHOWN_WORDS`] with "…".
pub fn shown_words(part: &super::chat::FilmShot) -> String {
    let one: String = part.words().split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= SHOWN_WORDS {
        return one;
    }
    let cut: String = one.chars().take(SHOWN_WORDS).collect();
    format!("{}…", cut.trim_end())
}

/// Whether a reply's words for a part are the words it was shown: the same
/// on one line, a cut part's shown start (with or without its "…").
pub fn same_words(reply: &str, part: &super::chat::FilmShot) -> bool {
    let norm = |s: &str| s.trim().trim_end_matches('…').trim_end_matches("...").split_whitespace().collect::<Vec<_>>().join(" ");
    let r = norm(reply);
    !r.is_empty() && r == norm(&shown_words(part))
}

/// A Video shot's id as the story names it: `{video}-s3` is `s3`.
pub fn short_id(id: &str) -> String {
    id.rsplit('-').next().unwrap_or(id).to_string()
}

/// The system message: who the agent is, what it may change and how to reply.
/// Saved cast entries a story turn is shown at most, and characters of a
/// look: the list is context, not the story, and a long library would crowd
/// out the board.
/// What a "film" is for, in the person's words rather than ours: they don't
/// know "storyboard", "scene" or "part", and on 2026-11-14 a 4 s "clip",
/// "shot" and "4 seconds of" were each answered with "I can't make video
/// clips" — the line this replaced told the agent so. The storyboard *is*
/// how a video is made here: the person presses Generate on it.
const FILM_ASKS: &str = "make a video. On the Video tab any request for something that moves is a film, \
     however short and whatever it is called: a clip, a shot, a scene, a video, an animation, a movie, \
     \"4 seconds of a dog stealing a sausage\", \"a short movie about a lighthouse keeper\". You write it as a \
     storyboard — its cast, scenes and shots — and the person generates the video from it with one press, so \
     never say you can't make video. Only when you can't tell whether they want a still picture or a moving \
     video, ask which in one sentence, with \"talk\".";

const LIBRARY_MAX: usize = 24;
const LIBRARY_LOOK_CHARS: usize = 240;

/// The saved cast the person asked for (`toolkit::offered`), for a story
/// turn: names it reuses exactly, so `session::apply_story` finds the entry
/// and its pictures come with it.
fn library_text(library: &[super::toolkit::SavedCast]) -> Option<String> {
    if library.is_empty() {
        return None;
    }
    let mut s = String::from(
        "\nFrom the person's saved cast, the ones they asked for. Use the name exactly as written here and the \
         look word for word: their pictures come with the name. Anyone else in the film is new.",
    );
    for e in library.iter().take(LIBRARY_MAX) {
        let mut look: String = e.look.chars().take(LIBRARY_LOOK_CHARS).collect();
        if look.len() < e.look.len() {
            look.push('…');
        }
        let kind = match e.kind {
            super::chat::MemberKind::Character => "character",
            super::chat::MemberKind::Place => "place",
            super::chat::MemberKind::Thing => "thing",
        };
        s.push_str(&format!("\n- {} ({kind}): {look}", e.name));
    }
    Some(s)
}

/// Variables a turn is shown at most, words of each, and characters of a word.
const VARS_MAX: usize = 12;
const VAR_WORDS: usize = 6;
const VAR_WORD_CHARS: usize = 30;

/// The person's prompt variables, for a turn that writes the prompt: the
/// names that work in braces, with a few of each one's words. `None` when
/// there are none — the agent then never hears of braces, and one it writes
/// anyway loses them (`session::apply_reply`).
fn variables_text(vars: &crate::variables::Variables) -> Option<String> {
    if vars.is_empty() {
        return None;
    }
    let mut s = String::from(
        "\n\nVariables: a {name} in the prompt is replaced by one of the person's words for it, picked anew for \
         every picture. Keep each {name} the prompt has exactly as written, braces and all, unless asked to \
         take it out. Put one in when the person asks for variety (\"a different animal each time\"). Only \
         these names work; never make one up:",
    );
    for (name, words) in vars.iter().take(VARS_MAX) {
        let shown: Vec<String> = words.iter().take(VAR_WORDS).map(|w| w.chars().take(VAR_WORD_CHARS).collect()).collect();
        let more = words.len().saturating_sub(VAR_WORDS);
        s.push_str(&format!("\n- {{{name}}}: {}{}", shown.join(", "), if more > 0 { format!(" (+{more} more)") } else { String::new() }));
    }
    Some(s)
}

/// The Image tab's word picks, for a turn that writes the prompt: yasdig
/// puts each look in front of the prompt, so a prompt that describes them
/// says it twice (`specs/assistant.md`, 2026-11-16).
fn cast_added_text(cast: &[(String, String)]) -> Option<String> {
    if cast.is_empty() {
        return None;
    }
    let mut s = String::from(
        "\n\nThe person's cast, picked for this picture. yasdig puts each one's look in front of the prompt \
         itself, so in the prompt call them by name and leave out every detail that repeats their look, \
         including where the current prompt repeats it. Keep what the person wrote that differs from the \
         look or adds to it (\"in a blue shirt\"): that is their change. A detail of the look may come in where \
         the scene touches it (the light on something they wear), never as a description of them. Write what \
         they are doing and what the look leaves open:",
    );
    for (name, look) in cast.iter().take(VARS_MAX) {
        let look: String = look.chars().take(300).collect();
        s.push_str(&format!("\n- {name}: {look}"));
    }
    Some(s)
}

/// What the person said while the session ran, quoted for the turn that
/// takes it up, with what to do about it; empty when they said nothing.
fn steered_text(steers: &[String], what: &str) -> String {
    if steers.is_empty() {
        return String::new();
    }
    let said: Vec<String> = steers.iter().map(|s| format!("- \"{}\"", s.trim())).collect();
    format!("\n\nWhile you worked, the person added:\n{}\n{what}", said.join("\n"))
}

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
                "\n\nThe film is for {}, a {} video model: {}×{}, each shot at most {:.1} s ({} frames at {} fps).",
                v.name,
                v.label,
                v.width,
                v.height,
                v.longest_seconds(),
                v.longest(),
                v.fps
            ));
            if let Some(n) = v.prompt_chars {
                s.push_str(&format!(
                    " A part's words and its scene's `every` together stay under {n} characters: past that \
                     {} ignores the prompt and draws something unrelated.",
                    v.label
                ));
            }
        }
    } else if turn.mode != Mode::Describe {
        if offered.is_empty() {
            s.push_str("\n\nYou may not change any settings in this chat; advise only.");
        } else {
            s.push_str(
                "\n\nSettings you may change, in `change`. Each is shown at the value the next picture is drawn \
                 with: this model's own unless the chat changed it. Change one only when the person asks for it \
                 or a check needs it.",
            );
            if t.image_tab {
                s.push_str(
                    " They are the Image tab's controls, as the person left them; what you change, they see \
                     change there, and your pictures land on its stage.",
                );
            }
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
        // A critique writes the next round's prompt too (`schema`); a sweep's
        // critique writes checks only.
        let next_prompt = matches!(turn.mode, Mode::Chat | Mode::Plan) || (turn.mode == Mode::Critique && !turn.sweep);
        if let Some(cast) = cast_added_text(&t.cast_added).filter(|_| next_prompt && schema::writes_prompt(t)) {
            s.push_str(&cast);
        }
        if matches!(turn.mode, Mode::Chat | Mode::Plan) {
            s.push_str(if schema::writes_prompt(t) {
                "\n\nThe prompt you write in `change.prompt` is exactly what is drawn. Write the whole \
                 prompt there, never in `reply`; the reply says in a sentence what you are drawing."
            } else {
                "\n\nThe prompt is not yours to change; pictures are drawn from the current one."
            });
            if let Some(vars) = variables_text(&t.variables).filter(|_| schema::writes_prompt(t)) {
                s.push_str(&vars);
            }
            if turn.mode == Mode::Chat {
                // On its own, not only as `Now:` at the end of a settings
                // line: `agent-check` had a 26B model read past it there and
                // ask for the prompt instead of comparing on it.
                match t.knob("prompt").and_then(|k| k.value.as_str()).map(str::trim).filter(|p| !p.is_empty()) {
                    Some(p) => s.push_str(&format!(
                        "\n\nThe current prompt, already set — what \"this prompt\", \"it\" or \"the picture\" mean \
                         when the person names no other. Never ask for it:\n{p}"
                    )),
                    None => s.push_str("\n\nThere is no prompt yet."),
                }
                // Named only where it is offered: a Video chat's text stays as
                // it was, and its film cases with it (agent-check, 2026-11-23).
                let rework = if schema::writes_prompt(t) && t.image_tab && t.has_prompt() { ", a rework" } else { "" };
                s.push_str(&format!(
                    "\n\nWhat you do this turn is your `action`, chosen from what the person asked. Start \
                     `notes` by naming it: talk, a drawing, a description, an improvement{rework} or a comparison.\n\
                     - \"talk\": answer, and change settings if asked. \"Give me a prompt for a castle\", \
                     \"what does CFG do?\"\n\
                     - \"draw\": draw once. `count` is how many pictures the person asked for (1 to {}; 1 \
                     when they didn't say). `seeds` \"same\" keeps the last round's compositions, to try a \
                     change on the same pictures; \"new\" draws different ones. \"Draw a fox\", \"three more\", \
                     \"now with a blue sky\".",
                    super::knobs::MAX_BATCH,
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
                if schema::writes_prompt(t) {
                    s.push_str(
                        " `prompt`: the whole prompt its first round would draw, written out; the current one \
                         unchanged when it already asks for everything.",
                    );
                }
                if schema::writes_prompt(t) && t.image_tab && t.has_prompt() {
                    s.push_str(
                        "\n- \"rethink\": the person asks to rework the picture wholesale — a new style, setting or \
                         mood, \"make it completely different\", \"reimagine it\", \"something more dramatic\". Don't \
                         draw: offer 2 to 4 `ideas` (a few words each: what you'd change and how), and in `prompt` \
                         the whole prompt you'd draw from them. The person presses Just do it, or answers. A change \
                         to one thing (a colour, a hat, the light) is \"draw\": the current prompt is theirs, so keep \
                         everything they didn't ask to change.",
                    );
                }
                if t.picks_film_model() {
                    // The tab's model can't make a film: the person picks one
                    // on a card of buttons; the agent's pick is a recommendation.
                    let list: Vec<String> =
                        t.film_pick.iter().map(|v| format!("{} ({}: {})", v.name, v.label, v.about)).collect();
                    s.push_str(&format!(
                        "\n- \"film\": {FILM_ASKS} The Video tab's model can't make one ({why}), so the person picks the video model from buttons first: `video_model` is \
                         the one you recommend for this story, one of {list} — the one the person named, else one \
                         with speech and sound when people talk or sound matters. `reply` says in a sentence which \
                         you recommend and why; don't ask whether to go ahead, the buttons do.",
                        why = t.film_off.as_deref().unwrap_or_default(),
                        list = list.join(", "),
                    ));
                } else if let Some(why) = &t.film_off {
                    s.push_str(&format!(
                        "\n- A film can't be made in this chat: {why}. If the person asks for one, say so and what would fix it."
                    ));
                } else {
                    s.push_str(&format!(
                        "\n- \"film\": {FILM_ASKS} With a storyboard already, \"film\" changes it: \"make shot 2 \
                         at night\", \"add a dog\", \"Leo says something kinder\". The storyboard is written next, \
                         so `reply` only says in one sentence that you are writing it — never the storyboard itself."
                    ));
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
                s.push_str(
                    "\n\nAn improvement or a comparison is offered to the person, who starts it or turns it \
                     down: `reply` asks, in a sentence, whether to go ahead with what you propose.",
                );
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
        s.push_str(&format!(
            "\n\nThis chat's storyboard, as the person sees it on the Video tab now — they may have changed it since \
             you wrote it. They name shots by these numbers; in `reply`, so do you, never by id:\n{}",
            board_text(f, t.video.as_ref().map(|v| v.fps))
        ));
    }
    if let Some(c) = turn.cast.filter(|_| turn.mode == Mode::Chat && t.casts()) {
        let kind = match c.kind {
            MemberKind::Character => "character",
            MemberKind::Place => "place",
            MemberKind::Thing => "thing",
        };
        s.push_str(&format!(
            "\n\nThis chat is making a cast member: {} ({kind}). Look: {}{}\nTo change it, answer action \"cast\" \
             with the whole member rewritten, keeping every word the person didn't ask to change.",
            c.name,
            c.look,
            if turn.cast_shown { "\nThe picture shown is its newest try, not one the person attached." } else { "" }
        ));
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
            Msg::Agent { notes, reply, prompt, did, .. } => {
                // In the order a reply is written (`preserve_order`): notes,
                // what it chose to do, then the rest. Without the action a
                // model never saw that it had drawn or proposed a comparison,
                // and its own history had another shape than its reply.
                let mut v = serde_json::json!({ "notes": notes });
                if let Some(a) = did {
                    v["action"] = serde_json::Value::String(a.clone());
                }
                if let Some(p) = prompt {
                    v["prompt"] = serde_json::Value::String(p.clone());
                }
                v["reply"] = serde_json::Value::String(reply.clone());
                push(&mut out, Role::Assistant, vec![Part::Text { text: v.to_string() }]);
            }
            Msg::Batch { round, summary, images, .. } => {
                let text = format!("[yasdig] Round {round}: {} picture(s) drawn — {summary}", images.len());
                push(&mut out, Role::User, vec![Part::Text { text }]);
            }
            Msg::Note { text, .. } => {
                push(&mut out, Role::User, vec![Part::Text { text: format!("[yasdig] {text}") }]);
            }
            Msg::Clip { id, .. } => {
                let text = format!("[yasdig] The film is done: video {id}, in the Video tab's Current.");
                push(&mut out, Role::User, vec![Part::Text { text }]);
            }
            Msg::Cast { name, try_n, .. } => {
                let text = format!(
                    "[yasdig] Drew {name} for the cast (try {}), into the cast editor: the person saves it, or asks for changes.",
                    try_n + 1
                );
                push(&mut out, Role::User, vec![Part::Text { text }]);
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
            let rolls = if crate::variables::uses_any(prompt) {
                "\nThe prompt has variables, so every picture differs in those words: write checks that hold \
                 whichever word was picked (\"Is the animal wearing a hat?\", not \"Is there a fox?\")."
            } else {
                ""
            };
            let mut parts = reference_parts(
                "The picture to match: the checks are what it shows, and the prompt should draw something like it.",
            );
            parts.push(Part::Text {
                    text: format!(
                        "Goal: {}{}\nThe prompt now: {prompt}\nWrite the checks for this goal. {next}{rolls}\nSet `rounds` and \
                         `count` (pictures per round) from the person's words: \"try 2 rounds of 2\" is 2 and 2. \
                         When they don't say, 3 rounds of 4. At most {} rounds of {}.",
                        turn.goal,
                        steered_text(&chat.session.steers, "Make each thing they point out one of your checks, and \
                            write the prompt to meet it."),
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
            // What each picture's variables rolled to: the prompt says
            // {animal}, and the check is answered about the cat it drew.
            let prompt = chat.settings.get("prompt").and_then(|p| p.as_str()).unwrap_or("");
            let seeds: Vec<Option<i64>> = history
                .iter()
                .rev()
                .find_map(|m| match m {
                    Msg::Batch { round, images, .. } if *round == turn.round => Some(images.iter().map(|i| i.seed).collect()),
                    _ => None,
                })
                .unwrap_or_default();
            for (k, p) in own.iter().enumerate() {
                let rolled: Vec<String> = seeds
                    .get(k)
                    .copied()
                    .flatten()
                    .map(|seed| crate::variables::picks(prompt, &t.variables, seed))
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(name, word)| format!("{name}: {word}"))
                    .collect();
                let drew = if rolled.is_empty() { String::new() } else { format!(" (drawn with {})", rolled.join(", ")) };
                parts.push(Part::Text { text: format!("Picture {}{drew}:", k + 1) });
                parts.push(Part::Image { path: p.clone() });
            }
            let checks: Vec<String> = turn.checks.iter().enumerate().map(|(i, c)| format!("{}. {c}", i + 1)).collect();
            parts.push(Part::Text {
                text: format!(
                    "Answer each check for each picture, in order:\n{}{}",
                    checks.join("\n"),
                    if sweep.is_some() {
                        String::new()
                    } else {
                        steered_text(
                            &chat.session.steers,
                            "Answer the checks above as they are, then put each thing they point out in `add_checks`, \
                             as a yes/no question answered by looking (\"Is the knight holding one shield?\"); those \
                             are judged from the next round. Where they change what a check above asks for (a \
                             sword, not an axe), put that check's number in `drop_checks`, and write `goal` as it \
                             stands now with their change. Act on it now: unless this round already shows it, \
                             change the prompt and draw again, and say in `reply` what you changed.",
                        )
                    }
                ),
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
            // A typical shot, for counting shots off a length the person
            // gives; the longest one may run is `seconds`' cap.
            let longest = t.video.as_ref().map_or(5.0, |v| v.longest_seconds());
            let secs = longest.min(3.0);
            let mut text = String::from("Write the storyboard for a short film, as the person asked.");
            if let Some(saved) = library_text(&t.library) {
                text.push_str(&saved);
            }
            if let Some(f) = turn.film {
                text.push_str(&format!(
                    "\nThe storyboard now, as the person sees it on the Video tab — they may have changed it since you \
                     wrote it, and what they changed stands:\n{}\n\
                     The person names scenes and shots by these numbers (\"shot 2\" is the second shot listed in its \
                     scene); `id` is only for `keep` and for the shot you change. In `reply`, name shots by their number, \
                     never by id. Change only what they asked: every shot you don't change is `keep`, and in a shot you \
                     change, a part you don't is `{{\"keep\": true}}` in its place. A part marked (the person's) is \
                     theirs: keep it unless they asked to change it. A scene you keep gives its `id`.",
                    board_text(f, t.video.as_ref().map(|v| v.fps))
                ));
            }
            text.push_str(&format!(
                "\nA part is one action, and runs about {:.0} s. When the person says how long the film is, \
                 that many seconds is that many parts of it (\"10 seconds\" is {} parts). When they say neither, \
                 4 to 6 parts in all.\n\
                 A shot starts fresh: {} Its later parts go on from the last frame of the part before, so the same \
                 people and place carry on.\n\
                 `cast`: each person, place or thing that must look the same in more than one shot, with its `look` \
                 in words and a `prompt` for its picture: that one subject alone, whole, on a plain background, in \
                 the film's look. A shot's first part's `cast` names who is in it, at most {}. A later part names only \
                 who its first frame shows; start a new shot to bring anyone in. The part before must end with their \
                 faces in view; when someone leaves, turns away or is framed out, start a new shot after it.\n\
                 A scene's `every`: its look in a few words (style, light, colour), put in front of every part of it.\n\
                 A part's `text` says what happens, in the present tense, naming who is in it.\n\
                 `seconds`: how long the part's action takes at a natural pace, at most {:.1}. Add up its \
                 actions: a look or a smile 1-2 s, a step or a turn 2 s, someone walking into frame 2-3 s, kneeling, \
                 sitting down or standing up 2 s, crossing a room 3-4 s. \"She walks into frame and kneels by the \
                 oasis\" is about 5 s. An action longer than that is two parts.",
                secs,
                (10.0 / secs.max(0.1)).round().max(1.0),
                if refs {
                    "it is drawn from the pictures of the cast its first part names, so name everyone in it."
                } else {
                    "it starts from a picture drawn first, with the cast its first part names."
                },
                crate::config::MAX_REFERENCES,
                longest,
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
        Mode::Chat | Mode::Describe | Mode::Take => {}
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
                         keepers: vec![], passed: vec![], changes: vec![], action: None, did: Some("draw".into()), prompt: None, negative: None,
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
        Turn {
            mode,
            images,
            reference: 0,
            sweep: false,
            skills: "SKILLS",
            checks: &[],
            goal: "a fox in a red hat",
            round: 2,
            history: usize::MAX,
            film: None,
            cast: None,
            cast_shown: false,
        }
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
        // Its own turn comes back in the order it was written, action included.
        assert!(matches!(&m[2].content[0], Part::Text { text } if text == r#"{"notes":"need hat","action":"draw","reply":"On it."}"#));
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
        assert!(blind.contains("The current prompt, already set") && blind.contains("Never ask for it:\na fox"));
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

    /// A word pick is named with its look and the rule not to write it; a
    /// chat with none hears nothing of it.
    #[test]
    fn what_the_person_added_mid_session_is_quoted_to_the_critique() {
        let mut c = chat();
        let t = toolkit();
        let text = |c: &Chat| -> String {
            let m = messages(c, &t, &Turn { checks: &["Is there a hat?".to_string()], ..turn(Mode::Critique, vec!["/w/a.png".into()]) });
            m.last().unwrap().content.iter().filter_map(|p| match p { Part::Text { text } => Some(text.clone()), _ => None }).collect()
        };
        assert!(!text(&c).contains("the person added"));
        c.session.steers = vec!["the hat is green, not red".into()];
        let s = text(&c);
        assert!(s.contains("While you worked, the person added:\n- \"the hat is green, not red\"") && s.contains("`add_checks`"), "{s}");
    }

    #[test]
    fn the_picked_cast_is_named_and_its_look_left_to_the_server() {
        let mut t = toolkit();
        assert!(!system(&t, &turn(Mode::Chat, vec![])).contains("cast, picked"));
        t.cast_added = vec![("Elias".into(), "Elias is a weathered cowboy.".into())];
        let s = system(&t, &turn(Mode::Chat, vec![]));
        assert!(s.contains("call them by name and leave out") && s.contains("- Elias: Elias is a weathered cowboy."), "{s}");
        assert!(system(&t, &turn(Mode::Critique, vec![])).contains("- Elias: Elias"), "a critique writes the next prompt");
    }

    /// The agent hears of variables only when there are some, by name with a
    /// few words; a critique is told what each picture rolled.
    #[test]
    fn variables_are_named_to_the_agent_and_their_rolls_to_a_critique() {
        let mut t = toolkit();
        assert!(!system(&t, &turn(Mode::Chat, vec![])).contains("Variables:"));
        t.variables.insert("animal".into(), (0..9).map(|n| format!("beast{n}")).collect());
        let s = system(&t, &turn(Mode::Chat, vec![]));
        assert!(s.contains("- {animal}: beast0, beast1") && s.contains("(+3 more)") && !s.contains("beast7"), "{s}");

        let mut c = chat();
        c.settings["prompt"] = "a {animal} in a hat".into();
        let checks = ["Is the animal wearing a hat?".to_string()];
        let tn = Turn { checks: &checks, round: 1, ..turn(Mode::Critique, vec!["/w/a.png".into(), "/w/b.png".into()]) };
        let m = messages(&c, &t, &tn);
        let text: String = m.last().unwrap().content.iter().filter_map(|p| match p { Part::Text { text } => Some(text.as_str()), _ => None }).collect();
        let word = &crate::variables::picks("a {animal} in a hat", &t.variables, 1)[0].1;
        assert!(text.contains(&format!("Picture 1 (drawn with animal: {word}):")), "{text}");
        let plan = messages(&c, &t, &turn(Mode::Plan, vec![]));
        let text: String = plan.last().unwrap().content.iter().filter_map(|p| match p { Part::Text { text } => Some(text.as_str()), _ => None }).collect();
        assert!(text.contains("whichever word was picked"), "{text}");
    }

    #[test]
    fn a_story_turn_is_shown_the_saved_cast_trimmed() {
        use super::super::toolkit::SavedCast;
        assert_eq!(library_text(&[]), None);
        let long = "Elias is a weathered cowboy. ".repeat(20);
        let lib: Vec<SavedCast> = (0..30)
            .map(|n| SavedCast { id: format!("c{n}"), name: format!("Name{n}"), look: long.clone(), ..Default::default() })
            .collect();
        let s = library_text(&lib).unwrap();
        assert!(s.contains("- Name0 (character): Elias is"));
        assert!(!s.contains("Name24"), "at most {LIBRARY_MAX}");
        assert!(s.lines().all(|l| l.chars().count() < LIBRARY_LOOK_CHARS + 40), "a look is cut short");
    }
}
