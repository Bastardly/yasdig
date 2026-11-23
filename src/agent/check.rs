//! `yasdig agent-check`: what an agent model can be trusted with.
//!
//! A handful of fixed turns, each through the real system text, skills and
//! reply grammar (`context::messages`, `schema::output`), scored on what the
//! server would act on: the action picked, the count, the knob compared, the
//! checks written, a picture judged. A 4B model that chats well may still
//! propose an improvement for "three castles" or answer every check true —
//! this says so before a session is spent finding out, and it is the
//! regression test for an edit to a skill: run it before and after.
//!
//! The grammar already holds the reply's shape, so nothing here checks JSON;
//! every case is a *choice* the grammar leaves open. Seeds are fixed per
//! case, so two runs of one model on one build agree.
//!
//! Outside the server and off any chat: the toolkit is the default rules on
//! an SDXL-shaped model (a real CFG, a negative prompt), whatever is
//! installed, so two machines' results compare.

use super::chat::{Chat, Film, FilmScene, FilmShot, ImageRef, Member, MemberKind, Mode, Msg};
use super::knobs::{Locks, Rules};
use super::session::{CameraJson, ReplyJson};
use super::enhance::{self, Enhanced, Target};
use super::{agent_for, context, n_predict, schema, session, skills, toolkit, tuning, user_skills_dir, Staged};
use crate::config::{Config, ImageFamily, VideoFamily};
use crate::llm::{self, LlmRunner, Request, Sampling};
use anyhow::Result;
use serde_json::json;

const PROMPT: &str = "a fox, snowy forest, soft morning light, photograph";
/// The same with a prompt variable (`{animal}`: fox, owl, hare) in it.
const VAR_PROMPT: &str = "a {animal}, snowy forest, soft morning light, photograph";

struct Case {
    name: &'static str,
    mode: Mode,
    /// The person's message (a chat turn), or the session's goal (a plan).
    says: &'static str,
    /// A critique: the checks the test picture is judged by.
    checks: &'static [&'static str],
    /// The prompt already in the box, where the case needs its own.
    prompt: Option<&'static str>,
    /// A Video tab chat, where a film can be made (on H3).
    video: bool,
    /// A story turn's storyboard as the person left it (on Wan): the turn
    /// rewrites it.
    film: Option<fn() -> Film>,
    /// What a model that understood wrote, or what was wrong.
    want: fn(&ReplyJson) -> Result<(), String>,
}

/// A request for something that moves, in the person's words: a film, and
/// no "I can't make video" (2026-11-14: all three were answered so).
fn films(r: &ReplyJson) -> Result<(), String> {
    action(r, "film")?;
    let reply = r.reply.to_lowercase();
    match ["can't", "cannot", "can not", "unable"].iter().find(|w| reply.contains(*w)) {
        Some(w) => Err(format!("its reply says \"{w}\": {:?}", r.reply)),
        None => Ok(()),
    }
}

fn action(r: &ReplyJson, want: &str) -> Result<(), String> {
    match r.action.as_deref() {
        Some(a) if a == want => Ok(()),
        a => Err(format!("action {} where \"{want}\" was asked for", a.map_or("none".into(), |a| format!("\"{a}\"")))),
    }
}

fn new_prompt(r: &ReplyJson) -> Option<&str> {
    r.change.as_ref()?.get("prompt")?.as_str()
}

/// The story fixture: a Video whose shot ids have drifted from the numbers
/// the storyboard shows (deleted and redrawn shots), with a shot the person
/// added and left empty at the start of scene 1 — the user's horror film of
/// 2026-11-14, where "shot 1" was answered as "Shot s2".
fn drifted() -> Film {
    let part = |text: &str, scene: usize, shot: &str, cut: bool, cast: &[&str]| FilmShot {
        text: text.into(),
        brief: String::new(),
        scene,
        shot: Some(format!("v-{shot}")),
        cut,
        cast: cast.iter().map(|c| c.to_string()).collect(),
        ..Default::default()
    };
    let member = |name: &str, look: &str| Member { name: name.into(), kind: MemberKind::Character, look: look.into(), ..Default::default() };
    Film {
        title: "Unseen".into(),
        scenes: vec![
            FilmScene { id: "c1".into(), title: "The house".into(), every: "35mm film, dark, cold blue light".into() },
            FilmScene { id: "c2".into(), title: "The living room".into(), every: "35mm film, warm lamp light".into() },
        ],
        cast: vec![
            member("Elias", "a pale man in his late 20s with short messy dark hair"),
            member("Clara", "a woman in her late 20s with long wavy blonde hair"),
        ],
        shots: vec![
            part("", 0, "s9", false, &[]),
            part("Elias presses himself against the wall of a dark hallway, breathing hard.", 0, "s6", true, &["Elias"]),
            part("Elias slides down the wall and covers his mouth with both hands.", 0, "s7", true, &["Elias"]),
            part("Clara sits on the sofa with a radio playing and types a text message.", 1, "s3", true, &["Clara"]),
        ],
        video_model: "wan".into(),
        video: Some("v".into()),
        ..Default::default()
    }
}

/// The same, with a part the person rewrote by hand at length: no story
/// words stand behind it, so the turn is shown it whole.
fn hand_edited() -> Film {
    let mut f = drifted();
    f.shots.remove(0);
    f.shots[0].cut = false;
    f.shots[1].text = "Elias slides slowly down the peeling wallpaper of the hallway until he sits on the dusty floorboards, \
        knees drawn up to his chest, both hands pressed hard over his mouth to keep his breathing silent. His eyes are wide \
        and wet, flicking from the dark doorway at the end of the hall to the staircase beside him. A thin line of cold \
        moonlight from a cracked window falls across his face and the floor. Somewhere above him a floorboard creaks, \
        slowly, then again, closer. He squeezes his eyes shut and holds completely still, his shoulders trembling, the \
        camera holding close on his face as a faint shadow passes over the moonlight on the wall behind him."
        .into();
    f
}

