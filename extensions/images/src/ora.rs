//! A texture or an atlas as an `OpenRaster` source (`pe2.images@ora`): what a painting program opens, from which the
//! stored texels and palettes are made again byte for byte.
//!
//! The document is the image's size and holds, topmost first:
//! - a hidden group `palettes`, a strip a palette (its colors in a row, one pixel each, alpha kept), named after it;
//! - a hidden group `indices`, the texels' indices in gray (`texels`, and a group `cells` of a layer a cell of another
//!   depth than the image's), what tells which index a texel is when a program has saved its layer in color;
//! - a group `cells`, an atlas's cells, each a layer named after it at the place it is drawn, its texels at its own
//!   depth through its own palette;
//! - the layer `texels`, every texel through the first palette (at 16 bpp, its colors).
//!
//! Texel layers are indexed PNGs, so a palette's repeated colors cannot blur which index a texel is. A layer a program
//! saved in color is matched back to its palette: a texel still the color of its index in `indices` keeps that index,
//! a repainted one takes the first entry of its color, the nearest where none is exact. Which palette a cell
//! is drawn through, its depth and the image's are the asset's settings, joined to the layers by name: `OpenRaster` has
//! no links between layers, and programs drop attributes they do not know. The image's size and each cell's rectangle
//! are the source's, derived on import; a cell layer the settings do not know is a cell of the image's depth without a
//! palette. Where layers overlap, a repainted texel (one whose index differs from the one recorded for it) wins over
//! one left as it was, and among those a cell over the texels, a layer over those below it: what the painter saw.

use codecs::ora::{self, Document, Group, Layer, Node, Pixels};
use codecs::texture::{color, entries, gray_ramp, rgba};
use pe2_ext::{Asset, Result, Table, Value, anyhow};
use std::collections::HashMap;

use crate::{TexelImage, bpp, cells, halfwords, palette_names, texels_per_halfword};

/// Where a source is when the declaration does not say.
pub const SOURCE: &str = "source.ora";
const TEXELS_LAYER: &str = "texels";
const PALETTES: &str = "palettes";
const CELLS: &str = "cells";
const INDICES: &str = "indices";
/// The side of the thumbnail `OpenRaster` asks for (at most 256).
const THUMBNAIL: usize = 128;

/// The texels of an image: a value a texel (an index, or a color at 16 bpp), row by row.
struct Texels {
    width: usize,
    height: usize,
    bpp: u8,
    values: Vec<u16>,
}

impl Texels {
    fn at(&self, x: usize, y: usize) -> u16 {
        self.values.get(y * self.width + x).copied().unwrap_or(0)
    }

    /// The bits a texel holds in a halfword, at the image's depth.
    fn bits(&self) -> usize {
        16 / texels_per_halfword(self.bpp)
    }

    /// The halfword holding image texel `x`'s word, rebuilt from the texels.
    fn word(&self, word_x: usize, y: usize) -> u16 {
        let per = texels_per_halfword(self.bpp);
        (0..per).fold(0, |word, j| word | self.at(word_x * per + j, y) << (self.bits() * j))
    }

    fn set_word(&mut self, word_x: usize, y: usize, word: u16) {
        let per = texels_per_halfword(self.bpp);
        let mask = mask(self.bits());
        for j in 0..per {
            let x = word_x * per + j;
            if x < self.width && y < self.height {
                self.values[y * self.width + x] = (word >> (self.bits() * j)) & mask;
            }
        }
    }

    /// A cell's texel at its own depth: texel `x` of the halfwords the cell's row starts at.
    fn cell_texel(&self, own: u8, x: usize, y: usize) -> u16 {
        if own == self.bpp {
            return self.at(x, y);
        }
        let per = texels_per_halfword(own);
        let bits = 16 / per;
        (self.word(x / per, y) >> (bits * (x % per))) & mask(bits)
    }

    fn set_cell_texel(&mut self, own: u8, x: usize, y: usize, value: u16) {
        if own == self.bpp {
            if x < self.width && y < self.height {
                self.values[y * self.width + x] = value;
            }
            return;
        }
        let per = texels_per_halfword(own);
        let (bits, word_x) = (16 / per, x / per);
        let shift = bits * (x % per);
        let word = self.word(word_x, y) & !(mask(bits) << shift) | (value & mask(bits)) << shift;
        self.set_word(word_x, y, word);
    }

