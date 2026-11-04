//! The stage containers: STAGE0.HED with STAGE0.CDF, and STAGE1..5.CDF.
//!
//! STAGE0.HED holds three stream descriptors, then the stage 0 files as (id, sector) pairs into STAGE0.CDF, ended by
//! `0xFFFFFFFF`. A STAGE<N>.CDF opens with 256 folders as (id, sectors); the folders follow, from sector 1, each
//! beginning with a header sector of 162 files as (id, sector from the folder's start) and its stream descriptors.
//! A file is a chain of chunks, each a 16-byte header - type, end flag (`0xFF` on the last), the bytes of each sector
//! that hold data (`sector_len`), the chunk's sectors, a load address, zero - and its sectors. A chunk's payload, its
//! native form, is its sectors' data without the header and without each sector's bytes past `sector_len`.
//!
//! `pack_chunk` makes a chunk again, and makes every chunk of both discs (`examples/cdf_identity.rs`): the header and
//! payload laid `sector_len` bytes to a sector, zeros after; an LZSS chunk (package, image, palette) as few sectors
//! as hold its data and a byte more, a payload kept as stored as many as it fills. `sector_len` is the file's choice
//! (2048, or less where a file streams in while it decodes) and the load address the package's or bundle's.
//! `image_payload` lays out an image's strips as the loader takes them: each right after the one before unless two
//! strips already end in that sector - the loader uploads two a sector at most - then at the next sector's start;
//! a zero byte after a strip whose bits end on a byte boundary, or that ends a column of eight.

use std::io;

pub const SECTOR: u64 = 0x800;
const HEADER: usize = 0x10;
const HED_STREAMS: usize = 3 * 0x28;
const FOLDERS: usize = 0x100;
const FOLDER_FILES: usize = 0xA2;
const LAST_CHUNK: u8 = 0xFF;

/// Bytes read at an offset: a container held in memory, or one read sector by sector from a disc.
pub trait Source {
    /// # Errors
    /// The bytes cannot be read.
    fn read_at(&mut self, offset: u64, len: usize) -> io::Result<Vec<u8>>;
}

impl Source for &[u8] {
    fn read_at(&mut self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        let start = usize::try_from(offset).map_err(|_| invalid("offset past memory".into()))?;
        self.get(start..start + len)
            .map(<[u8]>::to_vec)
            .ok_or_else(|| invalid(format!("{len} bytes at {offset:#x} past the end")))
    }
}

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// A file's place in its container: its folder (none in stage 0), its id, and its byte offset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileEntry {
    pub folder: Option<u32>,
    pub id: u32,
    pub offset: u64,
}

/// Stage 0's files, from STAGE0.HED, in table order.
///
/// # Errors
/// The table has no terminator.
pub fn stage0_files(hed: &[u8]) -> io::Result<Vec<FileEntry>> {
    let mut out = Vec::new();
    let mut at = HED_STREAMS;
    loop {
        if at + 8 > hed.len() {
            return Err(invalid("STAGE0.HED's file table has no terminator".into()));
        }
        let id = u32_at(hed, at);
        if id == u32::MAX {
            return Ok(out);
        }
        out.push(FileEntry { folder: None, id, offset: u64::from(u32_at(hed, at + 4)) * SECTOR });
        at += 8;
    }
}

/// A STAGE<N>.CDF's files, folder by folder, in table order.
///
/// # Errors
/// The tables cannot be read.
pub fn stage_files(cdf: &mut impl Source) -> io::Result<Vec<FileEntry>> {
    let folders = cdf.read_at(0, FOLDERS * 8)?;
    let (mut out, mut base) = (Vec::new(), SECTOR);
    for f in 0..FOLDERS {
        let (id, sectors) = (u32_at(&folders, f * 8), u32_at(&folders, f * 8 + 4));
        if id == 0 && sectors == 0 {
            continue;
        }
        let table = cdf.read_at(base, FOLDER_FILES * 8)?;
        for k in 0..FOLDER_FILES {
            let (file, sector) = (u32_at(&table, k * 8), u32_at(&table, k * 8 + 4));
            if file != 0 || sector != 0 {
                out.push(FileEntry { folder: Some(id), id: file, offset: base + u64::from(sector) * SECTOR });
            }
        }
        base += u64::from(sectors) * SECTOR;
    }
    Ok(out)
}

