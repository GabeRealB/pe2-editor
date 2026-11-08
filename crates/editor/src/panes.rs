//! The editor's panes - assets, preview, inspector, references, project - and how the tile layout shows them: each
//! a tab that can be dragged into another group or split off, resized, and closed (closing hides it; the View menu
//! brings it back).

use eframe::egui::{self, Color32, RichText};
use egui_tiles::{Tile, TileId, Tiles, UiResponse};
use project::ext::Extensions;
use project::index::Asset;
use serde::{Deserialize, Serialize};

use crate::dnd::{self, Dragged};
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
    /// The tree inside what is shown.
    Contents,
    /// A tab of the centre showing one entry: the Preview pane as layouts saved it.
    Document,
    Inspector,
    References,
    Problems,
    Project,
    Settings,
    Layers,
    Layer,
    History,
    Extensions,
    /// The tests of what the project has.
    Tests,
    Events,
    Panel(PanelName),
}

/// A pane kind as layouts saved it: the editor's VRAM pane, before it was an extension's panel, read as that panel.
#[derive(Deserialize)]
enum StoredKind {
    Assets,
    Contents,
    Preview,
    Document,
    Inspector,
    References,
    Problems,
    Project,
    Settings,
    Vram,
    Layers,
    Layer,
    History,
    Extensions,
    Events,
    Tests,
    Panel(PanelName),
}

impl From<StoredKind> for PaneKind {
    fn from(stored: StoredKind) -> PaneKind {
        match stored {
            StoredKind::Assets => PaneKind::Assets,
            StoredKind::Contents => PaneKind::Contents,
            StoredKind::Preview | StoredKind::Document => PaneKind::Document,
            StoredKind::Inspector => PaneKind::Inspector,
            StoredKind::References => PaneKind::References,
            StoredKind::Problems => PaneKind::Problems,
            StoredKind::Project => PaneKind::Project,
            StoredKind::Settings => PaneKind::Settings,
            StoredKind::Vram => PaneKind::Panel(PanelName::new("pe2.images@vram")),
            StoredKind::Layers => PaneKind::Layers,
            StoredKind::Layer => PaneKind::Layer,
            StoredKind::History => PaneKind::History,
            StoredKind::Extensions => PaneKind::Extensions,
            StoredKind::Events => PaneKind::Events,
            StoredKind::Tests => PaneKind::Tests,
            StoredKind::Panel(name) => PaneKind::Panel(name),
        }
    }
}

impl PaneKind {
    /// The editor's own panes, in the View menu's order; the extensions' panels follow them (`with_panels`).
    pub const ALL: [PaneKind; 14] = [
        PaneKind::Assets,
        PaneKind::Document,
        PaneKind::Contents,
        PaneKind::Layer,
        PaneKind::Inspector,
        PaneKind::References,
        PaneKind::Problems,
        PaneKind::Layers,
        PaneKind::History,
        PaneKind::Project,
        PaneKind::Extensions,
        PaneKind::Tests,
        PaneKind::Events,
        PaneKind::Settings,
    ];

    /// The panes it opens among, where no layout holds it: the area it belongs with. What takes the room a document does
    /// has none, and opens among the documents.
    pub fn home(self) -> &'static [PaneKind] {
        const BAND: &[PaneKind] =
            &[PaneKind::Assets, PaneKind::Layers, PaneKind::History, PaneKind::Problems, PaneKind::Project];
        match self {
            PaneKind::Assets | PaneKind::Layers | PaneKind::History | PaneKind::Problems | PaneKind::Project => BAND,
            PaneKind::Contents => &[PaneKind::Contents],
            PaneKind::Inspector | PaneKind::References => &[PaneKind::Inspector, PaneKind::References],
            _ => &[],
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            PaneKind::Assets => icons::FOLDER,
            PaneKind::Contents => icons::LIST_BULLETS,
            PaneKind::Document => icons::FILE,
            PaneKind::Inspector => icons::FILE_TEXT,
            PaneKind::References => icons::STACK,
            PaneKind::Problems => icons::WARNING,
            PaneKind::Project => icons::HARD_DRIVES,
            PaneKind::Settings => icons::GEAR,
            PaneKind::Layers => icons::STACK_SIMPLE,
            PaneKind::Layer => icons::GIT_DIFF,
            PaneKind::History => icons::CLOCK_COUNTER_CLOCKWISE,
            PaneKind::Extensions => icons::PLUGS_CONNECTED,
            PaneKind::Tests => icons::CHECK_CIRCLE,
            PaneKind::Events => icons::TREE_STRUCTURE,
            PaneKind::Panel(name) => {
                extensions::shared().panel(name.as_str()).and_then(|p| p.icon).map_or(icons::SQUARES_FOUR, icons::glyph)
            }
        }
    }

    /// The editor's own panes, then the extensions' panels.
    pub fn with_panels() -> Vec<PaneKind> {
        let panels = extensions::shared()
            .panels()
            .iter()
            .map(|p| PaneKind::Panel(PanelName::new(p.name.as_str())))
            .collect::<Vec<_>>();
        PaneKind::ALL.into_iter().chain(panels).collect()
    }

    /// Whether it may be open several times, each instance with its own state; the others are singletons.
    pub fn several(self) -> bool {
        matches!(self, PaneKind::Document | PaneKind::Inspector | PaneKind::References | PaneKind::Panel(_))
    }

    /// Whether it shows one asset or entry: the selection, or the one an instance is pinned to.
    pub fn shows_subject(self) -> bool {
        matches!(self, PaneKind::Document | PaneKind::Contents | PaneKind::Inspector | PaneKind::References)
    }

    /// Whether it lays out its own scrolling - a fixed filter over a scrolled tree, a canvas that pans, a sidebar
    /// beside a scrolled page. Every other pane scrolls whole, so none is ever cut off.
    fn scrolls_itself(self) -> bool {
        matches!(
            self,
            PaneKind::Assets
                | PaneKind::Inspector
                | PaneKind::Panel(_)
                | PaneKind::Settings
                | PaneKind::Document
                | PaneKind::Extensions
        )
    }

    pub fn title(self) -> String {
        match self {
            PaneKind::Assets => "Assets",
            PaneKind::Contents => "Contents",
            PaneKind::Document => "Document",
            PaneKind::Inspector => "Inspector",
            PaneKind::References => "References",
            PaneKind::Problems => "Problems",
            PaneKind::Project => "Project",
            PaneKind::Settings => "Settings",
            PaneKind::Layers => "Layers",
            PaneKind::Layer => "Layer",
            PaneKind::History => "History",
            PaneKind::Extensions => "Extensions",
            PaneKind::Tests => "Tests",
            PaneKind::Events => "Events",
            PaneKind::Panel(name) => {
                return extensions::shared()
                    .panel(name.as_str())
                    .map_or_else(|| name.as_str().to_string(), |p| p.label.clone());
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
/// selection - unlinked from it, as an inspector locked to one asset. A document tab always shows its own, and `preview`
/// says the next entry opened replaces it, until it is kept.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneState {
    #[serde(default)]
    pub pinned: Option<String>,
    #[serde(default)]
    pub preview: bool,
    /// The editor a document was switched to; none: the entry's first.
    #[serde(default)]
    pub editor: Option<EditorKind>,
    /// Its asset's uid: what finds it again once it has moved.
    #[serde(default)]
    pub uid: Option<String>,
}

/// How a document shows its entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EditorKind {
    /// Its pictures, or its stream played.
    Preview,
    /// A graph layer of its stack, as boxes and wires.
    Graph,
    /// Its bundle's files, as text or bytes.
    Files,
    /// One an extension gives its type, or a viewer of what it can be looked at as, by name.
    Extension(PanelName),
    /// A preview its stack asks for, by the output's name.
    Tap(PanelName),
}

impl EditorKind {
    /// Its icon on a document of an entry: an extension's editor says its own.
    pub fn icon(self, ws: &Workspace, path: &str) -> &'static str {
        match self {
            EditorKind::Preview => icons::IMAGE,
            EditorKind::Tap(_) => icons::EYE,
            EditorKind::Graph => icons::TREE_STRUCTURE,
            EditorKind::Files => icons::FILE_TEXT,
            EditorKind::Extension(name) => ws
                .index
                .assets
                .get(path)
                .and_then(|asset| {
                    ws.extensions
                        .tabs(asset)
                        .into_iter()
                        .find(|def| def.name == name.as_str())?
                        .tab
                        .as_ref()
                        .map(|tab| icons::glyph(tab.icon))
                })
                .unwrap_or(icons::SQUARES_FOUR),
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            EditorKind::Preview => "Preview",
            EditorKind::Graph => "Graph",
            EditorKind::Files => "Files",
            EditorKind::Extension(name) | EditorKind::Tap(name) => name.as_str(),
        }
    }
}

