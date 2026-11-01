//! The stack as the editor shows it: the Layers pane - one row per layer, top to bottom, with its eye, its lock, its
//! kind and counts; the active one highlighted; dragged to reorder within what the layers need of each other - and the
//! Layer pane, the active layer's changeset: its operations with their status, its creates, its problems.

use eframe::egui::{self, RichText};
use project::LayerKind;
use project::changeset::Op;
use project::index::{Kind, LayerInfo, OpStatus};

use crate::icons;
use crate::theme;
use crate::workspace::{EditRequest, Workspace};

/// What the Layers pane keeps between frames: the name being typed for a new layer.
#[derive(Default)]
pub struct LayersState {
    pub new_layer: Option<String>,
    /// The kind of the layer being added: plain files (true) or assets.
    pub files: bool,
}

/// A layer's row being dragged: its name.
#[derive(Clone)]
struct Dragged(String);

/// The Layers pane.
pub fn layers_ui(ui: &mut egui::Ui, ws: &mut Workspace, state: &mut LayersState, message: &mut Option<String>) {
    let mut action: Option<Action> = None;
    let active = ws.active_layer().map(|l| l.name.clone());
    let rows: Vec<LayerInfo> = ws.layers.iter().rev().cloned().collect();
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        for layer in &rows {
            row_ui(ui, ws, layer, active.as_deref() == Some(layer.name.as_str()), &mut action);
        }
    });
    ui.add_space(6.0);
    ui.separator();
    footer_ui(ui, ws, state, &mut action);
    if let Some(action) = action {
        *message = action.apply(ws, state);
    }
}

/// What a click in the pane asks for.
enum Action {
    Eye(String),
    Solo(String),
    Lock(String),
    Activate(String),
    /// A layer put above (true) or below another.
    Move {
        layer: String,
        target: String,
        above: bool,
    },
    /// A new layer, of plain files (true) or of assets.
    Add(String, bool),
    Remove(String),
}

impl Action {
    /// Do it; what to say in the status bar.
    fn apply(self, ws: &mut Workspace, state: &mut LayersState) -> Option<String> {
        let result = match self {
            Action::Eye(name) => ws.toggle_eye(&name).map(|()| None),
            Action::Solo(name) => ws.solo(&name).map(|()| None),
            Action::Lock(name) => ws.toggle_lock(&name).map(|()| None),
            Action::Activate(name) => ws.activate(&name).map(|()| Some(format!("{name} is the active layer"))),
            Action::Move { layer, target, above } => {
                let mut order = ws.stack_order();
                order.retain(|n| *n != layer);
                let at = order.iter().position(|n| *n == target).map_or(order.len(), |i| if above { i + 1 } else { i });
                order.insert(at, layer.clone());
                ws.reorder(&order).map(|()| Some(format!("{layer} moved")))
            }
            Action::Add(name, files) => {
                let kind = if files { project::LayerKind::Files } else { project::LayerKind::Assets };
                ws.add_layer(&name, kind).map(|_| {
                    state.new_layer = None;
                    Some(format!("{name} added at the top of the stack"))
                })
            }
            Action::Remove(name) => {
                ws.remove_layer(&name).map(|()| Some(format!("{name} taken out of the stack; its files are kept")))
            }
        };
        result.unwrap_or_else(|e| Some(format!("{e:#}")))
    }
}

