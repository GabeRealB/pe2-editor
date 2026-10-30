//! pe2.str: movies kept as STR, the game's form. A clip of an STR stream is its run of sectors as the disc stores it -
//! a container, interleaving MDEC video in BS frames with XA ADPCM audio - kept as `native.str`. The disc's movie files
//! (INTER0.STR, INTER1.STR) are containers holding movies back to back (`pe2.game`'s `stream`), assembled from them; a
//! record of a stage (`pe2.game`'s `movie_slot`) names the movie it plays. This extension extracts the movies from their
//! containers (`extract`), says what one's stream is from a few of its sectors (`stream`): a movie is tens of
//! megabytes - and decodes its video and sound to play it (`frames`), what `pe2.movies` takes a movie to be. Extraction
//! makes no source of a movie: STR is its only truth, and what a source would hold is lossy by then. An import brings a
//! stream to `encode`, which is held to the STR it replaces.
//!
//! A video file is made of that stream by what provides the `video-container` service; that dependency is optional, as
//! a movie plays without one: without one a movie has no `video` output, nothing else changing.

use formats::str::{Coding, SECTOR, SECTORS_A_SECOND, Sector};
use pe2_ext::ty::{self, Files, Registered};
use pe2_ext::{Asset, Name, Registry, Result, Retain, Signature, Table, Value, anyhow};

mod encode;
mod extract;
mod frames;

pe2_ext::extension!("pe2.str", "0.1.0", register, requires = ["pe2.movies@^0.1"], optional = ["video-container@^1"]);

/// A movie's kind.
const MOVIE: &str = "pe2.movies@movie";

/// A movie's native file: its sectors.
pub const NATIVE: &str = "native.str";

/// How many sectors at either end are looked through for a video one, or an audio one (they are interleaved).
const LOOK: u64 = 32;

/// A movie's native form: its sectors as the disc stores them, recognized by their name, passed by reference.
pub struct Str;

impl Registered for Str {
    const NAME: &str = "pe2.str@str";
    const LABEL: &str = "STR movie";

    fn signatures() -> Vec<Signature> {
        vec![Signature::Name(NATIVE.into())]
    }
}

impl Files for Str {}

/// A movie's stack: its stored sectors.
const STACK: &str = r#"[[stack]]
op = "file"
file = "native.str"

[[stack]]
op = "output"
name = "native"
"#;

fn register(r: &mut Registry) -> Result<()> {
    let _ = r.value_type::<Str>();
    let _ = r.op("stream").label("Stream").input::<Str>("clip").output::<ty::Table>().run(|call| {
        let clip = call.take::<Str>(0)?;
        Ok(self::stream(&call.lend(&clip).asset()))
    });
    // Played: each frame with the sound up to it.
    let _ = r.op("frames").label("Frames").input::<Str>("clip").output::<ty::Playback>().streams(|call| {
        let clip = call.take::<Str>(0)?;
        Ok(Box::new(frames::Frames::start(call, clip)?))
    });
    // A movie made of a stream, with the one it replaces, as the layer below has it: an import's last step.
    let _ = r.op("encode").label("STR").input::<ty::Stream<ty::Playback>>("frames").input::<Str>("original").output::<Str>().run(|call| {
        let stream = call.take::<ty::Stream<ty::Playback>>(0)?;
        let original = call.take::<Str>(1)?;
        encode::encode(call, &stream, &original)
    });
    r.add_stack(MOVIE, STACK);
    r.add_output(MOVIE, "stream", "pe2.str@stream", None, Retain::Transient);
    r.add_output(MOVIE, "frames", "pe2.str@frames", None, Retain::Transient);
    // Shown by a frame a quarter in: the first are often black.
    r.add_output(MOVIE, "frame", "pe2.movies@frame", Some("frames"), Retain::Transient)
        .members(|asset| timing(asset).map(|t| (t.count / 4, format!("Frame {}", t.first + t.count / 4))).into_iter().collect());
    // A movie's frames, as a video file in an encoding: made when it is asked for.
    r.add_output(MOVIE, "video", "video-container@encode", Some("frames"), Retain::Cache).members(|_| vec![("lossless".to_string(), "Lossless".to_string())]);
    let movie = Name::parse(MOVIE).ok_or_else(|| anyhow!("{MOVIE}: not a qualified kind"))?;
    let _ = r.extractor("clips", &movie, &[], extract::begin);
    Ok(())
}

/// The bytes of a count of sectors.
pub(crate) fn sectors(count: u64) -> usize {
    usize::try_from(count).unwrap_or(usize::MAX).saturating_mul(SECTOR)
}

/// The first video sector's chunk at or after a sector, among the `LOOK` from it.
pub(crate) fn chunk_at(asset: &Asset, at: u64) -> Option<formats::str::Chunk> {
    let bytes = asset.read_range(NATIVE, at * SECTOR as u64, sectors(LOOK)).ok()?;
    bytes.as_chunks::<SECTOR>().0.iter().find_map(|s| match Sector::raw(s) {
        Sector::Video(chunk, _) => Some(chunk),
        _ => None,
    })
}

/// How a movie plays: frames numbered from `first`, `count` of them, `rate` a second.
pub(crate) struct Timing {
    pub first: u32,
    pub count: u32,
    pub rate: f64,
}

/// How a movie plays: its frames, by the numbers its first and last carry, at the rate they fill its sectors.
pub(crate) fn timing(asset: &Asset) -> Option<Timing> {
    let sectors = asset.size(NATIVE)? / SECTOR as u64;
    let first = chunk_at(asset, 0)?.frame;
    let last = (0..LOOK.min(sectors)).find_map(|back| {
        let bytes = asset.read_range(NATIVE, (sectors - 1 - back) * SECTOR as u64, SECTOR).ok()?;
        match Sector::raw(&bytes) {
            Sector::Video(chunk, _) => Some(chunk.frame),
            _ => None,
        }
    })?;
    let count = last.checked_sub(first)? + 1;
    #[expect(clippy::cast_precision_loss, reason = "sector counts")]
    let rate = f64::from(count) * SECTORS_A_SECOND as f64 / sectors as f64;
    Some(Timing { first, count, rate })
}

/// What a movie's stream is, from a few sectors at each end: its sectors and duration, its frames (the last one's
/// number) and their size, the sectors a frame takes, and its audio.
fn stream(asset: &Asset) -> Table {
    let mut stream = Table::new();
    let Some(size) = asset.size(NATIVE) else { return stream };
    let sectors = size / SECTOR as u64;
    let integer = |v: u64| Value::Integer(i64::try_from(v).unwrap_or(i64::MAX));
    stream.insert("sectors".into(), integer(sectors));
    let seconds = sectors / SECTORS_A_SECOND;
    stream.insert("duration".into(), Value::String(format!("{}:{:02}", seconds / 60, seconds % 60)));
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
        stream.insert("frames".into(), integer(u64::from(last.frame)));
        stream.insert("frame_size".into(), Value::Array(vec![integer(first.width.into()), integer(first.height.into())]));
        stream.insert("sectors_per_frame".into(), integer(first.chunks.into()));
    }
    let audio = near(0, true).filter_map(read).find_map(|s| match Sector::raw(&s) {
        Sector::Audio(coding, _) => Some(Coding::of(coding)),
        _ => None,
    });
    let audio = audio.map_or_else(
        || "none".to_string(),
        |c| format!("XA ADPCM, {}, {} Hz", if c.stereo { "stereo" } else { "mono" }, c.rate),
    );
    stream.insert("audio".into(), Value::String(audio));
    stream
}
