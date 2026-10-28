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
}

/// A video model as a film needs it described: what its shots can do, and
/// the size and length the Video tab gives each one.
#[derive(Debug, Clone, Serialize, PartialEq)]
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
    /// The most a shot may run: the model's own length, what fits this card.
    pub frames: u32,
    pub fps: u32,
}

impl VideoView {
    pub fn of(m: &crate::config::VideoModel) -> VideoView {
        VideoView {
            name: m.name.clone(),
            family: m.family.as_str(),
            label: m.family.label(),
            chains: m.family.chains(),
            references: m.family.takes_references(),
            sounds: m.family.takes_references() && m.audio_vae.is_some(),
            width: m.width,
            height: m.height,
            frames: m.frames,
            fps: m.effective_fps(),
        }
    }

    /// A shot's length in seconds, at most.
    pub fn shot_seconds(&self) -> f32 {
        self.frames as f32 / self.fps.max(1) as f32
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
    let rules = crate::settings::Settings::load_or_default(&cfg.user_dir()).agent;
    let registry = cfg.registry();
    let template = chat.settings.get("template").and_then(|t| t.as_str()).unwrap_or("");
    let profile = registry.get(template);
    let locks = locks_for(cfg, profile, &chat.settings);
    let models = runnable(cfg, &registry);
    let image = models.iter().any(|m| m.name == template);
    let installed: Vec<VideoView> = cfg.video_models().iter().filter(|m| m.exists).map(VideoView::of).collect();
    let video = chat.video.as_deref().and_then(|name| installed.iter().find(|v| v.name == name)).cloned();
    let mut t = build(&rules, &shown(cfg, chat, profile), &locks, models, profile.map(|p| p.family));
    t.film_off = match chat.context {
        // In context: a film is begun from the Video tab's panel.
        super::chat::Pane::Image => Some("films are made from the Assistant on the Video tab".into()),
        super::chat::Pane::Video => film_off(&rules, image, chat.video.as_deref(), video.as_ref()),
    };
    t.image_tab = chat.context == super::chat::Pane::Image;
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            width: 1280, height: 704, frames: 121, fps: 24,
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
