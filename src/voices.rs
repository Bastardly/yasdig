//! Voice models: text-to-speech models in `models/voices/`
//! (`Config::voice_dir`), read off GGUF headers and paired into what the
//! driver's speak task runs ([`crate::llm::SpeakRequest`]).
//!
//! A voice model is llama.cpp's `tools/tts` layout: a backbone (Qwen3-TTS's
//! talker, `general.architecture = qwen3tts`) and a projector that generates
//! sound — `clip.has_gen_audio_encoder`, which also carries the speaker encoder
//! and the codec that turns codes into a waveform. Both files say what they
//! are, so nothing is read off a filename. Read 2026-11-09 off
//! `ggml-org/Qwen3-TTS-12Hz-1.7B-Base-GGUF`: the backbone has no chat template
//! (so the agents' scan never offered it) and carries its author's sampling in
//! `general.sampling.*`. Chatterbox's (`ggml-org/chatterbox-turbo-GGUF`,
//! `chatterbox-GGUF`, read the same day) is a plain `gpt2` or `llama` backbone
//! beside a `chatterbox` projector, and lists its languages
//! (`general.languages`).
//!
//! **Paired by folder**, as agents are: a folder with one generating
//! projector and one backbone is one voice model. The projector is what makes
//! a folder a voice model, so any backbone beside it counts — llama.cpp's TTS
//! models don't share an architecture, and the driver's loop is the generic
//! one `tts.cpp` runs for all of them.
//!
//! Its own folder and scan, like the agents', so neither listing ever offers
//! the other's files, and the image scan leaves the folder out.

use crate::agents::{self, GgufMeta};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Backbone architectures that are only ever a voice model's, so a copy left
/// in the agents' folder is named for what it is.
const TTS_ARCHES: &[&str] = &["qwen3tts", "pockettts"];

/// Whether a file is part of a voice model: a projector that generates sound,
/// or a backbone of an architecture that exists for nothing else.
pub fn is_voice_file(m: &GgufMeta) -> bool {
    match m.arch.as_deref() {
        Some("clip") => m.gen_audio == Some(true),
        Some(a) => TTS_ARCHES.contains(&a),
        None => false,
    }
}

/// A voice model the Video tab can narrate with.
#[derive(Debug, Clone, PartialEq)]
pub struct VoiceModel {
    /// The backbone's path under the voices folder, `/`-separated and without
    /// `.gguf` — unique, and what the clip's model records.
    pub name: String,
    /// What the file calls itself (`general.name`), else the folder's name.
    pub label: String,
    pub lm: PathBuf,
    pub mmproj: PathBuf,
    pub bytes: u64,
    /// The author's sampling from the backbone's header, where it has one.
    pub temp: Option<f32>,
    pub top_p: Option<f32>,
    pub top_k: Option<u32>,
    /// The language codes it reads: the backbone's `general.languages` (else
    /// the projector's), else [`crate::narration::QWEN_LANGUAGES`] — the file
    /// that lists none.
    pub languages: Vec<String>,
}

/// What a scan found: the voice models, and every other file with the reason.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Scan {
    pub models: Vec<VoiceModel>,
    pub other: Vec<(PathBuf, String)>,
}

/// A folder's GGUFs, each with its metadata or why it couldn't be read.
type Folder = Vec<(PathBuf, Result<GgufMeta, String>)>;

