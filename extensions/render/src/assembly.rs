//! What a 3D view shows: a mesh, or a model with its materials filled, or a prefab's models in one tree. The game has
//! one coordinate node for a joint and an attachment alike, so a prefab comes out as one model, a child's root hanging
//! under the node it is attached to; posing and picking need to know nothing of how it was put together.

use pe2_ext::{Asset, Project, Result, Value, anyhow, bail};

use crate::material::{self, Definition};
use crate::mesh;
use crate::prefab::{self, Filled};
use crate::model::{Blend, Corner, Face, Model, Part, Shading};
use crate::skeleton;

/// What fills a texture slot: a cell of an image, through a palette of its or the cell's own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bound {
    pub image: String,
    pub cell: String,
    pub palette: Option<String>,
    /// In texels.
    pub size: [u32; 2],
}

impl Bound {
    /// The output of its image that is the picture.
    #[must_use]
    pub fn output(&self) -> String {
        material::cell_address(&self.cell, self.palette.as_deref())
    }
}

/// How a slot's faces draw, a prefab's overrides taken.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Drawn {
    pub transparent: bool,
    pub blend: Blend,
    pub main: Option<Bound>,
    pub environment: Option<Bound>,
    /// The environment's share, of `material::LAYER_FULL`.
    pub strength: u8,
    pub effect: u32,
}

/// A node of what was assembled: a part of the model, by what it is called.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub name: String,
    /// The model attached here, for the root of one; a prefab's root and a joint have none.
    pub attached: Option<Attached>,
    /// Where its transform is written, where it can be.
    pub place: Option<Place>,
}

/// Where a node's transform is kept: a skeleton's own, or a prefab's for what it does to a joint or to what fills a part.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    Joint(usize),
    Pose(String),
    /// Where in the prefab what fills it is, step by step.
    Fill(Vec<String>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attached {
    pub path: String,
    pub reference: String,
}

/// A material slot of what was assembled.
#[derive(Clone, Debug, PartialEq)]
pub struct Slot {
    pub name: String,
    /// What fills it, for a model; a mesh alone has its slots empty.
    pub material: Option<Attached>,
    /// As the model or the prefab's override names it.
    pub reference: Option<Value>,
    pub drawn: Option<Drawn>,
    /// The prefab node it is on: what it is called, and where in the prefab its overrides are.
    pub node: Option<(String, Vec<String>)>,
    /// What the node overrides of it.
    pub overrides: Option<Value>,
}

impl Slot {
    fn empty(name: &str) -> Slot {
        Slot { name: name.to_string(), material: None, reference: None, drawn: None, node: None, overrides: None }
    }
}

pub struct Assembled {
    pub model: Model,
    /// The joints a prefab moves from where the skeleton has them.
    pub posed: Vec<String>,
    /// By the model's part.
    pub nodes: Vec<Node>,
    /// By the model's material.
    pub slots: Vec<Slot>,
}

fn leaf(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn resolved<'a>(project: &'a dyn Project, holder: &Asset, key: &str, reference: Option<&Value>) -> Result<Asset<'a>> {
    let reference = reference.ok_or_else(|| anyhow!("{}: no {key}", holder.path()))?;
    project.resolve(reference.get("slot").unwrap_or(reference)).ok_or_else(|| anyhow!("{}: its {key} is not in the project", holder.path()))
}

fn bound_of(project: &dyn Project, holder: &Asset, texture: &Value) -> Result<Bound> {
    let image = resolved(project, holder, "image", Some(texture))?;
    let cell = material::cell_of(texture).ok_or_else(|| anyhow!("{}: a texture has no cell", holder.path()))?;
    let held = image.parts("cells").into_iter().find(|(name, _)| *name == cell).ok_or_else(|| anyhow!("{}: {} has no cell {cell}", holder.path(), image.path()))?;
    let rect = held.1.get("rect").and_then(Value::as_array).ok_or_else(|| anyhow!("{}: {cell} has no size", image.path()))?;
    let side = |at: usize| rect.get(at).and_then(Value::as_integer).and_then(|side| u32::try_from(side).ok()).unwrap_or(1);
    Ok(Bound { image: image.path().to_string(), cell: cell.to_string(), palette: material::palette_of(texture).map(str::to_string), size: [side(2), side(3)] })
}

