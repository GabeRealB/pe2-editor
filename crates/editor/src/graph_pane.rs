//! The graph editor: a graph layer of a stack, a document's way of showing it. An operation is a box, its input ports down its left
//! and what it makes at its foot on the right; what comes down from above is a box at the left, and what the layer passes
//! on down one at the right. Ports and wires are colored by what they carry. Dragging from a port to another makes a
//! wire, refused where the types do not fit, ports lit while one is being drawn; dragging a wire off an input moves or
//! removes it. Right-clicking the canvas adds an operation, a box is dragged to place it and Delete removes the one
//! selected. A box selected is what is selected inside the asset, its settings in the inspector with everything else
//! selected there. Every edit is written as any is: undoable, unapplied until applied, and what is wrong with the stack as
//! written is told from its shape, under the canvas.
//!
//! The canvas is a scene: the wheel zooms, dragging where nothing is pans and a double-click fits, as in the preview. A
//! box not yet placed sits by how far it is from its sources, as Arrange puts them all.

use eframe::egui::{self, Color32, Pos2, Rect, RichText, Sense, Vec2, pos2, vec2};
use pe2_ext::{Port, Type};
use project::graph::{self, ABOVE, layers};
use project::index::Asset;
use std::collections::HashMap;
use toml::Value;

use crate::inspector;
use crate::workspace::{EditRequest, GraphAsk, Workspace};
use crate::theme;

const WIDTH: f32 = 172.0;
/// The boxes at either end, which say little.
const END_WIDTH: f32 = 128.0;
const HEADER: f32 = 28.0;
const ROW: f32 = 22.0;
/// Between columns and between boxes of one.
const GAP: Vec2 = vec2(52.0, 24.0);
const PORT: f32 = 18.0;
/// Around what is shown when it is fit.
const MARGIN: f32 = 20.0;

/// What the pane keeps between frames.
#[derive(Clone, Default)]
struct State {
    /// What of the canvas is in view; none, it is fit to what there is.
    view: Option<Rect>,
    selected: Option<String>,
    /// A wire being drawn: what it comes from, and the input it was pulled off.
    wire: Option<(String, Option<(String, String)>)>,
    /// A box being dragged, and where it is.
    moving: Option<(String, Pos2)>,
    /// Where on the canvas the add menu was opened.
    adding: Pos2,
    filter: String,
    /// Why the last wire was refused.
    refused: Option<String>,
}

/// An operation of the layer, as it is drawn.
struct Node {
    id: String,
    label: String,
    at: Pos2,
    placed: bool,
    ports: Vec<Port>,
    made: Option<Type>,
    /// What each port takes from, by its place.
    wired: Vec<Vec<String>>,
}

/// A count of rows or columns, in points.
#[expect(clippy::cast_precision_loss, reason = "a handful of them")]
fn rows(count: usize) -> f32 {
    count as f32
}

impl Node {
    fn size(&self) -> Vec2 {
        vec2(WIDTH, HEADER + ROW * rows(self.ports.len() + 1) + 6.0)
    }

    fn rect(&self) -> Rect {
        Rect::from_min_size(self.at, self.size())
    }

    fn input(&self, port: usize) -> Pos2 {
        self.at + vec2(0.0, HEADER + ROW * (rows(port) + 0.5))
    }

    fn output(&self) -> Pos2 {
        self.at + vec2(WIDTH, HEADER + ROW * (rows(self.ports.len()) + 0.5))
    }
}

/// Whether an asset's stack has a graph layer.
pub fn has_graph(asset: &Asset) -> bool {
    let layers = asset.fields().get("stack").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    layers.iter().any(|layer| layer.get("op").and_then(Value::as_str) == Some(graph::GRAPH))
}

