//! The inspector: the selected asset's declaration shown as properties, not as TOML. A header names the asset (its
//! icon, kind and role, a thumbnail for a picture); a property sheet gives each field a readable label and its value
//! the form its meaning calls for - a depth, a size, a VRAM offset or rectangle, a placement, a reference as a chip
//! that opens its asset; lists (slots, variants, parts, cells) are rows or cards with a summary, a variant with its
//! palette's colors; the bundle's files and the originals' sha1s close it.

use eframe::egui::{self, Color32, RichText};
use project::ext::Extensions;
use project::index::{Asset, Index};

use crate::external::{self, Category};
use crate::settings::{Tools, Upscale};
use crate::workspace::{EditRequest, FieldEdit, Workspace};
use crate::{icons, theme};

/// Fields the header shows, or that only extraction writes.
const HEADER_FIELDS: [&str; 4] = ["uid", "kind", "role", "original"];
const ROW_HEIGHT: f32 = 22.0;
const THUMBNAIL: f32 = 64.0;

/// What a value is read with: the index its references resolve in, the registry their kinds' icons come from, and the
/// fields the asset's kind and role describe, as their extensions register them.
pub struct Sheet<'a> {
    pub index: &'a Index,
    pub extensions: &'a Extensions,
    pub fields: Vec<pe2_ext::Field>,
    /// The table of the declaration the fields are in: none for the asset's own, `stack.1` for a layer of its stack
    /// (a list's tables by their places).
    pub within: Option<String>,
    /// The fields the asset's import derives from its source, by importer: shown, not edited here.
    pub derived: Option<(String, Vec<String>)>,
}

impl Sheet<'_> {
    /// A field's place in the declaration: `mode`, or `stack.1.mode`.
    fn path(&self, key: &str) -> String {
        self.within.as_ref().map_or_else(|| key.to_string(), |within| format!("{within}.{key}"))
    }

    fn field(&self, key: &str) -> Option<&pe2_ext::Field> {
        self.fields.iter().find(|f| f.key == key)
    }

    /// A field's label: its extension's, or the name a reader would use.
    fn label(&self, key: &str) -> String {
        self.field(key).and_then(|f| f.label.clone()).unwrap_or_else(|| label_of(key))
    }

    /// Drawn by its kind's inspector view, not here.
    fn custom(&self, key: &str) -> bool {
        self.field(key).is_some_and(|f| f.custom)
    }
}

pub fn inspector_ui(
    ui: &mut egui::Ui,
    ws: &mut Workspace,
    subject: Option<&str>,
    nav: &mut Option<String>,
    tools: &Tools,
    message: &mut Option<String>,
    edit: &mut Option<EditRequest>,
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
    unapplied_ui(ui, ws, &asset, nav, edit);
    let target = edit_target(ws, &asset);
    {
        let sheet = Sheet {
            index: &ws.index,
            extensions: &ws.extensions,
            fields: ws.extensions.fields(&asset).into_iter().cloned().collect(),
            within: None,
            derived: ws.extensions.import_op(&asset).filter(|op| !op.derives.is_empty()).map(|op| {
                (op.label.clone(), op.derives.iter().filter(|p| !p.contains('.')).cloned().collect())
            }),
        };
        // The settings as resolved: what the import derives in its place.
        let fields: Vec<(&String, &toml::Value)> = asset
            .settings()
            .iter()
            .filter(|(k, _)| !HEADER_FIELDS.contains(&k.as_str()) && *k != "stack" && !sheet.custom(k))
            .collect();
        let (lists, properties): (Vec<_>, Vec<_>) = fields.into_iter().partition(|(_, v)| is_table_list(v));
        let (field_edit, parts) = (&mut ws.field_edit, &mut ws.parts);
        if !properties.is_empty() {
            theme::section(ui, "Properties", |ui| {
                property_sheet(ui, &sheet, field_edit, &target, &asset, &properties, nav, edit);
            });
        }
        for (key, value) in lists {
            let items = value.as_array().map_or(&[][..], Vec::as_slice);
            theme::section(ui, &format!("{}  ·  {}", sheet.label(key), items.len()), |ui| {
                collection_ui(ui, &sheet, parts, key, items, nav);
            });
        }
    }
    ws.show_inspector_views(ui, &asset.path, nav);
    stack_ui(ui, ws, &asset, &target, nav, edit);
    files_ui(ui, &asset, tools, message);
    declaration_ui(ui, ws, &asset, edit);
}

/// Where an edit of an asset goes, as said beside its controls: the active layer - copied into it first where the asset
/// comes from below - or why it cannot be edited.
fn edit_target(ws: &Workspace, asset: &Asset) -> Result<String, String> {
    let layer = ws.editable_layer()?;
    let theirs = !asset.layers.contains(&layer.name);
    Ok(if theirs { format!("Saving overrides it in {}", layer.name) } else { format!("Saved in {}", layer.name) })
}

/// The asset's declaration (`asset.toml`) as text, to edit and save whole: refused while it is not TOML.
fn declaration_ui(ui: &mut egui::Ui, ws: &mut Workspace, asset: &Asset, edit: &mut Option<EditRequest>) {
    let Some(uid) = asset.uid.clone() else { return };
    if ws.declaration_draft.as_ref().is_some_and(|(a, _)| *a != asset.path) {
        ws.declaration_draft = None;
    }
    let target = edit_target(ws, asset);
    let id = ui.make_persistent_id(("declaration", &asset.path));
    egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false)
        .show_header(ui, |ui| ui.label(RichText::new("DECLARATION").small().strong().color(theme::weak())))
        .body(|ui| {
            let (_, draft) = ws.declaration_draft.get_or_insert_with(|| (asset.path.clone(), asset.text().to_string()));
            let editable = target.is_ok();
            ui.add_enabled(
                editable,
                egui::TextEdit::multiline(draft).code_editor().desired_rows(12).desired_width(f32::INFINITY),
            );
            let changed = draft.as_str() != asset.text();
            let parsed = toml::from_str::<toml::Table>(draft);
            ui.horizontal_wrapped(|ui| {
                let save = ui.add_enabled(editable && changed && parsed.is_ok(), egui::Button::new("Save"));
                if save.clicked() {
                    let name = asset.path.rsplit('/').next().unwrap_or(&asset.path);
                    *edit = Some(EditRequest::Write {
                        uid: uid.clone(),
                        file: project::index::ASSET_FILE.to_string(),
                        bytes: draft.clone().into_bytes(),
                        label: format!("Edit {name}'s declaration"),
                        merge: None,
                    });
                }
                if ui.add_enabled(changed, egui::Button::new("Revert")).clicked() {
                    *draft = asset.text().to_string();
                }
                match (&target, &parsed) {
                    (_, Err(e)) => {
                        let first = e.to_string().lines().next().unwrap_or_default().to_string();
                        ui.label(RichText::new(first).small().color(ui.visuals().error_fg_color));
                    }
                    (Ok(note) | Err(note), Ok(_)) => {
                        ui.label(RichText::new(note).small().color(theme::weak()));
                    }
                }
            });
        });
}

