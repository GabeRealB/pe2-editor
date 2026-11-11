//! What whoever holds the project writes to its files goes through the log the project's processes share
//! (`sfs::storage`): a set of changes is said there whole, and had on disk, before any file is touched. So a process
//! that dies between - or a loss of power - leaves the files to be written again from the log (`redo`, by whoever
//! takes the project next), never half a set with nothing to tell what the rest was; and a set that never came to
//! be whole touched no file.
//!
//! The files themselves are written with no wait for the disk. Every few seconds, and as whoever holds the project
//! lets go of it, what was written is had on disk - by a thread of its own, so that nobody making a change or
//! drawing a frame waits for it - and the log is needed for it no longer: the header says from where it still is
//! (`sfs::Header::storage`), which who lets go of the log reads.
//!
//! A set that changes only records of records layers is **written behind** (`Storage::say_later`): said in the log
//! and in the space the project's processes share - each file it writes or removes kept there as waiting, by its
//! path - and that is all its commit waits for. The thread then, for all sets said since it last did, has on disk
//! what they name in the store, the header's word that the log is needed, and the log; writes their files; and
//! takes them out of what waits. Whoever reads such a file meanwhile - whoever holds the project taking in its own
//! change, the server reading a record's text - asks what waits first (`waiting`), in whichever process. A file
//! that is not as the set found it when its turn comes - changed by someone else in between - is not written over:
//! said, ours kept aside (`.cache/conflicts`), and the file left for the project to take in as it is. A set that
//! changes anything else waits for all that is behind to be written, and is written as it is said.

use anyhow::{Context, Result};
use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, RwLock};
use std::time::Duration;

pub use sfs::Stored;

/// How long what was written may wait to be had on disk, while more is written.
const EVERY: Duration = Duration::from_secs(5);

/// The log's folder, in the project's.
fn log_dir(root: &Path) -> PathBuf {
    root.join(crate::cache::DIR).join(crate::cache::STAGING).join("sfs")
}

/// The tree of the shared space that holds what waits to be written: a file's path in the project, to the set
/// that wrote or removed it and what it holds.
const WAITING: usize = 8;
/// The longest path a file that waits is kept under.
const WAITING_PATH: usize = 700;

/// The projects whose files this process reads through what waits to be written, each with its shared space.
static READ_THROUGH: RwLock<Vec<(PathBuf, Arc<sfs::Space>)>> = RwLock::new(Vec::new());

/// Read the files of the project in `root` through what waits to be written of them, in this process, from now
/// on: what the server does as it starts, and whoever holds the project as it opens its storage.
pub fn read_through(root: &Path, space: Arc<sfs::Space>) {
    let mut read = READ_THROUGH.write().unwrap_or_else(PoisonError::into_inner);
    read.retain(|(of, _)| of != root);
    read.push((root.to_path_buf(), space));
}

fn name_in(root: &Path, path: &Path) -> Option<String> {
    Some(path.strip_prefix(root).ok()?.to_string_lossy().replace('\\', "/"))
}

/// What waits of a file, as the shared space keeps it: the set that said it (8), when - in microseconds since
/// 1970 (8) - and whether it is written (1, then its bytes) or removed (0).
fn waits_as(began: u64, file: &Behind) -> Vec<u8> {
    let at = file.at.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_micros();
    let mut value = began.to_le_bytes().to_vec();
    value.extend_from_slice(&u64::try_from(at).unwrap_or(0).to_le_bytes());
    match &file.after {
        Some(bytes) => {
            value.push(1);
            value.extend_from_slice(bytes);
        }
        None => value.push(0),
    }
    value
}

/// What `waits_as` wrote, read: when it was said, and the file's bytes - none where it is removed.
fn waited(value: &[u8]) -> Option<(std::time::SystemTime, Option<Vec<u8>>)> {
    let (_, rest) = value.split_first_chunk::<8>()?;
    let (at, what) = rest.split_first_chunk::<8>()?;
    let at = std::time::UNIX_EPOCH + Duration::from_micros(u64::from_le_bytes(*at));
    match what.split_first()? {
        (1, bytes) => Some((at, Some(bytes.to_vec()))),
        _ => Some((at, None)),
    }
}

/// A time as a file's own can say it on every system this runs on: to the microsecond.
#[must_use]
pub fn stamped(time: std::time::SystemTime) -> std::time::SystemTime {
    let since = time.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    std::time::UNIX_EPOCH + Duration::from_micros(u64::try_from(since.as_micros()).unwrap_or(0))
}

