//! An open project as the editor holds it: its index, kept up to date by a watcher on its layers, the tree of its
//! entries, the selection with its history, the pictures decoded so far, the user's view of the layers (eyes, hand
//! locks, the active layer), and the link agents reach it by (`mcp::link`).

use anyhow::{Context, Result};
use eframe::egui;
use project::Project;
use project::cache::Cache;
use project::ext::{Extensions, Lens};
use project::history::{History, StepError};
use project::index::{Asset, Index, Kind, LayerInfo};
use project::lock::Lock;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use crate::settings::{Preview, Upscale};
use crate::watch::{Change, Watch};

/// A node of the tree of the resolved project: its children and, where an entry has this path, that entry (its real
/// path, a bundle's with `.asset`) and the asset it is, or that it is a plain file. An image with several variants -
/// its other looks - is a virtual folder of them: choosing one shows the image so.
#[derive(Default)]
pub struct Node {
    pub children: BTreeMap<String, Node>,
    pub asset: Option<String>,
    pub entry: Option<Entry>,
    /// The resolved entry here, by its real path.
    pub path: Option<String>,
    pub file: bool,
}

/// A virtual entry: an image and one of its variants.
#[derive(Clone, PartialEq, Eq)]
pub struct Entry {
    pub image: String,
    pub variant: usize,
}

