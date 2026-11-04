//! A movie as a stream (`frames`): each item a frame, its picture with the sound decoded since the frame before, read
//! on from where it is a range of sectors at a time. Its pictures go to the host as the decoder makes them - YCbCr
//! 4:2:0, where the host takes it - else as RGBA; its sound as 16-bit samples, interleaved. It goes on from any frame: the frame's first sector found by
//! halving the clip's sectors, the sound's decoder starting again there.

use std::collections::BTreeMap;

use formats::str::{Form, Sector};
use pe2_ext::media::{AudioData, MediaBuffer, PixelFormat, SampleFormat, VideoFrame};
use pe2_ext::{Call, Data, Generator, Result, Table, Typed, Value, anyhow};

use crate::{NATIVE, chunk_at, read_sectors};

/// How many sectors are read at a time.
const RANGE: u64 = 64;

/// A movie's frames, as a stream is read.
pub struct Frames {
    clip: Typed,
    header: Table,
    /// How the clip's sectors are stored, how many, and the one read next.
    form: Form,
    sectors: u64,
    at: u64,
    /// The number of the stream's first frame, and the frames a second.
    first: u32,
    rate: f64,
    /// The layout its pictures go in.
    format: PixelFormat,
    /// The sound's samples (frames of its channels) made since the start or the last seek, and the time that was.
    heard_since: u64,
    since: f64,
    sound: codecs::xa::Decoder,
    /// The sound decoded since the last frame was made, and how it is coded.
    heard: Vec<i16>,
    coding: Option<codecs::xa::Coding>,
    /// The frame being gathered: its first chunk's header and its chunks so far.
    gathering: Option<(formats::str::Chunk, BTreeMap<u16, Vec<u8>>)>,
}

impl Frames {
    /// A movie's frames from its first: its header its timing - the frames, their first's number, their rate - and its
    /// sound's.
    ///
    /// # Errors
    /// It has no sectors, or no frames.
    pub fn start(call: &Call<'_>, clip: Typed) -> Result<Frames> {
        let ((form, sectors), timing) = {
            let lent = call.lend(&clip);
            let asset = lent.asset();
            let stored = crate::stored(&asset).ok_or_else(|| anyhow!("{}: no {NATIVE}", asset.path()))?;
            (stored, crate::timing(&asset).ok_or_else(|| anyhow!("{}: no frames", asset.path()))?)
        };
        let mut header = Table::new();
        header.insert("count".into(), Value::Integer(timing.count.into()));
        header.insert("rate".into(), Value::Float(timing.rate));
        header.insert("first".into(), Value::Integer(timing.first.into()));
        header.insert("holds".into(), Value::String("a frame's picture, and the sound up to it".into()));
        let format =
            if pe2_ext::media::pixel_formats().contains(&PixelFormat::Yuv420p) { PixelFormat::Yuv420p } else { PixelFormat::Rgba8888 };
        header.insert("video".into(), Value::String(format.name().into()));
        header.insert("audio".into(), Value::String(SampleFormat::S16Interleaved.name().into()));
        let (first, rate) = (timing.first, timing.rate);
        Ok(Frames {
            clip,
            header,
            form,
            sectors,
            at: 0,
            first,
            rate,
            format,
            heard_since: 0,
            since: 0.0,
            sound: codecs::xa::Decoder::default(),
            heard: Vec::new(),
            coding: None,
            gathering: None,
        })
    }

