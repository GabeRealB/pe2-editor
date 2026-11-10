//! The editor's window: a menu bar and a toolbar on top, the panes in a tile layout, a status bar below; and the
//! windows panes were moved into (`windows.rs`). The layouts, the windows and each pane instance's state are
//! remembered in eframe's storage, the editor's settings in their own `settings.toml`.

use eframe::egui::{self, RichText};
use egui_tiles::{Behavior as _, TileId};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::commands::{Binding, Command};
use crate::panes::{self, EditorKind, Pane, PaneAction, PaneKind, PaneState, Panes};
use crate::settings::EditorSettings;
use crate::windows::{Layouts, Window};
use crate::workspace::{EditRequest, GraphAsk, Update, Workspace};
use crate::{icons, theme};
use pe2_ext::events::{Build, Clean, ConfigChanged, Event};
use project::history::StepError;

/// What eframe and earlier versions stored and nothing reads any more. eframe's window goes on being applied, whether or not it
/// is told to persist one: left there, it maximizes every window it opens.
const OLD_KEYS: [&str; 3] = ["window", "layout", "sidebar"];
const LAYOUT_KEY: &str = "main layout";
const MODES_KEY: &str = "modes";
const MODE_KEY: &str = "mode";
const WINDOWS_KEY: &str = "windows";
const PANES_KEY: &str = "panes";
const HOMES_KEY: &str = "homes";
const SETTINGS_KEY: &str = "settings";
/// How long the status bar says an agent is using the editor after it last acted.
const AGENT_ACTED: std::time::Duration = std::time::Duration::from_secs(5);

/// The palette while it is up: what is typed, and the row marked.
#[derive(Default)]
struct Palette {
    typed: String,
    at: usize,
}

/// Frames to wait before a screenshot, so the window has its size and the selection has scrolled into view.
const CAPTURE_AFTER_FRAMES: u32 = 30;
/// The monitor height a zoom of 1 is made for.
const REFERENCE_HEIGHT: f32 = 1080.0;
/// Frames the scale must hold before the monitor's size is read.
const STEADY_FRAMES: u32 = 2;

/// The settings eframe's storage held before `settings.toml`: taken over once.
#[derive(Default, Deserialize)]
struct OldSettings {
    zoom: Option<f32>,
    last_project: Option<PathBuf>,
}

/// A screenshot asked for on the command line: taken once the window has settled, then the editor exits.
struct Capture {
    path: PathBuf,
    frames: u32,
    requested: bool,
    /// Whether the image is still to be saved (1) or saved (0).
    pending: Arc<std::sync::atomic::AtomicUsize>,
}

/// What was asked while the project was being read, done once it has been.
enum Deferred {
    Edit(EditRequest),
    Step { redo: bool, force: bool, scope: Option<String> },
}

/// What waits on the documents with unsaved changes being saved or let go.
#[derive(Clone, Debug)]
enum Closing {
    /// The window, closing.
    Quit,
    /// Another project, opening.
    Open(PathBuf),
    /// A build or a clean, by its event.
    Signal(&'static str),
    /// A document's last tab: where it is, and the document's uid.
    Tab { window: Option<u64>, tile: TileId, uid: String },
}

/// A project being opened on a thread of its own: how far it is, and the workspace's start once it is.
struct Opening {
    dir: PathBuf,
    began: std::time::Instant,
    progress: Arc<std::sync::Mutex<(String, usize, usize)>>,
    ready: std::sync::mpsc::Receiver<anyhow::Result<crate::workspace::Opened>>,
    /// Agents' requests, from the link the opening starts as it takes the project's lock: answered as it opens - what
    /// they ask of the window as ever, what they ask of the project with how far it is - then the workspace's.
    agents: std::sync::mpsc::Receiver<crate::workspace::AgentRequest>,
    /// Shown once it is open: the selection kept across a reopen, or asked on the command line.
    select: Option<String>,
}

#[expect(clippy::struct_excessive_bools, reason = "each answers for itself")]
pub struct Editor {
    workspace: Option<Workspace>,
    /// What waits for the job of its ticket, in the order it was asked.
    deferred: Vec<(u64, Deferred)>,
    /// A project being opened, off the window's thread.
    opening: Option<Opening>,
    /// The window's context: what a project being opened wakes.
    ctx: egui::Context,
    /// The main window's layout, the other windows', and each pane instance's state.
    layouts: Layouts,
    /// A window to bring to the front once it is drawn.
    focus: Option<egui::ViewportId>,
    settings: EditorSettings,
    /// The settings as last saved: a change is saved, and a new appearance applied, at the end of the frame.
    saved: EditorSettings,
    /// What the last action did, for the status bar.
    message: Option<String>,
    /// The zoom the editor last set, to tell a zoom the user made with the keyboard.
    applied_zoom: Option<f32>,
    /// The monitor height (in pixels) the automatic zoom was last made for: it is made again on another monitor.
    zoomed_for: Option<f32>,
    /// The pixels per point of the last frames, and for how many frames they have not changed: egui reports the
    /// monitor's size as measured with the previous frame's scale, so a size is read only once the scale holds.
    steady: (f32, u32),
    open_dialog: Option<String>,
    /// The directory a new project is to be made in, being typed.
    new_dialog: Option<String>,
    /// A step that what changed outside the editor blocks: redo (true) or undo, of which document, and why.
    force: Option<(bool, Option<String>, String)>,
    /// What waits on unsaved documents being saved or let go, while it is asked.
    closing: Option<Closing>,
    /// The window closing, its documents saved or let go: it closes unasked.
    quitting: bool,
    /// An agent's command is being run: what it does, an agent does.
    agent_running: bool,
    /// An export being chosen, while it is.
    exporting: Option<Exporting>,
    /// Exports being made, off the window's thread: what each is, and what came of it once it has.
    exports: Vec<(String, std::sync::mpsc::Receiver<Result<String, String>>)>,
    /// An entry being renamed: its uid, its real path, the new name being typed.
    renaming: Option<(String, String, String)>,
    /// Code being overridden while an asset layer is active: which files layer takes it.
    choosing: Option<Choosing>,
    layers_state: crate::layers::LayersState,
    /// A project that could not be opened because another process holds it: its directory and who holds it.
    in_use: Option<(PathBuf, String)>,
    /// The selection last opened as a document.
    opened: Option<String>,
    /// Files being imported: their paths, a line each.
    import: Option<String>,
    /// Whether the assets are up in their drawer.
    drawer: bool,
    /// The panes put away to leave the documents alone, while they are.
    bare: Option<Vec<TileId>>,
    /// A new mode being named.
    naming: Option<String>,
    /// The palette, while it is up.
    palette: Option<Palette>,
    /// Whether the pointer is over the assets' browser: back and forward are then its own.
    in_browser: bool,
    /// The documents brought into focus, oldest first, and which of them back and forward stand at.
    visits: (Vec<Visit>, usize),
    /// What reads where the main window is, and the place it was last seen in.
    tracker: crate::windows::Tracker,
    place: Option<crate::windows::Place>,
    /// Whether it fills the screen now: maximized, in full screen.
    filling: (bool, bool),
    /// How it filled the screen when it was left, until it is back where it was.
    refill: Option<(bool, bool)>,
    /// What eframe stored of the window, applied to this one and to be undone.
    stored_window: Option<String>,
    error: Option<String>,
    capture: Option<Capture>,
    /// The device's largest surface side in pixels, which the window must not outgrow; applied once, then taken.
    max_surface: Option<u32>,
    /// What the project opened asks of whoever opened it.
    onboarding: Option<crate::onboarding::Onboarding>,
    /// The project told "not now": this session leaves it alone.
    not_now: Option<PathBuf>,
    /// Nobody is there to ask.
    unattended: bool,
    /// Agents using the window, and whether this session lets them act whatever the settings say.
    drive: crate::drive::Driver,
    /// What it says of itself to whoever lists the editors on this computer (`mcp::presence`).
    presence: Option<mcp::presence::Keeper>,
    agents_drive: bool,
}

pub struct Start {
    pub project: Option<PathBuf>,
    pub select: Option<String>,
    pub show: Option<String>,
    pub screenshot: Option<PathBuf>,
    pub max_surface: Option<u32>,
    /// Agents may act in the editor this session, whatever the settings say.
    pub agents_drive: bool,
    /// Nobody is there: a project is not to ask anything.
    pub unattended: bool,
    /// The monitors, in pixels of the desktop: what a window's place is judged by.
    pub monitors: Vec<egui::Rect>,
}

/// How long a project takes to open before the window says so over its panes.
const CARD_AFTER: std::time::Duration = std::time::Duration::from_millis(500);

/// An epoch running, in the status bar, and what stops it.
/// What extensions fetch beside the editor, in the status bar: whether it was clicked, for the Extensions pane.
fn fetch_status(ui: &mut egui::Ui, ws: &Workspace) -> bool {
    let Some((said, failed)) = ws.fetcher.status() else { return false };
    let color = if failed { ui.visuals().warn_fg_color } else { theme::weak() };
    let label = egui::Label::new(RichText::new(said).small().color(color)).sense(egui::Sense::click());
    let shown = ui.add(label).on_hover_cursor(egui::CursorIcon::PointingHand);
    let clicked = shown.on_hover_text("What extensions fetch: each in its Fetches, in the Extensions pane").clicked();
    if !failed {
        ui.add(egui::Spinner::new().size(10.0));
    }
    ui.separator();
    clicked
}

fn epoch_status(ui: &mut egui::Ui, ws: &Workspace) {
    if !ws.in_epoch() {
        return;
    }
    ui.label(RichText::new(ws.epoch_doing()).small().color(theme::weak())).on_hover_text(
        "What is wired to the events signaled runs: a build, or what follows files changing (the Events pane)",
    );
    ui.add(egui::Spinner::new().size(10.0));
    let cancel = ui.add(egui::Button::new(RichText::new("Cancel").small()).frame(false));
    if cancel.on_hover_text("Stop once what is running now is done").clicked() {
        ws.cancel_epoch();
    }
    ui.separator();
}

/// The config in use for a project: the one chosen, else its first.
fn active_config(settings: &EditorSettings, project: &project::Project) -> Option<project::Config> {
    let chosen = settings.active_configs.get(&project.root.display().to_string());
    let found = chosen.and_then(|name| project.configs.iter().find(|config| config.name == *name));
    found.or(project.configs.first()).cloned()
}

impl Editor {
    #[expect(clippy::too_many_lines, reason = "what it starts with, one line each")]
    pub fn new(cc: &eframe::CreationContext<'_>, start: Start) -> Editor {
        let stored = |key| cc.storage.and_then(|s| s.get_string(key));
        let layout =
            stored(LAYOUT_KEY).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_else(panes::default_layout);
        let windows: Vec<Window> = stored(WINDOWS_KEY).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let states: Vec<(Pane, PaneState)> =
            stored(PANES_KEY).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let homes: Vec<(Pane, TileId)> =
            stored(HOMES_KEY).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let modes: Vec<crate::windows::Mode> =
            stored(MODES_KEY).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_else(crate::windows::first_modes);
        let mode = stored(MODE_KEY).and_then(|s| s.parse().ok()).filter(|at| *at < modes.len()).unwrap_or(0);
        let (mut settings, problem) = EditorSettings::load();
        // A place on a monitor no longer there is no place: the platform chooses, and the windows of its own open anew.
        let window = settings.main_window.filter(|window| window.place.is_reachable(&start.monitors));
        let window_lost = settings.main_window.is_some() && window.is_none();
        let mut windows = windows;
        for window in &mut windows {
            window.place = window.place.filter(|place| place.is_reachable(&start.monitors));
        }
        if crate::settings::path().is_some_and(|p| !p.exists())
            && let Some(old) = stored(SETTINGS_KEY).and_then(|s| serde_json::from_str::<OldSettings>(&s).ok())
        {
            settings.appearance.zoom = old.zoom;
            settings.last_project = old.last_project;
        }
        // Windows inside the main one rather than native ones: where native windows misbehave, and for screenshots,
        // which eframe takes of the main window only.
        if std::env::var_os("PE2_EDITOR_EMBED_WINDOWS").is_some() {
            cc.egui_ctx.set_embed_viewports(true);
        }
        icons::install(&cc.egui_ctx);
        theme::apply(&cc.egui_ctx, &settings.appearance);
        let last = settings.startup.reopen_last_project.then(|| settings.last_project.clone()).flatten();
        let project = start.project.or(last);
        let mut editor = Editor {
            workspace: None,
            deferred: Vec::new(),
            opening: None,
            ctx: cc.egui_ctx.clone(),
            layouts: Layouts {
                main: layout,
                windows,
                states: states.into_iter().collect(),
                homes: homes.into_iter().collect(),
                focused: None,
                reveal: None,
                modes,
                mode,
            },
            focus: None,
            saved: settings.clone(),
            settings,
            message: window_lost.then(|| "The monitor the window was on is gone: it opened on another".to_string()),
            applied_zoom: None,
            zoomed_for: None,
            steady: (0.0, 0),
            open_dialog: None,
            new_dialog: None,
            force: None,
            closing: None,
            quitting: false,
            exporting: None,
            exports: Vec::new(),
            agent_running: false,
            renaming: None,
            choosing: None,
            layers_state: crate::layers::LayersState::default(),
            in_use: None,
            opened: None,
            import: None,
            drawer: false,
            bare: None,
            naming: None,
            palette: None,
            in_browser: false,
            visits: (Vec::new(), 0),
            tracker: crate::windows::Tracker::new(window.map(|window| window.place)),
            place: window.map(|window| window.place),
            filling: window.map_or((false, false), |window| (window.maximized, window.fullscreen)),
            refill: window.map(|window| (window.maximized, window.fullscreen)),
            stored_window: stored("window").filter(|text| !text.is_empty()),
            error: problem,
            capture: start.screenshot.map(|path| Capture {
                path,
                frames: 0,
                requested: false,
                pending: Arc::default(),
            }),
            max_surface: start.max_surface,
            onboarding: None,
            not_now: None,
            unattended: start.unattended,
            drive: crate::drive::Driver::new(),
            presence: mcp::presence::Keeper::start(),
            agents_drive: start.agents_drive,
        };
        if let Some(dir) = project {
            editor.open(&dir);
        }
        // Shown once the project is open.
        if let Some(opening) = &mut editor.opening {
            opening.select = start.select;
        }
        if let Some(kind) = start
            .show
            .and_then(|name| PaneKind::with_panels().into_iter().find(|k| k.title().eq_ignore_ascii_case(&name)))
        {
            editor.show_pane(Pane::first(kind));
        }
        editor
    }

    /// Open a project - the documents of the one open with unsaved changes saved or let go first, as the user says.
    fn open(&mut self, dir: &Path) {
        if self.workspace.as_ref().is_some_and(|ws| !ws.unsaved_documents().is_empty()) {
            self.closing = Some(Closing::Open(dir.to_path_buf()));
            return;
        }
        self.open_now(dir);
    }

