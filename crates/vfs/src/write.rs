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

/// Where what a program wrote is, until it is applied.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Written {
    /// A file of its own.
    File(PathBuf),
    /// In the log the project's processes share (`sfs::Log`), kept in this folder: how long it is, and its ranges -
    /// where each starts in the file, how long it is, where it is in the log. Read where the log is mapped.
    Logged { log: PathBuf, len: u64, ranges: Vec<(u64, u64, u64)> },
}

impl From<PathBuf> for Written {
    fn from(file: PathBuf) -> Written {
        Written::File(file)
    }
}

impl Written {
    /// All of it.
    ///
    /// # Errors
    /// Its file, or the log, cannot be read.
    pub fn read(&self) -> std::io::Result<Vec<u8>> {
        match self {
            Written::File(file) => std::fs::read(file),
            Written::Logged { log, len, ranges } => {
                let log = sfs::Log::shared(log, sfs::log::Config::default())?;
                let mut bytes = Vec::with_capacity(usize::try_from(*len).unwrap_or(0));
                sfs::Content::of_ranges(*len, ranges).write_to(&log, &mut bytes)?;
                Ok(bytes)
            }
        }
    }

    /// What it is said as, where reading it failed.
    fn shown(&self) -> String {
        match self {
            Written::File(file) => file.display().to_string(),
            Written::Logged { log, .. } => format!("the log in {}", log.display()),
        }
    }
}

/// The logs whoever applies changes has read from: kept open, since opening one beside another process that has
/// it takes a moment.
fn keep_open(written: &Written) {
    static KEPT: std::sync::Mutex<Vec<std::sync::Arc<sfs::Log>>> = std::sync::Mutex::new(Vec::new());
    let Written::Logged { log, .. } = written else { return };
    let Ok(log) = sfs::Log::shared(log, sfs::log::Config::default()) else { return };
    let mut kept = KEPT.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if !kept.iter().any(|open| std::sync::Arc::ptr_eq(open, &log)) {
        kept.push(log);
    }
}

/// A change a program made through the filesystem, by resolved paths.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// A file's new content, kept where `staged` says until applied.
    Write {
        path: String,
        staged: Written,
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

/// Why a change was not made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// It cannot be made: why, for the program to be told it failed.
    Never(String),
    /// Nobody takes changes this moment - what holds the project does not (a tool at work on it, an editor still
    /// opening): why. Asked again later, it is made: what was written is kept for it.
    Later(String),
}

impl Refusal {
    /// Whether the change is to be asked again.
    #[must_use]
    pub fn later(&self) -> bool {
        matches!(self, Refusal::Later(_))
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (Refusal::Never(why) | Refusal::Later(why)) = self;
        f.write_str(why)
    }
}

impl From<String> for Refusal {
    fn from(why: String) -> Refusal {
        Refusal::Never(why)
    }
}

/// What applies the changes a filesystem's programs make: whoever holds the project.
pub trait Owner: Send + Sync {
    /// Apply a change; once this returns, the tree served shows it, or will on its next update.
    ///
    /// # Errors
    /// The change cannot be made (`Refusal::Never`: why, for the program to be told it failed), or nobody takes one
    /// now (`Later`).
    fn change(&self, change: Change) -> Result<(), Refusal>;

    /// Apply several changes that do not depend on each other, in order: how each went. One that can applies them
    /// together - a step of the history each, what follows from them (its index, the tree served) brought up to date
    /// once for all - which is what makes many files written at once land in the time of a few.
    fn changes(&self, changes: Vec<Change>) -> Vec<Result<(), Refusal>> {
        changes.into_iter().map(|change| self.change(change)).collect()
    }

    /// Whether a change would be taken this moment, asked without giving one.
    ///
    /// # Errors
    /// Nobody takes one now: why.
    fn ready(&self) -> Result<(), Refusal> {
        Ok(())
    }

    /// What a program wrote at this path, staged in this file, was not given to the project by the time serving
    /// ends: the owner's to keep, where it can - the staging is emptied when the folder is next served.
    fn left(&self, _path: &str, _staged: &std::path::Path) {}
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

/// Whether something is at a path of the tree: an entry, or a file of the node it is in - or files under it there.
fn there(index: &Index, path: &str) -> bool {
    match place(index, path) {
        Place::Entry(..) => true,
        Place::File(real, _, file) => {
            let asset = index.assets.get(real.strip_suffix(BUNDLE_SUFFIX).unwrap_or(real));
            let inside = format!("{file}/");
            asset.is_some_and(|a| a.on_disk().files.iter().any(|f| f.name == file || f.name.starts_with(&inside)))
        }
        Place::New => false,
    }
}

/// Whether what a change asks is so already: a folder that is there, a thing moved to where it is asked to be and
/// gone from where it was, a thing gone. What something due is when it is taken a second time - by whoever holds
/// the project after one that made it and ended before saying so, or said due again by a serving after one that
/// ended: made again it would be refused, and whoever serves would show it undone. Never a written file, which is
/// written again as well as once.
#[must_use]
pub fn so_already(index: &Index, change: &Change) -> bool {
    match change {
        Change::Folder { path } => matches!(place(index, path), Place::Entry(_, entry) if entry.kind != Kind::File),
        Change::Rename { from, to } => !there(index, from) && there(index, to),
        Change::Remove { path } => !there(index, path),
        Change::Write { .. } => false,
    }
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
            keep_open(staged);
            let bytes = staged.read().with_context(|| staged.shown())?;
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
