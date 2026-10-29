//! The Extensions pane: every extension present, enabled or not, and how they depend on each other.
//!
//! Its overview is a graph: a node an extension, colored by what became of it (loaded, left off by the project, refused)
//! and marked by where it comes from; an edge a requirement, from the extension needing it down to the one it needs,
//! labeled with the version wanted and the one found, red where it is not met; a requirement nothing provides is a
//! dashed node of its own. Extensions are laid out in rows by depth, those depending on others above them. Selecting
//! one lights what it needs and what needs it, and shows it beside the graph: who it is, where from, its size, how long
//! it took to load and the memory it holds, what it requires and what requires it, what it registers, and why it is
//! not loaded where it is not. A filterable list below the graph names them all.
//!
//! An extension is turned off or on for the project (`[extensions] disabled` in `project.toml`, a step of the
//! history), saying first what goes with it: those requiring it, turned off with it, or those it requires, turned on
//! with it. The editor then opens the project again with its extensions as they now are.

use eframe::egui::{self, Color32, RichText};
use project::ext::{Origin, Present, Status};
use std::collections::{BTreeSet, HashMap, HashSet};

use crate::workspace::Workspace;
use crate::{icons, theme};

/// What the pane keeps between frames.
#[derive(Clone, Default)]
struct State {
    selected: Option<String>,
    filter: String,
    /// A change asked for, waiting for its confirmation: the extensions to turn off or on, the one asked first.
    asked: Option<(bool, String, Vec<String>)>,
}

const NODE: egui::Vec2 = egui::vec2(170.0, 46.0);
const GAP: egui::Vec2 = egui::vec2(28.0, 54.0);

/// The pane.
pub fn extensions_ui(ui: &mut egui::Ui, ws: &mut Workspace, message: &mut Option<String>) {
    let id = egui::Id::unique("extensions pane");
    let mut state: State = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    let present = ws.extensions.present.clone();
    if state.selected.as_ref().is_none_or(|s| !present.iter().any(|p| p.id == *s)) {
        state.selected = present.first().map(|p| p.id.clone());
    }
    egui::Panel::right("extension details").resizable(true).default_size(320.0).show(ui, |ui| {
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            if let Some(selected) = state.selected.clone() {
                details_ui(ui, ws, &present, &selected, &mut state, message);
            }
        });
    });
    egui::CentralPanel::no_frame().show(ui, |ui| {
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            graph_ui(ui, &present, &mut state);
            ui.add_space(10.0);
            list_ui(ui, &present, &mut state);
        });
    });
    ui.data_mut(|d| d.insert_temp(id, state));
}

/// An extension's id from a requirement (`pe2.images@^0.1`), and the version it wants.
fn requirement(text: &str) -> (&str, Option<&str>) {
    match text.split_once('@') {
        Some((id, version)) => (id, Some(version)),
        None => (text, None),
    }
}

fn status_color(ui: &egui::Ui, status: Option<&Status>) -> Color32 {
    match status {
        Some(Status::Loaded) => theme::accent(),
        Some(Status::Disabled) => theme::weak(),
        Some(Status::Refused(_)) => ui.visuals().error_fg_color,
        None => ui.visuals().warn_fg_color,
    }
}

fn status_text(status: &Status) -> &'static str {
    match status {
        Status::Loaded => "loaded",
        Status::Disabled => "left off by the project",
        Status::Refused(_) => "not loaded",
    }
}

fn origin_text(origin: &Origin) -> String {
    match origin {
        Origin::Builtin => "built in".into(),
        Origin::Linked => "linked in".into(),
        Origin::Component(path) => path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned()),
    }
}

/// What an extension needs, and what needs it, both ways transitively.
fn chains(present: &[Present], id: &str) -> (HashSet<String>, HashSet<String>) {
    let needs_of = |id: &str| -> Vec<String> {
        present.iter().find(|p| p.id == id).map_or_default(|p| p.requires.iter().map(|r| requirement(r).0.to_string()).collect())
    };
    let mut needs = HashSet::new();
    let mut stack = needs_of(id);
    while let Some(next) = stack.pop() {
        if needs.insert(next.clone()) {
            stack.extend(needs_of(&next));
        }
    }
    let mut needed = HashSet::new();
    let mut stack = vec![id.to_string()];
    while let Some(next) = stack.pop() {
        for p in present.iter().filter(|p| p.requires.iter().any(|r| requirement(r).0 == next)) {
            if needed.insert(p.id.clone()) {
                stack.push(p.id.clone());
            }
        }
    }
    (needs, needed)
}

