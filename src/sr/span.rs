//! SPAN (Wan et al. 2024, spandrel's `span_arch.py`), inference only.
//!
//! Every conv in it is a `Conv3XC`: in training, a 1×1 → 3×3 → 1×1 chain plus
//! a 1×1 skip; at inference, `update_params` folds all four into one 3×3. That
//! fold is done here at load, on the CPU, from the training weights rather than
//! from the file's `eval_conv` — a checkpoint saved mid-training can carry an
//! `eval_conv` that was never refreshed, and folding is what the reference
//! itself does before every eval forward.
//!
//! One detail that reads like a bug and isn't: SPAB's `act1` is
//! `SiLU(inplace=True)`, so the `out1` it returns for the concat is the
//! *activated* tensor, not the conv output.

use anyhow::{Context, Result};

use super::arch::Span;
use super::gpu::{Act, Gpu, T};
use super::layer::Conv;
use super::weights::Weights;

/// DIV2K's RGB mean, which SPAN subtracts before scaling by 255.
pub const MEAN: [f32; 3] = [0.4488, 0.4371, 0.4040];

struct Block {
    c1: Conv,
    c2: Conv,
    c3: Conv,
}

pub struct Net {
    pub cfg: Span,
    conv_1: Conv,
    blocks: Vec<Block>,
    conv_2: Conv,
    conv_cat: Conv,
    up: Conv,
}

/// `Conv3XC.update_params`: one 3×3 kernel equal to conv[2]∘conv[1]∘conv[0]
/// plus the 1×1 skip, returned in [out][ky][kx][in] order with its bias.
pub fn fold_conv3xc(w: &Weights, p: &str) -> Result<(Vec<f32>, Vec<f32>)> {
    let shape = |n: &str| -> Result<Vec<usize>> {
        Ok(w.shape(&format!("{p}.{n}")).with_context(|| format!("no tensor {p}.{n}"))?.to_vec())
    };
    let s0 = shape("conv.0.weight")?; // [m1, in, 1, 1]
    let s1 = shape("conv.1.weight")?; // [m2, m1, 3, 3]
    let s2 = shape("conv.2.weight")?; // [out, m2, 1, 1]
    let (m1, cin) = (s0[0], s0[1]);
    let m2 = s1[0];
    let cout = s2[0];
    let w1 = w.get(&format!("{p}.conv.0.weight"))?;
    let b1 = w.get(&format!("{p}.conv.0.bias"))?;
    let w2 = w.get(&format!("{p}.conv.1.weight"))?;
    let b2 = w.get(&format!("{p}.conv.1.bias"))?;
    let w3 = w.get(&format!("{p}.conv.2.weight"))?;
    let b3 = w.get(&format!("{p}.conv.2.bias"))?;
    let sk = w.get(&format!("{p}.sk.weight"))?;
    let skb = w.get(&format!("{p}.sk.bias"))?;

    // conv[1]∘conv[0]: [m2][in][9], and the bias conv[0] pushes through it.
    let mut w12 = vec![0.0f32; m2 * cin * 9];
    let mut b12 = b2.to_vec();
    for o in 0..m2 {
        for m in 0..m1 {
            for t in 0..9 {
                let k = w2[(o * m1 + m) * 9 + t];
                b12[o] += k * b1[m];
                for i in 0..cin {
                    w12[(o * cin + i) * 9 + t] += k * w1[m * cin + i];
                }
            }
        }
    }
    // conv[2] on top, then the skip at the centre tap.
    let mut out = vec![0.0f32; cout * 9 * cin];
    let mut bias = b3.to_vec();
    for o in 0..cout {
        for m in 0..m2 {
            let k = w3[o * m2 + m];
            bias[o] += k * b12[m];
            for i in 0..cin {
                for t in 0..9 {
                    out[(o * 9 + t) * cin + i] += k * w12[(m * cin + i) * 9 + t];
                }
            }
        }
        bias[o] += skb[o];
        for i in 0..cin {
            out[(o * 9 + 4) * cin + i] += sk[o * cin + i];
        }
    }
    Ok((out, bias))
}

fn load3xc(g: &Gpu, w: &Weights, p: &str) -> Result<Conv> {
    let (wt, b) = fold_conv3xc(w, p)?;
    Ok(Conv::from_parts(g, &wt, Some(&b), 3))
}

impl Net {
    pub fn load(g: &Gpu, w: &Weights, cfg: &Span) -> Result<Net> {
        let mut blocks = Vec::new();
        for i in 1..=6 {
            blocks.push(Block {
                c1: load3xc(g, w, &format!("block_{i}.c1_r"))?,
                c2: load3xc(g, w, &format!("block_{i}.c2_r"))?,
                c3: load3xc(g, w, &format!("block_{i}.c3_r"))?,
            });
        }
        Ok(Net {
            cfg: cfg.clone(),
            conv_1: load3xc(g, w, "conv_1")?,
            blocks,
            conv_2: load3xc(g, w, "conv_2")?,
            conv_cat: Conv::load(g, w, "conv_cat")?,
            up: Conv::load(g, w, "upsampler.0")?,
        })
    }

