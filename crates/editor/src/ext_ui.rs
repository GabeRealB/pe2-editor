//! The extensions' views, drawn: the editor's side of `pe2_ext::Ui`. Each widget a view asks for is the editor's own -
//! a label, a button, the canvas the preview zooms and pans - so an extension's view looks and behaves as the
//! editor's do; the textures a view shows are uploaded once each, for the upscaling the preview uses.

use eframe::egui::{self, RichText};
use pe2_ext::{Canvas, Image, Project, Style, Texture, UiHost, Value};
use project::ext::Lens;
use project::index::Asset;
use std::collections::HashMap;
use std::sync::{Arc, Weak};

use crate::inspector::Sheet;
use crate::panes::{Overlay, overlay_ui};
use crate::settings::{Preview, Upscale};
use crate::theme;

/// A rectangle of an extension's, as the editor's.
pub fn to_rect(r: pe2_ext::Rect) -> egui::Rect {
    egui::Rect::from_min_size(egui::pos2(r.x, r.y), egui::vec2(r.width, r.height))
}

pub fn color_image(image: &Image) -> egui::ColorImage {
    egui::ColorImage::from_rgba_unmultiplied([image.width, image.height], &image.rgba)
}

/// The views' textures, uploaded by (key, upscaling): uploaded again when the image under a key is a new one, let go
/// once no view holds the image.
#[derive(Default)]
pub struct Textures {
    uploaded: HashMap<(String, Upscale), (Weak<Image>, egui::TextureHandle)>,
    /// The pictures drawn off the window's thread, which views ask for (`UiHost::picture`).
    pub pictures: Option<crate::pictures::Pictures>,
    /// By asset, what a view of it left for when it is made again.
    pub notes: HashMap<String, String>,
    /// What a gesture in a view has made of a part so far.
    pub live: Option<Live>,
}

impl Textures {
    /// The textures of views, with the pictures drawn for them.
    pub fn with_pictures(pictures: crate::pictures::Pictures) -> Textures {
        Textures { uploaded: HashMap::new(), pictures: Some(pictures), notes: HashMap::new(), live: None }
    }

    pub fn get(&mut self, ctx: &egui::Context, texture: &Texture, upscale: Upscale) -> egui::TextureId {
        let key = (texture.key().to_string(), upscale);
        if let Some((image, handle)) = self.uploaded.get(&key)
            && image.upgrade().is_some_and(|i| Arc::ptr_eq(&i, texture.image()))
        {
            return handle.id();
        }
        self.uploaded.retain(|_, (image, _)| image.strong_count() > 0);
        let (image, options) = crate::upscale::apply(color_image(texture.image()), upscale);
        let handle = ctx.load_texture(texture.key(), image, options);
        let id = handle.id();
        self.uploaded.insert(key, (Arc::downgrade(texture.image()), handle));
        id
    }
}

/// Where a view draws: the pane's `Ui`; a row, an indent or a collapsing body of it, which advances its parent's
/// cursor past it when it ends; or a section, whose frame closes when it ends.
enum Target<'a> {
    Pane(&'a mut egui::Ui),
    Child { parent: &'a mut egui::Ui, child: Box<egui::Ui> },
    Section { parent: &'a mut egui::Ui, prepared: Option<Box<egui::containers::frame::Prepared>> },
}

impl Target<'_> {
    fn ui(&mut self) -> &mut egui::Ui {
        match self {
            Target::Pane(ui) => ui,
            Target::Child { child, .. } => child,
            Target::Section { prepared, parent } => match prepared {
                Some(prepared) => &mut prepared.content_ui,
                None => parent,
            },
        }
    }
}

impl Target<'_> {
    fn ctx(&self) -> &egui::Context {
        match self {
            Target::Pane(ui) | Target::Section { parent: ui, .. } => ui.ctx(),
            Target::Child { child, .. } => child.ctx(),
        }
    }
}

impl Drop for Target<'_> {
    fn drop(&mut self) {
        match self {
            Target::Pane(_) => {}
            Target::Child { parent, child } => {
                parent.advance_cursor_after_rect(child.min_rect());
            }
            Target::Section { parent, prepared } => {
                if let Some(prepared) = prepared.take() {
                    prepared.end(parent);
                }
            }
        }
    }
}

