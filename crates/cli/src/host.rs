//! `pe2-cli project host`: the process serving a project as a filesystem (`project::host`), started by whoever wants it
//! served - `project serve`, the editor - and ending a while after nobody keeps it, once no file is open in what it
//! serves: what only looks at the folder (a watcher, a file browser) keeps nothing, and no read is cut off. Where this
//! machine says to keep it (`[filesystem] keep`), it serves on until stopped. It holds the project while nobody else does: what programs write through what it serves, and the edits sent
//! to it (`pe2-cli project write`, ...), are steps of its own history. Lent to another process (`Lend`), it lets go of
//! the lock, its history and the cache's record store, reads on what changes in the layers, and sends what programs
//! write to whoever holds the project now (an editor answers on its link); it takes the project back once let go.

use anyhow::{Context, Result, bail};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use interprocess::local_socket::ListenerOptions;
use interprocess::local_socket::prelude::*;
use project::Project;
use project::cache::Cache;
use project::history::History;
use project::host::{FromHost, ToHost};
use project::index::Index;
use project::lock::{Lock, LockError};

const WHO: &str = "pe2-cli project host";
/// How long after nobody keeps it it stops (once no file is open): a project opened again soon finds it serving.
const GRACE: Duration = Duration::from_secs(30);
/// How long after the last thing asked of it on the holder's link it stops, where nothing else keeps it: what is asked
/// next - an agent's next read, the next command - finds the project open.
const IDLE: Duration = Duration::from_mins(2);

/// What the host has while it holds the project; let go of in this order.
struct Held {
    link: Option<mcp::link::Listener>,
    history: History,
    lock: Lock,
}

struct State {
    /// Its index, while it holds the project; none while another does, which sends what is served (`listing`).
    index: Option<Index>,
    /// What it serves, as the holder last listed it (or as its own index did, when it let go).
    listing: project::host::Listing,
    /// What answering reads takes, made at the first and kept while it holds the project and nothing changes.
    reading: Option<Reading>,
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
    served: vfs::Served,
    /// Where it serves, as said to who keeps it, and what they should know of how.
    at: Mutex<String>,
    note: Mutex<Option<String>>,
    state: Mutex<State>,
    leases: AtomicUsize,
    /// When the last lease ended.
    quiet_since: Mutex<Instant>,
    stopping: AtomicBool,
    /// Held for a process that started it under its own lock (`PE2_PROJECT_LOCKED`): it ends with its last lease.
    inherited: bool,
    /// Whether anyone has kept it yet.
    kept: AtomicBool,
    /// What is being answered on the holder's link this moment, and when something was last asked there.
    answering: AtomicUsize,
    asked: Mutex<Instant>,
}

