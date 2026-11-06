//! pe2.fonts: encodings, font faces from UFO 3, and the atlases - texture pages and glyph tables - the game draws them
//! from.
//!
//! - An `encoding` is data: which character each code stands for (`encoding.rs`).
//! - A `font` is a face: its UFO folder (`<name>.ufo`), which a font editor opens and which holds its glyphs and their
//!   names, and on it its binding - what the game's table of it is: its `encoding`, the codes it reads from `first`
//!   (`count` of them), its own `characters` for codes over the encoding's, what a character the font lacks draws
//!   (`missing`, a `character` and the code it does so `from`), the codes drawing `nothing`, the pixels its kerning
//!   classes tighten a pair by (`kerning`), its `family` and `style`.
//! - A `font_atlas` makes a page and its faces' tables (`atlas.rs`): its `faces`, each with its `bias` and `band` of
//!   the page's rows, the page's `size`, its `background` and `gap` indices, the `blank` cell a glyph without art draws,
//!   and `layout.toml`: by face, its `rows`, each a `baseline` and its `cells`' x by a character the glyph draws, and the
//!   records it `kept` that no glyph makes, by code. Placed (`mode = "placed"`, the default), every glyph a code draws
//!   must be placed; its `packed` output is the layout with what is not packed into rows of its own, which its
//!   contents' "Pack unplaced" writes. Generated (`mode = "generated"`), it holds the glyphs of its `text` alone, in the
//!   order the text first uses them, laid out by a rule (`atlas::Flow`: `start`, `row_height`, `spacing`, `align`,
//!   `wrap`), but where its layout places a character - the flow going on from there.
//!
//! Extraction reads a page and its faces' tables into the faces' UFOs and the atlas's layout, and holds what they make
//! again to the disc's bytes (`extract.rs`).

pub mod atlas;
pub mod encoding;
mod extract;
pub mod face;
pub mod raster;
pub mod render;

use pe2_ext::ty::{self, Files, Registered};
use pe2_ext::{
    Call, ContentAction, ContentOutcome, ContentRequest, ContentRow, Contents, Data, Field, Icon, Registry, Result,
    Signature, Table, Tint, Typed, Value, anyhow, bail,
};

use atlas::{FaceLayout, FaceSpec, Flow, Layout, PageSpec, Record, Row};
use encoding::Encoding;

pe2_ext::extension!(register);

/// A UFO 3 font: a folder named `*.ufo`.
pub struct UfoFont;

impl Registered for UfoFont {
    const NAME: &str = "pe2.fonts@ufo";
    const LABEL: &str = "UFO font";

    fn signatures() -> Vec<Signature> {
        vec![Signature::Name("*.ufo".into())]
    }
}

impl Files for UfoFont {}

/// An atlas's page: a texel a byte, row by row (`native.texels`, as `pe2.images` holds a texture's).
pub struct AtlasPage;

impl Registered for AtlasPage {
    const NAME: &str = "pe2.fonts@page";
    const LABEL: &str = "Font atlas page";
}

impl Files for AtlasPage {}

/// A face's stack: its UFO folder, its native form, what is set on it on its layer; a line of text as the game draws it
/// with the face, and its glyphs.
const FACE: &str = r#"[[stack]]
id = "ufo"
op = "file"
file = "{file}"

[[stack]]
op = "output"
name = "native"

[[stack]]
op = "pe2.fonts@sample"
source = { from = "ufo" }

[[stack]]
op = "preview"
name = "sample"

[[stack]]
op = "pe2.fonts@sheet"
source = { from = "ufo" }

[[stack]]
op = "preview"
name = "glyphs"
"#;

/// What a face's sample says where its settings set none.
const SAMPLE: &str = "The quick brown fox jumps over the lazy dog.\n0123456789 !?&()+-:',\"\n○ △ ✕ □";

/// A face's glyphs, read from its UFO's files.
fn glyphs_of(call: &mut Call) -> Result<Vec<atlas::Glyph>> {
    let source = call.take::<UfoFont>(0)?;
    let files = source.files.iter().map(|file| Ok((file.name.clone(), call.read(file)?))).collect::<Result<_>>()?;
    let font = codecs::ufo::read(&files).map_err(|e| anyhow!(e))?;
    face::from_ufo(&font, &raster_of(&source.settings)?).map_err(|e| anyhow!(e))
}

