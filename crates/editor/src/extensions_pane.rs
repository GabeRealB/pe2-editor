//! The Extensions pane: every extension present, enabled or not, and how they depend on each other.
//!
//! Its overview is a graph: a node an extension, colored by what became of it (loaded, left off by the project, refused)
//! and marked by where it comes from; an edge a dependency, from the extension having it down to the one it names,
//! labeled with the version wanted and the one found, red where a requirement is not met, dashed where it is weak and
//! dotted where optional; a requirement nothing provides is a dashed node of its own, and any other dependency, none. Extensions are laid out in rows by depth, those depending on others above them. Selecting
//! one lights what it needs and what needs it, and shows it beside the graph: who it is, where from, its size, how long
//! it took to load and the memory it holds, what it requires and what requires it, what it registers, and why it is
//! not loaded where it is not. A filterable list below the graph names them all.
//!
//! An extension is turned off or on for the project (`[extensions] disabled` in `project.toml`, a step of the
//! history), saying first what goes with it: those requiring it and those weakly linked to it, turned off with it, or
//! those it requires, turned on with it. The editor then opens the project again with its extensions as they now
//! are. One that runs outside the sandbox is enabled first, by the user and for every project; until then it does not
//! run.

use eframe::egui::{self, Color32, RichText};
use pe2_ext::{Scope, Setting};
use project::ext::{Dependency, Origin, Present, Status};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::workspace::Workspace;
use crate::{icons, theme};

/// What the pane keeps between frames.
#[derive(Clone, Default)]
struct State {
    selected: Option<String>,
    filter: String,
    /// A change asked for, waiting for its confirmation: the extensions to turn off or on, the one asked first.
    asked: Option<(bool, String, Vec<String>)>,
    /// The services behind dependencies shown through what provides them, by the dependency as shown: name and version.
    services: Services,
}

/// Services by a dependency on them as shown (`pe2.ffmpeg@^1`): their name and version.
type Services = HashMap<String, (String, String)>;

const NODE: egui::Vec2 = egui::vec2(170.0, 46.0);
const GAP: egui::Vec2 = egui::vec2(28.0, 54.0);

/// The pane.
pub fn extensions_ui(ui: &mut egui::Ui, ws: &mut Workspace, user: &mut UserSettings, message: &mut Option<String>) {
    let id = egui::Id::unique("extensions pane");
    let mut state: State = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    // One of an id in effect; those whose place it takes are listed, and named in its details.
    let (all, services) = through_providers(ws.extensions.present.clone());
    state.services = services;
    let present: Vec<Present> = all.iter().filter(|p| !matches!(p.status, Status::Replaced(_))).cloned().collect();
    if state.selected.as_ref().is_none_or(|s| !present.iter().any(|p| p.id == *s)) {
        state.selected = present.first().map(|p| p.id.clone());
    }
    // At most most of the pane: a long value is cut short in it, never widening it over the graph.
    let widest = (ui.available_width() * 0.5).max(160.0);
    egui::Panel::right("extension details").resizable(true).default_size(300.0).size_range(160.0..=widest).show(
        ui,
        |ui| {
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                if let Some(selected) = state.selected.clone() {
                    details_ui(ui, ws, &all, &selected, &mut state, user, message);
                }
            });
        },
    );
    egui::CentralPanel::no_frame().show(ui, |ui| {
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            graph_ui(ui, &present, &mut state);
            ui.add_space(10.0);
            list_ui(ui, &all, &mut state);
        });
    });
    ui.data_mut(|d| d.insert_temp(id, state));
}

/// An extension's id from a requirement (`pe2.images@^0.1`), and the version it wants.
/// The extensions present with each requirement of a service shown as what provides it, where something does: what the
/// pane draws is what is met by what.
fn through_providers(mut present: Vec<Present>) -> (Vec<Present>, Services) {
    let providers: HashMap<String, (String, String)> = present
        .iter()
        .filter(|p| !matches!(p.status, Status::Replaced(_)))
        .flat_map(|p| {
            p.provides.iter().map(|s| {
                let (service, version) = requirement(s);
                (service.to_string(), (p.id.clone(), version.unwrap_or("0.0.0").to_string()))
            })
        })
        .collect();
    let mut services = Services::new();
    for p in &mut present {
        for text in p.requires.iter_mut().chain(&mut p.weak).chain(&mut p.optional).chain(&mut p.linked) {
            let (service, wanted) = requirement(text);
            if let Some((provider, version)) = providers.get(service) {
                let shown = wanted.map_or_else(|| provider.clone(), |wanted| format!("{provider}@{wanted}"));
                services.insert(shown.clone(), (service.to_string(), version.clone()));
                *text = shown;
            }
        }
    }
    (present, services)
}

