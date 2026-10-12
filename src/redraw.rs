//! Redraw: the geometry and the compositing around one masked img2img pass.
//!
//! A painted mask says which pixels to redraw. Diffusing the whole image with
//! that mask would redraw an 80-pixel face at 80 pixels, which comes back as
//! bad as the one it replaces — so the pass works on a *crop*: the mask's box
//! plus some context ([`context_rect`]), enlarged to the size the model was
//! trained at ([`work_size`]), diffused, and shrunk back. Forge calls this
//! "inpaint only masked".
//!
//! The result is then laid over the original in **pixel space**, through a
//! feathered copy of the mask ([`feather`], [`composite`]), rather than taken
//! whole from sd-cli. Every VAE round trip shifts colour and softens a little
//! everywhere, so the pass's own frame would quietly change the whole picture
//! and leave a seam at the crop's edge. Composited, every pixel the feather
//! leaves at zero is the original's bytes — which is the guarantee this module
//! exists to keep, and `untouched_pixels_are_the_original_bytes` pins it.
//!
//! Everything here is a pure function of image buffers, so it is tested without
//! a GPU; `Engine::redraw` is the orchestration.

use image::{GrayImage, RgbImage};

/// An axis-aligned box in image pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// What counts as painted. The page paints opaque white, and a PNG of it is
/// exact, but a mask from anywhere else may be antialiased — half and over is in.
const PAINTED: u8 = 128;

/// The box around every painted pixel, or `None` when nothing is painted.
pub fn mask_bbox(mask: &GrayImage) -> Option<Rect> {
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    for (x, y, p) in mask.enumerate_pixels() {
        if p.0[0] >= PAINTED {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    (x0 != u32::MAX).then(|| Rect { x: x0, y: y0, w: x1 - x0 + 1, h: y1 - y0 + 1 })
}

/// The painted box grown by some context, clamped to the image. The model has
/// to see what surrounds a hand to draw one that belongs there: 15% of the
/// box's long side, and never under 32 px, which is what a tiny mask needs —
/// plus the reach of the edge softness `soft`, since everything sd-cli redraws
/// ([`diffusion_mask`]) has to be inside the crop.
pub fn context_rect(b: Rect, img_w: u32, img_h: u32, soft: u32) -> Rect {
    let pad = ((b.w.max(b.h) as f32) * 0.15).round().max(32.0) as u32 + 3 * soft + 8;
    let x0 = b.x.saturating_sub(pad);
    let y0 = b.y.saturating_sub(pad);
    let x1 = (b.x + b.w + pad).min(img_w);
    let y1 = (b.y + b.h + pad).min(img_h);
    Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 }
}

/// The size the crop is diffused at: its long side at `target` (the model's own
/// base size), each axis a multiple of 64.
///
/// Both axes are snapped, unlike `fit_to`, and that is fine *here* because the
/// result is resized back to the crop's exact size before it is composited —
/// the few percent of aspect the snap costs is undone on the way back, and
/// sd.cpp's own rounding to the latent grid (invariant 5) never gets a size it
/// would change.
pub fn work_size(w: u32, h: u32, target: u32) -> (u32, u32) {
    let scale = target as f32 / w.max(h).max(1) as f32;
    let snap = |v: u32| (((v as f32 * scale) / 64.0).round() as u32).max(1) * 64;
    (snap(w), snap(h))
}

/// The mask as 0 or 255: what sd-cli is handed, and what the blend starts from.
pub fn binarize(mask: &GrayImage) -> GrayImage {
    let mut hard = mask.clone();
    for p in hard.pixels_mut() {
        p.0[0] = if p.0[0] >= PAINTED { 255 } else { 0 };
    }
    hard
}

/// Grow every painted pixel to a `2r+1` square — a separable running max, so
/// it costs the same at any radius. Square rather than round because it only
/// ever feeds a blur or sd-cli's latent grid, neither of which can tell.
pub fn dilate(mask: &GrayImage, r: u32) -> GrayImage {
    if r == 0 {
        return mask.clone();
    }
    let (w, h) = mask.dimensions();
    let pass = |src: &GrayImage, horizontal: bool| {
        GrayImage::from_fn(w, h, |x, y| {
            let (c, n) = if horizontal { (x, w) } else { (y, h) };
            let lo = c.saturating_sub(r);
            let hi = (c + r).min(n - 1);
            let v = (lo..=hi)
                .map(|k| if horizontal { src.get_pixel(k, y).0[0] } else { src.get_pixel(x, k).0[0] })
                .max()
                .unwrap_or(0);
            image::Luma([v])
        })
    };
    pass(&pass(mask, true), false)
}

/// The blend weights for an edge softness of `soft` pixels: 255 over
/// everything painted, falling to 0 about `2·soft` beyond it. The fade goes
/// *outward* — what was painted is replaced in full, which is what a brush
/// stroke promises — so the painted area is dilated by `soft` before the blur
/// (sigma `soft/2`), and the result is never below the hard mask: a blur
/// reaches only ~98% two sigmas in, and a painted pixel at 248 would keep a
/// sliver of what was asked to go. Pixels the fade never reaches stay exactly 0.
pub fn feather(mask: &GrayImage, soft: u32) -> GrayImage {
    let hard = binarize(mask);
    if soft == 0 {
        return hard;
    }
    let mut soft_edge = image::imageops::blur(&dilate(&hard, soft), soft as f32 / 2.0);
    for (a, h) in soft_edge.pixels_mut().zip(hard.pixels()) {
        a.0[0] = a.0[0].max(h.0[0]);
    }
    soft_edge
}

/// What sd-cli redraws for an edge softness of `soft`: past the whole fade,
/// so the blend mixes the original with pixels the model really drew rather
/// than with the original's own VAE round trip — that mix is the rough edge at
/// high strength. `+8` is one latent cell, which is how coarse sd.cpp's mask is.
pub fn diffusion_mask(mask: &GrayImage, soft: u32) -> GrayImage {
    dilate(&binarize(mask), 3 * soft + 8)
}

/// The largest edge softness a request may ask for, in image pixels.
pub const MAX_SOFTNESS: u32 = 128;

/// A side of the image to extend (outpainting).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Top,
    Right,
    Bottom,
}

