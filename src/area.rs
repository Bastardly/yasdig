//! An area of the picture with a look of its own: a rectangle or an ellipse,
//! a feathered edge, and its own set of the viewer's filters, applied on top
//! of the whole-image look and only inside the shape.
//!
//! Pure geometry and compositing, like `redraw.rs`, so it is tested without a
//! GPU. `pipeline::publish` runs the filters (twice: the whole image, then the
//! area's set on top) and calls [`composite`] to blend the two.
//!
//! **The shape is in fractions of the frame**, 0..1 on both axes, not pixels.
//! That is what lets it survive a Hi-res or an Upscale — the picture is the
//! same picture, larger — with nothing to translate. A crop is the one change
//! of frame, and [`Area::crop`] maps it.
//!
//! One area per image. Brushes, when they come, paint the same weight this
//! module computes analytically; the compositing below does not care where a
//! weight came from.

use crate::config::Filters;
use anyhow::{bail, Result};
use image::RgbaImage;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Shape {
    Rect,
    Ellipse,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Area {
    pub shape: Shape,
    /// The bounding box, as fractions of the frame: left, top, right, bottom.
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    /// Width of the soft edge, as a fraction of the frame's shorter side,
    /// centred on the shape's outline. 0 is a hard edge.
    pub feather: f32,
    /// Everything *but* the shape.
    #[serde(default)]
    pub invert: bool,
    /// The area's own look. Starts neutral; it is applied over the whole-image
    /// look, so a 0 here means "as the rest of the picture".
    pub filters: Filters,
}

/// The widest soft edge: half the shorter side, past which an ellipse's edge
/// would reach the far side of a small area and the shape stops reading.
pub const MAX_FEATHER: f32 = 0.5;

impl Area {
    pub fn validate(&self) -> Result<()> {
        let all = [self.x0, self.y0, self.x1, self.y1, self.feather];
        if all.iter().any(|v| !v.is_finite()) {
            bail!("the area's shape must be numbers");
        }
        if [self.x0, self.y0, self.x1, self.y1].iter().any(|v| !(0.0..=1.0).contains(v)) {
            bail!("the area must lie inside the picture");
        }
        if self.x0 >= self.x1 || self.y0 >= self.y1 {
            bail!("the area must have a width and a height");
        }
        if !(0.0..=MAX_FEATHER).contains(&self.feather) {
            bail!("the area's feather must be between 0 and {MAX_FEATHER}");
        }
        self.filters.validate("the area")
    }

    /// Whether it changes anything: an area whose filters are all off is a
    /// shape with nothing in it, and publishing skips it outright.
    pub fn acts(&self) -> bool {
        self.filters != Filters::NONE
    }

    /// What it becomes when a pass replaces the pixels: the shape and the
    /// colour carry, detail resets to off — [`Filters::restart`]'s rule, with
    /// the area's own start point, which is 0.
    pub fn restart(&self) -> Area {
        Area {
            filters: self.filters.restart(0.0, 0.0),
            ..*self
        }
    }

    /// The same area in a cropped frame: `(x, y, w, h)` in pixels of a
    /// `full_w × full_h` picture. `None` when nothing of it is left inside the
    /// crop — or, inverted, when the crop lies wholly inside the shape's box,
    /// which leaves the inverse nothing either way it is read; dropping it is
    /// simpler than keeping a box that has left the frame.
    pub fn crop(&self, (x, y, w, h): (u32, u32, u32, u32), full_w: u32, full_h: u32) -> Option<Area> {
        let (fw, fh) = (full_w as f32, full_h as f32);
        let (cx, cy, cw, ch) = (x as f32 / fw, y as f32 / fh, w as f32 / fw, h as f32 / fh);
        let map = |v: f32, o: f32, s: f32| ((v - o) / s).clamp(0.0, 1.0);
        let short_old = fw.min(fh);
        let short_new = (w as f32).min(h as f32);
        let a = Area {
            x0: map(self.x0, cx, cw),
            y0: map(self.y0, cy, ch),
            x1: map(self.x1, cx, cw),
            y1: map(self.y1, cy, ch),
            // The edge keeps its width in pixels: a fraction of the old short
            // side, re-expressed as one of the new.
            feather: (self.feather * short_old / short_new.max(1.0)).min(MAX_FEATHER),
            ..*self
        };
        (a.x0 < a.x1 && a.y0 < a.y1).then_some(a)
    }

    /// How much of the area's look pixel `(px, py)` of a `w × h` picture gets:
    /// 1 inside, 0 outside, a smoothstep across the feathered edge.
    pub fn weight(&self, px: u32, py: u32, w: u32, h: u32) -> f32 {
        // Pixel centres, in pixels, so a feather is round on a non-square frame.
        let (x, y) = (px as f32 + 0.5, py as f32 + 0.5);
        let (l, t) = (self.x0 * w as f32, self.y0 * h as f32);
        let (r, b) = (self.x1 * w as f32, self.y1 * h as f32);
        // Signed distance to the outline in pixels, negative inside.
        let d = match self.shape {
            Shape::Rect => {
                let dx = (l - x).max(x - r);
                let dy = (t - y).max(y - b);
                let outside = (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt();
                outside + dx.max(dy).min(0.0)
            }
            Shape::Ellipse => {
                let (cx, cy) = ((l + r) / 2.0, (t + b) / 2.0);
                let (rx, ry) = (((r - l) / 2.0).max(0.5), ((b - t) / 2.0).max(0.5));
                let (nx, ny) = ((x - cx) / rx, (y - cy) / ry);
                let k = (nx * nx + ny * ny).sqrt();
                // The radial distance scaled by the radius along this
                // direction: exact on the axes, close enough between them for
                // a soft edge, and cheap.
                let along = if k > 0.0 { rx * ry / ((ry * nx).hypot(rx * ny) / k) } else { rx.min(ry) };
                (k - 1.0) * along
            }
        };
        let soft = self.feather * w.min(h) as f32;
        let inside = if soft <= 0.0 {
            if d <= 0.0 { 1.0 } else { 0.0 }
        } else {
            let s = ((soft / 2.0 - d) / soft).clamp(0.0, 1.0);
            s * s * (3.0 - 2.0 * s)
        };
        if self.invert { 1.0 - inside } else { inside }
    }
}

/// Blend `local` (the picture with the area's look on top) into `base` by the
/// area's weight. A pixel at weight 0 keeps `base`'s exact bytes and one at 1
/// takes `local`'s, so outside the shape and its edge nothing moves at all.
pub fn composite(base: &mut RgbaImage, local: &RgbaImage, area: &Area) {
    let (w, h) = base.dimensions();
    debug_assert_eq!((w, h), local.dimensions());
    for (x, y, px) in base.enumerate_pixels_mut() {
        let k = area.weight(x, y, w, h);
        if k <= 0.0 {
            continue;
        }
        let over = local.get_pixel(x, y);
        if k >= 1.0 {
            *px = *over;
            continue;
        }
        for c in 0..4 {
            let a = px.0[c] as f32;
            px.0[c] = (a + (over.0[c] as f32 - a) * k).round() as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(shape: Shape) -> Area {
        Area {
            shape,
            x0: 0.25,
            y0: 0.25,
            x1: 0.75,
            y1: 0.75,
            feather: 0.1,
            invert: false,
            filters: Filters { lightness: 0.3, ..Filters::NONE },
        }
    }

    #[test]
    fn the_weight_is_one_inside_zero_outside_and_soft_between() {
        for shape in [Shape::Rect, Shape::Ellipse] {
            let a = area(shape);
            assert_eq!(a.weight(100, 100, 200, 200), 1.0, "{shape:?} centre");
            assert_eq!(a.weight(2, 2, 200, 200), 0.0, "{shape:?} corner");
            // On the outline, the middle of the edge.
            let edge = a.weight(150, 100, 200, 200);
            assert!((0.3..0.7).contains(&edge), "{shape:?} edge {edge}");
            let inv = Area { invert: true, ..a };
            assert_eq!(inv.weight(100, 100, 200, 200), 0.0);
            assert_eq!(inv.weight(2, 2, 200, 200), 1.0);
        }
        // A hard edge is hard.
        let hard = Area { feather: 0.0, ..area(Shape::Rect) };
        // The box starts at x = 50: pixel 50's centre is inside, 49's is not.
        assert_eq!(hard.weight(50, 100, 200, 200), 1.0);
        assert_eq!(hard.weight(49, 100, 200, 200), 0.0);
    }

    /// The promise that makes an area safe to leave on: nothing outside its
    /// edge is touched, to the byte.
    #[test]
    fn pixels_outside_the_edge_are_the_original_bytes() {
        let base = RgbaImage::from_fn(64, 64, |x, y| image::Rgba([x as u8, y as u8, 7, 255]));
        let local = RgbaImage::from_pixel(64, 64, image::Rgba([200, 200, 200, 255]));
        let a = area(Shape::Ellipse);
        let mut out = base.clone();
        composite(&mut out, &local, &a);
        for (x, y, p) in out.enumerate_pixels() {
            let k = a.weight(x, y, 64, 64);
            if k == 0.0 {
                assert_eq!(p, base.get_pixel(x, y), "({x},{y}) moved");
            } else if k == 1.0 {
                assert_eq!(p, local.get_pixel(x, y), "({x},{y}) not taken");
            }
        }
        assert_eq!(out.get_pixel(32, 32), local.get_pixel(32, 32));
        assert_eq!(out.get_pixel(0, 0), base.get_pixel(0, 0));
    }

    #[test]
    fn a_crop_keeps_the_area_on_the_same_pixels() {
        let a = area(Shape::Rect);
        // The right half of a 400×200 picture.
        let c = a.crop((200, 0, 200, 200), 400, 200).unwrap();
        // x 0.25..0.75 of 400 is 100..300; in the crop that is 0..100 of 200.
        assert_eq!((c.x0, c.x1), (0.0, 0.5));
        assert_eq!((c.y0, c.y1), (0.25, 0.75));
        // The edge keeps its pixels: 0.1 × 200 before, 0.1 × 200 after.
        assert!((c.feather - 0.1).abs() < 1e-6);
        // Wholly outside the crop: gone.
        assert!(a.crop((0, 0, 80, 40), 400, 200).is_none());
    }

    #[test]
    fn a_pass_keeps_the_shape_and_colour_and_resets_detail() {
        let a = Area {
            filters: Filters { lightness: 0.3, sharpness: 0.5, clarity: 0.4, ..Filters::NONE },
            ..area(Shape::Ellipse)
        };
        let r = a.restart();
        assert_eq!((r.x0, r.shape), (a.x0, a.shape));
        assert_eq!(r.filters.lightness, 0.3);
        assert_eq!((r.filters.sharpness, r.filters.clarity), (0.0, 0.0));
    }

    #[test]
    fn a_malformed_area_is_refused() {
        let ok = area(Shape::Rect);
        assert!(ok.validate().is_ok());
        assert!(Area { x0: 0.8, ..ok }.validate().is_err(), "no width");
        assert!(Area { x1: 1.5, ..ok }.validate().is_err(), "outside");
        assert!(Area { feather: f32::NAN, ..ok }.validate().is_err());
        assert!(Area { feather: 0.9, ..ok }.validate().is_err());
        let wild = Area { filters: Filters { lightness: 3.0, ..Filters::NONE }, ..ok };
        assert!(wild.validate().is_err(), "its filters go through the same bounds");
        assert!(!Area { filters: Filters::NONE, ..ok }.acts());
    }
}