/// A story reply's shots, in order: its `id` (as written), whether it is a
/// `keep`, and its parts' words.
fn story_shots(r: &ReplyJson) -> Vec<(&CameraJson, String)> {
    r.scenes.iter().flatten().flat_map(|s| s.shots.iter()).map(|c| (c, c.id.trim().trim_start_matches("v-").to_lowercase())).collect()
}

/// Every shot id in `ids` is in the reply — kept, or written — and none
/// twice: a shot the turn left out is one the server would remove.
fn none_left_out(r: &ReplyJson, ids: &[&str]) -> Result<(), String> {
    let shots = story_shots(r);
    if shots.is_empty() {
        return Err("no storyboard".into());
    }
    for id in ids {
        match shots.iter().filter(|(_, i)| i == id).count() {
            0 => return Err(format!("shot {id} is left out")),
            1 => {}
            n => return Err(format!("shot {id} is in it {n} times")),
        }
    }
    Ok(())
}

/// Shot `id` of the reply: kept (`keep`, or only parts kept in place —
/// a kept part has no words), or its parts' words joined. Written against
/// fields every build has, so the same file scores HEAD for a baseline.
fn shot_words(r: &ReplyJson, id: &str) -> Option<(bool, String)> {
    story_shots(r).into_iter().find(|(_, i)| i == id).map(|(c, _)| {
        let words = c.parts.iter().map(|p| p.text.trim()).filter(|t| !t.is_empty()).collect::<Vec<_>>().join(" ");
        (c.keep || words.is_empty(), words)
    })
}

/// Words compared as a board shows them: on one line, a long part cut at
/// 400 characters (`context::SHOWN_WORDS`) with its "…".
fn as_shown(s: &str) -> String {
    let one = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let cut: String = one.chars().take(400).collect();
    cut.trim_end().trim_end_matches('…').trim_end_matches("...").trim_end().to_string()
}

/// Shot `id` is kept: `keep`, or its words as shown.
fn kept(r: &ReplyJson, id: &str, shown: &str) -> Result<(), String> {
    match shot_words(r, id) {
        Some((true, _)) => Ok(()),
        Some((false, w)) if as_shown(&w) == as_shown(shown) => Ok(()),
        Some((false, w)) => Err(format!("shot {id} was rewritten, unasked: {w:?}")),
        None => Err(format!("shot {id} is left out")),
    }
}

