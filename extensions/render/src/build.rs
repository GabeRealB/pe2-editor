//! What a build reads of meshes, skeletons, materials and models, as outputs of their stacks: a skeleton's joints, a mesh
//! at rest on them, a material's binding as the game stores it, and a model's arrays as the tokens that define them in
//! C. Each reads the others' outputs, so what is made follows what it reached.

use pe2_ext::ty::{self, Files, Registered};
use pe2_ext::{Call, Data, File, Registry, Result, Signature, Table, Typed, Value, anyhow, bail};

use crate::material::{self, Definition};
use crate::mesh;
use crate::model::{Arrays, Binding, Blend, Bone, Placement, arrays};
use crate::skeleton;

/// A mesh, in USD's text form.
pub struct Usd;

impl Registered for Usd {
    const NAME: &str = "pe2.render@usd";
    const LABEL: &str = "USD text";

    fn signatures() -> Vec<Signature> {
        vec![Signature::Name(mesh::FILE.into())]
    }
}

impl Files for Usd {}

/// The stack of what names no file: an operation of this extension, kept as the node's `native`.
macro_rules! made_by {
    ($op:literal) => {
        concat!("[[stack]]\nop = \"pe2.render@", $op, "\"\n\n[[stack]]\nop = \"output\"\nname = \"native\"\nretain = \"cache\"\n")
    };
}

pub const SKELETON: &str = made_by!("joints");
pub const MATERIAL: &str = made_by!("binding");
pub const MODEL: &str = made_by!("arrays");

/// A mesh's stack: its file, which `rest` poses.
pub const MESH: &str = "[[stack]]\nop = \"file\"\nfile = \"mesh.usda\"\n\n[[stack]]\nop = \"output\"\nname = \"native\"\nretain = \"cache\"\n";

/// What a reference names, as an output of what it names is asked for: a uid, or a path.
fn named(reference: &Value) -> Option<String> {
    let reference = reference.get("slot").unwrap_or(reference);
    let reference = reference.get("slot").unwrap_or(reference);
    match reference {
        Value::String(text) => Some(text.split_once('#').map_or(text.as_str(), |(uid, _)| uid).to_string()),
        Value::Table(table) => table.get("uid")?.as_str().map(str::to_string),
        _ => None,
    }
}

/// What a build reads of a mesh: the output taking its stack's result.
pub fn register(r: &mut Registry) -> pe2_ext::Name {
    let _ = r.value_type::<Usd>();
    let _ = r.op("joints").label("Joints").output::<ty::Table>().run(joints);
    let rested = r.op("rest").label("At rest on its skeleton").input::<Usd>("mesh").output::<Usd>().run(rest);
    let _ = r.op("binding").label("What the game stores of it").output::<ty::Table>().run(binding);
    let _ = r.op("arrays").label("What the game stores of it").output::<ty::Table>().run(model_arrays);
    rested
}

#[expect(clippy::unnecessary_wraps, reason = "an operation may fail; this one does not")]
fn joints(call: &mut Call) -> Result<Table> {
    let joints = call.host.fields().get("joints").cloned().unwrap_or_else(|| Value::Array(Vec::new()));
    Ok(Table::from_iter([("joints".to_string(), joints)]))
}

fn rest(call: &mut Call) -> Result<Typed> {
    let source = call.take::<Usd>(0)?;
    let text = String::from_utf8(call.read(source.file()?)?)?;
    let (mut model, slots) = mesh::read(&text)?;
    let names = match call.host.fields().get("skeleton").and_then(named) {
        Some(skeleton) => {
            let table = call.table_of(&skeleton, "native")?;
            let joints = skeleton::parse(table.get("joints").and_then(Value::as_array).map_or(&[][..], Vec::as_slice))?;
            mesh::pose(&mut model, &joints)?;
            Some(joints.into_iter().map(|joint| joint.name).collect::<Vec<_>>())
        }
        None => None,
    };
    Ok(Typed::new(Usd::NAME, vec![File::new(mesh::FILE, mesh::write(&model, &slots, names.as_deref()).into_bytes())]))
}

/// A pair of numbers.
fn place(pair: Option<&Value>) -> Option<[i64; 2]> {
    let values = pair?.as_array()?;
    Some([values.first()?.as_integer()?, values.get(1)?.as_integer()?])
}

