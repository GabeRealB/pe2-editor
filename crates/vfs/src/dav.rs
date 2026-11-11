//! The tree served over WebDAV: for where FUSE is not (Windows mounts it natively, macOS's Finder and Linux's davfs2
//! too), each request answered from the tree served at that moment. Read-only, or written as the FUSE mount is: through
//! `Writes`, a file put staged and applied when it is flushed, a folder made, a file or folder removed or moved. A server
//! lasts as long as its `DavServer`; dropping it stops it.

use std::io::SeekFrom;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::SystemTime;

use dav_server::davpath::DavPath;
use dav_server::fs::{
    DavDirEntry, DavFile, DavFileSystem, DavMetaData, FsError, FsFuture, FsResult, FsStream, OpenOptions, ReadDirMeta,
};
use dav_server::{DavHandler, fakels::FakeLs};

use crate::writes::Writes;
use crate::{Node, ROOT, Served, Tree, What};

#[derive(Clone)]
struct Dav {
    served: Served,
    writes: Option<Writes>,
}

/// A path as the tree names it: its steps joined by `/`, the root empty.
fn rel(path: &DavPath) -> FsResult<String> {
    let steps: Option<Vec<&str>> = path.as_rel_ospath().components().map(|c| c.as_os_str().to_str()).collect();
    Ok(steps.ok_or(FsError::NotFound)?.join("/"))
}

fn failed(e: &std::io::Error) -> FsError {
    match e.kind() {
        std::io::ErrorKind::NotFound => FsError::NotFound,
        std::io::ErrorKind::AlreadyExists => FsError::Exists,
        _ => FsError::GeneralFailure,
    }
}

/// Run what may wait on the project's owner off the server's threads.
async fn blocking<T: Send + 'static>(work: impl FnOnce() -> std::io::Result<T> + Send + 'static) -> FsResult<T> {
    match tokio::task::spawn_blocking(work).await {
        Ok(done) => done.map_err(|e| failed(&e)),
        Err(_) => Err(FsError::GeneralFailure),
    }
}

/// A node's inode number by its path from the root.
fn find(tree: &Tree, path: &DavPath) -> FsResult<u64> {
    let mut ino = ROOT;
    for step in path.as_rel_ospath().components() {
        let name = step.as_os_str().to_str().ok_or(FsError::NotFound)?;
        ino = tree.lookup(ino, name).ok_or(FsError::NotFound)?;
    }
    Ok(ino)
}

#[derive(Clone, Debug)]
struct Meta {
    len: u64,
    modified: SystemTime,
    dir: bool,
}

impl Meta {
    fn of(node: &Node) -> Meta {
        Meta { len: node.size, modified: node.modified, dir: matches!(node.what, What::Folder(_)) }
    }

    fn shown(shown: &crate::writes::Shown) -> Meta {
        Meta { len: shown.size, modified: shown.modified, dir: shown.folder }
    }
}

impl DavMetaData for Meta {
    fn len(&self) -> u64 {
        self.len
    }

    fn modified(&self) -> FsResult<SystemTime> {
        Ok(self.modified)
    }

    fn is_dir(&self) -> bool {
        self.dir
    }
}

struct Entry {
    name: String,
    meta: Meta,
}

impl DavDirEntry for Entry {
    fn name(&self) -> Vec<u8> {
        self.name.as_bytes().to_vec()
    }

    fn metadata(&self) -> FsFuture<'_, Box<dyn DavMetaData>> {
        let meta: Box<dyn DavMetaData> = Box::new(self.meta.clone());
        Box::pin(async move { Ok(meta) })
    }
}

/// A file being read: the tree it was opened in, and where it is read from.
#[derive(Debug)]
struct Opened {
    tree: Arc<Tree>,
    ino: u64,
    meta: Meta,
    at: u64,
    /// Open until dropped: what serving waits for before it stops.
    _open: crate::Open,
}

