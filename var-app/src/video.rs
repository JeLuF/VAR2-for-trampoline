//! NV12 video rendering with wgpu (AD-7).
//!
//! Each video view owns a Y texture (R8) and a CbCr texture (RG8, half size). Frames
//! are uploaded as they are and converted to RGB in the fragment shader with a colour
//! matrix taken from the frame's colorimetry, so the CPU does no colour conversion.

use std::collections::HashMap;
use std::sync::Arc;

use eframe::egui;
use eframe::egui_wgpu::{self, CallbackResources, CallbackTrait, ScreenDescriptor};
use var_capture::{PreviewFrame, YuvColorimetry, YuvMatrix};
use var_playback::ReviewFrame;

const SHADER: &str = include_str!("video.wgsl");

/// An NV12 frame that can be shown: live preview or decoded review frame.
pub trait Nv12Frame: Send + Sync {
    fn width(&self) -> u32;
    fn height(&self) -> u32;
    /// Plane 0: Y, plane 1: interleaved CbCr, rows `stride` bytes apart.
    fn plane(&self, index: u32) -> &[u8];
    fn stride(&self, index: u32) -> u32;
    fn colorimetry(&self) -> YuvColorimetry;
}

macro_rules! impl_nv12_frame {
    ($type:ty) => {
        impl Nv12Frame for $type {
            fn width(&self) -> u32 {
                <$type>::width(self)
            }
            fn height(&self) -> u32 {
                <$type>::height(self)
            }
            fn plane(&self, index: u32) -> &[u8] {
                <$type>::plane(self, index)
            }
            fn stride(&self, index: u32) -> u32 {
                <$type>::stride(self, index)
            }
            fn colorimetry(&self) -> YuvColorimetry {
                <$type>::colorimetry(self)
            }
        }
    };
}
impl_nv12_frame!(PreviewFrame);
impl_nv12_frame!(ReviewFrame);

/// Identifies a video view; each view has its own textures.
pub type ViewId = u64;

/// GPU resources shared by all video views, stored in egui's callback resources.
pub struct VideoRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    views: HashMap<ViewId, ViewTextures>,
}

struct ViewTextures {
    size: (u32, u32),
    y: wgpu::Texture,
    uv: wgpu::Texture,
    uniforms: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

impl VideoRenderer {
    /// Creates the renderer and registers it with egui's wgpu renderer.
    pub fn register(render_state: &egui_wgpu::RenderState) {
        let renderer = VideoRenderer::new(&render_state.device, render_state.target_format);
        render_state
            .renderer
            .write()
            .callback_resources
            .insert(renderer);
    }

    fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        // egui normally renders to a non-sRGB target with gamma-encoded colours, which
        // is what the video already is. For an sRGB target, linearise in the shader.
        let source = SHADER.replace(
            "const LINEAR_TARGET: bool = false;",
            &format!("const LINEAR_TARGET: bool = {};", target_format.is_srgb()),
        );
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("video"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });

        let texture_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("video"),
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
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("video"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("video"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("video"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        VideoRenderer {
            pipeline,
            bind_group_layout,
            sampler,
            views: HashMap::new(),
        }
    }

    fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: ViewId,
        frame: &dyn Nv12Frame,
    ) {
        let size = (frame.width(), frame.height());
        if self.views.get(&view).is_none_or(|t| t.size != size) {
            let textures = self.create_textures(device, size);
            self.views.insert(view, textures);
        }
        let textures = &self.views[&view];
        let (width, height) = size;
        let (chroma_width, chroma_height) = (width.div_ceil(2), height.div_ceil(2));

        write_plane(
            queue,
            &textures.y,
            frame.plane(0),
            frame.stride(0),
            width,
            height,
        );
        write_plane(
            queue,
            &textures.uv,
            frame.plane(1),
            frame.stride(1),
            chroma_width,
            chroma_height,
        );
        queue.write_buffer(
            &textures.uniforms,
            0,
            bytemuck_cast(&color_matrix(frame.colorimetry())),
        );
    }

    fn create_textures(&self, device: &wgpu::Device, (width, height): (u32, u32)) -> ViewTextures {
        let texture = |label, width, height, format| {
            device.create_texture(&wgpu::TextureDescriptor {
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
            })
        };
        let y = texture("video y", width, height, wgpu::TextureFormat::R8Unorm);
        let uv = texture(
            "video uv",
            width.div_ceil(2),
            height.div_ceil(2),
            wgpu::TextureFormat::Rg8Unorm,
        );
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("video colour matrix"),
            size: std::mem::size_of::<[f32; 16]>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("video"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(
                        &y.create_view(&Default::default()),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(
                        &uv.create_view(&Default::default()),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: uniforms.as_entire_binding(),
                },
            ],
        });
        ViewTextures {
            size: (width, height),
            y,
            uv,
            uniforms,
            bind_group,
        }
    }
}

fn write_plane(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    data: &[u8],
    stride: u32,
    width: u32,
    height: u32,
) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(stride),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
}