/// A layer's card: the whole of it clickable (it makes the layer active) and, but for a mount, draggable to reorder;
/// its eye and lock buttons take their own clicks.
fn row_ui(ui: &mut egui::Ui, ws: &Workspace, layer: &LayerInfo, active: bool, action: &mut Option<Action>) {
    let hidden = ws.is_hidden(layer);
    let locked = ws.is_locked(layer);
    let body = ui.text_style_height(&egui::TextStyle::Body);
    let small = ui.text_style_height(&egui::TextStyle::Small);
    let size = egui::vec2(ui.available_width(), body + small + 8.0);
    // The card's own interaction comes first: the controls placed on it after are above it, and keep their clicks.
    let sense = if layer.mounted { egui::Sense::click() } else { egui::Sense::click_and_drag() };
    let (rect, card) = ui.allocate_exact_size(size, sense);
    if !layer.mounted {
        card.dnd_set_drag_payload(Dragged(layer.name.clone()));
    }
    let dragged = card.dragged();
    let soloed = ws.layer_view.solo.as_deref() == Some(layer.name.as_str());
    let (mut fill, mut stroke) = (theme::raised(), egui::Stroke::new(1.0, theme::border()));
    if soloed {
        fill = fill.lerp_to_gamma(theme::solo(), 0.2);
        stroke = egui::Stroke::new(1.5, theme::solo());
    } else if active {
        fill = fill.lerp_to_gamma(theme::accent(), 0.22);
        stroke = egui::Stroke::new(1.5, theme::accent());
    } else if card.hovered() {
        fill = fill.lerp_to_gamma(theme::text(), 0.06);
    }
    if dragged {
        fill = fill.gamma_multiply(0.6);
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    }
    ui.painter().rect(rect, 5.0, fill, stroke, egui::StrokeKind::Inside);
    let inner = rect.shrink2(egui::vec2(8.0, 3.0));
    let mut content =
        ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::left_to_right(egui::Align::Center)));
    handle_ui(&mut content, layer);
    eye_ui(&mut content, ws, layer, hidden, action);
    solo_ui(&mut content, layer, soloed, action);
    lock_ui(&mut content, ws, layer, locked, action);
    content.add_space(4.0);
    content.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let tint = if hidden { theme::weak() } else { theme::text() };
        ui.add(egui::Label::new(RichText::new(&layer.name).strong().color(tint)).selectable(false).truncate());
        let summary = RichText::new(summary(layer)).small().color(theme::weak());
        ui.add(egui::Label::new(summary).selectable(false).truncate());
    });
    let card = card.on_hover_ui(|ui| hover_ui(ui, ws, layer));
    if card.clicked() {
        *action = Some(Action::Activate(layer.name.clone()));
    }
    card.context_menu(|ui| menu_ui(ui, ws, layer, action));
    drop_ui(ui, layer, rect, action);
}

/// A layer's eye: shown or hidden. While a layer is soloed the solo decides, and the eyes only say what it shows.
fn eye_ui(ui: &mut egui::Ui, ws: &Workspace, layer: &LayerInfo, hidden: bool, action: &mut Option<Action>) {
    let eye = if hidden { icons::EYE_SLASH } else { icons::EYE };
    let tint = if hidden { theme::weak() } else { theme::text() };
    if let Some(solo) = &ws.layer_view.solo {
        ui.label(RichText::new(eye).color(tint.gamma_multiply(0.7)))
            .on_hover_text(format!("{solo} is soloed: its solo decides what is shown"));
        return;
    }
    let tip = if hidden { "Show it" } else { "Hide it" };
    if icon_button(ui, eye, tint).on_hover_text(tip).clicked() {
        *action = Some(Action::Eye(layer.name.clone()));
    }
}

/// A layer's solo, as on a mixer channel: lit while it is shown alone - the layers above it hidden, every other
/// layer locked, it the active one. Clicking it again, or another layer's, ends or moves the solo.
fn solo_ui(ui: &mut egui::Ui, layer: &LayerInfo, soloed: bool, action: &mut Option<Action>) {
    let (text, fill) = if soloed {
        (RichText::new("S").strong().color(theme::raised()), theme::solo())
    } else {
        (RichText::new("S").strong().color(theme::weak()), egui::Color32::TRANSPARENT)
    };
    let button = egui::Button::new(text)
        .fill(fill)
        .stroke(egui::Stroke::new(1.0, if soloed { theme::solo() } else { theme::weak().gamma_multiply(0.5) }))
        .corner_radius(3.0)
        .min_size(egui::vec2(18.0, 16.0));
    let tip = if soloed {
        "Soloed: end the solo"
    } else {
        "Solo: show it alone - the layers above it hidden, every other layer locked"
    };
    if ui.add(button).on_hover_text(tip).clicked() {
        *action = Some(Action::Solo(layer.name.clone()));
    }
}