fn drawn_of<'a>(project: &dyn Project, holder: &Asset, field: impl Fn(&str) -> Option<&'a Value>) -> Result<Drawn> {
    let read = Definition::read(&field);
    let texture = |key: &str| {
        let taken = read.shader.textures.iter().any(|(known, _)| *known == key);
        field(key).filter(|_| taken).map(|texture| bound_of(project, holder, texture)).transpose()
    };
    Ok(Drawn { transparent: read.transparent, blend: read.blend, main: texture(material::MAIN)?, environment: texture(material::ENVIRONMENT)?, strength: read.strength, effect: read.effect })
}

/// A slot of a model, with what the prefab node holding it overrides: the same keys as a material's, and `material`.
fn slot_of(project: &dyn Project, model: &Asset, name: &str, overrides: Option<&Value>) -> Result<Slot> {
    let of_model = model.field("materials").and_then(|materials| materials.get(name));
    let Some(reference) = overrides.and_then(|overrides| overrides.get("material")).or(of_model) else {
        return Ok(Slot::empty(name));
    };
    let held = resolved(project, model, name, Some(reference))?;
    let drawn = drawn_of(project, &held, |key| overrides.and_then(|overrides| overrides.get(key)).or_else(|| held.field(key)))?;
    let attached = Attached { path: held.path().to_string(), reference: held.uid().unwrap_or_default().to_string() };
    Ok(Slot { material: Some(attached), reference: Some(reference.clone()), drawn: Some(drawn), overrides: overrides.cloned(), ..Slot::empty(name) })
}

/// A mesh, posed by its skeleton where it has one, with its parts named.
fn mesh_of(project: &dyn Project, mesh: &Asset) -> Result<Assembled> {
    let (mut model, names) = mesh::read(&String::from_utf8_lossy(&mesh.read(mesh::FILE)?))?;
    let slots = names.iter().map(|name| Slot::empty(name)).collect();
    let mut nodes: Vec<Node> = (0..model.parts.len()).map(|_| Node { name: leaf(mesh.path()).to_string(), attached: None, place: None }).collect();
    if mesh.field("skeleton").is_some() {
        let skeleton = resolved(project, mesh, "skeleton", mesh.field("skeleton"))?;
        let joints = skeleton::of(&skeleton)?;
        mesh::pose(&mut model, &joints)?;
        nodes = joints.into_iter().map(|joint| Node { name: joint.name, attached: None, place: None }).collect();
    }
    Ok(Assembled { posed: Vec::new(), model, nodes, slots })
}

fn model_of(project: &dyn Project, asset: &Asset, overrides: Option<&Value>, node: Option<(&str, &[String])>) -> Result<Assembled> {
    let mesh = resolved(project, asset, "mesh", asset.field("mesh"))?;
    let mut assembled = mesh_of(project, &mesh)?;
    for slot in &mut assembled.slots {
        let overridden = overrides.and_then(|overrides| overrides.get(&slot.name));
        *slot = slot_of(project, asset, &slot.name, overridden)?;
        slot.node = node.map(|(name, within)| (name.to_string(), within.to_vec()));
    }
    Ok(assembled)
}

/// A skeleton alone: its joints, as parts without vertices.
fn skeleton_of(asset: &Asset) -> Result<Assembled> {
    let joints = skeleton::of(asset)?;
    let part = |(at, joint): (usize, &skeleton::Joint)| Part { parent: joint.parent.unwrap_or(at), rotation: skeleton::matrix(joint.rotation), translation: joint.translation, vertices: Vec::new() };
    let parts = joints.iter().enumerate().map(part).collect();
    let named = joints.into_iter().enumerate();
    let nodes = named.map(|(at, joint)| Node { name: joint.name, attached: None, place: Some(Place::Joint(at)) }).collect();
    Ok(Assembled { posed: Vec::new(), model: Model { parts, ..Model::default() }, nodes, slots: Vec::new() })
}

