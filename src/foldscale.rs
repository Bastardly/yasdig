//! Fold a scaled-fp8 checkpoint's per-tensor scales into its weights, in place.
//!
//! ComfyUI's "scaled fp8" format stores each Linear's weight as raw e4m3
//! numbers (up to ±448) beside a `X.scale_weight` that multiplies the *output*.
//! sd.cpp's `-M convert` copies that layout straight into a GGUF — the scale is
//! one element, so it is under every block size and `tensor_should_be_converted`
//! passes it through — and `Linear::forward` (`ggml_block.hpp:273`) runs the
//! matmul first and applies the scale after it. Measured on a Wan 2.2 A14B
//! converted to q4_K: 400 pairs, scales from 9.2e-5 to 4.1e-3, so every matmul
//! accumulates 250–11 000× its real magnitude. ggml-vulkan accumulates in fp16
//! by default (max 65 504), and the clip came back as a mesh on frame 0 and a
//! mosaic by frame 32. Community GGUFs don't hit it because they are quantized
//! from bf16, where the scale was never separate.
//!
//! A per-tensor (or per-row) scale is a linear factor, and every ggml block
//! format keeps its dequantisation multipliers as fp16 fields — `d`, and `dmin`
//! or `m` where there is one. So folding is a rewrite of those fields and the
//! scale set to 1.0: the same bytes in the same places, no re-quantisation, and
//! no second 8 GB file. Quantising is scale-equivariant, so the result matches
//! quantising the folded weights up to the fp16 rounding of `d`.
//!
//! **A pair is folded under a journal.** Weight and scale are two writes, and a
//! crash between them leaves either a folded weight with its old scale (folded
//! twice by the next run) or a scale of 1 over a raw weight (never folded) —
//! both of which load and render garbage with nothing saying why. So the
//! original bytes of both go to `<file>.fold-journal` first, through a `.part`
//! and a rename so a journal that exists is complete, and the next run restores
//! them before doing anything else.

use anyhow::{bail, Context, Result};
use std::fs::{File, OpenOptions};
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::discover::{read_gguf_string, read_u32, read_u64, skip_gguf_value};

const F32: u32 = 0;
const F16: u32 = 1;
const BF16: u32 = 30;
const JOURNAL_MAGIC: &[u8; 4] = b"RIFJ";

/// One tensor as the GGUF index states it.
struct Entry {
    name: String,
    ty: u32,
    /// `ne`, ggml order: `dims[0]` is the row length.
    dims: Vec<u64>,
    /// Absolute, data-section start included.
    offset: u64,
}

impl Entry {
    fn elements(&self) -> u64 {
        self.dims.iter().product()
    }

    fn rows(&self) -> u64 {
        self.elements() / self.dims.first().copied().unwrap_or(1).max(1)
    }

    fn bytes(&self) -> Option<u64> {
        let (per, elems) = match layout(self.ty)? {
            Layout::Plain(w) => (w, 1),
            Layout::Blocks { bytes, elems, .. } => (bytes, elems),
        };
        self.elements().is_multiple_of(elems).then(|| self.elements() / elems * per)
    }
}

/// How a type stores its numbers, as far as scaling them is concerned.
enum Layout {
    /// One float per element, this many bytes wide.
    Plain(u64),
    /// Blocks of `elems` values in `bytes` bytes, whose fp16 multipliers sit at
    /// the byte offsets in `fields`. From ggml's `block_*` structs.
    Blocks {
        bytes: u64,
        elems: u64,
        fields: &'static [usize],
    },
}

