//! The project served as a filesystem while the editor has it open, at its `assets` folder, by the backend this machine
//! says (`project.local.toml`, `[filesystem] serve`) - by the host (`pe2-cli project host`, `project::host`), which the
//! editor starts where none runs and keeps while it has the project, and which serves on after it, while anything uses
//! what it serves. Opening the project borrows it from the host (`Lock::try_acquire`), so what programs write through
//! the folder reaches the editor on its link as edits (`mcp::Request::Edit`), applied between frames as steps of the
//! session's log (`Workspace::apply_edit`); the program waits for it, as an agent waits for its request.
//!
//! The host does not serve over files in the folder that are not the project's (`vfs::strays`): the user chooses to set
//! them aside or take them in, and the host is started again to.

use eframe::egui;
use std::sync::{Arc, Mutex, PoisonError};

/// The host kept serving the project, or why it is not.
pub struct Files {
    project: project::Project,
    state: Arc<Mutex<Kept>>,
}

enum Kept {
    Starting,
    Served(project::host::Lease),
    /// Why not, and how many files in the folder are not the project's (none where that is not why).
    Failed(String, usize),
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
    /// Keep the host serving the project, started where none runs: none where this machine serves nothing and no host
    /// serves it already. Before the project is taken, for the host to have it first and lend it.
    #[must_use]
    pub fn keep(project: &project::Project) -> Option<Files> {
        let root = &project.root;
        if project.filesystem.serving == project::Serving::None && project::host::Connection::connect(root).is_none() {
            return None;
        }
        let state = Arc::new(Mutex::new(kept(project, None)));
        Some(Files { project: project.clone(), state })
    }

    #[must_use]
    pub fn state(&self) -> State {
        match &*self.state.lock().unwrap_or_else(PoisonError::into_inner) {
            Kept::Starting => State::Starting,
            Kept::Served(lease) => State::Served(lease.at.clone(), lease.note.clone()),
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
        Ok(lease) => Kept::Served(lease),
        Err(e) => {
            let folder = matches!(project.filesystem.serving, project::Serving::Fuse | project::Serving::Cloud);
            let strays = if folder { vfs::strays::strays(&project.filesystem.at).len() } else { 0 };
            Kept::Failed(format!("{e:#}"), strays)
        }
    }
}
