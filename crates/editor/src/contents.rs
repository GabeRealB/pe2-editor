//! The contents pane: the tree inside what is shown, as Blender's outliner. For an asset that is what its kind says, its parts
//! unless it says more, then its stack, its layers and a graph layer's operations. A kind has most of the pane: its rows' names,
//! icons and pictures, how they fold, what they offer and what comes of dragging them, views at the head of the pane for what rows
//! cannot say, and whether the stack shows at all. Selecting a row is selecting it everywhere (`Workspace::inner`).

use eframe::egui::{self, Color32, RichText};
use pe2_ext::{ContentAction, ContentPlace, ContentRequest, ContentRow, ContentStyle};
use project::graph;
use project::index::Asset;
use toml::Value;

use crate::dnd::{self, Dragged};
use crate::workspace::{EditRequest, Workspace};
use crate::{icons, theme};

/// How far a row within another is set in.
const INDENT: f32 = 14.0;
/// What a row's arrow takes, or leaves where it has none, so that names at a depth line up.
const ARROW: f32 = 16.0;
/// A button on a row.
const ROW_BUTTON: f32 = 24.0;
/// A row's picture, at most this high and twice as wide.
const PICTURE: f32 = 16.0;

/// A row as it is drawn.
struct Line<'a> {
    depth: u32,
    /// Whether it is open, where it folds.
    fold: Option<bool>,
    selected: bool,
    style: ContentStyle,
    picture: Option<(egui::TextureId, egui::Vec2)>,
    icon: &'a str,
    tint: Color32,
    label: &'a str,
    detail: &'a str,
    dragged: bool,
    /// Where it ends: the pane's edge, less its buttons. Told, not measured from what is left: in a pane that scrolls both ways
    /// what is left grows with what is drawn, and each row would be wider than the last.
    right: f32,
}

impl Line<'_> {
    /// A plain row: a layer of the stack.
    fn plain<'a>(right: f32, depth: u32, selected: bool, icon: &'a str, label: &'a str, detail: &'a str) -> Line<'a> {
        Line {
            depth,
            fold: None,
            selected,
            style: ContentStyle::Item,
            picture: None,
            icon,
            tint: theme::weak(),
            label,
            detail,
            dragged: false,
            right,
        }
    }
}

/// Draw a row in a horizontal layout: its response, and whether its arrow was clicked.
fn line(ui: &mut egui::Ui, line: &Line) -> (egui::Response, bool) {
    #[expect(clippy::cast_precision_loss, reason = "a tree's depth")]
    ui.add_space(INDENT * line.depth as f32);
    let toggled = line.fold.is_some_and(|open| {
        let icon = if open { icons::CARET_DOWN } else { icons::CARET_RIGHT };
        let arrow = egui::Button::new(RichText::new(icon).small().color(theme::weak())).frame(false);
        ui.add_sized([ARROW, ui.spacing().interact_size.y], arrow).clicked()
    });
    if line.fold.is_none() {
        ui.add_space(ARROW);
    }
    let strength = if line.style == ContentStyle::Dim { 0.5 } else { 1.0 };
    if let Some((texture, size)) = line.picture {
        // In a slot of one size, so that the names beside pictures of any shape line up.
        let (slot, _) = ui.allocate_exact_size(egui::vec2(PICTURE * 2.0, PICTURE), egui::Sense::hover());
        let shown = size * (PICTURE / size.y.max(1.0)).min(slot.width() / size.x.max(1.0));
        let shown = egui::vec2(shown.x, shown.y.max(PICTURE / 2.0));
        let place = egui::Rect::from_center_size(slot.center(), shown);
        egui::Image::new(egui::load::SizedTexture::new(texture, shown)).tint(Color32::WHITE.gamma_multiply(strength)).paint_at(ui, place);
    }
    let body = |color| egui::TextFormat { font_id: egui::TextStyle::Body.resolve(ui.style()), color, ..Default::default() };
    let small = |color| egui::TextFormat { font_id: egui::TextStyle::Small.resolve(ui.style()), color, ..Default::default() };
    let mut text = egui::text::LayoutJob::default();
    let heading = line.style == ContentStyle::Heading;
    if heading {
        text.append(&line.label.to_uppercase(), 0.0, small(theme::weak()));
    } else {
        if line.picture.is_none() {
            text.append(line.icon, 0.0, body(line.tint.gamma_multiply(strength)));
        }
        text.append(line.label, if line.picture.is_none() { 6.0 } else { 0.0 }, body(theme::text().gamma_multiply(strength)));
        text.append(line.detail, 6.0, small(theme::weak().gamma_multiply(strength)));
    }
    let button = if heading { egui::Button::new(()).frame(false) } else { egui::Button::selectable(line.selected, ()) };
    let sense = if line.dragged { egui::Sense::click_and_drag() } else { egui::Sense::click() };
    let width = (line.right - ui.cursor().min.x).max(0.0);
    // On the left of a row as wide as there is: all of it can be clicked. No wider, or its buttons go off the pane.
    let row = button.left_text(text).truncate().min_size(egui::vec2(width, 0.0)).sense(sense);
    let size = egui::vec2(width, ui.spacing().interact_size.y);
    let layout = *ui.layout();
    (ui.allocate_ui_with_layout(size, layout, |ui| ui.add(row)).inner, toggled)
}

