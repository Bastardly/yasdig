//! A film shot as MiniMax H3 is prompted: its model card's prompt guide
//! (`skills/h3-prompt-writing/references/base-en.txt` and `ref-en.txt` in
//! github.com/MiniMax-AI/MiniMax-H3). Written by the server from the board's
//! fields, so a small model can't leave a section out: free prose was filled
//! in by H3 itself, with narration and a score nobody asked for.
//!
//! - Text alone or from a first frame: `integrated_multimodal_description`,
//!   `overall_soundscape`, `non_diegetic_music`, one `label: value` each; a
//!   first frame (`-i`, a continue) puts the I2VA instruction line first.
//! - Following the cast's pictures or voices (Ref2VA): six sections, `label:`
//!   and the value on the next line, the people as `<Subject N>` defined
//!   against the `<Picture N>` sd.cpp presents each reference as, in the order
//!   sent, and each voice an `<Audio N>` bound to its speaker's `(Sx)`.
//!
//! sd.cpp encodes H3's prompt whole (`prompt_attn_range {0,0}`), so `(S1)`,
//! `[Shot 1]` and `<d>` reach the encoder as written.

use super::chat::{Film, FilmShot, Line};

/// What stands for "no voice of the cast": an off-screen narrator.
pub const NARRATOR: &str = "narrator";

/// The I2VA instruction: the shot goes on from the frame it is handed.
const FROM_FRAME: &str = "For the target video, at 0.00 seconds into the target video, <Picture 1> (from [Shot 1]) is fully referenced.";

/// Shot `k` of an H3 film. `refs` are the members' names whose pictures are
/// sent as references, in order; `voices` those whose voices are sent
/// (`--ref-audio`), in order; `from_frame` when it goes on from the last
/// frame of the shot before.
pub fn compose(f: &Film, k: usize, refs: &[String], voices: &[String], from_frame: bool) -> String {
    let s = &f.shots[k];
    let ids = speakers(f);
    let sound = or_na(&s.sound);
    let music = or_na(&s.music);
    if refs.is_empty() && voices.is_empty() {
        let body = with_lines(&crate::videos::shot_prompt(&f.every, &s.text), s, &ids, &[], &[]);
        let core =
            format!("integrated_multimodal_description: [Shot 1] {body}\n\noverall_soundscape: {sound}\n\nnon_diegetic_music: {music}");
        return if from_frame { format!("{FROM_FRAME}\n\n{core}") } else { core };
    }
    let mut defs: Vec<String> = refs
        .iter()
        .enumerate()
        .map(|(i, n)| {
            let look = f.member(n).map(|m| m.look.trim().trim_end_matches('.').to_string()).unwrap_or_default();
            let n_ = i + 1;
            if look.is_empty() {
                format!("<Subject {n_}> is {n} in <Picture {n_}>.")
            } else {
                format!("<Subject {n_}> is {n} in <Picture {n_}>: {look}.")
            }
        })
        .collect();
    let mut kept: Vec<String> = refs
        .iter()
        .enumerate()
        .map(|(i, n)| format!("<Subject {}> (appears in [Shot 1]): fully_preserved - {n}'s face, hair, build and clothes are retained.", i + 1))
        .collect();
    // The guide's binding: the speaker's ID, never one of its own, and none
    // in `retention_analysis`.
    for (i, n) in voices.iter().enumerate() {
        let a = i + 1;
        let id = ids.iter().position(|w| *w == n.to_lowercase()).map_or(0, |i| i + 1);
        defs.push(format!("<Audio {a}> is the voice-timbre reference for {} (S{id}).", named(n, refs)));
        kept.push(format!(
            "<Audio {a}>: reference - the target speaker follows <Audio {a}>'s voice timbre without copying the original signal."
        ));
    }
    let task = if voices.is_empty() { "reference generation" } else { "reference generation + audio reference" };
    let text = subjects(&s.text, refs);
    let style = f.every.trim().trim_end_matches('.');
    let opening = if style.is_empty() { String::new() } else { format!("The target video is {style}.\n") };
    let body = with_lines(&text, s, &ids, refs, voices);
    format!(
        "subject_definitions:\n{}\n\nsummary:\n[{task}] {text}\n\nretention_analysis:\n{}\n\n\
         detailed_description:\n{opening}[Shot 1] {body}\n\noverall_soundscape:\n{sound}\n\nnon_diegetic_music:\n{music}",
        defs.join("\n"),
        kept.join("\n"),
    )
}