const CASES: &[Case] = &[
    Case {
        name: "draws when asked to draw",
        mode: Mode::Chat,
        says: "Draw a lighthouse on a cliff in a storm.",
        checks: &[],
        prompt: None,
        video: false,
        film: None,
        want: |r| {
            action(r, "draw")?;
            let p = new_prompt(r).unwrap_or("").to_lowercase();
            if !p.contains("lighthouse") {
                return Err(format!("the prompt it would draw doesn't mention the lighthouse: {p:?}"));
            }
            match r.count {
                Some(1) | None => Ok(()),
                Some(n) => Err(format!("{n} pictures where one was asked for")),
            }
        },
    },
    Case {
        name: "counts the pictures asked for",
        mode: Mode::Chat,
        says: "Give me three pictures of a castle at night.",
        checks: &[],
        prompt: None,
        video: false,
        film: None,
        want: |r| {
            action(r, "draw")?;
            if r.count == Some(3) { Ok(()) } else { Err(format!("count {:?} where 3 were asked for", r.count)) }
        },
    },
    Case {
        name: "answers a question without drawing",
        mode: Mode::Chat,
        says: "What does CFG scale do? Don't draw anything.",
        checks: &[],
        prompt: None,
        video: false,
        film: None,
        want: |r| {
            action(r, "talk")?;
            match new_prompt(r) {
                Some(p) if p.trim() != PROMPT => Err("it rewrote the prompt when only asked a question".into()),
                _ if r.reply.trim().len() < 20 => Err("the answer is empty".into()),
                _ => Ok(()),
            }
        },
    },
    Case {
        name: "proposes a session for \"keep trying\"",
        mode: Mode::Chat,
        says: "Keep trying until the fox is wearing a red knitted hat.",
        checks: &[],
        prompt: None,
        video: false,
        film: None,
        want: |r| {
            action(r, "improve")?;
            let g = r.goal.as_deref().unwrap_or("").to_lowercase();
            if !g.contains("hat") {
                return Err(format!("the goal leaves out the hat: {g:?}"));
            }
            // The card shows it, and *Draw one* draws it.
            let p = r.prompt.as_deref().unwrap_or("").to_lowercase();
            if p.contains("hat") && p.contains("fox") { Ok(()) } else { Err(format!("its first prompt isn't the whole prompt with the hat in it: {p:?}")) }
        },
    },
    Case {
        name: "proposes a comparison for \"which is best\"",
        mode: Mode::Chat,
        says: "Which CFG works best for this prompt? Try a few values.",
        checks: &[],
        prompt: None,
        video: false,
        film: None,
        want: |r| {
            action(r, "compare")?;
            if r.vary.as_deref() != Some("cfg_scale") {
                return Err(format!("it varies {:?}, not cfg_scale", r.vary));
            }
            let values: Vec<f32> = r
                .values
                .iter()
                .flatten()
                .filter_map(|v| v.as_f64().map(|f| f as f32).or_else(|| v.as_str()?.trim().parse().ok()))
                .collect();
            let written = r.values.as_ref().map_or(0, |v| v.len());
            if values.len() < 2 || values.len() != written {
                return Err(format!("its values aren't numbers to compare: {:?}", r.values));
            }
            Ok(())
        },
    },
    Case {
        name: "keeps a prompt variable when it rewrites the prompt",
        mode: Mode::Chat,
        says: "Add a red knitted hat and draw it.",
        checks: &[],
        prompt: Some(VAR_PROMPT),
        video: false,
        film: None,
        want: |r| {
            action(r, "draw")?;
            let p = new_prompt(r).unwrap_or("");
            if !p.to_lowercase().contains("hat") {
                return Err(format!("the prompt it would draw has no hat: {p:?}"));
            }
            // Written out as one animal, every picture is that animal.
            if p.contains("{animal}") { Ok(()) } else { Err(format!("it wrote the variable out, or dropped it: {p:?}")) }
        },
    },
    Case {
        name: "makes a cast member, and says so before it is drawn",
        mode: Mode::Chat,
        says: "Make a character for my cast: Elias, a weathered fisherman in his fifties with a grey beard.",
        checks: &[],
        prompt: None,
        video: false,
        film: None,
        want: |r| {
            action(r, "cast")?;
            let m = r.member.as_ref().ok_or("no member")?;
            if !m.name.to_lowercase().contains("elias") {
                return Err(format!("the member is named {:?}, not Elias", m.name));
            }
            // The reply is written before the picture is.
            let reply = r.reply.to_lowercase();
            match ["i drew", "i've drawn", "i have drawn", "here is", "here's"].iter().find(|w| reply.contains(*w)) {
                Some(w) => Err(format!("its reply says \"{w}\" before anything is drawn: {:?}", r.reply)),
                None => Ok(()),
            }
        },
    },
    Case {
        name: "makes a film when asked for a clip",
        mode: Mode::Chat,
        says: "A 4s clip of a dog stealing a sausage from the kitchen",
        checks: &[],
        prompt: None,
        video: true,
        film: None,
        want: films,
    },
    Case {
        name: "makes a film when asked for a shot",
        mode: Mode::Chat,
        says: "Please create a 4s shot of a dog stealing a sausage from the kitchen",
        checks: &[],
        prompt: None,
        video: true,
        film: None,
        want: films,
    },
    Case {
        name: "makes a film when asked for seconds of something",
        mode: Mode::Chat,
        says: "4 seconds of a dog stealing a sausage from the kitchen",
        checks: &[],
        prompt: None,
        video: true,
        film: None,
        want: films,
    },
    Case {
        name: "changes the shot the person names by its storyboard number",
        mode: Mode::Story,
        // Scene 1 · Shot 2 is s6: s9 (empty) is shot 1.
        says: "In scene 1, make shot 2 calmer: Elias just stands still in the hallway, listening.",
        checks: &[],
        prompt: None,
        video: false,
        film: Some(drifted),
        want: |r| {
            none_left_out(r, &["s9", "s6", "s7", "s3"])?;
            match shot_words(r, "s6") {
                Some((false, w)) if w.to_lowercase().contains("listen") || w.to_lowercase().contains("still") => {}
                Some((_, w)) => return Err(format!("Scene 1 · Shot 2 (s6) wasn't changed as asked: {w:?}")),
                None => return Err("s6 is left out".into()),
            }
            kept(r, "s7", &drifted().shots[2].text)?;
            kept(r, "s3", &drifted().shots[3].text)
        },
    },
    Case {
        name: "fills the empty shot the person added",
        mode: Mode::Story,
        says: "I've added an empty shot at the start of scene 1. Put a short glimpse of the monster in it.",
        checks: &[],
        prompt: None,
        video: false,
        film: Some(drifted),
        want: |r| {
            none_left_out(r, &["s9", "s6", "s7", "s3"])?;
            match shot_words(r, "s9") {
                Some((false, w)) if !w.is_empty() => {}
                _ => return Err("the shot the person added (s9) is still empty".into()),
            }
            if let Some((c, _)) = story_shots(r).into_iter().find(|(_, i)| !["s9", "s6", "s7", "s3"].contains(&i.as_str())) {
                return Err(format!("it made a new shot {:?} instead of filling s9", c.id));
            }
            kept(r, "s6", &drifted().shots[1].text)?;
            kept(r, "s7", &drifted().shots[2].text)
        },
    },
    Case {
        name: "leaves a part the person wrote alone",
        mode: Mode::Story,
        says: "Call the film \"The Hideout\".",
        checks: &[],
        prompt: None,
        video: false,
        film: Some(hand_edited),
        want: |r| {
            none_left_out(r, &["s6", "s7", "s3"])?;
            let f = hand_edited();
            kept(r, "s6", &f.shots[0].text)?;
            kept(r, "s7", &f.shots[1].text)?;
            kept(r, "s3", &f.shots[2].text)?;
            match r.title.as_deref() {
                Some(t) if t.to_lowercase().contains("hideout") => Ok(()),
                t => Err(format!("the title is {t:?}")),
            }
        },
    },
    Case {
        name: "gives a character who talks a voice",
        mode: Mode::Story,
        says: "A short film: at a bus stop at night, Mara tells Leo the last bus has gone, and he laughs.",
        checks: &[],
        prompt: None,
        video: true,
        film: None,
        want: |r| {
            let cast = r.cast.as_deref().unwrap_or_default();
            let mara = cast.iter().find(|m| m.name.to_lowercase().contains("mara")).ok_or("no Mara in the cast")?;
            if mara.speaks.split_whitespace().count() < 2 {
                return Err(format!("Mara talks, and her voice is {:?}", mara.speaks));
            }
            Ok(())
        },
    },
    Case {
        name: "writes checks that can be answered by looking",
        mode: Mode::Plan,
        says: "A fox wearing a red knitted hat in a snowy forest.",
        checks: &[],
        prompt: None,
        video: false,
        film: None,
        want: |r| {
            let checks: Vec<&str> = r.checks.as_ref().and_then(|c| c.as_array()).into_iter().flatten().filter_map(|c| c.as_str()).collect();
            if !(2..=5).contains(&checks.len()) {
                return Err(format!("{} checks where two to five fit the goal", checks.len()));
            }
            if let Some(c) = checks.iter().find(|c| !c.trim_end().ends_with('?')) {
                return Err(format!("a check that isn't a question: {c:?}"));
            }
            if !checks.iter().any(|c| c.to_lowercase().contains("hat")) {
                return Err(format!("no check asks about the hat: {checks:?}"));
            }
            Ok(())
        },
    },
    Case {
        name: "judges a picture by what it shows",
        mode: Mode::Critique,
        says: "A red square and a blue circle on a white background.",
        // The picture has the square alone (`test_picture`).
        checks: &["Is there a red square?", "Is there a blue circle?"],
        prompt: None,
        video: false,
        film: None,
        want: |r| {
            let row: Vec<bool> = r
                .checks
                .as_ref()
                .and_then(|c| c.as_array()?.first()?.as_array().cloned())
                .unwrap_or_default()
                .iter()
                .filter_map(|b| b.as_bool())
                .collect();
            match row.as_slice() {
                [true, false] => Ok(()),
                [true, true] => Err("it said the blue circle is there: it answers from the goal, not the picture".into()),
                other => Err(format!("it answered {other:?}; the picture has a red square and no blue circle")),
            }
        },
    },
];

