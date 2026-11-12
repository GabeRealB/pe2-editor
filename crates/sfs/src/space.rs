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
//!   is free once no reader is at a generation that could reach it: one from the generation that wrote the page on,
//!   before the one whose commit let go of it. So a reader held long holds back the pages of the commit it reads,
//!   and none written since. The writer keeps two lists, in files every writer maps, each in pieces that are made
//!   as it grows (`<n>.free`, `<n>.held`, their counts in `free`): the pages free to be taken again, and those
//!   held - each with the generation that wrote it and the one that freed it, in the order they were freed. A
//!   held page is free once the readers that could reach it are gone, and stays so: a reader only ever begins at
//!   the current commit, which reaches nothing freed before it. So those held are looked through only where a
//!   reader that was there when they were last judged is gone, from the first freed after it. The lists are
//!   changed under the writer's lock in an order that a death at any point leaves without a page listed twice: at
//!   worst a page is listed no more, and is not used again until the space is made anew. A writer that dies
//!   inside a transaction has published nothing; the pages it took are past what the last commit says is in use,
//!   or lost that way.
//! - **Readers take no lock**: a reader says the generation it reads at in a cell of its process's (`pins`; said,
//!   then checked that it is still the current one, else said again), which a writer reads once its commit is
//!   published, before it judges what the commit freed. A process says each generation it reads at by itself, up
//!   to `PINS` of them; reading at more, it says the lowest of the rest and counts as reading at every one from
//!   there on. Its cells are its own by a lock on a file (`reader.<n>`), so those of a process that died are
//!   found and cleared.
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
/// How many generations a process says it reads at, each by itself; reading at more, it says the lowest of the
/// rest, and counts as reading at every one from there on (`Readers::from`).
const PINS: usize = 31;
/// How long what the readers of every process say is (`pins`): a process's cells, for each that may attach.
const PINNED: usize = READERS * (PINS + 1) * 8;
/// How long the header of the writer's lists of pages is (`free`): how many each holds, and what readers were at
/// when the held ones were last looked through - a generation for every cell there is, at most.
const LISTS: usize = 1 << 15;
/// A piece of one of the writer's lists: 64 KiB, a file of its own.
const PIECE: usize = 1 << 16;
/// The most pieces a list has: room for more entries than a space has pages.
const PIECES: usize = 2048;
/// What a page begins with: its kind (1), nothing (3), its own number (4), the generation that wrote it (8).
pub const PAGE_HEAD: usize = 16;
/// How often, at most, a writer that readers hold back looks whether their processes still run.
const PROBE: Duration = Duration::from_millis(100);

const MAGIC: u64 = u64::from_le_bytes(*b"pe2spc\x00\x03");

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
    /// How many of its cells each attached process says something in (`pins`): 0, and a writer does not look.
    pub const READERS: usize = 256;
    /// Whether each attached process wrote pages it has not flushed.
    pub const WROTE: usize = 768;
    pub const WHY: usize = 1280;
}

/// Where each value of the header of the writer's lists (`free`) is.
mod listed {
    /// How many pages are free to be taken again.
    pub const FREE: usize = 0;
    /// How many are held: freed, and reached by a reader still when last judged.
    pub const HELD: usize = 8;
    /// Set while what follows is written, while those held are looked through, and where what follows may not be
    /// what they were judged by: found set, they are looked through whole.
    pub const TORN: usize = 16;
    /// What readers were at when those held were last judged (`Readers`): from which generation on every one
    /// counted as read at, plus one - 0 for none; how many generations by themselves; and those, in order.
    pub const FROM: usize = 24;
    pub const SEEN: usize = 32;
    pub const AT: usize = 40;
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
        pins: memmap2::MmapRaw,
        writer: File,
        /// Held shared while the space is attached here; and this process's slot, held alone.
        _attached: File,
        _reader: File,
    },
    Memory {
        chunks: Mutex<Vec<Mem>>,
        head: Mem,
        free: Mem,
        pins: Mem,
    },
}

/// One of the writer's two lists of pages: entries of one length in pieces, each piece a file of the space mapped
/// by itself - memory, of a private one - and made as the list comes to need it. Read and written by who has the
/// writer's lock alone.
struct List {
    /// What its files' names end with.
    name: &'static str,
    /// How many numbers an entry is.
    entry: usize,
    /// Each piece's first byte, once mapped here.
    pieces: Box<[AtomicPtr<u8>]>,
}

