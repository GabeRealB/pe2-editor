//! The image's sections in the inspector: its palettes, each with a swatch of its colors; its variants (a texture's
//! each a palette, an atlas's each the cells it recolors); its cells.

use pe2_ext::{Asset, Picture, Style, Texture, Ui, Value, View};
use std::collections::HashMap;

use crate::{Pictures, palette_names, rect};

/// A list's entry's title: its name, else its place.
fn title(item: &Value, i: usize) -> String {
    item.get("name").and_then(Value::as_str).map_or_else(|| format!("#{i}"), str::to_string)
}

/// `n` of a plural noun: "1 cell", "8 cells".
fn count(n: usize, plural: &str) -> String {
    if n == 1 { format!("1 {}", plural.strip_suffix('s').unwrap_or(plural)) } else { format!("{n} {plural}") }
}

/// A variant: a texture's (its palette) or an atlas's (each cell it recolors, with the palette it takes).
struct Variant {
    title: String,
    palette: Option<String>,
    cells: Vec<(String, String)>,
}

/// What the sections show, read from the asset before drawing.
struct Shown {
    path: String,
    palettes: Vec<(String, Vec<(String, Value)>)>,
    variants: Vec<Variant>,
    cells: Vec<(String, String)>,
}

impl Shown {
    fn read(asset: &Asset) -> Shown {
        let palettes = asset
            .array("palettes")
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let rest = p.as_table().map_or_default(|t| {
                    t.iter().filter(|(k, _)| *k != "name").map(|(k, v)| (k.clone(), v.clone())).collect()
                });
                (title(p, i), rest)
            })
            .collect();
        let variants = asset
            .array("variants")
            .iter()
            .enumerate()
            .map(|(i, v)| Variant {
                title: title(v, i),
                palette: v.get("palette").and_then(Value::as_str).map(str::to_string),
                cells: v.get("palettes").and_then(Value::as_table).map_or_default(|t| {
                    t.iter().map(|(cell, p)| (cell.clone(), p.as_str().unwrap_or_default().to_string())).collect()
                }),
            })
            .collect();
        let cells = asset
            .array("cells")
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let palette = c.get("palette").and_then(Value::as_str).map_or_default(|p| format!(", palette {p}"));
                let hint = c.get("rect").and_then(rect).map_or_default(|r| {
                    format!("{} × {} at ({}, {}){palette}", r.width, r.height, r.x, r.y)
                });
                (title(c, i), hint)
            })
            .collect();
        Shown { path: asset.path().to_string(), palettes, variants, cells }
    }
}

/// The sections, keeping each palette's swatch.
#[derive(Default)]
pub(crate) struct Collections {
    swatches: HashMap<String, Option<Texture>>,
}

impl Collections {
    fn swatch_of(&mut self, asset: &Asset, palette: &str) {
        self.swatches.entry(palette.to_string()).or_insert_with(|| {
            let strip = Pictures.part(asset, &format!("palettes/{palette}"))?.ok()?;
            Some(Texture::new(format!("{}#palettes/{palette}#swatch", asset.path()), strip))
        });
    }
}

/// A palette's swatch, its width by its colors.
fn swatch(ui: &mut Ui, texture: Option<&Texture>) {
    if let Some(texture) = texture {
        ui.image(texture, if texture.width() <= 16 { 96.0 } else { 160.0 }, 10.0);
    }
}

impl View for Collections {
    fn show(&mut self, ui: &mut Ui) {
        let shown = {
            let Some(asset) = ui.asset() else { return };
            for palette in palette_names(&asset) {
                self.swatch_of(&asset, &palette);
            }
            Shown::read(&asset)
        };
        let swatches = &self.swatches;
        let swatch_of = |name: &str| swatches.get(name).and_then(Option::as_ref);
        if !shown.palettes.is_empty() {
            ui.section(&format!("Palettes  ·  {}", shown.palettes.len()), |ui| {
                for (name, rest) in &shown.palettes {
                    ui.wrapped(|ui| {
                        ui.styled(name, Style::Strong);
                        swatch(ui, swatch_of(name));
                        for (key, value) in rest {
                            ui.field(key, value);
                        }
                    });
                }
            });
        }
        if !shown.variants.is_empty() {
            ui.section(&format!("Variants  ·  {}", shown.variants.len()), |ui| {
                for (i, variant) in shown.variants.iter().enumerate() {
                    if let Some(palette) = &variant.palette {
                        ui.indented(|ui| {
                            ui.horizontal(|ui| {
                                ui.styled(&variant.title, Style::Strong);
                                swatch(ui, swatch_of(palette));
                                ui.styled(palette, Style::Note);
                            });
                        });
                        continue;
                    }
                    let id = format!("{}#variants/{i}", shown.path);
                    ui.collapsing(&id, &variant.title, &count(variant.cells.len(), "cells"), |ui| {
                        for (cell, palette) in &variant.cells {
                            ui.horizontal(|ui| {
                                ui.label(cell);
                                ui.styled("→", Style::Weak);
                                swatch(ui, swatch_of(palette));
                                ui.styled(palette, Style::Note);
                            });
                        }
                    });
                }
            });
        }
        if !shown.cells.is_empty() {
            ui.section(&format!("Cells  ·  {}", shown.cells.len()), |ui| {
                ui.wrapped(|ui| {
                    for (name, hint) in &shown.cells {
                        ui.chip(name);
                        if !hint.is_empty() {
                            ui.tooltip(hint);
                        }
                    }
                });
            });
        }
    }
}
