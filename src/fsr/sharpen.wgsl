// FSR post-process, stage 2 of 2: AMD FidelityFX CAS (Contrast Adaptive
// Sharpening). Faithful port of the non-scaling CAS kernel: a 3x3 neighborhood,
// soft min/max with the cross weighted fully and the diagonals folded in, an
// amplitude term that backs off near signal limits (so it sharpens detail
// without clipping), and the standard filter weight peak in [-1/8 .. -1/5].

struct Params {
    size: vec2<u32>,
    sharpness: f32,
    _pad: f32,
};

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var dst: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(2) var<uniform> p: Params;

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

    //  a b c
    //  d e f
    //  g h i
    let a = ld(x - 1, y - 1);
    let b = ld(x, y - 1);
    let c = ld(x + 1, y - 1);
    let d = ld(x - 1, y);
    let e = ld(x, y);
    let f = ld(x + 1, y);
    let g = ld(x - 1, y + 1);
    let h = ld(x, y + 1);
    let ii = ld(x + 1, y + 1);

    // Soft min/max: cross ring first, then fold in the diagonal ring (each of
    // mn/mx ends up as the sum of two rings, so the range is [0, 2]).
    var mn = min(min(min(d, e), min(f, b)), h);
    let mn2 = min(min(min(a, c), min(g, ii)), mn);
    mn = mn + mn2;
    var mx = max(max(max(d, e), max(f, b)), h);
    let mx2 = max(max(max(a, c), max(g, ii)), mx);
    mx = mx + mx2;

    // Amplitude: how much headroom there is before hitting the local limits.
    let rcp_mx = vec3<f32>(1.0) / max(mx, vec3<f32>(1e-5));
    var amp = clamp(min(mn, vec3<f32>(2.0) - mx) * rcp_mx, vec3<f32>(0.0), vec3<f32>(1.0));
    amp = sqrt(amp);

    // Filter weight. sharpness 0 -> gentle (-1/8), 1 -> strong (-1/5).
    let peak = -1.0 / mix(8.0, 5.0, clamp(p.sharpness, 0.0, 1.0));
    let w = amp * peak;
    let rcp_w = vec3<f32>(1.0) / (vec3<f32>(1.0) + 4.0 * w);
    var outc = ((b + d + f + h) * w + e) * rcp_w;
    outc = clamp(outc, vec3<f32>(0.0), vec3<f32>(1.0));

    textureStore(dst, vec2<i32>(x, y), vec4<f32>(outc, 1.0));
}
