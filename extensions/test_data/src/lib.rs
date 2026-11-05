//! What test extensions make their nodes from.

use codecs::ora::{Document, Group, Layer, Node, write};
use codecs::texture::rgba_png;

/// A picture as a tool would save one: an `.ora` of 16 colors, a texel layer and a cell.
///
/// # Panics
/// A PNG this small cannot be written.
#[must_use]
pub fn painting() -> Vec<u8> {
    let layer = |name: &str, png: &[u8]| {
        Node::Layer(Layer { name: name.to_string(), x: 0, y: 0, visible: true, png: png.to_vec() })
    };
    let group = |name: &str, children| Node::Group(Group { name: name.to_string(), visible: true, children });
    // Each channel full or empty, as those are exact in the 5 bits a color has.
    let full = |on: bool| if on { 255u8 } else { 0 };
    let colors: Vec<u8> = (0u8..16)
        .map(|at| at % 7 + 1)
        .flat_map(|c| [full(c & 1 != 0), full(c & 2 != 0), full(c & 4 != 0), 255])
        .collect();
    let strip = rgba_png(16, 1, &colors).expect("a strip");
    let texels: Vec<u8> = (0..64).flat_map(|at: usize| colors[at % 16 * 4..at % 16 * 4 + 4].to_vec()).collect();
    let picture = rgba_png(8, 8, &texels).expect("a picture");
    let stack = vec![
        group("palettes", vec![layer("default", &strip)]),
        group("cells", vec![layer("page_0", &picture)]),
        layer("texels", &picture),
    ];
    write(&Document { width: 8, height: 8, stack }, &picture, &picture)
}
