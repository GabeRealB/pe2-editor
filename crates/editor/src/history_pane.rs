//! The History pane: the session's log as a timeline - every step, oldest first, with when and by whom, the saves and
//! the checkpoints among them, and what was undone greyed after them; of every document, or only the one selected, and
//! of one author. A step chosen shows what it changed - a declaration field by field - and the point after it is shown
//! everywhere, read-only: previews, views and players show the documents as they were then (`Workspace::view`). From
//! there the documents are restored to it, as a new step, or flipped with now (A/B) to compare. A step's edits alone are
//! taken back where nothing since edited what it did.

use eframe::egui::{self, RichText};
use project::history::{Kind, Step};

use crate::workspace::{Kept, Viewing, Workspace};
use crate::{icons, theme};

/// What the History pane asks.
#[derive(Clone, Debug)]
pub enum HistoryAsk {
    /// Show a point read-only; none: now.
    View(Option<Viewing>),
    /// Put the documents back as at the point shown, as a new step.
    Restore,
    /// Take back one step's edits, as a new step: its place among the steps done.
    Revert(usize),
    /// Undo (false) or redo so many steps.
    Steps(bool, usize),
    /// Name the point the history is at.
    Checkpoint(String),
}

/// What the pane keeps between frames: whose steps it lists, and a checkpoint being named.
#[derive(Clone, Default)]
struct State {
    this_document: bool,
    author: Option<String>,
    naming: Option<String>,
}

pub fn history_ui(ui: &mut egui::Ui, ws: &Workspace, ask: &mut Option<HistoryAsk>) {
    let id = ui.scope_id().with("history pane");
    let mut state: State = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    let document = ws.selected.as_deref().and_then(|p| ws.index.assets.get(p)).and_then(|a| a.uid.clone());
    let (done, undone) = (ws.history.done(), ws.history.undone());
    bar(ui, &mut state, document.is_some(), done.iter().chain(undone), ask);
    if let Some(viewing) = &ws.viewing {
        banner(ui, ws, viewing, ask);
    }
    ui.add_space(4.0);
    if done.is_empty() && undone.is_empty() {
        ui.label(
            RichText::new("Nothing done yet. What is done is kept until the project is closed.").color(theme::weak()),
        );
        ui.data_mut(|d| d.insert_temp(id, state));
        return;
    }
    let scope = document.filter(|_| state.this_document);
    let shown = |step: &Step| {
        scope.as_deref().is_none_or(|uid| step.edits_document(uid))
            && state.author.as_deref().is_none_or(|author| step.author == author)
    };
    // The point shown: after so many steps; now, the last.
    let at = ws.viewing.as_ref().filter(|v| !v.now).map_or(done.len(), |v| v.point);
    egui::ScrollArea::vertical().id_salt("history").auto_shrink(false).stick_to_bottom(true).show(ui, |ui| {
        for (i, step) in done.iter().enumerate().filter(|(_, step)| shown(step)) {
            let current = i + 1 == at;
            let response = row(ui, step, current, false);
            if response.clicked() {
                let point = i + 1;
                let now = point == done.len() && scope.is_none();
                let viewing = (!now).then(|| Viewing { point, scope: scope.clone(), now: false });
                *ask = Some(HistoryAsk::View(viewing));
            }
            response.context_menu(|ui| {
                if ui.button("Show as after this").clicked() {
                    *ask = Some(HistoryAsk::View(Some(Viewing { point: i + 1, scope: scope.clone(), now: false })));
                    ui.close();
                }
                let undoes = done[i + 1..].iter().filter(|s| s.undoable()).count();
                if ui.add_enabled(undoes > 0, egui::Button::new("Undo back to here")).clicked() {
                    *ask = Some(HistoryAsk::Steps(false, undoes));
                    ui.close();
                }
                let reverts = step.undoable() && step.changes.is_empty() && !step.edits.is_empty();
                if ui.add_enabled(reverts, egui::Button::new("Take this change back")).clicked() {
                    *ask = Some(HistoryAsk::Revert(i));
                    ui.close();
                }
            });
            if current {
                changes_ui(ui, ws, step);
            }
        }
        for (i, step) in undone.iter().rev().enumerate().filter(|(_, step)| shown(step)) {
            if row(ui, step, false, true).on_hover_text("Undone: click to redo up to it").clicked() {
                *ask = Some(HistoryAsk::Steps(true, i + 1));
            }
        }
    });
    // A/B: F flips between the point shown and now, while the pointer is over the pane.
    if let Some(viewing) = &ws.viewing
        && ui.ui_contains_pointer()
        && ui.input(|i| i.key_pressed(egui::Key::F))
    {
        *ask = Some(HistoryAsk::View(Some(Viewing { now: !viewing.now, ..viewing.clone() })));
    }
    ui.data_mut(|d| d.insert_temp(id, state));
}

