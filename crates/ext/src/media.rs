//! Media as the host holds it, shaped after the `wasi:media` draft's types: frames and sound in buffers the host keeps,
//! in the formats it says it takes (`pixel_formats`, `sample_formats`). A buffer is made from bytes once; in a
//! component the bytes cross to the host then, and the value holds a handle to them, passed on without copying.

use std::fmt;
#[cfg(not(target_family = "wasm"))]
use std::sync::Arc;

/// A frame's layout. `Yuv420p`: three planes back to back - Y at full size, then Cb and Cr at half each way. `Nv12`:
/// Y, then Cb and Cr interleaved at half each way. `Rgba8888` and `Bgra8888`: one plane, row by row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    Rgba8888,
    Bgra8888,
    Yuv420p,
    Nv12,
}

impl PixelFormat {
    /// Its name, as a stream's header says it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            PixelFormat::Rgba8888 => "rgba8888",
            PixelFormat::Bgra8888 => "bgra8888",
            PixelFormat::Yuv420p => "yuv420p",
            PixelFormat::Nv12 => "nv12",
        }
    }

    /// One by its name.
    #[must_use]
    pub fn named(name: &str) -> Option<PixelFormat> {
        [PixelFormat::Rgba8888, PixelFormat::Bgra8888, PixelFormat::Yuv420p, PixelFormat::Nv12]
            .into_iter()
            .find(|f| f.name() == name)
    }

    /// The bytes a frame of a size takes.
    #[must_use]
    pub fn size(self, width: u32, height: u32) -> u64 {
        let (w, h) = (u64::from(width), u64::from(height));
        match self {
            PixelFormat::Rgba8888 | PixelFormat::Bgra8888 => w * h * 4,
            PixelFormat::Yuv420p | PixelFormat::Nv12 => w * h + 2 * w.div_ceil(2) * h.div_ceil(2),
        }
    }
}

/// A stretch of sound's layout: `S16Interleaved`, 16-bit little-endian samples, the channels' interleaved;
/// `F32Planar`, 32-bit float samples, each channel's in turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleFormat {
    S16Interleaved,
    F32Planar,
}

impl SampleFormat {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            SampleFormat::S16Interleaved => "s16-interleaved",
            SampleFormat::F32Planar => "f32-planar",
        }
    }

    #[must_use]
    pub fn named(name: &str) -> Option<SampleFormat> {
        [SampleFormat::S16Interleaved, SampleFormat::F32Planar].into_iter().find(|f| f.name() == name)
    }
}

/// Bytes the host holds: a frame's planes, a stretch of sound. In the host, the bytes; in a component, a handle to
/// them.
#[derive(Clone)]
pub struct MediaBuffer {
    size: u64,
    #[cfg(not(target_family = "wasm"))]
    bytes: Arc<Vec<u8>>,
    #[cfg(target_family = "wasm")]
    handle: std::rc::Rc<crate::guest::bindings::pe2::extension::media::MediaBuffer>,
}

impl MediaBuffer {
    /// A buffer of these bytes: in a component, handed to the host now.
    #[must_use]
    #[cfg_attr(target_family = "wasm", expect(clippy::needless_pass_by_value, reason = "the host keeps them as given"))]
    pub fn new(bytes: Vec<u8>) -> MediaBuffer {
        let size = bytes.len() as u64;
        #[cfg(not(target_family = "wasm"))]
        return MediaBuffer { size, bytes: Arc::new(bytes) };
        #[cfg(target_family = "wasm")]
        {
            let handle = crate::guest::bindings::pe2::extension::media::MediaBuffer::new(&bytes);
            MediaBuffer { size, handle: std::rc::Rc::new(handle) }
        }
    }

    #[must_use]
    pub fn size(&self) -> u64 {
        self.size
    }

    /// A copy of its bytes.
    #[must_use]
    pub fn read(&self) -> Vec<u8> {
        #[cfg(not(target_family = "wasm"))]
        return self.bytes.to_vec();
        #[cfg(target_family = "wasm")]
        self.handle.bytes()
    }

    /// A buffer of bytes the host holds already.
    #[cfg(not(target_family = "wasm"))]
    #[must_use]
    pub fn shared(bytes: Arc<Vec<u8>>) -> MediaBuffer {
        MediaBuffer { size: bytes.len() as u64, bytes }
    }

    /// Its bytes, where the host holds them.
    #[cfg(not(target_family = "wasm"))]
    #[must_use]
    pub fn bytes(&self) -> &Arc<Vec<u8>> {
        &self.bytes
    }

    #[cfg(target_family = "wasm")]
    pub(crate) fn from_host(handle: crate::guest::bindings::pe2::extension::media::MediaBuffer) -> MediaBuffer {
        MediaBuffer { size: handle.size(), handle: std::rc::Rc::new(handle) }
    }

    /// The handle the host knows it by.
    #[cfg(target_family = "wasm")]
    pub(crate) fn handle(&self) -> &crate::guest::bindings::pe2::extension::media::MediaBuffer {
        &self.handle
    }

