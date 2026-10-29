//! The editor's window: a menu bar and a toolbar on top, the panes in a tile layout, a status bar below; and the
//! windows panes were moved into (`windows.rs`). The layouts, the windows and each pane instance's state are
//! remembered in eframe's storage, the editor's settings in their own `settings.toml`.

use eframe::egui::{self, RichText};
use egui_tiles::TileId;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::panes::{self, Pane, PaneAction, PaneKind, PaneState, Panes};
use crate::settings::EditorSettings;
use crate::windows::{Layouts, Window};
use crate::workspace::{EditRequest, Update, Workspace};
use crate::{icons, theme};
use project::history::StepError;

const LAYOUT_KEY: &str = "layout";
const WINDOWS_KEY: &str = "windows";
const PANES_KEY: &str = "panes";
const HOMES_KEY: &str = "homes";
const SETTINGS_KEY: &str = "settings";
/// How long the status bar says an agent is using the editor after it last acted.
const AGENT_ACTED: std::time::Duration = std::time::Duration::from_secs(5);

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

pub struct Editor {
    workspace: Option<Workspace>,
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
    /// A step that files changed outside the editor block: redo (true) or undo, and why.
    force: Option<(bool, String)>,
    /// An entry being renamed: its uid, its real path, the new name being typed.
    renaming: Option<(String, String, String)>,
    /// Code being overridden while an asset layer is active: which files layer takes it.
    choosing: Option<Choosing>,
    layers_state: crate::layers::LayersState,
    /// A project that could not be opened because another process holds it: its directory and who holds it.
    in_use: Option<(PathBuf, String)>,
    error: Option<String>,
    capture: Option<Capture>,
    /// The device's largest surface side in pixels, which the window must not outgrow; applied once, then taken.
    max_surface: Option<u32>,
    /// Agents using the window, and whether this session lets them act whatever the settings say.
    drive: crate::drive::Driver,
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
}

