//! The session's log: what was done to a project while it was open, each a step undo takes back and redo makes again.
//!
//! A step is of two kinds of change, which one log keeps in the order they were made:
//!
//! - **Edits of documents.** An asset being edited is a document: its working copy - the files edited, as the edits
//!   leave them - shared by everything showing it, unsaved until it is saved (`Document`). An edit changes the
//!   working copy only; the step records each file it changed, by hash before and after. The index shows an asset with
//!   unsaved changes as its working copy has it (`working`, `index::Working`), so that what is made of it follows.
//!   Saving writes the working copy's files (`edit::save`), a step of its own that undo passes over: undoing an edit
//!   saved since makes the document unsaved again, as in any editor.
//! - **Changes to files**, which every other modification is (a copy-on-write override, a changeset changed,
//!   `project.toml` rewritten, a move, a delete): a transaction records what it does - a file written (its content
//!   before and after, by hash; absent is a state too, so creating, overwriting and deleting a file are all writes), a
//!   file or folder renamed, a folder made for a write - and touches no file until it is committed: what it reads
//!   of what it changed so far it reads from its own changes. Committed, the whole of it is said in the log the
//!   project's processes share and had on disk, and only then are the files written (`storage`): a process that
//!   dies between leaves them to be written again from the log, never half a transaction. Undo restores the
//!   files as they were, the same way, so no operation brings undo code of its own.
//!
//! Undo takes back the last step, or the last that edited one document (`undo` with a scope): a step that edited it
//! and others gives back that document's part. A step checks that what it changed is as it left it: a file changed
//! since outside the editor, or a working copy taken from disk since, blocks it unless forced - and a forced step
//! records what it overwrote, so the opposite brings that back. A new step drops what was undone of what it touches.
//!
//! What a change overwrites or deletes is kept in the cache: a content by its hash (`.cache/history/blobs/<sha1>`,
//! once), a folder deleted moved whole into `.cache/history/trash/`. A working copy's files are kept there too.
//!
//! The log lasts the session. Where it is journaled (`journaled`, as the editor's is), it is written as it changes
//! (`.cache/history/session.jsonl`): a step made, or joined to the one before it, is appended with the documents it
//! edited - the session written whole only where it changed otherwise (an undo, a save), and when what was appended
//! has grown long. A clean close (`close`) wipes it all; one a crash left behind is found on the
//! next opening (`left`) and taken up again (`resume`) or let go (`discard_left`).
//!
//! Only the holder of the project lock writes (`lock.rs`).

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::cache::{Cache, write_atomic};
use crate::extract::sha1_hex;
use crate::index::{Index, Working, WorkingFile};

const DIR: &str = "history";
const SESSION: &str = "session.jsonl";
/// How long the session's file may grow by what is appended before it is written whole again.
const SESSION_LONG: u64 = 4 << 20;
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

/// A change to a document's working copy: one of its files, by hash before and after; none: absent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edit {
    pub uid: String,
    /// Where the asset was when it was edited.
    pub path: String,
    pub file: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

impl Edit {
    fn inverse(&self) -> Edit {
        Edit { before: self.after.clone(), after: self.before.clone(), ..self.clone() }
    }
}

/// What a step is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    /// A modification: undone and redone.
    #[default]
    Edit,
    /// Documents saved: their files written, which undo passes over.
    Save,
    /// A point of the log named, to come back to: it changes nothing.
    Checkpoint,
}

/// A step of the log: what it was, who made it and when, what it changed, and what was selected when it was made.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Step {
    pub label: String,
    pub author: String,
    #[serde(default)]
    pub kind: Kind,
    /// When it was made, in milliseconds since the epoch.
    #[serde(default)]
    pub at: u64,
    pub changes: Vec<Change>,
    #[serde(default)]
    pub edits: Vec<Edit>,
    pub selection: Option<String>,
    #[serde(skip)]
    merge: Option<(String, Instant)>,
}

impl Step {
    fn new(label: String, author: String) -> Step {
        let at = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(0));
        Step { label, author, kind: Kind::Edit, at, changes: vec![], edits: vec![], selection: None, merge: None }
    }

    /// Every path it touches.
    #[must_use]
    pub fn paths(&self) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = self.changes.iter().flat_map(Change::paths).map(Path::to_path_buf).collect();
        paths.sort();
        paths.dedup();
        paths
    }

    /// The documents it edited, by uid.
    #[must_use]
    pub fn documents(&self) -> BTreeSet<&str> {
        self.edits.iter().map(|e| e.uid.as_str()).collect()
    }

    /// Whether it edited this document.
    #[must_use]
    pub fn edits_document(&self, uid: &str) -> bool {
        self.edits.iter().any(|e| e.uid == uid)
    }

    /// Whether undo and redo take it: a modification, not a save.
    #[must_use]
    pub fn undoable(&self) -> bool {
        self.kind == Kind::Edit
    }

    /// Whether a new step drops it, undone: it touches what the new one does - a document both edit, or files.
    fn overtaken_by(&self, new: &Step) -> bool {
        let theirs = new.documents();
        self.edits.iter().any(|e| theirs.contains(e.uid.as_str()))
            || (!self.changes.is_empty() && !new.changes.is_empty())
    }
}

/// An asset being edited: its working copy, which is unsaved - the files edited, each by hash (none: absent) as on
/// disk when it was first edited and as the edits leave it - and, where its files changed on disk meanwhile, what they
/// are there now: it is stale, and the user keeps theirs or takes the disk's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Document {
    /// Where the asset is.
    pub path: String,
    pub base: BTreeMap<String, Option<String>>,
    pub working: BTreeMap<String, Option<String>>,
    pub stale: Option<BTreeMap<String, Option<String>>>,
}

/// What a session a crash ended left: its steps, and its documents with unsaved changes.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Session {
    pub done: Vec<Step>,
    pub undone: Vec<Step>,
    pub documents: BTreeMap<String, Document>,
}

/// Why a step cannot be undone or redone.
#[derive(Debug)]
pub enum StepError {
    /// Files it touches changed since outside the editor, or documents it edited were taken from disk since: forcing
    /// it overwrites them.
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

/// A file of an asset as it is on disk, by hash: none where it has no such file, or the index no such asset.
fn on_disk(index: &Index, uid: &str, file: &str) -> Option<String> {
    disk_file(index, uid, file).map(|f| f.sha1.clone())
}

fn disk_file<'i>(index: &'i Index, uid: &str, file: &str) -> Option<&'i crate::index::BundleFile> {
    let asset = index.by_uid.get(uid).and_then(|path| index.assets.get(path))?.on_disk();
    asset.files.iter().find(|f| f.name == file && f.made_by.is_none())
}

/// What a history shows of itself - its steps done and undone, the documents with unsaved changes, a session a
/// crash left, and the contents it keeps by their hash: the history's own, or a copy as of a moment
/// (`History::view`) for whoever shows it while another thread has the history.
#[derive(Clone, Debug, Default)]
pub struct View {
    dir: PathBuf,
    done: Vec<Step>,
    undone: Vec<Step>,
    documents: BTreeMap<String, Document>,
    /// What a session a crash ended left, until it is taken up or let go.
    left: Option<Session>,
}

pub struct History {
    /// Its steps, its documents and what it keeps: read through it as of the history itself (`Deref`).
    view: View,
    /// How many times what it shows changed: counted where the session is written, which every change of its
    /// steps and documents does.
    revision: std::cell::Cell<u64>,
    /// Whether the session is written as it changes.
    journaled: bool,
    /// What the session's file holds, where it was written whole and appended to since: how many steps done and
    /// undone, and how long it is. None where the next writing is of the whole.
    appended: std::cell::Cell<Option<(usize, usize, u64)>>,
    /// Who makes the steps begun now.
    author: String,
    trashed: usize,
    /// The paths its steps wrote - made, undone, redone - since they were last taken (`take_written`): what the
    /// editor takes in as its own changes.
    written: Vec<PathBuf>,
    /// The log its changes to files are said in before they are made; none where it could not be opened.
    storage: Option<crate::storage::Storage>,
}

