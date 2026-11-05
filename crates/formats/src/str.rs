//! STR: the `PlayStation`'s streamed movies, MDEC video interleaved with XA audio, sector by sector.
//!
//! The INTER files' sectors are 2336 bytes - mode 2's 8-byte XA subheader (file, channel, submode, coding, twice),
//! 2048 bytes of data and the rest error correction; the stage folders' movies are 2048-byte sectors of data alone,
//! video only. A video sector's data is a 32-byte header - magic `0x80010160`, the chunk's index and the frame's
//! count of chunks, the frame's number and size in bytes, its width and height, then the frame's own BS header - and
//! 2016 bytes of the frame; a frame is its chunks joined, cut to its size. An audio sector (submode bit 2) holds XA
//! ADPCM in its data's first 2304 bytes. (After `tools/peassets/str_codec.py`.)

use std::collections::BTreeMap;

/// An INTER file's sector: subheader, data, error correction.
pub const SECTOR: usize = 2336;
/// Streams are read at the drive's double speed: 150 sectors a second.
pub const SECTORS_A_SECOND: u64 = 150;
/// A sector's data.
pub const DATA: usize = 2048;
/// The XA subheader before a raw sector's data.
pub const SUBHEADER: usize = 8;
/// A video sector's magic.
pub const MAGIC: u32 = 0x8001_0160;
/// A video sector's header, before its chunk of the frame.
pub const HEADER: usize = 32;
/// A video sector's chunk of its frame.
pub const CHUNK: usize = DATA - HEADER;

/// A raw sector's XA subheader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Subheader {
    pub file: u8,
    pub channel: u8,
    pub submode: u8,
    pub coding: u8,
}

impl Subheader {
    #[must_use]
    pub fn of(sector: &[u8]) -> Option<Subheader> {
        let b = sector.get(..4)?;
        Some(Subheader { file: b[0], channel: b[1], submode: b[2], coding: b[3] })
    }

    /// Audio (submode bit 2).
    #[must_use]
    pub fn audio(self) -> bool {
        self.submode & 0x04 != 0
    }
}

/// A video sector's header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chunk {
    pub index: u16,
    pub chunks: u16,
    pub frame: u32,
    pub frame_size: u32,
    pub width: u16,
    pub height: u16,
}

impl Chunk {
    /// The header of a sector's data, where it is a video sector's.
    #[must_use]
    pub fn of(data: &[u8]) -> Option<Chunk> {
        let u16_at = |at: usize| data.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
        let u32_at = |at: usize| data.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
        if data.len() < HEADER || u32_at(0)? != MAGIC {
            return None;
        }
        Some(Chunk {
            index: u16_at(4)?,
            chunks: u16_at(6)?,
            frame: u32_at(8)?,
            frame_size: u32_at(12)?,
            width: u16_at(16)?,
            height: u16_at(18)?,
        })
    }
}

/// How a sector's audio is coded: its subheader's coding byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Coding {
    pub stereo: bool,
    /// Samples a second, a channel: 37800 or 18900.
    pub rate: u32,
    /// 4 bits a sample; else 8, which PE2 does not use.
    pub four_bit: bool,
}

impl Coding {
    #[must_use]
    pub fn of(byte: u8) -> Coding {
        Coding {
            stereo: byte & 3 == 1,
            rate: if (byte >> 2) & 3 == 1 { 18900 } else { 37800 },
            four_bit: (byte >> 4) & 3 != 1,
        }
    }
}

/// One sector of a stream, as read.
#[derive(Clone, Copy, Debug)]
pub enum Sector<'a> {
    /// A chunk of a frame: its header and its bytes of the frame.
    Video(Chunk, &'a [u8]),
    /// XA audio: its coding byte and its sound data.
    Audio(u8, &'a [u8]),
    /// Neither: padding, an end.
    Other,
}

impl Sector<'_> {
    /// A raw 2336-byte sector of an INTER file.
    #[must_use]
    pub fn raw(sector: &[u8]) -> Sector<'_> {
        let data = sector.get(SUBHEADER..).unwrap_or_default();
        if let Some(chunk) = Chunk::of(data) {
            return Sector::Video(chunk, &data[HEADER..DATA.min(data.len())]);
        }
        match Subheader::of(sector) {
            Some(sub) if sub.audio() && data.len() >= 2304 => Sector::Audio(sub.coding, &data[..2304]),
            _ => Sector::Other,
        }
    }

    /// A 2048-byte data sector of a stage folder's movie: video, or nothing.
    #[must_use]
    pub fn data(sector: &[u8]) -> Sector<'_> {
        match Chunk::of(sector) {
            Some(chunk) => Sector::Video(chunk, &sector[HEADER..DATA.min(sector.len())]),
            None => Sector::Other,
        }
    }
}

/// How a stream's sectors are stored: raw, a disc's movie file's (2336 bytes, video and audio), or data, a movie
/// stored in a stage (2048 bytes, video alone). A data sector starts with the video magic, a raw one with its
/// subheader, so a stream's first sector tells which.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    Raw,
    Data,
}