fn bytemuck_cast(matrix: &[f32; 16]) -> &[u8] {
    // SAFETY: f32 has no padding or invalid bit patterns; the slice covers exactly the array.
    unsafe { std::slice::from_raw_parts(matrix.as_ptr().cast(), std::mem::size_of_val(matrix)) }
}

/// Column-major 4×4 matrix mapping `(Y, Cb, Cr, 1)` with samples normalised to 0–1
/// to gamma-encoded `(R, G, B, 1)`.
pub fn color_matrix(colorimetry: YuvColorimetry) -> [f32; 16] {
    let (kr, kb) = match colorimetry.matrix {
        YuvMatrix::Bt601 => (0.299, 0.114),
        YuvMatrix::Bt709 => (0.2126, 0.0722),
        YuvMatrix::Bt2020 => (0.2627, 0.0593),
    };
    let kg = 1.0 - kr - kb;

    // Y' = ys·Y + yo, C' = cs·C + co (C' centred on 0).
    let (ys, yo, cs, co): (f32, f32, f32, f32) = if colorimetry.full_range {
        (1.0, 0.0, 1.0, -128.0 / 255.0)
    } else {
        (255.0 / 219.0, -16.0 / 219.0, 255.0 / 224.0, -128.0 / 224.0)
    };

    let rv = 2.0 * (1.0 - kr);
    let gu = -2.0 * kb * (1.0 - kb) / kg;
    let gv = -2.0 * kr * (1.0 - kr) / kg;
    let bu = 2.0 * (1.0 - kb);

    [
        // column 0: Y
        ys,
        ys,
        ys,
        0.0,
        // column 1: Cb
        0.0,
        gu * cs,
        bu * cs,
        0.0,
        // column 2: Cr
        rv * cs,
        gv * cs,
        0.0,
        0.0,
        // column 3: offsets
        yo + rv * co,
        yo + (gu + gv) * co,
        yo + bu * co,
        1.0,
    ]
}

/// Paint callback drawing one video view into its rectangle.
pub struct VideoPaintCallback {
    pub view: ViewId,
    /// A new frame to upload before painting; `None` repaints the last frame.
    pub upload: Option<Arc<dyn Nv12Frame>>,
}

impl VideoPaintCallback {
    pub fn shape(
        rect: egui::Rect,
        view: ViewId,
        upload: Option<Arc<dyn Nv12Frame>>,
    ) -> egui::Shape {
        egui_wgpu::Callback::new_paint_callback(rect, VideoPaintCallback { view, upload }).into()
    }
}

impl CallbackTrait for VideoPaintCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if let (Some(frame), Some(renderer)) = (&self.upload, resources.get_mut::<VideoRenderer>())
        {
            renderer.upload(device, queue, self.view, frame.as_ref());
        }
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        resources: &CallbackResources,
    ) {
        let Some(renderer) = resources.get::<VideoRenderer>() else {
            return;
        };
        let Some(textures) = renderer.views.get(&self.view) else {
            return;
        };
        render_pass.set_pipeline(&renderer.pipeline);
        render_pass.set_bind_group(0, &textures.bind_group, &[]);
        render_pass.draw(0..4, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(m: &[f32; 16], y: f32, cb: f32, cr: f32) -> [f32; 3] {
        let v = [y, cb, cr, 1.0];
        let mut out = [0.0; 3];
        for (row, o) in out.iter_mut().enumerate() {
            *o = (0..4).map(|col| m[col * 4 + row] * v[col]).sum();
        }
        out
    }

    fn assert_rgb(actual: [f32; 3], expected: [f32; 3]) {
        for (a, e) in actual.iter().zip(expected) {
            // Tolerance covers the rounding of the 8-bit test values.
            assert!((a - e).abs() < 0.01, "{actual:?} != {expected:?}");
        }
    }

    #[test]
    fn limited_range_black_and_white() {
        for matrix in [YuvMatrix::Bt601, YuvMatrix::Bt709, YuvMatrix::Bt2020] {
            let m = color_matrix(YuvColorimetry {
                matrix,
                full_range: false,
            });
            assert_rgb(apply(&m, 16.0 / 255.0, 0.5019608, 0.5019608), [0.0; 3]);
            assert_rgb(apply(&m, 235.0 / 255.0, 0.5019608, 0.5019608), [1.0; 3]);
        }
    }

    #[test]
    fn full_range_bt601_red() {
        // JPEG (full range BT.601) encoding of pure red: Y 76, Cb 85, Cr 255.
        let m = color_matrix(YuvColorimetry {
            matrix: YuvMatrix::Bt601,
            full_range: true,
        });
        assert_rgb(
            apply(&m, 76.0 / 255.0, 85.0 / 255.0, 255.0 / 255.0),
            [1.0, 0.0, 0.0],
        );
    }

    #[test]
    fn limited_range_bt709_green() {
        // BT.709 limited range green: Y 173, Cb 42, Cr 26.
        let m = color_matrix(YuvColorimetry {
            matrix: YuvMatrix::Bt709,
            full_range: false,
        });
        assert_rgb(
            apply(&m, 173.0 / 255.0, 42.0 / 255.0, 26.0 / 255.0),
            [0.0, 1.0, 0.0],
        );
    }
}