/// How a face draws its glyphs' outlines, from its settings: the em in texels (`pixels`; else a unit a texel) and the
/// ring's width (`outline`, a texel where unset, 0 none).
fn raster_of(face: &Table) -> Result<raster::Raster> {
    let number = |key: &str| {
        face.get(key)
            .map(|v| {
                v.as_float()
                    .or_else(|| v.as_integer().and_then(|n| i32::try_from(n).ok()).map(f64::from))
                    .filter(|n| *n >= 0.0)
                    .ok_or_else(|| anyhow!("its {key} is not a size"))
            })
            .transpose()
    };
    let default = raster::Raster::default();
    Ok(raster::Raster {
        pixels_per_em: number("pixels")?,
        outline: number("outline")?.unwrap_or(default.outline),
        ..default
    })
}

/// An atlas's stack: its layout, what lies under its glyphs (`under.png`, where it has any), its layout with what it does
/// not place packed (what "Pack unplaced" writes), and the layout made with its faces into its page and their tables -
/// the atlas's settings set on the packing, which hands them on.
const ATLAS: &str = r#"[[stack]]
id = "layout"
op = "file"
file = "layout.toml"

[[stack]]
id = "under"
op = "file"
file = "under.png"
optional = true

[[stack]]
id = "pack"
op = "pe2.fonts@pack"
layout = { from = "layout" }

[[stack]]
op = "output"
name = "packed"

[[stack]]
op = "pe2.fonts@atlas"
layout = { from = "layout" }
atlas = { from = "pack.settings" }
under = { from = "under" }

[[stack]]
op = "output"
name = "page"
retain = "cache"
"#;

/// The file an atlas's layout is.
const LAYOUT: &str = "layout.toml";
/// An atlas's modes: each glyph where its layout places it, or its text's glyphs packed by a rule, the layout placing
/// those it names.
const PLACED: &str = "placed";
const GENERATED: &str = "generated";

/// A generated atlas's rule, from its settings (`text`, `start`, `row_height`, `spacing`, `align`, `wrap`); none for a
/// placed one.
pub(crate) fn flow_of(given: &Table, page: &PageSpec) -> Result<Option<Flow>> {
    match given.get("mode").and_then(Value::as_str).unwrap_or(PLACED) {
        PLACED => return Ok(None),
        GENERATED => {}
        other => bail!("no mode {other} ({PLACED}, {GENERATED})"),
    }
    let number = |key: &str| -> Result<Option<usize>> {
        given
            .get(key)
            .map(|v| {
                v.as_integer().and_then(|n| usize::try_from(n).ok()).ok_or_else(|| anyhow!("its {key} is not a count"))
            })
            .transpose()
    };
    let start = match given.get("start").and_then(Value::as_array) {
        Some(at) => {
            let at = |i: usize| {
                at.get(i)
                    .and_then(Value::as_integer)
                    .and_then(|n| usize::try_from(n).ok())
                    .ok_or_else(|| anyhow!("its start is an x and a y"))
            };
            (at(0)?, at(1)?)
        }
        None => (0, 0),
    };
    Ok(Some(Flow {
        text: given.get("text").and_then(Value::as_str).map(str::to_string),
        start,
        row_height: number("row_height")?,
        spacing: number("spacing")?.unwrap_or(0),
        align: number("align")?.unwrap_or(1).max(1),
        wrap: number("wrap")?.unwrap_or(page.width),
    }))
}

/// A texture whose texels are an atlas's page (`atlas`): what follows its first layer is any texture's.
const TEXTURE: &str = r#"[[stack]]
id = "image"
op = "pe2.fonts@texture"

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

/// The type a texture's texels and palettes are, `pe2.images`'s: a texel a byte (`native.texels`), each stored
/// palette's colors, and the settings made with them.
const TEXELS: &str = "pe2.images@texels";

/// `pe2.images`'s texels, named: what its drawing, palettes and cells take.
struct Texels;

impl ty::ValueType for Texels {
    type Value = Typed;

    fn ty() -> pe2_ext::Type {
        pe2_ext::Type::Named(TEXELS.to_string())
    }

