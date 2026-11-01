//! The take-in worker: what changed on disk taken into the project, off the UI thread. It owns the index, the imports
//! and what the extensions were found as; each job - paths changed, the layers hidden from view - is taken in on its
//! own thread, jobs queued meanwhile merged into one, and the result handed to the UI as a snapshot: the index (its
//! records shared with the worker's, so making one costs what changed), the tree and the layers. The UI swaps it in;
//! nothing it draws waits for a take-in.
//!
//! Every job gets a ticket, and a snapshot says the last it covers: an edit waits until the jobs before it are taken
//! in (it reads the index), an undo shows its selection once its job is, an agent's request is answered once what was
//! pending when it came is.

use project::Project;
use project::applied::Applied;
use project::ext::Extensions;
use project::imports::Imports;
use project::index::{Asset, Index, LayerInfo};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;

use crate::workspace::Node;

/// What the worker is asked.
enum Job {
    /// Paths changed on disk: by the editor, or by another program.
    Paths(Vec<PathBuf>),
    /// The layers hidden from view (the editor's eyes): every asset assembled again.
    Hidden(BTreeSet<String>),
    /// These assets' settings were applied (`project::applied`): their outputs made again from them.
    Applied(BTreeSet<String>),
}

/// The project as a take-in left it.
pub struct Snapshot {
    /// The last job it covers.
    pub ticket: u64,
    pub index: Arc<Index>,
    pub tree: Arc<Node>,
    pub layers: Vec<LayerInfo>,
    /// The assets whose content or entry changed; every asset where `all`.
    pub changed: BTreeSet<String>,
    /// It took in files changed, assets or not.
    pub files: bool,
    pub all: bool,
    /// The project is to be opened again, as this changed (`project.toml`, the extensions found).
    pub reopen: Option<&'static str>,
    /// What went wrong taking a change in.
    pub error: Option<String>,
}

/// The worker, as the UI holds it.
pub struct TakeIn {
    jobs: Option<Sender<(u64, Job)>>,
    snapshots: Receiver<Snapshot>,
    /// The last ticket given, and the last a snapshot covered.
    issued: u64,
    covered: u64,
    /// The window to wake when a snapshot is ready, once the first frame has given it.
    ctx: Arc<OnceLock<eframe::egui::Context>>,
    thread: Option<JoinHandle<()>>,
}

/// What the worker owns.
struct Worker {
    project: Project,
    index: Index,
    applied: Arc<Applied>,
    imports: Imports,
    extensions: Arc<Extensions>,
    found: Vec<extensions::Found>,
    tree: Arc<Node>,
}

impl TakeIn {
    /// Start the worker on the project as opened: its index, imports attached, and tree.
    pub fn start(
        project: Project,
        index: Index,
        applied: Arc<Applied>,
        imports: Imports,
        extensions: Arc<Extensions>,
        found: Vec<extensions::Found>,
        tree: Arc<Node>,
    ) -> TakeIn {
        let (jobs, queued) = mpsc::channel::<(u64, Job)>();
        let (made, snapshots) = mpsc::channel();
        let ctx: Arc<OnceLock<eframe::egui::Context>> = Arc::default();
        let wake = ctx.clone();
        let mut worker = Worker { project, index, applied, imports, extensions, found, tree };
        let thread = std::thread::Builder::new()
            .name("take-in".into())
            .spawn(move || {
                // Each job as it comes, with those queued behind it merged in.
                while let Ok(first) = queued.recv() {
                    let jobs: Vec<(u64, Job)> = std::iter::once(first).chain(queued.try_iter()).collect();
                    let files = jobs.iter().any(|(_, job)| matches!(job, Job::Paths(paths) if !paths.is_empty()));
                    let mut snapshot = worker.take_in(jobs);
                    snapshot.files = files;
                    if made.send(snapshot).is_err() {
                        return;
                    }
                    if let Some(ctx) = wake.get() {
                        ctx.request_repaint();
                    }
                }
            })
            .ok();
        TakeIn { jobs: Some(jobs), snapshots, issued: 0, covered: 0, ctx, thread }
    }

    /// The window to wake when a snapshot is ready.
    pub fn wake(&self, ctx: &eframe::egui::Context) {
        let _ = self.ctx.set(ctx.clone());
    }

    fn send(&mut self, job: Job) -> u64 {
        self.issued += 1;
        if let Some(jobs) = &self.jobs {
            let _ = jobs.send((self.issued, job));
        }
        self.issued
    }

    /// Take these paths in; returns the job's ticket.
    pub fn paths(&mut self, paths: Vec<PathBuf>) -> u64 {
        self.send(Job::Paths(paths))
    }

    /// Hide these layers from view; returns the job's ticket.
    pub fn hidden(&mut self, names: BTreeSet<String>) -> u64 {
        self.send(Job::Hidden(names))
    }

    /// Make these assets' outputs again from their settings as now applied; returns the job's ticket.
    pub fn applied(&mut self, paths: BTreeSet<String>) -> u64 {
        self.send(Job::Applied(paths))
    }

