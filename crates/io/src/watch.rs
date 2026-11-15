//! What changes on disk, told as it changes: places watched, each change said to a callback, until the watching
//! returned is dropped. The system's tells what its watcher sees (`notify`: inotify, `ReadDirectoryChangesW`,
//! `FSEvents`); [`Telling`] tells what a test says.

use crate::HostPath;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError, Weak};

/// How much of a folder is watched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Depth {
    /// It and all in it, at any depth.
    All,
    /// Its own entries.
    Own,
}

/// What became of what changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    /// It was read: no change.
    Read,
    /// It was made.
    Made,
    /// Its content or what is said of it changed - of a folder, what came or went in it.
    Changed,
    /// It was renamed: its old name, its new one, or both.
    Renamed,
    /// It was removed.
    Removed,
    /// Something the system does not say.
    Other,
}

/// A change: what became of these places.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub kind: ChangeKind,
    /// The places, in the form of the place watched they are in.
    pub paths: Vec<PathBuf>,
}

/// What is told each change, or that watching failed (the system's limit of watches, which leaves changes unseen).
pub type Tell = Box<dyn Fn(io::Result<Change>) + Send + Sync>;

/// What watches places on disk.
pub trait Watch: Send + Sync {
    /// Watch these places, links unfollowed, each change told to `tell` as it comes - on a thread of the watcher's -
    /// until what this returns is dropped.
    ///
    /// # Errors
    /// A place cannot be watched: it is not there, or the system's limit of watches is reached.
    fn watch(&self, places: &[(HostPath, Depth)], tell: Tell) -> io::Result<Box<dyn Watching>>;
}

/// Places being watched: no longer, once dropped.
pub trait Watching: Send {}

/// The system's watcher.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemWatch;

struct SystemWatching(#[allow(dead_code, reason = "held: watching ends as it is dropped")] notify::RecommendedWatcher);

impl Watching for SystemWatching {}

impl Watch for SystemWatch {
    fn watch(&self, places: &[(HostPath, Depth)], tell: Tell) -> io::Result<Box<dyn Watching>> {
        use notify::Watcher as _;
        let handler = move |event: notify::Result<notify::Event>| tell(event.map(change_of).map_err(io_error));
        let config = notify::Config::default().with_follow_symlinks(false);
        let mut watcher = notify::RecommendedWatcher::new(handler, config).map_err(io_error)?;
        for (place, depth) in places {
            let mode = match depth {
                Depth::All => notify::RecursiveMode::Recursive,
                Depth::Own => notify::RecursiveMode::NonRecursive,
            };
            watcher.watch(place, mode).map_err(io_error)?;
        }
        Ok(Box::new(SystemWatching(watcher)))
    }
}

fn change_of(event: notify::Event) -> Change {
    use notify::EventKind;
    use notify::event::ModifyKind;
    let kind = match event.kind {
        EventKind::Access(_) => ChangeKind::Read,
        EventKind::Create(_) => ChangeKind::Made,
        EventKind::Modify(ModifyKind::Name(_)) => ChangeKind::Renamed,
        EventKind::Modify(_) => ChangeKind::Changed,
        EventKind::Remove(_) => ChangeKind::Removed,
        EventKind::Any | EventKind::Other => ChangeKind::Other,
    };
    Change { kind, paths: event.paths }
}

fn io_error(e: notify::Error) -> io::Error {
    match e.kind {
        notify::ErrorKind::Io(e) => e,
        _ => io::Error::other(e.to_string()),
    }
}

/// Watching a test drives: what it says changed is told to every watching that still lives, whatever places it
/// watches.
#[derive(Clone, Default)]
pub struct Telling {
    told: Arc<Mutex<Vec<Weak<Tell>>>>,
}

impl std::fmt::Debug for Telling {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("watching a test drives")
    }
}

struct Told(#[allow(dead_code, reason = "held: what is told lives as long as the watching")] Arc<Tell>);

impl Watching for Told {}

impl Telling {
    /// Tell each watching of a change.
    pub fn tell(&self, change: &Change) {
        for tell in self.live() {
            tell(Ok(change.clone()));
        }
    }

    /// Tell each watching that watching failed.
    pub fn fail(&self, why: &str) {
        for tell in self.live() {
            tell(Err(io::Error::other(why.to_string())));
        }
    }

    fn live(&self) -> Vec<Arc<Tell>> {
        let mut told = self.told.lock().unwrap_or_else(PoisonError::into_inner);
        told.retain(|tell| tell.strong_count() > 0);
        told.iter().filter_map(Weak::upgrade).collect()
    }
}

impl Watch for Telling {
    fn watch(&self, _places: &[(HostPath, Depth)], tell: Tell) -> io::Result<Box<dyn Watching>> {
        let tell = Arc::new(tell);
        self.told.lock().unwrap_or_else(PoisonError::into_inner).push(Arc::downgrade(&tell));
        Ok(Box::new(Told(tell)))
    }
}

#[cfg(test)]
mod tests {
    use super::{Change, ChangeKind, Telling, Watch};
    use std::sync::mpsc;

    #[test]
    fn what_a_test_tells_reaches_each_watching_while_it_lives() {
        let telling = Telling::default();
        let (told, heard) = mpsc::channel();
        let tell = move |change: std::io::Result<Change>| {
            let _ = told.send(change.map_err(|e| e.to_string()));
        };
        let watching = telling.watch(&[], Box::new(tell)).unwrap();
        let change = Change { kind: ChangeKind::Made, paths: vec!["a".into()] };
        telling.tell(&change);
        telling.fail("too many watches");
        assert_eq!(heard.try_recv().unwrap(), Ok(change.clone()));
        assert_eq!(heard.try_recv().unwrap(), Err("too many watches".to_string()));
        drop(watching);
        telling.tell(&change);
        assert!(heard.try_recv().is_err(), "a watching dropped hears nothing");
    }
}
