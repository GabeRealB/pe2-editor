//! What the game's packages need of each type: the forms the game stores them in, as C. A target with other forms overrides
//! these (`Tokenizer::target`). What is read of other nodes is read through their `settings` and their outputs, so what is
//! made follows what it reached, and nothing here is a node's output of its own: a model is just a model.

use pe2_ext::tokens::{Args, Yields};
use pe2_ext::{Call, Data, Registry, Result, Table, Value, anyhow, bail};
use pe2_model::material;
use pe2_model::mesh;
use pe2_model::model::{Arrays, Binding, Model, arrays};
use pe2_model::skeleton;

pub const SETTINGS: &str = "settings";

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

/// A mesh at rest on its skeleton, the joints a prefab moves where it moves them, and the names of its slots.
fn rested(call: &Call, reference: &str, pose: Option<&Value>) -> Result<(Model, Vec<String>)> {
    let Data::Typed(stored) = call.output_of(reference, "native")? else { bail!("{reference}: its output is no mesh") };
    let (mut model, slots) = mesh::read(&String::from_utf8(call.read(stored.file()?)?)?)?;
    if let Some(named) = call.table_of(reference, SETTINGS)?.get("skeleton").and_then(pe2_ext::target_of) {
        let table = call.table_of(named, SETTINGS)?;
        let joints = skeleton::parse(table.get("joints").and_then(Value::as_array).map_or(&[][..], Vec::as_slice))?;
        mesh::pose(&mut model, &joints)?;
        for (part, joint) in model.parts.iter_mut().zip(&joints) {
            if let Some(pose) = pose.and_then(|pose| pose.get(&joint.name)) {
                skeleton::posed(part, pose);
            }
        }
    }
    Ok((model, slots))
}

/// A model's arrays' input: a model's settings, with what holds it over them where a prefab does.
fn model_as(call: &Call, fields: &Table, fill: Option<&Value>, pose: Option<&Value>) -> Result<(Model, Vec<Binding>)> {
    let reference = fields.get("mesh").and_then(pe2_ext::target_of).ok_or_else(|| anyhow!("it names no mesh"))?;
    let (mut model, slots) = rested(call, reference, pose)?;
    if let (Some(fill), Some(root)) = (fill, model.parts.first_mut()) {
        skeleton::held(root, fill);
    }
    let materials = fields.get("materials");
    let mut bindings = Vec::with_capacity(slots.len());
    for slot in &slots {
        let overrides = fill.and_then(|fill| fill.get("overrides")?.get(slot));
        let named = overrides.and_then(|overrides| overrides.get("material")).or_else(|| materials.and_then(|materials| materials.get(slot)));
        let filled = named.and_then(pe2_ext::target_of).ok_or_else(|| anyhow!("its slot {slot} is empty"))?;
        let mut settings = call.table_of(filled, SETTINGS)?;
        for (key, value) in overrides.and_then(Value::as_table).into_iter().flatten().filter(|(key, _)| *key != "material") {
            settings.insert(key.clone(), value.clone());
        }
        bindings.push(binding_of(call, &settings).map_err(|why| anyhow!("{slot}: {why:#}"))?);
    }
    Ok((model, bindings))
}

fn model_of(call: &Call) -> Result<(Model, Vec<Binding>)> {
    model_as(call, &call.given(), None, None)
}

