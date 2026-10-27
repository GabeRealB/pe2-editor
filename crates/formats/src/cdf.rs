//! The stage containers: STAGE0.HED with STAGE0.CDF, and STAGE1..5.CDF.
//!
//! STAGE0.HED holds three stream descriptors, then the stage 0 files as (id, sector) pairs into STAGE0.CDF, ended by
//! `0xFFFFFFFF`. A STAGE<N>.CDF opens with 256 folders as (id, sectors); the folders follow, from sector 1, each
//! beginning with a header sector of 162 files as (id, sector from the folder's start) and its stream descriptors.
//! A file is a chain of chunks, each a 16-byte header - type, end flag (`0xFF` on the last), the bytes of each sector
//! that hold data (`sector_len`), the chunk's sectors, a load address, zero - and its sectors. A chunk's payload, its
//! native form, is its sectors' data without the header and without each sector's bytes past `sector_len`.

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
}
