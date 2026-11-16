//! Files, as `wasi:filesystem` has them: a [`Descriptor`] - a folder or a file - and names within a folder. A
//! [`Filesystem`] opens a folder by its place on disk; everything else is opened, made, renamed and removed by a checked
//! name ([`RelPath`]) within a folder already open, and a file is read and written by position, with nothing to seek.
//!
//! Beyond WASI, what the host's storage needs of a file: its blocks given it ([`Descriptor::allocate`]), a lock on it
//! shared with other processes ([`Descriptor::lock`]), and its bytes mapped into memory ([`Descriptor::map`]), whose
//! pointer is read in place: a mapping costs nothing a read through it would not.

#![allow(clippy::disallowed_methods, reason = "the system's files: what every other reads through")]

use std::any::Any;
use std::ffi::OsString;
use std::fmt;
use std::io;
use std::time::SystemTime;

use pe2_path::RelPath;

use crate::HostPath;

/// Where files and folders are: opened as WASI opens them, a folder by its place and what is in it by name within it
/// (`open_folder`, then the folder's [`Descriptor`]) - and, the host's own, by its place on disk, as what names one
/// whole does (`open`, `stat`, `read`, ...), at no cost a folder's descriptor would add.
pub trait Filesystem: Send + Sync {
    /// The folder at a place on disk; made, with the folders leading to it, where it is not there and `make`. One not
    /// there, and not to be made, fails as it is used.
    ///
    /// # Errors
    /// It cannot be made.
    fn open_folder(&self, at: &HostPath, make: bool) -> io::Result<Box<dyn Descriptor>>;

    /// What is at a place, opened as `how` says: a folder where `how` asks for one.
    ///
    /// # Errors
    /// As the system's open.
    fn open(&self, at: &HostPath, how: Open) -> io::Result<Box<dyn Descriptor>>;

    /// What is at a place, links followed.
    ///
    /// # Errors
    /// Nothing is there, or the system cannot say.
    fn stat(&self, at: &HostPath) -> io::Result<Stat>;

    /// What is at a place, a link as itself.
    ///
    /// # Errors
    /// Nothing is there, or the system cannot say.
    fn stat_link(&self, at: &HostPath) -> io::Result<Stat>;

    /// What a folder holds, in no order.
    ///
    /// # Errors
    /// It is no folder, or cannot be read.
    fn read_directory(&self, at: &HostPath) -> io::Result<Box<[DirEntry]>>;

    /// The whole file at a place.
    ///
    /// # Errors
    /// It is not there, or cannot be read.
    fn read(&self, at: &HostPath) -> io::Result<Vec<u8>>;

    /// Write the file at a place anew: made, or emptied, then `bytes` written.
    ///
    /// # Errors
    /// It cannot be made or written.
    fn write(&self, at: &HostPath, bytes: &[u8]) -> io::Result<()>;

    /// Make a folder, those leading to it there already.
    ///
    /// # Errors
    /// Something is there, or it cannot be made.
    fn create_folder(&self, at: &HostPath) -> io::Result<()>;

    /// Make a folder, with those leading to it, where they are not there.
    ///
    /// # Errors
    /// One cannot be made.
    fn create_folder_all(&self, at: &HostPath) -> io::Result<()>;

    /// Remove a file (or a link).
    ///
    /// # Errors
    /// It is not there, or cannot be removed.
    fn remove_file(&self, at: &HostPath) -> io::Result<()>;

    /// Remove an empty folder.
    ///
    /// # Errors
    /// It is not there, holds something, or cannot be removed.
    fn remove_folder(&self, at: &HostPath) -> io::Result<()>;

    /// Remove a folder and all it holds.
    ///
    /// # Errors
    /// It is not there, or something of it cannot be removed.
    fn remove_all(&self, at: &HostPath) -> io::Result<()>;

    /// Move what is at `from` to `to`, in one step: what was there is replaced.
    ///
    /// # Errors
    /// It is not there, or cannot be moved there.
    fn rename(&self, from: &HostPath, to: &HostPath) -> io::Result<()>;

    /// Copy a file: how many bytes.
    ///
    /// # Errors
    /// It is not there, or cannot be copied there.
    fn copy(&self, from: &HostPath, to: &HostPath) -> io::Result<u64>;

    /// Make `to` another name of the file at `from`.
    ///
    /// # Errors
    /// The system cannot, there.
    fn hard_link(&self, from: &HostPath, to: &HostPath) -> io::Result<()>;

    /// Make `at` a link to `target`, written as the link holds it: a place, or one relative to the link's folder. On
    /// Windows a link is to a file or to a folder, as what `target` names is now.
    ///
    /// # Errors
    /// The system makes no links there, or this process may not.
    fn symlink(&self, target: &std::path::Path, at: &HostPath) -> io::Result<()>;

    /// Say it was last read at `accessed` and last changed at `modified`, each where given: a file or a folder, a
    /// link followed.
    ///
    /// # Errors
    /// It is not there, or its times may not be changed.
    fn set_times(&self, at: &HostPath, accessed: Option<SystemTime>, modified: Option<SystemTime>) -> io::Result<()>;

