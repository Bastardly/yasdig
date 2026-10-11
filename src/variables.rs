//! `{animal}` in a prompt, and the named word lists behind it.
//!
//! A variable is a name and a handful of words, stored in `[variables]` in
//! `user/settings.toml` and edited on the Settings tab. Every `{name}` in a
//! prompt is replaced by one of that variable's words on the way to sd.cpp, so
//! a batch run varies its subject rather than only its seed.
//!
//! **Braces, not square brackets.** `[x]` is sd.cpp's ÷1.1 down-weight
//! (`parse_prompt_attention`, `stable-diffusion.cpp src/core/util.cpp:900` —
//! the same function `web/js/prompt-weight.js` is built around), so `[animl]`
//! is a *legitimate* prompt meaning "the word animl, de-emphasised". With that
//! spelling a typo could never be reported; it would quietly be conditioned as
//! literal text at weight 0.909. `{` and `}` fall inside that parser's
//! plain-text run (`[^\\()\[\]:B]{1,1024}`) and mean nothing to it, which is
//! what lets an unknown name be an error naming the variables that do exist.
//!
//! Three rules, and all three are visible in [`expand`]:
//!
//!   * **Case is ignored.** `{Animal}` finds `animal`. Names are stored
//!     lowercase ([`normalize`]) so the file and the lookup cannot disagree,
//!     and the lookup is case-insensitive anyway so a hand-edited `Animal =`
//!     still resolves rather than reading as a missing variable.
//!   * **A replacement is never rescanned.** Words are appended to the output,
//!     so a word containing `{color}` lands in the prompt as those characters.
//!     One pass, no recursion, nothing that can loop.
//!   * **The roll comes from the seed.** Same seed, same settings, same
//!     picture — which is what the seed box already promises and what clicking
//!     a reported seed to reproduce an image relies on. A batch walks the seed
//!     (`genPayload(index)` sends `seed + index`), so each image still rolls
//!     its own words.
//!
//! What ran and what was typed are therefore two different strings, and a work
//! item keeps both — [`typed`] is the one it stores beside the resolved prompt,
//! so reopening a generation puts the braces back in the box rather than the
//! animal they drew. The seed comes back with them, which is what makes that
//! restore lossless: same seed, same roll, same picture.
//!
//! Nothing here is a safety check: a prompt travels verbatim from the box to
//! `cmd.args(["-p", …])` with no shell in between, so the worst a nonsense
//! word list can produce is a nonsense picture. [`validate`] exists so that the
//! failure is a sentence on the Settings tab rather than a surprise two minutes
//! into a render.

use anyhow::{bail, Result};
use std::collections::BTreeMap;

/// The `[variables]` table: a name, and the words it can stand for.
///
/// `BTreeMap` rather than a hash: it is what keeps the written file in a stable
/// order, so a save that changed nothing produces no diff.
pub type Variables = BTreeMap<String, Vec<String>>;

/// More than this many is a hand-edited file rather than a workflow, and the
/// error is friendlier than a settings form with 4000 rows in it.
const MAX_VARIABLES: usize = 200;
/// A name has to be typeable inside braces in a prompt box.
const MAX_NAME: usize = 64;

/// Whether `s` could be written as `{s}` in a prompt.
///
/// The whitelist is the point: a name containing a space or a brace could never
/// be matched by [`expand`], so accepting one would store a variable that can
/// only ever sit there looking as though it should work.
fn is_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_NAME
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// One word of `n`, chosen from the seed and which occurrence this is.
///
/// splitmix64, which is two multiplies and three shifts — hand-rolled for the
/// same reason `random_seed` is: this is the only randomness the program needs
/// and it is not worth a dependency. `nth` is mixed in so that
/// `a {color} hat and a {color} coat` rolls twice rather than agreeing with
/// itself, which is the whole reason to write the variable twice.
fn pick(seed: i64, nth: u64, n: usize) -> usize {
    let mut x = (seed as u64).wrapping_add(0x9E37_79B9_7F4A_7C15_u64.wrapping_mul(nth + 1));
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    (x % n as u64) as usize
}

