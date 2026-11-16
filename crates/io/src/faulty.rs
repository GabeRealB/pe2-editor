//! A filesystem that fails where a test says: any other, with each change it makes to the disk counted - written,
//! synced, renamed, removed, made, opened to write - on it and on what it opens, and the one the test names failing.
//! What reads is never failed. A failing write writes half of what it was given first, as a write cut short leaves a
//! file.
//!
//! A test runs what it tests once to count its changes ([`Faulty::changes`]), then again with each of them failing in
//! turn: once ([`Fault::Once`], an error midway, which what was written is to be put back from), or from there on
//! ([`Fault::From`], a crash - nothing reaches the disk any more, putting back included), and looks at what is left.

use crate::HostPath;
use crate::filesystem::{Descriptor, DirEntry, Filesystem, LockKind, Mapping, Open, Stat};
use pe2_path::RelPath;
use std::any::Any;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

/// How a change a test names fails.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// It alone: what comes after works.
    Once,
    /// It and every change after: the disk is gone, as where the process stopped there.
    From,
}

/// What fails, and how many changes were made: shared by a filesystem and all it opened.
#[derive(Debug, Default)]
struct Plan {
    changes: AtomicU64,
    /// The change that fails, counted from 1; 0 for none.
    at: AtomicU64,
    /// Whether every change after it fails too.
    from: std::sync::atomic::AtomicBool,
}

impl Plan {
    /// One change more, failing where it is to.
    fn change(&self) -> io::Result<()> {
        let n = self.changes.fetch_add(1, Ordering::SeqCst) + 1;
        let at = self.at.load(Ordering::SeqCst);
        let fails = at != 0 && (n == at || (n > at && self.from.load(Ordering::SeqCst)));
        if fails { Err(io::Error::other(format!("the disk failed at change {n}, as the test said"))) } else { Ok(()) }
    }

    /// A write: whole, or - where it fails - half of it written first.
    fn write(&self, bytes: &[u8], write: impl FnOnce(&[u8]) -> io::Result<usize>) -> io::Result<usize> {
        match self.change() {
            Ok(()) => write(bytes),
            Err(e) => {
                let _ = write(&bytes[..bytes.len() / 2]);
                Err(e)
            }
        }
    }
}

/// Any filesystem, failing where a test says.
#[derive(Clone)]
pub struct Faulty {
    inner: Arc<dyn Filesystem>,
    plan: Arc<Plan>,
}

impl std::fmt::Debug for Faulty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Faulty").field("plan", &self.plan).finish_non_exhaustive()
    }
}

impl Faulty {
    /// `inner`, failing nothing yet.
    #[must_use]
    pub fn new(inner: Arc<dyn Filesystem>) -> Faulty {
        Faulty { inner, plan: Arc::default() }
    }

    /// How many changes it made, or tried to.
    #[must_use]
    pub fn changes(&self) -> u64 {
        self.plan.changes.load(Ordering::SeqCst)
    }

    /// The `nth` change from now fails (1 the next), as `how` says.
    pub fn fail(&self, nth: u64, how: Fault) {
        self.plan.from.store(how == Fault::From, Ordering::SeqCst);
        self.plan.at.store(self.changes() + nth, Ordering::SeqCst);
    }

    /// Nothing fails any more.
    pub fn heal(&self) {
        self.plan.at.store(0, Ordering::SeqCst);
    }

    fn opened(&self, opened: Box<dyn Descriptor>) -> Box<dyn Descriptor> {
        Box::new(FaultyDescriptor { inner: opened, plan: self.plan.clone() })
    }
}

/// Whether opening so changes what is on the disk.
fn changes(how: Open) -> bool {
    how.write || how.create || how.truncate || how.append
}

impl Filesystem for Faulty {
    fn open_folder(&self, at: &HostPath, make: bool) -> io::Result<Box<dyn Descriptor>> {
        if make {
            self.plan.change()?;
        }
        Ok(self.opened(self.inner.open_folder(at, make)?))
    }

    fn open(&self, at: &HostPath, how: Open) -> io::Result<Box<dyn Descriptor>> {
        if changes(how) {
            self.plan.change()?;
        }
        Ok(self.opened(self.inner.open(at, how)?))
    }

