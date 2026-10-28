//! What an asset looks like, as plain data: an image's pixels in each of its looks (a background's picture, a
//! texture through a palette, an atlas with each cell through its own), a palette's colors, an atlas's cells. The
//! editor draws them, and the agents' server (`mcp`) hands them out as PNG; neither decodes an asset itself.

use anyhow::{Context, Result};
use project::index::Asset;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Pixels as unmultiplied RGBA, row by row.
pub struct Image {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

impl Image {
    /// As PNG.
    ///
    /// # Errors
    /// The image cannot be encoded.
    pub fn png(&self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        let (width, height) = (u32::try_from(self.width)?, u32::try_from(self.height)?);
        let mut encoder = png::Encoder::new(&mut out, width.max(1), height.max(1));
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        if self.rgba.is_empty() {
            writer.write_image_data(&[0; 4])?;
        } else {
            writer.write_image_data(&self.rgba)?;
        }
        writer.finish()?;
        Ok(out)
    }
}

impl Image {
    /// The image at most `side` pixels on its longer side: each pixel the average of the block it stands for, by
    /// coverage (a transparent pixel adds nothing to the color). One already that small is kept as it is.
    #[must_use]
    pub fn thumbnail(&self, side: usize) -> Image {
        let longest = self.width.max(self.height);
        if longest <= side || self.width == 0 || self.height == 0 {
            return Image { width: self.width, height: self.height, rgba: self.rgba.clone() };
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

    /// An image from PNG, as `png` writes it.
    ///
    /// # Errors
    /// It is not an 8-bit RGBA PNG.
    pub fn from_png(bytes: &[u8]) -> Result<Image> {
        let mut reader = png::Decoder::new(std::io::Cursor::new(bytes)).read_info()?;
        let mut rgba = vec![0; reader.output_buffer_size().unwrap_or(0)];
        let info = reader.next_frame(&mut rgba)?;
        anyhow::ensure!(
            info.color_type == png::ColorType::Rgba && info.bit_depth == png::BitDepth::Eight,
            "not 8-bit RGBA"
        );
        rgba.truncate(info.buffer_size());
        Ok(Image { width: info.width as usize, height: info.height as usize, rgba })
    }
}

/// A rectangle in texels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// An image's role (`texture`, `atlas`, `background`, ...).
#[must_use]
pub fn role(asset: &Asset) -> Option<&str> {
    asset.role.as_deref()
}

/// A texture or an atlas: an image of texels and palettes.
#[must_use]
pub fn is_texture(asset: &Asset) -> bool {
    matches!(role(asset), Some("texture" | "atlas"))
}

/// A field's array; empty where it has none.
#[must_use]
pub fn array<'a>(asset: &'a Asset, key: &str) -> &'a [toml::Value] {
    asset.fields().get(key).and_then(toml::Value::as_array).map_or(&[][..], Vec::as_slice)
}

/// An image's palettes by name, in the order it declares them.
#[must_use]
pub fn palette_names(asset: &Asset) -> Vec<String> {
    array(asset, "palettes").iter().filter_map(|v| v.get("name")?.as_str().map(str::to_string)).collect()
}

/// An image's looks, by name: a texture's variants (the first its default); an atlas's default and its variants.
/// From what the index keeps of the declaration: the tree asks for every image's, and parses none.
#[must_use]
pub fn variant_names(asset: &Asset) -> Vec<String> {
    let named = asset.variants.iter().cloned();
    if role(asset) == Some("atlas") && !asset.variants.is_empty() {
        std::iter::once("default".to_string()).chain(named).collect()
    } else {
        named.collect()
    }
}

/// The palette each part of an image takes in one of its looks: a texture's (`None` for its only cell, the image),
/// or each of an atlas's cells by name.
fn look(asset: &Asset, variant: usize) -> (Option<usize>, HashMap<String, usize>) {
    let names = palette_names(asset);
    let index = |name: Option<&str>| name.and_then(|n| names.iter().position(|p| p == n));
    let variants = array(asset, "variants");
    if role(asset) == Some("atlas") {
        let overrides = variant
            .checked_sub(1)
            .and_then(|v| variants.get(v)?.get("palettes")?.as_table().cloned())
            .unwrap_or_default();
        let chosen = overrides.iter().filter_map(|(cell, p)| Some((cell.clone(), index(p.as_str())?))).collect();
        return (None, chosen);
    }
    let palette = variants.get(variant).and_then(|v| v.get("palette")).and_then(toml::Value::as_str);
    (index(palette).or(Some(0)), HashMap::new())
}

fn int(value: Option<&toml::Value>) -> Option<f32> {
    #[expect(clippy::cast_precision_loss, reason = "VRAM coordinates")]
    value.and_then(toml::Value::as_integer).map(|v| v as f32)
}

/// A `[x, y]` pair.
#[must_use]
pub fn pair(value: Option<&toml::Value>) -> Option<(f32, f32)> {
    let a = value?.as_array()?;
    Some((int(a.first())?, int(a.get(1))?))
}

/// A `rect = [x, y, w, h]` as a rectangle.
#[must_use]
pub fn rect(value: &toml::Value) -> Option<Rect> {
    let r = value.as_array()?;
    Some(Rect { x: int(r.first())?, y: int(r.get(1))?, width: int(r.get(2))?, height: int(r.get(3))? })
}

/// A depth in bits a texel; `default` where none is given.
#[must_use]
pub fn bpp_of(value: Option<&toml::Value>, default: u8) -> u8 {
    value.and_then(toml::Value::as_integer).and_then(|v| u8::try_from(v).ok()).unwrap_or(default)
}

/// An image's depth.
#[must_use]
pub fn bpp(asset: &Asset) -> u8 {
    bpp_of(asset.fields().get("bpp"), 8)
}

#[must_use]
pub fn texels_per_halfword(bpp: u8) -> usize {
    match bpp {
        4 => 4,
        8 => 2,
        _ => 1,
    }
}

/// A cell as drawn in its image: its rectangle in the image's texels - a cell of a deeper depth starts where its
/// halfwords do and is drawn at its own width - with its depth and palette.
pub struct Cell {
    pub name: String,
    pub rect: Rect,
    pub bpp: u8,
    pub source: Rect,
    pub palette: Option<usize>,
}

/// An atlas's cells.
#[must_use]
pub fn cells_of(asset: &Asset) -> Vec<Cell> {
    let bpp = bpp(asset);
    let names = palette_names(asset);
    array(asset, "cells")
        .iter()
        .filter_map(|c| {
            let source = rect(c.get("rect")?)?;
            let own = bpp_of(c.get("bpp"), bpp);
            #[expect(clippy::cast_precision_loss, reason = "texels per halfword")]
            let scale = texels_per_halfword(bpp) as f32 / texels_per_halfword(own) as f32;
            let rect = Rect { x: source.x * scale, ..source };
            let palette =
                c.get("palette").and_then(toml::Value::as_str).and_then(|p| names.iter().position(|n| n == p));
            Some(Cell { name: c.get("name")?.as_str()?.to_string(), rect, bpp: own, source, palette })
        })
        .collect()
}

/// What a picture is drawn from: a background's `native.bs`, or an image's stored texels and its palettes' colors
/// (`project::declarations`; `project::textures` writes them).
enum Source {
    Bs(PathBuf),
    Texels { texels: PathBuf, colors: Vec<Option<PathBuf>> },
}

fn source(asset: &Asset) -> Option<Source> {
    let file = |name: &str| asset.files.iter().find(|f| f.name == name).map(|f| f.path.clone());
    if let Some(bs) = file("native.bs") {
        return Some(Source::Bs(bs));
    }
    if !is_texture(asset) {
        return None;
    }
    let texels = file("native.texels")?;
    let colors = palette_names(asset).iter().map(|p| file(&format!("native.{p}.colors"))).collect();
    Some(Source::Texels { texels, colors })
}

/// Whether an asset has a picture: a background, a texture, an atlas.
#[must_use]
pub fn has_picture(asset: &Asset) -> bool {
    source(asset).is_some()
}

fn halfwords(bytes: &[u8]) -> Vec<u16> {
    bytes.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect()
}

fn read(file: &Path) -> Result<Vec<u8>> {
    std::fs::read(file).with_context(|| file.display().to_string())
}

/// An asset's picture in one of its looks (an index into `variant_names`; a look past them is its first); none when
/// it has no picture.
///
/// # Errors
/// Its files cannot be read or decoded.
pub fn picture(asset: &Asset, variant: usize) -> Option<Result<Image>> {
    let variant = if variant < variant_names(asset).len() { variant } else { 0 };
    Some(match source(asset)? {
        Source::Bs(file) => decode_bs(&file),
        Source::Texels { texels, colors } => read(&texels).map(|bytes| {
            let colors: Vec<Option<Vec<u16>>> =
                colors.iter().map(|f| f.as_ref().and_then(|f| std::fs::read(f).ok()).map(|b| halfwords(&b))).collect();
            decode_texels(asset, &bytes, &colors, variant)
        }),
    })
}

/// A palette's colors, as ABGR1555 halfwords; none when the image has no such palette or its colors are not stored.
///
/// # Errors
/// Its colors cannot be read.
pub fn palette_colors(asset: &Asset, palette: usize) -> Option<Result<Vec<u16>>> {
    let Some(Source::Texels { colors, .. }) = source(asset) else { return None };
    let file = colors.get(palette).cloned().flatten()?;
    Some(read(&file).map(|b| halfwords(&b)))
}

/// A color as unmultiplied RGBA.
#[must_use]
pub fn rgba(color: u16) -> [u8; 4] {
    codecs::texture::rgba(color)
}

/// An image's pixels in one of its looks: a texture through its variant's palette; an atlas faintly through its first
/// palette, and each of its cells through its own - the variant's where it gives the cell one - a deeper cell's texels
/// taken from the same halfwords at its depth. Where no colors are extracted (an executable uploads them) or none are
/// known, in gray.
fn decode_texels(asset: &Asset, texels: &[u8], colors: &[Option<Vec<u16>>], variant: usize) -> Image {
    const CLEAR: [u8; 4] = [0; 4];
    let bpp = bpp(asset);
    let (width, height) = pair(asset.fields().get("size")).unwrap_or_default();
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "image sizes")]
    let (w, h) = (width as usize, height as usize);
    let values: Vec<u16> = if bpp == 16 { halfwords(texels) } else { texels.iter().map(|&b| u16::from(b)).collect() };
    let at = |x: usize, y: usize| values.get(y * w + x).copied().unwrap_or(0);
    let mut pixels: Vec<[u8; 4]>;
    if bpp == 16 {
        pixels = (0..w * h).map(|i| rgba(at(i % w, i / w))).collect();
        return Image { width: w, height: h, rgba: pixels.concat() };
    }
    let gray = codecs::texture::gray_ramp(bpp);
    let palette = |i: Option<usize>| -> &[u16] { i.and_then(|i| colors.get(i)?.as_deref()).unwrap_or(&gray) };
    let cells = cells_of(asset);
    let atlas = !cells.is_empty();
    let (texture_palette, overrides) = look(asset, variant);
    let base = palette(texture_palette.or(Some(0)));
    pixels = (0..w * h)
        .map(|i| {
            let mut c = base.get(usize::from(at(i % w, i / w))).map_or(CLEAR, |&c| rgba(c));
            // An atlas's texels outside its cells, faintly.
            if atlas {
                c[3] /= 4;
            }
            c
        })
        .collect();
    if atlas {
        let per = texels_per_halfword(bpp);
        for cell in &cells {
            let colors = palette(overrides.get(&cell.name).copied().or(cell.palette));
            #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "texel coordinates")]
            let (sx, sy, cw, ch, dx) = (
                cell.source.x as usize,
                cell.source.y as usize,
                cell.source.width as usize,
                cell.source.height as usize,
                cell.rect.x as usize,
            );
            // A deeper cell spans more of the image's texels than it is wide: the span past its width is its own
            // halfwords read at the image's depth, so it is left blank.
            let span = cw * per / texels_per_halfword(cell.bpp);
            for y in sy..(sy + ch).min(h) {
                for x in (dx + cw)..(dx + span).min(w) {
                    pixels[y * w + x] = CLEAR;
                }
                for i in 0..cw {
                    let index = if cell.bpp == bpp {
                        at(sx + i, y)
                    } else {
                        // The cell's texel from the halfword holding it, rebuilt from the image's texels.
                        let own = texels_per_halfword(cell.bpp);
                        let (word_x, k) = ((sx + i) / own, (sx + i) % own);
                        let word = (0..per).fold(0u16, |word, j| word | at(word_x * per + j, y) << (16 / per * j));
                        (word >> (16 / own * k)) & ((1 << (16 / own)) - 1)
                    };
                    if dx + i < w {
                        pixels[y * w + dx + i] = colors.get(usize::from(index)).map_or(CLEAR, |&c| rgba(c));
                    }
                }
            }
        }
    }
    Image { width: w, height: h, rgba: pixels.concat() }
}

/// A background's picture.
fn decode_bs(file: &Path) -> Result<Image> {
    let picture = codecs::bs::decode(&read(file)?).map_err(|e| anyhow::anyhow!("{}: {e}", file.display()))?;
    let rgba = picture.rgb.as_chunks::<3>().0.iter().flat_map(|&[r, g, b]| [r, g, b, 255]).collect();
    Ok(Image { width: picture.width, height: picture.height, rgba })
}
