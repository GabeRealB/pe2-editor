//! A layer file's content, read and written in one place. A layer holds a file's content as the file itself or, for a
//! large one (`chunks::THRESHOLD`), as a manifest beside it (`<name>.chunks`) naming its chunks in the project's store
//! (`chunks.rs`); these functions read either as the file it stands for, and write a content in the form its length
//! calls for. Code reading or writing what a layer holds - an asset's files, a generated layer's, a plain file of the
//! project - goes through here rather than to the path.
//!
//! What is not a layer's content keeps its own access: the cache's stores (the graph's blobs, the history's), the
//! project's own files (`project.toml`, changesets), and temporary files. The history keeps what is on disk as it is - a
//! manifest, not the chunks it names - so undo restores a large file by its manifest.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use crate::chunks::{self, Manifest, Store};

/// How a layer holds a file's content.
enum Form {
    Plain(PathBuf),
    Chunked {
        manifest: Arc<Manifest>,
        store: Store,
        at: PathBuf,
    },
    /// Text a record holds inline (`records`).
    Inline {
        bytes: Vec<u8>,
        modified: std::time::SystemTime,
    },
    /// In a packed layer's listing (`pack`), or kept in the store for a record.
    Packed {
        manifest: Arc<Manifest>,
        store: Store,
        modified: std::time::SystemTime,
    },
    /// A file of an extension a layer holds, a whole object of its tree (`records::held_at`): read where it is, never
    /// written.
    Object {
        path: PathBuf,
        sha1: String,
        size: u64,
        modified: std::time::SystemTime,
    },
}

/// Manifests read, by what tells their versions apart: a store's file's by its SHA-1 (it never changes), a chunked
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

/// A file kept in the store, by its SHA-1: its manifest.
fn stored_manifest(store: &Store, sha1: &str) -> io::Result<Arc<Manifest>> {
    manifest(format!("{}:{sha1}", store.dir.display()), || store.file_manifest(sha1))
}

/// A chunked file's manifest, at `at` as its metadata says it is.
fn chunked_manifest(at: &Path, meta: &std::fs::Metadata) -> io::Result<Arc<Manifest>> {
    let time = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).unwrap_or_default();
    let key = format!("{}:{}:{}", at.display(), time.as_nanos(), meta.len());
    manifest(key, || Manifest::parse(&std::fs::read_to_string(at)?))
}

/// A held extension's file at a path: its object.
fn held(path: &Path) -> Option<Form> {
    let (held, within) = crate::records::held_at(path)?;
    let file = held.file(&within)?;
    Some(Form::Object {
        path: held.store.object_path(&file.sha1),
        sha1: file.sha1.clone(),
        size: file.size,
        modified: held.modified,
    })
}

/// Where a path's content is: what is known without the disk first - a packed layer's listing, a records layer's
/// records, by the path's prefix - then the file, then its manifest.
fn form(path: &Path) -> io::Result<Form> {
    if let Some((pack, rel)) = crate::pack::Pack::of(path)
        && let Some(entry) = pack.get(&rel)
    {
        return Ok(Form::Packed { manifest: entry.manifest, store: pack.store().clone(), modified: entry.modified });
    }
    if let Some(form) = held(path) {
        return Ok(form);
    }
    if let Some((content, modified, store)) = crate::records::Set::file(path) {
        return Ok(match content {
            crate::records::Content::Text(text) => Form::Inline { bytes: text.into_bytes(), modified },
            crate::records::Content::Stored { sha1, .. } => {
                Form::Packed { manifest: stored_manifest(&store, &sha1)?, store, modified }
            }
        });
    }
    if std::fs::metadata(path).is_ok_and(|m| m.is_file()) {
        return Ok(Form::Plain(path.to_path_buf()));
    }
    let at = chunks::manifest_path(path);
    if let Some(meta) = std::fs::metadata(&at).ok().filter(std::fs::Metadata::is_file) {
        let manifest = chunked_manifest(&at, &meta)?;
        let store = Store::of(path).ok_or_else(|| io::Error::other(format!("{}: no store holds it", at.display())))?;
        return Ok(Form::Chunked { manifest, store, at });
    }
    Err(io::Error::new(io::ErrorKind::NotFound, path.display().to_string()))
}