// --- header -----------------------------------------------------------------------------------------------------------

fn header_ui(ui: &mut egui::Ui, ws: &mut Workspace, asset: &Asset) {
    ui.horizontal(|ui| {
        // Its picture, drawn off the window's thread; its kind's icon until it is, or where it has none.
        let ctx = ui.ctx().clone();
        let shown = ws.has_picture(&asset.path).then(|| ws.picture(&ctx, &asset.path, Upscale::Nearest)).flatten();
        if let Some(Ok(shown)) = shown {
            let scale = (THUMBNAIL / shown.size.y).min(THUMBNAIL * 2.0 / shown.size.x);
            egui::Frame::new().stroke(egui::Stroke::new(1.0, theme::border())).show(ui, |ui| {
                ui.add(egui::Image::new(egui::load::SizedTexture::new(shown.texture.id(), shown.size * scale)));
            });
        } else {
            let (icon, tint) = icons::for_kind(&ws.extensions, asset.kind.as_deref());
            ui.label(RichText::new(icon).size(28.0).color(tint));
        }
        ui.vertical(|ui| {
            let name = asset.path.rsplit('/').next().unwrap_or(&asset.path);
            ui.label(RichText::new(name).heading().strong());
            ui.label(RichText::new(&asset.path).small().color(theme::weak()));
            ui.horizontal(|ui| {
                badge(ui, asset.kind.as_deref().unwrap_or("no kind"));
                if let Some(role) = &asset.role {
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
        "at" => "Offset",
        "rect" => "Rectangle",
        "sha1" => "SHA-1",
        "id" => "Id",
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

#[expect(clippy::too_many_arguments, reason = "a sheet's context")]
fn property_sheet(
    ui: &mut egui::Ui,
    sheet: &Sheet,
    field_edit: &mut Option<FieldEdit>,
    target: &Result<String, String>,
    asset: &Asset,
    fields: &[(&String, &toml::Value)],
    nav: &mut Option<String>,
    edit: &mut Option<EditRequest>,
) {
    let editable = target.is_ok();
    egui::Grid::new(ui.next_auto_id()).num_columns(2).spacing([18.0, 6.0]).min_row_height(ROW_HEIGHT).show(ui, |ui| {
        for (key, value) in fields {
            ui.label(RichText::new(sheet.label(key)).color(theme::weak()));
            ui.horizontal_wrapped(|ui| {
                let editing = field_edit.as_ref().is_some_and(|f| f.asset == asset.path && f.key == sheet.path(key));
                if editing {
                    field_editor(ui, field_edit, asset, key, value, sheet, edit);
                    return;
                }
                let field = sheet.field(key);
                if let Some((importer, _)) = sheet.derived.as_ref().filter(|(_, keys)| keys.iter().any(|k| k == *key)) {
                    value_ui(ui, sheet, key, value, nav);
                    let overridden = asset.imported.as_ref().is_some_and(|i| i.overridden.iter().any(|o| o == *key));
                    let note = if overridden { "from the source; the declaration's is overridden" } else { "from the source" };
                    ui.label(RichText::new(note).small().color(theme::weak())).on_hover_text(format!(
                        "Derived by {importer} from the source: change it there, and it is imported again"
                    ));
                    return;
                }
                let editable = editable && !field.is_some_and(|f| f.read_only);
                if let Some(field) = field.filter(|f| editable && !f.choices.is_empty()) {
                    choice_ui(ui, sheet, asset, key, value, field, edit);
                    return;
                }
                value_ui(ui, sheet, key, value, nav);
                if editable && is_scalar(value) {
                    let pencil = ui.add(egui::Button::new(RichText::new(icons::PENCIL_SIMPLE).small()).frame(false));
                    if pencil.on_hover_text(target.clone().unwrap_or_default()).clicked() {
                        *field_edit =
                            Some(FieldEdit { asset: asset.path.clone(), key: sheet.path(key), text: raw_text(value) });
                    }
                }
            });
            ui.end_row();
        }
    });
}

/// A table inside a field, shown as its own sheet: edited in the declaration's text, not in place.
fn nested_sheet(ui: &mut egui::Ui, sheet: &Sheet, fields: &[(&String, &toml::Value)], nav: &mut Option<String>) {
    egui::Grid::new(ui.next_auto_id()).num_columns(2).spacing([18.0, 6.0]).min_row_height(ROW_HEIGHT).show(ui, |ui| {
        for (key, value) in fields {
            ui.label(RichText::new(sheet.label(key)).color(theme::weak()));
            ui.horizontal_wrapped(|ui| value_ui(ui, sheet, key, value, nav));
            ui.end_row();
        }
    });
}

/// A value the inspector edits in place: a string, a number, a truth.
fn is_scalar(value: &toml::Value) -> bool {
    matches!(value, toml::Value::String(_) | toml::Value::Integer(_) | toml::Value::Float(_) | toml::Value::Boolean(_))
}

/// A scalar as it is typed: a string unquoted, a number as written.
fn raw_text(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// The value of a field being edited: committed (a step of the history, the same field's next edits joining it) on
/// Enter or when it loses the keyboard; Escape leaves it.
fn field_editor(
    ui: &mut egui::Ui,
    field_edit: &mut Option<FieldEdit>,
    asset: &Asset,
    key: &str,
    value: &toml::Value,
    sheet: &Sheet,
    edit: &mut Option<EditRequest>,
) {
    let Some(field) = field_edit.as_mut() else { return };
    let parsed: Result<toml_edit::Value, String> = match value {
        toml::Value::String(_) => Ok(field.text.as_str().into()),
        toml::Value::Integer(_) => {
            let text = field.text.trim();
            let n = text.strip_prefix("0x").map_or_else(|| text.parse::<i64>(), |hex| i64::from_str_radix(hex, 16));
            n.map(Into::into).map_err(|_| "a whole number".to_string())
        }
        toml::Value::Float(_) => field.text.trim().parse::<f64>().map(Into::into).map_err(|_| "a number".to_string()),
        toml::Value::Boolean(_) => match field.text.trim() {
            "true" => Ok(true.into()),
            "false" => Ok(false.into()),
            _ => Err("true or false".to_string()),
        },
        _ => Err("not edited here".to_string()),
    };
    let response = ui.add(egui::TextEdit::singleline(&mut field.text).desired_width(180.0));
    response.request_focus();
    let (enter, escape) = ui.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
    if let Err(why) = &parsed {
        ui.label(RichText::new(why).small().color(ui.visuals().error_fg_color));
    }
    if escape {
        *field_edit = None;
    } else if (enter || response.lost_focus())
        && let Ok(new) = parsed
    {
        *field_edit = None;
        if let Some(request) = set_field(asset, sheet.within.as_deref(), key, new, &sheet.label(key)) {
            *edit = Some(request);
        }
    }
}

/// The edit setting a field of an asset's declaration - its own, or one of a table of it (`import`) - its layout kept;
/// none where it changes nothing. Edits of the same field in a row merge into one step.
fn set_field(
    asset: &Asset,
    within: Option<&str>,
    key: &str,
    new: toml_edit::Value,
    label: &str,
) -> Option<EditRequest> {
    let mut document = asset.text().parse::<toml_edit::DocumentMut>().ok()?;
    match within {
        Some(path) => table_at(&mut document, path)?.insert(key, toml_edit::Item::Value(new)),
        None => document.insert(key, toml_edit::Item::Value(new)),
    };
    if document.to_string() == asset.text() {
        return None;
    }
    let name = asset.path.rsplit('/').next().unwrap_or(&asset.path);
    Some(EditRequest::Write {
        uid: asset.uid.clone().unwrap_or_default(),
        file: project::index::ASSET_FILE.to_string(),
        bytes: document.to_string().into_bytes(),
        label: format!("Set {} of {name}", label.to_lowercase()),
        merge: Some(format!("{}#{}{key}", asset.path, within.map_or_else(String::new, |w| format!("{w}.")))),
    })
}

/// The table at a path of a declaration: keys, and a list's tables by their places (`stack.1`).
fn table_at<'a>(document: &'a mut toml_edit::DocumentMut, path: &str) -> Option<&'a mut dyn toml_edit::TableLike> {
    let mut steps = path.split('.').peekable();
    let mut table: &mut dyn toml_edit::TableLike = document.as_table_mut();
    while let Some(step) = steps.next() {
        let item = table.get_mut(step)?;
        table = match steps.next_if(|s| s.parse::<usize>().is_ok()) {
            Some(i) => item.as_array_of_tables_mut()?.get_mut(i.parse().ok()?)?,
            None => item.as_table_like_mut()?,
        };
    }
    Some(table)
}

/// A value as `toml_edit` writes it: inline, tables as inline tables.
fn inline(value: &toml::Value) -> toml_edit::Value {
    match value {
        toml::Value::String(s) => s.as_str().into(),
        toml::Value::Integer(n) => (*n).into(),
        toml::Value::Float(f) => (*f).into(),
        toml::Value::Boolean(b) => (*b).into(),
        toml::Value::Datetime(d) => d.to_string().parse().unwrap_or_else(|_| d.to_string().into()),
        toml::Value::Array(items) => toml_edit::Value::Array(items.iter().map(inline).collect()),
        toml::Value::Table(table) => {
            toml_edit::Value::InlineTable(table.iter().map(|(k, v)| (k.as_str(), inline(v))).collect())
        }
    }
}

/// A field as written into a declaration: a table of parts (each entry a table) a line each, `cells.disc_1 = { ... }`,
/// in its order; anything else inline.
fn item(value: &toml::Value) -> toml_edit::Item {
    match value {
        toml::Value::Table(parts) if !parts.is_empty() && parts.values().all(toml::Value::is_table) => {
            let mut table = toml_edit::Table::new();
            table.set_dotted(true);
            for (name, part) in parts {
                table.insert(name, toml_edit::Item::Value(inline(part)));
            }
            toml_edit::Item::Table(table)
        }
        other => toml_edit::Item::Value(inline(other)),
    }
}

/// The edit making what a view set of the asset it shows: each field's new value or its removal, its declaration's
/// layout otherwise kept, in one step; none where it changes nothing.
pub fn set_fields(asset: &Asset, sets: &[(String, Option<toml::Value>)]) -> Option<EditRequest> {
    let mut document = asset.text().parse::<toml_edit::DocumentMut>().ok()?;
    for (key, value) in sets {
        match value {
            Some(value) => {
                document.insert(key, item(value));
            }
            None => {
                document.remove(key);
            }
        }
    }
    if document.to_string() == asset.text() {
        return None;
    }
    let mut keys: Vec<&str> = sets.iter().map(|(k, _)| k.as_str()).collect();
    keys.dedup();
    let name = asset.path.rsplit('/').next().unwrap_or(&asset.path);
    Some(EditRequest::Write {
        uid: asset.uid.clone().unwrap_or_default(),
        file: project::index::ASSET_FILE.to_string(),
        bytes: document.to_string().into_bytes(),
        label: format!("Edit {} of {name}", keys.join(", ")),
        merge: Some(format!("{}#{}", asset.path, keys.join(","))),
    })
}

/// A field with choices, edited by choosing one: each by its label, the value set at once.
fn choice_ui(
    ui: &mut egui::Ui,
    sheet: &Sheet,
    asset: &Asset,
    key: &str,
    value: &toml::Value,
    field: &pe2_ext::Field,
    edit: &mut Option<EditRequest>,
) {
    let current = field.choice(value).map_or_else(|| raw_text(value), str::to_string);
    let mut chosen = None;
    egui::ComboBox::from_id_salt(("choice", &asset.path, sheet.path(key))).selected_text(current).show_ui(ui, |ui| {
        for (choice, label) in &field.choices {
            if ui.selectable_label(choice == value, label).clicked() && choice != value {
                chosen = Some(choice.clone());
            }
        }
    });
    let new = chosen.and_then(|choice| match choice {
        toml::Value::String(s) => Some(toml_edit::Value::from(s)),
        toml::Value::Integer(n) => Some(n.into()),
        toml::Value::Float(f) => Some(f.into()),
        toml::Value::Boolean(b) => Some(b.into()),
        _ => None,
    });
    let label = field.label.as_deref().unwrap_or(key);
    if let Some(request) = new.and_then(|new| set_field(asset, sheet.within.as_deref(), key, new, label)) {
        *edit = Some(request);
    }
}

fn int(value: &toml::Value) -> Option<i64> {
    value.as_integer()
}

fn ints(value: &toml::Value) -> Option<Vec<i64>> {
    value.as_array()?.iter().map(int).collect()
}

/// A value by its field's meaning: a depth, a size, an offset, a rectangle, a placement, a reference, a list.
pub fn value_ui(ui: &mut egui::Ui, sheet: &Sheet, key: &str, value: &toml::Value, nav: &mut Option<String>) {
    let field = sheet.field(key);
    let formatted = field.and_then(|f| f.format.as_ref()).and_then(|format| format(value));
    if let Some(text) = formatted.or_else(|| field.and_then(|f| f.choice(value)).map(str::to_string)) {
        ui.label(text);
        return;
    }
    if let Some(n) = int(value) {
        ui.label(n.to_string());
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
                value_ui(ui, sheet, key, item, nav);
            }
        }
        toml::Value::Table(t) => {
            if is_reference(value) {
                reference_chip(ui, sheet, value, nav);
            } else if let (Some(slot), Some(at)) = (t.get("slot"), t.get("at")) {
                // A placement: the VRAM slot and the offset in it.
                reference_chip(ui, sheet, slot, nav);
                if let Some([x, y]) = ints(at).as_deref() {
                    ui.label(RichText::new(format!("+ ({x}, {y})")).monospace().color(theme::weak()));
                }
                if let Some([w, h]) = t.get("size").and_then(ints).as_deref() {
                    ui.label(RichText::new(format!("{w} × {h}")).monospace().color(theme::weak()));
                }
            } else {
                ui.vertical(|ui| {
                    let fields: Vec<(&String, &toml::Value)> = t.iter().collect();
                    nested_sheet(ui, sheet, &fields, nav);
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
    project::index::reference(value).is_some()
}

/// A reference as a chip: its asset's icon and short name, opening it on a click; red where it names nothing.
pub fn reference_chip(ui: &mut egui::Ui, sheet: &Sheet, value: &toml::Value, nav: &mut Option<String>) {
    let (uid, path) = project::index::reference(value).unwrap_or_default();
    let target = path.clone().unwrap_or_else(|| uid.clone());
    match sheet.index.resolve(&uid, path.as_deref().unwrap_or_default()) {
        Some(asset) => {
            let (icon, tint) = icons::for_kind(sheet.extensions, asset.kind.as_deref());
            let mut parts = asset.path.rsplit('/');
            let name = parts.next().unwrap_or_default();
            let short = match parts.next() {
                Some(parent) => format!("{parent}/{name}"),
                None => name.to_string(),
            };
            // A part, by its fragment after the asset's name.
            let short = match project::index::part_of(value) {
                Some(part) => format!("{short}#{part}"),
                None => short,
            };
            if chip(ui, icon, &short, tint).on_hover_text(&asset.path).clicked() {
                *nav = Some(asset.path.clone());
            }
        }
        None => {
            chip(ui, icons::FILE_DASHED, &target, ui.visuals().error_fg_color).on_hover_text("No such asset");
        }
    }
}

/// A reference or a cell as a chip: one widget, so that a row of them wraps where the pane is narrow.
pub fn chip(ui: &mut egui::Ui, icon: &str, text: &str, tint: Color32) -> egui::Response {
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
    sheet: &Sheet,
    parts: &mut crate::workspace::Parts,
    key: &str,
    items: &[toml::Value],
    nav: &mut Option<String>,
) {
    for (i, item) in items.iter().enumerate() {
        let title = title_of(item, i);
        let nested = item.as_table().is_some_and(|t| t.values().any(is_table_list));
        if nested {
            let id = ui.make_persistent_id((key, i));
            egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false)
                .show_header(ui, |ui| row_ui(ui, sheet, parts, &title, item, nav))
                .body(|ui| {
                    let table = item.as_table().cloned().unwrap_or_default();
                    for (k, v) in table.iter().filter(|(_, v)| is_table_list(v)) {
                        let list = v.as_array().map_or(&[][..], Vec::as_slice);
                        ui.label(
                            RichText::new(format!("{}  ·  {}", sheet.label(k), list.len())).small().color(theme::weak()),
                        );
                        ui.indent((key, i, k.as_str()), |ui| collection_ui(ui, sheet, parts, k, list, nav));
                    }
                });
        } else {
            ui.horizontal(|ui| {
                ui.add_space(18.0);
                row_ui(ui, sheet, parts, &title, item, nav);
            });
        }
    }
}

/// A list's entry on one line: its title, its type, and its fields the row's way - a reference to a part with a
/// picture shown with it (a palette's swatch, a variant's look).
fn row_ui(
    ui: &mut egui::Ui,
    sheet: &Sheet,
    parts: &mut crate::workspace::Parts,
    title: &str,
    item: &toml::Value,
    nav: &mut Option<String>,
) {
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(title).strong());
        if let Some(ty) = item.get("type").and_then(toml::Value::as_str) {
            badge(ui, ty);
        }
        let Some(table) = item.as_table() else { return };
        for (k, v) in table {
            if !matches!(k.as_str(), "name" | "index" | "type") && !is_table_list(v) {
                part_picture(ui, sheet, parts, v);
                field_ui(ui, sheet, k, v, nav);
            }
        }
        for (k, v) in table.iter().filter(|(_, v)| is_table_list(v)) {
            let n = v.as_array().map_or(0, Vec::len);
            ui.label(RichText::new(count(n, &sheet.label(k).to_lowercase())).small().color(theme::weak()));
        }
    });
}

/// The picture of the part a reference names, where its asset's kind draws one: a strip (a palette) as a swatch,
/// anything else small.
fn part_picture(ui: &mut egui::Ui, sheet: &Sheet, parts: &mut crate::workspace::Parts, value: &toml::Value) {
    let (Some(part), Some((uid, path))) = (project::index::part_of(value), project::index::reference(value)) else {
        return;
    };
    let Some(asset) = sheet.index.resolve(&uid, path.as_deref().unwrap_or_default()) else { return };
    let ctx = ui.ctx().clone();
    let Some(Ok(picture)) = parts.get(&ctx, sheet.extensions, asset, part) else { return };
    let size = if picture.size.y <= 1.0 {
        egui::vec2(if picture.size.x <= 16.0 { 96.0 } else { 160.0 }, 10.0)
    } else {
        picture.size * (24.0 / picture.size.y.max(1.0))
    };
    egui::Frame::new().stroke(egui::Stroke::new(1.0, theme::border())).show(ui, |ui| {
        ui.add(egui::Image::new(egui::load::SizedTexture::new(picture.texture.id(), size)));
    });
}

/// A field within a row: a reference as a chip, a flag as a badge, anything else its label and its reading.
pub fn field_ui(ui: &mut egui::Ui, sheet: &Sheet, key: &str, value: &toml::Value, nav: &mut Option<String>) {
    if is_reference(value) {
        reference_chip(ui, sheet, value, nav);
        return;
    }
    if let Some(brief) = sheet.field(key).and_then(|f| f.brief.as_ref()).and_then(|brief| brief(value)) {
        ui.label(RichText::new(brief).color(theme::weak()));
        return;
    }
    match value {
        toml::Value::Boolean(false) => {}
        toml::Value::Boolean(true) => badge(ui, &sheet.label(key).to_lowercase()),
        _ => {
            ui.label(RichText::new(sheet.label(key).to_lowercase()).small().color(theme::weak()));
            value_ui(ui, sheet, key, value, nav);
        }
    }
}

/// `n` of a plural noun: "1 variant", "8 cells".
fn count(n: usize, plural: &str) -> String {
    if n == 1 { format!("1 {}", plural.strip_suffix('s').unwrap_or(plural)) } else { format!("{n} {plural}") }
}

// --- import ---

/// Where an asset's settings changed and are not applied: what applying makes again, and Apply, Revert, and Apply all
/// where others wait too.
fn unapplied_ui(ui: &mut egui::Ui, ws: &mut Workspace, asset: &Asset, nav: &mut Option<String>, edit: &mut Option<EditRequest>) {
    if !ws.unapplied(&asset.path) {
        return;
    }
    // What applying makes again: its outputs, as its settings now say them.
    let ops = ws.extensions.operations();
    let tail = ws.extensions.tail(asset);
    let (plan, _) = project::graph::Plan::of(asset.fields(), &ops, &tail);
    let outputs: Vec<&str> = plan.outputs.iter().map(|o| o.name.as_str()).collect();
    // And what reads them, made again with them.
    let dependents = ws.extensions.dependents(&ws.index, &std::iter::once(asset.path.clone()).collect());
    let others = ws.unapplied_count().saturating_sub(1);
    let (mut apply, mut apply_all) = (false, false);
    theme::section(ui, "Unapplied changes", |ui| {
        let says = if outputs.is_empty() {
            "Its settings changed; it makes nothing from them.".to_string()
        } else {
            format!("Its outputs are made from its settings as last applied. Applying makes again: {}.", outputs.join(", "))
        };
        ui.label(RichText::new(says).color(theme::weak()));
        if !dependents.is_empty() {
            let count = dependents.len();
            ui.label(RichText::new(format!("And what read{} them:", if count == 1 { "s" } else { "" })).color(theme::weak()));
            ui.horizontal_wrapped(|ui| {
                for path in &dependents {
                    if ui.link(path).on_hover_text("Show it").clicked() {
                        *nav = Some(path.clone());
                    }
                }
            });
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            apply = ui.button("Apply").on_hover_text("Make its outputs again from its settings as they are now").clicked();
            if ui.button("Revert").on_hover_text("Put its settings back as last applied (an undoable step)").clicked() {
                *edit = ws.revert_settings(&asset.path);
            }
            if others > 0 {
                let text = format!("Apply all ({})", others + 1);
                apply_all = ui.button(text).on_hover_text("Apply every asset's changed settings").clicked();
            }
        });
    });
    if apply_all {
        ws.apply_all_settings();
    } else if apply {
        ws.apply_settings(&asset.path);
    }
}

/// An asset's stack, top to bottom in the order it runs: each layer's operation and what sets it - a stored file with
/// the formats it is and the tool it is edited in, an importer with a choice of those fitting its source and its
/// settings as it describes them, an output with how it is kept - then the importer's views, and the other importers
/// that fit the source.
fn stack_ui(
    ui: &mut egui::Ui,
    ws: &mut Workspace,
    asset: &Asset,
    target: &Result<String, String>,
    nav: &mut Option<String>,
    edit: &mut Option<EditRequest>,
) {
    let Some(layers) = asset.fields().get("stack").and_then(toml::Value::as_array) else { return };
    let import = project::graph::import_of(asset.fields());
    let name = import.as_ref().map_or("", |i| i.op);
    let source = import.as_ref().map(|i| i.source);
    let importer = ws.extensions.op(name);
    let fits: Vec<(String, String)> = ws.extensions.ops_for(asset).into_iter().map(|op| (op.name.to_string(), op.label.clone())).collect();
    // What each stored file is recognized as, by its layer.
    let recognized: Vec<Vec<String>> = layers
        .iter()
        .map(|layer| {
            let name = layer.get("file").and_then(toml::Value::as_str).filter(|_| layer.get("op").and_then(toml::Value::as_str) == Some(project::graph::FILE));
            let file = name.and_then(|n| asset.files.iter().find(|f| f.name == n && f.made_by.is_none()));
            file.map_or_default(|f| ws.extensions.recognize(f).into_iter().map(|(ty, _)| ty.label.clone()).collect())
        })
        .collect();
    // Its source, edited in the tool its format is: in the active layer, copied there first where it is below.
    let tool = ws.source_tool(&asset.path).map(|(category, file)| (category, file.layer.clone()));
    let active = ws.editable_layer().map(|l| l.name.clone());
    // The importer's settings, as it describes them, in its layer.
    let importing = layers.iter().position(|l| l.get("op").and_then(toml::Value::as_str) == Some(name)).filter(|_| import.is_some());
    let sheet = Sheet {
        index: &ws.index,
        extensions: &ws.extensions,
        fields: importer.map_or_default(|i| i.fields.clone()),
        within: importing.map(|i| format!("stack.{i}")),
        derived: None,
    };
    let settings: Vec<(&String, &toml::Value)> = importing
        .and_then(|i| layers[i].as_table())
        .into_iter()
        .flatten()
        .filter(|(k, v)| k.as_str() != "op" && !sheet.custom(k) && !is_table_list(v))
        .collect();
    let ops = ws.extensions.operations();
    let field_edit = &mut ws.field_edit;
    // What is wrong with it as it is written, told from its shape, and what comes down to each layer.
    let (plan, wrong) = ws.extensions.plan_written(asset);
    let editable = target.is_ok();
    theme::section(ui, &format!("Stack  ·  {}", layers.len()), |ui| {
        egui::Grid::new(("stack", &asset.path)).num_columns(3).spacing([12.0, 6.0]).min_row_height(ROW_HEIGHT).show(ui, |ui| {
            for (i, layer) in layers.iter().enumerate() {
                let op = layer.get("op").and_then(toml::Value::as_str).unwrap_or_default();
                let text = |key: &str| layer.get(key).and_then(toml::Value::as_str);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{i}")).color(theme::weak()));
                    if editable {
                        let (above, below) = (plan.above.get(i).cloned().flatten(), plan.above.get(i + 1).cloned().flatten());
                        let place = Place { asset, at: i, count: layers.len(), above: above.as_ref(), below: below.as_ref() };
                        layer_menu(ui, &ws.extensions, &ws.index, &place, edit);
                    }
                });
                match op {
                    project::graph::FILE => {
                        ui.label(RichText::new("Stored file").color(theme::weak()));
                        let source_layer = Some(i) == importing.map(|j| j - 1);
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(text("file").unwrap_or_default()).monospace());
                            for ty in &recognized[i] {
                                badge(ui, ty);
                            }
                        });
                        // Edited in its tool: a row of its own, under the file.
                        if let (true, Some((category, home)), Some(uid), Some(source)) = (source_layer, &tool, &asset.uid, source) {
                            ui.end_row();
                            ui.label("");
                            ui.label("");
                            edit_in_button(ui, *category, home, &active, source, uid, edit);
                        }
                    }
                    project::graph::OUTPUT => {
                        ui.label(RichText::new("Output").color(theme::weak()));
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(text("name").unwrap_or_default()).monospace());
                            badge(ui, text("retain").unwrap_or("transient"));
                            badge(ui, text("when").unwrap_or("lazy"));
                        });
                    }
                    project::graph::GRAPH => {
                        ui.label(RichText::new("Graph").color(theme::weak()));
                        let count = layer.get("ops").and_then(toml::Value::as_table).map_or(0, toml::Table::len);
                        ui.label(format!("{count} operation{}", if count == 1 { "" } else { "s" }));
                    }
                    _ if Some(i) == importing => {
                        ui.label(RichText::new("Importer").color(theme::weak()));
                        importer_row(ui, asset, i, name, importer, &fits, target.is_ok(), edit);
                    }
                    _ => operation_row(ui, &ops, op, layer),
                }
                ui.end_row();
            }
        });
        if editable {
            ui.menu_button(format!("{}  Add layer", icons::PLUS), |ui| {
                let end = plan.above.last().cloned().flatten();
                add_items(ui, &ws.extensions, &ws.index, asset, layers.len(), end.as_ref(), edit);
            });
        }
        problems_ui(ui, &wrong);
        if !settings.is_empty() {
            ui.add_space(4.0);
            property_sheet(ui, &sheet, field_edit, target, asset, &settings, nav, edit);
        }
        fits_ui(ui, &fits, name);
    });
}

