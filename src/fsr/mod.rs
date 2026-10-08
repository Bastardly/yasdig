//! Native FSR 1.0 post-process on the GPU via wgpu (Vulkan backend). The two
//! passes FSR actually specifies: EASU (edge-adaptive upscale) then RCAS
//! (sharpen). No model weights, no Python — just compute shaders on the same
//! AMD GPU sd.cpp uses.
//!
//! Plus two passes FSR doesn't specify. A local-contrast ("clarity") unsharp
//! between the two, for the mid-frequency depth a diffusion refine can't reach
//! without re-composing the image — its knob is signed, so negative softens that
//! band rather than boosting it (`contrast.wgsl`). And a colour grade ahead of
//! both, eight per-pixel knobs in one dispatch (`grade.wgsl`).
//!
//! Every stage is skipped when it would be a no-op — EASU at scale 1.0, the
//! grade when all eight of its knobs are 0, contrast at clarity 0.0 — so
//! `process(.., 1.0, &Filters { sharpness, ..Filters::NONE })` is an exact
//! sharpen-only pass, and `Filters::NONE` at scale 1.0 is byte-exact.
//!
//! Both filters scale their working band with the image rather than with the
//! pixel grid ([`clarity_sigmas`], [`rcas_radius`]), so a setting means the same
//! thing before and after a hi-res pass.