/// A file's content, whole.
///
/// # Errors
/// It cannot be read.
pub fn read(path: &Path) -> io::Result<Vec<u8>> {
    match form(path)? {
        Form::Plain(path) | Form::Object { path, .. } => std::fs::read(path),
        Form::Inline { bytes, .. } => Ok(bytes),
        Form::Chunked { manifest, store, .. } | Form::Packed { manifest, store, .. } => {
            manifest.read_range(&store, 0, usize::try_from(manifest.size).unwrap_or(usize::MAX))
        }
    }
}

/// A file's content as text.
///
/// # Errors
/// It cannot be read, or is not UTF-8.
pub fn read_to_string(path: &Path) -> io::Result<String> {
    String::from_utf8(read(path)?).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Up to `len` bytes of a file's content from `offset`: fewer past its end.
///
/// # Errors
/// It cannot be opened or read.
pub fn read_range(path: &Path, offset: u64, len: usize) -> io::Result<Vec<u8>> {
    match form(path)? {
        Form::Plain(path) | Form::Object { path, .. } => {
            let mut file = File::open(path)?;
            file.seek(SeekFrom::Start(offset))?;
            let mut out = Vec::new();
            file.take(len as u64).read_to_end(&mut out)?;
            Ok(out)
        }
        Form::Inline { bytes, .. } => {
            let start = usize::try_from(offset).unwrap_or(usize::MAX).min(bytes.len());
            Ok(bytes[start..start.saturating_add(len).min(bytes.len())].to_vec())
        }
        Form::Chunked { manifest, store, .. } | Form::Packed { manifest, store, .. } => {
            manifest.read_range(&store, offset, len)
        }
    }
}

/// Up to `len` bytes from a file's start; nothing where it cannot be read.
#[must_use]
pub fn head(path: &Path, len: usize) -> Vec<u8> {
    read_range(path, 0, len).unwrap_or_default()
}

/// A file's content's length.
///
/// # Errors
/// It cannot be found.
pub fn len(path: &Path) -> io::Result<u64> {
    stat(path).map(|(len, _)| len)
}

/// Whether a layer holds a content at a path, in either form.
#[must_use]
pub fn exists(path: &Path) -> bool {
    crate::pack::is_file(path) || chunks::manifest_path(path).is_file()
}

/// Refuse to write a records layer's file as a file: its records are written (`records`).
fn not_a_record(path: &Path) -> io::Result<()> {
    if crate::records::Set::is_folder(path).is_some() {
        return Err(io::Error::other(format!("{}: a records layer is written by its records", path.display())));
    }
    Ok(())
}

/// The file on disk holding a path's content: itself, or its manifest; none where neither is.
#[must_use]
pub fn stored(path: &Path) -> Option<PathBuf> {
    if path.is_file() {
        return Some(path.to_path_buf());
    }
    Some(chunks::manifest_path(path)).filter(|at| at.is_file())
}

/// A file's content's length and when it was last changed (its manifest's time, for a chunked one).
///
/// # Errors
/// It cannot be found.
pub fn stat(path: &Path) -> io::Result<(u64, std::time::SystemTime)> {
    // What is known without the disk, and a file's own metadata, without reading any content.
    if let Some((pack, rel)) = crate::pack::Pack::of(path)
        && let Some((len, modified)) = pack.stat(&rel)
    {
        return Ok((len, modified));
    }
    if let Some(found) = crate::records::Set::stat(path) {
        return Ok(found);
    }
    if let Some(meta) = std::fs::metadata(path).ok().filter(std::fs::Metadata::is_file) {
        return Ok((meta.len(), meta.modified()?));
    }
    match form(path)? {
        Form::Plain(path) => {
            let meta = std::fs::metadata(path)?;
            Ok((meta.len(), meta.modified()?))
        }
        Form::Chunked { manifest, at, .. } => Ok((manifest.size, std::fs::metadata(at)?.modified()?)),
        Form::Packed { manifest, modified, .. } => Ok((manifest.size, modified)),
        Form::Inline { bytes, modified } => Ok((bytes.len() as u64, modified)),
        Form::Object { size, modified, .. } => Ok((size, modified)),
    }
}

/// A file's content's hash: a chunked file's from its manifest, read from nothing else.
///
/// # Errors
/// It cannot be read.
pub fn sha1(path: &Path) -> io::Result<String> {
    match form(path)? {
        Form::Plain(path) => crate::extract::sha1_file(&path).map_err(io::Error::other),
        Form::Inline { bytes, .. } => Ok(crate::extract::sha1_hex(&bytes)),
        Form::Chunked { manifest, .. } | Form::Packed { manifest, .. } => Ok(manifest.sha1.clone()),
        Form::Object { sha1, .. } => Ok(sha1),
    }
}

/// What a content is stored as at a path in a layer: a manifest's text where it is to be chunked (its chunks put in the
/// store now), else none - the content itself.
///
/// # Errors
/// Its chunks cannot be stored.
pub fn chunked(path: &Path, bytes: &[u8]) -> io::Result<Option<String>> {
    match Store::of(path) {
        Some(store) if bytes.len() >= chunks::THRESHOLD => Ok(Some(store.put(bytes)?.render())),
        _ => Ok(None),
    }
}

/// Write a file's content whole, atomically, in the form its length calls for, the other form removed: what reads it
/// sees the old content or the new, never part of either.
///
/// # Errors
/// Its folder cannot be made, its chunks stored, or it cannot be written.
pub fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    not_a_record(path)?;
    // A packed layer's file is a line of its listing.
    if let Some((pack, rel)) = crate::pack::Pack::of(path) {
        remove_stored(path)?;
        return pack.put(&rel, bytes);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let manifest = chunks::manifest_path(path);
    let (at, content, other) = match chunked(path, bytes)? {
        Some(text) => (manifest.clone(), text.into_bytes(), path.to_path_buf()),
        None => (path.to_path_buf(), bytes.to_vec(), manifest),
    };
    crate::cache::write_atomic(&at, &content).map_err(io::Error::other)?;
    match std::fs::remove_file(other) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Remove a file's content, in whichever form it is.
///
/// # Errors
/// It cannot be removed.
pub fn remove(path: &Path) -> io::Result<()> {
    not_a_record(path)?;
    remove_stored(path)?;
    if let Some((pack, rel)) = crate::pack::Pack::of(path) {
        pack.remove(&rel)?;
    }
    Ok(())
}

/// Remove what is stored of a path on disk: the file, or its manifest.
fn remove_stored(path: &Path) -> io::Result<()> {
    for at in [path.to_path_buf(), chunks::manifest_path(path)] {
        match std::fs::remove_file(at) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    Ok(())
}

/// What serves a chunked content to programs as a file (a scoped filesystem, `vfs::scoped`): given the layer's path,
/// where a program reads it.
type Exposer = Arc<dyn Fn(&Path) -> io::Result<PathBuf> + Send + Sync>;

/// The exposers registered, by the store whose contents each serves (any, for none), and the number its registration
/// was given.
static EXPOSERS: RwLock<Vec<(u64, Option<PathBuf>, Exposer)>> = RwLock::new(Vec::new());

/// An exposer's registration: it serves until this is dropped.
pub struct Exposing(u64);

impl Drop for Exposing {
    fn drop(&mut self) {
        EXPOSERS.write().unwrap_or_else(std::sync::PoisonError::into_inner).retain(|(id, ..)| *id != self.0);
    }
}

/// Have `exposer` serve the chunked contents of a store (`.layers/.store`), or of every store for none, to programs, for
/// as long as what this returns is held; the latest registered for a store serves it.
pub fn expose_with(
    store: Option<&Path>,
    exposer: impl Fn(&Path) -> io::Result<PathBuf> + Send + Sync + 'static,
) -> Exposing {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut exposers = EXPOSERS.write().unwrap_or_else(std::sync::PoisonError::into_inner);
    exposers.push((id, store.map(Path::to_path_buf), Arc::new(exposer)));
    Exposing(id)
}

/// A path a program can open for a content, as a file: the file itself where a layer holds it whole; for a chunked one,
/// where an exposer serves it (`expose_with`), else a copy assembled in the cache (`materialize`).
///
/// # Errors
/// It cannot be found, or neither served nor assembled.
pub fn for_program(path: &Path) -> io::Result<PathBuf> {
    let exposed = |store: &Path| {
        let exposer = {
            let exposers = EXPOSERS.read().unwrap_or_else(std::sync::PoisonError::into_inner);
            let serves = |at: &Option<PathBuf>| at.as_ref().is_none_or(|at| *at == store);
            exposers.iter().rev().find(|(_, at, _)| serves(at)).map(|(.., e)| Arc::clone(e))
        };
        exposer.and_then(|serve| serve(path).ok())
    };
    match form(path)? {
        Form::Plain(path) => Ok(path),
        Form::Inline { .. } => materialize(path),
        Form::Chunked { store, .. } | Form::Packed { store, .. } => match exposed(&store.dir) {
            Some(served) => Ok(served),
            None => materialize(path),
        },
        // A held extension's file: served by its name where a scope is, else the object itself, read-only, in place.
        Form::Object { path: object, .. } => {
            let store = object.ancestors().nth(3).map_or_else(PathBuf::new, Path::to_path_buf);
            Ok(exposed(&store).unwrap_or(object))
        }
    }
}

/// A path a program can open for a content, as a file: the file itself where it is one; a chunked one assembled once,
/// by its hash, in the project's cache (`.cache/materialized/<sha1>/<name>`), its name kept for what reads names.
///
/// # Errors
/// It cannot be read, or the cache cannot be written.
pub fn materialize(path: &Path) -> io::Result<PathBuf> {
    match form(path)? {
        // A whole object is a file already: never copied.
        Form::Plain(path) | Form::Object { path, .. } => Ok(path),
        Form::Inline { bytes, .. } => {
            let layers = path.ancestors().find(|a| a.file_name().is_some_and(|n| n == crate::LAYERS));
            let project = layers.and_then(Path::parent).unwrap_or(Path::new("."));
            let name = path.file_name().unwrap_or_default();
            let sha1 = crate::extract::sha1_hex(&bytes);
            let out = project.join(crate::cache::DIR).join("materialized").join(sha1).join(name);
            if !out.is_file() {
                if let Some(parent) = out.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                crate::cache::write_atomic(&out, &bytes).map_err(io::Error::other)?;
            }
            Ok(out)
        }
        Form::Chunked { manifest, store, .. } | Form::Packed { manifest, store, .. } => {
            let project = store.dir.parent().and_then(Path::parent).unwrap_or(Path::new("."));
            let name = path.file_name().unwrap_or_default();
            let out = project.join(crate::cache::DIR).join("materialized").join(&manifest.sha1).join(name);
            if !out.is_file() {
                let bytes = manifest.read_range(&store, 0, usize::try_from(manifest.size).unwrap_or(usize::MAX))?;
                if let Some(parent) = out.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                crate::cache::write_atomic(&out, &bytes).map_err(io::Error::other)?;
            }
            Ok(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{exists, head, len, read, read_range, remove, sha1, stored, write};

    #[test]
    fn a_content_reads_whole_and_by_range() {
        let dir = std::env::temp_dir().join(format!("pe2-content-{}", std::process::id()));
        let path = dir.join("a/b/c.bin");
        write(&path, b"0123456789").expect("written");
        assert_eq!(read(&path).expect("read"), b"0123456789");
        assert_eq!(read_range(&path, 3, 4).expect("range"), b"3456");
        assert_eq!(read_range(&path, 8, 10).expect("past the end"), b"89");
        assert_eq!(head(&path, 2), b"01");
        assert_eq!(len(&path).expect("len"), 10);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_large_content_in_a_layer_is_chunked_and_read_as_itself() {
        let root = std::env::temp_dir().join(format!("pe2-content-chunked-{}", std::process::id()));
        let path = root.join(".layers/top/a.node/native.str");
        let bytes: Vec<u8> = (0..(3u32 << 20)).map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8).collect();
        write(&path, &bytes).expect("written");
        assert!(!path.exists() && exists(&path), "stored as its manifest");
        assert_eq!(stored(&path), Some(root.join(".layers/top/a.node/native.str.chunks")));
        assert_eq!(read(&path).expect("read"), bytes);
        assert_eq!(read_range(&path, 1_000_000, 16).expect("range"), bytes[1_000_000..1_000_016]);
        assert_eq!(len(&path).expect("len"), bytes.len() as u64);
        assert_eq!(sha1(&path).expect("sha1"), crate::extract::sha1_hex(&bytes));
        // Written small again, it is a file once more.
        write(&path, b"small").expect("written");
        assert!(path.is_file() && stored(&path) == Some(path.clone()));
        remove(&path).expect("removed");
        assert!(!exists(&path));
        let _ = std::fs::remove_dir_all(root);
    }
}
