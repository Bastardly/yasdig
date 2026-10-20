//! Layout boxes: where things go in a MOSS-Image picture, and what they are.
//!
//! MOSS-Image reads a **JSON layout** as its prompt — a scene caption plus a
//! flat list of items, each a label, a caption, the words to render for a text
//! item, and a box. Neither its pipeline nor sd.cpp parses it: the string goes
//! to the Qwen3.5 encoder as it is (`encode_prompt` upstream, the MOSS branch
//! of `conditioner.hpp` here, whose `prompt_attn_range` of `{0, 0}` also skips
//! the `(word:1.2)` parser that would otherwise eat the JSON's brackets). So
//! the whole feature is writing that string, and the model card's compiler
//! (`skills/moss-image-prompt-enhancer/scripts/compile_prompt.py`) is the spec
//! [`compose`] follows to the byte: compact separators, keys in its order,
//! integer `abs_centralized` / `yxyx` boxes, left/top floored and right/bottom
//! ceiled before the centre is subtracted.
//!
//! Two things break silently:
//!
//! - **A box is pixels at one size.** `abs_centralized` is ±W/2, ±H/2 of *the
//!   canvas being drawn*, so a layout composed for the base render is wrong for
//!   a hi-res pass at twice the size. A [`Layout`] is therefore stored
//!   normalised (0..1 of the frame) and composed per sd-cli call at that call's
//!   size ([`conditioned`]); the JSON is never stored as the prompt.
//! - **Only a whole-picture pass can use it.** A Redraw or an Extend works on a
//!   crop, where the boxes mean nothing; they condition on the caption alone.
//!
//! On [`crate::config::Profile`] like `references`: request-only, retained with
//! the image so every pass and the sidecar have it, refused by
//! `Profile::validate` on a family that doesn't read layouts.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

/// More boxes than this is not a layout anyone draws by hand; it keeps a
/// pasted record from becoming a 4096-token prompt.
pub const MAX_ITEMS: usize = 32;

/// What an item is, in the model card's vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Label {
    Object,
    Character,
    Text,
}

/// One box. `rect` is `[x0, y0, x1, y1]` as fractions of the frame from its
/// top-left — the page's drawing convention, and size-free.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub label: Label,
    pub caption: String,
    /// The exact words, for a [`Label::Text`] item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub rect: [f64; 4],
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    pub items: Vec<Item>,
}

impl Layout {
    /// The model card's rules, checked before anything runs: a nonempty
    /// caption on every item, the words on every text item, a box with area
    /// inside the frame.
    pub fn validate(&self) -> Result<()> {
        if self.items.len() > MAX_ITEMS {
            bail!("a layout has at most {MAX_ITEMS} boxes; this one has {}", self.items.len());
        }
        for (i, it) in self.items.iter().enumerate() {
            let n = i + 1;
            if it.caption.trim().is_empty() {
                bail!("layout box {n} needs a caption: what is in it");
            }
            if it.label == Label::Text && it.text.as_deref().is_none_or(|t| t.trim().is_empty()) {
                bail!("layout box {n} is text, so it needs the words to render");
            }
            let [x0, y0, x1, y1] = it.rect;
            if !it.rect.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)) || x0 >= x1 || y0 >= y1 {
                bail!("layout box {n} must lie inside the picture and have a size");
            }
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct Summary<'a> {
    caption: &'a str,
}

/// An item as the model reads it: the compiler keeps the draft's key order,
/// which is `label`, `caption`, `text`, `bbox` in every example it ships.
#[derive(Serialize)]
struct Compiled<'a> {
    label: Label,
    caption: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<&'a str>,
    bbox: [i64; 4],
}

#[derive(Serialize)]
struct Prompt<'a> {
    summary: Summary<'a>,
    items: Vec<Compiled<'a>>,
    bbox_coord_space: &'static str,
    bbox_format: &'static str,
}

