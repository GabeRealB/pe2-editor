//! A project's layers watched for what changes by other means than its holder - a text editor's, git's, an
//! extraction's: each layer's and mount's folder, and the folders of its changesets and `project.toml`, gathered
//! into batches. A batch goes once the layers have been quiet for a moment, or after a second of steady change.
//! Not the project's, and left out: what is read (the filesystem served reads the layers all the time), its cache,
//! what its builds write, the folder it is served at, its unmanaged mounts (the discs), the temporary files of
//! atomic writes, and what else sits in its own folder besides its `project*.toml`.

use sfs::HostPath;
use std::collections::BTreeSet;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use notify::event::ModifyKind;
use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};
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
pub fn layers(p: &Project, taken: impl Fn(Change) -> bool + Send + 'static) -> anyhow::Result<RecommendedWatcher> {
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
    watch(&roots, &shallow, move |path| !ignore.iter().any(|i| path.starts_with(i)) && concerns(path), taken)
}

/// Watch `roots` recursively, symlinks unfollowed (mounted trees are not the index's), and `shallow` for their own
/// entries only; of what changes there, what is `wanted` is handed to `taken` a batch at a time, for as long as
/// it says to go on.
///
/// # Errors
/// A folder cannot be watched.
pub fn watch(
    roots: &[HostPath],
    shallow: &[HostPath],
    wanted: impl Fn(&HostPath) -> bool + Send + 'static,
    taken: impl Fn(Change) -> bool + Send + 'static,
) -> anyhow::Result<RecommendedWatcher> {
    let (events, received) = mpsc::channel();
    let mut watcher = RecommendedWatcher::new(events, Config::default().with_follow_symlinks(false))?;
    let roots: Vec<&HostPath> = roots.iter().filter(|root| root.is_dir()).collect();
    // A root inside another is watched with it.
    for root in roots.iter().filter(|r| !roots.iter().any(|o| o != *r && r.starts_with(o))) {
        watcher.watch(root, RecursiveMode::Recursive)?;
    }
    let mut shallow: Vec<&HostPath> =
        shallow.iter().filter(|d| d.is_dir() && !roots.iter().any(|r| d.starts_with(r))).collect();
    shallow.sort();
    shallow.dedup();
    for dir in shallow {
        watcher.watch(dir, RecursiveMode::NonRecursive)?;
    }
    std::thread::Builder::new().name("watch".into()).spawn(move || debounce(&received, &wanted, &taken))?;
    Ok(watcher)
}

/// Gather events into batches until the watcher goes away, or what takes them says to stop. A wait times out once
/// the layers have been quiet for `QUIET`, or the batch's first change is `LONGEST` old: either way the batch goes.
fn debounce(
    events: &Receiver<notify::Result<notify::Event>>,
    wanted: &dyn Fn(&HostPath) -> bool,
    taken: &dyn Fn(Change) -> bool,
) {
    // The temporary files of atomic writes (`.<name>.<pid>.tmp`, `project::cache::write_atomic`) are nobody's.
    let atomic = |path: &HostPath| {
        path.extension().is_some_and(|e| e == "tmp")
            && path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('.'))
    };
    let mut paths = BTreeSet::new();
    let mut since: Option<Instant> = None;
    loop {
        let wait = since.map_or(Duration::from_secs(3600), |t| QUIET.min(LONGEST.saturating_sub(t.elapsed())));
        match events.recv_timeout(wait) {
            Ok(Ok(event)) => {
                // What is read is no change.
                if !matches!(event.kind, EventKind::Access(_)) {
                    // A folder told modified is what came or went in it, which its entries' own events say: Windows
                    // tells it so for every file made or removed in it - by the holder's own edits too, which would
                    // take the folder for another program's change. Renamed, it is told so (`Name`), and kept.
                    let listing = matches!(event.kind, EventKind::Modify(kind) if !matches!(kind, ModifyKind::Name(_)));
                    let listed = |path: &HostPath| listing && path.is_dir();
                    // What the system says changed is in the form of the folder watched, which is a HostPath's.
                    let changed = event.paths.into_iter().filter_map(HostPath::from_plain);
                    for path in changed.filter(|p| !atomic(p) && !listed(p) && wanted(p)) {
                        paths.insert(path);
                        since.get_or_insert_with(Instant::now);
                    }
                }
                if since.is_none_or(|t| t.elapsed() < LONGEST) {
                    continue;
                }
            }
            Ok(Err(e)) => {
                if !taken(Change::Error(e.to_string())) {
                    return;
                }
                continue;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        since = None;
        if !paths.is_empty() && !taken(Change::Paths(std::mem::take(&mut paths).into_iter().collect())) {
            return;
        }
    }
}

#[cfg(test)]
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
        let _watching = watch(std::slice::from_ref(&root), &[], move |path| !path.starts_with(&cache), taken).unwrap();
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
