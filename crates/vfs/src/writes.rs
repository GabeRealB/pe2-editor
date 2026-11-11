//! Writing through the filesystem: what programs write is appended to the log the project's processes share
//! (`sfs::Log`) - its bytes there, found again by the ranges each file keeps of it (`sfs::Content`) - and shown over
//! the tree served, so a program reads back what it wrote at once, from memory; the owner applies it to the project
//! as programs finish with it (`write::Change`).
//!
//! Nothing a program does waits for the project but a sync, and what is asked of a file while it is being given:
//! a thread gives the project what is due (`settle`), asleep until the next is, woken where something is due
//! before. What is asked in an order - folders made, things renamed - is given in that order, before anything
//! else, and while nobody takes it nothing after it is given.
//!
//! - A file the project has is the project's once the program closes it having written it - given off the
//!   program's thread, the close returning at once - or syncs it, which waits for it.
//! - A file the project does not have is the project's once nothing has it open and its burst of changes has ended
//!   (`Pace`): one written under a temporary name and renamed over the file it saves is a save of that file, never
//!   a file of its own.
//! - A file removed is hidden at once and removed from the project a moment later, unless a file is made there
//!   meanwhile: removing a file and writing it again is one save.
//! - Junk (`write::is_junk`: swap files, backups, any name starting with `.`) stays staged, never applied; it goes
//!   with the staging. A name ending in `.node` - storage's mark of a node - is refused.
//! - A folder made in a folder is shown at once, and the project given it in the order folders were made, before
//!   anything else it is asked. One made in a node waits for what is written in it: a `node.toml`
//!   makes it a node, other files make it a folder of the node's files. Files written beside a new `node.toml` wait for
//!   the tree to show its folder a node, so they are its files.
//! - A rename of what the project has is shown at once - moved in the tree served, with what is in it - and
//!   the project told in its turn; what is written there and not the project's yet goes with it. One the project
//!   refuses is put back where it was.
//! - Removing a folder removes what is in it as one: a node's folder deletes the node, with what is inside it. A
//!   removal the project refuses (a node's `node.toml` alone) shows the file again.
//!
//! What is staged and applied stays shown over the tree until the tree served has it (its length and time).

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, PoisonError, RwLock, Weak};
use std::time::{Duration, Instant, SystemTime};

use crate::write::{Change, Owner, Refusal, Written, is_junk};
use crate::{Served, What};

/// The folder of the log, in the writes' own.
pub const LOG: &str = "sfs";

/// When a new file is the project's: once nothing has it open and its burst of changes has ended - the pause since
/// its last change is several times the time between its changes, within these bounds. A file written under a
/// temporary name and renamed over the one it saves is, within its burst, a save of that one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pace {
    pub least: Duration,
    pub most: Duration,
}

impl Default for Pace {
    fn default() -> Pace {
        Pace { least: Duration::from_millis(10), most: Duration::from_millis(250) }
    }
}

/// How many times the time between a file's changes the pause after its last is, for its burst to have ended.
const RHYTHMS: u32 = 8;

/// How long a file renamed to a backup's name stays hidden for the file to come back, before it is removed.
const BACKUP: Duration = Duration::from_secs(5);

/// How long until what is due and waits for the tree to show something is looked at again: the tree changing
/// wakes what looks before.
const RECHECK: Duration = Duration::from_millis(100);

/// How long what applies changes sleeps at most, with nothing due: what the tree shows by now is let go of then.
const IDLE: Duration = Duration::from_millis(200);

/// How long the removal of a file the project was just given waits for the tree to show it, before it is given up.
const AWAITED: Duration = Duration::from_secs(30);

/// A file staged: its content - in the log, or in memory where it is junk - whether it was written since it was
/// last applied, and when it was applied.
#[derive(Debug)]
struct Staged {
    content: Arc<RwLock<sfs::Content>>,
    dirty: bool,
    applied: Option<SystemTime>,
}

fn reading(content: &RwLock<sfs::Content>) -> std::sync::RwLockReadGuard<'_, sfs::Content> {
    content.read().unwrap_or_else(PoisonError::into_inner)
}

fn writing(content: &RwLock<sfs::Content>) -> std::sync::RwLockWriteGuard<'_, sfs::Content> {
    content.write().unwrap_or_else(PoisonError::into_inner)
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
    /// Files to give the project, and when: new ones once their burst has ended, saved ones at once.
    new: HashMap<String, Instant>,
    /// When each file waiting in `new` was last changed, and the time between its changes.
    touched: HashMap<String, (Instant, Duration)>,
    /// Folders made and things renamed, for the project to be given in the order they were: shown so meanwhile.
    queued: VecDeque<(u64, Change)>,
    /// When the project is asked again to take what is queued, after nobody did.
    again: Option<Instant>,
    /// When what applies changes wakes next: told earlier where something is due before.
    wakes: Option<Instant>,
    /// Open files by handle: their path.
    handles: HashMap<u64, String>,
    /// Paths whose content the owner is applying this moment: the project's any moment now, whatever is asked of them
    /// meanwhile - which waits for it (`applied`).
    applying: BTreeSet<String>,
    /// Paths removed while the project had them and the tree did not show them yet, and when to stop waiting for it to:
    /// their removal is applied once it does.
    awaited: HashMap<String, Instant>,
    /// The record of each removal the project is still to be told (`sfs::Op::Removed`).
    removals: HashMap<String, u64>,
    /// Paths whose change nobody took yet (`Refusal::Later`): asked again, and said once.
    waiting: BTreeSet<String>,
    next: u64,
}

/// A map's entries at a path and inside it, put where the path is now.
fn rekeyed<T>(map: &mut HashMap<String, T>, from: &str, to: &str) {
    let inside = format!("{from}/");
    for path in map.keys().filter(|path| *path == from || path.starts_with(&inside)).cloned().collect::<Vec<_>>() {
        if let Some(it) = map.remove(&path) {
            map.insert(format!("{to}{}", &path[from.len()..]), it);
        }
    }
}

impl State {
    /// A path hidden is shown again - made anew where it was removed: its removal is told to nobody. Whether it
    /// was hidden.
    fn unhide(&mut self, path: &str) -> bool {
        self.removals.remove(path);
        self.hidden.remove(path).is_some()
    }

    /// A folder was renamed: what is shown at it and in it, and not the project's yet, is at its new place.
    fn moved(&mut self, from: &str, to: &str) {
        rekeyed(&mut self.staged, from, to);
        rekeyed(&mut self.new, from, to);
        rekeyed(&mut self.touched, from, to);
        rekeyed(&mut self.given, from, to);
        rekeyed(&mut self.removals, from, to);
        let inside = format!("{from}/");
        for path in std::mem::take(&mut self.folders) {
            let moved = path == from || path.starts_with(&inside);
            self.folders.insert(if moved { format!("{to}{}", &path[from.len()..]) } else { path });
        }
    }
}

/// How long after nobody took a change it is asked again.
const AGAIN: Duration = Duration::from_secs(1);
/// What the owner was given of a staged file, for as long as it reads it: the log kept from where it is, or the
/// file it was written out to.
struct Given {
    pins: Arc<Mutex<Vec<u64>>>,
    pin: Option<u64>,
    file: Option<PathBuf>,
    /// The file's number in the log, and where the log ended when it was read.
    taken: Option<(u64, u64)>,
    /// How many times the file had been changed when it was read.
    changes: u64,
}

impl Drop for Given {
    fn drop(&mut self) {
        if let Some(pin) = self.pin {
            let mut pins = self.pins.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(at) = pins.iter().position(|pinned| *pinned == pin) {
                pins.swap_remove(at);
            }
        }
        if let Some(file) = &self.file {
            let _ = std::fs::remove_file(file);
        }
    }
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
    /// Where a file is written out for who takes it whole, for as long as that takes.
    dir: PathBuf,
    /// The log what is written goes to.
    log: Arc<sfs::Log>,
    /// Every content there is, for as long as anything has it - staged, or being read or given this moment: the log
    /// is kept from the first position any of them reads from.
    contents: Arc<Mutex<Vec<Weak<RwLock<sfs::Content>>>>>,
    /// Positions of the log kept for what the owner was given and is reading this moment.
    pins: Arc<Mutex<Vec<u64>>>,
    state: Arc<Mutex<State>>,
    /// Held by what applies what has settled: one at a time, so that nothing is given to the project twice.
    settling: Arc<Mutex<()>>,
    /// Held by what gives the project what is queued: in order, one at a time.
    queueing: Arc<Mutex<()>>,
    /// What wakes the thread applying changes before it would wake by itself.
    wake: Arc<(Mutex<bool>, Condvar)>,
    /// Held while something is done that is said in the log in the order it was done - a file begun, removed,
    /// renamed, a folder made: the log has them as they happened, for who takes them up from it.
    ordering: Arc<Mutex<()>>,
    /// The position of the record being done again, while what the log holds is taken up (`replay`): nothing is
    /// appended for it. `NOT` otherwise.
    replaying: Arc<std::sync::atomic::AtomicU64>,
    pace: Pace,
}

