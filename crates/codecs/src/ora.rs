//! `OpenRaster` (`.ora`): a zip of PNG layers and a `stack.xml` placing them, which painting programs (Krita, `GIMP`,
//! `MyPaint`) open and save. Read and written here as a tree of layers and groups with their names, places and
//! visibility; what the layers mean is their reader's business. Written deterministically - the same document is the
//! same bytes - with `mergedimage.png` and a thumbnail, as the format asks.

use std::fmt::{self, Write as _};
use std::io::Cursor;

use crate::zip;

pub const MIMETYPE: &str = "image/openraster";

/// Why a document could not be read or written.
#[derive(Debug)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<zip::Error> for Error {
    fn from(e: zip::Error) -> Error {
        Error(e.to_string())
    }
}

/// A document: its size and its stack, topmost first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Document {
    pub width: u32,
    pub height: u32,
    pub stack: Vec<Node>,
}

/// A layer, or a group of them.
#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Layer(Layer),
    Group(Group),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    pub name: String,
    /// Where its top-left corner is, in the document.
    pub x: i32,
    pub y: i32,
    pub visible: bool,
    /// Its pixels, as a PNG.
    pub png: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub name: String,
    pub visible: bool,
    /// Topmost first.
    pub children: Vec<Node>,
}

impl Document {
    /// A group at the top of the stack, by name.
    #[must_use]
    pub fn group(&self, name: &str) -> Option<&Group> {
        self.stack.iter().find_map(|n| match n {
            Node::Group(g) if g.name == name => Some(g),
            _ => None,
        })
    }

    /// A layer at the top of the stack, by name.
    #[must_use]
    pub fn layer(&self, name: &str) -> Option<&Layer> {
        layers(&self.stack).find(|l| l.name == name)
    }
}

impl Group {
    /// A group within it, by name.
    #[must_use]
    pub fn group(&self, name: &str) -> Option<&Group> {
        self.children.iter().find_map(|n| match n {
            Node::Group(g) if g.name == name => Some(g),
            _ => None,
        })
    }

    /// Its layers (not those of groups within it), topmost first.
    pub fn layers(&self) -> impl Iterator<Item = &Layer> {
        layers(&self.children)
    }
}

fn layers(nodes: &[Node]) -> impl Iterator<Item = &Layer> {
    nodes.iter().filter_map(|n| match n {
        Node::Layer(l) => Some(l),
        Node::Group(_) => None,
    })
}

/// A document as an archive, with the merged image (`mergedimage.png`) and a thumbnail of at most 256 pixels a side,
/// both PNGs.
#[must_use]
pub fn write(document: &Document, merged: &[u8], thumbnail: &[u8]) -> Vec<u8> {
    let mut xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<image version=\"0.0.6\" w=\"{}\" h=\"{}\" xres=\"72\" yres=\"72\">\n",
        document.width, document.height
    );
    let mut files: Vec<(String, &[u8])> = Vec::new();
    write_stack(&mut xml, &mut files, None, &document.stack, 1);
    xml.push_str("</image>\n");
    let mut entries = vec![
        zip::Entry { name: "mimetype", data: MIMETYPE.as_bytes(), deflate: false },
        zip::Entry { name: "stack.xml", data: xml.as_bytes(), deflate: true },
    ];
    // PNGs are compressed already: stored.
    entries.extend(files.iter().map(|(name, data)| zip::Entry { name, data, deflate: false }));
    entries.push(zip::Entry { name: "mergedimage.png", data: merged, deflate: false });
    entries.push(zip::Entry { name: "Thumbnails/thumbnail.png", data: thumbnail, deflate: false });
    zip::write(&entries)
}

fn write_stack<'a>(
    xml: &mut String,
    files: &mut Vec<(String, &'a [u8])>,
    group: Option<&Group>,
    nodes: &'a [Node],
    depth: usize,
) {
    let indent = " ".repeat(depth);
    match group {
        Some(g) => {
            let _ = writeln!(xml, "{indent}<stack name=\"{}\"{}>", escape(&g.name), hidden(g.visible));
        }
        None => {
            let _ = writeln!(xml, "{indent}<stack>");
        }
    }
    for node in nodes {
        match node {
            Node::Layer(l) => {
                // Files by number: a name may hold what a path cannot.
                let src = format!("data/layer{}.png", files.len());
                let _ = writeln!(
                    xml,
                    "{indent} <layer name=\"{}\" src=\"{src}\" x=\"{}\" y=\"{}\"{}/>",
                    escape(&l.name),
                    l.x,
                    l.y,
                    hidden(l.visible)
                );
                files.push((src, &l.png));
            }
            Node::Group(g) => write_stack(xml, files, Some(g), &g.children, depth + 1),
        }
    }
    let _ = writeln!(xml, "{indent}</stack>");
}