/// What is wrong with a stack, each as its rows number it.
fn problems_ui(ui: &mut egui::Ui, wrong: &[project::graph::Problem]) {
    for problem in wrong {
        let said = format!("{}: {}", problem_place(&problem.at), problem.what);
        ui.label(RichText::new(said).small().color(ui.visuals().error_fg_color));
    }
}

/// A layer of a stack, for its menu: where it is, what comes down to it and what comes out of it.
struct Place<'a> {
    asset: &'a Asset,
    at: usize,
    count: usize,
    above: Option<&'a pe2_ext::Type>,
    below: Option<&'a pe2_ext::Type>,
}

/// A change to a stack's layers, as an edit: `change` the declaration's text to the text wanted.
fn layers_edit(asset: &Asset, label: String, text: anyhow::Result<String>) -> Option<EditRequest> {
    Some(EditRequest::Write {
        uid: asset.uid.clone().unwrap_or_default(),
        file: project::index::ASSET_FILE.to_string(),
        bytes: text.ok()?.into_bytes(),
        label,
        merge: None,
    })
}

/// A layer's menu: moving it, removing it, and adding one over or under it.
fn layer_menu(ui: &mut egui::Ui, extensions: &project::ext::Extensions, index: &project::index::Index, place: &Place, edit: &mut Option<EditRequest>) {
    use project::graph::layers;
    let (asset, at) = (place.asset, place.at);
    let name = asset.path.rsplit('/').next().unwrap_or(&asset.path);
    ui.menu_button(icons::DOTS_THREE, |ui| {
        if ui.add_enabled(at > 0, egui::Button::new(format!("{}  Move up", icons::ARROW_UP))).clicked() {
            *edit = layers_edit(asset, format!("Move a layer of {name} up"), layers::shift(asset.text(), at, at - 1));
        }
        if ui.add_enabled(at + 1 < place.count, egui::Button::new(format!("{}  Move down", icons::ARROW_DOWN))).clicked() {
            *edit = layers_edit(asset, format!("Move a layer of {name} down"), layers::shift(asset.text(), at, at + 1));
        }
        if ui.button(format!("{}  Remove", icons::TRASH)).clicked() {
            *edit = layers_edit(asset, format!("Remove a layer of {name}"), layers::remove(asset.text(), at));
        }
        ui.separator();
        ui.menu_button(format!("{}  Add above", icons::PLUS), |ui| {
            add_items(ui, extensions, index, asset, at, place.above, edit);
        });
        ui.menu_button(format!("{}  Add below", icons::PLUS), |ui| {
            add_items(ui, extensions, index, asset, at + 1, place.below, edit);
        });
    });
}