fn layout(ty: u32) -> Option<Layout> {
    let b = |bytes, elems, fields| Some(Layout::Blocks { bytes, elems, fields });
    match ty {
        F32 => Some(Layout::Plain(4)),
        F16 | BF16 => Some(Layout::Plain(2)),
        2 => b(18, 32, &[0]),         // q4_0: d, qs[16]
        3 => b(20, 32, &[0, 2]),      // q4_1: d, m, qs[16]
        6 => b(22, 32, &[0]),         // q5_0: d, qh[4], qs[16]
        7 => b(24, 32, &[0, 2]),      // q5_1: d, m, qh[4], qs[16]
        8 => b(34, 32, &[0]),         // q8_0: d, qs[32]
        10 => b(84, 256, &[80, 82]),  // q2_K: scales[16], qs[64], d, dmin
        11 => b(110, 256, &[108]),    // q3_K: hmask[32], qs[64], scales[12], d
        12 => b(144, 256, &[0, 2]),   // q4_K: d, dmin, scales[12], qs[128]
        13 => b(176, 256, &[0, 2]),   // q5_K: d, dmin, scales[12], qh[32], qs[128]
        14 => b(210, 256, &[208]),    // q6_K: ql[128], qh[64], scales[16], d
        _ => None,
    }
}

/// What a fold did.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct FoldReport {
    /// Pairs whose scale was folded into the weight on this run.
    pub folded: usize,
    /// Pairs whose scale was already 1.
    pub already: usize,
    /// Pairs left alone, each with why. A weight type with no known layout
    /// (sd.cpp's int8 tensorwise, say) is the expected case; such a file is
    /// not made worse, only not made better.
    pub skipped: Vec<String>,
    /// An interrupted earlier run was undone before this one started.
    pub recovered: bool,
}

/// A scale tensor and the weight it multiplies.
struct Pair<'a> {
    scale: &'a Entry,
    weight: Option<&'a Entry>,
}

struct Index {
    entries: Vec<Entry>,
}

impl Index {
    fn read(path: &Path) -> Result<Index> {
        let f = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let mut r = BufReader::new(f);
        let mut magic = [0u8; 4];
        r.read_exact(&mut magic)?;
        if &magic != b"GGUF" {
            bail!("{} is not a GGUF file", path.display());
        }
        let version = read_u32(&mut r)?;
        if !(2..=3).contains(&version) {
            bail!("GGUF version {version} is not supported (expected 2 or 3)");
        }
        let tensor_count = read_u64(&mut r)?;
        let kv_count = read_u64(&mut r)?;
        if tensor_count > 1_000_000 || kv_count > 1_000_000 {
            bail!("implausible GGUF header: {tensor_count} tensors, {kv_count} metadata keys");
        }
        let mut alignment = 32u64;
        for _ in 0..kv_count {
            let key = read_gguf_string(&mut r)?;
            let ty = read_u32(&mut r)?;
            // The one key that moves the data section. uint32 per the spec.
            if key == "general.alignment" && ty == 4 {
                alignment = read_u32(&mut r)?.max(1) as u64;
            } else {
                skip_gguf_value(&mut r, ty)?;
            }
        }
        let mut entries = Vec::with_capacity(tensor_count as usize);
        for _ in 0..tensor_count {
            let name = read_gguf_string(&mut r)?;
            let n_dims = read_u32(&mut r)?;
            if n_dims > 4 {
                bail!("tensor '{name}' claims {n_dims} dimensions; GGUF allows at most 4");
            }
            let dims = (0..n_dims).map(|_| read_u64(&mut r)).collect::<Result<Vec<_>>>()?;
            let ty = read_u32(&mut r)?;
            let offset = read_u64(&mut r)?;
            entries.push(Entry { name, ty, dims, offset });
        }
        let end = r.stream_position()?;
        let data = end.div_ceil(alignment) * alignment;
        for e in &mut entries {
            e.offset += data;
        }
        Ok(Index { entries })
    }

    fn find(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.name == name)
    }

    /// Every scale tensor, with its weight. sd.cpp renames `.scale_weight` to
    /// `.weight_scale` at load (`name_conversion.cpp:1575`), so a file may
    /// carry either spelling.
    fn pairs(&self) -> Vec<Pair<'_>> {
        self.entries
            .iter()
            .filter_map(|e| {
                let stem = e
                    .name
                    .strip_suffix(".scale_weight")
                    .or_else(|| e.name.strip_suffix(".weight_scale"))?;
                Some(Pair {
                    scale: e,
                    weight: self.find(&format!("{stem}.weight")),
                })
            })
            .collect()
    }
}

fn journal_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".fold-journal");
    PathBuf::from(s)
}

