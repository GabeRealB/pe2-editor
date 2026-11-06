//! Zip archives, as far as `OpenRaster` needs them: written deterministically (fixed times, entries in the order given,
//! each stored or deflated), read whatever wrote them (stored or deflated entries, data descriptors) short of Zip64.

use std::fmt;

/// Why an archive could not be read.
#[derive(Debug)]
pub struct Error(String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

fn error<T>(message: impl Into<String>) -> Result<T, Error> {
    Err(Error(message.into()))
}

const LOCAL: u32 = 0x0403_4B50;
const CENTRAL: u32 = 0x0201_4B50;
const END: u32 = 0x0605_4B50;
/// 1980-01-01 00:00, the earliest time a zip can say: an archive written twice is the same bytes.
const DOS_DATE: u16 = 0x21;
/// The deflate level: what PNG writers use by default, the archive being mostly PNGs already compressed.
const LEVEL: u8 = 6;

/// An entry to write.
pub struct Entry<'a> {
    pub name: &'a str,
    pub data: &'a [u8],
    /// Deflated, or stored as it is (`OpenRaster`'s `mimetype` must be stored, and first).
    pub deflate: bool,
    /// Its Unix permissions (`0o755` for a program), which unzipping on Unix gives it back; 0 says none, and the
    /// archive is written as one made anywhere.
    pub mode: u32,
}

/// An archive of these entries, in their order.
#[must_use]
pub fn write(entries: &[Entry]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    let n = |v: usize| u32::try_from(v).unwrap_or(u32::MAX);
    for entry in entries {
        let crc = crc32fast::hash(entry.data);
        let deflated;
        let (method, body): (u16, &[u8]) = if entry.deflate {
            deflated = miniz_oxide::deflate::compress_to_vec(entry.data, LEVEL);
            (8, &deflated)
        } else {
            (0, entry.data)
        };
        let offset = n(out.len());
        let header = |out: &mut Vec<u8>| {
            put16(out, 20); // version needed: 2.0
            put16(out, 0x0800); // flags: the name is UTF-8
            put16(out, method);
            put16(out, 0); // time
            put16(out, DOS_DATE);
            put32(out, crc);
            put32(out, n(body.len()));
            put32(out, n(entry.data.len()));
            put16(out, u16::try_from(entry.name.len()).unwrap_or(u16::MAX));
            put16(out, 0); // extra
        };
        put32(&mut out, LOCAL);
        header(&mut out);
        out.extend_from_slice(entry.name.as_bytes());
        out.extend_from_slice(body);
        put32(&mut central, CENTRAL);
        // Made by: 2.0, on Unix where it says permissions, which then are the external attributes' high half.
        put16(&mut central, if entry.mode == 0 { 0x14 } else { 0x0300 | 0x14 });
        header(&mut central);
        put16(&mut central, 0); // comment
        put16(&mut central, 0); // disk
        put16(&mut central, 0); // internal attributes
        put32(&mut central, if entry.mode == 0 { 0 } else { (0o100_000 | (entry.mode & 0o7777)) << 16 });
        put32(&mut central, offset);
        central.extend_from_slice(entry.name.as_bytes());
    }
    let start = n(out.len());
    let count = u16::try_from(entries.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&central);
    put32(&mut out, END);
    put16(&mut out, 0);
    put16(&mut out, 0);
    put16(&mut out, count);
    put16(&mut out, count);
    put32(&mut out, n(central.len()));
    put32(&mut out, start);
    put16(&mut out, 0);
    out
}

fn put16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn get16(bytes: &[u8], at: usize) -> Result<u16, Error> {
    match bytes.get(at..at + 2) {
        Some(b) => Ok(u16::from_le_bytes([b[0], b[1]])),
        None => error("truncated"),
    }
}

fn get32(bytes: &[u8], at: usize) -> Result<u32, Error> {
    match bytes.get(at..at + 4) {
        Some(b) => Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]])),
        None => error("truncated"),
    }
}

/// An archive's entries, by name in their order, read through its central directory.
pub struct Archive<'a> {
    bytes: &'a [u8],
    entries: Vec<(String, Located)>,
}

#[derive(Clone, Copy)]
struct Located {
    /// Its Unix permissions, where it was made on Unix and says them; 0 otherwise.
    mode: u32,
    method: u16,
    crc: u32,
    compressed: usize,
    size: usize,
    local: usize,
}

