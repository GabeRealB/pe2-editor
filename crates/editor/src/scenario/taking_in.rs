//! What a project takes in from outside itself.

use super::{Fixture, Scenario, isolate};

/// An extension in the user's own folder: a built-in's component, as there is no other to hand. Gone with it.
struct Kept(std::path::PathBuf);

impl Kept {
    fn formats() -> Kept {
        isolate();
        let folder = extensions::user_folder().expect("a data folder");
        std::fs::create_dir_all(&folder).expect("a folder");
        let (_, bytes) = extensions::components().iter().find(|(package, _)| *package == "pe2-formats").expect("a built-in");
        let file = folder.join("formats.wasm");
        std::fs::write(&file, bytes).expect("a component");
        Kept(file)
    }
}

impl Drop for Kept {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn what_is_kept_apart_is_taken_in_and_let_go() {
    let mut editor = Scenario::open_showing(Fixture::rig(), Some("extensions"));
    let kept = Kept::formats();
    // The folder is looked at each second.
    editor.until("it to be found", |editor| editor.workspace().extensions.present.iter().any(|p| p.status == project::ext::Status::Available));
    editor.settle();
    editor.click("pe2.formats: loaded");
    editor.expect("available, not taken in");
    editor.click("Take it into the project");
    assert!(editor.project_file().contains("id = \"pe2.formats\""), "{}", editor.project_file());
    editor.until("the project to open again", |editor| !editor.says("Take it into the project") && editor.says("Let it go"));
    editor.click("Let it go");
    assert!(!editor.project_file().contains("[[extension]]"), "{}", editor.project_file());
    drop(kept);
}