/// `rect` on a `w`×`h` canvas, as the compiler's `centered_box`: enclosed in
/// whole pixels (floor the near edges, ceil the far ones), then the centre
/// subtracted with integer halves, in `[y0, x0, y1, x1]` order.
fn bbox(rect: [f64; 4], w: u32, h: u32) -> [i64; 4] {
    let (hw, hh) = ((w / 2) as i64, (h / 2) as i64);
    let (w, h) = (w as f64, h as f64);
    // Rounded to 9 places first, as the compiler does, so 176/1536 × 1536
    // landing a hair under 176 doesn't floor a pixel away. `f64` rather than
    // `f32` for the same reason: an f32 fraction is off by ~1e-4 px there.
    let px = |v: f64, side: f64| (v * side * 1e9).round() / 1e9;
    let [x0, y0, x1, y1] = rect;
    let mut b = [
        px(y0, h).floor() as i64 - hh,
        px(x0, w).floor() as i64 - hw,
        px(y1, h).ceil() as i64 - hh,
        px(x1, w).ceil() as i64 - hw,
    ];
    // A sliver that encloses to nothing still gets one pixel, rather than the
    // compiler's refusal: the page lets a box be dragged that small.
    b[2] = b[2].max(b[0] + 1);
    b[3] = b[3].max(b[1] + 1);
    b
}

/// The prompt MOSS-Image reads for `caption` and `layout` on a `w`×`h` canvas.
/// `roll` rolls the `{variables}` in each item's caption and words, off the
/// image's seed like the prompt's — so every pass draws the same words.
pub fn compose(
    caption: &str,
    layout: &Layout,
    w: u32,
    h: u32,
    roll: &dyn Fn(&str) -> Result<String>,
) -> Result<String> {
    let rolled: Vec<(String, Option<String>)> = layout
        .items
        .iter()
        .map(|it| Ok((roll(&it.caption)?, it.text.as_deref().map(roll).transpose()?)))
        .collect::<Result<_>>()?;
    let prompt = Prompt {
        summary: Summary { caption },
        items: layout
            .items
            .iter()
            .zip(&rolled)
            .map(|(it, (caption, text))| Compiled {
                label: it.label,
                caption,
                text: text.as_deref(),
                bbox: bbox(it.rect, w, h),
            })
            .collect(),
        bbox_coord_space: "abs_centralized",
        bbox_format: "yxyx",
    };
    // serde_json leaves non-ASCII as it is, which is `ensure_ascii=False`.
    Ok(serde_json::to_string(&prompt)?)
}

