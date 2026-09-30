//! Running super-resolution models on the wgpu device FSR already owns.
//!
//! sd.cpp's upscaler is hard-wired to one network, ESRGAN (RRDBNet), and has
//! no way to add another. This module runs the architectures it can't — SRVGG
//! "compact", SwinIR and SPAN — with hand-written compute shaders, the same
//! approach `fsr/` takes: no ML framework, no second wgpu, no Python. ESRGAN
//! files keep going through sd-cli, which runs them well.
//!
//! Three things about it are load-bearing:
//!
//! - **One device, and nothing held.** It borrows `Fsr`'s device and queue, so
//!   there is no second adapter, and every buffer — weights and pool — is
//!   dropped when [`upscale`] returns. sd-cli runs next on the same card and
//!   needs all of it; the GPU worker is serialized, so the two never overlap.
//! - **Tiled, with the tile sized from the device.** Activations at the
//!   output resolution are the peak (a 4× SwinIR tail is 64 channels at 4× the
//!   tile), so the tile is shrunk until [`Net::peak`] fits the storage binding
//!   limit. Tiles overlap by [`Net::overlap`] pixels of context that are
//!   computed and thrown away — cropped, not blended, as spandrel does.
//! - **Checked by round trip, not by reference.** There is no PyTorch here to
//!   compare against. What a correct SR model *does* promise is that its output,
//!   shrunk back by its own scale, is the input again; [`consistency`] measures
//!   that as a PSNR, and a mis-wired network misses it by tens of dB. The
//!   `upscale` subcommand prints it. The ops themselves are checked against CPU
//!   references by `sr-selftest`.

pub mod arch;
mod compact;
pub mod gpu;
mod layer;
mod span;
mod swinir;
pub mod weights;

use anyhow::{bail, Context, Result};
use std::path::Path;
use std::time::Instant;

use arch::Arch;
use gpu::{Gpu, T};

/// What [`upscale`] did, for the CLI and the logs.
pub struct Report {
    pub arch: String,
    pub scale: u32,
    pub tile: u32,
    pub tiles: usize,
    pub ms: u128,
}

// One per upscale, never in a collection, so the size spread between the
// variants costs nothing worth a Box.
#[allow(clippy::large_enum_variant)]
enum Net {
    Compact(compact::Net),
    SwinIR(swinir::Net),
    Span(span::Net),
}

/// SwinIR's input normalisation: the DIV2K mean, same as SPAN's.
const SWINIR_MEAN: [f32; 3] = span::MEAN;

impl Net {
    fn scale(&self) -> u32 {
        match self {
            Net::Compact(n) => n.cfg.scale,
            Net::SwinIR(n) => n.cfg.scale,
            Net::Span(n) => n.cfg.scale,
        }
    }

    /// Tile sides must be a multiple of this: SwinIR's window.
    fn multiple(&self) -> u32 {
        match self {
            Net::SwinIR(n) => n.cfg.window as u32,
            _ => 1,
        }
    }

    /// Context on each side of a tile. The conv nets see ~35 px in each
    /// direction; SwinIR's shifted windows see further but weakly, and 16 px is
    /// what spandrel's default tiling gives it.
    fn overlap(&self) -> u32 {
        match self {
            Net::SwinIR(_) => 16,
            _ => 32,
        }
    }

    /// The tile to start from before the memory limit is applied. SwinIR is
    /// ~20 MFLOP a pixel, so a smaller tile also keeps each submission short.
    fn start_tile(&self) -> u32 {
        match self {
            Net::SwinIR(_) => 256,
            _ => 512,
        }
    }

    /// Largest single tensor, in bytes, for a padded tile of side `p`.
    fn peak(&self, p: u64) -> u64 {
        let s = self.scale() as u64;
        let base = match self {
            Net::Compact(n) => n.cfg.feat.max(3 * s as usize * s as usize) as u64,
            Net::SwinIR(n) => (3 * n.cfg.embed).max(n.cfg.mlp_hidden) as u64,
            Net::Span(n) => 4 * n.cfg.feat as u64,
        };
        let out = match self {
            Net::SwinIR(n) => n.cfg.feat.max(3) as u64,
            _ => 3,
        };
        (p * p * base).max(s * s * p * p * out) * 4
    }