/// What a dependency as shown asks for, said, and the version of the service meeting it, where one does.
fn asked_for<'a>(state: &'a State, text: &str) -> (String, Option<&'a str>) {
    let wanted = requirement(text).1.unwrap_or("any");
    match state.services.get(text) {
        Some((service, version)) => (format!("{service} {wanted}"), Some(version)),
        None => (wanted.to_string(), None),
    }
}

fn requirement(text: &str) -> (&str, Option<&str>) {
    match text.split_once('@') {
        Some((id, version)) => (id, Some(version)),
        None => (text, None),
    }
}

fn status_color(ui: &egui::Ui, status: Option<&Status>) -> Color32 {
    match status {
        Some(Status::Loaded) => theme::accent(),
        Some(Status::Disabled | Status::NotEnabled { .. } | Status::Replaced(_) | Status::Available) => theme::weak(),
        Some(Status::Refused(_)) => ui.visuals().error_fg_color,
        None => ui.visuals().warn_fg_color,
    }
}

fn status_text(status: &Status) -> &'static str {
    match status {
        Status::Loaded => "loaded",
        Status::Disabled => "left off by the project",
        Status::NotEnabled { changed: false } => "not enabled",
        Status::NotEnabled { changed: true } => "not the one you enabled",
        Status::Available => "available, not taken in",
        Status::Refused(_) => "not loaded",
        Status::Replaced(_) => "replaced",
    }
}

fn origin_text(origin: &Origin) -> String {
    match origin {
        Origin::Builtin => "built in".into(),
        Origin::Linked => "linked in".into(),
        Origin::Component(path) => {
            path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
        }
        Origin::Project { .. } => "project".into(),
        Origin::User(_) => "user".into(),
        Origin::Install(_) => "beside the editor".into(),
    }
}

/// What an extension is linked to weakly: turned off, it goes too.
fn weakly_linked(p: &Present) -> impl Iterator<Item = &str> {
    p.weak.iter().filter(|text| p.linked.contains(text)).map(|text| requirement(text).0)
}

/// What an extension goes with: what it requires, and what it is weakly linked to.
fn held(p: &Present) -> impl Iterator<Item = &str> {
    p.requires.iter().map(|text| requirement(text).0).chain(weakly_linked(p))
}

/// Of those going with an extension turned off, the ones to leave off by name: those weakly linked to it or to another
/// going, which would otherwise run without. What requires one is refused by itself, and back when it is.
fn left_off_too(present: &[Present], asked: &str, with: &[String]) -> Vec<String> {
    let going = |id: &str| id == asked || with.iter().any(|w| w == id);
    let linked = |id: &String| present.iter().find(|q| q.id == *id).is_some_and(|q| weakly_linked(q).any(going));
    with.iter().filter(|id| linked(id)).cloned().collect()
}

