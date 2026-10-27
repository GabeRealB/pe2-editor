//! An open project as the editor holds it: its index, the asset tree, the selection with its history, and the
//! pictures decoded so far.

use anyhow::Result;
use eframe::egui;
use project::Project;
use project::index::{Asset, Index};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use crate::settings::Upscale;

/// A directory of the asset tree: its subdirectories and, where an asset has this path, that asset. An image with
/// several variants - its other looks - is a virtual folder of them: choosing one shows the image so.
#[derive(Default)]
pub struct Node {
    pub children: BTreeMap<String, Node>,
    pub asset: Option<String>,
    pub entry: Option<Entry>,
}

/// A virtual entry: an image and one of its variants.
#[derive(Clone, PartialEq, Eq)]
pub struct Entry {
    pub image: String,
    pub variant: usize,
}

impl Node {
    fn build(index: &Index) -> Node {
        let mut root = Node::default();
        for (path, asset) in &index.assets {
            let mut node = &mut root;
            for part in path.split('/') {
                node = node.children.entry(part.to_string()).or_default();
            }
            node.asset = Some(path.clone());
            let names = variant_names(asset);
            if names.len() > 1 {
                for (i, name) in names.into_iter().enumerate() {
                    let entry = Some(Entry { image: path.clone(), variant: i });
                    node.children.insert(name, Node { entry, ..Node::default() });
                }
            }
        }
        root
    }

    /// The children in natural order: `view2` before `view10`.
    pub fn sorted_children(&self) -> Vec<(&String, &Node)> {
        let mut children: Vec<(&String, &Node)> = self.children.iter().collect();
        children.sort_by_cached_key(|(name, _)| natural(name));
        children
    }
}

/// A name's sort key: its runs of digits compared as numbers.
fn natural(name: &str) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    let mut rest = name;
    while !rest.is_empty() {
        let text_len = rest.find(|c: char| c.is_ascii_digit()).unwrap_or(rest.len());
        let (text, after) = rest.split_at(text_len);
        let digits_len = after.find(|c: char| !c.is_ascii_digit()).unwrap_or(after.len());
        let (digits, next) = after.split_at(digits_len);
        out.push((text.to_string(), digits.parse().unwrap_or(0)));
        rest = next;
    }
    out
}

/// A picture as uploaded - scaled by the upscaling method - and its own size in pixels, or why there is none.
pub type Picture = Result<Shown, String>;

pub struct Shown {
    pub texture: egui::TextureHandle,
    pub size: egui::Vec2,
}

pub struct Workspace {
    pub project: Project,
    pub index: Index,
    pub tree: Node,
    pub selected: Option<String>,
    back: Vec<String>,
    forward: Vec<String>,
    pub filter: String,
    /// An asset just navigated to: the tree opens its ancestors and scrolls to it once.
    pub reveal: Option<String>,
    /// Decoded pictures by (image, variant, upscaling).
    pictures: HashMap<(String, usize, Upscale), Picture>,
    /// Palettes as strips of colors, by (image, palette).
    strips: HashMap<(String, usize), Picture>,
    /// The variant an image is shown in (an index into `variants_of`); without one, its first.
    pub variant_choice: HashMap<String, usize>,
}

/// An image's pages in VRAM, in halfwords: where, and how large (a 4-bit texel is a quarter of a halfword).
pub struct Upload {
    pub image: String,
    pub rect: egui::Rect,
}

/// A palette's colors in VRAM: a run of halfwords on one row.
pub struct PaletteUpload {
    pub image: String,
    pub palette: usize,
    pub rect: egui::Rect,
}

/// An image's role (`texture`, `atlas`, `background`, ...).
pub fn role(asset: &Asset) -> Option<&str> {
    asset.fields.get("role").and_then(toml::Value::as_str)
}

/// A texture or an atlas: an image of texels and palettes.
fn is_texture(asset: &Asset) -> bool {
    matches!(role(asset), Some("texture" | "atlas"))
}