    /// Open a project, off the window's thread. The panes say how far it is, and it is shown once open (`opened`).
    fn open_now(&mut self, dir: &Path) {
        // The project open now lets go of its lock first: opening it again must find it free. It finishes what it is on to
        // do that, on the thread opening the next.
        let leaving = self.workspace.take().map(Workspace::leave);
        self.deferred.clear();
        let progress: Arc<std::sync::Mutex<(String, usize, usize)>> = Arc::default();
        // Said before the lock is taken: an agent's server finding it about to open the project leaves the project to it.
        self.announce_opening(dir);
        let (done, ready) = std::sync::mpsc::channel();
        let (to_frames, agents) = std::sync::mpsc::channel();
        let (dir_owned, said, ctx) = (dir.to_path_buf(), progress.clone(), self.ctx.clone());
        let link_ctx = self.ctx.clone();
        let started = std::thread::Builder::new().name("open".into()).spawn(move || {
            let report = |stage: &str, done: usize, total: usize| {
                if let Ok(mut p) = said.lock() {
                    *p = (stage.to_string(), done, total);
                }
                ctx.request_repaint();
            };
            report("Closing the project", 0, 0);
            drop(leaving);
            // Opening is the editor's own doing: what the user asks of it meanwhile goes first.
            let group = project::schedule::Group::new(project::schedule::Class::Background, "opening");
            let _ = done.send(group.enter(|| Workspace::prepare(&dir_owned, &report, (to_frames, link_ctx))));
            ctx.request_repaint();
        });
        match started {
            Ok(_) => {
                let began = std::time::Instant::now();
                self.opening = Some(Opening { dir: dir.to_path_buf(), began, progress, ready, agents, select: None });
            }
            Err(e) => self.error = Some(format!("{}: {e}", dir.display())),
        }
    }

    /// Agents' requests while the project opens: what they ask of the window answered as ever, what they ask of the
    /// project said to wait, with how far it is.
    fn answer_while_opening(&mut self, ctx: &egui::Context) {
        let Some(opening) = &self.opening else { return };
        let arrived: Vec<crate::workspace::AgentRequest> = opening.agents.try_iter().collect();
        if arrived.is_empty() {
            return;
        }
        let (stage, done, total) = opening.progress.lock().map_or_default(|p| p.clone());
        let far = if total > 0 { format!("{stage}, {done} of {total}") } else { stage };
        let mut window = Vec::new();
        for (request, reply) in arrived {
            if request.uses_window() {
                window.push((request, reply));
            } else {
                let said = format!("the editor is still opening the project ({far}): ask again once it is open");
                let _ = reply.send(mcp::Reply::Error(said));
            }
        }
        self.drive.take(ctx, window);
    }

    /// The project being opened, once it is: shown, its selection kept; or why it could not be.
    fn opened(&mut self) {
        let Some(opening) = &self.opening else { return };
        let Ok(result) = opening.ready.try_recv() else { return };
        let Some(opening) = self.opening.take() else { return };
        let dir = opening.dir;
        match result {
            Ok(opened) => {
                self.settings.last_project = std::path::absolute(&dir).ok();
                if let Some(label) = &opened.recovered {
                    self.message = Some(format!("An edit a crash interrupted was rolled back: {label}"));
                }
                let mut ws = Workspace::from_opened(opened, opening.agents);
                // A layout outlives a project: tabs of what is not here close, and what is in front is what is selected.
                self.layouts.close_documents(|path| !ws.exists(path));
                self.opened = None;
                let front =
                    self.layouts.active_document().and_then(|pane| self.layouts.subject(pane)).map(str::to_string);
                if let Some(path) = opening.select.filter(|p| ws.exists(p)).or(front) {
                    ws.navigate(path);
                }
                self.workspace = Some(ws);
                self.ask_what_it_needs();
                self.error = None;
            }
            Err(e) => {
                if let Some(project::lock::LockError::Held(owner)) = e.downcast_ref() {
                    self.in_use = Some((dir.clone(), owner.clone()));
                }
                self.error = Some(format!("{}: {e:#}", dir.display()));
            }
        }
    }

    /// What the project has to settle, asked of whoever is there. A run that only looks, as a screenshot does, or a test
    /// run, has nobody to ask.
    fn ask_what_it_needs(&mut self) {
        let Some(ws) = &self.workspace else { return };
        let chosen = self.settings.active_configs.contains_key(&ws.project.root.display().to_string());
        let asked = if self.capture.is_some() || self.unattended { None } else { crate::onboarding::asked(ws, chosen) };
        self.onboarding = asked.filter(|asked| self.not_now.as_ref() != Some(&asked.root));
    }

    /// How far the project being opened is, as the panes show it.
    fn opening_shown(&self) -> Option<panes::OpeningShown> {
        let opening = self.opening.as_ref()?;
        let (stage, done, total) = opening.progress.lock().map_or_default(|p| p.clone());
        let name = opening
            .dir
            .file_name()
            .map_or_else(|| opening.dir.display().to_string(), |n| n.to_string_lossy().into_owned());
        #[expect(clippy::cast_precision_loss, reason = "a fraction shown")]
        let fraction = (total > 0).then(|| done as f32 / total as f32);
        Some(panes::OpeningShown { name, stage, fraction })
    }

    /// What was asked while the project was being read, in order, once it has been.
    fn do_deferred(&mut self) {
        let Some(ws) = &self.workspace else { return };
        let ready = self.deferred.iter().take_while(|(ticket, _)| ws.covers(*ticket)).count();
        let ready: Vec<_> = self.deferred.drain(..ready).collect();
        for (_, deferred) in ready {
            match deferred {
                Deferred::Edit(request) => self.edit(&request),
                Deferred::Step { redo, force, scope } => drop(self.step(redo, force, scope)),
            }
        }
    }

    /// Bring in what changed on disk; read the project again when its `project.toml` changed, keeping what is showing.
    fn follow_disk(&mut self, ctx: &egui::Context) {
        let Some(ws) = &mut self.workspace else { return };
        // The config in use, as what events set off reads it, and what is made reads it.
        ws.set_config(active_config(&self.settings, &ws.project));
        ws.watch(ctx);
        ws.answer_agents();
        let focused = ctx.input(|i| i.focused);
        let live = self.settings.changes.take_in == crate::settings::TakeIn::AsTheyHappen;
        match ws.update(focused, live) {
            Update::Nothing => {}
            Update::Refreshed(n) => {
                self.message = Some(format!("{n} asset{} changed on disk", if n == 1 { "" } else { "s" }));
            }
            Update::Said(said) => self.message = Some(said),
            Update::Reloaded(why) => {
                self.message = Some(format!("{why}: the project was read again"));
                // A tab of what is no longer there closes.
                let ws: &Workspace = ws;
                self.layouts.close_documents(|path| !ws.exists(path));
                self.ask_what_it_needs();
            }
        }
        self.close_gone_documents();
    }

    /// The tabs of assets gone - deleted, their making undone - closed: one moved finds it by its uid instead.
    fn close_gone_documents(&mut self) {
        let Some(ws) = &self.workspace else { return };
        if ws.reading_until().is_some() {
            return;
        }
        let gone: Vec<String> = self
            .layouts
            .states
            .values()
            .filter_map(|state| {
                let path = state.pinned.as_ref()?;
                let moved = state.uid.as_ref().is_some_and(|uid| ws.index.by_uid.contains_key(uid));
                (!ws.exists(path) && !moved).then(|| path.clone())
            })
            .collect();
        if !gone.is_empty() {
            self.layouts.close_documents(|path| gone.iter().any(|g| g == path));
        }
    }

    /// Undo the last step, or redo - of them all, or of one document; forced over what changed outside the editor.
    /// Blocked by such changes, it asks.
    fn step(&mut self, redo: bool, force: bool, scope: Option<String>) -> bool {
        let author = self.author();
        let Some(ws) = &mut self.workspace else { return false };
        if let Some(ticket) = ws.reading_until() {
            self.deferred.push((ticket, Deferred::Step { redo, force, scope }));
            return true;
        }
        ws.history.set_author(author);
        let verb = if redo { "Redo" } else { "Undo" };
        match ws.step(redo, force, scope.as_deref()) {
            // Its files taken in by the worker: a `project.toml` changed has the project read again.
            Ok(label) => {
                self.message = Some(format!("{}: {label}", if redo { "Redone" } else { "Undone" }));
                true
            }
            Err(StepError::Changed(paths)) => {
                self.force = Some((redo, scope, StepError::Changed(paths).to_string()));
                false
            }
            Err(e) => {
                self.message = Some(format!("{verb}: {e}"));
                false
            }
        }
    }

    /// Make an edit in the active layer; a rename first asks for the new name, and an override of code while an
    /// asset layer is active asks which files layer takes it.
    fn edit(&mut self, request: &EditRequest) {
        if let EditRequest::Rename(uid, real) = request {
            let name = real.trim_end_matches(project::index::BUNDLE_SUFFIX).rsplit('/').next().unwrap_or(real);
            self.renaming = Some((uid.clone(), real.clone(), name.to_string()));
            return;
        }
        if matches!(request, EditRequest::ShowGraph) {
            if let Some(path) = self.workspace.as_ref().and_then(|ws| ws.selected.clone()) {
                self.layouts.open(&path, false);
                self.layouts.show_in(crate::panes::EditorKind::Graph);
            }
            return;
        }
        let author = self.author();
        let Some(ws) = &mut self.workspace else { return };
        if let Some(ticket) = ws.reading_until() {
            self.deferred.push((ticket, Deferred::Edit(request.clone())));
            return;
        }
        ws.history.set_author(author);
        if let EditRequest::Override(uid) = request {
            match ws.override_choice(uid) {
                Err(why) => {
                    self.message = Some(why);
                    return;
                }
                Ok(Some(layers)) => {
                    let entry = ws.index.entries.iter().find(|(_, e)| e.uid == *uid);
                    let (path, home) = entry.map_or_default(|(p, e)| (p.clone(), e.home.clone()));
                    self.choosing = Some(Choosing { uid: uid.clone(), path, home, layers, name: String::new() });
                    return;
                }
                Ok(None) => {}
            }
        }
        self.message = Some(match ws.edit(request) {
            Ok(label) => label,
            Err(e) => format!("{e:#}"),
        });
    }

