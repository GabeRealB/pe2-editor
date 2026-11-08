//! The tree served over FUSE (Linux, and macOS with macFUSE or FUSE-T): each request answered from the tree served at
//! that moment and, where it can be written, what programs wrote over it (`writes::Writes`). A mount lasts as long as
//! its `Mounted`; dropping it unmounts.

use std::ffi::OsStr;
use std::path::Path;
use std::time::{Duration, SystemTime};

use fuser::{
    BsdFileFlags, Config, Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation, INodeNo,
    LockOwner, MountOption, OpenAccMode, OpenFlags, RenameFlags, ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory,
    ReplyEmpty, ReplyEntry, ReplyOpen, ReplyWrite, Request, TimeOrNow, WriteFlags,
};

use crate::writes::{Shown, Writes};
use crate::{ROOT, Served, What};

/// How long the kernel may keep what it was told of a node: short, as a take-in may change it.
const TTL: Duration = Duration::from_secs(1);

struct Fuse {
    served: Served,
    writes: Option<Writes>,
    uid: u32,
    gid: u32,
}

fn errno(e: &std::io::Error) -> Errno {
    match e.kind() {
        std::io::ErrorKind::NotFound => Errno::ENOENT,
        std::io::ErrorKind::AlreadyExists => Errno::EEXIST,
        std::io::ErrorKind::PermissionDenied => Errno::EACCES,
        std::io::ErrorKind::InvalidInput => Errno::EINVAL,
        _ => Errno::EIO,
    }
}

fn join(parent: &str, name: &str) -> String {
    if parent.is_empty() { name.to_string() } else { format!("{parent}/{name}") }
}

impl Fuse {
    fn path(&self, ino: INodeNo) -> Option<String> {
        self.served.path(u64::from(ino))
    }

    fn child(&self, parent: INodeNo, name: &OsStr) -> Option<String> {
        Some(join(&self.path(parent)?, name.to_str()?))
    }

    /// What a path is: written over the tree where it can be written, else the tree's.
    fn shown(&self, path: &str) -> Option<Shown> {
        if let Some(writes) = &self.writes {
            return writes.shown(path);
        }
        let tree = self.served.tree();
        let (_, node) = tree.at(path)?;
        Some(Shown { folder: matches!(node.what, What::Folder(_)), size: node.size, modified: node.modified })
    }

    fn attr(&self, path: &str, shown: &Shown) -> FileAttr {
        let writable = self.writes.is_some();
        let (kind, perm, nlink) = match (shown.folder, writable) {
            (true, false) => (FileType::Directory, 0o555, 2),
            (true, true) => (FileType::Directory, 0o755, 2),
            (false, false) => (FileType::RegularFile, 0o444, 1),
            (false, true) => (FileType::RegularFile, 0o644, 1),
        };
        FileAttr {
            ino: INodeNo(self.served.ino(path)),
            size: shown.size,
            blocks: shown.size.div_ceil(512),
            atime: shown.modified,
            mtime: shown.modified,
            ctime: shown.modified,
            crtime: shown.modified,
            kind,
            perm,
            nlink,
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            blksize: 4096,
            flags: 0,
        }
    }

    fn entry(&self, path: &str, reply: ReplyEntry) {
        match self.shown(path) {
            Some(shown) => reply.entry(&TTL, &self.attr(path, &shown), Generation(0)),
            None => reply.error(Errno::ENOENT),
        }
    }

    fn writes(&self) -> Result<&Writes, Errno> {
        self.writes.as_ref().ok_or(Errno::EROFS)
    }
}

