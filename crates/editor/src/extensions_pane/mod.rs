//! The Extensions pane: every extension present, enabled or not, what each is and does, and what they fetch onto this
//! computer. A toolbar chooses what it shows: the extensions as a list (filtered by name) or as their dependency graph
//! (`graph`), each beside the details of the one selected (`details`); or everything fetched onto this computer
//! (`fetched`). The layout follows the pane's size: on a wide pane the overview and the details sit side by side, each
//! scrolling on its own; on a narrow one they are stacked, the overview above, kept short so that the details stay in
//! reach. Details and the fetched view are cards, as many columns of them as fit (`Cards`).
//!
//! An extension is turned off or on for the project (`[extensions] disabled` in `project.toml`, a step of the
//! history), saying first what goes with it: those requiring it and those weakly linked to it, turned off with it, or
//! those it requires, turned on with it. The editor then opens the project again with its extensions as they now
//! are. One that runs outside the sandbox is enabled first, by the user and for every project; until then it does not
//! run.
//!
//! What the project holds it holds as copies (`project::ext::folder`): one kept apart - in your own extensions, beside
//! the editor - is shown, and added to the project from there, or the one it holds of its id updated from there; one it
//! holds is removed. Each is a step of the history. Its folder's guides, installation notes and the licenses of what it
//! ships open from its details.

mod details;
mod fetched;
mod graph;

use eframe::egui::{self, Color32, RichText};
use project::ext::setup::kept::Kept;
use project::ext::{Origin, Present, Status};
use std::collections::{BTreeMap, HashMap, HashSet};

use crate::workspace::Workspace;
use crate::{icons, theme};

/// What the pane shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum View {
    /// The extensions by name, beside the details of the one selected.
    #[default]
    List,
    /// How they depend on each other, beside the details of the one selected.
    Graph,
    /// Everything fetched onto this computer: the project's extensions' packages and what else is kept.
    Fetched,
}

/// What the pane keeps between frames.
#[derive(Clone, Default)]
struct State {
    view: View,
    selected: Option<String>,
    filter: String,
    /// A change asked for, waiting for its confirmation: the extensions to turn off or on, the one asked first.
    asked: Option<(bool, String, Vec<String>)>,
    /// The services behind dependencies shown through what provides them, by the dependency as shown: name and version.
    services: Services,
    /// What the store of what extensions fetch holds, and when it was read: read again a few seconds after.
    kept: Option<(std::time::Instant, Vec<Kept>)>,
}

/// Below this width, in points, the overview and the details are stacked; at it and above, side by side.
const SIDE_BY_SIDE: f32 = 640.0;
/// The narrowest the details are beside the overview.
const DETAILS_NARROWEST: f32 = 300.0;
/// The narrowest a card is: as many columns as fit at least this wide.
const CARD: f32 = 280.0;
/// Between cards, and between the overview and the details.
const GUTTER: f32 = 10.0;

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
    toolbar_ui(ui, &mut state);
    ui.add_space(4.0);
    let body = ui.available_rect_before_wrap();
    if state.view == View::Fetched {
        region(ui, body, "fetched", false, |ui| fetched::fetched_ui(ui, ws, &mut state, message));
    } else {
        let graph = state.view == View::Graph;
        let mut wide = if graph { (body.width() * 0.55).max(360.0) } else { (body.width() * 0.3).clamp(220.0, 340.0) };
        // Laid out across, the graph takes what it needs of its share, the details the rest.
        if graph && let Some(needs) = graph::width_across(&present, egui::vec2(wide, body.height())) {
            wide = wide.min(needs + 16.0);
        }
        let (overview, details) = if body.width() >= SIDE_BY_SIDE && body.width() - wide - GUTTER >= DETAILS_NARROWEST {
            let (left, right) = body.split_left_right_at_x(body.left() + wide);
            ui.painter().vline(left.right() + GUTTER / 2.0, body.y_range(), egui::Stroke::new(1.0, theme::border()));
            (left, right.with_min_x(right.left() + GUTTER))
        } else {
            // Stacked: the overview no taller than it needs, and never most of the pane.
            let filter = state.filter.to_lowercase();
            let rows = all.iter().filter(|p| p.id.to_lowercase().contains(&filter)).count();
            #[expect(clippy::cast_precision_loss, reason = "a handful of extensions")]
            let listed = rows as f32 * LIST_ROW + 16.0;
            let tall = if graph { body.height() * 0.5 } else { listed.min(body.height() * 0.4) }.max(80.0);
            let (top, bottom) = body.split_top_bottom_at_y(body.top() + tall);
            ui.painter().hline(body.x_range(), top.bottom() + GUTTER / 2.0, egui::Stroke::new(1.0, theme::border()));
            (top, bottom.with_min_y(bottom.top() + GUTTER))
        };
        // The graph scrolls both ways: laid out across, it may be wider than its part of the pane.
        region(ui, overview, "overview", graph, |ui| {
            if graph {
                graph::graph_ui(ui, &present, &mut state, overview.size());
            } else {
                list_ui(ui, &all, &mut state);
            }
        });
        region(ui, details, "details", false, |ui| {
            if let Some(selected) = state.selected.clone() {
                details::details_ui(ui, ws, &all, &selected, &mut state, user, message);
            }
        });
    }
    ui.advance_cursor_after_rect(body);
    ui.data_mut(|d| d.insert_temp(id, state));
}