/// An Enhance case: one box rewritten for a video or image model through the real
/// call (`enhance::system_text`, `messages`, `schema`), scored on the
/// answer. Enhance is where a film's trouble showed (strangers in an empty
/// street, the same man drawn and voiced anew each shot), and until
/// 2026-11-14 nothing measured an edit to its guides.
struct EnhanceCase {
    name: &'static str,
    target: fn() -> Target,
    req: fn() -> enhance::Request,
    want: fn(&Enhanced) -> Result<(), String>,
}

fn video(f: VideoFamily) -> Target {
    Target::Video(f, f.takes_references())
}
fn ltx() -> Target {
    video(VideoFamily::Ltx25)
}
fn wan() -> Target {
    video(VideoFamily::Wan22Ti2v5b)
}
fn h3() -> Target {
    video(VideoFamily::MinimaxH3)
}
fn qwen_image() -> Target {
    Target::Image(crate::config::ImageFamily::QwenImage21)
}

fn part(text: &str) -> enhance::Request {
    enhance::Request { kind: "video".into(), model: "agent-check".into(), text: text.into(), seconds: Some(5.0), ..Default::default() }
}

/// Lower-case words, punctuation off: what "is it in there" compares.
fn plain_words(s: &str) -> Vec<String> {
    s.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_string).collect()
}

/// `phrase` appears in `text`, word for word, punctuation aside.
fn says(text: &str, phrase: &str) -> bool {
    let (t, p) = (plain_words(text).join(" "), plain_words(phrase).join(" "));
    format!(" {t} ").contains(&format!(" {p} "))
}

/// LTX's guide asks for 150–220 words; a little either side is let pass.
fn ltx_length(p: &str) -> Result<(), String> {
    match p.split_whitespace().count() {
        130..=250 => Ok(()),
        n => Err(format!("{n} words, where the guide asks for 150–220")),
    }
}

/// A name means nothing to LTX or Wan: the person is drawn from words.
fn names_no_one(p: &str, names: &[&str]) -> Result<(), String> {
    let w = plain_words(p);
    match names.iter().find(|n| w.contains(&n.to_lowercase())) {
        Some(n) => Err(format!("it names {n}, which the model can't draw")),
        None => Ok(()),
    }
}

const MARA: &str = "Mara is a grey-haired woman in her seventies in a red wool coat.";
const LEO: &str = "Leo is a broad man in his forties with a shaved head and a green work jacket.";

