//! A room's resource bundle (`.pe2bundle`, a file's chunk of opcode 4): its resources by directory index, each made of
//! what it is - a palette resource of the palettes it lists, placed where each is uploaded in the VRAM rectangle it
//! fills; a dialogue by its dialogue node; anything else (stage 0's credits, pictures and demos) its stored bytes,
//! `resources/<index>.bin`, as is a palette resource holding a run no image owns, which gives that run its colors.
//! Where it is laid in RAM (`base`, its first resource's destination, which differs by release)
//! and the zero sectors past its usual end are its `bundle.toml`, which extraction writes, with the palette streams the
//! game's tool wrote a byte short of their rectangle (`palette_bytes`, by index: 38 of the 934).

use std::fmt::Write as _;

use pe2_ext::{Call, Data, Result, Table, Typed, Value, anyhow, bail, ty};

/// What the node declares, and what extraction wrote beside it.
pub const STACK: &str = r#"[[stack]]
id = "bundle"
op = "pe2.game@bundle"

[[stack]]
id = "layout"
op = "file"
file = "bundle.toml"

[[stack]]
id = "stored"
op = "file"
file = "resources"
optional = true

[[stack]]
op = "pe2.game@join_bundle"
bundle = { from = "bundle" }
layout = { from = "layout" }
stored = { from = "stored" }

[[stack]]
op = "output"
name = "native"
retain = "cache"
"#;

/// A resource's directory kind: 2 a palette chunk, 3 data.
const PALETTE: u8 = 2;
const DATA: u8 = 3;

fn list<'a>(table: &'a Table, key: &str) -> &'a [Value] {
    table.get(key).and_then(Value::as_array).map_or(&[][..], Vec::as_slice)
}

fn integer(table: &Table, key: &str) -> Option<i64> {
    table.get(key).and_then(Value::as_integer)
}

fn target(value: Option<&Value>, what: &str) -> Result<String> {
    value.and_then(pe2_ext::target_of).map(str::to_string).ok_or_else(|| anyhow!("{what} names nothing"))
}

fn pair(value: Option<&Value>) -> Option<(i64, i64)> {
    let items = value?.as_array()?;
    Some((items.first()?.as_integer()?, items.get(1)?.as_integer()?))
}

/// A resource as declared.
struct Resource<'a> {
    index: usize,
    table: &'a Table,
}

impl Resource<'_> {
    fn kind(&self) -> &str {
        self.table.get("type").and_then(Value::as_str).unwrap_or("")
    }
}

fn resources(given: &Table) -> Result<Vec<Resource<'_>>> {
    list(given, "resources")
        .iter()
        .map(|r| {
            let table = r.as_table().ok_or_else(|| anyhow!("a resource is no table"))?;
            let index = integer(table, "index").and_then(|i| usize::try_from(i).ok());
            Ok(Resource { index: index.ok_or_else(|| anyhow!("a resource has no index"))?, table })
        })
        .collect()
}

/// A palette entry: the image whose palette it is and the palette's name; none for a run no image owns, whose colors
/// are the stored chunk's (`resources/<index>.bin`).
fn entry(value: &Value) -> Result<Option<(String, String)>> {
    let Some(palette) = value.get("palette") else { return Ok(None) };
    let image = target(Some(palette), "a palette")?;
    let output = pe2_ext::output_of(palette).ok_or_else(|| anyhow!("{image}: a palette names no output"))?;
    let name = output.strip_prefix("palettes/").ok_or_else(|| anyhow!("{image}: {output} is no palette"))?;
    Ok(Some((image, name.to_string())))
}

/// Whether a palette resource holds a run no image owns.
fn has_runs(resource: &Table) -> bool {
    list(resource, "palettes").iter().any(|e| e.get("palette").is_none())
}

/// The VRAM rectangle a slot's settings say, `[x, y, w, h]`.
fn rect(settings: &Table, what: &str) -> Result<[i64; 4]> {
    let items = list(settings, "rect");
    let mut out = [0; 4];
    for (n, slot) in out.iter_mut().enumerate() {
        *slot = items.get(n).and_then(Value::as_integer).ok_or_else(|| anyhow!("{what}: no rect"))?;
    }
    Ok(out)
}

/// What a palette resource is made with: the slot it uploads into, its images' palettes and the pictures of them, the
/// slots those and its runs are placed in, and its stored chunk where it holds runs.
struct PaletteMaking<'a> {
    upload: Option<&'a Table>,
    images: &'a [(String, String, Table)],
    pictures: &'a [Data],
    slots: &'a [(String, Table)],
    stored_chunk: Option<&'a [u8]>,
    stored_bytes: Option<usize>,
}

