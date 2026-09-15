// FSR post-process, stage 1 of 2: spatial upscale.
//
// This is the EASU slot. It currently implements a high-quality Lanczos-2
// resample (edge-preserving, no ringing halos at a=2) as the approved interim
// while the full FSR1 EASU directional kernel is ported. The host logs
// "upscale=lanczos2" so it's obvious which path is live. RCAS/CAS sharpening in
// stage 2 is independent and unchanged by this choice.

struct Params {
    in_size: vec2<u32>,
    out_size: vec2<u32>,
};

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var dst: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(2) var<uniform> p: Params;

const PI: f32 = 3.14159265358979;

fn sinc(x: f32) -> f32 {
    if (abs(x) < 1e-6) {
        return 1.0;
    }
    let pix = PI * x;
    return sin(pix) / pix;
}

fn lanczos(x: f32) -> f32 {
    let a = 2.0;
    if (abs(x) >= a) {
        return 0.0;
    }
    return sinc(x) * sinc(x / a);
}

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= p.out_size.x || gid.y >= p.out_size.y) {
        return;
    }
    let in_w = f32(p.in_size.x);
    let in_h = f32(p.in_size.y);
    let out_w = f32(p.out_size.x);
    let out_h = f32(p.out_size.y);

    // Map output pixel center back into source space.
    let sx = (f32(gid.x) + 0.5) * in_w / out_w - 0.5;
    let sy = (f32(gid.y) + 0.5) * in_h / out_h - 0.5;
    let bx = i32(floor(sx));
    let by = i32(floor(sy));

    var acc = vec3<f32>(0.0);
    var wsum = 0.0;
    for (var j: i32 = -1; j <= 2; j = j + 1) {
        let yi = by + j;
        let wy = lanczos(sy - f32(yi));
        let cy = clamp(yi, 0, i32(p.in_size.y) - 1);
        for (var i: i32 = -1; i <= 2; i = i + 1) {
            let xi = bx + i;
            let wx = lanczos(sx - f32(xi));
            let cx = clamp(xi, 0, i32(p.in_size.x) - 1);
            let w = wx * wy;
            let col = textureLoad(src, vec2<i32>(cx, cy), 0).rgb;
            acc = acc + col * w;
            wsum = wsum + w;
        }
    }

    var outc = acc / max(wsum, 1e-5);
    outc = clamp(outc, vec3<f32>(0.0), vec3<f32>(1.0));
    textureStore(dst, vec2<i32>(i32(gid.x), i32(gid.y)), vec4<f32>(outc, 1.0));
}
