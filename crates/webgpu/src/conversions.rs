//! From what crosses the contract to what wgpu takes. What an extension gives is never trusted: what cannot be converted is an error of its call, never a panic of the editor's.

use wasmtime::component::ResourceTable;

use crate::wasi::webgpu::webgpu;

fn or<T>(value: Option<T>, default: T) -> T {
    value.unwrap_or(default)
}

pub fn extent(size: webgpu::GpuExtent3D) -> wgpu::Extent3d {
    wgpu::Extent3d {
        width: size.width,
        height: or(size.height, 1),
        depth_or_array_layers: or(size.depth_or_array_layers, 1),
    }
}

pub fn origin(origin: Option<webgpu::GpuOrigin3D>) -> wgpu::Origin3d {
    origin.map_or(wgpu::Origin3d::ZERO, |o| wgpu::Origin3d { x: or(o.x, 0), y: or(o.y, 0), z: or(o.z, 0) })
}

/// A texture format, those the host has no answer for refused here, so that the plain conversion need not say.
pub fn format(format: webgpu::GpuTextureFormat) -> wasmtime::Result<wgpu::TextureFormat> {
    if format!("{format:?}").starts_with("Astc") {
        wasmtime::bail!("{format:?} is not supported yet");
    }
    Ok(format.into())
}

pub fn multisample(state: Option<webgpu::GpuMultisampleState>) -> wgpu::MultisampleState {
    state.map_or_else(wgpu::MultisampleState::default, |s| wgpu::MultisampleState {
        count: or(s.count, 1),
        mask: u64::from(or(s.mask, u32::MAX)),
        alpha_to_coverage_enabled: or(s.alpha_to_coverage_enabled, false),
    })
}

fn stencil_face(face: Option<webgpu::GpuStencilFaceState>) -> wgpu::StencilFaceState {
    face.map_or(wgpu::StencilFaceState::IGNORE, |f| wgpu::StencilFaceState {
        compare: f.compare.map_or(wgpu::CompareFunction::Always, Into::into),
        fail_op: f.fail_op.map_or(wgpu::StencilOperation::Keep, Into::into),
        depth_fail_op: f.depth_fail_op.map_or(wgpu::StencilOperation::Keep, Into::into),
        pass_op: f.pass_op.map_or(wgpu::StencilOperation::Keep, Into::into),
    })
}

pub fn depth_stencil(state: webgpu::GpuDepthStencilState) -> wasmtime::Result<wgpu::DepthStencilState> {
    Ok(wgpu::DepthStencilState {
        format: format(state.format)?,
        depth_write_enabled: state.depth_write_enabled,
        depth_compare: state.depth_compare.map(Into::into),
        stencil: wgpu::StencilState {
            front: stencil_face(state.stencil_front),
            back: stencil_face(state.stencil_back),
            read_mask: or(state.stencil_read_mask, u32::MAX),
            write_mask: or(state.stencil_write_mask, u32::MAX),
        },
        bias: wgpu::DepthBiasState {
            constant: or(state.depth_bias, 0),
            slope_scale: or(state.depth_bias_slope_scale, 0.0),
            clamp: or(state.depth_bias_clamp, 0.0),
        },
    })
}

pub fn primitive(state: Option<webgpu::GpuPrimitiveState>) -> wgpu::PrimitiveState {
    state.map_or_else(wgpu::PrimitiveState::default, |s| wgpu::PrimitiveState {
        topology: s.topology.map_or(wgpu::PrimitiveTopology::TriangleList, Into::into),
        strip_index_format: s.strip_index_format.map(Into::into),
        front_face: s.front_face.map_or(wgpu::FrontFace::Ccw, Into::into),
        cull_mode: s.cull_mode.and_then(Into::into),
        unclipped_depth: or(s.unclipped_depth, false),
        polygon_mode: wgpu::PolygonMode::Fill,
        conservative: false,
    })
}

fn blend_component(component: webgpu::GpuBlendComponent) -> wgpu::BlendComponent {
    wgpu::BlendComponent {
        operation: component.operation.map_or(wgpu::BlendOperation::Add, Into::into),
        src_factor: component.src_factor.map_or(wgpu::BlendFactor::One, Into::into),
        dst_factor: component.dst_factor.map_or(wgpu::BlendFactor::Zero, Into::into),
    }
}