    /// Take in one sector: its sound decoded, its chunk gathered; a frame, when its last chunk comes.
    fn take(&mut self, sector: &[u8], path: &str) -> Result<Option<Data>> {
        match self.form.sector(sector) {
            Sector::Audio(coding, data) => {
                let coding = codecs::xa::Coding::of(coding);
                self.coding.get_or_insert(coding);
                self.heard.extend(self.sound.sector(coding, data).unwrap_or_default());
                Ok(None)
            }
            Sector::Video(chunk, bytes) if chunk.chunks > 0 && chunk.index < chunk.chunks => {
                if self.gathering.as_ref().is_none_or(|(first, _)| first.frame != chunk.frame) {
                    self.gathering = Some((chunk, BTreeMap::new()));
                }
                let Some((first, chunks)) = &mut self.gathering else { return Ok(None) };
                chunks.insert(chunk.index, bytes.to_vec());
                if chunks.len() < usize::from(first.chunks) {
                    return Ok(None);
                }
                let first = *first;
                let mut joined: Vec<u8> = std::mem::take(chunks).into_values().flatten().collect();
                self.gathering = None;
                if first.frame_size > 0 {
                    joined.truncate(first.frame_size as usize);
                }
                let decoded = codecs::bs::decode_frame(&joined, first.width.into(), first.height.into())
                    .map_err(|e| anyhow!("{path}: frame {}: {e}", first.frame))?;
                // When it is shown, from the stream's start.
                let shown = f64::from(first.frame.saturating_sub(self.first)) / self.rate;
                #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "microseconds of a movie")]
                let timestamp = (shown * 1e6) as u64;
                let bytes = match self.format {
                    PixelFormat::Yuv420p => [decoded.y, decoded.cb, decoded.cr].concat(),
                    _ => decoded.rgb.as_chunks::<3>().0.iter().flat_map(|&[r, g, b]| [r, g, b, 255]).collect(),
                };
                let (width, height) = (u32::from(first.width), u32::from(first.height));
                let picture = VideoFrame { buffer: MediaBuffer::new(bytes), format: self.format, width, height, timestamp };
                let mut items = vec![Data::Video(picture)];
                if let Some(coding) = self.coding {
                    let channels = if coding.stereo { 2 } else { 1 };
                    let samples = std::mem::take(&mut self.heard);
                    #[expect(clippy::cast_precision_loss, reason = "samples of a movie")]
                    let begins = self.since + self.heard_since as f64 / f64::from(coding.rate);
                    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "microseconds of a movie")]
                    let timestamp = (begins * 1e6) as u64;
                    self.heard_since += samples.len() as u64 / channels;
                    let bytes = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
                    #[expect(clippy::cast_possible_truncation, reason = "one or two channels")]
                    let channels = channels as u32;
                    let sound = AudioData {
                        buffer: MediaBuffer::new(bytes),
                        format: SampleFormat::S16Interleaved,
                        sample_rate: coding.rate,
                        channels,
                        timestamp,
                    };
                    items.push(Data::Audio(sound));
                }
                Ok(Some(Data::List(items)))
            }
            _ => Ok(None),
        }
    }
}

impl Generator for Frames {
    fn header(&self) -> Table {
        self.header.clone()
    }

    fn next(&mut self, call: &Call<'_>, count: usize) -> Result<Vec<Data>> {
        let clip = self.clip.clone();
        let lent = call.lend(&clip);
        let asset = lent.asset();
        let path = asset.path().to_string();
        let mut items = Vec::new();
        while items.len() < count && self.at < self.sectors {
            let read = RANGE.min(self.sectors - self.at);
            let bytes = read_sectors(&asset, self.form, self.at, read)?;
            for sector in bytes.chunks_exact(self.form.size()) {
                self.at += 1;
                if let Some(item) = self.take(sector, &path)? {
                    items.push(item);
                    if items.len() == count {
                        break;
                    }
                }
            }
        }
        Ok(items)
    }

    fn seek(&mut self, call: &Call<'_>, position: u64) -> Result<bool> {
        let lent = call.lend(&self.clip);
        let asset = lent.asset();
        let number = u32::try_from(position).ok().and_then(|p| p.checked_add(self.first)).ok_or_else(|| anyhow!("no frame {position}"))?;
        // The first sector whose next video chunk is of this frame or after.
        let (mut low, mut high) = (0, self.sectors);
        while low < high {
            let middle = u64::midpoint(low, high);
            match chunk_at(&asset, self.form, middle) {
                Some(chunk) if chunk.frame < number => low = middle + 1,
                _ => high = middle,
            }
        }
        self.at = low;
        self.sound = codecs::xa::Decoder::default();
        self.heard.clear();
        self.gathering = None;
        // The sound heard from here on begins where the frame is shown.
        self.since = f64::from(number - self.first) / self.rate;
        self.heard_since = 0;
        Ok(true)
    }
}