/// The speakers' IDs, `(S1)` on: by first line in the film, so a voice
/// keeps its ID from shot to shot. Lowercased names.
fn speakers(f: &Film) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for l in f.shots.iter().flat_map(|s| &s.lines) {
        let who = l.who.to_lowercase();
        if !ids.contains(&who) {
            ids.push(who);
        }
    }
    ids
}

/// The shot's words and then its lines, each as the guide writes speech,
/// then who is silent. H3 fills time nobody accounted for: an 8 s shot
/// holding one short line got a made-up sentence at its end. So the speech
/// is closed off (the guide's own examples close the speaker's lips after a
/// line) and each of the shot's cast with no line is said to stay silent.
fn with_lines(text: &str, s: &FilmShot, ids: &[String], refs: &[String], voices: &[String]) -> String {
    let named = |n: &str| named(n, refs);
    let mut out = text.trim().to_string();
    let mut last_on_screen = None;
    for l in &s.lines {
        let words = l.words.trim();
        if words.is_empty() {
            continue;
        }
        let id = ids.iter().position(|w| *w == l.who.to_lowercase()).map_or(0, |i| i + 1);
        let lang = if l.language.trim().is_empty() { "English" } else { l.language.trim() };
        let end = if words.ends_with(['.', '?', '!']) { "" } else { "." };
        let said = format!("<d>[{lang}] {words}{end}</d>");
        let line = if l.who.eq_ignore_ascii_case(NARRATOR) {
            last_on_screen = None;
            format!("An off-screen narrator (S{id}) says in an off-screen voiceover: {said}")
        } else {
            let who = named(&l.who);
            // As the guide's example cites its voice: where the speech is.
            let timbre = voices
                .iter()
                .position(|v| v.eq_ignore_ascii_case(&l.who))
                .map_or(String::new(), |a| format!(" in the voice timbre referenced from <Audio {}>", a + 1));
            if l.voiceover {
                last_on_screen = None;
                format!("{who} (S{id}) says in an off-screen voiceover{timbre}: {said} while {who}'s lips remain completely closed.")
            } else {
                last_on_screen = Some(who.clone());
                format!("{who} (S{id}) says{timbre}: {said}")
            }
        };
        out.push(' ');
        out.push_str(&line);
    }
    let spoke = |n: &str| s.lines.iter().any(|l| l.who.eq_ignore_ascii_case(n) && !l.words.trim().is_empty());
    let quiet: Vec<String> = s.cast.iter().filter(|n| !spoke(n)).map(|n| named(n)).collect();
    if !s.lines.iter().any(|l| !l.words.trim().is_empty()) {
        out.push_str(" No one speaks in this shot: everyone stays silent, lips closed, until it ends.");
        return out;
    }
    if let Some(who) = last_on_screen {
        out.push_str(&format!(" Then {who} closes their lips."));
    }
    match quiet.as_slice() {
        [] => {}
        [one] => out.push_str(&format!(" {one} stays silent throughout, lips closed.")),
        [rest @ .., last] => out.push_str(&format!(" {} and {last} stay silent throughout, lips closed.", rest.join(", "))),
    }
    out.push_str(" Nothing more is said for the rest of the shot.");
    out
}

/// A member as the prompt names them: its `<Subject N>` when pictured.
fn named(n: &str, refs: &[String]) -> String {
    refs.iter().position(|r| r.eq_ignore_ascii_case(n)).map_or(n.to_string(), |i| format!("<Subject {}>", i + 1))
}

/// `N/A`: no music, or — for the soundscape — silence, as the guide says.
fn or_na(s: &str) -> &str {
    let s = s.trim();
    if s.is_empty() { "N/A" } else { s }
}

/// The text with each referenced name, as a whole word, its `<Subject N>`.
fn subjects(text: &str, refs: &[String]) -> String {
    let mut out = text.trim().to_string();
    for (i, n) in refs.iter().enumerate() {
        out = whole_word(&out, n, &format!("<Subject {}>", i + 1));
    }
    out
}

