//! A layer file's content, read and written in one place. A records layer holds a file's content in its record - text
//! inline, or kept by hash in a store (`chunks.rs`); a held extension's in its tree; a mounted tree's as the file
//! itself. These functions read each as the file it is, and write a mounted tree's file whole. Code reading or writing
//! what a layer holds - an asset's files, a generated layer's, a plain file of the project - goes through here rather
//! than to the path.
//!
//! What is not a layer's content keeps its own access: the local folder's stores (the graph's blobs, the history's), the
//! project's own files (`project.toml`, changesets), and temporary files.

use pe2_path::Name;
use sfs::HostPath;
use std::collections::BTreeMap;
use std::io;
use std::sync::{Arc, RwLock};

use pe2_io::Open;

/// The project's I/O, as what reads and writes its content is given it.
type Io = Arc<dyn pe2_io::Io>;

use crate::chunks::{Manifest, Store};

/// How a layer holds a file's content.
enum Form {
    Plain(HostPath),
    /// Text a record holds inline (`records`).
    Inline {
        bytes: Vec<u8>,
        modified: std::time::SystemTime,
    },
    /// Kept in a store, by hash, for a record.
    Stored {
        manifest: Arc<Manifest>,
        store: Store,
        modified: std::time::SystemTime,
    },
    /// A file of a sealed node, a whole object of its tree (`records::Record::tree`): read where it is, never written.
    Object {
        path: HostPath,
        sha1: pe2_ext::Sha1,
        size: u64,
        modified: std::time::SystemTime,
    },
}

/// Manifests read, by what tells their versions apart: a store's file's by its SHA-1 (it never changes)
/// file's by its manifest's path, time and length. Read once each, not at each read of a range.
static MANIFESTS: RwLock<Option<std::collections::HashMap<String, Arc<Manifest>>>> = RwLock::new(None);

/// How many manifests are remembered at most: past it, they are read again.
const MANIFESTS_HELD: usize = 4096;

fn manifest(key: String, read: impl FnOnce() -> io::Result<Manifest>) -> io::Result<Arc<Manifest>> {
    if let Some(found) =
        MANIFESTS.read().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref().and_then(|m| m.get(&key))
    {
        return Ok(found.clone());
    }
    let manifest = Arc::new(read()?);
    let mut held = MANIFESTS.write().unwrap_or_else(std::sync::PoisonError::into_inner);
    let held = held.get_or_insert_default();
    if held.len() >= MANIFESTS_HELD {
        held.clear();
    }
    held.insert(key, manifest.clone());
    Ok(manifest)
}

/// A file kept in the store, by its SHA-1: its manifest, with the store its chunks are read from - this one, or, where
/// only the project's other store keeps it, that one.
fn stored_manifest(store: &Store, sha1: &pe2_ext::Sha1) -> io::Result<(Arc<Manifest>, Store)> {
    let held = |store: &Store| manifest(format!("{}:{sha1}", store.dir.display()), || store.file_manifest(sha1));
    held(store).map(|manifest| (manifest, store.clone())).or_else(|e| {
        match store.other().filter(|o| o.has_file(sha1)) {
            Some(other) => held(&other).map(|manifest| (manifest, other)),
            None => Err(e),
        }
    })
}

/// Where the files this process serves live, by where a layer holds each - those that are not files there, but a
/// record's text or kept in a store (`serve_whole`, `serve_changed`): none where this process serves nothing.
static SERVED: RwLock<Option<std::collections::HashMap<HostPath, Lives>>> = RwLock::new(None);

/// Where a file's content lives, and when it changed.
type Lives = (crate::server::Source, (u64, u32));

/// Read the files a project's records hold as this listing says from now on, in this process: what serves a
/// project's folder knows where each file's content lives from what whoever holds the project says is served, and
/// reads no records of its own. In place of what it read by before.
pub fn serve_whole(listing: &crate::server::Listing) {
    let mut lives = std::collections::HashMap::new();
    for shown in listing.values() {
        if let crate::server::Shown::File { disk, modified, source, .. } = shown
            && *source != crate::server::Source::Disk
        {
            lives.insert(disk.clone(), (source.clone(), *modified));
        }
    }
    *SERVED.write().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(lives);
}

