//! Settings inside a PNG: written into every saved output, read back from any
//! PNG dropped on the prompt box.
//!
//! Two text chunks, both written before the first `IDAT` (PIL-based readers —
//! A1111's PNG Info among them — only collect text seen before the image data):
//!
//! - **`parameters`**, A1111's layout: the prompt, a `Negative prompt:` line and a
//!   last line of `Key: value` pairs. Every tool in the ecosystem reads it —
//!   A1111, Forge, Civitai — so it is what a shared picture is understood by.
//! - **`yasdig`**, a JSON record that restores every box the Image tab has,
//!   which `parameters` can't (refine and hi-res settings, the typed prompt
//!   before its `{variables}` rolled, the LoRA picker).
//!
//! **The `yasdig` record is curated, never the sidecar.** A [`Profile`] holds
//! absolute paths — the checkpoint, each part, possibly `extra_args` — and a PNG
//! is a file people post. [`Embedded`] lists exactly what the page restores and
//! nothing that names a place on this disk; `the_record_never_carries_a_path`
//! pins it.
//!
//! **Read on the server, not the page**, so the sampler-name table below exists
//! once. The server also knows what is installed, so a model or LoRA this
//! install hasn't got becomes a note instead of a setting the next Generate
//! would be refused over.
//!
//! No dependency: a PNG chunk is a length, a type, the data and a CRC-32 of the
//! type and data, and the CRC is a dozen lines.

use crate::config::{LoraRef, Profile};
use crate::workitems::WorkItem;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

const SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
const PARAMETERS: &str = "parameters";
const YASDIG: &str = "yasdig";

/// The settings a PNG carries for yasdig: what `applyWorkItem` on the page
/// restores, in its shape, and nothing with a path in it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Embedded {
    /// The record's version, for whoever has to read an old one.
    pub v: u32,
    pub prompt: String,
    pub negative: Option<String>,
    pub typed_prompt: Option<String>,
    pub typed_negative: Option<String>,
    pub seed: i64,
    /// The registry name of the model that made it.
    pub template: String,
    pub profile: EmbeddedProfile,
}

/// The knobs of a [`Profile`] that describe a picture, not a machine. No
/// `model`, `components`, `extra_args` (paths), no `memory` or reserve (how a
/// run fit *here*), no `references` or source (stems on someone else's disk).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EmbeddedProfile {
    pub width: u32,
    pub height: u32,
    pub steps: u32,
    pub cfg_scale: f32,
    pub sampler: String,
    pub scheduler: String,
    pub cache_mode: String,
    pub upscale_factor: f32,
    pub upscaler: Option<String>,
    pub refine_steps: u32,
    pub refine_strength: f32,
    pub refine_cfg_scale: Option<f32>,
    pub hires_steps: u32,
    pub hires_strength: f32,
    pub hires_cfg_scale: Option<f32>,
    pub refine_model: Option<String>,
    pub hires_model: Option<String>,
    pub source_strength: f32,
    /// Names relative to the LoRA folder — a name, not a path on this disk.
    pub loras: Vec<LoraRef>,
    /// MOSS-Image's layout boxes: fractions of the frame and words, nothing
    /// of this machine's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layout: Option<crate::layout::Layout>,
}

impl EmbeddedProfile {
    fn of(p: &Profile) -> EmbeddedProfile {
        EmbeddedProfile {
        width: p.width,
        height: p.height,
        steps: p.steps,
        cfg_scale: p.cfg_scale,
        sampler: p.sampler.clone(),
        scheduler: p.scheduler.clone(),
        cache_mode: p.cache_mode.clone(),
        upscale_factor: p.upscale_factor,
        upscaler: p.upscaler.clone(),
        refine_steps: p.refine_steps,
        refine_strength: p.refine_strength,
        refine_cfg_scale: p.refine_cfg_scale,
        hires_steps: p.hires_steps,
        hires_strength: p.hires_strength,
        hires_cfg_scale: p.hires_cfg_scale,
        refine_model: p.refine_model.clone(),
        hires_model: p.hires_model.clone(),
        source_strength: p.source_strength,
        loras: p.loras.clone(),
        layout: p.layout.clone(),
    }
    }
}

