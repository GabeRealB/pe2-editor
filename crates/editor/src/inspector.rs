//! The inspector: the selected asset's declaration shown as properties, not as TOML. A header names the asset (its
//! icon, kind and role, a thumbnail for a picture); a property sheet gives each field a readable label and its value
//! the form its meaning calls for - a depth, a size, a VRAM offset or rectangle, a placement, a reference as a chip
//! that opens its asset; lists (slots, variants, parts, cells) are rows or cards with a summary, a variant with its
//! palette's colors; the bundle's files and the originals' sha1s close it.

use eframe::egui::{self, Color32, RichText};
use project::index::Asset;

use crate::external::{self, Category};
use crate::settings::{Tools, Upscale};
use crate::workspace::{Workspace, role};
use crate::{icons, theme};

/// Fields the header shows, or that only extraction writes.
const HEADER_FIELDS: [&str; 4] = ["uid", "kind", "role", "original"];
const ROW_HEIGHT: f32 = 22.0;
const THUMBNAIL: f32 = 64.0;

/// Where in the declaration a value is: the asset's own fields, or an atlas part's (whose variants are that part's).
#[derive(Clone)]
struct Context {
    piece: Option<String>,
}

pub fn inspector_ui(
    ui: &mut egui::Ui,
    ws: &mut Workspace,
    subject: Option<&str>,
    nav: &mut Option<String>,
    tools: &Tools,
    message: &mut Option<String>,
) {
    let Some(asset) = subject.and_then(|s| ws.index.assets.get(s)).cloned() else {
        if let Some((path, entry)) = subject.and_then(|s| ws.entry_at(s)) {
            entry_ui(ui, path, entry, tools, message);
        } else {
            ui.label(RichText::new("Select an asset").color(theme::weak()));
        }
        return;
    };
    header_ui(ui, ws, &asset);
    let context = Context { piece: matches!(role(&asset), Some("texture" | "atlas")).then(|| asset.path.clone()) };
    let fields: Vec<(&String, &toml::Value)> =
        asset.fields().iter().filter(|(k, _)| !HEADER_FIELDS.contains(&k.as_str())).collect();
    let (lists, properties): (Vec<_>, Vec<_>) = fields.into_iter().partition(|(_, v)| is_table_list(v));
    if !properties.is_empty() {
        theme::section(ui, "Properties", |ui| property_sheet(ui, ws, &properties, &context, nav));
    }
    for (key, value) in lists {
        let items = value.as_array().map_or(&[][..], Vec::as_slice);
        theme::section(ui, &format!("{}  ·  {}", label_of(key), items.len()), |ui| {
            collection_ui(ui, ws, &asset, key, items, &context, nav);
        });
    }
    files_ui(ui, &asset, tools, message);
}

// --- header -----------------------------------------------------------------------------------------------------------

fn header_ui(ui: &mut egui::Ui, ws: &mut Workspace, asset: &Asset) {
    ui.horizontal(|ui| {
        if ws.has_picture(&asset.path) {
            let ctx = ui.ctx().clone();
            if let Some(Ok(shown)) = ws.picture(&ctx, &asset.path, Upscale::Nearest) {
                let scale = (THUMBNAIL / shown.size.y).min(THUMBNAIL * 2.0 / shown.size.x);
                egui::Frame::new().stroke(egui::Stroke::new(1.0, theme::border())).show(ui, |ui| {
                    ui.add(egui::Image::new(egui::load::SizedTexture::new(shown.texture.id(), shown.size * scale)));
                });
            }
        } else {
            let (icon, tint) = icons::for_kind(asset.kind.as_deref());
            ui.label(RichText::new(icon).size(28.0).color(tint));
        }
        ui.vertical(|ui| {
            let name = asset.path.rsplit('/').next().unwrap_or(&asset.path);
            ui.label(RichText::new(name).heading().strong());
            ui.label(RichText::new(&asset.path).small().color(theme::weak()));
            ui.horizontal(|ui| {
                badge(ui, asset.kind.as_deref().unwrap_or("no kind"));
                if let Some(role) = role(asset) {
                    badge(ui, role);
                }
                if let Some(uid) = &asset.uid {
                    ui.label(RichText::new(uid).monospace().small().color(theme::weak()));
                    let copy = ui.add(egui::Button::new(RichText::new("Copy").small()).frame(false));
                    if copy.on_hover_text("Copy the uid").clicked() {
                        ui.ctx().copy_text(uid.clone());
                    }
                }
            });
        });
    });
}

pub fn badge(ui: &mut egui::Ui, text: &str) {
    egui::Frame::new()
        .fill(theme::accent().gamma_multiply(0.25))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(7, 1))
        .show(ui, |ui| ui.label(RichText::new(text).small().color(theme::link())));
}

