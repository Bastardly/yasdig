//! A safetensors file's f32 tensors rewritten as f16, everything else copied
//! through — the one conversion sd.cpp's `-M convert` can't do for us.
//!
//! The case that needs it is Klein's Flux.2 VAE: 250 f32 tensors and one i32
//! (BatchNorm's `num_batches_tracked`). sd.cpp's converter turns every tensor
//! into the target type, has no dequantization for an integer, and aborts
//! ("type i32 unsupported for integer quantization"); its `--tensor-type-rules`
//! only accepts float types, so the counter can't be told to stay put. An f16
//! VAE halves the file and the memory it holds. It does **not** speed the
//! decode up — measured, Klein's stayed at 9.3 s — because the decode is bound
//! by full-resolution activations, not by 84M weights; what did is
//! `--vae-conv-direct` (`Settings::vae_conv_direct`), 9.3 s → 0.9 s.
//!
//! Safetensors in, safetensors out, same tensor names: a VAE through `--vae`
//! needs no prefix, and discovery reads the result exactly like the original.
//! Written to a `.part` and renamed, so a failed run leaves no half file for the
//! scan to find.

use anyhow::{bail, Context, Result};
use std::io::{Read, Write};
use std::path::Path;

/// IEEE 754 binary32 → binary16, rounding to nearest even — what a weight
/// conversion has to be. Out of range saturates to ±inf, as numpy's does.
pub fn f32_to_f16(x: f32) -> u16 {
    let b = x.to_bits();
    let sign = ((b >> 16) & 0x8000) as u16;
    let exp = ((b >> 23) & 0xff) as i32;
    let man = b & 0x7f_ffff;
    if exp == 0xff {
        // Inf stays inf; a NaN stays a (quiet) NaN.
        return sign | 0x7c00 | if man != 0 { 0x200 } else { 0 };
    }
    let e = exp - 127 + 15;
    if e >= 0x1f {
        return sign | 0x7c00;
    }
    if e <= 0 {
        // Subnormal in f16, or zero. Shift the implicit 1 in, then round.
        if e < -10 {
            return sign;
        }
        let m = man | 0x80_0000;
        let shift = (14 - e) as u32;
        let half = 1u32 << (shift - 1);
        let rest = m & ((1 << shift) - 1);
        let mut v = m >> shift;
        if rest > half || (rest == half && v & 1 == 1) {
            v += 1;
        }
        return sign | v as u16;
    }
    let mut v = ((e as u32) << 10) | (man >> 13);
    let rest = man & 0x1fff;
    if rest > 0x1000 || (rest == 0x1000 && v & 1 == 1) {
        v += 1; // may carry into the exponent, which is correct rounding
    }
    sign | v as u16
}

/// What a conversion did, for the Convert tab's result line.
pub struct Converted {
    pub tensors: usize,
    pub halved: usize,
}

