//! Edits to the stack in `project.toml`: the layers' order, a layer added, a layer removed. Written in place, keeping
//! the file's comments and layout, and checked first against what the layers need of each other (`Index::layers`): a
//! layer stays above every layer it acts on or references. A layer removed leaves its files on disk. Each edit is a
//! transaction of the project's history (`history.rs`), so it can be undone.
//!
//! The mounts (`[[mount]]`) are files layers at the bottom of the stack, apart until mounts move into the stack: they
//! are neither reordered nor removed here.

use anyhow::{Context, Result, bail};
use std::path::Path;
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, value};

use crate::history::History;
use crate::index::LayerInfo;
use crate::{FILE, LAYERS, LayerKind, Project};

pub(crate) fn read(root: &Path) -> Result<DocumentMut> {
    let path = root.join(FILE);
    let text = std::fs::read_to_string(&path).with_context(|| format!("{}", path.display()))?;
    text.parse().with_context(|| format!("{}", path.display()))
}

pub(crate) fn write(root: &Path, document: &DocumentMut, history: &mut History, label: String) -> Result<()> {
    let mut tx = history.begin(label);
    tx.write(&root.join(FILE), document.to_string().as_bytes())?;
    tx.commit().map(drop)
}

fn layer_tables(document: &mut DocumentMut) -> Result<&mut ArrayOfTables> {
    let item = document.entry("layer").or_insert(Item::ArrayOfTables(ArrayOfTables::new()));
    item.as_array_of_tables_mut().context("project.toml: `layer` is not a list of tables")
}

fn name_of(table: &Table) -> Option<&str> {
    table.get("name").and_then(Item::as_str)
}

/// Why `order` (the stack's own layers, bottom to top) breaks what a layer needs; none when it keeps it.
#[must_use]
pub fn order_breaks(layers: &[LayerInfo], order: &[String]) -> Option<String> {
    let position = |name: &str| order.iter().position(|n| n == name);
    for layer in layers {
        let Some(at) = position(&layer.name) else { continue };
        for dependency in &layer.depends_on {
            if position(&dependency.layer).is_some_and(|below| below > at) {
                return Some(format!("{}, so it stays above it", dependency.describe(&layer.name)));
            }
        }
    }
    None
}

/// Put the stack's own layers in `order` (bottom to top; every one of them, once).
///
/// # Errors
/// The order breaks what a layer needs, does not name each layer once, or `project.toml` cannot be rewritten.
pub fn reorder(project: &Project, layers: &[LayerInfo], order: &[String], history: &mut History) -> Result<()> {
    if let Some(why) = order_breaks(layers, order) {
        bail!("{why}");
    }
    let mut document = read(&project.root)?;
    let tables = layer_tables(&mut document)?;
    let mut current: Vec<Table> = tables.iter().cloned().collect();
    // Tables are written where their positions say: the new order takes the old positions, in turn.
    let positions: Vec<Option<usize>> = current.iter().map(Table::position).collect();
    let mut sorted = Vec::new();
    for name in order {
        let at = current
            .iter()
            .position(|t| name_of(t) == Some(name.as_str()))
            .with_context(|| format!("the stack has no layer {name}"))?;
        sorted.push(current.remove(at));
    }
    if let Some(left) = current.first() {
        bail!("the order leaves out the layer {}", name_of(left).unwrap_or("?"));
    }
    tables.clear();
    for (mut table, position) in sorted.into_iter().zip(positions) {
        if let Some(position) = position {
            table.set_position(position);
        }
        tables.push(table);
    }
    write(&project.root, &document, history, "Reorder the layers".into())
}

/// A name a new layer may take: letters, digits, `_` and `-`, not a layer's or a mount point's already.
///
/// # Errors
/// It may not.
pub fn check_name(project: &Project, name: &str) -> Result<()> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        bail!("a layer's name is letters, digits, `_` and `-`");
    }
    if project.layers.iter().any(|l| l.name == name) {
        bail!("the project has a layer {name} already");
    }
    Ok(())
}

/// Add a new, empty layer of a kind at the top of the stack: `.layers/<name>/` and its entry in `project.toml`.
///
/// # Errors
/// The name is not one a layer may take, or the files cannot be written.
pub fn add(project: &Project, name: &str, kind: LayerKind, history: &mut History) -> Result<()> {
    check_name(project, name)?;
    let mut document = read(&project.root)?;
    let mut table = Table::new();
    table.insert("name", value(name));
    table.insert("path", value(format!("{LAYERS}/{name}")));
    if kind == LayerKind::Files {
        table.insert("kind", value("files"));
    }
    layer_tables(&mut document)?.push(table);
    let mut tx = history.begin(format!("Add the layer {name}"));
    tx.make_dir(&project.root.join(LAYERS).join(name))?;
    tx.write(&project.root.join(FILE), document.to_string().as_bytes())?;
    tx.commit().map(drop)
}