const BALL_RADIUS: f64 = 350.0;
const BALL_ROWS: usize = 24;
const BALL_COLUMNS: usize = 36;

/// A ball with a cell of `size` wrapped once around it, for a material to show on.
fn ball(size: Option<[u32; 2]>) -> Model {
    let across = BALL_COLUMNS + 1;
    let (mut vertices, mut normals) = (Vec::new(), Vec::new());
    #[expect(clippy::cast_precision_loss, clippy::cast_possible_truncation, reason = "a dozen rows, of coordinates well within a short")]
    for (row, column) in (0..=BALL_ROWS).flat_map(|row| (0..=BALL_COLUMNS).map(move |column| (row, column))) {
        let (down, around) = (std::f64::consts::PI * row as f64 / BALL_ROWS as f64, std::f64::consts::TAU * column as f64 / BALL_COLUMNS as f64);
        let unit = [down.sin() * around.cos(), -down.cos(), down.sin() * around.sin()];
        vertices.push(unit.map(|x| (x * BALL_RADIUS) as i16));
        normals.push(unit.map(|x| (x * 4096.0) as i16));
    }
    let uv = |vertex: usize| {
        let [width, height] = size?;
        #[expect(clippy::cast_possible_truncation, reason = "a cell is at most a page across")]
        let place = |at: usize, of: usize, side: u32| (at * (side.min(256) as usize - 1) / of) as u8;
        Some([place(vertex % across, BALL_COLUMNS, width), place(vertex / across, BALL_ROWS, height)])
    };
    let corner = |vertex: usize| Corner { vertex, normal: Some(vertex), color: size.is_none().then_some([200, 200, 205]), uv: uv(vertex) };
    let face = |(row, column): (usize, usize)| {
        let at = row * across + column;
        Face { material: 0, shading: Shading::Smooth, part: Some(0), normal: None, color: None, corners: vec![corner(at), corner(at + 1), corner(at + across + 1), corner(at + across)] }
    };
    let faces = (0..BALL_ROWS).flat_map(|row| (0..BALL_COLUMNS).map(move |column| (row, column))).map(face).collect();
    let part = Part { parent: 0, rotation: skeleton::matrix([0.0; 3]), translation: [0, -500, 0], vertices };
    Model { parts: vec![part], normals, materials: vec![crate::model::Material { transparent: false, slot: None, blend: None }], faces, ..Model::default() }
}

/// A material alone, on a ball.
fn material_of(project: &dyn Project, asset: &Asset) -> Result<Assembled> {
    let drawn = drawn_of(project, asset, |key| asset.field(key))?;
    let model = ball(drawn.main.as_ref().map(|main| main.size));
    let slot = Slot { drawn: Some(drawn), ..Slot::empty(leaf(asset.path())) };
    Ok(Assembled { posed: Vec::new(), model, nodes: vec![Node { name: leaf(asset.path()).to_string(), attached: None, place: None }], slots: vec![slot] })
}

/// What a prefab says of a joint: it replaces what the skeleton says, as far as it goes.
fn posed(part: &mut Part, pose: &Value) {
    part.translation = skeleton::translation_of(pose).unwrap_or(part.translation);
    if let Some(rotation) = skeleton::numbers_of(pose, "rotation") {
        part.rotation = skeleton::matrix(rotation);
    }
}

