//! The project served as a filesystem while something holds it, at its `assets` folder, by the backend this machine
//! says (`project.local.toml`, `[filesystem] serve`) - by the server (`pe2-cli project server`, `project::server`),
//! which the holder starts where none runs, keeps while it has the project and feeds: the holder has the project's
//! only index, and the server serves what the holder lists of it (`Index::served`), written after each refresh as a
//! table both map into memory (`project::served`) and named on the server's lease - by a thread of its own, so that
//! the holder never waits on the server. It serves from the first listing on. What programs write through the folder reaches the holder on its
//! link as edits (`mcp::Request::Edit`, `Edits`): the editor applies them between frames as steps of the session's
//! log (`Workspace::apply_edit`), a host as steps of its history; the program waits for it, as an agent waits for its
//! request.
//!
//! The server does not serve over files in the folder that are not the project's (`vfs::strays`): the user chooses to
//! set them aside or take them in, and the server is started again to.

use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};

use project::server::{FromServer, Lease, Listing, ToServer};

/// The server kept serving the project and fed what is served, or why it is not.
pub struct Files {
    project: project::Project,
    /// What a server started for it is started with, besides what is done with strays.
    args: Vec<String>,
    state: Arc<Mutex<Kept>>,
    /// What is served, the newest, for the thread sending it.
    shown: mpsc::Sender<Shown>,
}

/// What the thread sending what is served is given: what is served now, the files of the layers that changed since
/// what it was given before, and who waits for it to be served (told by this being dropped).
struct Shown {
    said: Said,
    changed: Vec<std::path::PathBuf>,
    served: Option<mpsc::Sender<()>>,
}

/// What is served now, as whoever holds the project says it.
enum Said {
    /// All of it.
    Whole(Arc<Listing>),
    /// What of it differs from what was said before.
    Changed(project::server::Delta),
}

/// How long who made a change waits for the server to serve it.
const PATIENCE: std::time::Duration = std::time::Duration::from_secs(5);

enum Kept {
    Starting,
    /// The lease, which the thread sending what is served writes on; where it serves and what it says of how, once it
    /// took what is served.
    Leased(Arc<Mutex<Lease>>, Option<(String, Option<String>)>),
    /// Why not, and how many files in the folder are not the project's (none where that is not why).
    Failed(String, usize),
}

/// How serving goes.
pub enum State {
    Starting,
    /// Where, and what the server says of how it serves.
    Served(String, Option<String>),
    Failed {
        why: String,
        strays: usize,
    },
}

