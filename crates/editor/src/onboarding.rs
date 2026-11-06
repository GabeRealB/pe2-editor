//! What a project asks of whoever opens it, once, rather than keeping quiet: which of the places holding what runs outside
//! the sandbox to trust - each extension shown with what it downloads, which trusting it consents to, what is optional
//! with a switch - what it requires and does not have, and which config to work in. It asks nothing where there is
//! nothing to settle, and again only of what is new, or while the project is not whole. The answers are the user's: an agent
//! driving the editor cannot give them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use eframe::egui::{self, RichText};
use extensions::trust;
use project::ext::setup::Setup;
use project::ext::{Origin, Present, Status};

use crate::theme;
use crate::workspace::Workspace;

/// A place to trust, and what it holds.
pub struct Place {
    pub path: PathBuf,
    /// What it is to the project: `layer mods`, or where it is.
    pub label: String,
    pub extensions: Vec<String>,
    /// What each downloads for this platform, by its id: what is optional with whether to let it.
    pub fetches: Vec<(String, Vec<(Setup, bool)>)>,
    /// Without it, the project is not whole.
    pub required: bool,
    pub trust: bool,
}

/// What is being asked.
pub struct Onboarding {
    pub root: PathBuf,
    pub places: Vec<Place>,
    /// What the project requires and no answer here gives it, and why.
    pub missing: Vec<(String, String)>,
    /// The configs to choose from, where there is a choice to make.
    pub configs: Vec<String>,
    pub config: String,
}

/// What was answered.
pub enum Answer {
    /// As it stands, the places ticked trusted, the others not asked about again.
    Continue,
    /// All inside the project, as well as what is ticked outside it.
    Everything,
    /// Later: it is asked again the next time.
    NotNow,
}

/// The place an extension is in: the layer or mount holding it, else its own folder.
pub fn place_of(ws: &Workspace, file: &Path) -> (PathBuf, String) {
    let real = file.canonicalize().unwrap_or_else(|_| file.to_path_buf());
    let holding = ws
        .project
        .layers
        .iter()
        .filter_map(|layer| Some((layer, layer.path.canonicalize().ok()?)))
        .find(|(_, path)| real.starts_with(path));
    if let Some((layer, path)) = holding {
        return (path, format!("{} {}", if layer.mounted { "mount" } else { "layer" }, layer.name));
    }
    let folder = real.parent().unwrap_or(&real).to_path_buf();
    let label = folder.display().to_string();
    (folder, label)
}

/// Where an extension is that a place may hold: the folder a project holds it in, or a file named.
pub fn file_of(present: &Present) -> Option<&PathBuf> {
    match &present.origin {
        Origin::Project(file) | Origin::Component(file) => Some(file),
        _ => None,
    }
}

/// What a project has to ask; none where there is nothing. `config_chosen`: the user has settled on one already.
#[must_use]
pub fn asked(ws: &Workspace, config_chosen: bool) -> Option<Onboarding> {
    let root = ws.project.root.canonicalize().ok()?;
    let answered = trust::trusted().into_iter().find(|known| known.project == root);
    let declined = answered.map_or_default(|known| known.declined);
    let required = |id: &str| ws.extensions.unmet.iter().any(|(unmet, _)| unmet == id);
    let mut places: BTreeMap<PathBuf, Place> = BTreeMap::new();
    let waiting = ws.extensions.present.iter().filter(|present| matches!(present.status, Status::NotEnabled { .. }));
    for present in waiting {
        let Some(file) = file_of(present) else { continue };
        let (path, label) = place_of(ws, file);
        // Said no to, it is left alone, but for what the project cannot do without.
        if declined.contains(&path) && !required(&present.id) {
            continue;
        }
        let place = places.entry(path.clone()).or_insert_with(|| Place {
            path,
            label,
            extensions: Vec::new(),
            fetches: Vec::new(),
            required: false,
            trust: false,
        });
        place.extensions.push(present.id.clone());
        let fetches: Vec<(Setup, bool)> = present
            .setup
            .iter()
            .filter(|setup| setup.here())
            .map(|setup| (setup.clone(), !setup.lazy || extensions::optional_on(setup)))
            .collect();
        if !fetches.is_empty() {
            place.fetches.push((present.id.clone(), fetches));
        }
        place.required |= required(&present.id);
    }
    let untrusted = |id: &str| places.values().any(|place| place.extensions.iter().any(|extension| extension == id));
    let missing: Vec<(String, String)> = ws.extensions.unmet.iter().filter(|(id, _)| !untrusted(id)).cloned().collect();
    let configs: Vec<String> = ws.project.configs.iter().map(|config| config.name.clone()).collect();
    let configs = if configs.len() > 1 && !config_chosen { configs } else { Vec::new() };
    let places: Vec<Place> = places.into_values().collect();
    if places.is_empty() && missing.is_empty() && configs.is_empty() {
        return None;
    }
    let config = configs.first().cloned().unwrap_or_default();
    Some(Onboarding { root, places, missing, configs, config })
}