/// Read these files as listed now, where they changed since what this process reads by was said.
pub fn serve_changed(shown: &[(Arc<crate::TreePath>, crate::server::Shown)]) {
    let mut lives = SERVED.write().unwrap_or_else(std::sync::PoisonError::into_inner);
    let lives = lives.get_or_insert_default();
    for (_, shown) in shown {
        if let crate::server::Shown::File { disk, modified, source, .. } = shown {
            if *source == crate::server::Source::Disk {
                lives.remove(disk);
            } else {
                lives.insert(disk.clone(), (source.clone(), *modified));
            }
        }
    }
}

/// Records read for the text they hold, by their files as they were then: a few, read again where they changed.
static RECORDS: std::sync::Mutex<Vec<(HostPath, std::time::SystemTime, Arc<crate::records::Record>)>> =
    std::sync::Mutex::new(Vec::new());

/// How many records are remembered for their text.
const RECORDS_HELD: usize = 256;

/// The text a record holds for one of its files, read from the record's own file.
/// A file of a record, by the name what serves it was told: a path within the record's entry.
fn record_file(record: &HostPath, name: &str) -> io::Result<Arc<pe2_path::RelPath>> {
    (pe2_path::RelPath::new(name)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, format!("{}: {e}", record.display()))))
    .map(ToOwned::to_owned)
}

