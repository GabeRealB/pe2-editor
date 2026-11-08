//! pe2.images: the image type. What one is follows from what it declares: a BS picture has no `mode`, a texture does
//! (texels through its palettes), and an atlas has `cells` as well (each through its own palette).
//!
//! It registers what the host shows an image by: its picture in each of its looks and its palettes as parts, its
//! thumbnail, its fields' labels and readings, what is marked on it (an atlas's cells, a wide texture's pages), its
//! inspector sections (palettes, variants, cells), what agents are told of it and its
//! palette tool; and VRAM - its slots (`vram_slot`) and the panel showing what an asset uploads where.

mod extract;
mod inspector;
mod marks;
mod ora;
mod texels;
mod tools;
mod vram;

use pe2_ext::ty::{self, Files, Registered};
use pe2_ext::{
    Asset, ContentAction, ContentRow, Contents, Field, Icon, Image, PartField, PartSheet, Rect, Registry, Result,
    Setting, Signature, Tint, Typed, Value,
};
use std::collections::HashMap;
pub use texels::{TexelImage, Texels};

pe2_ext::extension!(register);

/// What an image is.
const IMAGE: &str = "pe2.images@image";

/// A texture or an atlas as layers a painting program opens, made back into its texels and palettes exactly.
fn ora_ops(r: &mut Registry) {
    let ora_source = r
        .op("ora_source")
        .label("OpenRaster layers, written")
        .input::<Texels>("native")
        .output::<ty::File<OpenRaster>>()
        .run(|call| {
            let image = call.take::<Texels>(0)?;
            let bytes = ora::export(&call.lend_settings(&image.derived).asset(), &image)?;
            // Written through the host: kept there as it comes, never held twice here.
            let mut file = call.writer(ora::SOURCE)?;
            std::io::Write::write_all(&mut file, &bytes)?;
            Ok(Typed::new(codecs::ora::MIMETYPE, vec![file.finish()?]))
        });
    let _ = r
        .op("ora")
        .label("OpenRaster layers")
        .input::<OpenRaster>("source")
        .output::<Texels>()
        .also::<ty::Table>("settings")
        .derives("size")
        .derives("cells.*.rect")
        .inverse(&ora_source)
        .priority(10)
        .run(|call| {
            let source = call.take::<OpenRaster>(0)?;
            let bytes = call.read(source.file()?)?;
            // What it makes comes with what it is set to and what it derives, for what reads an image.
            let mut image = ora::import(&call.lend_files(&[]).asset(), &bytes)?;
            image.derived = pe2_ext::merged(call.settings, &image.derived);
            call.make("settings", pe2_ext::Data::Table(image.derived.clone()));
            Ok(image)
        });
}

/// What a plan runs of them: a texture's native files from the chunks it was given (`extract::extract_texture`).
fn planned(r: &mut Registry) {
    let _ = r
        .op("extract_texture")
        .label("A texture, read from its chunks")
        .port("chunks", "bytes", true)
        .output_described("bytes")
        .run_described(|call| {
            let given = call.inputs.first().map_or(&[][..], Vec::as_slice);
            let chunks = given
                .iter()
                .map(|value| match value {
                    pe2_ext::Data::Typed(typed) => call.read(typed.file()?),
                    other => Err(pe2_ext::anyhow!("a chunk given as {}", other.ty())),
                })
                .collect::<Result<Vec<_>>>()?;
            let made = extract::extract_texture(chunks, call.settings)?;
            let files = made.into_iter().map(|(name, bytes)| pe2_ext::File::new(&name, bytes)).collect();
            Ok(pe2_ext::Data::Typed(pe2_ext::Typed::new("bytes", files)))
        });
}