    /// As stored: a byte a texel, a halfword at 16 bpp.
    fn stored(&self) -> Vec<u8> {
        if self.bpp == 16 {
            self.values.iter().flat_map(|v| v.to_le_bytes()).collect()
        } else {
            #[expect(clippy::cast_possible_truncation, reason = "an index of eight bits or fewer")]
            self.values.iter().map(|&v| v as u8).collect()
        }
    }
}

fn mask(bits: usize) -> u16 {
    if bits >= 16 { u16::MAX } else { (1 << bits) - 1 }
}

/// A palette as many entries as its depth indexes, gray where it has none, so every index a PNG holds is in it.
fn padded(colors: Option<&[u16]>, bpp: u8) -> Vec<u16> {
    let n = entries(bpp);
    let mut colors = colors.map_or_else(|| gray_ramp(bpp), <[u16]>::to_vec);
    if colors.len() < n {
        colors.resize(n, 0);
    }
    colors
}

fn png_error(e: impl std::fmt::Display) -> pe2_ext::Error {
    anyhow!("PNG: {e}")
}

/// The source, written from an image's texels and palettes, with its settings (its cells, its palettes' names).
///
/// # Errors
/// They cannot be written as layers.
pub fn export(asset: &Asset, image: &TexelImage) -> Result<Vec<u8>> {
    let (width, height, bpp) = (image.width, image.height, image.bpp);
    let stored = &image.texels;
    let values = if bpp == 16 { halfwords(stored) } else { stored.iter().map(|&b| u16::from(b)).collect() };
    let texels = Texels { width, height, bpp, values };
    let names = palette_names(asset);
    let colors = image.colors();
    let palette = |i: Option<usize>| i.and_then(|i| colors.get(i)?.as_deref());

    let strips = names
        .iter()
        .zip(&colors)
        .filter_map(|(name, colors)| {
            let colors = colors.as_ref()?;
            let pixels: Vec<u8> = colors.iter().flat_map(|&c| rgba(c)).collect();
            Some(codecs::texture::rgba_png(colors.len(), 1, &pixels).map(|png| layer(name, 0, 0, false, png)))
        })
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(png_error)?;
    let (mut cell_layers, mut cell_indices) = (Vec::new(), Vec::new());
    for cell in cells(asset) {
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "texel coordinates")]
        let (sx, sy, cw, ch, dx) = (
            cell.source.x as usize,
            cell.source.y as usize,
            cell.source.width as usize,
            cell.source.height as usize,
            cell.rect.x as usize,
        );
        let values: Vec<u16> =
            (sy..sy + ch).flat_map(|y| (0..cw).map(move |i| (sx + i, y))).map(|(x, y)| texels.cell_texel(cell.bpp, x, y)).collect();
        let png = codecs::texture::png(cw, ch, cell.bpp, &values, &padded(palette(cell.palette), cell.bpp))
            .map_err(png_error)?;
        cell_layers.push(layer(&cell.name, dx, sy, true, png));
        // A cell of the image's depth has its indices in the texels'.
        if cell.bpp != 16 && cell.bpp != bpp {
            let png = codecs::texture::index_png(cw, ch, cell.bpp, &values).map_err(png_error)?;
            cell_indices.push(layer(&cell.name, dx, sy, false, png));
        }
    }
    let texels_png =
        codecs::texture::png(width, height, bpp, &texels.values, &padded(palette(Some(0)), bpp)).map_err(png_error)?;

    let mut stack = vec![Node::Group(Group { name: PALETTES.into(), visible: false, children: strips })];
    if bpp != 16 || !cell_indices.is_empty() {
        let mut children = vec![];
        if !cell_indices.is_empty() {
            children.push(Node::Group(Group { name: CELLS.into(), visible: false, children: cell_indices }));
        }
        if bpp != 16 {
            let png = codecs::texture::index_png(width, height, bpp, &texels.values).map_err(png_error)?;
            children.push(layer(TEXELS_LAYER, 0, 0, false, png));
        }
        stack.push(Node::Group(Group { name: INDICES.into(), visible: false, children }));
    }
    if !cell_layers.is_empty() {
        stack.push(Node::Group(Group { name: CELLS.into(), visible: true, children: cell_layers }));
    }
    stack.push(layer(TEXELS_LAYER, 0, 0, true, texels_png));
    let document = Document { width: u32::try_from(width)?, height: u32::try_from(height)?, stack };
    let merged = crate::draw(asset, image, 0);
    let thumbnail = merged.thumbnail(THUMBNAIL);
    let merged = codecs::texture::rgba_png(merged.width, merged.height, &merged.rgba).map_err(png_error)?;
    let thumbnail = codecs::texture::rgba_png(thumbnail.width, thumbnail.height, &thumbnail.rgba).map_err(png_error)?;
    Ok(ora::write(&document, &merged, &thumbnail))
}

