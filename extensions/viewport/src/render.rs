//! A scene drawn to a texture of the editor's device, shown by the window as it is: indexed triangles, depth tested, through a camera.

use pe2_ext::guest::bindings::pe2::extension::gpu;
use pe2_ext::guest::bindings::wasi::webgpu::webgpu as wg;

const SHADER: &str = r"
@group(0) @binding(0) var<uniform> camera: mat4x4<f32>;

struct In {
    @location(0) position: vec3<f32>,
    @location(1) color: vec3<f32>,
};

struct Out {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec3<f32>,
};

@vertex fn vs(in: In) -> Out {
    var out: Out;
    out.position = camera * vec4(in.position, 1.0);
    out.color = in.color;
    return out;
}

@fragment fn fs(in: Out) -> @location(0) vec4<f32> {
    return vec4(in.color, 1.0);
}
";

const COLOR: wg::GpuTextureFormat = wg::GpuTextureFormat::Rgba8unorm;
const DEPTH: wg::GpuTextureFormat = wg::GpuTextureFormat::Depth32float;
/// Position and color, three floats each.
const STRIDE: u64 = 24;

/// A cube's corners, each colored by where it is.
fn vertices() -> Vec<u8> {
    let mut bytes = Vec::with_capacity(8 * 24);
    for corner in 0..8u8 {
        let at = |bit: u8| f32::from(corner >> bit & 1);
        for value in [at(0) * 2.0 - 1.0, at(1) * 2.0 - 1.0, at(2) * 2.0 - 1.0, at(0), at(1), at(2)] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
    }
    bytes
}

/// Its six faces, two triangles each.
fn indices() -> Vec<u8> {
    const FACES: [[u16; 4]; 6] = [[0, 1, 3, 2], [4, 6, 7, 5], [0, 4, 5, 1], [2, 3, 7, 6], [0, 2, 6, 4], [1, 5, 7, 3]];
    FACES.iter().flat_map(|[a, b, c, d]| [*a, *b, *c, *a, *c, *d]).flat_map(u16::to_le_bytes).collect()
}

fn multiply(a: &[[f32; 4]; 4], b: &[[f32; 4]; 4]) -> [[f32; 4]; 4] {
    std::array::from_fn(|column| std::array::from_fn(|row| (0..4).map(|k| a[k][row] * b[column][k]).sum()))
}

/// What takes the world to the screen: a turn around the vertical and a tilt, moved back, in perspective.
fn camera(yaw: f32, pitch: f32, distance: f32) -> Vec<u8> {
    let (sy, cy) = yaw.sin_cos();
    let (sp, cp) = pitch.sin_cos();
    let turn = [[cy, 0.0, -sy, 0.0], [0.0, 1.0, 0.0, 0.0], [sy, 0.0, cy, 0.0], [0.0, 0.0, 0.0, 1.0]];
    let tilt = [[1.0, 0.0, 0.0, 0.0], [0.0, cp, sp, 0.0], [0.0, -sp, cp, 0.0], [0.0, 0.0, 0.0, 1.0]];
    let back = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, -distance, 1.0]];
    let (near, far, focal) = (0.1_f32, 100.0_f32, 2.0_f32);
    let perspective = [
        [focal, 0.0, 0.0, 0.0],
        [0.0, focal, 0.0, 0.0],
        [0.0, 0.0, far / (near - far), -1.0],
        [0.0, 0.0, near * far / (near - far), 0.0],
    ];
    let view = multiply(&back, &multiply(&tilt, &turn));
    multiply(&perspective, &view).iter().flatten().flat_map(|value| value.to_le_bytes()).collect()
}

fn buffer(device: &wg::GpuDevice, queue: &wg::GpuQueue, usage: wg::GpuBufferUsage, bytes: &[u8]) -> wg::GpuBuffer {
    let made = device.create_buffer(&wg::GpuBufferDescriptor {
        size: bytes.len() as u64,
        usage: usage | wg::GpuBufferUsage::COPY_DST,
        mapped_at_creation: None,
        label: None,
    });
    queue.write_buffer_with_copy(&made, 0, bytes, None, None).ok();
    made
}

pub struct Scene {
    device: wg::GpuDevice,
    queue: wg::GpuQueue,
    pipeline: wg::GpuRenderPipeline,
    color: wg::GpuTextureView,
    depth: wg::GpuTextureView,
    vertices: wg::GpuBuffer,
    indices: wg::GpuBuffer,
    index_count: u32,
    group: wg::GpuBindGroup,
    id: u64,
    _textures: [wg::GpuTexture; 2],
}

fn target(device: &wg::GpuDevice, side: u32, format: wg::GpuTextureFormat, usage: wg::GpuTextureUsage) -> wg::GpuTexture {
    device.create_texture(&wg::GpuTextureDescriptor {
        size: wg::GpuExtent3D { width: side, height: Some(side), depth_or_array_layers: None },
        mip_level_count: None,
        sample_count: None,
        dimension: None,
        format,
        usage,
        view_formats: None,
        texture_binding_view_dimension: None,
        label: None,
    })
}