/// The graph layer a pane shows of an asset: the one selected, else its first.
fn layer_of(ws: &Workspace, asset: &Asset) -> Option<usize> {
    let layers = asset.fields().get("stack")?.as_array()?;
    let is_graph = |at: usize| layers.get(at).and_then(|l| l.get("op")?.as_str()) == Some(graph::GRAPH);
    let chosen = ws.inner_of(&asset.path).and_then(inspector::layer_of_inner).map(|(at, _)| at);
    chosen.filter(|at| is_graph(*at)).or_else(|| (0..layers.len()).find(|at| is_graph(*at)))
}

/// What a port of a layer's operation takes from: an id, or several.
fn sources(from: Option<&Value>) -> Vec<String> {
    match from {
        Some(Value::String(id)) => vec![id.clone()],
        Some(Value::Array(ids)) => ids.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        _ => Vec::new(),
    }
}

/// Where each box goes, by its id: in columns by how far it is from what has no source here, each column ordered by
/// where its sources are, so wires cross little, and spaced so no box touches another.
fn arrangement(nodes: &[Node]) -> HashMap<String, Pos2> {
    let mut depth: HashMap<&str, usize> = nodes.iter().map(|n| (n.id.as_str(), 0)).collect();
    for _ in 0..nodes.len() {
        for node in nodes {
            let deepest = node.wired.iter().flatten().filter_map(|s| depth.get(s.as_str())).max().map(|d| d + 1);
            if let Some(deepest) = deepest {
                depth.insert(&node.id, deepest);
            }
        }
    }
    let mut places: HashMap<String, Pos2> = HashMap::new();
    let columns = depth.values().max().map_or(0, |d| d + 1);
    for column in 0..columns {
        let mut held: Vec<(&Node, f32)> = nodes
            .iter()
            .filter(|n| depth[n.id.as_str()] == column)
            .map(|node| {
                let heights: Vec<f32> = node.wired.iter().flatten().filter_map(|s| places.get(s)).map(|p| p.y).collect();
                (node, heights.iter().sum::<f32>() / rows(heights.len().max(1)))
            })
            .collect();
        held.sort_by(|a, b| a.1.total_cmp(&b.1));
        let mut y = 0.0;
        for (node, _) in held {
            places.insert(node.id.clone(), pos2(rows(column) * (WIDTH + GAP.x), y));
            y += node.size().y + GAP.y;
        }
    }
    places
}

/// A graph layer's operations: those not placed where they would be arranged.
fn nodes_of(ws: &Workspace, layer: &Value, plan: &graph::Plan, at: usize) -> Vec<Node> {
    let ops = ws.extensions.operations();
    let table = layer.get("ops").and_then(Value::as_table);
    let mut nodes: Vec<Node> = table
        .into_iter()
        .flatten()
        .map(|(id, op)| {
            let name = op.get("op").and_then(Value::as_str).unwrap_or_default().to_string();
            let def = ops.get(&name);
            let ports = def.map_or_default(|d| d.inputs.clone());
            let from = op.get("from");
            let wired = ports.iter().map(|p| sources(from.and_then(|f| f.get(&p.name)))).collect();
            let here = format!("stack[{at}].ops.{id}");
            let made = plan.steps.iter().find(|s| s.at == here).map(|s| s.ty.clone()).or_else(|| def.and_then(|d| d.output.clone()));
            let place = op.get(layers::POS).and_then(Value::as_array);
            let number = |i: usize| place.and_then(|p| p.get(i)?.as_float());
            #[expect(clippy::cast_possible_truncation, reason = "a place on a canvas")]
            let placed = number(0).zip(number(1)).map(|(x, y)| pos2(x as f32, y as f32));
            let label = def.map_or_else(|| name.clone(), |d| d.label.clone());
            Node { id: id.clone(), label, at: placed.unwrap_or_default(), placed: placed.is_some(), ports, made, wired }
        })
        .collect();
    let arranged = arrangement(&nodes);
    for node in nodes.iter_mut().filter(|n| !n.placed) {
        node.at = arranged.get(&node.id).copied().unwrap_or_default();
    }
    nodes
}

