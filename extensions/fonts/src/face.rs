//! A face as a UFO 3 font, and back: what a font editor opens, made from the glyphs the game draws and read into them
//! again exactly. One unit is one texel, the baseline at 0.
//!
//! - A glyph's image is its art, an indexed PNG whose index `i` (0-15) is the texel's value, on a gray ramp, and index
//!   16 what it does not paint. Its bottom-left corner is placed so that the drawn cell's is at the record's offsets
//!   (`xOffset`, and `yOffset` rows above the baseline); the advance is the pen's, `xOffset + width - 1 + advanceExtra`.
//! - The kerning classes are the groups `public.kern1.<class>` (a glyph's right side) and `public.kern2.<class>` (its
//!   left), `positive` (1), `negative` (255) or `class<n>`; `kerning.plist` states the pairs they tighten, for a font
//!   editor to show - what is read back is the groups.
//! - A glyph without an image draws nothing but advances (the space): its cell is the page's blank one, its offsets 0,
//!   or where the page has none, a cell of its own of the size the lib says (`org.pe2decomp.cell`).
//! - The one thing UFO has no place for is in the glyph's lib: `org.pe2decomp.cell`, `[x, y, width, height]`, where the
//!   drawn cell is in an image that runs past it.
//!
//! Nothing of the game's tables is in the font: which codes draw which characters is the face's binding's, and what a
//! table holds that no glyph makes is the atlas's.

use std::collections::BTreeMap;

use codecs::ufo::{self, Plist};

use crate::atlas::{Art, Glyph, TRANSPARENT};
use crate::raster::{self, Raster};

const LIB: &str = "org.pe2decomp";
/// The index of an image's palette that paints nothing.
const NOTHING: u8 = 16;

/// A kerning class's group name, for a side (1: a glyph's right side, 2: its left).
fn class_group(side: u8, class: u8) -> String {
    let name = match class {
        1 => "positive".to_string(),
        255 => "negative".to_string(),
        other => format!("class{other}"),
    };
    format!("public.kern{side}.{name}")
}

fn group_class(group: &str) -> Option<u8> {
    match group.rsplit('.').next()? {
        "positive" => Some(1),
        "negative" => Some(255),
        other => other.strip_prefix("class")?.parse().ok(),
    }
}

fn key(name: &str) -> String {
    format!("{LIB}.{name}")
}

fn integers(values: &[i64]) -> Plist {
    Plist::Array(values.iter().map(|v| Plist::Integer(*v)).collect())
}

/// An art's PNG: 8-bit indexed, a gray ramp and a seventeenth entry painting nothing.
pub(crate) fn art_png(art: &Art) -> Vec<u8> {
    let mut out = Vec::new();
    let to_u32 = |v: usize| u32::try_from(v).unwrap_or(u32::MAX);
    let mut encoder = png::Encoder::new(&mut out, to_u32(art.width.max(1)), to_u32(art.height.max(1)));
    encoder.set_color(png::ColorType::Indexed);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::High);
    let mut palette: Vec<u8> = (0..16u8).flat_map(|i| [i * 17; 3]).collect();
    palette.extend([255, 0, 255]);
    encoder.set_palette(palette);
    let mut alpha = vec![255u8; 16];
    alpha.push(0);
    encoder.set_trns(alpha);
    let data: Vec<u8> = art.texels.iter().map(|&t| if t == TRANSPARENT { NOTHING } else { t }).collect();
    let written = encoder.write_header().and_then(|mut w| w.write_image_data(&data));
    if written.is_err() {
        return Vec::new();
    }
    out
}

/// An image's art: an indexed PNG's indices as values (one past 15, or a clear entry, painting nothing), or gray levels
/// of another PNG as the nearest values (clear pixels painting nothing).
pub(crate) fn art_of(png: &[u8]) -> Result<Art, String> {
    let picture = codecs::ora::decode_png(png).map_err(|e| e.to_string())?;
    let texels =
        match picture.pixels {
            codecs::ora::Pixels::Indexed { indices, palette } => indices
                .iter()
                .map(|&i| {
                    if i >= NOTHING || palette.get(usize::from(i)).is_some_and(|c| c[3] < 128) {
                        TRANSPARENT
                    } else {
                        i
                    }
                })
                .collect(),
            codecs::ora::Pixels::Rgba(pixels) => pixels
                .iter()
                .map(|[r, g, b, a]| {
                    if *a < 128 {
                        return TRANSPARENT;
                    }
                    let gray = (u16::from(*r) + u16::from(*g) + u16::from(*b)) / 3;
                    u8::try_from((gray + 8) / 17).unwrap_or(15).min(15)
                })
                .collect(),
        };
    Ok(Art { width: picture.width, height: picture.height, texels })
}

