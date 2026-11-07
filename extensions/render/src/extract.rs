//! Extracting meshes (`pe2.render@meshes`). A mesh is read from a run of a package, which the disc holds compressed: its declaration says the chunk (`at`), where in what that inflates to the
//! model is (`offset`, `size`), where it loads (`address`), what its slots are called (`material_slots`), and its bytes'
//! sha1. All of that is the recipe: what the bundle holds is the geometry. A package holding many is inflated once.
//!
//! A clip is read the same way, from the package that stores it first, and checked to come out the same once written.
//!
//! A model's `from` is where the disc stores it, and it extracts to nothing: its bytes are rebuilt from its mesh, its
//! skeleton and its materials, and have to come out the same, so that nothing a material leaves out goes unnoticed.

use formats::lzss;
use pe2_ext::{Declared, Discs, Extraction, Made, Result, Table, Value, anyhow, bail};
use sha1::{Digest, Sha1};

use crate::material::{self, Definition};
use crate::mesh;
use crate::model::{Binding, Model, Placement, decode, encode};
use crate::skeleton;
use crate::{clip, clip_usd};
use std::collections::HashMap;
use std::fmt::Write as _;

/// The declarations a model's bytes are rebuilt from, by path.
struct Tables {
    meshes: HashMap<String, Table>,
    skeletons: HashMap<String, Table>,
    materials: HashMap<String, Table>,
    images: HashMap<String, Table>,
    slots: HashMap<String, Table>,
}

impl Tables {
    fn new(declared: &dyn Declared) -> Tables {
        let of = |node_type: &str| declared.declarations(node_type).into_iter().collect();
        Tables {
            meshes: of("mesh"),
            skeletons: of("skeleton"),
            materials: of("material"),
            images: of("image"),
            slots: of("vram_slot"),
        }
    }
}

struct Meshes {
    tables: Tables,
    /// The package last inflated, by where it is.
    package: Option<(String, Vec<u8>)>,
}

#[expect(clippy::unnecessary_wraps, reason = "what begins an extraction may fail; this does not")]
pub fn begin(declared: &dyn Declared) -> Result<Box<dyn Extraction>> {
    Ok(Box::new(Meshes { tables: Tables::new(declared), package: None }))
}

#[expect(clippy::unnecessary_wraps, reason = "what begins an extraction may fail; this does not")]
pub fn models(declared: &dyn Declared) -> Result<Box<dyn Extraction>> {
    Ok(Box::new(Models {
        meshes: Meshes { tables: Tables::new(declared), package: None },
        declared: declared.declarations("model").into_iter().collect(),
    }))
}

#[expect(clippy::unnecessary_wraps, reason = "what begins an extraction may fail; this does not")]
pub fn clips(declared: &dyn Declared) -> Result<Box<dyn Extraction>> {
    Ok(Box::new(Clips {
        meshes: Meshes { tables: Tables::new(declared), package: None },
        declared: declared.declarations("clip").into_iter().collect(),
    }))
}

/// Clips, read from where a package stores them, on the skeleton they move.
struct Clips {
    meshes: Meshes,
    declared: HashMap<String, Table>,
}

impl Extraction for Clips {
    fn extract(&mut self, path: &str, release: &str, discs: &mut dyn Discs) -> Result<Made> {
        let declared = self.declared.get(path).ok_or_else(|| anyhow!("{path}: not a clip"))?.clone();
        let (stored, address) = self.meshes.stored(path, &declared, release, discs)?;
        let named = declared.get("skeleton").and_then(target).ok_or_else(|| anyhow!("{path}: it moves no skeleton"))?;
        let table = self.meshes.tables.skeletons.get(named).ok_or_else(|| anyhow!("{path}: no skeleton {named}"))?;
        let joints = skeleton::parse(table.get("joints").and_then(Value::as_array).map_or(&[][..], Vec::as_slice))?;
        let decoded = clip::decode(&stored, address).map_err(|why| anyhow!("{path}: {why:#}"))?;
        let text = clip_usd::write(&decoded, &joints).map_err(|why| anyhow!("{path}: {why:#}"))?;
        // What the bundle holds, read back, has to be what the disc stores.
        let again = clip::encode(&clip_usd::read(&text)?, address)?;
        if again != stored {
            let first = again.iter().zip(&stored).position(|(a, b)| a != b).unwrap_or(again.len().min(stored.len()));
            bail!("{path}: written and read back it differs from the disc at {first:#x}");
        }
        Ok(vec![(clip_usd::FILE.to_string(), text.into_bytes())])
    }
}

/// A model holds nothing of its own: extracting one is rebuilding what the disc stores from its mesh, its skeleton, its
/// materials and their images, and holding that to the disc.
struct Models {
    meshes: Meshes,
    declared: HashMap<String, Table>,
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

/// What an asset made by hand needs of an extraction: nothing but its bundle.
struct Authored;

impl Extraction for Authored {
    fn extract(&mut self, path: &str, _: &str, _: &mut dyn Discs) -> Result<Made> {
        bail!("{path}: comes from no disc")
    }
}

#[expect(clippy::unnecessary_wraps, reason = "what begins an extraction may fail; this does not")]
pub fn authored(_: &dyn Declared) -> Result<Box<dyn Extraction>> {
    Ok(Box::new(Authored))
}

impl Meshes {
    /// The joints of the skeleton a mesh names, held to it: the asset's pose replaces the mesh's only where it is the same.
    fn check_skeleton(&self, path: &str, declared: &Table, model: &Model) -> Result<Option<Vec<String>>> {
        let Some(reference) = declared.get("skeleton") else { return Ok(None) };
        let named = target(reference).ok_or_else(|| anyhow!("{path}: its skeleton is no reference"))?;
        let table = self.tables.skeletons.get(named).ok_or_else(|| anyhow!("{path}: no skeleton {named}"))?;
        let joints = table.get("joints").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
        let joints = skeleton::parse(joints)?;
        if !mesh::rests_on(model, &joints) {
            bail!("{path}: does not rest as {named} says");
        }
        Ok(Some(joints.into_iter().map(|joint| joint.name).collect()))
    }

