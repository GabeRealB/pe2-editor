//! What is edited from outside the window - through the served folder, the command line, an agent - as the editor
//! takes it: a step of its history, in the layer edits go to, taken back by undo.

use super::{Fixture, Scenario};

/// What a records layer holds: its records' files, by name.
fn records(scenario: &mut Scenario) -> Vec<String> {
    let at = scenario.workspace().project.root.join(".layers/top/.records");
    let mut found: Vec<String> = walk(&at).into_iter().filter(|name| name != "layer.toml").collect();
    found.sort();
    found
}

fn walk(dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            out.extend(walk(&entry.path()));
        } else {
            out.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    out
}

#[test]
fn an_edit_from_outside_is_a_step_in_a_records_layer_and_undone() {
    let mut editor = Scenario::open(Fixture::notes_under_records());
    // A folder made: a record of the layer's, and an entry of the project's.
    let op = vfs::ops::Op::Folder { path: "made".into() };
    let (label, _) = editor.workspace().apply_edit(&op, None).expect("made");
    assert!(label.contains("made"), "{label}");
    editor.until("the folder to be taken in", |e| e.workspace().index.entries.contains_key("made"));
    assert_eq!(records(&mut editor).len(), 1);
    // The node below overridden: its record says what it was made from.
    let op = vfs::ops::Op::Override { path: "notes".into() };
    editor.workspace().apply_edit(&op, None).expect("overridden");
    editor.until("the override to be taken in", |e| e.workspace().index.assets["notes"].layers.len() == 2);
    assert_eq!(records(&mut editor).len(), 2);
    // Both taken back, one step each: the layer holds nothing again.
    for _ in 0..2 {
        editor.workspace().apply_edit(&vfs::ops::Op::Undo, None).expect("undone");
    }
    editor.until("both to be gone", |e| {
        !e.workspace().index.entries.contains_key("made") && e.workspace().index.assets["notes"].layers.len() == 1
    });
    assert!(records(&mut editor).is_empty(), "{:?}", records(&mut editor));
}

#[test]
fn what_is_due_is_taken_from_where_it_is_said_and_answered_with_its_take_in() {
    let mut editor = Scenario::open(Fixture::notes_under_records());
    let root = editor.workspace().project.root.clone();
    let space = project::shared::space(&root).expect("the shared space");
    // What a serving would say due: a folder, one in it, and a removal of what is not there - so already.
    let folder = |path: &str| vfs::ops::Op::Folder { path: path.into() };
    let ops = [folder("made"), folder("made/inside"), vfs::ops::Op::Remove { path: "never".into() }];
    let numbers = vfs::due::put(&space, &ops).expect("said due");
    let (reply, answered) = std::sync::mpsc::channel();
    editor.workspace().take_due(&reply);
    editor.until("who asked to look to be answered", |_| answered.try_recv().is_ok_and(|reply| matches!(reply, mcp::Reply::Taken { count: 3 })));
    // Answered once it is taken in, and said where it was due: nothing is due, each has its answer.
    assert!(editor.workspace().index.entries.contains_key("made/inside"));
    assert!(!vfs::due::any(&space.read().unwrap()));
    let became = vfs::due::settle(&space, &numbers, false).expect("read");
    assert!(matches!(&became[..], [Some(Ok(_)), Some(Ok(_)), Some(Ok(so))] if so.contains("already")), "{became:?}");
    let said = records(&mut editor);
    assert_eq!(said.len(), 2, "a step each, and none for what was so already: {said:?} {became:?}");
    // Said due again - by a serving after one that ended: so already, and no edit made of it.
    let again = vfs::due::put(&space, &ops[..1]).expect("said due");
    editor.workspace().take_due(&reply);
    editor.until("the second look to be answered", |_| answered.try_recv().is_ok());
    let became = vfs::due::settle(&space, &again, false).expect("read");
    assert!(matches!(&became[..], [Some(Ok(so))] if so.contains("already")), "{became:?}");
    assert_eq!(records(&mut editor).len(), 2);
    // And with nothing due, asked to look, it says so at once.
    editor.workspace().take_due(&reply);
    assert!(matches!(answered.try_recv(), Ok(mcp::Reply::Taken { count: 0 })));
}