impl Editor {
    pub fn new(cc: &eframe::CreationContext<'_>, start: Start) -> Editor {
        let stored = |key| cc.storage.and_then(|s| s.get_string(key));
        let layout =
            stored(LAYOUT_KEY).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_else(panes::default_layout);
        let windows: Vec<Window> = stored(WINDOWS_KEY).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let states: Vec<(Pane, PaneState)> =
            stored(PANES_KEY).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let homes: Vec<(Pane, TileId)> =
            stored(HOMES_KEY).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let (mut settings, problem) = EditorSettings::load();
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
            layouts: Layouts {
                main: layout,
                windows,
                states: states.into_iter().collect(),
                homes: homes.into_iter().collect(),
            },
            focus: None,
            saved: settings.clone(),
            settings,
            message: None,
            applied_zoom: None,
            zoomed_for: None,
            steady: (0.0, 0),
            open_dialog: None,
            force: None,
            renaming: None,
            choosing: None,
            layers_state: crate::layers::LayersState::default(),
            in_use: None,
            error: problem,
            capture: start.screenshot.map(|path| Capture {
                path,
                frames: 0,
                requested: false,
                pending: Arc::default(),
            }),
            max_surface: start.max_surface,
            drive: crate::drive::Driver::new(),
            agents_drive: start.agents_drive,
        };
        if let Some(dir) = project {
            editor.open(&dir);
        }
        if let (Some(ws), Some(asset)) = (&mut editor.workspace, start.select) {
            ws.navigate(asset);
        }
        if let Some(kind) =
            start.show.and_then(|name| PaneKind::with_panels().into_iter().find(|k| k.title().eq_ignore_ascii_case(&name)))
        {
            editor.show_pane(Pane::first(kind));
        }
        editor
    }

    fn open(&mut self, dir: &Path) {
        self.open_keeping(dir, None);
    }

    /// Open a project; with a history, the same project again, its session going on.
    fn open_keeping(&mut self, dir: &Path, history: Option<project::history::History>) {
        // The project open now lets go of its lock first: opening it again must find it free.
        self.workspace = None;
        match Workspace::open_keeping(dir, history) {
            Ok(ws) => {
                self.settings.last_project = std::path::absolute(dir).ok();
                if let Some(label) = &ws.recovered {
                    self.message = Some(format!("An edit a crash interrupted was rolled back: {label}"));
                }
                self.workspace = Some(ws);
                self.error = None;
            }
            Err(e) => {
                if let Some(project::lock::LockError::Held(owner)) = e.downcast_ref() {
                    self.in_use = Some((dir.to_path_buf(), owner.clone()));
                }
                self.error = Some(format!("{}: {e:#}", dir.display()));
            }
        }
    }

    /// Bring in what changed on disk; open the project again when its `project.toml` changed, keeping the selection.
    fn follow_disk(&mut self, ctx: &egui::Context) {
        let Some(ws) = &mut self.workspace else { return };
        ws.watch(ctx);
        ws.answer_agents();
        match ws.update() {
            Update::Nothing => {}
            Update::Refreshed(n) => {
                self.message = Some(format!("{n} asset{} changed on disk", if n == 1 { "" } else { "s" }));
            }
            Update::Reopen(why) => {
                self.reopen();
                self.message = Some(format!("{why}: the project was opened again"));
            }
        }
    }

    /// Open the project again - `project.toml` changed - keeping the session's history and the selection.
    fn reopen(&mut self) {
        let Some(ws) = self.workspace.take() else { return };
        let (root, selected) = (ws.project.root.clone(), ws.selected.clone());
        self.open_keeping(&root, Some(ws.into_history()));
        if let (Some(ws), Some(path)) = (&mut self.workspace, selected)
            && ws.exists(&path)
        {
            ws.navigate(path);
        }
    }

    /// Undo the last step, or redo; forced over files changed outside the editor. Blocked by such files, it asks.
    fn step(&mut self, redo: bool, force: bool) -> bool {
        let Some(ws) = &mut self.workspace else { return false };
        let verb = if redo { "Redo" } else { "Undo" };
        match ws.step(redo, force) {
            Ok((label, update)) => {
                self.message = Some(format!("{}: {label}", if redo { "Redone" } else { "Undone" }));
                if matches!(update, Update::Reopen(_)) {
                    self.reopen();
                }
                true
            }
            Err(StepError::Changed(paths)) => {
                self.force = Some((redo, StepError::Changed(paths).to_string()));
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
        let Some(ws) = &mut self.workspace else { return };
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
    /// opened again, then the override made in it).
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
                Some(Ok(())) => {
                    // The new layer is in project.toml: the project, opened again, has it.
                    self.reopen();
                    self.edit(&EditRequest::OverrideIn(uid, name));
                }
                Some(Err(e)) => self.message = Some(format!("{e:#}")),
                None => {}
            }
        }
    }

    /// The new name of an entry being renamed: in its folder, a bundle keeping its `.asset`.
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
                let field = ui.add(egui::TextEdit::singleline(name).desired_width(320.0));
                field.request_focus();
                if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    go = valid;
                }
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
        let Some((redo, why)) = &self.force else { return };
        let (redo, mut go, mut cancel) = (*redo, false, false);
        egui::Window::new("Changed outside the editor")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_max_width(460.0);
                ui.label(format!("Files this step touches {why}."));
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
            self.step(redo, true);
        } else if cancel {
            self.force = None;
        }
    }

    /// Ctrl+Z undoes, Ctrl+Shift+Z and Ctrl+Y redo - unless a text field has the keyboard, which undoes its own text.
    fn history_keys(&mut self, ctx: &egui::Context) {
        if self.workspace.is_none() || ctx.memory(|m| m.focused().is_some()) {
            return;
        }
        let shortcut = |modifiers, key| egui::KeyboardShortcut::new(modifiers, key);
        let (command, shift) = (egui::Modifiers::COMMAND, egui::Modifiers::COMMAND | egui::Modifiers::SHIFT);
        let redo = ctx.input_mut(|i| {
            i.consume_shortcut(&shortcut(shift, egui::Key::Z)) || i.consume_shortcut(&shortcut(command, egui::Key::Y))
        });
        if redo {
            self.step(true, false);
        } else if ctx.input_mut(|i| i.consume_shortcut(&shortcut(command, egui::Key::Z))) {
            self.step(false, false);
        }
    }

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
        self.focus = self.layouts.show(pane);
    }

    /// The windows: each its own native window (an immediate viewport, drawn now, with the editor's state), with a
    /// dock-back button; closing one docks its panes back.
    fn show_windows(&mut self, ctx: &egui::Context, titles: &[(u64, String)]) -> Outcome {
        let mut outcome = Outcome::default();
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
                egui::CentralPanel::no_frame().show(ui, |ui| {
                    let mut behavior = Panes {
                        workspace: self.workspace.as_mut(),
                        nav: None,
                        pick: None,
                        settings: &mut self.settings,
                        layers: &mut self.layers_state,
                        states: &mut self.layouts.states,
                        windows: titles,
                        window: Some(id),
                        actions: vec![],
                        steps: None,
                        edit: None,
                        message: None,
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
        if let Some(message) = outcome.message {
            self.message = Some(message);
        }
        if let Some(ws) = self.workspace.as_mut() {
            if let Some(path) = outcome.nav {
                ws.navigate(path);
            } else if let Some(path) = outcome.pick {
                ws.select(path);
            }
        }
        for window in outcome.docked {
            self.layouts.dock_back(window);
        }
        for (window, tile, action) in outcome.actions {
            self.layouts.act(window, tile, action);
        }
        let view_edit = self.workspace.as_mut().and_then(|ws| ws.view_edit.take());
        if let Some(request) = outcome.edit.or(view_edit) {
            self.edit(&request);
        }
        if let Some((redo, count)) = outcome.steps {
            for _ in 0..count {
                if !self.step(redo, false) {
                    break;
                }
            }
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("Open project…").clicked() {
                    self.open_dialog = Some(
                        self.workspace.as_ref().map_or_else(String::new, |w| w.project.root.display().to_string()),
                    );
                }
                if let Some(ws) = &self.workspace
                    && ui.button("Reload project").clicked()
                {
                    let root = ws.project.root.clone();
                    self.open(&root);
                }
                ui.separator();
                if ui.button("Quit").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("Edit", |ui| {
                let labels = self.workspace.as_ref().map(|ws| {
                    let label = |step: Option<&project::history::Step>| step.map(|s| s.label.clone());
                    (label(ws.history.done().last()), label(ws.history.undone().last()))
                });
                let (undo, redo) = labels.unwrap_or_default();
                let item = |verb: &str, label: &Option<String>, keys: &str| {
                    let text = label.as_ref().map_or_else(|| verb.to_string(), |l| format!("{verb} {l}"));
                    egui::Button::new(text).shortcut_text(keys)
                };
                if ui.add_enabled(undo.is_some(), item("Undo", &undo, "Ctrl+Z")).clicked() {
                    self.step(false, false);
                }
                if ui.add_enabled(redo.is_some(), item("Redo", &redo, "Ctrl+Shift+Z")).clicked() {
                    self.step(true, false);
                }
                ui.separator();
                if ui.button(format!("{}  Settings…", icons::GEAR)).clicked() {
                    self.show_pane(Pane::first(PaneKind::Settings));
                }
            });
            ui.menu_button("View", |ui| {
                for kind in PaneKind::with_panels() {
                    let pane = Pane::first(kind);
                    let mut visible = self.pane_visible(pane);
                    if ui.checkbox(&mut visible, kind.title()).clicked() {
                        self.toggle_pane(pane);
                    }
                }
                ui.separator();
                if ui.button("Reset layout").on_hover_text("Close the other windows and the extra instances").clicked()
                {
                    self.layouts = Layouts::new(panes::default_layout());
                }
                ui.separator();
                let zoom = ui.ctx().zoom_factor();
                if ui.button("Zoom in").on_hover_text("Ctrl +").clicked() {
                    self.set_zoom(Some(zoom + 0.25));
                }
                if ui.button("Zoom out").on_hover_text("Ctrl −").clicked() {
                    self.set_zoom(Some(zoom - 0.25));
                }
                if ui.button("Actual size").on_hover_text("Ctrl 0").clicked() {
                    self.set_zoom(Some(1.0));
                }
                let mut automatic = self.settings.appearance.zoom.is_none();
                if ui.checkbox(&mut automatic, "Zoom with the monitor").clicked() {
                    self.set_zoom(if automatic { None } else { Some(zoom) });
                }
            });
            ui.menu_button("Go", |ui| {
                let ws = self.workspace.as_mut();
                let (back, forward) = ws.as_ref().map_or((false, false), |w| (w.can_go_back(), w.can_go_forward()));
                if let Some(ws) = ws {
                    if ui.add_enabled(back, egui::Button::new("Back")).clicked() {
                        ws.go_back();
                    }
                    if ui.add_enabled(forward, egui::Button::new("Forward")).clicked() {
                        ws.go_forward();
                    }
                }
            });
        });
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let Some(ws) = &mut self.workspace else {
                ui.label(RichText::new("No project").color(theme::weak()));
                return;
            };
            if ui
                .add_enabled(ws.can_go_back(), egui::Button::new(icons::CARET_LEFT))
                .on_hover_text("Back (mouse button 4)")
                .clicked()
            {
                ws.go_back();
            }
            if ui
                .add_enabled(ws.can_go_forward(), egui::Button::new(icons::CARET_RIGHT))
                .on_hover_text("Forward (mouse button 5)")
                .clicked()
            {
                ws.go_forward();
            }
            ui.add_space(8.0);
            if let Some(path) = ws.selected.clone() {
                let parts: Vec<&str> = path.split('/').collect();
                for (i, part) in parts.iter().enumerate() {
                    if i + 1 == parts.len() {
                        ui.label(RichText::new(*part).strong());
                    } else {
                        ui.label(RichText::new(*part).color(theme::weak()));
                        ui.label(RichText::new("/").color(theme::border().gamma_multiply(3.0)));
                    }
                }
            }
        });
    }

    /// The status bar; true when its problem count was clicked.
    fn status_bar(&self, ui: &mut egui::Ui) -> bool {
        let mut show_problems = false;
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let zoom = ui.ctx().zoom_factor();
            let mode = if self.settings.appearance.zoom.is_some() { "" } else { " (monitor)" };
            ui.label(RichText::new(format!("zoom {:.0}%{mode}", zoom * 100.0)).small().color(theme::weak()));
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
                if let Some(why) = ws.unprotected() {
                    ui.label(RichText::new("not locked").small().color(ui.visuals().warn_fg_color)).on_hover_text(why);
                    ui.separator();
                }
                if ws.link_error.is_none() && (self.agents_drive || self.settings.agents.drive) {
                    let recent = self.drive.acted.is_some_and(|t| t.elapsed() < AGENT_ACTED);
                    let (text, color) = if recent {
                        ("an agent is using the editor", ui.visuals().warn_fg_color)
                    } else {
                        ("agents may use the editor", theme::weak())
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
                    ui.label(RichText::new("agents cannot connect").small().color(ui.visuals().warn_fg_color))
                        .on_hover_text(e);
                    ui.separator();
                }
                ui.label(RichText::new(format!("{} assets", ws.index.assets.len())).small().color(theme::weak()));
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
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
                        .map_or_else(|| "No project open".to_string(), |ws| ws.project.root.display().to_string());
                    ui.add(egui::Label::new(RichText::new(text).small().color(theme::weak())).truncate());
                }
            });
        });
        show_problems
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
                edit.request_focus();
                if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    open = true;
                }
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