/// Take a layer out of the stack; its directory and changeset stay on disk.
///
/// # Errors
/// It is a mount or the game layer, another layer needs it, or `project.toml` cannot be rewritten.
pub fn remove(project: &Project, layers: &[LayerInfo], name: &str, history: &mut History) -> Result<()> {
    if project.game.as_ref().is_some_and(|g| g.layer == name) {
        bail!("{name} is the game layer, which extraction fills");
    }
    if layers.iter().any(|l| l.name == name && l.mounted) {
        bail!("{name} is a mount, declared apart in project.toml ([[mount]])");
    }
    if let Some((layer, dependency)) =
        layers.iter().find_map(|l| Some((l, l.depends_on.iter().find(|d| d.layer == name)?)))
    {
        bail!("{}, so it cannot be taken out", dependency.describe(&layer.name));
    }
    let mut document = read(&project.root)?;
    let tables = layer_tables(&mut document)?;
    let at = tables
        .iter()
        .position(|t| name_of(t) == Some(name))
        .with_context(|| format!("the stack has no layer {name}"))?;
    tables.remove(at);
    write(&project.root, &document, history, format!("Take the layer {name} out of the stack"))
}

#[cfg(test)]
mod tests {
    use crate::Project;
    use crate::cache::Cache;
    use crate::history::History;
    use crate::index::{Dependency, LayerInfo};
    use crate::lock::Lock;
    use crate::{LayerKind, stack};

    fn info(name: &str, below: &[&str]) -> LayerInfo {
        LayerInfo {
            name: name.into(),
            kind: LayerKind::Assets,
            at: String::new(),
            read_only: false,
            writable: true,
            mounted: false,
            entries: 0,
            overrides: 0,
            moves: 0,
            deletes: 0,
            problems: 0,
            depends_on: below.iter().map(|l| Dependency { layer: (*l).into(), operations: 2, references: 0 }).collect(),
        }
    }

    #[test]
    fn edits_the_stack_in_place() {
        let root = std::env::temp_dir().join(format!("pe2-stack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let text = "# the stack\n[game]\nlayer = \"game\"\ndeclarations = \"res://d\"\n\n[[layer]]\nname = \"game\"\n\
                    \n# mine\n[[layer]]\nname = \"mod\"\n";
        std::fs::write(root.join("project.toml"), text).unwrap();
        let project = Project::load(&root).unwrap();
        let lock = Lock::try_acquire(&root, "a test").unwrap();
        let (mut history, _) = History::open(&Cache::open(&project, &lock).unwrap()).unwrap();
        let history = &mut history;
        let layers = [info("game", &[]), info("mod", &["game"])];
        let order = ["mod".to_string(), "game".to_string()];
        let why = stack::order_breaks(&layers, &order).unwrap();
        assert_eq!(why, "mod acts on 2 entries of game, so it stays above it");
        assert!(stack::reorder(&project, &layers, &order, history).is_err());
        stack::add(&project, "fixes", LayerKind::Assets, history).unwrap();
        let project = Project::load(&root).unwrap();
        assert!(stack::add(&project, "fixes", LayerKind::Assets, history).is_err());
        stack::add(&project, "patch", LayerKind::Files, history).unwrap();
        let read = Project::load(&root).unwrap();
        assert_eq!(read.layer("patch").unwrap().kind, LayerKind::Files);
        history.undo(false).unwrap();
        let project = Project::load(&root).unwrap();
        let layers = [info("game", &[]), info("mod", &["game"]), info("fixes", &[])];
        let order: Vec<String> = ["game", "fixes", "mod"].map(String::from).into();
        stack::reorder(&project, &layers, &order, history).unwrap();
        let project = Project::load(&root).unwrap();
        assert_eq!(project.layers.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), ["game", "fixes", "mod"]);
        assert!(root.join(".layers/fixes").is_dir());
        // Comments travel with their tables.
        let text = std::fs::read_to_string(root.join("project.toml")).unwrap();
        assert!(text.starts_with("# the stack\n") && text.contains("# mine\n[[layer]]\nname = \"mod\""), "{text}");
        assert!(stack::remove(&project, &layers, "game", history).is_err());
        stack::remove(&project, &layers, "fixes", history).unwrap();
        assert_eq!(Project::load(&root).unwrap().layers.len(), 2);
        assert!(root.join(".layers/fixes").is_dir());
        // Undone: taken out, reordered, added - back to the two layers, and the new layer's folder gone.
        for _ in 0..3 {
            history.undo(false).unwrap();
        }
        assert_eq!(Project::load(&root).unwrap().layers.len(), 2);
        assert!(!root.join(".layers/fixes").exists());
        drop(lock);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
