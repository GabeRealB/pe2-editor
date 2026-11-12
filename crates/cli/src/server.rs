//! `pe2-cli project server`: the process serving a project as a filesystem (`project::server`), and doing nothing
//! else. It has no index, reads no records and loads no extension: whoever holds the project - an editor, a host -
//! says what is served in the filesystem the project's processes share (`project::shared`) and tells it that it
//! did; it reads there what changed since it last read, and serves each file from where it is said to live. It
//! starts serving the first time it is told; the connection that told it feeds it until it ends. What programs write
//! through what it serves it sends to whoever holds the project as edits, a host started to hold it where nothing
//! does. It is started by whoever wants the folder served - a host, the editor - and stops a while after nobody keeps
//! it, once no file is open in what it serves: what only looks at the folder (a watcher, a file browser) keeps
//! nothing, and no read is cut off.

use anyhow::{Context, Result, bail};
use std::io::BufReader;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use project::Project;
use project::server::{FromServer, ROLE, ToServer};
use vfs::write::Refusal;

/// How long after nobody keeps it it stops (once no file is open): whoever holds the project next finds it serving.
const GRACE: Duration = Duration::from_secs(30);

struct Server {
    project: Project,
    served: vfs::Served,
    /// The space the project's processes share, attached for as long as it serves: what is served is said there
    /// by whoever holds the project (`project::shared`).
    shared: Arc<sfs::Space>,
    /// What it serves by, at which address where it serves at one, and what it does with what is not the project's.
    serving: &'static dyn vfs::backend::Backend,
    address: Option<std::net::SocketAddr>,
    strays: Strays,
    state: Mutex<State>,
    leases: AtomicUsize,
    /// When the last lease ended.
    quiet_since: Mutex<Instant>,
    stopping: Arc<AtomicBool>,
    /// The layers' files it was told changed, each as it was when what is read of it was read again: reported again
    /// as it still is - by its watch, after whoever holds the project told of it - it is read already.
    reread: Mutex<std::collections::HashMap<std::path::PathBuf, Option<Stamp>>>,
}

/// What tells a file's version from another: its time and length; none where it is gone.
type Stamp = (std::time::SystemTime, u64);

#[derive(Default)]
struct State {
    /// What it serves, as it last read it in the shared filesystem, and the commit it read it at - none before
    /// the first.
    listing: project::server::Listing,
    seen: Option<u64>,
    /// The connection feeding it, and whose it is.
    feeder: Option<(u64, String)>,
    /// What it holds while it serves, from the first listing on.
    mounted: Option<Mounted>,
    /// Where it serves, as said to who asks, and what they should know of how.
    at: String,
    note: Option<String>,
}

/// What a server holds while it serves: the mount - dropped, it no longer does - what is read there, and what keeps
/// what the backend keeps of it within its budget.
struct Mounted {
    mount: Box<dyn vfs::backend::Mount>,
    record: Arc<vfs::access::Access>,
    collecting: Option<std::thread::JoinHandle<()>>,
}

