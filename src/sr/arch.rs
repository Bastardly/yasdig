//! What kind of upscaler a file is, read off its tensor names and shapes.
//!
//! Never the filename, for the reason `discover.rs` gives: `4xNomos…` says
//! nothing about whether the file inside is an ESRGAN, a DAT or a HAT, and the
//! three need different code. The discriminators and the config inference
//! follow spandrel's (`libs/spandrel/spandrel/architectures/*/__init__.py`,
//! MIT), which is what chaiNNer and ComfyUI identify these files with — so a
//! file that loads there is classified the same way here.

use super::weights::Weights;

/// A recognised architecture, with everything its forward pass needs to know
/// that isn't a weight.
#[derive(Debug, Clone, PartialEq)]
pub enum Arch {
    /// RRDBNet. Runs through sd-cli, which has its own loader for it.
    Esrgan { scale: u32 },
    Compact(Compact),
    SwinIR(SwinIR),
    Span(Span),
    /// Recognised and not runnable here, with the architecture's name.
    Unsupported(&'static str),
    Unknown,
}

/// SRVGGNetCompact (Real-ESRGAN's "compact", `srvgg_arch.py`).
#[derive(Debug, Clone, PartialEq)]
pub struct Compact {
    pub in_ch: usize,
    pub feat: usize,
    /// Number of 3×3 convs between the first and the last.
    pub num_conv: usize,
    /// `prelu` has a learned slope per channel; otherwise it's a plain ReLU.
    pub prelu: bool,
    pub scale: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SwinUp {
    /// Classical SR: conv → LeakyReLU(0.01) → (conv, shuffle ×2)ⁿ → conv.
    PixelShuffle,
    /// Lightweight: one conv straight to `out × s²` channels, then a shuffle.
    Direct,
    /// Real-world SR: nearest 2× and a conv, once or twice.
    NearestConv,
}

/// SwinIR (`network_swinir.py`).
#[derive(Debug, Clone, PartialEq)]
pub struct SwinIR {
    pub in_ch: usize,
    pub embed: usize,
    pub depths: Vec<usize>,
    pub heads: Vec<usize>,
    pub window: usize,
    pub mlp_hidden: usize,
    pub upsampler: SwinUp,
    pub scale: u32,
    /// `patch_norm`: a LayerNorm on the embedding.
    pub patch_norm: bool,
    /// `resi_connection = '3conv'`: each group ends in three convs, not one.
    pub three_conv: bool,
    /// Feature width of the upsampling tail (`num_feat`, 64 in every release).
    pub feat: usize,
}

/// SPAN (`span_arch.py`).
#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub in_ch: usize,
    pub feat: usize,
    pub scale: u32,
    /// Most releases subtract the DIV2K mean and scale by 255 first; a file
    /// carrying a `no_norm` buffer doesn't.
    pub norm: bool,
}

impl Arch {
    /// The name the select and `doctor` show.
    pub fn name(&self) -> String {
        match self {
            Arch::Esrgan { .. } => "ESRGAN".into(),
            Arch::Compact(_) => "Compact".into(),
            Arch::SwinIR(s) => format!("SwinIR-{}", if s.embed >= 240 { "L" } else if s.embed >= 180 { "M" } else { "S" }),
            Arch::Span(_) => "SPAN".into(),
            Arch::Unsupported(n) => (*n).into(),
            Arch::Unknown => "unknown".into(),
        }
    }

    /// Can this program run it, by either route?
    pub fn supported(&self) -> bool {
        !matches!(self, Arch::Unsupported(_) | Arch::Unknown)
    }

