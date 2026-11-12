//! The worker holding the project for the window: one thread with the project's index and its history, as any
//! holder has them (`holder::core`), saying what each change comes to where the project's processes share it;
//! with the imports and what the extensions were found as. What changes the project is done there, in the order
//! it was asked: jobs - paths changed on disk, the layers hidden from view, the point of the history shown -
//! taken in, those asked meanwhile merged into one; and work on the history and the index together (`held`: an
//! edit, a step undone, a document saved), which finds the project as all asked before it left it. What each
//! comes to is handed to the UI as a snapshot: the index (its records shared with the worker's, so making one
//! costs what changed), the tree, the layers and the history as it shows. The UI swaps it in; nothing it draws
//! reads what the worker is changing.
//!
//! Every job gets a ticket, and a snapshot says the last it covers: an agent's request is answered once what was
//! pending when it came is taken in, what an event needs waits for its ticket.
//!
//! What the project is made of changing - its `project.toml`, the extensions it finds - is a job like these. The project
//! is read again here, the old one answering until a snapshot hands the new one over, so that nothing shown goes away
//! and what is asked meanwhile comes after it, in order. So is attaching the imports of a project just opened.

use holder::core::{Core, Queue};
use project::Project;
use project::cache::Cache;
use project::ext::Extensions;
use project::imports::Imports;
use project::history::History;
use project::index::{Index, LayerInfo, Working};
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use crate::workspace::{Loaded, Node};

/// What the worker is asked.
pub enum Job {
    /// Paths changed on disk, by another program.
    Paths(Vec<PathBuf>),
    /// The layers hidden from view (the editor's eyes): every asset assembled again.
    Hidden(BTreeSet<String>),
    /// The point of the history shown, read-only - after its first steps, of one document or all - or none: now.
    /// The index shows the documents' working copies as they were then.
    Viewing(Option<(usize, Option<String>)>),
    /// A project just opened: what its importers made attached.
    Attach,
    /// The project read again, as this changed; its files too where `whole`, as they are otherwise followed already.
    Reload { why: &'static str, whole: bool },
    /// The files held to what the index took of them as known when it was opened (`Index::open_behind`): those
    /// that are not, taken in as paths changed.
    Verify(Verify),
    /// Work on the history and the index was done (`Worker::held`): what it came to is handed over.
    Held,
    /// Reading until told to stop: a test's way to find it at work.
    #[cfg(any(test, feature = "tests"))]
    Hold(Receiver<()>),
}

/// What tells which files are not as an index opened behind took them to be (`Index::verification`).
pub type Verify = Box<dyn FnOnce() -> Vec<PathBuf> + Send>;

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
    /// The history as it shows now: its steps, the documents with unsaved changes.
    pub history: Arc<project::history::View>,
    /// The files the editor's own edits wrote: told of again by what watches the layers, they are its own.
    pub wrote: Vec<PathBuf>,
}

/// The worker, as the UI holds it.
pub struct TakeIn {
    /// The thread holding the project, with what it owns; none once it was told to end.
    queue: Option<Queue<Worker>>,
    /// What was asked and is not taken in yet: taken together by the thread, whichever asking wakes it.
    asked: Arc<Mutex<Vec<(u64, Job)>>>,
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
}

/// What the worker owns.
pub struct Worker {
    /// The project held: its index, and what is said of it where the project's processes share it.
    pub core: Core,
    /// What was done this session, to undo and redo: its steps and the documents with unsaved changes.
    pub history: History,
    /// The history as last shown (`History::view`), and which of its revisions that was.
    viewed: (u64, Arc<project::history::View>),
    /// The point of the history shown, where one is (`Job::Viewing`).
    viewing: Option<(usize, Option<String>)>,
    /// What work on the history and the index came to and is not handed over yet.
    taken: holder::core::Taken,
    pub cache: Cache,
    pub store: project::graph::store::Store,
    pub imports: Imports,
    pub extensions: Arc<Extensions>,
    pub found: Vec<extensions::Found>,
    pub tree: Arc<Node>,
    pub stage: Arc<Mutex<String>>,
    /// The index as last handed over.
    pub shown: Option<Arc<Index>>,
    /// Files were found not to be as the index took them when it was opened: a change of files, as any.
    pub changed_behind: bool,
    /// What is asked of it (`TakeIn::asked`), where its snapshots go, and the window to wake for them.
    pub asked: Arc<Mutex<Vec<(u64, Job)>>>,
    pub made: Sender<Snapshot>,
    pub wake: Arc<OnceLock<eframe::egui::Context>>,
    /// Taking in changes is the editor's own doing, behind what the user looks at.
    pub group: project::schedule::Group,
}