/// Where a material's texture is on the console, from what its image says of itself.
///
/// # Errors
/// It names what the image does not hold, or what the console cannot draw.
pub fn placement(image: &Table, texture: &Value) -> Result<Placement> {
    let bits = image.get("bits").and_then(Value::as_integer).ok_or_else(|| anyhow!("it has no depth"))?;
    if !matches!(bits, 4 | 8) {
        bail!("a {bits} bit image is no texture of a face");
    }
    let name = material::cell_of(texture).ok_or_else(|| anyhow!("no cell is named"))?;
    let cell = image.get("cells").and_then(|cells| cells.get(name)).ok_or_else(|| anyhow!("no cell {name}"))?;
    let palette = material::palette_of(texture).or_else(|| cell.get("palette").and_then(Value::as_str));
    let palette = palette.ok_or_else(|| anyhow!("{name} has no palette"))?;
    let colors = place(image.get("palettes").and_then(|palettes| palettes.get(palette))).ok_or_else(|| anyhow!("{palette} is in no slot"))?;
    let [x, y] = place(cell.get("rect")).ok_or_else(|| anyhow!("{name} has no place"))?;
    let [page_x, page_y] = place(image.get("page")).ok_or_else(|| anyhow!("no page"))?;
    let narrow = |value: i64| u16::try_from(value).map_err(|_| anyhow!("{value} is off the console's memory"));
    let page = [narrow((page_x + x * bits / 16) / 64 * 64)?, narrow((page_y + y) / 256 * 256)?];
    Ok(Placement { bits: u8::try_from(bits)?, page, palette: [narrow(colors[0])?, narrow(colors[1])?] })
}

/// A blend as a binding's table names it.
fn blend_id(blend: Blend) -> &'static str {
    material::BLENDS.iter().find(|(.., known)| *known == blend).map_or("mix", |(id, ..)| id)
}

fn binding_table(binding: &Binding) -> Table {
    let mut table = Table::new();
    let pair = |values: [u16; 2]| Value::Array(values.iter().map(|value| Value::Integer(i64::from(*value))).collect());
    if let Some(texture) = &binding.texture {
        table.insert("bits".to_string(), Value::Integer(i64::from(texture.bits)));
        table.insert("page".to_string(), pair(texture.page));
        table.insert("palette".to_string(), pair(texture.palette));
    }
    table.insert("transparent".to_string(), binding.transparent.into());
    table.insert("blend".to_string(), blend_id(binding.blend).into());
    table.insert("reflective".to_string(), binding.reflective.into());
    table.insert("effect".to_string(), Value::Integer(i64::from(binding.effect)));
    table
}

fn binding_from(table: &Table) -> Binding {
    let pair = |key: &str| -> Option<[u16; 2]> {
        let [a, b] = place(table.get(key))?;
        Some([u16::try_from(a).ok()?, u16::try_from(b).ok()?])
    };
    let bits = table.get("bits").and_then(Value::as_integer).and_then(|bits| u8::try_from(bits).ok());
    let texture = bits.and_then(|bits| Some(Placement { bits, page: pair("page")?, palette: pair("palette")? }));
    let flag = |key: &str| table.get(key).and_then(Value::as_bool).unwrap_or(false);
    let read = Definition::read(|key| table.get(key));
    Binding { texture, transparent: flag("transparent"), blend: read.blend, reflective: flag("reflective"), effect: read.effect }
}

fn binding(call: &mut Call) -> Result<Table> {
    let read = Definition::read(|key| call.host.fields().get(key));
    let texture = if read.shader.textures.is_empty() {
        None
    } else {
        let texture = call.host.fields().get(material::MAIN).ok_or_else(|| anyhow!("it has no texture"))?.clone();
        let image = named(&texture).ok_or_else(|| anyhow!("its texture names nothing"))?;
        Some(placement(&call.table_of(&image, "placement")?, &texture)?)
    };
    Ok(binding_table(&Binding { texture, transparent: read.transparent, blend: read.blend, reflective: read.shader.reflective, effect: read.effect }))
}

/// A C initializer's items.
fn lines(items: impl Iterator<Item = String>) -> String {
    items.map(|item| format!("{item},")).collect::<Vec<_>>().join(" ")
}

