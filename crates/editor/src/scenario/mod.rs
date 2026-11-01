//! The editor run with no window, a scenario at a time: what a person does, step by step, and what must have come of it.
//! A scenario is an ordinary test. It opens a project of its own (`Fixture`) and acts as the user does: the same pointer and
//! keys, found by what the window says, through egui's test harness and its accessibility tree. What it checks is what came
//! of that - what is written, the problems, what a window says - not pixels, which differ between GPUs.
//!
//! Work goes on off the window's thread, so that a harness with no repaint to make has not finished: `settle` waits until
//! the editor says it has nothing going on.

mod fixture;
mod inspector;
mod models;
mod nodes;
mod taking_in;

use std::sync::Once;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::kittest::{By, NodeT as _, Queryable as _};
use egui_kittest::{Harness, Node};

use crate::app::{Editor, Start};
pub use fixture::Fixture;

/// What a window is in a scenario: what a laptop has, so that nothing it asks for is out of sight.
const SIZE: egui::Vec2 = egui::vec2(1400.0, 900.0);
/// How long work off the window's thread may take: a first run compiles the extensions.
const PATIENCE: Duration = Duration::from_secs(120);
/// How many frames with nothing going on make it settled: what comes of a frame shows in the next.
const QUIET: u32 = 3;

/// What scenarios write goes under this, emptied by the next run: work off the window's thread can outlast one.
fn folder() -> std::path::PathBuf {
    std::env::temp_dir().join("pe2-scenarios")
}

/// The process's data folder, one for every scenario, as nothing reaches it from the user's.
fn isolate() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = std::fs::remove_dir_all(folder());
        let data = folder().join("data");
        std::fs::create_dir_all(&data).expect("a data folder");
        extensions::use_data_folder(data);
        project::ext::component::link_gpu(pe2_webgpu::add_to_linker);
    });
}

/// A file dropped on the window.
#[derive(Debug)]
struct Dropped(std::path::PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &std::path::Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|err| err.to_string())
    }
}

/// The editor at work on a fixture.
pub struct Scenario {
    harness: Harness<'static, Editor>,
    fixture: Fixture,
    /// One at a time: a process has one set of extensions, keeping what they make in the project open.
    _alone: std::sync::MutexGuard<'static, ()>,
}

impl Scenario {
    /// The editor on a fixture, its project open.
    #[track_caller]
    pub fn open(fixture: Fixture) -> Scenario {
        Scenario::open_showing(fixture, None)
    }

    /// The editor on a fixture, with a pane in front.
    #[track_caller]
    pub fn open_showing(fixture: Fixture, show: Option<&str>) -> Scenario {
        static ALONE: std::sync::Mutex<()> = std::sync::Mutex::new(());
        // A scenario that failed says so itself: the next has nothing to do with it.
        let alone = ALONE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = fixture.root().to_path_buf();
        let show = show.map(str::to_string);
        let harness = Harness::builder().with_size(SIZE).wgpu().build_eframe(move |cc| {
            if let Some(state) = &cc.wgpu_render_state {
                crate::install_gpu(state);
            }
            let start = Start { project: Some(root), select: None, show, screenshot: None, max_surface: None, agents_drive: false, monitors: Vec::new() };
            Editor::new(cc, start)
        });
        let mut scenario = Scenario { harness, fixture, _alone: alone };
        scenario.settle();
        assert!(scenario.harness.state_mut().workspace().is_some(), "the project did not open: {:?}", scenario.harness.state().error());
        scenario
    }