/// Where a place in a slot is in VRAM.
fn absolute(slots: &[(String, Table)], placed: &Value, what: &str) -> Result<(i64, i64)> {
    let slot = target(placed.get("slot"), what)?;
    let found = slots.iter().find(|(s, _)| *s == slot).ok_or_else(|| anyhow!("{slot}: not read"))?;
    let [px, py, _, _] = rect(&found.1, &slot)?;
    let (at_x, at_y) = pair(placed.get("at")).unwrap_or((0, 0));
    Ok((px + at_x, py + at_y))
}

/// A palette resource: its rectangle's colors - a run no image owns as the stored chunk holds it, each palette where its
/// image places it - and its chunk.
fn palette_resource(resource: &Resource<'_>, making: &PaletteMaking<'_>) -> Result<Vec<u8>> {
    let what = format!("resource {}", resource.index);
    // Its rectangle: where it uploads, or - holding runs alone, naming no slot - the stored chunk's.
    let (x, y, w, h) = if let (Some(slot), Some(up)) = (making.upload, resource.table.get("upload").and_then(Value::as_table)) {
        let [sx, sy, sw, sh] = rect(slot, &what)?;
        let (ax, ay) = pair(up.get("at")).unwrap_or((0, 0));
        let (w, h) = pair(up.get("size")).unwrap_or((sw, sh));
        (sx + ax, sy + ay, w, h)
    } else {
        let chunk = making.stored_chunk.ok_or_else(|| anyhow!("{what}: no upload, and no stored chunk"))?;
        let stored = codecs::texture::palette(chunk).map_err(|e| anyhow!("{what}: {e:?}"))?;
        let rows = i64::try_from(stored.rows.len())?;
        (i64::from(stored.x), i64::from(stored.y), i64::try_from(stored.width)?, rows)
    };
    let width = usize::try_from(w)?;
    let mut colors = vec![0u16; width * usize::try_from(h)?];
    let mut put = |(cx, cy): (i64, i64), words: &mut dyn Iterator<Item = u16>, said: &str| -> Result<()> {
        let start = usize::try_from(cy - y)? * width + usize::try_from(cx - x)?;
        for (n, word) in words.enumerate() {
            *colors.get_mut(start + n).ok_or_else(|| anyhow!("{said} passes {what}'s rectangle"))? = word;
        }
        Ok(())
    };
    for run in list(resource.table, "palettes").iter().filter(|e| e.get("palette").is_none()) {
        let name = run.get("name").and_then(Value::as_str).unwrap_or("a run");
        let chunk = making.stored_chunk.ok_or_else(|| anyhow!("{what}: {name}: no stored chunk holds it"))?;
        let stored = codecs::texture::palette(chunk).map_err(|e| anyhow!("{what}: {e:?}"))?;
        let (rx, ry) = absolute(making.slots, run, name)?;
        let count = usize::try_from(integer(run.as_table().unwrap_or(&Table::new()), "entries").unwrap_or(16))?;
        let row = stored.rows.get(usize::try_from(ry - i64::from(stored.y))?).ok_or_else(|| anyhow!("{name}: not stored"))?;
        let from = usize::try_from(rx - i64::from(stored.x))?;
        let mut words = row.get(from..from + count).ok_or_else(|| anyhow!("{name}: not stored"))?.iter().copied();
        put((rx, ry), &mut words, name)?;
    }
    for ((image, name, settings), picture) in making.images.iter().zip(making.pictures) {
        let placed = settings.get("palettes").and_then(|p| p.get(name)).ok_or_else(|| anyhow!("{image}: no {name}"))?;
        let at = absolute(making.slots, placed, &format!("{image}'s {name}"))?;
        let Data::Image(picture) = picture else { bail!("{image}: its {name} is no picture") };
        let mut words = picture.rgba.as_chunks::<4>().0.iter().map(|c| codecs_color(*c));
        put(at, &mut words, &format!("{image}: {name}"))?;
    }
    let stored = making.stored_bytes.unwrap_or(colors.len() * 2);
    Ok(formats::cdf::palette_payload(u16::try_from(x)?, u16::try_from(y)?, u16::try_from(w)?, &colors, stored))
}

/// A color back from RGBA, as `codecs::texture::color` inverts what a palette picture is made of.
fn codecs_color(rgba: [u8; 4]) -> u16 {
    codecs::texture::color(rgba)
}

/// The slots palette resources upload into, each palette entry's image and palette, and the slots its runs are in.
type PaletteReads = (Vec<String>, Vec<(String, String)>, Vec<String>);

