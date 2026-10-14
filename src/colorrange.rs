//! Selecting by colour: the viewer's Area picked by hue, lightness and
//! saturation as well as (or instead of) a shape.
//!
//! **In OKLCh**, the grade pass's own space (`grade.wgsl`), so "hue 30°"
//! means the same on the wheel, in the selection and in the Hue slider:
//! hue is `atan2(b, a)` in degrees, lightness `L × 100`, saturation the
//! chroma `√(a² + b²)` as a share of [`FULL_CHROMA`], × 100.
//!
//! A range is four numbers ([`Ramp`]): fully selected from `b` to `c`, fading
//! to nothing at `a` and `d` along a smoothstep, so a strong look shows no
//! line where the fade starts. Hue wraps: a red range is `330, 340, 380, 390`
//! — kept unwrapped, `a` in 0..360 and the rest counting on past 360 — so
//! "340° to 20°" needs no special case anywhere.
//!
//! `web/js/colorrange.js` is this file again for the page's live preview and
//! its wheel; one fixture table, here and in `colorrange.test.js`, keeps the
//! two from drifting.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// The chroma counted as 100% saturation: about a vivid sRGB primary's
/// (OKLab chroma runs to 0.26–0.32 on them).
pub const FULL_CHROMA: f32 = 0.32;

/// Saturation below which a hue is not yet a hue: the hue weight fades in
/// from 0 at a grey to full here, so a hue range never picks greys whose
/// `atan2` angle is noise.
const HUE_FROM_SATURATION: f32 = 10.0;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Ramp {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
}

impl Ramp {
    /// 1 from `b` to `c`, a smoothstep down to 0 at `a` and `d`, 0 outside.
    /// Smooth rather than straight: a straight fade has a corner at each end,
    /// and a strong look shows the corner as a line in the picture.
    pub fn weight(&self, v: f32) -> f32 {
        let t = if v < self.a || v > self.d {
            0.0
        } else if v < self.b {
            if self.b > self.a { (v - self.a) / (self.b - self.a) } else { 1.0 }
        } else if v <= self.c {
            1.0
        } else if self.d > self.c {
            (self.d - v) / (self.d - self.c)
        } else {
            1.0
        };
        t * t * (3.0 - 2.0 * t)
    }

    /// A hue's weight: the angle moved into `a .. a + 360` first, so a range
    /// that runs past 360° wraps.
    pub fn hue_weight(&self, h: f32) -> f32 {
        self.weight(self.a + (h - self.a).rem_euclid(360.0))
    }

    fn validate(&self, what: &str, max: f32, hue: bool) -> Result<()> {
        let all = [self.a, self.b, self.c, self.d];
        if all.iter().any(|v| !v.is_finite()) || !(self.a <= self.b && self.b <= self.c && self.c <= self.d) {
            bail!("the {what} range must be four numbers in order");
        }
        let ok = if hue {
            (0.0..360.0).contains(&self.a) && self.d - self.a <= 360.0
        } else {
            self.a >= 0.0 && self.d <= max
        };
        if !ok {
            bail!("the {what} range is outside 0..{max}");
        }
        Ok(())
    }
}

/// What an area selects by colour. A channel left `None` selects everything.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct ColorRange {
    #[serde(default)]
    pub hue: Option<Ramp>,
    #[serde(default)]
    pub lightness: Option<Ramp>,
    #[serde(default)]
    pub saturation: Option<Ramp>,
    /// How far the selection's edge is spread in the picture, 0..1: a blur
    /// of the colour weight ([`ColorRange::feather_sigma`]), so a colour that
    /// changes abruptly — or noisily — doesn't leave a hard edge in the look.
    #[serde(default)]
    pub feather: f32,
}

/// The feather's σ at 1, as a share of the picture's short side: 2% is a
/// soft halo at any size (invariant 6's rule, as the brushes' Blur).
const FEATHER_REACH: f32 = 0.02;

impl ColorRange {
    pub fn validate(&self) -> Result<()> {
        if let Some(r) = &self.hue {
            r.validate("hue", 360.0, true)?;
        }
        if let Some(r) = &self.lightness {
            r.validate("lightness", 100.0, false)?;
        }
        if let Some(r) = &self.saturation {
            r.validate("saturation", 100.0, false)?;
        }
        if !(0.0..=1.0).contains(&self.feather) {
            bail!("the colour feather must be between 0 and 1");
        }
        Ok(())
    }

    /// Whether it narrows anything: three `None`s select every pixel.
    pub fn narrows(&self) -> bool {
        self.hue.is_some() || self.lightness.is_some() || self.saturation.is_some()
    }

    /// The feather as a blur σ in pixels of a `w × h` picture.
    pub fn feather_sigma(&self, w: u32, h: u32) -> f32 {
        self.feather * FEATHER_REACH * w.min(h) as f32
    }