    /// Where a link leads, as it says.
    ///
    /// # Errors
    /// It is no link.
    fn read_link(&self, at: &HostPath) -> io::Result<std::path::PathBuf>;

    /// Say whether nobody may write it.
    ///
    /// # Errors
    /// It is not there, or cannot be changed.
    fn set_readonly(&self, at: &HostPath, readonly: bool) -> io::Result<()>;

    /// Say whether it runs as a program, as its owner's own (a permission on Unix: read and run by all; elsewhere
    /// nothing).
    ///
    /// # Errors
    /// It is not there, or cannot be changed.
    fn set_executable(&self, at: &HostPath, executable: bool) -> io::Result<()>;

    /// Whether something is there, a link followed.
    fn exists(&self, at: &HostPath) -> bool {
        self.stat(at).is_ok()
    }

    /// Whether a file is there.
    fn is_file(&self, at: &HostPath) -> bool {
        self.stat(at).is_ok_and(|stat| stat.kind == Kind::File)
    }

    /// Whether a folder is there.
    fn is_folder(&self, at: &HostPath) -> bool {
        self.stat(at).is_ok_and(|stat| stat.kind == Kind::Folder)
    }

    /// The whole file at a place, as text.
    ///
    /// # Errors
    /// It is not there, cannot be read, or is no UTF-8.
    fn read_to_string(&self, at: &HostPath) -> io::Result<String> {
        String::from_utf8(self.read(at)?).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }
}

/// How a file is opened: WASI's open flags and descriptor flags.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[expect(clippy::struct_excessive_bools, reason = "the flags as WASI has them, each its own")]
pub struct Open {
    pub read: bool,
    pub write: bool,
    /// Made where it is not there.
    pub create: bool,
    /// Made, and an error where it is there.
    pub exclusive: bool,
    /// Emptied as it is opened.
    pub truncate: bool,
    /// Every write goes to its end, whoever else writes it (`Descriptor::append`). Beyond WASI 0.2's.
    pub append: bool,
    /// A folder, to open what is in it: nothing is read of the place to find out what it is.
    pub directory: bool,
}

impl Open {
    /// To read.
    pub const READ: Open = Open {
        read: true,
        write: false,
        create: false,
        exclusive: false,
        truncate: false,
        append: false,
        directory: false,
    };
    /// To read and write what is there: an error where nothing is.
    pub const CHANGE: Open = Open {
        read: true,
        write: true,
        create: false,
        exclusive: false,
        truncate: false,
        append: false,
        directory: false,
    };
    /// To read and write, made where it is not there, kept as it is where it is.
    pub const KEEP: Open = Open {
        read: true,
        write: true,
        create: true,
        exclusive: false,
        truncate: false,
        append: false,
        directory: false,
    };
    /// To write anew: made, or emptied.
    pub const WRITE: Open = Open {
        read: false,
        write: true,
        create: true,
        exclusive: false,
        truncate: true,
        append: false,
        directory: false,
    };
    /// A folder, to open what is in it.
    pub const FOLDER: Open = Open {
        read: true,
        write: false,
        create: false,
        exclusive: false,
        truncate: false,
        append: false,
        directory: true,
    };
    /// To add to its end, made where it is not there.
    pub const APPEND: Open = Open {
        read: false,
        write: false,
        create: true,
        exclusive: false,
        truncate: false,
        append: true,
        directory: false,
    };
    /// To write, made here and now: an error where it is there.
    pub const NEW: Open = Open {
        read: false,
        write: true,
        create: true,
        exclusive: true,
        truncate: false,
        append: false,
        directory: false,
    };
}

/// What a place is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Folder,
    /// A link, as itself (`stat_at` follows it).
    Link,
    Other,
}

/// What is said of a file or folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stat {
    pub kind: Kind,
    pub size: u64,
    pub modified: Option<SystemTime>,
    /// Nobody may write it.
    pub readonly: bool,
    /// It runs as a program (a permission on Unix; never elsewhere).
    pub executable: bool,
    pub accessed: Option<SystemTime>,
    /// When what is said of it last changed (Unix's `ctime`; none elsewhere).
    pub changed: Option<SystemTime>,
    /// How many names it has (one where the system does not say).
    pub links: u64,
    /// What tells it from every other file of the system: its device and its number on it, where the system says
    /// (Unix).
    pub identity: Option<(u64, u64)>,
}

/// A folder's entry: its name as the system gives it - checked by who takes it as a name - and what it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub name: OsString,
    pub kind: Kind,
}

/// How a lock on a file is held.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockKind {
    /// With others holding it so.
    Shared,
    /// Alone.
    Exclusive,
}

/// A folder or a file, open.
pub trait Descriptor: Send + Sync + fmt::Debug {
    /// What is at `path` within this folder, opened as `how` says: a folder where `how` asks for one.
    ///
    /// # Errors
    /// As the system's open: it is not there, or there already (`exclusive`), or may not be opened so.
    fn open_at(&self, path: &RelPath, how: Open) -> io::Result<Box<dyn Descriptor>>;

