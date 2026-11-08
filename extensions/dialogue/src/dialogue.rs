//! A dialogue as its structure, its text and its faces, and back.
//!
//! Its structure is a table: `command`, its commands in number order (`number`, the opcode and its fields, `record`
//! its records: a line or a scene step, `text` the key of its text in the dialogue's string table), `atlas` what the
//! layout rule does not imply, and `trailing_empty_text`. Its text is markup (`markup.rs`) in a string table, by key.
//! `make` lays the file and its atlas out from them; `decode` reads a stored file and its atlas into them, and adds
//! the pictures it finds to the faces.

use std::collections::{BTreeMap, BTreeSet};

use formats::cap::{self, Cap, Command, Record};
use pe2_ext::{Table, Value};

use crate::layout::{self, FIRST_GLYPH_CELL, Font, Laid, Picture, Titles, crop};
use crate::markup::{self, CELL_MASK, draws};
use crate::named::{self, Speakers};
use crate::recognize::{self, Note};

fn records_of(command: &Table) -> Vec<&Table> {
    command.get("record").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_table).collect()
}

fn highest_key(records: &[&Table]) -> u8 {
    records.iter().filter_map(|r| r.get("key").and_then(Value::as_integer)).max().and_then(|k| u8::try_from(k).ok()).unwrap_or(0)
}

/// A dialogue's atlas table; empty where it has none.
fn atlas_table(settings: &Table) -> Table {
    settings.get("atlas").and_then(Value::as_table).cloned().unwrap_or_default()
}

/// The registries a dialogue's commands name their flags by.
#[must_use]
pub fn registries(settings: &Table) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for command in settings.get("command").and_then(Value::as_array).into_iter().flatten() {
        if let Some(registry) = command.get("game_flag").and_then(|f| f.get("flags")).and_then(pe2_ext::target_of)
            && !out.iter().any(|r| r == registry)
        {
            out.push(registry.to_string());
        }
    }
    out
}

/// Make a dialogue: its file, and its cells and atlas laid out. `strings` its string table's entries, `cast` each
/// plate's speaker numbers, `flags` each registry's flags' IDs by name.
///
/// # Errors
/// A field is out of range, a text key is not in the table, a flag is not in the registry named, a speaker or a glyph
/// is not known.
pub fn make(
    settings: &Table,
    strings: &Table,
    cast: &BTreeMap<String, Vec<u8>>,
    font: &Font,
    flags: &BTreeMap<String, BTreeMap<String, i64>>,
) -> Result<(Vec<u8>, Laid), String> {
    let mut settings = settings.clone();
    if let Some(Value::Array(commands)) = settings.get_mut("command") {
        for command in commands.iter_mut().filter_map(Value::as_table_mut) {
            let Some(Value::Table(named)) = command.get("game_flag") else { continue };
            let registry = named.get("flags").and_then(pe2_ext::target_of).ok_or("a game flag names no registry")?;
            let flag = named.get("flag").and_then(Value::as_str).ok_or("a game flag names no flag")?;
            let id = flags.get(registry).and_then(|f| f.get(flag)).ok_or_else(|| format!("{registry} has no flag {flag}"))?;
            command.insert("game_flag".into(), Value::Integer(*id));
        }
    }
    let settings = &settings;
    let atlas = atlas_table(settings);
    let speakers = Speakers { numbers: cast.clone(), chosen: BTreeMap::new(), titles_are_cells: atlas.contains_key("first_glyph") };
    let listed: Vec<&Table> =
        settings.get("command").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_table).collect();
    let mut commands: Vec<Option<Command>> = Vec::new();
    let (mut texts, mut titles, mut shown): (Vec<String>, Titles, BTreeMap<u8, String>) = (Vec::new(), Vec::new(), BTreeMap::new());
    for command in listed {
        let number = command.get("number").and_then(Value::as_integer).and_then(|n| usize::try_from(n).ok());
        let number = number.ok_or("a command without a number")?;
        let records = records_of(command);
        let highest = highest_key(&records);
        let mut made = Command { bytes: named::command_bytes(command, highest).map_err(|e| format!("command {number}: {e}"))?, records: Vec::new() };
        for (n, record) in records.iter().enumerate() {
            let bytes = named::record_bytes(record, &speakers).map_err(|e| format!("command {number}, record {n}: {e}"))?;
            let key = named::record_text(record);
            let text = if key.is_empty() {
                String::new()
            } else {
                let found = strings.get(&key).and_then(Value::as_str);
                found.ok_or_else(|| format!("command {number}, record {n}: no text {key:?} in the string table"))?.to_string()
            };
            if let Some(speaker) = record.get("speaker")
                && let Some(token) = Speakers::token(speaker)
            {
                shown.insert(speakers.number(speaker)?, token);
            }
            titles.push(named::is_title(bytes).then(|| bytes[0] - 1));
            texts.push(text);
            made.records.push(Record { bytes, text: Vec::new() });
        }
        if commands.len() <= number {
            commands.resize(number + 1, None);
        }
        commands[number] = Some(made);
    }
    let trailing_empty_text = settings.get("trailing_empty_text").and_then(Value::as_bool) == Some(true);
    if trailing_empty_text {
        texts.push(String::new());
        titles.push(None);
    }
    let laid = layout::lay_out(&texts, &titles, &shown, &|n| speakers.plate(n), &atlas, font)?;
    let mut codes = laid.texts.iter();
    for record in commands.iter_mut().flatten().flat_map(|c| c.records.iter_mut()) {
        record.text = codes.next().cloned().unwrap_or_default();
    }
    let file = cap::write(&Cap { cells: laid.cells.clone(), commands, trailing_empty_text });
    Ok((file, laid))
}

