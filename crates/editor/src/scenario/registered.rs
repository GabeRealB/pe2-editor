//! Tests extensions register, run as a project takes them in.

use super::{Fixture, Scenario};

#[test]
fn a_test_in_a_component_drives_the_editor() {
    let mut editor = Scenario::open(Fixture::tested());
    let tests = editor.workspace().extensions.tests();
    assert!(tests.iter().any(|name| name.starts_with("pe2.images.tests@")), "{tests:?}");
    assert!(tests.iter().any(|name| name.starts_with("pe2.render.tests@")), "{tests:?}");
    editor.run_test("pe2.render.tests@test.a_joint_selected_shows_its_translation").unwrap_or_else(|why| panic!("{why}"));
}

#[test]
fn a_test_failing_says_why() {
    let mut editor = Scenario::open(Fixture::tested());
    // What the test asks for is not there.
    std::fs::write(editor.node_folder("rig/skeleton").join("node.toml"), "[[stack]]\nop = \"pe2.render@skeleton\"\n").expect("a declaration");
    editor.take_in_outside_changes();
    let why = editor.run_test("pe2.render.tests@test.a_joint_selected_shows_its_translation").expect_err("nothing to select");
    assert!(why.contains("pelvis"), "{why}");
}

#[test]
fn a_run_finds_its_tests_through_the_project() {
    let fixture = Fixture::tested();
    let untouched = std::fs::read_dir(fixture.root().join(".layers/base")).expect("its layer").count();
    assert!(super::run(fixture.root(), None));
    assert_eq!(std::fs::read_dir(fixture.root().join(".layers/base")).expect("its layer").count(), untouched, "what the tests made is gone");
    assert!(!super::run(fixture.root(), Some("nothing is called this")), "a run of nothing has shown nothing");
}
