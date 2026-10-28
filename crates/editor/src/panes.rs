//! The editor's panes - assets, preview, inspector, references, project - and how the tile layout shows them: each
//! a tab that can be dragged into another group or split off, resized, and closed (closing hides it; the View menu
//! brings it back).

use eframe::egui::{self, Color32, RichText};
use egui_tiles::{Tile, TileId, Tiles, UiResponse};
use project::index::Asset;
use serde::{Deserialize, Serialize};

use crate::external::{self, Category};
use crate::inspector::{badge, size_text};
use crate::settings::{EditorSettings, Preview, Theme, Tools, Upscale};
use crate::workspace::{Entry, Node, Workspace, cells_of};
use crate::{icons, theme};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Pane {
    Assets,
    Preview,
    Inspector,
    References,
    Problems,
    Project,
    Settings,
    Vram,
}

impl Pane {
    pub const ALL: [Pane; 8] = [
        Pane::Assets,
        Pane::Preview,
        Pane::Vram,
        Pane::Inspector,
        Pane::References,
        Pane::Problems,
        Pane::Project,
        Pane::Settings,
    ];

    pub fn icon(self) -> &'static str {
        match self {
            Pane::Assets => icons::FOLDER,
            Pane::Preview => icons::IMAGE,
            Pane::Inspector => icons::FILE_TEXT,
            Pane::References => icons::STACK,
            Pane::Problems => icons::WARNING,
            Pane::Project => icons::HARD_DRIVES,
            Pane::Settings => icons::GEAR,
            Pane::Vram => icons::SQUARES_FOUR,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Pane::Assets => "Assets",
            Pane::Preview => "Preview",
            Pane::Inspector => "Inspector",
            Pane::References => "References",
            Pane::Problems => "Problems",
            Pane::Project => "Project",
            Pane::Settings => "Settings",
            Pane::Vram => "VRAM",
        }
    }
}

/// The default layout: assets and project on the left, the preview in the middle, the inspector over the
/// references and problems on the right.
pub fn default_layout() -> egui_tiles::Tree<Pane> {
    let mut tiles = Tiles::default();
    let left = [Pane::Assets, Pane::Project].map(|p| tiles.insert_pane(p));
    let left = tiles.insert_tab_tile(left.to_vec());
    let center = [Pane::Preview, Pane::Vram].map(|p| tiles.insert_pane(p));
    let center = tiles.insert_tab_tile(center.to_vec());
    let inspector = tiles.insert_pane(Pane::Inspector);
    let inspector = tiles.insert_tab_tile(vec![inspector]);
    let references = [Pane::References, Pane::Problems].map(|p| tiles.insert_pane(p));
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

/// The tile of a pane, if the layout has one.
pub fn tile_of(tree: &egui_tiles::Tree<Pane>, pane: Pane) -> Option<TileId> {
    tree.tiles.iter().find_map(|(id, tile)| matches!(tile, Tile::Pane(p) if *p == pane).then_some(*id))
}

/// What the layout needs to draw the panes: the open project, and where a click asks to go - `nav` for a link
/// (the asset tree then reveals the asset), `pick` for a click in the tree itself (which must not scroll it).
pub struct Panes<'a> {
    pub workspace: Option<&'a mut Workspace>,
    pub nav: Option<String>,
    pub pick: Option<String>,
    pub settings: &'a mut EditorSettings,
    /// A line for the status bar: what an action did, or why it failed.
    pub message: Option<String>,
}