fn array<'a>(asset: &'a Asset, key: &str) -> &'a [toml::Value] {
    asset.fields.get(key).and_then(toml::Value::as_array).map_or(&[][..], Vec::as_slice)
}

/// An image's palettes by name, in the order it declares them.
fn palette_names(asset: &Asset) -> Vec<String> {
    array(asset, "palettes").iter().filter_map(|v| v.get("name")?.as_str().map(str::to_string)).collect()
}

/// An image's looks, by name: a texture's variants (the first its default); an atlas's default and its variants.
fn variant_names(asset: &Asset) -> Vec<String> {
    let named = array(asset, "variants").iter().filter_map(|v| v.get("name")?.as_str().map(str::to_string));
    if role(asset) == Some("atlas") && !array(asset, "variants").is_empty() {
        std::iter::once("default".to_string()).chain(named).collect()
    } else {
        named.collect()
    }
}

/// The palette each part of an image takes in one of its looks: a texture's (`None` for its only cell, the image),
/// or each of an atlas's cells by name.
fn look(asset: &Asset, variant: usize) -> (Option<usize>, HashMap<String, usize>) {
    let names = palette_names(asset);
    let index = |name: Option<&str>| name.and_then(|n| names.iter().position(|p| p == n));
    let variants = array(asset, "variants");
    if role(asset) == Some("atlas") {
        let overrides = variant
            .checked_sub(1)
            .and_then(|v| variants.get(v)?.get("palettes")?.as_table().cloned())
            .unwrap_or_default();
        let chosen = overrides.iter().filter_map(|(cell, p)| Some((cell.clone(), index(p.as_str())?))).collect();
        return (None, chosen);
    }
    let palette = variants.get(variant).and_then(|v| v.get("palette")).and_then(toml::Value::as_str);
    (index(palette).or(Some(0)), HashMap::new())
}

fn int(value: Option<&toml::Value>) -> Option<f32> {
    #[expect(clippy::cast_precision_loss, reason = "VRAM coordinates")]
    value.and_then(toml::Value::as_integer).map(|v| v as f32)
}

fn pair(value: Option<&toml::Value>) -> Option<egui::Vec2> {
    let a = value?.as_array()?;
    Some(egui::vec2(int(a.first())?, int(a.get(1))?))
}

fn bpp_of(value: Option<&toml::Value>, default: u8) -> u8 {
    value.and_then(toml::Value::as_integer).and_then(|v| u8::try_from(v).ok()).unwrap_or(default)
}

fn texels_per_halfword(bpp: u8) -> usize {
    match bpp {
        4 => 4,
        8 => 2,
        _ => 1,
    }
}

/// A `rect = [x, y, w, h]` as a rectangle.
fn rect(value: &toml::Value) -> Option<egui::Rect> {
    let r = value.as_array()?;
    Some(egui::Rect::from_min_size(
        egui::pos2(int(r.first())?, int(r.get(1))?),
        egui::vec2(int(r.get(2))?, int(r.get(3))?),
    ))
}

/// A cell as drawn in its image: its rectangle in the image's texels - a cell of a deeper depth starts where its
/// halfwords do and is drawn at its own width - with its depth and palette.
pub struct Cell {
    pub name: String,
    pub rect: egui::Rect,
    pub bpp: u8,
    pub source: egui::Rect,
    pub palette: Option<usize>,
}

/// An atlas's cells.
pub fn cells_of(asset: &Asset) -> Vec<Cell> {
    let bpp = bpp_of(asset.fields.get("bpp"), 8);
    let names = palette_names(asset);
    array(asset, "cells")
        .iter()
        .filter_map(|c| {
            let source = rect(c.get("rect")?)?;
            let own = bpp_of(c.get("bpp"), bpp);
            #[expect(clippy::cast_precision_loss, reason = "texels per halfword")]
            let scale = texels_per_halfword(bpp) as f32 / texels_per_halfword(own) as f32;
            let rect = egui::Rect::from_min_size(egui::pos2(source.min.x * scale, source.min.y), source.size());
            let palette =
                c.get("palette").and_then(toml::Value::as_str).and_then(|p| names.iter().position(|n| n == p));
            Some(Cell { name: c.get("name")?.as_str()?.to_string(), rect, bpp: own, source, palette })
        })
        .collect()
}