/// What a worker is made of, besides what `TakeIn::start` gives it.
pub struct Start {
    pub core: Core,
    pub history: History,
    pub cache: Cache,
    pub store: project::graph::store::Store,
    pub imports: Imports,
    pub extensions: Arc<Extensions>,
    pub found: Vec<extensions::Found>,
    pub tree: Arc<Node>,
}

impl TakeIn {
    /// Start the worker on the project as opened.
    pub fn start(start: Start) -> TakeIn {
        let asked: Arc<Mutex<Vec<(u64, Job)>>> = Arc::default();
        let (made, snapshots) = mpsc::channel();
        let ctx: Arc<OnceLock<eframe::egui::Context>> = Arc::default();
        let stage: Arc<Mutex<String>> = Arc::default();
        let stop = start.imports.stopper();
        let Start { core, history, cache, store, imports, extensions, found, tree } = start;
        let worker = Worker {
            core,
            viewed: (history.revision(), Arc::new(history.view())),
            history,
            viewing: None,
            taken: holder::core::Taken::default(),
            cache,
            store,
            imports,
            extensions,
            found,
            tree,
            stage: stage.clone(),
            shown: None,
            changed_behind: false,
            asked: asked.clone(),
            made,
            wake: ctx.clone(),
            group: project::schedule::Group::new(project::schedule::Class::Background, "take-in"),
        };
        let queue = Queue::start("take-in", worker).map_err(|e| eprintln!("no thread takes changes in: {e}")).ok();
        TakeIn { queue, asked, snapshots, issued: 0, covered: 0, ctx, stage, stop }
    }

    /// The window to wake when a snapshot is ready.
    pub fn wake(&self, ctx: &eframe::egui::Context) {
        let _ = self.ctx.set(ctx.clone());
    }

    /// Ask this of the worker, under the ticket given: taken in with all that was asked and is not yet.
    fn ask(&self, ticket: u64, job: Job) {
        let Some(queue) = &self.queue else { return };
        self.asked.lock().unwrap_or_else(PoisonError::into_inner).push((ticket, job));
        queue.post(Worker::work);
    }

    fn send(&mut self, job: Job) -> u64 {
        self.issued += 1;
        self.ask(self.issued, job);
        // That it is under way shows.
        if let Some(ctx) = self.ctx.get() {
            ctx.request_repaint();
        }
        self.issued
    }

    /// Have the worker do this with the history and the index together, once all that was asked before it is
    /// taken in, and wait for it: what it wrote is taken in, and what it came to handed over, by the time this
    /// returns. None where the worker is gone.
    pub fn held<R: Send + 'static>(&mut self, work: impl FnOnce(&mut Worker) -> R + Send + 'static) -> Option<R> {
        self.issued += 1;
        let ticket = self.issued;
        self.queue.as_ref()?.call(move |worker| worker.held(ticket, work))
    }

    /// Hold the files to what the index took of them as known when it was opened, and take in those that are
    /// not; returns the job's ticket, which an edit waits for as for any.
    pub fn verify(&mut self, verify: Verify) -> u64 {
        self.send(Job::Verify(verify))
    }

    /// Take these paths in; returns the job's ticket.
    pub fn paths(&mut self, paths: Vec<PathBuf>) -> u64 {
        self.send(Job::Paths(paths))
    }

    /// Hide these layers from view; returns the job's ticket.
    pub fn hidden(&mut self, names: BTreeSet<String>) -> u64 {
        self.send(Job::Hidden(names))
    }

    /// Show the documents as at this point of the history - after its first steps, of one document or all -
    /// or as they are now (none); returns the job's ticket.
    pub fn viewing(&mut self, point: Option<(usize, Option<String>)>) -> u64 {
        self.send(Job::Viewing(point))
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
        // Waited for: what it was given before this is done first.
        self.queue = None;
    }
}

impl Worker {
    /// Take in all that was asked and is not yet, as one, and hand over what it came to: what the thread does
    /// each time something was asked - nothing, where an asking before this one took it along.
    fn work(&mut self) {
        let jobs: Vec<(u64, Job)> = std::mem::take(&mut *self.asked.lock().unwrap_or_else(PoisonError::into_inner));
        if jobs.is_empty() {
            return;
        }
        let files = jobs.iter().any(|(_, job)| matches!(job, Job::Paths(paths) if !paths.is_empty()));
        let group = self.group.clone();
        let mut snapshot = group.enter(|| {
            let _span = tracing::info_span!("take-in worker: a take-in").entered();
            self.take_in(jobs)
        });
        snapshot.files |= files;
        if self.made.send(snapshot).is_ok()
            && let Some(ctx) = self.wake.get()
        {
            ctx.request_repaint();
        }
    }