/// Rewrite `src` as `dst`, every `F32` tensor as `F16`.
pub fn safetensors_to_f16(src: &Path, dst: &Path) -> Result<Converted> {
    let mut f = std::fs::File::open(src).with_context(|| format!("opening {}", src.display()))?;
    let mut len = [0u8; 8];
    f.read_exact(&mut len).context("not a safetensors file")?;
    let len = u64::from_le_bytes(len) as usize;
    if len == 0 || len > 100 << 20 {
        bail!("{} has no safetensors header", src.display());
    }
    let mut header = vec![0u8; len];
    f.read_exact(&mut header)?;
    let header: serde_json::Map<String, serde_json::Value> =
        serde_json::from_slice(&header).context("reading the safetensors header")?;
    let mut data = Vec::new();
    f.read_to_end(&mut data)?;

    // In data order, so the output keeps the source's layout.
    let mut tensors: Vec<(String, String, Vec<u64>, usize, usize)> = Vec::new();
    let mut meta = None;
    for (name, v) in header {
        if name == "__metadata__" {
            meta = Some(v);
            continue;
        }
        let dtype = v["dtype"].as_str().context("a tensor with no dtype")?.to_string();
        let shape = v["shape"]
            .as_array()
            .context("a tensor with no shape")?
            .iter()
            .map(|d| d.as_u64().context("a non-numeric dimension"))
            .collect::<Result<Vec<_>>>()?;
        let off = v["data_offsets"].as_array().context("a tensor with no offsets")?;
        let (a, b) = (
            off.first().and_then(|x| x.as_u64()).context("bad offsets")? as usize,
            off.get(1).and_then(|x| x.as_u64()).context("bad offsets")? as usize,
        );
        if a > b || b > data.len() {
            bail!("{name}'s data runs past the end of the file");
        }
        tensors.push((name, dtype, shape, a, b));
    }
    tensors.sort_by_key(|t| t.3);

    let mut out_header = serde_json::Map::new();
    if let Some(m) = meta {
        out_header.insert("__metadata__".into(), m);
    }
    let mut body: Vec<u8> = Vec::with_capacity(data.len() / 2 + 1024);
    let mut halved = 0;
    for (name, dtype, shape, a, b) in &tensors {
        let bytes = &data[*a..*b];
        let start = body.len();
        let dtype = if dtype == "F32" {
            if bytes.len() % 4 != 0 {
                bail!("{name} is F32 but {} bytes long", bytes.len());
            }
            for c in bytes.chunks_exact(4) {
                let h = f32_to_f16(f32::from_le_bytes([c[0], c[1], c[2], c[3]]));
                body.extend_from_slice(&h.to_le_bytes());
            }
            halved += 1;
            "F16".to_string()
        } else {
            body.extend_from_slice(bytes);
            dtype.clone()
        };
        out_header.insert(
            name.clone(),
            serde_json::json!({ "dtype": dtype, "shape": shape, "data_offsets": [start, body.len()] }),
        );
    }

    // The header is padded with spaces to a multiple of 8, as the format's
    // reference writer does, so the data that follows starts aligned.
    let mut h = serde_json::to_vec(&serde_json::Value::Object(out_header))?;
    while h.len() % 8 != 0 {
        h.push(b' ');
    }
    let mut part = dst.as_os_str().to_owned();
    part.push(".part");
    let part = std::path::PathBuf::from(part);
    let write = || -> Result<()> {
        let mut w = std::io::BufWriter::new(std::fs::File::create(&part)?);
        w.write_all(&(h.len() as u64).to_le_bytes())?;
        w.write_all(&h)?;
        w.write_all(&body)?;
        w.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        Ok(())
    };
    if let Err(e) = write() {
        let _ = std::fs::remove_file(&part);
        return Err(e.context(format!("writing {}", dst.display())));
    }
    std::fs::rename(&part, dst).with_context(|| format!("naming {}", dst.display()))?;
    Ok(Converted { tensors: tensors.len(), halved })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The values a weight conversion has to get right: exact ones, the
    /// rounding tie, the range edges, subnormals and the specials.
    #[test]
    fn f32_to_f16_rounds_to_nearest_even() {
        assert_eq!(f32_to_f16(0.0), 0x0000);
        assert_eq!(f32_to_f16(-0.0), 0x8000);
        assert_eq!(f32_to_f16(1.0), 0x3c00);
        assert_eq!(f32_to_f16(-2.0), 0xc000);
        assert_eq!(f32_to_f16(0.5), 0x3800);
        assert_eq!(f32_to_f16(65504.0), 0x7bff);
        assert_eq!(f32_to_f16(70000.0), 0x7c00, "past the range is inf");
        assert_eq!(f32_to_f16(f32::INFINITY), 0x7c00);
        assert_eq!(f32_to_f16(f32::NAN) & 0x7c00, 0x7c00);
        assert_ne!(f32_to_f16(f32::NAN) & 0x3ff, 0);
        // 1 + 2^-11 is exactly between 1 and the next f16: ties go to even (1).
        assert_eq!(f32_to_f16(1.0 + 2f32.powi(-11)), 0x3c00);
        // 1 + 3·2^-11 is between two odd/even neighbours: up to the even one.
        assert_eq!(f32_to_f16(1.0 + 3.0 * 2f32.powi(-11)), 0x3c02);
        // The smallest f16 subnormal, and half of it rounding to zero (even).
        assert_eq!(f32_to_f16(2f32.powi(-24)), 0x0001);
        assert_eq!(f32_to_f16(2f32.powi(-25)), 0x0000);
        assert_eq!(f32_to_f16(6.0e-5), 0x03ef);
    }

    /// A VAE-shaped file — f32 weights beside an i32 counter — comes back with
    /// the weights halved, the counter untouched, and offsets that add up.
    #[test]
    fn f32_tensors_halve_and_the_rest_copies_through() {
        let dir = std::env::temp_dir().join(format!("rustimage-half-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("vae.safetensors");
        let w: Vec<f32> = vec![1.0, -0.5, 3.25, 65504.0];
        let n: i32 = 1234;
        let mut data = Vec::new();
        for v in &w {
            data.extend_from_slice(&v.to_le_bytes());
        }
        data.extend_from_slice(&n.to_le_bytes());
        let header = serde_json::json!({
            "__metadata__": { "format": "pt" },
            "decoder.conv_in.weight": { "dtype": "F32", "shape": [2, 2], "data_offsets": [0, 16] },
            "bn.num_batches_tracked": { "dtype": "I32", "shape": [], "data_offsets": [16, 20] },
        });
        let h = serde_json::to_vec(&header).unwrap();
        let mut file = (h.len() as u64).to_le_bytes().to_vec();
        file.extend_from_slice(&h);
        file.extend_from_slice(&data);
        std::fs::write(&src, file).unwrap();

        let dst = dir.join("vae-f16.safetensors");
        let r = safetensors_to_f16(&src, &dst).unwrap();
        assert_eq!((r.tensors, r.halved), (2, 1));
        assert!(!dir.join("vae-f16.safetensors.part").exists());

        let out = std::fs::read(&dst).unwrap();
        let hl = u64::from_le_bytes(out[..8].try_into().unwrap()) as usize;
        assert_eq!(hl % 8, 0, "the data starts aligned");
        let head: serde_json::Value = serde_json::from_slice(&out[8..8 + hl]).unwrap();
        let body = &out[8 + hl..];
        assert_eq!(head["__metadata__"]["format"], "pt");
        let wt = &head["decoder.conv_in.weight"];
        assert_eq!(wt["dtype"], "F16");
        assert_eq!(wt["shape"], serde_json::json!([2, 2]));
        let (a, b) = (wt["data_offsets"][0].as_u64().unwrap() as usize, wt["data_offsets"][1].as_u64().unwrap() as usize);
        let halves: Vec<u16> = body[a..b].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        assert_eq!(halves, [0x3c00, 0xb800, 0x4280, 0x7bff]);
        let bn = &head["bn.num_batches_tracked"];
        assert_eq!(bn["dtype"], "I32");
        let (a, b) = (bn["data_offsets"][0].as_u64().unwrap() as usize, bn["data_offsets"][1].as_u64().unwrap() as usize);
        assert_eq!(i32::from_le_bytes(body[a..b].try_into().unwrap()), 1234);
        assert_eq!(body.len(), 12, "8 bytes of f16 and 4 of i32, nothing else");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
