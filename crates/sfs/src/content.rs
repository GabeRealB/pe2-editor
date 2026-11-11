//! A file's content while it is in transit: which of its bytes are where in the log. What a program writes is
//! appended to the log and found again from here - read where the log is mapped, with no file of its own and no
//! copy - until the project has it.
//!
//! A file that will never be the project's - what an editor leaves beside the file it edits - is kept in memory
//! instead: the log holds only what the project is to have, so nothing that stays for long keeps it from letting
//! go of what is before.

use std::collections::BTreeMap;
use std::io;
use std::time::SystemTime;

use crate::Log;

/// The kind of a record that holds bytes of a file: the file's number, where they go in it, then the bytes.
pub const DATA: u8 = 1;
/// What a data record has before its bytes.
const BEFORE: u64 = 16;
/// The most one record holds of a file: a longer write is several.
const MOST: usize = 1 << 20;

/// A range of a file's bytes: how long, and where it starts in the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Extent {
    len: u64,
    at: u64,
}

/// The content of a file in transit.
#[derive(Clone, Debug)]
pub struct Content {
    /// The file's number in the log's records.
    id: u64,
    held: Held,
    len: u64,
    modified: SystemTime,
}

#[derive(Clone, Debug)]
enum Held {
    /// In the log: each range by where it starts in the file. What no range covers reads as zeros.
    Logged(BTreeMap<u64, Extent>),
    /// In memory alone.
    Memory(Vec<u8>),
}

impl Content {
    /// An empty file whose bytes go to the log, under this number.
    #[must_use]
    pub fn logged(id: u64) -> Content {
        Content { id, held: Held::Logged(BTreeMap::new()), len: 0, modified: SystemTime::now() }
    }

    /// An empty file kept in memory: never the project's.
    #[must_use]
    pub fn in_memory(id: u64) -> Content {
        Content { id, held: Held::Memory(Vec::new()), len: 0, modified: SystemTime::now() }
    }

    #[must_use]
    pub fn id(&self) -> u64 {
        self.id
    }