/// What may be added at a place of a stack: what takes what comes down to it, a tap and a graph; on top, where nothing
/// comes down, what starts a stack - a stored file, or the version below.
fn add_items(
    ui: &mut egui::Ui,
    extensions: &project::ext::Extensions,
    index: &project::index::Index,
    asset: &Asset,
    at: usize,
    above: Option<&pe2_ext::Type>,
    edit: &mut Option<EditRequest>,
) {
    use project::graph::{self, layers};
    let name = asset.path.rsplit('/').next().unwrap_or(&asset.path);
    let mut add = |ui: &mut egui::Ui, label: &str, says: &str, layer: toml::Table| {
        if ui.button(label).on_hover_text(says).clicked() {
            *edit = layers_edit(asset, format!("Add {label} to {name}"), layers::add(asset.text(), at, layer));
            ui.close();
        }
    };
    let table = |entries: &[(&str, &str)]| -> toml::Table { entries.iter().map(|(k, v)| ((*k).to_string(), toml::Value::from(*v))).collect() };
    if let Some(above) = above {
        for (op, label) in extensions.layers_taking(above) {
            add(ui, &label, &op, table(&[("op", op.as_str())]));
        }
        ui.separator();
        let taken: Vec<&str> = asset.fields().get("stack").and_then(toml::Value::as_array).into_iter().flatten().filter_map(|l| l.get("name")?.as_str()).collect();
        let names = std::iter::once("output".to_string()).chain((2..=taken.len() + 2).map(|n| format!("output_{n}")));
        let free = names.into_iter().find(|n| !taken.contains(&n.as_str())).unwrap_or_default();
        add(ui, "Output", "A tap: what is here, as an output of this node", table(&[("op", graph::OUTPUT), ("name", free.as_str())]));
    } else {
        for file in asset.files.iter().filter(|f| f.made_by.is_none() && f.name != project::index::ASSET_FILE) {
            add(ui, &file.name, "A stored file", table(&[("op", graph::FILE), ("file", file.name.as_str())]));
        }
        let version = asset.uid.as_ref().and_then(|uid| index.version(&format!("{}{uid}", project::index::BELOW)));
        if let Some(version) = version {
            // Its type declared, as a stack's shape cannot reach the version below.
            let (plan, _) = extensions.plan(version);
            let native = plan.outputs.iter().find(|o| o.name == "native").map(|o| plan.steps[o.from].ty.to_string());
            let mut layer = table(&[("op", graph::BELOW)]);
            if let Some(native) = native {
                layer.insert("type".into(), native.into());
            }
            add(ui, "The version below", "What the layer below has of this asset, its native form, not copied", layer);
        }
    }
    add(ui, "Graph", "Operations wired together, its result going on down", table(&[("op", graph::GRAPH)]));
}

