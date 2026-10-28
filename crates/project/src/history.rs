//! Undo and redo: every modification of a project is a transaction, and undoing it is the transaction run backwards.
//!
//! A transaction records what it does to files - a file written (its content before and after, by hash; absent is a
//! state too, so creating, overwriting and deleting a file are all writes), a file or folder renamed, a folder made
//! for a write - and applies each change as it is made. Whatever an operation is (an inspector edit, a copy-on-write
//! override, a changeset changed, `project.toml` rewritten), undo restores the files as they were, so no operation
//! brings undo code of its own. What a change overwrites or deletes is kept in the cache: a file's content by its hash
//! (`.cache/history/blobs/<sha1>`, once), a folder deleted moved whole into `.cache/history/trash/`.
//!
//! The history lasts the session: opening a project wipes `.cache/history/`. Apart from it, the transaction being
//! applied is recorded first (`.cache/history/applying`), so that one a crash leaves half done is rolled back when
//! the project is opened again. Undoing a step checks that its files are as it left them; one changed since outside
//! the editor blocks it, unless forced - and a forced undo records what it overwrote, so redo brings that back.
//!
//! Only the holder of the project lock writes (`lock.rs`).

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::cache::{Cache, write_atomic};
use crate::extract::sha1_hex;

const DIR: &str = "history";
const APPLYING: &str = "applying";
/// How many steps a session keeps.
const STEPS: usize = 500;
/// How close in time edits of one kind (`Transaction::merging`) must follow each other to make one step.
const MERGE_WITHIN: Duration = Duration::from_millis(1500);

/// A change to the project's files.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Change {
    /// A file's content before and after, by hash; none: absent.
    Write { path: PathBuf, before: Option<String>, after: Option<String> },
    /// A file or a folder moved.
    Rename { from: PathBuf, to: PathBuf },
    /// A folder made (for a file written into it).
    MakeDir { path: PathBuf },
    /// An empty folder removed.
    RemoveDir { path: PathBuf },
}

impl Change {
    fn inverse(&self) -> Change {
        match self {
            Change::Write { path, before, after } => {
                Change::Write { path: path.clone(), before: after.clone(), after: before.clone() }
            }
            Change::Rename { from, to } => Change::Rename { from: to.clone(), to: from.clone() },
            Change::MakeDir { path } => Change::RemoveDir { path: path.clone() },
            Change::RemoveDir { path } => Change::MakeDir { path: path.clone() },
        }
    }

    /// The paths it touches.
    fn paths(&self) -> Vec<&Path> {
        match self {
            Change::Write { path, .. } | Change::MakeDir { path } | Change::RemoveDir { path } => vec![path],
            Change::Rename { from, to } => vec![from, to],
        }
    }
}

/// A step of the history: what it was, who made it, what it changed, and what was selected when it was made.
#[derive(Clone, Debug)]
pub struct Step {
    pub label: String,
    pub author: String,
    pub changes: Vec<Change>,
    pub selection: Option<String>,
    merge: Option<(String, Instant)>,
}

impl Step {
    /// Every path it touches.
    #[must_use]
    pub fn paths(&self) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = self.changes.iter().flat_map(Change::paths).map(Path::to_path_buf).collect();
        paths.sort();
        paths.dedup();
        paths
    }
}

/// Why a step cannot be undone or redone.
#[derive(Debug)]
pub enum StepError {
    /// Files it touches changed since outside the editor: forcing it overwrites them.
    Changed(Vec<PathBuf>),
    Io(anyhow::Error),
}

impl std::fmt::Display for StepError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StepError::Changed(paths) => {
                let names: Vec<String> = paths.iter().take(3).map(|p| p.display().to_string()).collect();
                let more = if paths.len() > 3 { format!(" and {} more", paths.len() - 3) } else { String::new() };
                write!(f, "changed outside the editor since: {}{more}", names.join(", "))
            }
            StepError::Io(e) => write!(f, "{e:#}"),
        }
    }
}

impl std::error::Error for StepError {}

pub struct History {
    dir: PathBuf,
    done: Vec<Step>,
    undone: Vec<Step>,
    trashed: usize,
}

