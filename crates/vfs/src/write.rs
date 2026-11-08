//! What a write through the filesystem is to the project: a change (`Change`) its owner applies (`Owner`) - the editor
//! between frames, `pe2-cli project serve` at once - as an edit of the active layer, a step of the session's log that
//! undo takes back (`apply`).
//!
//! Paths are the tree's: a node is the folder of its name (no `.node`), its files in it beside the entries inside it.
//!
//! - A file of a node is written in the layer's own version of it, made first where it comes from below; one removed
//!   is removed from it. A new file belongs to the nearest node or folder it is in: in a node, it is one of its files.
//! - A plain file is written where it is, or made with a new uid; a folder made with one.
//! - A `node.toml` written in a folder makes it a node of that declaration - a folder there already becomes the node's
//!   folder of children. A node's `node.toml` is not removed: its folder is, deleting the node and what is in it.
//! - Removing an entry deletes it (a delete of what comes from below); renaming one moves it, its uid kept - a node
//!   with what is inside it.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use project::history::History;
use project::index::{BUNDLE_SUFFIX, Index, Kind, NODE_FILE};
use project::{Project, edit};

/// A change a program made through the filesystem, by resolved paths.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// A file's new content, kept at `staged` until applied.
    Write {
        path: String,
        staged: PathBuf,
    },
    Remove {
        path: String,
    },
    Rename {
        from: String,
        to: String,
    },
    Folder {
        path: String,
    },
}

impl Change {
    /// How the history names it.
    #[must_use]
    pub fn label(&self) -> String {
        let name = |path: &str| path.rsplit('/').next().unwrap_or(path).to_string();
        match self {
            Change::Write { path, .. } => format!("Write {} from outside", name(path)),
            Change::Remove { path } => format!("Remove {} from outside", name(path)),
            Change::Rename { from, to } => format!("Rename {} to {} from outside", name(from), name(to)),
            Change::Folder { path } => format!("New {} from outside", name(path)),
        }
    }
}

/// What applies the changes a filesystem's programs make: whoever holds the project.
pub trait Owner: Send + Sync {
    /// Apply a change; once this returns, the tree served shows it, or will on its next update.
    ///
    /// # Errors
    /// The change cannot be made: why, for the program to be told it failed.
    fn change(&self, change: Change) -> Result<(), String>;
}

/// Files programs make for themselves beside what they edit - swap files, backups, lock files - which stay in the
/// filesystem's staging while it serves and never reach a layer.
#[must_use]
pub fn is_junk(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    // A name starting with `.` is none of the project's: the index leaves it out.
    name.starts_with('.')
        || name.ends_with('~')
        || name == "4913"
        || name.starts_with(".~lock.")
        || name.starts_with(".#")
        || (name.starts_with('.') && [".swp", ".swo", ".swx", ".swn"].iter().any(|s| name.ends_with(s)))
        || name == ".DS_Store"
        || name.starts_with("._")
}

/// What a path of the tree is in what the index resolves.
enum Place<'a> {
    /// An entry, by its real path: a node (`x.node`), a folder or a plain file.
    Entry(&'a str, &'a project::index::Entry),
    /// A file of a node (its real path, its entry), by its name in it.
    File(&'a str, &'a project::index::Entry, String),
    /// Nothing yet.
    New,
}

/// Where a path of the tree is: a node or entry of its own; a file of the nearest node it is in; or nothing yet. A
/// folder or plain file on the way makes it a new path of that folder.
fn place<'a>(index: &'a Index, path: &str) -> Place<'a> {
    let entry = |real: &str| index.entries.get_key_value(real);
    if let Some((key, found)) = entry(&format!("{path}{BUNDLE_SUFFIX}")).or_else(|| entry(path)) {
        return Place::Entry(key.as_str(), found);
    }
    let mut at = path.len();
    while let Some(slash) = path[..at].rfind('/') {
        let prefix = &path[..slash];
        if let Some((real, node)) = entry(&format!("{prefix}{BUNDLE_SUFFIX}")) {
            return Place::File(real.as_str(), node, path[slash + 1..].to_string());
        }
        if entry(prefix).is_some() {
            return Place::New;
        }
        at = slash;
    }
    Place::New
}

/// Whether a path of the tree is a node, or inside one: what a folder made there waits on.
#[must_use]
pub fn in_node(index: &Index, path: &str) -> bool {
    matches!(place(index, path), Place::File(..))
        || index.entries.get(&format!("{path}{BUNDLE_SUFFIX}")).is_some_and(|e| e.kind == Kind::Asset)
}

/// A name the tree does not take: what storage marks a node with.
#[must_use]
pub fn refused(path: &str) -> bool {
    path.split('/').any(|step| step.ends_with(BUNDLE_SUFFIX))
}

/// The layer a filesystem's writes go to where nothing chooses one: the highest the project lets be written.
#[must_use]
pub fn writable_layer(index: &Index) -> Option<String> {
    index.layers().into_iter().rev().find(|l| l.writable && !l.mounted).map(|l| l.name)
}

