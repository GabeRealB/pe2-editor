//! What a model's or a prefab's faces are drawn with, in the inspector: each material once, with its texture, however many
//! nodes of a prefab draw with it, narrowed to what the part selected uses. A material dropped on a row fills its slot: a
//! model's own, or a prefab's override for every part the row stands for, one alone once it is selected. An override is
//! also where a texture of its own goes, and what Revert takes away, and what is overridden has a row of its own. A prefab
//! lists its parts first, as what fills them now: one on the skeleton has no row in the tree, and one stored has no node.

use std::collections::BTreeMap;

use pe2_ext::{Asset, Icon, Image, Project, Style, Table, Texture, Ui, Value, View};

use crate::assembly::{self, Assembled, Slot};
use crate::material;
use crate::material_panel::{Pictures, choices_of};
use crate::parts;
use crate::prefab::{self, Filled};

/// How wide and tall a slot's picture is at most.
const THUMBNAIL: f32 = 56.0;
/// The columns of the parts: what a name's letter takes, what is left after it, and what a model's chip takes.
const CHARACTER: f32 = 7.5;
const GAP: f32 = 12.0;
const CHIP: f32 = 205.0;

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
    slots
        .filter(|(index, slot)| {
            first_of(assembled, slot) == *index && (part.is_none() || faces_with(assembled, slot, part) > 0)
        })
        .map(|(index, _)| index)
        .collect()
}