#[expect(clippy::unnecessary_wraps, reason = "an entry point may fail; this one does not")]
fn register(r: &mut Registry) -> Result<()> {
    // An image's native form, as stored: a background's BS picture, or a texture's or atlas's texels and palettes.
    let _ = r.value_type::<BsPicture>();
    let _ = r.value_type::<Texels>();
    ora_ops(r);
    // An image in one of its looks, by name: `default`, or a variant's.
    let _ = r
        .op("draw")
        .label("Picture")
        .input::<NativeForm>("native")
        .output::<ty::Image>()
        .parameter::<String>("look")
        .priority(10)
        .field(
            Field::new("mode").label("Mode").choices(MODES).brief(|v| Some(format!("{} bpp", depth_of(v.as_str()?)?))),
        )
        .field(Field::new("size").read_only())
        .field(Field::new("palettes").custom())
        .field(Field::new("cells").custom())
        .field(Field::new("variants").custom())
        .members(|asset| {
            if has_picture(asset) { look_names(asset).into_iter().map(|n| (n.clone(), n)).collect() } else { vec![] }
        })
        .run(|call| {
            let look: String = call.parameter()?;
            match call.take::<NativeForm>(0)? {
                ty::Either::First(bs) => decode_bs(&call.read(bs.file()?)?),
                ty::Either::Second(image) => {
                    let lent = call.lend_settings(&image.derived);
                    let asset = lent.asset();
                    let names = look_names(&asset);
                    let index = names.iter().position(|n| *n == look);
                    let index = index
                        .ok_or_else(|| pe2_ext::anyhow!("it has no look {look}; its looks: {}", names.join(", ")))?;
                    Ok(draw(&asset, &image, index))
                }
            }
        });
    part_outputs(r);
    // What is marked on it: an atlas's cells, a wide texture's pages.
    let _ = r.op("marks").label("Cells and pages").input::<NativeForm>("native").output::<ty::Marks>().run(|call| {
        Ok(match call.take::<NativeForm>(0)? {
            ty::Either::First(_) => pe2_ext::Marks::default(),
            ty::Either::Second(image) => marks::of(&call.lend_settings(&image.derived).asset()),
        })
    });
    let _ = r
        .named_type("image")
        .alias("image")
        .label("Image")
        .of(BsPicture::NAME)
        .of(Texels::NAME)
        .icon(Icon::Image, Tint::Picture)
        .inspector_tab("Palettes", Icon::Palette, &["palettes", "variants"], |_| {
            Box::new(inspector::Collections::of(inspector::Shows::Palettes))
        })
        .inspector_tab("Cells", Icon::Grid, &["cells"], |_| {
            Box::new(inspector::Collections::of(inspector::Shows::Cells))
        })
        .editor("Cells", Icon::Grid, &["cells"], |_| Box::new(inspector::CellSheet::default()))
        .contents(contents)
        .content_act(inspector::act)
        .part_sheet(part_sheet)
        .contents_view("Summary", |_| Box::new(inspector::Summary))
        .register();
    drop(r.template("texture", IMAGE, &format!("{TEXTURE}{OFFERS}")).label("Texture, from layers").files(&["*.ora"]));
    drop(
        r.template("background", IMAGE, &format!("{BACKGROUND}{OFFERS}")).label("Background picture").files(&["*.bs"]),
    );
    planned(r);
    r.named_type("vram_slot")
        .alias("vram_slot")
        .label("VRAM slot")
        .icon(Icon::Grid, Tint::Picture)
        .data()
        .field(Field::new("rect").label("Rectangle").required())
        .editor("Place", Icon::Grid, &["rect"], |_| Box::new(vram::SlotPlace))
        .register();
    let grid = r.setting(
        Setting::new("vram_page_grid", true)
            .label("VRAM page grid")
            .help("Draw the texture pages' grid over VRAM, 64 by 256 halfwords a page.")
            .user(),
    );
    let grid = grid.as_bool().unwrap_or(true);
    r.panel("vram", "VRAM", Icon::Grid, move || Box::new(vram::Vram::new(grid)));
    r.tool("palette", tools::PALETTE, tools::PALETTE_ARGUMENTS, tools::palette);
    Ok(())
}

