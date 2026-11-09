//! What a project says of its extensions in `project.toml`: those it leaves off (`[extensions] disabled`, a list of
//! ids) and their project settings (`[extensions."<id>"]`). Each is written in place (the file's comments and layout
//! kept) as a step of the project's history, so it can be undone. The editor opens the project again when
//! `project.toml` changes, loading its extensions as it now says.
//!
//! The extensions it holds are sealed, kept whole as trees in its store and named by its layers' records
//! (`records::EXTENSIONS`): one is added from wherever it is kept - and updated the same way - and removed, each as one
//! step with what the project requires of it (`[[extension]]`).

use std::path::Path;

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
    let mut document = read(&project.root)?;
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

/// What a layer's records say of an extension.
#[derive(PartialEq, Eq)]
enum Says {
    /// It holds it, as this tree.
    Holds(String),
    /// It leaves the one below off.
    LeavesOff,
}

/// What a layer's records say of an extension; nothing where they say nothing of it.
fn held_in(layer: &crate::Layer, id: &str) -> Option<Says> {
    let said = crate::records::extensions_of(&layer.path).ok()?.remove(id)?;
    Some(said.map_or(Says::LeavesOff, Says::Holds))
}

/// Add an extension to the project, or update the one it holds of its id: its folder kept whole as a tree in the
/// store, named by a layer's record in place of what was there, and required at the versions of its line (`^0.1`). The
/// layer is the one named (`into`), else the one holding it already, else the project's highest that is written. What
/// it is is checked first, as loading checks it.
///
/// # Errors
/// It is no extension's folder, the project has no such layer or none to hold it, or it cannot be kept or
/// `project.toml` written. Held as it is already, and required so, it is no step.
pub fn add_extension(
    project: &Project,
    from: &Path,
    into: Option<&str>,
    history: &mut History,
    label: String,
) -> Result<About> {
    let component = from.join(EXTENSION_COMPONENT);
    let bytes = std::fs::read(&component).with_context(|| component.display().to_string())?;
    let about = folder::checked(from, &bytes).with_context(|| from.display().to_string())?;
    let holding =
        || project.layers.iter().rev().find(|l| !l.mounted && matches!(held_in(l, &about.id), Some(Says::Holds(_))));
    let layer = match into {
        Some(into) => project.layer(into)?,
        None => {
            holding().or_else(|| project.extensions_layer()).context("the project has no layer it writes to hold it")?
        }
    };
    anyhow::ensure!(
        !layer.mounted && !layer.read_only && layer.kind == crate::LayerKind::Assets,
        "{} cannot hold extensions: the project does not write it",
        layer.name
    );
    let (tree, _) = crate::chunks::Store::at(&project.root).put_folder(from)?;
    let held_already = held_in(layer, &about.id) == Some(Says::Holds(tree.clone()));
    let mut document = read(&project.root)?;
    let before = document.to_string();
    let line = about.version.rsplit_once('.').map_or(about.version.as_str(), |(line, _)| line);
    require(&mut document, &Requirement { id: about.id.clone(), version: Some(format!("^{line}")) })?;
    // Held as it is, and required so: nothing to do.
    if held_already && document.to_string() == before {
        return Ok(about);
    }
    let mut tx = history.begin(label);
    if !held_already {
        let record = crate::records::extension_path(&layer.path, &about.id);
        tx.write(&record, crate::records::extension_record(Some(&tree)).as_bytes())?;
    }
    tx.write(&project.root.join(FILE), document.to_string().as_bytes())?;
    tx.commit()?;
    Ok(about)
}

