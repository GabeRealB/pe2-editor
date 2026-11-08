//! `pe2-cli project host`: the process serving a project as a filesystem (`project::host`), started by whoever wants it
//! served - `project serve`, the editor - and ending a while after nobody keeps it, once no file is open in what it
//! serves: what only looks at the folder (a watcher, a file browser) keeps nothing, and no read is cut off. Where this
//! machine says to keep it (`[filesystem] keep`), it serves on until stopped. It holds the project while nobody else does: what programs write through what it serves, and the edits sent
//! to it (`pe2-cli project write`, ...), are steps of its own history. Lent to another process (`Lend`), it lets go of
//! the lock, its history and the cache's record store, reads on what changes in the layers, and sends what programs
//! write to whoever holds the project now (an editor answers on its link); it takes the project back once let go.

use anyhow::{Context, Result, bail};
use std::io::{BufRead, BufReader, Write};
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

/// What the host has while it holds the project; let go of in this order.
struct Held {
    link: Option<mcp::link::Listener>,
    history: History,
    lock: Lock,
}

struct State {
    index: Index,
    held: Option<Held>,
    /// Who it lent the project to, while they have it.
    lent_to: Option<String>,
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
}

/// `pe2-cli project host`.
pub fn run(directory: &Path, backend: Option<&str>, address: Option<&str>, strays: Strays) -> Result<()> {
    let p = Project::load(directory)?;
    let serving = serving_of(&p, backend, address)?;
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
    let (index, held) = match Lock::try_take(&p.root, WHO) {
        Ok(lock) => {
            let (index, history) = hold(&p, &lock)?;
            (index, Some(Held { link: None, history, lock }))
        }
        Err(LockError::Held(owner)) => {
            println!("{owner} holds the project: serving what its layers hold, and taking it once let go");
            (Index::without_store(&p)?, None)
        }
        Err(e) => return Err(e.into()),
    };
    let inherited = held.as_ref().is_some_and(|h| h.lock.inherited());
    let host = Arc::new(Host {
        served: vfs::Served::of(&index),
        project: p.clone(),
        at: Mutex::new(String::new()),
        note: Mutex::new(None),
        state: Mutex::new(State { index, held, lent_to: None }),
        leases: AtomicUsize::new(0),
        quiet_since: Mutex::new(Instant::now()),
        stopping: AtomicBool::new(false),
        inherited,
        kept: AtomicBool::new(false),
    });
    host.listen_for_edits();
    let _watching = watch_layers(&p, &host)?;
    let stopping = Arc::downgrade(&host);
    ctrlc::set_handler(move || {
        if let Some(host) = stopping.upgrade() {
            host.stopping.store(true, Ordering::Relaxed);
        }
    })?;
    let staging = p.root.join(project::cache::DIR).join(project::cache::STAGING);
    let owner: Arc<dyn vfs::write::Owner> = host.clone();
    if matches!(serving, project::Serving::Fuse | project::Serving::Cloud) {
        clear_strays(&p, strays, owner.as_ref(), &staging)?;
    }
    let (serving, at) = serve(&host.served, owner, &staging, serving, &p)?;
    println!("{} served at {at} ({} entries)", project::host::shown(&p.root), host.served.tree().len());
    *lock(&host.at) = at;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if host.project.filesystem.serving == project::Serving::Fuse || backend == Some("fuse") {
        *lock(&host.note) = vfs::fuse::windows_cannot_reach();
    }
    // Listened to once it serves: who keeps it is told where.
    let _talking = talk(&host)?;
    host.keep_going();
    // Stopping, whyever: what is said to it from now on finds it so.
    host.stopping.store(true, Ordering::Relaxed);
    drop(serving);
    println!("stopped");
    Ok(())
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
fn clear_strays(p: &Project, strays: Strays, owner: &dyn vfs::write::Owner, staging: &Path) -> Result<()> {
    let at = &p.filesystem.at;
    let found = vfs::strays::strays(at);
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

/// How a project is served: as asked, or as this machine says.
fn serving_of(p: &Project, backend: Option<&str>, address: Option<&str>) -> Result<project::Serving> {
    Ok(match (backend, p.filesystem.serving) {
        (Some(name), _) => project::Serving::named(name, address)?,
        (None, project::Serving::WebDav(_)) if address.is_some() => project::Serving::named("webdav", address)?,
        (None, project::Serving::None) => project::Serving::default_here(),
        (None, serving) => serving,
    })
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
    /// Answer edits sent on the editor's link while it holds the project; the rest is declined, for whoever asked to
    /// take the project for it (borrowed from here).
    fn listen_for_edits(self: &Arc<Self>) {
        let me: Weak<Host> = Arc::downgrade(self);
        let answer = move |request: mcp::Request| match (request, me.upgrade()) {
            (mcp::Request::Edit { op, layer }, Some(host)) => host.edit(&op, layer.as_deref()),
            _ => mcp::Reply::Declined,
        };
        let mut state = lock(&self.state);
        if let Some(held) = &mut state.held {
            match mcp::link::Listener::start(&self.project.root, answer) {
                Ok(link) => held.link = Some(link),
                Err(e) => eprintln!("edits cannot be sent here: {e}"),
            }
        }
    }

    /// Make an edit while holding the project: a step of its history, taken in at once.
    fn edit(&self, op: &vfs::ops::Op, layer: Option<&str>) -> mcp::Reply {
        let mut state = lock(&self.state);
        let State { index, held, .. } = &mut *state;
        let Some(held) = held else { return mcp::Reply::failed("the host has lent the project".into()) };
        let made = (|| {
            let layer = match layer {
                Some(layer) => layer.to_string(),
                None => vfs::write::writable_layer(index).context("no layer of the project can be written")?,
            };
            let (label, touched) = vfs::ops::apply(op, &self.project, index, &layer, &mut held.history)?;
            let mut changed = touched.clone();
            changed.extend(held.history.take_written());
            index.refresh(&self.project, &changed)?;
            self.served.update(index);
            anyhow::Ok((label, touched))
        })();
        match made {
            Ok((label, touched)) => {
                println!("{label}");
                mcp::Reply::Edited { label, touched }
            }
            Err(e) => mcp::Reply::failed(format!("{e:#}")),
        }
    }

    /// Take in what changed in the layers: the index refreshed, the tree served made of it.
    fn take_in(&self, changed: &[PathBuf]) {
        let mut state = lock(&self.state);
        match state.index.refresh(&self.project, changed) {
            Ok(_) => self.served.update(&state.index),
            Err(e) => eprintln!("taking in what changed: {e:#}"),
        }
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
        state.index.let_go_of_store();
        drop(lock);
        state.lent_to = Some(who.to_string());
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
                state.index.keep_records(&cache)?;
                anyhow::Ok(history)
            })();
            match held {
                Ok(history) => state.held = Some(Held { link: None, history, lock: taken }),
                Err(e) => return eprintln!("taking the project back: {e:#}"),
            }
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
            match self.served.open_files() {
                0 => {
                    println!("nobody has kept it for {}s, and no file is open in it", GRACE.as_secs());
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
        let request = mcp::Request::Edit { op, layer: None };
        match mcp::link::forward(&self.project.root, &request) {
            Some(mcp::Reply::Edited { label, touched }) => {
                self.take_in(&touched);
                println!("{label} (by whoever holds the project)");
                Ok(())
            }
            Some(reply) => Err(reply.told().err().unwrap_or_else(|| "it answered with something else".into())),
            None => {
                let holder = project::lock::holder(&self.project.root).unwrap_or_else(|| "another process".into());
                Err(format!("the project is in use by {holder}, which takes no edits: write it again once it is done"))
            }
        }
    }
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
        // Woken, it finds the host stopping (or gone) and lets go of the name.
        if let Ok(name) = project::host::name(&self.root) {
            let _ = interprocess::local_socket::Stream::connect(name);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// One process talking to the host: a lease while it keeps it or borrows the project, ended with the connection.
fn connection(host: &Arc<Host>, stream: interprocess::local_socket::Stream) {
    let mut stream = BufReader::new(stream);
    let (mut leased, mut borrowing) = (false, false);
    let mut line = String::new();
    while matches!(stream.read_line(&mut line), Ok(n) if n > 0) {
        let answer = match serde_json::from_str::<ToHost>(&line) {
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
            Ok(ToHost::Status) => host.status(),
            Ok(ToHost::Stop) => {
                host.stopping.store(true, Ordering::Relaxed);
                FromHost::Stopping
            }
            Err(e) => FromHost::Error { why: format!("not something said to the host: {e}") },
        };
        let Ok(text) = serde_json::to_string(&answer) else { break };
        if stream.get_mut().write_all(format!("{text}\n").as_bytes()).is_err() {
            break;
        }
        line.clear();
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
#[expect(dead_code, reason = "held for its drop")]
enum Serving {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    Mounted(vfs::fuse::Mounted),
    Dav(vfs::dav::DavServer),
    #[cfg(windows)]
    Cloud(Box<vfs::cloud::Cloud>),
}

/// Serve the tree by a backend, what programs write applied by `owner`: what serves it, and where.
fn serve(
    served: &vfs::Served,
    owner: Arc<dyn vfs::write::Owner>,
    staging: &Path,
    serving: project::Serving,
    p: &Project,
) -> Result<(Serving, String)> {
    let at = &p.filesystem.at;
    match serving {
        project::Serving::WebDav(addr) => {
            let writes = vfs::writes::Writes::new(served, owner, staging)?;
            let dav = vfs::dav::serve(served, Some(writes), addr).with_context(|| format!("serving at {addr}"))?;
            let at = format!("http://{}/", dav.addr);
            Ok((Serving::Dav(dav), at))
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        project::Serving::Fuse => {
            std::fs::create_dir_all(at).with_context(|| at.display().to_string())?;
            let writes = vfs::writes::Writes::new(served, owner, staging)?;
            let mounted = vfs::fuse::mount(served, Some(writes), at, &served_name(p))
                .with_context(|| format!("mounting at {}", at.display()))?;
            Ok((Serving::Mounted(mounted), project::host::shown(at)))
        }
        #[cfg(windows)]
        project::Serving::Cloud => {
            let cloud = vfs::cloud::serve(served, Some(owner), at, staging, &served_name(p))
                .with_context(|| format!("serving at {}", at.display()))?;
            Ok((Serving::Cloud(Box::new(cloud)), project::host::shown(at)))
        }
        _ => bail!("{serving:?} cannot serve here: FUSE on Linux and macOS, `cloud` on Windows, or `webdav`"),
    }
}

/// The name a project is served by: its folder's.
pub fn served_name(p: &Project) -> String {
    format!("pe2:{}", p.root.file_name().and_then(|n| n.to_str()).unwrap_or("project"))
}

/// Watch a project's layers - each layer's and mount's folder, and the folders of its changesets and `project.toml` -
/// for what changes by other means than the filesystem served, taken in a batch at a time (`Host::take_in`). Not the
/// project's: its cache, what its builds write, its unmanaged mounts (the discs), the temporary files of atomic writes.
/// Watching ends with what this returns.
fn watch_layers(p: &Project, host: &Arc<Host>) -> Result<notify::RecommendedWatcher> {
    use notify::Watcher as _;
    let mut ignore = vec![p.root.join(project::cache::DIR), p.root.join("build"), p.filesystem.at.clone()];
    ignore.extend(p.unmanaged.iter().map(|m| m.path.clone()));
    let (seen, changes) = std::sync::mpsc::channel::<PathBuf>();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        // What is read is no change: the filesystem served reads the layers all the time.
        let Ok(event) = event.map_err(drop) else { return };
        if matches!(event.kind, notify::EventKind::Access(_)) {
            return;
        }
        for path in event.paths {
            let _ = seen.send(path);
        }
    })?;
    for layer in &p.layers {
        if layer.path.is_dir() {
            watcher.watch(&layer.path, notify::RecursiveMode::Recursive)?;
        }
    }
    let mut folders: Vec<PathBuf> =
        p.layers.iter().filter_map(|l| l.changeset.parent().map(Path::to_path_buf)).collect();
    folders.push(p.root.clone());
    folders.sort();
    folders.dedup();
    for folder in folders {
        if folder.is_dir() {
            watcher.watch(&folder, notify::RecursiveMode::NonRecursive)?;
        }
    }
    let host = Arc::downgrade(host);
    std::thread::Builder::new().name("watch".into()).spawn(move || {
        let atomic = |path: &Path| {
            path.extension().is_some_and(|e| e == "tmp")
                && path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('.'))
        };
        // A batch: what arrives until a moment passes with nothing more.
        while let Ok(first) = changes.recv() {
            let mut batch = vec![first];
            while let Ok(more) = changes.recv_timeout(Duration::from_millis(300)) {
                batch.push(more);
            }
            batch.retain(|path| !atomic(path) && !ignore.iter().any(|i| path.starts_with(i)));
            batch.sort();
            batch.dedup();
            let Some(host) = host.upgrade() else { break };
            if !batch.is_empty() {
                host.take_in(&batch);
            }
        }
    })?;
    Ok(watcher)
}