/// `pe2-cli project server`.
pub fn run(directory: &Path, backend: Option<&str>, address: Option<&str>, strays: Strays) -> Result<()> {
    // It serves files, and runs nothing of a project's: no extension is loaded in this process, whatever comes.
    project::ext::forbid();
    let p = Project::load(directory)?;
    let (serving, address) = serving_of(&p, backend, address)?;
    let _one = ROLE.claim(&p.root)?;
    let shared = project::shared::space(&p.root).context("the space the project's processes share")?;
    // What a serving before this one said due it says again itself - from the log, or as it finds the folder.
    vfs::due::clear(&shared).context("what is due, where the project's processes share it")?;
    // What is read of its files is kept in a ring of pages, in memory every process of the project can name.
    let pages = {
        let machine = extensions::filesystem_defaults();
        vfs::backend::budget(&[p.filesystem.pages.as_deref(), machine.pages.as_deref()], vfs::backend::PAGES)
    };
    let ring = (pages > 0).then(|| project::shared::ring(&p.root, &shared, pages)).transpose().context("the pages kept of what is read")?;
    let ring = ring.map(Arc::new);
    // A record whoever holds the project changed and has yet to write is read as that leaves it.
    project::storage::read_through(&p.root, shared.clone());
    let server = Arc::new(Server {
        project: p.clone(),
        // The tree it serves is the shared filesystem itself, read as it is asked: nothing of it is kept here.
        served: vfs::Served::shared(shared.clone(), ring.clone()),
        shared,
        serving,
        address,
        strays,
        state: Mutex::default(),
        leases: AtomicUsize::new(0),
        quiet_since: Mutex::new(Instant::now()),
        stopping: Arc::new(AtomicBool::new(false)),
        reread: Mutex::default(),
    });
    let watched = Arc::downgrade(&server);
    let _watching = holder::watch::layers(&p, move |batch| {
        let Some(server) = watched.upgrade() else { return false };
        server.reread(batch);
        true
    })?;
    let stopping = server.stopping.clone();
    ctrlc::set_handler(move || stopping.store(true, Ordering::Relaxed))?;
    let (talking, ids) = (Arc::downgrade(&server), AtomicU64::new(0));
    let _talking = ROLE.listen(&p.root, server.stopping.clone(), move |stream| {
        if let Some(server) = talking.upgrade() {
            connection(&server, ids.fetch_add(1, Ordering::Relaxed), stream);
        }
    })?;
    println!("{} is served by `{}` once it is listed", project::host::shown(&p.root), serving.name());
    server.keep_going();
    // Stopping, whyever: what is said to it from now on finds it so.
    server.stopping.store(true, Ordering::Relaxed);
    let mounted = lock(&server.state).mounted.take();
    if let Some(Mounted { mount, record, collecting }) = mounted {
        if let Some(collecting) = collecting {
            let _ = collecting.join();
        }
        record.save();
        drop(mount);
    }
    // The memory the pages are in is let go of here: a process that ends runs nothing more of its own.
    if let Some(ring) = &ring {
        ring.let_go();
    }
    // And the space the project's processes share is left as whoever next attaches alone may trust it: what was
    // written of it here on disk, and all of it where nothing else has it attached.
    if let Err(e) = server.shared.rest() {
        eprintln!("the shared space is not left to be trusted: {e}");
    }
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
    /// The arguments a server, or a host that starts one, is started with to do this.
    pub fn args(self) -> Vec<String> {
        match self {
            Strays::Refuse => vec![],
            Strays::SetAside => vec!["--strays".into(), "set-aside".into()],
            Strays::TakeIn => vec!["--strays".into(), "take-in".into()],
        }
    }
}

/// A count of files, as said.
pub fn files(count: usize) -> String {
    if count == 1 { "1 file".into() } else { format!("{count} files") }
}

/// The name a project is served by: its folder's.
pub fn served_name(p: &Project) -> String {
    format!("pe2:{}", p.root.file_name().and_then(|n| n.to_str()).unwrap_or("project"))
}