    /// Do this with the history and the index as they are once all that was asked before is taken in; what it
    /// wrote is taken in after it, and what that came to handed over under this ticket.
    fn held<R>(&mut self, ticket: u64, work: impl FnOnce(&mut Worker) -> R) -> R {
        self.work();
        let done = {
            let _span = tracing::info_span!("take-in worker: work on the history").entered();
            work(self)
        };
        // What the history wrote by other means than the core's own edits: a step undone, a document saved.
        match self.core.take_written(&mut self.history) {
            Ok(taken) => self.taken.and(taken),
            Err(e) => eprintln!("the index could not follow what an edit wrote: {e}"),
        }
        self.asked.lock().unwrap_or_else(PoisonError::into_inner).push((ticket, Job::Held));
        self.work();
        done
    }

    /// Make edits asked from outside the window - a command's, a program's through the served folder - as
    /// steps of the history (`Core::edits`): how each went.
    pub fn edits(&mut self, ops: &[vfs::ops::Op], layer: &str) -> Vec<mcp::Reply> {
        let (replies, taken) = self.core.edits(&mut self.history, ops, Some(layer));
        self.taken.and(taken);
        replies
    }

    /// Take what is due of what programs did through the served folder (`Core::take_due`), in this layer - or
    /// none of it, where no layer can be edited now, and why: `then` given what was answered once that is said
    /// and served.
    pub fn take_due(&mut self, layer: Result<&str, &str>, then: holder::core::Then) {
        let (_, taken) = self.core.take_due(&mut self.history, layer.map(Some), Some(then));
        self.taken.and(taken);
    }

    /// Show the documents as they are now, where a point of the history was shown.
    pub fn show_now(&mut self) {
        self.viewing = None;
    }

    /// The working copies the index shows: the documents' as they are now, or as at the point of the history
    /// shown.
    fn working(&self) -> HashMap<String, Working> {
        match &self.viewing {
            Some((point, scope)) => self.history.working_at(*point, scope.as_deref()),
            None => self.history.working(),
        }
    }

