//! A project's merged layers as a filesystem: the tree the index resolves - every node a folder of its name holding its
//! files and the entries inside it (a node and its folder of children are one, `edit::paired`), every folder and plain
//! file where it is - as one tree any program can read, whatever layer each file comes from. Names are the asset
//! browser's: a node's has no `.node`. Backends serve it - `fuse` on Linux and macOS, `dokan`, `projfs` and `cloud` on Windows,
//! `dav` everywhere - each chosen for the jobs it does whole (`backend`); a tree is made of an index snapshot
//! (`Tree::of`), and what serves it takes a newer one without being mounted again (`Served::update`). Inode numbers
//! follow paths across snapshots, so a program holding a file keeps it as the project changes beneath it.
//!
//! Content is read through `project::content`, as the layers store it.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, RwLock};
use std::time::SystemTime;

use project::content::Given;
use project::index::Index;

pub mod access;
pub mod backend;
#[cfg(windows)]
pub mod cloud;
pub mod dav;
#[cfg(windows)]
pub mod dokan;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod fuse;
pub mod ops;
#[cfg(windows)]
pub mod projfs;
pub mod scoped;
pub mod strays;
pub mod write;
pub mod writes;

/// The root's inode number.
pub const ROOT: u64 = 1;

/// What a node of the tree is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum What {
    /// A folder, its children by name.
    Folder(BTreeMap<String, u64>),
    /// A file, its content where a layer stores it, or held in memory (`Given`).
    File(Given),
}

/// A node of the tree: its path from the root, its parent, what it is, its length and when it last changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub path: String,
    pub parent: u64,
    pub what: What,
    pub size: u64,
    pub modified: SystemTime,
    /// A folder that is a project node (an asset): its files in it beside what is inside it.
    pub asset: bool,
    /// A folder of a node's files (a UFO's `glyphs`), no entry of its own.
    pub of_files: bool,
}

/// The tree an index resolves, by inode number.
#[derive(Clone, Debug, Default)]
pub struct Tree {
    nodes: HashMap<u64, Node>,
    /// While a node's files are placed: the folders made are of its files.
    of_files: bool,
}

/// The inode numbers given to paths, kept across trees so that a path keeps its number.
#[derive(Debug)]
pub struct Inodes {
    by_path: HashMap<String, u64>,
    by_ino: HashMap<u64, String>,
    next: u64,
}

impl Default for Inodes {
    fn default() -> Inodes {
        Inodes {
            by_path: HashMap::from([(String::new(), ROOT)]),
            by_ino: HashMap::from([(ROOT, String::new())]),
            next: ROOT + 1,
        }
    }
}

impl Inodes {
    /// A path's number: the one it was given, or a new one.
    pub fn of(&mut self, path: &str) -> u64 {
        if let Some(ino) = self.by_path.get(path) {
            return *ino;
        }
        let ino = self.next;
        self.next += 1;
        self.by_path.insert(path.to_string(), ino);
        self.by_ino.insert(ino, path.to_string());
        ino
    }

    /// The path a number was given to.
    #[must_use]
    pub fn path(&self, ino: u64) -> Option<&str> {
        self.by_ino.get(&ino).map(String::as_str)
    }

    /// A folder's number and those of what is inside it let go of, for what is served no more: new paths there are
    /// numbered anew.
    pub fn forget(&mut self, folder: &str) {
        let inside = format!("{folder}/");
        let gone: Vec<String> =
            self.by_path.keys().filter(|path| *path == folder || path.starts_with(&inside)).cloned().collect();
        for path in gone {
            if let Some(ino) = self.by_path.remove(&path) {
                self.by_ino.remove(&ino);
            }
        }
    }

    /// A path renamed: its number, and those of what is inside it, go with it - as the kernel keeps them - and what
    /// was at the new path before lets go of its number.
    pub fn rename(&mut self, from: &str, to: &str) {
        let moved: Vec<(String, u64)> = self
            .by_path
            .iter()
            .filter(|(path, _)| *path == from || path.starts_with(&format!("{from}/")))
            .map(|(path, ino)| (path.clone(), *ino))
            .collect();
        for (path, _) in &moved {
            self.by_path.remove(path);
        }
        let replaced: Vec<String> =
            self.by_path.keys().filter(|p| *p == to || p.starts_with(&format!("{to}/"))).cloned().collect();
        for path in replaced {
            if let Some(ino) = self.by_path.remove(&path) {
                self.by_ino.remove(&ino);
            }
        }
        for (path, ino) in moved {
            let renamed = format!("{to}{}", &path[from.len()..]);
            self.by_ino.insert(ino, renamed.clone());
            self.by_path.insert(renamed, ino);
        }
    }
}