/// What serves a project's folder, written through, and the address it does at where it serves at one: as asked, then
/// as the project says (`project.local.toml`), then the machine - else the backend best suited here. A project or
/// machine saying `none` is only not served by itself: asked to be, it is, by the best suited.
fn serving_of(
    p: &Project,
    asked: Option<&str>,
    address: Option<&str>,
) -> Result<(&'static dyn vfs::backend::Backend, Option<std::net::SocketAddr>)> {
    use vfs::backend::{Can, choose, setting, why_none};
    let machine = extensions::filesystem_defaults();
    let said = |says: &Option<String>| says.clone().filter(|says| says != "none");
    let (project, machine) = (said(&p.filesystem.serve), said(&machine.serve));
    let setting = setting(&[asked, project.as_deref(), machine.as_deref()]);
    let at = &p.filesystem.at;
    // A folder programs work in: any of it, any number of times.
    let how = project::content::Use::general();
    let Some(backend) = choose(&setting, Can::WRITTEN, at, &how, false).map_err(anyhow::Error::msg)? else {
        bail!("nothing serves {} here ({})", project::host::shown(&p.root), why_none(Can::WRITTEN, at));
    };
    let address = address.or(p.filesystem.address.as_deref());
    let address = address.map(|a| a.parse().with_context(|| format!("`{a}`: no address"))).transpose()?;
    Ok((backend, address))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Server {
    /// Read again what these files of the layers are to what is read of the project, where they changed since it
    /// last did: its packs' listings. What its records hold it reads by what is said to be served, not by them.
    fn reread(&self, changed: &[std::path::PathBuf]) {
        let stamp = |path: &Path| {
            let meta = std::fs::metadata(path).ok()?;
            Some((meta.modified().ok()?, meta.len()))
        };
        let changed: Vec<std::path::PathBuf> = {
            let mut read = lock(&self.reread);
            if read.len() > 10_000 {
                read.clear();
            }
            let other = |path: &&std::path::PathBuf| {
                let now = stamp(path);
                read.insert((*path).clone(), now) != Some(now)
            };
            changed.iter().filter(other).cloned().collect()
        };
        project::pack::reread_changed(&changed);
    }

    /// Take in what whoever holds the project said is served, in the shared filesystem - which is the tree it
    /// serves, so nothing of that is changed here: what the commits since it last read did (the first time, and
    /// where those are no longer kept, all of it against what it had) is what its files are read by, and what those
    /// that tell of the tree - the kernel, what watches a mount - are told.
    fn follow(&self, state: &mut State) -> Result<()> {
        use project::server::Delta;
        let read = self.shared.read()?;
        let generation = read.commit().generation;
        if state.seen == Some(generation) {
            return Ok(());
        }
        let by_change = match state.seen {
            Some(seen) => project::shared::changed(&read, seen, &state.listing)?,
            None => None,
        };
        let before = std::mem::take(&mut state.listing);
        let delta = if let Some(delta) = by_change {
            state.listing = before.clone();
            delta.apply_under(&mut state.listing);
            delta
        } else {
            state.listing = project::shared::listed(&read)?;
            Delta::between(&before, &state.listing)
        };
        drop(read);
        if state.seen.replace(generation).is_none() {
            // The first: all its files are read by this, and nobody was shown anything yet.
            project::content::serve_whole(&state.listing);
            return Ok(());
        }
        if !delta.is_empty() {
            let changes = vfs::Changes::of(&before, &delta, &state.listing);
            let shown: Vec<_> = delta.shown.into_iter().collect();
            project::content::serve_changed(&shown);
            self.served.told(changes);
        }
        Ok(())
    }

    /// Whoever holds the project says, on the connection `from`, that it said what is served: read, and served -
    /// from the first time on. Not taken from a second connection while one feeds it.
    fn show(self: &Arc<Self>, (from, who): (u64, &str)) -> FromServer {
        let mut state = lock(&self.state);
        if let Some((feeds, by)) = &state.feeder
            && *feeds != from
        {
            return FromServer::Fed { by: by.clone() };
        }
        if self.stopping.load(Ordering::Relaxed) && state.mounted.is_some() {
            return FromServer::Stopping;
        }
        if let Err(e) = self.follow(&mut state) {
            // What it cannot read it does not serve: said to who asked, and to its log.
            println!("what is served could not be read in the shared filesystem: {e:#}");
            return FromServer::Error { why: format!("what is served could not be read in the shared filesystem: {e:#}") };
        }
        state.feeder = Some((from, who.to_string()));
        if state.mounted.is_none() {
            // Stopping, it serves no more: what it is listed now - what it gave the project as it ended, taken in -
            // is not mounted again.
            if self.stopping.load(Ordering::Relaxed) {
                return FromServer::Stopping;
            }
            match self.mount() {
                Ok((mounted, at, note)) => {
                    state.mounted = Some(mounted);
                    (state.at, state.note) = (at, note);
                }
                Err(e) => {
                    // It serves this folder or nothing: said to who asked, and to its log, as it ends.
                    println!("{e:#}");
                    self.stopping.store(true, Ordering::Relaxed);
                    return FromServer::Error { why: format!("{e:#}") };
                }
            }
        }
        FromServer::Serving { at: state.at.clone(), note: state.note.clone() }
    }

    /// Serve the project's folder: what others left there retired, what is not the project's dealt with as asked,
    /// then mounted. What it holds while it serves, where that is, and what is to be known of how.
    fn mount(self: &Arc<Self>) -> Result<(Mounted, String, Option<String>)> {
        let (p, serving) = (&self.project, self.serving);
        let staging = p.root.join(project::cache::DIR).join(project::cache::STAGING);
        let owner: Arc<dyn vfs::write::Owner> = self.clone();
        if serving.can().folder {
            // What another backend left there of its own serving is no stray: it goes first.
            vfs::backend::hand_over(serving, &p.filesystem.at, &served_name(p));
            clear_strays(p, serving, self.strays, owner.as_ref(), &staging)?;
        }
        let job = vfs::backend::Job {
            served: &self.served,
            owner: Some(owner),
            at: &p.filesystem.at,
            staging: &staging,
            name: &served_name(p),
            address: self.address,
        };
        let mount = serving
            .mount(&job)
            .with_context(|| format!("serving {} by `{}`", project::host::shown(&p.root), serving.name()))?;
        let at = mount.shown();
        println!(
            "{} served at {at} by `{}` ({} entries)",
            project::host::shown(&p.root),
            serving.name(),
            self.served.tree().len()
        );
        let record = self.remembered();
        let collecting = serving.keeps().then(|| {
            let (server, record) = (self.clone(), record.clone());
            std::thread::Builder::new().name("collect".into()).spawn(move || server.keep_within(&record)).ok()
        });
        let note = mount.note();
        Ok((Mounted { mount, record, collecting: collecting.flatten() }, at, note))
    }

    /// What is read of the folder served, remembered from one serving to the next: what a backend that keeps what is
    /// read lets go of last.
    fn remembered(&self) -> Arc<vfs::access::Access> {
        let record = Arc::new(vfs::access::Access::of(&self.project.root, "the project's folder"));
        if !self.serving.keeps() {
            // Whatever kept what was read before has handed the folder over, and with it let go of what it kept.
            record.nothing_kept();
        }
        let noted = Arc::clone(&record);
        self.served.watch(move |path, seen| match seen {
            vfs::Seen::Read => noted.read(path),
            vfs::Seen::Kept(bytes) => noted.kept(path, bytes),
            vfs::Seen::Dropped => noted.dropped(path),
        });
        record
    }

    /// For a backend that keeps what is read, while it serves: what it keeps of the folder brought within its budget
    /// (`[filesystem] kept`) - now, every ten minutes, and as serving ends.
    fn keep_within(&self, record: &vfs::access::Access) {
        let machine = extensions::filesystem_defaults();
        let says = &self.project.filesystem;
        let kept = vfs::backend::budget(&[says.kept.as_deref(), machine.kept.as_deref()], vfs::backend::KEPT);
        self.serving.collect(&says.at, record, kept);
        let mut collected = Instant::now();
        while !self.stopping.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(500));
            if collected.elapsed() >= COLLECTING {
                self.serving.collect(&says.at, record, kept);
                record.save();
                collected = Instant::now();
            }
        }
        self.serving.collect(&says.at, record, kept);
    }

    /// Serve until stopped, or until nobody keeps it and no file was open in it for a while.
    fn keep_going(&self) {
        let mut waiting = false;
        while !self.stopping.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(200));
            if self.leases.load(Ordering::Relaxed) > 0 {
                waiting = false;
                continue;
            }
            if lock(&self.quiet_since).elapsed() < GRACE {
                continue;
            }
            match self.served.open_files() {
                0 => {
                    println!("nobody keeps it, and no file is open in it");
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
            self.leases.fetch_add(1, Ordering::Relaxed);
        } else {
            self.leases.fetch_sub(1, Ordering::Relaxed);
        }
        *lock(&self.quiet_since) = Instant::now();
    }

    /// A connection ended: where it fed the server, nothing does until another lists what is served.
    fn left(&self, connection: u64) {
        let mut state = lock(&self.state);
        if state.feeder.take_if(|(feeds, _)| *feeds == connection).is_some() {
            println!("no longer fed: serving what was listed last");
        }
    }

    fn status(&self) -> FromServer {
        let state = lock(&self.state);
        FromServer::Status {
            pid: std::process::id(),
            at: state.at.clone(),
            note: state.note.clone(),
            fed_by: state.feeder.as_ref().map(|(_, by)| by.clone()),
            leases: self.leases.load(Ordering::Relaxed),
            open: self.served.open_files(),
        }
    }

    /// Ask something of whoever holds the project - an editor, a host - waiting for the answer for as long as the
    /// link lives; where nothing holds it, of a host started to.
    fn ask(&self, request: &mcp::Request) -> Asked {
        let root = &self.project.root;
        if let Some(reply) = mcp::link::forward_patiently(root, request) {
            return Asked::Answered(reply);
        }
        if let Err(later) = self.ready() {
            return Asked::Later(later);
        }
        match mcp::server::ask_holder(root, request) {
            Some(reply) => Asked::Answered(reply),
            // Taken meanwhile by what answers nothing, or no host could be started to hold it.
            None => match self.ready() {
                Err(later) => Asked::Later(later),
                Ok(()) => Asked::Nobody,
            },
        }
    }

    /// Whether whoever holds the project takes edits: one that listens on the holder's link does, and where nothing
    /// holds the project a host is started to. Not what holds it and answers nothing - an extraction, a tool, an
    /// editor or a host still opening: no host started beside it would hold the project either.
    fn ready(&self) -> std::result::Result<(), Refusal> {
        let root = &self.project.root;
        if mcp::link::listens(root) {
            return Ok(());
        }
        match project::lock::holder(root) {
            Some(holder) => Err(Refusal::Later(format!(
                "the project is in use by {holder}, which takes no edits now: it is taken in once it does, or is done"
            ))),
            None => Ok(()),
        }
    }
}

