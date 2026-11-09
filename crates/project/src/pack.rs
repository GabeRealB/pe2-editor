//! A packed layer: its files kept not as files but as one listing in its folder (`.pack`) - each file's path, length,
//! hash, time and chunks - the chunks in the project's store (`chunks`), as large files are. A layer is packed where
//! `project.toml` says (`storage = "packed"`), for one that is read and written by the project alone, such as what a
//! plan extracts: nothing to scan, and what is alike stored once. What reads and writes a layer's content
//! (`content`) and what lists its folders (`read_dir`, `is_dir`, `is_file`) find a packed layer's files as if they
//! were there; a file really there beside it (a plan's record) is read as it is.
//!
//! The listing is a journal - each file put or removed appended as a line - read again in order, so a write is one
//! line and a crash loses nothing written; it is written anew, compacted, when it holds far more lines than files.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::chunks::{Manifest, Store};

/// The listing's name in a packed layer's folder.
pub const FILE: &str = ".pack";

const HEADER: &str = "pe2 pack 1";

/// A file of a packed layer: its content's manifest, and when it was written.
#[derive(Clone, Debug)]
pub struct Entry {
    pub manifest: Manifest,
    pub modified: SystemTime,
}

/// A packed layer: its folder, its files by path, the store holding their chunks, and its journal.
#[derive(Debug)]
pub struct Pack {
    root: PathBuf,
    store: Store,
    entries: RwLock<BTreeMap<String, Entry>>,
    journal: Mutex<Journal>,
}

#[derive(Debug, Default)]
struct Journal {
    file: Option<File>,
    lines: usize,
}

/// The packs open in this process, by their folders.
static PACKS: RwLock<Vec<Arc<Pack>>> = RwLock::new(Vec::new());

fn bad(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("a pack: {why}"))
}

fn stamp(time: SystemTime) -> String {
    let d = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    format!("{}.{:09}", d.as_secs(), d.subsec_nanos())
}

fn time(text: &str) -> Option<SystemTime> {
    let (secs, nanos) = text.split_once('.')?;
    Some(UNIX_EPOCH + Duration::new(secs.parse().ok()?, nanos.parse().ok()?))
}

/// A line putting a file.
fn put_line(rel: &str, entry: &Entry) -> String {
    let chunks: Vec<String> = entry.manifest.chunks.iter().map(|(h, l)| format!("{h}:{l}")).collect();
    let m = &entry.manifest;
    format!("put\t{rel}\t{}\t{}\t{}\t{}", m.size, m.sha1, stamp(entry.modified), chunks.join(","))
}

/// A listing's entries, its lines replayed in order; how many lines it has.
fn replay(text: &str) -> io::Result<(BTreeMap<String, Entry>, usize)> {
    let mut lines = text.lines();
    if lines.next() != Some(HEADER) {
        return Err(bad("no header"));
    }
    let (mut entries, mut count) = (BTreeMap::new(), 0);
    for line in lines.filter(|l| !l.is_empty()) {
        count += 1;
        let fields: Vec<&str> = line.split('\t').collect();
        match fields.as_slice() {
            ["put", rel, size, sha1, modified, chunks] => {
                let chunks: Option<Vec<(String, u64)>> = chunks
                    .split(',')
                    .filter(|c| !c.is_empty())
                    .map(|c| c.split_once(':').and_then(|(h, l)| Some((h.to_string(), l.parse().ok()?))))
                    .collect();
                let size: u64 = size.parse().map_err(|_| bad("a length"))?;
                let manifest = Manifest::new(size, (*sha1).to_string(), chunks.ok_or_else(|| bad("a chunk"))?);
                let modified = time(modified).ok_or_else(|| bad("a time"))?;
                entries.insert((*rel).to_string(), Entry { manifest, modified });
            }
            ["del", rel] => {
                entries.remove(*rel);
            }
            // A line cut short by a crash is the last: what it was writing was not written.
            _ => break,
        }
    }
    Ok((entries, count))
}

