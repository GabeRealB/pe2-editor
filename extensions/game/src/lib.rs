//! pe2.game: the types of the game's data besides its images (`pe2.images`) and movies (`pe2.movies`, `pe2.str`) - the discs and
//! their stages, folders and files, the files grouped, a disc's movie file (`stream`: its stages' movies with sound, in
//! order, made from them by `inter`), the scenes' audio streams a stage stores (`scene_audio`), the executable, plain
//! text and binary data. A file lists the streams its package plays (`streams`): each a movie or a scene's audio, by
//! name, with what of its record does not follow from where it is. Each type's icon and label, and the fields read in
//! hex: a file's sector length.

use pe2_ext::{Asset, Call, Data, Field, Icon, Registry, Result, Table, Tint, Typed, Value, anyhow, ty};

pe2_ext::extension!("pe2.game", "0.1.0", register);

/// A number as it is written in the game's tables: in hex.
fn hex(value: &Value) -> Option<String> {
    Some(format!("0x{:X}", value.as_integer()?))
}

/// A file as the disc stores it.
const STORED: &str = "[[stack]]\nop = \"file\"\nfile = \"{file}\"\n\n[[stack]]\nop = \"output\"\nname = \"native\"\n";

/// A movie file's stack: its stages' movies with sound, joined in order - the file as the disc stores it.
const STREAM: &str = r#"[[stack]]
id = "file"
op = "pe2.game@stream"

[[stack]]
op = "pe2.game@inter"
file = { from = "file" }

[[stack]]
op = "output"
name = "native"
retain = "cache"
"#;

/// What a node's fields are, read by another's op.
const SETTINGS: &str = "settings";

/// A stage's files in its order, each once: stage 0's across its groups by id (a group's number, as many ten
/// thousands, and the file's in it), a stage's folders in its layout's order and their files by id.
fn stage_files(call: &Call, layout: &Table) -> Result<Vec<String>> {
    let list = |table: &Table, key: &str| table.get(key).and_then(Value::as_array).cloned().unwrap_or_default();
    let id = |v: &Value| v.get("id").and_then(Value::as_integer).unwrap_or(0);
    let asset = |v: &Value| v.get("asset").and_then(pe2_ext::target_of).map(str::to_string);
    let mut out: Vec<String> = Vec::new();
    let mut take = |files: Vec<(i64, String)>| {
        let mut files = files;
        files.sort_by_key(|f| f.0);
        for (_, file) in files {
            if !out.contains(&file) {
                out.push(file);
            }
        }
    };
    if layout.contains_key("groups") {
        let mut files = Vec::new();
        for group in list(layout, "groups") {
            let node = asset(&group).ok_or_else(|| anyhow!("a group of the layout names nothing"))?;
            for file in list(&call.table_of(&node, SETTINGS)?, "files") {
                files.extend(asset(&file).map(|a| (id(&group) * 10_000 + id(&file), a)));
            }
        }
        take(files);
    } else {
        for folder in list(layout, "folders") {
            let node = asset(&folder).ok_or_else(|| anyhow!("a folder of the layout names nothing"))?;
            take(
                list(&call.table_of(&node, SETTINGS)?, "files")
                    .iter()
                    .filter_map(|f| Some((id(f), asset(f)?)))
                    .collect(),
            );
        }
    }
    Ok(out)
}

/// A disc's movie file: the movies with sound its stages' files play on this disc, in the stages' order - a movie
/// entry with a volume (a movie without plays from the stage), and naming this disc where it names one.
fn inter(call: &mut Call) -> Result<Typed> {
    let given = call.given();
    let disc = given.get("disc").and_then(Value::as_integer).ok_or_else(|| anyhow!("it names no disc"))?;
    let stages = given.get("stages").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut movies = Vec::new();
    for stage in &stages {
        let stage = pe2_ext::target_of(stage).ok_or_else(|| anyhow!("a stage names nothing"))?;
        let layout = call.table_of(stage, SETTINGS)?;
        for file in stage_files(call, &layout)? {
            let fields = call.table_of(&file, SETTINGS)?;
            for entry in fields.get("streams").and_then(Value::as_array).into_iter().flatten() {
                let integer = |key: &str| entry.get(key).and_then(Value::as_integer);
                let movie = entry.get("type").and_then(Value::as_str) == Some("movie");
                if movie && integer("volume_table_index").unwrap_or(0) != 0 && integer("disc").is_none_or(|d| d == disc)
                {
                    movies.push(
                        entry
                            .get("asset")
                            .and_then(pe2_ext::target_of)
                            .ok_or_else(|| anyhow!("{file}: a stream names nothing"))?
                            .to_string(),
                    );
                }
            }
        }
    }
    let mut writer = call.writer("native.str")?;
    for movie in &movies {
        let Data::Typed(native) = call.output_of(movie, "native")? else {
            pe2_ext::bail!("{movie}: its native is no file")
        };
        std::io::Write::write_all(&mut writer, &call.read(native.file()?)?)?;
    }
    Ok(Typed::new(pe2_ext::data::BYTES, vec![writer.finish()?]))
}