/// How a prefab holds what it attaches: over what the model's root says of itself, as a child of the node.
fn held(part: &mut Part, child: &Value) {
    let [x, y, z] = skeleton::translation_of(child).unwrap_or_default();
    part.translation = [part.translation[0] + x, part.translation[1] + y, part.translation[2] + z];
    let turned = skeleton::numbers_of(child, "rotation").map_or([[4096, 0, 0], [0, 4096, 0], [0, 0, 4096]], skeleton::matrix);
    let scale = skeleton::numbers_of(child, "scale").unwrap_or([1.0; 3]);
    let wide = |row: &[i16; 3]| row.map(f64::from);
    let (turned, rest) = (turned.map(|row| wide(&row)), part.rotation.map(|row| wide(&row)));
    #[expect(clippy::cast_possible_truncation, reason = "clamped to what the game's fixed point holds")]
    let cell = |row: usize, column: usize| ((0..3).map(|k| turned[row][k] * scale[k] * rest[k][column]).sum::<f64>() / 4096.0).clamp(-32768.0, 32767.0).round() as i16;
    part.rotation = std::array::from_fn(|row| std::array::from_fn(|column| cell(row, column)));
}

/// Whether what fills a part is built on a skeleton: its parts are that skeleton's joints, not something to hang on them.
fn on_skeleton(project: &dyn Project, asset: &Asset, skeleton: &Asset, fill: &Value) -> Result<bool> {
    let model = resolved(project, asset, "model", fill.get("model"))?;
    let mesh = resolved(project, &model, "mesh", model.field("mesh"))?;
    let named = mesh.field("skeleton").map(|reference| resolved(project, &mesh, "skeleton", Some(reference))).transpose()?;
    Ok(named.is_some_and(|named| named.path() == skeleton.path()))
}

/// A part's own node, under the node it hangs on.
fn hang(project: &dyn Project, asset: &Asset, tree: &mut Assembled, part: &Filled, on: usize) -> Result<()> {
    let place = Some(Place::Fill(part.within.clone()));
    let Some(fill) = &part.fill else {
        // Open: a place to see and to move, with nothing there yet.
        let rest = Part { parent: 0, rotation: skeleton::matrix([0.0; 3]), translation: [0; 3], vertices: Vec::new() };
        attach(&mut tree.model, Model { parts: vec![rest], ..Model::default() }, on);
        tree.nodes.push(Node { name: part.part.clone(), attached: None, place });
        return Ok(());
    };
    let model = resolved(project, asset, "model", fill.get("model"))?;
    let Assembled { model: mut built, mut nodes, mut slots, .. } = model_of(project, &model, fill.get("overrides"), Some((&part.part, &part.within)))?;
    if let Some(root) = built.parts.first_mut() {
        held(root, fill);
    }
    // What it hangs by is the part; a part of it deeper down keeps its own name.
    if let Some(first) = nodes.first_mut() {
        part.part.clone_into(&mut first.name);
        first.attached = Some(Attached { path: model.path().to_string(), reference: model.uid().unwrap_or_default().to_string() });
        first.place = place;
    }
    attach(&mut tree.model, built, on);
    tree.nodes.append(&mut nodes);
    tree.slots.append(&mut slots);
    Ok(())
}

