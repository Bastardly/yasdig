//! Requantizing a model file you already have, instead of downloading a
//! different copy of it.
//!
//! sd.cpp ships a `-M convert` mode that reads any checkpoint it can load and
//! writes a GGUF at a chosen weight type. That turns out to matter much more on
//! this hardware than it looks, because **a file's on-disk type is not what it
//! costs to run**. `Linear::forward` (`src/model/common/ggml_block.hpp:211`)
//! asks the backend whether it can matmul the stored type and, when it can't,
//! inserts a `ggml_cast` into the *compute graph* for every linear layer in the
//! model. A 15 GB fp8 Wan checkpoint on a Vulkan device that reports no fp8 —
//! which is every RADV device today — therefore executes as BF16 and behaves
//! like a file twice its size. Converting it to `q4_K` is not a downgrade from
//! fp8; it is the first format in that list the backend can actually multiply.
//!
//! So this is a local, offline transform of a file the user already downloaded,
//! and the point of putting it in the app rather than in the README is that the
//! app already knows — from [`crate::discover`] — what every file in the models
//! dir *is*, which is the one thing the command line needs and the one thing a
//! filename doesn't say.
//!
//! ## The slot is not cosmetic
//!
//! `convert_with_components` (`src/convert.cpp:354`) takes a *separate path per
//! component* and prefixes every tensor name according to which one it was
//! passed as: `--diffusion-model` prepends `model.diffusion_model.`, `--vae`
//! prepends `vae.`, `--t5xxl` prepends `text_encoders.t5xxl.transformer.`, and
//! `-m` prepends nothing. Convert a VAE through the diffusion slot and you get
//! a perfectly valid 8 GB GGUF whose every tensor is misnamed, which fails
//! minutes into a later run rather than here. [`Slot`] is that mapping, and it
//! is derived from the discovered [`Role`] rather than from anything the client
//! sends.
//!
//! The prefixing is idempotent (`model_loader.cpp:292` only prepends when the
//! name doesn't already start with it), so a file that already carries the
//! canonical prefix round-trips unchanged.

use crate::config::Config;
use crate::discover::{self, DitKind, Role, Tensors, VaeKind};
use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

/// A weight type `--type` accepts, with what it costs per weight.
///
/// `bits` is `ggml_type_size * 8 / ggml_blck_size`, read out of the linked
/// `libggml-base` rather than from a table on a wiki — the K-quants are not the
/// round numbers their names suggest (`q4_K` is 4.5 bits, `q6_K` is 6.5625).
/// `block` is `ggml_blck_size`, which decides whether a given tensor can be
/// quantized at all; see [`quantized`].
pub struct WeightType {
    pub name: &'static str,
    pub bits: f64,
    pub block: u64,
    pub note: &'static str,
}

/// The types worth offering, largest-to-smallest within each family.
///
/// Deliberately not every `ggml_type`: the IQ quants need an importance matrix
/// to be worth anything and `-M convert` has no way to produce one here, and
/// `f32` only ever doubles a file. The order is the order the select shows.
pub const TYPES: &[WeightType] = &[
    WeightType {
        name: "q8_0",
        bits: 8.5,
        block: 32,
        note: "near-lossless, but no smaller than fp8 on disk",
    },
    WeightType {
        name: "q6_K",
        bits: 6.5625,
        block: 256,
        note: "very close to f16; the safe choice when it fits",
    },
    WeightType {
        name: "q5_K",
        bits: 5.5,
        block: 256,
        note: "a middle step when q4_K is short of memory",
    },
    WeightType {
        name: "q4_K",
        bits: 4.5,
        block: 256,
        note: "the usual choice for a 14B video model on 24 GB",
    },
    WeightType {
        name: "q4_0",
        bits: 4.5,
        block: 32,
        note: "same size as q4_K and worse; only for old loaders",
    },
    WeightType {
        name: "q3_K",
        bits: 3.4375,
        block: 256,
        note: "visible quality loss; a last resort before not running",
    },
    WeightType {
        name: "q2_K",
        bits: 2.625,
        block: 256,
        note: "severe loss — usually not worth the memory it saves",
    },
    WeightType {
        name: "f16",
        bits: 16.0,
        block: 1,
        note: "unquantized; only useful to widen an fp8 file",
    },
    WeightType {
        name: "bf16",
        bits: 16.0,
        block: 1,
        note: "unquantized, wider exponent than f16",
    },
];

