//! A package as the panes show it: what its sources ask for, read from its `reflection`, so it is what they say now, with what it is set to
//! as written, which an edit changes at once: its slots, each a chip taking a node of the slot's type, and its options, each a field of the
//! type its source defines, together as its sources declare them; and what it depends on. Args are the source's, and shown only. A slot is selected as `slots/<name>`, in
//! every pane at once.

use std::collections::{BTreeSet, HashMap};

use pe2_ext::{Cell, Icon, Style, Table, Ui, Value, View};

use crate::typed::{self, Step};

/// How rows line up: a character of a name, and the room after it.
const CHARACTER: f32 = 8.0;
const GAP: f32 = 16.0;
/// How wide an integer's field is: room for one of any primitive up to 32 bits, sign and all.
const CELL: f32 = 80.0;

/// A package's slots and options, as the head of the contents pane or the inspector's sheet.
#[derive(Default)]
pub struct Slots {
    sheet: bool,
    told: Option<Result<Table, String>>,
    /// The project's revision it was told at: its sources change outside any edit of the asset it shows.
    revision: u64,
    /// What is being typed in an input, by its id: with what was held when typing began, so that a change made elsewhere (an undo)
    /// shows through.
    typed: HashMap<String, (String, String)>,
    /// What was typed and refused, by input: why it cannot be held.
    refused: HashMap<String, String>,
}

/// What an input asks for: a value written, or none, back to the default.
enum Edit {
    Set(Value),
    Clear,
}

impl Slots {
    pub fn sheet() -> Slots {
        Slots { sheet: true, ..Slots::default() }
    }

    fn ask(&mut self, ui: &Ui) {
        let revision = ui.project().revision();
        if self.told.is_some() && self.revision == revision {
            return;
        }
        self.revision = revision;
        let Some(path) = ui.asset().map(|asset| asset.path().to_string()) else { return };
        let told = ui.project().table(&path, "reflection");
        self.told = told.map(|told| told.map_err(|e| format!("{e:#}")));
    }
}

fn tables(table: &Table, key: &str) -> Vec<Table> {
    table.get(key).and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_table).cloned().collect()
}

fn text<'a>(table: &'a Table, key: &str) -> &'a str {
    table.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn words(table: &Table, key: &str) -> Vec<String> {
    table.get(key).and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect()
}

fn is_package(slot: &Table) -> bool {
    text(slot, "type") == "package"
}

/// What is set of a slot or an option, as written.
fn held_in<'a>(field: Option<&'a Value>, name: &str) -> Option<&'a Value> {
    field?.get(name)
}

/// The tables of a list by the section their sources put them in, in the order they come; one with no label in the first.
fn sections(list: &[Table]) -> Vec<(&str, Vec<&Table>)> {
    let mut found: Vec<(&str, Vec<&Table>)> = Vec::new();
    for table in list {
        let label = text(table, "label");
        match found.iter_mut().find(|(known, _)| *known == label) {
            Some((_, tables)) => tables.push(table),
            None => found.push((label, vec![table])),
        }
    }
    found.sort_by_key(|(label, _)| !label.is_empty());
    found
}

/// An option's default as an input shows it, if its source gives one: a string without its quotes, a bool as true or false.
fn default_shown(option: &Table) -> Option<String> {
    let default = option.get("default")?;
    let shown = match (text(option, "type"), default.as_str()) {
        ("string", Some(text)) => text.strip_prefix('"').and_then(|text| text.strip_suffix('"')).unwrap_or(text).to_string(),
        ("bool", Some(text)) => (if text == "1" { "true" } else { "false" }).to_string(),
        (_, Some(text)) => text.to_string(),
        (_, None) => default.to_string(),
    };
    Some(shown)
}

/// Why an option's value cannot be read; none where it can. One the package leaves unset is its default, where its source gives one.
fn wrong(option: &Table, value: Option<&Value>) -> Option<String> {
    let Some(value) = value else { return (!option.contains_key("default")).then(|| "not set".to_string()) };
    if let Some(shape) = option.get("shape").and_then(Value::as_table) {
        return typed::wrong(shape, value);
    }
    let right = match text(option, "type") {
        "int" => value.is_integer(),
        "bool" => value.is_bool(),
        "string" => value.is_str(),
        _ => value.as_str().is_some_and(|member| words(option, "members").iter().any(|known| known == member)),
    };
    (!right).then(|| "not of its type".to_string())
}