fn layer(name: &str, x: usize, y: usize, visible: bool, png: Vec<u8>) -> Node {
    let at = |v: usize| i32::try_from(v).unwrap_or(i32::MAX);
    Node::Layer(Layer { name: name.to_string(), x: at(x), y: at(y), visible, png })
}

/// A layer's values: its indices where it is indexed, else its colors matched to `palette` (at 16 bpp, the colors
/// themselves), a texel still the color of its index in `known` (its index layer) keeping that index.
fn layer_values(
    layer: &Layer,
    bpp: u8,
    palette: &[u16],
    known: Option<&Known>,
    notes: &mut Vec<String>,
) -> Result<(usize, usize, Vec<u16>)> {
    let picture = ora::decode_png(&layer.png).map_err(|e| anyhow!("layer {}: {e}", layer.name))?;
    let values = match picture.pixels {
        Pixels::Indexed { indices, palette: plte } if bpp == 16 => {
            indices.iter().map(|&i| plte.get(usize::from(i)).map_or(0, |&c| color(c))).collect()
        }
        Pixels::Indexed { indices, .. } => {
            let limit = entries(bpp);
            if let Some(&i) = indices.iter().find(|&&i| usize::from(i) >= limit) {
                return Err(anyhow!("layer {}: index {i} past {limit} colors at {bpp} bpp", layer.name));
            }
            indices.into_iter().map(u16::from).collect()
        }
        Pixels::Rgba(pixels) if bpp == 16 => pixels.into_iter().map(color).collect(),
        Pixels::Rgba(pixels) => {
            let mut matcher = Matcher::new(palette);
            let width = picture.width.max(1);
            let values = pixels
                .into_iter()
                .enumerate()
                .map(|(i, p)| match known.and_then(|k| k.at(layer.x, layer.y, i % width, i / width)) {
                    Some(k) if palette.get(usize::from(k)).is_some_and(|&c| rgba(c) == p) => k,
                    _ => matcher.index(p),
                })
                .collect();
            notes.extend(matcher.note(&layer.name));
            values
        }
    };
    Ok((picture.width, picture.height, values))
}

/// An index layer's indices, however it was saved, at its place in the document.
struct Known {
    x: i32,
    y: i32,
    width: usize,
    height: usize,
    indices: Vec<u16>,
}

impl Known {
    fn read(layer: &Layer, bpp: u8) -> Result<Known> {
        let picture = ora::decode_png(&layer.png).map_err(|e| anyhow!("layer {INDICES}/{}: {e}", layer.name))?;
        let indices = match picture.pixels {
            Pixels::Indexed { indices, .. } => indices.into_iter().map(u16::from).collect(),
            Pixels::Rgba(pixels) => pixels.into_iter().map(|p| codecs::texture::index_of_gray(p[0], bpp)).collect(),
        };
        Ok(Known { x: layer.x, y: layer.y, width: picture.width, height: picture.height, indices })
    }

    /// The index of a layer's pixel, the layer at `(lx, ly)`.
    fn at(&self, lx: i32, ly: i32, x: usize, y: usize) -> Option<u16> {
        let x = usize::try_from(i64::from(lx) - i64::from(self.x) + i64::try_from(x).ok()?).ok()?;
        let y = usize::try_from(i64::from(ly) - i64::from(self.y) + i64::try_from(y).ok()?).ok()?;
        (x < self.width && y < self.height).then(|| self.indices.get(y * self.width + x).copied()).flatten()
    }
}

