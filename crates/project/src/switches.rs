//! What a project says of its extensions in `project.toml`: those it leaves off (`[extensions] disabled`, a list of
//! ids) and their project settings (`[extensions."<id>"]`). Each is written in place (the file's comments and layout
//! kept) as a step of the project's history, so it can be undone. The editor opens the project again when
//! `project.toml` changes, loading its extensions as it now says.
//!
//! The extensions it holds are sealed nodes, kept whole as trees in its store (`records::Record::tree`) and found by
//! their manifests wherever they are: one is added from wherever it is kept - and updated the same way - and removed,
//! each as one step with what the project requires of it (`[[extension]]`).

use pe2_path::Name;
use sfs::HostPath;

use anyhow::{Context, Result, bail};
use toml_edit::{Array, ArrayOfTables, DocumentMut, Item, Table, value};

use crate::ext::component::About;
use crate::ext::{EXTENSION_COMPONENT, folder};
use crate::history::History;
use crate::stack::{read, write};
use crate::{FILE, Project, Requirement};

/// Leave these extensions off, and every other on.
///
/// # Errors
/// `project.toml` cannot be read or written.
pub fn set_disabled(project: &Project, disabled: &[String], history: &mut History, label: String) -> Result<()> {
    let mut document = read(project.io.filesystem(), &project.root)?;
    let mut ids: Vec<&str> = disabled.iter().map(String::as_str).collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        if let Some(table) = document.get_mut("extensions").and_then(Item::as_table_mut) {
            table.remove("disabled");
            if table.is_empty() {
                document.remove("extensions");
            }
        }
    } else {
        let table = document.entry("extensions").or_insert(Item::Table(Table::new()));
        if let Some(table) = table.as_table_mut() {
            table.insert("disabled", value(ids.into_iter().collect::<Array>()));
        }
    }
    write(&project.root, &document, history, label)
}

/// The id an extension's tree says it is, by its manifest; none where it holds none, or one not read.
fn id_in(project: &Project, tree: &crate::chunks::Tree) -> Option<String> {
    let manifest = tree.files.get(folder::MANIFEST.as_str())?;
    let bytes = crate::chunks::Store::at(&project.root, project.io.clone()).get_object(&manifest.sha1).ok()?;
    let about: About = toml::from_str(std::str::from_utf8(&bytes).ok()?).ok()?;
    Some(about.id)
}

/// What the layers record of the extension of an id, bottom to top: each record of a sealed node whose tree is that
/// extension, and each deleting one of those.
fn records_of(project: &Project, set: &crate::records::Set, id: &str) -> Vec<crate::records::SealedRecord> {
    let all = set.sealed_records();
    let uids: std::collections::HashSet<pe2_path::Uid> = all
        .iter()
        .filter(|held| held.tree.as_ref().is_some_and(|tree| id_in(project, tree).as_deref() == Some(id)))
        .map(|held| held.uid)
        .collect();
    all.into_iter().filter(|held| uids.contains(&held.uid)).collect()
}

/// The layer whose records' folder it is.
fn layer_of<'a>(project: &'a Project, folder: &HostPath) -> Option<&'a crate::Layer> {
    project.layers.iter().find(|layer| layer.records == *folder)
}

/// The project's record set, as it is open, or read now.
fn set_of(project: &Project) -> Result<std::sync::Arc<crate::records::Set>> {
    crate::records::Set::current(&project.root).map_or_else(|| crate::records::Set::open(project), Ok)
}

/// Where an extension is placed when the project holds it nowhere yet: in the folder `extensions`, by its id.
const PLACED_IN: &Name = Name::constant("extensions");

