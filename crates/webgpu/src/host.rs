//! The contract's calls, each onto wgpu. What wgpu cannot be asked is said where it is: a resource's label is not kept, its device is the editor's and lives on.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use wasmtime::component::{Access, Accessor, FutureReader, HasData, Resource, ResourceTable, StreamReader};
use wasmtime_wasi::WasiView;

use crate::conversions::{self as to, get};
use crate::types::{
    Buffer, BundleCommand, BundleEncoder, Compilation, ComputePass, Encoder, ErrorStream, Failure, Instance,
    LostFuture, Message, Pass, QuerySet, Recorded, WgslFeatures, completion,
};
use crate::wasi::webgpu::webgpu::{
    self, CreatePipelineError, CreateQuerySetError, GetMappedRangeError, GetMappedRangeErrorKind, Gpu, GpuAdapter,
    GpuAdapterInfo, GpuAddressMode, GpuBindGroup, GpuBindGroupDescriptor, GpuBindGroupLayout,
    GpuBindGroupLayoutDescriptor, GpuBindingResource, GpuBuffer, GpuBufferDescriptor, GpuBufferDynamicOffset,
    GpuBufferMapState, GpuBufferUsage, GpuCanvasConfiguration, GpuCanvasConfigurationOwned, GpuCanvasContext, GpuColor,
    GpuCommandBuffer, GpuCommandBufferDescriptor, GpuCommandEncoder, GpuCommandEncoderDescriptor, GpuCompilationInfo,
    GpuCompilationMessage, GpuCompilationMessageType, GpuComputePassDescriptor, GpuComputePassEncoder,
    GpuComputePipeline, GpuComputePipelineDescriptor, GpuDevice, GpuDeviceDescriptor, GpuDeviceLostInfo,
    GpuDeviceLostReason, GpuError, GpuErrorFilter, GpuErrorKind, GpuExtent3D, GpuFilterMode, GpuIndex32,
    GpuIndexFormat, GpuIntegerCoordinate, GpuIntegerCoordinateOut, GpuLayoutMode, GpuLoadOp, GpuMapMode,
    GpuPipelineConstantValue, GpuPipelineLayout, GpuPipelineLayoutDescriptor, GpuQuerySet, GpuQuerySetDescriptor,
    GpuQueryType, GpuQueue, GpuRenderBundle, GpuRenderBundleDescriptor, GpuRenderBundleEncoder,
    GpuRenderBundleEncoderDescriptor, GpuRenderPassDescriptor, GpuRenderPassEncoder, GpuRenderPipeline,
    GpuRenderPipelineDescriptor, GpuRequestAdapterOptions, GpuSampler, GpuSamplerDescriptor, GpuShaderModule,
    GpuShaderModuleDescriptor, GpuSignedOffset32, GpuSize32, GpuSize32Out, GpuSize64, GpuSize64Out, GpuStencilValue,
    GpuSupportedFeatures, GpuSupportedLimits, GpuTexelCopyBufferInfo, GpuTexelCopyBufferLayout,
    GpuTexelCopyTextureInfo, GpuTexture, GpuTextureDescriptor, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
    GpuTextureView, GpuTextureViewDescriptor, GpuTextureViewDimension, GpuUncapturedErrorEvent, MapAsyncError,
    MapAsyncErrorKind, PopErrorScopeError, PopErrorScopeErrorKind, RecordGpuPipelineConstantValue,
    RecordOptionGpuSize64, RequestDeviceError, RequestDeviceErrorKind, SetBindGroupError, UnmapError,
    WgslLanguageFeatures, WriteBufferError,
};

/// What the contract's calls are given.
pub struct Ctx<'a> {
    pub table: &'a mut ResourceTable,
}

pub struct HasGpu;

impl HasData for HasGpu {
    type Data<'a> = Ctx<'a>;
}

fn unsupported(name: &str) -> wasmtime::Error {
    wasmtime::Error::msg(format!("wasi:webgpu {name} is not supported yet"))
}

/// What a run of bytes the contract says by an offset and a size is.
fn window(data: &[u8], offset: Option<u64>, size: Option<u64>) -> wasmtime::Result<&[u8]> {
    let past = || wasmtime::Error::msg("past the end of its data");
    let bytes = data.get(usize::try_from(offset.unwrap_or(0))?..).ok_or_else(past)?;
    match size {
        Some(size) => bytes.get(..usize::try_from(size)?).ok_or_else(past),
        None => Ok(bytes),
    }
}

fn offsets_window(offsets: Option<Vec<u32>>, start: Option<u64>, length: Option<u32>) -> wasmtime::Result<Vec<u32>> {
    let offsets = offsets.unwrap_or_default();
    let from = usize::try_from(start.unwrap_or(0))?;
    let end = length.map_or(Ok(offsets.len()), |length| usize::try_from(length).map(|length| from + length))?;
    Ok(offsets.get(from..end).ok_or_else(|| wasmtime::Error::msg("past the end of its offsets"))?.to_vec())
}

fn sliced(buffer: &wgpu::Buffer, offset: Option<u64>, size: Option<u64>) -> wgpu::BufferSlice<'_> {
    let from = offset.unwrap_or(0);
    match size {
        Some(size) => buffer.slice(from..from + size),
        None => buffer.slice(from..),
    }
}

/// A texture copy's end, holding what it names.
struct TextureEnd {
    texture: wgpu::Texture,
    mip_level: u32,
    origin: wgpu::Origin3d,
    aspect: wgpu::TextureAspect,
}

impl TextureEnd {
    fn as_info(&self) -> wgpu::TexelCopyTextureInfo<'_> {
        wgpu::TexelCopyTextureInfo {
            texture: &self.texture,
            mip_level: self.mip_level,
            origin: self.origin,
            aspect: self.aspect,
        }
    }
}

/// A buffer copy's end, holding what it names.
struct BufferEnd {
    buffer: wgpu::Buffer,
    layout: wgpu::TexelCopyBufferLayout,
}

impl BufferEnd {
    fn as_info(&self) -> wgpu::TexelCopyBufferInfo<'_> {
        wgpu::TexelCopyBufferInfo { buffer: &self.buffer, layout: self.layout }
    }
}

/// A stage's constants as a descriptor names them.
fn named(constants: &[(String, f64)]) -> Vec<(&str, f64)> {
    constants.iter().map(|(name, value)| (name.as_str(), *value)).collect()
}

impl Ctx<'_> {
    fn with_pass(
        &mut self,
        pass: &Resource<Pass>,
        run: impl FnOnce(&mut wgpu::RenderPass<'static>),
    ) -> wasmtime::Result<()> {
        let pass = self.table.get_mut(pass)?;
        run(pass.0.as_mut().ok_or_else(|| wasmtime::Error::msg("a render pass already ended"))?);
        Ok(())
    }

    fn with_compute(
        &mut self,
        pass: &Resource<ComputePass>,
        run: impl FnOnce(&mut wgpu::ComputePass<'static>),
    ) -> wasmtime::Result<()> {
        let pass = self.table.get_mut(pass)?;
        run(pass.0.as_mut().ok_or_else(|| wasmtime::Error::msg("a compute pass already ended"))?);
        Ok(())
    }

    fn with_encoder(
        &mut self,
        encoder: &Resource<Encoder>,
        run: impl FnOnce(&mut wgpu::CommandEncoder),
    ) -> wasmtime::Result<()> {
        let encoder = self.table.get_mut(encoder)?;
        run(encoder.0.as_mut().ok_or_else(|| wasmtime::Error::msg("the encoder is finished"))?);
        Ok(())
    }

    fn record(&mut self, encoder: &Resource<BundleEncoder>, command: BundleCommand) -> wasmtime::Result<()> {
        let encoder = self.table.get_mut(encoder)?;
        wasmtime::ensure!(!encoder.done, "the bundle is finished");
        encoder.commands.push(command);
        Ok(())
    }

    fn texture_copy(&self, end: &GpuTexelCopyTextureInfo) -> wasmtime::Result<TextureEnd> {
        Ok(TextureEnd {
            texture: get(self.table, &end.texture)?.clone(),
            mip_level: end.mip_level.unwrap_or(0),
            origin: to::origin(end.origin),
            aspect: to::aspect(end.aspect),
        })
    }

    fn buffer_copy(&self, end: &GpuTexelCopyBufferInfo) -> wasmtime::Result<BufferEnd> {
        let layout = wgpu::TexelCopyBufferLayout {
            offset: end.offset.unwrap_or(0),
            bytes_per_row: end.bytes_per_row,
            rows_per_image: end.rows_per_image,
        };
        Ok(BufferEnd { buffer: get(self.table, &end.buffer)?.inner.clone(), layout })
    }

    fn constants(&self, record: Option<&Resource<HashMap<String, f64>>>) -> wasmtime::Result<Vec<(String, f64)>> {
        let Some(record) = record else { return Ok(Vec::new()) };
        Ok(get(self.table, record)?.iter().map(|(name, value)| (name.clone(), *value)).collect())
    }

    /// The device, for what asks one: the editor's, which has what it asks or does not.
    fn request_device(
        &mut self,
        descriptor: Option<&GpuDeviceDescriptor>,
    ) -> wasmtime::Result<Result<Resource<wgpu::Device>, RequestDeviceError>> {
        let refused =
            |message: String| Ok(Err(RequestDeviceError { kind: RequestDeviceErrorKind::OperationError, message }));
        let Some(device) = crate::device() else { return refused("nothing here draws".to_string()) };
        let wanted = descriptor
            .and_then(|d| d.required_features.as_deref())
            .map_or(wgpu::Features::empty(), crate::features::of);
        if !device.features().contains(wanted) {
            return refused(format!("the editor's device has no {:?}", wanted - device.features()));
        }
        Ok(Ok(self.table.push(device.clone())?))
    }

    fn render_pipeline(
        &mut self,
        device: &Resource<wgpu::Device>,
        descriptor: GpuRenderPipelineDescriptor,
    ) -> wasmtime::Result<wgpu::RenderPipeline> {
        let layout = match &descriptor.layout {
            GpuLayoutMode::Specific(layout) => Some(get(self.table, layout)?),
            GpuLayoutMode::Auto => None,
        };
        let buffers = descriptor.vertex.buffers.unwrap_or_default();
        let attributes: Vec<Vec<wgpu::VertexAttribute>> = buffers
            .iter()
            .map(|b| b.as_ref().map_or_else(Vec::new, |b| b.attributes.iter().map(to::vertex_attribute).collect()))
            .collect();
        let layouts: Vec<Option<wgpu::VertexBufferLayout<'_>>> = buffers
            .iter()
            .zip(&attributes)
            .map(|(buffer, attributes)| {
                buffer.as_ref().map(|buffer| wgpu::VertexBufferLayout {
                    array_stride: buffer.array_stride,
                    step_mode: buffer.step_mode.map_or(wgpu::VertexStepMode::Vertex, Into::into),
                    attributes,
                })
            })
            .collect();
        let held = self.constants(descriptor.vertex.constants.as_ref())?;
        let constants = named(&held);
        let vertex = wgpu::VertexState {
            module: get(self.table, &descriptor.vertex.module)?,
            entry_point: descriptor.vertex.entry_point.as_deref(),
            compilation_options: wgpu::PipelineCompilationOptions {
                constants: &constants,
                ..wgpu::PipelineCompilationOptions::default()
            },
            buffers: &layouts,
        };
        let targets = match &descriptor.fragment {
            Some(fragment) => fragment
                .targets
                .iter()
                .copied()
                .map(|t| t.map(to::color_target).transpose())
                .collect::<wasmtime::Result<Vec<_>>>()?,
            None => Vec::new(),
        };
        let held = self.constants(descriptor.fragment.as_ref().and_then(|f| f.constants.as_ref()))?;
        let fragment_constants = named(&held);
        let fragment = match &descriptor.fragment {
            Some(state) => Some(wgpu::FragmentState {
                module: get(self.table, &state.module)?,
                entry_point: state.entry_point.as_deref(),
                compilation_options: wgpu::PipelineCompilationOptions {
                    constants: &fragment_constants,
                    ..wgpu::PipelineCompilationOptions::default()
                },
                targets: &targets,
            }),
            None => None,
        };
        Ok(get(self.table, device)?.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: descriptor.label.as_deref(),
            layout,
            vertex,
            primitive: to::primitive(descriptor.primitive),
            depth_stencil: descriptor.depth_stencil.map(to::depth_stencil).transpose()?,
            multisample: to::multisample(descriptor.multisample),
            fragment,
            multiview_mask: None,
            cache: None,
        }))
    }

    fn compute_pipeline(
        &mut self,
        device: &Resource<wgpu::Device>,
        descriptor: &GpuComputePipelineDescriptor,
    ) -> wasmtime::Result<wgpu::ComputePipeline> {
        let layout = match &descriptor.layout {
            GpuLayoutMode::Specific(layout) => Some(get(self.table, layout)?),
            GpuLayoutMode::Auto => None,
        };
        let held = self.constants(descriptor.compute.constants.as_ref())?;
        let constants = named(&held);
        Ok(get(self.table, device)?.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: descriptor.label.as_deref(),
            layout,
            module: get(self.table, &descriptor.compute.module)?,
            entry_point: descriptor.compute.entry_point.as_deref(),
            compilation_options: wgpu::PipelineCompilationOptions {
                constants: &constants,
                ..wgpu::PipelineCompilationOptions::default()
            },
            cache: None,
        }))
    }

    /// A bundle's commands, recorded for real now that the device can be borrowed.
    fn replay(
        &mut self,
        encoder: &Resource<BundleEncoder>,
        label: Option<&str>,
    ) -> wasmtime::Result<wgpu::RenderBundle> {
        let recorded = self.table.get_mut(encoder)?;
        wasmtime::ensure!(!recorded.done, "the bundle is finished");
        recorded.done = true;
        let commands = std::mem::take(&mut recorded.commands);
        let colors = recorded
            .color_formats
            .iter()
            .map(|format| format.map(to::format).transpose())
            .collect::<wasmtime::Result<Vec<_>>>()?;
        let (depth_read_only, stencil_read_only) = (recorded.depth_read_only, recorded.stencil_read_only);
        let depth_stencil = recorded
            .depth_stencil
            .map(to::format)
            .transpose()?
            .map(|format| wgpu::RenderBundleDepthStencil { format, depth_read_only, stencil_read_only });
        let (device, sample_count) = (recorded.device.clone(), recorded.sample_count);
        let descriptor = wgpu::RenderBundleEncoderDescriptor {
            label: None,
            color_formats: &colors,
            depth_stencil,
            sample_count,
            multiview: None,
        };
        let mut bundle = device.create_render_bundle_encoder(&descriptor);
        for command in &commands {
            match command {
                BundleCommand::Pipeline(pipeline) => bundle.set_pipeline(pipeline),
                BundleCommand::BindGroup(index, group, offsets) => {
                    bundle.set_bind_group(*index, group.as_ref(), offsets);
                }
                BundleCommand::Immediates(offset, bytes) => bundle.set_immediates(*offset, bytes),
                BundleCommand::Vertex(slot, buffer, offset, size) => {
                    bundle.set_vertex_buffer(*slot, sliced(buffer, Some(*offset), *size));
                }
                BundleCommand::Index(buffer, format, offset, size) => {
                    bundle.set_index_buffer(sliced(buffer, Some(*offset), *size), *format);
                }
                BundleCommand::Draw(vertices, instances) => bundle.draw(vertices.clone(), instances.clone()),
                BundleCommand::DrawIndexed(indices, base, instances) => {
                    bundle.draw_indexed(indices.clone(), *base, instances.clone());
                }
                BundleCommand::DrawIndirect(buffer, offset) => bundle.draw_indirect(buffer, *offset),
                BundleCommand::DrawIndexedIndirect(buffer, offset) => bundle.draw_indexed_indirect(buffer, *offset),
            }
        }
        Ok(bundle.finish(&wgpu::RenderBundleDescriptor { label }))
    }
}

