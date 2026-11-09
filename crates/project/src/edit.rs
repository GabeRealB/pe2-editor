//! Edits of a layer: what the editor does to the active layer, each a transaction of the project's history
//! (`history.rs`), so each can be undone. They keep the layer's changeset its net effect (Layers, in the design):
//!
//! - **override** (copy-on-write): the version of an entry directly below the layer is copied into it, at the entry's
//!   resolved path, and the changeset records an override with that version's hash as its base;
//! - **delete**: an entry the layer makes leaves it - its content and its record (a create then a delete cancel); an
//!   entry from below gets a delete, and a copy the layer held of it goes; a folder deleted takes the records of what
//!   it holds;
//! - **rename**: an entry the layer makes moves on disk and in its record; one from below gets a move (with its
//!   override, if it has one, whose copy moves too), and a move back where it was below is dropped;
//! - **resolve**: a stale override is kept (its base becomes the version below now) or given up (its copy goes); an
//!   operation whose entry is gone below is dropped;
//! - **write a file** of an entry (its `node.toml`, a plain file's content): in the layer's own copy, made first -
//!   in the same step - where the entry comes from below.
//!
//! Who may edit which layer (locks, the active layer) is the caller's to decide.

use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use crate::changeset::{Base, Changeset, Op};
use crate::history::History;
use crate::index::{BUNDLE_SUFFIX, Index, Kind};
use crate::{Layer, Project};

/// How an operation's problem is resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// Keep the layer's version of a stale override: its base becomes the version below now.
    KeepMine,
    /// Give up the layer's version: the override's copy goes (a move with it stays a move).
    TakeBase,
    /// Drop the operation, and the copy an override held.
    Drop,
}

/// The layer to edit and its changeset as it stands.
fn layer<'a>(project: &'a Project, index: &Index, name: &str) -> Result<(&'a Layer, Changeset)> {
    let layer = project.layer(name)?;
    let changeset = index.changeset(name).with_context(|| format!("the index has no layer {name}"))?.clone();
    Ok((layer, changeset))
}

/// An entry's name, as a step's label shows it.
fn name(path: &str) -> &str {
    let path = path.strip_suffix(BUNDLE_SUFFIX).unwrap_or(path);
    path.rsplit('/').next().unwrap_or(path)
}

/// Where an operation of the layer puts an entry: its move's target, else where it is below.
fn target(op: Option<&Op>, below: &str) -> String {
    match op {
        Some(Op::Move { to } | Op::Override { to: Some(to), .. }) => to.clone(),
        _ => below.to_string(),
    }
}

/// Whether a layer holds a file or a folder at a path: on disk, or as its store or records keep it.
fn holds(path: &std::path::Path) -> bool {
    path.exists() || crate::content::exists(path) || crate::pack::is_dir(path)
}

/// What an edit does to a layer's files and its list, through its step: on a layer stored as a tree, at once; on a
/// records layer (`records`), what it writes of files is gathered, and written as its records with its changeset - one
/// record an entry - so that undo takes back record files, as any.
struct Out<'a, 'h> {
    tx: &'a mut crate::history::Transaction<'h>,
    project: &'a Project,
    index: &'a Index,
    layer: &'a Layer,
    /// What a records layer's files become, by their path in it: written, or (none) removed.
    records: Option<BTreeMap<String, Option<Vec<u8>>>>,
}

impl<'a, 'h> Out<'a, 'h> {
    fn new(
        tx: &'a mut crate::history::Transaction<'h>,
        project: &'a Project,
        index: &'a Index,
        layer: &'a Layer,
    ) -> Self {
        let records = crate::records::is_records(&layer.path).then(BTreeMap::new);
        Out { tx, project, index, layer, records }
    }

    /// A path's place in the layer, as its records name it.
    fn rel(&self, path: &std::path::Path) -> Result<String> {
        let rel =
            path.strip_prefix(&self.layer.path).with_context(|| format!("{}: not in the layer", path.display()))?;
        Ok(rel.to_string_lossy().replace('\\', "/"))
    }

    fn write(&mut self, path: &std::path::Path, bytes: &[u8]) -> Result<()> {
        if self.records.is_some() {
            let rel = self.rel(path)?;
            if let Some(records) = &mut self.records {
                records.insert(rel, Some(bytes.to_vec()));
            }
            return Ok(());
        }
        self.tx.write(path, bytes)
    }

    /// Remove a file, or a folder and all in it.
    fn remove(&mut self, path: &std::path::Path) -> Result<()> {
        if self.records.is_some() {
            let rel = self.rel(path)?;
            if let Some(records) = &mut self.records {
                let inside = format!("{rel}/");
                for (at, bytes) in records.iter_mut() {
                    if at.starts_with(&inside) {
                        *bytes = None;
                    }
                }
                records.insert(rel, None);
            }
            return Ok(());
        }
        self.tx.remove(path)
    }

    /// A folder made: a records layer's are where its entries are.
    fn make_dir(&mut self, path: &std::path::Path) -> Result<()> {
        if self.records.is_some() {
            return Ok(());
        }
        self.tx.make_dir(path)
    }

    /// What a layer holds at a path moved: a records layer's goes with its entry's place.
    fn rename(&mut self, from: &std::path::Path, to: &std::path::Path) -> Result<()> {
        if self.records.is_some() {
            return Ok(());
        }
        self.tx.rename(from, to)
    }

    /// Whether the layer holds a file or a folder at a path, as this edit leaves it so far.
    fn exists(&self, path: &std::path::Path) -> bool {
        if let Some(records) = &self.records
            && let Ok(rel) = self.rel(path)
            && let Some(bytes) = records.get(&rel)
        {
            return bytes.is_some();
        }
        holds(path)
    }

    /// Done with the layer: what was written of a records layer's files must be in its records, or it would be lost.
    fn done(self) -> Result<()> {
        if self.records.as_ref().is_some_and(|written| !written.is_empty()) {
            bail!("{}: files written, and its records not: an edit's mistake", self.layer.name);
        }
        Ok(())
    }

    /// What was written of a records layer's files, written into its records (nothing to do for a tree).
    fn flush(&mut self, changeset: &Changeset) -> Result<()> {
        if self.records.as_ref().is_some_and(|written| !written.is_empty()) {
            return self.changeset(changeset);
        }
        Ok(())
    }

    /// Write the layer's list: its changeset, or its records as the changeset and the files written make them.
    fn changeset(&mut self, changeset: &Changeset) -> Result<()> {
        let Some(written) = &self.records else {
            return self.tx.write(&self.layer.changeset, changeset.render().as_bytes());
        };
        let old = crate::records::Set::records_of(&self.layer.path).unwrap_or_default();
        let (new, gone) = crate::records::records_for(self.project, self.index, self.layer, changeset, &old, written)?;
        for (uid, record) in &new {
            self.tx.write(&crate::records::path_of(&self.layer.path, uid), record.render().as_bytes())?;
        }
        for uid in &gone {
            self.tx.remove(&crate::records::path_of(&self.layer.path, uid))?;
        }
        // What is written is in the records now.
        self.records = Some(BTreeMap::new());
        Ok(())
    }
}

