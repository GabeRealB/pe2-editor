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
use holder::core::{Core, Queue};
use project::index::Index;
use project::lock::{Lock, LockError};

const WHO: &str = "pe2-cli project host";
/// How long after the last thing asked of it - on the holder's link, or by who kept it - it stops, where nothing
/// keeps it: what is asked next - an agent's next read, the next command - finds the project open.
const IDLE: Duration = Duration::from_mins(2);
/// How long a server is given to serve what it was first listed: where another backend served the folder before, it
/// retires what that one left there first - some tens of seconds for a project listed whole.
const SERVING: Duration = Duration::from_mins(3);

/// What the host has while it holds the project: its index and what it says of it (`holder::core::Core`), a
/// history of its own, the link it is asked on and the project's lock.
struct Held {
    link: Option<mcp::link::Listener>,
    core: Core,
    history: History,
    lock: Lock,
}

/// What the thread holding the project owns (`Host::queue`): all that is asked of the project is done there, in
/// the order it was asked.
struct State {
    held: Option<Held>,
    /// What answering reads takes, made at the first and kept while it holds the project and nothing changes.
    reading: Option<Reading>,
    /// What runs the epochs events set off, once one was signaled, and the extensions it runs them with: kept while
    /// one runs whatever changes meanwhile - an edit does not end a build - and let go of with the project.
    runner: Option<(holder::runner::Runner, std::sync::Arc<project::ext::Extensions>)>,
    /// Who it lent the project to, while they have it.
    lent_to: Option<String>,
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

struct Host {
    project: Project,
    /// What a server it starts is started with, where it was started to keep the project's folder served.
    serve: Option<Vec<String>>,
    /// The server kept serving the folder, for as long as the host runs.
    server: Mutex<Option<project::server::Lease>>,
    /// The feed of what is served, while it holds the project: the one its core says through, here for who asks
    /// how serving goes.
    files: Mutex<Option<Arc<holder::files::Files>>>,
    /// Where it says what it holds - the project's model, what is served, what became of what was due - a
    /// commit a refresh, whether or not a server is fed.
    says: Arc<holder::says::Says>,
    /// The thread holding the project, and what it owns: asked, never reached into.
    queue: Queue<State>,
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
    let says = Arc::new(holder::says::Says::of(&p));
    let held = match Lock::try_take(&p.root, WHO) {
        Ok(lock) => {
            let (index, history) = hold(&p, &lock)?;
            Some(Held { link: None, core: core(&p, index, &says), history, lock })
        }
        Err(LockError::Held(owner)) => {
            println!("{owner} holds the project: taking it once let go");
            None
        }
        Err(e) => return Err(e.into()),
    };
    let inherited = held.as_ref().is_some_and(|h| h.lock.inherited());
    let state = State { held, reading: None, runner: None, lent_to: None };
    let host = Arc::new(Host {
        project: p.clone(),
        serve,
        server: Mutex::new(None),
        files: Mutex::new(None),
        says,
        queue: Queue::start("holder", state)?,
        leases: AtomicUsize::new(0),
        quiet_since: Mutex::new(Instant::now()),
        stopping: Arc::new(AtomicBool::new(false)),
        inherited,
        kept: AtomicBool::new(false),
        answering: AtomicUsize::new(0),
        asked: Mutex::new(Instant::now()),
    });
    // The project's model, where its processes share it, holds what the index read; and the files are held to
    // what it took of them as known, behind what is asked first.
    let opened = host.clone();
    host.ask(move |state| {
        if let Some(held) = &mut state.held {
            opened.took(&mut held.core);
        }
    });
    // What reading the project freed, given back: it holds on for long.
    project::give_back_memory();
    host.listen_for_edits();
    // What changes in its layers by other means than through it, taken in a batch at a time.
    let watched = Arc::downgrade(&host);
    let _watching = holder::watch::layers(&p, move |change| {
        let Some(host) = watched.upgrade() else { return false };
        match change {
            holder::watch::Change::Paths(batch) => host.take_in(batch),
            holder::watch::Change::Error(e) => eprintln!("changes in the layers may be missed: {e}"),
        }
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
    // Ending in order, what it holds of the project is let go of as where it lends it: its history closed - what
    // its edits wrote had on disk, and the log needed for it no longer - before its lock is. A process that ends
    // lets go of nothing by itself, and whoever took the project next wrote its last edits again from the log.
    let ending = host.clone();
    host.ask(move |state| {
        if let Some(mut held) = state.held.take() {
            held.core.verified();
            ending.let_go(held);
        }
    });
    println!("stopped");
    Ok(())
}

/// A project held from here on, of an index just opened: each step it makes said as the host's log has it.
fn core(p: &Project, index: Index, says: &Arc<holder::says::Says>) -> Core {
    let mut core = Core::new(p.clone(), index, says.clone());
    core.made = Box::new(|label| println!("{label}"));
    core
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
    // Over the files as the project's model has them: held to that behind what is asked first (`Host::took`).
    Ok((Index::open_behind(p, &cache, &|_, _, _| {})?, history))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Host {
    /// Have the thread holding the project do this, after all it was asked before, and wait for what it comes to.
    /// None where that thread is gone - what it was doing failed, as it said: the host stops, for whoever asks
    /// next to start another.
    fn ask<R: Send + 'static>(&self, work: impl FnOnce(&mut State) -> R + Send + 'static) -> Option<R> {
        let done = self.queue.call(work);
        if done.is_none() && !self.stopping.swap(true, Ordering::Relaxed) {
            eprintln!("the thread holding the project is gone: stopping");
        }
        done
    }

    /// Answer what is asked on the holder's link while it holds the project - edits, and reads as an editor would; what
    /// needs a window is declined.
    fn listen_for_edits(self: &Arc<Self>) {
        let me: Weak<Host> = Arc::downgrade(self);
        let answer = move |request: mcp::Request| {
            let Some(host) = me.upgrade() else { return mcp::Reply::Declined };
            // Asked, and answering: it stays for both, and a while after.
            host.answering.fetch_add(1, Ordering::Relaxed);
            let reply = match request {
                mcp::Request::Edit { op, layer } => host.edit(op, layer),
                mcp::Request::Due => host.take_due(),
                mcp::Request::Signal { events, config } => host.signal(events, config),
                // Said of itself, with nothing of the project read for it.
                mcp::Request::Hello => mcp::Reply::Hello(mcp::Holder::this(false)),
                request if !request.uses_window() => host.read(request),
                _ => mcp::Reply::Declined,
            };
            *lock(&host.asked) = Instant::now();
            host.answering.fetch_sub(1, Ordering::Relaxed);
            reply
        };
        let root = self.project.root.clone();
        self.ask(move |state| {
            if let Some(held) = &mut state.held {
                match mcp::link::Listener::start(&root, answer) {
                    Ok(link) => held.link = Some(link),
                    Err(e) => eprintln!("edits cannot be sent here: {e}"),
                }
            }
        });
    }

    /// Answer a read while holding the project, from its index: the extensions loaded and the imports attached at the
    /// first, kept until something changes.
    fn read(self: &Arc<Self>, request: mcp::Request) -> mcp::Reply {
        let host = self.clone();
        let answered = self.ask(move |state| {
            if let Err(declined) = host.ready(state) {
                return declined;
            }
            let State { held, reading, .. } = state;
            let (Some(held), Some(reading)) = (held.as_mut(), reading.as_mut()) else { return mcp::Reply::Declined };
            let index = &mut held.core.index;
            let view = &mut mcp::View { project: &host.project, index, extensions: &reading.extensions, editor: None };
            let reply = mcp::answer(view, &request);
            // Between requests, nothing draws frames: the extensions' readings give their memory back.
            reading.extensions.release();
            let _ = project::imports::sweep_if_due(&reading.store, index);
            reply
        });
        answered.unwrap_or(mcp::Reply::Declined)
    }

    /// Have what answering takes: the extensions loaded and the imports attached to the index, made where they are not
    /// (the first time, and after what changed the project).
    ///
    /// # Errors
    /// It does not hold the project (declined), or the project cannot be read.
    fn ready(&self, state: &mut State) -> std::result::Result<(), mcp::Reply> {
        let State { held, reading, .. } = state;
        let Some(held) = held.as_mut() else { return Err(mcp::Reply::Declined) };
        if reading.is_none() {
            let made = (|| {
                let cache = Cache::open(&self.project, &held.lock)?;
                let extensions = extensions::for_project(&self.project);
                let _ = project::imports::Imports::open(&cache)?.attach(&mut held.core.index, &extensions);
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
    /// off the thread holding it, so that it reads and edits on meanwhile - and say what came of it once it has
    /// ended.
    fn signal(self: &Arc<Self>, events: Vec<String>, config: Option<String>) -> mcp::Reply {
        let host = self.clone();
        let started = self.ask(move |state| {
            host.ready(state)?;
            let State { held, reading, runner, .. } = state;
            let (Some(held), Some(reading)) = (held.as_ref(), reading.as_ref()) else { return Err(mcp::Reply::Declined) };
            // What the project requires and lacks, said; what its extensions have yet to fetch, fetched.
            if let Err(e) = crate::whole(&reading.extensions) {
                return Err(mcp::Reply::failed(format!("{e:#}")));
            }
            let chosen = host.project.configs.iter().find(|c| config.as_deref().is_none_or(|name| c.name == name));
            let Some(config) = chosen.cloned() else {
                return Err(mcp::Reply::failed("no such config in project.toml".into()));
            };
            // Run with the extensions as they are now, unless an epoch runs: that one keeps what it started with.
            let idle = runner.as_ref().is_none_or(|(runner, _)| !runner.busy());
            if idle && !runner.as_ref().is_some_and(|(_, with)| Arc::ptr_eq(with, &reading.extensions)) {
                let started = holder::runner::Runner::start(host.project.clone(), reading.extensions.clone());
                *runner = Some((started, reading.extensions.clone()));
            }
            let Some((runner, _)) = runner.as_ref() else { return Err(mcp::Reply::Declined) };
            let (reply, answered) = std::sync::mpsc::channel();
            let index = Arc::new(held.core.index.clone());
            runner.run(holder::runner::Job { roots: events, index, config, reply: Some(reply) });
            Ok(answered)
        });
        let answered = match started {
            Some(Ok(answered)) => answered,
            Some(Err(reply)) => return reply,
            None => return mcp::Reply::Declined,
        };
        let reply = answered.recv();
        // What it ended as is said to who asked: nothing here reads it again.
        self.ask(|state| {
            if let Some((runner, _)) = &state.runner {
                drop(runner.poll());
            }
        });
        reply.unwrap_or_else(|_| mcp::Reply::Error("the host let go of the project before it ended".into()))
    }

    /// Make an edit while holding the project: a step of its history, taken in at once.
    fn edit(&self, op: vfs::ops::Op, layer: Option<String>) -> mcp::Reply {
        let made = self.ask(move |state| {
            let held = state.held.as_mut()?;
            // What reads were answered from changes: made again at the next.
            state.reading = None;
            held.core.edits(&mut held.history, &[op], layer.as_deref()).0.pop()
        });
        match made {
            Some(Some(made)) => made,
            Some(None) => mcp::Reply::failed("the host does not hold the project".into()),
            None => mcp::Reply::failed("the host is stopping".into()),
        }
    }

    /// What it does of an index just opened as it takes the project (`Core::took`): what the files are found to
    /// be, where that is not as they were taken to be, is taken in by the thread holding the project once it is
    /// known - or as the first edit is made.
    fn took(self: &Arc<Self>, core: &mut Core) {
        let host = Arc::downgrade(self);
        core.took(move || {
            let Some(host) = host.upgrade() else { return };
            host.queue.post(|state| {
                if state.held.as_mut().is_some_and(|held| held.core.verified().is_some()) {
                    state.reading = None;
                }
            });
        });
    }

    /// Take what is due where the project's processes share it (`vfs::due`) - what programs did through the served
    /// folder - and make edits of it, in its order; what became of each is said there in the commit that says what
    /// is served after them. Declined while it does not hold the project: what is due stays so.
    fn take_due(&self) -> mcp::Reply {
        let taken = self.ask(|state| {
            let held = state.held.as_mut()?;
            state.reading = None;
            Some(held.core.take_due(&mut held.history, Ok(None), None).0)
        });
        taken.flatten().unwrap_or(mcp::Reply::Declined)
    }

    /// Take in what changed in the layers while it holds the project: the index refreshed, what is served listed
    /// again. While another holds it, that one does.
    fn take_in(&self, changed: Vec<PathBuf>) {
        self.ask(move |state| {
            let Some(held) = state.held.as_mut() else { return };
            match held.core.take_in(&changed) {
                Ok(Some(_)) => state.reading = None,
                Ok(None) => {}
                Err(e) => eprintln!("{e}"),
            }
        });
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
    fn feed_from_now(self: &Arc<Self>) {
        let host = self.clone();
        self.ask(move |state| {
            if let Some(held) = &mut state.held {
                host.feed_whole(&mut held.core);
            }
        });
    }

    /// Feed the server it keeps what is served, from now on: all the index lists first.
    fn feed_whole(&self, core: &mut Core) {
        if lock(&self.server).is_none() {
            return;
        }
        let args = self.serve.clone().unwrap_or_default();
        let files = Arc::new(holder::files::Files::feed(&self.project, args, self.says.clone()));
        core.feed_whole(files.clone());
        *lock(&self.files) = Some(files);
    }

    /// Feed no server any more: whoever holds the project does, or the one fed is gone.
    fn feed_no_more(&self, core: &mut Core) {
        *lock(&self.files) = None;
        core.files = None;
    }

    /// Let go of the project: the feed of what is served, its link, its history - what its edits wrote had on
    /// disk - its index, and its lock last.
    fn let_go(&self, held: Held) {
        let Held { link, mut core, history, lock: held } = held;
        self.feed_no_more(&mut core);
        drop(link);
        history.let_go();
        drop(core);
        drop(held);
    }

    /// Wait for the server to serve what it was first listed, where the host feeds it.
    ///
    /// # Errors
    /// It does not serve: over what the folder holds that is not the project's, most likely, as it says.
    fn served(&self) -> Result<()> {
        use holder::files::State;
        let asked = Instant::now();
        loop {
            let state = lock(&self.files).as_ref().map(|files| files.state());
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
    fn where_served(self: &Arc<Self>) -> FromHost {
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
            let feed = lock(&self.files).as_ref().map(|files| files.state());
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
    fn server_gone(self: &Arc<Self>) -> bool {
        if lock(&self.server).is_none() || project::server::running(&self.project.root) {
            return false;
        }
        let host = self.clone();
        self.ask(move |state| match &mut state.held {
            Some(held) => host.feed_no_more(&mut held.core),
            None => *lock(&host.files) = None,
        });
        *lock(&self.server) = None;
        println!("the server serving its folder stopped");
        true
    }

    /// Let go of the project for `who`: its link, history, record store and lock, in that order - and of the feed of
    /// what is served, which whoever holds the project lists from now on.
    fn lend(self: &Arc<Self>, who: String) -> FromHost {
        let host = self.clone();
        let lent = self.ask(move |state| {
            let holder = || project::lock::holder(&host.project.root).unwrap_or_else(|| "nobody".into());
            // Not while an epoch runs: who asks waits for it, as for any holder that is busy.
            if state.runner.as_ref().is_some_and(|(runner, _)| runner.busy()) {
                return FromHost::NotHolding { holder: format!("{WHO}, running {}", running(state)) };
            }
            let Some(held) = state.held.take_if(|held| !held.lock.inherited()) else {
                return FromHost::NotHolding { holder: holder() };
            };
            // Its index goes: the borrower holds the project's, and lists what is served from it. Until it does,
            // what this one listed is served on.
            host.let_go(held);
            state.reading = None;
            state.runner = None;
            println!("lent to {who}");
            state.lent_to = Some(who);
            // What its index held, given back.
            project::give_back_memory();
            FromHost::Lent
        });
        lent.unwrap_or(FromHost::Error { why: "the host is stopping".into() })
    }

    /// The borrower let go: the project is taken back as soon as it is free.
    fn returned(&self) {
        self.ask(|state| state.lent_to = None);
    }

    /// Take the project back, where nobody holds it and nobody borrowed it.
    fn take_back(self: &Arc<Self>) {
        let host = self.clone();
        let found = self.ask(move |state| {
            if state.held.is_some() || state.lent_to.is_some() {
                return None;
            }
            let taken = Lock::try_take(&host.project.root, WHO).ok()?;
            let found = Instant::now();
            let held = (|| {
                let cache = Cache::open(&host.project, &taken)?;
                let (history, recovered) = History::open(&cache)?;
                // It holds the project to stay: what its first edit would wait for is made ahead of it.
                history.prepare();
                if let Some(recovered) = recovered {
                    println!("{recovered}");
                }
                let index = Index::open_behind(&host.project, &cache, &|_, _, _| {})?;
                anyhow::Ok((history, index))
            })();
            match held {
                Ok((history, index)) => {
                    // Its folder served on: the server kept, where one runs, and listed what is served now.
                    if let Err(e) = host.keep_server() {
                        eprintln!("its folder is not served: {e:#}");
                    }
                    let mut core = core(&host.project, index, &host.says);
                    host.took(&mut core);
                    host.feed_whole(&mut core);
                    state.held = Some(Held { link: None, core, history, lock: taken });
                }
                Err(e) => {
                    eprintln!("taking the project back: {e:#}");
                    return None;
                }
            }
            project::give_back_memory();
            Some(found)
        });
        if let Some(found) = found.flatten() {
            self.listen_for_edits();
            println!("holding the project, {:.2}s after finding it free", found.elapsed().as_secs_f64());
        }
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
            let busy = |state: &mut State| {
                state.held.is_none() || state.runner.as_ref().is_some_and(|(runner, _)| runner.busy())
            };
            if self.ask(busy).unwrap_or(true) {
                continue;
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
        self.ask(|state| {
            if state.reading.as_ref().is_some_and(|r| r.last.elapsed() > READING_KEPT) {
                state.reading = None;
                // What ran its epochs goes with it, once none runs.
                drop(state.runner.take_if(|(runner, _)| !runner.busy()));
                project::give_back_memory();
            }
        });
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
        let root = self.project.root.clone();
        let holder = self.ask(move |state| match (&state.held, &state.lent_to) {
            (Some(_), _) => "the host".to_string(),
            (None, Some(who)) => who.clone(),
            (None, None) => project::lock::holder(&root).unwrap_or_else(|| "nobody".into()),
        });
        FromHost::Status {
            pid: std::process::id(),
            serves: self.serve.is_some() || lock(&self.server).is_some(),
            holder: holder.unwrap_or_else(|| "nobody".into()),
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
                let answer = host.lend(who);
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