/// What came of asking whoever holds the project.
enum Asked {
    Answered(mcp::Reply),
    /// What holds the project takes no edits now: why.
    Later(Refusal),
    /// Nothing holds it, and no host could be started to.
    Nobody,
}

/// Why an edit was not made, where nothing holds the project to make it: not now - a host that could not be
/// started this moment, as where the one before it is still letting go, is started the next time it is asked.
fn nobody() -> Refusal {
    Refusal::Later("nothing holds the project, and no host could be started to this moment".into())
}

/// How often what a backend keeps of the folder is brought within its budget.
const COLLECTING: Duration = Duration::from_mins(10);

/// How many times whoever holds the project is asked to look at what is due before what it left there is taken
/// back, to be said again later.
const ASKED_TO_LOOK: usize = 3;

impl vfs::write::Owner for Server {
    /// What a program did: said due, as `changes`.
    fn change(&self, change: vfs::write::Change) -> std::result::Result<(), Refusal> {
        self.changes(vec![change]).pop().unwrap_or(Ok(()))
    }

    /// What programs did: said due where the project's processes share it (`vfs::due`), in this order, and
    /// whoever holds the project asked to look - a host started where nothing holds it. It takes them from there,
    /// makes each an edit, and says what became of each in the commit that says what is served after them: read
    /// here once it answers. What nobody took - nothing holds the project that takes edits now - is taken back,
    /// and said again later.
    fn changes(&self, changes: Vec<vfs::write::Change>) -> Vec<std::result::Result<(), Refusal>> {
        let ops: Vec<vfs::ops::Op> = changes.into_iter().map(vfs::ops::Op::from).collect();
        // Nothing to ask: a holder is not started for it.
        if ops.is_empty() {
            return Vec::new();
        }
        let numbers = match vfs::due::put(&self.shared, &ops) {
            Ok(numbers) => numbers,
            Err(e) => {
                let why = Refusal::Later(format!("it could not be said due: {e}"));
                return ops.iter().map(|_| Err(why.clone())).collect();
            }
        };
        let mut became: Vec<Option<vfs::due::Answer>> = vec![None; numbers.len()];
        let mut why = Refusal::Later("whoever holds the project did not say what became of it".into());
        for asked in 1..=ASKED_TO_LOOK {
            let nobody_took = match self.ask(&mcp::Request::Due) {
                Asked::Answered(mcp::Reply::Taken { .. }) => None,
                // It holds the project and takes nothing now - a host that lent it, an editor still opening.
                Asked::Answered(reply) => Some(Refusal::Later(match reply.told() {
                    Err(why) => why,
                    Ok(said) => said,
                })),
                Asked::Later(later) => Some(later),
                Asked::Nobody => Some(nobody()),
            };
            // What is answered by now; what is not, where nobody took it or nobody will be asked again, is taken
            // back - one taken this moment after all is answered to nobody, and so already when it is said again.
            let last = nobody_took.is_some() || asked == ASKED_TO_LOOK;
            let open: Vec<usize> = (0..numbers.len()).filter(|at| became[*at].is_none()).collect();
            let asked_of: Vec<u64> = open.iter().map(|at| numbers[*at]).collect();
            match vfs::due::settle(&self.shared, &asked_of, last) {
                Ok(answers) => {
                    for (at, answer) in open.into_iter().zip(answers) {
                        became[at] = answer;
                    }
                }
                Err(e) => why = Refusal::Later(format!("what became of it could not be read: {e}")),
            }
            if let Some(nobody_took) = nobody_took {
                why = nobody_took;
            }
            if last || became.iter().all(Option::is_some) {
                break;
            }
            // Taken, and what became of it not said yet: a moment for it to be.
            std::thread::sleep(Duration::from_millis(50));
        }
        let told = |answer: Option<vfs::due::Answer>| match answer {
            Some(Ok(label)) => {
                println!("{label}");
                Ok(())
            }
            Some(Err(refused)) => Err(Refusal::Never(refused)),
            None => Err(why.clone()),
        };
        became.into_iter().map(told).collect()
    }

