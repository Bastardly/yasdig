// Local contrast ("clarity"): a band-pass boost on luma, weighted toward the
// midtones. Not part of FSR — this sits between EASU and RCAS. The knob runs
// -1..1: positive adds the band (depth), negative subtracts it (softening).
//
// The point is the *band* it works in. A diffusion img2img pass enters the noise
// schedule at a sigma set by its denoise strength, and sigma is effectively a
// spatial-frequency dial: high sigma rebuilds coarse structure (tone, contrast,
// depth), low sigma rebuilds fine texture. A low-strength refine therefore only
// ever touches the detail band, which is why it adds texture but can't add
// depth — and why reaching for depth by raising strength instead re-composes the
// image, badly, at resolutions past what the model was trained on.
//
// So depth doesn't come from the model here. Two things this is NOT:
//
//   * not a sharpener. A plain unsharp mask (`src - blur`) is a high-pass: it
//     lifts everything above its cutoff, the fine grain most of all, which is
//     the opposite of what's wanted — that grain is exactly what a diffusion
//     pass leaves behind at high resolution. So the boosted term is a
//     difference of two Gaussians, narrow minus wide, which passes the middle
//     and rejects both ends.
//   * not a full-resolution filter. The wide lobe has to be ~4% of the image to
//     reach subject-scale structure — 120px at 3072 — and a separable blur that
//     wide costs hundreds of taps per pixel. Both lobes are computed on a 1/8
//     luma plane instead and bilinearly resampled; at these radii the plane is
//     smooth long before it is sampled, so nothing is lost.
//
// Four passes: downsample, blur_h, blur_v (both at 1/8), then combine at full
// resolution. The two blurs are 1/64 the work of a full-res pass, so the whole
// thing costs about two reads of the image.

struct Params {
    size: vec2<u32>,
    low_size: vec2<u32>,
    // The -1..1 knob. 0.0 makes this an exact identity — see `combine`.
    // Negative subtracts the band instead of adding it, which softens the
    // mid-scale structure the way a photo editor's negative Clarity does.
    amount: f32,
    // Gaussian sigmas, in *low-res* pixels. Wide sets where depth starts, narrow
    // sets where grain is excluded.
    sigma_wide: f32,
    sigma_narrow: f32,
    _pad: f32,
};

// Passes 1-3 share a layout and use 0, 1, 2; pass 4 uses 0, 2, 3, 4. Binding 0
// is just "the input texture" — the full-resolution image for `downsample` and
// `combine`, the 1/8 plane for the two blurs.
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var plane_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(2) var<uniform> p: Params;
@group(0) @binding(3) var plane_in: texture_2d<f32>;
@group(0) @binding(4) var final_out: texture_storage_2d<rgba8unorm, write>;

// Must match DOWNSCALE in mod.rs.
const DOWN: i32 = 8;
// Bounds the dynamic loops. 3 sigma at the caller's low-res sigma cap.
const MAX_RADIUS: i32 = 48;

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

fn radius(sigma: f32) -> i32 {
    return clamp(i32(ceil(3.0 * sigma)), 1, MAX_RADIUS);
}

fn gauss(i: i32, sigma: f32) -> f32 {
    let x = f32(i);
    return exp(-(x * x) / (2.0 * sigma * sigma));
}

fn low_xy(x: i32, y: i32) -> vec2<i32> {
    return vec2<i32>(
        clamp(x, 0, i32(p.low_size.x) - 1),
        clamp(y, 0, i32(p.low_size.y) - 1),
    );
}

// Pass 1: 8x8 box-average of luma into the 1/8 plane. The box is itself a mild
// low-pass (~2.3px at full scale), which is why the narrow lobe below can be
// small without aliasing.
@compute @workgroup_size(16, 16, 1)
fn downsample(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= p.low_size.x || gid.y >= p.low_size.y) {
        return;
    }
    let ox = i32(gid.x) * DOWN;
    let oy = i32(gid.y) * DOWN;
    let w = i32(p.size.x) - 1;
    let h = i32(p.size.y) - 1;

    var acc = 0.0;
    for (var dy = 0; dy < DOWN; dy++) {
        for (var dx = 0; dx < DOWN; dx++) {
            let xy = vec2<i32>(clamp(ox + dx, 0, w), clamp(oy + dy, 0, h));
            acc += luma(textureLoad(src, xy, 0).rgb);
        }
    }

    let mean = acc / f32(DOWN * DOWN);
    textureStore(plane_out, vec2<i32>(i32(gid.x), i32(gid.y)),
                 vec4<f32>(mean, mean, 0.0, 1.0));
}

