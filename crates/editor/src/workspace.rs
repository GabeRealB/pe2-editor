//! An open project as the editor holds it: its index, kept up to date by a watcher on its layers, the tree of its
//! entries, the selection with its history, the pictures decoded so far, the user's view of the layers (eyes, hand
//! locks, the active layer), and the link agents reach it by (`mcp::link`).

use anyhow::{Context, Result};
use eframe::egui;
use pe2_ext::events::{Build, Clean, Event, FilesChanged, ProjectOpened};
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
/// path, a bundle's with `.node`) and the asset it is, or that it is a plain file. An image with several variants -
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

/// How the asset browser shows the project.
#[derive(Clone, Copy, Debug, Default)]
pub struct Browsing {
    /// Each node's files inside it, beside what is inside it, as the project's filesystem shows them.
    pub show_files: bool,
}

/// A virtual entry: an image and one of its variants.
#[derive(Clone, PartialEq, Eq)]
pub struct Entry {
    pub image: String,
    pub look: String,
}

/// Where the look an asset is shown in is chosen.
fn look_of(path: &str) -> (String, String) {
    (path.to_string(), project::ext::PICTURE.to_string())
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
                        for name in names {
                            let entry = Some(Entry { image: path.to_string(), look: name.clone() });
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
    /// The Tests pane's: what is known of tests, and what is running.
    pub tests: crate::tests_pane::Tests,
    /// The tree's thumbnails, kept in the project's store.
    pub thumbs: crate::thumbs::Thumbs,
    /// The extensions, and what they register: the types' icons, fields, pictures, thumbnails and views.
    pub extensions: Arc<Extensions>,
    /// What the work graph keeps.
    store: project::graph::store::Store,
    /// The ticket of attaching the imports of the project as opened.
    attaching: u64,
    /// What views ask without waiting, and how many were answered when what was said meanwhile was last dropped.
    outputs: Arc<project::ext::requests::Requests>,
    requests_seen: u64,
    /// Events that came before what the importers made was attached: an epoch works from the project as last taken in,
    /// which they need whole. That it opened is the first.
    held_events: Vec<Signaled>,
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
    /// Where the asset of the view being drawn under a player plays, and what that view asked of the player.
    view_playback: Option<pe2_ext::Playing>,
    view_transport: Vec<pe2_ext::Transport>,
    played: HashSet<crate::panes::Pane>,
    /// The views the panes show, by (pane, asset, view): made when first shown, made again when the asset changes.
    views: HashMap<(egui::Id, String, usize), Box<dyn pe2_ext::View>>,
    /// What is inside assets, as their types said: asked again when the asset changes, not each frame.
    contents: HashMap<String, Arc<pe2_ext::Contents>>,
    /// Parts' sheets, as their types said: as `contents`.
    part_sheets: HashMap<(String, String), Arc<Option<pe2_ext::PartSheet>>>,
    /// Rows of the contents folded or unfolded by hand: by asset and row.
    pub content_open: HashMap<(String, String), bool>,
    /// The row of the contents being renamed: the asset, the row, what is typed.
    pub content_rename: Option<(String, String, String)>,
    /// The view an asset is shown in, where its type has several.
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
    /// The project served as a filesystem, where this machine says (`project.local.toml`): started with the watcher.
    pub files: Option<crate::files::Files>,
    /// The host standing by told that the editor is letting go of the project: it takes it the moment this goes.
    handing: Option<project::host::Lease>,
    /// The watcher being started, off the window's thread.
    watch_starting: Option<mpsc::Receiver<std::result::Result<Watch, String>>>,
    /// The generated layers as their plans say they are.
    pub plans: crate::plans::Plans,
    /// Filling them: the step of the project's onboarding that runs their plans.
    pub fill: crate::fill::Fill,
    /// The agents' link, started with the watcher; their requests, answered between frames.
    /// Agents' link, listening while it is kept: it stops with the workspace.
    _link: Option<mcp::link::Listener>,
    requests: Option<mpsc::Receiver<(mcp::Request, mpsc::Sender<mcp::Reply>)>>,
    /// Requests using the editor's window, which the app's driver answers across frames.
    pub window_requests: Vec<crate::drive::Request>,
    /// What the project's extensions fetch onto this machine, fetched beside the editor.
    pub fetcher: crate::fetcher::Fetcher,
    /// The take-in worker: it owns the index and the imports, and hands snapshots back.
    takein: crate::takein::TakeIn,
    /// Runs what events set off.
    runner: crate::runner::Runner,
    /// The config in use.
    pub config: Option<project::Config>,
    /// The config the extensions were told, and which extensions were: told again once they are read again.
    config_told: Option<(usize, Option<pe2_ext::data::BuildConfig>, Vec<String>)>,
    /// How the last epoch ended.
    pub last_epoch: Option<crate::runner::Finished>,
    /// The name of an event being defined.
    pub new_event_name: String,
    /// What the GPU said of what extensions gave it, first first.
    pub gpu_errors: Vec<String>,
    /// Changes other programs made, waiting to be taken in: when the editor regains focus, as they happen (the
    /// setting), before an agent is answered, or on View > Refresh.
    pending: BTreeSet<PathBuf>,
    /// How many times changes were taken in: what a view read of plain files may be out of date once it moves.
    revision: u64,
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
    /// The components found beside the built-ins when the project was read: another finding has it read again.
    extensions_found: Vec<extensions::Found>,
    /// When the project's extensions and where they are kept were last looked at, which no watcher follows.
    extensions_checked: std::time::Instant,
    pub tree: Arc<Node>,
    /// The tree's paths (an asset's without `.node`) that can be modified, and the folders holding one: the rest is
    /// drawn dimmed.
    editable: HashSet<String>,
    pub selected: Option<String>,
    /// A point of the history shown, read-only, while it is.
    pub viewing: Option<Viewing>,
    /// What is selected inside an asset: its path, and a path in its declaration (`stack.1`, `stack.0.ops.draw`). The one
    /// address every pane showing the asset shares.
    pub inner: Option<(String, String)>,
    pub filter: String,
    /// How the asset browser shows the project.
    pub browsing: Browsing,
    /// An asset just navigated to: the tree opens its ancestors and scrolls to it once.
    pub reveal: Option<String>,
    /// Decoded pictures by (image, variant, upscaling).
    pictures: HashMap<(String, usize, Upscale), Picture>,
    /// Parts of pictures (palettes' strips), as the inspector shows them.
    pub parts: Parts,
    /// The inspector views - the types', or the importers' - by (asset, the importer's, view).
    inspector_views: HashMap<(String, usize), Box<dyn pe2_ext::View>>,
    /// The panels' views, by pane.
    panels: HashMap<egui::Id, Box<dyn pe2_ext::View>>,
    /// What a view set of the asset it shows this frame, for the app to make after drawing.
    pub view_edit: Option<EditRequest>,
    /// The picture a stack's preview last showed.
    tapped: Option<Tapped>,
    /// The members of families of outputs assets are being looked at in: an image's look, a prefab's outfit.
    pub chosen: project::ext::Chosen,
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
    /// Take an entry gone from this layer out of its list: the layer and the uid. Any layer whose list can be written,
    /// its content read-only or not.
    Forget(String, String),
    /// List what a layer holds unlisted at a path, and under it: the layer and the path.
    Adopt(String, String),
    /// Remove what a layer holds unlisted at a path from its folder: the layer and the path.
    RemoveUnlisted(String, String),
    Resolve(String, project::edit::Resolution),
    /// Show the graph pane, for the graph layer chosen.
    ShowGraph,
    /// An edit that could not be made, and why: said to the user.
    Refused(String),
    /// Write a file of the entry (its `node.toml`; empty: a plain file itself), labeled for the history; edits of one
    /// merge key made in a row are one step.
    Write {
        uid: String,
        file: String,
        bytes: Vec<u8>,
        label: String,
        merge: Option<String>,
    },
}

/// A node just made: its uid, its path, and what it was made as.
pub struct NewNode {
    pub uid: String,
    pub path: String,
    pub label: String,
}

/// A point of the history shown, read-only: the documents as after its first `point` steps - one, or all - or, while A/B
/// flips to it, now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Viewing {
    pub point: usize,
    pub scope: Option<String>,
    pub now: bool,
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

/// How many GPU errors are kept: an extension failing a frame says the same each frame.
const MOST_GPU_ERRORS: usize = 8;

/// What a take-in did to the workspace, as the app is told of it.
#[derive(Default)]
pub enum Update {
    #[default]
    Nothing,
    /// This many assets were added, changed or removed.
    Refreshed(usize),
    /// The project was read again, as this changed: `project.toml` (its layers, the extensions it leaves off) or
    /// the extensions found (one in the project or the user's folder came, went or changed).
    Reloaded(&'static str),
    /// What someone asked for came to this.
    Said(String),
}

/// A part's name, what its fragment says after its group: `cursor` of `cells/cursor`.
#[must_use]
pub fn part_name(id: &str) -> &str {
    id.split_once('/').map_or(id, |(_, name)| name)
}

/// A stack's preview as last drawn: by asset, output and revision, or why it could not be made.
type Tapped = ((String, String, u64), Result<egui::TextureHandle, String>);

/// Parts of assets' pictures, uploaded once each: a palette's strip of colors (`palettes/<name>`), as its type draws it.
#[derive(Default)]
pub struct Parts {
    shown: HashMap<(String, String), Picture>,
    /// Those its type draws nothing of: asked once, like the others.
    none: HashSet<(String, String)>,
}

impl Parts {
    /// Whether an asset's part has been asked for: getting it costs nothing.
    pub fn has(&self, asset: &Asset, part: &str) -> bool {
        let key = (asset.path.clone(), part.to_string());
        self.shown.contains_key(&key) || self.none.contains(&key)
    }

    /// An asset's part, uploaded; none where its type draws no such part.
    pub fn get(&mut self, ctx: &egui::Context, extensions: &Extensions, asset: &Asset, part: &str) -> Option<&Picture> {
        let key = (asset.path.clone(), part.to_string());
        if self.none.contains(&key) {
            return None;
        }
        if !self.shown.contains_key(&key) {
            let Some(drawn) = extensions.output_image(asset, part) else {
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
    asset.fields_toml().get(key).and_then(toml::Value::as_array).map_or(&[][..], Vec::as_slice)
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
    cache: Cache,
    history: History,
    /// The label of an edit a crash left half done, rolled back.
    pub recovered: Option<String>,
    index: Index,
    store: project::graph::store::Store,
    extensions: Arc<Extensions>,
    extensions_found: Vec<extensions::Found>,
    imports: project::imports::Imports,
    layer_view: LayerView,
    tree: Arc<Node>,
    /// Agents' link to it, listening since its lock was taken; or why agents cannot reach it.
    link: std::result::Result<mcp::link::Listener, String>,
    /// The server of its files, kept and fed what is served.
    files: Option<crate::files::Files>,
}

/// What agents ask of the editor's own state, answered while the imports attach as the user has it then: what it shows,
/// showing another asset, what it costs.
const EDITOR_OWN: [&str; 3] = ["editor_selection", "editor_show", "editor_performance"];

/// Whether the opening waits for an agent once the link listens (`--wait-for-agent`), and whether one came.
static WAITING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static ARRIVED: (std::sync::Mutex<bool>, std::sync::Condvar) =
    (std::sync::Mutex::new(false), std::sync::Condvar::new());

/// Hold each opening, once agents can reach the editor, until the first asks it something (`--wait-for-agent`).
pub fn wait_for_agent() {
    WAITING.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// An agent asked something: what the opening waits for, where it does.
fn arrived() {
    *ARRIVED.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = true;
    ARRIVED.1.notify_all();
}

/// Hold here, where the opening waits for an agent, until one asks something.
fn await_agent(report: &project::index::Report<'_>) {
    if !WAITING.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    report("Waiting for an agent", 0, 0);
    let mut came = ARRIVED.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    while !*came {
        came = ARRIVED.1.wait(came).unwrap_or_else(std::sync::PoisonError::into_inner);
    }
}

/// Agents' requests, each with where its answer goes: what the link hands the window.
pub type AgentRequest = (mcp::Request, mpsc::Sender<mcp::Reply>);

/// Listen for agents on the project in `root`, which this process holds: each request handed to the window's frames
/// (`to_frames`), which wake for it, and its answer awaited. Started as the project's lock is taken, before the rest
/// of opening it, so that an agent reaches the editor - its window, and what it says of the project - all along.
fn listen(
    root: &Path,
    to_frames: mpsc::Sender<AgentRequest>,
    ctx: egui::Context,
) -> std::result::Result<mcp::link::Listener, String> {
    mcp::link::Listener::start(root, move |request| {
        arrived();
        let (reply, answered) = mpsc::channel();
        let delivered = to_frames.send((request, reply)).is_ok();
        ctx.request_repaint();
        let closed = || mcp::Reply::Error("the editor closed the project".into());
        if delivered { answered.recv().unwrap_or_else(|_| closed()) } else { closed() }
    })
    .map_err(|e| format!("agents cannot reach the editor: {e}"))
}

/// What a project being left has yet to finish. Let go of in this order: what writes first, the lock on all it writes
/// last - and only then what merely takes time: the index's memory, which nobody waits for.
pub struct Leaving {
    takein: Option<crate::takein::TakeIn>,
    runner: Option<crate::runner::Runner>,
    index: Option<Arc<Index>>,
    lock: Option<Lock>,
    /// The server kept: let go of once the project is, for whoever holds it next to feed.
    files: Option<crate::files::Files>,
    /// The host standing by, told: it takes the project the moment this goes, after the lock.
    handing: Option<project::host::Lease>,
}

impl Drop for Leaving {
    fn drop(&mut self) {
        // What may still write: the worker finishes the job it is on, an epoch running is given up.
        self.takein = None;
        self.runner = None;
        // The index writes through its store alone: closed, the project is whoever takes it next's, while the index
        // itself is freed after - some tenths of a second for a large project, which a host standing by would wait.
        // Where something else still has the index, it goes first, as it did.
        let mut index = self.index.take();
        match index.as_mut().and_then(Arc::get_mut) {
            Some(index) => index.let_go_of_store(),
            None => index = None,
        }
        self.lock = None;
        self.files = None;
        self.handing = None;
        drop(index);
    }
}

/// Have a host stand by for the project the editor holds (`project::host::stand_by`), off the caller's thread: the
/// one running, or `pe2-cli` beside the editor started as one. Where that is not there, none does.
fn stand_by(root: &Path) {
    let Ok(program) = std::env::current_exe().map(|exe| project::host::cli_beside(&exe)) else { return };
    if !program.is_file() {
        return;
    }
    let root = root.to_path_buf();
    let _ = std::thread::Builder::new().name("standby".into()).spawn(move || {
        if let Err(e) = project::host::stand_by(&root, &program) {
            eprintln!("no host stands by for {}: {e:#}", project::host::shown(&root));
        }
    });
}

/// An event asked for: the events, the config, and who waits for what became of it.
type Signaled = (Vec<String>, Option<project::Config>, Option<mpsc::Sender<mcp::Reply>>);

/// What is made of a project's `project.toml` and its files, and made again when either changes.
pub struct Loaded {
    pub project: Project,
    pub index: Index,
    pub extensions: Arc<Extensions>,
    pub found: Vec<extensions::Found>,
    layer_view: LayerView,
}

impl Loaded {
    /// The extensions as the project has them, and the assets as the user views them.
    pub fn of(
        project: Project,
        mut index: Index,
        cache: &Cache,
        store: &project::graph::store::Store,
        report: &project::index::Report<'_>,
    ) -> Loaded {
        // Without those it leaves off (`[extensions] disabled`). Those it holds before the built-ins.
        report("Loading the extensions", 0, 0);
        let found = extensions::found(&project);
        let extensions = extensions::for_project(&project);
        // What they are asked in passing reads and keeps what the project's store holds.
        extensions.keep_in(store.clone());
        let layer_view: LayerView = std::fs::read_to_string(cache.path(LAYER_VIEW))
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default();
        // The view first: hiding layers assembles the assets again, so the imports are attached to what it shows.
        index.set_hidden(&layer_view.hidden(&index.layers()));
        Loaded { project, index, extensions, found, layer_view }
    }
}

impl Workspace {
    /// Open the project in `dir` as far as a workspace starts from it, off the window's thread, saying how far it is.
    /// What the importers made is attached after that, by the worker, the project already showing (`Job::Attach`).
    ///
    /// # Errors
    /// Its `project.toml` or its assets cannot be read, or another holds it.
    pub fn prepare(
        dir: &Path,
        report: &project::index::Report<'_>,
        agents: (mpsc::Sender<AgentRequest>, egui::Context),
    ) -> Result<Opened> {
        report("Reading project.toml", 0, 0);
        let project = Project::load(dir)?;
        // An agent's request holds the project for a moment (`pe2-cli mcp`): outlast one, not a real holder.
        let lock = match Lock::acquire_within(&project.root, "pe2-editor", Duration::from_secs(2)) {
            // An agent's server answering a request of its own: brief, if longer than that - it lets go.
            Err(project::lock::LockError::Held(owner)) if owner.starts_with("pe2-cli mcp") => {
                Lock::acquire_within(&project.root, "pe2-editor", Duration::from_secs(30))?
            }
            locked => locked?,
        };
        // The server of its files, started where none runs, beside the opening: fed what is served once that is
        // listed.
        let files = crate::files::Files::keep(&project);
        // A host stands by where its files are served: holding nothing while the editor has the project, it takes
        // it once the editor lets go, and feeds the server from then on - closing the window ends no serving.
        if files.is_some() {
            stand_by(&project.root);
        }
        // Agents reach it from now on: what they ask of the window is answered as it opens.
        let link = listen(&project.root, agents.0, agents.1);
        if link.is_ok() {
            await_agent(report);
        }
        let cache = Cache::open(&project, &lock)?;
        // The history first: a transaction a crash left half done is rolled back before the index reads the files.
        // Journaled: a session a crash ends is offered back on the next opening.
        let (history, recovered) = History::open(&cache)?;
        let history = history.journaled();
        let index = Index::open_reporting(&project, &cache, report)?;
        // What is served of it, listed for the server, which has no index: it serves from this first listing on.
        if let Some(files) = &files {
            files.show(Arc::new(index.served()));
        }
        // The work graph's store (what was kept as artifacts before it, gone); attaching the imports sweeps it.
        let store = project::graph::store::Store::open(&cache)?;
        let old = cache.path("artifacts");
        if old.exists() {
            std::fs::remove_dir_all(&old).with_context(|| old.display().to_string())?;
        }
        let Loaded { project, index, extensions, found: extensions_found, layer_view } =
            Loaded::of(project, index, &cache, &store, report);
        // What the assets' importers made of their sources, seen as their files.
        let imports = project::imports::Imports::open(&cache)?;
        report("Building the tree", 0, 0);
        let tree = Arc::new(Node::build(&index, &extensions));
        Ok(Opened {
            project,
            lock,
            cache,
            history,
            recovered,
            index,
            store,
            extensions,
            extensions_found,
            imports,
            layer_view,
            tree,
            link,
            files,
        })
    }

    /// The workspace of a project opened (`prepare`): its worker started, its index shown.
    #[expect(clippy::too_many_lines, reason = "what it holds, one line each")]
    pub fn from_opened(opened: Opened, requests: mpsc::Receiver<AgentRequest>) -> Workspace {
        let Opened {
            project,
            lock,
            cache,
            history,
            index,
            store,
            extensions,
            extensions_found,
            imports,
            layer_view,
            tree,
            link,
            files,
            ..
        } = opened;
        let (link, link_error) = match link {
            Ok(link) => (Some(link), None),
            Err(why) => (None, Some(why)),
        };
        let thumbs = crate::thumbs::Thumbs::start(extensions.clone(), store.clone());
        let layers = index.layers();
        // The worker takes the index from here; the UI shows a snapshot of it.
        let shown = Arc::new(index.clone());
        let pictures = crate::pictures::Pictures::start(shown.clone(), extensions.clone());
        let outputs = project::ext::requests::Requests::start(shown.clone());
        outputs.set_extensions(&extensions);
        extensions.serve_requests(Some(outputs.clone()));
        let cache_dir = cache.dir.clone();
        let mut takein = crate::takein::TakeIn::start(crate::takein::Worker {
            project: project.clone(),
            index,
            cache,
            store: store.clone(),
            imports,
            extensions: extensions.clone(),
            found: extensions_found.clone(),
            tree: tree.clone(),
            stage: Arc::default(),
        });
        let attaching = takein.attach();
        let runner = crate::runner::Runner::start(project.clone(), extensions.clone());
        let mut workspace = Workspace {
            runner,
            config: None,
            config_told: None,
            last_epoch: None,
            new_event_name: String::new(),
            gpu_errors: Vec::new(),
            project,
            history,
            tests: crate::tests_pane::Tests::default(),
            thumbs,
            graph_ask: None,
            asset_folder: String::new(),
            folders_back: Vec::new(),
            folders_forward: Vec::new(),
            marked_folder: None,
            in_browser: false,
            folder_moved: false,
            players: HashMap::new(),
            view_playback: None,
            view_transport: Vec::new(),
            played: HashSet::new(),
            extensions,
            store,
            attaching,
            outputs,
            requests_seen: 0,
            held_events: vec![(vec![ProjectOpened::NAME.to_string()], None, None)],
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
            files,
            handing: None,
            watch_starting: None,
            plans: crate::plans::Plans::default(),
            fill: crate::fill::Fill::default(),
            watch_error: None,
            _link: link,
            requests: Some(requests),
            window_requests: Vec::new(),
            fetcher: crate::fetcher::Fetcher::default(),
            link_error,
            extensions_found,
            extensions_checked: std::time::Instant::now(),
            takein,
            pending: BTreeSet::new(),
            revision: 0,
            focused: true,
            own: Vec::new(),
            external: (0, 0),
            selection_after: None,
            agents_waiting: Vec::new(),
            outcome: Update::Nothing,
            tree,
            editable: HashSet::new(),
            selected: None,
            viewing: None,
            inner: None,
            filter: String::new(),
            browsing: Browsing::default(),
            reveal: None,
            pictures: HashMap::new(),
            parts: Parts::default(),
            inspector_views: HashMap::new(),
            panels: HashMap::new(),
            view_edit: None,
            tapped: None,
            chosen: project::ext::Chosen::new(),
            field_edit: None,
            viewer: crate::viewer::Viewer::default(),
            viewed_file: HashMap::new(),
            declaration_draft: None,
        };
        workspace.refresh_editable();
        workspace
    }

    /// Start watching the layers, once: every change on disk is then brought in by `update`. Asked each frame: what
    /// has started since is told of the window, and an event waits for the project to have what it is about.
    pub fn watch(&mut self, ctx: &egui::Context) {
        if !self.outputs.wakes() {
            let ctx = ctx.clone();
            self.outputs.wake_by(move |after| ctx.request_repaint_after(after));
        }
        self.takein.wake(ctx);
        let window = ctx.clone();
        self.runner.wake(move || window.request_repaint());
        self.plans.update(&self.project, &self.extensions, ctx);
        if let Some(pictures) = &self.textures.pictures {
            pictures.wake(ctx);
        }
        if !self.holding_events() {
            for (roots, config, reply) in std::mem::take(&mut self.held_events) {
                self.signal_all(roots, config, reply);
            }
        }
        if let Some(starting) = &self.watch_starting
            && let Ok(started) = starting.try_recv()
        {
            self.watch_starting = None;
            match started {
                Ok(watch) => self.watch = Some(watch),
                Err(why) => self.watch_error = Some(why),
            }
        }
        if self.watch.is_some() || self.watch_error.is_some() || self.watch_starting.is_some() {
            return;
        }
        let root = &self.project.root;
        let roots: Vec<PathBuf> = self.project.layers.iter().map(|l| l.path.clone()).collect();
        // Each layer's changeset sits beside its directory; project.toml in the project's own.
        let mut shallow: Vec<PathBuf> =
            self.project.layers.iter().filter_map(|l| l.changeset.parent().map(Path::to_path_buf)).collect();
        shallow.push(root.clone());
        shallow.dedup();
        // Not the project's: its cache, what its builds write, and what is mounted unmanaged (the discs).
        let mut ignore = vec![self.cache_dir.clone(), root.join("build")];
        ignore.extend(self.project.unmanaged.iter().map(|m| m.path.clone()));
        // Reading every folder of a project to watch it takes more than a frame has.
        let (ctx, (started, starting)) = (ctx.clone(), mpsc::channel());
        let start = move || {
            let watch = Watch::start(&roots, &shallow, ignore, ctx.clone())
                .map_err(|e| format!("the layers are not watched: {e}"));
            let _ = started.send(watch);
            ctx.request_repaint();
        };
        if std::thread::Builder::new().name("watch".into()).spawn(start).is_ok() {
            self.watch_starting = Some(starting);
        }
    }

    /// How many views are alive: what going on as it is keeps.
    #[cfg(test)]
    pub fn views_alive(&self) -> usize {
        self.views.len()
    }

    /// Whether an output is on its way to a view.
    pub fn requests_busy(&self) -> bool {
        self.outputs.busy()
    }

    /// Whether the layers are yet to be watched.
    pub fn watch_is_starting(&self) -> bool {
        self.watch_starting.is_some()
    }

    /// Whether what the importers made is attached: until it is, the project shows without their outputs.
    pub fn attached(&self) -> bool {
        self.takein.covers(self.attaching)
    }

    /// How far the project being read again or attached is, while it is.
    pub fn reading(&self) -> Option<String> {
        self.takein.stage()
    }

    /// Read the project again, as this changed: it goes on showing until the new one is ready.
    pub fn reload(&mut self, why: &'static str) {
        self.takein.reload(why, false);
    }

    /// Read the project again, its files too, as someone asked.
    pub fn reload_whole(&mut self) {
        self.takein.reload("Reload", true);
    }

    /// The config in use: what events set off are for, and, told the extensions as it changes, what is made of the
    /// assets is made for (a file the releases have apart is read as its release has it).
    pub fn set_config(&mut self, config: Option<project::Config>) {
        // What extensions are told is the config as built: its properties over the project's defaults, and what every
        // config says - changed by more than this config's own table.
        let built = config.as_ref().map(|c| project::build::build_config(&self.project, c));
        let disabled = config.as_ref().map_or_else(Vec::new, |c| c.disabled.clone());
        let told = (Arc::as_ptr(&self.extensions) as usize, built, disabled);
        if self.config_told.as_ref() != Some(&told) {
            self.extensions.set_config(told.1.clone(), &told.2);
            self.config_told = Some(told);
        }
        self.config = config;
    }

    /// Read the project again, as this changed (not its files).
    pub fn reload_project(&mut self, why: &'static str) {
        self.takein.reload(why, false);
    }

    /// Take in what is at these paths now, as another program wrote it (a plan run). The take-in's ticket.
    pub fn take_in(&mut self, paths: Vec<PathBuf>) -> u64 {
        self.external.0 = self.takein.paths(paths);
        self.external.0
    }

    /// Run a generated layer's plan as the work graph's only work: after the epoch running, before what is asked next.
    pub fn extract(&mut self, extraction: crate::runner::Extraction) {
        self.runner.extract(extraction);
    }

    /// Whether events wait to be signaled: the project is not attached yet, or a plan is filling a layer.
    fn holding_events(&mut self) -> bool {
        let takein = &self.takein;
        !self.attached() || self.fill.holds_events(|ticket| takein.covers(ticket))
    }

    /// While the project is being read or attached, the ticket of the last job asked: what would have to wait for it is
    /// better done after, the window going on, than waited for.
    pub fn reading_until(&self) -> Option<u64> {
        (self.takein.stage().is_some() || !self.attached()).then(|| self.takein.issued())
    }

    /// Leave the project. What takes a moment to finish - what the worker is on, an epoch, letting go of the index - is
    /// handed back, to finish away from the window.
    pub fn leave(mut self) -> Leaving {
        self.leaving();
        // The extensions outlive a project in what is loaded: what it was asked should not.
        self.extensions.serve_requests(None);
        // Closed cleanly, its session goes - unless documents are unsaved still: the next opening offers them back.
        if self.history.documents().is_empty() {
            self.history.close();
        }
        Leaving {
            takein: Some(self.takein),
            runner: Some(self.runner),
            index: Some(self.index),
            lock: Some(self.lock),
            files: self.files,
            handing: self.handing,
        }
    }

    /// The project is about to be let go of: the host standing by is told, once, and takes it the moment the editor
    /// has let go, without looking for it.
    fn leaving(&mut self) {
        if self.handing.is_none() && self.files.is_some() {
            self.handing = project::host::leaving(&self.project.root, "pe2-editor");
        }
    }

    /// Have the worker read until what is returned goes.
    #[cfg(any(test, feature = "tests"))]
    pub fn hold_worker(&mut self) -> mpsc::Sender<()> {
        self.takein.hold()
    }

    /// Whether what this ticket asked is done.
    pub fn covers(&self, ticket: u64) -> bool {
        self.takein.covers(ticket)
    }

    /// Signal an event: an epoch begins, on the project as last taken in, for the config in use.
    pub fn signal(&mut self, event: &str) {
        self.signal_all(vec![event.to_string()], None, None);
    }

    /// Define an event of the project: an asset of the active layer.
    ///
    /// # Errors
    /// No layer takes it, or it cannot be written.
    pub fn new_event(&mut self, name: &str) -> Result<(), String> {
        let layer = self.active_layer().ok_or("no layer is active")?.name.clone();
        let id = project::uid::new(|id| self.index.by_uid.contains_key(&project::uid::text(id)))
            .map_err(|e| e.to_string())?;
        let uid = project::uid::text(id);
        let slug: String =
            name.chars().map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect();
        let quoted = toml::Value::String(name.to_string());
        let declaration = format!("uid = \"{uid}\"\n\n[[stack]]\nop = \"pe2.events@event\"\nlabel = {quoted}\n");
        project::edit::create(
            &self.project,
            &self.index,
            &layer,
            &uid,
            &format!("events/{slug}"),
            (&declaration, &[]),
            &mut self.history,
        )
        .map_err(|e| format!("{e:#}"))?;
        Ok(())
    }

    /// Make a node of a template in the folder shown, of the active layer: with this file in it, where it takes one.
    ///
    /// # Errors
    /// No layer takes it, the template does not stamp, the file cannot be read or the node cannot be written.
    pub fn new_node(&mut self, template: &str, file: Option<&Path>, folder: &str) -> Result<NewNode, String> {
        let layer = self.active_layer().ok_or("no layer is active")?.name.clone();
        let found = self.extensions.template(template).ok_or_else(|| format!("no template {template}"))?;
        let name =
            file.and_then(|file| file.file_name()).and_then(|name| name.to_str()).unwrap_or_default().to_string();
        let stem = file.and_then(|file| file.file_stem()).and_then(|stem| stem.to_str()).unwrap_or(template);
        let slug: String =
            stem.chars().map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect();
        let at = |n: usize| {
            let leaf = if n == 0 { slug.clone() } else { format!("{slug}_{n}") };
            if folder.is_empty() { leaf } else { format!("{folder}/{leaf}") }
        };
        let path = (0..=self.index.assets.len())
            .map(at)
            .find(|path| !self.index.assets.contains_key(path))
            .unwrap_or_default();
        let stack = self.extensions.stamp(&found, &name)?;
        // A stack naming its own file has the file under that name.
        let name = project::graph::layers::stored_file(&stack).unwrap_or(name);
        let id = project::uid::new(|id| self.index.by_uid.contains_key(&project::uid::text(id)))
            .map_err(|e| e.to_string())?;
        let uid = project::uid::text(id);
        let declaration = format!("uid = \"{uid}\"\n\n{stack}");
        let bytes = file.map(std::fs::read).transpose().map_err(|e| e.to_string())?;
        let files: Vec<(&str, &[u8])> = bytes.as_deref().map(|bytes| (name.as_str(), bytes)).into_iter().collect();
        project::edit::create(
            &self.project,
            &self.index,
            &layer,
            &uid,
            &path,
            (&declaration, &files),
            &mut self.history,
        )
        .map_err(|e| format!("{e:#}"))?;
        // Shown once it is taken in: until then nothing is there to open.
        let label = found.label.clone();
        let ticket = self.take_written();
        self.selection_after = ticket.map(|ticket| (ticket, path.clone()));
        Ok(NewNode { uid, path, label })
    }

    /// The folder new nodes go in: the one the asset in use is in, else the one the browser shows.
    #[must_use]
    pub fn new_node_folder(&self, in_use: Option<&str>) -> String {
        match in_use.filter(|path| self.index.assets.contains_key(*path)) {
            Some(path) => path.rsplit_once('/').map_or_else(String::new, |(folder, _)| folder.to_string()),
            None => self.asset_folder.clone(),
        }
    }

    /// Make a node of this declaration and these files, in the active layer: what a test works on.
    ///
    /// # Errors
    /// No layer is active, or it cannot be written.
    #[cfg(any(test, feature = "tests"))]
    pub fn make_node(&mut self, uid: &str, path: &str, node: (&str, &[(&str, &[u8])])) -> Result<(), String> {
        let layer = self.active_layer().ok_or("no layer is active")?.name.clone();
        project::edit::create(&self.project, &self.index, &layer, uid, path, node, &mut self.history)
            .map(drop)
            .map_err(|e| format!("{e:#}"))
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

    /// Set a config's property, or leave it to its default (`None`): written in `project.toml`, as a step.
    ///
    /// # Errors
    /// The value is not as the project declares it, or it cannot be written.
    pub fn set_property(&mut self, config: &str, property: &str, to: Option<&toml::Value>) -> Result<(), String> {
        project::setup::set_property(&self.project, config, property, to, &mut self.history)
            .map_err(|e| format!("{e:#}"))
    }

    /// Stop the epoch running.
    pub fn cancel_epoch(&self) {
        self.runner.cancel();
    }

    /// An epoch is running or waiting.
    pub fn in_epoch(&self) -> bool {
        self.runner.busy()
    }

    /// A file of the project's cache, by name.
    pub fn cache_path(&self, name: &str) -> PathBuf {
        self.cache_dir.join(name)
    }

    /// What the epoch running does, as the status bar says it: by the events it was signaled by.
    pub fn epoch_doing(&self) -> String {
        let roots = self.runner.running();
        let said: Vec<String> = roots
            .iter()
            .map(|root| match root.as_str() {
                name if name == Build::NAME => "building".to_string(),
                name if name == Clean::NAME => "cleaning".to_string(),
                name if name == FilesChanged::NAME => "following the files changed".to_string(),
                name if name == ProjectOpened::NAME => "following the project opened".to_string(),
                other => format!("running {other}"),
            })
            .collect();
        if said.is_empty() { "waiting to run".to_string() } else { said.join(", ") }
    }

    fn signal_all(
        &mut self,
        roots: Vec<String>,
        config: Option<project::Config>,
        reply: Option<mpsc::Sender<mcp::Reply>>,
    ) {
        if self.holding_events() {
            self.held_events.push((roots, config, reply));
            return;
        }
        let Some(config) = config.or_else(|| self.config.clone()) else {
            if let Some(reply) = reply {
                let _ = reply.send(mcp::Reply::failed("the project has no config".to_string()));
            }
            return;
        };
        self.runner.run(crate::runner::Job { roots, index: self.index.clone(), config, reply });
    }

    /// Answer the agents' requests that arrived since the last frame, against the index and the selection as shown. A
    /// request first has what other programs changed taken in - an agent reads what it just wrote - and waits for it.
    pub fn answer_agents(&mut self) {
        let Some(requests) = &self.requests else { return };
        let arrived: Vec<_> = requests.try_iter().collect();
        for (request, reply) in arrived {
            if let mcp::Request::Signal { events, config } = &request {
                let config =
                    config.as_ref().and_then(|name| self.project.configs.iter().find(|c| c.name == *name)).cloned();
                self.signal_all(events.clone(), config, Some(reply));
                continue;
            }
            if request.uses_window() {
                self.window_requests.push((request, reply));
                continue;
            }
            // What is the editor's own - what it shows, what it costs - the user has while the imports attach; the
            // agents too.
            if !self.attached()
                && matches!(&request, mcp::Request::Call { name, .. } if EDITOR_OWN.contains(&name.as_str()))
            {
                self.answer_agent(&request, &reply);
                continue;
            }
            // Opened, what its importers made not yet attached: said now, not left to wait for it.
            if !self.attached() {
                let stage = self.reading().unwrap_or_else(|| "taking in what its importers make".to_string());
                let said = format!("the editor is still opening the project ({stage}): ask again once it is open");
                let _ = reply.send(mcp::Reply::Error(said));
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
        if let mcp::Request::Edit { op, layer } = request {
            let answer = match self.apply_edit(op, layer.as_deref()) {
                Ok((label, touched)) => mcp::Reply::Edited { label, touched },
                Err(why) => mcp::Reply::failed(why),
            };
            let _ = reply.send(answer);
            return;
        }
        if let mcp::Request::Edits { ops, layer } = request {
            let (made, touched) = self.apply_edits(ops, layer.as_deref());
            let _ = reply.send(mcp::Reply::EditedEach { made, touched });
            return;
        }
        let selection = self.selected.clone();
        let variant = selection.as_deref().and_then(|s| self.variants_of(s).get(self.variant(s)).cloned());
        let editor = mcp::EditorState { selection, variant, show: None, performance: crate::perf::read() };
        let mut view = mcp::View {
            project: &self.project,
            index: &self.index,
            extensions: &self.extensions,
            editor: Some(editor),
        };
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
        // The project's extensions, and where they are kept to be added, looked at each second: none is in the index.
        if self.extensions_checked.elapsed() >= Duration::from_secs(1) {
            self.extensions_checked = std::time::Instant::now();
            let found = extensions::found(&self.project);
            if found != self.extensions_found {
                // Asked once: what it is taken for is what was found then, read again or not.
                self.extensions_found = found;
                self.takein.reload("an extension changed", false);
            }
        }
        // What said it was waiting for an output is out of date once that is there.
        let answered = self.outputs.finished();
        if answered != self.requests_seen {
            self.requests_seen = answered;
            self.contents.clear();
            self.part_sheets.clear();
        }
        // What the editor wrote (an edit, a step, a setting of the project): taken in at once.
        self.take_written();
        self.own.retain(|(_, _, at)| at.elapsed() < Duration::from_secs(30));
        if let Some(watch) = &self.watch {
            for change in watch.poll() {
                match change {
                    Change::Paths(changed) => {
                        let theirs: Vec<PathBuf> =
                            changed.into_iter().filter(|p| !self.is_own(p) && self.concerns(p)).collect();
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
        self.gpu_errors.extend(pe2_webgpu::take_errors());
        self.gpu_errors.truncate(MOST_GPU_ERRORS);
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

    /// Whether a path changing concerns the project: in a layer, a layer's changeset, or one of its files at its root.
    /// What else sits in its folders (exports, a user's notes) does not.
    fn concerns(&self, path: &Path) -> bool {
        let root = &self.project.root;
        let at_root = path.parent() == Some(root.as_path())
            && path.extension().is_some_and(|e| e.eq_ignore_ascii_case("toml"))
            && path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("project"));
        at_root || self.project.layers.iter().any(|l| path.starts_with(&l.path) || path == l.changeset)
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
            if let Some(said) = snapshot.said {
                self.outcome = Update::Said(said);
            }
            let before =
                (std::mem::replace(&mut self.index, snapshot.index), std::mem::replace(&mut self.tree, snapshot.tree));
            self.revision += 1;
            if let Some(reloaded) = snapshot.reloaded {
                self.take_up(reloaded, snapshot.all);
            }
            if snapshot.files {
                self.signal(FilesChanged::NAME);
            }
            if let (Some(files), Some(served)) = (&self.files, snapshot.served) {
                files.show(served);
            }
            if let Some(pictures) = &mut self.textures.pictures {
                if snapshot.all {
                    pictures.clear();
                }
                pictures.set_index(self.index.clone());
            }
            self.outputs.set_index(self.index.clone());
            crate::aside::drop_later(before);
            self.layers = snapshot.layers;
            self.refresh_editable();
            if snapshot.all || self.textures.live.as_ref().is_some_and(|live| snapshot.changed.contains(&live.asset)) {
                self.textures.live = None;
            }
            if snapshot.all {
                self.pictures.clear();
                self.parts = Parts::default();
                self.inspector_views.clear();
                self.panels.clear();
                self.views.clear();
                self.contents.clear();
                self.part_sheets.clear();
                self.thumbs.clear();
                self.outputs.clear();
            } else if !snapshot.changed.is_empty() {
                let changed = &snapshot.changed;
                self.pictures.retain(|(image, ..), _| !changed.contains(image));
                self.parts.forget(changed);
                self.panels.clear();
                self.thumbs.forget(changed);
                // A view shows what it refers to as well: a model what its material says.
                let reached = reached_from(&self.index, changed);
                self.inspector_views.retain(|(asset, ..), _| !reached.contains(asset));
                self.views.retain(|(_, asset, _), _| !reached.contains(asset));
                self.contents.retain(|asset, _| !reached.contains(asset));
                self.part_sheets.retain(|(asset, _), _| !reached.contains(asset));
                self.outputs.forget(&|asset| reached.contains(asset));
            }
            self.drop_lost_selection();
            // Other programs' changes taken in: the app says how many assets they changed.
            let (asked, told) = self.external;
            if asked > told && snapshot.ticket >= asked {
                self.external.1 = asked;
                if !matches!(self.outcome, Update::Reloaded(_)) && !snapshot.changed.is_empty() {
                    let before = if let Update::Refreshed(n) = self.outcome { n } else { 0 };
                    self.outcome = Update::Refreshed(before + snapshot.changed.len());
                }
            }
        }
        // The documents follow the disk under them: stale where it changed, saved where it is their working copy.
        if self.history.reconcile(&self.index) {
            self.send_working();
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

    /// Go on with the project as it was read again: what works for the extensions starts anew with the new ones, and what
    /// was showing is shown by them.
    fn take_up(&mut self, reloaded: crate::takein::Reloaded, all: bool) {
        let crate::takein::Reloaded { project, extensions, found, why } = reloaded;
        let places = |project: &Project| project.layers.iter().map(|l| l.path.clone()).collect::<Vec<_>>();
        if places(&project) != places(&self.project) {
            self.watch = None;
        }
        self.runner.restart(project.clone(), extensions.clone());
        self.project = project;
        self.plans.again();

        self.extensions_found = found;
        self.outcome = Update::Reloaded(why);
        // What came of it is what was showing: it goes on.
        if Arc::ptr_eq(&extensions, &self.extensions) && !all {
            return;
        }
        self.thumbs = crate::thumbs::Thumbs::start(extensions.clone(), self.store.clone());
        self.extensions.serve_requests(None);
        self.outputs.set_extensions(&extensions);
        extensions.serve_requests(Some(self.outputs.clone()));
        let pictures = crate::pictures::Pictures::start(self.index.clone(), extensions.clone());
        crate::aside::drop_later(self.textures.pictures.replace(pictures));
        self.extensions = extensions;
        self.players.clear();
        self.played.clear();
        self.view_edit = None;
        self.field_edit = None;
        self.signal(ProjectOpened::NAME);
    }

    /// Undo the last step, or redo the one undone last - of them all, or the last editing one document (`scope`);
    /// forced, over what changed since outside the editor. What it changed is taken in, and what was selected when the
    /// step was made is shown again once it is. Returns the step's label.
    ///
    /// # Errors
    /// Nothing to undo or redo, files changed outside the editor (unless forced), or a file not written.
    pub fn step(&mut self, redo: bool, force: bool, scope: Option<&str>) -> Result<String, StepError> {
        if let Some(why) = self.read_only() {
            return Err(StepError::Io(anyhow::anyhow!(why)));
        }
        self.settle();
        let step = if redo {
            self.history.redo(&self.index, scope, force)?
        } else {
            self.history.undo(&self.index, scope, force)?
        };
        let (label, selection, edited) = (step.label.clone(), step.selection.clone(), !step.edits.is_empty());
        let mut ticket = self.take_written();
        if edited {
            ticket = Some(self.send_working());
        }
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
            return row
                || self.extensions.addresses(&asset).iter().any(|(address, _)| address == part)
                || !self.extensions.contents_views(&asset).is_empty();
        }
        crate::inspector::layer_of_inner(&inner).is_none_or(|(at, op)| {
            let layer =
                asset.fields_toml().get("stack").and_then(toml::Value::as_array).and_then(|layers| layers.get(at));
            layer.is_some_and(|layer| op.is_none_or(|op| layer.get("ops").and_then(|ops| ops.get(op)).is_some()))
        })
    }

    // --- the layers ---------------------------------------------------------------------------------------------------

    /// A layer is locked by nature where only its maker writes it - a generated layer, a read-only tree - or by hand.
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
    /// Why it cannot: none is active, it is locked (by nature, by hand, by a solo), or a point of the history is shown.
    pub fn editable_layer(&self) -> Result<&LayerInfo, String> {
        if let Some(why) = self.read_only() {
            return Err(why.to_string());
        }
        let layer = self.active_layer().ok_or("no layer is active")?;
        if self.is_locked(layer) {
            let why = if self.locked_by_solo(layer) { "while another layer is soloed" } else { "it is locked" };
            return Err(format!("{} cannot be edited: {why}", layer.name));
        }
        Ok(layer)
    }

    // --- documents ------------------------------------------------------------------------------------------------

    /// An asset's document, where it has unsaved changes.
    pub fn document(&self, path: &str) -> Option<&project::history::Document> {
        let uid = self.index.assets.get(path).and_then(|a| a.uid.as_deref())?;
        self.history.document(uid)
    }

    /// Whether an asset has unsaved changes.
    pub fn unsaved(&self, path: &str) -> bool {
        self.document(path).is_some()
    }

    /// The documents with unsaved changes: their uids and paths.
    pub fn unsaved_documents(&self) -> Vec<(String, String)> {
        self.history.documents().iter().map(|(uid, d)| (uid.clone(), d.path.clone())).collect()
    }

    /// Show the working copies - as they are now, or as at the point of the history viewed: the index takes them in,
    /// in place of the files on disk.
    fn send_working(&mut self) -> u64 {
        let working = match &self.viewing {
            Some(viewing) if !viewing.now => self.history.working_at(viewing.point, viewing.scope.as_deref()),
            _ => self.history.working(),
        };
        self.takein.working(working)
    }

    /// Why nothing can be edited now: a point of the history is viewed.
    fn read_only(&self) -> Option<&'static str> {
        self.viewing.as_ref().map(|_| "an earlier point of the history is shown: back to now to edit")
    }

    /// Show a point of the history, read-only - or now again.
    pub fn view(&mut self, viewing: Option<Viewing>) {
        if self.viewing != viewing {
            self.viewing = viewing;
            self.send_working();
        }
    }

    /// Put the documents back as at the point viewed, as a new step, and show now. Returns what was done.
    pub fn restore_viewed(&mut self) -> String {
        let Some(viewing) = self.viewing.take() else { return String::new() };
        self.settle();
        let at = viewing.point.checked_sub(1).and_then(|i| self.history.done().get(i));
        let label = at
            .map_or_else(|| "Restore to the start".to_string(), |step| format!("Restore to after \"{}\"", step.label));
        let changed = self.history.restore(&self.index, viewing.point, viewing.scope.as_deref(), label.clone());
        if changed {
            self.history.note_selection(self.selected.clone());
        }
        self.send_working();
        if changed { label } else { "Nothing to restore: the documents are as they were".into() }
    }

    /// Take back one step's edits, as a new step. Returns what was done.
    ///
    /// # Errors
    /// It cannot be: a point is viewed, it is no edit, or what it edited was edited since.
    pub fn revert_step(&mut self, at: usize) -> Result<String, String> {
        if let Some(why) = self.read_only() {
            return Err(why.into());
        }
        self.settle();
        let label = format!("Take back {}", self.history.done().get(at).map_or("", |s| s.label.as_str()));
        if self.history.revert_step(&self.index, at, label.clone())? {
            self.history.note_selection(self.selected.clone());
            self.send_working();
        }
        Ok(label)
    }

    /// Name the point the history is at.
    pub fn checkpoint(&mut self, name: String) {
        self.history.checkpoint(name);
    }

    /// Make an edit asked from outside the window - the command line's, a program's through the served folder - in a
    /// layer, by default the active one, as a step of the session's log: its label, and the paths it touched. Undo and
    /// redo are the project's, as the History pane's.
    ///
    /// # Errors
    /// The layer cannot be edited, or the edit cannot be made.
    pub fn apply_edit(
        &mut self,
        op: &vfs::ops::Op,
        layer: Option<&str>,
    ) -> Result<(String, Vec<std::path::PathBuf>), String> {
        if let vfs::ops::Op::Undo | vfs::ops::Op::Redo = op {
            let redo = matches!(op, vfs::ops::Op::Redo);
            let touched = {
                let steps = if redo { self.history.undone() } else { self.history.done() };
                steps.iter().rev().find(|s| s.undoable() || redo).map_or_else(Vec::new, project::history::Step::paths)
            };
            let verb = if redo { "Redid" } else { "Undid" };
            return self
                .step(redo, false, None)
                .map(|label| (format!("{verb} {label}"), touched))
                .map_err(|e| e.to_string());
        }
        let layer = match layer {
            Some(name) => {
                let info = self.layers.iter().find(|l| l.name == name).ok_or(format!("no layer {name}"))?;
                if let Some(why) = self.read_only() {
                    return Err(why.to_string());
                }
                if self.is_locked(info) {
                    return Err(format!("{name} cannot be edited: it is locked"));
                }
                name.to_string()
            }
            None => self.editable_layer()?.name.clone(),
        };
        self.settle();
        let (label, mut touched) = self.apply_op(op, &layer)?;
        touched.extend(self.history.written().iter().cloned());
        self.take_written();
        Ok((label, touched))
    }

    /// An edit made over the index as it is, as a step of the history: what it wrote is yet to be taken in.
    fn apply_op(&mut self, op: &vfs::ops::Op, layer: &str) -> Result<(String, Vec<std::path::PathBuf>), String> {
        vfs::ops::apply(op, &self.project, &self.index, layer, &mut self.history).map_err(|e| format!("{e:#}"))
    }

    /// Edits made together, each a step of its own, taken in once for all of them - what programs wrote at once
    /// through the served folder: each one's label, or why it was not made, and every path they touched. One that
    /// cannot be made over the index as it is may need what those before it made: they are taken in, and it is tried
    /// once more.
    pub fn apply_edits(
        &mut self,
        ops: &[vfs::ops::Op],
        layer: Option<&str>,
    ) -> (Vec<Result<String, String>>, Vec<std::path::PathBuf>) {
        let plain = |op: &vfs::ops::Op| !matches!(op, vfs::ops::Op::Undo | vfs::ops::Op::Redo);
        // What takes steps back, or names its layer, is made as one edit alone is.
        if layer.is_some() || !ops.iter().all(plain) {
            let mut touched = Vec::new();
            let made = ops.iter().map(|op| {
                let (label, paths) = self.apply_edit(op, layer)?;
                touched.extend(paths);
                Ok(label)
            });
            return (made.collect(), touched);
        }
        let layer = match self.editable_layer() {
            Ok(layer) => layer.name.clone(),
            Err(why) => return (ops.iter().map(|_| Err(why.clone())).collect(), Vec::new()),
        };
        self.settle();
        let (mut touched, mut pending) = (Vec::new(), false);
        let mut made = Vec::with_capacity(ops.len());
        for op in ops {
            let mut tried = self.apply_op(op, &layer);
            if tried.is_err() && pending {
                self.take_written();
                self.settle();
                pending = false;
                tried = self.apply_op(op, &layer);
            }
            made.push(tried.map(|(label, paths)| {
                touched.extend(paths);
                pending = true;
                label
            }));
        }
        touched.extend(self.history.written().iter().cloned());
        self.take_written();
        (made, touched)
    }

    pub fn save(&mut self, uid: &str) -> anyhow::Result<String> {
        if let Some(why) = self.read_only() {
            anyhow::bail!(why);
        }
        self.settle();
        let Some(document) = self.history.document(uid) else { return Ok(String::new()) };
        let path = document.path.clone();
        let layer = self.editable_layer().map_err(anyhow::Error::msg)?.name.clone();
        if let Some(choices) = self.override_choice(uid).map_err(anyhow::Error::msg)? {
            anyhow::bail!(
                "code is overridden in a files layer ({})",
                if choices.is_empty() { "none yet".into() } else { choices.join(", ") }
            );
        }
        let files = self.history.to_save(uid)?;
        let label = format!("Save {}", path.rsplit('/').next().unwrap_or(&path));
        if !files.is_empty() {
            project::edit::save(&self.project, &self.index, &layer, uid, &files, label.clone(), &mut self.history)?;
        }
        self.history.saved(uid);
        self.take_written();
        self.send_working();
        Ok(label)
    }

    /// Put a document back as on disk, a step undo takes back: its unsaved changes undone - or, stale, the disk's
    /// taken. Returns what was done.
    pub fn revert(&mut self, uid: &str) -> String {
        if let Some(why) = self.read_only() {
            return why.into();
        }
        self.settle();
        let Some(document) = self.history.document(uid) else { return String::new() };
        let name = document.path.rsplit('/').next().unwrap_or(&document.path).to_string();
        let label = if document.stale.is_some() { format!("Take {name} as on disk") } else { format!("Revert {name}") };
        self.history.revert(uid, label.clone());
        self.history.note_selection(self.selected.clone());
        self.send_working();
        label
    }

    /// Keep a stale document's working copy: what is on disk now is what saving it overwrites.
    pub fn keep_mine(&mut self, uid: &str) {
        self.history.keep_mine(uid);
    }

    /// Let every unsaved change go (the project is closing without saving).
    pub fn discard_unsaved(&mut self) {
        self.history.discard_documents();
        self.send_working();
    }

    /// The session a crash ended, where one was left and is not taken up or let go yet: how many steps and unsaved
    /// documents it has.
    pub fn left_session(&self) -> Option<(usize, usize)> {
        self.history.left().map(|left| (left.done.len() + left.undone.len(), left.documents.len()))
    }

    /// Take up the session a crash ended, or let it go.
    pub fn resume_left(&mut self, resume: bool) {
        if resume {
            self.history.resume();
            self.history.reconcile(&self.index);
            self.send_working();
        } else {
            self.history.discard_left();
        }
    }

    /// Make an edit: of a document, its working copy changed; anything else, made in the active layer at once. A step of
    /// the history either way, the index taking it in at once. Returns what was done.
    ///
    /// # Errors
    /// The active layer cannot be written, the edit does not apply, or files cannot be written.
    pub fn edit(&mut self, request: &EditRequest) -> anyhow::Result<String> {
        use project::edit;
        if let Some(why) = self.read_only() {
            anyhow::bail!(why);
        }
        if let EditRequest::Refused(why) = request {
            anyhow::bail!("{why}");
        }
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
            // Only the list is written: its own, whether it is the active layer or not.
            EditRequest::Forget(layer, _) => {
                let chosen = self.layers.iter().find(|l| l.name == *layer);
                if !chosen.with_context(|| format!("the project has no layer {layer}"))?.writable {
                    anyhow::bail!("{layer}'s list cannot be written");
                }
                layer.clone()
            }
            // What a layer holds unlisted: the list written, or its folder; that layer's, whichever is active.
            EditRequest::Adopt(layer, path) | EditRequest::RemoveUnlisted(layer, path) => {
                let chosen = self.layers.iter().find(|l| l.name == *layer);
                let chosen = chosen.with_context(|| format!("the project has no layer {layer}"))?;
                let removing = matches!(request, EditRequest::RemoveUnlisted(..));
                if !chosen.writable || (removing && chosen.read_only) {
                    anyhow::bail!("{layer} cannot be written");
                }
                let (project, index, history) = (&self.project, &self.index, &mut self.history);
                if removing {
                    edit::remove_unlisted(project, index, layer, path, history)?;
                } else {
                    edit::adopt(project, index, layer, path, history)?;
                }
                let label = self.history.done().last().map_or_default(|s| s.label.clone());
                // What it wrote, taken in ahead of what other programs changed.
                self.take_written();
                return Ok(label);
            }
            _ => self.editable_layer().map_err(anyhow::Error::msg)?.name.clone(),
        };
        let uid = match request {
            EditRequest::Override(uid)
            | EditRequest::OverrideIn(uid, _)
            | EditRequest::Rename(uid, _)
            | EditRequest::RenameTo(uid, _)
            | EditRequest::Delete(uid)
            | EditRequest::Forget(_, uid)
            | EditRequest::Resolve(uid, _)
            | EditRequest::Write { uid, .. } => uid.clone(),
            EditRequest::ShowGraph
            | EditRequest::Refused(_)
            | EditRequest::Adopt(..)
            | EditRequest::RemoveUnlisted(..) => {
                return Ok(String::new());
            }
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
            EditRequest::Forget(_, uid) => edit::forget(project, index, &name, uid, history)?,
            EditRequest::RenameTo(uid, to) => edit::rename(project, index, &name, uid, to, history)?,
            EditRequest::Resolve(uid, resolution) => edit::resolve(project, index, &name, uid, *resolution, history)?,
            // An edit of a document: its working copy changes, saving writes it.
            EditRequest::Write { uid, file, bytes, label, merge } => {
                let path = index.by_uid.get(uid).cloned().unwrap_or_default();
                if !history.edit(index, uid, &path, file, bytes, label.clone(), merge.clone())? {
                    return Ok(String::new());
                }
                self.history.note_selection(self.selected.clone());
                self.send_working();
                return Ok(label.clone());
            }
            EditRequest::Rename(..)
            | EditRequest::ShowGraph
            | EditRequest::Refused(_)
            | EditRequest::Adopt(..)
            | EditRequest::RemoveUnlisted(..) => return Ok(String::new()),
        };
        self.history.note_selection(self.selected.clone());
        let label = self.history.done().last().map_or_default(|s| s.label.clone());
        // What it wrote, taken in ahead of what other programs changed.
        self.take_written();
        Ok(label)
    }

    /// The stack's own layers (not the mounts), bottom to top.
    pub fn stack_order(&self) -> Vec<String> {
        self.layers.iter().filter(|l| !l.mounted).map(|l| l.name.clone()).collect()
    }

    /// Put the stack's own layers in this order (bottom to top), if it keeps what each needs: written to
    /// `project.toml`, which the watcher sees and the project is read again.
    ///
    /// # Errors
    /// Why the order cannot be: a layer it would put below one it needs, or `project.toml` not written.
    pub fn reorder(&mut self, order: &[String]) -> Result<()> {
        project::stack::reorder(&self.project, &self.layers, order, &mut self.history)
    }

    /// Add a new, empty layer of a type at the top of the stack.
    ///
    /// # Errors
    /// The name is not one a layer may take, or it cannot be written.
    pub fn add_layer(&mut self, name: &str, kind: project::LayerKind) -> Result<u64> {
        project::stack::add(&self.project, name, kind, &mut self.history)?;
        Ok(self.take_written().unwrap_or_else(|| self.takein.issued()))
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
    /// Another layer needs it, a generated layer fills it, it is a mount, or `project.toml` cannot be rewritten.
    pub fn remove_layer(&mut self, name: &str) -> Result<()> {
        project::stack::remove(&self.project, &self.layers, name, &mut self.history)
    }

    /// Run an extension's tool for a command, with no arguments; what it answers, briefly.
    ///
    /// # Errors
    /// It is gone, or it fails.
    pub fn run_tool(&self, name: &str) -> std::result::Result<String, String> {
        let tool = self
            .extensions
            .tools()
            .iter()
            .find(|t| t.name.to_string() == name)
            .ok_or_else(|| format!("no tool {name}"))?;
        let lens = Lens {
            index: &self.index,
            extensions: &self.extensions,
            chosen: Some(&self.chosen),
            selection: self.selected.as_deref(),
            revision: self.revision,
        };
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

    /// A document tab's asset, played: under the player's controls, its type's view where it has one, told where it
    /// plays and asking the player to play or go elsewhere.
    pub fn play(
        &mut self,
        ui: &mut egui::Ui,
        pane: crate::panes::Pane,
        path: &str,
        (view, nav): (&mut crate::settings::Preview, &mut Option<String>),
    ) {
        self.played.insert(pane);
        let (extensions, index) = (self.extensions.clone(), self.index.clone());
        let mut player = self.players.remove(&pane).unwrap_or_else(|| crate::player::Player::new(extensions.clone()));
        let look = self.chosen.get(&(path.to_string(), project::ext::PICTURE.to_string())).cloned();
        let make = index.assets.get(path).and_then(|asset| extensions.views(asset).first().map(|def| def.make.clone()));
        let preview = *view;
        match make {
            Some(make) => {
                let mut body = |ui: &mut egui::Ui, playing: Option<pe2_ext::Playing>| {
                    self.view_playback = playing;
                    let key = (egui::Id::unique(("played", pane)), path.to_string(), 0);
                    self.draw_view(ui, key, &*make, (preview, 1), nav, None);
                    self.view_playback = None;
                    std::mem::take(&mut self.view_transport)
                };
                player.ui(ui, (&extensions, &index), (path, look.as_deref()), view, Some(&mut body));
            }
            None => player.ui(ui, (&extensions, &index), (path, look.as_deref()), view, None),
        }
        self.players.insert(pane, player);
    }

    /// Once every window is drawn: a tab gone takes its player, and one not shown pauses.
    pub fn settle_players(&mut self, open: impl Fn(crate::panes::Pane) -> bool) {
        let played = std::mem::take(&mut self.played);
        self.players.retain(|pane, _| open(*pane));
        for (_, player) in self.players.iter_mut().filter(|(pane, _)| !played.contains(*pane)) {
            player.pause();
        }
    }

    /// The edits that make a file the source of an asset, by its type's recipe: the file in its bundle, and its stack.
    ///
    /// # Errors
    /// Nothing makes this file of this asset, or it cannot be read.
    pub fn replacing(&self, asset: &Asset, file: &Path) -> std::result::Result<Vec<EditRequest>, String> {
        let name = file.file_name().and_then(|n| n.to_str()).ok_or("It has no name")?;
        let stack = self.extensions.stack_for(asset, name).ok_or("Nothing imports it over this")??;
        let uid = asset.uid.clone().ok_or("It has no uid")?;
        let declaration = project::graph::layers::with_new_stack(asset.text(), &stack).map_err(|e| format!("{e:#}"))?;
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
        Ok(vec![write(name, bytes), write(project::index::NODE_FILE, declaration.into_bytes())])
    }

    /// Whether an entry opens as a document: an asset, a container folder among them, or a plain file; a filesystem
    /// folder does not.
    #[must_use]
    pub fn is_document(&self, path: &str) -> bool {
        self.index.assets.contains_key(path)
            || self.entry_at(path).is_some_and(|(_, e)| e.kind == project::index::Kind::File)
    }

    /// What is inside an asset, as its type says.
    pub fn contents_of(&mut self, asset: &Asset) -> Arc<pe2_ext::Contents> {
        let (extensions, index, chosen) = (&self.extensions, &self.index, &self.chosen);
        self.contents
            .entry(asset.path.clone())
            .or_insert_with(|| Arc::new(extensions.contents(index, asset, chosen)))
            .clone()
    }

    /// A part's sheet, where its asset's type says what it holds.
    pub fn part_sheet_of(&mut self, asset: &Asset, part: &str) -> Arc<Option<pe2_ext::PartSheet>> {
        let (extensions, index, chosen) = (&self.extensions, &self.index, &self.chosen);
        let key = (asset.path.clone(), part.to_string());
        self.part_sheets
            .entry(key)
            .or_insert_with(|| Arc::new(extensions.part_sheet(index, asset, chosen, part)))
            .clone()
    }

    /// Look at an asset in a member of a family: all that shows it is made again.
    pub fn choose_member(&mut self, path: &str, family: String, member: String) {
        self.chosen.insert((path.to_string(), family), member);
        self.contents.remove(path);
        self.part_sheets.retain(|(asset, _), _| asset != path);
        self.views.retain(|(_, asset, _), _| asset != path);
        self.inspector_views.retain(|(asset, _), _| asset != path);
        self.textures.live = None;
        self.drop_lost_selection();
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
        let part = self.extensions.addresses(&asset).into_iter().map(|(address, _)| address).find(|id| is_it(id));
        let contents = self.contents_of(&asset);
        let row =
            || contents.rows.iter().filter(|row| !row.is_heading()).map(|row| row.id.clone()).find(|id| is_it(id));
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
        self.reveal = Some(path.clone());
        self.selected = Some(path);
    }

    /// Choose a virtual entry: its image selected and shown in the entry's variant; choosing the entry shown again
    /// shows the image in its first.
    pub fn choose(&mut self, entry: &Entry) {
        if self.is_chosen(entry) {
            self.chosen.remove(&look_of(&entry.image));
            return;
        }
        if self.selected.as_deref() != Some(&entry.image) {
            self.select(entry.image.clone());
        }
        self.show_look(&entry.image, entry.look.clone());
    }

    /// Whether a virtual entry is what is shown, chosen.
    pub fn is_chosen(&self, entry: &Entry) -> bool {
        self.selected.as_deref() == Some(&entry.image) && self.chosen.get(&look_of(&entry.image)) == Some(&entry.look)
    }

    /// Select an asset picked in the tree: like `navigate`, but the tree does not move. Picking the selected image
    /// again shows it in its first variant.
    pub fn select(&mut self, path: String) {
        if self.selected.as_deref() == Some(&path) {
            self.chosen.remove(&look_of(&path));
            return;
        }
        self.navigate(path);
        self.reveal = None;
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
        let chosen = self.chosen.get(&look_of(path)).map(String::as_str);
        self.index.assets.get(path).map_or(0, |asset| self.extensions.look_at(asset, chosen))
    }

    /// Show an image in a look. Nothing need be made again: what shows it asks for its look each time.
    pub fn show_look(&mut self, path: &str, look: String) {
        self.chosen.insert(look_of(path), look);
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

    /// Whether the preview shows an asset: by its picture, or - its type drawing none - by its type's views.
    pub fn has_view(&self, path: &str) -> bool {
        self.index.assets.get(path).is_some_and(|a| {
            self.extensions.has_picture(a)
                || (!self.extensions.shows_pictures(a) && !self.extensions.views(a).is_empty())
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

    /// Show an asset's view in a pane - the one chosen, where its type has several - made when first shown.
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
        self.draw_view(ui, (pane, path.to_string(), chosen), &*make, (preview, count), nav, None);
    }

    /// Show an editor an extension gives an asset, made when first shown.
    pub fn show_editor(
        &mut self,
        ui: &mut egui::Ui,
        pane: egui::Id,
        path: &str,
        name: &str,
        preview: Preview,
        nav: &mut Option<String>,
    ) {
        let Some(asset) = self.index.assets.get(path) else { return };
        // After its views, in the numbers the pane keeps views by.
        let views = self.extensions.views(asset).len();
        let editors = self.extensions.editors(asset);
        let shown = self.extensions.previews(asset);
        let viewer = shown
            .iter()
            .enumerate()
            .find_map(|(at, shown)| Some((at, shown.viewer.filter(|def| def.name == name)?, &shown.output)));
        if let Some((at, def, output)) = viewer {
            let (at, make, output) = (views + editors.len() + at, def.make.clone(), output.clone());
            self.draw_view(ui, (pane, path.to_string(), at), &*make, (preview, 1), nav, Some(&output));
            return;
        }
        let Some((at, def)) = editors.iter().enumerate().find(|(_, def)| def.name == name) else { return };
        let (at, make) = (views + at, def.make.clone());
        self.draw_view(ui, (pane, path.to_string(), at), &*make, (preview, 1), nav, None);
    }

    /// Show a preview an asset's stack asks for: by a viewer of what it is, else - a picture - as the host draws one.
    pub fn show_tap(
        &mut self,
        ui: &mut egui::Ui,
        pane: egui::Id,
        path: Option<&str>,
        output: &str,
        preview: Preview,
        nav: &mut Option<String>,
    ) {
        let Some(asset) = path.and_then(|path| self.index.assets.get(path)) else { return };
        let path = asset.path.clone();
        let path = path.as_str();
        let views = self.extensions.views(asset).len() + self.extensions.editors(asset).len();
        let shown = self.extensions.previews(asset);
        let found = shown.iter().enumerate().find(|(_, shown)| shown.tapped && shown.output == output);
        let Some((at, shown)) = found else { return };
        if let Some(viewer) = shown.viewer {
            let make = viewer.make.clone();
            self.draw_view(ui, (pane, path.to_string(), views + at), &*make, (preview, 1), nav, Some(output));
            return;
        }
        let key = (path.to_string(), output.to_string(), self.revision);
        if self.tapped.as_ref().is_none_or(|(known, _)| *known != key) {
            let made = self.extensions.output_picture(&self.index, asset, output);
            let made = made.map(|image| {
                let color = egui::ColorImage::from_rgba_unmultiplied([image.width, image.height], &image.rgba);
                ui.ctx().load_texture(format!("tap {path} {output}"), color, egui::TextureOptions::NEAREST)
            });
            self.tapped = Some((key, made.map_err(|e| format!("{e:#}"))));
        }
        match self.tapped.as_ref().map(|(_, made)| made) {
            Some(Ok(texture)) => {
                ui.centered_and_justified(|ui| {
                    ui.add(egui::Image::new(texture).max_size(ui.available_size()).shrink_to_fit())
                });
            }
            Some(Err(why)) => {
                ui.label(why.as_str());
            }
            None => {}
        }
    }

    /// Show a view of an asset in a pane, made when first shown.
    fn draw_view(
        &mut self,
        ui: &mut egui::Ui,
        key: (egui::Id, String, usize),
        make: &pe2_ext::MakeView,
        (preview, count): (Preview, usize),
        nav: &mut Option<String>,
        shown: Option<&str>,
    ) {
        let path = key.1.clone();
        // As the inspector's own fields: an asset from below is overridden into the active layer.
        let editable = self.editable_layer().is_ok();
        let part = self.part_of(&path).map(str::to_string);
        let Some(asset) = self.index.assets.get(&path) else { return };
        let view = self.views.entry(key).or_insert_with(|| make(&self.extensions.lend(asset)));
        let lens = Lens {
            index: &self.index,
            extensions: &self.extensions,
            selection: self.selected.as_deref(),
            chosen: Some(&self.chosen),
            revision: self.revision,
        };
        let playback = self.view_playback;
        let frame = crate::ext_ui::Frame {
            asset: Some(asset),
            preview,
            count,
            editable,
            part: part.as_deref(),
            shown,
            playback,
        };
        let mut sets = crate::ext_ui::Sets::default();
        let mut host = crate::ext_ui::Host::new(ui, &lens, &mut self.textures, nav, &mut sets, frame);
        view.show(&mut pe2_ext::Ui::new(&mut host));
        drop(host);
        if !sets.values.is_empty() {
            self.view_edit = crate::inspector::set_fields(asset, &sets);
        }
        if let Some((group, member)) = sets.choose {
            self.choose_member(&path, group, member);
        }
        self.view_transport.extend(sets.transport);
    }

    /// What a gesture in a view has made so far of a part's fields, as yet unwritten.
    pub fn live_fields(&self, asset: &str, part: &str) -> Vec<(String, toml::Value)> {
        let live = self.textures.live.as_ref().filter(|live| live.asset == asset && live.part == part);
        live.map_or_else(Vec::new, |live| live.fields.clone())
    }

    /// Show an asset's picture in its look on the host's canvas, with what its type marks on it over it.
    fn show_picture(
        &mut self,
        ui: &mut egui::Ui,
        path: &str,
        preview: Preview,
        count: usize,
        nav: &mut Option<String>,
    ) {
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
            boxes: marks
                .boxes
                .iter()
                .map(|m| (m.name.clone(), crate::ext_ui::to_rect(m.rect), selected.as_ref() == Some(&m.name)))
                .collect(),
            summary: marks.summary.clone(),
            toggle: marks.toggle.clone(),
            legend: marks.legend,
            grid: None,
            traces: Vec::new(),
        };
        if let Some(link) = crate::panes::overlay_ui(ui, &overlay, preview, count) {
            *nav = Some(link);
        }
    }

    /// Show an asset's inspector views - what its type draws of it after the property sheet - made when first
    /// shown: those of a tab, or those with none.
    pub fn show_inspector_views(
        &mut self,
        ui: &mut egui::Ui,
        path: &str,
        tab: Option<usize>,
        nav: &mut Option<String>,
    ) {
        self.show_lent_views(ui, path, nav, |extensions, asset| {
            let shown = extensions.inspector(asset).into_iter().enumerate();
            shown.filter(|(i, def)| tab.map_or(def.tab.is_none(), |tab| tab == *i)).collect()
        });
    }

    /// Show the views a type gives the head of the contents pane, made when first shown.
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
        // As the inspector's own fields: an asset from below is overridden into the active layer.
        let editable = self.editable_layer().is_ok();
        let part = self.part_of(path).map(str::to_string);
        let Some(asset) = self.index.assets.get(path) else { return };
        let lens = Lens {
            index: &self.index,
            extensions: &self.extensions,
            selection: self.selected.as_deref(),
            chosen: Some(&self.chosen),
            revision: self.revision,
        };
        for (i, def) in chosen(&self.extensions, asset) {
            let key = (path.to_string(), i);
            let view = self.inspector_views.entry(key).or_insert_with(|| (def.make)(&self.extensions.lend(asset)));
            let frame = crate::ext_ui::Frame {
                asset: Some(asset),
                shown: None,
                preview: Preview::default(),
                count: 1,
                editable,
                part: part.as_deref(),
                playback: None,
            };
            let mut sets = crate::ext_ui::Sets::default();
            let mut host = crate::ext_ui::Host::new(ui, &lens, &mut self.textures, nav, &mut sets, frame);
            view.show(&mut pe2_ext::Ui::new(&mut host));
            drop(host);
            if !sets.values.is_empty() {
                self.view_edit = crate::inspector::set_fields(asset, &sets);
            }
        }
    }

    /// Show a panel in a pane, its view made when the pane first shows it.
    pub fn show_panel(
        &mut self,
        ui: &mut egui::Ui,
        pane: egui::Id,
        name: &str,
        preview: Preview,
        nav: &mut Option<String>,
    ) {
        let Some(panel) = self.extensions.panel(name) else {
            ui.label(egui::RichText::new(format!("No extension provides {name}")).color(crate::theme::weak()));
            return;
        };
        let view = self.panels.entry(pane).or_insert_with(|| (panel.make)());
        let lens = Lens {
            index: &self.index,
            extensions: &self.extensions,
            selection: self.selected.as_deref(),
            chosen: Some(&self.chosen),
            revision: self.revision,
        };
        // A panel's canvas is not a picture's: no texel grid over it.
        let preview = Preview { grid: false, ..preview };
        let frame = crate::ext_ui::Frame {
            asset: None,
            preview,
            count: 1,
            editable: false,
            part: None,
            shown: None,
            playback: None,
        };
        let mut sets = crate::ext_ui::Sets::default();
        let mut host = crate::ext_ui::Host::new(ui, &lens, &mut self.textures, nav, &mut sets, frame);
        view.show(&mut pe2_ext::Ui::new(&mut host));
    }
}

/// What changed, and everything that refers to it, however far off.
fn reached_from(index: &project::index::Index, changed: &BTreeSet<String>) -> BTreeSet<String> {
    let mut reached = changed.clone();
    let mut waiting: Vec<&str> = changed.iter().map(String::as_str).collect();
    while let Some(path) = waiting.pop() {
        for reference in index.referenced_by.get(path).into_iter().flatten() {
            if reached.insert(reference.from.clone()) {
                waiting.push(&reference.from);
            }
        }
    }
    reached
}
