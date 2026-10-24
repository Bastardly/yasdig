//! The agent's models: what is in `models/agents/` (`Config::agent_dir`), read
//! off GGUF headers, and paired into something the driver can run.
//!
//! An agent is two files in llama.cpp's layout: the language model, and — for
//! one that can see — its vision projector ("mmproj"). Both are GGUF, and both
//! say what they are in their metadata (`general.architecture` is `clip` for a
//! projector), so, as everywhere else here, nothing is identified by filename.
//!
//! **Pairing is by folder.** A projector carries `clip.vision.projection_dim`,
//! which is the language model's width for most projectors (Qwen-VL, Gemma)
//! and not for others — Mistral's pixtral projector says 14336, its own
//! feed-forward width, beside a 4096-wide model — so width can't be the rule.
//! A folder holding one language model and one projector is one agent; that is
//! how the catalog lays its downloads out. A folder with several of either is
//! paired by width where that is unambiguous, and anything left over is listed
//! with the reason, never guessed at. llama.cpp itself refuses a projector of
//! the wrong width when it loads, so a wrong pair fails loudly, not quietly.
//!
//! This is a scan of its own, apart from the image scan (which leaves the
//! folder out): a Qwen agent is also the shape of a text encoder, and the two
//! listings must never compete for one file.

use crate::discover::{read_gguf_string, read_u32, read_u64, skip_gguf_value};
use anyhow::{bail, Result};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

/// The metadata keys an agent file is judged by. Everything else in the
/// header (the vocabulary is most of it) is skipped.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GgufMeta {
    pub arch: Option<String>,
    pub name: Option<String>,
    /// `{arch}.embedding_length`.
    pub n_embd: Option<u64>,
    /// `{arch}.context_length`: what the model was trained to, not what a
    /// turn asks for.
    pub n_ctx_train: Option<u64>,
    pub chat_template: bool,
    pub has_vision: Option<bool>,
    pub projector_type: Option<String>,
    /// `clip.vision.projection_dim`; see the module comment for why it is only
    /// a tiebreak.
    pub projection_dim: Option<u64>,
    /// The model author's own sampling, where the file carries it
    /// (`general.sampling.*` — Gemma 4's do).
    pub temp: Option<f32>,
    pub top_p: Option<f32>,
    pub top_k: Option<u32>,
}

/// A numeric or boolean metadata value, as the few keys read here need.
enum Scalar {
    Int(u64),
    Float(f32),
    Bool(bool),
    Text(String),
}

fn read_scalar(r: &mut impl Read, ty: u32) -> Result<Option<Scalar>> {
    let mut b = [0u8; 8];
    Ok(Some(match ty {
        0 | 1 => {
            r.read_exact(&mut b[..1])?;
            Scalar::Int(b[0] as u64)
        }
        2 | 3 => {
            r.read_exact(&mut b[..2])?;
            Scalar::Int(u16::from_le_bytes([b[0], b[1]]) as u64)
        }
        4 | 5 => Scalar::Int(read_u32(r)? as u64),
        6 => Scalar::Float(f32::from_bits(read_u32(r)?)),
        7 => {
            r.read_exact(&mut b[..1])?;
            Scalar::Bool(b[0] != 0)
        }
        8 => Scalar::Text(read_gguf_string(r)?),
        10 | 11 => Scalar::Int(read_u64(r)?),
        12 => Scalar::Float(f64::from_bits(read_u64(r)?) as f32),
        _ => {
            skip_gguf_value(r, ty)?;
            return Ok(None);
        }
    }))
}

