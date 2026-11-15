//! `wasi:filesystem` as a component is given it, over the host's I/O (`pe2_io::Filesystem`): what an unsandboxed
//! component reads and writes of the disk goes where the `Io` it runs with says, as the host's own files do. A sandboxed
//! one is given no folder.
//!
//! A descriptor is a folder's place, or a file held open. A path given within a folder is taken as the system names
//! it, part by part: `.` is the folder, `..` the one it is in, but never above the folder given (a preopen), and a path
//! beginning with `/` is refused, as WASI has it. What links lead to is the system's to follow. Each call does its I/O
//! as it is made: what runs an unsandboxed component may block on it (`allow_blocking_current_thread`).

use pe2_io::HostPath;
use std::ffi::OsStr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::SystemTime;
use wasmtime::StoreContextMut;
use wasmtime::component::{
    Access, Accessor, Destination, FutureReader, HasData, Resource, ResourceTable, Source, StreamConsumer,
    StreamProducer, StreamReader, StreamResult, VecBuffer,
};

mod bindings {
    wasmtime::component::bindgen!({
        path: "wit/filesystem",
        world: "wasi:filesystem/imports",
        imports: {
            "wasi:filesystem/types.[method]descriptor.read-via-stream": store | trappable,
            "wasi:filesystem/types.[method]descriptor.write-via-stream": store | trappable,
            "wasi:filesystem/types.[method]descriptor.append-via-stream": store | trappable,
            "wasi:filesystem/types.[method]descriptor.read-directory": store | trappable,
            default: trappable,
        },
        with: {
            "wasi:filesystem/types.descriptor": super::Opened,
        },
        trappable_error_type: {
            "wasi:filesystem/types.error-code" => super::FilesError,
        },
    });
}

use bindings::wasi::clocks::system_clock::Instant;
use bindings::wasi::filesystem::{preopens, types};
use types::{
    Advice, DescriptorFlags, DescriptorStat, DescriptorType, DirectoryEntry, ErrorCode, Filesize, MetadataHashValue,
    NewTimestamp, OpenFlags, PathFlags,
};

/// What a call fails with: an error code the component is told, or a trap.
pub type FilesError = wasmtime_wasi::TrappableError<ErrorCode>;
type FilesResult<T> = Result<T, FilesError>;

/// How much of a file a read gives at a time, where the reader is the host.
const PIECE: usize = 64 << 10;

/// A descriptor: a folder by its place, or a file held open.
#[derive(Clone)]
pub struct Opened {
    place: HostPath,
    /// The folder it was found within, at the top: what `..` stops at.
    root: HostPath,
    file: Option<Arc<dyn pe2_io::Descriptor>>,
    read: bool,
    write: bool,
    /// What is in it may be made, renamed and removed (a folder).
    mutate: bool,
    io: Arc<dyn pe2_io::Io>,
}

impl Opened {
    /// A folder given to a component, at the top of what it reaches through it: read, and changed where `mutate`.
    #[must_use]
    pub fn folder(place: HostPath, mutate: bool, io: Arc<dyn pe2_io::Io>) -> Opened {
        Opened { root: place.clone(), place, file: None, read: true, write: false, mutate, io }
    }

    fn fs(&self) -> &dyn pe2_io::Filesystem {
        self.io.filesystem()
    }

    fn file(&self) -> FilesResult<&Arc<dyn pe2_io::Descriptor>> {
        self.file.as_ref().ok_or_else(|| ErrorCode::IsDirectory.into())
    }

    fn writable(&self) -> FilesResult<&Arc<dyn pe2_io::Descriptor>> {
        let file = self.file()?;
        if !self.write {
            return Err(ErrorCode::NotPermitted.into());
        }
        Ok(file)
    }

    fn changing(&self) -> FilesResult<()> {
        if self.file.is_some() {
            return Err(ErrorCode::NotDirectory.into());
        }
        if !self.mutate {
            return Err(ErrorCode::NotPermitted.into());
        }
        Ok(())
    }