/// Whose steps are listed - every document's or the selected one's, every author's or one's - and a checkpoint named.
fn bar<'s>(
    ui: &mut egui::Ui,
    state: &mut State,
    document: bool,
    steps: impl Iterator<Item = &'s Step>,
    ask: &mut Option<HistoryAsk>,
) {
    let mut authors: Vec<&str> = steps.map(|s| s.author.as_str()).collect();
    authors.sort_unstable();
    authors.dedup();
    ui.horizontal(|ui| {
        ui.selectable_value(&mut state.this_document, false, "All");
        ui.add_enabled_ui(document, |ui| {
            ui.selectable_value(&mut state.this_document, true, "This document")
                .on_hover_text("The steps that edited the asset selected");
        });
        if authors.len() > 1 || state.author.is_some() {
            egui::ComboBox::from_id_salt("history author")
                .selected_text(state.author.clone().unwrap_or_else(|| "Anyone".into()))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut state.author, None, "Anyone");
                    for author in &authors {
                        ui.selectable_value(&mut state.author, Some((*author).to_string()), *author);
                    }
                });
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button(format!("{}  Checkpoint", icons::FLAG)).on_hover_text("Name this point").clicked() {
                state.naming = Some(String::new());
            }
        });
    });
    if let Some(name) = &mut state.naming {
        let response = ui.add(egui::TextEdit::singleline(name).hint_text("Name the checkpoint, then Enter"));
        response.request_focus();
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            state.naming = None;
        } else if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            let name = name.trim().to_string();
            if !name.is_empty() {
                *ask = Some(HistoryAsk::Checkpoint(name));
            }
            state.naming = None;
        }
    }
}

/// A step: when, what - a save and a checkpoint marked - and who, where it was not the user.
fn row(ui: &mut egui::Ui, step: &Step, current: bool, undone: bool) -> egui::Response {
    ui.horizontal(|ui| {
        ui.add_sized(
            [56.0, ui.spacing().interact_size.y],
            egui::Label::new(RichText::new(ago(step.at)).small().color(theme::weak())),
        );
        let mark = match step.kind {
            Kind::Save => format!("{}  ", icons::FLOPPY_DISK),
            Kind::Checkpoint => format!("{}  ", icons::FLAG),
            Kind::Edit => String::new(),
        };
        let mut text = RichText::new(format!("{mark}{}", step.label));
        if undone {
            text = text.color(theme::weak()).italics();
        } else if step.kind == Kind::Checkpoint {
            text = text.strong();
        }
        let response = ui.add(egui::Button::selectable(current, text).truncate());
        if step.author != "user" {
            ui.label(RichText::new(&step.author).small().color(theme::weak()));
        }
        response
    })
    .inner
}

/// The point shown, read-only: what it is, and what can be done from it.
fn banner(ui: &mut egui::Ui, ws: &Workspace, viewing: &Viewing, ask: &mut Option<HistoryAsk>) {
    let warn = ui.visuals().warn_fg_color;
    let after = viewing.point.checked_sub(1).and_then(|i| ws.history.done().get(i));
    let what =
        after.map_or_else(|| "as at the start of the session".to_string(), |s| format!("as after \"{}\"", s.label));
    let of = viewing
        .scope
        .as_deref()
        .and_then(|uid| ws.index.by_uid.get(uid))
        .map_or_else(|| "The documents".to_string(), |path| path.rsplit('/').next().unwrap_or(path).to_string());
    egui::Frame::new()
        .stroke(egui::Stroke::new(1.0, warn.gamma_multiply(0.6)))
        .corner_radius(4.0)
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let shown = if viewing.now { "Now is shown".to_string() } else { format!("{of} {what}") };
            ui.label(RichText::new(format!("{}  {shown}", icons::CLOCK_COUNTER_CLOCKWISE)).color(warn));
            ui.label(
                RichText::new("Read-only: everything shows it. Moves and new assets stay as they are now.")
                    .small()
                    .color(theme::weak()),
            );
            ui.horizontal(|ui| {
                if ui.button("Restore here").on_hover_text("Put the documents back as they were: a new step").clicked()
                {
                    *ask = Some(HistoryAsk::Restore);
                }
                let flip = if viewing.now { "Show then" } else { "Show now" };
                if ui.button(format!("{}  {flip}", icons::SWAP)).on_hover_text("Compare: F flips too").clicked() {
                    *ask = Some(HistoryAsk::View(Some(Viewing { now: !viewing.now, ..viewing.clone() })));
                }
                if ui.button("Back to now").clicked() {
                    *ask = Some(HistoryAsk::View(None));
                }
            });
        });
}