/// Drop a dragged row on this one: above or below it by where the pointer is. A mount takes no drops.
fn drop_ui(ui: &egui::Ui, layer: &LayerInfo, row: egui::Rect, action: &mut Option<Action>) {
    let Some(pointer) = ui.ctx().pointer_hover_pos().filter(|p| row.contains(*p)) else { return };
    let Some(dragged) = egui::DragAndDrop::payload::<Dragged>(ui.ctx()) else { return };
    if layer.mounted || dragged.0 == layer.name {
        return;
    }
    let above = pointer.y < row.center().y;
    let y = if above { row.top() } else { row.bottom() };
    ui.painter().hline(row.x_range(), y, egui::Stroke::new(2.0, ui.visuals().selection.stroke.color));
    if ui.input(|i| i.pointer.any_released()) {
        *action = Some(Action::Move { layer: dragged.0.clone(), target: layer.name.clone(), above });
        egui::DragAndDrop::clear_payload(ui.ctx());
    }
}

/// What the card's left edge says: a grip where it can be dragged, a plug where it is a mount, fixed.
fn handle_ui(ui: &mut egui::Ui, layer: &LayerInfo) {
    if layer.mounted {
        ui.label(RichText::new(icons::PLUGS_CONNECTED).color(theme::weak()))
            .on_hover_text("A mount: declared apart in project.toml ([[mount]]), at the bottom of the stack");
    } else {
        ui.add(egui::Label::new(RichText::new(icons::DOTS_SIX_VERTICAL).color(theme::weak())).selectable(false));
    }
}

/// A layer's lock: closed and plain where it is locked - by nature, which no click undoes, or by hand - and an open
/// lock, faint, where it is not.
fn lock_ui(ui: &mut egui::Ui, ws: &Workspace, layer: &LayerInfo, locked: bool, action: &mut Option<Action>) {
    if Workspace::locked_by_nature(layer) || ws.locked_by_solo(layer) {
        ui.label(RichText::new(icons::LOCK_SIMPLE).color(theme::text())).on_hover_text(locked_reason(ws, layer));
        return;
    }
    let (icon, tint, tip) = if locked {
        (icons::LOCK_SIMPLE, theme::text(), "Locked by hand: unlock it")
    } else {
        (icons::LOCK_SIMPLE_OPEN, theme::weak().gamma_multiply(0.5), "Lock it")
    };
    if icon_button(ui, icon, tint).on_hover_text(tip).clicked() {
        *action = Some(Action::Lock(layer.name.clone()));
    }
}

fn locked_reason(ws: &Workspace, layer: &LayerInfo) -> String {
    if !Workspace::locked_by_nature(layer)
        && let Some(solo) = &ws.layer_view.solo
    {
        format!("Locked while {solo} is shown alone")
    } else if ws.project.game.as_ref().is_some_and(|g| g.layer == layer.name) {
        "Locked: the game layer is written by extraction alone".into()
    } else if layer.mounted {
        "Locked: the mounted tree is read-only".into()
    } else {
        "Locked: its maker writes it".into()
    }
}

fn icon_button(ui: &mut egui::Ui, icon: &str, tint: egui::Color32) -> egui::Response {
    ui.add(egui::Button::new(RichText::new(icon).color(tint)).frame(false))
}

/// A layer's kind and counts, one line.
fn summary(layer: &LayerInfo) -> String {
    let kind = match layer.kind {
        LayerKind::Assets => "assets".to_string(),
        LayerKind::Files if layer.mounted => format!("files, mounted at {}/", layer.at),
        LayerKind::Files => "files".to_string(),
    };
    let mut parts = vec![kind, format!("{} entr{}", layer.entries, if layer.entries == 1 { "y" } else { "ies" })];
    let ops = layer.overrides + layer.moves + layer.deletes;
    if ops > 0 {
        parts.push(format!("{ops} operation{}", if ops == 1 { "" } else { "s" }));
    }
    if layer.problems > 0 {
        parts.push(format!("{} problem{}", layer.problems, if layer.problems == 1 { "" } else { "s" }));
    }
    parts.join("  ·  ")
}