/// Colors matched to a palette's indices: the first entry of the exact color, else the nearest.
struct Matcher {
    colors: Vec<[u8; 4]>,
    /// Each color met: its index, and whether the palette has it exactly.
    known: HashMap<[u8; 4], (u16, bool)>,
    /// The texels whose color the palette lacks, taken as the nearest it has.
    taken: usize,
}

impl Matcher {
    fn new(palette: &[u16]) -> Matcher {
        Matcher { colors: palette.iter().map(|&c| rgba(c)).collect(), known: HashMap::new(), taken: 0 }
    }

    fn index(&mut self, pixel: [u8; 4]) -> u16 {
        let colors = &self.colors;
        let (index, exact) = *self.known.entry(pixel).or_insert_with(|| {
            let distance = |c: &[u8; 4]| c.iter().zip(pixel).map(|(&a, b)| (i32::from(a) - i32::from(b)).pow(2)).sum::<i32>();
            let best = colors.iter().enumerate().min_by_key(|(_, c)| distance(c));
            best.map_or((0, false), |(i, c)| (u16::try_from(i).unwrap_or(0), distance(c) == 0))
        });
        if !exact {
            self.taken += 1;
        }
        index
    }

    /// What it took otherwise than the layer has it, said of `layer`; none where every color was its palette's.
    fn note(&self, layer: &str) -> Option<String> {
        let colors = self.known.values().filter(|(_, exact)| !exact).count();
        let s = |n: usize| if n == 1 { "" } else { "s" };
        (self.taken > 0).then(|| {
            format!(
                "layer {layer}: {} texel{} of {colors} color{} its palette lacks, taken as the nearest it has",
                self.taken,
                s(self.taken),
                s(colors)
            )
        })
    }
}

/// The stored texels and palettes, made from the source by the asset's settings.
///
/// # Errors
/// The source cannot be read, lacks its texels, or a layer holds what its depth cannot.
pub fn import(asset: &Asset, source: &[u8]) -> Result<TexelImage> {
    let document = ora::read(source).map_err(|e| anyhow!("{SOURCE}: {e}"))?;
    let (width, height) = (document.width as usize, document.height as usize);
    let bpp = bpp(asset);
    let names = palette_names(asset);

    // Palettes, by name: a strip's colors in a row.
    let strips: HashMap<&str, &Layer> =
        document.group(PALETTES).map_or_default(|g| g.layers().map(|l| (l.name.as_str(), l)).collect());
    let mut colors: Vec<Option<Vec<u16>>> = Vec::with_capacity(names.len());
    let mut notes = Vec::new();
    for name in &names {
        let Some(strip) = strips.get(name.as_str()) else {
            colors.push(None);
            continue;
        };
        let (w, _, values) = layer_values(strip, 16, &[], None, &mut notes)?;
        colors.push(Some(values[..w.min(values.len())].to_vec()));
    }
    let palette = |i: Option<usize>, bpp: u8| padded(i.and_then(|i| colors.get(i)?.as_deref()), bpp);

    // Each texel layer, bottom first: the texels, then the cells as the stack shows them.
    let base = document.layer(TEXELS_LAYER).ok_or_else(|| anyhow!("{SOURCE}: no layer {TEXELS_LAYER}"))?;
    let indices = document.group(INDICES);
    let known = indices.and_then(|g| g.layers().find(|l| l.name == TEXELS_LAYER)).map(|l| Known::read(l, bpp)).transpose()?;
    let mut painted = vec![painting(base, bpp, 0, &palette(Some(0), bpp), known.as_ref(), &mut notes)?];
    let declared: HashMap<&str, &Value> = asset.parts("cells").into_iter().collect();
    let mut settings = Table::new();
    settings.insert("size".into(), Value::Array(vec![Value::Integer(i64::from(document.width)), Value::Integer(i64::from(document.height))]));
    let mut rects = Table::new();
    let cell_layers: Vec<&Layer> = document.group(CELLS).map_or_default(|g| g.layers().collect());
    for layer in cell_layers.iter().rev() {
        let own = declared.get(layer.name.as_str()).and_then(|c| c.get("mode")).and_then(Value::as_str).and_then(crate::depth_of).unwrap_or(bpp);
        let cell_palette = declared
            .get(layer.name.as_str())
            .and_then(|c| c.get("palette"))
            .and_then(Value::as_str)
            .and_then(|p| names.iter().position(|n| n == p));
        // A cell of the image's depth is known by the texels' indices, one of another by its own.
        let own_known = if own == bpp {
            None
        } else {
            let index = indices.and_then(|g| g.group(CELLS)).and_then(|g| g.layers().find(|l| l.name == layer.name));
            index.map(|l| Known::read(l, own)).transpose()?
        };
        let known = if own == bpp { known.as_ref() } else { own_known.as_ref() };
        // Its texels are counted at its own depth from where its halfwords start, which is where it is drawn.
        let start = usize::try_from(layer.x).unwrap_or(0) * texels_per_halfword(own) / texels_per_halfword(bpp);
        let cell = painting(layer, own, start, &palette(cell_palette, own), known, &mut notes)?;
        let rect = [start, usize::try_from(layer.y).unwrap_or(0), cell.width, cell.height];
        let rect = rect.iter().map(|&v| Value::Integer(i64::try_from(v).unwrap_or(0))).collect();
        let mut fields = Table::new();
        fields.insert("rect".into(), Value::Array(rect));
        rects.insert(layer.name.clone(), Value::Table(fields));
        painted.push(cell);
    }
    // In the stack's order: the cells as it lists them, topmost first.
    let rects: Table = cell_layers.iter().filter_map(|l| Some((l.name.clone(), rects.get(&l.name)?.clone()))).collect();
    if !rects.is_empty() || !declared.is_empty() {
        settings.insert("cells".into(), Value::Table(rects));
    }

    // Every layer's texels, bottom first; then its repainted ones again, so a texel painted in one layer is not
    // undone by another that left it as it was.
    let mut texels = Texels { width, height, bpp, values: vec![0; width * height] };
    for edited in [false, true] {
        for layer in &painted {
            layer.apply(&mut texels, edited);
        }
    }
    let palettes = names.into_iter().zip(colors).collect();
    Ok(TexelImage { width, height, bpp, texels: texels.stored(), palettes, derived: settings, notes })
}

