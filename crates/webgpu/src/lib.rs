//! wasi:webgpu for extensions, on the device the editor draws with, so that what one renders is shown without a copy. Its own crate, as the editor is what links it: the command line
//! and the MCP server stay without a GPU, where a call traps.

#![recursion_limit = "1024"]

mod conversions;
mod enums;
mod features;
mod flags;
mod host;
mod types;

use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::task::{Context, Poll, Waker};

use wasmtime::component::{Linker, Resource};
use wasmtime_wasi::WasiView;

pub use host::{Ctx, HasGpu};
use types::{Failure, Inbox, Lost};
use wasi::webgpu::webgpu::GpuDeviceLostReason;

wasmtime::component::bindgen!({
    world: "wasi:webgpu/imports@0.3.0-rc.2",
    path: "../ext/wit",
    imports: {
        "wasi:webgpu/webgpu.[method]gpu-device.lost": store | trappable,
        "wasi:webgpu/webgpu.[method]gpu-device.on-uncaptured-error": store | trappable,
        default: trappable,
    },
    with: {
        "wasi:webgpu/webgpu.gpu": types::Instance,
        "wasi:webgpu/webgpu.wgsl-language-features": types::WgslFeatures,
        "wasi:webgpu/webgpu.gpu-adapter": wgpu::Adapter,
        "wasi:webgpu/webgpu.gpu-adapter-info": wgpu::AdapterInfo,
        "wasi:webgpu/webgpu.gpu-supported-features": wgpu::Features,
        "wasi:webgpu/webgpu.gpu-supported-limits": wgpu::Limits,
        "wasi:webgpu/webgpu.gpu-device": wgpu::Device,
        "wasi:webgpu/webgpu.gpu-queue": wgpu::Queue,
        "wasi:webgpu/webgpu.gpu-buffer": types::Buffer,
        "wasi:webgpu/webgpu.gpu-texture": wgpu::Texture,
        "wasi:webgpu/webgpu.gpu-texture-view": wgpu::TextureView,
        "wasi:webgpu/webgpu.gpu-sampler": wgpu::Sampler,
        "wasi:webgpu/webgpu.gpu-shader-module": wgpu::ShaderModule,
        "wasi:webgpu/webgpu.gpu-bind-group": wgpu::BindGroup,
        "wasi:webgpu/webgpu.gpu-bind-group-layout": wgpu::BindGroupLayout,
        "wasi:webgpu/webgpu.gpu-pipeline-layout": wgpu::PipelineLayout,
        "wasi:webgpu/webgpu.gpu-render-pipeline": wgpu::RenderPipeline,
        "wasi:webgpu/webgpu.gpu-compute-pipeline": wgpu::ComputePipeline,
        "wasi:webgpu/webgpu.gpu-command-encoder": types::Encoder,
        "wasi:webgpu/webgpu.gpu-command-buffer": types::Recorded,
        "wasi:webgpu/webgpu.gpu-render-pass-encoder": types::Pass,
        "wasi:webgpu/webgpu.gpu-compute-pass-encoder": types::ComputePass,
        "wasi:webgpu/webgpu.gpu-render-bundle": wgpu::RenderBundle,
        "wasi:webgpu/webgpu.gpu-render-bundle-encoder": types::BundleEncoder,
        "wasi:webgpu/webgpu.gpu-query-set": types::QuerySet,
        "wasi:webgpu/webgpu.gpu-error": types::Failure,
        "wasi:webgpu/webgpu.gpu-device-lost-info": types::Lost,
        "wasi:webgpu/webgpu.gpu-compilation-info": types::Compilation,
        "wasi:webgpu/webgpu.gpu-compilation-message": types::Message,
        "wasi:webgpu/webgpu.record-gpu-pipeline-constant-value": types::PipelineConstants,
        "wasi:webgpu/webgpu.record-option-gpu-size64": types::OptionalSizes,
    },
});

/// What the window draws with; none where nothing draws.
static DEVICE: OnceLock<Shared> = OnceLock::new();
/// What extensions published, for the window to show, by id.
static PUBLISHED: Mutex<Option<HashMap<u64, wgpu::Texture>>> = Mutex::new(None);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
/// The errors the device reported of what extensions gave it, last first.
static ERRORS: Mutex<Vec<String>> = Mutex::new(Vec::new());

struct Shared {
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    wgsl: wgpu::WgslLanguageFeatures,
}

/// Those who asked to hear of errors and of the device's loss.
static ERROR_INBOXES: Mutex<Vec<Arc<Mutex<Inbox<Failure>>>>> = Mutex::new(Vec::new());
static LOST_INBOXES: Mutex<Vec<Arc<Mutex<Inbox<Lost>>>>> = Mutex::new(Vec::new());

thread_local! {
    /// wgpu keeps error scopes by thread, and what guards one is bound to it.
    pub(crate) static SCOPES: RefCell<Vec<wgpu::ErrorScopeGuard>> = const { RefCell::new(Vec::new()) };
}

