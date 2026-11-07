//! The details of the extension selected, as cards laid out in as many columns as fit (`Cards`): what needs the user
//! first - running outside the sandbox, its switch for the project, what it fetches - then who it is, its settings,
//! what it depends on and what depends on it, its documents and what it registers.

use eframe::egui::{self, RichText};
use pe2_ext::{Scope, Setting};
use project::ext::{Dependency, Origin, Present, Status};
use std::collections::BTreeSet;

use super::{
    Cards, State, UserSettings, View, asked_for, chains, left_off_too, origin_text, requirement, status_color,
    status_text,
};
use crate::workspace::Workspace;
use crate::{icons, theme};

/// The extension selected: who it is, then its cards.
pub(super) fn details_ui(
    ui: &mut egui::Ui,
    ws: &mut Workspace,
    all: &[Present],
    selected: &str,
    state: &mut State,
    user: &mut UserSettings,
    message: &mut Option<String>,
) {
    // Those in effect: one of each id.
    let present: Vec<Present> = all.iter().filter(|p| !matches!(p.status, Status::Replaced(_))).cloned().collect();
    let present = present.as_slice();
    let Some(p) = present.iter().find(|p| p.id == selected) else { return };
    header_ui(ui, p);
    let mut cards = Cards::new(ui);
    if p.trust.is_some() {
        cards.card(ui, |ui| theme::section(ui, "Outside the sandbox", |ui| unsandboxed_ui(ui, ws, p, message)));
    }
    if switch_shown(ws, present, p) {
        cards.card(ui, |ui| theme::section(ui, "In this project", |ui| switch_ui(ui, ws, present, p, state, message)));
    }
    cards.card(ui, |ui| fetches_ui(ui, ws, p, state, message));
    cards.card(ui, |ui| about_ui(ui, ws, all, p));
    cards.card(ui, |ui| settings_ui(ui, ws, p, user, message));
    cards.card(ui, |ui| requires_ui(ui, p, present, state));
    cards.card(ui, |ui| required_by_ui(ui, p, present, state));
    cards.card(ui, |ui| documents_ui(ui, p, message));
    cards.card(ui, |ui| registers_ui(ui, p));
    cards.finish(ui);
}

/// Who it is, above its cards: its name, version and what became of it, what it says it is, and why it is not loaded.
fn header_ui(ui: &mut egui::Ui, p: &Present) {
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(&p.id).heading());
        if !p.version.is_empty() {
            ui.label(RichText::new(&p.version).color(theme::weak()));
        }
        ui.label(RichText::new(status_text(&p.status)).color(status_color(ui, Some(&p.status))));
    });
    if let Some(description) = &p.described.description {
        ui.label(description);
    }
    if let Status::Refused(why) = &p.status {
        ui.label(RichText::new(why).small().color(ui.visuals().error_fg_color));
    }
    ui.add_space(2.0);
}

/// Whether the project has anything to do with it: add, update or remove it, or turn it on or off - or say why not.
fn switch_shown(ws: &Workspace, present: &[Present], p: &Present) -> bool {
    let kept = present.iter().any(|q| q.id == p.id && q.status == Status::Available && q.origin.folder().is_some());
    if kept || matches!(p.origin, Origin::Project(_)) {
        return true;
    }
    if matches!(p.status, Status::NotEnabled { .. }) {
        return false;
    }
    let off = ws.project.disabled_extensions.contains(&p.id);
    if !off && matches!(p.status, Status::Refused(_)) {
        return p
            .requires
            .iter()
            .any(|r| present.iter().find(|q| q.id == requirement(r).0).is_none_or(|q| q.status != Status::Loaded));
    }
    true
}

