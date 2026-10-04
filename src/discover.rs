//! Identifying model files by what is *inside* them.
//!
//! This started on the video side, where a model is three to five files with
//! distinct roles and the roles are not in the filenames — the file that
//! arrived here called `minimax_h3Comfy.safetensors` is a Qwen3-VL-32B text
//! encoder, which filename matching would have slotted in as a transformer and
//! then failed on minutes into a run.
//!
//! The image side needed it next, for the same reason arriving from the other
//! direction: its scan used to take any `.safetensors` in the models dir as an
//! SDXL checkpoint, so an LTX-2 video transformer dropped there was offered in
//! the Image tab's select. Every image family after SDXL is also a set of files
//! rather than one — a Z-Image is a DiT, a Qwen3 text encoder and Flux's VAE —
//! which is the shape this module already assembles.
//!
//! So nothing here reads a filename to decide *what* a file is. Both formats
//! carry a tensor index in a header at the front — safetensors a JSON blob
//! behind a `u64` length, GGUF a typed key/value block — and the tensor names
//! in it identify the architecture outright. Reading it costs one seek and a
//! few hundred KB against files of 8-20 GB: measured, a `doctor` run that
//! identifies 37 GB across three files takes 7 ms in total, so the scan is
//! redone on every listing rather than cached — a download that finished a
//! minute ago should appear without a restart.
//!
//! The discriminators are sd.cpp's own, from `ModelLoader::get_sd_version`
//! (`src/model_loader.cpp:495-605`), because the only classification that
//! matters is the one the binary we shell out to will agree with. Where this
//! module uses a different tensor to reach the same answer, the comment says
//! which and why.
//!
//! Two things genuinely cannot be read from a header, and both are handled by
//! saying so rather than by guessing:
//!
//! - **Which half of a mixture-of-experts pair a file is.** The high- and
//!   low-noise experts of an A14B are the same architecture with the same
//!   tensor names and the same shapes; they differ only in their weights. That
//!   one falls back to the filename, which is the only place the information
//!   exists — and when the filename doesn't say either, the file becomes a
//!   dense single-expert model rather than a guess about which half it is.
//! - **A file in a format we can't parse.** It is reported as unrecognized,
//!   with its path, rather than assumed to be anything.

use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::config::{Components, ImageFamily, VideoFamily, VideoModel};

/// One tensor's name, shape and stored type.
///
/// `shape` is kept in the file's own order and only ever read through
/// [`Tensors::square_dim`] — which is order-agnostic on purpose, see there — or
/// through [`Tensors::inner_dim`], which needs [`Tensors::layout`] to know which
/// end to take.
///
/// `dtype` is spelled the way **ggml** spells it (`f16`, `bf16`, `q4_K`,
/// `f8_e4m3`), not the way safetensors does (`F16`, `BF16`), because that is the
/// spelling `--type` accepts and the one `crate::convert` compares against.
/// Classification ignores it; the convert tab is what it exists for.
#[derive(Debug, Clone)]
pub struct TensorInfo {
    pub name: String,
    pub shape: Vec<u64>,
    pub dtype: String,
}

/// Which end of `shape` is the fastest-varying dimension.
///
/// safetensors writes a shape in PyTorch order, so a `Linear` weight is
/// `[out, in]` and ggml's `ne[0]` — the contiguous axis a quantization block is
/// laid along — is the **last** entry. GGUF already stores `ne` and so is the
/// **first**. Nothing in classification cares, because `square_dim` only reads
/// square tensors; `crate::convert` does, because whether a tensor can be
/// quantized at all depends on `ne[0] % block_size`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// safetensors: `[out, in]`, so `ne[0]` is last.
    PyTorch,
    /// GGUF: already `ne`, so `ne[0]` is first.
    Ggml,
}

/// A file's tensor index, with the lookups classification needs.
#[derive(Debug, Clone)]
pub struct Tensors {
    pub tensors: Vec<TensorInfo>,
    pub layout: Layout,
}

impl Tensors {
    /// Is there a tensor whose name contains this?
    ///
    /// A substring test rather than an exact match because the same checkpoint
    /// ships with and without a `model.diffusion_model.` prefix depending on who
    /// repackaged it, and sd.cpp matches these the same way (`.find(...) !=
    /// npos`) for the same reason.
    pub fn has(&self, needle: &str) -> bool {
        self.tensors.iter().any(|t| t.name.contains(needle))
    }

    /// The dimension of the first tensor matching `needle`, **provided it is
    /// square**.
    ///
    /// Squareness is the point, not a coincidence. safetensors stores a shape in
    /// PyTorch order and GGUF stores it reversed, so any rectangular tensor
    /// means picking the right end of the array per format and being wrong in
    /// one of them. A model's width shows up in plenty of square weights — an
    /// attention projection is `[dim, dim]` — so reading it from one of those
    /// makes the answer independent of the layout question entirely.
    pub fn square_dim(&self, needle: &str) -> Option<u64> {
        self.tensors
            .iter()
            .filter(|t| t.name.contains(needle))
            .find_map(|t| match t.shape.as_slice() {
                // A Conv3d weight is [out, in, kT, kH, kW]; the trailing kernel
                // dims are 1 for the 1x1x1 convolutions this is used on, so the
                // leading pair is what has to match.
                [a, b, rest @ ..] if a == b && rest.iter().all(|&d| d == 1) => Some(*a),
                _ => None,
            })
    }
}

impl Tensors {
    /// The length of the first **one-dimensional** tensor matching `needle`.
    ///
    /// A vector has one axis, so which end of `shape` it is written at cannot
    /// matter — unlike [`Tensors::square_dim`], this is proof against the layout
    /// flip *and* against GGUF's four-dimension cap, which is a second way a
    /// shape fails to round-trip. sd.cpp collapses a 5-D Conv3d weight on
    /// export: `[16, 16, 1, 1, 1]` comes back as `[1, 1, 1, 256]`, element count
    /// preserved and structure gone. Reading a width off the matching `.bias`
    /// instead sidesteps both problems, and biases are never quantized
    /// (`model_loader.cpp:1517`), so they survive a conversion untouched.
    pub fn vector_len(&self, needle: &str) -> Option<u64> {
        self.tensors
            .iter()
            .filter(|t| t.name.contains(needle))
            .find_map(|t| match t.shape.as_slice() {
                [n] => Some(*n),
                _ => None,
            })
    }
}

impl TensorInfo {
    /// ggml's `ne[0]`: the contiguous axis, which is the one a quantization
    /// block is laid along. Which end of `shape` that is depends on the file
    /// format — see [`Layout`].
    pub fn inner_dim(&self, layout: Layout) -> u64 {
        match layout {
            Layout::PyTorch => self.shape.last().copied().unwrap_or(0),
            Layout::Ggml => self.shape.first().copied().unwrap_or(0),
        }
    }

    /// How many weights the tensor holds.
    pub fn elements(&self) -> u64 {
        self.shape.iter().product()
    }
}

/// What a file turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Role {
    /// A diffusion transformer, with the family it maps onto when there is one.
    Dit(DitKind),
    /// A Wan VAE. The two are not interchangeable and the wrong one produces
    /// garbage rather than an error, so which one is recorded.
    Vae(VaeKind),
    /// The umt5-xxl text encoder every Wan family shares.
    Umt5,
    /// A Qwen3-VL vision-language model — MiniMax H3's text encoder.
    QwenVl,
    /// A CLIP-Vision tower, wanted by Wan 2.1 I2V and FLF2V.
    ClipVision,
    /// A LoRA, and the base it was trained against — read off the names of the
    /// weights it patches, the same way a checkpoint's family is. See
    /// [`lora_base`].
    Lora(LoraBase),
    /// An image diffusion model — what the Image tab's select lists.
    ///
    /// `bundled` is whether its VAE and text encoder travel in the same file,
    /// which is the difference between sd.cpp's `-m` and `--diffusion-model`
    /// plus a flag per component. Every SDXL checkpoint on a download site is
    /// bundled; a Z-Image or Flux DiT almost never is.
    Image { family: ImageFamily, bundled: bool },
    /// A VAE in the Stable Diffusion (LDM) layout — `decoder.conv_in`,
    /// `decoder.mid` — told apart by latent width.
    ImageVae(ImageVaeKind),
    /// A plain causal language model used as a text encoder (`--llm`), with no
    /// vision tower — that one is [`Role::QwenVl`].
    Llm(LlmKind),
    /// An architecture sd.cpp knows and this build does not run yet, named.
    ///
    /// Distinct from [`Role::Unknown`] because the two call for different
    /// sentences: "nothing matched" is a bad download or a new architecture,
    /// while "this is an LTX-2 video transformer" is a perfectly good file in
    /// the wrong place, or one waiting for its family to be added.
    Unsupported(String),
    /// Parsed, but nothing matched. Carries a short reason for `doctor`.
    Unknown(String),
}

/// What a LoRA patches.
///
/// Needed because sd.cpp takes a LoRA for the wrong model without complaint: it
/// matches tensor names, logs the ones it couldn't place, and renders. An SDXL
/// LoRA on a Z-Image run changes nothing; an SD 1 one on an SDXL run patches the
/// one text encoder the two share and nothing else. Either way the picture comes
/// back looking like the LoRA was there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoraBase {
    /// An image family — runnable here or not; [`LoraBase::fits`] compares it
    /// with the model it would be applied to.
    Image(ImageFamily),
    /// An architecture with no [`ImageFamily`] variant, named: a Wan LoRA
    /// (video, named in a `[[video]]` entry instead), a Krea 2 one.
    Other(&'static str),
    /// No marker matched. Offered anyway: refusing a file that works because
    /// this couldn't read it is worse than the mismatch the check exists for.
    Unknown,
}

impl LoraBase {
    /// Whether a LoRA for this base belongs on a `family` model.
    pub fn fits(self, family: ImageFamily) -> bool {
        match self {
            LoraBase::Image(f) => f == family,
            LoraBase::Other(_) => false,
            LoraBase::Unknown => true,
        }
    }