    /// The place `path` names within this folder.
    fn at(&self, path: &str) -> FilesResult<HostPath> {
        if self.file.is_some() {
            return Err(ErrorCode::NotDirectory.into());
        }
        if path.starts_with('/') {
            return Err(ErrorCode::NotPermitted.into());
        }
        let mut at = self.place.clone();
        for part in path.split('/') {
            match part {
                "" | "." => {}
                ".." if at == self.root => return Err(ErrorCode::NotPermitted.into()),
                ".." => at = at.parent().ok_or(ErrorCode::NotPermitted)?,
                // A name the system would take as more than one, or as a place of its own, is none.
                name if cfg!(windows) && (name.contains('\\') || name.contains(':')) => {
                    return Err(ErrorCode::Invalid.into());
                }
                name => at = at.entry(OsStr::new(name)),
            }
        }
        Ok(at)
    }

    fn stat(&self) -> FilesResult<pe2_io::Stat> {
        Ok(match &self.file {
            Some(file) => file.stat().coded()?,
            None => self.fs().stat(&self.place).coded()?,
        })
    }

    fn stat_at(&self, follow: PathFlags, path: &str) -> FilesResult<pe2_io::Stat> {
        let at = self.at(path)?;
        let fs = self.fs();
        Ok(if follow.contains(PathFlags::SYMLINK_FOLLOW) {
            fs.stat(&at).coded()?
        } else {
            fs.stat_link(&at).coded()?
        })
    }

    fn now(&self) -> SystemTime {
        self.io.clock().wall()
    }

    fn times(
        &self,
        accessed: NewTimestamp,
        modified: NewTimestamp,
    ) -> FilesResult<(Option<SystemTime>, Option<SystemTime>)> {
        let time = |stamp: NewTimestamp| -> FilesResult<Option<SystemTime>> {
            Ok(match stamp {
                NewTimestamp::NoChange => None,
                NewTimestamp::Now => Some(self.now()),
                NewTimestamp::Timestamp(at) => Some(system_time(at)?),
            })
        };
        Ok((time(accessed)?, time(modified)?))
    }

    fn open_at(&self, follow: PathFlags, path: &str, open: OpenFlags, flags: DescriptorFlags) -> FilesResult<Opened> {
        let at = self.at(path)?;
        let fs = self.fs();
        let writes = flags.contains(DescriptorFlags::WRITE) || open.intersects(OpenFlags::CREATE | OpenFlags::TRUNCATE);
        if writes && !self.mutate {
            return Err(ErrorCode::NotPermitted.into());
        }
        if !follow.contains(PathFlags::SYMLINK_FOLLOW)
            && fs.stat_link(&at).is_ok_and(|stat| stat.kind == pe2_io::Kind::Link)
        {
            return Err(ErrorCode::Loop.into());
        }
        let mutate = self.mutate && flags.contains(DescriptorFlags::MUTATE_DIRECTORY);
        let folder = |place: HostPath| Opened {
            place,
            root: self.root.clone(),
            file: None,
            read: flags.contains(DescriptorFlags::READ),
            write: false,
            mutate,
            io: self.io.clone(),
        };
        if open.contains(OpenFlags::DIRECTORY) {
            if open.intersects(OpenFlags::CREATE | OpenFlags::TRUNCATE) || flags.contains(DescriptorFlags::WRITE) {
                return Err(ErrorCode::Invalid.into());
            }
            return match fs.stat(&at).coded()?.kind {
                pe2_io::Kind::Folder => Ok(folder(at)),
                _ => Err(ErrorCode::NotDirectory.into()),
            };
        }
        let how = pe2_io::Open {
            read: flags.contains(DescriptorFlags::READ) || !flags.contains(DescriptorFlags::WRITE),
            write: flags.contains(DescriptorFlags::WRITE),
            create: open.contains(OpenFlags::CREATE),
            exclusive: open.contains(OpenFlags::EXCLUSIVE),
            truncate: open.contains(OpenFlags::TRUNCATE),
            append: false,
            directory: false,
        };
        let file = fs.open(&at, how).coded()?;
        if file.stat().coded()?.kind == pe2_io::Kind::Folder {
            return Ok(folder(at));
        }
        Ok(Opened {
            place: at,
            root: self.root.clone(),
            file: Some(Arc::from(file)),
            read: how.read,
            write: how.write,
            mutate: false,
            io: self.io.clone(),
        })
    }
}

