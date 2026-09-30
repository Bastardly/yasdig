//! The host side of the shaders: tensors, a buffer pool, and one function per op.
//!
//! Every op records into one open command encoder and returns immediately, so a
//! network is built by calling these in forward order. The encoder is
//! submitted every [`FLUSH_EVERY`] dispatches — not once per tile, because a
//! single submission that runs for seconds is what the kernel's GPU hang
//! detector exists to kill — and read back only at the end of a tile.
//!
//! **Intermediates are pooled, not freed.** A SwinIR-M tile is ~500
//! dispatches, and each wants an output the size of the feature map (a 288²
//! tile at 180 channels is 57 MB). Without reuse that is tens of gigabytes
//! allocated before the first submission. [`Gpu::recycle`] hands a tensor's
//! buffer back and the next op of a similar size takes it; ordering is safe
//! because every dispatch is recorded on the one queue, where wgpu places the
//! barriers between a read and a later write of the same buffer.

use anyhow::{Context, Result};
use std::cell::{Cell, RefCell};
use wgpu::util::DeviceExt;

/// A channels-last image: `h·w` rows of `c` floats.
pub struct T {
    pub buf: wgpu::Buffer,
    pub h: u32,
    pub w: u32,
    pub c: u32,
}

impl T {
    pub fn len(&self) -> u64 {
        self.h as u64 * self.w as u64 * self.c as u64
    }
}

/// A weight on the GPU, and how many floats it holds.
pub struct Wt {
    pub buf: wgpu::Buffer,
    pub n: usize,
}

const FLUSH_EVERY: u32 = 64;

/// Activation applied in the matmul epilogue. Numbers match `gemm.wgsl`.
#[derive(Clone, Copy)]
pub enum Act {
    None,
    /// LeakyReLU with this slope; 0 is a ReLU.
    Leaky(f32),
    Gelu,
    /// PReLU, one slope per output channel.
    Prelu,
    Silu,
}

struct Pipes {
    gemm: wgpu::ComputePipeline,
    attn: wgpu::ComputePipeline,
    add: wgpu::ComputePipeline,
    span_gate: wgpu::ComputePipeline,
    pixel_shuffle: wgpu::ComputePipeline,
    nearest: wgpu::ComputePipeline,
    copy_channels: wgpu::ComputePipeline,
    layernorm: wgpu::ComputePipeline,
}

pub struct Gpu<'d> {
    pub device: &'d wgpu::Device,
    pub queue: &'d wgpu::Queue,
    bgl: wgpu::BindGroupLayout,
    pipes: Pipes,
    dummy: wgpu::Buffer,
    pool: RefCell<Vec<wgpu::Buffer>>,
    encoder: RefCell<Option<wgpu::CommandEncoder>>,
    pending: Cell<u32>,
}