/// What an extension needs, and what needs it, both ways transitively.
fn chains(present: &[Present], id: &str) -> (HashSet<String>, HashSet<String>) {
    let needs_of = |id: &str| -> Vec<String> {
        present.iter().find(|p| p.id == id).map_or_default(|p| held(p).map(str::to_string).collect())
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
        for p in present.iter().filter(|p| held(p).any(|r| r == next)) {
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
    // Every way it depends on another, by id: what depends on another, weakly or not, sits above it.
    let needs: Vec<String> = present
        .iter()
        .find(|p| p.id == id)
        .map_or_default(|p| p.dependencies().map(|(text, _)| requirement(text).0.to_string()).collect());
    let d = needs.iter().map(|r| depth_of(r, present, depth, seen) + 1).max().unwrap_or(0);
    depth.insert(id.to_string(), d);
    d
}

/// An edge's line: whole for a requirement, dashed where weak, dotted where optional.
fn edge(painter: &egui::Painter, ends: [egui::Pos2; 2], stroke: egui::Stroke, dependency: Dependency) {
    match dependency {
        Dependency::Required => {
            painter.line_segment(ends, stroke);
        }
        Dependency::Weak => painter.extend(egui::Shape::dashed_line(&ends, stroke, 6.0, 4.0)),
        Dependency::Optional => painter.extend(egui::Shape::dashed_line(&ends, stroke, 2.0, 4.0)),
    }
}

/// Where each edge between two placed nodes starts and ends, by the ids at its ends: it leaves its node's bottom and
/// enters the other's top at a place of its own, spread along the side in the order of the nodes at their other ends,
/// so that edges sharing a node do not meet before it.
fn edge_ends(present: &[Present], places: &HashMap<&String, egui::Rect>) -> HashMap<(String, String), [egui::Pos2; 2]> {
    let edges: Vec<(&String, String)> = present
        .iter()
        .flat_map(|p| p.dependencies().map(|(text, _)| (&p.id, requirement(text).0.to_string())))
        .filter(|(from, to)| places.contains_key(from) && places.contains_key(to))
        .collect();
    let port = |rect: &egui::Rect, at: usize, of: usize, top: bool| {
        #[expect(clippy::cast_precision_loss, reason = "a handful of edges")]
        let x = rect.left() + rect.width() * (0.2 + 0.6 * (at as f32 + 1.0) / (of as f32 + 1.0));
        egui::pos2(x, if top { rect.top() } else { rect.bottom() })
    };
    let x_of = |id: &String| places.get(id).map_or(0.0, |r| r.center().x);
    let mut ends = HashMap::new();
    for (from, to) in &edges {
        let mut leaving: Vec<&String> = edges.iter().filter(|e| e.0 == *from).map(|e| &e.1).collect();
        leaving.sort_by(|a, b| x_of(a).total_cmp(&x_of(b)));
        let mut entering: Vec<&String> = edges.iter().filter(|e| e.1 == *to).map(|e| e.0).collect();
        entering.sort_by(|a, b| x_of(a).total_cmp(&x_of(b)));
        let (Some(out), Some(into)) =
            (leaving.iter().position(|id| *id == to), entering.iter().position(|id| id == from))
        else {
            continue;
        };
        let (Some(start), Some(end)) = (places.get(from), places.get(to)) else { continue };
        ends.insert(
            ((*from).clone(), to.clone()),
            [port(start, out, leaving.len(), false), port(end, into, entering.len(), true)],
        );
    }
    ends
}

/// An edge's label where it covers no node and no other label (`taken`, which it joins): along the edge from its middle
/// outward, on a chip of the pane's own color that hides the lines it lies across. Left out where there is no room;
/// the details say it.
fn edge_label(
    painter: &egui::Painter,
    [a, b]: [egui::Pos2; 2],
    galley: std::sync::Arc<egui::Galley>,
    fill: Color32,
    taken: &mut Vec<egui::Rect>,
) {
    let size = galley.size() + egui::vec2(8.0, 2.0);
    let chip = [0.5, 0.35, 0.65, 0.25, 0.75]
        .into_iter()
        .map(|t| egui::Rect::from_center_size(a.lerp(b, t), size))
        .find(|r| !taken.iter().any(|o| o.intersects(*r)));
    if let Some(chip) = chip {
        painter.rect_filled(chip, 3.0, fill);
        // Laid out in its color: nothing in it is left to the fallback.
        painter.galley(chip.center() - galley.size() / 2.0, galley, Color32::PLACEHOLDER);
        taken.push(chip);
    }
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
    let needed_by = |id: &str| present.iter().filter(|p| p.dependencies().any(|(r, _)| requirement(r).0 == id)).count();
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
    let ends = edge_ends(present, &places);
    // What the labels cover, so that none covers another: the nodes first.
    let mut taken: Vec<egui::Rect> = places.values().map(|r| r.expand(2.0)).collect();
    // Edges first, under the nodes.
    for p in present {
        for (text, dependency) in p.dependencies() {
            let id = requirement(text).0;
            let Some(&[a, b]) = ends.get(&(p.id.clone(), id.to_string())) else { continue };
            let found = present.iter().find(|q| q.id == id);
            // Met: what it needs is loaded, and it was not refused over it (a version too old).
            let refused = matches!(&p.status, Status::Refused(why) if why.contains(id));
            let there = found.is_some_and(|q| q.status == Status::Loaded) && !refused;
            let met = there || dependency != Dependency::Required;
            let on = lit(&p.id) && lit(id);
            let color = if !met {
                ui.visuals().error_fg_color
            } else if on {
                theme::accent()
            } else {
                theme::border().gamma_multiply(2.0)
            };
            let stroke = egui::Stroke::new(if on { 2.0 } else { 1.0 }, color);
            edge(&painter, [a, b], stroke, dependency);
            let tip = b - (b - a).normalized() * 9.0;
            let across = (b - a).normalized().rot90() * 4.0;
            painter.add(egui::Shape::convex_polygon(vec![b, tip + across, tip - across], color, egui::Stroke::NONE));
            // Short, on a line of its own beside the edge: the details name the service.
            let (wanted, service) = (requirement(text).1, asked_for(state, text).1);
            let label = format!(
                "{}  ·  has {}",
                wanted.unwrap_or("any"),
                service.or(found.map(|q| q.version.as_str())).unwrap_or("none")
            );
            edge_label(
                &painter,
                [a, b],
                painter.layout_no_wrap(label, font.clone(), color),
                ui.visuals().panel_fill,
                &mut taken,
            );
        }
    }
    nodes_ui(ui, &painter, present, &places, state, &lit);
}

/// The graph's nodes: each extension's name, version and where it is from, in the color of what became of it, dim
/// where something is chosen and it is not `lit`, dashed where nothing provides it; clicked, chosen.
fn nodes_ui(
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    present: &[Present],
    places: &HashMap<&String, egui::Rect>,
    state: &mut State,
    lit: &dyn Fn(&str) -> bool,
) {
    let chosen = state.selected.clone();
    let font = egui::TextStyle::Small.resolve(ui.style());
    for (id, rect) in places {
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
        let stroke =
            egui::Stroke::new(if selected { 2.0 } else { 1.0 }, color.gamma_multiply(if dim { 0.4 } else { 1.0 }));
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
        let body = egui::TextStyle::Body.resolve(ui.style());
        fitted(painter, rect.center_top() + egui::vec2(0.0, 13.0), id, body, text, NODE.x - 12.0);
        let note = found.map_or_else(
            || "not installed".to_string(),
            |p| {
                if p.version.is_empty() {
                    origin_text(&p.origin)
                } else {
                    format!("{}  ·  {}", p.version, origin_text(&p.origin))
                }
            },
        );
        fitted(
            painter,
            rect.center_bottom() - egui::vec2(0.0, 12.0),
            &note,
            font.clone(),
            theme::weak(),
            NODE.x - 12.0,
        );
    }
}

/// Text on one line centered at `at`, cut short with an ellipsis where wider than `width`.
pub(crate) fn fitted(
    painter: &egui::Painter,
    at: egui::Pos2,
    text: &str,
    font: egui::FontId,
    color: Color32,
    width: f32,
) {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_string(), font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(width);
    let galley = painter.layout_job(job);
    painter.galley(at - galley.size() / 2.0, galley, color);
}

/// Every extension by name, filtered: those in effect, then those whose place one takes.
fn list_ui(ui: &mut egui::Ui, all: &[Present], state: &mut State) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(icons::MAGNIFYING_GLASS).color(theme::weak()));
        ui.add(egui::TextEdit::singleline(&mut state.filter).hint_text("Filter extensions").desired_width(220.0));
    });
    let filter = state.filter.to_lowercase();
    let (replaced, effective): (Vec<&Present>, Vec<&Present>) = all
        .iter()
        .filter(|p| p.id.to_lowercase().contains(&filter))
        .partition(|p| matches!(p.status, Status::Replaced(_)));
    // A column each - what became of it (a dot, said on hover), name, version, where from - lined up across the rows.
    // Scrolled sideways where the pane is narrower than the columns, never cut.
    egui::ScrollArea::horizontal().id_salt("extensions list").show(ui, |ui| {
        egui::Grid::new("extensions list").num_columns(4).min_col_width(0.0).spacing([10.0, 4.0]).show(ui, |ui| {
            for p in effective.into_iter().chain(replaced) {
                let said = match &p.status {
                    Status::Refused(why) => format!("not loaded: {why}"),
                    Status::Replaced(by) => format!("replaced by {by}"),
                    status => status_text(status).to_string(),
                };
                let (dot, response) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                ui.painter().circle_filled(dot.center(), 4.0, status_color(ui, Some(&p.status)));
                response.on_hover_text(said);
                let selected =
                    state.selected.as_deref() == Some(p.id.as_str()) && !matches!(p.status, Status::Replaced(_));
                let name = RichText::new(&p.id).strong();
                let name = if matches!(p.status, Status::Replaced(_)) { name.color(theme::weak()) } else { name };
                if ui.selectable_label(selected, name).clicked() {
                    state.selected = Some(p.id.clone());
                }
                ui.label(RichText::new(&p.version).color(theme::weak()));
                ui.label(RichText::new(origin_text(&p.origin)).small().color(theme::weak()));
                ui.end_row();
            }
        });
    });
}

