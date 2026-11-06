//! What extensions fetch, fetched beside the editor once they run: trusting them consents to it.

use super::isolate;
use crate::fetcher::{Fetcher, State};
use project::ext::setup::{One, Setup, Take};
use project::ext::{Extensions, Origin, Present, Status};

/// A running extension of the project's requiring a package, and one that is optional.
fn requiring() -> (Extensions, Setup, Setup) {
    let setup = |name: &str, lazy: bool| Setup {
        name: name.to_string(),
        license: None,
        copyright: None,
        platform: None,
        url: format!("https://example.invalid/{name}.tar.gz"),
        sha256: format!("{:0>64}", name.len()),
        take: Take::File(One { file: name.to_string(), sha256: format!("{:0>64}", name.len()) }),
        lazy,
        patches: vec![],
        mirrors: vec![],
    };
    let (required, optional) = (setup("required-tool", false), setup("optional", true));
    let present = Present {
        id: "test.fetching".to_string(),
        version: "0.1.0".to_string(),
        origin: Origin::Project("extensions/test.fetching.extension".into()),
        requires: vec![],
        weak: vec![],
        optional: vec![],
        provides: vec![],
        linked: vec![],
        status: Status::Loaded,
        size: None,
        contract: None,
        load: None,
        registers: vec![],
        trust: Some("a hash".to_string()),
        described: project::ext::component::Described::default(),
        setup: vec![required.clone(), optional.clone()],
    };
    let mut extensions = Extensions::default();
    extensions.present.push(present);
    (extensions, required, optional)
}

/// Tick until a package's fetch stands otherwise than running.
fn settled(
    fetcher: &mut Fetcher,
    extensions: &Extensions,
    ctx: &eframe::egui::Context,
    setup: &Setup,
) -> Option<State> {
    let started = std::time::Instant::now();
    loop {
        fetcher.tick(extensions, ctx);
        let state = fetcher.state_of(setup);
        if !matches!(state, Some(State::Fetching(..))) || started.elapsed() > std::time::Duration::from_secs(10) {
            return state;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn what_it_requires_is_fetched_unasked_and_what_is_optional_when_asked_for() {
    isolate();
    let (extensions, required, optional) = requiring();
    let ctx = eframe::egui::Context::default();
    let mut fetcher = Fetcher::default();
    // Started by itself: here, where downloads are off, it fails, saying why, and is not tried again unasked.
    let Some(State::Failed(why)) = settled(&mut fetcher, &extensions, &ctx, &required) else { panic!("not fetched") };
    assert!(why.contains("downloads are off"), "{why}");
    assert!(fetcher.status().is_some_and(|(said, failed)| failed && said.contains("failed")));
    fetcher.tick(&extensions, &ctx);
    assert!(matches!(fetcher.state_of(&required), Some(State::Failed(_))), "no loop of attempts");
    // What is optional only when asked for.
    assert_eq!(fetcher.state_of(&optional), None);
    fetcher.fetch_now("test.fetching", None, &optional);
    assert!(matches!(settled(&mut fetcher, &extensions, &ctx, &optional), Some(State::Failed(_))));
}

#[test]
fn what_was_fetched_is_listed_and_what_the_project_does_not_use_removed() {
    use super::{Fixture, Scenario};
    isolate();
    let store = extensions::setup_folder().expect("a store");
    // What an older editor left, which no project names.
    let left = store.join("0123456789abcdef-scenario");
    std::fs::create_dir_all(&left).expect("a folder");
    std::fs::write(left.join("cc1"), "a compiler").expect("a file");
    let mut editor = Scenario::open(Fixture::untrusted(false));
    editor.click_label("Continue");
    let mut editor = Scenario::open_showing(editor.into_fixture(), Some("extensions"));
    editor.click_label("Fetched");
    editor.expect("PE2.PSX");
    editor.expect("GCC 2.8.1 for the PlayStation");
    editor.expect("ELSEWHERE");
    editor.expect("an older editor's 0123456789abcdef-scenario");
    editor.click("Remove what this project does not use");
    assert!(!left.exists(), "removed");
    let psx: Vec<Setup> = editor
        .workspace()
        .extensions
        .present
        .iter()
        .filter(|p| p.id == "pe2.psx")
        .flat_map(|p| p.setup.clone())
        .collect();
    assert!(
        psx.iter().filter(|setup| setup.here()).all(|setup| setup.fetched(&store)),
        "what the project uses is kept"
    );
    editor.expect_not("an older editor's");
    editor.expect("all of it this project's");
}