/// `pe2-cli project host`.
pub fn run(directory: &Path, backend: Option<&str>, address: Option<&str>, strays: Strays) -> Result<()> {
    // What it says of itself (`mcp::Holder`) is of the program as it is now, not as it may be built again.
    mcp::presence::note_start();
    let p = Project::load(directory)?;
    let (serving, address) = serving_of(&p, backend, address)?;
    project::cache::make(&p.root)?;
    // One host a project: a second one started in a race ends here, and its starter finds the first.
    let one = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(p.root.join(project::cache::DIR).join(project::host::LOCK))?;
    if one.try_lock().is_err() {
        bail!("{}: a host serves it already", project::host::shown(&p.root));
    }
    let served = vfs::Served::default();
    let (index, held) = match Lock::try_take(&p.root, WHO) {
        Ok(lock) => {
            let (index, history) = hold(&p, &lock)?;
            served.update(&index);
            (Some(index), Some(Held { link: None, history, lock }))
        }
        Err(LockError::Held(owner)) => {
            // Its holder sends what is served; what is read of it, its layers give - records read here.
            println!("{owner} holds the project: serving what it lists, and taking it once let go");
            if let Err(e) = project::records::Set::open(&p) {
                eprintln!("its records: {e:#}");
            }
            (None, None)
        }
        Err(e) => return Err(e.into()),
    };
    let inherited = held.as_ref().is_some_and(|h| h.lock.inherited());
    let state = State {
        index,
        listing: project::host::Listing::new(),
        reading: None,
        held,
        lent_to: None,
        written: std::collections::HashMap::new(),
    };
    let host = Arc::new(Host {
        served,
        project: p.clone(),
        at: Mutex::new(String::new()),
        note: Mutex::new(None),
        state: Mutex::new(state),
        leases: AtomicUsize::new(0),
        quiet_since: Mutex::new(Instant::now()),
        stopping: AtomicBool::new(false),
        inherited,
        kept: AtomicBool::new(false),
        answering: AtomicUsize::new(0),
        asked: Mutex::new(Instant::now()),
    });
    // What reading the project freed, given back: it serves on for long.
    project::give_back_memory();
    host.listen_for_edits();
    // What changes in its layers by other means than the filesystem served, taken in a batch at a time.
    let watched = Arc::downgrade(&host);
    let _watching = holder::watch::layers(&p, move |batch| {
        let Some(host) = watched.upgrade() else { return false };
        host.take_in(batch);
        true
    })?;
    let stopping = Arc::downgrade(&host);
    ctrlc::set_handler(move || {
        if let Some(host) = stopping.upgrade() {
            host.stopping.store(true, Ordering::Relaxed);
        }
    })?;
    // Held while it serves: dropped, it no longer does. None where it only holds the project, for what is asked of it.
    let mounted = serving.map(|serving| mount(&p, &host, serving, address, strays)).transpose()?;
    if mounted.is_none() {
        println!("{} held, and served nowhere", project::host::shown(&p.root));
    }
    // Listened to once it serves: who keeps it is told where.
    let _talking = talk(&host)?;
    std::thread::scope(|scope| {
        if let Some((serving, _, record)) = mounted.as_ref().filter(|(serving, ..)| serving.keeps()) {
            scope.spawn(|| keep_within(&host, *serving, record));
        }
        host.keep_going();
        // Stopping, whyever: what is said to it from now on finds it so.
        host.stopping.store(true, Ordering::Relaxed);
    });
    if let Some((_, mount, record)) = mounted {
        record.save();
        drop(mount);
    }
    println!("stopped");
    Ok(())
}