/// What waits to be written of a file: when it was said - the time its file is given once written - and its
/// bytes, or none where it is removed. None where nothing of it waits - it is as the disk has it - or this process
/// reads its project through nothing.
#[must_use]
pub fn waiting_since(path: &Path) -> Option<(std::time::SystemTime, Option<Vec<u8>>)> {
    let read = READ_THROUGH.read().unwrap_or_else(PoisonError::into_inner);
    let (root, space) = read.iter().find(|(root, _)| path.starts_with(root))?;
    let pages = space.read().ok()?;
    if sfs::space::Pages::root(&pages, WAITING) == 0 {
        return None;
    }
    waited(&sfs::tree::get(&pages, WAITING, name_in(root, path)?.as_bytes()).ok()??)
}

/// What a file holds as what waits to be written leaves it: its bytes, or none where it is removed. None where
/// nothing of it waits.
#[must_use]
pub fn waiting(path: &Path) -> Option<Option<Vec<u8>>> {
    waiting_since(path).map(|(_, bytes)| bytes)
}

/// The files under a folder that wait to be written, each with when it was said and what it holds, or none where
/// it is removed.
#[must_use]
pub fn waiting_under(folder: &Path) -> Vec<(PathBuf, std::time::SystemTime, Option<Vec<u8>>)> {
    let read = READ_THROUGH.read().unwrap_or_else(PoisonError::into_inner);
    let Some((root, space)) = read.iter().find(|(root, _)| folder.starts_with(root)) else { return Vec::new() };
    let (Ok(pages), Some(inside)) = (space.read(), name_in(root, folder).map(|name| format!("{name}/"))) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    let _ = sfs::tree::scan(&pages, WAITING, inside.as_bytes(), &mut |key, value| {
        let Some(name) = std::str::from_utf8(key).ok().filter(|name| name.starts_with(&inside)) else { return false };
        if let Some((at, bytes)) = waited(value) {
            found.push((root.join(name), at, bytes));
        }
        true
    });
    found
}

/// Whether a folder is there for what waits to be written in it, though the disk may not have it yet.
#[must_use]
pub fn folder_waits(folder: &Path) -> bool {
    let read = READ_THROUGH.read().unwrap_or_else(PoisonError::into_inner);
    let Some((root, space)) = read.iter().find(|(root, _)| folder.starts_with(root)) else { return false };
    let (Ok(pages), Some(inside)) = (space.read(), name_in(root, folder).map(|name| format!("{name}/"))) else {
        return false;
    };
    let mut waits = false;
    let _ = sfs::tree::scan(&pages, WAITING, inside.as_bytes(), &mut |key, value| {
        // A file written in it; one removed there makes no folder.
        waits = key.starts_with(inside.as_bytes()) && value.get(16) == Some(&1);
        !waits && key.starts_with(inside.as_bytes())
    });
    waits
}

