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
//!
//! The store keeps files three ways, by what they are:
//!
//! - **chunked** (`files/<aa>/<sha1>`): a manifest of chunks, for content that is edited, or shared in parts with other
//!   versions - what a record keeps of a binary file (`put_file`);
//! - **whole** (`objects/<aa>/<sha1>`): the bytes themselves, written once and read in one go, for what is only ever
//!   taken or replaced whole and shares nothing in part with its other versions (`put_object`);
//! - **trees** (`trees/<aa>/<sha1>`): a listing of named whole objects, one hash for a set of files kept as one - an
//!   extension (`put_tree`):
//!
//! ```text
//! pe2 tree 1
//! <sha1> <size> <mode> <name>
//! ...
//! ```
//!
//! where the mode says whether the file is run (`755`, a program it ships) or not (`644`).
//!
//! Each says its own format (a manifest's and a tree's header; an object is its bytes), so none needs a mark of the
//! store's.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::io;
use std::path::Path;
use std::sync::Arc;

use pe2_io::{Filesystem, Open};

/// Where a store keeps what it keeps by `key`, as `ab/abcdef`: an error where the key is no name a file can have.
fn sharded(dir: &HostPath, key: &str) -> io::Result<HostPath> {
    Ok(dir.join(&RelPath::sharded(key, "").map_err(io::Error::other)?))
}
use pe2_ext::Sha1;
use pe2_path::{Name, RelPath};
use sfs::HostPath;
use std::time::Duration;

/// Files this long or longer are stored as chunks.
pub const THRESHOLD: usize = 1 << 20;

/// What this process wrote into stores and has not had on disk yet - as many as are worth telling apart.
static WRITTEN: std::sync::Mutex<Vec<HostPath>> = std::sync::Mutex::new(Vec::new());
/// How many of them are kept: a process that fills a layer from the discs writes far more, and takes none of them up.
const KEPT: usize = 4096;

/// A file was written into a store.
fn written(path: &HostPath) {
    let mut written = WRITTEN.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if written.len() < KEPT {
        written.push(path.clone());
    }
}

/// Put what the store keeps by its hash in its place: written aside, then moved there whole. Each write has its own
/// file aside - two threads may be putting the same chunk at once, as two files sharing it do - and one finding it
/// put there meanwhile lets its own go: what is there is the same, and may be read-only already.
fn put_in_place(fs: &dyn Filesystem, path: &HostPath, bytes: &[u8]) -> io::Result<()> {
    static WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let write = WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let aside = format!(".{}.{write}.tmp", std::process::id());
    let aside = Name::new(&aside).map_err(io::Error::other)?;
    let temporary = path.with_suffix(aside);
    fs.write(&temporary, bytes)?;
    match fs.rename(&temporary, path) {
        Ok(()) => {}
        Err(_) if fs.exists(path) => fs.remove_file(&temporary)?,
        Err(e) => {
            let _ = fs.remove_file(&temporary);
            return Err(e);
        }
    }
    written(path);
    Ok(())
}

/// Have on disk what this process wrote into the stores of the project in `root` since it last did: before a
/// record that names any of it is said in the log, so that what the log brings back names nothing a loss of power
/// took (`storage::Storage::say`). What it wrote for another project is that one's to have on disk.
pub fn sync_written(root: &HostPath) {
    let written: Vec<HostPath> = {
        let mut all = WRITTEN.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (ours, others) = std::mem::take(&mut *all).into_iter().partition(|path| path.starts_with(root));
        *all = others;
        ours
    };
    for path in written {
        crate::storage::sync_file(&path);
    }
}
/// The name a manifest adds to its file's.
pub const SUFFIX: &Name = Name::constant(".chunks");
/// The store, among a project's layers.
pub const STORE: &Name = Name::constant(".store");
/// Where the store keeps whole files by their SHA-1 - a manifest of chunks each, whatever their size: the versions an
/// override was made from (`changeset::Base`), what a layer holds of its own.
const FILES: &Name = Name::constant("files");
/// Where it keeps whole objects, and trees of them.
const OBJECTS: &Name = Name::constant("objects");
const TREES: &Name = Name::constant("trees");
const TREE_HEADER: &str = "pe2 tree 1";
const HEADER: &str = "pe2 chunks 1";
const MIN: usize = 16 << 10;
const AVG: usize = 64 << 10;
const MAX: usize = 256 << 10;