/// Who it is: its version, where it is from and its files, its license, what it was built against, and what it costs.
fn about_ui(ui: &mut egui::Ui, ws: &Workspace, all: &[Present], p: &Present) {
    theme::section(ui, "About", |ui| {
        egui::Grid::new(("extension about", &p.id)).num_columns(2).spacing([14.0, 4.0]).show(ui, |ui| {
            let mut row = |label: &str, value: String| {
                ui.label(RichText::new(label).color(theme::weak()));
                theme::short(ui, value.as_str()).on_hover_text(value);
                ui.end_row();
            };
            // One that cannot be read says nothing of who it is.
            if !p.version.is_empty() {
                row("Version", p.version.clone());
            }
            row("From", origin_text(&p.origin));
            if p.trust.is_some() {
                row("Sandbox", "none".to_string());
            }
            match &p.origin {
                Origin::Project(path) | Origin::User(path) | Origin::Install(path) => {
                    row(if path.is_dir() { "Folder" } else { "Archive" }, path.display().to_string());
                }
                Origin::Component(path) => row("File", path.display().to_string()),
                Origin::Builtin | Origin::Linked => {}
            }
            if let Some(license) = &p.described.license {
                row("License", license.clone());
            }
            if let Some(repository) = &p.described.repository {
                row("Repository", repository.clone());
            }
            for q in all.iter().filter(|q| q.id == p.id && matches!(q.status, Status::Replaced(_))) {
                row("In place of", format!("{} {}", q.version, origin_text(&q.origin)));
            }
            // What it was built against; this editor's, where that differs (a version of the same line).
            let ours = project::ext::component::CONTRACT;
            let contract = match (&p.origin, &p.contract) {
                (Origin::Linked, _) => "none: linked in".to_string(),
                (_, Some(c)) if c == ours => format!("pe2:extension@{c}"),
                (_, Some(c)) => format!("pe2:extension@{c}, run by {ours}"),
                (_, None) => String::new(),
            };
            if !contract.is_empty() {
                row("Contract", contract);
            }
            if let Some(size) = p.size {
                row("Size", format!("{} KB", size.div_ceil(1024)));
            }
            if let Some(load) = p.load {
                row("Loaded in", format!("{:.1} ms", load.as_secs_f64() * 1000.0));
            }
            if let Some((_, m)) =
                ws.extensions.memory().into_iter().find(|(id, _)| *id == p.id).filter(|(_, m)| m.peak > 0)
            {
                let mib = |bytes: usize| bytes.div_ceil(1 << 20);
                let now = match m.instances {
                    0 => "none held now".to_string(),
                    n => format!("{} MiB in {n} instance{}", mib(m.bytes), if n == 1 { "" } else { "s" }),
                };
                row("Memory", now);
                row("Most held", format!("{} MiB", mib(m.peak)));
            }
        });
    });
}

/// What it registers, by kind, each kind opening to its names.
fn registers_ui(ui: &mut egui::Ui, p: &Present) {
    theme::section(ui, "Registers", |ui| {
        if p.registers.iter().all(|(_, names)| names.is_empty()) {
            let note = if p.status == Status::Loaded { "Nothing" } else { "Nothing: it is not loaded" };
            ui.label(RichText::new(note).color(theme::weak()));
        }
        for (what, names) in p.registers.iter().filter(|(_, names)| !names.is_empty()) {
            egui::CollapsingHeader::new(format!("{what}  ·  {}", names.len()))
                .id_salt(("registers", &p.id, *what))
                .show(ui, |ui| {
                    for name in names {
                        ui.label(
                            RichText::new(name.split_once('@').map_or(name.as_str(), |(_, local)| local)).monospace(),
                        );
                    }
                });
        }
    });
}

/// What it depends on, each a link to it, with the version wanted and the one found.
fn requires_ui(ui: &mut egui::Ui, p: &Present, present: &[Present], state: &mut State) {
    theme::section(ui, "Requires", |ui| {
        if p.dependencies().next().is_none() {
            ui.label(RichText::new("Nothing").color(theme::weak()));
        }
        for (text, dependency) in p.dependencies() {
            let id = requirement(text).0;
            let found = present.iter().find(|q| q.id == id);
            let (asked, service) = asked_for(state, text);
            let service = service.map(str::to_string);
            ui.horizontal_wrapped(|ui| {
                if ui.link(id).clicked() && found.is_some() {
                    state.selected = Some(id.to_string());
                }
                let have = found.map_or_else(
                    || "not installed".to_string(),
                    |q| format!("{} ({})", service.as_deref().unwrap_or(&q.version), status_text(&q.status)),
                );
                let how = match dependency {
                    Dependency::Required => "",
                    Dependency::Weak => "if it can, and goes with it: ",
                    Dependency::Optional => "if it can, ",
                };
                ui.label(RichText::new(format!("{how}wants {asked}, has {have}")).small().color(theme::weak()));
            });
        }
    });
}