    fn stat(&self, at: &HostPath) -> io::Result<Stat> {
        self.inner.stat(at)
    }

    fn stat_link(&self, at: &HostPath) -> io::Result<Stat> {
        self.inner.stat_link(at)
    }

    fn read_directory(&self, at: &HostPath) -> io::Result<Box<[DirEntry]>> {
        self.inner.read_directory(at)
    }

    fn read(&self, at: &HostPath) -> io::Result<Vec<u8>> {
        self.inner.read(at)
    }

    fn write(&self, at: &HostPath, bytes: &[u8]) -> io::Result<()> {
        self.plan.write(bytes, |bytes| self.inner.write(at, bytes).map(|()| bytes.len())).map(drop)
    }

    fn create_folder(&self, at: &HostPath) -> io::Result<()> {
        self.plan.change()?;
        self.inner.create_folder(at)
    }

    fn create_folder_all(&self, at: &HostPath) -> io::Result<()> {
        self.plan.change()?;
        self.inner.create_folder_all(at)
    }

    fn remove_file(&self, at: &HostPath) -> io::Result<()> {
        self.plan.change()?;
        self.inner.remove_file(at)
    }

    fn remove_folder(&self, at: &HostPath) -> io::Result<()> {
        self.plan.change()?;
        self.inner.remove_folder(at)
    }

    fn remove_all(&self, at: &HostPath) -> io::Result<()> {
        self.plan.change()?;
        self.inner.remove_all(at)
    }

    fn rename(&self, from: &HostPath, to: &HostPath) -> io::Result<()> {
        self.plan.change()?;
        self.inner.rename(from, to)
    }

    fn copy(&self, from: &HostPath, to: &HostPath) -> io::Result<u64> {
        self.plan.change()?;
        self.inner.copy(from, to)
    }

    fn hard_link(&self, from: &HostPath, to: &HostPath) -> io::Result<()> {
        self.plan.change()?;
        self.inner.hard_link(from, to)
    }

    fn symlink(&self, target: &std::path::Path, at: &HostPath) -> io::Result<()> {
        self.plan.change()?;
        self.inner.symlink(target, at)
    }

    fn set_times(&self, at: &HostPath, accessed: Option<SystemTime>, modified: Option<SystemTime>) -> io::Result<()> {
        self.plan.change()?;
        self.inner.set_times(at, accessed, modified)
    }

    fn read_link(&self, at: &HostPath) -> io::Result<std::path::PathBuf> {
        self.inner.read_link(at)
    }

    fn set_readonly(&self, at: &HostPath, readonly: bool) -> io::Result<()> {
        self.plan.change()?;
        self.inner.set_readonly(at, readonly)
    }

    fn set_executable(&self, at: &HostPath, executable: bool) -> io::Result<()> {
        self.plan.change()?;
        self.inner.set_executable(at, executable)
    }
}

/// What a faulty filesystem opened: its changes counted with the filesystem's.
#[derive(Debug)]
struct FaultyDescriptor {
    inner: Box<dyn Descriptor>,
    plan: Arc<Plan>,
}

impl Descriptor for FaultyDescriptor {
    fn open_at(&self, path: &RelPath, how: Open) -> io::Result<Box<dyn Descriptor>> {
        if changes(how) {
            self.plan.change()?;
        }
        Ok(Box::new(FaultyDescriptor { inner: self.inner.open_at(path, how)?, plan: self.plan.clone() }))
    }

    fn read_at(&self, into: &mut [u8], offset: u64) -> io::Result<usize> {
        self.inner.read_at(into, offset)
    }

    fn write_at(&self, bytes: &[u8], offset: u64) -> io::Result<usize> {
        self.plan.write(bytes, |bytes| self.inner.write_at(bytes, offset))
    }

    fn stat(&self) -> io::Result<Stat> {
        self.inner.stat()
    }

    fn stat_at(&self, path: &RelPath) -> io::Result<Stat> {
        self.inner.stat_at(path)
    }

    fn read_directory(&self) -> io::Result<Box<[DirEntry]>> {
        self.inner.read_directory()
    }

