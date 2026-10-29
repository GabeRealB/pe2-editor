//! The editor's panes - assets, preview, inspector, references, project - and how the tile layout shows them: each
//! a tab that can be dragged into another group or split off, resized, and closed (closing hides it; the View menu
//! brings it back).

use eframe::egui::{self, Color32, RichText};
use egui_tiles::{Tile, TileId, Tiles, UiResponse};
use project::ext::Extensions;
use project::index::Asset;
use serde::{Deserialize, Serialize};

use crate::external::{self, Category};
use crate::inspector::{badge, size_text};
use crate::settings::{EditorSettings, Preview, Theme, Tools, Upscale};
use crate::workspace::{EditRequest, Entry, Node, Workspace};
use crate::{icons, theme};

/// A panel's name (`pe2.images@vram`), interned: a pane stays small and `Copy`, and a saved layout names its panels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PanelName(&'static str);

impl PanelName {
    pub fn new(name: &str) -> PanelName {
        static NAMES: std::sync::Mutex<std::collections::BTreeSet<&'static str>> =
            std::sync::Mutex::new(std::collections::BTreeSet::new());
        let mut names = NAMES.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(known) = names.get(name) {
            return PanelName(known);
        }
        let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
        names.insert(leaked);
        PanelName(leaked)
    }

    pub fn as_str(self) -> &'static str {
        self.0
    }
}

impl Serialize for PanelName {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.0)
    }
}

impl<'de> Deserialize<'de> for PanelName {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(PanelName::new(&String::deserialize(deserializer)?))
    }
}

/// What a pane shows: one of the editor's own panes, or a panel an extension registers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(from = "StoredKind")]
pub enum PaneKind {
    Assets,
    Preview,
    Inspector,
    References,
    Problems,
    Project,
    Settings,
    Layers,
    Layer,
    History,
    Panel(PanelName),
}

/// A pane kind as layouts saved it: the editor's VRAM pane, before it was an extension's panel, read as that panel.
#[derive(Deserialize)]
enum StoredKind {
    Assets,
    Preview,
    Inspector,
    References,
    Problems,
    Project,
    Settings,
    Vram,
    Layers,
    Layer,
    History,
    Panel(PanelName),
}

impl From<StoredKind> for PaneKind {
    fn from(stored: StoredKind) -> PaneKind {
        match stored {
            StoredKind::Assets => PaneKind::Assets,
            StoredKind::Preview => PaneKind::Preview,
            StoredKind::Inspector => PaneKind::Inspector,
            StoredKind::References => PaneKind::References,
            StoredKind::Problems => PaneKind::Problems,
            StoredKind::Project => PaneKind::Project,
            StoredKind::Settings => PaneKind::Settings,
            StoredKind::Vram => PaneKind::Panel(PanelName::new("pe2.images@vram")),
            StoredKind::Layers => PaneKind::Layers,
            StoredKind::Layer => PaneKind::Layer,
            StoredKind::History => PaneKind::History,
            StoredKind::Panel(name) => PaneKind::Panel(name),
        }
    }
}

impl PaneKind {
    /// The editor's own panes, in the View menu's order; the extensions' panels follow them (`with_panels`).
    pub const ALL: [PaneKind; 10] = [
        PaneKind::Assets,
        PaneKind::Preview,
        PaneKind::Layer,
        PaneKind::Inspector,
        PaneKind::References,
        PaneKind::Problems,
        PaneKind::Layers,
        PaneKind::History,
        PaneKind::Project,
        PaneKind::Settings,
    ];

    pub fn icon(self) -> &'static str {
        match self {
            PaneKind::Assets => icons::FOLDER,
            PaneKind::Preview => icons::IMAGE,
            PaneKind::Inspector => icons::FILE_TEXT,
            PaneKind::References => icons::STACK,
            PaneKind::Problems => icons::WARNING,
            PaneKind::Project => icons::HARD_DRIVES,
            PaneKind::Settings => icons::GEAR,
            PaneKind::Layers => icons::STACK_SIMPLE,
            PaneKind::Layer => icons::GIT_DIFF,
            PaneKind::History => icons::CLOCK_COUNTER_CLOCKWISE,
            PaneKind::Panel(name) => {
                extensions::shared().panel(name.as_str()).and_then(|p| p.icon).map_or(icons::SQUARES_FOUR, icons::glyph)
            }
        }
    }

    /// The editor's own panes, then the extensions' panels.
    pub fn with_panels() -> Vec<PaneKind> {
        let panels = extensions::shared().panels().iter().map(|p| PaneKind::Panel(PanelName::new(p.name.as_str()))).collect::<Vec<_>>();
        PaneKind::ALL.into_iter().chain(panels).collect()
    }

    /// Whether it may be open several times, each instance with its own state; the others are singletons.
    pub fn several(self) -> bool {
        matches!(self, PaneKind::Preview | PaneKind::Inspector | PaneKind::References | PaneKind::Panel(_))
    }

    /// Whether it shows one asset or entry: the selection, or the one an instance is pinned to.
    pub fn shows_subject(self) -> bool {
        matches!(self, PaneKind::Preview | PaneKind::Inspector | PaneKind::References)
    }

    /// Whether it lays out its own scrolling - a fixed filter over a scrolled tree, a canvas that pans, a sidebar
    /// beside a scrolled page. Every other pane scrolls whole, so none is ever cut off.
    fn scrolls_itself(self) -> bool {
        matches!(self, PaneKind::Assets | PaneKind::Panel(_) | PaneKind::Settings | PaneKind::Preview)
    }

    pub fn title(self) -> String {
        match self {
            PaneKind::Assets => "Assets",
            PaneKind::Preview => "Preview",
            PaneKind::Inspector => "Inspector",
            PaneKind::References => "References",
            PaneKind::Problems => "Problems",
            PaneKind::Project => "Project",
            PaneKind::Settings => "Settings",
            PaneKind::Layers => "Layers",
            PaneKind::Layer => "Layer",
            PaneKind::History => "History",
            PaneKind::Panel(name) => {
                return extensions::shared().panel(name.as_str()).map_or_else(|| name.as_str().to_string(), |p| p.label.clone());
            }
        }
        .to_string()
    }
}

/// A pane in a layout: its kind, and which instance of it (a singleton's is 0, as is the first of the others).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Pane {
    pub kind: PaneKind,
    pub instance: u32,
}

impl Pane {
    pub const fn first(kind: PaneKind) -> Pane {
        Pane { kind, instance: 0 }
    }
}

/// What an instance keeps of its own: the entry it is pinned to (its real path), where it does not follow the
/// selection - unlinked from it, as an inspector locked to one asset.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneState {
    #[serde(default)]
    pub pinned: Option<String>,
}

/// What a tab asks for, done once every layout is drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneAction {
    /// Another instance of its kind, beside it.
    Another,
    /// Show this entry, whatever is selected.
    Pin(String),
    /// Follow the selection again.
    Follow,
    /// Into a window of its own.
    ToNewWindow,
    /// Into another window; none: the main window.
    ToWindow(Option<u64>),
    /// Its tab closed: an instance after the first goes, a first instance is hidden in the main window.
    Close,
}

/// The default layout: assets and project on the left, the preview, the extensions' panels (VRAM) and the active
/// layer's changeset in the middle, the inspector over the layers, references and problems on the right.
pub fn default_layout() -> egui_tiles::Tree<Pane> {
    let mut tiles = Tiles::default();
    let left = [PaneKind::Assets, PaneKind::Project].map(|k| tiles.insert_pane(Pane::first(k)));
    let left = tiles.insert_tab_tile(left.to_vec());
    let panels = PaneKind::with_panels().into_iter().filter(|k| matches!(k, PaneKind::Panel(_)));
    let center = std::iter::once(PaneKind::Preview).chain(panels).chain([PaneKind::Layer]);
    let center = center.map(|k| tiles.insert_pane(Pane::first(k))).collect();
    let center = tiles.insert_tab_tile(center);
    let inspector = tiles.insert_pane(Pane::first(PaneKind::Inspector));
    let inspector = tiles.insert_tab_tile(vec![inspector]);
    let references = [PaneKind::Layers, PaneKind::History, PaneKind::References, PaneKind::Problems]
        .map(|k| tiles.insert_pane(Pane::first(k)));
    let references = tiles.insert_tab_tile(references.to_vec());
    let right = tiles.insert_vertical_tile(vec![inspector, references]);
    if let Some(Tile::Container(egui_tiles::Container::Linear(linear))) = tiles.get_mut(right) {
        linear.shares.set_share(inspector, 2.0);
        linear.shares.set_share(references, 1.0);
    }
    let root = tiles.insert_horizontal_tile(vec![left, center, right]);
    if let Some(Tile::Container(egui_tiles::Container::Linear(linear))) = tiles.get_mut(root) {
        linear.shares.set_share(left, 1.0);
        linear.shares.set_share(center, 1.9);
        linear.shares.set_share(right, 1.5);
    }
    egui_tiles::Tree::new(egui::Id::unique("pe2_editor_layout"), root, tiles)
}

