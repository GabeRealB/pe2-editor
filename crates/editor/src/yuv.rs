//! YCbCr 4:2:0 frames drawn by the GPU: their planes uploaded as they come and turned into RGB by a shader, as the
//! decoder does (BT.601, the chroma offset by 128) - no conversion on the CPU's side, and 1.5 bytes a pixel uploaded.
//! The pipeline is made when the window is (`install`), for its target's format; a frame is drawn by a paint callback
//! (`Draw`), its planes uploaded once a frame they change.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use eframe::egui;
use eframe::egui_wgpu::{self, wgpu};
use pe2_ext::media::VideoFrame;

const SHADER: &str = r"
@group(0) @binding(0) var y_plane: texture_2d<f32>;
@group(0) @binding(1) var cb_plane: texture_2d<f32>;
@group(0) @binding(2) var cr_plane: texture_2d<f32>;
@group(0) @binding(3) var planes: sampler;

struct Out {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// One triangle covering the viewport: the callback's rectangle.
@vertex fn vs(@builtin(vertex_index) i: u32) -> Out {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var out: Out;
    out.position = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    out.uv = uv;
    return out;
}

fn rgb(uv: vec2<f32>) -> vec3<f32> {
    let y = textureSample(y_plane, planes, uv).r * 255.0;
    let cb = textureSample(cb_plane, planes, uv).r * 255.0 - 128.0;
    let cr = textureSample(cr_plane, planes, uv).r * 255.0 - 128.0;
    let rgb = vec3<f32>(y + 1.402 * cr, y - 0.3437 * cb - 0.7143 * cr, y + 1.772 * cb) / 255.0;
    return clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0));
}

// A target in gamma space takes the colors as they are; an sRGB one, linear.
@fragment fn fs_gamma(in: Out) -> @location(0) vec4<f32> {
    return vec4<f32>(rgb(in.uv), 1.0);
}

@fragment fn fs_linear(in: Out) -> @location(0) vec4<f32> {
    let c = rgb(in.uv);
    let linear = select(pow((c + 0.055) / 1.055, vec3<f32>(2.4)), c / 12.92, c < vec3<f32>(0.04045));
    return vec4<f32>(linear, 1.0);
}
";

/// What draws frames, kept in the renderer's callback resources: the pipeline, and the frame's planes as uploaded last.
pub struct Renderer {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    /// Nearest, then linear.
    samplers: [wgpu::Sampler; 2],
    /// By player: each shows a frame of its own.
    uploaded: HashMap<u64, Uploaded>,
}

/// Players gone, whose planes are let go at the next upload.
static GONE: Mutex<Vec<u64>> = Mutex::new(Vec::new());

/// A player is gone: its planes are not needed.
pub fn forget(player: u64) {
    GONE.lock().unwrap_or_else(PoisonError::into_inner).push(player);
}

/// A frame's planes on the GPU: which frame, their size, and the bind group drawing them.
struct Uploaded {
    key: u64,
    size: (u32, u32),
    planes: [wgpu::Texture; 3],
    /// By sampler.
    binds: [wgpu::BindGroup; 2],
}

/// Make the pipeline for the window's renderer.
pub fn install(state: &egui_wgpu::RenderState) {
    let device = &state.device;
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("yuv420p"),
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });
    let plane = |binding| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    };
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("yuv420p"),
        entries: &[
            plane(0),
            plane(1),
            plane(2),
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("yuv420p"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let fragment = if state.target_format.is_srgb() { "fs_linear" } else { "fs_gamma" };
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("yuv420p"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some(fragment),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: state.target_format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    });
    let samplers = [wgpu::FilterMode::Nearest, wgpu::FilterMode::Linear].map(|filter| {
        device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("yuv420p"),
            mag_filter: filter,
            min_filter: filter,
            ..wgpu::SamplerDescriptor::default()
        })
    });
    state.renderer.write().callback_resources.insert(Renderer { pipeline, layout, samplers, uploaded: HashMap::new() });
}

/// A YCbCr 4:2:0 frame to draw where the callback is, known by a key: one with the key drawn last is not uploaded
/// again.
pub struct Draw {
    pub frame: VideoFrame,
    pub key: u64,
    pub player: u64,
    /// Blended between its pixels, rather than each kept as it is.
    pub smooth: bool,
}

impl egui_wgpu::CallbackTrait for Draw {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _: &egui_wgpu::ScreenDescriptor,
        _: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(renderer) = resources.get_mut::<Renderer>() else { return Vec::new() };
        for gone in GONE.lock().unwrap_or_else(PoisonError::into_inner).drain(..) {
            renderer.uploaded.remove(&gone);
        }
        if renderer.uploaded.get(&self.player).is_some_and(|u| u.key == self.key) {
            return Vec::new();
        }
        let (width, height) = (self.frame.width, self.frame.height);
        let (half_w, half_h) = (width.div_ceil(2), height.div_ceil(2));
        let sizes = [(width, height), (half_w, half_h), (half_w, half_h)];
        // Planes made again only where the size changed.
        if renderer.uploaded.get(&self.player).is_none_or(|u| u.size != (width, height)) {
            let planes = sizes.map(|(w, h)| {
                device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("yuv420p plane"),
                    size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::R8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                })
            });
            let views = planes.each_ref().map(|p| p.create_view(&wgpu::TextureViewDescriptor::default()));
            let binds = [0, 1].map(|sampler| device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("yuv420p"),
                layout: &renderer.layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&views[0]) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&views[1]) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&views[2]) },
                    wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&renderer.samplers[sampler]) },
                ],
            }));
            renderer.uploaded.insert(self.player, Uploaded { key: self.key, size: (width, height), planes, binds });
        }
        let Some(uploaded) = renderer.uploaded.get_mut(&self.player) else { return Vec::new() };
        let bytes = self.frame.buffer.bytes();
        let mut at = 0usize;
        for (plane, &(w, h)) in uploaded.planes.iter().zip(&sizes) {
            let len = (w * h) as usize;
            let Some(data) = bytes.get(at..at + len) else { break };
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: plane,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                data,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w), rows_per_image: Some(h) },
                wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            );
            at += len;
        }
        uploaded.key = self.key;
        Vec::new()
    }

    fn paint(
        &self,
        _: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        let Some(renderer) = resources.get::<Renderer>() else { return };
        let Some(uploaded) = renderer.uploaded.get(&self.player) else { return };
        pass.set_pipeline(&renderer.pipeline);
        pass.set_bind_group(0, &uploaded.binds[usize::from(self.smooth)], &[]);
        pass.draw(0..3, 0..1);
    }
}

