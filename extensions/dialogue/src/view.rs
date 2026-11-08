//! A dialogue as a script: its commands in number order, each with the rule that picks a record (`counter` on its flag,
//! by the flag's name where a registry names it), its records as lines - the speaker and the text its string table
//! holds - or scene steps on one line each. The string table, the cast and the atlas page are a click away.

use std::fmt::Write as _;

use pe2_ext::{Asset, Cell, Icon, Style, Table, Ui, Value, View};

pub struct Script;

impl Script {
    pub fn of_dialogue() -> Box<dyn View> {
        Box::new(Script)
    }
}

fn or(value: String, default: &str) -> String {
    if value.is_empty() { default.to_string() } else { value }
}

fn text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Integer(n)) => n.to_string(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

/// A command's rule, as a line: its opcode, and what it reads.
fn rule(command: &Table, ui: &Ui) -> String {
    let opcode = text(command.get("opcode"));
    let flag = match command.get("game_flag") {
        Some(Value::Table(named)) => {
            let registry = named.get("flags").and_then(|r| ui.project().resolve(r)).map(|a| a.path().rsplit('/').next().unwrap_or_default().to_string());
            format!(" {} ({})", text(named.get("flag")), registry.unwrap_or_default())
        }
        Some(other) => format!(" flag {}", text(Some(other))),
        None => String::new(),
    };
    let mut out = opcode;
    out.push_str(&flag);
    for key in ["wraps", "persists", "branches"] {
        if command.get(key).and_then(Value::as_bool) == Some(true) {
            let _ = write!(out, " · {key}");
        }
    }
    for key in ["limit", "counter", "tally_first", "tally_count", "next"] {
        if let Some(value) = command.get(key) {
            let _ = write!(out, " · {key} {}", text(Some(value)));
        }
    }
    out
}

/// A scene step, as a line.
fn scene(record: &Table) -> String {
    let mut parts = vec![format!("view {}", or(text(record.get("view")), "0"))];
    for key in ["scene_sync", "wait_for_cue"] {
        if record.get(key).and_then(Value::as_bool) == Some(true) {
            parts.push(key.replace('_', " "));
        }
    }
    if record.get("send_message").and_then(Value::as_bool) == Some(true) {
        parts.push(format!(
            "message {} to {} after {}",
            text(record.get("message")),
            or(text(record.get("message_to")), "player"),
            or(text(record.get("message_delay")), "0")
        ));
    }
    if let Some(Value::Table(action)) = record.get("action") {
        parts.extend(action.iter().map(|(k, v)| format!("{} {}", k.replace('_', " "), text(Some(v)))));
    }
    parts.join(" · ")
}

fn speaker(record: &Table) -> String {
    match record.get("speaker") {
        Some(Value::String(name)) => name.rsplit_once('#').map_or(name.as_str(), |(plate, _)| plate).to_string(),
        Some(Value::Integer(n)) => format!("#{n}"),
        _ => String::new(),
    }
}

impl View for Script {
    fn show(&mut self, ui: &mut Ui) {
        let Some(asset) = ui.asset() else { return };
        let settings: Table = asset_fields(&asset);
        if let Some(why) = settings.get("failed").and_then(Value::as_str) {
            ui.error(&format!("Extraction could not read it exactly: {why}"));
            return;
        }
        let strings_ref = settings.get("strings").cloned();
        let strings: Table = strings_ref
            .as_ref()
            .and_then(|r| ui.project().resolve(r))
            .and_then(|table| table.field("strings").and_then(Value::as_table).cloned())
            .unwrap_or_default();
        let page = settings.get("page").cloned();
        let strings_path: Option<String> =
            strings_ref.as_ref().and_then(|r| ui.project().resolve(r)).map(|table| table.path().to_string());
        let unsettled: Vec<String> =
            settings.get("unsettled").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect();
        ui.horizontal(|ui| {
            if let Some(reference) = &strings_ref {
                ui.styled("Text", Style::Weak);
                ui.reference(reference);
            }
            if let Some(reference) = &page {
                ui.styled("Atlas", Style::Weak);
                ui.reference(reference);
            }
        });
        for note in &unsettled {
            ui.styled(note, Style::Note);
        }
        let commands = settings.get("command").and_then(Value::as_array).cloned().unwrap_or_default();
        if commands.is_empty() {
            ui.styled("No commands.", Style::Weak);
        }
        for command in commands.iter().filter_map(Value::as_table) {
            let number = text(command.get("number"));
            let rule = rule(command, ui);
            ui.horizontal(|ui| {
                ui.styled(&format!("Command {number}"), Style::Strong);
                ui.styled(&rule, Style::Weak);
            });
            ui.indented(|ui| {
                let records = command.get("record").and_then(Value::as_array).cloned().unwrap_or_default();
                let mut rows: Vec<Vec<Cell>> = Vec::new();
                let mut edit_keys: Vec<String> = Vec::new();
                for record in records.iter().filter_map(Value::as_table) {
                    let key = text(record.get("key"));
                    if record.get("type").and_then(Value::as_str) == Some("scene") {
                        edit_keys.push(String::new());
                        rows.push(vec![
                            Cell::Text(key, Style::Note),
                            Cell::Text("scene".into(), Style::Weak),
                            Cell::Text(scene(record), Style::Weak),
                            Cell::Empty,
                        ]);
                        continue;
                    }
                    let line = record.get("text").and_then(Value::as_str).unwrap_or_default();
                    let said = strings.get(line).and_then(Value::as_str).unwrap_or(if line.is_empty() { "" } else { "(not in the table)" });
                    // As the caption box shows it: its breaks kept, the one closing it left out.
                    let mut shown = said.trim_end_matches('\n').to_string();
                    if let Some(Value::Table(action)) = record.get("action") {
                        let acts: Vec<String> = action.iter().map(|(k, v)| format!("{} {}", k.replace('_', " "), text(Some(v)))).collect();
                        shown = format!("{shown} [{}]", acts.join(", "));
                    }
                    let blank = shown.is_empty();
                    edit_keys.push(line.to_string());
                    rows.push(vec![
                        Cell::Text(key, Style::Note),
                        Cell::Text(speaker(record), Style::Strong),
                        if blank { Cell::Text("(a blank line)".into(), Style::Weak) } else { Cell::Text(shown, Style::Normal) },
                        if line.is_empty() { Cell::Empty } else { Cell::Button(Icon::Text, "Edit its text".into()) },
                    ]);
                }
                // A line's text is its string table's: edited there, the line's key opened.
                if let Some((row, _)) = ui.table(&format!("records {number}"), &rows)
                    && let Some(key) = edit_keys.get(row).filter(|k| !k.is_empty())
                    && let Some(table) = &strings_path
                {
                    ui.navigate(&format!("{table}#{key}"));
                }
            });
        }
    }
}

/// What a dialogue is set to: its fields, where a generated file sets them.
fn asset_fields(asset: &Asset<'_>) -> Table {
    let mut out = Table::new();
    for key in ["strings", "page", "command", "unsettled", "failed", "atlas", "trailing_empty_text"] {
        if let Some(value) = asset.field(key) {
            out.insert(key.to_string(), value.clone());
        }
    }
    out
}