/// What the layout needs to draw the panes: the open project, and where a click asks to go - `nav` for a link
/// (the asset tree then reveals the asset), `pick` for a click in the tree itself (which must not scroll it).
pub struct Panes<'a> {
    pub workspace: Option<&'a mut Workspace>,
    pub nav: Option<String>,
    pub pick: Option<String>,
    pub settings: &'a mut EditorSettings,
    pub layers: &'a mut crate::layers::LayersState,
    /// Each instance's own state.
    pub states: &'a mut std::collections::BTreeMap<Pane, PaneState>,
    /// The windows a tab may move to, by id and title; and the one this layout is in (none: the main window).
    pub windows: &'a [(u64, String)],
    pub window: Option<u64>,
    /// What tabs asked for, by tile.
    pub actions: Vec<(TileId, PaneAction)>,
    /// Steps the History pane asked to undo (false) or redo (true), and how many.
    pub steps: Option<(bool, usize)>,
    /// An edit asked for in a pane.
    pub edit: Option<EditRequest>,
    /// A line for the status bar: what an action did, or why it failed.
    pub message: Option<String>,
}

impl egui_tiles::Behavior<Pane> for Panes<'_> {
    fn pane_ui(&mut self, ui: &mut egui::Ui, _tile_id: TileId, pane: &mut Pane) -> UiResponse {
        let pane = *pane;
        let number = if pane.instance > 0 { format!(" {}", pane.instance + 1) } else { String::new() };
        crate::drive::note_pane(ui, format!("{}{number}", pane.kind.title()));
        if pane.kind == PaneKind::Settings {
            egui::Frame::new().inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                settings_ui(ui, self.settings, self.workspace.as_deref(), &mut self.message);
            });
            return UiResponse::None;
        }
        let Some(ws) = self.workspace.as_deref_mut() else {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new("No project open - File ▸ Open project").color(theme::weak()))
            });
            return UiResponse::None;
        };
        // An instance pinned to an entry shows it; the others show the selection.
        let pinned = self.states.get(&pane).and_then(|s| s.pinned.clone());
        let subject = pinned.clone().or_else(|| ws.selected.clone());
        let (nav, pick, settings, layers, message, steps, edit) = (
            &mut self.nav,
            &mut self.pick,
            &mut *self.settings,
            &mut *self.layers,
            &mut self.message,
            &mut self.steps,
            &mut self.edit,
        );
        let mut contents = |ui: &mut egui::Ui| {
            egui::Frame::new().inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                if let Some(pinned) = pinned.as_deref().filter(|p| !ws.exists(p)) {
                    ui.label(RichText::new(format!("{pinned} is no longer in the project")).color(theme::weak()));
                    return;
                }
                let subject = subject.as_deref();
                match pane.kind {
                    PaneKind::Assets => assets_ui(ui, ws, pick, edit),
                    PaneKind::Preview => preview_ui(ui, ws, egui::Id::unique(pane), subject, nav, &mut settings.preview),
                    PaneKind::Panel(name) => {
                        ws.show_panel(ui, egui::Id::unique(pane), name.as_str(), settings.preview, nav);
                    }
                    PaneKind::Inspector => {
                        crate::inspector::inspector_ui(ui, ws, subject, nav, &settings.tools, message, edit);
                    }
                    PaneKind::References => references_ui(ui, ws, subject, nav),
                    PaneKind::Problems => problems_ui(ui, ws, nav),
                    PaneKind::Project => project_ui(ui, ws, nav),
                    PaneKind::Layers => crate::layers::layers_ui(ui, ws, layers, message),
                    PaneKind::Layer => crate::layers::layer_ui(ui, ws, nav, edit),
                    PaneKind::History => history_ui(ui, ws, steps),
                    PaneKind::Settings => {}
                }
            });
        };
        if pane.kind.scrolls_itself() {
            contents(ui);
        } else {
            egui::ScrollArea::vertical().id_salt(pane).auto_shrink(false).show(ui, contents);
        }
        UiResponse::None
    }

    fn tab_title_for_pane(&mut self, pane: &Pane) -> egui::WidgetText {
        let number = if pane.instance > 0 { format!(" {}", pane.instance + 1) } else { String::new() };
        let pinned = self.states.get(pane).and_then(|s| s.pinned.as_deref()).map_or_else(String::new, |pinned| {
            let name = pinned.trim_end_matches(project::index::BUNDLE_SUFFIX).rsplit('/').next().unwrap_or(pinned);
            format!("  {}  {name}", icons::PUSH_PIN)
        });
        format!("{}  {}{number}{pinned}", pane.kind.icon(), pane.kind.title()).into()
    }

    fn is_tab_closable(&self, _tiles: &Tiles<Pane>, _tile_id: TileId) -> bool {
        true
    }

    /// The app decides what closing does (`PaneAction::Close`): `egui_tiles` never removes the tile itself.
    fn on_tab_close(&mut self, _tiles: &mut Tiles<Pane>, tile_id: TileId) -> bool {
        self.actions.push((tile_id, PaneAction::Close));
        false
    }

    /// A tab's menu: another instance, pinning, moving between windows.
    fn on_tab_button(&mut self, tiles: &mut Tiles<Pane>, tile_id: TileId, response: egui::Response) -> egui::Response {
        let Some(Tile::Pane(pane)) = tiles.get(tile_id) else { return response };
        let pane = *pane;
        response.context_menu(|ui| self.tab_menu(ui, tile_id, pane));
        response
    }

    fn simplification_options(&self) -> egui_tiles::SimplificationOptions {
        egui_tiles::SimplificationOptions { all_panes_must_have_tabs: true, ..Default::default() }
    }

    fn tab_bar_height(&self, _style: &egui::Style) -> f32 {
        28.0
    }

    fn gap_width(&self, _style: &egui::Style) -> f32 {
        3.0
    }

    fn tab_text_color(
        &self,
        _visuals: &egui::Visuals,
        _tiles: &Tiles<Pane>,
        _tile_id: TileId,
        state: &egui_tiles::TabState,
    ) -> Color32 {
        if state.active { theme::text() } else { theme::weak() }
    }
}

impl Panes<'_> {
    fn tab_menu(&mut self, ui: &mut egui::Ui, tile_id: TileId, pane: Pane) {
        let mut ask = |action| self.actions.push((tile_id, action));
        if pane.kind.several() && ui.button(format!("{}  Another {}", icons::PLUS, pane.kind.title())).clicked() {
            ask(PaneAction::Another);
        }
        if pane.kind.shows_subject() {
            let pinned = self.states.get(&pane).and_then(|s| s.pinned.clone());
            let selected = self.workspace.as_ref().and_then(|ws| ws.selected.clone());
            if pinned.is_some() {
                if ui.button("Follow the selection").clicked() {
                    ask(PaneAction::Follow);
                }
            } else if let Some(selected) = selected {
                let name = selected.rsplit('/').next().unwrap_or(&selected).to_string();
                let hint = "Keep showing it whatever is selected";
                if ui.button(format!("{}  Pin to {name}", icons::PUSH_PIN)).on_hover_text(hint).clicked() {
                    ask(PaneAction::Pin(selected));
                }
            }
        }
        ui.separator();
        if ui.button(format!("{}  Move to a new window", icons::APP_WINDOW)).clicked() {
            ask(PaneAction::ToNewWindow);
        }
        for (id, title) in self.windows {
            if Some(*id) != self.window && ui.button(format!("Move to {title}")).clicked() {
                ask(PaneAction::ToWindow(Some(*id)));
            }
        }
        if self.window.is_some() && ui.button(format!("{}  Move to the main window", icons::ARROW_SQUARE_IN)).clicked()
        {
            ask(PaneAction::ToWindow(None));
        }
    }
}

