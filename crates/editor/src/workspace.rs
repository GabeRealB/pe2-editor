//! An open project as the editor holds it: its index, kept up to date by a watcher on its layers, the tree of its
//! entries, the selection with its history, the pictures decoded so far, the user's view of the layers (eyes, hand
//! locks, the active layer), and the link agents reach it by (`mcp::link`).

use anyhow::{Context, Result};
use eframe::egui;
use pe2_ext::events::{Event, FilesChanged, ProjectOpened};
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
    pub(crate) fn build(index: &Index, extensions: &Extensions) -> Node {
        let mut root = Node::default();
        for (real, entry) in index.entries.iter() {
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
    /// The node at a path below it.
    #[must_use]
    pub fn at(&self, path: &str) -> Option<&Node> {
        path.split('/').filter(|step| !step.is_empty()).try_fold(self, |node, step| node.children.get(step))
    }

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
    /// The project as the last take-in left it: a snapshot of the worker's index, swapped in when another arrives.
    pub index: Arc<Index>,
    /// The stack as the layer panel shows it, bottom to top.
    pub layers: Vec<LayerInfo>,
    /// What was done this session, to undo and redo.
    pub history: History,
    /// The tree's thumbnails, kept in the project's store.
    pub thumbs: crate::thumbs::Thumbs,
    /// The extensions, and what they register: the kinds' icons, fields, pictures, thumbnails and views.
    pub extensions: Arc<Extensions>,
    /// What plays in the preview: a movie, frame by frame.
    /// Asked of the graph in front.
    pub graph_ask: Option<GraphAsk>,
    /// The folder the assets' browser shows, and those it came from and went back from.
    pub asset_folder: String,
    folders_back: Vec<String>,
    folders_forward: Vec<String>,
    /// A folder chosen in the browser's grid: a click marks it, a double click opens it.
    pub marked_folder: Option<String>,
    /// Whether the pointer was over the browser: where back and forward are its own.
    pub in_browser: bool,
    /// Whether the folder shown changed: the tree beside the grid goes to it.
    pub folder_moved: bool,
    /// A player for each document tab playing, and the tabs that drew theirs this frame.
    players: HashMap<crate::panes::Pane, crate::player::Player>,
    played: HashSet<crate::panes::Pane>,
    /// The views the panes show, by (pane, asset, view): made when first shown, made again when the asset changes.
    views: HashMap<(egui::Id, String, usize), Box<dyn pe2_ext::View>>,
    /// What is inside assets, as their kinds said: asked again when the asset changes, not each frame.
    contents: HashMap<String, Arc<pe2_ext::Contents>>,
    /// Parts' sheets, as their kinds said: as `contents`.
    part_sheets: HashMap<(String, String), Arc<Option<pe2_ext::PartSheet>>>,
    /// Rows of the contents folded or unfolded by hand: by asset and row.
    pub content_open: HashMap<(String, String), bool>,
    /// The row of the contents being renamed: the asset, the row, what is typed.
    pub content_rename: Option<(String, String, String)>,
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
    /// The take-in worker: it owns the index and the imports, and hands snapshots back.
    takein: crate::takein::TakeIn,
    /// Runs what events set off.
    runner: crate::runner::Runner,
    /// The config in use.
    pub config: Option<project::Config>,
    /// How the last epoch ended.
    pub last_epoch: Option<crate::runner::Finished>,
    /// The name of an event being defined.
    pub new_event_name: String,
    /// The assets' settings as last applied (`project::applied`): what their outputs are made from.
    applied: Arc<project::applied::Applied>,
    /// Changes other programs made, waiting to be taken in: when the editor regains focus, as they happen (the
    /// setting), before an agent is answered, or on View > Refresh.
    pending: BTreeSet<PathBuf>,
    /// Whether the window had focus at the last frame.
    focused: bool,
    /// What the editor wrote lately, each path with its stamp then: the watcher's report of it is the editor's own
    /// change coming back, not another program's.
    own: Vec<(PathBuf, Option<Stamp>, std::time::Instant)>,
    /// The last job of other programs' changes, and the last whose snapshot the app was told of.
    external: (u64, u64),
    /// What was selected when an undone or redone step was made, shown once its job is taken in.
    selection_after: Option<(u64, String)>,
    /// Agents' requests waiting for the take-in of what was pending when they came, by its ticket.
    agents_waiting: Vec<(u64, mcp::Request, mpsc::Sender<mcp::Reply>)>,
    /// What the snapshots taken in since the app last asked did.
    outcome: Update,
    /// Why agents cannot reach the editor.
    pub link_error: Option<String>,
    /// The components found beside the built-ins when the project was opened: another finding reopens it.
    extensions_found: Vec<extensions::Found>,
    /// When the user's extensions folder was last looked at, which no watcher follows.
    extensions_checked: std::time::Instant,
    pub tree: Arc<Node>,
    /// The tree's paths (an asset's without `.asset`) that can be modified, and the folders holding one: the rest is
    /// drawn dimmed.
    editable: HashSet<String>,
    pub selected: Option<String>,
    /// What is selected inside an asset: its path, and a path in its declaration (`stack.1`, `stack.0.ops.draw`). The one
    /// address every pane showing the asset shares.
    pub inner: Option<(String, String)>,
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
    inspector_views: HashMap<(String, usize), Box<dyn pe2_ext::View>>,
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
    /// Edit the asset's source in the tool its format is edited in: copied into the active layer first where it is
    /// below it, and imported again when the tool saves it.
    EditIn(String),
    /// Show the graph pane, for the graph layer chosen.
    ShowGraph,
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

/// What commands ask of the graph in front.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphAsk {
    Fit,
    Arrange,
}

/// The edit that puts a file in an asset's bundle.
///
/// # Errors
/// It cannot be read.
pub fn adding(asset: &Asset, file: &Path) -> std::result::Result<EditRequest, String> {
    let name = file.file_name().and_then(|n| n.to_str()).ok_or("It has no name")?;
    let bytes = std::fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let short = asset.path.rsplit('/').next().unwrap_or(&asset.path);
    Ok(EditRequest::Write {
        uid: asset.uid.clone().ok_or("It has no uid")?,
        file: name.to_string(),
        bytes,
        label: format!("Add {name} to {short}"),
        merge: None,
    })
}

/// A path's size and time of change.
type Stamp = (u64, std::time::SystemTime);

/// A path's stamp, where it is.
fn stamp(path: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

/// What a take-in did to the workspace, as the app is told of it.
#[derive(Default)]
pub enum Update {
    #[default]
    Nothing,
    /// This many assets were added, changed or removed.
    Refreshed(usize),
    /// The project is opened again, as this changed: `project.toml` (its layers, the extensions it leaves off) or
    /// the extensions found (one in the project or the user's folder came, went or changed).
    Reopen(&'static str),
    /// What someone asked for came to this.
    Said(String),
}

/// A part's name, what its fragment says after its group: `cursor` of `cells/cursor`.
#[must_use]
pub fn part_name(id: &str) -> &str {
    id.split_once('/').map_or(id, |(_, name)| name)
}

/// Parts of assets' pictures, uploaded once each: a palette's strip of colors (`palettes/<name>`), as its kind draws it.
#[derive(Default)]
pub struct Parts {
    shown: HashMap<(String, String), Picture>,
    /// Those its kind draws nothing of: asked once, like the others.
    none: HashSet<(String, String)>,
}

impl Parts {
    /// Whether an asset's part has been asked for: getting it costs nothing.
    pub fn has(&self, asset: &Asset, part: &str) -> bool {
        let key = (asset.path.clone(), part.to_string());
        self.shown.contains_key(&key) || self.none.contains(&key)
    }

    /// An asset's part, uploaded; none where its kind draws no such part.
    pub fn get(&mut self, ctx: &egui::Context, extensions: &Extensions, asset: &Asset, part: &str) -> Option<&Picture> {
        let key = (asset.path.clone(), part.to_string());
        if self.none.contains(&key) {
            return None;
        }
        if !self.shown.contains_key(&key) {
            let Some(drawn) = extensions.part(asset, part) else {
                self.none.insert(key);
                return None;
            };
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
        self.none.retain(|(asset, _)| !assets.contains(asset));
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

/// A project opened as far as a workspace starts from it: what `Workspace::prepare` makes off the window's thread.
pub struct Opened {
    project: Project,
    lock: Lock,
    cache_dir: PathBuf,
    history: History,
    /// The label of an edit a crash left half done, rolled back.
    pub recovered: Option<String>,
    index: Index,
    store: project::graph::store::Store,
    applied: Arc<project::applied::Applied>,
    extensions: Arc<Extensions>,
    extensions_found: Vec<extensions::Found>,
    imports: project::imports::Imports,
    layer_view: LayerView,
    tree: Arc<Node>,
}

impl Workspace {
    /// Open the project in `dir` as far as a workspace starts from it, off the window's thread, saying how far it is;
    /// with a history, again, keeping this session's history: a change to `project.toml` - an edit of the stack, or
    /// its undoing - opens the project again, and must not end the session's history.
    ///
    /// # Errors
    /// Its `project.toml` or its assets cannot be read, or another holds it.
    pub fn prepare(dir: &Path, history: Option<History>, report: &project::index::Report<'_>) -> Result<Opened> {
        report("Reading project.toml", 0, 0);
        let project = Project::load(dir)?;
        // An agent's request holds the project for a moment (`pe2-cli mcp`): outlast one, not a real holder.
        let lock = Lock::acquire_within(&project.root, "pe2-editor", Duration::from_secs(2))?;
        let cache = Cache::open(&project, &lock)?;
        // The history first: a transaction a crash left half done is rolled back before the index reads the files.
        let (history, recovered) = match history {
            Some(history) => (history, None),
            None => History::open(&cache)?,
        };
        let mut index = Index::open_reporting(&project, &cache, report)?;
        // The work graph's store (what was kept as artifacts before it, gone); attaching the imports sweeps it.
        let store = project::graph::store::Store::open(&cache)?;
        let (kept, sweeping) = (store.clone(), store.clone());
        let old = cache.path("artifacts");
        if old.exists() {
            std::fs::remove_dir_all(&old).with_context(|| old.display().to_string())?;
        }
        // The extensions as the project has them: without those it leaves off (`[extensions] disabled`).
        // Its own (`extension` assets) and the user's before the built-ins.
        report("Loading the extensions", 0, 0);
        let extensions_found = extensions::found(Some(&index));
        let extensions = extensions::for_project(&project, Some(&index));
        // What they are asked in passing reads and keeps what the project's store holds.
        extensions.keep_in(kept);
        let layer_view: LayerView = std::fs::read_to_string(cache.path(LAYER_VIEW))
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default();
        // The view first: hiding layers assembles the assets again, so the imports are attached to what it shows.
        index.set_hidden(&layer_view.hidden(&index.layers()));
        // Their settings as last applied, which the work graph makes their outputs from.
        let applied = Arc::new(project::applied::Applied::open(&cache)?);
        applied.mark(&mut index, None);
        // What the assets' importers made of their sources, seen as their files.
        let imports = project::imports::Imports::open(&cache)?;
        let _ = imports.attach_reporting(&mut index, &extensions, report);
        // What the store keeps that nothing makes any more goes, aside: it reads all the store keeps.
        let (sweeping, nodes) = (sweeping, project::imports::live_nodes(&index));
        let _ = std::thread::Builder::new().name("store sweep".into()).spawn(move || {
            let _ = sweeping.sweep(&nodes, &std::collections::HashSet::new());
        });
        report("Building the tree", 0, 0);
        let tree = Arc::new(Node::build(&index, &extensions));
        Ok(Opened {
            project,
            lock,
            cache_dir: cache.dir,
            history,
            recovered,
            index,
            store,
            applied,
            extensions,
            extensions_found,
            imports,
            layer_view,
            tree,
        })
    }

    /// The workspace of a project opened (`prepare`): its worker started, its index shown.
    pub fn from_opened(opened: Opened) -> Workspace {
        let Opened {
            project,
            lock,
            cache_dir,
            history,
            index,
            store,
            applied,
            extensions,
            extensions_found,
            imports,
            layer_view,
            tree,
            ..
        } = opened;
        let thumbs = crate::thumbs::Thumbs::start(extensions.clone(), store);
        let layers = index.layers();
        // The worker takes the index from here; the UI shows a snapshot of it.
        let shown = Arc::new(index.clone());
        let pictures = crate::pictures::Pictures::start(shown.clone(), extensions.clone());
        let takein = crate::takein::TakeIn::start(
            project.clone(),
            index,
            applied.clone(),
            imports,
            extensions.clone(),
            extensions_found.clone(),
            tree.clone(),
        );
        let runner = crate::runner::Runner::start(project.clone(), extensions.clone());
        let mut workspace = Workspace {
            runner,
            config: None,
            last_epoch: None,
            new_event_name: String::new(),
            project,
            applied,
            history,
            thumbs,
            graph_ask: None,
            asset_folder: String::new(),
            folders_back: Vec::new(),
            folders_forward: Vec::new(),
            marked_folder: None,
            in_browser: false,
            folder_moved: false,
            players: HashMap::new(),
            played: HashSet::new(),
            extensions,
            views: HashMap::new(),
            contents: HashMap::new(),
            part_sheets: HashMap::new(),
            content_open: HashMap::new(),
            content_rename: None,
            view_choice: HashMap::new(),
            textures: crate::ext_ui::Textures::with_pictures(pictures),
            layers,
            layer_view,
            index: shown,
            lock,
            cache_dir,
            watch: None,
            watch_error: None,
            link: None,
            requests: None,
            window_requests: Vec::new(),
            link_error: None,
            extensions_found,
            extensions_checked: std::time::Instant::now(),
            takein,
            pending: BTreeSet::new(),
            focused: true,
            own: Vec::new(),
            external: (0, 0),
            selection_after: None,
            agents_waiting: Vec::new(),
            outcome: Update::Nothing,
            tree,
            editable: HashSet::new(),
            selected: None,
            inner: None,
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
        workspace
    }

    /// Start watching the layers, once: every change on disk is then brought in by `update`.
    pub fn watch(&mut self, ctx: &egui::Context) {
        if self.watch.is_some() || self.watch_error.is_some() {
            return;
        }
        self.takein.wake(ctx);
        self.runner.wake(ctx);
        if let Some(pictures) = &self.textures.pictures {
            pictures.wake(ctx);
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
        self.signal(ProjectOpened::NAME);
    }

    /// Signal an event: an epoch begins, on the project as last taken in, for the config in use.
    pub fn signal(&self, event: &str) {
        self.signal_all(vec![event.to_string()], None, None);
    }

    /// Define an event of the project: an asset of the active layer.
    ///
    /// # Errors
    /// No layer takes it, or it cannot be written.
    pub fn new_event(&mut self, name: &str) -> Result<(), String> {
        let layer = self.active_layer().ok_or("no layer is active")?.name.clone();
        let id = project::uid::new(|id| self.index.by_uid.contains_key(&project::uid::text(id))).map_err(|e| e.to_string())?;
        let uid = project::uid::text(id);
        let slug: String = name.chars().map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect();
        let quoted = toml::Value::String(name.to_string());
        let declaration = format!("uid = \"{uid}\"\nkind = \"event\"\nlabel = {quoted}\n");
        project::edit::create(&self.project, &self.index, &layer, &uid, &format!("events/{slug}"), &declaration, &mut self.history).map_err(|e| format!("{e:#}"))?;
        Ok(())
    }

    /// Leave an asset out of the config in use, or take it back. It is the project's, so written in its `project.toml`, as a step.
    ///
    /// # Errors
    /// The project has no config, or it cannot be written.
    pub fn set_disabled(&mut self, path: &str, disabled: bool) -> Result<(), String> {
        let config = self.config.as_ref().ok_or("the project has no config")?.name.clone();
        let file = self.project.root.join(project::FILE);
        let text = std::fs::read_to_string(&file).map_err(|e| e.to_string())?;
        let mut document: toml_edit::DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| e.to_string())?;
        let section = document["config"][config.as_str()].as_table_mut().ok_or("no such config in project.toml")?;
        let list = section.entry("disabled").or_insert(toml_edit::value(toml_edit::Array::new()));
        let list = list.as_array_mut().ok_or("its `disabled` is no list")?;
        list.retain(|item| item.as_str() != Some(path));
        if disabled {
            list.push(path);
        }
        let label = format!("{} {path} in {config}", if disabled { "Disable" } else { "Enable" });
        let mut step = self.history.begin(label);
        step.write(&file, document.to_string().as_bytes()).map_err(|e| format!("{e:#}"))?;
        step.commit().map_err(|e| format!("{e:#}"))?;
        Ok(())
    }

    /// Stop the epoch running.
    pub fn cancel_epoch(&self) {
        self.runner.cancel();
    }

    /// An epoch is running or waiting.
    pub fn in_epoch(&self) -> bool {
        self.runner.busy()
    }

    fn signal_all(&self, roots: Vec<String>, config: Option<project::Config>, reply: Option<mpsc::Sender<mcp::Reply>>) {
        let Some(config) = config.or_else(|| self.config.clone()) else {
            if let Some(reply) = reply {
                let _ = reply.send(mcp::Reply::failed("the project has no config".to_string()));
            }
            return;
        };
        self.runner.run(crate::runner::Job { roots, index: self.index.clone(), config, reply });
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

    /// Answer the agents' requests that arrived since the last frame, against the index and the selection as shown. A
    /// request first has what other programs changed taken in - an agent reads what it just wrote - and waits for it.
    pub fn answer_agents(&mut self) {
        let Some(requests) = &self.requests else { return };
        let arrived: Vec<_> = requests.try_iter().collect();
        for (request, reply) in arrived {
            if let mcp::Request::Signal { events, config } = &request {
                let config = config.as_ref().and_then(|name| self.project.configs.iter().find(|c| c.name == *name)).cloned();
                self.signal_all(events.clone(), config, Some(reply));
                continue;
            }
            if request.uses_window() {
                self.window_requests.push((request, reply));
                continue;
            }
            self.take_in_pending();
            if self.takein.busy() {
                self.agents_waiting.push((self.takein.issued(), request, reply));
            } else {
                self.answer_agent(&request, &reply);
            }
        }
    }

    fn answer_agent(&mut self, request: &mcp::Request, reply: &mpsc::Sender<mcp::Reply>) {
        let selection = self.selected.clone();
        let variant = selection.as_deref().and_then(|s| self.variants_of(s).get(self.variant(s)).cloned());
        let editor = mcp::EditorState { selection, variant, show: None };
        let mut view = mcp::View { project: &self.project, index: &self.index, extensions: &self.extensions, editor: Some(editor) };
        let answer = mcp::answer(&mut view, request);
        let show = view.editor.and_then(|e| e.show);
        if let Some(path) = show {
            self.navigate(path);
        }
        let _ = reply.send(answer);
    }

    /// Bring in what changed: other programs' changes collected from the watcher, taken in as they happen (`live`) or
    /// when the window regains focus; and the snapshots the worker made since, swapped in. Returns what they did.
    pub fn update(&mut self, focused: bool, live: bool) -> Update {
        // The user's extensions folder, looked at each second.
        if self.extensions_checked.elapsed() >= Duration::from_secs(1) {
            self.extensions_checked = std::time::Instant::now();
            if extensions::found(Some(&self.index)) != self.extensions_found {
                return Update::Reopen("an extension changed");
            }
        }
        // What the editor wrote (an edit, a step, a setting of the project): taken in at once.
        self.take_written();
        self.own.retain(|(_, _, at)| at.elapsed() < Duration::from_secs(30));
        if let Some(watch) = &self.watch {
            for change in watch.poll() {
                match change {
                    Change::Paths(changed) => {
                        let theirs: Vec<PathBuf> = changed.into_iter().filter(|p| !self.is_own(p)).collect();
                        self.pending.extend(theirs);
                    }
                    Change::Error(e) => self.watch_error = Some(format!("changes may be missed: {e}")),
                }
            }
        }
        let regained = focused && !self.focused;
        self.focused = focused;
        if live || regained {
            self.take_in_pending();
        }
        let ready = self.takein.poll();
        self.apply(ready);
        for ended in self.runner.poll() {
            if matches!(self.outcome, Update::Nothing)
                && let Some(said) = ended.said()
            {
                self.outcome = Update::Said(said);
            }
            self.last_epoch = Some(ended);
        }
        std::mem::take(&mut self.outcome)
    }

    /// Take in what the history wrote since this was last asked - the editor's own changes - remembering each path's
    /// stamp to know the watcher's report of it again; returns the job's ticket.
    fn take_written(&mut self) -> Option<u64> {
        let written = self.history.take_written();
        if written.is_empty() {
            return None;
        }
        let now = std::time::Instant::now();
        self.own.extend(written.iter().map(|p| (p.clone(), stamp(p), now)));
        Some(self.takein.paths(written))
    }

    /// Whether the watcher's report of a path is of what the editor wrote: as it left it.
    fn is_own(&self, path: &Path) -> bool {
        self.own.iter().rev().find(|(p, ..)| p == path).is_some_and(|(_, then, _)| *then == stamp(path))
    }

    /// Take in what other programs changed, now.
    fn take_in_pending(&mut self) {
        if !self.pending.is_empty() {
            let paths = std::mem::take(&mut self.pending).into_iter().collect();
            self.external.0 = self.takein.paths(paths);
        }
    }

    /// View > Refresh: what other programs changed taken in; where the watcher failed, every layer scanned again.
    pub fn refresh_now(&mut self) {
        if self.watch_error.is_some() {
            self.pending.extend(self.project.layers.iter().map(|l| l.path.clone()));
        }
        self.take_in_pending();
    }

    /// Changes other programs made, not taken in yet.
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    /// A take-in is running.
    pub fn taking_in(&self) -> bool {
        self.takein.busy()
    }

    /// Wait for the take-ins asked so far, and swap in what they made: what an edit does before reading the index.
    fn settle(&mut self) {
        let ready = self.takein.settle();
        self.apply(ready);
    }

    /// Swap in snapshots, oldest first: the index, the tree and the layers; the pictures and views of what changed
    /// forgotten; a selection shown once its step is taken in; agents waiting on them answered.
    fn apply(&mut self, snapshots: Vec<crate::takein::Snapshot>) {
        for snapshot in snapshots {
            if let Some(error) = snapshot.error {
                self.watch_error = Some(error);
            }
            if let Some(why) = snapshot.reopen {
                self.outcome = Update::Reopen(why);
            }
            self.index = snapshot.index;
            if snapshot.files {
                self.signal(FilesChanged::NAME);
            }
            if let Some(pictures) = &mut self.textures.pictures {
                if snapshot.all {
                    pictures.clear();
                }
                pictures.set_index(self.index.clone());
            }
            self.tree = snapshot.tree;
            self.layers = snapshot.layers;
            self.refresh_editable();
            if snapshot.all {
                self.pictures.clear();
                self.parts = Parts::default();
                self.inspector_views.clear();
                self.panels.clear();
                self.views.clear();
                self.contents.clear();
                self.part_sheets.clear();
                self.thumbs.clear();
            } else if !snapshot.changed.is_empty() {
                let changed = &snapshot.changed;
                self.pictures.retain(|(image, ..), _| !changed.contains(image));
                self.parts.forget(changed);
                self.inspector_views.retain(|(asset, ..), _| !changed.contains(asset));
                self.panels.clear();
                self.views.retain(|(_, asset, _), _| !changed.contains(asset));
                self.contents.retain(|asset, _| !changed.contains(asset));
                self.part_sheets.retain(|(asset, _), _| !changed.contains(asset));
                self.thumbs.forget(changed);
            }
            self.drop_lost_selection();
            // Other programs' changes taken in: the app says how many assets they changed.
            let (asked, told) = self.external;
            if asked > told && snapshot.ticket >= asked {
                self.external.1 = asked;
                if !matches!(self.outcome, Update::Reopen(_)) && !snapshot.changed.is_empty() {
                    let before = if let Update::Refreshed(n) = self.outcome { n } else { 0 };
                    self.outcome = Update::Refreshed(before + snapshot.changed.len());
                }
            }
        }
        if let Some((ticket, path)) = self.selection_after.take() {
            if !self.takein.covers(ticket) {
                self.selection_after = Some((ticket, path));
            } else if self.exists(&path) {
                self.navigate(path);
            }
        }
        let waiting = std::mem::take(&mut self.agents_waiting);
        for (ticket, request, reply) in waiting {
            if self.takein.covers(ticket) {
                self.answer_agent(&request, &reply);
            } else {
                self.agents_waiting.push((ticket, request, reply));
            }
        }
    }

    /// Give up the workspace, keeping its history for the project opened again.
    pub fn into_history(self) -> History {
        self.history
    }

    /// Undo the last step, or redo the one undone last; forced, over files changed since outside the editor. Its
    /// files are taken in, and what was selected when the step was made is shown again once they are. Returns the
    /// step's label.
    ///
    /// # Errors
    /// Nothing to undo or redo, files changed outside the editor (unless forced), or a file not written.
    pub fn step(&mut self, redo: bool, force: bool) -> Result<String, StepError> {
        self.settle();
        let step = if redo { self.history.redo(force)? } else { self.history.undo(force)? };
        let (label, selection) = (step.label.clone(), step.selection.clone());
        let ticket = self.take_written();
        self.selection_after = selection.zip(ticket).map(|(path, ticket)| (ticket, path));
        Ok(label)
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
        if !self.inner_is_there() {
            self.inner = None;
        }
    }

    /// Whether what is selected inside an asset is still in it: a part or a row by its id, a layer or operation by its place.
    fn inner_is_there(&mut self) -> bool {
        let Some((path, inner)) = self.inner.clone() else { return true };
        let Some(asset) = self.index.assets.get(&path).cloned() else { return false };
        if let Some(part) = inner.strip_prefix('#') {
            let row = self.contents_of(&asset).rows.iter().any(|row| row.id == part && !row.is_heading());
            // A view may hold what it selected; only it could say whether that is still there.
            return row || self.extensions.parts(&asset).iter().any(|named| named.id == part) || !self.extensions.contents_views(&asset).is_empty();
        }
        crate::inspector::layer_of_inner(&inner).is_none_or(|(at, op)| {
            let layer = asset.fields().get("stack").and_then(toml::Value::as_array).and_then(|layers| layers.get(at));
            layer.is_some_and(|layer| op.is_none_or(|op| layer.get("ops").and_then(|ops| ops.get(op)).is_some()))
        })
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
        // The worker assembles the view again; its snapshot brings the tree and forgets the pictures.
        self.takein.hidden(self.layer_view.hidden(&self.layers));
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
        for (real, entry) in self.index.entries.iter() {
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

    /// Whether an asset has settings not applied yet: its outputs are made from its settings as last applied.
    pub fn unapplied(&self, path: &str) -> bool {
        let uid = self.index.assets.get(path).and_then(|a| a.uid.as_deref());
        uid.is_some_and(|uid| self.applied.text(uid).is_some())
    }

    /// How many assets have settings not applied yet.
    pub fn unapplied_count(&self) -> usize {
        self.applied.pending().len()
    }

    /// Apply an asset's settings: its outputs made again from them.
    pub fn apply_settings(&mut self, path: &str) {
        let Some(uid) = self.index.assets.get(path).and_then(|a| a.uid.clone()) else { return };
        if self.applied.apply(&uid) {
            self.takein.applied(std::iter::once(path.to_string()).collect());
        }
    }

    /// Apply every asset's settings.
    pub fn apply_all_settings(&mut self) {
        let uids: std::collections::BTreeSet<String> = self.applied.apply_all().into_iter().collect();
        let paths = self.index.assets.values().filter(|a| a.uid.as_ref().is_some_and(|u| uids.contains(u)));
        let paths: std::collections::BTreeSet<String> = paths.map(|a| a.path.clone()).collect();
        if !paths.is_empty() {
            self.takein.applied(paths);
        }
    }

    /// The edit putting an asset's declaration back as last applied: its unapplied changes reverted, a step of the
    /// history.
    pub fn revert_settings(&self, path: &str) -> Option<EditRequest> {
        let asset = self.index.assets.get(path)?;
        let uid = asset.uid.clone()?;
        let text = self.applied.text(&uid)?;
        let name = path.rsplit('/').next().unwrap_or(path);
        let label = format!("Revert the settings of {name}");
        Some(EditRequest::Write { uid, file: project::index::ASSET_FILE.to_string(), bytes: text.into_bytes(), label, merge: None })
    }

    /// Make an edit in the active layer: a step of the history, the index taking it in at once. Returns what was done.
    ///
    /// # Errors
    /// The active layer cannot be written, the edit does not apply, or files cannot be written.
    pub fn edit(&mut self, request: &EditRequest) -> anyhow::Result<String> {
        use project::edit;
        // An edit reads the index (a layer's changeset): what was asked before is taken in first.
        self.settle();
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
            | EditRequest::EditIn(uid)
            | EditRequest::Write { uid, .. } => uid.clone(),
            EditRequest::ShowGraph => return Ok(String::new()),
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
        match request {
            EditRequest::Override(uid) | EditRequest::OverrideIn(uid, _) => {
                edit::override_entry(project, index, &name, uid, history)?
            }
            EditRequest::Delete(uid) => edit::delete(project, index, &name, uid, history)?,
            EditRequest::RenameTo(uid, to) => edit::rename(project, index, &name, uid, to, history)?,
            EditRequest::Resolve(uid, resolution) => edit::resolve(project, index, &name, uid, *resolution, history)?,
            EditRequest::Write { uid, file, bytes, label, merge } => {
                edit::write_file(project, index, &name, uid, file, bytes, label.clone(), merge.clone(), history)?
            }
            EditRequest::Rename(..) | EditRequest::EditIn(_) | EditRequest::ShowGraph => return Ok(String::new()),
        };
        self.history.note_selection(self.selected.clone());
        let label = self.history.done().last().map_or_default(|s| s.label.clone());
        // What it wrote, taken in ahead of what other programs changed.
        self.take_written();
        Ok(label)
    }

    /// The tool an asset's source is edited in - by the media type of the format it is recognized as - and the source
    /// file; none where it has no source, or no tool of the settings takes its format.
    pub fn source_tool(&self, path: &str) -> Option<(crate::external::Category, &project::index::BundleFile)> {
        let asset = self.index.assets.get(path)?;
        let name = project::graph::import_of(asset.fields())?.source;
        let file = asset.files.iter().find(|f| f.name == name && f.made_by.is_none())?;
        let category = self.extensions.recognize(file).into_iter().find_map(|(format, _)| {
            crate::external::Category::of_media_type(format.media_type.as_deref()?)
        })?;
        Some((category, file))
    }

    /// Edit an asset's source in the tool its format is edited in: where the asset is below the active layer, it is
    /// overridden there first (a step of the history), so the tool writes the layer's own copy; the watcher brings
    /// what the tool saves in, and the import runs again. Returns what was done.
    ///
    /// # Errors
    /// No such asset, no source or tool for it, the active layer cannot take it, or the tool cannot be started.
    pub fn edit_in(&mut self, uid: &str, tools: &crate::settings::Tools) -> anyhow::Result<String> {
        let path_of = |ws: &Workspace| ws.index.assets.values().find(|a| a.uid.as_deref() == Some(uid)).map(|a| a.path.clone());
        let path = path_of(self).with_context(|| format!("{uid}: no such asset"))?;
        let layer = self.editable_layer().map_err(anyhow::Error::msg)?.name.clone();
        let (_, source) = self.source_tool(&path).context("it has no source a tool of the settings edits")?;
        let mut done = vec![];
        if source.layer != layer {
            let label = self.edit(&EditRequest::Override(uid.to_string()))?;
            done.push(label);
            // The copy taken in, to open it.
            self.settle();
        }
        let path = path_of(self).unwrap_or(path);
        let (category, source) = self.source_tool(&path).context("its source is gone")?;
        anyhow::ensure!(source.layer == layer, "its source is in {}, not in {layer}", source.layer);
        let opened = crate::external::open_as(&source.path, &path, category, tools).map_err(anyhow::Error::msg)?;
        done.push(format!("{opened} in the {}: saved, it is imported again", category.label().to_lowercase()));
        Ok(done.join("; "))
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

    /// Run an extension's tool for a command, with no arguments; what it answers, briefly.
    ///
    /// # Errors
    /// It is gone, or it fails.
    pub fn run_tool(&self, name: &str) -> std::result::Result<String, String> {
        let tool = self.extensions.tools().iter().find(|t| t.name.to_string() == name).ok_or_else(|| format!("no tool {name}"))?;
        let lens = Lens { index: &self.index, extensions: &self.extensions, selection: self.selected.as_deref(), looks: None };
        let answer = (tool.run)(&lens, &toml::Value::Table(toml::Table::new())).map_err(|e| format!("{e:#}"))?;
        Ok(answer.to_string())
    }

    /// The edit removing what is selected inside an asset: an operation of a graph, or a layer.
    #[must_use]
    pub fn removing_selected(&self) -> Option<EditRequest> {
        use project::graph::layers;
        let asset = self.index.assets.get(self.selected.as_deref()?)?;
        let (at, op) = crate::inspector::layer_of_inner(self.inner_of(&asset.path)?)?;
        let (label, text) = match op {
            Some(id) => (format!("Remove {id}"), layers::remove_operation(asset.text(), at, id)),
            None => (format!("Remove layer {at}"), layers::remove(asset.text(), at)),
        };
        crate::inspector::layers_edit(asset, label, text)
    }

    /// Show a folder in the browser, the one left to go back to.
    pub fn open_folder(&mut self, folder: String) {
        if folder != self.asset_folder {
            self.folders_back.push(std::mem::replace(&mut self.asset_folder, folder));
            self.folders_forward.clear();
            self.folder_moved = true;
        }
        self.marked_folder = None;
    }

    pub fn folder_back(&mut self) {
        if let Some(folder) = self.folders_back.pop() {
            self.folders_forward.push(std::mem::replace(&mut self.asset_folder, folder));
            self.marked_folder = None;
            self.folder_moved = true;
        }
    }

    pub fn folder_forward(&mut self) {
        if let Some(folder) = self.folders_forward.pop() {
            self.folders_back.push(std::mem::replace(&mut self.asset_folder, folder));
            self.marked_folder = None;
            self.folder_moved = true;
        }
    }

    /// The folder holding the one shown.
    pub fn folder_up(&mut self) {
        if !self.asset_folder.is_empty() {
            let parent = self.asset_folder.rsplit_once('/').map_or_else(String::new, |(parent, _)| parent.to_string());
            self.open_folder(parent);
        }
    }

    #[must_use]
    pub fn can_folder_back(&self) -> bool {
        !self.folders_back.is_empty()
    }

    #[must_use]
    pub fn can_folder_forward(&self) -> bool {
        !self.folders_forward.is_empty()
    }

    /// A document tab's asset, played.
    pub fn play(&mut self, ui: &mut egui::Ui, pane: crate::panes::Pane, path: &str, view: &mut crate::settings::Preview) {
        self.played.insert(pane);
        let (extensions, index) = (self.extensions.clone(), self.index.clone());
        let player = self.players.entry(pane).or_insert_with(|| crate::player::Player::new(extensions.clone()));
        player.ui(ui, (&extensions, &index), path, view);
    }

    /// Once every window is drawn: a tab gone takes its player, and one not shown pauses.
    pub fn settle_players(&mut self, open: impl Fn(crate::panes::Pane) -> bool) {
        let played = std::mem::take(&mut self.played);
        self.players.retain(|pane, _| open(*pane));
        for (_, player) in self.players.iter_mut().filter(|(pane, _)| !played.contains(*pane)) {
            player.pause();
        }
    }

    /// The edits that make a file the source of an asset, by its kind's recipe: the file in its bundle, and its stack.
    ///
    /// # Errors
    /// Nothing makes this file of this asset, or it cannot be read.
    pub fn replacing(&self, asset: &Asset, file: &Path) -> std::result::Result<Vec<EditRequest>, String> {
        let name = file.file_name().and_then(|n| n.to_str()).ok_or("It has no name")?;
        let stack = self.extensions.recipe_for(asset, name).ok_or("Nothing imports it over this")??;
        let uid = asset.uid.clone().ok_or("It has no uid")?;
        let mut document = asset.text().parse::<toml_edit::DocumentMut>().map_err(|e| e.to_string())?;
        document.remove("stack");
        // As text, after the rest: a table inserted keeps its place in the document it came from.
        let declaration = format!("{}\n\n{stack}", document.to_string().trim_end());
        let bytes = std::fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
        let short = asset.path.rsplit('/').next().unwrap_or(&asset.path);
        let (label, merge) = (format!("Import {name} over {short}"), Some(format!("import {}", asset.path)));
        let write = |file: &str, bytes: Vec<u8>| EditRequest::Write {
            uid: uid.clone(),
            file: file.to_string(),
            bytes,
            label: label.clone(),
            merge: merge.clone(),
        };
        Ok(vec![write(name, bytes), write(project::index::ASSET_FILE, declaration.into_bytes())])
    }

    /// Whether an entry opens as a document: an asset, a container folder among them, or a plain file; a filesystem
    /// folder does not.
    #[must_use]
    pub fn is_document(&self, path: &str) -> bool {
        self.index.assets.contains_key(path) || self.entry_at(path).is_some_and(|(_, e)| e.kind == project::index::Kind::File)
    }

    /// What is inside an asset, as its kind says.
    pub fn contents_of(&mut self, asset: &Asset) -> Arc<pe2_ext::Contents> {
        let extensions = &self.extensions;
        self.contents.entry(asset.path.clone()).or_insert_with(|| Arc::new(extensions.contents(asset))).clone()
    }

    /// A part's sheet, where its asset's kind says what it holds.
    pub fn part_sheet_of(&mut self, asset: &Asset, part: &str) -> Arc<Option<pe2_ext::PartSheet>> {
        let extensions = &self.extensions;
        let key = (asset.path.clone(), part.to_string());
        self.part_sheets.entry(key).or_insert_with(|| Arc::new(extensions.part_sheet(asset, part))).clone()
    }

    /// Select the part of the asset selected a fragment or a name stands for - a box knows only its name - or a row of its
    /// contents, which need be no part.
    pub fn select_part(&mut self, name: &str) {
        let Some(asset) = self.selected.as_ref().and_then(|path| self.index.assets.get(path)).cloned() else { return };
        // A fragment is the view's own to name: what it stands for may be neither a part nor a row.
        if name.contains('/') {
            self.inner = Some((asset.path.clone(), format!("#{name}")));
            return;
        }
        let is_it = |id: &str| id == name || part_name(id) == name;
        let part = self.extensions.parts(&asset).into_iter().map(|part| part.id).find(|id| is_it(id));
        let contents = self.contents_of(&asset);
        let row = || contents.rows.iter().filter(|row| !row.is_heading()).map(|row| row.id.clone()).find(|id| is_it(id));
        if let Some(id) = part.or_else(row) {
            self.inner = Some((asset.path.clone(), format!("#{id}")));
        }
    }

    /// The part selected of an asset: its fragment.
    #[must_use]
    pub fn part_of(&self, asset: &str) -> Option<&str> {
        self.inner_of(asset)?.strip_prefix('#')
    }

    /// What is selected inside an asset, as a path in its declaration, or `#` and a part's fragment.
    #[must_use]
    pub fn inner_of(&self, asset: &str) -> Option<&str> {
        self.inner.as_ref().filter(|(of, _)| of == asset).map(|(_, inner)| inner.as_str())
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
    /// An asset's picture in its look, uploaded: drawn off the window's thread, none until it is.
    pub fn picture(&mut self, ctx: &egui::Context, path: &str, upscale: Upscale) -> Option<&Picture> {
        let chosen = self.variant(path);
        let key = (path.to_string(), chosen, upscale);
        if !self.pictures.contains_key(&key) {
            let drawn = self.textures.pictures.as_mut()?.get(path, chosen)?;
            self.pictures.insert(key.clone(), drawn.map(|image| show(ctx, path, chosen, upscale, &image)));
        }
        self.pictures.get(&key)
    }

    /// Whether the preview shows an asset: by its picture, or - its kind drawing none - by its kind's views.
    pub fn has_view(&self, path: &str) -> bool {
        self.index.assets.get(path).is_some_and(|a| {
            self.extensions.has_picture(a) || (!self.extensions.shows_pictures(a) && !self.extensions.views(a).is_empty())
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
        if self.index.assets.get(path).is_some_and(|a| self.extensions.has_picture(a)) {
            self.show_picture(ui, path, preview, count, nav);
            return;
        }
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
        let make = views[chosen].make.clone();
        self.draw_view(ui, (pane, path.to_string(), chosen), &*make, preview, count, nav);
    }

    /// Show an editor an extension gives an asset, made when first shown.
    pub fn show_editor(&mut self, ui: &mut egui::Ui, pane: egui::Id, path: &str, name: &str, preview: Preview, nav: &mut Option<String>) {
        let Some(asset) = self.index.assets.get(path) else { return };
        // After its views, in the numbers the pane keeps views by.
        let views = self.extensions.views(asset).len();
        let editors = self.extensions.editors(asset);
        let Some((at, def)) = editors.iter().enumerate().find(|(_, def)| def.name == name) else { return };
        let (at, make) = (views + at, def.make.clone());
        self.draw_view(ui, (pane, path.to_string(), at), &*make, preview, 1, nav);
    }

    /// Show a view of an asset in a pane, made when first shown.
    fn draw_view(
        &mut self,
        ui: &mut egui::Ui,
        key: (egui::Id, String, usize),
        make: &pe2_ext::MakeView,
        preview: Preview,
        count: usize,
        nav: &mut Option<String>,
    ) {
        let path = key.1.clone();
        let look = self.variant(&path);
        // As the inspector's own fields: an asset from below is overridden into the active layer.
        let editable = self.editable_layer().is_ok();
        let part = self.part_of(&path).map(str::to_string);
        let Some(asset) = self.index.assets.get(&path) else { return };
        let view = self.views.entry(key).or_insert_with(|| make(&self.extensions.lend(asset)));
        let lens = Lens {
            index: &self.index,
            extensions: &self.extensions,
            selection: self.selected.as_deref(),
            looks: Some(&self.variant_choice),
        };
        let frame = crate::ext_ui::Frame { asset: Some(asset), look, preview, count, editable, part: part.as_deref() };
        let mut sets = vec![];
        let mut host = crate::ext_ui::Host::new(ui, &lens, &mut self.textures, nav, &mut sets, frame);
        view.show(&mut pe2_ext::Ui::new(&mut host));
        drop(host);
        if !sets.is_empty() {
            self.view_edit = crate::inspector::set_fields(asset, &sets);
        }
    }

    /// Show an asset's picture in its look on the host's canvas, with what its kind marks on it over it.
    fn show_picture(&mut self, ui: &mut egui::Ui, path: &str, preview: Preview, count: usize, nav: &mut Option<String>) {
        let look = self.variant(path);
        let Some(pictures) = self.textures.pictures.as_mut() else { return };
        let image = match pictures.get(path, look) {
            Some(Ok(image)) => image,
            Some(Err(e)) => {
                ui.colored_label(ui.visuals().error_fg_color, format!("No preview: {e}"));
                return;
            }
            None => {
                ui.label(egui::RichText::new("Drawing the picture...").small().color(crate::theme::weak()));
                return;
            }
        };
        let marks = pictures.marks(path, look).unwrap_or_default();
        let texture = pe2_ext::Texture::shared(format!("{path}#{look}#picture"), image.clone());
        let id = self.textures.get(ui.ctx(), &texture, preview.upscale);
        #[expect(clippy::cast_precision_loss, reason = "picture sizes")]
        let size = egui::vec2(image.width as f32, image.height as f32);
        let selected = self.part_of(path).map(part_name).map(str::to_string);
        let overlay = crate::panes::Overlay {
            id: path.to_string(),
            size,
            pictures: vec![(id, egui::Rect::from_min_size(egui::Pos2::ZERO, size), None, None)],
            boxes: marks.boxes.iter().map(|m| (m.name.clone(), crate::ext_ui::to_rect(m.rect), selected.as_ref() == Some(&m.name))).collect(),
            summary: marks.summary.clone(),
            toggle: marks.toggle.clone(),
            legend: marks.legend,
            grid: None,
        };
        if let Some(link) = crate::panes::overlay_ui(ui, &overlay, preview, count) {
            *nav = Some(link);
        }
    }

    /// Show an asset's inspector views - what its kind and role draw of it after the property sheet - made when first
    /// shown: those of a tab, or those with none.
    pub fn show_inspector_views(&mut self, ui: &mut egui::Ui, path: &str, tab: Option<usize>, nav: &mut Option<String>) {
        self.show_lent_views(ui, path, nav, |extensions, asset| {
            let shown = extensions.inspector(asset).into_iter().enumerate();
            shown.filter(|(i, def)| tab.map_or(def.tab.is_none(), |tab| tab == *i)).collect()
        });
    }

    /// Show the views a kind and a role give the head of the contents pane, made when first shown.
    pub fn show_contents_views(&mut self, ui: &mut egui::Ui, path: &str, nav: &mut Option<String>) {
        self.show_lent_views(ui, path, nav, |extensions, asset| {
            // After the inspector's, in the numbers these are kept by.
            let inspector = extensions.inspector(asset).len();
            extensions.contents_views(asset).into_iter().enumerate().map(|(i, def)| (inspector + i, def)).collect()
        });
    }

    /// Show those of an asset's views `chosen` says, each by the number it is kept by.
    fn show_lent_views(
        &mut self,
        ui: &mut egui::Ui,
        path: &str,
        nav: &mut Option<String>,
        chosen: impl for<'a> Fn(&'a Extensions, &'a Asset) -> Vec<(usize, &'a pe2_ext::ViewDef)>,
    ) {
        let look = self.variant(path);
        // As the inspector's own fields: an asset from below is overridden into the active layer.
        let editable = self.editable_layer().is_ok();
        let part = self.part_of(path).map(str::to_string);
        let Some(asset) = self.index.assets.get(path) else { return };
        let lens = Lens {
            index: &self.index,
            extensions: &self.extensions,
            selection: self.selected.as_deref(),
            looks: Some(&self.variant_choice),
        };
        for (i, def) in chosen(&self.extensions, asset) {
            let key = (path.to_string(), i);
            let view = self.inspector_views.entry(key).or_insert_with(|| (def.make)(&self.extensions.lend(asset)));
            let frame = crate::ext_ui::Frame { asset: Some(asset), look, preview: Preview::default(), count: 1, editable, part: part.as_deref() };
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
        let frame = crate::ext_ui::Frame { asset: None, look: 0, preview, count: 1, editable: false, part: None };
        let mut sets = vec![];
        let mut host = crate::ext_ui::Host::new(ui, &lens, &mut self.textures, nav, &mut sets, frame);
        view.show(&mut pe2_ext::Ui::new(&mut host));
    }
}