/// A file read as what waits to be written leaves it, else as the disk has it; none where it is absent.
///
/// # Errors
/// It is there and cannot be read.
pub fn read(path: &Path) -> std::io::Result<Option<Vec<u8>>> {
    if let Some(waits) = waiting(path) {
        return Ok(waits);
    }
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// A file's length and time of change, as what waits to be written leaves it: what its file will say once it is
/// written, for whoever is to know that writing as its own. None where it is absent.
#[must_use]
pub fn stamp(path: &Path) -> Option<(u64, std::time::SystemTime)> {
    match waiting_since(path) {
        Some((at, Some(bytes))) => Some((bytes.len() as u64, at)),
        Some((_, None)) => None,
        None => {
            let meta = std::fs::metadata(path).ok()?;
            Some((meta.len(), meta.modified().ok()?))
        }
    }
}

/// Whether a file is there, as what waits to be written leaves it.
#[must_use]
pub fn is_file(path: &Path) -> bool {
    waiting(path).map_or_else(|| path.is_file(), |waits| waits.is_some())
}

/// A file of a set written behind: its path, what it comes to hold (none: removed), and the hash of what the set
/// found it holding (none: absent) - what it has to hold still when it is written.
#[derive(Clone, Debug)]
pub struct Behind {
    pub path: PathBuf,
    pub after: Option<Vec<u8>>,
    pub before: Option<String>,
    /// A folder to make, in place of a file.
    pub folder: bool,
    /// When the set was said: the time the file is given once written, so that who read it while it waited finds
    /// the file written to be the one it read.
    pub at: std::time::SystemTime,
}

/// What the holder of a project writes to its files, said in the log first.
pub struct Storage {
    root: PathBuf,
    shared: Arc<Shared>,
    /// What has on disk what was written, off the threads that write.
    syncing: Option<std::thread::JoinHandle<()>>,
}

struct Shared {
    root: PathBuf,
    log: Arc<sfs::Log>,
    /// The space what waits to be written is kept in, for every process to read, attached when it is first
    /// needed - by a set that can be written behind, or asked for ahead of one (`Storage::prepare`); none where
    /// it cannot be had - then nothing is written behind.
    space: std::sync::OnceLock<Option<Arc<sfs::Space>>>,
    /// Whether room for what waits first is to be made ahead of it (`Storage::prepare`).
    prepare: std::sync::atomic::AtomicBool,
    state: Mutex<State>,
    wake: Condvar,
    /// Whether the header's word that the log is needed has yet to be had on disk.
    header_unflushed: std::sync::atomic::AtomicBool,
    /// Whether what is to be written behind is held back for now (`Storage::hold_back`).
    held_back: std::sync::atomic::AtomicBool,
}

#[derive(Default)]
struct State {
    /// What was written since it was last had on disk.
    unsynced: BTreeSet<PathBuf>,
    /// Where the first set began whose files are not all on disk: from there the log is needed.
    since: Option<u64>,
    /// The set said whole whose files are being written this moment.
    open: Option<u64>,
    /// How many times what was written was had on disk, and whether that is asked for now.
    synced: u64,
    asked: bool,
    stopping: bool,
    /// The sets said and not written yet, in the order they were said: where each began, and its files.
    behind: VecDeque<(u64, String, Vec<Behind>)>,
    /// Whether the sets taken from `behind` are being written this moment.
    writing: bool,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The shared space, attached where it is not yet: read through by this process from then on.
    fn space(&self) -> Option<&Arc<sfs::Space>> {
        let attach = || {
            let root = &self.root;
            let space = crate::shared::space(root)
                .inspect_err(|e| eprintln!("{}: nothing is written behind, the shared space cannot be had: {e}", root.display()))
                .ok()?;
            read_through(root, space.clone());
            Some(space)
        };
        self.space.get_or_init(attach).as_ref()
    }

    /// Have on disk what was written by now, and say from where the log is still needed: where the first set
    /// began that was said since - its files are not in what was taken - or for nothing.
    fn checkpoint(&self) {
        let paths = {
            let mut state = self.lock();
            // A set whose files are being written is not on disk by this: the log is needed from it on.
            state.since = state.open;
            std::mem::take(&mut state.unsynced)
        };
        sync(&paths);
        let mut state = self.lock();
        self.log.header().set_storage(state.since);
        state.synced += 1;
        state.asked = false;
        drop(state);
        // Whether anything serves the project or not: what nobody needs of the log goes.
        self.log.keep();
        self.wake.notify_all();
    }

    /// Write what was said to be written behind: for all the sets said since this last did - what they name in
    /// the store, the header and the log had on disk, then their files written and taken out of what waits.
    fn write_behind(&self) {
        let sets: Vec<(u64, String, Vec<Behind>)> = {
            let mut state = self.lock();
            if state.behind.is_empty() || self.held_back.load(std::sync::atomic::Ordering::Acquire) {
                return;
            }
            state.writing = true;
            state.behind.drain(..).collect()
        };
        crate::chunks::sync_written(&self.root);
        let flushed = (|| {
            if self.header_unflushed.swap(false, std::sync::atomic::Ordering::AcqRel) {
                self.log.header().flush()?;
            }
            self.log.flush(self.log.header().tail())
        })();
        let mut written = BTreeSet::new();
        match flushed {
            // Not on disk, the sets are not to reach the files: said, and tried again with what comes next.
            Err(e) => {
                eprintln!("{}: what was changed could not be had on disk in the log, and waits: {e}", self.root.display());
                let mut state = self.lock();
                for set in sets.into_iter().rev() {
                    state.behind.push_front(set);
                }
                state.writing = false;
                drop(state);
                self.wake.notify_all();
                std::thread::sleep(Duration::from_millis(200));
                return;
            }
            Ok(()) => {
                for (_, label, files) in &sets {
                    for file in files {
                        self.write_one(label, file);
                        written.insert(file.path.clone());
                    }
                }
            }
        }
        // Written: no longer what waits - but what a set said since put there in its place.
        if let Some(space) = self.space() {
            let taken = (|| {
                let mut txn = space.write()?;
                for (began, _, files) in &sets {
                    for file in files.iter().filter(|file| !file.folder) {
                        let Some(name) = name_in(&self.root, &file.path) else { continue };
                        let ours = sfs::tree::get(&txn, WAITING, name.as_bytes())?
                            .is_some_and(|value| value.first_chunk::<8>() == Some(&began.to_le_bytes()));
                        if ours {
                            sfs::tree::remove(&mut txn, WAITING, name.as_bytes())?;
                        }
                    }
                }
                txn.commit()
            })();
            if let Err(e) = taken {
                eprintln!("{}: what was written could not be taken out of what waits: {e}", self.root.display());
            }
        }
        let mut state = self.lock();
        state.unsynced.extend(written);
        state.writing = false;
        // The log is needed from the first set still to be written, or - all written - from none said since the
        // files were last had on disk.
        state.open = state.behind.front().map(|(began, _, _)| *began);
        drop(state);
        self.wake.notify_all();
    }

    /// Write one file of a set written behind - unless it is not as the set found it: then someone else changed
    /// it meanwhile, and it is theirs. Ours is kept aside, and the file touched, for whoever holds the project to
    /// take it in as it is.
    fn write_one(&self, label: &str, file: &Behind) {
        if file.folder {
            if !file.path.is_dir()
                && let Err(e) = std::fs::create_dir_all(&file.path)
            {
                eprintln!("{}: {label}: the folder could not be made: {e}", file.path.display());
            }
            return;
        }
        let now = std::fs::read(&file.path).ok().map(|bytes| crate::extract::sha1_hex(&bytes));
        // As the set leaves it already - written again from the log by whoever took the project meanwhile.
        if now != file.before && now == file.after.as_ref().map(|bytes| crate::extract::sha1_hex(bytes)) {
            return;
        }
        if now != file.before {
            let aside = self.root.join(crate::cache::DIR).join("conflicts").join(name_in(&self.root, &file.path).unwrap_or_default());
            let kept = file.after.as_ref().map(|bytes| {
                aside.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(&aside, bytes))
            });
            eprintln!(
                "{}: changed by something else while `{label}` waited to be written: left as it is{}",
                file.path.display(),
                match kept {
                    Some(Ok(())) => format!(", what `{label}` made of it kept in {}", aside.display()),
                    Some(Err(e)) => format!(", and what `{label}` made of it lost: {e}"),
                    None => String::new(),
                }
            );
            // Looked at again by whoever watches the project's files.
            if let Ok(theirs) = std::fs::File::options().write(true).open(&file.path) {
                let _ = theirs.set_modified(std::time::SystemTime::now());
            }
            return;
        }
        let done = match &file.after {
            Some(bytes) => crate::cache::write_atomic(&file.path, bytes).map_err(|e| format!("{e:#}")).map(|()| {
                if let Ok(written) = std::fs::File::options().write(true).open(&file.path) {
                    let _ = written.set_modified(file.at);
                }
            }),
            None => match std::fs::remove_file(&file.path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
                _ => Ok(()),
            },
        };
        if let Err(e) = done {
            // Said whole in the log: written again from it by whoever takes the project next.
            eprintln!("{}: `{label}` could not be written, and is written again from the log later: {e}", file.path.display());
        }
    }

    /// Every few seconds while anything waits to be had on disk, and whenever it is asked for, until stopped -
    /// then once more, for what was written last. What is to be written behind is written as it comes.
    fn run(&self) {
        // When what was written is next had on disk: every few seconds, however much is written behind meanwhile.
        let mut due = std::time::Instant::now() + EVERY;
        loop {
            self.write_behind();
            let (stopping, checkpoint) = {
                let mut state = self.lock();
                let held_back = || self.held_back.load(std::sync::atomic::Ordering::Acquire);
                let prepare = || self.prepare.load(std::sync::atomic::Ordering::Acquire);
                while !state.asked && !state.stopping && !prepare() && (state.behind.is_empty() || held_back()) {
                    let left = due.saturating_duration_since(std::time::Instant::now());
                    if left.is_zero() {
                        break;
                    }
                    state = self.wake.wait_timeout(state, left).unwrap_or_else(PoisonError::into_inner).0;
                }
                (state.stopping, state.asked || state.stopping || due <= std::time::Instant::now())
            };
            // Room for what waits first, made here: the first change does not wait for it.
            if self.prepare.swap(false, std::sync::atomic::Ordering::AcqRel)
                && !stopping
                && let Some(space) = self.space()
            {
                space.ready();
            }
            if !checkpoint {
                // Woken for what is to be written behind: that, and the wait goes on.
                continue;
            }
            self.write_behind();
            due = std::time::Instant::now() + EVERY;
            let waits = !self.lock().unsynced.is_empty() || self.log.header().storage().is_some();
            if waits {
                self.checkpoint();
            } else {
                let mut state = self.lock();
                state.synced += 1;
                state.asked = false;
                drop(state);
                self.wake.notify_all();
            }
            if stopping {
                return;
            }
        }
    }
}

