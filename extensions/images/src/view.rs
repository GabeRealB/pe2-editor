//! The image's view: its picture on a canvas, in the look the host's picker chose.

use pe2_ext::{Asset, Canvas, Marked, Picture, Placed, Rect, Texture, Ui, View};
use std::collections::HashMap;

use crate::{Pictures, cells, pages, role};

/// An image on a canvas: an atlas with a box over each cell, a texture wider than a page with a box over each page.
pub(crate) struct ImageView {
    size: (f32, f32),
    boxes: Vec<Marked>,
    summary: String,
    toggle: &'static str,
    /// Each look's picture, decoded when first shown.
    pictures: HashMap<usize, Result<Texture, String>>,
}

fn marked(name: String, rect: Rect) -> Marked {
    Marked { name, rect, highlight: false }
}

impl ImageView {
    pub(crate) fn new(asset: &Asset) -> ImageView {
        let (size, page_width) = pages(asset);
        let mut view = ImageView { size, boxes: vec![], summary: String::new(), toggle: "", pictures: HashMap::new() };
        match role(asset) {
            Some("atlas") => {
                view.boxes = cells(asset).into_iter().map(|c| marked(c.name, c.rect)).collect();
                let n = view.boxes.len();
                view.summary = format!("{n} cell{}", if n == 1 { "" } else { "s" });
                view.toggle = "Cells";
            }
            Some("texture") if size.0 > page_width => {
                #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a page count")]
                let n = (size.0 / page_width).ceil() as usize;
                #[expect(clippy::cast_precision_loss, reason = "a page count")]
                let page = |k: usize| {
                    let x = k as f32 * page_width;
                    marked(format!("page {k}"), Rect { x, y: 0.0, width: (size.0 - x).min(page_width), height: size.1 })
                };
                view.boxes = (0..n).map(page).collect();
                view.summary = format!("{n} pages of {page_width} texels");
                view.toggle = "Pages";
            }
            _ => {}
        }
        view
    }
}

impl View for ImageView {
    fn show(&mut self, ui: &mut Ui) {
        let look = ui.look();
        let picture = self.pictures.entry(look).or_insert_with(|| {
            let asset = ui.asset().ok_or("no asset")?;
            Pictures
                .draw(&asset, look)
                .map(|image| Texture::new(format!("{}#{look}", asset.path()), image))
                .map_err(|e| format!("{e:#}"))
        });
        let texture = match picture {
            Ok(texture) => texture.clone(),
            Err(e) => {
                ui.error(&format!("No preview: {e}"));
                return;
            }
        };
        let (width, height) = self.size;
        // A background's size is its picture's: it declares none.
        #[expect(clippy::cast_precision_loss, reason = "picture sizes")]
        let (width, height) =
            if width > 0.0 { (width, height) } else { (texture.width() as f32, texture.height() as f32) };
        ui.canvas(&Canvas {
            width,
            height,
            pictures: vec![Placed { texture, rect: Rect { x: 0.0, y: 0.0, width, height }, label: None, link: None }],
            boxes: self.boxes.clone(),
            summary: self.summary.clone(),
            toggle: self.toggle.to_string(),
            legend: true,
            grid: None,
        });
    }
}
