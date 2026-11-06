//! The graph view: extensions in rows by depth, those depending on others above them, requirements as edges - a node
//! an extension, colored by what became of it (loaded, left off by the project, refused) and marked by where it comes
//! from; an edge a dependency, from the extension having it down to the one it names, labeled with the version wanted
//! and the one found, red where a requirement is not met, dashed where it is weak and dotted where optional; a
//! requirement nothing provides is a dashed node of its own. Selecting one lights what it needs and what needs it.

use eframe::egui::{self, Color32};
use project::ext::{Dependency, Present, Status};
use std::collections::{BTreeSet, HashMap, HashSet};

use super::{State, asked_for, chains, origin_text, requirement, status_color, status_text};
use crate::theme;

const NODE: egui::Vec2 = egui::vec2(170.0, 46.0);

const GAP: egui::Vec2 = egui::vec2(28.0, 54.0);
/// Between nodes laid out across: room beside them for the edges' labels, little between those of a column.
const GAP_ACROSS: egui::Vec2 = egui::vec2(90.0, 14.0);
/// A node laid out across where full ones would not fit the pane's height: its name alone, the rest on hover.
const COMPACT: egui::Vec2 = egui::vec2(150.0, 26.0);
const GAP_COMPACT: egui::Vec2 = egui::vec2(90.0, 7.0);
/// How much wider than tall a pane is for the graph to run across it.
const ACROSS: f32 = 2.0;

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
fn edge_ends(
    present: &[Present],
    places: &HashMap<&String, egui::Rect>,
    across: bool,
) -> HashMap<(String, String), [egui::Pos2; 2]> {
    let edges: Vec<(&String, String)> = present
        .iter()
        .flat_map(|p| p.dependencies().map(|(text, _)| (&p.id, requirement(text).0.to_string())))
        .filter(|(from, to)| places.contains_key(from) && places.contains_key(to))
        .collect();
    // Laid out across, an edge leaves its node's right side and enters the other's left, spread down the side.
    let port = |rect: &egui::Rect, at: usize, of: usize, entering: bool| {
        #[expect(clippy::cast_precision_loss, reason = "a handful of edges")]
        let along = 0.2 + 0.6 * (at as f32 + 1.0) / (of as f32 + 1.0);
        if across {
            egui::pos2(if entering { rect.left() } else { rect.right() }, rect.top() + rect.height() * along)
        } else {
            egui::pos2(rect.left() + rect.width() * along, if entering { rect.top() } else { rect.bottom() })
        }
    };
    let x_of = |id: &String| places.get(id).map_or(0.0, |r| if across { r.center().y } else { r.center().x });
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

/// The graph's lines - rows down the pane, or columns across it - and its nodes' size, the gaps between them, whether
/// across, and the room it takes.
struct Arranged<'a> {
    lines: Vec<Vec<&'a String>>,
    node: egui::Vec2,
    gap: egui::Vec2,
    across: bool,
    size: egui::Vec2,
}

/// The graph's rows (`by_row`, by depth) arranged down a pane `width` wide, or across it where it is much wider than
/// tall (`space`).
fn arranged<'a>(by_row: &'a [Vec<String>], space: egui::Vec2, width: f32) -> Arranged<'a> {
    let across = space.x > space.y * ACROSS;
    // Across, nodes are made compact - their names alone - where a column would not fit the pane otherwise: a column
    // wrapping onto another has edges running behind the nodes of the first.
    #[expect(clippy::cast_precision_loss, reason = "a handful of extensions")]
    let tallest = by_row.iter().map(Vec::len).max().unwrap_or(1) as f32;
    let compact = across && tallest * (NODE.y + GAP_ACROSS.y) - GAP_ACROSS.y > space.y - 8.0;
    let (node, gap) = match (across, compact) {
        (false, _) => (NODE, GAP),
        (true, false) => (NODE, GAP_ACROSS),
        (true, true) => (COMPACT, GAP_COMPACT),
    };
    // A row wider than the pane wraps onto more lines of its own, dependents' rows above; across, a column taller than
    // it onto more columns, dependents' to the left.
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a pane's size in nodes")]
    let fit = if across {
        (((space.y - 8.0 + gap.y) / (node.y + gap.y)).floor() as usize).max(1)
    } else {
        (((width.max(node.x) + gap.x) / (node.x + gap.x)).floor() as usize).max(1)
    };
    let lines: Vec<Vec<&'a String>> = by_row
        .iter()
        .rev()
        .flat_map(|ids| ids.chunks(fit).map(|line| line.iter().collect()).collect::<Vec<_>>())
        .collect();
    let longest = lines.iter().map(Vec::len).max().unwrap_or(1);
    #[expect(clippy::cast_precision_loss, reason = "a handful of extensions")]
    let (count, longest) = (lines.len() as f32, longest as f32);
    let size = if across {
        egui::vec2(count * (node.x + gap.x) - gap.x + 8.0, longest * (node.y + gap.y) - gap.y + 8.0)
    } else {
        egui::vec2(width.max(node.x), count * (node.y + gap.y) + 8.0)
    };
    Arranged { lines, node, gap, across, size }
}