impl std::fmt::Debug for Storage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let waiting = self.shared.lock().unsynced.len();
        write!(f, "storage of {} ({waiting} paths not on disk yet)", self.root.display())
    }
}

impl Storage {
    /// The storage of the project in `root`; none where its log cannot be opened - what is written is then written
    /// as it comes, with nothing to take it up from.
    #[must_use]
    pub fn open(root: &Path) -> Option<Storage> {
        let log = sfs::Log::shared(&log_dir(root), sfs::log::Config::default());
        let log = log.inspect_err(|e| eprintln!("{}: no log to write through: {e}", root.display())).ok()?;
        let shared = Arc::new(Shared {
            root: root.to_path_buf(),
            log,
            space: std::sync::OnceLock::new(),
            prepare: std::sync::atomic::AtomicBool::new(false),
            state: Mutex::default(),
            wake: Condvar::new(),
            header_unflushed: std::sync::atomic::AtomicBool::new(false),
            held_back: std::sync::atomic::AtomicBool::new(false),
        });
        let syncing = {
            let shared = shared.clone();
            std::thread::Builder::new().name("storage".into()).spawn(move || shared.run()).ok()
        };
        Some(Storage { root: root.to_path_buf(), shared, syncing })
    }

    /// A path as the log says it: within the project, from its root; elsewhere - a tree mounted from another
    /// place - whole.
    #[must_use]
    pub fn name(&self, path: &Path) -> String {
        path.strip_prefix(&self.root).unwrap_or(path).to_string_lossy().replace('\\', "/")
    }

