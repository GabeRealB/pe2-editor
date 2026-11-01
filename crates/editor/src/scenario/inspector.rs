//! The Inspector's fields.

use eframe::egui;

use super::{Fixture, Scenario};

/// The second joint's translation, as written.
fn translation(editor: &Scenario, joint: usize) -> Vec<i64> {
    let declaration = editor.declaration("rig/skeleton");
    let joints = declaration["stack"][0]["joints"].as_array().expect("its joints");
    joints[joint]["translation"].as_array().expect("three numbers").iter().filter_map(toml::Value::as_integer).collect()
}

#[test]
fn a_typed_number_survives_selecting_something_else() {
    let mut editor = Scenario::open(Fixture::rig());
    editor.show("rig/skeleton");
    editor.click("pelvis");
    assert_eq!(editor.number("Translation", 1), "-927");
    editor.typing("-900");
    editor.click("waist");
    assert_eq!(translation(&editor, 1), [0, -900, 0]);
    assert!(editor.says("bones/2"), "the selection moved on");
    assert_eq!(editor.problems(), [""; 0]);
    editor.undo();
    assert_eq!(translation(&editor, 1), [0, -927, 0]);
}

#[test]
fn escape_leaves_a_number_as_it_was() {
    let mut editor = Scenario::open(Fixture::rig());
    editor.show("rig/skeleton");
    editor.click("pelvis");
    editor.number("Translation", 1);
    editor.typing("-900");
    editor.press(egui::Key::Escape);
    assert_eq!(translation(&editor, 1), [0, -927, 0]);
}