/// What a host serving a folder holds while it does: the backend, its mount, and what is read there.
type Mounted = (&'static dyn vfs::backend::Backend, Box<dyn vfs::backend::Mount>, Arc<vfs::access::Access>);

/// Serve the project's folder by a backend: what others left there retired, what is not the project's dealt with
/// as asked, then mounted.
fn mount(
    p: &Project,
    host: &Arc<Host>,
    serving: &'static dyn vfs::backend::Backend,
    address: Option<std::net::SocketAddr>,
    strays: Strays,
) -> Result<Mounted> {
    let staging = p.root.join(project::cache::DIR).join(project::cache::STAGING);
    let owner: Arc<dyn vfs::write::Owner> = host.clone();
    if serving.can().folder {
        // What another backend left there of its own serving is no stray: it goes first.
        vfs::backend::hand_over(serving, &p.filesystem.at, &served_name(p));
        clear_strays(p, serving, strays, owner.as_ref(), &staging)?;
    }
    let job = vfs::backend::Job {
        served: &host.served,
        owner: Some(owner),
        at: &p.filesystem.at,
        staging: &staging,
        name: &served_name(p),
        address,
    };
    let mount = serving
        .mount(&job)
        .with_context(|| format!("serving {} by `{}`", project::host::shown(&p.root), serving.name()))?;
    let at = mount.shown();
    println!(
        "{} served at {at} by `{}` ({} entries)",
        project::host::shown(&p.root),
        serving.name(),
        host.served.tree().len()
    );
    *lock(&host.at) = at;
    *lock(&host.note) = mount.note();
    Ok((serving, mount, remembered(host, serving.keeps())))
}

/// What is read of the folder served, remembered from one serving to the next: what a backend that keeps what is read
/// lets go of last.
fn remembered(host: &Host, keeps: bool) -> Arc<vfs::access::Access> {
    let record = Arc::new(vfs::access::Access::of(&host.project.root, "the project's folder"));
    if !keeps {
        // Whatever kept what was read before has handed the folder over, and with it let go of what it kept.
        record.nothing_kept();
    }
    let noted = Arc::clone(&record);
    host.served.watch(move |path, seen| match seen {
        vfs::Seen::Read => noted.read(path),
        vfs::Seen::Kept(bytes) => noted.kept(path, bytes),
        vfs::Seen::Dropped => noted.dropped(path),
    });
    record
}

/// How often what a backend keeps of the folder is brought within its budget.
const COLLECTING: Duration = Duration::from_mins(10);

/// For a backend that keeps what is read, while the host serves: what it keeps of the folder brought within its
/// budget (`[filesystem] kept`) - now, every ten minutes, and as serving ends.
fn keep_within(host: &Host, serving: &dyn vfs::backend::Backend, record: &vfs::access::Access) {
    let machine = extensions::filesystem_defaults();
    let says = &host.project.filesystem;
    let kept = vfs::backend::budget(&[says.kept.as_deref(), machine.kept.as_deref()], vfs::backend::KEPT);
    serving.collect(&says.at, record, kept);
    let mut collected = Instant::now();
    while !host.stopping.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(500));
        if collected.elapsed() >= COLLECTING {
            serving.collect(&says.at, record, kept);
            record.save();
            collected = Instant::now();
        }
    }
    serving.collect(&says.at, record, kept);
}

/// What is done with what the folder a project is served at holds that is not the project's (`vfs::strays`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Strays {
    /// It is not served over them: they are said, and stay.
    Refuse,
    /// They are moved beside it, to `assets.set-aside/<time>`.
    SetAside,
    /// They are written into the project, as edits through the folder are.
    TakeIn,
}

impl Strays {
    /// The arguments a host is started with to do this.
    pub fn args(self) -> Vec<String> {
        match self {
            Strays::Refuse => vec![],
            Strays::SetAside => vec!["--strays".into(), "set-aside".into()],
            Strays::TakeIn => vec!["--strays".into(), "take-in".into()],
        }
    }
}

/// Leave the folder a project is served at holding nothing that is not the project's, as `strays` says.
fn clear_strays(
    p: &Project,
    serving: &dyn vfs::backend::Backend,
    strays: Strays,
    owner: &dyn vfs::write::Owner,
    staging: &Path,
) -> Result<()> {
    let at = &p.filesystem.at;
    let found = vfs::strays::strays(at, serving);
    if found.is_empty() {
        return Ok(());
    }
    let seconds = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let aside = p.root.join(project::SET_ASIDE).join(seconds.to_string());
    let shown = project::host::shown(at);
    match strays {
        Strays::Refuse => {
            let listed: Vec<&str> = found.iter().take(8).map(String::as_str).collect();
            let more = if found.len() > listed.len() {
                format!(", and {} more", found.len() - listed.len())
            } else {
                String::new()
            };
            bail!(
                "{shown} holds {} that {} not the project's ({}{more}): it is not served over them. Serve with \
                 --set-aside to move them to {}, or --take-in to write them into the project",
                files(found.len()),
                if found.len() == 1 { "is" } else { "are" },
                listed.join(", "),
                project::host::shown(&p.root.join(project::SET_ASIDE)),
            );
        }
        Strays::SetAside => {
            vfs::strays::set_aside(at, &found, &aside)?;
            println!("{} {shown} held set aside in {}", files(found.len()), project::host::shown(&aside));
        }
        Strays::TakeIn => {
            let kept = vfs::strays::take_in(at, &found, owner, staging, &aside)?;
            println!("{} {shown} held written into the project", files(found.len() - kept.len()));
            if !kept.is_empty() {
                println!(
                    "{} more - editors' leftovers, names storage keeps - set aside in {}",
                    files(kept.len()),
                    project::host::shown(&aside)
                );
            }
        }
    }
    Ok(())
}

