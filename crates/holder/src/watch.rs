//! A project's layers watched for what changes by other means than its holder - a text editor's, git's, an
//! extraction's: each layer's and mount's folder, and the folders of its changesets and `project.toml`, gathered
//! into batches. A batch goes once the layers have been quiet for a moment, or after a second of steady change.
//! Not the project's, and left out: what is read (the filesystem served reads the layers all the time), its cache,
//! what its builds write, the folder it is served at, its unmanaged mounts (the discs), the temporary files of
//! atomic writes, and what else sits in its own folder besides its `project*.toml`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};
use project::Project;

/// How long the layers must be quiet before a batch goes.
const QUIET: Duration = Duration::from_millis(150);
/// The longest a batch waits while changes keep coming.
const LONGEST: Duration = Duration::from_secs(1);

/// What watching came to.
pub enum Change {
    /// Paths that changed: created, modified, removed or renamed (both names).
    Paths(Vec<PathBuf>),
    /// The watcher failed - typically the system's limit of watches, which leaves changes unseen.
    Error(String),
}

/// Watch a project's layers, handing what changed to `taken` a batch at a time, for as long as it says to go on.
/// Watching ends with what this returns.
///
/// # Errors
/// The folders cannot be watched - the system's limit of watches reached.
pub fn layers(p: &Project, taken: impl Fn(Change) -> bool + Send + 'static) -> anyhow::Result<RecommendedWatcher> {
    let roots: Vec<PathBuf> = p.layers.iter().map(|l| l.path.clone()).collect();
    // Each layer's changeset sits beside its directory; `project.toml` in the project's own.
    let mut shallow: Vec<PathBuf> =
        p.layers.iter().filter_map(|l| l.changeset.parent().map(Path::to_path_buf)).collect();
    shallow.push(p.root.clone());
    let mut ignore = vec![p.root.join(project::cache::DIR), p.root.join("build"), p.filesystem.at.clone()];
    ignore.extend(p.unmanaged.iter().map(|m| m.path.clone()));
    let (root, changesets) = (p.root.clone(), p.layers.iter().map(|l| l.changeset.clone()).collect::<Vec<_>>());
    let layers = roots.clone();
    // Whether a path changing concerns the project: in a layer, a layer's changeset, or one of its files at its
    // root. What else sits in its folders (exports, a user's notes) does not.
    let concerns = move |path: &Path| {
        let at_root = path.parent() == Some(root.as_path())
            && path.extension().is_some_and(|e| e.eq_ignore_ascii_case("toml"))
            && path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("project"));
        at_root || layers.iter().any(|layer| path.starts_with(layer)) || changesets.iter().any(|c| path == c)
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
    roots: &[PathBuf],
    shallow: &[PathBuf],
    wanted: impl Fn(&Path) -> bool + Send + 'static,
    taken: impl Fn(Change) -> bool + Send + 'static,
) -> anyhow::Result<RecommendedWatcher> {
    let (events, received) = mpsc::channel();
    let mut watcher = RecommendedWatcher::new(events, Config::default().with_follow_symlinks(false))?;
    let roots: Vec<&PathBuf> = roots.iter().filter(|root| root.is_dir()).collect();
    // A root inside another is watched with it.
    for root in roots.iter().filter(|r| !roots.iter().any(|o| o != *r && r.starts_with(o))) {
        watcher.watch(root, RecursiveMode::Recursive)?;
    }
    let mut shallow: Vec<&PathBuf> =
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
    wanted: &dyn Fn(&Path) -> bool,
    taken: &dyn Fn(Change) -> bool,
) {
    // The temporary files of atomic writes (`.<name>.<pid>.tmp`, `project::cache::write_atomic`) are nobody's.
    let atomic = |path: &Path| {
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
                    for path in event.paths.into_iter().filter(|p| !atomic(p) && wanted(p)) {
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
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    /// The paths of the batches that arrive within a few seconds.
    fn collect(batches: &mpsc::Receiver<Vec<PathBuf>>, until: impl Fn(&[PathBuf]) -> bool) -> Vec<PathBuf> {
        let start = Instant::now();
        let mut seen = Vec::new();
        while start.elapsed() < Duration::from_secs(5) && !until(&seen) {
            if let Ok(paths) = batches.recv_timeout(Duration::from_millis(20)) {
                seen.extend(paths);
            }
        }
        seen
    }

    #[test]
    fn batches_changes_and_leaves_out_bookkeeping() {
        let root = std::env::temp_dir().join(format!("pe2-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("a.node")).unwrap();
        std::fs::create_dir_all(root.join(".cache")).unwrap();
        let (seen, batches) = mpsc::channel();
        let cache = root.join(".cache");
        let taken = move |change| match change {
            Change::Paths(paths) => seen.send(paths).is_ok(),
            Change::Error(_) => true,
        };
        let _watching = watch(std::slice::from_ref(&root), &[], move |path| !path.starts_with(&cache), taken).unwrap();
        std::fs::write(root.join(".cache/index.json"), "{}").unwrap();
        std::fs::write(root.join("a.node/.node.toml.1.tmp"), "x").unwrap();
        let declaration = root.join("a.node/node.toml");
        std::fs::write(&declaration, "uid = \"uid://a\"\n").unwrap();
        let seen = collect(&batches, |seen| seen.contains(&declaration));
        assert!(seen.contains(&declaration), "{seen:?}");
        assert!(seen.iter().all(|p| !p.starts_with(root.join(".cache")) && p.extension().is_none_or(|e| e != "tmp")));
        // A rename reports both names.
        std::fs::rename(root.join("a.node"), root.join("b.node")).unwrap();
        let seen = collect(&batches, |seen| seen.contains(&root.join("b.node")));
        assert!(seen.contains(&root.join("a.node")) && seen.contains(&root.join("b.node")), "{seen:?}");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