/// The component's side of what it is given: the table its descriptors are in, and the folders it starts with.
pub struct FilesView<'a> {
    pub table: &'a mut ResourceTable,
    pub preopens: &'a [(Opened, String)],
}

/// What `wasi:filesystem` is linked as (`add_to_linker`).
pub struct Files;

impl HasData for Files {
    type Data<'a> = FilesView<'a>;
}

/// Link `wasi:filesystem` as this module gives it, `view` giving each call what it works in.
///
/// # Errors
/// It is linked already.
pub fn add_to_linker<T: Send + 'static>(
    linker: &mut wasmtime::component::Linker<T>,
    view: fn(&mut T) -> FilesView<'_>,
) -> wasmtime::Result<()> {
    types::add_to_linker::<T, Files>(linker, view)?;
    preopens::add_to_linker::<T, Files>(linker, view)?;
    Ok(())
}

fn opened(table: &ResourceTable, fd: &Resource<Opened>) -> FilesResult<Opened> {
    table.get(fd).cloned().map_err(FilesError::trap)
}

/// What the system's error is to a component.
fn code_of(e: &std::io::Error) -> ErrorCode {
    use std::io::ErrorKind as K;
    #[cfg(unix)]
    if let Some(code) = e.raw_os_error() {
        match code {
            libc::ELOOP => return ErrorCode::Loop,
            libc::ENAMETOOLONG => return ErrorCode::NameTooLong,
            libc::EBADF => return ErrorCode::BadDescriptor,
            libc::EMLINK => return ErrorCode::TooManyLinks,
            libc::ETXTBSY => return ErrorCode::TextFileBusy,
            libc::EOVERFLOW => return ErrorCode::Overflow,
            libc::EPERM => return ErrorCode::NotPermitted,
            libc::EACCES => return ErrorCode::Access,
            _ => {}
        }
    }
    match e.kind() {
        K::NotFound => ErrorCode::NoEntry,
        K::PermissionDenied => ErrorCode::Access,
        K::AlreadyExists => ErrorCode::Exist,
        K::DirectoryNotEmpty => ErrorCode::NotEmpty,
        K::IsADirectory => ErrorCode::IsDirectory,
        K::NotADirectory => ErrorCode::NotDirectory,
        K::InvalidInput | K::InvalidFilename => ErrorCode::Invalid,
        K::ReadOnlyFilesystem => ErrorCode::ReadOnly,
        K::StorageFull => ErrorCode::InsufficientSpace,
        K::QuotaExceeded => ErrorCode::Quota,
        K::FileTooLarge => ErrorCode::FileTooLarge,
        K::ResourceBusy => ErrorCode::Busy,
        K::CrossesDevices => ErrorCode::CrossDevice,
        K::TooManyLinks => ErrorCode::TooManyLinks,
        K::Deadlock => ErrorCode::Deadlock,
        K::Interrupted => ErrorCode::Interrupted,
        K::Unsupported => ErrorCode::Unsupported,
        K::OutOfMemory => ErrorCode::InsufficientMemory,
        K::NotSeekable => ErrorCode::InvalidSeek,
        K::BrokenPipe => ErrorCode::Pipe,
        _ => ErrorCode::Io,
    }
}

/// A system's result, as a component is told it.
trait Coded<T> {
    fn coded(self) -> FilesResult<T>;
}

impl<T> Coded<T> for std::io::Result<T> {
    fn coded(self) -> FilesResult<T> {
        self.map_err(|e| code_of(&e).into())
    }
}

fn system_time(at: Instant) -> FilesResult<SystemTime> {
    let seconds = std::time::Duration::from_secs(at.seconds.unsigned_abs());
    let whole = if at.seconds >= 0 {
        SystemTime::UNIX_EPOCH.checked_add(seconds)
    } else {
        SystemTime::UNIX_EPOCH.checked_sub(seconds)
    };
    whole
        .and_then(|whole| whole.checked_add(std::time::Duration::from_nanos(u64::from(at.nanoseconds))))
        .ok_or_else(|| ErrorCode::Overflow.into())
}