/// Add an extension to the project, or update the one it holds of its id: its folder kept whole as a tree in the
/// store - a sealed node, found by its manifest wherever it is - and required at the versions of its line (`^0.1`). The
/// layer is the one named (`into`), else the one holding it already, else the project's highest that is written: where
/// that layer records it, its record holds the new tree; where one below does, it overrides it; else it is placed in
/// `extensions/`, named by its id. What it is is checked first, as loading checks it.
///
/// # Errors
/// It is no extension's folder, the project has no such layer or none to hold it, or it cannot be kept or
/// `project.toml` written. Held as it is already, and required so, it is no step.
pub fn add_extension(
    project: &Project,
    from: &std::path::Path,
    into: Option<&Name>,
    history: &mut History,
    label: String,
) -> Result<About> {
    use crate::records::{Kind, Op, Record};
    let from = &HostPath::new(from).with_context(|| from.display().to_string())?;
    // One that is only its manifest - programs and what they are fetched from - is checked as loading checks it.
    let about = if folder::alone(from, &project.io) {
        folder::checked_alone(from, &project.io).with_context(|| from.display().to_string())?
    } else {
        let component = from.join(&EXTENSION_COMPONENT);
        let bytes = project.io.filesystem().read(&component).with_context(|| component.display().to_string())?;
        folder::checked(from, &bytes, &project.io).with_context(|| from.display().to_string())?
    };
    let set = set_of(project)?;
    let held = records_of(project, &set, &about.id);
    let writable = |layer: &crate::Layer| !layer.mounted && !layer.read_only && layer.kind == crate::LayerKind::Assets;
    let holding = || {
        held.iter()
            .rev()
            .filter(|h| h.tree.is_some())
            .find_map(|h| layer_of(project, &h.folder).filter(|l| writable(l)))
    };
    let layer = match into {
        Some(into) => project.layer(into)?,
        None => {
            holding().or_else(|| project.extensions_layer()).context("the project has no layer it writes to hold it")?
        }
    };
    anyhow::ensure!(writable(layer), "{} cannot hold extensions: the project does not write it", layer.name);
    let (tree, _) = crate::chunks::Store::at(&project.root, project.io.clone()).put_folder(from)?;
    let at = |uid| crate::records::path_of(&layer.records, uid).map_err(anyhow::Error::from);
    let below: Vec<&crate::records::SealedRecord> = held.iter().filter(|h| h.folder != layer.records).collect();
    let own = held.iter().find(|h| h.folder == layer.records);
    let mut writes: Vec<(HostPath, String)> = Vec::new();
    match own {
        // Its record here holds the new tree, where and as what it is.
        Some(own) if own.record.tree == Some(tree) => {}
        Some(own) => {
            // An override's removals are of the version below, by the new tree.
            let below = below.iter().rev().find_map(|h| h.tree.as_deref().filter(|_| h.uid == own.uid));
            let files = if own.record.op == Op::Override {
                removed(below, &project.io, &tree, &project.root)
            } else {
                own.record.files.clone()
            };
            writes.push((at(own.uid)?, Record { tree: Some(tree), files, ..(*own.record).clone() }.render()));
        }
        // Held below: overridden here, whole.
        None if let Some(below) = below.iter().rev().find(|h| h.tree.is_some()) => {
            let record = Record {
                op: Op::Override,
                kind: Kind::Node,
                parent: None,
                name: None,
                files: removed(below.tree.as_deref(), &project.io, &tree, &project.root),
                tree: Some(tree),
                local: false,
                theirs: None,
            };
            writes.push((at(below.uid)?, record.render()));
        }
        // Held nowhere: placed in `extensions/`, which is made where the stack has no such folder.
        None => writes.extend(placed(project, &set, layer, &about.id, tree)?),
    }
    let mut document = read(project.io.filesystem(), &project.root)?;
    let before = document.to_string();
    let line = about.version.rsplit_once('.').map_or(about.version.as_str(), |(line, _)| line);
    require(&mut document, &Requirement { id: about.id.clone(), version: Some(format!("^{line}")) })?;
    // Held as it is, and required so: nothing to do.
    if writes.is_empty() && document.to_string() == before {
        return Ok(about);
    }
    let mut tx = history.begin(label);
    for (path, text) in writes {
        tx.write(&path, text.as_bytes())?;
    }
    tx.write(&project.root.join(&FILE), document.to_string().as_bytes())?;
    tx.commit()?;
    Ok(about)
}

