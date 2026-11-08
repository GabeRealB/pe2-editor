//! pe2.dialogue: the rooms' dialogues - CAP files (`doc/CAP_FORMAT.md`), a room's script of lines and scene steps - as
//! their structure, their text and their faces.
//!
//! - A `dialogue` is a data node: its commands and their records (`dialogue.rs`), each record's text a key of the
//!   string table it names (`strings`), its speakers named by plate (`cast`), its glyphs drawn from its faces (`faces`:
//!   `glyphs`, `bold` and `speakers`, `pe2.fonts` faces whose glyphs are the pictures the atlases show). It makes the
//!   file the bundle stores (`native`) and the atlas page it lays its glyphs out on (`atlas`), as the game's tool did.
//! - A `string_table` is a data node of text by key (`strings`), in the dialogues' markup (`markup.rs`); any number of
//!   them, where convenient.
//! - A `cast` is the speakers a dialogue names, each plate with the speaker numbers it stands for.
//!
//! Extraction reads every dialogue at once (`extract_dialogues`): its structure, its stage's string table and the faces'
//! pictures, each written only where they make the stored file and its atlas again exactly.

mod dialogue;
mod layout;
mod markup;
mod named;
mod recognize;
mod strings;
mod view;

use std::collections::BTreeMap;

use pe2_ext::{Call, Data, Field, Icon, Registry, Result, Table, Tint, Typed, Value, anyhow, bail, ty};
use typeface::{Art, Glyph, TRANSPARENT};

use layout::{Font, Picture};

pe2_ext::extension!(register);

/// A dialogue's stack: its structure, the file and atlas made of it with its strings and faces.
const DIALOGUE: &str = r#"[[stack]]
id = "dialogue"
op = "pe2.dialogue@dialogue"

[[stack]]
id = "make"
op = "pe2.dialogue@make"
dialogue = { from = "dialogue" }

[[stack]]
op = "output"
name = "native"
retain = "cache"

[[stack]]
op = "output"
name = "atlas"
value = { from = "make.atlas" }
"#;

const SETTINGS: &str = "settings";
const FACES: [&str; 3] = ["glyphs", "bold", "speakers"];

fn reference(table: &Table, key: &str) -> Result<String> {
    table.get(key).and_then(pe2_ext::target_of).map(str::to_string).ok_or_else(|| anyhow!("it names no {key}"))
}

/// Each plate's speaker numbers, from a cast's settings.
fn cast_of(settings: &Table) -> BTreeMap<String, Vec<u8>> {
    let speakers = settings.get("speakers").and_then(Value::as_table).into_iter().flatten();
    speakers
        .map(|(name, numbers)| {
            let numbers = numbers.as_array().into_iter().flatten();
            (name.clone(), numbers.filter_map(Value::as_integer).filter_map(|n| u8::try_from(n).ok()).collect())
        })
        .collect()
}

/// A face's glyphs as pictures by token: a glyph drawing one character by it, any other by its name.
fn pictures_of(glyphs: &[Glyph]) -> BTreeMap<String, Picture> {
    glyphs
        .iter()
        .filter_map(|glyph| {
            let art = glyph.art.as_ref()?;
            let token = match glyph.chars.as_slice() {
                [c] => c.to_string(),
                _ => glyph.name.clone(),
            };
            let values = art.texels.iter().map(|t| if *t == TRANSPARENT { 0 } else { *t }).collect();
            Some((token, Picture { width: art.width, height: art.height, values }))
        })
        .collect()
}

/// A face's pictures as glyphs: a character's named as fonts name it, a plate's or placeholder's by its token.
fn glyphs_of(face: &str, pictures: &BTreeMap<String, Picture>) -> Vec<Glyph> {
    let mut glyphs: Vec<Glyph> = pictures
        .iter()
        .map(|(token, picture)| {
            let mut chars = token.chars();
            let one = match (chars.next(), chars.next()) {
                (Some(c), None) if face != "speakers" => Some(c),
                _ => None,
            };
            let texels = picture.values.iter().map(|v| if *v == 0 { TRANSPARENT } else { *v }).collect();
            let byte = |v: usize| u8::try_from(v).unwrap_or(u8::MAX);
            Glyph {
                name: one.map_or_else(|| token.clone(), typeface::glyph_name),
                chars: one.into_iter().collect(),
                width: byte(picture.width),
                height: byte(picture.height),
                x_offset: 0,
                y_offset: 0,
                advance_extra: 1,
                left: 0,
                right: 0,
                art: Some(Art { width: picture.width, height: picture.height, texels }),
                cell_at: (0, 0),
            }
        })
        .collect();
    glyphs.sort_by(|a, b| (a.chars.is_empty(), &a.name).cmp(&(b.chars.is_empty(), &b.name)));
    glyphs
}