impl Tree {
    /// The tree an index resolves (`Index::served`), its inode numbers those `inodes` gave before, new paths given new
    /// ones. Each node is its bundle's folder with the files of the version on disk (not an unsaved working copy, nor
    /// what an import made); each folder and plain file is where it is.
    #[must_use]
    pub fn of(index: &Index, inodes: &mut Inodes) -> Tree {
        Tree::of_listing(&index.served(), inodes)
    }

    /// The tree of what is served, as listed (`project::host::Listing`): its entries first, then the nodes' files.
    #[must_use]
    pub fn of_listing(listing: &project::host::Listing, inodes: &mut Inodes) -> Tree {
        use project::host::Shown;
        let mut tree = Tree::default();
        tree.nodes.insert(
            ROOT,
            Node {
                path: String::new(),
                parent: ROOT,
                what: What::Folder(BTreeMap::new()),
                size: 0,
                modified: SystemTime::UNIX_EPOCH,
                asset: false,
                of_files: false,
            },
        );
        let time = |(secs, nanos): (u64, u32)| SystemTime::UNIX_EPOCH + std::time::Duration::new(secs, nanos);
        for (path, shown) in listing {
            match shown {
                Shown::Folder => drop(tree.folder(path, inodes)),
                Shown::Node => {
                    // A node is the folder of its name, beside - one with - its folder of children.
                    let ino = tree.folder(path, inodes);
                    if let Some(node) = tree.nodes.get_mut(&ino) {
                        node.asset = true;
                    }
                }
                Shown::File { disk, size, modified, of_files: false } => {
                    tree.file(path, Given::Disk(disk.clone()), *size, time(*modified), inodes);
                }
                Shown::File { of_files: true, .. } => {}
            }
        }
        // Then each node's files in its folder: the folders their names make are the node's, not entries.
        for (path, shown) in listing {
            if let Shown::File { disk, size, modified, of_files: true } = shown {
                tree.of_files = true;
                tree.file(path, Given::Disk(disk.clone()), *size, time(*modified), inodes);
                tree.of_files = false;
            }
        }
        tree
    }

    /// A tree of the files given, by their paths in it, each read where a layer stores it.
    #[must_use]
    pub fn of_files(files: &BTreeMap<String, Given>, inodes: &mut Inodes) -> Tree {
        let mut tree = Tree::default();
        let root = Node {
            path: String::new(),
            parent: ROOT,
            what: What::Folder(BTreeMap::new()),
            size: 0,
            modified: SystemTime::UNIX_EPOCH,
            asset: false,
            of_files: false,
        };
        tree.nodes.insert(ROOT, root);
        for (path, given) in files {
            let (size, modified) = match given {
                Given::Disk(disk) => project::content::stat(disk).unwrap_or((0, SystemTime::UNIX_EPOCH)),
                Given::Bytes(bytes) => (bytes.len() as u64, SystemTime::UNIX_EPOCH),
            };
            tree.file(path, given.clone(), size, modified, inodes);
        }
        tree
    }

    /// The folder at a path, made with its parents where missing.
    fn folder(&mut self, path: &str, inodes: &mut Inodes) -> u64 {
        if path.is_empty() {
            return ROOT;
        }
        let ino = inodes.of(path);
        if self.nodes.contains_key(&ino) {
            return ino;
        }
        let (parent_path, name) = path.rsplit_once('/').unwrap_or(("", path));
        let parent = self.folder(parent_path, inodes);
        self.adopt(parent, name, ino);
        let node = Node {
            path: path.to_string(),
            parent,
            what: What::Folder(BTreeMap::new()),
            size: 0,
            modified: SystemTime::UNIX_EPOCH,
            asset: false,
            of_files: self.of_files,
        };
        self.nodes.insert(ino, node);
        ino
    }

    fn file(&mut self, path: &str, content: Given, size: u64, modified: SystemTime, inodes: &mut Inodes) {
        let (parent_path, name) = path.rsplit_once('/').unwrap_or(("", path));
        let parent = self.folder(parent_path, inodes);
        let ino = inodes.of(path);
        self.adopt(parent, name, ino);
        self.nodes.insert(
            ino,
            Node {
                path: path.to_string(),
                parent,
                what: What::File(content),
                size,
                modified,
                asset: false,
                of_files: false,
            },
        );
    }

    fn adopt(&mut self, parent: u64, name: &str, ino: u64) {
        if let Some(Node { what: What::Folder(children), .. }) = self.nodes.get_mut(&parent) {
            children.insert(name.to_string(), ino);
        }
    }

    #[must_use]
    pub fn node(&self, ino: u64) -> Option<&Node> {
        self.nodes.get(&ino)
    }

    /// A node by its path.
    #[must_use]
    pub fn at(&self, path: &str) -> Option<(u64, &Node)> {
        let mut ino = ROOT;
        for step in path.split('/').filter(|s| !s.is_empty()) {
            ino = self.lookup(ino, step)?;
        }
        Some((ino, self.node(ino)?))
    }

