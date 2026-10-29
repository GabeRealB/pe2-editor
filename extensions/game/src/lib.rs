//! pe2.game: the kinds of the game's data besides its images (`pe2.images`) - the discs and their stages, folders and
//! files, the files grouped, the streams and movies, the scenes' audio, the executable, plain text and binary data.
//! Each kind's icon and label, and the fields read in hex: a file's sector length, a scene's sound bank mask; and what a
//! movie stream is (its frames and their size), read from a sector at each end of it - a stream is hundreds of
//! megabytes, which an extension never holds whole.

use pe2_ext::{Asset, Field, Icon, Registry, Result, Table, Tint, Value};

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
        ("executable", "Executable", Icon::Terminal, Tint::Code),
        ("text", "Text", Icon::Text, Tint::Plain),
        ("binary", "Binary data", Icon::Binary, Tint::Plain),
    ];
    for (name, label, icon, tint) in kinds {
        r.kind(name).alias(name).label(label).icon(icon, tint).register();
    }
    r.kind("stream").alias("stream").label("Stream").icon(Icon::Film, Tint::Motion).facts(stream_facts).register();
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

/// A stream as its file stores it: 2336-byte sectors (a CD-ROM XA sector less its sync and header), each its 8-byte
/// subheader and its data; a video sector's data starts with the frame header (`0x0160`, `0x8001`).
const STREAM: &str = "native.str";
const SECTOR: u64 = 2336;
/// How many sectors at either end are looked through for a video one (audio sectors are interleaved).
const LOOK: u64 = 32;

/// A video sector's frame header: its frame's number, size, and sectors.
struct Frame {
    number: u32,
    width: u16,
    height: u16,
    sectors: u16,
}

fn frame(sector: &[u8]) -> Option<Frame> {
    let word = |at: usize| Some(u16::from_le_bytes(sector.get(at..at + 2)?.try_into().ok()?));
    if word(8)? != 0x0160 || word(10)? != 0x8001 {
        return None;
    }
    let number = u32::from_le_bytes(sector.get(16..20)?.try_into().ok()?);
    Some(Frame { number, sectors: word(14)?, width: word(24)?, height: word(26)? })
}

/// The first video sector among `LOOK` from `start`, forward or backward.
fn video(asset: &Asset, sectors: u64, start: u64, forward: bool) -> Option<Frame> {
    (0..LOOK.min(sectors)).find_map(|i| {
        let at = if forward { start + i } else { start.checked_sub(i)? };
        frame(&asset.read_range(STREAM, at * SECTOR, 32).ok()?)
    })
}

/// A movie stream's frames, their first size and its sectors, read from a sector at each end.
fn stream_facts(asset: &Asset) -> Table {
    let mut facts = Table::new();
    let Some(size) = asset.size(STREAM) else { return facts };
    let sectors = size / SECTOR;
    facts.insert("sectors".into(), Value::Integer(i64::try_from(sectors).unwrap_or(i64::MAX)));
    let (Some(first), Some(last)) = (video(asset, sectors, 0, true), video(asset, sectors, sectors.saturating_sub(1), false))
    else {
        return facts;
    };
    facts.insert("frames".into(), Value::Integer(i64::from(last.number)));
    facts.insert("frame_size".into(), Value::Array(vec![Value::Integer(first.width.into()), Value::Integer(first.height.into())]));
    facts.insert("sectors_per_frame".into(), Value::Integer(first.sectors.into()));
    facts
}