impl<T: WasiView + 'static> webgpu::HostGpuWithStore<T> for HasGpu {
    fn request_adapter(
        accessor: &Accessor<T, Self>,
        self_: Resource<Gpu>,
        options: Option<GpuRequestAdapterOptions>,
    ) -> impl ::core::future::Future<Output = wasmtime::Result<Option<Resource<GpuAdapter>>>> + Send {
        let _ = (self_, options);
        std::future::ready(accessor.with(|mut access| {
            let ctx = access.get();
            crate::adapter().cloned().map(|adapter| ctx.table.push(adapter)).transpose().map_err(Into::into)
        }))
    }
}

impl webgpu::HostGpu for Ctx<'_> {
    fn get_preferred_canvas_format(&mut self, self_: Resource<Gpu>) -> wasmtime::Result<GpuTextureFormat> {
        let _ = self_;
        Ok(GpuTextureFormat::Rgba8unorm)
    }
    fn wgsl_language_features(&mut self, self_: Resource<Gpu>) -> wasmtime::Result<Resource<WgslLanguageFeatures>> {
        let _ = self_;
        Ok(self.table.push(WgslFeatures)?)
    }
    fn drop(&mut self, rep: Resource<Gpu>) -> wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl<T: WasiView + 'static> webgpu::HostGpuAdapterWithStore<T> for HasGpu {
    fn request_device(
        accessor: &Accessor<T, Self>,
        self_: Resource<GpuAdapter>,
        descriptor: Option<GpuDeviceDescriptor>,
    ) -> impl ::core::future::Future<Output = wasmtime::Result<Result<Resource<GpuDevice>, RequestDeviceError>>> + Send
    {
        let _ = self_;
        std::future::ready(accessor.with(|mut access| {
            let mut ctx = access.get();
            ctx.request_device(descriptor.as_ref())
        }))
    }
}

impl webgpu::HostGpuAdapter for Ctx<'_> {
    fn features(&mut self, self_: Resource<GpuAdapter>) -> wasmtime::Result<Resource<GpuSupportedFeatures>> {
        let features = get(self.table, &self_)?.features();
        Ok(self.table.push(features)?)
    }
    fn limits(&mut self, self_: Resource<GpuAdapter>) -> wasmtime::Result<Resource<GpuSupportedLimits>> {
        let limits = get(self.table, &self_)?.limits();
        Ok(self.table.push(limits)?)
    }
    fn info(&mut self, self_: Resource<GpuAdapter>) -> wasmtime::Result<Resource<GpuAdapterInfo>> {
        let info = get(self.table, &self_)?.get_info();
        Ok(self.table.push(info)?)
    }
    fn drop(&mut self, rep: Resource<GpuAdapter>) -> wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl webgpu::HostGpuAdapterInfo for Ctx<'_> {
    fn vendor(&mut self, self_: Resource<GpuAdapterInfo>) -> wasmtime::Result<String> {
        Ok(get(self.table, &self_)?.vendor.to_string())
    }
    fn architecture(&mut self, self_: Resource<GpuAdapterInfo>) -> wasmtime::Result<String> {
        let _ = self_;
        Ok(String::new())
    }
    fn device(&mut self, self_: Resource<GpuAdapterInfo>) -> wasmtime::Result<String> {
        Ok(get(self.table, &self_)?.device.to_string())
    }
    fn description(&mut self, self_: Resource<GpuAdapterInfo>) -> wasmtime::Result<String> {
        Ok(get(self.table, &self_)?.name.clone())
    }
    fn subgroup_min_size(&mut self, self_: Resource<GpuAdapterInfo>) -> wasmtime::Result<u32> {
        Ok(get(self.table, &self_)?.subgroup_min_size)
    }
    fn subgroup_max_size(&mut self, self_: Resource<GpuAdapterInfo>) -> wasmtime::Result<u32> {
        Ok(get(self.table, &self_)?.subgroup_max_size)
    }
    fn is_fallback_adapter(&mut self, self_: Resource<GpuAdapterInfo>) -> wasmtime::Result<bool> {
        Ok(get(self.table, &self_)?.device_type == wgpu::DeviceType::Cpu)
    }
    fn drop(&mut self, rep: Resource<GpuAdapterInfo>) -> wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl webgpu::HostGpuBindGroup for Ctx<'_> {
    fn label(&mut self, self_: Resource<GpuBindGroup>) -> wasmtime::Result<String> {
        let _ = self_;
        Ok(String::new())
    }
    fn set_label(&mut self, self_: Resource<GpuBindGroup>, label: String) -> wasmtime::Result<()> {
        let _ = (self_, label);
        Ok(())
    }
    fn drop(&mut self, rep: Resource<GpuBindGroup>) -> wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl webgpu::HostGpuBindGroupLayout for Ctx<'_> {
    fn label(&mut self, self_: Resource<GpuBindGroupLayout>) -> wasmtime::Result<String> {
        let _ = self_;
        Ok(String::new())
    }
    fn set_label(&mut self, self_: Resource<GpuBindGroupLayout>, label: String) -> wasmtime::Result<()> {
        let _ = (self_, label);
        Ok(())
    }
    fn drop(&mut self, rep: Resource<GpuBindGroupLayout>) -> wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl<T: WasiView + 'static> webgpu::HostGpuBufferWithStore<T> for HasGpu {
    async fn map_async(
        accessor: &Accessor<T, Self>,
        self_: Resource<GpuBuffer>,
        mode: GpuMapMode,
        offset: Option<GpuSize64>,
        size: Option<GpuSize64>,
    ) -> wasmtime::Result<Result<(), MapAsyncError>> {
        let (finished, state) = accessor.with(|mut access| -> wasmtime::Result<_> {
            let ctx = access.get();
            let buffer = get(ctx.table, &self_)?;
            let from = offset.unwrap_or(0);
            let end = size.map_or(buffer.inner.size(), |size| from + size);
            let (done, finished) = completion();
            let wanted = if mode.contains(GpuMapMode::WRITE) { wgpu::MapMode::Write } else { wgpu::MapMode::Read };
            *buffer.state.lock().unwrap_or_else(PoisonError::into_inner) = GpuBufferMapState::Pending;
            buffer.inner.map_async(wanted, from..end, move |result| done.complete(result));
            Ok((finished, buffer.state.clone()))
        })?;
        crate::poll_soon();
        let mapped = finished.await;
        *state.lock().unwrap_or_else(PoisonError::into_inner) =
            if mapped.is_ok() { GpuBufferMapState::Mapped } else { GpuBufferMapState::Unmapped };
        Ok(mapped.map_err(|e| MapAsyncError { kind: MapAsyncErrorKind::OperationError, message: e.to_string() }))
    }
}

impl webgpu::HostGpuBuffer for Ctx<'_> {
    fn size(&mut self, self_: Resource<GpuBuffer>) -> wasmtime::Result<GpuSize64Out> {
        Ok(get(self.table, &self_)?.inner.size())
    }
    fn usage(&mut self, self_: Resource<GpuBuffer>) -> wasmtime::Result<GpuBufferUsage> {
        get(self.table, &self_)?.inner.usage().try_into()
    }
    fn map_state(&mut self, self_: Resource<GpuBuffer>) -> wasmtime::Result<GpuBufferMapState> {
        Ok(*get(self.table, &self_)?.state.lock().unwrap_or_else(PoisonError::into_inner))
    }
    fn get_mapped_range_get_with_copy(
        &mut self,
        self_: Resource<GpuBuffer>,
        offset: Option<GpuSize64>,
        size: Option<GpuSize64>,
    ) -> wasmtime::Result<Result<Vec<u8>, GetMappedRangeError>> {
        let buffer = &get(self.table, &self_)?.inner;
        let from = offset.unwrap_or(0);
        let end = size.map_or(buffer.size(), |size| from + size);
        Ok(match buffer.slice(from..end).get_mapped_range() {
            Ok(view) => Ok(view.to_vec()),
            Err(e) => {
                Err(GetMappedRangeError { kind: GetMappedRangeErrorKind::OperationError, message: e.to_string() })
            }
        })
    }
    fn unmap(&mut self, self_: Resource<GpuBuffer>) -> wasmtime::Result<Result<(), UnmapError>> {
        let buffer = get(self.table, &self_)?;
        buffer.inner.unmap();
        *buffer.state.lock().unwrap_or_else(PoisonError::into_inner) = GpuBufferMapState::Unmapped;
        Ok(Ok(()))
    }
    fn destroy(&mut self, self_: Resource<GpuBuffer>) -> wasmtime::Result<()> {
        get(self.table, &self_)?.inner.destroy();
        Ok(())
    }
    fn label(&mut self, self_: Resource<GpuBuffer>) -> wasmtime::Result<String> {
        let _ = self_;
        Ok(String::new())
    }
    fn set_label(&mut self, self_: Resource<GpuBuffer>, label: String) -> wasmtime::Result<()> {
        let _ = (self_, label);
        Ok(())
    }
    fn get_mapped_range_set_with_copy(
        &mut self,
        self_: Resource<GpuBuffer>,
        data: Vec<u8>,
        offset: Option<GpuSize64>,
        size: Option<GpuSize64>,
    ) -> wasmtime::Result<Result<(), GetMappedRangeError>> {
        let buffer = &get(self.table, &self_)?.inner;
        let from = offset.unwrap_or(0);
        let length = size.unwrap_or(data.len() as u64);
        let bytes =
            data.get(..usize::try_from(length)?).ok_or_else(|| wasmtime::Error::msg("past the end of its data"))?;
        Ok(match buffer.slice(from..from + length).get_mapped_range_mut() {
            Ok(mut view) => {
                view.copy_from_slice(bytes);
                Ok(())
            }
            Err(e) => {
                Err(GetMappedRangeError { kind: GetMappedRangeErrorKind::OperationError, message: e.to_string() })
            }
        })
    }
    fn drop(&mut self, rep: Resource<GpuBuffer>) -> wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl webgpu::HostGpuCanvasContext for Ctx<'_> {
    fn configure(
        &mut self,
        _self_: Resource<GpuCanvasContext>,
        _configuration: GpuCanvasConfiguration,
    ) -> wasmtime::Result<()> {
        Err(unsupported("GpuCanvasContext.configure"))
    }
    fn unconfigure(&mut self, _self_: Resource<GpuCanvasContext>) -> wasmtime::Result<()> {
        Err(unsupported("GpuCanvasContext.unconfigure"))
    }
    fn get_configuration(
        &mut self,
        _self_: Resource<GpuCanvasContext>,
    ) -> wasmtime::Result<Option<GpuCanvasConfigurationOwned>> {
        Err(unsupported("GpuCanvasContext.get_configuration"))
    }
    fn get_current_texture(&mut self, _self_: Resource<GpuCanvasContext>) -> wasmtime::Result<Resource<GpuTexture>> {
        Err(unsupported("GpuCanvasContext.get_current_texture"))
    }
    fn drop(&mut self, rep: Resource<GpuCanvasContext>) -> wasmtime::Result<()> {
        let _ = rep;
        Ok(())
    }
}