pub fn weight_type(name: &str) -> Option<&'static WeightType> {
    TYPES.iter().find(|t| t.name == name)
}

/// One type as the page sees it.
///
/// The list is *served* rather than written into the page, for the reason
/// `VideoModelDto::frame_grid` is: a second copy of the table in JavaScript is a
/// second copy free to disagree with the one the CLI validates against, and the
/// disagreement would show up as a select offering a type `--type` rejects.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TypeDto {
    pub name: &'static str,
    pub bits: f64,
    pub note: &'static str,
}

/// Everything the convert tab needs to render itself.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Listing {
    pub types: Vec<TypeDto>,
    pub files: Vec<Candidate>,
}

pub fn listing(cfg: &Config) -> Listing {
    Listing {
        types: TYPES
            .iter()
            .map(|t| TypeDto {
                name: t.name,
                bits: t.bits,
                note: t.note,
            })
            .collect(),
        files: candidates(cfg),
    }
}

/// Which `sd-cli` flag a file has to be passed as, which decides the prefix its
/// tensors get. See the module docs — this is the part that fails silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// `--diffusion-model`, prefix `model.diffusion_model.`
    Diffusion,
    /// `--vae`, prefix `vae.`
    Vae,
    /// `--t5xxl`, prefix `text_encoders.t5xxl.transformer.`
    T5xxl,
    /// `-m`, no prefix. A whole single-file checkpoint, which is what the image
    /// side's models dir holds.
    Checkpoint,
}

impl Slot {
    pub fn flag(self) -> &'static str {
        match self {
            Slot::Diffusion => "--diffusion-model",
            Slot::Vae => "--vae",
            Slot::T5xxl => "--t5xxl",
            Slot::Checkpoint => "-m",
        }
    }

    /// The slot a discovered role belongs in, or `None` for a role `-M convert`
    /// has no component flag for.
    ///
    /// `QwenVl` and `ClipVision` are the `None`s that matter: H3's LLM goes in
    /// through `--llm` and the CLIP-Vision tower through `--clip_vision`, and
    /// neither is a parameter of `convert_with_components`. Converting them
    /// through some *other* slot would rename every tensor, so they are left off
    /// the list rather than offered with a footnote. A LoRA is excluded for a
    /// different reason: it is a low-rank delta whose values span a much wider
    /// range than a weight matrix, and quantizing it is how you lose the effect
    /// you downloaded it for.
    pub fn for_role(role: &Role) -> Option<Slot> {
        match role {
            Role::Dit(_) => Some(Slot::Diffusion),
            Role::Vae(_) => Some(Slot::Vae),
            Role::Umt5 => Some(Slot::T5xxl),
            Role::QwenVl | Role::ClipVision | Role::Lora | Role::Unknown(_) => None,
            // Image files reach this list through the registry as
            // `Slot::Checkpoint`, not through the video dir's scan. A bare image
            // DiT or VAE gets a slot when the image side can assemble one.
            Role::Image { .. } | Role::ImageVae(_) | Role::Unsupported(_) => None,
        }
    }
}