impl Filesystem for Fuse {
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        match self.child(parent, name) {
            Some(path) => self.entry(&path, reply),
            None => reply.error(Errno::ENOENT),
        }
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        match self.path(ino).and_then(|path| Some((self.shown(&path)?, path))) {
            Some((shown, path)) => reply.attr(&TTL, &self.attr(&path, &shown)),
            None => reply.error(Errno::ENOENT),
        }
    }

    fn setattr(
        &self,
        _req: &Request,
        ino: INodeNo,
        _mode: Option<u32>,
        _uid: Option<u32>,
        _gid: Option<u32>,
        size: Option<u64>,
        _atime: Option<TimeOrNow>,
        _mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        fh: Option<FileHandle>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<BsdFileFlags>,
        reply: ReplyAttr,
    ) {
        let Some(path) = self.path(ino) else { return reply.error(Errno::ENOENT) };
        if let Some(size) = size {
            let writes = match self.writes() {
                Ok(writes) => writes,
                Err(e) => return reply.error(e),
            };
            if let Err(e) = writes.truncate(&path, size) {
                return reply.error(errno(&e));
            }
            // Truncated with no file open: applied now, as nothing else will close it.
            if fh.is_none()
                && let Ok(handle) = writes.open(&path, false)
                && let Err(e) = writes.close(handle, true)
            {
                return reply.error(errno(&e));
            }
        }
        match self.shown(&path) {
            Some(shown) => reply.attr(&TTL, &self.attr(&path, &shown)),
            None => reply.error(Errno::ENOENT),
        }
    }

    fn mkdir(&self, _req: &Request, parent: INodeNo, name: &OsStr, _mode: u32, _umask: u32, reply: ReplyEntry) {
        let Some(path) = self.child(parent, name) else { return reply.error(Errno::ENOENT) };
        match self.writes().map(|w| w.make_folder(&path)) {
            Ok(Ok(())) => self.entry(&path, reply),
            Ok(Err(e)) => reply.error(errno(&e)),
            Err(e) => reply.error(e),
        }
    }

    fn unlink(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let Some(path) = self.child(parent, name) else { return reply.error(Errno::ENOENT) };
        match self.writes().map(|w| w.remove(&path)) {
            Ok(Ok(())) => reply.ok(),
            Ok(Err(e)) => reply.error(errno(&e)),
            Err(e) => reply.error(e),
        }
    }

    fn rmdir(&self, req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        if let Some(path) = self.child(parent, name)
            && self.writes.as_ref().is_some_and(|w| !w.list(&path).is_empty())
        {
            return reply.error(Errno::ENOTEMPTY);
        }
        self.unlink(req, parent, name, reply);
    }

    fn rename(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        newparent: INodeNo,
        newname: &OsStr,
        _flags: RenameFlags,
        reply: ReplyEmpty,
    ) {
        let (Some(from), Some(to)) = (self.child(parent, name), self.child(newparent, newname)) else {
            return reply.error(Errno::ENOENT);
        };
        match self.writes().map(|w| w.rename(&from, &to)) {
            Ok(Ok(())) => reply.ok(),
            Ok(Err(e)) => reply.error(errno(&e)),
            Err(e) => reply.error(e),
        }
    }

    fn open(&self, _req: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        let Some(path) = self.path(ino) else { return reply.error(Errno::ENOENT) };
        match self.shown(&path) {
            Some(Shown { folder: true, .. }) => return reply.error(Errno::EISDIR),
            None => return reply.error(Errno::ENOENT),
            Some(_) => {}
        }
        // Open until released: what serving waits for before it stops.
        if flags.acc_mode() == OpenAccMode::O_RDONLY {
            self.served.opened();
            return reply.opened(FileHandle(0), FopenFlags::empty());
        }
        let truncate = flags.0 & libc::O_TRUNC != 0;
        match self.writes().map(|w| w.open(&path, truncate)) {
            Ok(Ok(handle)) => {
                self.served.opened();
                reply.opened(FileHandle(handle), FopenFlags::FOPEN_DIRECT_IO);
            }
            Ok(Err(e)) => reply.error(errno(&e)),
            Err(e) => reply.error(e),
        }
    }

    fn create(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        _flags: i32,
        reply: ReplyCreate,
    ) {
        let Some(path) = self.child(parent, name) else { return reply.error(Errno::ENOENT) };
        let made = self.writes().map(|w| w.create(&path));
        match made {
            Ok(Ok(handle)) => match self.shown(&path) {
                Some(shown) => {
                    self.served.opened();
                    reply.created(
                        &TTL,
                        &self.attr(&path, &shown),
                        Generation(0),
                        FileHandle(handle),
                        FopenFlags::FOPEN_DIRECT_IO,
                    );
                }
                None => reply.error(Errno::EIO),
            },
            Ok(Err(e)) => reply.error(errno(&e)),
            Err(e) => reply.error(e),
        }
    }

    fn read(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        let read = match (&self.writes, self.path(ino)) {
            (Some(writes), Some(path)) => writes.read(&path, offset, size as usize),
            (None, _) => self.served.tree().read(u64::from(ino), offset, size as usize),
            (Some(_), None) => Err(std::io::Error::from(std::io::ErrorKind::NotFound)),
        };
        match read {
            Ok(bytes) => reply.data(&bytes),
            Err(e) => reply.error(errno(&e)),
        }
    }

    fn write(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        data: &[u8],
        _write_flags: WriteFlags,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyWrite,
    ) {
        match self.writes().map(|w| w.write(u64::from(fh), offset, data)) {
            Ok(Ok(n)) => reply.written(u32::try_from(n).unwrap_or(u32::MAX)),
            Ok(Err(e)) => reply.error(errno(&e)),
            Err(e) => reply.error(e),
        }
    }

    fn fsync(&self, _req: &Request, _ino: INodeNo, fh: FileHandle, _datasync: bool, reply: ReplyEmpty) {
        match self.writes.as_ref().map(|w| w.close(u64::from(fh), false)) {
            Some(Err(e)) => reply.error(errno(&e)),
            _ => reply.ok(),
        }
    }

    fn release(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        self.served.released();
        match self.writes.as_ref().filter(|_| u64::from(fh) != 0).map(|w| w.close(u64::from(fh), true)) {
            Some(Err(e)) => reply.error(errno(&e)),
            _ => reply.ok(),
        }
    }

    fn readdir(&self, _req: &Request, ino: INodeNo, _fh: FileHandle, offset: u64, mut reply: ReplyDirectory) {
        let Some(path) = self.path(ino) else { return reply.error(Errno::ENOENT) };
        let children: Vec<(String, bool)> = if let Some(writes) = &self.writes {
            writes.list(&path).into_iter().collect()
        } else {
            let tree = self.served.tree();
            match tree.at(&path).map(|(_, n)| &n.what) {
                Some(What::Folder(children)) => children
                    .iter()
                    .map(|(name, child)| {
                        (name.clone(), matches!(tree.node(*child).map(|n| &n.what), Some(What::Folder(_))))
                    })
                    .collect(),
                _ => return reply.error(Errno::ENOTDIR),
            }
        };
        let parent =
            if u64::from(ino) == ROOT { ROOT } else { self.served.ino(path.rsplit_once('/').map_or("", |(p, _)| p)) };
        let listed =
            [(u64::from(ino), FileType::Directory, ".".to_string()), (parent, FileType::Directory, "..".to_string())]
                .into_iter()
                .chain(children.into_iter().map(|(name, folder)| {
                    let kind = if folder { FileType::Directory } else { FileType::RegularFile };
                    (self.served.ino(&join(&path, &name)), kind, name)
                }));
        for (at, (child, kind, name)) in listed.enumerate().skip(usize::try_from(offset).unwrap_or(usize::MAX)) {
            if reply.add(INodeNo(child), (at + 1) as u64, kind, &name) {
                break;
            }
        }
        reply.ok();
    }
}

