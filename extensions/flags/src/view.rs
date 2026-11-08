//! A registry as a table: each flag's name, ID, kind and what its values or bits are, in ID order. A flag is added by
//! its name and ID and removed by its row's button; renaming one is removing it and adding it again under the new name,
//! its fields kept. Each is an edit of the registry's `flag` table, undone as any.

use std::fmt::Write as _;

use pe2_ext::{Cell, Icon, Style, Table, Ui, Value, View};

/// The table, with what is being typed into its add row.
#[derive(Default)]
pub struct Flags {
    name: String,
    id: String,
    renaming: Option<(String, String)>,
}

impl Flags {
    pub fn of_registry() -> Box<dyn View> {
        Box::<Flags>::default()
    }
}

/// A flag's values or bits, as one line: `name 1, other 2`, a bit as `name bit 0`.
fn values_line(flag: &Table) -> String {
    let mut out = String::new();
    for (key, prefix) in [("values", ""), ("bits", "bit ")] {
        for (name, value) in flag.get(key).and_then(Value::as_table).into_iter().flatten() {
            if !out.is_empty() {
                out.push_str(", ");
            }
            let _ = write!(out, "{name} {prefix}{}", value.as_integer().unwrap_or(0));
        }
    }
    out
}

/// An ID typed: hex with `0x`, else decimal.
fn parse_id(text: &str) -> Option<i64> {
    let text = text.trim();
    match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(hex) => i64::from_str_radix(hex, 16).ok(),
        None => text.parse().ok(),
    }
    .filter(|id| (0..crate::BANK).contains(id))
}

impl View for Flags {
    fn show(&mut self, ui: &mut Ui) {
        let Some(asset) = ui.asset() else { return };
        let flags: Table = asset.field("flag").and_then(Value::as_table).cloned().unwrap_or_default();
        let editable = ui.editable();
        let mut sorted: Vec<(&String, &Value)> = flags.iter().collect();
        sorted.sort_by_key(|(_, f)| f.get("id").and_then(Value::as_integer).unwrap_or(i64::MAX));
        let mut rows = vec![vec![
            Cell::Text("ID".into(), Style::Weak),
            Cell::Text("Flag".into(), Style::Weak),
            Cell::Text("Kind".into(), Style::Weak),
            Cell::Text("Values".into(), Style::Weak),
            Cell::Empty,
            Cell::Empty,
        ]];
        for (name, flag) in &sorted {
            let flag = flag.as_table().cloned().unwrap_or_default();
            let id = flag.get("id").and_then(Value::as_integer).unwrap_or(0);
            let count = flag.get("count").and_then(Value::as_integer).unwrap_or(1);
            let ids = if count > 1 { format!("{id:#05x}..{:#05x}", id + count - 1) } else { format!("{id:#05x}") };
            let kind = flag.get("kind").and_then(Value::as_str).unwrap_or("marker").to_string();
            let verified = flag.get("verified").and_then(Value::as_bool) == Some(true);
            rows.push(vec![
                Cell::Text(ids, Style::Note),
                Cell::Text((*name).clone(), if verified { Style::Strong } else { Style::Normal }),
                Cell::Text(kind, Style::Weak),
                Cell::Text(values_line(&flag), Style::Normal),
                Cell::Button(Icon::Text, format!("Rename {name}")),
                Cell::Button(Icon::Cross, format!("Remove {name}")),
            ]);
        }
        if let Some((row, column)) = ui.table("flags", &rows)
            && let Some((name, _)) = sorted.get(row.wrapping_sub(1))
        {
            if column == 5 {
                let mut changed = flags.clone();
                changed.remove(name.as_str());
                ui.set_step("flag", Some(&Value::Table(changed)));
            } else {
                self.renaming = Some(((*name).clone(), (*name).clone()));
            }
        }
        if !editable {
            return;
        }
        if let Some((from, mut to)) = self.renaming.take() {
            let mut keep = true;
            ui.horizontal(|ui| {
                ui.styled(&format!("Rename {from} to"), Style::Weak);
                let _ = ui.text_edit_hinted("rename", &mut to, "name", 200.0);
                let free = !to.is_empty() && !flags.contains_key(&to);
                if ui.button("Rename") && free {
                    let mut changed = Table::new();
                    for (name, flag) in &flags {
                        changed.insert(if *name == from { to.clone() } else { name.clone() }, flag.clone());
                    }
                    ui.set_step("flag", Some(&Value::Table(changed)));
                    keep = false;
                }
                if ui.button("Cancel") {
                    keep = false;
                }
            });
            if keep {
                self.renaming = Some((from, to));
            }
        }
        ui.horizontal(|ui| {
            let _ = ui.text_edit_hinted("new flag", &mut self.name, "new flag", 200.0);
            let _ = ui.text_edit_hinted("new id", &mut self.id, "ID (0x0A2)", 80.0);
            let id = parse_id(&self.id);
            let free = !self.name.is_empty() && !flags.contains_key(&self.name) && id.is_some();
            let added = ui.button("Add flag");
            if !self.id.trim().is_empty() && id.is_none() {
                ui.styled(&format!("IDs are 0x000 to {:#05x}", crate::BANK - 1), Style::Note);
            } else if flags.contains_key(&self.name) {
                ui.styled("a flag of that name is here", Style::Note);
            }
            if added && free {
                let mut flag = Table::new();
                flag.insert("id".into(), Value::Integer(id.unwrap_or(0)));
                flag.insert("kind".into(), Value::String("marker".into()));
                let mut changed = flags.clone();
                changed.insert(std::mem::take(&mut self.name), Value::Table(flag));
                self.id.clear();
                ui.set_step("flag", Some(&Value::Table(changed)));
            }
        });
    }
}
