//! The editor run with no window, a scenario at a time: what a person does, step by step, and what must have come of it.
//! A scenario is an ordinary test. It opens a project of its own (`Fixture`) and acts as the user does: the same pointer and
//! keys, found by what the window says, through egui's test harness and its accessibility tree. What it checks is what came
//! of that - what is written, the problems, what a window says - not pixels, which differ between GPUs.
//!
//! Work goes on off the window's thread, so that a harness with no repaint to make has not finished: `settle` waits until
//! the editor says it has nothing going on.

// What only the scenarios here use is left over in a build for running a project's tests.
#[cfg(test)]
mod documents;
#[cfg(test)]
mod edits;
#[cfg(test)]
mod fetching;
#[cfg_attr(not(test), allow(dead_code))]
mod fixture;
pub mod host;
#[cfg(test)]
mod inspector;
#[cfg(test)]
mod models;
#[cfg(test)]
mod nodes;
#[cfg(test)]
mod onboarding;
#[cfg(test)]
mod reading_again;
#[cfg(test)]
mod registered;
pub mod suite;
#[cfg(test)]
mod taking_in;
#[cfg(all(test, unix))]
mod tests_pane;

use std::fmt::Write as _;
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
/// The most frames waited for what is to be clicked to stay put: it has, long before.
const STEADY_FRAMES: usize = 60;
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
        extensions::test_mode(host::extension);
        project::ext::component::link_gpu(pe2_webgpu::add_to_linker);
        // A test reaches no network: what an extension would fetch, it is told is not there.
        project::ext::setup::forbid_downloads();
    });
}

/// Bytes as they cross as text.
fn from_hex(text: &str) -> Result<Vec<u8>, String> {
    let pairs = text
        .as_bytes()
        .chunks(2)
        .map(|pair| std::str::from_utf8(pair).ok().and_then(|pair| u8::from_str_radix(pair, 16).ok()));
    pairs.collect::<Option<Vec<u8>>>().ok_or_else(|| "not hex".to_string())
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

/// Run the tests of the extensions a project has, those whose names hold one of `filters`, in the project: whether they passed.
/// What a test changes is undone after it.
#[must_use]
pub fn run(root: &std::path::Path, filters: &[String]) -> bool {
    isolate();
    if !root.join(project::FILE).is_file() {
        eprintln!("{}: no project", root.display());
        return false;
    }
    let mut editor = Scenario::open_with(Fixture::given(root), None, true);
    let mut tests = editor.workspace().extensions.tests();
    tests.retain(|name| suite::wanted(name, filters));
    for problem in editor.workspace().extensions.problems.clone() {
        eprintln!("warning: {problem}");
    }
    let mut failed = 0;
    for name in &tests {
        let before = editor.workspace().history.done().len();
        let result = editor.run_test(name);
        while editor.workspace().history.done().len() > before {
            editor.undo();
        }
        match result {
            Ok(()) => println!("ok      {name}"),
            Err(why) => {
                failed += 1;
                println!("FAILED  {name}\n{why}");
            }
        }
    }
    println!("{} passed, {failed} failed", tests.len() - failed);
    failed == 0 && !tests.is_empty()
}

/// `pe2-test-runner`: what the process exits with.
#[must_use]
pub fn main() -> i32 {
    let mut args = std::env::args_os().skip(1).peekable();
    let (mut project, mut folders, mut filters, mut built) = (None, Vec::new(), Vec::new(), false);
    let mut out = suite::Out::default();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--project") => project = args.next().map(std::path::PathBuf::from),
            Some("--built") => built = true,
            Some("--suites") => {
                while let Some(folder) = args.next_if(|arg| !arg.to_string_lossy().starts_with("--")) {
                    folders.push(std::path::PathBuf::from(folder));
                }
            }
            Some("--filter") => filters.extend(args.next().map(|text| text.to_string_lossy().into_owned())),
            Some("--list") => out.list = true,
            Some("--json") => out.json = true,
            Some("--artifacts") => out.artifacts = args.next().map(std::path::PathBuf::from),
            Some("--trust-everything") => extensions::trust::trust_everything(),
            _ => {
                eprintln!("{}: not something this takes", arg.to_string_lossy());
                return 2;
            }
        }
    }
    if let Some(root) = project {
        return i32::from(!run(&root, &filters));
    }
    let mut suites = if built || folders.is_empty() { suite::built() } else { Vec::new() };
    match suite::in_folders(&folders) {
        Ok(found) => suites.extend(found),
        Err(why) => {
            eprintln!("{why}");
            return 2;
        }
    }
    if out.list {
        return i32::from(!suite::list(&suites, &out));
    }
    i32::from(!suite::run(&suites, &filters, &out).passed())
}

