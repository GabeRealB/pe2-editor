//! Writing through the filesystem: what programs write is staged (`staging`, a folder of the project's cache) and shown
//! over the tree served, so a program reads back what it wrote at once; the owner applies it to the project as programs
//! finish with it (`write::Change`).
//!
//! - A file the project has is applied when the program closes it (or syncs it) having written it.
//! - A file the project does not have is applied a moment after the program last touched it (`SETTLE`): one written
//!   under a temporary name and renamed over the file it saves is a save of that file, never a file of its own.
//! - A file removed is hidden at once and removed from the project a moment later, unless a file is made there
//!   meanwhile: removing a file and writing it again is one save.
//! - Junk (`write::is_junk`: swap files, backups, any name starting with `.`) stays staged, never applied; it goes
//!   with the staging. A name ending in `.node` - storage's mark of a node - is refused.
//! - A folder made in a folder is applied at once. One made in a node waits for what is written in it: a `node.toml`
//!   makes it a node, other files make it a folder of the node's files. Files written beside a new `node.toml` wait for
//!   the tree to show its folder a node, so they are its files.
//! - Removing a folder removes what is in it as one: a node's folder deletes the node, with what is inside it. A
//!   removal the project refuses (a node's `node.toml` alone) shows the file again.
//!
//! What is staged and applied stays shown over the tree until the tree served has it (its length and time).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use crate::write::{Change, Owner, is_junk};
use crate::{Served, What};

/// How long a new file or a removal waits for what follows it before it is applied.
pub const SETTLE: Duration = Duration::from_millis(1000);

/// A file staged: where its content is, whether it was written since it was last applied, whether the project has it,
/// and when it was applied.
#[derive(Debug)]
struct Staged {
    file: PathBuf,
    dirty: bool,
    applied: Option<SystemTime>,
}

#[derive(Default, Debug)]
struct State {
    staged: HashMap<String, Staged>,
    /// Folders made in nodes, waiting for what is written in them.
    folders: BTreeSet<String>,
    /// Paths hidden, and when their removal is applied (none: never, a backup's move awaiting the file's return).
    hidden: HashMap<String, Option<Instant>>,
    /// New files, and when they are applied.
    new: HashMap<String, Instant>,
    /// Open files by handle: their path.
    handles: HashMap<u64, String>,
    next: u64,
}

/// What a path is, as the filesystem shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shown {
    pub folder: bool,
    pub size: u64,
    pub modified: SystemTime,
}

/// The filesystem's writes: staged, shown, applied.
#[derive(Clone)]
pub struct Writes {
    served: Served,
    owner: Arc<dyn Owner>,
    dir: PathBuf,
    state: Arc<Mutex<State>>,
}

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(p, _)| p)
}