    /// `x` is normalised already.
    pub fn forward(&self, g: &Gpu, x: &T) -> T {
        let feat = self.conv_1.run(g, x, Act::None);
        let mut b1: Option<T> = None;
        let mut out1_last: Option<T> = None;
        let mut cur: Option<T> = None;
        for (i, blk) in self.blocks.iter().enumerate() {
            let input = cur.as_ref().unwrap_or(&feat);
            let o1 = blk.c1.run(g, input, Act::Silu);
            let o2 = blk.c2.run(g, &o1, Act::Silu);
            let o3 = blk.c3.run(g, &o2, Act::None);
            g.recycle(o2);
            let out = g.span_gate(&o3, input);
            g.recycle(o3);
            if i == 5 {
                out1_last = Some(o1);
            } else {
                g.recycle(o1);
            }
            let prev = cur.replace(out);
            match (i, prev) {
                // block 1's output is kept for the concat.
                (1, Some(p)) => b1 = Some(p),
                (_, Some(p)) => g.recycle(p),
                _ => {}
            }
        }
        let b6 = cur.expect("six blocks");
        let b6c = self.conv_2.run(g, &b6, Act::None);
        g.recycle(b6);
        let b1 = b1.expect("block 1 output");
        let o1 = out1_last.expect("block 6 out1");
        let cat = g.concat(&[&feat, &b6c, &b1, &o1]);
        for t in [feat, b6c, b1, o1] {
            g.recycle(t);
        }
        let y = self.conv_cat.run(g, &cat, Act::None);
        g.recycle(cat);
        let u = self.up.run(g, &y, Act::None);
        g.recycle(y);
        let out = g.pixel_shuffle(&u, self.cfg.scale);
        g.recycle(u);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::super::weights::{Tensor, Weights};
    use super::*;

    fn put(w: &mut Weights, n: &str, shape: &[usize], data: Vec<f32>) {
        w.map.insert(n.into(), Tensor { shape: shape.to_vec(), data: Some(data) });
    }

    /// Run the unfolded chain on one pixel's neighbourhood directly and compare
    /// with the folded kernel. Interior pixel, so zero padding doesn't enter.
    #[test]
    fn the_fold_equals_the_chain() {
        let (cin, m1, m2, cout) = (2, 3, 4, 2);
        let seq = |n: usize, k: f32| (0..n).map(|i| (i as f32 * 0.37 + k).sin()).collect::<Vec<_>>();
        let mut w = Weights::default();
        put(&mut w, "p.conv.0.weight", &[m1, cin, 1, 1], seq(m1 * cin, 0.1));
        put(&mut w, "p.conv.0.bias", &[m1], seq(m1, 0.2));
        put(&mut w, "p.conv.1.weight", &[m2, m1, 3, 3], seq(m2 * m1 * 9, 0.3));
        put(&mut w, "p.conv.1.bias", &[m2], seq(m2, 0.4));
        put(&mut w, "p.conv.2.weight", &[cout, m2, 1, 1], seq(cout * m2, 0.5));
        put(&mut w, "p.conv.2.bias", &[cout], seq(cout, 0.6));
        put(&mut w, "p.sk.weight", &[cout, cin, 1, 1], seq(cout * cin, 0.7));
        put(&mut w, "p.sk.bias", &[cout], seq(cout, 0.8));
        let (k, b) = fold_conv3xc(&w, "p").unwrap();

        // A 3×3 patch of cin channels: x[t][i].
        let x: Vec<f32> = seq(9 * cin, 0.9);
        let g = |n: &str| w.get(n).unwrap().to_vec();
        let (w1, b1, w2, b2, w3, b3, sk, skb) = (
            g("p.conv.0.weight"), g("p.conv.0.bias"), g("p.conv.1.weight"), g("p.conv.1.bias"),
            g("p.conv.2.weight"), g("p.conv.2.bias"), g("p.sk.weight"), g("p.sk.bias"),
        );
        for o in 0..cout {
            // Chain.
            let mut h1 = vec![0.0; 9 * m1];
            for t in 0..9 {
                for m in 0..m1 {
                    h1[t * m1 + m] = b1[m] + (0..cin).map(|i| w1[m * cin + i] * x[t * cin + i]).sum::<f32>();
                }
            }
            let mut h2 = vec![0.0; m2];
            for (mm, v) in h2.iter_mut().enumerate() {
                *v = b2[mm] + (0..9).map(|t| (0..m1).map(|m| w2[(mm * m1 + m) * 9 + t] * h1[t * m1 + m]).sum::<f32>()).sum::<f32>();
            }
            let chain = b3[o]
                + (0..m2).map(|m| w3[o * m2 + m] * h2[m]).sum::<f32>()
                + skb[o]
                + (0..cin).map(|i| sk[o * cin + i] * x[4 * cin + i]).sum::<f32>();
            // Folded.
            let folded = b[o] + (0..9).map(|t| (0..cin).map(|i| k[(o * 9 + t) * cin + i] * x[t * cin + i]).sum::<f32>()).sum::<f32>();
            assert!((chain - folded).abs() < 1e-4, "out {o}: chain {chain} folded {folded}");
        }
    }
}
