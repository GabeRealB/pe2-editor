//! Holding a project, as an editor and a host both do it: one thread has the project's only index that changes, and
//! does all that changes it in the order it was asked (`Queue`) - the edits commands, agents and programs writing
//! through the served folder ask for, what is due of those, what changed in the layers by other means - and after
//! each says what it came to where the project's processes share it, in one commit (`Core`): what it changed of
//! the project's model, what is served after it, what became of what was due.
//!
//! Whoever asks never has the index: it gives work to the thread and waits for what that came to (`Queue::call`),
//! or does not (`Queue::post`). So nothing asked reads a project that lacks what was asked before it, and nobody
//! waits for anything but its own turn.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread::JoinHandle;

use project::Project;
use project::history::History;
use project::index::{Index, Refreshed};
use project::server::Listing;

use crate::files::Files;
use crate::says::{Answers, Said, Says};

/// Work for the thread that owns `S`, or that there is no more.
enum Work<S> {
    Do(Box<dyn FnOnce(&mut S) + Send>),
    End,
}

/// A thread that owns something and does what it is given with it, in the order it was given.
pub struct Queue<S> {
    work: mpsc::Sender<Work<S>>,
    thread: Option<JoinHandle<()>>,
}

impl<S: Send + 'static> Queue<S> {
    /// Start the thread, named `name`, owning `state`.
    ///
    /// # Errors
    /// No thread can be started.
    pub fn start(name: &str, mut state: S) -> std::io::Result<Queue<S>> {
        let (work, given) = mpsc::channel::<Work<S>>();
        let thread = std::thread::Builder::new().name(name.to_string()).spawn(move || {
            while let Ok(Work::Do(work)) = given.recv() {
                work(&mut state);
            }
        })?;
        Ok(Queue { work, thread: Some(thread) })
    }

    /// Have this done after all that was given before it; nobody waits for it.
    pub fn post(&self, work: impl FnOnce(&mut S) + Send + 'static) {
        let _ = self.work.send(Work::Do(Box::new(work)));
    }

    /// Have this done after all that was given before it, and wait for what it comes to: none where the thread
    /// is gone - ended, or what it was doing panicked. Never asked from the thread itself: it would wait for
    /// itself.
    pub fn call<R: Send + 'static>(&self, work: impl FnOnce(&mut S) -> R + Send + 'static) -> Option<R> {
        let (done, made) = mpsc::channel();
        self.post(move |state| drop(done.send(work(state))));
        made.recv().ok()
    }
}

impl<S> Drop for Queue<S> {
    /// What was given is done, and the thread ends: waited for, but where it is the one letting go.
    fn drop(&mut self) {
        let _ = self.work.send(Work::End);
        if let Some(thread) = self.thread.take()
            && thread.thread().id() != std::thread::current().id()
        {
            let _ = thread.join();
        }
    }
}

/// What tells a file's version from another: its time and length; none where it is gone.
type Stamp = (std::time::SystemTime, u64);

