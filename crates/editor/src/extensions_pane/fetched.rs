//! The fetched view: everything extensions fetch onto this computer, in one place. The project's extensions' packages
//! first, a card each, every package with how it stands - downloading, failed, turned off, fetched and how big and when
//! last used - and what can be done with it; then what else the store keeps (other projects' packages, an older
//! editor's, what fetches left unfinished); then cleaning it up: what is due now, or what this project does not use.
//! The store is the computer's, shared by every project (`setup::kept`): what goes is fetched again where one needs it.

use std::collections::{HashMap, HashSet};

use eframe::egui::{self, RichText};
use project::ext::setup::Setup;
use project::ext::setup::kept::{self, Kept};
use project::ext::{Origin, Status};

use super::{Cards, State, details::fetch_state_ui};
use crate::theme;
use crate::workspace::Workspace;

/// How long what the store holds is shown before it is read again.
const READ_AGAIN: std::time::Duration = std::time::Duration::from_secs(3);

/// The view.
pub(super) fn fetched_ui(ui: &mut egui::Ui, ws: &mut Workspace, state: &mut State, message: &mut Option<String>) {
    let Some(store) = extensions::setup_folder() else {
        ui.label(
            RichText::new("There is no data folder on this computer to keep what is fetched in.").color(theme::weak()),
        );
        return;
    };
    if state.kept.as_ref().is_none_or(|(read, _)| read.elapsed() >= READ_AGAIN) {
        state.kept = Some((std::time::Instant::now(), kept::list(&store)));
    }
    let held: Vec<Kept> = state.kept.as_ref().map_or_default(|(_, held)| held.clone());
    let by_key: HashMap<&str, &Kept> = held.iter().map(|k| (k.key.as_str(), k)).collect();
    // What the project's extensions name, every platform's: kept by a sweep, and not "elsewhere".
    let named = extensions::named_in_store(&ws.extensions);
    let weak = |text: String| RichText::new(text).small().color(theme::weak());
    let mut removed: Vec<String> = vec![];
    let elsewhere: Vec<&Kept> = held.iter().filter(|k| !named.contains(&k.key)).collect();
    clean_up_ui(ui, &store, &held, &named, elsewhere.len(), &mut removed, message);
    ui.add_space(4.0);
    // A card a package, its extension's name over it: on a wide pane they spread across, on a narrow one they stack.
    let mut cards = Cards::new(ui);
    let fetching: Vec<project::ext::Present> = ws
        .extensions
        .present
        .iter()
        .filter(|p| !matches!(p.status, Status::Replaced(_) | Status::Available) && p.setup.iter().any(Setup::here))
        .cloned()
        .collect();
    for p in &fetching {
        let runs = p.status == Status::Loaded && matches!(p.origin, Origin::Project(_));
        for setup in p.setup.iter().filter(|setup| setup.here()) {
            let held = by_key.get(setup.key().as_str()).copied();
            cards.card(ui, |ui| {
                theme::section(ui, &p.id, |ui| package_ui(ui, ws, p, setup, &store, runs, held, &mut removed, message));
            });
        }
    }
    for k in &elsewhere {
        cards.card(ui, |ui| {
            theme::section(ui, "Elsewhere", |ui| {
                removable_row(
                    ui,
                    &k.said(),
                    "Remove",
                    "From this computer: fetched again where a project needs it",
                    || {
                        removed.push(k.key.clone());
                    },
                )
                .on_hover_text(
                    k.name.as_ref().map_or_else(|| store.join(&k.key).display().to_string(), |(_, url)| url.clone()),
                );
                ui.label(weak(format!("{} · used {}", kept::size(k.bytes), kept::ago(k.used))));
            });
        });
    }
    cards.finish(ui);
    for key in removed {
        if let Err(e) = kept::remove(&store, &key) {
            *message = Some(format!("{e:#}"));
        }
        state.kept = None;
    }
}

