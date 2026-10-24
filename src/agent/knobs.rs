//! What the agent may change, and how far: the knob table, the person's
//! permissions over it, and the locks a model imposes.
//!
//! **One table, three readers.** [`KNOBS`] is what the Permissions panel draws
//! (a row each, its ⓘ the Image tab's own `HELP` text by id), what
//! `agent::schema` builds the reply grammar from — enabled, unlocked knobs
//! only, so a field the person switched off can't even be written — and what
//! [`apply`] checks a reply against, since a grammar doesn't enforce float
//! ranges and the server is the gate anyway. Bounds come from the same consts
//! `Profile::validate` uses; `agent_knobs_agree_with_fields_and_validate`
//! pins the table to the settings form and the validator.
//!
//! The values live in the chat's copy of a `/generate` body, as JSON
//! (`chat.settings`), so a knob reads and writes keys of that object.

use crate::config::{MAX_BASE_SIDE, MAX_CFG, MAX_STEPS};
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// Off, ask first, or apply.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Perm {
    #[default]
    Off,
    Ask,
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Text { max_chars: usize },
    Int { min: u32, max: u32 },
    Float { min: f32, max: f32 },
    /// Width and height together, each snapped to `multiple`.
    Size { min: u32, max: u32, multiple: u32 },
    /// The checkpoint, from the person's allowed list.
    Model,
}

pub struct Knob {
    /// The knob's name on the wire and in the reply's `change` object.
    pub key: &'static str,
    pub label: &'static str,
    /// The Image tab control whose `HELP` entry explains it (`hints.js`).
    pub help: &'static str,
    /// The `FIELDS` rows in `settings.js` it shares bounds with — what the
    /// pinning test compares, and only it reads.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fields: &'static [&'static str],
    /// The `/generate` keys it reads and writes.
    pub keys: &'static [&'static str],
    pub kind: Kind,
    pub default: Perm,
    /// The person's starting bounds, inside the hard ones.
    pub default_bounds: Option<(f64, f64)>,
    /// What the agent is told the knob does. For a small model: short, plain,
    /// and about choosing.
    pub about: &'static str,
}

pub const KNOBS: &[Knob] = &[
    Knob {
        key: "prompt",
        label: "Prompt",
        help: "prompt",
        fields: &[],
        keys: &["prompt"],
        kind: Kind::Text { max_chars: 2000 },
        default: Perm::Auto,
        default_bounds: None,
        about: "What the picture shows. The biggest lever: rewrite it to add what is missing and \
                say plainly what each thing looks like.",
    },
    Knob {
        key: "negative",
        label: "Negative prompt",
        help: "negative",
        fields: &[],
        keys: &["negative"],
        kind: Kind::Text { max_chars: 1000 },
        default: Perm::Auto,
        default_bounds: None,
        about: "What the picture should not show. Short lists of things, not sentences.",
    },
    Knob {
        key: "cfg_scale",
        label: "CFG scale",
        help: "cfg",
        fields: &["cfg_scale"],
        keys: &["cfg_scale"],
        kind: Kind::Float { min: 0.0, max: MAX_CFG },
        default: Perm::Auto,
        default_bounds: Some((1.0, 8.0)),
        about: "How strictly the model follows the prompt. Higher follows harder but looks \
                harsher; change it a little at a time.",
    },
    Knob {
        key: "steps",
        label: "Steps",
        help: "steps",
        fields: &["steps"],
        keys: &["steps"],
        kind: Kind::Int { min: 1, max: MAX_STEPS },
        default: Perm::Off,
        default_bounds: Some((4.0, 40.0)),
        about: "How many denoising steps. More is slower and finer; past the model's usual \
                number it rarely helps.",
    },
    Knob {
        key: "size",
        label: "Resolution",
        help: "w",
        fields: &["width", "height"],
        keys: &["width", "height"],
        kind: Kind::Size { min: 64, max: MAX_BASE_SIDE, multiple: 64 },
        default: Perm::Off,
        default_bounds: Some((512.0, 1536.0)),
        about: "The picture's width and height in pixels. Changes the framing: wide for \
                landscapes, tall for portraits.",
    },
    Knob {
        key: "model",
        label: "Model",
        help: "template",
        fields: &[],
        keys: &["template"],
        kind: Kind::Model,
        default: Perm::Off,
        default_bounds: None,
        about: "Which image model draws. Switching reloads weights and resets steps, CFG and \
                LoRAs to the new model's own.",
    },
];