    fn create_directory_at(&self, path: &RelPath) -> io::Result<()> {
        self.plan.change()?;
        self.inner.create_directory_at(path)
    }

    fn remove_directory_at(&self, path: &RelPath) -> io::Result<()> {
        self.plan.change()?;
        self.inner.remove_directory_at(path)
    }

    fn unlink_file_at(&self, path: &RelPath) -> io::Result<()> {
        self.plan.change()?;
        self.inner.unlink_file_at(path)
    }

    fn rename_at(&self, from: &RelPath, to: &dyn Descriptor, to_path: &RelPath) -> io::Result<()> {
        self.plan.change()?;
        self.inner.rename_at(from, to, to_path)
    }

    fn append(&self, bytes: &[u8]) -> io::Result<()> {
        self.plan.write(bytes, |bytes| self.inner.append(bytes).map(|()| bytes.len())).map(drop)
    }

    fn set_size(&self, size: u64) -> io::Result<()> {
        self.plan.change()?;
        self.inner.set_size(size)
    }

    fn set_times(&self, accessed: Option<SystemTime>, modified: Option<SystemTime>) -> io::Result<()> {
        self.plan.change()?;
        self.inner.set_times(accessed, modified)
    }

    fn sync(&self) -> io::Result<()> {
        self.plan.change()?;
        self.inner.sync()
    }

    fn sync_data(&self) -> io::Result<()> {
        self.plan.change()?;
        self.inner.sync_data()
    }

    fn allocate(&self, len: u64) -> io::Result<()> {
        self.plan.change()?;
        self.inner.allocate(len)
    }

    fn lock(&self, kind: LockKind) -> io::Result<()> {
        self.inner.lock(kind)
    }

    fn try_lock(&self, kind: LockKind) -> io::Result<bool> {
        self.inner.try_lock(kind)
    }

    fn unlock(&self) -> io::Result<()> {
        self.inner.unlock()
    }

    unsafe fn map(&self, len: usize, writable: bool) -> io::Result<Mapping> {
        // SAFETY: as the caller promises of this one.
        unsafe { self.inner.map(len, writable) }
    }

    fn place(&self) -> Option<&HostPath> {
        self.inner.place()
    }

    fn as_any(&self) -> &dyn Any {
        // What the filesystem under it finds of its own across descriptors is its own descriptor.
        self.inner.as_any()
    }
}

#[cfg(test)]
#[allow(
    clippy::disallowed_methods,
    reason = "a test's fixtures: the machine's time, places, files and environment, which nothing tested reads"
)]
mod tests {
    use super::{Fault, Faulty};
    use crate::{Filesystem as _, HostPath, Open, SystemFilesystem};
    use std::sync::Arc;

    #[test]
    fn the_change_a_test_names_fails_once_or_from_there_on_and_a_write_cut_short_is_half_written() {
        let dir = std::env::temp_dir().join(format!("pe2-io-faulty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = HostPath::new(&dir).unwrap();
        let at = |name: &str| dir.entry(std::ffi::OsStr::new(name));
        let fs = Faulty::new(Arc::new(SystemFilesystem));
        fs.write(&at("a"), b"whole").unwrap();
        assert_eq!(fs.changes(), 1, "a write is a change");
        let _ = fs.read(&at("a")).unwrap();
        assert_eq!(fs.changes(), 1, "a read is none");
        // Once: the second change from now fails, and what comes after works.
        fs.fail(2, Fault::Once);
        fs.write(&at("b"), b"first").unwrap();
        assert!(fs.write(&at("c"), b"cut short").is_err());
        assert_eq!(fs.read(&at("c")).unwrap(), b"cut ", "half of it written");
        fs.write(&at("d"), b"after").unwrap();
        // From there on: what a file held open writes, and syncs, fails too.
        let held = fs.open(&at("e"), Open::WRITE).unwrap();
        fs.fail(1, Fault::From);
        assert!(held.write_at(b"x", 0).is_err() && held.sync().is_err() && fs.rename(&at("a"), &at("f")).is_err());
        assert_eq!(fs.read(&at("a")).unwrap(), b"whole", "nothing reached the disk");
        fs.heal();
        fs.rename(&at("a"), &at("f")).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
