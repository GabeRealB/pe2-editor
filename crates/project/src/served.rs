//! What is served of a project, as a table in a file both sides map into memory: whoever holds the project writes it
//! (`write`) - every path of the served folder, what it is, and where its content lives - and the server maps it
//! (`Table::map`) and serves from it, reading nothing else of the project to know what a file is. A table is never
//! changed once written: what is served next is another file, of a later generation, which the server is told of.
//!
//! The file: a header, the entries - one a path, of a fixed size, sorted by path - the entries' numbers sorted by
//! where a layer holds each (for a read to find what it reads), and the text all of them name, little-endian
//! throughout.

use std::io;
use std::path::{Path, PathBuf};

use crate::server::{Listing, Shown, Source};

/// The folder of a project's cache the tables are in.
pub const DIR: &str = "served";

const MAGIC: [u8; 8] = *b"pe2srv\x00\x01";
const HEADER: usize = 48;
const ENTRY: usize = 56;

/// Where the table of a generation is, for the project in `root`.
#[must_use]
pub fn path(root: &Path, generation: u64) -> PathBuf {
    root.join(crate::cache::DIR).join(DIR).join(format!("{generation:020}.table"))
}

/// The generation after every table the project in `root` has: later than the time now, and than any there.
#[must_use]
pub fn next_generation(root: &Path) -> u64 {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    let now = u64::try_from(now).unwrap_or(u64::MAX - 1);
    generations(root).last().map_or(now, |last| now.max(last + 1))
}

/// The generations the project in `root` has tables of, oldest first.
#[must_use]
pub fn generations(root: &Path) -> Vec<u64> {
    let dir = root.join(crate::cache::DIR).join(DIR);
    let names = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|entry| entry.file_name());
    let mut found: Vec<u64> =
        names.filter_map(|name| name.to_str()?.strip_suffix(".table")?.parse().ok()).collect();
    found.sort_unstable();
    found
}

/// Remove the tables of the project in `root` older than `generation`, where nothing maps them still (Windows keeps
/// a file something maps: it goes the next time).
pub fn remove_before(root: &Path, generation: u64) {
    for old in generations(root).into_iter().filter(|g| *g < generation) {
        let _ = std::fs::remove_file(path(root, old));
    }
}

/// The text a table names, each piece once: where it is, and how long.
#[derive(Default)]
struct Text {
    bytes: Vec<u8>,
    known: std::collections::HashMap<String, (u32, u32)>,
}

impl Text {
    fn place(&mut self, text: &str) -> io::Result<[u8; 8]> {
        let (at, len) = if let Some(placed) = self.known.get(text) {
            *placed
        } else {
            let too_long = |_| io::Error::other("more text than a table holds");
            let placed =
                (u32::try_from(self.bytes.len()).map_err(too_long)?, u32::try_from(text.len()).map_err(too_long)?);
            self.bytes.extend_from_slice(text.as_bytes());
            self.known.insert(text.to_string(), placed);
            placed
        };
        let mut out = [0u8; 8];
        out[..4].copy_from_slice(&at.to_le_bytes());
        out[4..].copy_from_slice(&len.to_le_bytes());
        Ok(out)
    }
}