const ENHANCE_CASES: &[EnhanceCase] = &[
    EnhanceCase {
        name: "LTX: speaks the line word for word, and names no one",
        target: ltx,
        req: || enhance::Request {
            cast: vec!["Leo".into(), "Mara".into()],
            looks: vec![("Leo".into(), LEO.into()), ("Mara".into(), MARA.into())],
            lines: vec![crate::videos::Line { who: "Leo".into(), words: "The last bus has gone.".into(), ..Default::default() }],
            ..part("At the kitchen table Leo tells Mara the bus has gone.")
        },
        want: |e| {
            if !says(&e.prompt, "The last bus has gone") {
                return Err(format!("the line isn't in it word for word: {:?}", e.prompt));
            }
            names_no_one(&e.prompt, &["Leo", "Mara"])?;
            ltx_length(&e.prompt)
        },
    },
    EnhanceCase {
        name: "LTX: draws a person from their look",
        target: ltx,
        req: || enhance::Request {
            cast: vec!["Mara".into()],
            looks: vec![("Mara".into(), MARA.into())],
            ..part("Mara waits at a bus stop in the rain.")
        },
        want: |e| {
            names_no_one(&e.prompt, &["Mara"])?;
            for a in ["grey-haired", "red wool coat"] {
                if !says(&e.prompt, a) {
                    return Err(format!("her look loses \"{a}\": {:?}", e.prompt));
                }
            }
            ltx_length(&e.prompt)
        },
    },
    EnhanceCase {
        name: "LTX: an empty place stays empty",
        target: ltx,
        req: || part("An empty kitchen at dawn. A kettle on the stove starts to steam."),
        want: |e| {
            let w = plain_words(&e.prompt);
            let people = ["man", "woman", "person", "people", "someone", "he", "she", "his", "her", "him", "girl", "boy", "hand", "hands", "figure"];
            match people.iter().find(|p| w.contains(&p.to_string())) {
                Some(p) => Err(format!("it put someone in (\"{p}\"): {:?}", e.prompt)),
                None => ltx_length(&e.prompt),
            }
        },
    },
    EnhanceCase {
        name: "LTX: the scene's sound is heard",
        target: ltx,
        req: || enhance::Request {
            sound: "rain on a tin roof, a distant train, no music".into(),
            cast: vec!["Mara".into()],
            looks: vec![("Mara".into(), MARA.into())],
            ..part("Mara stirs her tea by the window.")
        },
        want: |e| {
            for a in ["rain", "tin roof", "train"] {
                if !says(&e.prompt, a) {
                    return Err(format!("the sound loses \"{a}\": {:?}", e.prompt));
                }
            }
            ltx_length(&e.prompt)
        },
    },
    EnhanceCase {
        name: "Wan: the camera move asked for, and only that",
        target: wan,
        req: || part("A slow dolly in on a lighthouse at dusk."),
        want: |e| {
            let w = plain_words(&e.prompt);
            if !w.iter().any(|x| x.starts_with("dolly") || x == "push" || x == "pushes") {
                return Err(format!("the dolly in is gone: {:?}", e.prompt));
            }
            // A dolly moves the camera; a zoom moves the lens. Static is nothing moving.
            match ["zoom", "zooms", "static", "pan", "pans", "orbit", "orbits"].iter().find(|m| w.contains(&m.to_string())) {
                Some(m) => Err(format!("it adds \"{m}\" to the dolly: {:?}", e.prompt)),
                None => Ok(()),
            }
        },
    },
    EnhanceCase {
        name: "H3: no one speaks unless the words say so",
        target: h3,
        req: || enhance::Request {
            cast: vec!["Mara".into()],
            looks: vec![("Mara".into(), MARA.into())],
            ..part("Mara walks along the pier at dusk, watching the boats.")
        },
        want: |e| {
            if !e.spoken.is_empty() {
                return Err(format!("it gave her lines: {:?}", e.spoken));
            }
            if e.sound.trim().is_empty() { Err("no sound line".into()) } else { Ok(()) }
        },
    },
    EnhanceCase {
        name: "H3: a line the words ask for goes into Spoken",
        target: h3,
        req: || enhance::Request {
            cast: vec!["Mara".into(), "Leo".into()],
            looks: vec![("Leo".into(), LEO.into()), ("Mara".into(), MARA.into())],
            ..part("Mara turns to Leo and asks him if he is coming home.")
        },
        want: |e| match e.spoken.iter().find(|l| l.who.to_lowercase().contains("mara")) {
            Some(l) if says(&l.words, "coming") || says(&l.words, "home") => Ok(()),
            Some(l) => Err(format!("Mara's line isn't the question: {:?}", l.words)),
            None => Err(format!("no line of Mara's: {:?}", e.spoken)),
        },
    },
    // LTX 2.5 leads the part with the look itself (`ShotWords::looks`).
    EnhanceCase {
        name: "LTX: a member whose look yasdig adds is named, not described",
        target: ltx,
        req: || mara_added("Mara waits at a bus stop in the rain."),
        want: |e| {
            mara_named_not_described(&e.prompt)?;
            ltx_length_with_look(&e.prompt)
        },
    },
    // H3 writes a referenced member's look into its subject lines itself.
    EnhanceCase {
        name: "H3: a member shown by a reference is named, not described",
        target: h3,
        req: || enhance::Request {
            cast: vec!["Leo".into()],
            looks: vec![("Leo".into(), LEO.into())],
            added: vec!["Leo".into()],
            ..part("Leo waits at the bus stop in the rain and checks his watch.")
        },
        want: |e| {
            if !plain_words(&e.prompt).contains(&"leo".to_string()) {
                return Err(format!("Leo isn't named: {:?}", e.prompt));
            }
            let found: Vec<&str> =
                ["shaved head", "green work jacket", "forties", "broad man"].into_iter().filter(|a| says(&e.prompt, a)).collect();
            if found.len() >= 2 {
                return Err(format!("his look is written out ({}), which H3's subject lines carry: {:?}", found.join(", "), e.prompt));
            }
            Ok(())
        },
    },
    // The Image tab puts a picked member's look in front of the prompt; the
    // box saying it too ran Elias's look twice (2026-11-16).
    EnhanceCase {
        name: "Image: a picked cast member is named, not described",
        target: qwen_image,
        req: || picked("Elias holding a cat"),
        want: |e| image_names_without_look(&e.prompt),
    },
    EnhanceCase {
        name: "Image: the person's change to a picked look stays",
        target: qwen_image,
        req: || picked("Elias in a blue shirt holding a cat"),
        want: |e| {
            image_names_without_look(&e.prompt)?;
            // "a blue cotton shirt" keeps it as well as "a blue shirt" does.
            let w = plain_words(&e.prompt);
            if !(w.contains(&"blue".to_string()) && w.contains(&"shirt".to_string())) {
                return Err(format!("the person's blue shirt is gone: {:?}", e.prompt));
            }
            Ok(())
        },
    },
    EnhanceCase {
        name: "Image: a look the person wrote beside a pick is taken out",
        target: qwen_image,
        req: || picked("Elias, a weathered fisherman with a grey beard, holding a cat on a pier"),
        want: |e| image_names_without_look(&e.prompt),
    },
];

/// The call an Enhance case makes, as `enhance::run` would make it.
const ELIAS: &str = "Elias is a weathered fisherman in his fifties with a grey beard and a yellow oilskin coat.";

/// An Image tab Enhance with Elias picked for his words.
fn picked(text: &str) -> enhance::Request {
    enhance::Request {
        kind: "image".into(),
        model: "agent-check".into(),
        text: text.into(),
        cast: vec!["Elias".into()],
        looks: vec![("Elias".into(), ELIAS.into())],
        added: vec!["Elias".into()],
        ..Default::default()
    }
}

/// Elias by name, the cat kept, his look not written out again. One detail
/// where the scene touches it is wanted ("the cat against his yellow oilskin
/// coat", as Oliver's compass caught the light, 2026-11-16); two or more is
/// the look described.
fn image_names_without_look(p: &str) -> Result<(), String> {
    let w = plain_words(p);
    if !w.contains(&"elias".to_string()) {
        return Err(format!("Elias isn't named: {p:?}"));
    }
    if !w.contains(&"cat".to_string()) {
        return Err(format!("the cat is gone: {p:?}"));
    }
    // Phrases, not words: a weathered pier is no repeat of a weathered face.
    let found: Vec<&str> = ["grey beard", "oilskin", "fifties", "weathered fisherman"].into_iter().filter(|a| says(p, a)).collect();
    if found.len() >= 2 {
        return Err(format!("his look is written out ({}), which yasdig adds itself: {p:?}", found.join(", ")));
    }
    Ok(())
}

fn enhance_request(t: Target, req: &enhance::Request, guide: &str, model: &super::AgentModel, tune: &super::Tuning, seed: u32) -> Request {
    let vars = crate::variables::Variables::new();
    let every = req.every.trim().chars().count() as u32;
    let cap = if matches!(t, Target::Video(f, _) if f.is_ltx()) {
        crate::config::LTX_PROMPT_CHARS.saturating_sub(every + 2).clamp(200, enhance::PROMPT_CHARS)
    } else {
        enhance::PROMPT_CHARS
    };
    Request {
        v: llm::PROTOCOL,
        model: model.lm.clone(),
        mmproj: None,
        n_ctx: tune.n_ctx.min(enhance::N_CTX),
        n_predict: enhance::N_PREDICT,
        thinking: false,
        sampling: Sampling { seed, ..tune.sampling },
        json_schema: Some(enhance::schema(t, cap)),
        messages: enhance::messages(&enhance::system_text(guide, &vars, t), t, req, &req.looks, &[]),
    }
}