/// The passes the agent may run on a picture it picked.
pub struct Action {
    pub key: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub about: &'static str,
}

pub const ACTIONS: &[Action] = &[
    Action {
        key: "hires",
        label: "Hi-res pass",
        help: "opthires",
        about: "Redraw a picture at twice the size, adding detail. For a keeper that is right \
                but small.",
    },
    Action {
        key: "refine",
        label: "Refine pass",
        help: "optrefine",
        about: "A light redraw at the same size that cleans up texture and small faults.",
    },
    Action {
        key: "edit",
        label: "Edit",
        help: "rdeditprompt",
        about: "Change one thing in a picture by instruction (\"make the hat red\"), keeping the \
                rest. Only some models can.",
    },
];

pub fn knob(key: &str) -> Option<&'static Knob> {
    KNOBS.iter().find(|k| k.key == key)
}

/// One knob's permission and the person's bounds.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct KnobPerm {
    #[serde(default)]
    pub perm: Perm,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// For `model`: the checkpoints it may pick. Empty is none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed: Vec<String>,
}

/// What a chat lets its agent do — a copy of the person's defaults
/// (`user/settings.toml`'s `[agent]`) taken when the chat starts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Permissions {
    // The plain values first and the two tables last: TOML wants a table's
    // own values ahead of its sub-tables (`ValueAfterTable`).
    /// The same seed for image *k* of every round, so a change is judged on
    /// the change alone.
    #[serde(default = "yes")]
    pub seed_lock: bool,
    /// Pictures per round.
    #[serde(default = "default_batch")]
    pub batch: u32,
    /// Rounds a session may run before it stops by itself.
    #[serde(default = "default_rounds")]
    pub rounds: u32,
    /// Knobs one reply may change. Small models do better changing one thing
    /// at a time and seeing what it did.
    #[serde(default = "default_max_changes")]
    pub max_changes: u32,
    /// Off/ask/auto per pass on a picture.
    #[serde(default)]
    pub actions: BTreeMap<String, Perm>,
    #[serde(default)]
    pub knobs: BTreeMap<String, KnobPerm>,
}

fn yes() -> bool {
    true
}
fn default_batch() -> u32 {
    4
}
fn default_rounds() -> u32 {
    3
}
fn default_max_changes() -> u32 {
    2
}

pub const MAX_BATCH: u32 = 8;
pub const MAX_ROUNDS: u32 = 10;

impl Default for Permissions {
    fn default() -> Self {
        Permissions {
            actions: ACTIONS.iter().map(|a| (a.key.to_string(), Perm::Off)).collect(),
            seed_lock: true,
            batch: default_batch(),
            rounds: default_rounds(),
            max_changes: default_max_changes(),
            knobs: KNOBS
                .iter()
                .map(|k| {
                    let (min, max) = k.default_bounds.map_or((None, None), |(a, b)| (Some(a), Some(b)));
                    (k.key.to_string(), KnobPerm { perm: k.default, min, max, allowed: Vec::new() })
                })
                .collect(),
        }
    }
}

impl Permissions {
    pub fn knob(&self, k: &Knob) -> KnobPerm {
        self.knobs.get(k.key).cloned().unwrap_or(KnobPerm {
            perm: k.default,
            min: k.default_bounds.map(|b| b.0),
            max: k.default_bounds.map(|b| b.1),
            allowed: Vec::new(),
        })
    }