/// What reading a stored dialogue needs besides its bytes.
pub struct Reading<'a> {
    /// Its name, for placeholders and problems.
    pub name: &'a str,
    /// What its text keys start with in its string table.
    pub prefix: &'a str,
    /// Its atlas page (`ATLAS_SIZE` texels a row), where it generates one.
    pub atlas: Option<&'a [u8]>,
    /// Characters by glyph picture hash, and speaker plates by theirs.
    pub glyphs: &'a BTreeMap<String, Vec<String>>,
    pub plates: &'a BTreeMap<String, String>,
    /// Its choices for cells whose picture several characters share.
    pub chosen: &'a BTreeMap<u16, String>,
    pub cast: &'a BTreeMap<String, Vec<u8>>,
    /// The game flags registries name, by ID: the registry's uid and the flag's name.
    pub flags: &'a BTreeMap<u16, (String, String)>,
}

/// A dialogue read: its structure, its strings, and what recognition could not settle.
pub struct Decoded {
    pub settings: Table,
    pub strings: Vec<(String, String)>,
    pub notes: Vec<String>,
}

fn int(n: usize) -> Value {
    Value::Integer(i64::try_from(n).unwrap_or(i64::MAX))
}

fn place(cell: [u8; 4]) -> Value {
    Value::Array(vec![int(usize::from(cell[0])), int(usize::from(cell[1]))])
}

fn rect(cell: [u8; 4]) -> Value {
    Value::Array(cell.iter().map(|v| int(usize::from(*v))).collect())
}

/// Add a picture to a face; one already there must be the same.
fn add(font: &mut Font, face: &str, token: &str, picture: Picture) -> Result<(), String> {
    let pictures = font.faces.entry(face.to_string()).or_default();
    match pictures.get(token) {
        Some(known) if *known != picture => Err(format!("the {face} face's {token:?} has a different picture here")),
        Some(_) => Ok(()),
        None => {
            pictures.insert(token.to_string(), picture);
            Ok(())
        }
    }
}

