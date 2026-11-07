//! Enhance: a short prompt rewritten in full for the model it is for, by the
//! agent's model in one call — the Image tab's prompt, a Video shot, or a
//! storyboard's shot after the story turn wrote it.
//!
//! Why it exists: a video model hears everything a prompt leaves open as
//! "anything fits". LTX asked for "a man sings opera" in six words drew a
//! woman talking, or silence; the same shot as a paragraph that names the
//! voice, the sounds of the place and the camera drew the man singing. A
//! storyboard turn is already the fullest a small model writes, so its shots
//! came out as one line each.
//!
//! What is asked of the model is a family's guide (`skills/enhance-*.md`,
//! `modes: [enhance]`, replaceable from `user/skills/` by id) plus that
//! family's prompt-style notes, and a brief of what else the shot has: the
//! clip's look, the shot before, who is in it and how they look, the H3
//! fields, the picture it starts from when the model can see. The answer is a
//! schema-held JSON object, so no preamble ever lands in a prompt box.

use super::{agent_for, shrink_png, skills, tuning, user_skills_dir, Staged};
use crate::config::{Config, ImageFamily, VideoFamily};
use crate::llm::{self, LlmRunner, Sampling};
use crate::sd::Monitor;
use crate::videos::Line;
use anyhow::{anyhow, bail, Context as _, Result};
use serde::{Deserialize, Serialize};

/// A reply runs to a 220-word paragraph and a sound line: under 500 tokens.
const N_PREDICT: u32 = 1024;
/// The brief and a guide come to a couple of thousand tokens; a picture
/// adds its tiles. More context than that only costs load time.
const N_CTX: u32 = 8192;

/// What to enhance, as `POST /enhance` takes it and a storyboard builds it.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Request {
    /// `"video"` for a shot; anything else is the Image tab's prompt.
    #[serde(default)]
    pub kind: String,
    /// The image model (registry name) or the video model it is for.
    pub model: String,
    pub text: String,
    /// The clip's look, added to every shot by the server.
    #[serde(default)]
    pub every: String,
    /// The shot this one goes on from, when it isn't a cut.
    #[serde(default)]
    pub before: String,
    #[serde(default)]
    pub cut: bool,
    /// Shot 1, or a cut, starting from a picture (an output stem).
    #[serde(default)]
    pub picture: Option<String>,
    #[serde(default)]
    pub seconds: Option<f32>,
    /// Who is in it, by name; looks are found in the cast unless given.
    #[serde(default)]
    pub cast: Vec<String>,
    #[serde(default)]
    pub looks: Vec<(String, String)>,
    #[serde(default)]
    pub lines: Vec<Line>,
    #[serde(default)]
    pub sound: String,
    #[serde(default)]
    pub music: String,
    /// The agent model to use; Settings' default when absent.
    #[serde(default)]
    pub agent: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Enhanced {
    pub prompt: String,
    /// H3's sound line, written only when the shot had none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sound: String,
}

/// What a request is for, resolved against what is installed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Target {
    Image(ImageFamily),
    /// The family, and whether its sound is H3's separate fields.
    Video(VideoFamily, bool),
}

impl Target {
    fn family(self) -> &'static str {
        match self {
            Target::Image(f) => f.as_str(),
            Target::Video(f, _) => f.as_str(),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Target::Image(f) => f.label(),
            Target::Video(f, _) => f.label(),
        }
    }

    fn fields(self) -> bool {
        matches!(self, Target::Video(_, true))
    }
}

pub fn target(cfg: &Config, req: &Request) -> Result<Target> {
    if req.kind == "video" {
        let m = cfg
            .video_models()
            .into_iter()
            .find(|m| m.name == req.model)
            .ok_or_else(|| anyhow!("the video model '{}' isn't installed", req.model))?;
        Ok(Target::Video(m.family, m.family.takes_references() && m.audio_vae.is_some()))
    } else {
        let reg = cfg.registry();
        let p = reg.get(&req.model).ok_or_else(|| anyhow!("the image model '{}' isn't installed", req.model))?;
        Ok(Target::Image(p.family))
    }
}