fn prefab_of(project: &dyn Project, asset: &Asset) -> Result<Assembled> {
    let frame = resolved(project, asset, "skeleton", asset.field("skeleton"))?;
    let parts = prefab::filled(asset, project)?;
    let mut waiting: Vec<&Filled> = Vec::new();
    let mut root = None;
    for part in parts.iter().filter(|part| part.node.is_some() || part.within.first().is_some_and(|step| step == "groups")) {
        let built_on = part.fill.as_ref().map(|fill| on_skeleton(project, asset, &frame, fill)).transpose()?.unwrap_or(false);
        match (built_on, part.node.as_deref()) {
            (true, _) if root.is_some() => bail!("{}: {} is a second model on its skeleton", asset.path(), part.part),
            (true, _) => root = Some(part),
            (false, Some(_)) => waiting.push(part),
            // What is stored and hangs nowhere is the package's, and shows nowhere.
            (false, None) => {}
        }
    }
    let mut tree = match root.and_then(|part| part.fill.as_ref().map(|fill| (part, fill))) {
        Some((part, fill)) => {
            let model = resolved(project, asset, "model", fill.get("model"))?;
            model_of(project, &model, fill.get("overrides"), Some((&part.part, &part.within)))?
        }
        None => skeleton_of(&frame)?,
    };
    let joints: Vec<String> = tree.nodes.iter().map(|node| node.name.clone()).collect();
    for (at, node) in tree.nodes.iter_mut().enumerate() {
        // A mesh with no skeleton has its parts all under its own name: nothing to say which one is meant.
        if joints.iter().filter(|name| **name == node.name).count() == 1 {
            node.place = Some(Place::Pose(node.name.clone()));
            if let Some(pose) = asset.field("pose").and_then(|poses| poses.get(&node.name)) {
                posed(&mut tree.model.parts[at], pose);
            }
        }
    }
    tree.posed = asset.parts("pose").into_iter().map(|(name, _)| name.to_string()).collect();
    // A part may hang on one that comes later.
    while !waiting.is_empty() {
        let before = waiting.len();
        let mut later = Vec::new();
        for part in waiting {
            match tree.nodes.iter().position(|node| Some(node.name.as_str()) == part.node.as_deref()) {
                Some(on) => hang(project, asset, &mut tree, part, on)?,
                None => later.push(part),
            }
        }
        if later.len() == before {
            bail!("{}: {} hangs on nothing there is", asset.path(), later[0].part);
        }
        waiting = later;
    }
    Ok(tree)
}

/// Hang `child` under `node` of `tree`: its parts follow the tree's, and whatever it names by index moves with them.
pub fn attach(tree: &mut Model, mut child: Model, node: usize) {
    let (parts, materials, normals) = (tree.parts.len(), tree.materials.len(), tree.normals.len());
    let own_vertices: usize = tree.parts.iter().map(|part| part.vertices.len()).sum();
    let added: usize = child.parts.iter().map(|part| part.vertices.len()).sum();
    // Vertices count parts first: what was loose moves up by what the child adds.
    for corner in tree.faces.iter_mut().flat_map(|face| &mut face.corners) {
        if corner.vertex >= own_vertices {
            corner.vertex += added;
        }
    }
    for face in &mut child.faces {
        face.material += materials;
        face.part = face.part.map(|part| part + parts);
        face.normal = face.normal.map(|normal| normal + normals);
        for corner in &mut face.corners {
            corner.vertex += own_vertices;
            corner.normal = corner.normal.map(|normal| normal + normals);
        }
    }
    for (at, part) in child.parts.iter_mut().enumerate() {
        part.parent = if part.parent == at { node } else { part.parent + parts };
    }
    tree.parts.append(&mut child.parts);
    tree.normals.append(&mut child.normals);
    tree.materials.append(&mut child.materials);
    tree.faces.append(&mut child.faces);
    tree.loose_vertices.append(&mut child.loose_vertices);
}