/// Remove an extension from the project: the records of the layers written that hold it, and what the project requires
/// of it. Where a layer the project does not write holds it still, its highest written layer leaves that one off (a
/// record deleting it).
///
/// # Errors
/// The project neither holds nor requires it, a layer the project does not write holds it and none written can leave it
/// off, or what holds it cannot be removed or `project.toml` written.
pub fn remove_extension(project: &Project, id: &str, history: &mut History, label: String) -> Result<()> {
    let saying: Vec<&crate::Layer> =
        project.layers.iter().filter(|layer| !layer.mounted && held_in(layer, id).is_some()).collect();
    let holding = |layer: &crate::Layer| matches!(held_in(layer, id), Some(Says::Holds(_)));
    let fixed = saying.iter().find(|layer| holding(layer) && layer.read_only);
    let off_in = match fixed {
        Some(fixed) => {
            let layer = project.extensions_layer().filter(|l| l.kind == crate::LayerKind::Assets);
            Some(layer.with_context(|| format!("{id} is in {}, and no layer written can leave it off", fixed.name))?)
        }
        None => None,
    };
    let mut document = read(&project.root)?;
    let required = unrequire(&mut document, id);
    if !saying.iter().any(|layer| holding(layer)) && !required {
        bail!("the project neither holds nor requires {id}");
    }
    let mut tx = history.begin(label);
    for layer in saying.iter().filter(|layer| !layer.read_only) {
        tx.remove(&crate::records::extension_path(&layer.path, id))?;
    }
    if let Some(layer) = off_in {
        tx.write(&crate::records::extension_path(&layer.path, id), crate::records::extension_record(None).as_bytes())?;
    }
    if required {
        tx.write(&project.root.join(FILE), document.to_string().as_bytes())?;
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
    let mut document = read(&project.root)?;
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
mod tests {
    use super::{add_extension, remove_extension, set_disabled, set_setting};
    use crate::Project;
    use crate::cache::Cache;
    use crate::history::History;
    use crate::lock::Lock;

    #[test]
    fn writes_what_the_project_says_of_its_extensions_in_place() {
        let root = std::env::temp_dir().join(format!("pe2-switches-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let text = "# the project\n[[layer]]\nname = \"game\"\n";
        std::fs::write(root.join("project.toml"), text).unwrap();
        let project = Project::load(&root).unwrap();
        let lock = Lock::try_acquire(&root, "a test").unwrap();
        let (mut history, _) = History::open(&Cache::open(&project, &lock).unwrap()).unwrap();
        let index = crate::index::Index::open(&project, &Cache::open(&project, &lock).unwrap()).unwrap();
        let depth = toml::Value::Integer(8);
        set_setting(&project, "pe2.images", "depth", Some(&depth), &mut history, "depth".into()).unwrap();
        set_disabled(&project, &["pe2.game".to_string()], &mut history, "off".into()).unwrap();
        let written = std::fs::read_to_string(root.join("project.toml")).unwrap();
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
        assert_eq!(std::fs::read_to_string(root.join("project.toml")).unwrap(), text);
        drop(lock);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A folder the way an extension is kept, its manifest beside a component carrying another: what the check
    /// refuses.
    fn kept(at: &std::path::Path, id: &str) {
        std::fs::create_dir_all(at.join("linux-x86_64")).unwrap();
        std::fs::write(at.join("extension.toml"), format!("id = \"{id}\"\nversion = \"0.2.1\"\n")).unwrap();
        std::fs::write(at.join("extension.wasm"), b"\0asm").unwrap();
        std::fs::write(at.join("linux-x86_64/tool"), b"a tool").unwrap();
    }

    #[test]
    fn an_extension_is_added_only_as_what_it_says_it_is() {
        let root = std::env::temp_dir().join(format!("pe2-add-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("project.toml"), "[[layer]]\nname = \"base\"\n").unwrap();
        let project = Project::load(&root).unwrap();
        let lock = Lock::try_acquire(&root, "a test").unwrap();
        let (mut history, _) = History::open(&Cache::open(&project, &lock).unwrap()).unwrap();
        let source = root.join("kept/my.tool.extension");
        kept(&source, "my.tool");
        // Its component carries no manifest: nothing is copied, nothing required.
        let refused = add_extension(&project, &source, None, &mut history, "add".into()).unwrap_err();
        assert!(format!("{refused:#}").contains("carries no manifest"), "{refused:#}");
        assert!(!root.join(".layers/base/extensions").exists());
        assert_eq!(Project::load(&root).unwrap().requires, []);
        // Neither held nor required, it cannot go.
        assert!(remove_extension(&project, "my.tool", &mut history, "remove".into()).is_err());
        drop(lock);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
