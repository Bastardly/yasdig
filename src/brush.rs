//! The viewer's brushes: Blur, Sharpen, Dodge, Burn, Saturate, Desaturate.
//!
//! Each tool is a painted mask and an amount. `pipeline::publish` runs them
//! after the whole-picture look and the area: per tool, the picture with that
//! tool's effect at full strength is made, and blended in by the mask's alpha
//! (`area::composite_by`, the same blend an area uses). So a stroke is live —
//! moving the amount re-filters it — until *Apply adjustments* bakes it in.
//!
//! **Masks are in pixels**, unlike an area's fractions: a stroke is where it
//! was painted. A crop crops them (`crop_mask`); any other change of size — a
//! Hi-res, an Upscale, a History jump across a crop — is met by resizing the
//! mask to the picture when it is read ([`weights`]).
//!
//! Unsigned, named tools rather than one signed "light" brush, as photo
//! editors have them: what a stroke does shouldn't flip with a slider.

use crate::config::Filters;
use anyhow::{bail, Context, Result};
use image::RgbaImage;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tool {
    Blur,
    Sharpen,
    Dodge,
    Burn,
    Saturate,
    Desaturate,
}

impl Tool {
    /// The order they run in, which is also the map's (`Ord` follows the
    /// declaration). Blur before Sharpen, so a sharpened stroke beside a
    /// blurred one isn't softened afterwards; tone before colour, as the grade.
    pub const ORDER: [Tool; 6] =
        [Tool::Blur, Tool::Sharpen, Tool::Dodge, Tool::Burn, Tool::Saturate, Tool::Desaturate];

    pub fn as_str(self) -> &'static str {
        match self {
            Tool::Blur => "blur",
            Tool::Sharpen => "sharpen",
            Tool::Dodge => "dodge",
            Tool::Burn => "burn",
            Tool::Saturate => "saturate",
            Tool::Desaturate => "desaturate",
        }
    }

    pub fn parse(s: &str) -> Option<Tool> {
        Tool::ORDER.into_iter().find(|t| t.as_str() == s)
    }

    /// The filters that make this tool's effect at `amount`, or `None` for
    /// Blur, which is not one of the FSR passes and runs here ([`blur`]).
    pub fn filters(self, amount: f32) -> Option<Filters> {
        let f = Filters::NONE;
        Some(match self {
            Tool::Blur => return None,
            Tool::Sharpen => Filters { sharpness: amount, ..f },
            Tool::Dodge => Filters { lightness: amount, ..f },
            Tool::Burn => Filters { lightness: -amount, ..f },
            Tool::Saturate => Filters { saturation: amount, ..f },
            Tool::Desaturate => Filters { saturation: -amount, ..f },
        })
    }
}

/// One tool's strokes: the mask file (scratch-owned, like everything a
/// `Pending` holds — invariant 8) and how strong the effect is under it.
#[derive(Debug, Clone, PartialEq)]
pub struct Stroke {
    pub mask: PathBuf,
    pub amount: f32,
}

pub type Brushes = BTreeMap<Tool, Stroke>;

pub fn validate_amount(amount: f32) -> Result<()> {
    if !amount.is_finite() || !(0.0..=1.0).contains(&amount) {
        bail!("a brush's amount must be between 0 and 1");
    }
    Ok(())
}

/// Blur's σ at full amount, as a share of the picture's short side: 1% is a
/// soft background at any size, the way the detail filters size their band
/// from the image (invariant 6).
const BLUR_REACH: f32 = 0.01;

pub fn blur_sigma(amount: f32, w: u32, h: u32) -> f32 {
    amount * BLUR_REACH * w.min(h) as f32
}

/// A Gaussian blur of σ, as three box blurs — within a few percent of the
/// real kernel and O(n) whatever σ is, which matters at 3072² with a σ of 30.
/// σ below half a pixel is no blur at all and returns the input.
pub fn blur(img: &RgbaImage, sigma: f32) -> RgbaImage {
    // The box radius whose three passes have variance σ²: a box of width
    // 2r + 1 has variance ((2r + 1)² − 1) / 12, and three add up.
    let r = (((4.0 * sigma * sigma + 1.0).sqrt() - 1.0) / 2.0).round() as usize;
    if r == 0 {
        return img.clone();
    }
    let (w, h) = (img.width() as usize, img.height() as usize);
    let mut a: Vec<f32> = img.as_raw().iter().map(|&v| v as f32).collect();
    let mut b = vec![0.0f32; a.len()];
    for _ in 0..3 {
        box_pass(&a, &mut b, w, h, r, 4, w * 4);
        box_pass(&b, &mut a, h, w, r, w * 4, 4);
    }
    let out: Vec<u8> = a.iter().map(|v| v.round().clamp(0.0, 255.0) as u8).collect();
    RgbaImage::from_raw(img.width(), img.height(), out).expect("same size as the input")
}

/// One box pass along a line: `len` samples `step` apart, `lines` lines
/// `stride` apart, four channels interleaved. A running sum, edges clamped.
fn box_pass(src: &[f32], dst: &mut [f32], len: usize, lines: usize, r: usize, step: usize, stride: usize) {
    let n = (2 * r + 1) as f32;
    for line in 0..lines {
        let base = line * stride;
        for c in 0..4 {
            let at = |i: isize| src[base + i.clamp(0, len as isize - 1) as usize * step + c];
            let mut sum: f32 = (-(r as isize)..=r as isize).map(at).sum();
            for i in 0..len {
                dst[base + i * step + c] = sum / n;
                sum += at(i as isize + r as isize + 1) - at(i as isize - r as isize);
            }
        }
    }
}