/// What a view's frame shows: the asset (none for a panel), the preview's settings, and how many views
/// share the pane's height.
#[derive(Clone, Copy)]
pub struct Frame<'a> {
    pub asset: Option<&'a Asset>,
    pub preview: Preview,
    pub count: usize,
    /// Whether the asset can be edited.
    pub editable: bool,
    /// The part selected of the asset: its fragment.
    pub part: Option<&'a str>,
    /// The output it is a viewer of, for a viewer.
    pub shown: Option<&'a str>,
}

/// What a view set of the asset it shows this frame.
#[derive(Default)]
pub struct Sets {
    /// Each field's new value, or none to remove it.
    pub values: Vec<(String, Option<Value>)>,
    /// A step of its own, never merged with the one before: a gesture ended.
    pub apart: bool,
    /// A member of a family to look at.
    pub choose: Option<(String, String)>,
}

/// What a gesture has made so far of a part's fields, nothing being written yet: until the asset next changes.
pub struct Live {
    pub asset: String,
    pub part: String,
    pub fields: Vec<(String, Value)>,
}

/// A view's handle on a frame: where it draws, what it shows, what it can ask of the project.
pub struct Host<'a> {
    target: Target<'a>,
    lens: &'a Lens<'a>,
    textures: &'a mut Textures,
    nav: &'a mut Option<String>,
    sets: &'a mut Sets,
    frame: Frame<'a>,
    /// The last widget drawn, for a tooltip.
    last: Option<egui::Response>,
}

/// How a child frame lays out.
#[derive(Clone, Copy)]
enum Child {
    Row(egui::Layout),
    Indent,
    Column,
}

impl<'a> Host<'a> {
    pub fn new(
        ui: &'a mut egui::Ui,
        lens: &'a Lens<'a>,
        textures: &'a mut Textures,
        nav: &'a mut Option<String>,
        sets: &'a mut Sets,
        frame: Frame<'a>,
    ) -> Host<'a> {
        Host { target: Target::Pane(ui), lens, textures, nav, sets, frame, last: None }
    }

    fn sheet(&self, fields: bool) -> Sheet<'a> {
        let fields = match self.frame.asset {
            Some(asset) if fields => self.lens.extensions.fields(asset).into_iter().cloned().collect(),
            _ => vec![],
        };
        Sheet { index: self.lens.index, extensions: self.lens.extensions, fields, within: vec![], derived: None }
    }

    fn child(&mut self, node_type: Child) -> Box<dyn UiHost + '_> {
        let Host { target, lens, textures, nav, sets, frame, .. } = self;
        let parent = target.ui();
        let mut rect = parent.available_rect_before_wrap();
        let layout = match node_type {
            // A row starts one widget high, as egui's own rows do, and grows with what it holds.
            Child::Row(layout) => {
                rect.set_height(parent.spacing().interact_size.y);
                layout
            }
            Child::Indent => {
                rect.min.x += parent.spacing().indent;
                egui::Layout::top_down(egui::Align::Min)
            }
            // As tall as it needs, whatever its row is so far.
            Child::Column => {
                rect.set_height(parent.ctx().content_rect().height());
                egui::Layout::top_down(egui::Align::Min)
            }
        };
        let child = parent.new_child(egui::UiBuilder::new().max_rect(rect).layout(layout));
        let target = Target::Child { parent, child: Box::new(child) };
        Box::new(Host { target, lens, textures, nav, sets, frame: *frame, last: None })
    }
}