impl View {
    /// The session a crash ended, where one was left and is not taken up or let go yet.
    #[must_use]
    pub fn left(&self) -> Option<&Session> {
        self.left.as_ref()
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

    /// The step undo takes back: the last, or the last editing this document.
    #[must_use]
    pub fn to_undo(&self, scope: Option<&str>) -> Option<&Step> {
        self.done.iter().rev().find(|s| s.undoable() && scope.is_none_or(|uid| s.edits_document(uid)))
    }

    /// The step redo makes again: the last undone, or the last undone editing this document.
    #[must_use]
    pub fn to_redo(&self, scope: Option<&str>) -> Option<&Step> {
        self.undone.iter().rev().find(|s| scope.is_none_or(|uid| s.edits_document(uid)))
    }

    /// The documents with unsaved changes, by uid.
    #[must_use]
    pub fn documents(&self) -> &BTreeMap<String, Document> {
        &self.documents
    }

    /// An asset's document, where it has unsaved changes.
    #[must_use]
    pub fn document(&self, uid: &str) -> Option<&Document> {
        self.documents.get(uid)
    }

    /// The working copies, as the index shows them (`Index::set_working`).
    #[must_use]
    pub fn working(&self) -> HashMap<String, Working> {
        self.documents
            .iter()
            .map(|(uid, document)| {
                let files = document.working.iter().map(|(name, sha1)| {
                    let file = sha1.as_ref().map(|sha1| WorkingFile { path: self.blob_path(sha1), sha1: sha1.clone() });
                    (name.clone(), file)
                });
                (uid.clone(), Working { files: files.collect() })
            })
            .collect()
    }

    /// A content kept, by its hash: a working copy's file, what a step overwrote.
    ///
    /// # Errors
    /// It is not kept.
    pub fn content(&self, sha1: &str) -> Result<Vec<u8>> {
        self.blob(sha1)
    }

    /// The documents' files as they were at a point of the log - after its first `point` steps done - where a step
    /// since edited them: each by uid and name, by hash (none: absent). Of one document, or all. What steps did to
    /// files otherwise (a move, a new asset) is not taken back: a point is of the documents.
    #[must_use]
    pub fn states_at(&self, point: usize, scope: Option<&str>) -> BTreeMap<(String, String), Option<String>> {
        let mut states = BTreeMap::new();
        for step in self.done[point.min(self.done.len())..].iter().rev() {
            for edit in step.edits.iter().rev().filter(|e| scope.is_none_or(|uid| e.uid == uid)) {
                states.insert((edit.uid.clone(), edit.file.clone()), edit.before.clone());
            }
        }
        states
    }

    /// The working copies as they were at a point of the log, for the index to show (`Index::set_working`): those of
    /// now, with what steps since changed as it was.
    #[must_use]
    pub fn working_at(&self, point: usize, scope: Option<&str>) -> HashMap<String, Working> {
        let mut working = self.working();
        for ((uid, file), sha1) in self.states_at(point, scope) {
            let file_at = sha1.map(|sha1| WorkingFile { path: self.blob_path(&sha1), sha1 });
            working.entry(uid).or_default().files.insert(file, file_at);
        }
        working
    }

    fn blob_path(&self, hash: &str) -> PathBuf {
        self.dir.join("blobs").join(hash)
    }

    fn blob(&self, hash: &str) -> Result<Vec<u8>> {
        let path = self.blob_path(hash);
        std::fs::read(&path).with_context(|| format!("{}: a content the history kept is gone", path.display()))
    }
}

impl std::ops::Deref for History {
    type Target = View;

    fn deref(&self) -> &View {
        &self.view
    }
}

impl History {
    /// What it shows of itself, as it is now: for whoever shows it while another thread has the history.
    #[must_use]
    pub fn view(&self) -> View {
        self.view.clone()
    }

    /// The project's log for this session: a session a crash ended is kept aside (`left`), and otherwise what an
    /// earlier session kept is wiped. Returns it and what was written again from the log as the cache was opened,
    /// if anything was - changes whoever held the project before had not had in its files.
    ///
    /// # Errors
    /// The history's folder cannot be read, cleared or made.
    pub fn open(cache: &Cache) -> Result<(History, Option<String>)> {
        let dir = cache.path(DIR);
        let mut history = History {
            view: View { dir: dir.clone(), ..View::default() },
            revision: std::cell::Cell::new(0),
            journaled: false,
            appended: std::cell::Cell::new(None),
            author: "user".into(),
            trashed: 0,
            written: vec![],
            storage: cache.dir.parent().and_then(crate::storage::Storage::open),
        };
        let recovered = cache.recovered.clone();
        let left = std::fs::read_to_string(dir.join(SESSION))
            .ok()
            .and_then(|text| Session::read(&text))
            .filter(|left| !left.done.is_empty() || !left.undone.is_empty() || !left.documents.is_empty());
        if left.is_some() {
            // What it kept stays, for what it left to be taken up; the trash goes on numbering after it.
            history.trashed = std::fs::read_dir(dir.join("trash"))
                .map(|entries| entries.filter_map(|e| e.ok()?.file_name().to_str()?.parse::<usize>().ok()).max())
                .ok()
                .flatten()
                .unwrap_or(0);
        } else if dir.exists() {
            std::fs::remove_dir_all(&dir).with_context(|| format!("{}", dir.display()))?;
        }
        std::fs::create_dir_all(dir.join("blobs")).with_context(|| format!("{}", dir.display()))?;
        history.view.left = left;
        Ok((history, recovered))
    }

    /// Written as it changes, so that a crash leaves the session to be taken up again (the editor's).
    #[must_use]
    pub fn journaled(mut self) -> Self {
        self.journaled = true;
        self
    }

    /// Take up the session a crash ended: its steps before this one's, its documents where this one has none.
    pub fn resume(&mut self) {
        let Some(left) = self.view.left.take() else { return };
        let done = std::mem::replace(&mut self.view.done, left.done);
        self.view.done.extend(done);
        if self.view.undone.is_empty() {
            self.view.undone = left.undone;
        }
        for (uid, document) in left.documents {
            self.view.documents.entry(uid).or_insert(document);
        }
        self.journal();
    }

    /// Let the session a crash ended go.
    pub fn discard_left(&mut self) {
        self.view.left = None;
        self.journal();
    }

    /// The session closed cleanly: what it kept is wiped.
    pub fn close(self) {
        self.wipe();
    }

    /// What the session kept, wiped: it is closing cleanly, and goes with this.
    pub fn wipe(&self) {
        let _ = std::fs::remove_dir_all(&self.view.dir);
    }

    /// Which of its revisions it is at: another, once what it shows (`view`) changed.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision.get()
    }

    /// Let go of the log where it was opened for a moment's work (a host, a command's edit), not taken up: closed, unless
    /// it found a session another left - an editor's, crashed or closed with documents unsaved - which stays, for the
    /// editor to offer back.
    pub fn let_go(self) {
        if self.view.left.is_none() {
            self.close();
        }
    }

    /// Who makes the steps begun from now on (`user`, `agent`).
    pub fn set_author(&mut self, author: &str) {
        if self.author != author {
            author.clone_into(&mut self.author);
        }
    }