/// One of an extension's packages: its name, how it stands and what can be done with it, and, fetched, how big it is
/// and when it was last used. A required one fetched can be fetched again - removed, and fetched whole once more.
#[expect(clippy::too_many_arguments, reason = "a row of the view, drawn where it is")]
fn package_ui(
    ui: &mut egui::Ui,
    ws: &mut Workspace,
    p: &project::ext::Present,
    setup: &Setup,
    store: &std::path::Path,
    runs: bool,
    held: Option<&Kept>,
    removed: &mut Vec<String>,
    message: &mut Option<String>,
) {
    let weak = |text: String| RichText::new(text).small().color(theme::weak());
    ui.horizontal_wrapped(|ui| {
        theme::short(ui, RichText::new(&setup.name).strong());
        if setup.lazy {
            ui.label(weak("optional".to_string()));
        }
    })
    .response
    .on_hover_text(
        [setup.license.clone(), setup.copyright.as_ref().map(|c| format!("© {c}"))]
            .into_iter()
            .flatten()
            .chain(setup.sources().map(str::to_string))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    ui.horizontal_wrapped(|ui| {
        fetch_state_ui(ui, ws, p, setup, store, runs, message);
        if let Some(held) = held {
            ui.label(weak(format!("· {} · used {}", kept::size(held.bytes), kept::ago(held.used))));
            if !setup.lazy
                && runs
                && ui
                    .small_button("Fetch again")
                    .on_hover_text("Removed and fetched once more, held to its hashes")
                    .clicked()
            {
                removed.push(held.key.clone());
            }
        }
    });
}

/// A row: a name, cut short where it does not fit, and a button on the right; the button's response.
fn removable_row(ui: &mut egui::Ui, name: &str, button: &str, says: &str, clicked: impl FnOnce()) -> egui::Response {
    let mut named = None;
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button(button).on_hover_text(says).clicked() {
                clicked();
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                named = Some(theme::short(ui, RichText::new(name).strong()));
            });
        });
    });
    named.unwrap_or_else(|| ui.label(""))
}

/// What the store holds, on a line over the cards: how much, how it is kept, and cleaning it up - what is due now
/// (unused for long and not this project's, and what fetches left unfinished), or everything this project does not use.
fn clean_up_ui(
    ui: &mut egui::Ui,
    store: &std::path::Path,
    held: &[Kept],
    named: &HashSet<String>,
    elsewhere: usize,
    removed: &mut Vec<String>,
    message: &mut Option<String>,
) {
    let total: u64 = held.iter().map(|k| k.bytes).sum();
    let days = kept::UNUSED_FOR.as_secs() / 86_400;
    ui.horizontal_wrapped(|ui| {
        let whose = match elsewhere {
            0 => "all of it this project's".to_string(),
            1 => "1 thing elsewhere".to_string(),
            n => format!("{n} things elsewhere"),
        };
        ui.label(format!("{} on this computer, for every project: {whose}.", kept::size(total)))
            .on_hover_text(format!("Kept in {}, each held to its hashes", store.display()));
        ui.label(
            RichText::new(format!(
                "What no project uses for {days} days goes by itself, and what fetches left unfinished."
            ))
            .small()
            .color(theme::weak()),
        );
        let says = "Remove now what is due: unused for long and not this project's, and what fetches left unfinished";
        if ui.small_button("Clean up now").on_hover_text(says).clicked() {
            let (gone, failed) = kept::sweep(store, named);
            let bytes: u64 = gone.iter().map(|k| k.bytes).sum();
            *message = Some(match (gone.len(), failed.first()) {
                (_, Some((k, why))) => format!("{}: {why}", k.said()),
                (0, None) => "Nothing was due: what is kept was used lately, or this project uses it".to_string(),
                (n, None) => format!("Removed {n} from this computer, {}", kept::size(bytes)),
            });
        }
        let unused: Vec<String> = held.iter().filter(|k| !named.contains(&k.key)).map(|k| k.key.clone()).collect();
        if !unused.is_empty()
            && ui
                .small_button("Remove what this project does not use")
                .on_hover_text("However lately it was used: another project fetches again what it needs")
                .clicked()
        {
            removed.extend(unused);
        }
    });
}