/// Does sd.cpp quantize this tensor, or keep it at its stored type?
///
/// A transcription of `ModelLoader::tensor_should_be_converted`
/// (`src/model_loader.cpp:1509`). It exists here only to make [`estimate`]
/// honest: quantizing a 14B transformer leaves a surprising amount of it
/// untouched — every bias, every norm scale, every embedding — and an estimate
/// that assumed otherwise would under-predict by enough to matter when the
/// question being asked is "does this fit in 24 GB".
///
/// `inner` is ggml's `ne[0]`, which is why [`discover::Tensors`] tracks its
/// layout: a block quantization is laid along the contiguous axis, so a tensor
/// whose contiguous axis isn't a whole number of blocks is skipped.
fn quantized(name: &str, inner: u64, block: u64) -> bool {
    if block > 1 && !inner.is_multiple_of(block) {
        return false;
    }
    if name.ends_with(".bias") || name.ends_with(".scale") || name.ends_with(".weight_scale") {
        return false;
    }
    // FLUX, MMDiT and UNet keep their embedders and final layer at full width;
    // the list is sd.cpp's, in sd.cpp's order, and `embedding` at the end
    // catches Wan's `patch_embedding` and every text encoder's token table.
    const KEEP: &[&str] = &[
        "img_in.",
        "txt_in.",
        "time_in.",
        "vector_in.",
        "guidance_in.",
        "final_layer.",
        "x_embedder.",
        "t_embedder.",
        "y_embedder.",
        "pos_embed",
        "context_embedder.",
        "time_embed.",
        "label_emb.",
        "embedding",
    ];
    !KEEP.iter().any(|k| name.contains(k))
}

/// Bits per weight of a stored type, for the tensors that keep theirs.
///
/// Only the types a checkpoint is actually shipped in. An unrecognized one is
/// counted as f16, which is what nearly everything that isn't quantized is; the
/// result is an estimate and says so.
fn stored_bits(dtype: &str) -> f64 {
    match dtype {
        "f64" | "i64" => 64.0,
        "f32" | "i32" => 32.0,
        "f16" | "bf16" | "i16" => 16.0,
        "f8_e4m3" | "f8_e5m2" | "i8" | "q8_0" => 8.0,
        "bool" => 8.0,
        _ => weight_type(dtype).map(|t| t.bits).unwrap_or(16.0),
    }
}

/// What the converted file will roughly weigh.
///
/// An **estimate**, and the two things it leaves out are both small and both in
/// the same direction: GGUF aligns each tensor's data to a 32-byte boundary, and
/// the metadata block costs a few hundred KB on a large model. Measured against
/// the real thing it lands within a percent, which is the accuracy the question
/// deserves — it is asked to decide whether a conversion is worth ten minutes,
/// not to plan a partition.
pub fn estimate(t: &Tensors, target: &WeightType) -> u64 {
    let mut bits = 0.0f64;
    for tensor in &t.tensors {
        let n = tensor.elements() as f64;
        let inner = tensor.inner_dim(t.layout);
        bits += n * if quantized(&tensor.name, inner, target.block) {
            target.bits
        } else {
            stored_bits(&tensor.dtype)
        };
    }
    (bits / 8.0) as u64
}

/// A file the convert tab offers, with everything the UI needs to describe it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Candidate {
    /// The key the client sends back. A path relative to the project root, which
    /// is also what makes it readable in the list.
    pub id: String,
    /// Just the filename, for the option label.
    pub file: String,
    /// What it was identified as, in words.
    pub role: String,
    /// The `sd-cli` flag it goes in as.
    pub slot: &'static str,
    pub bytes: u64,
    /// The dominant stored type, e.g. `f8_e4m3`. The type the file mostly *is*.
    pub dtype: String,
    /// Predicted output size per target type, keyed by type name.
    pub estimates: std::collections::BTreeMap<String, u64>,
    /// Set when this file is already stored at a type the backend runs natively
    /// and converting it is probably pointless.
    pub note: Option<String>,
}