/// What depends on it, each a link to it.
fn required_by_ui(ui: &mut egui::Ui, p: &Present, present: &[Present], state: &mut State) {
    theme::section(ui, "Required by", |ui| {
        let by: Vec<&Present> =
            present.iter().filter(|q| q.dependencies().any(|(r, _)| requirement(r).0 == p.id)).collect();
        if by.is_empty() {
            ui.label(RichText::new("Nothing").color(theme::weak()));
        }
        for q in by {
            if ui.link(&q.id).clicked() {
                state.selected = Some(q.id.clone());
            }
        }
    });
}

/// Its settings, each edited the way its default's type is, where it is kept said: the project's written to
/// `project.toml` as a step of the history, the user's to the editor's settings. Either way it is loaded again.
fn settings_ui(
    ui: &mut egui::Ui,
    ws: &mut Workspace,
    p: &Present,
    user: &mut UserSettings,
    message: &mut Option<String>,
) {
    let declared = ws.extensions.settings.get(&p.id).cloned().unwrap_or_default();
    if declared.is_empty() {
        return;
    }
    theme::section(ui, "Settings", |ui| {
        egui::Grid::new(("extension settings", &p.id)).num_columns(3).min_col_width(0.0).spacing([12.0, 6.0]).show(
            ui,
            |ui| {
                for (setting, value) in &declared {
                    let label = setting.label.clone().unwrap_or_else(|| setting.key.clone());
                    let response = ui.label(&label);
                    if let Some(help) = &setting.help {
                        response.on_hover_text(help);
                    }
                    let mut chosen = value.clone();
                    setting_input(ui, (&p.id, &setting.key), &label, setting, &mut chosen);
                    let kept = match setting.scope {
                        Scope::Project => {
                            ws.project.extension_settings.get(&p.id).and_then(|t| t.get(&setting.key)).cloned()
                        }
                        Scope::User => user.get(&p.id).and_then(|t| t.get(&setting.key)).cloned(),
                    };
                    let mut next = (chosen != *value).then_some(Some(chosen));
                    ui.horizontal(|ui| {
                        let (scope, says) = match setting.scope {
                            Scope::Project => ("project", "Kept in project.toml: the same for everyone on the project"),
                            Scope::User => ("yours", "Kept in your editor settings: the same in every project"),
                        };
                        ui.label(RichText::new(scope).small().color(theme::weak())).on_hover_text(says);
                        if kept.is_some() {
                            let back = format!("Back to {}", setting.default);
                            let response = ui.small_button(icons::ARROW_ARC_LEFT);
                            response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &back));
                            if response.on_hover_text(&back).clicked() {
                                next = Some(None);
                            }
                        }
                    });
                    ui.end_row();
                    // A value its default's is not kept: going back to it removes the setting.
                    let Some(next) = next.map(|n| n.filter(|v| *v != setting.default)) else { continue };
                    let said = next.as_ref().map_or_else(|| "its default".to_string(), ToString::to_string);
                    match setting.scope {
                        Scope::Project => {
                            let written = project::switches::set_setting(
                                &ws.project,
                                &p.id,
                                &setting.key,
                                next.as_ref(),
                                &mut ws.history,
                                format!("Set {label} to {said}"),
                            );
                            if let Err(e) = written {
                                *message = Some(format!("project.toml: {e:#}"));
                            }
                        }
                        Scope::User => {
                            let table = user.entry(p.id.clone()).or_default();
                            match next {
                                Some(v) => {
                                    table.insert(setting.key.clone(), v);
                                }
                                None => {
                                    table.remove(&setting.key);
                                }
                            }
                            if table.is_empty() {
                                user.remove(&p.id);
                            }
                        }
                    }
                }
            },
        );
    });
}

