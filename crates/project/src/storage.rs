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

use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

pub use sfs::Stored;

/// How long what was written may wait to be had on disk, while more is written.
const EVERY: Duration = Duration::from_secs(5);

/// The log's folder, in the project's.
fn log_dir(root: &Path) -> PathBuf {
    root.join(crate::cache::DIR).join(crate::cache::STAGING).join("sfs")
}

/// What the holder of a project writes to its files, said in the log first.
pub struct Storage {
    root: PathBuf,
    shared: Arc<Shared>,
    /// What has on disk what was written, off the threads that write.
    syncing: Option<std::thread::JoinHandle<()>>,
}

struct Shared {
    log: Arc<sfs::Log>,
    state: Mutex<State>,
    wake: Condvar,
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
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
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

    /// Every few seconds while anything waits to be had on disk, and whenever it is asked for, until stopped -
    /// then once more, for what was written last.
    fn run(&self) {
        loop {
            let stopping = {
                let mut state = self.lock();
                let due = std::time::Instant::now() + EVERY;
                while !state.asked && !state.stopping {
                    let left = due.saturating_duration_since(std::time::Instant::now());
                    if left.is_zero() {
                        break;
                    }
                    state = self.wake.wait_timeout(state, left).unwrap_or_else(PoisonError::into_inner).0;
                }
                state.stopping
            };
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
        let shared = Arc::new(Shared { log, state: Mutex::default(), wake: Condvar::new() });
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
        // What the store was given for these changes is on disk before the records that name it.
        crate::chunks::sync_written(&self.root);
        let began = match self.begin() {
            // As much in the log as it may hold, some of it this side's: had on disk, it is let go of.
            Err(e) if e.kind() == std::io::ErrorKind::StorageFull => {
                self.checkpoint();
                self.begin()?
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
            self.shared.lock().open = None;
            return Err(e);
        }
        Ok(began)
    }

    /// Begin a set: the header says, on disk, that the log is needed from here for what is written to the files,
    /// before the first record of it.
    fn begin(&self) -> std::io::Result<u64> {
        let log = &self.shared.log;
        let mut state = self.shared.lock();
        let header = log.header();
        if header.storage().is_none() {
            header.set_storage(Some(header.tail()));
            header.flush()?;
        }
        let began = sfs::storage::began(log)?;
        state.since.get_or_insert(began);
        state.open = Some(began);
        Ok(began)
    }

    /// A set said whole could not be written, and what it had written was put back: it is not to be written again.
    pub fn gave_up(&self, began: u64) {
        let _ = Stored::Aborted { began }.append(&self.shared.log);
        self.shared.lock().open = None;
    }

    /// The files of the set said last were written, these: to be had on disk soon, off this thread.
    pub fn wrote(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        let mut state = self.shared.lock();
        state.unsynced.extend(paths);
        state.open = None;
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
    let Some(from) = log.header().storage() else { return Ok(None) };
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
