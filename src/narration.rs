//! Narrated stills: a clip of pictures held while a voice reads each shot's
//! words, spoken by a text-to-speech model through the agent's driver
//! ([`crate::llm::LlmRunner::speak`]).
//!
//! This module is the pure part — how a shot's words are cut into pieces the
//! model reads well, and how the pieces' WAVs are laid end to end so that the
//! sound and the frames agree to the sample. The engine side is
//! `Engine::narrate` in `pipeline.rs`.
//!
//! **Why pieces**: a TTS backbone generates codec frames up to a cap per input
//! ([`MAX_PIECE_FRAMES`]), and a long input drifts
//! — speed and pitch wander the further it reads from the speaker reference.
//! So a shot's words are cut at sentence ends into stretches of about 20 s,
//! each read on its own with the same voice and seed, and joined with a short
//! breath of silence.

use crate::avi::Audio;
use anyhow::{bail, Result};

/// What a voice model reads when its file lists no languages: the codes
/// llama.cpp's Qwen3-TTS pipeline maps to its language tokens
/// (`tts_lang_codes`, `tools/mtmd/mtmd-helper-gen.cpp`) — Qwen3-TTS's GGUF
/// is the one that lists none. A file that does (`general.languages`,
/// Chatterbox's) is taken at its word ([`crate::voices::VoiceModel`]).
pub const QWEN_LANGUAGES: &[&str] = &["en", "de", "fr", "es", "it", "pt", "ru", "zh", "ja", "ko"];

/// A name for every language code a voice model in the catalog reads: the
/// union of Qwen3-TTS's ten and Chatterbox Multilingual's 23. A code a file
/// lists beyond these is offered under the code itself.
pub const NAMES: &[(&str, &str)] = &[
    ("ar", "Arabic"),
    ("da", "Danish"),
    ("de", "German"),
    ("el", "Greek"),
    ("en", "English"),
    ("es", "Spanish"),
    ("fi", "Finnish"),
    ("fr", "French"),
    ("he", "Hebrew"),
    ("hi", "Hindi"),
    ("it", "Italian"),
    ("ja", "Japanese"),
    ("ko", "Korean"),
    ("ms", "Malay"),
    ("nl", "Dutch"),
    ("no", "Norwegian"),
    ("pl", "Polish"),
    ("pt", "Portuguese"),
    ("ru", "Russian"),
    ("sv", "Swedish"),
    ("sw", "Swahili"),
    ("tr", "Turkish"),
    ("zh", "Chinese"),
];

/// Whether `code` is a well-formed language code (`en`, `pt-br`): what a
/// clip's record may hold. Whether *its* voice model reads it is checked when
/// it speaks, against [`crate::voices::VoiceModel::languages`].
pub fn is_code(code: &str) -> bool {
    (2..=8).contains(&code.len()) && code.chars().all(|c| c.is_ascii_lowercase() || c == '-')
}