fn instant(time: SystemTime) -> Instant {
    match time.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(after) => {
            Instant { seconds: i64::try_from(after.as_secs()).unwrap_or(i64::MAX), nanoseconds: after.subsec_nanos() }
        }
        Err(before) => {
            let before = before.duration();
            let seconds = i64::try_from(before.as_secs()).unwrap_or(i64::MAX);
            match before.subsec_nanos() {
                0 => Instant { seconds: -seconds, nanoseconds: 0 },
                nanos => Instant { seconds: -seconds - 1, nanoseconds: 1_000_000_000 - nanos },
            }
        }
    }
}

fn type_of(kind: pe2_io::Kind) -> DescriptorType {
    match kind {
        pe2_io::Kind::File => DescriptorType::RegularFile,
        pe2_io::Kind::Folder => DescriptorType::Directory,
        pe2_io::Kind::Link => DescriptorType::SymbolicLink,
        pe2_io::Kind::Other => DescriptorType::Other(None),
    }
}

fn described(stat: &pe2_io::Stat) -> DescriptorStat {
    DescriptorStat {
        type_: type_of(stat.kind),
        link_count: stat.links,
        size: stat.size,
        data_access_timestamp: stat.accessed.map(instant),
        data_modification_timestamp: stat.modified.map(instant),
        status_change_timestamp: stat.changed.map(instant),
    }
}

/// What tells a file's state from another's: what it is, its length and its times.
fn hashed(stat: &pe2_io::Stat, place: &HostPath) -> MetadataHashValue {
    use std::hash::{Hash as _, Hasher as _};
    let hash = |salt: u64| {
        let mut hasher = std::hash::DefaultHasher::new();
        salt.hash(&mut hasher);
        match stat.identity {
            Some(identity) => identity.hash(&mut hasher),
            None => place.hash(&mut hasher),
        }
        (stat.size, stat.modified, stat.changed).hash(&mut hasher);
        hasher.finish()
    };
    MetadataHashValue { lower: hash(0), upper: hash(1) }
}

/// What a read through a stream gives: the file's bytes from where it starts, as they are asked for.
struct Reading {
    file: Arc<dyn pe2_io::Descriptor>,
    offset: u64,
    result: Option<tokio::sync::oneshot::Sender<Result<(), ErrorCode>>>,
}

impl Reading {
    fn close(&mut self, result: Result<(), ErrorCode>) {
        if let Some(said) = self.result.take() {
            let _ = said.send(result);
        }
    }
}

impl Drop for Reading {
    fn drop(&mut self) {
        self.close(Ok(()));
    }
}

impl<D> StreamProducer<D> for Reading {
    type Item = u8;
    type Buffer = VecBuffer<u8>;

    fn poll_produce<'a>(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        store: StoreContextMut<'a, D>,
        dst: Destination<'a, Self::Item, Self::Buffer>,
        _: bool,
    ) -> Poll<wasmtime::Result<StreamResult>> {
        let mut dst = dst.as_direct(store, PIECE);
        let into = dst.remaining();
        if into.is_empty() {
            return Poll::Ready(Ok(StreamResult::Completed));
        }
        Poll::Ready(Ok(match self.file.read_at(into, self.offset) {
            Ok(0) => {
                self.close(Ok(()));
                StreamResult::Dropped
            }
            Ok(read) => {
                dst.mark_written(read);
                self.offset += read as u64;
                StreamResult::Completed
            }
            Err(e) => {
                self.close(Err(code_of(&e)));
                StreamResult::Dropped
            }
        }))
    }
}

/// What a write through a stream takes: the bytes it is given, written where it starts on, or each at the file's end.
struct Writing {
    file: Arc<dyn pe2_io::Descriptor>,
    /// Where the next is written; none: at the end, as it is then.
    offset: Option<u64>,
    result: Option<tokio::sync::oneshot::Sender<Result<(), ErrorCode>>>,
}

impl Writing {
    fn close(&mut self, result: Result<(), ErrorCode>) {
        if let Some(said) = self.result.take() {
            let _ = said.send(result);
        }
    }

    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let at = match self.offset {
            Some(at) => at,
            None => self.file.stat()?.size,
        };
        let written = self.file.write_at(bytes, at)?;
        if let Some(offset) = &mut self.offset {
            *offset += written as u64;
        }
        Ok(written)
    }
}

