//! A `PlayStation` disc image: one `MODE2/2352` track, its raw sectors, and the ISO 9660 file system on it.
//!
//! Every sector is 2352 bytes: 12 sync bytes, a 4-byte header, an 8-byte XA subheader, then the user data - 2048 bytes
//! in form 1 (data files, the file system), 2324 in form 2 (the streams of a `mixed` file). A mixed file's native form
//! is its sectors without sync and header, 2336 bytes each (subheader, data, error-correction tail), which is how
//! a disc dump keeps the streams' interleaving; the license data in the system area is kept the same way.

use std::fs::File;
use std::io;
use std::path::Path;

pub const RAW_SECTOR: usize = 2352;
pub const DATA: usize = 2048;
const DATA_BYTES: u32 = 2048;
/// A sector without its sync bytes and header: subheader, user data and the error-correction tail.
pub const MODE2: usize = 2336;
const FORM1_DATA_AT: usize = 24;
const MODE2_AT: usize = 16;
const PVD_SECTOR: u32 = 16;
/// The system area's sectors kept as the license data (a mkpsxiso license file is 12 of them).
pub const LICENSE_SECTORS: u32 = 12;

/// A file in the image's root directory.
#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub lba: u32,
    pub size: u32,
    pub directory: bool,
}

impl Entry {
    /// The sectors its data spans.
    #[must_use]
    pub fn sectors(&self) -> u32 {
        self.size.div_ceil(DATA_BYTES)
    }
}

/// Read `buf.len()` bytes at `offset`, without moving a cursor of the file's: an image is read by many at once.
fn read_exact_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::FileExt::read_exact_at(file, buf, offset)
    }
    #[cfg(windows)]
    {
        let mut done = 0;
        while done < buf.len() {
            match std::os::windows::fs::FileExt::seek_read(file, &mut buf[done..], offset + done as u64)? {
                0 => return Err(io::ErrorKind::UnexpectedEof.into()),
                n => done += n,
            }
        }
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        // One thread, where neither is: the file's own cursor.
        use std::io::{Read, Seek, SeekFrom};
        let mut file = file;
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(buf)
    }
}

/// A disc image, read by sector from any number of threads at once.
pub struct Image {
    file: File,
    sectors: u32,
}

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

impl Image {
    /// # Errors
    /// The file cannot be read, or is not a whole number of raw sectors.
    pub fn open(path: &Path) -> io::Result<Image> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        if len % RAW_SECTOR as u64 != 0 {
            return Err(invalid(format!("{}: not a whole number of 2352-byte sectors", path.display())));
        }
        let sectors = u32::try_from(len / RAW_SECTOR as u64)
            .map_err(|_| invalid(format!("{}: too many sectors for a disc", path.display())))?;
        Ok(Image { file, sectors })
    }

    #[must_use]
    pub fn sector_count(&self) -> u32 {
        self.sectors
    }

    /// `count` raw sectors from `lba`.
    ///
    /// # Errors
    /// The sectors run past the image's end, or cannot be read.
    pub fn raw(&self, lba: u32, count: u32) -> io::Result<Vec<u8>> {
        if lba.checked_add(count).is_none_or(|end| end > self.sectors) {
            return Err(invalid(format!("sectors {lba}+{count} are past the image's end ({})", self.sectors)));
        }
        let mut buf = vec![0; count as usize * RAW_SECTOR];
        read_exact_at(&self.file, &mut buf, u64::from(lba) * RAW_SECTOR as u64)?;
        Ok(buf)
    }

    /// `count` sectors' form 1 user data.
    ///
    /// # Errors
    /// As [`Image::raw`].
    pub fn data(&self, lba: u32, count: u32) -> io::Result<Vec<u8>> {
        let raw = self.raw(lba, count)?;
        let (sectors, _) = raw.as_chunks::<RAW_SECTOR>();
        Ok(sectors.iter().flat_map(|s| &s[FORM1_DATA_AT..FORM1_DATA_AT + DATA]).copied().collect())
    }

    /// `count` sectors without sync and header, 2336 bytes each.
    ///
    /// # Errors
    /// As [`Image::raw`].
    pub fn mode2(&self, lba: u32, count: u32) -> io::Result<Vec<u8>> {
        let raw = self.raw(lba, count)?;
        let (sectors, _) = raw.as_chunks::<RAW_SECTOR>();
        Ok(sectors.iter().flat_map(|s| &s[MODE2_AT..]).copied().collect())
    }

    /// The license data: the system area's first sectors, as `mode2` keeps them.
    ///
    /// # Errors
    /// As [`Image::raw`].
    pub fn license(&self) -> io::Result<Vec<u8>> {
        self.mode2(0, LICENSE_SECTORS)
    }

    /// The root directory's files and directories, in the order the directory records them.
    ///
    /// # Errors
    /// The image has no ISO 9660 volume descriptor or root directory, or cannot be read.
    pub fn root(&self) -> io::Result<Vec<Entry>> {
        let pvd = self.data(PVD_SECTOR, 1)?;
        if pvd[0] != 1 || &pvd[1..6] != b"CD001" {
            return Err(invalid("no primary volume descriptor at sector 16".into()));
        }
        let root = record(&pvd[156..190]).ok_or_else(|| invalid("the root directory record is empty".into()))?;
        let dir = self.data(root.lba, root.sectors())?;
        let mut out = Vec::new();
        for sector in dir[..root.size as usize].chunks(DATA) {
            let mut at = 0;
            while at < sector.len() && sector[at] != 0 {
                let len = sector[at] as usize;
                if let Some(e) = record(&sector[at..at + len])
                    && e.name != "\0"
                    && e.name != "\u{1}"
                {
                    out.push(e);
                }
                at += len;
            }
        }
        Ok(out)
    }
}

/// A directory record: extent, size, flags and name (its `;1` version dropped).
fn record(r: &[u8]) -> Option<Entry> {
    if r.len() < 34 || r[0] == 0 {
        return None;
    }
    let lba = u32::from_le_bytes(r[2..6].try_into().ok()?);
    let size = u32::from_le_bytes(r[10..14].try_into().ok()?);
    let name_len = r[32] as usize;
    let raw = &r[33..33 + name_len];
    let name = String::from_utf8_lossy(raw);
    let name = name.split(';').next().unwrap_or_default().to_string();
    Some(Entry { name, lba, size, directory: r[25] & 2 != 0 })
}