/// A pair case: part 1 of a shot, then part 2 told part 1's actual answer
/// (as `enhance_shot` tells it the part before), scored on whether the two
/// agree — what a film's drift is, which one part alone can't show. Run on
/// [`PAIR_SEEDS`] seeds: one seed is thin for a "sometimes".
struct PairCase {
    name: &'static str,
    family: VideoFamily,
    first: fn() -> enhance::Request,
    second: fn() -> enhance::Request,
    want: fn(&Enhanced, &Enhanced) -> Result<(), String>,
}

const PAIR_SEEDS: u32 = 3;

/// An LTX 2.5 part whose shot carries Mara's look, as a part given her by
/// *Add cast…* is enhanced.
fn mara_added(text: &str) -> enhance::Request {
    enhance::Request {
        cast: vec!["Mara".into()],
        looks: vec![("Mara".into(), MARA.into())],
        added: vec!["Mara".into()],
        ..part(text)
    }
}

/// Mara by name; at most one detail of her look, where the scene touches it.
fn mara_named_not_described(p: &str) -> Result<(), String> {
    if !plain_words(p).contains(&"mara".to_string()) {
        return Err(format!("Mara isn't named, which her added look is written under: {p:?}"));
    }
    let found: Vec<&str> = ["grey-haired", "red wool coat", "seventies"].into_iter().filter(|a| says(p, a)).collect();
    if found.len() >= 2 {
        return Err(format!("her look is written out ({}), which yasdig adds itself: {p:?}", found.join(", ")));
    }
    Ok(())
}

/// The look ahead of it takes some of LTX's room, so the part may run shorter
/// than the guide's 150–220 words; still past a sentence or two.
fn ltx_length_with_look(p: &str) -> Result<(), String> {
    match p.split_whitespace().count() {
        60..=250 => Ok(()),
        n => Err(format!("{n} words, where the guide asks for 150–220 (less with a look ahead of it)")),
    }
}

/// Leo as a film's cast holds him, told to Enhance as `enhance_shot` tells
/// it (`Member::look_and_voice`): his voice must reach both parts word for word.
fn leo(text: &str, line: &str) -> enhance::Request {
    let m = Member { name: "Leo".into(), look: LEO.into(), speaks: "a low, tired voice".into(), ..Default::default() };
    enhance::Request {
        cast: vec!["Leo".into()],
        looks: vec![("Leo".into(), m.look_and_voice())],
        lines: vec![crate::videos::Line { who: "Leo".into(), words: line.into(), ..Default::default() }],
        ..part(text)
    }
}

const PAIR_CASES: &[PairCase] = &[
    PairCase {
        name: "LTX: one man, the same look and voice in both parts",
        family: VideoFamily::Ltx25,
        first: || enhance::Request { then_continues: true, ..leo("Leo sits at the kitchen table and says the last bus has gone.", "The last bus has gone.") },
        second: || leo("Leo stands up and says they will walk.", "Then we walk."),
        want: |a, b| {
            for (k, e) in [a, b].into_iter().enumerate() {
                for w in ["shaved head", "green work jacket", "low, tired voice"] {
                    if !says(&e.prompt, w) {
                        return Err(format!("part {} loses \"{w}\": {:?}", k + 1, e.prompt));
                    }
                }
            }
            Ok(())
        },
    },
    PairCase {
        name: "LTX: both parts name Mara when yasdig adds her look",
        family: VideoFamily::Ltx25,
        first: || enhance::Request { then_continues: true, ..mara_added("Mara steps out of the house into the rain.") },
        second: || mara_added("Mara looks up at the sky."),
        want: |a, b| {
            for (k, e) in [a, b].into_iter().enumerate() {
                mara_named_not_described(&e.prompt).map_err(|why| format!("part {}: {why}", k + 1))?;
            }
            Ok(())
        },
    },
    PairCase {
        name: "LTX: the rain of part 1 goes on in part 2",
        family: VideoFamily::Ltx25,
        first: || enhance::Request {
            then_continues: true,
            cast: vec!["Mara".into()],
            looks: vec![("Mara".into(), MARA.into())],
            ..part("Rain drums on the tin roof of a bus shelter at night. Mara waits inside it, shivering.")
        },
        second: || enhance::Request { cast: vec!["Mara".into()], looks: vec![("Mara".into(), MARA.into())], ..part("Mara checks her watch.") },
        want: |_, b| if says(&b.prompt, "rain") { Ok(()) } else { Err(format!("part 2 has no rain: {:?}", b.prompt)) },
    },
];

/// A red square on white, 512², for the critique: one check true, one false.
fn test_picture() -> Result<Vec<u8>> {
    let img = image::RgbImage::from_fn(512, 512, |x, y| {
        if (156..356).contains(&x) && (156..356).contains(&y) { image::Rgb([220, 30, 30]) } else { image::Rgb([255, 255, 255]) }
    });
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)?;
    Ok(png)
}

/// The chat a case's turn is asked in: a prompt already set, and for a
/// critique the round it judges.
fn chat_for(case: &Case) -> Chat {
    let mut chat = Chat::new("agent-check".into(), json!({ "prompt": case.prompt.unwrap_or(PROMPT), "cfg_scale": 6.0 }), None);
    if case.video || case.film.is_some() {
        chat.context = super::chat::Pane::Video;
    }
    chat.film = case.film.map(|f| f());
    match case.mode {
        Mode::Chat | Mode::Story => chat.messages.push(Msg::User { n: 0, ms: 0, text: case.says.into(), images: vec![] }),
        Mode::Critique => {
            chat.session.goal = case.says.into();
            chat.session.checks = case.checks.iter().map(|c| c.to_string()).collect();
            chat.session.round = 1;
            let image = ImageRef { kind: "item".into(), id: "check".into(), file: "check.png".into(), seed: Some(1) };
            chat.messages.push(Msg::Batch {
                n: 0,
                ms: 0,
                round: 1,
                settings_rev: 0,
                summary: format!("prompt '{}'", case.says),
                size: 1,
                images: vec![image],
            });
        }
        _ => chat.session.goal = case.says.into(),
    }
    chat
}

