//! An open project as the editor holds it: its index, kept up to date by a watcher on its layers, the asset tree, the
//! selection with its history, the pictures decoded so far, and the link agents reach it by (`mcp::link`).

use anyhow::Result;
use eframe::egui;
use project::Project;
use project::cache::Cache;
use project::index::{Asset, Index};
use project::lock::Lock;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use crate::settings::Upscale;
use crate::watch::{Change, Watch};

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
    /// The project's lock, held while the workspace is open: no other process works on the project meanwhile.
    _lock: Lock,
    cache_dir: PathBuf,
    /// The watcher on the layers, started with the first frame (it wakes the window).
    watch: Option<Watch>,
    /// Why the layers are not (or no longer fully) watched.
    pub watch_error: Option<String>,
    /// The agents' link, started with the watcher; their requests, answered between frames.
    link: Option<mcp::link::Listener>,
    requests: Option<mpsc::Receiver<(mcp::Request, mpsc::Sender<mcp::Reply>)>>,
    /// Why agents cannot reach the editor.
    pub link_error: Option<String>,
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

/// What a watcher batch did to the workspace.
pub enum Update {
    Nothing,
    /// This many assets were added, changed or removed.
    Refreshed(usize),
    /// `project.toml` changed: its layers may have, so the project is opened again.
    Reopen,
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

pub use preview::role;
use preview::{array, bpp, is_texture, palette_names, texels_per_halfword, variant_names};

fn vec2((x, y): (f32, f32)) -> egui::Vec2 {
    egui::vec2(x, y)
}

fn pair(value: Option<&toml::Value>) -> Option<egui::Vec2> {
    preview::pair(value).map(vec2)
}

fn to_rect(r: preview::Rect) -> egui::Rect {
    egui::Rect::from_min_size(egui::pos2(r.x, r.y), egui::vec2(r.width, r.height))
}

/// A `rect = [x, y, w, h]` as a rectangle.
fn rect(value: &toml::Value) -> Option<egui::Rect> {
    preview::rect(value).map(to_rect)
}

/// A cell as drawn in its image (`preview::Cell`), in the editor's rectangles.
pub struct Cell {
    pub name: String,
    pub rect: egui::Rect,
    pub bpp: u8,
    pub palette: Option<usize>,
}

/// An atlas's cells.
pub fn cells_of(asset: &Asset) -> Vec<Cell> {
    preview::cells_of(asset)
        .into_iter()
        .map(|c| Cell { name: c.name, rect: to_rect(c.rect), bpp: c.bpp, palette: c.palette })
        .collect()
}

fn color_image(image: &preview::Image) -> egui::ColorImage {
    egui::ColorImage::from_rgba_unmultiplied([image.width, image.height], &image.rgba)
}

/// A decoded picture uploaded, scaled by the upscaling method.
fn show(ctx: &egui::Context, path: &str, chosen: usize, upscale: Upscale, image: &preview::Image) -> Shown {
    #[expect(clippy::cast_precision_loss, reason = "picture sizes")]
    let size = egui::vec2(image.width as f32, image.height as f32);
    let (image, options) = crate::upscale::apply(color_image(image), upscale);
    Shown { texture: ctx.load_texture(format!("{path}#{chosen}#{upscale:?}"), image, options), size }
}

impl Workspace {
    /// Open the project in `dir`.
    ///
    /// # Errors
    /// Its `project.toml` or its assets cannot be read.
    pub fn open(dir: &Path) -> Result<Workspace> {
        let project = Project::load(dir)?;
        // An agent's request holds the project for a moment (`pe2-cli mcp`): outlast one, not a real holder.
        let lock = Lock::acquire_within(&project.root, "pe2-editor", Duration::from_secs(2))?;
        let cache = Cache::open(&project, &lock)?;
        let index = Index::open(&project, &cache)?;
        let tree = Node::build(&index);
        Ok(Workspace {
            project,
            index,
            _lock: lock,
            cache_dir: cache.dir,
            watch: None,
            watch_error: None,
            link: None,
            requests: None,
            link_error: None,
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

    /// Start watching the layers, once: every change on disk is then brought in by `update`.
    pub fn watch(&mut self, ctx: &egui::Context) {
        if self.watch.is_some() || self.watch_error.is_some() {
            return;
        }
        let root = &self.project.root;
        let roots: Vec<PathBuf> = self.project.layers.iter().map(|l| l.path.clone()).collect();
        // Each layer's changeset sits beside its directory; project.toml in the project's own.
        let mut shallow: Vec<PathBuf> =
            self.project.layers.iter().filter_map(|l| l.changeset.parent().map(Path::to_path_buf)).collect();
        shallow.push(root.clone());
        shallow.dedup();
        let ignore = vec![self.cache_dir.clone(), root.join("roms"), root.join("build")];
        match Watch::start(&roots, &shallow, ignore, ctx.clone()) {
            Ok(watch) => self.watch = Some(watch),
            Err(e) => self.watch_error = Some(format!("the layers are not watched: {e}")),
        }
        self.serve(ctx);
    }

    /// Listen for agents: each request is passed to the frame loop (`answer_agents`), which wakes for it.
    fn serve(&mut self, ctx: &egui::Context) {
        let (to_frames, from_agents) = mpsc::channel::<(mcp::Request, mpsc::Sender<mcp::Reply>)>();
        let ctx = ctx.clone();
        let started = mcp::link::Listener::start(&self.project.root, move |request| {
            let (reply, answered) = mpsc::channel();
            let delivered = to_frames.send((request, reply)).is_ok();
            ctx.request_repaint();
            let closed = || mcp::Reply::Error("the editor closed the project".into());
            if delivered { answered.recv().unwrap_or_else(|_| closed()) } else { closed() }
        });
        match started {
            Ok(link) => {
                self.link = Some(link);
                self.requests = Some(from_agents);
            }
            Err(e) => self.link_error = Some(format!("agents cannot reach the editor: {e}")),
        }
    }

    /// Answer the agents' requests that arrived since the last frame, against the index and the selection as shown.
    pub fn answer_agents(&mut self) {
        let Some(requests) = &self.requests else { return };
        let pending: Vec<_> = requests.try_iter().collect();
        for (request, reply) in pending {
            let selection = self.selected.clone();
            let variant = selection.as_deref().and_then(|s| self.variants_of(s).get(self.variant(s)).cloned());
            let editor = mcp::EditorState { selection, variant, show: None };
            let mut view = mcp::View { project: &self.project, index: &self.index, editor: Some(editor) };
            let answer = mcp::answer(&mut view, &request);
            let show = view.editor.and_then(|e| e.show);
            if let Some(path) = show {
                self.navigate(path);
            }
            let _ = reply.send(answer);
        }
    }

    /// Bring in what the watcher saw: the index refreshed, the pictures of the assets that changed forgotten, the
    /// tree built again, a selection that is gone dropped.
    pub fn update(&mut self) -> Update {
        let Some(watch) = &self.watch else { return Update::Nothing };
        let mut paths = Vec::new();
        for change in watch.poll() {
            match change {
                Change::Paths(changed) => paths.extend(changed),
                Change::Error(e) => self.watch_error = Some(format!("changes may be missed: {e}")),
            }
        }
        if paths.is_empty() {
            return Update::Nothing;
        }
        let refreshed = match self.index.refresh(&self.project, &paths) {
            Ok(refreshed) => refreshed,
            Err(e) => {
                self.watch_error = Some(format!("the index could not follow a change: {e:#}"));
                return Update::Nothing;
            }
        };
        if refreshed.project_file {
            return Update::Reopen;
        }
        if refreshed.assets.is_empty() {
            return Update::Nothing;
        }
        self.pictures.retain(|(image, ..), _| !refreshed.assets.contains(image));
        self.strips.retain(|(image, _), _| !refreshed.assets.contains(image));
        self.tree = Node::build(&self.index);
        if self.selected.as_ref().is_some_and(|s| !self.index.assets.contains_key(s)) {
            self.selected = None;
        }
        Update::Refreshed(refreshed.assets.len())
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
        self.index.assets.get(path).is_some_and(preview::has_picture)
    }

    /// The picture of an image - a background, a texture or an atlas, in its chosen variant - decoded once per variant
    /// and upscaling method.
    pub fn picture(&mut self, ctx: &egui::Context, path: &str, upscale: Upscale) -> Option<&Picture> {
        let chosen = self.variant(path);
        let key = (path.to_string(), chosen, upscale);
        if !self.pictures.contains_key(&key) {
            let decoded = preview::picture(self.index.assets.get(path)?, chosen)?;
            self.pictures.insert(
                key.clone(),
                decoded.map_err(|e| format!("{e:#}")).map(|image| show(ctx, path, chosen, upscale, &image)),
            );
        }
        self.pictures.get(&key)
    }

    /// A palette's colors, as a strip.
    pub fn palette(&mut self, ctx: &egui::Context, path: &str, palette: usize) -> Option<&Picture> {
        let key = (path.to_string(), palette);
        if !self.strips.contains_key(&key) {
            let colors = preview::palette_colors(self.index.assets.get(path)?, palette)?;
            let strip = colors.map_err(|e| format!("{e:#}")).map(|colors| {
                let pixels: Vec<egui::Color32> = colors
                    .into_iter()
                    .map(|c| {
                        let [red, green, blue, alpha] = preview::rgba(c);
                        egui::Color32::from_rgba_unmultiplied(red, green, blue, alpha)
                    })
                    .collect();
                #[expect(clippy::cast_precision_loss, reason = "palette sizes")]
                let size = egui::vec2(pixels.len() as f32, 1.0);
                let image = egui::ColorImage::new([pixels.len().max(1), 1], pixels);
                let options = egui::TextureOptions::NEAREST;
                Shown { texture: ctx.load_texture(format!("{path}#{palette}#palette"), image, options), size }
            });
            self.strips.insert(key.clone(), strip);
        }
        self.strips.get(&key)
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
            .filter_map(|a| Some((a.path.clone(), rect(a.fields().get("rect")?)?)))
            .collect()
    }

    /// A placement's VRAM halfword: its slot's corner plus its `at`.
    fn place(&self, placement: &toml::Value) -> Option<egui::Pos2> {
        let slot = rect(self.target(placement.get("slot")?)?.fields().get("rect")?)?;
        Some(slot.min + pair(placement.get("at"))?)
    }

    /// What an asset puts in VRAM: an image its pages and its placed palettes, a file what its image, palette and
    /// bundle slots upload.
    pub fn uploads_of(&self, asset: &Asset) -> (Vec<Upload>, Vec<PaletteUpload>) {
        let (mut pages, mut palettes) = (vec![], vec![]);
        if is_texture(asset) {
            let bpp = bpp(asset);
            if let (Some(origin), Some(size)) =
                (asset.fields().get("page").and_then(|p| self.place(p)), pair(asset.fields().get("size")))
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
        let bpp = bpp(asset);
        let deepest = cells_of(asset).iter().filter(|c| c.palette == Some(palette)).map(|c| c.bpp).max().unwrap_or(bpp);
        let n = if deepest == 4 { 16.0 } else { 256.0 };
        out.push(PaletteUpload {
            image: asset.path.clone(),
            palette,
            rect: egui::Rect::from_min_size(at, egui::vec2(n, 1.0)),
        });
    }
}
