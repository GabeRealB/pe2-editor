//! What the game's packages need of each type: the forms the game stores them in, as C. A target with other forms overrides
//! these (`Tokenizer::target`). What is read of other nodes is read through their `settings` and their outputs, so what is
//! made follows what it reached, and nothing here is a node's output of its own: a model is just a model.

use pe2_ext::ty::Any;
use pe2_ext::tokens::{Args, Yields};
use pe2_ext::{Call, Data, Registry, Result, Table, Value, anyhow, bail, ty};
use pe2_model::material;
use pe2_model::mesh;
use pe2_model::model::{Arrays, Binding, Model, arrays};
use pe2_model::skeleton;

pub const SETTINGS: &str = "settings";

/// A node's settings, with what its stack derived in their places.
fn resolved(call: &mut Call) -> Result<Table> {
    let given = call.take::<Any>(0)?;
    let derived = if let Data::Typed(typed) = &given { typed.settings.clone() } else { Table::new() };
    Ok(pe2_ext::merged(call.host.fields(), &derived))
}

#[expect(clippy::unnecessary_wraps, reason = "an operation may fail; this one does not")]
fn plain(call: &mut Call) -> Result<Table> {
    Ok(call.host.fields().clone())
}

/// Where a placement (`slot`, `at`) puts what it holds, in VRAM halfwords.
fn placed(call: &Call, placement: &Value) -> Option<[i64; 2]> {
    let rect = call.table_of(pe2_ext::target_of(placement.get("slot")?)?, SETTINGS).ok()?;
    let rect = rect.get("rect")?.as_array()?.clone();
    let at = placement.get("at").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    let added = |n: usize| at.get(n).and_then(Value::as_integer).unwrap_or(0);
    Some([rect.first()?.as_integer()? + added(0), rect.get(1)?.as_integer()? + added(1)])
}

fn binding_of(call: &Call, fields: &Table) -> Result<Binding> {
    material::binding(fields, |texture| {
        let image = pe2_ext::target_of(texture).ok_or_else(|| anyhow!("its texture names nothing"))?;
        let settings = call.table_of(image, SETTINGS)?;
        material::placement(&material::image_placement(&settings, |placement| placed(call, placement)), texture)
    })
}

/// A mesh at rest on its skeleton, and the names of its slots.
fn rested(call: &Call, reference: &str) -> Result<(Model, Vec<String>)> {
    let Data::Typed(stored) = call.output_of(reference, "native")? else { bail!("{reference}: its output is no mesh") };
    let (mut model, slots) = mesh::read(&String::from_utf8(call.read(stored.file()?)?)?)?;
    if let Some(named) = call.table_of(reference, SETTINGS)?.get("skeleton").and_then(pe2_ext::target_of) {
        let table = call.table_of(named, SETTINGS)?;
        mesh::pose(&mut model, &skeleton::parse(table.get("joints").and_then(Value::as_array).map_or(&[][..], Vec::as_slice))?)?;
    }
    Ok((model, slots))
}

fn model_of(call: &Call) -> Result<(Model, Vec<Binding>)> {
    let fields = call.host.fields();
    let reference = fields.get("mesh").and_then(pe2_ext::target_of).ok_or_else(|| anyhow!("it names no mesh"))?;
    let (model, slots) = rested(call, reference)?;
    let materials = fields.get("materials");
    let mut bindings = Vec::with_capacity(slots.len());
    for slot in &slots {
        let filled = materials.and_then(|materials| materials.get(slot)).and_then(pe2_ext::target_of).ok_or_else(|| anyhow!("its slot {slot} is empty"))?;
        let settings = call.table_of(filled, SETTINGS)?;
        bindings.push(binding_of(call, &settings).map_err(|why| anyhow!("{slot}: {why:#}"))?);
    }
    Ok((model, bindings))
}

/// Plain data as what goes between braces: an array in braces, a number as itself, a text as tokens. The largest words read as
/// the bits they are, past what a signed word holds.
#[must_use]
pub fn printed(value: &Value) -> String {
    match value {
        Value::Array(items) => format!("{{ {} }}", items.iter().map(printed).collect::<Vec<_>>().join(", ")),
        Value::Integer(number) if *number > i64::from(i32::MAX) => format!("{number:#X}"),
        Value::Integer(number) => number.to_string(),
        Value::String(text) => text.clone(),
        Value::Boolean(flag) => i32::from(*flag).to_string(),
        other => other.to_string(),
    }
}