    /// Read into `into` from `offset` on: how many bytes, none at the end.
    ///
    /// # Errors
    /// It is no file, or cannot be read.
    fn read_at(&self, into: &mut [u8], offset: u64) -> io::Result<usize>;

    /// Write `bytes` at `offset`: how many were.
    ///
    /// # Errors
    /// It is no file open to write, or cannot be written.
    fn write_at(&self, bytes: &[u8], offset: u64) -> io::Result<usize>;

    /// What it is.
    ///
    /// # Errors
    /// The system cannot say.
    fn stat(&self) -> io::Result<Stat>;

    /// What is at `path` within this folder, links followed.
    ///
    /// # Errors
    /// Nothing is there, or the system cannot say.
    fn stat_at(&self, path: &RelPath) -> io::Result<Stat>;

    /// What this folder holds, in no order.
    ///
    /// # Errors
    /// It is no folder, or cannot be read.
    fn read_directory(&self) -> io::Result<Box<[DirEntry]>>;

    /// Make a folder at `path` within this one, the folders leading to it there already.
    ///
    /// # Errors
    /// Something is there, or it cannot be made.
    fn create_directory_at(&self, path: &RelPath) -> io::Result<()>;

    /// Remove the empty folder at `path` within this one.
    ///
    /// # Errors
    /// It is not there, holds something, or cannot be removed.
    fn remove_directory_at(&self, path: &RelPath) -> io::Result<()>;

    /// Remove the file at `path` within this folder.
    ///
    /// # Errors
    /// It is not there, or cannot be removed.
    fn unlink_file_at(&self, path: &RelPath) -> io::Result<()>;

    /// Move what is at `from` within this folder to `to_path` within `to`, in one step: what was there is replaced.
    ///
    /// # Errors
    /// It is not there, `to` is no folder of the same filesystem, or it cannot be moved.
    fn rename_at(&self, from: &RelPath, to: &dyn Descriptor, to_path: &RelPath) -> io::Result<()>;

    /// Write all of `bytes` at the file's end, as one write: what another process appends goes before or after them,
    /// never among them (a file opened to append). Beyond WASI 0.2's.
    ///
    /// # Errors
    /// It is no file open to append, or cannot be written.
    fn append(&self, bytes: &[u8]) -> io::Result<()>;

    /// Make the file this long: what is past it gone, what is added zeros.
    ///
    /// # Errors
    /// It is no file open to write, or cannot be changed.
    fn set_size(&self, size: u64) -> io::Result<()>;

    /// Say it was last changed at `modified`.
    ///
    /// # Errors
    /// It cannot be changed.
    fn set_modified(&self, modified: SystemTime) -> io::Result<()> {
        self.set_times(None, Some(modified))
    }

    /// Say it was last read at `accessed` and last changed at `modified`, each where given.
    ///
    /// # Errors
    /// It cannot be changed.
    fn set_times(&self, accessed: Option<SystemTime>, modified: Option<SystemTime>) -> io::Result<()>;

    /// Have what was written of it on the disk, its length and times with it - of a folder, what it holds (where the
    /// system has a folder synced: not on Windows, where what a folder holds is on the disk as it changes).
    ///
    /// # Errors
    /// The disk cannot take it.
    fn sync(&self) -> io::Result<()>;

    /// Have what was written of it on the disk, its times aside.
    ///
    /// # Errors
    /// The disk cannot take it.
    fn sync_data(&self) -> io::Result<()>;

    /// Give the file its blocks up to `len`, so that writing them through a mapping finds room - the system's own
    /// way, where it has one, else the file made that long. Beyond WASI: the host's.
    ///
    /// # Errors
    /// The disk has no room, or the file cannot be changed.
    fn allocate(&self, len: u64) -> io::Result<()>;

    /// Hold the file's lock as `kind` says, waiting while another process holds it otherwise. Beyond WASI.
    ///
    /// # Errors
    /// The system cannot lock it.
    fn lock(&self, kind: LockKind) -> io::Result<()>;

    /// Hold the file's lock as `kind` says where nobody holds it otherwise now: whether it was taken. Beyond WASI.
    ///
    /// # Errors
    /// The system cannot lock it.
    fn try_lock(&self, kind: LockKind) -> io::Result<bool>;

    /// Let go of the file's lock. Beyond WASI.
    ///
    /// # Errors
    /// The system cannot let go of it.
    fn unlock(&self) -> io::Result<()>;

    /// Map `len` bytes of the file from its start into memory, shared with every process mapping it - to write
    /// where `writable`. Beyond WASI.
    ///
    /// # Safety
    /// What changes the file elsewhere changes the memory: whoever reads it holds to the file's own protocol.
    ///
    /// # Errors
    /// It cannot be mapped.
    unsafe fn map(&self, len: usize, writable: bool) -> io::Result<Mapping>;

