//! The Models tab's *Installed*: every model file yasdig can see, grouped by
//! what it is, with the families it serves — for `GET /installed`, and the
//! whitelist a delete is checked against.
//!
//! Built from the scans discovery already does (`video_scan`, which holds the
//! image scan too, `list_loras`, `list_upscalers`, `agents::scan`), so a file
//! is in one group for the reason the pickers offer it, and nothing here reads
//! a header a second time: `identify` memoizes on `(path, length, mtime)`, and
//! a repeat costs a stat per file.
//!
//! An item's id is `<root>/<path under it>`, root one of [`ROOTS`]: the page
//! names a file by it and never by a path, and a folder configured outside
//! `models/` still has a key that doesn't move with the install.

use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use crate::config::{slash_name, Config, ImageFamily, VideoFamily};
use crate::discover::{DitKind, Found, LtxVersion, Role};

/// What the rail lists under *Installed*, in this order.
pub const GROUPS: [(&str, &str); 8] = [
    ("checkpoints", "Checkpoints"),
    ("video", "Video models"),
    ("loras", "LoRAs"),
    ("encoders", "Text encoders"),
    ("vaes", "VAEs"),
    ("upscalers", "Upscalers"),
    ("agents", "Agent models"),
    ("other", "Other"),
];

/// The folders an id may start with. The more specific first: the video,
/// LoRA, upscaler and agent folders sit inside the models folder by default,
/// and a file there is named from its own.
const ROOTS: [&str; 5] = ["video", "loras", "upscalers", "agents", "models"];

fn root_dir(cfg: &Config, root: &str) -> Option<PathBuf> {
    Some(match root {
        "models" => cfg.models_dir(),
        "video" => cfg.video_dir(),
        "loras" => cfg.lora_dir(),
        "upscalers" => cfg.upscaler_dir(),
        "agents" => cfg.agent_dir(),
        _ => return None,
    })
}

/// The id of a file under one of the roots, or `None` outside them all.
pub fn id_of(cfg: &Config, path: &Path) -> Option<String> {
    ROOTS.iter().find_map(|r| {
        let dir = root_dir(cfg, r)?;
        path.starts_with(&dir).then(|| format!("{r}/{}", slash_name(&dir, path)))
    })
}

/// The path an id names, when its shape is one [`id_of`] could have made: a
/// known root and plain components only. Whether the file is *listed* is the
/// caller's question ([`find`]).
pub fn resolve(cfg: &Config, id: &str) -> Option<PathBuf> {
    let (root, rel) = id.split_once('/')?;
    let rel = Path::new(rel);
    if rel.as_os_str().is_empty() || !rel.components().all(|c| matches!(c, Component::Normal(_))) {
        return None;
    }
    Some(root_dir(cfg, root)?.join(rel))
}

#[derive(Debug, Clone, Serialize)]
pub struct Item {
    pub id: String,
    pub group: &'static str,
    /// Its path under its folder, as the person would find it.
    pub name: String,
    /// What the pickers call it: the file stem, or an agent's own name.
    pub label: String,
    pub bytes: u64,
    /// [`Role::describe`]'s sentence, or the reason it can't be used.
    pub what: String,
    /// The families it serves, as labels: its own for a model, every family
    /// a slot of which it fits for a part, what a LoRA fits.
    pub families: Vec<&'static str>,
    /// The `models.toml` entries naming it (`[templates.x]`, `[[video]] x`),
    /// which yasdig never edits: a delete's confirm says so.
    pub named_by: Vec<String>,
    /// Every file deleting it removes: one, or an agent's model and projector.
    #[serde(skip)]
    pub paths: Vec<PathBuf>,
}

fn bytes(p: &Path) -> u64 {
    std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
}

fn stem(name: &str) -> String {
    let base = name.rsplit('/').next().unwrap_or(name);
    match base.rsplit_once('.') {
        Some((s, _)) => s.to_string(),
        None => base.to_string(),
    }
}

