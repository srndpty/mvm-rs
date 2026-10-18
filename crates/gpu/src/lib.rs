//! preview と書き出しが共有する compositor。
//!
//! layer は下から順に、premultiplied alpha で重ねる。座標は出力 pixel の中心
//! （fragment の position）を affine で素材 pixel へ写す。
//! YUV から RGB への変換は、layer の色空間とレンジを使う。

mod params;
mod yuv;

pub use mvm_core::{YuvMatrix, YuvRange};
pub use params::{LayerParams, MAX_DIMENSION, MAX_PIXELS, SourceKind, validate_output};

use std::fmt;

const SHADER: &str = include_str!("composite.wgsl");

#[derive(Debug)]
pub struct GpuError {
    message: String,
}

impl GpuError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for GpuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for GpuError {}

/// 1 layer の画素。stride は byte。NV12 / P010 の UV は幅・高さが半分。
pub enum LayerPixels<'a> {
    Nv12 {
        y: &'a [u8],
        uv: &'a [u8],
    },
    P010 {
        y: &'a [u8],
        uv: &'a [u8],
    },
    Rgba {
        data: &'a [u8],
    },
    /// 同じ [`Compositor`] の device で作った NV12 texture。`array_layer` は配列の slice。
    Nv12Texture {
        texture: &'a wgpu::Texture,
        array_layer: u32,
    },
    /// 同じ [`Compositor`] の device で作った P010 texture。`array_layer` は配列の slice。
    P010Texture {
        texture: &'a wgpu::Texture,
        array_layer: u32,
    },
}

pub struct Layer<'a> {
    pub params: LayerParams,
    pub pixels: LayerPixels<'a>,
}

pub struct Compositor {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    dummy_uv: wgpu::Texture,
}