/// A count of files, as said.
pub fn files(count: usize) -> String {
    if count == 1 { "1 file".into() } else { format!("{count} files") }
}

/// What serves a project's folder, written through, and the address it does at where it serves at one: as asked, then
/// as the project says (`project.local.toml`), then the machine - else the backend best suited here. A project or
/// machine saying `none` is only not served by itself: asked to be, it is, by the best suited. Asked to be served by
/// `none`, it is served nowhere: the host only holds it.
fn serving_of(
    p: &Project,
    asked: Option<&str>,
    address: Option<&str>,
) -> Result<(Option<&'static dyn vfs::backend::Backend>, Option<std::net::SocketAddr>)> {
    use vfs::backend::{Can, Setting, choose, setting, why_none};
    let machine = extensions::filesystem_defaults();
    let said = |says: &Option<String>| says.clone().filter(|says| says != "none");
    let (project, machine) = (said(&p.filesystem.serve), said(&machine.serve));
    let setting = setting(&[asked, project.as_deref(), machine.as_deref()]);
    if setting == Setting::None {
        return Ok((None, None));
    }
    let at = &p.filesystem.at;
    // A folder programs work in: any of it, any number of times.
    let how = project::content::Use::general();
    let Some(backend) = choose(&setting, Can::WRITTEN, at, &how, false).map_err(anyhow::Error::msg)? else {
        bail!("nothing serves {} here ({})", project::host::shown(&p.root), why_none(Can::WRITTEN, at));
    };
    let address = address.or(p.filesystem.address.as_deref());
    let address = address.map(|a| a.parse().with_context(|| format!("`{a}`: no address"))).transpose()?;
    Ok((Some(backend), address))
}