    /// Begin a transaction, named as the user will see it in the history.
    pub fn begin(&mut self, label: impl Into<String>) -> Transaction<'_> {
        let step = Step::new(label.into(), self.author.clone());
        Transaction { history: self, step, committed: false }
    }

    /// Note what was selected when the last step was made: undoing it shows that again.
    pub fn note_selection(&mut self, selection: Option<String>) {
        if let Some(step) = self.view.done.last_mut() {
            step.selection = selection;
        }
        self.journal_last(&[]);
    }

    /// The paths its steps wrote since they were last taken, made, undone or redone.
    #[must_use]
    pub fn written(&self) -> &[PathBuf] {
        &self.written
    }

    /// The files steps wrote since this was last asked, for the index to take in.
    pub fn take_written(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.written)
    }

    // --- documents -------------------------------------------------------------------------------------------------

    /// Edit a file of an asset's working copy - made of its files on disk where it has none - as a step of its own,
    /// joining the last where `merge` continues it. Whether it changed anything.
    ///
    /// # Errors
    /// The content cannot be kept.
    #[expect(clippy::too_many_arguments, reason = "an edit's context")]
    pub fn edit(
        &mut self,
        index: &Index,
        uid: &str,
        path: &str,
        file: &str,
        bytes: &[u8],
        label: String,
        merge: Option<String>,
    ) -> Result<bool> {
        let after = Some(self.keep(bytes)?);
        let before = self.current(index, uid, file);
        if before == after {
            return Ok(false);
        }
        let edit = Edit { uid: uid.to_string(), path: path.to_string(), file: file.to_string(), before, after };
        self.set_working(index, &edit);
        let mut step = Step::new(label, self.author.clone());
        step.edits.push(edit);
        step.merge = merge.map(|key| (key, Instant::now()));
        self.push(step);
        Ok(true)
    }

    /// A file of an asset as its working copy has it, else as on disk.
    fn current(&self, index: &Index, uid: &str, file: &str) -> Option<String> {
        match self.view.documents.get(uid).and_then(|d| d.working.get(file)) {
            Some(working) => working.clone(),
            None => on_disk(index, uid, file),
        }
    }

    /// Put an edit's after in its document's working copy, made where there is none; a working copy back as on disk
    /// is no longer unsaved.
    fn set_working(&mut self, index: &Index, edit: &Edit) {
        let path = index.by_uid.get(&edit.uid).cloned().unwrap_or_else(|| edit.path.clone());
        let document = self.view.documents.entry(edit.uid.clone()).or_insert_with(|| Document {
            path,
            base: BTreeMap::new(),
            working: BTreeMap::new(),
            stale: None,
        });
        if !document.base.contains_key(&edit.file) {
            document.base.insert(edit.file.clone(), on_disk(index, &edit.uid, &edit.file));
            // What it was on disk is kept: undone back to it, it is shown though the disk has changed since.
            if let Some(file) = disk_file(index, &edit.uid, &edit.file)
                && let Ok(bytes) = crate::content::read(&file.path)
            {
                let _ = keep_in(&self.view.dir, &bytes);
            }
        }
        document.working.insert(edit.file.clone(), edit.after.clone());
        if document.working == document.base && document.stale.is_none() {
            self.view.documents.remove(&edit.uid);
        }
    }

    /// The files a document's save writes, each with its content (none: absent), where it has unsaved changes.
    ///
    /// # Errors
    /// A content is not kept.
    pub fn to_save(&self, uid: &str) -> Result<Vec<(String, Option<Vec<u8>>)>> {
        let Some(document) = self.view.documents.get(uid) else { return Ok(vec![]) };
        let disk = document.stale.as_ref().unwrap_or(&document.base);
        let changed = document.working.iter().filter(|(name, sha1)| disk.get(*name) != Some(*sha1));
        changed.map(|(name, sha1)| Ok((name.clone(), sha1.as_deref().map(|s| self.blob(s)).transpose()?))).collect()
    }

    /// A document was saved: its working copy is what is on disk.
    pub fn saved(&mut self, uid: &str) {
        self.view.documents.remove(uid);
        self.journal();
    }

    /// Put a document back as on disk, as a step: its unsaved changes undone, or - stale - the disk's taken.
    pub fn revert(&mut self, uid: &str, label: String) {
        let Some(document) = self.view.documents.get(uid).cloned() else { return };
        let disk = document.stale.as_ref().unwrap_or(&document.base);
        let mut step = Step::new(label, self.author.clone());
        for (file, working) in &document.working {
            let after = disk.get(file).cloned().flatten();
            if *working != after {
                let edit = Edit {
                    uid: uid.to_string(),
                    path: document.path.clone(),
                    file: file.clone(),
                    before: working.clone(),
                    after,
                };
                step.edits.push(edit);
            }
        }
        self.view.documents.remove(uid);
        if step.edits.is_empty() {
            self.journal();
        } else {
            self.push(step);
        }
    }

    /// Keep a stale document's working copy: what is on disk now is what saving overwrites.
    pub fn keep_mine(&mut self, uid: &str) {
        if let Some(document) = self.view.documents.get_mut(uid)
            && let Some(disk) = document.stale.take()
        {
            document.base = disk;
            self.journal();
        }
    }

    /// Follow the disk under the documents: where a document's files on disk are no longer those its edits started
    /// from, it is stale - or saved, where they are its working copy. A document follows its asset as it moves.
    /// Whether any changed.
    pub fn reconcile(&mut self, index: &Index) -> bool {
        let mut changed = false;
        let mut clean = vec![];
        for (uid, document) in &mut self.view.documents {
            if let Some(path) = index.by_uid.get(uid)
                && *path != document.path
            {
                document.path.clone_from(path);
                changed = true;
            }
            let now: BTreeMap<String, Option<String>> =
                document.base.keys().map(|file| (file.clone(), on_disk(index, uid, file))).collect();
            let stale = if now == document.base { None } else { Some(now.clone()) };
            // What the disk has now is kept: taken, it is the working copy.
            if stale.is_some() && stale != document.stale {
                for file in now.keys() {
                    if let Some(file) = disk_file(index, uid, file)
                        && let Ok(bytes) = crate::content::read(&file.path)
                    {
                        let _ = keep_in(&self.view.dir, &bytes);
                    }
                }
            }
            if now != document.base && now == document.working {
                clean.push(uid.clone());
            } else if stale != document.stale {
                document.stale = stale;
                changed = true;
            }
        }
        for uid in clean {
            self.view.documents.remove(&uid);
            changed = true;
        }
        if changed {
            self.journal();
        }
        changed
    }

    /// Let every unsaved change go: documents back as on disk, not as a step (the session is closing).
    pub fn discard_documents(&mut self) {
        self.view.documents.clear();
        self.journal();
    }

    // --- points of the log -----------------------------------------------------------------------------------------

    /// Put the documents back as they were at a point of the log - one, or all - as a new step: what was done since
    /// stays in the log, undone by undoing this. Whether anything changed.
    pub fn restore(&mut self, index: &Index, point: usize, scope: Option<&str>, label: String) -> bool {
        let mut step = Step::new(label, self.author.clone());
        for ((uid, file), after) in self.states_at(point, scope) {
            let before = self.current(index, &uid, &file);
            if before != after {
                let path = self.edit_path(index, &uid);
                step.edits.push(Edit { uid, path, file, before, after });
            }
        }
        self.apply_new(index, step)
    }

    /// Take back one step's edits as a new step, where nothing since edited what it did. Whether anything changed.
    ///
    /// # Errors
    /// Why it cannot be: it is no edit, it changed files (undo takes those back), or what it edited was edited since.
    pub fn revert_step(&mut self, index: &Index, at: usize, label: String) -> Result<bool, String> {
        let step = self.view.done.get(at).ok_or("no such step")?;
        if !step.undoable() {
            return Err("It changed nothing to take back".into());
        }
        if !step.changes.is_empty() {
            return Err("It changed the project's files: undoing takes it back".into());
        }
        let later = &self.view.done[at + 1..];
        for edit in &step.edits {
            let since = later.iter().any(|s| s.edits.iter().any(|e| e.uid == edit.uid && e.file == edit.file));
            if since || self.current(index, &edit.uid, &edit.file) != edit.after {
                let name = edit.path.rsplit('/').next().unwrap_or(&edit.path);
                return Err(format!("{name} was edited since: restore an earlier point instead"));
            }
        }
        let mut reverting = Step::new(label, self.author.clone());
        reverting.edits = step.edits.iter().rev().map(Edit::inverse).collect();
        Ok(self.apply_new(index, reverting))
    }

    /// Name the point the log is at, to come back to.
    pub fn checkpoint(&mut self, name: String) {
        let mut step = Step::new(name, self.author.clone());
        step.kind = Kind::Checkpoint;
        self.push(step);
    }

    /// Make a new step of edits: the working copies take them, and the log keeps it. Whether it changed anything.
    fn apply_new(&mut self, index: &Index, step: Step) -> bool {
        if step.edits.is_empty() {
            return false;
        }
        for edit in &step.edits {
            self.set_working(index, edit);
        }
        self.push(step);
        true
    }

    /// Where an asset is, for an edit of it: as the index has it, else as its document last had it.
    fn edit_path(&self, index: &Index, uid: &str) -> String {
        index
            .by_uid
            .get(uid)
            .or_else(|| self.view.documents.get(uid).map(|d| &d.path))
            .cloned()
            .or_else(|| self.view.done.iter().flat_map(|s| &s.edits).find(|e| e.uid == uid).map(|e| e.path.clone()))
            .unwrap_or_default()
    }

    // --- undo and redo ---------------------------------------------------------------------------------------------

    /// Undo the last step, or the last editing one document (of a step editing others too, that document's part).
    /// Returns it.
    ///
    /// # Errors
    /// There is nothing to undo, what it changed was changed since (unless forced), or files cannot be written.
    pub fn undo(&mut self, index: &Index, scope: Option<&str>, force: bool) -> Result<&Step, StepError> {
        let at = self.view.done.iter().rposition(|s| s.undoable() && scope.is_none_or(|uid| s.edits_document(uid)));
        let Some(at) = at else {
            return Err(StepError::Io(anyhow::anyhow!("nothing to undo")));
        };
        let mut step = self.take_part(at, scope, true);
        match self.run(index, &mut step, false, force) {
            Ok(()) => {
                self.written.extend(step.paths());
                self.view.undone.push(step);
                self.journal();
                self.view.undone.last().ok_or_else(|| StepError::Io(anyhow::anyhow!("the step undone is gone")))
            }
            Err(e) => {
                self.give_back(at, step, true);
                Err(e)
            }
        }
    }

    /// Redo the step undone last, or the last undone editing one document. Returns it.
    ///
    /// # Errors
    /// There is nothing to redo, what it changed was changed since (unless forced), or files cannot be written.
    pub fn redo(&mut self, index: &Index, scope: Option<&str>, force: bool) -> Result<&Step, StepError> {
        let at = self.view.undone.iter().rposition(|s| scope.is_none_or(|uid| s.edits_document(uid)));
        let Some(at) = at else {
            return Err(StepError::Io(anyhow::anyhow!("nothing to redo")));
        };
        let mut step = self.take_part(at, scope, false);
        match self.run(index, &mut step, true, force) {
            Ok(()) => {
                self.written.extend(step.paths());
                self.view.done.push(step);
                self.journal();
                self.view.done.last().ok_or_else(|| StepError::Io(anyhow::anyhow!("the step redone is gone")))
            }
            Err(e) => {
                self.give_back(at, step, false);
                Err(e)
            }
        }
    }

    /// Take a step out of the done (or undone) steps, whole - or, scoped to a document where it changed more, its part
    /// of it, the rest staying where it was.
    fn take_part(&mut self, at: usize, scope: Option<&str>, done: bool) -> Step {
        let steps = if done { &mut self.view.done } else { &mut self.view.undone };
        let whole =
            scope.is_none_or(|uid| steps[at].changes.is_empty() && steps[at].edits.iter().all(|e| e.uid == uid));
        if whole {
            return steps.remove(at);
        }
        let uid = scope.unwrap_or_default();
        let rest = &mut steps[at];
        let (mine, others): (Vec<Edit>, Vec<Edit>) = rest.edits.drain(..).partition(|e| e.uid == uid);
        rest.edits = others;
        let mut part = Step::new(rest.label.clone(), rest.author.clone());
        part.at = rest.at;
        part.selection.clone_from(&rest.selection);
        part.edits = mine;
        part
    }

    /// Put a step that could not be run back where it was taken from: whole, or a part joining what it was taken from.
    fn give_back(&mut self, at: usize, step: Step, done: bool) {
        let steps = if done { &mut self.view.done } else { &mut self.view.undone };
        match steps.get_mut(at) {
            Some(rest) if rest.at == step.at && rest.label == step.label && step.changes.is_empty() => {
                rest.edits.extend(step.edits);
            }
            _ => steps.insert(at.min(steps.len()), step),
        }
    }

    /// Run a step forwards (redo) or backwards (undo): check what it changed is as it left it, then apply.
    fn run(&mut self, index: &Index, step: &mut Step, forwards: bool, force: bool) -> Result<(), StepError> {
        let list: Vec<Change> =
            if forwards { step.changes.clone() } else { step.changes.iter().rev().map(Change::inverse).collect() };
        let edits: Vec<Edit> =
            if forwards { step.edits.clone() } else { step.edits.iter().rev().map(Edit::inverse).collect() };
        let mut differing = Self::changed(&list);
        differing.extend(self.edited(index, &edits));
        if !differing.is_empty() {
            if !force {
                return Err(StepError::Changed(differing));
            }
            // What is there now is what the step will have left: undoing it again brings it back.
            self.adopt(index, step, forwards).map_err(StepError::Io)?;
            return self.run(index, step, forwards, false);
        }
        self.apply_all(&step.label, &list).map_err(StepError::Io)?;
        for edit in &edits {
            self.set_working(index, edit);
        }
        Ok(())
    }

    /// The documents' files not as a list of edits expects them, followed in order, each named by its asset's path.
    fn edited(&self, index: &Index, edits: &[Edit]) -> Vec<PathBuf> {
        let mut state: HashMap<(&str, &str), Option<String>> = HashMap::new();
        let mut differing = vec![];
        for edit in edits {
            let key = (edit.uid.as_str(), edit.file.as_str());
            let now = state.get(&key).cloned().unwrap_or_else(|| self.current(index, &edit.uid, &edit.file));
            if now != edit.before {
                let path = index.by_uid.get(&edit.uid).unwrap_or(&edit.path);
                differing.push(Path::new(path).join(&edit.file));
            }
            state.insert(key, edit.after.clone());
        }
        differing
    }

    /// The files not as a list of changes expects them, followed in order: a file written twice is expected as the
    /// first write left it. Folders are not checked: one made is there or not, and one to remove that holds something
    /// now is left alone.
    fn changed(list: &[Change]) -> Vec<PathBuf> {
        let mut state: HashMap<&Path, Option<String>> = HashMap::new();
        // Whether what an earlier move took away or put there is there, as the list leaves it.
        let mut moved: HashMap<&Path, bool> = HashMap::new();
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
                    let there = |path: &Path| moved.get(path).copied().unwrap_or_else(|| path.exists());
                    if untouched && (!there(from) || there(to)) {
                        differing.push(from.clone());
                    }
                    moved.insert(from, false);
                    moved.insert(to, true);
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

    /// Take the files and working copies as they are now as what the step leaves (before undoing it) or starts from
    /// (before redoing).
    fn adopt(&mut self, index: &Index, step: &mut Step, forwards: bool) -> Result<()> {
        for change in &mut step.changes {
            if let Change::Write { path, before, after } = change {
                let now = self.keep_current(path)?;
                if forwards { *before = now } else { *after = now }
            }
        }
        for edit in &mut step.edits {
            let now = self.current(index, &edit.uid, &edit.file);
            if forwards { edit.before = now } else { edit.after = now }
        }
        Ok(())
    }

    /// Make changes to files: said whole in the log and had on disk first (`storage::Storage::say`), then made - so
    /// that a process that dies midway leaves them to be made again from the log by whoever takes the project next,
    /// and one that dies before the log has them all has touched nothing.
    fn apply_all(&mut self, label: &str, changes: &[Change]) -> Result<()> {
        if changes.is_empty() {
            return Ok(());
        }
        let began = match &self.storage {
            Some(storage) => {
                let said: Vec<crate::storage::Stored> =
                    changes.iter().map(|change| self.said(storage, change)).collect::<Result<_>>()?;
                // Records of records layers alone: said, kept as waiting for whoever reads them, and written
                // behind - nothing here waits for a disk.
                let behind = self.behind(changes).filter(|files| storage.defers(files));
                let storage = self.storage.as_mut().context("the log is gone")?;
                if let Some(files) = behind {
                    storage.say_later(label, &said, files).context("saying the changes in the log")?;
                    return Ok(());
                }
                Some(storage.say(label, &said).context("saying the changes in the log")?)
            }
            None => None,
        };
        for (made, change) in changes.iter().enumerate() {
            if let Err(e) = self.apply(change) {
                // A file could not be written: what was is put back, and the log told not to make it again.
                for done in changes[..made].iter().rev() {
                    let _ = self.apply(&done.inverse());
                }
                if let (Some(storage), Some(began)) = (&self.storage, began) {
                    storage.gave_up(began);
                }
                return Err(e);
            }
        }
        if let Some(storage) = &mut self.storage {
            storage.wrote(changes.iter().flat_map(Change::paths).map(Path::to_path_buf));
        }
        Ok(())
    }

    /// The files of a set as they are written behind; none where it does more than write and remove files and make
    /// folders.
    fn behind(&self, changes: &[Change]) -> Option<Vec<crate::storage::Behind>> {
        use crate::storage::Behind;
        let at = crate::storage::stamped(std::time::SystemTime::now());
        changes
            .iter()
            .map(|change| match change {
                Change::Write { path, before, after } => {
                    let after = after.as_ref().map(|hash| self.blob(hash)).transpose().ok()?;
                    Some(Behind { path: path.clone(), after, before: before.clone(), folder: false, at })
                }
                Change::MakeDir { path } => Some(Behind { path: path.clone(), after: None, before: None, folder: true, at }),
                Change::Rename { .. } | Change::RemoveDir { .. } => None,
            })
            .collect()
    }

    /// Have what writing behind needs made ahead of the first edit (`Storage::prepare`): for whoever holds the
    /// project to stay.
    pub fn prepare(&self) {
        if let Some(storage) = &self.storage {
            storage.prepare();
        }
    }

    /// Hold back what is written behind, or write it again (`Storage::hold_back`).
    pub fn hold_back(&self, held: bool) {
        if let Some(storage) = &self.storage {
            storage.hold_back(held);
        }
    }

    /// Wait for what was said to be written behind to be in the project's files: for who answers something that
    /// is to find them so - a command, an agent, whatever reads the files by other means next.
    pub fn settle(&self) {
        if let Some(storage) = &self.storage {
            storage.settle();
        }
    }

    /// A change as the log says it: a file with the bytes it comes to hold.
    fn said(&self, storage: &crate::storage::Storage, change: &Change) -> Result<crate::storage::Stored> {
        use crate::storage::Stored;
        Ok(match change {
            Change::Write { path, after: Some(hash), .. } => {
                Stored::Put { path: storage.name(path), bytes: self.blob(hash)? }
            }
            Change::Write { path, after: None, .. } => Stored::Delete { path: storage.name(path) },
            Change::Rename { from, to } => Stored::Move { from: storage.name(from), to: storage.name(to) },
            Change::MakeDir { path } => Stored::MakeDir { path: storage.name(path) },
            Change::RemoveDir { path } => Stored::RemoveDir { path: storage.name(path) },
        })
    }

    /// The session written whole, where it is journaled: what a crash would leave.
    fn journal(&self) {
        self.revision.set(self.revision.get() + 1);
        if !self.journaled {
            return;
        }
        let session = SessionRef { done: &self.view.done, undone: &self.view.undone, documents: &self.view.documents };
        self.appended.set(None);
        if let Ok(mut text) = serde_json::to_string(&Journaled::Whole(session)) {
            text.push('\n');
            if write_atomic(&self.view.dir.join(SESSION), text.as_bytes()).is_ok() {
                self.appended.set(Some((self.view.done.len(), self.view.undone.len(), text.len() as u64)));
            }
        }
    }

    /// The session written after its last step was made, or changed - joined by another, given a selection - with
    /// these documents edited: appended to what its file holds, as the steps from the last it held on, the steps
    /// undone where their number changed, and those documents. Written whole where its file does not hold it yet, or
    /// has grown long.
    fn journal_last(&self, documents: &[String]) {
        if !self.journaled {
            self.revision.set(self.revision.get() + 1);
            return;
        }
        let Some((done, undone, long)) = self.appended.get().filter(|(done, _, long)| *done <= self.view.done.len() && *long < SESSION_LONG) else {
            return self.journal();
        };
        self.revision.set(self.revision.get() + 1);
        // The last step it held may be the one joined: from that one on.
        let from = done.saturating_sub(1);
        let since = Journaled::Since {
            from,
            done: &self.view.done[from..],
            undone: (undone != self.view.undone.len()).then_some(self.view.undone.as_slice()),
            documents: documents.iter().map(|uid| (uid.as_str(), self.view.documents.get(uid))).collect(),
        };
        let appended = serde_json::to_string(&since).ok().and_then(|mut text| {
            use std::io::Write as _;
            text.push('\n');
            let mut file = std::fs::OpenOptions::new().append(true).open(self.view.dir.join(SESSION)).ok()?;
            file.write_all(text.as_bytes()).ok()?;
            Some(text.len() as u64)
        });
        match appended {
            Some(more) => self.appended.set(Some((self.view.done.len(), self.view.undone.len(), long + more))),
            None => self.journal(),
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

    /// Keep bytes in the history; their hash.
    fn keep(&self, bytes: &[u8]) -> Result<String> {
        keep_in(&self.view.dir, bytes)
    }

    /// Keep a file's current content; its hash, none where it is absent.
    fn keep_current(&self, path: &Path) -> Result<Option<String>> {
        match crate::storage::read(path) {
            Ok(Some(bytes)) => self.keep(&bytes).map(Some),
            Ok(None) => Ok(None),
            Err(e) => Err(e).with_context(|| path.display().to_string()),
        }
    }

    fn push(&mut self, mut step: Step) {
        if step.undoable() {
            self.view.undone.retain(|undone| !undone.overtaken_by(&step));
        }
        // The documents it edited: written with it.
        let edited: Vec<String> = step.edits.iter().map(|edit| edit.uid.clone()).collect();
        if let (Some(last), Some((key, _))) = (self.view.done.last_mut(), &step.merge)
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
            for edit in step.edits.drain(..) {
                match last.edits.iter_mut().rev().find(|e| e.uid == edit.uid && e.file == edit.file) {
                    Some(last_edit) => last_edit.after = edit.after,
                    None => last.edits.push(edit),
                }
            }
            last.merge = step.merge;
            self.journal_last(&edited);
            return;
        }
        self.view.done.push(step);
        if self.view.done.len() > STEPS {
            // The oldest goes: every step is at another place, and the session is written whole.
            self.view.done.remove(0);
            return self.journal();
        }
        self.journal_last(&edited);
    }
}

/// The session as it is written.
#[derive(Serialize)]
struct SessionRef<'a> {
    done: &'a [Step],
    undone: &'a [Step],
    documents: &'a BTreeMap<String, Document>,
}