/// The icon of an action; its label where it has none.
fn action_text(action: &ContentAction) -> String {
    action.icon.map_or_else(|| action.label.clone(), |icon| icons::glyph(icon).to_string())
}

/// A row's own buttons, on its right.
fn row_buttons(ui: &mut egui::Ui, row: &ContentRow, editable: bool, ask: &mut Option<ContentRequest>) {
    for action in row.actions.iter().filter(|action| action.on_row) {
        let button = egui::Button::selectable(action.on, action_text(action));
        let size = egui::vec2(ROW_BUTTON, ui.spacing().interact_size.y);
        if ui.add_enabled_ui(editable, |ui| ui.add_sized(size, button)).inner.on_hover_text(&action.label).clicked() {
            *ask = Some(ContentRequest::Action { row: row.id.clone(), action: action.id.clone() });
        }
    }
}

/// A row's menu: renaming it, and what its kind offers there.
fn row_menu(ui: &mut egui::Ui, row: &ContentRow, rename: &mut bool, ask: &mut Option<ContentRequest>) {
    if row.rename && ui.button("Rename").clicked() {
        *rename = true;
        ui.close();
    }
    for action in row.actions.iter().filter(|action| !action.on_row) {
        let text = action.icon.map_or_else(|| action.label.clone(), |icon| format!("{}  {}", icons::glyph(icon), action.label));
        if ui.button(text).clicked() {
            *ask = Some(ContentRequest::Action { row: row.id.clone(), action: action.id.clone() });
            ui.close();
        }
    }
}

/// What a drop on a row would do, for the place of it the pointer is over.
fn says(row: &ContentRow, of: &str, dragged: &Dragged, place: ContentPlace, index: &project::index::Index) -> Result<String, String> {
    if let Dragged::Asset(path) = dragged {
        let kind = index.assets.get(path).and_then(|asset| asset.kind.as_deref()).map(|kind| kind.rsplit('@').next().unwrap_or(kind));
        let taken = kind.is_some_and(|kind| row.takes.iter().any(|takes| takes.strip_prefix("asset:") == Some(kind)));
        return if taken { Ok(format!("Attach it to {}", row.label)) } else { Err(format!("{} does not take it", row.label)) };
    }
    let Dragged::Row { asset, id, what, .. } = dragged else { return Err("Only what is inside it goes here".to_string()) };
    if asset != of {
        return Err("It stays in its asset".to_string());
    }
    if *id == row.id {
        return Err("It is here".to_string());
    }
    match place {
        ContentPlace::Inside if row.takes.contains(what) => Ok(format!("Put it in {}", row.label)),
        ContentPlace::Inside => Err(format!("{} does not take it", row.label)),
        _ if row.drag.as_ref().is_none_or(|drag| drag.what != *what) => Err("It goes among its own".to_string()),
        ContentPlace::Before => Ok(format!("Put it before {}", row.label)),
        ContentPlace::After => Ok(format!("Put it after {}", row.label)),
    }
}

