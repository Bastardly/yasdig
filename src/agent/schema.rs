//! The shape of a reply, as a JSON schema llama.cpp turns into the grammar
//! the reply is sampled under.
//!
//! **Built from what the toolkit offers, nothing more**: a knob that is off or
//! locked is not a property, a pass that is off is not an `action`, so a small
//! model cannot write what it may not do. The server checks the reply anyway
//! (`knobs::apply`) — llama.cpp's grammar holds integer ranges, lengths,
//! enums and array sizes, but not a float's range or a count of properties.
//!
//! `notes` comes first in every mode: a small model reasons better when it
//! writes before it decides, and with thinking switched off this is where it
//! does. It is also what a later turn is shown of this one.
//!
//! **Every top-level property is required**, because llama.cpp's grammar
//! writes all required properties first and the optional ones after them,
//! whatever the schema's order. With `change` optional, a chat reply had to
//! write `reply` and `action` before it could reach `change` — so the model put
//! its new prompt in the reply, chose "generate", skipped `change`, and the
//! round drew the old prompt. An empty `change` is `{}`, an absent escalation
//! `null`. **Choosing "generate" requires a prompt** wherever the prompt is
//! the agent's to write (`anyOf` on `action`), so a drawing is always of what
//! the agent just wrote; repeating the current prompt is no change.

use super::chat::Mode;
use super::toolkit::{KnobView, Toolkit};
use serde_json::{json, Map, Value};

pub const NOTES_CHARS: u32 = 600;
pub const REPLY_CHARS: u32 = 800;
pub const MAX_CHECKS: u32 = 6;

fn text(max: u32) -> Value {
    json!({ "type": "string", "maxLength": max })
}

fn knob_schema(k: &KnobView, t: &Toolkit) -> Value {
    match k.kind {
        "text" => text(k.max_chars.unwrap_or(1000) as u32),
        "int" => json!({ "type": "integer", "minimum": k.min.unwrap_or(1.0) as i64, "maximum": k.max.unwrap_or(1.0) as i64 }),
        "float" => json!({ "type": "number" }),
        "size" => {
            let side = json!({ "type": "integer", "minimum": k.min.unwrap_or(64.0) as i64, "maximum": k.max.unwrap_or(4096.0) as i64 });
            json!({
                "type": "object",
                "properties": { "width": side, "height": side },
                "required": ["width", "height"],
                "additionalProperties": false
            })
        }
        _ => {
            let names: Vec<&String> = k.allowed.iter().filter(|a| t.models.iter().any(|m| &m.name == *a)).collect();
            json!({ "enum": names })
        }
    }
}

/// Whether the prompt is the agent's to write this turn.
pub fn writes_prompt(t: &Toolkit) -> bool {
    t.offered_knobs().any(|k| k.key == "prompt")
}

/// The `change` object, or `None` when nothing may be changed. With
/// `need_prompt` (and the prompt offered) the prompt must be in it.
fn change(t: &Toolkit, need_prompt: bool) -> Option<Value> {
    let mut props = Map::new();
    for k in t.offered_knobs() {
        if k.kind == "model" && k.allowed.is_empty() {
            continue;
        }
        props.insert(k.key.to_string(), knob_schema(k, t));
    }
    if props.is_empty() {
        return None;
    }
    let mut c = json!({ "type": "object", "properties": props, "additionalProperties": false });
    if need_prompt && writes_prompt(t) {
        c["required"] = json!(["prompt"]);
    }
    Some(c)
}

/// An object whose properties are all required, so they are written in
/// this order (see the module comment).
fn object(props: Map<String, Value>) -> Value {
    let required: Vec<&String> = props.keys().collect();
    json!({ "type": "object", "properties": props, "required": required, "additionalProperties": false })
}

/// `change` and `reason`, when anything may be changed.
fn with_change(props: &mut Map<String, Value>, t: &Toolkit, need_prompt: bool) {
    if let Some(c) = change(t, need_prompt) {
        props.insert("change".into(), c);
        props.insert("reason".into(), text(300));
    }
}