    /// How selected an sRGB pixel is, 0..1, before the feather.
    pub fn weight(&self, rgb: [u8; 3]) -> f32 {
        let (h, l, s) = oklch(rgb);
        let hue = self.hue.map_or(1.0, |r| r.hue_weight(h) * (s / HUE_FROM_SATURATION).min(1.0));
        let light = self.lightness.map_or(1.0, |r| r.weight(l));
        let sat = self.saturation.map_or(1.0, |r| r.weight(s));
        hue * light * sat
    }
}

/// sRGB 0..255 → linear light, once: a frame is millions of lookups.
fn linear(v: u8) -> f32 {
    static TABLE: OnceLock<[f32; 256]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t = [0.0; 256];
        for (i, x) in t.iter_mut().enumerate() {
            let c = i as f32 / 255.0;
            *x = if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
        }
        t
    })[v as usize]
}

/// An sRGB pixel as (hue°, lightness 0..100, saturation 0..100) in OKLCh —
/// the matrices `grade.wgsl` uses, Björn Ottosson's.
///
/// In f64: the matrices carry ten digits, which f32 would round away.
pub fn oklch([r, g, b]: [u8; 3]) -> (f32, f32, f32) {
    let (r, g, b) = (linear(r) as f64, linear(g) as f64, linear(b) as f64);
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    let big_l = 0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s;
    let a = 1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s;
    let bb = 0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s;
    let hue = bb.atan2(a).to_degrees().rem_euclid(360.0) as f32;
    let chroma = (a * a + bb * bb).sqrt() as f32;
    (hue, big_l as f32 * 100.0, (chroma / FULL_CHROMA * 100.0).min(100.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ranges and pixels `web/js/colorrange.test.js` checks too, with the
    /// same expected weights: change both or neither.
    pub(crate) const RED: ColorRange = ColorRange {
        hue: Some(Ramp { a: 340.0, b: 350.0, c: 400.0, d: 410.0 }),
        lightness: None,
        saturation: None,
        feather: 0.0,
    };
    pub(crate) const SHADOWS: ColorRange = ColorRange {
        hue: None,
        lightness: Some(Ramp { a: 0.0, b: 0.0, c: 30.0, d: 60.0 }),
        saturation: None,
        feather: 0.0,
    };

    #[test]
    fn the_fixture_table_the_page_checks_too() {
        let table: [(ColorRange, [u8; 3], f32); 6] = [
            (RED, [220, 30, 40], 1.0),
            (RED, [30, 60, 220], 0.0),
            (RED, [128, 128, 128], 0.0),
            (SHADOWS, [10, 10, 10], 1.0),
            (SHADOWS, [250, 250, 250], 0.0),
            (SHADOWS, [80, 80, 80], 0.593),
        ];
        for (range, rgb, want) in table {
            let got = range.weight(rgb);
            assert!((got - want).abs() < 0.005, "{rgb:?}: {got} not {want}");
        }
    }

    #[test]
    fn a_hue_range_wraps_past_360() {
        let r = RED.hue.unwrap();
        assert_eq!(r.hue_weight(355.0), 1.0);
        assert_eq!(r.hue_weight(30.0), 1.0);
        assert_eq!(r.hue_weight(45.0), 0.5);
        assert_eq!(r.hue_weight(345.0), 0.5);
        assert_eq!(r.hue_weight(180.0), 0.0);
    }

    #[test]
    fn oklch_reads_the_primaries_and_greys_as_expected() {
        let (_, l, s) = oklch([255, 255, 255]);
        assert!((l - 100.0).abs() < 0.1 && s < 0.1);
        let (h, _, s) = oklch([255, 0, 0]);
        assert!((25.0..35.0).contains(&h) && s > 75.0, "red at {h}°, {s}%");
        let (h, ..) = oklch([0, 0, 255]);
        assert!((260.0..270.0).contains(&h), "blue at {h}°");
    }

    #[test]
    fn a_malformed_range_is_refused() {
        assert!(RED.validate().is_ok());
        let bad = |r: Ramp| ColorRange { lightness: Some(r), ..Default::default() }.validate().is_err();
        assert!(bad(Ramp { a: 10.0, b: 5.0, c: 20.0, d: 30.0 }), "out of order");
        assert!(bad(Ramp { a: 0.0, b: 0.0, c: 50.0, d: 120.0 }), "past 100");
        let wide = ColorRange { hue: Some(Ramp { a: 10.0, b: 20.0, c: 300.0, d: 400.0 }), ..Default::default() };
        assert!(wide.validate().is_err(), "more than a full turn");
        assert!(!ColorRange::default().narrows());
        assert!(ColorRange { feather: 1.5, ..RED }.validate().is_err(), "feather past 1");
    }
}
