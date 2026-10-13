// Colour grading: nine per-pixel knobs in one pass.
//
// Everything here reads exactly one texel and writes exactly one texel, which is
// the whole reason there is one shader rather than nine. The two filters either
// side of it (clarity, RCAS) are *spatial* — their kernels are sized from the
// image — and this one has no kernel at all, so its cost is one dispatch
// whatever any of the knobs are set to: one full-resolution pass of two
// transfer-function evaluations, two 3x3s and three cube roots per pixel. Too
// cheap to measure on a 7900 XTX — a 3072² publish is 56 ms best-of-5 with the
// grade and 56 ms without, the PNG round trip being most of it.
//
// Exact identity when all nine are 0 — but `Fsr::process` skips the pass
// entirely at neutral rather than relying on that, because the round trip below
// is not byte-exact: sRGB -> linear -> OKLab -> linear -> sRGB loses a
// least-significant bit here and there, and `fsr-selftest` asserts byte
// equality.
//
// ## Why OKLab
//
// Because in it all nine knobs are one named operation on three numbers:
// lightness and the three tone knobs move L, saturation scales the chroma pair,
// hue rotates it, and the two white-balance knobs are *exactly* its two chroma
// axes — b is blue-yellow (temperature), a is green-magenta (tint). Doing the
// same work in RGB means a different hack per knob and a hue shift in every one
// of them. The matrices and the cube roots are Björn Ottosson's
// (https://bottosson.github.io/posts/oklab/), unmodified.
//
// ## What temperature and tint are, and are not
//
// They are shifts along those two chroma axes, scaled by L so that black stays
// black (a white balance moves what is lit, not what isn't). They are *not* a
// von Kries adaptation: the physical version maps a colour temperature through
// the Planckian locus to xy and builds a 3x3 per frame, which is a second colour
// space in the same pass for a knob nearly everyone drags until it looks right.
// If the response ever reads wrong at the ends — warm going orange rather than
// yellow is the symptom — that is the upgrade, and it belongs on the CPU side:
// build the matrix in `GradeParams` and hand it down, don't grow this shader.
//
// ## Order
//
// White balance, then tone, then colour — a photo editor's order, and it is
// load-bearing for two of them: contrast pivots about mid-grey, so it has to see
// the tone the white balance produced, and saturation scales chroma, so running
// it before a temperature shift would scale a shift that hasn't happened yet.

@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var dst_tex: texture_storage_2d<rgba8unorm, write>;

struct Params {
    size: vec2<u32>,
    lightness: f32,
    contrast: f32,
    highlights: f32,
    shadows: f32,
    saturation: f32,
    temperature: f32,
    tint: f32,
    // Precomputed on the CPU: a hue rotation is one 2x2 matrix, and there is no
    // reason to pay for a sin and a cos per pixel to rebuild it 9 million times.
    hue_sin: f32,
    hue_cos: f32,
    vibrance: f32,
}

@group(0) @binding(2) var<uniform> params: Params;

/// The OKLab L of mid-grey, which is what contrast pivots about: sRGB 0.5
/// decodes to 0.2140 linear, and a neutral has l = m = s = Y, so
/// L = cbrt(0.2140) = 0.5981 (the L row of the matrix sums to 1).
const MID_L: f32 = 0.5981;

/// How far the two white-balance knobs push their chroma axis at full travel.
/// OKLab's a and b run to about ±0.3 on a saturated colour, so 0.06 is a shift
/// you can see on a neutral without it becoming a colour of its own.
const WB_RANGE: f32 = 0.06;

/// The chroma at which vibrance has stopped acting. OKLab chroma on a fully
/// saturated sRGB primary is about 0.26-0.32 and on skin about 0.05-0.10, so
/// 0.2 leaves the vivid colours alone while a muted background still gets the
/// most of it. Skin sits in the lower half, so it moves, but less than grey-blue
/// sky does — that is the difference from Saturation, which moves all of it.
const VIBRANCE_KNEE: f32 = 0.2;

/// How far highlights/shadows move their end of the range at full travel, in L.
/// Less than lightness gets, deliberately: the point of these two is that they
/// leave the other end alone.
const TONE_RANGE: f32 = 0.25;

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let v = clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
    let lo = v * 12.92;
    let hi = 1.055 * pow(v, vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(hi, lo, v <= vec3<f32>(0.0031308));
}