/// A face's UFO files, read into its glyphs.
fn face_glyphs(call: &Call, face: &Data, what: &str) -> Result<Vec<Glyph>> {
    let Data::Typed(typed) = face else { bail!("{what}: its native form is no files") };
    let files = typed.files.iter().map(|file| Ok((file.name.clone(), call.read(file)?))).collect::<Result<_>>()?;
    let font = codecs::ufo::read(&files).map_err(|e| anyhow!("{what}: {e}"))?;
    typeface::face::from_ufo(&font, &typeface::raster::Raster::default()).map_err(|e| anyhow!("{what}: {e}"))
}

/// A dialogue's file and atlas, made of its structure, its string table, its cast and its faces.
#[pe2_ext::steps]
fn make(call: &mut Call) -> Result<Typed> {
    let given: Table = call.given();
    if let Some(why) = given.get("failed").and_then(Value::as_str) {
        bail!("extraction could not read it exactly: {why}");
    }
    let reads: Vec<String> = [reference(&given, "strings")?, reference(&given, "cast")?].into();
    let faces = given.get("faces").and_then(Value::as_table).cloned().unwrap_or_default();
    let face_nodes: Vec<String> = FACES.iter().map(|f| reference(&faces, f)).collect::<Result<_>>()?;
    #[request]
    let tables: Vec<Table> = call.tables_of(reads.iter().map(|r| (r.as_str(), SETTINGS)))?;
    let registries: Vec<String> = dialogue::registries(&given);
    #[request]
    let ids: Vec<Table> = call.tables_of(registries.iter().map(|r| (r.as_str(), "ids")))?;
    // The faces last: what a step holds of them is not carried.
    #[request]
    let natives: Vec<Data> = call.outputs_of(face_nodes.iter().map(|f| (f.as_str(), "native")))?;
    let mut font = Font::default();
    for ((face, native), node) in FACES.iter().zip(&natives).zip(&face_nodes) {
        font.faces.insert((*face).to_string(), pictures_of(&face_glyphs(call, native, node)?));
    }
    let strings = tables[0].get("strings").and_then(Value::as_table).cloned().unwrap_or_default();
    let flags: BTreeMap<String, BTreeMap<String, i64>> = registries
        .iter()
        .zip(&ids)
        .map(|(registry, ids)| (registry.clone(), ids.iter().filter_map(|(k, v)| Some((k.clone(), v.as_integer()?))).collect()))
        .collect();
    let (file, laid) = dialogue::make(&given, &strings, &cast_of(&tables[1]), &font, &flags).map_err(|e| anyhow!(e))?;
    let mut atlas = call.writer("atlas.texels")?;
    std::io::Write::write_all(&mut atlas, &laid.atlas)?;
    call.make("atlas", Data::Typed(Typed::new(pe2_ext::data::BYTES, vec![atlas.finish()?])));
    let mut writer = call.writer("native.cap")?;
    std::io::Write::write_all(&mut writer, &file)?;
    Ok(Typed::new(pe2_ext::data::BYTES, vec![writer.finish()?]))
}

/// The texels of a column of an atlas image's chunk: its page at that place among the chunk's, 256 texels a row, to
/// 256 rows.
fn atlas_page(chunk: &[u8], column: usize) -> Result<Vec<u8>> {
    let mut pages = codecs::texture::pages(chunk).map_err(|e| anyhow!("an atlas: {e:?}"))?;
    pages.sort_by_key(|p| (p.x, p.y));
    let page = pages.get(column).ok_or_else(|| anyhow!("an atlas has no column {column}"))?;
    let mut texels = vec![0u8; layout::ATLAS_SIZE * layout::ATLAS_SIZE];
    for (n, word) in page.words.iter().enumerate().take(texels.len() / 4) {
        for nibble in 0..4 {
            texels[n * 4 + nibble] = u8::try_from(word >> (4 * nibble) & 0xF).unwrap_or(0);
        }
    }
    Ok(texels)
}

/// A table of `[[key, value...]]` rows, as the plan passes the glyph and plate tables.
fn rows(args: &Table, key: &str) -> Vec<Vec<String>> {
    let rows = args.get(key).and_then(Value::as_array).into_iter().flatten();
    rows.map(|row| row.as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect()).collect()
}

fn toml_text(table: &Table) -> Result<Vec<u8>> {
    Ok(toml::to_string(table)?.into_bytes())
}

