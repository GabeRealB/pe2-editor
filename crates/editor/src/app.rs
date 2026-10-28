//! The editor's window: a menu bar and a toolbar on top, the panes in a tile layout, a status bar below. The layout
//! is remembered in eframe's storage, the editor's settings in their own `settings.toml`.

use eframe::egui::{self, RichText};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::panes::{self, Pane, Panes};
use crate::settings::EditorSettings;
use crate::workspace::{Update, Workspace};
use crate::{icons, theme};

const LAYOUT_KEY: &str = "layout";
const SETTINGS_KEY: &str = "settings";
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
    done: Arc<AtomicBool>,
}

pub struct Editor {
    workspace: Option<Workspace>,
    layout: egui_tiles::Tree<Pane>,
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
    /// A project that could not be opened because another process holds it: its directory and who holds it.
    in_use: Option<(PathBuf, String)>,
    error: Option<String>,
    capture: Option<Capture>,
    /// The device's largest surface side in pixels, which the window must not outgrow; applied once, then taken.
    max_surface: Option<u32>,
}

pub struct Start {
    pub project: Option<PathBuf>,
    pub select: Option<String>,
    pub show: Option<String>,
    pub screenshot: Option<PathBuf>,
    pub max_surface: Option<u32>,
}

impl Editor {
    pub fn new(cc: &eframe::CreationContext<'_>, start: Start) -> Editor {
        let stored = |key| cc.storage.and_then(|s| s.get_string(key));
        let layout =
            stored(LAYOUT_KEY).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_else(panes::default_layout);
        let (mut settings, problem) = EditorSettings::load();
        if crate::settings::path().is_some_and(|p| !p.exists())
            && let Some(old) = stored(SETTINGS_KEY).and_then(|s| serde_json::from_str::<OldSettings>(&s).ok())
        {
            settings.appearance.zoom = old.zoom;
            settings.last_project = old.last_project;
        }
        icons::install(&cc.egui_ctx);
        theme::apply(&cc.egui_ctx, &settings.appearance);
        let last = settings.startup.reopen_last_project.then(|| settings.last_project.clone()).flatten();
        let project = start.project.or(last);
        let mut editor = Editor {
            workspace: None,
            layout,
            saved: settings.clone(),
            settings,
            message: None,
            applied_zoom: None,
            zoomed_for: None,
            steady: (0.0, 0),
            open_dialog: None,
            in_use: None,
            error: problem,
            capture: start.screenshot.map(|path| Capture { path, frames: 0, requested: false, done: Arc::default() }),
            max_surface: start.max_surface,
        };
        if let Some(dir) = project {
            editor.open(&dir);
        }
        if let (Some(ws), Some(asset)) = (&mut editor.workspace, start.select) {
            ws.navigate(asset);
        }
        if let Some(pane) =
            start.show.and_then(|name| Pane::ALL.into_iter().find(|p| p.title().eq_ignore_ascii_case(&name)))
        {
            editor.show_pane(pane);
        }
        editor
    }

    fn open(&mut self, dir: &Path) {
        // The project open now lets go of its lock first: opening it again must find it free.
        self.workspace = None;
        match Workspace::open(dir) {
            Ok(ws) => {
                self.settings.last_project = std::path::absolute(dir).ok();
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
            Update::Reopen => {
                let (root, selected) = (ws.project.root.clone(), ws.selected.clone());
                self.open(&root);
                if let (Some(ws), Some(path)) = (&mut self.workspace, selected)
                    && ws.index.assets.contains_key(&path)
                {
                    ws.navigate(path);
                }
                self.message = Some("project.toml changed: the project was opened again".into());
            }
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
        panes::tile_of(&self.layout, pane).is_some_and(|id| self.layout.tiles.is_visible(id))
    }

    /// Show or hide a pane.
    fn toggle_pane(&mut self, pane: Pane) {
        match panes::tile_of(&self.layout, pane) {
            Some(id) if self.layout.tiles.is_visible(id) => self.layout.tiles.set_visible(id, false),
            _ => self.show_pane(pane),
        }
    }

    /// Show a pane and bring its tab to the front; a pane the layout has not got joins the preview's tabs.
    fn show_pane(&mut self, pane: Pane) {
        let id = if let Some(id) = panes::tile_of(&self.layout, pane) {
            id
        } else {
            let id = self.layout.tiles.insert_pane(pane);
            let group = panes::tile_of(&self.layout, Pane::Preview)
                .and_then(|p| self.layout.tiles.parent_of(p))
                .or_else(|| self.layout.root());
            if let Some(egui_tiles::Tile::Container(container)) = group.and_then(|g| self.layout.tiles.get_mut(g)) {
                container.add_child(id);
            } else {
                self.layout = panes::default_layout();
            }
            id
        };
        self.layout.tiles.set_visible(id, true);
        if let Some(parent) = self.layout.tiles.parent_of(id)
            && let Some(egui_tiles::Tile::Container(egui_tiles::Container::Tabs(tabs))) =
                self.layout.tiles.get_mut(parent)
        {
            tabs.set_active(id);
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
                if ui.button(format!("{}  Settings…", icons::GEAR)).clicked() {
                    self.show_pane(Pane::Settings);
                }
            });
            ui.menu_button("View", |ui| {
                for pane in Pane::ALL {
                    let mut visible = self.pane_visible(pane);
                    if ui.checkbox(&mut visible, pane.title()).clicked() {
                        self.toggle_pane(pane);
                    }
                }
                ui.separator();
                if ui.button("Reset layout").clicked() {
                    self.layout = panes::default_layout();
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
    fn capture(&mut self, ctx: &egui::Context) {
        let Some(capture) = &mut self.capture else { return };
        if capture.done.load(Ordering::Acquire) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        capture.frames += 1;
        if !capture.requested && capture.frames >= CAPTURE_AFTER_FRAMES {
            capture.requested = true;
            let (path, done) = (capture.path.clone(), capture.done.clone());
            let repaint = ctx.clone();
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |image| {
                if let Err(e) = save_png(&path, &image) {
                    eprintln!("{}: {e}", path.display());
                }
                done.store(true, Ordering::Release);
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
            self.show_pane(Pane::Problems);
        }
        egui::CentralPanel::no_frame().show(ui, |ui| {
            let mut behavior = Panes {
                workspace: self.workspace.as_mut(),
                nav: None,
                pick: None,
                settings: &mut self.settings,
                message: None,
            };
            self.layout.ui(&mut behavior, ui);
            let (nav, pick) = (behavior.nav, behavior.pick);
            if let Some(message) = behavior.message {
                self.message = Some(message);
            }
            if let Some(ws) = self.workspace.as_mut() {
                if let Some(path) = nav {
                    ws.navigate(path);
                } else if let Some(path) = pick {
                    ws.select(path);
                }
            }
        });
        self.open_dialog(&ctx);
        self.in_use_dialog(&ctx);
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
        if let Ok(layout) = serde_json::to_string(&self.layout) {
            storage.set_string(LAYOUT_KEY, layout);
        }
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