// --- properties -------------------------------------------------------------------------------------------------------

/// A field's label: the name a reader would use.
fn label_of(key: &str) -> String {
    let known = match key {
        "bpp" => "Depth",
        "at" => "Offset",
        "rect" => "Rectangle",
        "sha1" => "SHA-1",
        "id" => "Id",
        "executable_palette" => "Palette from",
        "no_palette" => "No palette",
        "sector_len" => "Sector length",
        "native" => "Native file",
        _ => "",
    };
    if !known.is_empty() {
        return known.to_string();
    }
    let words = key.replace('_', " ");
    let mut chars = words.chars();
    chars.next().map_or_default(|c| c.to_uppercase().chain(chars).collect())
}

fn property_sheet(
    ui: &mut egui::Ui,
    ws: &mut Workspace,
    fields: &[(&String, &toml::Value)],
    context: &Context,
    nav: &mut Option<String>,
) {
    egui::Grid::new(ui.next_auto_id()).num_columns(2).spacing([18.0, 6.0]).min_row_height(ROW_HEIGHT).show(ui, |ui| {
        for (key, value) in fields {
            ui.label(RichText::new(label_of(key)).color(theme::weak()));
            ui.horizontal_wrapped(|ui| value_ui(ui, ws, key, value, context, nav));
            ui.end_row();
        }
    });
}

fn int(value: &toml::Value) -> Option<i64> {
    value.as_integer()
}

fn ints(value: &toml::Value) -> Option<Vec<i64>> {
    value.as_array()?.iter().map(int).collect()
}

/// A value by its field's meaning: a depth, a size, an offset, a rectangle, a placement, a reference, a list.
fn value_ui(
    ui: &mut egui::Ui,
    ws: &mut Workspace,
    key: &str,
    value: &toml::Value,
    context: &Context,
    nav: &mut Option<String>,
) {
    if let Some(n) = int(value) {
        let text = match key {
            "bpp" if n == 16 => "16-bit direct color".to_string(),
            "bpp" => format!("{n}-bit indexed ({} colors)", 1 << n),
            "sector_len" | "header_bits" | "sound_bank_mask" => format!("0x{n:X}"),
            _ => n.to_string(),
        };
        ui.label(text);
        return;
    }
    if let Some(v) = ints(value) {
        let text = match (key, v.as_slice()) {
            ("size", [w, h]) => format!("{w} × {h}"),
            ("rect", [x, y, w, h]) => format!("{w} × {h}  at ({x}, {y})"),
            (_, [x, y]) => format!("({x}, {y})"),
            _ => v.iter().map(i64::to_string).collect::<Vec<_>>().join(", "),
        };
        ui.label(RichText::new(text).monospace());
        return;
    }
    match value {
        toml::Value::Boolean(b) => {
            ui.label(if *b { "Yes" } else { "No" });
        }
        toml::Value::String(s) if key == "sha1" || s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit()) => {
            hash_ui(ui, s);
        }
        toml::Value::String(s) => {
            ui.label(s);
        }
        toml::Value::Array(items) => {
            for item in items {
                value_ui(ui, ws, key, item, context, nav);
            }
        }
        toml::Value::Table(t) => {
            if is_reference(value) {
                reference_chip(ui, ws, value, nav);
            } else if let (Some(slot), Some(at)) = (t.get("slot"), t.get("at")) {
                // A placement: the VRAM slot and the offset in it.
                reference_chip(ui, ws, slot, nav);
                if let Some([x, y]) = ints(at).as_deref() {
                    ui.label(RichText::new(format!("+ ({x}, {y})")).monospace().color(theme::weak()));
                }
                if let Some([w, h]) = t.get("size").and_then(ints).as_deref() {
                    ui.label(RichText::new(format!("{w} × {h}")).monospace().color(theme::weak()));
                }
            } else {
                ui.vertical(|ui| {
                    let fields: Vec<(&String, &toml::Value)> = t.iter().collect();
                    property_sheet(ui, ws, &fields, context, nav);
                });
            }
        }
        other => {
            ui.label(other.to_string());
        }
    }
}

fn hash_ui(ui: &mut egui::Ui, hash: &str) {
    let short = format!("{}…{}", &hash[..8], &hash[hash.len() - 4..]);
    let r = ui.add(egui::Label::new(RichText::new(short).monospace().color(theme::weak())).sense(egui::Sense::click()));
    if r.on_hover_text(format!("{hash}\nClick to copy")).clicked() {
        ui.ctx().copy_text(hash.to_string());
    }
}