/// A texel layer's values at their places: where its texels start (at its own depth), and which it repainted - those
/// whose index differs from the one recorded for their place.
struct Painting {
    own: u8,
    start: usize,
    y: usize,
    width: usize,
    height: usize,
    values: Vec<u16>,
    repainted: Vec<bool>,
    /// The texels layer, placed by its offset in the image's texels rather than as a cell is.
    base: bool,
}

fn painting(
    layer: &Layer,
    own: u8,
    start: usize,
    palette: &[u16],
    known: Option<&Known>,
    notes: &mut Vec<String>,
) -> Result<Painting> {
    let (width, height, values) = layer_values(layer, own, palette, known, notes)?;
    let repainted = values
        .iter()
        .enumerate()
        .map(|(i, &v)| known.and_then(|k| k.at(layer.x, layer.y, i % width.max(1), i / width.max(1))).is_none_or(|k| k != v))
        .collect();
    let base = layer.name == TEXELS_LAYER;
    let y = usize::try_from(layer.y).unwrap_or(0);
    let start = if base { usize::try_from(layer.x).unwrap_or(0) } else { start };
    Ok(Painting { own, start, y, width, height, values, repainted, base })
}

impl Painting {
    /// Its texels into the image's: all of them, or only those it repainted.
    fn apply(&self, texels: &mut Texels, only_repainted: bool) {
        for row in 0..self.height {
            for i in 0..self.width {
                let at = row * self.width + i;
                if only_repainted && !self.repainted[at] {
                    continue;
                }
                let (x, y) = (self.start + i, self.y + row);
                if y >= texels.height {
                    continue;
                }
                if self.base {
                    if x < texels.width {
                        texels.values[y * texels.width + x] = self.values[at];
                    }
                } else {
                    texels.set_cell_texel(self.own, x, y, self.values[at]);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{export, import};
    use codecs::ora::{self, Node, Pixels};
    use pe2_ext::{Asset, Result, Source, Table, anyhow};
    use std::collections::{BTreeMap, HashMap};

    /// What an import made, as the tests compare it.
    struct Made {
        files: Vec<(String, Vec<u8>)>,
        settings: Table,
        notes: Vec<String>,
    }

    struct Lent {
        fields: Table,
        files: HashMap<String, Vec<u8>>,
    }

    impl Source for Lent {
        fn path(&self) -> &'static str {
            "test/atlas"
        }

        fn uid(&self) -> Option<&str> {
            None
        }

        fn fields(&self) -> &Table {
            &self.fields
        }

        fn declaration(&self) -> &'static str {
            ""
        }

        fn has_file(&self, name: &str) -> bool {
            self.files.contains_key(name)
        }

        fn read(&self, name: &str) -> Result<Vec<u8>> {
            self.files.get(name).cloned().ok_or_else(|| anyhow!("no {name}"))
        }
    }

    const RED: u16 = 0x001F;
    const BLUE: u16 = 0x7C00;

    fn stored(colors: &[u16]) -> Vec<u8> {
        colors.iter().flat_map(|c| c.to_le_bytes()).collect()
    }

    /// An 8 bpp atlas whose first palette repeats red, with a cell of its depth and one of 4 bpp.
    fn atlas() -> Lent {
        let fields: Table = toml::from_str(
            r#"
            kind = "image"
            role = "atlas"
            mode = "indexed_8bpp"
            size = [8, 2]
            palettes.warm = {}
            palettes.cold = {}
            cells.wide = { rect = [4, 0, 4, 2], palette = "cold" }
            cells.small = { rect = [0, 1, 4, 1], palette = "warm", mode = "indexed_4bpp" }
            "#,
        )
        .unwrap();
        let files = HashMap::from([
            ("native.texels".to_string(), vec![0, 1, 2, 3, 1, 2, 3, 0, 0x21, 0x03, 5, 6, 0, 1, 2, 3]),
            ("native.warm.colors".to_string(), stored(&[0, RED, RED, BLUE])),
            ("native.cold.colors".to_string(), stored(&[0, BLUE, BLUE, RED])),
        ]);
        Lent { fields, files }
    }

    /// The atlas made from a source instead of its stored files.
    fn imported(source: &[u8]) -> BTreeMap<String, Vec<u8>> {
        imported_with(source, atlas().fields).files.into_iter().collect()
    }

    fn imported_with(source: &[u8], fields: Table) -> Made {
        let lent = Lent { fields, files: HashMap::new() };
        let image = import(&Asset::new(&lent, Some("pe2.images@image"), Some("pe2.images@atlas")), source).unwrap();
        // As it is stored: its files, as its type encodes them.
        let pe2_ext::Data::Typed(typed) = <crate::Texels as pe2_ext::ty::ValueType>::encode(image) else { panic!("no files") };
        let files = typed
            .files
            .into_iter()
            .map(|f| match f.content {
                pe2_ext::Content::Inline(bytes) => (f.name, bytes),
                pe2_ext::Content::Stored { .. } => panic!("made stored"),
            })
            .collect();
        Made { files, settings: typed.settings, notes: typed.notes }
    }

    /// Every layer of a source as a painting program saves it, in color; `paint` changes one.
    /// A change to a layer's pixels, the layer by its path (`cells/wide`), its width given.
    type Paint<'a> = dyn Fn(&str, &mut Vec<[u8; 4]>, usize) + 'a;

    fn resaved(source: &[u8], paint: &Paint<'_>) -> Vec<u8> {
        fn walk(nodes: &mut [Node], path: &str, paint: &Paint<'_>) {
            for node in nodes {
                match node {
                    Node::Layer(l) => {
                        let picture = ora::decode_png(&l.png).unwrap();
                        let mut pixels = match picture.pixels {
                            Pixels::Indexed { indices, palette } => indices.iter().map(|&i| palette[usize::from(i)]).collect(),
                            Pixels::Rgba(pixels) => pixels,
                        };
                        paint(&format!("{path}{}", l.name), &mut pixels, picture.width);
                        l.png = codecs::texture::rgba_png(picture.width, picture.height, &pixels.concat()).unwrap();
                    }
                    Node::Group(g) => walk(&mut g.children, &format!("{path}{}/", g.name), paint),
                }
            }
        }
        let mut document = ora::read(source).unwrap();
        walk(&mut document.stack, "", paint);
        ora::write(&document, &[], &[])
    }

    fn native() -> BTreeMap<String, Vec<u8>> {
        atlas().files.into_iter().collect()
    }

    fn source() -> Vec<u8> {
        let lent = atlas();
        let asset = Asset::new(&lent, Some("pe2.images@image"), Some("pe2.images@atlas"));
        export(&asset, &crate::TexelImage::read(&asset).unwrap()).unwrap()
    }

    #[test]
    fn a_source_makes_the_stored_files_again() {
        assert_eq!(imported(&source()), native());
        // Saved in color by a painting program: the repeated reds keep their indices.
        assert_eq!(imported(&resaved(&source(), &|_, _, _| {})), native());
    }

    #[test]
    fn the_size_and_the_cells_rectangles_are_the_sources() {
        // Without them in the settings, as extraction writes them: the import gives them, the same.
        let fields: Table = toml::from_str(
            r#"
            mode = "indexed_8bpp"
            palettes.warm = {}
            palettes.cold = {}
            cells.wide = { palette = "cold" }
            cells.small = { palette = "warm", mode = "indexed_4bpp" }
            "#,
        )
        .unwrap();
        let made = imported_with(&source(), fields);
        assert_eq!(made.files.into_iter().collect::<BTreeMap<_, _>>(), native());
        let expected: Table = toml::from_str(
            "size = [8, 2]\ncells.wide = { rect = [4, 0, 4, 2] }\ncells.small = { rect = [0, 1, 4, 1] }",
        )
        .unwrap();
        assert_eq!(made.settings, expected);
    }

    #[test]
    fn a_repainted_texel_alone_changes() {
        let blue = codecs::texture::rgba(BLUE);
        let painted = resaved(&source(), &|layer, pixels, _| {
            if layer == "texels" {
                pixels[1] = blue;
            }
        });
        let mut expected = native();
        // Blue is the warm palette's fourth color; the second red (index 2) beside it stays.
        expected.get_mut("native.texels").unwrap()[1] = 3;
        assert_eq!(imported(&painted), expected);
    }

    #[test]
    fn a_color_its_palette_lacks_is_taken_as_the_nearest_and_said() {
        // Blue a shade off, twice: no palette has it, so the import takes the nearest, blue, and says so.
        let [r, g, b, a] = codecs::texture::rgba(BLUE);
        let off = [r, g, b.saturating_sub(3), a];
        let painted = resaved(&source(), &|layer, pixels, _| {
            if layer == "texels" {
                pixels[1] = off;
                pixels[2] = off;
            }
        });
        let made = imported_with(&painted, atlas().fields.clone());
        let texels = &made.files.iter().find(|(n, _)| n == "native.texels").unwrap().1;
        assert_eq!(texels[1..3], [3, 3]);
        assert_eq!(made.notes, ["layer texels: 2 texels of 1 color its palette lacks, taken as the nearest it has"]);
        // The source as exported: nothing said.
        assert_eq!(imported_with(&source(), atlas().fields.clone()).notes, Vec::<String>::new());
    }

    #[test]
    fn a_repainted_texel_wins_over_a_layer_that_left_it() {
        // The texels layer repainted beneath the wide cell, which still shows the old texel: the repaint holds.
        let blue = codecs::texture::rgba(BLUE);
        let painted = resaved(&source(), &|layer, pixels, _| {
            if layer == "texels" {
                pixels[4] = blue;
            }
        });
        let mut expected = native();
        expected.get_mut("native.texels").unwrap()[4] = 3;
        assert_eq!(imported(&painted), expected);
    }

    #[test]
    fn a_cell_wins_over_the_texels_beneath_it() {
        let red = codecs::texture::rgba(RED);
        let painted = resaved(&source(), &|layer, pixels, width| {
            if layer == "cells/wide" {
                pixels[width] = red;
            }
            if layer == "cells/small" {
                // Its first 4 bpp texel, the low nibble of the image's texel 0 of row 1.
                pixels[0] = codecs::texture::rgba(BLUE);
            }
        });
        let mut expected = native();
        let texels = expected.get_mut("native.texels").unwrap();
        // The wide cell's row 1, column 0 is texel 12: red is the cold palette's fourth color.
        texels[12] = 3;
        texels[8] = 0x23;
        assert_eq!(imported(&painted), expected);
    }
}