/// One chunk of a file.
#[derive(Clone, Copy, Debug)]
pub struct Chunk {
    pub kind: u8,
    pub sector_len: u16,
    pub load: u32,
    pub offset: u64,
    pub sectors: u32,
}

/// The slot type a chunk's type stands for.
#[must_use]
pub fn slot_type(kind: u8) -> Option<&'static str> {
    Some(match kind {
        0 => "package",
        1 => "image",
        2 => "palette",
        4 => "bundle",
        5 => "bs",
        6 => "sound_bank",
        7 => "text",
        _ => return None,
    })
}

/// A file's chunks, from its offset.
///
/// # Errors
/// A chunk header is not one, or cannot be read.
pub fn chunks(src: &mut impl Source, mut at: u64) -> io::Result<Vec<Chunk>> {
    let mut out = Vec::new();
    loop {
        let h = src.read_at(at, HEADER)?;
        let (kind, end, sector_len, sectors) = (h[0], h[1], u16_at(&h, 2), u32_at(&h, 4));
        if slot_type(kind).is_none() || u32_at(&h, 0xC) != 0 || sectors == 0 || !(17..=0x800).contains(&sector_len) {
            return Err(invalid(format!("no chunk header at {at:#x}")));
        }
        out.push(Chunk { kind, sector_len, load: u32_at(&h, 8), offset: at, sectors });
        at += u64::from(sectors) * SECTOR;
        if end == LAST_CHUNK {
            return Ok(out);
        }
    }
}

impl Chunk {
    /// The chunk's payload, its native form.
    ///
    /// # Errors
    /// Its sectors cannot be read.
    pub fn payload(&self, src: &mut impl Source) -> io::Result<Vec<u8>> {
        let len =
            usize::try_from(u64::from(self.sectors) * SECTOR).map_err(|_| invalid("a chunk past memory".into()))?;
        let bytes = src.read_at(self.offset, len)?;
        let sector = usize::try_from(SECTOR).unwrap_or(0x800);
        let used = usize::from(self.sector_len);
        Ok(bytes
            .chunks(sector)
            .enumerate()
            .flat_map(|(i, s)| &s[if i == 0 { HEADER } else { 0 }..used])
            .copied()
            .collect())
    }
}

/// A stream record as stored, 0x28 bytes (the game's `StreamSlot`): a movie (`kind` 1) or a scene's audio (2), its
/// key, where its data is, and what plays it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StreamRecord {
    pub kind: u16,
    /// A movie's HED marker in bit 15 (stage 0); a scene's volume (low byte) and timing buffer kind (high byte).
    pub control: u16,
    /// Sectors from its container's start: a stored stream's data; an INTER movie's, as an earlier layout had it.
    pub start: u32,
    /// A movie: sectors into its disc's INTER file. A scene: its initial decode buffer in bytes.
    pub source: u32,
    /// A movie: the room state (0 any). A scene: its exact group.
    pub group: u16,
    /// A movie: the view it plays at, or 100 and up a full-screen one. A scene: its id.
    pub id: u16,
    pub sub_id: u16,
    /// A movie: width, height, VRAM x and y, frame limit, loop mode, view stream, an unproven halfword, display mode,
    /// volume, upload mode. A scene: second sub id, resume sector offset, sound bank mask, VLC table mode, VLC buffer
    /// kind, timing bytes, timing buffer bytes, eight unproven bytes.
    pub data: [u16; 11],
}

pub const STREAM_RECORD: usize = 0x28;
/// Where a folder's stream records are in its header sector, and how many.
const FOLDER_STREAMS: usize = 0x514;
const FOLDER_STREAM_COUNT: usize = 0x12;

impl StreamRecord {
    #[must_use]
    pub fn read(b: &[u8; STREAM_RECORD]) -> StreamRecord {
        let mut data = [0u16; 11];
        for (k, d) in data.iter_mut().enumerate() {
            *d = u16_at(b, 0x12 + 2 * k);
        }
        StreamRecord {
            kind: u16_at(b, 0),
            control: u16_at(b, 2),
            start: u32_at(b, 4),
            source: u32_at(b, 8),
            group: u16_at(b, 0xC),
            id: u16_at(b, 0xE),
            sub_id: u16_at(b, 0x10),
            data,
        }
    }