impl webgpu::HostGpuCommandBuffer for Ctx<'_> {
    fn label(&mut self, self_: Resource<GpuCommandBuffer>) -> wasmtime::Result<String> {
        let _ = self_;
        Ok(String::new())
    }
    fn set_label(&mut self, self_: Resource<GpuCommandBuffer>, label: String) -> wasmtime::Result<()> {
        let _ = (self_, label);
        Ok(())
    }
    fn drop(&mut self, rep: Resource<GpuCommandBuffer>) -> wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl webgpu::HostGpuCommandEncoder for Ctx<'_> {
    fn begin_render_pass(
        &mut self,
        self_: Resource<GpuCommandEncoder>,
        descriptor: GpuRenderPassDescriptor,
    ) -> wasmtime::Result<Resource<GpuRenderPassEncoder>> {
        let mut encoder =
            self.table.get_mut(&self_)?.0.take().ok_or_else(|| wasmtime::Error::msg("the encoder is finished"))?;
        let mut colors = Vec::with_capacity(descriptor.color_attachments.len());
        for attachment in &descriptor.color_attachments {
            colors.push(match attachment {
                Some(a) => Some(wgpu::RenderPassColorAttachment {
                    view: get(self.table, &a.view)?,
                    depth_slice: a.depth_slice,
                    resolve_target: a.resolve_target.as_ref().map(|view| get(self.table, view)).transpose()?,
                    ops: wgpu::Operations {
                        load: match a.load_op {
                            GpuLoadOp::Clear => {
                                wgpu::LoadOp::Clear(a.clear_value.map_or(wgpu::Color::TRANSPARENT, to::clear_color))
                            }
                            GpuLoadOp::Load => wgpu::LoadOp::Load,
                        },
                        store: a.store_op.into(),
                    },
                }),
                None => None,
            });
        }
        let depth = match &descriptor.depth_stencil_attachment {
            Some(a) => Some(wgpu::RenderPassDepthStencilAttachment {
                view: get(self.table, &a.view)?,
                depth_ops: a.depth_load_op.map(|load| wgpu::Operations {
                    load: match load {
                        GpuLoadOp::Clear => wgpu::LoadOp::Clear(a.depth_clear_value.unwrap_or(1.0)),
                        GpuLoadOp::Load => wgpu::LoadOp::Load,
                    },
                    store: a.depth_store_op.map_or(wgpu::StoreOp::Store, Into::into),
                }),
                stencil_ops: a.stencil_load_op.map(|load| wgpu::Operations {
                    load: match load {
                        GpuLoadOp::Clear => wgpu::LoadOp::Clear(a.stencil_clear_value.unwrap_or(0)),
                        GpuLoadOp::Load => wgpu::LoadOp::Load,
                    },
                    store: a.stencil_store_op.map_or(wgpu::StoreOp::Store, Into::into),
                }),
            }),
            None => None,
        };
        let occlusion =
            descriptor.occlusion_query_set.as_ref().map(|set| get(self.table, set)).transpose()?.map(|set| &set.inner);
        let mut timestamps = None;
        if let Some(writes) = &descriptor.timestamp_writes {
            timestamps = Some(wgpu::RenderPassTimestampWrites {
                query_set: &get(self.table, &writes.query_set)?.inner,
                beginning_of_pass_write_index: writes.beginning_of_pass_write_index,
                end_of_pass_write_index: writes.end_of_pass_write_index,
            });
        }
        let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: descriptor.label.as_deref(),
            color_attachments: &colors,
            depth_stencil_attachment: depth,
            timestamp_writes: timestamps,
            occlusion_query_set: occlusion,
            multiview_mask: None,
        });
        let pass = pass.forget_lifetime();
        self.table.get_mut(&self_)?.0 = Some(encoder);
        Ok(self.table.push(Pass(Some(pass)))?)
    }
    fn begin_compute_pass(
        &mut self,
        self_: Resource<GpuCommandEncoder>,
        descriptor: Option<GpuComputePassDescriptor>,
    ) -> wasmtime::Result<Resource<GpuComputePassEncoder>> {
        let mut encoder =
            self.table.get_mut(&self_)?.0.take().ok_or_else(|| wasmtime::Error::msg("the encoder is finished"))?;
        let mut timestamps = None;
        if let Some(writes) = descriptor.as_ref().and_then(|d| d.timestamp_writes.as_ref()) {
            timestamps = Some(wgpu::ComputePassTimestampWrites {
                query_set: &get(self.table, &writes.query_set)?.inner,
                beginning_of_pass_write_index: writes.beginning_of_pass_write_index,
                end_of_pass_write_index: writes.end_of_pass_write_index,
            });
        }
        let label = descriptor.as_ref().and_then(|d| d.label.as_deref());
        let pass = encoder
            .begin_compute_pass(&wgpu::ComputePassDescriptor { label, timestamp_writes: timestamps })
            .forget_lifetime();
        self.table.get_mut(&self_)?.0 = Some(encoder);
        Ok(self.table.push(ComputePass(Some(pass)))?)
    }
    fn copy_buffer_to_buffer(
        &mut self,
        self_: Resource<GpuCommandEncoder>,
        source: Resource<GpuBuffer>,
        source_offset: Option<GpuSize64>,
        destination: Resource<GpuBuffer>,
        destination_offset: Option<GpuSize64>,
        size: Option<GpuSize64>,
    ) -> wasmtime::Result<()> {
        let source = get(self.table, &source)?.inner.clone();
        let destination = get(self.table, &destination)?.inner.clone();
        let (from, to_) = (source_offset.unwrap_or(0), destination_offset.unwrap_or(0));
        self.with_encoder(&self_, |encoder| encoder.copy_buffer_to_buffer(&source, from, &destination, to_, size))
    }
    fn copy_buffer_to_texture(
        &mut self,
        self_: Resource<GpuCommandEncoder>,
        source: GpuTexelCopyBufferInfo,
        destination: GpuTexelCopyTextureInfo,
        copy_size: GpuExtent3D,
    ) -> wasmtime::Result<()> {
        let (buffer, texture) = (self.buffer_copy(&source)?, self.texture_copy(&destination)?);
        self.with_encoder(&self_, |encoder| {
            encoder.copy_buffer_to_texture(buffer.as_info(), texture.as_info(), to::extent(copy_size));
        })
    }
    fn copy_texture_to_buffer(
        &mut self,
        self_: Resource<GpuCommandEncoder>,
        source: GpuTexelCopyTextureInfo,
        destination: GpuTexelCopyBufferInfo,
        copy_size: GpuExtent3D,
    ) -> wasmtime::Result<()> {
        let (texture, buffer) = (self.texture_copy(&source)?, self.buffer_copy(&destination)?);
        self.with_encoder(&self_, |encoder| {
            encoder.copy_texture_to_buffer(texture.as_info(), buffer.as_info(), to::extent(copy_size));
        })
    }
    fn copy_texture_to_texture(
        &mut self,
        self_: Resource<GpuCommandEncoder>,
        source: GpuTexelCopyTextureInfo,
        destination: GpuTexelCopyTextureInfo,
        copy_size: GpuExtent3D,
    ) -> wasmtime::Result<()> {
        let (from, into) = (self.texture_copy(&source)?, self.texture_copy(&destination)?);
        self.with_encoder(&self_, |encoder| {
            encoder.copy_texture_to_texture(from.as_info(), into.as_info(), to::extent(copy_size));
        })
    }
    fn clear_buffer(
        &mut self,
        self_: Resource<GpuCommandEncoder>,
        buffer: Resource<GpuBuffer>,
        offset: Option<GpuSize64>,
        size: Option<GpuSize64>,
    ) -> wasmtime::Result<()> {
        let buffer = get(self.table, &buffer)?.inner.clone();
        self.with_encoder(&self_, |encoder| encoder.clear_buffer(&buffer, offset.unwrap_or(0), size))
    }
    fn resolve_query_set(
        &mut self,
        self_: Resource<GpuCommandEncoder>,
        query_set: Resource<GpuQuerySet>,
        first_query: GpuSize32,
        query_count: GpuSize32,
        destination: Resource<GpuBuffer>,
        destination_offset: GpuSize64,
    ) -> wasmtime::Result<()> {
        let set = get(self.table, &query_set)?.inner.clone();
        let destination = get(self.table, &destination)?.inner.clone();
        self.with_encoder(&self_, |encoder| {
            encoder.resolve_query_set(&set, first_query..first_query + query_count, &destination, destination_offset);
        })
    }
    fn finish(
        &mut self,
        self_: Resource<GpuCommandEncoder>,
        descriptor: Option<GpuCommandBufferDescriptor>,
    ) -> wasmtime::Result<Resource<GpuCommandBuffer>> {
        let label = descriptor.and_then(|d| d.label);
        let encoder =
            self.table.get_mut(&self_)?.0.take().ok_or_else(|| wasmtime::Error::msg("the encoder is finished"))?;
        let _ = label;
        Ok(self.table.push(Recorded(Some(encoder.finish())))?)
    }
    fn label(&mut self, self_: Resource<GpuCommandEncoder>) -> wasmtime::Result<String> {
        let _ = self_;
        Ok(String::new())
    }
    fn set_label(&mut self, self_: Resource<GpuCommandEncoder>, label: String) -> wasmtime::Result<()> {
        let _ = (self_, label);
        Ok(())
    }
    fn push_debug_group(&mut self, self_: Resource<GpuCommandEncoder>, group_label: String) -> wasmtime::Result<()> {
        self.with_encoder(&self_, |encoder| encoder.push_debug_group(&group_label))
    }
    fn pop_debug_group(&mut self, self_: Resource<GpuCommandEncoder>) -> wasmtime::Result<()> {
        self.with_encoder(&self_, wgpu::CommandEncoder::pop_debug_group)
    }
    fn insert_debug_marker(
        &mut self,
        self_: Resource<GpuCommandEncoder>,
        marker_label: String,
    ) -> wasmtime::Result<()> {
        self.with_encoder(&self_, |encoder| encoder.insert_debug_marker(&marker_label))
    }
    fn drop(&mut self, rep: Resource<GpuCommandEncoder>) -> wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl webgpu::HostGpuCompilationInfo for Ctx<'_> {
    fn messages(
        &mut self,
        self_: Resource<GpuCompilationInfo>,
    ) -> wasmtime::Result<Vec<Resource<GpuCompilationMessage>>> {
        let messages = get(self.table, &self_)?.0.clone();
        messages.into_iter().map(|message| Ok(self.table.push(message)?)).collect()
    }
    fn drop(&mut self, rep: Resource<GpuCompilationInfo>) -> wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl webgpu::HostGpuCompilationMessage for Ctx<'_> {
    fn message(&mut self, self_: Resource<GpuCompilationMessage>) -> wasmtime::Result<String> {
        Ok(get(self.table, &self_)?.text.clone())
    }
    fn type_(&mut self, self_: Resource<GpuCompilationMessage>) -> wasmtime::Result<GpuCompilationMessageType> {
        Ok(get(self.table, &self_)?.kind)
    }
    fn line_num(&mut self, self_: Resource<GpuCompilationMessage>) -> wasmtime::Result<u64> {
        Ok(get(self.table, &self_)?.line)
    }
    fn line_pos(&mut self, self_: Resource<GpuCompilationMessage>) -> wasmtime::Result<u64> {
        Ok(get(self.table, &self_)?.position)
    }
    fn offset(&mut self, self_: Resource<GpuCompilationMessage>) -> wasmtime::Result<u64> {
        Ok(get(self.table, &self_)?.offset)
    }
    fn length(&mut self, self_: Resource<GpuCompilationMessage>) -> wasmtime::Result<u64> {
        Ok(get(self.table, &self_)?.length)
    }
    fn drop(&mut self, rep: Resource<GpuCompilationMessage>) -> wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl webgpu::HostGpuComputePassEncoder for Ctx<'_> {
    fn set_pipeline(
        &mut self,
        self_: Resource<GpuComputePassEncoder>,
        pipeline: Resource<GpuComputePipeline>,
    ) -> wasmtime::Result<()> {
        let pipeline = get(self.table, &pipeline)?.clone();
        self.with_compute(&self_, |pass| pass.set_pipeline(&pipeline))
    }
    fn dispatch_workgroups(
        &mut self,
        self_: Resource<GpuComputePassEncoder>,
        workgroup_count_x: GpuSize32,
        workgroup_count_y: Option<GpuSize32>,
        workgroup_count_z: Option<GpuSize32>,
    ) -> wasmtime::Result<()> {
        self.with_compute(&self_, |pass| {
            pass.dispatch_workgroups(workgroup_count_x, workgroup_count_y.unwrap_or(1), workgroup_count_z.unwrap_or(1));
        })
    }
    fn dispatch_workgroups_indirect(
        &mut self,
        self_: Resource<GpuComputePassEncoder>,
        indirect_buffer: Resource<GpuBuffer>,
        indirect_offset: GpuSize64,
    ) -> wasmtime::Result<()> {
        let buffer = get(self.table, &indirect_buffer)?.inner.clone();
        self.with_compute(&self_, |pass| pass.dispatch_workgroups_indirect(&buffer, indirect_offset))
    }
    fn end(&mut self, self_: Resource<GpuComputePassEncoder>) -> wasmtime::Result<()> {
        self.table.get_mut(&self_)?.0 = None;
        Ok(())
    }
    fn label(&mut self, self_: Resource<GpuComputePassEncoder>) -> wasmtime::Result<String> {
        let _ = self_;
        Ok(String::new())
    }
    fn set_label(&mut self, self_: Resource<GpuComputePassEncoder>, label: String) -> wasmtime::Result<()> {
        let _ = (self_, label);
        Ok(())
    }
    fn push_debug_group(
        &mut self,
        self_: Resource<GpuComputePassEncoder>,
        group_label: String,
    ) -> wasmtime::Result<()> {
        self.with_compute(&self_, |pass| pass.push_debug_group(&group_label))
    }
    fn pop_debug_group(&mut self, self_: Resource<GpuComputePassEncoder>) -> wasmtime::Result<()> {
        self.with_compute(&self_, wgpu::ComputePass::pop_debug_group)
    }
    fn insert_debug_marker(
        &mut self,
        self_: Resource<GpuComputePassEncoder>,
        marker_label: String,
    ) -> wasmtime::Result<()> {
        self.with_compute(&self_, |pass| pass.insert_debug_marker(&marker_label))
    }
    fn set_bind_group(
        &mut self,
        self_: Resource<GpuComputePassEncoder>,
        index: GpuIndex32,
        bind_group: Option<Resource<GpuBindGroup>>,
        dynamic_offsets_data: Option<Vec<GpuBufferDynamicOffset>>,
        dynamic_offsets_data_start: Option<GpuSize64>,
        dynamic_offsets_data_length: Option<GpuSize32>,
    ) -> wasmtime::Result<Result<(), SetBindGroupError>> {
        let group = bind_group.as_ref().map(|group| get(self.table, group)).transpose()?.cloned();
        let offsets = offsets_window(dynamic_offsets_data, dynamic_offsets_data_start, dynamic_offsets_data_length)?;
        self.with_compute(&self_, |pass| pass.set_bind_group(index, group.as_ref(), &offsets))?;
        Ok(Ok(()))
    }
    fn set_immediates(
        &mut self,
        self_: Resource<GpuComputePassEncoder>,
        range_offset: GpuSize32,
        data: Vec<u8>,
        data_offset: Option<GpuSize64>,
        data_size: Option<GpuSize64>,
    ) -> wasmtime::Result<()> {
        let bytes = window(&data, data_offset, data_size)?.to_vec();
        self.with_compute(&self_, |pass| pass.set_immediates(range_offset, &bytes))
    }
    fn drop(&mut self, rep: Resource<GpuComputePassEncoder>) -> wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl webgpu::HostGpuComputePipeline for Ctx<'_> {
    fn label(&mut self, self_: Resource<GpuComputePipeline>) -> wasmtime::Result<String> {
        let _ = self_;
        Ok(String::new())
    }
    fn set_label(&mut self, self_: Resource<GpuComputePipeline>, label: String) -> wasmtime::Result<()> {
        let _ = (self_, label);
        Ok(())
    }
    fn get_bind_group_layout(
        &mut self,
        self_: Resource<GpuComputePipeline>,
        index: u32,
    ) -> wasmtime::Result<Resource<GpuBindGroupLayout>> {
        let layout = get(self.table, &self_)?.get_bind_group_layout(index);
        Ok(self.table.push(layout)?)
    }
    fn drop(&mut self, rep: Resource<GpuComputePipeline>) -> wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl<T: WasiView + 'static> webgpu::HostGpuDeviceWithStore<T> for HasGpu {
    fn create_compute_pipeline_async(
        accessor: &Accessor<T, Self>,
        self_: Resource<GpuDevice>,
        descriptor: GpuComputePipelineDescriptor,
    ) -> impl ::core::future::Future<
        Output = wasmtime::Result<Result<Resource<GpuComputePipeline>, CreatePipelineError>>,
    > + Send {
        std::future::ready(accessor.with(|mut access| {
            let mut ctx = access.get();
            let made = ctx.compute_pipeline(&self_, &descriptor)?;
            Ok(Ok(ctx.table.push(made)?))
        }))
    }
    fn create_render_pipeline_async(
        accessor: &Accessor<T, Self>,
        self_: Resource<GpuDevice>,
        descriptor: GpuRenderPipelineDescriptor,
    ) -> impl ::core::future::Future<Output = wasmtime::Result<Result<Resource<GpuRenderPipeline>, CreatePipelineError>>>
    + Send {
        std::future::ready(accessor.with(|mut access| {
            let mut ctx = access.get();
            let made = ctx.render_pipeline(&self_, descriptor)?;
            Ok(Ok(ctx.table.push(made)?))
        }))
    }
    fn lost(
        host: Access<T, Self>,
        self_: Resource<GpuDevice>,
    ) -> wasmtime::Result<FutureReader<Resource<GpuDeviceLostInfo>>> {
        let _ = self_;
        FutureReader::new(host, LostFuture(crate::lost_inbox()))
    }
    fn pop_error_scope(
        accessor: &Accessor<T, Self>,
        self_: Resource<GpuDevice>,
    ) -> impl ::core::future::Future<Output = wasmtime::Result<Result<Option<Resource<GpuError>>, PopErrorScopeError>>> + Send
    {
        let _ = self_;
        std::future::ready(accessor.with(|mut access| {
            let ctx = access.get();
            let Some(scope) = crate::SCOPES.with_borrow_mut(Vec::pop) else {
                return Ok(Err(PopErrorScopeError {
                    kind: PopErrorScopeErrorKind::OperationError,
                    message: "no scope is open on this thread".to_string(),
                }));
            };
            let caught = crate::now(scope.pop()).flatten();
            Ok(Ok(caught.map(|error| ctx.table.push(Failure::from(error))).transpose()?))
        }))
    }
    fn on_uncaptured_error(
        host: Access<T, Self>,
        self_: Resource<GpuDevice>,
    ) -> wasmtime::Result<StreamReader<Resource<GpuError>>> {
        let _ = self_;
        StreamReader::new(host, ErrorStream(crate::error_inbox()))
    }
}