fn text_of(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Write the table of what is served of the project in `root`, as `generation`: whole, then named so, so that who
/// maps it never sees part of one.
///
/// # Errors
/// It cannot be written.
pub fn write(root: &Path, generation: u64, listing: &Listing) -> io::Result<()> {
    let mut text = Text::default();
    let mut entries = Vec::with_capacity(listing.len() * ENTRY);
    // Each entry's number by where a layer holds it, for those that are files.
    let mut by_disk: Vec<(&Path, u32)> = Vec::new();
    for (number, (path, shown)) in listing.iter().enumerate() {
        let number = u32::try_from(number).map_err(|_| io::Error::other("more entries than a table holds"))?;
        let mut entry = [0u8; ENTRY];
        entry[..8].copy_from_slice(&text.place(path)?);
        match shown {
            Shown::Folder => entry[8] = 0,
            Shown::Node => entry[8] = 1,
            Shown::File { disk, size, modified, of_files, source } => {
                entry[8] = if *of_files { 3 } else { 2 };
                entry[12..16].copy_from_slice(&modified.1.to_le_bytes());
                entry[16..24].copy_from_slice(&size.to_le_bytes());
                entry[24..32].copy_from_slice(&modified.0.to_le_bytes());
                entry[32..40].copy_from_slice(&text.place(&text_of(disk))?);
                match source {
                    Source::Disk => entry[9] = 1,
                    Source::RecordText { record, name } => {
                        entry[9] = 2;
                        entry[40..48].copy_from_slice(&text.place(&text_of(record))?);
                        entry[48..56].copy_from_slice(&text.place(name)?);
                    }
                    Source::Stored { sha1, store } => {
                        entry[9] = 3;
                        entry[40..48].copy_from_slice(&text.place(sha1)?);
                        entry[48..56].copy_from_slice(&text.place(&text_of(store))?);
                    }
                }
                by_disk.push((disk, number));
            }
        }
        entries.extend_from_slice(&entry);
    }
    by_disk.sort_by(|a, b| text_of(a.0).as_bytes().cmp(text_of(b.0).as_bytes()));
    let count = listing.len() as u64;
    let entries_at = HEADER as u64;
    let disks_at = entries_at + entries.len() as u64;
    let text_at = disks_at + by_disk.len() as u64 * 4;
    let mut file = Vec::with_capacity(HEADER + entries.len() + by_disk.len() * 4 + text.bytes.len());
    file.extend_from_slice(&MAGIC);
    for field in [generation, count, by_disk.len() as u64, text_at, text.bytes.len() as u64] {
        file.extend_from_slice(&field.to_le_bytes());
    }
    file.extend_from_slice(&entries);
    for (_, number) in &by_disk {
        file.extend_from_slice(&number.to_le_bytes());
    }
    file.extend_from_slice(&text.bytes);
    let at = path(root, generation);
    if let Some(dir) = at.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let writing = at.with_extension("writing");
    std::fs::write(&writing, &file)?;
    std::fs::rename(&writing, &at)
}

/// A table mapped into memory: read where it is, nothing of it copied until asked for.
pub struct Table {
    map: memmap2::Mmap,
    count: usize,
    disks: usize,
    text_at: usize,
}

impl std::fmt::Debug for Table {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "a table of {} entries (generation {})", self.count, self.generation())
    }
}

/// An entry of a table: a path of the served folder, as the table says it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry<'a> {
    pub path: &'a str,
    pub is: Is<'a>,
}

/// What an entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Is<'a> {
    Folder,
    Node,
    File {
        /// Where a layer holds it: what a read of it names.
        disk: &'a str,
        size: u64,
        modified: (u64, u32),
        of_files: bool,
        source: From<'a>,
    },
}

/// Where a file's content lives (`Source`, as the table has it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum From<'a> {
    Disk,
    RecordText { record: &'a str, name: &'a str },
    Stored { sha1: &'a str, store: &'a str },
}

impl Table {
    /// Map the table of `generation` of the project in `root`.
    ///
    /// # Errors
    /// It is not there, or is no table: of another format, or shorter than it says.
    pub fn map(root: &Path, generation: u64) -> io::Result<Table> {
        let file = std::fs::File::open(path(root, generation))?;
        // SAFETY: a table is written whole before it is named, and never written again: what is mapped does not
        // change under the mapping.
        let map = unsafe { memmap2::Mmap::map(&file)? };
        let bad = |why: &str| io::Error::new(io::ErrorKind::InvalidData, format!("no table: {why}"));
        if map.len() < HEADER || map[..8] != MAGIC {
            return Err(bad("not of this format"));
        }
        let field = |i: usize| {
            let at = 8 + i * 8;
            usize::try_from(u64::from_le_bytes(map[at..at + 8].try_into().unwrap_or_default())).unwrap_or(usize::MAX)
        };
        let (said, count, disks, text_at, text_len) = (field(0), field(1), field(2), field(3), field(4));
        let ends = count.checked_mul(ENTRY).and_then(|n| n.checked_add(HEADER));
        let ends = ends.and_then(|n| disks.checked_mul(4).and_then(|d| n.checked_add(d)));
        if ends != Some(text_at) || text_at.checked_add(text_len) != Some(map.len()) {
            return Err(bad("shorter or longer than it says"));
        }
        if said as u64 != generation {
            return Err(bad("of another generation than its name"));
        }
        Ok(Table { map, count, disks, text_at })
    }

    #[must_use]
    pub fn generation(&self) -> u64 {
        u64::from_le_bytes(self.map[8..16].try_into().unwrap_or_default())
    }