/// Override an entry from below in a layer, recording it: the layer holds nothing of it yet - what it writes it holds,
/// the rest coming from below as it is there. Where the override is.
fn own_copy(
    out: &mut Out<'_, '_>,
    index: &Index,
    layer: &Layer,
    layer_name: &str,
    changeset: &mut Changeset,
    uid: &str,
) -> Result<String> {
    let below = index.below(layer_name, uid).with_context(|| format!("no layer below {layer_name} has {uid}"))?;
    if below.kind == Kind::Folder {
        bail!("{}: a folder is overridden by its files, one by one", below.path);
    }
    let op = changeset.ops.get(uid);
    if matches!(op, Some(Op::Override { .. } | Op::Delete)) {
        bail!("{layer_name} overrides or deletes {} already", below.path);
    }
    let at = target(op, &below.path);
    let to = matches!(op, Some(Op::Move { .. })).then(|| at.clone());
    let mut files = BTreeMap::new();
    // Code is edited where it is, with its own tools: a files layer holds a copy of what it overrides at once.
    if layer.kind == crate::LayerKind::Files {
        let store = crate::chunks::Store::at(project_root(layer));
        for (file, source) in &below.files {
            let bytes = crate::content::read(source).with_context(|| source.display().to_string())?;
            let disk = if file.is_empty() { layer.disk(&at) } else { layer.disk(&at).join(file) };
            out.write(&disk, &bytes)?;
            let was = store.put_file(&bytes)?;
            files.insert(file.clone(), Base { was: Some(was), removed: false });
        }
    }
    changeset.ops.insert(uid.to_string(), Op::Override { files, to });
    out.changeset(changeset)?;
    Ok(at)
}

/// The project a layer is of: the folder holding `.layers`, where its changeset is.
fn project_root(layer: &Layer) -> &std::path::Path {
    let layers = layer.changeset.parent().unwrap_or(&layer.changeset);
    layers.parent().unwrap_or(layers)
}

/// Write a file of an entry in a layer - a bundle's file by its name, a plain file itself (an empty name) - in the
/// layer's own version of it: the layer's entry, its override's copy, or a copy made now (in the same step, so one
/// undo takes both back). `merge` makes continued edits (typing in a field) one step.
///
/// # Errors
/// The entry is a folder, deleted in the layer, not found, or files cannot be written.
#[expect(clippy::too_many_arguments, reason = "an edit's context")]
pub fn write_file(
    project: &Project,
    index: &Index,
    layer_name: &str,
    uid: &str,
    file: &str,
    bytes: &[u8],
    label: String,
    merge: Option<String>,
    history: &mut History,
) -> Result<Vec<PathBuf>> {
    let (layer, mut changeset) = layer(project, index, layer_name)?;
    let mut tx = history.begin(label);
    if let Some(key) = merge {
        tx = tx.merging(key);
    }
    let mut out = Out::new(&mut tx, project, index, layer);
    let path = own_path(&mut out, index, layer, layer_name, &mut changeset, uid)?;
    let at = Place { project, index, layer, layer_name, uid, path: &path };
    put_files(&mut out, &at, &mut changeset, &[(file, Some(bytes))])?;
    out.done()?;
    tx.commit()
}

/// Where files of an entry are written: in a layer's own version of it.
struct Place<'a> {
    project: &'a Project,
    index: &'a Index,
    layer: &'a Layer,
    layer_name: &'a str,
    uid: &'a str,
    /// The entry's path in the layer.
    path: &'a str,
}

impl Place<'_> {
    /// A file of the entry on disk: a bundle's by its name, a plain file itself (an empty name).
    fn disk(&self, file: &str) -> PathBuf {
        if file.is_empty() { self.layer.disk(self.path) } else { self.layer.disk(self.path).join(file) }
    }
}

/// Write (with content) or remove (without) files of an entry in the layer's own version of it. The layer's own entry
/// takes them as they are. An override holds a file it writes whole, recording the version below now as what it was
/// made from (kept in the store, for merging when it changes below) - or, written as it is below, holds nothing of it -
/// and records a file it removes as removed.
///
/// # Errors
/// A file cannot be written or kept.
fn put_files(
    out: &mut Out<'_, '_>,
    at: &Place<'_>,
    changeset: &mut Changeset,
    files: &[(&str, Option<&[u8]>)],
) -> Result<()> {
    let Some(Op::Override { files: mut bases, to }) = changeset.ops.get(at.uid).cloned() else {
        for (file, bytes) in files {
            match bytes {
                Some(bytes) => out.write(&at.disk(file), bytes)?,
                None if out.exists(&at.disk(file)) => out.remove(&at.disk(file))?,
                None => {}
            }
        }
        // A records layer's files are its records': written as they now are.
        return out.flush(changeset);
    };
    let below =
        at.index.below(at.layer_name, at.uid).with_context(|| format!("{} is gone below {}", at.uid, at.layer_name))?;
    let store = crate::chunks::Store::at(&at.project.root);
    for (file, bytes) in files {
        let disk = at.disk(file);
        let now = below.files.iter().find(|(name, _)| name == file).map(|(_, path)| path);
        let now_sha1 = below.sha1s.get(*file).cloned();
        let held = out.exists(&disk);
        match bytes {
            Some(bytes) if now_sha1.as_deref() == Some(crate::extract::sha1_hex(bytes).as_str()) => {
                // As it is below: nothing to hold.
                if held {
                    out.remove(&disk)?;
                }
                bases.remove(*file);
            }
            Some(bytes) => {
                out.write(&disk, bytes)?;
                if let Some(now) = now {
                    store.put_file(&crate::content::read(now)?)?;
                }
                bases.insert((*file).to_string(), Base { was: now_sha1, removed: false });
            }
            None => {
                if held {
                    out.remove(&disk)?;
                }
                match now_sha1 {
                    Some(was) => bases.insert((*file).to_string(), Base { was: Some(was), removed: true }),
                    None => bases.remove(*file),
                };
            }
        }
    }
    changeset.ops.insert(at.uid.to_string(), Op::Override { files: bases, to });
    out.changeset(changeset)
}

/// Where a layer's own version of an entry is: the layer's entry, its override's copy, or a copy made now.
fn own_path(
    out: &mut Out<'_, '_>,
    index: &Index,
    layer: &Layer,
    layer_name: &str,
    changeset: &mut Changeset,
    uid: &str,
) -> Result<String> {
    Ok(match (changeset.entries.get(uid).cloned(), changeset.ops.get(uid).cloned()) {
        (Some(path), _) => path,
        (None, Some(op @ Op::Override { .. })) => {
            let below = index.below(layer_name, uid).with_context(|| format!("{uid} is gone below {layer_name}"))?;
            target(Some(&op), &below.path)
        }
        (None, Some(Op::Delete)) => bail!("{layer_name} deletes {uid}"),
        (None, _) => own_copy(out, index, layer, layer_name, changeset, uid)?,
    })
}

/// Save a document: write its files (each with its content, none to remove it) into the layer's own version of its
/// entry - made first, in the same step, where the entry comes from below. A step undo passes over: what it undoes are
/// the edits, which leave the document unsaved again.
///
/// # Errors
/// The entry is deleted in the layer or not found, or files cannot be written.
pub fn save(
    project: &Project,
    index: &Index,
    layer_name: &str,
    uid: &str,
    files: &[(String, Option<Vec<u8>>)],
    label: String,
    history: &mut History,
) -> Result<Vec<PathBuf>> {
    let (layer, mut changeset) = layer(project, index, layer_name)?;
    let mut tx = history.begin(label).kind(crate::history::Kind::Save);
    let mut out = Out::new(&mut tx, project, index, layer);
    let path = own_path(&mut out, index, layer, layer_name, &mut changeset, uid)?;
    let at = Place { project, index, layer, layer_name, uid, path: &path };
    let files: Vec<(&str, Option<&[u8]>)> = files.iter().map(|(f, b)| (f.as_str(), b.as_deref())).collect();
    put_files(&mut out, &at, &mut changeset, &files)?;
    out.done()?;
    tx.commit()
}

