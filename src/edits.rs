//! A work item's Refine & adjust edits, kept beside it: `{shelf}/{id}.edits/`.
//!
//! An edited image lives in scratch (`work/current/`) as a `Pending`, which
//! goes when another image takes its slot or the server stops. This folder is
//! what brings it back: every History step's raw, every brush mask, and
//! `edits.json` naming them with the look (filters, area, brush amounts).
//! Written on every write-back of an edited work item (`Retained`), so a new
//! History step is a save, and read by `Display::open_in`.
//!
//! Invariant 8 holds both ways: files are **linked or copied out** of scratch
//! and **copied in** on open, so `Pending::remove_files` never reaches here
//! and nothing here is owned by a `Pending`.
//!
//! Names here are scratch names with a per-run prefix. Scratch names are
//! unique within one run only — job ids restart at 0 — and a file already in
//! the folder is never written again, so a name from an earlier run must not
//! be able to come back meaning another picture.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::area::Area;
use crate::brush::Tool;
use crate::config::{Filters, Profile};

const RECORD: &str = "edits.json";
pub const VERSION: u32 = 1;

/// What `edits.json` holds. Files are names inside the folder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub version: u32,
    /// Which step is on screen.
    pub at: usize,
    pub steps: Vec<Step>,
    pub filters: Filters,
    pub area: Option<Area>,
    pub brushes: Vec<BrushRec>,
}

/// One History step: `pipeline::Snapshot` with a file name for its raw.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub file: String,
    pub profile: Profile,
    pub refine_passes: u32,
    pub hires_passes: u32,
    pub step: u32,
    pub label: String,
}

/// One brush tool's strokes: `brush::Stroke` with a file name for its mask.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrushRec {
    pub tool: Tool,
    pub file: String,
    pub amount: f32,
    pub from: Option<usize>,
}

/// This run's prefix for names in an edits folder: start time and process id.
fn run_prefix() -> &'static str {
    static P: OnceLock<String> = OnceLock::new();
    P.get_or_init(|| {
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        format!("r{ms}-{}", std::process::id())
    })
}

/// The name a scratch file takes in an edits folder.
pub fn name_for(scratch: &Path) -> String {
    let base = scratch.file_name().and_then(|n| n.to_str()).unwrap_or("file.png");
    format!("{}-{base}", run_prefix())
}

/// Write `record` to `dir`: `files` (folder name → scratch source) first, each
/// only when the folder lacks it — a hard link, a copy when linking fails —
/// then the record through a temporary name, then every file the record no
/// longer names is deleted. A crash at any point leaves the last record whole.
pub fn save(dir: &Path, record: &Record, files: &[(String, PathBuf)]) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    for (name, src) in files {
        let dst = dir.join(name);
        if dst.is_file() {
            continue;
        }
        if std::fs::hard_link(src, &dst).is_err() {
            std::fs::copy(src, &dst).with_context(|| format!("keeping {}", src.display()))?;
        }
    }
    let tmp = dir.join(format!("{RECORD}.part"));
    std::fs::write(&tmp, serde_json::to_vec_pretty(record)?)
        .with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, dir.join(RECORD)).with_context(|| format!("writing {RECORD}"))?;
    let keep: BTreeSet<&str> = record
        .steps
        .iter()
        .map(|s| s.file.as_str())
        .chain(record.brushes.iter().map(|b| b.file.as_str()))
        .chain([RECORD])
        .collect();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let name = e.file_name();
            let stale = name.to_str().is_none_or(|n| !keep.contains(n));
            if stale && e.path().is_file() {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    Ok(())
}

/// The record in `dir`, or `None` when there is none. An unreadable record, a
/// newer version or a missing file is an error: the caller opens the original.
pub fn load(dir: &Path) -> Result<Option<Record>> {
    let path = dir.join(RECORD);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let r: Record = serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))?;
    if r.version != VERSION {
        bail!("{} is version {}, this build reads {VERSION}", path.display(), r.version);
    }
    if r.steps.is_empty() || r.at >= r.steps.len() {
        bail!("{} has no step {}", path.display(), r.at);
    }
    for f in r.steps.iter().map(|s| &s.file).chain(r.brushes.iter().map(|b| &b.file)) {
        // Names only: a record is ours, but never a reason to read elsewhere.
        if f.contains(['/', '\\']) || f.starts_with('.') || !dir.join(f).is_file() {
            bail!("{} names a missing file '{f}'", path.display());
        }
    }
    Ok(Some(r))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("yasdig-edits-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn step(file: &str) -> Step {
        Step {
            file: file.into(),
            profile: Profile::default(),
            refine_passes: 0,
            hires_passes: 0,
            step: 0,
            label: "Generated".into(),
        }
    }

    /// Saved files are kept by name, the record round-trips, and a later
    /// save that drops a step deletes its file — the scratch source never.
    #[test]
    fn a_save_keeps_what_the_record_names_and_nothing_else() {
        let root = tmp("save");
        let scratch = root.join("scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        let (a, b) = (scratch.join("1-0-raw.png"), scratch.join("1-1-5-raw.png"));
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();
        let dir = root.join("x.edits");
        let files = vec![(name_for(&a), a.clone()), (name_for(&b), b.clone())];
        let mut rec = Record {
            version: VERSION,
            at: 1,
            steps: vec![step(&files[0].0), step(&files[1].0)],
            filters: Filters { saturation: 0.25, ..Filters::NONE },
            area: None,
            brushes: Vec::new(),
        };
        save(&dir, &rec, &files).unwrap();
        let back = load(&dir).unwrap().unwrap();
        assert_eq!((back.at, back.steps.len(), back.filters.saturation), (1, 2, 0.25));
        // Removing scratch leaves the folder whole (a link, not the same name).
        std::fs::remove_file(&b).unwrap();
        assert_eq!(std::fs::read(dir.join(&files[1].0)).unwrap(), b"b");

        rec.steps.pop();
        rec.at = 0;
        save(&dir, &rec, &files[..1]).unwrap();
        assert!(!dir.join(&files[1].0).exists(), "a dropped step's file goes");
        assert!(a.is_file(), "scratch is never touched");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// No record is no edits; a broken one, or one naming a missing file, is
    /// an error the opener falls back from.
    #[test]
    fn a_missing_or_broken_record_is_told_apart() {
        let dir = tmp("load");
        assert!(load(&dir).unwrap().is_none());
        std::fs::write(dir.join(RECORD), b"{ not json").unwrap();
        assert!(load(&dir).is_err());
        let rec = Record {
            version: VERSION,
            at: 0,
            steps: vec![step("gone.png")],
            filters: Filters::NONE,
            area: None,
            brushes: Vec::new(),
        };
        std::fs::write(dir.join(RECORD), serde_json::to_vec(&rec).unwrap()).unwrap();
        assert!(load(&dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
