//! What a resource of the contract is, where wgpu's own type will not do: what ends takes itself out, so a second use is an error of its call; what the contract asks of a
//! resource that wgpu does not keep is kept here.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll, Waker};

use wasmtime::StoreContextMut;
use wasmtime::component::{Destination, FutureProducer, Resource, StreamProducer, StreamResult, VecBuffer};
use wasmtime_wasi::WasiView;

use crate::wasi::webgpu::webgpu::{GpuBufferMapState, GpuDeviceLostReason, GpuErrorKind, GpuCompilationMessageType, GpuQueryType, GpuTextureFormat};

/// `get-gpu`, which holds nothing.
pub struct Instance;

pub struct WgslFeatures;

pub struct Encoder(pub Option<wgpu::CommandEncoder>);

pub struct Recorded(pub Option<wgpu::CommandBuffer>);

pub struct Pass(pub Option<wgpu::RenderPass<'static>>);

pub struct ComputePass(pub Option<wgpu::ComputePass<'static>>);

pub struct Buffer {
    pub inner: wgpu::Buffer,
    /// wgpu does not say.
    pub state: Arc<Mutex<GpuBufferMapState>>,
}

pub struct QuerySet {
    pub inner: wgpu::QuerySet,
    pub kind: GpuQueryType,
    pub count: u32,
}

/// `gpu-error`.
pub struct Failure {
    pub kind: GpuErrorKind,
    pub message: String,
}

impl From<wgpu::Error> for Failure {
    fn from(error: wgpu::Error) -> Failure {
        let kind = match &error {
            wgpu::Error::OutOfMemory { .. } => GpuErrorKind::OutOfMemoryError,
            wgpu::Error::Validation { .. } => GpuErrorKind::ValidationError,
            wgpu::Error::Internal { .. } => GpuErrorKind::InternalError,
        };
        Failure { kind, message: error.to_string() }
    }
}

pub struct Lost {
    pub reason: GpuDeviceLostReason,
    pub message: String,
}

#[derive(Clone)]
pub struct Message {
    pub text: String,
    pub kind: GpuCompilationMessageType,
    pub line: u64,
    pub position: u64,
    pub offset: u64,
    pub length: u64,
}

pub struct Compilation(pub Vec<Message>);

pub type PipelineConstants = HashMap<String, f64>;
pub type OptionalSizes = HashMap<String, Option<u64>>;

/// A render bundle being recorded. wgpu's encoder borrows its device, which a resource cannot, so the commands are kept and replayed at `finish`.
pub struct BundleEncoder {
    pub device: wgpu::Device,
    pub color_formats: Vec<Option<GpuTextureFormat>>,
    pub depth_stencil: Option<GpuTextureFormat>,
    pub depth_read_only: bool,
    pub stencil_read_only: bool,
    pub sample_count: u32,
    pub commands: Vec<BundleCommand>,
    pub done: bool,
}

pub enum BundleCommand {
    Pipeline(wgpu::RenderPipeline),
    BindGroup(u32, Option<wgpu::BindGroup>, Vec<u32>),
    Immediates(u32, Vec<u8>),
    Vertex(u32, wgpu::Buffer, u64, Option<u64>),
    Index(wgpu::Buffer, wgpu::IndexFormat, u64, Option<u64>),
    Draw(std::ops::Range<u32>, std::ops::Range<u32>),
    DrawIndexed(std::ops::Range<u32>, i32, std::ops::Range<u32>),
    DrawIndirect(wgpu::Buffer, u64),
    DrawIndexedIndirect(wgpu::Buffer, u64),
}

/// A value that arrives later, from a thread or a callback of wgpu's, without blocking whoever awaits it.
pub struct Completion<T>(Arc<Mutex<(Option<T>, Option<Waker>)>>);

pub struct Completer<T>(Arc<Mutex<(Option<T>, Option<Waker>)>>);

#[must_use]
pub fn completion<T>() -> (Completer<T>, Completion<T>) {
    let shared = Arc::new(Mutex::new((None, None)));
    (Completer(shared.clone()), Completion(shared))
}

impl<T> Completer<T> {
    pub fn complete(self, value: T) {
        let mut shared = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        shared.0 = Some(value);
        if let Some(waker) = shared.1.take() {
            waker.wake();
        }
    }
}

impl<T> Future for Completion<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        let mut shared = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(value) = shared.0.take() {
            return Poll::Ready(value);
        }
        shared.1 = Some(cx.waker().clone());
        Poll::Pending
    }
}

/// Where what the device says of itself waits for an extension that asked to hear it.
pub struct Inbox<T> {
    pub queue: VecDeque<T>,
    pub waker: Option<Waker>,
}

impl<T> Default for Inbox<T> {
    fn default() -> Inbox<T> {
        Inbox { queue: VecDeque::new(), waker: None }
    }
}

/// `on-uncaptured-error`.
pub struct ErrorStream(pub Arc<Mutex<Inbox<Failure>>>);

impl<T: WasiView + 'static> StreamProducer<T> for ErrorStream {
    type Item = Resource<Failure>;
    type Buffer = VecBuffer<Resource<Failure>>;

    fn poll_produce<'a>(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut store: StoreContextMut<'a, T>,
        mut destination: Destination<'a, Self::Item, Self::Buffer>,
        finish: bool,
    ) -> Poll<wasmtime::Result<StreamResult>> {
        let mut inbox = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if inbox.queue.is_empty() {
            if finish {
                return Poll::Ready(Ok(StreamResult::Cancelled));
            }
            inbox.waker = Some(cx.waker().clone());
            return Poll::Pending;
        }
        let mut made = Vec::with_capacity(inbox.queue.len());
        for failure in inbox.queue.drain(..) {
            made.push(store.data_mut().ctx().table.push(failure)?);
        }
        destination.set_buffer(made.into());
        Poll::Ready(Ok(StreamResult::Completed))
    }
}

/// `lost`: pending for as long as the device lives, which is the editor's.
pub struct LostFuture(pub Arc<Mutex<Inbox<Lost>>>);

impl<T: WasiView + 'static> FutureProducer<T> for LostFuture {
    type Item = Resource<Lost>;

    fn poll_produce(self: Pin<&mut Self>, cx: &mut Context<'_>, mut store: StoreContextMut<'_, T>, finish: bool) -> Poll<wasmtime::Result<Option<Self::Item>>> {
        let mut inbox = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        match inbox.queue.pop_front() {
            Some(lost) => Poll::Ready(Ok(Some(store.data_mut().ctx().table.push(lost)?))),
            None if finish => Poll::Ready(Ok(None)),
            None => {
                inbox.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}