/// The editor at work on a fixture.
#[cfg_attr(not(test), allow(dead_code))]
pub struct Scenario {
    harness: Harness<'static, Editor>,
    fixture: Fixture,
    /// What lets the worker go, where it is held (`hold_reading`).
    held: Option<std::sync::mpsc::Sender<()>>,
    /// One at a time: a process has one set of extensions, keeping what they make in the project open.
    _alone: std::sync::MutexGuard<'static, ()>,
}

#[cfg_attr(not(test), allow(dead_code))]
impl Scenario {
    /// The editor on a fixture, its project open.
    #[track_caller]
    #[must_use]
    pub fn open(fixture: Fixture) -> Scenario {
        Scenario::open_showing(fixture, None)
    }

    /// The editor on a fixture, with a pane in front.
    #[track_caller]
    #[must_use]
    pub fn open_showing(fixture: Fixture, show: Option<&str>) -> Scenario {
        Scenario::open_with(fixture, show, false)
    }

    /// The editor on a fixture, with a pane in front, in a window of this size: how a pane lays itself out.
    #[track_caller]
    #[must_use]
    pub fn open_sized(fixture: Fixture, show: Option<&str>, size: egui::Vec2) -> Scenario {
        Scenario::open_in(fixture, show, false, size)
    }

    /// The editor on a fixture; `unattended`, with nobody there to answer what a project asks.
    #[track_caller]
    fn open_with(fixture: Fixture, show: Option<&str>, unattended: bool) -> Scenario {
        Scenario::open_in(fixture, show, unattended, SIZE)
    }

    /// The editor on a fixture in a window of a size; `unattended`, with nobody there to answer what a project asks.
    #[track_caller]
    fn open_in(fixture: Fixture, show: Option<&str>, unattended: bool, size: egui::Vec2) -> Scenario {
        static ALONE: std::sync::Mutex<()> = std::sync::Mutex::new(());
        // A scenario that failed says so itself: the next has nothing to do with it.
        let alone = ALONE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = fixture.root().to_path_buf();
        let show = show.map(str::to_string);
        let harness = Harness::builder().with_size(size).wgpu().build_eframe(move |cc| {
            if let Some(state) = &cc.wgpu_render_state {
                crate::install_gpu(state);
            }
            let start = Start {
                project: Some(root),
                select: None,
                show,
                screenshot: None,
                max_surface: None,
                agents_drive: false,
                unattended,
                monitors: Vec::new(),
            };
            Editor::new(cc, start)
        });
        let mut scenario = Scenario { harness, fixture, held: None, _alone: alone };
        scenario.settle();
        assert!(
            scenario.harness.state_mut().workspace().is_some(),
            "the project did not open: {:?}",
            scenario.harness.state().error()
        );
        scenario
    }

    /// Have the worker read until `release_reading`, as though the project were being read. What is asked meanwhile waits.
    ///
    /// # Errors
    /// It is held already.
    pub fn hold_reading(&mut self) -> Result<(), String> {
        if self.held.is_some() {
            return Err("it is held already".to_string());
        }
        self.held = Some(self.workspace().hold_worker());
        Ok(())
    }

    /// Let the worker go, and let what waited for it finish.
    ///
    /// # Errors
    /// It is not held.
    pub fn release_reading(&mut self) -> Result<(), String> {
        self.held.take().ok_or("it is not held")?;
        self.settle();
        Ok(())
    }

    /// Make an output of a node, as a view asking for it does, nothing kept: what went wrong, or nothing.
    ///
    /// # Errors
    /// There is no such node.
    pub fn made(&mut self, path: &str, address: &str) -> Result<String, String> {
        let ws = self.workspace();
        let asset = ws.index.assets.get(path).cloned().ok_or_else(|| format!("no asset {path}"))?;
        let engine = project::graph::exec::Engine::new(ws.extensions.operations(), None);
        let made = ws.extensions.output(&engine, &ws.index, &asset, address, false);
        Ok(made.err().map_or_else(String::new, |why| format!("{why:#}")))
    }