impl Node {
    /// The tree of the resolved entries, with an asset's looks as its children where its extension draws several. Only
    /// an asset whose declaration names `variants` is asked (the index lists those without parsing a declaration).
    fn build(index: &Index, extensions: &Extensions) -> Node {
        let mut root = Node::default();
        for (real, entry) in &index.entries {
            let path = real.strip_suffix(project::index::BUNDLE_SUFFIX).unwrap_or(real);
            let mut node = &mut root;
            for part in path.split('/') {
                node = node.children.entry(part.to_string()).or_default();
            }
            node.path = Some(real.clone());
            match entry.kind {
                Kind::Folder => {}
                Kind::File => node.file = true,
                Kind::Asset => {
                    let Some(asset) = index.assets.get(path) else { continue };
                    node.asset = Some(path.to_string());
                    let names = if asset.variants.is_empty() { vec![] } else { extensions.looks(asset) };
                    if names.len() > 1 {
                        for (i, name) in names.into_iter().enumerate() {
                            let entry = Some(Entry { image: path.to_string(), variant: i });
                            node.children.insert(name, Node { entry, ..Node::default() });
                        }
                    }
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

/// One user's view of the stack, kept in the project cache (`editor/layers.toml`), so toggling a layer never changes a
/// tracked file: the layers hidden from the view (the eyes), the layers locked by hand, the active layer, and the
/// layer shown alone, if one is - a mode over the others, which leaves them as they were when it ends.
#[derive(Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LayerView {
    #[serde(default)]
    pub hidden: BTreeSet<String>,
    #[serde(default)]
    pub locked: BTreeSet<String>,
    #[serde(default)]
    pub active: Option<String>,
    /// Shown alone: the layers above it hidden, those below shown, every other layer locked, and it the active one.
    #[serde(default)]
    pub solo: Option<String>,
}

impl LayerView {
    /// The layers hidden from the view: the eyes', or, while a layer is shown alone, the layers above it.
    fn hidden(&self, layers: &[LayerInfo]) -> BTreeSet<String> {
        match &self.solo {
            Some(solo) => layers.iter().skip_while(|l| l.name != *solo).skip(1).map(|l| l.name.clone()).collect(),
            None => self.hidden.clone(),
        }
    }
}

const LAYER_VIEW: &str = "editor/layers.toml";

pub struct Workspace {
    pub project: Project,
    pub index: Index,
    /// The stack as the layer panel shows it, bottom to top.
    pub layers: Vec<LayerInfo>,
    /// What was done this session, to undo and redo.
    pub history: History,
    /// The label of an edit a crash left half done, rolled back when the project was opened.
    pub recovered: Option<String>,
    /// The tree's thumbnails, kept as artifacts in the cache.
    pub thumbs: crate::thumbs::Thumbs,
    /// The extensions, and what they register: the kinds' icons, fields, pictures, thumbnails and views.
    pub extensions: Arc<Extensions>,
    /// The views the panes show, by (pane, asset, view): made when first shown, made again when the asset changes.
    views: HashMap<(egui::Id, String, usize), Box<dyn pe2_ext::View>>,
    /// The view an asset is shown in, where its kind and role have several.
    pub view_choice: HashMap<String, usize>,
    /// What the views show, uploaded.
    textures: crate::ext_ui::Textures,
    pub layer_view: LayerView,
    /// The project's lock, held while the workspace is open: no other process works on the project meanwhile.
    lock: Lock,
    cache_dir: PathBuf,
    /// The watcher on the layers, started with the first frame (it wakes the window).
    watch: Option<Watch>,
    /// Why the layers are not (or no longer fully) watched.
    pub watch_error: Option<String>,
    /// The agents' link, started with the watcher; their requests, answered between frames.
    link: Option<mcp::link::Listener>,
    requests: Option<mpsc::Receiver<(mcp::Request, mpsc::Sender<mcp::Reply>)>>,
    /// Requests using the editor's window, which the app's driver answers across frames.
    pub window_requests: Vec<crate::drive::Request>,
    /// Why agents cannot reach the editor.
    pub link_error: Option<String>,
    pub tree: Node,
    /// The tree's paths (an asset's without `.asset`) that can be modified, and the folders holding one: the rest is
    /// drawn dimmed.
    editable: HashSet<String>,
    pub selected: Option<String>,
    back: Vec<String>,
    forward: Vec<String>,
    pub filter: String,
    /// An asset just navigated to: the tree opens its ancestors and scrolls to it once.
    pub reveal: Option<String>,
    /// Decoded pictures by (image, variant, upscaling).
    pictures: HashMap<(String, usize, Upscale), Picture>,
    /// Parts of pictures (palettes' strips), as the inspector shows them.
    pub parts: Parts,
    /// The inspector views - the kinds' and roles', or the importers' - by (asset, the importer's, view).
    inspector_views: HashMap<(String, bool, usize), Box<dyn pe2_ext::View>>,
    /// The panels' views, by pane.
    panels: HashMap<egui::Id, Box<dyn pe2_ext::View>>,
    /// What a view set of the asset it shows this frame, for the app to make after drawing.
    pub view_edit: Option<EditRequest>,
    /// The variant an image is shown in (an index into `variants_of`); without one, its first.
    pub variant_choice: HashMap<String, usize>,
    /// A field being edited in the inspector.
    pub field_edit: Option<FieldEdit>,
    /// The files shown as text or bytes, and which file of an asset without a picture is shown.
    pub viewer: crate::viewer::Viewer,
    pub viewed_file: HashMap<String, String>,
    /// An asset's declaration being edited in the inspector, as text: the asset, the text.
    pub declaration_draft: Option<(String, String)>,
}

/// An edit asked for in a pane, of the entry with this uid, made in the active layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditRequest {
    Override(String),
    /// Override in this layer, not the active one: code, overridden in a files layer chosen for it.
    OverrideIn(String, String),
    /// Ask for a new name, then rename: the uid and the entry's real path.
    Rename(String, String),
    /// Rename to this real path.
    RenameTo(String, String),
    Delete(String),
    Resolve(String, project::edit::Resolution),
    /// Write a file of the entry (its `asset.toml`; empty: a plain file itself), labeled for the history; edits of one
    /// merge key made in a row are one step.
    Write {
        uid: String,
        file: String,
        bytes: Vec<u8>,
        label: String,
        merge: Option<String>,
    },
}

/// A field of an asset's declaration being edited in the inspector: the asset, the field, the text typed.
pub struct FieldEdit {
    pub asset: String,
    pub key: String,
    pub text: String,
}

/// What a watcher batch did to the workspace.
pub enum Update {
    Nothing,
    /// This many assets were added, changed or removed.
    Refreshed(usize),
    /// `project.toml` changed: its layers may have, so the project is opened again.
    Reopen,
}

/// Parts of assets' pictures, uploaded once each: a palette's strip of colors (`palettes/<name>`), as its kind draws it.
#[derive(Default)]
pub struct Parts {
    shown: HashMap<(String, String), Picture>,
}

impl Parts {
    /// An asset's part, uploaded; none where its kind draws no such part.
    pub fn get(&mut self, ctx: &egui::Context, extensions: &Extensions, asset: &Asset, part: &str) -> Option<&Picture> {
        let key = (asset.path.clone(), part.to_string());
        if !self.shown.contains_key(&key) {
            let drawn = extensions.part(asset, part)?;
            let shown = drawn.map_err(|e| format!("{e:#}")).map(|image| {
                #[expect(clippy::cast_precision_loss, reason = "picture sizes")]
                let size = egui::vec2(image.width as f32, image.height as f32);
                let image = crate::ext_ui::color_image(&image);
                let texture = ctx.load_texture(format!("{}#{part}", asset.path), image, egui::TextureOptions::NEAREST);
                Shown { texture, size }
            });
            self.shown.insert(key.clone(), shown);
        }
        self.shown.get(&key)
    }

    fn forget(&mut self, assets: &BTreeSet<String>) {
        self.shown.retain(|(asset, _), _| !assets.contains(asset));
    }
}

/// A field's array; empty where it has none.
fn array<'a>(asset: &'a Asset, key: &str) -> &'a [toml::Value] {
    asset.fields().get(key).and_then(toml::Value::as_array).map_or(&[][..], Vec::as_slice)
}

/// A decoded picture uploaded, scaled by the upscaling method.
fn show(ctx: &egui::Context, path: &str, chosen: usize, upscale: Upscale, image: &pe2_ext::Image) -> Shown {
    #[expect(clippy::cast_precision_loss, reason = "picture sizes")]
    let size = egui::vec2(image.width as f32, image.height as f32);
    let (image, options) = crate::upscale::apply(crate::ext_ui::color_image(image), upscale);
    Shown { texture: ctx.load_texture(format!("{path}#{chosen}#{upscale:?}"), image, options), size }
}

impl Workspace {
    /// Open the project in `dir`; with a history, again, keeping this session's history: a change to `project.toml` -
    /// an edit of the stack, or its undoing - opens the project again, and must not end the session's history.
    ///
    /// # Errors
    /// Its `project.toml` or its assets cannot be read.
    pub fn open_keeping(dir: &Path, history: Option<History>) -> Result<Workspace> {
        let project = Project::load(dir)?;
        // An agent's request holds the project for a moment (`pe2-cli mcp`): outlast one, not a real holder.
        let lock = Lock::acquire_within(&project.root, "pe2-editor", Duration::from_secs(2))?;
        let cache = Cache::open(&project, &lock)?;
        // The history first: a transaction a crash left half done is rolled back before the index reads the files.
        let (history, recovered) = match history {
            Some(history) => (history, None),
            None => History::open(&cache)?,
        };
        let mut index = Index::open(&project, &cache)?;
        // The artifacts of assets the project no longer has go; the whole stack's entries are what it has.
        let artifacts = project::artifacts::Artifacts::open(&cache)?;
        let _ = artifacts.sweep(&index.entries.values().map(|e| e.uid.as_str()).collect());
        let extensions = extensions::shared();
        let thumbs = crate::thumbs::Thumbs::start(extensions.clone(), artifacts);
        let layer_view: LayerView = std::fs::read_to_string(cache.path(LAYER_VIEW))
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default();
        index.set_hidden(&layer_view.hidden(&index.layers()));
        let tree = Node::build(&index, &extensions);
        let mut workspace = Workspace {
            project,
            history,
            recovered,
            thumbs,
            extensions,
            views: HashMap::new(),
            view_choice: HashMap::new(),
            textures: crate::ext_ui::Textures::default(),
            layers: index.layers(),
            layer_view,
            index,
            lock,
            cache_dir: cache.dir,
            watch: None,
            watch_error: None,
            link: None,
            requests: None,
            window_requests: Vec::new(),
            link_error: None,
            tree,
            editable: HashSet::new(),
            selected: None,
            back: vec![],
            forward: vec![],
            filter: String::new(),
            reveal: None,
            pictures: HashMap::new(),
            parts: Parts::default(),
            inspector_views: HashMap::new(),
            panels: HashMap::new(),
            view_edit: None,
            variant_choice: HashMap::new(),
            field_edit: None,
            viewer: crate::viewer::Viewer::default(),
            viewed_file: HashMap::new(),
            declaration_draft: None,
        };
        workspace.refresh_editable();
        Ok(workspace)
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
            if request.uses_window() {
                self.window_requests.push((request, reply));
                continue;
            }
            let selection = self.selected.clone();
            let variant = selection.as_deref().and_then(|s| self.variants_of(s).get(self.variant(s)).cloned());
            let editor = mcp::EditorState { selection, variant, show: None };
            let mut view = mcp::View {
                project: &self.project,
                index: &self.index,
                extensions: &self.extensions,
                editor: Some(editor),
            };
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
        self.take_in(&paths)
    }