    /// Which files layer takes code being overridden: one of those above its own, or a new one (added, the project
    /// read again, then the override made in it).
    fn choose_dialog(&mut self, ctx: &egui::Context) {
        let Some(choosing) = &mut self.choosing else { return };
        let (mut chosen, mut create, mut cancel) = (None, false, false);
        let valid = self.workspace.as_ref().map(|ws| project::stack::check_name(&ws.project, choosing.name.trim()));
        egui::Window::new("Override in a files layer")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_max_width(460.0);
                ui.label(format!(
                    "{} is code, from {}: it is overridden in a files layer above it, not in an asset layer.",
                    choosing.path, choosing.home
                ));
                ui.add_space(6.0);
                for layer in &choosing.layers {
                    if ui.button(format!("Override in {layer}")).clicked() {
                        chosen = Some(layer.clone());
                    }
                }
                if choosing.layers.is_empty() {
                    ui.label(RichText::new("No files layer can take it yet.").color(theme::weak()));
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut choosing.name)
                            .hint_text("new files layer")
                            .desired_width(160.0),
                    );
                    let ok = valid.as_ref().is_some_and(Result::is_ok);
                    create = ui.add_enabled(ok, egui::Button::new("Create and override")).clicked();
                });
                if let Some(Err(e)) = &valid
                    && !choosing.name.trim().is_empty()
                {
                    ui.label(RichText::new(e.to_string()).small().color(ui.visuals().warn_fg_color));
                }
                ui.add_space(6.0);
                cancel = ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape));
            });
        if cancel {
            self.choosing = None;
        } else if let Some(layer) = chosen {
            let uid = choosing.uid.clone();
            self.choosing = None;
            self.edit(&EditRequest::OverrideIn(uid, layer));
        } else if create {
            let (uid, name) = (choosing.uid.clone(), choosing.name.trim().to_string());
            self.choosing = None;
            let added = self.workspace.as_mut().map(|ws| ws.add_layer(&name, project::LayerKind::Files));
            match added {
                // The layer has to be there: the project is read again with it first.
                Some(Ok(ticket)) => self.deferred.push((ticket, Deferred::Edit(EditRequest::OverrideIn(uid, name)))),
                Some(Err(e)) => self.message = Some(format!("{e:#}")),
                None => {}
            }
        }
    }

    /// The new name of an entry being renamed: in its folder, a bundle keeping its `.node`.
    fn rename_dialog(&mut self, ctx: &egui::Context) {
        let Some((uid, real, name)) = &mut self.renaming else { return };
        let (mut go, mut cancel) = (false, false);
        let valid = !name.trim().is_empty() && !name.contains('/') && !name.starts_with('.');
        egui::Window::new("Rename")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(real.trim_end_matches(project::index::BUNDLE_SUFFIX).to_string())
                        .color(theme::weak()),
                );
                let output = egui::TextEdit::singleline(name).desired_width(320.0).show(ui);
                let field = output.response;
                // Opened, the whole name is selected: what is typed replaces it.
                if !field.has_focus() {
                    let mut state = output.state;
                    let all = egui::text::CCursorRange::two(
                        egui::text::CCursor::new(0),
                        egui::text::CCursor::new(name.chars().count()),
                    );
                    state.cursor.set_char_range(Some(all));
                    state.store(ui.ctx(), field.id);
                }
                // Leaving the field (Enter) renames; Escape cancels.
                let escaped = ui.input(|i| i.key_pressed(egui::Key::Escape));
                if field.lost_focus() && !escaped {
                    go = valid;
                }
                field.request_focus();
                ui.horizontal(|ui| {
                    go |= ui.add_enabled(valid, egui::Button::new("Rename")).clicked();
                    cancel = ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape));
                });
            });
        if go {
            let suffix = if real.ends_with(project::index::BUNDLE_SUFFIX) { project::index::BUNDLE_SUFFIX } else { "" };
            let folder = real.rsplit_once('/').map_or_default(|(f, _)| format!("{f}/"));
            let request = EditRequest::RenameTo(uid.clone(), format!("{folder}{}{suffix}", name.trim()));
            self.renaming = None;
            self.edit(&request);
        } else if cancel {
            self.renaming = None;
        }
    }

    /// Steps blocked by files changed outside the editor: undone or redone anyway, or left.
    fn force_dialog(&mut self, ctx: &egui::Context) {
        let Some((redo, scope, why)) = &self.force else { return };
        let (redo, scope, mut go, mut cancel) = (*redo, scope.clone(), false, false);
        egui::Window::new("Changed outside the editor")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_max_width(460.0);
                ui.label(format!("What this step changed was {why}."));
                ui.label(
                    RichText::new(
                        "Going on overwrites them; what it overwrites can be brought back by the opposite step.",
                    )
                    .color(theme::weak()),
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    go = ui.button(if redo { "Redo anyway" } else { "Undo anyway" }).clicked();
                    cancel = ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape));
                });
            });
        if go {
            self.force = None;
            self.step(redo, true, scope);
        } else if cancel {
            self.force = None;
        }
    }

    /// F5: what other programs changed taken in now.
    fn refresh(&mut self) {
        let Some(ws) = &mut self.workspace else { return };
        let pending = ws.pending();
        ws.refresh_now();
        self.message = Some(match pending {
            0 => "Nothing changed on disk".to_string(),
            n => format!("Taking in {n} change{} made on disk", if n == 1 { "" } else { "s" }),
        });
    }

    /// Ctrl+Z undoes, Ctrl+Shift+Z and Ctrl+Y redo - unless a text field has the keyboard, which undoes its own text.
    /// Follow the monitor's size unless the user chose a zoom: a 4K monitor without OS scaling (X11 under `WSLg`)
    /// would otherwise draw everything at half size. A zoom made with the keyboard (Ctrl +/-/0) is kept as chosen.
    fn zoom(&mut self, ctx: &egui::Context) {
        let current = ctx.zoom_factor();
        if let Some(applied) = self.applied_zoom
            && (current - applied).abs() > 0.001
        {
            self.settings.appearance.zoom = Some(current);
        }
        let ppp = ctx.input(|i| i.pixels_per_point);
        self.steady = if (ppp - self.steady.0).abs() < 0.001 { (ppp, self.steady.1 + 1) } else { (ppp, 0) };
        let measured = if self.steady.1 >= STEADY_FRAMES { monitor_height(ctx) } else { None };
        let wanted = match self.settings.appearance.zoom {
            Some(zoom) => zoom,
            None => match measured {
                Some(height) if self.zoomed_for.is_none_or(|h| (h - height).abs() > 1.0) => {
                    self.zoomed_for = Some(height);
                    automatic_zoom(ctx, height)
                }
                _ => current,
            },
        };
        if (wanted - current).abs() > 0.001 {
            ctx.set_zoom_factor(wanted);
        }
        self.applied_zoom = Some(wanted);
    }

    fn set_zoom(&mut self, zoom: Option<f32>) {
        self.settings.appearance.zoom = zoom.map(|z| z.clamp(0.5, 4.0));
    }

    fn pane_visible(&self, pane: Pane) -> bool {
        self.layouts.is_visible(pane)
    }

    /// Show or hide a pane: one shown in a window of its own is hidden by returning it to the main window, hidden.
    fn toggle_pane(&mut self, pane: Pane) {
        if self.layouts.is_visible(pane) {
            self.layouts.hide(pane);
        } else {
            self.show_pane(pane);
        }
    }

    /// Show a pane and bring its tab to the front - and its window, where it is in one.
    fn show_pane(&mut self, pane: Pane) {
        self.leave_bare();
        self.focus = self.layouts.show(pane);
    }

    /// Every command there is now: the modes are the user's.
    fn commands(&self) -> Vec<Command> {
        Command::all().into_iter().chain((0..self.layouts.modes.len()).map(Command::Mode)).collect()
    }

    /// A command's name: a mode's is its own.
    fn title_of(&self, command: Command) -> String {
        match command {
            Command::Mode(at) => format!("Mode: {}", self.layouts.modes[at].name),
            other => other.title(),
        }
    }

    /// Whether a command can be run now.
    fn can_run(&self, command: Command) -> bool {
        let ws = self.workspace.as_ref();
        let document = self.layouts.active_document();
        match command {
            Command::Palette
            | Command::OpenProject
            | Command::NewProject
            | Command::Quit
            | Command::Settings
            | Command::DocumentsOnly
            | Command::Maximize
            | Command::Fullscreen
            | Command::ResetLayout
            | Command::NewMode
            | Command::ResetMode
            | Command::ZoomIn
            | Command::ZoomOut
            | Command::ActualSize
            | Command::PerformanceOverlay
            | Command::Pane(_)
            | Command::Mode(_) => true,
            Command::DeleteMode => self.layouts.modes.len() > 1,
            Command::Undo => self.step_scope(false).is_some(),
            Command::Redo => self.step_scope(true).is_some(),
            Command::UndoAnywhere => ws.is_some_and(|ws| ws.history.to_undo(None).is_some()),
            Command::RedoAnywhere => ws.is_some_and(|ws| ws.history.to_redo(None).is_some()),
            Command::Save | Command::Revert => {
                self.document_uid().zip(ws).is_some_and(|(uid, ws)| ws.history.document(&uid).is_some())
            }
            Command::SaveAll => ws.is_some_and(|ws| !ws.history.documents().is_empty()),
            Command::Export => self.document_path().is_some(),
            Command::Back => {
                if self.in_browser {
                    ws.is_some_and(Workspace::can_folder_back)
                } else {
                    self.visit_at(false).is_some()
                }
            }
            Command::Forward => {
                if self.in_browser {
                    ws.is_some_and(Workspace::can_folder_forward)
                } else {
                    self.visit_at(true).is_some()
                }
            }
            Command::Up => ws.is_some_and(|ws| !ws.asset_folder.is_empty()),
            Command::CloseTab | Command::NextTab | Command::PreviousTab => self.layouts.has_tab(),
            // A view the document's asset has: not every asset has a graph, a preview or an extension's editor.
            Command::ShowIn(editor) => {
                let path = document.and_then(|pane| self.layouts.states.get(&pane)?.pinned.as_deref());
                path.zip(ws).is_some_and(|(path, ws)| panes::editors_of(ws, path).contains(&editor))
            }
            Command::KeepTab => document.is_some(),
            Command::Import | Command::ReloadProject | Command::Refresh | Command::Extension(_) => ws.is_some(),
            Command::Build | Command::Clean => ws.is_some_and(|ws| !ws.project.configs.is_empty()),
            Command::Fill => ws.is_some_and(|ws| ws.project.generated().next().is_some() && !ws.fill.running()),
            Command::CancelEpoch => ws.is_some_and(Workspace::in_epoch),
            Command::DeleteSelected => ws.is_some_and(|ws| ws.removing_selected().is_some()),
            // Its graph shown: asked for, and the asset having one.
            Command::GraphFit | Command::GraphArrange => {
                document.and_then(|pane| self.layouts.states.get(&pane)).is_some_and(|state| {
                    state.editor == Some(EditorKind::Graph)
                        && state
                            .pinned
                            .as_deref()
                            .zip(ws)
                            .is_some_and(|(path, ws)| panes::editors_of(ws, path).contains(&EditorKind::Graph))
                })
            }
        }
    }

    /// Do what a command says.
    fn run(&mut self, ctx: &egui::Context, command: Command) {
        match command {
            Command::Palette => self.palette = self.palette.is_none().then(Palette::default),
            Command::OpenProject => {
                self.open_dialog =
                    Some(self.workspace.as_ref().map_or_else(String::new, |w| w.project.root.display().to_string()));
            }
            Command::NewProject => self.new_dialog = Some(String::new()),
            Command::Import => self.import = Some(String::new()),
            Command::Fill => {
                if let Some(ws) = &mut self.workspace {
                    ws.fill.ask();
                }
            }
            Command::Build => self.signal_saved(Build::NAME),
            Command::Clean => self.signal_saved(Clean::NAME),
            Command::CancelEpoch => {
                if let Some(ws) = &self.workspace {
                    ws.cancel_epoch();
                }
            }
            Command::ReloadProject => {
                if let Some(ws) = &mut self.workspace {
                    ws.reload_whole();
                }
            }
            Command::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Command::Settings => self.show_pane(Pane::first(PaneKind::Settings)),
            Command::Undo
            | Command::Redo
            | Command::UndoAnywhere
            | Command::RedoAnywhere
            | Command::Save
            | Command::SaveAll
            | Command::Revert => self.run_on_documents(command),
            Command::Export => self.exporting = self.document_path().and_then(|path| self.export_choice(&path)),
            Command::Refresh => self.refresh(),
            Command::Back | Command::Forward => {
                let forward = command == Command::Forward;
                match (self.in_browser, &mut self.workspace) {
                    (true, Some(ws)) if forward => ws.folder_forward(),
                    (true, Some(ws)) => ws.folder_back(),
                    _ => self.visit(forward),
                }
            }
            Command::Up => {
                if let Some(ws) = &mut self.workspace {
                    ws.folder_up();
                }
            }
            Command::CloseTab => {
                if let Some((window, _, tile)) = self.layouts.tab_in_use()
                    && self.asks_to_close(window, tile)
                {
                    return;
                }
                self.layouts.close_tab();
                self.select_in_use();
            }
            Command::KeepTab => {
                if let Some((window, tile)) = self.layouts.active_document().and_then(|pane| self.layouts.find(pane)) {
                    self.layouts.act(window, tile, PaneAction::Keep);
                }
            }
            Command::NextTab | Command::PreviousTab => {
                self.layouts.next_tab(command == Command::NextTab);
                self.select_in_use();
            }
            Command::ShowIn(editor) => {
                self.layouts.focused = self.layouts.active_document();
                self.layouts.show_in(editor);
            }
            Command::DocumentsOnly => self.toggle_bare(),
            Command::ResetLayout => {
                self.bare = None;
                self.layouts = Layouts::new(panes::default_layout());
                self.opened = None;
            }
            Command::NewMode => self.naming = Some(String::new()),
            Command::ResetMode => {
                self.leave_bare();
                self.layouts.reset_mode();
            }
            Command::DeleteMode => {
                self.leave_bare();
                self.layouts.delete_mode();
            }
            Command::ZoomIn => self.set_zoom(Some(ctx.zoom_factor() + 0.25)),
            Command::ZoomOut => self.set_zoom(Some(ctx.zoom_factor() - 0.25)),
            Command::ActualSize => self.set_zoom(Some(1.0)),
            Command::PerformanceOverlay => self.settings.performance_overlay = !self.settings.performance_overlay,
            Command::Pane(kind) => self.show_pane(Pane::first(kind)),
            Command::Mode(at) => {
                self.leave_bare();
                self.layouts.switch_mode(at);
            }
            other => self.run_on_what_is_shown(ctx, other),
        }
    }

    /// The commands acting on documents: saving them, reverting them, undoing and redoing.
    fn run_on_documents(&mut self, command: Command) {
        match command {
            Command::Undo | Command::Redo => {
                let redo = command == Command::Redo;
                if let Some(scope) = self.step_scope(redo) {
                    self.step(redo, false, scope.uid());
                }
            }
            Command::UndoAnywhere => drop(self.step(false, false, None)),
            Command::RedoAnywhere => drop(self.step(true, false, None)),
            Command::Save => {
                if let Some(uid) = self.document_uid() {
                    self.save(&[uid]);
                }
            }
            Command::SaveAll => self.save_all(),
            Command::Revert => {
                if let Some(uid) = self.document_uid() {
                    self.revert(&uid);
                }
            }
            _ => {}
        }
    }

    /// Save every document with unsaved changes.
    fn save_all(&mut self) {
        let all = self.workspace.as_ref().map_or_default(Workspace::unsaved_documents);
        self.save(&all.into_iter().map(|(uid, _)| uid).collect::<Vec<_>>());
    }

    /// The commands acting on what is selected, the graph in front, the window itself and what extensions offer.
    fn run_on_what_is_shown(&mut self, ctx: &egui::Context, command: Command) {
        match command {
            Command::DeleteSelected => {
                if let Some(request) = self.workspace.as_ref().and_then(Workspace::removing_selected) {
                    self.edit(&request);
                    if let Some(ws) = &mut self.workspace {
                        ws.inner = None;
                    }
                }
            }
            Command::GraphFit | Command::GraphArrange => {
                if let Some(ws) = &mut self.workspace {
                    ws.graph_ask = Some(if command == Command::GraphFit { GraphAsk::Fit } else { GraphAsk::Arrange });
                }
            }
            Command::Maximize => {
                let maximized = ctx.input(|i| i.viewport().maximized).unwrap_or(false);
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
            }
            Command::Fullscreen => {
                let fullscreen = ctx.input(|i| i.viewport().fullscreen).unwrap_or(false);
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen));
            }
            Command::Extension(tool) => {
                let answer = self.workspace.as_ref().map(|ws| ws.run_tool(tool.as_str()));
                match answer {
                    Some(Ok(answer)) => self.message = Some(answer),
                    Some(Err(why)) => self.error = Some(why),
                    None => {}
                }
            }
            _ => {}
        }
    }

    /// Note the document in focus as visited, where it is another than the one back and forward stand at: what comes
    /// after that is let go, as a browser does.
    fn note_visit(&mut self) {
        let Some(ws) = &self.workspace else { return };
        let Some(pane) = self.layouts.focused.or_else(|| self.layouts.active_document()) else { return };
        let Some(state) = self.layouts.states.get(&pane) else { return };
        let Some(path) = state.pinned.clone() else { return };
        let uid = ws.index.assets.get(&path).and_then(|a| a.uid.clone());
        let (visits, at) = &mut self.visits;
        if visits.get(*at).is_some_and(|v| v.path == path || (uid.is_some() && v.uid == uid)) {
            visits[*at].editor = state.editor;
            return;
        }
        visits.truncate(*at + 1);
        visits.push(Visit { path, uid, editor: state.editor });
        if visits.len() > VISITS {
            visits.remove(0);
        }
        *at = visits.len() - 1;
    }

    /// Where back (or forward) goes: the next visit that is still there.
    fn visit_at(&self, forward: bool) -> Option<usize> {
        let ws = self.workspace.as_ref()?;
        let (visits, at) = (&self.visits.0, self.visits.1);
        let there = |visit: &Visit| visit.place(ws).is_some();
        if forward {
            (at + 1..visits.len()).find(|&i| there(&visits[i]))
        } else {
            (0..at.min(visits.len())).rev().find(|&i| there(&visits[i]))
        }
    }

    /// Go back (or forward) to a document visited: focused, opened again where it was closed, shown as it was.
    fn visit(&mut self, forward: bool) {
        let Some(at) = self.visit_at(forward) else { return };
        let Some(ws) = &mut self.workspace else { return };
        let visit = self.visits.0[at].clone();
        let Some(path) = visit.place(ws) else { return };
        self.visits.1 = at;
        self.visits.0[at].path.clone_from(&path);
        ws.navigate(path.clone());
        self.opened.clone_from(&ws.selected);
        self.layouts.open(&path, true);
        if let Some(editor) = visit.editor {
            self.layouts.show_in(editor);
        }
    }

    /// Who makes what is done now: an agent - its input fed to this frame, or its command being run - or the user.
    fn author(&self) -> &'static str {
        if self.drive.fed() || self.agent_running { "agent" } else { "user" }
    }

    /// The uid of the document in use: the asset its tab in front shows, else the asset selected.
    fn document_uid(&self) -> Option<String> {
        let ws = self.workspace.as_ref()?;
        ws.index.assets.get(&self.document_path()?)?.uid.clone()
    }

    /// The asset of the document in use: its tab in front's, else the asset selected.
    fn document_path(&self) -> Option<String> {
        let ws = self.workspace.as_ref()?;
        let shown = self.layouts.active_document().and_then(|pane| self.layouts.subject(pane));
        let path = shown.or(ws.selected.as_deref())?;
        ws.index.assets.contains_key(path).then(|| path.to_string())
    }

    /// The export of an asset to choose: its exporters, the first chosen, its file in the project's `exports` folder.
    fn export_choice(&self, path: &str) -> Option<Exporting> {
        let ws = self.workspace.as_ref()?;
        let asset = ws.index.assets.get(path)?;
        let exporters = ws.extensions.exporters(asset);
        let mut exporting = Exporting {
            path: path.to_string(),
            exporters,
            chosen: 0,
            member: 0,
            to: String::new(),
            typed: false,
            reveal: false,
        };
        exporting.to = exporting.default_to(&ws.project.root);
        Some(exporting)
    }

    /// The export being chosen: which exporter, which member, and where its file goes.
    fn export_dialog(&mut self, ctx: &egui::Context) {
        let Some(exporting) = &mut self.exporting else { return };
        let Some(ws) = &self.workspace else { return };
        let name = exporting.path.rsplit('/').next().unwrap_or(&exporting.path).to_string();
        let (mut go, mut cancel) = (false, false);
        egui::Window::new(format!("Export {name}"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_width(460.0);
                if exporting.exporters.is_empty() {
                    ui.label(
                        RichText::new("Nothing exports this asset: no extension makes a file of what it makes.")
                            .color(theme::weak()),
                    );
                    cancel = ui.button("Close").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape));
                    return;
                }
                let before = (exporting.chosen, exporting.member);
                for (i, exporter) in exporting.exporters.iter().enumerate() {
                    ui.radio_value(&mut exporting.chosen, i, format!("{}  (.{})", exporter.label, exporter.extension));
                }
                let members = &exporting.exporters[exporting.chosen].members;
                if !members.is_empty() {
                    exporting.member = exporting.member.min(members.len() - 1);
                    ui.horizontal(|ui| {
                        ui.label("Of");
                        egui::ComboBox::from_id_salt("export member")
                            .selected_text(&members[exporting.member].1)
                            .show_ui(ui, |ui| {
                                for (i, (_, label)) in members.iter().enumerate() {
                                    ui.selectable_value(&mut exporting.member, i, label);
                                }
                            });
                    });
                }
                if before != (exporting.chosen, exporting.member) && !exporting.typed {
                    exporting.to = exporting.default_to(&ws.project.root);
                }
                ui.add_space(4.0);
                ui.label("Save to");
                let field = ui.add(egui::TextEdit::singleline(&mut exporting.to).desired_width(f32::INFINITY));
                exporting.typed |= field.changed();
                ui.checkbox(&mut exporting.reveal, "Show its folder when it is written");
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    go = ui.button("Export").clicked();
                    cancel = ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape));
                });
            });
        if cancel {
            self.exporting = None;
        } else if go && let Some(exporting) = self.exporting.take() {
            self.export(&exporting, ctx);
        }
    }

    /// Make an export's file, off the window's thread: from the project as shown, unsaved changes and all.
    fn export(&mut self, exporting: &Exporting, ctx: &egui::Context) {
        let Some(ws) = &self.workspace else { return };
        let exporter = exporting.exporters[exporting.chosen].clone();
        let member = exporter.members.get(exporting.member).map(|(key, _)| key.clone());
        let (extensions, index, path) = (ws.extensions.clone(), ws.index.clone(), exporting.path.clone());
        let to = PathBuf::from(exporting.to.trim());
        let (done, result) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        let what = format!("{} as {}", path.rsplit('/').next().unwrap_or(&path), exporter.label);
        let reveal = exporting.reveal;
        let made = std::thread::Builder::new().name("export".into()).spawn(move || {
            let made = (|| -> anyhow::Result<String> {
                let asset = anyhow::Context::context(index.assets.get(&path), "the asset is gone")?;
                let bytes = extensions.export(&index, asset, &exporter.output, member.as_deref())?;
                if let Some(folder) = to.parent().filter(|f| !f.as_os_str().is_empty()) {
                    std::fs::create_dir_all(folder)?;
                }
                anyhow::Context::with_context(std::fs::write(&to, bytes), || to.display().to_string())?;
                if reveal && let Some(folder) = to.parent() {
                    let _ = crate::external::open_default(folder);
                }
                Ok(to.display().to_string())
            })();
            let _ = done.send(made.map_err(|e| format!("{e:#}")));
            ctx.request_repaint();
        });
        match made {
            Ok(_) => {
                self.message = Some(format!("Exporting {what}"));
                self.exports.push((what, result));
            }
            Err(e) => self.error = Some(format!("the export could not start: {e}")),
        }
    }

    /// Exports done since the last frame, said.
    fn exports_done(&mut self) {
        let mut said = vec![];
        self.exports.retain(|(what, result)| match result.try_recv() {
            Ok(Ok(to)) => {
                said.push(Ok(format!("Exported {what} to {to}")));
                false
            }
            Ok(Err(e)) => {
                said.push(Err(format!("Exporting {what}: {e}")));
                false
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => true,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => false,
        });
        for said in said {
            match said {
                Ok(message) => self.message = Some(message),
                Err(error) => self.error = Some(error),
            }
        }
    }

    /// What undo (or redo) takes back now: the last edit of the document in use; with none, the last change to the
    /// project that edited no document - never another document's edit. None: nothing.
    fn step_scope(&self, redo: bool) -> Option<StepScope> {
        let ws = self.workspace.as_ref()?;
        let next = |scope: Option<&str>| if redo { ws.history.to_redo(scope) } else { ws.history.to_undo(scope) };
        match self.document_uid() {
            Some(uid) if next(Some(&uid)).is_some() => Some(StepScope::Document(uid)),
            Some(_) => next(None).filter(|step| step.edits.is_empty()).map(|_| StepScope::Project),
            None => next(None).map(|_| StepScope::Project),
        }
    }

    /// Why undo (or redo) has nothing now, where the project has: the document in use has nothing of its own.
    fn nothing_to_step(&self, redo: bool) -> Option<String> {
        let ws = self.workspace.as_ref()?;
        let step = if redo { ws.history.to_redo(None) } else { ws.history.to_undo(None) }?;
        let path = self.document_path()?;
        let name = path.rsplit('/').next().unwrap_or(&path);
        let (verb, anywhere) = if redo { ("redo", Command::RedoAnywhere) } else { ("undo", Command::UndoAnywhere) };
        Some(format!(
            "Nothing to {verb} in {name}. {} ({}): {}",
            anywhere.title(),
            anywhere.keys(&self.settings),
            step.label
        ))
    }

    /// The label of what undo (or redo) takes back now.
    fn step_label(&self, redo: bool) -> Option<String> {
        let ws = self.workspace.as_ref()?;
        let scope = self.step_scope(redo)?.uid();
        let step = if redo { ws.history.to_redo(scope.as_deref()) } else { ws.history.to_undo(scope.as_deref()) };
        step.map(|step| step.label.clone())
    }

    /// What is asked of the user over the window this frame, and what comes in from outside it.
    fn dialogs(&mut self, ctx: &egui::Context) {
        self.open_dialog(ctx);
        self.new_dialog(ctx);
        self.naming_dialog(ctx);
        self.palette_ui(ctx);
        self.dropped_files(ctx);
        self.import_dialog(ctx);
        self.onboarding_ui(ctx);
        self.fill_ui(ctx);
        self.fetch(ctx);
        self.keep_presence(ctx);
        self.in_use_dialog(ctx);
        self.force_dialog(ctx);
        self.closing_dialog(ctx);
        self.export_dialog(ctx);
        self.exports_done();
        self.left_dialog(ctx);
        self.rename_dialog(ctx);
        self.choose_dialog(ctx);
    }

    /// The window closing with unsaved documents: they are asked about first.
    fn ask_before_closing(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested())
            && !self.quitting
            && self.workspace.as_ref().is_some_and(|ws| !ws.unsaved_documents().is_empty())
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.closing = Some(Closing::Quit);
        }
    }

    /// Do what the status bar's list of unsaved documents asked.
    fn unsaved_asked(&mut self, asked: Unsaved) {
        match asked {
            Unsaved::SaveAll => self.save_all(),
            Unsaved::Save(uid) => drop(self.save(&[uid])),
            Unsaved::Revert(uid) => self.revert(&uid),
            Unsaved::Show(path) => {
                if let Some(ws) = &mut self.workspace {
                    ws.navigate(path.clone());
                }
                self.layouts.open(&path, true);
            }
        }
    }

    /// Save documents; the first that cannot be stops it, said.
    fn save(&mut self, uids: &[String]) -> bool {
        let author = self.author();
        let Some(ws) = &mut self.workspace else { return false };
        ws.history.set_author(author);
        let mut saved = vec![];
        for uid in uids {
            match ws.save(uid) {
                Ok(label) => saved.push(label),
                Err(e) => {
                    self.error = Some(format!("{e:#}"));
                    return false;
                }
            }
        }
        self.message = match saved.as_slice() {
            [] => None,
            [one] => Some(one.clone()),
            many => Some(format!("Saved {} documents", many.len())),
        };
        true
    }

    /// Put a document back as on disk: its unsaved changes undone, or - stale - the disk's taken.
    fn revert(&mut self, uid: &str) {
        let author = self.author();
        if let Some(ws) = &mut self.workspace {
            ws.history.set_author(author);
            self.message = Some(ws.revert(uid));
        }
    }

    /// A build or a clean, its documents saved first: it reads what is saved.
    fn signal_saved(&mut self, event: &'static str) {
        if self.workspace.as_ref().is_some_and(|ws| !ws.unsaved_documents().is_empty()) {
            self.closing = Some(Closing::Signal(event));
        } else {
            self.signal(event);
        }
    }

    /// Whether closing a tab waits to ask first: it is the last showing a document with unsaved changes.
    fn asks_to_close(&mut self, window: Option<u64>, tile: TileId) -> bool {
        let Some(path) = self.layouts.last_tab_of(window, tile) else { return false };
        let Some(ws) = &self.workspace else { return false };
        let Some(uid) = ws.index.assets.get(&path).and_then(|a| a.uid.clone()) else { return false };
        if ws.history.document(&uid).is_none() {
            return false;
        }
        self.closing = Some(Closing::Tab { window, tile, uid });
        true
    }

    /// Unsaved documents asked about before what waits on them: saved, let go, or what waits left.
    fn closing_dialog(&mut self, ctx: &egui::Context) {
        let Some(closing) = self.closing.clone() else { return };
        let Some(ws) = &self.workspace else {
            self.closing = None;
            return;
        };
        let documents: Vec<(String, String)> = match &closing {
            Closing::Tab { uid, .. } => ws.unsaved_documents().into_iter().filter(|(u, _)| u == uid).collect(),
            _ => ws.unsaved_documents(),
        };
        if documents.is_empty() {
            self.closing = None;
            self.go_on(closing, ctx);
            return;
        }
        let name = |path: &str| path.rsplit('/').next().unwrap_or(path).to_string();
        let what = match (&closing, documents.as_slice()) {
            (_, [(_, path)]) => name(path),
            (_, many) => format!("{} documents", many.len()),
        };
        let question = match &closing {
            Closing::Quit => format!("Save the changes to {what} before quitting?"),
            Closing::Open(_) => format!("Save the changes to {what} before opening another project?"),
            Closing::Signal(_) => format!("What is built is what is saved: save the changes to {what} first?"),
            Closing::Tab { .. } => format!("Save the changes to {what} before closing it?"),
        };
        let (mut save, mut discard, mut cancel) = (false, false, false);
        egui::Modal::new(egui::Id::unique("unsaved")).show(ctx, |ui| {
            ui.set_max_width(460.0);
            ui.label(RichText::new("Unsaved changes").strong());
            ui.add_space(4.0);
            ui.label(question);
            if documents.len() > 1 {
                for (_, path) in documents.iter().take(8) {
                    ui.label(RichText::new(format!("  {path}")).color(theme::weak()));
                }
                if documents.len() > 8 {
                    ui.label(RichText::new(format!("  and {} more", documents.len() - 8)).color(theme::weak()));
                }
            }
            if !matches!(closing, Closing::Signal(_)) {
                ui.label(RichText::new("Not saving them lets them go.").color(theme::weak()));
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                save = ui.button(if documents.len() > 1 { "Save all" } else { "Save" }).clicked();
                if !matches!(closing, Closing::Signal(_)) {
                    discard = ui.button("Don't save").clicked();
                }
                cancel = ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape));
            });
        });
        if save {
            self.closing = None;
            let uids: Vec<String> = documents.into_iter().map(|(uid, _)| uid).collect();
            if self.save(&uids) {
                self.go_on(closing, ctx);
            }
        } else if discard {
            self.closing = None;
            match &closing {
                Closing::Tab { uid, .. } => self.revert(uid),
                _ => {
                    if let Some(ws) = &mut self.workspace {
                        ws.discard_unsaved();
                    }
                }
            }
            self.go_on(closing, ctx);
        } else if cancel {
            self.closing = None;
        }
    }

    /// What waited on unsaved documents, done.
    fn go_on(&mut self, closing: Closing, ctx: &egui::Context) {
        match closing {
            Closing::Quit => {
                self.quitting = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Closing::Open(dir) => self.open_now(&dir),
            Closing::Signal(event) => self.signal(event),
            Closing::Tab { window, tile, .. } => {
                self.layouts.act(window, tile, PaneAction::Close);
                self.select_in_use();
            }
        }
    }

    /// The session a crash ended, offered back: its unsaved documents and its history taken up, or let go.
    fn left_dialog(&mut self, ctx: &egui::Context) {
        let Some(ws) = &self.workspace else { return };
        if self.onboarding.is_some() || ws.reading_until().is_some() {
            return;
        }
        let Some((steps, documents)) = ws.left_session() else { return };
        let (mut resume, mut discard) = (false, false);
        egui::Modal::new(egui::Id::unique("left session")).show(ctx, |ui| {
            ui.set_max_width(460.0);
            ui.label(RichText::new("The last session did not close").strong());
            ui.add_space(4.0);
            let plural = |n: usize, one: &str| format!("{n} {one}{}", if n == 1 { "" } else { "s" });
            ui.label(format!(
                "The editor ended without closing this project. Its session left {} with unsaved changes and {} of \
                 history.",
                plural(documents, "document"),
                plural(steps, "step")
            ));
            ui.label(
                RichText::new("Resuming brings them back as they were; discarding lets them go.").color(theme::weak()),
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                resume = ui.button("Resume").clicked();
                discard = ui.button("Discard").clicked();
            });
        });
        if let Some(ws) = &mut self.workspace
            && (resume || discard)
        {
            ws.resume_left(resume);
            self.message = Some(if resume { "Resumed the last session" } else { "Discarded the last session" }.into());
        }
    }

    /// What the document tab in use shows is what is selected.
    fn select_in_use(&mut self) {
        let path = self.layouts.focused.and_then(|pane| self.layouts.subject(pane)).map(str::to_string);
        if let (Some(ws), Some(path)) = (self.workspace.as_mut(), path) {
            ws.navigate(path);
            self.opened.clone_from(&ws.selected);
        }
    }

    /// Answer what agents asked of the commands, as the palette would: the same list, the same refusal.
    fn agent_commands(&mut self, ctx: &egui::Context, drive: bool) {
        for (name, arguments, reply) in std::mem::take(&mut self.drive.commands) {
            let answer = if name == "editor_commands" {
                self.commands_listed()
            } else if !drive {
                mcp::Reply::failed(self.drive.refusal().to_string())
            } else {
                match serde_json::from_value::<mcp::RunArgs>(serde_json::Value::Object(arguments)) {
                    Ok(args) => self.run_by_id(ctx, &args.command),
                    Err(e) => mcp::Reply::failed(format!("arguments: {e}")),
                }
            };
            let _ = reply.send(answer);
        }
    }

    /// The commands, as agents read them.
    fn commands_listed(&self) -> mcp::Reply {
        let listed: Vec<serde_json::Value> = self
            .commands()
            .into_iter()
            .map(|command| {
                let keys: Vec<String> = command.bindings(&self.settings).into_iter().map(Binding::text).collect();
                serde_json::json!({ "id": command.id(), "title": self.title_of(command), "keys": keys, "can_run": self.can_run(command) })
            })
            .collect();
        mcp::Reply::text(serde_json::Value::Array(listed).to_string())
    }

    /// Run a command an agent named.
    fn run_by_id(&mut self, ctx: &egui::Context, id: &str) -> mcp::Reply {
        let Some(command) = self.commands().into_iter().find(|command| command.id() == id) else {
            return mcp::Reply::failed(format!("{id}: no such command (editor_commands lists them)"));
        };
        if !self.can_run(command) {
            return mcp::Reply::failed(format!("{id}: it cannot run now"));
        }
        self.drive.acted = Some(std::time::Instant::now());
        self.agent_running = true;
        self.run(ctx, command);
        self.agent_running = false;
        mcp::Reply::text(format!("ran {}", self.title_of(command)))
    }

    /// Note where the main window is, once its scale has settled.
    fn note_place(&mut self, ctx: &egui::Context) {
        if self.stored_window.take().is_some() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(false));
        }
        if self.steady.1 < 2 {
            return;
        }
        // Wayland says nothing of where a window is, so all it comes back with is its size, the one it has filling the
        // screen too: told to fill it, `WSLg`'s compositor can crash, and this would be every time it opens.
        if ctx.input(|i| i.viewport().inner_rect.is_none()) {
            self.refill = None;
            let size = ctx.content_rect().size();
            self.place = Some(crate::windows::Place {
                position: [0.0, 0.0],
                size: size.into(),
                contents: [0.0, 0.0],
                scale: 0.0,
            });
            return;
        }
        // Noticed first: what a window does on its way out is not a place.
        let filling = ctx.input(|i| (i.viewport().maximized == Some(true), i.viewport().fullscreen == Some(true)));
        // Made filling the screen, it is: a platform that took no notice has it asked for below, once it has settled.
        if filling != (false, false) {
            self.refill = None;
        }
        if self.refill.is_none() && self.filling != filling {
            if filling == (false, false) {
                self.tracker.return_to(self.place);
            }
            self.filling = filling;
        }
        if let Some(place) = self.tracker.observe(ctx) {
            self.place = Some(place);
        }
        if !self.tracker.is_settled() {
            return;
        }
        match self.refill.take() {
            Some((true, _)) => ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true)),
            Some((_, true)) => ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true)),
            _ => {}
        }
    }

    /// Run what was pressed. Keys typed into a field are the field's, and the palette takes its own.
    fn shortcuts(&mut self, ctx: &egui::Context) {
        if let Some(ws) = &mut self.workspace {
            self.in_browser = std::mem::take(&mut ws.in_browser);
        }
        if crate::commands::is_taking(ctx) {
            return;
        }
        let mut bound: Vec<(Binding, Command)> = Vec::new();
        for command in self.commands() {
            bound.extend(command.bindings(&self.settings).into_iter().map(|binding| (binding, command)));
        }
        bound.sort_by_key(|(binding, _)| std::cmp::Reverse(binding.weight()));
        let typing = ctx.egui_wants_keyboard_input();
        for (binding, command) in bound {
            let pressed = match binding {
                Binding::Key(shortcut) => {
                    (!typing || command == Command::Palette) && ctx.input_mut(|i| i.consume_shortcut(&shortcut))
                }
                Binding::Mouse(button) => ctx.input(|i| i.pointer.button_pressed(button)),
            };
            if pressed {
                if self.can_run(command) {
                    self.run(ctx, command);
                }
                return;
            }
        }
    }

    /// A menu's item for a command, what runs it beside it.
    fn item(&mut self, ui: &mut egui::Ui, command: Command, label: &str) {
        let button = egui::Button::new(label).shortcut_text(command.keys(&self.settings));
        if ui.add_enabled(self.can_run(command), button).clicked() {
            self.run(ui.ctx(), command);
            ui.close();
        }
    }

    /// The palette: every command that can run, what is typed narrowing them, Enter running the one marked.
    fn palette_ui(&mut self, ctx: &egui::Context) {
        let Some(mut palette) = self.palette.take() else { return };
        let mut found: Vec<(u32, String, Command)> = Vec::new();
        for command in
            self.commands().into_iter().filter(|command| self.can_run(*command) && *command != Command::Palette)
        {
            let title = self.title_of(command);
            if let Some(rank) = crate::commands::rank(&title, &palette.typed) {
                found.push((rank, title, command));
            }
        }
        found.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
        let (up, down, enter, escape) = ctx.input_mut(|i| {
            let mut key = |key| i.consume_key(egui::Modifiers::NONE, key);
            (key(egui::Key::ArrowUp), key(egui::Key::ArrowDown), key(egui::Key::Enter), key(egui::Key::Escape))
        });
        let last = found.len().saturating_sub(1);
        palette.at =
            if down { (palette.at + 1).min(last) } else { palette.at.saturating_sub(usize::from(up)).min(last) };
        let mut chosen = (enter && !found.is_empty()).then(|| found[palette.at].2);
        let drawn = egui::Area::new(egui::Id::unique("palette"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 48.0))
            .fade_in(false)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).fill(ui.visuals().panel_fill).show(ui, |ui| {
                    ui.set_width(520.0);
                    let edit = egui::TextEdit::singleline(&mut palette.typed)
                        .hint_text("Type a command")
                        .desired_width(f32::INFINITY);
                    let edit = ui.add(edit);
                    edit.request_focus();
                    if edit.changed() {
                        palette.at = 0;
                    }
                    ui.add_space(4.0);
                    egui::ScrollArea::vertical().max_height(320.0).auto_shrink([false, true]).show(ui, |ui| {
                        for (at, (_, title, command)) in found.iter().enumerate() {
                            let keys = command.keys(&self.settings);
                            let row = egui::Button::selectable(at == palette.at, title.as_str())
                                .right_text(RichText::new(keys).small().color(theme::weak()));
                            let row = ui.add_sized([ui.available_width(), 22.0], row);
                            if at == palette.at && (up || down) {
                                row.scroll_to_me(None);
                            }
                            if row.clicked() {
                                chosen = Some(*command);
                            }
                        }
                        if found.is_empty() {
                            ui.label(RichText::new("No such command").color(theme::weak()));
                        }
                    });
                });
            });
        let away = ctx.input(|i| {
            i.pointer.any_pressed() && i.pointer.latest_pos().is_some_and(|at| !drawn.response.rect.contains(at))
        });
        if let Some(command) = chosen {
            self.run(ctx, command);
        } else if !escape && !away {
            self.palette = Some(palette);
        }
    }

    /// Put away everything but the documents, or bring it back.
    fn toggle_bare(&mut self) {
        if self.bare.is_some() {
            self.leave_bare();
            return;
        }
        let main = &mut self.layouts.main;
        let others = crate::windows::panes_of(main).into_iter().filter(|(_, pane)| pane.kind != PaneKind::Document);
        let shown: Vec<TileId> = others.map(|(tile, _)| tile).filter(|tile| main.tiles.is_visible(*tile)).collect();
        for tile in &shown {
            main.tiles.set_visible(*tile, false);
        }
        self.bare = Some(shown);
    }

    /// Bring back what was put away.
    fn leave_bare(&mut self) {
        for tile in self.bare.take().into_iter().flatten() {
            self.layouts.main.tiles.set_visible(tile, true);
        }
    }

    /// The mode in use, at the end of the header, and what can be done with it.
    /// The config in use for the project open.
    fn active_config(&self) -> Option<project::Config> {
        active_config(&self.settings, &self.workspace.as_ref()?.project)
    }

    /// Signal an event of the project open.
    fn signal(&mut self, event: &str) {
        if let Some(ws) = &mut self.workspace {
            ws.signal(event);
        }
    }

    /// The config in use: a header's dropdown, choosing one building for it.
    fn config_menu(&mut self, ui: &mut egui::Ui) {
        let Some(active) = self.active_config() else { return };
        let Some(ws) = self.workspace.as_ref() else { return };
        let (configs, root) = (ws.project.configs.clone(), ws.project.root.display().to_string());
        let mut chosen = None;
        ui.scope(|ui| {
            ui.menu_button(format!("{}  {}", active.name, icons::CARET_DOWN), |ui| {
                for config in &configs {
                    let label = config
                        .target
                        .as_ref()
                        .map_or_else(|| config.name.clone(), |target| format!("{}  ·  {target}", config.name));
                    if ui.selectable_label(config.name == active.name, label).clicked() {
                        chosen = Some(config.name.clone());
                        ui.close();
                    }
                }
                ui.separator();
                self.item(ui, Command::Build, "Build");
                self.item(ui, Command::Clean, "Clean");
            });
        });
        if let Some(name) = chosen {
            self.settings.active_configs.insert(root, name);
            if let Some(ws) = &mut self.workspace {
                ws.set_config(active_config(&self.settings, &ws.project));
            }
            self.signal(ConfigChanged::NAME);
        }
    }

    fn mode_menu(&mut self, ui: &mut egui::Ui) {
        ui.scope(|ui| {
            // A mode, so said: a mode named Edit is not the Edit menu.
            let name = &self.layouts.modes[self.layouts.mode].name;
            let title = format!("{}  {name} mode  {}", icons::APP_WINDOW, icons::CARET_DOWN);
            ui.menu_button(title, |ui| {
                let mut chosen = None;
                for (at, mode) in self.layouts.modes.iter().enumerate() {
                    if ui.selectable_label(at == self.layouts.mode, &mode.name).clicked() {
                        chosen = Some(at);
                    }
                }
                if let Some(at) = chosen {
                    self.leave_bare();
                    self.layouts.switch_mode(at);
                }
                ui.separator();
                self.item(ui, Command::NewMode, "New mode…");
                self.item(ui, Command::ResetMode, "Reset this mode");
                self.item(ui, Command::DeleteMode, "Delete this mode");
            });
        });
    }

    /// Naming a new mode.
    fn naming_dialog(&mut self, ctx: &egui::Context) {
        let Some(name) = &mut self.naming else { return };
        let (mut add, mut cancel) = (false, false);
        egui::Window::new("New mode")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label("A copy of this mode, called:");
                let edit = ui.add(egui::TextEdit::singleline(name).desired_width(240.0));
                // Before it takes the focus back, or it has never lost it.
                let entered = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                edit.request_focus();
                let named = !name.trim().is_empty();
                ui.horizontal(|ui| {
                    add = ui.add_enabled(named, egui::Button::new("Add")).clicked() || (entered && named);
                    cancel = ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape));
                });
            });
        if add {
            let name = name.trim().to_string();
            self.leave_bare();
            self.layouts.add_mode(&name);
        }
        if add || cancel {
            self.naming = None;
        }
    }

    /// The main window's layout, and the assets' drawer over it.
    fn center(&mut self, ui: &mut egui::Ui, titles: &[(u64, String)], reveal: Option<Pane>) -> Outcome {
        let mut outcome = Outcome::default();
        let opening = self.opening_shown();
        let mut docked = false;
        let assets = Pane::first(PaneKind::Assets);
        let docked_already = self.layouts.is_showing(assets);
        egui::CentralPanel::no_frame().show(ui, |ui| {
            let mut behavior = Panes {
                workspace: self.workspace.as_mut(),
                nav: None,
                pick: None,
                focus: None,
                reveal,
                settings: &mut self.settings,
                layers: &mut self.layers_state,
                states: &mut self.layouts.states,
                windows: titles,
                window: None,
                actions: vec![],
                history: None,
                edit: None,
                message: None,
                opening,
            };
            self.layouts.main.ui(&mut behavior, ui);
            // Up over the layout, on a click, and away again once something else is used. What is dragged out of it needs
            // what is under it.
            self.drawer &= !docked_already && egui::DragAndDrop::payload::<crate::dnd::Dragged>(ui.ctx()).is_none();
            if self.drawer {
                let room = ui.max_rect();
                let size = egui::vec2(room.width() * 0.6, room.height() * 0.5);
                let drawn = egui::Area::new(egui::Id::unique("assets drawer"))
                    .order(egui::Order::Foreground)
                    .fade_in(false)
                    .fixed_pos(egui::pos2(room.left() + 6.0, room.bottom() - size.y - 8.0))
                    .show(ui.ctx(), |ui| {
                        let frame = egui::Frame::popup(ui.style())
                            .inner_margin(egui::Margin::ZERO)
                            .fill(ui.visuals().panel_fill);
                        frame.show(ui, |ui| {
                            ui.set_max_size(size);
                            ui.set_min_size(size);
                            ui.horizontal(|ui| {
                                ui.add_space(10.0);
                                ui.label(RichText::new("ASSETS").small().strong().color(theme::weak()));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    ui.add_space(6.0);
                                    let hint = "Keep them in the layout";
                                    docked = ui.button(RichText::new("Dock").small()).on_hover_text(hint).clicked();
                                });
                            });
                            let _ = behavior.pane_ui(ui, TileId::from_u64(u64::MAX), &mut { assets });
                        });
                    });
                let pressed = ui.ctx().input(|i| i.pointer.any_pressed().then(|| i.pointer.latest_pos()).flatten());
                self.drawer = !pressed.is_some_and(|at| room.contains(at) && !drawn.response.rect.contains(at));
            }
            outcome.take(behavior, None);
        });
        if docked {
            self.drawer = false;
            self.show_pane(Pane::first(PaneKind::Assets));
        }
        outcome
    }

    /// What was selected since last time, whatever selected it, opens as a document.
    fn open_selected(&mut self) {
        let Some(ws) = self.workspace.as_ref() else { return };
        if ws.selected == self.opened {
            return;
        }
        self.opened.clone_from(&ws.selected);
        if let Some(path) = self.opened.as_deref().filter(|path| ws.is_document(path)) {
            self.layouts.open(path, false);
        }
    }

    /// The windows: each its own native window (an immediate viewport, drawn now, with the editor's state), with a
    /// dock-back button; closing one docks its panes back.
    fn show_windows(&mut self, ctx: &egui::Context, titles: &[(u64, String)], reveal: Option<Pane>) -> Outcome {
        let mut outcome = Outcome::default();
        let opening = self.opening_shown();
        for window in &mut self.layouts.windows {
            let id = window.id;
            ctx.show_viewport_immediate(window.viewport(), window.builder(), |ui, _class| {
                let bar = egui::Frame::new().fill(theme::background()).inner_margin(egui::Margin::symmetric(8, 4));
                egui::Panel::top(egui::Id::unique(("window bar", id))).frame(bar).show(ui, |ui| {
                    let dock = egui::Button::new(format!("{}  Dock back", icons::ARROW_SQUARE_IN));
                    if ui.add(dock).on_hover_text("Return these panes to the main window").clicked() {
                        outcome.docked.push(id);
                    }
                });
                // Its own, as eframe clears a window of this kind to nothing whatever `clear_color` says.
                let body = egui::Frame::NONE.fill(ui.visuals().panel_fill);
                egui::CentralPanel::no_frame().frame(body).show(ui, |ui| {
                    let mut behavior = Panes {
                        workspace: self.workspace.as_mut(),
                        nav: None,
                        pick: None,
                        focus: None,
                        reveal,
                        settings: &mut self.settings,
                        layers: &mut self.layers_state,
                        states: &mut self.layouts.states,
                        windows: titles,
                        window: Some(id),
                        actions: vec![],
                        history: None,
                        edit: None,
                        message: None,
                        opening: opening.clone(),
                    };
                    window.tree.ui(&mut behavior, ui);
                    outcome.take(behavior, Some(id));
                });
                if ui.ctx().input(|i| i.viewport().close_requested()) {
                    outcome.docked.push(id);
                }
                window.remember_place(ui.ctx());
            });
        }
        if let Some(focus) = self.focus.take() {
            ctx.send_viewport_cmd_to(focus, egui::ViewportCommand::Focus);
        }
        outcome
    }

    /// Apply what the layouts asked for this frame.
    fn apply(&mut self, outcome: Outcome) {
        let mut keep = None;
        if let Some(message) = outcome.message {
            self.message = Some(message);
        }
        if let Some(ws) = self.workspace.as_mut() {
            if let Some(pane) = outcome.focus {
                self.layouts.focused = Some(pane);
                if let Some(path) = self.layouts.subject(pane).map(str::to_string) {
                    ws.navigate(path);
                    self.opened.clone_from(&ws.selected);
                }
            }
            // Asked for, it opens even selected already: its tab may have been closed.
            // A part named is selected in what is shown: what the preview's boxes ask.
            if let Some(name) = outcome.nav.as_deref().and_then(|path| path.strip_prefix('#')) {
                ws.select_part(name);
            } else if let Some(path) = outcome.nav {
                // An asset with a part after `#`: the asset opened, the part selected in it.
                match path.split_once('#') {
                    Some((asset, part)) => {
                        ws.navigate(asset.to_string());
                        ws.select_part(part);
                    }
                    None => ws.navigate(path),
                }
                self.opened = None;
            } else if let Some(pick) = outcome.pick {
                ws.select(pick.path.clone());
                self.opened = None;
                keep = pick.keep.then_some(pick.path);
            }
        }
        for window in outcome.docked {
            self.layouts.dock_back(window);
        }
        for (window, tile, action) in outcome.actions {
            let closed = action == PaneAction::Close;
            if closed && self.asks_to_close(window, tile) {
                continue;
            }
            self.layouts.act(window, tile, action);
            // What comes to the front is what is selected.
            if closed
                && let Some(pane) = self.layouts.focused
                && let Some(ws) = self.workspace.as_mut()
                && let Some(path) = self.layouts.subject(pane).map(str::to_string)
            {
                ws.navigate(path);
                self.opened.clone_from(&ws.selected);
            }
        }
        self.open_selected();
        if let Some(path) = keep {
            self.layouts.open(&path, true);
        }
        let view_edit = self.workspace.as_mut().and_then(|ws| ws.view_edit.take());
        if let Some(request) = outcome.edit.or(view_edit) {
            // An edit keeps what it edited open.
            if matches!(request, EditRequest::Override(_) | EditRequest::OverrideIn(..) | EditRequest::Write { .. })
                && let Some(path) = self.workspace.as_ref().and_then(|ws| ws.selected.clone())
            {
                self.layouts.keep(&path);
            }
            self.edit(&request);
        }
        if let Some(ask) = outcome.history {
            self.history_asked(ask);
        }
    }

    /// Do what the History pane asked.
    fn history_asked(&mut self, ask: crate::history_pane::HistoryAsk) {
        use crate::history_pane::HistoryAsk;
        let author = self.author();
        let Some(ws) = &mut self.workspace else { return };
        ws.history.set_author(author);
        match ask {
            HistoryAsk::View(viewing) => ws.view(viewing),
            HistoryAsk::Restore => self.message = Some(ws.restore_viewed()),
            HistoryAsk::Revert(at) => match ws.revert_step(at) {
                Ok(label) => self.message = Some(label),
                Err(why) => self.message = Some(why),
            },
            HistoryAsk::Checkpoint(name) => ws.checkpoint(name),
            HistoryAsk::Steps(redo, count) => {
                for _ in 0..count {
                    if !self.step(redo, false, None) {
                        break;
                    }
                }
            }
        }
    }

    /// Every pane, shown where it belongs or put away, and the documents alone.
    fn window_menu(&mut self, ui: &mut egui::Ui) {
        for kind in PaneKind::with_panels().into_iter().filter(|kind| *kind != PaneKind::Document) {
            let pane = Pane::first(kind);
            let mut visible = self.pane_visible(pane);
            if ui.checkbox(&mut visible, kind.title()).clicked() {
                self.toggle_pane(pane);
            }
        }
        ui.separator();
        self.item(ui, Command::DocumentsOnly, "Documents only");
    }

    /// What a node can be made of with no file: the templates that take none.
    fn new_node_menu(&mut self, ui: &mut egui::Ui) {
        let Some(ws) = &self.workspace else { return };
        let mut listed: Vec<(String, String)> = ws
            .extensions
            .templates()
            .into_iter()
            .filter(|template| template.offered && !template.stored())
            .map(|template| (template.label.clone(), template.name.clone()))
            .collect();
        listed.sort();
        let mut chosen = None;
        ui.menu_button("New node", |ui| {
            for (label, name) in &listed {
                if ui.button(label).clicked() {
                    chosen = Some(name.clone());
                    ui.close();
                }
            }
        });
        if let Some(name) = chosen {
            self.new_node(&name, None);
        }
    }

    /// Make a node of a template - of a file, where it takes one - beside the document in use, else in the folder the
    /// browser shows; it is then named, as it is made with the template's.
    fn new_node(&mut self, template: &str, file: Option<&Path>) {
        let in_use = self.document_path();
        let Some(ws) = &mut self.workspace else { return };
        let folder = ws.new_node_folder(in_use.as_deref());
        match ws.new_node(template, file, &folder) {
            Ok(made) => {
                let place = if folder.is_empty() { "the project".to_string() } else { folder };
                self.message = Some(format!("New {} in {place}", made.label.to_lowercase()));
                let name = made.path.rsplit('/').next().unwrap_or(&made.path).to_string();
                let real = format!("{}{}", made.path, project::index::BUNDLE_SUFFIX);
                self.renaming = Some((made.uid, real, name));
            }
            Err(why) => self.error = Some(why),
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                self.item(ui, Command::NewProject, "New project…");
                self.item(ui, Command::OpenProject, "Open project…");
                self.item(ui, Command::Import, "Import…");
                self.new_node_menu(ui);
                ui.separator();
                self.item(ui, Command::Save, "Save");
                self.item(ui, Command::SaveAll, "Save all");
                self.item(ui, Command::Revert, "Revert to saved");
                self.item(ui, Command::Export, "Export…");
                ui.separator();
                self.item(ui, Command::ReloadProject, "Reload project");
                ui.separator();
                self.item(ui, Command::Quit, "Quit");
            });
            ui.menu_button("Edit", |ui| {
                let (undo, redo) = (self.step_label(false), self.step_label(true));
                let anywhere = self.workspace.as_ref().map(|ws| {
                    let label = |step: Option<&project::history::Step>| step.map(|s| s.label.clone());
                    (label(ws.history.to_undo(None)), label(ws.history.to_redo(None)))
                });
                let (undo_anywhere, redo_anywhere) = anywhere.unwrap_or_default();
                let text = |verb: &str, label: &Option<String>| {
                    label.as_ref().map_or_else(|| verb.to_string(), |l| format!("{verb} {l}"))
                };
                self.item(ui, Command::Undo, &text("Undo", &undo));
                self.item(ui, Command::Redo, &text("Redo", &redo));
                self.item(ui, Command::UndoAnywhere, &text("Undo in the project:", &undo_anywhere));
                self.item(ui, Command::RedoAnywhere, &text("Redo in the project:", &redo_anywhere));
                ui.separator();
                self.item(ui, Command::Settings, &format!("{}  Settings…", icons::GEAR));
            });
            ui.menu_button("Window", |ui| self.window_menu(ui));
            ui.menu_button("View", |ui| {
                self.item(ui, Command::Refresh, "Refresh");
                ui.separator();
                self.item(ui, Command::ResetLayout, "Reset layout");
                ui.separator();
                self.item(ui, Command::ZoomIn, "Zoom in");
                self.item(ui, Command::ZoomOut, "Zoom out");
                self.item(ui, Command::ActualSize, "Actual size");
                ui.separator();
                self.item(ui, Command::PerformanceOverlay, "Performance overlay");
                let zoom = ui.ctx().zoom_factor();
                let mut automatic = self.settings.appearance.zoom.is_none();
                if ui.checkbox(&mut automatic, "Zoom with the monitor").clicked() {
                    self.set_zoom(if automatic { None } else { Some(zoom) });
                }
            });
            ui.menu_button("Go", |ui| {
                self.item(ui, Command::Back, "Back");
                self.item(ui, Command::Forward, "Forward");
                self.item(ui, Command::Up, "Up a folder");
                ui.separator();
                self.item(ui, Command::Palette, "Commands…");
            });
            ui.separator();
            // The mode first, from the right: the path takes what is left, and gives way.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.mode_menu(ui);
                self.config_menu(ui);
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| self.toolbar(ui));
            });
        });
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let mut step = None;
        let mut folder = None;
        ui.horizontal(|ui| {
            let hint = |command: Command| format!("{} ({})", command.title(), command.keys(&self.settings));
            let what = |command: Command, label: &str| {
                format!("{} {label} ({})", command.title(), command.keys(&self.settings))
            };
            if self.workspace.is_none() {
                let text = if self.opening.is_some() { "Opening..." } else { "No project" };
                ui.label(RichText::new(text).color(theme::weak()));
                return;
            }
            let done = self.step_label(false).map(|label| what(Command::Undo, &label));
            let undone = self.step_label(true).map(|label| what(Command::Redo, &label));
            let nothing = |redo: bool| self.nothing_to_step(redo);
            let Some(ws) = &self.workspace else { return };
            for (command, icon) in [(Command::Back, icons::ARROW_LEFT), (Command::Forward, icons::ARROW_RIGHT)] {
                let at = self.visit_at(command == Command::Forward);
                let to = at.map(|at| self.visits.0[at].path.rsplit('/').next().unwrap_or_default().to_string());
                let tip = to.map_or_else(|| hint(command), |to| what(command, &to));
                if ui.add_enabled(at.is_some(), egui::Button::new(icon)).on_hover_text(tip).clicked() {
                    step = Some(command);
                }
            }
            ui.add_space(4.0);
            for (command, icon, step_of) in
                [(Command::Undo, icons::ARROW_ARC_LEFT, done), (Command::Redo, icons::ARROW_ARC_RIGHT, undone)]
            {
                let enabled = step_of.is_some();
                let tip = step_of.or_else(|| nothing(command == Command::Redo)).unwrap_or_else(|| hint(command));
                let button = ui.add_enabled(enabled, egui::Button::new(icon));
                if button.on_hover_text(&tip).on_disabled_hover_text(&tip).clicked() {
                    step = Some(command);
                }
            }
            ui.add_space(8.0);
            if let Some(path) = ws.selected.clone() {
                let parts: Vec<&str> = path.split('/').collect();
                let first = theme::fitting_steps(ui, &parts, ui.available_width());
                if first > 0 {
                    ui.label(RichText::new("…").color(theme::weak()));
                }
                // Each folder of the way opens in the browser.
                for (i, part) in parts.iter().enumerate().skip(first) {
                    if i + 1 == parts.len() {
                        ui.add(egui::Label::new(RichText::new(*part).strong()).truncate());
                    } else {
                        let link =
                            egui::Label::new(RichText::new(*part).color(theme::weak())).sense(egui::Sense::click());
                        if ui
                            .add(link)
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .on_hover_text("Show this folder")
                            .clicked()
                        {
                            folder = Some(parts[..=i].join("/"));
                        }
                        ui.label(RichText::new("/").color(theme::border().gamma_multiply(3.0)));
                    }
                }
            }
        });
        if let Some(command) = step {
            self.run(ui.ctx(), command);
        }
        if let Some(folder) = folder
            && let Some(ws) = &mut self.workspace
        {
            ws.open_folder(folder);
            self.show_pane(Pane::first(PaneKind::Assets));
        }
    }

    /// The active layer's name in the status bar; the pane it asks for.
    fn pane_buttons(&self, ui: &mut egui::Ui) -> Option<PaneKind> {
        let mut open = None;
        let drawer =
            format!("{}  Assets  {}", icons::FOLDER, if self.drawer { icons::CARET_DOWN } else { icons::CARET_UP });
        let button = egui::Button::new(RichText::new(drawer).small()).frame(false);
        if ui.add(button).on_hover_text("The assets, in a drawer over the window").clicked() {
            open = Some(PaneKind::Assets);
        }
        if let Some(layer) = self.workspace.as_ref().and_then(|ws| ws.active_layer()) {
            let label = egui::Label::new(
                RichText::new(format!("{}  {}", icons::STACK_SIMPLE, layer.name)).small().color(theme::text()),
            );
            if ui.add(label.sense(egui::Sense::click())).on_hover_text("The active layer: edits go there").clicked() {
                open = Some(PaneKind::Layers);
            }
        }
        ui.separator();
        open
    }

    /// The status bar; whether its problem count was clicked, what was asked of the unsaved documents, and a pane
    /// asked for.
    /// Whether agents may use the editor, in the status bar, and whether they can connect.
    fn agents_status(&self, ui: &mut egui::Ui, ws: &Workspace) {
        if ws.link_error.is_none() && (self.agents_drive || self.settings.agents.drive) {
            if extensions::trust::trusts_everything() {
                ui.label(RichText::new("trusting every extension").small().color(ui.visuals().warn_fg_color))
                    .on_hover_text("This run was started with --trust-everything: unsandboxed extensions run unasked");
                ui.separator();
            }
            let recent = self.drive.acted.is_some_and(|t| t.elapsed() < AGENT_ACTED);
            let (text, color) = if recent {
                ("an agent is using the editor", ui.visuals().warn_fg_color)
            } else {
                ("agents allowed", theme::weak())
            };
            ui.label(RichText::new(text).small().color(color)).on_hover_text(
                "Agents connected to the project may click, type and press keys here; what they edit is in \
                 the history. Settings, Agents.",
            );
            if recent {
                ui.ctx().request_repaint_after(AGENT_ACTED);
            }
            ui.separator();
        }
        if let Some(e) = &ws.link_error {
            ui.label(RichText::new("agents cannot connect").small().color(ui.visuals().warn_fg_color)).on_hover_text(e);
            ui.separator();
        }
        if let Some(files) = &ws.files {
            match files.state() {
                crate::files::State::Starting => {
                    ui.label(RichText::new("serving its files…").small().color(theme::weak()));
                }
                crate::files::State::Served(at, note) => {
                    if let Some(note) = note {
                        ui.label(RichText::new("not for Windows").small().color(ui.visuals().warn_fg_color))
                            .on_hover_text(note);
                    }
                    ui.label(RichText::new(format!("files at {at}")).small().color(theme::weak())).on_hover_text(
                        "The project's merged layers as a folder any program can read and write, each change a step \
                         of the history (project.local.toml, [filesystem] serve). Its host stops 30 s after nothing \
                         keeps it - the editor, `pe2-cli project serve` - once no file is open in it, unless \
                         [filesystem] keep says to serve on.",
                    );
                }
                // Right to left: the choices, then what they are for.
                crate::files::State::Failed { why, strays } if strays > 0 => {
                    let aside = project::SET_ASIDE;
                    if ui
                        .small_button("Take in")
                        .on_hover_text("Write them into the project, as edits through the folder are, and serve it")
                        .clicked()
                    {
                        let window = ui.ctx().clone();
                        files.resolve(crate::files::Strays::TakeIn, move || window.request_repaint());
                    }
                    if ui
                        .small_button("Set aside")
                        .on_hover_text(format!("Move them beside it, to {aside}, and serve it"))
                        .clicked()
                    {
                        let window = ui.ctx().clone();
                        files.resolve(crate::files::Strays::SetAside, move || window.request_repaint());
                    }
                    let (count, are) =
                        if strays == 1 { ("1 file".to_string(), "is") } else { (format!("{strays} files"), "are") };
                    let said = format!("its files are not served: {count} in its folder {are} not the project's");
                    ui.label(RichText::new(said).small().color(ui.visuals().warn_fg_color)).on_hover_text(why);
                }
                crate::files::State::Failed { why, .. } => {
                    ui.label(RichText::new("its files are not served").small().color(ui.visuals().warn_fg_color))
                        .on_hover_text(why);
                }
            }
            ui.separator();
        }
    }

    fn status_bar(&self, ui: &mut egui::Ui) -> (bool, Option<Unsaved>, Option<PaneKind>) {
        let (mut show_problems, mut unsaved, mut open) = (false, None, None);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let zoom = ui.ctx().zoom_factor();
            let mode = if self.settings.appearance.zoom.is_some() { "set in Settings" } else { "the monitor's" };
            ui.label(RichText::new(format!("{:.0}%", zoom * 100.0)).small().color(theme::weak()))
                .on_hover_text(format!("Zoom: {mode}"));
            if let Some(ws) = &self.workspace {
                ui.separator();
                if !ws.index.dangling.is_empty() {
                    let text = format!("{} dangling references", ws.index.dangling.len());
                    ui.label(RichText::new(text).small().color(ui.visuals().warn_fg_color));
                    ui.separator();
                }
                if !ws.index.problems.is_empty() {
                    let n = ws.index.problems.len();
                    let text = format!("{} {n} problem{}", icons::WARNING, if n == 1 { "" } else { "s" });
                    let label = egui::Label::new(RichText::new(text).small().color(ui.visuals().error_fg_color))
                        .sense(egui::Sense::click());
                    show_problems = ui.add(label).on_hover_cursor(egui::CursorIcon::PointingHand).clicked();
                    ui.separator();
                }
                if let Some(e) = &ws.watch_error {
                    ui.label(RichText::new("not watching").small().color(ui.visuals().warn_fg_color)).on_hover_text(e);
                    ui.separator();
                }
                // The documents with unsaved changes, each to show, save or revert.
                let documents = ws.unsaved_documents();
                if !documents.is_empty() {
                    unsaved = unsaved_menu(ui, ws, &documents);
                    ui.separator();
                }
                if fetch_status(ui, ws) {
                    open = Some(PaneKind::Extensions);
                }
                // Taking changes in, off the window's thread; or other programs' changes waiting for it.
                epoch_status(ui, ws);
                let working = ws.reading().or(ws.taking_in().then(|| "taking in changes".to_string()));
                if let Some(working) = working {
                    ui.label(RichText::new(working).small().color(theme::weak()));
                    ui.add(egui::Spinner::new().size(10.0));
                    ui.separator();
                } else if ws.pending() > 0 {
                    let n = ws.pending();
                    let text = format!("{n} change{} on disk", if n == 1 { "" } else { "s" });
                    ui.label(RichText::new(text).small().color(theme::weak())).on_hover_text(
                        "Made by other programs: taken in when the editor regains focus, or now with F5",
                    );
                    ui.separator();
                }
                if let Some(why) = ws.unprotected() {
                    ui.label(RichText::new("not locked").small().color(ui.visuals().warn_fg_color)).on_hover_text(why);
                    ui.separator();
                }
                self.agents_status(ui, ws);
                ui.label(RichText::new(format!("{} assets", ws.index.assets.len())).small().color(theme::weak()));
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                open = self.pane_buttons(ui);
                if let Some(error) = &self.error {
                    ui.add(
                        egui::Label::new(RichText::new(error).small().color(ui.visuals().error_fg_color)).truncate(),
                    );
                } else if let Some(message) = &self.message {
                    ui.add(egui::Label::new(RichText::new(message).small().color(theme::text())).truncate());
                } else {
                    let text = self
                        .workspace
                        .as_ref()
                        .map(|ws| project::host::shown(&ws.project.root))
                        .or_else(|| self.opening.as_ref().map(|o| format!("Opening {}", project::host::shown(&o.dir))))
                        .unwrap_or_else(|| "No project open".to_string());
                    ui.add(egui::Label::new(RichText::new(text).small().color(theme::weak())).truncate());
                }
            });
        });
        (show_problems, unsaved, open)
    }

    /// A project another process holds: it cannot be opened until that process lets go of it.
    fn in_use_dialog(&mut self, ctx: &egui::Context) {
        let Some((dir, owner)) = &self.in_use else { return };
        let (mut retry, mut close) = (false, false);
        egui::Window::new("Project in use")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label(RichText::new(dir.display().to_string()).strong());
                ui.add_space(4.0);
                ui.label(format!("This project is in use by {owner}."));
                ui.label(
                    RichText::new("One process works on a project at a time; it can be opened once that one is done.")
                        .color(theme::weak()),
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    retry = ui.button("Retry").clicked();
                    close = ui.button("Close").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape));
                });
            });
        if retry {
            let dir = dir.clone();
            self.in_use = None;
            self.open(&dir);
        } else if close {
            self.in_use = None;
        }
    }

    /// Files dropped on the window: winit says that they were, not where, so they go through the dialog rather than to a
    /// place guessed. While they are held over it, it says so.
    fn dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<String> =
            ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().display().to_string()).collect());
        if !dropped.is_empty() {
            self.import = Some(dropped.join("\n"));
        }
        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            egui::Area::new(egui::Id::unique("files over the window"))
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    egui::Frame::popup(ui.style()).inner_margin(egui::Margin::same(16)).show(ui, |ui| {
                        ui.label(RichText::new("Drop to import").strong());
                    });
                });
        }
    }

    /// Its generated layers filled, the step after what it asks to trust: not while that is asked, nor where nobody is
    /// there to answer.
    fn fill_ui(&mut self, ctx: &egui::Context) {
        if self.onboarding.is_some() || self.capture.is_some() || self.unattended {
            return;
        }
        let Some(ws) = self.workspace.as_mut().filter(|ws| ws.attached()) else { return };
        if let Some(said) = crate::fill::ui(ctx, ws) {
            self.message = Some(said);
        }
    }

    /// What the project opened asks.
    fn onboarding_ui(&mut self, ctx: &egui::Context) {
        let Some(onboarding) = &mut self.onboarding else { return };
        let Some(answer) = onboarding.ui(ctx) else { return };
        if matches!(answer, crate::onboarding::Answer::NotNow) {
            self.not_now = Some(onboarding.root.clone());
        } else {
            if let Err(e) = onboarding.apply(&answer) {
                self.error = Some(format!("What is trusted could not be written: {e}"));
            }
            if !onboarding.config.is_empty()
                && let Some(ws) = &self.workspace
            {
                self.settings.active_configs.insert(ws.project.root.display().to_string(), onboarding.config.clone());
                self.signal(ConfigChanged::NAME);
            }
        }
        self.onboarding = None;
    }

    /// What it is busy with, by name: nothing where it is not.
    #[must_use]
    pub fn busy_with(&self) -> Vec<&'static str> {
        let ws = self.workspace.as_ref();
        [
            ("opening", self.opening.is_some()),
            ("deferred", !self.deferred.is_empty()),
            ("taking in", ws.is_some_and(Workspace::taking_in)),
            ("starting its watch", ws.is_some_and(Workspace::watch_is_starting)),
            ("requests", ws.is_some_and(Workspace::requests_busy)),
            ("an epoch", ws.is_some_and(Workspace::in_epoch)),
            ("tests", ws.is_some_and(|ws| ws.tests.running())),
        ]
        .into_iter()
        .filter_map(|(what, busy)| busy.then_some(what))
        .collect()
    }

    /// Say, now, that it is opening the project in `dir`: before it takes the project, so that no agent's server takes
    /// it first for a request of its own.
    fn announce_opening(&mut self, dir: &Path) {
        let Some(keeper) = &mut self.presence else { return };
        keeper.keep_now(mcp::presence::Presence {
            opening: Some(std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf()).display().to_string()),
            stage: Some("Starting".to_string()),
            agents: self.agents_drive || self.settings.agents.drive,
            ..mcp::presence::Presence::default()
        });
    }

    /// Say what it is doing, to whoever lists the editors on this computer: written where it changed.
    fn keep_presence(&mut self, ctx: &egui::Context) {
        if self.presence.is_none() {
            return;
        }
        let ws = self.workspace.as_ref();
        let opening = self.opening.as_ref().map(|opening| {
            std::fs::canonicalize(&opening.dir).unwrap_or_else(|_| opening.dir.clone()).display().to_string()
        });
        let stage = self.opening.as_ref().map(|opening| {
            let (stage, done, total) = opening.progress.lock().map_or_else(|p| p.into_inner().clone(), |p| p.clone());
            if total > 0 { format!("{stage} {done}/{total}") } else { stage }
        });
        let last_run = ws.and_then(|ws| ws.last_epoch.as_ref()).map(|run| {
            let report = &run.report;
            let cancelled = if report.cancelled { ", cancelled" } else { "" };
            let failed = report.failed.iter().take(3).fold(String::new(), |mut said, (asset, why)| {
                let _ = std::fmt::Write::write_fmt(&mut said, format_args!("; {asset}: {why}"));
                said
            });
            format!(
                "{}: {} made, {} failed{cancelled}{failed}",
                run.roots.join(", "),
                report.made.len(),
                report.failed.len()
            )
        });
        let window = ctx.input(|i| {
            let viewport = i.viewport();
            if viewport.minimized == Some(true) {
                "minimized"
            } else if viewport.focused == Some(true) {
                "focused"
            } else {
                "in the background"
            }
        });
        let presence = mcp::presence::Presence {
            project: ws.map(|ws| ws.project.root.display().to_string()),
            opening,
            stage,
            busy: self.busy_with().into_iter().map(str::to_string).collect(),
            asking: self.onboarding.is_some(),
            error: self.error.clone(),
            agents: self.agents_drive || self.settings.agents.drive,
            fetching: ws.and_then(|ws| ws.fetcher.status()).map(|(said, _)| said),
            last_run,
            problems: ws.map_or(0, |ws| ws.index.problems.len()),
            window: window.to_string(),
            ..mcp::presence::Presence::default()
        };
        if let Some(keeper) = &mut self.presence {
            keeper.keep(presence);
        }
    }

    /// What the project's extensions fetch onto this machine, fetched beside the editor as they run: trusting them is
    /// consenting to it. A run that only looks, as a screenshot does, fetches nothing.
    fn fetch(&mut self, ctx: &egui::Context) {
        if self.capture.is_some() {
            return;
        }
        if let Some(ws) = &mut self.workspace {
            let extensions = ws.extensions.clone();
            ws.fetcher.tick(&extensions, ctx);
        }
    }

    /// Importing files into the selected asset: from a drop or from the menu, which takes pasted paths.
    fn import_dialog(&mut self, ctx: &egui::Context) {
        let Some(text) = &mut self.import else { return };
        let Some(ws) = &self.workspace else { return };
        let asset = ws.selected.as_ref().and_then(|path| ws.index.assets.get(path)).cloned();
        let files: Vec<PathBuf> = text.lines().filter_map(local_path).collect();
        let (mut replace, mut add, mut cancel) = (false, false, false);
        let mut made: Option<(String, PathBuf)> = None;
        egui::Window::new("Import")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label("Files, one a line:");
                ui.add(egui::TextEdit::multiline(text).desired_width(460.0).desired_rows(2));
                ui.add_space(4.0);
                // A node of its own, as the templates taking it make one.
                if let [file] = files.as_slice() {
                    let name = file.file_name().and_then(|n| n.to_str()).unwrap_or_default();
                    for template in ws.extensions.templates_for(name).into_iter().filter(|t| t.offered && file.is_file()) {
                        if ui.button(format!("New node: {}", template.label)).clicked() {
                            made = Some((template.name.clone(), file.clone()));
                        }
                    }
                }
                let Some(asset) = &asset else {
                    ui.label(RichText::new("Select the asset they go into").color(theme::weak()));
                    cancel = ui.button("Cancel").clicked();
                    return;
                };
                let name = asset.path.rsplit('/').next().unwrap_or(&asset.path);
                let missing =
                    files.iter().find(|file| !file.is_file()).map(|file| format!("{} is not a file", file.display()));
                let name_of =
                    |file: &PathBuf| file.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
                let stack = match files.as_slice() {
                    [] => Err("No file yet".to_string()),
                    [file] => ws
                        .extensions
                        .stack_for(asset, &name_of(file))
                        .unwrap_or_else(|| Err(format!("Nothing imports it over {name}"))),
                    _ => Err("One file replaces an asset's source".to_string()),
                };
                ui.horizontal(|ui| {
                    let can = stack.is_ok() && missing.is_none();
                    let button = ui.add_enabled(can, egui::Button::new(format!("Replace the source of {name}")));
                    let button = button
                        .on_disabled_hover_text(stack.clone().err().or_else(|| missing.clone()).unwrap_or_default());
                    replace = button.clicked();
                    let can = !files.is_empty() && missing.is_none();
                    add = ui.add_enabled(can, egui::Button::new(format!("Add to {name}"))).clicked();
                    cancel = ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape));
                });
                if let Some(why) = missing.or(stack.err()) {
                    ui.label(RichText::new(why).small().color(theme::weak()));
                }
            });
        if let Some((template, file)) = made {
            self.import = None;
            self.new_node(&template, Some(&file));
            return;
        }
        let Some(ws) = &self.workspace else { return };
        let (Some(asset), true) = (asset, replace || add) else {
            if cancel {
                self.import = None;
            }
            return;
        };
        let requests: Result<Vec<EditRequest>, String> = if replace {
            ws.replacing(&asset, &files[0])
        } else {
            files.iter().map(|file| crate::workspace::adding(&asset, file)).collect()
        };
        self.import = None;
        match requests {
            Ok(requests) => {
                self.layouts.keep(&asset.path);
                for request in &requests {
                    self.edit(request);
                }
            }
            Err(why) => self.error = Some(why),
        }
    }

    /// Make an empty project where the user says, and open it.
    fn new_dialog(&mut self, ctx: &egui::Context) {
        let Some(text) = &mut self.new_dialog else { return };
        let (mut make, mut cancel) = (false, false);
        egui::Window::new("New project")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label("The directory to make it in, empty or not there yet:");
                let edit = ui.add(egui::TextEdit::singleline(text).desired_width(460.0));
                if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    make = true;
                }
                edit.request_focus();
                ui.horizontal(|ui| {
                    make |= ui.button("Make").clicked();
                    cancel = ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape));
                });
            });
        if make {
            let dir = PathBuf::from(text.trim());
            self.new_dialog = None;
            match project::create(&dir) {
                Ok(()) => self.open(&dir),
                Err(why) => self.message = Some(format!("No project was made: {why:#}")),
            }
        } else if cancel {
            self.new_dialog = None;
        }
    }

    fn open_dialog(&mut self, ctx: &egui::Context) {
        let Some(text) = &mut self.open_dialog else { return };
        let (mut open, mut cancel) = (false, false);
        egui::Window::new("Open project")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label("The project's directory, the one holding its project.toml:");
                let edit = ui.add(egui::TextEdit::singleline(text).desired_width(460.0));
                if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    open = true;
                }
                edit.request_focus();
                ui.horizontal(|ui| {
                    open |= ui.button("Open").clicked();
                    cancel = ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape));
                });
            });
        if open {
            let dir = PathBuf::from(text.trim());
            self.open_dialog = None;
            self.open(&dir);
        } else if cancel {
            self.open_dialog = None;
        }
    }

    /// Advance a pending screenshot: request it once settled, close once it is saved.
    /// Advance a pending screenshot: request it once settled, close once it is saved. The main window only: eframe
    /// paints the other windows (immediate viewports) without taking screenshots of them.
    fn capture(&mut self, ctx: &egui::Context) {
        // The window is settled only once its project is open, and whole.
        if self.opening.is_some() || self.workspace.as_ref().is_some_and(|ws| !ws.attached()) {
            return;
        }
        let Some(capture) = &mut self.capture else { return };
        if capture.requested && capture.pending.load(Ordering::Acquire) == 0 {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        capture.frames += 1;
        if !capture.requested && capture.frames >= CAPTURE_AFTER_FRAMES {
            capture.requested = true;
            capture.pending.store(1, Ordering::Release);
            let (path, pending, repaint) = (capture.path.clone(), capture.pending.clone(), ctx.clone());
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |image| {
                if let Err(e) = save_png(&path, &image) {
                    eprintln!("{}: {e}", path.display());
                }
                pending.store(0, Ordering::Release);
                repaint.request_repaint();
            })));
        }
        ctx.request_repaint();
    }
}