    fn pre(&self, rgb: [f32; 3]) -> [f32; 3] {
        match self {
            Net::Compact(_) => rgb,
            Net::SwinIR(_) => [0, 1, 2].map(|i| rgb[i] - SWINIR_MEAN[i]),
            Net::Span(n) if n.cfg.norm => [0, 1, 2].map(|i| (rgb[i] - span::MEAN[i]) * 255.0),
            Net::Span(_) => rgb,
        }
    }

    fn post(&self, rgb: [f32; 3]) -> [f32; 3] {
        match self {
            Net::SwinIR(_) => [0, 1, 2].map(|i| rgb[i] + SWINIR_MEAN[i]),
            _ => rgb,
        }
    }

    fn forward(&self, g: &Gpu, x: &T) -> T {
        match self {
            Net::Compact(n) => n.forward(g, x),
            Net::SwinIR(n) => n.forward(g, x),
            Net::Span(n) => n.forward(g, x),
        }
    }
}

/// What a file is, from its names and shapes alone — cheap enough for a listing.
pub fn identify(path: &Path) -> Result<Arch> {
    Ok(arch::detect(&weights::load(path, false)?))
}

/// Is this file one sd-cli runs, rather than this module?
pub fn is_esrgan(path: &Path) -> Result<bool> {
    Ok(matches!(identify(path)?, Arch::Esrgan { .. }))
}

/// Enlarge `src` into `dst` by the model's own scale.
pub fn upscale(device: &wgpu::Device, queue: &wgpu::Queue, model: &Path, src: &Path, dst: &Path) -> Result<Report> {
    let t = Instant::now();
    let w = weights::load(model, true)?;
    let arch = arch::detect(&w);
    let g = Gpu::new(device, queue);
    let net = match &arch {
        Arch::Compact(c) => Net::Compact(compact::Net::load(&g, &w, c)?),
        Arch::SwinIR(s) => Net::SwinIR(swinir::Net::load(&g, &w, s)?),
        Arch::Span(s) => Net::Span(span::Net::load(&g, &w, s)?),
        Arch::Esrgan { .. } => bail!("{} is an ESRGAN, which sd-cli runs", model.display()),
        other => bail!(
            "{} is {} — not an architecture this can run (ESRGAN, Compact, SwinIR and SPAN are)",
            model.display(),
            match other {
                Arch::Unknown => "not a recognised upscaler".to_string(),
                a => format!("a {}", a.name()),
            }
        ),
    };
    let in_ch = match &arch {
        Arch::Compact(c) => c.in_ch,
        Arch::SwinIR(s) => s.in_ch,
        Arch::Span(s) => s.in_ch,
        _ => 3,
    };
    if in_ch != 3 {
        bail!("{} takes {in_ch} input channels; only RGB models are supported", model.display());
    }
    drop(w);

    let img = image::open(src)
        .with_context(|| format!("loading {}", src.display()))?
        .to_rgb8();
    let limit = device.limits().max_storage_buffer_binding_size as u64;
    let (out, tile, tiles) = run_tiled(&g, &net, &img, limit)?;
    out.save_with_format(dst, image::ImageFormat::Png)
        .with_context(|| format!("writing {}", dst.display()))?;
    Ok(Report {
        arch: arch.name(),
        scale: net.scale(),
        tile,
        tiles,
        ms: t.elapsed().as_millis(),
    })
}

/// Reflect an index into `0..n` the way `F.pad(mode='reflect')` does.
fn reflect(i: i64, n: i64) -> usize {
    if n == 1 {
        return 0;
    }
    let period = 2 * (n - 1);
    let m = i.rem_euclid(period);
    (if m < n { m } else { period - m }) as usize
}

/// The tile side, and its padded side, for an image under a memory limit.
fn plan_tile(net: &Net, limit: u64) -> Result<u32> {
    let mut tile = net.start_tile();
    let mult = net.multiple();
    loop {
        let p = (tile + 2 * net.overlap()).div_ceil(mult) * mult;
        if net.peak(p as u64) <= limit {
            return Ok(tile);
        }
        if tile <= 32 {
            bail!(
                "the GPU allows {} MiB per buffer, too little for even a 32 px tile of this model",
                limit >> 20
            );
        }
        tile -= 32;
    }
}