impl History {
    /// The project's history for this session: a transaction a crash left half applied is rolled back first, then
    /// what an earlier session kept is wiped. Returns it and the label of what was rolled back, if anything was.
    ///
    /// # Errors
    /// The history's folder cannot be read, cleared or made.
    pub fn open(cache: &Cache) -> Result<(History, Option<String>)> {
        let dir = cache.path(DIR);
        let mut recovered = None;
        if let Ok(text) = std::fs::read_to_string(dir.join(APPLYING)) {
            let (label, applied): (String, Vec<Change>) = serde_json::from_str(&text).unwrap_or_default();
            let history = History { dir: dir.clone(), done: vec![], undone: vec![], trashed: 0 };
            for change in applied.iter().rev() {
                // Best effort: what the crash left is put back as it was before.
                let _ = history.apply(&change.inverse());
            }
            recovered = Some(label);
        }
        if dir.exists() {
            std::fs::remove_dir_all(&dir).with_context(|| format!("{}", dir.display()))?;
        }
        std::fs::create_dir_all(dir.join("blobs")).with_context(|| format!("{}", dir.display()))?;
        Ok((History { dir, done: vec![], undone: vec![], trashed: 0 }, recovered))
    }

    /// Begin a transaction, named as the user will see it in the history.
    pub fn begin(&mut self, label: impl Into<String>) -> Transaction<'_> {
        let step = Step { label: label.into(), author: "user".into(), changes: vec![], selection: None, merge: None };
        Transaction { history: self, step, committed: false }
    }

    /// Note what was selected when the last step was made: undoing it shows that again.
    pub fn note_selection(&mut self, selection: Option<String>) {
        if let Some(step) = self.done.last_mut() {
            step.selection = selection;
        }
    }

    /// The steps done, oldest first.
    #[must_use]
    pub fn done(&self) -> &[Step] {
        &self.done
    }

    /// The steps undone, the next to redo last.
    #[must_use]
    pub fn undone(&self) -> &[Step] {
        &self.undone
    }

    /// Undo the last step. Returns it.
    ///
    /// # Errors
    /// There is nothing to undo, its files changed since outside the editor (unless forced), or they cannot be
    /// written.
    pub fn undo(&mut self, force: bool) -> Result<&Step, StepError> {
        let Some(mut step) = self.done.pop() else {
            return Err(StepError::Io(anyhow::anyhow!("nothing to undo")));
        };
        let result = self.run(&mut step, false, force);
        match result {
            Ok(()) => {
                self.undone.push(step);
                self.undone.last().ok_or_else(|| StepError::Io(anyhow::anyhow!("the step undone is gone")))
            }
            Err(e) => {
                self.done.push(step);
                Err(e)
            }
        }
    }

    /// Redo the step undone last. Returns it.
    ///
    /// # Errors
    /// There is nothing to redo, its files changed since outside the editor (unless forced), or they cannot be
    /// written.
    pub fn redo(&mut self, force: bool) -> Result<&Step, StepError> {
        let Some(mut step) = self.undone.pop() else {
            return Err(StepError::Io(anyhow::anyhow!("nothing to redo")));
        };
        let result = self.run(&mut step, true, force);
        match result {
            Ok(()) => {
                self.done.push(step);
                self.done.last().ok_or_else(|| StepError::Io(anyhow::anyhow!("the step redone is gone")))
            }
            Err(e) => {
                self.undone.push(step);
                Err(e)
            }
        }
    }

    /// Run a step forwards (redo) or backwards (undo): check its files are as it left them, then apply.
    fn run(&mut self, step: &mut Step, forwards: bool, force: bool) -> Result<(), StepError> {
        let list: Vec<Change> =
            if forwards { step.changes.clone() } else { step.changes.iter().rev().map(Change::inverse).collect() };
        let differing = Self::changed(&list);
        if !differing.is_empty() {
            if !force {
                return Err(StepError::Changed(differing));
            }
            // What is there now is what the step will have left: undoing it again brings it back.
            self.adopt(step, forwards).map_err(StepError::Io)?;
            return self.run(step, forwards, false);
        }
        self.apply_all(&step.label, &list).map_err(StepError::Io)
    }

    /// The files not as a list of changes expects them, followed in order: a file written twice is expected as the
    /// first write left it. Folders are not checked: one made is there or not, and one to remove that holds something
    /// now is left alone.
    fn changed(list: &[Change]) -> Vec<PathBuf> {
        let mut state: std::collections::HashMap<&Path, Option<String>> = std::collections::HashMap::new();
        let mut differing = vec![];
        for change in list {
            match change {
                Change::Write { path, before, after } => {
                    let now = state.get(path.as_path()).cloned().unwrap_or_else(|| current_hash(path));
                    if now != *before {
                        differing.push(path.clone());
                    }
                    state.insert(path, after.clone());
                }
                Change::Rename { from, to } => {
                    let untouched = !state.contains_key(from.as_path()) && !state.contains_key(to.as_path());
                    if untouched && (!from.exists() || to.exists()) {
                        differing.push(from.clone());
                    }
                }
                Change::MakeDir { path } => {
                    if path.exists() && !path.is_dir() {
                        differing.push(path.clone());
                    }
                }
                Change::RemoveDir { .. } => {}
            }
        }
        differing
    }

    /// Take the files as they are now as what the step leaves (before undoing it) or starts from (before redoing).
    fn adopt(&mut self, step: &mut Step, forwards: bool) -> Result<()> {
        for change in &mut step.changes {
            if let Change::Write { path, before, after } = change {
                let now = self.keep_current(path)?;
                if forwards { *before = now } else { *after = now }
            }
        }
        Ok(())
    }

    /// Apply changes, recorded first so that a crash midway is rolled back on the next opening.
    fn apply_all(&self, label: &str, changes: &[Change]) -> Result<()> {
        let mut applied: Vec<Change> = vec![];
        for change in changes {
            applied.push(change.clone());
            self.record(label, &applied)?;
            if let Err(e) = self.apply(change) {
                for done in applied[..applied.len() - 1].iter().rev() {
                    let _ = self.apply(&done.inverse());
                }
                self.settle()?;
                return Err(e);
            }
        }
        self.settle()
    }

    fn record(&self, label: &str, applied: &[Change]) -> Result<()> {
        write_atomic(&self.dir.join(APPLYING), serde_json::to_string(&(label, applied))?.as_bytes())
    }

    /// No transaction is being applied.
    fn settle(&self) -> Result<()> {
        match std::fs::remove_file(self.dir.join(APPLYING)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    fn apply(&self, change: &Change) -> Result<()> {
        match change {
            Change::Write { path, after: Some(hash), .. } => write_atomic(path, &self.blob(hash)?),
            Change::Write { path, after: None, .. } => match std::fs::remove_file(path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e).context(path.display().to_string()),
                _ => Ok(()),
            },
            Change::Rename { from, to } => {
                std::fs::rename(from, to).with_context(|| format!("{} -> {}", from.display(), to.display()))
            }
            Change::MakeDir { path } => {
                if path.is_dir() {
                    return Ok(());
                }
                std::fs::create_dir(path).with_context(|| path.display().to_string())
            }
            // A folder that holds something now (put there since, outside the editor) is left as it is.
            Change::RemoveDir { path } => {
                if is_empty_dir(path) {
                    std::fs::remove_dir(path).with_context(|| path.display().to_string())?;
                }
                Ok(())
            }
        }
    }

    fn blob_path(&self, hash: &str) -> PathBuf {
        self.dir.join("blobs").join(hash)
    }

    fn blob(&self, hash: &str) -> Result<Vec<u8>> {
        let path = self.blob_path(hash);
        std::fs::read(&path).with_context(|| format!("{}: a content the history kept is gone", path.display()))
    }

    /// Keep bytes in the history; their hash.
    fn keep(&self, bytes: &[u8]) -> Result<String> {
        let hash = sha1_hex(bytes);
        let path = self.blob_path(&hash);
        if !path.exists() {
            write_atomic(&path, bytes)?;
        }
        Ok(hash)
    }

    /// Keep a file's current content; its hash, none where it is absent.
    fn keep_current(&self, path: &Path) -> Result<Option<String>> {
        match std::fs::read(path) {
            Ok(bytes) => self.keep(&bytes).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| path.display().to_string()),
        }
    }

    fn push(&mut self, mut step: Step) {
        self.undone.clear();
        if let (Some(last), Some((key, _))) = (self.done.last_mut(), &step.merge)
            && last.merge.as_ref().is_some_and(|(k, at)| k == key && at.elapsed() < MERGE_WITHIN)
        {
            // One edit continued: the step keeps where it started from and takes where it ends - the last write of a
            // path continued, a step that wrote a file twice (a copy, then an edit of it) keeping the first.
            for change in step.changes.drain(..) {
                let merged = match (&change, last.changes.iter_mut().rev().find(|c| c.paths() == change.paths())) {
                    (Change::Write { after, .. }, Some(Change::Write { after: last_after, .. })) => {
                        last_after.clone_from(after);
                        true
                    }
                    _ => false,
                };
                if !merged {
                    last.changes.push(change);
                }
            }
            last.merge = step.merge;
            return;
        }
        self.done.push(step);
        if self.done.len() > STEPS {
            self.done.remove(0);
        }
    }
}