    fn decode(data: Data, _: &Call<'_>) -> Result<Typed> {
        match data {
            Data::Typed(typed) if typed.ty == TEXELS => Ok(typed),
            other => bail!("given {}, not {TEXELS}", other.ty()),
        }
    }

    fn encode(value: Typed) -> Data {
        Data::Typed(value)
    }
}

/// What a node's settings are, read by another's op.
const SETTINGS: &str = "settings";

#[expect(clippy::unnecessary_wraps, reason = "an entry point may fail; this one does not")]
fn register(r: &mut Registry) -> Result<()> {
    let ufo = r.value_type::<UfoFont>();
    let page = r.value_type::<AtlasPage>();
    r.named_type("encoding")
        .alias("encoding")
        .label("Encoding")
        .icon(Icon::Text, Tint::Structure)
        .data()
        .extracted()
        .field(Field::new("base").label("Base"))
        .field(Field::new("first").label("First code"))
        .field(Field::new("characters").label("Characters"))
        .register();
    let face = r
        .named_type("font")
        .alias("font")
        .label("Font face")
        .of(&ufo.to_string())
        .icon(Icon::Text, Tint::Plain)
        .field(Field::new("encoding").label("Encoding"))
        .field(Field::new("first").label("First code"))
        .field(Field::new("count").label("Codes"))
        .field(Field::new("format").label("Table").choices([
            ("font_glyph", "Glyph records (12 bytes)"),
            ("glyph_cell", "Glyph cells (4 bytes)"),
            ("stf_glyph", "Credits cells (4 bytes, 2 pages)"),
        ]))
        .field(Field::new("characters").label("Characters"))
        .field(Field::new("missing").label("Missing characters draw"))
        .field(Field::new("nothing").label("Codes drawing nothing"))
        .field(Field::new("kerning").label("Kerning pixels"))
        .field(Field::new("pixels").label("Em, in texels"))
        .field(Field::new("outline").label("Outline width"))
        .register();
    drop(r.template("font", "pe2.fonts@font", FACE).label("Font face, from UFO").files(&["*.ufo"]));
    let _ = r
        .op("sample")
        .label("A line of text, as the game draws it")
        .input::<UfoFont>("source")
        .output::<ty::Image>()
        .run(|call| {
            let given = call.given();
            let text = given.get("sample").and_then(Value::as_str).unwrap_or(SAMPLE).to_string();
            let kerning =
                given.get("kerning").and_then(Value::as_integer).and_then(|k| u8::try_from(k).ok()).unwrap_or(0);
            Ok(render::lines(&glyphs_of(call)?, &text, kerning))
        });
    let _ = r
        .op("sheet")
        .label("Its glyphs")
        .input::<UfoFont>("source")
        .output::<ty::Image>()
        .run(|call| Ok(render::sheet(&glyphs_of(call)?)));
    let atlas = register_atlas(r, &page.to_string());
    let _ = r.extractor("faces", &face, &[], extract::begin);
    let _ = r.extractor("atlases", &atlas, &[], extract::begin);
    Ok(())
}