impl Drop for Writing {
    fn drop(&mut self) {
        self.close(Ok(()));
    }
}

impl<D> StreamConsumer<D> for Writing {
    type Item = u8;

    fn poll_consume(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        store: StoreContextMut<D>,
        src: Source<Self::Item>,
        _: bool,
    ) -> Poll<wasmtime::Result<StreamResult>> {
        let mut src = src.as_direct(store);
        let bytes = src.remaining().to_vec();
        Poll::Ready(Ok(match self.write(&bytes) {
            Ok(written) => {
                src.mark_read(written);
                StreamResult::Completed
            }
            Err(e) => {
                self.close(Err(code_of(&e)));
                StreamResult::Dropped
            }
        }))
    }
}

/// A future that is what was said through `told`, once it is.
fn told(
    store: impl wasmtime::AsContextMut,
    told: tokio::sync::oneshot::Receiver<Result<(), ErrorCode>>,
) -> wasmtime::Result<FutureReader<Result<(), ErrorCode>>> {
    FutureReader::new(store, async move { Ok::<_, wasmtime::Error>(told.await.unwrap_or(Ok(()))) })
}

/// A future that is `result` at once.
fn at_once(
    store: impl wasmtime::AsContextMut,
    result: Result<(), ErrorCode>,
) -> wasmtime::Result<FutureReader<Result<(), ErrorCode>>> {
    FutureReader::new(store, async move { Ok::<_, wasmtime::Error>(result) })
}

fn failed(e: FilesError) -> wasmtime::Result<ErrorCode> {
    e.downcast()
}

fn write_via(
    mut store: Access<'_, impl Sized, Files>,
    fd: &Resource<Opened>,
    data: StreamReader<u8>,
    offset: Option<u64>,
) -> wasmtime::Result<FutureReader<Result<(), ErrorCode>>> {
    let file = opened(store.get().table, fd).and_then(|fd| fd.writable().cloned());
    match file {
        Ok(file) => {
            let (result, said) = tokio::sync::oneshot::channel();
            data.pipe(&mut store, Writing { file, offset, result: Some(result) })?;
            told(&mut store, said)
        }
        Err(e) => {
            let mut data = data;
            data.close(&mut store)?;
            at_once(&mut store, Err(failed(e)?))
        }
    }
}

impl types::Host for FilesView<'_> {
    fn convert_error_code(&mut self, error: FilesError) -> wasmtime::Result<ErrorCode> {
        error.downcast()
    }
}

impl types::HostDescriptor for FilesView<'_> {
    fn drop(&mut self, fd: Resource<Opened>) -> wasmtime::Result<()> {
        self.table.delete(fd)?;
        Ok(())
    }
}

impl preopens::Host for FilesView<'_> {
    fn get_directories(&mut self) -> wasmtime::Result<Vec<(Resource<Opened>, String)>> {
        let mut given = Vec::with_capacity(self.preopens.len());
        for (folder, name) in self.preopens {
            given.push((self.table.push(folder.clone())?, name.clone()));
        }
        Ok(given)
    }
}

/// The descriptor a call is about, as it is now.
fn of<U>(store: &Accessor<U, Files>, fd: &Resource<Opened>) -> FilesResult<Opened> {
    store.with(|mut view| opened(view.get().table, fd))
}

#[allow(
    clippy::unused_async_trait_impl,
    reason = "WASI's calls are asynchronous; each does its I/O as it is made (`allow_blocking_current_thread`)"
)]
impl<U> types::HostDescriptorWithStore<U> for Files {
    fn read_via_stream(
        mut store: Access<U, Self>,
        fd: Resource<Opened>,
        offset: Filesize,
    ) -> wasmtime::Result<(StreamReader<u8>, FutureReader<Result<(), ErrorCode>>)> {
        let fd = opened(store.get().table, &fd);
        match fd.and_then(|fd| fd.file().cloned()) {
            Ok(file) => {
                let (result, said) = tokio::sync::oneshot::channel();
                let reading = Reading { file, offset, result: Some(result) };
                Ok((StreamReader::new(&mut store, reading)?, told(&mut store, said)?))
            }
            Err(e) => {
                let code = failed(e)?;
                Ok((StreamReader::new(&mut store, Vec::<u8>::new())?, at_once(&mut store, Err(code))?))
            }
        }
    }