fn run_tiled(g: &Gpu, net: &Net, img: &image::RgbImage, limit: u64) -> Result<(image::RgbImage, u32, usize)> {
    let (w, h) = img.dimensions();
    let s = net.scale();
    let ov = net.overlap();
    let mult = net.multiple();
    let tile = plan_tile(net, limit)?;
    let mut out = image::RgbImage::new(w * s, h * s);
    let mut tiles = 0;
    for y0 in (0..h).step_by(tile as usize) {
        for x0 in (0..w).step_by(tile as usize) {
            let (y1, x1) = ((y0 + tile).min(h), (x0 + tile).min(w));
            // The context window, clamped to the image, then padded up to the
            // window multiple by reflection — what SwinIR's `check_image_size`
            // does to the whole image.
            let (cy0, cx0) = (y0.saturating_sub(ov), x0.saturating_sub(ov));
            let (cy1, cx1) = ((y1 + ov).min(h), (x1 + ov).min(w));
            let (ch, cw) = (cy1 - cy0, cx1 - cx0);
            let (ph, pw) = (ch.div_ceil(mult) * mult, cw.div_ceil(mult) * mult);
            let mut data = Vec::with_capacity((ph * pw * 3) as usize);
            for yy in 0..ph {
                let sy = cy0 as usize + reflect(yy as i64, ch as i64);
                for xx in 0..pw {
                    let sx = cx0 as usize + reflect(xx as i64, cw as i64);
                    let p = img.get_pixel(sx as u32, sy as u32).0;
                    data.extend(net.pre(p.map(|v| v as f32 / 255.0)));
                }
            }
            let x = g.tensor(ph, pw, 3, &data);
            let y = net.forward(g, &x);
            g.recycle(x);
            if (y.h, y.w, y.c) != (ph * s, pw * s, 3) {
                bail!("the network returned {}×{}×{} for a {pw}×{ph} tile at {s}×", y.w, y.h, y.c);
            }
            let res = g.read(&y)?;
            g.recycle(y);
            let ow = (pw * s) as usize;
            for yy in (y0 - cy0) * s..(y1 - cy0) * s {
                for xx in (x0 - cx0) * s..(x1 - cx0) * s {
                    let i = (yy as usize * ow + xx as usize) * 3;
                    let v = net.post([res[i], res[i + 1], res[i + 2]]);
                    out.put_pixel(
                        cx0 * s + xx,
                        cy0 * s + yy,
                        image::Rgb(v.map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8)),
                    );
                }
            }
            tiles += 1;
        }
    }
    Ok((out, tile, tiles))
}

/// How well `output`, box-shrunk by `scale`, reproduces `input`, as a PSNR in
/// dB. A working SR model lands in the 30s–40s; a wrong weight layout, a
/// swapped channel order or a missed normalisation lands under 20.
pub fn consistency(input: &image::RgbImage, output: &image::RgbImage, scale: u32) -> f64 {
    let (w, h) = input.dimensions();
    let n = (scale * scale) as f64;
    let mut se = 0.0f64;
    for y in 0..h {
        for x in 0..w {
            let mut sum = [0.0f64; 3];
            for dy in 0..scale {
                for dx in 0..scale {
                    let p = output.get_pixel(x * scale + dx, y * scale + dy).0;
                    for c in 0..3 {
                        sum[c] += p[c] as f64;
                    }
                }
            }
            let q = input.get_pixel(x, y).0;
            for c in 0..3 {
                let d = sum[c] / n - q[c] as f64;
                se += d * d;
            }
        }
    }
    let mse = se / (w as f64 * h as f64 * 3.0);
    if mse == 0.0 {
        return f64::INFINITY;
    }
    10.0 * (255.0f64 * 255.0 / mse).log10()
}

// ---------------------------------------------------------------------------
// sr-selftest: every op against a CPU reference written from the PyTorch
// definition rather than from the shader.
// ---------------------------------------------------------------------------

/// Deterministic pseudo-random floats in [-1, 1).
fn noise(n: usize, seed: u32) -> Vec<f32> {
    let mut s = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            (s as f32 / u32::MAX as f32) * 2.0 - 1.0
        })
        .collect()
}