/// The video family a transformer is, as `discover::model_for` maps it.
fn dit_family(kind: &DitKind) -> VideoFamily {
    match kind {
        DitKind::Wan22Ti2v5b => VideoFamily::Wan22Ti2v5b,
        DitKind::Wan22A14b => VideoFamily::Wan22A14b,
        DitKind::WanVace { .. } => VideoFamily::Wan21Vace,
        DitKind::WanOther { .. } => VideoFamily::WanDense,
        DitKind::MinimaxH3 => VideoFamily::MinimaxH3,
        DitKind::Ltx(LtxVersion::V23) => VideoFamily::Ltx23,
        DitKind::Ltx(LtxVersion::V25) => VideoFamily::Ltx25,
    }
}

/// The group a scanned file belongs in, by its role alone.
pub fn group_of(role: &Role) -> &'static str {
    match role {
        Role::Image { .. } => "checkpoints",
        Role::Dit(_) => "video",
        Role::Lora(_) => "loras",
        Role::Llm(_)
        | Role::T5xxl
        | Role::Umt5
        | Role::ClipL
        | Role::ClipVision
        | Role::Tokenizer(_)
        | Role::EmbeddingsConnectors => "encoders",
        Role::ImageVae(_) | Role::Vae(_) | Role::AudioVae(_) => "vaes",
        Role::LatentUpscaler => "upscalers",
        Role::Unsupported(_) | Role::Unknown(_) => "other",
    }
}

/// Every family `role` serves, by label: its own for a model, and for a part
/// every image and video family with a slot it fits — which is what the
/// pickers would offer it to, and what a delete leaves without.
pub fn families_of(role: &Role) -> Vec<&'static str> {
    match role {
        Role::Image { family, .. } => vec![family.label()],
        Role::Dit(kind) => vec![dit_family(kind).label()],
        _ => {
            let image = ImageFamily::ALL.iter().filter(|f| f.slots().iter().any(|s| (s.fits)(role))).map(|f| f.label());
            let video = VideoFamily::ALL.iter().filter(|f| f.slots().iter().any(|s| (s.fits)(role))).map(|f| f.label());
            image.chain(video).collect()
        }
    }
}

/// A LoRA's `families` as the page's wire names: back to labels.
fn label_of_wire(name: &str) -> Option<&'static str> {
    ImageFamily::ALL
        .iter()
        .find(|f| f.as_str() == name)
        .map(|f| f.label())
        .or_else(|| VideoFamily::ALL.iter().find(|f| f.as_str() == name).map(|f| f.label()))
}

fn item(cfg: &Config, path: &Path, group: &'static str, what: String, families: Vec<&'static str>) -> Option<Item> {
    let id = id_of(cfg, path)?;
    let name = id.split_once('/').map_or(id.as_str(), |(_, n)| n).to_string();
    Some(Item {
        label: stem(&name),
        name,
        id,
        group,
        bytes: bytes(path),
        what,
        families,
        named_by: Vec::new(),
        paths: vec![path.to_path_buf()],
    })
}

fn of_found(cfg: &Config, f: &Found) -> Option<Item> {
    let what = match &f.role {
        Role::Unknown(why) => why.clone(),
        r => r.describe(),
    };
    item(cfg, &f.path, group_of(&f.role), what, families_of(&f.role))
}

