//! The extensions' views, drawn: the editor's side of `pe2_ext::Ui`. Each widget a view asks for is the editor's own -
//! a label, a button, the canvas the preview zooms and pans - so an extension's view looks and behaves as the
//! editor's do; the textures a view shows are uploaded once each, for the upscaling the preview uses.

use eframe::egui::{self, RichText};
use pe2_ext::{Canvas, Image, Style, Texture, UiHost};
use project::ext::Extensions;
use project::index::Asset;
use std::collections::HashMap;
use std::sync::{Arc, Weak};

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
}

impl Textures {
    fn get(&mut self, ctx: &egui::Context, texture: &Texture, upscale: Upscale) -> egui::TextureId {
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

/// A view's frame: the pane's `Ui`, the asset shown, its look and the preview's settings.
pub struct Host<'a> {
    ui: &'a mut egui::Ui,
    asset: &'a Asset,
    extensions: &'a Extensions,
    textures: &'a mut Textures,
    look: usize,
    preview: Preview,
    /// How many views share the pane's height.
    count: usize,
}

impl<'a> Host<'a> {
    pub fn new(
        ui: &'a mut egui::Ui,
        asset: &'a Asset,
        extensions: &'a Extensions,
        textures: &'a mut Textures,
        look: usize,
        preview: Preview,
        count: usize,
    ) -> Host<'a> {
        Host { ui, asset, extensions, textures, look, preview, count }
    }
}

impl UiHost for Host<'_> {
    fn label(&mut self, text: &str, style: Style) {
        let text = RichText::new(text);
        let text = match style {
            Style::Normal => text,
            Style::Weak => text.color(theme::weak()),
            Style::Strong => text.strong(),
            Style::Error => text.color(self.ui.visuals().error_fg_color),
        };
        self.ui.label(text);
    }

    fn button(&mut self, text: &str) -> bool {
        self.ui.button(text).clicked()
    }

    fn checkbox(&mut self, text: &str, value: &mut bool) -> bool {
        self.ui.checkbox(value, text).changed()
    }

    fn horizontal(&mut self, body: &mut dyn FnMut(&mut dyn UiHost)) {
        let Host { ui, asset, extensions, textures, look, preview, count } = self;
        ui.horizontal(|ui| {
            let textures = &mut **textures;
            body(&mut Host { ui, asset, extensions, textures, look: *look, preview: *preview, count: *count });
        });
    }

    fn canvas(&mut self, canvas: &Canvas) {
        let ctx = self.ui.ctx().clone();
        let upscale = self.preview.upscale;
        let pictures = canvas.pictures.iter().map(|(t, r)| (self.textures.get(&ctx, t, upscale), to_rect(*r))).collect();
        let overlay = Overlay {
            id: self.asset.path.clone(),
            size: egui::vec2(canvas.width, canvas.height),
            pictures,
            boxes: canvas.boxes.iter().map(|(name, r)| (name.clone(), to_rect(*r))).collect(),
            summary: canvas.summary.clone(),
            toggle: canvas.toggle.clone(),
        };
        overlay_ui(self.ui, &overlay, self.preview, self.count);
    }

    fn asset(&self) -> pe2_ext::Asset<'_> {
        self.extensions.lend(self.asset)
    }

    fn look(&self) -> usize {
        self.look
    }
}