fn whole_word(text: &str, word: &str, with: &str) -> String {
    if word.is_empty() {
        return text.to_string();
    }
    let edge = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find(word) {
        let before = rest[..i].chars().last().or_else(|| out.chars().last());
        let after = rest[i + word.len()..].chars().next();
        out.push_str(&rest[..i]);
        if edge(before) && edge(after) {
            out.push_str(with);
        } else {
            out.push_str(word);
        }
        rest = &rest[i + word.len()..];
    }
    out.push_str(rest);
    out
}

/// The board's lines with every speaker one of the cast (by name, any case)
/// or the narrator, and the words as plain speech: a model that wrote the
/// guide's markup itself has it taken off, since the server writes it.
/// Returns the speakers that are neither.
pub fn clean_lines(names: &[String], lines: &[Line]) -> (Vec<Line>, Vec<String>) {
    let mut out = Vec::new();
    let mut unknown = Vec::new();
    for l in lines {
        let who = l.who.trim();
        let words = l.words.replace("<d>", "").replace("</d>", "");
        let words = words.trim();
        // "[English] Hello" — the tag is ours to write.
        let words = match words.strip_prefix('[').and_then(|w| w.split_once(']')) {
            Some((_, w)) => w.trim(),
            None => words,
        };
        if words.is_empty() {
            continue;
        }
        let who = if who.eq_ignore_ascii_case(NARRATOR) {
            NARRATOR.to_string()
        } else if let Some(n) = names.iter().find(|n| n.eq_ignore_ascii_case(who)) {
            n.clone()
        } else {
            if !who.is_empty() && !unknown.iter().any(|u: &String| u.eq_ignore_ascii_case(who)) {
                unknown.push(who.to_string());
            }
            continue;
        };
        let voiceover = l.voiceover && who != NARRATOR;
        out.push(Line { who, words: words.to_string(), language: l.language.trim().to_string(), voiceover });
    }
    (out, unknown)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::chat::Member;

    fn film() -> Film {
        Film {
            every: "Live-action, cinematic, soft rain light".into(),
            references: true,
            cast: vec![
                Member { name: "Mara".into(), look: "a woman in a red coat.".into(), ..Default::default() },
                Member { name: "Leo".into(), look: "a man in a grey raincoat".into(), ..Default::default() },
            ],
            shots: vec![
                FilmShot {
                    text: "Mara opens the door and sees Leo.".into(),
                    cast: vec!["Mara".into(), "Leo".into()],
                    lines: vec![Line { who: "Leo".into(), words: "You came back".into(), ..Default::default() }],
                    sound: "Rain on the porch roof.".into(),
                    ..Default::default()
                },
                FilmShot {
                    text: "Mara smiles at him.".into(),
                    cast: vec!["Mara".into()],
                    lines: vec![
                        Line { who: "Mara".into(), words: "I never left.".into(), voiceover: true, ..Default::default() },
                        Line { who: "Leo".into(), words: "Hej.".into(), language: "Danish".into(), ..Default::default() },
                    ],
                    music: "Slow solo piano.".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn a_shot_from_words_has_the_three_fields_and_no_score_unasked() {
        let p = compose(&film(), 1, &[], &[], false);
        assert_eq!(
            p,
            "integrated_multimodal_description: [Shot 1] Live-action, cinematic, soft rain light. Mara smiles at him. \
             Mara (S2) says in an off-screen voiceover: <d>[English] I never left.</d> while Mara's lips remain completely closed. \
             Leo (S1) says: <d>[Danish] Hej.</d> Then Leo closes their lips. Nothing more is said for the rest of the shot.\n\n\
             overall_soundscape: N/A\n\n\
             non_diegetic_music: Slow solo piano."
        );
        // Leo spoke first in the film, so he is S1 in every shot.
        let p0 = compose(&film(), 0, &[], &[], true);
        assert!(p0.starts_with(&format!("{FROM_FRAME}\n\nintegrated_multimodal_description: [Shot 1] ")), "{p0}");
        assert!(p0.contains(
            "Leo (S1) says: <d>[English] You came back.</d> Then Leo closes their lips. Mara stays silent throughout, lips closed. \
             Nothing more is said for the rest of the shot.\n\n"
        ), "{p0}");
        assert!(p0.ends_with("non_diegetic_music: N/A"), "no music unless the board asks for it");
    }

    #[test]
    fn a_shot_after_the_casts_pictures_names_them_as_subjects_in_the_order_sent() {
        let p = compose(&film(), 0, &["Mara".into(), "Leo".into()], &[], false);
        assert!(p.starts_with(
            "subject_definitions:\n<Subject 1> is Mara in <Picture 1>: a woman in a red coat.\n\
             <Subject 2> is Leo in <Picture 2>: a man in a grey raincoat.\n\nsummary:\n[reference generation] \
             <Subject 1> opens the door and sees <Subject 2>.\n\nretention_analysis:\n<Subject 1> (appears in [Shot 1]): fully_preserved"
        ), "{p}");
        assert!(p.contains(
            "detailed_description:\nThe target video is Live-action, cinematic, soft rain light.\n[Shot 1] <Subject 1> opens the door \
             and sees <Subject 2>. <Subject 2> (S1) says: <d>[English] You came back.</d> Then <Subject 2> closes their lips. \
             <Subject 1> stays silent throughout, lips closed. Nothing more is said for the rest of the shot.\n\noverall_soundscape:\nRain on the porch roof.\
             \n\nnon_diegetic_music:\nN/A"
        ), "{p}");
    }

    #[test]
    fn a_shot_with_no_lines_says_no_one_speaks() {
        let mut f = film();
        f.shots[1].lines.clear();
        let p = compose(&f, 1, &[], &[], false);
        assert!(p.contains("Mara smiles at him. No one speaks in this shot: everyone stays silent, lips closed, until it ends.\n\n"), "{p}");
    }

    #[test]
    fn a_voice_is_bound_to_its_speakers_id_and_cited_where_they_speak() {
        let p = compose(&film(), 0, &["Mara".into(), "Leo".into()], &["Leo".into()], false);
        assert!(p.contains("<Subject 2> is Leo in <Picture 2>: a man in a grey raincoat.\n<Audio 1> is the voice-timbre reference for <Subject 2> (S1).\n\nsummary:\n[reference generation + audio reference] "), "{p}");
        assert!(p.contains("\n<Audio 1>: reference - the target speaker follows <Audio 1>'s voice timbre"), "{p}");
        assert!(p.contains("<Subject 2> (S1) says in the voice timbre referenced from <Audio 1>: <d>[English] You came back.</d>"), "{p}");
        // A voice alone, nobody pictured, is Ref2VA too.
        let p = compose(&film(), 0, &[], &["Leo".into()], false);
        assert!(p.starts_with("subject_definitions:\n<Audio 1> is the voice-timbre reference for Leo (S1)."), "{p}");
    }

    #[test]
    fn a_name_is_replaced_only_as_a_whole_word() {
        assert_eq!(whole_word("Leo and Leonard saw Leo.", "Leo", "<S>"), "<S> and Leonard saw <S>.");
        assert_eq!(whole_word("Ana's hat", "Ana", "<S>"), "<S>'s hat");
    }

    #[test]
    fn lines_keep_the_casts_names_and_lose_markup_the_model_wrote() {
        let names = vec!["Mara".to_string()];
        let (lines, unknown) = clean_lines(
            &names,
            &[
                Line { who: "mara".into(), words: "<d>[English] Hello!</d>".into(), ..Default::default() },
                Line { who: "Narrator".into(), words: "Once.".into(), voiceover: true, ..Default::default() },
                Line { who: "Bob".into(), words: "Hi".into(), ..Default::default() },
                Line { who: "Mara".into(), words: "  ".into(), ..Default::default() },
            ],
        );
        assert_eq!(lines.len(), 2);
        assert_eq!((lines[0].who.as_str(), lines[0].words.as_str()), ("Mara", "Hello!"));
        assert_eq!((lines[1].who.as_str(), lines[1].voiceover), (NARRATOR, false));
        assert_eq!(unknown, ["Bob"]);
    }
}