impl Form {
    /// The form of a stream, from its first bytes.
    #[must_use]
    pub fn of(first: &[u8]) -> Form {
        if Chunk::of(first).is_some() { Form::Data } else { Form::Raw }
    }

    /// The bytes of one of its sectors.
    #[must_use]
    pub const fn size(self) -> usize {
        match self {
            Form::Raw => SECTOR,
            Form::Data => DATA,
        }
    }

    /// One of its sectors, as read.
    #[must_use]
    pub fn sector(self, sector: &[u8]) -> Sector<'_> {
        match self {
            Form::Raw => Sector::raw(sector),
            Form::Data => Sector::data(sector),
        }
    }
}

/// A frame of a stream, its chunks joined.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub number: u32,
    pub width: u16,
    pub height: u16,
    /// The frame as BS: its chunks joined, cut to its size.
    pub bytes: Vec<u8>,
}

/// The whole frames of a stream's sectors, by number: a frame missing a chunk, or not BS, is left out.
pub fn frames<'a>(sectors: impl IntoIterator<Item = Sector<'a>>) -> Vec<Frame> {
    struct Gathering<'a> {
        first: Chunk,
        chunks: BTreeMap<u16, &'a [u8]>,
    }
    let mut gathering: BTreeMap<u32, Gathering<'a>> = BTreeMap::new();
    for sector in sectors {
        let Sector::Video(chunk, bytes) = sector else { continue };
        if chunk.chunks == 0 || chunk.index >= chunk.chunks {
            continue;
        }
        let frame = gathering.entry(chunk.frame).or_insert_with(|| Gathering { first: chunk, chunks: BTreeMap::new() });
        frame.chunks.insert(chunk.index, bytes);
    }
    gathering
        .into_iter()
        .filter_map(|(number, frame)| {
            let first = frame.first;
            if (0..first.chunks).any(|i| !frame.chunks.contains_key(&i)) {
                return None;
            }
            let mut bytes: Vec<u8> = (0..first.chunks).flat_map(|i| frame.chunks[&i].iter().copied()).collect();
            if first.frame_size > 0 {
                bytes.truncate(first.frame_size as usize);
            }
            let bs = bytes.len() >= 8 && u16::from_le_bytes([bytes[2], bytes[3]]) == 0x3800;
            bs.then_some(Frame { number, width: first.width, height: first.height, bytes })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{CHUNK, Chunk, DATA, Form, MAGIC, SECTOR, SUBHEADER, Sector, frames};

    /// A raw video sector: chunk `index` of `chunks` of frame `frame`, filled with `fill`.
    fn video(frame: u32, index: u16, chunks: u16, size: u32, fill: u8) -> Vec<u8> {
        let mut s = vec![0u8; SECTOR];
        s[2] = 0x48;
        let d = &mut s[SUBHEADER..];
        d[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        d[4..6].copy_from_slice(&index.to_le_bytes());
        d[6..8].copy_from_slice(&chunks.to_le_bytes());
        d[8..12].copy_from_slice(&frame.to_le_bytes());
        d[12..16].copy_from_slice(&size.to_le_bytes());
        d[16..18].copy_from_slice(&320u16.to_le_bytes());
        d[18..20].copy_from_slice(&192u16.to_le_bytes());
        d[32..DATA].fill(fill);
        // The frame's BS header, in its first chunk.
        if index == 0 {
            d[32 + 2..32 + 4].copy_from_slice(&0x3800u16.to_le_bytes());
        }
        s
    }

    #[test]
    fn a_stream_says_its_form() {
        let raw = video(1, 0, 1, 4, 0);
        assert_eq!(Form::of(&raw), Form::Raw);
        assert_eq!(Form::of(&raw[SUBHEADER..]), Form::Data);
        assert!(matches!(
            Form::Data.sector(&raw[SUBHEADER..SUBHEADER + DATA]),
            Sector::Video(Chunk { frame: 1, .. }, _)
        ));
    }

    #[test]
    fn frames_are_their_chunks_joined_and_cut() {
        let mut audio = vec![0u8; SECTOR];
        audio[2] = 0x64;
        audio[3] = 0x01;
        let size = u32::try_from(CHUNK + 100).unwrap();
        // Frame 1 whole (its chunks out of order, audio between), frame 2 missing a chunk.
        let sectors = [video(1, 1, 2, size, 2), audio.clone(), video(1, 0, 2, size, 1), video(2, 0, 2, size, 3)];
        let read: Vec<Sector<'_>> = sectors.iter().map(|s| Sector::raw(s)).collect();
        assert!(matches!(read[1], Sector::Audio(0x01, data) if data.len() == 2304));
        let frames = frames(read);
        assert_eq!(frames.len(), 1);
        let frame = &frames[0];
        assert_eq!((frame.number, frame.width, frame.height, frame.bytes.len()), (1, 320, 192, CHUNK + 100));
        assert_eq!((frame.bytes[4], frame.bytes[CHUNK]), (1, 2));
    }
}