#[test]
fn what_is_asked_on_the_agents_link_is_done_with_no_frame_drawn() {
    let mut editor = Scenario::open(Fixture::notes_under_records());
    let root = editor.workspace().project.root.clone();
    let space = project::shared::space(&root).expect("the shared space");
    let outside = editor.workspace().outside();
    let folder = |path: &str| vfs::ops::Op::Folder { path: path.into() };
    let edit = |op: vfs::ops::Op| outside.answer(mcp::Request::Edit { op, layer: None }).expect("the worker's to answer");
    // An edit, and one over the project as the first left it: made by the worker, answered here - no frame is
    // drawn meanwhile.
    assert!(matches!(edit(folder("made")), mcp::Reply::Edited { .. }));
    let inside = edit(folder("made/inside"));
    assert!(matches!(&inside, mcp::Reply::Edited { label, .. } if label.contains("inside")), "{inside:?}");
    // What is due of what programs did through the served folder, taken and answered the same way.
    let numbers = vfs::due::put(&space, &[folder("due")]).expect("said due");
    let taken = outside.answer(mcp::Request::Due).expect("the worker's to answer");
    assert!(matches!(taken, mcp::Reply::Taken { count: 1 }), "{taken:?}");
    assert!(!vfs::due::any(&space.read().unwrap()));
    assert!(matches!(&vfs::due::settle(&space, &numbers, false).expect("read")[..], [Some(Ok(_))]));
    // The last step taken back, as the History pane's undo does.
    let undone = edit(vfs::ops::Op::Undo);
    assert!(matches!(&undone, mcp::Reply::Edited { label, .. } if label.starts_with("Undid")), "{undone:?}");
    // What reads the project is answered here too, against the project as those left it.
    let call = |name: &str| mcp::Request::Call { name: name.into(), arguments: serde_json::Map::new() };
    let listed = outside.answer(call("list_folder")).expect("answered off the window");
    assert!(listed.told().is_ok_and(|said| said.contains("made")), "{listed:?}");
    assert!(matches!(outside.answer(mcp::Request::Hello), Ok(mcp::Reply::Hello(_))));
    // What the window itself shows is the window's to say.
    assert!(outside.answer(call("editor_selection")).is_err());
    assert!(outside.answer(call("editor_ui")).is_err());
    // The window shows all of it once it draws again.
    editor.until("the window to show it", |e| {
        let entries = &e.workspace().index.entries;
        entries.contains_key("made/inside") && !entries.contains_key("due")
    });
    assert_eq!(editor.workspace().history.done().len(), 2, "two steps left of the three made");
    // While a point of the history is shown, nothing is edited from outside either.
    editor.workspace().view(Some(crate::workspace::Viewing { point: 1, scope: None, now: false }));
    editor.frame();
    let refused = edit(folder("later"));
    assert!(refused.told().is_err_and(|why| why.contains("an earlier point of the history")), "{refused:?}");
}

#[test]
fn the_view_of_the_layers_is_kept_off_the_windows_thread() {
    let mut editor = Scenario::open(Fixture::notes_under_records());
    let kept = editor.workspace().cache_path("editor/layers.toml");
    editor.workspace().toggle_lock("top").expect("locked");
    // Written by the worker's thread, aside: there a moment after, with no frame waited for.
    editor.until("the view to be kept", |_| std::fs::read_to_string(&kept).is_ok_and(|text| text.contains("top")));
}

#[test]
fn what_the_editor_wrote_is_no_change_of_another_program() {
    let mut editor = Scenario::open(Fixture::notes_under_records());
    let outside = editor.workspace().outside();
    let edit = |op: vfs::ops::Op| outside.answer(mcp::Request::Edit { op, layer: None }).expect("the worker's to answer");
    assert!(matches!(edit(vfs::ops::Op::Folder { path: "made".into() }), mcp::Reply::Edited { .. }));
    assert!(matches!(edit(vfs::ops::Op::Override { path: "notes".into() }), mcp::Reply::Edited { .. }));
    // What watches the layers tells of the files those wrote, a moment after: they are the editor's own.
    let started = std::time::Instant::now();
    while started.elapsed() < std::time::Duration::from_millis(900) {
        editor.frame();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let waiting = editor.workspace().pending_paths();
    assert!(waiting.is_empty(), "taken for another program's: {waiting:?}");
}