impl Writes {
    /// Writes staged in `dir` (emptied first), applied by `owner`; a thread applies what has settled, for as long as
    /// any clone of them lives.
    ///
    /// # Errors
    /// The staging folder cannot be made.
    pub fn new(served: &Served, owner: Arc<dyn Owner>, dir: &Path) -> io::Result<Writes> {
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir)?;
        let writes = Writes { served: served.clone(), owner, dir: dir.to_path_buf(), state: Arc::default() };
        // What has settled is applied off the filesystem's threads, for as long as the writes are kept.
        let (served, owner, dir, weak) =
            (writes.served.clone(), Arc::downgrade(&writes.owner), writes.dir.clone(), Arc::downgrade(&writes.state));
        let _ = std::thread::Builder::new().name("filesystem writes".into()).spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(200));
                let (Some(state), Some(owner)) = (weak.upgrade(), owner.upgrade()) else { return };
                Writes { served: served.clone(), owner, dir: dir.clone(), state }.settle();
            }
        });
        Ok(writes)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// What a path is: staged over the tree, hidden, or the tree's.
    #[must_use]
    pub fn shown(&self, path: &str) -> Option<Shown> {
        let tree = self.served.tree();
        let state = self.lock();
        if state.hidden.contains_key(path) {
            return None;
        }
        if let Some(staged) = state.staged.get(path) {
            let meta = std::fs::metadata(&staged.file).ok()?;
            return Some(Shown { folder: false, size: meta.len(), modified: meta.modified().ok()? });
        }
        if state.folders.contains(path) {
            return Some(Shown { folder: true, size: 0, modified: SystemTime::now() });
        }
        let (_, node) = tree.at(path)?;
        Some(Shown { folder: matches!(node.what, What::Folder(_)), size: node.size, modified: node.modified })
    }

    /// A folder's children as shown: the tree's, less what is hidden, with what is staged in it.
    #[must_use]
    pub fn list(&self, path: &str) -> BTreeMap<String, bool> {
        let tree = self.served.tree();
        let mut out: BTreeMap<String, bool> = BTreeMap::new();
        if let Some((_, node)) = tree.at(path)
            && let What::Folder(children) = &node.what
        {
            for (name, ino) in children {
                out.insert(name.clone(), matches!(tree.node(*ino).map(|n| &n.what), Some(What::Folder(_))));
            }
        }
        let state = self.lock();
        let join = |name: &str| if path.is_empty() { name.to_string() } else { format!("{path}/{name}") };
        out.retain(|name, _| !state.hidden.contains_key(&join(name)));
        for (staged, folder) in state.staged.keys().map(|p| (p, false)).chain(state.folders.iter().map(|p| (p, true))) {
            if parent(staged) == path {
                out.insert(staged.rsplit('/').next().unwrap_or(staged).to_string(), folder);
            }
        }
        out
    }

    /// Up to `len` bytes of a file from `offset`: staged, or the tree's.
    ///
    /// # Errors
    /// It is not there, or cannot be read.
    pub fn read(&self, path: &str, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        let staged = self.lock().staged.get(path).map(|s| s.file.clone());
        if let Some(file) = staged {
            let mut opened = File::open(file)?;
            opened.seek(SeekFrom::Start(offset))?;
            let mut out = Vec::new();
            opened.take(len as u64).read_to_end(&mut out)?;
            return Ok(out);
        }
        let tree = self.served.tree();
        let (ino, _) = tree.at(path).ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        tree.read(ino, offset, len)
    }

    /// A staging file of a path: its content now - the tree's, unless `empty` - copied in where it is not staged yet.
    fn stage(&self, path: &str, empty: bool) -> io::Result<()> {
        if self.lock().staged.contains_key(path) {
            if empty {
                let file = self.lock().staged.get(path).map(|s| s.file.clone());
                if let Some(file) = file {
                    File::create(file)?;
                }
            }
            return Ok(());
        }
        let file = {
            let mut state = self.lock();
            state.next += 1;
            self.dir.join(format!("{}", state.next))
        };
        let mut out = File::create(&file)?;
        if !empty && let Some((ino, node)) = self.served.tree().at(path) {
            let (tree, mut at) = (self.served.tree(), 0u64);
            while at < node.size {
                let bytes = tree.read(ino, at, 1 << 22)?;
                if bytes.is_empty() {
                    break;
                }
                out.write_all(&bytes)?;
                at += bytes.len() as u64;
            }
        }
        self.lock().staged.insert(path.to_string(), Staged { file, dirty: false, applied: None });
        Ok(())
    }

    /// Open a file for writing (`truncate`: emptied); its handle.
    ///
    /// # Errors
    /// It cannot be staged.
    pub fn open(&self, path: &str, truncate: bool) -> io::Result<u64> {
        self.stage(path, truncate)?;
        let mut state = self.lock();
        if truncate && let Some(staged) = state.staged.get_mut(path) {
            staged.dirty = true;
        }
        state.next += 1;
        let handle = state.next;
        state.handles.insert(handle, path.to_string());
        Ok(handle)
    }

    /// Make a file, empty, open for writing; its handle. A file made where one was removed a moment ago saves it.
    ///
    /// # Errors
    /// It cannot be staged.
    pub fn create(&self, path: &str) -> io::Result<u64> {
        if crate::write::refused(path) {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        let returning = self.lock().hidden.remove(path).is_some();
        let exists = returning || self.served.tree().at(path).is_some();
        let handle = self.open(path, true)?;
        if !exists && !is_junk(path) {
            self.lock().new.insert(path.to_string(), Instant::now() + SETTLE);
        }
        Ok(handle)
    }

    /// Write at an offset of an open file.
    ///
    /// # Errors
    /// The handle is not open, or the staging file cannot be written.
    pub fn write(&self, handle: u64, offset: u64, data: &[u8]) -> io::Result<usize> {
        let file = {
            let mut state = self.lock();
            let path = state.handles.get(&handle).cloned().ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
            if let Some(due) = state.new.get_mut(&path) {
                *due = Instant::now() + SETTLE;
            }
            let staged = state.staged.get_mut(&path).ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
            staged.dirty = true;
            staged.file.clone()
        };
        let mut opened = OpenOptions::new().write(true).open(file)?;
        opened.seek(SeekFrom::Start(offset))?;
        opened.write_all(data)?;
        Ok(data.len())
    }

    /// Set a file's length: staged, applied as a write.
    ///
    /// # Errors
    /// It cannot be staged.
    pub fn truncate(&self, path: &str, len: u64) -> io::Result<()> {
        self.stage(path, false)?;
        let file = {
            let mut state = self.lock();
            let staged = state.staged.get_mut(path).ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
            staged.dirty = true;
            staged.file.clone()
        };
        OpenOptions::new().write(true).open(file)?.set_len(len)
    }

    /// A handle let go of, or a file synced: what it wrote applied, where the project has the file.
    ///
    /// # Errors
    /// The owner could not apply it.
    pub fn close(&self, handle: u64, release: bool) -> io::Result<()> {
        let path = {
            let mut state = self.lock();
            if release { state.handles.remove(&handle) } else { state.handles.get(&handle).cloned() }
        };
        match path {
            Some(path) if !self.lock().new.contains_key(&path) => self.apply_written(&path),
            _ => Ok(()),
        }
    }

    /// Apply a staged file's content, where it was written and is no junk.
    fn apply_written(&self, path: &str) -> io::Result<()> {
        let file = {
            let state = self.lock();
            match state.staged.get(path) {
                Some(staged) if staged.dirty && !is_junk(path) => staged.file.clone(),
                _ => return Ok(()),
            }
        };
        self.apply(Change::Write { path: path.to_string(), staged: file })?;
        if let Some(staged) = self.lock().staged.get_mut(path) {
            staged.dirty = false;
            staged.applied = Some(SystemTime::now());
        }
        Ok(())
    }

    fn apply(&self, change: Change) -> io::Result<()> {
        self.owner.change(change).map_err(io::Error::other)
    }

    /// Remove a file or an empty folder: hidden now, removed when it settles. What is staged alone goes at once.
    ///
    /// # Errors
    /// It is not there.
    pub fn remove(&self, path: &str) -> io::Result<()> {
        let in_tree = self.served.tree().at(path).is_some();
        let mut state = self.lock();
        let staged = state.staged.remove(path);
        let folder = state.folders.remove(path);
        state.new.remove(path);
        if let Some(staged) = &staged {
            let _ = std::fs::remove_file(&staged.file);
        }
        let applied = staged.as_ref().is_some_and(|s| s.applied.is_some());
        if in_tree || applied {
            state.hidden.insert(path.to_string(), Some(Instant::now() + SETTLE));
        } else if staged.is_none() && !folder {
            return Err(io::Error::from(io::ErrorKind::NotFound));
        }
        Ok(())
    }

    /// Make a folder: in a folder, at once; in a node, waiting for what is written in it.
    ///
    /// # Errors
    /// Its name is refused, or the owner could not apply it.
    pub fn make_folder(&self, path: &str) -> io::Result<()> {
        if crate::write::refused(path) {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        if self.in_node(path) || is_junk(path) {
            self.lock().folders.insert(path.to_string());
            return Ok(());
        }
        self.apply(Change::Folder { path: path.to_string() })
    }

    /// Whether a new path is inside a node: its nearest folder shown is a node, one of a node's files, or one made in
    /// a node and waiting.
    fn in_node(&self, path: &str) -> bool {
        let tree = self.served.tree();
        let state = self.lock();
        let mut at = path;
        while let Some((parent, _)) = at.rsplit_once('/') {
            if state.folders.contains(parent) {
                return true;
            }
            if let Some((_, node)) = tree.at(parent) {
                return node.asset || node.of_files;
            }
            at = parent;
        }
        false
    }

    /// Rename: a staged file is applied under its new name - a save, where a file is there - or waits there as new; a
    /// file renamed to junk (a backup) is staged there and hidden, its removal waiting on the file's return; anything
    /// else is moved in the project.
    ///
    /// # Errors
    /// The owner could not apply it, or nothing is there.
    pub fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        if crate::write::refused(to) {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        // The kernel keeps the renamed node's number for its new name.
        self.served.rename(from, to);
        let staged = {
            let mut state = self.lock();
            state.new.remove(from);
            state.staged.remove(from)
        };
        if let Some(mut staged) = staged {
            let exists = {
                let mut state = self.lock();
                state.hidden.remove(to).is_some()
            } || self.served.tree().at(to).is_some();
            staged.dirty = true;
            self.lock().staged.insert(to.to_string(), staged);
            if is_junk(to) {
                return Ok(());
            }
            if exists {
                return self.apply_written(to);
            }
            self.lock().new.insert(to.to_string(), Instant::now() + SETTLE);
            return Ok(());
        }
        if is_junk(to) {
            self.stage(from, false)?;
            let mut state = self.lock();
            if let Some(copy) = state.staged.remove(from) {
                state.staged.insert(to.to_string(), Staged { dirty: false, ..copy });
            }
            state.hidden.insert(from.to_string(), Some(Instant::now() + SETTLE * 5));
            return Ok(());
        }
        self.apply(Change::Rename { from: from.to_string(), to: to.to_string() })
    }

    /// Apply what has settled - new files, removals - and let go of what the tree served now shows.
    pub fn settle(&self) {
        let now = Instant::now();
        let due_new: Vec<String> = {
            let state = self.lock();
            let open: BTreeSet<&String> = state.handles.values().collect();
            state.new.iter().filter(|(p, due)| **due <= now && !open.contains(p)).map(|(p, _)| p.clone()).collect()
        };
        // Declarations first: what is written beside one waits for its folder to be a node.
        let mut due_new = due_new;
        due_new.sort_by_key(|p| (!p.ends_with(&format!("/{}", project::index::NODE_FILE)), p.clone()));
        for path in due_new {
            if self.waits_for_its_node(&path) {
                continue;
            }
            self.lock().new.remove(&path);
            let _ = self.apply_written(&path);
            // A folder waiting in a node is settled by what was written in it.
            let mut state = self.lock();
            let mut at = path.as_str();
            while let Some((parent, _)) = at.rsplit_once('/') {
                state.folders.remove(parent);
                at = parent;
            }
        }
        let due_removals: Vec<String> = {
            let state = self.lock();
            state.hidden.iter().filter(|(_, due)| due.is_some_and(|d| d <= now)).map(|(p, _)| p.clone()).collect()
        };
        for path in due_removals {
            // What is in a folder being removed goes with it.
            let covered = {
                let state = self.lock();
                let mut at = path.as_str();
                let mut covered = false;
                while let Some((parent, _)) = at.rsplit_once('/') {
                    covered |= state.hidden.contains_key(parent);
                    at = parent;
                }
                covered
            };
            if !covered
                && self.served.tree().at(&path).is_some()
                && self.apply(Change::Remove { path: path.clone() }).is_err()
            {
                // Refused: it is shown again.
                self.lock().hidden.remove(&path);
                continue;
            }
            self.lock().hidden.insert(path, None);
        }
        // What the tree has now needs no showing over it.
        let tree = self.served.tree();
        let mut state = self.lock();
        state.hidden.retain(|path, due| due.is_some() || tree.at(path).is_some());
        let open: BTreeSet<String> = state.handles.values().cloned().collect();
        state.staged.retain(|path, staged| {
            let Some(applied) = staged.applied.filter(|_| !staged.dirty && !open.contains(path)) else { return true };
            let shown = tree.at(path).is_some_and(|(_, node)| {
                let len = std::fs::metadata(&staged.file).map(|m| m.len()).ok();
                Some(node.size) == len && node.modified + Duration::from_secs(2) >= applied
            });
            if shown {
                let _ = std::fs::remove_file(&staged.file);
            }
            !shown
        });
    }

    /// Whether a new file waits for the `node.toml` beside it to make its folder a node.
    fn waits_for_its_node(&self, path: &str) -> bool {
        let Some((folder, name)) = path.rsplit_once('/') else { return false };
        if name == project::index::NODE_FILE {
            return false;
        }
        let declared = self.lock().staged.contains_key(&format!("{folder}/{}", project::index::NODE_FILE));
        declared && !self.served.tree().at(folder).is_some_and(|(_, n)| n.asset)
    }

    /// The tree served.
    #[must_use]
    pub fn served(&self) -> &Served {
        &self.served
    }
}
