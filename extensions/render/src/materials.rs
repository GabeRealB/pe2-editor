//! What a model's or a prefab's faces are drawn with, in the inspector: each material once, with its texture, however many
//! nodes of a prefab draw with it, narrowed to what the part selected uses. A material dropped on a row fills its slot: a
//! model's own, or a prefab's override for every node the row stands for, one alone once it is selected. An override is
//! also where a texture of its own goes, and what Revert takes away, and what is overridden has a row of its own.

use pe2_ext::{Asset, Icon, Image, Project, Style, Table, Texture, Ui, Value, View};

use crate::assembly::{self, Assembled, Slot};
use crate::material;
use crate::material_panel::{Pictures, choices_of, with};
use crate::parts;

/// How wide and tall a slot's picture is at most.
const THUMBNAIL: f32 = 56.0;

fn same_material(a: &Slot, b: &Slot) -> bool {
    a.name == b.name && a.material == b.material && a.node == b.node
}

/// A material shows once, however many of the models it fills a slot of; a prefab's, by the node that can override it.
fn first_of(assembled: &Assembled, slot: &Slot) -> usize {
    assembled.slots.iter().position(|other| same_material(other, slot)).unwrap_or(0)
}

/// The faces of what fills a slot, of one part where one is said.
fn faces_with(assembled: &Assembled, slot: &Slot, part: Option<usize>) -> usize {
    let owners = assembled.model.owners();
    let same = |index: usize| assembled.slots.get(index).is_some_and(|other| same_material(other, slot));
    let on_part = |face: &&crate::model::Face| part.is_none() || face.part_in(&owners) == part;
    assembled.model.faces.iter().filter(on_part).filter(|face| same(face.material)).count()
}

/// The slots to list: each once, and of a part those its faces use.
fn listed(assembled: &Assembled, part: Option<usize>) -> Vec<usize> {
    let slots = assembled.slots.iter().enumerate();
    slots.filter(|(index, slot)| first_of(assembled, slot) == *index && (part.is_none() || faces_with(assembled, slot, part) > 0)).map(|(index, _)| index).collect()
}

/// Whether two nodes' slots read as one row: a node's own override makes it its own.
fn one_row(a: &Slot, b: &Slot) -> bool {
    a.name == b.name && a.material == b.material && a.reference == b.reference && a.overrides.is_none() && b.overrides.is_none()
}

/// The rows: each the slots it stands for.
fn rows(assembled: &Assembled, part: Option<usize>) -> Vec<Vec<usize>> {
    let mut rows: Vec<Vec<usize>> = Vec::new();
    for index in listed(assembled, part) {
        match rows.iter_mut().find(|row| one_row(&assembled.slots[row[0]], &assembled.slots[index])) {
            Some(row) => row.push(index),
            None => rows.push(vec![index]),
        }
    }
    rows
}

/// What was asked of a slot.
enum Change {
    Material(Value),
    OverrideTexture,
    Texture(Value),
    Revert,
}

/// A node's overrides with a slot's replaced: the last of a slot's going takes the slot with it.
fn overrides_with(mut all: Table, slot: &str, entry: Table) -> Table {
    if entry.is_empty() {
        all.remove(slot);
    } else {
        all.insert(slot.to_string(), Value::Table(entry));
    }
    all
}

/// A prefab's children with one's overrides replaced: none left, none written.
fn children_with(mut children: Vec<Value>, at: usize, overrides: Table) -> Option<Vec<Value>> {
    let child = children.get_mut(at)?.as_table_mut()?;
    if overrides.is_empty() {
        child.remove("overrides");
    } else {
        child.insert("overrides".to_string(), Value::Table(overrides));
    }
    Some(children)
}

/// A slot's texture as the material behind it holds it: what an override starts from.
fn texture_of(project: &dyn Project, slot: &Slot) -> Option<Value> {
    let reference = slot.reference.as_ref()?;
    project.resolve(reference.get("slot").unwrap_or(reference))?.field(material::MAIN).cloned()
}

/// What a slot comes to in a node's overrides.
fn entry_with(project: &dyn Project, slot: &Slot, mut entry: Table, change: &Change) -> Option<Table> {
    let set = match change {
        Change::Material(new) => ("material", filled(new)),
        Change::OverrideTexture => (material::MAIN, texture_of(project, slot)?),
        Change::Texture(new) => (material::MAIN, new.clone()),
        Change::Revert => return Some(Table::new()),
    };
    entry.insert(set.0.to_string(), set.1);
    Some(entry)
}