/// A field missing from a record (one written by another version) takes the
/// built-in setting, rather than the whole record failing to parse and the PNG
/// being read as an A1111 one without a word.
impl Default for EmbeddedProfile {
    fn default() -> Self {
        EmbeddedProfile::of(&Profile::default())
    }
}

impl Embedded {
    pub fn from_item(item: &WorkItem) -> Embedded {
        let p: &Profile = &item.profile;
        Embedded {
            v: 1,
            prompt: item.prompt.clone(),
            negative: item.negative.clone(),
            typed_prompt: item.typed_prompt.clone(),
            typed_negative: item.typed_negative.clone(),
            seed: item.seed,
            template: item.template.clone(),
            profile: EmbeddedProfile::of(p),
        }
    }
}

/// sd.cpp's sampler names and A1111's, for the ones both have. A name with no
/// counterpart is written as sd.cpp spells it, and read back only if sd.cpp
/// takes it.
const SAMPLERS: &[(&str, &str)] = &[
    ("euler", "Euler"),
    ("euler_a", "Euler a"),
    ("heun", "Heun"),
    ("dpm2", "DPM2"),
    ("dpm++2s_a", "DPM++ 2S a"),
    ("dpm++2m", "DPM++ 2M"),
    ("dpm++2m_sde", "DPM++ 2M SDE"),
    ("lms", "LMS"),
    ("lcm", "LCM"),
    ("tcd", "TCD"),
    ("ipndm", "iPNDM"),
    ("euler_cfg_pp", "Euler CFG++"),
    ("euler_a_cfg_pp", "Euler a CFG++"),
];

/// The same for schedulers. `auto` is A1111's "Automatic": each picks the
/// model's own.
const SCHEDULERS: &[(&str, &str)] = &[
    ("auto", "Automatic"),
    ("karras", "Karras"),
    ("exponential", "Exponential"),
    ("sgm_uniform", "SGM Uniform"),
    ("simple", "Simple"),
    ("kl_optimal", "KL Optimal"),
    ("beta", "Beta"),
    ("ays", "Align Your Steps"),
];

fn to_a1111<'a>(table: &[(&str, &'a str)], name: &'a str) -> &'a str {
    table.iter().find(|(ours, _)| *ours == name).map(|(_, theirs)| *theirs).unwrap_or(name)
}

/// Theirs to ours: the table first, then sd.cpp's own spelling if `valid`
/// takes it (a yasdig-written name with no A1111 counterpart).
fn from_a1111(table: &[(&str, &str)], name: &str, valid: &[&str]) -> Option<String> {
    let name = name.trim();
    table
        .iter()
        .find(|(_, theirs)| theirs.eq_ignore_ascii_case(name))
        .map(|(ours, _)| ours.to_string())
        .or_else(|| valid.contains(&name).then(|| name.to_string()))
}

/// The file stem of a LoRA name: `sdxl/detail.safetensors` → `detail`, what an
/// A1111 `<lora:…>` tag names.
fn lora_stem(name: &str) -> &str {
    let file = name.rsplit('/').next().unwrap_or(name);
    file.rsplit_once('.').map(|(s, _)| s).unwrap_or(file)
}