/// A setting's input, by its default's type: a checkbox, a number, a text, or a list of its choices.
fn setting_input(ui: &mut egui::Ui, id: (&str, &str), label: &str, setting: &Setting, value: &mut toml::Value) {
    use toml::Value;
    if !setting.choices.is_empty() {
        let shown =
            setting.choices.iter().find(|(v, _)| v == value).map_or_else(|| value.to_string(), |(_, l)| l.clone());
        egui::ComboBox::from_id_salt(("setting", id)).selected_text(shown).show_ui(ui, |ui| {
            for (v, l) in &setting.choices {
                ui.selectable_value(value, v.clone(), l);
            }
        });
        return;
    }
    match value {
        Value::Boolean(b) => {
            // Its label is the row's: the checkbox is named by it for the window tools.
            let response = ui.checkbox(b, "");
            let on = *b;
            response.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, true, on, label));
        }
        Value::Integer(i) => {
            ui.add(egui::DragValue::new(i));
        }
        Value::Float(f) => {
            ui.add(egui::DragValue::new(f).speed(0.1));
        }
        Value::String(text) => {
            // Taken when the edit is done, not at each key.
            let key = ui.scope_id().with(("setting text", id));
            let mut draft = ui.data(|d| d.get_temp::<String>(key)).unwrap_or_else(|| text.clone());
            let response = ui.add(egui::TextEdit::singleline(&mut draft).desired_width(160.0));
            if response.lost_focus() {
                *text = draft;
                ui.data_mut(|d| d.remove::<String>(key));
            } else if response.has_focus() {
                ui.data_mut(|d| d.insert_temp(key, draft));
            }
        }
        other => {
            ui.label(RichText::new(other.to_string()).monospace().color(theme::weak()));
        }
    }
}

/// For an unsandboxed extension: enabling it, which is the user's and no project's. Until then nothing of it runs.
fn unsandboxed_ui(ui: &mut egui::Ui, ws: &Workspace, p: &Present, message: &mut Option<String>) {
    let Some(trust) = &p.trust else { return };
    // Where it is says it may run: there is nothing of its own to switch.
    if let Some(allowed) = extensions::trust::allowed_by(&p.origin) {
        ui.label(allowed.said());
        if let extensions::trust::Allowed::Place(place) = &allowed
            && ui.button("Stop trusting it").on_hover_text("For you, on this computer").clicked()
            && let Err(e) = extensions::trust::distrust_place(place)
        {
            *message = Some(format!("{}: {e}", place.display()));
        }
        ui.add_space(6.0);
        return;
    }
    let enable = matches!(p.status, Status::NotEnabled { .. });
    // What enabling it consents to downloading, listed after the choice, which stays in reach however long it is.
    let mut fetches: Vec<(project::ext::setup::Setup, bool)> = p
        .setup
        .iter()
        .filter(|setup| enable && setup.here())
        .map(|setup| (setup.clone(), !setup.lazy || extensions::optional_on(setup)))
        .collect();
    if enable {
        ui.label("This extension runs outside the sandbox: it and the programs it starts can read and change your files and reach the network. Enable it only if you trust where it came from.");
        if !fetches.is_empty() {
            ui.label(RichText::new("Enabling it trusts what it downloads, listed below.").small());
        }
        if matches!(p.origin, Origin::Project(_)) {
            ui.label(RichText::new("It came with this project.").strong());
        }
    }
    let label = if enable { "Enable" } else { "Disable" };
    if ui.button(label).on_hover_text("For you, on this computer, in every project").clicked() {
        if enable && crate::drive::agent_acting(ui.ctx()) {
            *message = Some(format!("{}: only you can enable it, not an agent", p.id));
        } else if let Err(e) = extensions::set_unsandboxed_enabled(trust, &p.id, enable) {
            *message = Some(format!("{}: {e}", p.id));
        }
    }
    // Where it is can be trusted instead, a no said when the project was opened taken back.
    if let Some((place, label)) =
        crate::onboarding::file_of(p).filter(|_| enable).map(|file| crate::onboarding::place_of(ws, file))
        && ui
            .button(format!("Trust {label}"))
            .on_hover_text("What is there runs from now on: for you, on this computer")
            .clicked()
    {
        if crate::drive::agent_acting(ui.ctx()) {
            *message = Some(format!("{label}: only you can trust it, not an agent"));
        } else if let Err(e) = extensions::trust::trust(&ws.project.root, std::slice::from_ref(&place)) {
            *message = Some(format!("{label}: {e}"));
        }
    }
    if !fetches.is_empty() {
        ui.add_space(4.0);
        ui.label(RichText::new("It downloads:").small());
        if crate::onboarding::fetches_ui(ui, &mut fetches) {
            for (setup, on) in fetches.iter().filter(|(setup, on)| setup.lazy && *on != extensions::optional_on(setup))
            {
                if *on && crate::drive::agent_acting(ui.ctx()) {
                    *message = Some(format!("{}: only you can turn it on, not an agent", setup.name));
                } else if let Err(e) = extensions::trust::set_optional(&setup.key(), *on) {
                    *message = Some(format!("{}: {e}", setup.name));
                }
            }
        }
    }
    ui.add_space(6.0);
}

