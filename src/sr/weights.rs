//! Reading an upscaler's weights: safetensors, and PyTorch's `.pth`/`.pt`.
//!
//! Most upscalers ship as `.pth` — `torch.save` of a state dict — which is a
//! pickle, and no Python means reading that pickle here. It is less than it
//! sounds: `torch.save` emits a small, fixed subset of the pickle opcodes, and a
//! state dict is a flat `OrderedDict` of names to `_rebuild_tensor_v2` calls
//! whose storages live beside it. sd.cpp reads the same subset for the same
//! reason (`src/model_io/pickle_io.cpp`), and this follows it.
//!
//! Two container formats exist:
//!
//! - **zip** (torch ≥ 1.6, the default since 2020): `archive/data.pkl` plus one
//!   entry per storage under `archive/data/<key>`. torch writes these entries
//!   *stored*, never deflated, so a storage is a byte range of the file and no
//!   decompressor is needed. A deflated entry is refused by name.
//! - **legacy** (older ESRGAN releases): five pickles back to back — magic,
//!   protocol, sys info, the object, then the list of storage keys — followed by
//!   each storage as a `u64` element count and its raw bytes, in key order.
//!
//! What comes out is always the same: names, shapes, and — when asked for — the
//! floats. Integer tensors (`relative_position_index`, BatchNorm's
//! `num_batches_tracked`) keep their shape but not their data; every network
//! here recomputes its index tables rather than trusting a buffer.

use anyhow::{bail, Context, Result};
use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// One tensor. `shape` is PyTorch order (`[out, in, kh, kw]` for a conv), and
/// `data` is row-major in that order, as f32 whatever the file stored.
#[derive(Debug, Clone)]
pub struct Tensor {
    pub shape: Vec<usize>,
    pub data: Option<Vec<f32>>,
}

/// A state dict, keyed by the names the reference implementation uses.
#[derive(Debug, Default)]
pub struct Weights {
    pub map: BTreeMap<String, Tensor>,
}

impl Weights {
    pub fn has(&self, name: &str) -> bool {
        self.map.contains_key(name)
    }

    pub fn shape(&self, name: &str) -> Option<&[usize]> {
        self.map.get(name).map(|t| t.shape.as_slice())
    }

    /// The floats of a tensor that must exist, with the name in the error.
    pub fn get(&self, name: &str) -> Result<&[f32]> {
        let t = self
            .map
            .get(name)
            .with_context(|| format!("the model has no tensor '{name}'"))?;
        t.data
            .as_deref()
            .with_context(|| format!("tensor '{name}' was not loaded as floats"))
    }

    /// Like [`Weights::get`], and also checks the element count.
    pub fn get_n(&self, name: &str, n: usize) -> Result<&[f32]> {
        let d = self.get(name)?;
        if d.len() != n {
            bail!("tensor '{name}' has {} elements, expected {n}", d.len());
        }
        Ok(d)
    }

