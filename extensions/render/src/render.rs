//! Drawing a scene on the editor's device, to a texture of the size the view is given: the game's faces lit, what is drawn in the world in lines, and what is flat over all of it.

use std::rc::Rc;

use pe2_ext::guest::bindings::pe2::extension::gpu;
use pe2_ext::guest::bindings::wasi::webgpu::webgpu as wg;

use crate::gizmo;
use crate::model::Blend;
use crate::scene::{self, Run, Triangles};

const SHADER: &str = r"
struct Frame {
    camera: mat4x4<f32>,
    light: vec4<f32>,
};
@group(0) @binding(0) var<uniform> frame: Frame;
@group(1) @binding(0) var atlas: texture_2d<f32>;
@group(1) @binding(1) var nearest: sampler;

struct In {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec3<f32>,
};

struct Out {
    @builtin(position) position: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) color: vec3<f32>,
    @location(2) world: vec3<f32>,
};

@vertex fn world(in: In) -> Out {
    var out: Out;
    out.position = frame.camera * vec4(in.position, 1.0);
    out.normal = in.normal;
    out.color = in.color;
    out.world = in.position;
    return out;
}

// The floor, drawn by the pixel rather than in lines: as sharp at any distance, and gone before it reaches the eye or the horizon. Its step is the light's fourth.
@fragment fn floor_lines(in: Out) -> @location(0) vec4<f32> {
    let step = frame.light.w;
    let at = in.world.xz / step;
    let width = fwidth(at);
    let near_line = abs(fract(at - 0.5) - 0.5) / width;
    let fifth = abs(fract(at / 5.0 - 0.5) - 0.5) * 5.0 / width;
    let minor = 1.0 - min(min(near_line.x, near_line.y), 1.0);
    let major = 1.0 - min(min(fifth.x, fifth.y), 1.0);
    var color = vec3(0.62, 0.65, 0.72);
    var alpha = 0.16 * minor + 0.24 * major;
    let axes = abs(in.world.xz) / fwidth(in.world.xz);
    if (axes.x < 1.2) {
        color = vec3(0.26, 0.55, 0.93);
        alpha = 0.8;
    } else if (axes.y < 1.2) {
        color = vec3(0.91, 0.27, 0.32);
        alpha = 0.8;
    }
    let reach = length(in.world.xz) / (step * 24.0);
    return vec4(color, alpha * (1.0 - smoothstep(0.35, 1.0, reach)));
}

struct FaceIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec3<f32>,
    @location(3) place: vec3<f32>,
};

struct FaceOut {
    @builtin(position) position: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) color: vec3<f32>,
    @location(2) place: vec3<f32>,
};

@vertex fn face(in: FaceIn) -> FaceOut {
    var out: FaceOut;
    out.position = frame.camera * vec4(in.position, 1.0);
    out.normal = in.normal;
    out.color = in.color;
    out.place = in.place;
    return out;
}

// Studio light: from the eye and a little from above, either side of a face, as what the game winds the other way shows no back. A picture's texels are as the disc has them: the game's clear ones stay clear.
fn shaded(in: FaceOut, texel: vec4<f32>) -> vec4<f32> {
    let facing = abs(dot(normalize(in.normal), frame.light.xyz));
    let above = 0.5 + 0.5 * in.normal.y;
    let base = mix(in.color, texel.rgb * in.color, in.place.z);
    return vec4(base * (0.28 + 0.62 * facing + 0.14 * above), 1.0);
}

// A texel's bit: half alpha, but for black, where no other black is drawn at all.
fn semi(texel: vec4<f32>) -> bool {
    return texel.a < 0.75 || all(texel.rgb == vec3(0.0));
}

@fragment fn lit(in: FaceOut) -> @location(0) vec4<f32> {
    let texel = textureSampleLevel(atlas, nearest, in.place.xy, 0.0);
    if (in.place.z > 0.5 && texel.a < 0.5) {
        discard;
    }
    return shaded(in, texel);
}

// What of a transparent face is solid: only a texel with its bit set blends.
@fragment fn lit_solid(in: FaceOut) -> @location(0) vec4<f32> {
    let texel = textureSampleLevel(atlas, nearest, in.place.xy, 0.0);
    if (in.place.z > 0.5 && (texel.a < 0.5 || semi(texel))) {
        discard;
    }
    return shaded(in, texel);
}