/// The atlas's operations and type, and the texture drawn from one: the atlas type, for its extractor.
fn register_atlas(r: &mut Registry, page: &str) -> pe2_ext::Name {
    let _ = r
        .op("atlas")
        .label("Its page and its faces' glyph tables")
        .input::<ty::Bytes>("layout")
        .input::<ty::Table>("atlas")
        .input::<ty::Bytes>("under")
        .output::<AtlasPage>()
        .also::<ty::Bytes>("tables")
        .prepared_by_running()
        .run(make_atlas);
    let _ = r
        .op("pack")
        .label("Its layout, what it does not place packed")
        .input::<ty::Bytes>("layout")
        .output::<ty::Bytes>()
        .also::<ty::Table>("settings")
        .prepared_by_running()
        .run(pack_layout);
    let atlas = r
        .named_type("font_atlas")
        .alias("font_atlas")
        .label("Font atlas")
        .of(page)
        .icon(Icon::Grid, Tint::Picture)
        .field(Field::new("size").label("Size"))
        .field(Field::new("background").label("Background index"))
        .field(Field::new("gap").label("Index between bands"))
        .field(Field::new("blank").label("Blank cell"))
        .field(Field::new("pages").label("Pages"))
        .field(
            Field::new("mode")
                .label("Placement")
                .choices([(PLACED, "Placed: where its layout says"), (GENERATED, "Generated: its text, packed")]),
        )
        .field(Field::new("text").label("Text"))
        .field(Field::new("start").label("Starts at"))
        .field(Field::new("row_height").label("Row height"))
        .field(Field::new("spacing").label("Spacing"))
        .field(Field::new("align").label("Column"))
        .field(Field::new("wrap").label("Right edge"))
        .field(Field::new("faces").label("Faces"))
        .stack(ATLAS)
        .contents(atlas_contents)
        .content_act(atlas_act)
        .register();
    // A texture drawn from an atlas's page: the page as its texels, what is set on it (its size, mode, palettes) with
    // them; what the page's palettes' colors are, its executable uploads.
    let _ = r
        .op("texture")
        .label("An atlas's page, as a texture")
        .output::<Texels>()
        .also::<ty::Table>("settings")
        .prepared_by_running()
        .run(atlas_texture);
    drop(
        r.template("font_texture", "pe2.images@image", &format!("{TEXTURE}{OFFERS}"))
            .label("Texture, from a font atlas"),
    );
    atlas
}

/// A texture's texels drawn from an atlas's page (`atlas`), one page of several where it says which (`atlas_page`), and
/// what is set on it.
fn atlas_texture(call: &mut Call) -> Result<Typed> {
    let given = call.given();
    let atlas = given.get("atlas").and_then(pe2_ext::target_of).ok_or_else(|| anyhow!("it names no atlas"))?;
    let Data::Typed(page) = call.output_of(atlas, "page")? else { bail!("{atlas}: its page is no file") };
    let file = page.file()?.clone();
    call.make("settings", Data::Table(given.clone()));
    // One page of an atlas of several (`atlas_page`, from 0): its rows of the pages, one below another.
    let content = match given.get("atlas_page") {
        None => file.content,
        Some(sheet) => {
            let sheet = sheet
                .as_integer()
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(|| anyhow!("its atlas_page is not a page"))?;
            let dimensions =
                given.get("size").and_then(Value::as_array).ok_or_else(|| anyhow!("its size is not set"))?;
            let side = |i: usize| {
                dimensions
                    .get(i)
                    .and_then(Value::as_integer)
                    .and_then(|n| usize::try_from(n).ok())
                    .ok_or_else(|| anyhow!("its size is two numbers"))
            };
            let length = side(0)? * side(1)?;
            let texels = call.read(&file)?;
            let one = texels
                .get(sheet * length..(sheet + 1) * length)
                .ok_or_else(|| anyhow!("{atlas} has no page {sheet}"))?;
            let mut writer = call.writer("native.texels")?;
            std::io::Write::write_all(&mut writer, one)?;
            writer.finish()?.content
        }
    };
    Ok(Typed {
        ty: TEXELS.to_string(),
        files: vec![pe2_ext::File { name: "native.texels".into(), content }],
        settings: given,
        ..Typed::default()
    })
}

/// A face's spec on an atlas, from the face's settings and the atlas's entry for it.
fn face_spec(call: &Call, name: &str, face: &Table, entry: &Table) -> Result<FaceSpec> {
    let integer = |table: &Table, key: &str| {
        table.get(key).and_then(Value::as_integer).ok_or_else(|| anyhow!("{name}: no {key}"))
    };
    let encoding =
        face.get("encoding").and_then(pe2_ext::target_of).ok_or_else(|| anyhow!("{name}: it names no encoding"))?;
    let mut encoding = Encoding::of(&call.table_of(encoding, SETTINGS)?).map_err(|e| anyhow!("{encoding}: {e}"))?;
    apply_characters(&mut encoding, face).map_err(|e| anyhow!("{name}: {e}"))?;
    let (missing, nothing) = binding(face).map_err(|e| anyhow!("{name}: {e}"))?;
    let format = format_of(face).map_err(|e| anyhow!("{name}: {e}"))?;
    let band = entry.get("band").and_then(Value::as_array).ok_or_else(|| anyhow!("{name}: its band is not set"))?;
    let row = |i: usize| {
        band.get(i)
            .and_then(Value::as_integer)
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(|| anyhow!("{name}: its band is two rows"))
    };
    Ok(FaceSpec {
        name: name.to_string(),
        first: u32::try_from(integer(face, "first")?)?,
        count: usize::try_from(integer(face, "count")?)?,
        bias: u8::try_from(integer(entry, "bias")?.rem_euclid(256))?,
        band: (row(0)?, row(1)?),
        encoding,
        missing,
        nothing,
        only: None,
        format,
    })
}