/// Whether an option is a struct or an array, which opens into groups; a single integer is a field.
fn is_grouped(option: &Table) -> bool {
    option.get("shape").and_then(Value::as_table).is_some_and(|shape| shape.contains_key("count") || shape.contains_key("fields"))
}

/// A value as an input shows it.
fn shown(value: &Value) -> String {
    value.as_str().map_or_else(|| value.to_string(), str::to_string)
}

/// A value of a table of a package's, the rest kept: what a change writes.
fn with(field: Option<&Value>, key: &str, value: Value) -> Value {
    let mut table = field.and_then(Value::as_table).cloned().unwrap_or_default();
    table.insert(key.to_string(), value);
    Value::Table(table)
}

fn without(field: Option<&Value>, key: &str) -> Option<Value> {
    let mut table = field.and_then(Value::as_table).cloned().unwrap_or_default();
    table.remove(key);
    (!table.is_empty()).then_some(Value::Table(table))
}

/// What a package holds that no source reads: what it says is left from a source since changed.
fn unread(field: Option<&Value>, named: &[Table]) -> Vec<String> {
    let read: BTreeSet<&str> = named.iter().map(|table| text(table, "name")).collect();
    field.and_then(Value::as_table).into_iter().flatten().map(|(key, _)| key.clone()).filter(|key| !read.contains(key.as_str())).collect()
}

/// How wide a column of names is.
fn column(names: &[Table]) -> f32 {
    #[expect(clippy::cast_precision_loss, reason = "a name's length")]
    let widest = names.iter().map(|table| text(table, "name").len()).max().unwrap_or(0) as f32;
    widest * CHARACTER + GAP
}

impl View for Slots {
    fn show(&mut self, ui: &mut Ui) {
        self.ask(ui);
        let reflected = match &self.told {
            Some(Ok(table)) => table.clone(),
            Some(Err(why)) => {
                ui.styled(why, Style::Error);
                return;
            }
            None => return,
        };
        // A slot kept out of the editor is filled without it: by default, by the package itself.
        let shown = tables(&reflected, "slots").into_iter().filter(|slot| !slot.get("hidden").and_then(Value::as_bool).unwrap_or(false));
        let (dependencies, slots): (Vec<Table>, Vec<Table>) = shown.partition(is_package);
        if self.sheet {
            self.inputs(ui, &slots, &tables(&reflected, "options"));
            detail(ui, &slots);
            leftovers(ui, &slots, &tables(&reflected, "options"));
            depends(ui, &dependencies);
        } else {
            tree(ui, &reflected, &slots);
        }
    }
}

fn tree(ui: &mut Ui, reflected: &Table, slots: &[Table]) {
    let sources = words(reflected, "sources");
    if !sources.is_empty() {
        ui.styled("SOURCES", Style::Note);
        for source in &sources {
            ui.label(source);
        }
    }
    if slots.is_empty() {
        return;
    }
    ui.styled("SLOTS", Style::Note);
    let selected = ui.selected();
    ui.wrapped(|ui| {
        for slot in slots {
            let part = format!("slots/{}", text(slot, "name"));
            if ui.chip(text(slot, "name"), selected.as_deref() == Some(part.as_str())) {
                ui.select(&part);
            }
            ui.tooltip(text(slot, "type"));
        }
    });
}

/// The slot selected: what its sources read of it and the args they give.
fn detail(ui: &mut Ui, slots: &[Table]) {
    let selected = ui.selected();
    let shown = selected.as_deref().and_then(|part| part.strip_prefix("slots/"));
    let Some(slot) = shown.and_then(|name| slots.iter().find(|slot| text(slot, "name") == name)) else { return };
    ui.section(text(slot, "name"), |ui| {
        for read in words(slot, "reads") {
            ui.label(if read.is_empty() { "the whole value" } else { &read });
        }
        for given in tables(slot, "args") {
            let at = text(&given, "path");
            let values = words(&given, "values").join(", ");
            ui.styled(&format!("{}{} = {values}", if at.is_empty() { String::new() } else { format!("{at} ") }, text(&given, "arg")), Style::Note);
        }
    });
}