/// What an extension's folder holds to be read: its guides, what to install first, and the licenses of what it ships,
/// each opened as it is.
fn documents_ui(ui: &mut egui::Ui, p: &Present, message: &mut Option<String>) {
    // An archive's are read once it is added and unpacked.
    let Some(folder) = p.origin.folder().filter(|folder| folder.is_dir()) else { return };
    let described = &p.described;
    let pages = [("Guides", described.docs.as_deref()), ("Installation", described.install.as_deref())];
    let pages: Vec<(&str, &str)> = pages.into_iter().filter_map(|(label, page)| Some((label, page?))).collect();
    if pages.is_empty() && described.bundled.is_empty() {
        return;
    }
    let mut open = |ui: &mut egui::Ui, label: &str, page: &str| {
        let file = folder.join(page);
        if ui.link(label).on_hover_text(file.display().to_string()).clicked()
            && let Err(e) = crate::external::open_default(&file)
        {
            *message = Some(e);
        }
    };
    theme::section(ui, "Documents", |ui| {
        if described.install.is_some() {
            ui.label(
                RichText::new("What it needs on this machine, fetched or installed, is in Installation.")
                    .small()
                    .color(theme::weak()),
            );
        }
        ui.horizontal_wrapped(|ui| {
            for (label, page) in pages {
                open(ui, label, page);
            }
        });
        for bundled in &described.bundled {
            ui.horizontal_wrapped(|ui| {
                open(ui, &bundled.license, &bundled.text);
                ui.label(RichText::new(&bundled.name).small().color(theme::weak()));
            });
        }
    });
}

/// What an extension fetches onto this machine (`[[setup]]`), for this platform: each package, where from, and how it
/// stands. What it requires is fetched once it runs, as trusting it consents to; what is optional has its switch - on,
/// fetched when the extension asks for it; off, refused - which is the user's, on this computer, and an agent's only to
/// turn off. Fetched, an optional package can be removed again.
fn fetches_ui(ui: &mut egui::Ui, ws: &mut Workspace, p: &Present, state: &mut State, message: &mut Option<String>) {
    let here: Vec<project::ext::setup::Setup> = p.setup.iter().filter(|setup| setup.here()).cloned().collect();
    let Some(store) = extensions::setup_folder().filter(|_| !here.is_empty()) else { return };
    let runs = p.status == Status::Loaded && matches!(p.origin, Origin::Project(_));
    theme::section(ui, "Fetches", |ui| {
        for setup in &here {
            ui.horizontal_wrapped(|ui| {
                ui.label(&setup.name);
                if setup.lazy {
                    ui.label(RichText::new("optional").small().color(theme::weak()));
                }
                let from = ui.label(RichText::new(format!("from {}", setup.host())).small().color(theme::weak()));
                let said = [setup.license.clone(), setup.copyright.as_ref().map(|c| format!("© {c}"))];
                let said: Vec<String> = said.into_iter().flatten().chain(setup.sources().map(str::to_string)).collect();
                from.on_hover_text(said.join("\n"));
            });
            ui.horizontal_wrapped(|ui| fetch_state_ui(ui, ws, p, setup, &store, runs, message));
            ui.add_space(4.0);
        }
        // Kept for every project: the Fetched view shows all of it, and what else this computer keeps.
        if ui.link("Everything fetched onto this computer").clicked() {
            state.view = View::Fetched;
        }
    });
}