    /// Give up the workspace, keeping its history for the project opened again.
    pub fn into_history(self) -> History {
        self.history
    }

    /// Undo the last step, or redo the one undone last; forced, over files changed since outside the editor. The
    /// index takes the change in at once, and what was selected when the step was made is shown again. Returns the
    /// step's label and what the index made of it.
    ///
    /// # Errors
    /// Nothing to undo or redo, files changed outside the editor (unless forced), or a file not written.
    pub fn step(&mut self, redo: bool, force: bool) -> Result<(String, Update), StepError> {
        let step = if redo { self.history.redo(force)? } else { self.history.undo(force)? };
        let (label, paths, selection) = (step.label.clone(), step.paths(), step.selection.clone());
        let update = self.take_in(&paths);
        if let Some(path) = selection.filter(|p| self.exists(p)) {
            self.navigate(path);
        }
        Ok((label, update))
    }

    /// Bring changed paths into the index, and the tree and pictures after it.
    fn take_in(&mut self, paths: &[PathBuf]) -> Update {
        if paths.is_empty() {
            return Update::Nothing;
        }
        let refreshed = match self.index.refresh(&self.project, paths) {
            Ok(refreshed) => refreshed,
            Err(e) => {
                self.watch_error = Some(format!("the index could not follow a change: {e:#}"));
                return Update::Nothing;
            }
        };
        if refreshed.project_file {
            return Update::Reopen;
        }
        self.layers = self.index.layers();
        self.tree = Node::build(&self.index, &self.extensions);
        self.refresh_editable();
        if refreshed.assets.is_empty() {
            return Update::Nothing;
        }
        self.pictures.retain(|(image, ..), _| !refreshed.assets.contains(image));
        self.parts.forget(&refreshed.assets);
        self.inspector_views.retain(|(asset, ..), _| !refreshed.assets.contains(asset));
        self.panels.clear();
        self.views.retain(|(_, asset, _), _| !refreshed.assets.contains(asset));
        self.thumbs.forget(&refreshed.assets);
        self.drop_lost_selection();
        Update::Refreshed(refreshed.assets.len())
    }