/// The window's monitor's height in pixels: its size in points times the pixels per point - right once the scale has
/// held for a frame or two, since egui measures the size with the previous frame's scale.
fn monitor_height(ctx: &egui::Context) -> Option<f32> {
    ctx.input(|i| i.viewport().monitor_size.map(|size| size.y * i.pixels_per_point))
}

/// The zoom that makes the editor look the same on any monitor: its height over 1080, in quarter steps, divided by
/// the scale the OS already applies.
fn automatic_zoom(ctx: &egui::Context, height: f32) -> f32 {
    let native = ctx.native_pixels_per_point().unwrap_or(1.0);
    let scale = ((height / REFERENCE_HEIGHT) * 4.0).round() / 4.0;
    (scale.max(1.0) / native).clamp(1.0, 4.0)
}

#[cfg(any(test, feature = "tests"))]
impl Editor {
    /// What it has open.
    pub fn workspace(&mut self) -> Option<&mut Workspace> {
        self.workspace.as_mut()
    }

    /// Ask for an edit, as a pane does.
    #[cfg(any(test, feature = "tests"))]
    pub fn ask(&mut self, request: &EditRequest) {
        self.edit(request);
    }

    /// Open a project, as the menu does.
    #[cfg(test)]
    pub fn open_project(&mut self, dir: &Path) {
        self.open(dir);
    }

