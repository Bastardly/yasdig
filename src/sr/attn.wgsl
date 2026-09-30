// Swin window attention, read and written in image order.
//
// The reference partitions the image into windows, rolls it for the shifted
// blocks, attends, then un-partitions and rolls back — four copies of the whole
// feature map per block. None of that is needed: every step before attention
// (LayerNorm, the qkv Linear) is per-pixel, so qkv can be computed in image
// order and this kernel works out which pixel each window slot *is*. A slot
// (window w, index t) sits at (sy, sx) in the shifted frame, and
// `torch.roll(x, -s)` puts original pixel ((sy + s) mod H, (sx + s) mod W)
// there. The output goes back to that same pixel, which is exactly what the
// reverse roll does.
//
// The shift mask is computed here too. `calculate_mask` labels the shifted
// frame with nine regions — each axis split at H−ws and H−s — and adds −100
// between tokens of different regions, so that pixels wrapped round from the
// far edge don't attend to their new neighbours.
//
// One workgroup per (window, head, 64 queries); keys stream through workgroup
// memory 32 at a time with an online softmax, so a 16×16 window (256 keys)
// needs no more memory than an 8×8 one. head_dim ≤ 64.

struct P { u: array<vec4<u32>, 4> }

@group(0) @binding(0) var<storage, read> a: array<f32>;   // qkv [H·W, 3C], (3, heads, hd) per row
@group(0) @binding(1) var<storage, read> b: array<f32>;   // relative bias [heads][Nw][Nw]
@group(0) @binding(2) var<storage, read> c: array<f32>;   // unused
@group(0) @binding(3) var<storage, read> d: array<f32>;   // unused
@group(0) @binding(4) var<storage, read_write> o: array<f32>; // [H·W, C]
@group(0) @binding(5) var<uniform> p: P;

fn pu(i: u32) -> u32 { return p.u[i / 4u][i % 4u]; }
fn pf(i: u32) -> f32 { return bitcast<f32>(pu(i)); }

// Params: 0 H, 1 W, 2 C, 3 heads, 4 hd, 5 ws, 6 shift, 7 scale.

const KC: u32 = 32u;
var<workgroup> Ks: array<f32, 2048>; // [KC][hd ≤ 64]
var<workgroup> Vs: array<f32, 2048>;

fn region(v: u32, size: u32, ws: u32, s: u32) -> u32 {
    if (v < size - ws) { return 0u; }
    if (v < size - s) { return 1u; }
    return 2u;
}

// The shifted-frame coordinate of slot t in window `win`.
fn slot(win: u32, t: u32) -> vec2<u32> {
    let W = pu(1u);
    let ws = pu(5u);
    let nwx = W / ws;
    return vec2<u32>((win % nwx) * ws + t % ws, (win / nwx) * ws + t / ws);
}

fn pixel(sxy: vec2<u32>) -> u32 {
    let H = pu(0u);
    let W = pu(1u);
    let s = pu(6u);
    return ((sxy.y + s) % H) * W + (sxy.x + s) % W;
}

@compute @workgroup_size(64)
fn main(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_id) lid: vec3<u32>) {
    let H = pu(0u);
    let W = pu(1u);
    let C = pu(2u);
    let hd = pu(4u);
    let ws = pu(5u);
    let s = pu(6u);
    let scale = pf(7u);
    let nw = ws * ws;
    let win = wg.x;
    let h = wg.y;
    let qi = wg.z * 64u + lid.x;
    let valid = qi < nw;
    let stride = 3u * C;

    var q: array<f32, 64>;
    var acc: array<f32, 64>;
    var qreg = vec2<u32>(0u, 0u);
    var qpix = 0u;
    if (valid) {
        let sq = slot(win, qi);
        qpix = pixel(sq);
        qreg = vec2<u32>(region(sq.x, W, ws, s), region(sq.y, H, ws, s));
        for (var e = 0u; e < hd; e++) {
            q[e] = a[qpix * stride + h * hd + e] * scale;
            acc[e] = 0.0;
        }
    }
    var mx = -1e30;
    var l = 0.0;

    for (var k0 = 0u; k0 < nw; k0 += KC) {
        for (var e = lid.x; e < KC * hd; e += 64u) {
            let kk = e / hd;
            let dd = e % hd;
            let t = k0 + kk;
            if (t < nw) {
                let pk = pixel(slot(win, t));
                Ks[e] = a[pk * stride + C + h * hd + dd];
                Vs[e] = a[pk * stride + 2u * C + h * hd + dd];
            }
        }
        workgroupBarrier();
        if (valid) {
            let n = min(KC, nw - k0);
            var sc: array<f32, 32>;
            var bm = -1e30;
            for (var kk = 0u; kk < n; kk++) {
                var sd = 0.0;
                for (var e = 0u; e < hd; e++) { sd += q[e] * Ks[kk * hd + e]; }
                sd += b[(h * nw + qi) * nw + k0 + kk];
                if (s > 0u) {
                    let sk = slot(win, k0 + kk);
                    let kr = vec2<u32>(region(sk.x, W, ws, s), region(sk.y, H, ws, s));
                    if (kr.x != qreg.x || kr.y != qreg.y) { sd -= 100.0; }
                }
                sc[kk] = sd;
                bm = max(bm, sd);
            }
            let nm = max(mx, bm);
            let corr = exp(mx - nm);
            l *= corr;
            for (var e = 0u; e < hd; e++) { acc[e] *= corr; }
            for (var kk = 0u; kk < n; kk++) {
                let pw = exp(sc[kk] - nm);
                l += pw;
                for (var e = 0u; e < hd; e++) { acc[e] += pw * Vs[kk * hd + e]; }
            }
            mx = nm;
        }
        workgroupBarrier();
    }

    if (valid) {
        for (var e = 0u; e < hd; e++) {
            o[qpix * C + h * hd + e] = acc[e] / l;
        }
    }
}
