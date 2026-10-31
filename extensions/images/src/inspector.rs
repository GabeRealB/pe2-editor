//! The image's parts in the properties pane, each keyed by name: its palettes, each with a swatch of its colors and
//! where it is uploaded; its variants (a texture's each a palette, an atlas's each the cells it recolors); its cells.

use pe2_ext::{Asset, Canvas, ContentOutcome, ContentPlace, ContentRequest, Icon, Placed, Rect, Result, Style, Table, Texture, Ui, Value, View, bail};
use std::collections::HashMap;

use crate::{is_atlas, palette_names, rect};

/// `n` of a plural noun: "1 cell", "8 cells".
fn count_of(n: usize, plural: &str) -> String {
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
    /// An atlas's variants recolor cells; a texture's each take a palette.
    atlas: bool,
    palette_names: Vec<String>,
    palettes: Vec<(String, Vec<(String, Value)>)>,
    variants: Vec<Variant>,
    cells: Vec<(String, String)>,
}

impl Shown {
    fn read(asset: &Asset) -> Shown {
        let palettes = asset
            .parts("palettes")
            .into_iter()
            .map(|(name, p)| {
                let rest = p.as_table().map_or_default(|t| t.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
                (name.to_string(), rest)
            })
            .collect();
        let variants = asset
            .parts("variants")
            .into_iter()
            .map(|(name, v)| Variant {
                title: name.to_string(),
                palette: v.get("palette").and_then(Value::as_str).map(str::to_string),
                cells: v.get("palettes").and_then(Value::as_table).map_or_default(|t| {
                    t.iter().map(|(cell, p)| (cell.clone(), p.as_str().unwrap_or_default().to_string())).collect()
                }),
            })
            .collect();
        let cells = asset
            .parts("cells")
            .into_iter()
            .map(|(name, c)| {
                let palette = c.get("palette").and_then(Value::as_str).map_or_default(|p| format!(", palette {p}"));
                let hint = c.get("rect").and_then(rect).map_or_default(|r| {
                    format!("{} × {} at ({}, {}){palette}", r.width, r.height, r.x, r.y)
                });
                (name.to_string(), hint)
            })
            .collect();
        Shown {
            path: asset.path().to_string(),
            atlas: is_atlas(asset),
            palette_names: palette_names(asset),
            palettes,
            variants,
            cells,
        }
    }
}

/// The sections, keeping each palette's swatch and the names being typed for variants.
#[derive(Default)]
pub(crate) struct Collections {
    shows: Shows,
    swatches: HashMap<String, Option<Texture>>,
    names: HashMap<String, String>,
}

/// What the user did to the variants this frame.
enum Change {
    Rename(usize, String),
    Palette(usize, String),
    Recolor(usize, String, String),
    Keep(usize, String),
    Up(usize),
    Down(usize),
    Move(usize, usize),
    Remove(usize),
    Add,
}

impl Shown {
    /// The variants after a change, as the declaration writes them: none when there are none left.
    fn changed(&self, change: Change) -> Option<Value> {
        let mut variants: Vec<Variant> = self
            .variants
            .iter()
            .map(|v| Variant { title: v.title.clone(), palette: v.palette.clone(), cells: v.cells.clone() })
            .collect();
        match change {
            Change::Rename(i, name) => variants[i].title = name,
            Change::Palette(i, palette) => variants[i].palette = Some(palette),
            Change::Recolor(i, cell, palette) => match variants[i].cells.iter_mut().find(|(c, _)| *c == cell) {
                Some(entry) => entry.1 = palette,
                None => variants[i].cells.push((cell, palette)),
            },
            Change::Keep(i, cell) => variants[i].cells.retain(|(c, _)| *c != cell),
            Change::Up(i) if i > 0 => variants.swap(i, i - 1),
            Change::Down(i) if i + 1 < variants.len() => variants.swap(i, i + 1),
            Change::Up(_) | Change::Down(_) => {}
            Change::Move(from, to) => {
                let variant = variants.remove(from);
                variants.insert(to, variant);
            }
            Change::Remove(i) => {
                variants.remove(i);
            }
            Change::Add => {
                // One more name than there are variants: one of them is free.
                let name = (1..=variants.len() + 1)
                    .map(|n| format!("variant_{n}"))
                    .find(|n| !variants.iter().any(|v| v.title == *n));
                let palette = (!self.atlas).then(|| self.palette_names.first().cloned()).flatten();
                variants.push(Variant { title: name.unwrap_or_default(), palette, cells: vec![] });
            }
        }
        let table: Table = variants
            .into_iter()
            .map(|v| {
                let mut fields = Table::new();
                if let Some(palette) = v.palette {
                    fields.insert("palette".into(), Value::String(palette));
                }
                if self.atlas {
                    let cells = v.cells.into_iter().map(|(c, p)| (c, Value::String(p))).collect();
                    fields.insert("palettes".into(), Value::Table(cells));
                }
                (v.title, Value::Table(fields))
            })
            .collect();
        (!table.is_empty()).then_some(Value::Table(table))
    }
}

impl Collections {
    fn swatch_of(&mut self, asset: &Asset, palette: &str) {
        self.swatches.entry(palette.to_string()).or_insert_with(|| {
            let strip = crate::output_picture(asset, &format!("palettes/{palette}"))?.ok()?;
            Some(Texture::new(format!("{}#palettes/{palette}#swatch", asset.path()), strip))
        });
    }
}

/// A variant's name, editable: a rename is committed when it is not empty and no other variant has it.
fn name_ui(ui: &mut Ui, shown: &Shown, names: &mut HashMap<String, String>, i: usize) -> Option<Change> {
    let name = &shown.variants[i].title;
    if !ui.editable() {
        ui.styled(name, Style::Strong);
        return None;
    }
    let typed = names.entry(name.clone()).or_insert_with(|| name.clone());
    if !ui.text_edit(&format!("{}#variants/{i}/name", shown.path), typed) || typed == name {
        return None;
    }
    let new = typed.trim().to_string();
    typed.clone_from(name);
    let taken = shown.variants.iter().any(|v| v.title == new);
    (!new.is_empty() && !taken).then_some(Change::Rename(i, new))
}

/// A variant's place and removal.
fn order_ui(ui: &mut Ui, i: usize, count: usize, change: &mut Option<Change>) {
    if !ui.editable() {
        return;
    }
    if i > 0 && ui.icon_button(Icon::CaretUp, "Move it up: the first variant is the default look") {
        *change = Some(Change::Up(i));
    }
    if i + 1 < count && ui.icon_button(Icon::CaretDown, "Move it down") {
        *change = Some(Change::Down(i));
    }
    if ui.icon_button(Icon::Cross, "Remove the variant") {
        *change = Some(Change::Remove(i));
    }
}

/// The variants: a texture's each with its palette, an atlas's each with the cells it recolors; edited in place where
/// the asset can be.
fn variants_ui(
    ui: &mut Ui,
    shown: &Shown,
    names: &mut HashMap<String, String>,
    swatches: &HashMap<String, Option<Texture>>,
) -> Option<Change> {
    let swatch_of = |name: &str| swatches.get(name).and_then(Option::as_ref);
    let editable = ui.editable();
    if shown.variants.is_empty() && !editable {
        return None;
    }
    let mut change = None;
    let count = shown.variants.len();
    ui.section(&format!("Variants  ·  {count}"), |ui| {
        for (i, variant) in shown.variants.iter().enumerate() {
            if !shown.atlas {
                let palette = variant.palette.clone().unwrap_or_default();
                ui.wrapped(|ui| {
                    if let Some(renamed) = name_ui(ui, shown, names, i) {
                        change = Some(renamed);
                    }
                    small_swatch(ui, swatch_of(&palette));
                    if editable {
                        let id = format!("{}#variants/{i}/palette", shown.path);
                        if let Some(p) = ui.combo(&id, &palette, &shown.palette_names) {
                            change = Some(Change::Palette(i, shown.palette_names[p].clone()));
                        }
                    } else {
                        ui.styled(&palette, Style::Note);
                    }
                    order_ui(ui, i, count, &mut change);
                });
                continue;
            }
            let id = format!("{}#variants/{i}", shown.path);
            ui.collapsing(&id, &variant.title, &count_of(variant.cells.len(), "cells"), |ui| {
                ui.wrapped(|ui| {
                    if let Some(renamed) = name_ui(ui, shown, names, i) {
                        change = Some(renamed);
                    }
                    order_ui(ui, i, count, &mut change);
                });
                for (cell, palette) in &variant.cells {
                    ui.wrapped(|ui| {
                        ui.label(cell);
                        ui.styled("→", Style::Weak);
                        small_swatch(ui, swatch_of(palette));
                        if editable {
                            let id = format!("{}#variants/{i}/{cell}", shown.path);
                            if let Some(p) = ui.combo(&id, palette, &shown.palette_names) {
                                change = Some(Change::Recolor(i, cell.clone(), shown.palette_names[p].clone()));
                            }
                            if ui.icon_button(Icon::Cross, "Keep the cell's own palette in this variant") {
                                change = Some(Change::Keep(i, cell.clone()));
                            }
                        } else {
                            ui.styled(palette, Style::Note);
                        }
                    });
                }
                let others: Vec<String> = shown
                    .cells
                    .iter()
                    .map(|(c, _)| c.clone())
                    .filter(|c| !variant.cells.iter().any(|(v, _)| v == c))
                    .collect();
                if editable && !others.is_empty() && !shown.palette_names.is_empty() {
                    let id = format!("{}#variants/{i}/add", shown.path);
                    if let Some(c) = ui.combo(&id, "Recolor a cell…", &others) {
                        change = Some(Change::Recolor(i, others[c].clone(), shown.palette_names[0].clone()));
                    }
                }
            });
        }
        if editable && ui.button("Add variant") {
            change = Some(Change::Add);
        }
    });
    change
}

/// A palette's swatch, its width by its colors.
fn swatch(ui: &mut Ui, texture: Option<&Texture>) {
    if let Some(texture) = texture {
        ui.image(texture, if texture.width() <= 16 { 96.0 } else { 160.0 }, 10.0);
    }
}

/// A palette's swatch in a row with controls beside it: narrow, whatever its colors.
fn small_swatch(ui: &mut Ui, texture: Option<&Texture>) {
    if let Some(texture) = texture {
        ui.image(texture, 96.0, 10.0);
    }
}

/// Which of an image's collections a tab of the inspector shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Shows {
    #[default]
    Palettes,
    Cells,
}