impl Side {
    /// The order a multi-side extend runs in: left and right first, then top
    /// and bottom, so a top or bottom strip already spans the new width and
    /// draws the corners with both neighbours in view.
    pub const ORDER: [Side; 4] = [Side::Left, Side::Right, Side::Top, Side::Bottom];

    pub fn arrow(self) -> &'static str {
        match self {
            Side::Left => "←",
            Side::Top => "↑",
            Side::Right => "→",
            Side::Bottom => "↓",
        }
    }
}

/// The largest strip one side may grow by, in pixels. Past about half the
/// model's own size the new strip is mostly invention with little to continue.
pub const MAX_EXTEND: u32 = 512;

/// `base` grown by `px` on `side`, and the mask of the new strip.
///
/// The original is copied in untouched. The strip is not left empty: it is the
/// edge's own pixels stretched outward, then blurred hard (sigma a quarter of
/// the strip). FLUX.1 Fill is handed the image with the masked strip blanked
/// out, so what is in it matters less than it did for v1's ordinary model —
/// but the canvas is also the init sd.cpp encodes, and a blurred continuation
/// of the edge's colours is the one fill that can't print a streak or tint
/// the strip.
pub fn grow(base: &RgbImage, side: Side, px: u32) -> (RgbImage, GrayImage) {
    let (w, h) = base.dimensions();
    if px == 0 {
        return (base.clone(), GrayImage::new(w, h));
    }
    let (nw, nh, ox, oy) = match side {
        Side::Left => (w + px, h, px, 0),
        Side::Right => (w + px, h, 0, 0),
        Side::Top => (w, h + px, 0, px),
        Side::Bottom => (w, h + px, 0, 0),
    };
    let inside = |x: u32, y: u32| x >= ox && x < ox + w && y >= oy && y < oy + h;
    // Every pixel takes the nearest original pixel: the original itself inside,
    // its edge stretched outside.
    let stretched = RgbImage::from_fn(nw, nh, |x, y| {
        let sx = (x as i64 - ox as i64).clamp(0, w as i64 - 1) as u32;
        let sy = (y as i64 - oy as i64).clamp(0, h as i64 - 1) as u32;
        *base.get_pixel(sx, sy)
    });
    let blurred = image::imageops::blur(&stretched, (px as f32 / 4.0).max(2.0));
    let canvas = RgbImage::from_fn(nw, nh, |x, y| {
        if inside(x, y) {
            *stretched.get_pixel(x, y)
        } else {
            *blurred.get_pixel(x, y)
        }
    });
    let mask = GrayImage::from_fn(nw, nh, |x, y| image::Luma([if inside(x, y) { 0 } else { 255 }]));
    (canvas, mask)
}