    /// The base as a name, for the picker and `doctor`; empty when unknown.
    pub fn label(self) -> &'static str {
        match self {
            LoraBase::Image(f) => f.label(),
            LoraBase::Other(name) => name,
            LoraBase::Unknown => "",
        }
    }
}

/// The image-side VAEs, by latent channel count — which is what decides which
/// family can decode them, and the one thing about a VAE that is wrong silently
/// rather than loudly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageVaeKind {
    /// 4 latent channels: SD 1.x, SD 2.x and SDXL (whose VAE is a retrain of the
    /// same shape).
    Sd,
    /// 16 latent channels: Flux's `ae`, which Z-Image and Chroma share.
    Flux,
}

/// The language models the image families use as text encoders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmKind {
    /// Qwen3, at the hidden width read from its final norm. Z-Image wants the
    /// 4B, which is 2560 wide; Flux 2 Klein takes the same file, which is why
    /// the width is carried rather than the family decided here.
    Qwen3 { dim: u64 },
}

/// The transformer architectures worth telling apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DitKind {
    /// Wan 2.2 TI2V 5B — the only Wan whose width is 3072, and the only one
    /// taking `wan2.2_vae`.
    Wan22Ti2v5b,
    /// A Wan 2.2 A14B expert. Which half is not in the header; see
    /// [`noise_marker`].
    Wan22A14b,
    /// Wan VACE, at whatever width — the `vace_blocks` tensors are the marker
    /// and they are what `--control-video` drives.
    WanVace { dim: u64 },
    /// A Wan transformer at a width that isn't the 5B's or the A14B's — a Wan
    /// 2.1 T2V 14B or 1.3B. Dense, on the 2.1 VAE. The width is carried so
    /// `doctor` can name what it saw.
    WanOther { dim: u64 },
    /// MiniMax H3.
    MinimaxH3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaeKind {
    /// `wan_2.1_vae`, 16 latent channels. Everything except the TI2V-5B.
    Wan21,
    /// `wan2.2_vae`, 48 latent channels. TI2V-5B only.
    Wan22,
}

impl Role {
    /// One phrase naming what this is, for `doctor` and for the LoRA listing.
    ///
    /// On the enum rather than in `preflight.rs`, where it started, because two
    /// listings now print it and the descriptions are properties of the role
    /// rather than of either listing.
    pub fn describe(&self) -> String {
        match self {
            Role::Umt5 => "umt5-xxl text encoder (shared by every Wan family)".into(),
            Role::QwenVl => "Qwen3-VL text encoder (MiniMax H3 only)".into(),
            Role::ClipVision => "CLIP-Vision tower (Wan 2.1 I2V/FLF2V)".into(),
            Role::Vae(VaeKind::Wan21) => "wan_2.1 VAE — every Wan except the TI2V-5B".into(),
            Role::Vae(VaeKind::Wan22) => "wan2.2 VAE — TI2V-5B only".into(),
            Role::Lora(LoraBase::Unknown) => "LoRA".into(),
            Role::Lora(base) => format!("LoRA for {}", base.label()),
            Role::Dit(DitKind::WanOther { dim }) => {
                format!("a Wan transformer of width {dim}, which is not a family this build runs")
            }
            Role::Dit(_) => "transformer".into(),
            Role::Image { family, bundled: true } => format!("{} checkpoint", family.label()),
            Role::Image { family, bundled: false } => {
                format!("{} diffusion model (VAE and text encoder are separate files)", family.label())
            }
            Role::ImageVae(ImageVaeKind::Sd) => "SD VAE (4 channels) — SD 1.x/2.x/SDXL".into(),
            Role::ImageVae(ImageVaeKind::Flux) => {
                "Flux ae VAE (16 channels) — Flux, Z-Image, Chroma".into()
            }
            Role::Llm(LlmKind::Qwen3 { dim: 2560 }) => {
                "Qwen3-4B text encoder — Z-Image".into()
            }
            Role::Llm(LlmKind::Qwen3 { dim }) => format!("Qwen3 text encoder ({dim} wide)"),
            Role::Unsupported(what) => format!("{what}, which this build doesn't run yet"),
            Role::Unknown(why) => why.clone(),
        }
    }
}

/// A file on disk and what it turned out to be.
#[derive(Debug, Clone)]
pub struct Found {
    pub path: PathBuf,
    pub role: Role,
}

/// Read the tensor index out of a model file's header.
///
/// Dispatches on the magic rather than the extension: a `.gguf` is identified by
/// its first four bytes, a torch file (`.ckpt`, `.pt`) by its zip or pickle
/// magic, and anything else is tried as safetensors. None of them reads past
/// the index.
///
/// The torch branch is [`crate::sr::weights`]'s pickle subset, which exists for
/// `.pth` upscalers and reads a legacy SD 1.x `.ckpt` the same way — names and
/// shapes, no storages. Without it the image scan, which used to list any
/// `.ckpt` on its extension alone, would drop them.
pub fn read_tensors(path: &Path) -> Result<Tensors> {
    let mut f = std::fs::File::open(path)
        .with_context(|| format!("opening {} to identify it", path.display()))?;
    let mut magic = [0u8; 4];
    f.read_exact(&mut magic)
        .with_context(|| format!("{} is too short to be a model file", path.display()))?;
    f.seek(SeekFrom::Start(0))?;
    if &magic == b"GGUF" {
        read_gguf(&mut f).with_context(|| format!("reading the GGUF header of {}", path.display()))
    } else if &magic == b"PK\x03\x04" || magic == [0x80, 0x02, 0x8a, 0x0a] {
        let w = crate::sr::weights::load(path, false)?;
        Ok(Tensors {
            tensors: w
                .map
                .into_iter()
                .map(|(name, t)| TensorInfo {
                    name,
                    shape: t.shape.iter().map(|&d| d as u64).collect(),
                    dtype: String::new(),
                })
                .collect(),
            layout: Layout::PyTorch,
        })
    } else {
        read_safetensors(&mut f)
            .with_context(|| format!("reading the safetensors header of {}", path.display()))
    }
}

/// Largest header we'll read. The biggest seen in practice is ~230 KB (a 2054
/// tensor Qwen3-VL-32B); 64 MB is four hundred times that and still bounds a
/// corrupt or hostile length field to something a read can survive.
const MAX_HEADER: u64 = 64 * 1024 * 1024;

/// safetensors: `u64` little-endian header length, then that many bytes of JSON
/// mapping tensor name to `{dtype, shape, data_offsets}`, plus an optional
/// `__metadata__` key that is not a tensor.
fn read_safetensors(f: &mut std::fs::File) -> Result<Tensors> {
    let mut len = [0u8; 8];
    f.read_exact(&mut len)?;
    let len = u64::from_le_bytes(len);
    if len == 0 || len > MAX_HEADER {
        bail!("header length {len} is not plausible — is this a safetensors file?");
    }
    let mut buf = vec![0u8; len as usize];
    f.read_exact(&mut buf)?;
    let json: BTreeMap<String, serde_json::Value> =
        serde_json::from_slice(&buf).context("the header is not JSON")?;
    let tensors = json
        .into_iter()
        .filter(|(k, _)| k != "__metadata__")
        .map(|(name, v)| {
            let shape = v
                .get("shape")
                .and_then(|s| s.as_array())
                .map(|a| a.iter().filter_map(|d| d.as_u64()).collect())
                .unwrap_or_default();
            let dtype = v
                .get("dtype")
                .and_then(|d| d.as_str())
                .map(safetensors_dtype)
                .unwrap_or_default();
            TensorInfo { name, shape, dtype }
        })
        .collect();
    Ok(Tensors {
        tensors,
        layout: Layout::PyTorch,
    })
}

/// safetensors dtype names in ggml's spelling.
///
/// Only the ones that appear in a checkpoint anyone ships. An unknown name is
/// passed through lowercased rather than rejected: this is used for display and
/// for a size estimate, and neither is worth failing a scan over.
fn safetensors_dtype(s: &str) -> String {
    match s {
        "F64" => "f64",
        "F32" => "f32",
        "F16" => "f16",
        "BF16" => "bf16",
        "F8_E4M3" => "f8_e4m3",
        "F8_E5M2" => "f8_e5m2",
        "I64" => "i64",
        "I32" => "i32",
        "I16" => "i16",
        "I8" => "i8",
        "BOOL" => "bool",
        other => return other.to_ascii_lowercase(),
    }
    .to_string()
}

/// GGUF: magic, version, tensor count, KV count, the KV block, then the tensor
/// index. The KV block has to be walked to be skipped, since its entries are
/// variable width — hence [`skip_gguf_value`].
fn read_gguf(f: &mut std::fs::File) -> Result<Tensors> {
    let mut r = std::io::BufReader::new(f);
    let mut magic = [0u8; 4];
    r.read_exact(&mut magic)?;
    let version = read_u32(&mut r)?;
    // v1 sized its lengths as u32. It predates every quantization anyone ships
    // today, so it is refused by name rather than supported speculatively.
    if !(2..=3).contains(&version) {
        bail!("GGUF version {version} is not supported (expected 2 or 3)");
    }
    let tensor_count = read_u64(&mut r)?;
    let kv_count = read_u64(&mut r)?;
    if tensor_count > 1_000_000 || kv_count > 1_000_000 {
        bail!("implausible GGUF header: {tensor_count} tensors, {kv_count} metadata keys");
    }
    for _ in 0..kv_count {
        let _key = read_gguf_string(&mut r)?;
        let ty = read_u32(&mut r)?;
        skip_gguf_value(&mut r, ty)?;
    }
    let mut tensors = Vec::with_capacity(tensor_count as usize);
    for _ in 0..tensor_count {
        let name = read_gguf_string(&mut r)?;
        let n_dims = read_u32(&mut r)?;
        if n_dims > 4 {
            bail!("tensor '{name}' claims {n_dims} dimensions; GGUF allows at most 4");
        }
        let mut shape = Vec::with_capacity(n_dims as usize);
        for _ in 0..n_dims {
            shape.push(read_u64(&mut r)?);
        }
        let ty = read_u32(&mut r)?;
        let _offset = read_u64(&mut r)?;
        let dtype = gguf_dtype(ty);
        tensors.push(TensorInfo { name, shape, dtype });
    }
    Ok(Tensors {
        tensors,
        layout: Layout::Ggml,
    })
}