/// A row as a place to drop on: its top and foot where it reorders, the rest where it takes something.
fn row_target(ui: &egui::Ui, rect: egui::Rect, row: &ContentRow, of: &str, index: &project::index::Index, ask: &mut Option<ContentRequest>) {
    let (reorders, holds) = (row.drag.as_ref().is_some_and(|drag| drag.reorders), !row.takes.is_empty());
    let edge = rect.height() / if holds { 3.0 } else { 2.0 };
    let top = egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x, rect.min.y + edge));
    let foot = egui::Rect::from_min_max(egui::pos2(rect.min.x, rect.max.y - edge), rect.max);
    let middle = egui::Rect::from_min_max(egui::pos2(rect.min.x, top.max.y), egui::pos2(rect.max.x, foot.min.y));
    let places: Vec<(egui::Rect, ContentPlace)> = match (reorders, holds) {
        (true, true) => vec![(top, ContentPlace::Before), (middle, ContentPlace::Inside), (foot, ContentPlace::After)],
        (true, false) => vec![(top, ContentPlace::Before), (foot, ContentPlace::After)],
        (false, true) => vec![(rect, ContentPlace::Inside)],
        (false, false) => vec![],
    };
    for (rect, place) in places {
        match dnd::target(ui, rect, |dragged| says(row, of, dragged, place, index)) {
            Some(Dragged::Row { id, .. }) => *ask = Some(ContentRequest::Moved { row: id, onto: row.id.clone(), place }),
            Some(Dragged::Asset(asset)) => *ask = Some(ContentRequest::Dropped { row: row.id.clone(), asset }),
            _ => {}
        }
    }
}

/// A row's picture, once uploaded: one new one a frame, the rest following, so that an atlas of hundreds does not stall a frame.
fn picture_of(ws: &mut Workspace, ui: &egui::Ui, asset: &Asset, id: &str, fresh: &mut bool) -> Option<(egui::TextureId, egui::Vec2)> {
    if !ws.parts.has(asset, id) {
        if !*fresh {
            ui.ctx().request_repaint();
            return None;
        }
        *fresh = false;
    }
    match ws.parts.get(ui.ctx(), &ws.extensions, asset, id) {
        Some(Ok(picture)) => Some((picture.texture.id(), picture.size)),
        _ => None,
    }
}

/// A row being renamed: Enter, or a click elsewhere, commits it, Escape leaves it.
fn rename_ui(ui: &mut egui::Ui, ws: &mut Workspace, row: &ContentRow, ask: &mut Option<ContentRequest>) {
    let Some((_, _, typed)) = ws.content_rename.as_mut() else { return };
    let mut output = egui::TextEdit::singleline(typed).desired_width(ui.available_width()).show(ui);
    let response = output.response.response.clone();
    // The whole name at first: what is typed replaces it.
    if !response.has_focus() {
        let whole = egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(typed.chars().count()));
        output.state.cursor.set_char_range(Some(whole));
        output.state.store(ui.ctx(), response.id);
    }
    response.request_focus();
    let (enter, escape) = ui.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
    if escape {
        ws.content_rename = None;
    } else if enter || response.clicked_elsewhere() {
        let name = typed.trim().to_string();
        ws.content_rename = None;
        if name != row.label {
            *ask = Some(ContentRequest::Rename { row: row.id.clone(), name });
        }
    }
}