    /// Why the project is open without a lock: its file system cannot lock files (a WSL folder opened from Windows).
    pub fn unprotected(&self) -> Option<&str> {
        self.lock.unprotected()
    }

    /// The entry at a tree path when it is not an asset (a plain file, a folder): its real path and the entry.
    pub fn entry_at(&self, path: &str) -> Option<(&String, &project::index::Entry)> {
        if self.index.assets.contains_key(path) {
            return None;
        }
        self.index.entries.get_key_value(path)
    }

    /// Whether a tree path is an asset or an entry of the view.
    pub fn exists(&self, path: &str) -> bool {
        self.index.assets.contains_key(path) || self.index.entries.contains_key(path)
    }

    fn drop_lost_selection(&mut self) {
        if self
            .selected
            .as_ref()
            .is_some_and(|s| !self.index.assets.contains_key(s) && !self.index.entries.contains_key(s))
        {
            self.selected = None;
        }
    }

    // --- the layers ---------------------------------------------------------------------------------------------------

    /// A layer is locked by nature where only its maker writes it - the game layer, a read-only tree - or by hand.
    pub fn locked_by_nature(layer: &LayerInfo) -> bool {
        layer.read_only || (!layer.mounted && !layer.writable)
    }

    pub fn is_locked(&self, layer: &LayerInfo) -> bool {
        Self::locked_by_nature(layer) || self.layer_view.locked.contains(&layer.name) || self.locked_by_solo(layer)
    }

    /// Locked because another layer is shown alone.
    pub fn locked_by_solo(&self, layer: &LayerInfo) -> bool {
        self.layer_view.solo.as_ref().is_some_and(|solo| *solo != layer.name)
    }

    /// Hidden from the view: by its eye, or because a layer below it is shown alone.
    pub fn is_hidden(&self, layer: &LayerInfo) -> bool {
        self.layer_view.hidden(&self.layers).contains(&layer.name)
    }