/// A large file's content: its length, its hash, and its chunks in order, each with where it starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub size: u64,
    pub sha1: pe2_ext::Sha1,
    pub chunks: Vec<(String, u64)>,
    starts: Vec<u64>,
}

impl Manifest {
    pub(crate) fn new(size: u64, sha1: pe2_ext::Sha1, chunks: Vec<(String, u64)>) -> Manifest {
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
        let sha1 = Sha1::from_hex(&field("sha1 ")?).map_err(|_| bad("sha1"))?;
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
        let mut out = vec![0; usize::try_from(end.saturating_sub(offset)).unwrap_or(0)];
        let read = self.read_range_into(store, offset, &mut out)?;
        out.truncate(read);
        Ok(out)
    }

    /// Bytes from `offset` read into a buffer, from the store, as many as it takes or the content has: how many.
    /// Only the chunks they are in are read.
    ///
    /// # Errors
    /// A chunk is missing from the store, or cannot be read.
    pub fn read_range_into(&self, store: &Store, offset: u64, into: &mut [u8]) -> io::Result<usize> {
        let end = offset.saturating_add(into.len() as u64).min(self.size);
        if offset >= end {
            return Ok(0);
        }
        let mut read = 0;
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
            into[read..read + (to - from)].copy_from_slice(&chunk[from..to]);
            read += to - from;
        }
        Ok(read)
    }
}

/// A file of a tree: a whole object, by its SHA-1 and length, and whether it is run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeFile {
    pub sha1: pe2_ext::Sha1,
    pub size: u64,
    pub executable: bool,
}

/// Files kept as one, by name (`/` between folders).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tree {
    pub files: std::collections::BTreeMap<String, TreeFile>,
}

impl Tree {
    /// A tree's text read.
    ///
    /// # Errors
    /// It is not a tree.
    pub fn parse(text: &str) -> io::Result<Tree> {
        let bad = |why: &str| io::Error::new(io::ErrorKind::InvalidData, format!("a tree: {why}"));
        let mut lines = text.lines();
        if lines.next() != Some(TREE_HEADER) {
            return Err(bad("no header, or one this program does not know"));
        }
        let mut files = std::collections::BTreeMap::new();
        for line in lines.filter(|l| !l.is_empty()) {
            let mut parts = line.splitn(4, ' ');
            let (Some(sha1), Some(size), Some(mode), Some(name)) =
                (parts.next(), parts.next(), parts.next(), parts.next())
            else {
                return Err(bad("a file"));
            };
            let executable = match mode {
                "755" => true,
                "644" => false,
                _ => return Err(bad("a file's mode")),
            };
            let size = size.parse().map_err(|_| bad("a file's length"))?;
            let sha1 = Sha1::from_hex(sha1).map_err(|_| bad("a file's sha1"))?;
            files.insert(name.to_string(), TreeFile { sha1, size, executable });
        }
        Ok(Tree { files })
    }

    #[must_use]
    pub fn render(&self) -> String {
        let mut out = format!("{TREE_HEADER}\n");
        for (name, file) in &self.files {
            let mode = if file.executable { "755" } else { "644" };
            let _ = writeln!(out, "{} {} {mode} {name}", file.sha1, file.size);
        }
        out
    }

    /// Its hash: its text's SHA-1.
    #[must_use]
    pub fn sha1(&self) -> Sha1 {
        crate::extract::sha1_of(self.render().as_bytes())
    }
}

/// A project's chunk store.
#[derive(Clone)]
pub struct Store {
    pub dir: HostPath,
    /// The project's I/O: where its files are, and what tells the time they are kept at.
    pub io: Arc<dyn pe2_io::Io>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the store in {}", self.dir.display())
    }
}

impl Store {
    /// The store of the project in `root`, whose I/O is `io`.
    #[must_use]
    pub fn at(root: &HostPath, io: Arc<dyn pe2_io::Io>) -> Store {
        Store { dir: root.join(&crate::LAYERS).join(&STORE), io }
    }

    fn fs(&self) -> &dyn Filesystem {
        self.io.filesystem()
    }