/// A type as a box has room for it: without the extensions it comes from (`pe2.images@texels` is `texels`).
fn short(ty: &str) -> String {
    ty.split_inclusive(|c: char| " <>|".contains(c)).map(|word| word.rsplit_once('@').map_or(word, |(_, local)| local)).collect()
}

/// What a value of a type is drawn in, so what can be wired together looks it.
fn type_color(ty: Option<&Type>) -> Color32 {
    match ty {
        Some(Type::File(of)) => type_color(Some(of)),
        Some(Type::Stream(_) | Type::Video | Type::Audio | Type::List(_)) => Color32::from_rgb(178, 132, 226),
        Some(Type::Image) => Color32::from_rgb(86, 196, 168),
        Some(Type::Table | Type::Marks) => Color32::from_rgb(224, 172, 86),
        Some(Type::Named(_) | Type::Union(_) | Type::Bytes) => Color32::from_rgb(98, 160, 232),
        _ => theme::weak(),
    }
}

/// A wire, as a curve leaving its source and coming into its port level.
fn wire(painter: &egui::Painter, from: Pos2, to: Pos2, color: Color32) {
    let reach = ((to.x - from.x).abs() * 0.5).max(40.0);
    let points = [from, from + vec2(reach, 0.0), to - vec2(reach, 0.0), to];
    painter.add(egui::epaint::CubicBezierShape::from_points_stroke(points, false, Color32::TRANSPARENT, egui::Stroke::new(2.0, color)));
}

/// Text cut to fit, set from a point.
fn text_at(painter: &egui::Painter, at: Pos2, align: egui::Align2, text: &str, font: egui::FontId, color: Color32) {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_string(), font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(WIDTH - 24.0);
    let galley = painter.layout_job(job);
    let place = align.anchor_size(at, galley.size()).min;
    painter.galley(place, galley, color);
}

/// A box: its frame and its title on a band of what it makes.
fn box_at(painter: &egui::Painter, rect: Rect, title: &str, tint: Color32, (selected, wrong): (bool, bool), ui: &egui::Ui) {
    let edge = if wrong { ui.visuals().error_fg_color } else if selected { theme::accent() } else { theme::border() };
    let stroke = egui::Stroke::new(if selected || wrong { 2.0 } else { 1.0 }, edge);
    painter.rect_filled(rect, 7.0, ui.visuals().extreme_bg_color);
    let band = Rect::from_min_size(rect.min, vec2(rect.width(), HEADER));
    painter.rect_filled(band, egui::CornerRadius { nw: 7, ne: 7, sw: 0, se: 0 }, tint.gamma_multiply(0.22));
    painter.rect_stroke(rect, 7.0, stroke, egui::StrokeKind::Inside);
    let font = egui::TextStyle::Body.resolve(ui.style());
    text_at(painter, rect.min + vec2(12.0, HEADER * 0.5), egui::Align2::LEFT_CENTER, title, font, theme::text());
}

/// A port: a dot of what it carries, lit where a wire being drawn may end.
fn port_dot(painter: &egui::Painter, at: Pos2, color: Color32, lit: bool) {
    let ring = egui::Stroke::new(if lit { 2.0 } else { 1.0 }, if lit { theme::text() } else { theme::border() });
    painter.circle(at, if lit { 6.5 } else { 5.0 }, color, ring);
}

/// A port's widget: dragged, a wire; named for whoever reads the window.
fn port_response(ui: &egui::Ui, salt: (&str, &str, &str), at: Pos2, says: &str) -> egui::Response {
    let rect = Rect::from_center_size(at, Vec2::splat(PORT));
    let response = ui.interact(rect, egui::Id::unique(salt), Sense::drag());
    let label = says.to_string();
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &label));
    response.on_hover_text(says)
}