/// A modification being made: its changes are applied as they are made, and kept as one step when it is committed.
/// One dropped uncommitted - an error midway - is rolled back.
pub struct Transaction<'h> {
    history: &'h mut History,
    step: Step,
    committed: bool,
}

impl Transaction<'_> {
    /// Who makes it (the user, an agent, an extension), as the history shows it.
    #[must_use]
    pub fn by(mut self, author: impl Into<String>) -> Self {
        self.step.author = author.into();
        self
    }

    /// What was selected, which undoing it shows again.
    #[must_use]
    pub fn selecting(mut self, selection: Option<String>) -> Self {
        self.step.selection = selection;
        self
    }

    /// One continued edit (typing in a field, dragging a value): committed soon after another of the same key, it
    /// joins that step.
    #[must_use]
    pub fn merging(mut self, key: impl Into<String>) -> Self {
        self.step.merge = Some((key.into(), Instant::now()));
        self
    }

    fn change(&mut self, change: Change) -> Result<()> {
        self.step.changes.push(change);
        self.history.record(&self.step.label, &self.step.changes)?;
        let applied = self.step.changes.last().map(|c| self.history.apply(c));
        applied.unwrap_or(Ok(()))
    }

    /// Make the folders a path needs that do not exist.
    fn make_parents(&mut self, path: &Path) -> Result<()> {
        let mut missing: Vec<PathBuf> = path
            .ancestors()
            .skip(1)
            .take_while(|d| !d.as_os_str().is_empty() && !d.exists())
            .map(Path::to_path_buf)
            .collect();
        missing.reverse();
        for dir in missing {
            self.change(Change::MakeDir { path: dir })?;
        }
        Ok(())
    }

    /// Write a file: made, or overwritten.
    ///
    /// # Errors
    /// It cannot be written, or the history cannot keep what it was.
    pub fn write(&mut self, path: &Path, bytes: &[u8]) -> Result<()> {
        self.make_parents(path)?;
        let before = self.history.keep_current(path)?;
        let after = Some(self.history.keep(bytes)?);
        if before == after {
            return Ok(());
        }
        self.change(Change::Write { path: path.to_path_buf(), before, after })
    }

    /// Make a folder (and those it needs).
    ///
    /// # Errors
    /// It cannot be made.
    pub fn make_dir(&mut self, path: &Path) -> Result<()> {
        self.make_parents(path)?;
        if !path.is_dir() {
            self.change(Change::MakeDir { path: path.to_path_buf() })?;
        }
        Ok(())
    }

    /// Remove a file, or a folder with everything in it (moved into the history's trash, whole).
    ///
    /// # Errors
    /// It does not exist or cannot be removed.
    pub fn remove(&mut self, path: &Path) -> Result<()> {
        if path.is_dir() {
            self.history.trashed += 1;
            let trash = self.history.dir.join("trash");
            std::fs::create_dir_all(&trash)?;
            let to = trash.join(self.history.trashed.to_string());
            return self.change(Change::Rename { from: path.to_path_buf(), to });
        }
        let before = self.history.keep_current(path)?;
        if before.is_none() {
            bail!("{}: nothing to remove", path.display());
        }
        self.change(Change::Write { path: path.to_path_buf(), before, after: None })
    }

    /// Move a file or a folder.
    ///
    /// # Errors
    /// The source does not exist, the target does, or the move fails.
    pub fn rename(&mut self, from: &Path, to: &Path) -> Result<()> {
        if !from.exists() || to.exists() {
            bail!("{} cannot move to {}", from.display(), to.display());
        }
        self.make_parents(to)?;
        self.change(Change::Rename { from: from.to_path_buf(), to: to.to_path_buf() })
    }

    /// Keep it as a step of the history. Returns the paths it touched.
    ///
    /// # Errors
    /// The record of the transaction cannot be cleared.
    pub fn commit(mut self) -> Result<Vec<PathBuf>> {
        self.committed = true;
        self.history.settle()?;
        let paths = self.step.paths();
        if !self.step.changes.is_empty() {
            let step = std::mem::replace(
                &mut self.step,
                Step { label: String::new(), author: String::new(), changes: vec![], selection: None, merge: None },
            );
            self.history.push(step);
        }
        Ok(paths)
    }
}