/// A voice model's languages as the Language select shows them, `(code,
/// name)`: English first (what an unset language reads in), the rest by name.
pub fn offered(codes: &[String]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = codes
        .iter()
        .map(|c| {
            let name = NAMES.iter().find(|(k, _)| k == c).map_or(c.as_str(), |(_, n)| n);
            (c.clone(), name.to_string())
        })
        .collect();
    out.sort_by(|a, b| (a.0 != "en", &a.1).cmp(&(b.0 != "en", &b.1)));
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

/// The most codec frames one piece may run to, sent as the driver's
/// `n_predict`: a guard against a reading that never stops, not a length. A
/// [`PIECE_CHARS`] piece is ~20 s; this is a minute at Chatterbox's 25 frames
/// a second and two at Qwen3-TTS's 12. The driver's own default (512) would
/// cut a slow Chatterbox reading of a full piece at 20 s.
pub const MAX_PIECE_FRAMES: u32 = 1500;

/// The longest piece, in characters: about 20 s of English read at an
/// ordinary pace (~15 characters a second), well under the frame cap.
pub const PIECE_CHARS: usize = 300;

/// Silence between two pieces of one shot: a breath, not a pause.
pub const PIECE_GAP: f32 = 0.25;

/// Silence at the end of every shot, before the next picture: long enough to
/// read as a change of subject.
pub const SHOT_GAP: f32 = 0.5;

/// Cut `text` into pieces of at most [`PIECE_CHARS`] characters: whole
/// sentences where they fit, then clauses (`;`, `:`, `,`), then words — never
/// inside a word. A word longer than the cap is a piece of its own. Blank
/// text is no pieces.
pub fn pieces(text: &str) -> Vec<String> {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = Vec::new();
    let mut cur = String::new();
    for sentence in split_after(&text, &['.', '!', '?', '…']) {
        for part in fit(&sentence) {
            if !cur.is_empty() && cur.chars().count() + 1 + part.chars().count() > PIECE_CHARS {
                out.push(std::mem::take(&mut cur));
            }
            if !cur.is_empty() {
                cur.push(' ');
            }
            cur.push_str(&part);
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// A sentence as parts no longer than the cap: itself, else its clauses,
/// else runs of words.
fn fit(sentence: &str) -> Vec<String> {
    if sentence.chars().count() <= PIECE_CHARS {
        return vec![sentence.to_string()];
    }
    let mut out = Vec::new();
    for clause in split_after(sentence, &[';', ':', ',']) {
        if clause.chars().count() <= PIECE_CHARS {
            out.push(clause);
            continue;
        }
        let mut run = String::new();
        for word in clause.split(' ') {
            if !run.is_empty() && run.chars().count() + 1 + word.chars().count() > PIECE_CHARS {
                out.push(std::mem::take(&mut run));
            }
            if !run.is_empty() {
                run.push(' ');
            }
            run.push_str(word);
        }
        if !run.is_empty() {
            out.push(run);
        }
    }
    out
}

/// Split after any of `marks` followed by a space (closing quotes and
/// brackets stay with their sentence), trimmed, empties dropped.
fn split_after(text: &str, marks: &[char]) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < chars.len() {
        if marks.contains(&chars[i]) {
            let mut end = i + 1;
            while end < chars.len() && (marks.contains(&chars[end]) || "\"'”’)]»".contains(chars[end])) {
                end += 1;
            }
            if end == chars.len() || chars[end] == ' ' {
                out.push(chars[start..end].iter().collect::<String>());
                start = end;
            }
            i = end;
        } else {
            i += 1;
        }
    }
    out.push(chars[start..].iter().collect());
    out.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}

/// Lay the shots' pieces end to end: the pieces of a shot with [`PIECE_GAP`]
/// between them, [`SHOT_GAP`] after the shot, and each shot padded out to a
/// whole number of frames at `fps` — so the picture changes exactly where the
/// next shot's sound begins. `start` is the frame the first shot begins at
/// (an extended clip's length so far): the padding is measured from the
/// clip's start, so rounding never accumulates over many shots.
///
/// Returns the sound and each shot's frame count. Every piece must share one
/// sample rate and channel count — they come from one model.
pub fn lay(shots: &[Vec<Audio>], fps: u32, start: u32) -> Result<(Audio, Vec<u32>)> {
    let fps = u64::from(fps.max(1));
    let Some(first) = shots.iter().flatten().next() else {
        bail!("there is nothing to lay: no shot has any sound");
    };
    let (rate, channels) = (first.sample_rate, first.channels);
    if rate == 0 || channels == 0 {
        bail!("the voice model's sound has no sample rate");
    }
    let align = usize::from(channels) * 2;
    let silence = |seconds: f32| vec![0u8; (seconds * rate as f32).round() as usize * align];
    // Samples from the clip's start to frame `f`.
    let at = |f: u64| (f * u64::from(rate) + fps / 2) / fps;

    let mut pcm = Vec::new();
    let mut frames = Vec::with_capacity(shots.len());
    let mut done = u64::from(start);
    for (k, shot) in shots.iter().enumerate() {
        if shot.is_empty() {
            bail!("shot {} has no sound", k + 1);
        }
        let begin = pcm.len();
        for (i, piece) in shot.iter().enumerate() {
            if (piece.sample_rate, piece.channels) != (rate, channels) {
                bail!(
                    "shot {}'s sound is {} Hz × {} where the rest is {rate} Hz × {channels}",
                    k + 1,
                    piece.sample_rate,
                    piece.channels
                );
            }
            if i > 0 {
                pcm.extend(silence(PIECE_GAP));
            }
            pcm.extend_from_slice(&piece.pcm[..piece.pcm.len() / align * align]);
        }
        pcm.extend(silence(SHOT_GAP));
        let samples = ((pcm.len() - begin) / align) as u64;
        // The fewest frames whose span holds every sample, then the span padded.
        let from = at(done);
        let mut n = 1;
        while at(done + n) - from < samples {
            n += 1;
        }
        let want = (at(done + n) - from) as usize * align;
        pcm.resize(begin + want, 0);
        frames.push(n as u32);
        done += n;
    }
    Ok((Audio { sample_rate: rate, channels, pcm }, frames))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(seconds: f32, rate: u32) -> Audio {
        let n = (seconds * rate as f32) as usize;
        Audio { sample_rate: rate, channels: 1, pcm: vec![1u8; n * 2] }
    }

    #[test]
    fn languages_are_offered_english_first_then_by_name() {
        let codes: Vec<String> = ["sv", "en", "da", "xx", "en"].iter().map(|c| c.to_string()).collect();
        let got = offered(&codes);
        let names: Vec<&str> = got.iter().map(|(_, n)| n.as_str()).collect();
        assert_eq!(names, ["English", "Danish", "Swedish", "xx"], "an unnamed code is offered as itself, once");
        for code in QWEN_LANGUAGES {
            assert!(NAMES.iter().any(|(c, _)| c == code), "{code} has a name");
        }
        assert!(is_code("en") && is_code("pt-br"));
        assert!(!is_code("EN") && !is_code("e") && !is_code("en/../x"));
    }

    #[test]
    fn short_text_is_one_piece_and_blank_is_none() {
        assert_eq!(pieces("  Hello   there.\nHow are you?  "), vec!["Hello there. How are you?"]);
        assert!(pieces(" \n ").is_empty());
    }

    #[test]
    fn long_text_is_cut_at_sentence_ends_under_the_cap() {
        let sentence = "This sentence is about sixty characters long, give or take. ";
        let text = sentence.repeat(12);
        let p = pieces(&text);
        assert!(p.len() > 1, "{p:?}");
        for piece in &p {
            assert!(piece.chars().count() <= PIECE_CHARS, "{piece}");
            assert!(piece.ends_with('.'), "cut inside a sentence: {piece}");
        }
        // Nothing lost, nothing added.
        assert_eq!(p.join(" "), text.trim());
    }

    #[test]
    fn a_sentence_past_the_cap_is_cut_at_clauses_then_words() {
        let clause = "and then the long road wound on through the hills, ";
        let text = format!("{}the end.", clause.repeat(10));
        let p = pieces(&text);
        assert!(p.len() > 1);
        for piece in &p {
            assert!(piece.chars().count() <= PIECE_CHARS, "{piece}");
        }
        assert_eq!(p.join(" "), text.trim());
        let words = "word ".repeat(200);
        let p = pieces(&words);
        assert!(p.iter().all(|s| s.chars().count() <= PIECE_CHARS && !s.contains("wor ")));
        assert_eq!(p.join(" "), words.trim());
    }

    #[test]
    fn quotes_stay_with_their_sentence() {
        let p = split_after("He said \"stop.\" Then he left. Dr.Who stays.", &['.']);
        assert_eq!(p, vec!["He said \"stop.\"", "Then he left.", "Dr.Who stays."]);
    }

    #[test]
    fn shots_are_padded_to_whole_frames_with_their_gaps() {
        let rate = 24_000;
        let (audio, frames) = lay(&[vec![tone(1.0, rate)], vec![tone(0.5, rate), tone(0.5, rate)]], 12, 0).unwrap();
        // 1.0 + 0.5 gap = 1.5 s = 18 frames; 0.5 + 0.25 + 0.5 + 0.5 = 1.75 s = 21 frames.
        assert_eq!(frames, vec![18, 21]);
        assert_eq!(audio.pcm.len(), (18 + 21) * 2000 * 2);
        // The second shot begins exactly at frame 18.
        assert_eq!(audio.pcm[18 * 2000 * 2], 1);
        assert_eq!(audio.pcm[18 * 2000 * 2 - 1], 0);
    }

    #[test]
    fn frames_and_sound_agree_however_the_rate_divides() {
        // 44100 / 7 isn't whole: the padding is measured from the clip's start.
        let shots: Vec<Vec<Audio>> = (0..9).map(|k| vec![tone(0.3 + k as f32 * 0.11, 44_100)]).collect();
        let (audio, frames) = lay(&shots, 7, 5).unwrap();
        let total: u32 = frames.iter().sum();
        let at = |f: u64| (f * 44_100 + 3) / 7;
        assert_eq!(audio.pcm.len() as u64, (at(5 + total as u64) - at(5)) * 2);
    }

    #[test]
    fn pieces_of_another_rate_are_refused() {
        let err = lay(&[vec![tone(1.0, 24_000)], vec![tone(1.0, 16_000)]], 12, 0).unwrap_err();
        assert!(err.to_string().contains("shot 2"), "{err}");
    }
}