/// The extension selected: who it is, what it needs and registers, its switch.
fn details_ui(
    ui: &mut egui::Ui,
    ws: &mut Workspace,
    all: &[Present],
    selected: &str,
    state: &mut State,
    user: &mut UserSettings,
    message: &mut Option<String>,
) {
    // Those in effect: one of each id.
    let present: Vec<Present> = all.iter().filter(|p| !matches!(p.status, Status::Replaced(_))).cloned().collect();
    let present = present.as_slice();
    let Some(p) = present.iter().find(|p| p.id == selected) else { return };
    ui.label(RichText::new(&p.id).heading());
    ui.label(RichText::new(status_text(&p.status)).color(status_color(ui, Some(&p.status))));
    if let Status::Refused(why) = &p.status {
        ui.label(RichText::new(why).small().color(ui.visuals().error_fg_color));
    }
    ui.add_space(6.0);
    unsandboxed_ui(ui, ws, p, message);
    switch_ui(ui, ws, present, p, state, message);
    theme::section(ui, "About", |ui| {
        egui::Grid::new("extension about").num_columns(2).spacing([14.0, 4.0]).show(ui, |ui| {
            let mut row = |label: &str, value: String| {
                ui.label(RichText::new(label).color(theme::weak()));
                theme::short(ui, value.as_str()).on_hover_text(value);
                ui.end_row();
            };
            // One that cannot be read says nothing of who it is.
            let read = !p.version.is_empty();
            if read {
                row("Version", p.version.clone());
            }
            row("From", origin_text(&p.origin));
            if p.trust.is_some() {
                row("Sandbox", "none".to_string());
            }
            match &p.origin {
                Origin::Project { asset, .. } => row("Asset", asset.clone()),
                Origin::User(path) | Origin::Install(path) | Origin::Component(path) => {
                    row("File", path.display().to_string());
                }
                Origin::Builtin | Origin::Linked => {}
            }
            for q in all.iter().filter(|q| q.id == p.id && matches!(q.status, Status::Replaced(_))) {
                row("In place of", format!("{} {}", q.version, origin_text(&q.origin)));
            }
            // What it was built against; this editor's, where that differs (a version of the same line).
            let ours = project::ext::component::CONTRACT;
            let contract = match (&p.origin, &p.contract) {
                (Origin::Linked, _) => "none: linked in".to_string(),
                (_, Some(c)) if c == ours => format!("pe2:extension@{c}"),
                (_, Some(c)) => format!("pe2:extension@{c}, run by {ours}"),
                (_, None) => String::new(),
            };
            if !contract.is_empty() {
                row("Contract", contract);
            }
            if let Some(size) = p.size {
                row("Size", format!("{} KB", size.div_ceil(1024)));
            }
            if let Some(load) = p.load {
                row("Loaded in", format!("{:.1} ms", load.as_secs_f64() * 1000.0));
            }
            if let Some((_, m)) =
                ws.extensions.memory().into_iter().find(|(id, _)| *id == p.id).filter(|(_, m)| m.peak > 0)
            {
                let mib = |bytes: usize| bytes.div_ceil(1 << 20);
                let now = match m.instances {
                    0 => "none held now".to_string(),
                    n => format!("{} MiB in {n} instance{}", mib(m.bytes), if n == 1 { "" } else { "s" }),
                };
                row("Memory", now);
                row("Most held", format!("{} MiB", mib(m.peak)));
            }
        });
    });
    settings_ui(ui, ws, p, user, message);
    dependencies_ui(ui, p, present, state);
    theme::section(ui, "Registers", |ui| {
        if p.registers.iter().all(|(_, names)| names.is_empty()) {
            let note = if p.status == Status::Loaded { "Nothing" } else { "Nothing: it is not loaded" };
            ui.label(RichText::new(note).color(theme::weak()));
        }
        for (what, names) in p.registers.iter().filter(|(_, names)| !names.is_empty()) {
            egui::CollapsingHeader::new(format!("{what}  ·  {}", names.len()))
                .id_salt(("registers", &p.id, *what))
                .show(ui, |ui| {
                    for name in names {
                        ui.label(
                            RichText::new(name.split_once('@').map_or(name.as_str(), |(_, local)| local)).monospace(),
                        );
                    }
                });
        }
    });
}

