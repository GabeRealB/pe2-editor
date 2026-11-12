//! `pe2-cli project host`: the process holding a project while no editor does (`project::host`) - its lock, its index
//! and a history of its own - and answering what is asked of it on the holder's link: the edits commands and agents
//! make and programs write through the folder served, the reads, what events set off. It is started by whoever asks
//! something of a project nothing holds, and by whoever wants its folder served (`project serve`): then it keeps the
//! server doing so (`pe2-cli project server`) and feeds it what is served, as an editor does. Lent to another process
//! (`Lend`), it lets go of the lock, its history, its index and its link, and feeds nothing - whoever holds the project
//! does; it takes the project back once let go, and feeds the server again. It stops, only while it holds the
//! project, a while after nothing was asked of it and nothing keeps it.

use anyhow::{Result, bail};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use project::Project;
use project::cache::Cache;
use project::history::History;
use project::host::{FromHost, ROLE, ToHost};
use project::index::Index;
use project::lock::{Lock, LockError};

const WHO: &str = "pe2-cli project host";
/// How long after the last thing asked of it - on the holder's link, or by who kept it - it stops, where nothing
/// keeps it: what is asked next - an agent's next read, the next command - finds the project open.
const IDLE: Duration = Duration::from_mins(2);
/// How long a server is given to serve what it was first listed: where another backend served the folder before, it
/// retires what that one left there first - some tens of seconds for a project listed whole.
const SERVING: Duration = Duration::from_mins(3);

/// What the host has while it holds the project; let go of in this order.
struct Held {
    link: Option<mcp::link::Listener>,
    history: History,
    lock: Lock,
}

struct State {
    /// Its index, while it holds the project.
    index: Option<Index>,
    /// What is served of it, as last said to the server it feeds: brought up to each change, to say what differs.
    listing: project::server::Listing,
    /// What answering reads takes, made at the first and kept while it holds the project and nothing changes.
    reading: Option<Reading>,
    /// What runs the epochs events set off, once one was signaled, and the extensions it runs them with: kept while
    /// one runs whatever changes meanwhile - an edit does not end a build - and let go of with the project.
    runner: Option<(holder::runner::Runner, std::sync::Arc<project::ext::Extensions>)>,
    held: Option<Held>,
    /// Who it lent the project to, while they have it.
    lent_to: Option<String>,
    /// The files its own edits wrote, as they were then: reported again as they are - by its watch, by the program that
    /// asked for the edit - they are taken in already.
    written: std::collections::HashMap<PathBuf, Option<Stamp>>,
}

/// What answering an agent's read takes (`mcp::answer`): the extensions, the imports attached, the store kept; and when
/// it last answered one.
struct Reading {
    extensions: std::sync::Arc<project::ext::Extensions>,
    store: project::graph::store::Store,
    last: Instant,
}

/// How long what answering reads takes is kept without a read: it is most of what a host holding the project holds.
const READING_KEPT: Duration = Duration::from_mins(2);

/// What tells a file's version from another: its time and length; none where it is gone.
type Stamp = (std::time::SystemTime, u64);