/// The editors an entry opens in, the one it opens in first. A plain file has its viewer alone.
pub fn editors_of(ws: &Workspace, path: &str) -> Vec<EditorKind> {
    let Some(asset) = ws.index.assets.get(path) else { return vec![] };
    let mut editors = vec![];
    if crate::player::Player::plays(&ws.extensions, asset) || !ws.previews_of(asset).is_empty() {
        editors.push(EditorKind::Preview);
    }
    // What it is looked at and edited in, before what any asset has: a model opens in 3D, not as its bytes.
    for preview in ws.extensions.previews(asset) {
        let editor = match preview.viewer {
            Some(viewer) if !preview.tapped => EditorKind::Extension(PanelName::new(&viewer.name)),
            _ if preview.tapped && !preview.ty.is_stream() => EditorKind::Tap(PanelName::new(&preview.output)),
            _ => continue,
        };
        if !editors.contains(&editor) {
            editors.push(editor);
        }
    }
    editors
        .extend(ws.extensions.editors(asset).into_iter().map(|def| EditorKind::Extension(PanelName::new(&def.name))));
    if !asset.files.is_empty() {
        editors.push(EditorKind::Files);
    }
    if crate::graph_pane::has_graph(asset) {
        editors.push(EditorKind::Graph);
    }
    editors
}

/// A document's asset, followed by its uid: once it has moved, the tab shows it where it went and, the selection having
/// gone with it, selects it there.
fn follow_asset(ws: &Workspace, state: &mut PaneState, nav: &mut Option<String>) {
    let Some(path) = &state.pinned else { return };
    if let Some(asset) = ws.index.assets.get(path) {
        state.uid.clone_from(&asset.uid);
    } else if let Some(moved) = state.uid.as_ref().and_then(|uid| ws.index.by_uid.get(uid)) {
        state.pinned = Some(moved.clone());
        if ws.selected.is_none() {
            *nav = Some(moved.clone());
        }
    }
}

/// Where dropping an asset on a folder moves it: the edit, or why not.
fn move_into(ws: &Workspace, path: &str, folder: &str) -> Result<EditRequest, String> {
    let asset = ws.index.assets.get(path).ok_or("It is gone")?;
    let uid = asset.uid.clone().ok_or("It has no uid to be moved by")?;
    if folder == path || folder.starts_with(&format!("{path}/")) {
        return Err("A folder cannot go inside itself".to_string());
    }
    let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
    if parent == folder {
        return Err("It is in this folder already".to_string());
    }
    let to = format!("{folder}/{name}{}", project::index::BUNDLE_SUFFIX);
    if ws.index.entries.contains_key(&to) {
        return Err(format!("{name} is there already"));
    }
    Ok(EditRequest::RenameTo(uid, to))
}

/// What tells one path from others of the same name: the nearest folder they differ in.
fn telling_apart<'a>(path: &'a str, others: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    let mine: Vec<&str> = path.rsplit('/').collect();
    let depth = others
        .filter(|other| *other != path)
        .filter_map(|other| {
            let theirs: Vec<&str> = other.rsplit('/').collect();
            (theirs[0] == mine[0]).then(|| (1..mine.len()).find(|&at| theirs.get(at) != mine.get(at)))
        })
        .flatten()
        .max()?;
    mine.get(depth).copied()
}

/// A click on an entry of the tree: the entry, and whether it was a double click, which keeps its tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pick {
    pub path: String,
    pub keep: bool,
}

impl Pick {
    fn of(response: &egui::Response, path: &str) -> Pick {
        Pick { path: path.to_string(), keep: response.double_clicked() }
    }
}

/// What a tab asks for, done once every layout is drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneAction {
    /// Another instance of its kind, beside it.
    Another,
    /// Show this entry, whatever is selected.
    Pin(String),
    /// A document tab no entry replaces.
    Keep,
    /// A document shown in this editor.
    Editor(EditorKind),
    /// Follow the selection again.
    Follow,
    /// Into a window of its own.
    ToNewWindow,
    /// Into another window; none: the main window.
    ToWindow(Option<u64>),
    /// Its tab closed: an instance after the first goes, a first instance is hidden in the main window.
    Close,
}

/// A tabs group of panes.
fn group(tiles: &mut Tiles<Pane>, kinds: &[PaneKind]) -> TileId {
    let panes = kinds.iter().map(|kind| tiles.insert_pane(Pane::first(*kind))).collect();
    tiles.insert_tab_tile(panes)
}

/// A row or a column in shares.
fn shared(tiles: &mut Tiles<Pane>, along: egui_tiles::LinearDir, children: &[(TileId, f32)]) -> TileId {
    let ids: Vec<TileId> = children.iter().map(|(tile, _)| *tile).collect();
    let tile = match along {
        egui_tiles::LinearDir::Horizontal => tiles.insert_horizontal_tile(ids),
        egui_tiles::LinearDir::Vertical => tiles.insert_vertical_tile(ids),
    };
    if let Some(Tile::Container(egui_tiles::Container::Linear(linear))) = tiles.get_mut(tile) {
        for (child, share) in children {
            linear.shares.set_share(*child, *share);
        }
    }
    tile
}

/// The shape nearly every editor of this kind has: what is in the document on the left, the documents, the inspector on the
/// right, and a band along the foot for what is wide.
pub fn default_layout() -> egui_tiles::Tree<Pane> {
    use egui_tiles::LinearDir::{Horizontal, Vertical};
    let mut tiles = Tiles::default();
    let contents = group(&mut tiles, &[PaneKind::Contents]);
    let center = group(&mut tiles, &[PaneKind::Document]);
    let inspector = group(&mut tiles, &[PaneKind::Inspector]);
    let row = shared(&mut tiles, Horizontal, &[(contents, 1.0), (center, 3.4), (inspector, 1.5)]);
    let band = group(&mut tiles, &[PaneKind::Assets, PaneKind::Layers, PaneKind::History, PaneKind::Problems]);
    let root = shared(&mut tiles, Vertical, &[(row, 2.6), (band, 1.0)]);
    egui_tiles::Tree::new(egui::Id::unique("pe2_editor_layout"), root, tiles)
}

/// Layers and what the active one changes, beside the documents.
fn changes_layout() -> egui_tiles::Tree<Pane> {
    use egui_tiles::LinearDir::Horizontal;
    let mut tiles = Tiles::default();
    let layers = group(&mut tiles, &[PaneKind::Layers, PaneKind::History]);
    let center = group(&mut tiles, &[PaneKind::Document, PaneKind::Layer]);
    let inspector = group(&mut tiles, &[PaneKind::Inspector]);
    let root = shared(&mut tiles, Horizontal, &[(layers, 1.2), (center, 3.0), (inspector, 1.5)]);
    egui_tiles::Tree::new(egui::Id::unique("pe2_editor_changes_layout"), root, tiles)
}

/// The layout a mode starts from and is reset to.
pub fn preset(name: &str) -> egui_tiles::Tree<Pane> {
    if name == "Changes" { changes_layout() } else { default_layout() }
}

/// The modes there are at first.
pub const MODES: [&str; 2] = ["Edit", "Changes"];

/// What the layout needs to draw the panes: the open project, and where a click asks to go - `nav` for a link
/// (the asset tree then reveals the asset), `pick` for a click in the tree itself (which must not scroll it).
pub struct Panes<'a> {
    pub workspace: Option<&'a mut Workspace>,
    pub nav: Option<String>,
    pub pick: Option<Pick>,
    /// The document tab used, by a click on it or in it.
    pub focus: Option<Pane>,
    /// A tab just brought to the front: its strip scrolls to it.
    pub reveal: Option<Pane>,
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
    pub history: Option<crate::history_pane::HistoryAsk>,
    /// An edit asked for in a pane.
    pub edit: Option<EditRequest>,
    /// A line for the status bar: what an action did, or why it failed.
    pub message: Option<String>,
    /// A project being opened, as far as it is.
    pub opening: Option<OpeningShown>,
}

/// How far opening a project is, as the panes show it.
#[derive(Clone)]
pub struct OpeningShown {
    pub name: String,
    pub stage: String,
    /// Done of what the stage counts, where it counts.
    pub fraction: Option<f32>,
}

/// The project being opened, once over the window: its name, the stage, a bar where the stage counts, else a spinner.
pub fn opening_ui(ctx: &egui::Context, opening: &OpeningShown) {
    egui::Area::new(egui::Id::unique("opening"))
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).inner_margin(egui::Margin::symmetric(20, 16)).show(ui, |ui| {
                ui.set_width(320.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icons::FOLDER_OPEN).size(18.0).color(theme::accent()));
                    ui.label(RichText::new(format!("Opening {}", opening.name)).strong());
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if opening.fraction.is_none() {
                        ui.add(egui::Spinner::new().size(12.0));
                    }
                    ui.label(RichText::new(&opening.stage).color(theme::weak()));
                });
                if let Some(fraction) = opening.fraction {
                    ui.add_space(6.0);
                    ui.add(egui::ProgressBar::new(fraction).desired_height(6.0));
                }
            });
        });
}