    fn write_via_stream(
        store: Access<'_, U, Self>,
        fd: Resource<Opened>,
        data: StreamReader<u8>,
        offset: Filesize,
    ) -> wasmtime::Result<FutureReader<Result<(), ErrorCode>>> {
        write_via(store, &fd, data, Some(offset))
    }

    fn append_via_stream(
        store: Access<'_, U, Self>,
        fd: Resource<Opened>,
        data: StreamReader<u8>,
    ) -> wasmtime::Result<FutureReader<Result<(), ErrorCode>>> {
        write_via(store, &fd, data, None)
    }

    async fn advise(
        store: &Accessor<U, Self>,
        fd: Resource<Opened>,
        _offset: Filesize,
        _length: Filesize,
        _advice: Advice,
    ) -> FilesResult<()> {
        // Advice is taken as given: nothing is done of it.
        of(store, &fd)?.file()?;
        Ok(())
    }

    async fn sync_data(store: &Accessor<U, Self>, fd: Resource<Opened>) -> FilesResult<()> {
        let fd = of(store, &fd)?;
        match &fd.file {
            Some(file) => file.sync_data().coded()?,
            None => fd.fs().open(&fd.place, pe2_io::Open::FOLDER).and_then(|folder| folder.sync()).coded()?,
        }
        Ok(())
    }

    async fn get_flags(store: &Accessor<U, Self>, fd: Resource<Opened>) -> FilesResult<DescriptorFlags> {
        let fd = of(store, &fd)?;
        let mut flags = DescriptorFlags::empty();
        for (has, flag) in [
            (fd.read, DescriptorFlags::READ),
            (fd.write, DescriptorFlags::WRITE),
            (fd.mutate, DescriptorFlags::MUTATE_DIRECTORY),
        ] {
            if has {
                flags |= flag;
            }
        }
        Ok(flags)
    }

    async fn get_type(store: &Accessor<U, Self>, fd: Resource<Opened>) -> FilesResult<DescriptorType> {
        Ok(type_of(of(store, &fd)?.stat()?.kind))
    }

    async fn set_size(store: &Accessor<U, Self>, fd: Resource<Opened>, size: Filesize) -> FilesResult<()> {
        of(store, &fd)?.writable()?.set_size(size).coded()?;
        Ok(())
    }

    async fn set_times(
        store: &Accessor<U, Self>,
        fd: Resource<Opened>,
        accessed: NewTimestamp,
        modified: NewTimestamp,
    ) -> FilesResult<()> {
        let fd = of(store, &fd)?;
        let (accessed, modified) = fd.times(accessed, modified)?;
        match &fd.file {
            Some(file) => file.set_times(accessed, modified).coded()?,
            None => fd.fs().set_times(&fd.place, accessed, modified).coded()?,
        }
        Ok(())
    }

    fn read_directory(
        mut store: Access<'_, U, Self>,
        fd: Resource<Opened>,
    ) -> wasmtime::Result<(StreamReader<DirectoryEntry>, FutureReader<Result<(), ErrorCode>>)> {
        let listed = opened(store.get().table, &fd).and_then(|fd| {
            if fd.file.is_some() {
                return Err(ErrorCode::NotDirectory.into());
            }
            fd.fs().read_directory(&fd.place).coded()
        });
        let (entries, result) = match listed {
            Ok(listed) => {
                let mut entries = Vec::with_capacity(listed.len());
                let mut result = Ok(());
                for entry in listed {
                    // A name that is no text is no name a component is told.
                    let Ok(name) = entry.name.into_string() else {
                        result = Err(ErrorCode::IllegalByteSequence);
                        break;
                    };
                    entries.push(DirectoryEntry { type_: type_of(entry.kind), name });
                }
                (entries, result)
            }
            Err(e) => (Vec::new(), Err(failed(e)?)),
        };
        Ok((StreamReader::new(&mut store, entries)?, at_once(&mut store, result)?))
    }

