//! The knob table resolved for one chat: each knob's permission, bounds, lock
//! and current value, the passes and whether its model can run them, and the
//! models it may switch to. What `GET /chats/{id}` serves the Permissions
//! panel, what `agent::schema` builds a reply's grammar from, and what the
//! agent is told each turn — one value, so the three can't disagree.

use super::chat::Chat;
use super::knobs::{self, hard_bounds, Kind, Locks, Perm, Permissions, ACTIONS, KNOBS};
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
    pub hard_min: Option<f64>,
    pub hard_max: Option<f64>,
    pub perm: Perm,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub allowed: Vec<String>,
    /// Why the model rules it out, when it does.
    pub locked: Option<String>,
    pub value: Value,
    #[serde(skip)]
    pub about: &'static str,
    #[serde(skip)]
    pub max_chars: Option<usize>,
}

impl KnobView {
    /// Whether a reply may name it at all: switched on, and not locked.
    pub fn offered(&self) -> bool {
        self.perm != Perm::Off && self.locked.is_none()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ActionView {
    pub key: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub perm: Perm,
    /// Why the chat's model can't run it, when it can't.
    pub unavailable: Option<String>,
    #[serde(skip)]
    pub about: &'static str,
}

impl ActionView {
    pub fn offered(&self) -> bool {
        self.perm != Perm::Off && self.unavailable.is_none()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Toolkit {
    pub knobs: Vec<KnobView>,
    pub actions: Vec<ActionView>,
    /// Every model the chat's settings could name — the Settings panel's
    /// select and the allowed-models list's choices.
    pub models: Vec<ModelChoice>,
    /// The family of the chat's model, as `ImageFamily::as_str`.
    pub family: Option<&'static str>,
    pub family_label: Option<&'static str>,
    pub seed_lock: bool,
    pub batch: u32,
    pub rounds: u32,
    pub max_changes: u32,
    /// What the chat's model rules out, with the reasons.
    pub locks: Locks,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelChoice {
    pub name: String,
    pub family: &'static str,
    pub label: &'static str,
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
}

/// The models a chat could run: installed, whole, and not fill-only.
pub fn runnable(registry: &std::collections::BTreeMap<String, Profile>) -> Vec<ModelChoice> {
    registry
        .iter()
        .filter(|(_, p)| !p.family.fill_only() && p.missing().is_empty())
        .map(|(name, p)| ModelChoice { name: name.clone(), family: p.family.as_str(), label: p.family.label() })
        .collect()
}

/// The locks the chat's model and LoRA picks impose.
pub fn locks_for(cfg: &Config, profile: Option<&Profile>, settings: &Value) -> Locks {
    let Some(p) = profile else {
        return Locks::default();
    };
    let catalog = Catalog::get();
    let distilled = catalog.distilled(&cfg.models_dir(), &p.model).is_some();
    // A speed LoRA among the picks, by its catalog dest at its own length.
    let picked: Vec<String> = settings
        .get("loras")
        .and_then(|l| l.as_array())
        .map(|a| a.iter().filter_map(|r| r.get("name")?.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let turbo = catalog
        .lora
        .iter()
        .find(|l| picked.contains(&l.dest) && l.family == crate::catalog::Family::Image(p.family))
        .map(|l| l.label.as_str());
    Locks::of(p.family.cfg_locked(), distilled, turbo)
}

pub fn resolve(cfg: &Config, chat: &Chat) -> Toolkit {
    let registry = cfg.registry();
    let template = chat.settings.get("template").and_then(|t| t.as_str()).unwrap_or("");
    let profile = registry.get(template);
    let locks = locks_for(cfg, profile, &chat.settings);
    build(&chat.permissions, &chat.settings, &locks, runnable(&registry), profile.map(|p| p.family))
}

/// [`resolve`] past the reads, for tests.
pub fn build(
    perms: &Permissions,
    settings: &Value,
    locks: &Locks,
    models: Vec<ModelChoice>,
    family: Option<ImageFamily>,
) -> Toolkit {
    let knobs = KNOBS
        .iter()
        .map(|k| {
            let p = perms.knob(k);
            let (lo, hi) = hard_bounds(k.kind).map_or((None, None), |(a, b)| (Some(a), Some(b)));
            let bounds = perms.bounds(k);
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
                hard_min: lo,
                hard_max: hi,
                perm: p.perm,
                min: bounds.map(|b| b.0),
                max: bounds.map(|b| b.1),
                // Only what is still installed: a removed model can't be picked.
                allowed: p.allowed.iter().filter(|a| models.iter().any(|m| &m.name == *a)).cloned().collect(),
                locked: locks.get(k.key).map(str::to_string).or_else(|| {
                    (k.kind == Kind::Model && p.allowed.is_empty()).then(|| "no models are on its list".to_string())
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
            perm: perms.action(a.key),
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
        seed_lock: perms.seed_lock,
        batch: perms.batch,
        rounds: perms.rounds,
        max_changes: perms.max_changes,
        locks: locks.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_turbo_model_offers_neither_cfg_nor_negative() {
        let t = build(
            &Permissions::default(),
            &serde_json::json!({"prompt": "x", "cfg_scale": 1.0}),
            &Locks::of(true, false, None),
            vec![],
            Some(ImageFamily::ZImage),
        );
        let offered: Vec<&str> = t.offered_knobs().map(|k| k.key).collect();
        assert_eq!(offered, ["prompt"], "steps, size and model are off by default; CFG and negative locked");
        assert!(t.offered_actions().next().is_none(), "the passes are off by default");
        assert!(t.actions.iter().find(|a| a.key == "edit").unwrap().unavailable.is_some());
    }
}