    /// Say a set of changes whole, and have it on disk: where it began. Only then are its files to be touched -
    /// and said written once they are (`wrote`), or given up (`gave_up`).
    ///
    /// # Errors
    /// The log cannot take it, or cannot be had on disk: no file is to be touched.
    pub fn say(&mut self, label: &str, changes: &[Stored]) -> std::io::Result<u64> {
        let log = self.shared.log.clone();
        // In the order they were said: what was to be written behind is written before this is.
        self.settle();
        // What the store was given for these changes is on disk before the records that name it.
        crate::chunks::sync_written(&self.root);
        let began = match self.begin(false) {
            // As much in the log as it may hold, some of it this side's: had on disk, it is let go of.
            Err(e) if e.kind() == std::io::ErrorKind::StorageFull => {
                self.checkpoint();
                self.begin(false)?
            }
            began => began?,
        };
        let said = (|| {
            for change in changes {
                match change {
                    Stored::Put { path, bytes } => sfs::storage::put(&log, path, bytes)?,
                    change => drop(change.append(&log)?),
                }
            }
            Stored::Whole { began, label: label.to_string() }.append(&log)?;
            log.flush(log.header().tail())
        })();
        if let Err(e) = said {
            let mut state = self.shared.lock();
            state.open = state.behind.front().map(|(began, _, _)| *began);
            return Err(e);
        }
        Ok(began)
    }

    /// Begin a set: the header says that the log is needed from here for what is written to the files, before
    /// the first record of it - on disk now, or (`later`) by the thread before it writes any file of the set.
    fn begin(&self, later: bool) -> std::io::Result<u64> {
        let log = &self.shared.log;
        let mut state = self.shared.lock();
        let header = log.header();
        let unflushed = &self.shared.header_unflushed;
        if header.storage().is_none() {
            header.set_storage(Some(header.tail()));
            unflushed.store(true, std::sync::atomic::Ordering::Release);
        }
        if !later && unflushed.swap(false, std::sync::atomic::Ordering::AcqRel) {
            header.flush()?;
        }
        let began = sfs::storage::began(log)?;
        state.since.get_or_insert(began);
        state.open.get_or_insert(began);
        Ok(began)
    }