/// A palette's and a cell's pictures.
fn part_outputs(r: &mut Registry) {
    // A palette as a strip of its colors, and a cell where it is drawn, through a palette of its image's where one is
    // named (`skin@night`).
    let _ = r
        .op("palette_picture")
        .label("A palette's picture")
        .input::<NativeForm>("native")
        .output::<ty::Image>()
        .parameter::<String>("name")
        .members(|asset| palette_names(asset).into_iter().map(|name| (name.clone(), name)).collect())
        .run(|call| {
            let name: String = call.parameter()?;
            let ty::Either::Second(image) = call.take::<NativeForm>(0)? else {
                pe2_ext::bail!("a background has no palettes")
            };
            palette_picture(&image, &name).ok_or_else(|| pe2_ext::anyhow!("it has no palette {name}"))
        });
    // A cell as what reads its values sees it: the image again, told which cell (`tokens::leads_on`), so that
    // `PE2_VALUE(chrome, cell, prompt, _v)` reads the prompt's row of the chrome.
    let _ = r
        .op("cell")
        .label("A cell, as what reads it")
        .input::<NativeForm>("native")
        .output::<ty::Table>()
        .parameter::<String>("name")
        .members(|asset| {
            asset.parts("cells").into_iter().map(|(name, _)| (name.to_string(), name.to_string())).collect()
        })
        .run(|call| {
            let name: String = call.parameter()?;
            Ok(pe2_ext::tokens::leads_on(call.host.path(), "image", &[("cell", &name)]))
        });
    let _ = r
        .op("cell_picture")
        .label("A cell's picture")
        .input::<NativeForm>("native")
        .output::<ty::Image>()
        .parameter::<String>("name")
        .members(|asset| {
            asset.parts("cells").into_iter().map(|(name, _)| (name.to_string(), name.to_string())).collect()
        })
        .run(|call| {
            let name: String = call.parameter()?;
            let ty::Either::Second(image) = call.take::<NativeForm>(0)? else {
                pe2_ext::bail!("a background has no cells")
            };
            cell_picture(&call.lend_settings(&image.derived).asset(), &image, &name)
                .ok_or_else(|| pe2_ext::anyhow!("it has no cell {name}"))
        });
}

/// What an image offers, whichever way it is stored: each hanging off the layer named `image`.
const OFFERS: &str = r#"
[[stack]]
op = "pe2.images@draw"
native = { from = "image" }

[[stack]]
op = "preview"
name = "picture"

[[stack]]
op = "pe2.images@palette_picture"
native = { from = "image" }

[[stack]]
op = "output"
name = "palettes"

[[stack]]
op = "pe2.images@cell_picture"
native = { from = "image" }

[[stack]]
op = "output"
name = "cells"

[[stack]]
op = "pe2.images@cell"
native = { from = "image" }

[[stack]]
op = "output"
name = "cell"

[[stack]]
op = "pe2.images@marks"
native = { from = "image" }

[[stack]]
op = "output"
name = "marks"
"#;

/// An image of layers, turned into what the game stores when its source changes.
const TEXTURE: &str = r#"[[stack]]
op = "file"
file = "{file}"

[[stack]]
id = "image"
op = "pe2.images@ora"

[[stack]]
op = "output"
name = "native"
retain = "cache"
when = "eager"

[[stack]]
op = "output"
name = "settings"
value = { from = "image.settings" }
"#;

/// A picture stored as it is.
const BACKGROUND: &str = r#"[[stack]]
id = "image"
op = "file"
file = "{file}"

[[stack]]
op = "output"
name = "native"
"#;

/// An image's modes: what a texel is. A native-only image's texels are read in its mode; an imported one's are
/// converted to it, its palette generated or quantized to fit.
const MODES: [(&str, &str); 3] = [
    ("indexed_4bpp", "Indexed, 4 bpp (16 colors)"),
    ("indexed_8bpp", "Indexed, 8 bpp (256 colors)"),
    ("direct_16bpp", "Direct color, 16 bpp"),
];

/// A mode's depth, in bits a texel.
pub(crate) fn depth_of(mode: &str) -> Option<u8> {
    match mode {
        "indexed_4bpp" => Some(4),
        "indexed_8bpp" => Some(8),
        "direct_16bpp" => Some(16),
        _ => None,
    }
}

// --- the interface ------------------------------------------------------------------------------------------------

/// A texture or an atlas: an image of texels and palettes.
#[must_use]
pub fn is_texture(asset: &Asset) -> bool {
    asset.field("mode").is_some()
}

/// An atlas: texels in cells.
#[must_use]
pub fn is_atlas(asset: &Asset) -> bool {
    !asset.parts("cells").is_empty()
}

/// An image's palettes by name, in the order it declares them.
#[must_use]
pub fn palette_names(asset: &Asset) -> Vec<String> {
    asset.parts("palettes").into_iter().map(|(name, _)| name.to_string()).collect()
}

/// An image's looks, by name: a texture's variants (the first its default); an atlas's default and its variants.
#[must_use]
pub fn looks(asset: &Asset) -> Vec<String> {
    let variants = asset.parts("variants");
    let named = variants.iter().map(|(name, _)| (*name).to_string());
    if is_atlas(asset) && !variants.is_empty() {
        std::iter::once("default".to_string()).chain(named).collect()
    } else {
        named.collect()
    }
}