    /// The active layer: the one shown alone, else the user's choice, else the topmost asset layer not locked by
    /// nature.
    pub fn active_layer(&self) -> Option<&LayerInfo> {
        let chosen = self.layer_view.solo.as_ref().or(self.layer_view.active.as_ref());
        let chosen = chosen.and_then(|a| self.layers.iter().find(|l| l.name == *a));
        chosen.or_else(|| self.layers.iter().rev().find(|l| !l.mounted && !Self::locked_by_nature(l)))
    }

    /// What the layer panel shows for an entry beside its name: the layer that overrode or moved it, or the one of the
    /// user's own that made it; nothing for what comes from the game or a mounted tree untouched.
    pub fn badge(&self, real: &str) -> Option<String> {
        let entry = self.index.entries.get(real)?;
        if let Some(layer) = entry.overrides.last() {
            return Some(format!("overridden in {layer}"));
        }
        if let Some(layer) = &entry.moved {
            return Some(format!("moved by {layer}"));
        }
        let home = self.layers.iter().find(|l| l.name == entry.home)?;
        (!home.mounted && !Self::locked_by_nature(home)).then(|| entry.home.clone())
    }

    fn save_layer_view(&self) -> Result<()> {
        let path = self.cache_dir.join(LAYER_VIEW);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        project::cache::write_atomic(&path, toml::to_string(&self.layer_view)?.as_bytes())
    }

    /// Show the view as the eyes say: the index's resolved entries, the tree and the pictures follow.
    fn apply_eyes(&mut self) -> Result<()> {
        if self.index.set_hidden(&self.layer_view.hidden(&self.layers)) {
            self.tree = Node::build(&self.index, &self.extensions);
            self.pictures.clear();
            self.parts = Parts::default();
            self.inspector_views.clear();
            self.panels.clear();
            self.views.clear();
            self.thumbs.clear();
            self.drop_lost_selection();
        }
        self.refresh_editable();
        self.save_layer_view()
    }

    /// Which entries can be modified: those whose content comes from a layer that is not locked - by nature, by hand
    /// or by a solo - and, while a layer is soloed, every entry it creates, overrides or moves; with the folders
    /// holding any of them.
    fn refresh_editable(&mut self) {
        let unlocked: HashSet<&str> =
            self.layers.iter().filter(|l| !self.is_locked(l)).map(|l| l.name.as_str()).collect();
        let solo = self.layer_view.solo.as_deref();
        let mut editable = HashSet::new();
        for (real, entry) in &self.index.entries {
            let touched = solo.is_some_and(|solo| {
                entry.home == solo || entry.overrides.iter().any(|l| l == solo) || entry.moved.as_deref() == Some(solo)
            });
            if !unlocked.contains(entry.layer.as_str()) && !touched {
                continue;
            }
            let mut path = real.strip_suffix(project::index::BUNDLE_SUFFIX).unwrap_or(real);
            while editable.insert(path.to_string()) {
                let Some((parent, _)) = path.rsplit_once('/') else { break };
                path = parent;
            }
        }
        self.editable = editable;
    }

    /// Whether what is at a tree path can be modified, or holds something that can.
    pub fn is_editable(&self, path: &str) -> bool {
        self.editable.contains(path)
    }

    /// Toggle a layer's eye.
    ///
    /// # Errors
    /// The user's view cannot be saved.
    /// While a layer is soloed, the solo decides what is shown: the eyes wait until it ends.
    pub fn toggle_eye(&mut self, name: &str) -> Result<()> {
        if self.layer_view.solo.is_some() {
            return Ok(());
        }
        if !self.layer_view.hidden.remove(name) {
            self.layer_view.hidden.insert(name.to_string());
        }
        self.apply_eyes()
    }

    /// Show a layer alone - the layers above it hidden, those below shown (its entries stand on them), every other
    /// layer locked, it the active one - or, when it is shown alone already, end that: the eyes, locks and active
    /// layer are as they were.
    ///
    /// # Errors
    /// The user's view cannot be saved.
    pub fn solo(&mut self, name: &str) -> Result<()> {
        self.layer_view.solo =
            if self.layer_view.solo.as_deref() == Some(name) { None } else { Some(name.to_string()) };
        self.apply_eyes()
    }

    /// Lock a layer by hand, or unlock it.
    ///
    /// # Errors
    /// The user's view cannot be saved.
    pub fn toggle_lock(&mut self, name: &str) -> Result<()> {
        if self.layer_view.solo.is_some() {
            return Ok(());
        }
        if !self.layer_view.locked.remove(name) {
            self.layer_view.locked.insert(name.to_string());
        }
        self.refresh_editable();
        self.save_layer_view()
    }