/// # Errors
/// It is none of what shows in 3D, or something it refers to is missing or unreadable.
pub fn assemble(project: &dyn Project, asset: &Asset) -> Result<Assembled> {
    // A kind comes qualified by what registered it.
    let mut assembled = match asset.kind().and_then(|kind| kind.rsplit('@').next()) {
        Some("mesh") => mesh_of(project, asset),
        Some("skeleton") => skeleton_of(asset),
        Some("material") => material_of(project, asset),
        Some("model") => model_of(project, asset, None, None),
        Some("prefab") => prefab_of(project, asset),
        other => Err(anyhow!("{}: a {other:?} does not show in 3D", asset.path())),
    }?;
    for (material, slot) in assembled.model.materials.iter_mut().zip(&assembled.slots) {
        if let Some(drawn) = &slot.drawn {
            material.transparent = drawn.transparent;
            material.blend = Some(drawn.blend);
        }
    }
    assembled.model.effect = assembled.slots.iter().find_map(|slot| slot.drawn.as_ref()).map_or(0, |drawn| drawn.effect);
    Ok(assembled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Material;

    fn part(parent: usize, vertices: usize) -> Part {
        Part { parent, rotation: [[4096, 0, 0], [0, 4096, 0], [0, 0, 4096]], translation: [0; 3], vertices: vec![[1, 2, 3]; vertices] }
    }

    fn face(material: usize, part: usize, vertex: usize) -> Face {
        let corner = Corner { vertex, normal: Some(0), color: None, uv: None };
        Face { material, shading: Shading::Smooth, part: Some(part), normal: None, color: None, corners: vec![corner.clone(), corner.clone(), corner] }
    }

    fn material() -> Material {
        Material { transparent: false, slot: Some(0), blend: None }
    }

    #[test]
    fn a_child_hangs_under_its_node() {
        let mut tree = Model { parts: vec![part(0, 2), part(0, 3)], normals: vec![[0; 3]; 4], materials: vec![material(), material()], faces: vec![face(1, 1, 4)], ..Model::default() };
        let child = Model { parts: vec![part(0, 5)], normals: vec![[0; 3]; 2], materials: vec![material()], faces: vec![face(0, 0, 4)], ..Model::default() };
        attach(&mut tree, child, 1);
        assert_eq!(tree.parts.len(), 3);
        assert_eq!(tree.parts[2].parent, 1, "its root hangs under the node");
        let attached = &tree.faces[1];
        assert_eq!(attached.material, 2);
        assert_eq!(attached.part, Some(2));
        assert_eq!(attached.corners[0].vertex, 9, "after the five vertices the tree had");
        assert_eq!(attached.corners[0].normal, Some(4));
        assert_eq!(tree.faces[0].corners[0].vertex, 4, "what the tree had stays");
    }

    #[test]
    fn a_childs_own_tree_is_kept() {
        let mut tree = Model { parts: vec![part(0, 1)], ..Model::default() };
        let child = Model { parts: vec![part(0, 1), part(0, 1)], ..Model::default() };
        attach(&mut tree, child, 0);
        assert_eq!(tree.parts[1].parent, 0);
        assert_eq!(tree.parts[2].parent, 1, "a child's second part stays under its first");
    }

    #[test]
    fn vertices_that_were_loose_stay_after_the_parts() {
        let mut tree = Model { parts: vec![part(0, 1)], loose_vertices: vec![[0; 3]], faces: vec![face(0, 0, 1)], ..Model::default() };
        let child = Model { parts: vec![part(0, 2)], ..Model::default() };
        attach(&mut tree, child, 0);
        assert_eq!(tree.faces[0].corners[0].vertex, 3);
    }

    fn table(text: &str) -> Value {
        Value::Table(text.parse().unwrap())
    }

    fn resting() -> Part {
        Part { parent: 0, rotation: skeleton::matrix([0.0; 3]), translation: [1, 2, 3], vertices: Vec::new() }
    }

    #[test]
    fn a_pose_says_as_much_as_it_says() {
        let mut moved = resting();
        posed(&mut moved, &table("translation = [7, 8, 9]"));
        assert_eq!((moved.translation, moved.rotation), ([7, 8, 9], resting().rotation));
        posed(&mut moved, &table("rotation = [0.0, 0.0, 90.0]"));
        assert_eq!(moved.translation, [7, 8, 9]);
        assert_eq!(moved.rotation, skeleton::matrix([0.0, 0.0, 90.0]));
    }

    #[test]
    fn a_child_is_held_over_what_its_root_says() {
        let mut root = resting();
        held(&mut root, &table("translation = [10, 0, 0]\nscale = [2.0, 1.0, 1.0]"));
        assert_eq!(root.translation, [11, 2, 3]);
        assert_eq!(root.rotation, [[8192, 0, 0], [0, 4096, 0], [0, 0, 4096]]);
    }

    #[test]
    fn a_child_with_nothing_said_stays_as_it_is() {
        let mut root = resting();
        held(&mut root, &table("node = 'hand'"));
        assert_eq!((root.translation, root.rotation), (resting().translation, resting().rotation));
    }
}