/// A mount: unmounted when dropped.
pub struct Mounted {
    _session: fuser::BackgroundSession,
    at: std::path::PathBuf,
    told: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for Mounted {
    fn drop(&mut self) {
        self.told.store(true, std::sync::atomic::Ordering::Relaxed);
        MOUNTED.lock().unwrap_or_else(std::sync::PoisonError::into_inner).retain(|at| *at != self.at);
    }
}

/// Tell the kernel what changes in the tree served by other means than the mount - the project changed beneath it - so
/// that what it cached of it (lengths, pages, names) goes: a file changed is invalidated, a node gone deleted, a node
/// new no longer known missing. What watches the mount (an editor's inotify) hears only of what passes through it, so
/// each file changed, and the folder of each node gone or new, is touched through the mount (its time set: `ATTRIB`),
/// for the watcher to look again. Until `stop`.
fn tell_kernel(served: &Served, notifier: &fuser::Notifier, at: &Path, stop: &std::sync::atomic::AtomicBool) {
    let touch = |path: &str, time: SystemTime| {
        if let Ok(file) = std::fs::File::open(at.join(path)) {
            let _ = file.set_modified(time);
        }
    };
    let mut last = served.tree();
    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(200));
        let now = served.tree();
        if std::sync::Arc::ptr_eq(&last, &now) {
            continue;
        }
        // What the kernel did itself (a program's write through the mount) it knows: what it is told again it ignores.
        let changes = crate::Tree::changes(&last, &now);
        let mut folders = std::collections::BTreeSet::new();
        for ino in &changes.changed {
            let _ = notifier.inval_inode(INodeNo(*ino), 0, 0);
        }
        for (parent, ino, name) in &changes.gone {
            if notifier.delete(INodeNo(*parent), INodeNo(*ino), OsStr::new(name)).is_err() {
                let _ = notifier.inval_entry(INodeNo(*parent), OsStr::new(name));
            }
            folders.insert(*parent);
        }
        for (parent, name) in &changes.new {
            let _ = notifier.inval_entry(INodeNo(*parent), OsStr::new(name));
            folders.insert(*parent);
        }
        for node in changes.changed.iter().filter_map(|ino| now.node(*ino)) {
            touch(&node.path, node.modified);
        }
        for folder in folders.iter().filter_map(|ino| now.node(*ino)) {
            touch(&folder.path, SystemTime::now());
        }
        last = now;
    }
}