fn pipeline(device: &wg::GpuDevice, layout: &wg::GpuPipelineLayout) -> wg::GpuRenderPipeline {
    let module = device.create_shader_module(&wg::GpuShaderModuleDescriptor { code: SHADER.to_string(), compilation_hints: None, label: None });
    let attribute = |format, offset, shader_location| wg::GpuVertexAttribute { format, offset, shader_location };
    let attributes = vec![attribute(wg::GpuVertexFormat::Float32x3, 0, 0), attribute(wg::GpuVertexFormat::Float32x3, 12, 1)];
    let buffers = vec![Some(wg::GpuVertexBufferLayout { array_stride: STRIDE, attributes, step_mode: None })];
    let target = wg::GpuColorTargetState { format: COLOR, blend: None, write_mask: None };
    device.create_render_pipeline(wg::GpuRenderPipelineDescriptor {
        vertex: wg::GpuVertexState { buffers: Some(buffers), module: &module, entry_point: Some("vs".to_string()), constants: None },
        primitive: None,
        depth_stencil: Some(wg::GpuDepthStencilState {
            format: DEPTH,
            depth_write_enabled: Some(true),
            depth_compare: Some(wg::GpuCompareFunction::Less),
            stencil_front: None,
            stencil_back: None,
            stencil_read_mask: None,
            stencil_write_mask: None,
            depth_bias: None,
            depth_bias_slope_scale: None,
            depth_bias_clamp: None,
        }),
        multisample: None,
        fragment: Some(wg::GpuFragmentState { targets: vec![Some(target)], module: &module, entry_point: Some("fs".to_string()), constants: None }),
        layout: wg::GpuLayoutMode::Specific(layout),
        label: Some("viewport".to_string()),
    })
}

impl Scene {
    /// None where nothing draws.
    pub fn new(side: u32) -> Option<Scene> {
        let device = gpu::device()?;
        let queue = device.queue();
        let color = target(&device, side, COLOR, wg::GpuTextureUsage::RENDER_ATTACHMENT | wg::GpuTextureUsage::TEXTURE_BINDING);
        let depth = target(&device, side, DEPTH, wg::GpuTextureUsage::RENDER_ATTACHMENT);
        let camera = buffer(&device, &queue, wg::GpuBufferUsage::UNIFORM, &camera(0.6, 0.45, 6.0));
        let entry = wg::GpuBindGroupLayoutEntry {
            binding: 0,
            visibility: wg::GpuShaderStage::VERTEX,
            buffer: Some(wg::GpuBufferBindingLayout { type_: None, has_dynamic_offset: None, min_binding_size: None }),
            sampler: None,
            texture: None,
            storage_texture: None,
        };
        let bindings = device.create_bind_group_layout(&wg::GpuBindGroupLayoutDescriptor { entries: vec![entry], label: None });
        let layout = device.create_pipeline_layout(&wg::GpuPipelineLayoutDescriptor {
            bind_group_layouts: vec![Some(&bindings)],
            immediate_size: None,
            label: None,
        });
        let group = device.create_bind_group(&wg::GpuBindGroupDescriptor {
            layout: &bindings,
            entries: vec![wg::GpuBindGroupEntry { binding: 0, resource: wg::GpuBindingResource::GpuBuffer(&camera) }],
            label: None,
        });
        let pipeline = pipeline(&device, &layout);
        let (index_bytes, id) = (indices(), gpu::publish(&color));
        Some(Scene {
            pipeline,
            color: color.create_view(None),
            depth: depth.create_view(None),
            vertices: buffer(&device, &queue, wg::GpuBufferUsage::VERTEX, &vertices()),
            indices: buffer(&device, &queue, wg::GpuBufferUsage::INDEX, &index_bytes),
            index_count: u32::try_from(index_bytes.len() / 2).unwrap_or(0),
            group,
            id,
            _textures: [color, depth],
            device,
            queue,
        })
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn draw(&self) {
        let encoder = self.device.create_command_encoder(None);
        let color = wg::GpuRenderPassColorAttachment {
            view: &self.color,
            depth_slice: None,
            resolve_target: None,
            clear_value: Some(wg::GpuColor { r: 0.08, g: 0.09, b: 0.12, a: 1.0 }),
            load_op: wg::GpuLoadOp::Clear,
            store_op: wg::GpuStoreOp::Store,
        };
        let depth = wg::GpuRenderPassDepthStencilAttachment {
            view: &self.depth,
            depth_clear_value: Some(1.0),
            depth_load_op: Some(wg::GpuLoadOp::Clear),
            depth_store_op: Some(wg::GpuStoreOp::Store),
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
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, Some(&self.group), None, None, None).ok();
        pass.set_vertex_buffer(0, Some(&self.vertices), None, None);
        pass.set_index_buffer(&self.indices, wg::GpuIndexFormat::Uint16, None, None);
        pass.draw_indexed(self.index_count, None, None, None, None);
        pass.end();
        let commands = encoder.finish(None);
        self.queue.submit(&[&commands]);
    }
}
