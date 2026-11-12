//! The space: pages every process of a project maps and reads, changed by one writer at a time and never where a
//! reader can see. Chunk files of 4 MiB (`<n>.chunk`), each mapped by itself - nothing ever grows in place - and
//! pages of 4 KiB by number. What is in the pages is the trees' (`tree`); the space knows only pages, and what a
//! commit is.
//!
//! - **A commit** is a generation: the roots of the trees, how many pages are in use, how far the log is reflected.
//!   It is written whole to one of two pages in turn (`head`, pages 1 and 2, with a checksum) and then published in
//!   the shared header (page 0) under a counter readers check before and after reading it - so a writer that dies
//!   between the two leaves the header to be put right from the page it wrote, by the next to take the writer's
//!   lock.
//! - **One writer at a time**: a lock the system holds on a file (`writer`), let go of where its holder dies. A
//!   writer changes no page a reader can reach: it copies the page into one of its own (`Txn::own`), and the old one
//!   is free once no reader is at a generation that could reach it. The free pages are listed in a file of their
//!   own every writer maps (`free`), each with the generation whose commit freed it, changed under the writer's
//!   lock in an order that a death at any point leaves without a page listed twice: at worst a page is listed no
//!   more, and is not used again until the space is made anew. A writer that dies inside a transaction has
//!   published nothing; the pages it took are past what the last commit says is in use, or lost that way.
//! - **Readers take no lock**: a reader pins the generation it reads at in a slot of the header (pinned, then
//!   checked that it is still the current one, else pinned again), which a writer reads before it takes a freed page
//!   again. A slot is its process's by a lock on a file of its own (`reader.<n>`), so the pin of a process that died
//!   is found and cleared.
//! - **Loud**: every page says its kind, its own number and the generation that wrote it, and a pointer to a page
//!   says both its number and that generation - so whoever follows one finds the very page that was pointed to, or
//!   says so (`Pages::page`). What fails marks the space damaged for every process, with what was found; its files
//!   are kept aside (`damaged/`) by the next to attach alone.
//! - **Trusted** where another process has it attached - it is as its last commit left it - or, attached to alone,
//!   where it was had on disk at its current generation (`sync`, which says so in `clean`; the first change after
//!   takes that back, on disk, before it is made). Otherwise it begins empty (`Space::fresh`), for whoever owns
//!   what it holds to fill again: nothing in it is the only copy of anything.

use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

#[cfg(test)]
mod tests;

/// A page's length.
pub const PAGE: usize = 4096;
/// How many pages a chunk file holds: 4 MiB.
const CHUNK_PAGES: u64 = 1024;
const CHUNK: usize = 1024 * PAGE;
/// The most chunks a space maps: 16 GiB.
const CHUNKS: usize = 4096;
/// How many pages of its first chunk a space is made ready to write (`Space::ready`): what the first transactions
/// of a holder take.
const READY_PAGES: usize = 64;
/// How many trees a space has roots for.
pub const ROOTS: usize = 16;
/// How many processes have one space attached at a time.
const READERS: usize = 64;
/// How long the list of free pages is, in bytes: a count, then a page's number and the generation that freed it for
/// each - 8,191 of them. What is freed beyond that is not listed. Its file is written whole for every space there
/// is, so it is no longer than a long hold on old pages needs.
const FREE: usize = 1 << 17;
/// What a page begins with: its kind (1), nothing (3), its own number (4), the generation that wrote it (8).
pub const PAGE_HEAD: usize = 16;
/// How often, at most, a writer that readers hold back looks whether their processes still run.
const PROBE: Duration = Duration::from_millis(100);

const MAGIC: u64 = u64::from_le_bytes(*b"pe2spc\x00\x02");

/// Where each value of the shared header (page 0 of `head`) is.
mod at {
    pub const MAGIC: usize = 0;
    /// Odd while the commit below it is being published.
    pub const SEQ: usize = 8;
    pub const GENERATION: usize = 16;
    pub const HIGH: usize = 24;
    pub const LOG: usize = 32;
    /// Set, with what was found in `WHY`, by whoever finds a page that is not what it should be.
    pub const DAMAGED: usize = 40;
    /// Whether `clean` says the space is on disk as it is.
    pub const CLEAN: usize = 48;
    pub const ROOTS: usize = 64;
    /// Each attached process's pin: the generation it reads at, plus one; 0 for none.
    pub const READERS: usize = 256;
    /// Whether each attached process wrote pages it has not flushed.
    pub const WROTE: usize = 768;
    pub const WHY: usize = 1280;
}

/// A pointer to a page: its number, and - above it - the low half of the generation that wrote it.
const NUMBER: u64 = 0xFFFF_FFFF;

fn pointer(number: u64, generation: u64) -> u64 {
    number | ((generation & NUMBER) << 32)
}

/// What a commit is, as the header and the two pages after it hold it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Commit {
    pub generation: u64,
    /// How many pages are in use: those from here on were never written.
    pub high: u64,
    /// How far the log is reflected in the trees.
    pub log: u64,
    /// A pointer to each tree's root; 0 for a tree with nothing in it.
    pub roots: [u64; ROOTS],
}

fn number_at(bytes: &[u8], at: usize) -> u64 {
    bytes.get(at..at + 8).and_then(|b| b.try_into().ok()).map_or(0, u64::from_le_bytes)
}

impl Commit {
    fn write(&self, page: &mut [u8]) {
        page[..PAGE].fill(0);
        page[0..8].copy_from_slice(&MAGIC.to_le_bytes());
        page[8..16].copy_from_slice(&self.generation.to_le_bytes());
        page[16..24].copy_from_slice(&self.high.to_le_bytes());
        page[24..32].copy_from_slice(&self.log.to_le_bytes());
        for (i, root) in self.roots.iter().enumerate() {
            page[32 + i * 8..40 + i * 8].copy_from_slice(&root.to_le_bytes());
        }
        let sum = crc32fast::hash(&page[..PAGE - 4]);
        page[PAGE - 4..PAGE].copy_from_slice(&sum.to_le_bytes());
    }

    fn read(page: &[u8]) -> Option<Commit> {
        let sum = u32::from_le_bytes(page.get(PAGE - 4..PAGE)?.try_into().ok()?);
        if number_at(page, 0) != MAGIC || crc32fast::hash(&page[..PAGE - 4]) != sum {
            return None;
        }
        let mut roots = [0u64; ROOTS];
        for (i, root) in roots.iter_mut().enumerate() {
            *root = number_at(page, 32 + i * 8);
        }
        Some(Commit { generation: number_at(page, 8), high: number_at(page, 16), log: number_at(page, 24), roots })
    }