/// Read a stored dialogue into its structure and strings, adding the pictures it shows to `font`; nothing is added and
/// an error is given unless what is read makes the dialogue, and its atlas where it generates one, again exactly.
///
/// # Errors
/// The file is no CAP file laid out as the game's tool lays one out, a choice does not fit, a picture differs from the
/// face's, or what is read does not make the dialogue again.
pub fn decode(bytes: &[u8], reading: &Reading<'_>, font: &mut Font) -> Result<Decoded, String> {
    let stored = cap::parse(bytes)?;
    let mut work = font.clone();
    let records: Vec<&Record> = stored.commands.iter().flatten().flat_map(|c| &c.records).collect();
    let mut streams: Vec<Vec<u16>> = records.iter().map(|r| r.text.clone()).collect();
    if stored.trailing_empty_text {
        streams.push(Vec::new());
    }
    let draws_any = streams.iter().flatten().any(|c| draws(*c)) || records.iter().any(|r| named::is_title(r.bytes));
    let page: Vec<u8> = reading.atlas.map_or_else(|| vec![0; layout::ATLAS_SIZE * layout::ATLAS_SIZE], <[u8]>::to_vec);
    // A dialogue drawing on a page it does not generate keeps the cells it slices of it as stored: no character can
    // be read from them, so its text names them by number.
    let stored_cells = draws_any && reading.atlas.is_none();
    let width = layout::ATLAS_SIZE;
    let mut notes: Vec<String> = Vec::new();
    let recognized = if stored_cells || reading.atlas.is_none() {
        recognize::Recognized { chars: BTreeMap::new(), notes: Vec::new() }
    } else {
        recognize::recognize(&streams, &stored.cells, &page, width, reading.glyphs, reading.chosen)?
    };
    notes.extend(recognized.notes.iter().map(note_text));
    // Which tool laid it out: the house tool numbers glyph cells from 25, after the speaker plates; the other one from
    // 0, in a bolder face, with no plates - its titles are the glyph cells they show.
    let used: BTreeSet<u16> = streams.iter().flatten().filter(|c| draws(**c)).map(|c| c & CELL_MASK).collect();
    let mut layout_table = Table::new();
    if let Some(&first) = used.first()
        && first < FIRST_GLYPH_CELL
    {
        layout_table.insert("face".into(), Value::String("bold".into()));
        layout_table.insert("first_glyph".into(), int(usize::from(first)));
    }
    let face = if layout_table.is_empty() { "glyphs" } else { "bold" };
    let first_glyph = if layout_table.is_empty() { FIRST_GLYPH_CELL } else { used.first().copied().unwrap_or(0) };
    let mut speakers = Speakers {
        numbers: reading.cast.clone(),
        chosen: BTreeMap::new(),
        titles_are_cells: !layout_table.is_empty(),
    };
    let shown: BTreeSet<u8> = records.iter().filter(|r| named::is_title(r.bytes)).map(|r| r.bytes[0] - 1).collect();
    let mut cells = Cells::default();
    if !stored_cells {
        let read = CellReading { page: &page, face, first_glyph, shown: &shown, recognized: &recognized, reading };
        cells = read_cells(&stored.cells, &read, &mut work, &mut notes)?;
    }
    speakers.chosen = std::mem::take(&mut cells.chosen);
    let Cells { token_of, cast, drawn, .. } = cells;
    let texts: Vec<String> = streams.iter().map(|codes| markup::print(codes, &|cell| token_of.get(&cell).cloned())).collect();
    let mut atlas = layout_table.clone();
    if !cast.is_empty() {
        atlas.insert("cast".into(), Value::Table(cast.clone()));
    }
    if !drawn.is_empty() {
        atlas.insert("drawn".into(), Value::Table(drawn));
    }
    if stored_cells {
        atlas.insert("stored_cells".into(), Value::Array(stored.cells.iter().map(|c| rect(*c)).collect()));
    }
    // What the atlas shows outside every cell: a mark of the other tool's face, kept by its position.
    if !layout_table.is_empty()
        && reading.atlas.is_some()
        && let Some(bounds) = mark_of(&page, &stored.cells)
    {
        let picture = crop(&page, width, bounds);
        let token = reading.glyphs.get(&recognize::hash(&picture)).and_then(|c| c.first().cloned());
        let token = token.unwrap_or_else(|| format!("unseen {} mark", reading.name));
        add(&mut work, face, &token, picture)?;
        let mut marks = Table::new();
        marks.insert(token, place(bounds));
        atlas.insert("marks".into(), Value::Table(marks));
    }
    // Where the atlas holds a cell somewhere else than the rule puts it, say where.
    let titles: Titles = records
        .iter()
        .map(|r| (named::is_title(r.bytes) && !speakers.titles_are_cells).then(|| r.bytes[0] - 1))
        .chain(stored.trailing_empty_text.then_some(None))
        .collect();
    if !stored_cells {
        let ruled = layout::lay_out(&texts, &titles, &speakers.chosen, &|n| speakers.plate(n), &layout_table, &work)?;
        let mut placed = Table::new();
        for (n, cell) in stored.cells.iter().enumerate() {
            if n < ruled.cells.len() && ruled.cells[n] != *cell && !cast.contains_key(&n.to_string()) && cell.iter().any(|v| *v != 0) {
                placed.insert(n.to_string(), place(*cell));
            }
        }
        if !placed.is_empty() {
            atlas.insert("placed".into(), Value::Table(placed));
        }
        if stored.cells.len() > ruled.cells.len() {
            atlas.insert("cells".into(), int(stored.cells.len()));
        }
    }
    // The structure, its texts by key.
    let mut settings = Table::new();
    let (commands, strings) = structure(&stored, &texts, reading, &speakers);
    if stored.trailing_empty_text {
        settings.insert("trailing_empty_text".into(), Value::Boolean(true));
    }
    if !atlas.is_empty() {
        settings.insert("atlas".into(), Value::Table(atlas));
    }
    if !commands.is_empty() {
        settings.insert("command".into(), Value::Array(commands));
    }
    // Nothing is kept unless it makes the dialogue, and the atlas it generates, again.
    let table: Table = strings.iter().map(|(k, v)| (k.clone(), Value::String(v.clone()))).collect();
    remakes(bytes, reading, &settings, &table, &work)?;
    *font = work;
    Ok(Decoded { settings, strings, notes })
}

