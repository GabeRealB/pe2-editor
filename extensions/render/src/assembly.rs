//! What a 3D view shows: a mesh, or a model with its materials filled, or a prefab's models in one tree. The game has
//! one coordinate node for a joint and an attachment alike, so a prefab comes out as one model, a child's root hanging
//! under the node it is attached to; posing and picking need to know nothing of how it was put together.
//!
//! It is what a node's `scene` output is, so that what reads it reads a value: `scene` makes it from what the graph
//! holds, read as any operation reads other nodes, and `assemble` asks for it.

use pe2_ext::{
    Asset, Call, Data, File, Project, Requested, Result, Source, Style, Table, Typed, Ui, Value, anyhow, bail,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::material::{self, Definition};
use crate::mesh;
use crate::model::{Blend, Corner, Face, Model, Part, Shading};
use crate::prefab::{self, Filled};
use crate::skeleton;

/// What fills a texture slot: a cell of an image, through a palette of its or the cell's own.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    pub name: String,
    /// The model attached here, for the root of one; a prefab's root and a joint have none.
    pub attached: Option<Attached>,
    /// Where its transform is written, where it can be.
    pub place: Option<Place>,
}

/// Where a node's transform is kept: a skeleton's own, or a prefab's for what it does to a joint or to what fills a part.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Place {
    Joint(usize),
    Pose(String),
    /// Where in the prefab what fills it is, step by step.
    Fill(Vec<String>),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attached {
    pub path: String,
    pub reference: String,
}

/// A material slot of what was assembled.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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

#[derive(Serialize, Deserialize)]
pub struct Assembled {
    pub model: Model,
    /// What a clip shows: the clip, and the part each of its tracks moves.
    #[serde(default)]
    pub clip: Option<Played>,
    /// The joints a prefab moves from where the skeleton has them.
    pub posed: Vec<String>,
    /// By the model's part.
    pub nodes: Vec<Node>,
    /// By the model's material.
    pub slots: Vec<Slot>,
}

/// A clip as a view plays it: its tracks, and by track the part of what is shown it moves (none where nothing there
/// is that joint).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Played {
    pub clip: crate::clip::Clip,
    pub parts: Vec<Option<usize>>,
}

/// What a scene is made from: the nodes it reaches, read as an operation reads them, and the member shown of each group.
struct Nodes<'c, 'a> {
    call: &'c Call<'a>,
    chosen: BTreeMap<String, String>,
}

/// A node reached, by what the graph makes of it.
struct Held<'c, 'a> {
    call: &'c Call<'a>,
    path: String,
    uid: Option<String>,
    /// What it is.
    node_type: Option<String>,
    settings: Table,
}

impl Held<'_, '_> {
    fn asset(&self) -> Asset<'_> {
        Asset::new(self, self.node_type.as_deref())
    }
}

impl Source for Held<'_, '_> {
    fn path(&self) -> &str {
        &self.path
    }

    fn uid(&self) -> Option<&str> {
        self.uid.as_deref()
    }

    fn fields(&self) -> &Table {
        &self.settings
    }

    fn declaration(&self) -> &'static str {
        ""
    }

    fn has_file(&self, name: &str) -> bool {
        self.read(name).is_ok()
    }

    /// The stored file a node makes as itself.
    fn read(&self, _: &str) -> Result<Vec<u8>> {
        let Data::Typed(stored) = self.call.output_of(&self.path, "native")? else {
            bail!("{}: it makes no file", self.path)
        };
        self.call.read(stored.file()?)
    }
}

impl<'c, 'a> Nodes<'c, 'a> {
    /// The node a reference names.
    fn named(&self, reference: &str) -> Result<Held<'c, 'a>> {
        let named = self.call.node_named(reference).ok_or_else(|| anyhow!("{reference}: no such node"))?;
        let settings = self.call.table_of(&named.path, "settings")?;
        Ok(Held {
            call: self.call,
            path: named.path,
            uid: named.uid,
            node_type: named.types.into_iter().next(),
            settings,
        })
    }

    fn shown(&self, asset: &Asset) -> Vec<prefab::Group> {
        prefab::groups_as(asset, |group| self.chosen.get(group).cloned())
    }
}