impl webgpu::HostGpuDevice for Ctx<'_> {
    fn features(&mut self, self_: Resource<GpuDevice>) -> wasmtime::Result<Resource<GpuSupportedFeatures>> {
        let features = get(self.table, &self_)?.features();
        Ok(self.table.push(features)?)
    }
    fn limits(&mut self, self_: Resource<GpuDevice>) -> wasmtime::Result<Resource<GpuSupportedLimits>> {
        let limits = get(self.table, &self_)?.limits();
        Ok(self.table.push(limits)?)
    }
    fn adapter_info(&mut self, self_: Resource<GpuDevice>) -> wasmtime::Result<Resource<GpuAdapterInfo>> {
        let info = get(self.table, &self_)?.adapter_info();
        Ok(self.table.push(info)?)
    }
    fn queue(&mut self, self_: Resource<GpuDevice>) -> wasmtime::Result<Resource<GpuQueue>> {
        let _ = self_;
        let queue = crate::queue()?.clone();
        Ok(self.table.push(queue)?)
    }
    fn destroy(&mut self, self_: Resource<GpuDevice>) -> wasmtime::Result<()> {
        // The device is the editor's: an extension is done with it, the window is not.
        let _ = self_;
        Ok(())
    }
    fn create_buffer(
        &mut self,
        self_: Resource<GpuDevice>,
        descriptor: GpuBufferDescriptor,
    ) -> wasmtime::Result<Resource<GpuBuffer>> {
        let usage = descriptor.usage.try_into()?;
        let mapped = descriptor.mapped_at_creation.unwrap_or(false);
        let inner = get(self.table, &self_)?.create_buffer(&wgpu::BufferDescriptor {
            label: descriptor.label.as_deref(),
            size: descriptor.size,
            usage,
            mapped_at_creation: mapped,
        });
        let state = if mapped { GpuBufferMapState::Mapped } else { GpuBufferMapState::Unmapped };
        Ok(self.table.push(Buffer { inner, state: Arc::new(Mutex::new(state)) })?)
    }
    fn create_texture(
        &mut self,
        self_: Resource<GpuDevice>,
        descriptor: GpuTextureDescriptor,
    ) -> wasmtime::Result<Resource<GpuTexture>> {
        let usage = descriptor.usage.try_into()?;
        let made = get(self.table, &self_)?.create_texture(&wgpu::TextureDescriptor {
            label: descriptor.label.as_deref(),
            size: to::extent(descriptor.size),
            mip_level_count: descriptor.mip_level_count.unwrap_or(1),
            sample_count: descriptor.sample_count.unwrap_or(1),
            dimension: descriptor.dimension.map_or(wgpu::TextureDimension::D2, Into::into),
            format: to::format(descriptor.format)?,
            usage,
            view_formats: &[],
        });
        Ok(self.table.push(made)?)
    }
    fn create_sampler(
        &mut self,
        self_: Resource<GpuDevice>,
        descriptor: Option<GpuSamplerDescriptor>,
    ) -> wasmtime::Result<Resource<GpuSampler>> {
        let d = descriptor.as_ref();
        let address = |mode: Option<GpuAddressMode>| mode.map_or(wgpu::AddressMode::ClampToEdge, Into::into);
        let filter = |filter: Option<GpuFilterMode>| filter.map_or(wgpu::FilterMode::Nearest, Into::into);
        let made = get(self.table, &self_)?.create_sampler(&wgpu::SamplerDescriptor {
            label: d.and_then(|d| d.label.as_deref()),
            address_mode_u: address(d.and_then(|d| d.address_mode_u)),
            address_mode_v: address(d.and_then(|d| d.address_mode_v)),
            address_mode_w: address(d.and_then(|d| d.address_mode_w)),
            mag_filter: filter(d.and_then(|d| d.mag_filter)),
            min_filter: filter(d.and_then(|d| d.min_filter)),
            mipmap_filter: d.and_then(|d| d.mipmap_filter).map_or(wgpu::MipmapFilterMode::Nearest, Into::into),
            lod_min_clamp: d.and_then(|d| d.lod_min_clamp).unwrap_or(0.0),
            lod_max_clamp: d.and_then(|d| d.lod_max_clamp).unwrap_or(32.0),
            compare: d.and_then(|d| d.compare).map(Into::into),
            anisotropy_clamp: d.and_then(|d| d.max_anisotropy).unwrap_or(1),
            border_color: None,
        });
        Ok(self.table.push(made)?)
    }
    fn create_bind_group_layout(
        &mut self,
        self_: Resource<GpuDevice>,
        descriptor: GpuBindGroupLayoutDescriptor,
    ) -> wasmtime::Result<Resource<GpuBindGroupLayout>> {
        let entries = descriptor.entries.into_iter().map(to::layout_entry).collect::<wasmtime::Result<Vec<_>>>()?;
        let made = get(self.table, &self_)?.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: descriptor.label.as_deref(),
            entries: &entries,
        });
        Ok(self.table.push(made)?)
    }
    fn create_pipeline_layout(
        &mut self,
        self_: Resource<GpuDevice>,
        descriptor: GpuPipelineLayoutDescriptor,
    ) -> wasmtime::Result<Resource<GpuPipelineLayout>> {
        let mut layouts = Vec::with_capacity(descriptor.bind_group_layouts.len());
        for layout in &descriptor.bind_group_layouts {
            layouts.push(layout.as_ref().map(|layout| get(self.table, layout)).transpose()?);
        }
        let made = get(self.table, &self_)?.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: descriptor.label.as_deref(),
            bind_group_layouts: &layouts,
            immediate_size: descriptor.immediate_size.unwrap_or(0),
        });
        Ok(self.table.push(made)?)
    }
    fn create_bind_group(
        &mut self,
        self_: Resource<GpuDevice>,
        descriptor: GpuBindGroupDescriptor,
    ) -> wasmtime::Result<Resource<GpuBindGroup>> {
        let mut views = Vec::new();
        for entry in &descriptor.entries {
            if let GpuBindingResource::GpuTexture(texture) = &entry.resource {
                views.push(get(self.table, texture)?.create_view(&wgpu::TextureViewDescriptor::default()));
            }
        }
        let mut whole = views.iter();
        let mut entries = Vec::with_capacity(descriptor.entries.len());
        for entry in &descriptor.entries {
            let resource = match &entry.resource {
                GpuBindingResource::GpuBuffer(buffer) => get(self.table, buffer)?.inner.as_entire_binding(),
                GpuBindingResource::GpuBufferBinding(binding) => wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &get(self.table, &binding.buffer)?.inner,
                    offset: binding.offset.unwrap_or(0),
                    size: binding.size.and_then(wgpu::BufferSize::new),
                }),
                GpuBindingResource::GpuSampler(sampler) => wgpu::BindingResource::Sampler(get(self.table, sampler)?),
                GpuBindingResource::GpuTextureView(view) => wgpu::BindingResource::TextureView(get(self.table, view)?),
                GpuBindingResource::GpuTexture(_) => wgpu::BindingResource::TextureView(
                    whole.next().ok_or_else(|| wasmtime::Error::msg("a texture lost its view"))?,
                ),
            };
            entries.push(wgpu::BindGroupEntry { binding: entry.binding, resource });
        }
        let layout = get(self.table, &descriptor.layout)?;
        let made = get(self.table, &self_)?.create_bind_group(&wgpu::BindGroupDescriptor {
            label: descriptor.label.as_deref(),
            layout,
            entries: &entries,
        });
        Ok(self.table.push(made)?)
    }
    fn create_shader_module(
        &mut self,
        self_: Resource<GpuDevice>,
        descriptor: GpuShaderModuleDescriptor,
    ) -> wasmtime::Result<Resource<GpuShaderModule>> {
        let source = wgpu::ShaderSource::Wgsl(descriptor.code.into());
        let made = get(self.table, &self_)?
            .create_shader_module(wgpu::ShaderModuleDescriptor { label: descriptor.label.as_deref(), source });
        Ok(self.table.push(made)?)
    }
    fn create_compute_pipeline(
        &mut self,
        self_: Resource<GpuDevice>,
        descriptor: GpuComputePipelineDescriptor,
    ) -> wasmtime::Result<Resource<GpuComputePipeline>> {
        let made = self.compute_pipeline(&self_, &descriptor)?;
        Ok(self.table.push(made)?)
    }
    fn create_render_pipeline(
        &mut self,
        self_: Resource<GpuDevice>,
        descriptor: GpuRenderPipelineDescriptor,
    ) -> wasmtime::Result<Resource<GpuRenderPipeline>> {
        let made = self.render_pipeline(&self_, descriptor)?;
        Ok(self.table.push(made)?)
    }
    fn create_command_encoder(
        &mut self,
        self_: Resource<GpuDevice>,
        descriptor: Option<GpuCommandEncoderDescriptor>,
    ) -> wasmtime::Result<Resource<GpuCommandEncoder>> {
        let label = descriptor.and_then(|d| d.label);
        let made = get(self.table, &self_)?
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: label.as_deref() });
        Ok(self.table.push(Encoder(Some(made)))?)
    }
    fn create_render_bundle_encoder(
        &mut self,
        self_: Resource<GpuDevice>,
        descriptor: GpuRenderBundleEncoderDescriptor,
    ) -> wasmtime::Result<Resource<GpuRenderBundleEncoder>> {
        let device = get(self.table, &self_)?.clone();
        Ok(self.table.push(BundleEncoder {
            device,
            color_formats: descriptor.color_formats,
            depth_stencil: descriptor.depth_stencil_format,
            depth_read_only: descriptor.depth_read_only.unwrap_or(false),
            stencil_read_only: descriptor.stencil_read_only.unwrap_or(false),
            sample_count: descriptor.sample_count.unwrap_or(1),
            commands: Vec::new(),
            done: false,
        })?)
    }
    fn create_query_set(
        &mut self,
        self_: Resource<GpuDevice>,
        descriptor: GpuQuerySetDescriptor,
    ) -> wasmtime::Result<Result<Resource<GpuQuerySet>, CreateQuerySetError>> {
        let ty = match descriptor.type_ {
            GpuQueryType::Occlusion => wgpu::QueryType::Occlusion,
            GpuQueryType::Timestamp => wgpu::QueryType::Timestamp,
        };
        let inner = get(self.table, &self_)?.create_query_set(&wgpu::QuerySetDescriptor {
            label: descriptor.label.as_deref(),
            ty,
            count: descriptor.count,
        });
        Ok(Ok(self.table.push(QuerySet { inner, kind: descriptor.type_, count: descriptor.count })?))
    }
    fn label(&mut self, self_: Resource<GpuDevice>) -> wasmtime::Result<String> {
        let _ = self_;
        Ok(String::new())
    }
    fn set_label(&mut self, self_: Resource<GpuDevice>, label: String) -> wasmtime::Result<()> {
        let _ = (self_, label);
        Ok(())
    }
    fn push_error_scope(&mut self, self_: Resource<GpuDevice>, filter: GpuErrorFilter) -> wasmtime::Result<()> {
        let filter = match filter {
            GpuErrorFilter::Validation => wgpu::ErrorFilter::Validation,
            GpuErrorFilter::OutOfMemory => wgpu::ErrorFilter::OutOfMemory,
            GpuErrorFilter::Internal => wgpu::ErrorFilter::Internal,
        };
        let scope = get(self.table, &self_)?.push_error_scope(filter);
        crate::SCOPES.with_borrow_mut(|scopes| scopes.push(scope));
        Ok(())
    }
    fn drop(&mut self, rep: Resource<GpuDevice>) -> wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl webgpu::HostGpuDeviceLostInfo for Ctx<'_> {
    fn reason(&mut self, self_: Resource<GpuDeviceLostInfo>) -> wasmtime::Result<GpuDeviceLostReason> {
        Ok(get(self.table, &self_)?.reason)
    }
    fn message(&mut self, self_: Resource<GpuDeviceLostInfo>) -> wasmtime::Result<String> {
        Ok(get(self.table, &self_)?.message.clone())
    }
    fn drop(&mut self, rep: Resource<GpuDeviceLostInfo>) -> wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl webgpu::HostGpuError for Ctx<'_> {
    fn message(&mut self, self_: Resource<GpuError>) -> wasmtime::Result<String> {
        Ok(get(self.table, &self_)?.message.clone())
    }
    fn kind(&mut self, self_: Resource<GpuError>) -> wasmtime::Result<GpuErrorKind> {
        Ok(get(self.table, &self_)?.kind)
    }
    fn drop(&mut self, rep: Resource<GpuError>) -> wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl webgpu::HostGpuPipelineLayout for Ctx<'_> {
    fn label(&mut self, self_: Resource<GpuPipelineLayout>) -> wasmtime::Result<String> {
        let _ = self_;
        Ok(String::new())
    }
    fn set_label(&mut self, self_: Resource<GpuPipelineLayout>, label: String) -> wasmtime::Result<()> {
        let _ = (self_, label);
        Ok(())
    }
    fn drop(&mut self, rep: Resource<GpuPipelineLayout>) -> wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl webgpu::HostGpuQuerySet for Ctx<'_> {
    fn destroy(&mut self, self_: Resource<GpuQuerySet>) -> wasmtime::Result<()> {
        let _ = self_;
        Ok(())
    }
    fn type_(&mut self, self_: Resource<GpuQuerySet>) -> wasmtime::Result<GpuQueryType> {
        Ok(get(self.table, &self_)?.kind)
    }
    fn count(&mut self, self_: Resource<GpuQuerySet>) -> wasmtime::Result<GpuSize32Out> {
        Ok(get(self.table, &self_)?.count)
    }
    fn label(&mut self, self_: Resource<GpuQuerySet>) -> wasmtime::Result<String> {
        let _ = self_;
        Ok(String::new())
    }
    fn set_label(&mut self, self_: Resource<GpuQuerySet>, label: String) -> wasmtime::Result<()> {
        let _ = (self_, label);
        Ok(())
    }
    fn drop(&mut self, rep: Resource<GpuQuerySet>) -> wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl<T: WasiView + 'static> webgpu::HostGpuQueueWithStore<T> for HasGpu {
    async fn on_submitted_work_done(accessor: &Accessor<T, Self>, self_: Resource<GpuQueue>) -> wasmtime::Result<()> {
        let finished = accessor.with(|mut access| -> wasmtime::Result<_> {
            let ctx = access.get();
            let (done, finished) = completion();
            get(ctx.table, &self_)?.on_submitted_work_done(move || done.complete(()));
            Ok(finished)
        })?;
        crate::poll_soon();
        finished.await;
        Ok(())
    }
}