    /// A folder's child by name.
    #[must_use]
    pub fn lookup(&self, parent: u64, name: &str) -> Option<u64> {
        match &self.nodes.get(&parent)?.what {
            What::Folder(children) => children.get(name).copied(),
            What::File(_) => None,
        }
    }

    /// Times as programs see them, which never go back: a file read from elsewhere, or of another length, than in the
    /// tree before - a layer's copy of it removed, what is below shown again - changed `now`, where its own time is
    /// older (what polls a file, an editor's watcher, sees a change only when its time moves on); a file as it was keeps
    /// the time it was shown with.
    fn keep_times(&mut self, before: &Tree, now: SystemTime) {
        for (ino, node) in &mut self.nodes {
            let Some(old) = before.nodes.get(ino).filter(|old| matches!(old.what, What::File(_))) else { continue };
            if !matches!(node.what, What::File(_)) || node.modified > old.modified {
                continue;
            }
            // A second on at least: what polls compares whole seconds.
            let later = old.modified + std::time::Duration::from_secs(1);
            node.modified = if node.what != old.what || node.size != old.size { now.max(later) } else { old.modified };
        }
    }

    /// What changed from one tree to another, by inode number: each file whose content changed (its length, its time
    /// or where it is read from), each node gone (its parent, its number, its name), and each node new (its parent, its
    /// name).
    #[must_use]
    pub fn changes(old: &Tree, new: &Tree) -> Changes {
        let mut changes = Changes::default();
        let name = |node: &Node| node.path.rsplit('/').next().unwrap_or(&node.path).to_string();
        for (ino, before) in &old.nodes {
            match new.nodes.get(ino) {
                None => changes.gone.push((before.parent, *ino, name(before))),
                Some(after) if matches!(after.what, What::File(_)) => {
                    if after.size != before.size || after.modified != before.modified || after.what != before.what {
                        changes.changed.push(*ino);
                    }
                }
                Some(_) => {}
            }
        }
        for (ino, after) in &new.nodes {
            if !old.nodes.contains_key(ino) {
                changes.new.push((after.parent, name(after)));
            }
        }
        changes
    }

    /// How many nodes it has.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Bytes of a file's content from `offset` read into a buffer, as many as it takes or the file has: how many. What
    /// a backend answers a read with, into the buffer it is given to fill.
    ///
    /// # Errors
    /// It is no file of the tree, or it cannot be read.
    pub fn read_into(&self, ino: u64, offset: u64, buffer: &mut [u8]) -> std::io::Result<usize> {
        match self.nodes.get(&ino).map(|n| &n.what) {
            Some(What::File(Given::Disk(disk))) => project::content::read_range_into(disk, offset, buffer),
            Some(What::File(Given::Bytes(bytes))) => {
                let start = usize::try_from(offset).unwrap_or(usize::MAX).min(bytes.len());
                let read = (bytes.len() - start).min(buffer.len());
                buffer[..read].copy_from_slice(&bytes[start..start + read]);
                Ok(read)
            }
            Some(What::Folder(_)) => Err(std::io::Error::from(std::io::ErrorKind::IsADirectory)),
            None => Err(std::io::Error::from(std::io::ErrorKind::NotFound)),
        }
    }

    /// Up to `len` bytes of a file's content from `offset`.
    ///
    /// # Errors
    /// It is no file of the tree, or it cannot be read.
    pub fn read(&self, ino: u64, offset: u64, len: usize) -> std::io::Result<Vec<u8>> {
        match self.nodes.get(&ino).map(|n| &n.what) {
            Some(What::File(Given::Disk(disk))) => project::content::read_range(disk, offset, len),
            Some(What::File(Given::Bytes(bytes))) => {
                let start = usize::try_from(offset).unwrap_or(usize::MAX).min(bytes.len());
                Ok(bytes[start..start.saturating_add(len).min(bytes.len())].to_vec())
            }
            Some(What::Folder(_)) => Err(std::io::Error::from(std::io::ErrorKind::IsADirectory)),
            None => Err(std::io::Error::from(std::io::ErrorKind::NotFound)),
        }
    }
}

/// What changed from one tree to another (`Tree::changes`).
#[derive(Debug, Default)]
pub struct Changes {
    pub changed: Vec<u64>,
    pub gone: Vec<(u64, u64, String)>,
    pub new: Vec<(u64, String)>,
}

/// What a backend saw of a file it serves, told to whoever watches what is served (`Served::watch`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seen {
    /// A program opened it to read.
    Read,
    /// It was read, and this much of it is now kept on disk.
    Kept(u64),
    /// What was kept of it on disk no longer is.
    Dropped,
}

