//! pe2.game: the kinds of the game's data besides its images (`pe2.images`) - the discs and their stages, folders and
//! files, the files grouped, the streams and movies, the scenes' audio, the executable, plain text and binary data.
//! Each kind's icon and label, and the fields read in hex: a file's sector length, a scene's sound bank mask.

use pe2_ext::{Field, Icon, Registry, Result, Tint, Value};

pe2_ext::extension!("pe2.game", "0.1.0", register);

/// A number as it is written in the game's tables: in hex.
fn hex(value: &Value) -> Option<String> {
    Some(format!("0x{:X}", value.as_integer()?))
}

#[expect(clippy::unnecessary_wraps, reason = "an entry point may fail; this one does not")]
fn register(r: &mut Registry) -> Result<()> {
    let kinds = [
        ("disc", "Disc", Icon::Disc, Tint::Structure),
        ("stage", "Stage", Icon::Stack, Tint::Structure),
        ("folder", "Folder", Icon::Folder, Tint::Structure),
        ("file_group", "File group", Icon::Grid, Tint::Structure),
        ("movie", "Movie", Icon::Film, Tint::Motion),
        ("stream", "Stream", Icon::Film, Tint::Motion),
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
    r.kind("scene_audio")
        .alias("scene_audio")
        .label("Scene audio")
        .icon(Icon::Waveform, Tint::Sound)
        .field(Field::new("sound_bank_mask").label("Sound bank mask").format(hex))
        .register();
    Ok(())
}