/// What the add menu offers, narrowed by what is typed: choosing one adds it where the menu was opened.
fn add_menu(ui: &mut egui::Ui, ws: &Workspace, asset: &Asset, (at, layer): (usize, &Value), state: &mut State, edit: &mut Option<EditRequest>) {
    let response = ui.add(egui::TextEdit::singleline(&mut state.filter).hint_text("Filter operations").desired_width(220.0));
    response.request_focus();
    let filter = state.filter.to_lowercase();
    let taken: Vec<&str> = layer.get("ops").and_then(Value::as_table).into_iter().flatten().map(|(id, _)| id.as_str()).collect();
    egui::ScrollArea::vertical().max_height(280.0).show(ui, |ui| {
        for (op, label) in ws.extensions.graph_operations() {
            if !label.to_lowercase().contains(&filter) && !op.to_lowercase().contains(&filter) {
                continue;
            }
            if ui.button(&label).on_hover_text(&op).clicked() {
                let id = layers::free_id(&taken, &op);
                let mut table = toml::Table::new();
                table.insert("op".into(), op.as_str().into());
                // What it cannot do without, for the sheet to change.
                match op.as_str() {
                    graph::FILE => {
                        let stored = asset.files.iter().find(|f| f.made_by.is_none() && f.name != project::index::ASSET_FILE);
                        table.insert("file".into(), stored.map_or("", |f| f.name.as_str()).into());
                    }
                    graph::OUTPUT => {
                        table.insert("name".into(), id.as_str().into());
                    }
                    _ => {}
                }
                let place = [f64::from(state.adding.x), f64::from(state.adding.y)];
                *edit = inspector::layers_edit(asset, format!("Add {label}"), layers::add_operation(asset.text(), at, &id, table, place));
                state.selected = Some(id);
                state.filter.clear();
                ui.close();
            }
        }
    });
}

/// The boxes standing for what comes from above and what goes on down: their places, clear of the operations.
fn ends(nodes: &[Node]) -> (Pos2, Pos2) {
    let top = nodes.iter().map(|n| n.at.y).fold(f32::INFINITY, f32::min);
    let top = if top.is_finite() { top } else { 0.0 };
    let left = nodes.iter().map(|n| n.at.x).fold(f32::INFINITY, f32::min);
    let left = if left.is_finite() { left } else { 0.0 };
    let right = nodes.iter().map(|n| n.at.x + WIDTH).fold(f32::NEG_INFINITY, f32::max).max(left + WIDTH);
    (pos2(left - END_WIDTH - GAP.x, top), pos2(right + GAP.x, top))
}

/// An end's box.
fn end_rect(at: Pos2) -> Rect {
    Rect::from_min_size(at, vec2(END_WIDTH, HEADER + ROW + 6.0))
}

/// What is on the canvas, all of it: what fitting shows.
fn extent(nodes: &[Node], from_above: bool) -> Rect {
    let (above, below) = ends(nodes);
    let all = nodes.iter().map(Node::rect).chain([end_rect(below)]).chain(from_above.then(|| end_rect(above)));
    all.fold(Rect::NOTHING, Rect::union).expand(MARGIN)
}

/// A canvas's contents, with what they need of the pane.
struct Canvas<'a> {
    ws: &'a Workspace,
    asset: &'a Asset,
    at: usize,
    layer: &'a Value,
    nodes: Vec<Node>,
    above: Option<Type>,
    wrong: Vec<graph::Problem>,
}

