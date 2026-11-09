//! The graph editor: a graph layer of a stack, a document's way of showing it. An operation is a box, its input ports down its left
//! and what it makes at its foot on the right; what comes down from above is a box at the left, and what the layer passes
//! on down one at the right. Ports and wires are colored by what they carry. Dragging from a port to another makes a
//! wire, refused where the types do not fit, ports lit while one is being drawn; dragging a wire off an input moves or
//! removes it. Right-clicking the canvas adds an operation, a box is dragged to place it and Delete removes the one
//! selected. A box selected is what is selected inside the asset, its settings in the inspector with everything else
//! selected there. Every edit is made as any is: undoable, unsaved until saved, and what is wrong with the stack as it
//! stands is told from its shape, under the canvas.
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
use crate::theme;
use crate::workspace::{EditRequest, GraphAsk, Workspace};

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
    /// What it makes, each by name - its first under none - and type where known: a dot each.
    outs: Vec<(String, Option<Type>)>,
    /// What each port takes from, by its place.
    wired: Vec<Vec<String>>,
    /// The settings of the operation it is in that it reads (`{ setting = "<name>" }`); and, a setting box, its name.
    reads: Vec<String>,
    setting: Option<String>,
}

/// The settings a value reads, where it is `{ setting = "<name>" }`, in it.
fn settings_read(value: &Value, found: &mut Vec<String>) {
    match value {
        Value::Table(table) if table.len() == 1 && table.get("setting").is_some_and(Value::is_str) => {
            found.extend(table.get("setting").and_then(Value::as_str).map(str::to_string));
        }
        Value::Table(table) => table.values().for_each(|v| settings_read(v, found)),
        Value::Array(items) => items.iter().for_each(|v| settings_read(v, found)),
        _ => {}
    }
}

/// A count of rows or columns, in points.
#[expect(clippy::cast_precision_loss, reason = "a handful of them")]
fn rows(count: usize) -> f32 {
    count as f32
}

impl Node {
    fn size(&self) -> Vec2 {
        vec2(WIDTH, HEADER + ROW * rows((self.ports.len() + self.outs.len()).max(1)) + 6.0)
    }

    fn rect(&self) -> Rect {
        Rect::from_min_size(self.at, self.size())
    }

    fn input(&self, port: usize) -> Pos2 {
        self.at + vec2(0.0, HEADER + ROW * (rows(port) + 0.5))
    }

    fn output(&self, n: usize) -> Pos2 {
        self.at + vec2(WIDTH, HEADER + ROW * (rows(self.ports.len() + n) + 0.5))
    }

    /// What a wire from its output names: its id, or `id.<name>`.
    fn source(&self, name: &str) -> String {
        if name.is_empty() { self.id.clone() } else { format!("{}.{name}", self.id) }
    }

    /// What it makes first.
    fn made(&self) -> Option<&Type> {
        self.outs.first().and_then(|(_, ty)| ty.as_ref())
    }
}

/// Whether an asset's stack has a graph layer.
pub fn has_graph(asset: &Asset) -> bool {
    let layers = asset.fields_toml().get("stack").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    layers.iter().any(is_graph_layer)
}

/// Whether a layer holds a graph: a graph layer, or the graph of an operation the project defines.
fn is_graph_layer(layer: &Value) -> bool {
    matches!(layer.get("op").and_then(Value::as_str), Some(graph::GRAPH | graph::DEFINED))
}