/// The session's history: what was done, the last of it marked, and what was undone and can be redone, greyed. A
/// click on a step undoes or redoes everything up to it.
fn history_ui(ui: &mut egui::Ui, ws: &Workspace, steps: &mut Option<(bool, usize)>) {
    let (done, undone) = (ws.history.done(), ws.history.undone());
    ui.label(
        RichText::new("What was done since the project was opened: closing it ends the history.")
            .small()
            .color(theme::weak()),
    );
    ui.add_space(4.0);
    if done.is_empty() && undone.is_empty() {
        ui.label(RichText::new("Nothing done yet").color(theme::weak()));
        return;
    }
    let row = |ui: &mut egui::Ui, step: &project::history::Step, current: bool, redone: bool| {
        let mut text = RichText::new(&step.label);
        if redone {
            text = text.color(theme::weak());
        }
        let author = if step.author == "user" { String::new() } else { format!("  ·  {}", step.author) };
        let response = ui.add(egui::Button::selectable(current, text).truncate()).on_hover_text(format!(
            "{} file{}{author}",
            step.paths().len(),
            if step.paths().len() == 1 { "" } else { "s" }
        ));
        response.clicked()
    };
    for (i, step) in done.iter().enumerate() {
        let last = i + 1 == done.len();
        if row(ui, step, last, false) && !last {
            *steps = Some((false, done.len() - 1 - i));
        }
    }
    for (i, step) in undone.iter().rev().enumerate() {
        if row(ui, step, false, true) {
            *steps = Some((true, i + 1));
        }
    }
}

fn scroll(ui: &mut egui::Ui, id: &str, add_contents: impl FnOnce(&mut egui::Ui)) {
    egui::ScrollArea::vertical().id_salt(id).auto_shrink(false).show(ui, add_contents);
}

// --- assets -----------------------------------------------------------------------------------------------------------

fn assets_ui(ui: &mut egui::Ui, ws: &mut Workspace, pick: &mut Option<String>, edit: &mut Option<EditRequest>) {
    ui.add(
        egui::TextEdit::singleline(&mut ws.filter)
            .hint_text(format!("{}  Filter by path or kind", icons::MAGNIFYING_GLASS))
            .desired_width(f32::INFINITY),
    );
    ui.add_space(4.0);
    let filter = ws.filter.trim().to_lowercase();
    let mut chosen = None;
    scroll(ui, "assets", |ui| {
        if filter.is_empty() {
            let view = TreeView { ws, reveal: ws.reveal.as_deref(), edit: std::cell::RefCell::new(None) };
            view.ui(ui, &ws.tree, "", pick, &mut chosen);
            *edit = view.edit.into_inner().or(edit.take());
        } else {
            filtered_ui(ui, ws, &filter, pick);
        }
    });
    ws.reveal = None;
    if let Some(entry) = chosen {
        ws.choose(&entry);
    }
}

/// The asset tree as drawn: the workspace for kinds and the selection, and an asset to reveal once.
struct TreeView<'a> {
    ws: &'a Workspace,
    reveal: Option<&'a str>,
    /// An edit an entry's menu asked for.
    edit: std::cell::RefCell<Option<EditRequest>>,
}

impl TreeView<'_> {
    fn ui(&self, ui: &mut egui::Ui, node: &Node, prefix: &str, pick: &mut Option<String>, chosen: &mut Option<Entry>) {
        for (name, child) in node.sorted_children() {
            let path = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
            if child.children.is_empty() {
                if let Some(entry) = &child.entry {
                    self.entry_label(ui, name, entry, chosen);
                } else if child.asset.is_some() {
                    ui.horizontal(|ui| self.asset_label(ui, name, child, pick));
                } else {
                    self.file_label(ui, name, child, pick);
                }
                continue;
            }
            let id = ui.make_persistent_id(&path);
            let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false);
            if self.reveal.is_some_and(|r| r.starts_with(&format!("{path}/"))) {
                state.set_open(true);
            }
            let open = state.is_open();
            // A click on a folder's name toggles it; on a folder that is an asset, it selects the asset and opens
            // the folder (a double click toggles), so selecting it again does not fold it away.
            let mut set_open = None;
            state
                .show_header(ui, |ui| {
                    let label = match &child.entry {
                        Some(entry) => Some(self.entry_label(ui, name, entry, chosen)),
                        None => self.asset_label(ui, name, child, pick),
                    };
                    if child.asset.is_some() || child.entry.is_some() {
                        if let Some(r) = label {
                            if r.double_clicked() {
                                set_open = Some(!open);
                            } else if r.clicked() && !open {
                                set_open = Some(true);
                            }
                        }
                    } else {
                        let icon = if open { icons::FOLDER_OPEN } else { icons::FOLDER };
                        let strength = if self.ws.is_editable(&path) { 1.0 } else { DIMMED };
                        let tint = theme::weak().gamma_multiply(strength);
                        let label = egui::Label::new(RichText::new(format!("{icon}  {name}")).color(tint))
                            .sense(egui::Sense::click())
                            .selectable(false);
                        let response = ui.add(label).on_hover_cursor(egui::CursorIcon::PointingHand);
                        if response.clicked() {
                            set_open = Some(!open);
                        }
                        self.menu(&response, child);
                        self.badge(ui, child);
                    }
                })
                .body(|ui| self.ui(ui, child, &path, pick, chosen));
            if let Some(open) = set_open {
                let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false);
                state.set_open(open);
                state.store(ui.ctx());
            }
        }
    }

    /// A virtual entry: a variant (its palette's icon) or an atlas part.
    fn entry_label(&self, ui: &mut egui::Ui, name: &str, entry: &Entry, chosen: &mut Option<Entry>) -> egui::Response {
        let dim = !self.ws.is_editable(&entry.image);
        let job = icon_and_name(ui.style(), icons::PALETTE, theme::weak(), theme::text(), name, dim);
        let response = ui.add(egui::Button::selectable(self.ws.is_chosen(entry), job).truncate());
        if response.clicked() {
            *chosen = Some(entry.clone());
        }
        response
    }

    fn asset_label(
        &self,
        ui: &mut egui::Ui,
        name: &str,
        node: &Node,
        pick: &mut Option<String>,
    ) -> Option<egui::Response> {
        let asset = node.asset.as_deref()?;
        let found = self.ws.index.assets.get(asset);
        let kind = found.and_then(|a| a.kind.as_deref());
        let dim = !self.ws.is_editable(asset);
        let selected = self.ws.selected.as_deref() == Some(asset);
        // An asset whose kind makes thumbnails shows its own in place of its kind's icon, once it is made.
        let thumbnail =
            found.filter(|a| self.ws.extensions.has_thumbnail(a)).and_then(|a| self.ws.thumbs.get(ui.ctx(), &self.ws.extensions, a));
        let button = match thumbnail {
            Some(texture) => {
                let side = ui.text_style_height(&egui::TextStyle::Body);
                let tint = if dim { Color32::from_white_alpha(115) } else { Color32::WHITE };
                let image = egui::Image::new((texture, egui::vec2(side, side))).maintain_aspect_ratio(true).tint(tint);
                let font = egui::TextStyle::Body.resolve(ui.style());
                let color = if dim { theme::text().gamma_multiply(DIMMED) } else { theme::text() };
                let name = egui::text::LayoutJob::simple_singleline(name.to_string(), font, color);
                egui::Button::selectable(selected, (image, name))
            }
            None => egui::Button::selectable(selected, labelled(&self.ws.extensions, ui.style(), kind, name, dim)),
        };
        let response = ui.add(button.truncate());
        if self.reveal == Some(asset) {
            response.scroll_to_me(Some(egui::Align::Center));
        }
        if response.clicked() {
            *pick = Some(asset.to_string());
        }
        self.menu(&response, node);
        self.badge(ui, node);
        Some(response)
    }

    /// A plain file, or a folder with nothing in it: selected by its real path.
    fn file_label(&self, ui: &mut egui::Ui, name: &str, node: &Node, pick: &mut Option<String>) {
        let Some(path) = node.path.as_deref() else { return };
        let icon = if node.file { icons::FILE } else { icons::FOLDER };
        let job = icon_and_name(ui.style(), icon, theme::weak(), theme::text(), name, !self.ws.is_editable(path));
        ui.horizontal(|ui| {
            let response = ui.add(egui::Button::selectable(self.ws.selected.as_deref() == Some(path), job).truncate());
            if self.reveal == Some(path) {
                response.scroll_to_me(Some(egui::Align::Center));
            }
            if response.clicked() {
                *pick = Some(path.to_string());
            }
            self.menu(&response, node);
            self.badge(ui, node);
        });
    }

    /// An entry's menu: its edits in the active layer.
    fn menu(&self, response: &egui::Response, node: &Node) {
        let Some(real) = node.path.as_deref() else { return };
        response.context_menu(|ui| {
            if let Some(request) = entry_menu(ui, self.ws, real) {
                *self.edit.borrow_mut() = Some(request);
                ui.close();
            }
        });
    }

    /// Beside an entry's name: the layer that overrode, moved or made it, where that is worth saying.
    fn badge(&self, ui: &mut egui::Ui, node: &Node) {
        if let Some(text) = node.path.as_deref().and_then(|p| self.ws.badge(p)) {
            ui.label(RichText::new(text).small().color(theme::accent()));
        }
    }
}