fn hover_ui(ui: &mut egui::Ui, ws: &Workspace, layer: &LayerInfo) {
    ui.label(RichText::new(&layer.name).strong());
    ui.label(format!("{} overrides, {} moves, {} deletes", layer.overrides, layer.moves, layer.deletes));
    for d in &layer.depends_on {
        ui.label(RichText::new(d.describe(&layer.name)).color(theme::weak()));
    }
    for other in ws.layers.iter().filter(|l| l.depends_on.iter().any(|d| d.layer == layer.name)) {
        ui.label(RichText::new(format!("{} needs it", other.name)).color(theme::weak()));
    }
}

fn menu_ui(ui: &mut egui::Ui, ws: &Workspace, layer: &LayerInfo, action: &mut Option<Action>) {
    if ui.add_enabled(ws.layer_view.solo.is_none(), egui::Button::new("Make active")).clicked() {
        *action = Some(Action::Activate(layer.name.clone()));
    }
    let soloed = ws.layer_view.solo.as_deref() == Some(layer.name.as_str());
    let label = if soloed { "End the solo" } else { "Solo" };
    if ui.button(label).on_hover_text("Show it alone: the layers above it hidden, every other layer locked").clicked() {
        *action = Some(Action::Solo(layer.name.clone()));
    }
    if !layer.mounted {
        let order = ws.stack_order();
        let at = order.iter().position(|n| *n == layer.name);
        ui.separator();
        let above = at.and_then(|i| order.get(i + 1));
        if ui.add_enabled(above.is_some(), egui::Button::new("Move up")).clicked()
            && let Some(target) = above
        {
            *action = Some(Action::Move { layer: layer.name.clone(), target: target.clone(), above: true });
        }
        let below = at.and_then(|i| i.checked_sub(1)).and_then(|i| order.get(i));
        if ui.add_enabled(below.is_some(), egui::Button::new("Move down")).clicked()
            && let Some(target) = below
        {
            *action = Some(Action::Move { layer: layer.name.clone(), target: target.clone(), above: false });
        }
        ui.separator();
        if ui.button("Take out of the stack").on_hover_text("Its files stay on disk").clicked() {
            *action = Some(Action::Remove(layer.name.clone()));
        }
    }
}

fn footer_ui(ui: &mut egui::Ui, ws: &Workspace, state: &mut LayersState, action: &mut Option<Action>) {
    let mut cancel = false;
    match &mut state.new_layer {
        None => {
            if ui
                .button(format!("{}  New layer", icons::PLUS))
                .on_hover_text("An empty layer at the top of the stack")
                .clicked()
            {
                state.new_layer = Some(String::new());
            }
        }
        Some(name) => {
            ui.horizontal(|ui| {
                let edit = ui.add(egui::TextEdit::singleline(name).hint_text("name").desired_width(140.0));
                edit.request_focus();
                ui.selectable_value(&mut state.files, false, "Assets").on_hover_text("Assets and plain files");
                ui.selectable_value(&mut state.files, true, "Files").on_hover_text("Plain files only: code, a patch");
                let valid = project::stack::check_name(&ws.project, name.trim());
                let add = ui.add_enabled(valid.is_ok(), egui::Button::new("Add"));
                if add.clicked()
                    || (edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && valid.is_ok())
                {
                    *action = Some(Action::Add(name.trim().to_string(), state.files));
                }
                cancel = ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape));
                if let Err(e) = valid
                    && !name.trim().is_empty()
                {
                    ui.label(RichText::new(e.to_string()).small().color(ui.visuals().warn_fg_color));
                }
            });
        }
    }
    if cancel {
        state.new_layer = None;
    }
}