    /// The commit a header's page says.
    fn of_header(page: &[u8]) -> Commit {
        let mut roots = [0u64; ROOTS];
        for (i, root) in roots.iter_mut().enumerate() {
            *root = number_at(page, at::ROOTS + i * 8);
        }
        Commit {
            generation: number_at(page, at::GENERATION),
            high: number_at(page, at::HIGH),
            log: number_at(page, at::LOG),
            roots,
        }
    }
}

/// Why something of the space could not be done: the files, or the space itself - a page that is not what whoever
/// follows a pointer to it has to find (`Damaged`, said to every process from then on).
#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Damaged(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Damaged(why) => write!(f, "the shared space is damaged: {why}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Error {
        Error::Io(e)
    }
}

impl From<Error> for io::Error {
    fn from(e: Error) -> io::Error {
        match e {
            Error::Io(e) => e,
            Error::Damaged(why) => io::Error::new(io::ErrorKind::InvalidData, format!("the shared space is damaged: {why}")),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Memory of this process's own, zeroed, a page's alignment: what a private space is in.
struct Mem {
    at: *mut u8,
    layout: std::alloc::Layout,
}

// SAFETY: memory the space owns; what reads and writes it does so as it does a mapping's - atomics in the header,
// pages by the one writer.
unsafe impl Send for Mem {}
// SAFETY: as above.
unsafe impl Sync for Mem {}

impl Mem {
    fn zeroed(len: usize) -> io::Result<Mem> {
        let layout = std::alloc::Layout::from_size_align(len, PAGE).map_err(io::Error::other)?;
        // SAFETY: a layout of a length that is not zero.
        let at = unsafe { std::alloc::alloc_zeroed(layout) };
        if at.is_null() {
            return Err(io::Error::new(io::ErrorKind::OutOfMemory, "no memory for a private space"));
        }
        Ok(Mem { at, layout })
    }
}

impl Drop for Mem {
    fn drop(&mut self) {
        // SAFETY: allocated with this layout, freed once.
        unsafe { std::alloc::dealloc(self.at, self.layout) }
    }
}

/// Where the pages are: files of the project's cache, mapped and shared; or this process's own memory, for a space
/// nothing else reads (`Space::private`).
enum Backing {
    Files {
        dir: PathBuf,
        /// Kept mapped for as long as the space is.
        maps: Mutex<Vec<memmap2::MmapRaw>>,
        head: memmap2::MmapRaw,
        free: memmap2::MmapRaw,
        writer: File,
        /// Held shared while the space is attached here; and this process's slot, held alone.
        _attached: File,
        _reader: File,
    },
    Memory {
        chunks: Mutex<Vec<Mem>>,
        head: Mem,
        free: Mem,
    },
}

/// What a test has a space call at each step that a process may stop at.
#[cfg(test)]
type Faults = Arc<dyn Fn(&'static str) + Send + Sync>;

/// The pages of a project's shared structures.
pub struct Space {
    backing: Backing,
    /// Each chunk's first byte, once mapped here: read with no lock.
    chunks: Box<[AtomicPtr<u8>]>,
    /// One of this process's threads writes at a time; with it, when it last looked whether the processes whose
    /// pins hold pages back still run.
    writing: Mutex<Option<Instant>>,
    /// The generations this process's readers are at, each with how many: the lowest is what its slot says.
    pins: Mutex<BTreeMap<u64, usize>>,
    slot: usize,
    /// It began empty: what it is to hold is put there again by whoever owns it.
    fresh: AtomicBool,
    #[cfg(test)]
    faults: Mutex<Option<Faults>>,
}

impl std::fmt::Debug for Space {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "space at generation {}", self.atom(at::GENERATION).load(Ordering::Acquire))
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A file of a space, opened to be read, written and locked: made where there is none.
fn opened(path: &Path) -> io::Result<File> {
    std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)
}

/// A file of a space, of at least this length, all of it the file's own on the disk - never a hole a full disk
/// could refuse to who writes through a mapping: a full disk is an error here, to who asks for the room. Given
/// by the system where it can give a file its blocks without writing them; written as zeros elsewhere.
fn sized(path: &Path, len: u64) -> io::Result<File> {
    let file = opened(path)?;
    let has = file.metadata()?.len();
    if has < len {
        allocate(&file, has, len)?;
    }
    Ok(file)
}

#[cfg(target_os = "linux")]
fn allocate(file: &File, _has: u64, len: u64) -> io::Result<()> {
    use std::os::fd::AsRawFd as _;
    let len = libc::off_t::try_from(len).map_err(io::Error::other)?;
    // SAFETY: a file open here, for its whole length from its start.
    match unsafe { libc::posix_fallocate(file.as_raw_fd(), 0, len) } {
        0 => Ok(()),
        failed => Err(io::Error::from_raw_os_error(failed)),
    }
}

/// On Windows a file made longer is given its clusters then, and reads as zeros until written.
#[cfg(windows)]
fn allocate(file: &File, _has: u64, len: u64) -> io::Result<()> {
    file.set_len(len)
}

#[cfg(not(any(target_os = "linux", windows)))]
fn allocate(mut file: &File, has: u64, len: u64) -> io::Result<()> {
    use std::io::{Seek as _, Write as _};
    file.seek(io::SeekFrom::Start(has))?;
    let zeros = vec![0u8; 1 << 16];
    let mut left = len - has;
    while left > 0 {
        let part = usize::try_from(left).unwrap_or(usize::MAX).min(zeros.len());
        file.write_all(&zeros[..part])?;
        left -= part as u64;
    }
    Ok(())
}

/// Whether a file of a space's folder is one of its pages' - not one of its locks.
fn of_pages(name: &str) -> bool {
    ["head", "free", "clean"].contains(&name) || name.contains(".chunk")
}

/// What `clean` says: the generation the space was had on disk at, and the checksum of its list of free pages.
fn said_clean(generation: u64, free: &[u8]) -> [u8; 12] {
    let count = usize::try_from(number_at(free, 0)).unwrap_or(0).min((FREE - 8) / 16);
    let mut said = [0u8; 12];
    said[..8].copy_from_slice(&generation.to_le_bytes());
    said[8..].copy_from_slice(&crc32fast::hash(&free[..8 + count * 16]).to_le_bytes());
    said
}

/// Whether the files in `dir` are a space had on disk as it is: `clean` says the generation its header is at - which
/// is whole, the last commit written, not damaged - and its list of free pages is the one it was said with.
fn is_clean(dir: &Path) -> bool {
    let (Ok(said), Ok(head), Ok(free)) = (std::fs::read(dir.join("clean")), std::fs::read(dir.join("head")), std::fs::read(dir.join("free")))
    else {
        return false;
    };
    if head.len() < 3 * PAGE || free.len() < FREE {
        return false;
    }
    let header = Commit::of_header(&head);
    let latest = [1usize, 2].iter().filter_map(|n| Commit::read(&head[n * PAGE..(n + 1) * PAGE])).max_by_key(|c| c.generation);
    number_at(&head, at::MAGIC) == MAGIC
        && number_at(&head, at::SEQ) % 2 == 0
        && number_at(&head, at::DAMAGED) == 0
        // A space nothing was ever committed to has no commit written.
        && (latest == Some(header) || (latest.is_none() && header == Commit::default()))
        && said == said_clean(header.generation, &free)
}

/// Make a chunk's file, whole, where there is none: by one process at a time, under a name of its own until it is
/// whole - so that no process ever writes zeros into a chunk another is using.
fn make_chunk(dir: &Path, number: usize) -> io::Result<File> {
    let path = dir.join(format!("{number:08}.chunk"));
    let whole = |path: &Path| {
        let file = std::fs::OpenOptions::new().read(true).write(true).open(path).ok()?;
        (file.metadata().ok()?.len() >= CHUNK as u64).then_some(file)
    };
    if let Some(file) = whole(&path) {
        return Ok(file);
    }
    let grow = opened(&dir.join("grow"))?;
    grow.lock()?;
    let made = (|| {
        if let Some(file) = whole(&path) {
            return Ok(file);
        }
        let being_made = dir.join(format!("{number:08}.chunk.{}", std::process::id()));
        let _ = std::fs::remove_file(&being_made);
        drop(sized(&being_made, CHUNK as u64)?);
        std::fs::rename(&being_made, &path)?;
        std::fs::OpenOptions::new().read(true).write(true).open(&path)
    })();
    let _ = grow.unlock();
    made
}

impl Space {
    fn with(backing: Backing, slot: usize, fresh: bool) -> Space {
        Space {
            backing,
            chunks: (0..CHUNKS).map(|_| AtomicPtr::new(std::ptr::null_mut())).collect(),
            writing: Mutex::default(),
            pins: Mutex::default(),
            slot,
            fresh: AtomicBool::new(fresh),
            #[cfg(test)]
            faults: Mutex::default(),
        }
    }

    /// A space of this process's own, in its memory: what a scope and a test serve from.
    ///
    /// # Errors
    /// There is no memory for it.
    pub fn private() -> io::Result<Space> {
        let backing = Backing::Memory { chunks: Mutex::default(), head: Mem::zeroed(3 * PAGE)?, free: Mem::zeroed(FREE)? };
        let space = Space::with(backing, 0, true);
        space.atom(at::MAGIC).store(MAGIC, Ordering::Release);
        Ok(space)
    }

    /// The space kept in `dir`, made where there is none. Where nothing else has it attached and it was not had on
    /// disk as it is (`sync`), it begins empty (`fresh`) - its files kept aside where it was found damaged.
    ///
    /// # Errors
    /// Its files cannot be made or mapped, are of another format, or every slot is taken.
    pub fn open(dir: &Path) -> io::Result<Space> {
        std::fs::create_dir_all(dir)?;
        let writer = opened(&dir.join("writer"))?;
        let attached = opened(&dir.join("attached"))?;
        // One process attaches at a time: who has it alone decides whether it is trusted before another counts on it.
        writer.lock()?;
        let parts = (|| {
            // Alone with it - asked a few times: a process starting another holds, for a moment, what its threads
            // have open, the locks on it with it.
            let alone = (0..5).any(|again| {
                if again > 0 {
                    std::thread::sleep(Duration::from_millis(2));
                }
                attached.try_lock().is_ok()
            });
            let trusted = !alone || is_clean(dir);
            if !trusted {
                Space::set_aside(dir)?;
            }
            if alone {
                attached.unlock()?;
            }
            attached.lock_shared()?;
            let head = sized(&dir.join("head"), 3 * PAGE as u64)?;
            let head = memmap2::MmapOptions::new().len(3 * PAGE).map_raw(&head)?;
            let free = sized(&dir.join("free"), FREE as u64)?;
            let free = memmap2::MmapOptions::new().len(FREE).map_raw(&free)?;
            let reader = (0..READERS).find_map(|slot| {
                let file = opened(&dir.join(format!("reader.{slot}"))).ok()?;
                file.try_lock().ok().map(|()| (slot, file))
            });
            let (slot, reader) = reader.ok_or_else(|| io::Error::other("every slot of the space is taken"))?;
            Ok::<_, io::Error>((head, free, slot, reader, alone))
        })();
        let (head, free, slot, reader, alone) = match parts {
            Ok(parts) => parts,
            Err(e) => {
                let _ = writer.unlock();
                return Err(e);
            }
        };
        let backing =
            Backing::Files { dir: dir.to_path_buf(), maps: Mutex::default(), head, free, writer, _attached: attached, _reader: reader };
        let space = Space::with(backing, slot, false);
        let made = match space.atom(at::MAGIC).compare_exchange(0, MAGIC, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => Ok(true),
            Err(MAGIC) => Ok(false),
            Err(_) => Err(io::Error::new(io::ErrorKind::InvalidData, "no space of this format")),
        };
        if let Ok(made) = made {
            space.fresh.store(made, Ordering::Release);
            if alone {
                // What the processes before it said of themselves says nothing now.
                for slot in 0..READERS {
                    space.atom(at::READERS + slot * 8).store(0, Ordering::Release);
                    space.atom(at::WROTE + slot * 8).store(0, Ordering::Release);
                }
                space.atom(at::CLEAN).store(u64::from(!made), Ordering::Release);
            }
            // Its own slot says nothing yet, whoever had it before.
            space.atom(at::READERS + slot * 8).store(0, Ordering::Release);
            space.atom(at::WROTE + slot * 8).store(0, Ordering::Release);
        }
        space.unlock_writer();
        made.map(|_| space)
    }

    /// Take a space's pages out of its folder: nothing of it is trusted. Kept aside where it was found damaged, in
    /// place of what was kept before.
    fn set_aside(dir: &Path) -> io::Result<()> {
        let damaged = std::fs::read(dir.join("head")).is_ok_and(|head| number_at(&head, at::DAMAGED) != 0);
        let aside = dir.join("damaged");
        if damaged {
            let _ = std::fs::remove_dir_all(&aside);
            std::fs::create_dir_all(&aside)?;
        }
        for entry in std::fs::read_dir(dir)?.flatten() {
            let name = entry.file_name();
            if !name.to_str().is_some_and(of_pages) || !entry.file_type().is_ok_and(|kind| kind.is_file()) {
                continue;
            }
            if damaged {
                std::fs::rename(entry.path(), aside.join(&name))?;
            } else {
                std::fs::remove_file(entry.path())?;
            }
        }
        Ok(())
    }

    /// Have room for what is written first: the first chunk made, where there is none - by who can wait for it,
    /// so that the first to write does not.
    pub fn ready(&self) {
        let Ok(first) = self.chunk(0, true) else { return };
        // And its first pages, the two a commit is written to and the list of free ones touched: the first write
        // to a page of a file that was given its blocks unwritten is what the system takes its time over. Touched
        // without changing a byte, whoever may be writing there meanwhile.
        let touch = |at: *mut u8| {
            // SAFETY: a byte of a mapping of the space's, valid for as long as `self`, read and written here as an
            // atomic that is left as it is.
            let byte = unsafe { &*at.cast::<std::sync::atomic::AtomicU8>() };
            // Exchanged for itself: a write the system sees, which one that changes nothing may be compiled out of
            // being - and none at all where somebody wrote there meanwhile, which is as good.
            let holds = byte.load(Ordering::Relaxed);
            let _ = byte.compare_exchange(holds, holds, Ordering::AcqRel, Ordering::Relaxed);
        };
        for page in 0..READY_PAGES {
            // SAFETY: within the first chunk, which holds more pages than that.
            touch(unsafe { first.add(page * PAGE) });
        }
        for page in 1..3 {
            // SAFETY: the head's second and third pages.
            touch(unsafe { self.head().add(page * PAGE) });
        }
        touch(self.free().as_mut_ptr());
    }

    /// Whether it began empty when it was opened here: what it is to hold is to be put there.
    #[must_use]
    pub fn fresh(&self) -> bool {
        self.fresh.load(Ordering::Acquire)
    }

    /// Have a test's function called at each step a process may stop at.
    #[cfg(test)]
    fn set_faults(&self, faults: Faults) {
        *lock(&self.faults) = Some(faults);
    }

    #[cfg(test)]
    fn fault(&self, point: &'static str) {
        let faults = lock(&self.faults).clone();
        if let Some(faults) = faults {
            faults(point);
        }
    }

    #[cfg(not(test))]
    #[expect(clippy::unused_self, reason = "a step a test stops a process at")]
    #[inline]
    fn fault(&self, _point: &'static str) {}

    fn head(&self) -> *mut u8 {
        match &self.backing {
            Backing::Files { head, .. } => head.as_mut_ptr(),
            Backing::Memory { head, .. } => head.at,
        }
    }

    #[expect(clippy::cast_ptr_alignment, reason = "the header begins a page, and `at` is a multiple of eight")]
    fn atom(&self, at: usize) -> &AtomicU64 {
        debug_assert!(at.is_multiple_of(8) && at + 8 <= PAGE);
        // SAFETY: within the header's first page, aligned to eight bytes, valid for as long as `self`; every
        // process reads and writes it as an atomic.
        unsafe { &*self.head().add(at).cast::<AtomicU64>() }
    }

    /// One of the two pages a commit is written to, by its generation.
    #[expect(clippy::mut_from_ref, reason = "written by the one writer alone, under its lock")]
    fn commit_page(&self, generation: u64) -> &mut [u8] {
        let at = PAGE * (1 + usize::from(generation % 2 == 1));
        // SAFETY: within the head's three pages; written by the one writer, read by whoever puts the header right
        // under the same lock.
        unsafe { std::slice::from_raw_parts_mut(self.head().add(at), PAGE) }
    }

    /// The list of free pages: read and written by who has the writer's lock.
    #[expect(clippy::mut_from_ref, reason = "the one writer's alone, under its lock")]
    fn free(&self) -> &mut [u8] {
        let first = match &self.backing {
            Backing::Files { free, .. } => free.as_mut_ptr(),
            Backing::Memory { free, .. } => free.at,
        };
        // SAFETY: the list's whole length; nobody reads or writes it but who has the writer's lock.
        unsafe { std::slice::from_raw_parts_mut(first, FREE) }
    }

    /// What was found that should not be: said to every process, from now on.
    fn damaged(&self, why: String) -> Error {
        if self.atom(at::DAMAGED).swap(1, Ordering::AcqRel) == 0 {
            let bytes = why.as_bytes();
            let len = bytes.len().min(PAGE - at::WHY - 8);
            // SAFETY: within the header's page; written once, by whoever set the flag from 0.
            unsafe {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.head().add(at::WHY + 8), len);
            }
            self.atom(at::WHY).store(len as u64, Ordering::Release);
        }
        Error::Damaged(why)
    }

    /// Fail where the space was found damaged, by this process or another, with what was found.
    fn sound(&self) -> Result<()> {
        if self.atom(at::DAMAGED).load(Ordering::Acquire) == 0 {
            return Ok(());
        }
        let len = usize::try_from(self.atom(at::WHY).load(Ordering::Acquire)).unwrap_or(0).min(PAGE - at::WHY - 8);
        // SAFETY: within the header's page, written before its length was.
        let why = unsafe { std::slice::from_raw_parts(self.head().add(at::WHY + 8), len) };
        Err(Error::Damaged(String::from_utf8_lossy(why).into_owned()))
    }

    /// A chunk's first byte: mapped here where it is not yet - made, of its whole length, where `make` says so.
    fn chunk(&self, number: usize, make: bool) -> Result<*mut u8> {
        let Some(known) = self.chunks.get(number) else {
            if make {
                return Err(io::Error::new(io::ErrorKind::StorageFull, "the shared space holds all it may").into());
            }
            return Err(self.damaged(format!("a page in chunk {number}, past all a space holds")));
        };
        let mapped = known.load(Ordering::Acquire);
        if !mapped.is_null() {
            return Ok(mapped);
        }
        match &self.backing {
            Backing::Memory { chunks, .. } => {
                let mut chunks = lock(chunks);
                let again = known.load(Ordering::Acquire);
                if !again.is_null() {
                    return Ok(again);
                }
                if !make {
                    return Err(self.damaged(format!("a page in chunk {number}, which was never made")));
                }
                let chunk = Mem::zeroed(CHUNK)?;
                let first = chunk.at;
                chunks.push(chunk);
                known.store(first, Ordering::Release);
                Ok(first)
            }
            Backing::Files { dir, maps, .. } => {
                let mut maps = lock(maps);
                let again = known.load(Ordering::Acquire);
                if !again.is_null() {
                    return Ok(again);
                }
                let file = if make {
                    make_chunk(dir, number)?
                } else {
                    let path = dir.join(format!("{number:08}.chunk"));
                    match std::fs::OpenOptions::new().read(true).write(true).open(&path) {
                        Ok(file) if file.metadata()?.len() >= CHUNK as u64 => file,
                        Ok(_) => return Err(self.damaged(format!("chunk {number} is not whole"))),
                        Err(e) if e.kind() == io::ErrorKind::NotFound => {
                            return Err(self.damaged(format!("a page in chunk {number}, which is not there")));
                        }
                        Err(e) => return Err(e.into()),
                    }
                };
                let map = memmap2::MmapOptions::new().len(CHUNK).map_raw(&file)?;
                let first = map.as_mut_ptr();
                maps.push(map);
                known.store(first, Ordering::Release);
                Ok(first)
            }
        }
    }

    /// Where a page is.
    fn page_at(&self, number: u64, make: bool) -> Result<*mut u8> {
        let chunk = self.chunk(usize::try_from(number / CHUNK_PAGES).unwrap_or(usize::MAX), make)?;
        // SAFETY: within the chunk, which holds `CHUNK_PAGES` pages.
        Ok(unsafe { chunk.add(usize::try_from(number % CHUNK_PAGES).unwrap_or(0) * PAGE) })
    }

    /// Have the chunk after the one in use made, where the pages in use come near its end: ahead of need, and by
    /// who holds no lock others wait for.
    fn ahead(&self, high: u64) {
        if high % CHUNK_PAGES >= CHUNK_PAGES / 2 {
            let _ = self.chunk(usize::try_from(high / CHUNK_PAGES).unwrap_or(usize::MAX - 1) + 1, true);
        }
    }

    /// The commit the header says, as it is read at this moment.
    fn said(&self) -> Commit {
        let mut commit = Commit {
            generation: self.atom(at::GENERATION).load(Ordering::SeqCst),
            high: self.atom(at::HIGH).load(Ordering::Acquire),
            log: self.atom(at::LOG).load(Ordering::Acquire),
            roots: [0; ROOTS],
        };
        for (i, root) in commit.roots.iter_mut().enumerate() {
            *root = self.atom(at::ROOTS + i * 8).load(Ordering::Acquire);
        }
        commit
    }

    /// The commit the header says, read whole: before and after, its counter the same and even.
    fn current(&self) -> Result<Commit> {
        let mut spun = 0u32;
        loop {
            self.sound()?;
            let before = self.atom(at::SEQ).load(Ordering::Acquire);
            if before % 2 == 0 {
                let commit = self.said();
                if self.atom(at::SEQ).load(Ordering::Acquire) == before {
                    return Ok(commit);
                }
            }
            spun += 1;
            if spun > 1000 {
                // A writer that died publishing: put right by whoever has the writer's lock - here, where it is free.
                drop(self.write()?);
                spun = 0;
            }
            std::hint::spin_loop();
        }
    }

    /// Publish a commit in the header.
    fn publish(&self, commit: &Commit) {
        let seq = self.atom(at::SEQ);
        let odd = seq.load(Ordering::Acquire) | 1;
        seq.store(odd, Ordering::Release);
        self.atom(at::HIGH).store(commit.high, Ordering::Release);
        self.atom(at::LOG).store(commit.log, Ordering::Release);
        self.fault("publish: half said");
        for (i, root) in commit.roots.iter().enumerate() {
            self.atom(at::ROOTS + i * 8).store(*root, Ordering::Release);
        }
        // Last, and in one order with every reader's pin: a reader that pinned an earlier one sees this.
        self.atom(at::GENERATION).store(commit.generation, Ordering::SeqCst);
        seq.store(odd + 1, Ordering::Release);
    }

    /// Read: the trees as the last commit left them, for as long as what this returns is held - kept short, and
    /// never across a call into what may itself read or wait.
    ///
    /// # Errors
    /// The space is damaged.
    pub fn read(&self) -> Result<Snapshot<'_>> {
        loop {
            let commit = self.current()?;
            self.fault("read: before the pin");
            self.pin(commit.generation);
            self.fault("read: pinned");
            // Still the one: a writer that takes a freed page again reads the pins after this one was said.
            if self.atom(at::GENERATION).load(Ordering::SeqCst) == commit.generation {
                return Ok(Snapshot { space: self, commit });
            }
            self.unpin(commit.generation);
        }
    }

    /// Read as `read` does, for as long as what this returns is held - by whoever holds it, wherever: a view of
    /// what the trees hold at one commit, kept by what it is a view for.
    ///
    /// # Errors
    /// The space is damaged.
    pub fn pinned(self: &Arc<Self>) -> Result<Pinned> {
        let commit = {
            let read = self.read()?;
            // Pinned once more, for what is returned: the reading's own goes as it is dropped.
            self.pin(read.commit.generation);
            read.commit
        };
        Ok(Pinned { space: self.clone(), commit })
    }

    fn pin(&self, generation: u64) {
        let mut pins = lock(&self.pins);
        *pins.entry(generation).or_default() += 1;
        let lowest = pins.keys().next().copied().unwrap_or(generation);
        self.atom(at::READERS + self.slot * 8).store(lowest + 1, Ordering::SeqCst);
    }

    fn unpin(&self, generation: u64) {
        let mut pins = lock(&self.pins);
        if let Some(count) = pins.get_mut(&generation) {
            *count -= 1;
            if *count == 0 {
                pins.remove(&generation);
            }
        }
        let lowest = pins.keys().next().map_or(0, |lowest| lowest + 1);
        self.atom(at::READERS + self.slot * 8).store(lowest, Ordering::SeqCst);
    }

    /// The lowest generation any reader is at; none where none reads.
    fn oldest_reader(&self) -> Option<u64> {
        (0..READERS).filter_map(|slot| self.atom(at::READERS + slot * 8).load(Ordering::SeqCst).checked_sub(1)).min()
    }

    /// Whether the process a slot is of is gone: nobody holds the slot's file.
    fn gone(&self, slot: usize) -> bool {
        let Backing::Files { dir, .. } = &self.backing else { return false };
        if slot == self.slot {
            return false;
        }
        let Ok(file) = std::fs::OpenOptions::new().read(true).write(true).open(dir.join(format!("reader.{slot}"))) else {
            return false;
        };
        let gone = file.try_lock().is_ok();
        if gone {
            let _ = file.unlock();
        }
        gone
    }

    /// Clear the pins of processes that are gone.
    fn clear_gone(&self) {
        for slot in 0..READERS {
            if self.atom(at::READERS + slot * 8).load(Ordering::SeqCst) != 0 && self.gone(slot) {
                self.atom(at::READERS + slot * 8).store(0, Ordering::SeqCst);
            }
        }
    }

    /// Write: the one writer, until what this returns is committed or dropped - in which case nothing changed.
    ///
    /// # Errors
    /// The writer's lock cannot be taken, or the space is damaged.
    pub fn write(&self) -> Result<Txn<'_>> {
        let writing = lock(&self.writing);
        if let Backing::Files { writer, .. } = &self.backing {
            writer.lock()?;
        }
        let from = (|| {
            // A writer that died publishing left the header half said: said again from the page it wrote.
            if self.atom(at::SEQ).load(Ordering::Acquire) % 2 == 1 {
                let written = [0u64, 1].iter().filter_map(|g| Commit::read(self.commit_page(*g))).max_by_key(|c| c.generation);
                let Some(commit) = written else {
                    return Err(self.damaged("the header was left half said, and neither page a commit is written to is whole".into()));
                };
                self.publish(&commit);
            }
            self.sound()?;
            Ok(self.said())
        })();
        match from {
            Ok(from) => {
                let commit = Commit { generation: from.generation + 1, ..from };
                Ok(Txn { space: self, writing, from, commit, own: HashSet::new(), let_go: Vec::new(), spare: Vec::new(), touched: false, done: false })
            }
            Err(e) => {
                self.unlock_writer();
                Err(e)
            }
        }
    }

    fn unlock_writer(&self) {
        if let Backing::Files { writer, .. } = &self.backing {
            let _ = writer.unlock();
        }
    }

    /// Have on disk what this process wrote of the space.
    ///
    /// # Errors
    /// It cannot be flushed.
    pub fn flush(&self) -> io::Result<()> {
        let Backing::Files { maps, head, free, .. } = &self.backing else { return Ok(()) };
        // Said first: what is written while it flushes is not flushed for sure.
        self.atom(at::WROTE + self.slot * 8).store(0, Ordering::SeqCst);
        let flushed = (|| {
            for map in lock(maps).iter() {
                map.flush()?;
            }
            free.flush()?;
            head.flush()
        })();
        if flushed.is_err() {
            self.atom(at::WROTE + self.slot * 8).store(1, Ordering::SeqCst);
        }
        flushed
    }

    /// Have the space on disk as it is, and say so: attached to alone later, it is trusted at this generation.
    /// False where another process wrote pages it has yet to flush itself (`flush`), which only it can where the
    /// system writes a mapping's pages for the process that changed them alone.
    ///
    /// # Errors
    /// It cannot be flushed, or the saying written; or it is damaged.
    pub fn sync(&self) -> io::Result<bool> {
        let Backing::Files { dir, .. } = &self.backing else { return Ok(true) };
        // Under the writer's lock: what is flushed is one commit, whole.
        let held = self.write()?;
        self.flush()?;
        for slot in 0..READERS {
            if slot == self.slot || self.atom(at::WROTE + slot * 8).load(Ordering::SeqCst) == 0 {
                continue;
            }
            // What a process that is gone wrote is the files' own now, flushed with them below.
            if self.gone(slot) {
                self.atom(at::WROTE + slot * 8).store(0, Ordering::SeqCst);
            } else if cfg!(windows) {
                return Ok(false);
            }
        }
        for entry in std::fs::read_dir(dir)?.flatten() {
            let name = entry.file_name();
            if name.to_str().is_some_and(|name| of_pages(name) && name != "clean") {
                std::fs::OpenOptions::new().read(true).write(true).open(entry.path())?.sync_all()?;
            }
        }
        let clean = dir.join("clean");
        std::fs::write(&clean, said_clean(held.from.generation, self.free()))?;
        std::fs::OpenOptions::new().read(true).write(true).open(&clean)?.sync_all()?;
        if cfg!(unix) {
            File::open(dir)?.sync_all()?;
        }
        self.atom(at::CLEAN).store(1, Ordering::SeqCst);
        drop(held);
        Ok(true)
    }

    /// Before the first change after it was said clean: said so no more, on disk - the file first, so that a
    /// process stopped between leaves it to be taken back again.
    fn unclean(&self) -> io::Result<()> {
        if self.atom(at::CLEAN).load(Ordering::SeqCst) == 0 {
            return Ok(());
        }
        if let Backing::Files { dir, .. } = &self.backing {
            match std::fs::remove_file(dir.join("clean")) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
            if cfg!(unix) {
                File::open(dir)?.sync_all()?;
            }
        }
        self.atom(at::CLEAN).store(0, Ordering::SeqCst);
        Ok(())
    }

    /// How much of it is in use: its generation, how many pages were ever written, how many of those are listed
    /// free.
    ///
    /// # Errors
    /// As `write`.
    pub fn usage(&self) -> Result<Usage> {
        let held = self.write()?;
        let free = number_at(self.free(), 0);
        Ok(Usage { generation: held.from.generation, pages: held.from.high.saturating_sub(1), free })
    }
}

/// How much of a space is in use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Usage {
    pub generation: u64,
    /// The pages ever written.
    pub pages: u64,
    /// Those of them listed free.
    pub free: u64,
}

