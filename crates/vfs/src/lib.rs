//! A project's merged layers as a filesystem: the tree the index resolves - every node a folder of its name holding its
//! files and the entries inside it (a node and its folder of children are one, `edit::paired`), every folder and plain
//! file where it is - as one tree any program can read, whatever layer each file comes from. Names are the asset
//! browser's: a node's has no `.node`. Backends serve it - `fuse` on Linux and macOS, `dokan`, `projfs` and `cloud` on Windows,
//! `dav` everywhere - each chosen for the jobs it does whole (`backend`). A project's server serves the filesystem
//! the project's processes share (`Served::shared`): a view of what whoever holds the project says is served there
//! (`project::shared`), read by path as it is asked, nothing of it held here. A tree of files given - a scope's - is
//! held node by node (`Tree::of_files`). Inode numbers follow paths, whatever changes beneath them and wherever a
//! path is moved, so a program holding a file keeps it.
//!
//! Content is read through `project::content`, as the layers store it.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex, RwLock};
use std::time::SystemTime;

use pe2_path::{Name, RelPath};
use project::content::Given;

pub mod access;
pub mod backend;
#[cfg(windows)]
pub mod cloud;
pub mod dav;
#[cfg(windows)]
pub mod dokan;
pub mod due;
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
    /// A folder, its children by name - found however a name is written, each as it is written.
    Folder(BTreeMap<Arc<Name>, u64>),
    /// A file, its content where a layer stores it, or held in memory (`Given`).
    File(Given),
}

/// A node of the tree: its path from the root, its parent, what it is, its length and when it last changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub path: Arc<RelPath>,
    pub parent: u64,
    pub what: What,
    pub size: u64,
    pub modified: SystemTime,
    /// A folder that is a project node (an asset): its files in it beside what is inside it.
    pub asset: bool,
    /// A folder of a node's files (a UFO's `glyphs`), no entry of its own.
    pub of_files: bool,
}

/// The tree served, by inode number: held here, node by node - a tree of files given (`Tree::of_files`), of a
/// listing - or, for a project's server, a view of the filesystem the project's processes share (`View`), read
/// there as it is asked.
#[derive(Clone, Debug, Default)]
pub struct Tree {
    /// Each node shared with the trees this one was made from: a tree changed is a copy of the one before, which
    /// costs a count a node where the nodes themselves are not copied.
    nodes: HashMap<u64, Arc<Node>>,
    /// While a node's files are placed: the folders made are of its files.
    of_files: bool,
    /// Where it holds no nodes of its own: what it reads them from.
    shared: Option<View>,
}

/// An entry of a folder: its name as it is written, its number and its node.
pub type Entry = (Arc<Name>, u64, Arc<Node>);

/// The longest file whose pages are kept of what was read of it (`View::read_into`).
const PAGED: u64 = 256 << 10;

/// What a path was found to be, at one commit of the shared space and one state of what was moved here: its inode
/// number and node, and what the shared filesystem knows it by - its number there and its version.
type Found = HashMap<Arc<RelPath>, Option<(u64, Arc<Node>, (u64, u64))>>;

/// The shared filesystem as a tree: what whoever holds the project says is served (`project::shared`), and what
/// whoever serves the folder made of it as programs asked - a folder made, a path moved, a thing removed - read
/// by path as it is asked for. What was found is kept until the next commit.
#[derive(Clone)]
struct View {
    space: Arc<sfs::Space>,
    inodes: Arc<Mutex<Inodes>>,
    /// What was found, and at which commit.
    found: Arc<Mutex<(u64, Found)>>,
    /// What was read of its files, where that is kept: a file's pages by its number and version in the shared
    /// filesystem, read from here where they are.
    ring: Option<Arc<sfs::ring::Ring>>,
}

impl std::fmt::Debug for View {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "a view of the shared filesystem")
    }
}

impl View {
    /// What a path is at a commit, written as the shared filesystem has it whatever way it is asked by.
    fn build(&self, pages: &impl sfs::space::Pages, path: &RelPath) -> Option<(u64, Arc<Node>, (u64, u64))> {
        use project::shared::{GONE, NEW, NODE, OF_FILES};
        use sfs::fs::{Bytes, SERVED};
        let (mut number, mut written) = (sfs::fs::ROOT, RelPath::root().to_owned());
        for name in path.parts() {
            let (name, inside) = sfs::fs::entry(pages, SERVED, number, name).ok()??;
            (number, written) = (inside, written.join(&name));
        }
        // Removed through the folder: shown no more, whatever the project has of it.
        let said = sfs::fs::node(pages, SERVED, number).ok()?.filter(|said| said.flags & GONE == 0)?;
        let what = if said.folder {
            // What it holds is asked of the tree (`Tree::children`), not kept with it.
            What::Folder(BTreeMap::new())
        } else {
            match &said.bytes {
                Bytes::Derived(lives) => What::File(Given::Disk(project::shared::placed(lives)?.0)),
                // Made through the folder: what it holds is whoever serves it's to say (`Writes`).
                _ if said.flags & NEW != 0 => What::File(Given::Bytes(Arc::from(&[][..]))),
                _ => return None,
            }
        };
        let mut inodes = self.inodes.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (ino, parent) = (inodes.of(&written), inodes.of(parent_of(&written)));
        drop(inodes);
        let node = Node {
            path: written,
            parent,
            what,
            size: said.len,
            modified: SystemTime::UNIX_EPOCH + std::time::Duration::from_nanos(said.modified),
            asset: said.folder && said.flags & NODE != 0,
            of_files: said.folder && said.flags & OF_FILES != 0 && said.flags & NODE == 0,
        };
        Some((ino, Arc::new(node), (number, said.version)))
    }

    /// What is at a path: as found before at this commit, or read now.
    fn at(&self, path: &RelPath) -> Option<(u64, Arc<Node>)> {
        self.known(path).map(|(ino, node, _)| (ino, node))
    }

    /// As `at`, with what the shared filesystem knows it by.
    fn known(&self, path: &RelPath) -> Option<(u64, Arc<Node>, (u64, u64))> {
        let pages = self.space.read().ok()?;
        let generation = pages.commit().generation;
        {
            let mut found = self.found.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if found.0 != generation {
                *found = (generation, Found::new());
            }
            if let Some(known) = found.1.get(path) {
                return known.clone();
            }
        }
        let built = self.build(&pages, path);
        let mut found = self.found.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if found.0 == generation {
            found.1.insert(path.to_owned(), built.clone());
        }
        built
    }

