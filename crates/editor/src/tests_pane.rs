//! The Tests pane: the tests of what the project has, run by `pe2-test-runner`, which reports each as it goes.
//!
//! They run in a process of their own, not in the editor: what is in the editor has no harness, a test that hangs or crashes
//! is the runner's to lose, and each runs in a project of its own, so that the one open here stays as it is. With no runner
//! beside the editor, which a build that ships has not, the pane says so and that is all.

use std::collections::BTreeMap;
use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use eframe::egui::{self, RichText};
use project::ext::Origin;

use crate::workspace::Workspace;
use crate::{icons, theme};

/// How a test stands.
#[derive(Clone, Copy, Debug, PartialEq)]
enum State {
    /// Known of, and not run.
    Idle,
    Waiting,
    Running,
    Passed(f64),
    Failed(f64),
}

struct Test {
    name: String,
    state: State,
    /// Why it failed.
    why: String,
    /// The files of what the window showed.
    said: Option<PathBuf>,
    picture: Option<PathBuf>,
}

/// The runner at work.
struct Job {
    child: Child,
    lines: Receiver<String>,
    errors: Receiver<String>,
}

/// What the pane knows.
#[derive(Default)]
pub struct Tests {
    /// Set by what tests the pane; otherwise it is looked for beside the editor.
    runner: Option<PathBuf>,
    suites: BTreeMap<String, Vec<Test>>,
    job: Option<Job>,
    asked: bool,
    selected: Option<String>,
    filter: String,
    /// What the runner said that it should not have.
    trouble: Option<String>,
    /// The picture of the failure selected.
    shown: Option<(PathBuf, egui::TextureHandle)>,
}

impl Drop for Tests {
    fn drop(&mut self) {
        self.stop();
    }
}

/// A test's name without its extension and `test.`.
fn short(name: &str) -> &str {
    name.rsplit_once("test.").map_or(name, |(_, short)| short)
}

impl Tests {
    /// Use this runner, and ask it what there is.
    #[cfg(any(test, feature = "tests"))]
    pub fn use_runner(&mut self, runner: PathBuf) {
        self.runner = Some(runner);
        self.asked = false;
    }

    /// Whether a failure's picture is being shown.
    #[cfg(any(test, feature = "tests"))]
    #[must_use]
    pub fn showing_picture(&self) -> bool {
        self.shown.is_some()
    }

    /// Whether the runner is at work.
    #[must_use]
    pub fn running(&self) -> bool {
        self.job.is_some()
    }

    fn runner(&self) -> Option<PathBuf> {
        let found = || {
            let beside = std::env::current_exe()
                .ok()?
                .with_file_name(format!("pe2-test-runner{}", std::env::consts::EXE_SUFFIX));
            beside.is_file().then_some(beside)
        };
        self.runner.clone().or_else(found)
    }