/// The brief the model is handed: the person's words, then what else the
/// shot has. Pure, so the tests can read it.
pub fn brief(t: Target, req: &Request, looks: &[(String, String)], sees_picture: bool) -> String {
    let mut out = String::new();
    let what = match t {
        Target::Image(_) => "image prompt",
        Target::Video(..) => "shot",
    };
    out.push_str(&format!("Rewrite this {what} for {} in full.\n\nThe person's words:\n{}\n", t.label(), req.text.trim()));
    if matches!(t, Target::Video(..)) {
        if !req.every.trim().is_empty() {
            out.push_str(&format!(
                "\nThe whole clip's look, which the server adds to every shot itself (keep to it, don't repeat it): {}\n",
                req.every.trim()
            ));
        }
        if sees_picture {
            out.push_str("\nThe shot starts from the picture shown: it is the first frame.\n");
        } else if !req.cut && !req.before.trim().is_empty() {
            out.push_str(&format!(
                "\nThe shot goes on from the last frame of the shot before, which was: {}\n",
                req.before.trim()
            ));
        } else if req.cut {
            out.push_str("\nThe shot is a cut: a new scene, nothing carried over from before.\n");
        }
        if let Some(s) = req.seconds.filter(|s| *s > 0.0) {
            out.push_str(&format!("\nIt lasts about {} seconds.\n", (s * 10.0).round() / 10.0));
        }
    }
    let named: Vec<&String> = req.cast.iter().filter(|n| !n.trim().is_empty()).collect();
    if !named.is_empty() {
        out.push_str("\nWho is in it:\n");
        for n in named {
            match looks.iter().find(|(m, _)| m.eq_ignore_ascii_case(n)) {
                Some((_, l)) if !l.trim().is_empty() => out.push_str(&format!("- {n}: {}\n", l.trim())),
                _ => out.push_str(&format!("- {n}\n")),
            }
        }
    }
    if matches!(t, Target::Video(..)) {
        let lines: Vec<String> = req
            .lines
            .iter()
            .filter(|l| !l.words.trim().is_empty())
            .map(|l| {
                let how = if l.voiceover { " (voiceover)" } else { "" };
                let lang = if l.language.trim().is_empty() { String::new() } else { format!(" [{}]", l.language.trim()) };
                format!("- {}{how}{lang}: \"{}\"", l.who.trim(), l.words.trim())
            })
            .collect();
        let fields = t.fields();
        if !lines.is_empty() {
            out.push_str(if fields { "\nSpoken (its own field; not in your prompt):\n" } else { "\nSpoken, word for word:\n" });
            out.push_str(&lines.join("\n"));
            out.push('\n');
        }
        if !req.sound.trim().is_empty() {
            out.push_str(&format!(
                "\nSound{}: {}\n",
                if fields { " (its own field; leave `sound` empty)" } else { "" },
                req.sound.trim()
            ));
        } else if fields {
            out.push_str("\nSound: none written yet — write it in `sound`.\n");
        }
        if !req.music.trim().is_empty() {
            out.push_str(&format!("\nMusic{}: {}\n", if fields { " (its own field)" } else { "" }, req.music.trim()));
        }
    }
    out
}

/// The output's shape: the prompt, and H3's sound line beside it.
fn schema(t: Target) -> serde_json::Value {
    let mut props = serde_json::json!({ "prompt": { "type": "string", "minLength": 1, "maxLength": 2400 } });
    let mut required = vec!["prompt"];
    if t.fields() {
        props["sound"] = serde_json::json!({ "type": "string", "maxLength": 500 });
        required.push("sound");
    }
    serde_json::json!({ "type": "object", "properties": props, "required": required, "additionalProperties": false })
}

/// A reply read: the object, wherever the template left it, with Markdown
/// and wrapping quotes taken off.
pub fn parse(content: &str) -> Result<Enhanced> {
    let s = content.trim();
    let (a, b) = (s.find('{'), s.rfind('}'));
    let body = match (a, b) {
        (Some(a), Some(b)) if a < b => &s[a..=b],
        _ => bail!("the agent's answer wasn't the asked-for JSON: {}", s.chars().take(200).collect::<String>()),
    };
    let mut e: Enhanced = serde_json::from_str(body).context("reading the agent's answer")?;
    e.prompt = clean(&e.prompt);
    e.sound = clean(&e.sound);
    if e.prompt.is_empty() {
        bail!("the agent wrote an empty prompt");
    }
    Ok(e)
}

/// Markdown and wrapping quotes off, and typographic punctuation as ASCII:
/// the box then shows what LTX is sent (`sd::ascii_punctuation`).
fn clean(s: &str) -> String {
    let s = crate::sd::ascii_punctuation(&s.replace("**", "").replace("__", ""));
    let s = s.trim();
    let s = s.strip_prefix('"').and_then(|x| x.strip_suffix('"')).unwrap_or(s);
    s.trim().to_string()
}