/// The folders this process has mounted on, for `unmount_all`.
static MOUNTED: std::sync::Mutex<Vec<std::path::PathBuf>> = std::sync::Mutex::new(Vec::new());

/// Unmount everything this process mounted, as it is about to end without dropping its mounts (a signal): otherwise
/// each is left mounted with no one serving it, which every access fails.
pub fn unmount_all() {
    let mounted = std::mem::take(&mut *MOUNTED.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
    for at in mounted {
        unmount(&at);
    }
}

/// Unmount a folder, lazily: what has it open keeps it until it lets go.
fn unmount(at: &Path) {
    #[cfg(target_os = "linux")]
    let tries: &[(&str, &[&str])] = &[("fusermount3", &["-uz"]), ("fusermount", &["-uz"])];
    #[cfg(target_os = "macos")]
    let tries: &[(&str, &[&str])] = &[("umount", &["-f"])];
    for (program, args) in tries {
        let done = std::process::Command::new(program).args(*args).arg(at).status();
        if done.is_ok_and(|s| s.success()) {
            return;
        }
    }
}

/// Mount the tree served at a folder: read-only, or written through `writes`.
///
/// # Errors
/// The folder cannot be mounted on: FUSE is missing, or the folder is not an empty one.
pub fn mount(served: &Served, writes: Option<Writes>, at: &Path, name: &str) -> std::io::Result<Mounted> {
    clear_stale(at);
    // SAFETY: getuid and getgid cannot fail and touch no memory.
    let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
    let mut config = Config::default();
    config.mount_options.extend([MountOption::FSName(name.to_string()), MountOption::NoAtime]);
    if writes.is_none() {
        config.mount_options.push(MountOption::RO);
    }
    // What reads files for others as root - WSL's file server, through which Windows sees `\\wsl$` - is let in where
    // the system lets a user say so; FUSE refuses every user but the one mounting otherwise.
    if uid == 0 || users_may_allow_others() {
        config.acl = fuser::SessionACL::RootAndOwner;
    }
    config.n_threads = Some(4);
    let fuse = Fuse { served: served.clone(), writes, uid, gid };
    let session = fuser::spawn_mount(fuse, at, &config)?;
    let told = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (notifier, stop, watched, root) = (session.notifier(), told.clone(), served.clone(), at.to_path_buf());
    std::thread::Builder::new()
        .name("fuse-notify".into())
        .spawn(move || tell_kernel(&watched, &notifier, &root, &stop))?;
    MOUNTED.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(at.to_path_buf());
    Ok(Mounted { _session: session, at: at.to_path_buf(), told })
}

/// Why Windows programs cannot open a mount made here, where they cannot: under WSL, Windows reaches Linux files through
/// WSL's file server, which reads them as root - let in only where `/etc/fuse.conf` has `user_allow_other`.
#[must_use]
pub fn windows_cannot_reach() -> Option<String> {
    // SAFETY: getuid cannot fail and touches no memory.
    let root = unsafe { libc::getuid() } == 0;
    let wsl =
        std::env::var_os("WSL_DISTRO_NAME").is_some() || Path::new("/proc/sys/fs/binfmt_misc/WSLInterop").exists();
    (wsl && !root && !users_may_allow_others()).then(|| {
        "Windows programs cannot open it (\\\\wsl.localhost): add the line `user_allow_other` to /etc/fuse.conf \
         (sudo), and serve it again"
            .to_string()
    })
}

/// Whether `/etc/fuse.conf` lets a user mount for others than themselves (`user_allow_other`): without it, asking to
/// fails the mount.
fn users_may_allow_others() -> bool {
    std::fs::read_to_string("/etc/fuse.conf").is_ok_and(|text| text.lines().any(|l| l.trim() == "user_allow_other"))
}

/// Unmount what a process that ended without unmounting left at a folder: a mount no one serves, which every access
/// fails as not connected.
pub fn clear_stale(at: &Path) {
    if std::fs::metadata(at).is_err_and(|e| e.raw_os_error() == Some(libc::ENOTCONN)) {
        unmount(at);
    }
}