    /// What a folder holds: each name as it is written, with whether it is a folder. None where the path is none.
    fn children(&self, path: &RelPath) -> Option<BTreeMap<Arc<Name>, bool>> {
        use sfs::fs::SERVED;
        let pages = self.space.read().ok()?;
        let number = sfs::fs::at(&pages, SERVED, path).ok()??;
        let shown = |number: u64| {
            sfs::fs::node(&pages, SERVED, number).ok().flatten().filter(|node| node.flags & project::shared::GONE == 0)
        };
        if !shown(number)?.folder {
            return None;
        }
        let mut listed = Vec::new();
        let _ = sfs::fs::list(&pages, SERVED, number, None, &mut |name, number| {
            listed.push((name.to_owned(), number));
            true
        });
        // Each name with whether it is a folder, less what was removed through the folder.
        Some(listed.into_iter().filter_map(|(name, number)| Some((name, shown(number)?.folder))).collect())
    }

    /// Bytes of a file from `offset`, read into a buffer: page by page from the ring where it holds them, and
    /// where it does not, read where the file's content lives - all that is still asked for at once - and kept.
    fn read_into(&self, ino: u64, offset: u64, buffer: &mut [u8]) -> std::io::Result<usize> {
        use sfs::ring::{Key, PAGE};
        let path = self.inodes.lock().unwrap_or_else(std::sync::PoisonError::into_inner).path(ino).cloned();
        let found = path.and_then(|path| self.known(&path));
        let Some((_, node, (number, version))) = found else {
            return Err(std::io::Error::from(std::io::ErrorKind::NotFound));
        };
        let What::File(Given::Disk(disk)) = &node.what else {
            return Err(std::io::Error::from(std::io::ErrorKind::IsADirectory));
        };
        // A long file is read where it lives: read from one end to the other, the system has it there faster than
        // page by page from here, and it would take the place of very many short ones.
        let Some(ring) = self.ring.as_ref().filter(|_| node.size <= PAGED) else {
            return project::content::read_range_into(disk, offset, buffer);
        };
        let end = node.size.min(offset.saturating_add(buffer.len() as u64));
        let mut at = offset;
        while at < end {
            let key = Key { number, version, page: at / PAGE as u64 };
            let (start, within) = (key.page * PAGE as u64, usize::try_from(at % PAGE as u64).unwrap_or(0));
            let done = usize::try_from(at - offset).unwrap_or(usize::MAX);
            if let Some((take, held)) = ring.get_part(&key, within, &mut buffer[done..]) {
                at += take as u64;
                // A page shorter than one is the file's last - or one of a file that is shorter than was said.
                if held < PAGE || take == 0 {
                    break;
                }
                continue;
            }
            // Not held: from this page on, all that is asked for, read where it lives at once.
            let ends = end.next_multiple_of(PAGE as u64).min(node.size.max(end));
            let mut read = vec![0u8; usize::try_from(ends - start).unwrap_or(0)];
            let got = project::content::read_range_into(disk, start, &mut read)?;
            for (n, bytes) in read[..got].chunks(PAGE).enumerate() {
                ring.put(&Key { number, version, page: key.page + n as u64 }, bytes);
            }
            let take = got.saturating_sub(within).min(buffer.len() - done);
            buffer[done..done + take].copy_from_slice(&read[within..within + take]);
            at += take as u64;
            break;
        }
        Ok(usize::try_from(at - offset).unwrap_or(0))
    }

    /// How many folders and files it shows.
    fn len(&self) -> usize {
        let mut count = 1;
        let mut folders = vec![RelPath::root().to_owned()];
        while let Some(folder) = folders.pop() {
            for (name, is_folder) in self.children(&folder).unwrap_or_default() {
                count += 1;
                if is_folder {
                    folders.push(folder.join(&name));
                }
            }
        }
        count
    }
}

/// The folder a path is in; the root's is itself.
fn parent_of(path: &RelPath) -> &RelPath {
    path.parent().unwrap_or(RelPath::root())
}

/// The name a path has in its folder; the root has none.
fn name_of(path: &RelPath) -> Option<&Name> {
    path.last()
}

/// The inode numbers given to paths, kept across trees so that a path keeps its number - however it is written.
#[derive(Debug)]
pub struct Inodes {
    /// In order, so that a folder and what is inside it are found without looking at every path.
    by_path: BTreeMap<Arc<RelPath>, u64>,
    by_ino: HashMap<u64, Arc<RelPath>>,
    next: u64,
}

impl Default for Inodes {
    fn default() -> Inodes {
        let root = RelPath::root().to_owned();
        Inodes {
            by_path: BTreeMap::from([(root.clone(), ROOT)]),
            by_ino: HashMap::from([(ROOT, root)]),
            next: ROOT + 1,
        }
    }
}

impl Inodes {
    /// A path's number: the one it was given, or a new one.
    pub fn of(&mut self, path: &RelPath) -> u64 {
        if let Some(ino) = self.by_path.get(path) {
            return *ino;
        }
        let ino = self.next;
        self.next += 1;
        let path = path.to_owned();
        self.by_path.insert(path.clone(), ino);
        self.by_ino.insert(ino, path);
        ino
    }

    /// The path a number was given to.
    #[must_use]
    pub fn path(&self, ino: u64) -> Option<&Arc<RelPath>> {
        self.by_ino.get(&ino)
    }

    /// A path's number, where it was given one: none is given for the asking.
    #[must_use]
    pub fn known(&self, path: &RelPath) -> Option<u64> {
        self.by_path.get(path).copied()
    }

    /// The paths at a folder and inside it that have numbers, each with its own. Paths order name by name: what is
    /// inside a folder follows it. The root's is its own alone.
    fn at_and_inside(&self, folder: &RelPath) -> Vec<(Arc<RelPath>, u64)> {
        if folder.is_root() {
            return vec![(RelPath::root().to_owned(), ROOT)];
        }
        let inside =
            self.by_path.range::<RelPath, _>(pe2_path::from(folder)).take_while(|(path, _)| path.starts_with(folder));
        inside.map(|(path, ino)| (path.clone(), *ino)).collect()
    }

    /// A folder's number and those of what is inside it let go of, for what is served no more: new paths there are
    /// numbered anew.
    pub fn forget(&mut self, folder: &RelPath) {
        for (path, ino) in self.at_and_inside(folder) {
            self.by_path.remove(&*path);
            self.by_ino.remove(&ino);
        }
    }