/// ggml type ids, in ggml's own spelling.
///
/// The ids are the `ggml_type` enum's numbering and are stable — GGUF files in
/// the wild depend on that. Only the types a diffusion checkpoint is actually
/// shipped in are named; anything else displays as its number, which is enough
/// to say "this is not a type the convert list offers".
fn gguf_dtype(ty: u32) -> String {
    match ty {
        0 => "f32",
        1 => "f16",
        2 => "q4_0",
        3 => "q4_1",
        6 => "q5_0",
        7 => "q5_1",
        8 => "q8_0",
        9 => "q8_1",
        10 => "q2_K",
        11 => "q3_K",
        12 => "q4_K",
        13 => "q5_K",
        14 => "q6_K",
        15 => "q8_K",
        24 => "i8",
        25 => "i16",
        26 => "i32",
        27 => "i64",
        28 => "f64",
        30 => "bf16",
        other => return format!("type{other}"),
    }
    .to_string()
}

pub(crate) fn read_u32(r: &mut impl Read) -> Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

pub(crate) fn read_u64(r: &mut impl Read) -> Result<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

pub(crate) fn read_gguf_string(r: &mut impl Read) -> Result<String> {
    let len = read_u64(r)?;
    if len > MAX_HEADER {
        bail!("a GGUF string claims to be {len} bytes long");
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Advance past one GGUF metadata value of the given type.
///
/// The type numbers are GGUF's own enum. Arrays carry an element type and a
/// count and can hold strings, so this recurses exactly one level — GGUF has no
/// array of arrays.
pub(crate) fn skip_gguf_value(r: &mut impl Read, ty: u32) -> Result<()> {
    let width = match ty {
        0..=1 | 7 => 1, // uint8, int8, bool
        2..=3 => 2,     // uint16, int16
        4..=6 => 4,     // uint32, int32, float32
        10..=12 => 8,   // uint64, int64, float64
        8 => {
            read_gguf_string(r)?;
            return Ok(());
        }
        9 => {
            let elem = read_u32(r)?;
            let count = read_u64(r)?;
            if elem == 9 {
                bail!("nested GGUF arrays are not a thing this reads");
            }
            for _ in 0..count {
                skip_gguf_value(r, elem)?;
            }
            return Ok(());
        }
        other => bail!("unknown GGUF value type {other}"),
    };
    let mut buf = [0u8; 8];
    r.read_exact(&mut buf[..width])?;
    Ok(())
}

/// Decide what a parsed file is.
///
/// Order matters: the most specific marker wins, because several of these
/// architectures share generic tensor names. A VACE checkpoint is also a Wan
/// transformer, and a Wan VAE also has tensors under `encoder.` just as the umt5
/// encoder does.
pub fn classify(t: &Tensors) -> Role {
    // H3 is the pair of patch projections, which nothing else has.
    // `model_loader.cpp:520`.
    if t.has("video_patch_proj.weight") && t.has("audio_patch_proj.weight") {
        return Role::Dit(DitKind::MinimaxH3);
    }
    // A LoRA before anything else: it carries the base model's tensor names with
    // a suffix, so it would otherwise classify as whatever it patches.
    //
    // The last two are LyCORIS — LoHa's `hada_w1_a` and LoKr's `lokr_w1`, both
    // of which sd.cpp's own adapter loads (`src/model/adapter/lora.hpp`). They
    // are here because without them a LoHa file matches none of the `lora_*`
    // markers and falls through to whatever architecture it patches, so `doctor`
    // reports a 200 MB adapter as a transformer and the LoRA listing hides it.
    if t.has("lora_down")
        || t.has("lora_up")
        || t.has(".lora_A")
        || t.has(".lora_B")
        || t.has(".hada_w1_a")
        || t.has(".lokr_w1")
    {
        return Role::Lora(lora_base(t));
    }
    // Architectures sd.cpp names and this build doesn't run. Before everything
    // below, because two of them carry a generic marker a supported family
    // tests for: Hunyuan Video has Flux's `double_blocks`, and an SVD is a UNet.
    if let Some(what) = unsupported(t) {
        return Role::Unsupported(what.into());
    }
    // Wan's marker, `model_loader.cpp:524`.
    if t.has("blocks.0.cross_attn.norm_k.weight") {
        // Width, from a square attention projection rather than from the patch
        // embedding sd.cpp uses. sd.cpp reads `patch_embedding.weight`'s ne[3]
        // and compares it against 147456 and 184320, which are `dim *
        // in_channels` — 3072*48 for the TI2V-5B and 5120*36 for the A14B I2V.
        // The width alone separates the families this build supports and needs
        // no opinion about how a 5D convolution weight is laid out in either
        // container format.
        let dim = t
            .square_dim("blocks.0.self_attn.q.weight")
            .or_else(|| t.square_dim("blocks.0.self_attn.k.weight"))
            .unwrap_or(0);
        if t.has("vace_blocks.") || t.has("vace_patch_embedding") {
            return Role::Dit(DitKind::WanVace { dim });
        }
        return match dim {
            3072 => Role::Dit(DitKind::Wan22Ti2v5b),
            5120 => Role::Dit(DitKind::Wan22A14b),
            d => Role::Dit(DitKind::WanOther { dim: d }),
        };
    }
    // Image models, and **before either VAE test**: a bundled SDXL checkpoint
    // carries its VAE, whose resnets are named `conv1`/`conv2` under
    // `decoder.`, so it would otherwise be read as a Wan VAE of 512 channels.
    if let Some(role) = image_model(t) {
        return role;
    }
    // An image VAE, in the LDM layout. Tested ahead of the Wan VAE for the same
    // reason as above — an LDM VAE has `decoder.` and `conv2.` too — and told
    // apart from it by `conv_in`/`mid`, which Wan's (`conv1`, `middle`) doesn't
    // use. The width comes off `encoder.conv_out.bias`, which is `2 × z` (mean
    // and log-variance) and one-dimensional, so it reads the same in either
    // container — the same reason the Wan branch below reads a bias.
    if t.has("decoder.conv_in.") && t.has("decoder.mid.") {
        return match t.vector_len("encoder.conv_out.bias") {
            Some(8) => Role::ImageVae(ImageVaeKind::Sd),
            Some(32) => Role::ImageVae(ImageVaeKind::Flux),
            Some(64) => Role::Unsupported("a Flux 2 VAE (32 latent channels)".into()),
            Some(n) => Role::Unknown(format!("an LDM-style VAE with {} latent channels", n / 2)),
            None => Role::Unknown("an LDM-style VAE with no encoder to read its width from".into()),
        };
    }
    // The Wan VAE. `decoder.` plus `conv2` is the shape of it, and the latent
    // width is what separates the two — 16 for 2.1, 48 for the TI2V-5B's — which
    // is the same `z_dim` sd.cpp switches the whole block on
    // (`model/vae/wan_vae.hpp:1016`). Getting it wrong is a silent
    // garbage-output bug rather than an error, so it is read rather than
    // guessed from the filename.
    //
    // Read off `conv2.bias`, which is `[z_dim]`, rather than off
    // `conv2.weight`, which is `[z_dim, z_dim, 1, 1, 1]`. The weight is a 5-D
    // Conv3d kernel and GGUF only has four dimensions, so exporting one folds
    // the trailing axes together — `[16, 16, 1, 1, 1]` becomes `[1, 1, 1, 256]`
    // and the width is no longer in there to read. The bias is one-dimensional
    // and survives both that and the PyTorch/ggml order flip. This is not
    // hypothetical: it is what the convert tab produces.
    if t.has("decoder.") && t.has("conv2.") {
        // The weight is still the fallback, for a file that ships without
        // biases — and it still works for every safetensors VAE.
        let z = t
            .vector_len("conv2.bias")
            .or_else(|| t.square_dim("conv2.weight"));
        return match z {
            Some(48) => Role::Vae(VaeKind::Wan22),
            Some(16) => Role::Vae(VaeKind::Wan21),
            Some(z) => Role::Unknown(format!("a Wan-shaped VAE with {z} latent channels")),
            None => Role::Unknown("a VAE whose latent width could not be read".into()),
        };
    }
    // umt5. T5's own block naming, which is distinct from the VAE's `encoder.`
    // despite the shared prefix.
    if t.has("encoder.block.") && t.has("SelfAttention") {
        return Role::Umt5;
    }
    // Qwen3-VL: a causal LM (token embeddings and `layers.N.self_attn`) with a
    // vision tower bolted on. This is H3's text encoder.
    if t.has("model.embed_tokens.weight") && t.has("visual.") {
        return Role::QwenVl;
    }
    // A Qwen3 LM with no vision tower — Z-Image's text encoder. `q_norm` on the
    // attention is what separates Qwen3 from Qwen2.5, which has none. Spelled
    // both ways: Hugging Face's names in a safetensors, llama.cpp's in the GGUF
    // quantizations sd.cpp's own docs point at.
    if (t.has("embed_tokens.weight") && t.has("self_attn.q_norm.weight"))
        || (t.has("token_embd.weight") && t.has("attn_q_norm.weight"))
    {
        // The final norm is one-dimensional and as wide as the model.
        let dim = t
            .tensors
            .iter()
            .find(|x| matches!(x.name.as_str(), "model.norm.weight" | "norm.weight" | "output_norm.weight"))
            .and_then(|x| match x.shape.as_slice() {
                [n] => Some(*n),
                _ => None,
            });
        return match dim {
            Some(dim) => Role::Llm(LlmKind::Qwen3 { dim }),
            None => Role::Unknown("a Qwen3 language model whose width could not be read".into()),
        };
    }
    if t.has("vision_model.encoder.layers") {
        return Role::ClipVision;
    }
    Role::Unknown(format!("{} tensors, no known marker", t.tensors.len()))
}

/// The base a LoRA was trained against, from the names of what it patches.
///
/// **Never from the metadata.** kohya writes `ss_base_model_version` and
/// `modelspec.architecture`, and they lie: a hand-fixing LoRA in the models dir
/// says `stable-diffusion-v1/lora` and every one of its cross-attention keys is
/// 2048 wide, which is SDXL. The names can't be wrong in that way — they are
/// what sd.cpp matches against the model.
///
/// Each marker is tested in two spellings, because the trainers disagree: the
/// dotted original (`diffusion_model.double_blocks.0.…`, what sd.cpp's own
/// names look like) and kohya's flattened one (`lora_unet_double_blocks_0_…`).
/// A substring test catches both when the marker has no separator in it, and
/// the ones that do are spelled out twice.
///
/// Order matters in two places. Krea 2's blocks are `blocks.N.attn`, so it goes
/// before Wan's `blocks.N.cross_attn` would matter; and the UNet test goes
/// before Z-Image's, since a UNet LoRA's text-encoder keys have `layers` in
/// them too.
fn lora_base(t: &Tensors) -> LoraBase {
    let either = |dotted: &str, flat: &str| t.has(dotted) || t.has(flat);

    // `txtfusion` is sd.cpp's own Krea 2 marker; `attn.wq` catches a LoRA that
    // left the text-fusion blocks alone. Nothing else here names a projection
    // `wq` under `attn`.
    if t.has("txtfusion") || either(".attn.wq.", "_attn_wq") {
        return LoraBase::Other("Krea 2");
    }
    if t.has("double_blocks") || t.has("single_blocks") || t.has("single_transformer_blocks") {
        return LoraBase::Image(ImageFamily::Flux);
    }
    // Qwen-Image's dual-stream MLPs. Flux's original naming has `img_mlp` too,
    // inside `double_blocks`, which is why this comes second.
    if t.has("img_mlp") || t.has("txt_mlp") || t.has("img_mod") {
        return LoraBase::Image(ImageFamily::QwenImage);
    }
    // Flux in diffusers' spelling, which has no `double_blocks`: its text
    // stream is `ff_context` and `add_q_proj`. Qwen-Image has `add_q_proj` too,
    // and its `img_mlp` has already claimed it above.
    if t.has("ff_context") || t.has("add_q_proj") {
        return LoraBase::Image(ImageFamily::Flux);
    }
    if t.has("cross_attn") {
        return LoraBase::Other("Wan");
    }
    if let Some(family) = unet_lora(t) {
        return family;
    }
    // Z-Image: `layers.N.attention` and `layers.N.feed_forward`
    // (`z_image.hpp`), plus its two refiner stacks. `attention` with a
    // separator after it, since a diffusers UNet has `attentions`.
    if t.has("noise_refiner")
        || t.has("context_refiner")
        || (t.has("layers") && (either(".attention.", "_attention_") || t.has("feed_forward")))
    {
        return LoraBase::Image(ImageFamily::ZImage);
    }
    LoraBase::Unknown
}

/// SD 1.x, SD 2.x or SDXL, for a LoRA with UNet or CLIP keys; `None` when it
/// has neither.
///
/// The same test as the checkpoint's — the width the cross-attention keys are
/// projected from — read off the LoRA's down projection, which is
/// `[rank, context]`. A LoRA can leave cross-attention alone entirely (one in
/// the models dir trains `attn1` only), so the text encoders are the fallback:
/// a second one is SDXL, and a lone one is as wide as its hidden size, 768 for
/// SD 1's CLIP-L and 1024 for SD 2's OpenCLIP-H.
fn unet_lora(t: &Tensors) -> Option<LoraBase> {
    let unet = ["input_blocks", "output_blocks", "down_blocks", "up_blocks", "mid_block"]
        .iter()
        .any(|m| t.has(m));
    let te = t.has("lora_te") || t.has("text_encoder") || t.has("text_model");
    if !unet && !te {
        return None;
    }
    let down = |x: &&TensorInfo| x.name.contains("lora_down") || x.name.contains("lora_A");
    let width = |a: &str, b: &str| {
        t.tensors
            .iter()
            .filter(down)
            .find(|x| x.name.contains(a) && x.name.contains(b))
            .map(|x| x.inner_dim(t.layout))
    };
    let family = match width("attn2", "to_k") {
        Some(768) => ImageFamily::Sd1,
        Some(1024) => ImageFamily::Sd2,
        Some(2048) => ImageFamily::Sdxl,
        _ if t.has("lora_te2") || t.has("text_encoder_2") => ImageFamily::Sdxl,
        _ => match width("self_attn", "k_proj") {
            Some(768) => ImageFamily::Sd1,
            Some(1024) => ImageFamily::Sd2,
            _ => return Some(LoraBase::Unknown),
        },
    };
    Some(LoraBase::Image(family))
}

/// The first tensor whose name ends in `suffix`.
fn tensor<'a>(t: &'a Tensors, suffix: &str) -> Option<&'a TensorInfo> {
    t.tensors.iter().find(|x| x.name.ends_with(suffix))
}

