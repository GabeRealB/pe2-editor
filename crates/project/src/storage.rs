//! What whoever holds the project writes to its files goes through the log the project's processes share
//! (`sfs::storage`): a set of changes is said there whole, and had on disk, before any file is touched. So a process
//! that dies between - or a loss of power - leaves the files to be written again from the log (`redo`, by whoever
//! takes the project next), never half a set with nothing to tell what the rest was; and a set that never came to
//! be whole touched no file.
//!
//! The files themselves are written with no wait for the disk. Every few seconds, and as whoever holds the project
//! lets go of it, what was written is had on disk (`checkpoint`), and the log is needed for it no longer: the header
//! says from where it still is (`sfs::Header::storage`), which who lets go of the log reads.

use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub use sfs::Stored;

/// How long what was written may wait to be had on disk, while more is written.
const EVERY: Duration = Duration::from_secs(5);

/// The log's folder, in the project's.
fn log_dir(root: &Path) -> PathBuf {
    root.join(crate::cache::DIR).join(crate::cache::STAGING).join("sfs")
}

/// What the holder of a project writes to its files, said in the log first.
pub struct Storage {
    log: Arc<sfs::Log>,
    root: PathBuf,
    /// What was written since it was last had on disk.
    unsynced: BTreeSet<PathBuf>,
    synced: Instant,
}

impl std::fmt::Debug for Storage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "storage of {} ({} paths not on disk yet)", self.root.display(), self.unsynced.len())
    }
}

impl Storage {
    /// The storage of the project in `root`; none where its log cannot be opened - what is written is then written
    /// as it comes, with nothing to take it up from.
    #[must_use]
    pub fn open(root: &Path) -> Option<Storage> {
        let log = sfs::Log::shared(&log_dir(root), sfs::log::Config::default());
        let log = log.inspect_err(|e| eprintln!("{}: no log to write through: {e}", root.display())).ok()?;
        Some(Storage { log, root: root.to_path_buf(), unsynced: BTreeSet::new(), synced: Instant::now() })
    }

    /// A path as the log says it: within the project, from its root; elsewhere - a tree mounted from another
    /// place - whole.
    #[must_use]
    pub fn name(&self, path: &Path) -> String {
        path.strip_prefix(&self.root).unwrap_or(path).to_string_lossy().replace('\\', "/")
    }

    /// Say a set of changes whole, and have it on disk: where it began. Only then are its files to be touched.
    ///
    /// # Errors
    /// The log cannot take it, or cannot be had on disk: no file is to be touched.
    pub fn say(&mut self, label: &str, changes: &[Stored]) -> std::io::Result<u64> {
        if self.synced.elapsed() > EVERY {
            self.checkpoint();
        }
        // From here the log is needed for what is written to the files: said, on disk, before the first of it.
        let header = self.log.header();
        if header.storage().is_none() {
            header.set_storage(Some(header.tail()));
            header.flush()?;
        }
        // What the store was given for these changes is on disk before the records that name it.
        crate::chunks::sync_written();
        let began = match sfs::storage::began(&self.log) {
            // As much in the log as it may hold, some of it this side's: had on disk, it is let go of.
            Err(e) if e.kind() == std::io::ErrorKind::StorageFull => {
                self.checkpoint();
                let header = self.log.header();
                header.set_storage(Some(header.tail()));
                header.flush()?;
                sfs::storage::began(&self.log)?
            }
            began => began?,
        };
        for change in changes {
            match change {
                Stored::Put { path, bytes } => sfs::storage::put(&self.log, path, bytes)?,
                change => drop(change.append(&self.log)?),
            }
        }
        Stored::Whole { began, label: label.to_string() }.append(&self.log)?;
        self.log.flush(self.log.header().tail())?;
        Ok(began)
    }

    /// A set said whole could not be written, and what it had written was put back: it is not to be written again.
    pub fn gave_up(&self, began: u64) {
        let _ = Stored::Aborted { began }.append(&self.log);
    }

    /// These were written: to be had on disk at the next checkpoint.
    pub fn wrote(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        self.unsynced.extend(paths);
    }

    /// Have what was written on disk, where it is due.
    pub fn tick(&mut self) {
        if !self.unsynced.is_empty() && self.synced.elapsed() > EVERY {
            self.checkpoint();
        }
    }

    /// Have what was written on disk: the log is needed for none of it any longer.
    pub fn checkpoint(&mut self) {
        sync(&std::mem::take(&mut self.unsynced));
        self.log.header().set_storage(None);
        // Whether anything serves the project or not: what nobody needs of the log goes.
        self.log.keep();
        self.synced = Instant::now();
    }
}

impl Drop for Storage {
    fn drop(&mut self) {
        if !self.unsynced.is_empty() || self.log.header().storage().is_some() {
            self.checkpoint();
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