/// Everything, grouped in [`GROUPS`] order and by name within a group.
pub fn list(cfg: &Config) -> Vec<Item> {
    let mut items: Vec<Item> = Vec::new();
    let push = |items: &mut Vec<Item>, i: Option<Item>| {
        if let Some(i) = i {
            if !items.iter().any(|o| o.id == i.id) {
                items.push(i);
            }
        }
    };
    // The image scan is inside it: one list, so a VAE both tabs use is one row.
    for f in cfg.video_scan() {
        push(&mut items, of_found(cfg, &f));
    }
    let lora_dir = cfg.lora_dir();
    for l in cfg.list_loras() {
        let path = lora_dir.join(&l.name);
        let (group, what) = if l.usable {
            ("loras", if l.base.is_empty() { "LoRA".to_string() } else { format!("{} LoRA", l.base) })
        } else {
            ("other", l.note.clone())
        };
        let families = l.families.iter().filter_map(|n| label_of_wire(n)).collect();
        push(&mut items, item(cfg, &path, group, what, families));
    }
    let up_dir = cfg.upscaler_dir();
    for u in cfg.list_upscalers() {
        let path = up_dir.join(&u.name);
        let (group, what) = if u.supported {
            ("upscalers", format!("{} upscaler", u.arch))
        } else {
            ("other", format!("{} upscaler — this build can't run it", u.arch))
        };
        push(&mut items, item(cfg, &path, group, what, Vec::new()));
    }
    let scan = crate::agents::scan(&cfg.agent_dir());
    for a in &scan.models {
        let what = format!("{} agent model{}", a.arch, if a.vision() { ", sees pictures" } else { "" });
        if let Some(mut i) = item(cfg, &a.lm, "agents", what, Vec::new()) {
            i.label = a.label.clone();
            i.paths.extend(a.mmproj.clone());
            i.bytes = i.paths.iter().map(|p| bytes(p)).sum();
            push(&mut items, Some(i));
        }
    }
    for (path, why) in &scan.other {
        push(&mut items, item(cfg, path, "other", why.clone(), Vec::new()));
    }
    // What models.toml points at by hand.
    let mut named: Vec<(PathBuf, String)> =
        cfg.templates.iter().map(|(n, t)| (cfg.resolve(&t.model), format!("[templates.{n}]"))).collect();
    for v in &cfg.video {
        let paths = [Some(&v.diffusion_model), v.high_noise_diffusion_model.as_ref(), Some(&v.vae), v.t5xxl.as_ref(),
            v.llm.as_ref(), v.audio_vae.as_ref(), v.clip_vision.as_ref(), v.embeddings_connectors.as_ref(),
            v.latent_upscaler.as_ref()];
        for p in paths.into_iter().flatten() {
            named.push((cfg.resolve_path(p), format!("[[video]] {}", v.name)));
        }
    }
    for i in &mut items {
        i.named_by = named.iter().filter(|(p, _)| i.paths.contains(p)).map(|(_, n)| n.clone()).collect();
    }
    let rank = |g: &str| GROUPS.iter().position(|(k, _)| *k == g).unwrap_or(GROUPS.len());
    items.sort_by(|a, b| rank(a.group).cmp(&rank(b.group)).then_with(|| a.name.cmp(&b.name)));
    items
}

/// The listed item `id` names — the whitelist for anything that acts on one.
pub fn find(cfg: &Config, id: &str) -> Option<Item> {
    resolve(cfg, id)?;
    list(cfg).into_iter().find(|i| i.id == id)
}

/// Delete the listed item `id`: its file (an agent's two), any `.part` beside
/// it, and its note; and `user/model-settings.toml` made to follow
/// ([`crate::settings::ModelSettings::follow_delete`]). Never a folder.
///
/// Refused while `jobs_busy` — any job queued or running on the diffusion lane:
/// which of them reads which file is in each one's profile, and a Stop is the
/// clearer answer than a guess — and while `downloading` says a file of it is
/// being fetched. Answers what to tell the person, beyond "deleted".
pub fn delete(cfg: &Config, id: &str, jobs_busy: bool, downloading: impl Fn(&Path) -> bool) -> anyhow::Result<String> {
    use anyhow::bail;
    let Some(item) = find(cfg, id) else {
        bail!("{id} is not in the installed list any more — it may have been moved or deleted already");
    };
    if jobs_busy {
        bail!("Stop the running and queued jobs first (the Jobs button) — one of them may be using {}.", item.label);
    }
    if item.paths.iter().any(|p| downloading(p)) {
        bail!("{} is downloading. Cancel it first (the Downloads button).", item.label);
    }
    type Names = Vec<(String, PathBuf)>;
    let names = |cfg: &Config| -> (Names, Names) {
        (
            cfg.entries().into_iter().map(|e| (e.name, e.model)).collect(),
            cfg.video_models_untuned().into_iter().map(|m| (m.name, m.diffusion_model)).collect(),
        )
    };
    let (image_before, video_before) = names(cfg);
    for p in &item.paths {
        if let Err(e) = std::fs::remove_file(p) {
            if e.kind() == std::io::ErrorKind::NotFound {
                continue;
            }
            // Windows refuses to delete a file another program has open.
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                bail!("{} is in use by another program, or can't be deleted by you. Close what has it open and try again.", item.name);
            }
            bail!("deleting {}: {e}", p.display());
        }
        let mut part = p.as_os_str().to_owned();
        part.push(".part");
        let _ = std::fs::remove_file(PathBuf::from(part));
    }
    let (image_after, video_after) = names(cfg);
    let moves = |before: Vec<(String, PathBuf)>, after: &[(String, PathBuf)]| -> Vec<(String, Option<String>)> {
        before
            .into_iter()
            .map(|(name, path)| {
                let now = after.iter().find(|(_, p)| *p == path).map(|(n, _)| n.clone());
                (name, now)
            })
            .collect()
    };
    let image = moves(image_before, &image_after);
    let video = moves(video_before, &video_after);
    let part = slash_name(&cfg.models_dir(), &item.paths[0]);
    let mut said = String::new();
    if let Err(e) = crate::settings::ModelSettings::follow_delete_in(&cfg.user_dir(), &image, &video, &part) {
        eprintln!("[yasdig] after deleting {}: {e:#}", item.name);
        said = format!("Deleted, but user/model-settings.toml couldn't be updated: {e:#}");
    }
    if let Err(e) = crate::notes::set(&cfg.user_dir(), id, "") {
        eprintln!("[yasdig] deleting the note of {}: {e:#}", item.name);
    }
    if !item.named_by.is_empty() {
        said = format!("{said} models.toml still names it in {}; edit that by hand.", item.named_by.join(", "));
    }
    Ok(said.trim().to_string())
}