@fragment fn lit_semi(in: FaceOut) -> @location(0) vec4<f32> {
    let texel = textureSampleLevel(atlas, nearest, in.place.xy, 0.0);
    if (in.place.z > 0.5 && (texel.a < 0.5 || !semi(texel))) {
        discard;
    }
    return shaded(in, texel);
}

@fragment fn flat_color(in: Out) -> @location(0) vec4<f32> {
    return vec4(in.color, 1.0);
}

struct FlatIn {
    @location(0) position: vec2<f32>,
    @location(1) color: vec4<f32>,
};

struct FlatOut {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex fn screen(in: FlatIn) -> FlatOut {
    var out: FlatOut;
    out.position = vec4(in.position, 0.0, 1.0);
    out.color = in.color;
    return out;
}

@fragment fn over(in: FlatOut) -> @location(0) vec4<f32> {
    return in.color;
}
";

const COLOR: wg::GpuTextureFormat = wg::GpuTextureFormat::Rgba8unorm;
const DEPTH: wg::GpuTextureFormat = wg::GpuTextureFormat::Depth32float;
const SAMPLES: u32 = 4;
fn draw_all(pass: &wg::GpuRenderPassEncoder, pipeline: &wg::GpuRenderPipeline, geometry: &Geometry, stride: u64) {
    if let Some(buffer) = geometry.buffer.as_ref().filter(|_| geometry.used > 0) {
        pass.set_pipeline(pipeline);
        pass.set_vertex_buffer(0, Some(buffer), None, Some(geometry.used));
        pass.draw(u32::try_from(geometry.used / stride).unwrap_or(0), None, None, None);
    }
}

const CLEAR: wg::GpuColor = wg::GpuColor { r: 0.105, g: 0.118, b: 0.14, a: 1.0 };
/// The camera, and the light's direction with a word to spare.
const FRAME_SIZE: u64 = 80;

/// A buffer that grows.
struct Geometry {
    usage: wg::GpuBufferUsage,
    buffer: Option<wg::GpuBuffer>,
    capacity: u64,
    used: u64,
}

impl Geometry {
    fn new() -> Geometry {
        Geometry { usage: wg::GpuBufferUsage::VERTEX | wg::GpuBufferUsage::COPY_DST, buffer: None, capacity: 0, used: 0 }
    }

    fn write(&mut self, device: &wg::GpuDevice, queue: &wg::GpuQueue, bytes: &[u8]) {
        self.used = bytes.len() as u64;
        if bytes.is_empty() {
            return;
        }
        if self.capacity < self.used {
            self.capacity = self.used.next_power_of_two();
            self.buffer = Some(device.create_buffer(&wg::GpuBufferDescriptor { size: self.capacity, usage: self.usage, mapped_at_creation: None, label: None }));
        }
        if let Some(buffer) = &self.buffer {
            let _ = queue.write_buffer_with_copy(buffer, 0, bytes, None, None);
        }
    }
}

/// What is drawn to, at a size.
struct Target {
    id: u64,
    color: wg::GpuTextureView,
    samples: wg::GpuTextureView,
    depth: wg::GpuTextureView,
    size: (u32, u32),
    _textures: [wg::GpuTexture; 3],
}

impl Drop for Target {
    fn drop(&mut self) {
        gpu::unpublish(self.id);
    }
}

fn texture(device: &wg::GpuDevice, size: (u32, u32), format: wg::GpuTextureFormat, samples: u32, usage: wg::GpuTextureUsage) -> wg::GpuTexture {
    device.create_texture(&wg::GpuTextureDescriptor {
        size: wg::GpuExtent3D { width: size.0, height: Some(size.1), depth_or_array_layers: None },
        mip_level_count: None,
        sample_count: Some(samples),
        dimension: None,
        format,
        usage,
        view_formats: None,
        texture_binding_view_dimension: None,
        label: None,
    })
}

/// What a frame is made of.
pub struct Drawn {
    pub camera: Vec<u8>,
    pub light: [f32; 3],
    /// The floor's step, in world units.
    pub step: f32,
}

/// What no view changes and every view draws with: made once for the instance, as the views live in one, and kept while
/// no view is open (`register`), so that a view opening draws with what is compiled already.
struct Shared {
    device: wg::GpuDevice,
    queue: wg::GpuQueue,
    solid: wg::GpuRenderPipeline,
    solid_parts: wg::GpuRenderPipeline,
    /// In `Blend`'s order.
    blended: [wg::GpuRenderPipeline; 4],
    floor: wg::GpuRenderPipeline,
    lines: wg::GpuRenderPipeline,
    flat: wg::GpuRenderPipeline,
    bindings: wg::GpuBindGroupLayout,
    pictures: wg::GpuBindGroupLayout,
    sampler: wg::GpuSampler,
    /// What shows until a scene says: one white texel.
    blank: wg::GpuTexture,
}

/// `Shared`, held in a `static`: a `thread_local!` here was found empty again at each call, the instance being the same.
struct Held(Option<Rc<Shared>>);

// SAFETY: a component instance runs on one thread, and its handles mean nothing outside it.
unsafe impl Send for Held {}

static SHARED: std::sync::Mutex<Held> = std::sync::Mutex::new(Held(None));

impl Shared {
    /// None where nothing draws.
    fn get() -> Option<Rc<Shared>> {
        let mut held = SHARED.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if held.0.is_none() {
            held.0 = Shared::new().map(Rc::new);
        }
        held.0.clone()
    }