    /// Make a layer the active one: where new entries, edits and overrides go.
    ///
    /// # Errors
    /// The user's view cannot be saved.
    pub fn activate(&mut self, name: &str) -> Result<()> {
        if self.layer_view.solo.is_some() {
            return Ok(());
        }
        self.layer_view.active = Some(name.to_string());
        self.save_layer_view()
    }

    /// The layer edits go to: the active one, if it can be written now.
    ///
    /// # Errors
    /// Why it cannot: none is active, it is locked (by nature, by hand, by a solo).
    pub fn editable_layer(&self) -> Result<&LayerInfo, String> {
        let layer = self.active_layer().ok_or("no layer is active")?;
        if self.is_locked(layer) {
            let why = if self.locked_by_solo(layer) { "while another layer is soloed" } else { "it is locked" };
            return Err(format!("{} cannot be edited: {why}", layer.name));
        }
        Ok(layer)
    }

    /// Make an edit in the active layer: a step of the history, the index taking it in at once. Returns what was done.
    ///
    /// # Errors
    /// The active layer cannot be written, the edit does not apply, or files cannot be written.
    pub fn edit(&mut self, request: &EditRequest) -> anyhow::Result<String> {
        use project::edit;
        let name = match request {
            // A files layer chosen for code: it is to be writable, as the active one is.
            EditRequest::OverrideIn(_, layer) => {
                let chosen = self.layers.iter().find(|l| l.name == *layer);
                let chosen = chosen.with_context(|| format!("the project has no layer {layer}"))?;
                if self.is_locked(chosen) {
                    anyhow::bail!("{layer} is locked");
                }
                layer.clone()
            }
            _ => self.editable_layer().map_err(anyhow::Error::msg)?.name.clone(),
        };
        let uid = match request {
            EditRequest::Override(uid)
            | EditRequest::OverrideIn(uid, _)
            | EditRequest::Rename(uid, _)
            | EditRequest::RenameTo(uid, _)
            | EditRequest::Delete(uid)
            | EditRequest::Resolve(uid, _)
            | EditRequest::Write { uid, .. } => uid.clone(),
        };
        // Code goes to a files layer: an override of it in an asset layer is asked of the user first
        // (`override_choice`), and a write into it is made there.
        if matches!(request, EditRequest::Override(_) | EditRequest::Write { .. })
            && let Some(choices) = self.override_choice(&uid).map_err(anyhow::Error::msg)?
        {
            anyhow::bail!(
                "code is overridden in a files layer ({})",
                if choices.is_empty() { "none yet".into() } else { choices.join(", ") }
            );
        }
        let (project, index, history) = (&self.project, &self.index, &mut self.history);
        let paths = match request {
            EditRequest::Override(uid) | EditRequest::OverrideIn(uid, _) => {
                edit::override_entry(project, index, &name, uid, history)?
            }
            EditRequest::Delete(uid) => edit::delete(project, index, &name, uid, history)?,
            EditRequest::RenameTo(uid, to) => edit::rename(project, index, &name, uid, to, history)?,
            EditRequest::Resolve(uid, resolution) => edit::resolve(project, index, &name, uid, *resolution, history)?,
            EditRequest::Write { uid, file, bytes, label, merge } => {
                edit::write_file(project, index, &name, uid, file, bytes, label.clone(), merge.clone(), history)?
            }
            EditRequest::Rename(..) => return Ok(String::new()),
        };
        self.history.note_selection(self.selected.clone());
        let label = self.history.done().last().map_or_default(|s| s.label.clone());
        self.take_in(&paths);
        Ok(label)
    }

    /// The stack's own layers (not the mounts), bottom to top.
    pub fn stack_order(&self) -> Vec<String> {
        self.layers.iter().filter(|l| !l.mounted).map(|l| l.name.clone()).collect()
    }

    /// Put the stack's own layers in this order (bottom to top), if it keeps what each needs: written to
    /// `project.toml`, which the watcher sees and the project is opened again.
    ///
    /// # Errors
    /// Why the order cannot be: a layer it would put below one it needs, or `project.toml` not written.
    pub fn reorder(&mut self, order: &[String]) -> Result<()> {
        project::stack::reorder(&self.project, &self.layers, order, &mut self.history)
    }