/// Share the window's device with extensions. An error of theirs is theirs: wgpu's default answer to one is to panic, which would take the editor with it.
pub fn install(instance: &wgpu::Instance, adapter: wgpu::Adapter, device: wgpu::Device, queue: wgpu::Queue) {
    device.on_uncaptured_error(Arc::new(|error: wgpu::Error| {
        eprintln!("wgpu: {error}");
        let failure = Failure::from(error);
        ERRORS.lock().unwrap_or_else(PoisonError::into_inner).push(failure.message.clone());
        for inbox in ERROR_INBOXES.lock().unwrap_or_else(PoisonError::into_inner).iter() {
            let mut inbox = inbox.lock().unwrap_or_else(PoisonError::into_inner);
            inbox.queue.push_back(Failure { kind: failure.kind, message: failure.message.clone() });
            if let Some(waker) = inbox.waker.take() {
                waker.wake();
            }
        }
    }));
    device.set_device_lost_callback(|reason, message| {
        eprintln!("wgpu: the device is lost: {message}");
        let reason = if matches!(reason, wgpu::DeviceLostReason::Destroyed) { GpuDeviceLostReason::Destroyed } else { GpuDeviceLostReason::Unknown };
        for inbox in LOST_INBOXES.lock().unwrap_or_else(PoisonError::into_inner).iter() {
            let mut inbox = inbox.lock().unwrap_or_else(PoisonError::into_inner);
            inbox.queue.push_back(Lost { reason, message: message.clone() });
            if let Some(waker) = inbox.waker.take() {
                waker.wake();
            }
        }
    });
    let _ = DEVICE.set(Shared { adapter, device, queue, wgsl: instance.wgsl_language_features() });
}

pub(crate) fn error_inbox() -> Arc<Mutex<Inbox<Failure>>> {
    let inbox = Arc::default();
    ERROR_INBOXES.lock().unwrap_or_else(PoisonError::into_inner).push(Arc::clone(&inbox));
    inbox
}

pub(crate) fn lost_inbox() -> Arc<Mutex<Inbox<Lost>>> {
    let inbox = Arc::default();
    LOST_INBOXES.lock().unwrap_or_else(PoisonError::into_inner).push(Arc::clone(&inbox));
    inbox
}

/// Have what waits on the device finish: on a thread of its own, so that nothing waits here.
pub(crate) fn poll_soon() {
    let Some(shared) = DEVICE.get() else { return };
    let device = shared.device.clone();
    let _ = std::thread::Builder::new().name("gpu poll".into()).spawn(move || device.poll(wgpu::PollType::wait_indefinitely()));
}

/// What a future wgpu gives comes to, where it has already: native ones have.
pub(crate) fn now<T>(future: impl Future<Output = T>) -> Option<T> {
    match std::pin::pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(value) => Some(value),
        Poll::Pending => None,
    }
}

pub(crate) fn wgsl_feature(name: &str) -> bool {
    let known = match name {
        "readonly_and_readwrite_storage_textures" => wgpu::WgslLanguageFeatures::ReadOnlyAndReadWriteStorageTextures,
        "packed_4x8_integer_dot_product" => wgpu::WgslLanguageFeatures::Packed4x8IntegerDotProduct,
        "unrestricted_pointer_parameters" => wgpu::WgslLanguageFeatures::UnrestrictedPointerParameters,
        "pointer_composite_access" => wgpu::WgslLanguageFeatures::PointerCompositeAccess,
        _ => return false,
    };
    DEVICE.get().is_some_and(|shared| shared.wgsl.contains(known))
}

/// What the device reported since this was last asked.
#[must_use]
pub fn take_errors() -> Vec<String> {
    std::mem::take(&mut *ERRORS.lock().unwrap_or_else(PoisonError::into_inner))
}

/// A texture an extension published.
#[must_use]
pub fn published(id: u64) -> Option<wgpu::Texture> {
    PUBLISHED.lock().unwrap_or_else(PoisonError::into_inner).as_ref()?.get(&id).cloned()
}

pub(crate) fn device() -> Option<&'static wgpu::Device> {
    DEVICE.get().map(|shared| &shared.device)
}

pub(crate) fn adapter() -> Option<&'static wgpu::Adapter> {
    DEVICE.get().map(|shared| &shared.adapter)
}

pub(crate) fn queue() -> wasmtime::Result<&'static wgpu::Queue> {
    DEVICE.get().map(|shared| &shared.queue).ok_or_else(|| wasmtime::Error::msg("no device"))
}

/// A texture made known to the window: by its identity, so publishing it again gives the same id.
fn publish(texture: &wgpu::Texture) -> u64 {
    let mut published = PUBLISHED.lock().unwrap_or_else(PoisonError::into_inner);
    let published = published.get_or_insert_with(HashMap::new);
    if let Some((id, _)) = published.iter().find(|(_, known)| *known == texture) {
        return *id;
    }
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    published.insert(id, texture.clone());
    id
}

/// `wasi:webgpu` and the `gpu` interface of the extension world, linked by hand as it is no part of what bindgen sees elsewhere.
///
/// # Errors
/// The linker holds them already.
pub fn add_to_linker<T: WasiView + 'static>(linker: &mut Linker<T>) -> wasmtime::Result<()> {
    wasi::webgpu::webgpu::add_to_linker::<T, HasGpu>(linker, |state| Ctx { table: state.ctx().table })?;
    let mut gpu = linker.instance("pe2:extension/gpu@0.1.0")?;
    gpu.func_wrap("device", |mut store, (): ()| {
        let made = device().cloned().map(|device| store.data_mut().ctx().table.push(device)).transpose()?;
        Ok((made,))
    })?;
    gpu.func_wrap("publish", |mut store, (texture,): (Resource<wgpu::Texture>,)| {
        let known = store.data_mut().ctx().table.get(&texture)?;
        Ok((publish(known),))
    })?;
    gpu.func_wrap("unpublish", |_, (id,): (u64,)| {
        if let Some(published) = PUBLISHED.lock().unwrap_or_else(PoisonError::into_inner).as_mut() {
            published.remove(&id);
        }
        Ok(())
    })?;
    Ok(())
}
