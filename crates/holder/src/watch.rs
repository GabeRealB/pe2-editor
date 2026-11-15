//! A project's layers watched for what changes by other means than its holder - a text editor's, git's, an
//! extraction's: each layer's and mount's folder, and the folders of its changesets and `project.toml`, gathered
//! into batches. A batch goes once the layers have been quiet for a moment, or after a second of steady change.
//! Not the project's, and left out: what is read (the filesystem served reads the layers all the time), its cache,
//! what its builds write, the folder it is served at, its unmanaged mounts (the discs), the temporary files of
//! atomic writes, and what else sits in its own folder besides its `project*.toml`.

use sfs::HostPath;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use pe2_io::{ChangeKind, Depth, Watching};
use project::Project;

/// How long the layers must be quiet before a batch goes.
const QUIET: Duration = Duration::from_millis(150);
/// The longest a batch waits while changes keep coming.
const LONGEST: Duration = Duration::from_secs(1);

/// What watching came to.
pub enum Change {
    /// Paths that changed: created, modified, removed or renamed (both names).
    Paths(Vec<HostPath>),
    /// The watcher failed - typically the system's limit of watches, which leaves changes unseen.
    Error(String),
}

/// Watch a project's layers, handing what changed to `taken` a batch at a time, for as long as it says to go on.
/// Watching ends with what this returns.
///
/// # Errors
/// The folders cannot be watched - the system's limit of watches reached.
pub fn layers(p: &Project, taken: impl Fn(Change) -> bool + Send + 'static) -> anyhow::Result<Box<dyn Watching>> {
    let roots: Vec<HostPath> = p.layers.iter().map(|l| l.path.clone()).collect();
    // Each layer's changeset sits beside its directory; `project.toml` in the project's own.
    let mut shallow: Vec<HostPath> = p.layers.iter().filter_map(|l| l.changeset.parent()).collect();
    shallow.push(p.root.clone());
    let built = p.root.join(&pe2_path::Name::constant("build"));
    let mut ignore = vec![p.root.join(&project::cache::DIR), built, p.filesystem.at.clone()];
    ignore.extend(p.unmanaged.iter().map(|m| m.path.clone()));
    let (root, changesets) = (p.root.clone(), p.layers.iter().map(|l| l.changeset.clone()).collect::<Vec<_>>());
    let layers = roots.clone();
    // Whether a path changing concerns the project: in a layer, a layer's changeset, or one of its files at its
    // root. What else sits in its folders (exports, a user's notes) does not.
    let concerns = move |path: &HostPath| {
        let at_root = path.parent().is_some_and(|parent| parent == root)
            && path.extension().is_some_and(|e| e.eq_ignore_ascii_case("toml"))
            && path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("project"));
        at_root || layers.iter().any(|layer| path.starts_with(layer)) || changesets.contains(path)
    };
    let wanted = move |path: &HostPath| !ignore.iter().any(|i| path.starts_with(i)) && concerns(path);
    watch(&p.io, &roots, &shallow, wanted, taken)
}

/// Watch `roots` recursively, symlinks unfollowed (mounted trees are not the index's), and `shallow` for their own
/// entries only; of what changes there, what is `wanted` is handed to `taken` a batch at a time, for as long as
/// it says to go on. Batches are timed by the clock of `io`.
///
/// # Errors
/// A folder cannot be watched.
pub fn watch(
    io: &Arc<dyn pe2_io::Io>,
    roots: &[HostPath],
    shallow: &[HostPath],
    wanted: impl Fn(&HostPath) -> bool + Send + 'static,
    taken: impl Fn(Change) -> bool + Send + 'static,
) -> anyhow::Result<Box<dyn Watching>> {
    let (events, received) = mpsc::channel();
    // Each event sent, and the batching told of it; told as well when the watcher goes, which drops this.
    let told = Told { waiter: io.clock().waiter() };
    let waiter = told.waiter.clone();
    let tell = move |change: std::io::Result<pe2_io::Change>| {
        let _ = events.send(change);
        told.waiter.wake();
    };
    let roots: Vec<&HostPath> = roots.iter().filter(|root| io.filesystem().is_folder(root)).collect();
    // A root inside another is watched with it.
    let mut places: Vec<(HostPath, Depth)> = roots
        .iter()
        .filter(|r| !roots.iter().any(|o| o != *r && r.starts_with(o)))
        .map(|root| ((*root).clone(), Depth::All))
        .collect();
    let mut shallow: Vec<&HostPath> =
        shallow.iter().filter(|d| io.filesystem().is_folder(d) && !roots.iter().any(|r| d.starts_with(r))).collect();
    shallow.sort();
    shallow.dedup();
    places.extend(shallow.into_iter().map(|dir| (dir.clone(), Depth::Own)));
    let watcher = io.watch().watch(&places, Box::new(tell))?;
    let io = io.clone();
    std::thread::Builder::new()
        .name("watch".into())
        .spawn(move || debounce(&*io, &*waiter, &received, &wanted, &taken))?;
    Ok(watcher)
}

/// What wakes the batching for each event, and once more as the watcher goes.
struct Told {
    waiter: Arc<dyn pe2_io::Waiter>,
}

impl Drop for Told {
    fn drop(&mut self) {
        self.waiter.wake();
    }
}

