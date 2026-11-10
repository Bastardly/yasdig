//! `yasdig agent-check`: what an agent model can be trusted with.
//!
//! A handful of fixed turns, each through the real system text, skills and
//! reply grammar (`context::messages`, `schema::output`), scored on what the
//! server would act on: the action picked, the count, the knob compared, the
//! checks written, a picture judged. A 4B model that chats well may still
//! propose an improvement for "three castles" or answer every check true —
//! this says so before a session is spent finding out, and it is the
//! regression test for an edit to a skill: run it before and after.
//!
//! The grammar already holds the reply's shape, so nothing here checks JSON;
//! every case is a *choice* the grammar leaves open. Seeds are fixed per
//! case, so two runs of one model on one build agree.
//!
//! Outside the server and off any chat: the toolkit is the default rules on
//! an SDXL-shaped model (a real CFG, a negative prompt), whatever is
//! installed, so two machines' results compare.

use super::chat::{Chat, ImageRef, Mode, Msg};
use super::knobs::{Locks, Rules};
use super::session::ReplyJson;
use super::{agent_for, context, n_predict, schema, session, skills, toolkit, tuning, user_skills_dir, Staged};
use crate::config::{Config, ImageFamily};
use crate::llm::{self, LlmRunner, Request, Sampling};
use anyhow::Result;
use serde_json::json;

const PROMPT: &str = "a fox, snowy forest, soft morning light, photograph";

struct Case {
    name: &'static str,
    mode: Mode,
    /// The person's message (a chat turn), or the session's goal (a plan).
    says: &'static str,
    /// A critique: the checks the test picture is judged by.
    checks: &'static [&'static str],
    /// What a model that understood wrote, or what was wrong.
    want: fn(&ReplyJson) -> Result<(), String>,
}

fn action(r: &ReplyJson, want: &str) -> Result<(), String> {
    match r.action.as_deref() {
        Some(a) if a == want => Ok(()),
        a => Err(format!("action {} where \"{want}\" was asked for", a.map_or("none".into(), |a| format!("\"{a}\"")))),
    }
}

fn new_prompt(r: &ReplyJson) -> Option<&str> {
    r.change.as_ref()?.get("prompt")?.as_str()
}

const CASES: &[Case] = &[
    Case {
        name: "draws when asked to draw",
        mode: Mode::Chat,
        says: "Draw a lighthouse on a cliff in a storm.",
        checks: &[],
        want: |r| {
            action(r, "draw")?;
            let p = new_prompt(r).unwrap_or("").to_lowercase();
            if !p.contains("lighthouse") {
                return Err(format!("the prompt it would draw doesn't mention the lighthouse: {p:?}"));
            }
            match r.count {
                Some(1) | None => Ok(()),
                Some(n) => Err(format!("{n} pictures where one was asked for")),
            }
        },
    },
    Case {
        name: "counts the pictures asked for",
        mode: Mode::Chat,
        says: "Give me three pictures of a castle at night.",
        checks: &[],
        want: |r| {
            action(r, "draw")?;
            if r.count == Some(3) { Ok(()) } else { Err(format!("count {:?} where 3 were asked for", r.count)) }
        },
    },
    Case {
        name: "answers a question without drawing",
        mode: Mode::Chat,
        says: "What does CFG scale do? Don't draw anything.",
        checks: &[],
        want: |r| {
            action(r, "talk")?;
            match new_prompt(r) {
                Some(p) if p.trim() != PROMPT => Err("it rewrote the prompt when only asked a question".into()),
                _ if r.reply.trim().len() < 20 => Err("the answer is empty".into()),
                _ => Ok(()),
            }
        },
    },
    Case {
        name: "proposes a session for \"keep trying\"",
        mode: Mode::Chat,
        says: "Keep trying until the fox is wearing a red knitted hat.",
        checks: &[],
        want: |r| {
            action(r, "improve")?;
            let g = r.goal.as_deref().unwrap_or("").to_lowercase();
            if g.contains("hat") { Ok(()) } else { Err(format!("the goal leaves out the hat: {g:?}")) }
        },
    },
    Case {
        name: "proposes a comparison for \"which is best\"",
        mode: Mode::Chat,
        says: "Which CFG works best for this prompt? Try a few values.",
        checks: &[],
        want: |r| {
            action(r, "compare")?;
            if r.vary.as_deref() != Some("cfg_scale") {
                return Err(format!("it varies {:?}, not cfg_scale", r.vary));
            }
            let values: Vec<f32> = r
                .values
                .iter()
                .flatten()
                .filter_map(|v| v.as_f64().map(|f| f as f32).or_else(|| v.as_str()?.trim().parse().ok()))
                .collect();
            let written = r.values.as_ref().map_or(0, |v| v.len());
            if values.len() < 2 || values.len() != written {
                return Err(format!("its values aren't numbers to compare: {:?}", r.values));
            }
            Ok(())
        },
    },
    Case {
        name: "writes checks that can be answered by looking",
        mode: Mode::Plan,
        says: "A fox wearing a red knitted hat in a snowy forest.",
        checks: &[],
        want: |r| {
            let checks: Vec<&str> = r.checks.as_ref().and_then(|c| c.as_array()).into_iter().flatten().filter_map(|c| c.as_str()).collect();
            if !(2..=5).contains(&checks.len()) {
                return Err(format!("{} checks where two to five fit the goal", checks.len()));
            }
            if let Some(c) = checks.iter().find(|c| !c.trim_end().ends_with('?')) {
                return Err(format!("a check that isn't a question: {c:?}"));
            }
            if !checks.iter().any(|c| c.to_lowercase().contains("hat")) {
                return Err(format!("no check asks about the hat: {checks:?}"));
            }
            Ok(())
        },
    },
    Case {
        name: "judges a picture by what it shows",
        mode: Mode::Critique,
        says: "A red square and a blue circle on a white background.",
        // The picture has the square alone (`test_picture`).
        checks: &["Is there a red square?", "Is there a blue circle?"],
        want: |r| {
            let row: Vec<bool> = r
                .checks
                .as_ref()
                .and_then(|c| c.as_array()?.first()?.as_array().cloned())
                .unwrap_or_default()
                .iter()
                .filter_map(|b| b.as_bool())
                .collect();
            match row.as_slice() {
                [true, false] => Ok(()),
                [true, true] => Err("it said the blue circle is there: it answers from the goal, not the picture".into()),
                other => Err(format!("it answered {other:?}; the picture has a red square and no blue circle")),
            }
        },
    },
];