impl webgpu::HostGpuQueue for Ctx<'_> {
    fn submit(
        &mut self,
        self_: Resource<GpuQueue>,
        command_buffers: Vec<Resource<GpuCommandBuffer>>,
    ) -> wasmtime::Result<()> {
        let mut buffers = Vec::with_capacity(command_buffers.len());
        for buffer in &command_buffers {
            buffers.push(
                self.table
                    .get_mut(buffer)?
                    .0
                    .take()
                    .ok_or_else(|| wasmtime::Error::msg("a command buffer submitted twice"))?,
            );
        }
        get(self.table, &self_)?.submit(buffers);
        Ok(())
    }
    fn write_buffer_with_copy(
        &mut self,
        self_: Resource<GpuQueue>,
        buffer: Resource<GpuBuffer>,
        buffer_offset: GpuSize64,
        data: Vec<u8>,
        data_offset: Option<GpuSize64>,
        size: Option<GpuSize64>,
    ) -> wasmtime::Result<Result<(), WriteBufferError>> {
        let bytes = window(&data, data_offset, size)?;
        get(self.table, &self_)?.write_buffer(&get(self.table, &buffer)?.inner, buffer_offset, bytes);
        Ok(Ok(()))
    }
    fn write_texture_with_copy(
        &mut self,
        self_: Resource<GpuQueue>,
        destination: GpuTexelCopyTextureInfo,
        data: Vec<u8>,
        data_layout: GpuTexelCopyBufferLayout,
        size: GpuExtent3D,
    ) -> wasmtime::Result<()> {
        let target = wgpu::TexelCopyTextureInfo {
            texture: get(self.table, &destination.texture)?,
            mip_level: destination.mip_level.unwrap_or(0),
            origin: to::origin(destination.origin),
            aspect: to::aspect(destination.aspect),
        };
        get(self.table, &self_)?.write_texture(target, &data, to::texel_layout(&data_layout), to::extent(size));
        Ok(())
    }
    fn label(&mut self, self_: Resource<GpuQueue>) -> wasmtime::Result<String> {
        let _ = self_;
        Ok(String::new())
    }
    fn set_label(&mut self, self_: Resource<GpuQueue>, label: String) -> wasmtime::Result<()> {
        let _ = (self_, label);
        Ok(())
    }
    fn drop(&mut self, rep: Resource<GpuQueue>) -> wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl webgpu::HostGpuRenderBundle for Ctx<'_> {
    fn label(&mut self, self_: Resource<GpuRenderBundle>) -> wasmtime::Result<String> {
        let _ = self_;
        Ok(String::new())
    }
    fn set_label(&mut self, self_: Resource<GpuRenderBundle>, label: String) -> wasmtime::Result<()> {
        let _ = (self_, label);
        Ok(())
    }
    fn drop(&mut self, rep: Resource<GpuRenderBundle>) -> wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl webgpu::HostGpuRenderBundleEncoder for Ctx<'_> {
    fn finish(
        &mut self,
        self_: Resource<GpuRenderBundleEncoder>,
        descriptor: Option<GpuRenderBundleDescriptor>,
    ) -> wasmtime::Result<Resource<GpuRenderBundle>> {
        let label = descriptor.and_then(|d| d.label);
        let made = self.replay(&self_, label.as_deref())?;
        Ok(self.table.push(made)?)
    }
    fn label(&mut self, self_: Resource<GpuRenderBundleEncoder>) -> wasmtime::Result<String> {
        let _ = self_;
        Ok(String::new())
    }
    fn set_label(&mut self, self_: Resource<GpuRenderBundleEncoder>, label: String) -> wasmtime::Result<()> {
        let _ = (self_, label);
        Ok(())
    }
    fn push_debug_group(
        &mut self,
        self_: Resource<GpuRenderBundleEncoder>,
        group_label: String,
    ) -> wasmtime::Result<()> {
        let _ = (self_, group_label);
        Ok(())
    }
    fn pop_debug_group(&mut self, self_: Resource<GpuRenderBundleEncoder>) -> wasmtime::Result<()> {
        let _ = self_;
        Ok(())
    }
    fn insert_debug_marker(
        &mut self,
        self_: Resource<GpuRenderBundleEncoder>,
        marker_label: String,
    ) -> wasmtime::Result<()> {
        let _ = (self_, marker_label);
        Ok(())
    }
    fn set_bind_group(
        &mut self,
        self_: Resource<GpuRenderBundleEncoder>,
        index: GpuIndex32,
        bind_group: Option<Resource<GpuBindGroup>>,
        dynamic_offsets_data: Option<Vec<GpuBufferDynamicOffset>>,
        dynamic_offsets_data_start: Option<GpuSize64>,
        dynamic_offsets_data_length: Option<GpuSize32>,
    ) -> wasmtime::Result<Result<(), SetBindGroupError>> {
        let group = bind_group.as_ref().map(|group| get(self.table, group)).transpose()?.cloned();
        let offsets = offsets_window(dynamic_offsets_data, dynamic_offsets_data_start, dynamic_offsets_data_length)?;
        self.record(&self_, BundleCommand::BindGroup(index, group, offsets))?;
        Ok(Ok(()))
    }
    fn set_immediates(
        &mut self,
        self_: Resource<GpuRenderBundleEncoder>,
        range_offset: GpuSize32,
        data: Vec<u8>,
        data_offset: Option<GpuSize64>,
        data_size: Option<GpuSize64>,
    ) -> wasmtime::Result<()> {
        let bytes = window(&data, data_offset, data_size)?.to_vec();
        self.record(&self_, BundleCommand::Immediates(range_offset, bytes))
    }
    fn set_pipeline(
        &mut self,
        self_: Resource<GpuRenderBundleEncoder>,
        pipeline: Resource<GpuRenderPipeline>,
    ) -> wasmtime::Result<()> {
        let pipeline = get(self.table, &pipeline)?.clone();
        self.record(&self_, BundleCommand::Pipeline(pipeline))
    }
    fn set_index_buffer(
        &mut self,
        self_: Resource<GpuRenderBundleEncoder>,
        buffer: Resource<GpuBuffer>,
        index_format: GpuIndexFormat,
        offset: Option<GpuSize64>,
        size: Option<GpuSize64>,
    ) -> wasmtime::Result<()> {
        let buffer = get(self.table, &buffer)?.inner.clone();
        self.record(&self_, BundleCommand::Index(buffer, index_format.into(), offset.unwrap_or(0), size))
    }
    fn set_vertex_buffer(
        &mut self,
        self_: Resource<GpuRenderBundleEncoder>,
        slot: GpuIndex32,
        buffer: Option<Resource<GpuBuffer>>,
        offset: Option<GpuSize64>,
        size: Option<GpuSize64>,
    ) -> wasmtime::Result<()> {
        let Some(buffer) = buffer.as_ref().map(|buffer| get(self.table, buffer)).transpose()?.map(|b| b.inner.clone())
        else {
            return Ok(());
        };
        self.record(&self_, BundleCommand::Vertex(slot, buffer, offset.unwrap_or(0), size))
    }
    fn draw(
        &mut self,
        self_: Resource<GpuRenderBundleEncoder>,
        vertex_count: GpuSize32,
        instance_count: Option<GpuSize32>,
        first_vertex: Option<GpuSize32>,
        first_instance: Option<GpuSize32>,
    ) -> wasmtime::Result<()> {
        let (vertex, instance) = (first_vertex.unwrap_or(0), first_instance.unwrap_or(0));
        self.record(
            &self_,
            BundleCommand::Draw(vertex..vertex + vertex_count, instance..instance + instance_count.unwrap_or(1)),
        )
    }
    fn draw_indexed(
        &mut self,
        self_: Resource<GpuRenderBundleEncoder>,
        index_count: GpuSize32,
        instance_count: Option<GpuSize32>,
        first_index: Option<GpuSize32>,
        base_vertex: Option<GpuSignedOffset32>,
        first_instance: Option<GpuSize32>,
    ) -> wasmtime::Result<()> {
        let (index, instance) = (first_index.unwrap_or(0), first_instance.unwrap_or(0));
        self.record(
            &self_,
            BundleCommand::DrawIndexed(
                index..index + index_count,
                base_vertex.unwrap_or(0),
                instance..instance + instance_count.unwrap_or(1),
            ),
        )
    }
    fn draw_indirect(
        &mut self,
        self_: Resource<GpuRenderBundleEncoder>,
        indirect_buffer: Resource<GpuBuffer>,
        indirect_offset: GpuSize64,
    ) -> wasmtime::Result<()> {
        let buffer = get(self.table, &indirect_buffer)?.inner.clone();
        self.record(&self_, BundleCommand::DrawIndirect(buffer, indirect_offset))
    }
    fn draw_indexed_indirect(
        &mut self,
        self_: Resource<GpuRenderBundleEncoder>,
        indirect_buffer: Resource<GpuBuffer>,
        indirect_offset: GpuSize64,
    ) -> wasmtime::Result<()> {
        let buffer = get(self.table, &indirect_buffer)?.inner.clone();
        self.record(&self_, BundleCommand::DrawIndexedIndirect(buffer, indirect_offset))
    }
    fn drop(&mut self, rep: Resource<GpuRenderBundleEncoder>) -> wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl webgpu::HostGpuRenderPassEncoder for Ctx<'_> {
    fn set_viewport(
        &mut self,
        self_: Resource<GpuRenderPassEncoder>,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        min_depth: f32,
        max_depth: f32,
    ) -> wasmtime::Result<()> {
        self.with_pass(&self_, |pass| pass.set_viewport(x, y, width, height, min_depth, max_depth))
    }
    fn set_scissor_rect(
        &mut self,
        self_: Resource<GpuRenderPassEncoder>,
        x: GpuIntegerCoordinate,
        y: GpuIntegerCoordinate,
        width: GpuIntegerCoordinate,
        height: GpuIntegerCoordinate,
    ) -> wasmtime::Result<()> {
        self.with_pass(&self_, |pass| pass.set_scissor_rect(x, y, width, height))
    }
    fn set_blend_constant(&mut self, self_: Resource<GpuRenderPassEncoder>, color: GpuColor) -> wasmtime::Result<()> {
        self.with_pass(&self_, |pass| pass.set_blend_constant(to::clear_color(color)))
    }
    fn set_stencil_reference(
        &mut self,
        self_: Resource<GpuRenderPassEncoder>,
        reference: GpuStencilValue,
    ) -> wasmtime::Result<()> {
        self.with_pass(&self_, |pass| pass.set_stencil_reference(reference))
    }
    fn begin_occlusion_query(
        &mut self,
        self_: Resource<GpuRenderPassEncoder>,
        query_index: GpuSize32,
    ) -> wasmtime::Result<()> {
        self.with_pass(&self_, |pass| pass.begin_occlusion_query(query_index))
    }
    fn end_occlusion_query(&mut self, self_: Resource<GpuRenderPassEncoder>) -> wasmtime::Result<()> {
        self.with_pass(&self_, wgpu::RenderPass::end_occlusion_query)
    }
    fn execute_bundles(
        &mut self,
        self_: Resource<GpuRenderPassEncoder>,
        bundles: Vec<Resource<GpuRenderBundle>>,
    ) -> wasmtime::Result<()> {
        let mut held = Vec::with_capacity(bundles.len());
        for bundle in &bundles {
            held.push(get(self.table, bundle)?.clone());
        }
        self.with_pass(&self_, |pass| pass.execute_bundles(held.iter()))
    }
    fn end(&mut self, self_: Resource<GpuRenderPassEncoder>) -> wasmtime::Result<()> {
        self.table.get_mut(&self_)?.0 = None;
        Ok(())
    }
    fn label(&mut self, self_: Resource<GpuRenderPassEncoder>) -> wasmtime::Result<String> {
        let _ = self_;
        Ok(String::new())
    }
    fn set_label(&mut self, self_: Resource<GpuRenderPassEncoder>, label: String) -> wasmtime::Result<()> {
        let _ = (self_, label);
        Ok(())
    }
    fn push_debug_group(&mut self, self_: Resource<GpuRenderPassEncoder>, group_label: String) -> wasmtime::Result<()> {
        self.with_pass(&self_, |pass| pass.push_debug_group(&group_label))
    }
    fn pop_debug_group(&mut self, self_: Resource<GpuRenderPassEncoder>) -> wasmtime::Result<()> {
        self.with_pass(&self_, wgpu::RenderPass::pop_debug_group)
    }
    fn insert_debug_marker(
        &mut self,
        self_: Resource<GpuRenderPassEncoder>,
        marker_label: String,
    ) -> wasmtime::Result<()> {
        self.with_pass(&self_, |pass| pass.insert_debug_marker(&marker_label))
    }
    fn set_bind_group(
        &mut self,
        self_: Resource<GpuRenderPassEncoder>,
        index: GpuIndex32,
        bind_group: Option<Resource<GpuBindGroup>>,
        dynamic_offsets_data: Option<Vec<GpuBufferDynamicOffset>>,
        dynamic_offsets_data_start: Option<GpuSize64>,
        dynamic_offsets_data_length: Option<GpuSize32>,
    ) -> wasmtime::Result<Result<(), SetBindGroupError>> {
        let group = bind_group.as_ref().map(|group| get(self.table, group)).transpose()?.cloned();
        let offsets = offsets_window(dynamic_offsets_data, dynamic_offsets_data_start, dynamic_offsets_data_length)?;
        self.with_pass(&self_, |pass| pass.set_bind_group(index, group.as_ref(), &offsets))?;
        Ok(Ok(()))
    }
    fn set_immediates(
        &mut self,
        self_: Resource<GpuRenderPassEncoder>,
        range_offset: GpuSize32,
        data: Vec<u8>,
        data_offset: Option<GpuSize64>,
        data_size: Option<GpuSize64>,
    ) -> wasmtime::Result<()> {
        let bytes = window(&data, data_offset, data_size)?.to_vec();
        self.with_pass(&self_, |pass| pass.set_immediates(range_offset, &bytes))
    }
    fn set_pipeline(
        &mut self,
        self_: Resource<GpuRenderPassEncoder>,
        pipeline: Resource<GpuRenderPipeline>,
    ) -> wasmtime::Result<()> {
        let pipeline = get(self.table, &pipeline)?.clone();
        self.with_pass(&self_, |pass| pass.set_pipeline(&pipeline))
    }
    fn set_index_buffer(
        &mut self,
        self_: Resource<GpuRenderPassEncoder>,
        buffer: Resource<GpuBuffer>,
        index_format: GpuIndexFormat,
        offset: Option<GpuSize64>,
        size: Option<GpuSize64>,
    ) -> wasmtime::Result<()> {
        let buffer = get(self.table, &buffer)?.inner.clone();
        let slice = sliced(&buffer, offset, size);
        self.with_pass(&self_, |pass| pass.set_index_buffer(slice, index_format.into()))
    }
    fn set_vertex_buffer(
        &mut self,
        self_: Resource<GpuRenderPassEncoder>,
        slot: GpuIndex32,
        buffer: Option<Resource<GpuBuffer>>,
        offset: Option<GpuSize64>,
        size: Option<GpuSize64>,
    ) -> wasmtime::Result<()> {
        let Some(buffer) = buffer.as_ref().map(|buffer| get(self.table, buffer)).transpose()?.map(|b| b.inner.clone())
        else {
            return Ok(());
        };
        let slice = sliced(&buffer, offset, size);
        self.with_pass(&self_, |pass| pass.set_vertex_buffer(slot, slice))
    }
    fn draw(
        &mut self,
        self_: Resource<GpuRenderPassEncoder>,
        vertex_count: GpuSize32,
        instance_count: Option<GpuSize32>,
        first_vertex: Option<GpuSize32>,
        first_instance: Option<GpuSize32>,
    ) -> wasmtime::Result<()> {
        let (first_vertex, first_instance) = (first_vertex.unwrap_or(0), first_instance.unwrap_or(0));
        let instances = first_instance..first_instance + instance_count.unwrap_or(1);
        self.with_pass(&self_, |pass| pass.draw(first_vertex..first_vertex + vertex_count, instances))
    }
    fn draw_indexed(
        &mut self,
        self_: Resource<GpuRenderPassEncoder>,
        index_count: GpuSize32,
        instance_count: Option<GpuSize32>,
        first_index: Option<GpuSize32>,
        base_vertex: Option<GpuSignedOffset32>,
        first_instance: Option<GpuSize32>,
    ) -> wasmtime::Result<()> {
        let first_index = first_index.unwrap_or(0);
        let first_instance = first_instance.unwrap_or(0);
        let instances = first_instance..first_instance + instance_count.unwrap_or(1);
        self.with_pass(&self_, |pass| {
            pass.draw_indexed(first_index..first_index + index_count, base_vertex.unwrap_or(0), instances);
        })
    }
    fn draw_indirect(
        &mut self,
        self_: Resource<GpuRenderPassEncoder>,
        indirect_buffer: Resource<GpuBuffer>,
        indirect_offset: GpuSize64,
    ) -> wasmtime::Result<()> {
        let buffer = get(self.table, &indirect_buffer)?.inner.clone();
        self.with_pass(&self_, |pass| pass.draw_indirect(&buffer, indirect_offset))
    }
    fn draw_indexed_indirect(
        &mut self,
        self_: Resource<GpuRenderPassEncoder>,
        indirect_buffer: Resource<GpuBuffer>,
        indirect_offset: GpuSize64,
    ) -> wasmtime::Result<()> {
        let buffer = get(self.table, &indirect_buffer)?.inner.clone();
        self.with_pass(&self_, |pass| pass.draw_indexed_indirect(&buffer, indirect_offset))
    }
    fn drop(&mut self, rep: Resource<GpuRenderPassEncoder>) -> wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl webgpu::HostGpuRenderPipeline for Ctx<'_> {
    fn label(&mut self, self_: Resource<GpuRenderPipeline>) -> wasmtime::Result<String> {
        let _ = self_;
        Ok(String::new())
    }
    fn set_label(&mut self, self_: Resource<GpuRenderPipeline>, label: String) -> wasmtime::Result<()> {
        let _ = (self_, label);
        Ok(())
    }
    fn get_bind_group_layout(
        &mut self,
        self_: Resource<GpuRenderPipeline>,
        index: u32,
    ) -> wasmtime::Result<Resource<GpuBindGroupLayout>> {
        let layout = get(self.table, &self_)?.get_bind_group_layout(index);
        Ok(self.table.push(layout)?)
    }
    fn drop(&mut self, rep: Resource<GpuRenderPipeline>) -> wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl webgpu::HostGpuSampler for Ctx<'_> {
    fn label(&mut self, self_: Resource<GpuSampler>) -> wasmtime::Result<String> {
        let _ = self_;
        Ok(String::new())
    }
    fn set_label(&mut self, self_: Resource<GpuSampler>, label: String) -> wasmtime::Result<()> {
        let _ = (self_, label);
        Ok(())
    }
    fn drop(&mut self, rep: Resource<GpuSampler>) -> wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl<T: WasiView + 'static> webgpu::HostGpuShaderModuleWithStore<T> for HasGpu {
    fn get_compilation_info(
        accessor: &Accessor<T, Self>,
        self_: Resource<GpuShaderModule>,
    ) -> impl ::core::future::Future<Output = wasmtime::Result<Resource<GpuCompilationInfo>>> + Send {
        std::future::ready(accessor.with(|mut access| {
            let ctx = access.get();
            let info = crate::now(get(ctx.table, &self_)?.get_compilation_info())
                .ok_or_else(|| wasmtime::Error::msg("its compilation is not finished"))?;
            let message = |m: wgpu::CompilationMessage| {
                let at = m.location;
                let kind = match m.message_type {
                    wgpu::CompilationMessageType::Error => GpuCompilationMessageType::Error,
                    wgpu::CompilationMessageType::Warning => GpuCompilationMessageType::Warning,
                    wgpu::CompilationMessageType::Info => GpuCompilationMessageType::Info,
                };
                Message {
                    text: m.message,
                    kind,
                    line: at.map_or(0, |at| u64::from(at.line_number)),
                    position: at.map_or(0, |at| u64::from(at.line_position)),
                    offset: at.map_or(0, |at| u64::from(at.offset)),
                    length: at.map_or(0, |at| u64::from(at.length)),
                }
            };
            Ok(ctx.table.push(Compilation(info.messages.into_iter().map(message).collect()))?)
        }))
    }
}