/// What fills a slot: as written, or a dropped asset's uid made a reference.
fn filled(new: &Value) -> Value {
    let Value::String(uid) = new else { return new.clone() };
    let mut table = Table::new();
    table.insert("slot".to_string(), uid.as_str().into());
    Value::Table(table)
}

/// The fields a change of a row writes, and what they come to.
fn written(asset: &Asset, project: &dyn Project, slots: &[&Slot], change: &Change) -> Vec<(&'static str, Option<Value>)> {
    let name = &slots[0].name;
    if slots[0].node.is_none() {
        let Change::Material(new) = change else { return Vec::new() };
        let mut materials = asset.field("materials").and_then(Value::as_table).cloned().unwrap_or_default();
        materials.insert(name.clone(), filled(new));
        return vec![("materials", Some(Value::Table(materials)))];
    }
    let table_of = |value: Option<&Value>| value.and_then(Value::as_table).cloned().unwrap_or_default();
    let mut root = table_of(asset.field("overrides"));
    let mut children = asset.array("children").to_vec();
    let (mut own, mut held) = (false, false);
    for slot in slots {
        let Some((_, within)) = &slot.node else { continue };
        let at: Option<usize> = within.get(1).and_then(|at| at.parse().ok());
        let all = match at {
            Some(at) => table_of(children.get(at).and_then(|child| child.get("overrides"))),
            None => root.clone(),
        };
        let Some(entry) = entry_with(project, slot, table_of(all.get(name)), change) else { continue };
        let all = overrides_with(all, name, entry);
        let Some(at) = at else {
            root = all;
            own = true;
            continue;
        };
        if let Some(changed) = children_with(children.clone(), at, all) {
            children = changed;
            held = true;
        }
    }
    let mut out = Vec::new();
    if own {
        out.push(("overrides", (!root.is_empty()).then_some(Value::Table(root))));
    }
    if held {
        out.push(("children", Some(Value::Array(children))));
    }
    out
}

/// A texture of another image holds what that image has: a cell or a palette it does not is its first. An image with no
/// cells has nothing a face could read.
fn fitted(ui: &Ui, texture: Value) -> Option<Value> {
    let choices = choices_of(ui, &texture).filter(|choices| !choices.cells.is_empty())?;
    let fit = |texture: Value, key: &str, held: Option<&str>, known: &[String]| match known.first() {
        Some(first) if held.is_some_and(|held| !known.iter().any(|name| name == held)) => with(&texture, key, first),
        _ => texture,
    };
    let cell = material::cell_of(&texture).map(str::to_string);
    let palette = material::palette_of(&texture).map(str::to_string);
    let texture = fit(texture, "cell", cell.as_deref(), &choices.cells);
    Some(fit(texture, "palette", palette.as_deref(), &choices.palettes))
}

#[derive(Default)]
pub struct Bindings {
    assembled: Option<Result<Assembled, String>>,
    pictures: Pictures,
}

impl Bindings {
    fn thumbnail(&mut self, ui: &mut Ui, slot: &Slot) {
        let bound = slot.drawn.as_ref().and_then(|drawn| drawn.main.as_ref());
        let picture = bound.and_then(|bound| self.pictures.get(ui, &bound.image, &bound.part()).cloned());
        // Nothing to show keeps the place, so that the rows' text lines up.
        let picture = picture.unwrap_or_else(|| Texture::new("no texture", Image { width: 1, height: 2, rgba: vec![0; 8] }));
        #[expect(clippy::cast_precision_loss, reason = "a picture is far smaller than floats lose pixels at")]
        let (wide, tall) = (picture.width() as f32, picture.height().max(1) as f32);
        let fit = THUMBNAIL / wide.max(tall);
        ui.image(&picture, wide * fit, tall * fit);
    }

