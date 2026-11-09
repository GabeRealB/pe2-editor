//! Large files' content as chunks: a file a layer holds above `THRESHOLD` is stored as a manifest (`<name>.chunks`)
//! naming its chunks in the project's store (`.layers/.store/<aa>/<hash>`), where each chunk is kept once, by its
//! BLAKE3 hash. Chunks are cut where the content says (`FastCDC`, about 64 KiB each), so an edit to a large file stores
//! only the chunks around it, and what layers, releases and the history hold alike is held once.
//!
//! A manifest is text:
//!
//! ```text
//! pe2 chunks 1
//! size 43511808
//! sha1 <the whole content's>
//! <blake3> <length>
//! ...
//! ```
//!
//! It is the only copy of what it names: the store is not cache. What no manifest of any layer, nor the history, names
//! any more is swept (`unreferenced`).

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Files this long or longer are stored as chunks.
pub const THRESHOLD: usize = 1 << 20;
/// The name a manifest adds to its file's.
pub const SUFFIX: &str = ".chunks";
/// The store, among a project's layers.
pub const STORE: &str = ".store";
/// Where the store keeps whole files by their SHA-1 - a manifest of chunks each, whatever their size: the versions an
/// override was made from (`changeset::Base`), what a layer holds of its own.
const FILES: &str = "files";
const HEADER: &str = "pe2 chunks 1";
const MIN: usize = 16 << 10;
const AVG: usize = 64 << 10;
const MAX: usize = 256 << 10;

/// A large file's content: its length, its hash, and its chunks in order, each with where it starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub size: u64,
    pub sha1: String,
    pub chunks: Vec<(String, u64)>,
    starts: Vec<u64>,
}

impl Manifest {
    pub(crate) fn new(size: u64, sha1: String, chunks: Vec<(String, u64)>) -> Manifest {
        let mut starts = Vec::with_capacity(chunks.len());
        let mut at = 0;
        for (_, len) in &chunks {
            starts.push(at);
            at += len;
        }
        Manifest { size, sha1, chunks, starts }
    }

    /// A manifest's text read.
    ///
    /// # Errors
    /// It is not a manifest, or its chunks do not add up to its size.
    pub fn parse(text: &str) -> io::Result<Manifest> {
        let bad = |why: &str| io::Error::new(io::ErrorKind::InvalidData, format!("a chunk manifest: {why}"));
        let mut lines = text.lines();
        if lines.next() != Some(HEADER) {
            return Err(bad("no header"));
        }
        let mut field = |name: &str| {
            lines.next().and_then(|l| l.strip_prefix(name)).map(str::trim).map(str::to_string).ok_or_else(|| bad(name))
        };
        let size: u64 = field("size ")?.parse().map_err(|_| bad("size"))?;
        let sha1 = field("sha1 ")?;
        let chunks = lines
            .filter(|l| !l.is_empty())
            .map(|l| {
                let (hash, len) = l.split_once(' ').ok_or_else(|| bad("a chunk"))?;
                Ok((hash.to_string(), len.parse().map_err(|_| bad("a chunk's length"))?))
            })
            .collect::<io::Result<Vec<(String, u64)>>>()?;
        let manifest = Manifest::new(size, sha1, chunks);
        if manifest.chunks.iter().map(|(_, l)| l).sum::<u64>() != size {
            return Err(bad("its chunks are not its size"));
        }
        Ok(manifest)
    }

    #[must_use]
    pub fn render(&self) -> String {
        let mut out = format!("{HEADER}\nsize {}\nsha1 {}\n", self.size, self.sha1);
        for (hash, len) in &self.chunks {
            let _ = writeln!(out, "{hash} {len}");
        }
        out
    }

