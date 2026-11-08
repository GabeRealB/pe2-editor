//! Scoped filesystems: the few files a program is given, served for it alone. A program reads files by path, and a
//! layer holds a large file as chunks (`project::chunks`); rather than assemble one into a copy, a scope serves it where
//! it is asked for, at `<project>/.cache/scope/<process>/<sha1>/<name>`, read-only, as long as the process runs.
//!
//! `Scopes::install` registers them with `project::content` (`content::expose_with`), so whatever hands a layer's file
//! to a program (`content::for_program`) gets a path in its project's scope; nothing is mounted until the first file
//! is. Where no transport serves one (no FUSE here), `content::for_program` falls back on a copy in the cache.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::Served;

/// A project's scope: the files it serves, and its mount once it has one.
struct Scope {
    at: PathBuf,
    served: Served,
    files: BTreeMap<String, PathBuf>,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    mounted: Option<crate::fuse::Mounted>,
}

/// The scopes of the projects this process gives programs files of, by project: registered with `project::content`
/// while this is held, each unmounted when it is dropped.
pub struct Scopes {
    scopes: Arc<Mutex<BTreeMap<PathBuf, Scope>>>,
    _exposing: project::content::Exposing,
}

impl Scopes {
    /// Serve chunked contents to programs in scopes, from now until what this returns is dropped.
    #[must_use]
    pub fn install() -> Scopes {
        let scopes: Arc<Mutex<BTreeMap<PathBuf, Scope>>> = Arc::default();
        let held = Arc::clone(&scopes);
        let exposing = project::content::expose_with(None, move |disk| {
            expose(&mut held.lock().unwrap_or_else(std::sync::PoisonError::into_inner), disk)
        });
        Scopes { scopes, _exposing: exposing }
    }

    /// Where a project's scope is, for this process, mounted or not.
    #[must_use]
    pub fn at(root: &Path) -> PathBuf {
        root.join(project::cache::DIR).join("scope").join(std::process::id().to_string())
    }
}

impl Drop for Scopes {
    fn drop(&mut self) {
        let scopes = std::mem::take(&mut *self.scopes.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
        for (_, scope) in scopes {
            let at = scope.at.clone();
            drop(scope);
            let _ = std::fs::remove_dir(at);
        }
    }
}

/// The project whose store holds a layer's file: the folder holding the `.layers` it lies in.
fn project_of(disk: &Path) -> Option<PathBuf> {
    disk.ancestors()
        .find(|a| a.file_name().is_some_and(|n| n == project::LAYERS))
        .and_then(Path::parent)
        .map(Path::to_path_buf)
}

/// Serve a layer's file in its project's scope, mounting it first where it is not: its path there.
fn expose(scopes: &mut BTreeMap<PathBuf, Scope>, disk: &Path) -> io::Result<PathBuf> {
    let root =
        project_of(disk).ok_or_else(|| io::Error::other(format!("{}: in no project's layers", disk.display())))?;
    if !scopes.contains_key(&root) {
        sweep(&root);
    }
    let scope = scopes.entry(root.clone()).or_insert_with(|| Scope {
        at: Scopes::at(&root),
        served: Served::default(),
        files: BTreeMap::new(),
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        mounted: None,
    });
    let sha1 = project::content::sha1(disk)?;
    let name = disk.file_name().map_or_else(|| "content".into(), |n| n.to_string_lossy().into_owned());
    let path = format!("{sha1}/{name}");
    if !scope.files.contains_key(&path) {
        scope.files.insert(path.clone(), disk.to_path_buf());
        scope.served.set_files(&scope.files);
    }
    mount(scope)?;
    Ok(scope.at.join(path))
}

/// Remove the scopes processes that have ended left in a project's cache: their mounts cleared where they are stale.
fn sweep(root: &Path) {
    let Some(dir) = Scopes::at(root).parent().map(Path::to_path_buf) else { return };
    let mine = std::process::id().to_string();
    for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == mine || alive(&name) {
            continue;
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        crate::fuse::clear_stale(&entry.path());
        let _ = std::fs::remove_dir(entry.path());
    }
}

/// Whether the process a scope is named after still runs; where that cannot be told, it is taken to.
fn alive(pid: &str) -> bool {
    if cfg!(target_os = "linux") {
        return Path::new("/proc").join(pid).exists();
    }
    true
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn mount(scope: &mut Scope) -> io::Result<()> {
    if scope.mounted.is_none() {
        std::fs::create_dir_all(&scope.at)?;
        scope.mounted = Some(crate::fuse::mount(&scope.served, None, &scope.at, "pe2-scope")?);
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn mount(_: &mut Scope) -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "no filesystem transport here"))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::Scopes;

    #[test]
    fn a_chunked_file_is_given_to_programs_in_its_scope() {
        let root = std::env::temp_dir().join(format!("pe2-scope-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let file = root.join(".layers/top/a.node/movie.str");
        let bytes: Vec<u8> = (0..(3u32 << 20)).map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8).collect();
        project::content::write(&file, &bytes).expect("written");
        let scopes = Scopes::install();
        let given = project::content::for_program(&file).expect("given");
        let fuse = std::path::Path::new("/dev/fuse").exists();
        assert_eq!(given.starts_with(Scopes::at(&root)), fuse, "{}", given.display());
        if fuse {
            // As a program reads it: a file, by path, its name kept.
            assert_eq!(given.file_name().unwrap(), "movie.str");
            assert_eq!(std::fs::read(&given).expect("read through the scope"), bytes);
            assert!(!root.join(".cache/materialized").exists(), "nothing copied");
        } else {
            // No FUSE here: a copy in the cache.
            assert!(given.starts_with(root.join(".cache/materialized")));
        }
        drop(scopes);
        assert!(!Scopes::at(&root).exists(), "unmounted and gone");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