/// What the pane shows, chosen; and, for the list, its filter.
fn toolbar_ui(ui: &mut egui::Ui, state: &mut State) {
    ui.horizontal(|ui| {
        let views = [
            (View::List, "List", "The extensions by name"),
            (View::Graph, "Graph", "How the extensions depend on each other"),
            (View::Fetched, "Fetched", "Everything extensions fetched onto this computer, for every project"),
        ];
        for (view, label, says) in views {
            if ui.selectable_label(state.view == view, label).on_hover_text(says).clicked() {
                state.view = view;
            }
        }
        if state.view == View::List {
            ui.add_space(12.0);
            ui.label(RichText::new(icons::MAGNIFYING_GLASS).color(theme::weak()));
            ui.add(egui::TextEdit::singleline(&mut state.filter).hint_text("Filter extensions").desired_width(200.0));
        }
    });
}

/// A part of the pane, scrolling on its own: down, or both ways.
fn region(ui: &mut egui::Ui, rect: egui::Rect, salt: &str, both: bool, add: impl FnOnce(&mut egui::Ui)) {
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .id_salt(("extensions region", salt))
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    let scroll = if both { egui::ScrollArea::both() } else { egui::ScrollArea::vertical() };
    scroll.id_salt(("extensions region", salt)).auto_shrink(false).show(&mut child, add);
}

/// Cards laid out in as many columns as fit at least `CARD` wide, each placed under the shortest column: a pane's
/// width used whatever it is. A card that draws nothing takes no room.
struct Cards {
    left: f32,
    top: f32,
    width: f32,
    columns: Vec<f32>,
    count: usize,
}

impl Cards {
    fn new(ui: &egui::Ui) -> Cards {
        let rect = ui.available_rect_before_wrap();
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a pane's width in cards")]
        let n = (((rect.width() + GUTTER) / (CARD + GUTTER)).floor() as usize).max(1);
        #[expect(clippy::cast_precision_loss, reason = "a few columns")]
        let width = ((rect.width() - GUTTER * (n - 1) as f32) / n as f32).max(1.0);
        Cards { left: rect.left(), top: rect.top(), width, columns: vec![rect.top(); n], count: 0 }
    }

    /// A card, under the shortest column.
    fn card(&mut self, ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
        let (column, y) =
            self.columns.iter().copied().enumerate().min_by(|a, b| a.1.total_cmp(&b.1)).unwrap_or((0, self.top));
        #[expect(clippy::cast_precision_loss, reason = "a few columns")]
        let x = self.left + column as f32 * (self.width + GUTTER);
        let rect = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(self.width, f32::INFINITY));
        self.count += 1;
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .id_salt(("card", self.count))
                .max_rect(rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        add(&mut child);
        let used = child.min_rect();
        if used.height() >= 1.0 {
            self.columns[column] = used.bottom() + 4.0;
        }
    }

    /// The room the cards took, taken from the Ui they are in.
    fn finish(self, ui: &mut egui::Ui) {
        let bottom = self.columns.iter().copied().fold(self.top, f32::max);
        #[expect(clippy::cast_precision_loss, reason = "a few columns")]
        let right = self.left + self.columns.len() as f32 * (self.width + GUTTER) - GUTTER;
        ui.advance_cursor_after_rect(egui::Rect::from_min_max(
            egui::pos2(self.left, self.top),
            egui::pos2(right, bottom),
        ));
    }
}

/// How tall a row of the list is, for sizing it when stacked.
const LIST_ROW: f32 = 22.0;

/// Services by a dependency on them as shown (`pe2.ffmpeg@^1`): their name and version.
type Services = HashMap<String, (String, String)>;

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
        Status::Available => "kept, not in the project",
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
        Origin::Project(_) => "project".into(),
        Origin::User(_) => "yours".into(),
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

/// Extensions' user settings, by id: the editor's own settings keep them.
pub type UserSettings = BTreeMap<String, toml::Table>;

/// Every extension by name, filtered: those in effect, then those whose place one takes.
fn list_ui(ui: &mut egui::Ui, all: &[Present], state: &mut State) {
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
                // Named as the graph's nodes are, by what became of it, for the window's tools.
                let response = ui.selectable_label(selected, name);
                let named = format!("{}: {}", p.id, status_text(&p.status));
                response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, selected, &named));
                if response.clicked() {
                    state.selected = Some(p.id.clone());
                }
                ui.label(RichText::new(&p.version).color(theme::weak()));
                ui.label(RichText::new(origin_text(&p.origin)).small().color(theme::weak()));
                ui.end_row();
            }
        });
    });
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
            described: project::ext::component::Described::default(),
            setup: vec![],
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