/// A line of the session's file: the session whole, or what changed of it since the line before - its steps done
/// from one on, its steps undone where they changed, and documents (none: no longer one).
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Journaled<'a> {
    Whole(SessionRef<'a>),
    Since {
        from: usize,
        done: &'a [Step],
        undone: Option<&'a [Step]>,
        documents: BTreeMap<&'a str, Option<&'a Document>>,
    },
}

/// A line of the session's file, read.
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum JournaledLine {
    Whole(Session),
    Since { from: usize, done: Vec<Step>, undone: Option<Vec<Step>>, documents: BTreeMap<String, Option<Document>> },
}

impl Session {
    /// The session a file of lines holds (`Journaled`): the last written whole, with what was appended since - up
    /// to a line that is not whole, which a process ended writing. None where it holds no session.
    fn read(text: &str) -> Option<Session> {
        let mut session = None;
        for line in text.lines() {
            match serde_json::from_str::<JournaledLine>(line) {
                Ok(JournaledLine::Whole(whole)) => session = Some(whole),
                Ok(JournaledLine::Since { from, done, undone, documents }) => {
                    let session: &mut Session = session.as_mut()?;
                    session.done.truncate(from);
                    session.done.extend(done);
                    if let Some(undone) = undone {
                        session.undone = undone;
                    }
                    for (uid, document) in documents {
                        match document {
                            Some(document) => drop(session.documents.insert(uid, document)),
                            None => drop(session.documents.remove(&uid)),
                        }
                    }
                }
                Err(_) => break,
            }
        }
        session
    }
}