/// Pages, read: what a tree is read through, by a reader at its generation or by the writer among its own.
pub trait Pages {
    /// The page a pointer leads to, checked to be the one that was pointed to: of this kind, saying its own number
    /// and the generation the pointer says wrote it.
    ///
    /// # Errors
    /// It is not: the space is damaged, and says so to every process.
    fn page(&self, pointer: u64, kind: u8) -> Result<&[u8]> {
        self.page_either(pointer, kind, kind)
    }

    /// A page that may be of either of two kinds, checked as `page` checks one: its first byte says which.
    ///
    /// # Errors
    /// As `page`.
    fn page_either(&self, pointer: u64, kind: u8, or: u8) -> Result<&[u8]>;

    /// A pointer to the root of one of the space's trees; 0 for a tree with nothing in it yet.
    fn root(&self, tree: usize) -> u64;

    /// Say that what a page holds is not what it has to: the space is damaged, for every process, from now on.
    fn damaged(&self, why: String) -> Error;
}

fn checked(space: &Space, pointer: u64, kinds: [u8; 2], high: u64) -> Result<&[u8]> {
    let number = pointer & NUMBER;
    if number == 0 || number >= high {
        return Err(space.damaged(format!("a pointer to page {number}, of {high} in use")));
    }
    // SAFETY: a page in use is in a chunk that was made; it is not written while a reader at this generation, or
    // the writer that owns it, can reach it.
    let page = unsafe { std::slice::from_raw_parts(space.page_at(number, false)?, PAGE) };
    let said = u64::from(u32::from_le_bytes(page[4..8].try_into().unwrap_or_default()));
    let written = number_at(page, 8);
    if !kinds.contains(&page[0]) || said != number || written & NUMBER != pointer >> 32 {
        return Err(space.damaged(format!(
            "a pointer to page {number} of kind {} written by generation {} leads to one that says kind {}, number {said}, generation {written}",
            kinds[0],
            pointer >> 32,
            page[0]
        )));
    }
    Ok(page)
}

