//! pe2.str: movies kept as STR, the game's form. A clip of an STR stream is its run of sectors as the disc stores it -
//! a container, interleaving MDEC video in BS frames with XA ADPCM audio - kept as `native.str`. The disc's movie files
//! (INTER0.STR, INTER1.STR) are containers holding movies with sound back to back (`pe2.game`'s `stream`), assembled
//! from them; a movie without sound is stored in its stage, and a file lists the movies its package plays (`streams`).
//! This extension extracts the movies from where they are (`extract`), says what one's stream is from a few of its sectors (`stream`): a movie is tens of
//! megabytes - and decodes its video and sound to play it (`frames`), what `pe2.movies` takes a movie to be. Extraction
//! makes no source of a movie: STR is its only truth, and what a source would hold is lossy by then. An import brings a
//! stream to `encode`, which is held to the STR it replaces.
//!
//! A video file is made of that stream by what provides the `video-container` service; that dependency is optional, as
//! a movie plays without one: without one a movie has no `video` output, nothing else changing.

use formats::str::{Coding, Form, HEADER, SECTORS_A_SECOND, Sector};
use pe2_ext::media::VideoFrame;
use pe2_ext::ty::{self, Files, Registered};
use pe2_ext::{Asset, Data, Image, Name, Registry, Result, Signature, Table, Value, anyhow};

mod encode;
mod extract;
mod frames;

pe2_ext::extension!(register);

/// A movie's type.
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
id = "clip"
op = "file"
file = "native.str"

[[stack]]
op = "output"
name = "native"
"#;

/// What a movie offers: each hanging off the layer named `clip`.
const OFFERS: &str = r#"
[[stack]]
op = "pe2.str@stream"
clip = { from = "clip" }

[[stack]]
op = "output"
name = "stream"

[[stack]]
id = "frames"
op = "pe2.str@frames"
clip = { from = "clip" }

[[stack]]
op = "preview"
name = "frames"

[[stack]]
op = "pe2.str@frame"
frames = { from = "frames" }

[[stack]]
op = "output"
name = "frame"
"#;

/// A movie made of a video file, over the one it replaces: decoded by the service that reads video, and encoded by this.
const IMPORT: &str = r#"[[stack]]
id = "clip"
op = "graph"
result = "encoded"

[stack.ops.source]
op = "file"
file = "{file}"

[stack.ops.decoded]
op = "video-container@decode"
from = { source = "source" }

[stack.ops.original]
op = "below"
type = "pe2.str@str"

[stack.ops.encoded]
op = "pe2.str@encode"
from = { frames = "decoded", original = "original" }

[[stack]]
op = "output"
name = "native"
retain = "cache"
when = "eager"
"#;

/// The files that take: what a video container commonly is.
const VIDEO: [&str; 5] = ["*.mp4", "*.mkv", "*.mov", "*.webm", "*.avi"];

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
    let _ = r
        .op("encode")
        .label("STR")
        .input::<ty::Stream<ty::Playback>>("frames")
        .input::<Str>("original")
        .output::<Str>()
        .run(|call| {
            let stream = call.take::<ty::Stream<ty::Playback>>(0)?;
            let original = call.take::<Str>(1)?;
            encode::encode(call, &stream, &original)
        });
    // Shown by a frame a quarter in: the first are often black.
    let _ = r
        .op("frame")
        .label("Frame")
        .input::<ty::Stream<ty::Playback>>("frames")
        .output::<ty::Image>()
        .parameter::<u32>("frame")
        .members(|asset| {
            timing(asset).map(|t| (t.count / 4, format!("Frame {}", t.first + t.count / 4))).into_iter().collect()
        })
        .run(|call| {
            let number = call.parameter::<u32>()?;
            let frames = call.take::<ty::Stream<ty::Playback>>(0)?;
            frames.seek(number.into());
            let item = frames.next()?.ok_or_else(|| anyhow!("no frame {number}"))?;
            picture(&item).ok_or_else(|| anyhow!("frame {number} has no picture"))?
        });
    drop(
        r.template("movie_from_video", MOVIE, &format!("{IMPORT}{OFFERS}"))
            .label("Movie, from a video file")
            .files(&VIDEO),
    );
    let _ = r.extend(MOVIE).of(Str::NAME).stack(&format!("{STACK}{OFFERS}")).register();
    let movie = Name::parse(MOVIE).ok_or_else(|| anyhow!("{MOVIE}: not a qualified type"))?;
    let _ = r.extractor("clips", &movie, &[], extract::begin);
    Ok(())
}