impl Drop for Transaction<'_> {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        for change in self.step.changes.iter().rev() {
            let _ = self.history.apply(&change.inverse());
        }
        let _ = self.history.settle();
    }
}

fn current_hash(path: &Path) -> Option<String> {
    std::fs::read(path).ok().map(|bytes| sha1_hex(&bytes))
}

fn is_empty_dir(path: &Path) -> bool {
    std::fs::read_dir(path).is_ok_and(|mut entries| entries.next().is_none())
}

#[cfg(test)]
mod tests {
    use super::{History, StepError};
    use crate::Project;
    use crate::cache::Cache;
    use crate::lock::Lock;
    use std::path::{Path, PathBuf};

    fn project(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("pe2-history-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("layer/a")).unwrap();
        std::fs::write(root.join("project.toml"), "").unwrap();
        std::fs::write(root.join("layer/a/one"), "one").unwrap();
        root
    }

    fn history(root: &Path) -> (History, Option<String>) {
        let project = Project::load(root).unwrap();
        let lock = Lock::try_acquire(root, "a test").unwrap();
        History::open(&Cache::open(&project, &lock).unwrap()).unwrap()
    }

    fn read(path: &Path) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }

    #[test]
    fn undoes_and_redoes_writes_moves_and_deletes() {
        let root = project("steps");
        let (mut history, _) = history(&root);
        let (one, two) = (root.join("layer/a/one"), root.join("layer/b/c/two"));
        let mut tx = history.begin("edit");
        tx.write(&one, b"ONE").unwrap();
        tx.write(&two, b"two").unwrap();
        tx.commit().unwrap();
        let mut tx = history.begin("delete");
        tx.remove(&root.join("layer/a")).unwrap();
        tx.rename(&root.join("layer/b"), &root.join("layer/d")).unwrap();
        tx.commit().unwrap();
        assert!(!one.exists() && read(&root.join("layer/d/c/two")).as_deref() == Some("two"));
        history.undo(false).unwrap();
        assert_eq!((read(&one).as_deref(), read(&two).as_deref()), (Some("ONE"), Some("two")));
        history.undo(false).unwrap();
        // The folders the write made are gone with it: an undone copy leaves nothing behind.
        assert_eq!(read(&one).as_deref(), Some("one"));
        assert!(!root.join("layer/b").exists());
        history.redo(false).unwrap();
        history.redo(false).unwrap();
        assert!(!one.exists() && root.join("layer/d/c/two").exists());
        assert!(history.redo(false).is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn outside_changes_block_undo_unless_forced() {
        let root = project("outside");
        let (mut history, _) = history(&root);
        let one = root.join("layer/a/one");
        let mut tx = history.begin("edit");
        tx.write(&one, b"mine").unwrap();
        tx.commit().unwrap();
        std::fs::write(&one, "theirs").unwrap();
        match history.undo(false) {
            Err(StepError::Changed(paths)) => assert_eq!(paths, std::slice::from_ref(&one)),
            other => panic!("{other:?}"),
        }
        history.undo(true).unwrap();
        assert_eq!(read(&one).as_deref(), Some("one"));
        // Redo brings back what the forced undo overwrote.
        history.redo(false).unwrap();
        assert_eq!(read(&one).as_deref(), Some("theirs"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn rolls_back_what_did_not_finish() {
        let root = project("crash");
        let one = root.join("layer/a/one");
        {
            let (mut history, _) = history(&root);
            // Dropped uncommitted: rolled back at once.
            let mut tx = history.begin("failed");
            tx.write(&one, b"half").unwrap();
            drop(tx);
            assert_eq!(read(&one).as_deref(), Some("one"));
            // A crash: never dropped, never committed; the next opening rolls it back.
            let mut tx = history.begin("crashed");
            tx.write(&one, b"half").unwrap();
            tx.write(&root.join("layer/new/file"), b"x").unwrap();
            std::mem::forget(tx);
        }
        let (history, recovered) = history(&root);
        assert_eq!(recovered.as_deref(), Some("crashed"));
        assert_eq!(read(&one).as_deref(), Some("one"));
        assert!(!root.join("layer/new").exists());
        assert!(history.done().is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn merges_a_continued_edit() {
        let root = project("merge");
        let (mut history, _) = history(&root);
        let one = root.join("layer/a/one");
        for text in ["o", "on", "onx"] {
            let mut tx = history.begin("type").merging("one");
            tx.write(&one, text.as_bytes()).unwrap();
            tx.commit().unwrap();
        }
        assert_eq!(history.done().len(), 1);
        history.undo(false).unwrap();
        assert_eq!(read(&one).as_deref(), Some("one"));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
