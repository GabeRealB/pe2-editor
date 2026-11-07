//! Filling a project's generated layers, the step of its onboarding after what it asks to trust: where a layer is empty,
//! or not as its plan says, whoever opened the project is shown what its plan makes the layer's files from - each disc,
//! found or not, and where they are looked for, which they may change (`project.local.toml`) - and asked to run it, its
//! progress shown, stopped where they like (what was written stays, and the next run goes on from there). Not now, it is
//! asked again the next time the project is opened; the command asks it whatever the layers' states.
//!
//! A plan runs as the work graph's only work (`runner`): after the epoch running, which may be cancelled for it, and
//! before any event signaled meanwhile, which waits until what it wrote is taken in.

use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::Instant;

use eframe::egui::{self, RichText};
use project::plan::{Progress, Ran, State};

use crate::theme;
use crate::workspace::Workspace;

/// A plan running.
struct Running {
    layer: String,
    path: PathBuf,
    progress: Arc<Progress>,
    /// Set once it runs, the epoch before it ended.
    started: Arc<std::sync::atomic::AtomicBool>,
    done: mpsc::Receiver<anyhow::Result<Ran>>,
    began: Instant,
}

#[derive(Default)]
pub struct Fill {
    /// Not now: asked again when the project is opened again.
    put_off: bool,
    /// Asked for (the command), whatever the layers' states.
    asked: bool,
    running: Option<Running>,
    /// Why the last run did not finish, shown until the next.
    failed: Option<String>,
    /// Where the discs are looked for, as being typed: by the mount it locates.
    look_in: Option<(String, String)>,
    /// What a run wrote, being taken in: the take-in's ticket.
    settling: Option<u64>,
}

impl Fill {
    /// Asked for: shown whatever the layers' states.
    pub fn ask(&mut self) {
        self.asked = true;
        self.put_off = false;
    }

    pub fn running(&self) -> bool {
        self.running.is_some()
    }

    /// Whether events wait: a plan is to run or running, or what it wrote is not taken in yet (`covers`, by ticket).
    pub fn holds_events(&mut self, covers: impl Fn(u64) -> bool) -> bool {
        if self.settling.is_some_and(covers) {
            self.settling = None;
        }
        self.running.is_some() || self.settling.is_some()
    }
}

/// The generated layer to ask about: the one running, else the first not as its plan says, else - asked for - the first.
fn wanted(ws: &Workspace) -> Option<String> {
    let fill = &ws.fill;
    if let Some(running) = &fill.running {
        return Some(running.layer.clone());
    }
    let to_fill = |state: Option<&Result<State, String>>| matches!(state, Some(Ok(State::Empty | State::Stale(_))));
    let unfilled = ws.plans.layers.iter().find(|(_, p)| to_fill(p.state.as_ref())).map(|(name, _)| name.clone());
    match unfilled {
        Some(name) if !fill.put_off || fill.asked => Some(name),
        _ if fill.asked => ws.plans.layers.keys().next().cloned(),
        _ => None,
    }
}

/// Run a generated layer's plan, off the window's thread.
/// Have a generated layer's plan run, as the work graph's only work.
fn start(ws: &mut Workspace, layer: &str) {
    let Ok(generated) = ws.project.layer(layer).cloned() else { return };
    let progress = Arc::new(Progress::default());
    let started = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (sender, done) = mpsc::channel();
    let path = generated.path.clone();
    ws.extract(crate::runner::Extraction {
        layer: generated,
        progress: progress.clone(),
        started: started.clone(),
        done: sender,
    });
    ws.fill.failed = None;
    let began = Instant::now();
    ws.fill.running = Some(Running { layer: layer.to_string(), path, progress, started, done, began });
}

