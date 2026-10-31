//! A model's UVs, as a tab beside its 3D view: each texture its faces read, with those faces traced over it where they
//! lie, the selected part's filled; clicking one selects its part. Reading only: a face's place on its texture is the
//! mesh's. A reflection has its texture laid out with no faces: the game works out where they read it as they are drawn.

use std::fmt::Write;

use pe2_ext::{Canvas, Placed, Rect, Style, Traced, Ui, View};

use crate::assembly::{self, Assembled, Bound};
use crate::material_panel::Pictures;
use crate::model::Face;
use crate::parts;
use crate::prefab;

/// What is between two textures laid out.
const GAP: f32 = 16.0;
/// A UV is a byte: what a mesh's are laid over, with no texture to say.
const WHOLE: u32 = 256;

/// A texture faces are laid out on: a bound one, or the slot's alone where a mesh has none.
#[derive(Clone, Debug, PartialEq)]
pub struct Tile {
    pub bound: Option<Bound>,
    /// The slots reading it.
    pub names: Vec<String>,
    pub place: [f32; 2],
    pub size: [f32; 2],
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    pub tiles: Vec<Tile>,
    pub traces: Vec<Traced>,
    pub width: f32,
    pub height: f32,
    /// The faces traced, of the part selected where one is.
    pub faces: usize,
    pub strong: usize,
    /// Faces with no UVs: what no texture shows.
    pub without: usize,
}

fn tile_of(assembled: &Assembled, face: &Face) -> (Option<Bound>, String) {
    let slot = assembled.slots.get(face.material);
    let bound = slot.and_then(|slot| slot.drawn.as_ref()).and_then(|drawn| drawn.main.clone());
    (bound, slot.map_or_else(String::new, |slot| slot.name.clone()))
}

/// What the faces reflect, each texture once, by the slot reflecting it.
fn reflections(assembled: &Assembled) -> Vec<(Bound, String)> {
    let mut out: Vec<(Bound, String)> = Vec::new();
    for face in &assembled.model.faces {
        let slot = assembled.slots.get(face.material);
        let Some((slot, bound)) = slot.and_then(|slot| Some((slot, slot.drawn.as_ref()?.environment.clone()?))) else { continue };
        let name = format!("{} (reflection)", slot.name);
        if !out.iter().any(|(known, named)| *known == bound && *named == name) {
            out.push((bound, name));
        }
    }
    out
}

/// Tiles in rows, a square's worth to a row, each row as tall as its tallest.
fn laid_out(tiles: &mut [Tile]) -> (f32, f32) {
    let root = tiles.len().isqrt();
    let across = if root * root < tiles.len() { root + 1 } else { root }.max(1);
    let (mut width, mut top) = (0.0_f32, 0.0_f32);
    for row in tiles.chunks_mut(across) {
        let mut left = 0.0;
        let mut tall = 0.0_f32;
        for tile in row {
            tile.place = [left, top];
            left += tile.size[0] + GAP;
            tall = tall.max(tile.size[1]);
        }
        width = width.max(left - GAP);
        top += tall + GAP;
    }
    (width, (top - GAP).max(0.0))
}

/// Where each face lies, by the texture it reads.
#[must_use]
pub fn layout(assembled: &Assembled, selected: Option<usize>) -> Layout {
    let (model, owners) = (&assembled.model, assembled.model.owners());
    let mut out = Layout::default();
    let mut placed: Vec<(usize, &Face)> = Vec::new();
    for face in &model.faces {
        if !face.corners.iter().any(|corner| corner.uv.is_some()) {
            out.without += 1;
            continue;
        }
        let (bound, name) = tile_of(assembled, face);
        let key = |tile: &Tile| match (&bound, &tile.bound) {
            (Some(bound), Some(other)) => bound == other,
            (None, None) => tile.names.contains(&name),
            _ => false,
        };
        let at = out.tiles.iter().position(key).unwrap_or_else(|| {
            #[expect(clippy::cast_precision_loss, reason = "a texture is far smaller than floats lose pixels at")]
            let size = bound.as_ref().map_or([WHOLE; 2], |bound| bound.size).map(|side| side as f32);
            out.tiles.push(Tile { bound: bound.clone(), names: Vec::new(), place: [0.0; 2], size });
            out.tiles.len() - 1
        });
        if !out.tiles[at].names.contains(&name) {
            out.tiles[at].names.push(name);
        }
        placed.push((at, face));
    }
    for (bound, name) in reflections(assembled) {
        if let Some(tile) = out.tiles.iter_mut().find(|tile| tile.bound.as_ref() == Some(&bound)) {
            tile.names.push(name);
            continue;
        }
        #[expect(clippy::cast_precision_loss, reason = "a texture is far smaller than floats lose pixels at")]
        let size = bound.size.map(|side| side as f32);
        out.tiles.push(Tile { bound: Some(bound), names: vec![name], place: [0.0; 2], size });
    }
    (out.width, out.height) = laid_out(&mut out.tiles);
    for (at, face) in placed {
        let [left, top] = out.tiles[at].place;
        let corners = face.corners.iter().filter_map(|corner| corner.uv).map(|[u, v]| (left + f32::from(u), top + f32::from(v))).collect();
        let part = face.part_in(&owners);
        let strong = selected.is_some() && part == selected;
        out.faces += 1;
        out.strong += usize::from(strong);
        let label = part.and_then(|part| assembled.nodes.get(part)).map(|node| node.name.clone());
        out.traces.push(Traced { corners, strong, label, link: part.map(|part| format!("#{}{part}", parts::BONE)) });
    }
    out
}