/// The edits of an entry (by its real path) the active layer can make: override, rename, delete, take back.
fn entry_menu(ui: &mut egui::Ui, ws: &Workspace, real: &str) -> Option<EditRequest> {
    let entry = ws.index.entries.get(real)?;
    let layer = match ws.editable_layer() {
        Ok(layer) => layer,
        Err(why) => {
            ui.label(RichText::new(why).color(theme::weak()));
            return None;
        }
    };
    let name = &layer.name;
    let own = entry.home == *name;
    let overridden = entry.overrides.iter().any(|l| l == name);
    let uid = entry.uid.clone();
    let mut request = None;
    ui.label(RichText::new(format!("In the layer {name}")).small().color(theme::weak()));
    if entry.kind != project::index::Kind::Folder && !own && !overridden {
        let hint = "Copy the version below into the layer, to edit it there";
        if ui.button(format!("Override in {name}")).on_hover_text(hint).clicked() {
            request = Some(EditRequest::Override(uid.clone()));
        }
    }
    if overridden && ui.button("Take the version below").on_hover_text("Drop the layer's copy").clicked() {
        request = Some(EditRequest::Resolve(uid.clone(), project::edit::Resolution::TakeBase));
    }
    if ui.button("Rename…").clicked() {
        request = Some(EditRequest::Rename(uid.clone(), real.to_string()));
    }
    let delete = if own { "Delete".to_string() } else { format!("Delete in {name}") };
    let hint = if own { "Remove it from the layer" } else { "The layer hides it: the layers below keep it" };
    if ui.button(delete).on_hover_text(hint).clicked() {
        request = Some(EditRequest::Delete(uid));
    }
    request
}

/// A name after its kind's icon, the icon in the kind's tint; dimmed where it cannot be modified.
fn labelled(
    extensions: &Extensions,
    style: &egui::Style,
    kind: Option<&str>,
    name: &str,
    dim: bool,
) -> egui::text::LayoutJob {
    let (icon, tint) = icons::for_kind(extensions, kind);
    icon_and_name(style, icon, tint, theme::text(), name, dim)
}

/// What cannot be modified - from a locked layer, or not the soloed layer's - is drawn at this strength.
const DIMMED: f32 = 0.45;

fn icon_and_name(
    style: &egui::Style,
    icon: &str,
    tint: Color32,
    color: Color32,
    name: &str,
    dim: bool,
) -> egui::text::LayoutJob {
    let strength = if dim { DIMMED } else { 1.0 };
    let mut job = egui::text::LayoutJob::default();
    let font = egui::TextStyle::Body.resolve(style);
    job.append(icon, 0.0, egui::TextFormat::simple(font.clone(), tint.gamma_multiply(strength)));
    job.append(name, 7.0, egui::TextFormat::simple(font, color.gamma_multiply(strength)));
    job
}

fn filtered_ui(ui: &mut egui::Ui, ws: &Workspace, filter: &str, pick: &mut Option<String>) {
    const SHOWN: usize = 500;
    let matches: Vec<&Asset> = ws
        .index
        .assets
        .values()
        .filter(|a| a.path.to_lowercase().contains(filter) || a.kind.as_deref() == Some(filter))
        .collect();
    ui.label(RichText::new(format!("{} matches", matches.len())).small().color(theme::weak()));
    for asset in matches.iter().take(SHOWN) {
        let selected = ws.selected.as_deref() == Some(asset.path.as_str());
        let job = labelled(&ws.extensions, ui.style(), asset.kind.as_deref(), &asset.path, !ws.is_editable(&asset.path));
        if ui.add(egui::Button::selectable(selected, job).truncate()).clicked() {
            *pick = Some(asset.path.clone());
        }
    }
    if matches.len() > SHOWN {
        ui.label(RichText::new(format!("… {} more", matches.len() - SHOWN)).color(theme::weak()));
    }
}

// --- preview ----------------------------------------------------------------------------------------------------------

fn preview_ui(
    ui: &mut egui::Ui,
    ws: &mut Workspace,
    pane: egui::Id,
    subject: Option<&str>,
    nav: &mut Option<String>,
    view: &mut Preview,
) {
    // A plain file: its text or bytes.
    if let Some((_, entry)) = subject.and_then(|s| ws.entry_at(s))
        && entry.kind == project::index::Kind::File
    {
        let disk = entry.disk.clone();
        ws.viewer.ui(ui, &disk);
        return;
    }
    let Some(asset) = subject.and_then(|s| ws.index.assets.get(s)) else {
        ui.label(RichText::new("Select an asset").color(theme::weak()));
        return;
    };
    let shown = ws.previews_of(asset);
    if shown.is_empty() {
        files_view_ui(ui, ws, &asset.clone());
        return;
    }
    scroll(ui, "preview", |ui| views_ui(ui, ws, pane, nav, view, shown));
}

/// An asset without a picture: one of its files at a time, as text or bytes - its native form first, by default.
fn files_view_ui(ui: &mut egui::Ui, ws: &mut Workspace, asset: &Asset) {
    if asset.files.is_empty() {
        ui.label(RichText::new("Nothing to preview for this asset").color(theme::weak()));
        return;
    }
    let default = asset.files.iter().find(|f| f.name != project::index::ASSET_FILE).unwrap_or(&asset.files[0]);
    let mut chosen = ws.viewed_file.get(&asset.path).cloned().unwrap_or_else(|| default.name.clone());
    if !asset.files.iter().any(|f| f.name == chosen) {
        chosen.clone_from(&default.name);
    }
    ui.horizontal_wrapped(|ui| {
        for file in &asset.files {
            ui.selectable_value(&mut chosen, file.name.clone(), &file.name);
        }
    });
    ws.viewed_file.insert(asset.path.clone(), chosen.clone());
    ui.add_space(4.0);
    if let Some(file) = asset.files.iter().find(|f| f.name == chosen) {
        let path = file.path.clone();
        ws.viewer.ui(ui, &path);
    }
}

/// An asset's views, by its kind's extension: its own, or those of the assets its slots hold, each under its look
/// picker.
fn views_ui(
    ui: &mut egui::Ui,
    ws: &mut Workspace,
    pane: egui::Id,
    nav: &mut Option<String>,
    view: &mut Preview,
    shown: Vec<(Option<String>, String)>,
) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Upscaling").small().color(theme::weak()));
        upscale_picker(ui, &mut view.upscale);
        ui.checkbox(&mut view.grid, "Grid").on_hover_text("A texel grid, its spacing following the zoom");
    });
    let count = shown.len();
    for (slot, path) in shown {
        // The slot and the asset's path on the left, cut short where the pane is narrow; its look on the right.
        egui::containers::Sides::new().shrink_left().truncate().show(
            ui,
            |ui| {
                if let Some(slot) = &slot {
                    ui.label(RichText::new(slot).strong());
                    if theme::short_link(ui, &path).clicked() {
                        *nav = Some(path.clone());
                    }
                }
            },
            |ui| variant_picker(ui, ws, &path),
        );
        ws.show_view(ui, pane, &path, *view, count, nav);
        ui.add_space(8.0);
    }
}

