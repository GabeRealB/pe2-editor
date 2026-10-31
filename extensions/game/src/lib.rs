//! pe2.game: the kinds of the game's data besides its images (`pe2.images`) and movies (`pe2.movies`, `pe2.str`) - the discs and
//! their stages, folders and files, the files grouped, the disc's movie files (`stream`: a list of entries, each a
//! movie in order, assembled from them; the entries are the outputs the records name), the stages' records of the movies
//! they play (`movie_slot`: a `StreamSlot`'s fields, naming its movie file's entry) and of their scenes' audio, the
//! executable, plain text and binary data. Each kind's icon and label, and the fields read in hex: a file's sector
//! length, a scene's sound bank mask.

use pe2_ext::{Asset, Cell, ContentRow, Contents, Field, Icon, Project, Registry, Result, Retain, Style, Table, Tint, Ui, Value, View, ty};

pe2_ext::extension!("pe2.game", "0.1.0", register);

/// A number as it is written in the game's tables: in hex.
fn hex(value: &Value) -> Option<String> {
    Some(format!("0x{:X}", value.as_integer()?))
}

/// A movie file's stack: its entries' movies, joined in order - the file as the disc stores it.
const STREAM: &str = r#"[[stack]]
op = "references"
field = "entries"
key = "movie"
output = "native"

[[stack]]
op = "concatenate"
name = "native.str"

[[stack]]
op = "output"
name = "native"
retain = "cache"
"#;

#[expect(clippy::unnecessary_wraps, reason = "an entry point may fail; this one does not")]
fn register(r: &mut Registry) -> Result<()> {
    let kinds = [
        ("disc", "Disc", Icon::Disc, Tint::Structure),
        ("stage", "Stage", Icon::Stack, Tint::Structure),
        ("folder", "Folder", Icon::Folder, Tint::Structure),
        ("file_group", "File group", Icon::Grid, Tint::Structure),
        ("movie_slot", "Movie slot", Icon::Film, Tint::Motion),
        ("executable", "Executable", Icon::Terminal, Tint::Code),
        ("text", "Text", Icon::Text, Tint::Plain),
        ("binary", "Binary data", Icon::Binary, Tint::Plain),
    ];
    for (name, label, icon, tint) in kinds {
        r.kind(name).alias(name).label(label).icon(icon, tint).register();
    }
    r.kind("file")
        .alias("file")
        .label("File")
        .icon(Icon::File, Tint::Structure)
        .field(Field::new("sector_len").label("Sector length").format(hex))
        .register();
    // A disc's movie file: a list of entries, each a movie in order, which the stages' records name.
    let entry = r.op("entry").label("A movie file's entry").output::<ty::Table>().parameter::<String>("id").run(|call| {
        let id: String = call.parameter()?;
        let lent = call.lend_settings(&Table::new());
        let asset = lent.asset();
        let entries = asset.parts("entries");
        let (_, entry) = entries.iter().find(|(known, _)| **known == id).ok_or_else(|| pe2_ext::anyhow!("it has no entry {id}"))?;
        entry.as_table().cloned().ok_or_else(|| pe2_ext::anyhow!("its entry {id} is no table"))
    });
    r.kind("stream")
        .alias("stream")
        .label("Movie file")
        .icon(Icon::Stack, Tint::Motion)
        .stack(STREAM)
        .field(Field::new("entries").custom())
        .contents(entry_rows)
        .output("entries", &entry, Retain::Transient)
        .members(|asset| asset.parts("entries").into_iter().map(|(id, _)| (id.to_string(), id.to_string())).collect())
        .inspector("Entries", |_| Box::new(Entries))
        .register();
    r.kind("scene_audio")
        .alias("scene_audio")
        .label("Scene audio")
        .icon(Icon::Waveform, Tint::Sound)
        .field(Field::new("sound_bank_mask").label("Sound bank mask").format(hex))
        .register();
    Ok(())
}