/// Apply a change to a layer, as a step of the history. Returns the paths it touched, for the index to take in.
///
/// # Errors
/// What it names is not there, cannot be made there, or the layer cannot be written.
pub fn apply(
    change: &Change,
    project: &Project,
    index: &Index,
    layer: &str,
    history: &mut History,
) -> Result<Vec<PathBuf>> {
    let label = change.label();
    match change {
        Change::Write { path, staged } => {
            let bytes = std::fs::read(staged).with_context(|| staged.display().to_string())?;
            write(project, index, layer, path, &bytes, label, history)
        }
        Change::Remove { path } => match place(index, path) {
            Place::Entry(_, entry) => edit::delete(project, index, layer, &entry.uid, history),
            Place::File(_, _, file) if file == NODE_FILE => {
                bail!("{path}: a node's declaration is not removed; remove its folder to delete the node")
            }
            // A file of the node, or a folder of its files with all of them.
            Place::File(real, node, file) => {
                let asset = index.assets.get(real.strip_suffix(BUNDLE_SUFFIX).unwrap_or(real));
                let inside = format!("{file}/");
                let files = asset.map_or(&[][..], |a| a.on_disk().files.as_slice());
                let mut gone: Vec<(String, Option<Vec<u8>>)> =
                    files.iter().filter(|f| f.name.starts_with(&inside)).map(|f| (f.name.clone(), None)).collect();
                if gone.is_empty() {
                    gone.push((file, None));
                }
                edit::save(project, index, layer, &node.uid, &gone, label, history)
            }
            Place::New => bail!("{path}: nothing is there"),
        },
        Change::Rename { from, to } => rename(project, index, layer, from, to, label, history),
        Change::Folder { path } => edit::create_folder(project, index, layer, path, history),
    }
}

fn write(
    project: &Project,
    index: &Index,
    layer: &str,
    path: &str,
    bytes: &[u8],
    label: String,
    history: &mut History,
) -> Result<Vec<PathBuf>> {
    // A declaration makes its folder a node, wherever it is - in a folder, or inside another node - unless that folder
    // is a node already: one there already is its folder of children.
    if let Some(folder) = path.strip_suffix(&format!("/{NODE_FILE}")).filter(|f| !f.is_empty())
        && !index.entries.contains_key(&format!("{folder}{BUNDLE_SUFFIX}"))
    {
        let declaration = std::str::from_utf8(bytes).context("a declaration is not text")?;
        let uid = new_uid(index)?;
        return edit::create(project, index, layer, &uid, folder, (declaration, &[]), history);
    }
    match place(index, path) {
        Place::Entry(_, entry) if entry.kind == Kind::File => {
            edit::write_file(project, index, layer, &entry.uid, "", bytes, label, None, history)
        }
        Place::Entry(..) => bail!("{path} is a folder"),
        Place::File(_, node, file) => {
            edit::write_file(project, index, layer, &node.uid, &file, bytes, label, None, history)
        }
        Place::New if path == NODE_FILE => bail!("the top is no node"),
        Place::New => edit::create_file(project, index, layer, path, bytes, history),
    }
}

fn rename(
    project: &Project,
    index: &Index,
    layer: &str,
    from: &str,
    to: &str,
    label: String,
    history: &mut History,
) -> Result<Vec<PathBuf>> {
    match place(index, from) {
        Place::Entry(real, entry) => {
            let mut touched = Vec::new();
            if let Place::Entry(_, there) = place(index, to) {
                touched.extend(edit::delete(project, index, layer, &there.uid, history)?);
            }
            // A node keeps being one: stored with its mark.
            let to = if real.ends_with(BUNDLE_SUFFIX) { format!("{to}{BUNDLE_SUFFIX}") } else { to.to_string() };
            touched.extend(edit::rename(project, index, layer, &entry.uid, &to, history)?);
            Ok(touched)
        }
        // A file of a node renamed within it: written under its new name, removed under its old.
        Place::File(real, node, file) => {
            let Place::File(_, target, new) = place(index, to) else { bail!("{from} can move only within its node") };
            if target.uid != node.uid || file == NODE_FILE || new == NODE_FILE {
                bail!("{from} can move only within its node, and a declaration not at all");
            }
            // Where the file is: the node's own, or one below it lets through.
            let asset = index.assets.get(real.strip_suffix(BUNDLE_SUFFIX).unwrap_or(real));
            let disk = asset
                .and_then(|a| a.on_disk().files.iter().find(|f| f.name == file).map(|f| f.path.clone()))
                .with_context(|| format!("{from}: not there"))?;
            let bytes = project::content::read(&disk).with_context(|| disk.display().to_string())?;
            let files = [(new, Some(bytes)), (file, None)];
            edit::save(project, index, layer, &node.uid, &files, label, history)
        }
        Place::New => bail!("{from}: nothing is there"),
    }
}

fn new_uid(index: &Index) -> Result<String> {
    let id = project::uid::new(|id| index.by_uid.contains_key(&project::uid::text(id)))?;
    Ok(project::uid::text(id))
}

#[cfg(test)]
mod tests {
    use super::is_junk;

    #[test]
    fn junk_is_what_editors_leave_beside_a_file() {
        for junk in [
            "a/.node.toml.swp",
            "a/node.toml~",
            "a/4913",
            "a/.~lock.x.ods#",
            "a/.#x",
            "._x",
            "a/.DS_Store",
            "a/.gitignore",
        ] {
            assert!(is_junk(junk), "{junk}");
        }
        for kept in ["a/node.toml", "a/x.swp.png", "a/settings.toml"] {
            assert!(!is_junk(kept), "{kept}");
        }
    }
}