/// Extensions' user settings, by id: the editor's own settings keep them.
pub type UserSettings = BTreeMap<String, toml::Table>;

/// Its settings, each edited the way its default's type is, where it is kept said: the project's written to
/// `project.toml` as a step of the history, the user's to the editor's settings. Either way it is loaded again.
fn settings_ui(
    ui: &mut egui::Ui,
    ws: &mut Workspace,
    p: &Present,
    user: &mut UserSettings,
    message: &mut Option<String>,
) {
    let declared = ws.extensions.settings.get(&p.id).cloned().unwrap_or_default();
    if declared.is_empty() {
        return;
    }
    theme::section(ui, "Settings", |ui| {
        egui::Grid::new(("extension settings", &p.id)).num_columns(3).min_col_width(0.0).spacing([12.0, 6.0]).show(
            ui,
            |ui| {
                for (setting, value) in &declared {
                    let label = setting.label.clone().unwrap_or_else(|| setting.key.clone());
                    let response = ui.label(&label);
                    if let Some(help) = &setting.help {
                        response.on_hover_text(help);
                    }
                    let mut chosen = value.clone();
                    setting_input(ui, (&p.id, &setting.key), &label, setting, &mut chosen);
                    let kept = match setting.scope {
                        Scope::Project => {
                            ws.project.extension_settings.get(&p.id).and_then(|t| t.get(&setting.key)).cloned()
                        }
                        Scope::User => user.get(&p.id).and_then(|t| t.get(&setting.key)).cloned(),
                    };
                    let mut next = (chosen != *value).then_some(Some(chosen));
                    ui.horizontal(|ui| {
                        let (scope, says) = match setting.scope {
                            Scope::Project => ("project", "Kept in project.toml: the same for everyone on the project"),
                            Scope::User => ("yours", "Kept in your editor settings: the same in every project"),
                        };
                        ui.label(RichText::new(scope).small().color(theme::weak())).on_hover_text(says);
                        if kept.is_some() {
                            let back = format!("Back to {}", setting.default);
                            let response = ui.small_button(icons::ARROW_ARC_LEFT);
                            response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &back));
                            if response.on_hover_text(&back).clicked() {
                                next = Some(None);
                            }
                        }
                    });
                    ui.end_row();
                    // A value its default's is not kept: going back to it removes the setting.
                    let Some(next) = next.map(|n| n.filter(|v| *v != setting.default)) else { continue };
                    let said = next.as_ref().map_or_else(|| "its default".to_string(), ToString::to_string);
                    match setting.scope {
                        Scope::Project => {
                            let written = project::switches::set_setting(
                                &ws.project,
                                &p.id,
                                &setting.key,
                                next.as_ref(),
                                &mut ws.history,
                                format!("Set {label} to {said}"),
                            );
                            if let Err(e) = written {
                                *message = Some(format!("project.toml: {e:#}"));
                            }
                        }
                        Scope::User => {
                            let table = user.entry(p.id.clone()).or_default();
                            match next {
                                Some(v) => {
                                    table.insert(setting.key.clone(), v);
                                }
                                None => {
                                    table.remove(&setting.key);
                                }
                            }
                            if table.is_empty() {
                                user.remove(&p.id);
                            }
                        }
                    }
                }
            },
        );
    });
}