/// What a package holds that no source reads.
fn leftovers(ui: &mut Ui, slots: &[Table], options: &[Table]) {
    let Some(asset) = ui.asset() else { return };
    let (bind, option) = (asset.field("bind").cloned(), asset.field("option").cloned());
    let stale = |field: &'static str, held: &Option<Value>, named: &[Table]| unread(held.as_ref(), named).into_iter().map(move |key| (field, key));
    let listed: Vec<(&str, String)> = stale("bind", &bind, slots).chain(stale("option", &option, options)).collect();
    if listed.is_empty() {
        return;
    }
    let mut removed = None;
    ui.section("No source reads these", |ui| {
        for (field, key) in &listed {
            ui.horizontal(|ui| {
                ui.styled(key, Style::Weak);
                if ui.button("Remove") {
                    removed = Some((*field, key.clone()));
                }
            });
        }
    });
    if let Some((field, key)) = removed {
        let held = if field == "bind" { &bind } else { &option };
        ui.set(field, without(held.as_ref(), &key).as_ref());
    }
}

/// The packages it reads, each opening it.
fn depends(ui: &mut Ui, dependencies: &[Table]) {
    if dependencies.is_empty() {
        return;
    }
    let mut opened = None;
    ui.section(&format!("Depends on  ·  {}", dependencies.len()), |ui| {
        ui.wrapped(|ui| {
            for dependency in dependencies {
                if ui.chip(text(dependency, "name"), false) && !text(dependency, "package").is_empty() {
                    opened = Some(text(dependency, "package").to_string());
                }
                ui.tooltip(&format!("{} files read", words(dependency, "reads").len()));
            }
        });
    });
    if let Some(path) = opened {
        ui.navigate(&path);
    }
}

impl Slots {
    /// What a package is given, slots and options together, in the order its sources declare them, under their labels: a slot a chip taking
    /// a node of its type, an option a field of its type.
    fn inputs(&mut self, ui: &mut Ui, slots: &[Table], options: &[Table]) {
        let mut tables: Vec<Table> = slots.iter().chain(options).cloned().collect();
        tables.sort_by_key(|table| table.get("order").and_then(Value::as_integer).unwrap_or(0));
        if tables.is_empty() {
            return;
        }
        let (name, of_type) = (column(&tables) + GAP, 8.0 * CHARACTER);
        let held = |field: &str| ui.asset().and_then(|asset| asset.field(field).cloned());
        let (bind, set) = (held("bind"), held("option"));
        let selected = ui.selected();
        let mut asked: Option<(&str, String, Value)> = None;
        let mut cleared = None;
        let mut grids = vec![];
        for (label, listed) in sections(&tables) {
            let title = if label.is_empty() { format!("Inputs  ·  {}", listed.len()) } else { label.to_string() };
            ui.section(&title, |ui| {
                for input in listed {
                    let input_name = text(input, "name");
                    ui.horizontal(|ui| {
                        // A slot is read at addresses; an option says whether it is read.
                        if input.contains_key("reads") {
                            let part = format!("slots/{input_name}");
                            if ui.chip(input_name, selected.as_deref() == Some(part.as_str())) {
                                ui.select(&part);
                            }
                            ui.tab(name);
                            let filled = held_in(bind.as_ref(), input_name);
                            ui.styled(text(input, "type"), if filled.is_none() { Style::Error } else { Style::Note });
                            ui.tab(name + of_type);
                            if let Some(new) = ui.reference_taking(input_name, text(input, "type"), filled) {
                                asked = Some(("bind", input_name.to_string(), new));
                            }
                        } else {
                            let value = held_in(set.as_ref(), input_name);
                            ui.styled(input_name, Style::Normal);
                            ui.tab(name);
                            ui.styled(text(input, "type"), Style::Note);
                            ui.tab(name + of_type);
                            if is_grouped(input) {
                                grids.push(input);
                            } else {
                                match self.editor(ui, input, value) {
                                    Some(Edit::Set(new)) => asked = Some(("option", input_name.to_string(), new)),
                                    Some(Edit::Clear) => cleared = Some(input_name.to_string()),
                                    None => {}
                                }
                            }
                            // Set, it can go back to its default, or to having no value.
                            let back = if input.contains_key("default") { "Back to its default" } else { "Leave it not set" };
                            if value.is_some() && ui.icon_button(Icon::Cross, back) {
                                cleared = Some(input_name.to_string());
                            }
                            if let Some(why) = self.refused.get(input_name) {
                                ui.styled(why, Style::Error);
                            }
                            // What is read needs a value, if it is live; what is not read cannot need one.
                            let read = input.get("read").and_then(Value::as_bool).unwrap_or(false);
                            if let Some(why) = wrong(input, value) {
                                ui.styled(&why, if read || value.is_some() { Style::Error } else { Style::Note });
                            }
                        }
                    });
                    if grids.last() == Some(&input) {
                        ui.indented(|ui| match self.grouped(ui, input, held_in(set.as_ref(), input_name)) {
                            Some(Edit::Set(new)) => asked = Some(("option", input_name.to_string(), new)),
                            Some(Edit::Clear) => cleared = Some(input_name.to_string()),
                            None => {}
                        });
                    }
                }
            });
        }
        if let Some((field, key, new)) = asked {
            let holding = if field == "bind" { &bind } else { &set };
            ui.set(field, Some(&with(holding.as_ref(), &key, new)));
        } else if let Some(name) = cleared {
            ui.set("option", without(set.as_ref(), &name).as_ref());
        }
    }