    fn new() -> Option<Shared> {
        let device = gpu::device()?;
        let queue = device.queue();
        let entry = wg::GpuBindGroupLayoutEntry {
            binding: 0,
            visibility: wg::GpuShaderStage::VERTEX | wg::GpuShaderStage::FRAGMENT,
            buffer: Some(wg::GpuBufferBindingLayout { type_: None, has_dynamic_offset: None, min_binding_size: None }),
            sampler: None,
            texture: None,
            storage_texture: None,
        };
        let bindings = device.create_bind_group_layout(&wg::GpuBindGroupLayoutDescriptor { entries: vec![entry], label: None });
        let visible = wg::GpuShaderStage::FRAGMENT;
        let picture_entries = vec![
            wg::GpuBindGroupLayoutEntry { binding: 0, visibility: visible, buffer: None, sampler: None, texture: Some(wg::GpuTextureBindingLayout { sample_type: None, view_dimension: None, multisampled: None }), storage_texture: None },
            wg::GpuBindGroupLayoutEntry { binding: 1, visibility: visible, buffer: None, sampler: Some(wg::GpuSamplerBindingLayout { type_: None }), texture: None, storage_texture: None },
        ];
        let pictures = device.create_bind_group_layout(&wg::GpuBindGroupLayoutDescriptor { entries: picture_entries, label: None });
        let layout = device.create_pipeline_layout(&wg::GpuPipelineLayoutDescriptor { bind_group_layouts: vec![Some(&bindings), Some(&pictures)], immediate_size: None, label: None });
        let module = device.create_shader_module(&wg::GpuShaderModuleDescriptor { code: SHADER.to_string(), compilation_hints: None, label: None });
        let (solid, lines) = (world_pipeline(&device, &layout, &module, true), world_pipeline(&device, &layout, &module, false));
        let (floor, flat) = (floor_pipeline(&device, &layout, &module), flat_pipeline(&device, &module));
        // Nearest, as the console samples, and nothing to show until a scene says.
        let sampler = device.create_sampler(None);
        let blank = upload(&device, &queue, (1, 1), &[255; 4]);
        let blended = [Blend::Mix, Blend::Add, Blend::Subtract, Blend::AddQuarter].map(|blend| face_pipeline(&device, &layout, &module, Some(blend)));
        let solid_parts = face_pipeline(&device, &layout, &module, None);
        Some(Shared { device, queue, solid, solid_parts, blended, floor, lines, flat, bindings, pictures, sampler, blank })
    }
}

pub struct Renderer {
    shared: Rc<Shared>,
    vertices: [u32; 2],
    blended_faces: Geometry,
    runs: Vec<Run>,
    frame: wg::GpuBuffer,
    group: wg::GpuBindGroup,
    atlas: wg::GpuBindGroup,
    /// What `atlas` reads, where a scene gave one.
    atlas_texture: Option<wg::GpuTexture>,
    target: Option<Target>,
    triangles: Geometry,
    floor_quad: Geometry,
    world_lines: Geometry,
    overlay: Geometry,
}

fn multisample() -> wg::GpuMultisampleState {
    wg::GpuMultisampleState { count: Some(SAMPLES), mask: None, alpha_to_coverage_enabled: None }
}

fn world_attributes() -> Vec<Option<wg::GpuVertexBufferLayout>> {
    let attribute = |offset, shader_location| wg::GpuVertexAttribute { format: wg::GpuVertexFormat::Float32x3, offset, shader_location };
    vec![Some(wg::GpuVertexBufferLayout { array_stride: scene::VERTEX_SIZE, attributes: vec![attribute(0, 0), attribute(12, 1), attribute(24, 2)], step_mode: None })]
}

fn depth(compare: wg::GpuCompareFunction, write: bool, bias: Option<i32>) -> wg::GpuDepthStencilState {
    wg::GpuDepthStencilState {
        format: DEPTH,
        depth_write_enabled: Some(write),
        depth_compare: Some(compare),
        stencil_front: None,
        stencil_back: None,
        stencil_read_mask: None,
        stencil_write_mask: None,
        depth_bias: bias,
        depth_bias_slope_scale: bias.map(|_| 1.0),
        depth_bias_clamp: None,
    }
}

fn face_attributes() -> Vec<Option<wg::GpuVertexBufferLayout>> {
    let attribute = |offset, shader_location| wg::GpuVertexAttribute { format: wg::GpuVertexFormat::Float32x3, offset, shader_location };
    vec![Some(wg::GpuVertexBufferLayout { array_stride: scene::TRIANGLE_SIZE, attributes: vec![attribute(0, 0), attribute(12, 1), attribute(24, 2), attribute(36, 3)], step_mode: None })]
}

/// What each of the GPU's blends does with what is behind (B) and the face (F); a constant is `share`'s.
fn blend_of(blend: Blend) -> wg::GpuBlendState {
    use wg::GpuBlendFactor::{Constant, One, OneMinusConstant};
    use wg::GpuBlendOperation::{Add, ReverseSubtract};
    let part = |operation, src_factor, dst_factor| wg::GpuBlendComponent { operation: Some(operation), src_factor: Some(src_factor), dst_factor: Some(dst_factor) };
    let component = match blend {
        Blend::Mix => part(Add, Constant, OneMinusConstant),
        Blend::Add => part(Add, One, One),
        Blend::Subtract => part(ReverseSubtract, One, One),
        Blend::AddQuarter => part(Add, Constant, One),
    };
    wg::GpuBlendState { color: component, alpha: component }
}

/// What of the face a blend's constant takes: B/2 + F/2, B + F/4.
fn share(blend: Blend) -> f64 {
    if blend == Blend::AddQuarter { 0.25 } else { 0.5 }
}

/// Faces beyond the opaque: at the depth of what they go over, a layer being at its base's very depth.
fn face_pipeline(device: &wg::GpuDevice, layout: &wg::GpuPipelineLayout, module: &wg::GpuShaderModule, blend: Option<Blend>) -> wg::GpuRenderPipeline {
    let target = wg::GpuColorTargetState { format: COLOR, blend: blend.map(blend_of), write_mask: None };
    device.create_render_pipeline(wg::GpuRenderPipelineDescriptor {
        vertex: wg::GpuVertexState { buffers: Some(face_attributes()), module, entry_point: Some("face".to_string()), constants: None },
        primitive: Some(wg::GpuPrimitiveState { topology: Some(wg::GpuPrimitiveTopology::TriangleList), strip_index_format: None, front_face: None, cull_mode: None, unclipped_depth: None }),
        depth_stencil: Some(depth(wg::GpuCompareFunction::LessEqual, blend.is_none(), Some(2))),
        multisample: Some(multisample()),
        fragment: Some(wg::GpuFragmentState { targets: vec![Some(target)], module, entry_point: Some(if blend.is_some() { "lit_semi" } else { "lit_solid" }.to_string()), constants: None }),
        layout: wg::GpuLayoutMode::Specific(layout),
        label: Some("render faces".to_string()),
    })
}

fn world_pipeline(device: &wg::GpuDevice, layout: &wg::GpuPipelineLayout, module: &wg::GpuShaderModule, lit: bool) -> wg::GpuRenderPipeline {
    let topology = if lit { wg::GpuPrimitiveTopology::TriangleList } else { wg::GpuPrimitiveTopology::LineList };
    let target = wg::GpuColorTargetState { format: COLOR, blend: None, write_mask: None };
    device.create_render_pipeline(wg::GpuRenderPipelineDescriptor {
        vertex: wg::GpuVertexState { buffers: Some(if lit { face_attributes() } else { world_attributes() }), module, entry_point: Some(if lit { "face" } else { "world" }.to_string()), constants: None },
        primitive: Some(wg::GpuPrimitiveState { topology: Some(topology), strip_index_format: None, front_face: None, cull_mode: None, unclipped_depth: None }),
        // Faces a little back, so that lines on them are seen.
        depth_stencil: Some(if lit { depth(wg::GpuCompareFunction::Less, true, Some(2)) } else { depth(wg::GpuCompareFunction::LessEqual, true, None) }),
        multisample: Some(multisample()),
        fragment: Some(wg::GpuFragmentState { targets: vec![Some(target)], module, entry_point: Some(if lit { "lit" } else { "flat_color" }.to_string()), constants: None }),
        layout: wg::GpuLayoutMode::Specific(layout),
        label: Some("render world".to_string()),
    })
}

fn over_blend() -> wg::GpuBlendState {
    let over = wg::GpuBlendComponent { operation: None, src_factor: Some(wg::GpuBlendFactor::SrcAlpha), dst_factor: Some(wg::GpuBlendFactor::OneMinusSrcAlpha) };
    wg::GpuBlendState { color: over, alpha: over }
}

fn floor_pipeline(device: &wg::GpuDevice, layout: &wg::GpuPipelineLayout, module: &wg::GpuShaderModule) -> wg::GpuRenderPipeline {
    let target = wg::GpuColorTargetState { format: COLOR, blend: Some(over_blend()), write_mask: None };
    device.create_render_pipeline(wg::GpuRenderPipelineDescriptor {
        vertex: wg::GpuVertexState { buffers: Some(world_attributes()), module, entry_point: Some("world".to_string()), constants: None },
        primitive: None,
        depth_stencil: Some(depth(wg::GpuCompareFunction::LessEqual, false, None)),
        multisample: Some(multisample()),
        fragment: Some(wg::GpuFragmentState { targets: vec![Some(target)], module, entry_point: Some("floor_lines".to_string()), constants: None }),
        layout: wg::GpuLayoutMode::Specific(layout),
        label: Some("render floor".to_string()),
    })
}

fn flat_pipeline(device: &wg::GpuDevice, module: &wg::GpuShaderModule) -> wg::GpuRenderPipeline {
    let attributes = vec![
        wg::GpuVertexAttribute { format: wg::GpuVertexFormat::Float32x2, offset: 0, shader_location: 0 },
        wg::GpuVertexAttribute { format: wg::GpuVertexFormat::Float32x4, offset: 8, shader_location: 1 },
    ];
    let layout = vec![Some(wg::GpuVertexBufferLayout { array_stride: gizmo::VERTEX_SIZE, attributes, step_mode: None })];
    let target = wg::GpuColorTargetState { format: COLOR, blend: Some(over_blend()), write_mask: None };
    device.create_render_pipeline(wg::GpuRenderPipelineDescriptor {
        vertex: wg::GpuVertexState { buffers: Some(layout), module, entry_point: Some("screen".to_string()), constants: None },
        primitive: None,
        depth_stencil: Some(depth(wg::GpuCompareFunction::Always, false, None)),
        multisample: Some(multisample()),
        fragment: Some(wg::GpuFragmentState { targets: vec![Some(target)], module, entry_point: Some("over".to_string()), constants: None }),
        layout: wg::GpuLayoutMode::Auto,
        label: Some("render over".to_string()),
    })
}

fn upload(device: &wg::GpuDevice, queue: &wg::GpuQueue, size: (u32, u32), rgba: &[u8]) -> wg::GpuTexture {
    let made = texture(device, size, COLOR, 1, wg::GpuTextureUsage::TEXTURE_BINDING | wg::GpuTextureUsage::COPY_DST);
    let place = wg::GpuTexelCopyTextureInfo { texture: &made, mip_level: None, origin: None, aspect: None };
    let layout = wg::GpuTexelCopyBufferLayout { offset: None, bytes_per_row: Some(size.0 * 4), rows_per_image: Some(size.1) };
    let extent = wg::GpuExtent3D { width: size.0, height: Some(size.1), depth_or_array_layers: None };
    queue.write_texture_with_copy(&place, rgba, layout, extent);
    made
}

fn picture_group(device: &wg::GpuDevice, layout: &wg::GpuBindGroupLayout, sampler: &wg::GpuSampler, texture: &wg::GpuTexture) -> wg::GpuBindGroup {
    let view = texture.create_view(None);
    device.create_bind_group(&wg::GpuBindGroupDescriptor {
        layout,
        entries: vec![
            wg::GpuBindGroupEntry { binding: 0, resource: wg::GpuBindingResource::GpuTextureView(&view) },
            wg::GpuBindGroupEntry { binding: 1, resource: wg::GpuBindingResource::GpuSampler(sampler) },
        ],
        label: None,
    })
}

impl Renderer {
    /// None where nothing draws.
    pub fn new() -> Option<Renderer> {
        let shared = Shared::get()?;
        let device = &shared.device;
        let frame = device.create_buffer(&wg::GpuBufferDescriptor { size: FRAME_SIZE, usage: wg::GpuBufferUsage::UNIFORM | wg::GpuBufferUsage::COPY_DST, mapped_at_creation: None, label: None });
        let group = device.create_bind_group(&wg::GpuBindGroupDescriptor {
            layout: &shared.bindings,
            entries: vec![wg::GpuBindGroupEntry { binding: 0, resource: wg::GpuBindingResource::GpuBuffer(&frame) }],
            label: None,
        });
        let atlas = picture_group(device, &shared.pictures, &shared.sampler, &shared.blank);
        let geometry = Geometry::new;
        Some(Renderer { vertices: [0; 2], blended_faces: geometry(), runs: Vec::new(), frame, group, atlas, atlas_texture: None, target: None, triangles: geometry(), floor_quad: geometry(), world_lines: geometry(), overlay: geometry(), shared })
    }