use crate::config::Filters;
use anyhow::{bail, Context, Result};
use bytemuck::{Pod, Zeroable};
use std::path::Path;
use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct UpscaleParams {
    in_size: [u32; 2],
    out_size: [u32; 2],
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct SharpenParams {
    size: [u32; 2],
    sharpness: f32,
    radius: i32,
}

/// The eight grading knobs, plus the hue rotation already turned into a matrix.
/// Field order and padding must match `Params` in `grade.wgsl`: 2 u32 + 9 f32 +
/// one pad word = 48 bytes, which is the 16-byte multiple a uniform needs.
#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct GradeParams {
    size: [u32; 2],
    lightness: f32,
    contrast: f32,
    highlights: f32,
    shadows: f32,
    saturation: f32,
    temperature: f32,
    tint: f32,
    hue_sin: f32,
    hue_cos: f32,
    _pad: f32,
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct ContrastParams {
    size: [u32; 2],
    low_size: [u32; 2],
    amount: f32,
    sigma_wide: f32,
    sigma_narrow: f32,
    _pad: f32,
}

pub struct Fsr {
    device: wgpu::Device,
    queue: wgpu::Queue,
    adapter_name: String,
    upscale_pipeline: wgpu::ComputePipeline,
    upscale_bgl: wgpu::BindGroupLayout,
    sharpen_pipeline: wgpu::ComputePipeline,
    sharpen_bgl: wgpu::BindGroupLayout,
    /// Colour grading. One in, one out, one uniform — the same layout shape as
    /// sharpen, so it needs no layout of its own.
    grade_pipeline: wgpu::ComputePipeline,
    grade_bgl: wgpu::BindGroupLayout,
    /// Shared by the three passes that write the 1/8 luma plane: downsample,
    /// blur_h, blur_v. Same bind group shape, different entry points.
    plane_bgl: wgpu::BindGroupLayout,
    downsample_pipeline: wgpu::ComputePipeline,
    blur_h_pipeline: wgpu::ComputePipeline,
    blur_v_pipeline: wgpu::ComputePipeline,
    combine_pipeline: wgpu::ComputePipeline,
    combine_bgl: wgpu::BindGroupLayout,
}

const WG: u32 = 16;

/// Resolution divisor for the clarity pass's luma plane. Must match `DOWN` in
/// `contrast.wgsl`.
const DOWNSCALE: u32 = 8;

/// The two Gaussian sigmas for the clarity band-pass, in *low-res* pixels.
///
/// The wide lobe sets where depth begins — ~4% of the longest side, so
/// subject-scale structure moves and overall tone doesn't. The narrow lobe is an
/// eighth of that, and excludes grain.
///
/// Three octaves between the lobes makes this a deliberately *broad* pass, which
/// peaks around 8% of the longest side — ~120 px at 1536², between the two bands
/// `fsr-selftest` reports rather than at either. Measured there at clarity 0.6:
/// 48 px lifts 1.61×, 384 px lifts 1.40×, 3 px stays at 1.00× and overall tone
/// at 1.01×. So depth is the far shoulder of what this does and not its centre —
/// worth knowing before retuning either sigma, and worth re-running that command
/// afterwards.
///
/// Both lobes are proportional to the image, so "clarity 0.5" means the same
/// thing at 1536 and at 3072 — measured, not assumed: those same two bands come
/// back at 1.41× and 1.68× at 3072².
pub(crate) fn clarity_sigmas(w: u32, h: u32) -> (f32, f32) {
    let wide = (0.04 * w.max(h) as f32 / DOWNSCALE as f32).clamp(2.0, 16.0);
    (wide, (wide / 8.0).max(0.75))
}

/// Output size at which RCAS runs as the reference filter does, one pixel out.
/// It is the default pipeline's output — 768 base at `upscale_factor` 2.0 — so
/// the stock configuration sharpens exactly as it always has, and only images
/// that have been pushed past it change.
const RCAS_PIVOT: f32 = 1536.0;

/// Tap distance for the RCAS cross. Like [`clarity_sigmas`], it is proportional
/// to the image, for the same reason: a filter with a fixed pixel footprint
/// works on a finer and finer band as the picture grows, until it is grading
/// texture nobody is looking at. One pixel at 1536, two at 3072.
///
/// Capped at 3 — past that the cross is skipping enough pixels that it starts
/// to read as a halo rather than as an edge.
pub(crate) fn rcas_radius(w: u32, h: u32) -> i32 {
    (w.max(h) as f32 / RCAS_PIVOT).round().clamp(1.0, 3.0) as i32
}

impl Fsr {
    pub fn new() -> Result<Self> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..Default::default()
        });

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .context("no Vulkan adapter found (is the RADV/Vulkan driver installed?)")?;

        let adapter_name = adapter.get_info().name;

        // The defaults cap one storage binding at 128 MiB, which the FSR passes
        // never approach and `sr` does: a 4× SwinIR tail is 64 channels at 4×
        // the tile, 340 MB for a 288² tile. Asking for what the adapter offers,
        // capped at 1 GiB, lets `sr::plan_tile` pick tiles by the real limit.
        let offered = adapter.limits();
        // And the adapter's whole texture size, not wgpu's 8192 default: the
        // largest image is a setting (`Settings::max_image_side`), and the
        // device has to be able to go as far as that setting is allowed to.
        // Asking costs nothing until a texture that big is made.
        let limits = wgpu::Limits {
            max_storage_buffer_binding_size: offered.max_storage_buffer_binding_size.min(1 << 30),
            max_buffer_size: offered.max_buffer_size.min(1 << 30),
            max_texture_dimension_2d: offered.max_texture_dimension_2d,
            ..wgpu::Limits::default()
        };

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("rustimage-fsr"),
            required_features: wgpu::Features::empty(),
            required_limits: limits,
            memory_hints: wgpu::MemoryHints::Performance,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            trace: wgpu::Trace::Off,
        }))
        .context("failed to create wgpu device")?;

        let upscale_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("upscale"),
            source: wgpu::ShaderSource::Wgsl(include_str!("upscale.wgsl").into()),
        });
        let sharpen_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sharpen"),
            source: wgpu::ShaderSource::Wgsl(include_str!("sharpen.wgsl").into()),
        });

        let contrast_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("contrast"),
            source: wgpu::ShaderSource::Wgsl(include_str!("contrast.wgsl").into()),
        });

        let grade_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("grade"),
            source: wgpu::ShaderSource::Wgsl(include_str!("grade.wgsl").into()),
        });

        let upscale_bgl = Self::make_bgl(&device, "upscale-bgl", wgpu::TextureFormat::Rgba8Unorm);
        let sharpen_bgl = Self::make_bgl(&device, "sharpen-bgl", wgpu::TextureFormat::Rgba8Unorm);
        let grade_bgl = Self::make_bgl(&device, "grade-bgl", wgpu::TextureFormat::Rgba8Unorm);
        // The luma plane is 16-bit: an 8-bit low-pass bands visibly once the
        // difference between two of them is amplified.
        let plane_bgl = Self::make_bgl(&device, "plane-bgl", wgpu::TextureFormat::Rgba16Float);
        let combine_bgl = Self::make_combine_bgl(&device);

        let upscale_pipeline =
            Self::make_pipeline(&device, "upscale", &upscale_bgl, &upscale_shader, "main");
        let sharpen_pipeline =
            Self::make_pipeline(&device, "sharpen", &sharpen_bgl, &sharpen_shader, "main");
        let grade_pipeline =
            Self::make_pipeline(&device, "grade", &grade_bgl, &grade_shader, "main");
        let plane_pass = |name: &str, entry: &str| {
            Self::make_pipeline(&device, name, &plane_bgl, &contrast_shader, entry)
        };
        let downsample_pipeline = plane_pass("clarity-downsample", "downsample");
        let blur_h_pipeline = plane_pass("clarity-blur-h", "blur_h");
        let blur_v_pipeline = plane_pass("clarity-blur-v", "blur_v");
        let combine_pipeline = Self::make_pipeline(
            &device,
            "clarity-combine",
            &combine_bgl,
            &contrast_shader,
            "combine",
        );

        Ok(Fsr {
            device,
            queue,
            adapter_name,
            upscale_pipeline,
            upscale_bgl,
            sharpen_pipeline,
            sharpen_bgl,
            grade_pipeline,
            grade_bgl,
            plane_bgl,
            downsample_pipeline,
            blur_h_pipeline,
            blur_v_pipeline,
            combine_pipeline,
            combine_bgl,
        })
    }

    pub fn adapter_name(&self) -> &str {
        &self.adapter_name
    }

    /// The device and queue, for `sr`, which runs its networks on this same
    /// device rather than opening a second one.
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// Bind group layout for a plain one-in one-out pass: sampled texture in,
    /// storage texture out, uniform params. `format` is the storage format the
    /// pass writes.
    fn make_bgl(
        device: &wgpu::Device,
        label: &str,
        format: wgpu::TextureFormat,
    ) -> wgpu::BindGroupLayout {
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(label),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        })
    }

    /// The combine pass reads two textures — the 1/8 luma plane and the
    /// full-resolution source — so it needs its own layout. Binding numbers
    /// match `contrast.wgsl`: 0 source, 2 params, 3 plane, 4 output.
    fn make_combine_bgl(device: &wgpu::Device) -> wgpu::BindGroupLayout {
        let sampled = wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        };
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("clarity-combine-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: sampled,
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: sampled,
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
            ],
        })
    }

    fn make_pipeline(
        device: &wgpu::Device,
        label: &str,
        bgl: &wgpu::BindGroupLayout,
        shader: &wgpu::ShaderModule,
        entry_point: &str,
    ) -> wgpu::ComputePipeline {
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(label),
            bind_group_layouts: &[bgl],
            push_constant_ranges: &[],
        });
        device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(label),
            layout: Some(&layout),
            module: shader,
            entry_point: Some(entry_point),
            compilation_options: Default::default(),
            cache: None,
        })
    }

    fn make_texture(
        &self,
        w: u32,
        h: u32,
        usage: wgpu::TextureUsages,
        format: wgpu::TextureFormat,
        label: &str,
    ) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
    }

    /// Run upscale, then local contrast, then sharpen on `in_png`, writing
    /// `out_png`. Returns the output dimensions.
    ///
    /// Contrast sits between the two FSR passes, as in a photo pipeline: move
    /// the mid frequencies first, then sharpen the result. Each stage is skipped
    /// when it would be a no-op — EASU at scale 1.0, contrast at `clarity` 0.0.
    pub fn process(
        &self,
        in_png: &Path,
        out_png: &Path,
        upscale_factor: f32,
        filters: &Filters,
    ) -> Result<(u32, u32)> {
        let img = image::open(in_png)
            .with_context(|| format!("loading {}", in_png.display()))?
            .to_rgba8();
        let (in_w, in_h) = img.dimensions();
        let (out_w, out_h) = out_size((in_w, in_h), upscale_factor);
        self.check_fits(out_w.max(in_w), out_h.max(in_h))?;
        // Anything the checks above didn't foresee is caught here rather than
        // reaching wgpu's uncaptured-error handler, which *panics* — and a panic
        // takes the GPU worker thread down with the request still waiting on
        // it, so the page waits for ever. An error scope turns it into an `Err`.
        self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let done = self.process_inner(&img, out_png, (in_w, in_h), (out_w, out_h), filters);
        if let Some(e) = pollster::block_on(self.device.pop_error_scope()) {
            bail!("the GPU refused the post-process at {out_w}×{out_h}: {e}");
        }
        done
    }

    /// The largest side this device can process, input or output: the
    /// adapter's texture cap, which `new` asks for in full (16384 on a 7900
    /// XTX). The *chosen* limit, `Settings::max_image_side`, sits under it and
    /// is checked first, by `Engine::check_side`.
    pub fn max_side(&self) -> u32 {
        self.device.limits().max_texture_dimension_2d
    }

    /// Refuse a size the device can't hold, in words, before any GPU call: the
    /// texture side, and the readback buffer for the result (`process_inner`
    /// copies the whole output into one).
    fn check_fits(&self, w: u32, h: u32) -> Result<()> {
        let max = self.max_side();
        if w.max(h) > max {
            bail!(
                "{w}×{h} is past this GPU's limit of {max} px on a side, which no setting \
                 can raise — upscale by less, or from a smaller step in the history"
            );
        }
        let readback = u64::from((w * 4).next_multiple_of(256)) * u64::from(h);
        let cap = self.device.limits().max_buffer_size;
        if readback > cap {
            bail!("{w}×{h} needs a {readback}-byte readback, past this GPU's {cap}-byte buffer limit");
        }
        Ok(())
    }

    fn process_inner(
        &self,
        img: &image::RgbaImage,
        out_png: &Path,
        (in_w, in_h): (u32, u32),
        (out_w, out_h): (u32, u32),
        filters: &Filters,
    ) -> Result<(u32, u32)> {

        // Source texture (uploaded), intermediate (upscaled), and final.
        let src = self.make_texture(
            in_w,
            in_h,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            wgpu::TextureFormat::Rgba8Unorm,
            "src",
        );
        let mid = self.make_texture(
            out_w,
            out_h,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
            wgpu::TextureFormat::Rgba8Unorm,
            "mid",
        );
        let dst = self.make_texture(
            out_w,
            out_h,
            wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            wgpu::TextureFormat::Rgba8Unorm,
            "dst",
        );

        // Upload source pixels.
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &src,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            img,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(in_w * 4),
                rows_per_image: Some(in_h),
            },
            wgpu::Extent3d {
                width: in_w,
                height: in_h,
                depth_or_array_layers: 1,
            },
        );

        let src_view = src.create_view(&wgpu::TextureViewDescriptor::default());
        let mid_view = mid.create_view(&wgpu::TextureViewDescriptor::default());
        let dst_view = dst.create_view(&wgpu::TextureViewDescriptor::default());

        // Uniforms.
        let upscale_params = UpscaleParams {
            in_size: [in_w, in_h],
            out_size: [out_w, out_h],
        };
        let upscale_buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("upscale-params"),
                contents: bytemuck::bytes_of(&upscale_params),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let sharpen_params = SharpenParams {
            size: [out_w, out_h],
            sharpness: filters.sharpness,
            radius: rcas_radius(out_w, out_h),
        };
        let sharpen_buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("sharpen-params"),
                contents: bytemuck::bytes_of(&sharpen_params),
                usage: wgpu::BufferUsages::UNIFORM,
            });

        let upscale_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("upscale-bg"),
            layout: &self.upscale_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&src_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&mid_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: upscale_buf.as_entire_binding(),
                },
            ],
        });
        // At 1:1 there is nothing for EASU to reconstruct, and running it would
        // be a lossy no-op, so the next stage reads the source directly.
        let upscaling = out_w != in_w || out_h != in_h;
        let easu_out = if upscaling { &mid_view } else { &src_view };

        // Colour grading, before the two detail filters. A photo editor's order,
        // and load-bearing for both of them: clarity's band and RCAS's limiter
        // both act on contrast this pass has just changed, and grading *after* a
        // sharpen amplifies its halos and then clips them.
        //
        // Allocated only when it will run, like the clarity chain below. At
        // neutral the shader is an identity in intent but not to the bit — the
        // sRGB/OKLab round trip loses a least-significant bit here and there —
        // so skipping it is what keeps a neutral `process` byte-exact.
        let grading = filters.grades();
        let grade_tex = grading.then(|| {
            self.make_texture(
                out_w,
                out_h,
                wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
                wgpu::TextureFormat::Rgba8Unorm,
                "graded",
            )
        });
        let grade_view = grade_tex
            .as_ref()
            .map(|t| t.create_view(&wgpu::TextureViewDescriptor::default()));
        let (hue_sin, hue_cos) = (filters.hue * std::f32::consts::PI).sin_cos();
        let grade_params = GradeParams {
            size: [out_w, out_h],
            lightness: filters.lightness,
            contrast: filters.contrast,
            highlights: filters.highlights,
            shadows: filters.shadows,
            saturation: filters.saturation,
            temperature: filters.temperature,
            tint: filters.tint,
            hue_sin,
            hue_cos,
            _pad: 0.0,
        };
        let grade_buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("grade-params"),
                contents: bytemuck::bytes_of(&grade_params),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let grade_bg = grade_view.as_ref().map(|out| {
            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("grade-bg"),
                layout: &self.grade_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(easu_out),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(out),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: grade_buf.as_entire_binding(),
                    },
                ],
            })
        });
        let grade_out = grade_view.as_ref().unwrap_or(easu_out);

        // Local contrast, between the two FSR passes. Allocated only when it
        // will actually run: at clarity 0.0 the shader is an exact identity, so
        // there's no reason to pay for two dispatches and two textures. Either
        // sign is work — negative softens the band instead of boosting it.
        let contrasting = filters.clarity != 0.0;
        let (low_w, low_h) = (div_ceil(out_w, DOWNSCALE), div_ceil(out_h, DOWNSCALE));
        let plane = |label| {
            self.make_texture(
                low_w,
                low_h,
                wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
                wgpu::TextureFormat::Rgba16Float,
                label,
            )
        };
        // Two planes, ping-ponged: downsample writes a, blur_h a->b, blur_v
        // b->a, and combine reads a.
        let plane_a = contrasting.then(|| plane("clarity-plane-a"));
        let plane_b = contrasting.then(|| plane("clarity-plane-b"));
        let clarity_tex = contrasting.then(|| {
            self.make_texture(
                out_w,
                out_h,
                wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
                wgpu::TextureFormat::Rgba8Unorm,
                "clarity",
            )
        });
        let view = |t: &Option<wgpu::Texture>| {
            t.as_ref()
                .map(|t| t.create_view(&wgpu::TextureViewDescriptor::default()))
        };
        let (a_view, b_view, clarity_view) = (view(&plane_a), view(&plane_b), view(&clarity_tex));

        let (sigma_wide, sigma_narrow) = clarity_sigmas(out_w, out_h);
        let contrast_params = ContrastParams {
            size: [out_w, out_h],
            low_size: [low_w, low_h],
            amount: filters.clarity,
            sigma_wide,
            sigma_narrow,
            _pad: 0.0,
        };
        let contrast_buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("contrast-params"),
                contents: bytemuck::bytes_of(&contrast_params),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        // The three plane passes share one layout; only the bound textures differ.
        let plane_bg = |label, input: &wgpu::TextureView, output: &wgpu::TextureView| {
            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &self.plane_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(input),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(output),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: contrast_buf.as_entire_binding(),
                    },
                ],
            })
        };
        let clarity_bgs = match (&a_view, &b_view, &clarity_view) {
            (Some(a), Some(b), Some(out)) => Some((
                plane_bg("clarity-downsample-bg", grade_out, a),
                plane_bg("clarity-blur-h-bg", a, b),
                plane_bg("clarity-blur-v-bg", b, a),
                self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("clarity-combine-bg"),
                    layout: &self.combine_bgl,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(grade_out),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: contrast_buf.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: wgpu::BindingResource::TextureView(a),
                        },
                        wgpu::BindGroupEntry {
                            binding: 4,
                            resource: wgpu::BindingResource::TextureView(out),
                        },
                    ],
                }),
            )),
            _ => None,
        };

        let sharpen_in = clarity_view.as_ref().unwrap_or(grade_out);

        let sharpen_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sharpen-bg"),
            layout: &self.sharpen_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(sharpen_in),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&dst_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: sharpen_buf.as_entire_binding(),
                },
            ],
        });

        // Readback buffer (rows padded to 256-byte alignment).
        let bytes_per_row = out_w * 4;
        let padded_bpr = align_up(bytes_per_row, 256);
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (padded_bpr * out_h) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("fsr"),
            });
        if upscaling {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("easu-pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.upscale_pipeline);
            pass.set_bind_group(0, &upscale_bg, &[]);
            pass.dispatch_workgroups(div_ceil(out_w, WG), div_ceil(out_h, WG), 1);
        }
        if let Some(bg) = &grade_bg {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("grade-pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.grade_pipeline);
            pass.set_bind_group(0, bg, &[]);
            pass.dispatch_workgroups(div_ceil(out_w, WG), div_ceil(out_h, WG), 1);
        }
        if let Some((down_bg, h_bg, v_bg, combine_bg)) = &clarity_bgs {
            // The three plane passes run at 1/8, so together they cost a small
            // fraction of the full-resolution combine that follows.
            for (label, pipeline, bg) in [
                ("clarity-downsample-pass", &self.downsample_pipeline, down_bg),
                ("clarity-blur-h-pass", &self.blur_h_pipeline, h_bg),
                ("clarity-blur-v-pass", &self.blur_v_pipeline, v_bg),
            ] {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some(label),
                    timestamp_writes: None,
                });
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, bg, &[]);
                pass.dispatch_workgroups(div_ceil(low_w, WG), div_ceil(low_h, WG), 1);
            }

            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("clarity-combine-pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.combine_pipeline);
            pass.set_bind_group(0, combine_bg, &[]);
            pass.dispatch_workgroups(div_ceil(out_w, WG), div_ceil(out_h, WG), 1);
        }
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("rcas-pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.sharpen_pipeline);
            pass.set_bind_group(0, &sharpen_bg, &[]);
            pass.dispatch_workgroups(div_ceil(out_w, WG), div_ceil(out_h, WG), 1);
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &dst,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bpr),
                    rows_per_image: Some(out_h),
                },
            },
            wgpu::Extent3d {
                width: out_w,
                height: out_h,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(Some(encoder.finish()));

        // Map and wait.
        let slice = readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            let _ = tx.send(res);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .context("device poll failed")?;
        rx.recv()
            .context("map_async channel closed")?
            .context("failed to map readback buffer")?;

        let data = slice.get_mapped_range();
        let mut pixels = Vec::with_capacity((bytes_per_row * out_h) as usize);
        for row in 0..out_h {
            let start = (row * padded_bpr) as usize;
            let end = start + bytes_per_row as usize;
            pixels.extend_from_slice(&data[start..end]);
        }
        drop(data);
        readback.unmap();

        let out_img: image::RgbaImage = image::ImageBuffer::from_raw(out_w, out_h, pixels)
            .context("failed to assemble output image")?;
        out_img
            .save(out_png)
            .with_context(|| format!("saving {}", out_png.display()))?;

        Ok((out_w, out_h))
    }
}