fn stamp(path: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

/// How many files written are remembered as the holder's own before all are forgotten: forgotten, one is taken in
/// once more when what watches the layers tells of it, which changes nothing.
const WRITTEN_KEPT: usize = 10_000;

/// A project held: its index, and what is said of it where the project's processes share it. Owned by one
/// thread (`Queue`), which does all that changes it.
pub struct Core {
    pub project: Project,
    pub index: Index,
    /// Where it says what it holds - the project's model, what is served, what became of what was due - a
    /// commit a refresh, whether or not a server is fed.
    pub says: Arc<Says>,
    /// What tells the server serving the project's folder that something was said, where one is fed.
    pub files: Option<Arc<Files>>,
    /// Told each step made, by its label.
    pub made: Box<dyn Fn(&str) + Send>,
    /// What is served, as last listed for the server fed - none until all of it was: brought up to each change,
    /// to say what differs.
    pub listing: Option<Listing>,
    /// The files its own edits wrote, as they were then: reported again as they are - by what watches the layers,
    /// by the program that asked for the edit - they are taken in already.
    written: HashMap<PathBuf, Option<Stamp>>,
    /// What was due and taken, by its number, until it is due no more: taken once, however often it is asked
    /// to look while what became of it could not be said.
    due_taken: HashSet<u64>,
    /// The thread holding the files to what the index took of them as known, until it is done and what it found
    /// is taken in.
    verifying: Option<JoinHandle<Vec<PathBuf>>>,
}

impl Core {
    /// Of an index just opened.
    #[must_use]
    pub fn new(project: Project, index: Index, says: Arc<Says>) -> Core {
        Core {
            project,
            index,
            says,
            files: None,
            made: Box::new(|_| {}),
            listing: None,
            written: HashMap::new(),
            due_taken: HashSet::new(),
            verifying: None,
        }
    }

    /// What is done of an index just opened as the project is taken: what the opening read is said in the
    /// project's model, where its processes share it; and the files, which it took to be as the model has them
    /// (`Index::open_behind`), are held to that on a thread of its own - `wake` called once it is done, for
    /// whoever owns this to have what differs taken in (`verified`), as the first edit does whether or not it
    /// was.
    pub fn took(&mut self, wake: impl FnOnce() + Send + 'static) {
        if let Err(e) = self.says.took(&self.project.root, &mut self.index) {
            eprintln!("{e}");
        }
        let verify = self.index.verification();
        let verifying = std::thread::Builder::new().name("verify".into()).spawn(move || {
            let differing = verify();
            wake();
            differing
        });
        self.verifying = verifying.ok();
    }

    /// Wait until the files were held to what the index took of them as known (`took`), and take in those that
    /// were not, as a change of files is: what an edit does first. Whether anything was taken in.
    pub fn verified(&mut self) -> bool {
        let Some(verifying) = self.verifying.take() else { return false };
        let differing = verifying.join().unwrap_or_default();
        if differing.is_empty() {
            return false;
        }
        eprintln!("{} files were not as they were known: taken in", differing.len());
        self.take_in(&differing)
    }

    /// Feed a server what is served, from now on: all the index lists first, kept to tell what differs after
    /// each change.
    pub fn feed_whole(&mut self, files: Arc<Files>) {
        let listing = self.index.served();
        files.show(Arc::new(listing.clone()));
        self.listing = Some(listing);
        self.files = Some(files);
    }

    /// Take what is due where the project's processes share it (`vfs::due`) - what programs did through the
    /// served folder - and make edits of it, in its order; what became of each is said there in the commit that
    /// says what is served after them.
    pub fn take_due(&mut self, history: &mut History) -> mcp::Reply {
        let due = project::shared::space(&self.project.root).and_then(|space| vfs::due::due(&space.read()?));
        let due = match due {
            Ok(due) => due,
            Err(e) => return mcp::Reply::Error(format!("what is due could not be read: {e}")),
        };
        self.due_taken.retain(|number| due.iter().any(|(due, _)| due == number));
        let (numbers, ops): (Vec<u64>, Vec<vfs::ops::Op>) =
            due.into_iter().filter(|(number, _)| !self.due_taken.contains(number)).unzip();
        if numbers.is_empty() {
            return mcp::Reply::Taken { count: 0 };
        }
        let made = self.edits(history, &ops, None, Some(&numbers));
        self.due_taken.extend(numbers);
        mcp::Reply::Taken { count: made.len() }
    }

    /// Make edits, in order: each a step of the history, and what follows from them - the index, what is
    /// served - brought up to date once for all of them, which is most of what one edit takes. How each went.
    /// Where they are what was due, by these numbers, what became of each is said where it was due, with what
    /// is served after them - and one that is so already (`Op::so_already`: taken a second time) is made no
    /// edit of.
    pub fn edits(
        &mut self,
        history: &mut History,
        ops: &[vfs::ops::Op],
        layer: Option<&str>,
        due: Option<&[u64]>,
    ) -> Vec<mcp::Reply> {
        let failed = |why: String| ops.iter().map(|_| mcp::Reply::failed(why.clone())).collect::<Vec<_>>();
        // What became of each, for where it was due.
        let answers = |replies: &[mcp::Reply]| -> Vec<(u64, vfs::due::Answer)> {
            let became = replies.iter().map(|reply| match reply {
                mcp::Reply::Edited { label, .. } => Ok(label.clone()),
                other => Err(other.told().err().unwrap_or_else(|| "it answered with something else".into())),
            });
            due.map_or_else(Vec::new, |due| due.iter().copied().zip(became).collect())
        };
        // Over the files as they are: held first to what the index took of them as known.
        self.verified();
        let Some(layer) = layer.map(str::to_string).or_else(|| vfs::write::writable_layer(&self.index)) else {
            let replies = failed("no layer of the project can be written".into());
            self.answer_due(&answers(&replies));
            return replies;
        };
        // What the edits wrote that the index has not taken in yet, and all of it.
        let (mut pending, mut changed): (Vec<PathBuf>, Vec<PathBuf>) = (Vec::new(), Vec::new());
        let mut replies = Vec::with_capacity(ops.len());
        // What the refreshes of all of them changed, together.
        let mut all = Refreshed::default();
        // The paths the edits made and not taken in yet named: one that names a path at, in or above one of them
        // is made over the project as they left it.
        let mut earlier: Vec<String> = Vec::new();
        for op in ops {
            if !pending.is_empty()
                && op.follows(&earlier)
                && let Ok(refreshed) = self.index.refresh(&self.project, &pending)
            {
                all.and(refreshed);
                changed.append(&mut pending);
                earlier.clear();
            }
            earlier.extend(op.paths().into_iter().map(str::to_string));
            let project = &self.project;
            let mut apply = |index: &Index| {
                let _span = tracing::info_span!("holder edit apply").entered();
                vfs::ops::apply(op, project, index, &layer, history)
            };
            let mut made = apply(&self.index);
            // What it could not be made over may be what the ones before it made: taken in, and tried once more.
            if made.is_err()
                && !pending.is_empty()
                && let Ok(refreshed) = self.index.refresh(&self.project, &pending)
            {
                all.and(refreshed);
                changed.append(&mut pending);
                made = apply(&self.index);
            }
            // Due, not made, and so already - taken before, by a holder that ended before it said so, or said due
            // again by a serving after one that ended: it is as asked, and said so.
            if due.is_some() && made.is_err() && op.so_already(&self.index) {
                made = Ok(("it is so already".to_string(), Vec::new()));
            }
            replies.push(match made {
                Ok((label, touched)) => {
                    pending.extend(touched.iter().cloned());
                    pending.extend(history.take_written());
                    (self.made)(&label);
                    mcp::Reply::Edited { label, touched }
                }
                Err(e) => mcp::Reply::failed(format!("{e:#}")),
            });
        }
        if pending.is_empty() && changed.is_empty() {
            self.answer_due(&answers(&replies));
            return replies;
        }
        let refreshed = {
            let _span = tracing::info_span!("holder edit refresh").entered();
            self.index.refresh(&self.project, &pending)
        };
        changed.append(&mut pending);
        match refreshed {
            Ok(refreshed) => all.and(refreshed),
            Err(e) => {
                let replies = failed(format!("{e:#}"));
                self.answer_due(&answers(&replies));
                return replies;
            }
        }
        // Answered once the project's files have what was asked: whoever reads them next by other means - a
        // command after this one, a tool - finds them so. All that was asked together is had on disk together,
        // and each file is as its own writing left it when what watches the layers tells of it.
        history.settle();
        if self.written.len() > WRITTEN_KEPT {
            self.written.clear();
        }
        for path in &changed {
            self.written.insert(path.clone(), stamp(path));
        }
        self.feed(&changed, &all, &answers(&replies));
        project::give_back_memory();
        replies
    }

    /// Take in what changed in the layers by other means than its own edits: the index refreshed, what is served
    /// and the model said as they are after it. Whether anything was: what its own edits wrote, as they wrote
    /// it, it has taken in already.
    pub fn take_in(&mut self, changed: &[PathBuf]) -> bool {
        let written = &mut self.written;
        let changed: Vec<PathBuf> = changed
            .iter()
            .filter(|path| {
                // Written over since, it is forgotten.
                let taken = written.get(*path).is_some_and(|was| *was == stamp(path));
                if !taken {
                    written.remove(*path);
                }
                !taken
            })
            .cloned()
            .collect();
        if changed.is_empty() {
            return false;
        }
        match self.index.refresh(&self.project, &changed) {
            Ok(refreshed) => {
                self.feed(&changed, &refreshed, &[]);
                project::give_back_memory();
                true
            }
            Err(e) => {
                eprintln!("taking in what changed: {e:#}");
                false
            }
        }
    }

    /// Say what became of what was due with nothing of what is served changed for it: through what feeds the
    /// server, after all it was given to say before, or in the shared filesystem itself where nothing feeds one.
    fn answer_due(&self, answers: &Answers) {
        if answers.is_empty() {
            return;
        }
        let unchanged = project::model::Delta::default();
        if let Some(files) = &self.files {
            return files.show_changed_answering(Vec::new(), None, &unchanged, answers);
        }
        if let Err(e) = self.says.say(Said::Unchanged, &unchanged, answers) {
            eprintln!("what became of what was due could not be said: {e}");
        }
    }

    /// What is served follows what changed, where a server is fed: told which files changed, for what it reads of
    /// them to be read again, then what differs of what is served - of the entries the refresh touched and those
    /// gone - and waited for, so that who asked for a change finds it in the folder once answered. What became of
    /// what was due (`answers`) is said in the same commit, as is what the refresh changed of the model.
    fn feed(&mut self, changed: &[PathBuf], refreshed: &Refreshed, answers: &Answers) {
        let _span = tracing::info_span!("holder served update").entered();
        if let Some(files) = &self.files {
            if let Some(listing) = self.listing.as_mut().filter(|_| !refreshed.project_file) {
                let delta = self.index.serve_changed(listing, refreshed);
                files.show_delta_answering(changed.to_vec(), delta, &refreshed.model, answers);
            } else {
                // `project.toml` changed: the layers themselves may have. All of it, listed again.
                let listing = self.index.served();
                self.listing = Some(listing.clone());
                files.show_changed_answering(changed.to_vec(), Some(Arc::new(listing)), &refreshed.model, answers);
            }
            return;
        }
        // Nothing feeds a server: what changed of the model and what became of what was due, by themselves.
        if let Err(e) = self.says.say(Said::Unchanged, &refreshed.model, answers) {
            eprintln!("what the change came to could not be said: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Queue;

    #[test]
    fn what_is_given_is_done_in_its_order_and_what_is_asked_waits_for_what_came_before() {
        let queue = Queue::start("queue", Vec::<u32>::new()).unwrap();
        for n in 0..100 {
            queue.post(move |done| done.push(n));
        }
        let done = queue.call(|done| done.clone()).unwrap();
        assert_eq!(done, (0..100).collect::<Vec<u32>>());
        // What it is given from several threads at once is each done whole.
        std::thread::scope(|scope| {
            for thread in 0..4u32 {
                let queue = &queue;
                scope.spawn(move || {
                    for n in 0..50 {
                        let len = queue.call(move |done| {
                            done.push(1000 + thread * 100 + n);
                            done.len()
                        });
                        assert!(len.unwrap() > 100);
                    }
                });
            }
        });
        assert_eq!(queue.call(|done| done.len()), Some(300));
    }

    #[test]
    fn a_thread_that_is_gone_answers_nothing() {
        let queue = Queue::start("queue", ()).unwrap();
        let gone: Option<()> = queue.call(|()| panic!("what it was doing failed"));
        assert_eq!(gone, None);
        assert_eq!(queue.call(|()| 1), None, "and nothing after it");
    }

    #[test]
    fn let_go_of_by_its_own_thread_it_does_not_wait_for_itself() {
        let queue = std::sync::Arc::new(Queue::start("queue", ()).unwrap());
        let last = queue.clone();
        queue.post(move |()| {
            // The other holder is gone by the time this runs, or goes right after: either way this ends.
            std::thread::sleep(std::time::Duration::from_millis(20));
            drop(last);
        });
        drop(queue);
    }
}