    /// The texture a node holds of its own: which image, which of its cells, through which of its palettes.
    fn own_texture(ui: &mut Ui, id: &str, texture: &Value, change: &mut Option<Change>) {
        if let Some(new) = ui.reference_taking("Texture", "image", Some(texture)).and_then(|new| fitted(ui, new)) {
            *change = Some(Change::Texture(new));
        }
        let Some(choices) = choices_of(ui, texture) else { return };
        let cell = material::cell_of(texture).unwrap_or_default().to_string();
        let palette = material::palette_of(texture).map(str::to_string).or_else(|| choices.own.clone()).unwrap_or_default();
        ui.horizontal(|ui| {
            ui.styled("Cell", Style::Weak);
            if let Some(chosen) = ui.combo(&format!("{id}.cell"), &cell, &choices.cells) {
                *change = Some(Change::Texture(with(texture, "cell", &choices.cells[chosen])));
            }
            ui.styled("Palette", Style::Weak);
            if let Some(chosen) = ui.combo(&format!("{id}.palette"), &palette, &choices.palettes) {
                *change = Some(Change::Texture(with(texture, "palette", &choices.palettes[chosen])));
            }
        });
    }

    /// A row of slots; what was asked of it this frame.
    fn row(&mut self, ui: &mut Ui, assembled: &Assembled, row: &[usize], part: Option<usize>) -> Option<Change> {
        let mut asked = None;
        let change = &mut asked;
        let (index, slot) = (row[0], &assembled.slots[row[0]]);
        let faces: usize = row.iter().map(|at| faces_with(assembled, &assembled.slots[*at], part)).sum();
        let overridden = slot.overrides.is_some();
        let nodes: Vec<&str> = row.iter().filter_map(|at| assembled.slots[*at].node.as_ref()).map(|(node, _)| node.as_str()).collect();
        let detail = match nodes.as_slice() {
            [] => format!("{faces} faces"),
            [node] if overridden => format!("{node}  ·  {faces} faces  ·  overridden"),
            [node] => format!("{node}  ·  {faces} faces"),
            many => format!("{} models  ·  {faces} faces", many.len()),
        };
        let held = slot.overrides.as_ref().and_then(|overrides| overrides.get(material::MAIN));
        let editable = ui.editable();
        ui.horizontal(|ui| {
            ui.vertical(|ui| self.thumbnail(ui, slot));
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.styled(&slot.name, Style::Strong);
                    ui.styled(&detail, Style::Note);
                    if nodes.len() > 1 {
                        ui.tooltip(&format!("{}: select one in Contents to change it alone", nodes.join(", ")));
                    }
                });
                ui.horizontal(|ui| {
                    if let Some(new) = ui.reference_taking("Material", "material", slot.reference.as_ref()) {
                        *change = Some(Change::Material(new));
                    }
                    if slot.node.is_some() && slot.material.is_some() && editable {
                        if held.is_none() && ui.icon_button(Icon::Image, "Override the texture") {
                            *change = Some(Change::OverrideTexture);
                        }
                        if overridden && ui.icon_button(Icon::Cross, "Revert to the model's") {
                            *change = Some(Change::Revert);
                        }
                    }
                });
                if let Some(texture) = held {
                    Self::own_texture(ui, &format!("slot{index}"), texture, change);
                }
            });
        });
        asked
    }
}