impl Collections {
    pub(crate) fn of(shows: Shows) -> Collections {
        Collections { shows, ..Collections::default() }
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
        if self.shows == Shows::Cells {
            let selected = ui.selected();
            if !shown.cells.is_empty() {
                ui.section(&format!("Cells  ·  {}", shown.cells.len()), |ui| {
                    ui.wrapped(|ui| {
                        for (name, hint) in &shown.cells {
                            let part = format!("cells/{name}");
                            if ui.chip(name, selected.as_deref() == Some(part.as_str())) {
                                ui.select(&part);
                            }
                            if !hint.is_empty() {
                                ui.tooltip(hint);
                            }
                        }
                    });
                });
            }
            return;
        }
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
        let names = &mut self.names;
        let change = variants_ui(ui, &shown, names, swatches);
        if let Some(change) = change {
            let value = shown.changed(change);
            ui.set("variants", value.as_ref());
        }
    }
}

/// What comes of something done in an image's contents: the variants' rows act on them as the inspector's controls do.
pub(crate) fn act(asset: &Asset, _: &dyn pe2_ext::Project, request: ContentRequest) -> Result<ContentOutcome> {
    let shown = Shown::read(asset);
    let variant = |row: &str| row.strip_prefix("variants/").and_then(|name| shown.variants.iter().position(|v| v.title == name));
    let of_variants = |label: String, change, select| ContentOutcome {
        label,
        sets: vec![(vec!["variants".to_string()], shown.changed(change))],
        select,
    };
    match request {
        ContentRequest::Action { action, .. } if action == "add" => Ok(of_variants("Add a variant".to_string(), Change::Add, None)),
        ContentRequest::Action { row, action } if action == "remove" => match variant(&row) {
            Some(i) => Ok(of_variants(format!("Remove {}", shown.variants[i].title), Change::Remove(i), None)),
            None => bail!("no such variant"),
        },
        ContentRequest::Rename { row, name } => {
            let Some(i) = variant(&row) else { bail!("no such variant") };
            if name.is_empty() || shown.variants.iter().any(|v| v.title == name) {
                bail!("a variant needs a name of its own");
            }
            let select = format!("variants/{name}");
            Ok(of_variants(format!("Rename {} to {name}", shown.variants[i].title), Change::Rename(i, name), Some(select)))
        }
        ContentRequest::Moved { row, onto, place } => {
            if let (Some(cell), Some(palette)) = (row.strip_prefix("cells/"), onto.strip_prefix("palettes/")) {
                return Ok(ContentOutcome {
                    label: format!("Draw {cell} through {palette}"),
                    sets: vec![(vec!["cells".to_string(), cell.to_string(), "palette".to_string()], Some(Value::String(palette.to_string())))],
                    select: None,
                });
            }
            let (Some(from), Some(by)) = (variant(&row), variant(&onto)) else { bail!("it does not go there") };
            let by = if from < by { by - 1 } else { by };
            let to = if place == ContentPlace::After { by + 1 } else { by };
            Ok(of_variants(format!("Move {}", shown.variants[from].title), Change::Move(from, to), None))
        }
        ContentRequest::Action { action, .. } => bail!("an image has no action {action}"),
        ContentRequest::Dropped { .. } => bail!("nothing is dropped on an image's rows"),
    }
}