pub fn upscale_picker(ui: &mut egui::Ui, upscale: &mut Upscale) {
    egui::ComboBox::from_id_salt("upscale").selected_text(upscale.label()).show_ui(ui, |ui| {
        for method in Upscale::ALL {
            ui.selectable_value(upscale, method, method.label());
        }
    });
}

/// For an image with several variants - its other looks: the one it is shown in.
fn variant_picker(ui: &mut egui::Ui, ws: &mut Workspace, path: &str) {
    let variants = ws.variants_of(path);
    if variants.len() < 2 {
        return;
    }
    let chosen = ws.variant(path);
    let mut choice = chosen;
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.add_enabled_ui(choice + 1 < variants.len(), |ui| {
            if ui.small_button(icons::CARET_RIGHT).on_hover_text("Next variant").clicked() {
                choice += 1;
            }
        });
        egui::ComboBox::from_id_salt(("variant", path))
            .selected_text(format!("{}  {}", icons::IMAGE, variants[chosen]))
            .width(200.0)
            .show_ui(ui, |ui| {
                for (i, v) in variants.iter().enumerate() {
                    ui.selectable_value(&mut choice, i, v);
                }
            });
        ui.add_enabled_ui(choice > 0, |ui| {
            if ui.small_button(icons::CARET_LEFT).on_hover_text("Previous variant").clicked() {
                choice -= 1;
            }
        });
        ui.label(RichText::new(format!("{} of {}", chosen + 1, variants.len())).small().color(theme::weak()));
    });
    if choice != chosen {
        ws.variant_choice.insert(path.to_string(), choice);
    }
}

/// Pictures laid out on a canvas, with named boxes over them: an atlas's cells, a multi-page image's pages, VRAM's
/// slots - an extension's `Canvas`, uploaded.
pub struct Overlay {
    pub id: String,
    pub size: egui::Vec2,
    /// Each picture, where it is drawn, what hovering it says, what clicking it opens.
    pub pictures: Vec<(egui::TextureId, egui::Rect, Option<String>, Option<String>)>,
    /// Each box, named, and whether it is drawn stronger.
    pub boxes: Vec<(String, egui::Rect, bool)>,
    pub summary: String,
    /// The toggle showing the boxes; without one they are always drawn.
    pub toggle: String,
    /// Boxes each in a color of its own, named in a corner and in a legend; else plain.
    pub legend: bool,
    /// Lines every so many units across and down.
    pub grid: Option<(f32, f32)>,
}

/// An overlay drawn: each picture, a box over each named rectangle, the legend; hovering an entry of either highlights
/// it in both, hovering the canvas names what is under the pointer. Returns the link of a picture clicked.
pub fn overlay_ui(ui: &mut egui::Ui, overlay: &Overlay, view: Preview, count: usize) -> Option<String> {
    let (path, boxes, layout_size) = (overlay.id.as_str(), &overlay.boxes, overlay.size);
    let color = |i: usize, highlight: bool| {
        if overlay.legend {
            box_color(i)
        } else if highlight {
            theme::accent()
        } else {
            theme::weak().gamma_multiply(0.6)
        }
    };
    let shown_id = egui::Id::unique(("overlay boxes", path));
    let hover_id = egui::Id::unique(("overlay hovered", path));
    let mut show_boxes = ui.data(|d| d.get_temp::<bool>(shown_id)).unwrap_or(true);
    let hovered: Option<usize> = ui.data(|d| d.get_temp(hover_id)).flatten();
    let mut now_hovered = None;
    if overlay.toggle.is_empty() {
        show_boxes = true;
        if !overlay.summary.is_empty() {
            ui.label(RichText::new(&overlay.summary).small().color(theme::weak()));
        }
    } else if !boxes.is_empty() {
        ui.horizontal(|ui| {
            ui.checkbox(&mut show_boxes, &overlay.toggle);
            ui.label(RichText::new(&overlay.summary).small().color(theme::weak()));
        });
    }
    ui.data_mut(|d| d.insert_temp(shown_id, show_boxes));
    let legend = overlay.legend && show_boxes && !boxes.is_empty();
    #[expect(clippy::cast_precision_loss, reason = "a handful of pictures")]
    let height = (ui.available_height() * if legend { 0.7 } else { 1.0 } - 40.0).max(160.0) / count.max(1) as f32;
    let (canvas, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::click_and_drag());
    let painter = ui.painter_at(canvas);
    let (scale, origin) = zoom_pan(ui, &response, canvas, path, layout_size);
    let to_screen =
        |r: egui::Rect| egui::Rect::from_min_max(origin + r.min.to_vec2() * scale, origin + r.max.to_vec2() * scale);
    painter.rect_filled(canvas, 0.0, ui.visuals().extreme_bg_color);
    let whole = to_screen(egui::Rect::from_min_size(egui::Pos2::ZERO, layout_size));
    if let Some(grid) = overlay.grid {
        page_grid(&painter, whole, layout_size, scale, grid);
    }
    let uv = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
    for (texture, rect, _, _) in &overlay.pictures {
        let mut screen = to_screen(*rect);
        // A row of colors stays visible at small scales.
        screen.max.y = screen.max.y.max(screen.min.y + 2.0);
        painter.image(*texture, screen, uv, Color32::WHITE);
    }
    if view.grid {
        grid_ui(&painter, whole, layout_size, scale);
    }
    if show_boxes {
        let font = egui::TextStyle::Small.resolve(ui.style());
        for (i, (name, rect, highlight)) in boxes.iter().enumerate() {
            let strong = hovered == Some(i) || *highlight;
            let width = if strong { if overlay.legend { 3.0 } else { 2.0 } } else { 1.0 };
            let screen = to_screen(*rect);
            painter.rect_stroke(screen, 0.0, egui::Stroke::new(width, color(i, *highlight)), egui::StrokeKind::Inside);
            if !overlay.legend {
                continue;
            }
            // The name, on a dark band in the box's corner, where the box is large enough to hold it.
            let galley = painter.layout_no_wrap(name.clone(), font.clone(), Color32::WHITE);
            let band = egui::Rect::from_min_size(screen.min, galley.size() + egui::vec2(6.0, 2.0));
            if band.width() <= screen.width() && band.height() <= screen.height() || hovered == Some(i) {
                painter.rect_filled(band, 2.0, Color32::from_black_alpha(170));
                painter.galley(band.min + egui::vec2(3.0, 1.0), galley, Color32::WHITE);
            }
        }
    }
    let mut clicked = None;
    if let Some(pointer) = response.hover_pos() {
        let at = ((pointer - origin) / scale).to_pos2();
        let under: Vec<usize> = (0..boxes.len()).filter(|&i| boxes[i].1.contains(at)).collect();
        now_hovered = under.first().copied();
        let pictures: Vec<&(egui::TextureId, egui::Rect, Option<String>, Option<String>)> =
            overlay.pictures.iter().filter(|(_, rect, _, _)| rect.contains(at)).collect();
        let mut lines = vec![format!("({:.0}, {:.0})", at.x.floor(), at.y.floor())];
        lines.extend(pictures.iter().filter_map(|(_, _, label, _)| label.clone()));
        if show_boxes {
            lines.extend(under.iter().map(|&i| boxes[i].0.clone()));
        }
        if response.clicked() {
            clicked = pictures.iter().find_map(|(_, _, _, link)| link.clone());
        }
        response.clone().on_hover_text_at_pointer(lines.join("\n"));
    }
    ui.label(
        RichText::new(format!(
            "{} × {}  ·  {:.0}%  ·  wheel zooms, drag pans, double-click fits",
            layout_size.x,
            layout_size.y,
            scale * 100.0
        ))
        .small()
        .color(theme::weak()),
    );
    if legend && let Some(i) = legend_ui(ui, path, boxes, hovered) {
        now_hovered = Some(i);
    }
    ui.data_mut(|d| d.insert_temp(hover_id, now_hovered));
    clicked
}

