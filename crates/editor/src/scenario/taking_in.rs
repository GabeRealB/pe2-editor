//! What a project takes from where extensions are kept: added, copied whole, and removed.

use super::{Fixture, Scenario, isolate};

/// An extension in the user's own folder, as one is kept (`<id>.extension`): a built-in's component, as there is no
/// other to hand, with the manifest it carries. Gone with it.
struct Kept(std::path::PathBuf);

impl Kept {
    fn formats() -> Kept {
        isolate();
        let (_, bytes) =
            extensions::components().iter().find(|(package, _)| *package == "pe2-formats").expect("a built-in");
        let folder = extensions::user_folder().expect("a data folder").join("pe2.formats.extension");
        std::fs::create_dir_all(&folder).expect("a folder");
        std::fs::write(folder.join("extension.wasm"), bytes).expect("a component");
        let manifest = project::ext::component::manifest_text(bytes).expect("its manifest");
        std::fs::write(folder.join("extension.toml"), manifest).expect("a manifest");
        Kept(folder)
    }
}

impl Kept {
    /// The same, archived as `cargo xtask package` archives one: its folder whole inside.
    fn formats_archived() -> Kept {
        let folder = Kept::formats();
        let mut entries = vec![];
        for name in ["extension.wasm", "extension.toml"] {
            entries.push((format!("pe2.formats.extension/{name}"), std::fs::read(folder.0.join(name)).expect("it")));
        }
        let entries: Vec<codecs::zip::Entry> =
            entries.iter().map(|(name, data)| codecs::zip::Entry { name, data, deflate: true, mode: 0o644 }).collect();
        let archive = folder.0.with_file_name("pe2.formats-0.1.0.extension.zip");
        std::fs::write(&archive, codecs::zip::write(&entries)).expect("an archive");
        drop(folder);
        Kept(archive)
    }
}

impl Drop for Kept {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn what_is_kept_apart_is_added_and_removed() {
    let mut editor = Scenario::open_showing(Fixture::rig(), Some("extensions"));
    let kept = Kept::formats();
    // Where extensions are kept is looked at each second.
    editor.until("it to be found", |editor| {
        editor.workspace().extensions.present.iter().any(|p| p.status == project::ext::Status::Available)
    });
    editor.settle();
    editor.click("pe2.formats: loaded");
    editor.expect("pe2.formats 0.1.0 is kept: yours");
    editor.click("Add it to the project");
    assert!(editor.project_file().contains("id = \"pe2.formats\""), "{}", editor.project_file());
    editor.until("the project to hold it", |editor| {
        editor
            .workspace()
            .extensions
            .loaded
            .iter()
            .any(|l| l.id == "pe2.formats" && matches!(l.origin, project::ext::Origin::Project(_)))
    });
    editor.settle();
    editor.click("Remove it from the project");
    assert!(!editor.project_file().contains("[[extension]]"), "{}", editor.project_file());
    editor.until("the built-in to be back", |editor| {
        editor
            .workspace()
            .extensions
            .loaded
            .iter()
            .any(|l| l.id == "pe2.formats" && l.origin == project::ext::Origin::Builtin)
    });
    drop(kept);
}

#[test]
fn what_is_kept_archived_is_unpacked_into_the_project() {
    let mut editor = Scenario::open_showing(Fixture::rig(), Some("extensions"));
    let kept = Kept::formats_archived();
    editor.until("it to be found", |editor| {
        editor.workspace().extensions.present.iter().any(|p| p.status == project::ext::Status::Available)
    });
    editor.settle();
    editor.click("pe2.formats: loaded");
    editor.expect("pe2.formats 0.1.0 is kept: yours");
    editor.click("Add it to the project");
    editor.until("the project to hold it", |editor| {
        editor.workspace().extensions.loaded.iter().any(|l| {
            l.id == "pe2.formats"
                && matches!(&l.origin, project::ext::Origin::Project(folder) if folder.join("extension.toml").is_file())
        })
    });
    drop(kept);
}