fn storage(binding: u32, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

impl<'d> Gpu<'d> {
    pub fn new(device: &'d wgpu::Device, queue: &'d wgpu::Queue) -> Self {
        // One layout for every shader: four inputs, one output, one uniform.
        // An op that reads fewer binds the dummy — explicit rather than
        // auto-derived, since a derived layout drops the bindings an entry
        // point doesn't touch and the bind groups would stop matching.
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sr-bgl"),
            entries: &[
                storage(0, true),
                storage(1, true),
                storage(2, true),
                storage(3, true),
                storage(4, false),
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sr"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        let module = |label: &str, src: &str| {
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(src.into()),
            })
        };
        let gemm = module("sr-gemm", include_str!("gemm.wgsl"));
        let attn = module("sr-attn", include_str!("attn.wgsl"));
        let elt = module("sr-elt", include_str!("elt.wgsl"));
        let pipe = |m: &wgpu::ShaderModule, entry: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&layout),
                module: m,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let pipes = Pipes {
            gemm: pipe(&gemm, "main"),
            attn: pipe(&attn, "main"),
            add: pipe(&elt, "add"),
            span_gate: pipe(&elt, "span_gate"),
            pixel_shuffle: pipe(&elt, "pixel_shuffle"),
            nearest: pipe(&elt, "nearest"),
            copy_channels: pipe(&elt, "copy_channels"),
            layernorm: pipe(&elt, "layernorm"),
        };
        let dummy = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sr-dummy"),
            size: 16,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        Gpu {
            device,
            queue,
            bgl,
            pipes,
            dummy,
            pool: RefCell::new(Vec::new()),
            encoder: RefCell::new(None),
            pending: Cell::new(0),
        }
    }

    // --- memory ------------------------------------------------------------

    pub fn upload(&self, data: &[f32]) -> Wt {
        let buf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::cast_slice(if data.is_empty() { &[0.0f32][..] } else { data }),
            usage: wgpu::BufferUsages::STORAGE,
        });
        Wt { buf, n: data.len() }
    }

    /// A tensor with a pooled buffer and undefined contents.
    pub fn alloc(&self, h: u32, w: u32, c: u32) -> T {
        let bytes = (h as u64 * w as u64 * c as u64 * 4).max(16);
        let mut pool = self.pool.borrow_mut();
        // Best fit, and no more than twice the size: handing a 300 MB buffer
        // to a 1 MB tensor would pin the big one for nothing.
        let pick = pool
            .iter()
            .enumerate()
            .filter(|(_, b)| b.size() >= bytes && b.size() <= bytes * 2)
            .min_by_key(|(_, b)| b.size())
            .map(|(i, _)| i);
        let buf = match pick {
            Some(i) => pool.swap_remove(i),
            None => self.device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: bytes.div_ceil(256) * 256,
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_SRC
                    | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
        };
        T { buf, h, w, c }
    }

    pub fn recycle(&self, t: T) {
        self.pool.borrow_mut().push(t.buf);
    }

    /// A tensor holding `data`.
    ///
    /// Flushes first, and must. `write_buffer` lands before the *next*
    /// submission, so with commands still unsubmitted it would run ahead of
    /// them — and a pooled buffer may be one they still read.
    pub fn tensor(&self, h: u32, w: u32, c: u32, data: &[f32]) -> T {
        self.flush();
        let t = self.alloc(h, w, c);
        self.queue.write_buffer(&t.buf, 0, bytemuck::cast_slice(data));
        t
    }

    /// Submit everything recorded, wait, and copy a tensor out.
    pub fn read(&self, t: &T) -> Result<Vec<f32>> {
        let bytes = t.len() * 4;
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sr-readback"),
            size: bytes.max(4),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.with_encoder(|e| e.copy_buffer_to_buffer(&t.buf, 0, &staging, 0, bytes));
        self.flush();
        let slice = staging.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .context("device poll failed")?;
        rx.recv()
            .context("map_async channel closed")?
            .context("failed to map the readback buffer")?;
        // Decoded rather than cast: a mapped range makes no alignment promise
        // that `bytemuck::cast_slice` could rely on.
        let out = slice
            .get_mapped_range()
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        staging.unmap();
        Ok(out)
    }

    pub fn flush(&self) {
        if let Some(enc) = self.encoder.borrow_mut().take() {
            self.queue.submit(Some(enc.finish()));
        }
        self.pending.set(0);
    }

    fn with_encoder(&self, f: impl FnOnce(&mut wgpu::CommandEncoder)) {
        let mut slot = self.encoder.borrow_mut();
        let enc = slot.get_or_insert_with(|| {
            self.device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("sr") })
        });
        f(enc);
    }

    fn dispatch(
        &self,
        pipe: &wgpu::ComputePipeline,
        inputs: [Option<&wgpu::Buffer>; 4],
        out: &wgpu::Buffer,
        params: &[u32],
        groups: (u32, u32, u32),
    ) {
        let mut u = [0u32; 16];
        u[..params.len()].copy_from_slice(params);
        let ubuf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::cast_slice(&u),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind = |i: usize| inputs[i].unwrap_or(&self.dummy).as_entire_binding();
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: bind(0) },
                wgpu::BindGroupEntry { binding: 1, resource: bind(1) },
                wgpu::BindGroupEntry { binding: 2, resource: bind(2) },
                wgpu::BindGroupEntry { binding: 3, resource: bind(3) },
                wgpu::BindGroupEntry { binding: 4, resource: out.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 5, resource: ubuf.as_entire_binding() },
            ],
        });
        self.with_encoder(|e| {
            let mut pass = e.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(pipe);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups(groups.0, groups.1, groups.2);
        });
        let n = self.pending.get() + 1;
        self.pending.set(n);
        if n >= FLUSH_EVERY {
            self.flush();
        }
    }

    /// Workgroups for `n` elements at 256 per group, spread over x and y.
    fn spread(n: u64) -> (u32, u32, u32) {
        let groups = n.div_ceil(256).max(1);
        let x = groups.min(65_535);
        (x as u32, groups.div_ceil(x) as u32, 1)
    }

    // --- ops ---------------------------------------------------------------

    /// A k×k convolution, stride 1, zero padding k/2 — or, at `ks` 1, a Linear
    /// layer over every pixel. `w` must be in [out][ky][kx][in] order (see
    /// [`conv_weight`]). `extra` is the residual added after the activation,
    /// or the PReLU slopes, as `act` says.
    #[allow(clippy::too_many_arguments)]
    pub fn conv(&self, x: &T, w: &Wt, bias: Option<&Wt>, ks: u32, act: Act, extra: Option<&T>, slopes: Option<&Wt>) -> T {
        let k = x.c * ks * ks;
        let n = (w.n as u32) / k;
        debug_assert_eq!(n * k, w.n as u32, "weight does not match {}×{}×{}", ks, ks, x.c);
        let y = self.alloc(x.h, x.w, n);
        let m = x.h * x.w;
        let (act_id, slope) = match act {
            Act::None => (0, 0.0),
            Act::Leaky(s) => (1, s),
            Act::Gelu => (2, 0.0),
            Act::Prelu => (3, 0.0),
            Act::Silu => (4, 0.0),
        };
        let d = match act {
            Act::Prelu => slopes.map(|s| &s.buf),
            _ => extra.map(|e| &e.buf),
        };
        self.dispatch(
            &self.pipes.gemm,
            [Some(&x.buf), Some(&w.buf), bias.map(|b| &b.buf), d],
            &y.buf,
            &[
                m,
                n,
                k,
                x.h,
                x.w,
                x.c,
                ks,
                act_id,
                f32::to_bits(slope),
                bias.is_some() as u32,
                (extra.is_some() && !matches!(act, Act::Prelu)) as u32,
            ],
            (n.div_ceil(64), m.div_ceil(64), 1),
        );
        y
    }

    /// Window attention over `qkv` ([H·W, 3C]); `bias` is [heads][ws²][ws²].
    pub fn window_attn(&self, qkv: &T, c: u32, heads: u32, ws: u32, shift: u32, bias: &Wt) -> T {
        let hd = c / heads;
        let out = self.alloc(qkv.h, qkv.w, c);
        let windows = (qkv.h / ws) * (qkv.w / ws);
        self.dispatch(
            &self.pipes.attn,
            [Some(&qkv.buf), Some(&bias.buf), None, None],
            &out.buf,
            &[qkv.h, qkv.w, c, heads, hd, ws, shift, f32::to_bits((hd as f32).powf(-0.5))],
            (windows, heads, (ws * ws).div_ceil(64)),
        );
        out
    }

    /// A copy that owns its own buffer, for a value that is both read later and
    /// replaced — a group's input, which is its residual at the end.
    pub fn copy(&self, x: &T) -> T {
        let y = self.alloc(x.h, x.w, x.c);
        self.with_encoder(|e| e.copy_buffer_to_buffer(&x.buf, 0, &y.buf, 0, x.len() * 4));
        y
    }

    pub fn add(&self, a: &T, b: &T) -> T {
        let y = self.alloc(a.h, a.w, a.c);
        let n = a.len();
        self.dispatch(&self.pipes.add, [Some(&a.buf), Some(&b.buf), None, None], &y.buf, &[n as u32], Self::spread(n));
        y
    }

    pub fn span_gate(&self, out3: &T, x: &T) -> T {
        let y = self.alloc(x.h, x.w, x.c);
        let n = x.len();
        self.dispatch(&self.pipes.span_gate, [Some(&out3.buf), Some(&x.buf), None, None], &y.buf, &[n as u32], Self::spread(n));
        y
    }

    pub fn pixel_shuffle(&self, x: &T, r: u32) -> T {
        let c = x.c / (r * r);
        let y = self.alloc(x.h * r, x.w * r, c);
        self.dispatch(&self.pipes.pixel_shuffle, [Some(&x.buf), None, None, None], &y.buf, &[x.h, x.w, c, r], Self::spread(y.len()));
        y
    }

    pub fn nearest(&self, x: &T, r: u32) -> T {
        let y = self.alloc(x.h * r, x.w * r, x.c);
        self.dispatch(&self.pipes.nearest, [Some(&x.buf), None, None, None], &y.buf, &[x.h, x.w, x.c, r], Self::spread(y.len()));
        y
    }

    /// `torch.cat(parts, dim=1)`.
    pub fn concat(&self, parts: &[&T]) -> T {
        let cd: u32 = parts.iter().map(|p| p.c).sum();
        let y = self.alloc(parts[0].h, parts[0].w, cd);
        let n = parts[0].h * parts[0].w;
        let mut off = 0;
        for p in parts {
            self.dispatch(
                &self.pipes.copy_channels,
                [Some(&p.buf), None, None, None],
                &y.buf,
                &[n, p.c, cd, off],
                Self::spread(n as u64 * p.c as u64),
            );
            off += p.c;
        }
        y
    }

    pub fn layernorm(&self, x: &T, g: &Wt, b: &Wt) -> T {
        let y = self.alloc(x.h, x.w, x.c);
        let rows = x.h * x.w;
        self.dispatch(
            &self.pipes.layernorm,
            [Some(&x.buf), Some(&g.buf), Some(&b.buf), None],
            &y.buf,
            &[rows, x.c, f32::to_bits(1e-5)],
            Self::spread(rows as u64),
        );
        y
    }
}

/// PyTorch's conv weight [out][in][kh][kw] as the matmul wants it,
/// [out][kh][kw][in]. A Linear weight ([out][in]) is already right.
pub fn conv_weight(w: &[f32], shape: &[usize]) -> Vec<f32> {
    let [o, i, kh, kw] = [shape[0], shape[1], shape[2], shape[3]];
    let mut out = vec![0.0; w.len()];
    for oo in 0..o {
        for ii in 0..i {
            for y in 0..kh {
                for x in 0..kw {
                    out[((oo * kh + y) * kw + x) * i + ii] = w[((oo * i + ii) * kh + y) * kw + x];
                }
            }
        }
    }
    out
}
