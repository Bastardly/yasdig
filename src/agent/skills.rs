//! Skills: short markdown notes the agent is given with each turn, so a small
//! model knows what it is working with — how yasdig works, how a family wants
//! its prompts written, what a critique is for.
//!
//! Built-in ones are compiled in (`src/agent/skills/*.md`); the person's own
//! go in `user/skills/*.md`, re-read every turn, and one with a built-in's
//! `id` replaces it. Front matter says when a skill applies:
//!
//! ```text
//! ---
//! id: sentences
//! families: [flux, z_image]      # ImageFamily::as_str names; none = all
//! modes: [chat, critique]        # chat, plan, critique, describe; none = all
//! priority: 50                   # higher is kept first under the budget
//! ---
//! ```
//!
//! Everything picked must fit [`BUDGET`] characters, or the context a 4B model
//! has for the conversation and its pictures shrinks to nothing. A skill that
//! doesn't fit is left out whole, lowest priority first, never cut mid-way.

use super::chat::Mode;
use crate::config::ImageFamily;
use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

/// Characters of skills per turn.
pub const BUDGET: usize = 6000;

const BUILTIN: &[(&str, &str)] = &[
    ("general.md", include_str!("skills/general.md")),
    ("plan.md", include_str!("skills/plan.md")),
    ("critique.md", include_str!("skills/critique.md")),
    ("describe.md", include_str!("skills/describe.md")),
    ("tags.md", include_str!("skills/tags.md")),
    ("sentences.md", include_str!("skills/sentences.md")),
    ("qwen-image.md", include_str!("skills/qwen-image.md")),
    ("moss.md", include_str!("skills/moss.md")),
];

#[derive(Debug, Clone, PartialEq)]
pub struct Skill {
    pub id: String,
    pub families: Vec<String>,
    pub modes: Vec<Mode>,
    pub priority: i32,
    pub body: String,
    pub builtin: bool,
}

impl Skill {
    fn applies(&self, family: Option<&str>, mode: Mode) -> bool {
        (self.families.is_empty() || family.is_some_and(|f| self.families.iter().any(|x| x == f)))
            && (self.modes.is_empty() || self.modes.contains(&mode))
    }
}

/// `[a, b]` or `a, b` as a list.
fn list(v: &str) -> Vec<String> {
    v.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(|s| s.trim().trim_matches(|c| c == '"' || c == '\'').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

pub fn parse(src: &str, builtin: bool) -> Result<Skill> {
    let src = src.trim_start_matches('\u{feff}');
    let Some(rest) = src.strip_prefix("---") else {
        bail!("it doesn't start with a --- front matter block");
    };
    let Some((head, body)) = rest.split_once("\n---") else {
        bail!("its front matter has no closing ---");
    };
    let mut skill = Skill { id: String::new(), families: vec![], modes: vec![], priority: 50, body: String::new(), builtin };
    for line in head.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let Some((k, v)) = line.split_once(':') else {
            bail!("front matter line {line:?} is not `key: value`");
        };
        match k.trim() {
            "id" => skill.id = v.trim().to_string(),
            "families" => {
                skill.families = list(v);
                for f in &skill.families {
                    if !ImageFamily::ALL.iter().any(|x| x.as_str() == f) {
                        bail!("'{f}' is not a model family (one of: {})", families());
                    }
                }
            }
            "modes" => {
                skill.modes = list(v)
                    .iter()
                    .map(|m| match m.as_str() {
                        "chat" => Ok(Mode::Chat),
                        "plan" => Ok(Mode::Plan),
                        "critique" => Ok(Mode::Critique),
                        "describe" => Ok(Mode::Describe),
                        other => bail!("'{other}' is not a mode (chat, plan, critique, describe)"),
                    })
                    .collect::<Result<_>>()?
            }
            "priority" => skill.priority = v.trim().parse().map_err(|_| anyhow::anyhow!("priority {v:?} is not a number"))?,
            other => bail!("'{other}' is not a front matter key (id, families, modes, priority)"),
        }
    }
    if skill.id.is_empty() || !skill.id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        bail!("it needs an `id` of letters, digits, - and _");
    }
    skill.body = body.trim_start_matches(['\r', '\n']).trim_end().to_string();
    if skill.body.is_empty() {
        bail!("it has no text after the front matter");
    }
    Ok(skill)
}

