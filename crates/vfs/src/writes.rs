//! Writing through the filesystem. What a program does is done at once in what is served - the filesystem the
//! project's processes share (`sfs::fs`, as `project::shared` says it), which the tree served is a view of - by
//! whoever serves the folder, and the project takes it in its turn:
//!
//! - What is written is appended to the log the project's processes share (`sfs::Log`), its bytes there, found
//!   again by the ranges each file keeps of it (`sfs::Content`), kept here by the file's number in what is served;
//!   a program reads back what it wrote at once.
//! - A file made, a folder made, is a node of what is served that says it is not the project's (`NEW`); a path
//!   moved is moved there, with all that is in it, in one key; a thing removed says so (`GONE`) and is shown no
//!   more. No path is kept here: what is known of a file goes with its number wherever its folder is moved.
//! - What the project is to take is said due (`due`), in the order it was done: a folder made and a path moved at
//!   once, in the commit that makes them; a written file once its program is done with it; a removal a moment
//!   later. Whoever holds the project is asked to look (`Owner::look`), takes all of it, and says what became of
//!   each in the commit that shows it so. Nothing a program does waits for that but a sync, which waits for the
//!   log to be on disk.
//!
//! When each is due:
//!
//! - A file the project has is due once the program closes it having written it, or syncs it.
//! - A file the project does not have is due once nothing has it open and its burst of changes has ended
//!   (`Pace`): one written under a temporary name and renamed over the file it saves is a save of that file, never
//!   a file of its own.
//! - A file removed is removed from the project a moment later, unless a file is made there meanwhile: removing
//!   a file and writing it again is one save.
//! - Junk (`write::is_junk`: swap files, backups, any name starting with `.`) is kept in memory, never due; it
//!   goes with the serving. A name ending in `.node` - storage's mark of a node - is refused.
//! - A folder made in a node waits for what is written in it (`WAITS`): a `node.toml` makes it a node, other
//!   files make it a folder of the node's files. Files written beside a new `node.toml` wait for the project to
//!   show its folder a node, so they are its files.
//! - A move the project refuses is put back where it was; a removal it refuses (a node's `node.toml` alone)
//!   shows the file again; a folder it refuses goes, where nothing was put in it.

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, PoisonError, RwLock, Weak};
use std::time::{Duration, Instant, SystemTime};

use project::shared::{GONE, JUNK, NEW, NODE, OF_FILES, SERVER, WAITS};
use sfs::fs::{self, ROOT, SERVED};
use sfs::space::Pages;

use crate::ops::Op;
use crate::write::{Owner, Refusal, Written, is_junk};
use crate::{Changes, Served};

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

/// How long until what is due and waits for the project to show something is looked at again: the tree changing
/// wakes what looks before.
const RECHECK: Duration = Duration::from_millis(100);

/// How long what says things due sleeps at most, with nothing to say.
const IDLE: Duration = Duration::from_millis(200);

/// How long after nobody took what is due whoever holds the project is asked to look again.
const AGAIN: Duration = Duration::from_secs(1);

/// A file in transit: its content - in the log, or in memory where it is junk - whether it was written since it
/// was last said due, and the number it is due by while nobody has said what became of that.
#[derive(Debug)]
struct Staged {
    content: Arc<RwLock<sfs::Content>>,
    dirty: bool,
    asked: Option<u64>,
}

fn reading(content: &RwLock<sfs::Content>) -> std::sync::RwLockReadGuard<'_, sfs::Content> {
    content.read().unwrap_or_else(PoisonError::into_inner)
}

fn writing(content: &RwLock<sfs::Content>) -> std::sync::RwLockWriteGuard<'_, sfs::Content> {
    content.write().unwrap_or_else(PoisonError::into_inner)
}

/// Something said due, until whoever holds the project says what became of it: what it is of, by its number in
/// what is served, and the record that said it was done (`sfs::Op`), which is answered in the log.
enum Asked {
    /// A file as it was written: what was said of its content, kept while it is read.
    Write { number: u64, content: Arc<RwLock<sfs::Content>>, given: Given },
    Folder { number: u64, said: Option<u64> },
    /// A path moved, already so in what is served: where from, where to, and what the move is known by there.
    Rename { number: u64, from: String, to: String, said: Option<u64>, moved: u64 },
    Remove { number: u64, said: Option<u64> },
}

impl State {
    /// Something is said due, now.
    fn ask(&mut self, due: u64, asked: Asked) {
        self.asked.insert(due, asked);
        self.asked_at.insert(due, Instant::now());
    }

    /// What was said due under a number is answered: how long that took is kept.
    fn answered(&mut self, due: u64) -> Option<Asked> {
        if let Some(at) = self.asked_at.remove(&due) {
            self.waited.push(u32::try_from(at.elapsed().as_micros()).unwrap_or(u32::MAX));
        }
        self.asked.remove(&due)
    }

    /// What was said due under a number is due no more, with no answer to it.
    fn asked_no_more(&mut self, due: u64) -> Option<Asked> {
        self.asked_at.remove(&due);
        self.asked.remove(&due)
    }
}

impl Asked {
    fn said(&self) -> Option<u64> {
        match self {
            Asked::Write { .. } => None,
            Asked::Folder { said, .. } | Asked::Rename { said, .. } | Asked::Remove { said, .. } => *said,
        }
    }
}

#[derive(Default)]
struct State {
    /// Files in transit, by their number in what is served.
    staged: HashMap<u64, Staged>,
    /// Files to say due, and when: new ones once their burst has ended, saved ones at once.
    new: HashMap<u64, Instant>,
    /// When each file waiting in `new` was last changed, and the time between its changes.
    touched: HashMap<u64, (Instant, Duration)>,
    /// What was removed (`GONE`), and when its removal is said due (none: never - a folder's removal covers it).
    removed: HashMap<u64, Option<Instant>>,
    /// The record of each removal the project is still to take (`sfs::Op::Removed`).
    removals: HashMap<u64, u64>,
    /// What was said due and not answered yet, by the number it is due by: in the order it was said.
    asked: BTreeMap<u64, Asked>,
    /// When each of those was said due, and how long each one answered waited for it, in microseconds: what the
    /// project's taking of what programs do costs them, said as serving ends.
    asked_at: HashMap<u64, Instant>,
    waited: Vec<u32>,
    /// When whoever holds the project is asked to look again, after nobody took what is due; and whether that was
    /// said.
    again: Option<Instant>,
    waiting: bool,
    /// When what says things due wakes next: told earlier where something is due before.
    wakes: Option<Instant>,
    /// Open files by handle: their number.
    handles: HashMap<u64, u64>,
    /// Files the project refused for good after the program that wrote them was answered, each with why: said to
    /// whoever next syncs or closes a handle on the file (`Writes::close`, `flush`), once - as a system says a
    /// write it could not carry out later - or until the project takes the file after all.
    refused: HashMap<u64, String>,
    next: u64,
}

impl State {
    /// Whether the project is to have what has this number, as far as it was said: a file or a folder said due and
    /// not answered.
    fn promised(&self, number: u64) -> bool {
        self.asked.values().any(|asked| match asked {
            Asked::Write { number: of, .. } | Asked::Folder { number: of, .. } => *of == number,
            _ => false,
        })
    }

    fn open(&self, number: u64) -> bool {
        self.handles.values().any(|open| *open == number)
    }
}

/// What was said of a staged file, for as long as it is read: the log kept from where it is, or the file it was
/// written out to.
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

/// The filesystem's writes: done in what is served, said due, taken by the project.
#[derive(Clone)]
pub struct Writes(Arc<Inner>);

struct Inner {
    served: Served,
    /// The filesystem the project's processes share, which the tree served is a view of.
    space: Arc<sfs::Space>,
    owner: Arc<dyn Owner>,
    /// Where a file is written out for who takes it whole, for as long as that takes.
    dir: PathBuf,
    /// The log what is written goes to.
    log: Arc<sfs::Log>,
    /// Every content there is, for as long as anything has it - staged, or being read this moment: the log is kept
    /// from the first position any of them reads from.
    contents: Mutex<Vec<Weak<RwLock<sfs::Content>>>>,
    /// Positions of the log kept for what was said due and is being read this moment.
    pins: Arc<Mutex<Vec<u64>>>,
    state: Mutex<State>,
    /// Held by what says things due and reads what became of them: one at a time.
    settling: Mutex<()>,
    /// What wakes the thread saying things due before it would wake by itself.
    wake: Arc<(Mutex<bool>, Condvar)>,
    /// Held while something is done that is said in the log, and in what is served, in the order it was done - a
    /// file begun, removed, renamed, a folder made, a thing said due: the log and what is due have them as they
    /// happened.
    ordering: Mutex<()>,
    /// The position of the record being done again, while what the log holds is taken up (`replay`): nothing is
    /// appended for it. `NOT` otherwise.
    replaying: std::sync::atomic::AtomicU64,
    pace: Pace,
}

/// No record is being done again: what is done is appended to the log.
const NOT: u64 = u64::MAX;

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(p, _)| p)
}

fn name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn joined(folder: &str, name: &str) -> String {
    if folder.is_empty() { name.to_string() } else { format!("{folder}/{name}") }
}

fn nanos_now() -> u64 {
    let since = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
    u64::try_from(since.as_nanos()).unwrap_or(u64::MAX)
}

/// What is at a path of what is served, shown or not: its number and its node.
fn found(pages: &impl Pages, path: &str) -> Option<(u64, fs::Node)> {
    let number = fs::at(pages, SERVED, path.trim_matches('/')).ok()??;
    Some((number, fs::node(pages, SERVED, number).ok()??))
}

/// What is shown at a path: there, and not removed.
fn there(pages: &impl Pages, path: &str) -> Option<(u64, fs::Node)> {
    found(pages, path).filter(|(_, node)| node.flags & GONE == 0)
}

fn not_found() -> io::Error {
    io::Error::from(io::ErrorKind::NotFound)
}

impl Writes {
    /// Writes kept in `dir` (emptied first: a log of its own, and what is written out for the owner), taken by
    /// whoever `owner` asks to look; a thread says what is due, for as long as any clone of them lives.
    ///
    /// # Errors
    /// The folder or its log cannot be made, or the tree served is no view of a shared filesystem.
    pub fn new(served: &Served, owner: Arc<dyn Owner>, dir: &Path) -> io::Result<Writes> {
        Writes::with(served, owner, dir, sfs::log::Config::default(), Pace::default())
    }

    /// As `new`, the log laid out and new files paced as said.
    ///
    /// # Errors
    /// The folder or its log cannot be made, or the tree served is no view of a shared filesystem.
    pub fn with(
        served: &Served,
        owner: Arc<dyn Owner>,
        dir: &Path,
        log: sfs::log::Config,
        pace: Pace,
    ) -> io::Result<Writes> {
        let Some(space) = served.space() else {
            return Err(io::Error::other("what is written through is a view of the shared filesystem"));
        };
        std::fs::create_dir_all(dir)?;
        // Emptied of what a serving before this one staged - but the log and the space the project's processes
        // share, which whoever holds the project may have open.
        let shared = |name: &std::ffi::OsStr| name == LOG || name == project::shared::SPACE;
        for entry in std::fs::read_dir(dir)?.flatten().filter(|entry| !shared(&entry.file_name())) {
            let path = entry.path();
            let _ = if path.is_dir() { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) };
        }
        let log = sfs::Log::shared(&dir.join(LOG), log)?;
        let wake: Arc<(Mutex<bool>, Condvar)> = Arc::default();
        let writes = Writes(Arc::new(Inner {
            served: served.clone(),
            space,
            owner,
            dir: dir.to_path_buf(),
            log,
            contents: Mutex::default(),
            pins: Arc::default(),
            state: Mutex::default(),
            settling: Mutex::default(),
            wake: wake.clone(),
            ordering: Mutex::default(),
            replaying: std::sync::atomic::AtomicU64::new(NOT),
            pace,
        }));
        // What a serving before this one left in what is served - made there and never the project's, removed
        // there, moved there - goes: what it left in transit is taken up from the log, which does it again.
        writes.wipe()?;
        writes.replay();
        // What is due is said off the filesystem's threads, for as long as the writes are kept: woken where
        // something is due before it would look, asleep until the next is.
        let weak = Arc::downgrade(&writes.0);
        // The tree served changed: what became of what was due may be said.
        let woken = Arc::downgrade(&wake);
        served.on_change(move || {
            if let Some(wake) = woken.upgrade() {
                *wake.0.lock().unwrap_or_else(PoisonError::into_inner) = true;
                wake.1.notify_all();
            }
        });
        let _ = std::thread::Builder::new().name("filesystem writes".into()).spawn(move || {
            loop {
                // Until the next of what waits is due - said, for whoever makes something due before to wake this.
                let sleep = {
                    let Some(inner) = weak.upgrade() else { return };
                    let writes = Writes(inner);
                    let mut state = writes.lock();
                    let now = Instant::now();
                    let next = Writes::next_due(&state);
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
                let Some(inner) = weak.upgrade() else { return };
                Writes(inner).settle();
            }
        });
        Ok(writes)
    }