impl Compositor {
    /// DX12 の device を作る。adapter が無ければ失敗させる。
    pub fn new() -> Result<Self, GpuError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::DX12,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        }))
        .map_err(|error| GpuError::new(format!("DX12 adapter がありません: {error}")))?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("mvm-compositor"),
            required_features: wgpu::Features::TEXTURE_FORMAT_NV12
                | wgpu::Features::TEXTURE_FORMAT_P010
                | wgpu::Features::TEXTURE_FORMAT_16BIT_NORM,
            ..Default::default()
        }))
        .map_err(|error| GpuError::new(format!("DX12 device を作れません: {error}")))?;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("composite"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("layer"),
            entries: &[
                texture_entry(0),
                texture_entry(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: std::num::NonZeroU64::new(params::UNIFORM_BYTES),
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("composite"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("composite"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("layer"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let dummy = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("dummy-uv"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rg8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &dummy,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &[128, 128],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(256),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        Ok(Self {
            device,
            queue,
            pipeline,
            layout,
            sampler,
            dummy_uv: dummy,
        })
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    /// layer を下から順に `output_width` × `output_height` の RGBA へ合成し、読み戻す。
    pub fn composite(
        &self,
        output_width: u32,
        output_height: u32,
        layers: &[Layer<'_>],
    ) -> Result<Vec<u8>, GpuError> {
        validate_output(output_width, output_height)?;
        for layer in layers {
            params::validate_layer(output_width, output_height, &layer.params)?;
            check_bytes(layer)?;
        }
        let target = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("composite-target"),
            size: wgpu::Extent3d {
                width: output_width,
                height: output_height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("composite"),
            });
        let mut groups = Vec::new();
        let mut views = Vec::new();
        for layer in layers {
            views.push(self.views(layer)?);
            let uniform = self.uniform(&layer.params);
            let (view0, view1) = views
                .last()
                .ok_or_else(|| GpuError::new("layer がありません"))?;
            groups.push(self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("layer"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(view0),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(view1),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: uniform.as_entire_binding(),
                    },
                ],
            }));
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("composite"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            for group in &groups {
                pass.set_bind_group(0, group, &[]);
                pass.draw(0..3, 0..1);
            }
        }
        let row_bytes = output_width
            .checked_mul(4)
            .ok_or_else(|| GpuError::new("出力の stride が溢れました"))?;
        let aligned = align_256(row_bytes)?;
        let buffer_size = u64::from(aligned)
            .checked_mul(u64::from(output_height))
            .ok_or_else(|| GpuError::new("読み戻し buffer の大きさが溢れました"))?;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: buffer_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(aligned),
                    rows_per_image: Some(output_height),
                },
            },
            wgpu::Extent3d {
                width: output_width,
                height: output_height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(std::iter::once(encoder.finish()));
        let (sender, receiver) = std::sync::mpsc::channel();
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        receiver
            .recv()
            .map_err(|_| GpuError::new("読み戻しの完了を受け取れませんでした"))?
            .map_err(|error| GpuError::new(format!("読み戻しに失敗しました: {error}")))?;
        let mapped = buffer
            .slice(..)
            .get_mapped_range()
            .map_err(|error| GpuError::new(format!("読み戻し範囲を参照できません: {error}")))?;
        let mut rgba = vec![0u8; row_bytes as usize * output_height as usize];
        for row in 0..output_height as usize {
            let src = row * aligned as usize;
            let dst = row * row_bytes as usize;
            rgba[dst..dst + row_bytes as usize]
                .copy_from_slice(&mapped[src..src + row_bytes as usize]);
        }
        drop(mapped);
        buffer.unmap();
        Ok(rgba)
    }

    fn views(&self, layer: &Layer<'_>) -> Result<(wgpu::TextureView, wgpu::TextureView), GpuError> {
        match &layer.pixels {
            LayerPixels::Rgba { data } => {
                let texture = self.upload_rgba(&layer.params, data)?;
                Ok((
                    texture.create_view(&wgpu::TextureViewDescriptor::default()),
                    self.dummy_uv
                        .create_view(&wgpu::TextureViewDescriptor::default()),
                ))
            }
            LayerPixels::Nv12 { y, uv } => {
                let (y_tex, uv_tex) = self.upload_planes(
                    &layer.params,
                    y,
                    uv,
                    wgpu::TextureFormat::R8Unorm,
                    wgpu::TextureFormat::Rg8Unorm,
                    1,
                )?;
                Ok((
                    y_tex.create_view(&wgpu::TextureViewDescriptor::default()),
                    uv_tex.create_view(&wgpu::TextureViewDescriptor::default()),
                ))
            }
            LayerPixels::P010 { y, uv } => {
                let (y_tex, uv_tex) = self.upload_planes(
                    &layer.params,
                    y,
                    uv,
                    wgpu::TextureFormat::R16Unorm,
                    wgpu::TextureFormat::Rg16Unorm,
                    2,
                )?;
                Ok((
                    y_tex.create_view(&wgpu::TextureViewDescriptor::default()),
                    uv_tex.create_view(&wgpu::TextureViewDescriptor::default()),
                ))
            }
            LayerPixels::Nv12Texture {
                texture,
                array_layer,
            } => Ok((
                texture.create_view(&plane_view(
                    *array_layer,
                    wgpu::TextureFormat::R8Unorm,
                    wgpu::TextureAspect::Plane0,
                )),
                texture.create_view(&plane_view(
                    *array_layer,
                    wgpu::TextureFormat::Rg8Unorm,
                    wgpu::TextureAspect::Plane1,
                )),
            )),
            LayerPixels::P010Texture {
                texture,
                array_layer,
            } => Ok((
                texture.create_view(&plane_view(
                    *array_layer,
                    wgpu::TextureFormat::R16Unorm,
                    wgpu::TextureAspect::Plane0,
                )),
                texture.create_view(&plane_view(
                    *array_layer,
                    wgpu::TextureFormat::Rg16Unorm,
                    wgpu::TextureAspect::Plane1,
                )),
            )),
        }
    }

    fn upload_rgba(&self, params: &LayerParams, data: &[u8]) -> Result<wgpu::Texture, GpuError> {
        let texture = self.texture(
            params.width,
            params.height,
            wgpu::TextureFormat::Rgba8Unorm,
            "rgba",
        )?;
        self.write_plane(
            &texture,
            data,
            params.stride_y,
            params.width,
            params.height,
            4,
        )?;
        Ok(texture)
    }

    fn upload_planes(
        &self,
        params: &LayerParams,
        y: &[u8],
        uv: &[u8],
        y_format: wgpu::TextureFormat,
        uv_format: wgpu::TextureFormat,
        bytes_per_pixel: u32,
    ) -> Result<(wgpu::Texture, wgpu::Texture), GpuError> {
        let y_tex = self.texture(params.width, params.height, y_format, "y")?;
        let uv_width = params.width / 2;
        let uv_height = params.height / 2;
        let uv_tex = self.texture(uv_width, uv_height, uv_format, "uv")?;
        self.write_plane(
            &y_tex,
            y,
            params.stride_y,
            params.width,
            params.height,
            bytes_per_pixel,
        )?;
        self.write_plane(
            &uv_tex,
            uv,
            params.stride_uv,
            uv_width,
            uv_height,
            bytes_per_pixel * 2,
        )?;
        Ok((y_tex, uv_tex))
    }

    fn texture(
        &self,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
        label: &str,
    ) -> Result<wgpu::Texture, GpuError> {
        Ok(self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        }))
    }

    fn write_plane(
        &self,
        texture: &wgpu::Texture,
        data: &[u8],
        stride: u32,
        width: u32,
        height: u32,
        bytes_per_pixel: u32,
    ) -> Result<(), GpuError> {
        let tight = width
            .checked_mul(bytes_per_pixel)
            .ok_or_else(|| GpuError::new("画素の stride が溢れました"))?;
        let aligned = align_256(stride.max(tight))?;
        let row_count = usize::try_from(height).map_err(|_| GpuError::new("高さが不正です"))?;
        let stride_usize =
            usize::try_from(stride).map_err(|_| GpuError::new("stride が不正です"))?;
        let tight_usize = usize::try_from(tight).map_err(|_| GpuError::new("stride が不正です"))?;
        let aligned_usize =
            usize::try_from(aligned).map_err(|_| GpuError::new("stride が不正です"))?;
        let padded_len = aligned_usize
            .checked_mul(row_count)
            .ok_or_else(|| GpuError::new("画素 buffer の大きさが溢れました"))?;
        let mut padded = vec![0u8; padded_len];
        for row in 0..row_count {
            let src = row
                .checked_mul(stride_usize)
                .ok_or_else(|| GpuError::new("画素の行が範囲外です"))?;
            let end = src
                .checked_add(tight_usize)
                .ok_or_else(|| GpuError::new("画素の行が範囲外です"))?;
            let source = data
                .get(src..end)
                .ok_or_else(|| GpuError::new("画素データが stride に対して足りません"))?;
            let dst = row * aligned_usize;
            padded[dst..dst + tight_usize].copy_from_slice(source);
        }
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &padded,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(aligned),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        Ok(())
    }

    fn uniform(&self, params: &LayerParams) -> wgpu::Buffer {
        let data = params::uniform_words(params);
        let bytes = f32_bytes(&data);
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("layer-params"),
            size: params::UNIFORM_BYTES,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.queue.write_buffer(&buffer, 0, &bytes);
        buffer
    }
}

fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn plane_view(
    array_layer: u32,
    format: wgpu::TextureFormat,
    aspect: wgpu::TextureAspect,
) -> wgpu::TextureViewDescriptor<'static> {
    wgpu::TextureViewDescriptor {
        label: None,
        format: Some(format),
        dimension: Some(wgpu::TextureViewDimension::D2),
        usage: None,
        aspect,
        base_mip_level: 0,
        mip_level_count: Some(1),
        base_array_layer: array_layer,
        array_layer_count: Some(1),
    }
}