    /// Where it is on disk, where it is on one: for a message, or a program given its path.
    fn place(&self) -> Option<&HostPath>;

    /// Itself, for an implementation to find its own among descriptors (`rename_at`).
    fn as_any(&self) -> &dyn Any;
}

/// Bytes of a file mapped into memory: the pointer kept here, read in place; what keeps the mapping behind it.
pub struct Mapping {
    pointer: *mut u8,
    len: usize,
    held: Box<dyn Mapped>,
}

/// What keeps a mapping, and has what was written through it on the disk.
pub trait Mapped: Send + Sync {
    /// Have what was written through the mapping on the disk.
    ///
    /// # Errors
    /// The disk cannot take it.
    fn flush(&self) -> io::Result<()>;
}

// SAFETY: the memory is shared with other processes anyway; who reads and writes it holds to its own protocol.
unsafe impl Send for Mapping {}
// SAFETY: as `Send`.
unsafe impl Sync for Mapping {}

impl Mapping {
    /// A mapping of `len` bytes from `pointer`, kept by `held` for as long as it lives.
    ///
    /// # Safety
    /// `pointer` is valid for `len` bytes while `held` lives.
    pub unsafe fn new(pointer: *mut u8, len: usize, held: Box<dyn Mapped>) -> Mapping {
        Mapping { pointer, len, held }
    }

    /// Its first byte.
    #[must_use]
    pub fn as_ptr(&self) -> *mut u8 {
        self.pointer
    }

    /// How many bytes it holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether it holds none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Its bytes, as they are now.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: valid for its length while it lives (`new`).
        unsafe { std::slice::from_raw_parts(self.pointer, self.len) }
    }

    /// Have what was written through it on the disk.
    ///
    /// # Errors
    /// The disk cannot take it.
    pub fn flush(&self) -> io::Result<()> {
        self.held.flush()
    }
}

impl fmt::Debug for Mapping {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} bytes mapped", self.len)
    }
}

/// The system's files.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemFilesystem;

impl Filesystem for SystemFilesystem {
    fn open_folder(&self, at: &HostPath, make: bool) -> io::Result<Box<dyn Descriptor>> {
        if make {
            std::fs::create_dir_all(at)?;
        }
        Ok(Box::new(SystemDescriptor::Folder(at.clone())))
    }

    fn open(&self, at: &HostPath, how: Open) -> io::Result<Box<dyn Descriptor>> {
        open(at.clone(), how)
    }

    fn stat(&self, at: &HostPath) -> io::Result<Stat> {
        Ok(stat_of(&std::fs::metadata(at)?))
    }

    fn stat_link(&self, at: &HostPath) -> io::Result<Stat> {
        Ok(stat_of(&std::fs::symlink_metadata(at)?))
    }

    fn read_directory(&self, at: &HostPath) -> io::Result<Box<[DirEntry]>> {
        read_directory(at)
    }

    fn read(&self, at: &HostPath) -> io::Result<Vec<u8>> {
        std::fs::read(at)
    }

    fn write(&self, at: &HostPath, bytes: &[u8]) -> io::Result<()> {
        std::fs::write(at, bytes)
    }

    fn create_folder(&self, at: &HostPath) -> io::Result<()> {
        std::fs::create_dir(at)
    }

    fn create_folder_all(&self, at: &HostPath) -> io::Result<()> {
        std::fs::create_dir_all(at)
    }

    fn remove_file(&self, at: &HostPath) -> io::Result<()> {
        std::fs::remove_file(at)
    }

    fn remove_folder(&self, at: &HostPath) -> io::Result<()> {
        std::fs::remove_dir(at)
    }

    fn remove_all(&self, at: &HostPath) -> io::Result<()> {
        std::fs::remove_dir_all(at)
    }

    fn rename(&self, from: &HostPath, to: &HostPath) -> io::Result<()> {
        std::fs::rename(from, to)
    }

    fn copy(&self, from: &HostPath, to: &HostPath) -> io::Result<u64> {
        std::fs::copy(from, to)
    }

    fn hard_link(&self, from: &HostPath, to: &HostPath) -> io::Result<()> {
        std::fs::hard_link(from, to)
    }

    fn read_link(&self, at: &HostPath) -> io::Result<std::path::PathBuf> {
        std::fs::read_link(at)
    }