/// The `parameters` text for a work item, in A1111's layout.
pub fn parameters(item: &WorkItem) -> String {
    let p = &item.profile;
    let mut prompt = item.prompt.clone();
    // How Civitai and A1111 know a LoRA was used: a tag in the prompt. Ours
    // travel on the request instead, so they are written out here.
    for l in &p.loras {
        prompt.push_str(&format!(" <lora:{}:{}>", lora_stem(&l.name), l.strength));
    }
    let mut out = prompt;
    if let Some(n) = item.negative.as_deref().filter(|n| !n.is_empty()) {
        out.push_str("\nNegative prompt: ");
        out.push_str(n);
    }
    let mut fields = vec![
        format!("Steps: {}", p.steps),
        format!("Sampler: {}", to_a1111(SAMPLERS, &p.sampler)),
    ];
    if p.scheduler != crate::config::AUTO_SCHEDULER {
        fields.push(format!("Schedule type: {}", to_a1111(SCHEDULERS, &p.scheduler)));
    }
    fields.push(format!("CFG scale: {}", p.cfg_scale));
    fields.push(format!("Seed: {}", item.seed));
    fields.push(format!("Size: {}x{}", p.width, p.height));
    fields.push(format!("Model: {}", quoted(&item.template)));
    fields.push(format!("Version: yasdig {}", env!("CARGO_PKG_VERSION")));
    out.push('\n');
    out.push_str(&fields.join(", "));
    out
}

/// A value with a comma or a quote in it is quoted, as A1111 does.
fn quoted(v: &str) -> String {
    if v.contains(',') || v.contains('"') || v.contains(':') {
        format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        v.to_string()
    }
}

// ---- Chunks ----------------------------------------------------------------

fn crc32(bytes: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in bytes {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    !c
}

/// Every chunk as (type, data), checked for bounds. A truncated or non-PNG
/// file is an error, not a partial answer.
fn chunks(png: &[u8]) -> Result<Vec<(&[u8], &[u8])>> {
    if png.len() < 8 || &png[..8] != SIGNATURE {
        bail!("not a PNG file");
    }
    let mut out = Vec::new();
    let mut i = 8;
    while i + 12 <= png.len() {
        let len = u32::from_be_bytes(png[i..i + 4].try_into().unwrap()) as usize;
        let end = i + 12 + len;
        if end > png.len() {
            bail!("the PNG is cut short");
        }
        out.push((&png[i + 4..i + 8], &png[i + 8..i + 8 + len]));
        if &png[i + 4..i + 8] == b"IEND" {
            return Ok(out);
        }
        i = end;
    }
    bail!("the PNG has no end")
}

fn chunk(kind: &[u8], data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 12);
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
    out
}

/// A text chunk: `tEXt` when the value is Latin-1 (what that chunk holds),
/// else an uncompressed `iTXt`, which is UTF-8 — PIL's own rule.
fn text_chunk(key: &str, value: &str, force_utf8: bool) -> Vec<u8> {
    let latin1: Option<Vec<u8>> = value.chars().map(|c| u8::try_from(u32::from(c)).ok()).collect();
    match latin1.filter(|_| !force_utf8) {
        Some(bytes) => {
            let mut data = key.as_bytes().to_vec();
            data.push(0);
            data.extend_from_slice(&bytes);
            chunk(b"tEXt", &data)
        }
        None => {
            // keyword, NUL, compression flag 0, method 0, empty language tag
            // NUL, empty translated keyword NUL, text.
            let mut data = key.as_bytes().to_vec();
            data.extend_from_slice(&[0, 0, 0, 0, 0]);
            data.extend_from_slice(value.as_bytes());
            chunk(b"iTXt", &data)
        }
    }
}

/// The keyword and text of a `tEXt` or uncompressed `iTXt`; `None` for
/// anything else, compressed text included (neither A1111 nor ComfyUI writes
/// it, and inflating it would be a dependency).
fn read_text(kind: &[u8], data: &[u8]) -> Option<(String, String)> {
    let nul = data.iter().position(|&b| b == 0)?;
    let key = String::from_utf8_lossy(&data[..nul]).into_owned();
    match kind {
        b"tEXt" => Some((key, data[nul + 1..].iter().map(|&b| char::from(b)).collect())),
        b"iTXt" => {
            let rest = data.get(nul + 1..)?;
            if rest.first() != Some(&0) {
                return None; // compressed
            }
            let rest = rest.get(2..)?; // flag, method
            let lang = rest.iter().position(|&b| b == 0)?;
            let rest = &rest[lang + 1..];
            let tr = rest.iter().position(|&b| b == 0)?;
            Some((key, String::from_utf8_lossy(&rest[tr + 1..]).into_owned()))
        }
        _ => None,
    }
}

