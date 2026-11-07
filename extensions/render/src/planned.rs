//! What an extraction plan runs of render: a mesh, a clip and a model read from their bytes on the disc, each given what
//! it needs of the other declarations - a skeleton's joints, a model's materials and the images and slots of VRAM they
//! place their textures by - so that each is a function of what it is given alone.
//!
//! - `extract_mesh`: a mesh's bytes (`stored`), where they load (`address`), its slots' names (`material_slots`) and the
//!   joints of its skeleton where it has one (`joints`), held to them; makes `mesh.usda`.
//! - `extract_clip`: a clip's bytes, where they load, and its skeleton's joints; makes `clip.usda`, held to read back
//!   into the same bytes.
//! - `check_model`: a model's bytes (`stored`) and its mesh as extracted (`mesh`), rebuilt from them, its mesh's joints,
//!   the materials filling its slots (`model.materials`, `materials`) and their images (`images`, `vram_slots`), held to
//!   the bytes; makes nothing.

use pe2_ext::{Call, Data, File, Registry, Result, Table, Typed, Value, anyhow, bail};
use std::collections::HashMap;

use crate::material::{self, Definition};
use crate::model::{Binding, Placement, decode, encode};
use crate::{clip, clip_usd, mesh, skeleton};

/// What these make: their files.
const FILES: &str = "bytes";

pub fn register(r: &mut Registry) {
    let _ = r
        .op("extract_mesh")
        .label("A mesh, read from its bytes")
        .port("stored", "bytes", false)
        .output_described(FILES)
        .run_described(|call| {
            Ok(files(mesh::FILE, extract_mesh(&stored(call, "stored")?, call.settings)?.into_bytes()))
        });
    let _ = r
        .op("extract_clip")
        .label("A clip, read from its bytes")
        .port("stored", "bytes", false)
        .output_described(FILES)
        .run_described(|call| {
            Ok(files(clip_usd::FILE, extract_clip(&stored(call, "stored")?, call.settings)?.into_bytes()))
        });
    let _ = r
        .op("check_model")
        .label("A model, rebuilt and held to its bytes")
        .port("stored", "bytes", false)
        .port("mesh", "bytes", false)
        .output_described(FILES)
        .run_described(|call| {
            let mesh = file_of(call, "mesh", mesh::FILE)?;
            check_model(&stored(call, "stored")?, &String::from_utf8(mesh)?, call.settings)?;
            Ok(Data::Typed(Typed::new(FILES, Vec::new())))
        });
}

fn files(name: &str, bytes: Vec<u8>) -> Data {
    Data::Typed(Typed::new(FILES, vec![File::new(name, bytes)]))
}

fn port<'c>(call: &'c Call<'_>, name: &str) -> Result<&'c Typed> {
    let at = match name {
        "stored" => 0,
        _ => 1,
    };
    match call.inputs.get(at).and_then(|port| port.first()) {
        Some(Data::Typed(typed)) => Ok(typed),
        _ => bail!("it is given no {name}"),
    }
}

/// The one file given at a port.
fn stored(call: &Call<'_>, name: &str) -> Result<Vec<u8>> {
    call.read(port(call, name)?.file()?)
}

/// A file given at a port, by name.
fn file_of(call: &Call<'_>, name: &str, file: &str) -> Result<Vec<u8>> {
    let given = port(call, name)?;
    let found = given.files.iter().find(|f| f.name == file).ok_or_else(|| anyhow!("its {name} holds no {file}"))?;
    call.read(found)
}

fn address(settings: &Table) -> Result<u32> {
    let address = settings.get("address").and_then(Value::as_integer).ok_or_else(|| anyhow!("no address"))?;
    u32::try_from(address).map_err(|_| anyhow!("its address {address}"))
}

fn joints(settings: &Table) -> Result<Option<Vec<skeleton::Joint>>> {
    settings.get("joints").and_then(Value::as_array).map(|joints| skeleton::parse(joints)).transpose()
}

pub(crate) fn extract_mesh(stored: &[u8], settings: &Table) -> Result<String> {
    let model = decode(stored, address(settings)?)?;
    let names = match joints(settings)? {
        Some(joints) => {
            if !mesh::rests_on(&model, &joints) {
                bail!("it does not rest as its skeleton says");
            }
            Some(joints.into_iter().map(|joint| joint.name).collect::<Vec<_>>())
        }
        None => None,
    };
    let slots: Vec<String> = settings
        .get("material_slots")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    if slots.len() != model.materials.len() {
        bail!("it has {} materials, the declaration names {}", model.materials.len(), slots.len());
    }
    Ok(mesh::write(&model, &slots, names.as_deref()))
}

pub(crate) fn extract_clip(stored: &[u8], settings: &Table) -> Result<String> {
    let address = address(settings)?;
    let joints = joints(settings)?.ok_or_else(|| anyhow!("it moves no skeleton"))?;
    let text = clip_usd::write(&clip::decode(stored, address)?, &joints)?;
    // What the bundle holds, read back, has to be what the disc stores.
    let again = clip::encode(&clip_usd::read(&text)?, address)?;
    if again != stored {
        let first = again.iter().zip(stored).position(|(a, b)| a != b).unwrap_or(again.len().min(stored.len()));
        bail!("written and read back it differs from the disc at {first:#x}");
    }
    Ok(text)
}

fn tables_of(settings: &Table, key: &str) -> HashMap<String, Table> {
    let given = settings.get(key).and_then(Value::as_table).into_iter().flatten();
    given.filter_map(|(path, table)| Some((path.clone(), table.as_table()?.clone()))).collect()
}