    #[must_use]
    pub fn len(&self) -> u64 {
        self.len
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// When it was last written.
    #[must_use]
    pub fn modified(&self) -> SystemTime {
        self.modified
    }

    /// Whether its bytes are in the log.
    #[must_use]
    pub fn is_logged(&self) -> bool {
        matches!(self.held, Held::Logged(_))
    }

    /// The first position of the log it still reads from: the log before it is not this file's to keep. None where
    /// it holds nothing there.
    #[must_use]
    pub fn first(&self) -> Option<u64> {
        match &self.held {
            Held::Logged(extents) => extents.values().map(|extent| extent.at).min(),
            Held::Memory(_) => None,
        }
    }

    /// Where it is in the log: each range's start in the file, its length, and its place in the log. None where it
    /// is kept in memory.
    #[must_use]
    pub fn ranges(&self) -> Option<Vec<(u64, u64, u64)>> {
        match &self.held {
            Held::Logged(extents) => Some(extents.iter().map(|(start, extent)| (*start, extent.len, extent.at)).collect()),
            Held::Memory(_) => None,
        }
    }

    /// The content of this length whose ranges in the log are these: what another process says of a file it wrote.
    #[must_use]
    pub fn of_ranges(len: u64, ranges: &[(u64, u64, u64)]) -> Content {
        let extents = ranges.iter().map(|(start, len, at)| (*start, Extent { len: *len, at: *at })).collect();
        Content { id: 0, held: Held::Logged(extents), len, modified: SystemTime::now() }
    }

    /// Write bytes at a place in the file: appended to the log first, then found from here.
    ///
    /// # Errors
    /// The log cannot take them: it is full, or its disk is.
    pub fn write(&mut self, log: &Log, offset: u64, data: &[u8]) -> io::Result<()> {
        match &mut self.held {
            Held::Memory(bytes) => {
                let (start, end) = (to_usize(offset), to_usize(offset) + data.len());
                if bytes.len() < end {
                    bytes.resize(end, 0);
                }
                bytes[start..end].copy_from_slice(data);
            }
            Held::Logged(extents) => {
                let most = MOST.min(to_usize(log.config().segment) / 2);
                let mut at_file = offset;
                for part in data.chunks(most) {
                    let record = log.append(DATA, &[&self.id.to_le_bytes(), &at_file.to_le_bytes(), part])?;
                    place(extents, at_file, Extent { len: part.len() as u64, at: record + crate::log::HEAD + BEFORE });
                    at_file += part.len() as u64;
                }
            }
        }
        self.len = self.len.max(offset + data.len() as u64);
        self.modified = SystemTime::now();
        Ok(())
    }

    /// Make the file this long: shorter, what is past it is gone; longer, what is added reads as zeros.
    pub fn truncate(&mut self, len: u64) {
        match &mut self.held {
            Held::Memory(bytes) => bytes.resize(to_usize(len), 0),
            Held::Logged(extents) => {
                extents.split_off(&len);
                if let Some((start, last)) = extents.iter_mut().next_back()
                    && start + last.len > len
                {
                    last.len = len - start;
                }
            }
        }
        self.len = len;
        self.modified = SystemTime::now();
    }

    /// Read bytes from a place in the file into a buffer: as many as it takes, or the file has.
    ///
    /// # Errors
    /// The log no longer has what it reads from.
    pub fn read_into(&self, log: &Log, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
        if offset >= self.len {
            return Ok(0);
        }
        let take = to_usize((self.len - offset).min(buffer.len() as u64));
        let buffer = &mut buffer[..take];
        match &self.held {
            Held::Memory(bytes) => buffer.copy_from_slice(&bytes[to_usize(offset)..to_usize(offset) + take]),
            Held::Logged(extents) => {
                buffer.fill(0);
                let end = offset + take as u64;
                // The range that starts before the place, where it reaches into it, then those that start within.
                let before = extents.range(..offset).next_back().filter(|(start, extent)| *start + extent.len > offset);
                for (start, extent) in before.into_iter().chain(extents.range(offset..end)) {
                    let from = offset.max(*start);
                    let upto = end.min(start + extent.len);
                    let bytes = log.bytes(extent.at + (from - start), to_usize(upto - from))?;
                    buffer[to_usize(from - offset)..to_usize(upto - offset)].copy_from_slice(&bytes);
                }
            }
        }
        Ok(take)
    }

    /// All of it, written out: for who takes a file whole.
    ///
    /// # Errors
    /// It cannot be read from the log, or written.
    pub fn write_to(&self, log: &Log, out: &mut impl io::Write) -> io::Result<()> {
        let mut buffer = vec![0u8; MOST.min(to_usize(self.len).max(1))];
        let mut at = 0;
        while at < self.len {
            let read = self.read_into(log, at, &mut buffer)?;
            out.write_all(&buffer[..read])?;
            at += read as u64;
        }
        Ok(())
    }

    /// Its bytes appended to the log again and read from there from now on, so that the log before them can be let
    /// go of: for a file that waits long while others are written.
    ///
    /// # Errors
    /// It cannot be read from the log, or appended to it; it is then as it was.
    pub fn moved_forward(&mut self, log: &Log) -> io::Result<()> {
        let Held::Logged(extents) = &self.held else { return Ok(()) };
        let mut moved = Content::logged(self.id);
        for (start, extent) in extents {
            moved.write(log, *start, &log.bytes(extent.at, to_usize(extent.len))?)?;
        }
        self.held = moved.held;
        Ok(())
    }

    /// The same content, held the other way: brought into the log, where it comes to be what the project is to
    /// have; out of it into memory, where it will never be.
    ///
    /// # Errors
    /// It cannot be read from the log, or appended to it.
    pub fn held_in_log(&mut self, log: &Log, logged: bool) -> io::Result<()> {
        if logged == self.is_logged() {
            return Ok(());
        }
        let mut bytes = Vec::with_capacity(to_usize(self.len));
        self.write_to(log, &mut bytes)?;
        let modified = self.modified;
        self.held = if logged { Held::Logged(BTreeMap::new()) } else { Held::Memory(Vec::new()) };
        self.len = 0;
        self.write(log, 0, &bytes)?;
        self.modified = modified;
        Ok(())
    }
}

fn to_usize(n: u64) -> usize {
    usize::try_from(n).unwrap_or(usize::MAX)
}

/// Put a range among a file's: what it covers of those there is theirs no more.
fn place(extents: &mut BTreeMap<u64, Extent>, offset: u64, new: Extent) {
    let end = offset + new.len;
    // One that starts before it and reaches into it keeps its start; past the new one's end, its rest.
    if let Some((start, old)) = extents.range(..offset).next_back().map(|(start, old)| (*start, *old))
        && start + old.len > offset
    {
        extents.insert(start, Extent { len: offset - start, at: old.at });
        if start + old.len > end {
            extents.insert(end, Extent { len: start + old.len - end, at: old.at + (end - start) });
        }
    }
    // Those that start within it go; one that reaches past its end keeps its rest.
    let within: Vec<u64> = extents.range(offset..end).map(|(start, _)| *start).collect();
    for start in within {
        if let Some(old) = extents.remove(&start)
            && start + old.len > end
        {
            extents.insert(end, Extent { len: start + old.len - end, at: old.at + (end - start) });
        }
    }
    extents.insert(offset, new);
}

#[cfg(test)]
mod tests {
    use super::Content;
    use crate::log::{Config, Log};