/// A movie file's entries, in order, which a stage's record names (`entries/<id>`), its offset the entry's place.
fn entry_rows(asset: &Asset, _: &dyn Project) -> Contents {
    let rows = asset.parts("entries").into_iter().map(|(id, _)| ContentRow::new(&format!("entries/{id}"), id).depth(1));
    let rows: Vec<ContentRow> = std::iter::once(ContentRow::heading("entries", "Entries")).chain(rows).collect();
    Contents { rows, ..Contents::default() }
}

/// A change to a movie file's entries.
enum Change {
    Up(usize),
    Down(usize),
    Remove(usize),
    /// A movie added at the end: its uid and path.
    Add(String, String),
}

/// A movie file's entries, as a list: each its place, its id and its movie, and - where it can be edited - moved,
/// removed, or a movie added at the end; a change is one step of the history, and the file is laid out again from
/// the list (its records' offsets follow).
struct Entries;

impl View for Entries {
    fn show(&mut self, ui: &mut Ui) {
        let Some(list) = ui.asset().map(|a| a.parts("entries").into_iter().map(|(id, v)| (id.to_string(), v.clone())).collect::<Vec<_>>())
        else {
            return;
        };
        let movies: Vec<(String, String)> =
            ui.project().find("movie").iter().filter_map(|m| Some((m.uid()?.to_string(), m.path().to_string()))).collect();
        let editable = ui.editable();
        let count = list.len();
        let mut change = None;
        ui.section(&format!("Entries  ·  {count}"), |ui| {
            // A row each: its place; where it can be edited, its moves and its removal (a column each whether or not
            // the row has it, so they line up, and before the names, so a narrow pane never hides them); its id, its
            // movie.
            let rows: Vec<Vec<Cell>> = list
                .iter()
                .enumerate()
                .map(|(i, (id, entry))| {
                    let mut row = vec![Cell::Text(format!("{i}"), Style::Weak)];
                    if editable {
                        let up = Cell::Button(Icon::CaretUp, "Move it up: it is played from an earlier place".into());
                        let down = Cell::Button(Icon::CaretDown, "Move it down".into());
                        row.push(if i > 0 { up } else { Cell::Empty });
                        row.push(if i + 1 < count { down } else { Cell::Empty });
                        row.push(Cell::Button(Icon::Cross, "Remove it: records naming it no longer resolve".into()));
                    }
                    row.push(Cell::Text(id.clone(), Style::Strong));
                    row.push(entry.get("movie").map_or(Cell::Empty, |m| Cell::Reference(m.clone())));
                    row
                })
                .collect();
            change = match ui.table("entries", &rows) {
                Some((i, 1)) => Some(Change::Up(i)),
                Some((i, 2)) => Some(Change::Down(i)),
                Some((i, 3)) => Some(Change::Remove(i)),
                _ => None,
            };
            if editable {
                let options: Vec<String> = movies.iter().map(|(_, path)| path.clone()).collect();
                if let Some(chosen) = ui.combo("entries/add", "Add a movie...", &options) {
                    change = movies.get(chosen).map(|(uid, path)| Change::Add(uid.clone(), path.clone()));
                }
            }
        });
        if let Some(change) = change {
            let entries = changed(list, change);
            ui.set("entries", Some(&Value::Table(entries.into_iter().collect())));
        }
    }
}

/// The entries after a change: an added movie's id is its room and name (`cafeteria_movie_100`), numbered where taken.
fn changed(mut list: Vec<(String, Value)>, change: Change) -> Vec<(String, Value)> {
    match change {
        Change::Up(i) => list.swap(i - 1, i),
        Change::Down(i) => list.swap(i, i + 1),
        Change::Remove(i) => {
            list.remove(i);
        }
        Change::Add(uid, path) => {
            let base = path.split_once("/rooms/").map_or(path.as_str(), |(_, room)| room).replace("/movies/", "_").replace('/', "_");
            let mut id = base.clone();
            let mut n = 2;
            while list.iter().any(|(taken, _)| *taken == id) {
                id = format!("{base}_{n}");
                n += 1;
            }
            let mut entry = Table::new();
            entry.insert("movie".into(), Value::String(uid));
            list.push((id, Value::Table(entry)));
        }
    }
    list
}