impl egui_tiles::Behavior<Pane> for Panes<'_> {
    fn pane_ui(&mut self, ui: &mut egui::Ui, _tile_id: TileId, pane: &mut Pane) -> UiResponse {
        if *pane == Pane::Settings {
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
        egui::Frame::new().inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            let nav = &mut self.nav;
            match pane {
                Pane::Assets => assets_ui(ui, ws, &mut self.pick),
                Pane::Preview => {
                    let view = &mut self.settings.preview;
                    scroll(ui, "preview", |ui| preview_ui(ui, ws, nav, view));
                }
                Pane::Vram => vram_ui(ui, ws, nav, self.settings.preview.upscale),
                Pane::Inspector => {
                    let (tools, message) = (&self.settings.tools, &mut self.message);
                    scroll(ui, "inspector", |ui| crate::inspector::inspector_ui(ui, ws, nav, tools, message));
                }
                Pane::References => scroll(ui, "references", |ui| references_ui(ui, ws, nav)),
                Pane::Problems => scroll(ui, "problems", |ui| problems_ui(ui, ws, nav)),
                Pane::Project => scroll(ui, "project", |ui| project_ui(ui, ws, nav)),
                Pane::Settings => {}
            }
        });
        UiResponse::None
    }

    fn tab_title_for_pane(&mut self, pane: &Pane) -> egui::WidgetText {
        format!("{}  {}", pane.icon(), pane.title()).into()
    }

    fn is_tab_closable(&self, _tiles: &Tiles<Pane>, _tile_id: TileId) -> bool {
        true
    }

    fn on_tab_close(&mut self, tiles: &mut Tiles<Pane>, tile_id: TileId) -> bool {
        tiles.set_visible(tile_id, false);
        false
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

fn scroll(ui: &mut egui::Ui, id: &str, add_contents: impl FnOnce(&mut egui::Ui)) {
    egui::ScrollArea::vertical().id_salt(id).auto_shrink(false).show(ui, add_contents);
}

// --- assets -----------------------------------------------------------------------------------------------------------

fn assets_ui(ui: &mut egui::Ui, ws: &mut Workspace, pick: &mut Option<String>) {
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
            let view = TreeView { ws, reveal: ws.reveal.as_deref() };
            view.ui(ui, &ws.tree, "", pick, &mut chosen);
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
}

impl TreeView<'_> {
    fn ui(&self, ui: &mut egui::Ui, node: &Node, prefix: &str, pick: &mut Option<String>, chosen: &mut Option<Entry>) {
        for (name, child) in node.sorted_children() {
            let path = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
            if child.children.is_empty() {
                if let Some(entry) = &child.entry {
                    self.entry_label(ui, name, entry, chosen);
                } else {
                    self.asset_label(ui, name, child.asset.as_deref(), pick);
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
                        None => self.asset_label(ui, name, child.asset.as_deref(), pick),
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
                        let label = egui::Label::new(RichText::new(format!("{icon}  {name}")).color(theme::weak()))
                            .sense(egui::Sense::click())
                            .selectable(false);
                        if ui.add(label).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                            set_open = Some(!open);
                        }
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
        let (icon, tint) = (icons::PALETTE, theme::weak());
        let mut job = egui::text::LayoutJob::default();
        let font = egui::TextStyle::Body.resolve(ui.style());
        job.append(icon, 0.0, egui::TextFormat::simple(font.clone(), tint));
        job.append(name, 7.0, egui::TextFormat::simple(font, theme::text()));
        let response = ui.selectable_label(self.ws.is_chosen(entry), job);
        if response.clicked() {
            *chosen = Some(entry.clone());
        }
        response
    }

    fn asset_label(
        &self,
        ui: &mut egui::Ui,
        name: &str,
        asset: Option<&str>,
        pick: &mut Option<String>,
    ) -> Option<egui::Response> {
        let asset = asset?;
        let kind = self.ws.index.assets.get(asset).and_then(|a| a.kind.as_deref());
        let response =
            ui.selectable_label(self.ws.selected.as_deref() == Some(asset), labelled(ui.style(), kind, name));
        if self.reveal == Some(asset) {
            response.scroll_to_me(Some(egui::Align::Center));
        }
        if response.clicked() {
            *pick = Some(asset.to_string());
        }
        Some(response)
    }
}

/// A name after its kind's icon, the icon in the kind's tint.
fn labelled(style: &egui::Style, kind: Option<&str>, name: &str) -> egui::text::LayoutJob {
    let (icon, tint) = icons::for_kind(kind);
    let mut job = egui::text::LayoutJob::default();
    let font = egui::TextStyle::Body.resolve(style);
    job.append(icon, 0.0, egui::TextFormat::simple(font.clone(), tint));
    job.append(name, 7.0, egui::TextFormat::simple(font, theme::text()));
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
        if ui.selectable_label(selected, labelled(ui.style(), asset.kind.as_deref(), &asset.path)).clicked() {
            *pick = Some(asset.path.clone());
        }
    }
    if matches.len() > SHOWN {
        ui.label(RichText::new(format!("… {} more", matches.len() - SHOWN)).color(theme::weak()));
    }
}

// --- preview ----------------------------------------------------------------------------------------------------------

fn preview_ui(ui: &mut egui::Ui, ws: &mut Workspace, nav: &mut Option<String>, view: &mut Preview) {
    let Some(asset) = ws.selected_asset() else {
        ui.label(RichText::new("Select an asset").color(theme::weak()));
        return;
    };
    let shown = ws.pictures_of(asset);
    if shown.is_empty() {
        ui.label(RichText::new("Nothing to preview for this asset yet").color(theme::weak()));
        return;
    }
    ui.horizontal(|ui| {
        ui.label(RichText::new("Upscaling").small().color(theme::weak()));
        upscale_picker(ui, &mut view.upscale);
        ui.checkbox(&mut view.grid, "Grid").on_hover_text("A texel grid, its spacing following the zoom");
    });
    let (view, upscale) = (*view, &view.upscale);
    let ctx = ui.ctx().clone();
    let count = shown.len();
    for (slot, path) in shown {
        ui.horizontal(|ui| {
            if let Some(slot) = &slot {
                ui.label(RichText::new(slot).strong());
                if ui.link(&path).clicked() {
                    *nav = Some(path.clone());
                }
            }
            variant_picker(ui, ws, &path);
        });
        let role = ws.index.assets.get(&path).and_then(crate::workspace::role).map(str::to_string);
        let paged = ws.index.assets.get(&path).is_some_and(|a| {
            let (size, page_width) = image_pages(a);
            role.as_deref() == Some("texture") && size.x > page_width
        });
        let size = match ws.picture(&ctx, &path, *upscale) {
            Some(Err(e)) => {
                ui.colored_label(ui.visuals().error_fg_color, format!("No preview: {e}"));
                None
            }
            Some(Ok(shown)) => Some(shown.size),
            None => None,
        };
        if let Some(size) = size {
            if role.as_deref() == Some("atlas") {
                atlas_ui(ui, ws, &path, view, count);
            } else if paged {
                pages_ui(ui, ws, &path, view, count);
            } else {
                let overlay = Overlay {
                    id: path.clone(),
                    size,
                    pictures: vec![(path.clone(), egui::Rect::from_min_size(egui::Pos2::ZERO, size))],
                    boxes: vec![],
                    summary: String::new(),
                    toggle: "",
                };
                overlay_ui(ui, ws, &overlay, view, count);
            }
        }
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

/// An atlas with a box over each cell: drawn, each cell through its own palette.
fn atlas_ui(ui: &mut egui::Ui, ws: &mut Workspace, path: &str, view: Preview, count: usize) {
    let Some(asset) = ws.index.assets.get(path) else { return };
    let (size, _) = image_pages(asset);
    let cells = cells_of(asset);
    let overlay = Overlay {
        id: path.to_string(),
        size,
        pictures: vec![(path.to_string(), egui::Rect::from_min_size(egui::Pos2::ZERO, size))],
        summary: format!("{} cell{}", cells.len(), if cells.len() == 1 { "" } else { "s" }),
        toggle: "Cells",
        boxes: cells.into_iter().map(|c| (c.name, c.rect)).collect(),
    };
    overlay_ui(ui, ws, &overlay, view, count);
}

/// A multi-page image with a box over each page: page 1 is what a model's faces name as the image's second page.
fn pages_ui(ui: &mut egui::Ui, ws: &mut Workspace, path: &str, view: Preview, count: usize) {
    let Some(asset) = ws.index.assets.get(path) else { return };
    let (size, page_width) = image_pages(asset);
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a page count")]
    let pages = (size.x / page_width).ceil() as usize;
    #[expect(clippy::cast_precision_loss, reason = "a page count")]
    let boxes = (0..pages)
        .map(|k| {
            let x = k as f32 * page_width;
            (
                format!("page {k}"),
                egui::Rect::from_min_size(egui::pos2(x, 0.0), egui::vec2((size.x - x).min(page_width), size.y)),
            )
        })
        .collect();
    let overlay = Overlay {
        id: path.to_string(),
        size,
        pictures: vec![(path.to_string(), egui::Rect::from_min_size(egui::Pos2::ZERO, size))],
        summary: format!("{pages} pages of {page_width} texels"),
        toggle: "Pages",
        boxes,
    };
    overlay_ui(ui, ws, &overlay, view, count);
}

/// An image's size in texels and its pages' width (64 halfwords at its depth).
pub fn image_pages(asset: &project::index::Asset) -> (egui::Vec2, f32) {
    let get = |k: &str| asset.fields().get(k);
    let size = get("size").and_then(toml::Value::as_array).cloned().unwrap_or_default();
    #[expect(clippy::cast_precision_loss, reason = "image sizes")]
    let dim = |i: usize| size.get(i).and_then(toml::Value::as_integer).unwrap_or(0) as f32;
    let per = match get("bpp").and_then(toml::Value::as_integer) {
        Some(4) => 4.0,
        Some(8) => 2.0,
        _ => 1.0,
    };
    (egui::vec2(dim(0), dim(1)), 64.0 * per)
}

/// Pictures laid out on a canvas, with named boxes over them and a legend: an atlas's parts and cells, a multi-page
/// image's pages.
struct Overlay {
    id: String,
    size: egui::Vec2,
    pictures: Vec<(String, egui::Rect)>,
    boxes: Vec<(String, egui::Rect)>,
    summary: String,
    toggle: &'static str,
}

/// An overlay drawn: each picture in its chosen variant, a box over each named rectangle with its name where it fits,
/// and the legend; hovering an entry of either highlights it in both.
fn overlay_ui(ui: &mut egui::Ui, ws: &mut Workspace, overlay: &Overlay, view: Preview, count: usize) {
    let upscale = view.upscale;
    let (path, boxes, layout_size) = (overlay.id.as_str(), &overlay.boxes, overlay.size);
    let color = box_color;
    let shown_id = egui::Id::unique(("overlay boxes", path));
    let hover_id = egui::Id::unique(("overlay hovered", path));
    let mut show_boxes = ui.data(|d| d.get_temp::<bool>(shown_id)).unwrap_or(true);
    let hovered: Option<usize> = ui.data(|d| d.get_temp(hover_id)).flatten();
    let mut now_hovered = None;
    if !boxes.is_empty() {
        ui.horizontal(|ui| {
            ui.checkbox(&mut show_boxes, overlay.toggle);
            ui.label(RichText::new(&overlay.summary).small().color(theme::weak()));
        });
    }
    ui.data_mut(|d| d.insert_temp(shown_id, show_boxes));
    let legend = if show_boxes && !boxes.is_empty() { 0.7 } else { 1.0 };
    #[expect(clippy::cast_precision_loss, reason = "a handful of pictures")]
    let height = (ui.available_height() * legend - 40.0).max(160.0) / count.max(1) as f32;
    let ctx = ui.ctx().clone();
    let (canvas, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::click_and_drag());
    let painter = ui.painter_at(canvas);
    let (scale, origin) = zoom_pan(ui, &response, canvas, path, layout_size);
    let to_screen =
        |r: egui::Rect| egui::Rect::from_min_max(origin + r.min.to_vec2() * scale, origin + r.max.to_vec2() * scale);
    painter.rect_filled(canvas, 0.0, ui.visuals().extreme_bg_color);
    let uv = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
    for (piece, rect) in &overlay.pictures {
        if let Some(Ok(shown)) = ws.picture(&ctx, piece, upscale) {
            painter.image(shown.texture.id(), to_screen(*rect), uv, Color32::WHITE);
        }
    }
    if view.grid {
        grid_ui(&painter, to_screen(egui::Rect::from_min_size(egui::Pos2::ZERO, layout_size)), layout_size, scale);
    }
    if show_boxes {
        let font = egui::TextStyle::Small.resolve(ui.style());
        for (i, (name, rect)) in boxes.iter().enumerate() {
            let width = if hovered == Some(i) { 3.0 } else { 1.0 };
            let screen = to_screen(*rect);
            painter.rect_stroke(screen, 0.0, egui::Stroke::new(width, color(i)), egui::StrokeKind::Inside);
            // The name, on a dark band in the box's corner, where the box is large enough to hold it.
            let galley = painter.layout_no_wrap(name.clone(), font.clone(), Color32::WHITE);
            let band = egui::Rect::from_min_size(screen.min, galley.size() + egui::vec2(6.0, 2.0));
            if band.width() <= screen.width() && band.height() <= screen.height() || hovered == Some(i) {
                painter.rect_filled(band, 2.0, Color32::from_black_alpha(170));
                painter.galley(band.min + egui::vec2(3.0, 1.0), galley, Color32::WHITE);
            }
        }
    }
    if let Some(pointer) = response.hover_pos() {
        let at = ((pointer - origin) / scale).to_pos2();
        let under: Vec<usize> = (0..boxes.len()).filter(|&i| boxes[i].1.contains(at)).collect();
        now_hovered = under.first().copied();
        let mut lines = vec![format!("({:.0}, {:.0})", at.x.floor(), at.y.floor())];
        lines.extend(under.iter().map(|&i| boxes[i].0.clone()));
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
    if show_boxes && let Some(i) = legend_ui(ui, path, boxes, hovered) {
        now_hovered = Some(i);
    }
    ui.data_mut(|d| d.insert_temp(hover_id, now_hovered));
}

/// An overlay's legend: each box's color and name in columns; hovering an entry highlights its box. Returns the entry
/// hovered.
fn legend_ui(ui: &mut egui::Ui, path: &str, boxes: &[(String, egui::Rect)], hovered: Option<usize>) -> Option<usize> {
    let color = box_color;
    let mut now_hovered = None;
    ui.add_space(6.0);
    let font = egui::TextStyle::Small.resolve(ui.style());
    let widest = boxes
        .iter()
        .map(|(name, _)| ui.fonts_mut(|f| f.layout_no_wrap(name.clone(), font.clone(), Color32::WHITE).size().x))
        .fold(0.0, f32::max);
    let column = widest + 28.0;
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a column count")]
    let columns = ((ui.available_width() / column).floor() as usize).max(1);
    egui::Grid::new(("atlas legend", path)).min_col_width(column).spacing([4.0, 2.0]).show(ui, |ui| {
        for (i, (name, rect)) in boxes.iter().enumerate() {
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

// --- VRAM -------------------------------------------------------------------------------------------------------------

const VRAM: egui::Vec2 = egui::vec2(1024.0, 512.0);

/// VRAM, 1024 × 512 halfwords: the texture pages' grid, the slots, and what the selected asset uploads - its pages
/// at their places (a 4-bit page a quarter of its texels wide, as VRAM holds it) and its palettes' colors. Hovering
/// names what is there; clicking a texture opens it.
fn vram_ui(ui: &mut egui::Ui, ws: &mut Workspace, nav: &mut Option<String>, upscale: Upscale) {
    let selected = ws.selected_asset().cloned();
    let (pages, palettes) = selected.as_ref().map_or_default(|a| ws.uploads_of(a));
    let slots = ws.vram_slots();
    ui.horizontal(|ui| {
        let what = match &selected {
            Some(a) if !pages.is_empty() || !palettes.is_empty() => {
                format!("{}: {} pages, {} palettes", a.path, pages.len(), palettes.len())
            }
            Some(a) if a.kind.as_deref() == Some("vram_slot") => a.path.clone(),
            _ => "Select a texture, an atlas or a file to see what it uploads".into(),
        };
        ui.label(RichText::new(what).small().color(theme::weak()));
    });
    let scale = (ui.available_width() / VRAM.x).min((ui.available_height() - 4.0) / VRAM.y).max(0.2);
    let (response, painter) = ui.allocate_painter(VRAM * scale, egui::Sense::click());
    let origin = response.rect.min;
    let to_screen =
        |r: egui::Rect| egui::Rect::from_min_max(origin + r.min.to_vec2() * scale, origin + r.max.to_vec2() * scale);
    painter.rect_filled(response.rect, 0.0, ui.visuals().extreme_bg_color);
    let grid = egui::Stroke::new(1.0, theme::border().gamma_multiply(0.5));
    for column in 1..16u8 {
        let x = origin.x + f32::from(column) * 64.0 * scale;
        painter.line_segment([egui::pos2(x, response.rect.top()), egui::pos2(x, response.rect.bottom())], grid);
    }
    let middle = origin.y + 256.0 * scale;
    painter.line_segment([egui::pos2(response.rect.left(), middle), egui::pos2(response.rect.right(), middle)], grid);
    let ctx = ui.ctx().clone();
    for upload in &pages {
        if let Some(Ok(shown)) = ws.picture(&ctx, &upload.image, upscale) {
            let uv = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
            painter.image(shown.texture.id(), to_screen(upload.rect), uv, Color32::WHITE);
        }
    }
    for p in &palettes {
        if let Some(Ok(strip)) = ws.palette(&ctx, &p.image, p.palette) {
            let uv = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
            let mut r = to_screen(p.rect);
            r.max.y = r.max.y.max(r.min.y + 2.0); // a row of colors stays visible at small scales
            painter.image(strip.texture.id(), r, uv, Color32::WHITE);
        }
    }
    let selected_path = selected.as_ref().map(|a| a.path.as_str());
    for (path, rect) in &slots {
        let highlighted = Some(path.as_str()) == selected_path;
        let stroke = if highlighted {
            egui::Stroke::new(2.0, theme::accent())
        } else {
            egui::Stroke::new(1.0, theme::weak().gamma_multiply(0.6))
        };
        painter.rect_stroke(to_screen(*rect), 0.0, stroke, egui::StrokeKind::Inside);
    }
    let Some(hovered) = response.hover_pos() else { return };
    let at = ((hovered - origin) / scale).to_pos2();
    let mut lines = vec![format!("({:.0}, {:.0})", at.x.floor(), at.y.floor())];
    let under: Vec<&String> = pages.iter().filter(|u| u.rect.contains(at)).map(|u| &u.image).collect();
    lines.extend(under.iter().map(|t| format!("{}  {t}", icons::IMAGE)));
    lines.extend(palettes.iter().filter(|p| p.rect.contains(at)).map(|p| {
        let palette = ws.palettes_of(&p.image).get(p.palette).cloned().unwrap_or_default();
        format!("{}  {} · {palette}", icons::PALETTE, p.image)
    }));
    lines.extend(slots.iter().filter(|(_, r)| r.contains(at)).map(|(p, _)| format!("{}  {p}", icons::SQUARES_FOUR)));
    response.clone().on_hover_text_at_pointer(lines.join("\n"));
    if response.clicked()
        && let Some(texture) = under.first()
    {
        *nav = Some((*texture).clone());
    }
}

// --- inspector --------------------------------------------------------------------------------------------------------

// --- references -------------------------------------------------------------------------------------------------------

fn references_ui(ui: &mut egui::Ui, ws: &Workspace, nav: &mut Option<String>) {
    let Some(asset) = ws.selected_asset() else {
        ui.label(RichText::new("Select an asset").color(theme::weak()));
        return;
    };
    let incoming = ws.index.referenced_by.get(&asset.path).map_or(&[][..], Vec::as_slice);
    theme::section(ui, &format!("Referenced by ({})", incoming.len()), |ui| {
        if incoming.is_empty() {
            ui.label(RichText::new("Nothing references this asset").color(theme::weak()));
        }
        egui::Grid::new("incoming").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
            for r in incoming {
                if ui.link(&r.from).clicked() {
                    *nav = Some(r.from.clone());
                }
                ui.label(RichText::new(format!("slot {}", r.slot)).color(theme::weak()));
                ui.end_row();
            }
        });
    });
    let outgoing: Vec<(String, &Asset)> =
        asset.fields().iter().flat_map(|(key, value)| outgoing_refs(ws, key, value)).collect();
    theme::section(ui, &format!("References ({})", outgoing.len()), |ui| {
        if outgoing.is_empty() {
            ui.label(RichText::new("This asset references nothing").color(theme::weak()));
        }
        egui::Grid::new("outgoing").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
            for (slot, target) in &outgoing {
                if ui.link(&target.path).clicked() {
                    *nav = Some(target.path.clone());
                }
                ui.label(RichText::new(format!("slot {slot}")).color(theme::weak()));
                ui.end_row();
            }
        });
    });
}

/// The assets a field references, with the slot (or field) each sits in.
fn outgoing_refs<'a>(ws: &'a Workspace, field: &str, value: &toml::Value) -> Vec<(String, &'a Asset)> {
    let mut out = Vec::new();
    match value {
        toml::Value::Table(t) => {
            if let (Some(uid), Some(path)) =
                (t.get("uid").and_then(toml::Value::as_str), t.get("path").and_then(toml::Value::as_str))
            {
                let target = path.trim_start_matches("res://").trim_end_matches(project::index::BUNDLE_SUFFIX);
                if let Some(asset) = ws.index.resolve(uid, target) {
                    out.push((field.to_string(), asset));
                }
                return out;
            }
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
    if let Some(e) = &ws.link_error {
        theme::section(ui, "Agents", |ui| {
            ui.label(RichText::new(e).color(ui.visuals().warn_fg_color));
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
                    if ui.link(path).clicked()
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
        egui::Grid::new("dangling").num_columns(3).spacing([16.0, 4.0]).show(ui, |ui| {
            for r in dangling {
                if ui.link(&r.from).clicked() {
                    *nav = Some(r.from.clone());
                }
                ui.label(RichText::new(format!("slot {}", r.slot)).color(theme::weak()));
                ui.label(RichText::new(format!("{} ({})", r.target, r.uid)).color(theme::weak()));
                ui.end_row();
            }
        });
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
    theme::section(ui, "Layers", |ui| {
        for layer in &p.layers {
            ui.horizontal(|ui| {
                ui.label(RichText::new(&layer.name).strong());
                ui.label(RichText::new(layer.path.display().to_string()).color(theme::weak()));
            });
        }
    });
    theme::section(ui, "Mounts", |ui| {
        for m in &p.mounts {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("res://{}", m.at)).strong());
                ui.label(RichText::new(m.path.display().to_string()).color(theme::weak()));
                if m.read_only {
                    badge(ui, "read only");
                }
            });
        }
    });
    if !ws.index.dangling.is_empty() {
        theme::section(ui, &format!("Dangling references ({})", ws.index.dangling.len()), |ui| {
            for r in &ws.index.dangling {
                ui.horizontal(|ui| {
                    if ui.link(&r.from).clicked() {
                        *nav = Some(r.from.clone());
                    }
                    ui.colored_label(ui.visuals().error_fg_color, format!("{} → {}", r.slot, r.target));
                });
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
        for p in &PAGES[..3] {
            if ui.selectable_label(page == *p, p.title()).clicked() {
                page = *p;
            }
        }
        ui.add_space(10.0);
        ui.label(RichText::new("PROJECT").small().strong().color(theme::weak()));
        for p in &PAGES[3..] {
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

const PAGES: [SettingsPage; 7] = [
    SettingsPage::Appearance,
    SettingsPage::Tools,
    SettingsPage::Startup,
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
        ui.label(RichText::new(file.display().to_string()).small().color(theme::weak()));
        if ui.small_button(format!("{}  Open project.toml", icons::ARROW_SQUARE_OUT)).clicked() {
            *message = Some(external::open(&file, "project.toml", tools).unwrap_or_else(|e| e));
        }
    });
    let raw: toml::Table =
        std::fs::read_to_string(&file).ok().and_then(|t| toml::from_str(&t).ok()).unwrap_or_default();
    let grid = |ui: &mut egui::Ui, id: &str, rows: Vec<(String, String)>| {
        egui::Grid::new(id).num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
            for (key, value) in rows {
                ui.label(RichText::new(key).color(theme::weak()));
                ui.monospace(value);
                ui.end_row();
            }
        });
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
