//! pe2.movies: the game's movies. A movie is a clip of an STR stream - its run of sectors as the disc stores it, MDEC
//! video in BS frames interleaved with XA ADPCM audio - kept as `native.str`. The disc's movie files (INTER0.STR,
//! INTER1.STR) are containers holding movies back to back (`pe2.game`'s `stream`), assembled from them; a record of
//! a stage (`pe2.game`'s `movie_slot`) names the movie it plays. This extension extracts the movies from their
//! containers (`extract`) and says what one's stream is from a few of its sectors (`stream`): a movie is tens of megabytes.

use formats::str::{SECTOR, Sector};
use pe2_ext::ty::{self, Files, Registered};
use pe2_ext::{Asset, Icon, Image, Pcm, Registry, Result, Retain, Signature, Table, Timing, Tint, Value, anyhow};

mod extract;

pe2_ext::extension!("pe2.movies", "0.1.0", register);

/// A movie's native file: its sectors.
pub const NATIVE: &str = "native.str";

/// A movie's native form: its sectors as the disc stores them, recognized by their name, passed by reference.
pub struct Str;

impl Registered for Str {
    const NAME: &str = "pe2.movies@str";
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

/// The INTER files are read at the drive's double speed: 150 sectors a second.
const SECTORS_A_SECOND: u64 = 150;

/// How many sectors at either end are looked through for a video one, or an audio one (they are interleaved).
const LOOK: u64 = 32;

#[expect(clippy::unnecessary_wraps, reason = "an entry point may fail; this one does not")]
fn register(r: &mut Registry) -> Result<()> {
    let _ = r.value_type::<Str>();
    let stream = r.op("stream").label("Stream").input::<Str>("clip").output::<ty::Table>().run(|call| {
        let clip = call.take::<Str>(0)?;
        Ok(self::stream(&call.lend(&clip).asset()))
    });
    let frame = r.op("frame").label("Frame").input::<Str>("clip").output::<ty::Image>().parameter::<u32>("frame").run(|call| {
        let number = call.parameter::<u32>()?;
        let clip = call.take::<Str>(0)?;
        self::frame(&call.lend(&clip).asset(), number)
    });
    let audio = r.op("audio").label("Sound").input::<Str>("clip").output::<ty::Pcm>().run(|call| {
        let clip = call.take::<Str>(0)?;
        self::audio(&call.lend(&clip).asset())
    });
    let movie = r
        .kind("movie")
        .alias("movie")
        .label("Movie")
        .icon(Icon::Film, Tint::Motion)
        .stack(STACK)
        .output("stream", &stream, Retain::Transient)
        // Shown by a frame a quarter in: the first are often black.
        .output("frame", &frame, Retain::Transient)
        .members(|asset| timing(asset).map(|t| (t.first + t.count / 4, format!("Frame {}", t.first + t.count / 4))).into_iter().collect())
        .output("audio", &audio, Retain::Transient)
        .playback("frame", Some("audio"), timing)
        .register();
    let _ = r.extractor("clips", &movie, &[], extract::begin);
    Ok(())
}

/// The bytes of a count of sectors.
fn sectors(count: u64) -> usize {
    usize::try_from(count).unwrap_or(usize::MAX).saturating_mul(SECTOR)
}

/// The first video sector's chunk at or after a sector, among the `LOOK` from it.
fn chunk_at(asset: &Asset, at: u64) -> Option<formats::str::Chunk> {
    let bytes = asset.read_range(NATIVE, at * SECTOR as u64, sectors(LOOK)).ok()?;
    bytes.as_chunks::<SECTOR>().0.iter().find_map(|s| match Sector::raw(s) {
        Sector::Video(chunk, _) => Some(chunk),
        _ => None,
    })
}

/// How a movie plays: its frames, by the numbers its first and last carry, at the rate they fill its sectors.
fn timing(asset: &Asset) -> Option<Timing> {
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

/// A movie's frame, by its number: found by halving its sectors (a video sector's frame number never goes back), its
/// chunks read and joined, decoded at the size they say.
fn frame(asset: &Asset, number: u32) -> Result<Image> {
    // Its chunks follow, audio between them: a window holding the largest frame's.
    const WINDOW: u64 = 96;
    let sectors = asset.size(NATIVE).ok_or_else(|| anyhow!("{}: no {NATIVE}", asset.path()))? / SECTOR as u64;
    let (mut low, mut high) = (0, sectors);
    while low < high {
        let middle = u64::midpoint(low, high);
        match chunk_at(asset, middle) {
            Some(chunk) if chunk.frame < number => low = middle + 1,
            _ => high = middle,
        }
    }
    let bytes = asset.read_range(NATIVE, low * SECTOR as u64, self::sectors(WINDOW.min(sectors - low)))?;
    let frames = formats::str::frames(bytes.as_chunks::<SECTOR>().0.iter().map(|s| Sector::raw(s)));
    let found = frames.into_iter().find(|f| f.number == number).ok_or_else(|| anyhow!("{}: no whole frame {number}", asset.path()))?;
    let decoded = codecs::bs::decode_frame(&found.bytes, found.width.into(), found.height.into())
        .map_err(|e| anyhow!("{}: frame {number}: {e}", asset.path()))?;
    let rgba = decoded.rgb.as_chunks::<3>().0.iter().flat_map(|&[r, g, b]| [r, g, b, 255]).collect();
    Ok(Image { width: decoded.width, height: decoded.height, rgba })
}

/// A movie's sound: its XA sectors decoded in order, a range of sectors read at a time.
fn audio(asset: &Asset) -> Result<Pcm> {
    const RANGE: u64 = 4096;
    let sectors = asset.size(NATIVE).ok_or_else(|| anyhow!("{}: no {NATIVE}", asset.path()))? / SECTOR as u64;
    let mut decoder = codecs::xa::Decoder::default();
    let mut pcm = Pcm { rate: 0, channels: 0, samples: Vec::new() };
    let mut at = 0;
    while at < sectors {
        let count = RANGE.min(sectors - at);
        let bytes = asset.read_range(NATIVE, at * SECTOR as u64, self::sectors(count))?;
        for sector in bytes.as_chunks::<SECTOR>().0 {
            let Sector::Audio(coding, data) = Sector::raw(sector) else { continue };
            let coding = codecs::xa::Coding::of(coding);
            if pcm.rate == 0 {
                (pcm.rate, pcm.channels) = (coding.rate, if coding.stereo { 2 } else { 1 });
            }
            pcm.samples.extend(decoder.sector(coding, data).unwrap_or_default());
        }
        at += count;
    }
    Ok(pcm)
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
        Sector::Audio(coding, _) => Some(codecs::xa::Coding::of(coding)),
        _ => None,
    });
    let audio = audio.map_or_else(
        || "none".to_string(),
        |c| format!("XA ADPCM, {}, {} Hz", if c.stereo { "stereo" } else { "mono" }, c.rate),
    );
    stream.insert("audio".into(), Value::String(audio));
    stream
}