    fn symlink(&self, target: &std::path::Path, at: &HostPath) -> io::Result<()> {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, at)
        }
        #[cfg(windows)]
        {
            let named = at.parent().map_or_else(|| target.to_path_buf(), |folder| folder.as_path().join(target));
            if named.is_dir() {
                std::os::windows::fs::symlink_dir(target, at)
            } else {
                std::os::windows::fs::symlink_file(target, at)
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (target, at);
            Err(io::Error::new(io::ErrorKind::Unsupported, "links are made on Unix and Windows"))
        }
    }

    fn set_times(&self, at: &HostPath, accessed: Option<SystemTime>, modified: Option<SystemTime>) -> io::Result<()> {
        SystemDescriptor::Folder(at.clone()).set_times(accessed, modified)
    }

    fn set_readonly(&self, at: &HostPath, readonly: bool) -> io::Result<()> {
        let mut permissions = std::fs::metadata(at)?.permissions();
        if permissions.readonly() != readonly {
            permissions.set_readonly(readonly);
            std::fs::set_permissions(at, permissions)?;
        }
        Ok(())
    }

    fn set_executable(&self, at: &HostPath, executable: bool) -> io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = if executable { 0o555 } else { 0o444 } | (std::fs::metadata(at)?.permissions().mode() & 0o200);
            std::fs::set_permissions(at, std::fs::Permissions::from_mode(mode))
        }
        #[cfg(not(unix))]
        {
            let _ = (at, executable);
            Ok(())
        }
    }

    fn exists(&self, at: &HostPath) -> bool {
        at.as_path().exists()
    }

    fn is_file(&self, at: &HostPath) -> bool {
        at.as_path().is_file()
    }

    fn is_folder(&self, at: &HostPath) -> bool {
        at.as_path().is_dir()
    }

    fn read_to_string(&self, at: &HostPath) -> io::Result<String> {
        std::fs::read_to_string(at)
    }
}

/// What is at a place, opened as `how` says.
fn open(at: HostPath, how: Open) -> io::Result<Box<dyn Descriptor>> {
    let mut options = std::fs::OpenOptions::new();
    options.read(how.read).write(how.write).truncate(how.truncate).append(how.append);
    if how.exclusive {
        options.create_new(true);
    } else {
        options.create(how.create);
    }
    if how.directory {
        return Ok(Box::new(SystemDescriptor::Folder(at)));
    }
    let file = options.open(&at)?;
    Ok(Box::new(SystemDescriptor::File(at, file)))
}

fn read_directory(at: &HostPath) -> io::Result<Box<[DirEntry]>> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(at)? {
        let entry = entry?;
        entries.push(DirEntry { kind: kind_of(entry.file_type()?), name: entry.file_name() });
    }
    Ok(entries.into_boxed_slice())
}

/// A folder or file of the system's: a folder by its place, its names joined to it; a file open.
#[derive(Debug)]
pub enum SystemDescriptor {
    Folder(HostPath),
    File(HostPath, std::fs::File),
}

impl SystemDescriptor {
    fn folder(&self) -> io::Result<&HostPath> {
        match self {
            SystemDescriptor::Folder(at) => Ok(at),
            SystemDescriptor::File(at, _) => Err(io::Error::new(io::ErrorKind::NotADirectory, format!("{at}"))),
        }
    }

    fn file(&self) -> io::Result<&std::fs::File> {
        match self {
            SystemDescriptor::File(_, file) => Ok(file),
            SystemDescriptor::Folder(at) => Err(io::Error::new(io::ErrorKind::IsADirectory, format!("{at}"))),
        }
    }

    /// The system's file, where it is one: for what takes the system's own (a program's handle).
    #[must_use]
    pub fn as_file(&self) -> Option<&std::fs::File> {
        self.file().ok()
    }
}

fn kind_of(file_type: std::fs::FileType) -> Kind {
    if file_type.is_symlink() {
        Kind::Link
    } else if file_type.is_dir() {
        Kind::Folder
    } else if file_type.is_file() {
        Kind::File
    } else {
        Kind::Other
    }
}

fn stat_of(metadata: &std::fs::Metadata) -> Stat {
    #[cfg(unix)]
    let executable = {
        use std::os::unix::fs::PermissionsExt as _;
        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
    };
    #[cfg(not(unix))]
    let executable = false;
    #[cfg(unix)]
    let (changed, links, identity) = {
        use std::os::unix::fs::MetadataExt as _;
        let changed = u64::try_from(metadata.ctime()).ok().map(|seconds| {
            let nanos = u32::try_from(metadata.ctime_nsec()).unwrap_or_default();
            SystemTime::UNIX_EPOCH + std::time::Duration::new(seconds, nanos)
        });
        (changed, metadata.nlink(), Some((metadata.dev(), metadata.ino())))
    };
    #[cfg(not(unix))]
    let (changed, links, identity) = (None, 1, None);
    Stat {
        kind: kind_of(metadata.file_type()),
        size: metadata.len(),
        modified: metadata.modified().ok(),
        readonly: metadata.permissions().readonly(),
        executable,
        accessed: metadata.accessed().ok(),
        changed,
        links,
        identity,
    }
}

impl Descriptor for SystemDescriptor {
    fn open_at(&self, path: &RelPath, how: Open) -> io::Result<Box<dyn Descriptor>> {
        open(self.folder()?.join(path), how)
    }

    fn read_at(&self, into: &mut [u8], offset: u64) -> io::Result<usize> {
        let file = self.file()?;
        #[cfg(unix)]
        {
            std::os::unix::fs::FileExt::read_at(file, into, offset)
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::FileExt::seek_read(file, into, offset)
        }
    }

