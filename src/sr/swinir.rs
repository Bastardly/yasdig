//! SwinIR (Liang et al. 2021, `network_swinir.py`), inference only.
//!
//! The reference's shapes are [B, H·W, C] tokens between the convs, which is
//! already this module's channels-last layout — so `patch_embed` and
//! `patch_unembed` are nothing, and a Swin block is five ops: LayerNorm, qkv,
//! window attention, proj (+ shortcut), then LayerNorm, fc1+GELU, fc2
//! (+ shortcut). The window partition, the cyclic shift and its mask all live
//! in `attn.wgsl` as index maths.
//!
//! Three upsampling tails exist and all three are here: `pixelshuffle`
//! (classical SR), `pixelshuffledirect` (lightweight) and `nearest+conv`
//! (real-world SR, the GAN releases). The last is what most people mean by "a
//! SwinIR upscaler".

use anyhow::{bail, Result};

use super::arch::{SwinIR, SwinUp};
use super::gpu::{Act, Gpu, Wt, T};
use super::layer::{Conv, Norm};
use super::weights::Weights;

struct Block {
    norm1: Norm,
    qkv: Conv,
    /// Dense relative-position bias, [heads][ws²][ws²].
    bias: Wt,
    proj: Conv,
    norm2: Norm,
    fc1: Conv,
    fc2: Conv,
    heads: u32,
    shift: u32,
}

/// A group's closing conv(s): one 3×3, or `3conv`'s bottleneck.
enum Tail {
    One(Conv),
    Three(Conv, Conv, Conv),
}

impl Tail {
    fn load(g: &Gpu, w: &Weights, prefix: &str, three: bool) -> Result<Tail> {
        Ok(if three {
            Tail::Three(
                Conv::load(g, w, &format!("{prefix}.0"))?,
                Conv::load(g, w, &format!("{prefix}.2"))?,
                Conv::load(g, w, &format!("{prefix}.4"))?,
            )
        } else {
            Tail::One(Conv::load(g, w, prefix)?)
        })
    }

    /// `conv(x) + res`.
    fn run_res(&self, g: &Gpu, x: &T, res: &T) -> T {
        match self {
            Tail::One(c) => c.run_res(g, x, Act::None, res),
            Tail::Three(a, b, c) => {
                let t = a.run(g, x, Act::Leaky(0.2));
                let u = b.run(g, &t, Act::Leaky(0.2));
                g.recycle(t);
                let y = c.run_res(g, &u, Act::None, res);
                g.recycle(u);
                y
            }
        }
    }
}

// One per model; see `Net` in `mod.rs`.
#[allow(clippy::large_enum_variant)]
enum Up {
    PixelShuffle { before: Conv, stages: Vec<(Conv, u32)>, last: Conv },
    Direct { conv: Conv, scale: u32 },
    NearestConv { before: Conv, up1: Conv, up2: Option<Conv>, hr: Conv, last: Conv },
}

pub struct Net {
    pub cfg: SwinIR,
    conv_first: Conv,
    patch_norm: Option<Norm>,
    groups: Vec<(Vec<Block>, Tail)>,
    norm: Norm,
    after_body: Tail,
    up: Up,
}

/// The reference's `relative_position_index` expanded against the table:
/// bias[h][i][j] = table[(Δy + ws−1)·(2ws−1) + (Δx + ws−1)][h], Δ = pos(i) − pos(j).
pub fn dense_bias(table: &[f32], heads: usize, ws: usize) -> Vec<f32> {
    let n = ws * ws;
    let side = 2 * ws - 1;
    let mut out = vec![0.0; heads * n * n];
    for i in 0..n {
        let (yi, xi) = (i / ws, i % ws);
        for j in 0..n {
            let (yj, xj) = (j / ws, j % ws);
            let idx = (yi + ws - 1 - yj) * side + (xi + ws - 1 - xj);
            for h in 0..heads {
                out[(h * n + i) * n + j] = table[idx * heads + h];
            }
        }
    }
    out
}