    /// A text input, its typing kept across frames: what was typed, or what is held when that changed since typing began; the default
    /// greyed while it is empty. What was typed, when it is committed.
    fn typing(&mut self, ui: &mut Ui, id: &str, held: &str, hint: &str, width: f32) -> Option<String> {
        let entry = self.typed.entry(id.to_string()).or_insert_with(|| (held.to_string(), held.to_string()));
        if entry.0 != held {
            *entry = (held.to_string(), held.to_string());
        }
        let committed = ui.text_edit_hinted(id, &mut entry.1, hint, width);
        committed.then(|| entry.1.trim().to_string())
    }

    /// An option that is one value: a box for a bool, a choice for an enum, a field for the rest, which shows the default while the
    /// package sets none. An integer is held to its type's range; what is not is refused, and said.
    fn editor(&mut self, ui: &mut Ui, option: &Table, value: Option<&Value>) -> Option<Edit> {
        let name = text(option, "name");
        let default = default_shown(option);
        match text(option, "type") {
            "bool" => {
                let held = value.and_then(Value::as_bool).or_else(|| default.as_deref().map(|shown| shown == "true"));
                // A box says true or false, and this is neither yet: both are offered, as false would take two clicks.
                let Some(mut on) = held else {
                    let choices = ["false".to_string(), "true".to_string()];
                    return ui.combo(&format!("option.{name}"), "", &choices).map(|chosen| Edit::Set(Value::Boolean(chosen == 1)));
                };
                let changed = ui.checkbox("", &mut on).then_some(Edit::Set(Value::Boolean(on)));
                if value.is_none() {
                    ui.styled("default", Style::Weak);
                }
                changed
            }
            "enum" => {
                let members = words(option, "members");
                let shown = value.and_then(Value::as_str).unwrap_or_default();
                ui.combo(&format!("option.{name}"), shown, &members).map(|chosen| Edit::Set(Value::String(members[chosen].clone())))
            }
            ty => {
                let held = value.map_or_else(String::new, shown);
                let typed = self.typing(ui, &format!("option.{name}"), &held, default.as_deref().unwrap_or_default(), 0.0)?;
                if typed.is_empty() {
                    self.refused.remove(name);
                    return value.is_some().then_some(Edit::Clear);
                }
                if ty == "string" {
                    return Some(Edit::Set(Value::String(typed)));
                }
                let range = option.get("shape").and_then(Value::as_table).and_then(|shape| Some((shape.get("low")?.as_integer()?, shape.get("high")?.as_integer()?)));
                let (low, high) = range.unwrap_or((i64::MIN, i64::MAX));
                match typed::checked(&typed, ty, low, high) {
                    Ok(number) => {
                        self.refused.remove(name);
                        Some(Edit::Set(Value::Integer(number)))
                    }
                    Err(why) => {
                        self.refused.insert(name.to_string(), why);
                        None
                    }
                }
            }
        }
    }

    /// An option of a registered type or an array, as an inspector lists one: an array's elements and a struct's fields each a
    /// group that opens, down to its integers, each a row with a field showing what the package sets or, greyed, the default. A
    /// field emptied goes back to the default; an integer out of its type's range is refused, and said.
    fn grouped(&mut self, ui: &mut Ui, option: &Table, value: Option<&Value>) -> Option<Edit> {
        let name = text(option, "name").to_string();
        let shape = option.get("shape").and_then(Value::as_table)?.clone();
        let default = option.get("default");
        let full = typed::effective(&shape, default, value);
        let mut at = Members { option: &name, whole: &shape, value, default, full: &full, path: vec![], edit: None };
        self.members(ui, &shape, &mut at);
        let prefix = format!("option.{name}.");
        for why in self.refused.iter().filter(|(id, _)| id.starts_with(&prefix)).map(|(_, why)| why.clone()).collect::<Vec<_>>() {
            ui.styled(&why, Style::Error);
        }
        at.edit
    }