/// A face's own characters for codes, over its encoding's (`characters = { "0x24" = "○" }`).
fn apply_characters(encoding: &mut Encoding, face: &Table) -> std::result::Result<(), String> {
    for (code, c) in face.get("characters").and_then(Value::as_table).into_iter().flatten() {
        let code = encoding::parse_code(code)?;
        let c = c.as_str().and_then(encoding::single).ok_or_else(|| format!("{code:#X}: not one character"))?;
        encoding.chars.insert(code, c);
    }
    Ok(())
}

/// How a face's table writes its records (`format`: `font_glyph`, the default, or `glyph_cell`).
pub(crate) fn format_of(face: &Table) -> std::result::Result<atlas::Format, String> {
    match face.get("format").and_then(Value::as_str) {
        None => Ok(atlas::Format::FontGlyph),
        Some(name) => atlas::Format::named(name)
            .ok_or_else(|| format!("no table format {name} (font_glyph, glyph_cell, stf_glyph)")),
    }
}

/// What a character a font lacks draws, from which code on, and the codes drawing nothing.
type Binding = (Option<(char, u32)>, Vec<u32>);

/// What a face's binding says a character its font lacks draws (`missing = { character = " ", from = 0x7F }`), and the
/// codes drawing nothing (`nothing = [0x81]`).
fn binding(face: &Table) -> std::result::Result<Binding, String> {
    let missing = match face.get("missing") {
        None => None,
        Some(missing) => {
            let c = missing
                .get("character")
                .and_then(Value::as_str)
                .and_then(encoding::single)
                .ok_or("missing: its character is not one character")?;
            let from = missing.get("from").and_then(Value::as_integer).and_then(|n| u32::try_from(n).ok()).unwrap_or(0);
            Some((c, from))
        }
    };
    let nothing = face
        .get("nothing")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|code| {
            code.as_integer().and_then(|n| u32::try_from(n).ok()).ok_or_else(|| "nothing: not a code".to_string())
        })
        .collect::<std::result::Result<_, _>>()?;
    Ok((missing, nothing))
}

/// A page's spec, from an atlas's settings.
fn page_spec(given: &Table) -> Result<PageSpec> {
    let size = given.get("size").and_then(Value::as_array).ok_or_else(|| anyhow!("its size is not set"))?;
    let dimension = |i: usize| {
        size.get(i)
            .and_then(Value::as_integer)
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(|| anyhow!("its size is two numbers"))
    };
    let index = |key: &str| {
        given
            .get(key)
            .and_then(Value::as_integer)
            .and_then(|n| u8::try_from(n).ok())
            .ok_or_else(|| anyhow!("its {key} index is not set"))
    };
    let blank = match given.get("blank") {
        None => None,
        Some(cell) => {
            let cell = cell.as_array().ok_or_else(|| anyhow!("its blank cell is a u and a v"))?;
            let at = |i: usize| {
                cell.get(i)
                    .and_then(Value::as_integer)
                    .and_then(|n| u8::try_from(n).ok())
                    .ok_or_else(|| anyhow!("its blank cell is a u and a v"))
            };
            Some((at(0)?, at(1)?))
        }
    };
    let pages = given.get("pages").map_or(Ok(1), |n| {
        n.as_integer()
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n > 0)
            .ok_or_else(|| anyhow!("its pages are not a count"))
    })?;
    Ok(PageSpec {
        width: dimension(0)?,
        height: dimension(1)?,
        background: index("background")?,
        gap: index("gap")?,
        blank,
        under: None,
        pages,
    })
}