/// What the plan gives recognition: the glyph and plate tables, the cast, and the flags the registries name.
struct Recognition {
    glyphs: BTreeMap<String, Vec<String>>,
    plates: BTreeMap<String, String>,
    cast: BTreeMap<String, Vec<u8>>,
    flags: BTreeMap<u16, (String, String)>,
}

fn recognition(args: &Table) -> Recognition {
    let glyphs: BTreeMap<String, Vec<String>> = rows(args, "glyph_table")
        .into_iter()
        .filter_map(|mut row| {
            if row.is_empty() {
                return None;
            }
            let hash = row.remove(0);
            Some((hash, row.into_iter().map(|c| if c == "space" { " ".to_string() } else { c }).collect()))
        })
        .collect();
    let plates: BTreeMap<String, String> =
        rows(args, "plate_table").into_iter().filter(|r| r.len() >= 2).map(|r| (r[0].clone(), r[1].clone())).collect();
    let cast = cast_of(args);
    // The flags registries name, by ID: `flags`, each registry's uid with its flags' IDs by name.
    let mut flags: BTreeMap<u16, (String, String)> = BTreeMap::new();
    for registry in args.get("flags").and_then(Value::as_array).into_iter().flatten() {
        let uid = registry.get("registry").and_then(Value::as_str).unwrap_or_default().to_string();
        for (name, id) in registry.get("ids").and_then(Value::as_table).into_iter().flatten() {
            if let Some(id) = id.as_integer().and_then(|i| u16::try_from(i).ok()) {
                flags.insert(id, (uid.clone(), name.clone()));
            }
        }
    }
    Recognition { glyphs, plates, cast, flags }
}

/// What extraction writes of every dialogue, by node path: each dialogue's `settings.toml` (its structure), each string
/// table's, and each face's UFO. `chunks` are what the plan read, which `args` names by position: each dialogue's
/// bundle and resource, and its atlas image and column where it generates one.
///
/// # Errors
/// A chunk is not what it is said to be, or a dialogue is not read exactly.
pub fn extract_dialogues(chunks: &[Vec<u8>], args: &Table) -> Result<Vec<(String, Vec<u8>)>> {
    let Recognition { glyphs, plates, cast, flags } = recognition(args);
    let mut font = Font::default();
    let mut tables: BTreeMap<String, Table> = BTreeMap::new();
    let mut out = Vec::new();
    let mut bundles: BTreeMap<usize, BTreeMap<usize, (u8, Vec<u8>)>> = BTreeMap::new();
    for entry in args.get("dialogues").and_then(Value::as_array).into_iter().flatten() {
        let entry = entry.as_table().ok_or_else(|| anyhow!("a dialogue is no table"))?;
        let text = |key: &str| entry.get(key).and_then(Value::as_str).map(str::to_string).ok_or_else(|| anyhow!("a dialogue has no {key}"));
        let index = |key: &str| {
            entry.get(key).and_then(Value::as_integer).and_then(|n| usize::try_from(n).ok()).ok_or_else(|| anyhow!("no {key}"))
        };
        let path = text("path")?;
        let bundle = index("bundle")?;
        let held = match bundles.entry(bundle) {
            std::collections::btree_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::btree_map::Entry::Vacant(e) => {
                let chunk = chunks.get(bundle).ok_or_else(|| anyhow!("{path}: no chunk {bundle}"))?;
                let parsed = formats::bundle::resources(chunk).into_iter().map(|(n, (k, b))| (n, (k, b.to_vec()))).collect();
                e.insert(parsed)
            }
        };
        let resource = index("resource")?;
        let (_, bytes) = held.get(&resource).ok_or_else(|| anyhow!("{path}: its bundle has no resource {resource}"))?;
        let bytes = bytes.clone();
        let page = match entry.get("atlas").and_then(Value::as_table) {
            Some(atlas) => {
                let at = |key: &str| atlas.get(key).and_then(Value::as_integer).and_then(|n| usize::try_from(n).ok());
                let chunk = chunks.get(at("chunk").ok_or_else(|| anyhow!("{path}: an atlas without its chunk"))?);
                Some(atlas_page(chunk.ok_or_else(|| anyhow!("{path}: no atlas chunk"))?, at("column").unwrap_or(0))?)
            }
            None => None,
        };
        let chosen: BTreeMap<u16, String> = entry
            .get("glyphs")
            .and_then(Value::as_table)
            .into_iter()
            .flatten()
            .filter_map(|(cell, c)| Some((cell.parse().ok()?, c.as_str()?.to_string())))
            .collect();
        let name = text("name")?;
        let prefix = text("prefix")?;
        let reading = dialogue::Reading {
            name: &name,
            prefix: &prefix,
            atlas: page.as_deref(),
            glyphs: &glyphs,
            plates: &plates,
            chosen: &chosen,
            cast: &cast,
            flags: &flags,
        };
        // One not read exactly says why in its place, and adds nothing to the faces.
        let decoded = match dialogue::decode(&bytes, &reading, &mut font) {
            Ok(decoded) => decoded,
            Err(why) => {
                let mut failed = Table::new();
                failed.insert("failed".into(), Value::String(why));
                out.push((format!("{path}/settings.toml"), toml_text(&failed)?));
                continue;
            }
        };
        let mut settings = decoded.settings;
        if !decoded.notes.is_empty() {
            settings.insert("unsettled".into(), Value::Array(decoded.notes.into_iter().map(Value::String).collect()));
        }
        out.push((format!("{path}/settings.toml"), toml_text(&settings)?));
        let strings = tables.entry(text("strings")?).or_default();
        for (key, value) in decoded.strings {
            strings.insert(key, Value::String(value));
        }
    }
    for (path, strings) in tables {
        let mut table = Table::new();
        table.insert("strings".into(), Value::Table(strings));
        out.push((format!("{path}/settings.toml"), toml_text(&table)?));
    }
    let faces = args.get("faces").and_then(Value::as_table).cloned().unwrap_or_default();
    for face in FACES {
        let path = faces.get(face).and_then(Value::as_str).ok_or_else(|| anyhow!("no {face} face"))?;
        let pictures = font.faces.get(face).cloned().unwrap_or_default();
        let family = if face == "speakers" { "Parasite Eve II Speakers" } else { "Parasite Eve II Dialogue" };
        let style = if face == "bold" { "Bold" } else { "Regular" };
        let files = typeface::face::ufo_files(&glyphs_of(face, &pictures), family, style, 0);
        // In a folder named after the face's node, as its stack reads it.
        let node = path.rsplit('/').next().unwrap_or(path);
        out.extend(files.into_iter().map(|(name, bytes)| (format!("{path}/{node}.ufo/{name}"), bytes)));
    }
    Ok(out)
}

