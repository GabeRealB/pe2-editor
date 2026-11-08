//! Documents: an asset edited is unsaved until it is saved, its working copy shown everywhere; the disk changing under
//! it makes it stale; undo follows the document in use.

use eframe::egui;

use super::{Fixture, Scenario};
use crate::workspace::EditRequest;

/// The pelvis's translation in a declaration.
fn pelvis(declaration: &toml::Table) -> Vec<i64> {
    let joints = declaration["stack"][0]["joints"].as_array().expect("its joints");
    joints[1]["translation"].as_array().expect("three numbers").iter().filter_map(toml::Value::as_integer).collect()
}

/// The pelvis moved in the inspector, as a user types it.
fn move_pelvis(editor: &mut Scenario, to: &str) {
    editor.show("rig/skeleton");
    editor.click("pelvis");
    editor.number("Translation", 1);
    editor.typing(to);
    editor.press(egui::Key::Enter);
}

#[test]
fn an_edit_is_unsaved_until_it_is_saved() {
    let mut editor = Scenario::open(Fixture::rig());
    move_pelvis(&mut editor, "-900");
    editor.expect("Unsaved changes");
    editor.expect("skeleton *");
    assert_eq!(pelvis(&editor.working("rig/skeleton")), [0, -900, 0]);
    assert_eq!(pelvis(&editor.declaration("rig/skeleton")), [0, -927, 0], "nothing is written before it is saved");
    editor.shortcut(egui::Key::S);
    editor.expect_not("Unsaved changes");
    assert_eq!(pelvis(&editor.declaration("rig/skeleton")), [0, -900, 0]);
    // Undone after the save: the edit is taken back, and the document is unsaved again; the disk keeps the save.
    editor.undo();
    editor.expect("Unsaved changes");
    assert_eq!(pelvis(&editor.working("rig/skeleton")), [0, -927, 0]);
    assert_eq!(pelvis(&editor.declaration("rig/skeleton")), [0, -900, 0]);
}

#[test]
fn a_change_on_disk_under_unsaved_work_is_asked_about() {
    let mut editor = Scenario::open(Fixture::rig());
    move_pelvis(&mut editor, "-900");
    let file = editor.node_folder("rig/skeleton").join("node.toml");
    let text = std::fs::read_to_string(&file).expect("its declaration");
    std::fs::write(&file, text.replace("-927", "-800")).expect("written");
    editor.take_in_outside_changes();
    editor.expect("Changed on disk while you edited it");
    assert_eq!(pelvis(&editor.working("rig/skeleton")), [0, -900, 0], "the working copy stays the user's");
    editor.click_label("Take the disk's");
    editor.expect_not("Changed on disk while you edited it");
    editor.expect_not("Unsaved changes");
    assert_eq!(pelvis(&editor.working("rig/skeleton")), [0, -800, 0]);
    // Taking the disk's is a step: undone, the user's edit is back, unsaved over what the disk has now.
    editor.undo();
    editor.expect("Unsaved changes");
    assert_eq!(pelvis(&editor.working("rig/skeleton")), [0, -900, 0]);
}

#[test]
fn undo_takes_back_the_document_in_use_and_no_other() {
    let fixture = Fixture::rig().node("rig/notes", "[[stack]]\nop = \"settings\"\nnote = \"one\"\n");
    let mut editor = Scenario::open(fixture);
    move_pelvis(&mut editor, "-900");
    let uid = editor.workspace().index.assets["rig/notes"].uid.clone().expect("a uid");
    let request = EditRequest::Write {
        uid,
        file: "node.toml".into(),
        bytes: b"[[stack]]\nop = \"settings\"\nnote = \"two\"\n".to_vec(),
        label: "Edit notes".into(),
        merge: None,
    };
    editor.workspace().edit(&request).expect("an edit");
    editor.settle();
    // The skeleton in use: its edit is undone, the notes' stays, though it was made last.
    editor.show("rig/skeleton");
    editor.undo();
    assert_eq!(pelvis(&editor.working("rig/skeleton")), [0, -927, 0]);
    assert_eq!(editor.working("rig/notes")["stack"][0]["note"].as_str(), Some("two"));
}

#[test]
fn closing_an_unsaved_document_asks() {
    let mut editor = Scenario::open(Fixture::rig());
    move_pelvis(&mut editor, "-900");
    editor.shortcut(egui::Key::W);
    editor.expect("Save the changes to skeleton before closing it?");
    editor.click_label("Don't save");
    editor.expect_not("skeleton *");
    assert_eq!(pelvis(&editor.working("rig/skeleton")), [0, -927, 0]);
    assert_eq!(pelvis(&editor.declaration("rig/skeleton")), [0, -927, 0]);
}