/// A setting's input, by its default's type: a checkbox, a number, a text, or a list of its choices.
fn setting_input(ui: &mut egui::Ui, id: (&str, &str), label: &str, setting: &Setting, value: &mut toml::Value) {
    use toml::Value;
    if !setting.choices.is_empty() {
        let shown =
            setting.choices.iter().find(|(v, _)| v == value).map_or_else(|| value.to_string(), |(_, l)| l.clone());
        egui::ComboBox::from_id_salt(("setting", id)).selected_text(shown).show_ui(ui, |ui| {
            for (v, l) in &setting.choices {
                ui.selectable_value(value, v.clone(), l);
            }
        });
        return;
    }
    match value {
        Value::Boolean(b) => {
            // Its label is the row's: the checkbox is named by it for the window tools.
            let response = ui.checkbox(b, "");
            let on = *b;
            response.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, true, on, label));
        }
        Value::Integer(i) => {
            ui.add(egui::DragValue::new(i));
        }
        Value::Float(f) => {
            ui.add(egui::DragValue::new(f).speed(0.1));
        }
        Value::String(text) => {
            // Taken when the edit is done, not at each key.
            let key = ui.scope_id().with(("setting text", id));
            let mut draft = ui.data(|d| d.get_temp::<String>(key)).unwrap_or_else(|| text.clone());
            let response = ui.add(egui::TextEdit::singleline(&mut draft).desired_width(160.0));
            if response.lost_focus() {
                *text = draft;
                ui.data_mut(|d| d.remove::<String>(key));
            } else if response.has_focus() {
                ui.data_mut(|d| d.insert_temp(key, draft));
            }
        }
        other => {
            ui.label(RichText::new(other.to_string()).monospace().color(theme::weak()));
        }
    }
}