fn leaf(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn resolved<'c, 'a>(
    nodes: &Nodes<'c, 'a>,
    holder: &Asset,
    key: &str,
    reference: Option<&Value>,
) -> Result<Held<'c, 'a>> {
    let reference = reference.ok_or_else(|| anyhow!("{}: no {key}", holder.path()))?;
    let target = pe2_ext::target_of(reference.get("slot").unwrap_or(reference));
    let target = target.ok_or_else(|| anyhow!("{}: its {key} is no reference", holder.path()))?;
    nodes.named(target).map_err(|_| anyhow!("{}: its {key} is not in the project", holder.path()))
}

fn bound_of(nodes: &Nodes, holder: &Asset, texture: &Value) -> Result<Bound> {
    let image = resolved(nodes, holder, "image", Some(texture))?;
    let image = image.asset();
    let cell = material::cell_of(texture).ok_or_else(|| anyhow!("{}: a texture has no cell", holder.path()))?;
    let held = image
        .parts("cells")
        .into_iter()
        .find(|(name, _)| *name == cell)
        .ok_or_else(|| anyhow!("{}: {} has no cell {cell}", holder.path(), image.path()))?;
    let rect =
        held.1.get("rect").and_then(Value::as_array).ok_or_else(|| anyhow!("{}: {cell} has no size", image.path()))?;
    let side =
        |at: usize| rect.get(at).and_then(Value::as_integer).and_then(|side| u32::try_from(side).ok()).unwrap_or(1);
    Ok(Bound {
        image: image.path().to_string(),
        cell: cell.to_string(),
        palette: material::palette_of(texture).map(str::to_string),
        size: [side(2), side(3)],
    })
}

fn drawn_of<'a>(nodes: &Nodes, holder: &Asset, field: impl Fn(&str) -> Option<&'a Value>) -> Result<Drawn> {
    let read = Definition::read(&field);
    let texture = |key: &str| {
        let taken = read.shader.textures.iter().any(|(known, _)| *known == key);
        field(key).filter(|_| taken).map(|texture| bound_of(nodes, holder, texture)).transpose()
    };
    Ok(Drawn {
        transparent: read.transparent,
        blend: read.blend,
        main: texture(material::MAIN)?,
        environment: texture(material::ENVIRONMENT)?,
        strength: read.strength,
        effect: read.effect,
    })
}

/// A slot of a model, with what the prefab node holding it overrides: the same keys as a material's, and `material`.
fn slot_of(nodes: &Nodes, model: &Asset, name: &str, overrides: Option<&Value>) -> Result<Slot> {
    let of_model = model.field("materials").and_then(|materials| materials.get(name));
    let Some(reference) = overrides.and_then(|overrides| overrides.get("material")).or(of_model) else {
        return Ok(Slot::empty(name));
    };
    let held = resolved(nodes, model, name, Some(reference))?;
    let held = held.asset();
    let drawn =
        drawn_of(nodes, &held, |key| overrides.and_then(|overrides| overrides.get(key)).or_else(|| held.field(key)))?;
    let attached = Attached { path: held.path().to_string(), reference: held.uid().unwrap_or_default().to_string() };
    Ok(Slot {
        material: Some(attached),
        reference: Some(reference.clone()),
        drawn: Some(drawn),
        overrides: overrides.cloned(),
        ..Slot::empty(name)
    })
}

/// A mesh, posed by its skeleton where it has one, with its parts named.
fn mesh_of(nodes: &Nodes, mesh: &Asset) -> Result<Assembled> {
    let (mut model, names) = mesh::read(&String::from_utf8_lossy(&mesh.read(mesh::FILE)?))?;
    let slots = names.iter().map(|name| Slot::empty(name)).collect();
    let mut shown: Vec<Node> = (0..model.parts.len())
        .map(|_| Node { name: leaf(mesh.path()).to_string(), attached: None, place: None })
        .collect();
    if mesh.field("skeleton").is_some() {
        let skeleton = resolved(nodes, mesh, "skeleton", mesh.field("skeleton"))?;
        let skeleton = skeleton.asset();
        let joints = skeleton::of(&skeleton)?;
        mesh::pose(&mut model, &joints)?;
        shown = joints.into_iter().map(|joint| Node { name: joint.name, attached: None, place: None }).collect();
    }
    Ok(Assembled { posed: Vec::new(), model, clip: None, nodes: shown, slots })
}