impl View for Bindings {
    fn show(&mut self, ui: &mut Ui) {
        let Some(asset) = ui.asset() else { return };
        let assembled = self.assembled.take().unwrap_or_else(|| assembly::assemble(ui.project(), &asset).map_err(|why| format!("{why:#}")));
        let assembled = match assembled {
            Ok(assembled) => assembled,
            Err(why) => {
                ui.error(&why);
                self.assembled = Some(Err(why));
                return;
            }
        };
        let part = ui.selected().as_deref().and_then(parts::bone_of);
        let rows = rows(&assembled, part);
        let title = part.and_then(|part| assembled.nodes.get(part)).map_or_else(|| "Materials".to_string(), |node| format!("Materials of {}", node.name));
        let mut asked = None;
        ui.section(&title, |ui| {
            if rows.is_empty() {
                ui.styled("No faces of this part", Style::Note);
            }
            for row in &rows {
                if let Some(change) = self.row(ui, &assembled, row, part) {
                    asked = Some((row, change));
                }
            }
        });
        let written = asked.and_then(|(row, change)| {
            let slots: Vec<&Slot> = row.iter().map(|at| &assembled.slots[*at]).collect();
            Some(written(&ui.asset()?, ui.project(), &slots, &change))
        });
        self.assembled = Some(Ok(assembled));
        for (key, value) in written.into_iter().flatten() {
            ui.set_step(key, value.as_ref());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assembly::{Attached, Node};
    use crate::model::{Corner, Face, Material, Model, Part, Shading};

    fn table(text: &str) -> Table {
        text.parse().unwrap()
    }

    fn face(material: usize, part: usize) -> Face {
        Face { material, shading: Shading::Unlit, part: Some(part), normal: None, color: None, corners: vec![Corner::default(); 3] }
    }

    fn slot(name: &str, material: &str) -> Slot {
        let attached = Attached { path: material.to_string(), reference: material.to_string(), child: None };
        Slot { name: name.to_string(), material: Some(attached), reference: None, drawn: None, node: None, overrides: None }
    }

    fn two_parts() -> Assembled {
        let part = |parent| Part { parent, rotation: [[4096, 0, 0], [0, 4096, 0], [0, 0, 4096]], translation: [0; 3], vertices: vec![[0; 3]] };
        let model = Model {
            parts: vec![part(0), part(0)],
            materials: vec![Material { transparent: false, slot: None, blend: None }; 3],
            faces: vec![face(0, 0), face(1, 0), face(2, 1)],
            ..Model::default()
        };
        let node = |name: &str| Node { name: name.to_string(), attached: None, place: None };
        Assembled { posed: Vec::new(), model, nodes: vec![node("a"), node("b")], slots: vec![slot("x", "m"), slot("y", "n"), slot("x", "m")] }
    }

    #[test]
    fn a_slot_lists_once_and_counts_every_face_it_draws() {
        let assembled = two_parts();
        assert_eq!(listed(&assembled, None), [0, 1]);
        assert_eq!(faces_with(&assembled, &assembled.slots[0], None), 2);
    }

    #[test]
    fn a_part_lists_what_its_own_faces_use() {
        let assembled = two_parts();
        assert_eq!(listed(&assembled, Some(1)), [0], "only x is on b");
        assert_eq!(faces_with(&assembled, &assembled.slots[0], Some(1)), 1);
        assert_eq!(listed(&assembled, Some(0)), [0, 1]);
    }

    #[test]
    fn nodes_drawing_with_one_material_are_one_row_until_one_overrides_it() {
        let mut assembled = two_parts();
        let on = |slot: &mut Slot, node: &str| slot.node = Some((node.to_string(), Vec::new()));
        assembled.slots[0] = slot("x", "m");
        assembled.slots[2] = slot("x", "m");
        on(&mut assembled.slots[0], "body");
        on(&mut assembled.slots[2], "hand");
        assert_eq!(rows(&assembled, None), [vec![0, 2], vec![1]]);
        assert_eq!(rows(&assembled, Some(1)), [vec![2]], "a node selected is that node's alone");
        assembled.slots[2].overrides = Some(Value::Table(table("material = 'a'")));
        assert_eq!(rows(&assembled, None), [vec![0], vec![1], vec![2]]);
    }

    #[test]
    fn a_dropped_uid_is_written_as_a_reference_and_a_reference_as_it_is() {
        let made = filled(&Value::String("uid://a".to_string()));
        assert_eq!(made.get("slot").and_then(Value::as_str), Some("uid://a"));
        let kept = Value::Table(table("[slot]\nuid = 'uid://b'\npath = 'res://b'"));
        assert_eq!(filled(&kept), kept);
    }

    #[test]
    fn the_last_override_of_a_slot_takes_the_slot() {
        let all = table("[x]\nmaterial = 'a'\n[y]\nmaterial = 'b'");
        let left = overrides_with(all, "x", Table::new());
        assert_eq!(left.keys().collect::<Vec<_>>(), ["y"]);
        assert!(overrides_with(left, "y", Table::new()).is_empty());
    }

    #[test]
    fn a_child_with_no_overrides_left_holds_none() {
        let children = vec![Value::Table(table("node = 'hand'\n[overrides.x]\nmaterial = 'a'"))];
        let cleared = children_with(children.clone(), 0, Table::new()).unwrap();
        assert!(cleared[0].get("overrides").is_none());
        assert_eq!(cleared[0].get("node").and_then(Value::as_str), Some("hand"));
        let held = children_with(children, 0, table("[y]\nmaterial = 'b'")).unwrap();
        assert!(held[0].get("overrides").and_then(|overrides| overrides.get("y")).is_some());
    }
}