/// A mask as per-pixel weights for a `w × h` picture: its alpha, 0..1,
/// resized first when the picture's size has changed since it was painted.
pub fn weights(mask: &RgbaImage, w: u32, h: u32) -> Vec<f32> {
    let resized;
    let m = if mask.dimensions() == (w, h) {
        mask
    } else {
        resized = image::imageops::resize(mask, w, h, image::imageops::FilterType::Triangle);
        &resized
    };
    m.pixels().map(|p| p.0[3] as f32 / 255.0).collect()
}

/// Run every tool with strokes over `img`, in [`Tool::ORDER`]. `filter` is
/// the FSR pass (`Fsr::filter`); Blur runs here. A mask that can't be read is
/// skipped with a log line: a stroke lost is better than an image that can no
/// longer be adjusted.
pub fn apply(
    img: &mut RgbaImage,
    brushes: &Brushes,
    mut filter: impl FnMut(&RgbaImage, &Filters) -> Result<RgbaImage>,
) -> Result<()> {
    let (w, h) = img.dimensions();
    for (tool, stroke) in brushes {
        if stroke.amount <= 0.0 {
            continue;
        }
        let mask = match image::open(&stroke.mask) {
            Ok(m) => m.to_rgba8(),
            Err(e) => {
                eprintln!("[yasdig] skipping the {} strokes: {e}", tool.as_str());
                continue;
            }
        };
        let local = match tool.filters(stroke.amount) {
            Some(f) => filter(img, &f)?,
            None => blur(img, blur_sigma(stroke.amount, w, h)),
        };
        crate::area::composite_by(img, &local, &weights(&mask, w, h));
    }
    Ok(())
}

/// A mask cropped as the picture was: `(x, y, w, h)` of a picture `full_w ×
/// full_h`. A mask painted at another size is fitted to the picture first, so
/// the crop lands on the same strokes.
pub fn crop_mask(src: &Path, dst: &Path, (x, y, w, h): (u32, u32, u32, u32), full_w: u32, full_h: u32) -> Result<()> {
    let mut m = image::open(src).with_context(|| format!("reading {}", src.display()))?.to_rgba8();
    if m.dimensions() != (full_w, full_h) {
        m = image::imageops::resize(&m, full_w, full_h, image::imageops::FilterType::Triangle);
    }
    image::imageops::crop_imm(&m, x, y, w, h)
        .to_image()
        .save(dst)
        .with_context(|| format!("writing {}", dst.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blur_keeps_a_flat_picture_flat_and_spreads_a_spike() {
        let flat = RgbaImage::from_pixel(40, 30, image::Rgba([90, 120, 30, 255]));
        assert_eq!(blur(&flat, 4.0), flat, "edges are clamped, so nothing drifts in");
        let mut spike = RgbaImage::from_pixel(41, 41, image::Rgba([0, 0, 0, 255]));
        spike.put_pixel(20, 20, image::Rgba([255, 255, 255, 255]));
        assert_eq!(blur(&spike, 0.2), spike, "under half a pixel is no blur");
        let b = blur(&spike, 3.0);
        assert!(b.get_pixel(20, 20).0[0] < 50, "the peak spreads out");
        assert!(b.get_pixel(23, 20).0[0] > 0, "to its neighbours");
        assert_eq!(b.get_pixel(0, 0).0[0], 0, "and not across the picture");
    }

    #[test]
    fn a_mask_weighs_by_its_alpha_and_fits_the_picture() {
        let mut m = RgbaImage::from_pixel(4, 4, image::Rgba([255, 255, 255, 0]));
        m.put_pixel(1, 1, image::Rgba([255, 255, 255, 255]));
        let w = weights(&m, 4, 4);
        assert_eq!((w[5], w[0]), (1.0, 0.0));
        assert_eq!(weights(&m, 8, 2).len(), 16, "resized to the picture");
    }

    #[test]
    fn every_tool_but_blur_is_a_filter_and_names_round_trip() {
        for t in Tool::ORDER {
            assert_eq!(Tool::parse(t.as_str()), Some(t));
            assert_eq!(t.filters(0.5).is_none(), t == Tool::Blur);
        }
        assert_eq!(Tool::Burn.filters(0.3).unwrap().lightness, -0.3);
        assert_eq!(Tool::Desaturate.filters(0.3).unwrap().saturation, -0.3);
        assert!(validate_amount(1.5).is_err() && validate_amount(f32::NAN).is_err());
    }

    /// Strokes touch only what they cover: outside the mask, the picture is
    /// its own bytes.
    #[test]
    fn strokes_leave_unpainted_pixels_alone() {
        let dir = std::env::temp_dir().join(format!("yasdig-brush-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mask = dir.join("m.png");
        let mut m = RgbaImage::from_pixel(16, 16, image::Rgba([255, 255, 255, 0]));
        for x in 0..4 {
            m.put_pixel(x, 0, image::Rgba([255, 255, 255, 255]));
        }
        m.save(&mask).unwrap();
        let src = RgbaImage::from_fn(16, 16, |x, y| image::Rgba([x as u8 * 9, y as u8 * 9, 40, 255]));
        let mut img = src.clone();
        let brushes = Brushes::from([(Tool::Dodge, Stroke { mask, amount: 0.5 })]);
        let white = |i: &RgbaImage, _: &Filters| Ok(RgbaImage::from_pixel(i.width(), i.height(), image::Rgba([255; 4])));
        apply(&mut img, &brushes, white).unwrap();
        assert_eq!(img.get_pixel(0, 0).0, [255; 4]);
        assert_eq!(img.get_pixel(5, 5), src.get_pixel(5, 5));
        // A mask that has gone is skipped, not an error.
        let gone = Brushes::from([(Tool::Burn, Stroke { mask: dir.join("nope.png"), amount: 0.5 })]);
        assert!(apply(&mut img, &gone, white).is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }
}