    /// Every name matching `prefix` + a number + `.`, as the numbers, sorted.
    /// How block counts are read: `layers.0.`, `layers.1.`, … → `[0, 1, …]`.
    pub fn indices(&self, prefix: &str) -> Vec<usize> {
        let mut out: Vec<usize> = self
            .map
            .keys()
            .filter_map(|k| k.strip_prefix(prefix))
            .filter_map(|rest| rest.split('.').next()?.parse().ok())
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// The first eight bytes of every legacy torch file: pickle protocol 2, then the
/// start of `LONG1` holding torch's magic number `0x1950a86a20f9469cfc6c`.
///
/// All eight, not just the `0x80` that opens any pickle. A safetensors file
/// starts with its header length as a little-endian `u64`, so one whose header
/// happens to be 0x…80 bytes long opens with `0x80` too — the DAT this feature
/// started from is 238 208 bytes of header, `80 a2 03 00`, and was read as a
/// pickle until this said otherwise.
const LEGACY_MAGIC: [u8; 8] = [0x80, 0x02, 0x8a, 0x0a, 0x6c, 0xfc, 0x9c, 0x46];

/// Read a model file. `data: false` reads names and shapes only — what the
/// listing needs, and cheap: a safetensors header, or one pickle.
pub fn load(path: &Path, data: bool) -> Result<Weights> {
    let mut f = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut magic = [0u8; 8];
    let n = f.read(&mut magic)?;
    f.seek(SeekFrom::Start(0))?;
    let raw = if n >= 4 && magic[..4] == *b"PK\x03\x04" {
        read_torch_zip(&mut f, data)
    } else if n == 8 && magic == LEGACY_MAGIC {
        read_torch_legacy(&mut f, data)
    } else if n >= 4 && magic[..4] == *b"GGUF" {
        bail!("a GGUF upscaler is sd.cpp's format; only ESRGAN runs from one")
    } else {
        read_safetensors(&mut f, data)
    }
    .with_context(|| format!("reading {}", path.display()))?;
    Ok(Weights {
        map: unwrap_names(raw),
    })
}

/// Take the state dict out of whatever it was saved inside.
///
/// Training frameworks nest it: BasicSR saves `{"params": …, "params_ema": …}`,
/// others `{"state_dict": …}`, and `DataParallel` prefixes every name with
/// `module.`. Nested dicts arrive here flattened with their key as a prefix, so
/// this picks one wrapper — the EMA weights when there are both, since those
/// are the ones a release is evaluated with — and strips it.
fn unwrap_names(raw: Vec<(String, Tensor)>) -> BTreeMap<String, Tensor> {
    let wrapped = |p: &str| raw.iter().any(|(k, _)| k.starts_with(p));
    let pick = ["params_ema.", "params.", "state_dict.", "model_state_dict.", "net_g."]
        .into_iter()
        .find(|p| wrapped(p));
    let mut out: BTreeMap<String, Tensor> = BTreeMap::new();
    for (k, t) in raw {
        let k = match pick {
            Some(p) => match k.strip_prefix(p) {
                Some(rest) => rest.to_string(),
                None => continue,
            },
            None => k,
        };
        out.insert(k, t);
    }
    if !out.is_empty() && out.keys().all(|k| k.starts_with("module.")) {
        out = out
            .into_iter()
            .map(|(k, t)| (k["module.".len()..].to_string(), t))
            .collect();
    }
    out
}

// ---------------------------------------------------------------------------
// safetensors
// ---------------------------------------------------------------------------

/// Same bound as `discover`'s: a corrupt length field must not become a 16 EB
/// allocation.
const MAX_HEADER: u64 = 64 * 1024 * 1024;

fn read_safetensors(f: &mut std::fs::File, data: bool) -> Result<Vec<(String, Tensor)>> {
    let mut len = [0u8; 8];
    f.read_exact(&mut len)?;
    let len = u64::from_le_bytes(len);
    if len == 0 || len > MAX_HEADER {
        bail!("not a safetensors, .pth or .pt file (header length {len})");
    }
    let mut buf = vec![0u8; len as usize];
    f.read_exact(&mut buf)?;
    let json: BTreeMap<String, serde_json::Value> =
        serde_json::from_slice(&buf).context("the safetensors header is not JSON")?;
    let base = 8 + len;
    let mut out = Vec::new();
    for (name, v) in json {
        if name == "__metadata__" {
            continue;
        }
        let shape: Vec<usize> = v
            .get("shape")
            .and_then(|s| s.as_array())
            .map(|a| a.iter().filter_map(|d| d.as_u64()).map(|d| d as usize).collect())
            .unwrap_or_default();
        let dtype = match v.get("dtype").and_then(|d| d.as_str()) {
            Some("F32") => Some(DType::F32),
            Some("F16") => Some(DType::F16),
            Some("BF16") => Some(DType::BF16),
            Some("F64") => Some(DType::F64),
            _ => None,
        };
        let data = match (data, dtype) {
            (true, Some(dt)) => {
                let offs = v
                    .get("data_offsets")
                    .and_then(|o| o.as_array())
                    .and_then(|a| Some((a.first()?.as_u64()?, a.get(1)?.as_u64()?)))
                    .with_context(|| format!("tensor '{name}' has no data_offsets"))?;
                let n: usize = shape.iter().product();
                if offs.1 - offs.0 != (n * dt.size()) as u64 {
                    bail!("tensor '{name}': byte range does not match its shape");
                }
                f.seek(SeekFrom::Start(base + offs.0))?;
                let mut bytes = vec![0u8; (offs.1 - offs.0) as usize];
                f.read_exact(&mut bytes)?;
                Some(dt.to_f32(&bytes))
            }
            _ => None,
        };
        out.push((name, Tensor { shape, data }));
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum DType {
    F32,
    F16,
    BF16,
    F64,
    /// An integer or bool storage: its tensors keep their shape and nothing else.
    Other(usize),
}

impl DType {
    fn size(self) -> usize {
        match self {
            DType::F32 => 4,
            DType::F16 | DType::BF16 => 2,
            DType::F64 => 8,
            DType::Other(n) => n,
        }
    }

    /// From a pickled storage class, `torch.FloatStorage` and friends.
    fn from_storage(name: &str) -> Option<DType> {
        Some(match name.rsplit('.').next()? {
            "FloatStorage" => DType::F32,
            "HalfStorage" => DType::F16,
            "BFloat16Storage" => DType::BF16,
            "DoubleStorage" => DType::F64,
            "LongStorage" => DType::Other(8),
            "IntStorage" => DType::Other(4),
            "ShortStorage" => DType::Other(2),
            "CharStorage" | "ByteStorage" | "BoolStorage" => DType::Other(1),
            _ => return None,
        })
    }

    fn to_f32(self, b: &[u8]) -> Vec<f32> {
        match self {
            DType::F32 => b
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect(),
            DType::F16 => b
                .chunks_exact(2)
                .map(|c| f16_to_f32(u16::from_le_bytes([c[0], c[1]])))
                .collect(),
            DType::BF16 => b
                .chunks_exact(2)
                .map(|c| f32::from_bits((u16::from_le_bytes([c[0], c[1]]) as u32) << 16))
                .collect(),
            DType::F64 => b
                .chunks_exact(8)
                .map(|c| f64::from_le_bytes(c.try_into().unwrap()) as f32)
                .collect(),
            DType::Other(_) => Vec::new(),
        }
    }

    fn is_float(self) -> bool {
        !matches!(self, DType::Other(_))
    }
}

/// IEEE half to single, including subnormals, infinities and NaN.
pub fn f16_to_f32(h: u16) -> f32 {
    let sign = ((h >> 15) as u32) << 31;
    let exp = ((h >> 10) & 0x1f) as u32;
    let man = (h & 0x3ff) as u32;
    let bits = match (exp, man) {
        (0, 0) => sign,
        (0, m) => {
            // Subnormal: renormalise into a single's exponent range.
            let mut e = 127 - 15 + 1;
            let mut m = m;
            while m & 0x400 == 0 {
                m <<= 1;
                e -= 1;
            }
            sign | (e << 23) | ((m & 0x3ff) << 13)
        }
        (0x1f, m) => sign | 0x7f80_0000 | (m << 13),
        (e, m) => sign | ((e + 127 - 15) << 23) | (m << 13),
    };
    f32::from_bits(bits)
}

// ---------------------------------------------------------------------------
// torch zip
// ---------------------------------------------------------------------------

/// A stored zip entry's name and where its bytes are.
struct ZipEntry {
    name: String,
    offset: u64,
    size: u64,
}

/// The central directory of a zip, stored entries only.
fn zip_entries(f: &mut std::fs::File) -> Result<Vec<ZipEntry>> {
    let len = f.seek(SeekFrom::End(0))?;
    // The end-of-central-directory record is 22 bytes plus a comment of up to
    // 64 KiB, so it is somewhere in the last 64 KiB + 22.
    let tail = len.min(65_557);
    f.seek(SeekFrom::Start(len - tail))?;
    let mut buf = vec![0u8; tail as usize];
    f.read_exact(&mut buf)?;
    let eocd = (0..buf.len().saturating_sub(21))
        .rev()
        .find(|&i| buf[i..i + 4] == [0x50, 0x4b, 0x05, 0x06])
        .context("zip: no end-of-central-directory record")?;
    let u16at = |b: &[u8], i: usize| u16::from_le_bytes([b[i], b[i + 1]]) as u64;
    let u32at = |b: &[u8], i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]) as u64;
    let u64at = |b: &[u8], i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
    let mut count = u16at(&buf, eocd + 10);
    let mut cd_size = u32at(&buf, eocd + 12);
    let mut cd_off = u32at(&buf, eocd + 16);
    // ZIP64: torch writes one for any archive, so this is the common path, not
    // the rare one. The locator sits just before the EOCD and points at the
    // 64-bit record.
    if eocd >= 20 && buf[eocd - 20..eocd - 16] == [0x50, 0x4b, 0x06, 0x07] {
        let rec = u64at(&buf, eocd - 12);
        f.seek(SeekFrom::Start(rec))?;
        let mut r = [0u8; 56];
        f.read_exact(&mut r)?;
        if r[..4] != [0x50, 0x4b, 0x06, 0x06] {
            bail!("zip: bad ZIP64 end-of-central-directory record");
        }
        count = u64at(&r, 32);
        cd_size = u64at(&r, 40);
        cd_off = u64at(&r, 48);
    }
    if cd_size > MAX_HEADER || count > 1_000_000 {
        bail!("zip: implausible central directory");
    }
    f.seek(SeekFrom::Start(cd_off))?;
    let mut cd = vec![0u8; cd_size as usize];
    f.read_exact(&mut cd)?;

    let mut out = Vec::with_capacity(count as usize);
    let mut p = 0usize;
    for _ in 0..count {
        if p + 46 > cd.len() || cd[p..p + 4] != [0x50, 0x4b, 0x01, 0x02] {
            bail!("zip: bad central directory entry");
        }
        let method = u16at(&cd, p + 10);
        let mut size = u32at(&cd, p + 24);
        let usize_ = u32at(&cd, p + 20);
        let n = u16at(&cd, p + 28) as usize;
        let e = u16at(&cd, p + 30) as usize;
        let c = u16at(&cd, p + 32) as usize;
        let mut local = u32at(&cd, p + 42);
        let name = String::from_utf8_lossy(&cd[p + 46..p + 46 + n]).into_owned();
        // ZIP64 extra field: the 64-bit values appear in a fixed order, and
        // only for the fields whose 32-bit slot is saturated.
        let mut x = p + 46 + n;
        let xend = x + e;
        while x + 4 <= xend {
            let id = u16at(&cd, x);
            let sz = u16at(&cd, x + 2) as usize;
            if id == 1 {
                let mut q = x + 4;
                if usize_ == 0xffff_ffff {
                    q += 8; // uncompressed size, same as `size` for stored
                }
                if size == 0xffff_ffff {
                    size = u64at(&cd, q);
                    q += 8;
                }
                if local == 0xffff_ffff {
                    local = u64at(&cd, q);
                }
            }
            x += 4 + sz;
        }
        if size == 0xffff_ffff && usize_ != 0xffff_ffff {
            size = usize_;
        }
        if method != 0 {
            bail!("zip entry '{name}' is compressed; torch.save never writes one, so this is not a torch file");
        }
        out.push(ZipEntry { name, offset: local, size });
        p += 46 + n + e + c;
    }
    // Resolve the local headers: their extra field can differ from the central
    // one, so the data offset is only known by reading each.
    for ent in &mut out {
        f.seek(SeekFrom::Start(ent.offset))?;
        let mut h = [0u8; 30];
        f.read_exact(&mut h)?;
        if h[..4] != [0x50, 0x4b, 0x03, 0x04] {
            bail!("zip: bad local header for '{}'", ent.name);
        }
        ent.offset += 30 + u16at(&h, 26) + u16at(&h, 28);
    }
    Ok(out)
}

fn read_torch_zip(f: &mut std::fs::File, data: bool) -> Result<Vec<(String, Tensor)>> {
    let entries = zip_entries(f)?;
    let pkl = entries
        .iter()
        .find(|e| e.name.ends_with("data.pkl"))
        .context("no data.pkl in the zip — not a torch.save file")?;
    let root = pkl.name.trim_end_matches("data.pkl").to_string();
    f.seek(SeekFrom::Start(pkl.offset))?;
    let mut buf = vec![0u8; pkl.size as usize];
    f.read_exact(&mut buf)?;
    let (value, _) = Pickle::parse(&buf)?;
    let tensors = collect(&value);
    if tensors.is_empty() {
        bail!("the pickle holds no tensors — not a state dict");
    }
    let by_name: HashMap<&str, &ZipEntry> = entries.iter().map(|e| (e.name.as_str(), e)).collect();
    let mut out = Vec::with_capacity(tensors.len());
    for (name, t) in tensors {
        let floats = if data && t.dtype.is_float() {
            let key = format!("{root}data/{}", t.key);
            let ent = by_name
                .get(key.as_str())
                .with_context(|| format!("tensor '{name}': storage '{key}' is missing"))?;
            Some(read_strided(f, ent.offset, ent.size, &t, &name)?)
        } else {
            None
        };
        out.push((name, Tensor { shape: t.shape.clone(), data: floats }));
    }
    Ok(out)
}

/// A tensor's floats out of its storage, honouring offset and stride — a state
/// dict can hold views (a `qkv` split, a transposed weight) that share one.
fn read_strided(f: &mut std::fs::File, base: u64, size: u64, t: &PTensor, name: &str) -> Result<Vec<f32>> {
    let es = t.dtype.size();
    f.seek(SeekFrom::Start(base))?;
    let mut bytes = vec![0u8; size as usize];
    f.read_exact(&mut bytes)?;
    let all = t.dtype.to_f32(&bytes);
    let n: usize = t.shape.iter().product();
    let contiguous = {
        let mut s = 1usize;
        let mut ok = true;
        for (d, st) in t.shape.iter().zip(&t.stride).rev() {
            if *d > 1 && *st != s {
                ok = false;
            }
            s *= d;
        }
        ok
    };
    if contiguous {
        let end = t.offset + n;
        if end > all.len() {
            bail!("tensor '{name}' runs past its storage ({} of {} elements, {es}-byte)", end, all.len());
        }
        return Ok(all[t.offset..end].to_vec());
    }
    let mut out = Vec::with_capacity(n);
    let mut idx = vec![0usize; t.shape.len()];
    for _ in 0..n {
        let at = t.offset + idx.iter().zip(&t.stride).map(|(i, s)| i * s).sum::<usize>();
        out.push(*all.get(at).with_context(|| format!("tensor '{name}' strides past its storage"))?);
        for d in (0..idx.len()).rev() {
            idx[d] += 1;
            if idx[d] < t.shape[d] {
                break;
            }
            idx[d] = 0;
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// torch legacy
// ---------------------------------------------------------------------------

fn read_torch_legacy(f: &mut std::fs::File, data: bool) -> Result<Vec<(String, Tensor)>> {
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    let mut p = 0usize;
    // Magic number, protocol version, sys info: three pickles to step over.
    for _ in 0..3 {
        let (_, used) = Pickle::parse(&buf[p..]).context("legacy torch file: bad preamble")?;
        p += used;
    }
    let (value, used) = Pickle::parse(&buf[p..])?;
    p += used;
    let (keys, used) = Pickle::parse(&buf[p..])?;
    p += used;
    let tensors = collect(&value);
    if tensors.is_empty() {
        bail!("the pickle holds no tensors — not a state dict");
    }
    // Storages follow in key order, each an i64 element count then the bytes.
    let dtype_of: HashMap<&str, DType> = tensors.iter().map(|(_, t)| (t.key.as_str(), t.dtype)).collect();
    let mut at: HashMap<String, (usize, usize)> = HashMap::new();
    if let Value::List(ks) = &keys {
        for k in ks {
            let Value::Str(k) = k else { bail!("legacy torch file: storage key is not a string") };
            if p + 8 > buf.len() {
                bail!("legacy torch file: truncated storage '{k}'");
            }
            let n = u64::from_le_bytes(buf[p..p + 8].try_into().unwrap()) as usize;
            p += 8;
            let es = dtype_of.get(k.as_str()).map(|d| d.size()).unwrap_or(4);
            let bytes = n * es;
            if p + bytes > buf.len() {
                bail!("legacy torch file: storage '{k}' runs past the end");
            }
            at.insert(k.clone(), (p, bytes));
            p += bytes;
        }
    }
    let mut out = Vec::with_capacity(tensors.len());
    for (name, t) in tensors {
        let floats = if data && t.dtype.is_float() {
            let &(off, len) = at.get(&t.key).with_context(|| format!("tensor '{name}': storage missing"))?;
            let all = t.dtype.to_f32(&buf[off..off + len]);
            let n: usize = t.shape.iter().product();
            // Legacy states were written contiguous; views are refused rather
            // than half-supported.
            if t.offset + n > all.len() {
                bail!("tensor '{name}' runs past its storage");
            }
            Some(all[t.offset..t.offset + n].to_vec())
        } else {
            None
        };
        out.push((name, Tensor { shape: t.shape.clone(), data: floats }));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// pickle
// ---------------------------------------------------------------------------

/// A tensor as the pickle describes it: which storage, and how to view it.
#[derive(Debug, Clone, PartialEq)]
struct PTensor {
    key: String,
    dtype: DType,
    offset: usize,
    shape: Vec<usize>,
    stride: Vec<usize>,
}

/// The values a state-dict pickle can build. Anything else collapses to
/// `Other`, which is carried and ignored rather than refused — a checkpoint may
/// hold an optimizer state or an epoch count beside the weights.
#[derive(Debug, Clone, PartialEq)]
enum Value {
    None,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Bytes,
    Tuple(Vec<Value>),
    List(Vec<Value>),
    Dict(Vec<(Value, Value)>),
    Global(String),
    Storage { key: String, dtype: DType },
    Tensor(PTensor),
    Mark,
    Other,
}

/// Every tensor in a value, depth first, with nested dict keys joined by `.`.
fn collect(v: &Value) -> Vec<(String, PTensor)> {
    fn walk(v: &Value, prefix: &str, out: &mut Vec<(String, PTensor)>) {
        if let Value::Dict(items) = v {
            for (k, v) in items {
                let key = match k {
                    Value::Str(s) => s.clone(),
                    Value::Int(i) => i.to_string(),
                    _ => continue,
                };
                let name = if prefix.is_empty() { key } else { format!("{prefix}.{key}") };
                match v {
                    Value::Tensor(t) => out.push((name, t.clone())),
                    Value::Dict(_) => walk(v, &name, out),
                    _ => {}
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(v, "", &mut out);
    out
}

struct Pickle<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Pickle<'a> {
    /// Parse one pickle; returns its value and how many bytes it took.
    fn parse(b: &'a [u8]) -> Result<(Value, usize)> {
        let mut me = Pickle { b, p: 0 };
        let v = me.run()?;
        Ok((v, me.p))
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.p + n > self.b.len() {
            bail!("pickle: truncated");
        }
        let s = &self.b[self.p..self.p + n];
        self.p += n;
        Ok(s)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<usize> {
        let s = self.take(2)?;
        Ok(u16::from_le_bytes([s[0], s[1]]) as usize)
    }

    fn u32(&mut self) -> Result<usize> {
        let s = self.take(4)?;
        Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as usize)
    }

    fn line(&mut self) -> Result<String> {
        let start = self.p;
        while self.p < self.b.len() && self.b[self.p] != b'\n' {
            self.p += 1;
        }
        let s = String::from_utf8_lossy(&self.b[start..self.p]).into_owned();
        self.p += 1;
        Ok(s)
    }

    fn run(&mut self) -> Result<Value> {
        let mut stack: Vec<Value> = Vec::new();
        let mut memo: HashMap<usize, Value> = HashMap::new();
        let pop = |stack: &mut Vec<Value>| stack.pop().context("pickle: stack underflow");
        let pop_mark = |stack: &mut Vec<Value>| -> Result<Vec<Value>> {
            let at = stack
                .iter()
                .rposition(|v| *v == Value::Mark)
                .context("pickle: no mark")?;
            let items = stack.split_off(at + 1);
            stack.pop();
            Ok(items)
        };
        loop {
            let op = self.u8()?;
            match op {
                0x80 => {
                    self.u8()?; // PROTO
                }
                0x95 => {
                    self.take(8)?; // FRAME
                }
                b'.' => return pop(&mut stack),
                b'}' => stack.push(Value::Dict(Vec::new())),
                b']' => stack.push(Value::List(Vec::new())),
                b')' => stack.push(Value::Tuple(Vec::new())),
                b'(' => stack.push(Value::Mark),
                b'N' => stack.push(Value::None),
                0x88 => stack.push(Value::Bool(true)),
                0x89 => stack.push(Value::Bool(false)),
                b'J' => {
                    let s = self.take(4)?;
                    stack.push(Value::Int(i32::from_le_bytes([s[0], s[1], s[2], s[3]]) as i64));
                }
                b'K' => {
                    let v = self.u8()?;
                    stack.push(Value::Int(v as i64));
                }
                b'M' => {
                    let v = self.u16()?;
                    stack.push(Value::Int(v as i64));
                }
                0x8a => {
                    // LONG1: little-endian two's complement of n bytes.
                    let n = self.u8()? as usize;
                    let s = self.take(n)?;
                    let mut v: i128 = 0;
                    for (i, &byte) in s.iter().enumerate().take(16) {
                        v |= (byte as i128) << (8 * i);
                    }
                    if n > 0 && n < 16 && s[n - 1] & 0x80 != 0 {
                        v -= 1i128 << (8 * n);
                    }
                    stack.push(Value::Int(v as i64));
                }
                0x8b => {
                    let n = self.u32()?;
                    self.take(n)?;
                    stack.push(Value::Int(0));
                }
                b'G' => {
                    let s = self.take(8)?;
                    stack.push(Value::Float(f64::from_be_bytes(s.try_into().unwrap())));
                }
                b'X' => {
                    let n = self.u32()?;
                    let s = self.take(n)?;
                    stack.push(Value::Str(String::from_utf8_lossy(s).into_owned()));
                }
                0x8c => {
                    let n = self.u8()? as usize;
                    let s = self.take(n)?;
                    stack.push(Value::Str(String::from_utf8_lossy(s).into_owned()));
                }
                0x8d => {
                    let s = self.take(8)?;
                    let n = u64::from_le_bytes(s.try_into().unwrap()) as usize;
                    let s = self.take(n)?;
                    stack.push(Value::Str(String::from_utf8_lossy(s).into_owned()));
                }
                b'T' => {
                    let n = self.u32()?;
                    let s = self.take(n)?;
                    stack.push(Value::Str(String::from_utf8_lossy(s).into_owned()));
                }
                b'U' => {
                    let n = self.u8()? as usize;
                    let s = self.take(n)?;
                    stack.push(Value::Str(String::from_utf8_lossy(s).into_owned()));
                }
                b'B' => {
                    let n = self.u32()?;
                    self.take(n)?;
                    stack.push(Value::Bytes);
                }
                b'C' => {
                    let n = self.u8()? as usize;
                    self.take(n)?;
                    stack.push(Value::Bytes);
                }
                0x8e => {
                    let s = self.take(8)?;
                    let n = u64::from_le_bytes(s.try_into().unwrap()) as usize;
                    self.take(n)?;
                    stack.push(Value::Bytes);
                }
                b'c' => {
                    let module = self.line()?;
                    let name = self.line()?;
                    stack.push(Value::Global(format!("{module}.{name}")));
                }
                0x93 => {
                    let name = pop(&mut stack)?;
                    let module = pop(&mut stack)?;
                    match (module, name) {
                        (Value::Str(m), Value::Str(n)) => stack.push(Value::Global(format!("{m}.{n}"))),
                        _ => bail!("pickle: STACK_GLOBAL on non-strings"),
                    }
                }
                b'q' => {
                    let i = self.u8()? as usize;
                    memo.insert(i, stack.last().context("pickle: memo of nothing")?.clone());
                }
                b'r' => {
                    let i = self.u32()?;
                    memo.insert(i, stack.last().context("pickle: memo of nothing")?.clone());
                }
                0x94 => {
                    let i = memo.len();
                    memo.insert(i, stack.last().context("pickle: memo of nothing")?.clone());
                }
                b'h' => {
                    let i = self.u8()? as usize;
                    stack.push(memo.get(&i).context("pickle: unknown memo")?.clone());
                }
                b'j' => {
                    let i = self.u32()?;
                    stack.push(memo.get(&i).context("pickle: unknown memo")?.clone());
                }
                b't' => {
                    let items = pop_mark(&mut stack)?;
                    stack.push(Value::Tuple(items));
                }
                0x85 => {
                    let a = pop(&mut stack)?;
                    stack.push(Value::Tuple(vec![a]));
                }
                0x86 => {
                    let b = pop(&mut stack)?;
                    let a = pop(&mut stack)?;
                    stack.push(Value::Tuple(vec![a, b]));
                }
                0x87 => {
                    let c = pop(&mut stack)?;
                    let b = pop(&mut stack)?;
                    let a = pop(&mut stack)?;
                    stack.push(Value::Tuple(vec![a, b, c]));
                }
                b'l' => {
                    let items = pop_mark(&mut stack)?;
                    stack.push(Value::List(items));
                }
                b'd' => {
                    let items = pop_mark(&mut stack)?;
                    let pairs = items.chunks(2).filter(|c| c.len() == 2).map(|c| (c[0].clone(), c[1].clone())).collect();
                    stack.push(Value::Dict(pairs));
                }
                b'a' => {
                    let v = pop(&mut stack)?;
                    if let Some(Value::List(l)) = stack.last_mut() {
                        l.push(v);
                    }
                }
                b'e' => {
                    let items = pop_mark(&mut stack)?;
                    if let Some(Value::List(l)) = stack.last_mut() {
                        l.extend(items);
                    }
                }
                b's' => {
                    let v = pop(&mut stack)?;
                    let k = pop(&mut stack)?;
                    if let Some(Value::Dict(d)) = stack.last_mut() {
                        d.push((k, v));
                    }
                }
                b'u' => {
                    let items = pop_mark(&mut stack)?;
                    if let Some(Value::Dict(d)) = stack.last_mut() {
                        for c in items.chunks(2) {
                            if c.len() == 2 {
                                d.push((c[0].clone(), c[1].clone()));
                            }
                        }
                    }
                }
                b'b' => {
                    // BUILD: an OrderedDict's `_metadata`, or an object's state.
                    // The object itself is what is kept.
                    pop(&mut stack)?;
                }
                b'R' | 0x81 => {
                    let args = pop(&mut stack)?;
                    let callable = pop(&mut stack)?;
                    stack.push(reduce(callable, args));
                }
                b'Q' => {
                    let pid = pop(&mut stack)?;
                    stack.push(persistent(pid)?);
                }
                b'P' => {
                    let _ = self.line()?;
                    stack.push(Value::Other);
                }
                b'0' => {
                    pop(&mut stack)?;
                }
                b'2' => {
                    let top = stack.last().context("pickle: DUP of nothing")?.clone();
                    stack.push(top);
                }
                other => bail!("pickle: unsupported opcode 0x{other:02x} — not a plain state dict"),
            }
        }
    }
}

/// `('storage', torch.FloatStorage, '<key>', 'cpu', numel[, view])` → a storage.
fn persistent(pid: Value) -> Result<Value> {
    let Value::Tuple(items) = pid else { return Ok(Value::Other) };
    if items.first() != Some(&Value::Str("storage".into())) {
        return Ok(Value::Other);
    }
    let dtype = match items.get(1) {
        Some(Value::Global(g)) => DType::from_storage(g),
        _ => None,
    };
    let key = match items.get(2) {
        Some(Value::Str(s)) => s.clone(),
        Some(Value::Int(i)) => i.to_string(),
        _ => bail!("pickle: storage without a key"),
    };
    Ok(match dtype {
        Some(dtype) => Value::Storage { key, dtype },
        None => Value::Other,
    })
}

fn ints(v: &Value) -> Option<Vec<usize>> {
    match v {
        Value::Tuple(xs) | Value::List(xs) => xs
            .iter()
            .map(|x| match x {
                Value::Int(i) if *i >= 0 => Some(*i as usize),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

fn reduce(callable: Value, args: Value) -> Value {
    let Value::Global(name) = callable else { return Value::Other };
    let args = match args {
        Value::Tuple(a) => a,
        _ => return Value::Other,
    };
    match name.as_str() {
        "torch._utils._rebuild_tensor_v2" | "torch._utils._rebuild_tensor" => {
            let (Some(Value::Storage { key, dtype }), Some(Value::Int(off)), Some(shape), Some(stride)) = (
                args.first(),
                args.get(1),
                args.get(2).and_then(ints),
                args.get(3).and_then(ints),
            ) else {
                return Value::Other;
            };
            Value::Tensor(PTensor {
                key: key.clone(),
                dtype: *dtype,
                offset: *off as usize,
                shape,
                stride,
            })
        }
        // A saved `nn.Parameter` wraps the tensor.
        "torch._utils._rebuild_parameter" | "torch._utils._rebuild_parameter_with_state" => {
            args.into_iter().next().unwrap_or(Value::Other)
        }
        "collections.OrderedDict" => Value::Dict(Vec::new()),
        _ => Value::Other,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Pickle bytes for `{"a.weight": <2×3 f32 at key "0">, "n": <[4] i64 at "1">}`,
    /// written the way torch.save's protocol 2 writes a state dict.
    pub fn state_dict_pickle(wrapper: Option<&str>) -> Vec<u8> {
        let mut p = vec![0x80, 2];
        let str8 = |p: &mut Vec<u8>, s: &str| {
            p.push(b'X');
            p.extend((s.len() as u32).to_le_bytes());
            p.extend(s.as_bytes());
        };
        let tensor = |p: &mut Vec<u8>, class: &str, key: &str, shape: &[u8], stride: &[u8]| {
            p.extend(b"ctorch._utils\n_rebuild_tensor_v2\n");
            p.push(b'(');
            // persistent id tuple
            p.push(b'(');
            str8(p, "storage");
            p.extend(format!("ctorch\n{class}\n").as_bytes());
            str8(p, key);
            str8(p, "cpu");
            p.extend([b'K', 6]);
            p.push(b't');
            p.push(b'Q');
            p.extend([b'K', 0]); // offset
            p.push(b'(');
            for &d in shape {
                p.extend([b'K', d]);
            }
            p.push(b't');
            p.push(b'(');
            for &d in stride {
                p.extend([b'K', d]);
            }
            p.push(b't');
            p.push(0x89);
            p.extend(b"ccollections\nOrderedDict\n");
            p.push(b')');
            p.push(b'R');
            p.push(b't');
            p.push(b'R');
        };
        if let Some(w) = wrapper {
            p.push(b'}');
            str8(&mut p, w);
        }
        p.extend(b"ccollections\nOrderedDict\n");
        p.push(b')');
        p.push(b'R');
        p.push(b'(');
        str8(&mut p, "a.weight");
        tensor(&mut p, "FloatStorage", "0", &[2, 3], &[3, 1]);
        str8(&mut p, "n");
        tensor(&mut p, "LongStorage", "1", &[4], &[1]);
        p.push(b'u');
        if wrapper.is_some() {
            p.push(b's');
        }
        p.push(b'.');
        p
    }

    /// A stored zip holding `entries`, laid out as torch.save lays one out.
    pub fn stored_zip(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut cd = Vec::new();
        for (name, data) in entries {
            let off = out.len() as u32;
            out.extend([0x50, 0x4b, 0x03, 0x04]);
            out.extend([0u8; 22]);
            out[off as usize + 18..off as usize + 22].copy_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend((name.len() as u16).to_le_bytes());
            // A local extra field the central directory doesn't have, as torch
            // writes for alignment — the reason local headers are re-read.
            out.extend(4u16.to_le_bytes());
            out.extend(name.as_bytes());
            out.extend([0xfe, 0xca, 0, 0]);
            out.extend(data);
            cd.extend([0x50, 0x4b, 0x01, 0x02]);
            cd.extend([0u8; 16]);
            cd.extend((data.len() as u32).to_le_bytes());
            cd.extend((data.len() as u32).to_le_bytes());
            cd.extend((name.len() as u16).to_le_bytes());
            cd.extend([0u8; 12]);
            cd.extend(off.to_le_bytes());
            cd.extend(name.as_bytes());
        }
        let cd_off = out.len() as u32;
        let n = entries.len() as u16;
        out.extend(&cd);
        out.extend([0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0]);
        out.extend(n.to_le_bytes());
        out.extend(n.to_le_bytes());
        out.extend((cd.len() as u32).to_le_bytes());
        out.extend(cd_off.to_le_bytes());
        out.extend([0, 0]);
        out
    }

    fn floats(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|f| f.to_le_bytes()).collect()
    }

    fn tmp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("rustimage-sr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn a_torch_zip_state_dict_reads_back_its_floats() {
        for wrapper in [None, Some("params_ema")] {
            let zip = stored_zip(&[
                ("archive/data.pkl", state_dict_pickle(wrapper)),
                ("archive/data/0", floats(&[1., 2., 3., 4., 5., 6.])),
                ("archive/data/1", vec![0u8; 32]),
                ("archive/version", b"3\n".to_vec()),
            ]);
            let path = tmp(&format!("zip-{wrapper:?}.pth"), &zip);
            let w = load(&path, true).unwrap();
            assert_eq!(w.shape("a.weight"), Some(&[2, 3][..]), "{wrapper:?}");
            assert_eq!(w.get("a.weight").unwrap(), &[1., 2., 3., 4., 5., 6.]);
            // An integer buffer keeps its shape and nothing else.
            assert_eq!(w.shape("n"), Some(&[4][..]));
            assert!(w.get("n").is_err());
        }
    }

    #[test]
    fn a_transposed_view_is_read_through_its_stride() {
        // Same storage, but the pickle says shape [3,2] stride [1,3]: the
        // transpose of the 2×3 above.
        let pkl = state_dict_pickle(None);
        let find = |needle: &[u8]| pkl.windows(needle.len()).position(|w| w == needle).unwrap();
        let mut pkl2 = pkl.clone();
        let at = find(&[b'(', b'K', 2, b'K', 3, b't', b'(', b'K', 3, b'K', 1, b't']);
        pkl2[at..at + 12].copy_from_slice(&[b'(', b'K', 3, b'K', 2, b't', b'(', b'K', 1, b'K', 3, b't']);
        let zip = stored_zip(&[
            ("m/data.pkl", pkl2),
            ("m/data/0", floats(&[1., 2., 3., 4., 5., 6.])),
            ("m/data/1", vec![0u8; 32]),
        ]);
        let w = load(&tmp("view.pth", &zip), true).unwrap();
        assert_eq!(w.get("a.weight").unwrap(), &[1., 4., 2., 5., 3., 6.]);
    }

    #[test]
    fn a_legacy_torch_file_reads_back_its_floats() {
        let small = |v: u8| vec![0x80, 2, b'K', v, b'.'];
        let mut b = Vec::new();
        // The real magic number, as torch writes it.
        b.extend([0x80, 0x02, 0x8a, 0x0a, 0x6c, 0xfc, 0x9c, 0x46, 0xf9, 0x20, 0x6a, 0xa8, 0x50, 0x19, b'.']);
        b.extend(small(2)); // protocol
        b.extend(vec![0x80, 2, b'}', b'.']); // sys info
        b.extend(state_dict_pickle(None));
        // keys list
        b.extend([0x80, 2, b']', b'(']);
        for k in ["0", "1"] {
            b.push(b'X');
            b.extend(1u32.to_le_bytes());
            b.extend(k.as_bytes());
        }
        b.extend([b'e', b'.']);
        b.extend(6u64.to_le_bytes());
        b.extend(floats(&[1., 2., 3., 4., 5., 6.]));
        b.extend(4u64.to_le_bytes());
        b.extend([0u8; 32]);
        let w = load(&tmp("legacy.pth", &b), true).unwrap();
        assert_eq!(w.get("a.weight").unwrap(), &[1., 2., 3., 4., 5., 6.]);
    }

    #[test]
    fn a_safetensors_file_converts_half_floats() {
        let header = r#"{"x":{"dtype":"F16","shape":[3],"data_offsets":[0,6]},"__metadata__":{}}"#;
        let mut b = Vec::new();
        b.extend((header.len() as u64).to_le_bytes());
        b.extend(header.as_bytes());
        for h in [0x3c00u16, 0xc000, 0x0001] {
            b.extend(h.to_le_bytes());
        }
        let w = load(&tmp("h.safetensors", &b), true).unwrap();
        let x = w.get("x").unwrap();
        assert_eq!(&x[..2], &[1.0, -2.0]);
        assert!((x[2] - 5.960_464_5e-8).abs() < 1e-12, "smallest subnormal, got {}", x[2]);
    }

    #[test]
    fn a_safetensors_header_whose_length_starts_0x80_is_not_a_pickle() {
        // Pad the header to exactly 0x280 bytes: its length then opens with 0x80.
        let mut header = r#"{"x":{"dtype":"F32","shape":[1],"data_offsets":[0,4]}}"#.to_string();
        while header.len() < 0x280 {
            header.push(' ');
        }
        let mut b = Vec::new();
        b.extend((header.len() as u64).to_le_bytes());
        assert_eq!(b[0], 0x80);
        b.extend(header.as_bytes());
        b.extend(2.5f32.to_le_bytes());
        let w = load(&tmp("x80.safetensors", &b), true).unwrap();
        assert_eq!(w.get("x").unwrap(), &[2.5]);
    }

    #[test]
    fn module_prefixes_are_stripped() {
        let raw = vec![
            ("module.conv.weight".to_string(), Tensor { shape: vec![1], data: None }),
            ("module.conv.bias".to_string(), Tensor { shape: vec![1], data: None }),
        ];
        let m = unwrap_names(raw);
        assert!(m.contains_key("conv.weight") && m.contains_key("conv.bias"));
    }
}