fn is_ours(kind: &[u8], data: &[u8]) -> bool {
    matches!(read_text(kind, data), Some((k, _)) if k == PARAMETERS || k == YASDIG)
}

/// `png` with both text chunks for `item`, before the first `IDAT`. Any of ours
/// already there are replaced, so re-saving never stacks them.
pub fn embed(png: &[u8], item: &WorkItem) -> Result<Vec<u8>> {
    let record = serde_json::to_string(&Embedded::from_item(item))?;
    let mut out = Vec::with_capacity(png.len() + record.len() * 2 + 1024);
    out.extend_from_slice(SIGNATURE);
    let mut written = false;
    for (kind, data) in chunks(png)? {
        if is_ours(kind, data) {
            continue;
        }
        if kind == b"IDAT" && !written {
            out.extend(text_chunk(PARAMETERS, &parameters(item), false));
            out.extend(text_chunk(YASDIG, &record, true));
            written = true;
        }
        out.extend(chunk(kind, data));
    }
    if !written {
        bail!("the PNG has no image data");
    }
    Ok(out)
}

/// Every text chunk, keyword → text.
fn texts(png: &[u8]) -> Result<Vec<(String, String)>> {
    Ok(chunks(png)?.into_iter().filter_map(|(k, d)| read_text(k, d)).collect())
}

// ---- Reading ---------------------------------------------------------------

/// What this install has, to match a PNG's names against.
pub struct Install {
    pub models: Vec<String>,
    /// LoRA names relative to the LoRA folder, extension included.
    pub loras: Vec<String>,
    /// Runnable upscaler names, relative to the upscaler folder.
    pub upscalers: Vec<String>,
}

/// The settings an A1111-style `parameters` text gives — each only when the
/// text named it, so the page leaves every other box as it is.
#[derive(Debug, Default, PartialEq, Serialize)]
pub struct Params {
    pub prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub negative: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub steps: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cfg_scale: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampler: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheduler: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    pub loras: Vec<LoraRef>,
}

/// The answer to `POST /png-settings`.
#[derive(Debug, Serialize)]
pub struct Loaded {
    /// `yasdig`, `a1111`, or `none`.
    pub source: &'static str,
    /// What couldn't be applied, in words: a model or LoRA not installed, a
    /// sampler sd.cpp hasn't got, a ComfyUI graph.
    pub notes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<Embedded>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Params>,
}