    fn write_at(&self, bytes: &[u8], offset: u64) -> io::Result<usize> {
        let file = self.file()?;
        #[cfg(unix)]
        {
            std::os::unix::fs::FileExt::write_at(file, bytes, offset)
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::FileExt::seek_write(file, bytes, offset)
        }
    }

    fn stat(&self) -> io::Result<Stat> {
        match self {
            SystemDescriptor::File(_, file) => Ok(stat_of(&file.metadata()?)),
            SystemDescriptor::Folder(at) => Ok(stat_of(&std::fs::metadata(at)?)),
        }
    }

    fn stat_at(&self, path: &RelPath) -> io::Result<Stat> {
        Ok(stat_of(&std::fs::metadata(self.folder()?.join(path))?))
    }

    fn read_directory(&self) -> io::Result<Box<[DirEntry]>> {
        read_directory(self.folder()?)
    }

    fn create_directory_at(&self, path: &RelPath) -> io::Result<()> {
        std::fs::create_dir(self.folder()?.join(path))
    }

    fn remove_directory_at(&self, path: &RelPath) -> io::Result<()> {
        std::fs::remove_dir(self.folder()?.join(path))
    }

    fn unlink_file_at(&self, path: &RelPath) -> io::Result<()> {
        std::fs::remove_file(self.folder()?.join(path))
    }

    fn rename_at(&self, from: &RelPath, to: &dyn Descriptor, to_path: &RelPath) -> io::Result<()> {
        let Some(to) = to.as_any().downcast_ref::<SystemDescriptor>() else {
            return Err(io::Error::new(io::ErrorKind::CrossesDevices, "a folder of another filesystem"));
        };
        std::fs::rename(self.folder()?.join(from), to.folder()?.join(to_path))
    }

    fn append(&self, bytes: &[u8]) -> io::Result<()> {
        use std::io::Write as _;
        self.file()?.write_all(bytes)
    }

    fn set_size(&self, size: u64) -> io::Result<()> {
        self.file()?.set_len(size)
    }

    fn set_times(&self, accessed: Option<SystemTime>, modified: Option<SystemTime>) -> io::Result<()> {
        let mut times = std::fs::FileTimes::new();
        if let Some(accessed) = accessed {
            times = times.set_accessed(accessed);
        }
        if let Some(modified) = modified {
            times = times.set_modified(modified);
        }
        match self {
            SystemDescriptor::File(_, file) => file.set_times(times),
            // A folder's times are set through a handle to it, opened to write its times alone (Windows refuses them
            // through one opened only to read), and as one opens a folder there (backup semantics) - a file alike.
            SystemDescriptor::Folder(at) => {
                #[cfg(windows)]
                let handle = {
                    use std::os::windows::fs::OpenOptionsExt as _;
                    const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
                    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
                    let mut options = std::fs::OpenOptions::new();
                    options.access_mode(FILE_WRITE_ATTRIBUTES).custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
                    options.open(at)?
                };
                #[cfg(not(windows))]
                let handle = std::fs::File::open(at)?;
                handle.set_times(times)
            }
        }
    }

    fn sync(&self) -> io::Result<()> {
        match self {
            SystemDescriptor::File(_, file) => file.sync_all(),
            #[cfg(unix)]
            SystemDescriptor::Folder(at) => std::fs::File::open(at)?.sync_all(),
            #[cfg(not(unix))]
            SystemDescriptor::Folder(_) => Ok(()),
        }
    }

    fn sync_data(&self) -> io::Result<()> {
        self.file()?.sync_data()
    }

    fn allocate(&self, len: u64) -> io::Result<()> {
        allocate(self.file()?, len)
    }

    fn lock(&self, kind: LockKind) -> io::Result<()> {
        let file = self.file()?;
        match kind {
            LockKind::Shared => file.lock_shared(),
            LockKind::Exclusive => file.lock(),
        }
    }

    fn try_lock(&self, kind: LockKind) -> io::Result<bool> {
        let file = self.file()?;
        let tried = match kind {
            LockKind::Shared => file.try_lock_shared(),
            LockKind::Exclusive => file.try_lock(),
        };
        match tried {
            Ok(()) => Ok(true),
            Err(std::fs::TryLockError::WouldBlock) => Ok(false),
            Err(std::fs::TryLockError::Error(e)) => Err(e),
        }
    }

    fn unlock(&self) -> io::Result<()> {
        self.file()?.unlock()
    }

    unsafe fn map(&self, len: usize, writable: bool) -> io::Result<Mapping> {
        let file = self.file()?;
        let options = {
            let mut options = memmap2::MmapOptions::new();
            options.len(len);
            options
        };
        let map = if writable { options.map_raw(file)? } else { options.map_raw_read_only(file)? };
        let pointer = map.as_mut_ptr();
        // SAFETY: the mapping is valid for its length while it is held, and it is held by the `Mapping`.
        Ok(unsafe { Mapping::new(pointer, len, Box::new(Raw(map))) })
    }