/// How many scales in `path` are not yet 1, or an interrupted fold. For
/// `doctor`, which only reports; reads the index and one scalar per pair.
pub fn pending(path: &Path) -> Result<usize> {
    if journal_path(path).exists() {
        return Ok(1);
    }
    let index = Index::read(path)?;
    let mut f = File::open(path)?;
    let mut n = 0;
    for p in index.pairs() {
        if read_floats(&mut f, p.scale)?.iter().any(|&s| s != 1.0) {
            n += 1;
        }
    }
    Ok(n)
}

/// Fold every scale in `path` into its weight. Idempotent: a scale of 1 is
/// left as it is, so a second run reports everything as already done.
pub fn fold(path: &Path) -> Result<FoldReport> {
    let mut report = FoldReport {
        recovered: recover(path)?,
        ..FoldReport::default()
    };
    let index = Index::read(path)?;
    let mut f = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("opening {} to fold its scales", path.display()))?;

    for p in index.pairs() {
        let scale = p.scale;
        let scales = read_floats(&mut f, scale)?;
        if scales.iter().all(|&s| s == 1.0) {
            report.already += 1;
            continue;
        }
        let Some(weight) = p.weight else {
            report.skipped.push(format!("{}: no weight beside it", scale.name));
            continue;
        };
        let rows = weight.rows();
        if scales.len() as u64 != 1 && scales.len() as u64 != rows {
            report.skipped.push(format!(
                "{}: {} scales for {rows} rows",
                scale.name,
                scales.len()
            ));
            continue;
        }
        if !scales.iter().all(|s| s.is_finite()) {
            report.skipped.push(format!("{}: not a finite number", scale.name));
            continue;
        }
        let (Some(lay), Some(len)) = (layout(weight.ty), weight.bytes()) else {
            report.skipped.push(format!(
                "{}: type {} has no layout this knows",
                weight.name, weight.ty
            ));
            continue;
        };
        let original = read_at(&mut f, weight.offset, len)?;
        let mut folded = original.clone();
        let row_len = weight.dims.first().copied().unwrap_or(1).max(1);
        let s_of = |element: u64| scales[if scales.len() == 1 { 0 } else { (element / row_len) as usize }];
        match lay {
            Layout::Plain(_) => {
                for (i, mut v) in floats_mut(&mut folded, weight.ty).enumerate() {
                    v.scale(s_of(i as u64));
                }
            }
            Layout::Blocks { bytes, elems, fields } => {
                for (b, block) in folded.chunks_exact_mut(bytes as usize).enumerate() {
                    let s = s_of(b as u64 * elems);
                    for &at in fields {
                        let h = u16::from_le_bytes([block[at], block[at + 1]]);
                        block[at..at + 2].copy_from_slice(&f32_to_f16(f16_to_f32(h) * s).to_le_bytes());
                    }
                }
            }
        }
        // Checked before anything is written: a weight that overflows fp16 is
        // a pair to leave alone, not half a fold.
        if !all_finite(&folded, &lay, weight.ty) {
            report.skipped.push(format!("{}: folding would overflow", weight.name));
            continue;
        }
        let scale_len = scale.bytes().context("a scale has a plain type")?;
        let scale_original = read_at(&mut f, scale.offset, scale_len)?;
        let ones = one_bytes(scale.ty, scales.len());

        write_journal(path, &[(weight.offset, &original), (scale.offset, &scale_original)])?;
        write_at(&mut f, weight.offset, &folded)?;
        write_at(&mut f, scale.offset, &ones)?;
        f.sync_data()?;
        std::fs::remove_file(journal_path(path))?;
        report.folded += 1;
    }
    Ok(report)
}