    /// Have the folder a file of the store goes in.
    fn folder_for(&self, path: &HostPath) -> io::Result<()> {
        self.fs().create_folder_all(&path.parent().unwrap_or_else(|| self.dir.clone()))
    }

    /// Say a file of the store is as new to a sweep as one written now: touched through a handle opened to write
    /// - or, read-only, to read, which its owner may set the times through.
    fn touch(&self, path: &HostPath) {
        let fs = self.fs();
        let opened = fs.open(path, Open { write: true, ..Open::default() }).or_else(|_| fs.open(path, Open::READ));
        let _ = opened.and_then(|file| file.set_modified(self.io.clock().wall()));
    }

    fn file_path(&self, sha1: &Sha1) -> io::Result<HostPath> {
        sharded(&self.dir.join(&FILES), &sha1.to_string())
    }

    /// Keep a file whole, by its SHA-1 (given), its chunks shared with every other file holding them.
    ///
    /// # Errors
    /// It cannot be written.
    pub fn put_file(&self, bytes: &[u8]) -> io::Result<Sha1> {
        let sha1 = crate::extract::sha1_of(bytes);
        let path = self.file_path(&sha1)?;
        if self.fs().exists(&path) {
            // Kept again: as new to a sweep as one written now, its chunks too.
            self.touch(&path);
            let _ = self.put(bytes)?;
            return Ok(sha1);
        }
        let manifest = self.put(bytes)?;
        self.folder_for(&path)?;
        put_in_place(self.fs(), &path, manifest.render().as_bytes())?;
        Ok(sha1)
    }

    /// A file kept whole, by its SHA-1.
    ///
    /// # Errors
    /// It is not kept, or its chunks are not all there.
    pub fn get_file(&self, sha1: &Sha1) -> io::Result<Vec<u8>> {
        let manifest = Manifest::parse(&self.fs().read_to_string(&self.file_path(sha1)?)?)?;
        let len = usize::try_from(manifest.size).map_err(io::Error::other)?;
        manifest.read_range(self, 0, len)
    }

    /// The manifest of a file kept whole, by its SHA-1: for reading it by range.
    ///
    /// # Errors
    /// It is not kept.
    pub fn file_manifest(&self, sha1: &Sha1) -> io::Result<Manifest> {
        Manifest::parse(&self.fs().read_to_string(&self.file_path(sha1)?)?)
    }

    /// Where a whole object is: a file only ever read, never written once kept.
    ///
    /// # Errors
    /// Its hash is no name a file can have.
    pub fn object_path(&self, sha1: &Sha1) -> io::Result<HostPath> {
        sharded(&self.dir.join(&OBJECTS), &sha1.to_string())
    }

    fn tree_path(&self, sha1: &Sha1) -> io::Result<HostPath> {
        sharded(&self.dir.join(&TREES), &sha1.to_string())
    }

    /// Write `bytes` at `path` once: where it is there already, it is touched instead (as new to a sweep as one
    /// written now), as what is named by its hash holds the same.
    fn write_once(&self, path: &HostPath, bytes: &[u8]) -> io::Result<()> {
        if self.fs().exists(path) {
            self.touch(path);
            return Ok(());
        }
        self.folder_for(path)?;
        put_in_place(self.fs(), path, bytes)
    }

    /// Keep bytes as one whole object, by their SHA-1 (given).
    ///
    /// # Errors
    /// It cannot be written.
    pub fn put_object(&self, bytes: &[u8]) -> io::Result<Sha1> {
        let sha1 = crate::extract::sha1_of(bytes);
        let path = self.object_path(&sha1)?;
        self.write_once(&path, bytes)?;
        // Read-only: an object is what its hash says, and what reads it in place must not change it.
        self.fs().set_readonly(&path, true)?;
        Ok(sha1)
    }

    /// A whole object, by its SHA-1.
    ///
    /// # Errors
    /// It is not kept.
    pub fn get_object(&self, sha1: &Sha1) -> io::Result<Vec<u8>> {
        self.fs().read(&self.object_path(sha1)?)
    }

