//! What the folder a project is served at holds that is not the project's, before it is served there: files written in
//! it while nothing served it - a program left open on it after the host stopped, a file put there by hand - and, where
//! the backend keeps what was read on disk, a file read on an earlier run and changed while nothing served it (a
//! placeholder no longer in sync). A mount would hide them, and placeholders would serve them beside the project's, or
//! in place of them: so the host does not serve over them, unless told to set them aside (`set_aside`) or take them in
//! as edits (`take_in`).

use pe2_path::{Name, RelPath};
use project::TreePath;
use sfs::HostPath;
use std::io;
use std::sync::Arc;

use crate::backend::Backend;
use crate::write::{Change, Owner, is_junk, refused};

/// What the folder at `at` holds that is not the project's, before a backend serves it there: its files, by their path
/// in it, but those the backend left there as it served them. Folders are none: a folder holds nothing of its own.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Found {
    /// Each by its path, in order.
    pub strays: Vec<Arc<RelPath>>,
    /// What is named so no path takes it - a file or a folder, with all in it - shown as the system names it: nothing
    /// here moves it by its path, nor serves over it.
    pub unnamed: Vec<String>,
}

impl Found {
    /// How many it holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.strays.len() + self.unnamed.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// What the folder at `at` holds that is not the project's, before a backend serves it there (`Found`).
#[must_use]
pub fn strays(fs: &dyn pe2_io::Filesystem, at: &HostPath, backend: &dyn Backend) -> Found {
    let mut found = Found::default();
    walk(fs, at, RelPath::root(), backend, &mut found);
    found.strays.sort();
    found.unnamed.sort();
    found
}

fn walk(fs: &dyn pe2_io::Filesystem, dir: &HostPath, at: &RelPath, backend: &dyn Backend, found: &mut Found) {
    let Ok(entries) = fs.read_directory(dir) else { return };
    for entry in entries {
        let place = dir.entry(&entry.name);
        let Some(name) = entry.name.to_str().and_then(|name| Name::new(name).ok()).map(Name::to_owned) else {
            let shown = entry.name.to_string_lossy().into_owned();
            found.unnamed.push(if at.is_root() { shown } else { format!("{at}/{shown}") });
            continue;
        };
        let path = at.join(&name);
        if entry.kind == pe2_io::Kind::Folder {
            walk(fs, &place, &path, backend, found);
        } else if !backend.left_as_served(&place) {
            found.strays.push(path);
        }
    }
}

/// Move what is not the project's out of the folder at `at`, keeping its paths, into `to`; folders left empty go.
///
/// # Errors
/// A file cannot be moved.
pub fn set_aside(fs: &dyn pe2_io::Filesystem, at: &HostPath, strays: &[Arc<RelPath>], to: &HostPath) -> io::Result<()> {
    for stray in strays {
        let (from, into) = (at.join(&**stray), to.join(&**stray));
        if let Some(parent) = into.parent() {
            fs.create_folder_all(&parent)?;
        }
        // A move where it can be one, else a copy (another volume).
        if fs.rename(&from, &into).is_err() {
            fs.copy(&from, &into)?;
            fs.remove_file(&from)?;
        }
    }
    remove_emptied(fs, at, strays);
    Ok(())
}

/// Take what is not the project's into it, as the edits a program writing it through the folder makes - each file
/// written, a `node.toml` before what is beside it - and out of the folder. What no edit takes - an editor's leftovers,
/// a name storage keeps - is set aside into `aside`. Returns what was set aside.
///
/// # Errors
/// A file cannot be read or moved, or an edit is refused: what was taken in before it stays taken.
pub fn take_in(
    fs: &dyn pe2_io::Filesystem,
    at: &HostPath,
    strays: &[Arc<RelPath>],
    owner: &dyn Owner,
    staging: &HostPath,
    aside: &HostPath,
) -> io::Result<Vec<Arc<RelPath>>> {
    let (kept, taken): (Vec<Arc<RelPath>>, Vec<Arc<RelPath>>) =
        strays.iter().cloned().partition(|s| is_junk(s) || refused(s));
    let mut ordered = taken.clone();
    // A declaration first: it makes its folder a node, which what is beside it is then a file of.
    ordered.sort_by_key(|path| {
        (path.parent().map(RelPath::to_owned), path.last() != Some(project::index::NODE_FILE), path.clone())
    });
    fs.create_folder_all(staging)?;
    for path in &ordered {
        let staged: HostPath =
            staging.join(&pe2_path::Name::new(&format!("stray-{}", std::process::id())).map_err(io::Error::other)?);
        fs.copy(&at.join(&**path), &staged)?;
        let change = Change::Write { path: TreePath::within(path).to_owned(), staged: staged.clone().into() };
        let applied = owner.change(change);
        let _ = fs.remove_file(&staged);
        applied.map_err(|why| io::Error::other(format!("{path}: {why}")))?;
        fs.remove_file(&at.join(&**path))?;
    }
    remove_emptied(fs, at, &taken);
    set_aside(fs, at, &kept, aside)?;
    Ok(kept)
}

/// Remove the folders these paths were in where they are left empty, deepest first.
fn remove_emptied(fs: &dyn pe2_io::Filesystem, at: &HostPath, paths: &[Arc<RelPath>]) {
    let above = |path: &Arc<RelPath>| -> Vec<Arc<RelPath>> {
        std::iter::successors(path.parent(), |folder| folder.parent())
            .filter(|f| !f.is_root())
            .map(RelPath::to_owned)
            .collect()
    };
    let mut folders: Vec<Arc<RelPath>> = paths.iter().flat_map(above).collect();
    folders.sort_by_key(|folder| std::cmp::Reverse(folder.depth()));
    folders.dedup();
    for folder in folders {
        // Only an empty one goes; one in sync is left to serving.
        let _ = fs.remove_folder(&at.join(&*folder));
    }
}

#[cfg(test)]
#[allow(
    clippy::disallowed_methods,
    reason = "a test's fixtures: the machine's time, places, files and environment, which nothing tested reads"
)]
mod tests {
    use super::{set_aside, strays};
    use sfs::testing::At as _;