impl Pack {
    /// The packed layer in a folder: its listing read, or none yet; open once a process.
    ///
    /// # Errors
    /// Its listing cannot be read, or the folder is in no project's layers.
    pub fn open(root: &Path) -> io::Result<Arc<Pack>> {
        if let Some(open) =
            PACKS.read().unwrap_or_else(std::sync::PoisonError::into_inner).iter().find(|p| p.root == root)
        {
            return Ok(open.clone());
        }
        let store = Store::of(&root.join(FILE)).ok_or_else(|| bad("not in a project's layers"))?;
        let (entries, lines) = match std::fs::read_to_string(root.join(FILE)) {
            Ok(text) => replay(&text)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => (BTreeMap::new(), 0),
            Err(e) => return Err(e),
        };
        let pack = Arc::new(Pack {
            root: root.to_path_buf(),
            store,
            entries: RwLock::new(entries),
            journal: Mutex::new(Journal { file: None, lines }),
        });
        let mut packs = PACKS.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(open) = packs.iter().find(|p| p.root == root) {
            return Ok(open.clone());
        }
        packs.push(pack.clone());
        Ok(pack)
    }

    /// The pack holding a path, and the path within it: none where no open pack's folder holds it.
    #[must_use]
    pub fn of(path: &Path) -> Option<(Arc<Pack>, String)> {
        let packs = PACKS.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        let pack = packs.iter().find(|p| path.starts_with(&p.root) && path != p.root)?;
        let rel = path.strip_prefix(&pack.root).ok()?.to_string_lossy().replace('\\', "/");
        Some((pack.clone(), rel))
    }

    #[must_use]
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// A file of it.
    #[must_use]
    pub fn get(&self, rel: &str) -> Option<Entry> {
        self.entries.read().unwrap_or_else(std::sync::PoisonError::into_inner).get(rel).cloned()
    }

    /// How many files it holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.read().unwrap_or_else(std::sync::PoisonError::into_inner).len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Append a line to the journal, opening it - with its header, where it is new - the first time.
    fn append(&self, line: &str) -> io::Result<()> {
        let mut journal = self.journal.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if journal.file.is_none() {
            let at = self.root.join(FILE);
            std::fs::create_dir_all(&self.root)?;
            let new = !at.exists();
            let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&at)?;
            if new {
                writeln!(file, "{HEADER}")?;
            }
            journal.file = Some(file);
        }
        let file = journal.file.as_mut().expect("opened");
        writeln!(file, "{line}")?;
        journal.lines += 1;
        Ok(())
    }

    /// Write a file into it: its chunks stored, its line appended.
    ///
    /// # Errors
    /// Its chunks or its line cannot be written.
    pub fn put(&self, rel: &str, bytes: &[u8]) -> io::Result<()> {
        let entry = Entry { manifest: self.store.put(bytes)?, modified: SystemTime::now() };
        self.append(&put_line(rel, &entry))?;
        self.entries.write().unwrap_or_else(std::sync::PoisonError::into_inner).insert(rel.to_string(), entry);
        self.compact_if_due()
    }

    /// Remove a file from it. Whether it held one.
    ///
    /// # Errors
    /// Its line cannot be written.
    pub fn remove(&self, rel: &str) -> io::Result<bool> {
        if self.get(rel).is_none() {
            return Ok(false);
        }
        self.append(&format!("del\t{rel}"))?;
        self.entries.write().unwrap_or_else(std::sync::PoisonError::into_inner).remove(rel);
        self.compact_if_due().map(|()| true)
    }

    /// Write the listing anew, a line a file, where it holds more than twice as many lines as files.
    fn compact_if_due(&self) -> io::Result<()> {
        let due = {
            let journal = self.journal.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            journal.lines > 1024 && journal.lines > 2 * self.len()
        };
        if due { self.compact() } else { Ok(()) }
    }

    /// Write the listing anew: a line a file it holds, atomically.
    ///
    /// # Errors
    /// It cannot be written.
    pub fn compact(&self) -> io::Result<()> {
        let mut journal = self.journal.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let entries = self.entries.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut text = format!("{HEADER}\n");
        for (rel, entry) in entries.iter() {
            text.push_str(&put_line(rel, entry));
            text.push('\n');
        }
        crate::cache::write_atomic(&self.root.join(FILE), text.as_bytes()).map_err(io::Error::other)?;
        *journal = Journal { file: None, lines: entries.len() };
        Ok(())
    }

    /// What is directly in a folder of it (its path; empty for the top): each name, and whether it is a folder.
    #[must_use]
    pub fn list(&self, rel: &str) -> Vec<(String, bool)> {
        let prefix = if rel.is_empty() { String::new() } else { format!("{rel}/") };
        let entries = self.entries.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut out: Vec<(String, bool)> = Vec::new();
        for (path, _) in entries.range(prefix.clone()..).take_while(|(p, _)| p.starts_with(&prefix)) {
            let rest = &path[prefix.len()..];
            let (name, folder) = rest.split_once('/').map_or((rest, false), |(n, _)| (n, true));
            if out.last().is_none_or(|(last, _)| last != name) {
                out.push((name.to_string(), folder));
            }
        }
        out
    }

    /// Whether a path of it is a folder: something is in it.
    #[must_use]
    pub fn is_folder(&self, rel: &str) -> bool {
        let prefix = format!("{rel}/");
        let entries = self.entries.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        entries.range(prefix.clone()..).next().is_some_and(|(p, _)| p.starts_with(&prefix))
    }

    /// Every chunk it names.
    #[must_use]
    pub fn chunks(&self) -> Vec<String> {
        let entries = self.entries.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        entries.values().flat_map(|e| e.manifest.chunks.iter().map(|(h, _)| h.clone())).collect()
    }
}