/// The head of the contents pane: how much of each an image holds.
pub(crate) struct Summary;

impl View for Summary {
    fn show(&mut self, ui: &mut Ui) {
        let said = {
            let Some(asset) = ui.asset() else { return };
            let shown = Shown::read(&asset);
            let counts = [(shown.cells.len(), "cells"), (shown.palette_names.len(), "palettes"), (shown.variants.len(), "variants")];
            let said: Vec<String> = counts.iter().filter(|(n, _)| *n > 0).map(|(n, plural)| count_of(*n, plural)).collect();
            said.join("  ·  ")
        };
        if !said.is_empty() {
            ui.styled(&said, Style::Note);
        }
    }
}

/// The Cells editor: an atlas's cells to pick from, the one selected at its own size, zoomed and panned as any picture is.
#[derive(Default)]
pub(crate) struct CellSheet {
    pictures: HashMap<String, Option<(Texture, f32, f32)>>,
}

impl CellSheet {
    fn picture_of(&mut self, asset: &Asset, part: &str) -> Option<(Texture, f32, f32)> {
        let picture = self.pictures.entry(part.to_string()).or_insert_with(|| {
            let image = crate::output_picture(asset, part)?.ok()?;
            #[expect(clippy::cast_precision_loss, reason = "picture sizes")]
            let (width, height) = (image.width as f32, image.height as f32);
            Some((Texture::new(format!("{}#{part}#sheet", asset.path()), image), width, height))
        });
        picture.clone()
    }
}

