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
    pub const APPLIED: usize = 16;
    pub const DURABLE: usize = 24;
    pub const HOLDER: usize = 32;
}

/// The page, mapped: shared with every process that maps the same file.
pub struct Header {
    map: memmap2::MmapMut,
    /// Kept open: the lock appenders take is on this file.
    file: File,
}

impl std::fmt::Debug for Header {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "header (tail {}, applied {}, durable {})", self.tail(), self.applied(), self.durable())
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

    /// How far the project has what the log holds.
    #[must_use]
    pub fn applied(&self) -> u64 {
        self.u64_at(at::APPLIED).load(Ordering::Acquire)
    }

    /// The project has the log up to here: only ever said further on.
    pub fn set_applied(&self, position: u64) {
        self.u64_at(at::APPLIED).fetch_max(position, Ordering::AcqRel);
    }

    /// How far what the project has is on disk: the log before it is no longer needed.
    #[must_use]
    pub fn durable(&self) -> u64 {
        self.u64_at(at::DURABLE).load(Ordering::Acquire)
    }

    /// What the project has is on disk up to here: only ever said further on.
    pub fn set_durable(&self, position: u64) {
        self.u64_at(at::DURABLE).fetch_max(position, Ordering::AcqRel);
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