    /// The bytes a declaration's `from` says, checked against its sha1, and the address they load at.
    fn stored(&mut self, path: &str, declared: &Table, release: &str, discs: &mut dyn Discs) -> Result<(Vec<u8>, u32)> {
        let from =
            declared.get("from").and_then(|f| f.get(release)).ok_or_else(|| anyhow!("{path}: not in {release}"))?;
        let at = from
            .get("at")
            .and_then(Value::as_array)
            .and_then(|at| at.first())
            .and_then(Value::as_table)
            .ok_or_else(|| anyhow!("{path}: from no package"))?;
        let number = |key: &str| {
            from.get(key)
                .and_then(Value::as_integer)
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(|| anyhow!("{path}: no {key}"))
        };
        let (offset, size) = (number("offset")?, number("size")?);
        let want = from.get("sha1").and_then(Value::as_str).ok_or_else(|| anyhow!("{path}: {release} has no sha1"))?;
        let key = at.to_string();
        if self.package.as_ref().is_none_or(|(known, _)| *known != key) {
            let stored = discs.read(at)?;
            self.package = Some((key, lzss::decode_stream(&stored, 0).0));
        }
        let inflated = self.package.as_ref().map_or(&[][..], |(_, bytes)| bytes);
        let bytes = inflated
            .get(offset..offset + size)
            .ok_or_else(|| anyhow!("{path}: past the end of its package, of {}", inflated.len()))?;
        let got = sha1_of(bytes);
        if got != want {
            bail!("{path}: has sha1 {got}, the declaration {want}");
        }
        Ok((bytes.to_vec(), u32::try_from(number("address")?).map_err(|_| anyhow!("{path}: its address"))?))
    }

    /// What a mesh's bundle holds.
    fn text_of(&mut self, path: &str, declared: &Table, release: &str, discs: &mut dyn Discs) -> Result<String> {
        let (bytes, address) = self.stored(path, declared, release, discs)?;
        let model = decode(&bytes, address)?;
        let joints = self.check_skeleton(path, declared, &model)?;
        let from = declared.get("from").and_then(|f| f.get(release));
        let slots: Vec<String> = from
            .and_then(|from| from.get("material_slots"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        if slots.len() != model.materials.len() {
            bail!("{path}: has {} materials, the declaration names {}", model.materials.len(), slots.len());
        }
        Ok(mesh::write(&model, &slots, joints.as_deref()))
    }
}

fn sha1_of(bytes: &[u8]) -> String {
    Sha1::digest(bytes).iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

impl Extraction for Meshes {
    fn extract(&mut self, path: &str, release: &str, discs: &mut dyn Discs) -> Result<Made> {
        let declared = self.tables.meshes.get(path).ok_or_else(|| anyhow!("{path}: not a mesh"))?.clone();
        Ok(vec![(mesh::FILE.to_string(), self.text_of(path, &declared, release, discs)?.into_bytes())])
    }
}

impl Extraction for Models {
    fn extract(&mut self, path: &str, release: &str, discs: &mut dyn Discs) -> Result<Made> {
        let declared = self.declared.get(path).ok_or_else(|| anyhow!("{path}: not a model"))?.clone();
        let (stored, address) = self.meshes.stored(path, &declared, release, discs)?;
        let mesh_path = declared.get("mesh").and_then(target).ok_or_else(|| anyhow!("{path}: no mesh"))?.to_string();
        let mesh_table =
            self.meshes.tables.meshes.get(&mesh_path).ok_or_else(|| anyhow!("{path}: no mesh {mesh_path}"))?.clone();
        // From the text a bundle holds, so that this holds of what the editor reads.
        let text = self.meshes.text_of(&mesh_path, &mesh_table, release, discs)?;
        let (mut model, slots) = mesh::read(&text)?;
        if let Some(named) = mesh_table.get("skeleton").and_then(target) {
            let joints =
                self.meshes.tables.skeletons.get(named).and_then(|table| table.get("joints")).and_then(Value::as_array);
            mesh::pose(&mut model, &skeleton::parse(joints.map_or(&[][..], Vec::as_slice))?)?;
        }
        let bindings: Vec<Binding> = slots
            .iter()
            .map(|slot| binding(&self.meshes.tables, &declared, slot).map_err(|why| anyhow!("{path}: {why:#}")))
            .collect::<Result<_>>()?;
        let rebuilt = encode(&model, &bindings, address).map_err(|why| anyhow!("{path}: {why:#}"))?;
        if rebuilt != stored {
            let first =
                rebuilt.iter().zip(&stored).position(|(a, b)| a != b).unwrap_or(rebuilt.len().min(stored.len()));
            bail!(
                "{path}: rebuilt from its mesh and materials it differs from the disc at {first:#x}, of {} against {} bytes",
                rebuilt.len(),
                stored.len()
            );
        }
        Ok(Vec::new())
    }
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
            meshes: HashMap::new(),
            skeletons: HashMap::new(),
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
