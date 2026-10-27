//! What the agent may change: the knob table, the person's whitelist over it
//! (`[agent]` in `user/settings.toml`), and the locks a model imposes.
//!
//! **One table, three readers.** [`KNOBS`] is what Settings → Agent draws (a
//! checkbox each, its ⓘ the Image tab's own `HELP` text by id), what
//! `agent::schema` builds the reply grammar from — allowed, unlocked knobs
//! only, so a field the person switched off can't even be written — and what
//! [`apply`] checks a reply against, since a grammar doesn't enforce float
//! ranges and the server is the gate anyway. Bounds are the consts
//! `Profile::validate` uses; `agent_knobs_agree_with_fields_and_validate`
//! pins the table to the settings form and the validator.
//!
//! The values live in the chat's copy of a `/generate` body, as JSON
//! (`chat.settings`), so a knob reads and writes keys of that object.

use crate::config::{MAX_BASE_SIDE, MAX_CFG, MAX_STEPS};
use anyhow::{bail, Result};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Text { max_chars: usize },
    Int { min: u32, max: u32 },
    Float { min: f32, max: f32 },
    /// Width and height together, each snapped to `multiple`.
    Size { min: u32, max: u32, multiple: u32 },
    /// The checkpoint, from the runnable models.
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
    /// On the whitelist until the person takes it off.
    pub default: bool,
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
        default: true,
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
        default: true,
        about: "What the picture should not show. Short lists of things, not sentences.",
    },
    Knob {
        key: "cfg_scale",
        label: "CFG scale",
        help: "cfg",
        fields: &["cfg_scale"],
        keys: &["cfg_scale"],
        kind: Kind::Float { min: 0.0, max: MAX_CFG },
        default: true,
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
        default: false,
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
        default: true,
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
        default: true,
        about: "Which image model draws. Switching reloads weights and resets steps, CFG, size \
                and LoRAs to the new model's own.",
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

/// What the agent may do in every chat — `user/settings.toml`'s `[agent]`,
/// read afresh each turn, so an edit in Settings reaches a chat mid-session.
/// A whitelist: a knob or pass is the agent's when its key is in the set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rules {
    /// The agent (LLM) a new chat starts with; `None` is the first installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default = "default_knobs", deserialize_with = "knob_set")]
    pub knobs: BTreeSet<String>,
    #[serde(default, deserialize_with = "action_set")]
    pub actions: BTreeSet<String>,
    /// It may make a movie: write a storyboard and draw its pictures. On by
    /// default, since nothing is filmed until the person says go.
    #[serde(default = "yes")]
    pub film: bool,
    /// How many times a film's shot is filmed again when a check fails.
    #[serde(default = "default_retakes")]
    pub film_retakes: u32,
}

/// One more take of a failing shot, unless Settings says otherwise.
pub const DEFAULT_RETAKES: u32 = 1;

fn default_retakes() -> u32 {
    DEFAULT_RETAKES
}

fn yes() -> bool {
    true
}

/// The most rounds a session runs and pictures a round draws, whatever the
/// agent writes: the only caps, so a misread goal can't run all night.
pub const MAX_BATCH: u32 = 8;
pub const MAX_ROUNDS: u32 = 6;

fn default_knobs() -> BTreeSet<String> {
    KNOBS.iter().filter(|k| k.default).map(|k| k.key.to_string()).collect()
}

/// A list of keys, or — from a settings file written before the whitelist,
/// whose `knobs`/`actions` were tables of off/ask/auto — `None`, and the
/// defaults stand.
fn key_list<'de, D: Deserializer<'de>>(d: D) -> Result<Option<BTreeSet<String>>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Either {
        List(BTreeSet<String>),
        Old(serde::de::IgnoredAny),
    }
    Ok(match Either::deserialize(d)? {
        Either::List(s) => Some(s),
        Either::Old(_) => None,
    })
}

fn knob_set<'de, D: Deserializer<'de>>(d: D) -> Result<BTreeSet<String>, D::Error> {
    Ok(key_list(d)?.unwrap_or_else(default_knobs))
}

