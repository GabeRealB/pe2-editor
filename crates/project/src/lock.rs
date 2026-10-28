//! The project lock: one process at a time works on a project - the editor, an extraction, a build. Each holds an
//! exclusive OS lock on `.cache/lock` for as long as it runs (`flock` on Unix, `LockFileEx` on Windows), so the OS
//! releases it when the process ends, however it ends: a lock is never left behind. Beside it, `.cache/lock.owner`
//! names the process holding it, for whoever finds the project locked.
//!
//! A process holding the lock passes it to the processes it starts: `PE2_PROJECT_LOCKED` names the project, and a
//! process finding its project named there works under its parent's lock instead of waiting for it
//! (`tools/project_lock.py` takes the same lock for the Python tools).

use anyhow::{Context, Result};
use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::cache::DIR;

const LOCK: &str = "lock";
const OWNER: &str = "lock.owner";
const HELD: &str = "PE2_PROJECT_LOCKED";

/// A project held: the lock lasts until this is dropped. None when a parent process holds it for this one.
#[derive(Debug)]
pub struct Lock {
    _file: Option<File>,
}

/// Whether a parent process holds the project in `root` for this one.
fn inherited(root: &Path) -> bool {
    let held = std::env::var_os(HELD).map(PathBuf::from);
    held.is_some_and(|h| std::fs::canonicalize(h).ok() == std::fs::canonicalize(root).ok())
}

/// Why a project could not be locked.
#[derive(Debug)]
pub enum LockError {
    /// Another process holds it: who, as it named itself.
    Held(String),
    Io(anyhow::Error),
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockError::Held(owner) => write!(f, "the project is in use by {owner}"),
            LockError::Io(e) => write!(f, "the project cannot be locked: {e:#}"),
        }
    }
}

impl std::error::Error for LockError {}

fn paths(root: &Path) -> (PathBuf, PathBuf) {
    let dir = root.join(DIR);
    (dir.join(LOCK), dir.join(OWNER))
}

fn open(root: &Path) -> Result<File> {
    let (lock, _) = paths(root);
    crate::cache::make(root)?;
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock)
        .with_context(|| format!("{}", lock.display()))
}

/// Who holds the project, as its owner file says.
fn owner(root: &Path) -> String {
    std::fs::read_to_string(paths(root).1).map_or_else(|_| "another process".into(), |s| s.trim().to_string())
}

/// Record who holds the lock now.
fn claim(root: &Path, file: File, who: &str) -> Result<Lock, LockError> {
    let owner = format!("{who} (process {})", std::process::id());
    std::fs::write(paths(root).1, owner).map_err(|e| LockError::Io(e.into()))?;
    Ok(Lock { _file: Some(file) })
}

impl Lock {
    /// Lock the project in `root` for `who`, or say who holds it.
    ///
    /// # Errors
    /// Another process holds the project, or the lock file cannot be made.
    pub fn try_acquire(root: &Path, who: &str) -> Result<Lock, LockError> {
        if inherited(root) {
            return Ok(Lock { _file: None });
        }
        let file = open(root).map_err(LockError::Io)?;
        match file.try_lock() {
            Ok(()) => claim(root, file, who),
            Err(TryLockError::WouldBlock) => Err(LockError::Held(owner(root))),
            Err(TryLockError::Error(e)) => Err(LockError::Io(e.into())),
        }
    }

    /// Lock the project in `root` for `who`, waiting at most `limit` for another process to let go of it: for a
    /// holder that is expected to be brief (one agent's call, the editor opening while one is answered).
    ///
    /// # Errors
    /// The project is still held after `limit`, or the lock file cannot be made.
    pub fn acquire_within(root: &Path, who: &str, limit: Duration) -> Result<Lock, LockError> {
        let start = Instant::now();
        loop {
            match Lock::try_acquire(root, who) {
                Err(LockError::Held(_)) if start.elapsed() < limit => std::thread::sleep(Duration::from_millis(25)),
                other => return other,
            }
        }
    }

    /// Lock the project in `root` for `who`, waiting while another process holds it; `waiting` is told who, once.
    ///
    /// # Errors
    /// The lock file cannot be made or locked.
    pub fn acquire(root: &Path, who: &str, waiting: impl FnOnce(&str)) -> Result<Lock, LockError> {
        match Lock::try_acquire(root, who) {
            Err(LockError::Held(owner)) => {
                waiting(&owner);
                let file = open(root).map_err(LockError::Io)?;
                file.lock().map_err(|e| LockError::Io(e.into()))?;
                claim(root, file, who)
            }
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Lock, LockError};

    #[test]
    fn one_holder_at_a_time() {
        let root = std::env::temp_dir().join(format!("pe2-lock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let held = Lock::try_acquire(&root, "the first").unwrap();
        // Locking a project with no cache yet makes one that git ignores.
        assert_eq!(std::fs::read_to_string(root.join(".cache/.gitignore")).unwrap(), "*\n");
        // A second handle on the file is a second holder to the OS, as another process would be.
        match Lock::try_acquire(&root, "the second") {
            Err(LockError::Held(owner)) => assert!(owner.starts_with("the first"), "{owner}"),
            other => panic!("{other:?}"),
        }
        drop(held);
        let again = Lock::try_acquire(&root, "the second").unwrap();
        drop(again);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
