//! What is inside a model, as every pane shows it: its bones as a tree, each a part of the asset, selected as `bones/<n>` wherever it is.

use pe2_ext::{Asset, ContentAction, ContentOutcome, ContentRequest, ContentRow, Contents, Icon, PartField, PartSheet, Project, Result, Tint, Value, anyhow, bail};

use crate::assembly::{self, Assembled, Place};
use crate::material::{self, Definition};
use crate::model::{Model, Shading, Source};
use crate::prefab;
use crate::skeleton;

/// What a node's row starts with: the 3D view selects by it.
pub const BONE: &str = "bones/";

/// A node's index, from what it is selected as.
pub fn bone_of(part: &str) -> Option<usize> {
    part.strip_prefix('#').unwrap_or(part).strip_prefix(BONE)?.parse().ok()
}

/// The faces of a part, those spanning parts by their first corner's, as selecting it shows them everywhere.
fn faces_of(model: &Model, part: usize) -> usize {
    let owners = model.owners();
    model.faces.iter().filter(|face| face.part_in(&owners) == Some(part)).count()
}

fn counted(model: &Model, part: usize) -> String {
    let (vertices, faces) = (model.parts[part].vertices.len(), faces_of(model, part));
    if vertices == 0 && faces == 0 { String::new() } else { format!("{vertices}v  {faces}f") }
}

/// The nodes under one, depth first: what is attached reads as what it is, under where it hangs.
fn node_rows(assembled: &Assembled, index: usize, depth: u32, rows: &mut Vec<ContentRow>) {
    let node = &assembled.nodes[index];
    // A model's own name is long enough: what it counts is in the inspector.
    let (icon, tint, detail) = if node.attached.is_some() { (Icon::Package, Tint::Picture, String::new()) } else { (Icon::Bone, Tint::Structure, counted(&assembled.model, index)) };
    let mut row = ContentRow::new(&format!("{BONE}{index}"), &node.name).detail(&detail).depth(depth).icon(icon, tint);
    match &node.place {
        Some(Place::Fill(_)) if node.attached.is_some() => {
            row = row.drags(ATTACHMENT).takes(&[ATTACHMENT, "asset:model"]).action(ContentAction::new(DETACH, "Detach").icon(Icon::Cross).on_row());
        }
        Some(Place::Fill(_)) => {
            row = row.detail("open").drags(ATTACHMENT).takes(&[ATTACHMENT, "asset:model"]).action(ContentAction::new(REMOVE, "Remove the part").icon(Icon::Cross).on_row());
        }
        Some(Place::Pose(name)) => {
            row = row.takes(&[ATTACHMENT, "asset:model"]);
            if assembled.posed.contains(name) {
                row = row.detail(&format!("{detail}  moved")).action(ContentAction::new(REVERT, "Revert to the skeleton").icon(Icon::Cross).on_row());
            }
        }
        _ => {}
    }
    rows.push(row);
    for (child, part) in assembled.model.parts.iter().enumerate() {
        if child != index && part.parent == index {
            node_rows(assembled, child, depth + 1, rows);
        }
    }
}

const ATTACHMENT: &str = "attachment";
const DETACH: &str = "detach";
const REMOVE: &str = "remove";
const REVERT: &str = "revert";

/// A material's shape: its shader's slots, and what fills each.
fn material_contents(asset: &Asset) -> Contents {
    let read = Definition::read(|key| asset.field(key));
    let mut rows = vec![ContentRow::heading("shader", read.shader.label)];
    for (key, label) in read.shader.textures {
        let held = asset.field(key);
        let detail = held.map_or_else(|| "Empty".to_string(), |texture| {
            let cell = material::cell_of(texture).unwrap_or("?");
            material::palette_of(texture).map_or_else(|| cell.to_string(), |palette| format!("{cell}  {palette}"))
        });
        rows.push(ContentRow::new(&format!("textures/{key}"), label).detail(&detail).icon(Icon::Image, Tint::Picture));
    }
    Contents { rows, stack: false, ..Contents::default() }
}

fn is_material(asset: &Asset) -> bool {
    asset.kind().and_then(|kind| kind.rsplit('@').next()) == Some("material")
}