    /// Whether something asked waits for the project to be read.
    #[cfg(test)]
    pub fn is_deferring(&self) -> bool {
        !self.deferred.is_empty()
    }

    /// Whether a project is being opened.
    pub fn is_opening(&self) -> bool {
        self.opening.is_some()
    }

    /// Whether something it set going has yet to show.
    pub fn busy(&self) -> bool {
        !self.busy_with().is_empty()
    }

    /// The asset the document in use shows.
    pub fn document_in_use(&self) -> Option<String> {
        let pane = self.layouts.active_document()?;
        self.layouts.subject(pane).map(str::to_string)
    }

    /// Undo, as the command does: the last edit of the document in use, or the project's last change.
    pub fn undo(&mut self) -> bool {
        self.step_scope(false).is_some_and(|scope| self.step(false, false, scope.uid()))
    }

    /// Whether a project is asking something.
    pub fn asking(&self) -> bool {
        self.onboarding.is_some()
    }

    /// Why opening failed, or something went wrong since.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

impl eframe::App for Editor {
    /// What is behind the panes, which paint none of their own: eframe's is see-through, and comes out by what is under it.
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        visuals.panel_fill.to_normalized_gamma_f32()
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        // The frame before ended where this one begins.
        project::profile::frame();
        let _span = tracing::info_span!("frame").entered();
        let ctx = ui.ctx().clone();
        let pixels = ctx.content_rect().size() * ctx.pixels_per_point();
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a window's pixels")]
        crate::perf::frame(frame.info().cpu_usage, [pixels.x as u32, pixels.y as u32]);
        if self.settings.performance_overlay {
            crate::perf::overlay(&ctx);
        } else {
            crate::perf::overlay_hidden();
        }
        if let Some(max) = self.max_surface.take() {
            // In points: the surface is the window's size times the scale factor.
            #[expect(clippy::cast_precision_loss, reason = "texture limits are far below f32's exact integers")]
            let side = max as f32 / ctx.pixels_per_point();
            ctx.send_viewport_cmd(egui::ViewportCommand::MaxInnerSize(egui::vec2(side, side)));
        }
        self.zoom(&ctx);
        self.ask_before_closing(&ctx);
        self.capture(&ctx);
        self.opened();
        self.follow_disk(&ctx);
        self.do_deferred();
        // The window is the agents' from the first frame, whatever is still being read.
        self.answer_while_opening(&ctx);
        if let Some(ws) = self.workspace.as_mut() {
            self.drive.take(&ctx, std::mem::take(&mut ws.window_requests));
        }
        // What a project asks is for a person to answer: agents wait.
        self.drive.asking = self.onboarding.is_some();
        let drive = (self.agents_drive || self.settings.agents.drive) && !self.drive.asking;
        self.drive.update(&ctx, drive);
        self.agent_commands(&ctx, drive);
        // What is done this frame, by whoever gave its input.
        let author = self.author();
        if let Some(ws) = &mut self.workspace {
            ws.history.set_author(author);
        }
        self.note_place(&ctx);
        // A frame of the extensions' readings: one unused for long enough gives its memory back.
        if let Some(ws) = &self.workspace {
            ws.extensions.frame();
        }
        self.shortcuts(&ctx);
        let bar = egui::Frame::new().fill(theme::background()).inner_margin(egui::Margin::symmetric(6, 1));
        // One row: what a window can spare for what is not its work.
        egui::Panel::top("menu").frame(bar).show(ui, |ui| {
            self.menu_bar(ui);
        });
        let (show_problems, unsaved, open) =
            egui::Panel::bottom("status").frame(bar).show(ui, |ui| self.status_bar(ui)).inner;
        if show_problems {
            self.show_pane(Pane::first(PaneKind::Problems));
        }
        match open {
            Some(PaneKind::Assets) if self.layouts.is_showing(Pane::first(PaneKind::Assets)) => {}
            Some(PaneKind::Assets) => self.drawer = !self.drawer,
            Some(kind) => self.show_pane(Pane::first(kind)),
            None => {}
        }
        if let Some(unsaved) = unsaved {
            self.unsaved_asked(unsaved);
        }
        let titles: Vec<(u64, String)> = self.layouts.windows.iter().map(|w| (w.id, w.title())).collect();
        let reveal = self.layouts.reveal.take();
        let mut outcome = self.center(ui, &titles, reveal);
        // Only one that takes a while: most are done before there would be time to read it.
        let long = self.opening.as_ref().is_some_and(|opening| {
            let waited = opening.began.elapsed();
            if waited < CARD_AFTER {
                ctx.request_repaint_after(CARD_AFTER.saturating_sub(waited));
            }
            waited >= CARD_AFTER
        });
        if let Some(opening) = self.opening_shown().filter(|_| long) {
            panes::opening_ui(&ctx, &opening);
        }
        let windows = self.show_windows(&ctx, &titles, reveal);
        outcome.merge(windows);
        self.apply(outcome);
        self.note_visit();
        crate::dnd::ghost(&ctx);
        let layouts = &self.layouts;
        if let Some(ws) = self.workspace.as_mut() {
            ws.settle_players(|pane| layouts.find(pane).is_some());
        }
        self.dialogs(&ctx);
        if self.settings != self.saved {
            if self.settings.appearance != self.saved.appearance {
                theme::apply(&ctx, &self.settings.appearance);
            }
            if let Err(e) = self.settings.save() {
                self.error = Some(format!("settings: {e}"));
            }
            // Extensions read their user settings as they load: they are loaded again with the project read again.
            let reload = self.settings.extensions != self.saved.extensions;
            self.saved = self.settings.clone();
            if reload && let Some(ws) = &mut self.workspace {
                ws.reload("an extension's setting changed");
            }
        }
    }