fn numbers(values: impl IntoIterator<Item = i64>) -> Value {
    Value::Array(values.into_iter().map(Value::Integer).collect())
}

fn items(values: impl IntoIterator<Item = Value>) -> String {
    values.into_iter().map(|value| printed(&value)).collect::<Vec<_>>().join(", ")
}

/// A model's definition: its arrays, in the order the game stores them, then the record pointing at them.
#[must_use]
pub fn definition(symbol: &str, arrays: &Arrays) -> String {
    let triple = |row: &[i16; 3]| numbers(row.iter().copied().map(i64::from));
    let bone = |bone: &pe2_model::model::Bone| {
        let matrix = Value::Array(vec![Value::Array(bone.rotation.iter().map(triple).collect()), numbers(bone.translation.iter().copied().map(i64::from))]);
        Value::Array(vec![matrix, Value::Integer(i64::from(bone.parent))])
    };
    let point = |point: &[i16; 3]| numbers(point.iter().copied().map(i64::from).chain([0]));
    let words = |values: &[u32]| items(values.iter().map(|word| Value::Integer(i64::from(*word))));
    let bones = items(arrays.bones.iter().map(bone));
    let (counts, vertices) = (words(&arrays.part_vertices), items(arrays.vertices.iter().map(point)));
    let (normals, stream) = (items(arrays.normals.iter().map(point)), words(&arrays.stream));
    let (buffer, seam) = (arrays.buffer, arrays.seam);
    format!(
        "static TmdBone _{symbol}Skeleton[] = {{ {bones} }}; \
         static u32 _{symbol}PartVerts[] = {{ {counts} }}; \
         static SVECTOR _{symbol}Verts[] = {{ {vertices} }}; \
         static SVECTOR _{symbol}Normals[] = {{ {normals} }}; \
         static u32 _{symbol}Stream[] = {{ {stream} }}; \
         TmdSource {symbol} = {{ .handlersResolved = 0, .bufferHalfBytes = {buffer}, .preXformRegionBytes = {seam}, \
         .partCount = ARRAY_SIZE(_{symbol}Skeleton), .partVertexCounts = _{symbol}PartVerts, .verts = _{symbol}Verts, \
         .normals = _{symbol}Normals, .skeleton = _{symbol}Skeleton, .stream = _{symbol}Stream }};"
    )
}

fn model_whole(call: &Call, args: &Args) -> Result<String> {
    let (model, bindings) = model_of(call)?;
    Ok(definition(args.one("symbol")?, &arrays(&model, &bindings)?))
}

fn model_buffer(call: &Call, _: &Args) -> Result<String> {
    let (model, bindings) = model_of(call)?;
    Ok(arrays(&model, &bindings)?.buffer.to_string())
}

fn model_seam(call: &Call, _: &Args) -> Result<String> {
    let (model, bindings) = model_of(call)?;
    Ok(arrays(&model, &bindings)?.seam.to_string())
}

fn material_tpage(call: &Call, _: &Args) -> Result<String> {
    let binding = binding_of(call, call.host.fields())?;
    binding.tpage().map(|page| format!("{page:#X}")).ok_or_else(|| anyhow!("it has no texture"))
}

fn material_clut(call: &Call, _: &Args) -> Result<String> {
    let binding = binding_of(call, call.host.fields())?;
    binding.texture.map(|texture| format!("{:#X}", texture.clut())).ok_or_else(|| anyhow!("it has no texture"))
}

/// An image's texels, as the words of a C array.
fn image_words(call: &Call, _: &Args) -> Result<String> {
    let Data::Typed(native) = call.output_of(call.host.path(), "native")? else { bail!("its output is no image") };
    let texels = native.files.iter().find(|file| file.name == "native.texels").ok_or_else(|| anyhow!("it has no texels"))?;
    let bytes = call.read(texels)?;
    let (words, rest) = bytes.as_chunks::<4>();
    if !rest.is_empty() {
        bail!("{} bytes are no words", bytes.len());
    }
    Ok(items(words.iter().map(|word| Value::Integer(i64::from(u32::from_le_bytes(*word))))))
}