/// What is inside an image, by group: a palette and a cell shown by their pictures. A cell is dropped on the palette
/// that draws it, a variant renamed, moved and removed; a cell's name and rectangle are its source's.
fn contents(asset: &Asset, _: &dyn pe2_ext::Project) -> Contents {
    let palette_of: HashMap<String, Option<usize>> =
        cells(asset).into_iter().map(|cell| (cell.name, cell.palette)).collect();
    let names = palette_names(asset);
    let has_cells = !palette_of.is_empty();
    let shown_looks = looks(asset);
    let mut rows = Vec::new();
    let mut group = String::new();
    for (id, label) in listed(asset) {
        let (within, name) = id.split_once('/').unwrap_or(("", id.as_str()));
        if !within.is_empty() && within != group {
            within.clone_into(&mut group);
            let label = match within {
                "palettes" => "Palettes",
                "cells" => "Cells",
                _ => "Variants",
            };
            rows.push(ContentRow::heading(within, label));
        }
        let row = ContentRow::new(&id, &label).depth(u32::from(!within.is_empty()));
        rows.push(match within {
            "palettes" if has_cells => row
                .icon(Icon::Palette, Tint::Picture)
                .picture()
                .takes(&["cell"])
                .hint("Drop a cell here to draw it through this palette"),
            "palettes" => row.icon(Icon::Palette, Tint::Picture).picture(),
            "cells" => {
                let palette = palette_of.get(name).copied().flatten().and_then(|at| names.get(at));
                row.icon(Icon::Grid, Tint::Picture).picture().drags("cell").detail(palette.map_or("", String::as_str))
            }
            "variants" => {
                let remove = ContentAction::new("remove", "Remove the variant").icon(Icon::Cross).on_row();
                let row = row.icon(Icon::Image, Tint::Picture).renames().reorders("variant").action(remove);
                if shown_looks.iter().any(|look| look == name) { row.look(name) } else { row }
            }
            _ => row.icon(Icon::Image, Tint::Picture),
        });
    }
    let offered = !rows.is_empty() && is_texture(asset);
    let actions = offered.then(|| ContentAction::new("add", "Add variant").icon(Icon::Plus)).into_iter().collect();
    Contents { rows, actions, ..Contents::default() }
}

/// A cell's fields: its palette and depth are settings, its rectangle its source's.
fn part_sheet(asset: &Asset, _: &dyn pe2_ext::Project, part: &str) -> Option<PartSheet> {
    let name = part.strip_prefix("cells/")?;
    let (_, cell) = asset.parts("cells").into_iter().find(|(cell, _)| *cell == name)?;
    let palettes = palette_names(asset).into_iter().map(|p| (Value::String(p.clone()), p)).collect();
    let field = |key: &str, label: &str| PartField {
        key: key.to_string(),
        label: label.to_string(),
        value: cell.get(key).cloned(),
        absent: None,
        choices: vec![],
        read_only: false,
    };
    let depth = depth_of(asset.field("mode").and_then(Value::as_str).unwrap_or_default());
    let modes = MODES.iter().map(|(mode, label)| (Value::String((*mode).to_string()), (*label).to_string())).collect();
    Some(PartSheet {
        within: vec!["cells".to_string(), name.to_string()],
        fields: vec![
            PartField { choices: palettes, absent: Some("None".to_string()), ..field("palette", "Palette") },
            PartField {
                choices: modes,
                absent: Some(format!("As the image ({} bpp)", depth.unwrap_or_default())),
                ..field("mode", "Mode")
            },
            PartField { read_only: true, ..field("rect", "Rectangle") },
        ],
        problem: None,
    })
}

/// What the contents of an image list, by id and label: its texels, palettes, cells and variants.
fn listed(asset: &Asset) -> Vec<(String, String)> {
    if !is_texture(asset) {
        return vec![];
    }
    let mut out = vec![("texels".to_string(), "Texels".to_string())];
    out.extend(palette_names(asset).into_iter().map(|name| (format!("palettes/{name}"), name)));
    out.extend(asset.parts("cells").into_iter().map(|(name, _)| (format!("cells/{name}"), name.to_string())));
    let first = usize::from(is_atlas(asset));
    out.extend(looks(asset).into_iter().skip(first).map(|name| (format!("variants/{name}"), name)));
    out
}