    /// A path renamed: its number, and those of what is inside it, go with it - as the kernel keeps them - and what
    /// was at the new path before lets go of its number. Renamed to another way of writing its name, it is written
    /// so from then on.
    pub fn rename(&mut self, from: &RelPath, to: &RelPath) {
        let moved = self.at_and_inside(from);
        for (path, _) in &moved {
            self.by_path.remove(&**path);
        }
        self.forget(to);
        for (path, ino) in moved {
            let renamed = to.join_path(path.strip_prefix(from).unwrap_or(RelPath::root()));
            self.by_ino.insert(ino, renamed.clone());
            self.by_path.insert(renamed, ino);
        }
    }
}

impl Tree {
    /// The tree of what is listed as served (`project::server::Listing`), held: its entries first, then the nodes'
    /// files. What a view of the shared filesystem is held to, node by node.
    #[cfg(test)]
    fn of_listing(listing: &project::server::Listing, inodes: &mut Inodes) -> Tree {
        use project::server::Shown;
        let mut tree = Tree::default();
        tree.nodes.insert(
            ROOT,
            Arc::new(Node {
                path: RelPath::root().to_owned(),
                parent: ROOT,
                what: What::Folder(BTreeMap::new()),
                size: 0,
                modified: SystemTime::UNIX_EPOCH,
                asset: false,
                of_files: false,
            }),
        );
        let time = |(secs, nanos): (u64, u32)| SystemTime::UNIX_EPOCH + std::time::Duration::new(secs, nanos);
        for (path, shown) in listing {
            let path = path.rel();
            match shown {
                Shown::Folder => drop(tree.folder(path, inodes)),
                Shown::Node => {
                    // A node is the folder of its name, beside - one with - its folder of children.
                    let ino = tree.folder(path, inodes);
                    if let Some(node) = tree.nodes.get_mut(&ino).map(Arc::make_mut) {
                        node.asset = true;
                    }
                }
                Shown::File { disk, size, modified, of_files: false, .. } => {
                    tree.file(path, Given::Disk(disk.clone()), *size, time(*modified), inodes);
                }
                Shown::File { of_files: true, .. } => {}
            }
        }
        // Then each node's files in its folder: the folders their names make are the node's, not entries.
        for (path, shown) in listing {
            if let Shown::File { disk, size, modified, of_files: true, .. } = shown {
                tree.of_files = true;
                tree.file(path.rel(), Given::Disk(disk.clone()), *size, time(*modified), inodes);
                tree.of_files = false;
            }
        }
        tree
    }