/// The project held: its index, over the cache's store, and a history of its own.
fn hold(p: &Project, lock: &Lock) -> Result<(Index, History)> {
    let cache = Cache::open(p, lock)?;
    let (history, _) = History::open(&cache)?;
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
                mcp::Request::Edits { ops, layer } => each(host.edits(&ops, layer.as_deref())),
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
        let State { index, held, reading, .. } = &mut *state;
        let (Some(index), Some(held)) = (index.as_mut(), held.as_ref()) else { return mcp::Reply::Declined };
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
                Err(e) => return mcp::Reply::Error(format!("the project cannot be read: {e:#}")),
            }
        }
        let Some(reading) = reading.as_mut() else { return mcp::Reply::Declined };
        reading.last = Instant::now();
        let view = &mut mcp::View { project: &self.project, index, extensions: &reading.extensions, editor: None };
        let reply = mcp::answer(view, request);
        // Between requests, nothing draws frames: the extensions' readings give their memory back.
        reading.extensions.release();
        let _ = project::imports::sweep_if_due(&reading.store, index);
        reply
    }

    /// Make an edit while holding the project: a step of its history, taken in at once.
    fn edit(&self, op: &vfs::ops::Op, layer: Option<&str>) -> mcp::Reply {
        let made = self.edits(std::slice::from_ref(op), layer).pop();
        made.unwrap_or_else(|| mcp::Reply::failed("nothing was asked".into()))
    }

    /// Make edits while holding the project, in order: each a step of its history, and what follows from them - the
    /// index, the tree served - brought up to date once for all of them, which is most of what one edit takes. How
    /// each went.
    fn edits(&self, ops: &[vfs::ops::Op], layer: Option<&str>) -> Vec<mcp::Reply> {
        let failed = |why: String| ops.iter().map(|_| mcp::Reply::failed(why.clone())).collect();
        let mut state = lock(&self.state);
        let State { index, held, written, reading, .. } = &mut *state;
        let (Some(index), Some(held)) = (index.as_mut(), held) else {
            return failed("the host does not hold the project".into());
        };
        // What reads were answered from changes: made again at the next.
        *reading = None;
        let Some(layer) = layer.map(str::to_string).or_else(|| vfs::write::writable_layer(index)) else {
            return failed("no layer of the project can be written".into());
        };
        // What the edits wrote that the index has not taken in yet, and all of it.
        let (mut pending, mut changed): (Vec<PathBuf>, Vec<PathBuf>) = (Vec::new(), Vec::new());
        let mut replies = Vec::with_capacity(ops.len());
        for op in ops {
            let mut apply = |index: &Index| {
                let _span = tracing::info_span!("host edit apply").entered();
                vfs::ops::apply(op, &self.project, index, &layer, &mut held.history)
            };
            let mut made = apply(index);
            // What it could not be made over may be what the ones before it made: taken in, and tried once more.
            if made.is_err() && !pending.is_empty() && index.refresh(&self.project, &pending).is_ok() {
                changed.append(&mut pending);
                made = apply(index);
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
            return replies;
        }
        let refreshed = {
            let _span = tracing::info_span!("host edit refresh").entered();
            index.refresh(&self.project, &pending)
        };
        changed.append(&mut pending);
        if let Err(e) = refreshed {
            return failed(format!("{e:#}"));
        }
        if written.len() > 10_000 {
            written.clear();
        }
        for path in &changed {
            written.insert(path.clone(), stamp(path));
        }
        let _span = tracing::info_span!("host served update").entered();
        self.served.update(index);
        project::give_back_memory();
        replies
    }

    /// Take in what changed in the layers: the index refreshed, the tree served made of it.
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
        let Some(index) = state.index.as_mut() else {
            // Another holds the project and lists what is served: what is read of it here is kept current.
            project::pack::reread_changed(&changed);
            let records = std::ffi::OsStr::new(project::records::DIR);
            if changed.iter().any(|p| p.components().any(|c| c.as_os_str() == records))
                && let Err(e) = project::records::Set::open(&self.project)
            {
                eprintln!("its records: {e:#}");
            }
            return;
        };
        match index.refresh(&self.project, &changed) {
            Ok(_) => {
                self.served.update(index);
                state.reading = None;
                project::give_back_memory();
            }
            Err(e) => eprintln!("taking in what changed: {e:#}"),
        }
    }

    /// What the holder lists as served: shown, while another holds the project.
    fn show(&self, whole: bool, shown: Vec<(String, project::host::Shown)>, gone: &[String]) -> FromHost {
        let mut state = lock(&self.state);
        if state.index.is_some() {
            return FromHost::Error { why: "the host holds the project: it lists what it serves itself".into() };
        }
        if whole {
            state.listing.clear();
        }
        for path in gone {
            state.listing.remove(path);
        }
        state.listing.extend(shown);
        self.served.show(&state.listing);
        FromHost::Done
    }

    /// Let go of the project for `who`: its link, history, record store and lock, in that order.
    fn lend(&self, who: &str) -> FromHost {
        let mut state = lock(&self.state);
        let holder = || project::lock::holder(&self.project.root).unwrap_or_else(|| "nobody".into());
        let Some(held) = state.held.take_if(|held| !held.lock.inherited()) else {
            return FromHost::NotHolding { holder: holder() };
        };
        let Held { link, history, lock } = held;
        drop(link);
        history.let_go();
        // Its index goes: the borrower holds the project's, and lists what is served from it. Until it does, what this
        // one listed is served on.
        if let Some(index) = state.index.take() {
            state.listing = index.served();
        }
        state.reading = None;
        drop(lock);
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
        {
            let mut state = lock(&self.state);
            if state.held.is_some() || state.lent_to.is_some() {
                return;
            }
            let Ok(taken) = Lock::try_take(&self.project.root, WHO) else { return };
            let held = (|| {
                let cache = Cache::open(&self.project, &taken)?;
                let (history, _) = History::open(&cache)?;
                let index = Index::open(&self.project, &cache)?;
                anyhow::Ok((history, index))
            })();
            match held {
                Ok((history, index)) => {
                    self.served.update(&index);
                    state.index = Some(index);
                    state.listing.clear();
                    state.held = Some(Held { link: None, history, lock: taken });
                }
                Err(e) => return eprintln!("taking the project back: {e:#}"),
            }
            project::give_back_memory();
        }
        println!("holding the project");
        self.listen_for_edits();
    }

    /// Serve until stopped, or until nobody keeps it and nothing has used it for a while; meanwhile taking the project
    /// back whenever it is free.
    fn keep_going(self: &Arc<Self>) {
        let mut waiting = false;
        while !self.stopping.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(200));
            self.take_back();
            self.forget_reading();
            if self.leases.load(Ordering::Relaxed) > 0 {
                waiting = false;
                continue;
            }
            // Under its starter's lock, it serves its starter alone.
            if self.inherited && self.kept.load(Ordering::Relaxed) {
                break;
            }
            if self.project.filesystem.keep || lock(&self.quiet_since).elapsed() < GRACE {
                continue;
            }
            if self.answering.load(Ordering::Relaxed) > 0 || lock(&self.asked).elapsed() < IDLE {
                continue;
            }
            match self.served.open_files() {
                0 => {
                    println!("nobody keeps it, nothing was asked of it for a while, and no file is open in it");
                    break;
                }
                open => {
                    if !std::mem::replace(&mut waiting, true) {
                        let are = if open == 1 { "is" } else { "are" };
                        println!("nobody keeps it: stopping once the {} open in it {are} closed", files(open));
                    }
                }
            }
        }
    }

    /// What answering reads took, let go of once none was asked for a while.
    fn forget_reading(&self) {
        let mut state = lock(&self.state);
        if state.reading.as_ref().is_some_and(|r| r.last.elapsed() > READING_KEPT) {
            state.reading = None;
            drop(state);
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
        FromHost::Status {
            pid: std::process::id(),
            at: lock(&self.at).clone(),
            holder,
            leases: self.leases.load(Ordering::Relaxed),
            open: self.served.open_files(),
        }
    }
}

