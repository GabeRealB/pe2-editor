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
use std::collections::HashSet;
use std::path::PathBuf;

use crate::changeset::{Changeset, Op};
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

/// Copy an entry's content from below into the layer at `path`.
fn copy_in(
    tx: &mut crate::history::Transaction<'_>,
    layer: &Layer,
    path: &str,
    files: &[(String, PathBuf)],
) -> Result<()> {
    for (file, source) in files {
        let bytes = std::fs::read(source).with_context(|| source.display().to_string())?;
        let disk = if file.is_empty() { layer.disk(path) } else { layer.disk(path).join(file) };
        tx.write(&disk, &bytes)?;
    }
    Ok(())
}

fn save(tx: &mut crate::history::Transaction<'_>, layer: &Layer, changeset: &Changeset) -> Result<()> {
    tx.write(&layer.changeset, changeset.render().as_bytes())
}

/// Make the layer's own copy of an entry from below, recording the override: where the copy is.
fn own_copy(
    tx: &mut crate::history::Transaction<'_>,
    index: &Index,
    layer: &Layer,
    layer_name: &str,
    changeset: &mut Changeset,
    uid: &str,
) -> Result<String> {
    own(tx, index, layer, layer_name, changeset, uid, |_| true)
}

/// A layer's own version of an entry, made of those of its files `keep` takes.
fn own(
    tx: &mut crate::history::Transaction<'_>,
    index: &Index,
    layer: &Layer,
    layer_name: &str,
    changeset: &mut Changeset,
    uid: &str,
    keep: impl Fn(&str) -> bool,
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
    let base = below.hash.clone().with_context(|| format!("{}: no content to override", below.path))?;
    let files: Vec<(String, PathBuf)> = below.files.iter().filter(|(file, _)| keep(file)).cloned().collect();
    copy_in(tx, layer, &at, &files)?;
    changeset.ops.insert(uid.to_string(), Op::Override { base, to });
    save(tx, layer, changeset)?;
    Ok(at)
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
    let path = match (changeset.entries.get(uid).cloned(), changeset.ops.get(uid).cloned()) {
        (Some(path), _) => path,
        (None, Some(op @ Op::Override { .. })) => {
            let below = index.below(layer_name, uid).with_context(|| format!("{uid} is gone below {layer_name}"))?;
            target(Some(&op), &below.path)
        }
        (None, Some(Op::Delete)) => bail!("{layer_name} deletes {uid}"),
        (None, _) => own_copy(&mut tx, index, layer, layer_name, &mut changeset, uid)?,
    };
    let disk = if file.is_empty() { layer.disk(&path) } else { layer.disk(&path).join(file) };
    tx.write(&disk, bytes)?;
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
    // A folder it is the first thing in: of this step, or the index lists it and the layer's list changes under the history.
    let mut taken = HashSet::new();
    for (at, _) in path.match_indices('/') {
        let folder = &path[..at];
        if index.entries.contains_key(folder) || changeset.entries.values().any(|known| known == folder) {
            continue;
        }
        let id = crate::uid::new(|id| taken.contains(&id) || index.by_uid.contains_key(&crate::uid::text(id)))?;
        taken.insert(id);
        tx.make_dir(&layer.disk(folder))?;
        changeset.entries.insert(crate::uid::text(id), folder.to_string());
    }
    tx.write(&layer.disk(&bundle).join(crate::index::NODE_FILE), declaration.as_bytes())?;
    for (name, bytes) in files {
        tx.write(&layer.disk(&bundle).join(name), bytes)?;
    }
    changeset.entries.insert(uid.to_string(), bundle);
    save(&mut tx, layer, &changeset)?;
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
    own_copy(&mut tx, index, layer, layer_name, &mut changeset, uid)?;
    tx.commit()
}

/// Override an asset from below with its declaration alone, not its files: for a stack that reads what it needs of the
/// version it replaces (the graph's `below`), so a large file stays the layer's below. Returns the paths touched.
///
/// # Errors
/// As `override_entry`.
pub fn override_declaration(
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
    own(&mut tx, index, layer, layer_name, &mut changeset, uid, |file| file == crate::index::NODE_FILE)?;
    tx.commit()
}