/// The type most of a file's *weight* is stored in.
///
/// By bytes rather than by tensor count, because a checkpoint has thousands of
/// tiny f32 norm scales and a few hundred enormous quantized matrices, and it is
/// the matrices that decide what the file is.
fn dominant_dtype(t: &Tensors) -> String {
    let mut by_type: std::collections::BTreeMap<&str, f64> = Default::default();
    for tensor in &t.tensors {
        *by_type.entry(tensor.dtype.as_str()).or_default() +=
            tensor.elements() as f64 * stored_bits(&tensor.dtype);
    }
    by_type
        .into_iter()
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(k, _)| k.to_string())
        .unwrap_or_else(|| "unknown".into())
}

/// Every file both dirs hold that `-M convert` can usefully be pointed at.
///
/// The video side comes from [`discover::scan`], so the role — and therefore the
/// slot — is read out of the file's own header rather than guessed from its
/// name. The image side comes from the registry, where every entry is a whole
/// single-file checkpoint and so goes in through `-m` with no prefix at all.
///
/// Reading a header per file costs a seek and a few hundred KB; the measured
/// cost of the whole scan is single-digit milliseconds even across 37 GB.
pub fn candidates(cfg: &Config) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();

    let mut push = |path: &Path, slot: Slot, role: String, seen: &mut Vec<PathBuf>| {
        let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if seen.contains(&canon) {
            return;
        }
        let Ok(t) = discover::read_tensors(path) else {
            return;
        };
        let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let dtype = dominant_dtype(&t);
        let estimates = TYPES
            .iter()
            .map(|ty| (ty.name.to_string(), estimate(&t, ty)))
            .collect();
        // Worth saying once, in the list, rather than letting someone spend ten
        // minutes converting a file that was already in the best format for it.
        let note = match dtype.as_str() {
            d if d.starts_with('q') => {
                Some(format!("already quantized ({d}) — converting again loses more"))
            }
            "f8_e4m3" | "f8_e5m2" => Some(
                "fp8, which this Vulkan backend cannot multiply — it is cast to BF16 \
                 per layer at run time, so a quantized copy is both smaller and faster"
                    .into(),
            ),
            _ => None,
        };
        out.push(Candidate {
            id: rel(cfg, path),
            file: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            role,
            slot: slot.flag(),
            bytes,
            dtype,
            estimates,
            note,
        });
        seen.push(canon);
    };

    for f in discover::scan(&cfg.video_dir()) {
        if let Some(slot) = Slot::for_role(&f.role) {
            push(&f.path, slot, describe(&f.role), &mut seen);
        }
    }
    for p in cfg.registry().values() {
        if p.exists {
            push(
                &p.model,
                Slot::Checkpoint,
                "image checkpoint".into(),
                &mut seen,
            );
        }
    }

    out.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    out
}

/// A short human name for a role, for the candidate list.
fn describe(role: &Role) -> String {
    match role {
        Role::Dit(DitKind::Wan22Ti2v5b) => "Wan 2.2 TI2V-5B transformer".into(),
        Role::Dit(DitKind::Wan22A14b) => "Wan 2.2 A14B transformer".into(),
        Role::Dit(DitKind::WanVace { dim }) => format!("Wan VACE transformer ({dim})"),
        Role::Dit(DitKind::WanOther { dim }) => format!("Wan transformer ({dim})"),
        Role::Dit(DitKind::MinimaxH3) => "MiniMax H3 transformer".into(),
        Role::Vae(VaeKind::Wan21) => "Wan 2.1 VAE".into(),
        Role::Vae(VaeKind::Wan22) => "Wan 2.2 VAE".into(),
        Role::Umt5 => "umt5-xxl text encoder".into(),
        other => format!("{other:?}"),
    }
}

/// A path as the project sees it: relative to the base dir when it is under it,
/// absolute otherwise. This is the id the client sends back, and it is only ever
/// matched against [`candidates`] — never joined onto anything — so a `..` in it
/// can't escape, it just fails to match.
fn rel(cfg: &Config, p: &Path) -> String {
    p.strip_prefix(&cfg.base_dir)
        .unwrap_or(p)
        .to_string_lossy()
        .into_owned()
}