    #[must_use]
    pub fn bytes(&self) -> [u8; STREAM_RECORD] {
        let mut b = [0u8; STREAM_RECORD];
        b[0..2].copy_from_slice(&self.kind.to_le_bytes());
        b[2..4].copy_from_slice(&self.control.to_le_bytes());
        b[4..8].copy_from_slice(&self.start.to_le_bytes());
        b[8..12].copy_from_slice(&self.source.to_le_bytes());
        b[0xC..0xE].copy_from_slice(&self.group.to_le_bytes());
        b[0xE..0x10].copy_from_slice(&self.id.to_le_bytes());
        b[0x10..0x12].copy_from_slice(&self.sub_id.to_le_bytes());
        for (k, d) in self.data.iter().enumerate() {
            b[0x12 + 2 * k..0x14 + 2 * k].copy_from_slice(&d.to_le_bytes());
        }
        b
    }

    /// An empty place in a table.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == StreamRecord::default()
    }
}

fn records(bytes: &[u8], count: usize) -> Vec<StreamRecord> {
    bytes.as_chunks::<STREAM_RECORD>().0.iter().take(count).map(StreamRecord::read).collect()
}

/// Stage 0's three stream records, at the start of STAGE0.HED.
///
/// # Errors
/// The table is short.
pub fn stage0_streams(hed: &[u8]) -> io::Result<Vec<StreamRecord>> {
    let table = hed.get(..HED_STREAMS).ok_or_else(|| invalid("STAGE0.HED is shorter than its stream records".into()))?;
    Ok(records(table, HED_STREAMS / STREAM_RECORD))
}

/// A folder of a STAGE<N>.CDF: its id, where it starts and how long it is, in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Folder {
    pub id: u32,
    pub offset: u64,
    pub size: u64,
}

/// A STAGE<N>.CDF's folders, in table order.
///
/// # Errors
/// The table cannot be read.
pub fn stage_folders(cdf: &mut impl Source) -> io::Result<Vec<Folder>> {
    let table = cdf.read_at(0, FOLDERS * 8)?;
    let (mut out, mut base) = (Vec::new(), SECTOR);
    for f in 0..FOLDERS {
        let (id, sectors) = (u32_at(&table, f * 8), u32_at(&table, f * 8 + 4));
        if id == 0 && sectors == 0 {
            continue;
        }
        let size = u64::from(sectors) * SECTOR;
        out.push(Folder { id, offset: base, size });
        base += size;
    }
    Ok(out)
}

/// A folder's 18 stream records, from its header sector.
///
/// # Errors
/// The header cannot be read.
pub fn folder_streams(cdf: &mut impl Source, folder: &Folder) -> io::Result<Vec<StreamRecord>> {
    let table = cdf.read_at(folder.offset + FOLDER_STREAMS as u64, FOLDER_STREAM_COUNT * STREAM_RECORD)?;
    Ok(records(&table, FOLDER_STREAM_COUNT))
}

/// Where a file of a container is: bytes of its own, or the place of an earlier file of the same container (one file
/// under two ids).
#[derive(Clone, Copy, Debug)]
pub enum Place<'a> {
    Own(&'a [u8]),
    Shares(u32),
}