/// Put back what an interrupted fold had started on, so the pair it was on is
/// whole again before this run looks at it.
fn recover(path: &Path) -> Result<bool> {
    let _ = std::fs::remove_file(journal_part(path));
    let jp = journal_path(path);
    let Ok(bytes) = std::fs::read(&jp) else {
        return Ok(false);
    };
    if bytes.get(..4) != Some(JOURNAL_MAGIC.as_slice()) {
        bail!("{} is not a fold journal; move it aside and retry", jp.display());
    }
    let mut f = OpenOptions::new().write(true).open(path)?;
    let mut at = 4usize;
    let mut take = |n: usize| -> Result<&[u8]> {
        let s = bytes.get(at..at + n).context("the fold journal is truncated")?;
        at += n;
        Ok(s)
    };
    let count = u64::from_le_bytes(take(8)?.try_into()?);
    for _ in 0..count {
        let off = u64::from_le_bytes(take(8)?.try_into()?);
        let len = u64::from_le_bytes(take(8)?.try_into()?) as usize;
        write_at(&mut f, off, take(len)?)?;
    }
    f.sync_data()?;
    std::fs::remove_file(&jp)?;
    Ok(true)
}

fn journal_part(path: &Path) -> PathBuf {
    let mut s = journal_path(path).into_os_string();
    s.push(".part");
    PathBuf::from(s)
}

fn write_journal(path: &Path, regions: &[(u64, &[u8])]) -> Result<()> {
    let part = journal_part(path);
    let mut j = std::io::BufWriter::new(File::create(&part)?);
    j.write_all(JOURNAL_MAGIC)?;
    j.write_all(&(regions.len() as u64).to_le_bytes())?;
    for (off, data) in regions {
        j.write_all(&off.to_le_bytes())?;
        j.write_all(&(data.len() as u64).to_le_bytes())?;
        j.write_all(data)?;
    }
    j.into_inner().map_err(|e| e.into_error())?.sync_all()?;
    std::fs::rename(&part, journal_path(path))?;
    Ok(())
}

fn read_at(f: &mut File, off: u64, len: u64) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; len as usize];
    f.seek(SeekFrom::Start(off))?;
    f.read_exact(&mut buf)?;
    Ok(buf)
}

fn write_at(f: &mut File, off: u64, data: &[u8]) -> Result<()> {
    f.seek(SeekFrom::Start(off))?;
    f.write_all(data)?;
    Ok(())
}

/// A scale tensor's values, or an empty list for a type that isn't a float.
fn read_floats(f: &mut File, e: &Entry) -> Result<Vec<f32>> {
    let width = match e.ty {
        F32 => 4,
        F16 | BF16 => 2,
        _ => return Ok(Vec::new()),
    };
    let raw = read_at(f, e.offset, e.elements() * width)?;
    Ok(raw
        .chunks_exact(width as usize)
        .map(|c| match e.ty {
            F32 => f32::from_le_bytes(c.try_into().unwrap()),
            F16 => f16_to_f32(u16::from_le_bytes([c[0], c[1]])),
            _ => f32::from_bits((u16::from_le_bytes([c[0], c[1]]) as u32) << 16),
        })
        .collect())
}

fn one_bytes(ty: u32, n: usize) -> Vec<u8> {
    let one: &[u8] = match ty {
        F32 => &[0x00, 0x00, 0x80, 0x3f],
        F16 => &[0x00, 0x3c],
        _ => &[0x80, 0x3f],
    };
    one.repeat(n)
}

/// One element of a plain-typed tensor, scaled where it lies.
enum Elem<'a> {
    F32(&'a mut [u8]),
    F16(&'a mut [u8]),
    Bf16(&'a mut [u8]),
}

impl Elem<'_> {
    fn get(&self) -> f32 {
        match self {
            Elem::F32(b) => f32::from_le_bytes((&**b).try_into().unwrap()),
            Elem::F16(b) => f16_to_f32(u16::from_le_bytes([b[0], b[1]])),
            Elem::Bf16(b) => f32::from_bits((u16::from_le_bytes([b[0], b[1]]) as u32) << 16),
        }
    }

    fn scale(&mut self, s: f32) {
        let v = self.get() * s;
        match self {
            Elem::F32(b) => b.copy_from_slice(&v.to_le_bytes()),
            Elem::F16(b) => b.copy_from_slice(&f32_to_f16(v).to_le_bytes()),
            Elem::Bf16(b) => b.copy_from_slice(&f32_to_bf16(v).to_le_bytes()),
        }
    }
}

fn floats_mut(buf: &mut [u8], ty: u32) -> impl Iterator<Item = Elem<'_>> {
    let width = if ty == F32 { 4 } else { 2 };
    buf.chunks_exact_mut(width).map(move |c| match ty {
        F32 => Elem::F32(c),
        F16 => Elem::F16(c),
        _ => Elem::Bf16(c),
    })
}

