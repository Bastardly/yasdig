// FSR post-process, stage 1 of 2: AMD FidelityFX Super Resolution 1.0 EASU
// (Edge-Adaptive Spatial Upsampling). Port of FsrEasuF from ffx_fsr1.h.
//
// Per output pixel it fits a local edge direction and "edge-ness" from luma
// gradients over a 12-tap neighbourhood, then applies a Lanczos-2-shaped kernel
// stretched ALONG that edge and squashed ACROSS it. That anisotropy is the
// whole point: a fixed isotropic kernel (lanczos, bicubic) filters across an
// edge exactly as it does along one, which is what softens an upscale.
//
// Two deliberate departures from the reference, neither affecting output:
//   * textureLoad with clamped coords instead of the gather4 fetches — the
//     reference packs 4 taps per fetch, which WGSL has no portable equivalent
//     for, and we are not cycle-bound here.
//   * exact 1/x and inverseSqrt in place of the APrxLoRcpF1 / APrxLoRsqF1
//     approximations, which exist to save ALU on console GPUs.

struct Params {
    in_size: vec2<u32>,
    out_size: vec2<u32>,
};

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var dst: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(2) var<uniform> p: Params;

fn ld(x: i32, y: i32) -> vec3<f32> {
    let cx = clamp(x, 0, i32(p.in_size.x) - 1);
    let cy = clamp(y, 0, i32(p.in_size.y) - 1);
    return textureLoad(src, vec2<i32>(cx, cy), 0).rgb;
}

// FSR's cheap luma proxy (luma times 2). Kept exactly as the reference has it,
// since the direction/length constants below are tuned against this scale.
fn luma(c: vec3<f32>) -> f32 {
    return c.b * 0.5 + (c.r * 0.5 + c.g);
}

struct DirLen {
    dir: vec2<f32>,
    len: f32,
};

// Direction and length for one bilinear quadrant, over the cross
//      a
//    b c d
//      e
// Direction is the '+' difference across c. Length turns a gradient reversal
// into 0 smoothly, so the kernel only stretches where there is a real edge.
fn easu_set(w: f32, la: f32, lb: f32, lc: f32, ld_: f32, le: f32) -> DirLen {
    let dirx = ld_ - lb;
    let lenx = clamp(abs(dirx) / max(max(abs(ld_ - lc), abs(lc - lb)), 1e-6), 0.0, 1.0);

    let diry = le - la;
    let leny = clamp(abs(diry) / max(max(abs(le - lc), abs(lc - la)), 1e-6), 0.0, 1.0);

    var o: DirLen;
    o.dir = vec2<f32>(dirx, diry) * w;
    o.len = (lenx * lenx + leny * leny) * w;
    return o;
}

struct Tap {
    c: vec3<f32>,
    w: f32,
};