/// The records placing an extension the project holds nowhere yet in a layer: in the folder `extensions`, by its id - and
/// that folder's, where the stack has none.
fn placed(
    project: &Project,
    set: &crate::records::Set,
    layer: &crate::Layer,
    id: &str,
    tree: pe2_ext::Sha1,
) -> Result<Vec<(HostPath, String)>> {
    use crate::records::{Kind, Op, Record};
    let at = |uid| crate::records::path_of(&layer.records, uid).map_err(anyhow::Error::from);
    let mut writes = Vec::new();
    let mut taken = set.uids();
    let mut uid = || {
        let uid = crate::uid::new(project.io.random(), |uid| taken.contains(&uid));
        taken.insert(uid);
        uid
    };
    let placed_in = crate::TreePath::root().join(PLACED_IN);
    let parent = if let Some(parent) = set.placed_at(&placed_in) {
        parent
    } else {
        let folder = uid();
        let record = Record {
            op: Op::Create,
            kind: Kind::Folder,
            parent: None,
            name: Some(PLACED_IN.to_owned()),
            files: std::collections::BTreeMap::new(),
            tree: None,
            local: false,
            theirs: None,
        };
        writes.push((at(folder)?, record.render()));
        folder
    };
    let name = Name::new(id).with_context(|| format!("{id}: no name an entry takes"))?;
    let record = Record {
        op: Op::Create,
        kind: Kind::Node,
        parent: Some(parent),
        name: Some(name.to_owned()),
        files: std::collections::BTreeMap::new(),
        tree: Some(tree),
        local: false,
        theirs: None,
    };
    writes.push((at(uid())?, record.render()));
    Ok(writes)
}

/// What an override of a sealed node whole removes of the version below (`below`): each of its files the new tree
/// (`tree`, kept in the store) has not.
fn removed(
    below: Option<&crate::chunks::Tree>,
    io: &std::sync::Arc<dyn pe2_io::Io>,
    tree: &pe2_ext::Sha1,
    root: &HostPath,
) -> std::collections::BTreeMap<std::sync::Arc<pe2_path::RelPath>, crate::records::File> {
    let now = crate::chunks::Store::at(root, io.clone()).get_tree(tree).unwrap_or_default();
    let names = below.into_iter().flat_map(|below| below.files.keys()).filter(|name| !now.files.contains_key(*name));
    names
        .filter_map(|name| pe2_path::RelPath::new(name).ok().map(ToOwned::to_owned))
        .map(|name| (name, crate::records::File { content: None, base: None }))
        .collect()
}

/// Remove an extension from the project: the records of the layers written that hold it, and what the project requires
/// of it. Where a layer the project does not write holds it still, its highest written layer deletes it there.
///
/// # Errors
/// The project neither holds nor requires it, a layer the project does not write holds it and none written can delete
/// it, or what holds it cannot be removed or `project.toml` written.
pub fn remove_extension(project: &Project, id: &str, history: &mut History, label: String) -> Result<()> {
    let set = set_of(project)?;
    let held = records_of(project, &set, id);
    let written = |folder: &HostPath| layer_of(project, folder).is_some_and(|layer| !layer.read_only);
    let fixed = held.iter().find(|h| h.tree.is_some() && !written(&h.folder));
    let off_in = match fixed {
        Some(fixed) => {
            let layer = project.extensions_layer().filter(|l| l.kind == crate::LayerKind::Assets);
            let name = layer_of(project, &fixed.folder).map_or_default(|l| l.name.to_string());
            Some((layer.with_context(|| format!("{id} is in {name}, and no layer written can delete it"))?, fixed.uid))
        }
        None => None,
    };
    let mut document = read(project.io.filesystem(), &project.root)?;
    let required = unrequire(&mut document, id);
    if !held.iter().any(|h| h.tree.is_some()) && !required {
        bail!("the project neither holds nor requires {id}");
    }
    let mut tx = history.begin(label);
    for h in held.iter().filter(|h| written(&h.folder)) {
        tx.remove(&crate::records::path_of(&h.folder, h.uid)?)?;
    }
    if let Some((layer, uid)) = off_in {
        let record = crate::records::Record {
            op: crate::records::Op::Delete,
            kind: crate::records::Kind::Node,
            parent: None,
            name: None,
            files: std::collections::BTreeMap::new(),
            tree: None,
            local: false,
            theirs: None,
        };
        tx.write(&crate::records::path_of(&layer.records, uid)?, record.render().as_bytes())?;
    }
    if required {
        tx.write(&project.root.join(&FILE), document.to_string().as_bytes())?;
    }
    tx.commit().map(drop)
}