/// Where a problem of a stack is, as its rows number it: `stack[0].ops.encoded` is layer 0's operation `encoded`.
fn problem_place(at: &str) -> String {
    let Some(rest) = at.strip_prefix("stack[") else { return at.to_string() };
    let (layer, inside) = rest.split_once(']').unwrap_or((rest, ""));
    match inside.strip_prefix(".ops.") {
        Some(operation) => format!("Layer {layer}, {operation}"),
        None => format!("Layer {layer}"),
    }
}

/// A stack's layer of an operation: its label and name, then its settings, a row of their own under it.
fn operation_row(ui: &mut egui::Ui, ops: &project::graph::Ops, op: &str, layer: &toml::Value) {
    ui.label(RichText::new("Operation").color(theme::weak()));
    ui.horizontal_wrapped(|ui| {
        ui.label(ops.get(op).map_or(op, |def| def.label.as_str()));
        ui.label(RichText::new(op).small().color(theme::weak()));
    });
    let settings: Vec<String> =
        layer.as_table().into_iter().flatten().filter(|(k, _)| k.as_str() != "op").map(|(k, v)| format!("{k} = {v}")).collect();
    if !settings.is_empty() {
        ui.end_row();
        ui.label("");
        ui.label("");
        ui.label(RichText::new(settings.join("   ")).monospace().small().color(theme::weak()));
    }
}