impl Onboarding {
    /// What was answered, kept with the user.
    ///
    /// # Errors
    /// What is trusted cannot be written.
    pub fn apply(&self, answer: &Answer) -> std::io::Result<()> {
        let chosen = |trusted: bool| {
            self.places.iter().filter(move |place| place.trust == trusted).map(|place| place.path.clone())
        };
        let mut trusting: Vec<PathBuf> = chosen(true).collect();
        if matches!(answer, Answer::Everything) {
            trusting.push(self.root.clone());
        }
        // What is optional, as switched: the user's, before what it lets runs.
        for place in &self.places {
            for (setup, on) in place.fetches.iter().flat_map(|(_, fetches)| fetches).filter(|(setup, _)| setup.lazy) {
                if *on != extensions::optional_on(setup) {
                    trust::set_optional(&setup.key(), *on)?;
                }
            }
        }
        if !trusting.is_empty() {
            trust::trust(&self.root, &trusting)?;
        }
        if matches!(answer, Answer::Continue) {
            trust::decline(&self.root, &chosen(false).collect::<Vec<_>>())?;
        }
        Ok(())
    }

    /// The question; the answer, once given.
    pub fn ui(&mut self, ctx: &egui::Context) -> Option<Answer> {
        let mut answer = None;
        egui::Modal::new(egui::Id::unique("onboarding")).show(ctx, |ui| {
            ui.set_width(520.0);
            ui.heading(self.root.file_name().map_or_else(|| "This project".to_string(), |name| name.to_string_lossy().into_owned()));
            ui.add_space(8.0);
            if !self.places.is_empty() {
                ui.label("These run outside the sandbox: they and the programs they start can read and change your files and reach the network. Trust only what you know the source of. Trusting one trusts what it downloads, listed under it - for a place, what its extensions download now and later - and what is optional can be turned off.");
                ui.add_space(6.0);
                // However much they download, what is answered stays on the screen below.
                let height = (ui.ctx().content_rect().height() * 0.5).max(160.0);
                egui::ScrollArea::vertical().max_height(height).auto_shrink([false, true]).show(ui, |ui| {
                for place in &mut self.places {
                    ui.checkbox(&mut place.trust, place.label.as_str());
                    let required = if place.required { "  ·  the project requires it" } else { "" };
                    let indent = ui.spacing().icon_width + ui.spacing().icon_spacing;
                    ui.horizontal(|ui| {
                        // Under the box's label.
                        ui.add_space(indent);
                        ui.label(RichText::new(format!("{}{required}", place.extensions.join(", "))).small().color(theme::weak()));
                    });
                    for (id, fetches) in &mut place.fetches {
                        ui.horizontal(|ui| {
                            ui.add_space(indent);
                            ui.vertical(|ui| {
                                ui.label(RichText::new(format!("{id} downloads:")).small());
                                fetches_ui(ui, fetches);
                            });
                        });
                    }
                    ui.add_space(4.0);
                }
                });
            }
            if !self.missing.is_empty() {
                ui.add_space(4.0);
                ui.label(RichText::new("The project requires what it does not have:").strong());
                for (id, why) in &self.missing {
                    ui.label(RichText::new(format!("{id}: {why}")).color(ui.visuals().warn_fg_color));
                }
            }
            if !self.configs.is_empty() {
                ui.add_space(4.0);
                egui::ComboBox::new("onboarding config", "Config").selected_text(&self.config).show_ui(ui, |ui| {
                    for config in &self.configs {
                        ui.selectable_value(&mut self.config, config.clone(), config);
                    }
                });
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Continue").clicked() {
                    answer = Some(Answer::Continue);
                }
                if !self.places.is_empty() && ui.button("Trust the whole project").on_hover_text("All inside it, not what is mounted from outside").clicked() {
                    answer = Some(Answer::Everything);
                }
                if ui.button("Not now").on_hover_text("Asked again next time").clicked() {
                    answer = Some(Answer::NotNow);
                }
            });
        });
        answer
    }
}

/// What an extension downloads for this platform, as consenting to it shows it: each package by name and license, the
/// host it comes from, its whole URL and the hash it is held to; what is optional with its switch. Whether a switch
/// changed.
pub fn fetches_ui(ui: &mut egui::Ui, fetches: &mut [(Setup, bool)]) -> bool {
    let mut changed = false;
    for (setup, on) in fetches {
        ui.horizontal_wrapped(|ui| {
            if setup.lazy {
                let hint = "Optional: fetched when it asks for it. Off, it is refused - yours, on this computer";
                changed |= ui.checkbox(on, setup.name.as_str()).on_hover_text(hint).changed();
            } else {
                ui.label(&setup.name);
            }
            if let Some(license) = &setup.license {
                ui.label(RichText::new(license).small().color(theme::weak()));
            }
            ui.label(RichText::new(format!("from {}", setup.host())).small().strong());
        });
        if let Some(copyright) = &setup.copyright {
            ui.add(egui::Label::new(RichText::new(format!("© {copyright}")).small().color(theme::weak())).wrap());
        }
        let short = &setup.sha256[..setup.sha256.len().min(16)];
        ui.add(egui::Label::new(RichText::new(&setup.url).small().monospace().color(theme::weak())).wrap());
        // Where else it is: the same bytes, held to the same hash, tried where that fails.
        for mirror in &setup.mirrors {
            let said = RichText::new(format!("or {mirror}")).small().monospace().color(theme::weak());
            ui.add(egui::Label::new(said).wrap());
        }
        ui.label(RichText::new(format!("sha256 {short}…")).small().monospace().color(theme::weak()))
            .on_hover_text(&setup.sha256);
    }
    changed
}
