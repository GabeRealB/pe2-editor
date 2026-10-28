//! The project's watcher: every change under its layers - the editor's own, or a text editor's, git's, an
//! extraction's - gathered into batches the editor refreshes its index with. A batch goes once the layers have been
//! quiet for a moment, or after a second of steady change; reads (the editor's own opens) and the editor's bookkeeping
//! (the cache, the temporary files of atomic writes, what the project's own directory holds besides assets) are left
//! out.

use eframe::egui;
use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

/// How long the layers must be quiet before a batch goes.
const QUIET: Duration = Duration::from_millis(150);
/// The longest a batch waits while changes keep coming.
const LONGEST: Duration = Duration::from_secs(1);

pub enum Change {
    /// Paths that changed: created, modified, removed or renamed (both names).
    Paths(Vec<PathBuf>),
    /// The watcher failed - typically the system's limit of watches, which leaves changes unseen.
    Error(String),
}

pub struct Watch {
    /// Kept for its lifetime: dropping it stops the watching.
    _watcher: RecommendedWatcher,
    batches: Receiver<Change>,
}

impl Watch {
    /// Watch `roots` recursively, symlinks unfollowed (mounted trees are not the index's), and `shallow` for their
    /// own entries only, leaving out what lies under `ignore`; the editor is woken for every batch.
    ///
    /// # Errors
    /// A root cannot be watched - not found, or the system's watch limit reached.
    pub fn start(
        roots: &[PathBuf],
        shallow: &[PathBuf],
        ignore: Vec<PathBuf>,
        ctx: egui::Context,
    ) -> notify::Result<Watch> {
        let (events, received) = mpsc::channel();
        let mut watcher = RecommendedWatcher::new(events, Config::default().with_follow_symlinks(false))?;
        // A root inside another is watched with it.
        for root in roots.iter().filter(|r| !roots.iter().any(|o| o != *r && r.starts_with(o))) {
            watcher.watch(root, RecursiveMode::Recursive)?;
        }
        for dir in shallow.iter().filter(|d| !roots.iter().any(|r| d.starts_with(r))) {
            watcher.watch(dir, RecursiveMode::NonRecursive)?;
        }
        let (batches, polled) = mpsc::channel();
        std::thread::Builder::new()
            .name("watch".into())
            .spawn(move || debounce(&received, &batches, &ignore, &ctx))
            .map_err(|e| notify::Error::generic(&e.to_string()))?;
        Ok(Watch { _watcher: watcher, batches: polled })
    }

    /// The batches that arrived since the last poll.
    pub fn poll(&self) -> Vec<Change> {
        self.batches.try_iter().collect()
    }
}

/// Gather events into batches until the watcher goes away. A wait times out once the layers have been quiet for
/// `QUIET`, or the batch's first change is `LONGEST` old: either way the batch goes.
fn debounce(
    events: &Receiver<notify::Result<notify::Event>>,
    out: &Sender<Change>,
    ignore: &[PathBuf],
    ctx: &egui::Context,
) {
    let mut paths = BTreeSet::new();
    let mut since: Option<Instant> = None;
    loop {
        let wait = since.map_or(Duration::from_secs(3600), |t| QUIET.min(LONGEST.saturating_sub(t.elapsed())));
        match events.recv_timeout(wait) {
            Ok(Ok(event)) => {
                if !matches!(event.kind, EventKind::Access(_)) {
                    for path in event.paths.into_iter().filter(|p| !ignored(p, ignore)) {
                        paths.insert(path);
                        since.get_or_insert_with(Instant::now);
                    }
                }
                if since.is_none_or(|t| t.elapsed() < LONGEST) {
                    continue;
                }
            }
            Ok(Err(e)) => {
                let _ = out.send(Change::Error(e.to_string()));
                ctx.request_repaint();
                continue;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        since = None;
        if !paths.is_empty() {
            if out.send(Change::Paths(std::mem::take(&mut paths).into_iter().collect())).is_err() {
                return;
            }
            ctx.request_repaint();
        }
    }
}

/// What a batch leaves out: anything under an ignored directory, and the temporary files of atomic writes
/// (`.<name>.<pid>.tmp`, `project::cache::write_atomic`).
fn ignored(path: &Path, ignore: &[PathBuf]) -> bool {
    ignore.iter().any(|i| path.starts_with(i))
        || (path.extension().is_some_and(|e| e == "tmp")
            && path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('.')))
}

#[cfg(test)]
mod tests {
    use super::{Change, Watch};
    use eframe::egui;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    /// The paths of the batches that arrive within a few seconds.
    fn collect(watch: &Watch, until: impl Fn(&[PathBuf]) -> bool) -> Vec<PathBuf> {
        let start = Instant::now();
        let mut seen = Vec::new();
        while start.elapsed() < Duration::from_secs(5) && !until(&seen) {
            for change in watch.poll() {
                if let Change::Paths(paths) = change {
                    seen.extend(paths);
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        seen
    }

    #[test]
    fn batches_changes_and_leaves_out_bookkeeping() {
        let root = std::env::temp_dir().join(format!("pe2-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("a.asset")).unwrap();
        std::fs::create_dir_all(root.join(".cache")).unwrap();
        let watch = Watch::start(std::slice::from_ref(&root), &[], vec![root.join(".cache")], egui::Context::default())
            .unwrap();
        std::fs::write(root.join(".cache/index.json"), "{}").unwrap();
        std::fs::write(root.join("a.asset/.asset.toml.1.tmp"), "x").unwrap();
        let declaration = root.join("a.asset/asset.toml");
        std::fs::write(&declaration, "uid = \"uid://a\"\n").unwrap();
        let seen = collect(&watch, |seen| seen.contains(&declaration));
        assert!(seen.contains(&declaration), "{seen:?}");
        assert!(seen.iter().all(|p| !p.starts_with(root.join(".cache")) && p.extension().is_none_or(|e| e != "tmp")));
        // A rename reports both names.
        std::fs::rename(root.join("a.asset"), root.join("b.asset")).unwrap();
        let seen = collect(&watch, |seen| seen.contains(&root.join("b.asset")));
        assert!(seen.contains(&root.join("a.asset")) && seen.contains(&root.join("b.asset")), "{seen:?}");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