/// A layout's text: by face, its `rows` (each a `baseline` and its `cells`' x by character) and the records it keeps
/// (`kept`, by code: `cell` - u, v, width, height - `offset`, `advance`, `classes` - left, right - and, where not 0,
/// `advance_y` and `unread`).
///
/// # Errors
/// It is not TOML, or a row or a kept record lacks what it needs.
pub fn layout_of(text: &str) -> Result<Layout> {
    let table: Table = toml::from_str(text)?;
    let mut layout = Layout::default();
    for (face, entry) in &table {
        let mut out = FaceLayout::default();
        for row in entry.get("rows").and_then(Value::as_array).into_iter().flatten() {
            let baseline = row
                .get("baseline")
                .and_then(Value::as_integer)
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(|| anyhow!("{face}: a row without its baseline"))?;
            let cells =
                row.get("cells").and_then(Value::as_table).ok_or_else(|| anyhow!("{face}: a row without its cells"))?;
            let mut cells = cells
                .iter()
                .map(|(c, x)| {
                    let one = encoding::single(c).ok_or_else(|| anyhow!("{face}: {c:?} is not one character"))?;
                    Ok((
                        one,
                        x.as_integer()
                            .and_then(|n| usize::try_from(n).ok())
                            .ok_or_else(|| anyhow!("{face}: {c:?}'s x"))?,
                    ))
                })
                .collect::<Result<Vec<_>>>()?;
            cells.sort_by_key(|(_, x)| *x);
            out.rows.push(Row { baseline, cells });
        }
        for (code, record) in entry.get("kept").and_then(Value::as_table).into_iter().flatten() {
            let code = encoding::parse_code(code).map_err(|e| anyhow!("{face}: {e}"))?;
            out.kept.insert(code, kept_of(record).map_err(|e| anyhow!("{face}: {code:#X}: {e}"))?);
        }
        layout.faces.insert(face.clone(), out);
    }
    Ok(layout)
}

/// A kept record, from its table.
fn kept_of(record: &Value) -> std::result::Result<Record, String> {
    let numbers = |key: &str, n: usize| -> std::result::Result<Vec<i64>, String> {
        match record.get(key) {
            None => Ok(vec![0; n]),
            Some(value) => {
                let items: Vec<i64> = value.as_array().into_iter().flatten().filter_map(Value::as_integer).collect();
                if items.len() == n { Ok(items) } else { Err(format!("its {key} is {n} numbers")) }
            }
        }
    };
    let byte = |v: i64| u8::try_from(v).map_err(|_| format!("{v} is not a byte"));
    let signed = |v: i64| i8::try_from(v).map_err(|_| format!("{v} is not a signed byte"));
    let one = |key: &str| record.get(key).and_then(Value::as_integer).unwrap_or(0);
    let (cell, offset, classes, unread) =
        (numbers("cell", 4)?, numbers("offset", 2)?, numbers("classes", 2)?, numbers("unread", 2)?);
    Ok(Record {
        u: byte(cell[0])?,
        v: byte(cell[1])?,
        width: byte(cell[2])?,
        height: byte(cell[3])?,
        x_offset: signed(offset[0])?,
        y_offset: signed(offset[1])?,
        advance_extra: signed(one("advance"))?,
        advance_y: signed(one("advance_y"))?,
        left: byte(classes[0])?,
        right: byte(classes[1])?,
        unread: [byte(unread[0])?, byte(unread[1])?],
        page: byte(one("page"))?,
    })
}

/// A layout as its text: each row's cells in order across, on a line of their own.
#[must_use]
pub fn layout_text(layout: &Layout) -> String {
    use std::fmt::Write as _;
    let key = |text: &str| Value::String(text.to_string()).to_string();
    let mut out = String::from(
        "# Where each glyph's cell is on the page, by a character it draws: a face's rows, each its baseline and its cells' x.\n\
         # What a face's table holds that no glyph makes is kept as it is, by code.\n",
    );
    for (face, entry) in &layout.faces {
        for row in &entry.rows {
            let _ = write!(out, "\n[[{face}.rows]]\nbaseline = {}\ncells = {{ ", row.baseline);
            let cells: Vec<String> = row.cells.iter().map(|(c, x)| format!("{} = {x}", key(&c.to_string()))).collect();
            let _ = writeln!(out, "{} }}", cells.join(", "));
        }
        if !entry.kept.is_empty() {
            let _ = writeln!(out, "\n[{face}.kept]");
        }
        for (code, r) in &entry.kept {
            let _ = write!(
                out,
                "{} = {{ cell = [{}, {}, {}, {}], offset = [{}, {}], advance = {}, classes = [{}, {}]",
                key(&format!("0x{code:02X}")),
                r.u,
                r.v,
                r.width,
                r.height,
                r.x_offset,
                r.y_offset,
                r.advance_extra,
                r.left,
                r.right
            );
            if r.advance_y != 0 {
                let _ = write!(out, ", advance_y = {}", r.advance_y);
            }
            if r.page != 0 {
                let _ = write!(out, ", page = {}", r.page);
            }
            if r.unread != [0, 0] {
                let _ = write!(out, ", unread = [{}, {}]", r.unread[0], r.unread[1]);
            }
            out.push_str(" }\n");
        }
    }
    out
}