/// Make a new bundle in a layer, of this declaration, under its uid. Returns the paths touched.
///
/// # Errors
/// The layer holds no such uid to give, or it cannot be written.
pub fn create(
    project: &Project,
    index: &Index,
    layer_name: &str,
    uid: &str,
    path: &str,
    (declaration, files): (&str, &[(&str, &[u8])]),
    history: &mut History,
) -> Result<Vec<PathBuf>> {
    let (layer, mut changeset) = layer(project, index, layer_name)?;
    anyhow::ensure!(!changeset.entries.contains_key(uid), "{layer_name} already has {uid}");
    let bundle = format!("{path}{BUNDLE_SUFFIX}");
    let mut tx = history.begin(format!("New {}", name(&bundle)));
    let mut out = Out::new(&mut tx, project, index, layer);
    let mut taken = HashSet::from([uid.to_string()]);
    make_parents(&mut out, index, layer, &mut changeset, path, &mut taken)?;
    out.write(&layer.disk(&bundle).join(crate::index::NODE_FILE), declaration.as_bytes())?;
    for (name, bytes) in files {
        out.write(&layer.disk(&bundle).join(name), bytes)?;
    }
    changeset.entries.insert(uid.to_string(), bundle);
    out.changeset(&changeset)?;
    out.done()?;
    tx.commit()
}

/// The folders a new entry at `path` is the first thing in, made in the step and listed: of this step, or the index
/// lists them and the layer's list changes under the history. `taken` holds the uids given in the step.
fn make_parents(
    out: &mut Out<'_, '_>,
    index: &Index,
    layer: &Layer,
    changeset: &mut Changeset,
    path: &str,
    taken: &mut HashSet<String>,
) -> Result<()> {
    for (at, _) in path.match_indices('/') {
        let folder = &path[..at];
        if index.entries.contains_key(folder) || changeset.entries.values().any(|known| known == folder) {
            continue;
        }
        let uid = new_uid(index, taken)?;
        out.make_dir(&layer.disk(folder))?;
        changeset.entries.insert(uid, folder.to_string());
    }
    Ok(())
}

/// A uid neither the project nor this step has given.
fn new_uid(index: &Index, taken: &mut HashSet<String>) -> Result<String> {
    let id = crate::uid::new(|id| {
        let text = crate::uid::text(id);
        taken.contains(&text) || index.by_uid.contains_key(&text)
    })?;
    let text = crate::uid::text(id);
    taken.insert(text.clone());
    Ok(text)
}

/// Make a plain file in a layer at a resolved path, with its content and a new uid; the folders it is the first thing
/// in, too. Returns the paths touched.
///
/// # Errors
/// Something is at that path already, or the layer cannot be written.
pub fn create_file(
    project: &Project,
    index: &Index,
    layer_name: &str,
    path: &str,
    bytes: &[u8],
    history: &mut History,
) -> Result<Vec<PathBuf>> {
    let (layer, mut changeset) = layer(project, index, layer_name)?;
    anyhow::ensure!(!index.entries.contains_key(path), "{path} is there already");
    let mut tx = history.begin(format!("New {}", name(path)));
    let mut out = Out::new(&mut tx, project, index, layer);
    let mut taken = HashSet::new();
    make_parents(&mut out, index, layer, &mut changeset, path, &mut taken)?;
    out.write(&layer.disk(path), bytes)?;
    let uid = new_uid(index, &mut taken)?;
    changeset.entries.insert(uid, path.to_string());
    out.changeset(&changeset)?;
    out.done()?;
    tx.commit()
}

/// Make a folder in a layer at a resolved path, with a new uid; the folders it is in, too. Returns the paths touched.
///
/// # Errors
/// Something is at that path already, or the layer cannot be written.
pub fn create_folder(
    project: &Project,
    index: &Index,
    layer_name: &str,
    path: &str,
    history: &mut History,
) -> Result<Vec<PathBuf>> {
    let (layer, mut changeset) = layer(project, index, layer_name)?;
    anyhow::ensure!(!index.entries.contains_key(path), "{path} is there already");
    let mut tx = history.begin(format!("New {}", name(path)));
    let mut out = Out::new(&mut tx, project, index, layer);
    let mut taken = HashSet::new();
    make_parents(&mut out, index, layer, &mut changeset, path, &mut taken)?;
    out.make_dir(&layer.disk(path))?;
    let uid = new_uid(index, &mut taken)?;
    changeset.entries.insert(uid, path.to_string());
    out.changeset(&changeset)?;
    out.done()?;
    tx.commit()
}

/// Override an entry from below in a layer: its own copy, made from the version directly below. Returns the paths
/// touched.
///
/// # Errors
/// The entry is the layer's own, a folder, not below it, already overridden or deleted there, or not written.
pub fn override_entry(
    project: &Project,
    index: &Index,
    layer_name: &str,
    uid: &str,
    history: &mut History,
) -> Result<Vec<PathBuf>> {
    let (layer, mut changeset) = layer(project, index, layer_name)?;
    if changeset.entries.contains_key(uid) {
        bail!("{layer_name} makes it: it is the layer's own already");
    }
    let shown = index.below(layer_name, uid).map_or_default(|b| target(changeset.ops.get(uid), &b.path));
    let mut tx = history.begin(format!("Override {} in {layer_name}", name(&shown)));
    let mut out = Out::new(&mut tx, project, index, layer);
    own_copy(&mut out, index, layer, layer_name, &mut changeset, uid)?;
    out.done()?;
    tx.commit()
}

/// The entry paired with one, by its real path: a node's folder of children (`x.node` and `x`), or a folder's node.
/// They are one thing - a node with what is inside it - and are renamed, moved and deleted together.
#[must_use]
pub fn paired(index: &Index, real: &str) -> Option<String> {
    let other = match real.strip_suffix(BUNDLE_SUFFIX) {
        Some(folder) => (folder.to_string(), Kind::Folder),
        None => (format!("{real}{BUNDLE_SUFFIX}"), Kind::Asset),
    };
    let entry = index.entries.get(&other.0)?;
    let mine = index.entries.get(real)?;
    (entry.kind == other.1 && mine.kind != Kind::File).then_some(other.0)
}

/// The uid of the entry at a real path.
fn uid_at(index: &Index, real: &str) -> Option<String> {
    index.entries.get(real).map(|e| e.uid.clone())
}

/// The real path an entry resolves at now, by its uid.
fn real_of(index: &Index, uid: &str) -> Option<String> {
    index.entries.iter().find(|(_, e)| e.uid == uid).map(|(path, _)| path.clone())
}

/// Delete an entry from a layer's view, with the entry paired with it (`paired`): one the layer makes leaves it; one
/// from below gets a delete.
///
/// # Errors
/// The entry is neither the layer's nor below it, or files cannot be written.
pub fn delete(
    project: &Project,
    index: &Index,
    layer_name: &str,
    uid: &str,
    history: &mut History,
) -> Result<Vec<PathBuf>> {
    let (layer, mut changeset) = layer(project, index, layer_name)?;
    let shown = real_of(index, uid).unwrap_or_else(|| uid.to_string());
    let mut tx = history.begin(format!("Delete {}", name(&shown)));
    let mut out = Out::new(&mut tx, project, index, layer);
    delete_in(&mut out, index, layer, layer_name, &mut changeset, uid)?;
    if let Some(other) = paired(index, &shown).and_then(|p| uid_at(index, &p)) {
        delete_in(&mut out, index, layer, layer_name, &mut changeset, &other)?;
    }
    out.changeset(&changeset)?;
    out.done()?;
    tx.commit()
}