fn action_set<'de, D: Deserializer<'de>>(d: D) -> Result<BTreeSet<String>, D::Error> {
    Ok(key_list(d)?.unwrap_or_default())
}

impl Default for Rules {
    fn default() -> Self {
        Rules { agent: None, knobs: default_knobs(), actions: BTreeSet::new(), film: true, film_retakes: DEFAULT_RETAKES }
    }
}

impl Rules {
    pub fn allows(&self, k: &Knob) -> bool {
        self.knobs.contains(k.key)
    }

    pub fn allows_action(&self, key: &str) -> bool {
        self.actions.contains(key)
    }

    /// What the server accepts as `[agent]`.
    pub fn validate(&self) -> Result<()> {
        if let Some(key) = self.knobs.iter().find(|k| knob(k).is_none()) {
            bail!("the agent has no knob called '{key}'");
        }
        if let Some(key) = self.actions.iter().find(|k| !ACTIONS.iter().any(|a| a.key == k.as_str())) {
            bail!("the agent has no action called '{key}'");
        }
        Ok(())
    }
}

/// yasdig's own limits on a number knob — `Profile::validate`'s.
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
    /// A speed LoRA among the chat's picks and what it was tuned for. Advice,
    /// never a lock: the Image tab's Turbo only sets steps and CFG, and either
    /// can still be changed there (MOSS-Image's DMD LoRA takes a real CFG).
    pub turbo: Option<String>,
}

