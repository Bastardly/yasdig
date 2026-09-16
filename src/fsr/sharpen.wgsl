// FSR post-process, stage 2 of 2: AMD FidelityFX RCAS (Robust Contrast Adaptive
// Sharpening). Port of FsrRcasF from ffx_fsr1.h.
//
// This is the sharpener FSR pairs with EASU, and it differs from plain CAS in
// the two ways that matter after an upscale:
//   * a 5-tap cross rather than a 3x3 box, so it sharpens without pulling in
//     the diagonals EASU has already reconstructed;
//   * a limiter derived from the ring's min/max, so the lobe can never drive a
//     channel past its local neighbourhood — ringing and clipping are excluded
//     by construction rather than by backing off globally.
//
// One deviation from the reference: the cross is sampled at a distance that
// grows with the image (`radius`, set by `rcas_radius` in mod.rs) instead of
// always being the adjacent pixel. RCAS works on a fixed 3x3 footprint, so its
// effect is tied to the pixel grid rather than to the picture — at 3072 the same
// filter is working on a band three times finer, relative to the subject, than
// it was at 1024, which is why sharpening a hi-res result felt like it did
// almost nothing. Dilating the cross puts the band back where the eye expects
// it. It is also the more correct band to sharpen after an EASU upscale, where
// genuine detail lives at the pre-upscale pixel scale and what sits between
// those pixels is interpolation.
//
// This is the only lever there is, because the lobe itself is already maxed:
// the knob at 1.0 IS the reference filter at full strength (FSR parameterizes
// sharpness as `exp2(-stops)`, and 1.0 is 0 stops). Scaling the lobe past the
// limiter doesn't sharpen harder, it inverts the filter — the denominator
// `4*lobe + 1` crosses zero at lobe = -0.25, which is exactly what LIMIT below
// is holding it off. More lobe than the limiter allows is overshoot, i.e. the
// ringing this filter exists to rule out.
//
// RCAS's noise-detection term (FSR_RCAS_DENOISE) is deliberately NOT applied.
// It is opt-in in the reference, and for good reason here: it scales the lobe by
// a factor that is smallest exactly where the Laplacian is large relative to the
// local range — which on a diffusion render is fine detail, not noise. Enabling
// it held the 0..1 knob to ~22/255 of range on mid-tone detail and ~4/255 on
// fine texture, i.e. invisible. Don't put it back without measuring.

struct Params {
    size: vec2<u32>,
    sharpness: f32,
    // Tap distance for the cross, in pixels. 1 is the reference filter.
    radius: i32,
};

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var dst: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(2) var<uniform> p: Params;

// Past this the filter would invert rather than sharpen.
const LIMIT: f32 = 0.25 - (1.0 / 16.0);

fn ld(x: i32, y: i32) -> vec3<f32> {
    let cx = clamp(x, 0, i32(p.size.x) - 1);
    let cy = clamp(y, 0, i32(p.size.y) - 1);
    return textureLoad(src, vec2<i32>(cx, cy), 0).rgb;
}

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= p.size.x || gid.y >= p.size.y) {
        return;
    }
    let x = i32(gid.x);
    let y = i32(gid.y);

    //    b
    //  d e f      ... with d/f/b/h at `r` pixels out, not necessarily 1.
    //    h
    let r = max(p.radius, 1);
    let b = ld(x, y - r);
    let d = ld(x - r, y);
    let e = ld(x, y);
    let f = ld(x + r, y);
    let h = ld(x, y + r);

    // Per-channel limiter: the strongest lobe that still keeps the result
    // inside the ring's min/max. Both terms are <= 0, as is the lobe. Dilating
    // the cross doesn't weaken this — the guarantee is "stay inside the ring",
    // and the ring is simply measured further out.
    let mn4 = min(min(b, d), min(f, h));
    let mx4 = max(max(b, d), max(f, h));
    let hit_min = mn4 / max(4.0 * mx4, vec3<f32>(1e-5));
    let hit_max = (vec3<f32>(1.0) - mx4) / min(4.0 * mn4 - 4.0, vec3<f32>(-1e-5));
    let lobe_rgb = max(-hit_min, hit_max);
    var lobe = max(-LIMIT, min(max(max(lobe_rgb.r, lobe_rgb.g), lobe_rgb.b), 0.0));

    // Scale the limited lobe linearly by the knob. FSR itself parameterizes
    // sharpness in stops, but stops never reach zero, and 0.0 needs to mean off:
    // hires() hands this 0.0 for its pre-diffusion enlarge precisely so no
    // sharpening is baked into the diffusion. At 0.0 the filter below collapses
    // to e / 1.0 — an exact identity.
    lobe = lobe * clamp(p.sharpness, 0.0, 1.0);

    let outc = (b * lobe + d * lobe + f * lobe + h * lobe + e) / (4.0 * lobe + 1.0);

    textureStore(dst, vec2<i32>(x, y),
                 vec4<f32>(clamp(outc, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0));
}