    async fn sync(store: &Accessor<U, Self>, fd: Resource<Opened>) -> FilesResult<()> {
        let fd = of(store, &fd)?;
        match &fd.file {
            Some(file) => file.sync().coded()?,
            None => fd.fs().open(&fd.place, pe2_io::Open::FOLDER).and_then(|folder| folder.sync()).coded()?,
        }
        Ok(())
    }

    async fn create_directory_at(store: &Accessor<U, Self>, fd: Resource<Opened>, path: String) -> FilesResult<()> {
        let fd = of(store, &fd)?;
        fd.changing()?;
        fd.fs().create_folder(&fd.at(&path)?).coded()?;
        Ok(())
    }

    async fn stat(store: &Accessor<U, Self>, fd: Resource<Opened>) -> FilesResult<DescriptorStat> {
        Ok(described(&of(store, &fd)?.stat()?))
    }

    async fn stat_at(
        store: &Accessor<U, Self>,
        fd: Resource<Opened>,
        follow: PathFlags,
        path: String,
    ) -> FilesResult<DescriptorStat> {
        Ok(described(&of(store, &fd)?.stat_at(follow, &path)?))
    }

    async fn set_times_at(
        store: &Accessor<U, Self>,
        fd: Resource<Opened>,
        _follow: PathFlags,
        path: String,
        accessed: NewTimestamp,
        modified: NewTimestamp,
    ) -> FilesResult<()> {
        let fd = of(store, &fd)?;
        let (accessed, modified) = fd.times(accessed, modified)?;
        fd.fs().set_times(&fd.at(&path)?, accessed, modified).coded()?;
        Ok(())
    }

    async fn link_at(
        store: &Accessor<U, Self>,
        fd: Resource<Opened>,
        _follow: PathFlags,
        old_path: String,
        new_fd: Resource<Opened>,
        new_path: String,
    ) -> FilesResult<()> {
        let (fd, new_fd) = (of(store, &fd)?, of(store, &new_fd)?);
        new_fd.changing()?;
        fd.fs().hard_link(&fd.at(&old_path)?, &new_fd.at(&new_path)?).coded()?;
        Ok(())
    }

    async fn open_at(
        store: &Accessor<U, Self>,
        fd: Resource<Opened>,
        follow: PathFlags,
        path: String,
        open: OpenFlags,
        flags: DescriptorFlags,
    ) -> FilesResult<Resource<Opened>> {
        let opened = of(store, &fd)?.open_at(follow, &path, open, flags)?;
        store.with(|mut view| view.get().table.push(opened)).map_err(FilesError::trap)
    }

    async fn readlink_at(store: &Accessor<U, Self>, fd: Resource<Opened>, path: String) -> FilesResult<String> {
        let fd = of(store, &fd)?;
        let target = fd.fs().read_link(&fd.at(&path)?).coded()?;
        target.into_os_string().into_string().map_err(|_| ErrorCode::IllegalByteSequence.into())
    }

    async fn remove_directory_at(store: &Accessor<U, Self>, fd: Resource<Opened>, path: String) -> FilesResult<()> {
        let fd = of(store, &fd)?;
        fd.changing()?;
        fd.fs().remove_folder(&fd.at(&path)?).coded()?;
        Ok(())
    }

    async fn rename_at(
        store: &Accessor<U, Self>,
        fd: Resource<Opened>,
        old_path: String,
        new_fd: Resource<Opened>,
        new_path: String,
    ) -> FilesResult<()> {
        let (fd, new_fd) = (of(store, &fd)?, of(store, &new_fd)?);
        fd.changing()?;
        new_fd.changing()?;
        fd.fs().rename(&fd.at(&old_path)?, &new_fd.at(&new_path)?).coded()?;
        Ok(())
    }

    async fn symlink_at(
        store: &Accessor<U, Self>,
        fd: Resource<Opened>,
        old_path: String,
        new_path: String,
    ) -> FilesResult<()> {
        let fd = of(store, &fd)?;
        fd.changing()?;
        // A link leads within what it is in: to no place of its own.
        if old_path.starts_with('/') {
            return Err(ErrorCode::NotPermitted.into());
        }
        fd.fs().symlink(std::path::Path::new(&old_path), &fd.at(&new_path)?).coded()?;
        Ok(())
    }