/// The button editing a source in the tool its format is edited in: in the active layer, copied there first where the
/// source is below it (`home`, the layer holding it).
fn edit_in_button(
    ui: &mut egui::Ui,
    category: crate::external::Category,
    home: &str,
    active: &Result<String, String>,
    source: &str,
    uid: &str,
    edit: &mut Option<EditRequest>,
) {
    let tool = category.label().to_lowercase();
    let label = format!("{}  Edit in {tool}", icons::PENCIL_SIMPLE);
    let (enabled, says) = match active {
        Ok(l) if l == home => {
            (true, format!("Open {source} in the {tool} set in the settings (External tools); what it saves is imported again"))
        }
        Ok(l) => (true, format!("Copy the asset into {l} (an undoable step), then open its {source} there")),
        Err(why) => (false, why.clone()),
    };
    let response = ui.add_enabled(enabled, egui::Button::new(label)).on_hover_text(&says);
    let response = if enabled { response } else { response.on_disabled_hover_text(&says) };
    if response.clicked() {
        *edit = Some(EditRequest::EditIn(uid.to_string()));
    }
}

/// The operations taking an asset's source, where there is a choice: the one in use marked.
fn fits_ui(ui: &mut egui::Ui, fits: &[(String, String)], name: &str) {
    if fits.len() <= 1 && fits.iter().all(|(n, _)| n == name) {
        return;
    }
    ui.add_space(4.0);
    ui.label(RichText::new("Imports for this source").small().color(theme::weak()));
    for (fit, label) in fits {
        ui.horizontal_wrapped(|ui| {
            let text = RichText::new(label);
            ui.label(if fit == name { text.strong() } else { text });
            if fit == name {
                ui.label(RichText::new("in use").small().color(theme::weak()));
            }
        });
    }
}