    /// Frames until the editor has nothing going on.
    #[track_caller]
    pub fn settle(&mut self) {
        let started = Instant::now();
        let mut quiet = 0;
        while quiet < QUIET {
            self.harness.input_mut().focused = true;
            self.harness.step();
            quiet = if self.harness.state().busy() { 0 } else { quiet + 1 };
            assert!(started.elapsed() < PATIENCE, "the editor did not settle");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Open an asset, as selecting it does.
    #[track_caller]
    pub fn show(&mut self, path: &str) {
        let ws = self.workspace();
        assert!(ws.index.assets.contains_key(path), "no asset {path}");
        ws.select(path.to_string());
        self.settle();
    }

    /// The widget saying this, the one there is: a scenario naming two things alike says so.
    #[track_caller]
    fn widget<'a>(&'a self, text: &'a str) -> Node<'a> {
        let mut found: Vec<Node<'_>> = self.harness.query_all(By::new().label_contains(text)).collect();
        match found.len() {
            1 => found.remove(0),
            0 => panic!("nothing says {text:?}"),
            many => panic!("{many} things say {text:?}: {:?}", found.iter().filter_map(|node| node.accesskit_node().label()).collect::<Vec<_>>()),
        }
    }

    /// Click what says this.
    #[track_caller]
    pub fn click(&mut self, text: &str) {
        self.widget(text).click();
        self.settle();
    }

    /// Click what says exactly this: what is open in a menu, say.
    #[track_caller]
    pub fn click_label(&mut self, text: &str) {
        let found: Vec<Node<'_>> = self.harness.query_all(By::new().label(text)).collect();
        match found.as_slice() {
            [one] => one.click(),
            [] => panic!("nothing is called {text:?}"),
            many => panic!("{} things are called {text:?}", many.len()),
        }
        self.settle();
    }

    /// Open a menu and go on through it: `["File", "New node", "Skeleton"]`.
    #[track_caller]
    pub fn menu(&mut self, path: &[&str]) {
        for item in path {
            self.click_label(item);
        }
    }

    /// Press Escape: what is open closes.
    pub fn escape(&mut self) {
        self.press(egui::Key::Escape);
    }

    /// Frames until something holds that only the disk's changes bring: nothing says when those reach the editor.
    #[track_caller]
    pub fn until(&mut self, what: &str, holds: impl Fn(&mut Scenario) -> bool) {
        let started = Instant::now();
        while !holds(self) {
            assert!(started.elapsed() < PATIENCE, "{what} never came");
            self.settle();
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// What another program changed taken in, as going back to the window does: it waits for that until then.
    #[track_caller]
    pub fn take_in_outside_changes(&mut self) {
        self.until("the change to be seen", |editor| editor.workspace().pending() > 0);
        self.harness.input_mut().focused = false;
        self.harness.step();
        self.settle();
    }

    /// What the project's `project.toml` says.
    pub fn project_file(&self) -> String {
        std::fs::read_to_string(self.fixture.root().join(project::FILE)).expect("a project file")
    }

    /// The assets the project has, by path.
    pub fn assets(&mut self) -> Vec<String> {
        self.workspace().index.assets.keys().cloned().collect()
    }

    /// What it has open.
    #[track_caller]
    pub fn workspace(&mut self) -> &mut crate::workspace::Workspace {
        self.harness.state_mut().workspace().expect("a project is open")
    }

    /// That something says this, or what does say something.
    #[track_caller]
    pub fn expect(&self, text: &str) {
        assert!(self.says(text), "nothing says {text:?}; there is:\n{}", self.shown().join("\n"));
    }

    /// That nothing says this.
    #[track_caller]
    pub fn expect_not(&self, text: &str) {
        let said: Vec<String> = self.shown().into_iter().filter(|line| line.contains(text)).collect();
        assert!(said.is_empty(), "this should not say {text:?}: {said:?}");
    }

    /// Everything the window says.
    fn shown(&self) -> Vec<String> {
        let all = self.harness.query_all(By::new().predicate(|_| true).include_labels());
        all.filter_map(|node| node.accesskit_node().label().or_else(|| node.accesskit_node().value())).filter(|text| !text.trim().is_empty()).collect()
    }

    /// Whether something says this.
    pub fn says(&self, text: &str) -> bool {
        self.harness.query_all(By::new().label_contains(text).include_labels()).next().is_some()
    }

    /// Of the numbers in the row under a label, this one (from 0), clicked and so being typed in; what it shows now.
    #[track_caller]
    pub fn number(&mut self, row: &str, at: usize) -> String {
        let under = self.harness.query_all(By::new().label(row).include_labels()).next().unwrap_or_else(|| panic!("no row {row:?}")).rect();
        let mut numbers: Vec<Node<'_>> = self
            .harness
            .query_all(By::new().role(egui::accesskit::Role::SpinButton))
            .filter(|node| node.rect().top() >= under.top())
            .collect();
        numbers.sort_by(|a, b| (a.rect().top(), a.rect().left()).partial_cmp(&(b.rect().top(), b.rect().left())).expect("a place"));
        let number = numbers.get(at).unwrap_or_else(|| panic!("no number {at} under {row:?}"));
        number.click();
        let value = number.value().unwrap_or_default();
        self.settle();
        value
    }

    /// Type, into what has the keyboard.
    pub fn typing(&mut self, text: &str) {
        self.harness.event(egui::Event::Text(text.to_string()));
        self.settle();
    }

    /// Press a key.
    pub fn press(&mut self, key: egui::Key) {
        self.harness.key_press(key);
        self.settle();
    }

    /// Drop a file on the window.
    pub fn drop_file(&mut self, file: &std::path::Path) {
        self.harness.input_mut().dropped_files.push(std::sync::Arc::new(Dropped(file.to_path_buf())));
        self.settle();
    }

    /// Undo the last step.
    #[track_caller]
    pub fn undo(&mut self) {
        assert!(self.harness.state_mut().undo(), "nothing to undo");
        self.settle();
    }

    /// A node's declaration, as written in the top layer that has it.
    #[track_caller]
    pub fn declaration(&self, path: &str) -> toml::Table {
        let text = std::fs::read_to_string(self.written(path)).unwrap_or_else(|e| panic!("{path}: {e}"));
        text.parse().unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    fn written(&self, path: &str) -> std::path::PathBuf {
        self.fixture.node_file(path)
    }

    /// A file with nothing to do with the project, as one to drop on the window is.
    pub fn file_elsewhere(&self, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let file = self.fixture.root().with_extension("files").join(name);
        std::fs::create_dir_all(file.parent().expect("a folder")).expect("a folder");
        std::fs::write(&file, bytes).expect("a file");
        file
    }

    /// The folder holding a node.
    #[must_use]
    pub fn node_folder(&self, path: &str) -> std::path::PathBuf {
        self.written(path).parent().expect("a bundle").to_path_buf()
    }

    /// What is wrong with the project, in a line each.
    pub fn problems(&mut self) -> Vec<String> {
        self.workspace().index.problems.iter().map(project::index::Problem::describe).collect()
    }
}
