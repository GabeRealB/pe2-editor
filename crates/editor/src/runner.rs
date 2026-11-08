//! The epoch worker: what events set off, off the UI thread. Each job is an epoch on the project as last taken in, one at a time, in the order asked, so a clean and the build
//! after it stay two: only batches of files waiting behind each other are one, as they mean the same. It wakes the window when one ends.
//!
//! It runs the generated layers' plans too (`extract`): the work graph is theirs alone while they run, so an extraction
//! waits for the epoch running to end, and what is asked after it waits for it.

use eframe::egui;
use pe2_ext::events::{Build, Clean, Event, FilesChanged};
use project::events::Report;
use project::ext::Extensions;
use project::index::Index;
use project::{Config, Project};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;

/// An epoch to run.
pub struct Job {
    pub roots: Vec<String>,
    pub index: Arc<Index>,
    pub config: Config,
    /// Who waits for it, over the editor's link.
    pub reply: Option<Sender<mcp::Reply>>,
}

/// A generated layer's plan to run, as the work graph's only work.
pub struct Extraction {
    pub layer: project::Layer,
    pub progress: Arc<project::plan::Progress>,
    /// Set once it runs: until then, it waits for the epoch before it.
    pub started: Arc<AtomicBool>,
    pub done: Sender<anyhow::Result<project::plan::Ran>>,
}

/// What the worker is given.
enum Work {
    Epoch(Job),
    Extract(Extraction),
}

/// An epoch as it ended.
pub struct Finished {
    pub roots: Vec<String>,
    pub config: String,
    pub report: Report,
    /// What could not be wired.
    pub problems: Vec<(String, String)>,
    /// How much is wired to each event.
    pub wired: BTreeMap<String, usize>,
}

impl Finished {
    /// What to say of it, where someone asked for it.
    pub fn said(&self) -> Option<String> {
        let asked = |event: &str| self.roots.iter().any(|root| root == event);
        let what = if asked(Build::NAME) {
            "Built"
        } else if asked(Clean::NAME) {
            "Cleaned"
        } else {
            return None;
        };
        Some(match self.report.failed.first() {
            _ if self.report.cancelled => "Cancelled".to_string(),
            Some((path, _)) => format!("{path} failed: see Problems"),
            None => format!("{what} {}: {} made", self.config, self.report.made.len()),
        })
    }

    /// Whether it left something to be told.
    pub fn is_wrong(&self) -> bool {
        !(self.problems.is_empty() && self.report.failed.is_empty() && self.report.stale.is_empty())
    }
}

/// The worker, as the UI holds it.
pub struct Runner {
    jobs: Option<Sender<Work>>,
    finished: Receiver<Finished>,
    /// Stops the epoch running at its next step.
    cancel: Arc<AtomicBool>,
    /// The epochs asked for and not ended.
    waiting: Arc<AtomicUsize>,
    /// The events the epoch running was signaled by, while one runs.
    running: Arc<std::sync::Mutex<Vec<String>>>,
    ctx: Arc<OnceLock<egui::Context>>,
    thread: Option<JoinHandle<()>>,
}

/// Whether a job can be run as part of the next: they say the same thing of the same config.
fn same(job: &Job, next: &Job) -> bool {
    let files = |job: &Job| job.reply.is_none() && matches!(job.roots.as_slice(), [one] if one == FilesChanged::NAME);
    files(job) && files(next) && job.config == next.config
}

fn text(report: &Report) -> mcp::Reply {
    let mut lines: Vec<String> = report.made.iter().map(|(asset, output)| format!("{asset}: {output}")).collect();
    let failed = report.failed.iter().chain(&report.notes).map(|(asset, why)| format!("{asset}: {why}"));
    let stale = report
        .stale
        .iter()
        .map(|(asset, output)| format!("{asset}: {output} is stale: what it reads changed after it ran"));
    lines.extend(failed.chain(stale));
    let answer = lines.join("\n");
    if report.failed.is_empty() && !report.cancelled { mcp::Reply::text(answer) } else { mcp::Reply::failed(answer) }
}

impl Runner {
    pub fn start(project: Project, extensions: Arc<Extensions>) -> Runner {
        Runner::after(None, project, extensions)
    }

    /// Stop the epoch running and go on with this project and these extensions. An epoch that has yet to stop writes what
    /// it was writing: the next one starts once it has, and the window does not wait for that.
    pub fn restart(&mut self, project: Project, extensions: Arc<Extensions>) {
        self.cancel();
        self.jobs = None;
        *self = Runner::after(self.thread.take(), project, extensions);
    }

