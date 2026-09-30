//! A conv or linear layer, uploaded once per model and run once per tile.

use anyhow::{bail, Result};

use super::gpu::{conv_weight, Act, Gpu, Wt, T};
use super::weights::Weights;

pub struct Conv {
    pub w: Wt,
    pub b: Option<Wt>,
    pub ks: u32,
}

impl Conv {
    /// `{prefix}.weight` and, if present, `{prefix}.bias`. A 4-D weight is a
    /// square conv; a 2-D one is a Linear, which the matmul runs as a 1×1.
    pub fn load(g: &Gpu, w: &Weights, prefix: &str) -> Result<Conv> {
        let name = format!("{prefix}.weight");
        let shape = w.shape(&name).unwrap_or(&[]).to_vec();
        let data = w.get(&name)?;
        let (ks, data) = match shape.as_slice() {
            [_, _, kh, kw] if kh == kw => (*kh as u32, conv_weight(data, &shape)),
            [_, _] => (1, data.to_vec()),
            other => bail!("'{name}' has shape {other:?}; expected a square conv or a linear"),
        };
        Ok(Conv::from_parts(g, &data, w.get(&format!("{prefix}.bias")).ok(), ks))
    }

    /// From weights already in [out][ky][kx][in] order.
    pub fn from_parts(g: &Gpu, w: &[f32], b: Option<&[f32]>, ks: u32) -> Conv {
        Conv { w: g.upload(w), b: b.map(|b| g.upload(b)), ks }
    }

    pub fn run(&self, g: &Gpu, x: &T, act: Act) -> T {
        g.conv(x, &self.w, self.b.as_ref(), self.ks, act, None, None)
    }

    /// `x → conv(x) + res`, the residual added after the activation.
    pub fn run_res(&self, g: &Gpu, x: &T, act: Act, res: &T) -> T {
        g.conv(x, &self.w, self.b.as_ref(), self.ks, act, Some(res), None)
    }

    pub fn run_prelu(&self, g: &Gpu, x: &T, slopes: &Wt) -> T {
        g.conv(x, &self.w, self.b.as_ref(), self.ks, Act::Prelu, None, Some(slopes))
    }
}

pub struct Norm {
    pub g: Wt,
    pub b: Wt,
}

impl Norm {
    pub fn load(gpu: &Gpu, w: &Weights, prefix: &str, c: usize) -> Result<Norm> {
        Ok(Norm {
            g: gpu.upload(w.get_n(&format!("{prefix}.weight"), c)?),
            b: gpu.upload(w.get_n(&format!("{prefix}.bias"), c)?),
        })
    }

    pub fn run(&self, g: &Gpu, x: &T) -> T {
        g.layernorm(x, &self.g, &self.b)
    }
}