/// The trees as one commit left them: read for as long as this is held.
pub struct Snapshot<'a> {
    space: &'a Space,
    commit: Commit,
}

impl Snapshot<'_> {
    /// The commit read at.
    #[must_use]
    pub fn commit(&self) -> &Commit {
        &self.commit
    }
}

impl Pages for Snapshot<'_> {
    fn page_either(&self, pointer: u64, kind: u8, or: u8) -> Result<&[u8]> {
        checked(self.space, pointer, [kind, or], self.commit.high)
    }

    fn root(&self, tree: usize) -> u64 {
        self.commit.roots[tree]
    }

    fn damaged(&self, why: String) -> Error {
        self.space.damaged(why)
    }
}

impl Drop for Snapshot<'_> {
    fn drop(&mut self) {
        self.space.unpin(self.commit.generation);
    }
}

/// The trees as one commit left them, read for as long as this is held, by whoever holds it: what a `Snapshot`
/// is, kept with the space it is of.
pub struct Pinned {
    space: Arc<Space>,
    commit: Commit,
}

impl Pinned {
    /// The commit read at.
    #[must_use]
    pub fn commit(&self) -> &Commit {
        &self.commit
    }

    /// The space read.
    #[must_use]
    pub fn space(&self) -> &Arc<Space> {
        &self.space
    }
}

