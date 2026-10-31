//! A material: a shader, and what fills its shape. The shaders are the game's own ways of drawing a face, so they are
//! this extension's to define: what one is follows from code on the console, and a project cannot add one until
//! something also writes that code. What a shader cannot say stays in the mesh: whether a face takes light is whether
//! it has normals, and one material mixes faces that do with faces that do not.

use pe2_ext::Value;

use crate::model::Blend;

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
/// The layer the game adds over it, at the object's own page and palette offsets.
pub const ENVIRONMENT: &str = "environment";

pub const SHADERS: [Shader; 3] = [
    Shader { id: "textured", label: "Textured", textures: &[(MAIN, "Main texture")], reflective: false },
    Shader { id: "reflective", label: "Reflective", textures: &[(MAIN, "Main texture"), (ENVIRONMENT, "Environment")], reflective: true },
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
        let blend = text("blend").and_then(|id| BLENDS.iter().find(|(known, ..)| *known == id)).map_or(Blend::Mix, |(.., blend)| *blend);
        let strength = field(STRENGTH).and_then(Value::as_integer).and_then(|strength| u8::try_from(strength).ok()).map_or(LAYER_FULL / 2, |strength| strength.min(LAYER_FULL));
        let effect = field(EFFECT).and_then(Value::as_integer).and_then(|effect| u32::try_from(effect).ok()).unwrap_or(0);
        Definition { shader, transparent: text("surface") == Some("transparent"), blend, strength, effect }
    }
}

/// What a texture slot holds: an image, by the reference a table keeps beside the cell and the palette it reads.
#[must_use]
pub fn cell_of(texture: &Value) -> Option<&str> {
    texture.get("cell").and_then(Value::as_str)
}

#[must_use]
pub fn palette_of(texture: &Value) -> Option<&str> {
    texture.get("palette").and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_material_says_how_it_draws() {
        let table: pe2_ext::Table = "shader = 'reflective'\nsurface = 'transparent'\nblend = 'add'".parse().unwrap();
        let read = Definition::read(|key| table.get(key));
        assert_eq!(read, Definition { shader: &SHADERS[1], transparent: true, blend: Blend::Add, strength: LAYER_FULL / 2, effect: 0 });
    }

    #[test]
    fn an_effect_is_a_materials_to_say() {
        let table: pe2_ext::Table = "effect = 3".parse().unwrap();
        assert_eq!(Definition::read(|key| table.get(key)).effect, 3);
    }

    #[test]
    fn what_a_material_leaves_out_is_opaque_and_textured() {
        let read = Definition::read(|_| None);
        assert_eq!(read, Definition { shader: &SHADERS[0], transparent: false, blend: Blend::Mix, strength: LAYER_FULL / 2, effect: 0 });
    }
}
