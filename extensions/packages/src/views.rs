//! A package as the panes show it: what its sources ask for, read from its `reflection`, so it is what they say now, with what it is set to
//! as written, which an edit changes at once: its slots, each a chip taking a node of the slot's type, and its options, each a field of the
//! type its source defines, together as its sources declare them; and what it depends on. Args are the source's, and shown only. A slot is selected as `slots/<name>`, in
//! every pane at once.

use std::collections::{BTreeSet, HashMap};

use pe2_ext::{Cell, Icon, Style, Table, Ui, Value, View};

/// How rows line up: a character of a name, and the room after it.
const CHARACTER: f32 = 8.0;
const GAP: f32 = 16.0;

/// A package's slots and options, as the head of the contents pane or the inspector's sheet.
#[derive(Default)]
pub struct Slots {
    sheet: bool,
    told: Option<Result<Table, String>>,
    /// The project's revision it was told at: its sources change outside any edit of the asset it shows.
    revision: u64,
    /// What is being typed in an option.
    typed: HashMap<String, String>,
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

/// Why an option's value cannot be read; none where it can.
fn wrong(option: &Table, value: Option<&Value>) -> Option<&'static str> {
    let Some(value) = value else { return Some("not set") };
    let right = match text(option, "type") {
        "int" => value.is_integer(),
        "bool" => value.is_bool(),
        "string" => value.is_str(),
        _ => value.as_str().is_some_and(|member| words(option, "members").iter().any(|known| known == member)),
    };
    (!right).then_some("not of its type")
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

/// A number as typed.
fn number(typed: &str) -> Option<i64> {
    let typed = typed.trim();
    typed.strip_prefix("0x").map_or_else(|| typed.parse().ok(), |hex| i64::from_str_radix(hex, 16).ok())
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
                            if let Some(new) = self.editor(ui, input, value) {
                                asked = Some(("option", input_name.to_string(), new));
                            }
                            // Set, it can go back to having no value, which no value of its type says.
                            if value.is_some() && ui.icon_button(Icon::Cross, "Leave it not set") {
                                cleared = Some(input_name.to_string());
                            }
                            // What is read needs a value, if it is live; what is not read cannot need one.
                            let read = input.get("read").and_then(Value::as_bool).unwrap_or(false);
                            if let Some(why) = wrong(input, value) {
                                ui.styled(why, if read || value.is_some() { Style::Error } else { Style::Note });
                            }
                        }
                    });
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

    fn editor(&mut self, ui: &mut Ui, option: &Table, value: Option<&Value>) -> Option<Value> {
        let name = text(option, "name");
        match text(option, "type") {
            "bool" => {
                // A box says true or false, and this is neither yet: both are offered, as false would take two clicks.
                let Some(mut on) = value.and_then(Value::as_bool) else {
                    let choices = ["false".to_string(), "true".to_string()];
                    return ui.combo(&format!("option.{name}"), "", &choices).map(|chosen| Value::Boolean(chosen == 1));
                };
                ui.checkbox("", &mut on).then_some(Value::Boolean(on))
            }
            "enum" => {
                let members = words(option, "members");
                let shown = value.and_then(Value::as_str).unwrap_or_default();
                ui.combo(&format!("option.{name}"), shown, &members).map(|chosen| Value::String(members[chosen].clone()))
            }
            ty => {
                let held = value.map_or_else(String::new, |value| value.as_str().map_or_else(|| value.to_string(), str::to_string));
                let typed = self.typed.entry(name.to_string()).or_insert(held);
                if !ui.text_edit(&format!("option.{name}"), typed) {
                    return None;
                }
                if ty == "int" { number(typed).map(Value::Integer) } else { Some(Value::String(typed.clone())) }
            }
        }
    }
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
        assert_eq!(wrong(&option, None), Some("not set"));
        assert_eq!(wrong(&option, Some(&"gloss".into())), None);
        assert_eq!(wrong(&option, Some(&"satin".into())), Some("not of its type"));
        let number: Table = "type = 'int'".parse().unwrap();
        assert_eq!(wrong(&number, Some(&Value::Integer(3))), None);
        assert_eq!(wrong(&number, Some(&"3".into())), Some("not of its type"));
    }

    #[test]
    fn a_number_is_typed_either_way() {
        assert_eq!((number("31"), number(" 0x1F "), number("x")), (Some(31), Some(31), None));
    }
}