/// Read the settings out of a PNG, matched against what is installed.
pub fn read(png: &[u8], install: &Install) -> Result<Loaded> {
    let texts = texts(png)?;
    let get = |k: &str| texts.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
    let mut notes = Vec::new();

    if let Some(mut item) = get(YASDIG).and_then(|j| serde_json::from_str::<Embedded>(j).ok()) {
        if !install.models.contains(&item.template) {
            notes.push(format!("made with {}, which isn't installed — the current model stays", item.template));
        }
        item.profile.loras.retain(|l| {
            let have = install.loras.contains(&l.name);
            if !have {
                notes.push(format!("LoRA {} isn't installed", l.name));
            }
            have
        });
        for m in [&mut item.profile.refine_model, &mut item.profile.hires_model] {
            if m.as_ref().is_some_and(|n| !install.models.contains(n)) {
                *m = None;
            }
        }
        // The rest of what names something: each is a note and a fallback, never
        // a value the next Generate would be refused over.
        let p = &mut item.profile;
        if let Some(u) = p.upscaler.take_if(|u| !install.upscalers.contains(u)) {
            notes.push(format!("upscaler {u} isn't installed — FSR enlarges instead"));
        }
        if !crate::config::SAMPLERS.contains(&p.sampler.as_str()) {
            let fallback = Profile::default().sampler;
            notes.push(format!("sampler {} isn't one this build has — {fallback} instead", p.sampler));
            p.sampler = fallback;
        }
        if !crate::config::SCHEDULERS.contains(&p.scheduler.as_str()) {
            notes.push(format!("scheduler {} isn't one this build has — Auto instead", p.scheduler));
            p.scheduler = crate::config::AUTO_SCHEDULER.to_string();
        }
        if !crate::config::CACHE_MODES.contains(&p.cache_mode.as_str()) {
            p.cache_mode = Profile::default().cache_mode;
        }
        // Hand-edited or from a newer build: dropped with a note rather than
        // sent to a Generate that would refuse it.
        if let Some(e) = p.layout.as_ref().and_then(|l| l.validate().err()) {
            notes.push(format!("its layout boxes couldn't be read ({e}) — left out"));
            p.layout = None;
        }
        return Ok(Loaded { source: "yasdig", notes, item: Some(item), params: None });
    }

    if let Some(text) = get(PARAMETERS) {
        let params = parse_parameters(text, install, &mut notes);
        notes.push("a seed from another program gives a close picture, not the same one".into());
        return Ok(Loaded { source: "a1111", notes, item: None, params: Some(params) });
    }

    if get("workflow").is_some() || get("prompt").is_some() {
        notes.push("made in ComfyUI — its node graph can't be read as settings".into());
    } else {
        notes.push("this PNG carries no generation settings".into());
    }
    Ok(Loaded { source: "none", notes, item: None, params: None })
}

/// A1111's `parameters`: prompt lines, an optional `Negative prompt:` part, and
/// a last line of `Key: value` pairs starting with `Steps:`.
fn parse_parameters(text: &str, install: &Install, notes: &mut Vec<String>) -> Params {
    let text = text.replace("\r\n", "\n");
    let (body, settings) = match text.rfind("\nSteps: ") {
        Some(i) => (&text[..i], &text[i + 1..]),
        None if text.starts_with("Steps: ") => ("", text.as_str()),
        None => (text.as_str(), ""),
    };
    let (prompt, negative) = match body.find("Negative prompt:") {
        Some(i) => (&body[..i], Some(body[i + "Negative prompt:".len()..].trim().to_string())),
        None => (body, None),
    };
    let (prompt, tags) = take_lora_tags(prompt.trim());

    let mut p = Params { prompt, negative, ..Params::default() };
    for (stem, strength) in tags {
        match install.loras.iter().find(|n| lora_stem(n) == stem) {
            Some(name) => p.loras.push(LoraRef { name: name.clone(), strength }),
            None => notes.push(format!("LoRA {stem} isn't installed")),
        }
    }

    for (key, value) in pairs(settings) {
        match key.as_str() {
            "Steps" => p.steps = value.parse().ok(),
            "CFG scale" => p.cfg_scale = value.parse().ok(),
            "Seed" => p.seed = value.parse().ok(),
            "Size" => {
                if let Some((w, h)) = value.split_once('x') {
                    p.width = w.trim().parse().ok();
                    p.height = h.trim().parse().ok();
                }
            }
            "Sampler" => {
                // Before A1111 1.9 the schedule rode on the sampler's name.
                let (name, sched) = match value.strip_suffix(" Karras") {
                    Some(n) => (n, Some("karras")),
                    None => match value.strip_suffix(" Exponential") {
                        Some(n) => (n, Some("exponential")),
                        None => (value.as_str(), None),
                    },
                };
                p.sampler = from_a1111(SAMPLERS, name, crate::config::SAMPLERS);
                if p.sampler.is_none() {
                    notes.push(format!("sampler {name} has no sd.cpp counterpart — the current one stays"));
                }
                if let Some(s) = sched {
                    p.scheduler.get_or_insert_with(|| s.to_string());
                }
            }
            "Schedule type" => {
                p.scheduler = from_a1111(SCHEDULERS, &value, crate::config::SCHEDULERS);
                if p.scheduler.is_none() {
                    notes.push(format!("schedule {value} has no sd.cpp counterpart — the current one stays"));
                }
            }
            "Model" => {
                if install.models.contains(&value) {
                    p.template = Some(value);
                } else {
                    notes.push(format!("made with {value}, which isn't installed — the current model stays"));
                }
            }
            _ => {}
        }
    }
    p
}