/// The crop an Extend redraws its new strip in, on the grown canvas
/// (`canvas` is its size): the whole strip, and beside it **as much of the
/// original as makes the crop roughly square at the model's size** — at least
/// as wide as the strip itself, at most what the original has.
///
/// The area pass's own margin (`context_rect`) is right for a patch inside a
/// picture and wrong here: around a 384 px strip it gives ~200 px of picture,
/// too little scene to continue. With the crop at least half real picture the
/// Fill model has the scene to carry on — and the strip still gets the
/// model's full size, which a whole-image pass would squeeze.
pub fn extend_crop(canvas: (u32, u32), side: Side, px: u32, target: u32) -> Rect {
    let (w, h) = canvas;
    // Along the axis the strip grew: the strip plus the context.
    let along = match side {
        Side::Left | Side::Right => w,
        Side::Top | Side::Bottom => h,
    };
    let across = match side {
        Side::Left | Side::Right => h,
        Side::Top | Side::Bottom => w,
    };
    let want = across.min(target).saturating_sub(px).max(px);
    let ctx = want.min(along - px);
    let span = px + ctx;
    match side {
        Side::Left => Rect { x: 0, y: 0, w: span, h },
        Side::Right => Rect { x: w - span, y: 0, w: span, h },
        Side::Top => Rect { x: 0, y: 0, w, h: span },
        Side::Bottom => Rect { x: 0, y: h - span, w, h: span },
    }
}