/// Require an extension: what is said of it replaces what was.
fn require(document: &mut DocumentMut, requirement: &Requirement) -> Result<()> {
    let mut entry = Table::new();
    entry.insert("id", value(requirement.id.as_str()));
    if let Some(version) = &requirement.version {
        entry.insert("version", value(version.as_str()));
    }
    let entries = document.entry("extension").or_insert(Item::ArrayOfTables(ArrayOfTables::new()));
    let entries = entries.as_array_of_tables_mut().context("extension is not [[extension]]")?;
    let known =
        entries.iter().position(|known| known.get("id").and_then(Item::as_str) == Some(requirement.id.as_str()));
    match known {
        Some(at) => *entries.get_mut(at).context("what was just found")? = entry,
        None => entries.push(entry),
    }
    Ok(())
}

/// Require an extension no longer: whether it was.
fn unrequire(document: &mut DocumentMut, id: &str) -> bool {
    let Some(entries) = document.get_mut("extension").and_then(Item::as_array_of_tables_mut) else { return false };
    let before = entries.len();
    entries.retain(|known| known.get("id").and_then(Item::as_str) != Some(id));
    let removed = entries.len() != before;
    if entries.is_empty() {
        document.remove("extension");
    }
    removed
}

/// Keep a project setting of an extension, or (`None`) go back to its default.
///
/// # Errors
/// `project.toml` cannot be read or written.
pub fn set_setting(
    project: &Project,
    id: &str,
    key: &str,
    setting: Option<&toml::Value>,
    history: &mut History,
    label: String,
) -> Result<()> {
    let mut document = read(project.io.filesystem(), &project.root)?;
    match setting {
        Some(setting) => {
            let parsed: toml_edit::Value = setting.to_string().parse().context("a setting's value as TOML")?;
            let extensions = document.entry("extensions").or_insert_with(|| {
                // Made for the setting alone, it is written as its own header (`[extensions."pe2.images"]`).
                let mut table = Table::new();
                table.set_implicit(true);
                Item::Table(table)
            });
            let extensions = extensions.as_table_mut().context("[extensions] is not a table")?;
            let settings = extensions.entry(id).or_insert(Item::Table(Table::new()));
            let settings = settings.as_table_mut().with_context(|| format!("[extensions.\"{id}\"] is not a table"))?;
            settings.insert(key, value(parsed));
        }
        None => {
            if let Some(extensions) = document.get_mut("extensions").and_then(Item::as_table_mut) {
                if let Some(settings) = extensions.get_mut(id).and_then(Item::as_table_mut) {
                    settings.remove(key);
                    if settings.is_empty() {
                        extensions.remove(id);
                    }
                }
                if extensions.is_empty() {
                    document.remove("extensions");
                }
            }
        }
    }
    write(&project.root, &document, history, label)
}