/// What a picture is drawn from: a background's `native.bs`, or an image's stored texels and its palettes' colors
/// (`project::declarations`; `project::textures` writes them).
enum Source {
    Bs(PathBuf),
    Texels { texels: PathBuf, colors: Vec<Option<PathBuf>> },
}

fn source(asset: &Asset) -> Option<Source> {
    let file = |name: &str| asset.files.iter().find(|f| f.name == name).map(|f| f.path.clone());
    if let Some(bs) = file("native.bs") {
        return Some(Source::Bs(bs));
    }
    if !is_texture(asset) {
        return None;
    }
    let texels = file("native.texels")?;
    let colors = palette_names(asset).iter().map(|p| file(&format!("native.{p}.colors"))).collect();
    Some(Source::Texels { texels, colors })
}

fn halfwords(bytes: &[u8]) -> Vec<u16> {
    bytes.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect()
}

fn rgba(color: u16) -> egui::Color32 {
    let [red, green, blue, alpha] = codecs::texture::rgba(color);
    egui::Color32::from_rgba_unmultiplied(red, green, blue, alpha)
}

/// An image's pixels in one of its looks: a texture through its variant's palette; an atlas faintly through its first
/// palette, and each of its cells through its own - the variant's where it gives the cell one - a deeper cell's texels
/// taken from the same halfwords at its depth. Where no colors are extracted (an executable uploads them) or none are
/// known, in gray.
fn decode_texels(asset: &Asset, texels: &[u8], colors: &[Option<Vec<u16>>], variant: usize) -> egui::ColorImage {
    let bpp = bpp_of(asset.fields.get("bpp"), 8);
    let size = pair(asset.fields.get("size")).unwrap_or_default();
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "image sizes")]
    let (w, h) = (size.x as usize, size.y as usize);
    let values: Vec<u16> = if bpp == 16 { halfwords(texels) } else { texels.iter().map(|&b| u16::from(b)).collect() };
    let at = |x: usize, y: usize| values.get(y * w + x).copied().unwrap_or(0);
    if bpp == 16 {
        let pixels = (0..w * h).map(|i| rgba(at(i % w, i / w))).collect();
        return egui::ColorImage::new([w, h], pixels);
    }
    let gray = codecs::texture::gray_ramp(bpp);
    let palette = |i: Option<usize>| -> &[u16] { i.and_then(|i| colors.get(i)?.as_deref()).unwrap_or(&gray) };
    let cells = cells_of(asset);
    let atlas = !cells.is_empty();
    let (texture_palette, overrides) = look(asset, variant);
    let base = palette(texture_palette.or(Some(0)));
    let mut pixels: Vec<egui::Color32> = (0..w * h)
        .map(|i| {
            let c = base.get(usize::from(at(i % w, i / w))).map_or(egui::Color32::TRANSPARENT, |&c| rgba(c));
            if atlas { c.gamma_multiply(0.25) } else { c }
        })
        .collect();
    if atlas {
        let per = texels_per_halfword(bpp);
        for cell in &cells {
            let colors = palette(overrides.get(&cell.name).copied().or(cell.palette));
            #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "texel coordinates")]
            let (sx, sy, cw, ch, dx) = (
                cell.source.min.x as usize,
                cell.source.min.y as usize,
                cell.source.width() as usize,
                cell.source.height() as usize,
                cell.rect.min.x as usize,
            );
            // A deeper cell spans more of the image's texels than it is wide: the span past its width is its own
            // halfwords read at the image's depth, so it is left blank.
            let span = cw * per / texels_per_halfword(cell.bpp);
            for y in sy..(sy + ch).min(h) {
                for x in (dx + cw)..(dx + span).min(w) {
                    pixels[y * w + x] = egui::Color32::TRANSPARENT;
                }
                for i in 0..cw {
                    let index = if cell.bpp == bpp {
                        at(sx + i, y)
                    } else {
                        // The cell's texel from the halfword holding it, rebuilt from the image's texels.
                        let own = texels_per_halfword(cell.bpp);
                        let (word_x, k) = ((sx + i) / own, (sx + i) % own);
                        let word = (0..per).fold(0u16, |word, j| word | at(word_x * per + j, y) << (16 / per * j));
                        (word >> (16 / own * k)) & ((1 << (16 / own)) - 1)
                    };
                    if dx + i < w {
                        pixels[y * w + dx + i] =
                            colors.get(usize::from(index)).map_or(egui::Color32::TRANSPARENT, |&c| rgba(c));
                    }
                }
            }
        }
    }
    egui::ColorImage::new([w, h], pixels)
}