// --- the layer pane ---------------------------------------------------------------------------------------------------

/// The Layer pane: the active layer's changeset.
pub fn layer_ui(ui: &mut egui::Ui, ws: &Workspace, nav: &mut Option<String>, edit: &mut Option<EditRequest>) {
    let Some(layer) = ws.active_layer().cloned() else {
        ui.label(RichText::new("The project has no layers").color(theme::weak()));
        return;
    };
    let definition = ws.project.layers.iter().find(|l| l.name == layer.name);
    ui.horizontal(|ui| {
        ui.label(RichText::new(&layer.name).heading());
        if ws.is_locked(&layer) {
            ui.label(RichText::new(format!("{}  read-only", icons::LOCK_SIMPLE)).small().color(theme::weak()));
        }
    });
    if let Some(definition) = definition {
        ui.label(RichText::new(summary(&layer)).small().color(theme::weak()));
        let line = |text: String| RichText::new(text).small().color(theme::weak());
        theme::short(ui, line(format!("content  {}", definition.path.display())));
        theme::short(ui, line(format!("changeset  {}", definition.changeset.display())));
    }
    let needs: Vec<String> = layer.depends_on.iter().map(|d| d.describe(&layer.name)).collect();
    let needed: Vec<String> = ws
        .layers
        .iter()
        .flat_map(|l| l.depends_on.iter().filter(|d| d.layer == layer.name).map(|d| d.describe(&l.name)))
        .collect();
    if !needs.is_empty() || !needed.is_empty() {
        theme::section(ui, "Dependencies", |ui| {
            for line in needs.iter().chain(&needed) {
                ui.label(line);
            }
        });
    }
    let problems: Vec<String> = ws
        .index
        .problems
        .iter()
        .filter(|p| p.layers().contains(&layer.name.as_str()))
        .map(project::index::Problem::describe)
        .collect();
    if !problems.is_empty() {
        theme::section(ui, &format!("Problems ({})", problems.len()), |ui| {
            for p in problems.iter().take(200) {
                ui.label(RichText::new(format!("{}  {p}", icons::WARNING)).color(ui.visuals().warn_fg_color));
            }
        });
    }
    let writable = ws.editable_layer().is_ok();
    operations_ui(ui, ws, &layer.name, nav, writable.then_some(edit));
    creates_ui(ui, ws, &layer.name, nav);
}

/// A layer's overrides, moves and deletes, each with its status.
fn operations_ui(
    ui: &mut egui::Ui,
    ws: &Workspace,
    layer: &str,
    nav: &mut Option<String>,
    mut edit: Option<&mut Option<EditRequest>>,
) {
    let operations = ws.index.operations(layer);
    theme::section(ui, &format!("Operations ({})", operations.len()), |ui| {
        if operations.is_empty() {
            ui.label(RichText::new("It overrides, moves and deletes nothing below it").color(theme::weak()));
        }
        for op in operations.iter().take(500) {
            let (icon, tint, status) = match op.status {
                OpStatus::Applied => (icons::CHECK_CIRCLE, theme::weak(), "applied"),
                OpStatus::Stale => (icons::WARNING, ui.visuals().warn_fg_color, "stale: its base changed"),
                OpStatus::Unbound => (icons::X_CIRCLE, ui.visuals().error_fg_color, "unbound: nothing below"),
                OpStatus::Missing => (icons::X_CIRCLE, ui.visuals().error_fg_color, "its own version is missing"),
                OpStatus::Conflict => (icons::X_CIRCLE, ui.visuals().error_fg_color, "conflict at its target"),
            };
            let (what, to) = match &op.op {
                Op::Override { to, .. } => ("override", to.as_deref()),
                Op::Move { to } => ("move", Some(to.as_str())),
                Op::Delete => ("delete", None),
            };
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon).color(tint)).on_hover_text(status);
                ui.add_sized(egui::vec2(56.0, ui.spacing().interact_size.y), egui::Label::new(what));
                if let Some(edit) = edit.as_deref_mut() {
                    resolutions_ui(ui, op, edit);
                }
                path_link(ui, ws, op.path.as_deref().unwrap_or(&op.uid), nav);
            });
            if let Some(to) = to {
                ui.horizontal(|ui| {
                    ui.add_space(88.0);
                    ui.label(RichText::new("to").small().color(theme::weak()));
                    path_link(ui, ws, to, nav);
                });
            }
        }
        if operations.len() > 500 {
            ui.label(RichText::new(format!("… {} more", operations.len() - 500)).color(theme::weak()));
        }
    });
}