/// Delete an entry within a step: its files, and what the changeset says of it.
fn delete_in(
    out: &mut Out<'_, '_>,
    index: &Index,
    layer: &Layer,
    layer_name: &str,
    changeset: &mut Changeset,
    uid: &str,
) -> Result<()> {
    if let Some(path) = changeset.entries.get(uid).cloned() {
        if out.exists(&layer.disk(&path)) {
            out.remove(&layer.disk(&path))?;
        }
        let inside = format!("{path}/");
        changeset.entries.retain(|_, p| *p != path && !p.starts_with(&inside));
        return Ok(());
    }
    let below = index.below(layer_name, uid).with_context(|| format!("no layer below {layer_name} has {uid}"))?;
    let at = target(changeset.ops.get(uid), &below.path);
    if matches!(changeset.ops.get(uid), Some(Op::Override { .. })) && out.exists(&layer.disk(&at)) {
        out.remove(&layer.disk(&at))?;
    }
    if below.kind == Kind::Folder {
        // A folder deleted subsumes what the layer did inside it.
        for (inner, path) in index.below_inside(layer_name, &below.path) {
            if let Some(op) = changeset.ops.remove(&inner)
                && matches!(op, Op::Override { .. })
            {
                let copy = layer.disk(&target(Some(&op), &path));
                if out.exists(&copy) {
                    out.remove(&copy)?;
                }
            }
        }
    }
    changeset.ops.insert(uid.to_string(), Op::Delete);
    Ok(())
}

/// Take an entry the layer lists but no longer holds out of its list: deleted outside the editor, it is kept listed (its
/// uid held should its path come back) until this says it is gone. Nothing is written but the list, so a layer whose
/// content is its maker's alone (a read-only mount) can forget too.
///
/// # Errors
/// The layer does not list the uid, or still holds its path where the index does not leave it out.
pub fn forget(
    project: &Project,
    index: &Index,
    layer_name: &str,
    uid: &str,
    history: &mut History,
) -> Result<Vec<PathBuf>> {
    let (layer, mut changeset) = layer(project, index, layer_name)?;
    let path = changeset.entries.get(uid).cloned().with_context(|| format!("{layer_name} does not list {uid}"))?;
    // What the index leaves out is no entry, whatever is on disk: an extension's folder, listed by an older editor.
    anyhow::ensure!(
        index.leaves_out(layer_name, &path) || !holds(&layer.disk(&path)),
        "{layer_name} still holds {path}"
    );
    let mut tx = history.begin(format!("Forget {}", name(&path)));
    let mut out = Out::new(&mut tx, project, index, layer);
    let inside = format!("{path}/");
    changeset.entries.retain(|_, p| *p != path && !p.starts_with(&inside));
    out.changeset(&changeset)?;
    out.done()?;
    tx.commit()
}

/// Adopt what a layer holds unlisted at and under a path (`Problem::Unlisted`): listed in its changeset, an entry gone
/// from the layer whose content is found there keeping its uid (moved), the rest new entries (`Index::adoption`). Only
/// the list is written.
///
/// # Errors
/// The layer's list cannot be written, nothing is unlisted there, or a new uid cannot be made.
pub fn adopt(
    project: &Project,
    index: &Index,
    layer_name: &str,
    path: &str,
    history: &mut History,
) -> Result<Vec<PathBuf>> {
    let (layer, mut changeset) = layer(project, index, layer_name)?;
    let adopted = index.adoption(layer_name, path)?;
    anyhow::ensure!(!adopted.is_empty(), "{layer_name} lists all it holds at {path}");
    let mut tx = history.begin(format!("Adopt {}", name(path)));
    let mut out = Out::new(&mut tx, project, index, layer);
    changeset.entries.extend(adopted);
    out.changeset(&changeset)?;
    out.done()?;
    tx.commit()
}

/// Remove what a layer holds unlisted at a path (`Problem::Unlisted`) from its folder: the file, or the folder and all
/// in it.
///
/// # Errors
/// The path is listed, or cannot be removed.
pub fn remove_unlisted(
    project: &Project,
    index: &Index,
    layer_name: &str,
    path: &str,
    history: &mut History,
) -> Result<Vec<PathBuf>> {
    let (layer, changeset) = layer(project, index, layer_name)?;
    anyhow::ensure!(!changeset.entries.values().any(|p| p == path), "{layer_name} lists {path}: delete it instead");
    let mut tx = history.begin(format!("Remove {} from {layer_name}", name(path)));
    let mut out = Out::new(&mut tx, project, index, layer);
    out.remove(&layer.disk(path))?;
    out.done()?;
    tx.commit()
}

/// What a rename is called: of the name, or of the folder where the name stays.
fn rename_label(from: &str, to: &str) -> String {
    if name(from) != name(to) {
        return format!("Rename {} to {}", name(from), name(to));
    }
    let folder = to.rsplit_once('/').map_or("the top", |(folder, _)| folder.rsplit('/').next().unwrap_or(folder));
    format!("Move {} to {folder}", name(to))
}

/// Rename or move an entry to another resolved path, with the entry paired with it (`paired`): one the layer makes
/// moves on disk; one from below gets a move.
///
/// # Errors
/// Something resolves to the target already, the entry is neither the layer's nor below it, or files cannot be
/// moved.
pub fn rename(
    project: &Project,
    index: &Index,
    layer_name: &str,
    uid: &str,
    to: &str,
    history: &mut History,
) -> Result<Vec<PathBuf>> {
    let (layer, mut changeset) = layer(project, index, layer_name)?;
    let from = real_of(index, uid).with_context(|| format!("{uid}: no entry"))?;
    let mut tx = history.begin(rename_label(&from, to));
    let mut out = Out::new(&mut tx, project, index, layer);
    rename_in(&mut out, index, layer, layer_name, &mut changeset, uid, to)?;
    if let Some(other) = paired(index, &from) {
        let other_to = match to.strip_suffix(BUNDLE_SUFFIX) {
            Some(folder) => folder.to_string(),
            None => format!("{to}{BUNDLE_SUFFIX}"),
        };
        let other_uid = uid_at(index, &other).with_context(|| format!("{other}: no entry"))?;
        rename_in(&mut out, index, layer, layer_name, &mut changeset, &other_uid, &other_to)?;
    }
    out.changeset(&changeset)?;
    out.done()?;
    tx.commit()
}

/// Rename an entry within a step: its files where the layer holds them, and what the changeset says of it.
fn rename_in(
    out: &mut Out<'_, '_>,
    index: &Index,
    layer: &Layer,
    layer_name: &str,
    changeset: &mut Changeset,
    uid: &str,
    to: &str,
) -> Result<()> {
    if index.entries.contains_key(to) {
        bail!("{to}: an entry is there already");
    }
    if let Some(from) = changeset.entries.get(uid).cloned() {
        out.rename(&layer.disk(&from), &layer.disk(to))?;
        let inside = format!("{from}/");
        for path in changeset.entries.values_mut() {
            if *path == from {
                *path = to.to_string();
            } else if let Some(rest) = path.strip_prefix(&inside) {
                *path = format!("{to}/{rest}");
            }
        }
        return Ok(());
    }
    let below = index.below(layer_name, uid).with_context(|| format!("no layer below {layer_name} has {uid}"))?;
    let op = changeset.ops.get(uid).cloned();
    if matches!(op, Some(Op::Delete)) {
        bail!("{layer_name} deletes {}", below.path);
    }
    let from = target(op.as_ref(), &below.path);
    let back = (to != below.path).then(|| to.to_string());
    let new = match op {
        Some(Op::Override { files, .. }) => {
            // What it holds of it goes with it; it may hold nothing yet.
            let held = layer.disk(&from);
            if out.exists(&held) {
                out.rename(&held, &layer.disk(to))?;
            }
            Some(Op::Override { files, to: back })
        }
        _ => back.map(|to| Op::Move { to }),
    };
    match new {
        Some(op) => changeset.ops.insert(uid.to_string(), op),
        // Moved back where it is below: no move left.
        None => changeset.ops.remove(uid),
    };
    Ok(())
}