/// An extension's depth: 0 needing nothing present, else one more than the deepest it needs (a cycle's on one row).
fn depth_of(id: &str, present: &[Present], depth: &mut HashMap<String, usize>, seen: &mut HashSet<String>) -> usize {
    if let Some(d) = depth.get(id) {
        return *d;
    }
    if !seen.insert(id.to_string()) {
        return 0; // a cycle: its members on one row
    }
    let needs = present.iter().find(|p| p.id == id).map_or_default(|p| p.requires.clone());
    let d = needs.iter().map(|r| depth_of(requirement(r).0, present, depth, seen) + 1).max().unwrap_or(0);
    depth.insert(id.to_string(), d);
    d
}

/// The graph: extensions in rows by depth (those depending on others above them), requirements as edges.
fn graph_ui(ui: &mut egui::Ui, present: &[Present], state: &mut State) {
    let mut nodes: Vec<String> = present.iter().map(|p| p.id.clone()).collect();
    let missing: BTreeSet<String> = present
        .iter()
        .flat_map(|p| p.requires.iter().map(|r| requirement(r).0.to_string()))
        .filter(|id| !nodes.contains(id))
        .collect();
    nodes.extend(missing.iter().cloned());
    let mut depth: HashMap<String, usize> = HashMap::new();
    for id in &nodes {
        depth_of(id, present, &mut depth, &mut HashSet::new());
    }
    let rows = depth.values().copied().max().unwrap_or(0) + 1;
    let mut by_row: Vec<Vec<&String>> = vec![vec![]; rows];
    for id in &nodes {
        by_row[depth[id]].push(id);
    }
    // Within a row, those most needed first: when the row wraps, they stay closest to what needs them.
    let needed_by = |id: &str| present.iter().filter(|p| p.requires.iter().any(|r| requirement(r).0 == id)).count();
    for row in &mut by_row {
        row.sort_by(|a, b| needed_by(b).cmp(&needed_by(a)).then_with(|| a.cmp(b)));
    }
    // A row wider than the pane wraps onto more lines of its own; dependents' rows above.
    let width = ui.available_width().max(NODE.x);
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a pane's width in nodes")]
    let fit = (((width + GAP.x) / (NODE.x + GAP.x)).floor() as usize).max(1);
    let lines: Vec<Vec<&String>> =
        by_row.iter().rev().flat_map(|ids| ids.chunks(fit).map(<[&String]>::to_vec).collect::<Vec<_>>()).collect();
    #[expect(clippy::cast_precision_loss, reason = "a handful of extensions")]
    let size = egui::vec2(width, lines.len() as f32 * (NODE.y + GAP.y) + 8.0);
    let (area, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let mut places: HashMap<&String, egui::Rect> = HashMap::new();
    for (line, ids) in lines.iter().enumerate() {
        #[expect(clippy::cast_precision_loss, reason = "a handful of extensions")]
        let y = area.top() + line as f32 * (NODE.y + GAP.y) + 4.0;
        #[expect(clippy::cast_precision_loss, reason = "a handful of extensions")]
        let span = ids.len() as f32 * (NODE.x + GAP.x) - GAP.x;
        let mut x = area.center().x - span / 2.0;
        for id in ids {
            places.insert(id, egui::Rect::from_min_size(egui::pos2(x, y), NODE));
            x += NODE.x + GAP.x;
        }
    }
    let (needs, needed) = state.selected.as_deref().map_or_default(|s| chains(present, s));
    let chosen = state.selected.clone();
    let lit = |id: &str| chosen.as_deref() == Some(id) || needs.contains(id) || needed.contains(id);
    let painter = ui.painter_at(area);
    let font = egui::TextStyle::Small.resolve(ui.style());
    // Edges first, under the nodes.
    for p in present {
        for text in &p.requires {
            let (id, wanted) = requirement(text);
            let (Some(from), Some(to)) = (places.get(&p.id), places.get(&id.to_string())) else { continue };
            let found = present.iter().find(|q| q.id == id);
            // Met: what it needs is loaded, and it was not refused over it (a version too old).
            let met = found.is_some_and(|q| q.status == Status::Loaded) && !matches!(p.status, Status::Refused(_));
            let on = lit(&p.id) && lit(id);
            let color = if !met { ui.visuals().error_fg_color } else if on { theme::accent() } else { theme::border().gamma_multiply(2.0) };
            let (a, b) = (from.center_bottom(), to.center_top());
            painter.line_segment([a, b], egui::Stroke::new(if on { 2.0 } else { 1.0 }, color));
            let tip = b - (b - a).normalized() * 9.0;
            let across = (b - a).normalized().rot90() * 4.0;
            painter.add(egui::Shape::convex_polygon(vec![b, tip + across, tip - across], color, egui::Stroke::NONE));
            let label = format!("{}  ·  has {}", wanted.unwrap_or("any"), found.map_or("none", |q| q.version.as_str()));
            painter.text(a.lerp(b, 0.5) + egui::vec2(6.0, 0.0), egui::Align2::LEFT_CENTER, label, font.clone(), color);
        }
    }
    for (id, rect) in &places {
        let found = present.iter().find(|p| p.id == **id);
        let response = ui.interact(*rect, egui::Id::unique(("extension node", id.as_str())), egui::Sense::click());
        let selected = chosen.as_deref() == Some(id.as_str());
        let label = format!("{id}: {}", found.map_or("not installed", |p| status_text(&p.status)));
        response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, selected, &label));
        if response.clicked() && found.is_some() {
            state.selected = Some((*id).clone());
        }
        let color = status_color(ui, found.map(|p| &p.status));
        let dim = chosen.is_some() && !lit(id);
        let fill = color.gamma_multiply(if dim { 0.05 } else { 0.16 });
        let stroke = egui::Stroke::new(if selected { 2.0 } else { 1.0 }, color.gamma_multiply(if dim { 0.4 } else { 1.0 }));
        if found.is_some() {
            painter.rect(*rect, 6.0, fill, stroke, egui::StrokeKind::Inside);
        } else {
            // Nothing provides it: dashed.
            let r = *rect;
            let corners = [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()];
            for pair in corners.windows(2) {
                painter.extend(egui::Shape::dashed_line(&[pair[0], pair[1]], stroke, 5.0, 4.0));
            }
        }
        let text = if dim { theme::weak() } else { theme::text() };
        painter.text(rect.center_top() + egui::vec2(0.0, 13.0), egui::Align2::CENTER_CENTER, id.as_str(), egui::TextStyle::Body.resolve(ui.style()), text);
        let note = found.map_or_else(|| "not installed".to_string(), |p| format!("{}  ·  {}", p.version, origin_text(&p.origin)));
        painter.text(rect.center_bottom() - egui::vec2(0.0, 12.0), egui::Align2::CENTER_CENTER, note, font.clone(), theme::weak());
    }
}