/// A validated conversion, ready to hand to [`crate::sd::SdRunner::convert`].
#[derive(Debug, Clone, serde::Serialize)]
pub struct Plan {
    pub source: PathBuf,
    pub slot: &'static str,
    pub out: PathBuf,
    pub ty: String,
    pub source_bytes: u64,
    pub estimated_bytes: u64,
}

/// Where a converted file lands: beside its source, named for the type.
///
/// Beside it rather than in a chosen directory because the source is already in
/// the role folder discovery expects — put the q4_K copy of a transformer next
/// to the transformer and it is found on the next start with no config edit at
/// all. The extension is always `.gguf`: `export_loaded_model` picks its
/// container from the output extension (`src/convert.cpp:331`), and safetensors
/// has no way to store a K-quant.
pub fn output_path(source: &Path, ty: &str) -> PathBuf {
    let stem = source.file_stem().unwrap_or_default().to_string_lossy();
    source.with_file_name(format!("{stem}-{ty}.gguf"))
}

/// Resolve and check a request from the client.
///
/// `source` has to be one of the ids [`candidates`] published. That is the
/// traversal guard, and it is a whitelist in the same spirit as
/// `workitems::valid_id` — the client cannot name a path, only choose one the
/// server already offered.
pub fn plan(cfg: &Config, source: &str, ty: &str) -> Result<Plan> {
    if weight_type(ty).is_none() {
        bail!(
            "'{ty}' is not a weight type this can convert to (one of: {})",
            TYPES
                .iter()
                .map(|t| t.name)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let all = candidates(cfg);
    let Some(c) = all.iter().find(|c| c.id == source) else {
        bail!("'{source}' is not a model file this can convert");
    };
    let path = cfg.resolve(&c.id);
    let out = output_path(&path, ty);
    if out.exists() {
        bail!(
            "{} already exists — delete it first, or pick another type",
            out.display()
        );
    }
    if c.dtype == ty {
        bail!("{} is already stored as {ty}", c.file);
    }
    Ok(Plan {
        source: path,
        slot: c.slot,
        out,
        ty: ty.to_string(),
        source_bytes: c.bytes,
        estimated_bytes: c.estimates.get(ty).copied().unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover::{Layout, TensorInfo};

    fn t(rows: &[(&str, &[u64], &str)], layout: Layout) -> Tensors {
        Tensors {
            tensors: rows
                .iter()
                .map(|(n, s, d)| TensorInfo {
                    name: n.to_string(),
                    shape: s.to_vec(),
                    dtype: d.to_string(),
                })
                .collect(),
            layout,
        }
    }

    #[test]
    fn a_bias_keeps_its_type_while_the_matrix_is_quantized() {
        // The whole reason `estimate` transcribes sd.cpp's rule: 5120 f32 bias
        // values stay 4 bytes each however hard the matrix beside them is
        // squeezed.
        let q4k = weight_type("q4_K").unwrap();
        let m = t(
            &[("blocks.0.self_attn.q.weight", &[5120, 5120], "f16")],
            Layout::PyTorch,
        );
        let b = t(&[("blocks.0.self_attn.q.bias", &[5120], "f32")], Layout::PyTorch);
        assert_eq!(estimate(&m, q4k), (5120 * 5120) * 9 / 16); // 4.5 bits
        assert_eq!(estimate(&b, q4k), 5120 * 4); // untouched f32
    }

    #[test]
    fn an_embedding_is_left_alone_like_sd_cpp_leaves_it() {
        let q4k = weight_type("q4_K").unwrap();
        let e = t(
            &[("patch_embedding.weight", &[5120, 5120], "f16")],
            Layout::PyTorch,
        );
        assert_eq!(estimate(&e, q4k), 5120 * 5120 * 2, "still f16");
    }

    #[test]
    fn a_tensor_that_is_not_a_whole_number_of_blocks_is_skipped() {
        // 100 is not a multiple of 256, so q4_K cannot represent it and sd.cpp
        // leaves it at its stored type. The guard only fires on the *inner*
        // dimension, which is why the layout has to be known.
        let q4k = weight_type("q4_K").unwrap();
        let odd = t(&[("blocks.0.ffn.0.weight", &[5120, 100], "f16")], Layout::PyTorch);
        assert_eq!(estimate(&odd, q4k), 5120 * 100 * 2);
        let ok = t(&[("blocks.0.ffn.0.weight", &[100, 5120], "f16")], Layout::PyTorch);
        assert_eq!(estimate(&ok, q4k), 100 * 5120 * 9 / 16);
    }

    #[test]
    fn the_inner_dimension_flips_with_the_file_format() {
        // The same tensor, stored by the two formats, has to give the same
        // answer — which it only does if `estimate` reads `ne[0]` from the end
        // the format put it at.
        let q4k = weight_type("q4_K").unwrap();
        let pt = t(&[("blocks.0.ffn.0.weight", &[100, 5120], "f16")], Layout::PyTorch);
        let gg = t(&[("blocks.0.ffn.0.weight", &[5120, 100], "f16")], Layout::Ggml);
        assert_eq!(estimate(&pt, q4k), estimate(&gg, q4k));
    }

    #[test]
    fn fp8_shrinks_by_almost_half_at_q4_k() {
        // The measurement the convert tab exists to act on: an fp8 file is 8
        // bits a weight and q4_K is 4.5, so the transformer body nearly halves.
        let q4k = weight_type("q4_K").unwrap();
        let w = t(
            &[("blocks.0.self_attn.q.weight", &[5120, 5120], "f8_e4m3")],
            Layout::PyTorch,
        );
        let before = 5120u64 * 5120; // fp8 is one byte a weight
        let after = estimate(&w, q4k);
        assert_eq!(after, before * 9 / 16); // 4.5 bits = 9/16 of a byte
        assert_eq!(before - after, before * 7 / 16, "a 43.75% saving");
    }

    #[test]
    fn every_offered_type_is_a_name_sd_cpp_accepts() {
        // `str_to_sd_type` matches `ggml_get_type_traits()->type_name` exactly,
        // and the K in `q4_K` is capital there. A lowercase `q4_k` would be
        // rejected by the CLI after the user had already picked it.
        for ty in TYPES {
            assert_eq!(weight_type(ty.name).map(|t| t.name), Some(ty.name));
            assert!(!ty.name.contains("_k"), "{} should spell K capital", ty.name);
            assert!(ty.bits > 0.0 && ty.block >= 1);
        }
    }

    #[test]
    fn a_role_with_no_convert_flag_is_left_off_the_list() {
        // Qwen3-VL goes in through `--llm`, which `convert_with_components` has
        // no parameter for. Offering it through some other slot would rename
        // every tensor in it.
        assert_eq!(Slot::for_role(&Role::QwenVl), None);
        assert_eq!(Slot::for_role(&Role::ClipVision), None);
        assert_eq!(Slot::for_role(&Role::Lora), None);
        assert_eq!(
            Slot::for_role(&Role::Dit(DitKind::Wan22A14b)),
            Some(Slot::Diffusion)
        );
        assert_eq!(Slot::for_role(&Role::Vae(VaeKind::Wan21)), Some(Slot::Vae));
        assert_eq!(Slot::for_role(&Role::Umt5), Some(Slot::T5xxl));
    }

    #[test]
    fn the_output_lands_beside_its_source_as_a_gguf() {
        let p = output_path(
            Path::new("/m/video/diffusion_models/wan22-fp8.safetensors"),
            "q4_K",
        );
        assert_eq!(
            p,
            Path::new("/m/video/diffusion_models/wan22-fp8-q4_K.gguf"),
            "beside the source, so discovery finds it with no config edit"
        );
    }
}
