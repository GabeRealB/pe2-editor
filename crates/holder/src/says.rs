//! What whoever holds a project says of it where its processes share it (`project::shared::space`), a commit a
//! refresh, by the thread that refreshed: what the refresh changed of the project's model (`project::model`),
//! what is served after it (`project::shared`), and what became of what was due and is the project's by it
//! (`vfs::due`) - together, so that the folder shows a thing the project's, the server knows it taken and the
//! model holds it in one moment, and nothing of a refresh is said where the refresh failed. Whether a server is
//! fed makes no difference to it: what feeds one (`files::Files`) only tells it that something was said.

use std::sync::{Arc, Mutex, PoisonError};

use project::server::{Delta, Listing};

/// What is served now, as whoever holds the project says it.
pub enum Said {
    /// All of it.
    Whole(Arc<Listing>),
    /// What of it differs from what was said before.
    Changed(Delta),
    /// What of it differed after each of several refreshes, in their order.
    Changes(Vec<Delta>),
    /// As it was.
    Unchanged,
}

/// What became of something that was due, by its number.
pub type Answers = [(u64, vfs::due::Answer)];

/// Where a holder says what it holds, for as long as it holds it.
pub struct Says {
    shared: Result<Arc<sfs::Space>, String>,
    /// What is served now, as last said here: none until all of it was, and after `forget`.
    current: Mutex<Option<Listing>>,
}

impl Says {
    /// In the space the project's processes share, attached here where it is not yet.
    #[must_use]
    pub fn of(project: &project::Project) -> Says {
        let shared = project::shared::space(&project.root).map_err(|e| format!("the space the project's processes share: {e}"));
        Says { shared, current: Mutex::default() }
    }

    fn current(&self) -> std::sync::MutexGuard<'_, Option<Listing>> {
        self.current.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether all that is served was said here: until then nothing said is part of anything.
    #[must_use]
    pub fn said_whole(&self) -> bool {
        self.current().is_some()
    }

    /// What was said here is no longer what the space holds for sure - another held the project since: all of it
    /// is said again, by what differs from what the space holds then.
    pub fn forget(&self) {
        *self.current() = None;
    }

    /// Have the model hold what an index read of the project's files, and nothing else of them: what whoever
    /// takes a project says first, by what differs from what the space kept.
    ///
    /// # Errors
    /// The space is not there, or cannot be written.
    pub fn fill(&self, root: &std::path::Path, index: &project::index::Index) -> Result<(), String> {
        let _span = tracing::info_span!("model: filled with what the index read").entered();
        let space = self.shared.as_ref().map_err(Clone::clone)?;
        let filled = space.write().and_then(|mut txn| {
            project::model::fill_files(&mut txn, root, index.file_records())?;
            txn.commit()
        });
        filled.map_err(|e| format!("the project's model could not be said in the shared space: {e}"))
    }

    /// Say, in one commit: what a refresh changed of the model, what is served after it, and what became of what
    /// was due.
    ///
    /// # Errors
    /// The space is not there, or cannot be written: nothing of it is said.
    pub fn say(&self, said: Said, model: &project::model::Delta, answers: &Answers) -> Result<(), String> {
        let space = self.shared.as_ref().map_err(Clone::clone)?;
        // First in the commit, before what is served: a move the project took is one its paths are read
        // through no more (`vfs::due::answer`).
        let mut also = |txn: &mut sfs::space::Txn<'_>| {
            model.apply(txn)?;
            Ok(vfs::due::answer(txn, answers)?)
        };
        let alone = |also: &mut dyn FnMut(&mut sfs::space::Txn<'_>) -> sfs::space::Result<()>| {
            let mut txn = space.write()?;
            also(&mut txn)?;
            txn.commit()
        };
        let mut current = self.current();
        let said = match (said, &mut *current) {
            (Said::Whole(listing), Some(before)) => {
                let delta = Delta::between(before, &listing);
                *before = Arc::unwrap_or_clone(listing);
                if delta.is_empty() { alone(&mut also) } else { project::shared::say_with(space, &delta, &mut also) }
            }
            (Said::Whole(listing), None) => {
                let _span = tracing::info_span!("served: said whole in the shared filesystem").entered();
                let said = project::shared::say_whole_with(space, &listing, &mut also);
                *current = Some(Arc::unwrap_or_clone(listing));
                said
            }
            (Said::Changed(delta), Some(before)) => {
                delta.apply(before);
                project::shared::say_with(space, &delta, &mut also)
            }
            (Said::Changes(deltas), Some(before)) => {
                for delta in &deltas {
                    delta.apply(before);
                }
                project::shared::say_each_with(space, &deltas, &mut also)
            }
            // Part of nothing: whoever holds the project says all of it first.
            (Said::Changed(_) | Said::Changes(_) | Said::Unchanged, _) => alone(&mut also),
        };
        said.map_err(|e| format!("what is served could not be said in the shared filesystem: {e}"))
    }

    /// Nothing more is said here: the space is left as whoever next attaches alone may trust it.
    pub fn rest(&self) {
        if let Ok(space) = &self.shared
            && let Err(e) = space.rest()
        {
            eprintln!("the shared space is not left to be trusted: {e}");
        }
    }
}