impl Canvas<'_> {
    fn edit(&self, label: String, text: anyhow::Result<String>) -> Option<EditRequest> {
        inspector::layers_edit(self.asset, label, text)
    }

    /// Dots under everything, for the eye to tell panning and zoom by.
    fn dots(ui: &egui::Ui) {
        let (view, painter) = (ui.clip_rect(), ui.painter());
        let color = theme::border().gamma_multiply(0.7);
        let step = if view.width() > 2400.0 { 96.0 } else { 32.0 };
        let (mut x, top) = ((view.left() / step).floor() * step, (view.top() / step).floor() * step);
        while x < view.right() {
            let mut y = top;
            while y < view.bottom() {
                painter.circle_filled(pos2(x, y), 1.2, color);
                y += step;
            }
            x += step;
        }
    }

    /// The wires, under the boxes.
    fn wires(&self, ui: &egui::Ui, outs: &HashMap<&str, (Pos2, Option<Type>)>, into: Option<(Pos2, &str)>) {
        let painter = ui.painter();
        let bad = ui.visuals().error_fg_color;
        for node in &self.nodes {
            for (i, port) in node.ports.iter().enumerate() {
                for source in &node.wired[i] {
                    let Some((from, made)) = outs.get(source.as_str()) else { continue };
                    let fits = made.as_ref().is_none_or(|ty| port.ty.unify(ty, &mut Vec::new()));
                    wire(painter, *from, node.input(i), if fits { type_color(made.as_ref()) } else { bad });
                }
            }
        }
        if let Some((to, source)) = into
            && let Some((from, made)) = outs.get(source)
        {
            wire(painter, *from, to, type_color(made.as_ref()));
        }
    }

    /// A box and its ports: what dragging them does.
    fn operation(&self, ui: &egui::Ui, node: &Node, state: &mut State, drawing: Option<&Option<Type>>, edit: &mut Option<EditRequest>) {
        let painter = ui.painter();
        let small = egui::TextStyle::Small.resolve(ui.style());
        let response = ui.interact(node.rect(), egui::Id::unique(("graph box", &node.id)), Sense::click_and_drag());
        let says = format!("operation {}: {}", node.id, node.label);
        response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, state.selected.as_deref() == Some(&node.id), &says));
        if response.clicked() || response.drag_started() {
            state.selected = Some(node.id.clone());
        }
        if response.dragged() {
            let place = state.moving.as_ref().filter(|(id, _)| *id == node.id).map_or(node.at, |(_, place)| *place);
            state.moving = Some((node.id.clone(), place + response.drag_delta()));
        }
        if response.drag_stopped()
            && let Some((id, place)) = state.moving.take()
        {
            let to = [f64::from(place.x), f64::from(place.y)];
            *edit = self.edit(format!("Move {id}"), layers::place(self.asset.text(), self.at, &id, to));
        }
        let selected = state.selected.as_deref() == Some(&node.id);
        let here = format!("stack[{}].ops.{}", self.at, node.id);
        let wrong = self.wrong.iter().any(|p| p.at == here);
        box_at(painter, node.rect(), &node.label, type_color(node.made.as_ref()), (selected, wrong), ui);
        for (i, port) in node.ports.iter().enumerate() {
            let place = node.input(i);
            let lit = drawing.is_some_and(|made| made.as_ref().is_none_or(|ty| port.ty.unify(ty, &mut Vec::new())));
            port_dot(painter, place, type_color(Some(&port.ty)), lit);
            let text = format!("{}  ·  {}", port.name, short(&port.ty.to_string()));
            text_at(painter, place + vec2(12.0, 0.0), egui::Align2::LEFT_CENTER, &text, small.clone(), theme::weak());
            let says = format!("input {} of {}: takes {}", port.name, node.id, port.ty);
            let response = port_response(ui, ("graph in", &node.id, &port.name), place, &says);
            if response.drag_started()
                && let Some(source) = node.wired[i].last()
            {
                state.wire = Some((source.clone(), Some((node.id.clone(), port.name.clone()))));
            }
        }
        let place = node.output();
        port_dot(painter, place, type_color(node.made.as_ref()), false);
        let made = node.made.as_ref().map_or_else(|| "any".to_string(), ToString::to_string);
        text_at(painter, place - vec2(12.0, 0.0), egui::Align2::RIGHT_CENTER, &short(&made), small, theme::text());
        if port_response(ui, ("graph out", &node.id, ""), place, &format!("output of {}: makes {made}", node.id)).drag_started() {
            state.wire = Some((node.id.clone(), None));
        }
    }

    /// The two ends: what comes down from above, and what goes on down.
    fn ends_ui(&self, ui: &egui::Ui, (above, below): (Pos2, Pos2), passed: Option<&str>, state: &mut State) {
        let painter = ui.painter();
        let small = egui::TextStyle::Small.resolve(ui.style());
        if let Some(ty) = &self.above {
            box_at(painter, end_rect(above), "From above", type_color(Some(ty)), (false, false), ui);
            let place = above + vec2(END_WIDTH, HEADER + ROW * 0.5);
            let text = ty.to_string();
            text_at(painter, place - vec2(12.0, 0.0), egui::Align2::RIGHT_CENTER, &short(&text), small.clone(), theme::text());
            port_dot(painter, place, type_color(Some(ty)), false);
            if port_response(ui, ("graph out", ABOVE, ""), place, &format!("from above: {text}")).drag_started() {
                state.wire = Some((ABOVE.to_string(), None));
            }
        }
        box_at(painter, end_rect(below), "Passed on down", theme::weak(), (false, false), ui);
        let place = below + vec2(0.0, HEADER + ROW * 0.5);
        port_dot(painter, place, theme::weak(), state.wire.is_some());
        let response = port_response(ui, ("graph in", "result", ""), place, "passed on down");
        if response.drag_started()
            && let Some(source) = passed
        {
            state.wire = Some((source.to_string(), Some(("result".to_string(), String::new()))));
        }
    }

    /// A wire let go: where it ended says what it does.
    fn dropped(&self, at: Pos2, sink: Pos2, (source, off): (String, Option<(String, String)>), outs: &HashMap<&str, (Pos2, Option<Type>)>, state: &mut State) -> Option<EditRequest> {
        let (layer, mut text) = (self.at, Ok(self.asset.text().to_string()));
        let mut label = format!("Unplug {source}");
        let mut wrote = off.is_some();
        // What it is pulled off goes first: both one step.
        if let Some((to, port)) = &off {
            text = text.and_then(|t| if to == "result" { layers::set_result(&t, layer, None) } else { layers::disconnect(&t, layer, (to, port), &source) });
        }
        let port = self.nodes.iter().find_map(|node| (0..node.ports.len()).find(|i| node.input(*i).distance(at) <= PORT).map(|i| (node, &node.ports[i])));
        if sink.distance(at) <= PORT {
            text = text.and_then(|t| layers::set_result(&t, layer, Some(&source)));
            label = format!("Pass {source} on down");
            wrote = true;
        } else if let Some((node, port)) = port {
            let made = outs.get(source.as_str()).and_then(|(_, made)| made.as_ref());
            if made.is_none_or(|ty| port.ty.unify(ty, &mut Vec::new())) {
                text = text.and_then(|t| layers::connect(&t, layer, (&node.id, &port.name, port.many), &source));
                label = format!("Wire {source} to {}", node.id);
                wrote = true;
                state.refused = None;
            } else {
                state.refused = made.map(|ty| format!("{} of {} takes {}, and {source} makes {ty}", port.name, node.id, port.ty));
            }
        }
        wrote.then(|| self.edit(label, text)).flatten()
    }

    /// Everything on the canvas, in the scene's coordinates.
    fn contents(&self, ui: &egui::Ui, state: &mut State, edit: &mut Option<EditRequest>) {
        Self::dots(ui);
        let (above, below) = ends(&self.nodes);
        let source = above + vec2(END_WIDTH, HEADER + ROW * 0.5);
        let sink = below + vec2(0.0, HEADER + ROW * 0.5);
        let mut outs: HashMap<&str, (Pos2, Option<Type>)> = self.nodes.iter().map(|n| (n.id.as_str(), (n.output(), n.made.clone()))).collect();
        outs.insert(ABOVE, (source, self.above.clone()));
        let passed = self.layer.get("result").and_then(Value::as_str);
        self.wires(ui, &outs, passed.map(|p| (sink, p)));
        let drawing = state.wire.as_ref().and_then(|(source, _)| outs.get(source.as_str()).cloned());
        for node in &self.nodes {
            self.operation(ui, node, state, drawing.as_ref().map(|(_, made)| made), edit);
        }
        self.ends_ui(ui, (above, below), passed, state);
        // A wire being drawn, and where it ends, the pointer brought into the scene.
        let to_scene = ui.ctx().layer_transform_from_global(ui.layer_id()).unwrap_or_default();
        let pointer = ui.input(|i| i.pointer.latest_pos()).map(|p| to_scene.mul_pos(p));
        if let (Some((from, made)), Some(pointer)) = (&drawing, pointer) {
            wire(ui.painter(), *from, pointer, type_color(made.as_ref()));
        }
        if ui.input(|i| i.pointer.any_released())
            && let Some(wire) = state.wire.take()
            && let Some(pointer) = pointer
        {
            *edit = self.dropped(pointer, sink, wire, &outs, state).or(edit.take());
        }
    }
}