/// What a bundle's palette resources read.
fn palette_reads(given: &Table) -> Result<PaletteReads> {
    let all = resources(given)?;
    let palettes = || all.iter().filter(|r| r.kind() == "palette");
    let uploads = palettes()
        .filter_map(|r| r.table.get("upload").and_then(|u| u.get("slot")))
        .map(|slot| target(Some(slot), "an upload"))
        .collect::<Result<_>>()?;
    let entries: Vec<Option<(String, String)>> =
        palettes().flat_map(|r| list(r.table, "palettes")).map(entry).collect::<Result<_>>()?;
    let runs = palettes()
        .flat_map(|r| list(r.table, "palettes"))
        .filter(|e| e.get("palette").is_none())
        .map(|e| target(e.get("slot"), "a run"))
        .collect::<Result<_>>()?;
    Ok((uploads, entries.into_iter().flatten().collect(), runs))
}

/// The slots the images place their palettes in, and the runs are in, each once.
fn placing_slots(images: &[(String, String, Table)], runs: &[String]) -> Result<Vec<String>> {
    let mut placing: Vec<String> = Vec::new();
    for (image, name, settings) in images {
        let placed = settings.get("palettes").and_then(|p| p.get(name)).ok_or_else(|| anyhow!("{image}: no {name}"))?;
        let slot = target(placed.get("slot"), &format!("{image}'s {name}"))?;
        if !placing.contains(&slot) {
            placing.push(slot);
        }
    }
    for slot in runs {
        if !placing.contains(slot) {
            placing.push(slot.clone());
        }
    }
    Ok(placing)
}

/// The bundle joined: each resource made, laid out as the game's tool laid it out.
#[pe2_ext::steps]
pub fn join(call: &mut Call) -> Result<Typed> {
    let given: Table = call.given();
    // The slots palette resources upload into, and the images whose palettes they hold.
    let reads: PaletteReads = palette_reads(&given)?;
    let uploads: Vec<String> = reads.0;
    let entries: Vec<(String, String)> = reads.1;
    let runs: Vec<String> = reads.2;
    #[request]
    let upload_settings: Vec<Table> = call.tables_of(uploads.iter().map(|s| (s.as_str(), crate::SETTINGS)))?;
    #[request]
    let image_settings: Vec<Table> = call.tables_of(entries.iter().map(|(i, _)| (i.as_str(), crate::SETTINGS)))?;
    let images: Vec<(String, String, Table)> =
        entries.into_iter().zip(image_settings).map(|((i, n), s)| (i, n, s)).collect();
    let placing: Vec<String> = placing_slots(&images, &runs)?;
    #[request]
    let placing_settings: Vec<Table> = call.tables_of(placing.iter().map(|s| (s.as_str(), crate::SETTINGS)))?;
    let slots: Vec<(String, Table)> = placing.into_iter().zip(placing_settings).collect();
    let dialogues: Vec<String> = resources(&given)?
        .iter()
        .filter(|r| r.kind() == "dialogue")
        .map(|r| target(r.table.get("dialogue"), &format!("dialogue {}", r.index)))
        .collect::<Result<_>>()?;
    let addresses: Vec<String> = images.iter().map(|(_, n, _)| format!("palettes/{n}")).collect();
    // The palettes' pictures, then the dialogues' files: one request, as what a step holds of them is not carried.
    let pictures_of = images.iter().zip(&addresses).map(|((i, _, _), a)| (i.as_str(), a.as_str()));
    #[request]
    let made: Vec<Data> = call.outputs_of(pictures_of.chain(dialogues.iter().map(|d| (d.as_str(), "native"))))?;
    let (pictures, natives) = made.split_at(images.len());
    let all = resources(&given)?;
    let layout = call.take::<ty::Bytes>(1)?;
    let layout: Table = String::from_utf8(call.read(layout.file()?)?)?.parse()?;
    let stored = call.take::<ty::Bytes>(2)?;
    let mut bundle = formats::bundle::Bundle {
        base: u32::try_from(integer(&layout, "base").ok_or_else(|| anyhow!("bundle.toml: no base"))?)?,
        extra_sectors: usize::try_from(integer(&layout, "extra_sectors").unwrap_or(0))?,
        ..formats::bundle::Bundle::default()
    };
    let (mut upload, mut image, mut native) = (upload_settings.iter(), 0, natives.iter());
    for resource in &all {
        let made = match resource.kind() {
            "palette" => {
                let count = list(resource.table, "palettes").iter().filter(|e| e.get("palette").is_some()).count();
                let names_slot = resource.table.get("upload").and_then(|u| u.get("slot")).is_some();
                let up = if names_slot { Some(upload.next().ok_or_else(|| anyhow!("an upload was not read"))?) } else { None };
                let stored_bytes = layout
                    .get("palette_bytes")
                    .and_then(|p| p.get(resource.index.to_string()))
                    .and_then(Value::as_integer)
                    .and_then(|n| usize::try_from(n).ok());
                let chunk = if has_runs(resource.table) { Some(stored_file(call, &stored, resource.index)?) } else { None };
                let making = PaletteMaking {
                    upload: up,
                    images: &images[image..image + count],
                    pictures: &pictures[image..image + count],
                    slots: &slots,
                    stored_chunk: chunk.as_deref(),
                    stored_bytes,
                };
                let made = palette_resource(resource, &making)?;
                image += count;
                (PALETTE, made)
            }
            "dialogue" => {
                let Some(Data::Typed(typed)) = native.next() else { bail!("dialogue {}: no file", resource.index) };
                (DATA, call.read(typed.file()?)?)
            }
            _ => {
                let kind = integer(resource.table, "kind").and_then(|k| u8::try_from(k).ok()).unwrap_or(DATA);
                (kind, stored_file(call, &stored, resource.index)?)
            }
        };
        bundle.resources.insert(resource.index, made);
    }
    let mut writer = call.writer("native.bin")?;
    std::io::Write::write_all(&mut writer, &formats::bundle::write(&bundle))?;
    Ok(Typed::new(pe2_ext::data::BYTES, vec![writer.finish()?]))
}