fn is_reference(value: &toml::Value) -> bool {
    value.get("uid").is_some_and(toml::Value::is_str)
        && value.get("path").and_then(toml::Value::as_str).is_some_and(|p| p.starts_with("res://"))
}

/// A reference as a chip: its asset's icon and short name, opening it on a click; red where it names nothing.
fn reference_chip(ui: &mut egui::Ui, ws: &Workspace, value: &toml::Value, nav: &mut Option<String>) {
    let path = value.get("path").and_then(toml::Value::as_str).unwrap_or_default();
    let target = path.trim_start_matches("res://").trim_end_matches(project::index::BUNDLE_SUFFIX);
    match ws.target(value) {
        Some(asset) => {
            let (icon, tint) = icons::for_kind(asset.kind.as_deref());
            let mut parts = asset.path.rsplit('/');
            let name = parts.next().unwrap_or_default();
            let short = match parts.next() {
                Some(parent) => format!("{parent}/{name}"),
                None => name.to_string(),
            };
            if chip(ui, icon, &short, tint).on_hover_text(&asset.path).clicked() {
                *nav = Some(asset.path.clone());
            }
        }
        None => {
            chip(ui, icons::FILE_DASHED, target, ui.visuals().error_fg_color).on_hover_text("No such asset");
        }
    }
}

/// A reference or a cell as a chip: one widget, so that a row of them wraps where the pane is narrow.
fn chip(ui: &mut egui::Ui, icon: &str, text: &str, tint: Color32) -> egui::Response {
    let mut job = egui::text::LayoutJob::default();
    let font = egui::TextStyle::Body.resolve(ui.style());
    job.append(icon, 0.0, egui::text::TextFormat::simple(font.clone(), tint));
    job.append(text, 4.0, egui::text::TextFormat::simple(font, theme::link()));
    let chip = egui::Button::new(job)
        .fill(tint.gamma_multiply(0.14))
        .stroke(egui::Stroke::new(1.0, tint.gamma_multiply(0.35)))
        .corner_radius(egui::CornerRadius::same(5))
        .min_size(egui::Vec2::ZERO)
        // Its own width, whole: the row moves it to the next line rather than fold its text.
        .wrap_mode(egui::TextWrapMode::Extend);
    // Its padding set on the row itself and put back: a child Ui (a scope, a frame) is placed where the row is, and is
    // never moved to the next line.
    let padding = std::mem::replace(&mut ui.spacing_mut().button_padding, egui::vec2(6.0, 1.0));
    let response = ui.add(chip);
    ui.spacing_mut().button_padding = padding;
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

// --- collections ------------------------------------------------------------------------------------------------------

fn is_table_list(value: &toml::Value) -> bool {
    value.as_array().is_some_and(|a| !a.is_empty() && a.iter().all(toml::Value::is_table))
}

/// An item's title: its `name`, its `index`, or its place in the list.
fn title_of(item: &toml::Value, i: usize) -> String {
    if let Some(name) = item.get("name").and_then(toml::Value::as_str) {
        return name.to_string();
    }
    match item.get("index").and_then(toml::Value::as_integer) {
        Some(n) => format!("#{n}"),
        None => format!("#{i}"),
    }
}

/// A list of named items: cells as a grid of chips; variants with their palettes; anything holding lists of its own
/// (a file's slots with their pages and palettes, an atlas's parts) as cards that open; the rest as rows.
fn collection_ui(
    ui: &mut egui::Ui,
    ws: &mut Workspace,
    asset: &Asset,
    key: &str,
    items: &[toml::Value],
    context: &Context,
    nav: &mut Option<String>,
) {
    if key == "cells" {
        cells_ui(ui, items);
        return;
    }
    if key == "variants" && context.piece.is_some() {
        variants_ui(ui, ws, asset, items);
        return;
    }
    for (i, item) in items.iter().enumerate() {
        let title = title_of(item, i);
        let nested = item.as_table().is_some_and(|t| t.values().any(is_table_list));
        let item_context =
            if key == "parts" { Context { piece: Some(format!("{}#{title}", asset.path)) } } else { context.clone() };
        if nested {
            let id = ui.make_persistent_id((key, i));
            egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false)
                .show_header(ui, |ui| row_ui(ui, ws, key, i, &title, item, &item_context, nav))
                .body(|ui| {
                    let table = item.as_table().cloned().unwrap_or_default();
                    for (k, v) in table.iter().filter(|(_, v)| is_table_list(v)) {
                        let list = v.as_array().map_or(&[][..], Vec::as_slice);
                        ui.label(
                            RichText::new(format!("{}  ·  {}", label_of(k), list.len())).small().color(theme::weak()),
                        );
                        ui.indent((key, i, k.as_str()), |ui| collection_ui(ui, ws, asset, k, list, &item_context, nav));
                    }
                });
        } else {
            ui.horizontal(|ui| {
                ui.add_space(18.0);
                row_ui(ui, ws, key, i, &title, item, &item_context, nav);
            });
        }
    }
}

