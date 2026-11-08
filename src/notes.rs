//! A note per model file, `user/model-notes.toml`: plain text the person wrote
//! about a model — what it is good at, the settings that worked — keyed by its
//! [`crate::inventory`] id, `GET /model-notes` and `POST /model-notes`.
//!
//! Keyed by where the file is, not by a fingerprint of it: a file moved or
//! renamed outside yasdig loses its note until it is put back, and a note
//! whose file is gone is kept (not shown) for the same reason
//! `model-settings.toml` parks rather than deletes — the file may be in another
//! folder for an afternoon. Only deleting the file from the Models tab deletes
//! its note.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};

/// Read-modify-write under one lock: two tabs saving two notes at once must
/// both land.
static LOCK: Mutex<()> = Mutex::new(());

const FILE: &str = "model-notes.toml";
const HEADER: &str = "\
# Your notes on model files, written in the Models tab (the blue i).
# Keyed by where each file is: <folder>/<path under it>. Rewritten whole on
# every save.

";

/// The longest note kept — a typo guard for a pasted file, not a budget.
pub const MAX_CHARS: usize = 20_000;

fn path(user_dir: &Path) -> PathBuf {
    user_dir.join(FILE)
}

fn read(user_dir: &Path) -> Result<BTreeMap<String, String>> {
    let p = path(user_dir);
    if !p.exists() {
        return Ok(BTreeMap::new());
    }
    let text = std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", p.display()))
}

fn write(user_dir: &Path, notes: &BTreeMap<String, String>) -> Result<()> {
    let p = path(user_dir);
    if notes.is_empty() {
        let _ = std::fs::remove_file(&p);
        return Ok(());
    }
    std::fs::create_dir_all(user_dir).with_context(|| format!("creating {}", user_dir.display()))?;
    let body = format!("{HEADER}{}", toml::to_string_pretty(notes).context("serializing notes")?);
    let tmp = p.with_extension("toml.tmp");
    std::fs::write(&tmp, body).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &p).with_context(|| format!("writing {}", p.display()))?;
    Ok(())
}

fn lock() -> std::sync::MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

/// Every note. A broken file reads as none, with the reason on the console:
/// a note is never worth a failed page.
pub fn all(user_dir: &Path) -> BTreeMap<String, String> {
    let _g = lock();
    read(user_dir).unwrap_or_else(|e| {
        eprintln!("[yasdig] {e:#}");
        BTreeMap::new()
    })
}

/// Save `text` as `id`'s note; blank removes it.
pub fn set(user_dir: &Path, id: &str, text: &str) -> Result<()> {
    if text.chars().count() > MAX_CHARS {
        anyhow::bail!("a note is at most {MAX_CHARS} characters");
    }
    let _g = lock();
    let mut notes = read(user_dir)?;
    if text.trim().is_empty() {
        if notes.remove(id).is_none() {
            return Ok(());
        }
    } else {
        notes.insert(id.to_string(), text.to_string());
    }
    write(user_dir, &notes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_note_round_trips_and_blank_removes_it() {
        let dir = crate::config::fresh_test_root("yasdig-notes");
        set(&dir, "loras/sdxl/detail.safetensors", "Good at 0.6.\n\"quoted\"").unwrap();
        set(&dir, "models/a.gguf", "x").unwrap();
        let n = all(&dir);
        assert_eq!(n["loras/sdxl/detail.safetensors"], "Good at 0.6.\n\"quoted\"");
        set(&dir, "models/a.gguf", "  \n").unwrap();
        set(&dir, "loras/sdxl/detail.safetensors", "").unwrap();
        assert!(all(&dir).is_empty());
        assert!(!path(&dir).exists(), "no notes, no file");
        assert!(set(&dir, "x", &"a".repeat(MAX_CHARS + 1)).is_err());
    }
}
