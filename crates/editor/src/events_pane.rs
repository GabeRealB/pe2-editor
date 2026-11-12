//! The Events pane: every event there is, the project's, its extensions' and the built-in ones, how much is wired to each, and a button to signal it.

use eframe::egui::{self, RichText};
use pe2_ext::events::BUILT_IN;

use crate::workspace::Workspace;
use crate::{icons, theme};

/// A row of the pane.
struct Row {
    id: String,
    label: String,
    /// Where it is an asset, its path.
    asset: Option<String>,
}

fn rows(ui: &mut egui::Ui, id: &str, shown: &[Row], ws: &mut Workspace, nav: &mut Option<String>) {
    let mut signaled = None;
    let wired = ws.last_epoch.as_ref().map(|epoch| &epoch.wired);
    let fired = ws.last_epoch.as_ref().map(|epoch| &epoch.report.signaled);
    egui::Grid::new(id).num_columns(3).spacing([12.0, 6.0]).show(ui, |ui| {
        for row in shown {
            let tint =
                if fired.is_some_and(|f| f.contains(&row.id)) { theme::accent() } else { ui.visuals().text_color() };
            let name = RichText::new(&row.label).color(tint);
            match &row.asset {
                Some(path) => {
                    if ui.link(name).clicked() {
                        *nav = Some(path.clone());
                    }
                }
                None => {
                    ui.label(name).on_hover_text(&row.id);
                }
            }
            let count = wired.and_then(|w| w.get(&row.id)).copied().unwrap_or_default();
            let said = if count == 0 { "nothing wired".to_string() } else { format!("{count} wired") };
            ui.label(RichText::new(said).small().color(theme::weak()));
            if ui.small_button(format!("{}  Signal", icons::PLAY)).clicked() {
                signaled = Some(row.id.clone());
            }
            ui.end_row();
        }
    });
    if let Some(id) = signaled {
        ws.signal(&id);
    }
}

pub fn events_ui(ui: &mut egui::Ui, ws: &mut Workspace, nav: &mut Option<String>, message: &mut Option<String>) {
    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        let own: Vec<Row> = ws
            .extensions
            .find(&ws.index, "event")
            .filter_map(|asset| {
                let uid = asset.uid.clone()?;
                let name = asset.path.rsplit('/').next().unwrap_or(&asset.path);
                let label = asset.fields_toml().get("label").and_then(toml::Value::as_str).unwrap_or(name).to_string();
                Some(Row { id: uid, label, asset: Some(asset.path.clone()) })
            })
            .collect();
        theme::section(ui, &format!("Project  ·  {}", own.len()), |ui| {
            if !own.is_empty() {
                rows(ui, "own events", &own, ws, nav);
            }
            ui.horizontal(|ui| {
                let entered = ui.add(
                    egui::TextEdit::singleline(&mut ws.new_event_name).hint_text("A new event").desired_width(180.0),
                );
                let submitted = entered.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let name = ws.new_event_name.trim().to_string();
                if (ui.add_enabled(!name.is_empty(), egui::Button::new("Add")).clicked() || submitted)
                    && !name.is_empty()
                {
                    let failed = |editor: &mut crate::app::Editor, made: Result<(), String>| {
                        if let Err(why) = made {
                            editor.say(why);
                        }
                    };
                    match ws.new_event(&name, failed) {
                        Ok(()) => ws.new_event_name.clear(),
                        Err(why) => *message = Some(why),
                    }
                }
            });
        });
        let theirs: Vec<Row> = ws
            .extensions
            .events
            .iter()
            .map(|(id, label)| Row { id: id.clone(), label: label.clone(), asset: None })
            .collect();
        if !theirs.is_empty() {
            theme::section(ui, "Extensions", |ui| rows(ui, "their events", &theirs, ws, nav));
        }
        let built_in: Vec<Row> = BUILT_IN
            .iter()
            .map(|(id, label)| Row { id: (*id).to_string(), label: (*label).to_string(), asset: None })
            .collect();
        theme::section(ui, "Built in", |ui| rows(ui, "built-in events", &built_in, ws, nav));
    });
}
