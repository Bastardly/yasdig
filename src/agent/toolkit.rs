//! The knob table resolved for one chat: whether each knob is on the
//! person's whitelist, its lock and current value, the passes and whether
//! its model can run them, and the models it may switch to. What
//! `GET /chats/{id}` serves the page, what `agent::schema` builds a reply's
//! grammar from, and what the agent is told each turn — one value, so the
//! three can't disagree.

use super::chat::Chat;
use super::knobs::{self, hard_bounds, Kind, Locks, Rules, ACTIONS, KNOBS};
use crate::catalog::Catalog;
use crate::config::{Config, ImageFamily, Profile};
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Serialize)]
pub struct KnobView {
    pub key: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    /// `text`, `int`, `float`, `size` or `model`.
    pub kind: &'static str,
    /// yasdig's own limits, what a value is clamped to.
    pub min: Option<f64>,
    pub max: Option<f64>,
    /// On the whitelist in Settings → Agent.
    pub allowed: bool,
    /// Why the model rules it out, when it does.
    pub locked: Option<String>,
    pub value: Value,
    #[serde(skip)]
    pub about: &'static str,
    #[serde(skip)]
    pub max_chars: Option<usize>,
}

impl KnobView {
    /// Whether a reply may name it at all: allowed, and not locked.
    pub fn offered(&self) -> bool {
        self.allowed && self.locked.is_none()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ActionView {
    pub key: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub allowed: bool,
    /// Why the chat's model can't run it, when it can't.
    pub unavailable: Option<String>,
    #[serde(skip)]
    pub about: &'static str,
}

impl ActionView {
    pub fn offered(&self) -> bool {
        self.allowed && self.unavailable.is_none()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Toolkit {
    pub knobs: Vec<KnobView>,
    pub actions: Vec<ActionView>,
    /// Every model the chat's settings could name: what the model knob picks
    /// from.
    pub models: Vec<ModelChoice>,
    /// The family of the chat's model, as `ImageFamily::as_str`.
    pub family: Option<&'static str>,
    pub family_label: Option<&'static str>,
    /// What the chat's model rules out, with the reasons.
    pub locks: Locks,
    /// The video model a film is shot on: the Video tab's when the person
    /// last wrote.
    pub video: Option<VideoView>,
    /// Every installed video model, which a story turn may write the board
    /// for (`video_model`) when the model knob is allowed.
    pub videos: Vec<VideoView>,
    /// Why this chat can't make a film, or `None` when it can.
    pub film_off: Option<String>,
    /// An Image chat's: the settings shown are the Image tab's own controls.
    pub image_tab: bool,
    /// The agent model sees pictures. Set by a turn, which knows the model.
    #[serde(skip)]
    pub vision: bool,
    /// The whitelist it was built under, for `knobs::apply`.
    #[serde(skip)]
    pub rules: Rules,
    /// The saved cast (`crate::cast`), for a Video chat's story turn: the
    /// names it may reuse, each with its look.
    #[serde(skip)]
    pub library: Vec<SavedCast>,
}

/// A saved cast entry as a story turn is shown it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SavedCast {
    pub id: String,
    pub name: String,
    pub kind: super::chat::MemberKind,
    pub look: String,
}

/// The saved cast a turn may reuse: only what the person asked for. Listing
/// every entry had a film about a camel and a parrot pull in the person's
/// unrelated characters. An entry is offered when the person's messages name
/// it (a whole word, spelled as saved — the Image tab's rule, so a member
/// named "Rain" isn't found in "rain"), when the film already has it, or when
/// the newest message asks for their cast as a whole ("my cast", "my saved
/// characters"); that last is a phrase test and no more.
pub fn offered(library: Vec<SavedCast>, chat: &super::chat::Chat) -> Vec<SavedCast> {
    let said: Vec<&str> = chat
        .messages
        .iter()
        .filter_map(|m| match m {
            super::chat::Msg::User { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    if said.last().is_some_and(|t| asks_for_whole_cast(t)) {
        return library;
    }
    let bound: Vec<&str> = chat
        .film
        .iter()
        .flat_map(|f| f.cast.iter())
        .filter_map(|m| m.cast_id.as_deref())
        .collect();
    library
        .into_iter()
        .filter(|e| bound.contains(&e.id.as_str()) || said.iter().any(|t| names(t, &e.name)))
        .collect()
}

/// `name` in `text` as a whole word: letters and digits of any script are
/// word characters, so "Eliason" doesn't name Elias.
pub fn names(text: &str, name: &str) -> bool {
    let name = name.trim();
    if name.is_empty() {
        return false;
    }
    let word = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
    text.match_indices(name).any(|(at, _)| !word(text[..at].chars().next_back()) && !word(text[at + name.len()..].chars().next()))
}

fn asks_for_whole_cast(text: &str) -> bool {
    let words: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect();
    words.windows(2).any(|w| {
        matches!(w[0].as_str(), "my" | "saved") && matches!(w[1].as_str(), "cast" | "characters" | "people")
    })
}

/// How long a shot's words take to happen, at least: a beat to read the
/// frame, then each action in turn. Actions are the clauses the text chains
/// ("walks in, then kneels by the oasis" is two), and one that brings someone
/// in, across or down — entering, walking, kneeling, sitting — takes longer
/// than a glance. A floor under the agent's own `seconds`, which small models
/// set to a second whatever the shot holds.
pub fn action_seconds(text: &str) -> f32 {
    const SLOW: [&str; 26] = [
        "enter", "walk", "arriv", "appear", "shows up", "show up", "comes in", "come in", "into the frame",
        "into frame", "across", "kneel", "sits", "sit down", "stand", "climb", "crouch", "lies down", "lie down",
        "turns around", "turn around", "runs", "pick", "opens", "leaves", "steps",
    ];
    let lower = text.to_lowercase();
    let clauses: Vec<&str> = lower
        .split([',', ';', '.', '!', '?'])
        .flat_map(|c| c.split(" and "))
        .flat_map(|c| c.split(" then "))
        .flat_map(|c| c.split(" before "))
        .flat_map(|c| c.split(" after "))
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .collect();
    let mut secs: f32 = 1.0;
    for c in &clauses {
        secs += if SLOW.iter().any(|w| c.contains(w)) { 2.0 } else { 1.0 };
    }
    secs.max(2.0)
}

/// A video model as a film needs it described: what its shots can do, and
/// the size and length the Video tab gives each one.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct VideoView {
    pub name: String,
    pub family: &'static str,
    pub label: &'static str,
    /// It goes on from one shot's last frame to the next: a film of several.
    pub chains: bool,
    /// Its cuts follow the cast's pictures (H3) rather than a keyframe.
    pub references: bool,
    /// And sounds, voices among them: H3 with its audio VAE.
    pub sounds: bool,
    pub width: u32,
    pub height: u32,
    /// Where a Video tab row starts: the model's own length.
    pub frames: u32,
    pub fps: u32,
    /// The longest a storyboard shot may run, in seconds: Settings'
    /// `storyboard_shot_seconds` ([`VideoView::of`] gives the default).
    pub longest_secs: f32,
}

impl VideoView {
    pub fn of(m: &crate::config::VideoModel) -> VideoView {
        VideoView {
            name: m.name.clone(),
            family: m.family.as_str(),
            label: m.family.label(),
            // A narrated model "chains" (shots of their own pictures) but a
            // storyboard's rows describe motion, not what a voice reads, so a
            // film is never made on one.
            chains: m.family.chains() && m.family != crate::config::VideoFamily::Narrated,
            references: m.family.takes_references(),
            sounds: m.family.takes_references() && m.audio_vae.is_some(),
            width: m.width,
            height: m.height,
            frames: m.frames,
            fps: m.effective_fps(),
            longest_secs: crate::settings::DEFAULT_STORYBOARD_SHOT_SECONDS,
        }
    }

    /// A shot's length in seconds, at most.
    pub fn shot_seconds(&self) -> f32 {
        self.frames as f32 / self.fps.max(1) as f32
    }

    fn video_family(&self) -> Option<crate::config::VideoFamily> {
        crate::config::VideoFamily::ALL.into_iter().find(|f| f.as_str() == self.family)
    }

    /// The most frames a storyboard shot may run: Settings' seconds at the
    /// model's fps, at the nearest point of its grid — 5 s is LTX's 121, H3's
    /// 124, Wan A14B's 81.
    pub fn longest(&self) -> u32 {
        let want = ((self.longest_secs * self.fps.max(1) as f32).round() as u32).max(1);
        let Some(f) = self.video_family() else { return want };
        let step = f.frame_grid().multiple;
        let a = f.align_frames(want);
        let (lo, hi) = if a >= want { (a.saturating_sub(step), a) } else { (a, a + step) };
        if lo >= f.frame_grid().min.max(1) && want - lo <= hi - want { lo } else { hi }
    }

    pub fn longest_seconds(&self) -> f32 {
        self.longest() as f32 / self.fps.max(1) as f32
    }

    /// A storyboard shot's frame count: the story's `seconds`, but never
    /// less than its words need ([`action_seconds`] — a small model asked for
    /// a length writes a second for anything), at most [`longest`], on the
    /// model's grid as the engine rounds it.
    pub fn frames_for(&self, seconds: f32, text: &str) -> Option<u32> {
        let family = self.video_family()?;
        let asked = if seconds.is_finite() { seconds.max(0.0) } else { 0.0 };
        let want = (asked.max(action_seconds(text)) * self.fps.max(1) as f32).round() as u32;
        let top = self.longest();
        // Rounding up (H3's grid) must not pass the top.
        Some(family.align_frames(want.clamp(1, top)).min(top))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelChoice {
    pub name: String,
    pub family: &'static str,
    pub label: &'static str,
    /// It follows reference pictures (`-r`).
    pub references: bool,
    /// What a chat switched to it is locked to: a comparison that switches
    /// model checks its values against these, not the old model's.
    #[serde(skip)]
    pub locks: Locks,
}

impl Toolkit {
    pub fn knob(&self, key: &str) -> Option<&KnobView> {
        self.knobs.iter().find(|k| k.key == key)
    }

    pub fn offered_knobs(&self) -> impl Iterator<Item = &KnobView> {
        self.knobs.iter().filter(|k| k.offered())
    }

    pub fn offered_actions(&self) -> impl Iterator<Item = &ActionView> {
        self.actions.iter().filter(|a| a.offered())
    }

    pub fn model_names(&self) -> Vec<String> {
        self.models.iter().map(|m| m.name.clone()).collect()
    }

    pub fn locks_of(&self, model: &str) -> Option<&Locks> {
        self.models.iter().find(|m| m.name == model).map(|m| &m.locks)
    }

    /// Whether the agent may write a storyboard in this chat.
    pub fn films(&self) -> bool {
        self.film_off.is_none()
    }

    /// It may make a cast member: allowed in Settings → Agent, and an image
    /// model to draw it with.
    pub fn casts(&self) -> bool {
        self.rules.cast && !self.models.is_empty()
    }
}

/// The models a chat could run: installed, whole, and not fill-only.
/// Each with its locks as a switch leaves it: no LoRA picks (a switch drops
/// them), so its Turbo LoRA when installed.
pub fn runnable(cfg: &Config, registry: &std::collections::BTreeMap<String, Profile>) -> Vec<ModelChoice> {
    registry
        .iter()
        .filter(|(_, p)| !p.family.fill_only() && p.missing().is_empty())
        .map(|(name, p)| ModelChoice {
            name: name.clone(),
            family: p.family.as_str(),
            label: p.family.label(),
            references: p.family.takes_references(),
            locks: locks_for(cfg, Some(p), &serde_json::json!({})),
        })
        .collect()
}

/// The family's catalog speed LoRA, when it is installed — by its own dest
/// and length, as the page's *Turbo* switch finds it. A chat that names no
/// LoRAs draws with it, as the Image tab does with Turbo on by default.
pub fn turbo_lora(cfg: &Config, family: ImageFamily) -> Option<&'static crate::catalog::Lora> {
    Catalog::get().lora.iter().find(|l| {
        l.family == crate::catalog::Family::Image(family)
            && std::fs::metadata(cfg.lora_dir().join(&l.dest)).is_ok_and(|m| m.is_file() && m.len() == l.bytes)
    })
}

/// The locks the chat's model and LoRA picks impose, and the Turbo LoRA's
/// advice. No `loras` key at all is the Turbo LoRA a round adds ([`turbo_lora`]).
pub fn locks_for(cfg: &Config, profile: Option<&Profile>, settings: &Value) -> Locks {
    let Some(p) = profile else {
        return Locks::default();
    };
    let catalog = Catalog::get();
    let distilled = catalog.distilled(&cfg.models_dir(), &p.model).is_some();
    let turbo = match settings.get("loras").and_then(|l| l.as_array()) {
        // A speed LoRA among the picks, by its catalog dest.
        Some(a) => {
            let picked: Vec<&str> = a.iter().filter_map(|r| r.get("name")?.as_str()).collect();
            catalog
                .lora
                .iter()
                .find(|l| picked.contains(&l.dest.as_str()) && l.family == crate::catalog::Family::Image(p.family))
        }
        None => turbo_lora(cfg, p.family),
    };
    let turbo = turbo.map(|l| format!("{}, tuned for {} steps at CFG {}", l.label, l.turbo.steps, l.turbo.cfg_scale));
    Locks::of(p.family.cfg_locked(), distilled, turbo.as_deref())
}

/// The chat's toolkit under the rules in `user/settings.toml` now.
pub fn resolve(cfg: &Config, chat: &Chat) -> Toolkit {
    let settings = crate::settings::Settings::load_or_default(&cfg.user_dir());
    let rules = settings.agent;
    let registry = cfg.registry();
    let template = chat.settings.get("template").and_then(|t| t.as_str()).unwrap_or("");
    let profile = registry.get(template);
    let locks = locks_for(cfg, profile, &chat.settings);
    let models = runnable(cfg, &registry);
    let image = models.iter().any(|m| m.name == template);
    let longest_secs = settings.storyboard_shot_seconds;
    let installed: Vec<VideoView> = cfg
        .video_models()
        .iter()
        .filter(|m| m.exists)
        .map(|m| VideoView { longest_secs, ..VideoView::of(m) })
        .collect();
    let video = chat.video.as_deref().and_then(|name| installed.iter().find(|v| v.name == name)).cloned();
    let mut t = build(&rules, &shown(cfg, chat, profile), &locks, models, profile.map(|p| p.family));
    t.film_off = match chat.context {
        // In context: a film is begun from the Video tab's panel.
        super::chat::Pane::Image => Some("films are made from the Assistant on the Video tab".into()),
        super::chat::Pane::Video => film_off(&rules, image, chat.video.as_deref(), video.as_ref()),
    };
    t.image_tab = chat.context == super::chat::Pane::Image;
    // A story turn reuses the saved cast; a `cast` action may change a
    // saved entry it names.
    if t.film_off.is_none() || t.casts() {
        t.library = crate::cast::CastStore::of(cfg)
            .list()
            .into_iter()
            .filter(|v| !v.entry.draft && v.picture)
            .map(|v| SavedCast { id: v.entry.id, name: v.entry.name.trim().to_string(), kind: v.entry.kind, look: v.entry.look })
            .collect();
        t.library = offered(std::mem::take(&mut t.library), chat);
    }
    t.video = video;
    t.videos = installed;
    t
}

/// The settings a round of the chat draws with, as the agent is shown them:
/// the chat's own, the Turbo LoRA's steps and CFG where a round adds it, and
/// the model's for the rest. The chat holds only what was set in it, and a
/// setting shown as null was answered with a guess — SD's CFG 7 on MOSS.
fn shown(cfg: &Config, chat: &Chat, profile: Option<&Profile>) -> Value {
    let mut s = chat.settings.clone();
    super::with_turbo(cfg, &mut s);
    if let (Some(p), Some(o)) = (profile, s.as_object_mut()) {
        fill_from(o, p);
    }
    s
}

fn fill_from(o: &mut serde_json::Map<String, Value>, p: &Profile) {
    for (k, v) in [
        ("cfg_scale", serde_json::json!(p.cfg_scale)),
        ("steps", serde_json::json!(p.steps)),
        ("width", serde_json::json!(p.width)),
        ("height", serde_json::json!(p.height)),
    ] {
        o.entry(k).or_insert(v);
    }
}

/// Why a chat can't make a film: it needs the agent allowed to, an image
/// model for the cast and keyframes, and a video model that goes on from
/// one shot to the next.
pub fn film_off(rules: &Rules, image: bool, picked: Option<&str>, video: Option<&VideoView>) -> Option<String> {
    if !rules.film {
        return Some("making movies is off in Settings → Agent".into());
    }
    if !image {
        return Some("the chat's image model isn't installed, and the cast's pictures are drawn with it".into());
    }
    let Some(v) = video else {
        return Some(match picked {
            Some(name) => format!("the video model '{name}' isn't installed whole"),
            None => "no video model is picked on the Video tab".into(),
        });
    };
    (!v.chains).then(|| {
        format!("{} can't go on from one shot to the next, so it can't make a film; Wan 2.2 or MiniMax H3 can", v.label)
    })
}

/// [`resolve`] past the reads, for tests.
pub fn build(rules: &Rules, settings: &Value, locks: &Locks, models: Vec<ModelChoice>, family: Option<ImageFamily>) -> Toolkit {
    let knobs = KNOBS
        .iter()
        .map(|k| {
            let (min, max) = hard_bounds(k.kind).map_or((None, None), |(a, b)| (Some(a), Some(b)));
            KnobView {
                key: k.key,
                label: k.label,
                help: k.help,
                kind: match k.kind {
                    Kind::Text { .. } => "text",
                    Kind::Int { .. } => "int",
                    Kind::Float { .. } => "float",
                    Kind::Size { .. } => "size",
                    Kind::Model => "model",
                },
                min,
                max,
                allowed: rules.allows(k),
                locked: locks.get(k.key).map(str::to_string).or_else(|| {
                    (k.kind == Kind::Model && models.len() < 2).then(|| "only one model is installed".to_string())
                }),
                value: knobs::value_of(settings, k),
                about: k.about,
                max_chars: match k.kind {
                    Kind::Text { max_chars } => Some(max_chars),
                    _ => None,
                },
            }
        })
        .collect();
    let actions = ACTIONS
        .iter()
        .map(|a| ActionView {
            key: a.key,
            label: a.label,
            help: a.help,
            allowed: rules.allows_action(a.key),
            unavailable: match (a.key, family) {
                ("edit", Some(f)) if !f.takes_references() => Some(format!("{} can't edit by instruction", f.label())),
                (_, None) => Some("the chat's model isn't installed".into()),
                _ => None,
            },
            about: a.about,
        })
        .collect();
    Toolkit {
        knobs,
        actions,
        models,
        family: family.map(ImageFamily::as_str),
        family_label: family.map(ImageFamily::label),
        locks: locks.clone(),
        video: None,
        videos: Vec::new(),
        film_off: Some("no video model".into()),
        image_tab: false,
        vision: false,
        rules: rules.clone(),
        library: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(n: u32, text: &str) -> super::super::chat::Msg {
        super::super::chat::Msg::User { n, ms: 0, text: text.into(), images: Vec::new() }
    }

    fn saved(id: &str, name: &str) -> SavedCast {
        SavedCast { id: id.into(), name: name.into(), ..Default::default() }
    }

    #[test]
    fn only_the_saved_cast_the_person_asks_for_is_offered() {
        let lib = || vec![saved("c1", "Elias"), saved("c2", "Rain")];
        let mut chat = super::super::chat::Chat::new("1-0".into(), serde_json::json!({}), None);
        chat.messages.push(user(1, "A clip of a camel and a parrot on a rainy day"));
        assert!(offered(lib(), &chat).is_empty(), "nothing named, nothing offered");
        chat.messages.push(user(2, "Now Elias rides the camel"));
        assert_eq!(offered(lib(), &chat), vec![saved("c1", "Elias")]);
        let mut chat = super::super::chat::Chat::new("1-0".into(), serde_json::json!({}), None);
        chat.messages.push(user(1, "Use my cast for this one"));
        assert_eq!(offered(lib(), &chat).len(), 2, "asked for as a whole");
        // Already in the film: kept on a later turn that doesn't name it.
        let mut chat = super::super::chat::Chat::new("1-0".into(), serde_json::json!({}), None);
        chat.messages.push(user(1, "Make it longer"));
        chat.film = Some(super::super::chat::Film {
            cast: vec![super::super::chat::Member { name: "Rain".into(), cast_id: Some("c2".into()), ..Default::default() }],
            ..Default::default()
        });
        assert_eq!(offered(lib(), &chat), vec![saved("c2", "Rain")]);
    }

    #[test]
    fn a_shot_runs_as_long_as_its_actions_take() {
        assert_eq!(action_seconds("A woman shows up in the frame and then kneels beside the oasis."), 5.0);
        assert_eq!(action_seconds("Close on Leo's face as he reads the letter."), 2.0);
        assert_eq!(action_seconds("She smiles, looks up, and laughs."), 4.0);
        assert_eq!(action_seconds(""), 2.0);
        // On LTX at 24 fps the oasis shot is 5 s whatever the agent asked, not
        // the 33 frames a new row starts at.
        let ltx = VideoView { family: "ltx_2_5", frames: 33, fps: 24, longest_secs: 5.0, ..Default::default() };
        assert_eq!(ltx.frames_for(0.7, "A woman shows up in the frame and then kneels beside the oasis."), Some(113));
        assert_eq!(ltx.frames_for(30.0, "She waits."), Some(121), "at most the longest shot");
        let h3 = VideoView { family: "minimax_h3", fps: 24, longest_secs: 5.0, ..Default::default() };
        let wan = VideoView { family: "wan22_a14b", fps: 16, longest_secs: 5.0, ..Default::default() };
        assert_eq!((h3.longest(), wan.longest()), (124, 81), "the nearest point of each grid");
        assert_eq!(VideoView { longest_secs: 10.0, ..ltx.clone() }.longest(), 241, "Settings raises it");
        assert!((ltx.longest_seconds() - 5.04).abs() < 0.01);
    }

    #[test]
    fn a_name_is_a_whole_word() {
        assert!(names("Elias lights a cigar", "Elias"));
        assert!(names("with Elias.", "Elias"));
        assert!(!names("Eliason lights a cigar", "Elias"));
        assert!(!names("a rainy day", "Rain"));
        assert!(names("Zoë et Élise", "Élise"));
    }

    fn choice(name: &str, f: ImageFamily) -> ModelChoice {
        ModelChoice { name: name.into(), family: f.as_str(), label: f.label(), references: f.takes_references(), locks: Locks::default() }
    }

    #[test]
    fn a_setting_the_chat_never_set_is_shown_at_the_models_value() {
        let p = Profile { cfg_scale: 4.5, steps: 28, ..Profile::default() };
        let mut o = serde_json::json!({"template": "moss", "steps": 12}).as_object().cloned().unwrap();
        fill_from(&mut o, &p);
        let t = build(&Rules::default(), &Value::Object(o), &Locks::default(), vec![], Some(ImageFamily::Sdxl));
        assert_eq!(t.knob("cfg_scale").unwrap().value, serde_json::json!(4.5), "not null, for the agent to guess 7 at");
        assert_eq!(t.knob("steps").unwrap().value, serde_json::json!(12), "the chat's own wins");
    }

    #[test]
    fn a_turbo_model_offers_neither_cfg_nor_negative() {
        let t = build(
            &Rules::default(),
            &serde_json::json!({"prompt": "x", "cfg_scale": 1.0}),
            &Locks::of(true, false, None),
            vec![choice("z", ImageFamily::ZImage)],
            Some(ImageFamily::ZImage),
        );
        let offered: Vec<&str> = t.offered_knobs().map(|k| k.key).collect();
        assert_eq!(offered, ["prompt", "size"], "steps is off by default; CFG and negative locked; one model");
        assert!(t.offered_actions().next().is_none(), "the passes are off by default");
        assert!(t.actions.iter().find(|a| a.key == "edit").unwrap().unavailable.is_some());
    }

    #[test]
    fn a_turbo_lora_advises_and_locks_nothing() {
        let mut rules = Rules::default();
        rules.knobs.insert("steps".into());
        let t = build(
            &rules,
            &serde_json::json!({"prompt": "x"}),
            &Locks::of(false, false, Some("MOSS DMD, tuned for 8 steps at CFG 1")),
            vec![choice("moss", ImageFamily::MossImage)],
            Some(ImageFamily::MossImage),
        );
        let offered: Vec<&str> = t.offered_knobs().map(|k| k.key).collect();
        for k in ["cfg_scale", "steps", "negative"] {
            assert!(offered.contains(&k), "{k} stays the agent's beside a Turbo LoRA");
        }
        assert!(t.locks.turbo.as_deref().is_some_and(|s| s.contains("8 steps")));
    }

    #[test]
    fn a_film_needs_the_rule_an_image_model_and_a_video_model_that_chains() {
        let wan = VideoView {
            name: "wan".into(), family: "wan22_ti2v_5b", label: "Wan 2.2 TI2V 5B", chains: true, references: false, sounds: false,
            width: 1280, height: 704, frames: 121, fps: 24, longest_secs: 5.0,
        };
        let rules = Rules::default();
        assert_eq!(film_off(&rules, true, Some("wan"), Some(&wan)), None);
        assert!(film_off(&rules, true, None, None).unwrap().contains("Video tab"));
        assert!(film_off(&rules, false, Some("wan"), Some(&wan)).unwrap().contains("image model"));
        let vace = VideoView { chains: false, label: "Wan 2.1 VACE", ..wan.clone() };
        assert!(film_off(&rules, true, Some("wan"), Some(&vace)).unwrap().contains("can't go on"));
        let off = Rules { film: false, ..Rules::default() };
        assert!(film_off(&off, true, Some("wan"), Some(&wan)).unwrap().contains("Settings"));
    }

    #[test]
    fn the_model_knob_offers_every_runnable_model_while_allowed() {
        let models = vec![choice("sdxl", ImageFamily::Sdxl), choice("klein", ImageFamily::Flux2Klein4b)];
        let s = serde_json::json!({"template": "sdxl"});
        let t = build(&Rules::default(), &s, &Locks::default(), models.clone(), Some(ImageFamily::Sdxl));
        assert!(t.knob("model").unwrap().offered());
        let mut r = Rules::default();
        r.knobs.remove("model");
        let t = build(&r, &s, &Locks::default(), models, Some(ImageFamily::Sdxl));
        assert!(!t.knob("model").unwrap().offered());
    }
}