fn record_text(io: &Io, record: &HostPath, name: &str) -> io::Result<Vec<u8>> {
    let fs = io.filesystem();
    // A record that waits to be written is read as that leaves it: whoever holds the project said its files
    // served from it already.
    match crate::storage::waiting(record) {
        Some(Some(bytes)) => {
            let text = String::from_utf8(bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            let parsed = crate::records::Record::parse(&text)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {e:#}", record.display())))?;
            return match parsed.files.get(&record_file(record, name)?).and_then(|file| file.content.as_ref()) {
                Some(crate::records::Content::Text(text)) => Ok(text.clone().into_bytes()),
                _ => Err(io::Error::new(io::ErrorKind::NotFound, format!("{}: no text of `{name}`", record.display()))),
            };
        }
        Some(None) => return Err(io::Error::from(io::ErrorKind::NotFound)),
        None => {}
    }
    let written = fs.stat(record)?.modified.ok_or_else(|| io::Error::other("no time of the record"))?;
    let mut held = RECORDS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let known = held.iter().position(|(path, at, _)| path == record && *at == written);
    let parsed = if let Some(at) = known {
        held[at].2.clone()
    } else {
        let parsed = crate::records::Record::parse(&fs.read_to_string(record)?)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {e:#}", record.display())))?;
        let parsed = Arc::new(parsed);
        held.retain(|(path, _, _)| path != record);
        if held.len() >= RECORDS_HELD {
            held.remove(0);
        }
        held.push((record.clone(), written, parsed.clone()));
        parsed
    };
    match parsed.files.get(&record_file(record, name)?).and_then(|file| file.content.as_ref()) {
        Some(crate::records::Content::Text(text)) => Ok(text.clone().into_bytes()),
        _ => Err(io::Error::new(io::ErrorKind::NotFound, format!("{}: no text of `{name}`", record.display()))),
    }
}

/// A file's content as what this process serves by says where it lives; none where it serves by nothing, or the
/// file is where a layer holds it, as it is.
fn listed(io: &Io, path: &HostPath) -> Option<io::Result<Form>> {
    use crate::server::Source;
    let lives = SERVED.read().unwrap_or_else(std::sync::PoisonError::into_inner);
    let (source, modified) = lives.as_ref()?.get(path).cloned()?;
    drop(lives);
    let modified = std::time::UNIX_EPOCH + std::time::Duration::new(modified.0, modified.1);
    match source {
        Source::Disk => None,
        Source::RecordText { record, name } => {
            Some(record_text(io, &record, &name).map(|bytes| Form::Inline { bytes, modified }))
        }
        Source::Stored { sha1, store } => {
            let store = Store { dir: store, io: io.clone() };
            Some(stored_manifest(&store, &sha1).map(|(manifest, store)| Form::Stored { manifest, store, modified }))
        }
        Source::Object { sha1, size, store } => {
            let store = Store { dir: store, io: io.clone() };
            Some(store.held_object(&sha1).map(|path| Form::Object { path, sha1, size, modified }))
        }
    }
}

/// Where a path's content is: what is known without the disk first - what is served, a records layer's records, by the
/// path's prefix - then the file.
fn form(io: &Io, path: &HostPath) -> io::Result<Form> {
    let fs = io.filesystem();
    if let Some(form) = listed(io, path) {
        return form;
    }
    if let Some((content, modified, store)) = crate::records::Set::file(path) {
        return Ok(match content {
            crate::records::Content::Text(text) => Form::Inline { bytes: text.into_bytes(), modified },
            crate::records::Content::Stored { sha1, .. } => {
                let (manifest, store) = stored_manifest(&store, &sha1)?;
                Form::Stored { manifest, store, modified }
            }
            crate::records::Content::Object { sha1, size } => {
                Form::Object { path: store.held_object(&sha1)?, sha1, size, modified }
            }
        });
    }
    if fs.is_file(path) {
        return Ok(Form::Plain(path.clone()));
    }
    Err(io::Error::new(io::ErrorKind::NotFound, path.display().to_string()))
}

/// A file's content, whole.
///
/// # Errors
/// It cannot be read.
pub fn read(io: &Io, path: &HostPath) -> io::Result<Vec<u8>> {
    match form(io, path)? {
        Form::Plain(path) | Form::Object { path, .. } => io.filesystem().read(&path),
        Form::Inline { bytes, .. } => Ok(bytes),
        Form::Stored { manifest, store, .. } => {
            manifest.read_range(&store, 0, usize::try_from(manifest.size).unwrap_or(usize::MAX))
        }
    }
}

/// A file's content as text.
///
/// # Errors
/// It cannot be read, or is not UTF-8.
pub fn read_to_string(io: &Io, path: &HostPath) -> io::Result<String> {
    String::from_utf8(read(io, path)?).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Up to `len` bytes of a file's content from `offset`: fewer past its end.
///
/// # Errors
/// It cannot be opened or read.
pub fn read_range(io: &Io, path: &HostPath, offset: u64, len: usize) -> io::Result<Vec<u8>> {
    match form(io, path)? {
        Form::Plain(path) | Form::Object { path, .. } => {
            let file = io.filesystem().open(&path, Open::READ)?;
            let len = usize::try_from(file.stat()?.size.saturating_sub(offset)).unwrap_or(usize::MAX).min(len);
            let mut out = vec![0; len];
            let read = read_all_at(&*file, offset, &mut out)?;
            out.truncate(read);
            Ok(out)
        }
        Form::Inline { bytes, .. } => {
            let start = usize::try_from(offset).unwrap_or(usize::MAX).min(bytes.len());
            Ok(bytes[start..start.saturating_add(len).min(bytes.len())].to_vec())
        }
        Form::Stored { manifest, store, .. } => manifest.read_range(&store, offset, len),
    }
}

/// Bytes of a file's content from `offset` read into a buffer, as many as it takes or the content has: how many. What
/// serves content to programs reads it so, into where it is asked for, with nothing made on the way.
///
/// # Errors
/// It cannot be opened or read.
pub fn read_range_into(io: &Io, path: &HostPath, offset: u64, into: &mut [u8]) -> io::Result<usize> {
    match form(io, path)? {
        Form::Plain(path) | Form::Object { path, .. } => {
            read_all_at(&*io.filesystem().open(&path, Open::READ)?, offset, into)
        }
        Form::Inline { bytes, .. } => {
            let start = usize::try_from(offset).unwrap_or(usize::MAX).min(bytes.len());
            let read = (bytes.len() - start).min(into.len());
            into[..read].copy_from_slice(&bytes[start..start + read]);
            Ok(read)
        }
        Form::Stored { manifest, store, .. } => manifest.read_range_into(&store, offset, into),
    }
}

/// Up to `len` bytes from a file's start; nothing where it cannot be read.
#[must_use]
pub fn head(io: &Io, path: &HostPath, len: usize) -> Vec<u8> {
    read_range(io, path, 0, len).unwrap_or_default()
}

/// Read into a buffer from `offset` on, by position: as many bytes as it takes, or the file has.
fn read_all_at(file: &dyn pe2_io::Descriptor, offset: u64, into: &mut [u8]) -> io::Result<usize> {
    let mut read = 0;
    while read < into.len() {
        match file.read_at(&mut into[read..], offset + read as u64)? {
            0 => break,
            count => read += count,
        }
    }
    Ok(read)
}

/// A file's content's length.
///
/// # Errors
/// It cannot be found.
pub fn len(io: &Io, path: &HostPath) -> io::Result<u64> {
    stat(io, path).map(|(len, _)| len)
}

/// Whether a layer holds a content at a path.
#[must_use]
pub fn exists(io: &Io, path: &HostPath) -> bool {
    is_file(io, path)
}

/// Refuse to write a records layer's file as a file: its records are written (`records`).
fn not_a_record(path: &HostPath) -> io::Result<()> {
    if crate::records::Set::is_folder(path).is_some() {
        return Err(io::Error::other(format!("{}: a records layer is written by its records", path.display())));
    }
    Ok(())
}

/// A file's content's length and when it was last changed (its record's time, for a records layer's).
///
/// # Errors
/// It cannot be found.
pub fn stat(io: &Io, path: &HostPath) -> io::Result<(u64, std::time::SystemTime)> {
    let fs = io.filesystem();
    let no_time = || io::Error::other(format!("{}: the system gives no time of it", path.display()));
    // What is known without the disk, and a file's own metadata, without reading any content.
    if let Some(found) = crate::records::Set::stat(path) {
        return Ok(found);
    }
    if let Some(stat) = fs.stat(path).ok().filter(|stat| stat.kind == pe2_io::Kind::File) {
        return Ok((stat.size, stat.modified.ok_or_else(no_time)?));
    }
    match form(io, path)? {
        Form::Plain(path) => {
            let stat = fs.stat(&path)?;
            Ok((stat.size, stat.modified.ok_or_else(no_time)?))
        }
        Form::Stored { manifest, modified, .. } => Ok((manifest.size, modified)),
        Form::Inline { bytes, modified } => Ok((bytes.len() as u64, modified)),
        Form::Object { size, modified, .. } => Ok((size, modified)),
    }
}

/// A file's content's hash: a stored one's from its manifest, read from nothing else.
///
/// # Errors
/// It cannot be read.
pub fn sha1(io: &Io, path: &HostPath) -> io::Result<pe2_ext::Sha1> {
    match form(io, path)? {
        Form::Plain(path) => crate::extract::sha1_file(io.filesystem(), &path).map_err(io::Error::other),
        Form::Inline { bytes, .. } => Ok(crate::extract::sha1_of(&bytes)),
        Form::Stored { manifest, .. } => Ok(manifest.sha1),
        Form::Object { sha1, .. } => Ok(sha1),
    }
}

/// A file's content's hash as `sha1` gives it, remembered by what tells one version of the file from another -
/// when it changed and how long it is - so that asking again of a file that did not change reads nothing of it:
/// what holding a kept output to the files it read costs, at every change of any file. A file that had changed
/// less than a second before it was read is read again the next time: changed once more in the same moment, to
/// the same length, it would bear the same time.
///
/// # Errors
/// It cannot be read.
pub fn sha1_known(io: &Io, path: &HostPath) -> io::Result<pe2_ext::Sha1> {
    use std::time::{Duration, SystemTime};
    /// A file's time and length, when it was read, and what it hashed to.
    type Known = std::collections::HashMap<HostPath, ((SystemTime, u64), SystemTime, pe2_ext::Sha1)>;
    static KNOWN: std::sync::LazyLock<std::sync::Mutex<Known>> = std::sync::LazyLock::new(Default::default);
    // No file of that very path - a stored one, an object: its hash is read from what says it.
    let Some(stamp) = io.filesystem().stat(path).ok().and_then(|stat| Some((stat.modified?, stat.size))) else {
        return sha1(io, path);
    };
    let known = KNOWN.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(path).copied();
    let settled = |read: SystemTime| read.duration_since(stamp.0).is_ok_and(|since| since > Duration::from_secs(1));
    if let Some((_, _, hash)) = known.filter(|(at, read, _)| *at == stamp && settled(*read)) {
        return Ok(hash);
    }
    #[allow(
        clippy::disallowed_methods,
        reason = "compared with the times the system stamps files with: the system's own clock"
    )]
    let read = SystemTime::now();
    let hash = sha1(io, path)?;
    let mut known = KNOWN.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    known.insert(path.clone(), (stamp, read, hash));
    Ok(hash)
}

/// Write a file's content whole, atomically: what reads it sees the old content or the new, never part of either. A
/// records layer's file is no file: its records are written (`records`).
///
/// # Errors
/// Its folder cannot be made, or it cannot be written.
pub fn write(io: &Io, path: &HostPath, bytes: &[u8]) -> io::Result<()> {
    let fs = io.filesystem();
    not_a_record(path)?;
    if let Some(parent) = path.parent() {
        fs.create_folder_all(&parent)?;
    }
    crate::local::write_atomic(fs, path, bytes).map_err(io::Error::other)
}

/// Remove a file's content, where it is.
///
/// # Errors
/// It cannot be removed.
pub fn remove(io: &Io, path: &HostPath) -> io::Result<()> {
    not_a_record(path)?;
    match io.filesystem().remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// What serves a stored content to programs as a file (a scoped filesystem, `vfs::scoped`): given the layer's path,
/// where a program reads it.
type Exposer = Arc<dyn Fn(&Io, &HostPath) -> io::Result<HostPath> + Send + Sync>;

/// The exposers registered, by the store whose contents each serves (any, for none), and the number its registration
/// was given.
static EXPOSERS: RwLock<Vec<(u64, Option<HostPath>, Exposer)>> = RwLock::new(Vec::new());

/// An exposer's registration: it serves until this is dropped.
pub struct Exposing(u64);

impl Drop for Exposing {
    fn drop(&mut self) {
        EXPOSERS.write().unwrap_or_else(std::sync::PoisonError::into_inner).retain(|(id, ..)| *id != self.0);
    }
}

/// Have `exposer` serve the stored contents of a store (`store/`), or of every store for none, to programs, for
/// as long as what this returns is held; the latest registered for a store serves it.
pub fn expose_with(
    store: Option<&HostPath>,
    exposer: impl Fn(&Io, &HostPath) -> io::Result<HostPath> + Send + Sync + 'static,
) -> Exposing {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut exposers = EXPOSERS.write().unwrap_or_else(std::sync::PoisonError::into_inner);
    exposers.push((id, store.cloned(), Arc::new(exposer)));
    Exposing(id)
}

/// A file of a folder served to a program (`serve_folder`): a content where it is - a store's blob, a layer's file,
/// read through `read_range` - or bytes held in memory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Given {
    Disk(HostPath),
    Bytes(Arc<[u8]>),
}

/// A folder served to a program, until this is dropped: where it is.
pub struct Folder {
    pub at: HostPath,
    _held: Box<dyn Send + Sync>,
}

impl Folder {
    /// A folder at a place, served until `held` is dropped.
    #[must_use]
    pub fn new(at: HostPath, held: Box<dyn Send + Sync>) -> Folder {
        Folder { at, _held: held }
    }
}

/// How what is served to a program will be used (`pe2_ext::Use`): what it is served by is chosen by it.
pub use pe2_ext::Use;

/// A path in a folder of files given to a program (`serve_folder`).
pub type GivenPath = pe2_path::VfsPath<pe2_path::Given>;

/// What serves folders to programs (a scoped filesystem, `vfs::scoped`): given the project they are of, where there is
/// one, the files by their paths in the folder, and how they will be used. One that finds laying them out the better
/// way for that use serves none, and they are laid out.
type FolderServer =
    Arc<dyn Fn(&Io, Option<&HostPath>, &BTreeMap<Arc<GivenPath>, Given>, &Use) -> io::Result<Folder> + Send + Sync>;

static FOLDER_SERVERS: RwLock<Vec<(u64, FolderServer)>> = RwLock::new(Vec::new());

/// A folder server's registration: it serves until this is dropped.
pub struct ServingFolders(u64);

impl Drop for ServingFolders {
    fn drop(&mut self) {
        FOLDER_SERVERS.write().unwrap_or_else(std::sync::PoisonError::into_inner).retain(|(id, _)| *id != self.0);
    }
}

/// Have `server` serve folders to programs (`serve_folder`) for as long as what this returns is held; the latest
/// registered serves.
pub fn serve_folders_with(
    server: impl Fn(&Io, Option<&HostPath>, &BTreeMap<Arc<GivenPath>, Given>, &Use) -> io::Result<Folder>
    + Send
    + Sync
    + 'static,
) -> ServingFolders {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    FOLDER_SERVERS.write().unwrap_or_else(std::sync::PoisonError::into_inner).push((id, Arc::new(server)));
    ServingFolders(id)
}

/// A read-only folder holding these files by their paths in it (folders made of their parents), for a program to read,
/// until what this returns is dropped: served where they are by a folder server (`serve_folders_with`), else laid out
/// in `fallback` - each whole file on disk linked, the rest written - and removed whole once dropped.
///
/// # Errors
/// Nothing serves it, and it cannot be laid out.
pub fn serve_folder(
    io: &Io,
    project: Option<&HostPath>,
    files: &BTreeMap<Arc<GivenPath>, Given>,
    how: &Use,
    fallback: &HostPath,
) -> io::Result<Folder> {
    let server =
        FOLDER_SERVERS.read().unwrap_or_else(std::sync::PoisonError::into_inner).last().map(|(_, s)| Arc::clone(s));
    if let Some(served) = server.and_then(|serve| serve(io, project, files, how).ok()) {
        return Ok(served);
    }
    laid_out(io, files, fallback)
}

/// What laying these files out on disk would write, in bytes: what is held in memory, and what no layer holds as one
/// file to link. What is linked costs nothing, so a tree of whole files is laid out for next to nothing whatever its
/// size - which is what decides between laying files out and serving them where they are (`vfs::backend::choose`).
#[must_use]
pub fn written_to_lay_out(io: &Io, files: &BTreeMap<Arc<GivenPath>, Given>) -> u64 {
    files
        .values()
        .map(|given| match given {
            Given::Bytes(bytes) => bytes.len() as u64,
            Given::Disk(disk) => match form(io, disk) {
                Ok(Form::Plain(_) | Form::Object { .. }) => 0,
                _ => len(io, disk).unwrap_or(0),
            },
        })
        .sum()
}

/// A folder of these files laid out on disk, removed whole once what this returns is dropped.
fn laid_out(io: &Io, files: &BTreeMap<Arc<GivenPath>, Given>, under: &HostPath) -> io::Result<Folder> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let fs = io.filesystem();
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let at = under.join(&Name::new(&format!("served-{}-{n}", std::process::id())).map_err(io::Error::other)?);
    let removed = Removed(at.clone(), io.clone());
    for (name, given) in files {
        let to = at.join(name.rel());
        if let Some(parent) = to.parent() {
            fs.create_folder_all(&parent)?;
        }
        match given {
            Given::Bytes(bytes) => fs.write(&to, bytes)?,
            // A whole file linked where it is, else its content written.
            Given::Disk(disk) => {
                let whole = matches!(form(io, disk)?, Form::Plain(_) | Form::Object { .. });
                if !(whole && fs.hard_link(disk, &to).is_ok()) {
                    fs.write(&to, &read(io, disk)?)?;
                }
            }
        }
    }
    fs.create_folder_all(&at)?;
    Ok(Folder::new(at, Box::new(removed)))
}

/// A folder removed whole once this is dropped.
struct Removed(HostPath, Io);

impl Drop for Removed {
    fn drop(&mut self) {
        let _ = self.1.filesystem().remove_all(&self.0);
    }
}

/// A path a program can open for a content, as a file: the file itself where a layer holds it whole; for a stored one,
/// where an exposer serves it (`expose_with`), else a copy assembled in the outputs (`materialize`).
///
/// # Errors
/// It cannot be found, or neither served nor assembled.
pub fn for_program(io: &Io, path: &HostPath) -> io::Result<HostPath> {
    let exposed = |store: &HostPath| {
        let exposer = {
            let exposers = EXPOSERS.read().unwrap_or_else(std::sync::PoisonError::into_inner);
            let serves = |at: &Option<HostPath>| at.as_ref().is_none_or(|at| at == store);
            exposers.iter().rev().find(|(_, at, _)| serves(at)).map(|(.., e)| Arc::clone(e))
        };
        exposer.and_then(|serve| serve(io, path).ok())
    };
    match form(io, path)? {
        Form::Plain(path) => Ok(path),
        Form::Inline { .. } => materialize(io, path),
        Form::Stored { store, .. } => match exposed(&store.dir) {
            Some(served) => Ok(served),
            None => materialize(io, path),
        },
        // A held extension's file: served by its name where a scope is, else the object itself, read-only, in place.
        Form::Object { path: object, .. } => {
            let served = object.ancestors().nth(3).and_then(|store| exposed(&store));
            Ok(served.unwrap_or(object))
        }
    }
}

/// Where a project's cache keeps a content assembled for programs: by its hash, under the name of the file it is.
fn materialized(project: &HostPath, sha1: &pe2_ext::Sha1, path: &HostPath) -> io::Result<HostPath> {
    const MATERIALIZED: &Name = Name::constant("materialized");
    let hash = sha1.to_string();
    let hash = Name::new(&hash).map_err(io::Error::other)?;
    let folder = crate::local::part(project, crate::local::OUTPUTS).join(&MATERIALIZED).join(&hash);
    Ok(folder.entry(path.file_name().ok_or_else(|| not_in_a_project(path))?))
}

fn not_in_a_project(path: &HostPath) -> io::Error {
    io::Error::other(format!("{path}: in no project's layers"))
}

/// A path a program can open for a content, as a file: the file itself where it is one; a stored one assembled once,
/// by its hash, in the project's outputs (`.local/outputs/materialized/<sha1>/<name>`), its name kept for what reads names.
///
/// # Errors
/// It cannot be read, or the outputs cannot be written.
pub fn materialize(io: &Io, path: &HostPath) -> io::Result<HostPath> {
    let fs = io.filesystem();
    match form(io, path)? {
        // A whole object is a file already: never copied.
        Form::Plain(path) | Form::Object { path, .. } => Ok(path),
        Form::Inline { bytes, .. } => {
            let project = crate::holding(path).ok_or_else(|| not_in_a_project(path))?;
            let out = materialized(&project, &crate::extract::sha1_of(&bytes), path)?;
            if !fs.is_file(&out) {
                if let Some(parent) = out.parent() {
                    fs.create_folder_all(&parent)?;
                }
                crate::local::write_atomic(fs, &out, &bytes).map_err(io::Error::other)?;
            }
            Ok(out)
        }
        Form::Stored { manifest, store, .. } => {
            // The store is at the project's root.
            let project = store.dir.parent().ok_or_else(|| not_in_a_project(path))?;
            let out = materialized(&project, &manifest.sha1, path)?;
            if !fs.is_file(&out) {
                let bytes = manifest.read_range(&store, 0, usize::try_from(manifest.size).unwrap_or(usize::MAX))?;
                if let Some(parent) = out.parent() {
                    fs.create_folder_all(&parent)?;
                }
                crate::local::write_atomic(fs, &out, &bytes).map_err(io::Error::other)?;
            }
            Ok(out)
        }
    }
}

/// An entry's name as a folder lists it: a name, or what it was refused as (`PathError::text` shows it) and why.
pub type Listed = Result<Arc<Name>, pe2_path::PathError>;

/// The text an entry is listed by: its name, or how the one refused shows.
#[must_use]
pub fn shown(listed: &Listed) -> &str {
    match listed {
        Ok(name) => name.as_str(),
        Err(refused) => &refused.text,
    }
}

/// What is in a folder - on disk, in a held extension's tree, or in a records layer's tree: each entry's name - checked,
/// as every name the system gives is: one no path takes is listed as refused, for whoever reads the folder to say so,
/// never passed over or read otherwise than it is - its path and whether it is a folder; a file really there before
/// one the records hold alike.
#[must_use]
pub fn read_dir(io: &Arc<dyn pe2_io::Io>, dir: &HostPath) -> Vec<(Listed, HostPath, bool)> {
    let fs = io.filesystem();
    let mut out: Vec<(Listed, HostPath, bool)> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for entry in fs.read_directory(dir).unwrap_or_default() {
        let folder = entry.kind == pe2_io::Kind::Folder;
        if folder || entry.kind == pe2_io::Kind::File {
            let raw = entry.name;
            let name = match raw.to_str() {
                Some(text) => Name::new(text).map(ToOwned::to_owned),
                None => Err(pe2_path::PathError { text: raw.to_string_lossy().into(), why: pe2_path::Why::NotText }),
            };
            seen.insert(shown(&name).to_owned());
            out.push((name, dir.entry(&raw), folder));
        }
    }
    // A records layer's, as its records make it.
    let recorded = crate::records::Set::list(dir).into_iter().flatten().map(|(name, folder)| (Ok(name), folder));
    for (name, folder) in recorded {
        let text = shown(&name).to_owned();
        if seen.insert(text.clone()) {
            out.push((name, dir.entry(std::ffi::OsStr::new(&text)), folder));
        }
    }
    out
}

/// Whether a path is a folder: on disk, or in a records layer's.
#[must_use]
pub fn is_dir(io: &Arc<dyn pe2_io::Io>, path: &HostPath) -> bool {
    crate::records::Set::is_folder(path) == Some(true) || io.filesystem().is_folder(path)
}

/// Whether a path is a file: on disk, or in a records layer's.
#[must_use]
pub fn is_file(io: &Arc<dyn pe2_io::Io>, path: &HostPath) -> bool {
    crate::records::Set::is_file(path) == Some(true) || io.filesystem().is_file(path)
}

#[cfg(test)]
#[allow(
    clippy::disallowed_methods,
    reason = "a test's fixtures: the machine's time, places and environment, which nothing tested reads"
)]
mod tests {
    use super::{Given, head, len, read, read_range, write};
    use crate::testing::At as _;