/// No record is being done again: what is done is appended to the log.
const NOT: u64 = u64::MAX;

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(p, _)| p)
}

impl Writes {
    /// Writes kept in `dir` (emptied first: a log of its own, and what is written out for the owner), applied by
    /// `owner`; a thread applies what has settled, for as long as any clone of them lives.
    ///
    /// # Errors
    /// The folder or its log cannot be made.
    pub fn new(served: &Served, owner: Arc<dyn Owner>, dir: &Path) -> io::Result<Writes> {
        Writes::with(served, owner, dir, sfs::log::Config::default(), Pace::default())
    }

    /// As `new`, the log laid out and new files paced as said.
    ///
    /// # Errors
    /// The folder or its log cannot be made.
    pub fn with(
        served: &Served,
        owner: Arc<dyn Owner>,
        dir: &Path,
        log: sfs::log::Config,
        pace: Pace,
    ) -> io::Result<Writes> {
        // Emptied, but the log: whoever holds the project may have it open, and nothing of it is read again - what
        // it held for an earlier serving is let go of.
        std::fs::create_dir_all(dir)?;
        for entry in std::fs::read_dir(dir)?.flatten().filter(|entry| entry.file_name() != LOG) {
            let path = entry.path();
            let _ = if path.is_dir() { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) };
        }
        let log = sfs::Log::shared(&dir.join(LOG), log)?;
        let writes = Writes {
            served: served.clone(),
            owner,
            dir: dir.to_path_buf(),
            log,
            contents: Arc::default(),
            pins: Arc::default(),
            state: Arc::default(),
            settling: Arc::default(),
            queueing: Arc::default(),
            wake: Arc::default(),
            ordering: Arc::default(),
            replaying: Arc::new(std::sync::atomic::AtomicU64::new(NOT)),
            pace,
        };
        // What an earlier serving left in transit - it ended with no word of it - is taken up from the log.
        writes.replay();
        // What is due is applied off the filesystem's threads, for as long as the writes are kept: woken where
        // something is due before it would look, asleep until the next is.
        let (served, owner, dir, weak) =
            (writes.served.clone(), Arc::downgrade(&writes.owner), writes.dir.clone(), Arc::downgrade(&writes.state));
        let (settling, log, contents) = (writes.settling.clone(), writes.log.clone(), writes.contents.clone());
        let (pins, queueing, wake) = (writes.pins.clone(), writes.queueing.clone(), writes.wake.clone());
        let (ordering, replaying) = (writes.ordering.clone(), writes.replaying.clone());
        // The tree served changed: what it shows by now is let go of.
        let woken = Arc::downgrade(&writes.wake);
        writes.served.on_change(move || {
            if let Some(wake) = woken.upgrade() {
                *wake.0.lock().unwrap_or_else(PoisonError::into_inner) = true;
                wake.1.notify_all();
            }
        });
        let _ = std::thread::Builder::new().name("filesystem writes".into()).spawn(move || {
            loop {
                // Until the next of what waits is due - said, for whoever makes something due before to wake this.
                let sleep = {
                    let Some(state) = weak.upgrade() else { return };
                    let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
                    let now = Instant::now();
                    // A file still open is not due whenever it says: letting go of it wakes this.
                    let open: BTreeSet<&String> = state.handles.values().collect();
                    let new = state.new.iter().filter(|(path, _)| !open.contains(path)).map(|(_, due)| due);
                    let next = if state.queued.is_empty() {
                        new.chain(state.hidden.values().flatten()).min().copied()
                    } else {
                        Some(state.again.unwrap_or(now))
                    };
                    let sleep = next.map_or(IDLE, |due| due.saturating_duration_since(now)).min(IDLE);
                    state.wakes = Some(now + sleep);
                    sleep
                };
                {
                    let mut woken = wake.0.lock().unwrap_or_else(PoisonError::into_inner);
                    if !*woken && !sleep.is_zero() {
                        woken = wake.1.wait_timeout(woken, sleep).unwrap_or_else(PoisonError::into_inner).0;
                    }
                    *woken = false;
                }
                let (Some(state), Some(owner)) = (weak.upgrade(), owner.upgrade()) else { return };
                let (served, dir, log, settling) = (served.clone(), dir.clone(), log.clone(), settling.clone());
                let (contents, pins, queueing, wake) = (contents.clone(), pins.clone(), queueing.clone(), wake.clone());
                let (ordering, replaying) = (ordering.clone(), replaying.clone());
                let writes =
                    Writes { served, owner, dir, log, contents, pins, state, settling, queueing, wake, ordering, replaying, pace };
                writes.settle();
            }
        });
        Ok(writes)
    }

    /// Something is due at `due`: what applies changes is woken, where it would sleep past it.
    fn due(&self, state: &State, due: Instant) {
        if state.wakes.is_none_or(|wakes| wakes > due) {
            *self.wake.0.lock().unwrap_or_else(PoisonError::into_inner) = true;
            self.wake.1.notify_all();
        }
    }

    /// A new file changed: it is the project's once its burst of changes has ended - the pause since this change
    /// several times the time between its changes.
    fn touch(&self, state: &mut State, path: &str) {
        let now = Instant::now();
        let rhythm = match state.touched.get(path) {
            Some((last, rhythm)) => {
                let since = now.saturating_duration_since(*last);
                if rhythm.is_zero() { since } else { (*rhythm * 3 + since) / 4 }
            }
            None => Duration::ZERO,
        };
        state.touched.insert(path.to_string(), (now, rhythm));
        let due = now + (rhythm * RHYTHMS).clamp(self.pace.least, self.pace.most);
        state.new.insert(path.to_string(), due);
        self.due(state, due);
    }

    /// Give the project what is queued, in the order it was: before anything else it is asked, by whoever asks.
    /// What nobody takes now stays queued, for the next time.
    fn give_queued(&self) {
        let _queueing = self.queueing.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            let front = self.lock().queued.front().cloned();
            let Some((at, change)) = front else {
                self.lock().again = None;
                return;
            };
            let path = match &change {
                Change::Folder { path } | Change::Rename { to: path, .. } => path.clone(),
                _ => {
                    self.lock().queued.pop_front();
                    continue;
                }
            };
            match self.owner.change(change.clone()) {
                Ok(()) => {
                    self.lock().waiting.remove(&path);
                    let _ = self.record(&sfs::Op::Done { of: at });
                }
                Err(Refusal::Later(why)) => {
                    let mut state = self.lock();
                    if state.waiting.insert(path.clone()) {
                        eprintln!("{path}: {}, and kept until the project can be told: {why}", change.label());
                    }
                    state.again = Some(Instant::now() + AGAIN);
                    return;
                }
                // Not the project's after all: shown as it was before, and said, since nothing else tells.
                Err(Refusal::Never(why)) => {
                    eprintln!("{path}: {}, which the project did not take: {why}", change.label());
                    let _ordered = self.ordered();
                    self.lock().waiting.remove(&path);
                    self.undo(&change);
                    let _ = self.record(&sfs::Op::Refused { of: at });
                }
            }
            self.lock().queued.pop_front();
        }
    }

    /// Show again as it was what the project did not take: a folder made is gone, a rename is back where it was.
    fn undo(&self, change: &Change) {
        match change {
            Change::Rename { from, to } => {
                self.served.moved(to, from);
                self.lock().moved(to, from);
            }
            Change::Folder { path } => drop(self.lock().given.remove(path)),
            _ => {}
        }
    }

    /// Say in the log what was done: where it is said. Done again from the log, it is said there already.
    fn record(&self, op: &sfs::Op) -> io::Result<u64> {
        match self.replaying.load(std::sync::atomic::Ordering::Relaxed) {
            NOT => op.append(&self.log),
            position => Ok(position),
        }
    }

    fn ordered(&self) -> std::sync::MutexGuard<'_, ()> {
        self.ordering.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A content with nothing in it yet, begun at a path: in the log, where it is said to begin - `emptied`, as a
    /// change of its own - or in memory where it will never be the project's.
    fn begin(&self, path: &str, emptied: bool) -> io::Result<Arc<RwLock<sfs::Content>>> {
        let content = if is_junk(path) {
            let mut state = self.lock();
            state.next += 1;
            sfs::Content::in_memory(state.next)
        } else {
            sfs::Content::logged(self.record(&sfs::Op::Began { path: path.to_string(), emptied })?)
        };
        let content = Arc::new(RwLock::new(content));
        self.contents.lock().unwrap_or_else(PoisonError::into_inner).push(Arc::downgrade(&content));
        Ok(content)
    }

    /// Let go of the log before what anything is found again from: every content there is, what the owner is
    /// reading, what the project is still to be told. A file that waits long while much is written after it begins
    /// again further on (`Content::moved_forward`), so that it alone does not keep all of that.
    fn let_go_of_log(&self, written: u64) {
        let far = self.log.config().budget / 4;
        let behind: Vec<(String, Arc<RwLock<sfs::Content>>)> = {
            let state = self.lock();
            let behind = state.staged.iter().filter(|(_, staged)| {
                reading(&staged.content).first().is_some_and(|first| first + far < written)
            });
            behind.map(|(path, staged)| (path.clone(), staged.content.clone())).collect()
        };
        for (path, content) in behind {
            let _ordered = self.ordered();
            // Still what is staged there, and whether the project has it as it is.
            let dirty = match self.lock().staged.get(&path) {
                Some(staged) if Arc::ptr_eq(&staged.content, &content) => staged.dirty,
                _ => continue,
            };
            let Ok(id) = self.record(&sfs::Op::Began { path: path.clone(), emptied: false }) else { continue };
            let moved = writing(&content).moved_forward(&self.log, id);
            if moved.is_ok() && !dirty {
                let _ = self.record(&sfs::Op::Taken { file: id, upto: self.log.header().tail() });
            }
        }
        let live: Vec<Arc<RwLock<sfs::Content>>> = {
            let mut contents = self.contents.lock().unwrap_or_else(PoisonError::into_inner);
            contents.retain(|content| content.strong_count() > 0);
            contents.iter().filter_map(Weak::upgrade).collect()
        };
        let mut needed = written;
        for content in live {
            needed = needed.min(reading(&content).first().unwrap_or(needed));
        }
        let pinned = self.pins.lock().unwrap_or_else(PoisonError::into_inner).iter().copied().min();
        needed = needed.min(pinned.unwrap_or(needed));
        {
            let state = self.lock();
            let told = state.queued.iter().map(|(at, _)| *at).chain(state.removals.values().copied()).min();
            needed = needed.min(told.unwrap_or(needed));
        }
        self.log.header().set_applied(needed);
        self.log.header().set_durable(needed);
        self.log.let_go();
    }

    /// What a staged file holds this moment, said by where it is in the log for the owner to read it there - the
    /// log kept for it until what this returns with it is dropped - with the file's number and where the log ended
    /// then: what is written to it after is not what was given. Written out to a file where it is not in the log.
    fn given(&self, content: &RwLock<sfs::Content>) -> io::Result<(Written, Given)> {
        let (len, ranges, first, taken, changes) = {
            let content = reading(content);
            // Read under the file's lock, which every write to it holds as it appends: none is half in.
            let taken = (content.id(), self.log.header().tail());
            (content.len(), content.ranges(), content.first(), taken, content.changes())
        };
        let Some(ranges) = ranges else {
            let file = self.written_out(content)?;
            let given = Given { pins: self.pins.clone(), pin: None, file: Some(file.clone()), taken: None, changes };
            return Ok((Written::File(file), given));
        };
        // Pinned before the log could be let go of past it: the content is still held by who asked.
        if let Some(first) = first {
            self.pins.lock().unwrap_or_else(PoisonError::into_inner).push(first);
        }
        let written = Written::Logged { log: self.log.dir().to_path_buf(), len, ranges };
        Ok((written, Given { pins: self.pins.clone(), pin: first, file: None, taken: Some(taken), changes }))
    }

    /// A staged file's content written out to a file of its own, for who takes it whole: its path, to be removed
    /// once taken.
    fn written_out(&self, content: &RwLock<sfs::Content>) -> io::Result<PathBuf> {
        let number = {
            let mut state = self.lock();
            state.next += 1;
            state.next
        };
        let path = self.dir.join(format!("{number}"));
        let mut out = io::BufWriter::new(std::fs::File::create(&path)?);
        reading(content).write_to(&self.log, &mut out)?;
        io::Write::flush(&mut out)?;
        Ok(path)
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
            let content = reading(&staged.content);
            return Some(Shown { folder: false, size: content.len(), modified: content.modified() });
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
        let staged = self.lock().staged.get(path).map(|s| s.content.clone());
        if let Some(content) = staged {
            let mut out = vec![0u8; len];
            let read = reading(&content).read_into(&self.log, offset, &mut out)?;
            out.truncate(read);
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
        let staged = self.lock().staged.get(path).map(|s| s.content.clone());
        if let Some(content) = staged {
            return reading(&content).read_into(&self.log, offset, buffer);
        }
        let tree = self.served.tree();
        let (ino, _) = tree.at(path).ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        tree.read_into(ino, offset, buffer)
    }

    /// A path staged: its content now - the tree's, unless `empty` - brought into the log where it is not staged
    /// yet, so that what is written to it next is written over the file as it was then. What will never be the
    /// project's is kept in memory instead.
    fn stage(&self, path: &str, empty: bool) -> io::Result<()> {
        let staged = self.lock().staged.get(path).map(|s| s.content.clone());
        if let Some(content) = staged {
            if empty {
                let mut content = writing(&content);
                if content.is_logged() {
                    self.record(&sfs::Op::Truncated { file: content.id(), len: 0 })?;
                }
                content.truncate(0);
            }
            return Ok(());
        }
        let content = self.begin(path, empty)?;
        if !empty && let Some((ino, node)) = self.served.tree().at(path) {
            let (tree, mut at) = (self.served.tree(), 0u64);
            let mut filled = writing(&content);
            while at < node.size {
                let bytes = tree.read(ino, at, 1 << 20)?;
                if bytes.is_empty() {
                    break;
                }
                filled.fill(&self.log, at, &bytes)?;
                at += bytes.len() as u64;
            }
        }
        self.lock().staged.insert(path.to_string(), Staged { content, dirty: false, applied: None });
        Ok(())
    }

    /// Open a file for writing (`truncate`: emptied); its handle.
    ///
    /// # Errors
    /// It cannot be staged.
    pub fn open(&self, path: &str, truncate: bool) -> io::Result<u64> {
        let _ordered = self.ordered();
        self.opened(path, truncate)
    }

    fn opened(&self, path: &str, truncate: bool) -> io::Result<u64> {
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
        let _ordered = self.ordered();
        let returning = self.lock().unhide(path);
        let exists = returning || self.served.tree().at(path).is_some();
        let handle = self.opened(path, true)?;
        if !exists && !is_junk(path) {
            let mut state = self.lock();
            self.touch(&mut state, path);
        }
        Ok(handle)
    }

    /// Write at an offset of an open file.
    ///
    /// # Errors
    /// The handle is not open, or the staging file cannot be written.
    pub fn write(&self, handle: u64, offset: u64, data: &[u8]) -> io::Result<usize> {
        let content = {
            let mut state = self.lock();
            let path = state.handles.get(&handle).cloned().ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
            if state.new.contains_key(&path) {
                self.touch(&mut state, &path);
            }
            let staged = state.staged.get_mut(&path).ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
            staged.dirty = true;
            staged.content.clone()
        };
        // The log first, then the file's ranges: both under the file's own lock, so that two writes to one file
        // are in the file in the order they are in the log.
        writing(&content).write(&self.log, offset, data)?;
        Ok(data.len())
    }

    /// Set a file's length: staged, applied as a write.
    ///
    /// # Errors
    /// It cannot be staged.
    pub fn truncate(&self, path: &str, len: u64) -> io::Result<()> {
        let _ordered = self.ordered();
        self.stage(path, false)?;
        let content = {
            let mut state = self.lock();
            let staged = state.staged.get_mut(path).ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
            staged.dirty = true;
            staged.content.clone()
        };
        let mut content = writing(&content);
        if content.is_logged() {
            self.record(&sfs::Op::Truncated { file: content.id(), len })?;
        }
        content.truncate(len);
        Ok(())
    }

    /// Whether a program has the file at a path open to write.
    #[must_use]
    pub fn is_open(&self, path: &str) -> bool {
        self.lock().handles.values().any(|open| open == path)
    }

    /// A handle let go of, or a file synced: what it wrote is the project's, where the project has the file - given
    /// off this thread, at once. Synced, what was written is on disk in the log before this returns: from there it
    /// is the project's whatever becomes of this process (`replay`).
    ///
    /// # Errors
    /// Synced, the log could not be had on disk.
    pub fn close(&self, handle: u64, release: bool) -> io::Result<()> {
        {
            let mut state = self.lock();
            let path = if release { state.handles.remove(&handle) } else { state.handles.get(&handle).cloned() };
            let Some(path) = path else { return Ok(()) };
            let now = Instant::now();
            if let Some(due) = state.new.get(&path).copied() {
                // A new file: its burst is told from when it was last changed, not from when it was let go of.
                if release {
                    self.due(&state, due);
                }
            } else if state.staged.get(&path).is_some_and(|staged| staged.dirty) && !is_junk(&path) {
                state.new.insert(path, now);
                self.due(&state, now);
            }
        }
        if release { Ok(()) } else { self.log.flush(self.log.header().tail()) }
    }

    /// A path's change nobody took: asked again in a moment, and said the first time.
    fn waits(state: &mut State, path: &str, why: &str) {
        state.touched.remove(path);
        state.new.insert(path.to_string(), Instant::now() + AGAIN);
        if state.waiting.insert(path.to_string()) {
            eprintln!("{path}: written, and kept until it can be taken into the project: {why}");
        }
    }

    /// Give the project the files that are due, together (`Owner::changes`): all that were written and are no junk.
    /// One the project took is said so in the log, with where the log ended when it was read.
    fn apply_new(&self, paths: &[String]) {
        let mut given = Vec::new();
        {
            let mut state = self.lock();
            for path in paths {
                state.new.remove(path);
                state.touched.remove(path);
                let Some(staged) = state.staged.get(path).filter(|staged| staged.dirty && !is_junk(path)) else {
                    continue;
                };
                given.push((path.clone(), staged.content.clone()));
                state.applying.insert(path.clone());
            }
        }
        if given.is_empty() {
            return;
        }
        // Each said by where it is; one that cannot be is not given, and stays as it is.
        let said: Vec<io::Result<(Written, Given)>> = given.iter().map(|(_, content)| self.given(content)).collect();
        let asked = given.iter().zip(&said).filter_map(|((path, _), said)| {
            Some(Change::Write { path: path.clone(), staged: said.as_ref().ok()?.0.clone() })
        });
        let mut answers = self.owner.changes(asked.collect()).into_iter();
        let applied: Vec<Result<(), Refusal>> = said
            .iter()
            .map(|said| match said {
                Ok(_) => answers.next().unwrap_or_else(|| Err(Refusal::Never("it was not answered".into()))),
                Err(e) => Err(Refusal::Later(format!("it could not be read: {e}"))),
            })
            .collect();
        let mut state = self.lock();
        for (((path, content), applied), said) in given.into_iter().zip(applied).zip(said) {
            state.applying.remove(&path);
            match applied {
                Ok(()) => {
                    state.waiting.remove(&path);
                    let Ok((_, said)) = said else { continue };
                    if let Some((file, upto)) = said.taken {
                        let _ = self.record(&sfs::Op::Taken { file, upto });
                    }
                    // The project has it as it was read: what was written to it meanwhile it has not.
                    if let Some(staged) = state.staged.get_mut(&path)
                        && Arc::ptr_eq(&staged.content, &content)
                        && reading(&content).changes() == said.changes
                    {
                        staged.dirty = false;
                        staged.applied = Some(SystemTime::now());
                    }
                }
                Err(Refusal::Later(why)) => Self::waits(&mut state, &path, &why),
                // It stays as written, shown in the folder and not the project's: said, since nothing else tells.
                Err(Refusal::Never(why)) => eprintln!("{path}: written, and not taken into the project: {why}"),
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

    /// Remove a file or an empty folder: hidden now, removed when it settles. What is staged alone goes at once.
    ///
    /// # Errors
    /// It is not there.
    pub fn remove(&self, path: &str) -> io::Result<()> {
        self.applied(path);
        let _ordered = self.ordered();
        let said = if is_junk(path) { None } else { Some(self.record(&sfs::Op::Removed { path: path.to_string() })?) };
        let in_tree = self.served.tree().at(path).is_some();
        let mut state = self.lock();
        let staged = state.staged.remove(path);
        let folder = state.folders.remove(path);
        // A folder the project was given, the tree not showing it yet: the project has it, as a file applied.
        let given = state.given.remove(path).is_some();
        state.new.remove(path);
        state.touched.remove(path);
        let applied = given || staged.as_ref().is_some_and(|s| s.applied.is_some());
        if in_tree || applied {
            // A moment for a file to be made there again: removed and written anew is one save.
            let due = Instant::now() + self.pace.most;
            state.hidden.insert(path.to_string(), Some(due));
            if let Some(said) = said {
                state.removals.insert(path.to_string(), said);
            }
            self.due(&state, due);
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
        let _ordered = self.ordered();
        let said = if is_junk(path) { 0 } else { self.record(&sfs::Op::MadeFolder { path: path.to_string() })? };
        // Made where one was removed a moment ago, its removal not applied yet: it is not removed after all. What was
        // removed from it stays removed.
        {
            let mut state = self.lock();
            if matches!(state.hidden.get(path), Some(Some(_))) && self.served.tree().at(path).is_some() {
                state.unhide(path);
                state.awaited.remove(path);
                return Ok(());
            }
        }
        if self.in_node(path) || is_junk(path) {
            self.lock().folders.insert(path.to_string());
            return Ok(());
        }
        let there = self.served.tree().at(path).is_some();
        // Taken up from the log with the tree showing it: the project has it, whatever the log came to say of that.
        if there && self.replaying.load(std::sync::atomic::Ordering::Relaxed) != NOT {
            return Ok(());
        }
        // Shown from now on, whenever the tree comes to show it; the project is given it off this thread, in the
        // order folders were made.
        let mut state = self.lock();
        if !there {
            state.given.insert(path.to_string(), Instant::now() + AWAITED);
        }
        state.queued.push_back((said, Change::Folder { path: path.to_string() }));
        self.due(&state, Instant::now());
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
        let _ordered = self.ordered();
        let replaying = self.replaying.load(std::sync::atomic::Ordering::Relaxed) != NOT;
        // Said of what the log knows of: what was never the project's to have is found again by nothing.
        let said = if is_junk(from) {
            0
        } else {
            self.record(&sfs::Op::Renamed { from: from.to_string(), to: to.to_string() })?
        };
        // One the project has, with content of it staged - written and not taken yet, or taken a moment ago - is
        // renamed in the project, and what was written goes on to its new name: moved as what is only staged is, it
        // would stay in the project under its old name too.
        let held = self.served.tree().at(from).is_some() && self.lock().staged.contains_key(from);
        if held && !is_junk(to) && self.served.tree().at(to).is_some() {
            // Onto another the project has: that one takes what was written, and this one is removed.
            let mut state = self.lock();
            state.new.remove(from);
            state.touched.remove(from);
            state.waiting.remove(from);
            let now = Instant::now();
            if let Some(staged) = state.staged.remove(from) {
                state.staged.insert(to.to_string(), Staged { dirty: true, ..staged });
                state.new.insert(to.to_string(), now);
            }
            state.unhide(to);
            state.hidden.insert(from.to_string(), Some(now));
            state.removals.insert(from.to_string(), said);
            self.due(&state, now);
            return Ok(());
        }
        if held && !is_junk(to) {
            self.served.moved(from, to);
            let mut state = self.lock();
            state.queued.push_back((said, Change::Rename { from: from.to_string(), to: to.to_string() }));
            self.due(&state, Instant::now());
            state.new.remove(from);
            state.touched.remove(from);
            state.waiting.remove(from);
            if let Some(staged) = state.staged.remove(from) {
                if staged.dirty {
                    let now = Instant::now();
                    state.new.insert(to.to_string(), now);
                    self.due(&state, now);
                }
                state.staged.insert(to.to_string(), staged);
            }
            return Ok(());
        }
        let (staged, touched) = {
            let mut state = self.lock();
            state.new.remove(from);
            (state.staged.remove(from), state.touched.remove(from))
        };
        if let Some(staged) = staged {
            return self.rename_staged(staged, touched, from, to);
        }
        if is_junk(to) {
            // Taken up from the log, the backup is not made again: it was nobody's to keep.
            if !replaying {
                self.stage(from, false)?;
            }
            self.served.rename(from, to);
            let mut state = self.lock();
            if let Some(copy) = state.staged.remove(from) {
                // A backup is nobody's to give: out of the log.
                let kept = writing(&copy.content).held_in_log(&self.log, None);
                state.staged.insert(to.to_string(), Staged { dirty: false, ..copy });
                kept?;
            }
            state.hidden.insert(from.to_string(), Some(Instant::now() + BACKUP));
            return Ok(());
        }
        // Moved in what is served at once, with what is in it; the project is told in its turn.
        let in_tree = self.served.moved(from, to);
        let mut state = self.lock();
        // Something is there: in the tree, a folder made a moment ago, or only what was written in one - as where
        // the log is taken up and the project has the rename already, the tree showing it done.
        let inside = format!("{from}/");
        let written = || state.staged.keys().any(|path| path.starts_with(&inside));
        if !in_tree && !state.given.contains_key(from) && !state.folders.contains(from) && !written() {
            return Err(io::Error::from(io::ErrorKind::NotFound));
        }
        // What is shown in a folder renamed, and not the project's yet, goes with it.
        state.moved(from, to);
        // A folder waiting in a node for what is written in it is nothing of the project's yet: only shown.
        if in_tree || state.given.contains_key(to) {
            state.queued.push_back((said, Change::Rename { from: from.to_string(), to: to.to_string() }));
            self.due(&state, Instant::now());
        }
        Ok(())
    }

    /// A file only staged, renamed: staged under its new name - a save of the file there, or a new file at last.
    fn rename_staged(
        &self,
        mut staged: Staged,
        touched: Option<(Instant, Duration)>,
        from: &str,
        to: &str,
    ) -> io::Result<()> {
        // The kernel keeps the renamed file's number for its new name.
        self.served.rename(from, to);
        let exists = self.lock().unhide(to) || self.served.tree().at(to).is_some();
        staged.dirty = true;
        // What comes to be the project's is in the log - begun there, where it was kept in memory; what will
        // never be, in memory.
        let logged = {
            let content = reading(&staged.content);
            content.is_logged().then(|| content.id())
        };
        let begun = match logged {
            _ if is_junk(to) => Ok(None),
            Some(id) => Ok(Some(id)),
            None => self.record(&sfs::Op::Began { path: to.to_string(), emptied: true }).map(Some),
        };
        let kept = begun.and_then(|begun| writing(&staged.content).held_in_log(&self.log, begun));
        self.lock().staged.insert(to.to_string(), staged);
        kept?;
        if is_junk(to) {
            return Ok(());
        }
        // A save of the file there: the project's at once, off this thread. A new file under its name at last:
        // its burst goes on from where it was.
        let mut state = self.lock();
        if exists {
            let now = Instant::now();
            state.new.insert(to.to_string(), now);
            self.due(&state, now);
        } else {
            if let Some(touched) = touched {
                state.touched.insert(to.to_string(), touched);
            }
            self.touch(&mut state, to);
        }
        Ok(())
    }

    /// Apply what waits to settle, now: what serving does as it ends, so that what a program did a moment before is
    /// the project's too. A file still open is left, as is what waits for more than time.
    pub fn finish(&self) {
        {
            let mut state = self.lock();
            let now = Instant::now();
            state.again = None;
            for due in state.new.values_mut() {
                *due = now;
            }
            for due in state.hidden.values_mut().flatten() {
                *due = now;
            }
        }
        self.settle();
        // What nobody took by now is its owner's to keep: the staging is emptied when the folder is next served.
        let left: Vec<(String, Arc<RwLock<sfs::Content>>)> = {
            let state = self.lock();
            let unapplied = state.staged.iter().filter(|(path, staged)| staged.dirty && !is_junk(path));
            unapplied.map(|(path, staged)| (path.clone(), staged.content.clone())).collect()
        };
        for (path, content) in left {
            if let Ok(file) = self.written_out(&content) {
                self.owner.left(&path, &file);
                let _ = std::fs::remove_file(file);
            }
        }
        // Ended with nothing in transit that is not its owner's now: what the log holds is nobody's to take up.
        let _ordered = self.ordered();
        if let Ok(ended) = self.record(&sfs::Op::Ended) {
            self.log.header().set_applied(ended);
            self.log.header().set_durable(ended);
        }
    }

    /// Take up what the log holds from an earlier serving that ended with no word of it - its process gone, the
    /// power lost: each thing done is done again here in the order it was, with nothing appended for it, and what
    /// the project took by then is known as taken. All that is still to be given is due at once.
    fn replay(&self) {
        use std::sync::atomic::Ordering::Relaxed;
        let mut files: HashMap<u64, Arc<RwLock<sfs::Content>>> = HashMap::new();
        // Where each file was last changed, and files begun again further on whose move may not be whole.
        let mut changed: HashMap<u64, u64> = HashMap::new();
        let mut moving: HashMap<u64, (String, Staged)> = HashMap::new();
        let staged_as = |writes: &Writes, file: u64| -> Option<String> {
            let state = writes.lock();
            let found = state.staged.iter().find(|(_, staged)| reading(&staged.content).id() == file);
            found.map(|(path, _)| path.clone())
        };
        let mut taken_up = 0usize;
        for record in self.log.records(self.log.begins()) {
            let Ok(record) = record else { break };
            if let Some((file, offset, at, len, wrote)) = sfs::content::data(&record) {
                let Some(content) = files.get(&file) else { continue };
                writing(content).attach(offset, len, at);
                if wrote {
                    changed.insert(file, record.position);
                    if let Some(path) = staged_as(self, file)
                        && let Some(staged) = self.lock().staged.get_mut(&path)
                    {
                        staged.dirty = true;
                    }
                }
                continue;
            }
            let Some(op) = sfs::Op::of(record.kind, &record.body) else { continue };
            taken_up += 1;
            self.replaying.store(record.position, Relaxed);
            match op {
                sfs::Op::Began { path, emptied } => {
                    let Ok(content) = self.begin(&path, emptied) else { continue };
                    files.insert(record.position, content.clone());
                    if emptied {
                        changed.insert(record.position, record.position);
                    }
                    let mut state = self.lock();
                    state.unhide(&path);
                    let before = state.staged.insert(path.clone(), Staged { content, dirty: emptied, applied: None });
                    // Begun again as it is: the one before stands until this one is whole.
                    if let Some(before) = before.filter(|_| !emptied) {
                        moving.insert(record.position, (path, before));
                    }
                }
                sfs::Op::Truncated { file, len } => {
                    let Some(content) = files.get(&file) else { continue };
                    writing(content).truncate(len);
                    // What ends a file begun again further on: it is whole, and as its changes said it was.
                    if let Some((_, before)) = moving.remove(&file) {
                        if let Some(path) = staged_as(self, file)
                            && let Some(staged) = self.lock().staged.get_mut(&path)
                        {
                            staged.dirty = before.dirty;
                        }
                        continue;
                    }
                    changed.insert(file, record.position);
                    if let Some(path) = staged_as(self, file)
                        && let Some(staged) = self.lock().staged.get_mut(&path)
                    {
                        staged.dirty = true;
                    }
                }
                sfs::Op::Removed { path } => drop(self.remove(&path)),
                sfs::Op::MadeFolder { path } => drop(self.make_folder(&path)),
                sfs::Op::Renamed { from, to } => drop(self.rename(&from, &to)),
                sfs::Op::Taken { file, upto } => {
                    if changed.get(&file).is_none_or(|at| *at < upto)
                        && let Some(path) = staged_as(self, file)
                        && let Some(staged) = self.lock().staged.get_mut(&path)
                    {
                        staged.dirty = false;
                        // Shown over the tree until the tree has a file of its length, whenever that was.
                        staged.applied = Some(SystemTime::UNIX_EPOCH);
                    }
                }
                sfs::Op::Done { of } => self.became(of, false),
                sfs::Op::Refused { of } => self.became(of, true),
                sfs::Op::Ended => {
                    let mut state = self.lock();
                    let next = state.next;
                    *state = State { next, ..State::default() };
                    files.clear();
                    changed.clear();
                    moving.clear();
                    taken_up = 0;
                }
            }
        }
        self.replaying.store(NOT, Relaxed);
        self.taken_up(moving, taken_up);
    }

    /// What the log says became of something the project was to be told - a folder, a rename, a removal - when
    /// the log is taken up: told, or refused and shown again as it was.
    fn became(&self, of: u64, refused: bool) {
        let mut state = self.lock();
        if state.queued.front().is_some_and(|(at, _)| *at == of) {
            let told = state.queued.pop_front();
            drop(state);
            if let Some((_, change)) = told.filter(|_| refused) {
                self.undo(&change);
            }
        } else if let Some(path) = state.removals.iter().find(|(_, at)| **at == of).map(|(path, _)| path.clone()) {
            state.removals.remove(&path);
            state.awaited.remove(&path);
            if refused {
                state.hidden.remove(&path);
            } else {
                state.hidden.insert(path, None);
            }
        }
    }

    /// The log taken up: what was cut short stands as it was before, and all that is still to be given is due.
    fn taken_up(&self, moving: HashMap<u64, (String, Staged)>, taken_up: usize) {
        let mut state = self.lock();
        // A file begun again whose move was cut short: the one before it stands.
        for (file, (path, before)) in moving {
            if state.staged.get(&path).is_some_and(|staged| reading(&staged.content).id() == file) {
                state.staged.insert(path, before);
            }
        }
        // All that is still to be given is due: nothing of it is waited for any longer.
        let now = Instant::now();
        state.touched.clear();
        state.again = None;
        let due: Vec<String> =
            state.staged.iter().filter(|(path, staged)| staged.dirty && !is_junk(path)).map(|(p, _)| p.clone()).collect();
        state.new = due.into_iter().map(|path| (path, now)).collect();
        for due in state.hidden.values_mut().flatten() {
            *due = now;
        }
        let waits = state.new.len() + state.queued.len() + state.removals.len();
        if waits > 0 {
            eprintln!("{waits} changes an earlier serving left in transit are taken up ({taken_up} things it did)");
        }
    }

    /// Apply what has settled - new files, removals - and let go of what the tree served now shows.
    pub fn settle(&self) {
        // One at a time: what settles is found and given under it, by the thread that does so as it goes and by
        // what gives all of it as serving ends.
        let _settling = self.settling.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        // Where the log ends before anything is looked at: what is staged from now on is past it.
        let written = self.log.header().tail();
        // Folders first, in the order they were made: what was written in them is then written in what is there.
        let now = Instant::now();
        if self.lock().again.is_none_or(|again| again <= now) {
            self.give_queued();
        }
        // While nobody takes what was asked in an order, nothing asked after it is given.
        let blocked = !self.lock().queued.is_empty();
        let due_new: Vec<String> = {
            let state = self.lock();
            let open: BTreeSet<&String> = state.handles.values().collect();
            let due = state.new.iter().filter(|(p, due)| !blocked && **due <= now && !open.contains(p));
            due.map(|(p, _)| p.clone()).collect()
        };
        // Declarations first: what is written beside one waits for its folder to be a node.
        let mut due_new = due_new;
        due_new.sort_by_key(|p| (!p.ends_with(&format!("/{}", project::index::NODE_FILE)), p.clone()));
        due_new.retain(|path| {
            let waits = self.waits_for_its_node(path);
            if waits {
                self.lock().new.insert(path.clone(), now + RECHECK);
            }
            !waits
        });
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
        self.give_removals(now, blocked);
        // What the tree has now needs no showing over it.
        let tree = self.served.tree();
        let mut state = self.lock();
        let State { touched, new, .. } = &mut *state;
        touched.retain(|path, _| new.contains_key(path));
        state.hidden.retain(|path, due| due.is_some() || tree.at(path).is_some());
        state.given.retain(|path, until| tree.at(path).is_none() && *until > now);
        let open: BTreeSet<String> = state.handles.values().cloned().collect();
        state.staged.retain(|path, staged| {
            let Some(applied) = staged.applied.filter(|_| !staged.dirty && !open.contains(path)) else { return true };
            let shown = tree.at(path).is_some_and(|(_, node)| {
                node.size == reading(&staged.content).len() && node.modified + Duration::from_secs(2) >= applied
            });
            !shown
        });
        drop(state);
        self.let_go_of_log(written);
    }

    /// Tell the project of the removals that are due, together: each said so in the log once it is told.
    fn give_removals(&self, now: Instant, blocked: bool) {
        let due_removals: Vec<String> = {
            let state = self.lock();
            let due = state.hidden.iter().filter(|(_, due)| !blocked && due.is_some_and(|d| d <= now));
            due.map(|(p, _)| p.clone()).collect()
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
                self.lock().hidden.insert(path, Some(now + RECHECK));
                continue;
            }
            if !covered && in_tree {
                removing.push(path);
                continue;
            }
            let mut state = self.lock();
            state.awaited.remove(&path);
            if let Some(said) = state.removals.remove(&path) {
                let _ = self.record(&sfs::Op::Done { of: said });
            }
            state.hidden.insert(path, None);
        }
        // Removed from the project together.
        let asked = removing.iter().map(|path| Change::Remove { path: path.clone() }).collect();
        for (path, removed) in removing.into_iter().zip(self.owner.changes(asked)) {
            let mut state = self.lock();
            state.awaited.remove(&path);
            match removed {
                Ok(()) => {
                    state.waiting.remove(&path);
                    if let Some(said) = state.removals.remove(&path) {
                        let _ = self.record(&sfs::Op::Done { of: said });
                    }
                    state.hidden.insert(path, None);
                }
                // Nobody takes it now: it stays hidden, and is asked again.
                Err(Refusal::Later(why)) => {
                    if state.waiting.insert(path.clone()) {
                        eprintln!("{path}: removed, which waits until the project can be told: {why}");
                    }
                    state.hidden.insert(path, Some(Instant::now() + AGAIN));
                }
                // Refused: it is shown again.
                Err(Refusal::Never(_)) => {
                    if let Some(said) = state.removals.remove(&path) {
                        let _ = self.record(&sfs::Op::Refused { of: said });
                    }
                    state.hidden.remove(&path);
                }
            }
        }
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
    use crate::write::{Change, Owner, Refusal};
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
        fn change(&self, change: Change) -> Result<(), Refusal> {
            match &change {
                Change::Write { path, staged } => {
                    let _ = self.started.lock().unwrap().send(());
                    let _ = self.go_on.lock().unwrap().recv();
                    let bytes = staged.read().map_err(|e| Refusal::Never(e.to_string()))?;
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
        fn change(&self, change: Change) -> Result<(), Refusal> {
            self.batches.lock().unwrap().push(vec![change.label()]);
            Ok(())
        }

        fn changes(&self, changes: Vec<Change>) -> Vec<Result<(), Refusal>> {
            self.batches.lock().unwrap().push(changes.iter().map(Change::label).collect());
            changes.iter().map(|_| Ok(())).collect()
        }
    }

    /// A project nobody takes changes for until it is `open`: what it was given then, and what was left with it.
    #[derive(Default)]
    struct Busy {
        open: std::sync::atomic::AtomicBool,
        given: Mutex<Vec<String>>,
        left: Mutex<Vec<String>>,
    }

    impl Owner for Busy {
        fn change(&self, change: Change) -> Result<(), Refusal> {
            if !self.open.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(Refusal::Later("a tool has it".into()));
            }
            self.given.lock().unwrap().push(change.label());
            Ok(())
        }

        fn left(&self, path: &str, _staged: &std::path::Path) {
            self.left.lock().unwrap().push(path.to_string());
        }
    }

    #[test]
    fn what_nobody_takes_yet_is_kept_and_given_once_someone_does() {
        let dir = std::env::temp_dir().join(format!("pe2-writes-later-{}", std::process::id()));
        let served = Served::default();
        served.set_files(&BTreeMap::new());
        let owner = Arc::new(Busy::default());
        let writes = Writes::new(&served, owner.clone(), &dir).unwrap();
        let handle = writes.create("kept.txt").unwrap();
        writes.write(handle, 0, b"x").unwrap();
        writes.close(handle, true).unwrap();
        // Tried, and not taken: it is shown as written, and nothing was given.
        std::thread::sleep(Duration::from_millis(2500));
        assert!(writes.shown("kept.txt").is_some(), "shown as written while it waits");
        assert!(owner.given.lock().unwrap().is_empty());
        // Someone takes changes again: it is given without being written again.
        owner.open.store(true, std::sync::atomic::Ordering::Relaxed);
        let given = || !owner.given.lock().unwrap().is_empty();
        let asked = Instant::now();
        while !given() && asked.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(given(), "given once someone takes it");
        assert!(owner.left.lock().unwrap().is_empty());
        drop(writes);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_log_is_let_go_of_behind_what_is_written_whatever_waits() {
        let dir = std::env::temp_dir().join(format!("pe2-writes-log-{}", std::process::id()));
        let served = Served::default();
        served.set_files(&BTreeMap::new());
        // Nobody takes anything: what is written waits, for as long as this runs.
        let owner = Arc::new(Busy::default());
        let log = sfs::log::Config { segment: 64 << 10, budget: 1 << 20 };
        let writes = Writes::with(&served, owner, &dir, log, super::Pace::default()).unwrap();
        let waits = writes.create("waits.txt").unwrap();
        writes.write(waits, 0, b"written first, and kept").unwrap();
        writes.close(waits, true).unwrap();
        // Four times what the log may hold, written over one file again and again.
        let mut last = Vec::new();
        for round in 0..40u8 {
            let handle = writes.open("big.bin", true).unwrap();
            last = vec![round; 100 << 10];
            writes.write(handle, 0, &last).unwrap();
            writes.close(handle, false).unwrap();
            writes.settle();
        }
        assert_eq!(writes.read("waits.txt", 0, 100).unwrap(), b"written first, and kept");
        assert_eq!(writes.read("big.bin", 0, 200 << 10).unwrap(), last);
        let segments = std::fs::read_dir(dir.join("sfs").join("log")).unwrap().count();
        assert!(segments <= 8, "the log keeps what is read from, not all that was written: {segments} segments");
        drop(writes);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// How long the library itself takes for what programs do, with no backend before it.
    #[test]
    #[ignore = "a measurement"]
    fn bench_writes() {
        let dir = std::env::temp_dir().join(format!("pe2-writes-bench-{}", std::process::id()));
        let served = Served::default();
        served.set_files(&BTreeMap::new());
        let writes = Writes::new(&served, Arc::new(Told::default()), &dir).unwrap();
        let said = |what: &str, mut took: Vec<Duration>| {
            took.sort_unstable();
            let at = |part: usize| took[took.len() * part / 100].as_secs_f64() * 1e6;
            println!("BENCH {what}: median {:.2} us, 99th {:.2} us", at(50), at(99));
        };
        let timed = |run: &mut dyn FnMut(usize), count: usize| -> Vec<Duration> {
            (0..count)
                .map(|n| {
                    let started = Instant::now();
                    run(n);
                    started.elapsed()
                })
                .collect()
        };
        let block = [7u8; 4096];
        let new = timed(
            &mut |n| {
                let handle = writes.create(&format!("new{n}.bin")).unwrap();
                writes.write(handle, 0, &block).unwrap();
                writes.close(handle, true).unwrap();
            },
            2000,
        );
        said("a new file: create, write 4 KiB, close", new);
        let handle = writes.create("writes.bin").unwrap();
        let wrote = timed(
            &mut |n| {
                writes.write(handle, n as u64 * 4096, &block).unwrap();
            },
            20000,
        );
        said("a write of 4 KiB", wrote);
        let mut buffer = [0u8; 4096];
        let read = timed(
            &mut |n| {
                writes.read_into("writes.bin", (n * 37 % 20000) as u64 * 4096, &mut buffer).unwrap();
            },
            20000,
        );
        said("a read of 4 KiB of what was written", read);
        said("what a written file is (its length and time)", timed(&mut |_| drop(writes.shown("writes.bin")), 20000));
        writes.close(handle, true).unwrap();
        said("a file removed", timed(&mut |n| writes.remove(&format!("new{n}.bin")).unwrap(), 2000));
        drop(writes);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// New files given only long after they were last changed: for what is to happen before they are.
    const UNHURRIED: super::Pace = super::Pace { least: Duration::from_secs(20), most: Duration::from_secs(20) };

    fn slow(served: &Served, files: BTreeMap<String, Given>) -> (Arc<Slow>, Receiver<()>, Sender<()>) {
        let (started, starts) = channel();
        let (goes_on, go_on) = channel();
        let owner = Slow {
            served: served.clone(),
            files: Mutex::new(files),
            started: Mutex::new(started),
            go_on: Mutex::new(go_on),
            changes: Mutex::default(),
        };
        (Arc::new(owner), starts, goes_on)
    }

    #[test]
    fn a_save_returns_at_once_and_is_the_projects_a_moment_later() {
        let dir = std::env::temp_dir().join(format!("pe2-writes-save-{}", std::process::id()));
        let served = Served::default();
        let files = BTreeMap::from([("saved.txt".to_string(), Given::Bytes(Arc::from(&b"before"[..])))]);
        served.set_files(&files);
        let (owner, starts, goes_on) = slow(&served, files);
        let writes = Writes::new(&served, owner.clone(), &dir).unwrap();
        let handle = writes.open("saved.txt", true).unwrap();
        writes.write(handle, 0, b"after").unwrap();
        // Let go of: it returns while the project has not taken it - which here waits to be told to go on.
        writes.close(handle, true).unwrap();
        starts.recv_timeout(Duration::from_secs(5)).expect("the project is being given the file, off this thread");
        assert!(owner.changes.lock().unwrap().is_empty(), "not the project's yet");
        assert_eq!(writes.read("saved.txt", 0, 16).unwrap(), b"after", "read as written meanwhile");
        goes_on.send(()).unwrap();
        let asked = Instant::now();
        let saved = || owner.changes.lock().unwrap().iter().any(|c| c == "Write saved.txt from outside");
        while !saved() && asked.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(saved(), "the project's a moment later");
        drop(writes);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_new_file_is_the_projects_once_its_burst_of_changes_has_ended() {
        let dir = std::env::temp_dir().join(format!("pe2-writes-burst-{}", std::process::id()));
        let served = Served::default();
        served.set_files(&BTreeMap::new());
        let owner = Arc::new(Told::default());
        let pace = super::Pace { least: Duration::from_millis(400), most: Duration::from_secs(2) };
        let writes = Writes::with(&served, owner.clone(), &dir, sfs::log::Config::default(), pace).unwrap();
        let given = || owner.batches.lock().unwrap().iter().flatten().count();
        // Written again and again, let go of in between: one burst, for as long as it goes on.
        for round in 0..25u64 {
            let handle = if round == 0 { writes.create("log.txt") } else { writes.open("log.txt", false) }.unwrap();
            writes.write(handle, round, b"x").unwrap();
            writes.close(handle, true).unwrap();
            std::thread::sleep(Duration::from_millis(20));
            assert_eq!(given(), 0, "not given while it is still being changed");
        }
        // Ended: given once the pause is several times its rhythm, with no second gone by.
        let ended = Instant::now();
        while given() == 0 && ended.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(given(), 1, "given once its burst has ended");
        drop(writes);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_folder_is_made_at_once_whoever_takes_it_when() {
        let dir = std::env::temp_dir().join(format!("pe2-writes-mkdir-{}", std::process::id()));
        let served = Served::default();
        served.set_files(&BTreeMap::new());
        // Nobody takes anything yet.
        let owner = Arc::new(Busy::default());
        let writes = Writes::new(&served, owner.clone(), &dir).unwrap();
        let asked = Instant::now();
        writes.make_folder("made").expect("made, whatever the project is doing");
        writes.make_folder("made/inside").expect("and one in it");
        assert!(asked.elapsed() < Duration::from_secs(1), "without waiting for the project");
        assert!(writes.shown("made/inside").is_some_and(|shown| shown.folder), "shown at once");
        std::thread::sleep(Duration::from_millis(300));
        assert!(owner.given.lock().unwrap().is_empty());
        // Someone takes changes again: given in the order they were made.
        owner.open.store(true, std::sync::atomic::Ordering::Relaxed);
        let given = || owner.given.lock().unwrap().clone();
        let waited = Instant::now();
        while given().len() < 2 && waited.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(given(), ["New made from outside", "New inside from outside"]);
        drop(writes);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn what_is_written_in_a_folder_goes_with_it_when_it_is_renamed() {
        let dir = std::env::temp_dir().join(format!("pe2-writes-moved-{}", std::process::id()));
        let served = Served::default();
        served.set_files(&BTreeMap::new());
        let owner = Arc::new(Told::default());
        let writes = Writes::with(&served, owner.clone(), &dir, sfs::log::Config::default(), UNHURRIED).unwrap();
        writes.make_folder("a").unwrap();
        let handle = writes.create("a/x.txt").unwrap();
        writes.write(handle, 0, b"x").unwrap();
        writes.close(handle, true).unwrap();
        // Renamed before what was written in it is the project's.
        writes.rename("a", "b").unwrap();
        assert!(writes.shown("a/x.txt").is_none(), "no longer where it was");
        assert_eq!(writes.read("b/x.txt", 0, 4).unwrap(), b"x", "read where its folder is now");
        assert_eq!(writes.list("b").keys().collect::<Vec<_>>(), ["x.txt"]);
        writes.finish();
        let given: Vec<String> = owner.batches.lock().unwrap().iter().flatten().cloned().collect();
        assert_eq!(given, ["New a from outside", "Rename a to b from outside", "Write x.txt from outside"]);
        drop(writes);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A project that takes nothing, ever.
    struct Refusing;

    impl Owner for Refusing {
        fn change(&self, _change: Change) -> Result<(), Refusal> {
            Err(Refusal::Never("not here".into()))
        }
    }

    #[test]
    fn a_rename_is_shown_at_once_and_the_project_told_in_its_turn() {
        let dir = std::env::temp_dir().join(format!("pe2-writes-rename-{}", std::process::id()));
        let served = Served::default();
        served.set_files(&BTreeMap::from([("old/in/a.txt".to_string(), Given::Bytes(Arc::from(&b"a"[..])))]));
        // Nobody takes anything yet.
        let owner = Arc::new(Busy::default());
        let writes = Writes::new(&served, owner.clone(), &dir).unwrap();
        let number = served.ino("old/in/a.txt");
        writes.rename("old", "new").expect("renamed, whatever the project is doing");
        assert!(writes.shown("old").is_none(), "no longer where it was");
        assert_eq!(writes.read("new/in/a.txt", 0, 4).unwrap(), b"a", "read where it is now");
        assert_eq!(writes.list("new").keys().collect::<Vec<_>>(), ["in"]);
        assert_eq!(served.ino("new/in/a.txt"), number, "each file keeps its number");
        // What is asked after it waits for it: nothing overtakes what was asked in an order.
        let handle = writes.open("new/in/a.txt", true).unwrap();
        writes.write(handle, 0, b"after").unwrap();
        writes.close(handle, true).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert!(owner.given.lock().unwrap().is_empty());
        // Someone takes changes again: the rename first, then what was written where it led.
        owner.open.store(true, std::sync::atomic::Ordering::Relaxed);
        let given = || owner.given.lock().unwrap().clone();
        let waited = Instant::now();
        while given().len() < 2 && waited.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(given(), ["Rename old to new from outside", "Write a.txt from outside"]);
        drop(writes);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_rename_the_project_refuses_is_put_back() {
        let dir = std::env::temp_dir().join(format!("pe2-writes-back-{}", std::process::id()));
        let served = Served::default();
        served.set_files(&BTreeMap::from([("old/a.txt".to_string(), Given::Bytes(Arc::from(&b"a"[..])))]));
        let writes = Writes::new(&served, Arc::new(Refusing), &dir).unwrap();
        writes.rename("old", "new").expect("shown renamed, until the project says otherwise");
        let back = || writes.shown("old/a.txt").is_some() && writes.shown("new").is_none();
        let waited = Instant::now();
        while !back() && waited.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(back(), "where it was, as the project has it");
        drop(writes);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn files(of: &[(&str, &str)]) -> BTreeMap<String, Given> {
        of.iter().map(|(path, text)| ((*path).to_string(), Given::Bytes(Arc::from(text.as_bytes())))).collect()
    }

    #[test]
    fn what_was_in_transit_is_taken_up_by_a_serving_that_starts_again() {
        let dir = std::env::temp_dir().join(format!("pe2-writes-again-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let project = files(&[("kept.txt", "before"), ("gone.txt", "g"), ("old/x.txt", "x"), ("part.txt", "0123456789")]);
        {
            let served = Served::default();
            served.set_files(&project);
            // Nobody takes anything, for as long as this serving lasts.
            let writes = Writes::new(&served, Arc::new(Busy::default()), &dir).unwrap();
            writes.make_folder("made").unwrap();
            let handle = writes.create("made/a.txt").unwrap();
            writes.write(handle, 0, b"one").unwrap();
            writes.close(handle, true).unwrap();
            let handle = writes.open("kept.txt", true).unwrap();
            writes.write(handle, 0, b"after").unwrap();
            writes.close(handle, true).unwrap();
            // Written in part over what it held, and made longer.
            let handle = writes.open("part.txt", false).unwrap();
            writes.write(handle, 3, b"xy").unwrap();
            writes.close(handle, true).unwrap();
            writes.truncate("part.txt", 12).unwrap();
            writes.remove("gone.txt").unwrap();
            writes.rename("old", "new").unwrap();
            // What an editor leaves beside a file is nobody's to find again.
            let handle = writes.create("made/.a.txt.swp").unwrap();
            writes.write(handle, 0, b"swap").unwrap();
            writes.close(handle, true).unwrap();
            // It ends with no word of it: its process gone.
        }
        let served = Served::default();
        served.set_files(&project);
        let owner = Arc::new(Busy::default());
        let writes = Writes::new(&served, owner.clone(), &dir).unwrap();
        // Shown as it was left.
        assert!(writes.shown("made").is_some_and(|shown| shown.folder));
        assert_eq!(writes.read("made/a.txt", 0, 16).unwrap(), b"one");
        assert_eq!(writes.read("kept.txt", 0, 16).unwrap(), b"after");
        assert_eq!(writes.read("part.txt", 0, 16).unwrap(), b"012xy56789\0\0");
        assert!(writes.shown("gone.txt").is_none() && writes.shown("old").is_none());
        assert_eq!(writes.read("new/x.txt", 0, 4).unwrap(), b"x");
        assert!(writes.shown("made/.a.txt.swp").is_none());
        // And given once someone takes it: what was asked in an order first, in that order.
        owner.open.store(true, std::sync::atomic::Ordering::Relaxed);
        let given = || owner.given.lock().unwrap().clone();
        let waited = Instant::now();
        while given().len() < 6 && waited.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut given = given();
        assert_eq!(given[..2], ["New made from outside", "Rename old to new from outside"]);
        given.sort();
        assert_eq!(
            given,
            [
                "New made from outside",
                "Remove gone.txt from outside",
                "Rename old to new from outside",
                "Write a.txt from outside",
                "Write kept.txt from outside",
                "Write part.txt from outside"
            ]
        );
        drop(writes);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A project that takes what is asked in an order - folders, renames - and nothing written yet.
    #[derive(Default)]
    struct Ordered {
        given: Mutex<Vec<String>>,
    }

    impl Owner for Ordered {
        fn change(&self, change: Change) -> Result<(), Refusal> {
            if matches!(change, Change::Write { .. }) {
                return Err(Refusal::Later("not yet".into()));
            }
            self.given.lock().unwrap().push(change.label());
            Ok(())
        }
    }

    #[test]
    fn what_was_written_in_a_folder_the_project_has_renamed_is_taken_up_where_it_is() {
        let dir = std::env::temp_dir().join(format!("pe2-writes-where-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        {
            let served = Served::default();
            served.set_files(&files(&[("old/x.txt", "x")]));
            let owner = Arc::new(Ordered::default());
            let writes = Writes::new(&served, owner.clone(), &dir).unwrap();
            let handle = writes.create("old/written.txt").unwrap();
            writes.write(handle, 0, b"w").unwrap();
            writes.close(handle, true).unwrap();
            writes.rename("old", "new").unwrap();
            // The project has the rename, and not what was written: there it ends.
            let waited = Instant::now();
            while owner.given.lock().unwrap().is_empty() && waited.elapsed() < Duration::from_secs(10) {
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(*owner.given.lock().unwrap(), ["Rename old to new from outside"]);
        }
        // The tree as the project has it: renamed.
        let served = Served::default();
        served.set_files(&files(&[("new/x.txt", "x")]));
        let owner = Arc::new(Told::default());
        let writes = Writes::new(&served, owner.clone(), &dir).unwrap();
        assert_eq!(writes.read("new/written.txt", 0, 4).unwrap(), b"w", "where its folder is now");
        assert!(writes.shown("old").is_none() && writes.shown("old/written.txt").is_none());
        let given = || owner.batches.lock().unwrap().iter().flatten().cloned().collect::<Vec<_>>();
        let waited = Instant::now();
        while given().is_empty() && waited.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(given(), ["Write written.txt from outside"], "the rename is not asked again");
        drop(writes);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn what_the_project_took_is_not_given_again() {
        let dir = std::env::temp_dir().join(format!("pe2-writes-took-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        {
            let served = Served::default();
            served.set_files(&BTreeMap::new());
            let owner = Arc::new(Told::default());
            let writes = Writes::new(&served, owner.clone(), &dir).unwrap();
            writes.make_folder("made").unwrap();
            let handle = writes.create("made/a.txt").unwrap();
            writes.write(handle, 0, b"one").unwrap();
            writes.close(handle, true).unwrap();
            let handle = writes.create("late.txt").unwrap();
            writes.write(handle, 0, b"first").unwrap();
            writes.close(handle, true).unwrap();
            let given = || owner.batches.lock().unwrap().iter().flatten().count();
            let waited = Instant::now();
            while given() < 3 && waited.elapsed() < Duration::from_secs(10) {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(given(), 3, "the folder and the two files");
            // Written again after the project took it, and ended before it took that.
            let handle = writes.open("late.txt", false).unwrap();
            writes.write(handle, 5, b" and more").unwrap();
        }
        // The tree as whoever holds the project has it by now.
        let served = Served::default();
        served.set_files(&files(&[("made/a.txt", "one"), ("late.txt", "first")]));
        let owner = Arc::new(Told::default());
        let writes = Writes::new(&served, owner.clone(), &dir).unwrap();
        assert_eq!(writes.read("late.txt", 0, 32).unwrap(), b"first and more");
        let given = || owner.batches.lock().unwrap().iter().flatten().cloned().collect::<Vec<_>>();
        let waited = Instant::now();
        while given().is_empty() && waited.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(given(), ["Write late.txt from outside"], "only what the project had not taken");
        assert!(writes.settled("made/a.txt"), "what it had is the tree's alone");
        drop(writes);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_serving_that_ended_leaves_nothing_to_take_up() {
        let dir = std::env::temp_dir().join(format!("pe2-writes-ended-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let served = Served::default();
        served.set_files(&BTreeMap::new());
        {
            let owner = Arc::new(Busy::default());
            let writes = Writes::new(&served, owner.clone(), &dir).unwrap();
            let handle = writes.create("left.txt").unwrap();
            writes.write(handle, 0, b"x").unwrap();
            writes.close(handle, true).unwrap();
            writes.finish();
            assert_eq!(*owner.left.lock().unwrap(), ["left.txt"], "its owner's to keep, as serving ends");
        }
        let owner = Arc::new(Told::default());
        let writes = Writes::new(&served, owner.clone(), &dir).unwrap();
        assert!(writes.shown("left.txt").is_none(), "not taken up again: it was set aside");
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(owner.batches.lock().unwrap().iter().flatten().count(), 0, "and nothing is given");
        drop(writes);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn what_nobody_took_by_the_end_is_left_with_the_owner() {
        let dir = std::env::temp_dir().join(format!("pe2-writes-left-{}", std::process::id()));
        let served = Served::default();
        served.set_files(&BTreeMap::new());
        let owner = Arc::new(Busy::default());
        let writes = Writes::new(&served, owner.clone(), &dir).unwrap();
        let handle = writes.create("late.txt").unwrap();
        writes.write(handle, 0, b"x").unwrap();
        writes.close(handle, true).unwrap();
        writes.finish();
        assert_eq!(*owner.left.lock().unwrap(), ["late.txt"]);
        drop(writes);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn new_files_that_have_settled_are_given_to_the_project_together() {
        let dir = std::env::temp_dir().join(format!("pe2-writes-many-{}", std::process::id()));
        let served = Served::default();
        served.set_files(&BTreeMap::new());
        let owner = Arc::new(Told::default());
        // Paced so that nothing is given before it is asked for here.
        let writes = Writes::with(&served, owner.clone(), &dir, sfs::log::Config::default(), UNHURRIED).unwrap();
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
        let writes = Writes::with(&served, owner.clone(), &dir, sfs::log::Config::default(), UNHURRIED).unwrap();
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
        std::thread::sleep(super::Pace::default().most + Duration::from_millis(600));
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