/// For an unsandboxed extension: enabling it, which is the user's and no project's. Until then nothing of it runs.
fn unsandboxed_ui(ui: &mut egui::Ui, ws: &Workspace, p: &Present, message: &mut Option<String>) {
    let Some(trust) = &p.trust else { return };
    // Where it is says it may run: there is nothing of its own to switch.
    if let Some(allowed) = extensions::trust::allowed_by(&p.origin) {
        ui.label(allowed.said());
        if let extensions::trust::Allowed::Place(place) = &allowed
            && ui.button("Stop trusting it").on_hover_text("For you, on this computer").clicked()
            && let Err(e) = extensions::trust::distrust_place(place)
        {
            *message = Some(format!("{}: {e}", place.display()));
        }
        ui.add_space(6.0);
        return;
    }
    let enable = matches!(p.status, Status::NotEnabled { .. });
    if enable {
        ui.label("This extension runs outside the sandbox: it and the programs it starts can read and change your files and reach the network. Enable it only if you trust where it came from.");
        if matches!(p.origin, Origin::Project { .. }) {
            ui.label(RichText::new("It came with this project.").strong());
        }
    }
    let label = if enable { "Enable" } else { "Disable" };
    if ui.button(label).on_hover_text("For you, on this computer, in every project").clicked() {
        if enable && crate::drive::agent_acting(ui.ctx()) {
            *message = Some(format!("{}: only you can enable it, not an agent", p.id));
        } else if let Err(e) = extensions::set_unsandboxed_enabled(trust, &p.id, enable) {
            *message = Some(format!("{}: {e}", p.id));
        }
    }
    // Where it is can be trusted instead, a no said when the project was opened taken back.
    if let Some((place, label)) =
        crate::onboarding::file_of(p).filter(|_| enable).map(|file| crate::onboarding::place_of(ws, file))
        && ui
            .button(format!("Trust {label}"))
            .on_hover_text("What is there runs from now on: for you, on this computer")
            .clicked()
    {
        if crate::drive::agent_acting(ui.ctx()) {
            *message = Some(format!("{label}: only you can trust it, not an agent"));
        } else if let Err(e) = extensions::trust::trust(&ws.project.root, std::slice::from_ref(&place)) {
            *message = Some(format!("{label}: {e}"));
        }
    }
    ui.add_space(6.0);
}

/// What an extension depends on and what depends on it, each a link to it.
fn dependencies_ui(ui: &mut egui::Ui, p: &Present, present: &[Present], state: &mut State) {
    theme::section(ui, "Requires", |ui| {
        if p.dependencies().next().is_none() {
            ui.label(RichText::new("Nothing").color(theme::weak()));
        }
        for (text, dependency) in p.dependencies() {
            let id = requirement(text).0;
            let found = present.iter().find(|q| q.id == id);
            let (asked, service) = asked_for(state, text);
            let service = service.map(str::to_string);
            ui.horizontal_wrapped(|ui| {
                if ui.link(id).clicked() && found.is_some() {
                    state.selected = Some(id.to_string());
                }
                let have = found.map_or_else(
                    || "not installed".to_string(),
                    |q| format!("{} ({})", service.as_deref().unwrap_or(&q.version), status_text(&q.status)),
                );
                let how = match dependency {
                    Dependency::Required => "",
                    Dependency::Weak => "if it can, and goes with it: ",
                    Dependency::Optional => "if it can, ",
                };
                ui.label(RichText::new(format!("{how}wants {asked}, has {have}")).small().color(theme::weak()));
            });
        }
    });
    theme::section(ui, "Required by", |ui| {
        let by: Vec<&Present> =
            present.iter().filter(|q| q.dependencies().any(|(r, _)| requirement(r).0 == p.id)).collect();
        if by.is_empty() {
            ui.label(RichText::new("Nothing").color(theme::weak()));
        }
        for q in by {
            if ui.link(&q.id).clicked() {
                state.selected = Some(q.id.clone());
            }
        }
    });
}

/// Taking an extension from outside the project in, or letting it go: whether there is nothing more to say of it.
fn taking_in_ui(
    ui: &mut egui::Ui,
    ws: &mut Workspace,
    present: &[Present],
    p: &Present,
    message: &mut Option<String>,
) -> bool {
    let imported = ws.project.manifest.imported.iter().find(|imported| imported.id == p.id).cloned();
    // One of its id may be in effect, a built-in, and this in the user's folder to take its place.
    let waiting = present.iter().find(|q| q.id == p.id && q.status == Status::Available);
    let change = if let Some(waiting) = waiting {
        if p.status != Status::Available {
            ui.label(
                RichText::new(format!("{} {} is {}", waiting.id, waiting.version, status_text(&waiting.status)))
                    .color(theme::weak()),
            );
        }
        let take = ui
            .button("Take it into the project")
            .on_hover_text("It is listed in project.toml, for everyone on the project");
        take.clicked().then(|| {
            let wanted = project::Imported {
                id: p.id.clone(),
                version: Some(accepted(&waiting.version)),
                ..project::Imported::default()
            };
            project::switches::import(&ws.project, &wanted, &mut ws.history, format!("Take {} in", p.id))
        })
    } else if let Some(imported) = imported.as_ref().filter(|_| !matches!(p.origin, Origin::Project { .. })) {
        ui.label(
            RichText::new(if imported.required { "The project requires it" } else { "Taken in by the project" })
                .color(theme::weak()),
        );
        ui.button("Let it go")
            .on_hover_text("It stops being listed in project.toml")
            .clicked()
            .then(|| project::switches::remove(&ws.project, &p.id, &mut ws.history, format!("Let {} go", p.id)))
    } else {
        None
    };
    if let Some(written) = change {
        *message = Some(match written {
            Ok(()) => format!("{}: taken in", p.id),
            Err(e) => format!("project.toml: {e:#}"),
        });
    }
    p.status == Status::Available
}

