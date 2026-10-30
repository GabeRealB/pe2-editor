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
}

impl Textures {
    /// The textures of views, with the pictures drawn for them.
    pub fn with_pictures(pictures: crate::pictures::Pictures) -> Textures {
        Textures { uploaded: HashMap::new(), pictures: Some(pictures) }
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

/// What a view's frame shows: the asset (none for a panel), its look, the preview's settings, and how many views
/// share the pane's height.
#[derive(Clone, Copy)]
pub struct Frame<'a> {
    pub asset: Option<&'a Asset>,
    pub look: usize,
    pub preview: Preview,
    pub count: usize,
    /// Whether the asset can be edited.
    pub editable: bool,
    /// The part selected of the asset: its fragment.
    pub part: Option<&'a str>,
}

/// What a view set of the asset it shows this frame: each field's new value, or none to remove it.
pub type Sets = Vec<(String, Option<Value>)>;

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

    fn child(&mut self, kind: Child) -> Box<dyn UiHost + '_> {
        let Host { target, lens, textures, nav, sets, frame, .. } = self;
        let parent = target.ui();
        let mut rect = parent.available_rect_before_wrap();
        let layout = match kind {
            // A row starts one widget high, as egui's own rows do, and grows with what it holds.
            Child::Row(layout) => {
                rect.set_height(parent.spacing().interact_size.y);
                layout
            }
            Child::Indent => {
                rect.min.x += parent.spacing().indent;
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

    fn image(&mut self, texture: &Texture, width: f32, height: f32) {
        let ui = self.target.ui();
        let id = self.textures.get(ui.ctx(), texture, Upscale::Nearest);
        let shown = egui::Frame::new().stroke(egui::Stroke::new(1.0, theme::border())).show(ui, |ui| {
            ui.add(egui::Image::new(egui::load::SizedTexture::new(id, egui::vec2(width, height))))
        });
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

    fn field(&mut self, key: &str, value: &Value) {
        let sheet = self.sheet(true);
        crate::inspector::field_ui(self.target.ui(), &sheet, key, value, self.nav);
        self.last = None;
    }

    fn tooltip(&mut self, text: &str) {
        if let Some(response) = self.last.take() {
            response.on_hover_text(text);
        }
    }

    fn text_edit(&mut self, id: &str, text: &mut String) -> bool {
        let edit = egui::TextEdit::singleline(text).id_salt(id).desired_width(120.0);
        let response = self.target.ui().add_enabled(self.frame.editable, edit);
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

    fn editable(&self) -> bool {
        self.frame.editable
    }

    fn set(&mut self, key: &str, value: Option<&Value>) {
        if self.frame.editable && self.frame.asset.is_some() {
            self.sets.push((key.to_string(), value.cloned()));
        }
    }

    fn horizontal(&mut self) -> Box<dyn UiHost + '_> {
        self.child(Child::Row(egui::Layout::left_to_right(egui::Align::Center)))
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
            boxes: canvas.boxes.iter().map(|b| (b.name.clone(), to_rect(b.rect), b.highlight || shown == Some(b.name.as_str()))).collect(),
            summary: canvas.summary.clone(),
            toggle: canvas.toggle.clone(),
            legend: canvas.legend,
            grid: canvas.grid,
        };
        if let Some(link) = overlay_ui(ui, &overlay, self.frame.preview, self.frame.count) {
            *self.nav = Some(link);
        }
        self.last = None;
    }

    fn picture(&mut self, path: &str, look: usize) -> Option<Result<Texture, String>> {
        let drawn = self.textures.pictures.as_mut()?.get(path, look)?;
        Some(drawn.map(|image| Texture::shared(format!("{path}#{look}#picture"), image)))
    }

    fn navigate(&mut self, path: &str) {
        *self.nav = Some(path.to_string());
    }

    fn selected(&self) -> Option<String> {
        self.frame.part.map(str::to_string)
    }

    fn select(&mut self, part: &str) {
        *self.nav = Some(format!("#{part}"));
    }

    fn asset(&self) -> Option<pe2_ext::Asset<'_>> {
        self.frame.asset.map(|asset| self.lens.extensions.lend(asset))
    }

    fn look(&self) -> usize {
        self.frame.look
    }

    fn project(&self) -> &dyn Project {
        self.lens
    }
}