/// Resolve a layer's operation on an entry.
///
/// # Errors
/// The layer has no operation on it, the resolution does not fit it, or files cannot be written.
pub fn resolve(
    project: &Project,
    index: &Index,
    layer_name: &str,
    uid: &str,
    resolution: Resolution,
    history: &mut History,
) -> Result<Vec<PathBuf>> {
    let (layer, mut changeset) = layer(project, index, layer_name)?;
    let op = changeset.ops.get(uid).cloned().with_context(|| format!("{layer_name} does nothing to {uid}"))?;
    let below = index.below(layer_name, uid);
    let at = target(Some(&op), below.as_ref().map_or("", |b| b.path.as_str()));
    let shown = if at.is_empty() { uid } else { name(&at) };
    match (resolution, &op) {
        (Resolution::KeepMine, Op::Override { files, to }) => {
            let below = below.with_context(|| format!("{uid} is gone below {layer_name}"))?;
            let store = crate::chunks::Store::at(&project.root);
            let mut tx = history.begin(format!("Keep my version of {shown}"));
            let mut out = Out::new(&mut tx, project, index, layer);
            let place = Place { project, index, layer, layer_name, uid, path: &at };
            let mut kept = BTreeMap::new();
            for (file, base) in files {
                let now = below.files.iter().find(|(name, _)| name == file).map(|(_, path)| path);
                let now_sha1 = below.sha1s.get(file).cloned();
                // Mine over what is below now - mine where both changed the same thing - is what it holds from now.
                if !base.removed
                    && base.was != now_sha1
                    && let (Some(was), Some(now)) = (&base.was, now)
                    && let Ok(old) = store.get_file(was)
                {
                    let mine = crate::content::read(&place.disk(file))?;
                    let name = if file.is_empty() { name(&at) } else { file.as_str() };
                    let merged = crate::merge::merge(name, &old, &crate::content::read(now)?, &mine);
                    out.write(&place.disk(file), &merged.bytes)?;
                }
                if let Some(now) = now {
                    store.put_file(&crate::content::read(now)?)?;
                }
                kept.insert(file.clone(), Base { was: now_sha1, removed: base.removed });
            }
            changeset.ops.insert(uid.to_string(), Op::Override { files: kept, to: to.clone() });
            out.changeset(&changeset)?;
            out.done()?;
            tx.commit()
        }
        (Resolution::TakeBase | Resolution::Drop, Op::Override { to, .. }) => {
            let label =
                if resolution == Resolution::TakeBase { "Take the version below of" } else { "Drop the override of" };
            let mut tx = history.begin(format!("{label} {shown}"));
            let mut out = Out::new(&mut tx, project, index, layer);
            if !at.is_empty() && out.exists(&layer.disk(&at)) {
                out.remove(&layer.disk(&at))?;
            }
            match to {
                Some(to) if resolution == Resolution::TakeBase => {
                    changeset.ops.insert(uid.to_string(), Op::Move { to: to.clone() });
                }
                _ => {
                    changeset.ops.remove(uid);
                }
            }
            out.changeset(&changeset)?;
            out.done()?;
            tx.commit()
        }
        (Resolution::Drop, Op::Move { .. } | Op::Delete) => {
            let mut tx = history.begin(format!("Drop the operation on {shown}"));
            let mut out = Out::new(&mut tx, project, index, layer);
            changeset.ops.remove(uid);
            out.changeset(&changeset)?;
            out.done()?;
            tx.commit()
        }
        _ => bail!("{resolution:?} does not apply to {layer_name}'s {op:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{Resolution, delete, forget, override_entry, rename, resolve};
    use crate::Project;
    use crate::cache::Cache;
    use crate::changeset::Op;
    use crate::history::History;
    use crate::index::{Index, Problem};
    use crate::lock::Lock;
    use std::path::{Path, PathBuf};

    /// Two layers, `base` and `top`; `base` holds the bundle `a` in the folder `g`, and the plain file `n`.
    fn project(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("pe2-edit-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let write = |path: &str, text: &str| {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write("project.toml", "[[layer]]\nname = \"base\"\n\n[[layer]]\nname = \"top\"\n");
        write(".layers/base/g/a.node/node.toml", "stack = [{ op = \"t.image\" }]\n");
        write(".layers/base/g/a.node/native.texels", "texels");
        write(".layers/base/n", "notes");
        std::fs::create_dir_all(root.join(".layers/top")).unwrap();
        root
    }

    struct Open {
        project: Project,
        index: Index,
        history: History,
        _lock: Lock,
    }

    fn open(root: &Path) -> Open {
        let project = Project::load(root).unwrap();
        let lock = Lock::try_acquire(root, "a test").unwrap();
        let cache = Cache::open(&project, &lock).unwrap();
        let index = Index::open(&project, &cache).unwrap();
        let (history, _) = History::open(&cache).unwrap();
        Open { project, index, history, _lock: lock }
    }

    impl Open {
        fn refresh(&mut self, paths: &[PathBuf]) {
            self.index.refresh(&self.project, paths).unwrap();
        }

        fn uid(&self, path: &str) -> String {
            self.index.entries[path].uid.clone()
        }

        fn op(&self, path: &str) -> Option<Op> {
            self.index.changeset("top").unwrap().ops.get(&self.uid(path)).cloned()
        }
    }

    #[test]
    fn what_a_layer_holds_unlisted_is_adopted_or_removed() {
        let root = project("unlisted");
        let mut o = open(&root);
        // Put in the layer's folder by other means than the editor: no entry, a problem.
        let new = root.join(".layers/top/x.node/node.toml");
        std::fs::create_dir_all(new.parent().unwrap()).unwrap();
        std::fs::write(&new, "uid = \"uid://xxxxxxxxxxxx\"\nstack = []\n").unwrap();
        let junk = root.join(".layers/top/junk/a.bin");
        std::fs::create_dir_all(junk.parent().unwrap()).unwrap();
        std::fs::write(&junk, "junk").unwrap();
        // As a watcher reports them: the folders made, with what is in them.
        o.refresh(&[root.join(".layers/top/x.node"), root.join(".layers/top/junk")]);
        assert!(!o.index.assets.contains_key("x"));
        let unlisted = |o: &Open| -> Vec<String> {
            o.index
                .problems
                .iter()
                .filter_map(|p| match p {
                    Problem::Unlisted { path, .. } => Some(path.clone()),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(unlisted(&o), ["junk", "x.node"], "a tree is one problem");
        // Adopted: listed under the uid its declaration names.
        let paths = super::adopt(&o.project, &o.index, "top", "x.node", &mut o.history).unwrap();
        o.refresh(&paths);
        assert_eq!(o.index.assets["x"].uid.as_deref(), Some("uid://xxxxxxxxxxxx"));
        // Removed: gone from the folder, and undone, back.
        let paths = super::remove_unlisted(&o.project, &o.index, "top", "junk", &mut o.history).unwrap();
        o.refresh(&paths);
        assert!(!junk.exists() && unlisted(&o).is_empty(), "{:?}", o.index.problems);
        let paths = o.history.undo(&o.index, None, false).unwrap().paths();
        o.refresh(&paths);
        assert!(junk.exists());
        drop(o);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_override_whose_copy_is_deleted_outside_shows_what_is_below() {
        let root = project("copy-gone");
        let mut o = open(&root);
        let n = o.uid("n");
        let paths =
            super::write_file(&o.project, &o.index, "top", &n, "", b"mine", "Edit".into(), None, &mut o.history)
                .unwrap();
        o.refresh(&paths);
        assert_eq!(std::fs::read_to_string(&o.index.entries["n"].disk).unwrap(), "mine");
        // Its copy deleted by another program: the file is what is below again, and the override says it is missing.
        std::fs::remove_file(root.join(".layers/top/n")).unwrap();
        o.refresh(&[root.join(".layers/top/n")]);
        let entry = o.index.entries.get("n").expect("still there");
        assert_eq!(std::fs::read_to_string(&entry.disk).unwrap(), "notes");
        assert!(o.index.problems.iter().any(|p| matches!(p, Problem::Missing { .. })), "{:?}", o.index.problems);
        drop(o);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_node_and_its_folder_of_children_go_together() {
        let root = project("pair");
        let child = root.join(".layers/base/g/a/b.node/node.toml");
        std::fs::create_dir_all(child.parent().unwrap()).unwrap();
        std::fs::write(&child, "stack = []\n").unwrap();
        let mut o = open(&root);
        let a = o.uid("g/a.node");
        assert_eq!(super::paired(&o.index, "g/a.node").as_deref(), Some("g/a"));
        assert_eq!(super::paired(&o.index, "g/a").as_deref(), Some("g/a.node"));
        // Renamed, from below: both move, one step.
        let paths = rename(&o.project, &o.index, "top", &a, "g/c.node", &mut o.history).unwrap();
        o.refresh(&paths);
        assert!(
            o.index.assets.contains_key("g/c") && o.index.assets.contains_key("g/c/b"),
            "{:?}",
            o.index.assets.keys()
        );
        assert!(!o.index.entries.contains_key("g/a"));
        // Deleted: both go; undone, both are back.
        let c = o.uid("g/c.node");
        let paths = delete(&o.project, &o.index, "top", &c, &mut o.history).unwrap();
        o.refresh(&paths);
        assert!(!o.index.assets.contains_key("g/c") && !o.index.assets.contains_key("g/c/b"));
        let paths = o.history.undo(&o.index, None, false).unwrap().paths();
        o.refresh(&paths);
        assert!(o.index.assets.contains_key("g/c/b"));
        drop(o);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_override_holds_what_it_changes_and_merges_it_over_changes_below() {
        let root = project("merge");
        let mut o = open(&root);
        let uid = o.uid("g/a.node");
        let declared = |o: &Open| o.index.assets["g/a"].text().to_string();
        let stale = |o: &Open| o.index.problems.iter().any(|p| matches!(p, Problem::Stale { .. }));
        let below = root.join(".layers/base/g/a.node/node.toml");
        std::fs::write(&below, "stack = [{ op = \"t.image\" }]\nsize = 2\nname = \"a\"\n").unwrap();
        o.refresh(std::slice::from_ref(&below));
        let mine = "stack = [{ op = \"t.image\" }]\nsize = 3\nname = \"a\"\n";
        let write = |o: &mut Open, file: &str, bytes: &[u8]| {
            let paths =
                super::write_file(&o.project, &o.index, "top", &uid, file, bytes, "Edit".into(), None, &mut o.history)
                    .unwrap();
            o.refresh(&paths);
        };
        write(&mut o, "node.toml", mine.as_bytes());
        // It holds the file whole, with what it was made from; the texels are below's still.
        let held = root.join(".layers/top/g/a.node/node.toml");
        assert_eq!(std::fs::read_to_string(&held).unwrap(), mine);
        let Some(Op::Override { files, .. }) = o.op("g/a.node") else { panic!("no override") };
        assert_eq!(files.keys().collect::<Vec<_>>(), ["node.toml"]);
        assert!(files["node.toml"].was.is_some());
        assert!(!root.join(".layers/top/g/a.node/native.texels").exists());
        assert!(o.index.assets["g/a"].files.iter().any(|f| f.name == "native.texels" && f.layer == "base"));
        // Another field changed below reaches it, and it is not stale.
        std::fs::write(&below, "stack = [{ op = \"t.image\" }]\nsize = 2\nname = \"b\"\n").unwrap();
        o.refresh(std::slice::from_ref(&below));
        assert!(declared(&o).contains("name = \"b\"") && declared(&o).contains("size = 3"), "{}", declared(&o));
        assert!(!stale(&o), "{:?}", o.index.problems);
        // The same field changed below: mine is kept, and it is stale, saying where.
        std::fs::write(&below, "stack = [{ op = \"t.image\" }]\nsize = 5\nname = \"b\"\n").unwrap();
        o.refresh(std::slice::from_ref(&below));
        assert!(declared(&o).contains("size = 3") && stale(&o));
        let parts = o.index.stale_parts("top", &uid);
        let part = parts.iter().map(|p| (p.field.as_deref(), p.mine.as_deref(), p.was.as_deref(), p.now.as_deref()));
        assert_eq!(part.collect::<Vec<_>>(), [(Some("size"), Some("3"), Some("2"), Some("5"))]);
        // Kept: what it holds is the merge, made from what is below now.
        let paths = resolve(&o.project, &o.index, "top", &uid, Resolution::KeepMine, &mut o.history).unwrap();
        o.refresh(&paths);
        assert!(!stale(&o), "{:?}", o.index.problems);
        let kept = std::fs::read_to_string(&held).unwrap();
        assert!(kept.contains("size = 3") && kept.contains("name = \"b\""), "{kept}");
        // Text that is no TOML merges by line.
        let notes = root.join(".layers/base/g/a.node/notes.txt");
        std::fs::write(&notes, "one\ntwo\nthree\n").unwrap();
        o.refresh(std::slice::from_ref(&notes));
        write(&mut o, "notes.txt", b"ONE\ntwo\nthree\n");
        std::fs::write(&notes, "one\ntwo\nTHREE\n").unwrap();
        o.refresh(std::slice::from_ref(&notes));
        let shown = o.index.assets["g/a"].files.iter().find(|f| f.name == "notes.txt").unwrap().path.clone();
        assert_eq!(std::fs::read_to_string(shown).unwrap(), "ONE\ntwo\nTHREE\n");
        assert!(!stale(&o), "{:?}", o.index.problems);
        // Written as it is below, it holds nothing of it.
        write(&mut o, "notes.txt", b"one\ntwo\nTHREE\n");
        assert!(!root.join(".layers/top/g/a.node/notes.txt").exists());
        assert!(matches!(o.op("g/a.node"), Some(Op::Override { files, .. }) if !files.contains_key("notes.txt")));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn what_is_served_is_the_whole_stack_whatever_the_view_hides() {
        use crate::host::Shown;
        let root = project("served");
        let mut o = open(&root);
        let paths = super::create(
            &o.project,
            &o.index,
            "top",
            "uid://made1",
            "h/made",
            ("stack = [{ op = \"t.file\" }]\n", &[("extra.txt", b"hi".as_slice())]),
            &mut o.history,
        )
        .unwrap();
        o.refresh(&paths);
        let served = o.index.served();
        assert_eq!(served.get("g/a"), Some(&Shown::Node));
        assert_eq!(served.get("h/made"), Some(&Shown::Node));
        assert!(matches!(served.get("n"), Some(Shown::File { of_files: false, size: 5, .. })));
        assert!(matches!(served.get("h/made/extra.txt"), Some(Shown::File { of_files: true, size: 2, .. })));
        // The top layer hidden from the view: gone from its entries, served still.
        o.index.set_hidden(&std::collections::BTreeSet::from(["top".to_string()]));
        assert!(!o.index.entries.contains_key("h/made.node"));
        assert_eq!(o.index.served(), served);
    }

    #[test]
    fn an_edit_to_a_records_layer_refreshes_what_it_changed_alone() {
        let root = project("records-refresh");
        crate::records::mark(&root.join(".layers/top"), &crate::records::Settings::default()).unwrap();
        let mut o = open(&root);
        let declaration = "stack = [{ op = \"t.file\" }]\n";
        for (uid, path) in [("uid://one1", "h/one"), ("uid://two2", "h/two")] {
            let paths = super::create(
                &o.project,
                &o.index,
                "top",
                uid,
                path,
                (declaration, &[("extra.txt", b"hi".as_slice())]),
                &mut o.history,
            )
            .unwrap();
            o.refresh(&paths);
        }
        // One file of one: that asset alone, the stack standing.
        let paths = super::write_file(
            &o.project,
            &o.index,
            "top",
            "uid://one1",
            "extra.txt",
            b"again",
            "Edit".into(),
            None,
            &mut o.history,
        )
        .unwrap();
        let refreshed = o.index.refresh(&o.project, &paths).unwrap();
        assert_eq!(refreshed.assets.iter().map(String::as_str).collect::<Vec<_>>(), ["h/one"]);
        assert!(!refreshed.restacked, "a content changed applies the stack again");
        let extra = o.index.assets["h/one"].files.iter().find(|f| f.name == "extra.txt").unwrap().path.clone();
        assert_eq!(crate::content::read(&extra).unwrap(), b"again");
        // Renamed: the stack applied again, both places taken in.
        let paths = super::rename(&o.project, &o.index, "top", "uid://one1", "h/won", &mut o.history).unwrap();
        let refreshed = o.index.refresh(&o.project, &paths).unwrap();
        assert!(refreshed.restacked);
        assert!(o.index.assets.contains_key("h/won") && !o.index.assets.contains_key("h/one"));
        assert!(o.index.assets.contains_key("h/two"));
    }

    #[test]
    fn a_records_layer_is_edited_as_a_tree_is() {
        let root = project("records");
        crate::records::mark(&root.join(".layers/top"), &crate::records::Settings::default()).unwrap();
        let mut o = open(&root);
        let only_records = |root: &Path| {
            let held: Vec<String> = std::fs::read_dir(root.join(".layers/top"))
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            assert_eq!(held, [".records"], "a records layer holds no tree");
        };
        // A node made: its declaration and a file, read as any layer's.
        let declaration = "stack = [{ op = \"t.file\" }]\n";
        let paths = super::create(
            &o.project,
            &o.index,
            "top",
            "uid://made1",
            "h/made",
            (declaration, &[("extra.txt", b"hi".as_slice())]),
            &mut o.history,
        )
        .unwrap();
        o.refresh(&paths);
        let made = &o.index.assets["h/made"];
        assert_eq!((made.layers.as_slice(), made.text()), (["top".to_string()].as_slice(), declaration));
        let extra = made.files.iter().find(|f| f.name == "extra.txt").unwrap();
        assert_eq!(crate::content::read(&extra.path).unwrap(), b"hi");
        assert!(o.index.entries.contains_key("h"), "the folder it is in, made with it");
        // One of its files written again: its own record.
        let paths = super::write_file(
            &o.project,
            &o.index,
            "top",
            "uid://made1",
            "extra.txt",
            b"again",
            "Edit".into(),
            None,
            &mut o.history,
        )
        .unwrap();
        o.refresh(&paths);
        let extra = o.index.assets["h/made"].files.iter().find(|f| f.name == "extra.txt").unwrap().path.clone();
        assert_eq!(crate::content::read(&extra).unwrap(), b"again");
        only_records(&root);
        // An override of a node below: what it writes held whole, merged over a change below.
        let a = o.uid("g/a.node");
        let mine = "stack = [{ op = \"t.image\" }]\nnote = \"x\"\n";
        let paths = super::write_file(
            &o.project,
            &o.index,
            "top",
            &a,
            "node.toml",
            mine.as_bytes(),
            "Edit".into(),
            None,
            &mut o.history,
        )
        .unwrap();
        o.refresh(&paths);
        assert_eq!(o.index.assets["g/a"].text(), mine);
        let below = root.join(".layers/base/g/a.node/node.toml");
        std::fs::write(&below, "stack = [{ op = \"t.image\" }]\nsize = 2\n").unwrap();
        o.refresh(std::slice::from_ref(&below));
        let text = o.index.assets["g/a"].text().to_string();
        assert!(text.contains("note = \"x\"") && text.contains("size = 2"), "{text}");
        only_records(&root);
        // Renamed: one record changes, what is inside follows.
        let folder = o.uid("h");
        let records_before = crate::records::Set::records_of(&root.join(".layers/top")).unwrap();
        let paths = rename(&o.project, &o.index, "top", &folder, "k", &mut o.history).unwrap();
        o.refresh(&paths);
        assert!(o.index.assets.contains_key("k/made") && !o.index.assets.contains_key("h/made"));
        let records_after = crate::records::Set::records_of(&root.join(".layers/top")).unwrap();
        let changed = records_after.iter().filter(|(uid, r)| records_before.get(*uid) != Some(*r)).count();
        assert_eq!(changed, 1, "a rename is one record");
        // Deleted, its own and one below.
        let paths = delete(&o.project, &o.index, "top", "uid://made1", &mut o.history).unwrap();
        o.refresh(&paths);
        assert!(!o.index.assets.contains_key("k/made"));
        let n = o.uid("n");
        let paths = delete(&o.project, &o.index, "top", &n, &mut o.history).unwrap();
        o.refresh(&paths);
        assert!(!o.index.entries.contains_key("n"));
        // All undone: nothing of it left.
        while let Ok(step) = o.history.undo(&o.index, None, false) {
            let paths = step.paths();
            o.refresh(&paths);
        }
        assert!(crate::records::Set::records_of(&root.join(".layers/top")).unwrap().is_empty());
        assert_eq!(o.index.assets["g/a"].layers, ["base"]);
        assert!(o.index.entries.contains_key("n") && !o.index.entries.contains_key("h"));
        assert!(o.index.problems.is_empty(), "{:?}", o.index.problems);
        drop(o);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn overrides_renames_and_deletes() {
        let root = project("ops");
        let mut o = open(&root);
        let a = o.uid("g/a.node");
        // Override: the top's version wins, holding nothing yet, its files those below.
        let paths = override_entry(&o.project, &o.index, "top", &a, &mut o.history).unwrap();
        o.refresh(&paths);
        assert_eq!(o.index.assets["g/a"].layers, ["base", "top"]);
        assert!(!root.join(".layers/top/g/a.node").exists());
        assert!(!root.join(".layers/top/g/a.node/native.texels").exists());
        assert!(o.index.assets["g/a"].files.iter().any(|f| f.name == "native.texels" && f.layer == "base"));
        assert!(o.index.problems.is_empty(), "{:?}", o.index.problems);
        // Renamed: the move goes with the override, its copy moved too.
        let paths = rename(&o.project, &o.index, "top", &a, "g/b.node", &mut o.history).unwrap();
        o.refresh(&paths);
        assert!(o.index.assets.contains_key("g/b") && !o.index.assets.contains_key("g/a"));
        assert!(matches!(o.op("g/b.node"), Some(Op::Override { to: Some(_), .. })));
        // Renamed back: no move left.
        let paths = rename(&o.project, &o.index, "top", &a, "g/a.node", &mut o.history).unwrap();
        o.refresh(&paths);
        assert!(matches!(o.op("g/a.node"), Some(Op::Override { to: None, .. })));
        // Deleted from below: its copy goes, a delete is recorded.
        let n = o.uid("n");
        let paths = delete(&o.project, &o.index, "top", &a, &mut o.history).unwrap();
        o.refresh(&paths);
        assert!(!o.index.assets.contains_key("g/a"));
        assert!(!root.join(".layers/top/g/a.node").exists());
        // Everything undone: the top as it began.
        for _ in 0..4 {
            let step = o.history.undo(&o.index, None, false).unwrap();
            let paths = step.paths();
            o.refresh(&paths);
        }
        assert!(o.index.changeset("top").unwrap().ops.is_empty());
        assert!(!root.join(".layers/top/g").exists());
        assert_eq!(o.index.assets["g/a"].layers, ["base"]);
        assert_eq!(o.uid("n"), n);
        drop(o);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn writes_a_file_in_its_own_copy() {
        let root = project("write");
        let mut o = open(&root);
        let a = o.uid("g/a.node");
        let text = b"stack = [{ op = \"t.image\" }]\nnote = \"x\"\n";
        let paths = super::write_file(
            &o.project,
            &o.index,
            "top",
            &a,
            "node.toml",
            text,
            "Edit a".into(),
            Some("a".into()),
            &mut o.history,
        )
        .unwrap();
        o.refresh(&paths);
        assert_eq!(o.index.assets["g/a"].layers, ["base", "top"]);
        assert_eq!(o.index.assets["g/a"].fields().get("note").and_then(pe2_value::Value::as_str), Some("x"));
        // A continued edit joins the step; undone, the copy goes with it.
        let text = b"stack = [{ op = \"t.image\" }]\nnote = \"xy\"\n";
        let paths = super::write_file(
            &o.project,
            &o.index,
            "top",
            &a,
            "node.toml",
            text,
            "Edit a".into(),
            Some("a".into()),
            &mut o.history,
        )
        .unwrap();
        o.refresh(&paths);
        assert_eq!(o.history.done().len(), 1);
        let paths = o.history.undo(&o.index, None, false).unwrap().paths();
        o.refresh(&paths);
        assert_eq!(o.index.assets["g/a"].layers, ["base"]);
        assert!(!root.join(".layers/top/g").exists());
        drop(o);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn resolves_a_stale_override() {
        let root = project("stale");
        let mut o = open(&root);
        let a = o.uid("g/a.node");
        let paths = override_entry(&o.project, &o.index, "top", &a, &mut o.history).unwrap();
        o.refresh(&paths);
        // A file it does not hold changes below: that reaches it.
        let texels = root.join(".layers/base/g/a.node/native.texels");
        std::fs::write(&texels, "retouched").unwrap();
        o.refresh(std::slice::from_ref(&texels));
        assert!(o.index.problems.is_empty(), "{:?}", o.index.problems);
        // One it holds changes below: stale.
        let paths = super::write_file(
            &o.project,
            &o.index,
            "top",
            &a,
            "native.texels",
            b"mine",
            "Edit".into(),
            None,
            &mut o.history,
        )
        .unwrap();
        o.refresh(&paths);
        assert!(o.index.problems.is_empty(), "{:?}", o.index.problems);
        std::fs::write(&texels, "retouched again").unwrap();
        o.refresh(&[texels]);
        assert_eq!(o.index.problems, [Problem::Stale { layer: "top".into(), path: "g/a.node".into() }]);
        let paths = resolve(&o.project, &o.index, "top", &a, Resolution::KeepMine, &mut o.history).unwrap();
        o.refresh(&paths);
        assert!(o.index.problems.is_empty(), "{:?}", o.index.problems);
        let paths = resolve(&o.project, &o.index, "top", &a, Resolution::TakeBase, &mut o.history).unwrap();
        o.refresh(&paths);
        assert_eq!(o.index.assets["g/a"].layers, ["base"]);
        assert!(o.op("g/a.node").is_none());
        drop(o);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_read_only_mount_forgets_what_is_gone_from_it() {
        let root = project("forget");
        let mounted = root.join("mounted");
        std::fs::create_dir_all(&mounted).unwrap();
        std::fs::write(mounted.join("kept.c"), "kept").unwrap();
        std::fs::write(mounted.join("gone.c"), "gone").unwrap();
        std::fs::write(
            root.join("project.toml"),
            "[[mount]]\nat = \"code\"\npath = \"mounted\"\nread_only = true\n\n[[layer]]\nname = \"base\"\n\n[[layer]]\nname = \"top\"\n",
        )
        .unwrap();
        let mut o = open(&root);
        let (gone, kept) = (o.uid("code/gone.c"), o.uid("code/kept.c"));
        std::fs::remove_file(mounted.join("gone.c")).unwrap();
        o.refresh(&[mounted.join("gone.c")]);
        assert!(
            o.index.problems.contains(&Problem::Missing {
                layer: "code".into(),
                path: "code/gone.c".into(),
                left_out: false
            }),
            "{:?}",
            o.index.problems
        );
        assert!(forget(&o.project, &o.index, "code", &kept, &mut o.history).is_err(), "what is there is not forgotten");
        let paths = forget(&o.project, &o.index, "code", &gone, &mut o.history).unwrap();
        o.refresh(&paths);
        assert!(!o.index.problems.iter().any(|p| matches!(p, Problem::Missing { .. })), "{:?}", o.index.problems);
        assert!(mounted.join("kept.c").exists());
        drop(o);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn what_a_layer_lists_of_an_extension_is_forgotten() {
        let root = project("forget-extension");
        let held = root.join(".layers/top/extensions/a.extension");
        std::fs::create_dir_all(&held).unwrap();
        std::fs::write(held.join("extension.toml"), "id = \"a\"\n").unwrap();
        let mut o = open(&root);
        // As an older editor listed it, before an extension's folder was left out.
        let listed = ".layers/top.toml";
        let text = std::fs::read_to_string(root.join(listed)).unwrap_or_default();
        let entry = "\"uid://1an72qda531m5\" = \"extensions/a.extension/extension.toml\"\n";
        let text = if text.contains("[entries]") {
            text.replace("[entries]\n", &format!("[entries]\n{entry}"))
        } else {
            format!("{text}\n[entries]\n{entry}")
        };
        std::fs::write(root.join(listed), text).unwrap();
        o.refresh(&[root.join(listed)]);
        let path = "extensions/a.extension/extension.toml";
        let problem = Problem::Missing { layer: "top".into(), path: path.into(), left_out: true };
        assert!(o.index.problems.contains(&problem), "{:?}", o.index.problems);
        assert!(problem.describe().contains("leaves it out"));
        let paths = forget(&o.project, &o.index, "top", "uid://1an72qda531m5", &mut o.history).unwrap();
        o.refresh(&paths);
        assert!(!o.index.problems.iter().any(|p| matches!(p, Problem::Missing { .. })), "{:?}", o.index.problems);
        assert!(held.join("extension.toml").exists(), "only the list is written");
        drop(o);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn deletes_what_the_layer_makes() {
        let root = project("own");
        std::fs::create_dir_all(root.join(".layers/top/mine/x.node")).unwrap();
        std::fs::write(root.join(".layers/top/mine/x.node/node.toml"), "stack = [{ op = \"t.file\" }]\n").unwrap();
        let mut o = open(&root);
        let mine = o.uid("mine");
        let paths = rename(&o.project, &o.index, "top", &mine, "ours", &mut o.history).unwrap();
        o.refresh(&paths);
        assert!(o.index.assets.contains_key("ours/x"));
        let paths = delete(&o.project, &o.index, "top", &mine, &mut o.history).unwrap();
        o.refresh(&paths);
        // A create then a delete: nothing recorded, nothing left.
        assert!(o.index.changeset("top").unwrap().entries.is_empty());
        assert!(!root.join(".layers/top/ours").exists());
        drop(o);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