/// Read a GGUF file's metadata block. Stops before the tensor index, which an
/// agent file is not judged by.
pub fn read_meta(path: &Path) -> Result<GgufMeta> {
    let f = std::fs::File::open(path)?;
    let mut r = std::io::BufReader::new(f);
    let mut magic = [0u8; 4];
    r.read_exact(&mut magic)?;
    if &magic != b"GGUF" {
        bail!("not a GGUF file");
    }
    let version = read_u32(&mut r)?;
    if !(2..=3).contains(&version) {
        bail!("GGUF version {version} is not supported (expected 2 or 3)");
    }
    let _tensors = read_u64(&mut r)?;
    let kv_count = read_u64(&mut r)?;
    if kv_count > 1_000_000 {
        bail!("implausible GGUF header: {kv_count} metadata keys");
    }
    let mut m = GgufMeta::default();
    // `{arch}.` keys are kept by suffix and resolved once the architecture is
    // known — llama.cpp writes it first, but nothing requires that.
    let mut by_arch: BTreeMap<String, u64> = BTreeMap::new();
    for _ in 0..kv_count {
        let key = read_gguf_string(&mut r)?;
        let ty = read_u32(&mut r)?;
        let wanted = matches!(
            key.as_str(),
            "general.architecture"
                | "general.name"
                | "tokenizer.chat_template"
                | "clip.has_vision_encoder"
                | "clip.projector_type"
                | "clip.vision.projection_dim"
                | "general.sampling.temp"
                | "general.sampling.top_p"
                | "general.sampling.top_k"
        ) || key.ends_with(".embedding_length")
            || key.ends_with(".context_length");
        if !wanted {
            skip_gguf_value(&mut r, ty)?;
            continue;
        }
        let Some(v) = read_scalar(&mut r, ty)? else { continue };
        match (key.as_str(), v) {
            ("general.architecture", Scalar::Text(s)) => m.arch = Some(s),
            ("general.name", Scalar::Text(s)) => m.name = Some(s),
            ("tokenizer.chat_template", Scalar::Text(s)) => m.chat_template = !s.trim().is_empty(),
            ("clip.has_vision_encoder", Scalar::Bool(b)) => m.has_vision = Some(b),
            ("clip.projector_type", Scalar::Text(s)) => m.projector_type = Some(s),
            ("clip.vision.projection_dim", Scalar::Int(n)) => m.projection_dim = Some(n),
            ("general.sampling.temp", Scalar::Float(x)) => m.temp = Some(x),
            ("general.sampling.top_p", Scalar::Float(x)) => m.top_p = Some(x),
            ("general.sampling.top_k", Scalar::Int(n)) => m.top_k = Some(n as u32),
            (k, Scalar::Int(n)) => {
                by_arch.insert(k.to_string(), n);
            }
            _ => {}
        }
    }
    if let Some(a) = &m.arch {
        m.n_embd = by_arch.get(&format!("{a}.embedding_length")).copied();
        m.n_ctx_train = by_arch.get(&format!("{a}.context_length")).copied();
    }
    Ok(m)
}

/// What one file in the agents folder is.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentFile {
    Lm(GgufMeta),
    Projector(GgufMeta),
    Unknown(String),
}

/// Judge one file, memoized on `(path, length, mtime)` like
/// `discover::identify` — a download that finishes is the same path with a new
/// length, and is read again.
pub fn identify(path: &Path) -> AgentFile {
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};
    use std::time::SystemTime;
    type Memo = HashMap<PathBuf, (u64, SystemTime, AgentFile)>;
    static MEMO: LazyLock<Mutex<Memo>> = LazyLock::new(|| Mutex::new(HashMap::new()));

    let stamp = std::fs::metadata(path).ok().and_then(|m| Some((m.len(), m.modified().ok()?)));
    if let Some((len, mtime)) = stamp {
        if let Some((l, m, f)) = MEMO.lock().unwrap().get(path) {
            if (*l, *m) == (len, mtime) {
                return f.clone();
            }
        }
    }
    let file = match read_meta(path) {
        Ok(m) => classify(m),
        Err(e) => AgentFile::Unknown(format!("could not be read ({e})")),
    };
    if let Some((len, mtime)) = stamp {
        MEMO.lock().unwrap().insert(path.to_path_buf(), (len, mtime, file.clone()));
    }
    file
}

fn classify(m: GgufMeta) -> AgentFile {
    match m.arch.as_deref() {
        Some("clip") if m.has_vision == Some(false) => {
            AgentFile::Unknown("a projector without a vision encoder (audio only), which the agent can't use".into())
        }
        Some("clip") => AgentFile::Projector(m),
        Some(_) if !m.chat_template => AgentFile::Unknown(
            "a language model without a chat template — a base model, or a text encoder; \
             the agent needs an instruct model"
                .into(),
        ),
        Some(_) => AgentFile::Lm(m),
        None => AgentFile::Unknown("a GGUF file with no architecture".into()),
    }
}

/// An agent the page can pick: a language model and, if it can see, its
/// projector.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentModel {
    /// The language model's path under the agents folder, `/`-separated and
    /// without `.gguf` — the name on the wire and in a chat.
    pub name: String,
    /// What the file calls itself (`general.name`), for the select.
    pub label: String,
    pub lm: PathBuf,
    pub mmproj: Option<PathBuf>,
    pub arch: String,
    pub n_ctx_train: Option<u64>,
    /// Both files, for the fits-the-card note.
    pub bytes: u64,
    pub meta: GgufMeta,
    /// Why it has no projector, when it is in a folder that has one it
    /// couldn't be paired with.
    pub note: Option<String>,
}