/// A red square on white, 512², for the critique: one check true, one false.
fn test_picture() -> Result<Vec<u8>> {
    let img = image::RgbImage::from_fn(512, 512, |x, y| {
        if (156..356).contains(&x) && (156..356).contains(&y) { image::Rgb([220, 30, 30]) } else { image::Rgb([255, 255, 255]) }
    });
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)?;
    Ok(png)
}

/// The chat a case's turn is asked in: a prompt already set, and for a
/// critique the round it judges.
fn chat_for(case: &Case) -> Chat {
    let mut chat = Chat::new("agent-check".into(), json!({ "prompt": PROMPT, "cfg_scale": 6.0 }), None);
    match case.mode {
        Mode::Chat => chat.messages.push(Msg::User { n: 0, ms: 0, text: case.says.into(), images: vec![] }),
        Mode::Critique => {
            chat.session.goal = case.says.into();
            chat.session.checks = case.checks.iter().map(|c| c.to_string()).collect();
            chat.session.round = 1;
            let image = ImageRef { kind: "item".into(), id: "check".into(), file: "check.png".into(), seed: Some(1) };
            chat.messages.push(Msg::Batch {
                n: 0,
                ms: 0,
                round: 1,
                settings_rev: 0,
                summary: format!("prompt '{}'", case.says),
                size: 1,
                images: vec![image],
            });
        }
        _ => chat.session.goal = case.says.into(),
    }
    chat
}

