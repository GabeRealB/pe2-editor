//! A material: a shader, and what fills its shape. The shaders are the game's own ways of drawing a face, so they are
//! this extension's to define: what one is follows from code on the console, and a project cannot add one until
//! something also writes that code. What a shader cannot say stays in the mesh: whether a face takes light is whether
//! it has normals, and one material mixes faces that do with faces that do not.

use pe2_ext::{Result, Table, Value, anyhow, bail};

use crate::model::{Binding, Blend, Placement};

/// A way of drawing, by the textures it takes.
#[derive(Debug, PartialEq, Eq)]
pub struct Shader {
    pub id: &'static str,
    pub label: &'static str,
    /// Its slots: the key each is held under, and what it is called.
    pub textures: &'static [(&'static str, &'static str)],
    /// The second packet of a reflective face, added over the first.
    pub reflective: bool,
}

/// The texture a face's coordinates read.
pub const MAIN: &str = "main";
/// The depth faces read the main texture at, where it is not its image's: the M4A1 grenade launcher's round reads its
/// 4-bit sheet as 8-bit texels.
pub const BITS: &str = "bits";
/// The layer the game adds over it, at the object's own page and palette offsets.
pub const ENVIRONMENT: &str = "environment";

pub const SHADERS: [Shader; 3] = [
    Shader { id: "textured", label: "Textured", textures: &[(MAIN, "Main texture")], reflective: false },
    Shader {
        id: "reflective",
        label: "Reflective",
        textures: &[(MAIN, "Main texture"), (ENVIRONMENT, "Environment")],
        reflective: true,
    },
    Shader { id: "colored", label: "Colored", textures: &[], reflective: false },
];

pub const SURFACES: [(&str, &str); 2] = [("opaque", "Opaque"), ("transparent", "Transparent")];

/// The GPU's four, by what they make of what is behind (B) and the face (F).
pub const BLENDS: [(&str, &str, Blend); 4] = [
    ("mix", "Average  B/2 + F/2", Blend::Mix),
    ("add", "Add  B + F", Blend::Add),
    ("subtract", "Subtract  B - F", Blend::Subtract),
    ("add_quarter", "Add a quarter  B + F/4", Blend::AddQuarter),
];

/// The grey the game fades a reflective model's two packets by, of `LAYER_FULL`.
pub const STRENGTH: &str = "strength";
pub const LAYER_FULL: u8 = 128;
pub const EFFECT: &str = "effect";

/// What a material's fields say, read through what may override them.
#[derive(Debug, PartialEq, Eq)]
pub struct Definition {
    pub shader: &'static Shader,
    pub transparent: bool,
    pub blend: Blend,
    /// The layer's share: an object's at run time, so only what is shown, stored nowhere in a model's bytes.
    pub strength: u8,
    /// The draw routine the package gives its faces: the whole model's, as a reflection is. None is 0.
    pub effect: u32,
}

impl Definition {
    #[must_use]
    pub fn read<'a>(field: impl Fn(&str) -> Option<&'a Value>) -> Definition {
        let text = |key: &str| field(key).and_then(Value::as_str);
        let shader = text("shader").and_then(|id| SHADERS.iter().find(|shader| shader.id == id)).unwrap_or(&SHADERS[0]);
        let blend = text("blend")
            .and_then(|id| BLENDS.iter().find(|(known, ..)| *known == id))
            .map_or(Blend::Mix, |(.., blend)| *blend);
        let strength = field(STRENGTH)
            .and_then(Value::as_integer)
            .and_then(|strength| u8::try_from(strength).ok())
            .map_or(LAYER_FULL / 2, |strength| strength.min(LAYER_FULL));
        let effect =
            field(EFFECT).and_then(Value::as_integer).and_then(|effect| u32::try_from(effect).ok()).unwrap_or(0);
        Definition { shader, transparent: text("surface") == Some("transparent"), blend, strength, effect }
    }
}

