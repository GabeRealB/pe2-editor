//! What a project asks of whoever opens it, once, rather than keeping quiet: which of the places holding what runs outside
//! the sandbox to trust, what it requires and does not have, and which config to work in. It asks nothing where there is
//! nothing to settle, and again only of what is new, or while the project is not whole. The answers are the user's: an agent
//! driving the editor cannot give them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use eframe::egui::{self, RichText};
use extensions::trust;
use project::ext::{Origin, Present, Status};

use crate::theme;
use crate::workspace::Workspace;

/// A place to trust, and what it holds.
pub struct Place {
    pub path: PathBuf,
    /// What it is to the project: `layer mods`, or where it is.
    pub label: String,
    pub extensions: Vec<String>,
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
    let holding = ws.project.layers.iter().filter_map(|layer| Some((layer, layer.path.canonicalize().ok()?))).find(|(_, path)| real.starts_with(path));
    if let Some((layer, path)) = holding {
        return (path, format!("{} {}", if layer.mounted { "mount" } else { "layer" }, layer.name));
    }
    let folder = real.parent().unwrap_or(&real).to_path_buf();
    let label = folder.display().to_string();
    (folder, label)
}

pub fn file_of(present: &Present) -> Option<&PathBuf> {
    match &present.origin {
        Origin::Project { file, .. } | Origin::Component(file) => Some(file),
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
        let place = places.entry(path.clone()).or_insert_with(|| Place { path, label, extensions: Vec::new(), required: false, trust: false });
        place.extensions.push(present.id.clone());
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
        let chosen = |trusted: bool| self.places.iter().filter(move |place| place.trust == trusted).map(|place| place.path.clone());
        let mut trusting: Vec<PathBuf> = chosen(true).collect();
        if matches!(answer, Answer::Everything) {
            trusting.push(self.root.clone());
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
                ui.label("These run outside the sandbox: they and the programs they start can read and change your files and reach the network. Trust only what you know the source of.");
                ui.add_space(6.0);
                for place in &mut self.places {
                    ui.checkbox(&mut place.trust, place.label.as_str());
                    let required = if place.required { "  ·  the project requires it" } else { "" };
                    ui.horizontal(|ui| {
                        // Under the box's label.
                        ui.add_space(ui.spacing().icon_width + ui.spacing().icon_spacing);
                        ui.label(RichText::new(format!("{}{required}", place.extensions.join(", "))).small().color(theme::weak()));
                    });
                    ui.add_space(4.0);
                }
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