/// A file group's stack: its files, each read as itself (`files/<name>`), which a list of them names.
const FILE_GROUP: &str = r#"[[stack]]
id = "group"
op = "pe2.game@file_group"

[[stack]]
op = "pe2.game@group_file"
group = { from = "group" }

[[stack]]
op = "output"
name = "files"
"#;

/// A file's stack: its fields, and each stream it lists as the file read as that stream (`streams/<name>`), whose id its
/// tokenizer derives.
const FILE: &str = r#"[[stack]]
id = "file"
op = "pe2.game@file"

[[stack]]
op = "pe2.game@file_stream"
file = { from = "file" }

[[stack]]
op = "output"
name = "streams"
"#;

/// The names of a file's streams, in its order.
fn file_streams(asset: &Asset) -> Vec<(String, String)> {
    let streams = asset.field("streams").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    streams
        .iter()
        .filter_map(|stream| stream.get("name")?.as_str())
        .map(|name| (name.to_string(), name.to_string()))
        .collect()
}

/// The names of a file group's files, in its order.
fn group_files(asset: &Asset) -> Vec<(String, String)> {
    let files = asset.field("files").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    files
        .iter()
        .filter_map(|file| file.get("name")?.as_str())
        .map(|name| (name.to_string(), name.to_string()))
        .collect()
}

#[expect(clippy::unnecessary_wraps, reason = "an entry point may fail; this one does not")]
fn register(r: &mut Registry) -> Result<()> {
    let held = [
        ("disc", "Disc", Icon::Disc, Tint::Structure),
        ("stage", "Stage", Icon::Stack, Tint::Structure),
        ("folder", "Folder", Icon::Folder, Tint::Structure),
    ];
    for (name, label, icon, tint) in held {
        r.named_type(name).alias(name).label(label).icon(icon, tint).data().extracted().register();
    }
    // A file of a group, as what reads it: the group again, told which (`tokens::leads_on`), so that its id is the group's to say.
    let _ = r
        .op("group_file")
        .label("A file of the group")
        .takes("group", &["pe2.game@file_group"])
        .output::<ty::Table>()
        .parameter::<String>("name")
        .members(group_files)
        .run(|call| {
            let name: String = call.parameter()?;
            Ok(pe2_ext::tokens::leads_on(call.host.path(), "file", &[("file", &name)]))
        });
    r.named_type("file_group")
        .alias("file_group")
        .label("File group")
        .icon(Icon::Grid, Tint::Structure)
        .data()
        .extracted()
        .stack(FILE_GROUP)
        .register();
    // What the disc stores as files of their own, known by their names.
    let stored = [
        ("executable", "Executable", Icon::Terminal, Tint::Code, "exe", "*.exe"),
        ("text", "Text", Icon::Text, Tint::Plain, "cnf", "*.cnf"),
        ("binary", "Binary data", Icon::Binary, Tint::Plain, "bin", "*.bin"),
        ("scene_audio", "Scene audio", Icon::Waveform, Tint::Sound, "mts", "*.mts"),
    ];
    for (name, label, icon, tint, value, pattern) in stored {
        let value = r.named_type(value).label(label).named(pattern).register();
        r.named_type(name).alias(name).label(label).icon(icon, tint).of(&value.to_string()).stack(STORED).register();
    }
    // A stream of a file, as what reads it: the file again, told which, so that its id is the file's to say.
    let _ = r
        .op("file_stream")
        .label("A stream of the file")
        .takes("file", &["pe2.game@file"])
        .output::<ty::Table>()
        .parameter::<String>("name")
        .members(file_streams)
        .run(|call| {
            let name: String = call.parameter()?;
            Ok(pe2_ext::tokens::leads_on(call.host.path(), "file", &[("stream", &name)]))
        });
    r.named_type("file")
        .alias("file")
        .label("File")
        .icon(Icon::File, Tint::Structure)
        .data()
        .extracted()
        .stack(FILE)
        .field(Field::new("sector_len").label("Sector length").format(hex))
        .field(Field::new("streams").label("Streams"))
        .register();
    // A disc's movie file, made of its stages' movies (`inter`).
    let _ = r
        .op("inter")
        .label("Its stages' movies with sound")
        .takes("file", &["pe2.game@stream"])
        .output::<ty::Bytes>()
        .prepared_by_running()
        .run(inter);
    r.named_type("stream")
        .alias("stream")
        .label("Movie file")
        .data()
        .extracted()
        .icon(Icon::Stack, Tint::Motion)
        .stack(STREAM)
        .register();
    Ok(())
}
