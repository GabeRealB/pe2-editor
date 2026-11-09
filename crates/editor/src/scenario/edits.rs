//! What is edited from outside the window - through the served folder, the command line, an agent - as the editor
//! takes it: a step of its history, in the layer edits go to, taken back by undo.

use super::{Fixture, Scenario};

/// What a records layer holds: its records' files, by name.
fn records(scenario: &mut Scenario) -> Vec<String> {
    let at = scenario.workspace().project.root.join(".layers/top/.records");
    let mut found: Vec<String> = walk(&at).into_iter().filter(|name| name != "layer.toml").collect();
    found.sort();
    found
}

fn walk(dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            out.extend(walk(&entry.path()));
        } else {
            out.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    out
}

#[test]
fn an_edit_from_outside_is_a_step_in_a_records_layer_and_undone() {
    let mut editor = Scenario::open(Fixture::notes_under_records());
    // A folder made: a record of the layer's, and an entry of the project's.
    let op = vfs::ops::Op::Folder { path: "made".into() };
    let (label, _) = editor.workspace().apply_edit(&op, None).expect("made");
    assert!(label.contains("made"), "{label}");
    editor.until("the folder to be taken in", |e| e.workspace().index.entries.contains_key("made"));
    assert_eq!(records(&mut editor).len(), 1);
    // The node below overridden: its record says what it was made from.
    let op = vfs::ops::Op::Override { path: "notes".into() };
    editor.workspace().apply_edit(&op, None).expect("overridden");
    editor.until("the override to be taken in", |e| e.workspace().index.assets["notes"].layers.len() == 2);
    assert_eq!(records(&mut editor).len(), 2);
    // Both taken back, one step each: the layer holds nothing again.
    for _ in 0..2 {
        editor.workspace().apply_edit(&vfs::ops::Op::Undo, None).expect("undone");
    }
    editor.until("both to be gone", |e| {
        !e.workspace().index.entries.contains_key("made") && e.workspace().index.assets["notes"].layers.len() == 1
    });
    assert!(records(&mut editor).is_empty(), "{:?}", records(&mut editor));
}