fn align_256(value: u32) -> Result<u32, GpuError> {
    value
        .checked_add(255)
        .map(|sum| sum & !255)
        .ok_or_else(|| GpuError::new("stride の整列で溢れました"))
}

fn f32_bytes(values: &[f32; 24]) -> [u8; 96] {
    let mut bytes = [0u8; 96];
    for (index, value) in values.iter().enumerate() {
        bytes[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
    }
    bytes
}

fn check_bytes(layer: &Layer<'_>) -> Result<(), GpuError> {
    let height = layer.params.height;
    let y_need = layer
        .params
        .stride_y
        .checked_mul(height)
        .ok_or_else(|| GpuError::new("画素 buffer の大きさが溢れました"))?
        as usize;
    match &layer.pixels {
        LayerPixels::Rgba { data } => {
            if data.len() < y_need {
                return Err(GpuError::new("RGBA の画素が足りません"));
            }
        }
        LayerPixels::Nv12 { y, uv } | LayerPixels::P010 { y, uv } => {
            let uv_height = height / 2;
            let uv_need = layer
                .params
                .stride_uv
                .checked_mul(uv_height)
                .ok_or_else(|| GpuError::new("UV buffer の大きさが溢れました"))?
                as usize;
            if y.len() < y_need || uv.len() < uv_need {
                return Err(GpuError::new("NV12 / P010 の画素が足りません"));
            }
        }
        LayerPixels::Nv12Texture { .. } | LayerPixels::P010Texture { .. } => {}
    }
    Ok(())
}