    /// Start the runner with these arguments, what it reports read as it comes.
    fn start(&mut self, ctx: &egui::Context, runner: &Path, arguments: &[String]) {
        self.stop();
        self.trouble = None;
        let mut command = Command::new(runner);
        command.args(arguments).arg("--json").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(why) => {
                self.trouble = Some(format!("{}: {why}", runner.display()));
                return;
            }
        };
        let (lines, errors) = (reading(child.stdout.take(), ctx), reading(child.stderr.take(), ctx));
        self.job = Some(Job { child, lines, errors });
    }

    fn stop(&mut self) {
        if let Some(mut job) = self.job.take() {
            let _ = job.child.kill();
            let _ = job.child.wait();
        }
        for test in self.suites.values_mut().flatten() {
            if matches!(test.state, State::Waiting | State::Running) {
                test.state = State::Idle;
            }
        }
    }

    /// What the runner said since the last frame.
    fn take_in(&mut self) {
        let Some(job) = &mut self.job else { return };
        let mut said = Vec::new();
        let ended = loop {
            match job.lines.try_recv() {
                Ok(line) => said.push(line),
                Err(TryRecvError::Empty) => break false,
                Err(TryRecvError::Disconnected) => break true,
            }
        };
        let errors: Vec<String> = job
            .errors
            .try_iter()
            .filter(|line| !line.starts_with("MESA") && !line.contains("dzn") && !line.starts_with("TU:"))
            .collect();
        for line in said {
            self.heard(&line);
        }
        if !errors.is_empty() {
            let known = self.trouble.take().unwrap_or_default();
            self.trouble = Some(format!("{known}{}\n", errors.join("\n")));
        }
        if ended {
            let status = self.job.take().and_then(|mut job| job.child.wait().ok());
            // One that stopped before it said it was done says nothing of what it did not reach.
            for test in self.suites.values_mut().flatten() {
                if matches!(test.state, State::Waiting | State::Running) {
                    test.state = State::Idle;
                }
            }
            if status.is_some_and(|status| status.code().is_none()) {
                self.trouble.get_or_insert_with(|| "the runner was stopped".to_string());
            }
        }
    }

    /// A line of the runner's.
    fn heard(&mut self, line: &str) {
        let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else { return };
        let field = |key: &str| event.get(key).and_then(serde_json::Value::as_str).unwrap_or_default().to_string();
        let path = |key: &str| event.get(key).and_then(serde_json::Value::as_str).map(PathBuf::from);
        let seconds = event.get("seconds").and_then(serde_json::Value::as_f64).unwrap_or_default();
        let (suite, name) = (field("suite"), field("name"));
        // What is not of a test, as `done` is.
        if name.is_empty() {
            return;
        }
        if field("event") == "test" {
            let tests = self.suites.entry(suite).or_default();
            if !tests.iter().any(|test| test.name == name) {
                tests.push(Test { name, state: State::Idle, why: String::new(), said: None, picture: None });
            }
            return;
        }
        let tests = self.suites.entry(suite).or_default();
        let at = tests.iter().position(|test| test.name == name).unwrap_or_else(|| {
            tests.push(Test { name, state: State::Idle, why: String::new(), said: None, picture: None });
            tests.len() - 1
        });
        let test = &mut tests[at];
        match field("event").as_str() {
            "started" => test.state = State::Running,
            "passed" => test.state = State::Passed(seconds),
            "failed" => {
                test.state = State::Failed(seconds);
                test.why = field("why");
                (test.said, test.picture) = (path("said"), path("picture"));
            }
            _ => {}
        }
    }

    /// Run tests, by name: all with none named.
    fn run(&mut self, ctx: &egui::Context, runner: &Path, mut arguments: Vec<String>, names: &[String]) {
        for test in self.suites.values_mut().flatten() {
            if names.is_empty() || names.contains(&test.name) {
                *test = Test {
                    name: std::mem::take(&mut test.name),
                    state: State::Waiting,
                    why: String::new(),
                    said: None,
                    picture: None,
                };
            }
        }
        self.shown = None;
        for name in names {
            arguments.extend(["--filter".to_string(), name.clone()]);
        }
        // What the last run left goes: only what a test still shows is of use.
        let artifacts = std::env::temp_dir().join("pe2-test-failures");
        if names.is_empty() {
            let _ = std::fs::remove_dir_all(&artifacts);
        }
        arguments.extend(["--artifacts".to_string(), artifacts.display().to_string()]);
        self.start(ctx, runner, &arguments);
    }

    fn failed(&self) -> Vec<String> {
        self.suites
            .values()
            .flatten()
            .filter(|test| matches!(test.state, State::Failed(_)))
            .map(|test| test.name.clone())
            .collect()
    }
}

