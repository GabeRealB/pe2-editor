//! A package's slots as the panes show them: what it compiles and what it asks for, read from its `reflection`, so what is shown is what its
//! sources say now. A slot is selected as `slots/<name>`, in every pane at once.

use pe2_ext::{Cell, Style, Table, Ui, Value, View};

/// How many times a view is drawn before it asks again: its sources change outside any edit of the asset it shows.
const ASKED_AGAIN: u32 = 120;

/// The slots, as the head of the contents pane or a sheet in the inspector.
#[derive(Default)]
pub struct Slots {
    sheet: bool,
    told: Option<Result<Table, String>>,
    drawn: u32,
}

impl Slots {
    pub fn sheet() -> Slots {
        Slots { sheet: true, ..Slots::default() }
    }

    fn ask(&mut self, ui: &Ui) {
        self.drawn = (self.drawn + 1) % ASKED_AGAIN;
        if self.told.is_some() && self.drawn != 0 {
            return;
        }
        let Some(path) = ui.asset().map(|asset| asset.path().to_string()) else { return };
        let told = ui.project().table(&path, "reflection");
        self.told = told.map(|told| told.map_err(|e| format!("{e:#}")));
    }
}

fn names(table: &Table, key: &str) -> Vec<Table> {
    table.get(key).and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_table).cloned().collect()
}

fn text<'a>(table: &'a Table, key: &str) -> &'a str {
    table.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn is_public(slot: &Table) -> bool {
    slot.get("public").and_then(Value::as_bool).unwrap_or_default()
}

/// A slot's state: answered by a package of the graph, or open.
fn state(slot: &Table) -> &'static str {
    if text(slot, "package").is_empty() { "open" } else { "bound" }
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
        let slots = names(&reflected, "slots");
        if self.sheet {
            sheet(ui, &reflected, &slots);
        } else {
            tree(ui, &reflected, &slots);
        }
    }
}

fn tree(ui: &mut Ui, reflected: &Table, slots: &[Table]) {
    let sources = reflected.get("sources").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    if !sources.is_empty() {
        ui.styled("SOURCES", Style::Note);
        for source in sources.iter().filter_map(Value::as_str) {
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
            ui.tooltip(&format!("{}, {}", if is_public(slot) { "public" } else { "private" }, state(slot)));
        }
    });
}

fn sheet(ui: &mut Ui, reflected: &Table, slots: &[Table]) {
    ui.section(&format!("Slots  ·  {}", slots.len()), |ui| {
        let rows: Vec<Vec<Cell>> = slots
            .iter()
            .map(|slot| {
                vec![
                    Cell::Text(text(slot, "name").to_string(), Style::Strong),
                    Cell::Text(if is_public(slot) { "public" } else { "private" }.to_string(), Style::Weak),
                    Cell::Text(state(slot).to_string(), if state(slot) == "open" { Style::Error } else { Style::Weak }),
                ]
            })
            .collect();
        let _ = ui.table("slots", &rows);
    });
    let selected = ui.selected();
    let shown = selected.as_deref().and_then(|part| part.strip_prefix("slots/"));
    if let Some(slot) = shown.and_then(|name| slots.iter().find(|slot| text(slot, "name") == name)) {
        ui.section(text(slot, "name"), |ui| {
            ui.label(if text(slot, "package").is_empty() { "No package answers it" } else { text(slot, "package") });
            let uses = slot.get("uses").and_then(Value::as_integer).unwrap_or_default();
            ui.styled(&format!("{uses} includes"), Style::Note);
        });
    }
    let _ = reflected;
}