impl Clone for Pinned {
    fn clone(&self) -> Pinned {
        self.space.pin(self.commit.generation);
        Pinned { space: self.space.clone(), commit: self.commit }
    }
}

impl Pages for Pinned {
    fn page_either(&self, pointer: u64, kind: u8, or: u8) -> Result<&[u8]> {
        checked(&self.space, pointer, [kind, or], self.commit.high)
    }

    fn root(&self, tree: usize) -> u64 {
        self.commit.roots[tree]
    }

    fn damaged(&self, why: String) -> Error {
        self.space.damaged(why)
    }
}

impl Drop for Pinned {
    fn drop(&mut self) {
        self.space.unpin(self.commit.generation);
    }
}

/// The one writer's changes, until committed: pages of its own, which nobody else reads yet.
pub struct Txn<'a> {
    space: &'a Space,
    writing: MutexGuard<'a, Option<Instant>>,
    from: Commit,
    commit: Commit,
    /// The pages this transaction made, by number: written where they are.
    own: HashSet<u64>,
    /// The pages it reaches no more, free once it is committed and no reader is before it.
    let_go: Vec<u64>,
    /// Pages it made and reaches no more: the first it takes again, and free at once when it ends.
    spare: Vec<u64>,
    /// Whether it said, on disk, that the space is no longer as it was had there.
    touched: bool,
    done: bool,
}