/// Every extension by name, filtered.
fn list_ui(ui: &mut egui::Ui, present: &[Present], state: &mut State) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(icons::MAGNIFYING_GLASS).color(theme::weak()));
        ui.add(egui::TextEdit::singleline(&mut state.filter).hint_text("Filter extensions").desired_width(220.0));
    });
    let filter = state.filter.to_lowercase();
    // A column each - name, version, what became of it - so they line up across the rows.
    egui::Grid::new("extensions list").num_columns(3).spacing([16.0, 4.0]).show(ui, |ui| {
        for p in present.iter().filter(|p| p.id.to_lowercase().contains(&filter)) {
            let selected = state.selected.as_deref() == Some(p.id.as_str());
            if ui.selectable_label(selected, RichText::new(&p.id).strong()).clicked() {
                state.selected = Some(p.id.clone());
            }
            ui.label(RichText::new(&p.version).color(theme::weak()));
            ui.label(RichText::new(status_text(&p.status)).small().color(status_color(ui, Some(&p.status))));
            ui.end_row();
        }
    });
}

/// The extension selected: who it is, what it needs and registers, its switch.
fn details_ui(
    ui: &mut egui::Ui,
    ws: &mut Workspace,
    present: &[Present],
    selected: &str,
    state: &mut State,
    message: &mut Option<String>,
) {
    let Some(p) = present.iter().find(|p| p.id == selected) else { return };
    ui.label(RichText::new(&p.id).heading());
    ui.label(RichText::new(status_text(&p.status)).color(status_color(ui, Some(&p.status))));
    if let Status::Refused(why) = &p.status {
        ui.label(RichText::new(why).small().color(ui.visuals().error_fg_color));
    }
    ui.add_space(6.0);
    switch_ui(ui, ws, present, p, state, message);
    theme::section(ui, "About", |ui| {
        egui::Grid::new("extension about").num_columns(2).spacing([14.0, 4.0]).show(ui, |ui| {
            let mut row = |label: &str, value: String| {
                ui.label(RichText::new(label).color(theme::weak()));
                ui.label(value);
                ui.end_row();
            };
            row("Version", p.version.clone());
            row("From", origin_text(&p.origin));
            let contract = match p.origin {
                Origin::Linked => "linked in: none".to_string(),
                _ => "pe2:extension@0.1.0".to_string(),
            };
            row("Contract", contract);
            if let Some(size) = p.size {
                row("Size", format!("{} KB", size.div_ceil(1024)));
            }
            if let Some(load) = p.load {
                row("Loaded in", format!("{:.1} ms", load.as_secs_f64() * 1000.0));
            }
            if let Some((_, m)) = ws.extensions.memory().into_iter().find(|(id, _)| *id == p.id) {
                let mib = |bytes: usize| bytes.div_ceil(1 << 20);
                row("Memory", format!("{} MiB in {} instance{}, at most {} MiB", mib(m.bytes), m.instances, if m.instances == 1 { "" } else { "s" }, mib(m.peak)));
            }
        });
    });
    theme::section(ui, "Requires", |ui| {
        if p.requires.is_empty() {
            ui.label(RichText::new("Nothing").color(theme::weak()));
        }
        for text in &p.requires {
            let (id, wanted) = requirement(text);
            let found = present.iter().find(|q| q.id == id);
            ui.horizontal_wrapped(|ui| {
                if ui.link(id).clicked() && found.is_some() {
                    state.selected = Some(id.to_string());
                }
                let have = found.map_or_else(|| "not installed".to_string(), |q| format!("{} ({})", q.version, status_text(&q.status)));
                ui.label(RichText::new(format!("wants {}, has {have}", wanted.unwrap_or("any"))).small().color(theme::weak()));
            });
        }
    });
    theme::section(ui, "Required by", |ui| {
        let by: Vec<&Present> = present.iter().filter(|q| q.requires.iter().any(|r| requirement(r).0 == p.id)).collect();
        if by.is_empty() {
            ui.label(RichText::new("Nothing").color(theme::weak()));
        }
        for q in by {
            if ui.link(&q.id).clicked() {
                state.selected = Some(q.id.clone());
            }
        }
    });
    theme::section(ui, "Registers", |ui| {
        if p.registers.iter().all(|(_, names)| names.is_empty()) {
            let note = if p.status == Status::Loaded { "Nothing" } else { "Nothing: it is not loaded" };
            ui.label(RichText::new(note).color(theme::weak()));
        }
        for (what, names) in p.registers.iter().filter(|(_, names)| !names.is_empty()) {
            egui::CollapsingHeader::new(format!("{what}  ·  {}", names.len())).id_salt(("registers", &p.id, *what)).show(ui, |ui| {
                for name in names {
                    ui.label(RichText::new(name.split_once('@').map_or(name.as_str(), |(_, local)| local)).monospace());
                }
            });
        }
    });
}