    /// Closing: the project's session closed cleanly, what it kept wiped - unless documents are still unsaved, which the
    /// next opening offers back.
    fn on_exit(&mut self) {
        if let Some(ws) = self.workspace.take() {
            drop(ws.leave());
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.leave_bare();
        self.settings.main_window = self.place.map(|place| crate::settings::WindowState {
            place,
            maximized: self.filling.0,
            fullscreen: self.filling.1,
        });
        if self.settings != self.saved {
            let _ = self.settings.save();
            self.saved = self.settings.clone();
        }
        for key in OLD_KEYS {
            storage.set_string(key, String::new());
        }
        let layouts = &self.layouts;
        if let Ok(layout) = serde_json::to_string(&layouts.main) {
            storage.set_string(LAYOUT_KEY, layout);
        }
        if let Ok(modes) = serde_json::to_string(&layouts.modes) {
            storage.set_string(MODES_KEY, modes);
        }
        storage.set_string(MODE_KEY, layouts.mode.to_string());
        if let Ok(windows) = serde_json::to_string(&layouts.windows) {
            storage.set_string(WINDOWS_KEY, windows);
        }
        // Maps keyed by a pane are lists: JSON keys are strings.
        if let Ok(states) = serde_json::to_string(&layouts.states.iter().collect::<Vec<_>>()) {
            storage.set_string(PANES_KEY, states);
        }
        if let Ok(homes) = serde_json::to_string(&layouts.homes.iter().collect::<Vec<_>>()) {
            storage.set_string(HOMES_KEY, homes);
        }
    }
}

/// An export being chosen: the asset, its exporters, the one chosen and its member, and where its file goes - written
/// by the user (`typed`), or following what is chosen.
struct Exporting {
    path: String,
    exporters: Vec<project::ext::Exporter>,
    chosen: usize,
    member: usize,
    to: String,
    typed: bool,
    /// Its folder shown once it is written.
    reveal: bool,
}

impl Exporting {
    /// Where its file goes unless told: the project's `exports` folder, named after the asset and its member.
    fn default_to(&self, root: &Path) -> String {
        let exporter = &self.exporters.get(self.chosen);
        let name = self.path.rsplit('/').next().unwrap_or(&self.path);
        let member = exporter.and_then(|e| e.members.get(self.member)).map(|(_, label)| format!("-{label}"));
        let extension = exporter.map_or("", |e| e.extension.as_str());
        let file: String = format!("{name}{}.{extension}", member.unwrap_or_default())
            .chars()
            .map(|c| if c == '/' || c == '\\' { '_' } else { c })
            .collect();
        root.join("exports").join(file).display().to_string()
    }
}

/// What undo and redo take back: a document's last edit, or the project's last change.
enum StepScope {
    Document(String),
    Project,
}

impl StepScope {
    /// The document's uid; none: the project's.
    fn uid(self) -> Option<String> {
        match self {
            StepScope::Document(uid) => Some(uid),
            StepScope::Project => None,
        }
    }
}

/// How many documents back and forward remember.
const VISITS: usize = 100;

/// A document brought into focus: its path, its uid (which finds it where it moved), and how it was shown.
#[derive(Clone, Debug)]
struct Visit {
    path: String,
    uid: Option<String>,
    editor: Option<EditorKind>,
}

impl Visit {
    /// Where it is now: by its uid, else its path; none where it is gone.
    fn place(&self, ws: &Workspace) -> Option<String> {
        if let Some(path) = self.uid.as_ref().and_then(|uid| ws.index.by_uid.get(uid)) {
            return Some(path.clone());
        }
        ws.exists(&self.path).then(|| self.path.clone())
    }
}

/// What is asked of the documents with unsaved changes, in the status bar.
enum Unsaved {
    SaveAll,
    Save(String),
    Revert(String),
    /// Its path: shown.
    Show(String),
}

/// The unsaved documents' count in the status bar, opening their list: each shown, saved or reverted, or all saved.
fn unsaved_menu(ui: &mut egui::Ui, ws: &Workspace, documents: &[(String, String)]) -> Option<Unsaved> {
    let mut asked = None;
    let n = documents.len();
    let text = RichText::new(format!("{n} unsaved")).small().color(ui.visuals().warn_fg_color);
    ui.menu_button(text, |ui| {
        for (uid, path) in documents {
            let stale = ws.history.document(uid).is_some_and(|d| d.stale.is_some());
            ui.horizontal(|ui| {
                let name = path.rsplit('/').next().unwrap_or(path);
                let label = if stale { format!("{name}  (changed on disk)") } else { name.to_string() };
                if ui.link(label).on_hover_text(path).clicked() {
                    asked = Some(Unsaved::Show(path.clone()));
                }
                if ui.small_button("Save").clicked() {
                    asked = Some(Unsaved::Save(uid.clone()));
                }
                if ui.small_button("Revert").clicked() {
                    asked = Some(Unsaved::Revert(uid.clone()));
                }
            });
        }
        ui.separator();
        if ui.button("Save all").clicked() {
            asked = Some(Unsaved::SaveAll);
        }
        if asked.is_some() {
            ui.close();
        }
    })
    .response
    .on_hover_text("Documents with changes not saved: what is shown and made follows them; saving writes them");
    asked
}

/// Code being overridden: its uid, path and own layer, the files layers that can take it, a new one's name.
struct Choosing {
    uid: String,
    path: String,
    home: String,
    layers: Vec<String>,
    name: String,
}

/// What the layouts asked for in a frame: where to go, a message, windows to dock back, tab actions by window.
#[derive(Default)]
struct Outcome {
    nav: Option<String>,
    pick: Option<panes::Pick>,
    focus: Option<Pane>,
    message: Option<String>,
    docked: Vec<u64>,
    actions: Vec<(Option<u64>, TileId, PaneAction)>,
    history: Option<crate::history_pane::HistoryAsk>,
    edit: Option<EditRequest>,
}

impl Outcome {
    fn take(&mut self, behavior: Panes<'_>, window: Option<u64>) {
        self.nav = self.nav.take().or(behavior.nav);
        self.pick = self.pick.take().or(behavior.pick);
        self.focus = self.focus.take().or(behavior.focus);
        self.message = self.message.take().or(behavior.message);
        self.history = self.history.take().or(behavior.history);
        self.edit = self.edit.take().or(behavior.edit);
        self.actions.extend(behavior.actions.into_iter().map(|(tile, action)| (window, tile, action)));
    }

