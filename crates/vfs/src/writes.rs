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

/// How long the removal of a file the project was just given waits for the tree to show it, before it is given up.
const AWAITED: Duration = Duration::from_secs(30);

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
    /// Folders the project was given a moment ago, and until when each is shown whatever the tree says: where another
    /// process holds the project, the tree follows as that one lists it - a moment after the folder was made, and
    /// what makes a folder writes in it next.
    given: HashMap<String, Instant>,
    /// Paths hidden, and when their removal is applied (none: never, a backup's move awaiting the file's return).
    hidden: HashMap<String, Option<Instant>>,
    /// New files, and when they are applied.
    new: HashMap<String, Instant>,
    /// Open files by handle: their path.
    handles: HashMap<u64, String>,
    /// Paths whose content the owner is applying this moment: the project's any moment now, whatever is asked of them
    /// meanwhile - which waits for it (`applied`).
    applying: BTreeSet<String>,
    /// Paths removed while the project had them and the tree did not show them yet, and when to stop waiting for it to:
    /// their removal is applied once it does.
    awaited: HashMap<String, Instant>,
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
    /// Held by what applies what has settled: one at a time, so that nothing is given to the project twice.
    settling: Arc<Mutex<()>>,
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
        let writes = Writes {
            served: served.clone(),
            owner,
            dir: dir.to_path_buf(),
            state: Arc::default(),
            settling: Arc::default(),
        };
        // What has settled is applied off the filesystem's threads, for as long as the writes are kept.
        let (served, owner, dir, weak) =
            (writes.served.clone(), Arc::downgrade(&writes.owner), writes.dir.clone(), Arc::downgrade(&writes.state));
        let settling = writes.settling.clone();
        let _ = std::thread::Builder::new().name("filesystem writes".into()).spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(200));
                let (Some(state), Some(owner)) = (weak.upgrade(), owner.upgrade()) else { return };
                Writes { served: served.clone(), owner, dir: dir.clone(), state, settling: settling.clone() }.settle();
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
        if state.folders.contains(path) || (state.given.contains_key(path) && tree.at(path).is_none()) {
            return Some(Shown { folder: true, size: 0, modified: SystemTime::now() });
        }
        let (_, node) = tree.at(path)?;
        Some(Shown { folder: matches!(node.what, What::Folder(_)), size: node.size, modified: node.modified })
    }

    /// Whether a path is the tree's alone: nothing staged over it, hidden or waiting. What it is then changes only as
    /// the tree does, which is told to whatever caches it.
    #[must_use]
    pub fn settled(&self, path: &str) -> bool {
        let state = self.lock();
        !(state.staged.contains_key(path)
            || state.hidden.contains_key(path)
            || state.new.contains_key(path)
            || state.folders.contains(path)
            || state.given.contains_key(path))
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
        let folders = state.folders.iter().chain(state.given.keys());
        for (staged, folder) in state.staged.keys().map(|p| (p, false)).chain(folders.map(|p| (p, true))) {
            if parent(staged) == path {
                out.insert(staged.rsplit('/').next().unwrap_or(staged).to_string(), folder);
            }
        }
        out
    }

    /// The names written or made in a folder since the tree was told: few, whatever the folder holds.
    #[must_use]
    pub fn staged_in(&self, folder: &str) -> Vec<String> {
        let state = self.lock();
        let made = state.folders.iter().chain(state.given.keys());
        let staged = state.staged.keys().chain(made).filter(|path| parent(path) == folder);
        staged.map(|path| path.rsplit('/').next().unwrap_or(path).to_string()).collect()
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

    /// Bytes of a file from `offset` read into a buffer, as many as it takes or the file has: staged, or the tree's.
    ///
    /// # Errors
    /// It is not there, or cannot be read.
    pub fn read_into(&self, path: &str, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
        let staged = self.lock().staged.get(path).map(|s| s.file.clone());
        if let Some(file) = staged {
            let mut opened = File::open(file)?;
            opened.seek(SeekFrom::Start(offset))?;
            let mut read = 0;
            while read < buffer.len() {
                match opened.read(&mut buffer[read..])? {
                    0 => break,
                    count => read += count,
                }
            }
            return Ok(read);
        }
        let tree = self.served.tree();
        let (ino, _) = tree.at(path).ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        tree.read_into(ino, offset, buffer)
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
        self.lock().applying.insert(path.to_string());
        let applied = self.apply(Change::Write { path: path.to_string(), staged: file });
        let mut state = self.lock();
        state.applying.remove(path);
        applied?;
        if let Some(staged) = state.staged.get_mut(path) {
            staged.dirty = false;
            staged.applied = Some(SystemTime::now());
        }
        Ok(())
    }

    /// Give the project the new files that have settled, together (`Owner::changes`): what `apply_written` does for
    /// one, for all that were written and are no junk.
    fn apply_new(&self, paths: &[String]) {
        let mut given = Vec::new();
        {
            let mut state = self.lock();
            for path in paths {
                state.new.remove(path);
                let Some(staged) = state.staged.get(path).filter(|staged| staged.dirty && !is_junk(path)) else {
                    continue;
                };
                given.push((path.clone(), staged.file.clone()));
                state.applying.insert(path.clone());
            }
        }
        if given.is_empty() {
            return;
        }
        let asked = given.iter().map(|(path, file)| Change::Write { path: path.clone(), staged: file.clone() });
        let applied = self.owner.changes(asked.collect());
        let mut state = self.lock();
        for ((path, _), applied) in given.into_iter().zip(applied) {
            state.applying.remove(&path);
            if applied.is_ok()
                && let Some(staged) = state.staged.get_mut(&path)
            {
                staged.dirty = false;
                staged.applied = Some(SystemTime::now());
            }
        }
    }

    /// Wait until the owner is done applying a path's content, where it is this moment: what is asked of the path next
    /// - its removal, its renaming - is then asked of a file the project has, not of one it is about to be given.
    fn applied(&self, path: &str) {
        while self.lock().applying.contains(path) {
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn apply(&self, change: Change) -> io::Result<()> {
        self.owner.change(change).map_err(io::Error::other)
    }

    /// Remove a file or an empty folder: hidden now, removed when it settles. What is staged alone goes at once.
    ///
    /// # Errors
    /// It is not there.
    pub fn remove(&self, path: &str) -> io::Result<()> {
        self.applied(path);
        let in_tree = self.served.tree().at(path).is_some();
        let mut state = self.lock();
        let staged = state.staged.remove(path);
        let folder = state.folders.remove(path);
        // A folder the project was given, the tree not showing it yet: the project has it, as a file applied.
        let given = state.given.remove(path).is_some();
        state.new.remove(path);
        if let Some(staged) = &staged {
            let _ = std::fs::remove_file(&staged.file);
        }
        let applied = given || staged.as_ref().is_some_and(|s| s.applied.is_some());
        if in_tree || applied {
            let due = Instant::now() + SETTLE;
            state.hidden.insert(path.to_string(), Some(due));
            // The project has it, and the tree may not show it yet: its removal waits for the tree to.
            if !in_tree {
                state.awaited.insert(path.to_string(), due + AWAITED);
            }
        } else if staged.is_none() && !folder {
            return Err(io::Error::from(io::ErrorKind::NotFound));
        }
        Ok(())
    }

    /// Make a folder: in a folder, at once; in a node, waiting for what is written in it; where one was removed a
    /// moment ago, by keeping that one.
    ///
    /// # Errors
    /// Its name is refused, or the owner could not apply it.
    pub fn make_folder(&self, path: &str) -> io::Result<()> {
        if crate::write::refused(path) {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        // Made where one was removed a moment ago, its removal not applied yet: it is not removed after all. What was
        // removed from it stays removed.
        {
            let mut state = self.lock();
            if matches!(state.hidden.get(path), Some(Some(_))) && self.served.tree().at(path).is_some() {
                state.hidden.remove(path);
                state.awaited.remove(path);
                return Ok(());
            }
        }
        if self.in_node(path) || is_junk(path) {
            self.lock().folders.insert(path.to_string());
            return Ok(());
        }
        self.apply(Change::Folder { path: path.to_string() })?;
        // Shown from now on, whenever the tree comes to show it.
        if self.served.tree().at(path).is_none() {
            self.lock().given.insert(path.to_string(), Instant::now() + AWAITED);
        }
        Ok(())
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
            // A folder the project was given at once is a plain one, as it was when it was made.
            if state.given.contains_key(parent) {
                return false;
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
        self.applied(from);
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

    /// Apply what waits to settle, now: what serving does as it ends, so that what a program did a moment before is
    /// the project's too. A file still open is left, as is what waits for more than time.
    pub fn finish(&self) {
        {
            let mut state = self.lock();
            let now = Instant::now();
            for due in state.new.values_mut() {
                *due = now;
            }
            for due in state.hidden.values_mut().flatten() {
                *due = now;
            }
        }
        self.settle();
    }

    /// Apply what has settled - new files, removals - and let go of what the tree served now shows.
    pub fn settle(&self) {
        // One at a time: what settles is found and given under it, by the thread that does so as it goes and by
        // what gives all of it as serving ends.
        let _settling = self.settling.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        let due_new: Vec<String> = {
            let state = self.lock();
            let open: BTreeSet<&String> = state.handles.values().collect();
            state.new.iter().filter(|(p, due)| **due <= now && !open.contains(p)).map(|(p, _)| p.clone()).collect()
        };
        // Declarations first: what is written beside one waits for its folder to be a node.
        let mut due_new = due_new;
        due_new.sort_by_key(|p| (!p.ends_with(&format!("/{}", project::index::NODE_FILE)), p.clone()));
        due_new.retain(|path| !self.waits_for_its_node(path));
        // Given to the project together: each waits for nothing the others make.
        self.apply_new(&due_new);
        for path in &due_new {
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
        let mut removing = Vec::new();
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
            let in_tree = self.served.tree().at(&path).is_some();
            if !covered && !in_tree && self.lock().awaited.get(&path).is_some_and(|until| *until > now) {
                // The project was given it a moment ago, and the tree does not show it yet: removed once it does.
                continue;
            }
            if !covered && in_tree {
                removing.push(path);
                continue;
            }
            let mut state = self.lock();
            state.awaited.remove(&path);
            state.hidden.insert(path, None);
        }
        // Removed from the project together.
        let asked = removing.iter().map(|path| Change::Remove { path: path.clone() }).collect();
        for (path, removed) in removing.into_iter().zip(self.owner.changes(asked)) {
            let mut state = self.lock();
            state.awaited.remove(&path);
            if removed.is_ok() {
                state.hidden.insert(path, None);
            } else {
                // Refused: it is shown again.
                state.hidden.remove(&path);
            }
        }
        // What the tree has now needs no showing over it.
        let tree = self.served.tree();
        let mut state = self.lock();
        state.hidden.retain(|path, due| due.is_some() || tree.at(path).is_some());
        state.given.retain(|path, until| tree.at(path).is_none() && *until > now);
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

#[cfg(test)]
mod tests {
    use super::Writes;
    use crate::Served;
    use crate::write::{Change, Owner};
    use project::content::Given;
    use std::collections::BTreeMap;
    use std::sync::mpsc::{Receiver, Sender, channel};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    /// A project of files held in memory, which takes its time to be given one: it says when it starts to, and goes on
    /// when told.
    struct Slow {
        served: Served,
        files: Mutex<BTreeMap<String, Given>>,
        started: Mutex<Sender<()>>,
        go_on: Mutex<Receiver<()>>,
        changes: Mutex<Vec<String>>,
    }

    impl Owner for Slow {
        fn change(&self, change: Change) -> Result<(), String> {
            match &change {
                Change::Write { path, staged } => {
                    let _ = self.started.lock().unwrap().send(());
                    let _ = self.go_on.lock().unwrap().recv();
                    let bytes = std::fs::read(staged).map_err(|e| e.to_string())?;
                    let mut files = self.files.lock().unwrap();
                    files.insert(path.clone(), Given::Bytes(Arc::from(bytes)));
                    self.served.set_files(&files);
                }
                Change::Remove { path } => {
                    let mut files = self.files.lock().unwrap();
                    files.remove(path);
                    self.served.set_files(&files);
                }
                _ => {}
            }
            self.changes.lock().unwrap().push(change.label());
            Ok(())
        }
    }

    /// A project that only says what it was given, and how many at a time.
    #[derive(Default)]
    struct Told {
        batches: Mutex<Vec<Vec<String>>>,
    }

    impl Owner for Told {
        fn change(&self, change: Change) -> Result<(), String> {
            self.batches.lock().unwrap().push(vec![change.label()]);
            Ok(())
        }

        fn changes(&self, changes: Vec<Change>) -> Vec<Result<(), String>> {
            self.batches.lock().unwrap().push(changes.iter().map(Change::label).collect());
            changes.iter().map(|_| Ok(())).collect()
        }
    }

    #[test]
    fn new_files_that_have_settled_are_given_to_the_project_together() {
        let dir = std::env::temp_dir().join(format!("pe2-writes-many-{}", std::process::id()));
        let served = Served::default();
        served.set_files(&BTreeMap::new());
        let owner = Arc::new(Told::default());
        let writes = Writes::new(&served, owner.clone(), &dir).unwrap();
        for name in ["a.txt", "b.txt", "c.txt"] {
            let handle = writes.create(name).unwrap();
            writes.write(handle, 0, b"x").unwrap();
            writes.close(handle, true).unwrap();
        }
        // All that waits is given at once - as when it has settled by the same moment, which a machine doing other
        // things does not promise of three files made one after another.
        writes.finish();
        let batches = owner.batches.lock().unwrap().clone();
        let writes_given: Vec<&Vec<String>> = batches.iter().filter(|batch| !batch.is_empty()).collect();
        assert_eq!(
            writes_given,
            [&vec![
                "Write a.txt from outside".to_string(),
                "Write b.txt from outside".to_string(),
                "Write c.txt from outside".to_string()
            ]],
            "one batch of the three"
        );
        drop(writes);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn what_waits_to_settle_is_given_to_the_project_as_serving_ends() {
        let dir = std::env::temp_dir().join(format!("pe2-writes-finish-{}", std::process::id()));
        let served = Served::default();
        served.set_files(&BTreeMap::new());
        let owner = Arc::new(Told::default());
        let writes = Writes::new(&served, owner.clone(), &dir).unwrap();
        let handle = writes.create("late.txt").unwrap();
        writes.write(handle, 0, b"x").unwrap();
        writes.close(handle, true).unwrap();
        // At once, long before it would have settled.
        writes.finish();
        let given: Vec<String> = owner.batches.lock().unwrap().iter().flatten().cloned().collect();
        assert_eq!(given, ["Write late.txt from outside"]);
        drop(writes);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_folder_the_project_was_given_is_shown_before_the_tree_follows() {
        let dir = std::env::temp_dir().join(format!("pe2-writes-given-{}", std::process::id()));
        let served = Served::default();
        served.set_files(&BTreeMap::new());
        // An owner that takes what it is given, the tree served following in its own time - as where another process
        // holds the project, and lists what is served a moment later.
        let owner = Arc::new(Told::default());
        let writes = Writes::new(&served, owner.clone(), &dir).unwrap();
        writes.make_folder("made").expect("made");
        assert!(writes.shown("made").is_some_and(|shown| shown.folder), "there at once");
        assert_eq!(writes.list("").keys().collect::<Vec<_>>(), ["made"]);
        // What is written in it next finds it, and is the project's with it.
        let handle = writes.create("made/a.txt").unwrap();
        writes.write(handle, 0, b"x").unwrap();
        writes.close(handle, true).unwrap();
        writes.finish();
        let given: Vec<String> = owner.batches.lock().unwrap().iter().flatten().cloned().collect();
        assert_eq!(given, ["New made from outside", "Write a.txt from outside"]);
        // Once the tree shows it, it is the tree's.
        served.set_files(&BTreeMap::from([("made/a.txt".to_string(), Given::Bytes(Arc::from(&b"x"[..])))]));
        writes.settle();
        assert!(writes.settled("made"), "nothing shown over the tree any more");
        drop(writes);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_folder_made_where_one_was_just_removed_is_kept() {
        let dir = std::env::temp_dir().join(format!("pe2-writes-folder-{}", std::process::id()));
        let served = Served::default();
        let files = BTreeMap::from([("kept/a.txt".to_string(), Given::Bytes(Arc::from(&b"a"[..])))]);
        served.set_files(&files);
        let (started, _starts) = channel();
        let (_goes_on, go_on) = channel();
        let owner = Arc::new(Slow {
            served: served.clone(),
            files: Mutex::new(files),
            started: Mutex::new(started),
            go_on: Mutex::new(go_on),
            changes: Mutex::default(),
        });
        let writes = Writes::new(&served, owner.clone(), &dir).unwrap();
        // Emptied and removed, then made again before the removal has settled: as a program replacing a folder does.
        writes.remove("kept/a.txt").unwrap();
        writes.remove("kept").unwrap();
        assert!(writes.shown("kept").is_none(), "hidden at once");
        writes.make_folder("kept").expect("made again");
        assert!(writes.shown("kept").is_some_and(|shown| shown.folder), "shown again");
        assert!(writes.list("kept").is_empty(), "without what was removed from it");
        // Once the removal has settled - however long that takes a machine doing other things - and then for as long
        // as the folder's own would have taken to follow it.
        let waited = std::time::Instant::now();
        while owner.changes.lock().unwrap().is_empty() && waited.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(50));
        }
        std::thread::sleep(super::SETTLE + Duration::from_millis(600));
        let changes = owner.changes.lock().unwrap().clone();
        assert_eq!(changes, ["Remove a.txt from outside"], "only its file is removed from the project");
        drop(writes);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_removed_while_the_project_is_given_it_is_removed_from_the_project() {
        let dir = std::env::temp_dir().join(format!("pe2-writes-{}", std::process::id()));
        let served = Served::default();
        served.set_files(&BTreeMap::new());
        let (started, starts) = channel();
        let (goes_on, go_on) = channel();
        let owner = Arc::new(Slow {
            served: served.clone(),
            files: Mutex::default(),
            started: Mutex::new(started),
            go_on: Mutex::new(go_on),
            changes: Mutex::default(),
        });
        let writes = Writes::new(&served, owner.clone(), &dir).unwrap();
        // A new file, written and let go of: given to the project once it has settled.
        let handle = writes.create("a.txt").unwrap();
        writes.write(handle, 0, b"written").unwrap();
        writes.close(handle, true).unwrap();
        starts.recv_timeout(Duration::from_secs(5)).expect("the project is being given the file");
        // Removed at that moment: the removal waits for the project to have it, rather than taking the file for one
        // only staged - which would leave it in the project.
        let removing = {
            let writes = writes.clone();
            std::thread::spawn(move || writes.remove("a.txt"))
        };
        std::thread::sleep(Duration::from_millis(100));
        assert!(!removing.is_finished(), "the removal waits for the file to be the project's");
        goes_on.send(()).unwrap();
        removing.join().unwrap().expect("removed");
        assert!(writes.shown("a.txt").is_none(), "hidden at once");
        let asked = Instant::now();
        let removed = || owner.changes.lock().unwrap().iter().any(|c| c == "Remove a.txt from outside");
        while !removed() && asked.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(removed(), "removed from the project: {:?}", owner.changes.lock().unwrap());
        assert!(served.tree().at("a.txt").is_none() && writes.shown("a.txt").is_none(), "and gone");
        drop(writes);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