/// Take what a run made, once it is done: the layer taken in again, its state worked out again. What it did, said.
fn finish(ws: &mut Workspace) -> Option<String> {
    let running = ws.fill.running.as_ref()?;
    let ran = running.done.try_recv().ok()?;
    let running = ws.fill.running.take()?;
    ws.fill.settling = Some(ws.take_in(vec![running.path.clone()]));
    ws.plans.again();
    let took = running.began.elapsed().as_secs_f64();
    match ran {
        Ok(ran) => {
            ws.fill.asked = false;
            Some(format!(
                "{}: {} files written, {} kept as they were, {} removed, in {took:.1} s",
                running.layer, ran.written, ran.kept, ran.removed
            ))
        }
        Err(why) => {
            let stopped = running.progress.stop.load(std::sync::atomic::Ordering::Relaxed);
            let said = if stopped { "Stopped: what was written stays".to_string() } else { format!("{why:#}") };
            let message =
                format!("{} stopped: {}", running.layer, if stopped { "what was written stays" } else { &said });
            ws.fill.failed = Some(said);
            Some(message)
        }
    }
}

/// The step, where there is one to show; what a run did, once it is done.
pub fn ui(ctx: &egui::Context, ws: &mut Workspace) -> Option<String> {
    let said = finish(ws);
    let layer = wanted(ws)?;
    ws.plans.want_discs();
    let Some(planned) = ws.plans.layers.get(&layer).cloned() else { return said };
    let name = ws.project.root.file_name().map_or_else(|| "This project".into(), |n| n.to_string_lossy().into_owned());
    let mut close = false;
    let mut extract = false;
    let mut look_here = None;
    egui::Modal::new(egui::Id::unique("fill")).show(ctx, |ui| {
        ui.set_width(560.0);
        ui.heading(name);
        ui.add_space(8.0);
        let state = match &planned.state {
            Some(Ok(State::Empty)) => "is empty".to_string(),
            Some(Ok(State::Stale(why))) => format!("is out of date ({})", why.join("; ")),
            Some(Ok(State::Filled)) => "is filled, as its plan says".to_string(),
            Some(Ok(State::NoPlan)) => "has no plan".to_string(),
            Some(Err(why)) => format!("has a plan that cannot be read: {why}"),
            None => "is being looked at".to_string(),
        };
        let what = format!(
            "The layer {layer} {state}. It holds the files of the nodes of {}, which its plan makes from these:",
            planned.fills
        );
        ui.add(egui::Label::new(what).wrap());
        ui.add_space(4.0);
        let all_found = discs_ui(ui, &planned);
        // Where they are stays as it is while they are read.
        let running = ws.fill.running.is_some();
        ui.add_enabled_ui(!running, |ui| look_here = look_in_ui(ui, ws, &planned));
        if let Some(why) = &ws.fill.failed {
            ui.add_space(6.0);
            theme::warning(ui, why);
        }
        ui.add_space(10.0);
        if !running_ui(ui, ctx, ws) {
            ui.horizontal(|ui| {
                let hint = if all_found {
                    "Run its plan: only what is not there yet is made"
                } else {
                    "Not every disc is found"
                };
                extract = ui.add_enabled(all_found, egui::Button::new("Extract")).on_hover_text(hint).clicked();
                close = ui.button("Not now").on_hover_text("Asked again next time").clicked();
            });
        }
    });
    if let Some((at, path)) = look_here {
        match project::setup::locate(&ws.project, &at, &path) {
            Ok(()) => {
                ws.fill.look_in = None;
                ws.reload_project("where the discs are changed");
            }
            Err(why) => ws.fill.failed = Some(format!("{why:#}")),
        }
    }
    if extract {
        start(ws, &layer);
    } else if close {
        ws.fill.put_off = true;
        ws.fill.asked = false;
    }
    said
}