    /// Whether a set of these files can be written behind: there is where to keep what waits, and each is a record
    /// of a records layer - or the folder one is in - under a path short enough to keep it by.
    #[must_use]
    pub fn defers(&self, files: &[Behind]) -> bool {
        let record = |file: &Behind| {
            let shard = if file.folder { Some(file.path.as_path()) } else { file.path.parent() };
            let records = shard.filter(|shard| shard.file_name().is_some_and(|name| name.len() == 2)).and_then(Path::parent);
            let named = file.path.starts_with(&self.root) && self.name(&file.path).len() <= WAITING_PATH;
            named && records.is_some_and(|records| records.file_name().is_some_and(|name| name == crate::records::DIR))
        };
        // The space is asked for last: what holds a project and never changes a record attaches none.
        !files.is_empty() && files.iter().all(record) && self.shared.space().is_some()
    }

    /// Have what writing behind needs made ahead of the first change that is: the shared space attached and
    /// given room, off this thread. For whoever holds a project to stay - a host, the editor - so that its first
    /// edit waits for none of it; what holds one in passing asks for nothing, and makes none.
    pub fn prepare(&self) {
        self.shared.prepare.store(true, std::sync::atomic::Ordering::Release);
        self.shared.wake.notify_all();
    }

    /// Say a set of changes whole, to be written behind: said in the log, and its files kept as waiting where
    /// every process reads them - neither had on disk yet, nor any file touched. The thread does both, in that
    /// order. Where it began.
    ///
    /// # Errors
    /// The log or the shared space cannot take it: nothing of it is said to wait, and no file is to be touched.
    pub fn say_later(&mut self, label: &str, changes: &[Stored], files: Vec<Behind>) -> std::io::Result<u64> {
        let log = self.shared.log.clone();
        let space = self.shared.space().cloned().ok_or_else(|| std::io::Error::other("nowhere to keep what waits"))?;
        let began = match self.begin(true) {
            Err(e) if e.kind() == std::io::ErrorKind::StorageFull => {
                self.checkpoint();
                self.begin(true)?
            }
            began => began?,
        };
        let said = (|| {
            for change in changes {
                match change {
                    Stored::Put { path, bytes } => sfs::storage::put(&log, path, bytes)?,
                    change => drop(change.append(&log)?),
                }
            }
            Stored::Whole { began, label: label.to_string() }.append(&log)?;
            // Read by every process from here on, as this set leaves them.
            let mut txn = space.write()?;
            for file in files.iter().filter(|file| !file.folder) {
                sfs::tree::put(&mut txn, WAITING, self.name(&file.path).as_bytes(), &waits_as(began, file))?;
            }
            txn.commit().map_err(std::io::Error::from)
        })();
        let mut state = self.shared.lock();
        if let Err(e) = said {
            // Not whole in the log, or whole and never to be written: said so, and nothing of it waits.
            let _ = Stored::Aborted { began }.append(&log);
            state.open = state.behind.front().map(|(began, _, _)| *began);
            return Err(e);
        }
        state.behind.push_back((began, label.to_string(), files));
        drop(state);
        self.shared.wake.notify_all();
        Ok(began)
    }

    /// Hold back what is to be written behind, or write it again: for what holds this to what happens while
    /// files wait - nothing is waited for (`settle`) while it is held back.
    pub fn hold_back(&self, held: bool) {
        self.shared.held_back.store(held, std::sync::atomic::Ordering::Release);
        self.shared.wake.notify_all();
    }

    /// Wait for all that was said to be written behind to be written: its files as it left them.
    pub fn settle(&self) {
        if self.shared.held_back.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        let mut state = self.shared.lock();
        if self.syncing.is_none() {
            drop(state);
            return self.shared.write_behind();
        }
        while (!state.behind.is_empty() || state.writing) && !state.stopping {
            self.shared.wake.notify_all();
            state = self.shared.wake.wait_timeout(state, Duration::from_millis(50)).unwrap_or_else(PoisonError::into_inner).0;
        }
    }