/// Files laid back to back in the order given, a file that shares an earlier one's place taking it: the bytes, and
/// each file's offset from their start.
///
/// # Panics
/// A file shares the place of one not before it.
fn lay_out(files: &[(u32, Place<'_>)]) -> (Vec<u8>, Vec<(u32, u64)>) {
    let (mut out, mut places): (Vec<u8>, Vec<(u32, u64)>) = (Vec::new(), Vec::new());
    for &(id, place) in files {
        let at = match place {
            Place::Own(bytes) => {
                let at = out.len() as u64;
                out.extend_from_slice(bytes);
                at
            }
            Place::Shares(other) => places.iter().find(|p| p.0 == other).map(|p| p.1).expect("a file it shares the place of"),
        };
        places.push((id, at));
    }
    (out, places)
}

fn sectors(bytes: u64) -> u32 {
    u32::try_from(bytes / SECTOR).unwrap_or(u32::MAX)
}

/// A folder: its header sector - the files' (id, sector) list, a zero word, its 18 stream records - then its files in
/// the order given (each a whole number of sectors; one under two ids once), then `tail`, its stored streams' data.
///
/// # Panics
/// More files or stream records than a folder holds.
#[must_use]
pub fn pack_folder(files: &[(u32, Place<'_>)], streams: &[StreamRecord], tail: &[u8]) -> Vec<u8> {
    assert!(files.len() <= FOLDER_FILES && streams.len() <= FOLDER_STREAM_COUNT, "a folder holds 162 files, 18 streams");
    let (body, places) = lay_out(files);
    let mut out = vec![0u8; usize::try_from(SECTOR).unwrap_or(0x800)];
    for (k, (id, at)) in places.iter().enumerate() {
        out[k * 8..k * 8 + 4].copy_from_slice(&id.to_le_bytes());
        out[k * 8 + 4..k * 8 + 8].copy_from_slice(&(sectors(*at) + 1).to_le_bytes());
    }
    for (k, record) in streams.iter().enumerate() {
        out[FOLDER_STREAMS + k * STREAM_RECORD..FOLDER_STREAMS + (k + 1) * STREAM_RECORD].copy_from_slice(&record.bytes());
    }
    out.extend_from_slice(&body);
    out.extend_from_slice(tail);
    out
}

/// A STAGE<N>.CDF: its folder table - (id, sectors) - then its folders in the order given.
///
/// # Panics
/// More folders than a table holds.
#[must_use]
pub fn pack_stage(folders: &[(u32, &[u8])]) -> Vec<u8> {
    assert!(folders.len() <= FOLDERS, "a stage holds 256 folders");
    let mut out = vec![0u8; usize::try_from(SECTOR).unwrap_or(0x800)];
    for (k, (id, folder)) in folders.iter().enumerate() {
        out[k * 8..k * 8 + 4].copy_from_slice(&id.to_le_bytes());
        out[k * 8 + 4..k * 8 + 8].copy_from_slice(&sectors(folder.len() as u64).to_le_bytes());
    }
    for (_, folder) in folders {
        out.extend_from_slice(folder);
    }
    out
}

/// Stage 0's STAGE0.HED - its three stream records, then each file's (id, sector), ended by eight bytes of `0xFF` - and
/// STAGE0.CDF, its files in the order given.
#[must_use]
pub fn pack_stage0(files: &[(u32, Place<'_>)], streams: &[StreamRecord]) -> (Vec<u8>, Vec<u8>) {
    let (cdf, places) = lay_out(files);
    let mut hed = Vec::with_capacity(HED_STREAMS + 8 * (places.len() + 1));
    for k in 0..HED_STREAMS / STREAM_RECORD {
        hed.extend_from_slice(&streams.get(k).copied().unwrap_or_default().bytes());
    }
    for (id, at) in places {
        hed.extend_from_slice(&id.to_le_bytes());
        hed.extend_from_slice(&sectors(at).to_le_bytes());
    }
    hed.extend_from_slice(&[0xFF; 8]);
    (hed, cdf)
}

/// The chunk types whose payload is LZSS: a package, an image's strips, a palette's colors.
fn compressed(kind: u8) -> bool {
    matches!(kind, 0..=2)
}

/// A chunk of `kind` holding `payload`, the last of its file or not, `sector_len` data bytes to a sector.
///
/// # Panics
/// `sector_len` is more than a sector, or less than a header.
#[must_use]
pub fn pack_chunk(kind: u8, last: bool, sector_len: u16, load: u32, payload: &[u8]) -> Vec<u8> {
    let used = usize::from(sector_len);
    let sector = usize::try_from(SECTOR).unwrap_or(0x800);
    assert!((HEADER..=sector).contains(&used), "a sector length of {used}");
    let mut data = vec![0u8; HEADER];
    data.extend_from_slice(payload);
    let sectors = if compressed(kind) { data.len() / used + 1 } else { data.len().div_ceil(used).max(1) };
    data.resize(sectors * used, 0);
    data[0] = kind;
    data[1] = if last { LAST_CHUNK } else { 1 };
    data[2..4].copy_from_slice(&sector_len.to_le_bytes());
    data[4..8].copy_from_slice(&u32::try_from(sectors).unwrap_or(u32::MAX).to_le_bytes());
    data[8..12].copy_from_slice(&load.to_le_bytes());
    let mut out = Vec::with_capacity(sectors * sector);
    for s in data.chunks(used) {
        out.extend_from_slice(s);
        out.resize(out.len() + sector - used, 0);
    }
    out
}

/// How many bits an LZSS stream takes through its end token: 9 a literal, 13 a reference, 9 the end.
fn stream_bits(stream: &[u8]) -> usize {
    let (mut bits, mut at) = (0usize, 0usize);
    let bit = |at: usize| stream.get(at / 8).map(|b| (b >> (7 - at % 8)) & 1);
    loop {
        match bit(at) {
            Some(1) => (bits, at) = (bits + 9, at + 9),
            Some(_) => {
                let offset = (1..=8).try_fold(0u32, |x, k| bit(at + k).map(|b| (x << 1) | u32::from(b)));
                bits += 9;
                if offset.is_none_or(|o| o == 0) {
                    return bits;
                }
                (bits, at) = (bits + 4, at + 13);
            }
            None => return bits,
        }
    }
}

/// An image chunk's payload: its column table (through the first strip's offset), then each strip encoded.
#[must_use]
pub fn image_payload(table: &[u8], strips: &[Vec<u8>], sector_len: u16) -> Vec<u8> {
    let used = usize::from(sector_len);
    let sector_of = |x: usize| (HEADER + x) / used;
    let mut out = table.to_vec();
    let mut ends: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    for (index, strip) in strips.iter().enumerate() {
        if index > 0 && ends.get(&sector_of(out.len())).copied().unwrap_or(0) >= 2 {
            out.resize((sector_of(out.len()) + 1) * used - HEADER, 0);
        }
        let stream = crate::lzss::encode(strip);
        let pad = stream_bits(&stream) % 8 == 0 || (index + 1) % 8 == 0;
        out.extend_from_slice(&stream);
        if pad {
            out.push(0);
        }
        *ends.entry(sector_of(out.len() - 1)).or_default() += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_drops_header_and_padding() {
        let mut file = vec![0u8; 0x1000];
        file[0] = 5;
        file[1] = LAST_CHUNK;
        file[2..4].copy_from_slice(&0x600u16.to_le_bytes());
        file[4..8].copy_from_slice(&2u32.to_le_bytes());
        file[0x10..0x600].fill(1);
        file[0x800..0xE00].fill(2);
        file[0xE00..].fill(9); // past sector_len: padding
        let mut src: &[u8] = &file;
        let chunks = chunks(&mut src, 0).unwrap();
        assert_eq!(chunks.len(), 1);
        let payload = chunks[0].payload(&mut src).unwrap();
        assert_eq!(payload.len(), 0x5F0 + 0x600);
        assert!(payload[..0x5F0].iter().all(|&b| b == 1) && payload[0x5F0..].iter().all(|&b| b == 2));
    }
    #[test]
    fn a_packed_chunk_reads_back() {
        let payload: Vec<u8> = (0..3000u32).map(|i| (i % 251).to_le_bytes()[0]).collect();
        let chunk = pack_chunk(7, true, 0x600, 0, &payload);
        let mut src: &[u8] = &chunk;
        let found = chunks(&mut src, 0).unwrap();
        assert_eq!((found.len(), found[0].kind, found[0].sector_len), (1, 7, 0x600));
        let read = found[0].payload(&mut src).unwrap();
        assert_eq!(&read[..payload.len()], &payload[..]);
        assert!(read[payload.len()..].iter().all(|&b| b == 0));
    }

    #[test]
    fn a_compressed_chunk_keeps_a_byte_of_room() {
        // 16 + 2032 bytes fill one sector exactly: a compressed chunk takes a second, a stored one does not.
        assert_eq!(pack_chunk(0, true, 0x800, 0, &[1; 0x7F0]).len(), 2 * 0x800);
        assert_eq!(pack_chunk(5, true, 0x800, 0, &[1; 0x7F0]).len(), 0x800);
    }

    #[test]
    fn image_strips_decode_back_in_order() {
        let strips: Vec<Vec<u8>> = (0..10u8).map(|k| (0..0x1000u32).map(|i| i.to_le_bytes()[0].wrapping_mul(k)).collect()).collect();
        let table = [0u8; 0x20];
        let payload = image_payload(&table, &strips, 0x800);
        assert_eq!(crate::lzss::decode_streams(&payload, table.len()), strips);
    }

    #[test]
    fn a_stream_record_reads_back() {
        let mut b = [0u8; STREAM_RECORD];
        for (i, x) in b.iter_mut().enumerate() {
            *x = u8::try_from(i * 7 % 256).unwrap();
        }
        assert_eq!(StreamRecord::read(&b).bytes(), b);
        assert!(StreamRecord::read(&[0; STREAM_RECORD]).is_empty());
    }
}