/// A plan to run or running: waiting for the epoch before it, or how far it is, and what stops it. Whether there is one.
fn running_ui(ui: &mut egui::Ui, ctx: &egui::Context, ws: &mut Workspace) -> bool {
    let waiting = ws.fill.running.as_ref().is_some_and(|r| !r.started.load(std::sync::atomic::Ordering::Relaxed));
    if waiting {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(RichText::new("Waiting for the epoch running to end…").color(theme::weak()));
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button("Cancel the epoch").on_hover_text("It stops at its next step; the plan runs then").clicked() {
                ws.cancel_epoch();
            }
            if ui.button("Stop").on_hover_text("The plan does not run").clicked()
                && let Some(running) = &ws.fill.running
            {
                running.progress.stop.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        });
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    } else if let Some(running) = &ws.fill.running {
        let (ran, operations, written) = running.progress.now();
        #[expect(clippy::cast_precision_loss, reason = "a fraction shown")]
        let fraction = if operations == 0 { 0.0 } else { ran as f32 / operations as f32 };
        if operations == 0 {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Reading its plan…").color(theme::weak()));
            });
        } else {
            let text = format!("{ran} of {operations} operations  ·  {written} files written");
            ui.add(egui::ProgressBar::new(fraction).text(text));
        }
        ui.add_space(6.0);
        if ui.button("Stop").on_hover_text("What was written stays: the next run goes on from there").clicked() {
            running.progress.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    } else {
        return false;
    }
    true
}

/// Each release's discs, found or not, a row each. Whether every one was found.
fn discs_ui(ui: &mut egui::Ui, planned: &crate::plans::Planned) -> bool {
    let Some(discs) = &planned.discs else {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(RichText::new("Looking for the discs…").color(theme::weak()));
        });
        return false;
    };
    let mut releases: Vec<&str> = discs.iter().map(|d| d.release.as_str()).collect();
    releases.dedup();
    for release in releases {
        theme::section(ui, release, |ui| {
            egui::Grid::new(("discs", release)).num_columns(3).spacing([16.0, 4.0]).show(ui, |ui| {
                for disc in discs.iter().filter(|d| d.release == release) {
                    ui.monospace(&disc.name);
                    ui.label(RichText::new(crate::inspector::size_text(disc.size)).color(theme::weak()));
                    if let Some(path) = &disc.found {
                        let found =
                            RichText::new(format!("{}  found", crate::icons::CHECK_CIRCLE)).color(theme::accent());
                        ui.label(found).on_hover_text(path.display().to_string());
                    } else {
                        let missing = RichText::new(format!("{}  not found", crate::icons::X_CIRCLE));
                        ui.label(missing.color(ui.visuals().warn_fg_color))
                            .on_hover_text("Looked for by its size and sha1, whatever its name");
                    }
                    ui.end_row();
                }
            });
        });
    }
    discs.iter().all(|d| d.found.is_some())
}

/// Where the discs are looked for, where that is an unmanaged mount: its place on this machine, to change. The mount and
/// the place typed, once asked to look there.
fn look_in_ui(ui: &mut egui::Ui, ws: &mut Workspace, planned: &crate::plans::Planned) -> Option<(String, String)> {
    let header = planned.header.as_ref().ok()?;
    let look_in = header.inputs.values().next()?.look_in.trim_start_matches("res://").trim_matches('/');
    let mount = ws.project.unmanaged.iter().find(|m| m.at == look_in)?;
    let (at, now) = (mount.at.clone(), mount.path.display().to_string());
    let editing = ws.fill.look_in.get_or_insert_with(|| (at.clone(), now.clone()));
    let mut asked = None;
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label("Looked for in");
        let field = ui.add(egui::TextEdit::singleline(&mut editing.1).desired_width(340.0));
        let typed = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let changed = editing.1.trim() != now;
        let clicked = ui
            .add_enabled(changed, egui::Button::new("Look here"))
            .on_hover_text("Kept for this computer alone (project.local.toml)")
            .clicked();
        if changed && (typed || clicked) {
            asked = Some((at, editing.1.trim().to_string()));
        }
    });
    asked
}