/// Lines every so many units across and down a canvas drawn at `scale`: VRAM's texture pages.
fn page_grid(painter: &egui::Painter, whole: egui::Rect, size: egui::Vec2, scale: f32, (across, down): (f32, f32)) {
    let stroke = egui::Stroke::new(1.0, theme::border().gamma_multiply(0.5));
    let mut x = across;
    while x < size.x {
        let at = whole.left() + x * scale;
        painter.line_segment([egui::pos2(at, whole.top()), egui::pos2(at, whole.bottom())], stroke);
        x += across;
    }
    let mut y = down;
    while y < size.y {
        let at = whole.top() + y * scale;
        painter.line_segment([egui::pos2(whole.left(), at), egui::pos2(whole.right(), at)], stroke);
        y += down;
    }
}

/// An overlay's legend: each box's color and name in columns; hovering an entry highlights its box. Returns the entry
/// hovered.
fn legend_ui(ui: &mut egui::Ui, path: &str, boxes: &[(String, egui::Rect, bool)], hovered: Option<usize>) -> Option<usize> {
    let color = box_color;
    let mut now_hovered = None;
    ui.add_space(6.0);
    let font = egui::TextStyle::Small.resolve(ui.style());
    let widest = boxes
        .iter()
        .map(|(name, _, _)| ui.fonts_mut(|f| f.layout_no_wrap(name.clone(), font.clone(), Color32::WHITE).size().x))
        .fold(0.0, f32::max);
    let column = widest + 28.0;
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a column count")]
    let columns = ((ui.available_width() / column).floor() as usize).max(1);
    egui::Grid::new(("atlas legend", path)).min_col_width(column).spacing([4.0, 2.0]).show(ui, |ui| {
        for (i, (name, rect, _)) in boxes.iter().enumerate() {
            let entry = ui
                .horizontal(|ui| {
                    let (swatch, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                    ui.painter().rect_filled(swatch, 2.0, color(i));
                    let text = RichText::new(name).small();
                    ui.label(if hovered == Some(i) { text.strong() } else { text });
                })
                .response
                .interact(egui::Sense::hover())
                .on_hover_text(format!("{} × {} at ({}, {})", rect.width(), rect.height(), rect.min.x, rect.min.y));
            if entry.hovered() {
                now_hovered = Some(i);
            }
            if (i + 1) % columns == 0 {
                ui.end_row();
            }
        }
    });
    now_hovered
}

/// A box's color in an overlay: hues a golden angle apart.
fn box_color(i: usize) -> Color32 {
    #[expect(clippy::cast_precision_loss, reason = "a legend's length")]
    let hue = (i as f32 * 0.618_034).fract();
    egui::ecolor::Hsva::new(hue, 0.8, 1.0, 1.0).into()
}

/// A texel grid over a picture drawn at `scale` points a texel: lines every `step` texels - the finest power of two
/// leaving them at least 8 points apart, so one texel apart when zoomed in - and every eighth line stronger.
fn grid_ui(painter: &egui::Painter, picture: egui::Rect, size: egui::Vec2, scale: f32) {
    const MIN_SPACING: f32 = 8.0;
    let mut step = 1.0_f32;
    while step * scale < MIN_SPACING {
        step *= 2.0;
    }
    let minor = egui::Stroke::new(1.0, Color32::from_white_alpha(28));
    let major = egui::Stroke::new(1.0, Color32::from_white_alpha(64));
    let visible = picture.intersect(painter.clip_rect());
    let lines = |extent: f32, from: f32, to: f32, origin: f32, mut draw: Box<dyn FnMut(f32, egui::Stroke) + '_>| {
        // Only the lines on screen: the first at or before the visible edge, then every step to the far one.
        let first = ((from - origin) / scale / step).floor().max(0.0);
        let mut k = first;
        while k * step <= extent {
            let at = origin + k * step * scale;
            if at > to {
                break;
            }
            #[expect(clippy::cast_possible_truncation, reason = "a line's index")]
            let stroke = if (k as i64) % 8 == 0 { major } else { minor };
            draw(at, stroke);
            k += 1.0;
        }
    };
    lines(
        size.x,
        visible.left(),
        visible.right(),
        picture.left(),
        Box::new(|x, stroke| {
            painter.line_segment([egui::pos2(x, visible.top()), egui::pos2(x, visible.bottom())], stroke);
        }),
    );
    lines(
        size.y,
        visible.top(),
        visible.bottom(),
        picture.top(),
        Box::new(|y, stroke| {
            painter.line_segment([egui::pos2(visible.left(), y), egui::pos2(visible.right(), y)], stroke);
        }),
    );
}

/// Zoom and pan over a picture of `size` texels in `canvas`: the wheel (or a pinch, or Ctrl and the wheel) zooms
/// around the pointer, a drag pans, a double click fits the picture again. The view is kept per picture. Returns the
/// scale (screen points a texel) and where the picture's corner is drawn.
fn zoom_pan(
    ui: &mut egui::Ui,
    response: &egui::Response,
    canvas: egui::Rect,
    path: &str,
    layout_size: egui::Vec2,
) -> (f32, egui::Pos2) {
    let view_id = egui::Id::unique(("overlay view", path));
    let (mut zoom, mut offset) =
        ui.data(|d| d.get_temp::<(f32, egui::Vec2)>(view_id)).unwrap_or((1.0, egui::Vec2::ZERO));
    let fit = (canvas.width() / layout_size.x).min(canvas.height() / layout_size.y).max(0.01);
    if response.double_clicked() {
        (zoom, offset) = (1.0, egui::Vec2::ZERO);
    }
    if response.dragged() {
        offset += response.drag_delta();
    }
    if let Some(pointer) = response.hover_pos() {
        let (wheel, pinch) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
        let factor = (wheel / 300.0).exp() * pinch;
        if (factor - 1.0).abs() > f32::EPSILON {
            let new_zoom = (zoom * factor).clamp(0.1, 64.0);
            // Keep the texel under the pointer where it is.
            let origin = canvas.center() - layout_size * fit * zoom / 2.0 + offset;
            let texel = (pointer - origin) / (fit * zoom);
            let new_origin = pointer - texel * fit * new_zoom;
            offset = new_origin - (canvas.center() - layout_size * fit * new_zoom / 2.0);
            zoom = new_zoom;
            ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
        }
    }
    ui.data_mut(|d| d.insert_temp(view_id, (zoom, offset)));
    let scale = fit * zoom;
    let origin = canvas.center() - layout_size * scale / 2.0 + offset;
    (scale, origin)
}

// --- inspector --------------------------------------------------------------------------------------------------------

// --- references -------------------------------------------------------------------------------------------------------

fn references_ui(ui: &mut egui::Ui, ws: &Workspace, subject: Option<&str>, nav: &mut Option<String>) {
    let Some(asset) = subject.and_then(|s| ws.index.assets.get(s)) else {
        ui.label(RichText::new("Select an asset").color(theme::weak()));
        return;
    };
    let incoming = ws.index.referenced_by.get(&asset.path).map_or(&[][..], Vec::as_slice);
    theme::section(ui, &format!("Referenced by ({})", incoming.len()), |ui| {
        if incoming.is_empty() {
            ui.label(RichText::new("Nothing references this asset").color(theme::weak()));
        }
        for r in incoming {
            let slot = RichText::new(format!("slot {}", r.slot)).color(theme::weak());
            if theme::link_row(ui, &r.from, slot).clicked() {
                *nav = Some(r.from.clone());
            }
        }
    });
    let outgoing: Vec<(String, &Asset)> =
        asset.fields().iter().flat_map(|(key, value)| outgoing_refs(ws, key, value)).collect();
    theme::section(ui, &format!("References ({})", outgoing.len()), |ui| {
        if outgoing.is_empty() {
            ui.label(RichText::new("This asset references nothing").color(theme::weak()));
        }
        for (slot, target) in &outgoing {
            let slot = RichText::new(format!("slot {slot}")).color(theme::weak());
            if theme::link_row(ui, &target.path, slot).clicked() {
                *nav = Some(target.path.clone());
            }
        }
    });
}

/// The assets a field references, with the slot (or field) each sits in.
fn outgoing_refs<'a>(ws: &'a Workspace, field: &str, value: &toml::Value) -> Vec<(String, &'a Asset)> {
    let mut out = Vec::new();
    match value {
        _ if project::index::reference(value).is_some() => {
            if let Some(asset) = ws.target(value) {
                out.push((field.to_string(), asset));
            }
        }
        toml::Value::Table(t) => {
            let name = t.get("name").and_then(toml::Value::as_str);
            for (k, v) in t {
                out.extend(outgoing_refs(ws, name.unwrap_or(k), v));
            }
        }
        toml::Value::Array(a) => a.iter().for_each(|v| out.extend(outgoing_refs(ws, field, v))),
        _ => {}
    }
    out
}