    fn after(before: Option<JoinHandle<()>>, project: Project, extensions: Arc<Extensions>) -> Runner {
        let (jobs, queued) = mpsc::channel::<Work>();
        let (done, finished) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let waiting = Arc::new(AtomicUsize::new(0));
        let running: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
        let ctx: Arc<OnceLock<egui::Context>> = Arc::default();
        let (stop, left, wake, now) = (cancel.clone(), waiting.clone(), ctx.clone(), running.clone());
        let thread = std::thread::Builder::new()
            .name("epochs".into())
            .spawn(move || {
                if let Some(before) = before {
                    let _ = before.join();
                }
                let mut held: Option<Work> = None;
                loop {
                    let mut job = match held.take().or_else(|| queued.recv().ok()) {
                        None => return,
                        Some(Work::Extract(extraction)) => {
                            extract(&project, &extensions, &extraction);
                            if let Some(ctx) = wake.get() {
                                ctx.request_repaint();
                            }
                            continue;
                        }
                        Some(Work::Epoch(job)) => job,
                    };
                    for next in queued.try_iter() {
                        match next {
                            Work::Epoch(next) if same(&job, &next) => {
                                left.fetch_sub(1, Ordering::Relaxed);
                                job = next;
                            }
                            next => {
                                held = Some(next);
                                break;
                            }
                        }
                    }
                    stop.store(false, Ordering::Relaxed);
                    job.roots.clone_into(&mut now.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
                    let wiring = project::build::wiring(&job.index, &extensions, &job.config);
                    // What the user signaled: a group of its own, over what the editor does by itself.
                    let group =
                        project::schedule::Group::new(project::schedule::Class::Commanded, &job.roots.join(" "));
                    let report = group.enter(|| {
                        project::build::epoch(
                            &project,
                            &job.index,
                            &extensions,
                            &wiring,
                            &job.config,
                            &job.roots,
                            &stop,
                        )
                    });
                    if let Some(reply) = &job.reply {
                        let _ = reply.send(text(&report));
                    }
                    left.fetch_sub(1, Ordering::Relaxed);
                    now.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
                    let wired = wiring.counts();
                    let ended = Finished {
                        roots: job.roots,
                        config: job.config.name,
                        report,
                        problems: wiring.problems,
                        wired,
                    };
                    if done.send(ended).is_err() {
                        return;
                    }
                    if let Some(ctx) = wake.get() {
                        ctx.request_repaint();
                    }
                }
            })
            .ok();
        Runner { jobs: Some(jobs), finished, cancel, waiting, running, ctx, thread }
    }

    /// The window to wake when an epoch ends.
    pub fn wake(&self, ctx: &egui::Context) {
        let _ = self.ctx.set(ctx.clone());
    }

    pub fn run(&self, job: Job) {
        if let Some(jobs) = &self.jobs {
            self.waiting.fetch_add(1, Ordering::Relaxed);
            let _ = jobs.send(Work::Epoch(job));
        }
    }

    /// Run a generated layer's plan once the epochs asked before it have ended, nothing else running meanwhile.
    pub fn extract(&self, extraction: Extraction) {
        if let Some(jobs) = &self.jobs {
            let _ = jobs.send(Work::Extract(extraction));
        }
    }

    /// The epochs that ended since the last poll.
    pub fn poll(&self) -> Vec<Finished> {
        self.finished.try_iter().collect()
    }

    pub fn busy(&self) -> bool {
        self.waiting.load(Ordering::Relaxed) > 0
    }

    /// The events the epoch running was signaled by; none between epochs.
    pub fn running(&self) -> Vec<String> {
        self.running.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    /// Stop the epoch running, once what it is running now is done.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Run a plan, as the commands a user gives: what it made, or why not, to whoever asked.
fn extract(project: &Project, extensions: &Extensions, extraction: &Extraction) {
    extraction.started.store(true, Ordering::Relaxed);
    let group = project::schedule::Group::new(project::schedule::Class::Commanded, "extract");
    let ran = group.enter(|| {
        project::plan::run(
            project,
            &extraction.layer,
            &extensions.engine(),
            extensions,
            &extraction.progress,
            &project::plan::Releases::Available,
        )
    });
    let _ = extraction.done.send(ran);
}

impl Drop for Runner {
    fn drop(&mut self) {
        self.cancel();
        self.jobs = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