    /// Up to `len` bytes from `offset`, from the store: only the chunks they are in are read.
    ///
    /// # Errors
    /// A chunk is missing from the store, or cannot be read.
    pub fn read_range(&self, store: &Store, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        let end = offset.saturating_add(len as u64).min(self.size);
        let mut out = Vec::with_capacity(usize::try_from(end.saturating_sub(offset)).unwrap_or(0));
        if offset >= end {
            return Ok(out);
        }
        let first = self.starts.partition_point(|start| *start <= offset).saturating_sub(1);
        for (i, (hash, size)) in self.chunks.iter().enumerate().skip(first) {
            let start = self.starts[i];
            if start >= end {
                break;
            }
            let chunk = store.get(hash)?;
            if chunk.len() as u64 != *size {
                return Err(io::Error::new(io::ErrorKind::InvalidData, format!("chunk {hash} is not as named")));
            }
            let from = usize::try_from(offset.saturating_sub(start)).unwrap_or(0);
            let to = usize::try_from((end - start).min(*size)).unwrap_or(chunk.len());
            out.extend_from_slice(&chunk[from..to]);
        }
        Ok(out)
    }
}

/// A project's chunk store.
#[derive(Clone, Debug)]
pub struct Store {
    pub dir: PathBuf,
}

impl Store {
    /// The store of the project in `root`.
    #[must_use]
    pub fn at(root: &Path) -> Store {
        Store { dir: root.join(crate::LAYERS).join(STORE) }
    }

    fn file_path(&self, sha1: &str) -> PathBuf {
        self.dir.join(FILES).join(&sha1[..2.min(sha1.len())]).join(sha1)
    }

    /// Keep a file whole, by its SHA-1 (given), its chunks shared with every other file holding them.
    ///
    /// # Errors
    /// It cannot be written.
    pub fn put_file(&self, bytes: &[u8]) -> io::Result<String> {
        let sha1 = crate::extract::sha1_hex(bytes);
        let path = self.file_path(&sha1);
        if path.exists() {
            // Kept again: as new to a sweep as one written now, its chunks too.
            let _ = std::fs::File::options().write(true).open(&path).and_then(|f| f.set_modified(SystemTime::now()));
            let _ = self.put(bytes)?;
            return Ok(sha1);
        }
        let manifest = self.put(bytes)?;
        std::fs::create_dir_all(path.parent().unwrap_or(&self.dir))?;
        let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
        std::fs::write(&temporary, manifest.render())?;
        std::fs::rename(&temporary, &path)?;
        Ok(sha1)
    }

    /// A file kept whole, by its SHA-1.
    ///
    /// # Errors
    /// It is not kept, or its chunks are not all there.
    pub fn get_file(&self, sha1: &str) -> io::Result<Vec<u8>> {
        let manifest = Manifest::parse(&std::fs::read_to_string(self.file_path(sha1))?)?;
        let len = usize::try_from(manifest.size).map_err(io::Error::other)?;
        manifest.read_range(self, 0, len)
    }

    /// The manifest of a file kept whole, by its SHA-1: for reading it by range.
    ///
    /// # Errors
    /// It is not kept.
    pub fn file_manifest(&self, sha1: &str) -> io::Result<Manifest> {
        Manifest::parse(&std::fs::read_to_string(self.file_path(sha1))?)
    }

    /// Whether a file is kept, by its SHA-1.
    #[must_use]
    pub fn has_file(&self, sha1: &str) -> bool {
        self.file_path(sha1).is_file()
    }

    /// Every file kept whole, by SHA-1, with its manifest's text.
    fn files(&self) -> Vec<(String, PathBuf)> {
        let shards = std::fs::read_dir(self.dir.join(FILES)).into_iter().flatten().flatten();
        shards
            .flat_map(|shard| std::fs::read_dir(shard.path()).into_iter().flatten().flatten())
            .filter_map(|file| {
                let name = file.file_name().to_str().filter(|n| Path::new(n).extension().is_none())?.to_string();
                Some((name, file.path()))
            })
            .collect()
    }

    /// The store of the project whose layers hold a path: none for a path in no project's `.layers`.
    #[must_use]
    pub fn of(path: &Path) -> Option<Store> {
        let layers = path.ancestors().find(|a| a.file_name().is_some_and(|n| n == crate::LAYERS))?;
        Some(Store { dir: layers.join(STORE) })
    }

    fn path(&self, hash: &str) -> PathBuf {
        self.dir.join(&hash[..2.min(hash.len())]).join(hash)
    }

    /// A chunk.
    ///
    /// # Errors
    /// It is not in the store.
    pub fn get(&self, hash: &str) -> io::Result<Vec<u8>> {
        std::fs::read(self.path(hash))
    }