/// The versions a project accepts of what it takes in at this one.
fn accepted(version: &str) -> String {
    format!("^{}", version.rsplit_once('.').map_or(version, |(line, _)| line))
}

/// The switch: on or off for the project, saying first what goes with it.
fn switch_ui(
    ui: &mut egui::Ui,
    ws: &mut Workspace,
    present: &[Present],
    p: &Present,
    state: &mut State,
    message: &mut Option<String>,
) {
    // Not enabled, it is not this project's to turn on.
    if matches!(p.status, Status::NotEnabled { .. }) {
        return;
    }
    if taking_in_ui(ui, ws, present, p, message) {
        return;
    }
    let disabled: BTreeSet<String> = ws.project.disabled_extensions.iter().cloned().collect();
    let off = disabled.contains(&p.id);
    if !off && matches!(p.status, Status::Refused(_)) {
        // Refused over what it needs, it waits for that; one that cannot be read at all has no switch.
        let waits = p
            .requires
            .iter()
            .any(|r| present.iter().find(|q| q.id == requirement(r).0).is_none_or(|q| q.status != Status::Loaded));
        if waits {
            ui.label(RichText::new("It cannot be turned on until what it needs is.").small().color(theme::weak()));
        }
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
                    next.extend(left_off_too(present, &asked, &with));
                }
                let next: Vec<String> = next.into_iter().collect();
                let label = format!("Turn {asked} {verb}");
                let written = project::switches::set_disabled(&ws.project, &next, &mut ws.history, label);
                *message = Some(match written {
                    Ok(()) => format!("{asked} turned {verb}"),
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
        // Off: those going with it. On: those it requires and the project left off come with it.
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

#[cfg(test)]
mod tests {
    use super::{State, asked_for, chains, left_off_too, requirement, through_providers};
    use project::ext::{Origin, Present, Status};

    fn extension(id: &str, requires: &[&str], weak: &[&str], optional: &[&str], linked: &[&str]) -> Present {
        let strings = |list: &[&str]| list.iter().map(ToString::to_string).collect::<Vec<_>>();
        Present {
            id: id.to_string(),
            version: "0.1.0".to_string(),
            origin: Origin::Builtin,
            requires: strings(requires),
            weak: strings(weak),
            optional: strings(optional),
            provides: vec![],
            linked: strings(linked),
            status: Status::Loaded,
            size: None,
            contract: None,
            load: None,
            registers: vec![],
            trust: None,
        }
    }

    #[test]
    fn a_service_is_shown_through_what_provides_it_as_it_was_asked_for() {
        let mut provider = extension("pe2.ffmpeg", &[], &[], &[], &[]);
        provider.provides = vec!["video-container@1.0.0".to_string()];
        let present = vec![
            extension("user", &[], &["video-container@^1"], &[], &["video-container@^1"]),
            provider,
            extension("plain", &["pe2.ffmpeg"], &[], &[], &[]),
        ];
        let (shown, services) = through_providers(present);
        let state = State { services, ..State::default() };
        let weak = &shown[0].weak[0];
        assert_eq!(requirement(weak), ("pe2.ffmpeg", Some("^1")));
        assert_eq!(shown[0].linked, shown[0].weak);
        assert_eq!(asked_for(&state, weak), ("video-container ^1".to_string(), Some("1.0.0")));
        // Asking for the extension itself is not asking for its service, and any version is any.
        assert_eq!(asked_for(&state, &shown[2].requires[0]), ("any".to_string(), None));
    }

    #[test]
    fn turning_one_off_takes_what_is_linked_weakly_and_what_requires_it() {
        let present = [
            extension("a", &[], &[], &[], &[]),
            extension("linked", &[], &["a@^0.1"], &[], &["a@^0.1"]),
            extension("unlinked", &[], &["a@^2"], &[], &[]),
            extension("optional", &[], &[], &["a"], &["a"]),
            extension("requiring", &["a"], &[], &[], &["a"]),
            extension("after", &["linked"], &[], &[], &["linked"]),
            extension("further", &[], &["linked"], &[], &["linked"]),
        ];
        let (_, going) = chains(&present, "a");
        let mut with: Vec<String> = going.into_iter().collect();
        with.sort();
        assert_eq!(with, ["after", "further", "linked", "requiring"]);
        // What is requires follows by itself, and comes back by itself.
        assert_eq!(left_off_too(&present, "a", &with), ["further", "linked"]);
    }
}
