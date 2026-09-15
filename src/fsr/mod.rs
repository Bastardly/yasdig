//! Native FSR post-process on the GPU via wgpu (Vulkan backend). Two compute
//! passes: Lanczos upscale (EASU slot) then FidelityFX CAS sharpen. No model
//! weights, no Python — just compute shaders on the same AMD GPU sd.cpp uses.

use anyhow::{Context, Result};
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
}

const WG: u32 = 16;

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

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("rustimage-fsr"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
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

        let upscale_bgl = Self::make_bgl(&device, "upscale-bgl");
        let sharpen_bgl = Self::make_bgl(&device, "sharpen-bgl");

        let upscale_pipeline = Self::make_pipeline(&device, "upscale", &upscale_bgl, &upscale_shader);
        let sharpen_pipeline = Self::make_pipeline(&device, "sharpen", &sharpen_bgl, &sharpen_shader);

        Ok(Fsr {
            device,
            queue,
            adapter_name,
            upscale_pipeline,
            upscale_bgl,
            sharpen_pipeline,
            sharpen_bgl,
        })
    }

    pub fn adapter_name(&self) -> &str {
        &self.adapter_name
    }

    /// Bind group layout shared by both passes: sampled texture in, storage
    /// texture out, uniform params.
    fn make_bgl(device: &wgpu::Device, label: &str) -> wgpu::BindGroupLayout {
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
                        format: wgpu::TextureFormat::Rgba8Unorm,
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

    fn make_pipeline(
        device: &wgpu::Device,
        label: &str,
        bgl: &wgpu::BindGroupLayout,
        shader: &wgpu::ShaderModule,
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
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        })
    }

    fn make_texture(&self, w: u32, h: u32, usage: wgpu::TextureUsages, label: &str) -> wgpu::Texture {
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
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage,
            view_formats: &[],
        })
    }

    /// Run upscale + sharpen on `in_png`, writing `out_png`. Returns the output
    /// dimensions.
    pub fn process(
        &self,
        in_png: &Path,
        out_png: &Path,
        upscale_factor: f32,
        sharpness: f32,
    ) -> Result<(u32, u32)> {
        let img = image::open(in_png)
            .with_context(|| format!("loading {}", in_png.display()))?
            .to_rgba8();
        let (in_w, in_h) = img.dimensions();
        let out_w = ((in_w as f32) * upscale_factor).round().max(1.0) as u32;
        let out_h = ((in_h as f32) * upscale_factor).round().max(1.0) as u32;

        // Source texture (uploaded), intermediate (upscaled), and final.
        let src = self.make_texture(
            in_w,
            in_h,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            "src",
        );
        let mid = self.make_texture(
            out_w,
            out_h,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
            "mid",
        );
        let dst = self.make_texture(
            out_w,
            out_h,
            wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
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
            &img,
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
            sharpness,
            _pad: 0.0,
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
        let sharpen_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sharpen-bg"),
            layout: &self.sharpen_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&mid_view),
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
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("upscale-pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.upscale_pipeline);
            pass.set_bind_group(0, &upscale_bg, &[]);
            pass.dispatch_workgroups(div_ceil(out_w, WG), div_ceil(out_h, WG), 1);
        }
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("sharpen-pass"),
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

fn align_up(v: u32, align: u32) -> u32 {
    (v + align - 1) / align * align
}

fn div_ceil(v: u32, d: u32) -> u32 {
    (v + d - 1) / d
}