impl Locks {
    /// `family_cfg_locked`: `ImageFamily::cfg_locked`. `distilled`: a catalog
    /// file distilled to fixed settings (`Catalog::distilled`). `turbo`: a
    /// speed LoRA among the chat's picks, described.
    pub fn of(family_cfg_locked: bool, distilled: bool, turbo: Option<&str>) -> Locks {
        let mut l = Locks { turbo: turbo.map(str::to_string), ..Locks::default() };
        if distilled {
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

/// Check a reply's `change` object against the whitelist and the locks, and
/// write what passes into `settings`. `models` is the runnable registry.
///
/// Never trusts the grammar: an unknown, disallowed or locked key is dropped
/// with a note; a number is clamped to yasdig's limits; text is cut to its
/// length.
///
/// The model goes first: a switch clears steps, CFG and size, and the rest
/// are checked against the locks `locks_of` gives the new model — "SDXL at
/// CFG 7" from a chat on a model without CFG is one change.
pub fn apply(
    settings: &mut Value,
    change: &Map<String, Value>,
    rules: &Rules,
    locks: &Locks,
    models: &[String],
    locks_of: &dyn Fn(&str) -> Option<Locks>,
) -> Applied {
    let mut out = Applied::default();
    let mut locks = locks.clone();
    let order = change.iter().filter(|(k, _)| *k == "model").chain(change.iter().filter(|(k, _)| *k != "model"));
    for (key, v) in order {
        let Some(new) = check(key, v, rules, &locks, models, &mut out.notes) else { continue };
        if key == "model" {
            if let Some(l) = new.as_str().and_then(locks_of) {
                locks = l;
            }
        }
        let k = knob(key).expect("check knows the knob");
        let before = value_of(settings, k);
        if new == before {
            continue;
        }
        write(settings, k, &new);
        out.before.insert(key.clone(), before);
        out.set.insert(key.clone(), new);
    }
    out
}

/// One value for knob `key` as [`apply`] would write it: on the whitelist,
/// unlocked, inside its bounds — or `None`, with a note saying why not.
pub fn check(key: &str, v: &Value, rules: &Rules, locks: &Locks, models: &[String], notes: &mut Vec<String>) -> Option<Value> {
    let Some(k) = knob(key) else {
        notes.push(format!("'{key}' isn't a setting the agent has, so it was left alone"));
        return None;
    };
    if !rules.allows(k) {
        notes.push(format!("{} isn't the agent's to change, so it was left alone", k.label));
        return None;
    }
    if let Some(why) = locks.get(key) {
        notes.push(format!("{} is locked: {why}", k.label));
        return None;
    }
    coerce(k, v, models, notes)
}

/// The knobs a comparison may vary: one value per round says something on
/// its own. Size is left out — a different shape is a different composition
/// from the same seed.
pub const COMPARABLE: [&str; 5] = ["cfg_scale", "steps", "prompt", "negative", "model"];

/// A comparison's value as text for a number knob's box: what a reply wrote
/// as a string read as the number it says.
pub fn parse_value(key: &str, v: &Value) -> Value {
    match (knob(key).map(|k| k.kind), v.as_str()) {
        (Some(Kind::Int { .. } | Kind::Float { .. }), Some(s)) => {
            s.trim().parse::<f64>().ok().and_then(serde_json::Number::from_f64).map_or(Value::Null, Value::Number)
        }
        _ => v.clone(),
    }
}

/// One value of a knob in a line a person reads: "CFG 5", "Model klein",
/// "Prompt “a fox in a…”".
pub fn value_label(key: &str, v: &Value) -> String {
    let label = knob(key).map_or(key, |k| k.label);
    match v {
        Value::String(s) if s.chars().count() > 48 => format!("{label} “{}…”", s.chars().take(48).collect::<String>()),
        Value::String(s) if matches!(knob(key).map(|k| k.kind), Some(Kind::Text { .. })) => format!("{label} “{s}”"),
        Value::String(s) => format!("{label} {s}"),
        Value::Null => format!("{label} (unset)"),
        // 4.0 reads as 4: the person typed 4.
        Value::Number(n) if n.as_f64().is_some_and(|x| x.fract() == 0.0 && x.abs() < 1e9) => format!("{label} {}", n.as_f64().unwrap() as i64),
        other => format!("{label} {other}"),
    }
}

/// One value, checked and brought inside its bounds, or `None` (with a note)
/// for one that can't be used at all.
fn coerce(k: &Knob, v: &Value, models: &[String], notes: &mut Vec<String>) -> Option<Value> {
    let clamp = |x: f64, notes: &mut Vec<String>| {
        let (lo, hi) = hard_bounds(k.kind).expect("a numeric knob has bounds");
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
            if !models.iter().any(|m| m == name) {
                notes.push(format!("'{name}' isn't an installed model"));
                return None;
            }
            Some(Value::String(name.to_string()))
        }
    }
}

/// Write a coerced value into the settings. A model switch is wholesale, as
/// an escalation's `swap_model` is: the new model brings its own steps, CFG,
/// size, sampler, schedule, LoRAs and parts, so the old model's are taken out
/// rather than carried onto a model they weren't chosen for. The prompt and
/// negative stay — they are what the picture is.
pub fn write(settings: &mut Value, k: &Knob, v: &Value) {
    let Some(obj) = settings.as_object_mut() else { return };
    match k.kind {
        Kind::Size { .. } => {
            obj.insert("width".into(), v.get("width").cloned().unwrap_or(Value::Null));
            obj.insert("height".into(), v.get("height").cloned().unwrap_or(Value::Null));
        }
        Kind::Model => {
            if obj.get("template") != Some(v) {
                for key in [
                    "steps", "cfg_scale", "width", "height", "sampler", "scheduler", "sigmas", "parts", "layout",
                    "references", "loras",
                ] {
                    obj.remove(key);
                }
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
    fn a_reply_changes_only_what_is_allowed_inside_yasdigs_limits() {
        let mut s = settings();
        let a = apply(&mut s, &change(json!({"cfg_scale": 99.0, "steps": 50})), &Rules::default(), &Locks::default(), &[], &|_| None);
        assert_eq!(s["cfg_scale"], json!(MAX_CFG as f64), "clamped to what Profile::validate takes");
        assert_eq!(s["steps"], json!(30), "steps is off by default");
        assert_eq!(a.before["cfg_scale"], json!(6.0));
        assert_eq!(a.notes.len(), 2, "{:?}", a.notes);
        // Taken off the list, CFG is left alone too.
        let mut r = Rules::default();
        r.knobs.remove("cfg_scale");
        assert!(apply(&mut s, &change(json!({"cfg_scale": 3.0})), &r, &Locks::default(), &[], &|_| None).set.is_empty());
    }

    #[test]
    fn a_switch_of_model_comes_first_and_its_locks_decide_the_rest() {
        let mut s = settings();
        s["template"] = json!("flux");
        let models = ["flux".to_string(), "sdxl".to_string()];
        let no_cfg = Locks::of(true, false, None);
        let locks_of = |m: &str| Some(if m == "sdxl" { Locks::default() } else { Locks::of(true, false, None) });
        // CFG named before the model, as a reply may: still checked on SDXL.
        let a = apply(&mut s, &change(json!({"cfg_scale": 7.0, "model": "sdxl"})), &Rules::default(), &no_cfg, &models, &locks_of);
        assert_eq!((s["template"].clone(), s["cfg_scale"].clone()), (json!("sdxl"), json!(7.0)), "{:?}", a.notes);
        // Onto a model without CFG, it stays locked.
        let a = apply(&mut s, &change(json!({"model": "flux", "cfg_scale": 5.0})), &Rules::default(), &Locks::default(), &models, &locks_of);
        assert!(s.get("cfg_scale").is_none() && a.notes.iter().any(|n| n.contains("locked")), "{s} {:?}", a.notes);
    }

    #[test]
    fn a_locked_knob_is_never_written() {
        let mut s = settings();
        let locks = Locks::of(true, false, None);
        let a = apply(&mut s, &change(json!({"cfg_scale": 3.0, "negative": "blur"})), &Rules::default(), &locks, &[], &|_| None);
        assert_eq!(s["cfg_scale"], json!(6.0));
        assert!(s.get("negative").is_none());
        assert!(a.set.is_empty());
    }

    #[test]
    fn a_size_snaps_and_a_model_switch_is_wholesale() {
        let mut s = json!({"prompt": "x", "negative": "blur", "template": "sdxl", "steps": 30, "cfg_scale": 5.0,
                           "loras": [{"name": "a.safetensors", "strength": 1.0}], "width": 1024, "height": 1024});
        let models = vec!["sdxl".to_string(), "klein".to_string()];
        apply(&mut s, &change(json!({"size": {"width": 1000, "height": 700}})), &Rules::default(), &Locks::default(), &models, &|_| None);
        assert_eq!((s["width"].clone(), s["height"].clone()), (json!(1024), json!(704)));
        apply(&mut s, &change(json!({"model": "klein"})), &Rules::default(), &Locks::default(), &models, &|_| None);
        assert_eq!(s, json!({"prompt": "x", "negative": "blur", "template": "klein"}), "the new model brings its own");
        // Not installed: left alone.
        let a = apply(&mut s, &change(json!({"model": "flux"})), &Rules::default(), &Locks::default(), &models, &|_| None);
        assert!(a.set.is_empty() && !a.notes.is_empty());
    }

    #[test]
    fn rules_round_trip_through_toml_and_refuse_nonsense() {
        let r = Rules { agent: Some("qwen".into()), ..Rules::default() };
        let text = toml::to_string(&r).unwrap();
        assert_eq!(toml::from_str::<Rules>(&text).unwrap(), r);
        let mut bad = Rules::default();
        bad.knobs.insert("seed".into());
        assert!(bad.validate().is_err());
        let mut bad = Rules::default();
        bad.actions.insert("upscale".into());
        assert!(bad.validate().is_err());
    }

    /// The `[agent]` table as the per-chat permissions wrote it: tables of
    /// off/ask/auto and round rules. It loads, as the defaults.
    #[test]
    fn an_old_agent_table_loads_as_the_defaults() {
        let old = r#"
            seed_lock = true
            batch = 4
            rounds = 3
            max_changes = 2

            [actions]
            hires = "ask"

            [knobs.prompt]
            perm = "auto"

            [knobs.cfg_scale]
            perm = "auto"
            min = 1.0
            max = 8.0
        "#;
        assert_eq!(toml::from_str::<Rules>(old).unwrap(), Rules::default());
        assert_eq!(serde_json::from_value::<Rules>(json!({})).unwrap(), Rules::default());
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