    /// Add a new, empty layer of a kind at the top of the stack.
    ///
    /// # Errors
    /// The name is not one a layer may take, or it cannot be written.
    pub fn add_layer(&mut self, name: &str, kind: project::LayerKind) -> Result<()> {
        project::stack::add(&self.project, name, kind, &mut self.history)
    }

    /// Where an override of an entry (by uid) goes: the active layer (none), or - code (an entry of a files layer)
    /// while an asset layer is active - one of the files layers above its own, which can be written: the user's
    /// choice, these the candidates (perhaps none: a files layer is to be made).
    ///
    /// # Errors
    /// The active layer cannot be written, or holds plain files only and the entry is an asset.
    pub fn override_choice(&self, uid: &str) -> Result<Option<Vec<String>>, String> {
        use project::LayerKind;
        let active = self.editable_layer()?;
        let Some(entry) = self.index.entries.values().find(|e| e.uid == uid) else { return Ok(None) };
        if active.kind == LayerKind::Files && entry.kind == project::index::Kind::Asset {
            return Err(format!("{} is a files layer: it holds plain files, not assets", active.name));
        }
        let home = self.layers.iter().position(|l| l.name == entry.home);
        let code = home.is_some_and(|h| self.layers[h].kind == LayerKind::Files);
        if !code || active.kind == LayerKind::Files {
            return Ok(None);
        }
        let above = home.map_or(0, |h| h + 1);
        let candidates = self.layers[above..]
            .iter()
            .filter(|l| l.kind == LayerKind::Files && !l.mounted && !self.is_locked(l))
            .map(|l| l.name.clone())
            .collect();
        Ok(Some(candidates))
    }

    /// Take a layer out of the stack; its files stay on disk.
    ///
    /// # Errors
    /// Another layer needs it, it is the game layer or a mount, or `project.toml` cannot be rewritten.
    pub fn remove_layer(&mut self, name: &str) -> Result<()> {
        project::stack::remove(&self.project, &self.layers, name, &mut self.history)
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
        let (uid, path) = project::index::reference(value)?;
        self.index.resolve(&uid, path.as_deref().unwrap_or_default())
    }

    /// An asset's looks by name.
    pub fn variants_of(&self, path: &str) -> Vec<String> {
        self.index.assets.get(path).map_or_default(|a| self.extensions.looks(a))
    }

    /// The variant an image is shown in.
    pub fn variant(&self, path: &str) -> usize {
        let n = self.variants_of(path).len();
        self.variant_choice.get(path).copied().filter(|&v| v < n).unwrap_or(0)
    }

    /// Whether an asset has a picture.
    pub fn has_picture(&self, path: &str) -> bool {
        self.index.assets.get(path).is_some_and(|a| self.extensions.has_picture(a))
    }

    /// The picture of an asset, in its chosen look, as its extension draws it - decoded once per look and upscaling
    /// method.
    pub fn picture(&mut self, ctx: &egui::Context, path: &str, upscale: Upscale) -> Option<&Picture> {
        let chosen = self.variant(path);
        let key = (path.to_string(), chosen, upscale);
        if !self.pictures.contains_key(&key) {
            let decoded = self.extensions.picture(self.index.assets.get(path)?, chosen)?;
            self.pictures.insert(
                key.clone(),
                decoded.map_err(|e| format!("{e:#}")).map(|image| show(ctx, path, chosen, upscale, &image)),
            );
        }
        self.pictures.get(&key)
    }

    /// Whether the preview shows an asset by its kind's views: it has one, and a picture where its kind draws one.
    pub fn has_view(&self, path: &str) -> bool {
        self.index.assets.get(path).is_some_and(|a| {
            !self.extensions.views(a).is_empty()
                && (self.extensions.kind_of(a).is_none_or(|k| k.picture.is_none()) || self.extensions.has_picture(a))
        })
    }