/// What is in a folder, on disk or in the packed layer holding it: each entry's name, its path and whether it is a
/// folder - a file really there before one the pack holds alike.
#[must_use]
pub fn read_dir(dir: &Path) -> Vec<(String, PathBuf, bool)> {
    let mut out: Vec<(String, PathBuf, bool)> = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() || kind.is_file() {
            out.push((entry.file_name().to_string_lossy().into_owned(), entry.path(), kind.is_dir()));
        }
    }
    // A records layer's tree, as its records make it.
    for (name, folder) in crate::records::Set::list(dir).into_iter().flatten() {
        if !out.iter().any(|(n, ..)| *n == name) {
            out.push((name.clone(), dir.join(&name), folder));
        }
    }
    let packed = Pack::of(&dir.join(FILE)).map(|(pack, _)| pack).filter(|pack| dir.starts_with(&pack.root));
    if let Some(pack) = packed {
        let rel =
            dir.strip_prefix(&pack.root).map_or_else(|_| String::new(), |r| r.to_string_lossy().replace('\\', "/"));
        for (name, folder) in pack.list(&rel) {
            if !out.iter().any(|(n, ..)| *n == name) {
                out.push((name.clone(), dir.join(&name), folder));
            }
        }
    }
    out
}

/// Whether a path is a folder, on disk or in a packed layer.
#[must_use]
pub fn is_dir(path: &Path) -> bool {
    path.is_dir()
        || Pack::of(path).is_some_and(|(pack, rel)| pack.is_folder(&rel))
        || crate::records::Set::is_folder(path) == Some(true)
}

/// Whether a path is a file, on disk or in a packed layer.
#[must_use]
pub fn is_file(path: &Path) -> bool {
    path.is_file()
        || Pack::of(path).is_some_and(|(pack, rel)| pack.get(&rel).is_some())
        || crate::records::Set::is_file(path) == Some(true)
}

/// Set how a layer is stored in `project.toml` (`storage`): none for the default, its files as files.
fn set_storage(project: &crate::Project, layer: &str, storage: Option<&str>) -> io::Result<()> {
    let at = project.root.join(crate::FILE);
    let text = std::fs::read_to_string(&at)?;
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(io::Error::other)?;
    let layers = doc.get_mut("layer").and_then(toml_edit::Item::as_array_of_tables_mut);
    let table = layers
        .and_then(|layers| layers.iter_mut().find(|t| t.get("name").and_then(toml_edit::Item::as_str) == Some(layer)))
        .ok_or_else(|| io::Error::other(format!("{layer}: no such layer in project.toml")))?;
    match storage {
        Some(storage) => {
            table.insert("storage", toml_edit::value(storage));
        }
        None => {
            table.remove("storage");
        }
    }
    crate::cache::write_atomic(&at, doc.to_string().as_bytes()).map_err(io::Error::other)
}

/// The layer of a project to pack or unpack: a generated one, as only those are packed so far.
fn generated<'a>(project: &'a crate::Project, name: &str) -> io::Result<&'a crate::Layer> {
    let layer = project
        .layers
        .iter()
        .find(|l| l.name == name)
        .ok_or_else(|| io::Error::other(format!("{name}: no such layer")))?;
    if layer.kind != crate::LayerKind::Generated {
        return Err(io::Error::other(format!("{name}: only a generated layer is packed, so far")));
    }
    Ok(layer)
}

/// Every file of a folder, recursively, but what is hidden (its listing, its record's companions starting with `.`).
fn files_in(dir: &Path, rel: &str, out: &mut Vec<(String, PathBuf)>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let child = if rel.is_empty() { name } else { format!("{rel}/{name}") };
        match entry.file_type() {
            Ok(t) if t.is_dir() => files_in(&entry.path(), &child, out),
            Ok(t) if t.is_file() => out.push((child, entry.path())),
            _ => {}
        }
    }
}