    #[test]
    fn what_a_folder_holds_is_found_and_set_aside_with_its_paths() {
        let root = sfs::testing::scratch("strays");
        let (at, aside) = (root.at("assets"), root.at("aside"));
        std::fs::create_dir_all(at.at("notes/deep")).unwrap();
        std::fs::create_dir_all(at.at("empty")).unwrap();
        std::fs::write(at.at("stray.txt"), "s").unwrap();
        std::fs::write(at.at("notes/deep/c.md"), "c").unwrap();
        let backend = crate::dav::BACKEND;
        let found = strays(&pe2_io::SystemFilesystem, &at, backend);
        let shown: Vec<&str> = found.strays.iter().map(|path| path.as_str()).collect();
        assert_eq!((shown, found.unnamed.len()), (vec!["notes/deep/c.md", "stray.txt"], 0));
        set_aside(&pe2_io::SystemFilesystem, &at, &found.strays, &aside).unwrap();
        assert_eq!(std::fs::read_to_string(aside.at("notes/deep/c.md")).unwrap(), "c");
        assert!(strays(&pe2_io::SystemFilesystem, &at, backend).is_empty());
        assert!(!at.at("notes").exists(), "a folder emptied goes");
        assert!(at.at("empty").exists(), "one that was empty is no stray's");
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// What is named so no path takes it is found apart, its folder with all in it: nothing moves it by its path.
    #[cfg(unix)]
    #[test]
    fn what_no_path_names_is_found_apart() {
        let root = sfs::testing::scratch("strays-unnamed");
        let at = root.at("assets");
        std::fs::create_dir_all(at.at("notes")).unwrap();
        std::fs::write(at.at("notes").entry(std::ffi::OsStr::new("a:b.txt")), "x").unwrap();
        std::fs::create_dir_all(at.entry(std::ffi::OsStr::new("CON"))).unwrap();
        std::fs::write(at.at("kept.txt"), "k").unwrap();
        let found = strays(&pe2_io::SystemFilesystem, &at, crate::dav::BACKEND);
        assert_eq!(found.unnamed, ["CON", "notes/a:b.txt"]);
        assert_eq!(found.strays.iter().map(|path| path.as_str()).collect::<Vec<_>>(), ["kept.txt"]);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
