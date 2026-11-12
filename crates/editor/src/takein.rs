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
use project::cache::Cache;
use project::ext::Extensions;
use project::imports::Imports;
use project::index::{Index, LayerInfo, Working};
use std::collections::{BTreeSet, HashMap};
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
    /// The working copies of the assets with unsaved changes, by uid: the index shows them in place of their files.
    Working(HashMap<String, Working>),
    /// A project just opened: what its importers made attached.
    Attach,
    /// The project read again, as this changed; its files too where `whole`, as they are otherwise followed already.
    Reload { why: &'static str, whole: bool },
    /// Edits are about to be made of what was due (`Workspace::take_due`): nothing is said where the project's
    /// processes share it until what became of them is.
    DueBegin,
    /// What became of what was due, by its number, and what is done once that is said: said in the commit that
    /// says what is served after the edits made of it.
    DueEnd { answers: Vec<(u64, vfs::due::Answer)>, then: holder::files::Then },
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
    /// What is served of the project now: said by the worker (`Worker::say`), with what the take-in changed of
    /// the project's model.
    served: Option<Served>,
}

/// What the worker took in and has yet to say where the project's processes share it (`holder::says`): said
/// after each take-in, but while edits are made of what was due - then once what became of them is known, so
/// that each is the project's, and known to be, in one commit.
#[derive(Default)]
pub struct Unsaid {
    /// What each take-in it is of says is served after it, in their order.
    served: Vec<Served>,
    /// What they changed of the project's model.
    model: project::model::Delta,
    /// The index was read again whole: the model is held to all it read.
    read_again: bool,
    /// What became of what was due, and what is done once that is said.
    answers: Vec<(u64, vfs::due::Answer)>,
    then: Vec<holder::files::Then>,
    /// How many times edits of what was due were begun and not ended.
    due: usize,
}

