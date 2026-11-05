//! What a project says of its extensions in `project.toml`: those it leaves off (`[extensions] disabled`, a list of
//! ids) and their project settings (`[extensions."<id>"]`). Each is written in place (the file's comments and layout
//! kept) as a step of the project's history, so it can be undone. The editor opens the project again when
//! `project.toml` changes, loading its extensions as it now says. What it takes in from outside itself is `[[extension]]`,
//! with what each was beside it in `extensions.lock`.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use toml_edit::{Array, ArrayOfTables, Item, Table, value};

use crate::history::History;
use crate::stack::{read, write};
use crate::{Imported, Project};

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

/// Take an extension in, or say more of one taken in: what is said of it replaces what was.
///
/// # Errors
/// `project.toml` cannot be read or written.
pub fn import(project: &Project, imported: &Imported, history: &mut History, label: String) -> Result<()> {
    let mut document = read(&project.root)?;
    let mut entry = Table::new();
    entry.insert("id", value(imported.id.as_str()));
    if let Some(version) = &imported.version {
        entry.insert("version", value(version.as_str()));
    }
    if imported.required {
        entry.insert("required", value(true));
    }
    if let Some(from) = &imported.from {
        entry.insert("from", value(from.as_str()));
    }
    let entries = document.entry("extension").or_insert(Item::ArrayOfTables(ArrayOfTables::new()));
    let entries = entries.as_array_of_tables_mut().context("extension is not [[extension]]")?;
    let known = entries.iter().position(|known| known.get("id").and_then(Item::as_str) == Some(imported.id.as_str()));
    match known {
        Some(at) => *entries.get_mut(at).context("what was just found")? = entry,
        None => entries.push(entry),
    }
    write(&project.root, &document, history, label)
}

/// Let go of an extension taken in. What the project holds itself goes by deleting it.
///
/// # Errors
/// `project.toml` cannot be read or written.
pub fn remove(project: &Project, id: &str, history: &mut History, label: String) -> Result<()> {
    let mut document = read(&project.root)?;
    if let Some(entries) = document.get_mut("extension").and_then(Item::as_array_of_tables_mut) {
        entries.retain(|known| known.get("id").and_then(Item::as_str) != Some(id));
        if entries.is_empty() {
            document.remove("extension");
        }
    }
    write(&project.root, &document, history, label)
}

/// Write what each extension from outside the project is.
///
/// # Errors
/// It cannot be written.
pub fn write_lock(project: &Project, locked: &BTreeMap<String, String>) -> Result<()> {
    let text = format!(
        "# What each extension from outside the project was when it was taken in.\n\n{}",
        toml::to_string(locked)?
    );
    crate::cache::write_atomic(&project.root.join(crate::EXTENSIONS_LOCK), text.as_bytes())
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
    use super::{import, remove, set_disabled, set_setting, write_lock};
    use crate::cache::Cache;
    use crate::history::History;
    use crate::lock::Lock;
    use crate::{Imported, Project};

    #[test]
    fn writes_what_the_project_says_of_its_extensions_in_place() {
        let root = std::env::temp_dir().join(format!("pe2-switches-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let text =
            "# the project\n[game]\nlayer = \"game\"\ndeclarations = \"res://d\"\n\n[[layer]]\nname = \"game\"\n";
        std::fs::write(root.join("project.toml"), text).unwrap();
        let project = Project::load(&root).unwrap();
        let lock = Lock::try_acquire(&root, "a test").unwrap();
        let (mut history, _) = History::open(&Cache::open(&project, &lock).unwrap()).unwrap();
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
            history.undo(false).unwrap();
        }
        assert_eq!(std::fs::read_to_string(root.join("project.toml")).unwrap(), text);
        drop(lock);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_extension_is_taken_in_said_more_of_and_let_go() {
        let root = std::env::temp_dir().join(format!("pe2-import-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let text = "# the project\n[[layer]]\nname = \"base\"\n";
        std::fs::write(root.join("project.toml"), text).unwrap();
        let project = Project::load(&root).unwrap();
        let lock = Lock::try_acquire(&root, "a test").unwrap();
        let (mut history, _) = History::open(&Cache::open(&project, &lock).unwrap()).unwrap();
        let tool = Imported { id: "my.tool".to_string(), version: Some("^0.1".to_string()), ..Imported::default() };
        import(&project, &tool, &mut history, "take".into()).unwrap();
        let other = Imported { id: "my.other".to_string(), from: Some("../other".to_string()), ..Imported::default() };
        import(&project, &other, &mut history, "take".into()).unwrap();
        import(&project, &Imported { required: true, ..tool.clone() }, &mut history, "require".into()).unwrap();
        write_lock(&project, &[("my.other".to_string(), "abc".to_string())].into()).unwrap();
        let read = Project::load(&root).unwrap();
        assert_eq!(read.manifest.imported.len(), 2);
        assert!(read.manifest.imported[0].required);
        assert_eq!(read.manifest.imported[1].from.as_deref(), Some("../other"));
        assert_eq!(read.manifest.locked["my.other"], "abc");
        assert_eq!(read.imported_from("../other"), root.join("../other"));
        remove(&read, "my.tool", &mut history, "go".into()).unwrap();
        remove(&read, "my.other", &mut history, "go".into()).unwrap();
        assert_eq!(std::fs::read_to_string(root.join("project.toml")).unwrap(), text, "nothing is left of it");
        drop(lock);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