    pub fn action(&self, key: &str) -> Perm {
        self.actions.get(key).copied().unwrap_or(Perm::Off)
    }

    /// The bounds a value is clamped to: the person's, inside the hard ones.
    pub fn bounds(&self, k: &Knob) -> Option<(f64, f64)> {
        let (lo, hi) = hard_bounds(k.kind)?;
        let p = self.knob(k);
        let lo = p.min.map_or(lo, |m| m.max(lo));
        let hi = p.max.map_or(hi, |m| m.min(hi));
        Some((lo, hi.max(lo)))
    }

    /// What the server accepts as a chat's permissions (`/chats/permissions`
    /// and `[agent]` in the settings file).
    pub fn validate(&self) -> Result<()> {
        for key in self.knobs.keys() {
            if knob(key).is_none() {
                bail!("the agent has no knob called '{key}'");
            }
        }
        for key in self.actions.keys() {
            if !ACTIONS.iter().any(|a| a.key == key) {
                bail!("the agent has no action called '{key}'");
            }
        }
        if !(1..=MAX_BATCH).contains(&self.batch) {
            bail!("pictures per round is {}, must be 1 to {MAX_BATCH}", self.batch);
        }
        if !(1..=MAX_ROUNDS).contains(&self.rounds) {
            bail!("rounds is {}, must be 1 to {MAX_ROUNDS}", self.rounds);
        }
        if !(1..=2).contains(&self.max_changes) {
            bail!("changes per reply is {}, must be 1 or 2", self.max_changes);
        }
        for k in KNOBS {
            let p = self.knob(k);
            if let (Some(a), Some(b)) = (p.min, p.max) {
                if !(a.is_finite() && b.is_finite()) || a > b {
                    bail!("{}: the lower bound {a} is above the upper bound {b}", k.label);
                }
            }
        }
        Ok(())
    }
}

pub fn hard_bounds(kind: Kind) -> Option<(f64, f64)> {
    match kind {
        Kind::Int { min, max } | Kind::Size { min, max, .. } => Some((min as f64, max as f64)),
        Kind::Float { min, max } => Some((min as f64, max as f64)),
        Kind::Text { .. } | Kind::Model => None,
    }
}

/// What the picked model rules out, with the reason the panel shows.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Locks {
    pub cfg_scale: Option<String>,
    pub steps: Option<String>,
    pub negative: Option<String>,
}

impl Locks {
    /// `family_cfg_locked`: `ImageFamily::cfg_locked`. `distilled`: a catalog
    /// file distilled to fixed settings (`Catalog::distilled`). `turbo`: a
    /// speed LoRA among the chat's picks, by its label.
    pub fn of(family_cfg_locked: bool, distilled: bool, turbo: Option<&str>) -> Locks {
        let mut l = Locks::default();
        if let Some(name) = turbo {
            let why = format!("{name} is picked: it was distilled to its own steps and CFG");
            l.steps = Some(why.clone());
            l.cfg_scale = Some(why);
        } else if distilled {
            let why = "this model file is distilled to its own steps and CFG".to_string();
            l.steps = Some(why.clone());
            l.cfg_scale = Some(why);
        } else if family_cfg_locked {
            l.cfg_scale = Some("this model runs without CFG (it stays at 1)".into());
        }
        if l.cfg_scale.is_some() {
            l.negative = Some("without CFG the model ignores the negative prompt".into());
        }
        l
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        match key {
            "cfg_scale" => self.cfg_scale.as_deref(),
            "steps" => self.steps.as_deref(),
            "negative" => self.negative.as_deref(),
            _ => None,
        }
    }
}

/// A knob's current value in a `/generate` body: the key itself, or for
/// `size` the pair.
pub fn value_of(settings: &Value, k: &Knob) -> Value {
    match k.kind {
        Kind::Size { .. } => serde_json::json!({
            "width": settings.get("width").cloned().unwrap_or(Value::Null),
            "height": settings.get("height").cloned().unwrap_or(Value::Null),
        }),
        _ => settings.get(k.keys[0]).cloned().unwrap_or(Value::Null),
    }
}