/// The canvas, its background the scene's: a click there selects nothing, a right-click adds, a double-click fits.
fn canvas_ui(ui: &mut egui::Ui, canvas: &Canvas<'_>, state: &mut State, edit: &mut Option<EditRequest>) {
    let mut view = state.view.unwrap_or_else(|| extent(&canvas.nodes, canvas.above.is_some()));
    let scene = egui::Scene::new().zoom_range(0.2..=1.0).scroll_zooms(true);
    let shown = scene.show(ui, &mut view, |ui| canvas.contents(ui, state, edit));
    state.view = Some(view);
    let background = shown.response;
    if background.clicked() {
        state.selected = None;
        state.refused = None;
    }
    if background.double_clicked() {
        state.view = None;
    }
    if background.secondary_clicked()
        && let Some(pointer) = background.interact_pointer_pos()
    {
        state.adding = pointer;
    }
    if inspector::edit_target(canvas.ws, canvas.asset).is_ok() {
        background.context_menu(|ui| add_menu(ui, canvas.ws, canvas.asset, (canvas.at, canvas.layer), state, edit));
    }
}

/// Over the canvas: which layer this is, fitting it and arranging it.
fn toolbar_ui(ui: &mut egui::Ui, canvas: &Canvas<'_>, state: &mut State, edit: &mut Option<EditRequest>) {
    ui.horizontal(|ui| {
        let name = canvas.asset.path.rsplit('/').next().unwrap_or(&canvas.asset.path);
        ui.label(name);
        ui.label(RichText::new(format!("layer {}", canvas.at)).color(theme::weak()));
        ui.separator();
        if ui.button("Fit").on_hover_text("Show the whole graph (or double-click the canvas)").clicked() {
            state.view = None;
        }
        let editable = inspector::edit_target(canvas.ws, canvas.asset).is_ok();
        if ui.add_enabled(editable, egui::Button::new("Arrange")).on_hover_text("Put the boxes in columns, by what feeds what").clicked() {
            arrange(canvas, state, edit);
        }
    });
}