/// An architecture sd.cpp's `get_sd_version` names and this build doesn't run,
/// by the same markers (`model_loader.cpp:427-620`), minus the
/// `model.diffusion_model.` prefix, which a bare DiT on disk usually lacks.
///
/// Every one is a single tensor that nothing supported carries, so the order
/// among them doesn't matter; what matters is that this runs before the
/// supported families are tested — see the call site.
fn unsupported(t: &Tensors) -> Option<&'static str> {
    const MARKERS: &[(&str, &str)] = &[
        // Video, but not a family the Video tab has yet. The LTX one is not
        // hypothetical: an LTX-2.3 checkpoint in the models root was listed as an
        // SDXL checkpoint before this existed.
        ("adaln_single.emb.timestep_embedder.linear_1.bias", "an LTX-2 video transformer"),
        ("txt_in.individual_token_refiner.", "a Hunyuan Video transformer"),
        ("input_blocks.8.0.time_mixer.mix_factor", "a Stable Video Diffusion UNet"),
        // Image families not yet added.
        ("joint_blocks.", "an SD3 transformer"),
        ("double_stream_modulation_img.lin.weight", "a Flux 2 transformer"),
        ("distilled_guidance_layer.", "a Chroma transformer"),
        ("nerf_final_layer_conv.", "a Chroma Radiance transformer"),
        ("txtfusion.projector.weight", "a Krea 2 transformer"),
        ("text_fusion.projector.weight", "a Krea 2 transformer"),
        ("double_blocks.0.img_mlp.gate_proj.weight", "an Ovis-Image transformer"),
        ("layers.0.adaLN_sa_ln.weight", "an ERNIE-Image transformer"),
        ("llm_adapter.blocks.0.cross_attn.q_proj.weight", "an Anima transformer"),
        ("time_text_embed.addition_t_embedding.weight", "a Qwen-Image Layered transformer"),
    ];
    MARKERS
        .iter()
        .find(|(needle, _)| t.has(needle))
        .map(|(_, what)| *what)
}

/// An image diffusion model, or `None` when the file isn't one.
///
/// The DiT markers are sd.cpp's. The UNet split is not, and says why below.
fn image_model(t: &Tensors) -> Option<Role> {
    // A bundled checkpoint carries its own VAE and text encoder; the prefixes
    // are the ones single-file releases use (`first_stage_model.`,
    // `cond_stage_model.`/`conditioner.`) and the ones sd.cpp's own converter
    // writes (`vae.`, `text_encoders.`).
    let bundled = (t.has("first_stage_model.") || t.has("vae.decoder."))
        && (t.has("cond_stage_model.") || t.has("conditioner.") || t.has("text_encoders."));
    let image = |family| Some(Role::Image { family, bundled });

    if t.has("cap_embedder.0.weight") {
        return image(ImageFamily::ZImage);
    }
    if t.has("transformer_blocks.0.img_mod.1.weight") {
        // Mage-Flow shares Qwen-Image's block naming and differs in its patch
        // width: `img_in` takes 128 channels where Qwen-Image takes 64.
        if tensor(t, "img_in.weight").map(|x| x.inner_dim(t.layout)) == Some(128) {
            return Some(Role::Unsupported("a Mage-Flow transformer".into()));
        }
        return image(ImageFamily::QwenImage);
    }
    if t.has("double_blocks.") || t.has("single_transformer_blocks.") {
        // The variants that keep Flux's layout and change what goes in: sd.cpp
        // tells them apart by `img_in`'s input width (`model_loader.cpp:589`)
        // and LongCat by its text projection. They need inputs this build
        // doesn't send (a mask, a control image), so they are named rather
        // than offered.
        if tensor(t, "txt_in.weight").map(|x| x.inner_dim(t.layout)) == Some(3584) {
            return Some(Role::Unsupported("a LongCat-Image transformer".into()));
        }
        return match tensor(t, "img_in.weight").map(|x| x.inner_dim(t.layout)) {
            Some(384) => Some(Role::Unsupported("a Flux Fill transformer".into())),
            Some(128) => Some(Role::Unsupported("a Flux Controls transformer".into())),
            Some(196) => Some(Role::Unsupported("a Flex.2 transformer".into())),
            _ => image(ImageFamily::Flux),
        };
    }
    if t.has("input_blocks.") || t.has("unet.down_blocks.") {
        // The width the cross-attention keys are projected *from* is the text
        // encoder's: CLIP-L's 768 for SD 1.x, OpenCLIP-H's 1024 for SD 2.x, and
        // SDXL's two encoders concatenated to 2048. sd.cpp reads the text
        // encoder's token embedding instead, which only works while the encoder
        // is in the file — this also reads a bare UNet. `inner_dim` because
        // `to_k` is `[inner, context]` and so is rectangular; see [`Layout`].
        let context = tensor(t, "attn2.to_k.weight").map(|x| x.inner_dim(t.layout));
        return match context {
            Some(768) => image(ImageFamily::Sd1),
            Some(1024) => image(ImageFamily::Sd2),
            Some(2048) => image(ImageFamily::Sdxl),
            // SDXL's ADM label embedding, which neither SD 1 nor 2 has, for a
            // file whose attention keys weren't readable.
            _ if t.has("label_emb.") => image(ImageFamily::Sdxl),
            Some(w) => Some(Role::Unknown(format!(
                "a UNet conditioned on {w}-wide text, which is no SD family"
            ))),
            None => Some(Role::Unknown(
                "a UNet whose text-conditioning width could not be read".into(),
            )),
        };
    }
    None
}