fn model_of(
    nodes: &Nodes,
    asset: &Asset,
    overrides: Option<&Value>,
    node: Option<(&str, &[String])>,
) -> Result<Assembled> {
    let mesh = resolved(nodes, asset, "mesh", asset.field("mesh"))?;
    let mesh = mesh.asset();
    let mut assembled = mesh_of(nodes, &mesh)?;
    for slot in &mut assembled.slots {
        let overridden = overrides.and_then(|overrides| overrides.get(&slot.name));
        *slot = slot_of(nodes, asset, &slot.name, overridden)?;
        slot.node = node.map(|(name, within)| (name.to_string(), within.to_vec()));
    }
    Ok(assembled)
}

/// A skeleton alone: its joints, as parts without vertices.
fn skeleton_of(asset: &Asset) -> Result<Assembled> {
    let joints = skeleton::of(asset)?;
    let part = |(at, joint): (usize, &skeleton::Joint)| Part {
        parent: joint.parent.unwrap_or(at),
        rotation: skeleton::matrix(joint.rotation),
        translation: joint.translation,
        vertices: Vec::new(),
    };
    let parts = joints.iter().enumerate().map(part).collect();
    let named = joints.into_iter().enumerate();
    let nodes =
        named.map(|(at, joint)| Node { name: joint.name, attached: None, place: Some(Place::Joint(at)) }).collect();
    Ok(Assembled {
        posed: Vec::new(),
        model: Model { parts, ..Model::default() },
        clip: None,
        nodes,
        slots: Vec::new(),
    })
}

const BALL_RADIUS: f64 = 350.0;
const BALL_ROWS: usize = 24;
const BALL_COLUMNS: usize = 36;

/// A ball with a cell of `size` wrapped once around it, for a material to show on.
fn ball(size: Option<[u32; 2]>) -> Model {
    let across = BALL_COLUMNS + 1;
    let (mut vertices, mut normals) = (Vec::new(), Vec::new());
    #[expect(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        reason = "a dozen rows, of coordinates well within a short"
    )]
    for (row, column) in (0..=BALL_ROWS).flat_map(|row| (0..=BALL_COLUMNS).map(move |column| (row, column))) {
        let (down, around) = (
            std::f64::consts::PI * row as f64 / BALL_ROWS as f64,
            std::f64::consts::TAU * column as f64 / BALL_COLUMNS as f64,
        );
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
    let corner = |vertex: usize| Corner {
        vertex,
        normal: Some(vertex),
        color: size.is_none().then_some([200, 200, 205]),
        uv: uv(vertex),
    };
    let face = |(row, column): (usize, usize)| {
        let at = row * across + column;
        Face {
            material: 0,
            shading: Shading::Smooth,
            part: Some(0),
            normal: None,
            color: None,
            corners: vec![corner(at), corner(at + 1), corner(at + across + 1), corner(at + across)],
        }
    };
    let faces = (0..BALL_ROWS).flat_map(|row| (0..BALL_COLUMNS).map(move |column| (row, column))).map(face).collect();
    let part = Part { parent: 0, rotation: skeleton::matrix([0.0; 3]), translation: [0, -500, 0], vertices };
    Model {
        parts: vec![part],
        normals,
        materials: vec![crate::model::Material { transparent: false, slot: None, blend: None }],
        faces,
        ..Model::default()
    }
}

/// A material alone, on a ball.
fn material_of(nodes: &Nodes, asset: &Asset) -> Result<Assembled> {
    let drawn = drawn_of(nodes, asset, |key| asset.field(key))?;
    let model = ball(drawn.main.as_ref().map(|main| main.size));
    let slot = Slot { drawn: Some(drawn), ..Slot::empty(leaf(asset.path())) };
    Ok(Assembled {
        posed: Vec::new(),
        model,
        clip: None,
        nodes: vec![Node { name: leaf(asset.path()).to_string(), attached: None, place: None }],
        slots: vec![slot],
    })
}