/// Pack a generated layer: each file its plan wrote there - all but its record (`plan::RECORD`), a large one's manifest
/// as the file it stands for - put in its listing and removed from its folder; `project.toml` says it is packed. How
/// many files went in.
///
/// # Errors
/// It is no generated layer, or a file cannot be read, stored or removed.
pub fn pack_layer(project: &crate::Project, name: &str) -> io::Result<usize> {
    let layer = generated(project, name)?;
    let pack = Pack::open(&layer.path)?;
    let mut found = Vec::new();
    files_in(&layer.path, "", &mut found);
    let mut count = 0;
    for (rel, path) in found.into_iter().filter(|(rel, _)| rel != crate::plan::RECORD) {
        let logical = crate::chunks::logical(&path).unwrap_or_else(|| path.clone());
        let rel = crate::chunks::logical(Path::new(&rel)).map_or(rel, |l| l.to_string_lossy().into_owned());
        let bytes = crate::content::read(&logical)?;
        pack.put(&rel, &bytes)?;
        std::fs::remove_file(&path)?;
        count += 1;
    }
    remove_empty(&layer.path);
    set_storage(project, name, Some("packed"))?;
    Ok(count)
}

/// Unpack a layer: each file of its listing written back as a file (a large one as its manifest), the listing removed,
/// and `project.toml` no longer saying it is packed. How many files came out.
///
/// # Errors
/// It is no generated layer, or a file cannot be read or written.
pub fn unpack_layer(project: &crate::Project, name: &str) -> io::Result<usize> {
    let layer = generated(project, name)?;
    let pack = Pack::open(&layer.path)?;
    let entries = pack.entries.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    for (rel, entry) in &entries {
        let bytes =
            entry.manifest.read_range(&pack.store, 0, usize::try_from(entry.manifest.size).unwrap_or(usize::MAX))?;
        let at = layer.path.join(rel);
        std::fs::create_dir_all(at.parent().unwrap_or(&layer.path))?;
        match crate::content::chunked(&at, &bytes)? {
            Some(manifest) => crate::cache::write_atomic(&crate::chunks::manifest_path(&at), manifest.as_bytes()),
            None => crate::cache::write_atomic(&at, &bytes),
        }
        .map_err(io::Error::other)?;
    }
    std::fs::remove_file(layer.path.join(FILE))?;
    PACKS.write().unwrap_or_else(std::sync::PoisonError::into_inner).retain(|p| p.root != layer.path);
    set_storage(project, name, None)?;
    Ok(entries.len())
}

/// Remove the empty folders under a folder, it kept.
fn remove_empty(dir: &Path) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            remove_empty(&entry.path());
            let _ = std::fs::remove_dir(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Pack, is_dir, is_file, read_dir};

    #[test]
    fn a_packed_layer_holds_its_files_and_reads_back_after_a_restart_of_its_journal() {
        let root = std::env::temp_dir().join(format!("pe2-pack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let layer = root.join(".layers/gen");
        let pack = Pack::open(&layer).unwrap();
        pack.put("a.node/native.bin", b"native").unwrap();
        pack.put("a.node/node.toml", b"x = 1\n").unwrap();
        pack.put("b/c.bin", b"c").unwrap();
        pack.remove("b/c.bin").unwrap();
        assert!(is_dir(&layer.join("a.node")) && is_file(&layer.join("a.node/native.bin")));
        assert!(!is_file(&layer.join("b/c.bin")) && !is_dir(&layer.join("b")));
        let listed: Vec<(String, bool)> = read_dir(&layer).into_iter().map(|(n, _, d)| (n, d)).collect();
        assert!(listed.contains(&("a.node".into(), true)), "{listed:?}");
        let text = std::fs::read_to_string(layer.join(super::FILE)).unwrap();
        let (entries, lines) = super::replay(&text).unwrap();
        assert_eq!((entries.len(), lines), (2, 4));
        assert_eq!(entries["a.node/native.bin"].manifest.sha1, crate::extract::sha1_hex(b"native"));
        pack.compact().unwrap();
        let (entries, lines) = super::replay(&std::fs::read_to_string(layer.join(super::FILE)).unwrap()).unwrap();
        assert_eq!((entries.len(), lines), (2, 2));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