impl AgentModel {
    pub fn vision(&self) -> bool {
        self.mmproj.is_some()
    }
}

/// What a scan found: the agents, and every file that isn't part of one, with
/// the reason (`doctor` lists them).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Scan {
    pub models: Vec<AgentModel>,
    pub other: Vec<(PathBuf, String)>,
}

/// Every agent under `dir`, sorted by name. Recursive two folders deep: the
/// catalog puts each agent in a folder of its own.
pub fn scan(dir: &Path) -> Scan {
    let mut by_folder: BTreeMap<PathBuf, Vec<(PathBuf, AgentFile)>> = BTreeMap::new();
    walk(dir, 0, &mut |p| {
        let folder = p.parent().unwrap_or(dir).to_path_buf();
        by_folder.entry(folder).or_default().push((p.to_path_buf(), identify(p)));
    });
    let mut out = Scan::default();
    for (_, files) in by_folder {
        pair_folder(dir, files, &mut out);
    }
    out.models.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn walk(dir: &Path, depth: u32, f: &mut dyn FnMut(&Path)) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            if depth < 2 {
                walk(&p, depth + 1, f);
            }
        } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("gguf")) {
            f(&p);
        }
    }
}

fn len(p: &Path) -> u64 {
    std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
}

/// Pair one folder's files (see the module comment).
fn pair_folder(root: &Path, files: Vec<(PathBuf, AgentFile)>, out: &mut Scan) {
    let mut lms = Vec::new();
    let mut projs = Vec::new();
    for (p, f) in files {
        match f {
            AgentFile::Lm(m) => lms.push((p, m)),
            AgentFile::Projector(m) => projs.push((p, m)),
            AgentFile::Unknown(why) => out.other.push((p, why)),
        }
    }
    let mut taken = vec![false; projs.len()];
    let one_each = lms.len() == 1 && projs.len() == 1;
    for (lm, m) in lms.iter() {
        let pick = if one_each {
            Some(0)
        } else {
            // By width, only where exactly one projector fits.
            let fits: Vec<usize> = (0..projs.len())
                .filter(|&i| !taken[i] && m.n_embd.is_some() && projs[i].1.projection_dim == m.n_embd)
                .collect();
            (fits.len() == 1).then(|| fits[0])
        };
        if let Some(i) = pick {
            taken[i] = true;
        }
        let note = (pick.is_none() && !projs.is_empty()).then(|| {
            "this folder holds several models or projectors that can't be told apart — \
             put each model in a folder with its own projector"
                .to_string()
        });
        let mmproj = pick.map(|i| projs[i].0.clone());
        let stem = lm.with_extension("");
        out.models.push(AgentModel {
            name: crate::config::slash_name(root, &stem),
            label: m.name.clone().unwrap_or_else(|| {
                lm.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
            }),
            bytes: len(lm) + mmproj.as_deref().map_or(0, len),
            lm: lm.clone(),
            mmproj,
            arch: m.arch.clone().unwrap_or_default(),
            n_ctx_train: m.n_ctx_train,
            meta: m.clone(),
            note,
        });
    }
    for (i, (p, _)) in projs.into_iter().enumerate() {
        if !taken[i] {
            out.other.push((p, "a vision projector with no language model it could be paired with".into()));
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A GGUF header with the given metadata and no tensors.
    pub(crate) fn gguf(kv: &[(&str, Kv)]) -> Vec<u8> {
        let mut b = b"GGUF".to_vec();
        b.extend(3u32.to_le_bytes());
        b.extend(0u64.to_le_bytes());
        b.extend((kv.len() as u64).to_le_bytes());
        let s = |b: &mut Vec<u8>, t: &str| {
            b.extend((t.len() as u64).to_le_bytes());
            b.extend(t.as_bytes());
        };
        for (k, v) in kv {
            s(&mut b, k);
            match v {
                Kv::S(t) => {
                    b.extend(8u32.to_le_bytes());
                    s(&mut b, t);
                }
                Kv::U(n) => {
                    b.extend(4u32.to_le_bytes());
                    b.extend(n.to_le_bytes());
                }
                Kv::F(x) => {
                    b.extend(6u32.to_le_bytes());
                    b.extend(x.to_le_bytes());
                }
                Kv::B(x) => {
                    b.extend(7u32.to_le_bytes());
                    b.push(*x as u8);
                }
                Kv::Strings(n) => {
                    b.extend(9u32.to_le_bytes());
                    b.extend(8u32.to_le_bytes());
                    b.extend((*n as u64).to_le_bytes());
                    for i in 0..*n {
                        s(&mut b, &format!("tok{i}"));
                    }
                }
            }
        }
        b
    }

    pub(crate) enum Kv {
        S(&'static str),
        U(u32),
        F(f32),
        B(bool),
        Strings(usize),
    }

    pub(crate) fn lm(arch: &'static str, width: u32) -> Vec<u8> {
        let (e, c) = match arch {
            "qwen35" => ("qwen35.embedding_length", "qwen35.context_length"),
            "gemma4" => ("gemma4.embedding_length", "gemma4.context_length"),
            _ => ("mistral3.embedding_length", "mistral3.context_length"),
        };
        gguf(&[
            ("general.architecture", Kv::S(arch)),
            ("general.name", Kv::S("Test LM")),
            (e, Kv::U(width)),
            (c, Kv::U(262144)),
            ("tokenizer.ggml.tokens", Kv::Strings(3)),
            ("tokenizer.chat_template", Kv::S("{{ messages }}")),
            ("general.sampling.temp", Kv::F(1.0)),
        ])
    }

    pub(crate) fn proj(dim: u32) -> Vec<u8> {
        gguf(&[
            ("general.architecture", Kv::S("clip")),
            ("clip.has_vision_encoder", Kv::B(true)),
            ("clip.vision.projection_dim", Kv::U(dim)),
            ("clip.projector_type", Kv::S("qwen3vl_merger")),
        ])
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("yasdig-agents-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_header_says_what_a_file_is() {
        let d = tmp("meta");
        std::fs::write(d.join("a.gguf"), lm("qwen35", 2560)).unwrap();
        let m = read_meta(&d.join("a.gguf")).unwrap();
        assert_eq!(m.arch.as_deref(), Some("qwen35"));
        assert_eq!(m.n_embd, Some(2560));
        assert_eq!(m.n_ctx_train, Some(262144));
        assert!(m.chat_template);
        assert_eq!(m.temp, Some(1.0));
        assert!(matches!(classify(m), AgentFile::Lm(_)));
        std::fs::write(d.join("b.gguf"), proj(2560)).unwrap();
        assert!(matches!(identify(&d.join("b.gguf")), AgentFile::Projector(_)));
        // A text encoder: an LLM with no chat template.
        std::fs::write(d.join("c.gguf"), gguf(&[("general.architecture", Kv::S("qwen3"))])).unwrap();
        assert!(matches!(identify(&d.join("c.gguf")), AgentFile::Unknown(_)));
    }

    #[test]
    fn a_folder_with_one_of_each_is_one_agent_whatever_the_widths_say() {
        let d = tmp("pair");
        // Mistral's pixtral projector: 14336 beside a 4096-wide model.
        std::fs::create_dir_all(d.join("ministral")).unwrap();
        std::fs::write(d.join("ministral/m.gguf"), lm("mistral3", 4096)).unwrap();
        std::fs::write(d.join("ministral/mmproj.gguf"), proj(14336)).unwrap();
        // A text-only model on its own.
        std::fs::write(d.join("plain.gguf"), lm("qwen35", 2560)).unwrap();
        let s = scan(&d);
        assert_eq!(s.models.len(), 2, "{s:?}");
        let m = &s.models[0];
        assert_eq!(m.name, "ministral/m");
        assert!(m.vision());
        assert!(!s.models[1].vision());
        assert!(s.other.is_empty());
    }

    #[test]
    fn several_in_one_folder_pair_by_width_and_never_guess() {
        let d = tmp("several");
        std::fs::write(d.join("a.gguf"), lm("qwen35", 2560)).unwrap();
        std::fs::write(d.join("b.gguf"), lm("mistral3", 4096)).unwrap();
        std::fs::write(d.join("pa.gguf"), proj(2560)).unwrap();
        std::fs::write(d.join("pb.gguf"), proj(14336)).unwrap();
        let s = scan(&d);
        let a = s.models.iter().find(|m| m.name == "a").unwrap();
        let b = s.models.iter().find(|m| m.name == "b").unwrap();
        assert_eq!(a.mmproj.as_deref(), Some(d.join("pa.gguf").as_path()));
        assert!(b.mmproj.is_none() && b.note.is_some());
        assert_eq!(s.other.len(), 1, "the 14336 projector is left over, with a reason");
    }
}