/// Whether what fills a part is built on a skeleton: its parts are that skeleton's joints, not something to hang on them.
fn on_skeleton(nodes: &Nodes, asset: &Asset, skeleton: &Asset, fill: &Value) -> Result<bool> {
    let model = resolved(nodes, asset, "model", fill.get("model"))?;
    let model = model.asset();
    let mesh = resolved(nodes, &model, "mesh", model.field("mesh"))?;
    let mesh = mesh.asset();
    let named =
        mesh.field("skeleton").map(|reference| resolved(nodes, &mesh, "skeleton", Some(reference))).transpose()?;
    Ok(named.is_some_and(|named| named.path == skeleton.path()))
}

/// A part's own node, under the node it hangs on.
fn hang(nodes: &Nodes, asset: &Asset, tree: &mut Assembled, part: &Filled, on: usize) -> Result<()> {
    let place = Some(Place::Fill(part.within.clone()));
    let Some(fill) = &part.fill else {
        // Open: a place to see and to move, with nothing there yet.
        let rest = Part { parent: 0, rotation: skeleton::matrix([0.0; 3]), translation: [0; 3], vertices: Vec::new() };
        attach(&mut tree.model, Model { parts: vec![rest], ..Model::default() }, on);
        tree.nodes.push(Node { name: part.part.clone(), attached: None, place });
        return Ok(());
    };
    let model = resolved(nodes, asset, "model", fill.get("model"))?;
    let model = model.asset();
    let Assembled { model: mut built, mut nodes, mut slots, .. } =
        model_of(nodes, &model, fill.get("overrides"), Some((&part.part, &part.within)))?;
    if let Some(root) = built.parts.first_mut() {
        skeleton::held(root, fill);
    }
    // What it hangs by is the part; a part of it deeper down keeps its own name.
    if let Some(first) = nodes.first_mut() {
        part.part.clone_into(&mut first.name);
        first.attached =
            Some(Attached { path: model.path().to_string(), reference: model.uid().unwrap_or_default().to_string() });
        first.place = place;
    }
    attach(&mut tree.model, built, on);
    tree.nodes.append(&mut nodes);
    tree.slots.append(&mut slots);
    Ok(())
}