/// What a step changed: each document's files - a declaration field by field - and the project's files.
fn changes_ui(ui: &mut egui::Ui, ws: &Workspace, step: &Step) {
    let indent = 64.0;
    ui.horizontal(|ui| {
        ui.add_space(indent);
        ui.vertical(|ui| {
            for edit in &step.edits {
                let name = edit.path.rsplit('/').next().unwrap_or(&edit.path);
                ui.label(RichText::new(format!("{name} · {}", edit.file)).small().strong());
                // Each content read once, and kept: what a step changed is drawn every frame it shows.
                let kept = |sha1: &Option<String>| sha1.as_deref().map(|s| ws.kept(s));
                match (kept(&edit.before), kept(&edit.after)) {
                    (Some(Kept::Text(before)), Some(Kept::Text(after)))
                        if std::path::Path::new(&edit.file)
                            .extension()
                            .is_some_and(|e| e.eq_ignore_ascii_case("toml")) =>
                    {
                        let lines = field_changes(&before, &after);
                        for line in lines.iter().take(12) {
                            ui.label(RichText::new(line).small().monospace().color(theme::weak()));
                        }
                        if lines.len() > 12 {
                            ui.label(
                                RichText::new(format!("and {} more", lines.len() - 12)).small().color(theme::weak()),
                            );
                        }
                    }
                    (before, after) => {
                        let size = |kept: Option<Kept>| match kept {
                            None => "absent".to_string(),
                            Some(Kept::Text(text)) => format!("{} bytes", text.len()),
                            Some(Kept::Bytes(long)) => format!("{long} bytes"),
                            Some(Kept::Gone) => "not kept".to_string(),
                        };
                        let (before, after) = (size(before), size(after));
                        ui.label(RichText::new(format!("{before} → {after}")).small().color(theme::weak()));
                    }
                }
            }
            let paths = step.paths();
            if !paths.is_empty() {
                let names: Vec<String> = paths
                    .iter()
                    .take(6)
                    .map(|p| p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into()))
                    .collect();
                let more = if paths.len() > 6 { format!(" and {} more", paths.len() - 6) } else { String::new() };
                ui.label(RichText::new(format!("Files: {}{more}", names.join(", "))).small().color(theme::weak()));
            }
        });
    });
}

/// What changed between two declarations, field by field: `key: before → after`, a field added or removed said so.
fn field_changes(before: &str, after: &str) -> Vec<String> {
    let parse = |text: &str| text.parse::<toml::Table>().ok();
    match (parse(before), parse(after)) {
        (Some(before), Some(after)) => {
            let mut lines = vec![];
            diff(&toml::Value::Table(before), &toml::Value::Table(after), "", &mut lines);
            lines
        }
        _ => vec!["(not readable as fields)".into()],
    }
}

fn diff(before: &toml::Value, after: &toml::Value, at: &str, lines: &mut Vec<String>) {
    let key = |name: &str| if at.is_empty() { name.to_string() } else { format!("{at}.{name}") };
    match (before, after) {
        (toml::Value::Table(b), toml::Value::Table(a)) => {
            for (name, value) in b {
                match a.get(name) {
                    Some(other) => diff(value, other, &key(name), lines),
                    None => lines.push(format!("{}: removed", key(name))),
                }
            }
            for (name, value) in a.iter().filter(|(name, _)| !b.contains_key(*name)) {
                lines.push(format!("{}: added {}", key(name), short(value)));
            }
        }
        (toml::Value::Array(b), toml::Value::Array(a)) if b.len() == a.len() => {
            for (i, (b, a)) in b.iter().zip(a).enumerate() {
                diff(b, a, &format!("{at}[{i}]"), lines);
            }
        }
        (b, a) if b != a => lines.push(format!("{at}: {} → {}", short(b), short(a))),
        _ => {}
    }
}

/// A value, as one short line.
fn short(value: &toml::Value) -> String {
    let text = value.to_string();
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() > 40 { format!("{}…", text.chars().take(39).collect::<String>()) } else { text }
}

/// How long ago a time was (milliseconds since the epoch), briefly.
fn ago(at: u64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    let seconds = u64::try_from(now.saturating_sub(u128::from(at)) / 1000).unwrap_or(u64::MAX);
    match seconds {
        0..10 => "now".into(),
        10..60 => format!("{seconds} s"),
        60..3600 => format!("{} min", seconds / 60),
        _ => format!("{} h", seconds / 3600),
    }
}

#[cfg(test)]
mod tests {
    use super::field_changes;

    #[test]
    fn tells_what_changed_field_by_field() {
        let before = "a = 1\n[t]\nx = [1, 2]\ngone = true\n";
        let after = "a = 2\n[t]\nx = [1, 3]\nnew = \"n\"\n";
        // In the order the declaration has its fields.
        assert_eq!(
            field_changes(before, after),
            ["a: 1 → 2", "t.x[1]: 2 → 3", "t.gone: removed", "t.new: added \"n\""]
        );
    }
}