/// What a member of a prefab's group fills a part with, as the prefab holds it: `member` is its address.
fn member_of(call: &Call, args: &Args) -> Result<(Model, Vec<Binding>)> {
    let member = args.one("member")?;
    let fields = call.given();
    let fill = member.split('/').fold(fields.get("groups"), |at, step| at?.get(step));
    let fill = fill.ok_or_else(|| anyhow!("it has no member {member}"))?;
    let model = fill.get("model").and_then(pe2_ext::target_of).ok_or_else(|| anyhow!("{member} holds no model"))?;
    model_as(call, &call.table_of(model, SETTINGS)?, Some(fill), fields.get("pose"))
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

fn member_whole(call: &Call, args: &Args) -> Result<String> {
    let (model, bindings) = member_of(call, args)?;
    Ok(definition(args.one("symbol")?, &arrays(&model, &bindings)?))
}

fn member_buffer(call: &Call, args: &Args) -> Result<String> {
    let (model, bindings) = member_of(call, args)?;
    Ok(arrays(&model, &bindings)?.buffer.to_string())
}

fn member_seam(call: &Call, args: &Args) -> Result<String> {
    let (model, bindings) = member_of(call, args)?;
    Ok(arrays(&model, &bindings)?.seam.to_string())
}

fn material_tpage(call: &Call, _: &Args) -> Result<String> {
    let binding = binding_of(call, &call.given())?;
    binding.tpage().map(|page| format!("{page:#X}")).ok_or_else(|| anyhow!("it has no texture"))
}

fn material_clut(call: &Call, _: &Args) -> Result<String> {
    let binding = binding_of(call, &call.given())?;
    binding.texture.map(|texture| format!("{:#X}", texture.clut())).ok_or_else(|| anyhow!("it has no texture"))
}

/// An image's texels, as the initializer of a C array of its words.
fn image_words(call: &Call, _: &Args) -> Result<String> {
    let Some(Data::Typed(native)) = call.inputs.first().and_then(|port| port.first()) else { bail!("it is given no image") };
    let texels = native.files.iter().find(|file| file.name == "native.texels").ok_or_else(|| anyhow!("it has no texels"))?;
    let bytes = call.read(texels)?;
    let (words, rest) = bytes.as_chunks::<4>();
    if !rest.is_empty() {
        bail!("{} bytes are no words", bytes.len());
    }
    Ok(format!("{{ {} }}", items(words.iter().map(|word| Value::Integer(i64::from(u32::from_le_bytes(*word)))))))
}

fn image_size(call: &Call) -> Result<[i64; 2]> {
    let settings = call.given();
    let pair = settings.get("size").and_then(Value::as_array).ok_or_else(|| anyhow!("it has no size"))?;
    let side = |at: usize| pair.get(at).and_then(Value::as_integer).ok_or_else(|| anyhow!("its size is no pair"));
    Ok([side(0)?, side(1)?])
}

fn image_width_words(call: &Call, _: &Args) -> Result<String> {
    let per = match call.given().get("mode").and_then(Value::as_str) {
        Some("indexed_4bpp") => 4,
        Some("indexed_8bpp") => 2,
        _ => 1,
    };
    Ok((image_size(call)?[0] / per).to_string())
}

fn image_height(call: &Call, _: &Args) -> Result<String> {
    Ok(image_size(call)?[1].to_string())
}

/// An image's depth in bits, as its `mode` says.
fn image_bits(call: &Call) -> i64 {
    match call.given().get("mode").and_then(Value::as_str) {
        Some("indexed_4bpp") => 4,
        Some("indexed_8bpp") => 8,
        _ => 16,
    }
}

/// Where an image is in VRAM, in halfwords: its `page` placement.
fn image_placed(call: &Call) -> Result<[i64; 2]> {
    let page = call.given().get("page").cloned().ok_or_else(|| anyhow!("it is placed nowhere"))?;
    placed(call, &page).ok_or_else(|| anyhow!("its page names no VRAM slot"))
}

/// The texture page an image is in: its VRAM origin, in halfwords.
fn image_page(call: &Call) -> Result<[i64; 2]> {
    let [x, y] = image_placed(call)?;
    Ok([x / 64 * 64, y / 256 * 256])
}

fn image_page_x(call: &Call, _: &Args) -> Result<String> {
    Ok(image_page(call)?[0].to_string())
}

fn image_page_y(call: &Call, _: &Args) -> Result<String> {
    Ok(image_page(call)?[1].to_string())
}

/// A texture page's depth as the GPU is told it: 0 for 4 bits, 1 for 8, 2 for 16.
#[expect(clippy::unnecessary_wraps, reason = "a tokenizer's entry may fail; this one does not")]
fn image_depth(call: &Call, _: &Args) -> Result<String> {
    Ok(match image_bits(call) {
        4 => "0",
        8 => "1",
        _ => "2",
    }
    .to_string())
}

/// Where an image's `default` palette is in VRAM, else its first's.
fn image_palette(call: &Call) -> Result<[i64; 2]> {
    let given = call.given();
    let palettes = given.get("palettes").and_then(Value::as_table).ok_or_else(|| anyhow!("it has no palettes"))?;
    let palette = palettes.get("default").or_else(|| palettes.values().next()).ok_or_else(|| anyhow!("it has no palettes"))?;
    placed(call, palette).ok_or_else(|| anyhow!("its palette is placed nowhere"))
}

fn image_palette_x(call: &Call, _: &Args) -> Result<String> {
    Ok(image_palette(call)?[0].to_string())
}

fn image_palette_y(call: &Call, _: &Args) -> Result<String> {
    Ok(image_palette(call)?[1].to_string())
}

/// A cell's rectangle in its page's texels: where the image sits in the page, and the cell in the image.
fn image_cell(call: &Call, args: &Args) -> Result<[i64; 4]> {
    let name = args.one("cell")?;
    let given = call.given();
    let rect = given.get("cells").and_then(|cells| cells.get(name)).and_then(|cell| cell.get("rect")).and_then(Value::as_array);
    let rect = rect.ok_or_else(|| anyhow!("it has no cell {name}"))?;
    let side = |at: usize| rect.get(at).and_then(Value::as_integer).ok_or_else(|| anyhow!("cell {name}'s rect is no four numbers"));
    let ([x, y], [page_x, page_y]) = (image_placed(call)?, image_page(call)?);
    Ok([(x - page_x) * (16 / image_bits(call)) + side(0)?, (y - page_y) + side(1)?, side(2)?, side(3)?])
}

fn cell_u(call: &Call, args: &Args) -> Result<String> {
    Ok(image_cell(call, args)?[0].to_string())
}

fn cell_v(call: &Call, args: &Args) -> Result<String> {
    Ok(image_cell(call, args)?[1].to_string())
}

fn cell_w(call: &Call, args: &Args) -> Result<String> {
    Ok(image_cell(call, args)?[2].to_string())
}

fn cell_h(call: &Call, args: &Args) -> Result<String> {
    Ok(image_cell(call, args)?[3].to_string())
}

/// A list's items.
#[expect(clippy::unnecessary_wraps, reason = "a tokenizer's entry may fail; this one does not")]
fn list_count(call: &Call, _: &Args) -> Result<String> {
    Ok(call.given().get("items").and_then(Value::as_array).map_or(0, Vec::len).to_string())
}

/// An item's `id`, as its own tokenizer reads it: the item's output leads to it, with the args it is read by.
fn item_id(call: &Call, at: usize) -> Result<i64> {
    let led = call.table_of(call.host.path(), &format!("items/{at}"))?;
    let node = led.get(pe2_ext::tokens::NODE).and_then(Value::as_str).ok_or_else(|| anyhow!("item {at} leads nowhere"))?;
    let held = led.get(pe2_ext::tokens::ARGS).and_then(Value::as_table).into_iter().flatten();
    let args = Args(held.filter_map(|(name, value)| Some((name.clone(), vec![value.as_str()?.to_string()]))).collect());
    let output = crate::gather::tokenizer_of(call, node)?;
    let address = pe2_ext::tokens::address(Some("id"), &args).replacen(pe2_ext::tokens::OUTPUT, &output, 1);
    let text = pe2_ext::tokens::text_of(call, &call.output_of(node, &address)?)?;
    text.trim().parse().map_err(|_| anyhow!("item {at}'s id is {text}, no number"))
}

/// What each item's id is more than the one before it, the same for each: what code finding an item by its index
/// from the first's adds. A list of fewer than two has none, and is 0.
fn list_step(call: &Call, _: &Args) -> Result<String> {
    let count = call.given().get("items").and_then(Value::as_array).map_or(0, Vec::len);
    let ids = (0..count).map(|at| item_id(call, at)).collect::<Result<Vec<i64>>>()?;
    let Some(step) = ids.windows(2).next().map(|pair| pair[1] - pair[0]) else { return Ok("0".to_string()) };
    if let Some(at) = ids.windows(2).position(|pair| pair[1] - pair[0] != step) {
        bail!("its items' ids are not evenly apart: {} to {} is not {step}", ids[at], ids[at + 1]);
    }
    Ok(step.to_string())
}

/// A file of a group's id: the group's number in its stage's layout, as many ten thousands, and the file's in the group.
fn group_file_id(call: &Call, args: &Args) -> Result<String> {
    let name = args.one("file")?;
    let given = call.given();
    let files = given.get("files").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    let file = files.iter().find(|file| file.get("name").and_then(Value::as_str) == Some(name));
    let id = file.and_then(|file| file.get("id")).and_then(Value::as_integer).ok_or_else(|| anyhow!("it has no file {name}"))?;
    let release = call.config().and_then(|config| config.release).ok_or_else(|| anyhow!("no release is built"))?;
    let layout = call.table_of(&format!("disc/{release}/stage0"), SETTINGS)?;
    let groups = layout.get("groups").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    let here = call.host.path();
    let mine = groups.iter().find(|group| {
        let named = group.get("asset").and_then(pe2_ext::target_of).and_then(|asset| call.node_named(asset));
        named.is_some_and(|named| named.path == here)
    });
    let group = mine.and_then(|group| group.get("id")).and_then(Value::as_integer).ok_or_else(|| anyhow!("stage 0 of {release} lists no {here}"))?;
    Ok((group * 10_000 + id).to_string())
}

pub fn register(r: &mut Registry) {
    r.tokenizer(&["pe2.render@model"])
        .follows()
        .whole(Yields::Definition("TmdSource"), &["symbol"], model_whole)
        .derived("buffer", Yields::IntLiteral, &[], model_buffer)
        .derived("seam", Yields::IntLiteral, &[], model_seam)
        .register();
    // A prefab's member is a model, as the prefab holds it.
    r.tokenizer(&["pe2.render@prefab"])
        .follows()
        .whole(Yields::Definition("TmdSource"), &["symbol", "member"], member_whole)
        .derived("buffer", Yields::IntLiteral, &["member"], member_buffer)
        .derived("seam", Yields::IntLiteral, &["member"], member_seam)
        .register();
    r.tokenizer(&["pe2.render@material"])
        .follows()
        .derived("tpage", Yields::IntLiteral, &[], material_tpage)
        .derived("clut", Yields::IntLiteral, &[], material_clut)
        .register();
    // Where an image is placed is its VRAM slots' to say, read as it goes.
    r.tokenizer(&["pe2.images@texels", "pe2.images@bs_picture"])
        .follows()
        .whole(Yields::Initializer, &[], image_words)
        .derived("width_words", Yields::IntLiteral, &[], image_width_words)
        .derived("height", Yields::IntLiteral, &[], image_height)
        .derived("page_x", Yields::IntLiteral, &[], image_page_x)
        .derived("page_y", Yields::IntLiteral, &[], image_page_y)
        .derived("depth", Yields::IntLiteral, &[], image_depth)
        .derived("palette_x", Yields::IntLiteral, &[], image_palette_x)
        .derived("palette_y", Yields::IntLiteral, &[], image_palette_y)
        .derived("u", Yields::IntLiteral, &["cell"], cell_u)
        .derived("v", Yields::IntLiteral, &["cell"], cell_v)
        .derived("w", Yields::IntLiteral, &["cell"], cell_w)
        .derived("h", Yields::IntLiteral, &["cell"], cell_h)
        .register();
    // A list's values, and through its items what each item is.
    r.tokenizer(&["pe2.packages@list"])
        .follows()
        .derived("count", Yields::IntLiteral, &[], list_count)
        .derived("step", Yields::IntLiteral, &[], list_step)
        .register();
    // A file of a group, as the group reads it (`files/<name>`).
    r.tokenizer(&["pe2.game@file_group"]).follows().derived("id", Yields::IntLiteral, &["file"], group_file_id).register();
    for node_type in ["pe2.render@model", "pe2.render@material", "pe2.images@image"] {
        let _ = r.extend(node_type).inspector_tab("Tokens", pe2_ext::Icon::Text, &[], |_| Box::new(crate::views::Tokens::default())).register();
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