    /// Store a content's chunks - those not stored already - and give its manifest.
    ///
    /// # Errors
    /// A chunk cannot be written.
    pub fn put(&self, bytes: &[u8]) -> io::Result<Manifest> {
        let mut chunks = Vec::new();
        for cut in fastcdc::v2020::FastCDC::new(bytes, MIN, AVG, MAX) {
            let chunk = &bytes[cut.offset..cut.offset + cut.length];
            let hash = blake3::hash(chunk).to_hex().to_string();
            let path = self.path(&hash);
            if path.exists() {
                // Stored again: as new to a sweep as one written now (`sweep_settled`).
                let _ =
                    std::fs::File::options().write(true).open(&path).and_then(|f| f.set_modified(SystemTime::now()));
            } else {
                std::fs::create_dir_all(path.parent().unwrap_or(&self.dir))?;
                let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
                std::fs::File::create(&temporary)?.write_all(chunk)?;
                std::fs::rename(&temporary, &path)?;
            }
            chunks.push((hash, cut.length as u64));
        }
        Ok(Manifest::new(bytes.len() as u64, crate::extract::sha1_hex(bytes), chunks))
    }

    /// Every chunk it holds, by hash.
    #[must_use]
    pub fn hashes(&self) -> BTreeSet<String> {
        // Chunks are sharded by two characters; the files kept whole are elsewhere.
        let shards = std::fs::read_dir(&self.dir).into_iter().flatten().flatten().filter(|s| s.file_name().len() == 2);
        shards
            .flat_map(|shard| std::fs::read_dir(shard.path()).into_iter().flatten().flatten())
            .filter_map(|chunk| {
                chunk.file_name().to_str().filter(|n| Path::new(n).extension().is_none()).map(str::to_string)
            })
            .collect()
    }

    /// Remove chunks. Returns how many bytes it freed.
    #[must_use = "how much was freed"]
    pub fn remove(&self, hashes: &BTreeSet<String>) -> u64 {
        let mut freed = 0;
        for hash in hashes {
            let path = self.path(hash);
            freed += std::fs::metadata(&path).map_or(0, |m| m.len());
            let _ = std::fs::remove_file(path);
        }
        freed
    }
}

/// A file's manifest: its path with `.chunks` added.
#[must_use]
pub fn manifest_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(SUFFIX);
    PathBuf::from(name)
}

/// The file a manifest stands for: its path without `.chunks`; none for a path that is no manifest.
#[must_use]
pub fn logical(path: &Path) -> Option<PathBuf> {
    let text = path.to_str()?;
    text.strip_suffix(SUFFIX).map(PathBuf::from)
}

/// The chunks no manifest names: of the store, those absent from `named`.
#[must_use]
pub fn unreferenced(store: &Store, named: &BTreeSet<String>) -> BTreeSet<String> {
    store.hashes().difference(named).cloned().collect()
}

/// What a pass over a project's stored files did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pass {
    pub files: usize,
    pub before: u64,
    pub after: u64,
}

/// Every file of a folder, recursively, its hidden ones too (`.chunks` manifests are not hidden; the store is not walked).
fn files_under(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(t) if t.is_dir() && entry.file_name() != STORE => files_under(&path, out),
            Ok(t) if t.is_file() => out.push(path),
            _ => {}
        }
    }
}

