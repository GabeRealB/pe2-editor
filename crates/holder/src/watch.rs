//! A project's layers watched for what changes by other means than its holder: each layer's and mount's folder, and
//! the folders of its changesets and `project.toml`. Not the project's: its cache, what its builds write, the folder
//! it is served at, its unmanaged mounts (the discs), the temporary files of atomic writes.

use std::path::{Path, PathBuf};
use std::time::Duration;

use project::Project;

/// Watch a project's layers, handing what changed to `taken` a batch at a time - what arrives until a moment passes
/// with nothing more - for as long as it says to go on. Watching ends with what this returns.
///
/// # Errors
/// The folders cannot be watched.
pub fn layers(
    p: &Project,
    taken: impl Fn(&[PathBuf]) -> bool + Send + 'static,
) -> anyhow::Result<notify::RecommendedWatcher> {
    use notify::Watcher as _;
    let mut ignore = vec![p.root.join(project::cache::DIR), p.root.join("build"), p.filesystem.at.clone()];
    ignore.extend(p.unmanaged.iter().map(|m| m.path.clone()));
    let (seen, changes) = std::sync::mpsc::channel::<PathBuf>();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        // What is read is no change: the filesystem served reads the layers all the time.
        let Ok(event) = event.map_err(drop) else { return };
        if matches!(event.kind, notify::EventKind::Access(_)) {
            return;
        }
        for path in event.paths {
            let _ = seen.send(path);
        }
    })?;
    for layer in &p.layers {
        if layer.path.is_dir() {
            watcher.watch(&layer.path, notify::RecursiveMode::Recursive)?;
        }
    }
    let mut folders: Vec<PathBuf> =
        p.layers.iter().filter_map(|l| l.changeset.parent().map(Path::to_path_buf)).collect();
    folders.push(p.root.clone());
    folders.sort();
    folders.dedup();
    for folder in folders {
        if folder.is_dir() {
            watcher.watch(&folder, notify::RecursiveMode::NonRecursive)?;
        }
    }
    std::thread::Builder::new().name("watch".into()).spawn(move || {
        let atomic = |path: &Path| {
            path.extension().is_some_and(|e| e == "tmp")
                && path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('.'))
        };
        // A batch: what arrives until a moment passes with nothing more.
        while let Ok(first) = changes.recv() {
            let mut batch = vec![first];
            while let Ok(more) = changes.recv_timeout(Duration::from_millis(300)) {
                batch.push(more);
            }
            batch.retain(|path| !atomic(path) && !ignore.iter().any(|i| path.starts_with(i)));
            batch.sort();
            batch.dedup();
            if !batch.is_empty() && !taken(&batch) {
                break;
            }
        }
    })?;
    Ok(watcher)
}