/// Whether what is read makes the dialogue, and the atlas it generates, again.
fn remakes(bytes: &[u8], reading: &Reading<'_>, settings: &Table, strings: &Table, font: &Font) -> Result<(), String> {
    let mut named_flags: BTreeMap<String, BTreeMap<String, i64>> = BTreeMap::new();
    for (id, (registry, flag)) in reading.flags {
        named_flags.entry(registry.clone()).or_default().insert(flag.clone(), i64::from(*id));
    }
    let (rebuilt, laid) = make(settings, strings, reading.cast, font, &named_flags)?;
    if rebuilt != bytes {
        let at = rebuilt.iter().zip(bytes).position(|(a, b)| a != b).unwrap_or(rebuilt.len().min(bytes.len()));
        return Err(format!("what is read does not make the dialogue again: they differ from byte {at:#x}"));
    }
    if let Some(stored_page) = reading.atlas
        && laid.atlas.iter().zip(stored_page).any(|(a, b)| a != b)
    {
        return Err("what is read does not make its atlas again".into());
    }
    Ok(())
}

/// A note of what recognition could not settle, as said.
fn note_text(note: &Note) -> String {
    match note {
        Note::Unseen { cell, hash } => format!("cell {cell}: a glyph no table names ({hash})"),
        Note::Ambiguous { cell, candidates, guessed } => {
            format!("cell {cell}: {} share its picture; guessed {guessed:?}", candidates.join(" or "))
        }
        Note::Duplicate { cell, char, also } => format!("cell {cell}: {char:?} again (cell {also})"),
    }
}

/// What a dialogue's cells are read with.
struct CellReading<'a> {
    page: &'a [u8],
    face: &'a str,
    first_glyph: u16,
    shown: &'a BTreeSet<u8>,
    recognized: &'a recognize::Recognized,
    reading: &'a Reading<'a>,
}

/// A dialogue's cells read: each glyph cell's token, the cells of speakers no record shows (and their plates where
/// drawn), and the plate each shown speaker's cell is.
#[derive(Default)]
struct Cells {
    token_of: BTreeMap<u16, String>,
    cast: Table,
    drawn: Table,
    chosen: BTreeMap<u8, String>,
}