    fn place(&self) -> Option<&HostPath> {
        match self {
            SystemDescriptor::Folder(at) | SystemDescriptor::File(at, _) => Some(at),
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct Raw(memmap2::MmapRaw);

impl Mapped for Raw {
    fn flush(&self) -> io::Result<()> {
        self.0.flush()
    }
}

/// On Linux the system gives a file its blocks without writing them.
#[cfg(target_os = "linux")]
fn allocate(file: &std::fs::File, len: u64) -> io::Result<()> {
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
fn allocate(file: &std::fs::File, len: u64) -> io::Result<()> {
    if file.metadata()?.len() < len { file.set_len(len) } else { Ok(()) }
}

/// Elsewhere, what is added is written as zeros.
#[cfg(not(any(target_os = "linux", windows)))]
fn allocate(file: &std::fs::File, len: u64) -> io::Result<()> {
    let zeros = vec![0u8; 1 << 16];
    let mut at = file.metadata()?.len();
    while at < len {
        let part = usize::try_from(len - at).unwrap_or(usize::MAX).min(zeros.len());
        std::os::unix::fs::FileExt::write_all_at(file, &zeros[..part], at)?;
        at += part as u64;
    }
    Ok(())
}

/// A file open to append (`Open::APPEND`) as a stream is written: what writes one, written through.
pub struct Appending(pub Box<dyn Descriptor>);

impl io::Write for Appending {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.append(bytes)?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A file's whole content, read by position.
///
/// # Errors
/// It cannot be read.
pub fn read_whole(file: &dyn Descriptor) -> io::Result<Vec<u8>> {
    let len = usize::try_from(file.stat()?.size).map_err(io::Error::other)?;
    let mut bytes = vec![0; len];
    let mut at = 0;
    while at < bytes.len() {
        match file.read_at(&mut bytes[at..], at as u64)? {
            0 => break,
            read => at += read,
        }
    }
    bytes.truncate(at);
    Ok(bytes)
}

/// Write all of `bytes` at `offset`.
///
/// # Errors
/// It cannot be written, or takes nothing more.
pub fn write_all_at(file: &dyn Descriptor, bytes: &[u8], offset: u64) -> io::Result<()> {
    let mut at = 0;
    while at < bytes.len() {
        match file.write_at(&bytes[at..], offset + at as u64)? {
            0 => return Err(io::Error::from(io::ErrorKind::WriteZero)),
            written => at += written,
        }
    }
    Ok(())
}

/// Make the folder at `path` within `folder`, with those leading to it, where they are not there.
///
/// # Errors
/// One cannot be made.
pub fn create_directory_all_at(folder: &dyn Descriptor, path: &RelPath) -> io::Result<()> {
    let whole = path.as_str();
    if whole.is_empty() {
        return Ok(());
    }
    let ends = whole.match_indices('/').map(|(end, _)| end).chain(std::iter::once(whole.len()));
    for end in ends {
        let step = RelPath::new(&whole[..end]).map_err(io::Error::other)?;
        match folder.create_directory_at(step) {
            Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
            _ => {}
        }
    }
    Ok(())
}

/// The whole file at `path` within `folder`.
///
/// # Errors
/// It is not there, or cannot be read.
pub fn read_file_at(folder: &dyn Descriptor, path: &RelPath) -> io::Result<Vec<u8>> {
    read_whole(&*folder.open_at(path, Open::READ)?)
}

/// Write the file at `path` within `folder` anew: made, or emptied, then `bytes` written.
///
/// # Errors
/// It cannot be made or written.
pub fn write_file_at(folder: &dyn Descriptor, path: &RelPath, bytes: &[u8]) -> io::Result<()> {
    write_all_at(&*folder.open_at(path, Open::WRITE)?, bytes, 0)
}

/// Remove what is at `path` within `folder`, whatever it holds: nothing where nothing is there.
///
/// # Errors
/// Something of it cannot be removed.
pub fn remove_all_at(folder: &dyn Descriptor, path: &RelPath) -> io::Result<()> {
    let stat = match folder.stat_at(path) {
        Ok(stat) => stat,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if stat.kind != Kind::Folder {
        return folder.unlink_file_at(path);
    }
    let inside = folder.open_at(path, Open::FOLDER)?;
    for entry in inside.read_directory()? {
        let name = entry.name.to_str().and_then(|name| pe2_path::Name::new(name).ok()).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{path}: holds {}, a name no path takes", entry.name.display()),
            )
        })?;
        remove_all_at(&*inside, name.as_rel())?;
    }
    drop(inside);
    folder.remove_directory_at(path)
}

/// Whether a folder holds nothing: not where it is not one, or cannot be read.
#[must_use]
pub fn is_empty_folder(fs: &dyn Filesystem, at: &HostPath) -> bool {
    fs.read_directory(at).is_ok_and(|entries| entries.is_empty())
}

/// A file, or a folder with all it holds, copied: a file keeps its permissions.
///
/// # Errors
/// Something of it cannot be read or written.
pub fn copy_all(fs: &dyn Filesystem, from: &HostPath, to: &HostPath) -> io::Result<()> {
    if fs.is_folder(from) {
        fs.create_folder_all(to)?;
        for entry in fs.read_directory(from)? {
            copy_all(fs, &from.entry(&entry.name), &to.entry(&entry.name))?;
        }
        return Ok(());
    }
    fs.copy(from, to).map(drop)
}

/// Have a file on disk - or a folder, with all it holds, and what it holds: best effort, what is gone since is
/// nothing to have. A file is opened to read where that lets it be flushed (a read-only one too), to write elsewhere.
pub fn sync_all(fs: &dyn Filesystem, at: &HostPath) {
    if fs.is_folder(at) {
        for entry in fs.read_directory(at).unwrap_or_default() {
            sync_all(fs, &at.entry(&entry.name));
        }
        if let Ok(folder) = fs.open(at, Open::FOLDER) {
            let _ = folder.sync();
        }
    } else {
        sync_file(fs, at);
    }
}

/// Have a file on disk: best effort, as `sync_all`.
pub fn sync_file(fs: &dyn Filesystem, at: &HostPath) {
    let how = if cfg!(unix) { Open::READ } else { Open { write: true, ..Open::default() } };
    if let Ok(file) = fs.open(at, how) {
        let _ = file.sync();
    }
}

#[cfg(test)]
#[allow(
    clippy::disallowed_methods,
    reason = "a test's fixtures: the machine's time, places, files and environment, which nothing tested reads"
)]
mod tests {
    use super::{Filesystem as _, Kind, Open, SystemFilesystem};
    use crate::HostPath;
    use std::time::{Duration, SystemTime};