    /// A set said whole could not be written, and what it had written was put back: it is not to be written again.
    pub fn gave_up(&self, began: u64) {
        let _ = Stored::Aborted { began }.append(&self.shared.log);
        let mut state = self.shared.lock();
        state.open = state.behind.front().map(|(began, _, _)| *began);
    }

    /// The files of the set said last were written, these: to be had on disk soon, off this thread.
    pub fn wrote(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        let mut state = self.shared.lock();
        state.unsynced.extend(paths);
        state.open = state.behind.front().map(|(began, _, _)| *began);
    }

    /// Have what was written on disk now, and wait for it: the log is needed for none of it any longer.
    pub fn checkpoint(&self) {
        let mut state = self.shared.lock();
        if self.syncing.is_none() {
            drop(state);
            return self.shared.checkpoint();
        }
        let before = state.synced;
        state.asked = true;
        self.shared.wake.notify_all();
        // Two: one that was under way when this was asked may not have taken all that was written.
        while state.synced < before + 2 && !state.stopping {
            state = self.shared.wake.wait(state).unwrap_or_else(PoisonError::into_inner);
            if state.synced == before + 1 {
                state.asked = true;
                self.shared.wake.notify_all();
            }
        }
    }
}

impl Drop for Storage {
    fn drop(&mut self) {
        self.shared.lock().stopping = true;
        self.shared.wake.notify_all();
        match self.syncing.take() {
            Some(syncing) => drop(syncing.join()),
            None => self.shared.checkpoint(),
        }
    }
}

/// Have a file on disk: best effort - one gone since is nothing to have. Opened to read where that lets it be
/// flushed, which a file kept read-only is too.
pub(crate) fn sync_file(path: &Path) {
    let opened = if cfg!(unix) { std::fs::File::open(path) } else { std::fs::OpenOptions::new().write(true).open(path) };
    if let Ok(file) = opened {
        let _ = file.sync_all();
    }
}

/// Have files and folders on disk, each with the folder it is in.
fn sync(paths: &BTreeSet<PathBuf>) {
    let mut folders = BTreeSet::new();
    for path in paths {
        folders.extend(path.parent().map(Path::to_path_buf));
        if path.is_dir() {
            folders.insert(path.clone());
        } else {
            sync_file(path);
        }
    }
    // A folder's entries - a file made, renamed, removed - are on disk once the folder is: where a folder can be
    // opened to be asked.
    if cfg!(unix) {
        for folder in folders {
            let _ = std::fs::File::open(folder).and_then(|folder| folder.sync_all());
        }
    }
}

/// Write again, from the log, the sets of changes of the project in `root` that are whole and may not have reached
/// its files - whoever wrote them gone before it had them on disk: what was written again, in a few words, where
/// anything was. Asked by whoever takes the project, before it reads any file of it.
///
/// # Errors
/// The log cannot be read, or a file cannot be written.
pub fn redo(root: &Path) -> Result<Option<String>> {
    let dir = log_dir(root);
    // No log, nothing was ever written through one: none is made to find that out.
    if !dir.join(sfs::header::FILE).is_file() {
        return Ok(None);
    }
    let log = sfs::Log::shared(&dir, sfs::log::Config::default()).with_context(|| dir.display().to_string())?;
    let Some(from) = log.header().storage() else {
        forget_waiting(root);
        return Ok(None);
    };
    let sets = sfs::storage::whole(&log, from).context("reading what was written from the log")?;
    let mut written = BTreeSet::new();
    for (label, changes) in &sets {
        for change in changes {
            again(root, change, &mut written).with_context(|| format!("writing again what `{label}` changed"))?;
        }
    }
    sync(&written);
    log.header().set_storage(None);
    log.keep();
    forget_waiting(root);
    let said = match sets.as_slice() {
        [] => return Ok(None),
        [(label, _)] => format!("`{label}` was written again from the log: it may not have reached the files"),
        [.., (last, _)] => format!(
            "{} changes, `{last}` the last, were written again from the log: they may not have reached the files",
            sets.len()
        ),
    };
    Ok(Some(said))
}