fn stamp(path: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

struct Host {
    project: Project,
    /// What a server it starts is started with, where it was started to keep the project's folder served.
    serve: Option<Vec<String>>,
    /// The server kept serving the folder, for as long as the host runs; and, while it holds the project, the feed of
    /// what is served. Taken after `state`, never before.
    server: Mutex<Option<project::server::Lease>>,
    files: Mutex<Option<holder::files::Files>>,
    /// Where it says what it holds - the project's model, what is served, what became of what was due - a
    /// commit a refresh, whether or not a server is fed.
    says: Arc<holder::says::Says>,
    state: Mutex<State>,
    leases: AtomicUsize,
    /// When the last lease ended.
    quiet_since: Mutex<Instant>,
    stopping: Arc<AtomicBool>,
    /// Held for a process that started it under its own lock (`PE2_PROJECT_LOCKED`): it ends with its last lease.
    inherited: bool,
    /// Whether anyone has kept it yet.
    kept: AtomicBool,
    /// What is being answered on the holder's link this moment, and when something was last asked there.
    answering: AtomicUsize,
    asked: Mutex<Instant>,
    /// What was due and taken, by its number, until it is due no more: taken once, however often it is asked to
    /// look while what became of it is being said.
    due_taken: Mutex<std::collections::HashSet<u64>>,
}

/// `pe2-cli project host`: hold the project in `directory`, and keep its folder served by `backend` (at `address`,
/// dealing with what is not the project's there as `strays` says) unless that is `none`.
pub fn run(directory: &Path, backend: Option<&str>, address: Option<&str>, strays: crate::server::Strays) -> Result<()> {
    // What it says of itself (`mcp::Holder`) is of the program as it is now, not as it may be built again.
    mcp::presence::note_start();
    let p = Project::load(directory)?;
    // One host a project: a second one started in a race ends here, and its starter finds the first.
    let _one = ROLE.claim(&p.root)?;
    let serve = (backend != Some("none")).then(|| {
        let mut args = Vec::new();
        for (flag, value) in [("--backend", backend), ("--address", address)] {
            if let Some(value) = value {
                args.extend([flag.to_string(), value.to_string()]);
            }
        }
        args.extend(strays.args());
        args
    });
    let (index, held) = match Lock::try_take(&p.root, WHO) {
        Ok(lock) => {
            let (index, history) = hold(&p, &lock)?;
            (Some(index), Some(Held { link: None, history, lock }))
        }
        Err(LockError::Held(owner)) => {
            println!("{owner} holds the project: taking it once let go");
            (None, None)
        }
        Err(e) => return Err(e.into()),
    };
    let inherited = held.as_ref().is_some_and(|h| h.lock.inherited());
    let state = State {
        index,
        listing: project::server::Listing::new(),
        reading: None,
        runner: None,
        held,
        lent_to: None,
        written: std::collections::HashMap::new(),
    };
    let host = Arc::new(Host {
        project: p.clone(),
        serve,
        server: Mutex::new(None),
        files: Mutex::new(None),
        says: Arc::new(holder::says::Says::of(&p)),
        state: Mutex::new(state),
        leases: AtomicUsize::new(0),
        quiet_since: Mutex::new(Instant::now()),
        stopping: Arc::new(AtomicBool::new(false)),
        inherited,
        kept: AtomicBool::new(false),
        answering: AtomicUsize::new(0),
        due_taken: Mutex::default(),
        asked: Mutex::new(Instant::now()),
    });
    // The project's model, where its processes share it, holds what the index read.
    if let Some(index) = lock(&host.state).index.as_ref() {
        host.fill_model(index);
    }
    // What reading the project freed, given back: it holds on for long.
    project::give_back_memory();
    host.listen_for_edits();
    // What changes in its layers by other means than through it, taken in a batch at a time.
    let watched = Arc::downgrade(&host);
    let _watching = holder::watch::layers(&p, move |batch| {
        let Some(host) = watched.upgrade() else { return false };
        host.take_in(batch);
        true
    })?;
    let stopping = host.stopping.clone();
    ctrlc::set_handler(move || stopping.store(true, Ordering::Relaxed))?;
    // Its folder served, where it was started to keep it so or a server serves it already: kept first, then fed what
    // is served - before it listens, so that who started it for that finds it served, or is told why it is not.
    if host.serve.is_some() || project::server::running(&p.root) {
        host.keep_server()?;
        host.feed_from_now();
        host.served()?;
    } else {
        println!("{} held, and served nowhere", project::host::shown(&p.root));
    }
    let talking = Arc::downgrade(&host);
    let _talking = ROLE.listen(&p.root, host.stopping.clone(), move |stream| {
        if let Some(host) = talking.upgrade() {
            connection(&host, stream);
        }
    })?;
    host.keep_going();
    // Stopping, whyever: what is said to it from now on finds it so.
    host.stopping.store(true, Ordering::Relaxed);
    // Under its starter's lock it served its starter alone: what it had serve the folder ends with it.
    if host.inherited && lock(&host.server).is_some() {
        let _ = project::server::stop(&p.root);
    }
    println!("stopped");
    Ok(())
}

/// The project held: its index, over the cache's store, and a history of its own.
fn hold(p: &Project, lock: &Lock) -> Result<(Index, History)> {
    let cache = Cache::open(p, lock)?;
    let (history, recovered) = History::open(&cache)?;
    // It holds the project to stay: what its first edit would wait for is made ahead of it.
    history.prepare();
    if let Some(recovered) = recovered {
        println!("{recovered}");
    }
    Ok((Index::open(p, &cache)?, history))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Host {
    /// Answer what is asked on the holder's link while it holds the project - edits, and reads as an editor would; what
    /// needs a window is declined.
    fn listen_for_edits(self: &Arc<Self>) {
        let me: Weak<Host> = Arc::downgrade(self);
        let answer = move |request: mcp::Request| {
            let Some(host) = me.upgrade() else { return mcp::Reply::Declined };
            // Asked, and answering: it stays for both, and a while after.
            host.answering.fetch_add(1, Ordering::Relaxed);
            let reply = match request {
                mcp::Request::Edit { op, layer } => host.edit(&op, layer.as_deref()),
                mcp::Request::Due => host.take_due(),
                mcp::Request::Signal { events, config } => host.signal(events, config.as_deref()),
                // Said of itself, with nothing of the project read for it.
                mcp::Request::Hello => mcp::Reply::Hello(mcp::Holder::this(false)),
                request if !request.uses_window() => host.read(&request),
                _ => mcp::Reply::Declined,
            };
            *lock(&host.asked) = Instant::now();
            host.answering.fetch_sub(1, Ordering::Relaxed);
            reply
        };
        let mut state = lock(&self.state);
        if let Some(held) = &mut state.held {
            match mcp::link::Listener::start(&self.project.root, answer) {
                Ok(link) => held.link = Some(link),
                Err(e) => eprintln!("edits cannot be sent here: {e}"),
            }
        }
    }

    /// Answer a read while holding the project, from its index: the extensions loaded and the imports attached at the
    /// first, kept until something changes.
    fn read(&self, request: &mcp::Request) -> mcp::Reply {
        let mut state = lock(&self.state);
        if let Err(declined) = self.ready(&mut state) {
            return declined;
        }
        let State { index, reading, .. } = &mut *state;
        let (Some(index), Some(reading)) = (index.as_mut(), reading.as_mut()) else { return mcp::Reply::Declined };
        let view = &mut mcp::View { project: &self.project, index, extensions: &reading.extensions, editor: None };
        let reply = mcp::answer(view, request);
        // Between requests, nothing draws frames: the extensions' readings give their memory back.
        reading.extensions.release();
        let _ = project::imports::sweep_if_due(&reading.store, index);
        reply
    }

    /// Have what answering takes: the extensions loaded and the imports attached to the index, made where they are not
    /// (the first time, and after what changed the project).
    ///
    /// # Errors
    /// It does not hold the project (declined), or the project cannot be read.
    fn ready(&self, state: &mut State) -> std::result::Result<(), mcp::Reply> {
        let State { index, held, reading, .. } = state;
        let (Some(index), Some(held)) = (index.as_mut(), held.as_ref()) else { return Err(mcp::Reply::Declined) };
        if reading.is_none() {
            let made = (|| {
                let cache = Cache::open(&self.project, &held.lock)?;
                let extensions = extensions::for_project(&self.project);
                let _ = project::imports::Imports::open(&cache)?.attach(index, &extensions);
                let store = project::graph::store::Store::open(&cache)?;
                extensions.keep_in(store.clone());
                anyhow::Ok(Reading { extensions, store, last: Instant::now() })
            })();
            match made {
                Ok(made) => *reading = Some(made),
                Err(e) => return Err(mcp::Reply::Error(format!("the project cannot be read: {e:#}"))),
            }
        }
        if let Some(reading) = reading.as_mut() {
            reading.last = Instant::now();
        }
        Ok(())
    }

    /// Run what events set off while holding the project, as an editor does - one epoch, on the project as it is now,
    /// off this thread and not under the host's own lock, so that it reads and edits on meanwhile - and say what came
    /// of it once it has ended.
    fn signal(&self, events: Vec<String>, config: Option<&str>) -> mcp::Reply {
        let answered = {
            let mut state = lock(&self.state);
            if let Err(declined) = self.ready(&mut state) {
                return declined;
            }
            let State { index, reading, runner, .. } = &mut *state;
            let (Some(index), Some(reading)) = (index.as_ref(), reading.as_ref()) else { return mcp::Reply::Declined };
            // What the project requires and lacks, said; what its extensions have yet to fetch, fetched.
            if let Err(e) = crate::whole(&reading.extensions) {
                return mcp::Reply::failed(format!("{e:#}"));
            }
            let chosen = self.project.configs.iter().find(|c| config.is_none_or(|name| c.name == name));
            let Some(config) = chosen.cloned() else {
                return mcp::Reply::failed("no such config in project.toml".into());
            };
            // Run with the extensions as they are now, unless an epoch runs: that one keeps what it started with.
            let idle = runner.as_ref().is_none_or(|(runner, _)| !runner.busy());
            if idle && !runner.as_ref().is_some_and(|(_, with)| Arc::ptr_eq(with, &reading.extensions)) {
                let started = holder::runner::Runner::start(self.project.clone(), reading.extensions.clone());
                *runner = Some((started, reading.extensions.clone()));
            }
            let Some((runner, _)) = runner.as_ref() else { return mcp::Reply::Declined };
            let (reply, answered) = std::sync::mpsc::channel();
            let index = Arc::new(index.clone());
            runner.run(holder::runner::Job { roots: events, index, config, reply: Some(reply) });
            answered
        };
        let reply = answered.recv();
        // What it ended as is said to who asked: nothing here reads it again.
        if let Some((runner, _)) = &lock(&self.state).runner {
            drop(runner.poll());
        }
        reply.unwrap_or_else(|_| mcp::Reply::Error("the host let go of the project before it ended".into()))
    }

    /// Make an edit while holding the project: a step of its history, taken in at once.
    fn edit(&self, op: &vfs::ops::Op, layer: Option<&str>) -> mcp::Reply {
        let made = self.edits(std::slice::from_ref(op), layer).pop();
        made.unwrap_or_else(|| mcp::Reply::failed("nothing was asked".into()))
    }

    /// Have the project's model, where its processes share it, hold what an index just opened read: what it says
    /// first as it takes the project, another having held it since - or nobody - and said what it would.
    fn fill_model(&self, index: &Index) {
        self.says.forget();
        if let Err(e) = self.says.fill(&self.project.root, index) {
            eprintln!("{e}");
        }
    }

    /// Take what is due where the project's processes share it (`vfs::due`) - what programs did through the served
    /// folder - and make edits of it, in its order; what became of each is said there in the commit that says what
    /// is served after them. Declined while it does not hold the project: what is due stays so.
    fn take_due(&self) -> mcp::Reply {
        let root = &self.project.root;
        let due = project::shared::space(root).and_then(|space| vfs::due::due(&space.read()?));
        let due = match due {
            Ok(due) => due,
            Err(e) => return mcp::Reply::Error(format!("what is due could not be read: {e}")),
        };
        let due: Vec<(u64, vfs::ops::Op)> = {
            let mut taken = lock(&self.due_taken);
            taken.retain(|number| due.iter().any(|(due, _)| due == number));
            due.into_iter().filter(|(number, _)| !taken.contains(number)).collect()
        };
        if due.is_empty() {
            return mcp::Reply::Taken { count: 0 };
        }
        let (numbers, ops): (Vec<u64>, Vec<vfs::ops::Op>) = due.into_iter().unzip();
        match self.edits_answering(&ops, None, Some(&numbers)) {
            Some(made) => {
                lock(&self.due_taken).extend(numbers);
                mcp::Reply::Taken { count: made.len() }
            }
            None => mcp::Reply::Declined,
        }
    }

    /// Say what became of what was due with nothing of what is served changed for it: through what feeds the
    /// server, after all it was given to say before, or in the shared filesystem itself where nothing feeds one.
    fn answer_due(&self, answers: &[(u64, vfs::due::Answer)]) {
        if answers.is_empty() {
            return;
        }
        let unchanged = project::model::Delta::default();
        if let Some(files) = lock(&self.files).as_ref() {
            return files.show_changed_answering(Vec::new(), None, &unchanged, answers);
        }
        if let Err(e) = self.says.say(holder::says::Said::Unchanged, &unchanged, answers) {
            eprintln!("what became of what was due could not be said: {e}");
        }
    }

    /// Make edits while holding the project, in order: each a step of its history, and what follows from them - the
    /// index, what is served - brought up to date once for all of them, which is most of what one edit takes. How
    /// each went.
    fn edits(&self, ops: &[vfs::ops::Op], layer: Option<&str>) -> Vec<mcp::Reply> {
        let failed = || ops.iter().map(|_| mcp::Reply::failed("the host does not hold the project".into())).collect();
        self.edits_answering(ops, layer, None).unwrap_or_else(failed)
    }

    /// As `edits`; where they are what was due, by these numbers, what became of each is said where it was due,
    /// with what is served after them - and one that is so already (`Op::so_already`: taken a second time) is
    /// made no edit of. None where the host does not hold the project: nothing was made, and nothing said.
    fn edits_answering(&self, ops: &[vfs::ops::Op], layer: Option<&str>, due: Option<&[u64]>) -> Option<Vec<mcp::Reply>> {
        let failed = |why: String| ops.iter().map(|_| mcp::Reply::failed(why.clone())).collect::<Vec<_>>();
        // What became of each, for where it was due.
        let answers = |replies: &[mcp::Reply]| -> Vec<(u64, vfs::due::Answer)> {
            let became = replies.iter().map(|reply| match reply {
                mcp::Reply::Edited { label, .. } => Ok(label.clone()),
                other => Err(other.told().err().unwrap_or_else(|| "it answered with something else".into())),
            });
            due.map_or_else(Vec::new, |due| due.iter().copied().zip(became).collect())
        };
        let mut state = lock(&self.state);
        let State { index, listing, held, written, reading, .. } = &mut *state;
        let (Some(index), Some(held)) = (index.as_mut(), held) else { return None };
        // What reads were answered from changes: made again at the next.
        *reading = None;
        let Some(layer) = layer.map(str::to_string).or_else(|| vfs::write::writable_layer(index)) else {
            let replies = failed("no layer of the project can be written".into());
            let answers = answers(&replies);
            drop(state);
            self.answer_due(&answers);
            return Some(replies);
        };
        // What the edits wrote that the index has not taken in yet, and all of it.
        let (mut pending, mut changed): (Vec<PathBuf>, Vec<PathBuf>) = (Vec::new(), Vec::new());
        let mut replies = Vec::with_capacity(ops.len());
        // What the refreshes of all of them changed, together.
        let mut all = project::index::Refreshed::default();
        // The paths the edits made and not taken in yet named: one that names a path at, in or above one of them
        // is made over the project as they left it.
        let mut earlier: Vec<String> = Vec::new();
        for op in ops {
            if !pending.is_empty()
                && op.follows(&earlier)
                && let Ok(refreshed) = index.refresh(&self.project, &pending)
            {
                all.and(refreshed);
                changed.append(&mut pending);
                earlier.clear();
            }
            earlier.extend(op.paths().into_iter().map(str::to_string));
            let mut apply = |index: &Index| {
                let _span = tracing::info_span!("host edit apply").entered();
                vfs::ops::apply(op, &self.project, index, &layer, &mut held.history)
            };
            let mut made = apply(index);
            // What it could not be made over may be what the ones before it made: taken in, and tried once more.
            if made.is_err()
                && !pending.is_empty()
                && let Ok(refreshed) = index.refresh(&self.project, &pending)
            {
                all.and(refreshed);
                changed.append(&mut pending);
                made = apply(index);
            }
            // Due, not made, and so already - taken before, by a holder that ended before it said so, or said due
            // again by a serving after one that ended: it is as asked, and said so.
            if due.is_some() && made.is_err() && op.so_already(index) {
                made = Ok(("it is so already".to_string(), Vec::new()));
            }
            replies.push(match made {
                Ok((label, touched)) => {
                    pending.extend(touched.iter().cloned());
                    pending.extend(held.history.take_written());
                    println!("{label}");
                    mcp::Reply::Edited { label, touched }
                }
                Err(e) => mcp::Reply::failed(format!("{e:#}")),
            });
        }
        if pending.is_empty() && changed.is_empty() {
            let answers = answers(&replies);
            drop(state);
            self.answer_due(&answers);
            return Some(replies);
        }
        let refreshed = {
            let _span = tracing::info_span!("host edit refresh").entered();
            index.refresh(&self.project, &pending)
        };
        changed.append(&mut pending);
        match refreshed {
            Ok(refreshed) => all.and(refreshed),
            Err(e) => {
                let replies = failed(format!("{e:#}"));
                let answers = answers(&replies);
                drop(state);
                self.answer_due(&answers);
                return Some(replies);
            }
        }
        // Answered once the project's files have what was asked: whoever reads them next by other means - a
        // command after this one, a tool - finds them so. All that was asked together is had on disk together,
        // and each file is as its own writing left it when what watches the layers tells of it.
        held.history.settle();
        if written.len() > 10_000 {
            written.clear();
        }
        for path in &changed {
            written.insert(path.clone(), stamp(path));
        }
        self.feed(index, listing, &changed, &all, &answers(&replies));
        project::give_back_memory();
        Some(replies)
    }

    /// Take in what changed in the layers while it holds the project: the index refreshed, what is served listed
    /// again. While another holds it, that one does.
    fn take_in(&self, changed: &[PathBuf]) {
        let mut state = lock(&self.state);
        // What its own edits wrote, as they wrote it, it has taken in; written over since, it is forgotten.
        let written = &mut state.written;
        let changed: Vec<PathBuf> = changed
            .iter()
            .filter(|path| {
                let taken = written.get(*path).is_some_and(|was| *was == stamp(path));
                if !taken {
                    written.remove(*path);
                }
                !taken
            })
            .cloned()
            .collect();
        if changed.is_empty() {
            return;
        }
        let State { index, listing, reading, .. } = &mut *state;
        let Some(index) = index.as_mut() else { return };
        match index.refresh(&self.project, &changed) {
            Ok(refreshed) => {
                self.feed(index, listing, &changed, &refreshed, &[]);
                *reading = None;
                project::give_back_memory();
            }
            Err(e) => eprintln!("taking in what changed: {e:#}"),
        }
    }

    /// Keep the server serving the project's folder, for as long as the host runs: the one running or - where the
    /// host was started to keep the folder served - one started. Whether one is kept.
    ///
    /// # Errors
    /// None could be started: nothing can serve the folder here, as it says.
    fn keep_server(&self) -> Result<bool> {
        let mut server = lock(&self.server);
        if server.is_none() {
            let root = &self.project.root;
            *server = match &self.serve {
                Some(args) => Some(project::server::keep(root, WHO, &std::env::current_exe()?, args)?),
                None => project::server::keep_running(root, WHO),
            };
        }
        Ok(server.is_some())
    }

    /// Feed the server it keeps what is served, from now on, where it holds the project: the whole of it first.
    fn feed_from_now(&self) {
        let mut state = lock(&self.state);
        let State { index, listing, .. } = &mut *state;
        if let Some(index) = index.as_ref() {
            self.feed_whole(index, listing);
        }
    }

    /// Feed the server it keeps what is served, from now on: all `index` lists first, kept as `listing` to tell
    /// what differs after each change.
    fn feed_whole(&self, index: &Index, listing: &mut project::server::Listing) {
        if lock(&self.server).is_none() {
            return;
        }
        let files = holder::files::Files::feed(&self.project, self.serve.clone().unwrap_or_default(), self.says.clone());
        *listing = index.served();
        files.show(Arc::new(listing.clone()));
        *lock(&self.files) = Some(files);
    }

    /// What is served follows what changed, where it feeds a server: told which files changed, for what it reads of
    /// them to be read again, then what differs of what is served - of the entries the refresh touched and those
    /// gone - and waited for, so that who asked for a change finds it in the folder once answered. What became of
    /// what was due (`answers`) is said in the same commit.
    fn feed(
        &self,
        index: &Index,
        listing: &mut project::server::Listing,
        changed: &[PathBuf],
        refreshed: &project::index::Refreshed,
        answers: &[(u64, vfs::due::Answer)],
    ) {
        let _span = tracing::info_span!("host served update").entered();
        if let Some(files) = lock(&self.files).as_ref() {
            // `project.toml` changed: the layers themselves may have. All of it, listed again.
            if refreshed.project_file {
                *listing = index.served();
                files.show_changed_answering(changed.to_vec(), Some(Arc::new(listing.clone())), &refreshed.model, answers);
            } else {
                let delta = index.serve_changed(listing, refreshed);
                files.show_delta_answering(changed.to_vec(), delta, &refreshed.model, answers);
            }
            return;
        }
        // Nothing feeds a server: what changed of the model and what became of what was due, by themselves.
        if let Err(e) = self.says.say(holder::says::Said::Unchanged, &refreshed.model, answers) {
            eprintln!("what the change came to could not be said: {e}");
        }
    }

    /// Wait for the server to serve what it was first listed, where the host feeds it.
    ///
    /// # Errors
    /// It does not serve: over what the folder holds that is not the project's, most likely, as it says.
    fn served(&self) -> Result<()> {
        use holder::files::State;
        let asked = Instant::now();
        loop {
            let state = lock(&self.files).as_ref().map(holder::files::Files::state);
            match state {
                // Whoever holds the project feeds it.
                None => return Ok(()),
                Some(State::Served(at, _)) => {
                    println!("{} served at {at}", project::host::shown(&self.project.root));
                    return Ok(());
                }
                Some(State::Failed { why, .. }) => bail!(why),
                Some(State::Starting) if asked.elapsed() > SERVING => {
                    bail!("the server did not serve it within {}s", SERVING.as_secs())
                }
                Some(State::Starting) => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    }

    /// Where the project's folder is served, for who keeps the host to keep it so: once the server serves what it
    /// was listed - by the host, or by whoever holds the project.
    fn where_served(&self) -> FromHost {
        match self.keep_server() {
            Ok(true) => {}
            Ok(false) => return FromHost::Error { why: "the host serves the project's folder nowhere".into() },
            Err(e) => return FromHost::Error { why: format!("{e:#}") },
        }
        if lock(&self.files).is_none() {
            self.feed_from_now();
        }
        let asked = Instant::now();
        loop {
            let feed = lock(&self.files).as_ref().map(holder::files::Files::state);
            if let Some(holder::files::State::Failed { why, .. }) = feed {
                return FromHost::Error { why };
            }
            match project::server::status(&self.project.root) {
                Some(project::server::FromServer::Status { at, note, .. }) if !at.is_empty() => {
                    return FromHost::Kept { at, note };
                }
                Some(_) if asked.elapsed() < SERVING => std::thread::sleep(Duration::from_millis(50)),
                Some(_) => return FromHost::Error { why: "the server was listed nothing to serve in time".into() },
                None => return FromHost::Error { why: "the server serving its folder is gone".into() },
            }
        }
    }

    /// Whether the server it kept is gone: then its lease on it and its feed are let go of.
    fn server_gone(&self) -> bool {
        if lock(&self.server).is_none() || project::server::running(&self.project.root) {
            return false;
        }
        *lock(&self.files) = None;
        *lock(&self.server) = None;
        println!("the server serving its folder stopped");
        true
    }

    /// Let go of the project for `who`: its link, history, record store and lock, in that order - and of the feed of
    /// what is served, which whoever holds the project lists from now on.
    fn lend(&self, who: &str) -> FromHost {
        let mut state = lock(&self.state);
        let holder = || project::lock::holder(&self.project.root).unwrap_or_else(|| "nobody".into());
        // Not while an epoch runs: who asks waits for it, as for any holder that is busy.
        if state.runner.as_ref().is_some_and(|(runner, _)| runner.busy()) {
            return FromHost::NotHolding { holder: format!("{WHO}, running {}", running(&state)) };
        }
        let Some(held) = state.held.take_if(|held| !held.lock.inherited()) else {
            return FromHost::NotHolding { holder: holder() };
        };
        let Held { link, history, lock: held } = held;
        *lock(&self.files) = None;
        drop(link);
        history.let_go();
        // Its index goes: the borrower holds the project's, and lists what is served from it. Until it does, what this
        // one listed is served on.
        state.index = None;
        state.listing.clear();
        state.reading = None;
        state.runner = None;
        drop(held);
        state.lent_to = Some(who.to_string());
        drop(state);
        // What its index held, given back.
        project::give_back_memory();
        println!("lent to {who}");
        FromHost::Lent
    }

    /// The borrower let go: the project is taken back as soon as it is free.
    fn returned(&self) {
        lock(&self.state).lent_to = None;
    }

    /// Take the project back, where nobody holds it and nobody borrowed it.
    fn take_back(self: &Arc<Self>) {
        let found;
        {
            let mut state = lock(&self.state);
            if state.held.is_some() || state.lent_to.is_some() {
                return;
            }
            let Ok(taken) = Lock::try_take(&self.project.root, WHO) else { return };
            found = Instant::now();
            let held = (|| {
                let cache = Cache::open(&self.project, &taken)?;
                let (history, recovered) = History::open(&cache)?;
                // It holds the project to stay: what its first edit would wait for is made ahead of it.
                history.prepare();
                if let Some(recovered) = recovered {
                    println!("{recovered}");
                }
                let index = Index::open(&self.project, &cache)?;
                anyhow::Ok((history, index))
            })();
            match held {
                Ok((history, index)) => {
                    // Its folder served on: the server kept, where one runs, and listed what is served now.
                    if let Err(e) = self.keep_server() {
                        eprintln!("its folder is not served: {e:#}");
                    }
                    self.fill_model(&index);
                    self.feed_whole(&index, &mut state.listing);
                    state.index = Some(index);
                    state.held = Some(Held { link: None, history, lock: taken });
                }
                Err(e) => return eprintln!("taking the project back: {e:#}"),
            }
            project::give_back_memory();
        }
        self.listen_for_edits();
        println!("holding the project, {:.2}s after finding it free", found.elapsed().as_secs_f64());
    }

    /// Hold on until stopped, or until - while it holds the project - nothing keeps it and nothing was asked of it for
    /// a while; meanwhile taking the project back whenever it is free.
    fn keep_going(self: &Arc<Self>) {
        let (mut waiting, mut looked) = (false, Instant::now());
        while !self.stopping.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(200));
            self.take_back();
            self.forget_reading();
            if looked.elapsed() > Duration::from_secs(1) {
                looked = Instant::now();
                // What its index read is written to the cache's store here, between edits, not in one.
                if let Some(index) = &lock(&self.state).index {
                    index.tick();
                }
                // Started to keep the folder served, it stops where that is stopped.
                if self.server_gone() && self.serve.is_some() {
                    break;
                }
            }
            if self.leases.load(Ordering::Relaxed) > 0 {
                waiting = false;
                continue;
            }
            // Under its starter's lock, it serves its starter alone.
            if self.inherited && self.kept.load(Ordering::Relaxed) {
                break;
            }
            // It leaves only while it holds the project, with nothing running: lent, or waiting for it, it is there
            // for when the project is let go of.
            {
                let state = lock(&self.state);
                if state.held.is_none() || state.runner.as_ref().is_some_and(|(runner, _)| runner.busy()) {
                    continue;
                }
            }
            let serves = lock(&self.server).is_some();
            if serves && self.project.filesystem.keep {
                continue;
            }
            let asked = lock(&self.asked).elapsed().min(lock(&self.quiet_since).elapsed());
            if self.answering.load(Ordering::Relaxed) > 0 || asked < IDLE {
                continue;
            }
            // What is open in what is served keeps it too: the server stops after it, and no read is cut off.
            let open = match serves.then(|| project::server::status(&self.project.root)).flatten() {
                Some(project::server::FromServer::Status { open, .. }) => open,
                _ => 0,
            };
            if open == 0 {
                println!("nothing keeps it, and nothing was asked of it for a while");
                break;
            }
            if !std::mem::replace(&mut waiting, true) {
                let are = if open == 1 { "is" } else { "are" };
                let files = crate::server::files(open);
                println!("nothing keeps it: stopping once the {files} open in what is served {are} closed");
            }
        }
    }

    /// What answering reads took, let go of once none was asked for a while.
    fn forget_reading(&self) {
        let mut state = lock(&self.state);
        if state.reading.as_ref().is_some_and(|r| r.last.elapsed() > READING_KEPT) {
            state.reading = None;
            // What ran its epochs goes with it, once none runs.
            let runner = state.runner.take_if(|(runner, _)| !runner.busy());
            drop(state);
            drop(runner);
            project::give_back_memory();
        }
    }

    /// A lease begun, or ended.
    fn leased(&self, begun: bool) {
        if begun {
            self.kept.store(true, Ordering::Relaxed);
            self.leases.fetch_add(1, Ordering::Relaxed);
        } else {
            self.leases.fetch_sub(1, Ordering::Relaxed);
        }
        *lock(&self.quiet_since) = Instant::now();
    }

    fn status(&self) -> FromHost {
        let state = lock(&self.state);
        let holder = match (&state.held, &state.lent_to) {
            (Some(_), _) => "the host".to_string(),
            (None, Some(who)) => who.clone(),
            (None, None) => project::lock::holder(&self.project.root).unwrap_or_else(|| "nobody".into()),
        };
        drop(state);
        FromHost::Status {
            pid: std::process::id(),
            serves: self.serve.is_some() || lock(&self.server).is_some(),
            holder,
            leases: self.leases.load(Ordering::Relaxed),
        }
    }
}

/// The events the epoch running was signaled by, as said of a host that is busy.
fn running(state: &State) -> String {
    let events = state.runner.as_ref().map_or_default(|(runner, _)| runner.running());
    if events.is_empty() { "an epoch".to_string() } else { events.join(", ") }
}

/// One process talking to the host: a lease while it keeps the folder served or borrows the project, ended with the
/// connection.
fn connection(host: &Arc<Host>, stream: interprocess::local_socket::Stream) {
    let mut stream = BufReader::new(stream);
    let (mut leased, mut borrowing, mut leaving) = (false, false, None::<Instant>);
    loop {
        let said = match project::wire::receive::<ToHost>(&mut stream) {
            Ok(Some(said)) => Ok(said),
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => Err(e),
            Ok(None) | Err(_) => break,
        };
        let answer = match said {
            Ok(ToHost::Keep { who }) => {
                println!("kept by {who}");
                if !std::mem::replace(&mut leased, true) {
                    host.leased(true);
                }
                host.where_served()
            }
            Ok(ToHost::Lend { who }) => {
                let answer = host.lend(&who);
                if answer == FromHost::Lent {
                    borrowing = true;
                    if !std::mem::replace(&mut leased, true) {
                        host.leased(true);
                    }
                }
                answer
            }
            Ok(ToHost::Leaving { who }) => {
                println!("{who} is letting go of the project");
                leaving = Some(Instant::now());
                FromHost::Awaiting
            }
            Ok(ToHost::Status) => host.status(),
            Ok(ToHost::Stop) => {
                host.stopping.store(true, Ordering::Relaxed);
                FromHost::Stopping
            }
            Err(e) => FromHost::Error { why: format!("not something said to the host: {e}") },
        };
        if project::wire::send(stream.get_mut(), &answer).is_err() {
            break;
        }
    }
    if borrowing {
        println!("given back");
        host.returned();
    }
    // Let go of this moment, by who said so with its connection: taken now, not when next looked for.
    if let Some(said) = leaving {
        println!("let go of {:.2}s after saying so", said.elapsed().as_secs_f64());
    }
    if borrowing || leaving.is_some() {
        host.take_back();
    }
    if leased {
        host.leased(false);
    }
}