/// The palette each part of an image takes in one of its looks: a texture's (`None` for its only cell, the image),
/// or each of an atlas's cells by name.
fn look(asset: &Asset, variant: usize) -> (Option<usize>, HashMap<String, usize>) {
    let names = palette_names(asset);
    let index = |name: Option<&str>| name.and_then(|n| names.iter().position(|p| p == n));
    let variants = asset.parts("variants");
    if is_atlas(asset) {
        let overrides = variant
            .checked_sub(1)
            .and_then(|v| variants.get(v)?.1.get("palettes")?.as_table().cloned())
            .unwrap_or_default();
        let chosen = overrides.iter().filter_map(|(cell, p)| Some((cell.clone(), index(p.as_str())?))).collect();
        return (None, chosen);
    }
    let palette = variants.get(variant).and_then(|(_, v)| v.get("palette")).and_then(Value::as_str);
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
    /// Where it is in the image, in the image's texels.
    pub rect: Rect,
    pub bpp: u8,
    /// Where it is in its own texels.
    pub source: Rect,
    pub palette: Option<usize>,
}

/// An atlas's cells.
#[must_use]
pub fn cells(asset: &Asset) -> Vec<Cell> {
    let bpp = bpp(asset);
    let names = palette_names(asset);
    asset
        .parts("cells")
        .into_iter()
        .filter_map(|(name, c)| {
            let source = rect(c.get("rect")?)?;
            let own = bpp_of(c.get("mode"), bpp);
            #[expect(clippy::cast_precision_loss, reason = "texels per halfword")]
            let scale = texels_per_halfword(bpp) as f32 / texels_per_halfword(own) as f32;
            // What it takes of the image: its halfwords, whatever its own depth makes of them.
            let rect = Rect { x: source.x * scale, width: source.width * scale, ..source };
            let palette = c.get("palette").and_then(Value::as_str).and_then(|p| names.iter().position(|n| n == p));
            Some(Cell { name: name.to_string(), rect, bpp: own, source, palette })
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

/// A palette's file: its colors, as the extraction (`extract`) and the import (`ora`) make them.
pub(crate) fn colors_file(palette: &str) -> String {
    format!("native.{palette}.colors")
}

pub(crate) fn halfwords(bytes: &[u8]) -> Vec<u16> {
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
pub(crate) const TEXELS: &str = "native.texels";

/// An image's looks by name, as its picture's members are named: its looks, or - where it has none - `default`.
fn look_names(asset: &Asset) -> Vec<String> {
    let looks = looks(asset);
    if looks.is_empty() { vec!["default".to_string()] } else { looks }
}

/// A background's native form: its BS picture, recognized by its name (it has no header to tell it by).
pub struct BsPicture;

impl Registered for BsPicture {
    const NAME: &str = "pe2.images@bs_picture";
    const LABEL: &str = "BS picture";

    fn signatures() -> Vec<Signature> {
        vec![Signature::Name(BS.into()), Signature::Name("*.bs".into())]
    }
}

impl Files for BsPicture {}

pe2_ext::media_type!(
    /// An `OpenRaster` image, by its media type: whoever recognizes it.
    pub OpenRaster = "image/openraster"
);

/// What an image's native form is, either way.
type NativeForm = ty::OneOf<(BsPicture, Texels)>;

/// Whether an image has a picture: a background's `native.bs`, or a texture's or atlas's stored texels.
pub(crate) fn has_picture(asset: &Asset) -> bool {
    asset.has_file(BS) || is_texture(asset) && asset.has_file(TEXELS)
}

/// A palette or a cell of an image as a picture (`palettes/<name>`, `cells/<name>`), read from its files.
pub(crate) fn output_picture(asset: &Asset, address: &str) -> Option<Result<Image>> {
    if !is_texture(asset) {
        return None;
    }
    let image = match TexelImage::read(asset) {
        Ok(image) => image,
        Err(e) => return Some(Err(e)),
    };
    if let Some(name) = address.strip_prefix("cells/") {
        return cell_picture(asset, &image, name).map(Ok);
    }
    palette_picture(&image, address.strip_prefix("palettes/")?).map(Ok)
}

/// A palette as a strip of its colors.
fn palette_picture(image: &TexelImage, name: &str) -> Option<Image> {
    let colors = image.palette(name)?;
    Some(Image { width: colors.len(), height: 1, rgba: colors.iter().flat_map(|&c| rgba(c)).collect() })
}

/// A cell where it is drawn; `skin@night` through a palette of the image's other than its own.
fn cell_picture(asset: &Asset, image: &TexelImage, name: &str) -> Option<Image> {
    let (name, through) = name.split_once('@').map_or((name, None), |(name, palette)| (name, Some(palette)));
    let cell = cells(asset).into_iter().find(|c| c.name == name)?;
    let through = match through {
        Some(palette) => Some((name, palette_names(asset).iter().position(|known| known == palette)?)),
        None => None,
    };
    Some(decode_cell(asset, &image.texels, &image.colors(), &cell, through))
}

/// A cell's pixels, each of its texels one: not cut from the image, where a cell of fewer bits a texel has less room than it
/// has texels.
pub(crate) fn decode_cell(
    asset: &Asset,
    texels: &[u8],
    colors: &[Option<Vec<u16>>],
    cell: &Cell,
    through: Option<(&str, usize)>,
) -> Image {
    let gray = codecs::texture::gray_ramp(cell.bpp);
    let (_, overrides) = look(asset, 0);
    let asked = through.filter(|(name, _)| *name == cell.name).map(|(_, palette)| palette);
    let chosen = asked.or_else(|| overrides.get(&cell.name).copied()).or(cell.palette);
    let palette: &[u16] = chosen.and_then(|i| colors.get(i)?.as_deref()).unwrap_or(&gray);
    let rows = cell_rows(asset, texels, cell, palette);
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "texel coordinates")]
    let width = cell.source.width as usize;
    let height = rows.len().checked_div(width).unwrap_or(0);
    Image { width, height, rgba: rows.concat() }
}

/// A cell's texels through a palette, row by row: a cell of another depth than its image's rebuilt from the halfwords holding
/// it.
fn cell_rows(asset: &Asset, texels: &[u8], cell: &Cell, palette: &[u16]) -> Vec<[u8; 4]> {
    const CLEAR: [u8; 4] = [0; 4];
    let bpp = bpp(asset);
    let per = texels_per_halfword(bpp);
    let (width, height) = pair(asset.field("size")).unwrap_or_default();
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "image sizes")]
    let (w, h) = (width as usize, height as usize);
    let values: Vec<u16> = if bpp == 16 { halfwords(texels) } else { texels.iter().map(|&b| u16::from(b)).collect() };
    let at = |x: usize, y: usize| values.get(y * w + x).copied().unwrap_or(0);
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "texel coordinates")]
    let (sx, sy, cw, ch) =
        (cell.source.x as usize, cell.source.y as usize, cell.source.width as usize, cell.source.height as usize);
    let mut rows = Vec::with_capacity(cw * ch);
    for y in sy..(sy + ch).min(h) {
        for i in 0..cw {
            let index = if cell.bpp == bpp {
                at(sx + i, y)
            } else {
                let own = texels_per_halfword(cell.bpp);
                let (word_x, k) = ((sx + i) / own, (sx + i) % own);
                let word = (0..per).fold(0u16, |word, j| word | at(word_x * per + j, y) << (16 / per * j));
                (word >> (16 / own * k)) & ((1 << (16 / own)) - 1)
            };
            rows.push(palette.get(usize::from(index)).map_or(CLEAR, |&c| rgba(c)));
        }
    }
    rows
}