/// The lines of what a process writes, as they come.
fn reading(from: Option<impl std::io::Read + Send + 'static>, ctx: &egui::Context) -> Receiver<String> {
    let (lines, read) = channel();
    let ctx = ctx.clone();
    if let Some(from) = from {
        std::thread::spawn(move || {
            for line in BufReader::new(from).lines().map_while(Result::ok) {
                if lines.send(line).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
            ctx.request_repaint();
        });
    }
    read
}

/// What to give the runner for what this project has: the built-ins' tests and those beside its extensions.
fn suites_of(ws: &Workspace) -> Vec<String> {
    let mut arguments = vec!["--built".to_string()];
    // Those the project holds: what is kept to be added is not tested here.
    let folders: Vec<String> = ws
        .extensions
        .present
        .iter()
        .filter_map(|present| match &present.origin {
            Origin::Project(folder) => Some(folder.clone()),
            _ => None,
        })
        .filter(|folder| project::pack::is_file(&folder.join("tests").join("tests.wasm")))
        .map(|folder| folder.display().to_string())
        .collect();
    if !folders.is_empty() {
        arguments.push("--suites".to_string());
        arguments.extend(folders);
    }
    arguments
}

/// A failure's picture, as a texture.
fn picture(ctx: &egui::Context, file: &Path) -> Option<egui::TextureHandle> {
    let decoded = codecs::ora::decode_png(&std::fs::read(file).ok()?).ok()?;
    let codecs::ora::Pixels::Rgba(pixels) = decoded.pixels else { return None };
    let bytes: Vec<u8> = pixels.into_iter().flatten().collect();
    let image = egui::ColorImage::from_rgba_unmultiplied([decoded.width, decoded.height], &bytes);
    Some(ctx.load_texture("failure", image, egui::TextureOptions::LINEAR))
}

/// A test's row of the grid; whether it asked to run, and whether it was clicked.
fn row(ui: &mut egui::Ui, test: &Test, selected: bool, can_run: bool, names: f32) -> (bool, bool) {
    let (icon, color) = match test.state {
        State::Passed(_) => (icons::CHECK_CIRCLE, theme::accent()),
        State::Failed(_) => (icons::X_CIRCLE, ui.visuals().error_fg_color),
        State::Running => (icons::PLAY, ui.visuals().warn_fg_color),
        State::Waiting | State::Idle => (icons::PLAY, theme::weak()),
    };
    ui.label(RichText::new(icon).color(color));
    // As wide as the widest name of every suite, so that the columns after it line up from one suite to the next.
    let clicked = ui.scope(|ui| {
        ui.set_min_width(names);
        ui.selectable_label(selected, short(&test.name)).clicked()
    });
    let clicked = clicked.inner;
    match test.state {
        State::Passed(seconds) | State::Failed(seconds) => {
            drop(ui.label(RichText::new(format!("{seconds:.1} s")).small().color(theme::weak())));
        }
        _ => drop(ui.label("")),
    }
    let run = can_run && ui.small_button("Run").on_hover_text("Run this test").clicked();
    ui.end_row();
    (run, clicked)
}

/// The pane.
pub fn tests_ui(ui: &mut egui::Ui, ws: &mut Workspace) {
    let ctx = ui.ctx().clone();
    ws.tests.take_in();
    let Some(runner) = ws.tests.runner() else {
        ui.label(RichText::new("There is no pe2-test-runner beside the editor.").color(theme::weak()));
        ui.label(RichText::new("cargo build --release -p editor --features tests --bin pe2-test-runner").code());
        return;
    };
    // Built apart from the editor (`--features tests`), the runner can be left behind it, and then speaks an older contract
    // to the extensions than they do.
    let modified = |path: &Path| std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let behind =
        std::env::current_exe().ok().and_then(|editor| Some(modified(&runner)? < modified(&editor)?)).unwrap_or(false);
    let arguments = suites_of(ws);
    if !ws.tests.asked {
        ws.tests.asked = true;
        let mut listing = arguments.clone();
        listing.push("--list".to_string());
        ws.tests.start(&ctx, &runner, &listing);
    }
    let busy = ws.tests.job.is_some();
    let failed = ws.tests.failed();
    let mut asked: Option<Vec<String>> = None;
    ui.horizontal(|ui| {
        if ui.add_enabled(!busy, egui::Button::new("Run all")).clicked() {
            asked = Some(Vec::new());
        }
        if ui.add_enabled(!busy && !failed.is_empty(), egui::Button::new("Run failed")).clicked() {
            asked = Some(failed.clone());
        }
        if ui.add_enabled(busy, egui::Button::new("Stop")).clicked() {
            ws.tests.stop();
        }
        if busy {
            ui.add(egui::Spinner::new().size(12.0));
        }
        ui.add(egui::TextEdit::singleline(&mut ws.tests.filter).hint_text("Filter tests").desired_width(180.0));
    });
    if behind {
        theme::warning(
            ui,
            "The test runner is older than the editor, and may not load what the editor does. Build it again:",
        );
        ui.label(RichText::new("cargo build --release -p editor --features tests --bin pe2-test-runner").code());
    }
    if let Some(trouble) = &ws.tests.trouble {
        theme::warning(ui, trouble.trim_end());
    }
    let filter = ws.tests.filter.to_lowercase();
    let font = egui::TextStyle::Button.resolve(ui.style());
    let names = ws.tests.suites.values().flatten().map(|test| {
        ui.fonts_mut(|fonts| {
            fonts.layout_no_wrap(short(&test.name).to_string(), font.clone(), egui::Color32::WHITE).size().x
        })
    });
    let names = names.fold(0.0, f32::max) + 2.0 * ui.spacing().button_padding.x;
    let selected = ws.tests.selected.clone();
    let mut chosen = None;
    egui::ScrollArea::vertical().auto_shrink([false, true]).max_height(ui.available_height() * 0.6).show(ui, |ui| {
        if ws.tests.suites.is_empty() && !busy {
            ui.label(RichText::new("No tests were found").color(theme::weak()));
        }
        for (suite, tests) in &ws.tests.suites {
            let passed = tests.iter().filter(|test| matches!(test.state, State::Passed(_))).count();
            let failed = tests.iter().filter(|test| matches!(test.state, State::Failed(_))).count();
            let ran = tests.iter().any(|test| matches!(test.state, State::Passed(_) | State::Failed(_)));
            let said = if failed > 0 {
                format!("{suite}  ·  {failed} failed, {passed} passed")
            } else if ran {
                format!("{suite}  ·  {passed} of {} passed", tests.len())
            } else {
                format!("{suite}  ·  {} test{}", tests.len(), if tests.len() == 1 { "" } else { "s" })
            };
            egui::CollapsingHeader::new(said).id_salt(suite).default_open(true).show(ui, |ui| {
                egui::Grid::new(("tests", suite)).num_columns(4).spacing([10.0, 4.0]).min_col_width(0.0).show(
                    ui,
                    |ui| {
                        for test in tests.iter().filter(|test| test.name.to_lowercase().contains(&filter)) {
                            let (run, clicked) =
                                row(ui, test, selected.as_deref() == Some(test.name.as_str()), !busy, names);
                            if run {
                                asked = Some(vec![test.name.clone()]);
                            }
                            if clicked {
                                chosen = Some(test.name.clone());
                            }
                        }
                    },
                );
            });
        }
    });
    if let Some(chosen) = chosen {
        ws.tests.selected = Some(chosen);
        ws.tests.shown = None;
    }
    if let Some(names) = asked {
        ws.tests.run(&ctx, &runner, arguments, &names);
    }
    details(ui, ws);
}

/// What is known of the test selected.
fn details(ui: &mut egui::Ui, ws: &mut Workspace) {
    let selected = ws.tests.selected.clone();
    let test = ws.tests.suites.values().flatten().find(|test| Some(&test.name) == selected.as_ref());
    let Some(test) = test.filter(|test| matches!(test.state, State::Failed(_))) else { return };
    let (why, said, file) = (test.why.clone(), test.said.clone(), test.picture.clone());
    ui.separator();
    egui::ScrollArea::vertical().id_salt("test failure").auto_shrink(false).show(ui, |ui| {
        ui.label(RichText::new(short(selected.as_deref().unwrap_or_default())).strong());
        ui.label(RichText::new(why).monospace().color(ui.visuals().error_fg_color));
        if let Some(file) = file {
            if ws.tests.shown.as_ref().is_none_or(|(known, _)| *known != file) {
                ws.tests.shown = picture(ui.ctx(), &file).map(|texture| (file.clone(), texture));
            }
            if let Some((_, texture)) = &ws.tests.shown {
                let width = ui.available_width().min(texture.size_vec2().x);
                ui.add(egui::Image::new(texture).max_width(width));
            }
        }
        if let Some(text) = said.and_then(|file| std::fs::read_to_string(file).ok()) {
            ui.collapsing("What the window said", |ui| ui.label(RichText::new(text).small().monospace()));
        }
    });
}