    /// Make sure what is drawn to is this many pixels; what the window shows it by. Whether it changed.
    pub fn resize(&mut self, size: (u32, u32)) -> bool {
        if self.target.as_ref().is_some_and(|target| target.size == size) {
            return false;
        }
        let shown = texture(&self.shared.device, size, COLOR, 1, wg::GpuTextureUsage::RENDER_ATTACHMENT | wg::GpuTextureUsage::TEXTURE_BINDING);
        let samples = texture(&self.shared.device, size, COLOR, SAMPLES, wg::GpuTextureUsage::RENDER_ATTACHMENT);
        let depth = texture(&self.shared.device, size, DEPTH, SAMPLES, wg::GpuTextureUsage::RENDER_ATTACHMENT);
        self.target = Some(Target {
            id: gpu::publish(&shown),
            color: shown.create_view(None),
            samples: samples.create_view(None),
            depth: depth.create_view(None),
            size,
            _textures: [shown, samples, depth],
        });
        true
    }

    pub fn id(&self) -> u64 {
        self.target.as_ref().map_or(0, |target| target.id)
    }

    /// What faces with pictures read: RGBA, row by row.
    pub fn set_atlas(&mut self, size: (u32, u32), rgba: &[u8]) {
        let texture = upload(&self.shared.device, &self.shared.queue, size, rgba);
        self.atlas = picture_group(&self.shared.device, &self.shared.pictures, &self.shared.sampler, &texture);
        self.atlas_texture = Some(texture);
    }