impl View for CellSheet {
    fn show(&mut self, ui: &mut Ui) {
        let selected = ui.selected();
        let (names, shown) = {
            let Some(asset) = ui.asset() else { return };
            let names: Vec<String> = crate::cells(&asset).into_iter().map(|cell| cell.name).collect();
            let first = names.first().map(|name| format!("cells/{name}"));
            let part = selected.clone().filter(|part| part.starts_with("cells/")).or(first);
            let shown = part.and_then(|part| Some((self.picture_of(&asset, &part)?, part)));
            (names, shown)
        };
        ui.wrapped(|ui| {
            for name in &names {
                let part = format!("cells/{name}");
                if ui.chip(name, selected.as_deref() == Some(part.as_str())) {
                    ui.select(&part);
                }
            }
        });
        let Some(((texture, width, height), part)) = shown else { return };
        let rect = Rect { x: 0.0, y: 0.0, width, height };
        ui.canvas(&Canvas {
            width,
            height,
            pictures: vec![Placed { texture, rect, label: None, link: None }],
            boxes: vec![],
            summary: part,
            toggle: String::new(),
            legend: false,
            grid: None,
            traces: Vec::new(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{Change, Shown, Variant};
    use pe2_ext::Value;

    fn atlas() -> Shown {
        Shown {
            path: "test".into(),
            atlas: true,
            palette_names: vec!["warm".into(), "cold".into()],
            palettes: vec![],
            variants: vec![
                Variant { title: "night".into(), palette: None, cells: vec![("dot".into(), "cold".into())] },
                Variant { title: "dusk".into(), palette: None, cells: vec![] },
            ],
            cells: vec![("dot".into(), String::new()), ("line".into(), String::new())],
        }
    }

    fn names(value: Option<&Value>) -> Vec<String> {
        value.and_then(Value::as_table).map_or_default(|t| t.keys().cloned().collect())
    }

    #[test]
    fn variants_change_as_asked() {
        let shown = atlas();
        assert_eq!(names(shown.changed(Change::Rename(0, "midnight".into())).as_ref()), ["midnight", "dusk"]);
        assert_eq!(names(shown.changed(Change::Down(0)).as_ref()), ["dusk", "night"]);
        assert_eq!(names(shown.changed(Change::Up(0)).as_ref()), ["night", "dusk"]);
        assert_eq!(names(shown.changed(Change::Add).as_ref()), ["night", "dusk", "variant_1"]);
        let recolored = shown.changed(Change::Recolor(1, "line".into(), "warm".into())).unwrap();
        assert_eq!(recolored["dusk"]["palettes"]["line"].as_str(), Some("warm"));
        let kept = shown.changed(Change::Keep(0, "dot".into())).unwrap();
        assert!(kept["night"]["palettes"].as_table().unwrap().is_empty());
        let removed = shown.changed(Change::Remove(1));
        assert_eq!(names(removed.as_ref()), ["night"]);
        let one = Shown { variants: vec![Variant { title: "only".into(), palette: None, cells: vec![] }], ..atlas() };
        assert_eq!(one.changed(Change::Remove(0)), None);
    }
}
