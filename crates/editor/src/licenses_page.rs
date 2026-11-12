//! The Settings pane's Licenses page: what the editor ships, by its license, as this program was built
//! (`licenses::listing`) - the crates its programs link, to filter and choose among, the license texts of whatever is
//! chosen, what is bundled that is no crate, and what is used where the system has it. `pe2-cli licenses` says the
//! same.

use eframe::egui::{self, RichText};
use egui_extras::{Column, TableBuilder};
use licenses::{Listing, Text};

use crate::theme;

/// What the texts shown are of.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Chosen {
    /// The editor itself.
    #[default]
    Own,
    Crate(usize),
    Bundled(usize),
}

/// What the page keeps between frames.
#[derive(Clone, Default)]
struct State {
    filter: String,
    /// The program or extension whose crates are listed; none, all of them.
    within: Option<String>,
    chosen: Chosen,
    /// Which of the chosen's texts is shown.
    text: usize,
}

/// A cell's text: clicks on it are the row's.
fn cell(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>) {
    ui.add(egui::Label::new(text).selectable(false).truncate());
}

pub fn ui(ui: &mut egui::Ui) {
    let listing = licenses::listing();
    let id = egui::Id::unique("licenses page");
    let mut state: State = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    let own = &listing.own;
    ui.label(
        RichText::new(format!(
            "{} {} is under {}. What it is built with, as this program was built for {}:",
            own.name, own.version, own.license, listing.target
        ))
        .color(theme::weak()),
    );
    linked_ui(ui, listing, &mut state);
    texts_ui(ui, listing, &mut state);
    by_hand_ui(ui, listing, &mut state);
    ui.add_space(6.0);
    let fetched = "What an extension fetches onto this computer is no part of the editor either: it is listed with \
                   the extension, in the Extensions pane.";
    ui.label(RichText::new(fetched).small().color(theme::weak()));
    ui.data_mut(|d| d.insert_temp(id, state));
}

/// The crates linked: those of a program, those a filter finds, one of them chosen by its row.
fn linked_ui(ui: &mut egui::Ui, listing: &Listing, state: &mut State) {
    let needle = state.filter.trim().to_lowercase();
    let shown: Vec<usize> = (0..listing.crates.len())
        .filter(|at| {
            let c = &listing.crates[*at];
            state.within.as_ref().is_none_or(|within| c.within.contains(within))
                && (needle.is_empty()
                    || c.name.to_lowercase().contains(&needle)
                    || c.license.to_lowercase().contains(&needle))
        })
        .collect();
    let of = if shown.len() == listing.crates.len() {
        shown.len().to_string()
    } else {
        format!("{} of {}", shown.len(), listing.crates.len())
    };
    theme::section(ui, &format!("Linked into its programs ({of})"), |ui| {
        ui.horizontal(|ui| {
            let filter = egui::TextEdit::singleline(&mut state.filter).hint_text("Filter by name or license");
            ui.add(filter.desired_width(220.0));
            ui.label("in");
            let mut programs: Vec<&String> = listing.crates.iter().flat_map(|c| &c.within).collect();
            programs.sort();
            programs.dedup();
            let all = "every program";
            let combo = egui::ComboBox::from_id_salt("linked into").selected_text(state.within.as_deref().unwrap_or(all));
            combo.show_ui(ui, |ui| {
                if ui.selectable_label(state.within.is_none(), all).clicked() {
                    state.within = None;
                }
                for program in programs {
                    if ui.selectable_label(state.within.as_ref() == Some(program), program).clicked() {
                        state.within = Some(program.clone());
                    }
                }
            });
        });
        ui.add_space(4.0);
        let height = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
        let heading = |text: &str| RichText::new(text).small().strong().color(theme::weak());
        let table = TableBuilder::new(ui)
            .id_salt("linked crates")
            .striped(true)
            .sense(egui::Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::initial(200.0).at_least(80.0).clip(true).resizable(true))
            .column(Column::initial(110.0).at_least(50.0).clip(true).resizable(true))
            .column(Column::initial(260.0).at_least(80.0).clip(true).resizable(true))
            .column(Column::remainder().at_least(80.0).clip(true))
            .max_scroll_height(240.0)
            .auto_shrink([false, true]);
        let table = table.header(height, |mut header| {
            for title in ["CRATE", "VERSION", "LICENSE", "IN"] {
                header.col(|ui| cell(ui, heading(title)));
            }
        });
        table.body(|body| {
            body.rows(height, shown.len(), |mut row| {
                let at = shown[row.index()];
                let c = &listing.crates[at];
                let chosen = state.chosen == Chosen::Crate(at);
                row.set_selected(chosen);
                // The chosen row's text is read against the selection's fill: all of it strong.
                let ink = |ui: &egui::Ui, weak: bool| match (chosen, weak) {
                    (true, _) => ui.visuals().strong_text_color(),
                    (false, true) => theme::weak(),
                    (false, false) => ui.visuals().text_color(),
                };
                row.col(|ui| cell(ui, RichText::new(&c.name).color(ink(ui, false))));
                row.col(|ui| cell(ui, RichText::new(&c.version).color(ink(ui, true))));
                row.col(|ui| cell(ui, RichText::new(&c.license).color(ink(ui, false))));
                row.col(|ui| cell(ui, RichText::new(c.within.join(", ")).color(ink(ui, true))));
                if row.response().clicked() {
                    state.chosen = Chosen::Crate(at);
                    state.text = 0;
                }
            });
        });
    });
}