/// A background's picture.
fn decode_bs(file: &Path) -> Result<egui::ColorImage, String> {
    let bytes = std::fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let picture = codecs::bs::decode(&bytes).map_err(|e| e.to_string())?;
    Ok(egui::ColorImage::from_rgb([picture.width, picture.height], &picture.rgb))
}

impl Workspace {
    /// Open the project in `dir`.
    ///
    /// # Errors
    /// Its `project.toml` or its assets cannot be read.
    pub fn open(dir: &Path) -> Result<Workspace> {
        let project = Project::load(dir)?;
        let index = Index::load(&project)?;
        let tree = Node::build(&index);
        Ok(Workspace {
            project,
            index,
            tree,
            selected: None,
            back: vec![],
            forward: vec![],
            filter: String::new(),
            reveal: None,
            pictures: HashMap::new(),
            strips: HashMap::new(),
            variant_choice: HashMap::new(),
        })
    }

    pub fn selected_asset(&self) -> Option<&Asset> {
        self.index.assets.get(self.selected.as_deref()?)
    }

    pub fn can_go_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    pub fn navigate(&mut self, path: String) {
        if self.selected.as_ref() == Some(&path) {
            return;
        }
        if let Some(current) = self.selected.take() {
            self.back.push(current);
        }
        self.forward.clear();
        self.reveal = Some(path.clone());
        self.selected = Some(path);
    }

    /// Choose a virtual entry: its image selected and shown in the entry's variant; choosing the entry shown again
    /// shows the image in its first.
    pub fn choose(&mut self, entry: &Entry) {
        if self.is_chosen(entry) {
            self.variant_choice.remove(&entry.image);
            return;
        }
        if self.selected.as_deref() != Some(&entry.image) {
            self.select(entry.image.clone());
        }
        self.variant_choice.insert(entry.image.clone(), entry.variant);
    }

    /// Whether a virtual entry is what is shown, chosen.
    pub fn is_chosen(&self, entry: &Entry) -> bool {
        self.selected.as_deref() == Some(&entry.image) && self.variant_choice.get(&entry.image) == Some(&entry.variant)
    }

    /// Select an asset picked in the tree: like `navigate`, but the tree does not move. Picking the selected image
    /// again shows it in its first variant.
    pub fn select(&mut self, path: String) {
        if self.selected.as_deref() == Some(&path) {
            self.variant_choice.remove(&path);
            return;
        }
        self.navigate(path);
        self.reveal = None;
    }

    pub fn go_back(&mut self) {
        if let Some(path) = self.back.pop() {
            if let Some(current) = self.selected.take() {
                self.forward.push(current);
            }
            self.reveal = Some(path.clone());
            self.selected = Some(path);
        }
    }

    pub fn go_forward(&mut self) {
        if let Some(path) = self.forward.pop() {
            if let Some(current) = self.selected.take() {
                self.back.push(current);
            }
            self.reveal = Some(path.clone());
            self.selected = Some(path);
        }
    }
    /// The asset a reference value (`{ uid, path }`) names.
    pub fn target(&self, value: &toml::Value) -> Option<&Asset> {
        let path =
            value.get("path")?.as_str()?.trim_start_matches("res://").trim_end_matches(project::index::BUNDLE_SUFFIX);
        self.index.resolve(value.get("uid")?.as_str()?, path)
    }