/// One item's summary line: its title, then its plain fields as small label-value pairs, references as chips; a
/// variant's or a palette entry's colors as a swatch.
#[expect(clippy::too_many_arguments, reason = "one row's context")]
fn row_ui(
    ui: &mut egui::Ui,
    ws: &mut Workspace,
    key: &str,
    index: usize,
    title: &str,
    item: &toml::Value,
    context: &Context,
    nav: &mut Option<String>,
) {
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(title).strong());
        if let Some(ty) = item.get("type").and_then(toml::Value::as_str) {
            badge(ui, ty);
        }
        if key == "palettes" {
            // An image's own palette by its place in the list; a file's palette entry by the image it names.
            if item.get("image").is_some() {
                if let Some((image, palette)) = ws.entry_palette(item) {
                    swatch_ui(ui, ws, &image, palette);
                }
            } else if let Some(piece) = &context.piece {
                swatch_ui(ui, ws, piece, index);
            }
        }
        let Some(table) = item.as_table() else { return };
        for (k, v) in table {
            if matches!(k.as_str(), "name" | "index" | "type") || is_table_list(v) {
                continue;
            }
            if is_reference(v) {
                reference_chip(ui, ws, v, nav);
                continue;
            }
            match (k.as_str(), v) {
                (_, toml::Value::Boolean(false)) => {}
                (_, toml::Value::Boolean(true)) => badge(ui, &label_of(k).to_lowercase()),
                ("bpp", v) => {
                    ui.label(RichText::new(format!("{}-bit", v.as_integer().unwrap_or_default())).color(theme::weak()));
                }
                _ => {
                    ui.label(RichText::new(label_of(k).to_lowercase()).small().color(theme::weak()));
                    value_ui(ui, ws, k, v, context, nav);
                }
            }
        }
        for (k, v) in table.iter().filter(|(_, v)| is_table_list(v)) {
            let n = v.as_array().map_or(0, Vec::len);
            ui.label(RichText::new(count(n, &label_of(k).to_lowercase())).small().color(theme::weak()));
        }
    });
}

/// `n` of a plural noun: "1 variant", "8 cells".
fn count(n: usize, plural: &str) -> String {
    if n == 1 { format!("1 {}", plural.strip_suffix('s').unwrap_or(plural)) } else { format!("{n} {plural}") }
}

/// A palette's colors as a strip.
fn swatch_ui(ui: &mut egui::Ui, ws: &mut Workspace, piece: &str, variant: usize) {
    let ctx = ui.ctx().clone();
    if let Some(Ok(strip)) = ws.palette(&ctx, piece, variant) {
        let width = if strip.size.x <= 16.0 { 96.0 } else { 160.0 };
        egui::Frame::new().stroke(egui::Stroke::new(1.0, theme::border())).show(ui, |ui| {
            ui.add(egui::Image::new(egui::load::SizedTexture::new(strip.texture.id(), egui::vec2(width, 10.0))));
        });
    }
}

/// An image's variants as a tree: a texture's each one palette; an atlas's each the cells it recolors, every one with
/// the palette it takes - a swatch beside each palette.
fn variants_ui(ui: &mut egui::Ui, ws: &mut Workspace, asset: &Asset, items: &[toml::Value]) {
    let palettes = ws.palettes_of(&asset.path);
    let index = |name: &str| palettes.iter().position(|p| p == name);
    for (i, item) in items.iter().enumerate() {
        let title = title_of(item, i);
        if let Some(palette) = item.get("palette").and_then(toml::Value::as_str) {
            ui.horizontal(|ui| {
                ui.add_space(18.0);
                ui.label(RichText::new(&title).strong());
                if let Some(p) = index(palette) {
                    swatch_ui(ui, ws, &asset.path, p);
                }
                ui.label(RichText::new(palette).small().color(theme::weak()));
            });
            continue;
        }
        let cells = item.get("palettes").and_then(toml::Value::as_table).cloned().unwrap_or_default();
        let id = ui.make_persistent_id(("variant", i));
        egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false)
            .show_header(ui, |ui| {
                ui.label(RichText::new(&title).strong());
                ui.label(RichText::new(count(cells.len(), "cells")).small().color(theme::weak()));
            })
            .body(|ui| {
                for (cell, palette) in &cells {
                    let palette = palette.as_str().unwrap_or_default();
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(icons::SQUARES_FOUR).color(theme::weak()));
                        ui.label(cell);
                        ui.label(RichText::new("→").color(theme::weak()));
                        if let Some(p) = index(palette) {
                            swatch_ui(ui, ws, &asset.path, p);
                        }
                        ui.label(RichText::new(palette).small().color(theme::weak()));
                    });
                }
            });
    }
}