/// An item's picture, as an image; none where it has none.
fn picture(item: &Data) -> Option<Result<Image>> {
    let frame = |data: &Data| if let Data::Video(frame) = data { Some(image(frame)) } else { None };
    match item {
        Data::List(parts) => parts.iter().find_map(frame),
        other => frame(other),
    }
}

fn image(frame: &VideoFrame) -> Result<Image> {
    Ok(Image { width: frame.width as usize, height: frame.height as usize, rgba: frame.rgba()? })
}

/// How a movie's sectors are stored, from its first.
pub(crate) fn form(asset: &Asset) -> Option<Form> {
    Some(Form::of(&asset.read_range(NATIVE, 0, HEADER).ok()?))
}

/// A movie's sectors as stored: their form, and how many.
pub(crate) fn stored(asset: &Asset) -> Option<(Form, u64)> {
    let form = form(asset)?;
    Some((form, asset.size(NATIVE)? / form.size() as u64))
}

/// The bytes of a count of sectors of a form.
pub(crate) fn sectors(form: Form, count: u64) -> usize {
    usize::try_from(count).unwrap_or(usize::MAX).saturating_mul(form.size())
}

/// Sectors of a form, read from `at`: each one's bytes.
pub(crate) fn read_sectors(asset: &Asset, form: Form, at: u64, count: u64) -> Result<Vec<u8>> {
    asset.read_range(NATIVE, at * form.size() as u64, sectors(form, count))
}

/// The first video sector's chunk at or after a sector, among the `LOOK` from it.
pub(crate) fn chunk_at(asset: &Asset, form: Form, at: u64) -> Option<formats::str::Chunk> {
    let bytes = read_sectors(asset, form, at, LOOK).ok()?;
    bytes.chunks_exact(form.size()).find_map(|s| match form.sector(s) {
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
    let (form, sectors) = stored(asset)?;
    let first = chunk_at(asset, form, 0)?.frame;
    let last = (0..LOOK.min(sectors)).find_map(|back| {
        let bytes = read_sectors(asset, form, sectors - 1 - back, 1).ok()?;
        match form.sector(&bytes) {
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
    let Some((form, sectors)) = stored(asset) else { return stream };
    let integer = |v: u64| Value::Integer(i64::try_from(v).unwrap_or(i64::MAX));
    stream.insert("sectors".into(), integer(sectors));
    let seconds = sectors / SECTORS_A_SECOND;
    stream.insert("duration".into(), Value::String(format!("{}:{:02}", seconds / 60, seconds % 60)));
    let read = |at: u64| read_sectors(asset, form, at, 1).ok();
    let near = |start: u64, forward: bool| {
        (0..LOOK.min(sectors)).filter_map(move |i| if forward { Some(start + i) } else { start.checked_sub(i) })
    };
    let first = near(0, true).filter_map(read).find_map(|s| match form.sector(&s) {
        Sector::Video(chunk, _) => Some(chunk),
        _ => None,
    });
    let last = near(sectors.saturating_sub(1), false).filter_map(read).find_map(|s| match form.sector(&s) {
        Sector::Video(chunk, _) => Some(chunk),
        _ => None,
    });
    if let (Some(first), Some(last)) = (first, last) {
        stream.insert("frames".into(), integer(u64::from(last.frame)));
        stream
            .insert("frame_size".into(), Value::Array(vec![integer(first.width.into()), integer(first.height.into())]));
        stream.insert("sectors_per_frame".into(), integer(first.chunks.into()));
    }
    let audio = near(0, true).filter_map(read).find_map(|s| match form.sector(&s) {
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