impl webgpu::HostGpuShaderModule for Ctx<'_> {
    fn label(&mut self, self_: Resource<GpuShaderModule>) -> wasmtime::Result<String> {
        let _ = self_;
        Ok(String::new())
    }
    fn set_label(&mut self, self_: Resource<GpuShaderModule>, label: String) -> wasmtime::Result<()> {
        let _ = (self_, label);
        Ok(())
    }
    fn drop(&mut self, rep: Resource<GpuShaderModule>) -> wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl webgpu::HostGpuSupportedFeatures for Ctx<'_> {
    fn has(&mut self, self_: Resource<GpuSupportedFeatures>, value: String) -> wasmtime::Result<bool> {
        let known = crate::features::named(&value);
        Ok(known.is_some_and(|wanted| {
            get(self.table, &self_).is_ok_and(|features| features.contains(crate::features::whole(wanted)))
        }))
    }
    fn drop(&mut self, rep: Resource<GpuSupportedFeatures>) -> wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl webgpu::HostGpuSupportedLimits for Ctx<'_> {
    fn max_texture_dimension1_d(&mut self, self_: Resource<GpuSupportedLimits>) -> wasmtime::Result<u32> {
        let limits = get(self.table, &self_)?;
        Ok(limits.max_texture_dimension_1d)
    }
    fn max_texture_dimension2_d(&mut self, self_: Resource<GpuSupportedLimits>) -> wasmtime::Result<u32> {
        let limits = get(self.table, &self_)?;
        Ok(limits.max_texture_dimension_2d)
    }
    fn max_texture_dimension3_d(&mut self, self_: Resource<GpuSupportedLimits>) -> wasmtime::Result<u32> {
        let limits = get(self.table, &self_)?;
        Ok(limits.max_texture_dimension_3d)
    }
    fn max_texture_array_layers(&mut self, self_: Resource<GpuSupportedLimits>) -> wasmtime::Result<u32> {
        let limits = get(self.table, &self_)?;
        Ok(limits.max_texture_array_layers)
    }
    fn max_bind_groups(&mut self, self_: Resource<GpuSupportedLimits>) -> wasmtime::Result<u32> {
        let limits = get(self.table, &self_)?;
        Ok(limits.max_bind_groups)
    }
    fn max_bind_groups_plus_vertex_buffers(&mut self, self_: Resource<GpuSupportedLimits>) -> wasmtime::Result<u32> {
        let _ = self_;
        // wgpu has no such limit: the specification's default.
        Ok(24)
    }
    fn max_immediate_size(&mut self, self_: Resource<GpuSupportedLimits>) -> wasmtime::Result<u32> {
        let limits = get(self.table, &self_)?;
        Ok(limits.max_immediate_size)
    }
    fn max_bindings_per_bind_group(&mut self, self_: Resource<GpuSupportedLimits>) -> wasmtime::Result<u32> {
        let limits = get(self.table, &self_)?;
        Ok(limits.max_bindings_per_bind_group)
    }
    fn max_dynamic_uniform_buffers_per_pipeline_layout(
        &mut self,
        self_: Resource<GpuSupportedLimits>,
    ) -> wasmtime::Result<u32> {
        let limits = get(self.table, &self_)?;
        Ok(limits.max_dynamic_uniform_buffers_per_pipeline_layout)
    }
    fn max_dynamic_storage_buffers_per_pipeline_layout(
        &mut self,
        self_: Resource<GpuSupportedLimits>,
    ) -> wasmtime::Result<u32> {
        let limits = get(self.table, &self_)?;
        Ok(limits.max_dynamic_storage_buffers_per_pipeline_layout)
    }
    fn max_sampled_textures_per_shader_stage(&mut self, self_: Resource<GpuSupportedLimits>) -> wasmtime::Result<u32> {
        let limits = get(self.table, &self_)?;
        Ok(limits.max_sampled_textures_per_shader_stage)
    }
    fn max_samplers_per_shader_stage(&mut self, self_: Resource<GpuSupportedLimits>) -> wasmtime::Result<u32> {
        let limits = get(self.table, &self_)?;
        Ok(limits.max_samplers_per_shader_stage)
    }
    fn max_storage_buffers_per_shader_stage(&mut self, self_: Resource<GpuSupportedLimits>) -> wasmtime::Result<u32> {
        let limits = get(self.table, &self_)?;
        Ok(limits.max_storage_buffers_per_shader_stage)
    }
    fn max_storage_buffers_in_vertex_stage(&mut self, self_: Resource<GpuSupportedLimits>) -> wasmtime::Result<u32> {
        Ok(get(self.table, &self_)?.max_storage_buffers_per_shader_stage)
    }
    fn max_storage_buffers_in_fragment_stage(&mut self, self_: Resource<GpuSupportedLimits>) -> wasmtime::Result<u32> {
        Ok(get(self.table, &self_)?.max_storage_buffers_per_shader_stage)
    }
    fn max_storage_textures_per_shader_stage(&mut self, self_: Resource<GpuSupportedLimits>) -> wasmtime::Result<u32> {
        let limits = get(self.table, &self_)?;
        Ok(limits.max_storage_textures_per_shader_stage)
    }
    fn max_storage_textures_in_vertex_stage(&mut self, self_: Resource<GpuSupportedLimits>) -> wasmtime::Result<u32> {
        Ok(get(self.table, &self_)?.max_storage_textures_per_shader_stage)
    }
    fn max_storage_textures_in_fragment_stage(&mut self, self_: Resource<GpuSupportedLimits>) -> wasmtime::Result<u32> {
        Ok(get(self.table, &self_)?.max_storage_textures_per_shader_stage)
    }
    fn max_uniform_buffers_per_shader_stage(&mut self, self_: Resource<GpuSupportedLimits>) -> wasmtime::Result<u32> {
        let limits = get(self.table, &self_)?;
        Ok(limits.max_uniform_buffers_per_shader_stage)
    }
    fn max_uniform_buffer_binding_size(&mut self, self_: Resource<GpuSupportedLimits>) -> wasmtime::Result<u64> {
        let limits = get(self.table, &self_)?;
        Ok(limits.max_uniform_buffer_binding_size)
    }
    fn max_storage_buffer_binding_size(&mut self, self_: Resource<GpuSupportedLimits>) -> wasmtime::Result<u64> {
        let limits = get(self.table, &self_)?;
        Ok(limits.max_storage_buffer_binding_size)
    }
    fn min_uniform_buffer_offset_alignment(&mut self, self_: Resource<GpuSupportedLimits>) -> wasmtime::Result<u32> {
        let limits = get(self.table, &self_)?;
        Ok(limits.min_uniform_buffer_offset_alignment)
    }
    fn min_storage_buffer_offset_alignment(&mut self, self_: Resource<GpuSupportedLimits>) -> wasmtime::Result<u32> {
        let limits = get(self.table, &self_)?;
        Ok(limits.min_storage_buffer_offset_alignment)
    }
    fn max_vertex_buffers(&mut self, self_: Resource<GpuSupportedLimits>) -> wasmtime::Result<u32> {
        let limits = get(self.table, &self_)?;
        Ok(limits.max_vertex_buffers)
    }
    fn max_buffer_size(&mut self, self_: Resource<GpuSupportedLimits>) -> wasmtime::Result<u64> {
        let limits = get(self.table, &self_)?;
        Ok(limits.max_buffer_size)
    }
    fn max_vertex_attributes(&mut self, self_: Resource<GpuSupportedLimits>) -> wasmtime::Result<u32> {
        let limits = get(self.table, &self_)?;
        Ok(limits.max_vertex_attributes)
    }
    fn max_vertex_buffer_array_stride(&mut self, self_: Resource<GpuSupportedLimits>) -> wasmtime::Result<u32> {
        let limits = get(self.table, &self_)?;
        Ok(limits.max_vertex_buffer_array_stride)
    }
    fn max_inter_stage_shader_variables(&mut self, self_: Resource<GpuSupportedLimits>) -> wasmtime::Result<u32> {
        let limits = get(self.table, &self_)?;
        Ok(limits.max_inter_stage_shader_variables)
    }
    fn max_color_attachments(&mut self, self_: Resource<GpuSupportedLimits>) -> wasmtime::Result<u32> {
        let limits = get(self.table, &self_)?;
        Ok(limits.max_color_attachments)
    }
    fn max_color_attachment_bytes_per_sample(&mut self, self_: Resource<GpuSupportedLimits>) -> wasmtime::Result<u32> {
        let limits = get(self.table, &self_)?;
        Ok(limits.max_color_attachment_bytes_per_sample)
    }
    fn max_compute_workgroup_storage_size(&mut self, self_: Resource<GpuSupportedLimits>) -> wasmtime::Result<u32> {
        let limits = get(self.table, &self_)?;
        Ok(limits.max_compute_workgroup_storage_size)
    }
    fn max_compute_invocations_per_workgroup(&mut self, self_: Resource<GpuSupportedLimits>) -> wasmtime::Result<u32> {
        let limits = get(self.table, &self_)?;
        Ok(limits.max_compute_invocations_per_workgroup)
    }
    fn max_compute_workgroup_size_x(&mut self, self_: Resource<GpuSupportedLimits>) -> wasmtime::Result<u32> {
        let limits = get(self.table, &self_)?;
        Ok(limits.max_compute_workgroup_size_x)
    }
    fn max_compute_workgroup_size_y(&mut self, self_: Resource<GpuSupportedLimits>) -> wasmtime::Result<u32> {
        let limits = get(self.table, &self_)?;
        Ok(limits.max_compute_workgroup_size_y)
    }
    fn max_compute_workgroup_size_z(&mut self, self_: Resource<GpuSupportedLimits>) -> wasmtime::Result<u32> {
        let limits = get(self.table, &self_)?;
        Ok(limits.max_compute_workgroup_size_z)
    }
    fn max_compute_workgroups_per_dimension(&mut self, self_: Resource<GpuSupportedLimits>) -> wasmtime::Result<u32> {
        let limits = get(self.table, &self_)?;
        Ok(limits.max_compute_workgroups_per_dimension)
    }
    fn drop(&mut self, rep: Resource<GpuSupportedLimits>) -> wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl webgpu::HostGpuTexture for Ctx<'_> {
    fn create_view(
        &mut self,
        self_: Resource<GpuTexture>,
        descriptor: Option<GpuTextureViewDescriptor>,
    ) -> wasmtime::Result<Resource<GpuTextureView>> {
        let d = descriptor.as_ref();
        let usage = d.and_then(|d| d.usage).map(TryInto::try_into).transpose()?;
        let made = get(self.table, &self_)?.create_view(&wgpu::TextureViewDescriptor {
            label: d.and_then(|d| d.label.as_deref()),
            format: d.and_then(|d| d.format).map(to::format).transpose()?,
            dimension: d.and_then(|d| d.dimension).map(Into::into),
            usage,
            aspect: to::aspect(d.and_then(|d| d.aspect)),
            base_mip_level: d.and_then(|d| d.base_mip_level).unwrap_or(0),
            mip_level_count: d.and_then(|d| d.mip_level_count),
            base_array_layer: d.and_then(|d| d.base_array_layer).unwrap_or(0),
            array_layer_count: d.and_then(|d| d.array_layer_count),
        });
        Ok(self.table.push(made)?)
    }
    fn destroy(&mut self, self_: Resource<GpuTexture>) -> wasmtime::Result<()> {
        get(self.table, &self_)?.destroy();
        Ok(())
    }
    fn width(&mut self, self_: Resource<GpuTexture>) -> wasmtime::Result<GpuIntegerCoordinateOut> {
        Ok(get(self.table, &self_)?.width())
    }
    fn height(&mut self, self_: Resource<GpuTexture>) -> wasmtime::Result<GpuIntegerCoordinateOut> {
        Ok(get(self.table, &self_)?.height())
    }
    fn depth_or_array_layers(&mut self, self_: Resource<GpuTexture>) -> wasmtime::Result<GpuIntegerCoordinateOut> {
        Ok(get(self.table, &self_)?.depth_or_array_layers())
    }
    fn mip_level_count(&mut self, self_: Resource<GpuTexture>) -> wasmtime::Result<GpuIntegerCoordinateOut> {
        Ok(get(self.table, &self_)?.mip_level_count())
    }
    fn sample_count(&mut self, self_: Resource<GpuTexture>) -> wasmtime::Result<GpuSize32Out> {
        Ok(get(self.table, &self_)?.sample_count())
    }
    fn dimension(&mut self, self_: Resource<GpuTexture>) -> wasmtime::Result<GpuTextureDimension> {
        Ok(match get(self.table, &self_)?.dimension() {
            wgpu::TextureDimension::D1 => GpuTextureDimension::D1,
            wgpu::TextureDimension::D2 => GpuTextureDimension::D2,
            wgpu::TextureDimension::D3 => GpuTextureDimension::D3,
        })
    }
    fn format(&mut self, self_: Resource<GpuTexture>) -> wasmtime::Result<GpuTextureFormat> {
        Ok(get(self.table, &self_)?.format().into())
    }
    fn usage(&mut self, self_: Resource<GpuTexture>) -> wasmtime::Result<GpuTextureUsage> {
        get(self.table, &self_)?.usage().try_into()
    }
    fn texture_binding_view_dimension(
        &mut self,
        self_: Resource<GpuTexture>,
    ) -> wasmtime::Result<Option<GpuTextureViewDimension>> {
        let _ = self_;
        Ok(None)
    }
    fn label(&mut self, self_: Resource<GpuTexture>) -> wasmtime::Result<String> {
        let _ = self_;
        Ok(String::new())
    }
    fn set_label(&mut self, self_: Resource<GpuTexture>, label: String) -> wasmtime::Result<()> {
        let _ = (self_, label);
        Ok(())
    }
    fn drop(&mut self, rep: Resource<GpuTexture>) -> wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl webgpu::HostGpuTextureView for Ctx<'_> {
    fn label(&mut self, self_: Resource<GpuTextureView>) -> wasmtime::Result<String> {
        let _ = self_;
        Ok(String::new())
    }
    fn set_label(&mut self, self_: Resource<GpuTextureView>, label: String) -> wasmtime::Result<()> {
        let _ = (self_, label);
        Ok(())
    }
    fn drop(&mut self, rep: Resource<GpuTextureView>) -> wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl webgpu::HostGpuUncapturedErrorEvent for Ctx<'_> {
    fn error(&mut self, _self_: Resource<GpuUncapturedErrorEvent>) -> wasmtime::Result<Resource<GpuError>> {
        Err(unsupported("GpuUncapturedErrorEvent.error"))
    }
    fn drop(&mut self, rep: Resource<GpuUncapturedErrorEvent>) -> wasmtime::Result<()> {
        let _ = rep;
        Ok(())
    }
}

