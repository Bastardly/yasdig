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
    /// The whitelist it was built under, for `knobs::apply`.
    #[serde(skip)]
    pub rules: Rules,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelChoice {
    pub name: String,
    pub family: &'static str,
    pub label: &'static str,
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
    build(&rules, &chat.settings, &locks, runnable(cfg, &registry), profile.map(|p| p.family))
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
        rules: rules.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choice(name: &str, f: ImageFamily) -> ModelChoice {
        ModelChoice { name: name.into(), family: f.as_str(), label: f.label(), locks: Locks::default() }
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
