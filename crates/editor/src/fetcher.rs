//! What the project's extensions fetch onto this machine (`project::ext::setup`), fetched beside the editor ahead of
//! what needs it: an operation of an extension waits for what it requires as it runs (`have_required`), on the fetch
//! here where one is under way. Trusting an extension is consenting to it, so what it requires starts once it runs,
//! unasked; what is optional, when asked for
//! (Fetch now in the Extensions pane - the extension asks itself as it runs). One package at a time; how each goes is
//! shown in the status bar and the pane. What failed waits to be tried again, from the pane. Between fetches, the store
//! is swept where a sweep is due (`setup::kept`), keeping what the project's extensions name.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use project::ext::Extensions;
use project::ext::setup::Setup;

/// How a package's fetch stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// Bytes downloaded, of how many where the server said.
    Fetching(u64, Option<u64>),
    Failed(String),
}

/// A fetch beside the editor: the package, and how it stands.
struct Running {
    id: String,
    setup: Setup,
    state: Arc<Mutex<State>>,
    done: Arc<AtomicBool>,
}

/// What is fetched beside the editor, and what failed.
#[derive(Default)]
pub struct Fetcher {
    running: Option<Running>,
    /// Asked for, to fetch next: each with its extension and its folder, where its patches are.
    queued: VecDeque<(String, Option<PathBuf>, Setup)>,
    /// What failed, by key, with why: tried again only when asked.
    failed: BTreeMap<String, String>,
    /// When this asked last whether the store is due a sweep.
    swept: Option<std::time::Instant>,
}

/// How often the editor asks whether the store is due a sweep, which the store's own stamp answers.
const SWEEP_ASKED: std::time::Duration = std::time::Duration::from_mins(5);

impl Fetcher {
    /// Go on: a fetch done is taken in, and the next started - what the extensions running require first.
    pub fn tick(&mut self, extensions: &Extensions, ctx: &eframe::egui::Context) {
        if let Some(running) = self.running.take_if(|r| r.done.load(std::sync::atomic::Ordering::Relaxed)) {
            let state = running.state.lock().map(|state| state.clone()).ok();
            if let Some(State::Failed(why)) = state {
                self.failed.insert(running.setup.key(), why);
            }
        }
        if self.running.is_some() {
            return;
        }
        let Some(store) = extensions::setup_folder() else { return };
        if self.swept.is_none_or(|at| at.elapsed() >= SWEEP_ASKED) {
            self.swept = Some(std::time::Instant::now());
            let (keep, store) = (extensions::named_in_store(extensions), store.clone());
            std::thread::spawn(move || project::ext::setup::kept::sweep_if_due(&store, &keep));
        }
        let failed = &self.failed;
        let wanted = |setup: &Setup| !setup.fetched(&store) && !failed.contains_key(&setup.key());
        let required = extensions.present.iter().find_map(|present| {
            let setup = extensions::required(present).into_iter().find(|setup| wanted(setup))?;
            Some((present.id.clone(), present.origin.folder().map(PathBuf::from), setup.clone()))
        });
        let next = match required {
            Some(next) => Some(next),
            None => loop {
                match self.queued.pop_front() {
                    Some((id, folder, setup)) if !setup.fetched(&store) => break Some((id, folder, setup)),
                    Some(_) => {}
                    None => break None,
                }
            },
        };
        let Some((id, folder, setup)) = next else { return };
        let state = Arc::new(Mutex::new(State::Fetching(0, None)));
        let done = Arc::new(AtomicBool::new(false));
        let (shared, finished, ctx, fetched) = (state.clone(), done.clone(), ctx.clone(), setup.clone());
        std::thread::spawn(move || {
            let set = |next: State| {
                if let Ok(mut state) = shared.lock() {
                    *state = next;
                }
                ctx.request_repaint();
            };
            let mut progress = |done, total| set(State::Fetching(done, total));
            let cancel = AtomicBool::new(false);
            if let Err(e) = project::ext::setup::fetch(&fetched, &store, folder.as_deref(), &mut progress, &cancel) {
                set(State::Failed(format!("{e:#}")));
            }
            finished.store(true, std::sync::atomic::Ordering::Relaxed);
            ctx.request_repaint();
        });
        self.running = Some(Running { id, setup, state, done });
    }

    /// Fetch something optional now: what an extension would fetch when it asks for it, its patches in its folder.
    pub fn fetch_now(&mut self, id: &str, folder: Option<&std::path::Path>, setup: &Setup) {
        self.failed.remove(&setup.key());
        self.queued.push_back((id.to_string(), folder.map(PathBuf::from), setup.clone()));
    }

    /// Try what failed again.
    pub fn retry(&mut self, setup: &Setup) {
        self.failed.remove(&setup.key());
    }

    /// How a package's fetch stands, where it runs or failed.
    #[must_use]
    pub fn state_of(&self, setup: &Setup) -> Option<State> {
        let running = self.running.as_ref().filter(|running| running.setup == *setup);
        let state = running.and_then(|running| running.state.lock().ok().map(|state| state.clone()));
        state.or_else(|| self.failed.get(&setup.key()).map(|why| State::Failed(why.clone())))
    }

    /// What it is doing, in a few words, for the status bar; and what failed.
    #[must_use]
    pub fn status(&self) -> Option<(String, bool)> {
        if let Some(running) = &self.running {
            let state = running.state.lock().ok().map(|state| state.clone());
            if let Some(State::Fetching(done, total)) = state {
                let of = match total {
                    Some(total) if total > 0 => format!(", {}%", done * 100 / total),
                    _ => String::new(),
                };
                return Some((format!("{}: fetching {}{of}", running.id, running.setup.name), false));
            }
        }
        let n = self.failed.len();
        (n > 0).then(|| (format!("{n} fetch{} failed", if n == 1 { "" } else { "es" }), true))
    }
}