/// What is served of a project after a take-in.
pub enum Served {
    /// All of it, listed.
    Whole(Arc<project::server::Listing>),
    /// What of it differs from what was served before: what files changing costs, whatever the project holds.
    Changed(project::server::Delta),
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
    pub imports: Imports,
    pub extensions: Arc<Extensions>,
    pub found: Vec<extensions::Found>,
    pub tree: Arc<Node>,
    pub stage: Arc<Mutex<String>>,
    /// What is served, as last listed: brought up to what each change of files changed, to say what differs.
    pub listing: Option<project::server::Listing>,
    /// Where what it takes in is said - the project's model, what is served, what became of what was due - a
    /// commit a take-in, by this thread; and what tells the server of it, where one is fed.
    pub says: Arc<holder::says::Says>,
    pub files: Option<Arc<holder::files::Files>>,
    pub unsaid: Unsaid,
    /// The index as last handed over.
    pub shown: Option<Arc<Index>>,
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
                // Taking in changes is the editor's own doing, behind what the user looks at.
                let group = project::schedule::Group::new(project::schedule::Class::Background, "take-in");
                while let Ok(first) = queued.recv() {
                    let jobs: Vec<(u64, Job)> = std::iter::once(first).chain(queued.try_iter()).collect();
                    let files = jobs.iter().any(|(_, job)| matches!(job, Job::Paths(paths) if !paths.is_empty()));
                    let snapshot = group.enter(|| {
                        let _span = tracing::info_span!("take-in worker: a take-in").entered();
                        worker.take_in(jobs)
                    });
                    // Nothing was asked but to say what became of what was due: said, and nothing to hand over.
                    let Some(mut snapshot) = snapshot else { continue };
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

    /// Edits are about to be made of what was due: what the worker takes in from here on is said once what became
    /// of them is (`due_end`), never before. No job to wait for: it has no ticket of its own.
    pub fn due_begin(&mut self) {
        if let Some(jobs) = &self.jobs {
            let _ = jobs.send((self.issued, Job::DueBegin));
        }
    }

    /// What became of what was due, said by the worker with what is served after the edits made of it - those
    /// asked to be taken in so far - and `then` done once it is.
    pub fn due_end(&mut self, answers: Vec<(u64, vfs::due::Answer)>, then: holder::files::Then) {
        match &self.jobs {
            Some(jobs) => drop(jobs.send((self.issued, Job::DueEnd { answers, then }))),
            None => then(),
        }
    }

    /// Take these paths in; returns the job's ticket.
    pub fn paths(&mut self, paths: Vec<PathBuf>) -> u64 {
        self.send(Job::Paths(paths))
    }

    /// Hide these layers from view; returns the job's ticket.
    pub fn hidden(&mut self, names: BTreeSet<String>) -> u64 {
        self.send(Job::Hidden(names))
    }

    /// Show the assets with unsaved changes as their working copies have them; returns the job's ticket.
    pub fn working(&mut self, working: HashMap<String, Working>) -> u64 {
        self.send(Job::Working(working))
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
    /// Take the jobs in, as one, and say what that came to where the project's processes share it - unless edits
    /// are being made of what was due, whose outcome is said with it. None where nothing was asked to be taken in.
    fn take_in(&mut self, jobs: Vec<(u64, Job)>) -> Option<Snapshot> {
        let ticket = jobs.iter().map(|(t, _)| *t).max().unwrap_or(0);
        let mut asked = Vec::new();
        for (at, job) in jobs {
            match job {
                Job::DueBegin => self.unsaid.due += 1,
                Job::DueEnd { answers, then } => {
                    self.unsaid.due = self.unsaid.due.saturating_sub(1);
                    self.unsaid.answers.extend(answers);
                    self.unsaid.then.push(then);
                }
                job => asked.push((at, job)),
            }
        }
        let snapshot = (!asked.is_empty()).then(|| {
            let mut snapshot = self.taken_in(ticket, asked);
            self.unsaid.served.extend(snapshot.served.take());
            snapshot
        });
        if self.unsaid.due == 0 {
            self.say();
        }
        snapshot
    }

    /// Say what was taken in and is not said yet, in one commit: what it changed of the project's model, what is
    /// served after it, what became of what was due - and have the server told, where one is fed.
    fn say(&mut self) {
        use holder::says::Said;
        let _span = tracing::info_span!("take-in worker: what it came to said").entered();
        let unsaid = std::mem::take(&mut self.unsaid);
        // An index read again whole says all it read: what the take-ins before it changed is part of that.
        let model = if unsaid.read_again {
            if let Err(e) = self.says.fill(&self.project.root, &self.index) {
                eprintln!("{e}");
            }
            project::model::Delta::default()
        } else {
            unsaid.model
        };
        let mut served = unsaid.served;
        let said = match served.pop() {
            None => Said::Unchanged,
            // All of it as the last take-in left it: what those before it changed is part of that.
            Some(Served::Whole(listing)) => Said::Whole(listing),
            Some(Served::Changed(last)) => {
                let before: Option<Vec<_>> = served.into_iter().map(|served| if let Served::Changed(delta) = served { Some(delta) } else { None }).collect();
                match before {
                    Some(before) if before.is_empty() => Said::Changed(last),
                    // What each of several take-ins changed, in their order.
                    Some(mut each) => {
                        each.push(last);
                        Said::Changes(each)
                    }
                    // One of them listed all of it, and this one what changed since: all of it, as it is now.
                    None => self.listing.as_ref().map_or(Said::Unchanged, |listing| Said::Whole(Arc::new(listing.clone()))),
                }
            }
        };
        let answers = unsaid.answers;
        let mut thens = unsaid.then;
        let then = (!thens.is_empty()).then(|| Box::new(move || thens.drain(..).for_each(|then| then())) as holder::files::Then);
        let anything = !model.is_empty() || !answers.is_empty() || then.is_some();
        match (&self.files, said) {
            (Some(files), Said::Whole(listing)) => files.show_answering(listing, &model, &answers, then),
            (Some(files), Said::Changed(delta)) => files.show_delta_then(delta, &model, &answers, then),
            (Some(files), Said::Changes(deltas)) => files.show_deltas_then(deltas, &model, &answers, then),
            (Some(files), Said::Unchanged) if anything => files.answer(&model, &answers, then),
            (Some(_), Said::Unchanged) => {}
            // Nothing feeds a server: what changed of the model and what became of what was due, by themselves.
            (None, _) => {
                if anything && let Err(e) = self.says.say(Said::Unchanged, &model, &answers) {
                    eprintln!("what the take-in came to could not be said: {e}");
                }
                if let Some(then) = then {
                    then();
                }
            }
        }
    }

    /// Take the jobs in, as one: the layers hidden as the last asks, the paths of all.
    fn taken_in(&mut self, ticket: u64, jobs: Vec<(u64, Job)>) -> Snapshot {
        let mut paths = Vec::new();
        let mut hidden = None;
        let mut working = None;
        let (mut attach, mut reload, mut whole) = (false, None, false);
        for (_, job) in jobs {
            match job {
                Job::Paths(p) => paths.extend(p),
                Job::Hidden(names) => hidden = Some(names),
                Job::Working(copies) => working = Some(copies),
                Job::Attach => attach = true,
                Job::Reload { why, whole: all_of_it } => {
                    reload = Some(why);
                    whole |= all_of_it;
                }
                // Taken out before this (`take_in`).
                Job::DueBegin | Job::DueEnd { .. } => {}
                #[cfg(any(test, feature = "tests"))]
                Job::Hold(wait) => {
                    let _ = wait.recv();
                    self.reporter()("", 0, 0);
                }
            }
        }
        paths.sort();
        paths.dedup();
        // Files changed and nothing else was asked: what is served differs by what they changed.
        let only_paths = hidden.is_none() && working.is_none() && !attach && reload.is_none();
        let mut all = false;
        if let Some(names) = hidden
            && self.index.set_hidden(&names)
        {
            // Every asset assembled again: their imports attached again.
            let _ = self.imports.attach(&mut self.index, &self.extensions);
            all = true;
        }
        if attach && !all {
            self.attach_all();
            self.reporter()("", 0, 0);
            all = true;
        }
        let mut edited = BTreeSet::new();
        if let Some(working) = working {
            // The assets whose working copies changed, shown as they have them: what their imports make of them too.
            edited = self.index.set_working(working);
            if !edited.is_empty() {
                let _ = self.imports.attach_changed(&mut self.index, &self.extensions, &edited);
                self.sweep_if_due();
            }
        }
        if paths.is_empty() {
            return match reload {
                Some(why) => self.reload(ticket, why, whole),
                None => self.snapshot(ticket, edited, all, None, None),
            };
        }
        let mut refreshed = match self.index.refresh(&self.project, &paths) {
            Ok(refreshed) => refreshed,
            Err(e) => {
                let error = format!("the index could not follow a change: {e:#}");
                return self.snapshot(ticket, BTreeSet::new(), all, Some(error), None);
            }
        };
        // What it changed of the project's model: said with what is served after it (`say`).
        self.unsaid.model.and(std::mem::take(&mut refreshed.model));
        if refreshed.project_file {
            return self.reload(ticket, "project.toml changed", whole);
        }
        // An extension the project holds came, went or changed.
        if extensions::found(&self.project) != self.found {
            return self.reload(ticket, "an extension changed", whole);
        }
        // The assets the refresh assembled again: their imports attached again; the others keep theirs.
        let _ = self.imports.attach_changed(&mut self.index, &self.extensions, &refreshed.assets);
        self.sweep_if_due();
        // What is served follows what the refresh changed - the entries it touched, those gone - where it was
        // listed before and only files changed: listed whole, it costs what the project holds at every change.
        let served = match self.listing.as_mut().filter(|_| only_paths) {
            Some(listing) => {
                let _span = tracing::info_span!("take-in worker: what changed of what is served").entered();
                Some(Served::Changed(self.index.serve_changed(listing, &refreshed)))
            }
            None => None,
        };
        let mut changed = refreshed.assets;
        changed.extend(edited);
        match reload {
            Some(why) => self.reload(ticket, why, whole),
            None => self.snapshot(ticket, changed, all, None, served),
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
        let project = self.project.clone();
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
    fn reload(&mut self, ticket: u64, why: &'static str, whole: bool) -> Snapshot {
        self.reporter()("Reading the project", 0, 0);
        let read = self.read_again(whole);
        let snapshot = match read {
            Ok(changed) => {
                let mut snapshot = self.snapshot(ticket, BTreeSet::new(), changed, None, None);
                let (project, extensions, found) = (self.project.clone(), self.extensions.clone(), self.found.clone());
                snapshot.reloaded = Some(Reloaded { project, extensions, found, why });
                snapshot
            }
            Err(e) => {
                let mut snapshot = self.snapshot(ticket, BTreeSet::new(), false, None, None);
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
        let loaded = Loaded::of(project, index, &self.cache, &self.store, &report);
        self.project = loaded.project;
        self.found = loaded.found;
        if !read && Arc::ptr_eq(&loaded.extensions, &self.extensions) {
            return Ok(false);
        }
        self.index = loaded.index;
        self.extensions = loaded.extensions;
        self.unsaid.read_again |= read;
        self.attach_all();
        Ok(true)
    }

    /// The index as it is now, handed over: the tree built again where anything changed; what is served as
    /// `served` says it changed, or - where nothing says - listed whole.
    fn snapshot(
        &mut self,
        ticket: u64,
        changed: BTreeSet<String>,
        all: bool,
        error: Option<String>,
        served: Option<Served>,
    ) -> Snapshot {
        if all || !changed.is_empty() {
            let _span = tracing::info_span!("take-in worker: the tree built").entered();
            self.tree = Arc::new(Node::build(&self.index, &self.extensions));
        }
        // What is served: a plain file changed is no asset changed, so it is said at every take-in - where a
        // server is fed it.
        let served = served.or_else(|| {
            self.files.as_ref()?;
            let _span = tracing::info_span!("take-in worker: what is served listed").entered();
            let listing = self.index.served();
            self.listing = Some(listing.clone());
            Some(Served::Whole(Arc::new(listing)))
        });
        // What the job freed, given back: the worker's threads each keep what they freed otherwise.
        project::give_back_memory();
        let shown = Arc::new(self.index.clone());
        self.shown = Some(shown.clone());
        Snapshot {
            ticket,
            index: shown,
            tree: self.tree.clone(),
            layers: self.index.layers(),
            changed,
            all,
            reloaded: None,
            said: None,
            error,
            files: false,
            served,
        }
    }
}