/// A modification being made: its changes are gathered, no file touched, and made together when it is committed
/// (`History::apply_all`), kept as one step. What it asks of the files meanwhile - whether one is there, what it
/// holds - is answered as its changes so far leave them. One dropped uncommitted - an error midway - changed nothing.
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

    /// What it is: a save is passed over by undo.
    #[must_use]
    pub fn kind(mut self, kind: Kind) -> Self {
        self.step.kind = kind;
        self
    }

    /// One continued edit (typing in a field, dragging a value): committed soon after another of the same key, it
    /// joins that step.
    #[must_use]
    pub fn merging(mut self, key: impl Into<String>) -> Self {
        self.step.merge = Some((key.into(), Instant::now()));
        self
    }

    fn change(&mut self, change: Change) {
        self.step.changes.push(change);
    }

    /// What a path is as the changes so far leave it: a file they wrote or removed, a folder they made or
    /// removed, something they moved away - or what the disk has, at the path it had before they moved it.
    fn seen(&self, path: &Path) -> Seen {
        let mut at = path.to_path_buf();
        for change in self.step.changes.iter().rev() {
            match change {
                Change::Write { path, after, .. } if *path == at => return Seen::File(after.clone()),
                Change::MakeDir { path } if *path == at => return Seen::Folder,
                Change::RemoveDir { path } if *path == at => return Seen::Gone,
                Change::Rename { from, to } => {
                    if let Ok(inside) = at.strip_prefix(to) {
                        at = if inside.as_os_str().is_empty() { from.clone() } else { from.join(inside) };
                    } else if at.starts_with(from) {
                        return Seen::Gone;
                    }
                }
                _ => {}
            }
        }
        Seen::Disk(at)
    }

    /// Whether the changes so far say a file or a folder is at a path, or that nothing is; none where they say
    /// nothing of it - the disk does, at `on_disk`.
    #[must_use]
    pub fn pending(&self, path: &Path) -> Option<bool> {
        match self.seen(path) {
            Seen::File(after) => Some(after.is_some()),
            Seen::Folder => Some(true),
            Seen::Gone => Some(false),
            Seen::Disk(_) => None,
        }
    }

    /// Where on disk what is at a path is now, the changes so far having moved it there from elsewhere or not.
    #[must_use]
    pub fn on_disk(&self, path: &Path) -> PathBuf {
        match self.seen(path) {
            Seen::Disk(at) => at,
            _ => path.to_path_buf(),
        }
    }

    /// What a file holds as the changes so far leave it; none where it is absent.
    ///
    /// # Errors
    /// It cannot be read.
    pub fn read(&self, path: &Path) -> Result<Option<Vec<u8>>> {
        match self.seen(path) {
            Seen::File(Some(hash)) => self.history.blob(&hash).map(Some),
            Seen::File(None) | Seen::Folder | Seen::Gone => Ok(None),
            // As what waits to be written leaves it, then as the disk has it.
            Seen::Disk(at) => crate::storage::read(&at).with_context(|| at.display().to_string()),
        }
    }

    fn exists(&self, path: &Path) -> bool {
        self.pending(path).unwrap_or_else(|| {
            let at = self.on_disk(path);
            // A file as what waits to be written leaves it; a folder there for what waits in it.
            crate::storage::waiting(&at).map_or_else(|| at.exists() || crate::storage::folder_waits(&at), |waits| waits.is_some())
        })
    }

    fn is_file(&self, path: &Path) -> bool {
        match self.seen(path) {
            Seen::File(after) => after.is_some(),
            Seen::Disk(at) => crate::storage::is_file(&at),
            Seen::Folder | Seen::Gone => false,
        }
    }

    fn is_dir(&self, path: &Path) -> bool {
        match self.seen(path) {
            Seen::Folder => true,
            Seen::Disk(at) => at.is_dir(),
            Seen::File(_) | Seen::Gone => false,
        }
    }

    /// A file's content as the changes so far leave it, kept; its hash, none where it is absent.
    fn current(&self, path: &Path) -> Result<Option<String>> {
        match self.seen(path) {
            Seen::File(after) => Ok(after),
            Seen::Disk(at) => self.history.keep_current(&at),
            Seen::Folder | Seen::Gone => Ok(None),
        }
    }

    /// Make the folders a path needs that do not exist.
    fn make_parents(&mut self, path: &Path) {
        let mut missing: Vec<PathBuf> = path
            .ancestors()
            .skip(1)
            .take_while(|d| !d.as_os_str().is_empty() && !self.exists(d))
            .map(Path::to_path_buf)
            .collect();
        missing.reverse();
        for dir in missing {
            self.change(Change::MakeDir { path: dir });
        }
    }

    /// Write a file: made, or overwritten.
    ///
    /// # Errors
    /// It cannot be written, or the history cannot keep what it was.
    pub fn write(&mut self, path: &Path, bytes: &[u8]) -> Result<()> {
        // A large content in a layer is stored as chunks: what the step keeps is its manifest, the other form gone.
        let manifest = crate::chunks::manifest_path(path);
        if let Some(text) = crate::content::chunked(path, bytes)? {
            self.write_stored(&manifest, text.as_bytes())?;
            self.drop_stored(path)
        } else {
            self.write_stored(path, bytes)?;
            self.drop_stored(&manifest)
        }
    }

    /// Write a file as it is to be stored - a manifest as a manifest - not as the content it may stand for.
    ///
    /// # Errors
    /// Its folder cannot be made, or it cannot be written.
    pub fn write_stored(&mut self, path: &Path, bytes: &[u8]) -> Result<()> {
        self.make_parents(path);
        let before = self.current(path)?;
        let after = Some(self.history.keep(bytes)?);
        if before == after {
            return Ok(());
        }
        self.change(Change::Write { path: path.to_path_buf(), before, after });
        Ok(())
    }

    /// Remove a stored file where there is one.
    fn drop_stored(&mut self, path: &Path) -> Result<()> {
        if !self.is_file(path) {
            return Ok(());
        }
        let before = self.current(path)?;
        self.change(Change::Write { path: path.to_path_buf(), before, after: None });
        Ok(())
    }

    /// Make a folder (and those it needs).
    ///
    /// # Errors
    /// It cannot be made.
    pub fn make_dir(&mut self, path: &Path) -> Result<()> {
        self.make_parents(path);
        if !self.is_dir(path) {
            self.change(Change::MakeDir { path: path.to_path_buf() });
        }
        Ok(())
    }

    /// Remove a file, or a folder with everything in it (moved into the history's trash, whole).
    ///
    /// # Errors
    /// It does not exist or cannot be removed.
    pub fn remove(&mut self, path: &Path) -> Result<()> {
        if self.is_dir(path) {
            self.history.trashed += 1;
            let trash = self.history.view.dir.join("trash");
            std::fs::create_dir_all(&trash)?;
            let to = trash.join(self.history.trashed.to_string());
            self.change(Change::Rename { from: path.to_path_buf(), to });
            return Ok(());
        }
        // A file, in whichever form it is stored.
        let manifest = crate::chunks::manifest_path(path);
        if !self.is_file(path) && !self.is_file(&manifest) {
            bail!("{}: nothing to remove", path.display());
        }
        self.drop_stored(path)?;
        self.drop_stored(&manifest)
    }

    /// Put a copy of a file or a folder at `to`, where there is nothing: copied whole into the history's trash, each
    /// file's permissions kept, then moved into place - so that undoing and redoing it moves it, programs still programs.
    ///
    /// # Errors
    /// Something is at `to`, or it cannot be copied or moved.
    pub fn place(&mut self, from: &Path, to: &Path) -> Result<()> {
        if self.exists(to) {
            bail!("{}: something is there", to.display());
        }
        self.history.trashed += 1;
        let trash = self.history.view.dir.join("trash");
        std::fs::create_dir_all(&trash)?;
        let staged = trash.join(self.history.trashed.to_string());
        copy_all(from, &staged).with_context(|| format!("{} to {}", from.display(), staged.display()))?;
        // On disk before the log says it is moved into place: the log holds the move, not what is moved.
        sync_all(&staged);
        self.make_parents(to);
        self.change(Change::Rename { from: staged, to: to.to_path_buf() });
        Ok(())
    }

    /// Move a file or a folder.
    ///
    /// # Errors
    /// The source does not exist, the target does, or the move fails.
    pub fn rename(&mut self, from: &Path, to: &Path) -> Result<()> {
        // A file held as its manifest moves as it.
        let (manifest, to_manifest) = (crate::chunks::manifest_path(from), crate::chunks::manifest_path(to));
        if !self.exists(from) && self.is_file(&manifest) && !self.exists(to) && !self.exists(&to_manifest) {
            self.make_parents(&to_manifest);
            self.change(Change::Rename { from: manifest, to: to_manifest });
            return Ok(());
        }
        if !self.exists(from) || self.exists(to) {
            bail!("{} cannot move to {}", from.display(), to.display());
        }
        self.make_parents(to);
        self.change(Change::Rename { from: from.to_path_buf(), to: to.to_path_buf() });
        Ok(())
    }

    /// Make its changes, and keep it as a step of the history. Returns the paths it touched.
    ///
    /// # Errors
    /// The changes cannot be said in the log, or a file cannot be written: nothing is changed then.
    pub fn commit(mut self) -> Result<Vec<PathBuf>> {
        let label = self.step.label.clone();
        tracing::info_span!("edit: said in the log and written").in_scope(|| self.history.apply_all(&label, &self.step.changes))?;
        self.committed = true;
        let paths = self.step.paths();
        self.history.written.extend(paths.iter().cloned());
        if !self.step.changes.is_empty() {
            let step = std::mem::replace(&mut self.step, Step::new(String::new(), String::new()));
            self.history.push(step);
        }
        Ok(paths)
    }
}