#[expect(clippy::unnecessary_wraps, reason = "an entry point may fail; this one does not")]
fn register(r: &mut Registry) -> Result<()> {
    let _ = r
        .op("make")
        .label("Its file and atlas")
        .takes("dialogue", &["pe2.dialogue@dialogue"])
        .output::<ty::Bytes>()
        .also::<ty::Bytes>("atlas")
        .steps(make());
    r.named_type("dialogue")
        .alias("dialogue")
        .label("Dialogue")
        .icon(Icon::Text, Tint::Plain)
        .data()
        .extracted()
        .stack(DIALOGUE)
        .view("Script", |_| view::Script::of_dialogue())
        .field(Field::new("strings").label("Strings"))
        .field(Field::new("cast").label("Cast"))
        .field(Field::new("faces").label("Faces"))
        .field(Field::new("command").label("Commands"))
        .field(Field::new("atlas").label("Atlas"))
        .field(Field::new("trailing_empty_text").label("Trailing empty text"))
        .field(Field::new("unsettled").label("Unsettled"))
        .field(Field::new("failed").label("Failed"))
        .register();
    r.named_type("string_table")
        .alias("string_table")
        .label("String table")
        .icon(Icon::Text, Tint::Plain)
        .data()
        .view("Strings", |_| strings::Strings::of_table())
        .field(Field::new("strings").label("Strings"))
        .register();
    r.named_type("cast")
        .alias("cast")
        .label("Cast")
        .icon(Icon::Text, Tint::Plain)
        .data()
        .field(Field::new("speakers").label("Speakers"))
        .register();
    let _ = r
        .op("extract_dialogues")
        .label("Every dialogue, its strings and faces, read from the bundles and atlases")
        .port("chunks", "bytes", true)
        .output_described("bytes")
        .run_described(|call| {
            let given = call.inputs.first().map_or(&[][..], Vec::as_slice);
            let chunks = given
                .iter()
                .map(|data| match data {
                    Data::Typed(typed) => call.read(typed.file()?),
                    _ => bail!("a chunk is no file"),
                })
                .collect::<Result<Vec<_>>>()?;
            let files = extract_dialogues(&chunks, call.settings)?
                .into_iter()
                .map(|(name, bytes)| pe2_ext::File::new(&name, bytes))
                .collect();
            Ok(Data::Typed(Typed::new("bytes", files)))
        });
    Ok(())
}