/// What every kind that shows in 3D shows here: the tree it is.
pub fn contents(asset: &Asset, project: &dyn Project) -> Contents {
    if is_material(asset) {
        return material_contents(asset);
    }
    let assembled = match assembly::assemble(project, asset) {
        Ok(assembled) => assembled,
        Err(why) => return Contents { problem: Some(format!("{why:#}")), stack: false, ..Contents::default() },
    };
    let mut rows = vec![ContentRow::heading("hierarchy", "Hierarchy")];
    for (index, part) in assembled.model.parts.iter().enumerate() {
        if part.parent == index {
            node_rows(&assembled, index, 0, &mut rows);
        }
    }
    Contents { rows, stack: false, ..Contents::default() }
}

fn node_at(assembled: &Assembled, row: &str) -> Result<usize> {
    bone_of(row).filter(|index| *index < assembled.nodes.len()).ok_or_else(|| anyhow!("{row} is no node"))
}

/// What fills a part goes, and what held it with it: the part stays, open, as the code still asks for it.
fn detach(asset: &Asset, name: &str, within: &[String]) -> ContentOutcome {
    let label = format!("Detach {name}");
    if within.first().is_some_and(|step| step == "groups") {
        return ContentOutcome { label, sets: vec![(within.to_vec(), None)], select: None };
    }
    let mut part = pe2_ext::Table::new();
    if let Some(node) = prefab::fill_at(asset, within).and_then(|part| part.get("node")) {
        part.insert("node".to_string(), node.clone());
    }
    ContentOutcome { label, sets: vec![(within.to_vec(), Some(Value::Table(part)))], select: None }
}

/// An open part goes, where nothing depends on it.
fn remove(asset: &Asset, project: &dyn Project, name: &str, within: &[String]) -> Result<ContentOutcome> {
    if prefab::filled(asset, project)?.iter().any(|part| part.node.as_deref() == Some(name)) {
        bail!("something hangs on {name}");
    }
    let members = asset.parts("groups").into_iter().filter_map(|(_, members)| members.as_table()).flat_map(|members| members.values());
    if members.into_iter().any(|member| member.get(name).is_some()) {
        bail!("a member of a group fills {name}");
    }
    Ok(ContentOutcome { label: format!("Remove {name}"), sets: vec![(within.to_vec(), None)], select: None })
}

fn act_on_node(asset: &Asset, project: &dyn Project, assembled: &Assembled, row: &str, action: &str) -> Result<ContentOutcome> {
    let node = &assembled.nodes[node_at(assembled, row)?];
    match (&node.place, action) {
        (Some(Place::Fill(within)), DETACH) => Ok(detach(asset, &node.name, within)),
        (Some(Place::Fill(within)), REMOVE) => remove(asset, project, &node.name, within),
        (Some(Place::Pose(name)), REVERT) => {
            let mut poses = asset.field("pose").and_then(Value::as_table).cloned().unwrap_or_default();
            poses.remove(name);
            let sets = vec![(vec!["pose".to_string()], (!poses.is_empty()).then_some(Value::Table(poses)))];
            Ok(ContentOutcome { label: format!("Revert {name} to the skeleton"), sets, select: Some(row.to_string()) })
        }
        _ => bail!("no action {action} on {}", node.name),
    }
}

/// A part moved onto another node: what fills it comes along.
fn reattach(asset: &Asset, project: &dyn Project, assembled: &Assembled, row: &str, onto: &str) -> Result<ContentOutcome> {
    let (node, target) = (&assembled.nodes[node_at(assembled, row)?], &assembled.nodes[node_at(assembled, onto)?]);
    let (Some(Place::Fill(_)), Some(Place::Pose(_) | Place::Fill(_))) = (&node.place, &target.place) else { bail!("only a part moves, and onto a joint or a part") };
    if prefab::hangs_from(&prefab::filled(asset, project)?, &target.name, &node.name) {
        bail!("{} hangs from {}", target.name, node.name);
    }
    let path = vec!["parts".to_string(), node.name.clone(), "node".to_string()];
    Ok(ContentOutcome { label: format!("Attach {} to {}", node.name, target.name), sets: vec![(path, Some(target.name.as_str().into()))], select: None })
}