/// What is done with the files in the folder that are not the project's.
#[derive(Clone, Copy)]
pub enum Strays {
    SetAside,
    TakeIn,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Files {
    /// Keep the server serving the project, started where none runs, off the caller's thread, and feed it: none where
    /// this machine serves nothing and no server serves it already. After the project is taken: what is served is
    /// the holder's to list.
    #[must_use]
    pub fn keep(project: &project::Project) -> Option<Files> {
        let machine = extensions::filesystem_defaults().serve;
        let says = vfs::backend::setting(&[project.filesystem.serve.as_deref(), machine.as_deref()]);
        if says == vfs::backend::Setting::None && !project::server::running(&project.root) {
            return None;
        }
        Some(Files::feed(project, Vec::new()))
    }

    /// Keep the server serving the project - one started with `args` (after `project server`) where none runs - off
    /// the caller's thread, and feed it what is shown from now on.
    #[must_use]
    pub fn feed(project: &project::Project, args: Vec<String>) -> Files {
        let state = Arc::new(Mutex::new(Kept::Starting));
        let (kept_project, kept_state, kept_args) = (project.clone(), state.clone(), args.clone());
        std::thread::spawn(move || {
            let kept = kept(&kept_project, &kept_args);
            *lock(&kept_state) = kept;
        });
        let (shown, newest) = mpsc::channel();
        let (sending, sent_of) = (state.clone(), project.clone());
        let send = move || send_shown(&sent_of, &sending, &newest);
        let _ = std::thread::Builder::new().name("served".into()).spawn(send);
        Files { project: project.clone(), args, state, shown }
    }

    /// What is served now, as the index lists it: sent to the server, after what was sent before.
    pub fn show(&self, listing: Arc<Listing>) {
        let _ = self.shown.send(Shown { said: Said::Whole(listing), changed: Vec::new(), served: None });
    }

    /// What is served now, after these files of the layers changed: the server told of them first - what it reads of
    /// them is read again before what is listed is served - and, where it serves already, waited for to serve it, so
    /// that who made the change finds it in the folder.
    pub fn show_changed(&self, changed: Vec<std::path::PathBuf>, listing: Arc<Listing>) {
        self.say(Said::Whole(listing), changed);
    }

    /// What differs of what is served from what was shown before, after these files of the layers changed
    /// (`Index::serve_changed`): told and waited for as `show_changed` is, at what the change costs, not what the
    /// project holds.
    pub fn show_delta(&self, changed: Vec<std::path::PathBuf>, delta: project::server::Delta) {
        self.say(Said::Changed(delta), changed);
    }

    fn say(&self, said: Said, changed: Vec<std::path::PathBuf>) {
        let (served, waited) = mpsc::channel();
        let wait = matches!(self.state(), State::Served(..));
        let _ = self.shown.send(Shown { said, changed, served: wait.then_some(served) });
        if wait {
            let _ = waited.recv_timeout(PATIENCE);
        }
    }

    #[must_use]
    pub fn state(&self) -> State {
        match &*lock(&self.state) {
            Kept::Starting | Kept::Leased(_, None) => State::Starting,
            Kept::Leased(_, Some((at, note))) => State::Served(at.clone(), note.clone()),
            Kept::Failed(why, strays) => State::Failed { why: why.clone(), strays: *strays },
        }
    }

    /// Start the server again, doing this with the files that are not the project's: off the caller's thread, which
    /// is woken by `done` once it is kept. What is served is sent to it again, whole.
    pub fn resolve(&self, strays: Strays, done: impl FnOnce() + Send + 'static) {
        *lock(&self.state) = Kept::Starting;
        let (project, state) = (self.project.clone(), self.state.clone());
        let mut args = self.args.clone();
        args.extend(match strays {
            Strays::SetAside => ["--strays".to_string(), "set-aside".to_string()],
            Strays::TakeIn => ["--strays".to_string(), "take-in".to_string()],
        });
        std::thread::spawn(move || {
            let kept = kept(&project, &args);
            *lock(&state) = kept;
            done();
        });
    }
}

/// This program, as it names itself to the server it keeps.
fn who() -> &'static str {
    static WHO: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    WHO.get_or_init(|| {
        let program = std::env::current_exe().ok();
        let name = program.as_deref().and_then(|p| p.file_stem()).map(|n| n.to_string_lossy().into_owned());
        name.unwrap_or_else(|| "a holder".into())
    })
}

/// `pe2-cli`, which the server is: this program where it is that, else the one beside it.
fn cli() -> anyhow::Result<std::path::PathBuf> {
    let program = std::env::current_exe()?;
    let name = program.file_stem().map(|n| n.to_string_lossy().into_owned());
    let cli = if name.as_deref() == Some("pe2-cli") { program } else { project::host::cli_beside(&program) };
    if !cli.is_file() {
        anyhow::bail!("{} is not there: the server serving the files is `pe2-cli`", cli.display());
    }
    Ok(cli)
}

/// The server kept, started with `args` where none runs, or why not.
fn kept(project: &project::Project, args: &[String]) -> Kept {
    // The one running, whatever program this is; one started is `pe2-cli`.
    let kept = match project::server::keep_running(&project.root, who()) {
        Some(lease) => Ok(lease),
        None => cli().and_then(|program| project::server::keep(&project.root, who(), &program, args)),
    };
    match kept {
        Ok(lease) => Kept::Leased(Arc::new(Mutex::new(lease)), None),
        Err(e) => Kept::Failed(format!("{e:#}"), strays_in(project)),
    }
}

/// How many files the project's folder holds that are not the project's, where what serves it serves at one: why it
/// is not served, most likely.
fn strays_in(project: &project::Project) -> usize {
    let machine = extensions::filesystem_defaults().serve;
    let says = vfs::backend::setting(&[project.filesystem.serve.as_deref(), machine.as_deref()]);
    let at = &project.filesystem.at;
    let how = project::content::Use::general();
    let serving = vfs::backend::choose(&says, vfs::backend::Can::WRITTEN, at, &how, false).ok().flatten();
    serving.filter(|b| b.can().folder).map_or(0, |b| vfs::strays::strays(at, b).len())
}