/// The license texts of what is chosen - the editor's own until something is - one shown at a time.
fn texts_ui(ui: &mut egui::Ui, listing: &Listing, state: &mut State) {
    let (name, texts, repository): (String, &[Text], Option<&String>) = match state.chosen {
        Chosen::Crate(at) if at < listing.crates.len() => {
            let c = &listing.crates[at];
            (format!("{} {}", c.name, c.version), &c.texts, c.repository.as_ref())
        }
        Chosen::Bundled(at) if at < listing.bundled.len() => {
            let item = &listing.bundled[at];
            (item.name.clone(), &item.texts, item.from.as_ref())
        }
        _ => (listing.own.name.clone(), &listing.own.texts, None),
    };
    theme::section(ui, &format!("The license of {name}"), |ui| {
        state.text = state.text.min(texts.len().saturating_sub(1));
        if let Some(repository) = repository {
            ui.hyperlink(repository);
        }
        if texts.len() > 1 {
            ui.horizontal_wrapped(|ui| {
                for (at, text) in texts.iter().enumerate() {
                    if ui.selectable_label(state.text == at, &text.file).clicked() {
                        state.text = at;
                    }
                }
            });
            ui.separator();
        }
        if let Some(text) = texts.get(state.text) {
            // Another text begins at its top.
            let shown = egui::ScrollArea::both().id_salt(("license text", text.text)).max_height(300.0);
            shown.auto_shrink([false, true]).show(ui, |ui| {
                let text = RichText::new(listing.text(text)).small().monospace();
                ui.add(egui::Label::new(text).wrap_mode(egui::TextWrapMode::Extend));
            });
        }
    });
}

/// What is bundled that is no crate, each a link to its license texts, and what is used where the system has it.
fn by_hand_ui(ui: &mut egui::Ui, listing: &Listing, state: &mut State) {
    if !listing.bundled.is_empty() {
        theme::section(ui, "Bundled with them, and no crate", |ui| {
            for (at, item) in listing.bundled.iter().enumerate() {
                let version = item.version.as_ref().map_or_else(String::new, |v| format!(" {v}"));
                ui.horizontal(|ui| {
                    // A link: its texts are shown above, where a crate's are.
                    if ui.link(format!("{}{version}", item.name)).on_hover_text("Show its license").clicked() {
                        state.chosen = Chosen::Bundled(at);
                        state.text = 0;
                    }
                    ui.label(&item.license);
                    ui.label(RichText::new(format!("in {}", item.within.join(", "))).color(theme::weak()));
                });
                let rights = [Some(item.what.clone()), item.copyright.as_ref().map(|c| format!("(c) {c}"))];
                let rights: Vec<String> = rights.into_iter().flatten().collect();
                ui.add(egui::Label::new(RichText::new(rights.join("; ")).small().color(theme::weak())).wrap());
            }
        });
    }
    if !listing.installed.is_empty() {
        theme::section(ui, "No part of the editor: used where the system has it", |ui| {
            for item in &listing.installed {
                ui.horizontal(|ui| {
                    ui.label(&item.name);
                    ui.label(RichText::new(&item.license).color(theme::weak()));
                });
                ui.add(egui::Label::new(RichText::new(&item.what).small().color(theme::weak())).wrap());
            }
        });
    }
}
