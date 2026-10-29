//! The extensions a project leaves off: `[extensions] disabled` in `project.toml`, a list of ids, written in place
//! (the file's comments and layout kept) as a step of the project's history, so it can be undone. The editor opens the
//! project again when `project.toml` changes, loading its extensions without those.

use anyhow::Result;
use toml_edit::{Array, Item, Table, value};

use crate::Project;
use crate::history::History;
use crate::stack::{read, write};

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