pub(crate) fn check_model(stored: &[u8], mesh_text: &str, settings: &Table) -> Result<()> {
    let address = address(settings)?;
    let (mut model, slots) = mesh::read(mesh_text)?;
    if let Some(joints) = joints(settings)? {
        mesh::pose(&mut model, &joints)?;
    }
    let tables = Tables {
        materials: tables_of(settings, "materials"),
        images: tables_of(settings, "images"),
        slots: tables_of(settings, "vram_slots"),
    };
    let declared = settings.get("model").and_then(Value::as_table).cloned().unwrap_or_default();
    let bindings: Vec<Binding> = slots.iter().map(|slot| binding(&tables, &declared, slot)).collect::<Result<_>>()?;
    let rebuilt = encode(&model, &bindings, address)?;
    if rebuilt != stored {
        let first = rebuilt.iter().zip(stored).position(|(a, b)| a != b).unwrap_or(rebuilt.len().min(stored.len()));
        bail!(
            "rebuilt from its mesh and materials it differs from the disc at {first:#x}, of {} against {} bytes",
            rebuilt.len(),
            stored.len()
        );
    }
    Ok(())
}

/// The declarations a model's bytes are rebuilt from, by path: its materials, their images and the slots of VRAM those
/// are placed in.
pub(crate) struct Tables {
    pub materials: HashMap<String, Table>,
    pub images: HashMap<String, Table>,
    pub slots: HashMap<String, Table>,
}

/// The path a reference names, a `slot` beside what it places or the reference alone.
fn target(reference: &Value) -> Option<&str> {
    let reference = reference.get("slot").unwrap_or(reference);
    reference.get("slot").unwrap_or(reference).get("path")?.as_str()?.strip_prefix("res://")?.strip_suffix(".node")
}

fn pair(value: Option<&Value>) -> Option<[i64; 2]> {
    let values = value?.as_array()?;
    Some([values.first()?.as_integer()?, values.get(1)?.as_integer()?])
}

/// Where a slot of VRAM puts what it holds, in halfwords: its corner, plus what the placement adds.
fn placed(tables: &Tables, placement: &Value) -> Option<[i64; 2]> {
    let rect = tables.slots.get(target(placement)?)?.get("rect")?.as_array()?;
    let (x, y) = (rect.first()?.as_integer()?, rect.get(1)?.as_integer()?);
    let [ax, ay] = pair(placement.get("at")).unwrap_or_default();
    Some([x + ax, y + ay])
}

/// Where a material's texture is on the console: all of it follows from the image's declaration, and the depth the
/// material reads it at where it says.
fn placement(tables: &Tables, texture: &Value, bits: Option<i64>) -> Result<Placement> {
    let path = target(texture).ok_or_else(|| anyhow!("a texture names no image"))?;
    let image = tables.images.get(path).ok_or_else(|| anyhow!("no image {path}"))?;
    material::placement_at(&material::image_placement(image, |placement| placed(tables, placement)), texture, bits)
        .map_err(|why| anyhow!("{path}: {why:#}"))
}

fn binding(tables: &Tables, model: &Table, slot: &str) -> Result<Binding> {
    let reference = model
        .get("materials")
        .and_then(|materials| materials.get(slot))
        .ok_or_else(|| anyhow!("its slot {slot} is empty"))?;
    let path = target(reference).ok_or_else(|| anyhow!("its slot {slot} names nothing"))?;
    let table = tables.materials.get(path).ok_or_else(|| anyhow!("no material {path}"))?;
    let read = Definition::read(|key| table.get(key));
    let texture = if read.shader.textures.is_empty() {
        None
    } else {
        let main = table.get(material::MAIN).ok_or_else(|| anyhow!("{path}: no main texture"))?;
        Some(placement(tables, main, table.get(material::BITS).and_then(Value::as_integer))?)
    };
    Ok(Binding {
        texture,
        transparent: read.transparent,
        blend: read.blend,
        reflective: read.shader.reflective,
        effect: read.effect,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tables() -> Tables {
        let text = |toml: &str| toml.parse::<Table>().unwrap();
        let slot = |name: &str| format!("{{ uid = 'uid://x', path = 'res://{name}.node' }}");
        let image = format!(
            "mode = 'indexed_8bpp'\npage = {{ slot = {}, at = [0, 0] }}\ncells.left = {{ rect = [0, 0, 128, 256], palette = 'one' }}\ncells.right = {{ rect = [128, 0, 128, 256], palette = 'two' }}\npalettes.one = {{ slot = {}, at = [0, 0] }}\npalettes.two = {{ slot = {}, at = [0, 1] }}",
            slot("sheet"),
            slot("colors"),
            slot("colors")
        );
        Tables {
            materials: HashMap::new(),
            images: HashMap::from([("image".to_string(), text(&image))]),
            slots: HashMap::from([
                ("sheet".to_string(), text("rect = [384, 256, 128, 256]")),
                ("colors".to_string(), text("rect = [0, 245, 256, 2]")),
            ]),
        }
    }

    #[test]
    fn a_cell_past_a_page_is_on_the_next() {
        let texture: Table =
            "main = { uid = 'uid://x', path = 'res://image.node', output = 'cells/right' }".parse().unwrap();
        let at = placement(&tables(), texture.get("main").unwrap(), None).unwrap();
        assert_eq!(at, Placement { bits: 8, page: [448, 256], palette: [0, 246] });
    }

    #[test]
    fn a_binding_chooses_its_palette_over_its_cells() {
        let texture: Table =
            "main = { uid = 'uid://x', path = 'res://image.node', output = 'cells/left@two' }".parse().unwrap();
        let at = placement(&tables(), texture.get("main").unwrap(), None).unwrap();
        assert_eq!(at, Placement { bits: 8, page: [384, 256], palette: [0, 246] });
    }
}
