//! The project served as a filesystem while the editor has it open, at its `assets` folder, by the backend this machine
//! says (`project.local.toml`, `[filesystem] serve`) - by the host (`pe2-cli project host`, `project::host`), which the
//! editor starts where none runs and keeps while it has the project, and which serves on after it, while anything uses
//! what it serves. Opening the project borrows it from the host (`Lock::try_acquire`), so what programs write through
//! the folder reaches the editor on its link as edits (`mcp::Request::Edit`), applied between frames as steps of the
//! session's log (`Workspace::apply_edit`); the program waits for it, as an agent waits for its request.
//!
//! The editor holds the project's only index: the host keeps none while it does, and serves what the editor lists of it
//! (`Index::served`), sent on the host's lease after each refresh - whole at first, then what changed - by a thread of
//! its own, so that the window never waits on the host.
//!
//! The host does not serve over files in the folder that are not the project's (`vfs::strays`): the user chooses to set
//! them aside or take them in, and the host is started again to.

use eframe::egui;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};

use project::host::{Listing, ToHost};

/// The host kept serving the project, or why it is not.
pub struct Files {
    project: project::Project,
    state: Arc<Mutex<Kept>>,
    /// What is served, the newest, for the thread sending it.
    shown: mpsc::Sender<Arc<Listing>>,
}

enum Kept {
    Starting,
    /// Where it serves and what it says of how; the lease, which the thread sending what is served writes on.
    Served(String, Option<String>, Arc<Mutex<project::host::Lease>>),
    /// Why not, and how many files in the folder are not the project's (none where that is not why).
    Failed(String, usize),
}

impl From<project::host::Lease> for Kept {
    fn from(lease: project::host::Lease) -> Kept {
        Kept::Served(lease.at.clone(), lease.note.clone(), Arc::new(Mutex::new(lease)))
    }
}

/// How serving goes.
pub enum State {
    Starting,
    /// Where, and what the host says of how it serves.
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

impl Files {
    /// Keep the host serving the project, started where none runs, off the window's thread: none where this machine
    /// serves nothing and no host serves it already. After the project is taken: the host starts beside the editor's
    /// own opening rather than before it, reading the project without its store (as cheap as with it), and takes it
    /// once the editor lets go of it.
    #[must_use]
    pub fn keep(project: &project::Project) -> Option<Files> {
        let root = &project.root;
        if project.filesystem.serving == project::Serving::None && project::host::Connection::connect(root).is_none() {
            return None;
        }
        let state = Arc::new(Mutex::new(Kept::Starting));
        let (kept_project, kept_state) = (project.clone(), state.clone());
        std::thread::spawn(move || {
            let kept = kept(&kept_project, None);
            *kept_state.lock().unwrap_or_else(PoisonError::into_inner) = kept;
        });
        let (shown, newest) = mpsc::channel();
        let sending = state.clone();
        let _ = std::thread::Builder::new().name("served".into()).spawn(move || send_shown(&sending, &newest));
        Some(Files { project: project.clone(), state, shown })
    }

    /// What is served now, as the index lists it: sent to the host, after what was sent before.
    pub fn show(&self, listing: Arc<Listing>) {
        let _ = self.shown.send(listing);
    }

    #[must_use]
    pub fn state(&self) -> State {
        match &*self.state.lock().unwrap_or_else(PoisonError::into_inner) {
            Kept::Starting => State::Starting,
            Kept::Served(at, note, _) => State::Served(at.clone(), note.clone()),
            Kept::Failed(why, strays) => State::Failed { why: why.clone(), strays: *strays },
        }
    }

    /// Start the host again, doing this with the files that are not the project's: off the window's thread.
    pub fn resolve(&self, strays: Strays, ctx: &egui::Context) {
        *self.state.lock().unwrap_or_else(PoisonError::into_inner) = Kept::Starting;
        let (project, state, ctx) = (self.project.clone(), self.state.clone(), ctx.clone());
        std::thread::spawn(move || {
            let kept = kept(&project, Some(strays));
            *state.lock().unwrap_or_else(PoisonError::into_inner) = kept;
            ctx.request_repaint();
        });
    }
}

/// The host kept, started where none runs - told what to do with what is not the project's, where told - or why not.
fn kept(project: &project::Project, strays: Option<Strays>) -> Kept {
    let args: Vec<String> = match strays {
        None => vec![],
        Some(Strays::SetAside) => vec!["--strays".into(), "set-aside".into()],
        Some(Strays::TakeIn) => vec!["--strays".into(), "take-in".into()],
    };
    let program = std::env::current_exe().map(|exe| project::host::cli_beside(&exe));
    let kept = program.map_err(anyhow::Error::from).and_then(|program| {
        if !program.is_file() {
            anyhow::bail!("{} is not there: the host serving the files is `pe2-cli`", program.display());
        }
        project::host::keep(&project.root, "pe2-editor", &program, &args)
    });
    match kept {
        Ok(lease) => lease.into(),
        Err(e) => {
            let folder = matches!(project.filesystem.serving, project::Serving::Fuse | project::Serving::Cloud);
            let strays = if folder { vfs::strays::strays(&project.filesystem.at).len() } else { 0 };
            Kept::Failed(format!("{e:#}"), strays)
        }
    }
}

/// Send what is served to the host as it changes: the newest listing each time, whole to a lease that has none yet,
/// else what changed since the last it took. What the host is not keeping yet waits for it.
fn send_shown(state: &Mutex<Kept>, newest: &mpsc::Receiver<Arc<Listing>>) {
    let mut pending: Option<Arc<Listing>> = None;
    // The lease the last listing went on, and that listing.
    let mut sent: Option<(Arc<Mutex<project::host::Lease>>, Arc<Listing>)> = None;
    loop {
        match newest.recv_timeout(std::time::Duration::from_millis(250)) {
            Ok(listing) => pending = Some(listing),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
        while let Ok(listing) = newest.try_recv() {
            pending = Some(listing);
        }
        let lease = match &*state.lock().unwrap_or_else(PoisonError::into_inner) {
            Kept::Served(_, _, lease) => lease.clone(),
            _ => continue,
        };
        // A new lease (the host started again) takes the whole listing, the last one's at least.
        let same_lease = sent.as_ref().is_some_and(|(to, _)| Arc::ptr_eq(to, &lease));
        let Some(listing) =
            pending.take().or_else(|| (!same_lease).then(|| sent.as_ref().map(|(_, l)| l.clone())).flatten())
        else {
            continue;
        };
        let said = match sent.as_ref().filter(|_| same_lease) {
            Some((_, before)) => {
                let (shown, gone) = project::host::changes(before, &listing);
                if shown.is_empty() && gone.is_empty() {
                    continue;
                }
                ToHost::Shown { whole: false, shown, gone }
            }
            None => ToHost::Shown {
                whole: true,
                shown: listing.iter().map(|(p, s)| (p.clone(), s.clone())).collect(),
                gone: vec![],
            },
        };
        let told = lease.lock().unwrap_or_else(PoisonError::into_inner).tell(&said);
        if let Ok(project::host::FromHost::Done) = told {
            sent = Some((lease, listing));
        } else {
            // Holding the project itself, or gone: the whole listing again, once it takes it.
            sent = None;
            pending = Some(listing);
        }
    }
}