fn image_size(call: &Call) -> Result<[i64; 2]> {
    let settings = call.table_of(call.host.path(), SETTINGS)?;
    let pair = settings.get("size").and_then(Value::as_array).ok_or_else(|| anyhow!("it has no size"))?;
    let side = |at: usize| pair.get(at).and_then(Value::as_integer).ok_or_else(|| anyhow!("its size is no pair"));
    Ok([side(0)?, side(1)?])
}

fn image_width_words(call: &Call, _: &Args) -> Result<String> {
    let per = match call.table_of(call.host.path(), SETTINGS)?.get("mode").and_then(Value::as_str) {
        Some("indexed_4bpp") => 4,
        Some("indexed_8bpp") => 2,
        _ => 1,
    };
    Ok((image_size(call)?[0] / per).to_string())
}

fn image_height(call: &Call, _: &Args) -> Result<String> {
    Ok(image_size(call)?[1].to_string())
}

pub fn register(r: &mut Registry) {
    let resolved = r.op("resolved").label("Its settings, resolved").input::<Any>("result").output::<ty::Table>().run(resolved);
    let plain = r.op("plain").label("Its settings").output::<ty::Table>().run(plain);
    r.add_output("pe2.images@image", SETTINGS, &resolved.to_string(), None, pe2_ext::Retain::Transient);
    for kind in ["pe2.images@vram_slot", "pe2.render@mesh", "pe2.render@skeleton", "pe2.render@material", "pe2.render@model"] {
        r.add_output(kind, SETTINGS, &plain.to_string(), None, pe2_ext::Retain::Transient);
    }
    r.tokenizer("pe2.render@model")
        .whole(Yields::Definition("TmdSource"), &["symbol"], model_whole)
        .derived("buffer", Yields::IntLiteral, &[], model_buffer)
        .derived("seam", Yields::IntLiteral, &[], model_seam)
        .register();
    r.tokenizer("pe2.render@material")
        .derived("tpage", Yields::IntLiteral, &[], material_tpage)
        .derived("clut", Yields::IntLiteral, &[], material_clut)
        .register();
    r.tokenizer("pe2.images@image")
        .whole(Yields::BraceInit, &[], image_words)
        .derived("width_words", Yields::IntLiteral, &[], image_width_words)
        .derived("height", Yields::IntLiteral, &[], image_height)
        .register();
    for kind in ["pe2.render@model", "pe2.render@material", "pe2.images@image"] {
        r.add_inspector_tab(kind, "Tokens", pe2_ext::Icon::Text, |_| Box::new(crate::views::Tokens::default()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pe2_model::model::{Model, Part, arrays};

    #[test]
    fn plain_data_prints_as_c() {
        let value: Table = "a = [[1, 2], [3, -4]]\nb = 4294967295\nc = 'name'".parse().unwrap();
        assert_eq!(printed(&value["a"]), "{ { 1, 2 }, { 3, -4 } }");
        assert_eq!(printed(&value["b"]), "0xFFFFFFFF");
        assert_eq!(printed(&value["c"]), "name");
    }

    #[test]
    fn a_definition_holds_the_arrays_before_the_record() {
        let part = Part { parent: 0, rotation: [[4096, 0, 0], [0, 4096, 0], [0, 0, 4096]], translation: [1, -2, 3], vertices: vec![[1, 2, 3]] };
        let held = Model { parts: vec![part], ..Model::default() };
        let text = definition("gBody", &arrays(&held, &[]).unwrap());
        assert!(text.starts_with("static TmdBone _gBodySkeleton[] = { { { { { 4096, 0, 0 }, { 0, 4096, 0 }, { 0, 0, 4096 } }, { 1, -2, 3 } }, 0 } };"), "{text}");
        assert!(text.contains("static SVECTOR _gBodyVerts[] = { { 1, 2, 3, 0 } };"), "{text}");
        assert!(text.ends_with(".stream = _gBodyStream };"), "{text}");
        assert!(text.find("_gBodyStream[]") < text.find("TmdSource gBody"));
    }
}
