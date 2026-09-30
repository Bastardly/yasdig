//! SRVGGNetCompact — Real-ESRGAN's "compact" network (BasicSR
//! `srvgg_arch.py`). A plain stack of 3×3 convs at the input resolution, a
//! pixel shuffle, and the nearest-upscaled input added back so the convs only
//! have to learn the residual.

use anyhow::Result;

use super::arch::Compact;
use super::gpu::{Act, Gpu, Wt, T};
use super::layer::Conv;
use super::weights::Weights;

pub struct Net {
    pub cfg: Compact,
    /// Every conv in order, each with its PReLU slopes when it has them.
    convs: Vec<(Conv, Option<Wt>)>,
}

impl Net {
    pub fn load(g: &Gpu, w: &Weights, cfg: &Compact) -> Result<Net> {
        let mut convs = Vec::new();
        // body = conv, act, (conv, act) × num_conv, conv.
        for k in 0..=cfg.num_conv {
            let i = 2 * k;
            let slopes = if cfg.prelu {
                Some(g.upload(w.get_n(&format!("body.{}.weight", i + 1), cfg.feat)?))
            } else {
                None
            };
            convs.push((Conv::load(g, w, &format!("body.{i}"))?, slopes));
        }
        let last = 2 * cfg.num_conv + 2;
        convs.push((Conv::load(g, w, &format!("body.{last}"))?, None));
        Ok(Net { cfg: cfg.clone(), convs })
    }

    pub fn forward(&self, g: &Gpu, x: &T) -> T {
        let n = self.convs.len();
        let mut cur: Option<T> = None;
        for (i, (conv, slopes)) in self.convs.iter().enumerate() {
            let input = cur.as_ref().unwrap_or(x);
            let y = if i + 1 == n {
                conv.run(g, input, Act::None)
            } else if let Some(s) = slopes {
                conv.run_prelu(g, input, s)
            } else {
                conv.run(g, input, Act::Leaky(0.0))
            };
            if let Some(old) = cur.replace(y) {
                g.recycle(old);
            }
        }
        let body = cur.expect("at least one conv");
        let up = g.pixel_shuffle(&body, self.cfg.scale);
        g.recycle(body);
        let base = g.nearest(x, self.cfg.scale);
        let out = g.add(&up, &base);
        g.recycle(up);
        g.recycle(base);
        out
    }
}
