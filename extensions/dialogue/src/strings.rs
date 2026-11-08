//! A string table as what it holds: its keys and their text, found by a filter over both. A text is edited in place, in
//! the dialogues' markup (`markup.rs`), as an edit of the table's `strings`; one is opened by its row's button, or by
//! selecting its key (a dialogue's script does, to edit a line).

use pe2_ext::{Cell, Icon, Style, Table, Ui, Value, View};

/// Rows past this many are not shown: the filter finds them.
const SHOWN: usize = 400;

#[derive(Default)]
pub struct Strings {
    filter: String,
    editing: Option<(String, String)>,
}

impl Strings {
    pub fn of_table() -> Box<dyn View> {
        Box::<Strings>::default()
    }
}

impl View for Strings {
    fn show(&mut self, ui: &mut Ui) {
        let Some(asset) = ui.asset() else { return };
        let strings: Table = asset.field("strings").and_then(Value::as_table).cloned().unwrap_or_default();
        // A key selected - by a dialogue's script, or here - is opened for editing.
        if let Some(key) = ui.selected()
            && self.editing.as_ref().is_none_or(|(open, _)| *open != key)
            && let Some(text) = strings.get(&key).and_then(Value::as_str)
        {
            self.editing = Some((key, text.to_string()));
        }
        if let Some((key, mut text)) = self.editing.take() {
            let mut open = true;
            ui.section(&key, |ui| {
                let _ = ui.text_area("text", &mut text, 4, 0.0);
                ui.horizontal(|ui| {
                    let unchanged = strings.get(&key).and_then(Value::as_str) == Some(text.as_str());
                    if ui.button("Apply") && !unchanged {
                        let mut changed = strings.clone();
                        changed.insert(key.clone(), Value::String(text.clone()));
                        ui.set_step("strings", Some(&Value::Table(changed)));
                    }
                    if ui.button("Close") {
                        open = false;
                    }
                    if let Err(why) = crate::markup::parse(&text) {
                        ui.error(&why);
                    }
                });
            });
            if open {
                self.editing = Some((key, text));
            }
        }
        ui.horizontal(|ui| {
            let _ = ui.text_edit_hinted("filter", &mut self.filter, "filter keys and text", 300.0);
            ui.styled(&format!("{} strings", strings.len()), Style::Note);
        });
        let needle = self.filter.to_lowercase();
        let found: Vec<(&String, &str)> = strings
            .iter()
            .filter_map(|(k, v)| Some((k, v.as_str()?)))
            .filter(|(k, v)| needle.is_empty() || k.to_lowercase().contains(&needle) || v.to_lowercase().contains(&needle))
            .collect();
        let mut rows = Vec::new();
        for (key, text) in found.iter().take(SHOWN) {
            let first = text.trim_end_matches('\n');
            let shown = if first.is_empty() { "(a blank line)".to_string() } else { first.to_string() };
            rows.push(vec![
                Cell::Text((*key).clone(), Style::Note),
                Cell::Text(shown, if first.is_empty() { Style::Weak } else { Style::Normal }),
                Cell::Button(Icon::Text, format!("Edit {key}")),
            ]);
        }
        if let Some((row, _)) = ui.table("strings", &rows)
            && let Some((key, text)) = found.get(row)
        {
            self.editing = Some(((*key).clone(), (*text).to_string()));
        }
        if found.len() > SHOWN {
            ui.styled(&format!("{} more: filter to find them", found.len() - SHOWN), Style::Note);
        }
    }
}
