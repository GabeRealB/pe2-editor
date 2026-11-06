//! The take-in worker: what changed on disk taken into the project, off the UI thread. It owns the index, the imports
//! and what the extensions were found as; each job - paths changed, the layers hidden from view - is taken in on its
//! own thread, jobs queued meanwhile merged into one, and the result handed to the UI as a snapshot: the index (its
//! records shared with the worker's, so making one costs what changed), the tree and the layers. The UI swaps it in;
//! nothing it draws waits for a take-in.
//!
//! Every job gets a ticket, and a snapshot says the last it covers: an edit waits until the jobs before it are taken
//! in (it reads the index), an undo shows its selection once its job is, an agent's request is answered once what was
//! pending when it came is.
//!
//! What the project is made of changing - its `project.toml`, the extensions it finds - is a job like these. The project
//! is read again here, the old one answering until a snapshot hands the new one over, so that nothing shown goes away
//! and what is asked meanwhile comes after it, in order. So is attaching the imports of a project just opened.

use project::Project;
use project::applied::Applied;
use project::cache::Cache;
use project::ext::Extensions;
use project::imports::Imports;
use project::index::{Asset, Index, LayerInfo};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;

use crate::workspace::{Loaded, Node};

/// What the worker is asked.
enum Job {
    /// Paths changed on disk: by the editor, or by another program.
    Paths(Vec<PathBuf>),
    /// The layers hidden from view (the editor's eyes): every asset assembled again.
    Hidden(BTreeSet<String>),
    /// These assets' settings were applied (`project::applied`): their outputs made again from them.
    Applied(BTreeSet<String>),
    /// A project just opened: what its importers made attached.
    Attach,
    /// The project read again, as this changed; its files too where `whole`, as they are otherwise followed already.
    Reload { why: &'static str, whole: bool },
    /// Reading until told to stop: a test's way to find it at work.
    #[cfg(any(test, feature = "tests"))]
    Hold(Receiver<()>),
}

/// What a project read again has that the window takes up (`Workspace::take_up`).
pub struct Reloaded {
    pub project: Project,
    pub extensions: Arc<Extensions>,
    pub found: Vec<extensions::Found>,
    /// What changed.
    pub why: &'static str,
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
    /// The project as it was read again, where it was.
    pub reloaded: Option<Reloaded>,
    /// Why it could not be: it goes on as it was.
    pub said: Option<String>,
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
    /// How far a project being read again, or attached, is.
    stage: Arc<Mutex<String>>,
    /// What gives up attaching when nobody will see it.
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// What the worker owns.
pub struct Worker {
    pub project: Project,
    pub index: Index,
    pub cache: Cache,
    pub store: project::graph::store::Store,
    pub applied: Arc<Applied>,
    pub imports: Imports,
    pub extensions: Arc<Extensions>,
    pub found: Vec<extensions::Found>,
    pub tree: Arc<Node>,
    pub stage: Arc<Mutex<String>>,
}

impl TakeIn {
    /// Start the worker on the project as opened.
    pub fn start(mut worker: Worker) -> TakeIn {
        let (jobs, queued) = mpsc::channel::<(u64, Job)>();
        let (made, snapshots) = mpsc::channel();
        let ctx: Arc<OnceLock<eframe::egui::Context>> = Arc::default();
        let wake = ctx.clone();
        let (stage, stop) = (worker.stage.clone(), worker.imports.stopper());
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
        TakeIn { jobs: Some(jobs), snapshots, issued: 0, covered: 0, ctx, stage, stop, thread }
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
        // That it is under way shows.
        if let Some(ctx) = self.ctx.get() {
            ctx.request_repaint();
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

    /// Attach the imports of the project as opened; returns the job's ticket.
    pub fn attach(&mut self) -> u64 {
        self.send(Job::Attach)
    }

    /// Read the project again, as this changed; its files too where `whole`.
    pub fn reload(&mut self, why: &'static str, whole: bool) {
        self.send(Job::Reload { why, whole });
    }

    /// Have the worker read until what is returned goes: for a test, reading takes as long as it likes.
    #[cfg(any(test, feature = "tests"))]
    pub fn hold(&mut self) -> Sender<()> {
        let (release, wait) = mpsc::channel();
        if let Ok(mut stage) = self.stage.lock() {
            "held".clone_into(&mut stage);
        }
        self.send(Job::Hold(wait));
        release
    }

    /// How far the project being read again or attached is, while it is.
    pub fn stage(&self) -> Option<String> {
        self.stage.lock().ok().map(|stage| stage.clone()).filter(|stage| !stage.is_empty())
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
    /// The worker finishes the job it is on and stops: the project's records are written by one owner at a time. Attaching
    /// gives up where it is: what it would come to is for a window that is going.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
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
        let (mut attach, mut reload, mut whole) = (false, None, false);
        for (_, job) in jobs {
            match job {
                Job::Paths(p) => paths.extend(p),
                Job::Hidden(names) => hidden = Some(names),
                Job::Applied(assets) => applied.extend(assets),
                Job::Attach => attach = true,
                Job::Reload { why, whole: all_of_it } => {
                    reload = Some(why);
                    whole |= all_of_it;
                }
                #[cfg(any(test, feature = "tests"))]
                Job::Hold(wait) => {
                    let _ = wait.recv();
                    self.reporter()("", 0, 0);
                }
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
        if attach && !all {
            self.attach_all();
            self.reporter()("", 0, 0);
            all = true;
        }
        if !applied.is_empty() {
            // Their outputs made from their settings as now applied.
            self.applied.mark(&mut self.index, Some(&applied));
            let _ = self.imports.attach_changed(&mut self.index, &self.extensions, &applied);
            self.sweep_if_due();
        }
        if paths.is_empty() {
            return match reload {
                Some(why) => self.reload(ticket, why, whole),
                None => self.snapshot(ticket, applied, all, None),
            };
        }
        // What the assets were before: a declaration changed is noted unapplied, against what it was.
        let before = self.index.assets.clone();
        let refreshed = match self.index.refresh(&self.project, &paths) {
            Ok(refreshed) => refreshed,
            Err(e) => {
                let error = format!("the index could not follow a change: {e:#}");
                return self.snapshot(ticket, BTreeSet::new(), all, Some(error));
            }
        };
        if refreshed.project_file {
            return self.reload(ticket, "project.toml changed", whole);
        }
        // An `extension` asset came, went or changed.
        if extensions::found(&self.project, Some(&self.index)) != self.found {
            return self.reload(ticket, "an extension changed", whole);
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
        self.sweep_if_due();
        let mut changed = refreshed.assets;
        changed.extend(applied);
        match reload {
            Some(why) => self.reload(ticket, why, whole),
            None => self.snapshot(ticket, changed, all, None),
        }
    }

    /// How far it is, for the status bar.
    fn reporter(&self) -> impl Fn(&str, usize, usize) + use<> {
        let stage = self.stage.clone();
        move |said, _, _| {
            if let Ok(mut stage) = stage.lock() {
                said.clone_into(&mut stage);
            }
        }
    }

    /// What the importers made attached to every asset, and what the store keeps that nothing makes any more swept away.
    fn attach_all(&mut self) {
        let report = self.reporter();
        let _ = self.imports.attach_reporting(&mut self.index, &self.extensions, &report);
        self.sweep_aside();
    }

    /// The store swept of what the project's nodes no longer make, aside - it reads all the store keeps - noted as swept
    /// first, so that nothing else starts one meanwhile.
    fn sweep_aside(&self) {
        self.store.mark_swept();
        let (store, nodes) = (self.store.clone(), project::imports::live_nodes(&self.index));
        let _ = std::thread::Builder::new().name("store sweep".into()).spawn(move || {
            let _ = project::imports::sweep_store(&store, &nodes);
        });
    }

    /// The store swept aside where a sweep is due (`SWEEP_EVERY` since the last, something kept since): what a long
    /// session makes kept no longer than that past what the project makes now.
    fn sweep_if_due(&self) {
        if self.store.sweep_due(project::imports::SWEEP_EVERY) {
            self.sweep_aside();
        }
    }

    /// The project read again, as `why` changed, and handed over; where it cannot be, it goes on as it was.
    fn reload(&mut self, ticket: u64, why: &'static str, whole: bool) -> Snapshot {
        self.reporter()("Reading the project", 0, 0);
        let read = self.read_again(whole);
        let snapshot = match read {
            Ok(changed) => {
                let mut snapshot = self.snapshot(ticket, BTreeSet::new(), changed, None);
                let (project, extensions, found) = (self.project.clone(), self.extensions.clone(), self.found.clone());
                snapshot.reloaded = Some(Reloaded { project, extensions, found, why });
                snapshot
            }
            Err(e) => {
                let mut snapshot = self.snapshot(ticket, BTreeSet::new(), false, None);
                snapshot.said = Some(format!("{why}, and the project could not be read again: {e:#}"));
                snapshot
            }
        };
        self.reporter()("", 0, 0);
        snapshot
    }

    /// Whether it comes to anything: what was showing is shown by what it came to, which a window feels, so that a
    /// `project.toml` changed in what neither the layers nor the extensions read has it go on as it is.
    fn read_again(&mut self, whole: bool) -> anyhow::Result<bool> {
        let report = self.reporter();
        let project = Project::load(&self.project.root)?;
        // Where the layers are the same, what is in them is what the index holds: it has followed them all along. What
        // extensions make of it is attached anew, and an index knows nothing of them.
        let read = whole || project.layers != self.project.layers;
        let index = if read { self.index.again(&project, &report)? } else { self.index.clone() };
        let loaded = Loaded::of(project, index, &self.cache, &self.store, &self.applied, &report);
        self.project = loaded.project;
        self.found = loaded.found;
        if !read && Arc::ptr_eq(&loaded.extensions, &self.extensions) {
            return Ok(false);
        }
        self.index = loaded.index;
        self.extensions = loaded.extensions;
        self.attach_all();
        Ok(true)
    }

    /// The index as it is now, handed over: the tree built again where anything changed.
    fn snapshot(&mut self, ticket: u64, changed: BTreeSet<String>, all: bool, error: Option<String>) -> Snapshot {
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
            reloaded: None,
            said: None,
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