impl DavFile for Opened {
    fn metadata(&mut self) -> FsFuture<'_, Box<dyn DavMetaData>> {
        let meta: Box<dyn DavMetaData> = Box::new(self.meta.clone());
        Box::pin(async move { Ok(meta) })
    }

    fn write_buf(&mut self, _buf: Box<dyn bytes::Buf + Send>) -> FsFuture<'_, ()> {
        Box::pin(async { Err(FsError::Forbidden) })
    }

    fn write_bytes(&mut self, _buf: bytes::Bytes) -> FsFuture<'_, ()> {
        Box::pin(async { Err(FsError::Forbidden) })
    }

    fn read_bytes(&mut self, count: usize) -> FsFuture<'_, bytes::Bytes> {
        let read = self.tree.read(self.ino, self.at, count);
        if let Ok(bytes) = &read {
            self.at += bytes.len() as u64;
        }
        Box::pin(async move { read.map(bytes::Bytes::from).map_err(|_| FsError::GeneralFailure) })
    }

    fn seek(&mut self, pos: SeekFrom) -> FsFuture<'_, u64> {
        let to = match pos {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::Current(d) => self.at.checked_add_signed(d),
            SeekFrom::End(d) => self.meta.len.checked_add_signed(d),
        };
        if let Some(to) = to {
            self.at = to;
        }
        Box::pin(async move { to.ok_or(FsError::GeneralFailure) })
    }

    fn flush(&mut self) -> FsFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

/// A file as written through: read where it is shown, written into its staging file, applied when flushed and when let
/// go.
struct Through {
    writes: Writes,
    path: String,
    handle: Option<u64>,
    at: u64,
    _open: crate::Open,
}

impl std::fmt::Debug for Through {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Through").field("path", &self.path).field("handle", &self.handle).finish_non_exhaustive()
    }
}

impl Drop for Through {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            let writes = self.writes.clone();
            // Applying may wait on the project's owner: not on the server's thread.
            std::thread::spawn(move || writes.close(handle, true));
        }
    }
}

impl DavFile for Through {
    fn metadata(&mut self) -> FsFuture<'_, Box<dyn DavMetaData>> {
        let shown = self.writes.shown(&self.path);
        Box::pin(async move {
            let meta: Box<dyn DavMetaData> = Box::new(Meta::shown(&shown.ok_or(FsError::NotFound)?));
            Ok(meta)
        })
    }

    fn write_buf(&mut self, mut buf: Box<dyn bytes::Buf + Send>) -> FsFuture<'_, ()> {
        let bytes = buf.copy_to_bytes(buf.remaining());
        self.write_bytes(bytes)
    }

    fn write_bytes(&mut self, buf: bytes::Bytes) -> FsFuture<'_, ()> {
        let written = match self.handle {
            Some(handle) => self.writes.write(handle, self.at, &buf).map_err(|e| failed(&e)),
            None => Err(FsError::Forbidden),
        };
        if let Ok(n) = &written {
            self.at += *n as u64;
        }
        Box::pin(async move { written.map(|_| ()) })
    }

    fn read_bytes(&mut self, count: usize) -> FsFuture<'_, bytes::Bytes> {
        let read = self.writes.read(&self.path, self.at, count);
        if let Ok(bytes) = &read {
            self.at += bytes.len() as u64;
        }
        Box::pin(async move { read.map(bytes::Bytes::from).map_err(|e| failed(&e)) })
    }

    fn seek(&mut self, pos: SeekFrom) -> FsFuture<'_, u64> {
        let len = self.writes.shown(&self.path).map_or(0, |s| s.size);
        let to = match pos {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::Current(d) => self.at.checked_add_signed(d),
            SeekFrom::End(d) => len.checked_add_signed(d),
        };
        if let Some(to) = to {
            self.at = to;
        }
        Box::pin(async move { to.ok_or(FsError::GeneralFailure) })
    }

    fn flush(&mut self) -> FsFuture<'_, ()> {
        let (writes, handle) = (self.writes.clone(), self.handle);
        Box::pin(async move {
            match handle {
                Some(handle) => blocking(move || writes.close(handle, false)).await,
                None => Ok(()),
            }
        })
    }
}

