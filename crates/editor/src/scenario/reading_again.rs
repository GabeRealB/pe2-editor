//! What a project is made of changing: it is read again with what it was still showing.

use project::ext::Status;

use super::{Fixture, Scenario};

/// The project, turned to leave this extension off, as the Extensions pane does.
fn turn_off(editor: &mut Scenario, id: &str) {
    let ws = editor.workspace();
    let written = project::switches::set_disabled(&ws.project, &[id.to_string()], &mut ws.history, "Turn it off".to_string());
    written.expect("project.toml written");
}

#[test]
fn turning_an_extension_off_leaves_the_project_showing() {
    let mut editor = Scenario::open(Fixture::rig());
    editor.show("rig/skeleton");
    assert_eq!(editor.status("pe2.images"), Status::Loaded);
    turn_off(&mut editor, "pe2.images");
    // Each frame has a workspace, or `workspace` says so, and none of them stops to open it.
    while editor.status("pe2.images") != Status::Disabled {
        editor.frame();
        assert_eq!(editor.workspace().selected.as_deref(), Some("rig/skeleton"), "what was showing still is");
        assert!(!editor.is_opening(), "the project is not opened again");
    }
    editor.settle();
    editor.expect("project.toml changed: the project was read again");
    // The session goes on.
    editor.undo();
    editor.settle();
    assert_eq!(editor.status("pe2.images"), Status::Loaded);
}

#[test]
fn what_is_asked_while_it_is_read_again_comes_after_it() {
    let mut editor = Scenario::open(Fixture::rig());
    turn_off(&mut editor, "pe2.images");
    // Undone before it has been taken in.
    editor.undo();
    editor.settle();
    assert_eq!(editor.status("pe2.images"), Status::Loaded);
    assert!(!editor.project_file().contains("pe2.images"));
}

#[test]
fn a_project_toml_that_cannot_be_read_leaves_the_project_as_it_was() {
    let mut editor = Scenario::open(Fixture::rig());
    editor.show("rig/skeleton");
    let file = editor.fixture.root().join(project::FILE);
    let good = editor.project_file();
    std::fs::write(&file, "[[layer\n").expect("written");
    editor.take_in_outside_changes();
    editor.until("it to say so", |editor| editor.says("the project could not be read again"));
    assert_eq!(editor.workspace().selected.as_deref(), Some("rig/skeleton"));
    std::fs::write(&file, good).expect("written");
    editor.take_in_outside_changes();
    editor.until("it to read it", |editor| editor.says("the project was read again"));
}

#[test]
fn the_project_as_it_was_is_let_go() {
    let mut editor = Scenario::open(Fixture::rig());
    editor.show("rig/skeleton");
    let replaced = std::sync::Arc::downgrade(&editor.workspace().index);
    editor.workspace().reload("a test asked");
    editor.settle();
    editor.until("it to go", |_| replaced.strong_count() == 0);
}