/// The size [`Fsr::process`] turns `input` into at `factor`.
///
/// Public because it is the definition of the output size, not just EASU's:
/// an ESRGAN enlarge lands on exactly this too (`Engine::enlarge`), so which
/// upscaler ran never changes the size the page quoted before the click.
pub fn out_size(input: (u32, u32), factor: f32) -> (u32, u32) {
    let side = |v: u32| ((v as f32) * factor).round().max(1.0) as u32;
    (side(input.0), side(input.1))
}

fn align_up(v: u32, align: u32) -> u32 {
    (v + align - 1) / align * align
}

fn div_ceil(v: u32, d: u32) -> u32 {
    (v + d - 1) / d
}

#[cfg(test)]
mod tests {
    /// Parse and validate the WGSL the same way wgpu will at device creation,
    /// so a shader mistake fails here instead of on the GPU worker at startup.
    fn check(label: &str, source: &str) {
        let module = naga::front::wgsl::parse_str(source)
            .unwrap_or_else(|e| panic!("{label}: {}", e.emit_to_string(source)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{label}: {e:?}"));
    }

    #[test]
    fn shaders_compile() {
        check("easu", include_str!("upscale.wgsl"));
        check("rcas", include_str!("sharpen.wgsl"));
        check("clarity", include_str!("contrast.wgsl"));
        check("grade", include_str!("grade.wgsl"));
    }
}