// Passes 2 and 3: separable blur of both lobes at once, narrow in .r and wide in
// .g. Pass 2 reads the downsampled plane (both channels hold the same value, so
// each lobe starts from the same data); pass 3 reads pass 2's output.
fn blur(x: i32, y: i32, horizontal: bool) -> vec2<f32> {
    let rn = radius(p.sigma_narrow);
    let rw = radius(p.sigma_wide);
    let r = max(rn, rw);

    var acc = vec2<f32>(0.0);
    var wacc = vec2<f32>(0.0);
    for (var i = -r; i <= r; i++) {
        var xy: vec2<i32>;
        if (horizontal) {
            xy = low_xy(x + i, y);
        } else {
            xy = low_xy(x, y + i);
        }
        let s = textureLoad(src, xy, 0).rg;
        let w = vec2<f32>(
            select(0.0, gauss(i, p.sigma_narrow), abs(i) <= rn),
            select(0.0, gauss(i, p.sigma_wide), abs(i) <= rw),
        );
        acc += w * s;
        wacc += w;
    }
    return acc / wacc;
}

@compute @workgroup_size(16, 16, 1)
fn blur_h(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= p.low_size.x || gid.y >= p.low_size.y) {
        return;
    }
    let v = blur(i32(gid.x), i32(gid.y), true);
    textureStore(plane_out, vec2<i32>(i32(gid.x), i32(gid.y)),
                 vec4<f32>(v.x, v.y, 0.0, 1.0));
}

@compute @workgroup_size(16, 16, 1)
fn blur_v(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= p.low_size.x || gid.y >= p.low_size.y) {
        return;
    }
    let v = blur(i32(gid.x), i32(gid.y), false);
    textureStore(plane_out, vec2<i32>(i32(gid.x), i32(gid.y)),
                 vec4<f32>(v.x, v.y, 0.0, 1.0));
}

// Bilinear fetch of the 1/8 plane at a full-resolution pixel centre.
fn sample_plane(x: i32, y: i32) -> vec2<f32> {
    let fx = (f32(x) + 0.5) / f32(DOWN) - 0.5;
    let fy = (f32(y) + 0.5) / f32(DOWN) - 0.5;
    let x0 = i32(floor(fx));
    let y0 = i32(floor(fy));
    let tx = fx - floor(fx);
    let ty = fy - floor(fy);

    let a = textureLoad(plane_in, low_xy(x0, y0), 0).rg;
    let b = textureLoad(plane_in, low_xy(x0 + 1, y0), 0).rg;
    let c = textureLoad(plane_in, low_xy(x0, y0 + 1), 0).rg;
    let d = textureLoad(plane_in, low_xy(x0 + 1, y0 + 1), 0).rg;
    return mix(mix(a, b, tx), mix(c, d, tx), ty);
}

// Pass 4: apply the band to the full-resolution image.
@compute @workgroup_size(16, 16, 1)
fn combine(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= p.size.x || gid.y >= p.size.y) {
        return;
    }
    let x = i32(gid.x);
    let y = i32(gid.y);

    let lobes = sample_plane(x, y);
    // Narrow minus wide: the structure between the two cutoffs, with the fine
    // grain the narrow lobe already removed and the overall tone the wide lobe
    // still carries both excluded.
    let band = lobes.x - lobes.y;

    let c = textureLoad(src, vec2<i32>(x, y), 0).rgb;
    let l = luma(c);

    // Map the -1..1 knob to band gain. At 0.0 the gain is exactly 0 and the
    // store below writes `c` unchanged, so the pass is a true no-op rather than
    // a quiet tax on every image. Negative gain subtracts the band: the mid
    // frequencies flatten and the image reads softer and dreamier, which is what
    // negative Clarity does in a photo editor — useful on skin, and on a render
    // whose depth came out overcooked.
    //
    // The multiplier was 3.0 and is 2.0 because the top of the range was harsh
    // rather than merely strong: past roughly 2× on the depth band the boost
    // stops reading as depth and starts reading as a halo. Every value on the
    // slider is now two-thirds of what the same number used to do, so a setting
    // carried over from before wants multiplying by ~1.5.
    let gain = clamp(p.amount, -1.0, 1.0) * 2.0;

    // Full effect through the midtones, nothing at either extreme. Pushing depth
    // then can't crush shadows, blow highlights, or clip — the places an unsharp
    // mask normally shows its seams.
    let t = abs(2.0 * l - 1.0);
    let protect = 1.0 - t * t;

    // The delta goes on all three channels equally, so hue is untouched.
    let outc = clamp(c + band * gain * protect, vec3<f32>(0.0), vec3<f32>(1.0));
    textureStore(final_out, vec2<i32>(x, y), vec4<f32>(outc, 1.0));
}