    /// Take the jobs in, as one - the layers hidden as the last asks, the paths of all - with what work on the
    /// history came to since the last, and hand over the project as that leaves it. What changed of its files
    /// is said, where the project's processes share it, as it is taken in (`Core`).
    fn take_in(&mut self, jobs: Vec<(u64, Job)>) -> Snapshot {
        let ticket = jobs.iter().map(|(t, _)| *t).max().unwrap_or(0);
        let mut paths = Vec::new();
        let mut hidden = None;
        let (mut attach, mut reload, mut whole) = (false, None, false);
        for (_, job) in jobs {
            match job {
                Job::Paths(p) => paths.extend(p),
                Job::Hidden(names) => hidden = Some(names),
                Job::Viewing(point) => self.viewing = point,
                Job::Attach => attach = true,
                Job::Reload { why, whole: all_of_it } => {
                    reload = Some(why);
                    whole |= all_of_it;
                }
                Job::Verify(verify) => {
                    let differing = verify();
                    if !differing.is_empty() {
                        eprintln!("{} files were not as they were known: taken in", differing.len());
                        self.changed_behind = true;
                    }
                    paths.extend(differing);
                }
                Job::Held => {}
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
            && self.core.index.set_hidden(&names)
        {
            // Every asset assembled again: their imports attached again.
            let _ = self.imports.attach(&mut self.core.index, &self.extensions);
            all = true;
        }
        if attach && !all {
            self.attach_all();
            self.reporter()("", 0, 0);
            all = true;
        }
        // What changed of the files: what work on the history wrote, taken in already, and what other programs
        // changed.
        let mut taken = std::mem::take(&mut self.taken);
        let mut error = None;
        if !paths.is_empty() {
            match self.core.take_in(&paths) {
                Ok(Some(more)) => taken.and(more),
                Ok(None) => {}
                Err(e) => error = Some(format!("the index could not follow a change: {e}")),
            }
        }
        let holder::core::Taken { refreshed, wrote } = taken;
        let files_changed = !paths.is_empty() || !wrote.is_empty();
        if refreshed.project_file {
            return self.reload(ticket, "project.toml changed", whole, wrote);
        }
        // An extension the project holds came, went or changed.
        if files_changed && extensions::found(&self.core.project) != self.found {
            return self.reload(ticket, "an extension changed", whole, wrote);
        }
        // The documents follow the disk under them - stale where it changed, saved where it is their working
        // copy - and the index shows them as the history has them.
        self.history.reconcile(&self.core.index);
        let mut changed = self.core.index.set_working(self.working());
        changed.extend(refreshed.assets);
        // The assets assembled again: their imports attached again; the others keep theirs - but what failed,
        // which may have failed for what changed now, and is tried again.
        if !all && (files_changed || !changed.is_empty()) {
            let _ = self.imports.attach_changed(&mut self.core.index, &self.extensions, &changed);
            self.sweep_if_due();
        }
        match reload {
            Some(why) => self.reload(ticket, why, whole, wrote),
            None => self.snapshot(ticket, changed, all, error, wrote),
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
        let _ = self.imports.attach_reporting(&mut self.core.index, &self.extensions, &report);
        self.sweep_aside();
    }

    /// The store swept of what the project's nodes no longer make, aside - it reads all the store keeps - noted as swept
    /// first, so that nothing else starts one meanwhile.
    fn sweep_aside(&self) {
        self.store.mark_swept();
        let (store, nodes) = (self.store.clone(), project::imports::live_nodes(&self.core.index));
        let project = self.core.project.clone();
        let _ = std::thread::Builder::new().name("store sweep".into()).spawn(move || {
            let _ = project::imports::sweep_store(&store, &nodes);
            // The layers' chunks too, where it is due: those nothing names, and long enough ago to be no write's.
            if let Some(Ok((chunks, bytes))) = project::chunks::sweep_if_due(&project)
                && chunks > 0
            {
                tracing::info!(chunks, bytes, "swept the chunk store");
            }
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
    fn reload(&mut self, ticket: u64, why: &'static str, whole: bool, wrote: Vec<PathBuf>) -> Snapshot {
        self.reporter()("Reading the project", 0, 0);
        let read = self.read_again(whole);
        let snapshot = match read {
            Ok(changed) => {
                let mut snapshot = self.snapshot(ticket, BTreeSet::new(), changed, None, wrote);
                let (project, extensions, found) = (self.core.project.clone(), self.extensions.clone(), self.found.clone());
                snapshot.reloaded = Some(Reloaded { project, extensions, found, why });
                snapshot
            }
            Err(e) => {
                let mut snapshot = self.snapshot(ticket, BTreeSet::new(), false, None, wrote);
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
        let project = Project::load(&self.core.project.root)?;
        // Where the layers are the same, what is in them is what the index holds: it has followed them all along. What
        // extensions make of it is attached anew, and an index knows nothing of them.
        let read = whole || project.layers != self.core.project.layers;
        let index = if read { self.core.index.again(&project, &report)? } else { self.core.index.clone() };
        let loaded = Loaded::of(project, index, &self.cache, &self.store, &report);
        self.core.project = loaded.project;
        self.found = loaded.found;
        if !read && Arc::ptr_eq(&loaded.extensions, &self.extensions) {
            return Ok(false);
        }
        self.core.index = loaded.index;
        self.extensions = loaded.extensions;
        // The documents shown as the history has them, in the index made again.
        self.core.index.set_working(self.working());
        // What the index read again that differs from what it knew of the files, and all that is served as it
        // lists it now: what differs is not known by what a refresh changed.
        let opened = self.core.index.take_opened();
        self.core.say_whole(&opened);
        self.attach_all();
        Ok(true)
    }

    /// The index as it is now, handed over: the tree built again where anything changed, the history as it
    /// shows.
    fn snapshot(
        &mut self,
        ticket: u64,
        changed: BTreeSet<String>,
        all: bool,
        error: Option<String>,
        wrote: Vec<PathBuf>,
    ) -> Snapshot {
        if all || !changed.is_empty() {
            let _span = tracing::info_span!("take-in worker: the tree built").entered();
            self.tree = Arc::new(Node::build(&self.core.index, &self.extensions));
        }
        if self.viewed.0 != self.history.revision() {
            self.viewed = (self.history.revision(), Arc::new(self.history.view()));
        }
        // What the job freed, given back: the worker's threads each keep what they freed otherwise.
        project::give_back_memory();
        let shown = Arc::new(self.core.index.clone());
        self.shown = Some(shown.clone());
        Snapshot {
            ticket,
            index: shown,
            tree: self.tree.clone(),
            layers: self.core.index.layers(),
            changed,
            all,
            reloaded: None,
            said: None,
            error,
            files: std::mem::take(&mut self.changed_behind) || !wrote.is_empty(),
            history: self.viewed.1.clone(),
            wrote,
        }
    }
}

impl Drop for Worker {
    /// Closed cleanly, its session goes - unless documents are unsaved still: the next opening offers them back.
    /// Where the thread holding it failed, it is left as a crash leaves it.
    fn drop(&mut self) {
        if !std::thread::panicking() && self.history.documents().is_empty() {
            self.history.wipe();
        }
    }
}
