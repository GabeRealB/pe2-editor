//! pe2.images: the image kind and its roles - a background (a BS picture), a texture (texels through its palettes),
//! an atlas (texels in cells, each cell through its own palette).
//!
//! It registers what the host shows an image by: its picture in each of its looks and its palettes as parts, its
//! thumbnail, its fields' labels and readings, its view (the picture on a canvas, with an atlas's cells or a wide
//! texture's pages over it), its inspector sections (palettes, variants, cells), what agents are told of it and its
//! palette tool; and VRAM - its slots (`vram_slot`) and the panel showing what an asset uploads where.

mod inspector;
mod tools;
mod view;
mod vram;

use pe2_ext::{Asset, Field, Icon, Image, Picture, Rect, Registry, Result, Tint, Value};
use std::collections::HashMap;

pe2_ext::extension!("pe2.images", "0.1.0", register);

#[expect(clippy::unnecessary_wraps, reason = "an entry point may fail; this one does not")]
fn register(r: &mut Registry) -> Result<()> {
    let image = r
        .kind("image")
        .alias("image")
        .label("Image")
        .icon(Icon::Image, Tint::Picture)
        .field(Field::new("mode").label("Mode").choices(MODES).brief(|v| Some(format!("{} bpp", depth_of(v.as_str()?)?))))
        .field(Field::new("palettes").custom())
        .field(Field::new("variants").custom())
        .field(Field::new("cells").custom())
        .picture(Pictures)
        .thumbnail(|asset, side| {
            Pictures.has(asset).then(|| Pictures.draw(asset, 0).map(|image| image.thumbnail(side)))
        })
        .view("Picture", |asset| Box::new(view::ImageView::new(asset)))
        .inspector("Collections", |_| Box::new(inspector::Collections::default()))
        .facts(tools::facts)
        .register();
    for (role, label) in [("background", "Background"), ("texture", "Texture"), ("atlas", "Atlas")] {
        r.role(&image, role).alias(role).label(label).register();
    }
    r.kind("vram_slot").alias("vram_slot").label("VRAM slot").icon(Icon::Grid, Tint::Picture).register();
    r.panel("vram", "VRAM", Icon::Grid, || Box::<vram::Vram>::default());
    r.tool("palette", tools::PALETTE, tools::PALETTE_ARGUMENTS, tools::palette);
    Ok(())
}

/// An image's modes: what a texel is. A native-only image's texels are read in its mode; an imported one's are
/// converted to it, its palette generated or quantized to fit.
const MODES: [(&str, &str); 3] = [
    ("indexed_4bpp", "Indexed, 4 bpp (16 colors)"),
    ("indexed_8bpp", "Indexed, 8 bpp (256 colors)"),
    ("direct_16bpp", "Direct color, 16 bpp"),
];

/// A mode's depth, in bits a texel.
fn depth_of(mode: &str) -> Option<u8> {
    match mode {
        "indexed_4bpp" => Some(4),
        "indexed_8bpp" => Some(8),
        "direct_16bpp" => Some(16),
        _ => None,
    }
}

// --- the interface ------------------------------------------------------------------------------------------------

/// An image's role (`texture`, `atlas`, `background`).
#[must_use]
pub fn role<'a>(asset: &Asset<'a>) -> Option<&'a str> {
    asset.role_of(ID)
}

/// A texture or an atlas: an image of texels and palettes.
#[must_use]
pub fn is_texture(asset: &Asset) -> bool {
    matches!(role(asset), Some("texture" | "atlas"))
}

/// An image's palettes by name, in the order it declares them.
#[must_use]
pub fn palette_names(asset: &Asset) -> Vec<String> {
    asset.array("palettes").iter().filter_map(|v| v.get("name")?.as_str().map(str::to_string)).collect()
}

