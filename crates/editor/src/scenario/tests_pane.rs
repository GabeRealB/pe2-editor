//! The Tests pane, with a runner that says what it is told to.

use std::os::unix::fs::PermissionsExt as _;

use super::{Fixture, Scenario};

/// A runner saying it has two tests: one passes, the other fails.
const RUNNER: &str = r#"#!/bin/sh
case "$*" in
*--list*)
    echo '{"event":"test","suite":"render","name":"pe2.render.tests@test.draws"}'
    echo '{"event":"test","suite":"render","name":"pe2.render.tests@test.breaks"}'
    ;;
*)
    echo '{"event":"started","suite":"render","name":"pe2.render.tests@test.draws"}'
    echo '{"event":"passed","suite":"render","name":"pe2.render.tests@test.draws","seconds":0.25}'
    echo '{"event":"failed","suite":"render","name":"pe2.render.tests@test.breaks","seconds":0.5,"why":"nothing says it","picture":"PICTURE"}'
    echo '{"event":"done","passed":1,"failed":1}'
    ;;
esac
"#;

fn with_runner() -> Scenario {
    let mut editor = Scenario::open_showing(Fixture::rig(), Some("tests"));
    let png = codecs::texture::rgba_png(2, 2, &[255; 16]).expect("a picture");
    let picture = editor.file_elsewhere("failed.png", &png);
    let runner = editor.file_elsewhere("runner.sh", RUNNER.replace("PICTURE", &picture.display().to_string()).as_bytes());
    std::fs::set_permissions(&runner, std::fs::Permissions::from_mode(0o755)).expect("a program");
    editor.workspace().tests.use_runner(runner);
    editor.settle();
    editor.settle();
    editor
}

#[test]
fn it_lists_what_the_runner_says_there_is() {
    let editor = with_runner();
    editor.expect("draws");
    editor.expect("breaks");
    editor.expect("0 of 2 passed");
}

#[test]
fn a_run_shows_how_each_went_and_why_one_failed() {
    let mut editor = with_runner();
    editor.click_label("Run all");
    editor.settle();
    editor.expect("1 failed, 1 passed");
    editor.expect_not("  ·  0 of");
    editor.expect("0.5 s");
    editor.click("breaks");
    editor.expect("nothing says it");
    assert!(editor.workspace().tests.showing_picture());
}

#[test]
fn with_no_runner_it_says_how_to_have_one() {
    let editor = Scenario::open_showing(Fixture::rig(), Some("tests"));
    editor.expect("no pe2-test-runner");
}