#[derive(Default)]
pub struct UvView {
    assembled: Option<Result<Assembled, String>>,
    pictures: Pictures,
    /// What was drawn, for what was selected.
    drawn: Option<(Option<usize>, Canvas)>,
}

fn summary(layout: &Layout, part: Option<&str>) -> String {
    let mut names: Vec<&str> = Vec::new();
    for name in layout.tiles.iter().flat_map(|tile| tile.names.iter().map(String::as_str)) {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    let mut text = format!("{}  ·  {} faces", names.join(", "), layout.faces);
    if let Some(part) = part {
        let _ = write!(text, "  ·  {} on {part}", layout.strong);
    }
    if layout.without > 0 {
        let _ = write!(text, "  ·  {} with no UVs", layout.without);
    }
    text
}

impl UvView {
    fn canvas(&mut self, ui: &Ui, assembled: &Assembled, selected: Option<usize>) -> Canvas {
        let layout = layout(assembled, selected);
        let mut canvas = Canvas { width: layout.width, height: layout.height, ..Canvas::default() };
        let part = selected.and_then(|part| assembled.nodes.get(part)).map(|node| node.name.as_str());
        canvas.summary = summary(&layout, part);
        for tile in &layout.tiles {
            let ([left, top], [width, height]) = (tile.place, tile.size);
            let rect = Rect { x: left, y: top, width, height };
            let texture = tile.bound.as_ref().and_then(|bound| self.pictures.get(ui, &bound.image, &bound.output()).cloned());
            match texture {
                Some(texture) => canvas.pictures.push(Placed { texture, rect, label: Some(tile.names.join(", ")), link: None }),
                // Nothing to lie on but where it would be.
                None => canvas.traces.push(Traced { corners: vec![(left, top), (left + width, top), (left + width, top + height), (left, top + height)], strong: false, label: None, link: None }),
            }
        }
        canvas.traces.extend(layout.traces);
        canvas
    }
}

impl View for UvView {
    fn show(&mut self, ui: &mut Ui) {
        let assembled = self.assembled.take().or_else(|| ui.asset().map(|asset| assembly::assemble(ui.project(), &asset).map_err(|why| format!("{why:#}"))));
        let assembled = match assembled {
            Some(Ok(assembled)) => assembled,
            Some(Err(why)) => {
                ui.error(&why);
                self.assembled = Some(Err(why));
                return;
            }
            None => return,
        };
        let selected = ui.selected().as_deref().and_then(parts::bone_of);
        if self.drawn.as_ref().is_none_or(|(shown, _)| *shown != selected) {
            self.drawn = Some((selected, self.canvas(ui, &assembled, selected)));
        }
        ui.wrapped(prefab::bar);
        match &self.drawn {
            Some((_, canvas)) if canvas.width > 0.0 => ui.canvas(canvas),
            _ => ui.styled("No face here has UVs", Style::Note),
        }
        self.assembled = Some(Ok(assembled));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assembly::{Drawn, Node, Slot};
    use crate::model::{Blend, Corner, Material, Model, Part, Shading};

    fn bound(cell: &str, size: u32) -> Bound {
        Bound { image: "i".to_string(), cell: cell.to_string(), palette: None, size: [size; 2] }
    }

    fn slot(name: &str, main: Option<Bound>) -> Slot {
        let drawn = main.map(|main| Drawn { transparent: false, blend: Blend::Mix, main: Some(main), environment: None, strength: 0, effect: 0 });
        Slot { name: name.to_string(), material: None, reference: None, drawn, node: None, overrides: None }
    }

    fn face(material: usize, part: usize, uvs: &[[u8; 2]]) -> Face {
        let corners = uvs.iter().map(|uv| Corner { uv: Some(*uv), ..Corner::default() }).collect();
        Face { material, shading: Shading::Unlit, part: Some(part), normal: None, color: None, corners }
    }

    fn assembled(slots: Vec<Slot>, faces: Vec<Face>) -> Assembled {
        let part = |parent| Part { parent, rotation: [[4096, 0, 0], [0, 4096, 0], [0, 0, 4096]], translation: [0; 3], vertices: vec![[0; 3]] };
        let materials = vec![Material { transparent: false, slot: None, blend: None }; slots.len()];
        let model = Model { parts: vec![part(0), part(0)], materials, faces, ..Model::default() };
        let node = |name: &str| Node { name: name.to_string(), attached: None, place: None };
        Assembled { posed: Vec::new(), model, nodes: vec![node("a"), node("b")], slots }
    }

    #[test]
    fn faces_lie_on_their_texture_where_it_was_laid() {
        let slots = vec![slot("x", Some(bound("one", 100))), slot("y", Some(bound("two", 50)))];
        let shown = layout(&assembled(slots, vec![face(1, 0, &[[1, 2], [3, 4], [5, 6]]), face(0, 0, &[[7, 8], [9, 9], [1, 1]])]), None);
        assert_eq!(shown.tiles.len(), 2);
        assert_eq!(shown.tiles[0].names, ["y"], "in the order faces read them");
        assert_eq!(shown.tiles[1].place, [50.0 + GAP, 0.0]);
        assert_eq!(shown.traces[0].corners[0], (1.0, 2.0));
        assert_eq!(shown.traces[1].corners[0], (50.0 + GAP + 7.0, 8.0));
        assert_eq!((shown.width, shown.height), (150.0 + GAP, 100.0));
    }

    #[test]
    fn slots_reading_one_texture_share_its_tile() {
        let slots = vec![slot("x", Some(bound("one", 100))), slot("y", Some(bound("one", 100)))];
        let shown = layout(&assembled(slots, vec![face(0, 0, &[[0, 0]; 3]), face(1, 0, &[[0, 0]; 3])]), None);
        assert_eq!(shown.tiles.len(), 1);
        assert_eq!(shown.tiles[0].names, ["x", "y"]);
    }

    #[test]
    fn what_is_selected_is_strong_and_counted() {
        let faces = vec![face(0, 0, &[[0, 0]; 3]), face(0, 1, &[[0, 0]; 3]), face(0, 1, &[[0, 0]; 3])];
        let shown = layout(&assembled(vec![slot("x", Some(bound("one", 10)))], faces), Some(1));
        assert_eq!(shown.traces.iter().map(|trace| trace.strong).collect::<Vec<_>>(), [false, true, true]);
        assert_eq!((shown.faces, shown.strong), (3, 2));
    }

    #[test]
    fn a_mesh_alone_lies_on_a_whole_page_and_a_face_with_no_uvs_is_counted() {
        let mut plain = face(0, 0, &[[0, 0]; 3]);
        for corner in &mut plain.corners {
            corner.uv = None;
        }
        let shown = layout(&assembled(vec![slot("x", None)], vec![face(0, 0, &[[255, 255]; 3]), plain]), None);
        assert_eq!(shown.tiles[0].size, [256.0; 2]);
        assert_eq!((shown.faces, shown.without), (1, 1));
    }

    #[test]
    fn a_face_says_what_selects_it() {
        let shown = layout(&assembled(vec![slot("x", Some(bound("one", 10)))], vec![face(0, 1, &[[0, 0]; 3])]), None);
        assert_eq!(shown.traces[0].link.as_deref(), Some("#bones/1"));
        assert_eq!(shown.traces[0].label.as_deref(), Some("b"));
    }

    #[test]
    fn a_reflection_has_its_texture_and_no_faces() {
        let mut reflective = slot("x", Some(bound("one", 10)));
        reflective.drawn.as_mut().unwrap().environment = Some(bound("sky", 20));
        let shown = layout(&assembled(vec![reflective], vec![face(0, 0, &[[1, 1]; 3]), face(0, 0, &[[2, 2]; 3])]), None);
        assert_eq!(shown.tiles.len(), 2);
        assert_eq!(shown.tiles[1].names, ["x (reflection)"]);
        assert_eq!(shown.traces.len(), 2, "both on the main texture");
        assert!(shown.traces.iter().all(|trace| trace.corners[0].0 < 10.0));
    }

    #[test]
    fn four_textures_lie_two_by_two() {
        let mut tiles: Vec<Tile> = (0..4).map(|_| Tile { bound: None, names: Vec::new(), place: [0.0; 2], size: [10.0; 2] }).collect();
        assert_eq!(laid_out(&mut tiles), (20.0 + GAP, 20.0 + GAP));
        assert_eq!(tiles[3].place, [10.0 + GAP, 10.0 + GAP]);
    }
}
