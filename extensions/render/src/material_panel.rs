//! A material in the inspector: what its shader takes, each slot with what fills it, its picture, and the cell and
//! palette it reads. The shape follows the shader, so that a slot appears and goes as the shader is changed.

use std::collections::HashMap;

use pe2_ext::{Style, Texture, Ui, Value, View};

use crate::material::{self, Definition, LAYER_FULL, MAIN, STRENGTH};

/// How tall a cell's picture is shown.
const PREVIEW: f32 = 128.0;

/// Parts of images a view shows, each read once.
#[derive(Default)]
pub struct Pictures(HashMap<String, Option<Texture>>);

impl Pictures {
    pub fn get(&mut self, ui: &Ui, image: &str, part: &str) -> Option<&Texture> {
        let key = format!("{image}#{part}");
        self.0
            .entry(key.clone())
            .or_insert_with(|| ui.project().image(image, part)?.ok().map(|picture| Texture::new(key, picture)))
            .as_ref()
    }
}

#[derive(Default)]
pub struct Panel {
    pictures: Pictures,
    strength: Option<String>,
}

/// What can be chosen of the image a texture names.
pub struct Choices {
    pub image: String,
    pub cells: Vec<String>,
    pub palettes: Vec<String>,
    /// The palette a cell is declared with, where the texture does not say.
    pub own: Option<String>,
}

pub fn choices_of(ui: &Ui, texture: &Value) -> Option<Choices> {
    let image = ui.project().resolve(texture.get("slot").unwrap_or(texture))?;
    let named = |list: Vec<(&str, &Value)>| list.into_iter().map(|(name, _)| name.to_string()).collect();
    let cell =
        material::cell_of(texture).and_then(|cell| image.parts("cells").into_iter().find(|(name, _)| *name == cell));
    let own = cell.and_then(|(_, cell)| cell.get("palette")).and_then(Value::as_str).map(str::to_string);
    Some(Choices {
        image: image.path().to_string(),
        cells: named(image.parts("cells")),
        palettes: named(image.parts("palettes")),
        own,
    })
}

impl Panel {
    fn slot(&mut self, ui: &mut Ui, key: &str, texture: &Value) {
        ui.field(key, texture);
        let Some(choices) = choices_of(ui, texture) else {
            ui.styled("Its image is not in the project", Style::Error);
            return;
        };
        let cell = material::cell_of(texture).unwrap_or_default().to_string();
        let palette =
            material::palette_of(texture).map(str::to_string).or_else(|| choices.own.clone()).unwrap_or_default();
        let part = material::cell_address(&cell, Some(&palette));
        if let Some(picture) = self.pictures.get(ui, &choices.image, &part).cloned() {
            #[expect(clippy::cast_precision_loss, reason = "a picture is far smaller than floats lose pixels at")]
            let wide = picture.width() as f32 / picture.height().max(1) as f32;
            ui.image(&picture, PREVIEW * wide, PREVIEW);
        }
        ui.horizontal(|ui| {
            ui.label("Cell");
            if let Some(chosen) = ui.combo(&format!("{key}.cell"), &cell, &choices.cells) {
                ui.set(key, Some(&material::reading(texture, &choices.cells[chosen], material::palette_of(texture))));
            }
            ui.label("Palette");
            if let Some(chosen) = ui.combo(&format!("{key}.palette"), &palette, &choices.palettes) {
                ui.set(key, Some(&material::reading(texture, &cell, Some(&choices.palettes[chosen]))));
            }
        });
    }
}

impl Panel {
    /// How far an object is faded to its layer: it is the object's to say in the game, so this says how it is shown.
    fn strength(&mut self, ui: &mut Ui, strength: u8) {
        let typed = self.strength.get_or_insert_with(|| strength.to_string());
        ui.section("Layer strength", |ui| {
            ui.horizontal(|ui| {
                let changed = ui.text_edit("strength", typed);
                ui.styled(&format!("of {LAYER_FULL}"), Style::Note);
                if let Some(new) =
                    typed.trim().parse::<u8>().ok().filter(|new| changed && *new <= LAYER_FULL && *new != strength)
                {
                    ui.set(STRENGTH, Some(&Value::Integer(i64::from(new))));
                }
            });
        });
    }
}

impl View for Panel {
    fn show(&mut self, ui: &mut Ui) {
        let Some(asset) = ui.asset() else { return };
        let read = Definition::read(|key| asset.field(key));
        let slots: Vec<(&str, &str, Option<Value>)> =
            read.shader.textures.iter().map(|(key, label)| (*key, *label, asset.field(key).cloned())).collect();
        let main = asset.field(MAIN).cloned();
        let strength = read.shader.reflective.then_some(read.strength);
        for (key, label, texture) in slots {
            ui.section(label, |ui| {
                if let Some(texture) = &texture {
                    self.slot(ui, key, texture);
                    return;
                }
                ui.styled("Empty", Style::Note);
                if let Some(main) = main.as_ref().filter(|_| ui.editable())
                    && ui.button("Bind the main texture")
                {
                    ui.set(key, Some(main));
                }
            });
        }
        if let Some(strength) = strength {
            self.strength(ui, strength);
        }
    }
}