/// A resource's stored bytes, `resources/<index>.bin`.
fn stored_file(call: &Call, stored: &Typed, index: usize) -> Result<Vec<u8>> {
    let name = format!("{index}.bin");
    let file = stored.files.iter().find(|f| f.name.rsplit('/').next() == Some(name.as_str()));
    call.read(file.ok_or_else(|| anyhow!("resource {index}: no resources/{name}"))?)
}

/// What extraction writes of a bundle's chunk: its `bundle.toml` and the bytes of each resource no node makes, by the
/// node's path.
///
/// # Errors
/// The chunk is no bundle laid out as the game's tool lays one out, or it differs from what the node declares.
pub fn extract(chunk: &[u8], args: &Table) -> Result<Vec<(String, Vec<u8>)>> {
    let path = args.get("bundle").and_then(Value::as_str).ok_or_else(|| anyhow!("no bundle"))?;
    let bundle = formats::bundle::parse(chunk).map_err(|e| anyhow!("{path}: {e}"))?;
    let declared = resources(args)?;
    let indices: Vec<usize> = declared.iter().map(|r| r.index).collect();
    let held: Vec<usize> = bundle.resources.keys().copied().collect();
    if indices != held {
        bail!("{path}: resources {held:?} stored, {indices:?} declared");
    }
    let mut layout = Table::new();
    layout.insert("base".into(), Value::Integer(i64::from(bundle.base)));
    if bundle.extra_sectors > 0 {
        layout.insert("extra_sectors".into(), Value::Integer(i64::try_from(bundle.extra_sectors)?));
    }
    let mut short = Table::new();
    for resource in declared.iter().filter(|r| r.kind() == "palette") {
        let (_, bytes) = &bundle.resources[&resource.index];
        let rect = |at: usize| bytes.get(at..at + 2).map_or(0, |w| usize::from(u16::from_le_bytes([w[0], w[1]])));
        let decoded = formats::lzss::decode_stream(bytes, 16).0.len();
        if decoded != rect(4) * rect(6) * 2 {
            short.insert(resource.index.to_string(), Value::Integer(i64::try_from(decoded)?));
        }
    }
    if !short.is_empty() {
        layout.insert("palette_bytes".into(), Value::Table(short));
    }
    let mut out = vec![(format!("{path}/bundle.toml"), toml_text(&layout).into_bytes())];
    let stored = |r: &Resource<'_>| match r.kind() {
        "dialogue" => false,
        "palette" => r.table.get("runs").and_then(Value::as_bool) == Some(true) || has_runs(r.table),
        _ => true,
    };
    for resource in declared.iter().filter(|r| stored(r)) {
        let (_, bytes) = &bundle.resources[&resource.index];
        out.push((format!("{path}/resources/{}.bin", resource.index), bytes.clone()));
    }
    Ok(out)
}

/// A table as TOML, the base in hex as the game's addresses are written.
fn toml_text(table: &Table) -> String {
    let mut out = String::new();
    for (key, value) in table {
        let _ = match value {
            Value::Integer(n) if key == "base" => writeln!(out, "{key} = 0x{n:08X}"),
            other => writeln!(out, "{key} = {other}"),
        };
    }
    out
}