/// Nothing waits to be written of the project in `root` any more - whoever said it is gone, and what it said whole
/// was written again from the log: what the shared space still keeps as waiting, where a process has it attached
/// still, is taken out.
fn forget_waiting(root: &Path) {
    let dir = root.join(crate::cache::DIR).join(crate::cache::STAGING).join(crate::shared::SPACE);
    if !dir.is_dir() {
        return;
    }
    let forgotten = crate::shared::space(root).map_err(sfs::space::Error::Io).and_then(|space| {
        let mut txn = space.write()?;
        let mut waiting = Vec::new();
        sfs::tree::scan(&txn, WAITING, b"", &mut |key, _| {
            waiting.push(key.to_vec());
            true
        })?;
        for key in &waiting {
            sfs::tree::remove(&mut txn, WAITING, key)?;
        }
        txn.commit()
    });
    if let Err(e) = forgotten {
        eprintln!("{}: what waited to be written could not be taken out of the shared space: {e}", root.display());
    }
}

/// Make one change again: as it left the files, whatever of it they have already.
fn again(root: &Path, change: &Stored, written: &mut BTreeSet<PathBuf>) -> Result<()> {
    let at = |name: &str| if Path::new(name).is_absolute() { PathBuf::from(name) } else { root.join(name) };
    let gone = |e: &std::io::Error| e.kind() == std::io::ErrorKind::NotFound;
    match change {
        Stored::Put { path, bytes } => {
            let path = at(path);
            if let Some(folder) = path.parent() {
                std::fs::create_dir_all(folder).with_context(|| folder.display().to_string())?;
            }
            crate::cache::write_atomic(&path, bytes)?;
            written.insert(path);
        }
        Stored::Delete { path } => match std::fs::remove_file(at(path)) {
            Err(e) if !gone(&e) => return Err(e).context(path.clone()),
            _ => drop(written.insert(at(path))),
        },
        Stored::Move { from, to } => {
            let (from, to) = (at(from), at(to));
            // Moved already where nothing is left to move; else what is in its way - what a move replaces - goes.
            if from.exists() {
                if to.is_file() {
                    std::fs::remove_file(&to).with_context(|| to.display().to_string())?;
                }
                if let Some(folder) = to.parent() {
                    std::fs::create_dir_all(folder).with_context(|| folder.display().to_string())?;
                }
                std::fs::rename(&from, &to).with_context(|| format!("{} -> {}", from.display(), to.display()))?;
            }
            written.extend([from, to]);
        }
        Stored::MakeDir { path } => {
            std::fs::create_dir_all(at(path)).with_context(|| path.clone())?;
            written.insert(at(path));
        }
        Stored::RemoveDir { path } => {
            let path = at(path);
            if std::fs::read_dir(&path).is_ok_and(|mut entries| entries.next().is_none()) {
                std::fs::remove_dir(&path).with_context(|| path.display().to_string())?;
            }
            written.insert(path);
        }
        Stored::Began | Stored::More { .. } | Stored::Whole { .. } | Stored::Aborted { .. } => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Storage, Stored, log_dir};

    #[test]
    fn the_log_is_needed_until_what_was_written_is_on_disk() {
        let root = std::env::temp_dir().join(format!("pe2-storage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let log = sfs::Log::shared(&log_dir(&root), sfs::log::Config::default()).unwrap();
        let needed = || log.header().storage();
        let mut storage = Storage::open(&root).unwrap();
        assert_eq!(needed(), None, "nothing written, nothing needed");
        let file = root.join("a.txt");
        let put = |text: &str| [Stored::Put { path: "a.txt".into(), bytes: text.as_bytes().to_vec() }];
        // Said whole: from there the log is needed, while its files are written and after.
        let first = storage.say("one", &put("one")).unwrap();
        assert!(needed().is_some_and(|from| from <= first));
        // Asked for while the set's files are being written: the log is still needed from that set on.
        storage.checkpoint();
        assert_eq!(needed(), Some(first), "its files are not on disk by that");
        std::fs::write(&file, "one").unwrap();
        storage.wrote([file.clone()]);
        // Had on disk: needed no longer.
        storage.checkpoint();
        assert_eq!(needed(), None);
        // And again from the next set on - which whoever holds the project next would write again, were it left.
        storage.say("two", &put("two")).unwrap();
        std::fs::write(&file, "two").unwrap();
        storage.wrote([file]);
        assert!(needed().is_some());
        // Let go of, what was written last is had on disk first.
        drop(storage);
        assert_eq!(needed(), None);
        assert_eq!(super::redo(&root).unwrap(), None, "nothing is left to write again");
        drop(log);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
