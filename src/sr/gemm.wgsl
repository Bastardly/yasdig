// Tiled matrix multiply, and every convolution and linear layer built on it.
//
// Tensors are channels-last: an image is [H·W, C] row-major, so a pixel's
// channels are contiguous. That makes a Linear layer a plain [tokens, in] ×
// [out, in]ᵀ product with no reshaping at all, and a k×k convolution the same
// product over an *implicit* im2col: row m is pixel m, and column k is one
// (tap, input channel) pair gathered on the fly. The weights are reordered on
// the CPU at load to [out][ky][kx][in] so that consecutive k are consecutive
// input channels — the same pixel, one read apart — rather than PyTorch's
// [out][in][ky][kx], which would scatter every tile load across nine pixels.
//
// One kernel, three epilogues' worth of fusion: + bias, an activation, and
// + residual. Every one of those is a whole-tensor pass saved, and SwinIR's
// blocks are nothing but "linear, then add the shortcut".
//
// Tiling: 64×64 outputs per workgroup, 16×16 threads each owning a 4×4 block,
// K walked 16 at a time through workgroup memory. Plain fp32 and no
// cooperative-matrix extension, which wgpu doesn't expose — the number that
// matters is measured by `sr-selftest`, not assumed here.

struct P { u: array<vec4<u32>, 4> }

@group(0) @binding(0) var<storage, read> a: array<f32>;   // input [M, Cin] (conv: [H·W, Cin])
@group(0) @binding(1) var<storage, read> b: array<f32>;   // weight [N, K]
@group(0) @binding(2) var<storage, read> c: array<f32>;   // bias [N]
@group(0) @binding(3) var<storage, read> d: array<f32>;   // residual [M, N], or PReLU slopes [N]
@group(0) @binding(4) var<storage, read_write> o: array<f32>;
@group(0) @binding(5) var<uniform> p: P;

fn pu(i: u32) -> u32 { return p.u[i / 4u][i % 4u]; }
fn pf(i: u32) -> f32 { return bitcast<f32>(pu(i)); }

// Params: 0 M, 1 N, 2 K, 3 H, 4 W, 5 Cin, 6 ks, 7 act, 8 slope, 9 has_bias,
// 10 has_res.
const ACT_NONE: u32 = 0u;
const ACT_LEAKY: u32 = 1u;   // slope in param 8; 0 makes it a ReLU
const ACT_GELU: u32 = 2u;    // exact (erf), as nn.GELU()
const ACT_PRELU: u32 = 3u;   // per-channel slope from `d`
const ACT_SILU: u32 = 4u;

const BM: u32 = 64u;
const BN: u32 = 64u;
const BK: u32 = 16u;

var<workgroup> As: array<f32, 1024>; // [BK][BM]
var<workgroup> Bs: array<f32, 1024>; // [BK][BN]

// Abramowitz & Stegun 7.1.26, |error| < 1.5e-7 — below f32's own resolution
// around GELU's knee, so this is nn.GELU() for every purpose here.
fn erf(x: f32) -> f32 {
    let s = sign(x);
    let t = 1.0 / (1.0 + 0.3275911 * abs(x));
    let y = 1.0 - (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736) * t + 0.254829592) * t * exp(-x * x);
    return s * y;
}

fn a_at(m: u32, k: u32) -> f32 {
    let M = pu(0u);
    let K = pu(2u);
    if (m >= M || k >= K) { return 0.0; }
    let cin = pu(5u);
    let ks = pu(6u);
    if (ks == 1u) { return a[m * cin + k]; }
    let W = pu(4u);
    let H = pu(3u);
    let tap = k / cin;
    let ci = k - tap * cin;
    let pad = i32(ks / 2u);
    let y = i32(m / W) + i32(tap / ks) - pad;
    let x = i32(m % W) + i32(tap % ks) - pad;
    if (y < 0 || x < 0 || y >= i32(H) || x >= i32(W)) { return 0.0; }
    return a[(u32(y) * W + u32(x)) * cin + ci];
}

@compute @workgroup_size(16, 16)
fn main(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_id) lid: vec3<u32>) {
    let M = pu(0u);
    let N = pu(1u);
    let K = pu(2u);
    let m0 = wg.y * BM;
    let n0 = wg.x * BN;
    let tid = lid.y * 16u + lid.x;

    var acc: array<f32, 16>;
    for (var i = 0u; i < 16u; i++) { acc[i] = 0.0; }

    for (var k0 = 0u; k0 < K; k0 += BK) {
        for (var i = 0u; i < 4u; i++) {
            let e = tid + i * 256u;
            let r = e / BK;
            let kk = e % BK;
            As[kk * BM + r] = a_at(m0 + r, k0 + kk);
            let n = n0 + r;
            let k = k0 + kk;
            var w = 0.0;
            if (n < N && k < K) { w = b[n * K + k]; }
            Bs[kk * BN + r] = w;
        }
        workgroupBarrier();
        for (var kk = 0u; kk < BK; kk++) {
            let ar = kk * BM + lid.y * 4u;
            let br = kk * BN + lid.x * 4u;
            let av = vec4<f32>(As[ar], As[ar + 1u], As[ar + 2u], As[ar + 3u]);
            let bv = vec4<f32>(Bs[br], Bs[br + 1u], Bs[br + 2u], Bs[br + 3u]);
            for (var i = 0u; i < 4u; i++) {
                acc[i * 4u + 0u] += av[i] * bv.x;
                acc[i * 4u + 1u] += av[i] * bv.y;
                acc[i * 4u + 2u] += av[i] * bv.z;
                acc[i * 4u + 3u] += av[i] * bv.w;
            }
        }
        workgroupBarrier();
    }

    let act = pu(7u);
    let slope = pf(8u);
    let has_bias = pu(9u) != 0u;
    let has_res = pu(10u) != 0u;
    for (var i = 0u; i < 4u; i++) {
        let m = m0 + lid.y * 4u + i;
        if (m >= M) { continue; }
        for (var j = 0u; j < 4u; j++) {
            let n = n0 + lid.x * 4u + j;
            if (n >= N) { continue; }
            var v = acc[i * 4u + j];
            if (has_bias) { v += c[n]; }
            if (act == ACT_LEAKY) {
                v = select(v * slope, v, v >= 0.0);
            } else if (act == ACT_GELU) {
                v = 0.5 * v * (1.0 + erf(v * 0.70710678));
            } else if (act == ACT_PRELU) {
                v = select(v * d[n], v, v >= 0.0);
            } else if (act == ACT_SILU) {
                v = v / (1.0 + exp(-v));
            }
            if (has_res) { v += d[m * N + n]; }
            o[m * N + n] = v;
        }
    }
}