fn hidden(visible: bool) -> &'static str {
    if visible { " visibility=\"visible\"" } else { " visibility=\"hidden\"" }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Read a document: its stack and its layers' PNGs.
///
/// # Errors
/// It is not an `OpenRaster` document, or a part of it cannot be read.
pub fn read(bytes: &[u8]) -> Result<Document, Error> {
    use quick_xml::events::Event;
    let archive = zip::Archive::new(bytes)?;
    let xml = archive.read("stack.xml").ok_or_else(|| Error("no stack.xml: not an OpenRaster document".into()))??;
    let mut reader = quick_xml::Reader::from_reader(xml.as_slice());
    let mut document = Document::default();
    // The groups open, innermost last; a stack's offset adds to its children's.
    let mut open: Vec<(Group, i32, i32)> = Vec::new();
    let mut buf = Vec::new();
    loop {
        let event = reader.read_event_into(&mut buf).map_err(|e| Error(format!("stack.xml: {e}")))?;
        let empty = matches!(event, Event::Empty(_));
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let attributes = attributes(e)?;
                let get = |key: &str| attributes.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
                let number = |key: &str| get(key).and_then(|v| v.trim().parse::<f64>().ok()).unwrap_or(0.0);
                #[expect(clippy::cast_possible_truncation, reason = "pixel offsets")]
                let offset = |key: &str| number(key).round() as i32;
                let visible = get("visibility") != Some("hidden");
                let (ox, oy) = open.last().map_or((0, 0), |(_, x, y)| (*x, *y));
                match e.name().as_ref() {
                    b"image" => {
                        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a size")]
                        let size = |key: &str| number(key).max(0.0) as u32;
                        document.width = size("w");
                        document.height = size("h");
                    }
                    b"stack" => {
                        let group = Group { name: get("name").unwrap_or_default().into(), visible, children: vec![] };
                        if empty {
                            push(&mut document, &mut open, Node::Group(group));
                        } else {
                            open.push((group, ox + offset("x"), oy + offset("y")));
                        }
                    }
                    b"layer" => {
                        let src = get("src").ok_or_else(|| Error("a layer without src".into()))?;
                        let png = archive.read(src).ok_or_else(|| Error(format!("{src}: not in the archive")))??;
                        let layer = Layer {
                            name: get("name").unwrap_or_default().into(),
                            x: ox + offset("x"),
                            y: oy + offset("y"),
                            visible,
                            png,
                        };
                        push(&mut document, &mut open, Node::Layer(layer));
                    }
                    _ => {}
                }
            }
            Event::End(ref e) if e.name().as_ref() == b"stack" => {
                if let Some((group, _, _)) = open.pop() {
                    push(&mut document, &mut open, Node::Group(group));
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    // The root stack is a group of its own: its children are the document's stack.
    if let [Node::Group(root)] = document.stack.as_slice()
        && root.name.is_empty()
    {
        let children = root.children.clone();
        document.stack = children;
    }
    Ok(document)
}

fn attributes(e: &quick_xml::events::BytesStart) -> Result<Vec<(String, String)>, Error> {
    e.attributes()
        .map(|a| {
            let a = a.map_err(|e| Error(format!("stack.xml: {e}")))?;
            let key = String::from_utf8_lossy(a.key.as_ref()).into_owned();
            let value = a.normalized_value(quick_xml::XmlVersion::Implicit1_0).map_err(|e| Error(format!("stack.xml: {e}")))?.into_owned();
            Ok((key, value))
        })
        .collect()
}

fn push(document: &mut Document, open: &mut [(Group, i32, i32)], node: Node) {
    match open.last_mut() {
        Some((group, _, _)) => group.children.push(node),
        None => document.stack.push(node),
    }
}

/// A layer's pixels: indices and their palette where the PNG is indexed, colors otherwise.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pixels {
    /// An index a pixel (unpacked from 1, 2 or 4 bits), and the palette as RGBA (`tRNS` applied).
    Indexed { indices: Vec<u8>, palette: Vec<[u8; 4]> },
    /// RGBA a pixel, whatever the PNG stored (gray, RGB, 16 bits a channel).
    Rgba(Vec<[u8; 4]>),
}

/// A decoded layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picture {
    pub width: usize,
    pub height: usize,
    pub pixels: Pixels,
}