    /// The assets the preview shows for an asset, as (label, asset): itself, where it has a view; or what a container's
    /// slots hold - the assets filling them, the images its image slots upload.
    pub fn previews_of(&self, asset: &Asset) -> Vec<(Option<String>, String)> {
        if self.has_view(&asset.path) {
            return vec![(None, asset.path.clone())];
        }
        let mut out: Vec<(Option<String>, String)> = Vec::new();
        let mut push = |label: String, a: &Asset| {
            if self.has_view(&a.path) && !out.iter().any(|(_, p)| *p == a.path) {
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

    /// Show an asset's view in a pane - the one chosen, where its kind and role have several - made when first shown.
    /// `count` views share the pane's height.
    pub fn show_view(
        &mut self,
        ui: &mut egui::Ui,
        pane: egui::Id,
        path: &str,
        preview: Preview,
        count: usize,
        nav: &mut Option<String>,
    ) {
        let look = self.variant(path);
        // As the inspector's own fields: an asset from below is overridden into the active layer.
        let editable = self.editable_layer().is_ok();
        let Some(asset) = self.index.assets.get(path) else { return };
        let views = self.extensions.views(asset);
        if views.is_empty() {
            return;
        }
        let mut chosen = self.view_choice.get(path).copied().filter(|&v| v < views.len()).unwrap_or(0);
        if views.len() > 1 {
            ui.horizontal(|ui| {
                for (i, view) in views.iter().enumerate() {
                    ui.selectable_value(&mut chosen, i, &view.name);
                }
            });
            self.view_choice.insert(path.to_string(), chosen);
        }
        let make = &views[chosen].make;
        let view = self.views.entry((pane, path.to_string(), chosen)).or_insert_with(|| make(&self.extensions.lend(asset)));
        let lens = Lens {
            index: &self.index,
            extensions: &self.extensions,
            selection: self.selected.as_deref(),
            looks: Some(&self.variant_choice),
        };
        let frame = crate::ext_ui::Frame { asset: Some(asset), look, preview, count, editable };
        let mut sets = vec![];
        let mut host = crate::ext_ui::Host::new(ui, &lens, &mut self.textures, nav, &mut sets, frame);
        view.show(&mut pe2_ext::Ui::new(&mut host));
        drop(host);
        if !sets.is_empty() {
            self.view_edit = crate::inspector::set_fields(asset, &sets);
        }
    }

    /// Show an asset's inspector views - what its kind and role draw of it after the property sheet, or (`import`) its
    /// importer of its import settings - made when first shown.
    pub fn show_inspector_views(&mut self, ui: &mut egui::Ui, path: &str, nav: &mut Option<String>, import: bool) {
        let look = self.variant(path);
        // As the inspector's own fields: an asset from below is overridden into the active layer.
        let editable = self.editable_layer().is_ok();
        let Some(asset) = self.index.assets.get(path) else { return };
        let lens = Lens {
            index: &self.index,
            extensions: &self.extensions,
            selection: self.selected.as_deref(),
            looks: Some(&self.variant_choice),
        };
        let defs = if import {
            self.extensions.importer_of(asset).map_or_default(|i| i.inspector.iter().collect())
        } else {
            self.extensions.inspector(asset)
        };
        for (i, def) in defs.into_iter().enumerate() {
            let key = (path.to_string(), import, i);
            let view = self.inspector_views.entry(key).or_insert_with(|| (def.make)(&self.extensions.lend(asset)));
            let frame = crate::ext_ui::Frame { asset: Some(asset), look, preview: Preview::default(), count: 1, editable };
            let mut sets = vec![];
            let mut host = crate::ext_ui::Host::new(ui, &lens, &mut self.textures, nav, &mut sets, frame);
            view.show(&mut pe2_ext::Ui::new(&mut host));
            drop(host);
            if !sets.is_empty() {
                self.view_edit = crate::inspector::set_fields(asset, &sets);
            }
        }
    }

    /// Show a panel in a pane, its view made when the pane first shows it.
    pub fn show_panel(&mut self, ui: &mut egui::Ui, pane: egui::Id, name: &str, preview: Preview, nav: &mut Option<String>) {
        let Some(panel) = self.extensions.panel(name) else {
            ui.label(egui::RichText::new(format!("No extension provides {name}")).color(crate::theme::weak()));
            return;
        };
        let view = self.panels.entry(pane).or_insert_with(|| (panel.make)());
        let lens = Lens {
            index: &self.index,
            extensions: &self.extensions,
            selection: self.selected.as_deref(),
            looks: Some(&self.variant_choice),
        };
        // A panel's canvas is not a picture's: no texel grid over it.
        let preview = Preview { grid: false, ..preview };
        let frame = crate::ext_ui::Frame { asset: None, look: 0, preview, count: 1, editable: false };
        let mut sets = vec![];
        let mut host = crate::ext_ui::Host::new(ui, &lens, &mut self.textures, nav, &mut sets, frame);
        view.show(&mut pe2_ext::Ui::new(&mut host));
    }
}