/// Gather events into batches until the watcher goes away, or what takes them says to stop. A batch goes once the
/// layers have been quiet for `QUIET`, or its first change is `LONGEST` old, by the clock.
fn debounce(
    io: &dyn pe2_io::Io,
    woken: &dyn pe2_io::Waiter,
    events: &Receiver<std::io::Result<pe2_io::Change>>,
    wanted: &dyn Fn(&HostPath) -> bool,
    taken: &dyn Fn(Change) -> bool,
) {
    // The temporary files of atomic writes (`.<name>.<pid>.tmp`, `project::cache::write_atomic`) are nobody's.
    let atomic = |path: &HostPath| {
        path.extension().is_some_and(|e| e == "tmp")
            && path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('.'))
    };
    let clock = io.clock();
    let mut paths = BTreeSet::new();
    // When the batch's first change came, and its last.
    let mut since: Option<(Instant, Instant)> = None;
    loop {
        let due = since.map(|(first, last)| (last + QUIET).min(first + LONGEST));
        let event = match events.try_recv() {
            Ok(event) => Some(event),
            Err(TryRecvError::Disconnected) => return,
            Err(TryRecvError::Empty) if due.is_some_and(|due| clock.now() >= due) => None,
            Err(TryRecvError::Empty) => {
                woken.wait_until(due);
                continue;
            }
        };
        match event {
            Some(Ok(event)) => {
                // What is read is no change.
                if event.kind != ChangeKind::Read {
                    // A folder told changed is what came or went in it, which its entries' own events say: Windows
                    // tells it so for every file made or removed in it - by the holder's own edits too, which would
                    // take the folder for another program's change. Renamed, it is told so, and kept.
                    let listing = event.kind == ChangeKind::Changed;
                    let listed = |path: &HostPath| listing && io.filesystem().is_folder(path);
                    // What the system says changed is in the form of the folder watched, which is a HostPath's.
                    let changed = event.paths.into_iter().filter_map(HostPath::from_plain);
                    for path in changed.filter(|p| !atomic(p) && !listed(p) && wanted(p)) {
                        paths.insert(path);
                        let now = clock.now();
                        since = Some(since.map_or((now, now), |(first, _)| (first, now)));
                    }
                }
                if since.is_none_or(|(first, _)| clock.now() < first + LONGEST) {
                    continue;
                }
            }
            Some(Err(e)) => {
                if !taken(Change::Error(e.to_string())) {
                    return;
                }
                continue;
            }
            None => {}
        }
        since = None;
        if !paths.is_empty() && !taken(Change::Paths(std::mem::take(&mut paths).into_iter().collect())) {
            return;
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::disallowed_methods,
    reason = "a test's fixtures: the machine's time, places, files and environment, which nothing tested reads"
)]
mod tests {
    use super::{Change, watch};
    use sfs::HostPath;
    use sfs::testing::At as _;
    use std::sync::mpsc;
    /// The paths of the batches that arrive, until what has arrived holds.
    fn collect(batches: &mpsc::Receiver<Vec<HostPath>>, until: impl Fn(&[HostPath]) -> bool) -> Vec<HostPath> {
        let mut seen = Vec::new();
        while !until(&seen) {
            seen.extend(batches.recv().expect("the watcher goes on"));
        }
        seen
    }

    #[test]
    fn batches_changes_and_leaves_out_bookkeeping() {
        let root = sfs::testing::scratch("watch");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.at("a.node")).unwrap();
        std::fs::create_dir_all(root.at(".cache")).unwrap();
        let (seen, batches) = mpsc::channel();
        let cache = root.at(".cache");
        let taken = move |change| match change {
            Change::Paths(paths) => seen.send(paths).is_ok(),
            Change::Error(_) => true,
        };
        let _watching =
            watch(&pe2_io::system(), std::slice::from_ref(&root), &[], move |path| !path.starts_with(&cache), taken)
                .unwrap();
        std::fs::write(root.at(".cache/index.json"), "{}").unwrap();
        std::fs::write(root.at("a.node/.node.toml.1.tmp"), "x").unwrap();
        let declaration = root.at("a.node/node.toml");
        std::fs::write(&declaration, "uid = \"uid://a\"\n").unwrap();
        let seen = collect(&batches, |seen| seen.contains(&declaration));
        assert!(seen.contains(&declaration), "{seen:?}");
        assert!(seen.iter().all(|p| !p.starts_with(&root.at(".cache")) && p.extension().is_none_or(|e| e != "tmp")));
        // A rename reports both names.
        std::fs::rename(root.at("a.node"), root.at("b.node")).unwrap();
        let seen = collect(&batches, |seen| seen.contains(&root.at("b.node")));
        assert!(seen.contains(&root.at("a.node")) && seen.contains(&root.at("b.node")), "{seen:?}");
        // A file made in a folder there is that file: the folder is no change of its own, as Windows tells it.
        let shard = root.at("b.node/ab");
        std::fs::create_dir_all(&shard).unwrap();
        let file = shard.at("abcdef.toml");
        std::fs::write(&file, "x").unwrap();
        // (The file, or the folder made for it a moment before, whose watch it may come before: taken in whole.)
        let seen = collect(&batches, |seen| seen.contains(&file) || seen.contains(&shard));
        assert!(seen.contains(&file) || seen.contains(&shard), "{seen:?}");
        // What a folder told modified would bring comes before what is changed after it: a file written once all of
        // it has arrived.
        let after = root.at("after.txt");
        std::fs::write(&after, "x").unwrap();
        let seen = [seen, collect(&batches, |seen| seen.contains(&after))].concat();
        assert!(!seen.contains(&root.at("b.node")), "the folder they were made in: {seen:?}");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