/// What is told what backends see of the files served, each by its path in the tree.
type Watcher = Arc<dyn Fn(&str, Seen) + Send + Sync>;

/// A tree being served: the newest one, swapped in as the project changes, and the inode numbers kept across them.
#[derive(Clone, Default)]
pub struct Served {
    tree: Arc<RwLock<Arc<Tree>>>,
    inodes: Arc<Mutex<Inodes>>,
    /// How many files programs have open in it now: what serving waits for before it stops.
    open: Arc<std::sync::atomic::AtomicUsize>,
    /// What watches what is read of it, where something does (`access`).
    watcher: Arc<RwLock<Option<Watcher>>>,
}

/// A file open in what is served, for as long as this is held (`Served::opening`).
pub struct Open(Arc<std::sync::atomic::AtomicUsize>);

impl std::fmt::Debug for Open {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("open")
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

impl Served {
    /// A tree of an index, to serve.
    #[must_use]
    pub fn of(index: &Index) -> Served {
        let served = Served::default();
        served.update(index);
        served
    }

    /// Serve the tree of a newer index of the project.
    pub fn update(&self, index: &Index) {
        self.show(&index.served());
    }

    /// Serve what is listed (`Index::served`, as whoever holds the project sends it).
    pub fn show(&self, listing: &project::host::Listing) {
        let mut tree = {
            let mut inodes = self.inodes.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            Tree::of_listing(listing, &mut inodes)
        };
        tree.keep_times(&self.tree(), SystemTime::now());
        *self.tree.write().unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(tree);
    }

    /// Serve a tree of the files given (`Tree::of_files`).
    pub fn set_files(&self, files: &BTreeMap<String, Given>) {
        let tree = {
            let mut inodes = self.inodes.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            Tree::of_files(files, &mut inodes)
        };
        *self.tree.write().unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(tree);
    }

    /// A path's inode number: kept across trees, given now where the path has none yet (a file being made).
    #[must_use]
    pub fn ino(&self, path: &str) -> u64 {
        self.inodes.lock().unwrap_or_else(std::sync::PoisonError::into_inner).of(path)
    }

    /// The inode numbers of a folder and what was in it let go of: what is served no more (`Inodes::forget`).
    pub fn forget(&self, folder: &str) {
        self.inodes.lock().unwrap_or_else(std::sync::PoisonError::into_inner).forget(folder);
    }

    /// A path renamed: its inode number goes with it (`Inodes::rename`).
    pub fn rename(&self, from: &str, to: &str) {
        self.inodes.lock().unwrap_or_else(std::sync::PoisonError::into_inner).rename(from, to);
    }

    /// The path an inode number stands for.
    #[must_use]
    pub fn path(&self, ino: u64) -> Option<String> {
        self.inodes.lock().unwrap_or_else(std::sync::PoisonError::into_inner).path(ino).map(str::to_string)
    }

    /// The tree served now.
    #[must_use]
    pub fn tree(&self) -> Arc<Tree> {
        self.tree.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    /// Have what backends see of the files served told to a watcher, from now on.
    pub fn watch(&self, watcher: impl Fn(&str, Seen) + Send + Sync + 'static) {
        *self.watcher.write().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::new(watcher));
    }

    /// A backend saw something of a file it serves: told to what watches.
    pub fn seen(&self, path: &str, seen: Seen) {
        let watcher = self.watcher.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        if let Some(watcher) = watcher {
            watcher(path, seen);
        }
    }

    /// A file opened by a program, until it is released (`released`): one being read or written, not one looked at.
    pub fn opened(&self) {
        self.open.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// A file a program opened released.
    pub fn released(&self) {
        let _ = self.open.try_update(std::sync::atomic::Ordering::Relaxed, std::sync::atomic::Ordering::Relaxed, |n| {
            n.checked_sub(1)
        });
    }

    /// A file open for as long as what this returns is held.
    #[must_use]
    pub fn opening(&self) -> Open {
        self.opened();
        Open(self.open.clone())
    }

    /// How many files programs have open in it now.
    #[must_use]
    pub fn open_files(&self) -> usize {
        self.open.load(std::sync::atomic::Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::Inodes;

    #[test]
    fn a_renamed_path_keeps_its_number_and_its_contents_theirs() {
        let mut inodes = Inodes::default();
        let (file, folder, inner, old) = (inodes.of("a/tmp1"), inodes.of("a/b"), inodes.of("a/b/c"), inodes.of("a/x"));
        inodes.rename("a/tmp1", "a/x");
        assert_eq!(inodes.of("a/x"), file);
        assert_eq!(inodes.path(old), None);
        inodes.rename("a/b", "a/d");
        assert_eq!((inodes.of("a/d"), inodes.of("a/d/c")), (folder, inner));
        assert_eq!(inodes.path(inner), Some("a/d/c"));
    }
}
