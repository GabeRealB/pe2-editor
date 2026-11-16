//! The project's store: content kept by hash - a file as a manifest naming its chunks (`store/<aa>/<hash>`), where each
//! chunk is kept once, by its BLAKE3 hash. Chunks are cut where the content says (`FastCDC`, about 64 KiB each), so an
//! edit to a large file stores only the chunks around it, and what layers, releases and the history hold alike is held
//! once. The project's store is shipped with it (`store/`); what only this machine's records name is in its local one
//! (`.local/store/`, `Store::local`).
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
//! It is the only copy of what it names: the store is not cache. What no record of any layer, nor the history, names
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

/// What names a file kept in the store - a record, a changeset, a step's content - is shorter than this: a sweep reads
/// nothing longer for the hashes it names.
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
pub fn sync_written(fs: &dyn Filesystem, root: &HostPath) {
    let written: Vec<HostPath> = {
        let mut all = WRITTEN.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (ours, others) = std::mem::take(&mut *all).into_iter().partition(|path| path.starts_with(root));
        *all = others;
        ours
    };
    for path in written {
        pe2_io::sync_file(fs, &path);
    }
}
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
        Store { dir: root.join(&crate::STORE), io }
    }

    /// The store of what only this machine's records name, of the project in `root` (`.local/store`): what a plan
    /// makes of the discs, which is never shipped.
    #[must_use]
    pub fn local(root: &HostPath, io: Arc<dyn pe2_io::Io>) -> Store {
        Store { dir: crate::local::part(root, crate::local::STORE), io }
    }

    fn fs(&self) -> &dyn Filesystem {
        self.io.filesystem()
    }

    /// The project's other store: of its shipped one, this machine's (`.local/store`); of that, the shipped one. What
    /// one store's records name may be kept in the other a while - what an undo names again, until a sweep shares it -
    /// and is read there. None of a store that is neither.
    #[must_use]
    pub fn other(&self) -> Option<Store> {
        let parent = self.dir.parent()?;
        if self.dir.file_name().is_none_or(|name| name != crate::STORE.as_str()) {
            return None;
        }
        if parent.file_name().is_some_and(|name| name == crate::local::DIR.as_str()) {
            Some(Store::at(&parent.parent()?, self.io.clone()))
        } else {
            Some(Store::local(&parent, self.io.clone()))
        }
    }

    /// Of this store and the other, the first that keeps a file whole by its SHA-1.
    #[must_use]
    pub fn holding_file(&self, sha1: &Sha1) -> Option<Store> {
        std::iter::once(self.clone()).chain(self.other()).find(|store| store.has_file(sha1))
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
        // Kept here, or - for a while, or a base - in the other store.
        let held = |store: &Store| -> io::Result<Vec<u8>> {
            let manifest = store.file_manifest(sha1)?;
            let len = usize::try_from(manifest.size).map_err(io::Error::other)?;
            manifest.read_range(store, 0, len)
        };
        held(self).or_else(|e| match self.other().filter(|other| other.has_file(sha1)) {
            Some(other) => held(&other),
            None => Err(e),
        })
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

    /// Where a whole object is kept: in this store, else - for a while - in the other.
    ///
    /// # Errors
    /// Neither keeps it.
    pub fn held_object(&self, sha1: &Sha1) -> io::Result<HostPath> {
        let own = self.object_path(sha1)?;
        if self.fs().is_file(&own) {
            return Ok(own);
        }
        match self.other().map(|other| other.object_path(sha1)).transpose()? {
            Some(other) if self.fs().is_file(&other) => Ok(other),
            _ => Err(io::Error::new(io::ErrorKind::NotFound, format!("{}: no such object", own.display()))),
        }
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
        // Kept here, or - for a while - in the other store.
        self.fs().read(&self.object_path(sha1)?).or_else(|e| match self.other() {
            Some(other) => other.fs().read(&other.object_path(sha1)?).map_err(|_| e),
            None => Err(e),
        })
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

    /// Of this store and the other, the first that keeps a tree, with it.
    #[must_use]
    pub fn holding_tree(&self, sha1: &Sha1) -> Option<(Store, Tree)> {
        let found = std::iter::once(self.clone()).chain(self.other());
        found.map(|store| store.get_tree(sha1).map(|tree| (store, tree))).find_map(Result::ok)
    }

    /// Have a file kept whole in another store as well: its chunks, then its manifest naming them.
    fn copy_file(&self, sha1: &Sha1, to: &Store) -> io::Result<()> {
        let text = self.fs().read(&self.file_path(sha1)?)?;
        let manifest = Manifest::parse(std::str::from_utf8(&text).map_err(io::Error::other)?)?;
        for (hash, _) in &manifest.chunks {
            to.write_once(&to.path(hash)?, &self.get(hash)?)?;
        }
        to.write_once(&to.file_path(sha1)?, &text)
    }

    /// Have a tree in another store as well: its objects, then the tree naming them.
    fn copy_tree(&self, sha1: &Sha1, to: &Store) -> io::Result<()> {
        let text = self.fs().read(&self.tree_path(sha1)?)?;
        let tree = Tree::parse(std::str::from_utf8(&text).map_err(io::Error::other)?)?;
        for file in tree.files.values() {
            let copied = to.put_object(&self.get_object(&file.sha1)?)?;
            if file.executable {
                to.fs().set_executable(&to.object_path(&copied)?, true)?;
            }
        }
        to.write_once(&to.tree_path(sha1)?, &text)
    }

    /// Whether a tree is kept, by its SHA-1.
    fn has_tree(&self, sha1: &Sha1) -> bool {
        self.tree_path(sha1).is_ok_and(|path| self.fs().is_file(&path))
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

    /// The store of the project whose layers hold a path: none for a path in the layers of no project this process
    /// opened (`crate::holding`).
    #[must_use]
    pub fn of(path: &HostPath, io: &Arc<dyn pe2_io::Io>) -> Option<Store> {
        crate::holding(path).map(|root| Store::at(&root, io.clone()))
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

/// The chunks no manifest names: of the store, those absent from `named`.
#[must_use]
pub fn unreferenced(store: &Store, named: &BTreeSet<String>) -> BTreeSet<String> {
    store.hashes().difference(named).cloned().collect()
}

/// Every file of a folder, recursively, its hidden ones too.
fn files_under(fs: &dyn Filesystem, dir: &HostPath, out: &mut Vec<HostPath>) {
    for entry in fs.read_directory(dir).unwrap_or_default() {
        let path = dir.entry(&entry.name);
        match entry.kind {
            pe2_io::Kind::Folder => files_under(fs, &path, out),
            pe2_io::Kind::File => out.push(path),
            _ => {}
        }
    }
}

/// The chunks a manifest's text names; none where the text is no manifest.
fn named_by(text: &[u8]) -> Vec<String> {
    let manifest = std::str::from_utf8(text).ok().and_then(|t| Manifest::parse(t).ok());
    manifest.map_or_else(Vec::new, |m| m.chunks.into_iter().map(|(h, _)| h).collect())
}

/// Remove the chunks nothing names: no manifest of any layer, nor any content the history or the conflicts keep (a
/// manifest a step can put back). Returns how many went, and the bytes they held. Only where nothing writes the
/// project meanwhile (under its lock): a write stores its chunks before the manifest naming them.
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
/// When it was swept last, in the outputs.
const SWEPT: &Name = Name::constant("chunks.swept");

/// Sweep the store where it was not swept for `SWEEP_EVERY`, of chunks nothing names and stored longer ago than
/// `SETTLED`, while the project is written: how many went and the bytes they held, none where it was not due.
#[must_use = "what was swept is the sweep's report"]
pub fn sweep_if_due(project: &crate::Project) -> Option<io::Result<(usize, u64)>> {
    let fs = project.io.filesystem();
    let marker = crate::local::part(&project.root, crate::local::OUTPUTS).join(&SWEPT);
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

/// What names the content the stores keep, by whose it is: the shipped store's, what the shipped records name; this
/// machine's, what only it has. Read from what names it, parsed - never guessed at from text.
#[derive(Default)]
struct Named {
    shipped: Refs,
    /// This machine's records (a generated layer's), every record's bases - the versions below overrides were made
    /// from - what a plan's last run made, and what the history and the conflicts kept aside could put back.
    local: Refs,
}

/// Files kept whole and trees, by SHA-1.
#[derive(Default)]
struct Refs {
    files: BTreeSet<Sha1>,
    trees: BTreeSet<Sha1>,
}

impl Named {
    /// What a record names: its files' contents and its tree, as `local` says whose; their bases, this machine's.
    fn record(&mut self, local: bool, record: &crate::records::Record) {
        if let Some(tree) = record.tree {
            if local { &mut self.local } else { &mut self.shipped }.trees.insert(tree);
        }
        for file in record.files.values() {
            if let Some(crate::records::Content::Stored { sha1, .. }) = &file.content {
                if local { &mut self.local } else { &mut self.shipped }.files.insert(*sha1);
            }
            self.local.files.extend(file.base);
        }
    }

    /// What a text the history or the conflicts keep names, where it is a record: this machine's. Any other text, or
    /// bytes, names nothing.
    fn kept(&mut self, bytes: &[u8]) {
        let Ok(text) = std::str::from_utf8(bytes) else { return };
        if let Ok(record) = crate::records::Record::parse(text) {
            self.record(true, &record);
        }
    }
}

/// What names the content of a project's stores (`Named`).
///
/// # Errors
/// A layer's records, or a plan's record, cannot be read: what they name is not known, and nothing may go.
fn named(project: &crate::Project) -> anyhow::Result<Named> {
    let fs = project.io.filesystem();
    let local_layers = crate::local::part(&project.root, crate::local::LAYERS);
    let mut named = Named::default();
    // A mount's records name no content: its files are its tree.
    for layer in project.layers.iter().filter(|layer| !layer.mounted) {
        let local = layer.records.starts_with(&local_layers);
        for held in crate::records::read_records(fs, &layer.records, None)?.values() {
            named.record(local, &held.record);
        }
        if layer.kind == crate::LayerKind::Generated {
            named.local.files.extend(crate::plan::made(fs, layer)?);
        }
    }
    let mut kept = Vec::new();
    for part in [crate::local::HISTORY, crate::local::CONFLICTS] {
        files_under(fs, &crate::local::part(&project.root, part), &mut kept);
    }
    for path in kept {
        // A record is small; what is larger is a file's content, which names nothing.
        if fs.stat(&path).is_ok_and(|m| m.size < (THRESHOLD as u64))
            && let Ok(bytes) = fs.read(&path)
        {
            named.kept(&bytes);
        }
    }
    Ok(named)
}

/// Remove what nothing names, as `sweep` does, of what was stored longer ago than `settled`: what a write stores just
/// before what names it is written is left to it. What each store holds is the one's whose records name it - what a
/// shipped record names that only this machine's store holds is shared again; what the shipped store holds that only
/// this machine names moves to its store - each copied whole before anything of it is removed, so that a sweep cut
/// short loses nothing still named.
///
/// # Errors
/// What names the stores' content cannot be read (nothing is removed then), or something cannot be copied.
pub fn sweep_settled(project: &crate::Project, settled: Duration) -> io::Result<(usize, u64)> {
    let named = named(project).map_err(|e| io::Error::other(format!("{e:#}")))?;
    let fs = project.io.filesystem();
    let shipped = Store::at(&project.root, project.io.clone());
    let local = Store::local(&project.root, project.io.clone());
    let now = project.io.clock().wall();
    let old = |path: &HostPath| {
        let modified = fs.stat(path).ok().and_then(|m| m.modified);
        settled.is_zero() || modified.and_then(|t| now.duration_since(t).ok()).is_some_and(|age| age >= settled)
    };
    // What the shipped records name is shipped: shared again where only this machine's store has it.
    for sha1 in &named.shipped.files {
        if !shipped.has_file(sha1) && local.has_file(sha1) {
            local.copy_file(sha1, &shipped)?;
        }
    }
    for sha1 in &named.shipped.trees {
        if !shipped.has_tree(sha1) && local.has_tree(sha1) {
            local.copy_tree(sha1, &shipped)?;
        }
    }
    // Removed, read-only as an object is (Windows refuses to remove a read-only file): the bytes it held.
    let remove = |path: &HostPath| {
        let size = fs.stat(path).map_or(0, |m| m.size);
        let _ = fs.set_readonly(path, false);
        fs.remove_file(path).map_or(0, |()| size)
    };
    let (mut gone, mut freed) = (0, 0);
    // The shipped store's first: what only this machine names moves to its store, which then keeps what it names.
    for (store, own, moved_to) in [(&shipped, &named.shipped, Some(&local)), (&local, &named.local, None)] {
        for (sha1, path) in store.files() {
            if own.files.contains(&sha1) || !old(&path) {
                continue;
            }
            if let Some(to) = moved_to.filter(|_| named.local.files.contains(&sha1)) {
                store.copy_file(&sha1, to)?;
            }
            (gone, freed) = (gone + 1, freed + remove(&path));
        }
        for (sha1, path) in store.kept_in(TREES) {
            if own.trees.contains(&sha1) || !old(&path) {
                continue;
            }
            if let Some(to) = moved_to.filter(|_| named.local.trees.contains(&sha1)) {
                store.copy_tree(&sha1, to)?;
            }
            (gone, freed) = (gone + 1, freed + remove(&path));
        }
        // An object is kept by a tree the store keeps; a chunk, by a file it keeps.
        let mut objects = BTreeSet::new();
        for (_, path) in store.kept_in(TREES) {
            objects.extend(Tree::parse(&fs.read_to_string(&path)?)?.files.into_values().map(|file| file.sha1));
        }
        for (sha1, path) in store.kept_in(OBJECTS) {
            if !objects.contains(&sha1) && old(&path) {
                (gone, freed) = (gone + 1, freed + remove(&path));
            }
        }
        let mut chunks = BTreeSet::new();
        for (_, path) in store.files() {
            chunks.extend(named_by(&fs.read(&path)?));
        }
        let mut unnamed = unreferenced(store, &chunks);
        unnamed.retain(|hash| store.path(hash).is_ok_and(|path| old(&path)));
        gone += unnamed.len();
        freed += store.remove(&unnamed);
    }
    Ok((gone, freed))
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
        crate::create(&root, &pe2_io::SystemFilesystem).unwrap();
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
        // Named by a layer's sealed node, the tree and its objects stay; named by nothing, they go.
        let record = crate::records::Record {
            op: crate::records::Op::Create,
            kind: crate::records::Kind::Node,
            parent: None,
            name: Some(pe2_path::Name::constant("x").to_owned()),
            files: std::collections::BTreeMap::new(),
            tree: Some(sha1),
        };
        let layer = root.join(&crate::LAYERS).at("project");
        let named = crate::records::path_of(&layer, crate::testing::uid("uid://e1")).unwrap();
        std::fs::create_dir_all(named.parent().unwrap()).unwrap();
        std::fs::write(&named, record.render()).unwrap();
        assert_eq!(super::sweep(&project).unwrap().0, 0);
        std::fs::remove_file(&named).unwrap();
        assert_eq!(super::sweep(&project).unwrap().0, 3);
        assert!(store.get_tree(&sha1).is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A project of one layer of its own (`project`), with its stores: the shipped one and this machine's.
    fn stored(name: &str) -> (sfs::HostPath, Store, Store) {
        let root = crate::testing::scratch(name);
        let _ = std::fs::remove_dir_all(&root);
        crate::create(&root, &pe2_io::SystemFilesystem).unwrap();
        (root.clone(), Store::at(&root, crate::testing::io()), Store::local(&root, crate::testing::io()))
    }

    /// Have the layer `project` hold a record: a file of its own, its content kept by hash, made from `base`. Where
    /// its record is.
    fn held(root: &sfs::HostPath, uid: &str, sha1: pe2_ext::Sha1, base: Option<pe2_ext::Sha1>) -> sfs::HostPath {
        use crate::records::{Content, File, Kind, Op, Record};
        let root_file: std::sync::Arc<pe2_path::RelPath> = pe2_path::RelPath::root().to_owned();
        let file = File { content: Some(Content::Stored { sha1, size: 1 }), base };
        let op = if base.is_some() { Op::Override } else { Op::Create };
        let name = base.is_none().then(|| pe2_path::Name::new(uid.trim_start_matches("uid://")).unwrap().to_owned());
        let record = Record { op, kind: Kind::File, parent: None, name, files: [(root_file, file)].into(), tree: None };
        let layer = root.join(&crate::LAYERS).at("project");
        let path = crate::records::path_of(&layer, crate::testing::uid(uid)).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, record.render()).unwrap();
        path
    }

    /// Have the history keep a text, as it keeps what a step overwrote.
    fn kept_by_history(root: &sfs::HostPath, text: &str) {
        let blobs = crate::local::part(root, crate::local::HISTORY).at("blobs");
        std::fs::create_dir_all(&blobs).unwrap();
        std::fs::write(blobs.at(&crate::extract::sha1_of(text.as_bytes()).to_string()), text).unwrap();
    }

    #[test]
    fn what_only_this_machine_names_moves_to_its_store_and_is_shared_again_once_a_record_names_it() {
        let (root, shipped, local) = stored("chunks-moved");
        let project = crate::Project::load(&root).unwrap();
        let bytes = content(5000, 7);
        let sha1 = shipped.put_file(&bytes).unwrap();
        let record = held(&root, "uid://a1", sha1, None);
        assert_eq!(super::sweep(&project).unwrap().0, 0, "a record names it");
        // Its record removed by a step, which the history can put back: named by this machine alone.
        kept_by_history(&root, &std::fs::read_to_string(&record).unwrap());
        let text = std::fs::read_to_string(&record).unwrap();
        std::fs::remove_file(&record).unwrap();
        super::sweep(&project).unwrap();
        assert!(!shipped.has_file(&sha1) && local.has_file(&sha1), "moved to this machine's store");
        // Put back - an undo: read where it is, and shared again by the next sweep.
        std::fs::write(&record, text).unwrap();
        assert_eq!(shipped.get_file(&sha1).unwrap(), bytes);
        super::sweep(&project).unwrap();
        assert!(shipped.has_file(&sha1), "shared again");
        assert_eq!(shipped.get_file(&sha1).unwrap(), bytes);
        crate::local::remove_all(&root);
    }

    #[test]
    fn a_base_only_a_shipped_record_names_is_kept_in_this_machines_store() {
        let (root, shipped, local) = stored("chunks-base");
        let project = crate::Project::load(&root).unwrap();
        let (mine, base) = (content(3000, 8), content(3000, 9));
        let (mine, base) = (shipped.put_file(&mine).unwrap(), shipped.put_file(&base).unwrap());
        held(&root, "uid://b2", mine, Some(base));
        super::sweep(&project).unwrap();
        assert!(shipped.has_file(&mine) && !shipped.has_file(&base) && local.has_file(&base));
        assert_eq!(shipped.get_file(&base).unwrap(), content(3000, 9), "read where it is");
        crate::local::remove_all(&root);
    }

    #[test]
    fn a_record_not_read_leaves_everything() {
        let (root, shipped, local) = stored("chunks-unread");
        let project = crate::Project::load(&root).unwrap();
        let (one, two) = (shipped.put_file(&content(100, 1)).unwrap(), local.put_file(&content(100, 2)).unwrap());
        let record = held(&root, "uid://c3", one, None);
        std::fs::write(&record, "format = 1\nop = \"overturn\"\n").unwrap();
        assert!(super::sweep(&project).is_err());
        assert!(shipped.has_file(&one) && local.has_file(&two), "nothing went");
        crate::local::remove_all(&root);
    }

    #[test]
    fn a_sweep_cut_short_at_any_change_loses_nothing_still_named() {
        // A shipped record's content only this machine's store has; one only the history names, in the shipped store;
        // a base; and what nothing names, in both.
        let make = |name: &str| {
            let (root, shipped, local) = stored(name);
            let shared = local.put_file(&content(4000, 1)).unwrap();
            held(&root, "uid://d1", shared, None);
            let moved = shipped.put_file(&content(4000, 2)).unwrap();
            let gone = held(&root, "uid://d2", moved, None);
            kept_by_history(&root, &std::fs::read_to_string(&gone).unwrap());
            std::fs::remove_file(&gone).unwrap();
            let (mine, base) =
                (shipped.put_file(&content(4000, 3)).unwrap(), shipped.put_file(&content(4000, 4)).unwrap());
            held(&root, "uid://d3", mine, Some(base));
            shipped.put_file(&content(4000, 5)).unwrap();
            local.put_file(&content(4000, 6)).unwrap();
            (root, [(shared, 1), (moved, 2), (mine, 3), (base, 4)])
        };
        let faulty = |root: &sfs::HostPath| {
            let fs = pe2_io::Faulty::new(std::sync::Arc::new(pe2_io::SystemFilesystem));
            let io = pe2_io::Composed { filesystem: std::sync::Arc::new(fs.clone()), ..pe2_io::Composed::system() };
            (crate::Project::load_with(root.as_path(), std::sync::Arc::new(io)).unwrap(), fs)
        };
        let readable = |root: &sfs::HostPath, named: &[(pe2_ext::Sha1, u32)]| {
            let store = Store::at(root, crate::testing::io());
            named.iter().all(|(sha1, seed)| store.get_file(sha1).is_ok_and(|bytes| bytes == content(4000, *seed)))
        };
        let (root, named) = make("chunks-cut-counted");
        let (project, fs) = faulty(&root);
        let counted = fs.changes();
        super::sweep(&project).unwrap();
        let changes = fs.changes() - counted;
        assert!(changes > 8 && readable(&root, &named), "{changes} changes");
        crate::local::remove_all(&root);
        for how in [pe2_io::Fault::Once, pe2_io::Fault::From] {
            for nth in 1..=changes + 1 {
                let (root, named) = make(&format!("chunks-cut-{how:?}-{nth}"));
                let (project, fs) = faulty(&root);
                fs.fail(nth, how);
                let swept = super::sweep(&project);
                fs.heal();
                assert!(readable(&root, &named), "{how:?} at change {nth} ({swept:?}): something named is lost");
                // Swept again, whole: as one not cut short leaves it.
                super::sweep(&crate::Project::load(&root).unwrap()).unwrap();
                assert!(readable(&root, &named), "{how:?} at change {nth}, swept again");
                crate::local::remove_all(&root);
            }
        }
    }

    #[test]
    fn a_sweep_while_writing_leaves_what_was_stored_lately() {
        use std::time::{Duration, SystemTime};
        let root = crate::testing::scratch("chunks-sweep");
        let _ = std::fs::remove_dir_all(&root);
        crate::create(&root, &pe2_io::SystemFilesystem).unwrap();
        let project = crate::Project::load(&root).unwrap();
        let store = Store::at(&root, crate::testing::io());
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