/// What a whole-picture pass at `w`×`h` conditions on: the layout composed
/// around `prompt` when the profile carries boxes, else `prompt` itself.
pub fn conditioned(
    p: &crate::config::Profile,
    prompt: &str,
    (w, h): (u32, u32),
    vars: &crate::variables::Variables,
    seed: i64,
) -> Result<String> {
    match &p.layout {
        Some(l) if !l.items.is_empty() => {
            compose(prompt, l, w, h, &|s| crate::variables::expand(s, vars, seed))
        }
        _ => Ok(prompt.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(label: Label, caption: &str, text: Option<&str>, rect: [f64; 4]) -> Item {
        Item { label, caption: caption.into(), text: text.map(Into::into), rect }
    }

    fn plain(s: &str) -> Result<String> {
        Ok(s.to_string())
    }

    /// The model card's own example (`references/layout.md`): 1536×1024, the
    /// cup's `[176, 224, 800, 864]` becomes `[-288, -592, 352, 32]`.
    #[test]
    fn the_model_cards_example_compiles_to_its_own_numbers() {
        let (w, h) = (1536.0, 1024.0);
        let rect = |x0: f64, y0: f64, x1: f64, y1: f64| [x0 / w, y0 / h, x1 / w, y1 / h];
        let l = Layout {
            items: vec![
                item(Label::Object, "An ivory ceramic cup.", None, rect(176.0, 224.0, 800.0, 864.0)),
                item(Label::Text, "Large dark-green Chinese title.", Some("把日常，过成诗。"), rect(928.0, 272.0, 1408.0, 448.0)),
            ],
        };
        l.validate().unwrap();
        let s = compose("A warm studio campaign.", &l, 1536, 1024, &plain).unwrap();
        assert_eq!(
            s,
            r#"{"summary":{"caption":"A warm studio campaign."},"items":[{"label":"object","caption":"An ivory ceramic cup.","bbox":[-288,-592,352,32]},{"label":"text","caption":"Large dark-green Chinese title.","text":"把日常，过成诗。","bbox":[-240,160,-64,640]}],"bbox_coord_space":"abs_centralized","bbox_format":"yxyx"}"#
        );
    }

    /// The README's 1024² poster: the title's box, and the same box at twice
    /// the size — a hi-res pass recomposes, it doesn't reuse the numbers.
    #[test]
    fn a_box_is_pixels_at_the_size_it_is_composed_for() {
        let r = [128.0 / 1024.0, 96.0 / 1024.0, 896.0 / 1024.0, 224.0 / 1024.0];
        assert_eq!(bbox(r, 1024, 1024), [-416, -384, -288, 384]);
        assert_eq!(bbox(r, 2048, 2048), [-832, -768, -576, 768]);
        // A wide frame: x is measured against the width, y the height.
        assert_eq!(bbox([0.0, 0.0, 1.0, 1.0], 1344, 768), [-384, -672, 384, 672]);
        // Fractional edges enclose outwards, and a sliver keeps a pixel.
        assert_eq!(bbox([0.1001, 0.5, 0.1002, 0.5001], 1000, 1000), [0, -400, 1, -399]);
    }

    #[test]
    fn a_box_without_its_caption_or_its_words_is_refused() {
        let ok = item(Label::Text, "title", Some("HELLO"), [0.1, 0.1, 0.9, 0.3]);
        assert!(Layout { items: vec![ok.clone()] }.validate().is_ok());
        for bad in [
            Item { caption: " ".into(), ..ok.clone() },
            Item { text: None, ..ok.clone() },
            Item { rect: [0.5, 0.1, 0.4, 0.3], ..ok.clone() },
            Item { rect: [0.1, 0.1, 1.2, 0.3], ..ok.clone() },
            Item { rect: [f64::NAN, 0.1, 0.9, 0.3], ..ok.clone() },
        ] {
            assert!(Layout { items: vec![bad.clone()] }.validate().is_err(), "{bad:?}");
        }
        // An object needs no words.
        assert!(Layout { items: vec![item(Label::Object, "a cat", None, [0.0, 0.0, 1.0, 1.0])] }.validate().is_ok());
        assert!(Layout { items: vec![ok; MAX_ITEMS + 1] }.validate().is_err());
    }

    /// The wire shape the page sends and a sidecar stores.
    #[test]
    fn a_layout_round_trips_through_json() {
        let l: Layout = serde_json::from_str(
            r#"{"items":[{"label":"character","caption":"a knight","rect":[0,0.25,0.5,1]}]}"#,
        )
        .unwrap();
        assert_eq!(l.items[0].label, Label::Character);
        assert_eq!(serde_json::from_str::<Layout>(&serde_json::to_string(&l).unwrap()).unwrap(), l);
    }

    #[test]
    fn variables_roll_inside_the_boxes() {
        let l = Layout { items: vec![item(Label::Text, "{c} title", Some("{w}"), [0.0, 0.0, 1.0, 1.0])] };
        let roll = |s: &str| Ok(s.replace("{c}", "red").replace("{w}", "HI"));
        let s = compose("x", &l, 64, 64, &roll).unwrap();
        assert!(s.contains(r#""caption":"red title","text":"HI""#), "{s}");
    }
}
