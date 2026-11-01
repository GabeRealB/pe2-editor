//! Making nodes, and how a stack reads.

use eframe::egui;

use super::{Fixture, Scenario};
use crate::icons;

#[test]
fn new_node_makes_one_and_undo_takes_it_away() {
    let mut editor = Scenario::open(Fixture::rig());
    editor.menu(&["File", "New node", "Skeleton"]);
    assert!(editor.assets().iter().any(|path| path == "skeleton"), "{:?}", editor.assets());
    editor.undo();
    assert!(!editor.assets().iter().any(|path| path == "skeleton"), "{:?}", editor.assets());
}

#[test]
fn new_node_offers_what_needs_no_file() {
    let mut editor = Scenario::open(Fixture::rig());
    editor.menu(&["File", "New node"]);
    editor.expect("Skeleton");
    editor.expect_not("Mesh");
    editor.expect_not("from a video file");
    editor.expect_not("Disc");
    editor.expect_not("Stage");
    editor.expect("Package");
    editor.escape();
}

#[test]
fn a_node_short_of_a_setting_says_so_of_its_stack() {
    let mut editor = Scenario::open(Fixture::rig());
    editor.menu(&["File", "New node", "VRAM slot"]);
    let problems = editor.problems();
    assert!(problems.iter().any(|problem| problem.starts_with("its stack:") && problem.contains("has to be given")), "{problems:?}");
}

#[test]
fn contents_says_what_is_taken_on_the_operations_row() {
    let mut editor = Scenario::open(Fixture::notes());
    editor.show("notes");
    editor.expect("notes.txt, native, kept");
    editor.expect_not("Output");
}

#[test]
fn the_stack_tab_gives_wires_rather_than_settings() {
    let mut editor = Scenario::open(Fixture::rig());
    editor.show("rig/skeleton");
    editor.click(icons::STACK);
    editor.expect("node from skeleton");
    editor.expect("1 setting");
    editor.expect_not("joints =");
}

#[test]
fn apply_lists_what_the_node_makes_and_leaves_nothing_waiting() {
    let mut editor = Scenario::open(Fixture::rig());
    editor.show("rig/skeleton");
    editor.click("pelvis");
    editor.number("Translation", 1);
    editor.typing("-900");
    editor.press(egui::Key::Enter);
    editor.expect("UNAPPLIED CHANGES");
    editor.expect("Applying makes again: native");
    editor.expect_not("tokens");
    editor.expect_not("thumbnail");
    editor.click_label("Apply");
    editor.expect_not("UNAPPLIED CHANGES");
}

#[test]
fn a_second_preview_has_a_tab_of_its_own() {
    let mut editor = Scenario::open(Fixture::tapped());
    editor.show("rig/skeleton");
    editor.expect(icons::EYE);
}

#[test]
fn an_entry_gone_from_the_disk_can_be_forgotten() {
    let mut editor = Scenario::open(Fixture::rig());
    let folder = editor.node_folder("rig/skeleton");
    std::fs::remove_dir_all(folder).expect("a folder");
    editor.take_in_outside_changes();
    assert!(editor.problems().iter().any(|problem| problem.contains("does not hold it")), "{:?}", editor.problems());
    editor.click("Problems");
    editor.click("Forget it");
    assert_eq!(editor.problems(), [""; 0]);
}