/// The reply's schema for one turn. `batch` is how many pictures a critique
/// shows, `checks` how many questions each is judged by.
pub fn output(mode: Mode, t: &Toolkit, batch: u32, checks: u32) -> Value {
    let mut props = Map::new();
    props.insert("notes".into(), text(NOTES_CHARS));
    match mode {
        Mode::Describe => {
            props.insert("prompt".into(), text(1500));
            // Only where the model reads one.
            if t.knob("negative").is_some_and(|k| k.locked.is_none()) {
                props.insert("negative".into(), text(500));
            }
            object(props)
        }
        Mode::Plan => {
            props.insert(
                "checks".into(),
                json!({ "type": "array", "items": text(120), "minItems": 1, "maxItems": MAX_CHECKS }),
            );
            // A plan is followed by the first round, so it writes that round's
            // prompt — unless a round was just drawn (`batch` > 0), which it judges first.
            with_change(&mut props, t, batch == 0);
            props.insert("reply".into(), text(REPLY_CHARS));
            object(props)
        }
        Mode::Critique => {
            let b = batch.max(1);
            let n = checks.max(1);
            props.insert(
                "checks".into(),
                json!({
                    "type": "array",
                    "items": { "type": "array", "items": { "type": "boolean" }, "minItems": n, "maxItems": n },
                    "minItems": b,
                    "maxItems": b
                }),
            );
            props.insert(
                "keepers".into(),
                json!({ "type": "array", "items": { "type": "integer", "minimum": 1, "maximum": b }, "maxItems": b }),
            );
            let passes: Vec<&str> = t.offered_actions().map(|a| a.key).collect();
            let mut actions = vec!["generate", "done"];
            let escalate = (!passes.is_empty()).then(|| {
                actions.push("escalate");
                json!({
                    "anyOf": [
                        {
                            "type": "object",
                            "properties": {
                                "image": { "type": "integer", "minimum": 1, "maximum": b },
                                "pass": { "enum": passes },
                                "instruction": text(300)
                            },
                            "required": ["image", "pass", "instruction"],
                            "additionalProperties": false
                        },
                        { "type": "null" }
                    ]
                })
            });
            props.insert("action".into(), json!({ "enum": actions }));
            if let Some(e) = escalate {
                props.insert("escalate".into(), e);
            }
            // Not a required prompt here: the next round may as well be a CFG change.
            with_change(&mut props, t, false);
            props.insert("reply".into(), text(REPLY_CHARS));
            object(props)
        }
        Mode::Chat => {
            // Talking, or drawing: a drawing says how many pictures (one asked
            // for is one drawn) and whether to keep the last round's seeds,
            // which keeps its compositions while a change is tried.
            let mut talk = props.clone();
            talk.insert("action".into(), json!({ "const": "none" }));
            with_change(&mut talk, t, false);
            talk.insert("reply".into(), text(REPLY_CHARS));
            let mut draw = props;
            draw.insert("action".into(), json!({ "const": "generate" }));
            draw.insert("count".into(), json!({ "type": "integer", "minimum": 1, "maximum": t.batch.max(1) }));
            draw.insert("seeds".into(), json!({ "enum": ["new", "same"] }));
            with_change(&mut draw, t, true);
            draw.insert("reply".into(), text(REPLY_CHARS));
            json!({ "anyOf": [object(talk), object(draw)] })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::knobs::{Locks, Perm, Permissions};
    use super::super::toolkit::{build, ModelChoice};
    use super::*;
    use crate::config::ImageFamily;

    fn toolkit(perms: &Permissions, locks: &Locks) -> Toolkit {
        let models = vec![ModelChoice { name: "sdxl".into(), family: "sdxl", label: "SDXL" }];
        build(perms, &json!({"prompt": "x"}), locks, models, Some(ImageFamily::Sdxl))
    }

    fn keys(v: &Value) -> Vec<String> {
        v["properties"].as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default()
    }

    /// A chat reply's two shapes: talking, and drawing.
    fn branches(s: &Value) -> (&Value, &Value) {
        (&s["anyOf"][0], &s["anyOf"][1])
    }

    #[test]
    fn only_what_is_on_and_unlocked_is_a_property() {
        let t = toolkit(&Permissions::default(), &Locks::default());
        let s = output(Mode::Chat, &t, 4, 2);
        let (talk, _) = branches(&s);
        assert_eq!(keys(talk)[0], "notes", "the scratchpad comes first");
        assert_eq!(keys(&talk["properties"]["change"]), ["prompt", "negative", "cfg_scale"]);
        let locked = toolkit(&Permissions::default(), &Locks::of(true, false, None));
        let s = output(Mode::Chat, &locked, 4, 2);
        let (talk, _) = branches(&s);
        assert_eq!(keys(&talk["properties"]["change"]), ["prompt"]);
    }

    /// llama.cpp writes required properties first, so every one is required
    /// and the order is the schema's: the decision and the change before the
    /// reply, or the prompt ends up in the reply.
    #[test]
    fn every_reply_is_written_in_order_and_a_drawing_writes_its_prompt() {
        let t = toolkit(&Permissions::default(), &Locks::default());
        let chat = output(Mode::Chat, &t, 4, 2);
        let (talk, draw) = branches(&chat);
        for b in [talk, draw] {
            assert_eq!(b["required"], json!(keys(b)));
        }
        assert_eq!(keys(talk), ["notes", "action", "change", "reason", "reply"]);
        assert_eq!(keys(draw), ["notes", "action", "count", "seeds", "change", "reason", "reply"]);
        assert_eq!(draw["properties"]["count"]["maximum"], json!(t.batch));
        assert_eq!(draw["properties"]["action"]["const"], "generate");
        assert_eq!(draw["properties"]["change"]["required"], json!(["prompt"]));
        assert!(talk["properties"]["change"].get("required").is_none());

        let mut p = Permissions::default();
        p.actions.insert("hires".into(), Perm::Ask);
        let s = output(Mode::Critique, &toolkit(&p, &Locks::default()), 4, 3);
        assert_eq!(keys(&s), ["notes", "checks", "keepers", "action", "escalate", "change", "reason", "reply"]);
        assert_eq!(s["required"], json!(keys(&s)));
        let s = output(Mode::Plan, &t, 0, 0);
        assert_eq!(keys(&s), ["notes", "checks", "change", "reason", "reply"]);
        assert_eq!(s["properties"]["change"]["required"], json!(["prompt"]));
        let judging = output(Mode::Plan, &t, 4, 0);
        assert!(judging["properties"]["change"].get("required").is_none(), "a plan judging a round needn't rewrite the prompt");
    }

    #[test]
    fn without_the_prompt_a_drawing_draws_the_current_one() {
        let mut p = Permissions::default();
        p.knobs.get_mut("prompt").unwrap().perm = Perm::Off;
        let s = output(Mode::Chat, &toolkit(&p, &Locks::default()), 4, 2);
        let (_, draw) = branches(&s);
        assert!(draw["properties"]["change"].get("required").is_none());
    }

    #[test]
    fn a_critique_answers_every_check_for_every_picture() {
        let mut p = Permissions::default();
        let t = toolkit(&p, &Locks::default());
        let s = output(Mode::Critique, &t, 4, 3);
        let checks = &s["properties"]["checks"];
        assert_eq!((checks["minItems"].clone(), checks["maxItems"].clone()), (json!(4), json!(4)));
        assert_eq!(checks["items"]["minItems"], json!(3));
        assert_eq!(s["properties"]["action"]["enum"], json!(["generate", "done"]), "no pass is on");
        assert!(s["properties"].get("escalate").is_none());
        p.actions.insert("hires".into(), Perm::Ask);
        let s = output(Mode::Critique, &toolkit(&p, &Locks::default()), 4, 3);
        assert_eq!(s["properties"]["escalate"]["anyOf"][0]["properties"]["pass"]["enum"], json!(["hires"]));
    }

    #[test]
    fn nothing_to_change_means_no_change_field() {
        let mut p = Permissions::default();
        for k in p.knobs.values_mut() {
            k.perm = Perm::Off;
        }
        let s = output(Mode::Chat, &toolkit(&p, &Locks::default()), 4, 2);
        let (talk, draw) = branches(&s);
        assert!(talk["properties"].get("change").is_none() && draw["properties"].get("change").is_none());
    }

    #[test]
    fn a_describe_offers_a_negative_only_where_the_model_reads_one() {
        let t = toolkit(&Permissions::default(), &Locks::of(true, false, None));
        assert!(output(Mode::Describe, &t, 0, 0)["properties"].get("negative").is_none());
        let t = toolkit(&Permissions::default(), &Locks::default());
        assert!(output(Mode::Describe, &t, 0, 0)["properties"].get("negative").is_some());
    }
}