impl Dav {
    /// Open a file where writes are taken: read where it is shown, or staged for writing.
    async fn open_through(&self, writes: &Writes, path: &DavPath, options: OpenOptions) -> FsResult<Box<dyn DavFile>> {
        let path = rel(path)?;
        let shown = writes.shown(&path);
        if shown.as_ref().is_some_and(|s| s.folder) {
            return Err(FsError::Forbidden);
        }
        let writing = options.write || options.append || options.truncate || options.create || options.create_new;
        let handle = match (writing, &shown) {
            (false, None) => return Err(FsError::NotFound),
            (false, Some(_)) => None,
            (true, Some(_)) if options.create_new => return Err(FsError::Exists),
            (true, None) if !(options.create || options.create_new) => return Err(FsError::NotFound),
            (true, None) => {
                let (writes, path) = (writes.clone(), path.clone());
                Some(blocking(move || writes.create(&path)).await?)
            }
            (true, Some(_)) => {
                let (writes, path, truncate) = (writes.clone(), path.clone(), options.truncate);
                Some(blocking(move || writes.open(&path, truncate)).await?)
            }
        };
        let at = if options.append { shown.map_or(0, |s| s.size) } else { 0 };
        Ok(Box::new(Through { writes: writes.clone(), path, handle, at, _open: self.served.opening() }))
    }

    /// A change through the writes, where they are taken; forbidden where the tree is served read-only.
    async fn change(&self, work: impl FnOnce(&Writes) -> std::io::Result<()> + Send + 'static) -> FsResult<()> {
        let writes = self.writes.clone().ok_or(FsError::Forbidden)?;
        blocking(move || work(&writes)).await
    }
}

impl DavFileSystem for Dav {
    fn open<'a>(&'a self, path: &'a DavPath, options: OpenOptions) -> FsFuture<'a, Box<dyn DavFile>> {
        Box::pin(async move {
            if let Some(writes) = &self.writes {
                return self.open_through(writes, path, options).await;
            }
            if options.write || options.append || options.truncate || options.create || options.create_new {
                return Err(FsError::Forbidden);
            }
            let tree = self.served.tree();
            let ino = find(&tree, path)?;
            let node = tree.node(ino).ok_or(FsError::NotFound)?;
            if !matches!(node.what, What::File(_)) {
                return Err(FsError::Forbidden);
            }
            let meta = Meta::of(&node);
            let open = self.served.opening();
            let file: Box<dyn DavFile> = Box::new(Opened { tree: tree.clone(), ino, meta, at: 0, _open: open });
            Ok(file)
        })
    }

    fn read_dir<'a>(&'a self, path: &'a DavPath, _meta: ReadDirMeta) -> FsFuture<'a, FsStream<Box<dyn DavDirEntry>>> {
        Box::pin(async move {
            if let Some(writes) = &self.writes {
                let path = rel(path)?;
                if !writes.shown(&path).is_some_and(|s| s.folder) {
                    return Err(FsError::NotFound);
                }
                let join = |name: &str| if path.is_empty() { name.to_string() } else { format!("{path}/{name}") };
                let entries: Vec<FsResult<Box<dyn DavDirEntry>>> = writes
                    .list(&path)
                    .into_keys()
                    .filter_map(|name| {
                        let meta = Meta::shown(&writes.shown(&join(&name))?);
                        let entry: Box<dyn DavDirEntry> = Box::new(Entry { name, meta });
                        Some(Ok(entry))
                    })
                    .collect();
                let stream: FsStream<Box<dyn DavDirEntry>> = Box::pin(futures_util::stream::iter(entries));
                return Ok(stream);
            }
            let tree = self.served.tree();
            find(&tree, path)?;
            let Some(children) = tree.entries(&rel(path)?) else { return Err(FsError::Forbidden) };
            let entries: Vec<FsResult<Box<dyn DavDirEntry>>> = children
                .into_iter()
                .map(|(name, _, node)| {
                    let entry: Box<dyn DavDirEntry> = Box::new(Entry { name, meta: Meta::of(&node) });
                    Ok(entry)
                })
                .collect();
            let stream: FsStream<Box<dyn DavDirEntry>> = Box::pin(futures_util::stream::iter(entries));
            Ok(stream)
        })
    }

    fn metadata<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, Box<dyn DavMetaData>> {
        Box::pin(async move {
            if let Some(writes) = &self.writes {
                let shown = writes.shown(&rel(path)?).ok_or(FsError::NotFound)?;
                let meta: Box<dyn DavMetaData> = Box::new(Meta::shown(&shown));
                return Ok(meta);
            }
            let tree = self.served.tree();
            let node = tree.node(find(&tree, path)?).ok_or(FsError::NotFound)?;
            let meta: Box<dyn DavMetaData> = Box::new(Meta::of(&node));
            Ok(meta)
        })
    }

    fn create_dir<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, ()> {
        Box::pin(async move {
            let path = rel(path)?;
            self.change(move |writes| writes.make_folder(&path)).await
        })
    }