/// An atlas's page and its faces - their specs and their fonts' glyphs - from its settings.
fn atlas_faces(call: &mut Call, given: &Table) -> Result<(PageSpec, Vec<FaceSpec>, Vec<Vec<atlas::Glyph>>)> {
    let page = page_spec(given)?;
    let mut specs = Vec::new();
    let mut fonts = Vec::new();
    for entry in given.get("faces").and_then(Value::as_array).into_iter().flatten() {
        let entry = entry.as_table().ok_or_else(|| anyhow!("a face is not a table"))?;
        let node = entry.get("font").and_then(pe2_ext::target_of).ok_or_else(|| anyhow!("a face names no font"))?;
        // Named as its layout names it: by its node's name, which a reference by uid does not say.
        let path = call.node_named(node).map_or_else(|| node.to_string(), |named| named.path);
        let name = path.rsplit('/').next().unwrap_or(&path).to_string();
        let Data::Typed(source) = call.output_of(node, "native")? else { bail!("{node}: its native form is no files") };
        let files = source.files.iter().map(|file| Ok((file.name.clone(), call.read(file)?))).collect::<Result<_>>()?;
        let font = codecs::ufo::read(&files).map_err(|e| anyhow!("{node}: {e}"))?;
        fonts.push(face::from_ufo(&font, &raster_of(&source.settings)?).map_err(|e| anyhow!("{node}: {e}"))?);
        specs.push(face_spec(call, &name, &source.settings, entry)?);
    }
    Ok((page, specs, fonts))
}

/// An atlas's layout, from the file its stack reads.
fn layout_given(call: &mut Call) -> Result<Layout> {
    let layout = call.take::<ty::Bytes>(0)?;
    layout_of(&String::from_utf8(call.read(layout.file()?)?)?)
}

/// The page and its faces' tables, made from its layout and its faces' UFOs.
fn make_atlas(call: &mut Call) -> Result<Typed> {
    let layout = layout_given(call)?;
    let given = call.take::<ty::Table>(1)?;
    let under = call.take::<ty::Bytes>(2)?;
    let (mut page, mut specs, fonts) = atlas_faces(call, &given)?;
    if let Some(file) = under.files.first() {
        let art = face::art_of(&call.read(file)?).map_err(|e| anyhow!("under.png: {e}"))?;
        if (art.width, art.height) != (page.width, page.rows()) {
            bail!("under.png is {} x {}, the pages {} x {}", art.width, art.height, page.width, page.rows());
        }
        page.under = Some(art.texels);
    }
    // Generated, its text's glyphs alone, where its rule puts them.
    let layout = match flow_of(&given, &page)? {
        Some(flow) => {
            if let Some(text) = &flow.text {
                let used: std::collections::BTreeSet<char> = atlas::first_use(text).into_iter().collect();
                for spec in &mut specs {
                    spec.only = Some(used.clone());
                }
            }
            atlas::flow(&page, &specs, &fonts, &flow, &layout).map_err(|e| anyhow!(e))?
        }
        None => layout,
    };
    let (texels, tables) = atlas::generate(&page, &specs, &fonts, &layout).map_err(|e| anyhow!(e))?;
    let mut written = Vec::new();
    for (spec, table) in specs.iter().zip(&tables) {
        let extension = match spec.format {
            atlas::Format::FontGlyph => "fontglyph",
            atlas::Format::GlyphCell => "glyphcell",
            atlas::Format::StfGlyph => "stfglyph",
        };
        let mut writer = call.writer(&format!("{}.{extension}", spec.name))?;
        std::io::Write::write_all(&mut writer, table)?;
        written.push(writer.finish()?);
    }
    call.make("tables", Data::Typed(Typed::new(pe2_ext::data::BYTES, written)));
    let mut writer = call.writer("native.texels")?;
    std::io::Write::write_all(&mut writer, &texels)?;
    Ok(Typed::new(AtlasPage::NAME, vec![writer.finish()?]))
}