pub fn color_target(target: webgpu::GpuColorTargetState) -> wasmtime::Result<wgpu::ColorTargetState> {
    Ok(wgpu::ColorTargetState {
        format: format(target.format)?,
        blend: target
            .blend
            .map(|blend| wgpu::BlendState { color: blend_component(blend.color), alpha: blend_component(blend.alpha) }),
        write_mask: target.write_mask.map_or(Ok(wgpu::ColorWrites::ALL), TryInto::try_into)?,
    })
}

pub fn vertex_attribute(attribute: &webgpu::GpuVertexAttribute) -> wgpu::VertexAttribute {
    wgpu::VertexAttribute {
        format: attribute.format.into(),
        offset: attribute.offset,
        shader_location: attribute.shader_location,
    }
}

fn binding_type(entry: webgpu::GpuBindGroupLayoutEntry) -> wasmtime::Result<wgpu::BindingType> {
    Ok(match (entry.buffer, entry.sampler, entry.texture, entry.storage_texture) {
        (Some(buffer), None, None, None) => wgpu::BindingType::Buffer {
            ty: buffer.type_.map_or(wgpu::BufferBindingType::Uniform, Into::into),
            has_dynamic_offset: or(buffer.has_dynamic_offset, false),
            min_binding_size: buffer.min_binding_size.and_then(wgpu::BufferSize::new),
        },
        (None, Some(sampler), None, None) => {
            wgpu::BindingType::Sampler(sampler.type_.map_or(wgpu::SamplerBindingType::Filtering, Into::into))
        }
        (None, None, Some(texture), None) => wgpu::BindingType::Texture {
            sample_type: texture.sample_type.map_or(wgpu::TextureSampleType::Float { filterable: true }, Into::into),
            view_dimension: texture.view_dimension.map_or(wgpu::TextureViewDimension::D2, Into::into),
            multisampled: or(texture.multisampled, false),
        },
        (None, None, None, Some(texture)) => wgpu::BindingType::StorageTexture {
            access: match texture.access {
                None | Some(webgpu::GpuStorageTextureAccess::WriteOnly) => wgpu::StorageTextureAccess::WriteOnly,
                Some(webgpu::GpuStorageTextureAccess::ReadOnly) => wgpu::StorageTextureAccess::ReadOnly,
                Some(webgpu::GpuStorageTextureAccess::ReadWrite) => wgpu::StorageTextureAccess::ReadWrite,
            },
            format: format(texture.format)?,
            view_dimension: texture.view_dimension.map_or(wgpu::TextureViewDimension::D2, Into::into),
        },
        _ => wasmtime::bail!("a binding is exactly one of a buffer, a sampler or a texture"),
    })
}

pub fn layout_entry(entry: webgpu::GpuBindGroupLayoutEntry) -> wasmtime::Result<wgpu::BindGroupLayoutEntry> {
    let (binding, visibility) = (entry.binding, entry.visibility.try_into()?);
    Ok(wgpu::BindGroupLayoutEntry { binding, visibility, ty: binding_type(entry)?, count: None })
}

pub fn texel_layout(layout: &webgpu::GpuTexelCopyBufferLayout) -> wgpu::TexelCopyBufferLayout {
    wgpu::TexelCopyBufferLayout {
        offset: or(layout.offset, 0),
        bytes_per_row: layout.bytes_per_row,
        rows_per_image: layout.rows_per_image,
    }
}

pub fn aspect(aspect: Option<webgpu::GpuTextureAspect>) -> wgpu::TextureAspect {
    aspect.map_or(wgpu::TextureAspect::All, Into::into)
}

pub fn clear_color(color: webgpu::GpuColor) -> wgpu::Color {
    wgpu::Color { r: color.r, g: color.g, b: color.b, a: color.a }
}

pub fn get<'t, T: 'static>(
    table: &'t ResourceTable,
    handle: &wasmtime::component::Resource<T>,
) -> wasmtime::Result<&'t T> {
    Ok(table.get(handle)?)
}