    /// Ask whoever holds the project - a host started where nothing does - to look at what is due, and wait for
    /// it to have said what became of what it took.
    fn look(&self) -> std::result::Result<(), Refusal> {
        match self.ask(&mcp::Request::Due) {
            Asked::Answered(mcp::Reply::Taken { .. }) => Ok(()),
            // It holds the project and takes nothing now - a host that lent it, an editor still opening.
            Asked::Answered(reply) => Err(Refusal::Later(reply.told().unwrap_or_else(|why| why))),
            Asked::Later(later) => Err(later),
            Asked::Nobody => Err(nobody()),
        }
    }

    fn ready(&self) -> std::result::Result<(), Refusal> {
        Server::ready(self)
    }

    /// What was written and nobody took by the time serving ends: set aside beside the folder, by its path there,
    /// as what is not the project's is - the staging is emptied when the folder is next served.
    fn left(&self, path: &str, staged: &Path) {
        let seconds = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let aside = self.project.root.join(project::SET_ASIDE).join(seconds.to_string()).join(path);
        let kept = aside.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::copy(staged, &aside));
        match kept {
            Ok(_) => println!("{path}: not taken into the project, and set aside in {}", project::host::shown(&aside)),
            Err(e) => eprintln!("{path}: not taken into the project, and lost: {e}"),
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

/// One process talking to the server: a lease once it keeps it, ended with the connection; its feeder once it lists
/// what is served, until the connection ends.
fn connection(server: &Arc<Server>, id: u64, stream: interprocess::local_socket::Stream) {
    let mut stream = BufReader::new(stream);
    let (mut leased, mut who) = (false, "another process".to_string());
    loop {
        let said = match project::wire::receive::<ToServer>(&mut stream) {
            Ok(Some(said)) => Ok(said),
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => Err(e),
            Ok(None) | Err(_) => break,
        };
        let answer = match said {
            Ok(ToServer::Keep { who: keeps }) => {
                println!("kept by {keeps}");
                who = keeps;
                if !std::mem::replace(&mut leased, true) {
                    server.leased(true);
                }
                FromServer::Kept
            }
            Ok(ToServer::Shown) => server.show((id, &who)),
            Ok(ToServer::Changed { paths }) => {
                server.reread(&paths);
                FromServer::Done
            }
            Ok(ToServer::Status) => server.status(),
            Ok(ToServer::Stop) => {
                server.stopping.store(true, Ordering::Relaxed);
                FromServer::Stopping
            }
            Err(e) => FromServer::Error { why: format!("not something said to the server: {e}") },
        };
        if project::wire::send(stream.get_mut(), &answer).is_err() {
            break;
        }
    }
    server.left(id);
    if leased {
        server.leased(false);
    }
}
