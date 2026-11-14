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
pub(super) const N_PREDICT: u32 = 1024;
/// The brief and a guide come to a couple of thousand tokens; a picture
/// adds its tiles. More context than that only costs load time.
pub(super) const N_CTX: u32 = 8192;

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
    /// The part before this one, which it goes on from — or, on a cut (a
    /// shot's part 1), the last part of the shot before, which it cuts from.
    /// (An older page sent it on a continue only.)
    #[serde(default)]
    pub before: String,
    /// The scene's opening part (an Assistant film's shot 1), for every later
    /// one: what it set up — the place, the light, how its people look —
    /// holds unless the words change it.
    #[serde(default)]
    pub first: String,
    /// This camera shot's part 1 (a film's last cut before it), when that
    /// is neither `first` nor `before`.
    #[serde(default)]
    pub scene: String,
    /// The next part goes on from this one's last frame: it must end with
    /// the people in it in view, or they come back with new faces.
    #[serde(default)]
    pub then_continues: bool,
    /// The next shot of the scene, which this part cuts to (its last
    /// part's): led into, so a shot put first knows what it opens.
    #[serde(default)]
    pub next: String,
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
    /// H3's Spoken lines, written only when the part had none: its prompt
    /// is what is seen, so words the person had someone say would otherwise
    /// be lost from it with nowhere to go.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spoken: Vec<Line>,
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
        continuity(&mut out, req, sees_picture);
        if let Some(s) = req.seconds.filter(|s| *s > 0.0) {
            out.push_str(&format!("\nIt lasts about {} seconds.\n", (s * 10.0).round() / 10.0));
        }
    }
    let named: Vec<&String> = req.cast.iter().filter(|n| !n.trim().is_empty()).collect();
    // "Who is in it" and "every voice described" read as a call for people:
    // an empty street came back with two strangers in it (2026-11-14).
    if named.is_empty() && matches!(t, Target::Video(..)) {
        out.push_str(
            "\nNo one is named in it: put in only the people its words, or the parts it goes on from, put there. \
             A place with no one in it stays empty.\n",
        );
    }
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
            out.push_str(if fields {
                "\nSpoken (its own field; not in your prompt; leave `spoken` empty):\n"
            } else {
                "\nSpoken, word for word:\n"
            });
            out.push_str(&lines.join("\n"));
            out.push('\n');
        } else if fields {
            out.push_str(
                "\nSpoken: none written yet — if the person's words have anyone say something, write each line in \
                 `spoken`: who says it (their name as written, or \"narrator\" for a voice nobody on screen has) and \
                 the words. Empty when nobody speaks.\n",
            );
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

/// The most of a neighbouring shot the brief quotes: an enhanced LTX shot runs
/// to about 1400 characters, so this cuts only an unusually long one.
const NEIGHBOUR_CHARS: usize = 1500;

/// `s` to at most `max` characters, at a word.
fn quote(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    let cut = cut.rsplit_once(char::is_whitespace).map_or(cut.as_str(), |(a, _)| a);
    format!("{}…", cut.trim_end())
}

/// What the parts around this one set up, so a Video's shots read as one
/// film: the scene's opening part (the place, the light, the people), the
/// part this camera shot began with, and the part before — which a part goes
/// on from inside a shot, and a shot's part 1 cuts from. A cut used to be
/// told "nothing carried over", and each shot of a five-shot clip lit its
/// own place. An Assistant film fills the same fields from its own shots
/// ([`neighbours`]).
fn continuity(out: &mut String, req: &Request, sees_picture: bool) {
    let before = req.before.trim();
    let first = req.first.trim();
    let scene = req.scene.trim();
    if sees_picture {
        out.push_str("\nThe shot starts from the picture shown: it is the first frame.\n");
    }
    if !first.is_empty() && first != before {
        out.push_str(&format!(
            "\nThe scene opens with:\n{}\nKeep to what it set up — the place, the light, the time of day, the \
             colours, how its people look and dress — unless this part's words change them.\n",
            quote(first, NEIGHBOUR_CHARS)
        ));
    }
    if !scene.is_empty() && scene != first && scene != before {
        out.push_str(&format!("\nThis camera shot began with:\n{}\n", quote(scene, NEIGHBOUR_CHARS)));
    }
    if !req.cut {
        if !before.is_empty() && !sees_picture {
            out.push_str(&format!(
                "\nThis goes on from the last frame of the part before, which was:\n{}\nCarry on the same place, \
                 light, people and clothes, and describe what happens next and what changes; don't set the scene \
                 again.\n",
                quote(before, NEIGHBOUR_CHARS)
            ));
        }
    } else {
        out.push_str("\nThis is a cut: a new camera shot of the same film.");
        if !before.is_empty() {
            out.push_str(&format!(" It cuts from:\n{}\n", quote(before, NEIGHBOUR_CHARS)));
        } else {
            out.push('\n');
        }
        out.push_str(
            "Keep the film's light, time of day and colours unless this part's words move to another place or time, \
             and say again who is where, if anyone is.\n",
        );
    }
    let next = req.next.trim();
    if !next.is_empty() && !req.then_continues {
        out.push_str(&format!(
            "\nThe next shot, which this one cuts to, begins:\n{}\nLead into it — the place, the light, the weather, \
             the mood — but leave its people and what they do to that shot, unless this part's words put them here.\n",
            quote(next, NEIGHBOUR_CHARS)
        ));
    }
    // The next shot knows only this one's last frame: a face turned away or
    // framed out there comes back as a stranger's.
    if req.then_continues {
        out.push_str(
            "\nThe next part goes on from this one's last frame and knows only what it shows: if anyone is in it, \
             end with them in view, faces visible, no one turned away, leaving or cut off by the frame. Add no one \
             for this.\n",
        );
    }
}

/// What Enhance shows a part of what is around it: `Request`'s fields.
#[derive(Debug, Default, PartialEq)]
pub struct Neighbours {
    pub first: String,
    pub scene: String,
    pub before: String,
    pub cut: bool,
    pub then_continues: bool,
    pub next: String,
}

/// Part `k` of shot `s`'s neighbours, from every shot in storyboard order
/// as `(scene, each part's words)`: its scene's opening part, its shot's
/// first part, and the part before — inside the shot that one goes on from
/// its last frame; a shot's part 1 cuts from the last words of the shot
/// before, in this scene or the one before it. **The page's
/// `partNeighbours` (`shots.js`) is the same rule**; one fixture table pins
/// both (`the_fixture_table_the_page_checks_too`, `shots.test.js`).
pub fn part_neighbours(order: &[(usize, Vec<&str>)], s: usize, k: usize) -> Neighbours {
    let Some(at) = order.get(s) else { return Neighbours::default() };
    let t = |x: &str| x.trim().to_string();
    let opener = order.iter().position(|o| o.0 == at.0).unwrap_or(s);
    let first = if opener == s && k == 0 { String::new() } else { order[opener].1.iter().map(|x| t(x)).find(|x| !x.is_empty()).unwrap_or_default() };
    let mut before = String::new();
    if k > 0 {
        before = at.1.get(k - 1).map_or(String::new(), |x| t(x));
    } else {
        for j in (0..s).rev() {
            before = order[j].1.iter().rev().map(|x| t(x)).find(|x| !x.is_empty()).unwrap_or_default();
            if !before.is_empty() {
                break;
            }
        }
    }
    let then_continues = at.1.get(k + 1).is_some_and(|x| !x.trim().is_empty());
    // The first words of the next shot in this scene, for a part that ends
    // its shot: a new scene is another place, which this one doesn't lead to.
    let next = if then_continues {
        String::new()
    } else {
        order[s + 1..].iter().take_while(|o| o.0 == at.0).find_map(|o| o.1.iter().map(|x| t(x)).find(|x| !x.is_empty())).unwrap_or_default()
    };
    Neighbours {
        first,
        scene: if k > 0 { at.1.first().map_or(String::new(), |x| t(x)) } else { String::new() },
        cut: k == 0 && !before.is_empty(),
        before,
        then_continues,
        next,
    }
}

/// `req` with less of its neighbours, so the brief fits `budget` tokens of
/// context: the scene's opening goes first, then shot 1, then the shot
/// before is shortened. A small agent at 8192 never fails for continuity's
/// sake. `None` when the brief already fits.
fn trimmed(req: &Request, t: Target, fixed: usize, budget: usize) -> Option<Request> {
    let fits = |r: &Request| fixed + super::context::text_tokens(&brief(t, r, &r.looks, false)) <= budget;
    if fits(req) {
        return None;
    }
    let mut r = req.clone();
    r.scene.clear();
    if !fits(&r) {
        r.next.clear();
    }
    if !fits(&r) {
        r.first.clear();
    }
    if !fits(&r) {
        r.before = quote(&r.before, 300);
    }
    Some(r)
}

/// The output's shape: the prompt, and H3's sound line and Spoken lines
/// beside it.
/// The longest an enhanced prompt may be: 2400 characters, less on a model
/// with a prompt limit (`VideoFeatures::prompt_chars`, LTX's cliff) — what
/// is left of it beside the scene's description, which the server joins on
/// (`videos::shot_prompt`).
pub(super) const PROMPT_CHARS: u32 = 2400;

fn prompt_cap(cfg: &Config, req: &Request) -> u32 {
    let limit = (req.kind == "video")
        .then(|| cfg.video_models().into_iter().find(|m| m.name == req.model))
        .flatten()
        .and_then(|m| m.features().prompt_chars);
    let every = req.every.trim().chars().count() as u32;
    limit.map_or(PROMPT_CHARS, |n| n.saturating_sub(every + 2).clamp(200, PROMPT_CHARS))
}

pub(super) fn schema(t: Target, max: u32) -> serde_json::Value {
    let mut props = serde_json::json!({ "prompt": { "type": "string", "minLength": 1, "maxLength": max } });
    let mut required = vec!["prompt"];
    if t.fields() {
        props["sound"] = serde_json::json!({ "type": "string", "maxLength": 500 });
        props["spoken"] = serde_json::json!({
            "type": "array",
            "maxItems": 8,
            "items": {
                "type": "object",
                "properties": {
                    "who": { "type": "string", "maxLength": 60 },
                    "words": { "type": "string", "maxLength": 400 },
                    "voiceover": { "type": "boolean" },
                },
                "required": ["who", "words", "voiceover"],
                "additionalProperties": false,
            },
        });
        required.push("sound");
        required.push("spoken");
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
    for l in &mut e.spoken {
        l.who = clean(&l.who);
        l.words = clean(&l.words);
    }
    e.spoken.retain(|l| !l.words.is_empty());
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
    let cap = prompt_cap(cfg, req);
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
    // Only the variables the person's own words use: one Enhance put in
    // unasked rolls a different word into every picture.
    let vars = crate::variables::used(&format!("{}\n{}", req.text, req.sound), &cfg.settings().variables);
    let system = system_text(&guide, &vars, t);
    let n_ctx = tune.n_ctx.min(N_CTX);
    let fixed = super::context::text_tokens(&system) + if sees { super::context::image_tokens(tune.image_side) } else { 0 };
    let budget = (n_ctx as usize).saturating_sub(N_PREDICT as usize + 64);
    let fitted = trimmed(&Request { looks: looks.clone(), ..req.clone() }, t, fixed, budget);
    let req = fitted.as_ref().map_or(req, |r| r);
    let ask = |req: &Request| {
        llm::Request {
            v: llm::PROTOCOL,
            model: model.lm.clone(),
            mmproj: if sees { model.mmproj.clone() } else { None },
            n_ctx,
            n_predict: N_PREDICT,
            thinking: false,
            sampling: Sampling { seed: (super::rand_seed() & 0xffff_ffff) as u32, ..tune.sampling },
            json_schema: Some(schema(t, cap)),
            messages: messages(&system, t, req, &looks, &parts),
        }
    };
    let runner = LlmRunner::watched(cfg, monitor);
    let reply = match runner.run(&ask(req), &model.label, "enhance") {
        // The estimate is rough: once more with the shot's own words alone.
        Err(e) if e.downcast_ref::<llm::ContextOverflow>().is_some() && (!req.first.is_empty() || !req.before.is_empty()) => {
            let bare = Request { first: String::new(), scene: String::new(), before: quote(&req.before, 300), ..req.clone() };
            runner.run(&ask(&bare), &model.label, "enhance")?
        }
        r => r?,
    };
    // Cut off: a model that rambled. Once more, on another seed.
    let reply = if reply.stats.stop == "length" { runner.run(&ask(req), &model.label, "enhance")? } else { reply };
    if reply.stats.stop == "length" {
        bail!("the enhanced prompt ran past {N_PREDICT} tokens and was cut off");
    }
    let mut e = parse(&reply.content)?;
    if !t.fields() || !req.sound.trim().is_empty() {
        e.sound.clear();
    }
    for name in keep_known(&mut e, &vars) {
        eprintln!("enhance: {{{name}}} wasn't in the words it was given; the prompt says \"{name}\"");
    }
    Ok(e)
}

/// The system text: the family's guide, which variables to keep, the answer's
/// shape. `run`'s and `agent-check`'s, so a check scores the real call.
pub(super) fn system_text(guide: &str, vars: &crate::variables::Variables, t: Target) -> String {
    format!(
        "{guide}\n\n{}\n\nAnswer with one JSON object: `prompt` is the new prompt alone — no title, no \"Here is\", no quotes around it{}.",
        variables_line(vars),
        if t.fields() { ", and `sound` the shot's sound line (empty when it has one)" } else { "" }
    )
}

/// The call's messages: the system text, then the first frame (when shown)
/// and the brief.
pub(super) fn messages(system: &str, t: Target, req: &Request, looks: &[(String, String)], parts: &[llm::Part]) -> Vec<llm::Message> {
    let mut content = parts.to_vec();
    content.push(llm::Part::Text { text: brief(t, req, looks, !parts.is_empty()) });
    vec![llm::Message::text(llm::Role::System, system.to_string()), llm::Message { role: llm::Role::User, content }]
}

/// Which `{name}`s the answer keeps: the guides teach "keep a word in
/// braces", and a model not told which made some up — an unknown name fails
/// the next Generate — or added real ones the person never asked for.
fn variables_line(vars: &crate::variables::Variables) -> String {
    if vars.is_empty() {
        return "The person's words use no variables: write no {braces}.".to_string();
    }
    let names: Vec<String> = vars.keys().map(|n| format!("{{{n}}}")).collect();
    format!(
        "The person's words use the variables {}: keep each exactly, braces and all. Add no other {{name}}.",
        names.join(", ")
    )
}

/// The braces off any `{name}` in the answer that the person's words didn't
/// use (`variables::unbrace_unknown`, as a chat reply's prompt gets): never
/// a prompt Generate would refuse, nor a roll nobody asked for. Answers the
/// names.
fn keep_known(e: &mut Enhanced, vars: &crate::variables::Variables) -> Vec<String> {
    let mut dropped = Vec::new();
    for text in [&mut e.prompt, &mut e.sound].into_iter().chain(e.spoken.iter_mut().map(|l| &mut l.words)) {
        let (kept, names) = crate::variables::unbrace_unknown(text, vars);
        *text = kept;
        for n in names {
            if !dropped.contains(&n) {
                dropped.push(n);
            }
        }
    }
    dropped
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
        assert!(ltx.contains("part before, which was:\nLeo walks on stage"), "{ltx}");
        assert!(ltx.contains("about 5 seconds"), "{ltx}");
        assert!(ltx.contains("- Leo: Leo is a tall man in a black tailcoat."), "{ltx}");
        assert!(ltx.contains("Spoken, word for word:\n- Leo [Italian]: \"Nessun dorma\""), "{ltx}");
        // H3 keeps its speech and sound apart, and is asked for a sound line.
        let h3 = brief(Target::Video(VideoFamily::MinimaxH3, true), &r, &looks, false);
        assert!(h3.contains("Spoken (its own field; not in your prompt; leave `spoken` empty)"), "{h3}");
        assert!(h3.contains("write it in `sound`"), "{h3}");
        assert!(!h3.contains("write each line in `spoken`"), "{h3}");
        let unsaid = brief(Target::Video(VideoFamily::MinimaxH3, true), &Request { lines: vec![], ..r.clone() }, &looks, false);
        assert!(unsaid.contains("write each line in `spoken`"), "{unsaid}");
        assert!(!ltx.contains("`spoken`"), "LTX weaves speech into its paragraph");
        // A picture shown wins over the shot before; an image prompt has no shot parts.
        assert!(brief(Target::Video(VideoFamily::Wan22Ti2v5b, false), &r, &looks, true).contains("first frame"));
        let img = brief(Target::Image(ImageFamily::Flux), &r, &looks, false);
        assert!(img.starts_with("Rewrite this image prompt for"), "{img}");
        assert!(!img.contains("Spoken") && !img.contains("seconds"), "{img}");
    }

    /// The same table as `partNeighbours`'s in `shots.test.js`: change
    /// both or neither.
    #[test]
    fn the_fixture_table_the_page_checks_too() {
        let order = [
            (0, vec!["Dusk on a pier.", "She turns."]),
            (0, vec!["Close on her face.", "", "She smiles.", "She walks."]),
            (1, vec!["A cafe at noon."]),
        ];
        let n = |first: &str, scene: &str, before: &str, cut: bool, then_continues: bool, next: &str| Neighbours {
            first: first.into(),
            scene: scene.into(),
            before: before.into(),
            cut,
            then_continues,
            next: next.into(),
        };
        assert_eq!(part_neighbours(&order, 0, 0), n("", "", "", false, true, ""));
        assert_eq!(part_neighbours(&order, 0, 1), n("Dusk on a pier.", "Dusk on a pier.", "Dusk on a pier.", false, false, "Close on her face."));
        assert_eq!(part_neighbours(&order, 1, 0), n("Dusk on a pier.", "", "She turns.", true, false, ""));
        assert_eq!(part_neighbours(&order, 1, 2), n("Dusk on a pier.", "Close on her face.", "", false, true, ""));
        assert_eq!(part_neighbours(&order, 2, 0), n("", "", "She walks.", true, false, ""), "a new scene opens itself, and the last shot of one leads to nothing");
    }

    #[test]
    fn every_later_shot_keeps_to_shot_one_and_a_cut_to_the_shot_it_cuts_from() {
        let t = Target::Video(VideoFamily::Ltx25, false);
        let order = [(0, vec!["Dusk on a pier, orange light.", "Mara walks on."]), (0, vec!["Leo in a cafe.", "He sips.", "Mara looks back."])];
        assert_eq!(part_neighbours(&order, 0, 0), Neighbours { then_continues: true, ..Default::default() });
        // A shot put first leads into the next: its last part is told what that one opens with.
        let n = part_neighbours(&order, 0, 1);
        let r = super::Request { next: n.next, ..shot("Mara walks on.") };
        let b = brief(t, &r, &[], false);
        assert!(b.contains("cuts to, begins:\nLeo in a cafe.\nLead into it") && b.contains("leave its people"), "{b}");
        assert!(!brief(t, &super::Request { then_continues: true, ..r }, &[], false).contains("cuts to, begins"), "a part going on in its shot leads nowhere");
        let n = part_neighbours(&order, 1, 2);
        assert_eq!((n.first.as_str(), n.scene.as_str(), n.before.as_str()), ("Dusk on a pier, orange light.", "Leo in a cafe.", "He sips."));
        let mut r = shot("Mara looks back.");
        (r.first, r.scene, r.before, r.cut) = (n.first, n.scene, n.before, true);
        let b = brief(t, &r, &[], false);
        assert!(b.contains("opens with:\nDusk on a pier, orange light.\nKeep to what it set up"), "{b}");
        assert!(b.contains("camera shot began with:\nLeo in a cafe."), "{b}");
        assert!(b.contains("a new camera shot of the same film. It cuts from:\nHe sips."), "{b}");
        assert!(!b.contains("nothing carried over"), "{b}");
        // A continue goes on from the shot before; shot 1 isn't quoted twice.
        let n = part_neighbours(&order, 0, 1);
        let mut r = shot("Mara walks on.");
        (r.first, r.scene, r.before) = (n.first, n.scene, n.before);
        let b = brief(t, &r, &[], false);
        assert!(b.contains("which was:\nDusk on a pier") && b.contains("don't set the scene again"), "{b}");
        assert!(!b.contains("opens with"), "{b}");
        // Shot 1 has none of it.
        assert!(!brief(t, &shot("Dusk on a pier."), &[], false).contains("part before"));
        // One the next shot goes on from ends with its people in view.
        let r = super::Request { then_continues: true, ..shot("Dusk on a pier.") };
        let b = brief(t, &r, &[], false);
        assert!(b.contains("if anyone is in it, end with them in view, faces visible"), "{b}");
        // An empty place stays empty: that line once filled a street with strangers.
        assert!(b.contains("Add no one") && b.contains("A place with no one in it stays empty"), "{b}");
        let r = super::Request { cast: vec!["Mara".into()], ..r };
        assert!(!brief(t, &r, &[], false).contains("No one is named"));
    }

    #[test]
    fn a_brief_too_long_for_the_context_sheds_its_neighbours_in_order() {
        let t = Target::Video(VideoFamily::Ltx25, false);
        let long = "word ".repeat(280);
        let mut r = shot("Mara looks back.");
        (r.first, r.scene, r.before, r.cut) = (format!("first {long}"), format!("scene {long}"), format!("before {long}"), true);
        assert!(trimmed(&r, t, 0, 100_000).is_none(), "room for all of it");
        let whole = text_len(&brief(t, &r, &[], false));
        let a = trimmed(&r, t, 0, whole - 50).unwrap();
        assert!(a.scene.is_empty() && !a.first.is_empty(), "the scene goes first");
        let b = trimmed(&r, t, 0, whole - 700).unwrap();
        assert!(b.scene.is_empty() && b.first.is_empty() && b.before.len() > 1000);
        let c = trimmed(&r, t, 0, 300).unwrap();
        assert!(c.before.chars().count() <= 301 && c.before.ends_with('…'), "{}", c.before);
        assert_eq!(quote("one two three", 9), "one two…");
    }

    fn text_len(s: &str) -> usize {
        super::super::context::text_tokens(s)
    }

    #[test]
    fn an_answer_keeps_only_the_variables_the_persons_words_use() {
        let defined: crate::variables::Variables = [
            ("Animal".to_string(), vec!["cat".to_string(), "fox".to_string()]),
            ("mood".to_string(), vec!["calm".to_string()]),
        ]
        .into();
        // Only what the person's words use: {mood} is defined but wasn't written.
        let vars = crate::variables::used("a {animal} at dusk", &defined);
        assert_eq!(vars.keys().collect::<Vec<_>>(), ["Animal"]);
        let mut e = Enhanced { prompt: "a {animal}, {mood}, with {fur_color} fur".into(), sound: "a {animal} purrs, {fur_color}".into(), ..Default::default() };
        assert_eq!(keep_known(&mut e, &vars), ["mood".to_string(), "fur_color".to_string()]);
        assert_eq!(e.prompt, "a {animal}, mood, with fur_color fur");
        assert_eq!(e.sound, "a {animal} purrs, fur_color");
        assert!(variables_line(&vars).contains("{Animal}"));
        assert!(variables_line(&crate::variables::used("a fox", &defined)).contains("no variables"));
    }

    #[test]
    fn a_reply_is_read_wherever_the_object_is_and_cleaned() {
        let e = parse("Sure:\n```json\n{\"prompt\": \"\\\"**A tenor** sings.\\\"\"}\n```").unwrap();
        assert_eq!(e.prompt, "A tenor sings.");
        assert!(parse("{\"prompt\": \"  \"}").is_err());
        assert!(parse("no object").is_err());
        let s = schema(Target::Video(VideoFamily::MinimaxH3, true), PROMPT_CHARS);
        assert_eq!(s["required"], serde_json::json!(["prompt", "sound", "spoken"]));
        let e = parse(r#"{"prompt": "Mara waves.", "sound": "", "spoken": [
            {"who": "Mara", "words": "\"Hello there.\"", "voiceover": false}, {"who": "Leo", "words": " ", "voiceover": false}]}"#)
        .unwrap();
        assert_eq!(e.spoken.len(), 1, "a line with no words is dropped");
        assert_eq!((e.spoken[0].who.as_str(), e.spoken[0].words.as_str()), ("Mara", "Hello there."));
        assert_eq!(schema(Target::Video(VideoFamily::Ltx23, false), PROMPT_CHARS)["required"], serde_json::json!(["prompt"]));
    }
}
