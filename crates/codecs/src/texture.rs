//! Images (`pe2img`) and palettes (`pe2clut`): VRAM uploads, decoded to pages of 16-bit words and to palette rows,
//! and drawn as RGBA at a page's bit depth with a chosen palette.
//!
//! An image is a table of columns - each (VRAM x, VRAM y, offset), ended by x = `0xFFFF` and a `term_y` - then its
//! strips: LZSS streams of 64 x 32 words, eight to a 256-row column, column after column in one run from the first
//! entry's offset. A column is a texture page. A single-column image's height can differ: 64 rows when `term_y` is
//! `0xFFFF`, `term_y & 0x7FFF` when its top bit is set. A palette chunk is a 16-byte header (VRAM x, y, rows, width)
//! and one LZSS stream of ABGR1555 colors, a palette a row. (`tools/peassets/image_codec.py`, `doc/ASSET_FORMATS.md`.)

use formats::lzss;
use std::fmt;

const STRIP_WIDTH: usize = 64;
const STRIP_ROWS: usize = 32;
const STRIP_BYTES: usize = STRIP_WIDTH * STRIP_ROWS * 2;
const COLUMN_ROWS: usize = 256;
const END: u16 = 0xFFFF;

#[derive(Debug)]
pub enum Error {
    NoColumns,
    Short,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NoColumns => write!(f, "an image without columns"),
            Error::Short => write!(f, "shorter than its header"),
        }
    }
}

impl std::error::Error for Error {}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

/// One page of an image: where it uploads, and its words, 64 to a row.
#[derive(Clone)]
pub struct Page {
    pub x: u16,
    pub y: u16,
    pub rows: usize,
    pub words: Vec<u16>,
}

/// An image's pages.
///
/// # Errors
/// The column table is empty or cut short.
pub fn pages(data: &[u8]) -> Result<Vec<Page>, Error> {
    let mut columns = Vec::new();
    let mut at = 0;
    let term_y = loop {
        if at + 8 > data.len() {
            return Err(Error::Short);
        }
        let (x, y) = (u16_at(data, at), u16_at(data, at + 2));
        let offset = u32::from_le_bytes([data[at + 4], data[at + 5], data[at + 6], data[at + 7]]);
        at += 8;
        if x == END {
            break y;
        }
        columns.push((x, y, offset as usize));
    };
    let &(_, _, start) = columns.first().ok_or(Error::NoColumns)?;
    let rows = match (columns.len(), term_y) {
        (1, END) => 64,
        (1, t) if t & 0x8000 != 0 => usize::from(t & 0x7FFF),
        _ => COLUMN_ROWS,
    };
    let per_column = (rows / STRIP_ROWS).max(1);
    let strips = lzss::decode_streams(data, start.min(data.len()));
    let mut out = Vec::new();
    for (i, &(x, y, _)) in columns.iter().enumerate() {
        let mut words = Vec::with_capacity(per_column * STRIP_BYTES / 2);
        for s in 0..per_column {
            let mut strip = strips.get(i * per_column + s).cloned().unwrap_or_default();
            strip.resize(STRIP_BYTES, 0);
            words.extend(strip.as_chunks::<2>().0.iter().map(|w| u16::from_le_bytes(*w)));
        }
        out.push(Page { x, y, rows: per_column * STRIP_ROWS, words });
    }
    Ok(out)
}

/// A palette chunk's VRAM place, width and rows of colors.
pub struct Palette {
    pub x: u16,
    pub y: u16,
    pub width: usize,
    pub rows: Vec<Vec<u16>>,
}

/// A palette chunk's rows.
///
/// # Errors
/// The chunk is shorter than its header.
pub fn palette(data: &[u8]) -> Result<Palette, Error> {
    if data.len() < 16 {
        return Err(Error::Short);
    }
    let (x, y) = (u16_at(data, 0), u16_at(data, 2));
    let (rows, width) = (usize::from(u16_at(data, 4)), usize::from(u16_at(data, 6)));
    let mut bytes = lzss::decode_stream(data, 16).0;
    bytes.resize(rows * width * 2, 0);
    let colors: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
    let rows = if width == 0 { vec![] } else { colors.chunks(width).map(<[u16]>::to_vec).collect() };
    Ok(Palette { x, y, width, rows })
}

/// An ABGR1555 color as RGBA, one to one (`tools/peassets/texture/color.py`): each five bits `v << 3 | v >> 2`;
/// `0x0000` transparent (alpha 0), a color with bit 15 set semi-transparent where the primitive blends (128), any
/// other opaque (255) - `0x8000` is black that draws.
#[must_use]
pub fn rgba(c: u16) -> [u8; 4] {
    #[expect(clippy::cast_possible_truncation, reason = "five bits widened to eight")]
    let e = |v: u16| ((v & 0x1F) << 3 | (v & 0x1F) >> 2) as u8;
    let alpha = match c {
        0 => 0,
        c if c & 0x8000 != 0 && c & 0x7FFF != 0 => 128,
        _ => 255,
    };
    [e(c), e(c >> 5), e(c >> 10), alpha]
}

