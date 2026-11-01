//! Tests extensions carry, run by what they declare.

use super::suite::{self, Out, Suite};
use super::{Fixture, Scenario};

fn of(name: &str) -> Suite {
    suite::built().into_iter().find(|suite| suite.name == name).unwrap_or_else(|| panic!("no suite {name}"))
}

#[test]
fn every_suite_passes_in_projects_of_its_own() {
    let report = suite::run(&suite::built(), &[], &Out::default());
    assert!(report.passed(), "{:?}", report.failed);
    let tests = |of: &str| report.passed.iter().filter(|name| name.starts_with(of)).count();
    assert!(tests("pe2.render.tests@") >= 3, "{:?}", report.passed);
    assert!(tests("pe2.images.tests@") >= 1, "{:?}", report.passed);
}

#[test]
fn a_run_of_nothing_shows_nothing() {
    assert!(!suite::run(&suite::built(), &["nothing is called this".to_string()], &Out::default()).passed());
}

#[test]
fn a_test_failing_says_why() {
    // What the doll's texture is made with is left off.
    let mut suite = of("render");
    suite.declared.disabled.push("pe2.images".to_string());
    let artifacts = std::env::temp_dir().join("pe2-scenarios").join("failed");
    let out = Out { artifacts: Some(artifacts.clone()), ..Out::default() };
    let report = suite::run(&[suite], &["a_prefab".to_string()], &out);
    assert!(!report.passed());
    assert!(report.failed.iter().any(|(_, why)| why.contains("texture")), "{:?}", report.failed);
    let kept: Vec<std::path::PathBuf> = std::fs::read_dir(&artifacts).expect("what it kept").flatten().map(|entry| entry.path()).collect();
    let has = |extension: &str| kept.iter().any(|file| file.extension().is_some_and(|own| own == extension));
    assert!(has("txt") && has("png"), "{kept:?}");
}

#[test]
fn a_project_runs_the_tests_of_its_extensions_and_is_left_as_it_was() {
    let fixture = Fixture::with_tests();
    let layer = fixture.root().join(".layers/base");
    let count = || std::fs::read_dir(&layer).expect("its layer").count();
    let untouched = count();
    assert!(super::run(fixture.root(), &[]));
    assert_eq!(count(), untouched, "what the tests made is gone");
    assert!(!super::run(fixture.root(), &["nothing is called this".to_string()]));
}

#[test]
fn a_step_that_cannot_be_done_says_so() {
    let mut editor = Scenario::open(Fixture::rig());
    assert!(editor.run_test("pe2.render.tests@test.none").expect_err("no such test").contains("none"));
}

#[test]
fn a_folder_holds_a_suite_or_says_it_does_not() {
    let tests = extensions::built_suites().into_iter().next().expect("built with the others");
    let extension = tests.parent().expect("its extension").to_path_buf();
    assert_eq!(suite::in_folders(&[extension]).expect("its tests").len(), 1);
    assert!(suite::in_folders(&[tests.join("data")]).is_err());
}

#[test]
fn an_extension_says_what_counts_as_it() {
    let about = project::ext::component::about(&extensions::built_apart("pe2_psx.wasm").expect("built with the others")).expect("who it is");
    assert_eq!(about.hashed, ["linux-x86_64"]);
}
