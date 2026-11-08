//! VRAM, 1024 × 512 halfwords: the texture pages' grid, the slots (`vram_slot`), and what the selected asset uploads -
//! an image its pages at their places (a 4-bit page a quarter of its texels wide, as VRAM holds it) and its palettes'
//! colors, a file what its image and palette slots name. Hovering names what is there; clicking an image opens it.

use pe2_ext::{Asset, Canvas, Marked, Placed, Project, Rect, Style, Texture, Ui, Value, View};
use std::collections::HashMap;

use crate::{bpp, cells, is_texture, pair, palette_names, rect, texels_per_halfword};

/// VRAM's size, in halfwords.
const VRAM: (f32, f32) = (1024.0, 512.0);
/// Texture pages: 64 halfwords wide, 256 rows high.
const PAGE: (f32, f32) = (64.0, 256.0);

/// A slot's rectangle.
fn slot_rect(slot: &Asset) -> Option<Rect> {
    rect(slot.field("rect")?)
}

/// A placement's VRAM halfword: its slot's corner plus its `at`.
fn place(project: &dyn Project, placement: &Value) -> Option<(f32, f32)> {
    let slot = slot_rect(&project.resolve(placement.get("slot")?)?)?;
    let (x, y) = pair(placement.get("at"))?;
    Some((slot.x + x, slot.y + y))
}

/// An image's pages in VRAM.
struct Upload {
    image: String,
    rect: Rect,
}

/// A palette's colors in VRAM: a run of halfwords on one row.
struct PaletteUpload {
    image: String,
    palette: String,
    rect: Rect,
}

#[derive(Default)]
struct Uploads {
    pages: Vec<Upload>,
    palettes: Vec<PaletteUpload>,
}

impl Uploads {
    /// What an asset puts in VRAM: an image its pages and its placed palettes, a file what its image, palette and
    /// bundle slots upload.
    fn of(&mut self, project: &dyn Project, asset: &Asset) {
        if is_texture(asset) {
            #[expect(clippy::cast_precision_loss, reason = "texels per halfword")]
            let per = texels_per_halfword(bpp(asset)) as f32;
            if let (Some((x, y)), Some((w, h))) =
                (asset.field("page").and_then(|p| place(project, p)), pair(asset.field("size")))
            {
                self.pages
                    .push(Upload { image: asset.path().to_string(), rect: Rect { x, y, width: w / per, height: h } });
            }
            for palette in 0..palette_names(asset).len() {
                self.palette(project, asset, palette);
            }
            return;
        }
        for slot in asset.array("slots") {
            let list = |key: &str| slot.get(key).and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
            for page in list("pages") {
                if let Some(image) = page.get("image").and_then(|t| project.resolve(t)) {
                    let mut theirs = Uploads::default();
                    theirs.of(project, &image);
                    self.pages.extend(theirs.pages);
                }
            }
            // A bundle's palette resources, in the bundle node the slot names.
            let bundle = slot.get("bundle").and_then(|t| project.resolve(t));
            let resources: Vec<Value> = bundle.map_or_else(Vec::new, |b| b.array("resources").to_vec());
            let resources = resources.iter().filter_map(|r| r.get("palettes").and_then(Value::as_array));
            for entry in list("palettes").iter().chain(resources.flatten()) {
                if let Some(image) = entry.get("image").and_then(|t| project.resolve(t))
                    && let Some(name) = entry.get("palette").and_then(Value::as_str)
                    && let Some(palette) = palette_names(&image).iter().position(|n| n == name)
                {
                    self.palette(project, &image, palette);
                }
            }
        }
    }

    /// A palette at its place: as many halfwords as the deepest cell drawn with it has colors.
    fn palette(&mut self, project: &dyn Project, image: &Asset, palette: usize) {
        let declared = image.parts("palettes").get(palette).map(|(_, p)| *p);
        let Some((x, y)) = declared.and_then(|p| place(project, p)) else { return };
        let depth = bpp(image);
        let deepest = cells(image).iter().filter(|c| c.palette == Some(palette)).map(|c| c.bpp).max().unwrap_or(depth);
        let width = if deepest == 4 { 16.0 } else { 256.0 };
        let name = palette_names(image).get(palette).cloned().unwrap_or_default();
        self.palettes.push(PaletteUpload {
            image: image.path().to_string(),
            palette: name,
            rect: Rect { x, y, width, height: 1.0 },
        });
    }
}

/// The panel, keeping the swatches it uploaded; the pages' pictures are the host's.
#[derive(Default)]
pub(crate) struct Vram {
    /// Whether the pages' grid is drawn: the user's setting.
    pub(crate) grid: bool,
    strips: HashMap<(String, String), Option<Texture>>,
}

impl Vram {
    /// The panel, its pages' grid drawn or not.
    pub(crate) fn new(grid: bool) -> Vram {
        Vram { grid, ..Vram::default() }
    }
}