/// The graph layer a pane shows of an asset: the one selected, else its first.
fn layer_of(ws: &Workspace, asset: &Asset) -> Option<usize> {
    let layers = asset.fields_toml().get("stack")?.as_array()?;
    let is_graph = |at: usize| layers.get(at).is_some_and(is_graph_layer);
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

/// The box a wire's source names: its id, without the output (`id.<name>`).
fn base(source: &str) -> &str {
    source.split_once('.').map_or(source, |(id, _)| id)
}

/// Where each box goes, by its id: in columns by how far it is from what has no source here, each column ordered by
/// where its sources are, so wires cross little, and spaced so no box touches another.
fn arrangement(nodes: &[Node]) -> HashMap<String, Pos2> {
    let mut depth: HashMap<&str, usize> = nodes.iter().map(|n| (n.id.as_str(), 0)).collect();
    for _ in 0..nodes.len() {
        for node in nodes {
            let deepest = node.wired.iter().flatten().filter_map(|s| depth.get(base(s))).max().map(|d| d + 1);
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
                let heights: Vec<f32> =
                    node.wired.iter().flatten().filter_map(|s| places.get(base(s))).map(|p| p.y).collect();
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

/// A graph's operations as boxes, those not placed where they would be arranged: their steps found in the plan by where
/// they are (`<prefix>.ops.<id>`), in an operation the project defines its boxes as it is from outside.
fn nodes_of(ws: &Workspace, layer: &Value, plan: &graph::Plan, prefix: &str, defined: bool) -> Vec<Node> {
    let ops = ws.extensions.operations();
    let table = layer.get("ops").and_then(Value::as_table);
    let mut nodes: Vec<Node> = table
        .into_iter()
        .flatten()
        .map(|(id, op)| {
            let name = op.get("op").and_then(Value::as_str).unwrap_or_default().to_string();
            let def = ops.get(&name);
            let group = ops.definitions.get(&name);
            let output = name == graph::OUTPUT;
            let boxed = [graph::INPUT, graph::SETTING].contains(&name.as_str()) || (defined && output);
            let mut ports = ops.ports_of(&name);
            // An operation the project defines' output takes one value, and says nothing of events.
            if defined && output {
                ports.truncate(1);
            }
            // Any operation that makes a value may be told whether it runs; boxes of signals and of an operation's
            // interface may not.
            let signals = [graph::OUTPUT, graph::PREVIEW, graph::SIGNAL, graph::EVENT];
            let gate = graph::GATES.iter().any(|g| g.name == name);
            if (def.is_some() || group.is_some()) && !boxed && !signals.contains(&name.as_str()) && !gate {
                ports.push(pe2_ext::Port {
                    name: graph::ACTIVE.to_string(),
                    ty: Type::Named(graph::BOOL_TYPE.to_string()),
                    many: false,
                });
            }
            let from = op.get("from");
            let wired = ports.iter().map(|p| sources(from.and_then(|f| f.get(&p.name)))).collect();
            let here = format!("{prefix}.ops.{id}");
            let stepped = plan.steps.iter().find(|s| s.at == here).map(|s| s.ty.clone());
            let declared = || Type::parse(op.get("type")?.as_str()?, "").ok();
            let outs = match name.as_str() {
                graph::OUTPUT | graph::PREVIEW | graph::SIGNAL => Vec::new(),
                graph::INPUT => vec![(String::new(), Some(declared().unwrap_or(Type::Any)))],
                _ => {
                    let mut outs: Vec<(String, Option<Type>)> =
                        ops.outputs_of(&name).into_iter().map(|(name, ty)| (name, Some(ty))).collect();
                    if outs.is_empty() {
                        outs.push((String::new(), None));
                    }
                    if stepped.is_some() {
                        outs[0].1 = stepped;
                    }
                    outs
                }
            };
            let place = op.get(layers::POS).and_then(Value::as_array);
            let number = |i: usize| place.and_then(|p| p.get(i)?.as_float());
            #[expect(clippy::cast_possible_truncation, reason = "a place on a canvas")]
            let placed = number(0).zip(number(1)).map(|(x, y)| pos2(x as f32, y as f32));
            let named = op.get("name").and_then(Value::as_str).unwrap_or(id).to_string();
            let boxed_label = || Some(format!("{}: {named}", def.map_or("", |d| d.label.as_str())));
            let label = if boxed { boxed_label() } else { None }
                .or_else(|| def.map(|d| d.label.clone()))
                .or_else(|| group.map(|g| g.label.clone()))
                .unwrap_or_else(|| name.clone());
            let mut reads = Vec::new();
            settings_read(op, &mut reads);
            let setting = (name == graph::SETTING).then_some(named);
            let at = placed.unwrap_or_default();
            Node { id: id.clone(), label, at, placed: placed.is_some(), ports, outs, wired, reads, setting }
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
    ty.split_inclusive(|c: char| " <>|".contains(c))
        .map(|word| word.rsplit_once('@').map_or(word, |(_, local)| local))
        .collect()
}

/// What a value of a type is drawn in, so what can be wired together looks it.
fn type_color(ty: Option<&Type>) -> Color32 {
    match ty {
        Some(Type::File(of)) => type_color(Some(of)),
        Some(Type::Stream(_) | Type::Video | Type::Audio | Type::List(_)) => Color32::from_rgb(178, 132, 226),
        Some(Type::Image) => Color32::from_rgb(86, 196, 168),
        Some(Type::Table | Type::Marks) => Color32::from_rgb(224, 172, 86),
        Some(Type::Named(name)) if name == graph::BOOL_TYPE => Color32::from_rgb(228, 112, 128),
        Some(Type::Named(_) | Type::Union(_) | Type::Bytes) => Color32::from_rgb(98, 160, 232),
        _ => theme::weak(),
    }
}

/// A wire, as a curve leaving its source and coming into its port level.
fn wire(painter: &egui::Painter, from: Pos2, to: Pos2, color: Color32) {
    let reach = ((to.x - from.x).abs() * 0.5).max(40.0);
    let points = [from, from + vec2(reach, 0.0), to - vec2(reach, 0.0), to];
    painter.add(egui::epaint::CubicBezierShape::from_points_stroke(
        points,
        false,
        Color32::TRANSPARENT,
        egui::Stroke::new(2.0, color),
    ));
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
fn box_at(
    painter: &egui::Painter,
    rect: Rect,
    title: &str,
    tint: Color32,
    (selected, wrong): (bool, bool),
    ui: &egui::Ui,
) {
    let edge = if wrong {
        ui.visuals().error_fg_color
    } else if selected {
        theme::accent()
    } else {
        theme::border()
    };
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
fn add_menu(
    ui: &mut egui::Ui,
    ws: &Workspace,
    asset: &Asset,
    (at, layer): (usize, &Value),
    state: &mut State,
    edit: &mut Option<EditRequest>,
) {
    let response =
        ui.add(egui::TextEdit::singleline(&mut state.filter).hint_text("Filter operations").desired_width(220.0));
    response.request_focus();
    let filter = state.filter.to_lowercase();
    let taken: Vec<&str> =
        layer.get("ops").and_then(Value::as_table).into_iter().flatten().map(|(id, _)| id.as_str()).collect();
    egui::ScrollArea::vertical().max_height(280.0).show(ui, |ui| {
        let defined = layer.get("op").and_then(Value::as_str) == Some(graph::DEFINED);
        for (op, label) in ws.extensions.graph_operations() {
            if !label.to_lowercase().contains(&filter) && !op.to_lowercase().contains(&filter) {
                continue;
            }
            // What stands for a value from outside is never added; an operation's interface, only to one.
            if op == graph::GIVEN || (!defined && [graph::INPUT, graph::SETTING].contains(&op.as_str())) {
                continue;
            }
            if ui.button(&label).on_hover_text(&op).clicked() {
                let id = layers::free_id(&taken, &op);
                let mut table = toml::Table::new();
                table.insert("op".into(), op.as_str().into());
                // What it cannot do without, for the sheet to change.
                match op.as_str() {
                    graph::FILE => {
                        let stored =
                            asset.files.iter().find(|f| f.made_by.is_none() && f.name != project::index::NODE_FILE);
                        table.insert("file".into(), stored.map_or("", |f| f.name.as_str()).into());
                    }
                    graph::OUTPUT | graph::PREVIEW | graph::INPUT | graph::SETTING => {
                        table.insert("name".into(), id.as_str().into());
                    }
                    _ => {}
                }
                let place = [f64::from(state.adding.x), f64::from(state.adding.y)];
                *edit = inspector::layers_edit(
                    asset,
                    format!("Add {label}"),
                    layers::add_operation(asset.text(), at, &id, table, place),
                );
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
fn extent(nodes: &[Node], sources: usize) -> Rect {
    let (above, below) = ends(nodes);
    let all = nodes
        .iter()
        .map(Node::rect)
        .chain([end_rect(below)])
        .chain((0..sources).map(|n| end_rect(source_at(above, n))));
    all.fold(Rect::NOTHING, Rect::union).expand(MARGIN)
}

/// A canvas's contents, with what they need of the pane.
struct Canvas<'a> {
    ws: &'a Workspace,
    asset: &'a Asset,
    at: usize,
    layer: &'a Value,
    nodes: Vec<Node>,
    /// What the graph is given, a box each: what comes from above, by the name its operations take it by, what it is
    /// called, and its type. None in an operation the project defines, whose input boxes are its own.
    sources: Vec<(String, String, Type)>,
    wrong: Vec<graph::Problem>,
    /// Where its operations' steps are in the plan (`<prefix>.ops.<id>`).
    prefix: String,
    /// Whether it is an operation the project defines: what it makes is its output boxes', passed on to no layer.
    defined: bool,
}

/// Where a graph's `n`th source box is.
fn source_at(above: Pos2, n: usize) -> Pos2 {
    #[expect(clippy::cast_precision_loss, reason = "a handful of boxes")]
    let n = n as f32;
    above + vec2(0.0, n * (HEADER + ROW + 6.0 + GAP.y))
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
    fn wires(&self, ui: &egui::Ui, outs: &HashMap<String, (Pos2, Option<Type>)>, into: Option<(Pos2, &str)>) {
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
        // What reads a setting, by a faint line from the setting's box to the reader's title.
        for node in &self.nodes {
            for name in &node.reads {
                let Some(setting) = self.nodes.iter().find(|n| n.setting.as_deref() == Some(name)) else { continue };
                let color = type_color(setting.made()).gamma_multiply(0.35);
                wire(painter, setting.output(0), node.at + vec2(0.0, HEADER * 0.5), color);
            }
        }
        if let Some((to, source)) = into
            && let Some((from, made)) = outs.get(source)
        {
            wire(painter, *from, to, type_color(made.as_ref()));
        }
    }

    /// A box and its ports: what dragging them does.
    fn operation(
        &self,
        ui: &egui::Ui,
        node: &Node,
        state: &mut State,
        drawing: Option<&Option<Type>>,
        edit: &mut Option<EditRequest>,
    ) {
        let painter = ui.painter();
        let small = egui::TextStyle::Small.resolve(ui.style());
        let response = ui.interact(node.rect(), egui::Id::unique(("graph box", &node.id)), Sense::click_and_drag());
        let says = format!("operation {}: {}", node.id, node.label);
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::Role::Button, true, state.selected.as_deref() == Some(&node.id), &says)
        });
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
        let here = format!("{}.ops.{}", self.prefix, node.id);
        let wrong = self.wrong.iter().any(|p| p.at == here || p.at.starts_with(&format!("{here} ")));
        box_at(painter, node.rect(), &node.label, type_color(node.made()), (selected, wrong), ui);
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
        for (n, (name, ty)) in node.outs.iter().enumerate() {
            let place = node.output(n);
            port_dot(painter, place, type_color(ty.as_ref()), false);
            let made = ty.as_ref().map_or_else(|| "any".to_string(), ToString::to_string);
            let text = if name.is_empty() { short(&made) } else { format!("{name}  ·  {}", short(&made)) };
            text_at(painter, place - vec2(12.0, 0.0), egui::Align2::RIGHT_CENTER, &text, small.clone(), theme::text());
            let says = format!("output {} of {}: makes {made}", if name.is_empty() { "first" } else { name }, node.id);
            if port_response(ui, ("graph out", &node.id, name), place, &says).drag_started() {
                state.wire = Some((node.source(name), None));
            }
        }
    }

    /// The two ends: what comes down from above, and what goes on down.
    fn ends_ui(&self, ui: &egui::Ui, (above, below): (Pos2, Pos2), passed: Option<&str>, state: &mut State) {
        let painter = ui.painter();
        let small = egui::TextStyle::Small.resolve(ui.style());
        for (n, (id, label, ty)) in self.sources.iter().enumerate() {
            let at = source_at(above, n);
            box_at(painter, end_rect(at), label, type_color(Some(ty)), (false, false), ui);
            let place = at + vec2(END_WIDTH, HEADER + ROW * 0.5);
            let text = ty.to_string();
            text_at(
                painter,
                place - vec2(12.0, 0.0),
                egui::Align2::RIGHT_CENTER,
                &short(&text),
                small.clone(),
                theme::text(),
            );
            port_dot(painter, place, type_color(Some(ty)), false);
            if port_response(ui, ("graph out", id.as_str(), ""), place, &format!("{label}: {text}")).drag_started() {
                state.wire = Some((id.clone(), None));
            }
        }
        // What an operation the project defines makes is its output boxes': nothing goes on down.
        if self.defined {
            return;
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
    fn dropped(
        &self,
        at: Pos2,
        sink: Pos2,
        (source, off): (String, Option<(String, String)>),
        outs: &HashMap<String, (Pos2, Option<Type>)>,
        state: &mut State,
    ) -> Option<EditRequest> {
        let (layer, mut text) = (self.at, Ok(self.asset.text().to_string()));
        let mut label = format!("Unplug {source}");
        let mut wrote = off.is_some();
        // What it is pulled off goes first: both one step.
        if let Some((to, port)) = &off {
            text = text.and_then(|t| {
                if to == "result" {
                    layers::set_result(&t, layer, None)
                } else {
                    layers::disconnect(&t, layer, (to, port), &source)
                }
            });
        }
        let port = self.nodes.iter().find_map(|node| {
            (0..node.ports.len()).find(|i| node.input(*i).distance(at) <= PORT).map(|i| (node, &node.ports[i]))
        });
        if !self.defined && sink.distance(at) <= PORT {
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
                state.refused =
                    made.map(|ty| format!("{} of {} takes {}, and {source} makes {ty}", port.name, node.id, port.ty));
            }
        }
        wrote.then(|| self.edit(label, text)).flatten()
    }

    /// Everything on the canvas, in the scene's coordinates.
    fn contents(&self, ui: &egui::Ui, state: &mut State, edit: &mut Option<EditRequest>) {
        Self::dots(ui);
        let (above, below) = ends(&self.nodes);
        let source = |n: usize| source_at(above, n) + vec2(END_WIDTH, HEADER + ROW * 0.5);
        let sink = below + vec2(0.0, HEADER + ROW * 0.5);
        let mut outs: HashMap<String, (Pos2, Option<Type>)> = self
            .nodes
            .iter()
            .flat_map(|n| n.outs.iter().enumerate().map(move |(i, (name, ty))| (n.source(name), (n.output(i), ty.clone()))))
            .collect();
        for (n, (id, _, ty)) in self.sources.iter().enumerate() {
            outs.insert(id.clone(), (source(n), Some(ty.clone())));
        }
        let passed = self.layer.get("result").and_then(Value::as_str).filter(|_| !self.defined);
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
    let mut view = state.view.unwrap_or_else(|| extent(&canvas.nodes, canvas.sources.len()));
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
        if ui
            .add_enabled(editable, egui::Button::new("Arrange"))
            .on_hover_text("Put the boxes in columns, by what feeds what")
            .clicked()
        {
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
        ui.label(
            RichText::new("This asset's stack has no graph layer. Add one in the Inspector.").color(theme::weak()),
        );
        return;
    };
    let Some(layer) = asset.fields_toml().get("stack").and_then(Value::as_array).and_then(|l| l.get(at)).cloned()
    else {
        return;
    };
    let id = egui::Id::unique(("graph pane", &asset.path, at));
    let mut state: State = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    // What is selected of it is the workspace's: every pane shows the same.
    state.selected = ws
        .inner_of(&asset.path)
        .and_then(inspector::layer_of_inner)
        .filter(|(of, _)| *of == at)
        .and_then(|(_, op)| op.map(str::to_string));
    let before = state.selected.clone();
    // An operation the project defines is laid out alone, its settings their defaults: what its boxes make, and what
    // is wrong with it. A graph layer is its node's plan.
    let defined = layer.get("op").and_then(Value::as_str) == Some(graph::DEFINED);
    let found = asset.uid.as_deref().and_then(|uid| ws.extensions.definition(uid)).filter(|_| defined);
    let (plan, wrong, prefix) = if let Some(definition) = &found {
        let ops = ws.extensions.operations();
        let (plan, wrong) = graph::Plan::of_definition(definition, &toml::Table::new(), &ops, &format!("stack[{at}]"));
        (plan, wrong, format!("stack[{at}] ({})", definition.label))
    } else {
        let (plan, wrong) = ws.extensions.plan(&asset);
        (plan, wrong, format!("stack[{at}]"))
    };
    let mut nodes = nodes_of(ws, &layer, &plan, &prefix, defined);
    if let Some((moving, place)) = &state.moving
        && let Some(node) = nodes.iter_mut().find(|n| n.id == *moving)
    {
        node.at = *place;
    }
    // What a graph layer is given: what is above it. An operation the project defines has input boxes of its own.
    let sources = if defined {
        Vec::new()
    } else {
        plan.above
            .get(at)
            .cloned()
            .flatten()
            .map(|ty| (ABOVE.to_string(), "From above".to_string(), ty))
            .into_iter()
            .collect()
    };
    let ask = ws.graph_ask.take();
    let canvas = Canvas { ws, asset: &asset, at, layer: &layer, nodes, sources, wrong, prefix, defined };
    match ask {
        Some(GraphAsk::Fit) => state.view = None,
        Some(GraphAsk::Arrange) if inspector::edit_target(canvas.ws, canvas.asset).is_ok() => {
            arrange(&canvas, &mut state, edit);
        }
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
            outs: vec![(String::new(), None)],
            reads: Vec::new(),
            setting: None,
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