fn linear_to_oklab(c: vec3<f32>) -> vec3<f32> {
    let l = 0.4122214708 * c.r + 0.5363325363 * c.g + 0.0514459929 * c.b;
    let m = 0.2119034982 * c.r + 0.6806995451 * c.g + 0.1073969566 * c.b;
    let s = 0.0883024619 * c.r + 0.2817188376 * c.g + 0.6299787005 * c.b;
    // Non-negative by construction — every coefficient is positive and the input
    // is clamped — so a plain cube root suffices, no sign dance.
    let l_ = pow(max(l, 0.0), 1.0 / 3.0);
    let m_ = pow(max(m, 0.0), 1.0 / 3.0);
    let s_ = pow(max(s, 0.0), 1.0 / 3.0);
    return vec3<f32>(
        0.2104542553 * l_ + 0.7936177850 * m_ - 0.0040720468 * s_,
        1.9779984951 * l_ - 2.4285922050 * m_ + 0.4505937099 * s_,
        0.0259040371 * l_ + 0.7827717662 * m_ - 0.8086757660 * s_,
    );
}

fn oklab_to_linear(lab: vec3<f32>) -> vec3<f32> {
    let l_ = lab.x + 0.3963377774 * lab.y + 0.2158037573 * lab.z;
    let m_ = lab.x - 0.1055613458 * lab.y - 0.0638541728 * lab.z;
    let s_ = lab.x - 0.0894841775 * lab.y - 1.2914855480 * lab.z;
    let l = l_ * l_ * l_;
    let m = m_ * m_ * m_;
    let s = s_ * s_ * s_;
    return vec3<f32>(
        4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
        -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
        -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s,
    );
}

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.size.x || gid.y >= params.size.y) {
        return;
    }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    let src = textureLoad(src_tex, pos, 0);

    var lab = linear_to_oklab(srgb_to_linear(src.rgb));

    // --- White balance. Scaled by L: a light source tints what it lights, and
    // lifting black to a colour is the thing that reads as a broken filter.
    lab.z = lab.z + params.temperature * WB_RANGE * lab.x;
    lab.y = lab.y + params.tint * WB_RANGE * lab.x;

    // --- Tone, all on L.
    // Multiplicative, so +1 and -1 are each other's inverse rather than one
    // clipping and the other doing nothing. exp2(±0.5) is about ±1.5 stops of
    // linear light, L being roughly a cube root of luminance.
    lab.x = lab.x * exp2(params.lightness * 0.5);
    // Slope about mid-grey: exp2(1) doubles the contrast, exp2(-1) halves it.
    lab.x = MID_L + (lab.x - MID_L) * exp2(params.contrast);
    // The two ends, each with a smooth mask so neither touches the other half.
    // Negative highlights is the useful direction: it recovers a blown sky.
    lab.x = lab.x + params.highlights * TONE_RANGE * smoothstep(0.5, 1.0, lab.x);
    lab.x = lab.x + params.shadows * TONE_RANGE * (1.0 - smoothstep(0.0, 0.5, lab.x));
    lab.x = clamp(lab.x, 0.0, 1.0);

    // --- Colour. Vibrance first: a chroma scale that fades out as chroma
    // rises, so what is already vivid stays put and clipping doesn't arrive
    // first on the colours that were fine. Negative mutes the dull ones first.
    // Then saturation, a plain chroma scale, so -1 lands exactly on a = b = 0,
    // which is exactly greyscale rather than nearly.
    let muted = 1.0 - smoothstep(0.0, VIBRANCE_KNEE, length(lab.yz));
    let chroma = lab.yz * (1.0 + params.vibrance * muted) * (1.0 + params.saturation);
    lab.y = chroma.x * params.hue_cos - chroma.y * params.hue_sin;
    lab.z = chroma.x * params.hue_sin + chroma.y * params.hue_cos;

    let rgb = linear_to_srgb(oklab_to_linear(lab));
    textureStore(dst_tex, pos, vec4<f32>(rgb, src.a));
}