fn prefab_of(nodes: &Nodes, asset: &Asset) -> Result<Assembled> {
    let frame = resolved(nodes, asset, "skeleton", asset.field("skeleton"))?;
    let frame = frame.asset();
    let parts = prefab::filled_as(asset, &nodes.shown(asset))?;
    let mut waiting: Vec<&Filled> = Vec::new();
    let mut root = None;
    for part in
        parts.iter().filter(|part| part.node.is_some() || part.within.first().is_some_and(|step| step == "groups"))
    {
        let built_on =
            part.fill.as_ref().map(|fill| on_skeleton(nodes, asset, &frame, fill)).transpose()?.unwrap_or(false);
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
            let model = resolved(nodes, asset, "model", fill.get("model"))?;
            model_of(nodes, &model.asset(), fill.get("overrides"), Some((&part.part, &part.within)))?
        }
        None => skeleton_of(&frame)?,
    };
    let joints: Vec<String> = tree.nodes.iter().map(|node| node.name.clone()).collect();
    for (at, node) in tree.nodes.iter_mut().enumerate() {
        // A mesh with no skeleton has its parts all under its own name: nothing to say which one is meant.
        if joints.iter().filter(|name| **name == node.name).count() == 1 {
            node.place = Some(Place::Pose(node.name.clone()));
            if let Some(pose) = asset.field("pose").and_then(|poses| poses.get(&node.name)) {
                skeleton::posed(&mut tree.model.parts[at], pose);
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
                Some(on) => hang(nodes, asset, &mut tree, part, on)?,
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

/// A clip, on what it moves: the prefab built on its skeleton, a weapon's clip with that weapon in hand where the prefab
/// has it, else the skeleton's joints alone.
fn clip_of(nodes: &Nodes, asset: &Asset) -> Result<Assembled> {
    let clip = crate::clip_usd::read(&String::from_utf8_lossy(&asset.read(crate::clip_usd::FILE)?))?;
    let skeleton = resolved(nodes, asset, "skeleton", asset.field("skeleton"))?;
    let joints = skeleton::of(&skeleton.asset())?;
    let mut prefabs = nodes.call.assets_of("prefab")?.into_iter().filter_map(|(path, declaration)| {
        let table: Table = declaration.parse().ok()?;
        let named = table.get("skeleton").and_then(pe2_ext::target_of)?;
        let same = skeleton.uid.as_deref() == Some(named)
            || named.trim_start_matches("res://").trim_end_matches(".node") == skeleton.path;
        same.then_some(path)
    });
    let mut tree = match prefabs.next() {
        Some(path) => {
            let prefab = nodes.named(&path)?;
            // A weapon's clips (`weapons/<weapon>/...`) show it held.
            let mut chosen = nodes.chosen.clone();
            if let Some(weapon) = asset.path().strip_prefix("weapons/").and_then(|rest| rest.split('/').next()) {
                chosen.entry("weapon".to_string()).or_insert_with(|| weapon.to_string());
            }
            prefab_of(&Nodes { call: nodes.call, chosen }, &prefab.asset())?
        }
        None => skeleton_of(&skeleton.asset())?,
    };
    if clip.tracks.len() != joints.len() {
        bail!("{}: {} tracks for {} joints", asset.path(), clip.tracks.len(), joints.len());
    }
    let parts = joints.iter().map(|joint| tree.nodes.iter().position(|node| node.name == joint.name)).collect();
    tree.clip = Some(Played { clip, parts });
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

/// What a node shows in 3D, the member `chosen` of each of its groups.
///
/// # Errors
/// It is none of what shows in 3D, or something it refers to is missing or unreadable.
pub fn scene(call: &mut Call) -> Result<Typed> {
    let chosen: String = call.parameter()?;
    let chosen = chosen
        .split(',')
        .filter_map(|pair| pair.split_once('='))
        .map(|(group, member)| (group.to_string(), member.to_string()));
    let nodes = Nodes { call, chosen: chosen.collect() };
    let own = nodes.named(call.host.path())?;
    let assembled = built(&nodes, &own.asset())?;
    let file = File::new(FILE, serde_json::to_vec(&assembled)?);
    Ok(Typed::new("pe2.render@scene", vec![file]))
}

/// The file a scene is held in.
const FILE: &str = "scene.json";

/// No group chosen, as an address has to say something.
const NONE_CHOSEN: &str = "-";

fn built(nodes: &Nodes, asset: &Asset) -> Result<Assembled> {
    // A type comes qualified by what registered it.
    let mut assembled = match asset.node_type().and_then(|node_type| node_type.rsplit('@').next()) {
        Some("mesh") => mesh_of(nodes, asset),
        Some("skeleton") => skeleton_of(asset),
        Some("material") => material_of(nodes, asset),
        Some("model") => model_of(nodes, asset, None, None),
        Some("prefab") => prefab_of(nodes, asset),
        Some("clip") => clip_of(nodes, asset),
        other => Err(anyhow!("{}: a {other:?} does not show in 3D", asset.path())),
    }?;
    for (material, slot) in assembled.model.materials.iter_mut().zip(&assembled.slots) {
        if let Some(drawn) = &slot.drawn {
            material.transparent = drawn.transparent;
            material.blend = Some(drawn.blend);
        }
    }
    assembled.model.effect =
        assembled.slots.iter().find_map(|slot| slot.drawn.as_ref()).map_or(0, |drawn| drawn.effect);
    Ok(assembled)
}

/// A node's scene, as it is being looked at: its output, the members chosen of its groups.
///
/// # Errors
/// It has no such output, or it could not be made.
pub fn assemble(project: &dyn Project, asset: &Asset) -> Result<Assembled> {
    let address = scene_address(project, asset);
    let bytes =
        project.bytes(asset.path(), &address).ok_or_else(|| anyhow!("{}: it shows nothing in 3D", asset.path()))??;
    Ok(serde_json::from_slice(&bytes)?)
}

/// A scene as a frame can ask for it, not waiting for it to be made.
pub enum Scene {
    Ready(Box<Assembled>),
    /// On its way, for so many milliseconds.
    Waiting(u32),
    Failed(String),
}

/// What `assemble` gives, without waiting: a view shows what it has meanwhile, and is shown again when this is there.
pub fn requested(project: &dyn Project, asset: &Asset) -> Scene {
    match project.request(asset.path(), &scene_address(project, asset)) {
        Requested::Ready(bytes) => serde_json::from_slice(&bytes)
            .map_or_else(|why| Scene::Failed(why.to_string()), |made| Scene::Ready(Box::new(made))),
        Requested::Pending { waited } => Scene::Waiting(waited),
        Requested::Failed(why) => Scene::Failed(why),
    }
}

/// What a view says while it waits: nothing at first, as most are there before anyone could read it.
pub fn say_waiting(ui: &mut Ui, waited: u32) {
    const SAY_AFTER: u32 = 100;
    if waited >= SAY_AFTER {
        ui.styled("Making what it shows...", Style::Note);
    }
}

fn scene_address(project: &dyn Project, asset: &Asset) -> String {
    let chosen: Vec<String> =
        prefab::groups(asset, project).iter().map(|group| format!("{}={}", group.name, group.chosen)).collect();
    let chosen = if chosen.is_empty() { NONE_CHOSEN.to_string() } else { chosen.join(",") };
    format!("scene/{chosen}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Material;

    fn part(parent: usize, vertices: usize) -> Part {
        Part {
            parent,
            rotation: [[4096, 0, 0], [0, 4096, 0], [0, 0, 4096]],
            translation: [0; 3],
            vertices: vec![[1, 2, 3]; vertices],
        }
    }

    fn face(material: usize, part: usize, vertex: usize) -> Face {
        let corner = Corner { vertex, normal: Some(0), color: None, uv: None };
        Face {
            material,
            shading: Shading::Smooth,
            part: Some(part),
            normal: None,
            color: None,
            corners: vec![corner.clone(), corner.clone(), corner],
        }
    }

    fn material() -> Material {
        Material { transparent: false, slot: Some(0), blend: None }
    }

    #[test]
    fn a_child_hangs_under_its_node() {
        let mut tree = Model {
            parts: vec![part(0, 2), part(0, 3)],
            normals: vec![[0; 3]; 4],
            materials: vec![material(), material()],
            faces: vec![face(1, 1, 4)],
            ..Model::default()
        };
        let child = Model {
            parts: vec![part(0, 5)],
            normals: vec![[0; 3]; 2],
            materials: vec![material()],
            faces: vec![face(0, 0, 4)],
            ..Model::default()
        };
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
        let mut tree = Model {
            parts: vec![part(0, 1)],
            loose_vertices: vec![[0; 3]],
            faces: vec![face(0, 0, 1)],
            ..Model::default()
        };
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
        skeleton::posed(&mut moved, &table("translation = [7, 8, 9]"));
        assert_eq!((moved.translation, moved.rotation), ([7, 8, 9], resting().rotation));
        skeleton::posed(&mut moved, &table("rotation = [0.0, 0.0, 90.0]"));
        assert_eq!(moved.translation, [7, 8, 9]);
        assert_eq!(moved.rotation, skeleton::matrix([0.0, 0.0, 90.0]));
    }

    #[test]
    fn a_child_is_held_over_what_its_root_says() {
        let mut root = resting();
        skeleton::held(&mut root, &table("translation = [10, 0, 0]\nscale = [2.0, 1.0, 1.0]"));
        assert_eq!(root.translation, [11, 2, 3]);
        assert_eq!(root.rotation, [[8192, 0, 0], [0, 4096, 0], [0, 0, 4096]]);
    }

    #[test]
    fn a_child_with_nothing_said_stays_as_it_is() {
        let mut root = resting();
        skeleton::held(&mut root, &table("node = 'hand'"));
        assert_eq!((root.translation, root.rotation), (resting().translation, resting().rotation));
    }
}