/// Palette entries at a depth: 16 at 4 bits, 256 at 8, none at 16.
#[must_use]
pub fn entries(bpp: u8) -> usize {
    match bpp {
        4 => 16,
        8 => 256,
        _ => 0,
    }
}

/// The texels of a page's rectangle at a depth, row by row: palette indices at 4 and 8 bits, colors at 16. Rows past
/// what the page stores are zero.
#[must_use]
pub fn texels(page: &Page, bpp: u8, (u, v, w, h): (usize, usize, usize, usize)) -> Vec<u16> {
    let at = |x: usize, y: usize| -> u16 {
        if y >= page.rows {
            return 0;
        }
        let row = &page.words[y * STRIP_WIDTH..(y + 1) * STRIP_WIDTH];
        match bpp {
            4 => row.get(x / 4).map_or(0, |w| (w >> ((x % 4) * 4)) & 0xF),
            8 => row.get(x / 2).map_or(0, |w| if x % 2 == 0 { w & 0xFF } else { w >> 8 }),
            _ => row.get(x).copied().unwrap_or(0),
        }
    };
    (v..v + h).flat_map(|y| (u..u + w).map(move |x| at(x, y))).collect()
}

/// Texels as the bytes their sha1 is taken over: a byte each at 4 and 8 bits, a little-endian halfword at 16.
#[must_use]
pub fn texel_bytes(texels: &[u16], bpp: u8) -> Vec<u8> {
    if bpp == 16 {
        texels.iter().flat_map(|t| t.to_le_bytes()).collect()
    } else {
        #[expect(clippy::cast_possible_truncation, reason = "an index of eight bits or fewer")]
        texels.iter().map(|&t| t as u8).collect()
    }
}

/// The palette a texture with no known palette is written with: index i an even gray, opaque.
#[must_use]
pub fn gray_ramp(bpp: u8) -> Vec<u16> {
    let n = entries(bpp);
    #[expect(clippy::cast_possible_truncation, reason = "five bits")]
    (0..n).map(|i| (i * 31 / (n - 1)) as u16).map(|v| 0x8000 | v << 10 | v << 5 | v).collect()
}

/// A texture as a PNG: indexed at 4 and 8 bits, its PLTE (and tRNS) the palette in stored order, unused entries
/// included; RGBA at 16 bits.
///
/// # Errors
/// The PNG encoder fails.
pub fn png(
    width: usize,
    height: usize,
    bpp: u8,
    texels: &[u16],
    palette: &[u16],
) -> Result<Vec<u8>, png::EncodingError> {
    let mut out = Vec::new();
    let to_u32 = |v: usize| u32::try_from(v).unwrap_or(u32::MAX);
    let mut encoder = png::Encoder::new(&mut out, to_u32(width), to_u32(height));
    if bpp == 16 {
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let data: Vec<u8> = texels.iter().flat_map(|&c| rgba(c)).collect();
        encoder.write_header()?.write_image_data(&data)?;
        return Ok(out);
    }
    let colors: Vec<[u8; 4]> = palette.iter().map(|&c| rgba(c)).collect();
    encoder.set_color(png::ColorType::Indexed);
    encoder.set_depth(if bpp == 4 { png::BitDepth::Four } else { png::BitDepth::Eight });
    encoder.set_palette(colors.iter().flat_map(|c| [c[0], c[1], c[2]]).collect::<Vec<u8>>());
    encoder.set_trns(colors.iter().map(|c| c[3]).collect::<Vec<u8>>());
    #[expect(clippy::cast_possible_truncation, reason = "an index of eight bits or fewer")]
    let data: Vec<u8> = if bpp == 4 {
        texels
            .chunks(width)
            .flat_map(|row| row.chunks(2).map(|p| (p[0] as u8) << 4 | p.get(1).map_or(0, |&t| t as u8)))
            .collect()
    } else {
        texels.iter().map(|&t| t as u8).collect()
    };
    encoder.write_header()?.write_image_data(&data)?;
    Ok(out)
}

/// A page drawn at a bit depth (4, 8 or 16) through a palette, or in grey where there is none: RGBA, row by row, and
/// its width (64 words are 64, 128 or 256 pixels).
#[must_use]
pub fn render(page: &Page, bpp: u8, palette: Option<&[u16]>) -> (usize, Vec<u8>) {
    let per_word = match bpp {
        4 => 4,
        8 => 2,
        _ => 1,
    };
    let width = STRIP_WIDTH * per_word;
    let index = |i: u16| -> [u8; 4] {
        if let Some(&c) = palette.and_then(|p| p.get(usize::from(i))) {
            return rgba(c);
        }
        #[expect(clippy::cast_possible_truncation, reason = "an index of eight bits or fewer")]
        let g = if bpp == 8 { i as u8 } else { (i * 17) as u8 };
        [g, g, g, if i == 0 { 0 } else { 255 }]
    };
    let mut out = Vec::with_capacity(width * page.rows * 4);
    for &w in &page.words {
        match bpp {
            4 => (0..4).for_each(|k| out.extend(index((w >> (k * 4)) & 0xF))),
            8 => {
                out.extend(index(w & 0xFF));
                out.extend(index(w >> 8));
            }
            _ => out.extend(rgba(w)),
        }
    }
    (width, out)
}