/// Walk a directory tree and identify every model file in it.
///
/// Recursive, so the role subdirectories under `models/video/` are a convenience
/// for the person browsing them and not something this depends on: a flat folder
/// discovers exactly the same models. Unreadable files become
/// [`Role::Unknown`] rather than stopping the walk — a half-downloaded 8 GB
/// transformer is a normal thing to find here.
pub fn scan(dir: &Path) -> Vec<Found> {
    scan_excluding(dir, &[])
}

/// [`scan`], skipping the subtrees in `skip`.
///
/// For the image scan, which walks the whole models dir and must leave out the
/// folders that have scans of their own — the video dir, whose transformers
/// are the Video tab's, and the LoRA and upscaler dirs. Compared canonically,
/// so a `video_dir` written as an absolute path still matches the walk's
/// relative one.
pub fn scan_excluding(dir: &Path, skip: &[PathBuf]) -> Vec<Found> {
    let skip: Vec<PathBuf> = skip
        .iter()
        .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()))
        .collect();
    let mut out = Vec::new();
    walk(dir, 0, &skip, &mut out);
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// Depth limit is a loop guard for symlinked directories, which people do use to
/// share one copy of a 7 GB encoder between projects.
const MAX_DEPTH: usize = 4;

fn walk(dir: &Path, depth: usize, skip: &[PathBuf], out: &mut Vec<Found>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let canon = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            if !skip.contains(&canon) {
                walk(&path, depth + 1, skip, out);
            }
            continue;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        if !matches!(ext.as_str(), "safetensors" | "gguf" | "ckpt" | "pt" | "pth") {
            continue;
        }
        let role = identify(&path);
        out.push(Found { path, role });
    }
}

/// What a file is, from its header — or from the memo, if the file hasn't
/// changed since it was last read.
///
/// The memo exists because the image scan now reads a header for every
/// checkpoint, and `Config::registry` runs on every request. An SDXL header is
/// ~2 500 tensor names of JSON, so twenty checkpoints is a parse per request
/// that tells us nothing new. Keyed on length *and* mtime rather than on the
/// path alone, so a download that finishes — same path, new length — is read
/// again: the scan's promise that a new file shows up without a restart is
/// what the memo must not break.
pub fn identify(path: &Path) -> Role {
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};
    use std::time::SystemTime;
    type Memo = HashMap<PathBuf, (u64, SystemTime, Role)>;
    static MEMO: LazyLock<Mutex<Memo>> = LazyLock::new(|| Mutex::new(HashMap::new()));

    let stamp = std::fs::metadata(path)
        .ok()
        .and_then(|m| Some((m.len(), m.modified().ok()?)));
    if let Some((len, mtime)) = stamp {
        if let Some((l, m, role)) = MEMO.lock().unwrap().get(path) {
            if (*l, *m) == (len, mtime) {
                return role.clone();
            }
        }
    }
    let role = match read_tensors(path) {
        Ok(t) => classify(&t),
        // The message is kept: "is not JSON" and "too short" are what a
        // partial download looks like, and that is worth telling apart from
        // a file whose architecture we don't know.
        Err(e) => Role::Unknown(format!("could not be read ({e})")),
    };
    if let Some((len, mtime)) = stamp {
        MEMO.lock()
            .unwrap()
            .insert(path.to_path_buf(), (len, mtime, role.clone()));
    }
    role
}

/// Which half of a mixture-of-experts pair this file is, if the name says.
///
/// `Some(true)` for the high-noise expert, `Some(false)` for the low-noise one,
/// and **`None` when the filename doesn't say** — which is a real and common
/// case, not a defensive branch. This is the one classification that cannot come
/// from the header: the two experts have identical tensor names and identical
/// shapes and differ only in their weights, so the publisher's naming is the
/// only signal that exists. A file with no marker is therefore not assumed to be
/// either half; it becomes a [`VideoFamily::WanDense`] model instead, which runs
/// it alone rather than silently passing one expert as both.
///
/// All three spellings in circulation are accepted: `HighNoise` (QuantStack),
/// `high_noise` (Comfy-Org) and `high-noise`.
pub fn noise_marker(path: &Path) -> Option<bool> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let has = |stems: [&str; 3]| stems.iter().any(|s| name.contains(s));
    if has(["highnoise", "high_noise", "high-noise"]) {
        Some(true)
    } else if has(["lownoise", "low_noise", "low-noise"]) {
        Some(false)
    } else {
        None
    }
}

/// The filename its low-noise partner would have, for a high-noise file — and
/// the reverse.
///
/// Used to pair the two halves up, and when only one half has been downloaded,
/// to name the other one in the "still missing" list. That is the whole
/// download-checklist property: fetch either expert and the UI tells you the
/// exact filename of the one it is still waiting for.
pub fn moe_sibling(path: &Path, want_high: bool) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string();
    let (from, to): (&[&str], &str) = if want_high {
        (&["LowNoise", "lownoise", "low_noise", "low-noise"], "HighNoise")
    } else {
        (&["HighNoise", "highnoise", "high_noise", "high-noise"], "LowNoise")
    };
    for f in from {
        if name.contains(f) {
            // Keep the publisher's own separator style: swapping `low_noise`
            // for `HighNoise` would name a file nobody ships.
            let replacement = match *f {
                "low_noise" | "high_noise" => {
                    if want_high {
                        "high_noise"
                    } else {
                        "low_noise"
                    }
                }
                "low-noise" | "high-noise" => {
                    if want_high {
                        "high-noise"
                    } else {
                        "low-noise"
                    }
                }
                "lownoise" | "highnoise" => {
                    if want_high {
                        "highnoise"
                    } else {
                        "lownoise"
                    }
                }
                _ => to,
            };
            return path.with_file_name(name.replace(f, replacement));
        }
    }
    path.to_path_buf()
}

/// A file that parsed but matched nothing, or didn't parse at all — surfaced by
/// `doctor` so an unused 8 GB file in the models dir is visible rather than
/// silently ignored.
#[derive(Debug, Clone)]
pub struct Unrecognized {
    pub path: PathBuf,
    pub why: String,
}

/// What [`build`] produced, plus what it couldn't place.
#[derive(Debug, Clone, Default)]
pub struct Discovery {
    pub models: Vec<VideoModel>,
    pub unrecognized: Vec<Unrecognized>,
    /// Files that were identified but aren't a model on their own — the shared
    /// encoder, the VAEs, LoRAs. Kept for `doctor`, which is otherwise unable to
    /// explain why a folder with eight files in it produced two models.
    pub components: Vec<Found>,
}

/// Turn a scanned directory into video models.
///
/// The grouping rules, all of which follow from the family:
///
/// - a TI2V-5B takes the 2.2 VAE, everything else takes the 2.1 VAE;
/// - an A14B is a *pair*, so a low-noise expert claims the matching high-noise
///   file and the high-noise file is not listed on its own;
/// - every Wan family shares the one umt5 encoder, which is why it is picked
///   once here rather than per model.
///
/// When a component isn't in the folder, the model is still listed, pointing at
/// the conventional filename it would have. That is deliberate: `exists` comes
/// out false, the UI greys it out, and `doctor` prints the exact path it wants —
/// so a half-downloaded model is a checklist rather than a silence.
pub fn build(dir: &Path, found: &[Found]) -> Discovery {
    let mut d = Discovery::default();

    let pick = |want: &dyn Fn(&Role) -> bool| -> Option<PathBuf> {
        found
            .iter()
            .find(|f| want(&f.role))
            .map(|f| f.path.clone())
    };
    let umt5 = pick(&|r: &Role| matches!(r, Role::Umt5));
    let vae21 = pick(&|r: &Role| matches!(r, Role::Vae(VaeKind::Wan21)));
    let vae22 = pick(&|r: &Role| matches!(r, Role::Vae(VaeKind::Wan22)));
    let qwen = pick(&|r: &Role| matches!(r, Role::QwenVl));
    let clip_vision = pick(&|r: &Role| matches!(r, Role::ClipVision));

    // Where a component we don't have would go. The role subdirectories are the
    // layout `README.md` documents, so naming a missing file inside one is also
    // telling the reader where to put it.
    let expect = |sub: &str, file: &str| dir.join(sub).join(file);
    let umt5_path = umt5
        .clone()
        .unwrap_or_else(|| expect("text_encoders", "umt5-xxl-encoder-Q8_0.gguf"));
    let vae21_path = vae21
        .clone()
        .unwrap_or_else(|| expect("vae", "wan_2.1_vae.safetensors"));
    let vae22_path = vae22
        .clone()
        .unwrap_or_else(|| expect("vae", "wan2.2_vae.safetensors"));

    for f in found {
        match &f.role {
            Role::Dit(kind) => {
                if let Some(m) = model_for(f, kind, &umt5_path, &vae21_path, &vae22_path, &qwen, &clip_vision, found) {
                    d.models.push(m);
                }
            }
            Role::Unknown(why) => d.unrecognized.push(Unrecognized {
                path: f.path.clone(),
                why: why.clone(),
            }),
            Role::Unsupported(_) => d.unrecognized.push(Unrecognized {
                path: f.path.clone(),
                why: f.role.describe(),
            }),
            _ => d.components.push(f.clone()),
        }
    }
    d.models.sort_by(|a, b| a.name.cmp(&b.name));
    d
}