/// Read each cell off the page into the faces: a plate by its picture's name, a glyph by its character.
fn read_cells(cells: &[[u8; 4]], read: &CellReading<'_>, work: &mut Font, notes: &mut Vec<String>) -> Result<Cells, String> {
    let mut out = Cells::default();
    for (number, cell) in cells.iter().enumerate() {
        let number = u16::try_from(number).map_err(|_| "too many cells")?;
        if cell[2] == 0 || cell[3] == 0 {
            continue;
        }
        let picture = crop(read.page, layout::ATLAS_SIZE, *cell);
        if number >= read.first_glyph {
            let token = read.recognized.chars.get(&number).cloned();
            let token = token.unwrap_or_else(|| format!("unseen {} {number}", read.reading.name));
            add(work, read.face, &token, picture)?;
            out.token_of.insert(number, token);
            continue;
        }
        let speaker = read.reading.plates.get(&recognize::hash(&picture)).cloned();
        let small = u8::try_from(number).map_err(|_| "a plate past 255")?;
        if !read.shown.contains(&small) {
            out.cast.insert(number.to_string(), rect(*cell));
            if let Some(speaker) = speaker {
                add(work, "speakers", &speaker, picture)?;
                out.drawn.insert(number.to_string(), Value::String(speaker));
            }
            continue;
        }
        let speaker = speaker.unwrap_or_else(|| {
            notes.push(format!("cell {number}: a speaker plate no table names ({})", recognize::hash(&picture)));
            format!("unseen speaker {number}")
        });
        add(work, "speakers", &speaker, picture)?;
        out.chosen.insert(small, speaker);
    }
    Ok(out)
}

/// What a page shows outside every cell, as a rectangle: the other tool's mark.
fn mark_of(page: &[u8], cells: &[[u8; 4]]) -> Option<[u8; 4]> {
    let width = layout::ATLAS_SIZE;
    let mut bare = page.to_vec();
    for cell in cells.iter().filter(|c| c[2] != 0 && c[3] != 0) {
        for y in usize::from(cell[1])..usize::from(cell[1]) + usize::from(cell[3]) {
            for x in usize::from(cell[0])..usize::from(cell[0]) + usize::from(cell[2]) {
                if let Some(t) = bare.get_mut(y * width + x) {
                    *t = 0;
                }
            }
        }
    }
    bounding_box(&bare, width)
}

/// A dialogue's commands as named fields, and its texts by the keys its records name them by.
fn structure(stored: &Cap, texts: &[String], reading: &Reading<'_>, speakers: &Speakers) -> (Vec<Value>, Vec<(String, String)>) {
    let (mut strings, mut commands) = (Vec::new(), Vec::new());
    let mut text = texts.iter();
    for (number, command) in stored.commands.iter().enumerate() {
        let Some(command) = command else { continue };
        let highest = command.records.iter().map(|r| r.bytes[5]).max().unwrap_or(0);
        let mut table = Table::new();
        table.insert("number".into(), int(number));
        table.extend(named::command_named(&command.bytes, highest));
        // A flag the command reads, by name where a registry names it.
        if let Some(Value::String(hex)) = table.get("game_flag")
            && let Ok(id) = u16::from_str_radix(hex.trim_start_matches("0x"), 16)
            && let Some((registry, flag)) = reading.flags.get(&id)
        {
            let mut named = Table::new();
            named.insert("flags".into(), Value::String(registry.clone()));
            named.insert("flag".into(), Value::String(flag.clone()));
            table.insert("game_flag".into(), Value::Table(named));
        }
        let mut listed = Vec::new();
        for (n, record) in command.records.iter().enumerate() {
            let markup = text.next().cloned().unwrap_or_default();
            let key = if markup.is_empty() { String::new() } else { format!("{}/{number}/{n}", reading.prefix) };
            if !markup.is_empty() {
                strings.push((key.clone(), markup));
            }
            listed.push(Value::Table(named::record_named(record.bytes, &key, speakers)));
        }
        if !listed.is_empty() {
            table.insert("record".into(), Value::Array(listed));
        }
        commands.push(Value::Table(table));
    }
    (commands, strings)
}

/// The smallest rectangle holding every painted texel of a page, `[x, y, w, h]`.
fn bounding_box(page: &[u8], width: usize) -> Option<[u8; 4]> {
    let mut found: Option<(usize, usize, usize, usize)> = None;
    for (i, v) in page.iter().enumerate() {
        if *v == 0 {
            continue;
        }
        let (x, y) = (i % width, i / width);
        found = Some(found.map_or((x, y, x, y), |(a, b, c, d)| (a.min(x), b.min(y), c.max(x), d.max(y))));
    }
    let (x0, y0, x1, y1) = found?;
    let byte = |v: usize| u8::try_from(v).unwrap_or(u8::MAX);
    Some([byte(x0), byte(y0), byte(x1 - x0 + 1), byte(y1 - y0 + 1)])
}