    /// How many entries it has.
    #[must_use]
    pub fn len(&self) -> usize {
        self.count
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    fn text(&self, placed: &[u8]) -> &str {
        let at = u32::from_le_bytes(placed[..4].try_into().unwrap_or_default()) as usize;
        let len = u32::from_le_bytes(placed[4..8].try_into().unwrap_or_default()) as usize;
        let bytes = self.map.get(self.text_at + at..self.text_at + at + len).unwrap_or_default();
        std::str::from_utf8(bytes).unwrap_or_default()
    }

    /// An entry by its number, in the order of their paths.
    #[must_use]
    pub fn get(&self, number: usize) -> Option<Entry<'_>> {
        if number >= self.count {
            return None;
        }
        let entry = &self.map[HEADER + number * ENTRY..HEADER + (number + 1) * ENTRY];
        let path = self.text(&entry[..8]);
        let is = match entry[8] {
            0 => Is::Folder,
            1 => Is::Node,
            kind => Is::File {
                disk: self.text(&entry[32..40]),
                size: u64::from_le_bytes(entry[16..24].try_into().unwrap_or_default()),
                modified: (
                    u64::from_le_bytes(entry[24..32].try_into().unwrap_or_default()),
                    u32::from_le_bytes(entry[12..16].try_into().unwrap_or_default()),
                ),
                of_files: kind == 3,
                source: match entry[9] {
                    2 => From::RecordText { record: self.text(&entry[40..48]), name: self.text(&entry[48..56]) },
                    3 => From::Stored { sha1: self.text(&entry[40..48]), store: self.text(&entry[48..56]) },
                    _ => From::Disk,
                },
            },
        };
        Some(Entry { path, is })
    }

    /// Its entries, in the order of their paths.
    pub fn entries(&self) -> impl Iterator<Item = Entry<'_>> {
        (0..self.count).filter_map(|number| self.get(number))
    }

    /// The file a layer holds at `disk`, where the table has one: found without reading the others.
    #[must_use]
    pub fn held_at(&self, disk: &str) -> Option<Entry<'_>> {
        let disks_at = HEADER + self.count * ENTRY;
        let number = |i: usize| {
            let at = disks_at + i * 4;
            u32::from_le_bytes(self.map[at..at + 4].try_into().unwrap_or_default()) as usize
        };
        let disk_of = |i: usize| match self.get(number(i)).map(|entry| entry.is) {
            Some(Is::File { disk, .. }) => disk,
            _ => "",
        };
        let (mut low, mut high) = (0, self.disks);
        while low < high {
            let middle = low + (high - low) / 2;
            match disk_of(middle).as_bytes().cmp(disk.as_bytes()) {
                std::cmp::Ordering::Less => low = middle + 1,
                std::cmp::Ordering::Greater => high = middle,
                std::cmp::Ordering::Equal => return self.get(number(middle)),
            }
        }
        None
    }

    /// What it lists, as a listing of its own: every path copied out.
    #[must_use]
    pub fn listing(&self) -> Listing {
        let entries = self.entries().map(|entry| {
            let shown = match entry.is {
                Is::Folder => Shown::Folder,
                Is::Node => Shown::Node,
                Is::File { disk, size, modified, of_files, source } => Shown::File {
                    disk: PathBuf::from(disk),
                    size,
                    modified,
                    of_files,
                    source: match source {
                        From::Disk => Source::Disk,
                        From::RecordText { record, name } => {
                            Source::RecordText { record: PathBuf::from(record), name: name.to_string() }
                        }
                        From::Stored { sha1, store } => {
                            Source::Stored { sha1: sha1.to_string(), store: PathBuf::from(store) }
                        }
                    },
                },
            };
            (entry.path.to_string(), shown)
        });
        entries.collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{Table, write};
    use crate::server::{Listing, Shown, Source};
    use std::path::PathBuf;

    #[test]
    fn a_table_written_is_mapped_as_it_was_listed() {
        let root = std::env::temp_dir().join(format!("pe2-served-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let file = |disk: &str, source: Source| Shown::File {
            disk: PathBuf::from(disk),
            size: 7,
            modified: (1_700_000_000, 5),
            of_files: false,
            source,
        };
        let text = Source::RecordText { record: PathBuf::from("/l/.records/ab/uid.toml"), name: "notes.txt".into() };
        let stored = Source::Stored { sha1: "da39".into(), store: PathBuf::from("/l/.store") };
        let listing: Listing = [
            ("a".to_string(), Shown::Folder),
            ("a/n".to_string(), Shown::Node),
            ("a/n/node.toml".to_string(), file("/l/a/n.node/node.toml", text)),
            ("a/n/big.bin".to_string(), file("/l/a/n.node/big.bin", stored)),
            ("plain.txt".to_string(), file("/l/plain.txt", Source::Disk)),
        ]
        .into();
        write(&root, 3, &listing).unwrap();
        let table = Table::map(&root, 3).unwrap();
        assert_eq!((table.generation(), table.len()), (3, 5));
        assert_eq!(table.listing(), listing);
        // A read finds what it reads by where a layer holds it.
        let held = table.held_at("/l/a/n.node/big.bin").unwrap();
        assert_eq!(held.path, "a/n/big.bin");
        assert!(table.held_at("/l/nothing").is_none());
        // What is no table is said to be none.
        std::fs::write(super::path(&root, 4), b"something else").unwrap();
        assert!(Table::map(&root, 4).is_err());
        assert_eq!(super::generations(&root), [3, 4]);
        drop(table);
        super::remove_before(&root, 4);
        assert_eq!(super::generations(&root), [4]);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