impl UiHost for Host<'_> {
    fn label(&mut self, text: &str, style: Style) {
        let text = RichText::new(text);
        let text = match style {
            Style::Normal => text,
            Style::Weak => text.color(theme::weak()),
            Style::Note => text.small().color(theme::weak()),
            Style::Strong => text.strong(),
            Style::Error => text.color(self.target.ui().visuals().error_fg_color),
        };
        self.last = Some(self.target.ui().label(text));
    }

    fn button(&mut self, text: &str) -> bool {
        let response = self.target.ui().button(text);
        let clicked = response.clicked();
        self.last = Some(response);
        clicked
    }

    fn icon_button(&mut self, icon: pe2_ext::Icon, hint: &str) -> bool {
        let button = egui::Button::new(crate::icons::glyph(icon)).frame(false);
        let response = self.target.ui().add_enabled(self.frame.editable, button).on_hover_text(hint);
        // Read out by its hint, not its glyph.
        let enabled = response.enabled();
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, hint));
        let clicked = response.clicked();
        self.last = Some(response);
        clicked
    }

    fn checkbox(&mut self, text: &str, value: &mut bool) -> bool {
        let response = self.target.ui().checkbox(value, text);
        let changed = response.changed();
        self.last = Some(response);
        changed
    }

    fn badge(&mut self, text: &str) {
        crate::inspector::badge(self.target.ui(), text);
        self.last = None;
    }

    fn chip(&mut self, text: &str, selected: bool) -> bool {
        let tint = if selected { theme::accent() } else { theme::weak() };
        let response = crate::inspector::chip(self.target.ui(), crate::icons::SQUARES_FOUR, text, tint);
        let clicked = response.clicked();
        self.last = Some(response);
        clicked
    }

    fn toggle(&mut self, text: &str, on: bool) -> bool {
        let response = self.target.ui().selectable_label(on, text);
        let clicked = response.clicked();
        self.last = Some(response);
        clicked
    }

    fn available(&mut self) -> (f32, f32, f32) {
        let ui = self.target.ui();
        let room = ui.available_size();
        (room.x, room.y, ui.ctx().pixels_per_point())
    }

    fn gpu_viewport(&mut self, id: u64, width: f32, height: f32) -> pe2_ext::ViewportInput {
        let ui = self.target.ui();
        let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click_and_drag());
        if let Some(native) = crate::gpu_view::texture_id(id) {
            let whole = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            ui.painter().image(native, rect, whole, egui::Color32::WHITE);
        }
        let input = crate::gpu_view::input(ui, &response, rect);
        self.last = Some(response);
        input
    }

    fn request_repaint(&mut self) {
        self.target.ui().ctx().request_repaint();
    }

    fn image(&mut self, texture: &Texture, width: f32, height: f32) {
        let ui = self.target.ui();
        let id = self.textures.get(ui.ctx(), texture, Upscale::Nearest);
        let shown = egui::Frame::new()
            .stroke(egui::Stroke::new(1.0, theme::border()))
            .show(ui, |ui| ui.add(egui::Image::new(egui::load::SizedTexture::new(id, egui::vec2(width, height)))));
        self.last = Some(shown.inner);
    }

    fn table(&mut self, id: &str, rows: &[Vec<pe2_ext::Cell>]) -> Option<(usize, usize)> {
        use pe2_ext::Cell;
        let sheet = self.sheet(false);
        let (editable, nav) = (self.frame.editable, &mut *self.nav);
        let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
        let mut clicked = None;
        let ui = self.target.ui();
        egui::Grid::new(("table", id)).num_columns(columns).min_col_width(0.0).spacing([10.0, 4.0]).show(ui, |ui| {
            for (r, row) in rows.iter().enumerate() {
                for (c, cell) in row.iter().enumerate() {
                    match cell {
                        Cell::Empty => {
                            ui.label("");
                        }
                        Cell::Text(text, style) => {
                            let text = RichText::new(text);
                            ui.label(match style {
                                Style::Normal => text,
                                Style::Weak => text.color(theme::weak()),
                                Style::Note => text.small().color(theme::weak()),
                                Style::Strong => text.strong(),
                                Style::Error => text.color(ui.visuals().error_fg_color),
                            });
                        }
                        Cell::Reference(value) => {
                            crate::inspector::reference_chip(ui, &sheet, value, nav);
                        }
                        Cell::Button(icon, hint) => {
                            let button = egui::Button::new(crate::icons::glyph(*icon)).frame(false);
                            let response = ui.add_enabled(editable, button).on_hover_text(hint);
                            let enabled = response.enabled();
                            response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, hint));
                            if response.clicked() {
                                clicked = Some((r, c));
                            }
                        }
                    }
                }
                ui.end_row();
            }
        });
        self.last = None;
        clicked
    }

    fn reference(&mut self, reference: &Value) {
        let sheet = self.sheet(false);
        crate::inspector::reference_chip(self.target.ui(), &sheet, reference, self.nav);
        self.last = None;
    }

    fn editing(&self) -> Option<(String, Value)> {
        crate::inspector::being_edited(self.target.ctx(), &self.frame.asset?.path)
    }

    fn reference_taking(&mut self, label: &str, node_type: &str, reference: Option<&Value>) -> Option<Value> {
        let sheet = self.sheet(false);
        let ui = self.target.ui();
        let Some(chip) = reference.and_then(crate::inspector::reference_in) else {
            let article = if node_type.starts_with(['a', 'e', 'i', 'o', 'u']) { "an" } else { "a" };
            let shown =
                ui.label(RichText::new(format!("Drop {article} {node_type} here")).small().color(theme::weak()));
            self.last = None;
            let asset = self.frame.asset.filter(|_| self.frame.editable)?;
            return crate::inspector::reference_filling(ui, shown.rect, &sheet, asset, node_type);
        };
        let reference = reference?;
        let shown = ui.scope(|ui| crate::inspector::reference_chip(ui, &sheet, chip, self.nav));
        let rect = shown.response.rect;
        self.last = None;
        let asset = self.frame.asset.filter(|_| self.frame.editable)?;
        crate::inspector::reference_dropped(ui, rect, &sheet, asset, label, reference)
    }

    fn field(&mut self, key: &str, value: &Value) {
        let sheet = self.sheet(true);
        let ui = self.target.ui();
        let shown = ui.scope(|ui| {
            if let Some(reference) = crate::inspector::reference_in(value) {
                crate::inspector::reference_chip(ui, &sheet, reference, self.nav);
            } else {
                crate::inspector::field_ui(ui, &sheet, key, value, self.nav);
            }
        });
        let rect = shown.response.rect;
        // What a view shows of a reference takes another, as the inspector's own fields do.
        if self.frame.editable
            && let Some(asset) = self.frame.asset
            && let Some(new) = crate::inspector::reference_dropped(ui, rect, &sheet, asset, key, value)
        {
            self.sets.values.push((key.to_string(), Some(new)));
        }
        self.last = None;
    }

    fn tooltip(&mut self, text: &str) {
        if let Some(response) = self.last.take() {
            response.on_hover_text(text);
        }
    }

    fn text_edit(&mut self, id: &str, text: &mut String, hint: &str, width: f32) -> bool {
        let ui = self.target.ui();
        // On a section's background a field has to stand out as a dropdown does.
        let fill = ui.visuals().widgets.inactive.weak_bg_fill;
        let width = if width > 0.0 { width } else { 120.0 };
        let edit =
            egui::TextEdit::singleline(text).id_salt(id).desired_width(width).background_color(fill).hint_text(hint);
        let response = ui.add_enabled(self.frame.editable, edit);
        let committed = response.lost_focus();
        self.last = Some(response);
        committed
    }

    fn combo(&mut self, id: &str, selected: &str, options: &[String]) -> Option<usize> {
        let mut chosen = None;
        let editable = self.frame.editable;
        let ui = self.target.ui();
        ui.add_enabled_ui(editable, |ui| {
            egui::ComboBox::from_id_salt(id).selected_text(selected).show_ui(ui, |ui| {
                for (i, option) in options.iter().enumerate() {
                    if ui.selectable_label(option == selected, option).clicked() {
                        chosen = Some(i);
                    }
                }
            });
        });
        chosen
    }

    fn shown(&self) -> Option<String> {
        self.frame.shown.map(str::to_string)
    }

    fn editable(&self) -> bool {
        self.frame.editable
    }

    fn set(&mut self, key: &str, value: Option<&Value>) {
        if self.frame.editable && self.frame.asset.is_some() {
            self.sets.values.push((key.to_string(), value.cloned()));
        }
    }

    fn set_step(&mut self, key: &str, value: Option<&Value>) {
        self.set(key, value);
        self.sets.apart = true;
    }

    fn live(&mut self, fields: &[(&str, Value)]) {
        let live = self.frame.asset.zip(self.frame.part).filter(|_| !fields.is_empty()).map(|(asset, part)| Live {
            asset: asset.path.clone(),
            part: part.to_string(),
            fields: fields.iter().map(|(key, value)| ((*key).to_string(), value.clone())).collect(),
        });
        let same = match (&live, &self.textures.live) {
            (Some(new), Some(was)) => new.fields == was.fields && new.part == was.part && new.asset == was.asset,
            (None, None) => true,
            _ => false,
        };
        if !same {
            self.textures.live = live;
            // The inspector may have drawn already this frame.
            self.target.ui().ctx().request_repaint();
        }
    }

    fn horizontal(&mut self) -> Box<dyn UiHost + '_> {
        self.child(Child::Row(egui::Layout::left_to_right(egui::Align::Center)))
    }

    fn vertical(&mut self) -> Box<dyn UiHost + '_> {
        self.child(Child::Column)
    }

    fn wrapped(&mut self) -> Box<dyn UiHost + '_> {
        self.child(Child::Row(egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true)))
    }

    fn indented(&mut self) -> Box<dyn UiHost + '_> {
        self.child(Child::Indent)
    }

    fn section(&mut self, title: &str) -> Box<dyn UiHost + '_> {
        let Host { target, lens, textures, nav, sets, frame, .. } = self;
        let parent = target.ui();
        parent.add_space(6.0);
        parent.label(RichText::new(title.to_uppercase()).small().strong().color(theme::weak()));
        parent.add_space(2.0);
        let mut prepared = egui::Frame::new()
            .fill(theme::background())
            .corner_radius(egui::CornerRadius::same(4))
            .inner_margin(egui::Margin::same(8))
            .begin(parent);
        let width = prepared.content_ui.available_width();
        prepared.content_ui.set_width(width);
        let target = Target::Section { parent, prepared: Some(Box::new(prepared)) };
        Box::new(Host { target, lens, textures, nav, sets, frame: *frame, last: None })
    }

    fn collapsing(&mut self, id: &str, title: &str, note: &str) -> Option<Box<dyn UiHost + '_>> {
        let parent = self.target.ui();
        let id = parent.make_persistent_id(id);
        let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(parent.ctx(), id, false);
        parent.horizontal(|ui| {
            state.show_toggle_button(ui, egui::collapsing_header::paint_default_icon);
            ui.label(RichText::new(title).strong());
            if !note.is_empty() {
                ui.label(RichText::new(note).small().color(theme::weak()));
            }
        });
        state.store(parent.ctx());
        state.is_open().then(|| self.child(Child::Indent))
    }

    fn canvas(&mut self, canvas: &Canvas) {
        let ui = self.target.ui();
        let ctx = ui.ctx().clone();
        let upscale = self.frame.preview.upscale;
        let pictures = canvas
            .pictures
            .iter()
            .map(|p| (self.textures.get(&ctx, &p.texture, upscale), to_rect(p.rect), p.label.clone(), p.link.clone()))
            .collect();
        let shown = self.frame.part.map(crate::workspace::part_name);
        let overlay = Overlay {
            id: self.frame.asset.map_or_else(|| format!("{:?}", ui.scope_id()), |a| a.path.clone()),
            size: egui::vec2(canvas.width, canvas.height),
            pictures,
            // The selected one is the host's to mark, whatever view drew it.
            boxes: canvas
                .boxes
                .iter()
                .map(|b| (b.name.clone(), to_rect(b.rect), b.highlight || shown == Some(b.name.as_str())))
                .collect(),
            summary: canvas.summary.clone(),
            toggle: canvas.toggle.clone(),
            legend: canvas.legend,
            grid: canvas.grid,
            traces: canvas.traces.clone(),
        };
        if let Some(link) = overlay_ui(ui, &overlay, self.frame.preview, self.frame.count) {
            *self.nav = Some(link);
        }
        self.last = None;
    }

    fn picture(&mut self, path: &str, look: Option<&str>) -> Option<Result<Texture, String>> {
        let look = self.lens.index.assets.get(path).map_or(0, |asset| self.lens.extensions.look_at(asset, look));
        let drawn = self.textures.pictures.as_mut()?.get(path, look)?;
        Some(drawn.map(|image| Texture::shared(format!("{path}#{look}#picture"), image)))
    }

    fn navigate(&mut self, path: &str) {
        *self.nav = Some(path.to_string());
    }

    fn selected(&self) -> Option<String> {
        self.frame.part.map(str::to_string)
    }

    fn note(&self) -> Option<String> {
        self.textures.notes.get(&self.frame.asset?.path).cloned()
    }

    fn leave_note(&mut self, text: &str) {
        if let Some(asset) = self.frame.asset {
            self.textures.notes.insert(asset.path.clone(), text.to_string());
        }
    }

    fn select(&mut self, part: &str) {
        *self.nav = Some(format!("#{part}"));
    }

    fn choose(&mut self, group: &str, member: &str) {
        self.sets.choose = Some((group.to_string(), member.to_string()));
    }

    fn tab(&mut self, at: f32) {
        let ui = self.target.ui();
        let gap = ui.max_rect().left() + at - ui.cursor().left();
        if gap > 0.0 {
            ui.add_space(gap);
        }
    }

    fn asset(&self) -> Option<pe2_ext::Asset<'_>> {
        self.frame.asset.map(|asset| self.lens.extensions.lend(asset))
    }

    fn project(&self) -> &dyn Project {
        self.lens
    }
}