impl View for Vram {
    fn show(&mut self, ui: &mut Ui) {
        let (summary, mut canvas, pages) = {
            let project = ui.project();
            let selected = project.selection();
            let mut uploads = Uploads::default();
            if let Some(asset) = &selected {
                uploads.of(project, asset);
            }
            // The pages' pictures are asked of the host below, once the project is let go of.
            let pages: Vec<(String, Option<String>, Rect)> =
                uploads.pages.iter().map(|u| (u.image.clone(), project.chosen(&u.image, "picture"), u.rect)).collect();
            let mut pictures = vec![];
            for upload in &uploads.palettes {
                let key = (upload.image.clone(), upload.palette.clone());
                let strip = self.strips.entry(key).or_insert_with(|| {
                    let asset = project.asset(&upload.image)?;
                    let strip = crate::output_picture(&asset, &format!("palettes/{}", upload.palette))?.ok()?;
                    Some(Texture::new(format!("{}#palettes/{}#vram", upload.image, upload.palette), strip))
                });
                if let Some(texture) = strip {
                    let label = Some(format!("{} · {}", upload.image, upload.palette));
                    let link = Some(upload.image.clone());
                    pictures.push(Placed { texture: texture.clone(), rect: upload.rect, label, link });
                }
            }
            let selected_path = selected.as_ref().map(Asset::path);
            let boxes = project
                .find("vram_slot")
                .iter()
                .filter_map(|slot| {
                    let rect = slot_rect(slot)?;
                    Some(Marked { name: slot.path().to_string(), rect, highlight: Some(slot.path()) == selected_path })
                })
                .collect();
            let summary = match &selected {
                Some(a) if !uploads.pages.is_empty() || !uploads.palettes.is_empty() => {
                    format!("{}: {} pages, {} palettes", a.path(), uploads.pages.len(), uploads.palettes.len())
                }
                Some(a) if a.node_type() == Some("pe2.images@vram_slot") => a.path().to_string(),
                _ => "Select a texture, an atlas or a file to see what it uploads".to_string(),
            };
            let canvas = Canvas {
                width: VRAM.0,
                height: VRAM.1,
                pictures,
                boxes,
                legend: false,
                grid: self.grid.then_some(PAGE),
                ..Canvas::default()
            };
            (summary, canvas, pages)
        };
        // Each page's picture as the host drew it, under the palettes' strips; one being drawn is there when it is.
        let mut placed = Vec::with_capacity(pages.len() + canvas.pictures.len());
        let mut drawing = 0;
        for (image, look, rect) in pages {
            match ui.picture(&image, look.as_deref()) {
                Some(Ok(texture)) => {
                    placed.push(Placed { texture, rect, label: Some(image.clone()), link: Some(image) });
                }
                None => drawing += 1,
                Some(Err(_)) => {}
            }
        }
        placed.append(&mut canvas.pictures);
        canvas.pictures = placed;
        let summary = if drawing > 0 { format!("{summary} (drawing {drawing})") } else { summary };
        ui.styled(&summary, Style::Note);
        ui.canvas(&canvas);
    }
}

/// A slot where it is in VRAM, as its document shows it: the texture pages' grid, the other slots faintly, this one
/// marked, and what it covers said under it.
#[derive(Default)]
pub struct SlotPlace;

impl View for SlotPlace {
    fn show(&mut self, ui: &mut Ui<'_>) {
        use pe2_ext::{Align, Color, Role, Shape};
        let (width, _, _) = ui.available();
        let Some((own, path)) = ui.asset().map(|asset| (slot_rect(&asset), asset.path().to_string())) else { return };
        let Some(own) = own else {
            ui.label("It has no rectangle: set one, [x, y, width, height] in halfwords.");
            return;
        };
        let k = width.max(240.0) / VRAM.0;
        let at = |x: f32, y: f32| (x * k, y * k);
        let theme = Color::Theme;
        let mut shapes = vec![Shape::Rect {
            min: (0.0, 0.0),
            max: at(VRAM.0, VRAM.1),
            fill: Some(theme(Role::Raised)),
            stroke: Some((1.0, theme(Role::Border))),
            rounding: 0.0,
        }];
        // The texture pages.
        for x in (1..16_u8).map(|i| f32::from(i) * PAGE.0) {
            shapes.push(Shape::Line {
                points: vec![at(x, 0.0), at(x, VRAM.1)],
                width: 1.0,
                color: theme(Role::Border),
            });
        }
        shapes.push(Shape::Line {
            points: vec![at(0.0, PAGE.1), at(VRAM.0, PAGE.1)],
            width: 1.0,
            color: theme(Role::Border),
        });
        // The other slots, faintly, then this one.
        let others: Vec<Rect> = ui
            .project()
            .find("pe2.images@vram_slot")
            .iter()
            .filter(|slot| slot.path() != path)
            .filter_map(slot_rect)
            .collect();
        for slot in others {
            shapes.push(Shape::Rect {
                min: at(slot.x, slot.y),
                max: at(slot.x + slot.width, slot.y + slot.height),
                fill: None,
                stroke: Some((1.0, theme(Role::Weak))),
                rounding: 0.0,
            });
        }
        let (min, max) = (at(own.x, own.y), at(own.x + own.width, own.y + own.height));
        shapes.push(Shape::Rect {
            min,
            max,
            fill: Some(Color::Rgba(80, 140, 255, 90)),
            stroke: Some((2.0, theme(Role::Accent))),
            rounding: 0.0,
        });
        let name = path.rsplit('/').next().unwrap_or_default().to_string();
        let middle = (f32::midpoint(min.0, max.0), f32::midpoint(min.1, max.1));
        shapes.push(Shape::Text { at: middle, text: name, size: 12.0, color: theme(Role::Text), align: Align::Middle });
        let (_, height) = at(VRAM.0, VRAM.1);
        let _ = ui.paint(width.max(240.0), height, &shapes);
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "VRAM coordinates")]
        let page = |x: f32, y: f32| (y / PAGE.1) as u32 * 16 + (x / PAGE.0) as u32;
        let (first, last) = (page(own.x, own.y), page(own.x + own.width - 1.0, own.y + own.height - 1.0));
        let pages =
            if first == last { format!("texture page {first}") } else { format!("texture pages {first} to {last}") };
        ui.styled(
            &format!(
                "{} × {} halfwords at ({}, {}) of VRAM's 1024 × 512: {pages}",
                own.width, own.height, own.x, own.y
            ),
            Style::Weak,
        );
    }
}