pub fn run(cfg: &Config, name: Option<&str>) -> Result<()> {
    let model = agent_for(cfg, name)?;
    let tune = tuning(cfg, &model);
    println!(
        "[agent-check] {} ({}), {} cases",
        model.label,
        if model.vision() { "sees pictures" } else { "text only" },
        CASES.len()
    );
    let (all_skills, _) = skills::load(&user_skills_dir(cfg));
    let family = ImageFamily::Sdxl;
    let mut tk = toolkit::build(&Rules::default(), &json!({ "prompt": PROMPT, "cfg_scale": 6.0 }), &Locks::default(), vec![], Some(family));
    tk.vision = model.vision();

    let dir = crate::pipeline::scratch_dir(&cfg.base_dir.join("work"));
    std::fs::create_dir_all(&dir)?;
    let monitor = crate::sd::Monitor::default();
    let runner = LlmRunner::watched(cfg, &monitor);
    let (mut passed, mut ran) = (0, 0);
    for (k, case) in CASES.iter().enumerate() {
        let mut staged = Staged(Vec::new());
        if case.mode == Mode::Critique {
            if !model.vision() {
                println!("[agent-check] skip  {}: this model has no vision projector, so it can't run sessions", case.name);
                continue;
            }
            let p = dir.join("agent-check.png");
            std::fs::write(&p, super::shrink_png(&test_picture()?, tune.image_side)?)?;
            staged.0.push(p);
        }
        let chat = chat_for(case);
        let (skill_text, _, _) = skills::select(&all_skills, &[family.as_str()], case.mode);
        let turn = context::Turn {
            mode: case.mode,
            images: staged.0.clone(),
            reference: 0,
            sweep: false,
            skills: &skill_text,
            checks: &chat.session.checks,
            goal: &chat.session.goal,
            round: chat.session.round,
            history: usize::MAX,
            film: None,
            cast: None,
            cast_shown: false,
        };
        let req = Request {
            v: llm::PROTOCOL,
            model: model.lm.clone(),
            mmproj: model.mmproj.clone().filter(|_| !staged.0.is_empty()),
            n_ctx: tune.n_ctx,
            n_predict: n_predict(case.mode),
            thinking: false,
            // Fixed per case: the same model on the same build scores the same.
            sampling: Sampling { seed: 1 + k as u32, ..tune.sampling },
            json_schema: Some(schema::output(case.mode, &tk, staged.0.len() as u32, case.checks.len() as u32, false)),
            messages: context::messages(&chat, &tk, &turn),
        };
        ran += 1;
        let verdict = runner.run(&req, &model.label, "agent-check").map_err(|e| format!("the turn failed: {e:#}")).and_then(|reply| {
            if reply.stats.stop == "length" {
                return Err("the reply was cut off".to_string());
            }
            let r = session::parse_reply(&reply.content).map_err(|e| format!("{e:#}"))?;
            (case.want)(&r).map_err(|why| format!("{why}\n                    notes: {}", r.notes.trim()))
        });
        match verdict {
            Ok(()) => {
                passed += 1;
                println!("[agent-check] ok    {}", case.name);
            }
            Err(why) => println!("[agent-check] FAIL  {}\n                    {why}", case.name),
        }
    }
    println!("[agent-check] {passed} of {ran} passed");
    if passed < ran {
        println!(
            "[agent-check] A failed case is a choice this model gets wrong on this build's instructions, not a broken install \
             (`llm-selftest` checks that). The first five are chat turns; the last two are what a session needs."
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every case builds its turn — the messages and the schema — with no
    /// model, and its picture is a PNG; and each expectation refuses an
    /// empty reply, so none passes by default.
    #[test]
    fn every_case_builds_its_turn_and_refuses_an_empty_reply() {
        let tk = toolkit::build(&Rules::default(), &json!({ "prompt": PROMPT }), &Locks::default(), vec![], Some(ImageFamily::Sdxl));
        for case in CASES {
            let chat = chat_for(case);
            let images: Vec<std::path::PathBuf> = if case.mode == Mode::Critique { vec!["/w/check.png".into()] } else { vec![] };
            let turn = context::Turn {
                mode: case.mode,
                images: images.clone(),
                reference: 0,
                sweep: false,
                skills: "",
                checks: &chat.session.checks,
                goal: &chat.session.goal,
                round: chat.session.round,
                history: usize::MAX,
                film: None,
                cast: None,
                cast_shown: false,
            };
            let m = context::messages(&chat, &tk, &turn);
            assert_eq!(m.last().unwrap().role, llm::Role::User, "{}", case.name);
            let s = schema::output(case.mode, &tk, images.len() as u32, case.checks.len() as u32, false);
            assert!(s.is_object(), "{}", case.name);
            assert!((case.want)(&ReplyJson::default()).is_err(), "{} passes an empty reply", case.name);
        }
        // What the chat cases ask for is on offer under the default rules.
        let chat = schema::output(Mode::Chat, &tk, 0, 0, false).to_string();
        for a in ["\"talk\"", "\"draw\"", "\"improve\"", "\"compare\"", "cfg_scale"] {
            assert!(chat.contains(a), "{a} is not offered");
        }
        assert!(image::load_from_memory(&test_picture().unwrap()).is_ok());
    }
}