/// An image's looks, by name: a texture's variants (the first its default); an atlas's default and its variants.
#[must_use]
pub fn looks(asset: &Asset) -> Vec<String> {
    let named = asset.array("variants").iter().filter_map(|v| v.get("name")?.as_str().map(str::to_string));
    if role(asset) == Some("atlas") && !asset.array("variants").is_empty() {
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
    let variants = asset.array("variants");
    if role(asset) == Some("atlas") {
        let overrides = variant
            .checked_sub(1)
            .and_then(|v| variants.get(v)?.get("palettes")?.as_table().cloned())
            .unwrap_or_default();
        let chosen = overrides.iter().filter_map(|(cell, p)| Some((cell.clone(), index(p.as_str())?))).collect();
        return (None, chosen);
    }
    let palette = variants.get(variant).and_then(|v| v.get("palette")).and_then(Value::as_str);
    (index(palette).or(Some(0)), HashMap::new())
}

fn int(value: Option<&Value>) -> Option<f32> {
    #[expect(clippy::cast_precision_loss, reason = "VRAM coordinates")]
    value.and_then(Value::as_integer).map(|v| v as f32)
}

/// A `[x, y]` pair.
#[must_use]
pub fn pair(value: Option<&Value>) -> Option<(f32, f32)> {
    let a = value?.as_array()?;
    Some((int(a.first())?, int(a.get(1))?))
}

/// A `rect = [x, y, w, h]` as a rectangle.
#[must_use]
pub fn rect(value: &Value) -> Option<Rect> {
    let r = value.as_array()?;
    Some(Rect { x: int(r.first())?, y: int(r.get(1))?, width: int(r.get(2))?, height: int(r.get(3))? })
}

/// The depth of a `mode`, in bits a texel; `default` where none is given.
fn bpp_of(mode: Option<&Value>, default: u8) -> u8 {
    mode.and_then(Value::as_str).and_then(depth_of).unwrap_or(default)
}

/// An image's depth, by its mode.
#[must_use]
pub fn bpp(asset: &Asset) -> u8 {
    bpp_of(asset.field("mode"), 8)
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
pub fn cells(asset: &Asset) -> Vec<Cell> {
    let bpp = bpp(asset);
    let names = palette_names(asset);
    asset
        .array("cells")
        .iter()
        .filter_map(|c| {
            let source = rect(c.get("rect")?)?;
            let own = bpp_of(c.get("mode"), bpp);
            #[expect(clippy::cast_precision_loss, reason = "texels per halfword")]
            let scale = texels_per_halfword(bpp) as f32 / texels_per_halfword(own) as f32;
            let rect = Rect { x: source.x * scale, ..source };
            let palette = c.get("palette").and_then(Value::as_str).and_then(|p| names.iter().position(|n| n == p));
            Some(Cell { name: c.get("name")?.as_str()?.to_string(), rect, bpp: own, source, palette })
        })
        .collect()
}

/// An image's size in texels, and its pages' width: 64 halfwords at its depth.
#[must_use]
pub fn pages(asset: &Asset) -> ((f32, f32), f32) {
    #[expect(clippy::cast_precision_loss, reason = "texels per halfword")]
    let per = texels_per_halfword(bpp(asset)) as f32;
    (pair(asset.field("size")).unwrap_or_default(), 64.0 * per)
}

/// A palette's file: its colors, as `project::textures` stores them.
fn colors_file(palette: &str) -> String {
    format!("native.{palette}.colors")
}

fn halfwords(bytes: &[u8]) -> Vec<u16> {
    bytes.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect()
}

/// A palette's colors, as ABGR1555 halfwords; none when the image has no such palette or its colors are not stored.
///
/// # Errors
/// Its colors cannot be read.
#[must_use]
pub fn palette_colors(asset: &Asset, palette: usize) -> Option<Result<Vec<u16>>> {
    let file = colors_file(palette_names(asset).get(palette)?);
    (is_texture(asset) && asset.has_file(&file)).then(|| asset.read(&file).map(|b| halfwords(&b)))
}

/// A color as unmultiplied RGBA.
#[must_use]
pub fn rgba(color: u16) -> [u8; 4] {
    codecs::texture::rgba(color)
}

// --- pictures -----------------------------------------------------------------------------------------------------

const BS: &str = "native.bs";
const TEXELS: &str = "native.texels";

/// What an image is drawn from: a background's `native.bs`, or an image's stored texels and its palettes' colors.
pub(crate) struct Pictures;

impl Picture for Pictures {
    fn has(&self, asset: &Asset) -> bool {
        asset.has_file(BS) || is_texture(asset) && asset.has_file(TEXELS)
    }

    fn looks(&self, asset: &Asset) -> Vec<String> {
        looks(asset)
    }

    fn part(&self, asset: &Asset, part: &str) -> Option<Result<Image>> {
        let name = part.strip_prefix("palettes/")?;
        let index = palette_names(asset).iter().position(|n| n == name)?;
        Some(palette_colors(asset, index)?.map(|colors| Image {
            width: colors.len(),
            height: 1,
            rgba: colors.iter().flat_map(|&c| rgba(c)).collect(),
        }))
    }

    fn draw(&self, asset: &Asset, look: usize) -> Result<Image> {
        if asset.has_file(BS) {
            return decode_bs(&asset.read(BS)?);
        }
        let look = if look < looks(asset).len() { look } else { 0 };
        let texels = asset.read(TEXELS)?;
        let colors: Vec<Option<Vec<u16>>> = palette_names(asset)
            .iter()
            .map(|p| asset.read(&colors_file(p)).ok().map(|b| halfwords(&b)))
            .collect();
        Ok(decode_texels(asset, &texels, &colors, look))
    }
}

/// An image's pixels in one of its looks: a texture through its variant's palette; an atlas faintly through its first
/// palette, and each of its cells through its own - the variant's where it gives the cell one - a deeper cell's texels
/// taken from the same halfwords at its depth. Where no colors are extracted (an executable uploads them) or none are
/// known, in gray.
fn decode_texels(asset: &Asset, texels: &[u8], colors: &[Option<Vec<u16>>], variant: usize) -> Image {
    const CLEAR: [u8; 4] = [0; 4];
    let bpp = bpp(asset);
    let (width, height) = pair(asset.field("size")).unwrap_or_default();
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
    let cells = cells(asset);
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
fn decode_bs(bytes: &[u8]) -> Result<Image> {
    let picture = codecs::bs::decode(bytes).map_err(|e| pe2_ext::anyhow!("{BS}: {e}"))?;
    let rgba = picture.rgb.as_chunks::<3>().0.iter().flat_map(|&[r, g, b]| [r, g, b, 255]).collect();
    Ok(Image { width: picture.width, height: picture.height, rgba })
}

#[cfg(test)]
mod tests {
    use super::{Pictures, cells, looks, palette_colors};
    use pe2_ext::{Asset, Picture, Result, Source, Table, anyhow};
    use std::collections::HashMap;

    /// An asset as a host would lend it: a declaration and its files.
    struct Lent {
        fields: Table,
        files: HashMap<&'static str, Vec<u8>>,
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

    /// ABGR1555 halfwords, as stored.
    fn colors(first: u16, second: u16) -> Vec<u8> {
        [first, second].iter().flat_map(|c| c.to_le_bytes()).collect()
    }

    #[test]
    fn draws_an_atlas_cell_through_its_palette_and_its_variants() {
        let fields: Table = toml::from_str(
            r#"
            kind = "image"
            role = "atlas"
            mode = "indexed_4bpp"
            size = [2, 1]
            palettes = [{ name = "warm" }, { name = "cold" }]
            cells = [{ name = "dot", rect = [1, 0, 1, 1], palette = "warm" }]
            variants = [{ name = "night", palettes = { dot = "cold" } }]
            "#,
        )
        .unwrap();
        // Opaque: bit 15 clear.
        let (red, green, blue) = (0x001F, 0x03E0, 0x7C00);
        let files = HashMap::from([
            ("native.texels", vec![0, 1]),
            ("native.warm.colors", colors(red, green)),
            ("native.cold.colors", colors(blue, blue)),
        ]);
        let lent = Lent { fields, files };
        let asset = Asset::new(&lent, Some("pe2.images@image"), Some("pe2.images@atlas"));
        assert_eq!(looks(&asset), ["default", "night"]);
        assert_eq!(cells(&asset)[0].palette, Some(0));
        assert_eq!(palette_colors(&asset, 1).unwrap().unwrap(), [blue, blue]);
        assert!(Pictures.has(&asset));
        let default = Pictures.draw(&asset, 0).unwrap();
        // Outside the cell: the first palette, faintly; the cell: its palette's second color, opaque green.
        assert_eq!(default.rgba[..4], [255, 0, 0, 63]);
        assert_eq!(default.rgba[4..], [0, 255, 0, 255]);
        let night = Pictures.draw(&asset, 1).unwrap();
        assert_eq!(night.rgba[4..], [0, 0, 255, 255]);
    }
}