impl vfs::write::Owner for Host {
    /// What a program wrote: made here while the host holds the project, else sent to whoever does.
    fn change(&self, change: vfs::write::Change) -> std::result::Result<(), String> {
        let op = vfs::ops::Op::from(change);
        if lock(&self.state).held.is_some() {
            return self.edit(&op, None).told().map(drop);
        }
        // Waited for as long as its holder's link lives: given up on while it is being made, it would be made again.
        let request = mcp::Request::Edit { op, layer: None };
        match mcp::link::forward_patiently(&self.project.root, &request) {
            // What is served follows as the holder lists it.
            Some(mcp::Reply::Edited { label, .. }) => {
                println!("{label} (by whoever holds the project)");
                Ok(())
            }
            Some(reply) => Err(reply.told().err().unwrap_or_else(|| "it answered with something else".into())),
            None => Err(self.no_holder()),
        }
    }

    /// What programs wrote at once: made here together while the host holds the project, the index and the tree
    /// served brought up to date once; else sent to whoever does, together too.
    fn changes(&self, changes: Vec<vfs::write::Change>) -> Vec<std::result::Result<(), String>> {
        let ops: Vec<vfs::ops::Op> = changes.into_iter().map(vfs::ops::Op::from).collect();
        if lock(&self.state).held.is_some() {
            return self.edits(&ops, None).into_iter().map(|reply| reply.told().map(drop)).collect();
        }
        let count = ops.len();
        let all = |why: String| (0..count).map(|_| Err(why.clone())).collect();
        match mcp::link::forward_patiently(&self.project.root, &mcp::Request::Edits { ops, layer: None }) {
            Some(mcp::Reply::EditedEach { made, .. }) if made.len() == count => {
                for label in made.iter().flatten() {
                    println!("{label} (by whoever holds the project)");
                }
                made.into_iter().map(|made| made.map(drop)).collect()
            }
            Some(reply) => all(reply.told().err().unwrap_or_else(|| "it answered with something else".into())),
            None => all(self.no_holder()),
        }
    }
}

