// The element-wise and reshaping ops: everything that isn't a matmul or an
// attention. All channels-last ([H·W, C]); one invocation per output element
// unless the entry point says otherwise, with the index spread over x and y so
// a 1152²×64 tensor (85 M elements) stays under the 65 535-workgroup limit.

struct P { u: array<vec4<u32>, 4> }

@group(0) @binding(0) var<storage, read> a: array<f32>;
@group(0) @binding(1) var<storage, read> b: array<f32>;
@group(0) @binding(2) var<storage, read> c: array<f32>;
@group(0) @binding(3) var<storage, read> d: array<f32>;
@group(0) @binding(4) var<storage, read_write> o: array<f32>;
@group(0) @binding(5) var<uniform> p: P;

fn pu(i: u32) -> u32 { return p.u[i / 4u][i % 4u]; }
fn pf(i: u32) -> f32 { return bitcast<f32>(pu(i)); }

fn index(wg: vec3<u32>, nwg: vec3<u32>, lid: vec3<u32>) -> u32 {
    return (wg.y * nwg.x + wg.x) * 256u + lid.x;
}

// o = a + b. Params: 0 n.
@compute @workgroup_size(256)
fn add(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>, @builtin(local_invocation_id) lid: vec3<u32>) {
    let i = index(wg, nwg, lid);
    if (i >= pu(0u)) { return; }
    o[i] = a[i] + b[i];
}

// SPAN's attention: o = (a + b) · (sigmoid(a) − ½), a the block's last conv and
// b its input. Params: 0 n.
@compute @workgroup_size(256)
fn span_gate(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>, @builtin(local_invocation_id) lid: vec3<u32>) {
    let i = index(wg, nwg, lid);
    if (i >= pu(0u)) { return; }
    let x = a[i];
    o[i] = (x + b[i]) * (1.0 / (1.0 + exp(-x)) - 0.5);
}

// nn.PixelShuffle(r) from [H·W, C·r²] to [(H·r)·(W·r), C]:
// out[c, y·r+i, x·r+j] = in[c·r² + i·r + j, y, x]. Params: 0 H, 1 W, 2 C, 3 r.
@compute @workgroup_size(256)
fn pixel_shuffle(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>, @builtin(local_invocation_id) lid: vec3<u32>) {
    let W = pu(1u);
    let C = pu(2u);
    let r = pu(3u);
    let n = pu(0u) * r * W * r * C;
    let i = index(wg, nwg, lid);
    if (i >= n) { return; }
    let ch = i % C;
    let px = i / C;
    let X = px % (W * r);
    let Y = px / (W * r);
    let src = ((Y / r) * W + X / r) * (C * r * r) + ch * r * r + (Y % r) * r + X % r;
    o[i] = a[src];
}

// F.interpolate(scale_factor=r, mode='nearest'). Params: 0 H, 1 W, 2 C, 3 r.
@compute @workgroup_size(256)
fn nearest(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>, @builtin(local_invocation_id) lid: vec3<u32>) {
    let W = pu(1u);
    let C = pu(2u);
    let r = pu(3u);
    let n = pu(0u) * r * W * r * C;
    let i = index(wg, nwg, lid);
    if (i >= n) { return; }
    let ch = i % C;
    let px = i / C;
    let X = px % (W * r);
    let Y = px / (W * r);
    o[i] = a[((Y / r) * W + X / r) * C + ch];
}

// Write [N, Cs] into channels [off, off+Cs) of [N, Cd] — torch.cat on dim 1.
// Params: 0 N, 1 Cs, 2 Cd, 3 off.
@compute @workgroup_size(256)
fn copy_channels(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>, @builtin(local_invocation_id) lid: vec3<u32>) {
    let Cs = pu(1u);
    let i = index(wg, nwg, lid);
    if (i >= pu(0u) * Cs) { return; }
    let n = i / Cs;
    o[n * pu(2u) + pu(3u) + i % Cs] = a[i];
}

// nn.LayerNorm over the channel axis, one invocation per row, a = x,
// b = weight, c = bias. Params: 0 rows, 1 C, 2 eps.
@compute @workgroup_size(256)
fn layernorm(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>, @builtin(local_invocation_id) lid: vec3<u32>) {
    let row = index(wg, nwg, lid);
    if (row >= pu(0u)) { return; }
    let C = pu(1u);
    let base = row * C;
    var mean = 0.0;
    for (var i = 0u; i < C; i++) { mean += a[base + i]; }
    mean /= f32(C);
    var v = 0.0;
    for (var i = 0u; i < C; i++) {
        let t = a[base + i] - mean;
        v += t * t;
    }
    let inv = 1.0 / sqrt(v / f32(C) + pf(2u));
    for (var i = 0u; i < C; i++) {
        o[base + i] = (a[base + i] - mean) * inv * b[i] + c[i];
    }
}