fn waiting_ui(ui: &mut egui::Ui) -> UiResponse {
    ui.centered_and_justified(|ui| ui.add(egui::Spinner::new().size(16.0)));
    UiResponse::None
}

fn no_project_ui(ui: &mut egui::Ui, opening: bool) {
    if !opening {
        ui.centered_and_justified(|ui| {
            ui.label(RichText::new("No project open - File, Open project").color(theme::weak()))
        });
    }
}

impl egui_tiles::Behavior<Pane> for Panes<'_> {
    fn pane_ui(&mut self, ui: &mut egui::Ui, _tile_id: TileId, pane: &mut Pane) -> UiResponse {
        let pane = *pane;
        let number = if pane.instance > 0 { format!(" {}", pane.instance + 1) } else { String::new() };
        crate::drive::note_pane(ui, format!("{}{number}", pane.kind.title()));
        if pane.kind == PaneKind::Document
            && ui.input(|i| i.pointer.any_pressed())
            && ui.rect_contains_pointer(ui.max_rect())
        {
            self.focus = Some(pane);
        }
        if pane.kind == PaneKind::Settings {
            egui::Frame::new().inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                settings_ui(ui, self.settings, self.workspace.as_deref(), &mut self.message);
            });
            return UiResponse::None;
        }
        let Some(ws) = self.workspace.as_deref_mut() else {
            // While a project opens, the panes wait blank.
            no_project_ui(ui, self.opening.is_some());
            return UiResponse::None;
        };
        // What a document shows is what importers made: it waits for that, as the panes beside it need not.
        if pane.kind == PaneKind::Document && !ws.attached() {
            return waiting_ui(ui);
        }
        if pane.kind == PaneKind::Document {
            // What it shows is found again where it went, and something can be dropped on it to open it.
            if let Some(state) = self.states.get_mut(&pane) {
                follow_asset(ws, state, &mut self.nav);
            }
            let verdict = |dragged: &Dragged| match dragged {
                Dragged::Asset(_) => Ok("Open it in a tab".to_string()),
                Dragged::Layer(..) | Dragged::Row { .. } => Err("A layer opens with its asset".to_string()),
            };
            if let Some(Dragged::Asset(path)) = dnd::target(ui, ui.max_rect(), verdict) {
                self.focus = Some(pane);
                self.pick = Some(Pick { path, keep: true });
            }
        }
        // An instance pinned to an entry shows it; the others show the selection.
        let pinned = self.states.get(&pane).and_then(|s| s.pinned.clone());
        let editor = self.states.get(&pane).and_then(|s| s.editor);
        let subject = pinned.clone().or_else(|| ws.selected.clone().filter(|_| pane.kind != PaneKind::Document));
        let (nav, pick, settings, layers, message, history, edit) = (
            &mut self.nav,
            &mut self.pick,
            &mut *self.settings,
            &mut *self.layers,
            &mut self.message,
            &mut self.history,
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
                    PaneKind::Contents => crate::contents::contents_ui(ui, ws, subject, nav, message, edit),
                    PaneKind::Document => {
                        document_ui(ui, ws, pane, (editor, subject), &mut settings.preview, nav, edit);
                    }
                    PaneKind::Panel(name) => {
                        ws.show_panel(ui, egui::Id::unique(pane), name.as_str(), settings.preview, nav);
                    }
                    PaneKind::Inspector => {
                        crate::inspector::inspector_ui(ui, ws, subject, nav, &settings.tools, message, edit);
                    }
                    PaneKind::References => references_ui(ui, ws, subject, nav),
                    PaneKind::Problems => problems_ui(ui, ws, nav, edit),
                    PaneKind::Project => project_ui(ui, ws, nav),
                    PaneKind::Layers => crate::layers::layers_ui(ui, ws, layers, message),
                    PaneKind::Layer => crate::layers::layer_ui(ui, ws, nav, edit),
                    PaneKind::History => crate::history_pane::history_ui(ui, ws, history),
                    PaneKind::Events => crate::events_pane::events_ui(ui, ws, nav, message),
                    PaneKind::Tests => crate::tests_pane::tests_ui(ui, ws),
                    PaneKind::Extensions => {
                        crate::extensions_pane::extensions_ui(ui, ws, &mut settings.extensions, message);
                    }
                    PaneKind::Settings => {}
                }
            });
        };
        if pane.kind.scrolls_itself() {
            contents(ui);
        } else {
            // Both ways: what does not fit is reached, never cut off.
            egui::ScrollArea::both().id_salt(pane).auto_shrink(false).show(ui, contents);
        }
        UiResponse::None
    }

    fn tab_title_for_pane(&mut self, pane: &Pane) -> egui::WidgetText {
        if pane.kind == PaneKind::Document
            && let Some(state) = self.states.get(pane)
            && let Some(path) = &state.pinned
        {
            let name = path.trim_end_matches(project::index::BUNDLE_SUFFIX).rsplit('/').next().unwrap_or(path);
            let asset = self.workspace.as_ref().and_then(|ws| Some((ws, ws.index.assets.get(path.as_str())?)));
            let icon = asset
                .map_or(icons::FILE, |(ws, asset)| icons::for_type(&ws.extensions, ws.extensions.type_name(asset)).0);
            // As its graph where it is shown as one: the view asked for, where the asset has it.
            let graph = state.editor == Some(EditorKind::Graph)
                && asset.is_some_and(|(_, asset)| crate::graph_pane::has_graph(asset));
            let icon = if graph { icons::TREE_STRUCTURE } else { icon };
            let others = self
                .states
                .iter()
                .filter(|(p, _)| p.kind == PaneKind::Document)
                .filter_map(|(_, s)| s.pinned.as_deref());
            // A star: unsaved changes.
            let unsaved = if self.workspace.as_ref().is_some_and(|ws| ws.unsaved(path)) { " *" } else { "" };
            let text = match telling_apart(path, others) {
                Some(folder) => RichText::new(format!("{icon}  {name}{unsaved}  ·  {folder}")),
                None => RichText::new(format!("{icon}  {name}{unsaved}")),
            };
            return if state.preview { text.italics() } else { text }.into();
        }
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

    /// At the end of a strip, the editors of the document in front, where it has more than one.
    fn top_bar_right_ui(
        &mut self,
        tiles: &Tiles<Pane>,
        ui: &mut egui::Ui,
        _tile_id: TileId,
        tabs: &egui_tiles::Tabs,
        _scroll_offset: &mut f32,
    ) {
        let Some(active) = tabs.active else { return };
        let Some(Tile::Pane(pane)) = tiles.get(active) else { return };
        let Some(ws) = self.workspace.as_deref() else { return };
        let Some(state) = self.states.get(pane).filter(|_| pane.kind == PaneKind::Document) else { return };
        let Some(path) = &state.pinned else { return };
        let editors = editors_of(ws, path);
        if editors.len() < 2 {
            return;
        }
        let current = state.editor.filter(|e| editors.contains(e)).unwrap_or(editors[0]);
        // The strip's end is laid out from the right.
        for editor in editors.into_iter().rev() {
            let button = egui::Button::selectable(editor == current, RichText::new(editor.icon(ws, path)));
            if ui.add(button).on_hover_text(editor.title()).clicked() {
                self.actions.push((active, PaneAction::Editor(editor)));
                self.focus = Some(*pane);
            }
        }
    }

    /// A tab's menu: another instance, pinning, moving between windows.
    fn on_tab_button(&mut self, tiles: &mut Tiles<Pane>, tile_id: TileId, response: egui::Response) -> egui::Response {
        let Some(Tile::Pane(pane)) = tiles.get(tile_id) else { return response };
        let pane = *pane;
        response.context_menu(|ui| self.tab_menu(ui, tile_id, pane));
        if self.reveal == Some(pane) {
            response.scroll_to_me(None);
        }
        if response.middle_clicked() {
            self.actions.push((tile_id, PaneAction::Close));
        }
        if pane.kind != PaneKind::Document {
            return response;
        }
        if response.clicked() {
            self.focus = Some(pane);
        }
        if response.double_clicked() {
            self.actions.push((tile_id, PaneAction::Keep));
        }
        match self.states.get(&pane).and_then(|s| s.pinned.clone()) {
            Some(path) => response.on_hover_text(path),
            None => response,
        }
    }

    fn simplification_options(&self) -> egui_tiles::SimplificationOptions {
        egui_tiles::SimplificationOptions { all_panes_must_have_tabs: true, ..Default::default() }
    }

    fn tab_bar_height(&self, _style: &egui::Style) -> f32 {
        24.0
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
        if pane.kind == PaneKind::Document {
            let previewed = self.states.get(&pane).is_some_and(|s| s.preview);
            if previewed
                && ui
                    .button(format!("{}  Keep open", icons::PUSH_PIN))
                    .on_hover_text("The next asset opened takes another tab")
                    .clicked()
            {
                ask(PaneAction::Keep);
            }
        } else if pane.kind.shows_subject() {
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
/// What does not fit is reached, never cut off: both ways, but for what wraps to its width.
fn scroll(ui: &mut egui::Ui, id: &str, add_contents: impl FnOnce(&mut egui::Ui)) {
    egui::ScrollArea::both().id_salt(id).auto_shrink(false).show(ui, add_contents);
}

// --- assets -----------------------------------------------------------------------------------------------------------

fn assets_ui(ui: &mut egui::Ui, ws: &mut Workspace, pick: &mut Option<Pick>, edit: &mut Option<EditRequest>) {
    // Where there is more width than height a tree wastes it, and a row of height is what there is least of: folders, what is
    // in the one chosen as a grid, and the filter on the row of the way there.
    let wide = ui.available_width() > 2.0 * ui.available_height();
    let mut opened = None;
    if wide {
        browser_bar(ui, ws, &mut opened);
    } else {
        ui.add(filter_field(&mut ws.filter));
    }
    ui.add_space(4.0);
    let filter = ws.filter.trim().to_lowercase();
    if filter.is_empty() && wide {
        browser_ui(ui, ws, pick, edit, opened);
        return;
    }
    answer_browser(ws, opened);
    let mut chosen = None;
    scroll(ui, "assets", |ui| {
        if filter.is_empty() {
            let view = TreeView {
                ws,
                reveal: ws.reveal.as_deref(),
                edit: std::cell::RefCell::new(None),
                folders: None,
                follow: false,
            };
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

/// The field narrowing the assets.
fn filter_field(filter: &mut String) -> egui::TextEdit<'_> {
    egui::TextEdit::singleline(filter)
        .hint_text(format!("{}  Filter by path or type", icons::MAGNIFYING_GLASS))
        .desired_width(f32::INFINITY)
}

/// The folders down one side and what is in the one chosen as a grid: the assets where they have the width for it.
fn browser_ui(
    ui: &mut egui::Ui,
    ws: &mut Workspace,
    pick: &mut Option<Pick>,
    edit: &mut Option<EditRequest>,
    mut opened: Option<Opened>,
) {
    let reveal = ws.reveal.take();
    if let Some((folder, _)) = reveal.as_deref().and_then(|path| path.rsplit_once('/')) {
        ws.open_folder(folder.to_string());
    }
    let follow = std::mem::take(&mut ws.folder_moved) || reveal.is_some();
    ws.in_browser |= ui.rect_contains_pointer(ui.max_rect());
    let mut chosen = None;
    let id = ui.scope_id().with("asset browser");
    let shown = &*ws;
    egui::Panel::left(id).resizable(true).default_size(230.0).frame(egui::Frame::new()).show(ui, |ui| {
        ui.set_min_size(ui.available_size());
        scroll(ui, "asset folders", |ui| {
            let folders = Some(std::cell::RefCell::new(None));
            // The folder the grid went to, as what reveals it.
            let going = format!("{}/", shown.asset_folder);
            let reveal = reveal.as_deref().or(follow.then_some(going.as_str()));
            let view = TreeView { ws: shown, reveal, edit: std::cell::RefCell::new(None), folders, follow };
            view.ui(ui, &shown.tree, "", pick, &mut chosen);
            *edit = view.edit.into_inner().or(edit.take());
            opened = view.folders.and_then(std::cell::RefCell::into_inner).map(Opened::Folder).or(opened.take());
        });
    });
    egui::CentralPanel::no_frame().show(ui, |ui| {
        let rect = ui.max_rect();
        let grid = egui::ScrollArea::vertical().id_salt("asset grid").auto_shrink(false);
        grid.show(ui, |ui| assets_grid_ui(ui, shown, pick, edit, &mut opened));
        if let Some(request) = folder_target(ui, shown, rect, &shown.asset_folder) {
            *edit = Some(request);
        }
    });
    answer_browser(ws, opened);
    if let Some(entry) = chosen {
        ws.choose(&entry);
    }
}

/// Do what was asked of the browser.
fn answer_browser(ws: &mut Workspace, opened: Option<Opened>) {
    match opened {
        Some(Opened::Folder(folder)) => ws.open_folder(folder),
        Some(Opened::Marked(folder)) => ws.marked_folder = Some(folder),
        Some(Opened::Back) => ws.folder_back(),
        Some(Opened::Forward) => ws.folder_forward(),
        Some(Opened::Up) => ws.folder_up(),
        None => {}
    }
}

/// What was asked of the browser while it was drawn.
enum Opened {
    Folder(String),
    /// A folder marked by a click, not opened.
    Marked(String),
    Back,
    Forward,
    Up,
}

/// Back, forward and up, where the browser is, each step of it a way there, and the filter.
fn browser_bar(ui: &mut egui::Ui, ws: &mut Workspace, opened: &mut Option<Opened>) {
    let folder = ws.asset_folder.clone();
    let (back, forward) = (ws.can_folder_back(), ws.can_folder_forward());
    ui.horizontal(|ui| {
        let button = |ui: &mut egui::Ui, enabled: bool, icon: &str, hint: &str| {
            ui.add_enabled(enabled, egui::Button::new(icon).frame(false)).on_hover_text(hint).clicked()
        };
        if button(ui, back, icons::CARET_LEFT, "Back") {
            *opened = Some(Opened::Back);
        }
        if button(ui, forward, icons::CARET_RIGHT, "Forward") {
            *opened = Some(Opened::Forward);
        }
        if button(ui, !folder.is_empty(), icons::CARET_UP, "Up a folder") {
            *opened = Some(Opened::Up);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let width = (ui.available_width() * 0.35).clamp(120.0, 260.0);
            ui.add_sized([width, ui.spacing().interact_size.y], filter_field(&mut ws.filter));
            ui.add_space(6.0);
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                let step = |ui: &mut egui::Ui, name: &str| {
                    let text = RichText::new(name).small().color(theme::weak());
                    let label = egui::Label::new(text).sense(egui::Sense::click()).selectable(false);
                    ui.add(label).on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
                };
                let mut steps = vec!["The project"];
                steps.extend(folder.split('/').filter(|part| !part.is_empty()));
                let first = theme::fitting_steps(ui, &steps, ui.available_width());
                if first > 0 {
                    ui.label(RichText::new("…").small().color(theme::weak()));
                }
                for (at, name) in steps.iter().enumerate().skip(first) {
                    if at > first {
                        ui.label(RichText::new("/").small().color(theme::weak()));
                    }
                    if step(ui, name) {
                        *opened = Some(Opened::Folder(steps[1..=at].join("/")));
                    }
                }
            });
        });
    });
}

/// What is in the folder chosen, as a grid: a picture and a name each, folders opening.
fn assets_grid_ui(
    ui: &mut egui::Ui,
    ws: &Workspace,
    pick: &mut Option<Pick>,
    edit: &mut Option<EditRequest>,
    opened: &mut Option<Opened>,
) {
    let folder = ws.asset_folder.as_str();
    let Some(node) = ws.tree.at(folder) else { return };
    ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
    ui.horizontal_wrapped(|ui| {
        for (name, child) in node.sorted_children() {
            if child.entry.is_some() {
                continue;
            }
            let path = if folder.is_empty() { name.clone() } else { format!("{folder}/{name}") };
            let is_folder = !child.children.is_empty();
            let marked = ws.marked_folder.as_deref() == Some(&path);
            let response = grid_cell(ui, ws, name, child, marked);
            if let Some(asset) = child.asset.as_deref() {
                response.dnd_set_drag_payload(Dragged::Asset(asset.to_string()));
            }
            // A click chooses, as anywhere; a double click goes in, or keeps a tab open.
            if response.clicked() {
                let entry = child.asset.as_deref().or(child.path.as_deref().filter(|_| !is_folder));
                *pick =
                    entry.map(|path| Pick { path: path.to_string(), keep: response.double_clicked() && !is_folder });
                if is_folder && entry.is_none() {
                    *opened = Some(Opened::Marked(path.clone()));
                }
            }
            if is_folder && response.double_clicked() {
                *opened = Some(Opened::Folder(path.clone()));
            }
            if !child.children.is_empty()
                && let Some(request) = folder_target(ui, ws, response.rect, &path)
            {
                *edit = Some(request);
            }
            if let Some(real) = child.path.as_deref() {
                response.context_menu(|ui| {
                    if let Some(request) = entry_menu(ui, ws, real) {
                        *edit = Some(request);
                        ui.close();
                    }
                });
            }
        }
    });
}

/// An entry of the grid: its thumbnail, or its type's icon, over its name.
fn grid_cell(ui: &mut egui::Ui, ws: &Workspace, name: &str, node: &Node, marked: bool) -> egui::Response {
    const CELL: egui::Vec2 = egui::vec2(96.0, 88.0);
    const PICTURE: f32 = 60.0;
    let (rect, response) = ui.allocate_exact_size(CELL, egui::Sense::click_and_drag());
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let asset = node.asset.as_deref().and_then(|path| ws.index.assets.get(path));
    let selected = marked || (node.asset.is_some() && ws.selected == node.asset);
    let visuals = ui.style().interact_selectable(&response, selected);
    if selected || response.hovered() {
        ui.painter().rect_filled(rect, 4.0, visuals.weak_bg_fill);
    }
    let picture = egui::Rect::from_center_size(
        rect.center_top() + egui::vec2(0.0, 6.0 + PICTURE / 2.0),
        egui::Vec2::splat(PICTURE),
    );
    let thumbnail = asset
        .filter(|a| ws.attached() && ws.extensions.has_thumbnail(a))
        .and_then(|a| ws.thumbs.get(ui.ctx(), &ws.extensions, a));
    if let Some(texture) = thumbnail {
        let image = egui::Image::new((texture, egui::Vec2::splat(PICTURE))).maintain_aspect_ratio(true);
        image.paint_at(ui, picture);
    } else {
        let (icon, tint) = match asset {
            Some(asset) => icons::for_type(&ws.extensions, ws.extensions.type_name(asset)),
            None if node.children.is_empty() => (icons::FILE, theme::weak()),
            None => (icons::FOLDER, theme::weak()),
        };
        let font = egui::FontId::proportional(30.0);
        ui.painter().text(picture.center(), egui::Align2::CENTER_CENTER, icon, font, tint);
    }
    // A star: unsaved changes.
    let unsaved = node.asset.as_deref().is_some_and(|path| ws.unsaved(path));
    let mut job = egui::text::LayoutJob::simple_singleline(
        if unsaved { format!("{name} *") } else { name.to_string() },
        egui::TextStyle::Small.resolve(ui.style()),
        theme::text(),
    );
    job.wrap = egui::text::TextWrapping::truncate_at_width(CELL.x - 8.0);
    let galley = ui.painter().layout_job(job);
    let at = egui::pos2(rect.center().x - galley.size().x / 2.0, rect.bottom() - galley.size().y - 4.0);
    ui.painter().galley(at, galley, theme::text());
    response.on_hover_text(name)
}

/// A folder, what is dropped on it moving into it; the edit, once it is.
fn folder_target(ui: &egui::Ui, ws: &Workspace, rect: egui::Rect, folder: &str) -> Option<EditRequest> {
    let name = folder.rsplit('/').next().unwrap_or(folder);
    let verdict = |dragged: &Dragged| match dragged {
        Dragged::Asset(path) => move_into(ws, path, folder).map(|_| format!("Move into {name}")),
        Dragged::Layer(..) | Dragged::Row { .. } => Err("It stays in its asset".to_string()),
    };
    match dnd::target(ui, rect, verdict) {
        Some(Dragged::Asset(path)) => move_into(ws, &path, folder).ok(),
        _ => None,
    }
}

/// The asset tree as drawn: the workspace for kinds and the selection, and an asset to reveal once.
struct TreeView<'a> {
    ws: &'a Workspace,
    reveal: Option<&'a str>,
    /// An edit an entry's menu asked for.
    edit: std::cell::RefCell<Option<EditRequest>>,
    /// Beside a grid, folders only: the one opened, once one is.
    folders: Option<std::cell::RefCell<Option<String>>>,
    /// Whether the grid went to another folder: the tree goes there too, this once.
    follow: bool,
}

impl TreeView<'_> {
    fn ui(&self, ui: &mut egui::Ui, node: &Node, prefix: &str, pick: &mut Option<Pick>, chosen: &mut Option<Entry>) {
        for (name, child) in node.sorted_children() {
            let path = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
            if child.children.is_empty() {
                if self.folders.is_some() {
                    continue;
                }
                if let Some(entry) = &child.entry {
                    self.entry_label(ui, name, entry, chosen);
                } else if child.asset.is_some() {
                    ui.horizontal(|ui| self.asset_label(ui, name, child, pick));
                } else {
                    self.file_label(ui, name, child, pick);
                }
                continue;
            }
            // Beside a grid, what is in a folder is the grid's: one with no folder in it has nothing to unfold.
            if self.folders.is_some() && !child.children.values().any(|inner| !inner.children.is_empty()) {
                self.folder_leaf(ui, name, child, &path);
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
                    if let Some(response) = label.as_ref().filter(|_| child.entry.is_none()) {
                        self.folder_row(ui, response, &path);
                    }
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
                        let response = self.folder_label(ui, icon, name, &path);
                        // Beside a grid a click shows the folder there; its arrow folds it.
                        if response.clicked() {
                            set_open = Some(self.folders.is_some() || !open);
                        }
                        self.folder_row(ui, &response, &path);
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

    /// A folder's name: beside a grid, the one the grid shows is marked.
    fn folder_label(&self, ui: &mut egui::Ui, icon: &str, name: &str, path: &str) -> egui::Response {
        let strength = if self.ws.is_editable(path) { 1.0 } else { DIMMED };
        let text = RichText::new(format!("{icon}  {name}")).color(theme::weak().gamma_multiply(strength));
        if self.folders.is_some() {
            let shown = self.ws.asset_folder == path;
            let text = if shown { text.color(theme::text()) } else { text };
            let response = ui.add(egui::Button::selectable(shown, text).truncate());
            if shown && self.follow {
                response.scroll_to_me(Some(egui::Align::Center));
            }
            return response;
        }
        let label = egui::Label::new(text).sense(egui::Sense::click()).selectable(false);
        ui.add(label).on_hover_cursor(egui::CursorIcon::PointingHand)
    }

    /// A folder with nothing to unfold, beside a grid, in line with the names of those that have.
    fn folder_leaf(&self, ui: &mut egui::Ui, name: &str, node: &Node, path: &str) {
        ui.horizontal(|ui| {
            ui.add_space(ui.spacing().icon_width);
            let response = self.folder_label(ui, icons::FOLDER, name, path);
            self.folder_row(ui, &response, path);
            self.menu(&response, node);
        });
    }

    /// A folder's row: what is dropped on it moves into it, and beside a grid a click opens it there.
    fn folder_row(&self, ui: &egui::Ui, response: &egui::Response, folder: &str) {
        if let Some(request) = folder_target(ui, self.ws, response.rect, folder) {
            *self.edit.borrow_mut() = Some(request);
        }
        if let Some(opened) = self.folders.as_ref().filter(|_| response.clicked()) {
            *opened.borrow_mut() = Some(folder.to_string());
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
        pick: &mut Option<Pick>,
    ) -> Option<egui::Response> {
        let asset = node.asset.as_deref()?;
        let found = self.ws.index.assets.get(asset);
        let kind = found.and_then(|a| self.ws.extensions.type_name(a));
        // A star: unsaved changes.
        let starred = self.ws.unsaved(asset).then(|| format!("{name} *"));
        let name = starred.as_deref().unwrap_or(name);
        let dim = !self.ws.is_editable(asset);
        let selected = self.ws.selected.as_deref() == Some(asset);
        // An asset whose type makes thumbnails shows its own in place of its type's icon, once it is made.
        let thumbnail = found
            .filter(|a| self.ws.attached() && self.ws.extensions.has_thumbnail(a))
            .and_then(|a| self.ws.thumbs.get(ui.ctx(), &self.ws.extensions, a));
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
        let response = ui.add(button.truncate().sense(egui::Sense::click_and_drag()));
        response.dnd_set_drag_payload(Dragged::Asset(asset.to_string()));
        if self.reveal == Some(asset) {
            response.scroll_to_me(Some(egui::Align::Center));
        }
        if response.clicked() {
            *pick = Some(Pick::of(&response, asset));
        }
        self.menu(&response, node);
        self.badge(ui, node);
        Some(response)
    }

    /// A plain file, or a folder with nothing in it: selected by its real path.
    fn file_label(&self, ui: &mut egui::Ui, name: &str, node: &Node, pick: &mut Option<Pick>) {
        let Some(path) = node.path.as_deref() else { return };
        let icon = if node.file { icons::FILE } else { icons::FOLDER };
        let job = icon_and_name(ui.style(), icon, theme::weak(), theme::text(), name, !self.ws.is_editable(path));
        ui.horizontal(|ui| {
            let response = ui.add(egui::Button::selectable(self.ws.selected.as_deref() == Some(path), job).truncate());
            if self.reveal == Some(path) {
                response.scroll_to_me(Some(egui::Align::Center));
            }
            if response.clicked() {
                *pick = Some(Pick::of(&response, path));
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

/// A name after its type's icon, the icon in the type's tint; dimmed where it cannot be modified.
fn labelled(
    extensions: &Extensions,
    style: &egui::Style,
    kind: Option<&str>,
    name: &str,
    dim: bool,
) -> egui::text::LayoutJob {
    let (icon, tint) = icons::for_type(extensions, kind);
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

fn filtered_ui(ui: &mut egui::Ui, ws: &Workspace, filter: &str, pick: &mut Option<Pick>) {
    const SHOWN: usize = 500;
    let matches: Vec<&Asset> = ws
        .index
        .assets
        .values()
        .map(|a| &**a)
        .filter(|a| a.path.to_lowercase().contains(filter) || ws.extensions.type_label(a) == Some(filter))
        .collect();
    ui.label(RichText::new(format!("{} matches", matches.len())).small().color(theme::weak()));
    for asset in matches.iter().take(SHOWN) {
        let selected = ws.selected.as_deref() == Some(asset.path.as_str());
        let job = labelled(
            &ws.extensions,
            ui.style(),
            ws.extensions.type_name(asset),
            &asset.path,
            !ws.is_editable(&asset.path),
        );
        let response = ui.add(egui::Button::selectable(selected, job).truncate().sense(egui::Sense::click_and_drag()));
        response.dnd_set_drag_payload(Dragged::Asset(asset.path.clone()));
        if response.clicked() {
            *pick = Some(Pick::of(&response, &asset.path));
        }
    }
    if matches.len() > SHOWN {
        ui.label(RichText::new(format!("… {} more", matches.len() - SHOWN)).color(theme::weak()));
    }
}

// --- preview ----------------------------------------------------------------------------------------------------------

/// A document's body: its subject in the editor chosen for it where it can show it, else in the first that can, else
/// its preview.
fn document_ui(
    ui: &mut egui::Ui,
    ws: &mut Workspace,
    pane: Pane,
    (editor, subject): (Option<EditorKind>, Option<&str>),
    preview: &mut Preview,
    nav: &mut Option<String>,
    edit: &mut Option<EditRequest>,
) {
    let editors = subject.map_or_default(|path| editors_of(ws, path));
    match editor.filter(|e| editors.contains(e)).or(editors.first().copied()) {
        Some(EditorKind::Graph) => crate::graph_pane::graph_ui(ui, ws, subject, edit),
        Some(EditorKind::Files) => {
            if let Some(asset) = subject.and_then(|path| ws.index.assets.get(path)).cloned() {
                files_view_ui(ui, ws, &asset);
            }
        }
        Some(EditorKind::Extension(name)) => {
            if let Some(path) = subject {
                ws.show_editor(ui, egui::Id::unique(pane), path, name.as_str(), *preview, nav);
            }
        }
        Some(EditorKind::Tap(output)) => {
            ws.show_tap(ui, egui::Id::unique(pane), subject, output.as_str(), *preview, nav);
        }
        Some(EditorKind::Preview) | None => preview_ui(ui, ws, pane, subject, nav, preview),
    }
}

fn preview_ui(
    ui: &mut egui::Ui,
    ws: &mut Workspace,
    pane: Pane,
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
    // One that plays, in the player.
    if crate::player::Player::plays(&ws.extensions, asset) {
        let path = asset.path.clone();
        ws.play(ui, pane, &path, (view, nav));
        return;
    }
    let shown = ws.previews_of(asset);
    if shown.is_empty() {
        files_view_ui(ui, ws, &asset.clone());
        return;
    }
    scroll(ui, "preview", |ui| views_ui(ui, ws, egui::Id::unique(pane), nav, view, shown));
}

/// An asset without a picture: one of its files at a time, as text or bytes - its native form first, by default.
fn files_view_ui(ui: &mut egui::Ui, ws: &mut Workspace, asset: &Asset) {
    if asset.files.is_empty() {
        ui.label(RichText::new("Nothing to preview for this asset").color(theme::weak()));
        return;
    }
    let default = asset.files.iter().find(|f| f.name != project::index::NODE_FILE).unwrap_or(&asset.files[0]);
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

/// An asset's views, by its type's extension: its own, or those of the assets its slots hold, each under its look
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
        ws.show_look(path, variants[choice].clone());
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
    /// Polygons over the pictures.
    pub traces: Vec<pe2_ext::Traced>,
}

/// Whether a point is inside a polygon, its corners in rim order.
fn inside(corners: &[(f32, f32)], (x, y): (f32, f32)) -> bool {
    let mut odd = false;
    for (at, &(ax, ay)) in corners.iter().enumerate() {
        let (bx, by) = corners[(at + 1) % corners.len()];
        if (ay > y) != (by > y) && x < ax + (y - ay) / (by - ay) * (bx - ax) {
            odd = !odd;
        }
    }
    odd
}

/// The polygon at a point: the last drawn, what is selected being drawn over the rest.
fn trace_at(traces: &[pe2_ext::Traced], at: (f32, f32)) -> Option<usize> {
    let under = |strong: bool| traces.iter().rposition(|trace| trace.strong == strong && inside(&trace.corners, at));
    under(true).or_else(|| under(false))
}

/// The polygons of an overlay: thin over what they lie on, what is selected filled, so that the pixels it covers show;
/// the one under the pointer on top.
fn traces_ui(
    painter: &egui::Painter,
    traces: &[pe2_ext::Traced],
    hovered: Option<usize>,
    origin: egui::Pos2,
    scale: f32,
) {
    let points = |trace: &pe2_ext::Traced| {
        trace.corners.iter().map(|&(x, y)| origin + egui::vec2(x, y) * scale).collect::<Vec<_>>()
    };
    let thin = egui::Stroke::new(1.0, Color32::from_white_alpha(110));
    for trace in traces.iter().filter(|trace| !trace.strong) {
        painter.add(egui::Shape::closed_line(points(trace), thin));
    }
    let strong = egui::Stroke::new(1.5, theme::accent());
    for trace in traces.iter().filter(|trace| trace.strong) {
        painter.add(egui::Shape::convex_polygon(points(trace), theme::accent().gamma_multiply(0.3), strong));
    }
    if let Some(trace) = hovered.and_then(|at| traces.get(at)) {
        painter.add(egui::Shape::convex_polygon(
            points(trace),
            Color32::from_white_alpha(50),
            egui::Stroke::new(2.0, Color32::WHITE),
        ));
    }
}

/// An overlay's summary, and the toggle showing its boxes where it has one and boxes to show; without one, always shown.
fn boxes_toggle(ui: &mut egui::Ui, overlay: &Overlay, show_boxes: &mut bool) {
    if overlay.toggle.is_empty() {
        *show_boxes = true;
        if !overlay.summary.is_empty() {
            ui.label(RichText::new(&overlay.summary).small().color(theme::weak()));
        }
    } else if !overlay.boxes.is_empty() {
        ui.horizontal(|ui| {
            ui.checkbox(show_boxes, &overlay.toggle);
            ui.label(RichText::new(&overlay.summary).small().color(theme::weak()));
        });
    }
}

/// An overlay drawn: each picture, a box over each named rectangle, the legend; hovering an entry of either highlights
/// it in both, hovering the canvas names what is under the pointer. Returns the link of a picture clicked.
pub fn overlay_ui(ui: &mut egui::Ui, overlay: &Overlay, view: Preview, count: usize) -> Option<String> {
    let (path, boxes, layout_size) = (overlay.id.as_str(), &overlay.boxes, overlay.size);
    let plain = theme::weak().gamma_multiply(0.6);
    let color = |i: usize, highlight: bool| {
        if overlay.legend {
            box_color(i)
        } else if highlight {
            theme::accent()
        } else {
            plain
        }
    };
    let shown_id = egui::Id::unique(("overlay boxes", path));
    let hover_id = egui::Id::unique(("overlay hovered", path));
    let mut show_boxes = ui.data(|d| d.get_temp::<bool>(shown_id)).unwrap_or(true);
    let hovered: Option<usize> = ui.data(|d| d.get_temp(hover_id)).flatten();
    let mut now_hovered = None;
    boxes_toggle(ui, overlay, &mut show_boxes);
    ui.data_mut(|d| d.insert_temp(shown_id, show_boxes));
    let legend = overlay.legend && show_boxes && !boxes.is_empty();
    #[expect(clippy::cast_precision_loss, reason = "a handful of pictures")]
    let height = (ui.available_height() * if legend { 0.7 } else { 1.0 } - 40.0).max(80.0) / count.max(1) as f32;
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
    let pointed = response.hover_pos().map(|pointer| ((pointer - origin) / scale).to_pos2());
    let trace = pointed.and_then(|at| trace_at(&overlay.traces, (at.x, at.y)));
    traces_ui(&painter, &overlay.traces, trace, origin, scale);
    if show_boxes {
        let font = egui::TextStyle::Small.resolve(ui.style());
        for (i, (name, rect, highlight)) in boxes.iter().enumerate() {
            let named = overlay.legend.then_some(name.as_str());
            box_ui(
                &painter,
                &font,
                (to_screen(*rect), color(i, *highlight)),
                named,
                (hovered == Some(i) || *highlight, hovered == Some(i)),
            );
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
        lines.extend(trace.and_then(|trace| overlay.traces[trace].label.clone()));
        if show_boxes {
            lines.extend(under.iter().map(|&i| boxes[i].0.clone()));
        }
        if response.clicked() {
            // What is there to go to, else what is there to select: `#` and a box's name.
            let part = under.first().filter(|_| show_boxes).map(|&i| format!("#{}", boxes[i].0));
            let face = trace.and_then(|trace| overlay.traces[trace].link.clone());
            clicked = face.or_else(|| pictures.iter().find_map(|(_, _, _, link)| link.clone())).or(part);
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

/// A box of an overlay drawn: its outline, broad where `strong`, and its name (where given) on a dark band in its
/// corner, where the box is large enough to hold it or it is hovered.
fn box_ui(
    painter: &egui::Painter,
    font: &egui::FontId,
    (screen, color): (egui::Rect, Color32),
    name: Option<&str>,
    (strong, hovered): (bool, bool),
) {
    let width = if strong { if name.is_some() { 3.0 } else { 2.0 } } else { 1.0 };
    painter.rect_stroke(screen, 0.0, egui::Stroke::new(width, color), egui::StrokeKind::Inside);
    let Some(name) = name else { return };
    let galley = painter.layout_no_wrap(name.to_string(), font.clone(), Color32::WHITE);
    let band = egui::Rect::from_min_size(screen.min, galley.size() + egui::vec2(6.0, 2.0));
    if band.width() <= screen.width() && band.height() <= screen.height() || hovered {
        painter.rect_filled(band, 2.0, Color32::from_black_alpha(170));
        painter.galley(band.min + egui::vec2(3.0, 1.0), galley, Color32::WHITE);
    }
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
fn legend_ui(
    ui: &mut egui::Ui,
    path: &str,
    boxes: &[(String, egui::Rect, bool)],
    hovered: Option<usize>,
) -> Option<usize> {
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

pub(crate) fn references_ui(ui: &mut egui::Ui, ws: &Workspace, subject: Option<&str>, nav: &mut Option<String>) {
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
            // The output it names, where it names one.
            let output = r.output.as_ref().map_or_default(|o| format!("  #{o}"));
            let slot = RichText::new(format!("slot {}{output}", r.slot)).color(theme::weak());
            if theme::link_row(ui, &r.from, slot).clicked() {
                *nav = Some(r.from.clone());
            }
        }
    });
    // As the declaration makes them, each once: its fields hold the settings both on their layers and lifted out.
    let outgoing: Vec<(String, &Asset)> = ws
        .index
        .references_from(&asset.path)
        .into_iter()
        .filter_map(|r| Some((r.slot, ws.index.resolve(&r.uid, &r.target)?)))
        .collect();
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

// --- problems ---------------------------------------------------------------------------------------------------------

/// What the last build found wrong, where it did: its wiring, what failed and what is stale, each a link to where.
fn build_problems_ui(ui: &mut egui::Ui, ws: &Workspace, nav: &mut Option<String>) {
    if let Some(epoch) = ws.last_epoch.as_ref().filter(|epoch| epoch.is_wrong()) {
        theme::section(ui, &format!("Build  ·  {}", epoch.config), |ui| {
            let wiring = epoch.problems.iter().map(|(at, why)| (at, why.clone(), ui.visuals().error_fg_color));
            let failed = epoch.report.failed.iter().map(|(at, why)| (at, why.clone(), ui.visuals().error_fg_color));
            let stale_why = "is stale: what it reads changed after it ran";
            let stale = epoch
                .report
                .stale
                .iter()
                .map(|(at, output)| (at, format!("{output} {stale_why}"), ui.visuals().warn_fg_color));
            let shown: Vec<_> = wiring.chain(failed).chain(stale).collect();
            for (at, why, tint) in shown {
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(icons::WARNING).color(tint));
                        if ui.link(at.as_str()).clicked() {
                            *nav = Some(at.clone());
                        }
                    });
                    ui.label(RichText::new(why).monospace().color(theme::weak()));
                });
            }
        });
    }
}

/// What the index finds wrong, each with the assets it is about; the references that resolve to nothing; whether the
/// layers are watched and agents can connect.
fn problems_ui(ui: &mut egui::Ui, ws: &Workspace, nav: &mut Option<String>, edit: &mut Option<EditRequest>) {
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
    if !ws.gpu_errors.is_empty() {
        theme::section(ui, "GPU", |ui| {
            for error in &ws.gpu_errors {
                ui.label(RichText::new(error).monospace().color(ui.visuals().error_fg_color));
            }
        });
    }
    if !ws.extensions.problems.is_empty() {
        theme::section(ui, "Extensions", |ui| {
            for problem in &ws.extensions.problems {
                ui.label(RichText::new(problem).color(ui.visuals().error_fg_color));
            }
        });
    }
    build_problems_ui(ui, ws, nav);
    let problems = &ws.index.problems;
    theme::section(ui, &format!("Problems ({})", problems.len()), |ui| {
        if problems.is_empty() {
            ui.label(RichText::new("Nothing is wrong").color(theme::weak()));
        }
        for problem in problems {
            // What it is about on one line each, its icon before the first, and what is wrong under them.
            ui.vertical(|ui| {
                let tint = if problem.is_warning() { ui.visuals().warn_fg_color } else { ui.visuals().error_fg_color };
                for (at, path) in problem.paths().iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(icons::WARNING).color(if at == 0 {
                            tint
                        } else {
                            Color32::TRANSPARENT
                        }));
                        let asset = path.strip_suffix(project::index::BUNDLE_SUFFIX);
                        if theme::short_link(ui, path).clicked()
                            && let Some(asset) = asset
                        {
                            *nav = Some(asset.to_string());
                        }
                    });
                }
                ui.label(RichText::new(problem.describe()).color(theme::weak()));
                // An entry gone from a layer: left listed until it is said to be gone, as its path may come back. Only the
                // list is written, so a read-only mount forgets too.
                if let project::index::Problem::Missing { layer, path, .. } = problem
                    && ws.layers.iter().any(|l| l.name == *layer && l.writable)
                    && let Some(uid) = ws.index.listed(layer, path)
                    && ui
                        .small_button("Forget it")
                        .on_hover_text("Take it out of the layer's list (an undoable step)")
                        .clicked()
                {
                    *edit = Some(EditRequest::Forget(layer.clone(), uid.to_string()));
                }
                ui.add_space(4.0);
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

/// The extensions loaded: each one's name, version, where it comes from and the memory its instances hold.
fn project_extensions_ui(ui: &mut egui::Ui, ws: &Workspace) {
    theme::section(ui, "Extensions", |ui| {
        let memory = ws.extensions.memory();
        // A column each - name, version, origin, memory - aligned across the rows.
        egui::Grid::new("project extensions").num_columns(4).spacing([14.0, 4.0]).show(ui, |ui| {
            for extension in &ws.extensions.loaded {
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
                    project::ext::Origin::Project(folder) => {
                        ui.label(RichText::new("project").small().color(theme::weak()))
                            .on_hover_text(folder.display().to_string());
                    }
                    project::ext::Origin::User(path)
                    | project::ext::Origin::Install(path)
                    | project::ext::Origin::Component(path) => {
                        let text = path.file_name().map_or_default(|n| n.to_string_lossy().into_owned());
                        ui.label(RichText::new(text).small().color(theme::weak()))
                            .on_hover_text(path.display().to_string());
                    }
                }
                // What its instances hold now: a guest's memory only grows, until its instance is reclaimed.
                match memory.iter().find(|(id, _)| *id == extension.id).filter(|(_, m)| m.instances > 0) {
                    Some((_, m)) => {
                        let mib = |bytes: usize| bytes.div_ceil(1 << 20);
                        let s = if m.instances == 1 { "" } else { "s" };
                        ui.label(RichText::new(format!("{} MiB", mib(m.bytes))).small().color(theme::weak()))
                            .on_hover_text(format!(
                                "Held by {} instance{s}; the most one has held: {} MiB",
                                m.instances,
                                mib(m.peak)
                            ));
                    }
                    None => {
                        ui.label("");
                    }
                }
                ui.end_row();
            }
        });
    });
}

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
        let mut types = std::collections::BTreeMap::<&str, usize>::new();
        for a in ws.index.assets.values() {
            *types.entry(ws.extensions.type_label(a).unwrap_or("-")).or_default() += 1;
        }
        egui::Grid::new("types").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
            for (node_type, count) in types {
                ui.label(node_type);
                ui.label(RichText::new(count.to_string()).color(theme::weak()));
                ui.end_row();
            }
        });
    });
    let releases: Vec<_> =
        ws.plans.layers.values().filter_map(|p| p.header.as_ref().ok()).flat_map(|h| &h.inputs).collect();
    if !releases.is_empty() {
        theme::section(ui, "Releases", |ui| {
            for (release, input) in releases {
                ui.label(RichText::new(release).strong());
                for (name, size, _) in &input.discs {
                    ui.label(RichText::new(format!("{name}  ·  {}", size_text(*size))).color(theme::weak()));
                }
            }
        });
    }
    project_extensions_ui(ui, ws);
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
    Files,
    Startup,
    Shortcuts,
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
            SettingsPage::Files => "Files",
            SettingsPage::Startup => "Startup",
            SettingsPage::Shortcuts => "Shortcuts",
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
    // The pages take what they can: narrow, this gives way first.
    let categories = (ui.available_width() * 0.34).clamp(120.0, 170.0);
    egui::Panel::left("settings categories").resizable(false).exact_size(categories).show(ui, |ui| {
        ui.add_space(4.0);
        ui.label(RichText::new("EDITOR").small().strong().color(theme::weak()));
        for p in &PAGES[..6] {
            if ui.selectable_label(page == *p, p.title()).clicked() {
                page = *p;
            }
        }
        ui.add_space(10.0);
        ui.label(RichText::new("PROJECT").small().strong().color(theme::weak()));
        for p in &PAGES[6..] {
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
                    (SettingsPage::Appearance, _) => {
                        appearance_ui(ui, settings);
                        theme::section(ui, "Performance", |ui| {
                            ui.checkbox(&mut settings.performance_overlay, "Show what the editor costs, over the window");
                        });
                    }
                    (SettingsPage::Tools, _) => tools_ui(ui, &mut settings.tools),
                    (SettingsPage::Files, _) => {
                        use crate::settings::TakeIn;
                        theme::section(ui, "Changes other programs make", |ui| {
                            let take_in = &mut settings.changes.take_in;
                            ui.radio_value(take_in, TakeIn::OnFocus, "Take them in when the editor regains focus");
                            ui.radio_value(take_in, TakeIn::AsTheyHappen, "Take them in as they happen");
                            ui.label(
                                RichText::new(
                                    "A tool saving often, or a checkout, is taken in once, when you come back to the \
                                     editor. Refresh in the View menu (F5) takes them in at any time; an agent's request always \
                                     does first.",
                                )
                                .small()
                                .color(theme::weak()),
                            );
                        });
                    }
                    (SettingsPage::Shortcuts, _) => crate::commands::shortcuts_ui(ui, settings),
                    (SettingsPage::Startup, _) => {
                        theme::section(ui, "When the editor starts", |ui| {
                            ui.checkbox(&mut settings.startup.reopen_last_project, "Reopen the last project");
                            if let Some(last) = &settings.last_project {
                                ui.label(RichText::new(last.display().to_string()).small().color(theme::weak()));
                            }
                        });
                        if cfg!(target_os = "linux") {
                            display_server_ui(ui, &mut settings.startup.display_server);
                        }
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

const PAGES: [SettingsPage; 10] = [
    SettingsPage::Appearance,
    SettingsPage::Tools,
    SettingsPage::Files,
    SettingsPage::Startup,
    SettingsPage::Shortcuts,
    SettingsPage::Agents,
    SettingsPage::ProjectGeneral,
    SettingsPage::ProjectReleases,
    SettingsPage::ProjectLayers,
    SettingsPage::ProjectConfigurations,
];

fn display_server_ui(ui: &mut egui::Ui, chosen: &mut crate::settings::DisplayServer) {
    theme::section(ui, "Display server", |ui| {
        ui.horizontal(|ui| {
            for server in crate::settings::DisplayServer::ALL {
                ui.radio_value(chosen, server, server.label());
            }
        });
        let note = "Takes effect the next time the editor starts. Auto is X11 under WSL, where Wayland draws faster but \
                    its compositor has crashed on a maximize, taking every Linux window with it; elsewhere, what the \
                    system offers.";
        ui.label(RichText::new(note).small().color(theme::weak()));
        if std::env::var_os("PE2_EDITOR_BACKEND").is_some() {
            ui.label(RichText::new("PE2_EDITOR_BACKEND is set, and overrides this.").small().color(theme::weak()));
        }
    });
}

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
        // Each value beside its name, in the font of the name so the two share a line; a path too long for the pane
        // cut short, the whole of it on hover.
        egui::Grid::new(id).num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
            for (key, value) in &rows {
                ui.label(RichText::new(key).color(theme::weak()));
                theme::short(ui, value.as_str());
                ui.end_row();
            }
        });
    };
    match page {
        SettingsPage::ProjectGeneral => {
            if ws.plans.layers.is_empty() {
                ui.label(RichText::new("No layer of this project is generated by a plan").color(theme::weak()));
            }
            for (name, planned) in &ws.plans.layers {
                theme::section(ui, name, |ui| {
                    let mut rows = vec![("fills".into(), planned.fills.clone()), ("state".into(), ws.plans.said(name))];
                    if let Ok(header) = &planned.header {
                        let needs = header.requires.iter().map(|(id, version)| format!("{id} {version}"));
                        rows.push(("needs".into(), needs.collect::<Vec<_>>().join(", ")));
                    }
                    grid(ui, &format!("plan {name}"), rows);
                });
            }
            if !p.unmanaged.is_empty() {
                theme::section(ui, "Unmanaged mounts", |ui| {
                    let rows = p.unmanaged.iter().map(|m| (format!("res://{}", m.at), m.path.display().to_string()));
                    grid(ui, "unmanaged", rows.collect());
                });
            }
        }
        SettingsPage::ProjectReleases => {
            let headers = ws.plans.layers.values().filter_map(|p| p.header.as_ref().ok());
            for (release, input) in headers.flat_map(|h| &h.inputs) {
                theme::section(ui, release, |ui| {
                    let look_in =
                        p.resolve(&input.look_in).map_or_else(|_| input.look_in.clone(), |at| at.display().to_string());
                    ui.label(RichText::new(format!("looked for in {look_in}")).color(theme::weak()));
                    egui::Grid::new(("discs", release)).num_columns(3).spacing([16.0, 4.0]).striped(true).show(
                        ui,
                        |ui| {
                            for (name, size, sha1) in &input.discs {
                                ui.monospace(name);
                                ui.label(RichText::new(size_text(*size)).color(theme::weak()));
                                ui.label(RichText::new(sha1).monospace().small().color(theme::weak()));
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
                        // Where it is, as the layers above name theirs: resolved, not as project.toml reaches it.
                        let path = std::fs::canonicalize(&m.path).unwrap_or_else(|_| m.path.clone());
                        (format!("res://{}", m.at), format!("{}{access}", path.display()))
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

#[cfg(test)]
mod tests {
    use super::{inside, trace_at};
    use pe2_ext::Traced;

    fn square(left: f32, strong: bool) -> Traced {
        Traced {
            corners: vec![(left, 0.0), (left + 10.0, 0.0), (left + 10.0, 10.0), (left, 10.0)],
            strong,
            label: None,
            link: None,
        }
    }

    #[test]
    fn a_point_is_inside_a_polygon_by_its_rim() {
        let triangle = [(0.0, 0.0), (10.0, 0.0), (0.0, 10.0)];
        assert!(inside(&triangle, (2.0, 2.0)));
        assert!(!inside(&triangle, (8.0, 8.0)), "past the long edge");
    }

    #[test]
    fn what_is_selected_answers_over_what_lies_above_it() {
        let traces = [square(0.0, true), square(0.0, false), square(20.0, false)];
        assert_eq!(trace_at(&traces, (5.0, 5.0)), Some(0));
        assert_eq!(trace_at(&traces, (25.0, 5.0)), Some(2));
        assert_eq!(trace_at(&traces, (15.0, 5.0)), None);
    }

    use super::telling_apart;

    #[test]
    fn tabs_of_one_name_are_told_by_the_folder_they_differ_in() {
        let a = "stages/akropolis/rooms/cafeteria/movies/movie_100";
        let b = "stages/akropolis/rooms/bridge/movies/movie_100";
        assert_eq!(telling_apart(a, [a, b].into_iter()), Some("cafeteria"));
        assert_eq!(telling_apart(b, [a, b].into_iter()), Some("bridge"));
        // Its own, shown twice, and another name say nothing.
        assert_eq!(telling_apart(a, [a, a].into_iter()), None);
        assert_eq!(telling_apart(a, [a, "stages/image"].into_iter()), None);
    }
}