/// A face as a UFO font: its glyphs, its family and style, and the pixels a pair its classes tighten by.
#[must_use]
pub fn to_ufo(glyphs: &[Glyph], family: &str, style: &str, kerning: u8) -> ufo::Font {
    let mut font = ufo::Font {
        info: vec![
            ("familyName".into(), Plist::String(family.into())),
            ("styleName".into(), Plist::String(style.into())),
            ("unitsPerEm".into(), Plist::Integer(16)),
            ("versionMajor".into(), Plist::Integer(1)),
            ("versionMinor".into(), Plist::Integer(0)),
        ],
        ..ufo::Font::default()
    };
    let files = ufo::glyph_file_names(&glyphs.iter().map(|g| g.name.as_str()).collect::<Vec<_>>());
    for glyph in glyphs {
        let mut lib = Vec::new();
        let (width, height) = (i64::from(glyph.width), i64::from(glyph.height));
        let (xo, yo) = (i64::from(glyph.x_offset), i64::from(glyph.y_offset));
        // Without art, a cell of a size of its own: said in the lib, as UFO has no place for it.
        if glyph.art.is_none() && (glyph.width, glyph.height) != (1, 1) {
            lib.push((key("cell"), integers(&[0, 0, width, height])));
        }
        let image = glyph.art.as_ref().map(|art| {
            let file = files[&glyph.name].replace(".glif", ".png");
            font.images.insert(file.clone(), art_png(art));
            let (dx, dy) = (i64::try_from(glyph.cell_at.0).unwrap_or(0), i64::try_from(glyph.cell_at.1).unwrap_or(0));
            let below = i64::try_from(art.height).unwrap_or(0) - dy - height;
            if usize::from(glyph.width) != art.width || usize::from(glyph.height) != art.height {
                lib.push((key("cell"), integers(&[dx, dy, width, height])));
            }
            #[expect(clippy::cast_precision_loss, reason = "small offsets")]
            ufo::Image { file, x: (xo - dx) as f64, y: (-yo - below) as f64 }
        });
        for (side, class) in [(1, glyph.right), (2, glyph.left)] {
            if class != 0 {
                font.groups.entry(class_group(side, class)).or_default().push(glyph.name.clone());
            }
        }
        #[expect(clippy::cast_precision_loss, reason = "a small advance")]
        let advance = (xo + width - 1 + i64::from(glyph.advance_extra)) as f64;
        font.glyphs.push(ufo::Glyph {
            name: glyph.name.clone(),
            advance,
            unicodes: glyph.chars.iter().map(|c| u32::from(*c)).collect(),
            image,
            lib,
            ..ufo::Glyph::default()
        });
    }
    // The pairs the classes tighten, as a font editor shows them: a right class and a left class whose sum and one
    // are 3 or more, modulo 256.
    let firsts: Vec<String> = font.groups.keys().filter(|g| g.starts_with("public.kern1.")).cloned().collect();
    let seconds: Vec<String> = font.groups.keys().filter(|g| g.starts_with("public.kern2.")).cloned().collect();
    for first in &firsts {
        for second in &seconds {
            let (Some(a), Some(b)) = (group_class(first), group_class(second)) else { continue };
            if a.wrapping_add(b).wrapping_add(1) >= 3 {
                font.kerning.entry(first.clone()).or_default().insert(second.clone(), -f64::from(kerning));
            }
        }
    }
    font
}