/// Store a project's large files as chunks: each file the index lists - an asset's, or a plain file - in its layers in
/// `.layers/` (not its mounts), as long as `THRESHOLD` or longer and kept as itself until now, is put in the store and
/// replaced by its manifest. What a layer holds that is not its content (a generated layer's record of its plan) stays
/// as it is. The content is the same, so nothing that recorded it changes. Returns how many files moved, and the bytes
/// they held and hold now.
///
/// # Errors
/// A file cannot be read, or its chunks or manifest written.
pub fn migrate(project: &crate::Project, index: &crate::index::Index) -> io::Result<Pass> {
    let mut pass = Pass::default();
    let layers: Vec<&Path> = project.layers.iter().filter(|l| !l.mounted).map(|l| l.path.as_path()).collect();
    let in_layers = |path: &Path| layers.iter().any(|layer| path.starts_with(layer));
    let mut found: BTreeSet<PathBuf> = BTreeSet::new();
    for entry in index.entries.values().filter(|e| e.kind == crate::index::Kind::File) {
        found.insert(entry.disk.clone());
    }
    for asset in index.assets.values() {
        found.extend(asset.on_disk().files.iter().filter(|f| f.made_by.is_none()).map(|f| f.path.clone()));
    }
    for path in found.into_iter().filter(|p| in_layers(p) && p.is_file()) {
        let len = std::fs::metadata(&path)?.len();
        if usize::try_from(len).unwrap_or(usize::MAX) < THRESHOLD {
            continue;
        }
        let Some(store) = Store::of(&path) else { continue };
        let bytes = std::fs::read(&path)?;
        let manifest = store.put(&bytes)?.render();
        crate::cache::write_atomic(&manifest_path(&path), manifest.as_bytes()).map_err(io::Error::other)?;
        std::fs::remove_file(&path)?;
        pass.files += 1;
        pass.before += len;
        pass.after += manifest.len() as u64;
    }
    let store = Store { dir: project.root.join(crate::LAYERS).join(STORE) };
    let held: u64 = store.hashes().iter().map(|h| std::fs::metadata(store.path(h)).map_or(0, |m| m.len())).sum();
    pass.after += held;
    Ok(pass)
}

/// The chunks a manifest's text names; none where the text is no manifest.
fn named_by(text: &[u8]) -> Vec<String> {
    let manifest = std::str::from_utf8(text).ok().and_then(|t| Manifest::parse(t).ok());
    manifest.map_or_else(Vec::new, |m| m.chunks.into_iter().map(|(h, _)| h).collect())
}

/// Remove the chunks nothing names: no manifest of any layer, nor any content the history keeps (a manifest a step
/// can put back). Returns how many went, and the bytes they held. Only where nothing writes the project meanwhile (under
/// its lock): a write stores its chunks before the manifest naming them.
///
/// # Errors
/// The store cannot be read.
pub fn sweep(project: &crate::Project) -> io::Result<(usize, u64)> {
    sweep_settled(project, Duration::ZERO)
}

/// How often what holds the project for long (the editor) sweeps the store, and how long a chunk is left that nothing
/// names: longer than any write takes from storing it to naming it.
pub const SWEEP_EVERY: Duration = Duration::from_hours(24);
pub const SETTLED: Duration = Duration::from_hours(1);
/// When it was swept last, in the cache.
const SWEPT: &str = "chunks.swept";

/// Sweep the store where it was not swept for `SWEEP_EVERY`, of chunks nothing names and stored longer ago than
/// `SETTLED`, while the project is written: how many went and the bytes they held, none where it was not due.
#[must_use = "what was swept is the sweep's report"]
pub fn sweep_if_due(project: &crate::Project) -> Option<io::Result<(usize, u64)>> {
    let marker = project.root.join(crate::cache::DIR).join(SWEPT);
    let age = std::fs::metadata(&marker).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
    if age.is_some_and(|age| age < SWEEP_EVERY) {
        return None;
    }
    let _ = std::fs::write(&marker, b"");
    Some(sweep_settled(project, SETTLED))
}

/// Remove the chunks nothing names, as `sweep` does, of those stored longer ago than `settled`: what a write stores just
/// before its manifest names it is left to it.
///
/// # Errors
/// The store cannot be read.
pub fn sweep_settled(project: &crate::Project, settled: Duration) -> io::Result<(usize, u64)> {
    let mut named = BTreeSet::new();
    let mut found = Vec::new();
    for layer in project.layers.iter().filter(|l| !l.mounted) {
        files_under(&layer.path, &mut found);
    }
    files_under(&project.root.join(crate::cache::DIR).join("history"), &mut found);
    // The changesets name the files kept whole that overrides were made from; the history, those its steps can put back.
    found.extend(project.layers.iter().map(|l| l.changeset.clone()));
    let mut kept = BTreeSet::new();
    for path in found {
        let is_manifest = logical(&path).is_some();
        let small = std::fs::metadata(&path).is_ok_and(|m| m.len() < (THRESHOLD as u64));
        if (is_manifest || small)
            && let Ok(text) = std::fs::read(&path)
        {
            named.extend(named_by(&text));
            kept.extend(sha1s_in(&text));
        }
    }
    // A packed layer's listing names its chunks too.
    for layer in project.layers.iter().filter(|l| l.path.join(crate::pack::FILE).is_file()) {
        named.extend(crate::pack::Pack::open(&layer.path)?.chunks());
    }
    let store = Store::at(&project.root);
    let old = |path: &Path| {
        let modified = std::fs::metadata(path).and_then(|m| m.modified());
        settled.is_zero() || modified.ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age >= settled)
    };
    // A file kept whole that nothing names goes; one named keeps its chunks.
    let (mut gone_files, mut freed) = (0, 0);
    for (sha1, path) in store.files() {
        let text = std::fs::read(&path).unwrap_or_default();
        if kept.contains(&sha1) || !old(&path) {
            named.extend(named_by(&text));
        } else {
            freed += text.len() as u64;
            gone_files += 1;
            let _ = std::fs::remove_file(&path);
        }
    }
    let mut gone = unreferenced(&store, &named);
    gone.retain(|hash| old(&store.path(hash)));
    freed += store.remove(&gone);
    Ok((gone.len() + gone_files, freed))
}