/// What a reply's change became, for the chat's record and its undo.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Applied {
    /// Knob key → the value it now has, as [`value_of`] spells it.
    pub set: Map<String, Value>,
    /// Knob key → the value it had.
    pub before: Map<String, Value>,
    /// What the server did to the reply on the way: a clamp, a dropped key.
    pub notes: Vec<String>,
}

/// Check a reply's `change` object against the permissions and bounds, and
/// write what passes into `settings`. `models` is the runnable registry;
/// `allow_ask` says whether `ask` knobs are applied (an approval) or only
/// `auto` ones (a reply).
///
/// Never trusts the grammar: an unknown, disabled or locked key is dropped
/// with a note; a number is clamped to the bounds; text is cut to its length.
pub fn apply(
    settings: &mut Value,
    change: &Map<String, Value>,
    perms: &Permissions,
    locks: &Locks,
    models: &[String],
    allowed_perms: &[Perm],
) -> Applied {
    let mut out = Applied::default();
    let mut n = 0;
    for (key, v) in change {
        let Some(k) = knob(key) else {
            out.notes.push(format!("'{key}' isn't a setting the agent has, so it was left alone"));
            continue;
        };
        if !allowed_perms.contains(&perms.knob(k).perm) {
            out.notes.push(format!("{} isn't the agent's to change, so it was left alone", k.label));
            continue;
        }
        if let Some(why) = locks.get(key) {
            out.notes.push(format!("{} is locked: {why}", k.label));
            continue;
        }
        if n >= perms.max_changes {
            out.notes.push(format!("{} was one change more than a reply may make, so it waits", k.label));
            continue;
        }
        let before = value_of(settings, k);
        let new = match coerce(k, v, perms, models, &mut out.notes) {
            Some(v) => v,
            None => continue,
        };
        if new == before {
            continue;
        }
        write(settings, k, &new);
        out.before.insert(key.clone(), before);
        out.set.insert(key.clone(), new);
        n += 1;
    }
    out
}

/// One value, checked and brought inside its bounds, or `None` (with a note)
/// for one that can't be used at all.
fn coerce(k: &Knob, v: &Value, perms: &Permissions, models: &[String], notes: &mut Vec<String>) -> Option<Value> {
    let clamp = |x: f64, notes: &mut Vec<String>| {
        let (lo, hi) = perms.bounds(k).expect("a numeric knob has bounds");
        let c = x.clamp(lo, hi);
        if c != x {
            notes.push(format!("{} {x} is outside {lo}–{hi}, so it is {c}", k.label));
        }
        c
    };
    match k.kind {
        Kind::Text { max_chars } => {
            let s = v.as_str()?.trim();
            if s.is_empty() && k.key == "prompt" {
                notes.push("an empty prompt was left alone".into());
                return None;
            }
            let cut: String = s.chars().take(max_chars).collect();
            if cut.len() < s.len() {
                notes.push(format!("{} was cut to {max_chars} characters", k.label));
            }
            Some(Value::String(cut))
        }
        Kind::Int { .. } => {
            let x = v.as_f64()?;
            Some(Value::from(clamp(x.round(), notes) as u32))
        }
        Kind::Float { .. } => {
            let x = v.as_f64().filter(|x| x.is_finite())?;
            // One decimal, as the boxes step.
            Some(serde_json::json!((clamp(x, notes) * 10.0).round() / 10.0))
        }
        Kind::Size { multiple, .. } => {
            let side = |key: &str, notes: &mut Vec<String>| -> Option<u32> {
                let x = v.get(key)?.as_f64()?;
                let snapped = ((clamp(x, notes) / multiple as f64).round() * multiple as f64) as u32;
                Some(snapped.max(multiple))
            };
            let (w, h) = (side("width", notes)?, side("height", notes)?);
            Some(serde_json::json!({ "width": w, "height": h }))
        }
        Kind::Model => {
            let name = v.as_str()?;
            let p = perms.knob(k);
            if !p.allowed.iter().any(|a| a == name) {
                notes.push(format!("'{name}' isn't on the models the agent may pick"));
                return None;
            }
            if !models.iter().any(|m| m == name) {
                notes.push(format!("'{name}' isn't installed"));
                return None;
            }
            Some(Value::String(name.to_string()))
        }
    }
}