/// What a path is, as a transaction's changes so far leave it.
enum Seen {
    /// A file they wrote, by its content's hash, or (none) removed.
    File(Option<String>),
    /// A folder they made.
    Folder,
    /// Nothing: removed, or moved away.
    Gone,
    /// What the disk has, at this path - the one it had before they moved it.
    Disk(PathBuf),
}

/// Have a file, or a folder and all in it, on disk: best effort.
fn sync_all(path: &Path) {
    if path.is_dir() {
        for entry in std::fs::read_dir(path).into_iter().flatten().flatten() {
            sync_all(&entry.path());
        }
        if cfg!(unix) {
            let _ = std::fs::File::open(path).and_then(|folder| folder.sync_all());
        }
    } else {
        crate::storage::sync_file(path);
    }
}

/// A file or a folder copied, with everything in it; a file keeps its permissions.
fn copy_all(from: &Path, to: &Path) -> std::io::Result<()> {
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            copy_all(&entry.path(), &to.join(entry.file_name()))?;
        }
        return Ok(());
    }
    std::fs::copy(from, to).map(drop)
}

/// Keep bytes in a history's folder, once: their hash.
fn keep_in(dir: &Path, bytes: &[u8]) -> Result<String> {
    let hash = sha1_hex(bytes);
    let path = dir.join("blobs").join(&hash);
    if !path.exists() {
        write_atomic(&path, bytes)?;
    }
    Ok(hash)
}

/// The hash of what a file holds - as what waits to be written leaves it; none where it is absent.
fn current_hash(path: &Path) -> Option<String> {
    crate::storage::read(path).ok().flatten().map(|bytes| sha1_hex(&bytes))
}

fn is_empty_dir(path: &Path) -> bool {
    std::fs::read_dir(path).is_ok_and(|mut entries| entries.next().is_none())
}

#[cfg(test)]
mod tests {
    use super::{History, Kind, StepError};
    use crate::Project;
    use crate::cache::Cache;
    use crate::index::Index;
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

    /// The index of a project, for what undo reads of documents.
    fn index(root: &Path) -> Index {
        let project = Project::load(root).unwrap();
        let lock = Lock::try_acquire(root, "a test").unwrap();
        Index::open(&project, &Cache::open(&project, &lock).unwrap()).unwrap()
    }

