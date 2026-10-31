//! The project cache: `.cache/` beside `project.toml`, what the editor derives from the layers - the index, artifacts,
//! editor state. It is never a layer and never the only copy of anything, so it may be deleted at any time at the cost
//! of time only. It ignores itself (its own `.gitignore`, so no tracked file has to name it), and its `version` names
//! the schema and the tools that wrote it: a cache written by others is wiped on opening - all but the project lock
//! (`lock.rs`), which whoever opens the cache holds.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::Project;
use crate::lock::Lock;

pub const DIR: &str = ".cache";
/// Raised whenever what the cache holds changes shape.
const SCHEMA: u32 = 2;

pub struct Cache {
    pub dir: PathBuf,
}

impl Cache {
    /// The project's cache, made if missing and wiped if another schema or version wrote it. Only the holder of the
    /// project's lock opens it.
    ///
    /// # Errors
    /// The cache directory cannot be made, wiped or written.
    pub fn open(project: &Project, _lock: &Lock) -> Result<Cache> {
        let dir = project.root.join(DIR);
        let version = format!("schema {SCHEMA}\nproject {}\n", env!("CARGO_PKG_VERSION"));
        let current = std::fs::read_to_string(dir.join("version")).ok();
        if current.as_deref() != Some(version.as_str()) && dir.exists() {
            // Everything but the project lock, which the process opening the cache holds.
            for entry in std::fs::read_dir(&dir).with_context(|| format!("{}", dir.display()))? {
                let entry = entry?;
                if entry.file_name().to_str().is_some_and(|n| n.starts_with("lock")) {
                    continue;
                }
                let path = entry.path();
                if entry.file_type()?.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) }
                    .with_context(|| format!("wiping {}", path.display()))?;
            }
        }
        make(&project.root)?;
        if current.as_deref() != Some(version.as_str()) {
            write_atomic(&dir.join("version"), version.as_bytes())?;
        }
        Ok(Cache { dir })
    }

    /// A file of the cache.
    #[must_use]
    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

/// Make the project's cache directory, ignoring itself, if it is missing: whoever makes it first - the cache, the
/// project lock, the Python tools' lock (`tools/project_lock.py`) - writes its `.gitignore`, so nothing in it is ever
/// seen by git.
///
/// # Errors
/// The directory or its `.gitignore` cannot be written.
pub fn make(root: &Path) -> Result<PathBuf> {
    let dir = root.join(DIR);
    std::fs::create_dir_all(&dir).with_context(|| format!("{}", dir.display()))?;
    let ignore = dir.join(".gitignore");
    if !ignore.exists() {
        write_atomic(&ignore, b"*\n")?;
    }
    Ok(dir)
}

/// Write a file whole: to a temporary name beside it, then renamed over it, so no reader ever sees half a file.
///
/// # Errors
/// The temporary file cannot be written or renamed.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    // Each writer's own: two threads may be writing the same file, as two nodes making one value do.
    static WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let write = WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temporary = path.with_file_name(format!(".{name}.{}.{write}.tmp", std::process::id()));
    std::fs::write(&temporary, bytes).with_context(|| format!("{}", temporary.display()))?;
    std::fs::rename(&temporary, path).with_context(|| format!("{}", path.display()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn writers_of_one_file_do_not_meet() {
        let dir = std::env::temp_dir().join(format!("pe2-write-atomic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("value");
        let writers: Vec<_> = (0..8)
            .map(|_| {
                let file = file.clone();
                std::thread::spawn(move || (0..200).try_for_each(|_| super::write_atomic(&file, b"held")))
            })
            .collect();
        for writer in writers {
            writer.join().unwrap().unwrap();
        }
        assert_eq!(std::fs::read(&file).unwrap(), b"held");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