    pub fn set_triangles(&mut self, triangles: Option<&Triangles>) {
        self.vertices = triangles.map_or([0; 2], |triangles| triangles.vertices);
        self.triangles.write(&self.shared.device, &self.shared.queue, triangles.map_or(&[], |triangles| &triangles.bytes));
    }

    /// What blends, in the order it is to be drawn.
    pub fn set_blended(&mut self, bytes: &[u8], runs: Vec<Run>) {
        self.blended_faces.write(&self.shared.device, &self.shared.queue, bytes);
        self.runs = runs;
    }

    pub fn set_floor(&mut self, bytes: &[u8]) {
        self.floor_quad.write(&self.shared.device, &self.shared.queue, bytes);
    }

    pub fn set_lines(&mut self, bytes: &[u8]) {
        self.world_lines.write(&self.shared.device, &self.shared.queue, bytes);
    }

    pub fn set_overlay(&mut self, bytes: &[u8]) {
        self.overlay.write(&self.shared.device, &self.shared.queue, bytes);
    }

    pub fn draw(&self, drawn: &Drawn) {
        let Some(target) = &self.target else { return };
        let mut uniform = drawn.camera.clone();
        for value in drawn.light.into_iter().chain([drawn.step]) {
            uniform.extend_from_slice(&value.to_le_bytes());
        }
        let _ = self.shared.queue.write_buffer_with_copy(&self.frame, 0, &uniform, None, None);
        let encoder = self.shared.device.create_command_encoder(None);
        let color = wg::GpuRenderPassColorAttachment {
            view: &target.samples,
            depth_slice: None,
            resolve_target: Some(&target.color),
            clear_value: Some(CLEAR),
            load_op: wg::GpuLoadOp::Clear,
            store_op: wg::GpuStoreOp::Discard,
        };
        let depth = wg::GpuRenderPassDepthStencilAttachment {
            view: &target.depth,
            depth_clear_value: Some(1.0),
            depth_load_op: Some(wg::GpuLoadOp::Clear),
            depth_store_op: Some(wg::GpuStoreOp::Discard),
            depth_read_only: None,
            stencil_clear_value: None,
            stencil_load_op: None,
            stencil_store_op: None,
            stencil_read_only: None,
        };
        let pass = encoder.begin_render_pass(&wg::GpuRenderPassDescriptor {
            color_attachments: vec![Some(color)],
            depth_stencil_attachment: Some(depth),
            occlusion_query_set: None,
            timestamp_writes: None,
            max_draw_count: None,
            label: None,
        });
        let _ = pass.set_bind_group(0, Some(&self.group), None, None, None);
        let _ = pass.set_bind_group(1, Some(&self.atlas), None, None, None);
        if let Some(buffer) = self.triangles.buffer.as_ref().filter(|_| self.triangles.used > 0) {
            let [opaque, solid_parts] = self.vertices;
            pass.set_vertex_buffer(0, Some(buffer), None, Some(self.triangles.used));
            pass.set_pipeline(&self.shared.solid);
            pass.draw(opaque, None, None, None);
            pass.set_pipeline(&self.shared.solid_parts);
            pass.draw(solid_parts, None, Some(opaque), None);
        }
        let layers = [(&self.shared.lines, &self.world_lines, scene::VERTEX_SIZE), (&self.shared.floor, &self.floor_quad, scene::VERTEX_SIZE)];
        for (pipeline, geometry, stride) in layers {
            draw_all(&pass, pipeline, geometry, stride);
        }
        // Over everything solid, and writing no depth: what is behind is what they mix with.
        if let Some(buffer) = self.blended_faces.buffer.as_ref().filter(|_| self.blended_faces.used > 0) {
            pass.set_vertex_buffer(0, Some(buffer), None, Some(self.blended_faces.used));
            for run in &self.runs {
                pass.set_pipeline(&self.shared.blended[run.blend as usize]);
                let share = share(run.blend);
                pass.set_blend_constant(wg::GpuColor { r: share, g: share, b: share, a: share });
                pass.draw(run.count, None, Some(run.first), None);
            }
        }
        draw_all(&pass, &self.shared.flat, &self.overlay, gizmo::VERTEX_SIZE);
        pass.end();
        let commands = encoder.finish(None);
        self.shared.queue.submit(&[&commands]);
    }
}