/// The rows a kind says are inside its asset, and what was asked of them.
fn rows_ui(
    ui: &mut egui::Ui,
    ws: &mut Workspace,
    asset: &Asset,
    (rows, right, editable): (&[ContentRow], f32, bool),
    nav: &mut Option<String>,
) -> Option<ContentRequest> {
    let selected = ws.inner_of(&asset.path).map(str::to_string);
    let mut ask = None;
    let mut chosen = None;
    let mut folded_below = None;
    let mut fresh = true;
    for (at, row) in rows.iter().enumerate() {
        if folded_below.is_some_and(|depth| row.depth > depth) {
            continue;
        }
        folded_below = None;
        let folds = rows.get(at + 1).is_some_and(|next| next.depth > row.depth);
        let key = (asset.path.clone(), row.id.clone());
        let open = ws.content_open.get(&key).copied().unwrap_or(row.open);
        if folds && !open {
            folded_below = Some(row.depth);
        }
        let picture = row.picture.then(|| picture_of(ws, ui, asset, &row.id, &mut fresh)).flatten();
        let inner = format!("#{}", row.id);
        let (icon, tint) = row.icon.map_or((icons::PACKAGE, theme::weak()), |(icon, tint)| (icons::glyph(icon), icons::tint(tint)));
        let on_row = row.actions.iter().filter(|action| action.on_row).count();
        #[expect(clippy::cast_precision_loss, reason = "a row's few buttons")]
        let room = (ROW_BUTTON + ui.spacing().item_spacing.x) * on_row as f32;
        let drawn = Line {
            depth: row.depth,
            fold: folds.then_some(open),
            selected: selected.as_deref() == Some(&inner),
            style: row.style,
            picture,
            icon,
            tint,
            label: &row.label,
            detail: &row.detail,
            dragged: editable && row.drag.is_some(),
            right: right - room,
        };
        let renaming = ws.content_rename.as_ref().is_some_and(|(of, id, _)| *of == asset.path && *id == row.id);
        let mut rename = false;
        let inner_row = ui.horizontal(|ui| {
            if renaming {
                rename_ui(ui, ws, row, &mut ask);
                return;
            }
            let (response, toggled) = line(ui, &drawn);
            row_buttons(ui, row, editable, &mut ask);
            if toggled || (row.is_heading() && folds && response.clicked()) {
                ws.content_open.insert(key.clone(), !open);
            }
            if !row.is_heading() && response.clicked() {
                chosen = Some((inner.clone(), row.look));
            }
            if let Some(link) = row.link.as_ref().filter(|_| response.double_clicked()) {
                *nav = Some(link.clone());
            }
            if let Some(drag) = row.drag.as_ref().filter(|_| drawn.dragged) {
                response.dnd_set_drag_payload(Dragged::Row {
                    asset: asset.path.clone(),
                    id: row.id.clone(),
                    label: row.label.clone(),
                    what: drag.what.clone(),
                });
            }
            if editable && (row.rename || row.actions.iter().any(|action| !action.on_row)) {
                response.context_menu(|ui| row_menu(ui, row, &mut rename, &mut ask));
            }
            if !row.hint.is_empty() {
                drop(response.on_hover_text(&row.hint));
            }
        });
        if rename {
            ws.content_rename = Some((asset.path.clone(), row.id.clone(), row.label.clone()));
        }
        if editable {
            row_target(ui, inner_row.response.rect, row, &asset.path, &ws.index, &mut ask);
        }
    }
    if let Some((inner, look)) = chosen {
        let selecting = selected.as_deref() != Some(&inner);
        ws.inner = selecting.then(|| (asset.path.clone(), inner));
        if let Some(look) = look.filter(|_| selecting) {
            ws.variant_choice.insert(asset.path.clone(), look as usize);
        }
    }
    ask
}

/// What a layer or operation is called: its operation's label, and where the host has none of its own, the name it has.
fn label_of(ws: &Workspace, name: &str) -> String {
    if name == graph::GRAPH {
        return "Graph".to_string();
    }
    ws.extensions.operations().get(name).map_or_else(|| name.to_string(), |op| op.label.clone())
}

/// What tells a layer from others of its operation.
fn detail_of(layer: &Value) -> &str {
    ["name", "file"].iter().find_map(|key| layer.get(*key)?.as_str()).unwrap_or_default()
}