/// The layout with what it does not place packed, as its file; the atlas's settings handed on.
fn pack_layout(call: &mut Call) -> Result<Typed> {
    let layout = layout_given(call)?;
    let given = call.given();
    call.make(SETTINGS, Data::Table(given.clone()));
    let (page, specs, fonts) = atlas_faces(call, &given)?;
    // A generated atlas places all it holds itself: what its layout says stays as it is.
    let packed = match flow_of(&given, &page)? {
        Some(_) => layout,
        None => atlas::pack(&page, &specs, &fonts, &layout).map_err(|e| anyhow!(e))?,
    };
    let mut writer = call.writer(LAYOUT)?;
    std::io::Write::write_all(&mut writer, layout_text(&packed).as_bytes())?;
    Ok(Typed::new(pe2_ext::data::BYTES, vec![writer.finish()?]))
}

/// What an atlas holds: its faces, each with how many cells its layout places and how many records it keeps; "Pack
/// unplaced" writes the layout with what it does not place packed.
fn atlas_contents(asset: &pe2_ext::Asset, project: &dyn pe2_ext::Project) -> Contents {
    let layout =
        asset.read(LAYOUT).ok().and_then(|bytes| layout_of(&String::from_utf8_lossy(&bytes)).ok()).unwrap_or_default();
    let mut rows = vec![ContentRow::heading("faces", "Faces")];
    for entry in asset.array("faces") {
        let Some(font) = entry.get("font").and_then(|reference| project.resolve(reference)) else { continue };
        let name = font.path().rsplit('/').next().unwrap_or(font.path());
        let placed =
            layout.faces.get(name).map_or(0, |face| face.rows.iter().map(|row| row.cells.len()).sum::<usize>());
        let kept = layout.faces.get(name).map_or(0, |face| face.kept.len());
        let generated = asset.field("mode").and_then(Value::as_str) == Some(GENERATED);
        let detail = match (generated, kept) {
            (true, _) => format!("generated, {placed} pinned"),
            (false, 0) => format!("{placed} placed"),
            (false, _) => format!("{placed} placed, {kept} kept"),
        };
        rows.push(
            ContentRow::new(&format!("faces/{name}"), name)
                .depth(1)
                .icon(Icon::Text, Tint::Plain)
                .detail(&detail)
                .link(font.path()),
        );
    }
    let actions = vec![ContentAction::new(PACK, "Pack unplaced").icon(Icon::Grid)];
    Contents { rows, actions, ..Contents::default() }
}

/// The action that writes an atlas's layout with what it does not place packed.
const PACK: &str = "pack";

fn atlas_act(
    asset: &pe2_ext::Asset,
    project: &dyn pe2_ext::Project,
    request: ContentRequest,
) -> Result<ContentOutcome> {
    match request {
        ContentRequest::Action { action, .. } if action == PACK => {
            let packed = project.bytes(asset.path(), "packed").ok_or_else(|| anyhow!("it has no packed layout"))??;
            if asset.read(LAYOUT).is_ok_and(|now| now == packed) {
                bail!("its layout places everything its faces draw");
            }
            let name = asset.path().rsplit('/').next().unwrap_or(asset.path());
            Ok(ContentOutcome {
                label: format!("Pack what {name} does not place"),
                writes: Some((LAYOUT.to_string(), packed)),
                ..ContentOutcome::default()
            })
        }
        _ => bail!("an atlas does nothing else with its contents"),
    }
}

/// What any texture offers after its texels: its picture, palettes, cells and marks - `pe2.images`'s own stack
/// (`OFFERS` there), written here as its operations are named, this extension taking its types by name only.
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