impl List {
    fn new(name: &'static str, entry: usize) -> List {
        List { name, entry, pieces: (0..PIECES).map(|_| AtomicPtr::new(std::ptr::null_mut())).collect() }
    }

    /// How many entries a piece holds.
    fn per(&self) -> u64 {
        (PIECE / (self.entry * 8)) as u64
    }
}

/// A generation this process reads at.
struct Reading {
    /// How many read at it.
    readers: usize,
    /// The cell of the process's that says it; none where every cell says another, and it is said by the lowest
    /// of those that have none.
    cell: Option<usize>,
    since: Instant,
}

/// The generations readers are at, as a writer finds them said.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Readers {
    /// Those said by themselves, in order.
    at: Vec<u64>,
    /// The generation from which on every one counts as read at: the lowest a process says for all those it has
    /// no cell for.
    from: Option<u64>,
}

impl Readers {
    /// Whether a reader here may reach a page that one generation wrote and the commit of another let go of: it
    /// reads at one from the first on, before the second.
    fn reach(&self, written: u64, freed: u64) -> bool {
        self.from.is_some_and(|from| from < freed) || {
            let first = self.at.partition_point(|at| *at < written);
            self.at.get(first).is_some_and(|at| *at < freed)
        }
    }

    /// The lowest generation read at here that `now` no longer has a reader at: what was freed after it may be
    /// reached by nobody any more.
    fn left(&self, now: &Readers) -> Option<u64> {
        let from = self.from.filter(|from| now.from.is_none_or(|still| still > *from));
        let at = self.at.iter().copied().find(|at| now.at.binary_search(at).is_err());
        match (from, at) {
            (Some(from), Some(at)) => Some(from.min(at)),
            (from, at) => from.or(at),
        }
    }
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
    /// The generations this process's readers are at.
    pins: Mutex<BTreeMap<u64, Reading>>,
    slot: usize,
    /// The pages free to be taken again: each its number.
    free: List,
    /// Those held: each its number - 0 for none -, the generation that wrote it and the one that freed it, in the
    /// order they were freed.
    held: List,
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

/// Whether a file of a space's folder is one of its pages' - not one of its locks, nor what its readers say.
fn of_pages(name: &str) -> bool {
    ["head", "free", "clean"].contains(&name) || [".chunk", ".free", ".held"].iter().any(|of| name.contains(of))
}

/// The checksum of the writer's lists: their header as far as it says anything, and every entry they hold - of
/// each list's pieces, given by name and number. None where a piece is not there, or not whole.
fn lists_sum(header: &[u8], piece: &mut dyn FnMut(&str, usize) -> Option<Vec<u8>>) -> Option<u32> {
    let seen = usize::try_from(number_at(header, listed::SEEN)).ok()?.min((LISTS - listed::AT) / 8);
    let mut sum = crc32fast::Hasher::new();
    sum.update(header.get(..listed::AT + seen * 8)?);
    for (name, entry, count) in [("free", 8, number_at(header, listed::FREE)), ("held", 24, number_at(header, listed::HELD))] {
        let mut left = usize::try_from(count).ok()?;
        let mut number = 0;
        while left > 0 {
            let here = left.min(PIECE / entry);
            sum.update(piece(name, number)?.get(..here * entry)?);
            left -= here;
            number += 1;
        }
    }
    Some(sum.finalize())
}

/// What `clean` says: the generation the space was had on disk at, and the checksum of its lists of pages.
fn said_clean(generation: u64, lists: u32) -> [u8; 12] {
    let mut said = [0u8; 12];
    said[..8].copy_from_slice(&generation.to_le_bytes());
    said[8..].copy_from_slice(&lists.to_le_bytes());
    said
}

/// Whether the files in `dir` are a space had on disk as it is: `clean` says the generation its header is at - which
/// is whole, the last commit written, not damaged - and its lists of pages are those it was said with.
fn is_clean(dir: &Path) -> bool {
    let (Ok(said), Ok(head), Ok(free)) = (std::fs::read(dir.join("clean")), std::fs::read(dir.join("head")), std::fs::read(dir.join("free")))
    else {
        return false;
    };
    if head.len() < 3 * PAGE || free.len() < LISTS {
        return false;
    }
    let header = Commit::of_header(&head);
    let latest = [1usize, 2].iter().filter_map(|n| Commit::read(&head[n * PAGE..(n + 1) * PAGE])).max_by_key(|c| c.generation);
    let lists = lists_sum(&free, &mut |name, number| std::fs::read(dir.join(format!("{number:08}.{name}"))).ok());
    number_at(&head, at::MAGIC) == MAGIC
        && number_at(&head, at::SEQ) % 2 == 0
        && number_at(&head, at::DAMAGED) == 0
        // A space nothing was ever committed to has no commit written.
        && (latest == Some(header) || (latest.is_none() && header == Commit::default()))
        && lists.is_some_and(|lists| said == said_clean(header.generation, lists))
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
            free: List::new("free", 1),
            held: List::new("held", 3),
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
        let backing = Backing::Memory { chunks: Mutex::default(), head: Mem::zeroed(3 * PAGE)?, free: Mem::zeroed(LISTS)?, pins: Mem::zeroed(PINNED)? };
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
            let free = sized(&dir.join("free"), LISTS as u64)?;
            let free = memmap2::MmapOptions::new().len(LISTS).map_raw(&free)?;
            let pins = sized(&dir.join("pins"), PINNED as u64)?;
            let pins = memmap2::MmapOptions::new().len(PINNED).map_raw(&pins)?;
            let reader = (0..READERS).find_map(|slot| {
                let file = opened(&dir.join(format!("reader.{slot}"))).ok()?;
                file.try_lock().ok().map(|()| (slot, file))
            });
            let (slot, reader) = reader.ok_or_else(|| io::Error::other("every slot of the space is taken"))?;
            Ok::<_, io::Error>((head, free, pins, slot, reader, alone))
        })();
        let (head, free, pins, slot, reader, alone) = match parts {
            Ok(parts) => parts,
            Err(e) => {
                let _ = writer.unlock();
                return Err(e);
            }
        };
        let backing =
            Backing::Files { dir: dir.to_path_buf(), maps: Mutex::default(), head, free, pins, writer, _attached: attached, _reader: reader };
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
                    space.reads_nowhere(slot);
                    space.atom(at::WROTE + slot * 8).store(0, Ordering::Release);
                }
                space.atom(at::CLEAN).store(u64::from(!made), Ordering::Release);
            }
            // Its own slot says nothing yet, whoever had it before.
            space.reads_nowhere(slot);
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
        // The lists of pages, as far as the first transactions write them, and what this process's readers say.
        for page in 0..LISTS / PAGE {
            // SAFETY: within the lists' header.
            touch(unsafe { self.lists().add(page * PAGE) });
        }
        for list in [&self.free, &self.held] {
            let Ok(first) = self.piece(list, 0) else { continue };
            for page in 0..PIECE / PAGE {
                // SAFETY: within the piece.
                touch(unsafe { first.add(page * PAGE) });
            }
        }
        for page in 0..PINNED / PAGE {
            // SAFETY: within what the readers say.
            touch(unsafe { self.pinned_at().add(page * PAGE) });
        }
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

    /// The header of the writer's lists of pages.
    fn lists(&self) -> *mut u8 {
        match &self.backing {
            Backing::Files { free, .. } => free.as_mut_ptr(),
            Backing::Memory { free, .. } => free.at,
        }
    }

    /// One of the values of the lists' header: read and written by who has the writer's lock.
    #[expect(clippy::cast_ptr_alignment, reason = "the header begins a page, and `at` is a multiple of eight")]
    fn list(&self, at: usize) -> &AtomicU64 {
        debug_assert!(at.is_multiple_of(8) && at + 8 <= LISTS);
        // SAFETY: within the header, aligned to eight bytes, valid for as long as `self`.
        unsafe { &*self.lists().add(at).cast::<AtomicU64>() }
    }

    /// A piece of one of the lists: mapped here where it is not yet, and made where there is none.
    fn piece(&self, list: &List, number: usize) -> io::Result<*mut u8> {
        let Some(known) = list.pieces.get(number) else {
            return Err(io::Error::new(io::ErrorKind::StorageFull, "a list of the shared space's pages holds all it may"));
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
                let piece = Mem::zeroed(PIECE)?;
                let first = piece.at;
                chunks.push(piece);
                known.store(first, Ordering::Release);
                Ok(first)
            }
            Backing::Files { dir, maps, .. } => {
                let mut maps = lock(maps);
                let again = known.load(Ordering::Acquire);
                if !again.is_null() {
                    return Ok(again);
                }
                let file = sized(&dir.join(format!("{number:08}.{}", list.name)), PIECE as u64)?;
                let map = memmap2::MmapOptions::new().len(PIECE).map_raw(&file)?;
                let first = map.as_mut_ptr();
                maps.push(map);
                known.store(first, Ordering::Release);
                Ok(first)
            }
        }
    }

    /// An entry of one of the lists, by its place: its numbers, read and written by who has the writer's lock.
    #[expect(clippy::cast_ptr_alignment, reason = "a piece begins a page, and an entry is whole numbers")]
    fn entry(&self, list: &List, index: u64) -> io::Result<&[AtomicU64]> {
        let piece = self.piece(list, usize::try_from(index / list.per()).unwrap_or(usize::MAX))?;
        let within = usize::try_from(index % list.per()).unwrap_or(0) * list.entry * 8;
        // SAFETY: within the piece, which holds `per` entries; aligned to eight bytes; valid for as long as `self`.
        Ok(unsafe { std::slice::from_raw_parts(piece.add(within).cast::<AtomicU64>(), list.entry) })
    }

    /// What the readers of every process say.
    fn pinned_at(&self) -> *mut u8 {
        match &self.backing {
            Backing::Files { pins, .. } => pins.as_mut_ptr(),
            Backing::Memory { pins, .. } => pins.at,
        }
    }

    /// One of a process's cells: the first for the generation from which on it reads at every one, the others
    /// for a generation each - plus one; 0 for none.
    #[expect(clippy::cast_ptr_alignment, reason = "the cells begin a page, eight bytes each")]
    fn cell(&self, slot: usize, cell: usize) -> &AtomicU64 {
        debug_assert!(slot < READERS && cell <= PINS);
        // SAFETY: within the cells, aligned to eight bytes, valid for as long as `self`; every process reads and
        // writes them as atomics.
        unsafe { &*self.pinned_at().add((slot * (PINS + 1) + cell) * 8).cast::<AtomicU64>() }
    }

    /// A process's cells say nothing: it reads nowhere.
    fn reads_nowhere(&self, slot: usize) {
        for cell in 0..=PINS {
            self.cell(slot, cell).store(0, Ordering::SeqCst);
        }
        self.atom(at::READERS + slot * 8).store(0, Ordering::SeqCst);
    }

    /// The generations readers are at now, as every process says them.
    fn readers(&self) -> Readers {
        let mut readers = Readers::default();
        for slot in 0..READERS {
            if self.atom(at::READERS + slot * 8).load(Ordering::SeqCst) == 0 {
                continue;
            }
            if let Some(from) = self.cell(slot, 0).load(Ordering::SeqCst).checked_sub(1) {
                readers.from = Some(readers.from.map_or(from, |lowest| lowest.min(from)));
            }
            readers.at.extend((1..=PINS).filter_map(|cell| self.cell(slot, cell).load(Ordering::SeqCst).checked_sub(1)));
        }
        readers.at.sort_unstable();
        readers.at.dedup();
        readers
    }

    /// What readers were at when the held pages were last judged, as the lists' header says.
    fn seen(&self) -> Readers {
        let count = usize::try_from(self.list(listed::SEEN).load(Ordering::Acquire)).unwrap_or(0).min((LISTS - listed::AT) / 8);
        Readers {
            at: (0..count).map(|i| self.list(listed::AT + i * 8).load(Ordering::Acquire)).collect(),
            from: self.list(listed::FROM).load(Ordering::Acquire).checked_sub(1),
        }
    }

    /// Say what the held pages are judged by from now on - torn while it is said, so that a writer stopped here
    /// leaves them to be looked through whole.
    fn see(&self, readers: &Readers) {
        self.list(listed::TORN).store(1, Ordering::Release);
        self.fault("free: what readers were at half said");
        let count = readers.at.len().min((LISTS - listed::AT) / 8);
        self.list(listed::FROM).store(readers.from.map_or(0, |from| from + 1), Ordering::Release);
        for (i, at) in readers.at[..count].iter().enumerate() {
            self.list(listed::AT + i * 8).store(*at, Ordering::Release);
        }
        self.list(listed::SEEN).store(count as u64, Ordering::Release);
        self.list(listed::TORN).store(0, Ordering::Release);
    }

    /// The pages listed free, and those held - each with the generation that wrote it and the one that freed
    /// it -, as the lists hold them.
    #[cfg(test)]
    fn listed_pages(&self) -> (Vec<u64>, Vec<(u64, u64, u64)>) {
        let _held = self.write().unwrap();
        let free = (0..self.list(listed::FREE).load(Ordering::Acquire))
            .map(|at| self.entry(&self.free, at).unwrap()[0].load(Ordering::Acquire))
            .filter(|number| *number != 0);
        let held = (0..self.list(listed::HELD).load(Ordering::Acquire)).map(|at| {
            let entry = self.entry(&self.held, at).unwrap();
            (entry[0].load(Ordering::Acquire), entry[1].load(Ordering::Acquire), entry[2].load(Ordering::Acquire))
        });
        (free.collect(), held.filter(|(number, ..)| *number != 0).collect())
    }

    /// The lowest generation this process reads at, and for how long it has: what a view held long is told by.
    /// It holds back the pages of the commit it reads, and nothing written since.
    #[must_use]
    pub fn reading_longest(&self) -> Option<(u64, Duration)> {
        lock(&self.pins).iter().next().map(|(generation, reading)| (*generation, reading.since.elapsed()))
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
        if let Some(reading) = pins.get_mut(&generation) {
            reading.readers += 1;
            return;
        }
        let cell = (0..PINS).find(|cell| !pins.values().any(|reading| reading.cell == Some(*cell)));
        // How many cells say something goes up before one does: a writer that finds none said does not look.
        let said = self.atom(at::READERS + self.slot * 8);
        if let Some(cell) = cell {
            said.fetch_add(1, Ordering::SeqCst);
            self.cell(self.slot, 1 + cell).store(generation + 1, Ordering::SeqCst);
        } else {
            // Every cell says another: said by the lowest of those that have none, from which on this process
            // counts as reading at every generation.
            let lowest = pins.iter().find(|(_, reading)| reading.cell.is_none()).map_or(generation, |(lowest, _)| generation.min(*lowest));
            let from = self.cell(self.slot, 0);
            if from.load(Ordering::SeqCst) == 0 {
                said.fetch_add(1, Ordering::SeqCst);
            }
            from.store(lowest + 1, Ordering::SeqCst);
        }
        pins.insert(generation, Reading { readers: 1, cell, since: Instant::now() });
    }

    fn unpin(&self, generation: u64) {
        let mut pins = lock(&self.pins);
        let Some(reading) = pins.get_mut(&generation) else { return };
        reading.readers -= 1;
        if reading.readers > 0 {
            return;
        }
        let cell = pins.remove(&generation).and_then(|reading| reading.cell);
        let said = self.atom(at::READERS + self.slot * 8);
        if let Some(cell) = cell {
            // Its cell goes to the lowest of those that have none - said there before it is said nowhere else.
            if let Some((waiting, reading)) = pins.iter_mut().find(|(_, reading)| reading.cell.is_none()) {
                reading.cell = Some(cell);
                self.cell(self.slot, 1 + cell).store(*waiting + 1, Ordering::SeqCst);
            } else {
                self.cell(self.slot, 1 + cell).store(0, Ordering::SeqCst);
                said.fetch_sub(1, Ordering::SeqCst);
            }
        }
        let from = self.cell(self.slot, 0);
        if from.load(Ordering::SeqCst) != 0 {
            if let Some((lowest, _)) = pins.iter().find(|(_, reading)| reading.cell.is_none()) {
                from.store(*lowest + 1, Ordering::SeqCst);
            } else {
                from.store(0, Ordering::SeqCst);
                said.fetch_sub(1, Ordering::SeqCst);
            }
        }
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

    /// Clear what the readers of processes that are gone say.
    fn clear_gone(&self) {
        for slot in 0..READERS {
            if self.atom(at::READERS + slot * 8).load(Ordering::SeqCst) != 0 && self.gone(slot) {
                self.reads_nowhere(slot);
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
                Ok(Txn { space: self, writing, from, commit, own: HashSet::new(), let_go: Vec::new(), spare: Vec::new(), looked: false, touched: false, done: false })
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
        // SAFETY: the lists' header, whole; nobody writes it but who has the writer's lock, held here.
        let header = unsafe { std::slice::from_raw_parts(self.lists(), LISTS) }.to_vec();
        let lists = lists_sum(&header, &mut |name, number| {
            let list = if name == self.free.name { &self.free } else { &self.held };
            // SAFETY: a piece, whole, read under the writer's lock.
            self.piece(list, number).ok().map(|piece| unsafe { std::slice::from_raw_parts(piece, PIECE) }.to_vec())
        });
        let lists = lists.ok_or_else(|| io::Error::other("the lists of the shared space's pages cannot be read"))?;
        std::fs::write(&clean, said_clean(held.from.generation, lists))?;
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
    /// free, and how many of these held for readers.
    ///
    /// # Errors
    /// As `write`.
    pub fn usage(&self) -> Result<Usage> {
        let held = self.write()?;
        let held_back = self.list(listed::HELD).load(Ordering::Acquire);
        let free = self.list(listed::FREE).load(Ordering::Acquire) + held_back;
        Ok(Usage { generation: held.from.generation, pages: held.from.high.saturating_sub(1), free, held: held_back })
    }
}

/// How much of a space is in use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Usage {
    pub generation: u64,
    /// The pages ever written.
    pub pages: u64,
    /// Those of them no commit reaches any more: free to be taken again, or held.
    pub free: u64,
    /// Those of the free ones held: reached by a reader still, when last judged.
    pub held: u64,
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
    /// The pages it reaches no more, free once it is committed and no reader can reach them.
    let_go: Vec<u64>,
    /// Pages it made and reaches no more: the first it takes again, and free at once when it ends.
    spare: Vec<u64>,
    /// Whether it looked, for want of a free page, whether those held are reached still.
    looked: bool,
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
        // One that is free, where there is one; else one never written - never 0, which no pointer leads to.
        let number = match self.spare.pop().or_else(|| self.take_free()) {
            Some(number) => number,
            None => self.commit.high.max(1),
        };
        // SAFETY: a page nobody reaches: free, or never written.
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

    /// Take a free page: the last listed, said one fewer before it is anyone's - a writer stopped between leaves
    /// it listed no more. Where none is free and some are held, those are looked through once, for what the
    /// readers that are gone since no longer reach.
    fn take_free(&mut self) -> Option<u64> {
        let space = self.space;
        loop {
            let count = space.list(listed::FREE).load(Ordering::Acquire);
            if count > 0 {
                let number = space.entry(&space.free, count - 1).ok()?[0].load(Ordering::Acquire);
                space.list(listed::FREE).store(count - 1, Ordering::Release);
                space.fault("free: one fewer said");
                // No page is 0: an entry that says so is none.
                if number != 0 {
                    return Some(number);
                }
                continue;
            }
            if self.looked || space.list(listed::HELD).load(Ordering::Acquire) == 0 {
                return None;
            }
            self.looked = true;
            if self.writing.is_none_or(|probed| probed.elapsed() >= PROBE) {
                // Readers hold every one back: those whose processes are gone, no longer.
                *self.writing = Some(Instant::now());
                space.clear_gone();
            }
            self.settled();
        }
    }

    /// The readers there are now, with the held pages brought to them: those none of them reaches any more are
    /// free. Where that fails, the held ones are left to be looked through whole.
    fn settled(&mut self) -> Readers {
        let now = self.space.readers();
        if self.settle(&now).is_err() {
            self.space.list(listed::TORN).store(1, Ordering::Release);
        }
        now
    }

    /// Bring the held pages to the readers there are now. They were judged by those there were when it was last
    /// done (`seen`), and none came to be reached since - a reader begins at the current commit, which reaches
    /// nothing that was freed. So they are looked through only where a reader of then is gone: from the first
    /// freed after the lowest generation that one read at, the ones before it being held by readers before it.
    fn settle(&mut self, now: &Readers) -> io::Result<()> {
        let space = self.space;
        let held = space.list(listed::HELD).load(Ordering::Acquire);
        let seen = (space.list(listed::TORN).load(Ordering::Acquire) == 0).then(|| space.seen());
        let mut looked = false;
        if held != 0 {
            let from = match &seen {
                None => Some(0),
                Some(seen) => match seen.left(now) {
                    Some(gone) => Some(self.held_after(gone, held)?),
                    None => None,
                },
            };
            if let Some(from) = from {
                self.look_through(from, held, now)?;
                looked = true;
            }
        }
        if looked || seen.as_ref() != Some(now) {
            space.see(now);
        }
        Ok(())
    }

    /// The place of the first held page freed after a generation: they are in the order they were freed.
    fn held_after(&self, generation: u64, held: u64) -> io::Result<u64> {
        let (mut low, mut high) = (0, held);
        while low < high {
            let middle = low + (high - low) / 2;
            if self.space.entry(&self.space.held, middle)?[2].load(Ordering::Acquire) <= generation {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        Ok(low)
    }

    /// Look through the held pages from a place on: one no reader reaches any more is free, the others move up
    /// to where those were, in their order. Each is taken out of where it is before it is anywhere else, so that
    /// a writer stopped between leaves it listed no more, never twice - and the list said torn until it is whole
    /// again, since stopped here it is not in its order.
    fn look_through(&mut self, from: u64, held: u64, now: &Readers) -> io::Result<()> {
        let space = self.space;
        space.list(listed::TORN).store(1, Ordering::Release);
        let mut kept = from;
        for at in from..held {
            let entry = space.entry(&space.held, at)?;
            let number = entry[0].load(Ordering::Acquire);
            if number == 0 {
                continue;
            }
            let (written, freed) = (entry[1].load(Ordering::Acquire), entry[2].load(Ordering::Acquire));
            let reached = now.reach(written, freed);
            if reached && at == kept {
                kept += 1;
                continue;
            }
            entry[0].store(0, Ordering::Release);
            space.fault("free: one held taken out");
            if reached {
                let to = space.entry(&space.held, kept)?;
                to[1].store(written, Ordering::Release);
                to[2].store(freed, Ordering::Release);
                to[0].store(number, Ordering::Release);
                kept += 1;
            } else {
                self.list_free(number)?;
            }
        }
        space.list(listed::HELD).store(kept, Ordering::Release);
        Ok(())
    }

    /// List a page free. The entry is written before the count says it is there.
    fn list_free(&mut self, number: u64) -> io::Result<()> {
        let space = self.space;
        let count = space.list(listed::FREE).load(Ordering::Acquire);
        space.entry(&space.free, count)?[0].store(number, Ordering::Release);
        space.fault("free: an entry written");
        space.list(listed::FREE).store(count + 1, Ordering::Release);
        Ok(())
    }

    /// List a page held, after those freed before it: by the generation that wrote it and the one whose commit
    /// let go of it. The entry is written before the count says it is there.
    fn list_held(&mut self, number: u64, written: u64, freed: u64) -> io::Result<()> {
        let space = self.space;
        let count = space.list(listed::HELD).load(Ordering::Acquire);
        let entry = space.entry(&space.held, count)?;
        entry[1].store(written, Ordering::Release);
        entry[2].store(freed, Ordering::Release);
        entry[0].store(number, Ordering::Release);
        space.fault("free: one held");
        space.list(listed::HELD).store(count + 1, Ordering::Release);
        Ok(())
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
        // What it reaches no more is free where no reader can reach it: none reads at a generation from the one
        // that wrote the page on, before this one. Judged by the readers there are once this is published - one
        // that says itself after reads this commit, or finds it is no longer at the current one. A page that
        // cannot be listed - no room for the list - is listed no more.
        let now = self.settled();
        let any = now.from.is_some() || !now.at.is_empty();
        for page in std::mem::take(&mut self.let_go) {
            let written = if any {
                // SAFETY: the head of a page this transaction reached, which nobody writes.
                let Ok(head) = self.space.page_at(page, false).map(|at| unsafe { std::slice::from_raw_parts(at, PAGE_HEAD) }) else {
                    continue;
                };
                number_at(head, 8)
            } else {
                0
            };
            let _ = if now.reach(written, commit.generation) {
                self.list_held(page, written, commit.generation)
            } else {
                self.list_free(page)
            };
        }
        // What it made and reaches no more was never reached by anyone.
        for page in std::mem::take(&mut self.spare) {
            let _ = self.list_free(page);
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
                let _ = self.list_free(page);
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