    fn scratch(name: &str) -> HostPath {
        let dir = std::env::temp_dir().join(format!("pe2-io-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        HostPath::new(&dir).unwrap()
    }

    fn at(dir: &HostPath, name: &str) -> HostPath {
        dir.entry(std::ffi::OsStr::new(name))
    }

    #[test]
    fn a_link_leads_to_a_file_or_a_folder_as_written_where_the_system_lets_this_user_make_one() {
        let (fs, dir) = (SystemFilesystem, scratch("links"));
        fs.write(&at(&dir, "target.txt"), b"held").unwrap();
        fs.create_folder(&at(&dir, "folder")).unwrap();
        fs.write(&at(&at(&dir, "folder"), "inner.txt"), b"in").unwrap();
        let made = fs.symlink(std::path::Path::new("target.txt"), &at(&dir, "to-file"));
        // Windows makes links only for a user it lets: an administrator, or with developer mode on.
        if cfg!(windows) && made.as_ref().is_err_and(|e| e.raw_os_error() == Some(1314)) {
            eprintln!("this user may not make links here: what makes them is not tested");
            return;
        }
        made.unwrap();
        // As written - relative to where the link is - and followed to what it names.
        assert_eq!(fs.read_link(&at(&dir, "to-file")).unwrap(), std::path::Path::new("target.txt"));
        assert_eq!(fs.stat_link(&at(&dir, "to-file")).unwrap().kind, Kind::Link);
        let followed = fs.stat(&at(&dir, "to-file")).unwrap();
        assert_eq!((followed.kind, followed.size), (Kind::File, 4));
        assert_eq!(fs.read(&at(&dir, "to-file")).unwrap(), b"held");
        // To a folder: what is in it is reached through it (on Windows, a link to a folder is one of its own kind).
        fs.symlink(std::path::Path::new("folder"), &at(&dir, "to-folder")).unwrap();
        assert_eq!(fs.stat(&at(&dir, "to-folder")).unwrap().kind, Kind::Folder);
        let listed = fs.read_directory(&at(&dir, "to-folder")).unwrap();
        assert_eq!(listed.iter().map(|entry| entry.name.clone()).collect::<Vec<_>>(), ["inner.txt"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_and_a_folder_are_said_to_be_read_and_changed_when_they_are_told() {
        let (fs, dir) = (SystemFilesystem, scratch("times"));
        let file = at(&dir, "a.txt");
        fs.write(&file, b"a").unwrap();
        let folder = at(&dir, "folder");
        fs.create_folder(&folder).unwrap();
        let (read, changed) = (
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000),
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        );
        for place in [&file, &folder] {
            fs.set_times(place, Some(read), Some(changed)).unwrap();
            let stat = fs.stat(place).unwrap();
            assert_eq!((stat.accessed, stat.modified), (Some(read), Some(changed)), "{place}");
            // One of them alone leaves the other as it was.
            let later = changed + Duration::from_secs(60);
            fs.set_times(place, None, Some(later)).unwrap();
            let stat = fs.stat(place).unwrap();
            assert_eq!((stat.accessed, stat.modified), (Some(read), Some(later)), "{place}");
        }
        // Through a file held open, as a component's descriptor does.
        let held = fs.open(&file, Open::CHANGE).unwrap();
        held.set_times(None, Some(changed)).unwrap();
        assert_eq!(held.stat().unwrap().modified, Some(changed));
        drop(held);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