fn families() -> String {
    ImageFamily::ALL.iter().map(|f| f.as_str()).collect::<Vec<_>>().join(", ")
}

/// Every skill: the built-ins, with the person's own over them by `id`, and
/// the files that couldn't be read with the reason (`doctor` lists them).
pub fn load(user_skills: &Path) -> (Vec<Skill>, Vec<(PathBuf, String)>) {
    let mut skills: Vec<Skill> =
        BUILTIN.iter().map(|(_, src)| parse(src, true).expect("built-in skills are checked by cargo test")).collect();
    let mut errors = Vec::new();
    let mut files: Vec<PathBuf> = std::fs::read_dir(user_skills)
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "md")).collect())
        .unwrap_or_default();
    files.sort();
    for p in files {
        let read = std::fs::read_to_string(&p).map_err(anyhow::Error::from).and_then(|s| parse(&s, false));
        match read {
            Ok(s) => {
                skills.retain(|b| b.id != s.id);
                skills.push(s);
            }
            Err(e) => errors.push((p, format!("{e:#}"))),
        }
    }
    (skills, errors)
}

/// The skills for one turn, as one text, with the ids used and those left
/// out for the budget.
pub fn select(skills: &[Skill], family: Option<&str>, mode: Mode) -> (String, Vec<String>, Vec<String>) {
    let mut picked: Vec<&Skill> = skills.iter().filter(|s| s.applies(family, mode)).collect();
    picked.sort_by(|a, b| b.priority.cmp(&a.priority).then(a.id.cmp(&b.id)));
    let (mut text, mut used, mut dropped) = (String::new(), Vec::new(), Vec::new());
    for s in picked {
        if text.len() + s.body.len() + 2 > BUDGET {
            dropped.push(s.id.clone());
            continue;
        }
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(&s.body);
        used.push(s.id.clone());
    }
    (text, used, dropped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_built_in_skill_parses_and_names_real_families() {
        for (name, src) in BUILTIN {
            let s = parse(src, true).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(s.body.len() < BUDGET / 3, "{name} is too long for the budget to hold others beside it");
        }
        // Every family that generates has its prompt style.
        let (all, _) = load(Path::new("/nonexistent"));
        for f in ImageFamily::ALL.iter().filter(|f| f.runnable() && !f.fill_only()) {
            let (_, used, _) = select(&all, Some(f.as_str()), Mode::Describe);
            assert!(used.len() >= 3, "{} gets only {used:?}", f.label());
        }
    }

    #[test]
    fn a_users_skill_replaces_a_built_in_of_the_same_id() {
        let d = std::env::temp_dir().join(format!("yasdig-skills-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("tags.md"), "---\nid: tags\nfamilies: [sdxl]\n---\nMy own tags advice.").unwrap();
        std::fs::write(d.join("bad.md"), "no front matter").unwrap();
        let (all, errors) = load(&d);
        let (text, used, _) = select(&all, Some("sdxl"), Mode::Chat);
        assert!(text.contains("My own tags advice.") && used.contains(&"tags".to_string()));
        assert!(!text.contains("comma-separated"));
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn the_budget_drops_whole_skills_lowest_first() {
        let big = |id: &str, p: i32| Skill { id: id.into(), families: vec![], modes: vec![], priority: p, body: "x".repeat(4000), builtin: true };
        let (text, used, dropped) = select(&[big("low", 1), big("high", 9)], None, Mode::Chat);
        assert_eq!(used, ["high"]);
        assert_eq!(dropped, ["low"]);
        assert_eq!(text.len(), 4000);
    }

    #[test]
    fn front_matter_mistakes_say_what_is_wrong() {
        assert!(parse("---\nid: x\nfamilies: [sdxxl]\n---\nhi", false).unwrap_err().to_string().contains("sdxxl"));
        assert!(parse("---\nid: x\nmodes: [paint]\n---\nhi", false).is_err());
        assert!(parse("---\nid: x\n---\n", false).is_err());
    }
}