/// A part's name from a model's, kept apart from those there are.
fn new_part(asset: &Asset, name: &str) -> String {
    let taken = |candidate: &str| asset.field("parts").is_some_and(|parts| parts.get(candidate).is_some());
    let held = asset.parts("parts").len() + 1;
    (1..=held).map(|at| if at == 1 { name.to_string() } else { format!("{name}_{at}") }).find(|candidate| !taken(candidate)).unwrap_or_else(|| name.to_string())
}

/// A model from the assets: it fills an open part, or becomes a new part on what it was dropped on.
fn attach_model(asset: &Asset, project: &dyn Project, assembled: &Assembled, onto: &str, path: &str) -> Result<ContentOutcome> {
    let target = &assembled.nodes[node_at(assembled, onto)?];
    let dropped = project.asset(path).ok_or_else(|| anyhow!("{path} is not in the project"))?;
    let uid = dropped.uid().ok_or_else(|| anyhow!("{path} has no uid"))?;
    let name = path.rsplit('/').next().unwrap_or(path);
    if let (Some(Place::Fill(within)), None) = (&target.place, &target.attached) {
        let mut at = within.clone();
        at.push("model".to_string());
        return Ok(ContentOutcome { label: format!("Fill {} with {name}", target.name), sets: vec![(at, Some(reference(uid)))], select: None });
    }
    let mut part = pe2_ext::Table::new();
    part.insert("node".to_string(), target.name.as_str().into());
    part.insert("model".to_string(), reference(uid));
    let at = vec!["parts".to_string(), new_part(asset, name)];
    Ok(ContentOutcome { label: format!("Attach {name} to {}", target.name), sets: vec![(at, Some(Value::Table(part)))], select: None })
}

/// Something done in a prefab's contents.
pub fn act(asset: &Asset, project: &dyn Project, request: ContentRequest) -> Result<ContentOutcome> {
    let assembled = assembly::assemble(project, asset)?;
    match request {
        ContentRequest::Action { row, action } => act_on_node(asset, project, &assembled, &row, &action),
        ContentRequest::Moved { row, onto, .. } => reattach(asset, project, &assembled, &row, &onto),
        ContentRequest::Dropped { row, asset: path } => attach_model(asset, project, &assembled, &row, &path),
        ContentRequest::Rename { .. } => bail!("its contents are not renamed"),
    }
}

/// Euler angles in degrees, the order the game applies them: Rx Ry Rz.
pub fn euler_degrees(m: &[[f32; 3]; 3]) -> [f32; 3] {
    let y = m[0][2].clamp(-1.0, 1.0).asin();
    let (x, z) = if m[0][2].abs() < 0.999_9 { ((-m[1][2]).atan2(m[2][2]), (-m[0][1]).atan2(m[0][0])) } else { (m[2][1].atan2(m[1][1]), 0.0) };
    [x.to_degrees(), y.to_degrees(), z.to_degrees()]
}

fn field(key: &str, label: &str, value: impl Into<Value>) -> PartField {
    PartField { key: key.to_string(), label: label.to_string(), value: Some(value.into()), absent: None, choices: Vec::new(), read_only: true }
}

/// Three numbers, as the inspector lays them out by axis; a tenth rounded to nothing reads as nothing.
fn axes(values: [f32; 3]) -> Value {
    Value::Array(values.map(|value| Value::Float(f64::from((value * 10.0).round() / 10.0 + 0.0))).to_vec())
}

fn whole(values: [i32; 3]) -> Value {
    Value::Array(values.map(|value| Value::Integer(i64::from(value))).to_vec())
}

fn reference(uid: &str) -> Value {
    let mut table = pe2_ext::Table::new();
    table.insert("slot".to_string(), Value::String(uid.to_string()));
    Value::Table(table)
}

/// A vector's field, editable where it is the node's to change.
fn vector(key: &str, label: &str, value: Value, editable: bool) -> PartField {
    PartField { read_only: !editable, ..field(key, label, value) }
}