/// Run one enhancement. `monitor` is the engine's, so Stop kills it.
pub fn run(cfg: &Config, monitor: &Monitor, req: &Request) -> Result<Enhanced> {
    if req.text.trim().is_empty() {
        bail!("there is nothing to enhance — write a few words first");
    }
    let t = target(cfg, req)?;
    let model = agent_for(cfg, req.agent.as_deref())?;
    let tune = tuning(cfg, &model);
    let (all, _) = skills::load(&user_skills_dir(cfg));
    let (guide, used, _) = skills::select_enhance(&all, t.family());
    if used.is_empty() {
        bail!("no enhance guide applies to {}", t.label());
    }

    // Looks: what the caller gave (a storyboard's), else the saved cast's.
    let mut looks = req.looks.clone();
    if !req.cast.is_empty() {
        let store = crate::cast::CastStore::new(&cfg.user_dir());
        for v in store.list() {
            let e = v.entry;
            if !e.draft && !looks.iter().any(|(n, _)| n.eq_ignore_ascii_case(&e.name)) {
                looks.push((e.name, e.look));
            }
        }
    }

    // The first frame, when the model can see it.
    let mut staged = Staged(Vec::new());
    let mut parts = Vec::new();
    if let Some(stem) = req.picture.as_deref().filter(|s| model.vision() && crate::workitems::valid_id(s)) {
        let items = crate::workitems::Store::new(&cfg.base_dir.join("work"), &cfg.user_dir());
        if let Some(src) = items.find_output(stem) {
            let png = shrink_png(&std::fs::read(&src).with_context(|| format!("reading {}", src.display()))?, tune.image_side)?;
            let dir = crate::pipeline::scratch_dir(&cfg.base_dir.join("work"));
            std::fs::create_dir_all(&dir)?;
            let p = dir.join(format!("enhance-{}.png", super::chat::now_ms()));
            std::fs::write(&p, png)?;
            staged.0.push(p.clone());
            parts.push(llm::Part::Image { path: p });
        }
    }
    let sees = !parts.is_empty();
    parts.push(llm::Part::Text { text: brief(t, req, &looks, sees) });

    let system = format!(
        "{guide}\n\nAnswer with one JSON object: `prompt` is the new prompt alone — no title, no \"Here is\", no quotes around it{}.",
        if t.fields() { ", and `sound` the shot's sound line (empty when it has one)" } else { "" }
    );
    let ask = llm::Request {
        v: llm::PROTOCOL,
        model: model.lm.clone(),
        mmproj: if sees { model.mmproj.clone() } else { None },
        n_ctx: tune.n_ctx.min(N_CTX),
        n_predict: N_PREDICT,
        thinking: false,
        sampling: Sampling { seed: (super::rand_seed() & 0xffff_ffff) as u32, ..tune.sampling },
        json_schema: Some(schema(t)),
        messages: vec![llm::Message::text(llm::Role::System, system), llm::Message { role: llm::Role::User, content: parts }],
    };
    let reply = LlmRunner::watched(cfg, monitor).run(&ask, &model.label, "enhance")?;
    if reply.stats.stop == "length" {
        bail!("the enhanced prompt ran past {N_PREDICT} tokens and was cut off");
    }
    let mut e = parse(&reply.content)?;
    if !t.fields() || !req.sound.trim().is_empty() {
        e.sound.clear();
    }
    Ok(e)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shot(text: &str) -> super::Request {
        super::Request { kind: "video".into(), model: "m".into(), text: text.into(), ..Default::default() }
    }

    #[test]
    fn the_brief_says_what_the_shot_has_and_where_each_part_goes() {
        let mut r = shot("a man sings opera");
        r.every = "cinematic, warm light".into();
        r.cast = vec!["Leo".into()];
        r.before = "Leo walks on stage".into();
        r.seconds = Some(4.96);
        r.lines = vec![Line { who: "Leo".into(), words: "Nessun dorma".into(), language: "Italian".into(), voiceover: false }];
        let looks = vec![("leo".to_string(), "Leo is a tall man in a black tailcoat.".to_string())];
        let ltx = brief(Target::Video(VideoFamily::Ltx25, false), &r, &looks, false);
        assert!(ltx.contains("The person's words:\na man sings opera"), "{ltx}");
        assert!(ltx.contains("don't repeat it): cinematic, warm light"), "{ltx}");
        assert!(ltx.contains("shot before, which was: Leo walks on stage"), "{ltx}");
        assert!(ltx.contains("about 5 seconds"), "{ltx}");
        assert!(ltx.contains("- Leo: Leo is a tall man in a black tailcoat."), "{ltx}");
        assert!(ltx.contains("Spoken, word for word:\n- Leo [Italian]: \"Nessun dorma\""), "{ltx}");
        // H3 keeps its speech and sound apart, and is asked for a sound line.
        let h3 = brief(Target::Video(VideoFamily::MinimaxH3, true), &r, &looks, false);
        assert!(h3.contains("Spoken (its own field; not in your prompt)"), "{h3}");
        assert!(h3.contains("write it in `sound`"), "{h3}");
        // A picture shown wins over the shot before; an image prompt has no shot parts.
        assert!(brief(Target::Video(VideoFamily::Wan22Ti2v5b, false), &r, &looks, true).contains("first frame"));
        let img = brief(Target::Image(ImageFamily::Flux), &r, &looks, false);
        assert!(img.starts_with("Rewrite this image prompt for"), "{img}");
        assert!(!img.contains("Spoken") && !img.contains("seconds"), "{img}");
    }

    #[test]
    fn a_reply_is_read_wherever_the_object_is_and_cleaned() {
        let e = parse("Sure:\n```json\n{\"prompt\": \"\\\"**A tenor** sings.\\\"\"}\n```").unwrap();
        assert_eq!(e.prompt, "A tenor sings.");
        assert!(parse("{\"prompt\": \"  \"}").is_err());
        assert!(parse("no object").is_err());
        let s = schema(Target::Video(VideoFamily::MinimaxH3, true));
        assert_eq!(s["required"], serde_json::json!(["prompt", "sound"]));
        assert_eq!(schema(Target::Video(VideoFamily::Ltx23, false))["required"], serde_json::json!(["prompt"]));
    }
}
