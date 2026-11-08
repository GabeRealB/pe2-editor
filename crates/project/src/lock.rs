//! The project lock: one process at a time works on a project - the editor, an extraction, a build. Each holds an
//! exclusive OS lock on `.cache/lock` for as long as it runs (`flock` on Unix, `LockFileEx` on Windows), so the OS
//! releases it when the process ends, however it ends: a lock is never left behind. Beside it, `.cache/lock.owner`
//! names the process holding it, for whoever finds the project locked.
//!
//! A process holding the lock passes it to the processes it starts: `PE2_PROJECT_LOCKED` names the project, and a
//! process finding its project named there works under its parent's lock instead of waiting for it
//! (`tools/project_lock.py` takes the same lock for the Python tools).
//!
//! A project the host serving it holds (`host.rs`) is borrowed from it: the host lets go of the lock when asked, and
//! takes it back when the borrower lets go - the lease is held with the lock, and ends after it.
//!
//! Some file systems cannot lock a file at all - a WSL folder opened from Windows (`\\wsl.localhost\...`, whose 9P
//! share has no byte-range locks). There the project opens unprotected rather than not at all: the lock says why
//! (`unprotected`), for the editor and the tools to warn that nothing keeps a second process out. A Windows process
//! and a WSL one would not see each other's locks across that share anyway.

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
    file: Option<File>,
    unprotected: Option<String>,
    /// The project borrowed from its host: let go of after the lock, for the host to take it back.
    _borrowed: Option<crate::host::Lease>,
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
    Ok(Lock { file: Some(file), unprotected: None, _borrowed: None })
}

/// Whether a lock failed because the file system cannot lock files: `ERROR_INVALID_FUNCTION` or `ERROR_NOT_SUPPORTED`
/// on Windows (a WSL share), `ENOLCK` or an unsupported operation elsewhere.
fn unsupported(e: &std::io::Error) -> bool {
    #[cfg(windows)]
    let codes: &[i32] = &[1, 50];
    #[cfg(target_os = "linux")]
    let codes: &[i32] = &[37];
    #[cfg(target_os = "macos")]
    let codes: &[i32] = &[77];
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    let codes: &[i32] = &[];
    e.kind() == std::io::ErrorKind::Unsupported || e.raw_os_error().is_some_and(|c| codes.contains(&c))
}

/// The project, held without a lock where its file system cannot lock: who works on it is still recorded.
fn unprotected(root: &Path, lock: &Path, who: &str, e: &std::io::Error) -> Result<Lock, LockError> {
    let owner = format!("{who} (process {}, without a lock)", std::process::id());
    std::fs::write(paths(root).1, owner).map_err(|e| LockError::Io(e.into()))?;
    let why = format!(
        "{} cannot be locked on its file system ({e}): nothing keeps another editor or tool from working on the \
         project at the same time",
        lock.display()
    );
    Ok(Lock { file: None, unprotected: Some(why), _borrowed: None })
}

/// Who holds the project in `root` now, as its owner file says; none where nobody does. Asked by a shared lock tried and
/// let go of at once, which writes nothing: a process taking the project at that same instant is refused, and tries
/// again as it would on any holder.
#[must_use]
pub fn holder(root: &Path) -> Option<String> {
    let (lock, _) = paths(root);
    let file = OpenOptions::new().read(true).open(&lock).ok()?;
    // Taken, or not lockable at all: nobody holds it by a lock.
    matches!(file.try_lock_shared(), Err(TryLockError::WouldBlock)).then(|| owner(root))
}

impl Lock {
    /// Whether a parent process holds the project for this one: held only while that one works on it.
    #[must_use]
    pub fn inherited(&self) -> bool {
        self.file.is_none() && self.unprotected.is_none()
    }

    /// Why the project is held without a lock, where its file system cannot lock files.
    #[must_use]
    pub fn unprotected(&self) -> Option<&str> {
        self.unprotected.as_deref()
    }

    /// Lock the project in `root` for `who` - borrowed from the host serving it, where it holds it - or say who holds
    /// it.
    ///
    /// # Errors
    /// Another process holds the project, or the lock file cannot be made.
    pub fn try_acquire(root: &Path, who: &str) -> Result<Lock, LockError> {
        Lock::try_lock(root, who, true)
    }

    /// Lock the project in `root` for `who`, borrowing it from nobody: the host's own taking.
    ///
    /// # Errors
    /// As `try_acquire`.
    pub fn try_take(root: &Path, who: &str) -> Result<Lock, LockError> {
        Lock::try_lock(root, who, false)
    }

    fn try_lock(root: &Path, who: &str, borrowing: bool) -> Result<Lock, LockError> {
        if inherited(root) {
            return Ok(Lock { file: None, unprotected: None, _borrowed: None });
        }
        let file = open(root).map_err(LockError::Io)?;
        match file.try_lock() {
            Ok(()) => claim(root, file, who),
            // Held by the host: lent, it is taken at once - or by whoever was quicker.
            Err(TryLockError::WouldBlock)
                if let Some(lease) = borrowing.then(|| crate::host::borrow(root, who)).flatten() =>
            {
                match file.try_lock() {
                    Ok(()) => claim(root, file, who).map(|lock| Lock { _borrowed: Some(lease), ..lock }),
                    Err(_) => Err(LockError::Held(owner(root))),
                }
            }
            Err(TryLockError::WouldBlock) => Err(LockError::Held(owner(root))),
            Err(TryLockError::Error(e)) if unsupported(&e) => unprotected(root, &paths(root).0, who, &e),
            Err(TryLockError::Error(e)) => {
                Err(LockError::Io(anyhow::Error::new(e).context(paths(root).0.display().to_string())))
            }
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

    /// Lock the project in `root` for `who`, waiting while another process holds it; `waiting` is told who, once. It
    /// asks again and again rather than waiting on the lock, so a holder that lends it - the host - lends it here too.
    ///
    /// # Errors
    /// The lock file cannot be made or locked.
    pub fn acquire(root: &Path, who: &str, waiting: impl FnOnce(&str)) -> Result<Lock, LockError> {
        let mut waiting = Some(waiting);
        loop {
            match Lock::try_acquire(root, who) {
                Err(LockError::Held(owner)) => {
                    if let Some(waiting) = waiting.take() {
                        waiting(&owner);
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                other => return other,
            }
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

    #[test]
    fn a_file_system_without_locks_holds_unprotected() {
        let root = std::env::temp_dir().join(format!("pe2-unlockable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".cache")).unwrap();
        let e = std::io::Error::from(std::io::ErrorKind::Unsupported);
        assert!(super::unsupported(&e));
        assert!(!super::unsupported(&std::io::Error::from(std::io::ErrorKind::PermissionDenied)));
        let held =
            super::unprotected(&root, &root.join(".cache/lock"), "the editor", &e).unwrap_or_else(|e| panic!("{e}"));
        assert!(held.unprotected().is_some_and(|why| why.contains("nothing keeps another editor")));
        drop(held);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
