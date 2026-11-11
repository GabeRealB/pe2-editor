//! What the folder a project is served at holds that is not the project's, before it is served there: files written in
//! it while nothing served it - a program left open on it after the host stopped, a file put there by hand - and, where
//! the backend keeps what was read on disk, a file read on an earlier run and changed while nothing served it (a
//! placeholder no longer in sync). A mount would hide them, and placeholders would serve them beside the project's, or
//! in place of them: so the host does not serve over them, unless told to set them aside (`set_aside`) or take them in
//! as edits (`take_in`).

use std::io;
use std::path::{Path, PathBuf};

use crate::backend::Backend;
use crate::write::{Change, Owner, is_junk, refused};

/// What the folder at `at` holds that is not the project's, before a backend serves it there: its files, by their path
/// in it, `/`-separated, but those the backend left there as it served them. Folders are none: a folder holds nothing
/// of its own.
#[must_use]
pub fn strays(at: &Path, backend: &dyn Backend) -> Vec<String> {
    let mut found = Vec::new();
    walk(at, "", backend, &mut found);
    found.sort();
    found
}

fn walk(dir: &Path, at: &str, backend: &dyn Backend, found: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = if at.is_empty() { name } else { format!("{at}/{name}") };
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            walk(&entry.path(), &path, backend, found);
        } else if !backend.left_as_served(&entry.path()) {
            found.push(path);
        }
    }
}

/// Move what is not the project's out of the folder at `at`, keeping its paths, into `to`; folders left empty go.
///
/// # Errors
/// A file cannot be moved.
pub fn set_aside(at: &Path, strays: &[String], to: &Path) -> io::Result<()> {
    for stray in strays {
        let (from, into) = (at.join(stray), to.join(stray));
        if let Some(parent) = into.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // A move where it can be one, else a copy (another volume).
        if std::fs::rename(&from, &into).is_err() {
            std::fs::copy(&from, &into)?;
            std::fs::remove_file(&from)?;
        }
    }
    remove_emptied(at, strays);
    Ok(())
}

/// Take what is not the project's into it, as the edits a program writing it through the folder makes - each file
/// written, a `node.toml` before what is beside it - and out of the folder. What no edit takes - an editor's leftovers,
/// a name storage keeps - is set aside into `aside`. Returns what was set aside.
///
/// # Errors
/// A file cannot be read or moved, or an edit is refused: what was taken in before it stays taken.
pub fn take_in(
    at: &Path,
    strays: &[String],
    owner: &dyn Owner,
    staging: &Path,
    aside: &Path,
) -> io::Result<Vec<String>> {
    let (kept, taken): (Vec<String>, Vec<String>) = strays.iter().cloned().partition(|s| is_junk(s) || refused(s));
    let mut ordered = taken.clone();
    // A declaration first: it makes its folder a node, which what is beside it is then a file of.
    ordered.sort_by_key(|path| {
        let (folder, name) = path.rsplit_once('/').unwrap_or(("", path));
        (folder.to_string(), name != project::index::NODE_FILE, name.to_string())
    });
    std::fs::create_dir_all(staging)?;
    for path in &ordered {
        let staged: PathBuf = staging.join(format!("stray-{}", std::process::id()));
        std::fs::copy(at.join(path), &staged)?;
        let applied = owner.change(Change::Write { path: path.clone(), staged: staged.clone().into() });
        let _ = std::fs::remove_file(&staged);
        applied.map_err(|why| io::Error::other(format!("{path}: {why}")))?;
        std::fs::remove_file(at.join(path))?;
    }
    remove_emptied(at, &taken);
    set_aside(at, &kept, aside)?;
    Ok(kept)
}

/// Remove the folders these paths were in where they are left empty, deepest first.
fn remove_emptied(at: &Path, paths: &[String]) {
    let mut folders: Vec<&str> = paths.iter().flat_map(|p| p.match_indices('/').map(move |(i, _)| &p[..i])).collect();
    folders.sort_by_key(|f| std::cmp::Reverse(f.len()));
    folders.dedup();
    for folder in folders {
        // Only an empty one goes; one in sync is left to serving.
        let _ = std::fs::remove_dir(at.join(folder));
    }
}

#[cfg(test)]
mod tests {
    use super::{set_aside, strays};

    #[test]
    fn what_a_folder_holds_is_found_and_set_aside_with_its_paths() {
        let root = std::env::temp_dir().join(format!("pe2-strays-{}", std::process::id()));
        let (at, aside) = (root.join("assets"), root.join("aside"));
        std::fs::create_dir_all(at.join("notes/deep")).unwrap();
        std::fs::create_dir_all(at.join("empty")).unwrap();
        std::fs::write(at.join("stray.txt"), "s").unwrap();
        std::fs::write(at.join("notes/deep/c.md"), "c").unwrap();
        let backend = crate::dav::BACKEND;
        let found = strays(&at, backend);
        assert_eq!(found, ["notes/deep/c.md", "stray.txt"]);
        set_aside(&at, &found, &aside).unwrap();
        assert_eq!(std::fs::read_to_string(aside.join("notes/deep/c.md")).unwrap(), "c");
        assert_eq!(strays(&at, backend), Vec::<String>::new());
        assert!(!at.join("notes").exists(), "a folder emptied goes");
        assert!(at.join("empty").exists(), "one that was empty is no stray's");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