    /// Its scale, where the weights say.
    pub fn scale(&self) -> Option<u32> {
        match self {
            Arch::Esrgan { scale } => Some(*scale),
            Arch::Compact(c) => Some(c.scale),
            Arch::SwinIR(s) => Some(s.scale),
            Arch::Span(s) => Some(s.scale),
            _ => None,
        }
    }
}

/// Integer square root of a small, expected-square, positive number.
fn isqrt(n: usize) -> Option<usize> {
    let r = (n as f64).sqrt().round() as usize;
    (r > 0 && r * r == n).then_some(r)
}

/// Dimension `i` of a tensor's shape, when the tensor has one and it is not 0.
/// Shapes come out of the file: a short one must not index past its end, and a
/// zero would turn into a division by zero further down.
fn dim(w: &Weights, name: &str, i: usize) -> Option<usize> {
    w.shape(name)?.get(i).copied().filter(|&d| d > 0)
}

pub fn detect(w: &Weights) -> Arch {
    let has = |n: &str| w.has(n);
    let any = |needle: &str| w.map.keys().any(|k| k.contains(needle));

    // The transformers first: they share `conv_first` and `layers.` with
    // SwinIR, and each has a tensor nothing else does.
    if any("conv_block.cab") || has("relative_position_index_OCA") || any("overlap_attn") {
        return Arch::Unsupported("HAT");
    }
    if any(".attn.attns.0.rpe_biases") || any("before_RG") {
        return Arch::Unsupported("DAT");
    }
    if any("residual_group.blocks.0.attn.qkv") && has("layers.0.residual_group.blocks.0.norm1.weight") {
        return swinir(w).map(Arch::SwinIR).unwrap_or(Arch::Unknown);
    }
    if has("block_1.c1_r.sk.weight") {
        return span(w).map(Arch::Span).unwrap_or(Arch::Unknown);
    }
    // ESRGAN, both key layouts: BasicSR's (`body.N.rdb1.…`) and the original
    // (`model.1.sub.N.RDB1.…`).
    if has("body.0.rdb1.conv1.weight") || has("model.1.sub.0.RDB1.conv1.0.weight") {
        let scale = if has("conv_up2.weight") || has("model.6.weight") && has("model.8.weight") {
            4
        } else if has("conv_up1.weight") || has("model.3.weight") && has("model.5.weight") {
            2
        } else {
            1
        };
        return Arch::Esrgan { scale };
    }
    if has("body.0.weight") && w.shape("body.0.weight").map(|s| s.len()) == Some(4) {
        return compact(w).map(Arch::Compact).unwrap_or(Arch::Unknown);
    }
    // Recognisable and not built here — named, so the select can say what it
    // is rather than "unknown".
    if any("feats.") && has("feats.0.weight") {
        return Arch::Unsupported("RealPLKSR");
    }
    if any("residual_layer.") && any("window_block") {
        return Arch::Unsupported("OmniSR");
    }
    Arch::Unknown
}

fn compact(w: &Weights) -> Option<Compact> {
    let (feat, in_ch) = (dim(w, "body.0.weight", 0)?, dim(w, "body.0.weight", 1)?);
    // The last conv is the highest-numbered 4-D `body.N.weight`.
    let last = w
        .indices("body.")
        .into_iter()
        .filter(|i| w.shape(&format!("body.{i}.weight")).map(|s| s.len()) == Some(4))
        .max()?;
    let out = dim(w, &format!("body.{last}.weight"), 0)?;
    let scale = isqrt(out / in_ch)? as u32;
    let prelu = w.shape("body.1.weight").map(|s| s.len()) == Some(1);
    // body = conv, act, (conv, act)×num_conv, conv — so the last conv is at
    // 2·num_conv + 2 with an activation module between every pair.
    Some(Compact {
        in_ch,
        feat,
        num_conv: last.checked_sub(2)? / 2,
        prelu,
        scale,
    })
}

fn swinir(w: &Weights) -> Option<SwinIR> {
    let (embed, in_ch) = (dim(w, "conv_first.weight", 0)?, dim(w, "conv_first.weight", 1)?);
    let layers = w.indices("layers.");
    let mut depths = Vec::new();
    let mut heads = Vec::new();
    let mut window = 0;
    for l in &layers {
        let p = format!("layers.{l}.residual_group.blocks.");
        let blocks = w.indices(&p);
        depths.push(blocks.len());
        let table = format!("{p}0.attn.relative_position_bias_table");
        heads.push(dim(w, &table, 1)?);
        window = isqrt(dim(w, &table, 0)?)?.div_ceil(2);
    }
    if window == 0 {
        return None;
    }
    let mlp_hidden = dim(w, "layers.0.residual_group.blocks.0.mlp.fc1.weight", 0)?;
    let (upsampler, scale, feat) = if has_all(w, &["conv_up1.weight", "conv_hr.weight"]) {
        let s = if w.has("conv_up2.weight") { 4 } else { 2 };
        (SwinUp::NearestConv, s, dim(w, "conv_before_upsample.0.weight", 0)?)
    } else if w.has("conv_before_upsample.0.weight") {
        let feat = dim(w, "conv_before_upsample.0.weight", 0)?;
        // Upsample: (conv to 4·feat, shuffle 2) per power of two, or one conv
        // to 9·feat for 3×.
        let convs = w.indices("upsample.");
        let first = dim(w, "upsample.0.weight", 0)?;
        let s = if first == 9 * feat { 3 } else { 1u32 << convs.len() };
        (SwinUp::PixelShuffle, s, feat)
    } else if w.has("upsample.0.weight") {
        let out = dim(w, "upsample.0.weight", 0)?;
        let out_ch = in_ch;
        (SwinUp::Direct, isqrt(out / out_ch)? as u32, 0)
    } else {
        // Denoising/JPEG models: same size out. Not an upscaler.
        return None;
    };
    Some(SwinIR {
        in_ch,
        embed,
        depths,
        heads,
        window,
        mlp_hidden,
        upsampler,
        scale,
        patch_norm: w.has("patch_embed.norm.weight"),
        three_conv: w.has("layers.0.conv.4.weight"),
        feat,
    })
}

fn has_all(w: &Weights, names: &[&str]) -> bool {
    names.iter().all(|n| w.has(n))
}

fn span(w: &Weights) -> Option<Span> {
    let (feat, in_ch) = (dim(w, "conv_1.sk.weight", 0)?, dim(w, "conv_1.sk.weight", 1)?);
    let up = dim(w, "upsampler.0.weight", 0)?;
    Some(Span {
        in_ch,
        feat,
        scale: isqrt(up / in_ch)? as u32,
        norm: !w.has("no_norm"),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::weights::Tensor;
    use super::*;

    pub fn fake(names: &[(&str, &[usize])]) -> Weights {
        let mut w = Weights::default();
        for (n, s) in names {
            w.map.insert(n.to_string(), Tensor { shape: s.to_vec(), data: None });
        }
        w
    }

    /// Shapes are the file's claims. A short one, a zero, or a Compact whose
    /// only conv is `body.0` is an unknown file — never an index past the end
    /// or a division by zero while listing the models folder.
    #[test]
    fn a_malformed_shape_is_unknown_not_a_panic() {
        assert_eq!(detect(&fake(&[("body.0.weight", &[64, 0, 3, 3])])), Arch::Unknown);
        assert_eq!(detect(&fake(&[("body.0.weight", &[64, 3, 3, 3])])), Arch::Unknown);
        let swin = |table: &'static [usize]| {
            fake(&[
                ("conv_first.weight", &[180, 3, 3, 3]),
                ("layers.0.residual_group.blocks.0.norm1.weight", &[180]),
                ("layers.0.residual_group.blocks.0.attn.qkv.weight", &[540, 180]),
                ("layers.0.residual_group.blocks.0.attn.relative_position_bias_table", table),
                ("layers.0.residual_group.blocks.0.mlp.fc1.weight", &[360, 180]),
                ("conv_before_upsample.0.weight", &[64, 180, 3, 3]),
                ("upsample.0.weight", &[256, 64, 3, 3]),
            ])
        };
        assert_eq!(detect(&swin(&[0, 6])), Arch::Unknown);
        assert_eq!(detect(&swin(&[225])), Arch::Unknown);
        let span = fake(&[
            ("block_1.c1_r.sk.weight", &[48, 48, 1, 1]),
            ("conv_1.sk.weight", &[48]),
            ("upsampler.0.weight", &[48, 48, 3, 3]),
        ]);
        assert_eq!(detect(&span), Arch::Unknown);
    }

    #[test]
    fn esrgan_is_told_by_its_rrdb_blocks_in_both_layouts() {
        let new = fake(&[
            ("conv_first.weight", &[64, 3, 3, 3]),
            ("body.0.rdb1.conv1.weight", &[32, 64, 3, 3]),
            ("conv_up1.weight", &[64, 64, 3, 3]),
            ("conv_up2.weight", &[64, 64, 3, 3]),
        ]);
        assert_eq!(detect(&new), Arch::Esrgan { scale: 4 });
        let old = fake(&[
            ("model.0.weight", &[64, 3, 3, 3]),
            ("model.1.sub.0.RDB1.conv1.0.weight", &[32, 64, 3, 3]),
            ("model.3.weight", &[64, 64, 3, 3]),
            ("model.5.weight", &[64, 64, 3, 3]),
        ]);
        assert_eq!(detect(&old), Arch::Esrgan { scale: 2 });
    }

    #[test]
    fn a_compact_model_reads_its_depth_and_scale() {
        // realesr-general-x4v3: 64 feat, 32 convs, PReLU, 4×.
        let mut names: Vec<(String, Vec<usize>)> = vec![("body.0.weight".into(), vec![64, 3, 3, 3])];
        for i in 0..33 {
            names.push((format!("body.{}.weight", 2 * i + 1), vec![64]));
            if i < 32 {
                names.push((format!("body.{}.weight", 2 * i + 2), vec![64, 64, 3, 3]));
            }
        }
        names.push(("body.66.weight".into(), vec![48, 64, 3, 3]));
        let refs: Vec<(&str, &[usize])> = names.iter().map(|(n, s)| (n.as_str(), s.as_slice())).collect();
        assert_eq!(
            detect(&fake(&refs)),
            Arch::Compact(Compact { in_ch: 3, feat: 64, num_conv: 32, prelu: true, scale: 4 })
        );
    }

    #[test]
    fn swinir_reads_heads_and_window_off_the_bias_table() {
        let mut names: Vec<(String, Vec<usize>)> = vec![
            ("conv_first.weight".into(), vec![180, 3, 3, 3]),
            ("patch_embed.norm.weight".into(), vec![180]),
            ("conv_before_upsample.0.weight".into(), vec![64, 180, 3, 3]),
            ("upsample.0.weight".into(), vec![256, 64, 3, 3]),
            ("upsample.2.weight".into(), vec![256, 64, 3, 3]),
            ("conv_last.weight".into(), vec![3, 64, 3, 3]),
        ];
        for l in 0..6 {
            for b in 0..6 {
                let p = format!("layers.{l}.residual_group.blocks.{b}.");
                names.push((format!("{p}norm1.weight"), vec![180]));
                names.push((format!("{p}attn.qkv.weight"), vec![540, 180]));
                names.push((format!("{p}attn.relative_position_bias_table"), vec![225, 6]));
                names.push((format!("{p}mlp.fc1.weight"), vec![360, 180]));
            }
            names.push((format!("layers.{l}.conv.weight"), vec![180, 180, 3, 3]));
        }
        let refs: Vec<(&str, &[usize])> = names.iter().map(|(n, s)| (n.as_str(), s.as_slice())).collect();
        let Arch::SwinIR(s) = detect(&fake(&refs)) else { panic!("not SwinIR") };
        assert_eq!((s.embed, s.window, s.scale, s.mlp_hidden), (180, 8, 4, 360));
        assert_eq!(s.depths, vec![6; 6]);
        assert_eq!(s.heads, vec![6; 6]);
        assert_eq!(s.upsampler, SwinUp::PixelShuffle);
        assert!(s.patch_norm && !s.three_conv);
        assert_eq!(Arch::SwinIR(s).name(), "SwinIR-M");
    }

    #[test]
    fn hat_and_dat_are_named_not_run() {
        // The two files this feature started from, by their distinguishing keys.
        let hat = fake(&[
            ("conv_first.weight", &[180, 3, 3, 3]),
            ("relative_position_index_OCA", &[256, 576]),
            ("layers.0.residual_group.blocks.0.conv_block.cab.0.weight", &[60, 180, 3, 3]),
            ("layers.0.residual_group.blocks.0.attn.qkv.weight", &[540, 180]),
            ("layers.0.residual_group.blocks.0.norm1.weight", &[180]),
        ]);
        assert_eq!(detect(&hat), Arch::Unsupported("HAT"));
        let dat = fake(&[
            ("conv_first.weight", &[180, 3, 3, 3]),
            ("before_RG.1.weight", &[180]),
            ("layers.0.blocks.0.attn.attns.0.rpe_biases", &[945, 2]),
        ]);
        assert_eq!(detect(&dat), Arch::Unsupported("DAT"));
        assert!(!detect(&dat).supported());
    }

    #[test]
    fn span_reads_its_width_and_scale() {
        let w = fake(&[
            ("conv_1.sk.weight", &[48, 3, 1, 1]),
            ("block_1.c1_r.sk.weight", &[48, 48, 1, 1]),
            ("upsampler.0.weight", &[48, 48, 3, 3]),
        ]);
        assert_eq!(detect(&w), Arch::Span(Span { in_ch: 3, feat: 48, scale: 4, norm: true }));
    }
}