fn cells_ui(ui: &mut egui::Ui, items: &[toml::Value]) {
    ui.horizontal_wrapped(|ui| {
        for (i, item) in items.iter().enumerate() {
            let rect = item.get("rect").and_then(ints).unwrap_or_default();
            let palette =
                item.get("palette").and_then(toml::Value::as_str).map_or(String::new(), |p| format!(", palette {p}"));
            let hint = match rect.as_slice() {
                [x, y, w, h] => format!("{w} × {h} at ({x}, {y}){palette}"),
                _ => String::new(),
            };
            chip(ui, icons::SQUARES_FOUR, &title_of(item, i), theme::weak()).on_hover_text(hint);
        }
    });
}

// --- files ------------------------------------------------------------------------------------------------------------

/// A plain file or a folder of the resolved project: where it comes from and where it is on disk.
fn entry_ui(ui: &mut egui::Ui, path: &str, entry: &project::index::Entry, tools: &Tools, message: &mut Option<String>) {
    let name = path.rsplit('/').next().unwrap_or(path);
    let (icon, kind) = match entry.kind {
        project::index::Kind::File => (icons::FILE, "file"),
        _ => (icons::FOLDER, "folder"),
    };
    ui.label(RichText::new(format!("{icon}  {name}")).heading());
    ui.label(RichText::new(path).small().color(theme::weak()));
    theme::section(ui, "Entry", |ui| {
        egui::Grid::new("entry").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
            let mut row = |label: &str, value: String| {
                ui.label(RichText::new(label).color(theme::weak()));
                ui.label(value);
                ui.end_row();
            };
            row("Kind", kind.to_string());
            row("Uid", entry.uid.clone());
            row("Made by", entry.home.clone());
            if !entry.overrides.is_empty() {
                row("Overridden in", entry.overrides.join(", "));
            }
            if let Some(layer) = &entry.moved {
                row("Moved by", layer.clone());
            }
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new("On disk").color(theme::weak()));
            theme::short(ui, entry.disk.display().to_string());
        });
        if entry.kind == project::index::Kind::File && ui.button(format!("{}  Open", icons::ARROW_SQUARE_OUT)).clicked()
        {
            *message = Some(external::open(&entry.disk, path, tools).unwrap_or_else(|e| e));
        }
    });
}

fn files_ui(ui: &mut egui::Ui, asset: &Asset, tools: &Tools, message: &mut Option<String>) {
    if asset.files.is_empty() {
        return;
    }
    theme::section(ui, "Bundle", |ui| {
        egui::Grid::new("bundle files").num_columns(4).spacing([14.0, 4.0]).min_row_height(ROW_HEIGHT).show(ui, |ui| {
            for f in &asset.files {
                let tool = Category::of(&f.path);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icons::FILE).color(theme::weak()));
                    ui.label(&f.name);
                });
                ui.label(RichText::new(size_text(f.size)).small().color(theme::weak()));
                ui.label(RichText::new(&f.layer).small().color(theme::weak()));
                let hint = match tool.command(tools).trim() {
                    "" => format!("Open in the system's default application ({})", tool.label().to_lowercase()),
                    command => format!("Open with {command}"),
                };
                let open = ui.add(egui::Button::new(RichText::new(icons::ARROW_SQUARE_OUT)).frame(false));
                if open.on_hover_text(hint).clicked() {
                    *message = Some(external::open(&f.path, &asset.path, tools).unwrap_or_else(|e| e));
                }
                ui.end_row();
            }
        });
        if let Some(original) = asset.fields().get("original").and_then(toml::Value::as_table) {
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Original").small().color(theme::weak()));
                for (release, v) in original {
                    ui.label(RichText::new(release).small().strong());
                    if let Some(sha1) = v.get("sha1").and_then(toml::Value::as_str) {
                        hash_ui(ui, sha1);
                    }
                }
            });
        }
    });
}

#[expect(clippy::cast_precision_loss, reason = "a size shown to one decimal")]
pub fn size_text(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / f64::from(1 << 20)),
        b if b >= 1 << 10 => format!("{:.1} KB", b as f64 / f64::from(1 << 10)),
        b => format!("{b} bytes"),
    }
}