    /// An image's palettes by name.
    pub fn palettes_of(&self, path: &str) -> Vec<String> {
        self.index.assets.get(path).map_or_default(palette_names)
    }

    /// An image's looks by name.
    pub fn variants_of(&self, path: &str) -> Vec<String> {
        self.index.assets.get(path).map_or_default(variant_names)
    }

    /// The variant an image is shown in.
    pub fn variant(&self, path: &str) -> usize {
        let n = self.variants_of(path).len();
        self.variant_choice.get(path).copied().filter(|&v| v < n).unwrap_or(0)
    }

    /// Whether an image has a picture: a background, a texture, an atlas.
    pub fn has_picture(&self, path: &str) -> bool {
        self.index.assets.get(path).and_then(source).is_some()
    }

    /// The picture of an image - a background, a texture or an atlas, in its chosen variant - decoded once per variant
    /// and upscaling method.
    pub fn picture(&mut self, ctx: &egui::Context, path: &str, upscale: Upscale) -> Option<&Picture> {
        let chosen = self.variant(path);
        let asset = self.index.assets.get(path)?.clone();
        let source = source(&asset)?;
        Some(self.pictures.entry((path.to_string(), chosen, upscale)).or_insert_with(|| {
            let image = match source {
                Source::Bs(file) => decode_bs(&file)?,
                Source::Texels { texels, colors } => {
                    let bytes = std::fs::read(&texels).map_err(|e| format!("{}: {e}", texels.display()))?;
                    let colors = colors
                        .iter()
                        .map(|f| f.as_ref().and_then(|f| std::fs::read(f).ok()).map(|b| halfwords(&b)))
                        .collect::<Vec<_>>();
                    decode_texels(&asset, &bytes, &colors, chosen)
                }
            };
            #[expect(clippy::cast_precision_loss, reason = "picture sizes")]
            let size = egui::vec2(image.size[0] as f32, image.size[1] as f32);
            let (image, options) = crate::upscale::apply(image, upscale);
            Ok(Shown { texture: ctx.load_texture(format!("{path}#{chosen}#{upscale:?}"), image, options), size })
        }))
    }

    /// A palette's colors, as a strip.
    pub fn palette(&mut self, ctx: &egui::Context, path: &str, palette: usize) -> Option<&Picture> {
        let Some(Source::Texels { colors, .. }) = self.index.assets.get(path).and_then(source) else { return None };
        let file = colors.get(palette).cloned().flatten()?;
        Some(self.strips.entry((path.to_string(), palette)).or_insert_with(|| {
            let pixels: Vec<egui::Color32> =
                halfwords(&std::fs::read(&file).map_err(|e| e.to_string())?).into_iter().map(rgba).collect();
            #[expect(clippy::cast_precision_loss, reason = "palette sizes")]
            let size = egui::vec2(pixels.len() as f32, 1.0);
            let image = egui::ColorImage::new([pixels.len().max(1), 1], pixels);
            let options = egui::TextureOptions::NEAREST;
            Ok(Shown { texture: ctx.load_texture(format!("{path}#{palette}#palette"), image, options), size })
        }))
    }

    /// The pictures to show for an asset, as (label, image): its own; or what a container's slots hold - the assets
    /// filling them, the images its image slots upload.
    pub fn pictures_of(&self, asset: &Asset) -> Vec<(Option<String>, String)> {
        if self.has_picture(&asset.path) {
            return vec![(None, asset.path.clone())];
        }
        let mut out: Vec<(Option<String>, String)> = Vec::new();
        let mut push = |label: String, a: &Asset| {
            if self.has_picture(&a.path) && !out.iter().any(|(_, p)| *p == a.path) {
                out.push((Some(label), a.path.clone()));
            }
        };
        for slot in array(asset, "slots") {
            let name = slot.get("name").and_then(toml::Value::as_str).unwrap_or_default().to_string();
            if let Some(a) = slot.get("asset").and_then(|r| self.target(r)) {
                push(name.clone(), a);
            }
            let pages = slot.get("pages").and_then(toml::Value::as_array).map_or(&[][..], Vec::as_slice);
            for (i, page) in pages.iter().enumerate() {
                if let Some(image) = page.get("image").and_then(|t| self.target(t)) {
                    push(format!("{name} page {i}"), image);
                }
            }
        }
        out
    }