/// A stack, its layers and a graph layer's operations.
fn stack_ui(ui: &mut egui::Ui, ws: &mut Workspace, asset: &Asset, layers: &[Value], right: f32) {
    let selected = ws.inner_of(&asset.path).map(str::to_string);
    let mut chosen = None;
    let mut pick = |clicked: bool, inner: String| {
        if clicked {
            chosen = Some(inner);
        }
    };
    let op_of = |layer: &Value| layer.get("op").and_then(Value::as_str).unwrap_or_default().to_string();
    for (at, layer) in layers.iter().enumerate() {
        let inner = format!("stack.{at}");
        let name = op_of(layer);
        let icon = if name == graph::GRAPH { icons::TREE_STRUCTURE } else { icons::STACK_SIMPLE };
        let label = label_of(ws, &name);
        let row = Line::plain(right, 0, selected.as_deref() == Some(&inner), icon, &label, detail_of(layer));
        let clicked = ui.horizontal(|ui| line(ui, &row).0.clicked()).inner;
        pick(clicked, inner.clone());
        let Some(ops) = layer.get("ops").and_then(Value::as_table) else { continue };
        for (id, op) in ops {
            let inner = format!("{inner}.ops.{id}");
            let label = label_of(ws, &op_of(op));
            let row = Line::plain(right, 1, selected.as_deref() == Some(&inner), icons::PACKAGE, id, &label);
            let clicked = ui.horizontal(|ui| line(ui, &row).0.clicked()).inner;
            pick(clicked, inner);
        }
    }
    if let Some(inner) = chosen {
        ws.inner = (selected.as_deref() != Some(&inner)).then(|| (asset.path.clone(), inner));
    }
}

pub fn contents_ui(
    ui: &mut egui::Ui,
    ws: &mut Workspace,
    subject: Option<&str>,
    nav: &mut Option<String>,
    message: &mut Option<String>,
    edit: &mut Option<EditRequest>,
) {
    let Some(asset) = subject.and_then(|s| ws.index.assets.get(s)).cloned() else {
        ui.label(RichText::new("Select an asset").color(theme::weak()));
        return;
    };
    ws.show_contents_views(ui, &asset.path, nav);
    let contents = ws.contents_of(&asset);
    if let Some(problem) = &contents.problem {
        ui.colored_label(ui.visuals().error_fg_color, "Its extension could not say what is inside it").on_hover_text(problem);
    }
    let layers = asset.fields().get("stack").and_then(Value::as_array).filter(|_| contents.stack).map_or(&[][..], Vec::as_slice);
    let has_views = !ws.extensions.contents_views(&asset).is_empty();
    if layers.is_empty() && contents.rows.is_empty() && contents.actions.is_empty() {
        if !has_views && contents.problem.is_none() {
            ui.label(RichText::new("Nothing inside").color(theme::weak()));
        }
        return;
    }
    let editable = ws.editable_layer().is_ok();
    // The edge of what is shown, whatever is drawn after this.
    let right = ui.max_rect().right().min(ui.clip_rect().right()) - ui.spacing().item_spacing.x;
    let mut ask = None;
    if !contents.actions.is_empty() {
        ui.horizontal_wrapped(|ui| {
            for action in &contents.actions {
                let text = action.icon.map_or_else(|| action.label.clone(), |icon| format!("{}  {}", icons::glyph(icon), action.label));
                if ui.add_enabled(editable, egui::Button::selectable(action.on, text)).clicked() {
                    ask = Some(ContentRequest::Action { row: String::new(), action: action.id.clone() });
                }
            }
        });
    }
    let asked = rows_ui(ui, ws, &asset, (&contents.rows, right, editable), nav);
    if (!contents.rows.is_empty() || has_views) && !layers.is_empty() {
        ui.horizontal(|ui| {
            let heading = Line { style: ContentStyle::Heading, ..Line::plain(right, 0, false, "", "Stack", "") };
            line(ui, &heading);
        });
    }
    stack_ui(ui, ws, &asset, layers, right);
    if let Some(request) = asked.or(ask) {
        match ws.extensions.content_act(&ws.index, &asset, &ws.chosen, request) {
            Ok(outcome) => match crate::inspector::content_edit(&asset, &outcome) {
                Ok(request) => {
                    if let Some(select) = &outcome.select {
                        ws.inner = Some((asset.path.clone(), format!("#{select}")));
                    }
                    *edit = request;
                }
                Err(why) => *message = Some(why),
            },
            Err(why) => *message = Some(format!("{why:#}")),
        }
    }
}
