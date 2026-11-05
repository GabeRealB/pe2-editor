//! What a project is made of changing: it is read again with what it was still showing.

use project::ext::Status;

use super::{Fixture, Scenario};

/// The project, turned to leave this extension off, as the Extensions pane does.
fn turn_off(editor: &mut Scenario, id: &str) {
    let ws = editor.workspace();
    let written =
        project::switches::set_disabled(&ws.project, &[id.to_string()], &mut ws.history, "Turn it off".to_string());
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

#[test]
fn what_is_asked_while_it_is_being_read_waits_without_stopping_the_window() {
    let mut editor = Scenario::open(Fixture::rig());
    turn_off(&mut editor, "pe2.images");
    editor.settle();
    editor.hold_reading().expect("held");
    editor.undo();
    assert!(editor.editor().is_deferring(), "it waits its turn");
    assert_eq!(editor.status("pe2.images"), Status::Disabled, "the window goes on, and it is not done yet");
    editor.release_reading().expect("let go");
    assert_eq!(editor.status("pe2.images"), Status::Loaded);
}

#[test]
fn another_project_opens_without_waiting_for_the_one_left() {
    let mut editor = Scenario::open(Fixture::rig());
    let other = Fixture::notes();
    editor.hold_reading().expect("held");
    // A window that waited for the worker would wait here for good: it is let go below.
    editor.editor().open_project(other.root());
    editor.frame();
    assert!(editor.is_opening());
    editor.release_reading().expect("let go");
    assert!(editor.workspace().exists("notes"), "it opened");
}

#[test]
fn code_is_overridden_in_a_layer_made_for_it() {
    let mut editor = Scenario::open(Fixture::code());
    let root = editor.fixture.root().to_path_buf();
    editor.override_entry("src/thing.c").expect("an entry");
    editor.expect("is code, from code");
    editor.type_into("new files layer", "mine");
    editor.click("Create and override");
    assert!(editor.project_file().contains("name = \"mine\""), "the layer was added");
    let copy = root.join(".layers/mine/src/thing.c");
    assert_eq!(std::fs::read_to_string(&copy).expect("the copy"), "int thing;\n");
    assert!(editor.workspace().layers.iter().any(|layer| layer.name == "mine"), "the project has it");
}

#[test]
fn code_overridden_while_the_project_is_read_waits_for_the_layer() {
    let mut editor = Scenario::open(Fixture::code());
    let root = editor.fixture.root().to_path_buf();
    editor.override_entry("src/thing.c").expect("an entry");
    editor.type_into("new files layer", "mine");
    editor.hold_reading().expect("held");
    editor.click("Create and override");
    assert!(editor.project_file().contains("name = \"mine\""), "the layer was added");
    assert!(!root.join(".layers/mine/src/thing.c").exists(), "it waits for the project to have the layer");
    editor.release_reading().expect("let go");
    assert!(root.join(".layers/mine/src/thing.c").is_file());
}

#[test]
fn a_project_toml_changed_in_nothing_it_is_read_for_leaves_what_is_showing() {
    let mut editor = Scenario::open(Fixture::rig());
    editor.show("rig/skeleton");
    let shown = editor.workspace().views_alive();
    assert!(shown > 0, "something to keep");
    let file = editor.fixture.root().join(project::FILE);
    let said = format!("{}\n# a note\n", editor.project_file());
    std::fs::write(&file, said).expect("written");
    editor.take_in_outside_changes();
    editor.until("it to be read", |editor| editor.says("the project was read again"));
    assert_eq!(editor.workspace().views_alive(), shown, "nothing was made again");
}