    /// The VRAM slots, by asset path, and their rectangles in halfwords.
    pub fn vram_slots(&self) -> Vec<(String, egui::Rect)> {
        self.index
            .assets
            .values()
            .filter(|a| a.kind.as_deref() == Some("vram_slot"))
            .filter_map(|a| Some((a.path.clone(), rect(a.fields.get("rect")?)?)))
            .collect()
    }

    /// A placement's VRAM halfword: its slot's corner plus its `at`.
    fn place(&self, placement: &toml::Value) -> Option<egui::Pos2> {
        let slot = rect(self.target(placement.get("slot")?)?.fields.get("rect")?)?;
        Some(slot.min + pair(placement.get("at"))?)
    }

    /// What an asset puts in VRAM: an image its pages and its placed palettes, a file what its image, palette and
    /// bundle slots upload.
    pub fn uploads_of(&self, asset: &Asset) -> (Vec<Upload>, Vec<PaletteUpload>) {
        let (mut pages, mut palettes) = (vec![], vec![]);
        if is_texture(asset) {
            let bpp = bpp_of(asset.fields.get("bpp"), 8);
            if let (Some(origin), Some(size)) =
                (asset.fields.get("page").and_then(|p| self.place(p)), pair(asset.fields.get("size")))
            {
                #[expect(clippy::cast_precision_loss, reason = "texels per halfword")]
                let per = texels_per_halfword(bpp) as f32;
                let rect = egui::Rect::from_min_size(origin, egui::vec2(size.x / per, size.y));
                pages.push(Upload { image: asset.path.clone(), rect });
            }
            for i in 0..palette_names(asset).len() {
                self.palette_upload(asset, i, &mut palettes);
            }
            return (pages, palettes);
        }
        for slot in array(asset, "slots") {
            let list = |key: &str| slot.get(key).and_then(toml::Value::as_array).cloned().unwrap_or_default();
            for page in list("pages") {
                if let Some(image) = page.get("image").and_then(|t| self.target(t)) {
                    pages.extend(self.uploads_of(image).0);
                }
            }
            let resources = list("resources");
            let entries = list("palettes").into_iter().chain(
                resources
                    .iter()
                    .flat_map(|r| r.get("palettes").and_then(toml::Value::as_array).cloned().unwrap_or_default()),
            );
            for entry in entries {
                if let Some((image, i)) = self.entry_palette(&entry)
                    && let Some(asset) = self.index.assets.get(&image)
                {
                    self.palette_upload(asset, i, &mut palettes);
                }
            }
        }
        (pages, palettes)
    }

    /// The image and palette a file's palette entry (`{ image, palette }`) names.
    pub fn entry_palette(&self, entry: &toml::Value) -> Option<(String, usize)> {
        let image = self.target(entry.get("image")?)?;
        let name = entry.get("palette")?.as_str()?;
        Some((image.path.clone(), palette_names(image).iter().position(|n| n == name)?))
    }

    fn palette_upload(&self, asset: &Asset, palette: usize, out: &mut Vec<PaletteUpload>) {
        let declared = array(asset, "palettes").get(palette);
        let Some(at) = declared.and_then(|p| self.place(p)) else { return };
        let bpp = bpp_of(asset.fields.get("bpp"), 8);
        let deepest = cells_of(asset).iter().filter(|c| c.palette == Some(palette)).map(|c| c.bpp).max().unwrap_or(bpp);
        let n = if deepest == 4 { 16.0 } else { 256.0 };
        out.push(PaletteUpload {
            image: asset.path.clone(),
            palette,
            rect: egui::Rect::from_min_size(at, egui::vec2(n, 1.0)),
        });
    }
}
