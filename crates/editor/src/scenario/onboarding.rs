//! What a project asks of whoever opens it.

use project::ext::Status;

use super::{Fixture, Scenario};

const ASKS: &str = "outside the sandbox";

#[test]
fn a_project_with_nothing_to_settle_asks_nothing() {
    let editor = Scenario::open(Fixture::rig());
    assert!(!editor.asking());
}

#[test]
fn trusting_a_layer_runs_what_is_in_it() {
    let mut editor = Scenario::open(Fixture::untrusted(false));
    assert!(editor.asking());
    editor.expect(ASKS);
    editor.expect("pe2.psx");
    assert_eq!(editor.status("pe2.psx"), Status::NotEnabled { changed: false });
    editor.click("layer mods");
    editor.click_label("Continue");
    assert!(!editor.asking());
    editor.until("it to run", |editor| editor.status("pe2.psx") == Status::Loaded);
    let editor = editor.opened_again();
    assert!(!editor.asking(), "answered, it is not asked again");
}

#[test]
fn trusting_the_whole_project_covers_its_layers() {
    let mut editor = Scenario::open(Fixture::untrusted(false));
    editor.click_label("Trust the whole project");
    editor.until("it to run", |editor| editor.status("pe2.psx") == Status::Loaded);
}

#[test]
fn saying_no_is_remembered_and_not_now_is_not() {
    let mut editor = Scenario::open(Fixture::untrusted(false));
    editor.click_label("Not now");
    assert!(!editor.asking());
    assert_eq!(editor.status("pe2.psx"), Status::NotEnabled { changed: false });
    let mut editor = editor.opened_again();
    assert!(editor.asking(), "not now is for this session");
    editor.click_label("Continue");
    let editor = editor.opened_again();
    assert!(!editor.asking(), "left unticked, it is not asked again");
}

#[test]
fn what_the_project_requires_is_asked_about_even_after_a_no() {
    let mut editor = Scenario::open(Fixture::untrusted(true));
    editor.expect("the project requires it");
    editor.click_label("Continue");
    let mut editor = editor.opened_again();
    assert!(editor.asking(), "the project is not whole");
    assert!(editor.problems().iter().any(|problem| problem.contains("not trusted")), "{:?}", editor.problems());
    editor.click("layer mods");
    editor.click_label("Continue");
    editor.until("it to run", |editor| editor.status("pe2.psx") == Status::Loaded);
    assert_eq!(editor.problems(), [""; 0]);
}

#[test]
fn a_choice_of_configs_is_asked_once() {
    let mut editor = Scenario::open(Fixture::two_configs());
    assert!(editor.asking());
    editor.expect("Config");
    editor.click_label("Continue");
    assert!(!editor.asking());
}

#[test]
fn a_no_can_be_taken_back_in_the_extensions_pane() {
    let mut editor = Scenario::open(Fixture::untrusted(false));
    editor.click_label("Continue");
    let fixture = editor.into_fixture();
    let mut editor = Scenario::open_showing(fixture, Some("extensions"));
    assert!(!editor.asking());
    editor.click("pe2.psx: not enabled");
    editor.click("Trust layer mods");
    editor.until("it to run", |editor| editor.status("pe2.psx") == Status::Loaded);
}

#[test]
fn trusting_shows_what_it_downloads_and_asks_nothing_more() {
    let mut editor = Scenario::open(Fixture::untrusted(false));
    editor.expect("pe2.psx downloads:");
    editor.expect("GCC 2.8.1 for the PlayStation");
    editor.expect("https://github.com/GabeRealB/pe2-editor/releases/download/gcc-psx-2.8.1-1/gcc-psx-2.8.1");
    editor.expect("sha256");
    editor.click_label("Trust the whole project");
    editor.until("it to run", |editor| editor.status("pe2.psx") == Status::Loaded);
    assert!(!editor.asking(), "trusting it was consenting to what it downloads");
}

/// The Extensions pane, narrow and wide: its overview and the details of the one selected, the choice it asks for in
/// view without scrolling either way.
#[test]
fn the_extensions_pane_fits_a_narrow_window_and_a_wide_one() {
    for size in [eframe::egui::vec2(760.0, 900.0), eframe::egui::vec2(1900.0, 700.0)] {
        let mut editor = Scenario::open(Fixture::untrusted(false));
        editor.click_label("Continue");
        let mut editor = Scenario::open_sized(editor.into_fixture(), Some("extensions"), size);
        editor.type_into("Filter extensions", "psx");
        editor.click("pe2.psx: not enabled");
        editor.expect("OUTSIDE THE SANDBOX");
        editor.click("Trust layer mods");
        editor.until("it to run", |editor| editor.status("pe2.psx") == Status::Loaded);
    }
}