/// The importer: its label and name, or - where the asset can be edited and several importers are offered for its
/// source - a choice of them, switching as an undoable step.
#[allow(clippy::too_many_arguments)]
fn importer_row(
    ui: &mut egui::Ui,
    asset: &Asset,
    at: usize,
    name: &str,
    importer: Option<&pe2_ext::OpDef>,
    offered: &[(String, String)],
    editable: bool,
    edit: &mut Option<EditRequest>,
) {
    ui.horizontal_wrapped(|ui| {
        if editable && offered.len() > 1 {
            let shown = importer.map_or(name, |i| i.label.as_str());
            let mut chosen = None;
            egui::ComboBox::from_id_salt(("importer", &asset.path)).selected_text(shown).show_ui(ui, |ui| {
                for (fit, label) in offered {
                    if ui.selectable_label(fit == name, label).clicked() && fit != name {
                        chosen = Some(fit.clone());
                    }
                }
            });
            let within = format!("stack.{at}");
            let switched = chosen.and_then(|fit| set_field(asset, Some(&within), "op", fit.as_str().into(), "importer"));
            if let Some(switched) = switched {
                *edit = Some(switched);
            }
        } else if let Some(importer) = importer {
            ui.label(&importer.label);
            ui.label(RichText::new(name).small().color(theme::weak()));
        } else {
            let unknown = format!("{name}: no extension provides it");
            ui.label(RichText::new(unknown).color(ui.visuals().error_fg_color));
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
    let has_import = project::graph::import_of(asset.fields()).is_some();
    theme::section(ui, "Bundle", |ui| {
        egui::Grid::new("bundle files").num_columns(4).spacing([14.0, 4.0]).min_row_height(ROW_HEIGHT).show(ui, |ui| {
            for f in &asset.files {
                let tool = Category::of(&f.path);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icons::FILE).color(theme::weak()));
                    ui.label(&f.name);
                });
                ui.label(RichText::new(size_text(f.size)).small().color(theme::weak()));
                match &f.made_by {
                    // Made of the source, kept in the cache: not the layer's to edit.
                    Some(importer) if has_import => {
                        ui.label(RichText::new("imported").small().color(theme::weak())).on_hover_text(format!(
                            "Made of the source by {importer}, kept in the project cache; edit the source instead"
                        ));
                    }
                    // Made by its stack of what it reads: kept in the cache.
                    Some(maker) => {
                        ui.label(RichText::new("made").small().color(theme::weak())).on_hover_text(format!(
                            "Made by its stack ({maker}), kept in the project cache; edit what it is made of instead"
                        ));
                    }
                    None => {
                        ui.label(RichText::new(&f.layer).small().color(theme::weak()));
                    }
                }
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

#[cfg(test)]
mod tests {
    #[test]
    fn a_problem_says_where_it_is_as_the_rows_number_it() {
        assert_eq!(super::problem_place("stack[0].ops.encoded"), "Layer 0, encoded");
        assert_eq!(super::problem_place("stack[2]"), "Layer 2");
        assert_eq!(super::problem_place("kind.outputs[3]"), "kind.outputs[3]");
    }

    use super::item;

    #[test]
    fn a_table_of_parts_keeps_a_line_each_in_its_order() {
        let mut document: toml_edit::DocumentMut = r#"kind = "image"
mode = "indexed_4bpp"
variants.day = { palette = "warm" }
variants.night = { palette = "cold" }
size = [2, 1]

[[stack]]
op = "file"
file = "native.texels"
"#
        .parse()
        .unwrap();
        let variants: toml::Value = toml::from_str::<toml::Table>(
            r#"variants.night = { palette = "cold" }
variants.dusk = { palette = "warm" }"#,
        )
        .unwrap()
        .remove("variants")
        .unwrap();
        document.insert("variants", item(&variants));
        let written = document.to_string();
        assert!(
            written.contains("variants.night = { palette = \"cold\" }\nvariants.dusk = { palette = \"warm\" }\n"),
            "{written}"
        );
        assert!(!written.contains("day"), "{written}");
        assert!(written.contains("[[stack]]\nop = \"file\"\nfile = \"native.texels\""), "{written}");
        let parsed: toml::Table = toml::from_str(&written).unwrap();
        assert_eq!(parsed["variants"].as_table().unwrap().keys().collect::<Vec<_>>(), ["night", "dusk"]);
    }
}