/// How one package stands, and what can be done with it.
pub(super) fn fetch_state_ui(
    ui: &mut egui::Ui,
    ws: &mut Workspace,
    p: &Present,
    setup: &project::ext::setup::Setup,
    store: &std::path::Path,
    runs: bool,
    message: &mut Option<String>,
) {
    let weak = |text: &str| RichText::new(text.to_string()).small().color(theme::weak());
    let on = !setup.lazy || extensions::optional_on(setup);
    match ws.fetcher.state_of(setup) {
        Some(crate::fetcher::State::Fetching(done, total)) => {
            #[expect(clippy::cast_precision_loss, reason = "a fraction shown")]
            let fraction = total.filter(|t| *t > 0).map_or(0.0, |t| done as f32 / t as f32);
            ui.add(egui::ProgressBar::new(fraction).desired_width(160.0).show_percentage());
        }
        Some(crate::fetcher::State::Failed(why)) => {
            ui.label(RichText::new(why).small().color(ui.visuals().error_fg_color));
            if ui.small_button("Try again").clicked() {
                ws.fetcher.retry(setup);
            }
        }
        None if setup.fetched(store) => {
            ui.label(weak("fetched"));
            if setup.lazy
                && ui.small_button("Remove").on_hover_text("From this computer, for every project").clicked()
                && let Err(e) = project::ext::setup::remove(setup, store)
            {
                *message = Some(format!("{}: {e:#}", setup.name));
            }
        }
        None if !on => {
            ui.label(weak("turned off on this computer"));
        }
        None if setup.lazy => {
            ui.label(weak("fetched when it is asked for"));
            if runs && ui.small_button("Fetch now").clicked() {
                ws.fetcher.fetch_now(&p.id, p.origin.folder(), setup);
            }
        }
        None if runs => {
            ui.label(weak("to fetch"));
        }
        None => {
            ui.label(weak("fetched once it runs"));
        }
    }
    // An optional one's switch: an agent may turn it off, not on.
    if setup.lazy {
        let mut turned = on;
        let hint = "Whether it may fetch this when it asks for it: yours, on this computer, for every project";
        if ui.checkbox(&mut turned, "On").on_hover_text(hint).changed() {
            if turned && crate::drive::agent_acting(ui.ctx()) {
                *message = Some(format!("{}: only you can turn it on, not an agent", setup.name));
            } else if let Err(e) = extensions::trust::set_optional(&setup.key(), turned) {
                *message = Some(format!("{}: {e}", setup.name));
            }
        }
    }
}