fn all_finite(buf: &[u8], lay: &Layout, ty: u32) -> bool {
    match lay {
        Layout::Plain(_) => floats_mut(&mut buf.to_vec(), ty).all(|e| e.get().is_finite()),
        Layout::Blocks { bytes, fields, .. } => buf.chunks_exact(*bytes as usize).all(|b| {
            fields
                .iter()
                .all(|&at| f16_to_f32(u16::from_le_bytes([b[at], b[at + 1]])).is_finite())
        }),
    }
}

use crate::sr::weights::f16_to_f32;

/// IEEE single to half, round to nearest even, subnormals included. A carry
/// out of the mantissa rolls into the exponent, which is what rounding is.
fn f32_to_f16(x: f32) -> u16 {
    let b = x.to_bits();
    let sign = ((b >> 16) & 0x8000) as u16;
    let exp = ((b >> 23) & 0xff) as i32;
    let man = b & 0x7f_ffff;
    if exp == 0xff {
        return sign | 0x7c00 | if man != 0 { 0x200 } else { 0 };
    }
    let e = exp - 127 + 15;
    if e >= 0x1f {
        return sign | 0x7c00;
    }
    let round = |v: u32, shift: u32| {
        let kept = v >> shift;
        let rem = v & ((1 << shift) - 1);
        let mid = 1 << (shift - 1);
        if rem > mid || (rem == mid && kept & 1 == 1) {
            kept + 1
        } else {
            kept
        }
    };
    if e <= 0 {
        if e < -10 {
            return sign;
        }
        return sign | round(man | 0x80_0000, (14 - e) as u32) as u16;
    }
    sign | round(((e as u32) << 23) | man, 13) as u16
}