/// Put the boxes in columns, by what feeds what.
fn arrange(canvas: &Canvas<'_>, state: &mut State, edit: &mut Option<EditRequest>) {
    let places = arrangement(&canvas.nodes);
    let mut text = Ok(canvas.asset.text().to_string());
    for (id, place) in &places {
        text = text.and_then(|t| layers::place(&t, canvas.at, id, [f64::from(place.x), f64::from(place.y)]));
    }
    *edit = canvas.edit(format!("Arrange layer {}", canvas.at), text);
    state.view = None;
}

/// The pane.
pub fn graph_ui(ui: &mut egui::Ui, ws: &mut Workspace, subject: Option<&str>, edit: &mut Option<EditRequest>) {
    let Some(asset) = subject.and_then(|s| ws.index.assets.get(s)).cloned() else {
        ui.label(RichText::new("Select an asset with a graph layer.").color(theme::weak()));
        return;
    };
    let Some(at) = layer_of(ws, &asset) else {
        ui.label(RichText::new("This asset's stack has no graph layer. Add one in the Inspector.").color(theme::weak()));
        return;
    };
    let Some(layer) = asset.fields().get("stack").and_then(Value::as_array).and_then(|l| l.get(at)).cloned() else { return };
    let id = egui::Id::unique(("graph pane", &asset.path, at));
    let mut state: State = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    // What is selected of it is the workspace's: every pane shows the same.
    state.selected = ws.inner_of(&asset.path).and_then(inspector::layer_of_inner).filter(|(of, _)| *of == at).and_then(|(_, op)| op.map(str::to_string));
    let before = state.selected.clone();
    let (plan, wrong) = ws.extensions.plan_written(&asset);
    let mut nodes = nodes_of(ws, &layer, &plan, at);
    if let Some((moving, place)) = &state.moving
        && let Some(node) = nodes.iter_mut().find(|n| n.id == *moving)
    {
        node.at = *place;
    }
    let above = plan.above.get(at).cloned().flatten();
    let ask = ws.graph_ask.take();
    let canvas = Canvas { ws, asset: &asset, at, layer: &layer, nodes, above, wrong };
    match ask {
        Some(GraphAsk::Fit) => state.view = None,
        Some(GraphAsk::Arrange) if inspector::edit_target(canvas.ws, canvas.asset).is_ok() => arrange(&canvas, &mut state, edit),
        _ => {}
    }
    egui::Panel::top(("graph tools", id)).show(ui, |ui| toolbar_ui(ui, &canvas, &mut state, edit));
    if !canvas.wrong.is_empty() || state.refused.is_some() {
        egui::Panel::bottom(("graph problems", id)).show(ui, |ui| {
            ui.add_space(2.0);
            if let Some(why) = &state.refused {
                ui.label(RichText::new(why).small().color(ui.visuals().error_fg_color));
            }
            inspector::problems_ui(ui, &canvas.wrong);
        });
    }
    egui::CentralPanel::no_frame().show(ui, |ui| canvas_ui(ui, &canvas, &mut state, edit));
    if state.selected != before {
        let inner = state.selected.as_ref().map_or_else(|| format!("stack.{at}"), |op| format!("stack.{at}.ops.{op}"));
        ws.inner = Some((asset.path.clone(), inner));
    }
    ui.data_mut(|d| d.insert_temp(id, state));
}