    /// The snapshots ready since the last poll, oldest first.
    pub fn poll(&mut self) -> Vec<Snapshot> {
        let ready: Vec<Snapshot> = self.snapshots.try_iter().collect();
        if let Some(last) = ready.last() {
            self.covered = last.ticket;
        }
        ready
    }

    /// Wait for every job asked so far, returning their snapshots: what an edit does before reading the index.
    pub fn settle(&mut self) -> Vec<Snapshot> {
        let mut ready = self.poll();
        while self.covered < self.issued {
            let Ok(snapshot) = self.snapshots.recv() else { break };
            self.covered = snapshot.ticket;
            ready.push(snapshot);
        }
        ready
    }

    /// A job asked is not taken in yet.
    pub fn busy(&self) -> bool {
        self.covered < self.issued
    }

    /// Whether the job of this ticket is taken in.
    pub fn covers(&self, ticket: u64) -> bool {
        self.covered >= ticket
    }

    /// The last ticket given.
    pub fn issued(&self) -> u64 {
        self.issued
    }
}

impl Drop for TakeIn {
    /// The worker finishes the job it is on and stops: the project's records are written by one owner at a time.
    fn drop(&mut self) {
        self.jobs = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Worker {
    /// Take the jobs in, as one: the layers hidden as the last asks, the paths of all.
    fn take_in(&mut self, jobs: Vec<(u64, Job)>) -> Snapshot {
        let ticket = jobs.iter().map(|(t, _)| *t).max().unwrap_or(0);
        let mut paths = Vec::new();
        let mut hidden = None;
        let mut applied = BTreeSet::new();
        for (_, job) in jobs {
            match job {
                Job::Paths(p) => paths.extend(p),
                Job::Hidden(names) => hidden = Some(names),
                Job::Applied(assets) => applied.extend(assets),
            }
        }
        paths.sort();
        paths.dedup();
        let mut all = false;
        if let Some(names) = hidden
            && self.index.set_hidden(&names)
        {
            // Every asset assembled again, with its settings as applied: their imports attached again.
            self.applied.mark(&mut self.index, None);
            let _ = self.imports.attach(&mut self.index, &self.extensions);
            all = true;
        }
        if !applied.is_empty() {
            // Their outputs made from their settings as now applied.
            self.applied.mark(&mut self.index, Some(&applied));
            let _ = self.imports.attach_changed(&mut self.index, &self.extensions, &applied);
        }
        if paths.is_empty() {
            return self.snapshot(ticket, applied, all, None, None);
        }
        // What the assets were before: a declaration changed is noted unapplied, against what it was.
        let before = self.index.assets.clone();
        let refreshed = match self.index.refresh(&self.project, &paths) {
            Ok(refreshed) => refreshed,
            Err(e) => {
                let error = format!("the index could not follow a change: {e:#}");
                return self.snapshot(ticket, BTreeSet::new(), all, None, Some(error));
            }
        };
        if refreshed.project_file {
            return self.snapshot(ticket, refreshed.assets, all, Some("project.toml changed"), None);
        }
        // An `extension` asset came, went or changed.
        if extensions::found(Some(&self.index)) != self.found {
            return self.snapshot(ticket, refreshed.assets, all, Some("an extension changed"), None);
        }
        // The assets the refresh assembled again: their changed declarations noted, their settings as applied given
        // them, their imports attached again; the others keep theirs.
        for path in &refreshed.assets {
            let Some(old) = before.get(path) else { continue };
            let Some(uid) = old.uid.as_deref() else { continue };
            match self.index.assets.get(path) {
                // What makes nothing has nothing to apply: whatever reads it has the settings as they are.
                Some(now) if now.uid.as_deref() == Some(uid) && !makes_outputs(&self.extensions, now) => {}
                Some(now) if now.uid.as_deref() == Some(uid) => {
                    let _ = self.applied.changed(uid, old.text(), now.text());
                }
                Some(_) => {}
                None => self.applied.forget(uid),
            }
        }
        self.applied.mark(&mut self.index, Some(&refreshed.assets));
        let _ = self.imports.attach_changed(&mut self.index, &self.extensions, &refreshed.assets);
        let mut changed = refreshed.assets;
        changed.extend(applied);
        self.snapshot(ticket, changed, all, None, None)
    }

    /// The index as it is now, handed over: the tree built again where anything changed.
    fn snapshot(
        &mut self,
        ticket: u64,
        changed: BTreeSet<String>,
        all: bool,
        reopen: Option<&'static str>,
        error: Option<String>,
    ) -> Snapshot {
        if all || !changed.is_empty() {
            self.tree = Arc::new(Node::build(&self.index, &self.extensions));
        }
        Snapshot {
            ticket,
            index: Arc::new(self.index.clone()),
            tree: self.tree.clone(),
            layers: self.index.layers(),
            changed,
            all,
            reopen,
            error,
            files: false,
        }
    }
}

/// Whether anything is made from an asset's settings: more than the settings themselves.
fn makes_outputs(extensions: &Extensions, asset: &Asset) -> bool {
    let (ops, tail) = (extensions.operations(), extensions.tail(asset));
    let plan = project::graph::Plan::of(asset.fields(), &ops, &tail).0;
    plan.outputs.iter().any(|output| plan.steps[output.from].op != project::graph::SETTINGS)
}