/// The switch: on or off for the project, saying first what goes with it.
fn switch_ui(ui: &mut egui::Ui, ws: &mut Workspace, present: &[Present], p: &Present, state: &mut State, message: &mut Option<String>) {
    let disabled: BTreeSet<String> = ws.project.disabled_extensions.iter().cloned().collect();
    let off = disabled.contains(&p.id);
    if !off && matches!(p.status, Status::Refused(_)) {
        ui.label(RichText::new("It cannot be turned on until what it needs is.").small().color(theme::weak()));
        return;
    }
    if let Some((turn_on, asked, with)) = state.asked.clone().filter(|(_, asked, _)| *asked == p.id) {
        let verb = if turn_on { "on" } else { "off" };
        let others = if with.is_empty() { String::new() } else { format!(", and with it {}", with.join(", ")) };
        ui.label(format!("Turn {asked} {verb} for this project{others}?"));
        ui.horizontal(|ui| {
            if ui.button(format!("Turn {verb}")).clicked() {
                let mut next = disabled.clone();
                if turn_on {
                    next.remove(&asked);
                    for id in &with {
                        next.remove(id);
                    }
                } else {
                    next.insert(asked.clone());
                }
                let next: Vec<String> = next.into_iter().collect();
                let label = format!("Turn {asked} {verb}");
                let written = project::switches::set_disabled(&ws.project, &next, &mut ws.history, label);
                *message = Some(match written {
                    Ok(()) => format!("{asked} turned {verb}: the project opens again with its extensions"),
                    Err(e) => format!("project.toml: {e:#}"),
                });
                state.asked = None;
            }
            if ui.button("Cancel").clicked() {
                state.asked = None;
            }
        });
        return;
    }
    let mut on = !off;
    if ui.checkbox(&mut on, "Enabled for this project").changed() {
        let (needs, needed) = chains(present, &p.id);
        // Off: those requiring it go with it. On: those it requires and the project left off come with it.
        let with: Vec<String> = if on {
            needs.into_iter().filter(|id| disabled.contains(id)).collect()
        } else {
            needed.into_iter().filter(|id| present.iter().any(|q| q.id == *id && q.status == Status::Loaded)).collect()
        };
        let mut with = with;
        with.sort();
        state.asked = Some((on, p.id.clone(), with));
    }
}