    async fn unlink_file_at(store: &Accessor<U, Self>, fd: Resource<Opened>, path: String) -> FilesResult<()> {
        let fd = of(store, &fd)?;
        fd.changing()?;
        fd.fs().remove_file(&fd.at(&path)?).coded()?;
        Ok(())
    }

    async fn is_same_object(
        store: &Accessor<U, Self>,
        fd: Resource<Opened>,
        other: Resource<Opened>,
    ) -> wasmtime::Result<bool> {
        let (Ok(fd), Ok(other)) = (of(store, &fd), of(store, &other)) else { return Ok(false) };
        Ok(match (fd.stat(), other.stat()) {
            (Ok(a), Ok(b)) => match (a.identity, b.identity) {
                (Some(a), Some(b)) => a == b,
                _ => fd.place == other.place,
            },
            _ => false,
        })
    }

    async fn metadata_hash(store: &Accessor<U, Self>, fd: Resource<Opened>) -> FilesResult<MetadataHashValue> {
        let fd = of(store, &fd)?;
        Ok(hashed(&fd.stat()?, &fd.place))
    }

    async fn metadata_hash_at(
        store: &Accessor<U, Self>,
        fd: Resource<Opened>,
        follow: PathFlags,
        path: String,
    ) -> FilesResult<MetadataHashValue> {
        let fd = of(store, &fd)?;
        Ok(hashed(&fd.stat_at(follow, &path)?, &fd.at(&path)?))
    }
}

#[cfg(test)]
#[allow(
    clippy::disallowed_methods,
    reason = "a test's fixtures: the machine's time, places, files and environment, which nothing tested reads"
)]
mod tests {
    use super::{ErrorCode, FilesError, Opened, instant, system_time};
    use pe2_io::HostPath;
    use sfs::testing::At as _;

    fn code(result: Result<HostPath, FilesError>) -> ErrorCode {
        result.unwrap_err().downcast().unwrap()
    }

    #[test]
    fn a_path_is_taken_within_its_folder_never_above_the_one_given() {
        let root = crate::testing::scratch("wasi-paths");
        std::fs::create_dir_all(root.at("a")).unwrap();
        let given = Opened::folder(root.clone(), true, pe2_io::system());
        assert_eq!(given.at("a/./b//c").unwrap(), root.at("a/b/c"));
        assert_eq!(given.at("a/b/../c").unwrap(), root.at("a/c"));
        assert_eq!(given.at("").unwrap(), root, "the folder itself");
        assert!(matches!(code(given.at("..")), ErrorCode::NotPermitted), "above what was given");
        assert!(matches!(code(given.at("a/../..")), ErrorCode::NotPermitted));
        assert!(matches!(code(given.at("/etc")), ErrorCode::NotPermitted), "a place of its own");
        // Below what was given, `..` reaches what is above it there - up to what was given.
        let inner =
            given.open_at(super::PathFlags::empty(), "", super::OpenFlags::DIRECTORY, super::DescriptorFlags::READ);
        let inner = inner.unwrap();
        let a = inner.open_at(
            super::PathFlags::SYMLINK_FOLLOW,
            "a",
            super::OpenFlags::DIRECTORY,
            super::DescriptorFlags::READ,
        );
        let a = a.unwrap();
        assert_eq!(a.at("../b").unwrap(), root.at("b"));
        assert!(matches!(code(a.at("../..")), ErrorCode::NotPermitted));
        // Opened to read alone, what is in it is not changed through it.
        assert!(matches!(a.changing().unwrap_err().downcast().unwrap(), ErrorCode::NotPermitted));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_time_goes_to_a_component_and_back_as_it_was() {
        let after = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::new(1_700_000_000, 123);
        let before = std::time::SystemTime::UNIX_EPOCH - std::time::Duration::new(5, 250_000_000);
        for time in [after, before, std::time::SystemTime::UNIX_EPOCH] {
            assert_eq!(system_time(instant(time)).unwrap(), time);
        }
        let told = instant(before);
        assert_eq!((told.seconds, told.nanoseconds), (-6, 750_000_000), "whole seconds before, and what is after them");
    }
}