    /// The handle to give the host: this one where nothing else holds it, else another to the same bytes.
    #[cfg(target_family = "wasm")]
    pub(crate) fn into_host(self) -> crate::guest::bindings::pe2::extension::media::MediaBuffer {
        std::rc::Rc::try_unwrap(self.handle).unwrap_or_else(|shared| shared.share())
    }
}

impl fmt::Debug for MediaBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MediaBuffer({} bytes)", self.size)
    }
}

impl PartialEq for MediaBuffer {
    fn eq(&self, other: &MediaBuffer) -> bool {
        #[cfg(not(target_family = "wasm"))]
        return Arc::ptr_eq(&self.bytes, &other.bytes) || self.bytes == other.bytes;
        #[cfg(target_family = "wasm")]
        std::rc::Rc::ptr_eq(&self.handle, &other.handle)
    }
}

/// A frame: its buffer, its layout and size, when it is shown (in microseconds from its stream's start).
#[derive(Clone, Debug, PartialEq)]
pub struct VideoFrame {
    pub buffer: MediaBuffer,
    pub format: PixelFormat,
    pub width: u32,
    pub height: u32,
    pub timestamp: u64,
}

impl VideoFrame {
    /// Its picture as RGBA, row by row: YCbCr as the decoder of the game's movies turns it into RGB (BT.601, the chroma
    /// offset by 128, each chroma sample covering four pixels), as the player's shader does.
    ///
    /// # Errors
    /// Its layout is one this cannot read, or its buffer is short.
    pub fn rgba(&self) -> crate::Result<Vec<u8>> {
        let (width, height) = (self.width as usize, self.height as usize);
        let bytes = self.buffer.read();
        let size = usize::try_from(self.format.size(self.width, self.height))?;
        if bytes.len() < size {
            crate::bail!("a frame of {} bytes, not {size}", bytes.len());
        }
        match self.format {
            PixelFormat::Rgba8888 => Ok(bytes),
            PixelFormat::Yuv420p => {
                let (half_w, half_h) = (width.div_ceil(2), height.div_ceil(2));
                let (y, chroma) = bytes.split_at(width * height);
                let (cb, cr) = chroma.split_at(half_w * half_h);
                let mut rgba = Vec::with_capacity(width * height * 4);
                for row in 0..height {
                    for column in 0..width {
                        let at = row / 2 * half_w + column / 2;
                        let l = f64::from(y[row * width + column]);
                        let (b, r) = (f64::from(cb[at]) - 128.0, f64::from(cr[at]) - 128.0);
                        rgba.extend([
                            channel(l + 1.402 * r),
                            channel(l - 0.3437 * b - 0.7143 * r),
                            channel(l + 1.772 * b),
                            255,
                        ]);
                    }
                }
                Ok(rgba)
            }
            other => crate::bail!("a frame in {} cannot be read", other.name()),
        }
    }
}

#[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped to 0..=255 first")]
fn channel(value: f64) -> u8 {
    value.clamp(0.0, 255.0) as u8
}

/// A stretch of sound: its buffer, its layout, rate and channels, when it begins (in microseconds).
#[derive(Clone, Debug, PartialEq)]
pub struct AudioData {
    pub buffer: MediaBuffer,
    pub format: SampleFormat,
    pub sample_rate: u32,
    pub channels: u32,
    pub timestamp: u64,
}

/// The frame layouts the host takes.
#[must_use]
pub fn pixel_formats() -> Vec<PixelFormat> {
    #[cfg(target_family = "wasm")]
    return crate::guest::pixel_formats();
    #[cfg(not(target_family = "wasm"))]
    vec![PixelFormat::Rgba8888, PixelFormat::Yuv420p]
}

/// The sound layouts the host takes.
#[must_use]
pub fn sample_formats() -> Vec<SampleFormat> {
    #[cfg(target_family = "wasm")]
    return crate::guest::sample_formats();
    #[cfg(not(target_family = "wasm"))]
    vec![SampleFormat::S16Interleaved]
}

#[cfg(test)]
mod tests {
    use super::{MediaBuffer, PixelFormat, VideoFrame};

    fn frame(format: PixelFormat, bytes: Vec<u8>) -> VideoFrame {
        VideoFrame { buffer: MediaBuffer::new(bytes), format, width: 2, height: 2, timestamp: 0 }
    }

    #[test]
    fn a_frame_reads_as_rgba() {
        // Grey where the chroma is neutral, the luma's own value in each channel.
        let grey = frame(PixelFormat::Yuv420p, vec![10, 20, 30, 40, 128, 128]);
        assert_eq!(grey.rgba().unwrap(), [10, 10, 10, 255, 20, 20, 20, 255, 30, 30, 30, 255, 40, 40, 40, 255]);
        // Red chroma, clamped.
        let red = frame(PixelFormat::Yuv420p, vec![200; 4].into_iter().chain([128, 255]).collect());
        assert_eq!(&red.rgba().unwrap()[..4], [255, 109, 200, 255]);
        let plain = frame(PixelFormat::Rgba8888, vec![1; 16]);
        assert_eq!(plain.rgba().unwrap(), vec![1; 16]);
        assert!(frame(PixelFormat::Yuv420p, vec![0; 3]).rgba().is_err());
        assert!(frame(PixelFormat::Nv12, vec![0; 6]).rgba().is_err());
    }
}