impl<'a> Archive<'a> {
    /// Read an archive's directory.
    ///
    /// # Errors
    /// It is not a zip archive this reads: no end record, Zip64, an entry out of bounds.
    pub fn new(bytes: &'a [u8]) -> Result<Archive<'a>, Error> {
        // The end record, its comment of up to 64 KiB after it.
        let lowest = bytes.len().saturating_sub(22 + 0xFFFF);
        let Some(end) = (lowest..=bytes.len().saturating_sub(22)).rev().find(|&i| get32(bytes, i).ok() == Some(END))
        else {
            return error("not a zip archive: no end of central directory");
        };
        let count = usize::from(get16(bytes, end + 10)?);
        let mut at = get32(bytes, end + 16)? as usize;
        if at == 0xFFFF_FFFF || count == 0xFFFF {
            return error("a Zip64 archive, which is not read");
        }
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            if get32(bytes, at)? != CENTRAL {
                return error("a broken central directory");
            }
            // Made on Unix (the high byte 3), the external attributes' high half is the file's mode.
            let unix = get16(bytes, at + 4)? >> 8 == 3;
            let mode = if unix { (get32(bytes, at + 38)? >> 16) & 0o7777 } else { 0 };
            let located = Located {
                mode,
                method: get16(bytes, at + 10)?,
                crc: get32(bytes, at + 16)?,
                compressed: get32(bytes, at + 20)? as usize,
                size: get32(bytes, at + 24)? as usize,
                local: get32(bytes, at + 42)? as usize,
            };
            let name_len = usize::from(get16(bytes, at + 28)?);
            let extra = usize::from(get16(bytes, at + 30)?);
            let comment = usize::from(get16(bytes, at + 32)?);
            let Some(name) = bytes.get(at + 46..at + 46 + name_len) else { return error("truncated") };
            entries.push((String::from_utf8_lossy(name).into_owned(), located));
            at += 46 + name_len + extra + comment;
        }
        Ok(Archive { bytes, entries })
    }

    /// The names of its entries, in their order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(n, _)| n.as_str())
    }

    /// An entry's contents, when it has one of that name.
    ///
    /// # Errors
    /// The entry cannot be read: out of bounds, compressed by a method other than deflate, damaged.
    #[must_use]
    pub fn read(&self, name: &str) -> Option<Result<Vec<u8>, Error>> {
        let (_, located) = self.entries.iter().find(|(n, _)| n == name)?;
        Some(self.contents(name, *located))
    }

    /// An entry's Unix permissions, where its archive was made on Unix and says them; none otherwise, or when it has no
    /// entry of that name.
    #[must_use]
    pub fn mode(&self, name: &str) -> Option<u32> {
        let (_, located) = self.entries.iter().find(|(n, _)| n == name)?;
        (located.mode != 0).then_some(located.mode)
    }

    fn contents(&self, name: &str, e: Located) -> Result<Vec<u8>, Error> {
        if get32(self.bytes, e.local)? != LOCAL {
            return error(format!("{name}: no local header"));
        }
        let start = e.local
            + 30
            + usize::from(get16(self.bytes, e.local + 26)?)
            + usize::from(get16(self.bytes, e.local + 28)?);
        let Some(body) = self.bytes.get(start..start + e.compressed) else {
            return error(format!("{name}: truncated"));
        };
        let data = match e.method {
            0 => body.to_vec(),
            8 => miniz_oxide::inflate::decompress_to_vec_with_limit(body, e.size.max(1))
                .map_err(|err| Error(format!("{name}: {err:?}")))?,
            m => return error(format!("{name}: compressed by method {m}, which is not read")),
        };
        if data.len() != e.size || crc32fast::hash(&data) != e.crc {
            return error(format!("{name}: damaged (its size or checksum is wrong)"));
        }
        Ok(data)
    }
}

#[cfg(test)]
mod tests {
    use super::{Archive, Entry, write};

    #[test]
    fn an_archive_reads_back_and_writes_the_same_twice() {
        let big = vec![7u8; 10_000];
        let entries = [
            Entry { name: "mimetype", data: b"image/openraster", deflate: false, mode: 0 },
            Entry { name: "data/a.png", data: &big, deflate: true, mode: 0 },
        ];
        let bytes = write(&entries);
        assert_eq!(bytes, write(&entries));
        assert_eq!(&bytes[30..38], b"mimetype");
        let archive = Archive::new(&bytes).unwrap();
        assert_eq!(archive.names().collect::<Vec<_>>(), ["mimetype", "data/a.png"]);
        assert_eq!(archive.read("data/a.png").unwrap().unwrap(), big);
        assert_eq!(archive.read("mimetype").unwrap().unwrap(), b"image/openraster");
        assert!(archive.read("missing").is_none());
        assert_eq!(archive.mode("data/a.png"), None, "made anywhere, it says nothing of permissions");
        // A program's permissions, kept.
        let bytes = write(&[Entry { name: "bin/tool", data: b"#!/bin/sh\n", deflate: true, mode: 0o755 }]);
        assert_eq!(Archive::new(&bytes).unwrap().mode("bin/tool"), Some(0o755));
    }
}