    fn log(name: &str) -> (std::path::PathBuf, Log) {
        let dir = std::env::temp_dir().join(format!("pe2-sfs-content-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let log = Log::open(&dir, Config { segment: 1 << 16, budget: 1 << 30 }).unwrap();
        (dir, log)
    }

    fn all(content: &Content, log: &Log) -> Vec<u8> {
        let mut out = Vec::new();
        content.write_to(log, &mut out).unwrap();
        out
    }

    #[test]
    fn what_is_written_is_read_back_whatever_overlaps_it() {
        let (dir, log) = log("overlap");
        // The same writes to a file in the log, to one in memory, and to a plain buffer: all read the same.
        let (mut logged, mut memory, mut plain) = (Content::logged(1), Content::in_memory(2), Vec::<u8>::new());
        let writes: [(u64, &[u8]); 7] = [
            (0, b"0123456789"),
            (20, b"past a hole"),
            (5, b"over the middle and on"),
            (2, b"xy"),
            (0, b"A"),
            (40, b"far"),
            (3, b"spanning everything written before it, and more....."),
        ];
        for (offset, data) in writes {
            logged.write(&log, offset, data).unwrap();
            memory.write(&log, offset, data).unwrap();
            let end = usize::try_from(offset).unwrap() + data.len();
            if plain.len() < end {
                plain.resize(end, 0);
            }
            plain[end - data.len()..end].copy_from_slice(data);
            assert_eq!(all(&logged, &log), plain, "after writing {data:?} at {offset}");
            assert_eq!(all(&memory, &log), plain);
        }
        // Read in part, across ranges and a hole.
        let mut some = [0u8; 9];
        assert_eq!(logged.read_into(&log, 1, &mut some).unwrap(), 9);
        assert_eq!(some, plain[1..10]);
        assert_eq!(logged.read_into(&log, plain.len() as u64, &mut some).unwrap(), 0, "nothing past its end");
        // Made shorter, then longer: what is added reads as zeros.
        for content in [&mut logged, &mut memory] {
            content.truncate(7);
            content.truncate(12);
        }
        plain.truncate(7);
        plain.resize(12, 0);
        assert_eq!(all(&logged, &log), plain);
        assert_eq!(all(&memory, &log), plain);
        // Said by its ranges, as to another process, it reads the same.
        let said = Content::of_ranges(logged.len(), &logged.ranges().unwrap());
        assert_eq!(all(&said, &log), plain);
        assert!(memory.ranges().is_none());
        // Moved forward in the log, it reads the same, from past where the log ended.
        let ended = log.header().tail();
        logged.moved_forward(&log).unwrap();
        assert!(logged.first().unwrap() > ended);
        assert_eq!(all(&logged, &log), plain);
        // Held the other way, the same content.
        logged.held_in_log(&log, false).unwrap();
        memory.held_in_log(&log, true).unwrap();
        assert!(!logged.is_logged() && memory.is_logged());
        assert_eq!((all(&logged, &log), all(&memory, &log)), (plain.clone(), plain));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_long_write_is_several_records_and_one_file() {
        let (dir, log) = log("long");
        let mut content = Content::logged(7);
        let data: Vec<u8> = (0..200_000u32).map(|n| (n % 251) as u8).collect();
        content.write(&log, 0, &data).unwrap();
        assert!(log.records(0).count() > 1, "more than a segment's worth is several records");
        assert_eq!(all(&content, &log), data);
        assert_eq!(content.first(), Some(crate::log::HEAD + 16), "it reads from the log's first record on");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