/// The words for `name`, matched without regard to case.
///
/// Spelled as a scan rather than a `get` on the lowercased name so that a
/// hand-edited file with `Animal = [...]` in it resolves too. The table is a
/// handful of entries read once per generation, against a pass that takes
/// minutes.
fn words<'a>(vars: &'a Variables, name: &str) -> Option<&'a Vec<String>> {
    vars.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v)
}

/// Replace every `{name}` in `text` with one of that variable's words.
///
/// Anything that is not a well-formed placeholder is copied through untouched —
/// `{a nice day}`, an unclosed brace, a bare `{}` — so braces stay typeable in
/// a prompt. A placeholder whose name *is* well-formed but isn't defined is an
/// error rather than literal text: that is what the brace syntax buys, and
/// silently rendering `{animl}` as words is exactly the failure it exists to
/// prevent.
pub fn expand(text: &str, vars: &Variables, seed: i64) -> Result<String> {
    // The overwhelmingly common prompt has no braces in it at all.
    if !text.contains('{') {
        return Ok(text.to_string());
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut nth = 0u64;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        // `find` can run past a second `{`, which is what makes
        // `{a nice {color} day}` resolve the inner one: the outer candidate
        // isn't a name, so its brace becomes literal and the scan continues
        // from just after it.
        match after.find('}') {
            Some(close) if is_name(&after[..close]) => {
                let name = &after[..close];
                let Some(list) = words(vars, name) else {
                    bail!("{}", unknown(name, vars));
                };
                // An empty list would panic in `pick`; `validate` rejects one,
                // and a hand-edited file that slipped through says so here
                // rather than dividing by zero.
                if list.is_empty() {
                    bail!("the variable '{name}' has no words. Add some on the Settings tab.");
                }
                out.push_str(&list[pick(seed, nth, list.len())]);
                nth += 1;
                rest = &after[close + 1..];
            }
            _ => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    Ok(out)
}

/// The message for a name nothing answers to. It lists what *is* defined,
/// because the answer is nearly always a typo or a variable saved under
/// another name, and both are settled by seeing the list.
fn unknown(name: &str, vars: &Variables) -> String {
    if vars.is_empty() {
        return format!(
            "the prompt names {{{name}}}, but no variables are defined. \
             Add one in Settings → Prompt variables, or remove the braces."
        );
    }
    let defined = vars.keys().cloned().collect::<Vec<_>>().join(", ");
    format!("the prompt names {{{name}}}, which is not a variable. Defined: {defined}. Did you remember to save changes?")
}

/// What to record beside a resolved prompt: the original, and only when the
/// roll actually changed it.
///
/// `None` therefore means "there was nothing to roll", which is also what a
/// sidecar written before variables existed says — so a reader needs no version
/// check and no second field to consult, only
/// `typed_prompt.unwrap_or(prompt)`. Storing the original unconditionally would
/// make those two cases indistinguishable and put a second copy of every prompt
/// in every sidecar for nothing.
pub fn typed(original: &str, expanded: &str) -> Option<String> {
    (original != expanded).then(|| original.to_string())
}

/// How the table is stored: names lowercased, words trimmed, blanks dropped.
///
/// Run on the way in from `POST /settings`, so the file only ever holds the
/// canonical spelling — which is what lets the lookup, the error messages and
/// the form all say the same thing about a name.
pub fn normalize(vars: &Variables) -> Result<Variables> {
    let mut out = Variables::new();
    for (name, list) in vars {
        let key = name.trim().to_lowercase();
        let words: Vec<String> = list
            .iter()
            .map(|w| w.trim().to_string())
            .filter(|w| !w.is_empty())
            .collect();
        // Two names that differ only in case would collapse into one here, and
        // silently dropping one of a pair the user can see on screen is worse
        // than refusing to save.
        if out.insert(key.clone(), words).is_some() {
            bail!("'{key}' is defined twice — variable names ignore case, so two spellings of one name are one variable.");
        }
    }
    Ok(out)
}

/// Reject what could be stored but could never work.
pub fn validate(vars: &Variables) -> Result<()> {
    if vars.len() > MAX_VARIABLES {
        bail!(
            "{} variables defined, the limit is {MAX_VARIABLES}",
            vars.len()
        );
    }
    let mut seen: Vec<String> = Vec::new();
    for (name, words) in vars {
        if !is_name(name) {
            bail!(
                "'{name}' is not a usable variable name: letters, digits, '_' and '-' only, \
                 up to {MAX_NAME} characters — it has to be typeable as {{{name}}} in a prompt."
            );
        }
        let lower = name.to_lowercase();
        if seen.contains(&lower) {
            bail!("'{lower}' is defined twice — variable names ignore case.");
        }
        seen.push(lower);
        if words.is_empty() || words.iter().all(|w| w.trim().is_empty()) {
            bail!(
                "the variable '{name}' has no words. Give it a comma-separated list, or remove it."
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> Variables {
        Variables::from([
            (
                "animal".to_string(),
                vec!["crab".to_string(), "horse".to_string(), "dog".to_string()],
            ),
            (
                "color".to_string(),
                vec!["red".to_string(), "blue".to_string()],
            ),
        ])
    }

    #[test]
    fn a_placeholder_is_replaced_by_one_of_its_words() {
        let out = expand("a lovely {animal} in a {color} hat", &vars(), 7).unwrap();
        assert!(!out.contains('{'), "{out}");
        let word = out
            .strip_prefix("a lovely ")
            .and_then(|s| s.split(' ').next())
            .unwrap();
        assert!(vars()["animal"].contains(&word.to_string()), "{out}");
    }

    /// The seed box already promises that the same number gives the same
    /// picture, and clicking a reported seed to reproduce one relies on it.
    #[test]
    fn the_same_seed_rolls_the_same_prompt() {
        let v = vars();
        let a = expand("{animal} {color}", &v, 1234).unwrap();
        let b = expand("{animal} {color}", &v, 1234).unwrap();
        assert_eq!(a, b);
        // And the roll is not a constant: somewhere in a short walk of seeds —
        // which is exactly what a batch does — the words change.
        assert!(
            (0..20).any(|s| expand("{animal} {color}", &v, s).unwrap() != a),
            "every seed rolled {a}"
        );
    }

    /// Writing the variable twice is a request for two words, not for the same
    /// word twice.
    #[test]
    fn two_occurrences_roll_independently() {
        let v = vars();
        assert!(
            (0..30).any(|s| {
                let out = expand("{color} hat, {color} coat", &v, s).unwrap();
                let mut it = out.split(' ');
                it.next() != it.nth(1)
            }),
            "the two occurrences agreed at every seed"
        );
    }

    #[test]
    fn the_name_is_matched_without_case() {
        let v = vars();
        let seed = 99;
        assert_eq!(
            expand("{Animal}", &v, seed).unwrap(),
            expand("{animal}", &v, seed).unwrap()
        );
        assert_eq!(
            expand("{ANIMAL}", &v, seed).unwrap(),
            expand("{animal}", &v, seed).unwrap()
        );
        // A hand-edited file that used a capital in the key resolves too.
        let odd = Variables::from([("Animal".to_string(), vec!["crab".to_string()])]);
        assert_eq!(expand("a {animal}", &odd, 1).unwrap(), "a crab");
    }

    #[test]
    fn an_unknown_name_is_an_error_naming_the_ones_that_exist() {
        let e = expand("a lovely {animl}", &vars(), 1)
            .unwrap_err()
            .to_string();
        assert!(e.contains("{animl}"), "{e}");
        assert!(e.contains("animal, color"), "{e}");
        // With nothing defined it says where to define one instead of
        // printing an empty list.
        let e = expand("{animal}", &Variables::new(), 1)
            .unwrap_err()
            .to_string();
        assert!(e.contains("Settings"), "{e}");
    }

    /// One pass. A word that contains a placeholder is text, not a program.
    #[test]
    fn a_replacement_is_not_rescanned() {
        let v = Variables::from([("a".to_string(), vec!["{color}".to_string()])]);
        assert_eq!(expand("x {a} y", &v, 3).unwrap(), "x {color} y");
    }

    #[test]
    fn braces_that_are_not_a_placeholder_are_left_alone() {
        let v = vars();
        for text in ["{a nice day}", "a {lovely crab", "{}", "{ animal }", "}{"] {
            assert_eq!(expand(text, &v, 5).unwrap(), text, "{text}");
        }
        // The outer candidate is not a name, so its brace is literal and the
        // inner one still resolves.
        let out = expand("{a nice {color} day}", &v, 5).unwrap();
        assert!(
            out.starts_with("{a nice ") && out.ends_with(" day}"),
            "{out}"
        );
        assert!(!out.contains("{color}"), "{out}");
    }

    /// The reason for the syntax: sd.cpp's own de-emphasis has to survive
    /// untouched, including when the word inside it names a variable.
    #[test]
    fn a_down_weighted_word_is_untouched() {
        let v = vars();
        assert_eq!(
            expand("a crab, [blurry], (sharp:1.2), [animal]", &v, 5).unwrap(),
            "a crab, [blurry], (sharp:1.2), [animal]"
        );
    }

    /// The other half of the record. `None` has to mean "nothing was rolled"
    /// for a prompt with no braces in it *and* for a sidecar written before
    /// variables existed, because the reader can't tell those apart and
    /// shouldn't have to.
    #[test]
    fn the_typed_prompt_is_kept_only_when_the_roll_changed_it() {
        let v = vars();
        let plain = "a lovely fluffy crab";
        assert_eq!(typed(plain, &expand(plain, &v, 5).unwrap()), None);
        // Braces that aren't a placeholder are copied through, so they are not
        // a roll either.
        assert_eq!(
            typed("{a nice day}", &expand("{a nice day}", &v, 5).unwrap()),
            None
        );

        let src = "a lovely {animal}";
        let out = expand(src, &v, 5).unwrap();
        assert_ne!(out, src);
        assert_eq!(typed(src, &out).as_deref(), Some(src));
    }

    #[test]
    fn bad_variables_are_rejected() {
        let cases = [
            // Could never be written as a placeholder.
            Variables::from([("my animal".to_string(), vec!["crab".to_string()])]),
            Variables::from([("{animal}".to_string(), vec!["crab".to_string()])]),
            Variables::from([(String::new(), vec!["crab".to_string()])]),
            // Nothing to pick from.
            Variables::from([("animal".to_string(), Vec::new())]),
            Variables::from([("animal".to_string(), vec!["  ".to_string()])]),
            // One variable under two spellings.
            Variables::from([
                ("animal".to_string(), vec!["crab".to_string()]),
                ("Animal".to_string(), vec!["horse".to_string()]),
            ]),
        ];
        for v in cases {
            assert!(validate(&v).is_err(), "{v:?} was accepted");
        }
        assert!(validate(&vars()).is_ok());
        assert!(validate(&Variables::new()).is_ok());
    }

    #[test]
    fn normalizing_lowercases_the_name_and_trims_the_words() {
        let v = Variables::from([(
            " Animal ".to_string(),
            vec![" crab ".to_string(), String::new(), "horse".to_string()],
        )]);
        let out = normalize(&v).unwrap();
        assert_eq!(out["animal"], vec!["crab".to_string(), "horse".to_string()]);
        // And a pair that would collapse into one is refused rather than
        // quietly losing a row the user can see.
        let pair = Variables::from([
            ("Animal".to_string(), vec!["crab".to_string()]),
            ("animal".to_string(), vec!["horse".to_string()]),
        ]);
        assert!(normalize(&pair).is_err());
    }
}