/// Every voice model under `dir`, sorted by name; two folders deep, like the
/// agents' scan.
pub fn scan(dir: &Path) -> Scan {
    let mut by_folder: BTreeMap<PathBuf, Folder> = BTreeMap::new();
    agents::walk(dir, 0, &mut |p| {
        let folder = p.parent().unwrap_or(dir).to_path_buf();
        by_folder.entry(folder).or_default().push((p.to_path_buf(), agents::meta(p)));
    });
    let mut out = Scan::default();
    for (folder, files) in by_folder {
        pair_folder(dir, &folder, files, &mut out);
    }
    out.models.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn pair_folder(root: &Path, folder: &Path, files: Folder, out: &mut Scan) {
    let mut lms = Vec::new();
    let mut projs = Vec::new();
    for (p, m) in files {
        match m {
            Err(e) => out.other.push((p, format!("could not be read ({e})"))),
            Ok(m) if m.arch.as_deref() == Some("clip") && m.gen_audio == Some(true) => projs.push((p, m)),
            Ok(m) if m.arch.as_deref() == Some("clip") => {
                out.other.push((p, "a projector that doesn't generate sound — not a voice model's".into()))
            }
            Ok(m) if m.arch.is_some() => lms.push((p, m)),
            Ok(_) => out.other.push((p, "a GGUF file with no architecture".into())),
        }
    }
    if lms.len() == 1 && projs.len() == 1 {
        let (lm, m) = lms.remove(0);
        let (mmproj, pm) = projs.remove(0);
        let label = m.name.clone().unwrap_or_else(|| {
            let named = if folder == root { &lm } else { folder };
            named.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
        });
        out.models.push(VoiceModel {
            name: crate::config::slash_name(root, &lm.with_extension("")),
            label,
            bytes: agents::len(&lm) + agents::len(&mmproj),
            lm,
            mmproj,
            temp: m.temp,
            top_p: m.top_p,
            top_k: m.top_k,
            languages: m.languages.or(pm.languages).unwrap_or_else(|| {
                crate::narration::QWEN_LANGUAGES.iter().map(|c| c.to_string()).collect()
            }),
        });
        return;
    }
    let why_lm = if projs.is_empty() {
        "a model with no sound-generating projector beside it — a voice model is both files in one folder"
    } else {
        "this folder holds several voice models or projectors — put each model in a folder with its own projector"
    };
    for (p, _) in lms {
        out.other.push((p, why_lm.into()));
    }
    for (p, _) in projs {
        out.other.push((p, "a voice projector with no single model beside it to pair with".into()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(arch: &str, gen_audio: Option<bool>) -> GgufMeta {
        GgufMeta { arch: Some(arch.into()), gen_audio, ..Default::default() }
    }

    #[test]
    fn a_voice_file_is_told_by_its_header() {
        assert!(is_voice_file(&meta("clip", Some(true))));
        assert!(is_voice_file(&meta("qwen3tts", None)));
        assert!(!is_voice_file(&meta("clip", None)), "a vision projector");
        assert!(!is_voice_file(&meta("qwen3", None)), "an agent or an encoder");
    }

    #[test]
    fn one_backbone_and_one_projector_in_a_folder_are_a_voice() {
        let root = Path::new("/m/voices");
        let folder = root.join("qwen3-tts");
        let mut out = Scan::default();
        pair_folder(
            root,
            &folder,
            vec![
                (folder.join("tts.gguf"), Ok(GgufMeta { top_k: Some(50), ..meta("qwen3tts", None) })),
                (folder.join("mmproj.gguf"), Ok(meta("clip", Some(true)))),
            ],
            &mut out,
        );
        assert_eq!(out.other, vec![]);
        assert_eq!(out.models.len(), 1);
        let v = &out.models[0];
        assert_eq!(v.name, "qwen3-tts/tts");
        assert_eq!(v.label, "qwen3-tts");
        assert_eq!(v.mmproj, folder.join("mmproj.gguf"));
        assert_eq!(v.top_k, Some(50));
        assert_eq!(v.languages, crate::narration::QWEN_LANGUAGES, "a file that lists none reads Qwen3-TTS's ten");
    }

    #[test]
    fn a_chatterbox_folder_is_a_voice_that_reads_its_listed_languages() {
        // Turbo's backbone is a plain gpt2; only the projector says "voice".
        let root = Path::new("/m/voices");
        let folder = root.join("chatterbox-turbo");
        let mut out = Scan::default();
        let en = Some(vec!["en".to_string()]);
        pair_folder(
            root,
            &folder,
            vec![
                (folder.join("turbo.gguf"), Ok(GgufMeta { languages: en.clone(), ..meta("gpt2", None) })),
                (folder.join("mmproj.gguf"), Ok(meta("clip", Some(true)))),
            ],
            &mut out,
        );
        assert_eq!(out.other, vec![]);
        assert_eq!(out.models[0].languages, ["en"]);
        assert!(!is_voice_file(&meta("gpt2", None)), "gpt2 is no voice model's alone");

        // The projector's list counts when the backbone has none.
        let mut out = Scan::default();
        pair_folder(
            root,
            &folder,
            vec![
                (folder.join("lm.gguf"), Ok(meta("llama", None))),
                (folder.join("mmproj.gguf"), Ok(GgufMeta { languages: en, ..meta("clip", Some(true)) })),
            ],
            &mut out,
        );
        assert_eq!(out.models[0].languages, ["en"]);
    }

    #[test]
    fn a_lone_or_ambiguous_file_is_listed_with_the_reason() {
        let root = Path::new("/m/voices");
        let mut out = Scan::default();
        pair_folder(root, root, vec![(root.join("tts.gguf"), Ok(meta("qwen3tts", None)))], &mut out);
        assert!(out.models.is_empty());
        assert!(out.other[0].1.contains("no sound-generating projector"), "{:?}", out.other);

        let mut out = Scan::default();
        pair_folder(
            root,
            root,
            vec![
                (root.join("a.gguf"), Ok(meta("qwen3tts", None))),
                (root.join("b.gguf"), Ok(meta("qwen3tts", None))),
                (root.join("p.gguf"), Ok(meta("clip", Some(true)))),
            ],
            &mut out,
        );
        assert!(out.models.is_empty());
        assert_eq!(out.other.len(), 3);
    }

    /// The real files' headers, when they are at hand (a range read of the
    /// first megabytes is enough): `YASDIG_VOICE_HEADERS=<dir with lm.bin and
    /// mmproj.bin>`.
    #[test]
    fn the_released_headers_read_as_a_voice_model() {
        let Ok(dir) = std::env::var("YASDIG_VOICE_HEADERS") else { return };
        let dir = Path::new(&dir);
        let lm = agents::read_meta(&dir.join("lm.bin")).unwrap();
        let proj = agents::read_meta(&dir.join("mmproj.bin")).unwrap();
        assert_eq!(lm.arch.as_deref(), Some("qwen3tts"));
        assert!(!lm.chat_template);
        assert!(is_voice_file(&lm) && is_voice_file(&proj));
        assert_eq!(proj.gen_audio, Some(true));
        eprintln!("lm sampling: {:?} {:?} {:?}", lm.temp, lm.top_p, lm.top_k);
    }
}