/// One transformer to one `VideoModel`, or `None` when the file is part of
/// another model rather than a model itself.
#[allow(clippy::too_many_arguments)]
fn model_for(
    f: &Found,
    kind: &DitKind,
    umt5: &Path,
    vae21: &Path,
    vae22: &Path,
    qwen: &Option<PathBuf>,
    clip_vision: &Option<PathBuf>,
    found: &[Found],
) -> Option<VideoModel> {
    let stem = f
        .path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("video-model")
        .to_string();

    let (family, name, high_noise) = match kind {
        DitKind::Wan22Ti2v5b => (VideoFamily::Wan22Ti2v5b, stem, None),
        DitKind::WanVace { .. } => (VideoFamily::Wan21Vace, stem, None),
        DitKind::MinimaxH3 => (VideoFamily::MinimaxH3, stem, None),
        DitKind::Wan22A14b => {
            let expert = |high: bool| {
                found.iter().find(|o| {
                    matches!(o.role, Role::Dit(DitKind::Wan22A14b))
                        && noise_marker(&o.path) == Some(high)
                })
            };
            match noise_marker(&f.path) {
                // The low-noise expert leads the entry and claims its partner:
                // whichever high-noise file is in the folder, or — when none has
                // been downloaded yet — the name it would have, so the pair
                // lists as one model missing one file.
                Some(false) => {
                    let high = expert(true)
                        .map(|o| o.path.clone())
                        .unwrap_or_else(|| moe_sibling(&f.path, true));
                    // Name the pair, not the half. `LowNoise` in the filename is
                    // an implementation detail of a thing the user thinks of as
                    // one model.
                    (VideoFamily::Wan22A14b, strip_noise_marker(&stem), Some(high))
                }
                // The high-noise expert is normally claimed by its partner. It
                // only leads when the low-noise half isn't here yet, and then it
                // points `diffusion_model` at the file still to be fetched.
                Some(true) => {
                    if expert(false).is_some() {
                        return None;
                    }
                    let mut m = VideoModel::discovered(
                        strip_noise_marker(&stem),
                        VideoFamily::Wan22A14b,
                        moe_sibling(&f.path, false),
                    );
                    m.high_noise_diffusion_model = Some(f.path.clone());
                    m.vae = vae21.to_path_buf();
                    m.t5xxl = Some(umt5.to_path_buf());
                    m.description = Some(describe(kind, &f.path));
                    return Some(m);
                }
                // No marker: which expert this is isn't knowable, so it is not
                // treated as half of anything. Run alone it is a valid dense
                // model, and pairing it with something is a `[[video]]` entry —
                // a decision with information behind it, which this isn't.
                None => (VideoFamily::WanDense, stem, None),
            }
        }
        // A Wan transformer of some other width — a 2.1 T2V 14B or 1.3B. Dense,
        // on the 2.1 VAE, which is exactly what `WanDense` describes.
        DitKind::WanOther { .. } => (VideoFamily::WanDense, stem, None),
    };

    let mut m = VideoModel::discovered(name, family, f.path.clone());
    m.high_noise_diffusion_model = high_noise;
    match family {
        VideoFamily::Wan22Ti2v5b => {
            m.vae = vae22.to_path_buf();
            m.t5xxl = Some(umt5.to_path_buf());
        }
        VideoFamily::MinimaxH3 => {
            m.vae = vae22.to_path_buf();
            m.llm = qwen.clone();
        }
        _ => {
            m.vae = vae21.to_path_buf();
            m.t5xxl = Some(umt5.to_path_buf());
        }
    }
    // Wan 2.1 I2V wants a CLIP-Vision tower; 2.2 doesn't. Attaching it whenever
    // one is present would add a component that then reads as "missing" for
    // every 2.2 model in the folder, so it is only offered to the family that
    // takes one.
    if matches!(family, VideoFamily::Wan21Vace) && clip_vision.is_some() {
        m.clip_vision = clip_vision.clone();
    }
    m.description = Some(describe(kind, &f.path));
    Some(m)
}

/// The separate files an image family runs with, picked from a scan.
///
/// `None` for a family that takes none — a UNet checkpoint is bundled, and so
/// is anything that carries its own VAE and encoder — and for a family that
/// isn't runnable yet. Otherwise every slot the family needs is filled: with
/// the file the scan found, or, when there isn't one, with the conventional
/// path inside the role folder it belongs in. That is `discover::build`'s rule
/// for video — the model is listed greyed out, and `doctor` and the error
/// print a path that is also the instruction.
///
/// The encoder is picked by **width**, not just by kind: Qwen3 comes in 0.6B
/// to 32B, and only the 2560-wide 4B conditions a Z-Image.
pub fn image_components(
    family: ImageFamily,
    bundled: bool,
    found: &[Found],
    models_dir: &Path,
) -> Option<Components> {
    // A bundled checkpoint has its own, and a slot on it is an override that
    // somebody picks — never one the scan helps itself to. A bare UNet was
    // never assembled (it would need CLIP-L and CLIP-G slots too) and still
    // isn't.
    if bundled || family.is_unet() || family.slots().is_empty() {
        return None;
    }
    let mut parts = Components::default();
    for slot in family.slots() {
        let path = found
            .iter()
            .find(|f| (slot.fits)(&f.role))
            .map(|f| f.path.clone())
            .unwrap_or_else(|| models_dir.join(slot.dir).join(slot.file));
        parts.set(slot.key, Some(path));
    }
    Some(parts)
}

/// Drop the expert marker from a pair's name.
fn strip_noise_marker(stem: &str) -> String {
    let mut s = stem.to_string();
    for m in [
        "-LowNoise", "_LowNoise", "-lownoise", "_low_noise", "-low-noise", "LowNoise", "low_noise",
    ] {
        s = s.replace(m, "");
    }
    // A marker in the middle leaves a doubled separator behind.
    s = s.replace("--", "-").replace("__", "_");
    let trimmed = s.trim_matches(['-', '_', ' ']).to_string();
    if trimmed.is_empty() {
        stem.to_string()
    } else {
        trimmed
    }
}