    #[test]
    fn a_files_hash_is_remembered_and_read_again_where_the_file_changed() {
        let dir = crate::testing::scratch("content-known");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.at("a.c");
        let hash = |text: &str| crate::extract::sha1_of(text.as_bytes());
        std::fs::write(&file, "one").unwrap();
        assert_eq!(super::sha1_known(&crate::testing::io(), &file).unwrap(), hash("one"));
        assert_eq!(super::sha1_known(&crate::testing::io(), &file).unwrap(), hash("one"));
        // Changed in the moment it was read in, to the same length - its time may be the same: read again.
        std::fs::write(&file, "two").unwrap();
        assert_eq!(super::sha1_known(&crate::testing::io(), &file).unwrap(), hash("two"));
        // Changed long after it was written and read: told by its time.
        let long_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
        std::fs::File::options().write(true).open(&file).unwrap().set_modified(long_ago).unwrap();
        assert_eq!(super::sha1_known(&crate::testing::io(), &file).unwrap(), hash("two"));
        assert_eq!(
            super::sha1_known(&crate::testing::io(), &file).unwrap(),
            hash("two"),
            "remembered, as it was read long after it changed"
        );
        std::fs::write(&file, "six").unwrap();
        assert_eq!(super::sha1_known(&crate::testing::io(), &file).unwrap(), hash("six"));
        // And where no file of that path is, what `sha1` says.
        assert!(super::sha1_known(&crate::testing::io(), &dir.at("none")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_content_reads_whole_and_by_range() {
        let dir = crate::testing::scratch("content");
        let path = dir.at("a/b/c.bin");
        write(&crate::testing::io(), &path, b"0123456789").expect("written");
        assert_eq!(read(&crate::testing::io(), &path).expect("read"), b"0123456789");
        assert_eq!(read_range(&crate::testing::io(), &path, 3, 4).expect("range"), b"3456");
        assert_eq!(read_range(&crate::testing::io(), &path, 8, 10).expect("past the end"), b"89");
        assert_eq!(head(&crate::testing::io(), &path, 2), b"01");
        assert_eq!(len(&crate::testing::io(), &path).expect("len"), 10);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_folder_nothing_serves_is_laid_out_and_removed_once_let_go_of() {
        let dir = crate::testing::scratch("laid-out");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let whole = dir.at("whole.o");
        std::fs::write(&whole, b"object").unwrap();
        let files = std::collections::BTreeMap::from([
            (super::GivenPath::new("objects/a.o").unwrap().to_owned(), Given::Disk(whole.clone())),
            (super::GivenPath::new("units").unwrap().to_owned(), Given::Bytes(std::sync::Arc::from(&b"a.c"[..]))),
        ]);
        let folder = super::laid_out(&crate::testing::io(), &files, &dir.at("tmp")).unwrap();
        assert_eq!(std::fs::read(folder.at.at("objects/a.o")).unwrap(), b"object");
        assert_eq!(std::fs::read(folder.at.at("units")).unwrap(), b"a.c");
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let linked = std::fs::metadata(folder.at.at("objects/a.o")).unwrap().ino();
            assert_eq!(linked, std::fs::metadata(&whole).unwrap().ino(), "a whole file linked, not copied");
        }
        let at = folder.at.clone();
        drop(folder);
        assert!(!at.exists());
        assert!(whole.is_file(), "what it was linked to stays");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