    /// A tree of the files given, by their paths in it, each read where a layer stores it.
    #[must_use]
    pub fn of_files(files: &BTreeMap<Arc<RelPath>, Given>, inodes: &mut Inodes) -> Tree {
        let mut tree = Tree::default();
        let root = Node {
            path: RelPath::root().to_owned(),
            parent: ROOT,
            what: What::Folder(BTreeMap::new()),
            size: 0,
            modified: SystemTime::UNIX_EPOCH,
            asset: false,
            of_files: false,
        };
        tree.nodes.insert(ROOT, Arc::new(root));
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
    fn folder(&mut self, path: &RelPath, inodes: &mut Inodes) -> u64 {
        let (Some(name), Some(parent_path)) = (name_of(path), path.parent()) else { return ROOT };
        let ino = inodes.of(path);
        if self.nodes.contains_key(&ino) {
            return ino;
        }
        let parent = self.folder(parent_path, inodes);
        self.adopt(parent, name, ino);
        let node = Node {
            path: path.to_owned(),
            parent,
            what: What::Folder(BTreeMap::new()),
            size: 0,
            modified: SystemTime::UNIX_EPOCH,
            asset: false,
            of_files: self.of_files,
        };
        self.nodes.insert(ino, Arc::new(node));
        ino
    }

    fn file(&mut self, path: &RelPath, content: Given, size: u64, modified: SystemTime, inodes: &mut Inodes) {
        let Some(name) = name_of(path) else { return };
        let parent = self.folder(parent_of(path), inodes);
        let ino = inodes.of(path);
        self.adopt(parent, name, ino);
        self.nodes.insert(
            ino,
            Arc::new(Node {
                path: path.to_owned(),
                parent,
                what: What::File(content),
                size,
                modified,
                asset: false,
                of_files: false,
            }),
        );
    }

    /// Move a path, with what is in it, to another: there at once, each keeping its number. What was at the new
    /// path goes - but itself, renamed to another way of writing its name. Whether anything was there to move.
    pub fn rename(&mut self, from: &RelPath, to: &RelPath, inodes: &mut Inodes) -> bool {
        let Some((ino, above)) = self.at(from).map(|(ino, node)| (ino, node.parent)) else { return false };
        let Some(name) = name_of(to) else { return false };
        if self.at(to).is_some_and(|(there, _)| there != ino) {
            self.remove(to);
        }
        if let (Some(Node { what: What::Folder(children), .. }), Some(was)) =
            (self.nodes.get_mut(&above).map(Arc::make_mut), name_of(from))
        {
            children.remove(was);
        }
        let parent = self.folder(parent_of(to), inodes);
        inodes.rename(from, to);
        self.adopt(parent, name, ino);
        let mut moving = vec![ino];
        while let Some(at) = moving.pop() {
            let Some(node) = self.nodes.get_mut(&at).map(Arc::make_mut) else { continue };
            node.path = to.join_path(node.path.strip_prefix(from).unwrap_or(RelPath::root()));
            if at == ino {
                node.parent = parent;
            }
            if let What::Folder(children) = &node.what {
                moving.extend(children.values().copied());
            }
        }
        true
    }

    /// Take a path out of the tree, with what is in it.
    fn remove(&mut self, path: &RelPath) {
        let Some((ino, node)) = self.at(path).map(|(ino, node)| (ino, node.parent)) else { return };
        if let (Some(Node { what: What::Folder(children), .. }), Some(name)) =
            (self.nodes.get_mut(&node).map(Arc::make_mut), name_of(path))
        {
            children.remove(name);
        }
        let mut going = vec![ino];
        while let Some(ino) = going.pop() {
            if let Some(node) = self.nodes.remove(&ino)
                && let What::Folder(children) = &node.what
            {
                going.extend(children.values().copied());
            }
        }
        // A folder a node's files made, left empty, is no entry of its own: it goes with the last of them.
        let Some(above) = path.parent().filter(|above| !above.is_root()) else { return };
        let emptied = self.at(above).is_some_and(|(_, folder)| {
            folder.of_files && !folder.asset && matches!(&folder.what, What::Folder(children) if children.is_empty())
        });
        if emptied {
            self.remove(above);
        }
    }

    /// A folder has a child by a name, written as it is now: one written otherwise before gives way.
    fn adopt(&mut self, parent: u64, name: &Name, ino: u64) {
        if let Some(Node { what: What::Folder(children), .. }) = self.nodes.get_mut(&parent).map(Arc::make_mut) {
            children.remove(name);
            children.insert(name.to_owned(), ino);
        }
    }

    /// A node by its number.
    #[must_use]
    pub fn node(&self, ino: u64) -> Option<Arc<Node>> {
        let Some(view) = &self.shared else { return self.nodes.get(&ino).cloned() };
        let path = view.inodes.lock().unwrap_or_else(std::sync::PoisonError::into_inner).path(ino).cloned()?;
        view.at(&path).map(|(_, node)| node)
    }

    /// A node by its path.
    #[must_use]
    pub fn at(&self, path: &RelPath) -> Option<(u64, Arc<Node>)> {
        if let Some(view) = &self.shared {
            return view.at(path);
        }
        let mut ino = ROOT;
        for step in path.parts() {
            ino = self.lookup(ino, step)?;
        }
        Some((ino, self.node(ino)?))
    }

    /// What the folder at a path holds: each name as it is written, with whether it is a folder. None where the path
    /// is no folder. A folder's node says nothing of what it holds where the tree is a view: this does, for either.
    #[must_use]
    pub fn children(&self, path: &RelPath) -> Option<BTreeMap<Arc<Name>, bool>> {
        if let Some(view) = &self.shared {
            return view.children(path);
        }
        let (_, node) = self.at(path)?;
        let What::Folder(children) = &node.what else { return None };
        let folder = |ino: &u64| matches!(self.nodes.get(ino).map(|node| &node.what), Some(What::Folder(_)));
        Some(children.iter().map(|(name, ino)| (name.clone(), folder(ino))).collect())
    }

    /// What the folder at a path holds: each name with its number and its node. None where the path is no folder.
    #[must_use]
    pub fn entries(&self, path: &RelPath) -> Option<Vec<Entry>> {
        let names = self.children(path)?;
        Some(
            names
                .into_keys()
                .filter_map(|name| self.at(&path.join(&name)).map(|(ino, node)| (name, ino, node)))
                .collect(),
        )
    }

    /// A folder's child by name, however the name is written.
    #[must_use]
    pub fn lookup(&self, parent: u64, name: &Name) -> Option<u64> {
        if let Some(view) = &self.shared {
            let folder = view.inodes.lock().unwrap_or_else(std::sync::PoisonError::into_inner).path(parent).cloned()?;
            return view.at(&folder.join(name)).map(|(ino, _)| ino);
        }
        match &self.nodes.get(&parent)?.what {
            What::Folder(children) => children.get(name).copied(),
            What::File(_) => None,
        }
    }

    /// What changed from one tree held to another, by path: each file whose content changed (its length, its time or
    /// where it is read from), each node gone and each node new, with whether it is a folder.
    #[must_use]
    pub fn changes(old: &Tree, new: &Tree) -> Changes {
        let mut changes = Changes::default();
        let folder = |node: &Node| matches!(node.what, What::Folder(_));
        for (ino, before) in &old.nodes {
            match new.nodes.get(ino) {
                None => changes.gone.push((before.path.clone(), folder(before))),
                // The same number at another path - or written otherwise: moved.
                Some(after) if after.path.as_str() != before.path.as_str() => {
                    changes.gone.push((before.path.clone(), folder(before)));
                    changes.new.push((after.path.clone(), folder(after)));
                }
                Some(after) if matches!(after.what, What::File(_)) => {
                    if after.size != before.size || after.modified != before.modified || after.what != before.what {
                        changes.changed.push(after.path.clone());
                    }
                }
                Some(_) => {}
            }
        }
        for (ino, after) in &new.nodes {
            if !old.nodes.contains_key(ino) {
                changes.new.push((after.path.clone(), folder(after)));
            }
        }
        changes
    }

    /// How many nodes it has.
    #[must_use]
    pub fn len(&self) -> usize {
        self.shared.as_ref().map_or(self.nodes.len(), View::len)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.shared.is_none() && self.nodes.is_empty()
    }

    /// Bytes of a file's content from `offset` read into a buffer, as many as it takes or the file has: how many. What
    /// a backend answers a read with, into the buffer it is given to fill.
    ///
    /// # Errors
    /// It is no file of the tree, or it cannot be read.
    pub fn read_into(&self, ino: u64, offset: u64, buffer: &mut [u8]) -> std::io::Result<usize> {
        if let Some(view) = &self.shared {
            return view.read_into(ino, offset, buffer);
        }
        match self.node(ino).as_deref().map(|n| &n.what) {
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
        if let Some(view) = &self.shared {
            // No more than the file has left: nothing is made for what it has not.
            let left = self.node(ino).map_or(0, |node| node.size.saturating_sub(offset));
            let mut read = vec![0u8; len.min(usize::try_from(left).unwrap_or(usize::MAX))];
            let got = view.read_into(ino, offset, &mut read)?;
            read.truncate(got);
            return Ok(read);
        }
        match self.node(ino).as_deref().map(|n| &n.what) {
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

/// What changed of the tree served, by path: the files whose content changed, what is gone and what is new - each
/// with whether it is a folder.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    pub changed: Vec<Arc<RelPath>>,
    pub gone: Vec<(Arc<RelPath>, bool)>,
    pub new: Vec<(Arc<RelPath>, bool)>,
    /// The inode numbers that what is gone, and the folders it was in, had when this was said: what knows them
    /// by number - a kernel - is told by these, whatever became of the numbers since.
    pub numbers: HashMap<Arc<RelPath>, u64>,
}

impl Changes {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty() && self.gone.is_empty() && self.new.is_empty()
    }

    fn then(&mut self, later: Changes) {
        self.changed.extend(later.changed);
        self.gone.extend(later.gone);
        self.new.extend(later.new);
        self.numbers.extend(later.numbers);
    }

    /// What changed of the tree where a listing of what is served changed (`project::shared::changed`): the
    /// listing before, what differs, and the listing after. A file listed before and after changed; a path listed
    /// no more is gone, one not listed before is new - and with a node's file, each folder its name made or left
    /// empty, which no listing has.
    #[must_use]
    pub fn of(
        before: &project::server::Listing,
        delta: &project::server::Delta,
        after: &project::server::Listing,
    ) -> Changes {
        use project::TreePath;
        use project::server::{Listing, Shown};
        use std::ops::Bound;
        let folder = |shown: &Shown| !matches!(shown, Shown::File { .. });
        let rel = |path: &Arc<TreePath>| TreePath::into_rel(path.clone());
        // Whether a folder no listing has is there: something is listed inside it - paths order name by name, so it
        // follows the folder.
        let holds = |listing: &Listing, path: &TreePath| {
            let after = listing.range::<TreePath, _>((Bound::Excluded(path), Bound::Unbounded)).next();
            after.is_some_and(|(under, _)| under.starts_with(path))
        };
        let there = |listing: &Listing, path: &TreePath| listing.contains_key(path) || holds(listing, path);
        let above = |path: &TreePath| path.parent().filter(|above| !above.is_root()).map(TreePath::to_owned);
        let mut changes = Changes::default();
        for path in &delta.gone {
            // Listed no more - but still a folder, where a node's files are in it.
            let was_folder = before.get(&**path).is_none_or(folder);
            if !(was_folder && holds(after, path)) {
                changes.gone.push((rel(path), was_folder));
            }
            let mut at = above(path);
            while let Some(folder) = at.filter(|folder| !there(after, folder)) {
                changes.gone.push((rel(&folder), true));
                at = above(&folder);
            }
        }
        for (path, shown) in &delta.shown {
            match before.get_key_value(&**path) {
                // Written otherwise: gone where it was written so, new where it is written now.
                Some((was_at, was)) if was_at.as_str() != path.as_str() => {
                    changes.gone.push((rel(was_at), folder(was)));
                    changes.new.push((rel(path), folder(shown)));
                }
                Some((_, was)) if folder(was) == folder(shown) => {
                    if !folder(shown) && was != shown {
                        changes.changed.push(rel(path));
                    }
                }
                Some((_, was)) => {
                    changes.gone.push((rel(path), folder(was)));
                    changes.new.push((rel(path), folder(shown)));
                }
                // A folder a node's files made, now an entry, was there.
                None if folder(shown) && holds(before, path) => {}
                None => {
                    let mut made = Vec::new();
                    let mut at = above(path);
                    while let Some(folder) = at.filter(|folder| !there(before, folder)) {
                        made.push((rel(&folder), true));
                        at = above(&folder);
                    }
                    changes.new.extend(made.into_iter().rev());
                    changes.new.push((rel(path), folder(shown)));
                }
            }
        }
        changes.gone.sort();
        changes.gone.dedup();
        changes.new.dedup();
        changes
    }
}

/// How many changes of the tree served are kept for those that tell of them: one that falls further behind is
/// told so.
const CHANGES_KEPT: usize = 4096;

/// The changes of the tree served, each with its number, for what tells others of them to take in turn.
#[derive(Default)]
struct Feed {
    next: u64,
    kept: VecDeque<(u64, Changes)>,
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
type Watcher = Arc<dyn Fn(&RelPath, Seen) + Send + Sync>;

/// A tree being served: the newest one, swapped in as the project changes, and the inode numbers kept across them.
#[derive(Clone, Default)]
pub struct Served {
    tree: Arc<RwLock<Arc<Tree>>>,
    inodes: Arc<Mutex<Inodes>>,
    /// How many files programs have open in it now: what serving waits for before it stops.
    open: Arc<std::sync::atomic::AtomicUsize>,
    /// What watches what is read of it, where something does (`access`).
    watcher: Arc<RwLock<Option<Watcher>>>,
    /// What is woken when the tree changes: what tells of it - the kernel, what watches a mount, a folder brought in
    /// step - does so at once, where it would else find out when it next looked.
    wakers: Arc<Mutex<Vec<Waker>>>,
    /// What changed of the tree, for those that tell of it (`changes_since`).
    feed: Arc<Mutex<Feed>>,
}

/// What is called when the tree served changes.
type Waker = Box<dyn Fn() + Send + Sync>;

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
    /// What the filesystem a project's processes share says is served (`project::shared`), as a tree: read there
    /// as it is asked, nothing of it held here - its files' content through a ring of pages, where one is given.
    /// What a project's server serves.
    #[must_use]
    pub fn shared(space: Arc<sfs::Space>, ring: Option<Arc<sfs::ring::Ring>>) -> Served {
        let served = Served::default();
        let view = View { space, inodes: served.inodes.clone(), found: Arc::default(), ring };
        *served.tree.write().unwrap_or_else(std::sync::PoisonError::into_inner) =
            Arc::new(Tree { shared: Some(view), ..Tree::default() });
        served
    }

    /// The tree is a view of the shared filesystem, and that changed: what tells others of it is told what did.
    pub fn told(&self, changes: Changes) {
        self.changed(changes);
    }

    /// The shared filesystem the tree is a view of, where it is one.
    #[must_use]
    pub fn space(&self) -> Option<Arc<sfs::Space>> {
        self.tree().shared.as_ref().map(|view| view.space.clone())
    }

    /// A path was moved in the shared filesystem the tree is a view of, with what is in it, by whoever serves it,
    /// a program having asked: each file keeps its number, and those that tell of the tree are told it is gone
    /// where it was and new where it is, with what was at its new path before (`replaced`: whether a folder).
    pub fn moved_there(&self, from: &RelPath, to: &RelPath, folder: bool, replaced: Option<bool>) {
        self.rename(from, to);
        let mut gone = vec![(from.to_owned(), folder)];
        gone.extend(replaced.map(|folder| (to.to_owned(), folder)));
        self.changed(Changes { gone, new: vec![(to.to_owned(), folder)], ..Changes::default() });
    }

    /// Say what changed of the tree to those that tell of it, and wake them.
    fn changed(&self, mut changes: Changes) {
        if !changes.is_empty() {
            {
                let inodes = self.inodes.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                for (path, _) in &changes.gone {
                    for path in [&**path, parent_of(path)] {
                        if let Some(ino) = inodes.known(path) {
                            changes.numbers.insert(path.to_owned(), ino);
                        }
                    }
                }
            }
            let mut feed = self.feed.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let number = feed.next;
            feed.next += 1;
            feed.kept.push_back((number, changes));
            while feed.kept.len() > CHANGES_KEPT {
                feed.kept.pop_front();
            }
        }
        self.wake();
    }

    /// Where the changes of the tree are now: what is asked `changes_since` from here is what changes from now on.
    #[must_use]
    pub fn changes_from_now(&self) -> u64 {
        self.feed.lock().unwrap_or_else(std::sync::PoisonError::into_inner).next
    }

    /// What changed of the tree since `from` (`changes_from_now`, or what this left there), as one; `from` is where
    /// they are now. None where some of it is no longer kept: who asks cannot tell what changed.
    #[must_use]
    pub fn changes_since(&self, from: &mut u64) -> Option<Changes> {
        let feed = self.feed.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let since = std::mem::replace(from, feed.next);
        if feed.kept.front().is_some_and(|(first, _)| *first > since) {
            return None;
        }
        let mut all = Changes::default();
        for (_, changes) in feed.kept.iter().filter(|(number, _)| *number >= since) {
            all.then(changes.clone());
        }
        Some(all)
    }

    /// A path's inode number, where it was given one - by a lookup, a listing: none is given for the asking.
    #[must_use]
    pub fn known(&self, path: &RelPath) -> Option<u64> {
        self.inodes.lock().unwrap_or_else(std::sync::PoisonError::into_inner).known(path)
    }

    /// Put a tree held in place of the one before, and say what differs to those that tell of it.
    fn put(&self, tree: Tree) {
        let before = self.tree();
        // A view of the shared filesystem is what that says, whatever is listed here.
        if before.shared.is_some() {
            return self.wake();
        }
        let changes = Tree::changes(&before, &tree);
        *self.tree.write().unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(tree);
        self.changed(changes);
    }

    /// Call this whenever the tree served changes, from now on: for what tells others of it to do so at once.
    pub fn on_change(&self, wake: impl Fn() + Send + Sync + 'static) {
        self.wakers.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(Box::new(wake));
    }

    fn wake(&self) {
        for wake in self.wakers.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter() {
            wake();
        }
    }

    /// Serve a tree of the files given (`Tree::of_files`).
    pub fn set_files(&self, files: &BTreeMap<Arc<RelPath>, Given>) {
        let tree = {
            let mut inodes = self.inodes.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            Tree::of_files(files, &mut inodes)
        };
        self.put(tree);
    }

    /// A path's inode number: kept across trees, given now where the path has none yet (a file being made).
    #[must_use]
    pub fn ino(&self, path: &RelPath) -> u64 {
        self.inodes.lock().unwrap_or_else(std::sync::PoisonError::into_inner).of(path)
    }

    /// The inode numbers of a folder and what was in it let go of: what is served no more (`Inodes::forget`).
    pub fn forget(&self, folder: &RelPath) {
        self.inodes.lock().unwrap_or_else(std::sync::PoisonError::into_inner).forget(folder);
    }

    /// A path moved in the tree served, with what is in it, before whoever holds the project lists it so: shown at
    /// its new path at once, numbers kept. Whether the tree had it; where not, its number alone goes with it.
    pub fn moved(&self, from: &RelPath, to: &RelPath) -> bool {
        let Some((_, there)) = self.tree().at(from) else {
            self.rename(from, to);
            return false;
        };
        let before = self.tree();
        // A view of the shared filesystem is what that says: moved there by who serves it (`moved_there`).
        if before.shared.is_some() {
            return false;
        }
        drop(there);
        let mut tree = (*before).clone();
        {
            let mut inodes = self.inodes.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            tree.rename(from, to, &mut inodes);
        }
        self.put(tree);
        true
    }

    /// A path renamed: its inode number goes with it (`Inodes::rename`).
    pub fn rename(&self, from: &RelPath, to: &RelPath) {
        self.inodes.lock().unwrap_or_else(std::sync::PoisonError::into_inner).rename(from, to);
    }

    /// The path an inode number stands for.
    #[must_use]
    pub fn path(&self, ino: u64) -> Option<Arc<RelPath>> {
        self.inodes.lock().unwrap_or_else(std::sync::PoisonError::into_inner).path(ino).cloned()
    }

    /// The tree served now.
    #[must_use]
    pub fn tree(&self) -> Arc<Tree> {
        self.tree.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    /// Have what backends see of the files served told to a watcher, from now on.
    pub fn watch(&self, watcher: impl Fn(&RelPath, Seen) + Send + Sync + 'static) {
        *self.watcher.write().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::new(watcher));
    }

    /// A backend saw something of a file it serves: told to what watches.
    pub fn seen(&self, path: &RelPath, seen: Seen) {
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
    use super::{Changes, Inodes, Tree};
    use pe2_path::{Name, RelPath};
    use project::TreePath;
    use project::server::{Delta, Listing, Shown, Source};
    use sfs::testing::At as _;
    use std::sync::Arc;

    /// A path of the project a test writes.
    fn t(path: &str) -> Arc<TreePath> {
        TreePath::new(path).unwrap().to_owned()
    }

    /// A path of what is served, as a test writes it.
    fn p(path: &str) -> &RelPath {
        RelPath::new(path).unwrap()
    }

    /// A name a test writes.
    fn name(name: &str) -> &Name {
        Name::new(name).unwrap()
    }

    /// Paths as they are written.
    fn written<'a>(paths: impl IntoIterator<Item = &'a Arc<RelPath>>) -> Vec<&'a str> {
        paths.into_iter().map(|path| path.as_str()).collect()
    }

    fn file(disk: &str, size: u64, of_files: bool) -> Shown {
        let disk = sfs::testing::scratch("shown").at(disk.trim_start_matches('/'));
        let (modified, source) = ((1_700_000_000 + size, 0), Source::Disk);
        Shown::File { disk, size, modified, of_files, source }
    }

    #[test]
    fn what_changed_of_a_listing_is_what_changed_of_the_tree() {
        let before: Listing = [
            (t("a"), Shown::Folder),
            (t("a/n"), Shown::Node),
            (t("a/n/node.toml"), file("/l/a/n.node/node.toml", 3, true)),
            (t("a/n/glyphs/x.glif"), file("/l/a/n.node/glyphs/x.glif", 4, true)),
            (t("a/plain.txt"), file("/l/a/plain.txt", 5, false)),
            (t("b"), Shown::Folder),
            (t("b/old.txt"), file("/l/b/old.txt", 6, false)),
        ]
        .into();
        let mut after = before.clone();
        after.insert(t("a/plain.txt"), file("/l/a/plain.txt", 9, false));
        after.remove(&*t("a/n/glyphs/x.glif"));
        after.remove(&*t("b"));
        after.remove(&*t("b/old.txt"));
        after.insert(t("b"), file("/l/b", 2, false));
        after.insert(t("c"), Shown::Node);
        after.insert(t("c/sub/deep/y.glif"), file("/l/c.node/sub/deep/y.glif", 7, true));
        let changes = Changes::of(&before, &Delta::between(&before, &after), &after);
        assert_eq!(written(&changes.changed), ["a/plain.txt"]);
        // The folder a node's files made goes with the last of them; a folder where a file is now goes, and is new.
        let gone: Vec<(&str, bool)> = changes.gone.iter().map(|(path, folder)| (path.as_str(), *folder)).collect();
        assert_eq!(gone, [("a/n/glyphs", true), ("a/n/glyphs/x.glif", false), ("b", true), ("b/old.txt", false)]);
        // The folders a node's file makes come with it, outermost first.
        let new: Vec<(&str, bool)> = changes.new.iter().map(|(path, folder)| (path.as_str(), *folder)).collect();
        assert_eq!(
            new,
            [("b", false), ("c", true), ("c/sub", true), ("c/sub/deep", true), ("c/sub/deep/y.glif", false)]
        );
        // A folder that is an entry no more, with a node's file still in it: not gone. And an entry again: not new.
        let mut entry = before.clone();
        entry.insert(t("a/n/glyphs"), Shown::Folder);
        let back = Changes::of(&entry, &Delta::between(&entry, &before), &before);
        let forth = Changes::of(&before, &Delta::between(&before, &entry), &entry);
        assert!(back.is_empty() && forth.is_empty(), "{back:?} {forth:?}");
    }

    /// A tree that is a view of a shared filesystem holding this listing, with the space it is in.
    fn viewed(listing: &Listing) -> (std::sync::Arc<sfs::Space>, super::Served) {
        let space = std::sync::Arc::new(sfs::Space::private().unwrap());
        project::shared::say_whole(&space, listing, &pe2_io::SystemClock).unwrap();
        let served = super::Served::shared(space.clone(), None);
        (space, served)
    }

    #[test]
    fn a_view_of_the_shared_filesystem_is_the_tree_of_what_it_lists() {
        let listing: Listing = [
            (t("a"), Shown::Folder),
            (t("a/n"), Shown::Node),
            (t("a/n/node.toml"), file("/l/a/n.node/node.toml", 3, true)),
            (t("a/n/glyphs/x.glif"), file("/l/a/n.node/glyphs/x.glif", 4, true)),
            (t("a/plain.txt"), file("/l/a/plain.txt", 5, false)),
            (t("b"), Shown::Folder),
        ]
        .into();
        let (space, served) = viewed(&listing);
        let mut inodes = Inodes::default();
        let held = Tree::of_listing(&listing, &mut inodes);
        let tree = served.tree();
        assert_eq!(tree.len(), held.len());
        // Every node as a tree held has it - but its number, which is given as it is asked for, and what a folder
        // holds, which is asked of the tree.
        for node in held.nodes.values() {
            let (ino, viewed) = tree.at(&node.path).unwrap_or_else(|| panic!("{} is not shown", node.path));
            assert_eq!(
                (viewed.size, viewed.asset, viewed.of_files),
                (node.size, node.asset, node.of_files),
                "{}",
                node.path
            );
            assert_eq!(std::mem::discriminant(&viewed.what), std::mem::discriminant(&node.what));
            assert_eq!(served.path(ino).as_deref().map(RelPath::as_str), Some(node.path.as_str()));
            assert_eq!(tree.node(ino).map(|node| node.path.clone()), Some(node.path.clone()));
            assert_eq!(tree.children(&node.path), held.children(&node.path), "{}", node.path);
        }
        assert!(tree.at(p("a/none")).is_none() && tree.children(p("a/plain.txt")).is_none());
        let n = served.ino(p("a/n"));
        assert_eq!(tree.lookup(n, name("node.toml")), Some(served.ino(p("a/n/node.toml"))));
        // What whoever holds the project says next is what it shows, with no word to it.
        let mut next = listing.clone();
        next.remove(&*t("a/plain.txt"));
        next.insert(t("b/new.txt"), file("/l/b/new.txt", 8, false));
        project::shared::say(&space, &Delta::between(&listing, &next), &pe2_io::SystemClock).unwrap();
        assert!(served.tree().at(p("a/plain.txt")).is_none());
        assert_eq!(served.tree().at(p("b/new.txt")).map(|(_, node)| node.size), Some(8));
        assert_eq!(
            served
                .tree()
                .children(p("b"))
                .map(|names| names.into_keys().map(|name| name.to_string()).collect::<Vec<_>>()),
            Some(vec!["new.txt".to_string()])
        );
    }

    #[test]
    fn a_views_files_are_read_through_the_pages_kept_of_them() {
        let dir = sfs::testing::scratch("vfs-pages");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A file of some pages and a part of one, and an empty one.
        let content: Vec<u8> = (0..3 * 4096 + 1000u32).map(|n| u8::try_from(n % 251).unwrap()).collect();
        std::fs::write(dir.at("a.bin"), &content).unwrap();
        std::fs::write(dir.at("empty"), b"").unwrap();
        let shown = |name: &str, size: usize, at: u64| Shown::File {
            disk: dir.at(name),
            size: size as u64,
            modified: (at, 0),
            of_files: false,
            source: Source::Disk,
        };
        let listing: Listing =
            [(t("a.bin"), shown("a.bin", content.len(), 1)), (t("empty"), shown("empty", 0, 1))].into();
        let space = std::sync::Arc::new(sfs::Space::private().unwrap());
        project::shared::say_whole(&space, &listing, &pe2_io::SystemClock).unwrap();
        let ring = std::sync::Arc::new(sfs::ring::Ring::private(1 << 20).unwrap());
        let served = super::Served::shared(space.clone(), Some(ring));
        let (tree, a) = (served.tree(), served.ino(p("a.bin")));
        // Whatever is asked - within a page, across pages, past the end - is what the file holds there.
        for (offset, len) in
            [(0usize, 10usize), (4090, 20), (100, 9000), (12_000, 5000), (0, 1 << 20), (13_288, 10), (20_000, 10)]
        {
            let read = tree.read(a, offset as u64, len).unwrap();
            let end = content.len().min(offset + len);
            assert_eq!(read, &content[offset.min(end)..end], "from {offset}, {len}");
        }
        assert_eq!(tree.read(served.ino(p("empty")), 0, 100).unwrap(), b"");
        // Read from the pages kept: the file itself is gone, and all of it is read again.
        std::fs::remove_file(dir.at("a.bin")).unwrap();
        assert_eq!(tree.read(a, 0, 1 << 20).unwrap(), content);
        let mut part = [0u8; 300];
        assert_eq!(tree.read_into(a, 5000, &mut part).unwrap(), 300);
        assert_eq!(part[..], content[5000..5300]);
        // Another version of the file is another file to them: what was kept of the one before is not read as it.
        let other: Vec<u8> = content.iter().rev().copied().collect();
        std::fs::write(dir.at("a.bin"), &other).unwrap();
        let mut next = listing.clone();
        next.insert(t("a.bin"), shown("a.bin", other.len(), 2));
        project::shared::say(&space, &Delta::between(&listing, &next), &pe2_io::SystemClock).unwrap();
        assert_eq!(served.tree().read(a, 0, 1 << 20).unwrap(), other);
        assert_eq!(served.tree().read(a, 4000, 200).unwrap(), &other[4000..4200]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn what_is_moved_in_a_view_is_shown_so_until_the_project_says_so_itself() {
        let listing: Listing = [
            (t("a"), Shown::Folder),
            (t("a/x.txt"), file("/l/a/x.txt", 1, false)),
            (t("a/sub"), Shown::Folder),
            (t("a/sub/y.txt"), file("/l/a/sub/y.txt", 2, false)),
            (t("c"), Shown::Folder),
            (t("c/old.txt"), file("/l/c/old.txt", 9, false)),
            (t("d.txt"), file("/l/d.txt", 4, false)),
        ]
        .into();
        let (space, served) = viewed(&listing);
        let from = served.changes_from_now();
        let (x, y) = (served.ino(p("a/x.txt")), served.ino(p("a/sub/y.txt")));
        // Whoever serves the folder moves it in the shared filesystem, onto a folder that is there - what that held
        // is shown no more - and says so here.
        let moved = |from: &str, to: &str| {
            use sfs::fs::{ROOT, SERVED};
            let (from, to) = (p(from), p(to));
            let mut txn = space.write().unwrap();
            let replaced = sfs::fs::at(&txn, SERVED, to).unwrap();
            if let Some(there) = replaced {
                sfs::fs::remove(&mut txn, SERVED, there, project::shared::SERVER).unwrap();
            }
            let number = sfs::fs::at(&txn, SERVED, from).unwrap().unwrap();
            sfs::fs::rename(&mut txn, SERVED, number, ROOT, to.last().unwrap(), project::shared::SERVER).unwrap();
            txn.commit().unwrap();
            served.moved_there(from, to, true, replaced.map(|_| true));
        };
        moved("a", "c");
        let tree = served.tree();
        assert!(tree.at(p("a")).is_none() && tree.at(p("a/x.txt")).is_none() && tree.at(p("c/old.txt")).is_none());
        assert_eq!(tree.at(p("c/x.txt")).map(|(ino, node)| (ino, node.size)), Some((x, 1)));
        assert_eq!(tree.at(p("c/sub/y.txt")).map(|(ino, _)| ino), Some(y));
        assert_eq!(
            tree.children(p("")).map(|names| names.into_keys().map(|name| name.to_string()).collect::<Vec<_>>()),
            Some(vec!["c".to_string(), "d.txt".to_string()])
        );
        assert_eq!(
            tree.children(p("c")).map(|names| names.into_keys().map(|name| name.to_string()).collect::<Vec<_>>()),
            Some(vec!["sub".to_string(), "x.txt".to_string()])
        );
        // And moved on, and back - a rename the project refused.
        moved("c", "e");
        assert_eq!(served.tree().at(p("e/sub/y.txt")).map(|(ino, _)| ino), Some(y));
        assert!(served.tree().at(p("c")).is_none());
        moved("e", "c");
        assert!(served.tree().at(p("e")).is_none() && served.tree().at(p("c/x.txt")).is_some());
        // Those that tell of the tree were told each: gone where it was, new where it is.
        let mut cursor = from;
        let told = served.changes_since(&mut cursor).unwrap();
        assert_eq!(told.gone.iter().map(|(path, _)| path.as_str()).collect::<Vec<_>>(), ["a", "c", "c", "e"]);
        assert_eq!(told.new.iter().map(|(path, _)| path.as_str()).collect::<Vec<_>>(), ["c", "e", "c"]);
        assert!(served.changes_since(&mut cursor).unwrap().is_empty());
        // What was removed through the folder is shown no more, and listed no more, while the project has it.
        let mut txn = space.write().unwrap();
        let number = sfs::fs::at(&txn, sfs::fs::SERVED, p("d.txt")).unwrap().unwrap();
        let node = sfs::fs::node(&txn, sfs::fs::SERVED, number).unwrap().unwrap();
        let gone = sfs::fs::Node { flags: node.flags | project::shared::GONE, ..node };
        sfs::fs::set(&mut txn, sfs::fs::SERVED, number, &gone, project::shared::SERVER).unwrap();
        txn.commit().unwrap();
        assert!(served.tree().at(p("d.txt")).is_none());
        assert_eq!(
            served
                .tree()
                .children(p(""))
                .map(|names| names.into_keys().map(|name| name.to_string()).collect::<Vec<_>>()),
            Some(vec!["c".to_string()])
        );
    }

    #[test]
    fn a_path_moved_takes_what_is_in_it_and_keeps_its_numbers() {
        let served = super::Served::default();
        let bytes = |text: &str| project::content::Given::Bytes(std::sync::Arc::from(text.as_bytes()));
        let files = [("a/x.txt", "x"), ("a/sub/y.txt", "y"), ("b.txt", "b"), ("c/x.txt", "old")];
        served.set_files(&files.into_iter().map(|(path, text)| (p(path).to_owned(), bytes(text))).collect());
        let (x, y) = (served.ino(p("a/x.txt")), served.ino(p("a/sub/y.txt")));
        // Onto a folder that is there: what it held goes.
        assert!(served.moved(p("a"), p("c")));
        let tree = served.tree();
        assert!(tree.at(p("a")).is_none(), "nothing where it was");
        assert_eq!(
            tree.at(p("c/x.txt")).map(|(ino, node)| (ino, node.path.to_string())),
            Some((x, "c/x.txt".to_string()))
        );
        assert_eq!(tree.at(p("c/sub/y.txt")).map(|(ino, _)| ino), Some(y));
        assert_eq!(tree.read(x, 0, 8).unwrap(), b"x", "the file moved there, not the one that was");
        assert_eq!(served.path(y).as_deref().map(RelPath::as_str), Some("c/sub/y.txt"));
        // Into a folder not there yet, and a file alone.
        assert!(served.moved(p("b.txt"), p("made/for/it.txt")));
        assert!(served.tree().at(p("made/for/it.txt")).is_some() && served.tree().at(p("b.txt")).is_none());
        assert!(!served.moved(p("nothing"), p("anywhere")), "what is not there is not moved");
    }

    #[test]
    fn a_renamed_path_keeps_its_number_and_its_contents_theirs() {
        let mut inodes = Inodes::default();
        let (file, folder, inner, old) =
            (inodes.of(p("a/tmp1")), inodes.of(p("a/b")), inodes.of(p("a/b/c")), inodes.of(p("a/x")));
        inodes.rename(p("a/tmp1"), p("a/x"));
        assert_eq!(inodes.of(p("a/x")), file);
        assert_eq!(inodes.path(old), None);
        inodes.rename(p("a/b"), p("a/d"));
        assert_eq!((inodes.of(p("a/d")), inodes.of(p("a/d/c"))), (folder, inner));
        assert_eq!(inodes.path(inner).map(|path| path.as_str()), Some("a/d/c"));
    }
}