    /// What is inside a shape, at the path `at` has reached: an integer a row, anything else a group, which opens; a dynamic array's
    /// elements each removable, and one more to add, a copy of the last.
    fn members(&mut self, ui: &mut Ui, shape: &Table, at: &mut Members<'_>) {
        let children = typed::children(shape, typed::at(Some(at.full), &at.path));
        let dynamic = typed::is_dynamic(shape);
        let elements = children.len();
        #[expect(clippy::cast_precision_loss, reason = "a name's length")]
        let label = children.iter().filter(|(.., child)| typed::range_of(child).is_some()).map(|(_, title, _)| title.len()).max().unwrap_or(0) as f32;
        let label = label * CHARACTER + GAP;
        for (step, title, child) in children {
            at.path.push(step);
            let id = format!("option.{}.{}", at.option, path_text(&at.path));
            if let Some((low, high)) = typed::range_of(&child) {
                ui.horizontal(|ui| {
                    ui.styled(&title, Style::Normal);
                    ui.tooltip(&typed::about(&child));
                    ui.tab(label);
                    let held = typed::at(at.value, &at.path).map_or_else(String::new, shown);
                    let hint = typed::at(at.default, &at.path).map_or_else(String::new, shown);
                    let Some(typed) = self.typing(ui, &id, &held, &hint, CELL) else { return };
                    let new = if typed.is_empty() {
                        None
                    } else {
                        match typed::checked(&typed, text(&child, "type"), low, high) {
                            Ok(number) => Some(number),
                            Err(why) => {
                                self.refused.insert(id.clone(), format!("{}: {why}", path_text(&at.path)));
                                return;
                            }
                        }
                    };
                    self.refused.remove(&id);
                    at.edit = Some(typed::set(at.whole, at.value, at.default, &at.path, new).map_or(Edit::Clear, Edit::Set));
                });
            } else {
                // A group says when the package sets something in it, so that a change shows while it is closed.
                let holds = typed::at(at.value, &at.path).is_some_and(|value| value.as_table().is_none_or(|table| !table.is_empty()));
                let removable = dynamic && elements > 1;
                ui.collapsing(&id, &title, if holds { "set" } else { "" }, |ui| {
                    self.members(ui, &child, at);
                    if removable && ui.icon_button(Icon::Cross, "Remove this element") {
                        let Some(Step::Index(index)) = at.path.last() else { return };
                        at.edit = Some(typed::remove(at.whole, at.value, at.default, *index).map_or(Edit::Clear, Edit::Set));
                    }
                });
                ui.tooltip(&typed::about(&child));
            }
            at.path.pop();
        }
        if dynamic && ui.icon_button(Icon::Plus, "Add an element: a copy of the last") {
            at.edit = Some(typed::add(at.whole, at.value, at.default).map_or(Edit::Clear, Edit::Set));
        }
    }
}

/// Where a walk of an option's members is: the option, its whole shape, what its package sets and its default, the path reached,
/// and what an input there asked for.
struct Members<'a> {
    option: &'a str,
    whole: &'a Table,
    value: Option<&'a Value>,
    default: Option<&'a Value>,
    /// The value in force, which a dynamic array's length is read from.
    full: &'a Value,
    path: Vec<Step>,
    edit: Option<Edit>,
}

/// A path as an input's id and a message name it: `1.wedges`, `steps.0`.
fn path_text(path: &[Step]) -> String {
    let steps = path.iter().map(|step| match step {
        Step::Field(name) => name.clone(),
        Step::Index(index) => index.to_string(),
    });
    steps.collect::<Vec<_>>().join(".")
}

/// What a node makes for a package, from what its tokenizer declares: nothing is made, as a value may need args. For the target in use.
#[derive(Default)]
pub struct Tokens {
    told: Option<Result<Table, String>>,
}

