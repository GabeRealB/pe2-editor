//! pe2.movies: the game's movies. A movie is a clip of an STR stream - its run of sectors as the disc stores it, MDEC
//! video in BS frames interleaved with XA ADPCM audio - kept as `native.str`. The disc's movie files (INTER0.STR,
//! INTER1.STR) are containers holding movies back to back (`pe2.game`'s `stream`), assembled from them; a record of
//! a stage (`pe2.game`'s `movie_slot`) names the movie it plays. This extension extracts the movies from their
//! containers (`extract`) and says what one is from a few of its sectors (`facts`): a movie is tens of megabytes.

use formats::str::{SECTOR, Sector};
use pe2_ext::{Asset, Icon, Registry, Result, Table, Tint, Value};

mod extract;

pe2_ext::extension!("pe2.movies", "0.1.0", register);

/// A movie's native file: its sectors.
pub const NATIVE: &str = "native.str";

/// The INTER files are read at the drive's double speed: 150 sectors a second.
const SECTORS_A_SECOND: u64 = 150;

/// How many sectors at either end are looked through for a video one, or an audio one (they are interleaved).
const LOOK: u64 = 32;

#[expect(clippy::unnecessary_wraps, reason = "an entry point may fail; this one does not")]
fn register(r: &mut Registry) -> Result<()> {
    let _ = r.format("str").label("STR movie").named(NATIVE).register();
    let movie = r.kind("movie").alias("movie").label("Movie").icon(Icon::Film, Tint::Motion).facts(facts).register();
    let _ = r.extractor("clips", &movie, &[], extract::begin);
    Ok(())
}

/// What a movie is, from a few sectors at each end: its sectors and duration, its frames (the last one's number) and
/// their size, the sectors a frame takes, and its audio.
fn facts(asset: &Asset) -> Table {
    let mut facts = Table::new();
    let Some(size) = asset.size(NATIVE) else { return facts };
    let sectors = size / SECTOR as u64;
    let integer = |v: u64| Value::Integer(i64::try_from(v).unwrap_or(i64::MAX));
    facts.insert("sectors".into(), integer(sectors));
    let seconds = sectors / SECTORS_A_SECOND;
    facts.insert("duration".into(), Value::String(format!("{}:{:02}", seconds / 60, seconds % 60)));
    let read = |at: u64| asset.read_range(NATIVE, at * SECTOR as u64, SECTOR).ok();
    let near = |start: u64, forward: bool| {
        (0..LOOK.min(sectors)).filter_map(move |i| if forward { Some(start + i) } else { start.checked_sub(i) })
    };
    let first = near(0, true).filter_map(read).find_map(|s| match Sector::raw(&s) {
        Sector::Video(chunk, _) => Some(chunk),
        _ => None,
    });
    let last = near(sectors.saturating_sub(1), false).filter_map(read).find_map(|s| match Sector::raw(&s) {
        Sector::Video(chunk, _) => Some(chunk),
        _ => None,
    });
    if let (Some(first), Some(last)) = (first, last) {
        facts.insert("frames".into(), integer(u64::from(last.frame)));
        facts.insert("frame_size".into(), Value::Array(vec![integer(first.width.into()), integer(first.height.into())]));
        facts.insert("sectors_per_frame".into(), integer(first.chunks.into()));
    }
    let audio = near(0, true).filter_map(read).find_map(|s| match Sector::raw(&s) {
        Sector::Audio(coding, _) => Some(codecs::xa::Coding::of(coding)),
        _ => None,
    });
    let audio = audio.map_or_else(
        || "none".to_string(),
        |c| format!("XA ADPCM, {}, {} Hz", if c.stereo { "stereo" } else { "mono" }, c.rate),
    );
    facts.insert("audio".into(), Value::String(audio));
    facts
}