    /// When the next thing is to be said due, or asked for again: none where nothing waits.
    fn next_due(state: &State) -> Option<Instant> {
        // A file still open, or said due and not answered, is not due whenever it says: letting go of it, and its
        // answer, wake this.
        let files = state.new.iter().filter(|(number, _)| {
            !state.open(**number) && state.staged.get(*number).is_none_or(|staged| staged.asked.is_none())
        });
        let again = state.again.filter(|_| !state.asked.is_empty());
        files.map(|(_, due)| *due).chain(state.removed.values().flatten().copied()).chain(again).min()
    }

    /// Something is due at `due`: what says things due is woken, where it would sleep past it.
    fn due(&self, state: &State, due: Instant) {
        if state.wakes.is_none_or(|wakes| wakes > due) {
            self.wake_now();
        }
    }

    fn wake_now(&self) {
        *self.0.wake.0.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.0.wake.1.notify_all();
    }

    /// A new file changed: it is the project's once its burst of changes has ended - the pause since this change
    /// several times the time between its changes.
    fn touch(&self, state: &mut State, number: u64) {
        let now = Instant::now();
        let rhythm = match state.touched.get(&number) {
            Some((last, rhythm)) => {
                let since = now.saturating_duration_since(*last);
                if rhythm.is_zero() { since } else { (*rhythm * 3 + since) / 4 }
            }
            None => Duration::ZERO,
        };
        state.touched.insert(number, (now, rhythm));
        let due = now + (rhythm * RHYTHMS).clamp(self.0.pace.least, self.0.pace.most);
        state.new.insert(number, due);
        self.due(state, due);
    }

    /// Say in the log what was done: where it is said. Done again from the log, it is said there already.
    fn record(&self, op: &sfs::Op) -> io::Result<u64> {
        match self.0.replaying.load(std::sync::atomic::Ordering::Relaxed) {
            NOT => op.append(&self.0.log),
            position => Ok(position),
        }
    }

    fn replaying(&self) -> bool {
        self.0.replaying.load(std::sync::atomic::Ordering::Relaxed) != NOT
    }