impl Net {
    pub fn load(g: &Gpu, w: &Weights, cfg: &SwinIR) -> Result<Net> {
        let e = cfg.embed;
        let ws = cfg.window;
        let mut groups = Vec::new();
        for (l, (&depth, &heads)) in cfg.depths.iter().zip(&cfg.heads).enumerate() {
            if e / heads > 64 || !e.is_multiple_of(heads) {
                bail!("SwinIR head width {} is not supported (max 64)", e / heads);
            }
            let mut blocks = Vec::new();
            for b in 0..depth {
                let p = format!("layers.{l}.residual_group.blocks.{b}");
                let table = w.get_n(&format!("{p}.attn.relative_position_bias_table"), (2 * ws - 1).pow(2) * heads)?;
                blocks.push(Block {
                    norm1: Norm::load(g, w, &format!("{p}.norm1"), e)?,
                    qkv: Conv::load(g, w, &format!("{p}.attn.qkv"))?,
                    bias: g.upload(&dense_bias(table, heads, ws)),
                    proj: Conv::load(g, w, &format!("{p}.attn.proj"))?,
                    norm2: Norm::load(g, w, &format!("{p}.norm2"), e)?,
                    fc1: Conv::load(g, w, &format!("{p}.mlp.fc1"))?,
                    fc2: Conv::load(g, w, &format!("{p}.mlp.fc2"))?,
                    heads: heads as u32,
                    shift: if b % 2 == 1 { (ws / 2) as u32 } else { 0 },
                });
            }
            groups.push((blocks, Tail::load(g, w, &format!("layers.{l}.conv"), cfg.three_conv)?));
        }
        let up = match cfg.upsampler {
            SwinUp::PixelShuffle => {
                let mut stages = Vec::new();
                let r = if cfg.scale == 3 { 3 } else { 2 };
                for i in w.indices("upsample.") {
                    stages.push((Conv::load(g, w, &format!("upsample.{i}"))?, r));
                }
                Up::PixelShuffle {
                    before: Conv::load(g, w, "conv_before_upsample.0")?,
                    stages,
                    last: Conv::load(g, w, "conv_last")?,
                }
            }
            SwinUp::Direct => Up::Direct {
                conv: Conv::load(g, w, "upsample.0")?,
                scale: cfg.scale,
            },
            SwinUp::NearestConv => Up::NearestConv {
                before: Conv::load(g, w, "conv_before_upsample.0")?,
                up1: Conv::load(g, w, "conv_up1")?,
                up2: if cfg.scale == 4 { Some(Conv::load(g, w, "conv_up2")?) } else { None },
                hr: Conv::load(g, w, "conv_hr")?,
                last: Conv::load(g, w, "conv_last")?,
            },
        };
        Ok(Net {
            cfg: cfg.clone(),
            conv_first: Conv::load(g, w, "conv_first")?,
            patch_norm: if cfg.patch_norm { Some(Norm::load(g, w, "patch_embed.norm", e)?) } else { None },
            groups,
            norm: Norm::load(g, w, "norm", e)?,
            after_body: Tail::load(g, w, "conv_after_body", cfg.three_conv)?,
            up,
        })
    }

    /// `x` is normalised already and its sides are multiples of the window.
    pub fn forward(&self, g: &Gpu, x: &T) -> T {
        let e = self.cfg.embed as u32;
        let ws = self.cfg.window as u32;
        let x0 = self.conv_first.run(g, x, Act::None);
        let mut f = match &self.patch_norm {
            Some(n) => n.run(g, &x0),
            None => g.copy(&x0),
        };
        for (blocks, tail) in &self.groups {
            let group_in = g.copy(&f);
            for blk in blocks {
                let h1 = blk.norm1.run(g, &f);
                let qkv = blk.qkv.run(g, &h1, Act::None);
                g.recycle(h1);
                let att = g.window_attn(&qkv, e, blk.heads, ws, blk.shift, &blk.bias);
                g.recycle(qkv);
                let f2 = blk.proj.run_res(g, &att, Act::None, &f);
                g.recycle(att);
                g.recycle(f);
                let h2 = blk.norm2.run(g, &f2);
                let m = blk.fc1.run(g, &h2, Act::Gelu);
                g.recycle(h2);
                f = blk.fc2.run_res(g, &m, Act::None, &f2);
                g.recycle(m);
                g.recycle(f2);
            }
            let out = tail.run_res(g, &f, &group_in);
            g.recycle(f);
            g.recycle(group_in);
            f = out;
        }
        let n = self.norm.run(g, &f);
        g.recycle(f);
        let x1 = self.after_body.run_res(g, &n, &x0);
        g.recycle(n);
        g.recycle(x0);

        match &self.up {
            Up::PixelShuffle { before, stages, last } => {
                let mut t = before.run(g, &x1, Act::Leaky(0.01));
                g.recycle(x1);
                for (conv, r) in stages {
                    let c = conv.run(g, &t, Act::None);
                    g.recycle(t);
                    t = g.pixel_shuffle(&c, *r);
                    g.recycle(c);
                }
                let y = last.run(g, &t, Act::None);
                g.recycle(t);
                y
            }
            Up::Direct { conv, scale } => {
                let c = conv.run(g, &x1, Act::None);
                g.recycle(x1);
                let y = g.pixel_shuffle(&c, *scale);
                g.recycle(c);
                y
            }
            Up::NearestConv { before, up1, up2, hr, last } => {
                let mut t = before.run(g, &x1, Act::Leaky(0.01));
                g.recycle(x1);
                for conv in std::iter::once(up1).chain(up2.as_ref()) {
                    let n = g.nearest(&t, 2);
                    g.recycle(t);
                    t = conv.run(g, &n, Act::Leaky(0.2));
                    g.recycle(n);
                }
                let h = hr.run(g, &t, Act::Leaky(0.2));
                g.recycle(t);
                let y = last.run(g, &h, Act::None);
                g.recycle(h);
                y
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The index the reference builds with its meshgrid/permute dance, spelled
    /// out the way `network_swinir.py` does it, against the closed form above.
    #[test]
    fn the_dense_bias_matches_the_reference_index() {
        let ws = 3;
        let heads = 2;
        let side = 2 * ws - 1;
        let table: Vec<f32> = (0..side * side * heads).map(|i| i as f32).collect();
        let dense = dense_bias(&table, heads, ws);
        let coords: Vec<(i64, i64)> = (0..ws * ws).map(|i| ((i / ws) as i64, (i % ws) as i64)).collect();
        for (i, ci) in coords.iter().enumerate() {
            for (j, cj) in coords.iter().enumerate() {
                let mut r0 = ci.0 - cj.0 + ws as i64 - 1;
                let r1 = ci.1 - cj.1 + ws as i64 - 1;
                r0 *= 2 * ws as i64 - 1;
                let idx = (r0 + r1) as usize;
                for h in 0..heads {
                    assert_eq!(dense[(h * ws * ws + i) * ws * ws + j], table[idx * heads + h]);
                }
            }
        }
    }
}