impl Pages for Txn<'_> {
    fn page_either(&self, pointer: u64, kind: u8, or: u8) -> Result<&[u8]> {
        checked(self.space, pointer, [kind, or], self.commit.high)
    }

    fn root(&self, tree: usize) -> u64 {
        self.commit.roots[tree]
    }

    fn damaged(&self, why: String) -> Error {
        self.space.damaged(why)
    }
}

impl Txn<'_> {
    /// Say where a tree's root is now.
    pub fn set_root(&mut self, tree: usize, pointer: u64) {
        self.commit.roots[tree] = pointer;
    }

    /// Say how far the log is reflected by what this commits.
    pub fn set_log(&mut self, position: u64) {
        self.commit.log = position;
    }

    /// How far the log was reflected when this began.
    #[must_use]
    pub fn log(&self) -> u64 {
        self.from.log
    }

    /// The generation this commits.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.commit.generation
    }

    /// Before its first change: the space is no longer as it was had on disk, and this process has pages to flush.
    fn touch(&mut self) -> io::Result<()> {
        if !self.touched {
            self.space.unclean()?;
            self.space.atom(at::WROTE + self.space.slot * 8).store(1, Ordering::SeqCst);
            self.touched = true;
        }
        Ok(())
    }

    /// A page of this transaction's own, of a kind, with nothing in it: a pointer to it.
    ///
    /// # Errors
    /// No room can be made for it, or the space is damaged.
    pub fn make(&mut self, kind: u8) -> Result<u64> {
        self.touch()?;
        // One freed before every reader there is, where there is one; else one never written - never 0, which no
        // pointer leads to.
        let number = match self.spare.pop().or_else(|| self.take_free()) {
            Some(number) => number,
            None => self.commit.high.max(1),
        };
        // SAFETY: a page nobody reaches: freed before every reader, or never written.
        let page = unsafe { std::slice::from_raw_parts_mut(self.space.page_at(number, true)?, PAGE) };
        self.commit.high = self.commit.high.max(number + 1);
        page.fill(0);
        page[0] = kind;
        page[4..8].copy_from_slice(&u32::try_from(number).unwrap_or(u32::MAX).to_le_bytes());
        page[8..16].copy_from_slice(&self.commit.generation.to_le_bytes());
        self.own.insert(number);
        self.space.fault("make: a page taken");
        Ok(pointer(number, self.commit.generation))
    }

    fn free_entry(list: &[u8], i: usize) -> (u64, u64) {
        (number_at(list, 8 + i * 16), number_at(list, 16 + i * 16))
    }

    fn free_count(list: &[u8]) -> usize {
        usize::try_from(number_at(list, 0)).unwrap_or(0).min((FREE - 8) / 16)
    }

    /// Take a page from the free list: one freed by a commit no reader is before. Its entry goes first - the count
    /// said one less, then the last entry put in its place - so that a death between leaves a page unlisted, never
    /// one listed twice.
    fn take_free(&mut self) -> Option<u64> {
        let count = Txn::free_count(self.space.free());
        if count == 0 {
            return None;
        }
        // Looked for from the end: what was freed last is what a writer that frees and makes in turn takes first.
        let takeable = |space: &Space| {
            let oldest = space.oldest_reader();
            let list = space.free();
            (0..count).rev().take(256).find(|i| oldest.is_none_or(|oldest| oldest >= Txn::free_entry(list, *i).1))
        };
        let mut at = takeable(self.space);
        if at.is_none() && self.writing.is_none_or(|probed| probed.elapsed() >= PROBE) {
            // Readers hold every one back: those whose processes are gone, no longer.
            *self.writing = Some(Instant::now());
            self.space.clear_gone();
            at = takeable(self.space);
        }
        let at = at?;
        let list = self.space.free();
        let (number, _) = Txn::free_entry(list, at);
        let (moved, freed) = Txn::free_entry(list, count - 1);
        list[..8].copy_from_slice(&((count - 1) as u64).to_le_bytes());
        self.space.fault("free: one fewer said");
        if at != count - 1 {
            list[8 + at * 16..16 + at * 16].copy_from_slice(&moved.to_le_bytes());
            list[16 + at * 16..24 + at * 16].copy_from_slice(&freed.to_le_bytes());
        }
        // No page is 0: an entry that says so is none.
        (number != 0).then_some(number)
    }

    /// List a page free, by the generation whose commit freed it (0: never reached by anyone). The entry is written
    /// before the count says it is there. Past what the list holds, the page is not listed.
    fn list_free(&mut self, number: u64, generation: u64) {
        let list = self.space.free();
        let count = Txn::free_count(list);
        if count >= (FREE - 8) / 16 {
            return;
        }
        list[8 + count * 16..16 + count * 16].copy_from_slice(&number.to_le_bytes());
        list[16 + count * 16..24 + count * 16].copy_from_slice(&generation.to_le_bytes());
        self.space.fault("free: an entry written");
        list[..8].copy_from_slice(&((count + 1) as u64).to_le_bytes());
    }

    /// A page to write: the one pointed to where it is this transaction's own, else a copy of it that is - the one
    /// given let go of. A pointer to it.
    ///
    /// # Errors
    /// As `make` and `page`.
    pub fn own(&mut self, pointer: u64, kind: u8) -> Result<u64> {
        if self.own.contains(&(pointer & NUMBER)) {
            return Ok(pointer);
        }
        let copied = self.page(pointer, kind)?.to_vec();
        let mine = self.make(kind)?;
        let page = self.page_mut(mine)?;
        page[PAGE_HEAD..].copy_from_slice(&copied[PAGE_HEAD..]);
        self.let_go.push(pointer & NUMBER);
        Ok(mine)
    }

    /// A page of this transaction's own, to write.
    ///
    /// # Errors
    /// It is not its own.
    pub fn page_mut(&mut self, pointer: u64) -> Result<&mut [u8]> {
        let number = pointer & NUMBER;
        if !self.own.contains(&number) {
            return Err(self.space.damaged(format!("page {number} written by a transaction that does not own it")));
        }
        // SAFETY: a page of this transaction's own: nobody else reaches it until it is committed.
        Ok(unsafe { std::slice::from_raw_parts_mut(self.space.page_at(number, false)?, PAGE) })
    }

    /// Reach a page no more.
    pub fn let_go(&mut self, pointer: u64) {
        let number = pointer & NUMBER;
        if self.own.remove(&number) {
            // Its own, never published: nobody else ever reached it.
            self.spare.push(number);
        } else {
            self.let_go.push(number);
        }
    }

    /// Publish: every reader that begins from now on reads the trees as this leaves them.
    ///
    /// # Errors
    /// The space is damaged, or can no longer be said not to be as it was had on disk.
    pub fn commit(mut self) -> Result<()> {
        self.space.sound()?;
        if self.own.is_empty() && self.let_go.is_empty() && self.commit.roots == self.from.roots && self.commit.log == self.from.log {
            self.done = true;
            return Ok(());
        }
        self.touch()?;
        let commit = self.commit;
        self.space.fault("commit: before its page");
        commit.write(self.space.commit_page(commit.generation));
        self.space.fault("commit: its page written");
        self.space.publish(&commit);
        self.space.fault("commit: published");
        // What it reaches no more is free once no reader is before this commit.
        for page in std::mem::take(&mut self.let_go) {
            self.list_free(page, commit.generation);
        }
        for page in std::mem::take(&mut self.spare) {
            self.list_free(page, 0);
        }
        self.space.fresh.store(false, Ordering::Release);
        self.done = true;
        Ok(())
    }
}