impl Host {
    /// Why an edit could not be sent to whoever holds the project: nothing listens for one.
    fn no_holder(&self) -> String {
        let holder = project::lock::holder(&self.project.root).unwrap_or_else(|| "another process".into());
        format!("the project is in use by {holder}, which takes no edits: write it again once it is done")
    }
}

/// Edits made together, as one answer: each one's label, or why it was not made, and all they touched.
fn each(replies: Vec<mcp::Reply>) -> mcp::Reply {
    let mut touched = Vec::new();
    let made = replies.into_iter().map(|reply| match reply {
        mcp::Reply::Edited { label, touched: paths } => {
            touched.extend(paths);
            Ok(label)
        }
        other => Err(other.told().err().unwrap_or_else(|| "it answered with something else".into())),
    });
    let made = made.collect();
    mcp::Reply::EditedEach { made, touched }
}

/// Listen for what processes say to the host, each connection on a thread of its own. Listening ends with what this
/// returns.
fn talk(host: &Arc<Host>) -> Result<StopListening> {
    let name = project::host::name(&host.project.root)?;
    let listener = ListenerOptions::new().name(name).try_overwrite(true).reclaim_name(false).create_sync()?;
    let me = Arc::downgrade(host);
    let thread = std::thread::Builder::new().name("host".into()).spawn(move || {
        for stream in listener.incoming() {
            let Some(host) = me.upgrade() else { break };
            if host.stopping.load(Ordering::Relaxed) {
                break;
            }
            let Ok(stream) = stream else { continue };
            let _ = std::thread::Builder::new().name("host connection".into()).spawn(move || connection(&host, stream));
        }
    })?;
    Ok(StopListening { root: host.project.root.clone(), thread: Some(thread) })
}

/// Listening for what processes say: stopped when dropped.
struct StopListening {
    root: PathBuf,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for StopListening {
    fn drop(&mut self) {
        let Some(thread) = self.thread.take() else { return };
        // Woken, it finds the host stopping (or gone) and lets go of the name. Where it has left already - whoever
        // spoke to a host that was stopping woke it - nobody answers the name any more, and on Windows connecting to
        // it then waits for as long as a connection of it is still open, a keeper's: so never on this thread, which
        // would be the host never stopping.
        if !thread.is_finished()
            && let Ok(name) = project::host::name(&self.root)
        {
            let wake = move || drop(interprocess::local_socket::Stream::connect(name));
            let _ = std::thread::Builder::new().name("host wake".into()).spawn(wake);
        }
        let _ = thread.join();
    }
}

/// One process talking to the host: a lease while it keeps it or borrows the project, ended with the connection.
fn connection(host: &Arc<Host>, stream: interprocess::local_socket::Stream) {
    let mut stream = BufReader::new(stream);
    let (mut leased, mut borrowing) = (false, false);
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
                FromHost::Kept { at: lock(&host.at).clone(), note: lock(&host.note).clone() }
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
            Ok(ToHost::Changed { paths }) => {
                host.take_in(&paths);
                FromHost::Done
            }
            Ok(ToHost::Shown { whole, shown, gone }) => host.show(whole, shown, &gone),
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
    if leased {
        host.leased(false);
    }
}

/// What serves the tree, for as long as it is held: dropped, it stops.
/// The name a project is served by: its folder's.
pub fn served_name(p: &Project) -> String {
    format!("pe2:{}", p.root.file_name().and_then(|n| n.to_str()).unwrap_or("project"))
}