/// Whether two nodes' slots read as one row: a node's own override makes it its own.
fn one_row(a: &Slot, b: &Slot) -> bool {
    a.name == b.name
        && a.material == b.material
        && a.reference == b.reference
        && a.overrides.is_none()
        && b.overrides.is_none()
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
fn written(asset: &Asset, project: &dyn Project, slots: &[&Slot], change: &Change) -> Vec<(String, Option<Value>)> {
    let name = &slots[0].name;
    if slots[0].node.is_none() {
        let Change::Material(new) = change else { return Vec::new() };
        let mut materials = asset.field("materials").and_then(Value::as_table).cloned().unwrap_or_default();
        materials.insert(name.clone(), filled(new));
        return vec![("materials".to_string(), Some(Value::Table(materials)))];
    }
    let mut roots: BTreeMap<String, Value> = BTreeMap::new();
    for slot in slots {
        let Some((_, within)) = &slot.node else { continue };
        let Some((key, steps)) = within.split_first() else { continue };
        let Some(root) = roots.get(key).or_else(|| asset.field(key)).cloned() else { continue };
        let fill = steps.iter().try_fold(&root, |value, step| value.get(step));
        let all = fill.and_then(|fill| fill.get("overrides")).and_then(Value::as_table).cloned().unwrap_or_default();
        let Some(entry) =
            entry_with(project, slot, all.get(name).and_then(Value::as_table).cloned().unwrap_or_default(), change)
        else {
            continue;
        };
        let overrides = overrides_with(all, name, entry);
        let Some(new) = prefab::changed(root, steps, |fill| set_overrides(fill, overrides)) else { continue };
        roots.insert(key.clone(), new);
    }
    roots.into_iter().map(|(key, value)| (key, Some(value))).collect()
}

/// What fills a part with its overrides: none left, none written.
fn set_overrides(fill: &mut Table, overrides: Table) {
    if overrides.is_empty() {
        fill.remove("overrides");
    } else {
        fill.insert("overrides".to_string(), Value::Table(overrides));
    }
}

/// A texture of another image holds what that image has: a cell or a palette it does not is its first. An image with no
/// cells has nothing a face could read.
fn fitted(ui: &Ui, texture: &Value) -> Option<Value> {
    let choices = choices_of(ui, texture).filter(|choices| !choices.cells.is_empty())?;
    let held = |name: &str, known: &[String]| known.iter().any(|known| known == name);
    let cell = material::cell_of(texture).filter(|cell| held(cell, &choices.cells)).unwrap_or(&choices.cells[0]);
    let palette = material::palette_of(texture).map(|palette| {
        if held(palette, &choices.palettes) {
            palette
        } else {
            choices.palettes.first().map_or(palette, String::as_str)
        }
    });
    Some(material::reading(texture, cell, palette))
}

#[derive(Default)]
pub struct Bindings {
    assembled: Option<Result<Assembled, String>>,
    /// A prefab's parts, as what fills them now.
    parts: Option<Vec<Filled>>,
    pictures: Pictures,
}

impl Bindings {
    /// A prefab's parts, each with what fills it: the model dropped on one fills it.
    fn parts(ui: &mut Ui, parts: &[Filled], assembled: &Assembled) -> Option<(Vec<String>, Value)> {
        let widest = parts.iter().map(|part| part.part.len()).max().unwrap_or(0);
        #[expect(clippy::cast_precision_loss, reason = "a name's length")]
        let (name, chip) = (widest as f32 * CHARACTER + GAP, widest as f32 * CHARACTER + GAP + CHIP);
        let mut asked = None;
        ui.section("Parts", |ui| {
            for part in parts {
                ui.horizontal(|ui| {
                    ui.styled(&part.part, Style::Normal);
                    ui.tab(name);
                    let model = part.fill.as_ref().and_then(|fill| fill.get("model"));
                    if let Some(new) = ui.reference_taking("Model", "model", model) {
                        asked = Some((part.within.clone(), new));
                    }
                    ui.tab(chip);
                    let shown = assembled
                        .slots
                        .iter()
                        .any(|slot| slot.node.as_ref().is_some_and(|(node, _)| *node == part.part));
                    match &part.node {
                        Some(node) => ui.styled(&format!("on {node}"), Style::Note),
                        None if shown => ui.styled("the skeleton's", Style::Note),
                        None => ui.styled("stored, not shown", Style::Note),
                    }
                });
            }
        });
        asked
    }

    fn thumbnail(&mut self, ui: &mut Ui, slot: &Slot) {
        let bound = slot.drawn.as_ref().and_then(|drawn| drawn.main.as_ref());
        let picture = bound.and_then(|bound| self.pictures.get(ui, &bound.image, &bound.output()).cloned());
        // Nothing to show keeps the place, so that the rows' text lines up.
        let picture =
            picture.unwrap_or_else(|| Texture::new("no texture", Image { width: 1, height: 2, rgba: vec![0; 8] }));
        #[expect(clippy::cast_precision_loss, reason = "a picture is far smaller than floats lose pixels at")]
        let (wide, tall) = (picture.width() as f32, picture.height().max(1) as f32);
        let fit = THUMBNAIL / wide.max(tall);
        ui.image(&picture, wide * fit, tall * fit);
    }

    /// The texture a node holds of its own: which image, which of its cells, through which of its palettes.
    fn own_texture(ui: &mut Ui, id: &str, texture: &Value, change: &mut Option<Change>) {
        if let Some(new) = ui.reference_taking("Texture", "image", Some(texture)).and_then(|new| fitted(ui, &new)) {
            *change = Some(Change::Texture(new));
        }
        let Some(choices) = choices_of(ui, texture) else { return };
        let cell = material::cell_of(texture).unwrap_or_default().to_string();
        let palette =
            material::palette_of(texture).map(str::to_string).or_else(|| choices.own.clone()).unwrap_or_default();
        ui.horizontal(|ui| {
            ui.styled("Cell", Style::Weak);
            if let Some(chosen) = ui.combo(&format!("{id}.cell"), &cell, &choices.cells) {
                *change = Some(Change::Texture(material::reading(
                    texture,
                    &choices.cells[chosen],
                    material::palette_of(texture),
                )));
            }
            ui.styled("Palette", Style::Weak);
            if let Some(chosen) = ui.combo(&format!("{id}.palette"), &palette, &choices.palettes) {
                *change = Some(Change::Texture(material::reading(texture, &cell, Some(&choices.palettes[chosen]))));
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
        let nodes: Vec<&str> =
            row.iter().filter_map(|at| assembled.slots[*at].node.as_ref()).map(|(node, _)| node.as_str()).collect();
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
        let assembled = match self.assembled.take() {
            Some(assembled) => assembled,
            None => match assembly::requested(ui.project(), &asset) {
                assembly::Scene::Ready(assembled) => Ok(*assembled),
                assembly::Scene::Failed(why) => Err(why),
                assembly::Scene::Waiting(waited) => {
                    assembly::say_waiting(ui, waited);
                    return;
                }
            },
        };
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
        if self.parts.is_none() {
            self.parts = prefab::filled(&asset, ui.project()).ok();
        }
        let listed_parts = self.parts.take().unwrap_or_default();
        let model = if listed_parts.is_empty() { None } else { Self::parts(ui, &listed_parts, &assembled) };
        self.parts = Some(listed_parts);
        let title = part
            .and_then(|part| assembled.nodes.get(part))
            .map_or_else(|| "Materials".to_string(), |node| format!("Materials of {}", node.name));
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
        let filled_part = model.and_then(|(within, new)| {
            prefab::edited(&ui.asset()?, &within, |fill| drop(fill.insert("model".to_string(), filled(&new))))
        });
        for (key, value) in written.into_iter().flatten().chain(filled_part.map(|(key, value)| (key, Some(value)))) {
            ui.set_step(&key, value.as_ref());
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
        Face {
            material,
            shading: Shading::Unlit,
            part: Some(part),
            normal: None,
            color: None,
            corners: vec![Corner::default(); 3],
        }
    }

    fn slot(name: &str, material: &str) -> Slot {
        let attached = Attached { path: material.to_string(), reference: material.to_string() };
        Slot {
            name: name.to_string(),
            material: Some(attached),
            reference: None,
            drawn: None,
            node: None,
            overrides: None,
        }
    }

    fn two_parts() -> Assembled {
        let part = |parent| Part {
            parent,
            rotation: [[4096, 0, 0], [0, 4096, 0], [0, 0, 4096]],
            translation: [0; 3],
            vertices: vec![[0; 3]],
        };
        let model = Model {
            parts: vec![part(0), part(0)],
            materials: vec![Material { transparent: false, slot: None, blend: None }; 3],
            faces: vec![face(0, 0), face(1, 0), face(2, 1)],
            ..Model::default()
        };
        let node = |name: &str| Node { name: name.to_string(), attached: None, place: None };
        Assembled {
            posed: Vec::new(),
            model,
            clip: None,
            nodes: vec![node("a"), node("b")],
            slots: vec![slot("x", "m"), slot("y", "n"), slot("x", "m")],
        }
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
    fn a_part_with_no_overrides_left_holds_none() {
        let mut fill = table("model = 'uid://x'\n[overrides.x]\nmaterial = 'a'");
        set_overrides(&mut fill, Table::new());
        assert!(fill.get("overrides").is_none());
        assert!(fill.get("model").is_some());
        set_overrides(&mut fill, table("[y]\nmaterial = 'b'"));
        assert!(fill["overrides"].get("y").is_some());
    }
}