    fn read(path: &Path) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }

    #[test]
    fn undoes_and_redoes_writes_moves_and_deletes() {
        let root = project("steps");
        let (mut history, _) = history(&root);
        let index = index(&root);
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
        history.undo(&index, None, false).unwrap();
        assert_eq!((read(&one).as_deref(), read(&two).as_deref()), (Some("ONE"), Some("two")));
        history.undo(&index, None, false).unwrap();
        // The folders the write made are gone with it: an undone copy leaves nothing behind.
        assert_eq!(read(&one).as_deref(), Some("one"));
        assert!(!root.join("layer/b").exists());
        history.redo(&index, None, false).unwrap();
        history.redo(&index, None, false).unwrap();
        assert!(!one.exists() && root.join("layer/d/c/two").exists());
        assert!(history.redo(&index, None, false).is_err());
        crate::cache::remove_all(&root);
    }

    #[test]
    fn a_large_write_is_kept_as_its_manifest_and_undone_by_it() {
        let root = project("chunked");
        let (mut history, _) = history(&root);
        let index = index(&root);
        let file = root.join(".layers/top/a.node/native.bin");
        let manifest = crate::chunks::manifest_path(&file);
        let big: Vec<u8> = (0..(2u32 << 20)).map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8).collect();
        let mut tx = history.begin("write");
        tx.write(&file, b"small first").unwrap();
        tx.commit().unwrap();
        let mut tx = history.begin("write big");
        tx.write(&file, &big).unwrap();
        tx.commit().unwrap();
        assert!(!file.exists() && manifest.is_file(), "a large content is stored as its manifest");
        assert_eq!(crate::content::read(&file).unwrap(), big);
        history.undo(&index, None, false).unwrap();
        assert!(!manifest.exists() && read(&file).as_deref() == Some("small first"));
        history.redo(&index, None, false).unwrap();
        assert_eq!(crate::content::read(&file).unwrap(), big);
        let mut tx = history.begin("remove");
        tx.remove(&file).unwrap();
        tx.commit().unwrap();
        assert!(!crate::content::exists(&file));
        history.undo(&index, None, false).unwrap();
        assert_eq!(crate::content::read(&file).unwrap(), big);
        crate::cache::remove_all(&root);
    }

    #[test]
    fn outside_changes_block_undo_unless_forced() {
        let root = project("outside");
        let (mut history, _) = history(&root);
        let index = index(&root);
        let one = root.join("layer/a/one");
        let mut tx = history.begin("edit");
        tx.write(&one, b"mine").unwrap();
        tx.commit().unwrap();
        std::fs::write(&one, "theirs").unwrap();
        match history.undo(&index, None, false) {
            Err(StepError::Changed(paths)) => assert_eq!(paths, std::slice::from_ref(&one)),
            other => panic!("{other:?}"),
        }
        history.undo(&index, None, true).unwrap();
        assert_eq!(read(&one).as_deref(), Some("one"));
        // Redo brings back what the forced undo overwrote.
        history.redo(&index, None, false).unwrap();
        assert_eq!(read(&one).as_deref(), Some("theirs"));
        crate::cache::remove_all(&root);
    }

    #[test]
    fn what_is_not_committed_touches_nothing_and_reads_its_own_changes() {
        let root = project("pending");
        let (one, new) = (root.join("layer/a/one"), root.join("layer/new/file"));
        let (mut history, _) = history(&root);
        let mut tx = history.begin("failed");
        tx.write(&one, b"half").unwrap();
        tx.write(&new, b"x").unwrap();
        tx.rename(&root.join("layer/a"), &root.join("layer/moved")).unwrap();
        // Read as its changes so far leave the files: written, made, moved.
        assert_eq!(tx.read(&root.join("layer/moved/one")).unwrap().as_deref(), Some(&b"half"[..]));
        assert_eq!(tx.read(&one).unwrap(), None, "moved away");
        assert_eq!(tx.pending(&new), Some(true));
        assert_eq!(tx.pending(&root.join("layer/new")), Some(true), "the folder made for it");
        assert_eq!(tx.pending(&root.join("layer/untouched")), None, "what it says nothing of is the disk's");
        // And no file is touched: dropped uncommitted, it changed nothing.
        assert_eq!(read(&one).as_deref(), Some("one"));
        assert!(!root.join("layer/new").exists() && !root.join("layer/moved").exists());
        drop(tx);
        assert_eq!(read(&one).as_deref(), Some("one"));
        assert!(history.done().is_empty());
        // Committed, the files are as it read them.
        let mut tx = history.begin("made");
        tx.write(&one, b"whole").unwrap();
        tx.rename(&root.join("layer/a"), &root.join("layer/moved")).unwrap();
        tx.commit().unwrap();
        assert_eq!(read(&root.join("layer/moved/one")).as_deref(), Some("whole"));
        assert!(!root.join("layer/a").exists());
        drop(history);
        crate::cache::remove_all(&root);
    }

    #[test]
    fn what_the_log_has_whole_and_the_files_have_not_is_written_when_the_project_is_next_taken() {
        use crate::storage::{Storage, Stored};
        let root = project("redo");
        let one = root.join("layer/a/one");
        {
            // Said whole, on disk - and its process gone before any file was written.
            let mut storage = Storage::open(&root).unwrap();
            let changes = [
                Stored::Put { path: "layer/a/one".into(), bytes: b"ONE".to_vec() },
                Stored::MakeDir { path: "layer/new".into() },
                Stored::Put { path: "layer/new/file".into(), bytes: b"x".to_vec() },
                Stored::Move { from: "layer/a".into(), to: "layer/moved".into() },
            ];
            storage.say("crashed", &changes).unwrap();
            // Another, cut short before it was whole: nothing.
            sfs::storage::began(&sfs::Log::shared(&root.join(".cache/filesystem/sfs"), sfs::log::Config::default()).unwrap())
                .unwrap();
            std::mem::forget(storage);
        }
        assert_eq!(read(&one).as_deref(), Some("one"), "no file was touched");
        // Whoever takes the project next writes it, before anything reads the files.
        let (history, recovered) = history(&root);
        assert!(recovered.as_deref().is_some_and(|said| said.contains("crashed")), "{recovered:?}");
        assert_eq!(read(&root.join("layer/moved/one")).as_deref(), Some("ONE"));
        assert_eq!(read(&root.join("layer/new/file")).as_deref(), Some("x"));
        assert!(!root.join("layer/a").exists());
        assert!(history.done().is_empty());
        drop(history);
        // Written again once: the next to take it finds nothing to write.
        let (history, recovered) = self::history(&root);
        assert_eq!(recovered, None);
        drop(history);
        crate::cache::remove_all(&root);
    }

    /// Where a records layer of the project keeps the record of a uid - the layer's own folder there, as a
    /// layer's is: only a record and the folder of its first two letters are written behind.
    fn record(root: &Path, uid: &str) -> PathBuf {
        // Under the root as the project names it, which is how every path of an edit is made.
        let records = std::fs::canonicalize(root).unwrap().join(".layers").join("top").join(".records");
        std::fs::create_dir_all(&records).unwrap();
        records.join(&uid[..2]).join(format!("{uid}.toml"))
    }

    #[test]
    fn records_are_written_behind_and_read_as_they_wait() {
        use crate::storage::{is_file, read as read_through, waiting, waiting_since, waiting_under};
        let root = project("behind");
        let (a, b) = (record(&root, "aa1"), record(&root, "bb2"));
        let index = index(&root);
        let (mut history, _) = history(&root);
        history.hold_back(true);
        let mut tx = history.begin("made");
        tx.write_stored(&a, b"one").unwrap();
        tx.write_stored(&b, b"two").unwrap();
        tx.commit().unwrap();
        // Said, and no file touched - nor their folders made: read as the set leaves them by whoever asks.
        assert!(!a.exists() && !a.parent().unwrap().exists());
        assert_eq!(read_through(&a).unwrap().as_deref(), Some(&b"one"[..]));
        assert_eq!(waiting_under(a.parent().unwrap().parent().unwrap()).len(), 2);
        // Changed again and removed while they wait: each as the last set leaves it.
        let mut tx = history.begin("again");
        tx.write_stored(&a, b"ONE!").unwrap();
        tx.remove(&b).unwrap();
        tx.commit().unwrap();
        assert_eq!(read_through(&a).unwrap().as_deref(), Some(&b"ONE!"[..]));
        assert!(!is_file(&b) && read_through(&b).unwrap().is_none());
        // Undone and redone while they wait, as when they are files.
        history.undo(&index, None, false).unwrap();
        assert_eq!(read_through(&a).unwrap().as_deref(), Some(&b"one"[..]));
        assert_eq!(read_through(&b).unwrap().as_deref(), Some(&b"two"[..]));
        history.redo(&index, None, false).unwrap();
        let (said, _) = waiting_since(&a).unwrap();
        // Written: the files as the last set left them, each stamped with when its change was said - what read
        // it while it waited finds the file to be the one it read - and nothing waits.
        history.hold_back(false);
        history.settle();
        assert_eq!(read(&a).as_deref(), Some("ONE!"));
        assert!(!b.exists());
        assert_eq!(std::fs::metadata(&a).unwrap().modified().unwrap(), said);
        assert!(waiting(&a).is_none() && waiting(&b).is_none());
        // What is no record alone is written as it is said, after all that waited.
        history.hold_back(true);
        let mut tx = history.begin("a record");
        tx.write_stored(&a, b"last").unwrap();
        tx.commit().unwrap();
        assert_eq!(read(&a).as_deref(), Some("ONE!"));
        history.hold_back(false);
        let mut tx = history.begin("a file");
        tx.write(&root.join("layer/a/one"), b"ONE").unwrap();
        tx.commit().unwrap();
        assert_eq!((read(&a).as_deref(), read(&root.join("layer/a/one")).as_deref()), (Some("last"), Some("ONE")));
        drop(history);
        crate::cache::remove_all(&root);
    }

    #[test]
    fn a_file_changed_by_something_else_while_ours_waits_is_left_and_ours_kept_aside() {
        let root = project("conflict");
        let (mut history, _) = history(&root);
        let a = record(&root, "aa1");
        std::fs::create_dir_all(a.parent().unwrap()).unwrap();
        std::fs::write(&a, "was").unwrap();
        history.hold_back(true);
        let mut tx = history.begin("ours");
        tx.write_stored(&a, b"ours").unwrap();
        tx.commit().unwrap();
        // Something else writes it before ours is.
        std::fs::write(&a, "theirs").unwrap();
        history.hold_back(false);
        history.settle();
        assert_eq!(read(&a).as_deref(), Some("theirs"), "not written over");
        assert_eq!(read(&root.join(".cache/conflicts/.layers/top/.records/aa/aa1.toml")).as_deref(), Some("ours"));
        assert!(crate::storage::waiting(&a).is_none(), "read as it is from here on");
        drop(history);
        crate::cache::remove_all(&root);
    }

    #[test]
    fn what_waited_when_its_process_ended_is_written_by_who_takes_the_project_next() {
        let root = project("behind-redo");
        let a = record(&root, "aa1");
        {
            let (mut history, _) = history(&root);
            history.hold_back(true);
            let mut tx = history.begin("made");
            tx.write_stored(&a, b"one").unwrap();
            tx.commit().unwrap();
            // Its process gone, with nothing written.
            std::mem::forget(history);
        }
        assert!(!a.exists());
        let (history, recovered) = history(&root);
        assert!(recovered.as_deref().is_some_and(|said| said.contains("made")), "{recovered:?}");
        assert_eq!(read(&a).as_deref(), Some("one"));
        assert!(crate::storage::waiting(&a).is_none(), "nothing waits of what was written again");
        drop(history);
        crate::cache::remove_all(&root);
    }

    #[test]
    fn merges_a_continued_edit() {
        let root = project("merge");
        let (mut history, _) = history(&root);
        let index = index(&root);
        let one = root.join("layer/a/one");
        for text in ["o", "on", "onx"] {
            let mut tx = history.begin("type").merging("one");
            tx.write(&one, text.as_bytes()).unwrap();
            tx.commit().unwrap();
        }
        assert_eq!(history.done().len(), 1);
        history.undo(&index, None, false).unwrap();
        assert_eq!(read(&one).as_deref(), Some("one"));
        crate::cache::remove_all(&root);
    }

    /// A project of one layer holding the assets `a` and `b`.
    fn documents_project(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("pe2-history-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".layers/top/a.node")).unwrap();
        std::fs::create_dir_all(root.join(".layers/top/b.node")).unwrap();
        std::fs::write(root.join("project.toml"), "[[layer]]\nname = \"top\"\n").unwrap();
        std::fs::write(root.join(".layers/top/a.node/node.toml"), "uid = \"uid://a\"\nx = 1\n").unwrap();
        std::fs::write(root.join(".layers/top/b.node/node.toml"), "uid = \"uid://b\"\ny = 1\n").unwrap();
        root
    }

    fn working_text(history: &History, uid: &str) -> Option<String> {
        let sha1 = history.document(uid)?.working.get("node.toml")?.clone()?;
        Some(String::from_utf8(history.content(&sha1).unwrap()).unwrap())
    }

    #[test]
    fn edits_documents_and_undoes_one_document_at_a_time() {
        let root = documents_project("documents");
        let (mut history, _) = history(&root);
        let index = index(&root);
        let edit = |history: &mut History, uid: &str, text: &str| {
            let path = uid.trim_start_matches("uid://");
            history.edit(&index, uid, path, "node.toml", text.as_bytes(), format!("Edit {path}"), None).unwrap()
        };
        assert!(edit(&mut history, "uid://a", "uid = \"uid://a\"\nx = 2\n"));
        assert!(edit(&mut history, "uid://b", "uid = \"uid://b\"\ny = 2\n"));
        assert!(edit(&mut history, "uid://a", "uid = \"uid://a\"\nx = 3\n"));
        // Nothing is written: the working copies hold the edits, and the index shows them.
        assert_eq!(
            std::fs::read_to_string(root.join(".layers/top/a.node/node.toml")).unwrap(),
            "uid = \"uid://a\"\nx = 1\n"
        );
        let mut shown = index.clone();
        shown.set_working(history.working());
        assert!(shown.assets["a"].text().contains("x = 3") && shown.assets["a"].on_disk().text().contains("x = 1"));
        assert!(shown.assets["a"].unsaved() && !index.assets["a"].unsaved());
        // Undone in `a`: its edits go back one by one, `b`'s stays.
        history.undo(&index, Some("uid://a"), false).unwrap();
        assert!(working_text(&history, "uid://a").unwrap().contains("x = 2"));
        history.undo(&index, Some("uid://a"), false).unwrap();
        assert!(history.document("uid://a").is_none(), "back as on disk, it is not unsaved");
        assert!(working_text(&history, "uid://b").unwrap().contains("y = 2"));
        assert!(history.to_undo(Some("uid://a")).is_none());
        // Redone in `a`, then undone everywhere: the last step first, whichever document it edited.
        history.redo(&index, Some("uid://a"), false).unwrap();
        history.undo(&index, None, false).unwrap();
        assert!(history.document("uid://a").is_none());
        history.undo(&index, None, false).unwrap();
        assert!(history.document("uid://b").is_none());
        crate::cache::remove_all(&root);
    }

    #[test]
    fn a_save_is_passed_over_and_a_change_on_disk_makes_a_document_stale() {
        let root = documents_project("save");
        let (mut history, _) = history(&root);
        let index = index(&root);
        let a = root.join(".layers/top/a.node/node.toml");
        history
            .edit(&index, "uid://a", "a", "node.toml", b"uid = \"uid://a\"\nx = 2\n", "Edit a".into(), None)
            .unwrap();
        let files = history.to_save("uid://a").unwrap();
        assert_eq!(files.len(), 1);
        let mut tx = history.begin("Save a").kind(Kind::Save);
        tx.write(&a, files[0].1.as_deref().unwrap()).unwrap();
        tx.commit().unwrap();
        history.saved("uid://a");
        let index = self::index(&root);
        assert!(history.document("uid://a").is_none());
        // Undone after the save: the edit is taken back from the working copy, which is unsaved again; the save stays.
        history.undo(&index, None, false).unwrap();
        assert!(working_text(&history, "uid://a").unwrap().contains("x = 1"));
        assert!(std::fs::read_to_string(&a).unwrap().contains("x = 2"));
        assert!(history.to_undo(None).is_none());
        // Changed on disk meanwhile: stale, and kept or taken.
        std::fs::write(&a, "uid = \"uid://a\"\nx = 9\n").unwrap();
        let index = self::index(&root);
        assert!(history.reconcile(&index));
        assert!(history.document("uid://a").unwrap().stale.is_some());
        history.keep_mine("uid://a");
        assert!(history.document("uid://a").unwrap().stale.is_none());
        history.revert("uid://a", "Revert a".into());
        assert!(history.document("uid://a").is_none());
        crate::cache::remove_all(&root);
    }

    #[test]
    fn restores_a_point_and_reverts_a_step_as_new_steps() {
        let root = documents_project("points");
        let (mut history, _) = history(&root);
        let index = index(&root);
        for (uid, text) in [("uid://a", "x = 2"), ("uid://b", "y = 2"), ("uid://a", "x = 3")] {
            let path = uid.trim_start_matches("uid://");
            history.edit(&index, uid, path, "node.toml", text.as_bytes(), format!("Edit {path}"), None).unwrap();
        }
        history.checkpoint("Three edits".into());
        // As after the first step: `a` as it was, `b` not yet edited - as on disk.
        let states = history.states_at(1, None);
        assert_eq!(states.len(), 2);
        let shown = history.working_at(1, None);
        let a = shown["uid://a"].files["node.toml"].as_ref().unwrap();
        assert_eq!(std::fs::read_to_string(&a.path).unwrap(), "x = 2");
        // `b`'s step taken back alone; `a`'s first cannot be, as a later step edited `a` again.
        assert!(history.revert_step(&index, 1, "Revert b".into()).unwrap());
        assert!(history.document("uid://b").is_none());
        assert!(history.revert_step(&index, 0, "Revert a".into()).is_err(), "a was edited since");
        // Restored to the start: a new step, the log keeping what was done.
        let steps = history.done().len();
        assert!(history.restore(&index, 0, None, "Restore".into()));
        assert!(history.documents().is_empty());
        assert_eq!(history.done().len(), steps + 1);
        history.undo(&index, None, false).unwrap();
        assert_eq!(working_text(&history, "uid://a").as_deref(), Some("x = 3"));
        crate::cache::remove_all(&root);
    }

    #[test]
    fn a_journaled_session_is_appended_to_and_read_back_as_it_was() {
        let root = documents_project("appended");
        let index = index(&root);
        let session = root.join(".cache/history/session.jsonl");
        {
            let (history, _) = history(&root);
            let mut history = history.journaled();
            for (n, text) in ["x = 1", "x = 2", "x = 3", "x = 4"].iter().enumerate() {
                history.edit(&index, "uid://a", "a", "node.toml", text.as_bytes(), format!("Edit {n}"), None).unwrap();
            }
            history.note_selection(Some("uid://a".into()));
            // Appended to, not written again: a line the session whole, then one for each step and the note.
            let lines = std::fs::read_to_string(&session).unwrap().lines().count();
            assert_eq!(lines, 5, "the whole once, and what changed since");
            // One taken back: its shape changed, and it is written whole.
            history.undo(&index, Some("uid://a"), false).unwrap();
            assert_eq!(std::fs::read_to_string(&session).unwrap().lines().count(), 1);
            history.edit(&index, "uid://a", "a", "node.toml", b"x = 5", "Edit again".into(), None).unwrap();
            // A crash: never closed - and its last line cut short.
            drop(history);
            let mut text = std::fs::read_to_string(&session).unwrap();
            text.push_str("{\"since\":{\"from\":9,\"done\":[");
            std::fs::write(&session, text).unwrap();
        }
        let (history, _) = self::history(&root);
        let mut history = history.journaled();
        let left = history.left().unwrap();
        let labels: Vec<&str> = left.done.iter().map(|step| step.label.as_str()).collect();
        assert_eq!(labels, ["Edit 0", "Edit 1", "Edit 2", "Edit again"]);
        assert!(left.undone.is_empty(), "what was undone, overtaken by the edit after it");
        history.resume();
        assert_eq!(working_text(&history, "uid://a").as_deref(), Some("x = 5"));
        history.close();
        crate::cache::remove_all(&root);
    }

    #[test]
    fn a_journaled_session_a_crash_ends_is_left_and_one_closed_is_not() {
        let root = documents_project("journal");
        let index = index(&root);
        {
            let (history, _) = history(&root);
            let mut history = history.journaled();
            history.edit(&index, "uid://a", "a", "node.toml", b"x = 5", "Edit a".into(), None).unwrap();
            // A crash: never closed.
            drop(history);
        }
        // Opened for a moment's work by another process - the host, a command's edit - and let go: it stays.
        let (history, _) = self::history(&root);
        history.let_go();
        let (history, _) = self::history(&root);
        let mut history = history.journaled();
        let left = history.left().unwrap();
        assert_eq!((left.done.len(), left.documents.len()), (1, 1));
        history.resume();
        assert_eq!(working_text(&history, "uid://a").as_deref(), Some("x = 5"));
        history.undo(&index, Some("uid://a"), false).unwrap();
        history.close();
        let (history, _) = self::history(&root);
        assert!(history.left().is_none());
        crate::cache::remove_all(&root);
    }
}