/// A face's glyphs, read from a UFO font.
///
/// # Errors
/// An image is missing or is not a PNG, or a glyph's numbers do not fit a record.
pub fn from_ufo(font: &ufo::Font, raster: &Raster) -> Result<Vec<Glyph>, String> {
    let units = font.info.iter().find(|(key, _)| key == "unitsPerEm").and_then(|(_, v)| v.as_f64()).unwrap_or(1000.0);
    // Texels to a unit, for what is drawn of outlines rather than images: one, where the face sets no size.
    let scale = raster.pixels_per_em.map_or(1.0, |pixels| pixels / units);
    let class = |side: u8, name: &str| -> u8 {
        let prefix = format!("public.kern{side}.");
        font.groups
            .iter()
            .filter(|(g, members)| g.starts_with(&prefix) && members.iter().any(|m| m == name))
            .find_map(|(g, _)| group_class(g))
            .unwrap_or(0)
    };
    let small = |what: &str, name: &str, value: i64| -> Result<i8, String> {
        i8::try_from(value).map_err(|_| format!("{name}: its {what} {value} does not fit a record"))
    };
    let mut out = Vec::new();
    for g in &font.glyphs {
        let lib = |name: &str| g.lib.iter().find(|(k, _)| *k == key(name)).map(|(_, v)| v);
        let numbers = |name: &str| {
            lib(name)
                .and_then(Plist::as_array)
                .map(|items| items.iter().filter_map(Plist::as_integer).collect::<Vec<i64>>())
        };
        let art = match &g.image {
            Some(image) => {
                let png = font.images.get(&image.file).ok_or_else(|| format!("{}: no image {}", g.name, image.file))?;
                Some((art_of(png).map_err(|e| format!("{}: {e}", g.name))?, image))
            }
            None => None,
        };
        // The drawn cell: where the lib says, else the whole image; without one, the blank 1 x 1.
        let (dx, dy, width, height) = match (numbers("cell").as_deref(), &art) {
            (Some(&[dx, dy, w, h]), Some(_)) => (dx, dy, w, h),
            (_, Some((art, _))) => {
                (0, 0, i64::try_from(art.width).unwrap_or(0), i64::try_from(art.height).unwrap_or(0))
            }
            (Some(&[_, _, w, h]), None) => (0, 0, w, h),
            _ => (0, 0, 1, 1),
        };
        let (mut xo, mut yo) = match &art {
            #[expect(clippy::cast_possible_truncation, reason = "a whole offset")]
            Some((art, image)) => {
                let below = i64::try_from(art.height).unwrap_or(0) - dy - height;
                (image.x.round() as i64 + dx, -(image.y.round() as i64) - below)
            }
            None => (0, 0),
        };
        // Outlines without an image: drawn, the art the whole cell, its last row from the baseline's (texel row -1).
        let (mut width, mut height) = (width, height);
        let mut art = art.map(|(art, _)| art);
        if art.is_none()
            && (!g.contours.is_empty() || !g.components.is_empty())
            && let Some(drawn) = raster::rasterize(font, g, raster).map_err(|e| format!("{}: {e}", g.name))?
        {
            width = i64::try_from(drawn.art.width).unwrap_or(i64::MAX);
            height = i64::try_from(drawn.art.height).unwrap_or(i64::MAX);
            (xo, yo) = (drawn.left, drawn.bottom + 1);
            art = Some(drawn.art);
        }
        // In texels: an image's units are its texels, what is not an image is scaled to the face's size.
        let advance_units = if g.image.is_some() { g.advance } else { g.advance * scale };
        #[expect(clippy::cast_possible_truncation, reason = "a whole advance")]
        let advance = advance_units.round() as i64;
        let byte = |what: &str, value: i64| {
            u8::try_from(value).map_err(|_| format!("{}: its {what} {value} does not fit a record", g.name))
        };
        out.push(Glyph {
            name: g.name.clone(),
            chars: g.unicodes.iter().filter_map(|u| char::from_u32(*u)).collect(),
            width: byte("width", width)?,
            height: byte("height", height)?,
            x_offset: small("x offset", &g.name, xo)?,
            y_offset: small("y offset", &g.name, yo)?,
            advance_extra: small("advance", &g.name, advance - xo - width + 1)?,
            left: class(2, &g.name),
            right: class(1, &g.name),
            art,
            cell_at: (usize::try_from(dx).unwrap_or(0), usize::try_from(dy).unwrap_or(0)),
        });
    }
    Ok(out)
}

/// The files of a face's UFO, by their names within its folder.
#[must_use]
pub fn ufo_files(glyphs: &[Glyph], family: &str, style: &str, kerning: u8) -> BTreeMap<String, Vec<u8>> {
    ufo::write(&to_ufo(glyphs, family, style, kerning))
}
