//! Pictures as extensions make them and the host shows them.

#[cfg(not(target_family = "wasm"))]
use std::sync::Arc;

/// Pixels as unmultiplied RGBA, row by row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Image {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

impl Image {
    /// The image at most `side` pixels on its longer side: each pixel the average of the block it stands for, by
    /// coverage (a transparent pixel adds nothing to the color). One already that small is kept as it is.
    #[must_use]
    pub fn thumbnail(&self, side: usize) -> Image {
        let longest = self.width.max(self.height);
        if longest <= side || self.width == 0 || self.height == 0 {
            return self.clone();
        }
        let (width, height) =
            ((self.width * side).div_ceil(longest).max(1), (self.height * side).div_ceil(longest).max(1));
        let mut rgba = Vec::with_capacity(width * height * 4);
        for y in 0..height {
            let (y0, y1) =
                (y * self.height / height, ((y + 1) * self.height / height).max(y * self.height / height + 1));
            for x in 0..width {
                let (x0, x1) = (x * self.width / width, ((x + 1) * self.width / width).max(x * self.width / width + 1));
                let mut sum = [0u64; 4];
                for sy in y0..y1.min(self.height) {
                    for sx in x0..x1.min(self.width) {
                        let at = (sy * self.width + sx) * 4;
                        let alpha = u64::from(self.rgba[at + 3]);
                        for c in 0..3 {
                            sum[c] += u64::from(self.rgba[at + c]) * alpha;
                        }
                        sum[3] += alpha;
                    }
                }
                let count = ((y1.min(self.height) - y0) * (x1.min(self.width) - x0)).max(1) as u64;
                #[expect(clippy::cast_possible_truncation, reason = "averages of bytes are bytes")]
                let channel = |c: usize| sum[c].checked_div(sum[3]).unwrap_or(0) as u8;
                #[expect(clippy::cast_possible_truncation, reason = "an average of bytes is a byte")]
                rgba.extend_from_slice(&[channel(0), channel(1), channel(2), (sum[3] / count) as u8]);
            }
        }
        Image { width, height, rgba }
    }
}

/// A rectangle, in an image's pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// An image a view shows, by a key the host keeps its upload under: the same key with the same image is uploaded
/// once, a new image under it replaces the old. In a component the host holds the image, made once and shown by
/// handle.
#[derive(Clone)]
pub struct Texture {
    key: String,
    width: usize,
    height: usize,
    #[cfg(not(target_family = "wasm"))]
    image: Arc<Image>,
    #[cfg(target_family = "wasm")]
    handle: std::rc::Rc<crate::guest::bindings::pe2::extension::host::Texture>,
}

impl Texture {
    #[must_use]
    pub fn new(key: impl Into<String>, image: Image) -> Texture {
        let (key, width, height) = (key.into(), image.width, image.height);
        #[cfg(not(target_family = "wasm"))]
        return Texture { key, width, height, image: Arc::new(image) };
        #[cfg(target_family = "wasm")]
        {
            let handle = crate::guest::texture(&key, image);
            Texture { key, width, height, handle: std::rc::Rc::new(handle) }
        }
    }

    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    #[must_use]
    pub fn width(&self) -> usize {
        self.width
    }

    #[must_use]
    pub fn height(&self) -> usize {
        self.height
    }

    /// An image the host holds already, under a key: a component's texture.
    #[cfg(not(target_family = "wasm"))]
    #[must_use]
    pub fn shared(key: impl Into<String>, image: Arc<Image>) -> Texture {
        Texture { key: key.into(), width: image.width, height: image.height, image }
    }

    #[cfg(not(target_family = "wasm"))]
    #[must_use]
    pub fn image(&self) -> &Arc<Image> {
        &self.image
    }

    #[cfg(target_family = "wasm")]
    pub(crate) fn handle(&self) -> &crate::guest::bindings::pe2::extension::host::Texture {
        &self.handle
    }
}