#[cfg(test)]
mod tests {
    use super::{Node, arrangement, short};
    use eframe::egui::Pos2;
    use pe2_ext::{Port, Type};

    fn node(id: &str, wired: &[&str]) -> Node {
        let port = |i: usize| Port { name: format!("p{i}"), ty: Type::Any, many: false };
        Node {
            id: id.to_string(),
            label: id.to_string(),
            at: Pos2::ZERO,
            placed: false,
            ports: (0..wired.len()).map(port).collect(),
            made: None,
            wired: wired.iter().map(|w| vec![(*w).to_string()]).collect(),
        }
    }

    #[test]
    fn a_type_is_said_without_what_it_comes_from() {
        assert_eq!(short("pe2.images@texels"), "texels");
        assert_eq!(short("pe2.images@bs_picture | pe2.images@texels"), "bs_picture | texels");
        assert_eq!(short("file<pe2.str@str>"), "file<str>");
        assert_eq!(short("stream<list<video | audio>>"), "stream<list<video | audio>>");
    }

    #[test]
    fn boxes_are_arranged_by_what_feeds_them_and_touch_nowhere() {
        let mut nodes = vec![node("c", &["a", "b"]), node("a", &[]), node("b", &["a"]), node("d", &[])];
        let places = arrangement(&nodes);
        assert!(places["a"].x < places["b"].x && places["b"].x < places["c"].x);
        for node in &mut nodes {
            node.at = places[&node.id];
        }
        for (i, one) in nodes.iter().enumerate() {
            for other in &nodes[i + 1..] {
                assert!(!one.rect().intersects(other.rect()), "{} and {}", one.id, other.id);
            }
        }
    }
}