/// `GET /installed`.
pub fn json(cfg: &Config) -> serde_json::Value {
    let items = list(cfg);
    let groups: Vec<_> = GROUPS.iter().map(|(k, t)| serde_json::json!({"id": k, "title": t})).collect();
    serde_json::json!({"groups": groups, "items": items})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover::{AudioVaeKind, ImageVaeKind, LlmKind, LoraBase, TokenizerKind, VaeKind};

    fn fake_model(path: &Path, tensors: &[(&str, &str)]) {
        let body: Vec<String> = tensors
            .iter()
            .map(|(n, shape)| format!(r#""{n}":{{"dtype":"F16","shape":{shape},"data_offsets":[0,2]}}"#))
            .collect();
        let json = format!("{{{}}}", body.join(","));
        let mut buf = (json.len() as u64).to_le_bytes().to_vec();
        buf.extend_from_slice(json.as_bytes());
        buf.extend_from_slice(&[0u8; 2]);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, buf).unwrap();
    }

    fn config(tag: &str) -> (PathBuf, Config) {
        let root = crate::config::fresh_test_root(tag);
        std::fs::write(root.join("models.toml"), "[sd]\nbinary = \"sd-cli\"\n").unwrap();
        let cfg = Config::load(&root.join("models.toml")).unwrap();
        (root, cfg)
    }

    #[test]
    fn every_role_lands_in_a_known_group() {
        let roles = [
            Role::Image { family: ImageFamily::Sdxl, bundled: true },
            Role::Dit(DitKind::MinimaxH3),
            Role::Lora(LoraBase::Unknown),
            Role::Llm(LlmKind::Qwen3 { dim: 2560 }),
            Role::T5xxl,
            Role::Umt5,
            Role::ClipL,
            Role::ClipVision,
            Role::Tokenizer(TokenizerKind::Qwen35),
            Role::EmbeddingsConnectors,
            Role::ImageVae(ImageVaeKind::Sd),
            Role::Vae(VaeKind::Wan21),
            Role::AudioVae(AudioVaeKind::MinimaxH3),
            Role::LatentUpscaler,
            Role::Unsupported("x".into()),
            Role::Unknown("y".into()),
        ];
        for r in &roles {
            let g = group_of(r);
            assert!(GROUPS.iter().any(|(k, _)| *k == g), "{r:?} → {g}");
        }
        // The Wan 2.1 VAE serves the image families that decode with it and
        // the dense Wans: one file, both tabs.
        let fams = families_of(&Role::Vae(VaeKind::Wan21));
        assert!(fams.contains(&VideoFamily::WanDense.label()), "{fams:?}");
        assert!(fams.iter().any(|l| ImageFamily::ALL.iter().any(|f| f.label() == *l)), "{fams:?}");
    }

    #[test]
    fn an_id_is_a_known_root_and_plain_components() {
        let (_root, cfg) = config("yasdig-inv-ids");
        let p = cfg.lora_dir().join("sdxl/detail.safetensors");
        let id = id_of(&cfg, &p).unwrap();
        assert_eq!(id, "loras/sdxl/detail.safetensors");
        assert_eq!(resolve(&cfg, &id).unwrap(), p);
        // A file in the video folder is named from it, not from models/.
        let p = cfg.video_dir().join("vae/wan.safetensors");
        assert_eq!(id_of(&cfg, &p).unwrap(), "video/vae/wan.safetensors");
        assert_eq!(resolve(&cfg, "video/vae/wan.safetensors").unwrap(), p);
        for bad in ["loras/../settings.toml", "etc/passwd", "models/", "models//x", "/abs", "models/a/../../x", "user/x"] {
            assert!(resolve(&cfg, bad).is_none(), "{bad}");
        }
        assert!(id_of(&cfg, Path::new("/elsewhere/x.gguf")).is_none());
    }

    /// A delete is whitelisted, refused while anything runs, and takes the
    /// file, its `.part` and its note — and a survivor's settings follow its
    /// name when the delete moved it.
    #[test]
    fn a_delete_is_checked_and_its_settings_follow() {
        let (_root, cfg) = config("yasdig-inv-delete");
        let sdxl = [("conditioner.embedders.1.model.transformer.resblocks.0.attn.in_proj_weight", "[3840,1280]"),
            ("model.diffusion_model.input_blocks.4.1.transformer_blocks.0.attn2.to_k.weight", "[640,2048]"),
            ("first_stage_model.decoder.conv_in.weight", "[512,4,3,3]")];
        let a = cfg.models_dir().join("a/model.safetensors");
        let b = cfg.models_dir().join("b/model.safetensors");
        fake_model(&a, &sdxl);
        fake_model(&b, &sdxl);
        let names: Vec<_> = cfg.entries().into_iter().map(|e| (e.name, e.model)).collect();
        assert_eq!(names.len(), 2, "{names:?}");
        let (gone, kept) = (&names[0], &names[1]);
        assert_eq!((gone.0.as_str(), kept.0.as_str()), ("model", "model-2"), "{names:?}");
        let user = cfg.user_dir();
        let mut ms = crate::settings::ModelSettings::default();
        ms.model.insert("model-2".into(), crate::settings::PartialSettings { steps: Some(7), ..Default::default() });
        ms.save(&user).unwrap();
        let id = id_of(&cfg, &gone.1).unwrap();
        crate::notes::set(&user, &id, "a note").unwrap();
        std::fs::write(PathBuf::from(format!("{}.part", gone.1.display())), b"x").unwrap();

        assert!(delete(&cfg, "models/../etc", false, |_| false).is_err());
        assert!(delete(&cfg, &id, true, |_| false).unwrap_err().to_string().contains("Stop"));
        assert!(delete(&cfg, &id, false, |_| true).unwrap_err().to_string().contains("downloading"));
        assert!(gone.1.exists());
        delete(&cfg, &id, false, |_| false).unwrap();
        assert!(!gone.1.exists() && kept.1.exists());
        assert!(!PathBuf::from(format!("{}.part", gone.1.display())).exists());
        assert!(crate::notes::all(&user).is_empty());
        let ms = crate::settings::ModelSettings::load(&user).unwrap();
        assert_eq!(ms.model["model"].steps, Some(7), "the survivor is `model` now, with its own settings");
    }

    /// The Wan 2.1 VAE in the video folder is both the image scan's (Krea 2
    /// decodes with it) and the video scan's: one row, with both tabs'
    /// families.
    #[test]
    fn a_part_both_scans_find_is_listed_once() {
        let (_root, cfg) = config("yasdig-inv-once");
        fake_model(
            &cfg.video_dir().join("vae/wan_2.1_vae.safetensors"),
            &[("decoder.conv1.weight", "[160]"), ("conv2.bias", "[16]"), ("conv2.weight", "[16,16,1,1,1]")],
        );
        let items = list(&cfg);
        let vaes: Vec<_> = items.iter().filter(|i| i.group == "vaes").collect();
        assert_eq!(vaes.len(), 1, "{items:?}");
        assert_eq!(vaes[0].id, "video/vae/wan_2.1_vae.safetensors");
        assert!(vaes[0].families.contains(&VideoFamily::WanDense.label()));
        assert!(find(&cfg, &vaes[0].id).is_some());
        assert!(find(&cfg, "video/vae/other.safetensors").is_none());
    }
}