/// What a texture slot holds is a reference to a cell of an image: `cells/<cell>`, or `cells/<cell>@<palette>` through a
/// palette other than the cell's own.
fn cell_output(texture: &Value) -> Option<&str> {
    pe2_ext::output_of(texture)?.strip_prefix("cells/")
}

#[must_use]
pub fn cell_of(texture: &Value) -> Option<&str> {
    cell_output(texture).map(|output| output.split_once('@').map_or(output, |(cell, _)| cell))
}

#[must_use]
pub fn palette_of(texture: &Value) -> Option<&str> {
    cell_output(texture)?.split_once('@').map(|(_, palette)| palette)
}

/// The output naming a cell, through a palette where one is named.
#[must_use]
pub fn cell_address(cell: &str, palette: Option<&str>) -> String {
    palette.map_or_else(|| format!("cells/{cell}"), |palette| format!("cells/{cell}@{palette}"))
}

/// A texture naming this cell through this palette, of the image it names.
#[must_use]
pub fn reading(texture: &Value, cell: &str, palette: Option<&str>) -> Value {
    let output = cell_address(cell, palette);
    if let Value::Table(table) = texture {
        let mut table = table.clone();
        table.insert("output".to_string(), output.into());
        return Value::Table(table);
    }
    let reference = texture.as_str().unwrap_or_default();
    let uid = reference.split_once('#').map_or(reference, |(uid, _)| uid);
    format!("{uid}#{output}").into()
}

/// A pair of numbers.
fn pair(value: Option<&Value>) -> Option<[i64; 2]> {
    let values = value?.as_array()?;
    Some([values.first()?.as_integer()?, values.get(1)?.as_integer()?])
}

/// What an image says of itself to what draws it: its depth, where its page and palettes are uploaded, and its cells; `placed`
/// says where a placement (`slot`, `at`) puts what it holds, in VRAM halfwords.
#[must_use]
pub fn image_placement(image: &Table, placed: impl Fn(&Value) -> Option<[i64; 2]>) -> Table {
    let bits = match image.get("mode").and_then(Value::as_str) {
        Some("indexed_8bpp") => 8,
        Some("indexed_4bpp") => 4,
        _ => 16,
    };
    let at = |[x, y]: [i64; 2]| Value::Array(vec![Value::Integer(x), Value::Integer(y)]);
    let mut made = Table::from_iter([("bits".to_string(), Value::Integer(bits))]);
    if let Some(page) = image.get("page").and_then(&placed) {
        made.insert("page".to_string(), at(page));
    }
    if let Some(cells) = image.get("cells") {
        made.insert("cells".to_string(), cells.clone());
    }
    let palettes = image.get("palettes").and_then(Value::as_table).into_iter().flatten();
    let palettes = palettes.filter_map(|(name, palette)| Some((name.clone(), at(placed(palette)?))));
    made.insert("palettes".to_string(), Value::Table(palettes.collect()));
    made
}

/// Where a material's texture is on the console, from what its image says of itself.
///
/// # Errors
/// It names what the image does not hold, or what the console cannot draw.
pub fn placement(image: &Table, texture: &Value) -> Result<Placement> {
    placement_at(image, texture, None)
}