#[test]
fn back_and_forward_go_through_the_documents_visited() {
    let fixture = Fixture::rig().node("rig/notes", "[[stack]]\nop = \"settings\"\nnote = \"one\"\n");
    let mut editor = Scenario::open(fixture);
    editor.show("rig/skeleton");
    editor.show("rig/notes");
    assert_eq!(editor.editor().document_in_use().as_deref(), Some("rig/notes"));
    editor.alt(egui::Key::ArrowLeft);
    assert_eq!(editor.editor().document_in_use().as_deref(), Some("rig/skeleton"));
    editor.alt(egui::Key::ArrowRight);
    assert_eq!(editor.editor().document_in_use().as_deref(), Some("rig/notes"));
}

#[test]
fn a_point_of_the_history_is_shown_read_only_and_restored() {
    let mut editor = Scenario::open(Fixture::rig());
    move_pelvis(&mut editor, "-900");
    // Typed again, the pelvis still selected: a second step, once the first is no longer continued.
    std::thread::sleep(std::time::Duration::from_millis(1600));
    editor.number("Translation", 1);
    editor.typing("-800");
    editor.press(egui::Key::Enter);
    assert_eq!(pelvis(&editor.working("rig/skeleton")), [0, -800, 0]);
    assert_eq!(editor.workspace().history.done().len(), 2, "two steps");
    editor.click("History");
    // The first edit's point: the pelvis as it left it, everywhere, and nothing can be edited meanwhile.
    editor.click_topmost("Set translation of skeleton");
    editor.expect("Restore here");
    assert_eq!(pelvis(&editor.working("rig/skeleton")), [0, -900, 0]);
    let uid = editor.workspace().index.assets["rig/skeleton"].uid.clone().expect("a uid");
    let refused = editor.workspace().edit(&EditRequest::Write {
        uid,
        file: "node.toml".into(),
        bytes: b"x = 1".to_vec(),
        label: "Edit".into(),
        merge: None,
    });
    assert!(refused.is_err(), "read-only while a point is shown");
    editor.click_label("Restore here");
    editor.expect_not("Restore here");
    assert_eq!(pelvis(&editor.working("rig/skeleton")), [0, -900, 0]);
    // Restoring is a step: undone, the later edit is back.
    editor.undo();
    assert_eq!(pelvis(&editor.working("rig/skeleton")), [0, -800, 0]);
}

#[test]
fn a_sound_is_exported_as_a_wav_file() {
    let fixture = Fixture::beep();
    let mut editor = Scenario::open(fixture);
    editor.show("sfx/beep");
    editor.shortcut(egui::Key::E);
    editor.expect("Export beep");
    editor.click_label("WAV file, as played  (.wav)");
    editor.click_label("Export");
    let file = editor.workspace().project.root.join("exports/beep.wav");
    editor.until("the WAV file to be written", |_| file.exists());
    let bytes = std::fs::read(&file).expect("the file");
    assert_eq!(&bytes[..4], b"RIFF");
    assert!(bytes.len() > 44, "it holds what was played");
}

#[test]
fn a_new_node_goes_beside_the_document_in_use_named_and_from_its_defaults() {
    let mut editor = Scenario::open(Fixture::beep());
    editor.show("sfx/beep");
    editor.menu(&["File", "New node", "Sound script"]);
    editor.expect("New sound script in sfx");
    editor.expect("Rename");
    // Named at once: what is typed replaces the template's name.
    editor.typing("footstep");
    editor.press(egui::Key::Enter);
    editor.expect("Rename sound_script to footstep");
    assert!(editor.assets().iter().any(|a| a == "sfx/footstep"), "{:?}", editor.assets());
    editor.show("sfx/footstep");
    // Made from nothing, its settings are its type's defaults, shown so.
    editor.expect("Volume");
    editor.expect("default");
    editor.expect("Instruments: No instruments yet");
    editor.expect("no other instrument in the project to add");
}

#[test]
fn undoing_a_new_node_closes_its_tab() {
    let mut editor = Scenario::open(Fixture::beep());
    editor.show("sfx/beep");
    editor.menu(&["File", "New node", "Sound script"]);
    editor.escape();
    editor.show("sfx/sound_script");
    editor.editor().undo();
    editor.settle();
    assert!(!editor.assets().iter().any(|a| a == "sfx/sound_script"));
    editor.expect_not("is no longer in the project");
}