/// What may be a file's SHA-1 in a text: every run of forty hexadecimal digits.
fn sha1s_in(text: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    for run in text.split(|b| !b.is_ascii_hexdigit()) {
        if run.len() == 40 {
            out.push(String::from_utf8_lossy(run).into_owned());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{Manifest, Store};

    fn content(len: usize, seed: u32) -> Vec<u8> {
        let mut x = seed;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x >> 24) as u8
            })
            .collect()
    }

    #[test]
    fn a_large_file_reads_back_by_range_and_an_edit_stores_little() {
        let dir = std::env::temp_dir().join(format!("pe2-chunks-{}", std::process::id()));
        let store = Store { dir: dir.clone() };
        let bytes = content(3 << 20, 7);
        let manifest = store.put(&bytes).expect("stored");
        assert_eq!(Manifest::parse(&manifest.render()).expect("parsed"), manifest);
        for (offset, len) in [(0, 10), (12_345, 200_000), (bytes.len() - 5, 100), (1 << 20, 1 << 20)] {
            let end = (offset + len).min(bytes.len());
            assert_eq!(manifest.read_range(&store, offset as u64, len).expect("read"), bytes[offset..end]);
        }
        // A few bytes changed in the middle: only the chunks around them are new.
        let before = store.hashes().len();
        let mut edited = bytes.clone();
        edited[1_500_000..1_500_010].copy_from_slice(b"0123456789");
        let again = store.put(&edited).expect("stored");
        let new = store.hashes().len() - before;
        assert!(new <= 3, "{new} chunks stored for a ten-byte edit");
        assert_eq!(again.read_range(&store, 0, edited.len()).expect("read"), edited);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_sweep_while_writing_leaves_what_was_stored_lately() {
        use std::time::{Duration, SystemTime};
        let root = std::env::temp_dir().join(format!("pe2-chunks-sweep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        crate::create(&root).unwrap();
        let project = crate::Project::load(&root).unwrap();
        let store = Store { dir: root.join(crate::LAYERS).join(super::STORE) };
        let (old, fresh, again) = (content(200_000, 1), content(200_000, 2), content(200_000, 3));
        let age = |m: &Manifest, by: Duration| {
            for (hash, _) in &m.chunks {
                let file = std::fs::File::options().write(true).open(store.path(hash)).unwrap();
                file.set_modified(SystemTime::now() - by).unwrap();
            }
        };
        let old = store.put(&old).unwrap();
        age(&old, Duration::from_hours(2));
        let fresh = store.put(&fresh).unwrap();
        // Stored long ago, and stored again now (a write reusing it): new again.
        let again = store.put(&again).unwrap();
        age(&again, Duration::from_hours(2));
        let again = store.put(&content(200_000, 3)).unwrap();
        // Named by no manifest: the old ones alone go.
        let (gone, _) = super::sweep_settled(&project, super::SETTLED).unwrap();
        assert_eq!(gone, old.chunks.len());
        let kept = store.hashes();
        assert!(fresh.chunks.iter().chain(&again.chunks).all(|(h, _)| kept.contains(h)));
        assert!(old.chunks.iter().all(|(h, _)| !kept.contains(h)));
        // Under the lock, nothing writes: whatever nothing names goes.
        assert_eq!(super::sweep(&project).unwrap().0, fresh.chunks.len() + again.chunks.len());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