/// A PNG's pixels, kept indexed where it is.
///
/// # Errors
/// It is not a PNG the decoder reads.
pub fn decode_png(png: &[u8]) -> Result<Picture, Error> {
    let bad = |e: png::DecodingError| Error(e.to_string());
    let probe = png::Decoder::new(Cursor::new(png)).read_info().map_err(bad)?;
    let indexed = probe.info().color_type == png::ColorType::Indexed;
    let mut decoder = png::Decoder::new(Cursor::new(png));
    decoder.set_transformations(if indexed {
        png::Transformations::IDENTITY
    } else {
        png::Transformations::EXPAND | png::Transformations::STRIP_16
    });
    let mut reader = decoder.read_info().map_err(bad)?;
    let mut buf = vec![0; reader.output_buffer_size().ok_or_else(|| Error("a PNG too large".into()))?];
    let frame = reader.next_frame(&mut buf).map_err(bad)?;
    let (width, height) = (frame.width as usize, frame.height as usize);
    let rows = buf.chunks(frame.line_size).take(height);
    if indexed {
        let info = reader.info();
        let plte = info.palette.as_deref().unwrap_or_default();
        let trns = info.trns.as_deref().unwrap_or_default();
        let palette = plte
            .as_chunks::<3>()
            .0
            .iter()
            .enumerate()
            .map(|(i, [r, g, b])| [*r, *g, *b, trns.get(i).copied().unwrap_or(255)])
            .collect();
        let bits = frame.bit_depth as usize;
        let per = 8 / bits;
        let mask = u8::try_from((1u16 << bits) - 1).unwrap_or(0xFF);
        let indices = rows
            .flat_map(|row| {
                (0..width).map(move |x| {
                    let shift = (per - 1 - x % per) * bits;
                    (row[x / per] >> shift) & mask
                })
            })
            .collect();
        return Ok(Picture { width, height, pixels: Pixels::Indexed { indices, palette } });
    }
    let channels = frame.color_type.samples();
    let pixels = rows
        .flat_map(|row| {
            row.chunks_exact(channels).take(width).map(|p| match p {
                [g] => [*g, *g, *g, 255],
                [g, a] => [*g, *g, *g, *a],
                [r, g, b] => [*r, *g, *b, 255],
                [r, g, b, a] => [*r, *g, *b, *a],
                _ => [0; 4],
            })
        })
        .collect();
    Ok(Picture { width, height, pixels: Pixels::Rgba(pixels) })
}

#[cfg(test)]
mod tests {
    use super::{Document, Group, Layer, Node, Pixels, decode_png, read, write};

    fn layer(name: &str, x: i32, png: &[u8]) -> Node {
        Node::Layer(Layer { name: name.into(), x, y: 2, visible: true, png: png.to_vec() })
    }

    #[test]
    fn a_document_reads_back_as_written() {
        let png = crate::texture::png(3, 1, 4, &[1, 2, 15], &[0; 16]).unwrap();
        let document = Document {
            width: 8,
            height: 4,
            stack: vec![
                Node::Group(Group {
                    name: "palettes & more".into(),
                    visible: false,
                    children: vec![layer("<warm>", 0, &png)],
                }),
                layer("texels", 5, &png),
            ],
        };
        let bytes = write(&document, &png, &png);
        assert_eq!(bytes, write(&document, &png, &png));
        assert_eq!(read(&bytes).unwrap(), document);
        let picture = decode_png(&png).unwrap();
        assert_eq!((picture.width, picture.height), (3, 1));
        assert!(matches!(picture.pixels, Pixels::Indexed { ref indices, .. } if indices == &[1, 2, 15]));
    }
}