/// Send what is served to the server as it changes: to a lease that has none of it yet, all of it - written as a
/// table the server maps (`project::served`) and named to it; after that, what changed since (`ToServer::Changes`).
/// What the server is not kept for yet waits for it.
fn send_shown(project: &project::Project, state: &Mutex<Kept>, newest: &mpsc::Receiver<Shown>) {
    use project::server::Delta;
    // What is served now, as last said; the lease that has it, but for `unsent`; and what of it that lease has yet
    // to be told - none where no lease has any of it.
    let mut current: Option<Listing> = None;
    let mut has: Option<Arc<Mutex<Lease>>> = None;
    let mut unsent: Option<Delta> = None;
    // The files that changed since what was sent last, and who waits for what is said to be served: let go of -
    // which tells them - once it is, or will not be.
    let (mut changed, mut waiting) = (Vec::new(), Vec::new());
    loop {
        let first = match newest.recv_timeout(std::time::Duration::from_millis(250)) {
            Ok(shown) => Some(shown),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };
        for shown in first.into_iter().chain(newest.try_iter()) {
            let delta = match (shown.said, &mut current) {
                (Said::Whole(listing), Some(before)) => {
                    let delta = Delta::between(before, &listing);
                    *before = Arc::unwrap_or_clone(listing);
                    delta
                }
                (Said::Whole(listing), None) => {
                    current = Some(Arc::unwrap_or_clone(listing));
                    Delta::default()
                }
                (Said::Changed(delta), Some(before)) => {
                    delta.apply(before);
                    delta
                }
                // Part of nothing: whoever holds the project says all of it first.
                (Said::Changed(_), None) => Delta::default(),
            };
            if let Some(unsent) = &mut unsent {
                unsent.then(delta);
            }
            changed.extend(shown.changed);
            waiting.extend(shown.served);
        }
        // Whoever waits is told when this pass ends, however it does.
        let _told = std::mem::take(&mut waiting);
        let lease = match &*lock(state) {
            Kept::Leased(lease, _) => lease.clone(),
            _ => continue,
        };
        let Some(listing) = &current else { continue };
        // A new lease (the server started again) has none of it.
        if !has.as_ref().is_some_and(|has| Arc::ptr_eq(has, &lease)) {
            (has, unsent) = (None, None);
        }
        if unsent.as_ref().is_some_and(Delta::is_empty) && changed.is_empty() {
            continue;
        }
        // What changed in the layers first: what it reads of the files listed is read again before they are served.
        if !changed.is_empty() {
            let _ = lock(&lease).tell(&ToServer::Changed { paths: std::mem::take(&mut changed) });
        }
        let root = &project.root;
        let mut written = None;
        let told = if let Some(delta) = &unsent {
            let shown = delta.shown.iter().map(|(path, it)| (path.clone(), it.clone())).collect();
            let _span = tracing::info_span!("served: server told what changed").entered();
            lock(&lease).tell(&ToServer::Changes { shown, gone: delta.gone.iter().cloned().collect() })
        } else {
            // All of it: the table, written for the server to map, then named.
            let generation = project::served::next_generation(root);
            written = Some(generation);
            match project::served::write(root, generation, listing) {
                Ok(()) => lock(&lease).tell(&ToServer::Table { generation }),
                Err(e) => Err(anyhow::Error::new(e).context("writing the table of what is served")),
            }
        };
        let failed = match told {
            Ok(FromServer::Serving { at, note }) => {
                if let Kept::Leased(now, serving) = &mut *lock(state)
                    && Arc::ptr_eq(now, &lease)
                {
                    *serving = Some((at, note));
                }
                (has, unsent) = (Some(lease), Some(Delta::default()));
                // The tables before the one it has are nobody's any more.
                if let Some(generation) = written {
                    project::served::remove_before(root, generation);
                }
                continue;
            }
            // It does not serve - over what is in the folder that is not the project's, most likely - or is gone.
            Ok(FromServer::Error { why }) if written.is_some() => Some(why),
            // Whoever held the project before feeds it still, its connection yet to end; or what changed was not
            // taken, it having no table of this lease: all of it, in a moment.
            Ok(FromServer::Fed { .. } | FromServer::Error { .. }) => None,
            Ok(other) => Some(format!("the server answered {other:?}")),
            Err(e) => Some(format!("the server serving it stopped ({e:#})")),
        };
        if let Some(why) = failed {
            let mut state = lock(state);
            if matches!(&*state, Kept::Leased(now, _) if Arc::ptr_eq(now, &lease)) {
                *state = Kept::Failed(why, strays_in(project));
            }
        }
        (has, unsent) = (None, None);
    }
}