/// How wide the graph is laid out across a part of the pane (`space`), where it is: a pane wider than that gives the
/// rest to what is beside it.
pub(super) fn width_across(present: &[Present], space: egui::Vec2) -> Option<f32> {
    let by_row = rows(present);
    let arranged = arranged(&by_row, space, space.x);
    arranged.across.then_some(arranged.size.x)
}

/// Where each node goes, as `arranged` lays them out: the places, the room they take (allocated), and whether across.
fn laid_out<'a>(
    ui: &mut egui::Ui,
    by_row: &'a [Vec<String>],
    space: egui::Vec2,
) -> (HashMap<&'a String, egui::Rect>, egui::Rect, bool) {
    let Arranged { lines, node, gap, across, size } = arranged(by_row, space, ui.available_width());
    let (area, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let mut places: HashMap<&'a String, egui::Rect> = HashMap::new();
    for (line, ids) in lines.iter().enumerate() {
        #[expect(clippy::cast_precision_loss, reason = "a handful of extensions")]
        let (line, many) = (line as f32, ids.len() as f32);
        if across {
            let x = area.left() + 4.0 + line * (node.x + gap.x);
            let mut y = area.center().y - (many * (node.y + gap.y) - gap.y) / 2.0;
            for id in ids {
                places.insert(id, egui::Rect::from_min_size(egui::pos2(x, y), node));
                y += node.y + gap.y;
            }
        } else {
            let y = area.top() + line * (node.y + gap.y) + 4.0;
            let mut x = area.center().x - (many * (node.x + gap.x) - gap.x) / 2.0;
            for id in ids {
                places.insert(id, egui::Rect::from_min_size(egui::pos2(x, y), node));
                x += node.x + gap.x;
            }
        }
    }
    (places, area, across)
}

/// The extensions and what they require that nothing provides, in rows by depth, 0 needing nothing present; within a
/// row, those most needed first: when the row wraps, they stay closest to what needs them.
fn rows(present: &[Present]) -> Vec<Vec<String>> {
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
    let mut by_row: Vec<Vec<String>> = vec![vec![]; rows];
    for id in &nodes {
        by_row[depth[id]].push(id.clone());
    }
    let needed_by = |id: &str| present.iter().filter(|p| p.dependencies().any(|(r, _)| requirement(r).0 == id)).count();
    for row in &mut by_row {
        row.sort_by(|a, b| needed_by(b).cmp(&needed_by(a)).then_with(|| a.cmp(b)));
    }
    by_row
}

/// The graph: extensions in rows by depth (those depending on others above them), requirements as edges - or, on a
/// pane much wider than tall (`space`, the part of the pane it has), in columns by depth, those depending on others to
/// their left, a column taller than the pane wrapping onto more.
pub(super) fn graph_ui(ui: &mut egui::Ui, present: &[Present], state: &mut State, space: egui::Vec2) {
    let by_row = rows(present);
    let (places, area, across) = laid_out(ui, &by_row, space);
    let (needs, needed) = state.selected.as_deref().map_or_default(|s| chains(present, s));
    let chosen = state.selected.clone();
    let lit = |id: &str| chosen.as_deref() == Some(id) || needs.contains(id) || needed.contains(id);
    let painter = ui.painter_at(area);
    let font = egui::TextStyle::Small.resolve(ui.style());
    let ends = edge_ends(present, &places, across);
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
        let compact = rect.height() < NODE.y;
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
        let room = rect.width() - 12.0;
        if compact {
            fitted(painter, rect.center(), id, body, text, room);
            response.on_hover_text(note);
        } else {
            fitted(painter, rect.center_top() + egui::vec2(0.0, 13.0), id, body, text, room);
            fitted(painter, rect.center_bottom() - egui::vec2(0.0, 12.0), &note, font.clone(), theme::weak(), room);
        }
    }
}

/// Text on one line centered at `at`, cut short with an ellipsis where wider than `width`.
fn fitted(painter: &egui::Painter, at: egui::Pos2, text: &str, font: egui::FontId, color: Color32, width: f32) {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_string(), font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(width);
    let galley = painter.layout_job(job);
    painter.galley(at - galley.size() / 2.0, galley, color);
}