/// Adding an extension kept apart to the project, updating the one it holds from there, or removing it: whether there
/// is nothing more to say of it.
fn adding_ui(
    ui: &mut egui::Ui,
    ws: &mut Workspace,
    present: &[Present],
    p: &Present,
    message: &mut Option<String>,
) -> bool {
    let held = matches!(p.origin, Origin::Project(_));
    // One of its id may be in effect - held, or a built-in - and one kept apart to take its place.
    let kept = present.iter().find(|q| q.id == p.id && q.status == Status::Available);
    let mut change = None;
    if let Some((kept, folder)) = kept.and_then(|kept| Some((kept, kept.origin.folder()?))) {
        if p.status != Status::Available {
            let said = format!("{} {} is kept: {}", kept.id, kept.version, origin_text(&kept.origin));
            ui.label(RichText::new(said).color(theme::weak()));
        }
        let (label, verb) = if held { ("Update it from there", "updated") } else { ("Add it to the project", "added") };
        let clicked = ui.button(label).on_hover_text("Copied into the project, for everyone on it").clicked();
        if clicked {
            let to = format!("{} {}", if held { "Update" } else { "Add" }, p.id);
            // An archive is unpacked first, and copied in as its folder.
            let added = if extensions::archive::is_archive(folder) {
                extensions::archive::unpack(folder).and_then(|unpacked| {
                    project::switches::add_extension(&ws.project, &unpacked.folder, None, &mut ws.history, to)
                })
            } else {
                project::switches::add_extension(&ws.project, folder, None, &mut ws.history, to)
            };
            change = Some((verb, added.map(|about| about.described.install.is_some())));
        }
    }
    if held
        && ui
            .button("Remove it from the project")
            .on_hover_text("Its folder goes, and what the project requires of it")
            .clicked()
    {
        let removed =
            project::switches::remove_extension(&ws.project, &p.id, &mut ws.history, format!("Remove {}", p.id));
        change = Some(("removed", removed.map(|()| false)));
    }
    if let Some((verb, written)) = change {
        *message = Some(match written {
            Ok(false) => format!("{}: {verb}", p.id),
            Ok(true) => format!("{}: {verb}; what it needs on this machine is in its Installation", p.id),
            Err(e) => format!("{}: {e:#}", p.id),
        });
    }
    p.status == Status::Available
}

/// The switch: on or off for the project, saying first what goes with it.
fn switch_ui(
    ui: &mut egui::Ui,
    ws: &mut Workspace,
    present: &[Present],
    p: &Present,
    state: &mut State,
    message: &mut Option<String>,
) {
    // Kept apart, it is only to be added; not enabled, it is not this project's to turn on.
    if adding_ui(ui, ws, present, p, message) || matches!(p.status, Status::NotEnabled { .. }) {
        return;
    }
    let disabled: BTreeSet<String> = ws.project.disabled_extensions.iter().cloned().collect();
    let off = disabled.contains(&p.id);
    if !off && matches!(p.status, Status::Refused(_)) {
        // Refused over what it needs, it waits for that; one that cannot be read at all has no switch.
        let waits = p
            .requires
            .iter()
            .any(|r| present.iter().find(|q| q.id == requirement(r).0).is_none_or(|q| q.status != Status::Loaded));
        if waits {
            ui.label(RichText::new("It cannot be turned on until what it needs is.").small().color(theme::weak()));
        }
        return;
    }
    if let Some((turn_on, asked, with)) = state.asked.clone().filter(|(_, asked, _)| *asked == p.id) {
        let verb = if turn_on { "on" } else { "off" };
        let others = if with.is_empty() { String::new() } else { format!(", and with it {}", with.join(", ")) };
        ui.label(format!("Turn {asked} {verb} for this project{others}?"));
        ui.horizontal(|ui| {
            if ui.button(format!("Turn {verb}")).clicked() {
                let mut next = disabled.clone();
                if turn_on {
                    next.remove(&asked);
                    for id in &with {
                        next.remove(id);
                    }
                } else {
                    next.insert(asked.clone());
                    next.extend(left_off_too(present, &asked, &with));
                }
                let next: Vec<String> = next.into_iter().collect();
                let label = format!("Turn {asked} {verb}");
                let written = project::switches::set_disabled(&ws.project, &next, &mut ws.history, label);
                *message = Some(match written {
                    Ok(()) => format!("{asked} turned {verb}"),
                    Err(e) => format!("project.toml: {e:#}"),
                });
                state.asked = None;
            }
            if ui.button("Cancel").clicked() {
                state.asked = None;
            }
        });
        return;
    }
    let mut on = !off;
    if ui.checkbox(&mut on, "Enabled for this project").changed() {
        let (needs, needed) = chains(present, &p.id);
        // Off: those going with it. On: those it requires and the project left off come with it.
        let with: Vec<String> = if on {
            needs.into_iter().filter(|id| disabled.contains(id)).collect()
        } else {
            needed.into_iter().filter(|id| present.iter().any(|q| q.id == *id && q.status == Status::Loaded)).collect()
        };
        let mut with = with;
        with.sort();
        state.asked = Some((on, p.id.clone(), with));
    }
}