    fn merge(&mut self, other: Outcome) {
        self.nav = self.nav.take().or(other.nav);
        self.pick = self.pick.take().or(other.pick);
        self.focus = self.focus.take().or(other.focus);
        self.message = self.message.take().or(other.message);
        self.docked.extend(other.docked);
        self.actions.extend(other.actions);
        self.history = self.history.take().or(other.history);
        self.edit = self.edit.take().or(other.edit);
    }
}

/// A line of pasted paths as a path here: without the quotes Windows' Copy as path puts round it, and under WSL a Windows
/// path where Windows' disks are mounted.
fn local_path(line: &str) -> Option<PathBuf> {
    let line = line.trim().trim_matches('"');
    if line.is_empty() {
        return None;
    }
    if cfg!(target_os = "linux")
        && let [drive, b':', b'\\' | b'/', ..] = line.as_bytes()
        && drive.is_ascii_alphabetic()
    {
        let rest = line[3..].replace('\\', "/");
        return Some(PathBuf::from(format!("/mnt/{}/{rest}", drive.to_ascii_lowercase() as char)));
    }
    Some(PathBuf::from(line))
}

fn save_png(path: &Path, image: &egui::ColorImage) -> Result<(), Box<dyn std::error::Error>> {
    let [width, height] = image.size;
    let mut encoder = png::Encoder::new(
        std::io::BufWriter::new(std::fs::File::create(path)?),
        u32::try_from(width)?,
        u32::try_from(height)?,
    );
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let pixels: Vec<u8> = image.pixels.iter().flat_map(egui::Color32::to_array).collect();
    encoder.write_header()?.write_image_data(&pixels)?;
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::local_path;
    use std::path::PathBuf;

    #[test]
    fn a_pasted_path_is_read_as_a_path_here() {
        assert_eq!(local_path("  \"/tmp/a b.mp4\" "), Some(PathBuf::from("/tmp/a b.mp4")));
        assert_eq!(local_path(r#""C:\Users\me\a.mp4""#), Some(PathBuf::from("/mnt/c/Users/me/a.mp4")));
        assert_eq!(local_path("   "), None);
    }
}
