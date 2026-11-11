//! What can be done to a project from outside the editor's window, by path - the command line's edits, a program's write
//! through the served folder - as one vocabulary (`Op`), carried as JSON to whoever holds the project and applied there
//! (`apply`) as a step of its history. Paths are the served tree's (`packages/aya`, no `.node`); what a problem names
//! (`forget`, `adopt`, `remove-unlisted`) is a layer's own path, as the problem gives it. A file's content goes by the
//! path of a file holding it (`from`), never inline: what applies it reads it there.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use project::Project;
use project::edit::{self, Resolution};
use project::history::History;
use project::index::{BUNDLE_SUFFIX, Index};
use serde::{Deserialize, Serialize};

use crate::write::Change;

/// An edit of a project, by path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// Write a file - of a node, or a plain one - with what a file holds; a `node.toml` in a folder makes it a node.
    Write {
        path: String,
        from: crate::write::Written,
    },
    /// Make a folder.
    Folder {
        path: String,
    },
    /// Delete an entry (a node with what is inside it), or remove a file of a node.
    Remove {
        path: String,
    },
    /// Rename or move an entry, or a file within its node.
    Rename {
        from: String,
        to: String,
    },
    /// Make the layer's own version of an entry from below.
    Override {
        path: String,
    },
    /// Drop the layer's operation on an entry - an override, a move, a delete - and what it holds of it.
    Drop {
        path: String,
    },
    /// Keep the layer's version of a stale override: its base is what is below now.
    KeepMine {
        path: String,
    },
    /// Give up the layer's version of an entry for the one below (a move with it stays).
    TakeBase {
        path: String,
    },
    /// Take an entry a layer lists and no longer holds out of its list, by the layer's own path.
    Forget {
        layer: String,
        path: String,
    },
    /// List what a layer holds unlisted at and under its own path.
    Adopt {
        layer: String,
        path: String,
    },
    /// Remove what a layer holds unlisted at its own path from its folder.
    RemoveUnlisted {
        layer: String,
        path: String,
    },
    /// Take back the last step, or do again the last taken back.
    Undo,
    Redo,
}

impl From<Change> for Op {
    fn from(change: Change) -> Op {
        match change {
            Change::Write { path, staged } => Op::Write { path, from: staged },
            Change::Remove { path } => Op::Remove { path },
            Change::Rename { from, to } => Op::Rename { from, to },
            Change::Folder { path } => Op::Folder { path },
        }
    }
}

impl Op {
    /// Whether it changes what a layer holds as a program writes it (a file, a folder, a removal, a move): what the
    /// served folder sends.
    fn as_change(&self) -> Option<Change> {
        Some(match self {
            Op::Write { path, from } => Change::Write { path: path.clone(), staged: from.clone() },
            Op::Folder { path } => Change::Folder { path: path.clone() },
            Op::Remove { path } => Change::Remove { path: path.clone() },
            Op::Rename { from, to } => Change::Rename { from: from.clone(), to: to.clone() },
            _ => return None,
        })
    }
}

/// The uid of the entry at a path of the tree: a node's (`x` for `x.node`), a folder's or a file's.
fn uid_at(index: &Index, path: &str) -> Result<String> {
    let entry = index.entries.get(&format!("{path}{BUNDLE_SUFFIX}")).or_else(|| index.entries.get(path));
    entry.map(|e| e.uid.clone()).with_context(|| format!("{path}: nothing is there"))
}

/// Apply an edit to a layer, as a step of the history: the step's label, and the paths it touched for the index to take
/// in.
///
/// # Errors
/// What it names is not there or cannot be done there, the layer cannot be written, or there is nothing to take back
/// or do again.
pub fn apply(
    op: &Op,
    project: &Project,
    index: &Index,
    layer: &str,
    history: &mut History,
) -> Result<(String, Vec<PathBuf>)> {
    let touched = match op {
        _ if let Some(change) = op.as_change() => crate::write::apply(&change, project, index, layer, history)?,
        Op::Override { path } => edit::override_entry(project, index, layer, &uid_at(index, path)?, history)?,
        Op::Drop { path } => edit::resolve(project, index, layer, &uid_at(index, path)?, Resolution::Drop, history)?,
        Op::KeepMine { path } => {
            edit::resolve(project, index, layer, &uid_at(index, path)?, Resolution::KeepMine, history)?
        }
        Op::TakeBase { path } => {
            edit::resolve(project, index, layer, &uid_at(index, path)?, Resolution::TakeBase, history)?
        }
        Op::Forget { layer: of, path } => {
            let uid = index.listed(of, path).with_context(|| format!("{of} does not list {path}"))?.to_string();
            edit::forget(project, index, of, &uid, history)?
        }
        Op::Adopt { layer: of, path } => edit::adopt(project, index, of, path, history)?,
        Op::RemoveUnlisted { layer: of, path } => edit::remove_unlisted(project, index, of, path, history)?,
        Op::Undo => {
            let step = history.undo(index, None, false).map_err(|e| anyhow::anyhow!("{e}"))?;
            return Ok((format!("Undid {}", step.label), step.paths()));
        }
        Op::Redo => {
            let step = history.redo(index, None, false).map_err(|e| anyhow::anyhow!("{e}"))?;
            return Ok((format!("Redid {}", step.label), step.paths()));
        }
        Op::Write { .. } | Op::Folder { .. } | Op::Remove { .. } | Op::Rename { .. } => bail!("not a change"),
    };
    let label = history.done().last().map_or_else(String::new, |s| s.label.clone());
    Ok((label, touched))
}

#[cfg(test)]
mod tests {
    use super::Op;

    #[test]
    fn an_op_is_json_by_its_name() {
        let op = Op::Rename { from: "a".into(), to: "b".into() };
        let text = serde_json::to_string(&op).unwrap();
        assert_eq!(text, r#"{"op":"rename","from":"a","to":"b"}"#);
        assert_eq!(serde_json::from_str::<Op>(&text).unwrap(), op);
        assert_eq!(serde_json::to_string(&Op::Undo).unwrap(), r#"{"op":"undo"}"#);
    }
}