fn node_sheet(assembled: &Assembled, asset: &Asset, index: usize) -> Option<PartSheet> {
    let model = &assembled.model;
    let (node, part) = (assembled.nodes.get(index)?, model.parts.get(index)?);
    let parent = if part.parent == index { "None (root)".to_string() } else { assembled.nodes.get(part.parent)?.name.clone() };
    let matrix = part.rotation.map(|row| row.map(|x| f32::from(x) / 4096.0));
    let length = |axis: usize| (0..3).map(|row| matrix[row][axis] * matrix[row][axis]).sum::<f32>().sqrt();
    let mut fields = vec![field("name", "Name", node.name.clone()), field("parent", "Parent", parent)];
    if let Some(attached) = &node.attached {
        fields.push(PartField { read_only: false, ..field("model", "Model", reference(&attached.reference)) });
    }
    let mut translation = whole(part.translation);
    let (mut rotation, mut scale) = (axes(euler_degrees(&matrix)), axes([length(0), length(1), length(2)]));
    let within = match &node.place {
        Some(Place::Joint(at)) => vec!["joints".to_string(), at.to_string()],
        Some(Place::Pose(name)) => vec!["pose".to_string(), name.clone()],
        Some(Place::Fill(within)) => {
            // What the prefab holds it by, not where that puts it.
            let fill = prefab::fill_at(asset, within)?;
            translation = whole(skeleton::translation_of(fill).unwrap_or_default());
            #[expect(clippy::cast_possible_truncation, reason = "what is shown of a number stored wider")]
            let held = |key: &str, none: f64| axes(skeleton::numbers_of(fill, key).unwrap_or([none; 3]).map(|x| x as f32));
            (rotation, scale) = (held("rotation", 0.0), held("scale", 1.0));
            within.clone()
        }
        None => Vec::new(),
    };
    let moves = node.place.is_some();
    fields.push(vector("translation", "Translation", translation, moves));
    fields.push(vector("rotation", "Rotation", rotation, moves));
    fields.push(vector("scale", "Scale", scale, matches!(node.place, Some(Place::Fill(_)))));
    if !part.vertices.is_empty() {
        fields.push(field("vertices", "Vertices", i64::try_from(part.vertices.len()).ok()?));
        fields.push(field("faces", "Faces", i64::try_from(faces_of(model, index)).ok()?));
    }
    Some(PartSheet { within, fields, problem: None })
}

/// What a part says of itself in the inspector.
pub fn part_sheet(asset: &Asset, project: &dyn Project, part: &str) -> Option<PartSheet> {
    let assembled = assembly::assemble(project, asset).ok()?;
    node_sheet(&assembled, asset, bone_of(part)?)
}

/// What the model is, counted.
#[must_use]
pub fn summary(model: &Model) -> Vec<(&'static str, String)> {
    let of_shading = |shading: Shading| model.faces.iter().filter(|face| face.shading == shading).count();
    let of_source = |source: Source| model.faces.iter().filter(|face| face.source() == source).count();
    let cornered = |corners: usize| model.faces.iter().filter(|face| face.corners.len() == corners).count();
    let vertices: usize = model.parts.iter().map(|part| part.vertices.len()).sum();
    let seams = model.faces.iter().filter(|face| face.part.is_none()).count();
    let mut lines = vec![
        ("Parts", model.parts.len().to_string()),
        ("Vertices", vertices.to_string()),
        ("Normals", model.normals.len().to_string()),
        ("Faces", model.faces.len().to_string()),
        ("Triangles", cornered(3).to_string()),
        ("Quads", cornered(4).to_string()),
        ("Spanning parts", seams.to_string()),
        ("Smooth", of_shading(Shading::Smooth).to_string()),
        ("Flat", of_shading(Shading::Flat).to_string()),
        ("Unlit", of_shading(Shading::Unlit).to_string()),
        ("Textured", of_source(Source::Textured).to_string()),
        ("Vertex colors", of_source(Source::Vertex).to_string()),
        ("Flat color", of_source(Source::Flat).to_string()),
        ("Plain", of_source(Source::Plain).to_string()),
        ("Material slots", model.materials.len().to_string()),
    ];
    if !model.loose_vertices.is_empty() {
        lines.push(("Loose vertices", model.loose_vertices.len().to_string()));
    }
    if model.effect != 0 {
        lines.push(("Effect", model.effect.to_string()));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn angles_come_back_through_a_matrix() {
        let degrees = [12.5, -30.0, 70.0];
        let matrix = crate::skeleton::matrix(degrees).map(|row| row.map(|x| f32::from(x) / 4096.0));
        for (back, was) in euler_degrees(&matrix).into_iter().zip(degrees) {
            assert!((f64::from(back) - was).abs() < 0.05, "{back} from {was}");
        }
    }
}
