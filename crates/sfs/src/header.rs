//! The shared header: one page (`state`) every process of a project maps, of positions in the log kept as atomics -
//! read with no call to the system and no question to another process.

use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// The header's file, in the library's folder.
pub const FILE: &str = "state";

const MAGIC: u64 = u64::from_le_bytes(*b"pe2sfs\x00\x01");
const SIZE: usize = 4096;

/// Where each value is in the page.
mod at {
    pub const MAGIC: usize = 0;
    pub const TAIL: usize = 8;
    pub const DURABLE: usize = 24;
    pub const HOLDER: usize = 32;
    pub const SEGMENT: usize = 40;
    pub const STORAGE: usize = 48;
    pub const TRANSIT: usize = 56;
}

/// The page, mapped: shared with every process that maps the same file.
pub struct Header {
    map: memmap2::MmapMut,
    /// Kept open: the lock appenders take is on this file.
    file: File,
}

impl std::fmt::Debug for Header {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (tail, durable) = (self.tail(), self.durable());
        write!(f, "header (tail {tail}, kept from {durable}, in transit {:?}, storage {:?})", self.transit(), self.storage())
    }
}

impl Header {
    /// Map the header in `dir`, made - a page of zeros, then its mark - where it is not there.
    ///
    /// # Errors
    /// The file cannot be made or mapped, or is no header of this format.
    pub fn open(dir: &Path) -> io::Result<Header> {
        std::fs::create_dir_all(dir)?;
        let file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(dir.join(FILE))?;
        if file.metadata()?.len() < SIZE as u64 {
            // Written, not only sized: the page is the file's own, never a hole that a full disk could refuse.
            use std::io::Write as _;
            (&file).write_all(&[0u8; SIZE])?;
            file.sync_all()?;
        }
        // SAFETY: the file is the library's own, of at least a page, and only ever changed through such mappings,
        // by atomic stores.
        let map = unsafe { memmap2::MmapOptions::new().len(SIZE).map_mut(&file)? };
        let header = Header { map, file };
        match header.u64_at(at::MAGIC).compare_exchange(0, MAGIC, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) | Err(MAGIC) => Ok(header),
            Err(_) => Err(io::Error::new(io::ErrorKind::InvalidData, "no header of this format")),
        }
    }

    /// The header's own file: what appenders lock, across processes.
    #[must_use]
    pub fn file(&self) -> &File {
        &self.file
    }

    #[expect(clippy::cast_ptr_alignment, reason = "a mapping starts at a page, and `at` is a multiple of eight")]
    fn u64_at(&self, at: usize) -> &AtomicU64 {
        debug_assert!(at % 8 == 0 && at + 8 <= SIZE);
        // SAFETY: within the page, aligned to eight bytes (the page is page-aligned and `at` a multiple of eight),
        // and valid for as long as the mapping, which `self` holds; every process reads and writes it as an atomic.
        unsafe { &*self.map.as_ptr().add(at).cast::<AtomicU64>() }
    }

    #[expect(clippy::cast_ptr_alignment, reason = "a mapping starts at a page, and `at` is a multiple of four")]
    fn u32_at(&self, at: usize) -> &AtomicU32 {
        debug_assert!(at % 4 == 0 && at + 4 <= SIZE);
        // SAFETY: as `u64_at`, aligned to four bytes.
        unsafe { &*self.map.as_ptr().add(at).cast::<AtomicU32>() }
    }

    /// Where the log ends: everything before it is whole.
    #[must_use]
    pub fn tail(&self) -> u64 {
        self.u64_at(at::TAIL).load(Ordering::Acquire)
    }

    /// The log ends here now: said by who appended, once what it appended is whole.
    pub fn set_tail(&self, position: u64) {
        self.u64_at(at::TAIL).store(position, Ordering::Release);
    }

    /// From where the log holds what is in transit through the served folder - what a serving that starts takes up
    /// (`ops`). None where nothing is: the serving before ended with all of it given, or there was none.
    #[must_use]
    pub fn transit(&self) -> Option<u64> {
        self.u64_at(at::TRANSIT).load(Ordering::Acquire).checked_sub(1)
    }

    /// The log is needed from here for what is in transit, or (none) for none of it. Said - and had on disk
    /// (`flush`) - as a serving begins, further on as what it holds is given.
    pub fn set_transit(&self, from: Option<u64>) {
        self.u64_at(at::TRANSIT).store(from.map_or(0, |from| from + 1), Ordering::Release);
    }

    /// From where the log is kept: what is before it nobody needs - neither what is in transit (`transit`) nor what
    /// was written to the project's files (`storage`).
    #[must_use]
    pub fn durable(&self) -> u64 {
        self.u64_at(at::DURABLE).load(Ordering::Acquire)
    }

    /// The log is kept from here: only ever said further on.
    pub fn set_durable(&self, position: u64) {
        self.u64_at(at::DURABLE).fetch_max(position, Ordering::AcqRel);
    }

    /// The size of the log's segments: what whoever made the log first laid it out with, whatever is asked for after.
    #[must_use]
    pub fn segment_or(&self, size: u64) -> u64 {
        match self.u64_at(at::SEGMENT).compare_exchange(0, size, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => size,
            Err(set) => set,
        }
    }

    /// From where the log holds what whoever holds the project wrote to its files and may not have on disk yet:
    /// sets of changes that are to be written again where they were not. None where all of it is on disk.
    #[must_use]
    pub fn storage(&self) -> Option<u64> {
        self.u64_at(at::STORAGE).load(Ordering::Acquire).checked_sub(1)
    }

    /// The log is needed from here for what was written to the project's files, or (none) for none of it. Said
    /// before the first set of changes is appended, and had on disk (`flush`): who lets go of the log reads it.
    pub fn set_storage(&self, from: Option<u64>) {
        self.u64_at(at::STORAGE).store(from.map_or(0, |from| from + 1), Ordering::Release);
    }

    /// Have the header on disk as it is.
    ///
    /// # Errors
    /// It cannot be flushed.
    pub fn flush(&self) -> io::Result<()> {
        self.map.flush()
    }

    /// The process holding the project, where one said so; 0 where none.
    #[must_use]
    pub fn holder(&self) -> u32 {
        self.u32_at(at::HOLDER).load(Ordering::Acquire)
    }

    /// This process holds the project (its id), or nobody does (0).
    pub fn set_holder(&self, process: u32) {
        self.u32_at(at::HOLDER).store(process, Ordering::Release);
    }
}