    /// Keep a tree (its objects kept already), by the SHA-1 of its text (given).
    ///
    /// # Errors
    /// It cannot be written.
    pub fn put_tree(&self, tree: &Tree) -> io::Result<Sha1> {
        let text = tree.render();
        let sha1 = crate::extract::sha1_of(text.as_bytes());
        self.write_once(&self.tree_path(&sha1)?, text.as_bytes())?;
        Ok(sha1)
    }

    /// A tree, by its SHA-1.
    ///
    /// # Errors
    /// It is not kept, or is no tree.
    pub fn get_tree(&self, sha1: &Sha1) -> io::Result<Tree> {
        Tree::parse(&self.fs().read_to_string(&self.tree_path(sha1)?)?)
    }

    /// Keep a folder's files as a tree of whole objects, by the tree's SHA-1 (given), with the tree.
    ///
    /// # Errors
    /// A file cannot be read, or kept.
    pub fn put_folder(&self, folder: &HostPath) -> io::Result<(pe2_ext::Sha1, Tree)> {
        fn walk(fs: &dyn Filesystem, dir: &HostPath, at: &str, out: &mut Vec<(String, HostPath)>) -> io::Result<()> {
            for entry in fs.read_directory(dir)? {
                let name = entry.name.to_string_lossy().into_owned();
                let path = if at.is_empty() { name } else { format!("{at}/{name}") };
                if entry.kind == pe2_io::Kind::Folder {
                    walk(fs, &dir.entry(&entry.name), &path, out)?;
                } else {
                    out.push((path, dir.entry(&entry.name)));
                }
            }
            Ok(())
        }
        let fs = self.fs();
        let mut found = Vec::new();
        walk(fs, folder, "", &mut found)?;
        let mut tree = Tree::default();
        for (name, path) in found {
            let bytes = fs.read(&path)?;
            let executable = fs.stat(&path)?.executable;
            let sha1 = self.put_object(&bytes)?;
            // A program is run where it is kept: its object says so, whatever else holds the same bytes.
            if executable {
                fs.set_executable(&self.object_path(&sha1)?, true)?;
            }
            tree.files.insert(name, TreeFile { sha1, size: bytes.len() as u64, executable });
        }
        Ok((self.put_tree(&tree)?, tree))
    }

    /// Every whole object and tree kept, by SHA-1, with where.
    fn kept_in(&self, kind: &Name) -> Vec<(Sha1, HostPath)> {
        let fs = self.fs();
        let dir = self.dir.join(kind);
        let shards = fs.read_directory(&dir).unwrap_or_default();
        shards
            .iter()
            .flat_map(|shard| {
                let shard = dir.entry(&shard.name);
                let files = fs.read_directory(&shard).unwrap_or_default();
                files.into_iter().map(move |file| (shard.clone(), file))
            })
            .filter_map(|(shard, file)| {
                // What is named by no hash is none of the store's own, and left alone.
                let name = file.name.to_str().filter(|n| Path::new(n).extension().is_none())?.to_string();
                Some((Sha1::from_hex(&name).ok()?, shard.entry(&file.name)))
            })
            .collect()
    }

    /// Whether a file is kept, by its SHA-1.
    #[must_use]
    pub fn has_file(&self, sha1: &Sha1) -> bool {
        self.file_path(sha1).is_ok_and(|path| self.fs().is_file(&path))
    }

    /// Every file kept whole, by SHA-1, with its manifest's text.
    fn files(&self) -> Vec<(Sha1, HostPath)> {
        self.kept_in(FILES)
    }

    /// The store of the project whose layers hold a path: none for a path in no project's `.layers`.
    #[must_use]
    pub fn of(path: &HostPath, io: &Arc<dyn pe2_io::Io>) -> Option<Store> {
        let layers = path.ancestors().find(|a| a.file_name().is_some_and(|n| n == crate::LAYERS))?;
        Some(Store { dir: layers.join(&STORE), io: io.clone() })
    }

    fn path(&self, hash: &str) -> io::Result<HostPath> {
        sharded(&self.dir, hash)
    }