/// A model's arrays as the tokens initializing them, and the numbers its record holds.
#[must_use]
pub fn tokens(arrays: &Arrays) -> Table {
    let triple = |values: &[i16; 3]| format!("{{ {}, {}, {} }}", values[0], values[1], values[2]);
    let point = |values: &[i16; 3]| format!("{{ {}, {}, {}, 0 }}", values[0], values[1], values[2]);
    let bone = |bone: &Bone| {
        let rows = bone.rotation.iter().map(triple).collect::<Vec<_>>().join(", ");
        let at = bone.translation;
        format!("{{ {{ {{ {rows} }}, {{ {}, {}, {} }} }}, {} }}", at[0], at[1], at[2], bone.parent)
    };
    let mut table = Table::new();
    table.insert("bones".to_string(), lines(arrays.bones.iter().map(bone)).into());
    table.insert("part_vertices".to_string(), lines(arrays.part_vertices.iter().map(u32::to_string)).into());
    table.insert("vertices".to_string(), lines(arrays.vertices.iter().map(point)).into());
    table.insert("normals".to_string(), lines(arrays.normals.iter().map(point)).into());
    table.insert("stream".to_string(), lines(arrays.stream.iter().map(|word| format!("{word:#010X}"))).into());
    table.insert("buffer".to_string(), Value::Integer(i64::from(arrays.buffer)));
    table.insert("seam".to_string(), Value::Integer(i64::from(arrays.seam)));
    table
}

fn model_arrays(call: &mut Call) -> Result<Table> {
    let reference = call.host.fields().get("mesh").and_then(named).ok_or_else(|| anyhow!("it names no mesh"))?;
    let Data::Typed(rested) = call.output_of(&reference, "rest")? else { bail!("{reference}: its output is no mesh") };
    let (model, slots) = mesh::read(&String::from_utf8(call.read(rested.file()?)?)?)?;
    let materials = call.host.fields().get("materials").cloned().unwrap_or_else(|| Value::Table(Table::new()));
    let mut bindings = Vec::with_capacity(slots.len());
    for slot in &slots {
        let filled = materials.get(slot).and_then(named).ok_or_else(|| anyhow!("its slot {slot} is empty"))?;
        bindings.push(binding_from(&call.table_of(&filled, "native")?));
    }
    Ok(tokens(&arrays(&model, &bindings)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Part;

    #[test]
    fn a_binding_reads_as_it_was_written() {
        let texture = Some(Placement { bits: 8, page: [448, 256], palette: [0, 246] });
        let binding = Binding { texture, transparent: true, blend: Blend::Add, reflective: false, effect: 2 };
        assert_eq!(binding_from(&binding_table(&binding)), binding);
        let plain = Binding { texture: None, ..binding };
        assert_eq!(binding_from(&binding_table(&plain)), plain);
    }

    #[test]
    fn a_models_arrays_are_tokens_of_c() {
        let part = Part { parent: 0, rotation: [[4096, 0, 0], [0, 4096, 0], [0, 0, 4096]], translation: [1, -2, 3], vertices: vec![[1, 2, 3]] };
        let held = crate::model::Model { parts: vec![part], ..crate::model::Model::default() };
        let table = tokens(&arrays(&held, &[]).unwrap());
        assert_eq!(table["bones"].as_str(), Some("{ { { { 4096, 0, 0 }, { 0, 4096, 0 }, { 0, 0, 4096 } }, { 1, -2, 3 } }, 0 },"));
        assert_eq!(table["part_vertices"].as_str(), Some("1,"));
        assert_eq!(table["vertices"].as_str(), Some("{ 1, 2, 3, 0 },"));
        assert_eq!(table["stream"].as_str().unwrap().rsplit(' ').next(), Some("0xFFFFFFFF,"));
    }

    #[test]
    fn a_texture_is_placed_by_what_its_image_says() {
        let image: Table = "bits = 8\npage = [384, 256]\ncells.right = { rect = [128, 0, 128, 256], palette = 'one' }\npalettes.one = [0, 245]\npalettes.two = [0, 246]".parse().unwrap();
        let texture = Value::String("uid://x#cells/right@two".to_string());
        assert_eq!(placement(&image, &texture).unwrap(), Placement { bits: 8, page: [448, 256], palette: [0, 246] });
    }
}