// One tap of the anisotropic Lanczos-2 approximation.
fn easu_tap(off: vec2<f32>, dir: vec2<f32>, len2: vec2<f32>,
            lob: f32, clp: f32, c: vec3<f32>) -> Tap {
    // Rotate the tap offset into the edge's frame, then scale each axis.
    var v = vec2<f32>(off.x * dir.x + off.y * dir.y,
                      off.x * -dir.y + off.y * dir.x);
    v = v * len2;
    let d2 = min(v.x * v.x + v.y * v.y, clp);

    // Lanczos-2 with no sin(), rcp() or sqrt(): a 'base' lobe times a window,
    // both quadratics in d2.
    //  (25/16 * (2/5 * x^2 - 1)^2 - (25/16 - 1)) * (lob * x^2 - 1)^2
    //  |_______________________________________|   |_________________|
    //                   base                             window
    var wb = 2.0 / 5.0 * d2 - 1.0;
    var wa = lob * d2 - 1.0;
    wb = wb * wb;
    wa = wa * wa;
    wb = 25.0 / 16.0 * wb - (25.0 / 16.0 - 1.0);

    var o: Tap;
    o.w = wb * wa;
    o.c = c * o.w;
    return o;
}

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= p.out_size.x || gid.y >= p.out_size.y) {
        return;
    }

    let scale = vec2<f32>(f32(p.in_size.x) / f32(p.out_size.x),
                          f32(p.in_size.y) / f32(p.out_size.y));
    // Map the output pixel centre back into source space, then split into the
    // integer cell of 'f' and the fraction within the f/g/j/k square.
    let pos = (vec2<f32>(f32(gid.x), f32(gid.y)) + vec2<f32>(0.5)) * scale - vec2<f32>(0.5);
    let fp = floor(pos);
    let pp = pos - fp;
    let bx = i32(fp.x);
    let by = i32(fp.y);

    // 12-tap neighbourhood:
    //      b c
    //    e f g h
    //    i j k l
    //      n o
    let cb = ld(bx,     by - 1);
    let cc = ld(bx + 1, by - 1);
    let ce = ld(bx - 1, by);
    let cf = ld(bx,     by);
    let cg = ld(bx + 1, by);
    let ch = ld(bx + 2, by);
    let ci = ld(bx - 1, by + 1);
    let cj = ld(bx,     by + 1);
    let ck = ld(bx + 1, by + 1);
    let cl = ld(bx + 2, by + 1);
    let cn = ld(bx,     by + 2);
    let co = ld(bx + 1, by + 2);

    let lb = luma(cb); let lc = luma(cc);
    let le = luma(ce); let lf = luma(cf); let lg = luma(cg); let lh = luma(ch);
    let li = luma(ci); let lj = luma(cj); let lk = luma(ck); let ll = luma(cl);
    let ln = luma(cn); let lo = luma(co);

    // Accumulate over the four bilinear quadrants around the sample point.
    let s = easu_set((1.0 - pp.x) * (1.0 - pp.y), lb, le, lf, lg, lj);
    let t = easu_set(       pp.x  * (1.0 - pp.y), lc, lf, lg, lh, lk);
    let u = easu_set((1.0 - pp.x) *        pp.y , lf, li, lj, lk, ln);
    let v = easu_set(       pp.x  *        pp.y , lg, lj, lk, ll, lo);
    var dir = s.dir + t.dir + u.dir + v.dir;
    let len = s.len + t.len + u.len + v.len;

    // Normalize the direction; below a threshold there is no edge to speak of,
    // so fall back to axis-aligned and let 'len' (also ~0) keep it isotropic.
    var dirr = dir.x * dir.x + dir.y * dir.y;
    if (dirr < 1.0 / 32768.0) {
        dir = vec2<f32>(1.0, 0.0);
    } else {
        dir = dir * inverseSqrt(dirr);
    }

    // Length from {0..2} to {0..1}, shaped by a square.
    var l = len * 0.5;
    l = l * l;

    // Stretch the kernel: 1.0 on the axes, sqrt(2) on the diagonal.
    let stretch = (dir.x * dir.x + dir.y * dir.y)
        / max(max(abs(dir.x), abs(dir.y)), 1e-6);
    // Anisotropic lengths after rotation: along the edge it widens to
    // 'stretch', across the edge it narrows to half. That squash across the
    // gradient is what keeps the edge from being averaged away.
    let len2 = vec2<f32>(1.0 + (stretch - 1.0) * l, 1.0 - 0.5 * l);
    // With more 'edge', the window widens from +/-sqrt(2) toward 2.
    let lob = 0.5 + ((1.0 / 4.0 - 0.04) - 0.5) * l;
    let clp = 1.0 / lob;

    var acc = vec3<f32>(0.0);
    var wacc = 0.0;
    var taps = array<Tap, 12>(
        easu_tap(vec2<f32>( 0.0, -1.0) - pp, dir, len2, lob, clp, cb),
        easu_tap(vec2<f32>( 1.0, -1.0) - pp, dir, len2, lob, clp, cc),
        easu_tap(vec2<f32>(-1.0,  0.0) - pp, dir, len2, lob, clp, ce),
        easu_tap(vec2<f32>( 0.0,  0.0) - pp, dir, len2, lob, clp, cf),
        easu_tap(vec2<f32>( 1.0,  0.0) - pp, dir, len2, lob, clp, cg),
        easu_tap(vec2<f32>( 2.0,  0.0) - pp, dir, len2, lob, clp, ch),
        easu_tap(vec2<f32>(-1.0,  1.0) - pp, dir, len2, lob, clp, ci),
        easu_tap(vec2<f32>( 0.0,  1.0) - pp, dir, len2, lob, clp, cj),
        easu_tap(vec2<f32>( 1.0,  1.0) - pp, dir, len2, lob, clp, ck),
        easu_tap(vec2<f32>( 2.0,  1.0) - pp, dir, len2, lob, clp, cl),
        easu_tap(vec2<f32>( 0.0,  2.0) - pp, dir, len2, lob, clp, cn),
        easu_tap(vec2<f32>( 1.0,  2.0) - pp, dir, len2, lob, clp, co),
    );
    for (var i = 0; i < 12; i = i + 1) {
        acc = acc + taps[i].c;
        wacc = wacc + taps[i].w;
    }

    // Normalize, then dering against the nearest 2x2: the kernel's negative
    // lobe can overshoot, and clamping to the local range turns that into
    // sharpness instead of a halo.
    let min4 = min(min(cf, cg), min(cj, ck));
    let max4 = max(max(cf, cg), max(cj, ck));
    let outc = min(max4, max(min4, acc / max(wacc, 1e-5)));

    textureStore(dst, vec2<i32>(i32(gid.x), i32(gid.y)),
                 vec4<f32>(clamp(outc, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0));
}