    fn remove_dir<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, ()> {
        Box::pin(async move {
            let path = rel(path)?;
            self.change(move |writes| writes.remove(&path)).await
        })
    }

    fn remove_file<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, ()> {
        Box::pin(async move {
            let path = rel(path)?;
            self.change(move |writes| writes.remove(&path)).await
        })
    }

    fn rename<'a>(&'a self, from: &'a DavPath, to: &'a DavPath) -> FsFuture<'a, ()> {
        Box::pin(async move {
            let (from, to) = (rel(from)?, rel(to)?);
            self.change(move |writes| writes.rename(&from, &to)).await
        })
    }

    fn copy<'a>(&'a self, from: &'a DavPath, to: &'a DavPath) -> FsFuture<'a, ()> {
        Box::pin(async move {
            let (from, to) = (rel(from)?, rel(to)?);
            self.change(move |writes| {
                let len = writes.shown(&from).filter(|s| !s.folder).ok_or(std::io::ErrorKind::NotFound)?.size;
                let bytes = writes.read(&from, 0, usize::try_from(len).unwrap_or(usize::MAX))?;
                let handle = if writes.shown(&to).is_some() { writes.open(&to, true)? } else { writes.create(&to)? };
                writes.write(handle, 0, &bytes)?;
                writes.close(handle, true)
            })
            .await
        })
    }
}

/// A WebDAV server: stopped when dropped.
pub struct DavServer {
    pub addr: SocketAddr,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for DavServer {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Serve the tree over WebDAV at an address (port 0: one the system picks), on a thread of its own: read-only, or
/// written through `writes`.
///
/// # Errors
/// The address cannot be listened on, or the server's thread cannot start.
pub fn serve(served: &Served, writes: Option<Writes>, addr: SocketAddr) -> std::io::Result<DavServer> {
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?;
    let listener = runtime.block_on(tokio::net::TcpListener::bind(addr))?;
    let addr = listener.local_addr()?;
    let handler = DavHandler::builder()
        .filesystem(Box::new(Dav { served: served.clone(), writes }))
        .locksystem(FakeLs::new())
        .build_handler();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let accept = async move {
        let accepting = async {
            loop {
                let Ok((stream, _)) = listener.accept().await else { continue };
                let handler = handler.clone();
                tokio::spawn(async move {
                    let service = hyper::service::service_fn(move |request| {
                        let handler = handler.clone();
                        async move { Ok::<_, std::convert::Infallible>(handler.handle(request).await) }
                    });
                    let io = hyper_util::rt::TokioIo::new(stream);
                    let _ = hyper::server::conn::http1::Builder::new().serve_connection(io, service).await;
                });
            }
        };
        tokio::select! {
            () = accepting => {}
            _ = stopped => {}
        }
    };
    let thread = std::thread::Builder::new().name("webdav".into()).spawn(move || runtime.block_on(accept))?;
    Ok(DavServer { addr, stop: Some(stop), thread: Some(thread) })
}

/// WebDAV as a backend: the tree served at an address, which the system's own client mounts.
struct AsBackend;

/// WebDAV, among every system's backends.
pub const BACKEND: &dyn crate::backend::Backend = &AsBackend;

/// The address WebDAV serves at where nothing names one.
pub const ADDRESS: SocketAddr = SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 4918);

impl crate::backend::Backend for AsBackend {
    fn name(&self) -> &'static str {
        "webdav"
    }

    fn label(&self) -> &'static str {
        "WebDAV"
    }

    fn can(&self) -> crate::backend::Can {
        crate::backend::Can { write: true, folder: false }
    }

    fn available(&self, _at: &std::path::Path) -> Result<(), String> {
        Ok(())
    }

    fn mount(&self, job: &crate::backend::Job) -> std::io::Result<Box<dyn crate::backend::Mount>> {
        let writes = match &job.owner {
            Some(owner) => Some(Writes::new(job.served, owner.clone(), job.staging)?),
            None => None,
        };
        let server = serve(job.served, writes.clone(), job.address.unwrap_or(ADDRESS))?;
        Ok(crate::backend::Finishing::of(server, writes))
    }
}

impl crate::backend::Mount for DavServer {
    fn shown(&self) -> String {
        format!("http://{}/", self.addr)
    }
}
