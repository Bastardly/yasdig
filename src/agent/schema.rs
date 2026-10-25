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
use super::knobs::{COMPARABLE, MAX_BATCH, MAX_ROUNDS};
use super::toolkit::{KnobView, Toolkit};
use serde_json::{json, Map, Value};

pub const NOTES_CHARS: u32 = 600;
pub const REPLY_CHARS: u32 = 800;
pub const MAX_CHECKS: u32 = 6;
pub const GOAL_CHARS: u32 = 300;

fn text(max: u32) -> Value {
    json!({ "type": "string", "maxLength": max })
}

fn int(min: u32, max: u32) -> Value {
    json!({ "type": "integer", "minimum": min, "maximum": max })
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
        _ => json!({ "enum": t.model_names() }),
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
    // A knob locked on this model is offered while the model may change:
    // another model may take it, and `knobs::apply` checks it against the
    // model the change ends on.
    let switch = t.knob("model").is_some_and(|m| m.offered());
    for k in t.knobs.iter().filter(|k| k.offered() || (switch && k.allowed)) {
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

/// A plan's yes/no questions.
fn checks_list() -> Value {
    json!({ "type": "array", "items": text(120), "minItems": 1, "maxItems": MAX_CHECKS })
}

/// A critique's answers: a row per picture, an answer per check, and the
/// keepers.
fn critique_answers(props: &mut Map<String, Value>, batch: u32, checks: u32) {
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
}

/// A picture as a prompt: the prompt, and a negative only where the model
/// reads one.
fn describe_fields(props: &mut Map<String, Value>, t: &Toolkit) {
    props.insert("prompt".into(), text(1500));
    if t.knob("negative").is_some_and(|k| k.locked.is_none()) {
        props.insert("negative".into(), text(500));
    }
}

/// A storyboard, written whole: a title, the look every shot shares, the
/// cast and the shots. `attached` is how many pictures the person's message
/// showed, which a member's `picture` may name (0 is none).
///
/// With the model knob allowed and a choice to make, `image_model` picks the
/// model the cast and keyframes are drawn with.
fn story_fields(props: &mut Map<String, Value>, attached: u32, t: &Toolkit) {
    let mut member = Map::new();
    member.insert("name".into(), text(40));
    member.insert("kind".into(), json!({ "enum": ["character", "place", "thing"] }));
    member.insert("look".into(), text(300));
    member.insert("picture".into(), int(0, attached));
    member.insert("prompt".into(), text(600));
    let mut shot = Map::new();
    shot.insert("text".into(), text(500));
    shot.insert("cast".into(), json!({ "type": "array", "items": text(40), "maxItems": crate::config::MAX_REFERENCES }));
    shot.insert("cut".into(), json!({ "type": "boolean" }));
    props.insert("title".into(), text(80));
    props.insert("every".into(), text(300));
    if let Some(models) = board_models(t) {
        props.insert("image_model".into(), json!({ "enum": models }));
    }
    props.insert("cast".into(), json!({ "type": "array", "items": object(member), "maxItems": super::session::MAX_CAST }));
    props.insert(
        "shots".into(),
        json!({ "type": "array", "items": object(shot), "minItems": 1, "maxItems": crate::pipeline::MAX_SHOTS }),
    );
}

/// The models a storyboard may draw its pictures with, when it has a choice.
pub fn board_models(t: &Toolkit) -> Option<Vec<String>> {
    (t.knob("model").is_some_and(|k| k.allowed) && t.models.len() > 1).then(|| t.model_names())
}

/// The reply's schema for one turn. `batch` is how many pictures the turn
/// shows (a critique's round, a chat's attachments) — for a verdict, how many
/// values were compared — and `checks` how many questions each is judged by.
/// `sweep`: the session is a comparison, whose rounds the server runs, so its
/// plan and critiques only judge.
pub fn output(mode: Mode, t: &Toolkit, batch: u32, checks: u32, sweep: bool) -> Value {
    let mut props = Map::new();
    props.insert("notes".into(), text(NOTES_CHARS));
    match mode {
        Mode::Plan if sweep => {
            props.insert("checks".into(), checks_list());
            props.insert("reply".into(), text(REPLY_CHARS));
            object(props)
        }
        Mode::Critique if sweep => {
            critique_answers(&mut props, batch, checks);
            props.insert("reply".into(), text(REPLY_CHARS));
            object(props)
        }
        Mode::Verdict => {
            props.insert("best".into(), int(1, batch.max(1)));
            props.insert("reply".into(), text(REPLY_CHARS));
            object(props)
        }
        Mode::Describe => {
            describe_fields(&mut props, t);
            object(props)
        }
        Mode::Take => {
            let n = checks.max(1);
            props.insert("answers".into(), json!({ "type": "array", "items": { "type": "boolean" }, "minItems": n, "maxItems": n }));
            // The shot's words for a retake, when a check failed because of
            // them; "" keeps them.
            props.insert("better_text".into(), text(500));
            props.insert("reply".into(), text(REPLY_CHARS));
            object(props)
        }
        Mode::Story => {
            story_fields(&mut props, batch, t);
            props.insert("reply".into(), text(REPLY_CHARS));
            object(props)
        }
        Mode::Plan => {
            props.insert("checks".into(), checks_list());
            // How long the session runs, read off the goal's words.
            props.insert("rounds".into(), int(1, MAX_ROUNDS));
            props.insert("count".into(), int(1, MAX_BATCH));
            // A plan is followed by the first round, so it writes that round's
            // prompt — unless a round was just drawn (`batch` > 0), which it judges first.
            with_change(&mut props, t, batch == 0);
            props.insert("reply".into(), text(REPLY_CHARS));
            object(props)
        }
        Mode::Critique => {
            let b = batch.max(1);
            critique_answers(&mut props, batch, checks);
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
            // The next round's size and seeds, read when the action is "generate".
            props.insert("count".into(), int(1, MAX_BATCH));
            props.insert("seeds".into(), json!({ "enum": ["same", "new"] }));
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
            //
            // The actions are named for what the person asked, since a small
            // model picks among the words: "session" told it nothing.
            let mut talk = props.clone();
            talk.insert("action".into(), json!({ "const": "talk" }));
            with_change(&mut talk, t, false);
            talk.insert("reply".into(), text(REPLY_CHARS));
            let mut draw = props.clone();
            draw.insert("action".into(), json!({ "const": "draw" }));
            draw.insert("count".into(), int(1, MAX_BATCH));
            draw.insert("seeds".into(), json!({ "enum": ["new", "same"] }));
            with_change(&mut draw, t, true);
            draw.insert("reply".into(), text(REPLY_CHARS));
            let mut branches = vec![object(talk), object(draw)];
            // A picture written as a prompt, when the turn shows one.
            if batch > 0 {
                let mut describe = props.clone();
                describe.insert("action".into(), json!({ "const": "describe" }));
                describe_fields(&mut describe, t);
                describe.insert("reply".into(), text(REPLY_CHARS));
                branches.push(object(describe));
            }
            // A session: the goal in the agent's own words, since the person's
            // ("make it better") is often not one. The plan turn after it
            // writes the checks and sizes the rounds.
            let mut improve = props.clone();
            improve.insert("action".into(), json!({ "const": "improve" }));
            improve.insert("goal".into(), text(GOAL_CHARS));
            improve.insert("reply".into(), text(REPLY_CHARS));
            branches.push(object(improve));
            // A comparison: one knob, the values to try, pictures per value.
            // The values are strings, since the grammar can't type each item
            // by the knob; the server reads them (`knobs::parse_value`).
            //
            // A knob locked on this model is still offered: the comparison
            // may switch model first (its `change`), and the server checks the
            // values against the model it runs on. "Compare CFG on MOSS" from a
            // chat on a CFG-1 model otherwise had nothing to vary but the model.
            let vary: Vec<&str> = t.knobs.iter().filter(|k| k.allowed).map(|k| k.key).filter(|k| COMPARABLE.contains(k)).collect();
            // A movie: the storyboard is the next turn's (`Mode::Story`).
            if t.films() {
                let mut film = props.clone();
                film.insert("action".into(), json!({ "const": "film" }));
                film.insert("reply".into(), text(REPLY_CHARS));
                branches.push(object(film));
            }
            // Filming the storyboard, when it is ready.
            if t.board_ready {
                let mut go = props.clone();
                go.insert("action".into(), json!({ "const": "go" }));
                go.insert("reply".into(), text(REPLY_CHARS));
                branches.push(object(go));
            }
            if !vary.is_empty() {
                let mut compare = props;
                compare.insert("action".into(), json!({ "const": "compare" }));
                with_change(&mut compare, t, false);
                compare.insert("goal".into(), text(GOAL_CHARS));
                compare.insert("vary".into(), json!({ "enum": vary }));
                compare.insert(
                    "values".into(),
                    json!({ "type": "array", "items": text(1000), "minItems": 2, "maxItems": MAX_ROUNDS }),
                );
                compare.insert("count".into(), int(1, MAX_BATCH));
                compare.insert("reply".into(), text(REPLY_CHARS));
                branches.push(object(compare));
            }
            json!({ "anyOf": branches })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::knobs::{Locks, Rules};
    use super::super::toolkit::{build, ModelChoice};
    use super::*;
    use crate::config::ImageFamily;

    fn models() -> Vec<ModelChoice> {
        vec![
            ModelChoice { name: "sdxl".into(), family: "sdxl", label: "SDXL", references: false, locks: Default::default() },
            ModelChoice { name: "klein".into(), family: "flux2-klein-4b", label: "FLUX.2 Klein 4B", references: false, locks: Default::default() },
        ]
    }

    fn toolkit(rules: &Rules, locks: &Locks) -> Toolkit {
        build(rules, &json!({"prompt": "x"}), locks, models(), Some(ImageFamily::Sdxl))
    }

    fn keys(v: &Value) -> Vec<String> {
        v["properties"].as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default()
    }

    /// A chat reply's two shapes: talking, and drawing.
    fn branches(s: &Value) -> (&Value, &Value) {
        (&s["anyOf"][0], &s["anyOf"][1])
    }

    #[test]
    fn only_what_is_allowed_and_unlocked_or_another_model_takes_is_a_property() {
        let t = toolkit(&Rules::default(), &Locks::default());
        let s = output(Mode::Chat, &t, 4, 2, false);
        let (talk, _) = branches(&s);
        assert_eq!(keys(talk)[0], "notes", "the scratchpad comes first");
        assert_eq!(keys(&talk["properties"]["change"]), ["prompt", "negative", "cfg_scale", "size", "model"]);
        assert_eq!(talk["properties"]["change"]["properties"]["model"]["enum"], json!(["sdxl", "klein"]), "every runnable model");
        // Locked here, but a switch of model in the same change may take it.
        let locked = toolkit(&Rules::default(), &Locks::of(true, false, None));
        let s = output(Mode::Chat, &locked, 4, 2, false);
        let (talk, _) = branches(&s);
        assert_eq!(keys(&talk["properties"]["change"]), ["prompt", "negative", "cfg_scale", "size", "model"]);
        let mut fixed = Rules::default();
        fixed.knobs.remove("model");
        let s = output(Mode::Chat, &toolkit(&fixed, &Locks::of(true, false, None)), 4, 2, false);
        let (talk, _) = branches(&s);
        assert_eq!(keys(&talk["properties"]["change"]), ["prompt", "size"]);
    }

    /// llama.cpp writes required properties first, so every one is required
    /// and the order is the schema's: the decision and the change before the
    /// reply, or the prompt ends up in the reply.
    #[test]
    fn every_reply_is_written_in_order_and_a_drawing_writes_its_prompt() {
        let t = toolkit(&Rules::default(), &Locks::default());
        let chat = output(Mode::Chat, &t, 4, 2, false);
        let (talk, draw) = branches(&chat);
        for b in [talk, draw] {
            assert_eq!(b["required"], json!(keys(b)));
        }
        assert_eq!(keys(talk), ["notes", "action", "change", "reason", "reply"]);
        assert_eq!(keys(draw), ["notes", "action", "count", "seeds", "change", "reason", "reply"]);
        assert_eq!(draw["properties"]["count"]["maximum"], json!(MAX_BATCH));
        assert_eq!(draw["properties"]["action"]["const"], "draw");
        assert_eq!(draw["properties"]["change"]["required"], json!(["prompt"]));
        assert!(talk["properties"]["change"].get("required").is_none());

        let mut r = Rules::default();
        r.actions.insert("hires".into());
        let s = output(Mode::Critique, &toolkit(&r, &Locks::default()), 4, 3, false);
        assert_eq!(keys(&s), ["notes", "checks", "keepers", "action", "count", "seeds", "escalate", "change", "reason", "reply"]);
        assert_eq!(s["required"], json!(keys(&s)));
        let s = output(Mode::Plan, &t, 0, 0, false);
        assert_eq!(keys(&s), ["notes", "checks", "rounds", "count", "change", "reason", "reply"]);
        assert_eq!(s["properties"]["rounds"]["maximum"], json!(MAX_ROUNDS));
        assert_eq!(s["properties"]["change"]["required"], json!(["prompt"]));
        let judging = output(Mode::Plan, &t, 4, 0, false);
        assert!(judging["properties"]["change"].get("required").is_none(), "a plan judging a round needn't rewrite the prompt");
    }

    #[test]
    fn without_the_prompt_a_drawing_draws_the_current_one() {
        let mut r = Rules::default();
        r.knobs.remove("prompt");
        let s = output(Mode::Chat, &toolkit(&r, &Locks::default()), 4, 2, false);
        let (_, draw) = branches(&s);
        assert!(draw["properties"]["change"].get("required").is_none());
    }

    #[test]
    fn a_critique_answers_every_check_for_every_picture() {
        let mut r = Rules::default();
        let s = output(Mode::Critique, &toolkit(&r, &Locks::default()), 4, 3, false);
        let checks = &s["properties"]["checks"];
        assert_eq!((checks["minItems"].clone(), checks["maxItems"].clone()), (json!(4), json!(4)));
        assert_eq!(checks["items"]["minItems"], json!(3));
        assert_eq!(s["properties"]["action"]["enum"], json!(["generate", "done"]), "no pass is on");
        assert!(s["properties"].get("escalate").is_none());
        r.actions.insert("hires".into());
        let s = output(Mode::Critique, &toolkit(&r, &Locks::default()), 4, 3, false);
        assert_eq!(s["properties"]["escalate"]["anyOf"][0]["properties"]["pass"]["enum"], json!(["hires"]));
    }

    #[test]
    fn nothing_to_change_means_no_change_field() {
        let r = Rules { knobs: Default::default(), ..Rules::default() };
        let s = output(Mode::Chat, &toolkit(&r, &Locks::default()), 4, 2, false);
        let (talk, draw) = branches(&s);
        assert!(talk["properties"].get("change").is_none() && draw["properties"].get("change").is_none());
    }

    #[test]
    fn a_describe_offers_a_negative_only_where_the_model_reads_one() {
        let t = toolkit(&Rules::default(), &Locks::of(true, false, None));
        assert!(output(Mode::Describe, &t, 0, 0, false)["properties"].get("negative").is_none());
        let t = toolkit(&Rules::default(), &Locks::default());
        assert!(output(Mode::Describe, &t, 0, 0, false)["properties"].get("negative").is_some());
    }

    /// The `action` each of a chat reply's branches is.
    fn actions(s: &Value) -> Vec<String> {
        s["anyOf"].as_array().unwrap().iter().map(|b| b["properties"]["action"]["const"].as_str().unwrap().to_string()).collect()
    }

    #[test]
    fn a_chat_reply_may_describe_only_what_it_sees_and_may_always_improve_or_compare() {
        let t = toolkit(&Rules::default(), &Locks::default());
        assert_eq!(actions(&output(Mode::Chat, &t, 0, 0, false)), ["talk", "draw", "improve", "compare"]);
        let seeing = output(Mode::Chat, &t, 1, 0, false);
        assert_eq!(actions(&seeing), ["talk", "draw", "describe", "improve", "compare"]);
        assert_eq!(keys(&seeing["anyOf"][2]), ["notes", "action", "prompt", "negative", "reply"]);
        assert_eq!(keys(&seeing["anyOf"][3]), ["notes", "action", "goal", "reply"]);
        let compare = &seeing["anyOf"][4];
        assert_eq!(keys(compare), ["notes", "action", "change", "reason", "goal", "vary", "values", "count", "reply"]);
        // The default whitelist: prompt, negative, CFG, size, model — size isn't comparable.
        assert_eq!(compare["properties"]["vary"]["enum"], json!(["prompt", "negative", "cfg_scale", "model"]));
        assert_eq!(compare["properties"]["values"]["maxItems"], json!(MAX_ROUNDS));
    }

    #[test]
    fn a_film_is_offered_only_where_one_can_be_made_and_its_board_is_written_in_order() {
        let mut t = toolkit(&Rules::default(), &Locks::default());
        assert!(!actions(&output(Mode::Chat, &t, 0, 0, false)).contains(&"film".to_string()), "no video model");
        t.film_off = None;
        assert_eq!(actions(&output(Mode::Chat, &t, 0, 0, false)), ["talk", "draw", "improve", "film", "compare"]);
        t.board_ready = true;
        assert_eq!(actions(&output(Mode::Chat, &t, 0, 0, false)), ["talk", "draw", "improve", "film", "go", "compare"]);
        let take = output(Mode::Take, &t, 6, 3, false);
        assert_eq!(keys(&take), ["notes", "answers", "better_text", "reply"]);
        assert_eq!(take["properties"]["answers"]["minItems"], json!(3));
        let s = output(Mode::Story, &t, 2, 0, false);
        assert_eq!(keys(&s), ["notes", "title", "every", "image_model", "cast", "shots", "reply"]);
        assert_eq!(s["properties"]["image_model"]["enum"], json!(["sdxl", "klein"]));
        let mut fixed = Rules::default();
        fixed.knobs.remove("model");
        let mut t2 = toolkit(&fixed, &Locks::default());
        t2.film_off = None;
        assert!(output(Mode::Story, &t2, 0, 0, false)["properties"].get("image_model").is_none(), "the chat's model draws them");
        let member = &s["properties"]["cast"]["items"];
        assert_eq!(keys(member), ["name", "kind", "look", "picture", "prompt"]);
        assert_eq!(member["required"], json!(keys(member)));
        assert_eq!(member["properties"]["picture"]["maximum"], json!(2), "one of the pictures attached, or none");
        assert_eq!(keys(&s["properties"]["shots"]["items"]), ["text", "cast", "cut"]);
        assert_eq!(s["properties"]["shots"]["maxItems"], json!(crate::pipeline::MAX_SHOTS));
    }

    #[test]
    fn nothing_comparable_offered_means_no_comparison() {
        let r = Rules { knobs: ["size".to_string()].into_iter().collect(), ..Default::default() };
        let t = toolkit(&r, &Locks::default());
        assert_eq!(actions(&output(Mode::Chat, &t, 0, 0, false)), ["talk", "draw", "improve"]);
    }

    #[test]
    fn a_comparisons_turns_only_judge_and_its_verdict_picks_one_value() {
        let mut r = Rules::default();
        r.actions.insert("hires".into());
        let t = toolkit(&r, &Locks::default());
        assert_eq!(keys(&output(Mode::Plan, &t, 0, 0, true)), ["notes", "checks", "reply"]);
        let c = output(Mode::Critique, &t, 2, 3, true);
        assert_eq!(keys(&c), ["notes", "checks", "keepers", "reply"], "the server runs the rounds");
        let v = output(Mode::Verdict, &t, 4, 0, true);
        assert_eq!(keys(&v), ["notes", "best", "reply"]);
        assert_eq!(v["properties"]["best"]["maximum"], json!(4));
    }
}