    fn ordered(&self) -> std::sync::MutexGuard<'_, ()> {
        self.0.ordering.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// What is served, as it is this moment.
    fn pages(&self) -> io::Result<sfs::space::Snapshot<'_>> {
        Ok(self.0.space.read()?)
    }

    /// The path of what has a number in what is served.
    fn path_of(&self, number: u64) -> Option<String> {
        fs::path(&self.pages().ok()?, SERVED, number).ok().flatten()
    }

    /// Change what a node of what is served says of itself, in a commit of its own.
    fn flag(&self, number: u64, set: u32, clear: u32) -> io::Result<()> {
        let mut txn = self.0.space.write()?;
        let Some(node) = fs::node(&txn, SERVED, number)? else { return Ok(()) };
        let flags = (node.flags | set) & !clear;
        if flags != node.flags {
            fs::set(&mut txn, SERVED, number, &fs::Node { flags, ..node }, SERVER)?;
            txn.commit()?;
        }
        Ok(())
    }

    /// Make a node of what is served that is not the project's, at a path whose folder is there: its number.
    fn make(&self, path: &str, folder: bool, flags: u32) -> io::Result<u64> {
        let mut txn = self.0.space.write()?;
        let number = Self::make_in(&mut txn, path, folder, flags)?;
        txn.commit()?;
        Ok(number)
    }

    /// The folders above a path that are not there, made here and not the project's.
    fn make_above(&self, path: &str) -> io::Result<()> {
        let mut txn = self.0.space.write()?;
        let mut made = false;
        for (at, _) in path.match_indices('/') {
            if found(&txn, &path[..at]).is_none() {
                Self::make_in(&mut txn, &path[..at], true, 0)?;
                made = true;
            }
        }
        if made {
            txn.commit()?;
        }
        Ok(())
    }

    fn make_in(txn: &mut sfs::space::Txn<'_>, path: &str, folder: bool, flags: u32) -> io::Result<u64> {
        let above = there(txn, parent(path)).filter(|(_, node)| node.folder).map(|(number, _)| number);
        let above = above.ok_or_else(not_found)?;
        let node = fs::Node {
            folder,
            flags: NEW | flags,
            modified: nanos_now(),
            parent: above,
            name: name(path).to_string(),
            ..fs::Node::default()
        };
        Ok(fs::make(txn, SERVED, &node, SERVER)?)
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
        self.0.contents.lock().unwrap_or_else(PoisonError::into_inner).push(Arc::downgrade(&content));
        Ok(content)
    }

    /// Let go of the log before what anything is found again from: every content there is, what was said due and
    /// is being read, what the project is still to be told. A file that waits long while much is written after it
    /// begins again further on (`Content::moved_forward`), so that it alone does not keep all of that.
    fn let_go_of_log(&self, written: u64) {
        let far = self.0.log.config().budget / 4;
        // A file said due long ago, which nobody took, is said again as it is now - moved on in the log where it
        // is itself far behind - under the number it is due by: the log is kept from there.
        let said_long_ago: Vec<(u64, u64, Arc<RwLock<sfs::Content>>)> = {
            let state = self.lock();
            let old = state.asked.iter().filter_map(|(due, asked)| match asked {
                Asked::Write { number, content, given } if given.pin.is_some_and(|pin| pin + far < written) => {
                    Some((*due, *number, content.clone()))
                }
                _ => None,
            });
            old.collect()
        };
        for (due, number, content) in said_long_ago {
            let _ordered = self.ordered();
            let Some(path) = self.path_of(number) else { continue };
            if reading(&content).first().is_some_and(|first| first + far < written) {
                let Ok(id) = self.record(&sfs::Op::Began { path: path.clone(), emptied: false }) else { continue };
                if writing(&content).moved_forward(&self.0.log, id).is_err() {
                    continue;
                }
            }
            let Ok((from, given)) = self.given(&content) else { continue };
            if matches!(crate::due::replace(&self.0.space, due, &Op::Write { path, from }), Ok(true)) {
                let mut state = self.lock();
                if let Some(staged) = state.staged.get_mut(&number).filter(|staged| Arc::ptr_eq(&staged.content, &content)) {
                    staged.dirty = reading(&content).changes() != given.changes;
                }
                state.ask(due, Asked::Write { number, content, given });
            }
        }
        let behind: Vec<(u64, Arc<RwLock<sfs::Content>>)> = {
            let state = self.lock();
            let behind = state.staged.iter().filter(|(_, staged)| {
                reading(&staged.content).first().is_some_and(|first| first + far < written)
            });
            behind.map(|(number, staged)| (*number, staged.content.clone())).collect()
        };
        for (number, content) in behind {
            let _ordered = self.ordered();
            let Some(path) = self.path_of(number) else { continue };
            // Still what is staged there, and whether the project has it as it is.
            let dirty = match self.lock().staged.get(&number) {
                Some(staged) if Arc::ptr_eq(&staged.content, &content) => staged.dirty || staged.asked.is_some(),
                _ => continue,
            };
            let Ok(id) = self.record(&sfs::Op::Began { path, emptied: false }) else { continue };
            let moved = writing(&content).moved_forward(&self.0.log, id);
            if moved.is_ok() && !dirty {
                let _ = self.record(&sfs::Op::Taken { file: id, upto: self.0.log.header().tail() });
            }
        }
        let live: Vec<Arc<RwLock<sfs::Content>>> = {
            let mut contents = self.0.contents.lock().unwrap_or_else(PoisonError::into_inner);
            contents.retain(|content| content.strong_count() > 0);
            contents.iter().filter_map(Weak::upgrade).collect()
        };
        let mut needed = written;
        for content in live {
            needed = needed.min(reading(&content).first().unwrap_or(needed));
        }
        let pinned = self.0.pins.lock().unwrap_or_else(PoisonError::into_inner).iter().copied().min();
        needed = needed.min(pinned.unwrap_or(needed));
        {
            let state = self.lock();
            let told = state.asked.values().filter_map(Asked::said).chain(state.removals.values().copied()).min();
            needed = needed.min(told.unwrap_or(needed));
        }
        // From there the log is needed for what is in transit; what whoever holds the project needs of it, that one
        // says (`Header::storage`).
        self.0.log.header().set_transit(Some(needed));
        self.0.log.keep();
    }

    /// What a staged file holds this moment, said by where it is in the log for whoever takes it to read it there:
    /// the log is kept for it until what this returns with it is dropped. With the file's number and where the log
    /// ended then - what is written to it after is not what was said. Written out to a file where it is not in the
    /// log.
    fn given(&self, content: &RwLock<sfs::Content>) -> io::Result<(Written, Given)> {
        let (len, ranges, first, taken, changes) = {
            let content = reading(content);
            // Read under the file's lock, which every write to it holds as it appends: none is half in.
            let taken = (content.id(), self.0.log.header().tail());
            (content.len(), content.ranges(), content.first(), taken, content.changes())
        };
        let pins = self.0.pins.clone();
        let Some(ranges) = ranges else {
            let file = self.written_out(content)?;
            let given = Given { pins, pin: None, file: Some(file.clone()), taken: None, changes };
            return Ok((Written::File(file), given));
        };
        // Pinned before the log could be let go of past it: the content is still held by who asked.
        if let Some(first) = first {
            pins.lock().unwrap_or_else(PoisonError::into_inner).push(first);
        }
        let written = Written::Logged { log: self.0.log.dir().to_path_buf(), len, ranges };
        Ok((written, Given { pins, pin: first, file: None, taken: Some(taken), changes }))
    }

    /// A staged file's content written out to a file of its own, for who takes it whole: its path, to be removed
    /// once taken.
    fn written_out(&self, content: &RwLock<sfs::Content>) -> io::Result<PathBuf> {
        let number = {
            let mut state = self.lock();
            state.next += 1;
            state.next
        };
        let path = self.0.dir.join(format!("{number}"));
        let mut out = io::BufWriter::new(std::fs::File::create(&path)?);
        reading(content).write_to(&self.0.log, &mut out)?;
        io::Write::flush(&mut out)?;
        Ok(path)
    }

    /// What a serving before this one left in what is served goes: what was moved there and the project never
    /// took is put back, last first; what was made there and is not the project's is taken away; what was removed
    /// there is shown again. What that serving left in transit is done again from the log.
    fn wipe(&self) -> io::Result<()> {
        let mut txn = self.0.space.write()?;
        let mut changed = false;
        for (moved, from, to) in project::shared::moves_said(&txn)?.into_iter().rev() {
            let back = found(&txn, parent(&from)).filter(|_| found(&txn, &from).is_none());
            if let (Some((number, _)), Some((above, _))) = (found(&txn, &to), back) {
                fs::rename(&mut txn, SERVED, number, above, name(&from), SERVER)?;
            }
            project::shared::unmoved(&mut txn, moved)?;
            changed = true;
        }
        let mut folders = vec![ROOT];
        while let Some(folder) = folders.pop() {
            let mut inside = Vec::new();
            fs::list(&txn, SERVED, folder, None, &mut |_, number| {
                inside.push(number);
                true
            })?;
            for number in inside {
                let Some(node) = fs::node(&txn, SERVED, number)? else { continue };
                if node.flags & NEW != 0 {
                    fs::remove(&mut txn, SERVED, number, SERVER)?;
                    changed = true;
                    continue;
                }
                if node.folder {
                    folders.push(number);
                }
                if node.flags & GONE != 0 {
                    fs::set(&mut txn, SERVED, number, &fs::Node { flags: node.flags & !GONE, ..node }, SERVER)?;
                    changed = true;
                }
            }
        }
        if changed {
            txn.commit()?;
        }
        Ok(())
    }

    /// What a path is: as what is served has it, with what was written to it and is not the project's yet.
    #[must_use]
    pub fn shown(&self, path: &str) -> Option<Shown> {
        let (number, node) = there(&self.pages().ok()?, path)?;
        if !node.folder
            && let Some(staged) = self.lock().staged.get(&number)
        {
            let content = reading(&staged.content);
            return Some(Shown { folder: false, size: content.len(), modified: content.modified() });
        }
        let modified = SystemTime::UNIX_EPOCH + Duration::from_nanos(node.modified);
        Some(Shown { folder: node.folder, size: node.len, modified })
    }

    /// Whether a path is the project's alone: nothing written to it that the project is yet to take, not made or
    /// removed here. What it is then changes only as whoever holds the project says, which is told to whatever
    /// caches it.
    #[must_use]
    pub fn settled(&self, path: &str) -> bool {
        let Some((number, node)) = self.pages().ok().and_then(|pages| found(&pages, path)) else { return true };
        let state = self.lock();
        node.flags & (NEW | GONE) == 0 && !state.staged.contains_key(&number) && !state.new.contains_key(&number)
    }

    /// A folder's children as shown: each name, with whether it is a folder.
    #[must_use]
    pub fn list(&self, path: &str) -> BTreeMap<String, bool> {
        let Ok(pages) = self.pages() else { return BTreeMap::new() };
        let Some((number, _)) = there(&pages, path).filter(|(_, node)| node.folder) else { return BTreeMap::new() };
        let mut inside = Vec::new();
        let _ = fs::list(&pages, SERVED, number, None, &mut |name, number| {
            inside.push((name.to_string(), number));
            true
        });
        let shown = |number: u64| fs::node(&pages, SERVED, number).ok().flatten().filter(|node| node.flags & GONE == 0);
        inside.into_iter().filter_map(|(name, number)| Some((name, shown(number)?.folder))).collect()
    }

    /// The names made in a folder that are not the project's yet: few, whatever the folder holds.
    #[must_use]
    pub fn staged_in(&self, folder: &str) -> Vec<String> {
        let Ok(pages) = self.pages() else { return Vec::new() };
        let Some((number, _)) = there(&pages, folder) else { return Vec::new() };
        let mut inside = Vec::new();
        let _ = fs::list(&pages, SERVED, number, None, &mut |name, number| {
            inside.push((name.to_string(), number));
            true
        });
        let new = |number: u64| {
            fs::node(&pages, SERVED, number).ok().flatten().is_some_and(|node| node.flags & (NEW | GONE) == NEW)
        };
        inside.into_iter().filter(|(_, number)| new(*number)).map(|(name, _)| name).collect()
    }

    /// Up to `len` bytes of a file from `offset`: as written, or the project's.
    ///
    /// # Errors
    /// It is not there, or cannot be read.
    pub fn read(&self, path: &str, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        let mut out = vec![0u8; len];
        let read = self.read_into(path, offset, &mut out)?;
        out.truncate(read);
        Ok(out)
    }

    /// Bytes of a file from `offset` read into a buffer, as many as it takes or the file has: as written, or the
    /// project's.
    ///
    /// # Errors
    /// It is not there, or cannot be read.
    pub fn read_into(&self, path: &str, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
        let (number, node) = there(&self.pages()?, path).ok_or_else(not_found)?;
        let staged = self.lock().staged.get(&number).map(|s| s.content.clone());
        if let Some(content) = staged {
            return reading(&content).read_into(&self.0.log, offset, buffer);
        }
        // Made here, with nothing written to it.
        if node.flags & NEW != 0 {
            return Ok(0);
        }
        let tree = self.0.served.tree();
        let (ino, _) = tree.at(path).ok_or_else(not_found)?;
        tree.read_into(ino, offset, buffer)
    }

    /// A file staged: its content now - the project's, unless `empty` - brought into the log where it is not
    /// staged yet, so that what is written to it next is written over the file as it was then. What will never be
    /// the project's is kept in memory instead.
    fn stage(&self, number: u64, node: &fs::Node, path: &str, empty: bool) -> io::Result<Arc<RwLock<sfs::Content>>> {
        let staged = self.lock().staged.get(&number).map(|s| s.content.clone());
        if let Some(content) = staged {
            if empty {
                let mut content = writing(&content);
                if content.is_logged() {
                    self.record(&sfs::Op::Truncated { file: content.id(), len: 0 })?;
                }
                content.truncate(0);
            }
            return Ok(content);
        }
        let content = self.begin(path, empty)?;
        if !empty && node.flags & NEW == 0 {
            let tree = self.0.served.tree();
            if let Some((ino, shown)) = tree.at(path) {
                let mut at = 0u64;
                let mut filled = writing(&content);
                while at < shown.size {
                    let bytes = tree.read(ino, at, 1 << 20)?;
                    if bytes.is_empty() {
                        break;
                    }
                    filled.fill(&self.0.log, at, &bytes)?;
                    at += bytes.len() as u64;
                }
            }
        }
        self.lock().staged.insert(number, Staged { content: content.clone(), dirty: false, asked: None });
        Ok(content)
    }

    /// What was removed here is shown again - made anew where it was removed: its removal is told to nobody.
    fn unhide(&self, number: u64) -> io::Result<()> {
        {
            let mut state = self.lock();
            state.removed.remove(&number);
            state.removals.remove(&number);
        }
        self.flag(number, 0, GONE)
    }

    /// Something is to be where something removed here still is, in what is served, or where something the
    /// project has is replaced: its removal is said due now, in its turn, and it is out of what is served.
    fn vacate(&self, number: u64, path: &str) -> io::Result<()> {
        let mut state = self.lock();
        state.removed.remove(&number);
        state.staged.remove(&number);
        state.new.remove(&number);
        let said = state.removals.remove(&number);
        let asked = state.asked.values().any(|asked| matches!(asked, Asked::Remove { number: of, .. } if *of == number));
        let mut txn = self.0.space.write()?;
        let project = fs::node(&txn, SERVED, number)?.is_some_and(|node| node.flags & NEW == 0) || state.promised(number);
        let due = if asked || !project {
            None
        } else {
            Some(crate::due::put_in(&mut txn, &Op::Remove { path: path.to_string() }, None)?)
        };
        fs::remove(&mut txn, SERVED, number, SERVER)?;
        txn.commit()?;
        if let Some(due) = due {
            state.ask(due, Asked::Remove { number, said });
            self.wake_now();
        }
        Ok(())
    }

    /// Open a file for writing (`truncate`: emptied); its handle.
    ///
    /// # Errors
    /// It is not there, or cannot be staged.
    pub fn open(&self, path: &str, truncate: bool) -> io::Result<u64> {
        let _ordered = self.ordered();
        let at = there(&self.pages()?, path);
        let (number, node) = match at {
            Some(at) => at,
            // Opened to be written anew where nothing is: made.
            None if truncate && !crate::write::refused(path) => self.made_file(path)?,
            None => return Err(not_found()),
        };
        self.opened(number, &node, path, truncate)
    }

    fn opened(&self, number: u64, node: &fs::Node, path: &str, truncate: bool) -> io::Result<u64> {
        self.stage(number, node, path, truncate)?;
        let mut state = self.lock();
        if truncate && let Some(staged) = state.staged.get_mut(&number) {
            staged.dirty = true;
        }
        state.next += 1;
        let handle = state.next;
        state.handles.insert(handle, number);
        Ok(handle)
    }

    /// Make a file, empty, open for writing; its handle. A file made where one was removed a moment ago saves it.
    ///
    /// # Errors
    /// Its name is refused, its folder is not there, or it cannot be staged.
    pub fn create(&self, path: &str) -> io::Result<u64> {
        if crate::write::refused(path) {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        let _ordered = self.ordered();
        let at = found(&self.pages()?, path);
        let (number, node) = match at {
            // Removed a moment ago: made again, it is not removed after all.
            Some((number, node)) if node.flags & GONE != 0 && !node.folder => {
                self.unhide(number)?;
                (number, fs::Node { flags: node.flags & !GONE, ..node })
            }
            Some((number, node)) if node.flags & GONE != 0 => {
                self.vacate(number, path)?;
                let _ = (number, node);
                self.made_file(path)?
            }
            Some(at) => at,
            None => self.made_file(path)?,
        };
        let handle = self.opened(number, &node, path, true)?;
        // One the project does not have, and is not to yet: its once its burst of changes has ended.
        let mut state = self.lock();
        if node.flags & (NEW | JUNK) == NEW && !state.promised(number) {
            self.touch(&mut state, number);
        }
        Ok(handle)
    }

    fn made_file(&self, path: &str) -> io::Result<(u64, fs::Node)> {
        let flags = if is_junk(path) { JUNK } else { 0 };
        let number = self.make(path, false, flags)?;
        Ok((number, fs::Node { flags: NEW | flags, ..fs::Node::default() }))
    }

    /// Write at an offset of an open file.
    ///
    /// # Errors
    /// The handle is not open, or the log cannot be written.
    pub fn write(&self, handle: u64, offset: u64, data: &[u8]) -> io::Result<usize> {
        let content = {
            let mut state = self.lock();
            let number = state.handles.get(&handle).copied().ok_or_else(not_found)?;
            if state.new.contains_key(&number) && state.staged.get(&number).is_some_and(|staged| staged.asked.is_none()) {
                self.touch(&mut state, number);
            }
            let staged = state.staged.get_mut(&number).ok_or_else(not_found)?;
            staged.dirty = true;
            staged.content.clone()
        };
        // The log first, then the file's ranges: both under the file's own lock, so that two writes to one file
        // are in the file in the order they are in the log.
        writing(&content).write(&self.0.log, offset, data)?;
        Ok(data.len())
    }

    /// Set a file's length: staged, said due as a write.
    ///
    /// # Errors
    /// It is not there, or cannot be staged.
    pub fn truncate(&self, path: &str, len: u64) -> io::Result<()> {
        let _ordered = self.ordered();
        let (number, node) = there(&self.pages()?, path).ok_or_else(not_found)?;
        let content = self.stage(number, &node, path, false)?;
        if let Some(staged) = self.lock().staged.get_mut(&number) {
            staged.dirty = true;
        }
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
        let Some((number, _)) = self.pages().ok().and_then(|pages| found(&pages, path)) else { return false };
        self.lock().open(number)
    }

    /// A handle let go of, or a file synced: what it wrote is due, where the project has the file - said off this
    /// thread, at once. Synced, what was written is on disk in the log before this returns: from there it is the
    /// project's whatever becomes of this process (`replay`).
    ///
    /// # Errors
    /// Synced, the log could not be had on disk; or the project did not take what was written to the file before.
    pub fn close(&self, handle: u64, release: bool) -> io::Result<()> {
        {
            let mut state = self.lock();
            let number = if release { state.handles.remove(&handle) } else { state.handles.get(&handle).copied() };
            let Some(number) = number else { return Ok(()) };
            let now = Instant::now();
            let written = state.staged.get(&number).map(|staged| (staged.dirty, reading(&staged.content).is_logged()));
            if let Some(due) = state.new.get(&number).copied() {
                // A new file: its burst is told from when it was last changed, not from when it was let go of.
                if release {
                    self.due(&state, due);
                }
            } else if written == Some((true, true)) {
                state.new.insert(number, now);
                self.due(&state, now);
            } else if release && written == Some((false, true)) {
                drop(state);
                self.let_go(number);
            }
        }
        if release {
            return Ok(());
        }
        self.0.log.flush(self.0.log.header().tail())?;
        self.flush(handle)
    }

    /// A file's content is kept here no longer where it is the project's as it is: nothing written to it since,
    /// nothing has it open, and the project has it - what is shown of it is the project's from then on.
    fn let_go(&self, number: u64) {
        let Some(node) = self.pages().ok().and_then(|pages| fs::node(&pages, SERVED, number).ok().flatten()) else {
            self.lock().staged.remove(&number);
            return;
        };
        let mut state = self.lock();
        let clean = state.staged.get(&number).is_some_and(|staged| !staged.dirty && staged.asked.is_none());
        if clean && node.flags & NEW == 0 && !state.open(number) && !state.new.contains_key(&number) {
            state.staged.remove(&number);
        }
    }

    /// A program closes a handle, or syncs it: told now what the project refused of the file after the program
    /// was last answered - once, to the first that asks.
    ///
    /// # Errors
    /// The project did not take what was written to the file earlier, and why.
    pub fn flush(&self, handle: u64) -> io::Result<()> {
        let mut state = self.lock();
        let Some(number) = state.handles.get(&handle).copied() else { return Ok(()) };
        match state.refused.remove(&number) {
            Some(why) => Err(io::Error::other(format!("what was written before was not taken into the project: {why}"))),
            None => Ok(()),
        }
    }

    /// Remove a file or an empty folder: shown no more at once, removed from the project when its removal is
    /// due. What is not the project's goes at once.
    ///
    /// # Errors
    /// It is not there.
    pub fn remove(&self, path: &str) -> io::Result<()> {
        let _ordered = self.ordered();
        let (number, node) = there(&self.pages()?, path).ok_or_else(not_found)?;
        let junk = node.flags & JUNK != 0 || is_junk(path);
        let said = if junk { None } else { Some(self.record(&sfs::Op::Removed { path: path.to_string() })?) };
        let mut state = self.lock();
        state.staged.remove(&number);
        state.new.remove(&number);
        state.touched.remove(&number);
        state.refused.remove(&number);
        if node.flags & NEW == 0 || state.promised(number) {
            // A moment for a file to be made there again: removed and written anew is one save.
            let due = Instant::now() + self.0.pace.most;
            state.removed.insert(number, Some(due));
            if let Some(said) = said {
                state.removals.insert(number, said);
            }
            self.due(&state, due);
            drop(state);
            return self.flag(number, GONE, 0);
        }
        drop(state);
        let mut txn = self.0.space.write()?;
        fs::remove(&mut txn, SERVED, number, SERVER)?;
        Ok(txn.commit()?)
    }

    /// Make a folder: in a folder, at once, and said due in the same commit; in a node, waiting for what is
    /// written in it; where one was removed a moment ago, by keeping that one.
    ///
    /// # Errors
    /// Its name is refused, something is there, or its folder is not.
    pub fn make_folder(&self, path: &str) -> io::Result<()> {
        if crate::write::refused(path) {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        let _ordered = self.ordered();
        let junk = is_junk(path);
        let said = if junk { None } else { Some(self.record(&sfs::Op::MadeFolder { path: path.to_string() })?) };
        match found(&self.pages()?, path) {
            Some((number, node)) if node.flags & GONE != 0 => {
                // Made where one was removed a moment ago, its removal not said yet: it is not removed after all.
                // What was removed from it stays removed.
                let pending = matches!(self.lock().removed.get(&number), Some(Some(_)));
                if node.folder && pending {
                    return self.unhide(number);
                }
                self.vacate(number, path)?;
            }
            // Taken up from the log with the project having it: it is made.
            Some(_) if self.replaying() => return Ok(()),
            Some(_) => return Err(io::Error::from(io::ErrorKind::AlreadyExists)),
            None => {}
        }
        // In a node, it is nothing of the project's until something is written in it; nor is junk, ever.
        if junk || self.in_node(path) {
            return self.make(path, true, if junk { JUNK } else { WAITS }).map(drop);
        }
        let mut txn = self.0.space.write()?;
        let number = Self::make_in(&mut txn, path, true, 0)?;
        let due = crate::due::put_in(&mut txn, &Op::Folder { path: path.to_string() }, None)?;
        txn.commit()?;
        self.lock().ask(due, Asked::Folder { number, said });
        self.wake_now();
        Ok(())
    }

    /// Whether a new path is inside a node: its nearest folder there is a node, one of a node's files, or one made
    /// in a node and waiting.
    fn in_node(&self, path: &str) -> bool {
        let Ok(pages) = self.pages() else { return false };
        let mut at = path;
        while let Some((above, _)) = at.rsplit_once('/') {
            if let Some((_, node)) = found(&pages, above) {
                return node.flags & (WAITS | JUNK | NODE | OF_FILES) != 0;
            }
            at = above;
        }
        false
    }

    /// A number of this serving's own, for what no record of the log tells apart.
    fn unique(&self) -> u64 {
        let mut state = self.lock();
        state.next += 1;
        state.next | (1 << 63)
    }

    /// Rename: a file with content here, moved onto a file that is there, saves that one and goes; a file renamed to
    /// junk (a backup) is kept under that name and hidden, its removal waiting on the file's return; what is only
    /// here is moved here; anything else is moved in what is served at once, and the move said due in the same
    /// commit.
    ///
    /// # Errors
    /// Its new name is refused, or nothing is there.
    pub fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        if crate::write::refused(to) {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        let _ordered = self.ordered();
        let replaying = self.replaying();
        let (number, node) = there(&self.pages()?, from).ok_or_else(not_found)?;
        if from == to {
            return Ok(());
        }
        let (junk_from, junk_to) = (is_junk(from), is_junk(to));
        // What is where it goes. Removed a moment ago: a file moved onto a file saves it, anything else is in the
        // way no more.
        let mut onto = found(&self.pages()?, to);
        if let Some((there, old)) = onto.take() {
            if old.flags & GONE == 0 {
                onto = Some((there, old));
            } else if !node.folder && !old.folder {
                self.unhide(there)?;
                onto = Some((there, fs::Node { flags: old.flags & !GONE, ..old }));
            } else {
                self.vacate(there, to)?;
            }
        }
        let project = node.flags & NEW == 0 || self.lock().promised(number);
        let onto_file = onto.as_ref().is_some_and(|(_, old)| !old.folder) && !node.folder;
        // A file the project has, moved onto a file that is there: staged first, as the project has it - and
        // before the rename is said, so that the log has it in that order.
        if onto_file && project && !replaying && !junk_from && !junk_to && !self.lock().staged.contains_key(&number) {
            self.stage(number, &node, from, false)?;
        }
        // Said of what the log knows of: what was never the project's to have is found again by nothing.
        let said = if junk_from {
            None
        } else {
            Some(self.record(&sfs::Op::Renamed { from: from.to_string(), to: to.to_string() })?)
        };
        let staged = self.lock().staged.contains_key(&number);
        if let Some((there, old)) = onto.as_ref().filter(|_| onto_file && staged && !junk_to) {
            return self.saved_over((number, project, from), (*there, old, to), said);
        }
        if junk_to && !node.folder {
            return self.put_aside((number, &node, project, from), onto.as_ref(), to);
        }
        // Taken up from the log with the project having the move already - the folder there, and here only for
        // what was written in it: that is where its folder is now.
        if let Some((there, _)) = onto.as_ref().filter(|(_, old)| replaying && node.folder && !project && old.folder) {
            let mut txn = self.0.space.write()?;
            let mut inside = Vec::new();
            fs::list(&txn, SERVED, number, None, &mut |name, number| {
                inside.push((name.to_string(), number));
                true
            })?;
            for (name, inner) in inside {
                if fs::lookup(&txn, SERVED, *there, &name)?.is_none() {
                    fs::rename(&mut txn, SERVED, inner, *there, &name, SERVER)?;
                }
            }
            fs::remove(&mut txn, SERVED, number, SERVER)?;
            return Ok(txn.commit()?);
        }
        // Something else is where it goes: in the way no more, and said so in its turn where the project has it.
        let replaced = onto.as_ref().map(|(_, old)| old.folder);
        if let Some((there, _)) = onto {
            self.vacate(there, to)?;
        }
        if !project {
            // Only here - a file written and not the project's yet, a folder waiting in a node: moved here.
            let mut txn = self.0.space.write()?;
            let above = there(&txn, parent(to)).filter(|(_, node)| node.folder).ok_or_else(not_found)?.0;
            fs::rename(&mut txn, SERVED, number, above, name(to), SERVER)?;
            if junk_from && !junk_to && node.flags & JUNK != 0 {
                let moved = fs::node(&txn, SERVED, number)?.ok_or_else(not_found)?;
                fs::set(&mut txn, SERVED, number, &fs::Node { flags: moved.flags & !JUNK, ..moved }, SERVER)?;
            }
            txn.commit()?;
            // The kernel keeps the renamed file's number for its new name.
            self.0.served.rename(from, to);
            if !node.folder {
                self.named_at_last(number, to)?;
            }
            return Ok(());
        }
        // The project's: moved in what is served at once, with what is in it; the project is told in its turn.
        let moved = said.unwrap_or_else(|| self.unique());
        let mut txn = self.0.space.write()?;
        let above = there(&txn, parent(to)).filter(|(_, node)| node.folder).ok_or_else(not_found)?.0;
        fs::rename(&mut txn, SERVED, number, above, name(to), SERVER)?;
        project::shared::moved(&mut txn, moved, from, to)?;
        let op = Op::Rename { from: from.to_string(), to: to.to_string() };
        let due = crate::due::put_in(&mut txn, &op, Some(moved))?;
        txn.commit()?;
        self.0.served.moved_there(from, to, node.folder, replaced);
        let mut state = self.lock();
        state.ask(due, Asked::Rename { number, from: from.to_string(), to: to.to_string(), said, moved });
        // What was written to it and is not the project's yet goes on to its new name.
        if state.staged.get(&number).is_some_and(|staged| staged.dirty) && !state.open(number) {
            state.new.insert(number, Instant::now());
        }
        drop(state);
        self.wake_now();
        Ok(())
    }

    /// A file with content here, moved onto a file that is there: that one takes the content - a save of it - and
    /// this one goes, removed from the project where the project has it.
    fn saved_over(
        &self,
        (number, project, from): (u64, bool, &str),
        (there, old, to): (u64, &fs::Node, &str),
        said: Option<u64>,
    ) -> io::Result<()> {
        let moved = {
            let mut state = self.lock();
            state.new.remove(&number);
            state.touched.remove(&number);
            state.refused.remove(&number);
            state.staged.remove(&number)
        };
        let Some(moved) = moved else { return Ok(()) };
        // What comes to be the project's is in the log - begun there, where it was kept in memory.
        if !reading(&moved.content).is_logged() {
            let begun = self.record(&sfs::Op::Began { path: to.to_string(), emptied: true })?;
            writing(&moved.content).held_in_log(&self.0.log, Some(begun))?;
        }
        let mut state = self.lock();
        state.staged.insert(there, Staged { content: moved.content, dirty: true, asked: None });
        for open in state.handles.values_mut().filter(|open| **open == number) {
            *open = there;
        }
        let now = Instant::now();
        if old.flags & NEW != 0 && !state.promised(there) {
            // One written there that the project is yet to have: its burst goes on.
            self.touch(&mut state, there);
        } else {
            state.new.insert(there, now);
            self.due(&state, now);
        }
        if project {
            state.removed.insert(number, Some(now));
            if let Some(said) = said {
                state.removals.insert(number, said);
            }
            drop(state);
            return self.flag(number, GONE, 0);
        }
        drop(state);
        let mut txn = self.0.space.write()?;
        fs::remove(&mut txn, SERVED, number, SERVER)?;
        txn.commit()?;
        // The kernel keeps the renamed file's number for its new name.
        self.0.served.rename(from, to);
        Ok(())
    }

    /// A file renamed to a name that is never the project's - a backup, most likely: what it holds is kept in
    /// memory under that name. One the project has is hidden where it was, its removal waiting a while on the
    /// file's return - a file made there meanwhile saves it.
    fn put_aside(
        &self,
        (number, node, project, from): (u64, &fs::Node, bool, &str),
        onto: Option<&(u64, fs::Node)>,
        to: &str,
    ) -> io::Result<()> {
        if onto.is_some_and(|(_, old)| old.folder) {
            return Err(io::Error::from(io::ErrorKind::IsADirectory));
        }
        if !project {
            if let Some((there, _)) = onto {
                self.vacate(*there, to)?;
            }
            let mut txn = self.0.space.write()?;
            let above = there(&txn, parent(to)).filter(|(_, node)| node.folder).ok_or_else(not_found)?.0;
            fs::rename(&mut txn, SERVED, number, above, name(to), SERVER)?;
            let moved = fs::node(&txn, SERVED, number)?.ok_or_else(not_found)?;
            fs::set(&mut txn, SERVED, number, &fs::Node { flags: moved.flags | JUNK, ..moved }, SERVER)?;
            txn.commit()?;
            self.0.served.rename(from, to);
            let mut state = self.lock();
            state.new.remove(&number);
            state.touched.remove(&number);
            let content = state.staged.get(&number).map(|staged| staged.content.clone());
            drop(state);
            // Nobody's to give: out of the log.
            return content.map_or(Ok(()), |content| writing(&content).held_in_log(&self.0.log, None));
        }
        // Taken up from the log, the backup is not made again: it was nobody's to keep.
        if !self.replaying() {
            self.stage(number, node, from, false)?;
        }
        let copy = {
            let mut state = self.lock();
            state.new.remove(&number);
            state.touched.remove(&number);
            state.staged.remove(&number)
        };
        if let Some(copy) = copy {
            let aside = match onto {
                Some((there, _)) => *there,
                None => self.make(to, false, JUNK)?,
            };
            let kept = writing(&copy.content).held_in_log(&self.0.log, None);
            self.lock().staged.insert(aside, Staged { content: copy.content, dirty: false, asked: None });
            kept?;
        }
        self.0.served.rename(from, to);
        let mut state = self.lock();
        let due = Instant::now() + BACKUP;
        state.removed.insert(number, Some(due));
        self.due(&state, due);
        drop(state);
        self.flag(number, GONE, 0)
    }

    /// A file only here, under a name it may come to be the project's by: in the log from here on - begun there,
    /// where it was kept in memory - and its burst goes on from where it was.
    fn named_at_last(&self, number: u64, to: &str) -> io::Result<()> {
        let content = self.lock().staged.get(&number).map(|staged| staged.content.clone());
        let Some(content) = content else { return Ok(()) };
        if is_junk(to) {
            return Ok(());
        }
        if !reading(&content).is_logged() {
            let begun = self.record(&sfs::Op::Began { path: to.to_string(), emptied: true })?;
            writing(&content).held_in_log(&self.0.log, Some(begun))?;
        }
        let mut state = self.lock();
        if let Some(staged) = state.staged.get_mut(&number) {
            staged.dirty = true;
        }
        self.touch(&mut state, number);
        Ok(())
    }

    /// Whether a new file waits for the `node.toml` beside it to make its folder a node.
    fn waits_for_its_node(&self, pages: &impl Pages, path: &str) -> bool {
        let Some((folder, name)) = path.rsplit_once('/') else { return false };
        if name == project::index::NODE_FILE {
            return false;
        }
        let declared = found(pages, &joined(folder, project::index::NODE_FILE));
        let declared = declared.is_some_and(|(number, node)| node.flags & NEW != 0 && self.lock().staged.contains_key(&number));
        declared && found(pages, folder).is_none_or(|(_, node)| node.flags & NODE == 0)
    }

    /// Say due what is: the files written whose programs are done with them - a `node.toml` before what is
    /// written beside it - and the removals whose moment has passed, in one commit, after all that is due.
    fn say_due(&self, now: Instant) {
        let _ordered = self.ordered();
        let Ok(pages) = self.pages() else { return };
        let (files, removals): (Vec<u64>, Vec<u64>) = {
            let state = self.lock();
            let unasked = |number: &u64| state.staged.get(number).is_none_or(|staged| staged.asked.is_none());
            let files = state.new.iter().filter(|(number, due)| **due <= now && !state.open(**number) && unasked(number));
            let removals = state.removed.iter().filter(|(_, due)| due.is_some_and(|due| due <= now));
            (files.map(|(number, _)| *number).collect(), removals.map(|(number, _)| *number).collect())
        };
        if files.is_empty() && removals.is_empty() {
            return;
        }
        // Each file with its path, as it is now: declarations first - what is written beside one waits for its
        // folder to be a node.
        let mut writes: Vec<(String, u64, Arc<RwLock<sfs::Content>>)> = Vec::new();
        for number in files {
            let at = fs::node(&pages, SERVED, number).ok().flatten().zip(fs::path(&pages, SERVED, number).ok().flatten());
            let mut state = self.lock();
            let staged = state.staged.get(&number).filter(|staged| staged.dirty && reading(&staged.content).is_logged());
            let (Some((node, path)), Some(staged)) = (at, staged) else {
                state.new.remove(&number);
                state.touched.remove(&number);
                continue;
            };
            if node.flags & (GONE | JUNK) != 0 {
                state.new.remove(&number);
                continue;
            }
            let content = staged.content.clone();
            drop(state);
            if self.waits_for_its_node(&pages, &path) {
                self.lock().new.insert(number, now + RECHECK);
                continue;
            }
            writes.push((path, number, content));
        }
        writes.sort_by_key(|(path, ..)| (!path.ends_with(&format!("/{}", project::index::NODE_FILE)), path.clone()));
        // Each said by where it is; one that cannot be is not said, and stays as it is.
        let said = writes.into_iter().filter_map(|(path, number, content)| {
            let (from, given) = self.given(&content).ok()?;
            Some((Op::Write { path, from }, number, content, given))
        });
        let said: Vec<_> = said.collect();
        let (removed, gone) = self.removals_due(&pages, removals);
        drop(pages);
        if said.is_empty() && removed.is_empty() && gone.is_empty() {
            return;
        }
        let Ok(mut txn) = self.0.space.write() else { return };
        let mut put = |op: &Op| crate::due::put_in(&mut txn, op, None);
        let writes: Vec<Option<u64>> = said.iter().map(|(op, ..)| put(op).ok()).collect();
        let removals_due: Vec<Option<u64>> = removed.iter().map(|(op, _)| put(op).ok()).collect();
        for number in gone {
            let _ = fs::remove(&mut txn, SERVED, number, SERVER);
        }
        if txn.commit().is_err() {
            return;
        }
        let mut state = self.lock();
        for ((_, number, content, given), due) in said.into_iter().zip(writes) {
            let Some(due) = due else { continue };
            state.new.remove(&number);
            state.touched.remove(&number);
            if let Some(staged) = state.staged.get_mut(&number).filter(|staged| Arc::ptr_eq(&staged.content, &content)) {
                // What is written to it from here on is not what was said.
                staged.dirty = reading(&content).changes() != given.changes;
                staged.asked = Some(due);
            }
            state.ask(due, Asked::Write { number, content, given });
        }
        for ((_, number), due) in removed.into_iter().zip(removals_due) {
            let Some(due) = due else { continue };
            state.removed.insert(number, None);
            let said = state.removals.remove(&number);
            state.ask(due, Asked::Remove { number, said });
        }
    }

    /// Of the removals whose moment has passed: those to say due, each with its path as it is now, and what goes
    /// here with nobody told - it was not the project's after all. What is in a folder being removed goes with
    /// that folder, and is said by it.
    fn removals_due(&self, pages: &impl Pages, removals: Vec<u64>) -> (Vec<(Op, u64)>, Vec<u64>) {
        let mut removed = Vec::new();
        let mut gone = Vec::new();
        for number in removals {
            let at = fs::node(pages, SERVED, number).ok().flatten().zip(fs::path(pages, SERVED, number).ok().flatten());
            let mut state = self.lock();
            let Some((node, path)) = at else {
                state.removed.remove(&number);
                state.removals.remove(&number);
                continue;
            };
            // What is in a folder being removed goes with it.
            let mut above = node.parent;
            let mut covered = false;
            while above != ROOT && !covered {
                let Some(folder) = fs::node(pages, SERVED, above).ok().flatten() else { break };
                covered = folder.flags & GONE != 0;
                above = folder.parent;
            }
            if covered {
                state.removed.insert(number, None);
                if let Some(said) = state.removals.remove(&number) {
                    let _ = self.record(&sfs::Op::Done { of: said });
                }
            } else if node.flags & NEW != 0 && !state.promised(number) {
                // Not the project's after all: it goes here, and nobody is told.
                state.removed.remove(&number);
                state.removals.remove(&number);
                gone.push(number);
            } else {
                removed.push((Op::Remove { path }, number));
            }
        }
        (removed, gone)
    }

    /// Read what whoever holds the project said became of what was due, and go on from it: how many things it said
    /// it of.
    fn answered(&self) -> usize {
        let numbers: Vec<u64> = self.lock().asked.keys().copied().collect();
        if numbers.is_empty() {
            return 0;
        }
        let Ok(became) = crate::due::settle(&self.0.space, &numbers, false) else { return 0 };
        let mut answered = 0;
        for (due, answer) in numbers.into_iter().zip(became) {
            let Some(answer) = answer else { continue };
            let Some(asked) = self.lock().answered(due) else { continue };
            answered += 1;
            self.became_of(due, asked, answer);
        }
        answered
    }

    /// What became of one thing that was due: taken, it is said so in the log, and what is kept here of it is let
    /// go of; refused, it is put back as the project has it - a folder made goes, a path moved is where it was, a
    /// thing removed is shown again - and a written file stays as written, its refusal kept for its program.
    fn became_of(&self, due: u64, asked: Asked, answer: crate::due::Answer) {
        let done = |said: Option<u64>, refused: bool| {
            if let Some(of) = said {
                let _ = self.record(&if refused { sfs::Op::Refused { of } } else { sfs::Op::Done { of } });
            }
        };
        match (asked, answer) {
            (Asked::Write { number, content, given }, Ok(label)) => {
                println!("{label}");
                if let Some((file, upto)) = given.taken {
                    let _ = self.record(&sfs::Op::Taken { file, upto });
                }
                let mut state = self.lock();
                state.refused.remove(&number);
                let open = state.open(number);
                if let Some(staged) = state.staged.get_mut(&number).filter(|staged| Arc::ptr_eq(&staged.content, &content)) {
                    if staged.asked == Some(due) {
                        staged.asked = None;
                    }
                    // The project has it as it was read: what was written to it meanwhile it has not.
                    staged.dirty |= reading(&content).changes() != given.changes;
                    if staged.dirty && !open && !state.new.contains_key(&number) {
                        state.new.insert(number, Instant::now());
                    }
                }
                drop(state);
                self.let_go(number);
            }
            (Asked::Write { number, content, .. }, Err(why)) => {
                // It stays as written, shown in the folder and not the project's: said here, and to the program at
                // its next sync or close of the file - its last one was answered before the project was asked.
                let path = self.path_of(number).unwrap_or_default();
                eprintln!("{path}: written, and not taken into the project: {why}");
                let mut state = self.lock();
                state.refused.insert(number, why);
                if let Some(staged) = state.staged.get_mut(&number).filter(|staged| Arc::ptr_eq(&staged.content, &content)) {
                    staged.asked = None;
                    staged.dirty = true;
                }
            }
            (Asked::Folder { said, .. } | Asked::Rename { said, .. }, Ok(label)) => {
                println!("{label}");
                done(said, false);
            }
            (Asked::Folder { number, said }, Err(why)) => {
                let path = self.path_of(number).unwrap_or_default();
                eprintln!("{path}: made, which the project did not take: {why}");
                done(said, true);
                let _ordered = self.ordered();
                if self.undo_folder(number).unwrap_or(false) {
                    self.0.served.told(Changes { gone: vec![(path, true)], ..Changes::default() });
                }
            }
            (Asked::Rename { number, from, to, said, moved }, Err(why)) => {
                eprintln!("{to}: moved there from {from}, which the project did not take: {why}");
                done(said, true);
                let _ordered = self.ordered();
                let _ = self.undo_rename(number, &from, moved);
            }
            (Asked::Remove { number, said }, Ok(label)) => {
                println!("{label}");
                done(said, false);
                self.lock().removed.remove(&number);
                // What whoever holds the project left of it - it was not the project's after all, or a folder
                // with what was removed in it - goes.
                let _ordered = self.ordered();
                if let Ok(mut txn) = self.0.space.write()
                    && fs::node(&txn, SERVED, number).ok().flatten().is_some_and(|node| node.flags & GONE != 0)
                    && fs::remove(&mut txn, SERVED, number, SERVER).is_ok()
                {
                    let _ = txn.commit();
                }
            }
            (Asked::Remove { number, said }, Err(why)) => {
                // Refused: it is shown again.
                let path = self.path_of(number).unwrap_or_default();
                eprintln!("{path}: removed, which the project did not take: {why}");
                done(said, true);
                self.lock().removed.remove(&number);
                let _ordered = self.ordered();
                let folder = self.pages().ok().and_then(|pages| fs::node(&pages, SERVED, number).ok().flatten()).map(|node| node.folder);
                if let (Ok(()), Some(folder)) = (self.flag(number, 0, GONE), folder) {
                    self.0.served.told(Changes { new: vec![(path, folder)], ..Changes::default() });
                }
            }
        }
    }

    /// A folder made here that the project did not take goes, where nothing was put in it. Whether it went.
    fn undo_folder(&self, number: u64) -> io::Result<bool> {
        let mut txn = self.0.space.write()?;
        let Some(node) = fs::node(&txn, SERVED, number)?.filter(|node| node.flags & NEW != 0) else { return Ok(false) };
        let mut empty = true;
        fs::list(&txn, SERVED, number, None, &mut |_, _| {
            empty = false;
            false
        })?;
        if !empty || !node.folder {
            return Ok(false);
        }
        fs::remove(&mut txn, SERVED, number, SERVER)?;
        txn.commit()?;
        Ok(true)
    }

    /// A path moved here that the project did not move is put back where it was, and the project's paths are
    /// read through the move no more.
    fn undo_rename(&self, number: u64, from: &str, moved: u64) -> io::Result<()> {
        let mut txn = self.0.space.write()?;
        project::shared::unmoved(&mut txn, moved)?;
        let now = fs::path(&txn, SERVED, number)?;
        let node = fs::node(&txn, SERVED, number)?;
        let above = there(&txn, parent(from)).filter(|(_, node)| node.folder).filter(|_| found(&txn, from).is_none());
        let back = match (&now, &node, above) {
            (Some(_), Some(_), Some((above, _))) => fs::rename(&mut txn, SERVED, number, above, name(from), SERVER).is_ok(),
            _ => false,
        };
        txn.commit()?;
        if let (true, Some(now), Some(node)) = (back, now, node) {
            self.0.served.moved_there(&now, from, node.folder, None);
        }
        Ok(())
    }

    /// Say due what is, have whoever holds the project look, and go on from what it says became of each thing.
    pub fn settle(&self) {
        // One at a time: what is due is said and asked for under it, by the thread that does so as it goes and by
        // what gives all of it as serving ends.
        let _settling = self.0.settling.lock().unwrap_or_else(PoisonError::into_inner);
        // Where the log ends before anything is looked at: what is staged from now on is past it.
        let written = self.0.log.header().tail();
        self.answered();
        self.say_due(Instant::now());
        self.ask();
        self.let_go_of_log(written);
    }

    /// Have whoever holds the project look at what is due, for as long as it takes something of it: what nobody
    /// takes now stays due, and is asked for again in a moment.
    fn ask(&self) {
        loop {
            {
                let mut state = self.lock();
                if state.asked.is_empty() {
                    (state.again, state.waiting) = (None, false);
                    return;
                }
                if state.again.is_some_and(|again| again > Instant::now()) {
                    return;
                }
            }
            match self.0.owner.look() {
                Ok(()) => {
                    let answered = self.answered();
                    // What its answers made due: a file written again while it was read.
                    self.say_due(Instant::now());
                    let mut state = self.lock();
                    if answered == 0 {
                        // It looked, and took nothing of ours: asked again in a moment.
                        state.again = Some(Instant::now() + AGAIN);
                        return;
                    }
                    (state.again, state.waiting) = (None, false);
                }
                Err(Refusal::Later(why) | Refusal::Never(why)) => {
                    let mut state = self.lock();
                    if !std::mem::replace(&mut state.waiting, true) {
                        eprintln!("what was done through the folder is kept until the project can be told: {why}");
                    }
                    state.again = Some(Instant::now() + AGAIN);
                    return;
                }
            }
        }
    }

    /// Say due all that waits, now, and have it taken: what serving does as it ends, so that what a program did a
    /// moment before is the project's too. What nobody took by then is its owner's to keep, and what was made of
    /// it in what is served goes.
    pub fn finish(&self) {
        let _settling = self.0.settling.lock().unwrap_or_else(PoisonError::into_inner);
        {
            let mut state = self.lock();
            let now = Instant::now();
            state.again = None;
            for due in state.new.values_mut() {
                *due = now;
            }
            for due in state.removed.values_mut().flatten() {
                *due = now;
            }
        }
        self.answered();
        self.say_due(Instant::now());
        self.ask();
        // What nobody took is due no more: taken back, but what was answered this moment.
        let numbers: Vec<u64> = self.lock().asked.keys().copied().collect();
        if let Ok(became) = crate::due::settle(&self.0.space, &numbers, true) {
            for (due, answer) in numbers.into_iter().zip(became) {
                // Taken back, or answered: either way it is asked no more.
                let asked = match answer {
                    Some(_) => self.lock().answered(due),
                    None => self.lock().asked_no_more(due),
                };
                if let (Some(answer), Some(asked)) = (answer, asked) {
                    self.became_of(due, asked, answer);
                }
            }
        }
        // What nobody took by now is its owner's to keep: the staging is emptied when the folder is next served.
        let left: Vec<(String, Arc<RwLock<sfs::Content>>)> = {
            let pages = self.pages().ok();
            let state = self.lock();
            let untaken = state.staged.iter().filter_map(|(number, staged)| {
                let pages = pages.as_ref()?;
                let node = fs::node(pages, SERVED, *number).ok().flatten()?;
                let mine = staged.dirty || staged.asked.is_some() || node.flags & NEW != 0;
                let kept = mine && node.flags & (JUNK | GONE) == 0 && reading(&staged.content).is_logged();
                kept.then(|| Some((fs::path(pages, SERVED, *number).ok().flatten()?, staged.content.clone()))).flatten()
            });
            untaken.collect()
        };
        for (path, content) in left {
            if let Ok(file) = self.written_out(&content) {
                self.0.owner.left(&path, &file);
                let _ = std::fs::remove_file(file);
            }
        }
        // Ended with nothing in transit that is not its owner's now: what the log holds is nobody's to take up,
        // and what is served is the project's alone.
        let _ordered = self.ordered();
        if let Some(said) = self.said_of_waiting() {
            println!("{said}");
        }
        {
            let mut state = self.lock();
            let next = state.next;
            *state = State { next, ..State::default() };
        }
        let _ = self.wipe();
        if self.record(&sfs::Op::Ended).is_ok() {
            self.0.log.header().set_transit(None);
            self.0.log.keep();
        }
    }

    /// How long what was said due waited for whoever holds the project to say what became of it, over this
    /// serving: none where nothing was answered.
    #[must_use]
    pub fn said_of_waiting(&self) -> Option<String> {
        let mut waited = self.lock().waited.clone();
        if waited.is_empty() {
            return None;
        }
        waited.sort_unstable();
        let millis = |micros: u32| f64::from(micros) / 1000.0;
        Some(format!(
            "what was due: {} answered, {:.1} ms in the middle, {:.1} ms for nine in ten, {:.1} ms at most, from said due to answered",
            waited.len(),
            millis(waited[waited.len() / 2]),
            millis(waited[waited.len() * 9 / 10]),
            millis(waited[waited.len() - 1])
        ))
    }

    /// Take up what the log holds from an earlier serving that ended with no word of it - its process gone, the
    /// power lost: each thing done is done again here in the order it was, with nothing appended for it, and what
    /// the project took by then is known as taken. All that is still to be said due is due at once.
    fn replay(&self) {
        use std::sync::atomic::Ordering::Relaxed;
        // Each file of the log: its number in what is served, and its content.
        let mut files: HashMap<u64, (u64, Arc<RwLock<sfs::Content>>)> = HashMap::new();
        // Where each file was last changed, and files begun again further on whose move may not be whole.
        let mut changed: HashMap<u64, u64> = HashMap::new();
        let mut moving: HashMap<u64, (u64, Staged)> = HashMap::new();
        let mut taken_up = 0usize;
        // Nothing in transit, nothing to take up - but said from here on, on disk, before anything is: a header a
        // loss of power leaves saying nothing would have what was synced since never taken up.
        let header = self.0.log.header();
        let Some(begins) = header.transit() else {
            header.set_transit(Some(header.tail()));
            let _ = header.flush();
            return;
        };
        let dirty = |writes: &Writes, files: &HashMap<u64, (u64, Arc<RwLock<sfs::Content>>)>, file: u64, dirty: bool| {
            let Some((number, content)) = files.get(&file) else { return };
            let mut state = writes.lock();
            if let Some(staged) = state.staged.get_mut(number).filter(|staged| Arc::ptr_eq(&staged.content, content)) {
                staged.dirty = dirty;
            }
        };
        for record in self.0.log.records(self.0.log.from(begins)) {
            let Ok(record) = record else { break };
            if let Some((file, offset, at, len, wrote)) = sfs::content::data(&record) {
                let Some((_, content)) = files.get(&file) else { continue };
                writing(content).attach(offset, len, at);
                if wrote {
                    changed.insert(file, record.position);
                    dirty(self, &files, file, true);
                }
                continue;
            }
            let Some(op) = sfs::Op::of(record.kind, &record.body) else { continue };
            taken_up += 1;
            self.0.replaying.store(record.position, Relaxed);
            match op {
                sfs::Op::Began { path, emptied } => {
                    let at = self.pages().ok().and_then(|pages| found(&pages, &path));
                    let number = match at {
                        Some((number, node)) => {
                            if node.flags & GONE != 0 {
                                let _ = self.unhide(number);
                            }
                            number
                        }
                        // Where its folder is no more - the project moved it, as the log goes on to say - the
                        // folder is here for it until then.
                        None => match self.make_above(&path).and_then(|()| self.make(&path, false, 0)) {
                            Ok(number) => number,
                            Err(_) => continue,
                        },
                    };
                    let Ok(content) = self.begin(&path, emptied) else { continue };
                    files.insert(record.position, (number, content.clone()));
                    if emptied {
                        changed.insert(record.position, record.position);
                    }
                    let before = self.lock().staged.insert(number, Staged { content, dirty: emptied, asked: None });
                    // Begun again as it is: the one before stands until this one is whole.
                    if let Some(before) = before.filter(|_| !emptied) {
                        moving.insert(record.position, (number, before));
                    }
                }
                sfs::Op::Truncated { file, len } => {
                    let Some((_, content)) = files.get(&file) else { continue };
                    writing(content).truncate(len);
                    // What ends a file begun again further on: it is whole, and as its changes said it was.
                    if let Some((_, before)) = moving.remove(&file) {
                        dirty(self, &files, file, before.dirty);
                        continue;
                    }
                    changed.insert(file, record.position);
                    dirty(self, &files, file, true);
                }
                sfs::Op::Removed { path } => drop(self.remove(&path)),
                sfs::Op::MadeFolder { path } => drop(self.make_folder(&path)),
                sfs::Op::Renamed { from, to } => drop(self.rename(&from, &to)),
                sfs::Op::Taken { file, upto } => {
                    if changed.get(&file).is_none_or(|at| *at < upto) {
                        dirty(self, &files, file, false);
                    }
                }
                sfs::Op::Done { of } => self.became(of, false),
                sfs::Op::Refused { of } => self.became(of, true),
                sfs::Op::Ended => {
                    self.forget();
                    files.clear();
                    changed.clear();
                    moving.clear();
                    taken_up = 0;
                }
            }
        }
        self.0.replaying.store(NOT, Relaxed);
        self.taken_up(moving, taken_up);
    }

    /// All that is in transit is nobody's to take up: nothing is kept of it, nothing of it is due, and what is
    /// served is the project's alone.
    fn forget(&self) {
        let numbers: Vec<u64> = {
            let mut state = self.lock();
            let numbers = state.asked.keys().copied().collect();
            let next = state.next;
            *state = State { next, ..State::default() };
            numbers
        };
        let _ = crate::due::settle(&self.0.space, &numbers, true);
        let _ = self.wipe();
    }

    /// What the log says became of something the project was to be told - a folder, a rename, a removal - when
    /// the log is taken up: taken, or refused and put back as it was. It is due no more.
    fn became(&self, of: u64, refused: bool) {
        let mut state = self.lock();
        let due = state.asked.iter().find(|(_, asked)| asked.said() == Some(of)).map(|(due, _)| *due);
        if let Some(asked) = due.and_then(|due| state.asked_no_more(due)) {
            drop(state);
            let _ = crate::due::settle(&self.0.space, &due.into_iter().collect::<Vec<_>>(), true);
            match (asked, refused) {
                // The project has it, whatever it has said of what it serves by now.
                (Asked::Folder { number, .. }, false) => drop(self.flag(number, 0, NEW | WAITS)),
                (Asked::Folder { number, .. }, true) => drop(self.undo_folder(number)),
                (Asked::Rename { moved, .. }, false) => {
                    if let Ok(mut txn) = self.0.space.write()
                        && project::shared::unmoved(&mut txn, moved).is_ok()
                    {
                        let _ = txn.commit();
                    }
                }
                (Asked::Rename { number, from, moved, .. }, true) => drop(self.undo_rename(number, &from, moved)),
                (Asked::Remove { number, .. }, refused) => self.removed_as(number, refused),
                (Asked::Write { .. }, _) => {}
            }
            return;
        }
        // A removal not said due yet.
        let number = state.removals.iter().find(|(_, at)| **at == of).map(|(number, _)| *number);
        if let Some(number) = number {
            state.removals.remove(&number);
            drop(state);
            self.removed_as(number, refused);
        }
    }

    /// A removal is as the log says it became: the project's, and the thing is gone; refused, and it is shown
    /// again.
    fn removed_as(&self, number: u64, refused: bool) {
        self.lock().removed.remove(&number);
        if refused {
            let _ = self.flag(number, 0, GONE);
        } else if let Ok(mut txn) = self.0.space.write()
            && fs::remove(&mut txn, SERVED, number, SERVER).is_ok()
        {
            let _ = txn.commit();
        }
    }

    /// The log taken up: what was cut short stands as it was before, and all that is still to be said due is due.
    fn taken_up(&self, moving: HashMap<u64, (u64, Staged)>, taken_up: usize) {
        let clean: Vec<u64> = {
            let mut state = self.lock();
            // A file begun again whose move was cut short: the one before it stands.
            for (file, (number, before)) in moving {
                if state.staged.get(&number).is_some_and(|staged| reading(&staged.content).id() == file) {
                    state.staged.insert(number, before);
                }
            }
            // All that is still to be said due is due: nothing of it is waited for any longer.
            let now = Instant::now();
            state.touched.clear();
            state.again = None;
            let dirty = |staged: &Staged| staged.dirty && reading(&staged.content).is_logged();
            state.new = state.staged.iter().filter(|(_, staged)| dirty(staged)).map(|(number, _)| (*number, now)).collect();
            for due in state.removed.values_mut().flatten() {
                *due = now;
            }
            let waits = state.new.len() + state.asked.len() + state.removed.len();
            if waits > 0 {
                eprintln!("{waits} changes an earlier serving left in transit are taken up ({taken_up} things it did)");
            }
            state.staged.iter().filter(|(_, staged)| !staged.dirty).map(|(number, _)| *number).collect()
        };
        // What the project took as it is, is the project's to show.
        for number in clean {
            self.let_go(number);
        }
    }

    /// The tree served.
    #[must_use]
    pub fn served(&self) -> &Served {
        &self.0.served
    }
}

#[cfg(test)]
mod tests {
    use super::Writes;
    use crate::Served;
    use crate::write::{Change, Owner, Refusal};
    use project::server::{Delta, Listing, Shown as Listed, Source};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc::{Receiver, Sender, channel};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    /// How the stand-in for whoever holds a project takes what it is given.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Takes {
        All,
        /// Nobody takes anything now: a tool has the project.
        Nothing,
        /// Nothing is taken, ever.
        Never,
        /// What is asked in an order - folders, renames, removals - and nothing written yet.
        Ordered,
    }

    /// A project of files, held as whoever holds one does: what it takes of what programs did it makes its own,
    /// and says in a shared space of the test's own - which the tree served is a view of - before it answers.
    struct Holder {
        space: Arc<sfs::Space>,
        served: Served,
        /// Where its files' content is.
        files: PathBuf,
        listing: Mutex<Listing>,
        takes: Mutex<Takes>,
        /// Where it says that it starts to take a written file, and waits to be told to go on.
        gate: Mutex<Option<(Sender<()>, Receiver<()>)>>,
        /// What it took, as it was given together; and what was left with it as serving ended.
        batches: Mutex<Vec<Vec<String>>>,
        left: Mutex<Vec<String>>,
        made: AtomicU64,
    }

    fn above(listing: &mut Listing, path: &str) {
        for (at, _) in path.match_indices('/') {
            listing.entry(path[..at].to_string()).or_insert(Listed::Folder);
        }
    }

    impl Holder {
        /// A project of these files, each with its text, over the folder `dir` a test keeps its writes in.
        fn new(dir: &Path, files: &[(&str, &str)], takes: Takes) -> Arc<Holder> {
            let at = PathBuf::from(format!("{}-files", dir.display()));
            let _ = std::fs::remove_dir_all(&at);
            std::fs::create_dir_all(&at).unwrap();
            let space = Arc::new(sfs::Space::private().unwrap());
            let holder = Holder {
                served: Served::shared(space.clone(), None),
                space,
                files: at,
                listing: Mutex::default(),
                takes: Mutex::new(takes),
                gate: Mutex::default(),
                batches: Mutex::default(),
                left: Mutex::default(),
                made: AtomicU64::new(0),
            };
            {
                let mut listing = holder.listing.lock().unwrap();
                for (path, text) in files {
                    above(&mut listing, path);
                    listing.insert((*path).to_string(), holder.file(text.as_bytes()));
                }
                project::shared::say_whole(&holder.space, &listing).unwrap();
            }
            Arc::new(holder)
        }

        /// As `new`, taking a written file only once told to go on: it says when it starts to.
        fn gated(dir: &Path, files: &[(&str, &str)]) -> (Arc<Holder>, Receiver<()>, Sender<()>) {
            let holder = Holder::new(dir, files, Takes::All);
            let (started, starts) = channel();
            let (goes_on, go_on) = channel();
            *holder.gate.lock().unwrap() = Some((started, go_on));
            (holder, starts, goes_on)
        }

        fn file(&self, bytes: &[u8]) -> Listed {
            let disk = self.files.join(self.made.fetch_add(1, Ordering::Relaxed).to_string());
            std::fs::write(&disk, bytes).unwrap();
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
            let modified = (now.as_secs(), now.subsec_nanos());
            Listed::File { disk, size: bytes.len() as u64, modified, of_files: false, source: Source::Disk }
        }

        fn take(&self, takes: Takes) {
            *self.takes.lock().unwrap() = takes;
        }

        /// What it took so far, in the order it did.
        fn given(&self) -> Vec<String> {
            self.batches.lock().unwrap().iter().flatten().cloned().collect()
        }

        fn has(&self, path: &str) -> bool {
            self.listing.lock().unwrap().contains_key(path)
        }

        fn made_of(&self, listing: &mut Listing, change: &Change) -> Result<(), Refusal> {
            let under = |listing: &Listing, path: &str| -> Vec<String> {
                let inside = format!("{path}/");
                listing.keys().filter(|at| *at == path || at.starts_with(&inside)).cloned().collect()
            };
            let none = |path: &str| Refusal::Never(format!("{path}: nothing is there"));
            match change {
                Change::Write { path, staged } => {
                    let bytes = staged.read().map_err(|e| Refusal::Never(e.to_string()))?;
                    above(listing, path);
                    listing.insert(path.clone(), self.file(&bytes));
                }
                // What is so already is as asked, as with whoever holds a project.
                Change::Folder { path } => {
                    if matches!(listing.get(path), Some(Listed::File { .. })) {
                        return Err(Refusal::Never(format!("{path} is a file")));
                    }
                    above(listing, path);
                    listing.insert(path.clone(), Listed::Folder);
                }
                Change::Remove { path } => {
                    for path in under(listing, path) {
                        listing.remove(&path);
                    }
                }
                Change::Rename { from, to } => {
                    let moved = under(listing, from);
                    if moved.is_empty() && listing.contains_key(to) {
                        return Ok(());
                    }
                    if moved.is_empty() {
                        return Err(none(from));
                    }
                    for path in under(listing, to) {
                        listing.remove(&path);
                    }
                    above(listing, to);
                    for path in moved {
                        if let Some(it) = listing.remove(&path) {
                            listing.insert(format!("{to}{}", &path[from.len()..]), it);
                        }
                    }
                }
            }
            Ok(())
        }

        /// Take what is due, as it takes things now: each made the project's, what became of it said in the
        /// commit that says what is served after it - where the tree served reads it - and those that tell of
        /// the tree told.
        fn taken(&self) -> Result<(), Refusal> {
            let takes = *self.takes.lock().unwrap();
            if takes == Takes::Nothing {
                return Err(Refusal::Later("a tool has it".into()));
            }
            let due = crate::due::due(&self.space.read().unwrap()).unwrap();
            let mut listing = self.listing.lock().unwrap();
            let before = listing.clone();
            let mut took = Vec::new();
            let mut answers: Vec<(u64, crate::due::Answer)> = Vec::new();
            for (number, op) in due {
                let Some(change) = op.as_change() else { continue };
                if takes == Takes::Never {
                    answers.push((number, Err("not here".into())));
                    continue;
                }
                if matches!(change, Change::Write { .. }) {
                    // Not yet: it stays due.
                    if takes == Takes::Ordered {
                        continue;
                    }
                    if let Some((started, go_on)) = &*self.gate.lock().unwrap() {
                        let _ = started.send(());
                        let _ = go_on.recv();
                    }
                }
                let made = self.made_of(&mut listing, &change);
                if made.is_ok() {
                    took.push(change.label());
                }
                let (Refusal::Never(why) | Refusal::Later(why)) = match made {
                    Ok(()) => {
                        answers.push((number, Ok(change.label())));
                        continue;
                    }
                    Err(refusal) => refusal,
                };
                answers.push((number, Err(why)));
            }
            let delta = Delta::between(&before, &listing);
            project::shared::say_with(&self.space, &delta, &mut |txn| Ok(crate::due::answer(txn, &answers)?)).unwrap();
            self.served.told(crate::Changes::of(&before, &delta, &listing));
            self.batches.lock().unwrap().push(took);
            Ok(())
        }
    }

    impl Owner for Holder {
        fn change(&self, _change: Change) -> Result<(), Refusal> {
            Err(Refusal::Never("what programs did is taken from where it is said due".into()))
        }

        fn look(&self) -> Result<(), Refusal> {
            self.taken()
        }

        fn left(&self, path: &str, _staged: &Path) {
            self.left.lock().unwrap().push(path.to_string());
        }
    }

    /// Done with a test's folders: its writes' and its project's files'.
    fn done(dir: &Path) {
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(format!("{}-files", dir.display()));
    }

    #[test]
    fn what_nobody_takes_yet_is_kept_and_given_once_someone_does() {
        let dir = std::env::temp_dir().join(format!("pe2-writes-later-{}", std::process::id()));
        let owner = Holder::new(&dir, &[], Takes::Nothing);
        let served = owner.served.clone();
        let writes = Writes::new(&served, owner.clone(), &dir).unwrap();
        let handle = writes.create("kept.txt").unwrap();
        writes.write(handle, 0, b"x").unwrap();
        writes.close(handle, true).unwrap();
        // Tried, and not taken: it is shown as written, and nothing was given.
        std::thread::sleep(Duration::from_millis(2500));
        assert!(writes.shown("kept.txt").is_some(), "shown as written while it waits");
        assert_eq!(owner.given(), [] as [String; 0], "nothing was given");
        // Someone takes changes again: it is given without being written again.
        owner.take(Takes::All);
        let given = || !owner.given().is_empty();
        let asked = Instant::now();
        while !given() && asked.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(given(), "given once someone takes it");
        assert!(owner.left.lock().unwrap().is_empty());
        drop(writes);
        done(&dir);
    }

    #[test]
    fn the_log_is_let_go_of_behind_what_is_written_whatever_waits() {
        let dir = std::env::temp_dir().join(format!("pe2-writes-log-{}", std::process::id()));
        // Nobody takes anything: what is written waits, for as long as this runs.
        let owner = Holder::new(&dir, &[], Takes::Nothing);
        let served = owner.served.clone();
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
        done(&dir);
    }

    /// How long the library itself takes for what programs do, with no backend before it.
    #[test]
    #[ignore = "a measurement"]
    fn bench_writes() {
        let dir = std::env::temp_dir().join(format!("pe2-writes-bench-{}", std::process::id()));
        let owner = Holder::new(&dir, &[], Takes::All);
        let served = owner.served.clone();
        let writes = Writes::new(&served, owner, &dir).unwrap();
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
        done(&dir);
    }

    /// New files given only long after they were last changed: for what is to happen before they are.
    const UNHURRIED: super::Pace = super::Pace { least: Duration::from_secs(20), most: Duration::from_secs(20) };

    #[test]
    fn a_save_returns_at_once_and_is_the_projects_a_moment_later() {
        let dir = std::env::temp_dir().join(format!("pe2-writes-save-{}", std::process::id()));
        let (owner, starts, goes_on) = Holder::gated(&dir, &[("saved.txt", "before")]);
        let served = owner.served.clone();
        let writes = Writes::new(&served, owner.clone(), &dir).unwrap();
        let handle = writes.open("saved.txt", true).unwrap();
        writes.write(handle, 0, b"after").unwrap();
        // Let go of: it returns while the project has not taken it - which here waits to be told to go on.
        writes.close(handle, true).unwrap();
        starts.recv_timeout(Duration::from_secs(5)).expect("the project is being given the file, off this thread");
        assert!(owner.given().is_empty(), "not the project's yet");
        assert_eq!(writes.read("saved.txt", 0, 16).unwrap(), b"after", "read as written meanwhile");
        goes_on.send(()).unwrap();
        let asked = Instant::now();
        let saved = || owner.given().iter().any(|c| c == "Write saved.txt from outside");
        while !saved() && asked.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(saved(), "the project's a moment later");
        drop(writes);
        done(&dir);
    }

    #[test]
    fn a_new_file_is_the_projects_once_its_burst_of_changes_has_ended() {
        let dir = std::env::temp_dir().join(format!("pe2-writes-burst-{}", std::process::id()));
        let owner = Holder::new(&dir, &[], Takes::All);
        let served = owner.served.clone();
        let pace = super::Pace { least: Duration::from_millis(400), most: Duration::from_secs(2) };
        let writes = Writes::with(&served, owner.clone(), &dir, sfs::log::Config::default(), pace).unwrap();
        let given = || owner.given().len();
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
        done(&dir);
    }

    #[test]
    fn a_folder_is_made_at_once_whoever_takes_it_when() {
        let dir = std::env::temp_dir().join(format!("pe2-writes-mkdir-{}", std::process::id()));
        // Nobody takes anything yet.
        let owner = Holder::new(&dir, &[], Takes::Nothing);
        let served = owner.served.clone();
        let writes = Writes::new(&served, owner.clone(), &dir).unwrap();
        let asked = Instant::now();
        writes.make_folder("made").expect("made, whatever the project is doing");
        writes.make_folder("made/inside").expect("and one in it");
        assert!(asked.elapsed() < Duration::from_secs(1), "without waiting for the project");
        assert!(writes.shown("made/inside").is_some_and(|shown| shown.folder), "shown at once");
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(owner.given(), [] as [String; 0], "nothing was given");
        // Someone takes changes again: given in the order they were made.
        owner.take(Takes::All);
        let given = || owner.given();
        let waited = Instant::now();
        while given().len() < 2 && waited.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(given(), ["New made from outside", "New inside from outside"]);
        drop(writes);
        done(&dir);
    }

    #[test]
    fn what_is_written_in_a_folder_goes_with_it_when_it_is_renamed() {
        let dir = std::env::temp_dir().join(format!("pe2-writes-moved-{}", std::process::id()));
        let owner = Holder::new(&dir, &[], Takes::All);
        let served = owner.served.clone();
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
        let given = owner.given();
        assert_eq!(given, ["New a from outside", "Rename a to b from outside", "Write x.txt from outside"]);
        drop(writes);
        done(&dir);
    }

    #[test]
    fn what_the_project_refuses_late_is_said_at_the_next_sync_or_close() {
        let dir = std::env::temp_dir().join(format!("pe2-writes-late-{}", std::process::id()));
        let owner = Holder::new(&dir, &[], Takes::Never);
        let served = owner.served.clone();
        let writes = Writes::new(&served, owner, &dir).unwrap();
        // Written and closed: answered at once, before the project is asked.
        let handle = writes.create("a.txt").unwrap();
        writes.write(handle, 0, b"one").unwrap();
        writes.flush(handle).expect("nothing was refused yet");
        writes.close(handle, true).expect("closed");
        // The project refuses it a moment later - once nothing has it open. The file stays as written; whoever
        // opens it next is told as it closes or syncs.
        let told_at = |sync: bool| {
            let waited = Instant::now();
            loop {
                let handle = writes.open("a.txt", false).unwrap();
                let told = if sync { writes.close(handle, false) } else { writes.flush(handle) };
                if let Err(told) = told {
                    break (handle, told);
                }
                writes.close(handle, true).unwrap();
                assert!(waited.elapsed() < Duration::from_secs(10), "never told");
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        let (again, told) = told_at(false);
        assert!(told.to_string().contains("not here"), "{told}");
        // Once: the next close of the same handle says nothing of it - and a sync says what is refused after it.
        writes.flush(again).expect("said once");
        writes.write(again, 3, b"!").unwrap();
        writes.close(again, true).unwrap();
        let (third, told) = told_at(true);
        assert!(told.to_string().contains("not here"), "{told}");
        writes.close(third, true).unwrap();
        assert_eq!(writes.read("a.txt", 0, 16).unwrap(), b"one!", "as written, whatever the project took");
        drop(writes);
        done(&dir);
    }

    #[test]
    fn a_rename_is_shown_at_once_and_the_project_told_in_its_turn() {
        let dir = std::env::temp_dir().join(format!("pe2-writes-rename-{}", std::process::id()));
        // Nobody takes anything yet.
        let owner = Holder::new(&dir, &[("old/in/a.txt", "a")], Takes::Nothing);
        let served = owner.served.clone();
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
        assert_eq!(owner.given(), [] as [String; 0], "nothing was given");
        // Someone takes changes again: the rename first, then what was written where it led.
        owner.take(Takes::All);
        let given = || owner.given();
        let waited = Instant::now();
        while given().len() < 2 && waited.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(given(), ["Rename old to new from outside", "Write a.txt from outside"]);
        drop(writes);
        done(&dir);
    }

    #[test]
    fn a_file_moved_onto_one_that_is_there_gives_it_its_content_and_goes() {
        let dir = std::env::temp_dir().join(format!("pe2-writes-onto-{}", std::process::id()));
        let owner = Holder::new(&dir, &[("a.txt", "one"), ("b.txt", "two"), ("d.txt", "four")], Takes::All);
        let served = owner.served.clone();
        let writes = Writes::new(&served, owner.clone(), &dir).unwrap();
        let given = |many: usize| {
            let waited = Instant::now();
            while owner.given().len() < many && waited.elapsed() < Duration::from_secs(10) {
                std::thread::sleep(Duration::from_millis(20));
            }
            let mut given: Vec<String> = owner.batches.lock().unwrap().drain(..).flatten().collect();
            given.sort();
            given
        };
        // Both the project's, nothing of either staged: no rename the project would refuse - the one there takes
        // the content, the one moved is removed.
        writes.rename("a.txt", "b.txt").expect("moved onto it");
        assert_eq!(writes.read("b.txt", 0, 8).unwrap(), b"one");
        assert!(writes.shown("a.txt").is_none(), "no longer where it was");
        assert_eq!(given(2), ["Remove a.txt from outside", "Write b.txt from outside"]);
        // Onto one written there that the project is yet to have: the same, and what was written there is not given.
        let handle = writes.open("c.txt", true).unwrap();
        writes.write(handle, 0, b"written, never the project's").unwrap();
        writes.rename("d.txt", "c.txt").expect("moved onto it");
        assert_eq!(writes.read("c.txt", 0, 8).unwrap(), b"four");
        assert!(writes.shown("d.txt").is_none());
        writes.close(handle, true).unwrap();
        let given = given(2);
        assert_eq!(given, ["Remove d.txt from outside", "Write c.txt from outside"]);
        drop(writes);
        done(&dir);
    }

    #[test]
    fn a_rename_the_project_refuses_is_put_back() {
        let dir = std::env::temp_dir().join(format!("pe2-writes-back-{}", std::process::id()));
        let owner = Holder::new(&dir, &[("old/a.txt", "a")], Takes::Never);
        let served = owner.served.clone();
        let writes = Writes::new(&served, owner, &dir).unwrap();
        writes.rename("old", "new").expect("shown renamed, until the project says otherwise");
        let back = || writes.shown("old/a.txt").is_some() && writes.shown("new").is_none();
        let waited = Instant::now();
        while !back() && waited.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(back(), "where it was, as the project has it");
        drop(writes);
        done(&dir);
    }

    #[test]
    fn what_was_in_transit_is_taken_up_by_a_serving_that_starts_again() {
        let dir = std::env::temp_dir().join(format!("pe2-writes-again-{}", std::process::id()));
        done(&dir);
        let project = [("kept.txt", "before"), ("gone.txt", "g"), ("old/x.txt", "x"), ("part.txt", "0123456789")];
        {
            // Nobody takes anything, for as long as this serving lasts.
            let owner = Holder::new(&dir, &project, Takes::Nothing);
            let served = owner.served.clone();
            let writes = Writes::new(&served, owner, &dir).unwrap();
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
        let owner = Holder::new(&dir, &project, Takes::Nothing);
        let served = owner.served.clone();
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
        owner.take(Takes::All);
        let given = || owner.given();
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
        done(&dir);
    }

    #[test]
    fn what_was_written_in_a_folder_the_project_has_renamed_is_taken_up_where_it_is() {
        let dir = std::env::temp_dir().join(format!("pe2-writes-where-{}", std::process::id()));
        done(&dir);
        {
            let owner = Holder::new(&dir, &[("old/x.txt", "x")], Takes::Ordered);
            let served = owner.served.clone();
            let writes = Writes::new(&served, owner.clone(), &dir).unwrap();
            let handle = writes.create("old/written.txt").unwrap();
            writes.write(handle, 0, b"w").unwrap();
            writes.close(handle, true).unwrap();
            writes.rename("old", "new").unwrap();
            // The project has the rename, and not what was written: there it ends.
            let waited = Instant::now();
            while owner.given().is_empty() && waited.elapsed() < Duration::from_secs(10) {
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(owner.given(), ["Rename old to new from outside"]);
        }
        // The tree as the project has it: renamed.
        let owner = Holder::new(&dir, &[("new/x.txt", "x")], Takes::All);
        let served = owner.served.clone();
        let writes = Writes::new(&served, owner.clone(), &dir).unwrap();
        assert_eq!(writes.read("new/written.txt", 0, 4).unwrap(), b"w", "where its folder is now");
        assert!(writes.shown("old").is_none() && writes.shown("old/written.txt").is_none());
        let given = || owner.given();
        let waited = Instant::now();
        while given().is_empty() && waited.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(given(), ["Write written.txt from outside"], "the rename is not asked again");
        drop(writes);
        done(&dir);
    }

    #[test]
    fn what_the_project_took_is_not_given_again() {
        let dir = std::env::temp_dir().join(format!("pe2-writes-took-{}", std::process::id()));
        done(&dir);
        {
            let owner = Holder::new(&dir, &[], Takes::All);
            let served = owner.served.clone();
            let writes = Writes::new(&served, owner.clone(), &dir).unwrap();
            writes.make_folder("made").unwrap();
            let handle = writes.create("made/a.txt").unwrap();
            writes.write(handle, 0, b"one").unwrap();
            writes.close(handle, true).unwrap();
            let handle = writes.create("late.txt").unwrap();
            writes.write(handle, 0, b"first").unwrap();
            writes.close(handle, true).unwrap();
            let given = || owner.given().len();
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
        let owner = Holder::new(&dir, &[("made/a.txt", "one"), ("late.txt", "first")], Takes::All);
        let served = owner.served.clone();
        let writes = Writes::new(&served, owner.clone(), &dir).unwrap();
        assert_eq!(writes.read("late.txt", 0, 32).unwrap(), b"first and more");
        let given = || owner.given();
        let waited = Instant::now();
        while given().is_empty() && waited.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(given(), ["Write late.txt from outside"], "only what the project had not taken");
        assert!(writes.settled("made/a.txt"), "what it had is the tree's alone");
        drop(writes);
        done(&dir);
    }

    #[test]
    fn a_serving_that_ended_leaves_nothing_to_take_up() {
        let dir = std::env::temp_dir().join(format!("pe2-writes-ended-{}", std::process::id()));
        done(&dir);
        let owner = Holder::new(&dir, &[], Takes::Nothing);
        let served = owner.served.clone();
        {
            let writes = Writes::new(&served, owner.clone(), &dir).unwrap();
            let handle = writes.create("left.txt").unwrap();
            writes.write(handle, 0, b"x").unwrap();
            writes.close(handle, true).unwrap();
            writes.finish();
            assert_eq!(*owner.left.lock().unwrap(), ["left.txt"], "its owner's to keep, as serving ends");
        }
        owner.take(Takes::All);
        let writes = Writes::new(&served, owner.clone(), &dir).unwrap();
        assert!(writes.shown("left.txt").is_none(), "not taken up again: it was set aside");
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(owner.given().len(), 0, "and nothing is given");
        drop(writes);
        done(&dir);
    }

    #[test]
    fn what_nobody_took_by_the_end_is_left_with_the_owner() {
        let dir = std::env::temp_dir().join(format!("pe2-writes-left-{}", std::process::id()));
        let owner = Holder::new(&dir, &[], Takes::Nothing);
        let served = owner.served.clone();
        let writes = Writes::new(&served, owner.clone(), &dir).unwrap();
        let handle = writes.create("late.txt").unwrap();
        writes.write(handle, 0, b"x").unwrap();
        writes.close(handle, true).unwrap();
        writes.finish();
        assert_eq!(*owner.left.lock().unwrap(), ["late.txt"]);
        drop(writes);
        done(&dir);
    }

    #[test]
    fn new_files_that_have_settled_are_given_to_the_project_together() {
        let dir = std::env::temp_dir().join(format!("pe2-writes-many-{}", std::process::id()));
        let owner = Holder::new(&dir, &[], Takes::All);
        let served = owner.served.clone();
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
        done(&dir);
    }

    #[test]
    fn what_waits_to_settle_is_given_to_the_project_as_serving_ends() {
        let dir = std::env::temp_dir().join(format!("pe2-writes-finish-{}", std::process::id()));
        let owner = Holder::new(&dir, &[], Takes::All);
        let served = owner.served.clone();
        let writes = Writes::with(&served, owner.clone(), &dir, sfs::log::Config::default(), UNHURRIED).unwrap();
        let handle = writes.create("late.txt").unwrap();
        writes.write(handle, 0, b"x").unwrap();
        writes.close(handle, true).unwrap();
        // At once, long before it would have settled.
        writes.finish();
        let given = owner.given();
        assert_eq!(given, ["Write late.txt from outside"]);
        drop(writes);
        done(&dir);
    }

    #[test]
    fn a_folder_the_project_was_given_is_shown_before_the_tree_follows() {
        let dir = std::env::temp_dir().join(format!("pe2-writes-given-{}", std::process::id()));
        // An owner that takes what it is given, the tree served following in its own time - as where another process
        // holds the project, and lists what is served a moment later.
        let owner = Holder::new(&dir, &[], Takes::All);
        let served = owner.served.clone();
        let writes = Writes::new(&served, owner.clone(), &dir).unwrap();
        writes.make_folder("made").expect("made");
        assert!(writes.shown("made").is_some_and(|shown| shown.folder), "there at once");
        assert_eq!(writes.list("").keys().collect::<Vec<_>>(), ["made"]);
        // What is written in it next finds it, and is the project's with it.
        let handle = writes.create("made/a.txt").unwrap();
        writes.write(handle, 0, b"x").unwrap();
        writes.close(handle, true).unwrap();
        writes.finish();
        let given = owner.given();
        assert_eq!(given, ["New made from outside", "Write a.txt from outside"]);
        // The tree shows it by then, and it is the tree's.
        assert!(owner.has("made/a.txt"));
        writes.settle();
        assert!(writes.settled("made"), "nothing shown over the tree any more");
        drop(writes);
        done(&dir);
    }

    #[test]
    fn a_folder_made_where_one_was_just_removed_is_kept() {
        let dir = std::env::temp_dir().join(format!("pe2-writes-folder-{}", std::process::id()));
        let owner = Holder::new(&dir, &[("kept/a.txt", "a")], Takes::All);
        let served = owner.served.clone();
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
        while owner.given().is_empty() && waited.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(50));
        }
        std::thread::sleep(super::Pace::default().most + Duration::from_millis(600));
        let changes = owner.given();
        assert_eq!(changes, ["Remove a.txt from outside"], "only its file is removed from the project");
        drop(writes);
        done(&dir);
    }

    #[test]
    fn a_file_removed_while_the_project_is_given_it_is_removed_from_the_project() {
        let dir = std::env::temp_dir().join(format!("pe2-writes-{}", std::process::id()));
        let (owner, starts, goes_on) = Holder::gated(&dir, &[]);
        let served = owner.served.clone();
        let writes = Writes::new(&served, owner.clone(), &dir).unwrap();
        // A new file, written and let go of: given to the project once it has settled.
        let handle = writes.create("a.txt").unwrap();
        writes.write(handle, 0, b"written").unwrap();
        writes.close(handle, true).unwrap();
        starts.recv_timeout(Duration::from_secs(5)).expect("the project is being given the file");
        // Removed at that moment: at once, without waiting for the project - which is to have the file, so its
        // removal is said due after it, rather than the file taken for one only written here, which would leave
        // it in the project.
        let asked = Instant::now();
        writes.remove("a.txt").expect("removed");
        assert!(asked.elapsed() < Duration::from_millis(500), "without waiting for the project");
        assert!(writes.shown("a.txt").is_none(), "hidden at once");
        goes_on.send(()).unwrap();
        let asked = Instant::now();
        let removed = || owner.given().iter().any(|c| c == "Remove a.txt from outside");
        while !removed() && asked.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(removed(), "removed from the project: {:?}", owner.given());
        assert!(served.tree().at("a.txt").is_none() && writes.shown("a.txt").is_none(), "and gone");
        drop(writes);
        done(&dir);
    }
}