    /// Ask to override the entry at this path, as a layer's button does.
    ///
    /// # Errors
    /// There is no such entry.
    pub fn override_entry(&mut self, path: &str) -> Result<(), String> {
        let uid = self
            .workspace()
            .index
            .entries
            .get(path)
            .map(|entry| entry.uid.clone())
            .ok_or_else(|| format!("no entry {path}"))?;
        self.harness.state_mut().ask(&crate::workspace::EditRequest::Override(uid));
        self.settle();
        Ok(())
    }

    /// Type into the field showing this hint.
    #[track_caller]
    pub fn type_into(&mut self, hint: &str, text: &str) {
        let inputs = self.harness.query_all(By::new().role(egui::accesskit::Role::TextInput));
        let field = inputs.into_iter().find(|input| input.accesskit_node().placeholder() == Some(hint));
        field.unwrap_or_else(|| panic!("no field hinting {hint:?}")).click();
        self.settle();
        self.typing(text);
    }

    /// Frames until the editor has nothing going on: a few, while the worker is held, as it would never be.
    #[track_caller]
    pub fn settle(&mut self) {
        if self.held.is_some() {
            for _ in 0..QUIET {
                self.harness.step();
            }
            return;
        }
        let started = Instant::now();
        let mut quiet = 0;
        while quiet < QUIET {
            self.harness.input_mut().focused = true;
            self.harness.step();
            quiet = if self.harness.state().busy() { 0 } else { quiet + 1 };
            if started.elapsed() >= PATIENCE {
                let busy = self.harness.state().busy_with().join(", ");
                panic!(
                    "the editor did not settle in {}s, busy with {busy}; the window:\n{}",
                    PATIENCE.as_secs(),
                    self.shown().join("\n")
                );
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// One frame.
    #[track_caller]
    pub fn frame(&mut self) {
        self.harness.step();
    }

    /// Open an asset, as selecting it does.
    #[track_caller]
    pub fn show(&mut self, path: &str) {
        let ws = self.workspace();
        assert!(ws.index.assets.contains_key(path), "no asset {path}");
        ws.select(path.to_string());
        self.settle();
    }

    /// Frames until what is about to be clicked has stayed where it is for a few of them, as a pointer would find it.
    /// egui takes a click by where the widgets were the frame before, and a window just shown takes none in the first
    /// it is drawn (its sizing pass); a modal then centers itself by the size it found, and its contents may still move
    /// as they settle. Clicked earlier, a click lands where nothing is - more often the slower the machine.
    fn steady(&mut self, rect: impl Fn(&Scenario) -> Option<egui::Rect>) {
        let (mut last, mut same) = (None, 0);
        for _ in 0..STEADY_FRAMES {
            self.harness.step();
            let now = rect(self);
            same = if now.is_some() && now == last { same + 1 } else { 0 };
            if same >= 2 {
                return;
            }
            last = now;
        }
    }

    /// The widget saying this, the one there is: a scenario naming two things alike says so.
    #[track_caller]
    fn widget<'a>(&'a self, text: &'a str) -> Node<'a> {
        let mut found: Vec<Node<'_>> = self.harness.query_all(By::new().label_contains(text)).collect();
        match found.len() {
            1 => found.remove(0),
            0 => panic!("nothing says {text:?}"),
            many => panic!(
                "{many} things say {text:?}: {:?}",
                found.iter().filter_map(|node| node.accesskit_node().label()).collect::<Vec<_>>()
            ),
        }
    }

    /// Scroll what says this into view, as one scrolls down a pane to what is below it.
    #[track_caller]
    pub fn scroll_to(&mut self, text: &str) {
        self.widget(text).scroll_to_me();
        self.settle();
    }

    /// Click what says this: in view, as a click is.
    #[track_caller]
    pub fn click(&mut self, text: &str) {
        self.steady(|editor| editor.harness.query_all(By::new().label_contains(text)).next().map(|node| node.rect()));
        self.widget(text).click();
        self.settle();
    }

    /// Click the topmost of what says this: the first of a list's alike rows, not what the status bar says of it.
    #[track_caller]
    pub fn click_topmost(&mut self, text: &str) {
        self.steady(|editor| editor.harness.query_all(By::new().label_contains(text)).next().map(|node| node.rect()));
        let found = self.harness.query_all(By::new().label_contains(text));
        let top = found.min_by(|a, b| a.rect().top().total_cmp(&b.rect().top()));
        top.unwrap_or_else(|| panic!("nothing says {text:?}")).click();
        self.settle();
    }

    /// Click what says exactly this: what is open in a menu, say.
    #[track_caller]
    pub fn click_label(&mut self, text: &str) {
        self.steady(|editor| editor.harness.query_all(By::new().label(text)).next().map(|node| node.rect()));
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
            if started.elapsed() >= PATIENCE {
                // What there was instead: the extensions as they stood, and what the window showed.
                let extensions: Vec<String> =
                    self.workspace().extensions.present.iter().map(|p| format!("{} {:?}", p.id, p.status)).collect();
                panic!(
                    "{what} never came, in {}s; the extensions:\n{}\nthe window:\n{}",
                    PATIENCE.as_secs(),
                    extensions.join("\n"),
                    self.shown().join("\n")
                );
            }
            self.settle();
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// What another program changed taken in, as going back to the window does: it waits for that until then.
    #[track_caller]
    pub fn take_in_outside_changes(&mut self) {
        self.until("the change to be seen", |editor| editor.workspace().pending_changes() > 0);
        self.harness.input_mut().focused = false;
        self.harness.step();
        self.settle();
    }

    /// Run a test an extension registered: on a thread of its own, this one doing each step it asks for and drawing between.
    ///
    /// # Errors
    /// It failed, and why.
    pub fn run_test(&mut self, name: &str) -> Result<(), String> {
        let (steps, asked) = std::sync::mpsc::channel::<host::Step>();
        host::listen(Some(steps));
        let ws = self.workspace();
        let (extensions, index) = (ws.extensions.clone(), ws.index.clone());
        let name = name.to_string();
        let running = name.clone();
        let test = std::thread::spawn(move || {
            let lens = project::ext::Lens {
                index: &index,
                extensions: &extensions,
                selection: None,
                chosen: None,
                revision: 0,
            };
            let service = extensions.service(&name).ok_or_else(|| format!("no test {name}"))?;
            (service.run)(&lens, &toml::Value::Table(toml::Table::new())).map(drop).map_err(|why| format!("{why:#}"))
        });
        let started = Instant::now();
        while !test.is_finished() {
            match asked.recv_timeout(Duration::from_millis(5)) {
                Ok(step) => {
                    let done = self.do_step(&running, &step.name, &step.arguments);
                    let _ = step.done.send(done);
                }
                Err(_) => self.harness.step(),
            }
            assert!(started.elapsed() < PATIENCE, "the test did not end");
        }
        host::listen(None);
        test.join().map_err(|_| "the test panicked".to_string())?
    }

    /// A step a test asked for, and what it comes to.
    fn do_step(&mut self, test: &str, name: &str, arguments: &toml::Value) -> Result<toml::Value, String> {
        use toml::Value;
        let said = arguments.get("text").and_then(Value::as_str).unwrap_or_default();
        if name == "data" {
            return self.data_of(test, said).map(|bytes| {
                Value::String(bytes.iter().fold(String::new(), |mut out, byte| {
                    let _ = write!(out, "{byte:02x}");
                    out
                }))
            });
        }
        let strings = |list: Vec<String>| Value::Array(list.into_iter().map(Value::String).collect());
        let one =
            |found: usize, said: &str| if found == 1 { Ok(()) } else { Err(format!("{found} things say {said:?}")) };
        match name {
            "show" if !self.assets().iter().any(|path| path == said) => return Err(format!("no asset {said}")),
            "show" => self.show(said),
            "click" => {
                one(self.harness.query_all(By::new().label_contains(said)).count(), said)?;
                self.click(said);
            }
            "click_label" => {
                one(self.harness.query_all(By::new().label(said)).count(), said)?;
                self.click_label(said);
            }
            "typing" => self.typing(said),
            "press" => self.press(egui::Key::from_name(said).ok_or_else(|| format!("no key {said}"))?),
            "undo" => {
                if self.workspace().history.done().is_empty() {
                    return Err("nothing to undo".to_string());
                }
                self.undo();
            }
            "drop_file" => {
                let bytes = from_hex(arguments.get("bytes").and_then(Value::as_str).unwrap_or_default())?;
                let file = self.file_elsewhere(said, &bytes);
                self.drop_file(&file);
            }
            "node" => self.new_node(arguments)?,
            "make_every_template" => return self.make_every_template().map(Value::Integer),
            "expect" if !self.says(said) => {
                return Err(format!("nothing says {said:?}; there is:\n{}", self.shown().join("\n")));
            }
            "expect_not" if self.says(said) => {
                return Err(format!(
                    "something says {said:?}: {:?}",
                    self.shown().into_iter().filter(|line| line.contains(said)).collect::<Vec<_>>()
                ));
            }
            "expect" | "expect_not" => {}
            "problems" => return Ok(strings(self.problems())),
            "assets" => return Ok(strings(self.assets())),
            "selected" => {
                return self
                    .workspace()
                    .selected
                    .clone()
                    .map(Value::String)
                    .ok_or_else(|| "nothing is selected".to_string());
            }
            "override_entry" => self.override_entry(said)?,
            "declare" => {
                let given = |key: &str| arguments.get(key).and_then(Value::as_str).unwrap_or_default();
                self.workspace().make_node(given("uid"), said, (given("declaration"), &[]))?;
                self.settle();
            }
            "made" => {
                let address = arguments.get("address").and_then(Value::as_str).unwrap_or_default();
                return self.made(said, address).map(Value::String);
            }
            "hold_reading" => self.hold_reading()?,
            "release_reading" => self.release_reading()?,
            "pictured" => {
                let address = arguments.get("address").and_then(Value::as_str).unwrap_or_default();
                let ws = self.workspace();
                let asset = ws.index.assets.get(said).cloned().ok_or_else(|| format!("no asset {said}"))?;
                return Ok(Value::Boolean(
                    ws.extensions.output_image(&asset, address).is_some_and(|image| image.is_ok()),
                ));
            }
            other => return Err(format!("no step {other}")),
        }
        Ok(Value::Boolean(true))
    }

    /// What the window showed, kept where it can be looked at: a file of what it said, and a picture where one can be made.
    fn keep_failure(
        &mut self,
        folder: &std::path::Path,
        test: &str,
    ) -> (Option<std::path::PathBuf>, Option<std::path::PathBuf>) {
        let name: String = test.chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect();
        if std::fs::create_dir_all(folder).is_err() {
            return (None, None);
        }
        let said = folder.join(format!("{name}.txt"));
        let said = std::fs::write(&said, self.shown().join("\n")).is_ok().then_some(said);
        let picture = self.harness.render().ok().and_then(|image| {
            let png =
                codecs::texture::rgba_png(image.width() as usize, image.height() as usize, image.as_raw()).ok()?;
            let file = folder.join(format!("{name}.png"));
            std::fs::write(&file, png).is_ok().then_some(file)
        });
        (said, picture)
    }

    /// A file of the `data/` folder beside a test's component.
    fn data_of(&mut self, test: &str, name: &str) -> Result<Vec<u8>, String> {
        let id = test.split('@').next().unwrap_or_default();
        let present = self.workspace().extensions.present.iter().find(|present| present.id == id).cloned();
        let at = match present.map(|present| present.origin) {
            Some(project::ext::Origin::Project(folder)) => folder.join("data").join(name),
            Some(project::ext::Origin::Component(file)) => file.with_file_name("data").join(name),
            _ => return Err(format!("{id} has no folder")),
        };
        project::content::read(&at).map_err(|e| format!("{}: {e}", at.display()))
    }

    /// A node a test asked for, as its template makes one, in the active layer: a step of the history, so that it can go.
    fn new_node(&mut self, arguments: &toml::Value) -> Result<(), String> {
        let text = |key: &str| arguments.get(key).and_then(toml::Value::as_str).unwrap_or_default();
        let (path, uid, template, file, settings) =
            (text("path"), text("uid"), text("template"), text("file"), text("settings"));
        let mut files: Vec<(String, Vec<u8>)> = Vec::new();
        for (name, bytes) in arguments.get("files").and_then(toml::Value::as_table).into_iter().flatten() {
            files.push((name.clone(), from_hex(bytes.as_str().unwrap_or_default())?));
        }
        let ws = self.workspace();
        let found = ws.extensions.template(template).ok_or_else(|| format!("no template {template}"))?;
        let stack = ws.extensions.stamp(&found, file)?;
        let text = project::graph::layers::settings_on_layers(&format!("{settings}\n\n{stack}"))
            .map_err(|e| format!("{e:#}"))?;
        let declaration = format!("uid = \"{uid}\"\n\n{text}");
        let held: Vec<(&str, &[u8])> = files.iter().map(|(name, bytes)| (name.as_str(), bytes.as_slice())).collect();
        ws.make_node(uid, path, (&declaration, &held))?;
        self.settle();
        Ok(())
    }

    /// A node of every template that needs no file: how many.
    fn make_every_template(&mut self) -> Result<i64, String> {
        let ws = self.workspace();
        let made = ws
            .extensions
            .templates()
            .into_iter()
            .filter(|template| !template.stored() && ws.extensions.stamp(template, "").is_ok());
        let templates: Vec<String> = made.map(|template| template.name.clone()).collect();
        for template in &templates {
            self.workspace().new_node_now(template, None, "").map_err(|why| format!("{template}: {why}"))?;
            self.settle();
        }
        Ok(i64::try_from(templates.len()).unwrap_or(i64::MAX))
    }

    /// Whether the project is asking something.
    #[must_use]
    pub fn asking(&self) -> bool {
        self.harness.state().asking()
    }

    /// What became of an extension.
    #[track_caller]
    pub fn status(&mut self, id: &str) -> project::ext::Status {
        let present = self.workspace().extensions.present.iter().find(|present| present.id == id).cloned();
        present.unwrap_or_else(|| panic!("no extension {id}")).status
    }

    /// Whether the project is being opened, the panes waiting for it.
    pub fn is_opening(&self) -> bool {
        self.harness.state().is_opening()
    }

    /// The project, its session over.
    #[must_use]
    pub fn into_fixture(self) -> Fixture {
        let Scenario { harness, fixture, _alone: alone, .. } = self;
        drop(harness);
        drop(alone);
        fixture
    }

    /// Open the same project again, in a new session.
    #[track_caller]
    #[must_use]
    pub fn opened_again(self) -> Scenario {
        Scenario::open(self.into_fixture())
    }

    /// What the project's `project.toml` says.
    #[must_use]
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
        all.filter_map(|node| node.accesskit_node().label().or_else(|| node.accesskit_node().value()))
            .filter(|text| !text.trim().is_empty())
            .collect()
    }

    /// Whether something says this.
    #[must_use]
    pub fn says(&self, text: &str) -> bool {
        self.harness.query_all(By::new().label_contains(text).include_labels()).next().is_some()
    }

    /// Of the numbers in the row under a label, this one (from 0), clicked and so being typed in; what it shows now.
    #[track_caller]
    pub fn number(&mut self, row: &str, at: usize) -> String {
        let under = self
            .harness
            .query_all(By::new().label(row).include_labels())
            .next()
            .unwrap_or_else(|| panic!("no row {row:?}"))
            .rect();
        let mut numbers: Vec<Node<'_>> = self
            .harness
            .query_all(By::new().role(egui::accesskit::Role::SpinButton))
            .filter(|node| node.rect().top() >= under.top())
            .collect();
        numbers.sort_by(|a, b| {
            (a.rect().top(), a.rect().left()).partial_cmp(&(b.rect().top(), b.rect().left())).expect("a place")
        });
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

    /// Press a key with Alt held.
    pub fn alt(&mut self, key: egui::Key) {
        self.harness.key_press_modifiers(egui::Modifiers::ALT, key);
        self.settle();
    }

    /// Press a key with the command key held (Ctrl, ⌘): a shortcut.
    pub fn shortcut(&mut self, key: egui::Key) {
        // As a keyboard on Linux says it: Ctrl, which is the command key there.
        self.harness.key_press_modifiers(egui::Modifiers { ctrl: true, command: true, ..egui::Modifiers::NONE }, key);
        self.settle();
    }

    /// Drop a file on the window.
    pub fn drop_file(&mut self, file: &std::path::Path) {
        self.harness.input_mut().dropped_files.push(std::sync::Arc::new(Dropped(file.to_path_buf())));
        self.settle();
    }

    /// The harness's editor, for what a scenario does that no click does.
    #[cfg(test)]
    pub fn editor(&mut self) -> &mut crate::app::Editor {
        self.harness.state_mut()
    }

    /// Undo the last step.
    #[track_caller]
    pub fn undo(&mut self) {
        assert!(self.harness.state_mut().undo(), "nothing to undo");
        self.settle();
    }

    /// A node's declaration as the editor shows it: its working copy's, where it has unsaved changes.
    #[track_caller]
    pub fn working(&mut self, path: &str) -> toml::Table {
        let text = self.workspace().index.assets.get(path).map(|a| a.text().to_string());
        let text = text.unwrap_or_else(|| panic!("{path}: no such asset"));
        text.parse().unwrap_or_else(|e| panic!("{path}: {e}"))
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