// --- problems ---------------------------------------------------------------------------------------------------------

/// What the index finds wrong, each with the assets it is about; the references that resolve to nothing; whether the
/// layers are watched and agents can connect.
fn problems_ui(ui: &mut egui::Ui, ws: &Workspace, nav: &mut Option<String>) {
    if let Some(e) = &ws.watch_error {
        theme::section(ui, "Watching", |ui| {
            ui.label(RichText::new(e).color(ui.visuals().warn_fg_color));
        });
    }
    if let Some(why) = ws.unprotected() {
        theme::section(ui, "Project lock", |ui| {
            ui.label(RichText::new(why).color(ui.visuals().warn_fg_color));
        });
    }
    if let Some(e) = &ws.link_error {
        theme::section(ui, "Agents", |ui| {
            ui.label(RichText::new(e).color(ui.visuals().warn_fg_color));
        });
    }
    if !ws.extensions.problems.is_empty() {
        theme::section(ui, "Extensions", |ui| {
            for problem in &ws.extensions.problems {
                ui.label(RichText::new(problem).color(ui.visuals().error_fg_color));
            }
        });
    }
    let problems = &ws.index.problems;
    theme::section(ui, &format!("Problems ({})", problems.len()), |ui| {
        if problems.is_empty() {
            ui.label(RichText::new("Nothing is wrong").color(theme::weak()));
        }
        for problem in problems {
            ui.horizontal_wrapped(|ui| {
                let tint = if problem.is_warning() { ui.visuals().warn_fg_color } else { ui.visuals().error_fg_color };
                ui.label(RichText::new(icons::WARNING).color(tint));
                for path in problem.paths() {
                    let asset = path.strip_suffix(project::index::BUNDLE_SUFFIX);
                    if theme::short_link(ui, path).clicked()
                        && let Some(asset) = asset
                    {
                        *nav = Some(asset.to_string());
                    }
                }
                ui.label(RichText::new(problem.describe()).color(theme::weak()));
            });
        }
    });
    let dangling = &ws.index.dangling;
    theme::section(ui, &format!("Dangling references ({})", dangling.len()), |ui| {
        if dangling.is_empty() {
            ui.label(RichText::new("Every reference resolves").color(theme::weak()));
        }
        for r in dangling {
            let slot = RichText::new(format!("slot {}", r.slot)).color(theme::weak());
            if theme::link_row(ui, &r.from, slot).clicked() {
                *nav = Some(r.from.clone());
            }
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                theme::short(ui, RichText::new(format!("→ {} ({})", r.target, r.uid)).small().color(theme::weak()));
            });
        }
    });
}

// --- project ----------------------------------------------------------------------------------------------------------

fn project_ui(ui: &mut egui::Ui, ws: &Workspace, nav: &mut Option<String>) {
    let p = &ws.project;
    ui.label(
        RichText::new(
            p.root.file_name().map_or_else(|| p.root.display().to_string(), |n| n.to_string_lossy().into_owned()),
        )
        .heading(),
    );
    ui.label(RichText::new(p.root.display().to_string()).small().color(theme::weak()));
    theme::section(ui, "Assets", |ui| {
        let mut kinds = std::collections::BTreeMap::<&str, usize>::new();
        for a in ws.index.assets.values() {
            *kinds.entry(a.kind.as_deref().unwrap_or("-")).or_default() += 1;
        }
        egui::Grid::new("kinds").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
            for (kind, count) in kinds {
                ui.label(kind);
                ui.label(RichText::new(count.to_string()).color(theme::weak()));
                ui.end_row();
            }
        });
    });
    if let Some(game) = &p.game {
        theme::section(ui, "Releases", |ui| {
            for (release, discs) in &game.releases {
                ui.label(RichText::new(release).strong());
                for d in discs {
                    ui.label(RichText::new(format!("{}  ·  {}", d.name, size_text(d.size))).color(theme::weak()));
                }
            }
        });
    }
    theme::section(ui, "Extensions", |ui| {
        for extension in &ws.extensions.loaded {
            ui.horizontal(|ui| {
                ui.label(RichText::new(&extension.id).strong());
                ui.label(RichText::new(&extension.version).color(theme::weak()));
                match &extension.origin {
                    project::ext::Origin::Builtin => {
                        ui.label(RichText::new("built in").small().color(theme::weak()));
                    }
                    project::ext::Origin::Linked => {
                        ui.label(RichText::new("linked in").small().color(ui.visuals().warn_fg_color))
                            .on_hover_text("PE2_EXTENSIONS=linked: its crate runs in the editor, not its component");
                    }
                    project::ext::Origin::Component(path) => {
                        theme::short(ui, RichText::new(path.display().to_string()).small().color(theme::weak()));
                    }
                }
            });
        }
    });
    theme::section(ui, "Layers", |ui| {
        for layer in &p.layers {
            ui.horizontal(|ui| {
                ui.label(RichText::new(&layer.name).strong());
                theme::short(ui, RichText::new(layer.path.display().to_string()).color(theme::weak()));
            });
        }
    });
    theme::section(ui, "Mounts", |ui| {
        for m in &p.mounts {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("res://{}", m.at)).strong());
                if m.read_only {
                    badge(ui, "read only");
                }
                theme::short(ui, RichText::new(m.path.display().to_string()).color(theme::weak()));
            });
        }
    });
    if !ws.index.dangling.is_empty() {
        theme::section(ui, &format!("Dangling references ({})", ws.index.dangling.len()), |ui| {
            for r in &ws.index.dangling {
                let detail = RichText::new(format!("{} → {}", r.slot, r.target)).color(ui.visuals().error_fg_color);
                if theme::link_row(ui, &r.from, detail).clicked() {
                    *nav = Some(r.from.clone());
                }
            }
        });
    }
}

// --- settings ---------------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsPage {
    Appearance,
    Tools,
    Startup,
    Agents,
    ProjectGeneral,
    ProjectReleases,
    ProjectLayers,
    ProjectConfigurations,
}

impl SettingsPage {
    fn title(self) -> &'static str {
        match self {
            SettingsPage::Appearance => "Appearance",
            SettingsPage::Tools => "External tools",
            SettingsPage::Startup => "Startup",
            SettingsPage::Agents => "Agents",
            SettingsPage::ProjectGeneral => "General",
            SettingsPage::ProjectReleases => "Releases",
            SettingsPage::ProjectLayers => "Layers and mounts",
            SettingsPage::ProjectConfigurations => "Configurations",
        }
    }
}