/// The one-line description a discovered model carries into the UI.
fn describe(kind: &DitKind, path: &Path) -> String {
    match kind {
        DitKind::Wan22Ti2v5b => "Wan 2.2 TI2V 5B — one dense transformer".to_string(),
        DitKind::Wan22A14b => match noise_marker(path) {
            Some(true) => "Wan 2.2 A14B high-noise expert".to_string(),
            Some(false) => "Wan 2.2 A14B — mixture-of-experts pair".to_string(),
            // Worth spelling out on the model itself: this is the case where the
            // file could be either expert and is being run on its own.
            None => "Wan 2.2 A14B expert, unpaired (the filename doesn't say                      which half; name both in a [[video]] entry to run them as a pair)"
                .to_string(),
        },
        DitKind::WanVace { dim } => format!("Wan VACE (width {dim}) — frames in, frames out"),
        DitKind::WanOther { dim } => format!("Wan transformer, width {dim} — dense"),
        DitKind::MinimaxH3 => "MiniMax H3".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(names: &[(&str, &[u64])]) -> Tensors {
        Tensors {
            tensors: names
                .iter()
                .map(|(n, s)| TensorInfo {
                    name: n.to_string(),
                    shape: s.to_vec(),
                    dtype: "f16".into(),
                })
                .collect(),
            layout: Layout::PyTorch,
        }
    }

    #[test]
    fn the_five_b_is_told_from_the_a14b_by_width() {
        let five = t(&[
            ("blocks.0.cross_attn.norm_k.weight", &[3072]),
            ("blocks.0.self_attn.q.weight", &[3072, 3072]),
        ]);
        let big = t(&[
            ("blocks.0.cross_attn.norm_k.weight", &[5120]),
            ("blocks.0.self_attn.q.weight", &[5120, 5120]),
        ]);
        assert_eq!(classify(&five), Role::Dit(DitKind::Wan22Ti2v5b));
        assert_eq!(classify(&big), Role::Dit(DitKind::Wan22A14b));
    }

    #[test]
    fn a_vae_survives_the_trip_through_gguf() {
        // The regression the convert tab found in its first minute. sd.cpp's
        // exporter has only four dimensions to work with, so the 5-D Conv3d
        // weight `[16, 16, 1, 1, 1]` comes back as `[1, 1, 1, 256]` — the
        // element count kept, the 16 no longer readable anywhere in it. Reading
        // the width off the one-dimensional bias instead is proof against that
        // *and* against the layout flip, and it is what the real converted file
        // classifies by: before this, `wan_2.1_vae-f16.gguf` came back as "a VAE
        // whose latent width could not be read".
        let converted = Tensors {
            tensors: vec![
                TensorInfo {
                    name: "vae.decoder.conv1.weight".into(),
                    shape: vec![3, 3, 3, 16],
                    dtype: "f16".into(),
                },
                TensorInfo {
                    name: "vae.conv2.weight".into(),
                    shape: vec![1, 1, 1, 256],
                    dtype: "f16".into(),
                },
                TensorInfo {
                    name: "vae.conv2.bias".into(),
                    shape: vec![16],
                    dtype: "f32".into(),
                },
            ],
            layout: Layout::Ggml,
        };
        assert_eq!(classify(&converted), Role::Vae(VaeKind::Wan21));
    }

    #[test]
    fn the_vae_width_still_reads_without_a_bias() {
        // The fallback has to keep working: a safetensors VAE whose biases were
        // stripped still has the square weight to read.
        let no_bias = t(&[
            ("decoder.conv1.weight", &[96]),
            ("conv2.weight", &[48, 48, 1, 1, 1]),
        ]);
        assert_eq!(classify(&no_bias), Role::Vae(VaeKind::Wan22));
    }

    #[test]
    fn vace_wins_over_the_width_rule() {
        // A VACE checkpoint is also a Wan transformer of some width; the control
        // blocks are the thing that decides what it can do.
        let vace = t(&[
            ("blocks.0.cross_attn.norm_k.weight", &[1536]),
            ("blocks.0.self_attn.q.weight", &[1536, 1536]),
            ("vace_blocks.0.after_proj.weight", &[1536, 1536]),
        ]);
        assert_eq!(classify(&vace), Role::Dit(DitKind::WanVace { dim: 1536 }));
    }

    #[test]
    fn the_two_vaes_are_told_apart_by_latent_width() {
        let v21 = t(&[("decoder.conv1.weight", &[96]), ("conv2.weight", &[16, 16, 1, 1, 1])]);
        let v22 = t(&[("decoder.conv1.weight", &[160]), ("conv2.weight", &[48, 48, 1, 1, 1])]);
        assert_eq!(classify(&v21), Role::Vae(VaeKind::Wan21));
        assert_eq!(classify(&v22), Role::Vae(VaeKind::Wan22));
    }

    #[test]
    fn an_encoder_is_not_a_vae_despite_both_having_one() {
        // Both start their tensor names with `encoder.`, which is exactly the
        // collision that makes filename matching look reasonable until it isn't.
        let umt5 = t(&[
            ("encoder.block.0.layer.0.SelfAttention.q.weight", &[4096, 4096]),
            ("encoder.block.0.layer.1.DenseReluDense.wi_0.weight", &[10240, 4096]),
        ]);
        assert_eq!(classify(&umt5), Role::Umt5);
    }

    #[test]
    fn the_file_called_minimax_h3_is_a_qwen_encoder() {
        // The real one from models/video/: 2054 tensors, a causal LM plus a
        // vision tower. Named for a video model, and not one.
        let qwen = t(&[
            ("model.embed_tokens.weight", &[151936, 5120]),
            ("model.layers.0.self_attn.q_proj.weight", &[8192, 2560]),
            ("visual.patch_embed.proj.weight", &[1152, 3, 2, 16, 16]),
        ]);
        assert_eq!(classify(&qwen), Role::QwenVl);
    }

    #[test]
    fn a_lora_is_not_the_model_it_patches() {
        let lora = t(&[
            ("blocks.0.cross_attn.norm_k.weight.lora_down.weight", &[32, 5120]),
            ("blocks.0.cross_attn.norm_k.weight.lora_up.weight", &[5120, 32]),
        ]);
        assert_eq!(classify(&lora), Role::Lora(LoraBase::Other("Wan")));
    }

    /// LyCORIS carries the base model's tensor names exactly as a LoRA does, so
    /// this one would classify as a Wan transformer without its own marker — and
    /// then be offered for conversion and counted as a model.
    #[test]
    fn a_lycoris_is_still_a_lora() {
        let loha = t(&[
            ("blocks.0.cross_attn.norm_k.weight", &[5120, 5120]),
            ("blocks.0.cross_attn.norm_k.hada_w1_a", &[32, 5120]),
            ("blocks.0.cross_attn.norm_k.hada_w1_b", &[5120, 32]),
        ]);
        assert_eq!(classify(&loha), Role::Lora(LoraBase::Other("Wan")));

        let lokr = t(&[
            ("blocks.0.cross_attn.norm_k.weight", &[5120, 5120]),
            ("blocks.0.cross_attn.norm_k.lokr_w1", &[64, 64]),
            ("blocks.0.cross_attn.norm_k.lokr_w2", &[80, 80]),
        ]);
        assert_eq!(classify(&lokr), Role::Lora(LoraBase::Other("Wan")));
    }

    /// One case per LoRA in the models dir, keys and shapes as they are on disk,
    /// plus the spellings the other trainers use for the families here.
    #[test]
    fn a_lora_names_the_base_it_patches() {
        let base = |names: &[(&str, &[u64])]| match classify(&t(names)) {
            Role::Lora(b) => b,
            other => panic!("not a LoRA: {other:?}"),
        };
        // `hand 5.5`: metadata says SD 1, the cross-attention says SDXL.
        assert_eq!(
            base(&[
                ("lora_te1_text_model_encoder_layers_0_mlp_fc1.lora_down.weight", &[128, 768]),
                ("lora_unet_input_blocks_4_1_transformer_blocks_0_attn2_to_k.lora_down.weight", &[128, 2048]),
            ]),
            LoraBase::Image(ImageFamily::Sdxl)
        );
        // `dark`: diffusers block names, self-attention only, two text encoders.
        assert_eq!(
            base(&[
                ("lora_te1_text_model_encoder_layers_0_self_attn_k_proj.lora_down.weight", &[16, 768]),
                ("lora_te2_text_model_encoder_layers_0_self_attn_k_proj.lora_down.weight", &[16, 1280]),
                ("lora_unet_down_blocks_1_attentions_0_transformer_blocks_0_attn1_to_k.lora_down.weight", &[16, 640]),
            ]),
            LoraBase::Image(ImageFamily::Sdxl)
        );
        assert_eq!(
            base(&[("lora_unet_down_blocks_1_attentions_0_transformer_blocks_0_attn2_to_k.lora_down.weight", &[8, 768])]),
            LoraBase::Image(ImageFamily::Sd1)
        );
        assert_eq!(
            base(&[("lora_te_text_model_encoder_layers_0_self_attn_k_proj.lora_down.weight", &[8, 1024])]),
            LoraBase::Image(ImageFamily::Sd2)
        );
        // The Krea 2 one: peft's lora_A/lora_B under sd.cpp's own names.
        assert_eq!(
            base(&[
                ("diffusion_model.blocks.0.attn.wq.lora_A.weight", &[32, 6144]),
                ("diffusion_model.txtfusion.refiner_blocks.0.attn.wq.lora_A.weight", &[32, 2560]),
            ]),
            LoraBase::Other("Krea 2")
        );
        for zimage in [
            "diffusion_model.layers.0.attention.to_q.lora_A.weight",
            "lora_unet_layers_0_attention_qkv.lora_down.weight",
            "diffusion_model.layers.3.feed_forward.w1.lora_A.weight",
        ] {
            assert_eq!(base(&[(zimage, &[16, 3840])]), LoraBase::Image(ImageFamily::ZImage), "{zimage}");
        }
        for flux in [
            "lora_unet_double_blocks_0_img_attn_qkv.lora_down.weight",
            "transformer.single_transformer_blocks.0.attn.to_q.lora_A.weight",
            "transformer.transformer_blocks.0.attn.add_q_proj.lora_A.weight",
        ] {
            assert_eq!(base(&[(flux, &[16, 3072])]), LoraBase::Image(ImageFamily::Flux), "{flux}");
        }
        assert_eq!(
            base(&[
                ("transformer_blocks.0.attn.add_q_proj.lora_A.weight", &[16, 3072]),
                ("transformer_blocks.0.img_mlp.net.0.proj.lora_A.weight", &[16, 3072]),
            ]),
            LoraBase::Image(ImageFamily::QwenImage)
        );
        assert_eq!(base(&[("mystery.lora_down.weight", &[4, 4])]), LoraBase::Unknown);
    }

    #[test]
    fn only_a_lora_for_the_family_fits_it() {
        let sdxl = LoraBase::Image(ImageFamily::Sdxl);
        assert!(sdxl.fits(ImageFamily::Sdxl));
        assert!(!sdxl.fits(ImageFamily::ZImage));
        assert!(!sdxl.fits(ImageFamily::Sd1));
        assert!(!LoraBase::Other("Krea 2").fits(ImageFamily::Sdxl));
        assert!(LoraBase::Unknown.fits(ImageFamily::ZImage));
    }

    #[test]
    fn an_unknown_file_says_so_rather_than_guessing() {
        let junk = t(&[("something.weight", &[8])]);
        assert!(matches!(classify(&junk), Role::Unknown(_)));
    }

    /// The three UNet families differ in the width of the text they attend to,
    /// and a bundled checkpoint's VAE must not pull it into the VAE branch —
    /// its `decoder.…conv2` tensors are exactly what that branch looks for.
    #[test]
    fn a_unet_is_told_apart_by_its_text_width_and_its_vae_does_not_leak() {
        let unet = |context: u64| {
            t(&[
                ("model.diffusion_model.input_blocks.0.0.weight", &[320, 4, 3, 3]),
                (
                    "model.diffusion_model.output_blocks.3.1.transformer_blocks.0.attn2.to_k.weight",
                    &[640, context],
                ),
                ("first_stage_model.decoder.conv_in.weight", &[512, 4, 3, 3]),
                ("first_stage_model.decoder.up.0.block.0.conv2.bias", &[128]),
                ("cond_stage_model.transformer.text_model.final_layer_norm.weight", &[768]),
            ])
        };
        for (context, family) in [
            (768, ImageFamily::Sd1),
            (1024, ImageFamily::Sd2),
            (2048, ImageFamily::Sdxl),
        ] {
            assert_eq!(
                classify(&unet(context)),
                Role::Image { family, bundled: true },
                "context width {context}"
            );
        }
        assert!(matches!(classify(&unet(512)), Role::Unknown(_)));
    }

    /// `inner_dim` rather than a guess at which end of the shape to read: the
    /// same `to_k` quantized to GGUF is stored the other way round.
    #[test]
    fn a_gguf_unet_reads_the_same_width() {
        let mut g = t(&[
            ("model.diffusion_model.input_blocks.0.0.weight", &[3, 3, 4, 320]),
            ("input_blocks.4.1.transformer_blocks.0.attn2.to_k.weight", &[2048, 640]),
        ]);
        g.layout = Layout::Ggml;
        assert_eq!(
            classify(&g),
            Role::Image { family: ImageFamily::Sdxl, bundled: false }
        );
    }

    #[test]
    fn the_dits_are_named_by_their_own_markers() {
        let z = t(&[("cap_embedder.0.weight", &[3840, 2560]), ("layers.0.attention.qkv.weight", &[11520, 3840])]);
        assert_eq!(
            classify(&z),
            Role::Image { family: ImageFamily::ZImage, bundled: false }
        );
        let qwen = t(&[
            ("transformer_blocks.0.img_mod.1.weight", &[18432, 3072]),
            ("img_in.weight", &[3072, 64]),
        ]);
        assert_eq!(
            classify(&qwen),
            Role::Image { family: ImageFamily::QwenImage, bundled: false }
        );
        let flux = t(&[("double_blocks.0.img_attn.qkv.weight", &[9216, 3072]), ("img_in.weight", &[3072, 64])]);
        assert_eq!(
            classify(&flux),
            Role::Image { family: ImageFamily::Flux, bundled: false }
        );
        // Flux's layout with a different input width is a variant that needs
        // inputs this build doesn't send.
        let fill = t(&[("double_blocks.0.img_attn.qkv.weight", &[9216, 3072]), ("img_in.weight", &[3072, 384])]);
        assert!(matches!(classify(&fill), Role::Unsupported(_)));
    }

    /// The file that started this: an LTX checkpoint in the image root.
    #[test]
    fn an_ltx_transformer_is_named_and_is_not_an_image_model() {
        let ltx = t(&[
            ("model.diffusion_model.adaln_single.emb.timestep_embedder.linear_1.bias", &[4096]),
            ("model.diffusion_model.transformer_blocks.0.attn2.to_k.weight", &[4096, 4096]),
        ]);
        match classify(&ltx) {
            Role::Unsupported(what) => assert!(what.contains("LTX"), "{what}"),
            other => panic!("{other:?}"),
        }
        // Hunyuan Video carries Flux's `double_blocks` and must not be read as a
        // Flux.
        let hunyuan = t(&[
            ("double_blocks.0.img_attn_qkv.weight", &[9216, 3072]),
            ("txt_in.individual_token_refiner.blocks.0.adaLN_modulation.1.weight", &[6144, 3072]),
        ]);
        assert!(matches!(classify(&hunyuan), Role::Unsupported(_)));
    }

    /// An LDM VAE by latent width, and not a Wan VAE despite sharing its
    /// `decoder.`/`conv2.` names.
    #[test]
    fn an_image_vae_is_told_by_its_latent_width() {
        let vae = |two_z: u64| {
            t(&[
                ("decoder.conv_in.weight", &[512, two_z / 2, 3, 3]),
                ("decoder.mid.block_1.conv2.bias", &[512]),
                ("encoder.conv_out.bias", &[two_z]),
            ])
        };
        assert_eq!(classify(&vae(8)), Role::ImageVae(ImageVaeKind::Sd));
        assert_eq!(classify(&vae(32)), Role::ImageVae(ImageVaeKind::Flux));
        assert!(matches!(classify(&vae(64)), Role::Unsupported(_)));
    }

    /// A DiT is only offered once it has both halves: a recipe, since the
    /// user's settings are SDXL's, and the parts it is assembled from. Turning
    /// a family on without either is what this refuses.
    #[test]
    fn a_runnable_dit_has_a_recipe_and_its_parts() {
        let dir = Path::new("/m");
        for f in ImageFamily::ALL {
            if !f.runnable() || f.is_unet() {
                continue;
            }
            assert!(f.recipe().steps.is_some(), "{f:?} is runnable with no recipe");
            let parts = image_components(f, false, &[], dir);
            assert!(parts.is_some(), "{f:?} is runnable with no components");
        }
    }

    /// Found parts are used; a missing one is named at the conventional path
    /// inside its role folder. And the encoder is picked by width — a Qwen3 of
    /// the wrong size is not Z-Image's.
    #[test]
    fn a_z_image_takes_the_right_encoder_or_names_where_it_goes() {
        let dir = Path::new("/m");
        let found = [
            Found {
                path: dir.join("misc/ae.safetensors"),
                role: Role::ImageVae(ImageVaeKind::Flux),
            },
            Found {
                path: dir.join("big_qwen.safetensors"),
                role: Role::Llm(LlmKind::Qwen3 { dim: 4096 }),
            },
        ];
        let c = image_components(ImageFamily::ZImage, false, &found, dir).unwrap();
        assert_eq!(c.vae, Some(dir.join("misc/ae.safetensors")));
        assert_eq!(c.llm, Some(dir.join("text_encoders/qwen_3_4b.safetensors")));
        // Bundled needs nothing, whatever the family.
        assert_eq!(image_components(ImageFamily::ZImage, true, &found, dir), None);
    }

    #[test]
    fn a_qwen3_encoder_is_read_in_either_spelling() {
        let hf = t(&[
            ("model.embed_tokens.weight", &[151936, 2560]),
            ("model.layers.0.self_attn.q_norm.weight", &[128]),
            ("model.norm.weight", &[2560]),
        ]);
        assert_eq!(classify(&hf), Role::Llm(LlmKind::Qwen3 { dim: 2560 }));
        let gguf = t(&[
            ("token_embd.weight", &[2560, 151936]),
            ("blk.0.attn_q_norm.weight", &[128]),
            ("output_norm.weight", &[2560]),
        ]);
        assert_eq!(classify(&gguf), Role::Llm(LlmKind::Qwen3 { dim: 2560 }));
    }

    #[test]
    fn a_square_dim_reads_the_same_in_either_layout() {
        // The point of using square tensors: safetensors stores [out, in] and
        // GGUF stores [in, out], and this must not care.
        let a = t(&[("blocks.0.self_attn.q.weight", &[5120, 5120])]);
        assert_eq!(a.square_dim("self_attn.q.weight"), Some(5120));
        // A rectangular one is refused rather than read off the wrong end.
        let b = t(&[("blocks.0.self_attn.q.weight", &[10240, 5120])]);
        assert_eq!(b.square_dim("self_attn.q.weight"), None);
    }

    #[test]
    fn the_moe_pair_is_named_from_the_filename_and_only_there() {
        let low = Path::new("/m/Wan2.2-I2V-A14B-LowNoise-Q4_K_M.gguf");
        let high = Path::new("/m/Wan2.2-I2V-A14B-HighNoise-Q4_K_M.gguf");
        assert_eq!(noise_marker(low), Some(false));
        assert_eq!(noise_marker(high), Some(true));
        assert_eq!(moe_sibling(low, true), high);
        assert_eq!(moe_sibling(high, false), low);
        // The publisher's separator style survives the swap.
        let snake = Path::new("/m/wan2.2_t2v_low_noise_14B_fp8.safetensors");
        assert_eq!(
            moe_sibling(snake, true),
            Path::new("/m/wan2.2_t2v_high_noise_14B_fp8.safetensors")
        );
    }

    #[test]
    fn an_unmarked_expert_is_run_alone_rather_than_paired_with_itself() {
        // The real file that prompted this: a civitai repackaging of a Wan 2.2
        // I2V A14B expert whose name says nothing about which half it is.
        // Treating it as a pair would pass one file as both experts.
        let path = std::path::PathBuf::from("/m/wan22I2vA14bScaledFp8_v10_3169009.safetensors");
        assert_eq!(noise_marker(&path), None);
        let found = vec![Found {
            path: path.clone(),
            role: Role::Dit(DitKind::Wan22A14b),
        }];
        let d = build(Path::new("/m"), &found);
        assert_eq!(d.models.len(), 1);
        let m = &d.models[0];
        assert_eq!(m.family, VideoFamily::WanDense);
        assert_eq!(m.high_noise_diffusion_model, None);
        assert_eq!(m.diffusion_model, path);
    }

    #[test]
    fn a_marked_pair_becomes_one_model_and_a_lone_half_names_the_other() {
        let low = std::path::PathBuf::from("/m/Wan2.2-I2V-A14B-LowNoise-Q4_K_M.gguf");
        let high = std::path::PathBuf::from("/m/Wan2.2-I2V-A14B-HighNoise-Q4_K_M.gguf");
        let dit = |p: &std::path::Path| Found {
            path: p.to_path_buf(),
            role: Role::Dit(DitKind::Wan22A14b),
        };

        // Both halves present: one model, not two.
        let d = build(Path::new("/m"), &[dit(&low), dit(&high)]);
        assert_eq!(d.models.len(), 1);
        assert_eq!(d.models[0].family, VideoFamily::Wan22A14b);
        assert_eq!(d.models[0].diffusion_model, low);
        assert_eq!(d.models[0].high_noise_diffusion_model, Some(high.clone()));
        // Named for the model, not for the half that happened to lead.
        assert!(!d.models[0].name.to_lowercase().contains("lownoise"));

        // Only the high half downloaded: still one model, and it names the file
        // still to fetch — which is the download-checklist property.
        let d = build(Path::new("/m"), &[dit(&high)]);
        assert_eq!(d.models.len(), 1);
        assert_eq!(d.models[0].diffusion_model, low);
        assert_eq!(d.models[0].high_noise_diffusion_model, Some(high));
    }

    #[test]
    fn a_missing_component_is_named_where_it_should_go() {
        // Nothing in the folder but the transformer: the VAE and encoder it
        // needs are still listed, under the role directory they belong in, so
        // `doctor` prints a path that is also an instruction.
        let dit = Found {
            path: PathBuf::from("/m/diffusion_models/wan5b.safetensors"),
            role: Role::Dit(DitKind::Wan22Ti2v5b),
        };
        let d = build(Path::new("/m"), &[dit]);
        let m = &d.models[0];
        assert_eq!(m.vae, PathBuf::from("/m/vae/wan2.2_vae.safetensors"));
        assert_eq!(
            m.t5xxl,
            Some(PathBuf::from("/m/text_encoders/umt5-xxl-encoder-Q8_0.gguf"))
        );
    }

    #[test]
    fn a_pair_is_named_for_the_model_not_the_half() {
        assert_eq!(
            strip_noise_marker("Wan2.2-I2V-A14B-LowNoise-Q4_K_M"),
            "Wan2.2-I2V-A14B-Q4_K_M"
        );
    }

    #[test]
    fn a_safetensors_header_round_trips() {
        let dir = std::env::temp_dir().join(format!("rustimage-disc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tiny.safetensors");
        let json = br#"{"__metadata__":{"x":"y"},"encoder.block.0.layer.0.SelfAttention.q.weight":{"dtype":"F16","shape":[8,8],"data_offsets":[0,128]}}"#;
        let mut buf = (json.len() as u64).to_le_bytes().to_vec();
        buf.extend_from_slice(json);
        buf.extend_from_slice(&[0u8; 128]);
        std::fs::write(&path, &buf).unwrap();

        let t = read_tensors(&path).unwrap();
        // __metadata__ is not a tensor and must not be counted as one.
        assert_eq!(t.tensors.len(), 1);
        assert_eq!(classify(&t), Role::Umt5);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_truncated_file_is_unreadable_rather_than_misread() {
        let dir = std::env::temp_dir().join(format!("rustimage-disc-t-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("partial.safetensors");
        // A header length that promises far more than the file holds — what an
        // interrupted download looks like.
        let mut buf = 4096u64.to_le_bytes().to_vec();
        buf.extend_from_slice(b"{}");
        std::fs::write(&path, &buf).unwrap();
        assert!(read_tensors(&path).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