pub fn run(cfg: &Config, name: Option<&str>) -> Result<()> {
    let model = agent_for(cfg, name)?;
    let tune = tuning(cfg, &model);
    println!(
        "[agent-check] {} ({}), {} cases",
        model.label,
        if model.vision() { "sees pictures" } else { "text only" },
        CASES.len() + ENHANCE_CASES.len() + PAIR_CASES.len()
    );
    let (all_skills, _) = skills::load(&user_skills_dir(cfg));
    let family = ImageFamily::Sdxl;
    // One per case: what a turn is offered follows the prompt in the box, the
    // person's variables, and — for a cast member — an image model to draw with.
    let toolkit_for = |case: &Case| {
        let settings = json!({ "prompt": case.prompt.unwrap_or(PROMPT), "cfg_scale": 6.0 });
        let models = if case.name.contains("cast member") {
            vec![toolkit::ModelChoice { name: "sdxl".into(), family: family.as_str(), label: family.label(), references: false, locks: Locks::default() }]
        } else {
            vec![]
        };
        let mut tk = toolkit::build(&Rules::default(), &settings, &Locks::default(), models, Some(family));
        tk.vision = model.vision();
        if case.video {
            let h3 = toolkit::VideoView {
                name: "h3".into(),
                family: "minimax_h3",
                label: "MiniMax H3",
                chains: true,
                about: "speech and sound; follows the cast's pictures",
                prompt_chars: None,
                references: true,
                sounds: true,
                width: 832,
                height: 480,
                frames: 97,
                fps: 24,
                longest_secs: 5.0,
            };
            tk.videos = vec![h3.clone()];
            tk.video = Some(h3);
            tk.film_off = None;
        }
        if case.film.is_some() {
            let wan = toolkit::VideoView {
                name: "wan".into(),
                family: "wan22_ti2v_5b",
                label: "Wan 2.2 TI2V 5B",
                chains: true,
                about: "silent; each shot from its first frame",
                prompt_chars: None,
                references: false,
                sounds: false,
                width: 832,
                height: 480,
                frames: 121,
                fps: 24,
                longest_secs: 5.0,
            };
            tk.videos = vec![wan.clone()];
            tk.video = Some(wan);
            tk.film_off = None;
        }
        if case.prompt.is_some() {
            tk.variables.insert("animal".into(), vec!["fox".into(), "owl".into(), "hare".into()]);
        }
        tk
    };

    let dir = crate::pipeline::scratch_dir(&cfg.base_dir.join("work"));
    std::fs::create_dir_all(&dir)?;
    let monitor = crate::sd::Monitor::default();
    let runner = LlmRunner::watched(cfg, &monitor);
    let (mut passed, mut ran) = (0, 0);
    for (k, case) in CASES.iter().enumerate() {
        let mut staged = Staged(Vec::new());
        if case.mode == Mode::Critique {
            if !model.vision() {
                println!("[agent-check] skip  {}: this model has no vision projector, so it can't run sessions", case.name);
                continue;
            }
            let p = dir.join("agent-check.png");
            std::fs::write(&p, super::shrink_png(&test_picture()?, tune.image_side)?)?;
            staged.0.push(p);
        }
        let chat = chat_for(case);
        let tk = toolkit_for(case);
        // A storyboard is written for the video model too (`agent::turn`).
        let families: Vec<&str> = std::iter::once(family.as_str()).chain(tk.video.as_ref().map(|v| v.family).filter(|_| case.mode == Mode::Story)).collect();
        let (skill_text, _, _) = skills::select(&all_skills, &families, case.mode);
        let turn = context::Turn {
            mode: case.mode,
            images: staged.0.clone(),
            reference: 0,
            sweep: false,
            skills: &skill_text,
            checks: &chat.session.checks,
            goal: &chat.session.goal,
            round: chat.session.round,
            history: usize::MAX,
            film: chat.film.as_ref(),
            cast: None,
            cast_shown: false,
        };
        let req = Request {
            v: llm::PROTOCOL,
            model: model.lm.clone(),
            mmproj: model.mmproj.clone().filter(|_| !staged.0.is_empty()),
            n_ctx: tune.n_ctx,
            n_predict: n_predict(case.mode),
            thinking: false,
            // Fixed per case: the same model on the same build scores the same.
            sampling: Sampling { seed: 1 + k as u32, ..tune.sampling },
            json_schema: Some(schema::output(case.mode, &tk, staged.0.len() as u32, case.checks.len() as u32, false)),
            messages: context::messages(&chat, &tk, &turn),
        };
        ran += 1;
        let verdict = runner.run(&req, &model.label, "agent-check").map_err(|e| format!("the turn failed: {e:#}")).and_then(|reply| {
            if reply.stats.stop == "length" {
                return Err("the reply was cut off".to_string());
            }
            let r = session::parse_reply(&reply.content).map_err(|e| format!("{e:#}"))?;
            (case.want)(&r).map_err(|why| format!("{why}\n                    notes: {}", r.notes.trim()))
        });
        match verdict {
            Ok(()) => {
                passed += 1;
                println!("[agent-check] ok    {}", case.name);
            }
            Err(why) => println!("[agent-check] FAIL  {}\n                    {why}", case.name),
        }
    }
    for (k, case) in ENHANCE_CASES.iter().enumerate() {
        let t = (case.target)();
        let (guide, used, _) = skills::select_enhance(&all_skills, t.family());
        if used.is_empty() {
            println!("[agent-check] skip  {}: no enhance guide for {}", case.name, t.label());
            continue;
        }
        ran += 1;
        let req = enhance_request(t, &(case.req)(), &guide, &model, &tune, 101 + k as u32);
        let verdict = runner.run(&req, &model.label, "agent-check").map_err(|e| format!("the call failed: {e:#}")).and_then(|reply| {
            if reply.stats.stop == "length" {
                return Err("the answer was cut off".to_string());
            }
            let e = enhance::parse(&reply.content).map_err(|e| format!("{e:#}"))?;
            (case.want)(&e)
        });
        match verdict {
            Ok(()) => {
                passed += 1;
                println!("[agent-check] ok    {}", case.name);
            }
            Err(why) => println!("[agent-check] FAIL  {}\n                    {why}", case.name),
        }
    }
    let enhanced = |req: &Request| -> Result<Enhanced, String> {
        let reply = runner.run(req, &model.label, "agent-check").map_err(|e| format!("the call failed: {e:#}"))?;
        if reply.stats.stop == "length" {
            return Err("the answer was cut off".to_string());
        }
        enhance::parse(&reply.content).map_err(|e| format!("{e:#}"))
    };
    for (k, case) in PAIR_CASES.iter().enumerate() {
        let (guide, used, _) = skills::select_enhance(&all_skills, case.family.as_str());
        if used.is_empty() {
            println!("[agent-check] skip  {}: no enhance guide for {}", case.name, case.family.label());
            continue;
        }
        ran += 1;
        let mut fails = Vec::new();
        for n in 0..PAIR_SEEDS {
            let seed = 201 + (k as u32) * 10 + n;
            let verdict = enhanced(&enhance_request(video(case.family), &(case.first)(), &guide, &model, &tune, seed)).and_then(|a| {
                let second = enhance::Request { before: a.prompt.clone(), first: a.prompt.clone(), ..(case.second)() };
                let b = enhanced(&enhance_request(video(case.family), &second, &guide, &model, &tune, seed))?;
                (case.want)(&a, &b)
            });
            if let Err(why) = verdict {
                fails.push(format!("seed {seed}: {why}"));
            }
        }
        if fails.is_empty() {
            passed += 1;
            println!("[agent-check] ok    {} ({PAIR_SEEDS} of {PAIR_SEEDS} seeds)", case.name);
        } else {
            println!(
                "[agent-check] FAIL  {} ({} of {PAIR_SEEDS} seeds)\n                    {}",
                case.name,
                PAIR_SEEDS as usize - fails.len(),
                fails.join("\n                    ")
            );
        }
    }
    println!("[agent-check] {passed} of {ran} passed");
    if passed < ran {
        println!(
            "[agent-check] A failed case is a choice this model gets wrong on this build's instructions, not a broken install \
             (`llm-selftest` checks that). The chat turns come first (three of them a Video tab's films), then three storyboard \
             rewrites, the two a session needs, Enhance on LTX, Wan and H3, and last two parts in a row on LTX, which must agree."
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every case builds its turn — the messages and the schema — with no
    /// model, and its picture is a PNG; and each expectation refuses an
    /// empty reply, so none passes by default.
    #[test]
    fn every_case_builds_its_turn_and_refuses_an_empty_reply() {
        let tk = toolkit::build(&Rules::default(), &json!({ "prompt": PROMPT }), &Locks::default(), vec![], Some(ImageFamily::Sdxl));
        for case in CASES {
            let chat = chat_for(case);
            let images: Vec<std::path::PathBuf> = if case.mode == Mode::Critique { vec!["/w/check.png".into()] } else { vec![] };
            let turn = context::Turn {
                mode: case.mode,
                images: images.clone(),
                reference: 0,
                sweep: false,
                skills: "",
                checks: &chat.session.checks,
                goal: &chat.session.goal,
                round: chat.session.round,
                history: usize::MAX,
                film: chat.film.as_ref(),
                cast: None,
                cast_shown: false,
            };
            let m = context::messages(&chat, &tk, &turn);
            assert_eq!(m.last().unwrap().role, llm::Role::User, "{}", case.name);
            let s = schema::output(case.mode, &tk, images.len() as u32, case.checks.len() as u32, false);
            assert!(s.is_object(), "{}", case.name);
            assert!((case.want)(&ReplyJson::default()).is_err(), "{} passes an empty reply", case.name);
        }
        // What the chat cases ask for is on offer under the default rules.
        let chat = schema::output(Mode::Chat, &tk, 0, 0, false).to_string();
        for a in ["\"talk\"", "\"draw\"", "\"improve\"", "\"compare\"", "cfg_scale"] {
            assert!(chat.contains(a), "{a} is not offered");
        }
        assert!(image::load_from_memory(&test_picture().unwrap()).is_ok());
    }

    /// Every Enhance case finds its family's guide, builds its call, and
    /// refuses an empty answer; the scoring reads words, not characters.
    #[test]
    fn every_enhance_case_builds_its_call_and_refuses_an_empty_answer() {
        let (all, _) = skills::load(std::path::Path::new("/nonexistent"));
        for case in ENHANCE_CASES {
            let t = (case.target)();
            let (guide, used, _) = skills::select_enhance(&all, t.family());
            assert!(!used.is_empty(), "{}: no guide", case.name);
            let req = (case.req)();
            let m = enhance::messages(&enhance::system_text(&guide, &Default::default(), t), t, &req, &req.looks, &[]);
            assert_eq!(m.len(), 2, "{}", case.name);
            assert!((case.want)(&Enhanced::default()).is_err(), "{} passes an empty answer", case.name);
        }
        for case in PAIR_CASES {
            let (a, b) = ((case.first)(), (case.second)());
            assert!(!a.text.is_empty() && !b.text.is_empty(), "{}", case.name);
            assert!((case.want)(&Enhanced::default(), &Enhanced::default()).is_err(), "{} passes empty answers", case.name);
        }
        assert!(says("He says, \"The last bus has gone!\" quietly.", "The last bus has gone."));
        assert!(!says("the lastbus has gone", "last bus"));
        assert!(names_no_one("a man in a coat", &["Leo"]).is_ok() && names_no_one("Leo's coat", &["Leo"]).is_err());
        // Gemma 26B's answers of 2026-11-16: one detail where the cat touches him is no re-description.
        assert!(image_names_without_look("Elias cradles the cat against his yellow oilskin coat on the weathered planks of a pier.").is_ok());
        assert!(image_names_without_look("Elias, a weathered fisherman with a grey beard, holds a cat.").is_err());
        assert!(image_names_without_look("A man holds a cat.").is_err(), "Elias must be named");
    }
}