/// The editor's settings (global, per user) and the open project's (its `project.toml`): categories on the left, the
/// chosen one's form on the right.
fn settings_ui(ui: &mut egui::Ui, settings: &mut EditorSettings, ws: Option<&Workspace>, message: &mut Option<String>) {
    let id = egui::Id::unique("settings page");
    let mut page = ui.data(|d| d.get_temp::<u8>(id)).map_or(SettingsPage::Appearance, |i| PAGES[usize::from(i)]);
    egui::Panel::left("settings categories").resizable(false).default_size(170.0).show(ui, |ui| {
        ui.add_space(4.0);
        ui.label(RichText::new("EDITOR").small().strong().color(theme::weak()));
        for p in &PAGES[..4] {
            if ui.selectable_label(page == *p, p.title()).clicked() {
                page = *p;
            }
        }
        ui.add_space(10.0);
        ui.label(RichText::new("PROJECT").small().strong().color(theme::weak()));
        for p in &PAGES[4..] {
            if ui.add_enabled(ws.is_some(), egui::Button::selectable(page == *p, p.title())).clicked() {
                page = *p;
            }
        }
    });
    if let Some(index) = PAGES.iter().position(|p| *p == page) {
        ui.data_mut(|d| d.insert_temp(id, u8::try_from(index).unwrap_or(0)));
    }
    egui::CentralPanel::no_frame().show(ui, |ui| {
        egui::Frame::new().inner_margin(egui::Margin { left: 14, ..Default::default() }).show(ui, |ui| {
            scroll(ui, "settings page", |ui| {
                ui.label(RichText::new(page.title()).heading());
                match (page, ws) {
                    (SettingsPage::Appearance, _) => appearance_ui(ui, settings),
                    (SettingsPage::Tools, _) => tools_ui(ui, &mut settings.tools),
                    (SettingsPage::Startup, _) => {
                        theme::section(ui, "When the editor starts", |ui| {
                            ui.checkbox(&mut settings.startup.reopen_last_project, "Reopen the last project");
                            if let Some(last) = &settings.last_project {
                                ui.label(RichText::new(last.display().to_string()).small().color(theme::weak()));
                            }
                        });
                    }
                    (SettingsPage::Agents, _) => {
                        theme::section(ui, "Agents connected to the project", |ui| {
                            ui.label(
                                RichText::new(
                                    "They read the project, and the editor's window: its widgets and how it looks.",
                                )
                                .color(theme::weak()),
                            );
                            ui.checkbox(&mut settings.agents.drive, "Let them use the editor: click, type, press keys");
                            ui.label(
                                RichText::new(
                                    "What they edit goes through the history like your own edits, and can be undone. \
                                     The status bar says when they may, and when one is.",
                                )
                                .small()
                                .color(theme::weak()),
                            );
                        });
                    }
                    (_, Some(ws)) => project_settings_ui(ui, page, ws, &settings.tools, message),
                    (_, None) => {
                        ui.label(RichText::new("No project open").color(theme::weak()));
                    }
                }
                if let Some(path) = crate::settings::path() {
                    ui.add_space(12.0);
                    ui.label(
                        RichText::new(format!("Editor settings are saved to {}", path.display()))
                            .small()
                            .color(theme::weak()),
                    );
                }
            });
        });
    });
}

const PAGES: [SettingsPage; 8] = [
    SettingsPage::Appearance,
    SettingsPage::Tools,
    SettingsPage::Startup,
    SettingsPage::Agents,
    SettingsPage::ProjectGeneral,
    SettingsPage::ProjectReleases,
    SettingsPage::ProjectLayers,
    SettingsPage::ProjectConfigurations,
];

fn appearance_ui(ui: &mut egui::Ui, settings: &mut EditorSettings) {
    let appearance = &mut settings.appearance;
    theme::section(ui, "Theme", |ui| {
        ui.horizontal(|ui| {
            ui.radio_value(&mut appearance.theme, Theme::Dark, "Dark");
            ui.radio_value(&mut appearance.theme, Theme::Light, "Light");
        });
        ui.horizontal(|ui| {
            ui.label("Accent color");
            ui.color_edit_button_srgb(&mut appearance.accent);
            if ui.small_button("Default").clicked() {
                appearance.accent = crate::settings::Appearance::default().accent;
            }
        });
    });
    theme::section(ui, "Preview", |ui| {
        ui.horizontal(|ui| {
            ui.label("Upscaling");
            upscale_picker(ui, &mut settings.preview.upscale);
        });
        ui.label(
            RichText::new("Nearest shows the pixels exactly; the others smooth them for viewing only.")
                .small()
                .color(theme::weak()),
        );
    });
    let appearance = &mut settings.appearance;
    theme::section(ui, "Zoom", |ui| {
        let mut automatic = appearance.zoom.is_none();
        if ui.checkbox(&mut automatic, "Follow the monitor (1080 lines = 100%)").changed() {
            appearance.zoom = if automatic { None } else { Some(ui.ctx().zoom_factor()) };
        }
        if let Some(zoom) = &mut appearance.zoom {
            ui.add(
                egui::Slider::new(zoom, 0.5..=4.0).step_by(0.05).custom_formatter(|z, _| format!("{:.0}%", z * 100.0)),
            );
        }
        ui.label(RichText::new("Ctrl + / Ctrl − / Ctrl 0 change it from anywhere.").small().color(theme::weak()));
    });
}

fn tools_ui(ui: &mut egui::Ui, tools: &mut Tools) {
    ui.label(
        RichText::new("The command each kind of file opens with. {file} stands for the file; an empty command uses the system's default application.")
            .color(theme::weak()),
    );
    theme::section(ui, "Applications", |ui| {
        egui::Grid::new("tools").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
            for category in Category::ALL {
                ui.label(category.label());
                ui.add(
                    egui::TextEdit::singleline(category.command_mut(tools))
                        .hint_text(format!("system default - e.g. {}", category.examples()))
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();
            }
        });
    });
}

fn project_settings_ui(
    ui: &mut egui::Ui,
    page: SettingsPage,
    ws: &Workspace,
    tools: &Tools,
    message: &mut Option<String>,
) {
    let p = &ws.project;
    let file = p.root.join(project::FILE);
    ui.horizontal(|ui| {
        if ui.small_button(format!("{}  Open project.toml", icons::ARROW_SQUARE_OUT)).clicked() {
            *message = Some(external::open(&file, "project.toml", tools).unwrap_or_else(|e| e));
        }
        theme::short(ui, RichText::new(file.display().to_string()).small().color(theme::weak()));
    });
    let raw: toml::Table =
        std::fs::read_to_string(&file).ok().and_then(|t| toml::from_str(&t).ok()).unwrap_or_default();
    let grid = |ui: &mut egui::Ui, id: &str, rows: Vec<(String, String)>| {
        egui::Grid::new(id).num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
            for (key, value) in &rows {
                ui.label(RichText::new(key).color(theme::weak()));
                // The values (paths) go below the grid where they would not fit beside it.
                ui.label(RichText::new(if value.len() > 48 { "" } else { value.as_str() }).monospace());
                ui.end_row();
            }
        });
        for (key, value) in rows.iter().filter(|(_, v)| v.len() > 48) {
            ui.label(RichText::new(key).small().color(theme::weak()));
            theme::short(ui, RichText::new(value).monospace());
        }
    };
    match page {
        SettingsPage::ProjectGeneral => match &p.game {
            Some(game) => theme::section(ui, "Game", |ui| {
                grid(
                    ui,
                    "game",
                    vec![
                        ("game layer".into(), game.layer.clone()),
                        ("disc images".into(), game.roms.display().to_string()),
                        ("declarations".into(), game.declarations.clone()),
                    ],
                );
            }),
            None => {
                ui.label(RichText::new("This project builds on no game ([game] is absent)").color(theme::weak()));
            }
        },
        SettingsPage::ProjectReleases => {
            for (release, discs) in p.game.iter().flat_map(|g| g.releases.iter()) {
                theme::section(ui, release, |ui| {
                    egui::Grid::new(("discs", release)).num_columns(3).spacing([16.0, 4.0]).striped(true).show(
                        ui,
                        |ui| {
                            for d in discs {
                                ui.monospace(&d.name);
                                ui.label(RichText::new(size_text(d.size)).color(theme::weak()));
                                ui.label(RichText::new(&d.sha1).monospace().small().color(theme::weak()));
                                ui.end_row();
                            }
                        },
                    );
                });
            }
        }
        SettingsPage::ProjectLayers => {
            theme::section(ui, "Layers, bottom to top", |ui| {
                grid(ui, "layers", p.layers.iter().map(|l| (l.name.clone(), l.path.display().to_string())).collect());
                ui.label(RichText::new("The project's own directory is the top layer.").small().color(theme::weak()));
            });
            theme::section(ui, "Mounts", |ui| {
                let rows = p
                    .mounts
                    .iter()
                    .map(|m| {
                        let access = if m.read_only { "  (read only)" } else { "" };
                        (format!("res://{}", m.at), format!("{}{access}", m.path.display()))
                    })
                    .collect();
                grid(ui, "mounts", rows);
            });
        }
        SettingsPage::ProjectConfigurations => {
            let configs = raw.get("config").and_then(toml::Value::as_table);
            if configs.is_none_or(toml::Table::is_empty) {
                ui.label(RichText::new("No configurations ([config.<name>])").color(theme::weak()));
            }
            for (name, keys) in configs.into_iter().flatten() {
                theme::section(ui, name, |ui| {
                    let rows = keys
                        .as_table()
                        .into_iter()
                        .flatten()
                        .map(|(k, v)| (k.clone(), v.as_str().map_or_else(|| v.to_string(), str::to_string)))
                        .collect();
                    grid(ui, &format!("config {name}"), rows);
                });
            }
        }
        _ => {}
    }
}