    /// A chunk.
    ///
    /// # Errors
    /// It is not in the store.
    pub fn get(&self, hash: &str) -> io::Result<Vec<u8>> {
        self.fs().read(&self.path(hash)?)
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
            let path = self.path(&hash)?;
            if self.fs().exists(&path) {
                // Stored again: as new to a sweep as one written now (`sweep_settled`).
                self.touch(&path);
            } else {
                self.folder_for(&path)?;
                put_in_place(self.fs(), &path, chunk)?;
            }
            chunks.push((hash, cut.length as u64));
        }
        Ok(Manifest::new(bytes.len() as u64, crate::extract::sha1_of(bytes), chunks))
    }

    /// Every chunk it holds, by hash.
    #[must_use]
    pub fn hashes(&self) -> BTreeSet<String> {
        // Chunks are sharded by two characters; files, objects and trees are elsewhere.
        let fs = self.fs();
        let shards = fs.read_directory(&self.dir).unwrap_or_default();
        shards
            .iter()
            .filter(|shard| shard.name.len() == 2)
            .flat_map(|shard| fs.read_directory(&self.dir.entry(&shard.name)).unwrap_or_default())
            .filter_map(|chunk| chunk.name.to_str().filter(|n| Path::new(n).extension().is_none()).map(str::to_string))
            .collect()
    }

    /// Remove chunks. Returns how many bytes it freed.
    #[must_use = "how much was freed"]
    pub fn remove(&self, hashes: &BTreeSet<String>) -> u64 {
        let mut freed = 0;
        for hash in hashes {
            let Ok(path) = self.path(hash) else { continue };
            freed += self.fs().stat(&path).map_or(0, |m| m.size);
            let _ = self.fs().remove_file(&path);
        }
        freed
    }
}

/// A file's manifest: its path with `.chunks` added.
#[must_use]
pub fn manifest_path(path: &HostPath) -> HostPath {
    path.with_suffix(SUFFIX)
}