fn max_diff(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

pub mod reference {
    //! Straight transcriptions of the PyTorch ops, on channels-last data.

    /// nn.Conv2d(stride 1, padding k/2) with PyTorch-layout weights.
    #[allow(clippy::too_many_arguments)]
    pub fn conv(x: &[f32], h: usize, w: usize, cin: usize, wt: &[f32], b: &[f32], cout: usize, k: usize) -> Vec<f32> {
        let pad = (k / 2) as i64;
        let mut out = vec![0.0; h * w * cout];
        for y in 0..h {
            for xx in 0..w {
                for o in 0..cout {
                    let mut acc = b[o];
                    for i in 0..cin {
                        for ky in 0..k {
                            for kx in 0..k {
                                let sy = y as i64 + ky as i64 - pad;
                                let sx = xx as i64 + kx as i64 - pad;
                                if sy < 0 || sx < 0 || sy >= h as i64 || sx >= w as i64 {
                                    continue;
                                }
                                acc += wt[((o * cin + i) * k + ky) * k + kx] * x[(sy as usize * w + sx as usize) * cin + i];
                            }
                        }
                    }
                    out[(y * w + xx) * cout + o] = acc;
                }
            }
        }
        out
    }

    pub fn layernorm(x: &[f32], c: usize, g: &[f32], b: &[f32]) -> Vec<f32> {
        x.chunks(c)
            .flat_map(|row| {
                let mean = row.iter().sum::<f32>() / c as f32;
                let var = row.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / c as f32;
                let inv = 1.0 / (var + 1e-5).sqrt();
                row.iter().enumerate().map(move |(i, v)| (v - mean) * inv * g[i] + b[i]).collect::<Vec<_>>()
            })
            .collect()
    }

    /// nn.PixelShuffle on [H, W, C·r²] → [H·r, W·r, C], via NCHW indexing.
    pub fn pixel_shuffle(x: &[f32], h: usize, w: usize, c: usize, r: usize) -> Vec<f32> {
        let cin = c * r * r;
        let mut out = vec![0.0; h * r * w * r * c];
        for ci in 0..cin {
            let (oc, i, j) = (ci / (r * r), (ci % (r * r)) / r, ci % r);
            for y in 0..h {
                for xx in 0..w {
                    out[((y * r + i) * w * r + xx * r + j) * c + oc] = x[(y * w + xx) * cin + ci];
                }
            }
        }
        out
    }

    /// SwinTransformerBlock's attention path from qkv to the pre-proj output,
    /// literally: roll, window_partition, attention with bias and the
    /// `calculate_mask` mask, window_reverse, roll back.
    #[allow(clippy::too_many_arguments)]
    pub fn window_attn(qkv: &[f32], h: usize, w: usize, c: usize, heads: usize, ws: usize, shift: usize, table: &[f32]) -> Vec<f32> {
        let hd = c / heads;
        let scale = (hd as f32).powf(-0.5);
        // roll(-shift): shifted[y][x] = orig[(y+s)%H][(x+s)%W]
        let src = |y: usize, x: usize| ((y + shift) % h) * w + (x + shift) % w;
        // calculate_mask's labels on the shifted frame.
        let label = |v: usize, n: usize| {
            if v < n - ws {
                0
            } else if v < n - shift {
                1
            } else {
                2
            }
        };
        let mut out = vec![0.0; h * w * c];
        let side = 2 * ws - 1;
        for wy in 0..h / ws {
            for wx in 0..w / ws {
                let toks: Vec<(usize, usize)> = (0..ws * ws).map(|t| (wy * ws + t / ws, wx * ws + t % ws)).collect();
                for hh in 0..heads {
                    for (i, &(yi, xi)) in toks.iter().enumerate() {
                        let pi = src(yi, xi);
                        let mut scores = Vec::with_capacity(toks.len());
                        for (j, &(yj, xj)) in toks.iter().enumerate() {
                            let pj = src(yj, xj);
                            let mut s = 0.0;
                            for d in 0..hd {
                                s += qkv[pi * 3 * c + hh * hd + d] * scale * qkv[pj * 3 * c + c + hh * hd + d];
                            }
                            let (ly, lx) = (i / ws, i % ws);
                            let (my, mx) = (j / ws, j % ws);
                            let idx = (ly + ws - 1 - my) * side + (lx + ws - 1 - mx);
                            s += table[idx * heads + hh];
                            if shift > 0 && (label(yi, h) * 3 + label(xi, w)) != (label(yj, h) * 3 + label(xj, w)) {
                                s += -100.0;
                            }
                            scores.push(s);
                        }
                        let m = scores.iter().cloned().fold(f32::MIN, f32::max);
                        let e: Vec<f32> = scores.iter().map(|s| (s - m).exp()).collect();
                        let z: f32 = e.iter().sum();
                        for d in 0..hd {
                            let mut acc = 0.0;
                            for (j, &(yj, xj)) in toks.iter().enumerate() {
                                acc += e[j] / z * qkv[src(yj, xj) * 3 * c + 2 * c + hh * hd + d];
                            }
                            // window_reverse then roll(+shift) lands it back on pi.
                            out[pi * c + hh * hd + d] = acc;
                        }
                    }
                }
            }
        }
        out
    }
}

/// Run every op on the GPU and against [`reference`]; print the worst error and
/// a matmul throughput. Returns an error if any op disagrees.
pub fn selftest(device: &wgpu::Device, queue: &wgpu::Queue) -> Result<()> {
    let g = Gpu::new(device, queue);
    let mut failed = Vec::new();
    let mut check = |name: &str, got: &[f32], want: &[f32], tol: f32| {
        let d = max_diff(got, want);
        let ok = d <= tol;
        println!("  {:<34} max |Δ| {:>10.2e}  {}", name, d, if ok { "ok" } else { "FAIL" });
        if !ok {
            failed.push(name.to_string());
        }
    };

    // Convolutions: odd sizes so no tile edge is aligned.
    for (k, h, w, cin, cout) in [(3, 13, 21, 7, 70), (1, 9, 67, 33, 5), (3, 40, 37, 3, 64)] {
        let x = noise(h * w * cin, 1);
        let wt = noise(cout * cin * k * k, 2);
        let b = noise(cout, 3);
        let want = reference::conv(&x, h, w, cin, &wt, &b, cout, k);
        let xt = g.tensor(h as u32, w as u32, cin as u32, &x);
        let conv = layer::Conv::from_parts(&g, &gpu::conv_weight(&wt, &[cout, cin, k, k]), Some(&b), k as u32);
        let y = conv.run(&g, &xt, gpu::Act::None);
        check(&format!("conv {k}×{k} {h}×{w} {cin}→{cout}"), &g.read(&y)?, &want, 1e-4);
        // Epilogues: leaky, then the residual add on top.
        let res = g.tensor(h as u32, w as u32, cout as u32, &noise(h * w * cout, 4));
        let y2 = conv.run_res(&g, &xt, gpu::Act::Leaky(0.2), &res);
        let r = noise(h * w * cout, 4);
        let want2: Vec<f32> = want.iter().zip(&r).map(|(v, r)| if *v >= 0.0 { v + r } else { v * 0.2 + r }).collect();
        check("  + leaky 0.2 + residual", &g.read(&y2)?, &want2, 1e-4);
    }

    // LayerNorm.
    {
        let (n, c) = (1000, 180);
        let x = noise(n * c, 5).iter().map(|v| v * 3.0 + 1.0).collect::<Vec<_>>();
        let gm = noise(c, 6);
        let bt = noise(c, 7);
        let want = reference::layernorm(&x, c, &gm, &bt);
        let xt = g.tensor(1, n as u32, c as u32, &x);
        let norm = layer::Norm { g: g.upload(&gm), b: g.upload(&bt) };
        check("layernorm 1000×180", &g.read(&norm.run(&g, &xt))?, &want, 1e-4);
    }

    // Pixel shuffle and nearest.
    {
        let (h, w, c, r) = (5, 7, 3, 4);
        let x = noise(h * w * c * r * r, 8);
        let xt = g.tensor(h as u32, w as u32, (c * r * r) as u32, &x);
        check("pixel_shuffle ×4", &g.read(&g.pixel_shuffle(&xt, r as u32))?, &reference::pixel_shuffle(&x, h, w, c, r), 0.0);
        let xn = noise(h * w * c, 9);
        let tn = g.tensor(h as u32, w as u32, c as u32, &xn);
        let mut want = vec![0.0; h * 2 * w * 2 * c];
        for y in 0..h * 2 {
            for xx in 0..w * 2 {
                for ch in 0..c {
                    want[(y * w * 2 + xx) * c + ch] = xn[((y / 2) * w + xx / 2) * c + ch];
                }
            }
        }
        check("nearest ×2", &g.read(&g.nearest(&tn, 2))?, &want, 0.0);
    }

    // Window attention, unshifted and shifted, at SwinIR-M's head width and at
    // a 16-px window (more keys than one chunk).
    for (ws, shift, heads, c, h, w) in [(8usize, 0usize, 6usize, 180usize, 16usize, 24usize), (8, 4, 6, 180, 16, 24), (4, 2, 2, 20, 12, 8), (16, 8, 3, 48, 32, 16)] {
        let qkv = noise(h * w * 3 * c, 10 + ws as u32 + shift as u32);
        let table = noise((2 * ws - 1).pow(2) * heads, 11);
        let want = reference::window_attn(&qkv, h, w, c, heads, ws, shift, &table);
        let t = g.tensor(h as u32, w as u32, (3 * c) as u32, &qkv);
        let bias = g.upload(&swinir::dense_bias(&table, heads, ws));
        let y = g.window_attn(&t, c as u32, heads as u32, ws as u32, shift as u32, &bias);
        check(&format!("window attn ws{ws} shift{shift} {heads}h×{}", c / heads), &g.read(&y)?, &want, 1e-4);
    }

    // SPAN's gate and a concat.
    {
        let n = 777;
        let a = noise(n, 12);
        let b = noise(n, 13);
        let ta = g.tensor(1, n as u32, 1, &a);
        let tb = g.tensor(1, n as u32, 1, &b);
        let want: Vec<f32> = a.iter().zip(&b).map(|(x, y)| (x + y) * (1.0 / (1.0 + (-x).exp()) - 0.5)).collect();
        check("span gate", &g.read(&g.span_gate(&ta, &tb))?, &want, 1e-6);
        let cat = g.concat(&[&ta, &tb]);
        let want: Vec<f32> = a.iter().zip(&b).flat_map(|(x, y)| [*x, *y]).collect();
        check("concat", &g.read(&cat)?, &want, 0.0);
    }

    // Throughput: one SwinIR-M group conv at a 288² tile.
    {
        let (h, w, c) = (288u32, 288u32, 180u32);
        let x = g.tensor(h, w, c, &noise((h * w * c) as usize, 14));
        let conv = layer::Conv::from_parts(&g, &noise((c * c * 9) as usize, 15), None, 3);
        g.recycle(conv.run(&g, &x, gpu::Act::None));
        g.read(&x)?;
        let reps = 10;
        let t = Instant::now();
        for _ in 0..reps {
            g.recycle(conv.run(&g, &x, gpu::Act::None));
        }
        g.read(&x)?;
        let s = t.elapsed().as_secs_f64();
        let flop = 2.0 * (h * w) as f64 * c as f64 * (c * 9) as f64 * reps as f64;
        println!("  matmul throughput: {:.2} TFLOPS ({:.1} ms per 3×3 180→180 conv at 288²)", flop / s / 1e12, s * 1000.0 / reps as f64);
    }

    if failed.is_empty() {
        Ok(())
    } else {
        bail!("{} op(s) disagree with the CPU reference: {}", failed.len(), failed.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(label: &str, source: &str) {
        let module = naga::front::wgsl::parse_str(source)
            .unwrap_or_else(|e| panic!("{label}: {}", e.emit_to_string(source)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{label}: {e:?}"));
    }

    #[test]
    fn sr_shaders_compile() {
        check("gemm", include_str!("gemm.wgsl"));
        check("attn", include_str!("attn.wgsl"));
        check("elt", include_str!("elt.wgsl"));
    }

    #[test]
    fn reflection_matches_torch_pad() {
        // F.pad([0,1,2,3], (0,3), 'reflect') → 0 1 2 3 2 1 0
        let got: Vec<usize> = (0..7).map(|i| reflect(i, 4)).collect();
        assert_eq!(got, vec![0, 1, 2, 3, 2, 1, 0]);
        assert_eq!(reflect(5, 1), 0);
    }

    #[test]
    fn a_perfect_upscale_is_infinitely_consistent_and_a_blank_one_is_not() {
        let input = image::RgbImage::from_fn(8, 6, |x, y| image::Rgb([(x * 30) as u8, (y * 40) as u8, 128]));
        let up = image::RgbImage::from_fn(32, 24, |x, y| *input.get_pixel(x / 4, y / 4));
        assert!(consistency(&input, &up, 4).is_infinite());
        let blank = image::RgbImage::new(32, 24);
        assert!(consistency(&input, &blank, 4) < 15.0);
    }

    #[test]
    fn the_reference_attention_ignores_the_mask_when_unshifted() {
        // With a zero table and identical keys, every weight is equal and the
        // output is the mean of V over the window — shift or not the mask only
        // separates regions, so compare the unshifted case to that mean.
        let (h, w, c, heads, ws) = (4, 4, 2, 1, 2);
        let mut qkv = vec![0.0; h * w * 3 * c];
        for p in 0..h * w {
            qkv[p * 3 * c + 2 * c] = p as f32;
        }
        let out = reference::window_attn(&qkv, h, w, c, heads, ws, 0, &[0.0; 9]);
        // Pixel 0's window is pixels 0, 1, 4, 5.
        assert!((out[0] - 2.5).abs() < 1e-6);
    }

}