/// The prompt with every `<lora:name:weight>` taken out, and the tags.
fn take_lora_tags(prompt: &str) -> (String, Vec<(String, f32)>) {
    let mut out = String::new();
    let mut tags = Vec::new();
    let mut rest = prompt;
    while let Some(i) = rest.find("<lora:") {
        let Some(end) = rest[i..].find('>') else { break };
        let inner = &rest[i + "<lora:".len()..i + end];
        let (name, w) = match inner.split_once(':') {
            Some((n, w)) => (n, w.split(':').next().unwrap_or("1").trim().parse().unwrap_or(1.0)),
            None => (inner, 1.0),
        };
        tags.push((name.trim().to_string(), w));
        out.push_str(&rest[..i]);
        rest = &rest[i + end + 1..];
    }
    out.push_str(rest);
    // Tags usually sit at the end, after a comma or a space: tidy what's left.
    let out = out.split_whitespace().collect::<Vec<_>>().join(" ");
    (out.trim_end_matches([',', ' ']).to_string(), tags)
}

/// `Key: value, Key: "quoted, value", …` — A1111 quotes a value with a comma
/// in it and escapes quotes inside with a backslash.
fn pairs(line: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut chars = line.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| *c == ' ' || *c == ',') {
            chars.next();
        }
        let mut key = String::new();
        for c in chars.by_ref() {
            if c == ':' {
                break;
            }
            key.push(c);
        }
        if key.is_empty() {
            return out;
        }
        while chars.peek() == Some(&' ') {
            chars.next();
        }
        let mut value = String::new();
        if chars.peek() == Some(&'"') {
            chars.next();
            while let Some(c) = chars.next() {
                match c {
                    '\\' => value.extend(chars.next()),
                    '"' => break,
                    c => value.push(c),
                }
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c == ',' {
                    break;
                }
                value.push(c);
                chars.next();
            }
        }
        out.push((key.trim().to_string(), value.trim().to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_fn(w, h, |x, y| image::Rgba([x as u8, y as u8, 7, 255]));
        let mut out = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
        out
    }

    fn item() -> WorkItem {
        let profile = Profile {
            model: "/home/someone/models/checkpoints/secret.safetensors".into(),
            extra_args: vec!["--secret-path=/home/someone/x".into()],
            sampler: "euler_a".into(),
            scheduler: "karras".into(),
            steps: 25,
            cfg_scale: 6.5,
            width: 832,
            height: 1216,
            refine_steps: 4,
            loras: vec![LoraRef { name: "sdxl/detail.safetensors".into(), strength: 0.8 }],
            // Travels like the LoRAs: words and fractions of the frame.
            layout: Some(crate::layout::Layout {
                items: vec![crate::layout::Item {
                    label: crate::layout::Label::Text,
                    caption: "a sign".into(),
                    text: Some("OPEN".into()),
                    rect: [0.25, 0.1, 0.75, 0.3],
                }],
            }),
            ..Profile::default()
        };
        WorkItem {
            id: "1-1".into(),
            created_ms: 0,
            prompt: "a lovely fox with red hair".into(),
            negative: Some("blurry".into()),
            typed_prompt: Some("a lovely {animal} with {color} hair".into()),
            typed_negative: None,
            seed: 42,
            template: "sd_xl_base_1.0".into(),
            width: 1664,
            height: 2432,
            thumb_width: 0,
            thumb_height: 0,
            refine_passes: 0,
            hires_passes: 0,
            step: 0,
            sharpness: 0.0,
            clarity: 0.0,
            source: Some("someone-elses-stem".into()),
            profile,
            agent: None,
        }
    }

    fn install() -> Install {
        Install {
            models: vec!["sd_xl_base_1.0".into()],
            loras: vec!["sdxl/detail.safetensors".into()],
            upscalers: vec![],
        }
    }

    #[test]
    fn crc_matches_the_png_spec() {
        // Every PNG's IEND chunk ends in this CRC.
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
    }

    #[test]
    fn embedding_keeps_the_picture_and_puts_text_before_the_data() {
        let src = png(5, 3);
        let out = embed(&src, &item()).unwrap();
        let a = image::load_from_memory(&src).unwrap().to_rgba8();
        let b = image::load_from_memory(&out).unwrap().to_rgba8();
        assert_eq!(a, b);
        let kinds: Vec<&[u8]> = chunks(&out).unwrap().into_iter().map(|(k, _)| k).collect();
        let idat = kinds.iter().position(|k| *k == b"IDAT").unwrap();
        let text = kinds.iter().rposition(|k| *k == b"tEXt" || *k == b"iTXt").unwrap();
        assert!(text < idat, "{kinds:?}");
        // Saved again: replaced, not stacked.
        let twice = embed(&out, &item()).unwrap();
        assert_eq!(texts(&twice).unwrap().len(), 2);
    }

    #[test]
    fn the_record_never_carries_a_path() {
        let out = embed(&png(2, 2), &item()).unwrap();
        let all = String::from_utf8_lossy(&out);
        assert!(!all.contains("/home/someone"), "a path leaked");
        assert!(!all.contains("someone-elses-stem"));
    }

    #[test]
    fn parameters_are_a1111s_layout() {
        let text = parameters(&item());
        assert_eq!(
            text,
            "a lovely fox with red hair <lora:detail:0.8>\nNegative prompt: blurry\n\
             Steps: 25, Sampler: Euler a, Schedule type: Karras, CFG scale: 6.5, Seed: 42, \
             Size: 832x1216, Model: sd_xl_base_1.0, Version: yasdig "
                .to_string()
                + env!("CARGO_PKG_VERSION")
        );
    }

    #[test]
    fn a_yasdig_png_restores_everything_it_can() {
        let out = embed(&png(2, 2), &item()).unwrap();
        let got = read(&out, &install()).unwrap();
        assert_eq!(got.source, "yasdig");
        assert!(got.notes.is_empty(), "{:?}", got.notes);
        assert_eq!(got.item.unwrap(), Embedded::from_item(&item()));

        // Elsewhere, without the model or the LoRA: named, and the LoRA dropped.
        let bare = Install { models: vec![], loras: vec![], upscalers: vec![] };
        let got = read(&out, &bare).unwrap();
        assert_eq!(got.notes.len(), 2, "{:?}", got.notes);
        assert!(got.item.unwrap().profile.loras.is_empty());
    }

    /// Every name a record restores is matched against this install: a
    /// missing upscaler, an unknown sampler or scheduler is a note and a
    /// fallback, never a setting the next Generate is refused over.
    #[test]
    fn names_this_install_lacks_fall_back_with_a_note() {
        let mut it = item();
        it.profile.upscaler = Some("4x-Gone.pth".into());
        it.profile.sampler = "future_sampler".into();
        it.profile.scheduler = "future_schedule".into();
        let got = read(&embed(&png(2, 2), &it).unwrap(), &install()).unwrap();
        assert_eq!(got.notes.len(), 3, "{:?}", got.notes);
        let p = got.item.unwrap().profile;
        assert_eq!(p.upscaler, None);
        assert_eq!(p.sampler, Profile::default().sampler);
        assert_eq!(p.scheduler, crate::config::AUTO_SCHEDULER);
    }

    /// A record from another version, missing fields, is still ours.
    #[test]
    fn a_record_missing_fields_still_reads_as_yasdig() {
        let rec = r#"{"v":1,"prompt":"a cat","negative":null,"typed_prompt":null,
            "typed_negative":null,"seed":7,"template":"sd_xl_base_1.0","profile":{"steps":9}}"#;
        // The image's chunks with the record spliced in before the data.
        let mut out = SIGNATURE.to_vec();
        for (kind, data) in chunks(&png(2, 2)).unwrap() {
            if kind == b"IDAT" {
                out.extend(text_chunk(YASDIG, rec, true));
            }
            out.extend(chunk(kind, data));
        }
        let got = read(&out, &install()).unwrap();
        assert_eq!(got.source, "yasdig");
        let p = got.item.unwrap().profile;
        assert_eq!(p.steps, 9);
        assert_eq!(p.width, Profile::default().width);
    }

    #[test]
    fn an_a1111_png_fills_what_it_names() {
        let text = "masterpiece, (a red crab:1.2) on a beach, <lora:detail:0.7>, <lora:gone:1>\n\
                    Negative prompt: blurry, lowres\n\
                    Steps: 28, Sampler: DPM++ 2M Karras, CFG scale: 7, Seed: 1234, \
                    Size: 1024x768, Model hash: abc123, Model: sd_xl_base_1.0, \
                    Lora hashes: \"detail: 1a2b, gone: 3c4d\", Version: v1.6.0";
        let mut src = png(2, 2);
        // Insert the chunk by hand, as A1111 would have.
        let pos = src.len() - 12;
        src.splice(pos..pos, text_chunk("parameters", text, false));
        let got = read(&src, &install()).unwrap();
        assert_eq!(got.source, "a1111");
        let p = got.params.unwrap();
        assert_eq!(p.prompt, "masterpiece, (a red crab:1.2) on a beach");
        assert_eq!(p.negative.as_deref(), Some("blurry, lowres"));
        assert_eq!((p.steps, p.cfg_scale, p.seed), (Some(28), Some(7.0), Some(1234)));
        assert_eq!((p.width, p.height), (Some(1024), Some(768)));
        assert_eq!(p.sampler.as_deref(), Some("dpm++2m"));
        assert_eq!(p.scheduler.as_deref(), Some("karras"));
        assert_eq!(p.template.as_deref(), Some("sd_xl_base_1.0"));
        assert_eq!(p.loras, vec![LoraRef { name: "sdxl/detail.safetensors".into(), strength: 0.7 }]);
        assert!(got.notes.iter().any(|n| n.contains("LoRA gone")), "{:?}", got.notes);
    }

    #[test]
    fn our_own_parameters_read_back() {
        let text = parameters(&item());
        let mut notes = Vec::new();
        let p = parse_parameters(&text, &install(), &mut notes);
        assert_eq!(p.prompt, "a lovely fox with red hair");
        assert_eq!(p.sampler.as_deref(), Some("euler_a"));
        assert_eq!(p.scheduler.as_deref(), Some("karras"));
        assert_eq!(p.loras.len(), 1);
        assert!(notes.is_empty(), "{notes:?}");
    }

    #[test]
    fn every_mapped_name_is_one_sd_cpp_takes() {
        for (ours, _) in SAMPLERS {
            assert!(crate::config::SAMPLERS.contains(ours), "{ours}");
        }
        for (ours, _) in SCHEDULERS {
            assert!(crate::config::SCHEDULERS.contains(ours), "{ours}");
        }
    }

    #[test]
    fn a_png_without_settings_says_so() {
        let got = read(&png(2, 2), &install()).unwrap();
        assert_eq!(got.source, "none");
        let mut comfy = png(2, 2);
        let pos = comfy.len() - 12;
        comfy.splice(pos..pos, text_chunk("workflow", "{\"nodes\":[]}", false));
        assert!(read(&comfy, &install()).unwrap().notes[0].contains("ComfyUI"));
        assert!(read(b"GIF89a", &install()).is_err());
    }
}