/// What can be done about an operation, as small buttons: a stale override kept or given up, an operation whose
/// entry is gone dropped.
fn resolutions_ui(ui: &mut egui::Ui, op: &project::index::OpView, edit: &mut Option<EditRequest>) {
    use project::edit::Resolution;
    let mut ask = |ui: &mut egui::Ui, text: &str, hint: &str, resolution| {
        if ui.small_button(text).on_hover_text(hint).clicked() {
            *edit = Some(EditRequest::Resolve(op.uid.clone(), resolution));
        }
    };
    match (op.status, &op.op) {
        (OpStatus::Stale, _) => {
            ask(
                ui,
                "Keep mine",
                "Keep the layer's version: its base becomes the version below now",
                Resolution::KeepMine,
            );
            ask(ui, "Take below", "Drop the layer's version for the one below", Resolution::TakeBase);
        }
        (OpStatus::Unbound | OpStatus::Missing, _) => {
            ask(ui, "Drop", "Drop the operation: what it acted on is gone", Resolution::Drop);
        }
        (_, Op::Override { .. }) => {
            ask(ui, "Take below", "Drop the layer's version for the one below", Resolution::TakeBase);
        }
        (_, Op::Move { .. } | Op::Delete) => ask(ui, "Drop", "Undo this operation in the layer", Resolution::Drop),
    }
}

/// The entries a layer creates, a row each (the game layer's run to thousands, so only the rows in view are drawn).
fn creates_ui(ui: &mut egui::Ui, ws: &Workspace, layer: &str, nav: &mut Option<String>) {
    let creates = ws.index.creates(layer);
    theme::section(ui, &format!("Creates ({})", creates.len()), |ui| {
        if creates.is_empty() {
            ui.label(RichText::new("It creates nothing").color(theme::weak()));
            return;
        }
        let height = ui.text_style_height(&egui::TextStyle::Body) + ui.spacing().item_spacing.y;
        egui::ScrollArea::vertical().id_salt("creates").max_height(420.0).auto_shrink([false, true]).show_rows(
            ui,
            height,
            creates.len(),
            |ui, rows| {
                for create in &creates[rows] {
                    ui.horizontal(|ui| {
                        let (icon, tint) = match create.kind {
                            Some(Kind::Asset) => (icons::PACKAGE, theme::weak()),
                            Some(Kind::Folder) => (icons::FOLDER, theme::weak()),
                            Some(Kind::File) => (icons::FILE, theme::weak()),
                            None => (icons::X_CIRCLE, ui.visuals().error_fg_color),
                        };
                        ui.label(RichText::new(icon).color(tint)).on_hover_text(if create.kind.is_none() {
                            "missing from the layer"
                        } else {
                            &create.uid
                        });
                        path_link(ui, ws, &create.path, nav);
                    });
                }
            },
        );
    });
}

/// A resolved path, a link where it is an entry of the view.
fn path_link(ui: &mut egui::Ui, ws: &Workspace, real: &str, nav: &mut Option<String>) {
    let shown = real.strip_suffix(project::index::BUNDLE_SUFFIX).unwrap_or(real);
    let known = ws.index.assets.contains_key(shown) || ws.index.entries.contains_key(real);
    if known {
        let target = if ws.index.assets.contains_key(shown) { shown } else { real };
        if theme::short_link(ui, shown).clicked() {
            *nav = Some(target.to_string());
        }
    } else {
        theme::short(ui, RichText::new(shown).color(theme::weak()));
    }
}
