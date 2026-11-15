//! Bytes the host holds, passed by handle (`buffers`): a content in memory, a frame's planes, a stretch of sound. In the
//! host, the bytes; in a component, a handle to them, whose bytes are copied in only as they are read.

use std::fmt;
#[cfg(not(target_family = "wasm"))]
use std::sync::Arc;

#[cfg(target_family = "wasm")]
type Handle = crate::guest::bindings::pe2::extension::buffers::Buffer;

/// Bytes the host holds. Made of bytes, in a component they are handed to the host at once: the one copy out.
#[derive(Clone)]
pub struct Buffer {
    size: u64,
    #[cfg(not(target_family = "wasm"))]
    bytes: Arc<[u8]>,
    #[cfg(target_family = "wasm")]
    handle: std::sync::Arc<Handle>,
}

impl Buffer {
    #[must_use]
    #[cfg_attr(target_family = "wasm", expect(clippy::needless_pass_by_value, reason = "the host keeps them as given"))]
    pub fn new(bytes: Vec<u8>) -> Buffer {
        let size = bytes.len() as u64;
        #[cfg(not(target_family = "wasm"))]
        return Buffer { size, bytes: Arc::from(bytes) };
        #[cfg(target_family = "wasm")]
        Buffer { size, handle: std::sync::Arc::new(Handle::new(&bytes)) }
    }

    #[must_use]
    pub fn size(&self) -> u64 {
        self.size
    }

    /// A copy of all of its bytes.
    #[must_use]
    pub fn read(&self) -> Box<[u8]> {
        #[cfg(not(target_family = "wasm"))]
        return Box::from(&*self.bytes);
        #[cfg(target_family = "wasm")]
        self.handle.bytes().into_boxed_slice()
    }

    /// Up to `len` bytes from `offset`, fewer at its end: in a component, only those copied in.
    #[must_use]
    pub fn read_range(&self, offset: u64, len: usize) -> Box<[u8]> {
        #[cfg(not(target_family = "wasm"))]
        {
            let start = usize::try_from(offset).unwrap_or(usize::MAX).min(self.bytes.len());
            self.bytes[start..start.saturating_add(len).min(self.bytes.len())].into()
        }
        #[cfg(target_family = "wasm")]
        self.handle.read(offset, u32::try_from(len).unwrap_or(u32::MAX)).into_boxed_slice()
    }

    /// Bytes the host holds already, shared.
    #[cfg(not(target_family = "wasm"))]
    #[must_use]
    pub fn shared(bytes: Arc<[u8]>) -> Buffer {
        Buffer { size: bytes.len() as u64, bytes }
    }

    /// Its bytes, where the host holds them.
    #[cfg(not(target_family = "wasm"))]
    #[must_use]
    pub fn bytes(&self) -> &Arc<[u8]> {
        &self.bytes
    }

    #[cfg(target_family = "wasm")]
    pub(crate) fn from_host(handle: Handle) -> Buffer {
        Buffer { size: handle.size(), handle: std::sync::Arc::new(handle) }
    }

    /// The handle the host knows it by.
    #[cfg(target_family = "wasm")]
    pub(crate) fn handle(&self) -> &Handle {
        &self.handle
    }

    /// The handle to give the host: this one where nothing else holds it, else another to the same bytes.
    #[cfg(target_family = "wasm")]
    pub(crate) fn into_host(self) -> Handle {
        std::sync::Arc::try_unwrap(self.handle).unwrap_or_else(|shared| shared.share())
    }
}

impl fmt::Debug for Buffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Buffer({} bytes)", self.size)
    }
}

impl PartialEq for Buffer {
    fn eq(&self, other: &Buffer) -> bool {
        #[cfg(not(target_family = "wasm"))]
        return Arc::ptr_eq(&self.bytes, &other.bytes) || self.bytes == other.bytes;
        #[cfg(target_family = "wasm")]
        std::sync::Arc::ptr_eq(&self.handle, &other.handle)
    }
}