/// Lay `patch` over `base` at `at`, weighted by `alpha` (both the patch's size).
/// Weight 0 leaves the original's bytes, 255 takes the patch's exactly.
pub fn composite(base: &mut RgbImage, patch: &RgbImage, alpha: &GrayImage, at: (u32, u32)) {
    for (x, y, a) in alpha.enumerate_pixels() {
        let a = a.0[0] as u32;
        if a == 0 {
            continue;
        }
        let (bx, by) = (at.0 + x, at.1 + y);
        if bx >= base.width() || by >= base.height() {
            continue;
        }
        let new = patch.get_pixel(x, y).0;
        for (o, n) in base.get_pixel_mut(bx, by).0.iter_mut().zip(new) {
            *o = ((a * n as u32 + (255 - a) * *o as u32 + 127) / 255) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Luma, Rgb};

    fn painted(w: u32, h: u32, r: Rect) -> GrayImage {
        GrayImage::from_fn(w, h, |x, y| {
            let inside = x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h;
            Luma([if inside { 255 } else { 0 }])
        })
    }

    #[test]
    fn the_box_is_every_painted_pixel_and_nothing_is_none() {
        assert_eq!(mask_bbox(&GrayImage::new(64, 32)), None);
        let one = Rect { x: 7, y: 3, w: 1, h: 1 };
        assert_eq!(mask_bbox(&painted(64, 32, one)), Some(one));
        let edge = Rect { x: 0, y: 0, w: 64, h: 32 };
        assert_eq!(mask_bbox(&painted(64, 32, edge)), Some(edge));
        // Antialiased below the half is not painted.
        let mut soft = GrayImage::new(8, 8);
        soft.put_pixel(2, 2, Luma([127]));
        assert_eq!(mask_bbox(&soft), None);
    }

    #[test]
    fn context_pads_and_stays_inside_the_image() {
        // soft 0 still reaches one latent cell (8) past the 32 px floor.
        let mid = context_rect(Rect { x: 500, y: 500, w: 100, h: 40 }, 1024, 1024, 0);
        assert_eq!(mid, Rect { x: 460, y: 460, w: 180, h: 120 });
        let big = context_rect(Rect { x: 400, y: 400, w: 400, h: 100 }, 1024, 1024, 0);
        assert_eq!((big.x, big.w), (332, 536), "15% of the long side");
        let soft = context_rect(Rect { x: 500, y: 500, w: 100, h: 40 }, 1024, 1024, 16);
        assert_eq!(soft.x, 460 - 48, "and the softness's reach");
        let corner = context_rect(Rect { x: 0, y: 1000, w: 10, h: 24 }, 1024, 1024, 0);
        assert_eq!(corner, Rect { x: 0, y: 960, w: 50, h: 64 });
    }

    #[test]
    fn the_work_size_is_the_models_and_on_the_latent_grid() {
        for (w, h) in [(80, 120), (164, 104), (1500, 700), (64, 64), (1, 900)] {
            let (ww, wh) = work_size(w, h, 1024);
            assert!(ww % 64 == 0 && wh % 64 == 0 && ww >= 64 && wh >= 64, "{w}x{h}");
            assert_eq!(ww.max(wh), 1024, "{w}x{h}: long side at the target");
            // Within one grid cell of the true aspect on the short side.
            let exact = w.min(h) as f32 * 1024.0 / w.max(h) as f32;
            assert!((ww.min(wh) as f32 - exact).abs() <= 32.0 || exact < 64.0, "{w}x{h}");
        }
    }

    /// The guarantee the design rests on: outside the feathered mask, the
    /// result is not "close to" the original, it is the original.
    #[test]
    fn untouched_pixels_are_the_original_bytes() {
        let (w, h) = (96, 80);
        let mut base = RgbImage::from_fn(w, h, |x, y| Rgb([x as u8, y as u8, (x * y) as u8]));
        let orig = base.clone();
        let mask = painted(w, h, Rect { x: 40, y: 30, w: 16, h: 12 });
        let alpha = feather(&mask, 6);
        let patch = RgbImage::from_pixel(w, h, Rgb([255, 0, 0]));
        composite(&mut base, &patch, &alpha, (0, 0));

        let mut changed = 0;
        for (x, y, a) in alpha.enumerate_pixels() {
            if a.0[0] == 0 {
                assert_eq!(base.get_pixel(x, y), orig.get_pixel(x, y), "({x},{y}) moved");
            } else {
                changed += 1;
            }
        }
        assert!(changed > 16 * 12, "the feather reaches past the painted box");
        assert_eq!(base.get_pixel(0, 0), orig.get_pixel(0, 0));
        // The middle of a painted area is the patch's, exactly.
        let solid = feather(&mask, 0);
        let mut b2 = orig.clone();
        composite(&mut b2, &patch, &solid, (0, 0));
        assert_eq!(b2.get_pixel(48, 36), &Rgb([255, 0, 0]));
    }

    #[test]
    fn the_feather_falls_off_monotonically_across_an_edge() {
        let mask = painted(96, 16, Rect { x: 48, y: 0, w: 48, h: 16 });
        let a = feather(&mask, 8);
        let row: Vec<u8> = (0..96).map(|x| a.get_pixel(x, 8).0[0]).collect();
        assert!(row.windows(2).all(|p| p[0] <= p[1]), "{row:?}");
        assert_eq!(row[0], 0);
        // Everything painted is replaced in full: the fade is outside it.
        assert!(row[48..].iter().all(|&v| v == 255), "{row:?}");
        assert!(row[40] > 0, "and reaches past the stroke");
    }

    #[test]
    fn dilation_grows_a_square_and_stays_inside_the_image() {
        let one = painted(20, 20, Rect { x: 10, y: 10, w: 1, h: 1 });
        assert_eq!(mask_bbox(&dilate(&one, 3)), Some(Rect { x: 7, y: 7, w: 7, h: 7 }));
        let corner = painted(20, 20, Rect { x: 0, y: 19, w: 1, h: 1 });
        assert_eq!(mask_bbox(&dilate(&corner, 4)), Some(Rect { x: 0, y: 15, w: 5, h: 5 }));
        assert_eq!(dilate(&one, 0), one);
    }

    /// sd-cli must redraw everything the blend takes any of.
    #[test]
    fn the_model_redraws_past_the_whole_fade() {
        let mask = painted(128, 128, Rect { x: 50, y: 50, w: 20, h: 20 });
        for soft in [0, 4, 16, 32] {
            let alpha = feather(&mask, soft);
            let sd = diffusion_mask(&mask, soft);
            for (x, y, a) in alpha.enumerate_pixels() {
                if a.0[0] > 0 {
                    assert_eq!(sd.get_pixel(x, y).0[0], 255, "soft {soft}: ({x},{y})");
                }
            }
        }
    }

    /// Growing keeps the original exactly, where it belongs, and masks
    /// exactly the new strip — whose pixels continue the edge's colour.
    #[test]
    fn growing_keeps_the_original_and_masks_the_strip() {
        let base = RgbImage::from_fn(40, 30, |x, y| Rgb([x as u8 * 5, y as u8 * 7, 90]));
        for (side, size, at) in [
            (Side::Left, (56, 30), (16, 0)),
            (Side::Right, (56, 30), (0, 0)),
            (Side::Top, (40, 46), (0, 16)),
            (Side::Bottom, (40, 46), (0, 0)),
        ] {
            let (canvas, mask) = grow(&base, side, 16);
            assert_eq!(canvas.dimensions(), size, "{side:?}");
            assert_eq!(mask.dimensions(), size, "{side:?}");
            for (x, y, p) in base.enumerate_pixels() {
                assert_eq!(canvas.get_pixel(x + at.0, y + at.1), p, "{side:?} ({x},{y})");
                assert_eq!(mask.get_pixel(x + at.0, y + at.1).0[0], 0, "{side:?}");
            }
            let painted = mask.pixels().filter(|p| p.0[0] == 255).count() as u32;
            assert_eq!(painted, size.0 * size.1 - 40 * 30, "{side:?}: the strip and only it");
        }
        // Beside the seam the strip carries the edge's colour, blurred but close.
        let flat = RgbImage::from_pixel(40, 30, Rgb([200, 100, 50]));
        let (canvas, _) = grow(&flat, Side::Right, 32);
        assert_eq!(canvas.get_pixel(41, 15), &Rgb([200, 100, 50]));
        assert_eq!(canvas.get_pixel(71, 15), &Rgb([200, 100, 50]));
        // Nothing to grow is no change.
        let (same, none) = grow(&base, Side::Top, 0);
        assert_eq!(same, base);
        assert_eq!(mask_bbox(&none), None);
    }

    /// An extend's crop holds the whole strip and at least as much real
    /// picture beside it — squarish at the model's size, never past the canvas.
    #[test]
    fn an_extend_crop_is_mostly_real_picture() {
        // 1024² grown 384 px right: strip plus 640 px of picture, square.
        let r = extend_crop((1408, 1024), Side::Right, 384, 1024);
        assert_eq!(r, Rect { x: 1408 - 1024, y: 0, w: 1024, h: 1024 });
        // Grown up: the strip is the top rows.
        let t = extend_crop((1024, 1280), Side::Top, 256, 1024);
        assert_eq!(t, Rect { x: 0, y: 0, w: 1024, h: 1024 });
        // A wide strip still gets as much picture as strip.
        let wide = extend_crop((1536, 1024), Side::Left, 512, 1024);
        assert_eq!(wide, Rect { x: 0, y: 0, w: 1024, h: 1024 });
        // A narrow original gives what it has.
        let thin = extend_crop((300, 1024), Side::Right, 256, 1024);
        assert_eq!(thin, Rect { x: 0, y: 0, w: 300, h: 1024 });
        for (canvas, side, px) in [((1408, 1024), Side::Right, 384), ((1024, 1280), Side::Bottom, 256)] {
            let c = extend_crop(canvas, side, px, 1024);
            assert!(c.x + c.w <= canvas.0 && c.y + c.h <= canvas.1);
        }
    }

    #[test]
    fn a_patch_lands_at_its_offset() {
        let mut base = RgbImage::new(32, 32);
        let patch = RgbImage::from_pixel(4, 4, Rgb([9, 9, 9]));
        let alpha = GrayImage::from_pixel(4, 4, Luma([255]));
        composite(&mut base, &patch, &alpha, (10, 20));
        assert_eq!(base.get_pixel(10, 20), &Rgb([9, 9, 9]));
        assert_eq!(base.get_pixel(13, 23), &Rgb([9, 9, 9]));
        assert_eq!(base.get_pixel(14, 23), &Rgb([0, 0, 0]));
    }
}