impl webgpu::HostRecordGpuPipelineConstantValue for Ctx<'_> {
    fn new(&mut self) -> wasmtime::Result<Resource<RecordGpuPipelineConstantValue>> {
        Ok(self.table.push(HashMap::new())?)
    }
    fn add(
        &mut self,
        self_: Resource<RecordGpuPipelineConstantValue>,
        key: String,
        value: GpuPipelineConstantValue,
    ) -> wasmtime::Result<()> {
        self.table.get_mut(&self_)?.insert(key, value);
        Ok(())
    }
    fn get(
        &mut self,
        self_: Resource<RecordGpuPipelineConstantValue>,
        key: String,
    ) -> wasmtime::Result<Option<GpuPipelineConstantValue>> {
        Ok(get(self.table, &self_)?.get(&key).copied())
    }
    fn has(&mut self, self_: Resource<RecordGpuPipelineConstantValue>, key: String) -> wasmtime::Result<bool> {
        Ok(get(self.table, &self_)?.contains_key(&key))
    }
    fn remove(&mut self, self_: Resource<RecordGpuPipelineConstantValue>, key: String) -> wasmtime::Result<()> {
        self.table.get_mut(&self_)?.remove(&key);
        Ok(())
    }
    fn keys(&mut self, self_: Resource<RecordGpuPipelineConstantValue>) -> wasmtime::Result<Vec<String>> {
        Ok(get(self.table, &self_)?.keys().cloned().collect())
    }
    fn values(
        &mut self,
        self_: Resource<RecordGpuPipelineConstantValue>,
    ) -> wasmtime::Result<Vec<GpuPipelineConstantValue>> {
        Ok(get(self.table, &self_)?.values().copied().collect())
    }
    fn entries(
        &mut self,
        self_: Resource<RecordGpuPipelineConstantValue>,
    ) -> wasmtime::Result<Vec<(String, GpuPipelineConstantValue)>> {
        Ok(get(self.table, &self_)?.iter().map(|(key, value)| (key.clone(), *value)).collect())
    }
    fn drop(&mut self, rep: Resource<RecordGpuPipelineConstantValue>) -> wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl webgpu::HostRecordOptionGpuSize64 for Ctx<'_> {
    fn new(&mut self) -> wasmtime::Result<Resource<RecordOptionGpuSize64>> {
        Ok(self.table.push(HashMap::new())?)
    }
    fn add(
        &mut self,
        self_: Resource<RecordOptionGpuSize64>,
        key: String,
        value: Option<GpuSize64>,
    ) -> wasmtime::Result<()> {
        self.table.get_mut(&self_)?.insert(key, value);
        Ok(())
    }
    fn get(
        &mut self,
        self_: Resource<RecordOptionGpuSize64>,
        key: String,
    ) -> wasmtime::Result<Option<Option<GpuSize64>>> {
        Ok(get(self.table, &self_)?.get(&key).copied())
    }
    fn has(&mut self, self_: Resource<RecordOptionGpuSize64>, key: String) -> wasmtime::Result<bool> {
        Ok(get(self.table, &self_)?.contains_key(&key))
    }
    fn remove(&mut self, self_: Resource<RecordOptionGpuSize64>, key: String) -> wasmtime::Result<()> {
        self.table.get_mut(&self_)?.remove(&key);
        Ok(())
    }
    fn keys(&mut self, self_: Resource<RecordOptionGpuSize64>) -> wasmtime::Result<Vec<String>> {
        Ok(get(self.table, &self_)?.keys().cloned().collect())
    }
    fn values(&mut self, self_: Resource<RecordOptionGpuSize64>) -> wasmtime::Result<Vec<Option<GpuSize64>>> {
        Ok(get(self.table, &self_)?.values().copied().collect())
    }
    fn entries(
        &mut self,
        self_: Resource<RecordOptionGpuSize64>,
    ) -> wasmtime::Result<Vec<(String, Option<GpuSize64>)>> {
        Ok(get(self.table, &self_)?.iter().map(|(key, value)| (key.clone(), *value)).collect())
    }
    fn drop(&mut self, rep: Resource<RecordOptionGpuSize64>) -> wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl webgpu::HostWgslLanguageFeatures for Ctx<'_> {
    fn has(&mut self, self_: Resource<WgslLanguageFeatures>, value: String) -> wasmtime::Result<bool> {
        let _ = self_;
        Ok(crate::wgsl_feature(&value))
    }
    fn drop(&mut self, rep: Resource<WgslLanguageFeatures>) -> wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl webgpu::Host for Ctx<'_> {
    fn get_gpu(&mut self) -> wasmtime::Result<Resource<Gpu>> {
        Ok(self.table.push(Instance)?)
    }
}