/// Texels and palettes as a picture in a look (one past its looks, its first), with their image's settings.
pub(crate) fn draw(asset: &Asset, image: &TexelImage, look: usize) -> Image {
    let look = if look < looks(asset).len() { look } else { 0 };
    decode_texels(asset, &image.texels, &image.colors(), look, None)
}

/// An image's pixels in one of its looks: a texture through its variant's palette; an atlas faintly through its first
/// palette, and each of its cells through its own - the variant's where it gives the cell one - a deeper cell's texels
/// taken from the same halfwords at its depth. Where no colors are extracted (an executable uploads them) or none are
/// known, in gray.
fn decode_texels(
    asset: &Asset,
    texels: &[u8],
    colors: &[Option<Vec<u16>>],
    variant: usize,
    through: Option<(&str, usize)>,
) -> Image {
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
        for cell in &cells {
            let asked = through.filter(|(name, _)| *name == cell.name).map(|(_, palette)| palette);
            let colors = palette(asked.or_else(|| overrides.get(&cell.name).copied()).or(cell.palette));
            let rows = cell_rows(asset, texels, cell, colors);
            #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "texel coordinates")]
            let (sy, cw, dx, span) =
                (cell.source.y as usize, cell.source.width as usize, cell.rect.x as usize, cell.rect.width as usize);
            for (row, texels) in rows.chunks(cw.max(1)).enumerate() {
                let y = sy + row;
                if y >= h {
                    break;
                }
                // Each of the image's texels it takes shows the cell's nearest. A cell of more bits a texel takes more of
                // them than it is wide: the rest is its own halfwords read at the image's depth, and left blank.
                for x in 0..span {
                    if dx + x < w {
                        let i = if span < cw { x * cw / span.max(1) } else { x };
                        pixels[y * w + dx + x] = texels.get(i).copied().unwrap_or(CLEAR);
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
    use super::{TexelImage, cells, draw, has_picture, looks, palette_colors};
    use pe2_ext::{Asset, Result, Source, Table, anyhow};
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
    fn a_cell_of_fewer_bits_stays_in_the_room_it_has() {
        // Sixteen 4 bpp texels of each of two cells, side by side in a page of 8: each has eight of the page's texels.
        let declared = "mode = \"indexed_8bpp\"\nsize = [16, 1]\npalettes.warm = {}\npalettes.cold = {}\n\
                        cells.left = { rect = [0, 0, 16, 1], palette = \"warm\", mode = \"indexed_4bpp\" }\n\
                        cells.right = { rect = [16, 0, 16, 1], palette = \"cold\", mode = \"indexed_4bpp\" }\n";
        let (red, blue) = (0x001F_u16, 0x7C00_u16);
        let colors = |c: u16| [0u16, c].iter().flat_map(|c| c.to_le_bytes()).collect::<Vec<u8>>();
        let files = HashMap::from([
            ("native.texels", vec![0x11; 16]),
            ("native.warm.colors", colors(red)),
            ("native.cold.colors", colors(blue)),
        ]);
        let lent = Lent { fields: declared.parse().unwrap(), files };
        let asset = Asset::new(&lent, None);
        let rects: Vec<(f32, f32)> = cells(&asset).iter().map(|cell| (cell.rect.x, cell.rect.width)).collect();
        assert_eq!(rects, [(0.0, 8.0), (8.0, 8.0)]);
        let image = TexelImage::read(&asset).unwrap();
        let picture = draw(&asset, &image, 0);
        let pixel = |x: usize| picture.rgba[x * 4..x * 4 + 4].to_vec();
        assert_eq!(pixel(7), super::rgba(red));
        assert_eq!(pixel(8), super::rgba(blue), "the second is not under the first");
        // A cell alone has every texel it has, and none of its neighbor's.
        let alone = super::cell_picture(&asset, &image, "left").unwrap();
        assert_eq!((alone.width, alone.height), (16, 1));
        assert!(alone.rgba.chunks(4).all(|pixel| pixel == super::rgba(red)));
    }

    #[test]
    fn draws_an_atlas_cell_through_its_palette_and_its_variants() {
        let fields: Table = toml::from_str(
            r#"
            mode = "indexed_4bpp"
            size = [2, 1]
            palettes.warm = {}
            palettes.cold = {}
            cells.dot = { rect = [1, 0, 1, 1], palette = "warm" }
            variants.night = { palettes = { dot = "cold" } }

            [[stack]]
            op = "file"
            file = "native.texels"

            [[stack]]
            op = "pe2.images@texels"
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
        let asset = Asset::new(&lent, Some("pe2.images@image"));
        assert_eq!(looks(&asset), ["default", "night"]);
        assert_eq!(cells(&asset)[0].palette, Some(0));
        assert_eq!(palette_colors(&asset, 1).unwrap().unwrap(), [blue, blue]);
        assert!(has_picture(&asset));
        let default = draw(&asset, &TexelImage::read(&asset).unwrap(), 0);
        // Outside the cell: the first palette, faintly; the cell: its palette's second color, opaque green.
        assert_eq!(default.rgba[..4], [255, 0, 0, 63]);
        assert_eq!(default.rgba[4..], [0, 255, 0, 255]);
        let night = draw(&asset, &TexelImage::read(&asset).unwrap(), 1);
        assert_eq!(night.rgba[4..], [0, 0, 255, 255]);
    }
}