/// Delete an entry from a layer's view: one the layer makes leaves it; one from below gets a delete.
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
    if let Some(path) = changeset.entries.get(uid).cloned() {
        let mut tx = history.begin(format!("Delete {}", name(&path)));
        if layer.disk(&path).exists() {
            tx.remove(&layer.disk(&path))?;
        }
        let inside = format!("{path}/");
        changeset.entries.retain(|_, p| *p != path && !p.starts_with(&inside));
        save(&mut tx, layer, &changeset)?;
        return tx.commit();
    }
    let below = index.below(layer_name, uid).with_context(|| format!("no layer below {layer_name} has {uid}"))?;
    let at = target(changeset.ops.get(uid), &below.path);
    let mut tx = history.begin(format!("Delete {}", name(&at)));
    if matches!(changeset.ops.get(uid), Some(Op::Override { .. })) && layer.disk(&at).exists() {
        tx.remove(&layer.disk(&at))?;
    }
    if below.kind == Kind::Folder {
        // A folder deleted subsumes what the layer did inside it.
        for (inner, path) in index.below_inside(layer_name, &below.path) {
            if let Some(op) = changeset.ops.remove(&inner)
                && matches!(op, Op::Override { .. })
            {
                let copy = layer.disk(&target(Some(&op), &path));
                if copy.exists() {
                    tx.remove(&copy)?;
                }
            }
        }
    }
    changeset.ops.insert(uid.to_string(), Op::Delete);
    save(&mut tx, layer, &changeset)?;
    tx.commit()
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
        index.leaves_out(layer_name, &path) || !layer.disk(&path).exists(),
        "{layer_name} still holds {path}"
    );
    let mut tx = history.begin(format!("Forget {}", name(&path)));
    let inside = format!("{path}/");
    changeset.entries.retain(|_, p| *p != path && !p.starts_with(&inside));
    save(&mut tx, layer, &changeset)?;
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

/// Rename or move an entry to another resolved path: one the layer makes moves on disk; one from below gets a move.
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
    if index.entries.contains_key(to) {
        bail!("{to}: an entry is there already");
    }
    if let Some(from) = changeset.entries.get(uid).cloned() {
        let mut tx = history.begin(rename_label(&from, to));
        tx.rename(&layer.disk(&from), &layer.disk(to))?;
        let inside = format!("{from}/");
        for path in changeset.entries.values_mut() {
            if *path == from {
                *path = to.to_string();
            } else if let Some(rest) = path.strip_prefix(&inside) {
                *path = format!("{to}/{rest}");
            }
        }
        save(&mut tx, layer, &changeset)?;
        return tx.commit();
    }
    let below = index.below(layer_name, uid).with_context(|| format!("no layer below {layer_name} has {uid}"))?;
    let op = changeset.ops.get(uid).cloned();
    if matches!(op, Some(Op::Delete)) {
        bail!("{layer_name} deletes {}", below.path);
    }
    let from = target(op.as_ref(), &below.path);
    let mut tx = history.begin(rename_label(&from, to));
    let back = (to != below.path).then(|| to.to_string());
    let new = match op {
        Some(Op::Override { base, .. }) => {
            tx.rename(&layer.disk(&from), &layer.disk(to))?;
            Some(Op::Override { base, to: back })
        }
        _ => back.map(|to| Op::Move { to }),
    };
    match new {
        Some(op) => changeset.ops.insert(uid.to_string(), op),
        // Moved back where it is below: no move left.
        None => changeset.ops.remove(uid),
    };
    save(&mut tx, layer, &changeset)?;
    tx.commit()
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
        (Resolution::KeepMine, Op::Override { to, .. }) => {
            let below = below.with_context(|| format!("{uid} is gone below {layer_name}"))?;
            let base = below.hash.with_context(|| format!("{}: no content below", below.path))?;
            let mut tx = history.begin(format!("Keep my version of {shown}"));
            changeset.ops.insert(uid.to_string(), Op::Override { base, to: to.clone() });
            save(&mut tx, layer, &changeset)?;
            tx.commit()
        }
        (Resolution::TakeBase | Resolution::Drop, Op::Override { to, .. }) => {
            let label =
                if resolution == Resolution::TakeBase { "Take the version below of" } else { "Drop the override of" };
            let mut tx = history.begin(format!("{label} {shown}"));
            if !at.is_empty() && layer.disk(&at).exists() {
                tx.remove(&layer.disk(&at))?;
            }
            match to {
                Some(to) if resolution == Resolution::TakeBase => {
                    changeset.ops.insert(uid.to_string(), Op::Move { to: to.clone() });
                }
                _ => {
                    changeset.ops.remove(uid);
                }
            }
            save(&mut tx, layer, &changeset)?;
            tx.commit()
        }
        (Resolution::Drop, Op::Move { .. } | Op::Delete) => {
            let mut tx = history.begin(format!("Drop the operation on {shown}"));
            changeset.ops.remove(uid);
            save(&mut tx, layer, &changeset)?;
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
    fn overrides_renames_and_deletes() {
        let root = project("ops");
        let mut o = open(&root);
        let a = o.uid("g/a.node");
        // Override: the top's own copy wins.
        let paths = override_entry(&o.project, &o.index, "top", &a, &mut o.history).unwrap();
        o.refresh(&paths);
        assert_eq!(o.index.assets["g/a"].layers, ["base", "top"]);
        assert!(root.join(".layers/top/g/a.node/native.texels").exists());
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
            let step = o.history.undo(false).unwrap();
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
        assert_eq!(o.index.assets["g/a"].fields().get("note").and_then(toml::Value::as_str), Some("x"));
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
        let paths = o.history.undo(false).unwrap().paths();
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
        // The base changes under the override: stale.
        let texels = root.join(".layers/base/g/a.node/native.texels");
        std::fs::write(&texels, "retouched").unwrap();
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