impl Drop for Txn<'_> {
    fn drop(&mut self) {
        if !self.done {
            // Nothing was published: the pages it took of the free ones are free, as they were; those past what
            // the last commit says is in use were never written.
            let high = self.from.high;
            let made: Vec<u64> = self.own.drain().chain(self.spare.drain(..)).filter(|page| *page < high).collect();
            for page in made {
                self.list_free(page, 0);
            }
        }
        self.space.unlock_writer();
        if self.done {
            self.space.ahead(self.commit.high);
        }
    }
}

/// The spaces this process has attached, one a folder: a second asking for one gets the same.
///
/// # Errors
/// As `Space::open`.
pub fn shared(dir: &Path) -> io::Result<Arc<Space>> {
    static OPEN: Mutex<Vec<(PathBuf, std::sync::Weak<Space>)>> = Mutex::new(Vec::new());
    std::fs::create_dir_all(dir)?;
    let dir = dir.canonicalize()?;
    let mut open = lock(&OPEN);
    open.retain(|(_, space)| space.strong_count() > 0);
    if let Some(space) = open.iter().find(|(at, _)| *at == dir).and_then(|(_, space)| space.upgrade()) {
        return Ok(space);
    }
    let space = Arc::new(Space::open(&dir)?);
    open.push((dir, Arc::downgrade(&space)));
    Ok(space)
}