impl View for Tokens {
    fn show(&mut self, ui: &mut Ui) {
        if self.told.is_none() {
            let Some(path) = ui.asset().map(|asset| asset.path().to_string()) else { return };
            let own = ui.project().target().and_then(|target| ui.project().table(&path, &format!("{}.{target}", pe2_ext::tokens::DECLARED))?.ok());
            let told = own.map(Ok).or_else(|| ui.project().table(&path, pe2_ext::tokens::DECLARED));
            self.told = told.map(|told| told.map_err(|e| format!("{e:#}")));
        }
        let table = match &self.told {
            Some(Ok(table)) => table,
            Some(Err(why)) => {
                ui.styled(why, Style::Error);
                return;
            }
            None => return,
        };
        let entries = tables(table, "entries");
        ui.section("What a package can read", |ui| {
            let rows: Vec<Vec<Cell>> = entries.iter().map(row).collect();
            let _ = ui.table("tokens", &rows);
        });
    }
}

/// An entry of a tokenizer: as a source names it, what it generates, and the args it needs.
fn row(entry: &Table) -> Vec<Cell> {
    let name = if text(entry, "name").is_empty() { "(whole)".to_string() } else { format!("_{}", text(entry, "name")) };
    let args = words(entry, "args");
    let needs = if args.is_empty() { String::new() } else { format!("needs {}", args.join(", ")) };
    vec![Cell::Text(name, Style::Strong), Cell::Text(text(entry, "yields").to_string(), Style::Weak), Cell::Text(needs, Style::Note)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(names: &[&str]) -> Vec<Table> {
        names.iter().map(|name| Table::from_iter([("name".to_string(), Value::String((*name).to_string()))])).collect()
    }

    #[test]
    fn a_change_keeps_what_else_is_held() {
        let held: Table = "bind = { body = 'uid://a', hand = 'uid://b' }".parse().unwrap();
        let new = with(held.get("bind"), "body", "uid://c".into());
        assert_eq!(new["body"].as_str(), Some("uid://c"));
        assert_eq!(new["hand"].as_str(), Some("uid://b"));
        assert_eq!(without(held.get("bind"), "body").unwrap().get("hand").and_then(Value::as_str), Some("uid://b"));
        assert_eq!(without(without(held.get("bind"), "body").as_ref(), "hand"), None, "nothing left, no table written");
    }

    #[test]
    fn what_no_source_reads_is_told_apart() {
        let held: Table = "bind = { body = 'uid://a', gone = 'uid://b' }".parse().unwrap();
        assert_eq!(unread(held.get("bind"), &named(&["body"])), ["gone"]);
        assert_eq!(unread(None, &named(&["body"])), Vec::<String>::new());
    }

    #[test]
    fn what_is_labeled_goes_in_a_section_of_its_own() {
        let mut list = named(&["a", "b", "c", "d"]);
        list[1].insert("label".to_string(), "Face".into());
        list[3].insert("label".to_string(), "Face".into());
        let found = sections(&list);
        let names: Vec<(&str, Vec<&str>)> = found.iter().map(|(label, tables)| (*label, tables.iter().map(|table| text(table, "name")).collect())).collect();
        assert_eq!(names, [("", vec!["a", "c"]), ("Face", vec!["b", "d"])]);
    }

    #[test]
    fn an_option_is_held_to_its_type() {
        let option: Table = "type = 'enum'\nmembers = ['matte', 'gloss']".parse().unwrap();
        assert_eq!(wrong(&option, None).as_deref(), Some("not set"));
        assert_eq!(wrong(&option, Some(&"gloss".into())), None);
        assert_eq!(wrong(&option, Some(&"satin".into())).as_deref(), Some("not of its type"));
        let number: Table = "type = 'int'".parse().unwrap();
        assert_eq!(wrong(&number, Some(&Value::Integer(3))), None);
        assert_eq!(wrong(&number, Some(&"3".into())).as_deref(), Some("not of its type"));
    }

    #[test]
    fn a_default_is_no_value_missing_and_shows_as_written() {
        let option: Table = "type = 'int'\ndefault = '900'".parse().unwrap();
        assert_eq!(wrong(&option, None), None);
        assert_eq!(default_shown(&option).as_deref(), Some("900"));
        let named: Table = "type = 'string'\ndefault = '\"a\"'".parse().unwrap();
        assert_eq!(default_shown(&named).as_deref(), Some("a"));
        let flag: Table = "type = 'bool'\ndefault = '1'".parse().unwrap();
        assert_eq!(default_shown(&flag).as_deref(), Some("true"));
    }

    #[test]
    fn a_number_is_typed_either_way() {
        assert_eq!((typed::typed_number("31"), typed::typed_number(" 0x1F "), typed::typed_number("x")), (Some(31), Some(31), None));
    }
}