/// The file a manifest stands for: its path without `.chunks`; none for a path that is no manifest.
#[must_use]
pub fn logical(path: &HostPath) -> Option<HostPath> {
    path.without_suffix(SUFFIX)
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
fn files_under(fs: &dyn Filesystem, dir: &HostPath, out: &mut Vec<HostPath>) {
    for entry in fs.read_directory(dir).unwrap_or_default() {
        let path = dir.entry(&entry.name);
        match entry.kind {
            pe2_io::Kind::Folder if entry.name != *STORE => files_under(fs, &path, out),
            pe2_io::Kind::File => out.push(path),
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
    let fs = project.io.filesystem();
    let mut pass = Pass::default();
    let layers: Vec<&HostPath> = project.layers.iter().filter(|l| !l.mounted).map(|l| &l.path).collect();
    let in_layers = |path: &HostPath| layers.iter().any(|layer| path.starts_with(layer));
    let mut found: BTreeSet<HostPath> = BTreeSet::new();
    for entry in index.entries.values().filter(|e| e.kind == crate::index::Kind::File) {
        found.insert(entry.disk.clone());
    }
    for asset in index.assets.values() {
        found.extend(asset.on_disk().files.iter().filter(|f| f.made_by.is_none()).map(|f| f.path.clone()));
    }
    for path in found.into_iter().filter(|p| in_layers(p) && fs.is_file(p)) {
        let len = fs.stat(&path)?.size;
        if usize::try_from(len).unwrap_or(usize::MAX) < THRESHOLD {
            continue;
        }
        let Some(store) = Store::of(&path, &project.io) else { continue };
        let bytes = fs.read(&path)?;
        let manifest = store.put(&bytes)?.render();
        crate::cache::write_atomic(fs, &manifest_path(&path), manifest.as_bytes()).map_err(io::Error::other)?;
        fs.remove_file(&path)?;
        pass.files += 1;
        pass.before += len;
        pass.after += manifest.len() as u64;
    }
    let store = Store::at(&project.root, project.io.clone());
    let held: u64 =
        store.hashes().iter().filter_map(|h| store.path(h).ok()).map(|path| fs.stat(&path).map_or(0, |m| m.size)).sum();
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
const SWEPT: &Name = Name::constant("chunks.swept");

/// Sweep the store where it was not swept for `SWEEP_EVERY`, of chunks nothing names and stored longer ago than
/// `SETTLED`, while the project is written: how many went and the bytes they held, none where it was not due.
#[must_use = "what was swept is the sweep's report"]
pub fn sweep_if_due(project: &crate::Project) -> Option<io::Result<(usize, u64)>> {
    let fs = project.io.filesystem();
    let marker = project.root.join(&crate::cache::DIR).join(&SWEPT);
    let now = project.io.clock().wall();
    let age = fs.stat(&marker).ok().and_then(|m| m.modified).and_then(|t| now.duration_since(t).ok());
    if age.is_some_and(|age| age < SWEEP_EVERY) {
        return None;
    }
    // Written, then dated by the project's clock: what is due is told by it.
    let _ = fs.write(&marker, b"");
    let _ = fs.open(&marker, Open { write: true, ..Open::default() }).and_then(|file| file.set_modified(now));
    Some(sweep_settled(project, SETTLED))
}

/// Remove the chunks nothing names, as `sweep` does, of those stored longer ago than `settled`: what a write stores just
/// before its manifest names it is left to it.
///
/// # Errors
/// The store cannot be read.
pub fn sweep_settled(project: &crate::Project, settled: Duration) -> io::Result<(usize, u64)> {
    let fs = project.io.filesystem();
    let mut named = BTreeSet::new();
    let mut found = Vec::new();
    for layer in project.layers.iter().filter(|l| !l.mounted) {
        files_under(fs, &layer.path, &mut found);
    }
    files_under(fs, &project.root.join(&crate::cache::DIR).join(&crate::history::DIR), &mut found);
    // The changesets name the files kept whole that overrides were made from; the history, those its steps can put back.
    found.extend(project.layers.iter().map(|l| l.changeset.clone()));
    let mut kept = BTreeSet::new();
    for path in found {
        let is_manifest = logical(&path).is_some();
        let small = fs.stat(&path).is_ok_and(|m| m.size < (THRESHOLD as u64));
        if (is_manifest || small)
            && let Ok(text) = fs.read(&path)
        {
            named.extend(named_by(&text));
            kept.extend(sha1s_in(&text));
        }
    }
    // A packed layer's listing names its chunks too.
    for layer in project.layers.iter().filter(|l| fs.is_file(&l.path.join(&crate::pack::FILE))) {
        named.extend(crate::pack::Pack::open(&layer.path, &project.io)?.chunks());
    }
    let store = Store::at(&project.root, project.io.clone());
    let now = project.io.clock().wall();
    let old = |path: &HostPath| {
        let modified = fs.stat(path).ok().and_then(|m| m.modified);
        settled.is_zero() || modified.and_then(|t| now.duration_since(t).ok()).is_some_and(|age| age >= settled)
    };
    // A tree named keeps its objects; one nothing names goes, and so does an object neither a tree nor a text names.
    let (mut gone_files, mut freed) = (0, 0);
    for (sha1, path) in store.kept_in(TREES) {
        if kept.contains(&sha1) || !old(&path) {
            let tree = fs.read_to_string(&path).ok().and_then(|t| Tree::parse(&t).ok()).unwrap_or_default();
            kept.extend(tree.files.into_values().map(|file| file.sha1));
        } else {
            freed += fs.stat(&path).map_or(0, |m| m.size);
            gone_files += 1;
            let _ = fs.remove_file(&path);
        }
    }
    for (sha1, path) in store.kept_in(OBJECTS) {
        if !kept.contains(&sha1) && old(&path) {
            freed += fs.stat(&path).map_or(0, |m| m.size);
            gone_files += 1;
            // Read-only as it is: Windows refuses to remove a read-only file.
            let _ = fs.set_readonly(&path, false);
            let _ = fs.remove_file(&path);
        }
    }
    // A file kept whole that nothing names goes; one named keeps its chunks.
    for (sha1, path) in store.files() {
        let text = fs.read(&path).unwrap_or_default();
        if kept.contains(&sha1) || !old(&path) {
            named.extend(named_by(&text));
        } else {
            freed += text.len() as u64;
            gone_files += 1;
            let _ = fs.remove_file(&path);
        }
    }
    let mut gone = unreferenced(&store, &named);
    gone.retain(|hash| store.path(hash).is_ok_and(|path| old(&path)));
    freed += store.remove(&gone);
    Ok((gone.len() + gone_files, freed))
}

/// What may be a file's SHA-1 in a text: every run of forty hexadecimal digits.
fn sha1s_in(text: &[u8]) -> Vec<Sha1> {
    let runs = text.split(|b| !b.is_ascii_hexdigit()).filter(|run| run.len() == 40);
    runs.filter_map(|run| Sha1::from_hex(std::str::from_utf8(run).ok()?).ok()).collect()
}

#[cfg(test)]
#[allow(
    clippy::disallowed_methods,
    reason = "a test's fixtures: the machine's time, places and environment, which nothing tested reads"
)]
mod tests {
    use super::{Manifest, Store};
    use crate::testing::At as _;

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
        let dir = crate::testing::scratch("chunks");
        let store = Store { dir: dir.clone(), io: crate::testing::io() };
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
    fn threads_putting_one_file_at_once_each_have_it_kept() {
        let dir = crate::testing::scratch("chunks-at-once");
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store { dir: dir.clone(), io: crate::testing::io() };
        let bytes = content(1 << 20, 11);
        std::thread::scope(|scope| {
            let putting: Vec<_> = (0..16)
                .map(|_| scope.spawn(|| (store.put_file(&bytes).map(drop), store.put_object(&bytes).map(drop))))
                .collect();
            for put in putting {
                let (file, object) = put.join().unwrap();
                file.expect("kept as chunks");
                object.expect("kept whole");
            }
        });
        let sha1 = crate::extract::sha1_of(&bytes);
        assert_eq!(store.get_file(&sha1).unwrap(), bytes);
        assert_eq!(store.get_object(&sha1).unwrap(), bytes);
        let left = walkdir(&dir)
            .into_iter()
            .filter(|path| std::path::Path::new(path).extension().is_some_and(|e| e == "tmp"))
            .count();
        assert_eq!(left, 0, "nothing written aside stays");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every file in a folder, as text.
    fn walkdir(dir: &std::path::Path) -> Vec<String> {
        let mut found = Vec::new();
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                found.extend(walkdir(&path));
            } else {
                found.push(path.display().to_string());
            }
        }
        found
    }

    #[test]
    fn a_tree_keeps_its_objects_from_a_sweep_while_something_names_it() {
        let root = crate::testing::scratch("chunks-tree");
        let _ = std::fs::remove_dir_all(&root);
        crate::create(&root).unwrap();
        let project = crate::Project::load(&root).unwrap();
        let store = Store::at(&root, crate::testing::io());
        let folder = root.at("x.extension");
        std::fs::create_dir_all(folder.at("docs")).unwrap();
        std::fs::write(folder.at("extension.wasm"), content(300_000, 4)).unwrap();
        std::fs::write(folder.at("docs/a b.md"), "# A\n").unwrap();
        let (sha1, tree) = store.put_folder(&folder).unwrap();
        assert_eq!(super::Tree::parse(&tree.render()).unwrap(), tree);
        assert_eq!(store.get_tree(&sha1).unwrap(), tree);
        assert_eq!(store.get_object(&tree.files["docs/a b.md"].sha1).unwrap(), b"# A\n");
        // Kept again: the same hash, nothing new.
        assert_eq!(store.put_folder(&folder).unwrap().0, sha1);
        // Named in a layer's text, the tree and its objects stay; named by nothing, they go.
        let named = root.join(&crate::LAYERS).at("project").at("named.toml");
        std::fs::write(&named, format!("tree = \"{sha1}\"\n")).unwrap();
        assert_eq!(super::sweep(&project).unwrap().0, 0);
        std::fs::remove_file(&named).unwrap();
        assert_eq!(super::sweep(&project).unwrap().0, 3);
        assert!(store.get_tree(&sha1).is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_sweep_while_writing_leaves_what_was_stored_lately() {
        use std::time::{Duration, SystemTime};
        let root = crate::testing::scratch("chunks-sweep");
        let _ = std::fs::remove_dir_all(&root);
        crate::create(&root).unwrap();
        let project = crate::Project::load(&root).unwrap();
        let store = Store { dir: root.join(&crate::LAYERS).join(&super::STORE), io: crate::testing::io() };
        let (old, fresh, again) = (content(200_000, 1), content(200_000, 2), content(200_000, 3));
        let age = |m: &Manifest, by: Duration| {
            for (hash, _) in &m.chunks {
                let file = std::fs::File::options().write(true).open(store.path(hash).unwrap()).unwrap();
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