impl eframe::App for Editor {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if let Some(max) = self.max_surface.take() {
            // In points: the surface is the window's size times the scale factor.
            #[expect(clippy::cast_precision_loss, reason = "texture limits are far below f32's exact integers")]
            let side = max as f32 / ctx.pixels_per_point();
            ctx.send_viewport_cmd(egui::ViewportCommand::MaxInnerSize(egui::vec2(side, side)));
        }
        self.zoom(&ctx);
        self.capture(&ctx);
        self.follow_disk(&ctx);
        if let Some(ws) = &mut self.workspace {
            self.drive.take(&ctx, std::mem::take(&mut ws.window_requests));
        }
        self.drive.update(&ctx, self.agents_drive || self.settings.agents.drive);
        // A frame of the extensions' readings: one unused for long enough gives its memory back.
        if let Some(ws) = &self.workspace {
            ws.extensions.frame();
        }
        self.history_keys(&ctx);
        if let Some(ws) = &mut self.workspace {
            if ctx.input(|i| i.pointer.button_pressed(egui::PointerButton::Extra1)) {
                ws.go_back();
            }
            if ctx.input(|i| i.pointer.button_pressed(egui::PointerButton::Extra2)) {
                ws.go_forward();
            }
        }
        let bar = egui::Frame::new().fill(theme::background()).inner_margin(egui::Margin::symmetric(8, 4));
        egui::Panel::top("menu").frame(bar).show(ui, |ui| {
            self.menu_bar(ui);
            ui.add_space(2.0);
            self.toolbar(ui);
        });
        if egui::Panel::bottom("status").frame(bar).show(ui, |ui| self.status_bar(ui)).inner {
            self.show_pane(Pane::first(PaneKind::Problems));
        }
        let titles: Vec<(u64, String)> = self.layouts.windows.iter().map(|w| (w.id, w.title())).collect();
        let mut outcome = Outcome::default();
        egui::CentralPanel::no_frame().show(ui, |ui| {
            let mut behavior = Panes {
                workspace: self.workspace.as_mut(),
                nav: None,
                pick: None,
                settings: &mut self.settings,
                layers: &mut self.layers_state,
                states: &mut self.layouts.states,
                windows: &titles,
                window: None,
                actions: vec![],
                steps: None,
                edit: None,
                message: None,
            };
            self.layouts.main.ui(&mut behavior, ui);
            outcome.take(behavior, None);
        });
        let windows = self.show_windows(&ctx, &titles);
        outcome.merge(windows);
        self.apply(outcome);
        self.open_dialog(&ctx);
        self.in_use_dialog(&ctx);
        self.force_dialog(&ctx);
        self.rename_dialog(&ctx);
        self.choose_dialog(&ctx);
        if self.settings != self.saved {
            if self.settings.appearance != self.saved.appearance {
                theme::apply(&ctx, &self.settings.appearance);
            }
            if let Err(e) = self.settings.save() {
                self.error = Some(format!("settings: {e}"));
            }
            self.saved = self.settings.clone();
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        let layouts = &self.layouts;
        if let Ok(layout) = serde_json::to_string(&layouts.main) {
            storage.set_string(LAYOUT_KEY, layout);
        }
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
    pick: Option<String>,
    message: Option<String>,
    docked: Vec<u64>,
    actions: Vec<(Option<u64>, TileId, PaneAction)>,
    steps: Option<(bool, usize)>,
    edit: Option<EditRequest>,
}

impl Outcome {
    fn take(&mut self, behavior: Panes<'_>, window: Option<u64>) {
        self.nav = self.nav.take().or(behavior.nav);
        self.pick = self.pick.take().or(behavior.pick);
        self.message = self.message.take().or(behavior.message);
        self.steps = self.steps.take().or(behavior.steps);
        self.edit = self.edit.take().or(behavior.edit);
        self.actions.extend(behavior.actions.into_iter().map(|(tile, action)| (window, tile, action)));
    }

    fn merge(&mut self, other: Outcome) {
        self.nav = self.nav.take().or(other.nav);
        self.pick = self.pick.take().or(other.pick);
        self.message = self.message.take().or(other.message);
        self.docked.extend(other.docked);
        self.actions.extend(other.actions);
        self.steps = self.steps.take().or(other.steps);
        self.edit = self.edit.take().or(other.edit);
    }
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