#[cfg(test)]
#[allow(
    clippy::disallowed_methods,
    reason = "a test's fixtures: the machine's time, places, files and environment, which nothing tested reads"
)]
mod tests {
    use super::{add_extension, remove_extension, set_disabled, set_setting};
    use crate::Project;
    use crate::history::History;
    use crate::local::Local;
    use crate::lock::Lock;
    use crate::testing::At as _;
    use sfs::HostPath;

    #[test]
    fn writes_what_the_project_says_of_its_extensions_in_place() {
        let root = crate::testing::scratch("switches");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let text = "# the project\n[[layer]]\nname = \"game\"\n";
        std::fs::write(root.at("project.toml"), text).unwrap();
        crate::testing::as_records(&root);
        let project = Project::load(&root).unwrap();
        let lock = Lock::try_acquire(&root, "a test", &pe2_io::System).unwrap();
        let (mut history, _) = History::open(&Local::open(&project, &lock).unwrap()).unwrap();
        let index = crate::index::Index::open(&project, &Local::open(&project, &lock).unwrap()).unwrap();
        let depth = toml::Value::Integer(8);
        set_setting(&project, "pe2.images", "depth", Some(&depth), &mut history, "depth".into()).unwrap();
        set_disabled(&project, &["pe2.game".to_string()], &mut history, "off".into()).unwrap();
        let written = std::fs::read_to_string(root.at("project.toml")).unwrap();
        // Its own header, the comment kept.
        assert!(
            written.starts_with("# the project\n") && written.contains("[extensions.\"pe2.images\"]\ndepth = 8"),
            "{written}"
        );
        let read = Project::load(&root).unwrap();
        assert_eq!(read.extension_settings["pe2.images"]["depth"], depth);
        assert_eq!(read.disabled_extensions, ["pe2.game"]);
        // Back to its default: the setting goes, and with it its table.
        set_setting(&read, "pe2.images", "depth", None, &mut history, "default".into()).unwrap();
        assert!(Project::load(&root).unwrap().extension_settings.is_empty());
        for _ in 0..3 {
            history.undo(&index, None, false).unwrap();
        }
        assert_eq!(std::fs::read_to_string(root.at("project.toml")).unwrap(), text);
        drop(lock);
        crate::local::remove_all(&root);
    }

    /// A folder the way an extension is kept, its manifest beside a component carrying another: what the check
    /// refuses.
    fn kept(at: &HostPath, id: &str) {
        std::fs::create_dir_all(at.at("linux-x86_64")).unwrap();
        std::fs::write(at.at("extension.toml"), format!("id = \"{id}\"\nversion = \"0.2.1\"\n")).unwrap();
        std::fs::write(at.at("extension.wasm"), b"\0asm").unwrap();
        std::fs::write(at.at("linux-x86_64/tool"), b"a tool").unwrap();
    }

    #[test]
    fn an_extension_is_added_only_as_what_it_says_it_is() {
        let root = crate::testing::scratch("add");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.at("project.toml"), "[[layer]]\nname = \"base\"\n").unwrap();
        crate::testing::as_records(&root);
        let project = Project::load(&root).unwrap();
        let lock = Lock::try_acquire(&root, "a test", &pe2_io::System).unwrap();
        let (mut history, _) = History::open(&Local::open(&project, &lock).unwrap()).unwrap();
        let source = root.at("kept/my.tool.extension");
        kept(&source, "my.tool");
        // Its component carries no manifest: nothing is copied, nothing required.
        let refused = add_extension(&project, &source, None, &mut history, "add".into()).unwrap_err();
        assert!(format!("{refused:#}").contains("carries no manifest"), "{refused:#}");
        assert!(!root.at("layers/base/extensions").exists());
        assert_eq!(Project::load(&root).unwrap().requires, []);
        // Neither held nor required, it cannot go.
        assert!(remove_extension(&project, "my.tool", &mut history, "remove".into()).is_err());
        drop(lock);
        crate::local::remove_all(&root);
    }
}
