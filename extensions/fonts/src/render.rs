//! A face's glyphs drawn as the game draws them (`textDrawString`, `textdraw.c`), and laid out to look at.
//!
//! A line: before each glyph the pen moves back by the face's kerning pixels where the previous glyph's right class and
//! this one's left class sum, with one, to 3 or more modulo 256; the glyph's drawn cell - never its art past it - is
//! drawn at the pen plus its x offset, its last row at the baseline plus its y offset; the pen then advances by its
//! width less one, its advance extra and its x offset. A character the face has no glyph for is left out.
//!
//! The page's palettes are the executable's; here a texel 11-15 is the letter, 6-10 its outline, and below that nothing.

use pe2_ext::Image;

use crate::atlas::{Glyph, TRANSPARENT};

/// A texel as shown.
#[must_use]
pub fn color(value: u8) -> [u8; 4] {
    match value {
        11..=15 => {
            let level = 175 + (value - 11) * 20;
            [level, level, level, 255]
        }
        6..=10 => [40, 44, 56, 255],
        _ => [0, 0, 0, 0],
    }
}

/// Where a glyph's drawn cell's texels are in its art, and its size.
fn cell(glyph: &Glyph) -> (usize, usize, usize, usize) {
    (glyph.cell_at.0, glyph.cell_at.1, usize::from(glyph.width), usize::from(glyph.height))
}

/// Lines of text drawn with a face, its kerning pixels as given, on a transparent ground with a margin.
#[must_use]
pub fn lines(glyphs: &[Glyph], text: &str, kerning: u8) -> Image {
    const MARGIN: i64 = 4;
    let tallest = glyphs.iter().map(|g| i64::from(g.height)).max().unwrap_or(8);
    // Each drawn cell: its glyph and where its top-left texel lands.
    let mut drawn: Vec<(&Glyph, i64, i64)> = Vec::new();
    for (line, text) in text.lines().enumerate() {
        let (mut x, y) = (0i64, i64::try_from(line).unwrap_or(0) * (tallest + 4));
        let mut previous: u8 = 0;
        for c in text.chars() {
            let Some(glyph) = glyphs.iter().find(|g| g.chars.contains(&c)) else { continue };
            if previous.wrapping_add(glyph.left).wrapping_add(1) >= 3 {
                x -= i64::from(kerning);
            }
            previous = glyph.right;
            if glyph.art.is_some() {
                drawn.push((
                    glyph,
                    x + i64::from(glyph.x_offset),
                    y - i64::from(glyph.height) + 1 + i64::from(glyph.y_offset),
                ));
            }
            x += i64::from(glyph.width) - 1 + i64::from(glyph.advance_extra) + i64::from(glyph.x_offset);
        }
    }
    let left = drawn.iter().map(|d| d.1).min().unwrap_or(0) - MARGIN;
    let top = drawn.iter().map(|d| d.2).min().unwrap_or(0) - MARGIN;
    let right = drawn.iter().map(|(g, x, _)| x + i64::from(g.width)).max().unwrap_or(0) + MARGIN;
    let bottom = drawn.iter().map(|(g, _, y)| y + i64::from(g.height)).max().unwrap_or(0) + MARGIN;
    let (width, height) = (usize::try_from(right - left).unwrap_or(1), usize::try_from(bottom - top).unwrap_or(1));
    let mut rgba = vec![0u8; width * height * 4];
    for (glyph, x, y) in drawn {
        let Some(art) = &glyph.art else { continue };
        let (cx, cy, w, h) = cell(glyph);
        for row in 0..h {
            for col in 0..w {
                let value = art.texels.get((cy + row) * art.width + cx + col).copied().unwrap_or(TRANSPARENT);
                let shown = color(value);
                if shown[3] == 0 {
                    continue;
                }
                let (px, py) = (x - left + i64::try_from(col).unwrap_or(0), y - top + i64::try_from(row).unwrap_or(0));
                let (Ok(px), Ok(py)) = (usize::try_from(px), usize::try_from(py)) else { continue };
                if px < width && py < height {
                    rgba[(py * width + px) * 4..][..4].copy_from_slice(&shown);
                }
            }
        }
    }
    Image { width, height, rgba }
}

/// A face's glyphs side by side, in its order, sixteen to a row: each its whole art on a dark ground, its drawn cell's
/// texels brighter than what runs past it.
#[must_use]
pub fn sheet(glyphs: &[Glyph]) -> Image {
    const ACROSS: usize = 16;
    const SPACE: usize = 3;
    let drawn: Vec<&Glyph> = glyphs.iter().filter(|g| g.art.is_some()).collect();
    let slot_w = drawn.iter().filter_map(|g| g.art.as_ref()).map(|a| a.width).max().unwrap_or(1) + SPACE;
    let slot_h = drawn.iter().filter_map(|g| g.art.as_ref()).map(|a| a.height).max().unwrap_or(1) + SPACE;
    let rows = drawn.len().div_ceil(ACROSS).max(1);
    let (width, height) = (ACROSS * slot_w + SPACE, rows * slot_h + SPACE);
    let mut rgba = [24u8, 26, 32, 255].repeat(width * height);
    for (i, glyph) in drawn.iter().enumerate() {
        let Some(art) = &glyph.art else { continue };
        let (ox, oy) = (SPACE + (i % ACROSS) * slot_w, SPACE + (i / ACROSS) * slot_h);
        let (cx, cy, w, h) = cell(glyph);
        for row in 0..art.height {
            for col in 0..art.width {
                let mut shown = color(art.texels[row * art.width + col]);
                if shown[3] == 0 {
                    continue;
                }
                // What the game does not draw, dimmed.
                if !(cx..cx + w).contains(&col) || !(cy..cy + h).contains(&row) {
                    shown = [shown[0] / 2, shown[1] / 3, shown[2] / 3, 255];
                }
                rgba[((oy + row) * width + ox + col) * 4..][..4].copy_from_slice(&shown);
            }
        }
    }
    Image { width, height, rgba }
}