fn f32_to_bf16(x: f32) -> u16 {
    let b = x.to_bits();
    ((b + 0x7fff + ((b >> 16) & 1)) >> 16) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A GGUF with the given tensors, each `(name, ty, dims, data)`, laid out
    /// at 32-byte alignment the way sd.cpp writes one.
    fn gguf(dir: &Path, tensors: &[(&str, u32, &[u64], Vec<u8>)]) -> PathBuf {
        let mut h = Vec::new();
        h.extend_from_slice(b"GGUF");
        h.extend_from_slice(&3u32.to_le_bytes());
        h.extend_from_slice(&(tensors.len() as u64).to_le_bytes());
        h.extend_from_slice(&0u64.to_le_bytes());
        let mut data = Vec::new();
        for (name, ty, dims, bytes) in tensors {
            h.extend_from_slice(&(name.len() as u64).to_le_bytes());
            h.extend_from_slice(name.as_bytes());
            h.extend_from_slice(&(dims.len() as u32).to_le_bytes());
            for d in *dims {
                h.extend_from_slice(&d.to_le_bytes());
            }
            h.extend_from_slice(&ty.to_le_bytes());
            h.extend_from_slice(&(data.len() as u64).to_le_bytes());
            data.extend_from_slice(bytes);
            data.resize(data.len().div_ceil(32) * 32, 0);
        }
        h.resize(h.len().div_ceil(32) * 32, 0);
        h.extend_from_slice(&data);
        let p = dir.join("m.gguf");
        std::fs::write(&p, h).unwrap();
        p
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rustimage-fold-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn h(v: f32) -> [u8; 2] {
        f32_to_f16(v).to_le_bytes()
    }

    /// ggml's `dequantize_row_q8_0`.
    fn deq_q8_0(b: &[u8]) -> Vec<f32> {
        b.chunks_exact(34)
            .flat_map(|blk| {
                let d = f16_to_f32(u16::from_le_bytes([blk[0], blk[1]]));
                blk[2..].iter().map(move |&q| d * q as i8 as f32)
            })
            .collect()
    }

    /// ggml's `get_scale_min_k4`.
    fn scale_min_k4(j: usize, q: &[u8]) -> (u8, u8) {
        if j < 4 {
            (q[j] & 63, q[j + 4] & 63)
        } else {
            (
                (q[j + 4] & 0xF) | ((q[j - 4] >> 6) << 4),
                (q[j + 4] >> 4) | ((q[j] >> 6) << 4),
            )
        }
    }

    /// ggml's `dequantize_row_q4_K`.
    fn deq_q4_k(b: &[u8]) -> Vec<f32> {
        let mut out = Vec::new();
        for blk in b.chunks_exact(144) {
            let d = f16_to_f32(u16::from_le_bytes([blk[0], blk[1]]));
            let min = f16_to_f32(u16::from_le_bytes([blk[2], blk[3]]));
            let scales = &blk[4..16];
            let qs = &blk[16..];
            for (j, q) in qs.chunks_exact(32).enumerate() {
                let (sc, m) = scale_min_k4(2 * j, scales);
                let (d1, m1) = (d * sc as f32, min * m as f32);
                let (sc, m) = scale_min_k4(2 * j + 1, scales);
                let (d2, m2) = (d * sc as f32, min * m as f32);
                out.extend(q.iter().map(|&x| d1 * (x & 0xF) as f32 - m1));
                out.extend(q.iter().map(|&x| d2 * (x >> 4) as f32 - m2));
            }
        }
        out
    }

    fn q8_block(d: f32, seed: u8) -> Vec<u8> {
        let mut b = h(d).to_vec();
        b.extend((0..32u8).map(|i| i.wrapping_mul(37).wrapping_add(seed)));
        b
    }

    fn q4k_block(d: f32, dmin: f32, seed: u8) -> Vec<u8> {
        let mut b = h(d).to_vec();
        b.extend(h(dmin));
        b.extend((0..140u8).map(|i| i.wrapping_mul(53).wrapping_add(seed)));
        b
    }

    fn close(a: &[f32], b: &[f32], s: f32) {
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(b) {
            // fp16 `d` carries 11 bits, so one rounding is ~5e-4 relative.
            assert!((x * s - y).abs() <= 1e-3 * (x * s).abs() + 1e-7, "{x}·{s} vs {y}");
        }
    }

    #[test]
    fn a_scaled_fp8_file_folds_into_its_weights() {
        let dir = tmp("fold");
        let (sq, sk, sf) = (0.00176f32, 0.0003f32, 0.004f32);
        let q8: Vec<u8> = [q8_block(12.0, 1), q8_block(3.5, 9)].concat();
        let q4k: Vec<u8> = [q4k_block(30.0, 2.0, 3), q4k_block(0.75, 0.125, 7)].concat();
        let f16w: Vec<u8> = [1.0f32, -448.0, 0.5, 96.0].iter().flat_map(|&v| h(v)).collect();
        let bf = |v: f32| f32_to_bf16(v).to_le_bytes().to_vec();
        let p = gguf(
            &dir,
            &[
                ("blocks.0.q.weight", 8, &[32, 2], q8.clone()),
                ("blocks.0.q.scale_weight", BF16, &[1], bf(sq)),
                ("blocks.0.ffn.weight", 12, &[256, 2], q4k.clone()),
                ("blocks.0.ffn.scale_weight", F32, &[1], sk.to_le_bytes().to_vec()),
                ("head.weight", F16, &[2, 2], f16w.clone()),
                ("head.weight_scale", F16, &[1], h(sf).to_vec()),
                ("head.bias", F32, &[2], vec![0; 8]),
            ],
        );
        assert_eq!(pending(&p).unwrap(), 3);

        let r = fold(&p).unwrap();
        assert_eq!((r.folded, r.already, r.skipped.len()), (3, 0, 0), "{r:?}");
        assert_eq!(pending(&p).unwrap(), 0);
        assert!(!journal_path(&p).exists());

        let idx = Index::read(&p).unwrap();
        let mut f = File::open(&p).unwrap();
        let get = |f: &mut File, n: &str| {
            let e = idx.find(n).unwrap();
            read_at(f, e.offset, e.bytes().unwrap()).unwrap()
        };
        // The scale the file stored is what folds, so compare against it.
        let sq = f32::from_bits((f32_to_bf16(sq) as u32) << 16);
        let sf = f16_to_f32(f32_to_f16(sf));
        close(&deq_q8_0(&q8), &deq_q8_0(&get(&mut f, "blocks.0.q.weight")), sq);
        close(&deq_q4_k(&q4k), &deq_q4_k(&get(&mut f, "blocks.0.ffn.weight")), sk);
        let as_f = |b: Vec<u8>| -> Vec<f32> {
            b.chunks_exact(2).map(|c| f16_to_f32(u16::from_le_bytes([c[0], c[1]]))).collect()
        };
        close(&as_f(f16w), &as_f(get(&mut f, "head.weight")), sf);
        for n in ["blocks.0.q.scale_weight", "blocks.0.ffn.scale_weight", "head.weight_scale"] {
            assert_eq!(read_floats(&mut f, idx.find(n).unwrap()).unwrap(), vec![1.0], "{n}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_second_fold_changes_nothing() {
        let dir = tmp("twice");
        let p = gguf(
            &dir,
            &[
                ("a.weight", 8, &[32, 1], q8_block(9.0, 4)),
                ("a.scale_weight", F32, &[1], 0.01f32.to_le_bytes().to_vec()),
            ],
        );
        fold(&p).unwrap();
        let once = std::fs::read(&p).unwrap();
        let r = fold(&p).unwrap();
        assert_eq!((r.folded, r.already), (0, 1));
        assert_eq!(std::fs::read(&p).unwrap(), once);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_interrupted_fold_is_undone_before_the_next_one() {
        let dir = tmp("journal");
        let p = gguf(
            &dir,
            &[
                ("a.weight", 8, &[32, 1], q8_block(9.0, 4)),
                ("a.scale_weight", F32, &[1], 0.01f32.to_le_bytes().to_vec()),
            ],
        );
        let pristine = std::fs::read(&p).unwrap();
        fold(&p).unwrap();
        let folded = std::fs::read(&p).unwrap();

        // Crash after the journal and both writes, before the journal went:
        // the file is folded but the journal says to put it back.
        std::fs::write(&p, &pristine).unwrap();
        let idx = Index::read(&p).unwrap();
        let (w, s) = (idx.find("a.weight").unwrap(), idx.find("a.scale_weight").unwrap());
        write_journal(
            &p,
            &[
                (w.offset, &pristine[w.offset as usize..][..34]),
                (s.offset, &pristine[s.offset as usize..][..4]),
            ],
        )
        .unwrap();
        std::fs::write(&p, &folded).unwrap();
        assert_eq!(pending(&p).unwrap(), 1, "a journal is pending work");

        let r = fold(&p).unwrap();
        assert!(r.recovered);
        assert_eq!(r.folded, 1, "restored, then folded once — not twice");
        assert_eq!(std::fs::read(&p).unwrap(), folded);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unknown_weight_type_is_left_whole() {
        let dir = tmp("i8");
        let p = gguf(
            &dir,
            &[
                ("a.weight", 24, &[4, 1], vec![1, 2, 3, 4]),
                ("a.weight_scale", F32, &[1], 0.5f32.to_le_bytes().to_vec()),
            ],
        );
        let before = std::fs::read(&p).unwrap();
        let r = fold(&p).unwrap();
        assert_eq!((r.folded, r.skipped.len()), (0, 1));
        assert_eq!(std::fs::read(&p).unwrap(), before, "neither half of the pair moved");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn half_precision_rounds_to_nearest_even() {
        for v in [0.0f32, 1.0, -2.5, 65504.0, 6.1035156e-5, 5.9604645e-8, 0.0009765625, 448.0] {
            assert_eq!(f16_to_f32(f32_to_f16(v)), v, "{v}");
        }
        assert_eq!(f32_to_f16(1e6), 0x7c00, "past the range is infinity");
        assert_eq!(f32_to_f16(1e-9), 0, "below half the smallest subnormal is zero");
        // 1 + 2^-11 is exactly between 1 and the next half; even is 1.
        assert_eq!(f32_to_f16(1.0 + 1.0 / 2048.0), 0x3c00);
        assert_eq!(f32_to_f16(1.0 + 3.0 / 2048.0), 0x3c02);
    }
}