/// Write a coerced value into the settings. A model switch is wholesale, as
/// an escalation's `swap_model` is: the new model brings its own steps, CFG,
/// sampler, schedule, LoRAs and parts, so the old model's are taken out
/// rather than carried onto a model they weren't chosen for.
pub fn write(settings: &mut Value, k: &Knob, v: &Value) {
    let Some(obj) = settings.as_object_mut() else { return };
    match k.kind {
        Kind::Size { .. } => {
            obj.insert("width".into(), v.get("width").cloned().unwrap_or(Value::Null));
            obj.insert("height".into(), v.get("height").cloned().unwrap_or(Value::Null));
        }
        Kind::Model => {
            if obj.get("template") != Some(v) {
                for key in ["steps", "cfg_scale", "sampler", "scheduler", "sigmas", "parts", "layout", "references"] {
                    obj.remove(key);
                }
                obj.insert("loras".into(), Value::Array(Vec::new()));
            }
            obj.insert("template".into(), v.clone());
        }
        _ => {
            obj.insert(k.keys[0].into(), v.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn settings() -> Value {
        json!({"prompt": "a cat", "template": "sdxl", "cfg_scale": 6.0, "steps": 30, "width": 1024, "height": 1024})
    }

    fn change(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn a_reply_changes_only_what_is_on_and_inside_its_bounds() {
        let mut s = settings();
        let p = Permissions::default();
        let a = apply(&mut s, &change(json!({"cfg_scale": 12.0, "steps": 50})), &p, &Locks::default(), &[], &[Perm::Auto]);
        assert_eq!(s["cfg_scale"], json!(8.0), "clamped to the person's 1–8");
        assert_eq!(s["steps"], json!(30), "steps is off by default");
        assert_eq!(a.before["cfg_scale"], json!(6.0));
        assert_eq!(a.notes.len(), 2, "{:?}", a.notes);
    }

    #[test]
    fn a_locked_knob_is_never_written() {
        let mut s = settings();
        let locks = Locks::of(true, false, None);
        let a = apply(&mut s, &change(json!({"cfg_scale": 3.0, "negative": "blur"})), &Permissions::default(), &locks, &[], &[Perm::Auto]);
        assert_eq!(s["cfg_scale"], json!(6.0));
        assert!(s.get("negative").is_none());
        assert!(a.set.is_empty());
    }

    #[test]
    fn more_changes_than_allowed_wait() {
        let mut s = settings();
        let p = Permissions { max_changes: 1, ..Permissions::default() };
        let a = apply(&mut s, &change(json!({"prompt": "a dog", "cfg_scale": 5.0})), &p, &Locks::default(), &[], &[Perm::Auto]);
        assert_eq!(a.set.len(), 1);
        assert!(a.notes[0].contains("waits"));
    }

    #[test]
    fn ask_knobs_wait_for_an_approval() {
        let mut s = settings();
        let mut p = Permissions::default();
        p.knobs.get_mut("prompt").unwrap().perm = Perm::Ask;
        let c = change(json!({"prompt": "a dog"}));
        assert!(apply(&mut s, &c, &p, &Locks::default(), &[], &[Perm::Auto]).set.is_empty());
        assert_eq!(apply(&mut s, &c, &p, &Locks::default(), &[], &[Perm::Auto, Perm::Ask]).set["prompt"], json!("a dog"));
    }

    #[test]
    fn a_size_snaps_and_a_model_switch_is_wholesale() {
        let mut s = json!({"prompt": "x", "template": "sdxl", "steps": 30, "loras": [{"name": "a.safetensors", "strength": 1.0}],
                           "width": 1024, "height": 1024});
        let mut p = Permissions::default();
        p.knobs.get_mut("size").unwrap().perm = Perm::Auto;
        p.knobs.get_mut("model").unwrap().perm = Perm::Auto;
        p.knobs.get_mut("model").unwrap().allowed = vec!["klein".into()];
        let models = vec!["sdxl".to_string(), "klein".to_string()];
        apply(&mut s, &change(json!({"size": {"width": 1000, "height": 700}, "model": "klein"})), &p, &Locks::default(), &models, &[Perm::Auto]);
        assert_eq!((s["width"].clone(), s["height"].clone()), (json!(1024), json!(704)));
        assert_eq!(s["template"], json!("klein"));
        assert!(s.get("steps").is_none(), "the new model brings its own");
        assert_eq!(s["loras"], json!([]));
        // Not on the allowed list, or not installed: left alone.
        let a = apply(&mut s, &change(json!({"model": "sdxl"})), &p, &Locks::default(), &models, &[Perm::Auto]);
        assert!(a.set.is_empty() && !a.notes.is_empty());
    }

    #[test]
    fn permissions_round_trip_through_toml_and_refuse_nonsense() {
        let p = Permissions::default();
        let text = toml::to_string(&p).unwrap();
        assert_eq!(toml::from_str::<Permissions>(&text).unwrap(), p);
        let bad = Permissions { batch: 0, ..Permissions::default() };
        assert!(bad.validate().is_err());
        let mut bad = Permissions::default();
        bad.knobs.insert("seed".into(), KnobPerm::default());
        assert!(bad.validate().is_err());
    }

    /// The table against the settings form and the validator: the knobs share
    /// `FIELDS` rows with the same bounds, the ⓘ ids exist, and
    /// `Profile::validate` takes the top of each range and refuses one past it.
    #[test]
    fn agent_knobs_agree_with_fields_and_validate() {
        let fields = String::from_utf8(crate::web::lookup("/js/settings.js").unwrap().bytes.to_vec()).unwrap();
        let hints = String::from_utf8(crate::web::lookup("/js/hints.js").unwrap().bytes.to_vec()).unwrap();
        for k in KNOBS {
            assert!(hints.contains(&format!("'{}'", k.help)), "{}'s help id {:?} is in no HELP entry", k.key, k.help);
            for f in k.fields {
                let row = fields
                    .lines()
                    .find(|l| l.contains(&format!("key: '{f}'")))
                    .unwrap_or_else(|| panic!("no FIELDS row for {f}"));
                let num = |name: &str| {
                    row.split(&format!("{name}: ")).nth(1).and_then(|r| r.split([',', ' ', '}']).next()).and_then(|n| n.parse::<f64>().ok())
                };
                let (lo, hi) = hard_bounds(k.kind).unwrap();
                assert_eq!(num("min"), Some(lo), "{f}'s FIELDS min");
                if let Some(max) = num("max") {
                    assert_eq!(max, hi, "{f}'s FIELDS max");
                }
            }
        }
        for a in ACTIONS {
            assert!(hints.contains(&format!("'{}'", a.help)), "{}'s help id", a.key);
        }
        let ok = crate::config::Profile::default();
        let take = |f: &dyn Fn(&mut crate::config::Profile)| {
            let mut p = ok.clone();
            f(&mut p);
            p.validate().is_ok()
        };
        assert!(take(&|p| p.cfg_scale = MAX_CFG) && !take(&|p| p.cfg_scale = MAX_CFG + 0.1));
        assert!(take(&|p| p.steps = MAX_STEPS) && !take(&|p| p.steps = MAX_STEPS + 1));
        assert!(take(&|p| p.width = MAX_BASE_SIDE) && !take(&|p| p.width = MAX_BASE_SIDE + 1));
    }
}