/// Where a material's texture is on the console, its texels read at `bits` where the material says (`BITS`): faces may
/// read a texture deeper than its image is stored.
///
/// # Errors
/// As `placement`.
pub fn placement_at(image: &Table, texture: &Value, bits: Option<i64>) -> Result<Placement> {
    let bits =
        bits.or_else(|| image.get("bits").and_then(Value::as_integer)).ok_or_else(|| anyhow!("it has no depth"))?;
    if !matches!(bits, 4 | 8) {
        bail!("a {bits} bit image is no texture of a face");
    }
    let name = cell_of(texture).ok_or_else(|| anyhow!("no cell is named"))?;
    let cell = image.get("cells").and_then(|cells| cells.get(name)).ok_or_else(|| anyhow!("no cell {name}"))?;
    let palette = palette_of(texture).or_else(|| cell.get("palette").and_then(Value::as_str));
    let palette = palette.ok_or_else(|| anyhow!("{name} has no palette"))?;
    let colors = pair(image.get("palettes").and_then(|palettes| palettes.get(palette)))
        .ok_or_else(|| anyhow!("{palette} is in no slot"))?;
    let [x, y] = pair(cell.get("rect")).ok_or_else(|| anyhow!("{name} has no place"))?;
    let [page_x, page_y] = pair(image.get("page")).ok_or_else(|| anyhow!("no page"))?;
    let narrow = |value: i64| u16::try_from(value).map_err(|_| anyhow!("{value} is off the console's memory"));
    let page = [narrow((page_x + x * bits / 16) / 64 * 64)?, narrow((page_y + y) / 256 * 256)?];
    Ok(Placement { bits: u8::try_from(bits)?, page, palette: [narrow(colors[0])?, narrow(colors[1])?] })
}

/// A material as a slot of a model draws it, its texture placed by `place`.
///
/// # Errors
/// As what `place` says.
pub fn binding(fields: &Table, place: impl Fn(&Value, Option<i64>) -> Result<Placement>) -> Result<Binding> {
    let read = Definition::read(|key| fields.get(key));
    let texture = if read.shader.textures.is_empty() {
        None
    } else {
        Some(place(
            fields.get(MAIN).ok_or_else(|| anyhow!("it has no texture"))?,
            fields.get(BITS).and_then(Value::as_integer),
        )?)
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

    #[test]
    fn a_texture_is_placed_by_what_its_image_says() {
        let image: Table = "bits = 8\npage = [384, 256]\ncells.right = { rect = [128, 0, 128, 256], palette = 'one' }\npalettes.one = [0, 245]\npalettes.two = [0, 246]".parse().unwrap();
        let texture = Value::String("uid://x#cells/right@two".to_string());
        assert_eq!(placement(&image, &texture).unwrap(), Placement { bits: 8, page: [448, 256], palette: [0, 246] });
    }

    #[test]
    fn a_texture_names_a_cell_and_a_palette_of_its_image() {
        let table: pe2_ext::Table =
            "main = { uid = 'uid://x', path = 'res://image.node', output = 'cells/left@two' }".parse().unwrap();
        let written = &table["main"];
        assert_eq!((cell_of(written), palette_of(written)), (Some("left"), Some("two")));
        let again = reading(written, "right", None);
        assert_eq!((cell_of(&again), palette_of(&again)), (Some("right"), None));
        assert_eq!(again["uid"].as_str(), Some("uid://x"));
        let extracted = Value::String("uid://x#cells/left".to_string());
        assert_eq!(reading(&extracted, "left", Some("two")).as_str(), Some("uid://x#cells/left@two"));
    }

    #[test]
    fn a_material_says_how_it_draws() {
        let table: pe2_ext::Table = "shader = 'reflective'\nsurface = 'transparent'\nblend = 'add'".parse().unwrap();
        let read = Definition::read(|key| table.get(key));
        assert_eq!(
            read,
            Definition {
                shader: &SHADERS[1],
                transparent: true,
                blend: Blend::Add,
                strength: LAYER_FULL / 2,
                effect: 0
            }
        );
    }

    #[test]
    fn an_effect_is_a_materials_to_say() {
        let table: pe2_ext::Table = "effect = 3".parse().unwrap();
        assert_eq!(Definition::read(|key| table.get(key)).effect, 3);
    }

    #[test]
    fn what_a_material_leaves_out_is_opaque_and_textured() {
        let read = Definition::read(|_| None);
        assert_eq!(
            read,
            Definition {
                shader: &SHADERS[0],
                transparent: false,
                blend: Blend::Mix,
                strength: LAYER_FULL / 2,
                effect: 0
            }
        );
    }
}
