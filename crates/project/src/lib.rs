//! Projects: what a `project.toml` declares - its layers, among them generated ones, each filled by its plan (`plan`)
//! from the game's discs - and what is made of it.

/// A path of the project's tree: what the index resolves, the layers list and the served folder shows (`assets/`).
pub type TreePath = pe2_path::VfsPath<pe2_path::Tree>;

pub mod build;
pub mod cache;
pub mod changeset;
pub mod chunks;
pub mod content;
pub mod discs;
pub mod dump;
pub mod edit;
pub mod events;
pub mod ext;
pub mod extract;
pub mod graph;
pub mod history;
pub mod host;
pub mod imports;
pub mod index;
pub mod lock;
pub mod merge;
pub mod model;
pub mod pack;
pub mod places;
pub mod plan;
pub mod profile;
pub mod records;
pub mod role;
pub mod schedule;
pub mod server;
pub mod setup;
pub mod shared;
pub mod stack;
pub mod storage;
pub mod switches;
pub mod uid;
pub mod wire;

/// Give the memory freed lately back to the system, where the allocator keeps it: what holds a project for long (the
/// host, the editor) calls it once a refresh is done. glibc keeps what a thread freed in that thread's arena, and work
/// spread over every core leaves each with its own: a long-lived process grows by what its refreshes freed, up to its
/// arenas' count. Capping them would cost the parallel work far more.
pub fn give_back_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        unsafe extern "C" {
            fn malloc_trim(pad: usize) -> std::ffi::c_int;
        }
        // SAFETY: `malloc_trim` only releases memory glibc's allocator holds unused; it takes no pointer of ours.
        unsafe {
            malloc_trim(0);
        }
    }
}

use anyhow::{Context, Result, bail};
use pe2_path::Name;
use serde::Deserialize;
use sfs::HostPath;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

pub const FILE: &Name = Name::constant("project.toml");
/// Where a project's layers live, each a directory with its changeset beside it.
pub const LAYERS: &Name = Name::constant(".layers");
/// What follows a layer's folder's name in its changeset's.
pub const CHANGESET_SUFFIX: &Name = Name::constant(".toml");
/// What follows a generated layer's folder's name in its plan's.
pub const PLAN_SUFFIX: &Name = Name::constant(".plan.jsonl");

#[derive(Deserialize)]
struct ProjectFile {
    #[serde(default)]
    layer: Vec<LayerFile>,
    #[serde(default)]
    mount: Vec<MountFile>,
    #[serde(default)]
    extensions: ExtensionsFile,
    /// What it requires of extensions: `[[extension]]`.
    #[serde(default)]
    extension: Vec<Requirement>,
    /// The builds it can be: `[config.<name>]`.
    #[serde(default)]
    config: std::collections::BTreeMap<String, ConfigFile>,
    /// What its configs say beyond their release and target: `[property.<name>]`, in order.
    #[serde(default)]
    property: toml::Table,
}

/// A build the project can be: `[config.<name>]`.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// Its name, which names its folder of what is built (`build/<name>`).
    pub name: Arc<Name>,
    pub release: Option<String>,
    /// The target building it, what its `target.<name>` sections are for.
    pub target: Option<String>,
    /// The assets left out of it, by path: no event reaches them, and looking for a type's assets does not find them.
    pub disabled: Box<[Arc<TreePath>]>,
    /// The properties it sets (`[config.<name>.properties]`), as written: `Project::properties_of` gives them over
    /// what the project declares.
    pub properties: toml::Table,
}

/// What a project's configs can say beyond their release and target: `[property.<name>]`, with its shape and the value
/// a config not setting it has.
#[derive(Clone, Debug, PartialEq)]
pub struct Property {
    pub name: String,
    pub kind: PropertyKind,
    /// What a config not setting it has: none where every config has to.
    pub default: Option<toml::Value>,
    pub label: Option<String>,
    pub description: Option<String>,
}

/// The shape of a property's value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PropertyKind {
    /// A line of text (`text`).
    Text,
    /// A list of them (`texts`): compiler flags, say.
    Texts,
    Integer,
    Boolean,
    /// A table of anything (`table`).
    Table,
    /// Anything (`any`).
    Any,
    /// One of these (`values = [...]`).
    OneOf(Vec<String>),
}

impl PropertyKind {
    /// What a value of it is not, if anything.
    #[must_use]
    pub fn refuses(&self, value: &toml::Value) -> Option<String> {
        use toml::Value as V;
        let fits = match (self, value) {
            (PropertyKind::Text, V::String(_))
            | (PropertyKind::Integer, V::Integer(_))
            | (PropertyKind::Boolean, V::Boolean(_))
            | (PropertyKind::Table, V::Table(_))
            | (PropertyKind::Any, _) => true,
            (PropertyKind::Texts, V::Array(items)) => items.iter().all(toml::Value::is_str),
            (PropertyKind::OneOf(values), V::String(s)) => {
                return (!values.contains(s)).then(|| format!("{s:?} is none of {}", values.join(", ")));
            }
            _ => false,
        };
        (!fits).then(|| format!("{value} is no {self}"))
    }
}

impl std::fmt::Display for PropertyKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PropertyKind::Text => f.write_str("text"),
            PropertyKind::Texts => f.write_str("list of text"),
            PropertyKind::Integer => f.write_str("integer"),
            PropertyKind::Boolean => f.write_str("boolean"),
            PropertyKind::Table => f.write_str("table"),
            PropertyKind::Any => f.write_str("value"),
            PropertyKind::OneOf(values) => write!(f, "one of {}", values.join(", ")),
        }
    }
}

/// What is wrong with a config's property, or with a property's declaration (no config).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PropertyProblem {
    pub config: Option<Arc<Name>>,
    pub property: String,
    pub why: String,
}

/// `[property.<name>]`: its shape (`type`, or `values`), its default, and what it is called.
pub(crate) fn property_of(name: &str, value: &toml::Value) -> std::result::Result<Property, String> {
    let toml::Value::Table(table) = value else {
        return Err("is no table".into());
    };
    let text = |key: &str| table.get(key).and_then(toml::Value::as_str).map(str::to_owned);
    let kind = match (table.get("values"), text("type").as_deref()) {
        (Some(toml::Value::Array(values)), None | Some("one of")) => {
            let values: Option<Vec<String>> = values.iter().map(|v| v.as_str().map(str::to_owned)).collect();
            PropertyKind::OneOf(values.ok_or("its values are not all text")?)
        }
        (Some(_), _) => return Err("its values are no list of text, or it has a type too".into()),
        (None, Some("text")) => PropertyKind::Text,
        (None, Some("texts")) => PropertyKind::Texts,
        (None, Some("integer")) => PropertyKind::Integer,
        (None, Some("boolean")) => PropertyKind::Boolean,
        (None, Some("table")) => PropertyKind::Table,
        (None, Some("any") | None) => PropertyKind::Any,
        (None, Some(other)) => {
            return Err(format!("no type {other} (text, texts, integer, boolean, table, any, or values)"));
        }
    };
    Ok(Property {
        name: name.to_owned(),
        kind,
        default: table.get("default").cloned(),
        label: text("label"),
        description: text("description"),
    })
}

/// `[config.<name>]`: a release, and the target that builds it.
#[derive(Deserialize)]
struct ConfigFile {
    #[serde(default)]
    release: Option<String>,
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    disabled: Vec<String>,
    #[serde(default)]
    properties: toml::Table,
}

/// `[build]` of `project.local.toml`: where this machine also wants a build's results.
#[derive(Default, Deserialize)]
struct BuildFile {
    /// `[build.links]`: a link, relative to the project, for a deployed file - where a language server looks for it,
    /// where the project is not the root of what it is edited in.
    #[serde(default)]
    links: build::Links,
}

/// An extension a project requires: `[[extension]]`. Without it, of a version it accepts, the project is not whole. One
/// it holds (`ext::folder`) or a built-in.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Requirement {
    pub id: String,
    /// The versions it accepts: `^0.1`; none, any.
    #[serde(default)]
    pub version: Option<String>,
}

/// `[extensions]`: the extensions the project leaves off.
#[derive(Default, Deserialize)]
struct ExtensionsFile {
    #[serde(default)]
    disabled: Vec<String>,
    /// `[extensions."<id>"]`: an extension's project settings (what is not a table is no extension's).
    #[serde(flatten)]
    settings: std::collections::BTreeMap<String, toml::Value>,
}

#[derive(Deserialize)]
struct LayerFile {
    name: String,
    /// The layer's directory; by default `.layers/<name>`.
    path: Option<String>,
    #[serde(default)]
    read_only: bool,
    /// `files` for a layer of plain files only (code), `generated` for one its plan fills; an asset layer by default.
    kind: Option<String>,
    /// The layer whose nodes a generated layer gives their files.
    fills: Option<String>,
    /// How its content is stored: `text`, its files as files (by default), or `packed`, one listing of them with their
    /// chunks in the store (`pack`) - a generated layer's, for now.
    storage: Option<String>,
}

#[derive(Deserialize)]
struct MountFile {
    at: String,
    /// The tree; an unmanaged mount's is this machine's (`project.local.toml`), else the folder of its name.
    path: Option<String>,
    #[serde(default)]
    read_only: bool,
    /// Not indexed: a place files are read from by path (`res://<at>/...`) - the disc images - and nothing else.
    #[serde(default)]
    unmanaged: bool,
}

/// `project.local.toml`: what of a project is this machine's - where its unmanaged mounts are (`[mounts] <at> =
/// "<path>"`), the links it wants to what builds deploy (`[build.links]`), and how it serves the project as a
/// filesystem (`[filesystem]`). It is kept out of the project's history and of version control.
#[derive(Default, Deserialize)]
struct LocalFile {
    #[serde(default)]
    mounts: BTreeMap<String, String>,
    #[serde(default)]
    build: BuildFile,
    #[serde(default)]
    filesystem: FilesystemFile,
}

/// `[filesystem]` of `project.local.toml`: what serves the project's merged layers at its folder while it is open -
/// in the editor, or with `pe2-cli project serve` - and what serves the files programs are given (`scopes`): each
/// `auto`, `none` or a backend's name; `address` where the backend serves at one, whether its host serves on with
/// nobody keeping it (`keep`), and two budgets of bytes: what laying files out for a program may write (`copies`)
/// and what may be kept of what was read of the folder (`kept`).
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FilesystemFile {
    serve: Option<String>,
    scopes: Option<String>,
    address: Option<String>,
    keep: Option<bool>,
    copies: Option<String>,
    kept: Option<String>,
    pages: Option<String>,
    cache: Option<String>,
}

/// The folder a project is served at, in its root, for every program alike: its merged layers as one tree (`vfs`).
pub const SERVED: &Name = Name::constant("assets");
/// Where what that folder held that was not the project's is set aside before it is served (`vfs::strays`), a folder a
/// time: the only copy of it, so not in the cache.
pub const SET_ASIDE: &Name = Name::constant("assets.set-aside");

/// How this machine serves a project as a filesystem, as far as the project says (`project.local.toml`): what serves
/// is chosen where it is served (`vfs::backend`), which knows the backends - a name is not judged here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filesystem {
    /// The folder it is served at.
    pub at: HostPath,
    /// What serves it there (`[filesystem] serve`), where the project says.
    pub serve: Option<String>,
    /// What serves the files programs are given (`[filesystem] scopes`), where the project says.
    pub scopes: Option<String>,
    /// The address it is served at, by a backend serving at one (`[filesystem] address`).
    pub address: Option<String>,
    /// Whether its host serves on with nobody keeping it, until stopped (`[filesystem] keep`): for a folder that is
    /// always there, while the machine runs, once anything has started serving it.
    pub keep: bool,
    /// How much laying files out for a program may write (`[filesystem] copies`, as `16 MiB`): up to that it is
    /// given copies, beyond it they are served where they are.
    pub copies: Option<String>,
    /// How much a backend that keeps what is read may keep of the project's folder (`[filesystem] kept`, as `2 GiB`).
    pub kept: Option<String>,
    /// How much memory the pages kept of what was read of its files may take (`[filesystem] pages`, as `256 MiB`).
    pub pages: Option<String>,
    /// Where a backend that keeps the folder's content apart from it keeps it (`[filesystem] cache`, a folder).
    pub cache: Option<String>,
}

impl Filesystem {
    fn of(root: &HostPath, file: FilesystemFile) -> Filesystem {
        Filesystem {
            at: root.join(&SERVED),
            serve: file.serve,
            scopes: file.scopes,
            address: file.address,
            keep: file.keep.unwrap_or(false),
            copies: file.copies,
            kept: file.kept,
            pages: file.pages,
            cache: file.cache,
        }
    }
}

/// How this machine serves the project at a folder, read from its `project.local.toml` alone: for what serves a
/// project's files without loading it. One that cannot be read says nothing.
#[must_use]
pub fn local_filesystem(root: &HostPath, fs: &dyn pe2_io::Filesystem) -> Filesystem {
    Filesystem::of(root, read_local(fs, root).map_or_default(|local| local.filesystem))
}

fn read_local(fs: &dyn pe2_io::Filesystem, root: &HostPath) -> Result<LocalFile> {
    let path = root.join(&LOCAL);
    match fs.read_to_string(&path) {
        Ok(text) => toml::from_str(&text).with_context(|| format!("{}", path.display())),
        Err(_) => Ok(LocalFile::default()),
    }
}

/// The file what of a project is this machine's is kept in.
pub const LOCAL: &Name = Name::constant("project.local.toml");

/// What a layer may hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayerKind {
    /// Assets and plain files: a project's nodes, a mod's assets, the project's own.
    Assets,
    /// Plain files only - code. A mount is one whose content is an external tree left where it is.
    Files,
    /// What a plan writes (`plan`): the files of the nodes of the layer it fills, at their paths. Read-only, its plan
    /// beside its folder (`Layer::plan`).
    Generated,
}

pub(crate) use schedule::parallel;

#[derive(Clone, PartialEq)]
pub struct Layer {
    /// Its name, which names its folder in `.layers/` and its changeset.
    pub name: Arc<Name>,
    pub kind: LayerKind,
    /// Its content's directory: in `.layers/`, or a mounted tree.
    pub path: HostPath,
    /// Where its content appears in the resolved project: the root for a layer at the top, the mount point for a mount.
    pub at: Arc<TreePath>,
    /// Its changeset, in `.layers/`: `<path>.toml` beside a layer's directory, `.layers/<name>.toml` for a mount.
    pub changeset: HostPath,
    /// Its content is written only by its maker: stated in `project.toml`, or a generated layer (its plan's alone).
    pub read_only: bool,
    /// Whether its changeset is the project's to write: a mount's is, though its tree may be read-only.
    pub changeset_writable: bool,
    /// A mount, which `project.toml` declares apart (`[[mount]]`) until mounts move into the stack.
    pub mounted: bool,
    /// The layer a generated layer gives its nodes' files.
    pub fills: Option<Arc<Name>>,
    /// How its content is stored.
    pub storage: Storage,
}

#[derive(Clone)]
pub struct Mount {
    pub at: Arc<TreePath>,
    pub path: HostPath,
    pub read_only: bool,
}

impl Layer {
    /// A generated layer's plan: `<path>.plan.jsonl`, beside its folder as its changeset would be, so that what fills
    /// the layer travels with the project while what it made does not.
    #[must_use]
    pub fn plan(&self) -> HostPath {
        self.path.with_suffix(PLAN_SUFFIX)
    }

    /// A resolved path's place on disk in this layer: under its directory, or, for a mount, under its tree; joined part
    /// by part, so that it is in the system's own separators.
    #[must_use]
    pub fn disk(&self, path: &TreePath) -> HostPath {
        let within = path.strip_prefix(&self.at).unwrap_or(path.rel());
        self.path.join(&within)
    }
}

#[derive(Clone)]
pub struct Project {
    pub root: HostPath,
    pub layers: Vec<Layer>,
    pub mounts: Vec<Mount>,
    /// What is mounted and not indexed: read by path alone (`resolve`) - the disc images (`roms`).
    pub unmanaged: Vec<Mount>,
    /// The extensions it leaves off, by id (`[extensions] disabled`): everyone on it reads its assets the same way.
    pub disabled_extensions: Vec<String>,
    /// The extensions it requires (`[[extension]]`).
    pub requires: Vec<Requirement>,
    /// Its extensions' settings, by id (`[extensions."<id>"]`): the same for everyone on it.
    pub extension_settings: std::collections::BTreeMap<String, toml::Table>,
    /// The builds it can be, by name, in order.
    pub configs: Vec<Config>,
    /// What its configs can say beyond their release and target, in the order declared; those declared wrongly are
    /// left out, and said (`property_problems`).
    pub properties: Vec<Property>,
    /// What is wrong with the properties' declarations and the configs' values of them.
    pub property_problems: Vec<PropertyProblem>,
    /// Links this machine wants to what is deployed (`project.local.toml`): where each goes, relative to the project,
    /// by the deployed file it stands for.
    pub links: build::Links,
    /// How this machine serves the project as a filesystem while it is open, at its `assets` folder
    /// (`project.local.toml`).
    pub filesystem: Filesystem,
    /// The I/O what works on the project reads the outside world through: the system's, but where a test gives
    /// another (`load_with`).
    pub io: Arc<dyn pe2_io::Io>,
}

/// The configs, the properties declared and what is wrong with those.
fn configs_of(
    configs: std::collections::BTreeMap<String, ConfigFile>,
    declared: &toml::Table,
) -> Result<(Vec<Config>, Vec<Property>, Vec<PropertyProblem>)> {
    let configs: Vec<Config> = configs
        .into_iter()
        .map(|(name, c)| {
            Ok(Config {
                name: (Name::new(&name)
                    .with_context(|| format!("the config {name}, which names its build's folder"))?)
                .to_owned(),
                release: c.release,
                target: c.target,
                disabled: c
                    .disabled
                    .iter()
                    .map(|path| {
                        let path =
                            TreePath::new(path).with_context(|| format!("the config {name} leaves out {path}"))?;
                        Ok(path.to_owned())
                    })
                    .collect::<Result<_>>()?,
                properties: c.properties,
            })
        })
        .collect::<Result<_>>()?;
    let (properties, problems) = properties_of(declared, &configs);
    Ok((configs, properties, problems))
}

/// A value written as TOML alone (`fr` is none; `"fr"`, `2`, `["-O2"]` are): what a property is set to from text.
#[must_use]
pub fn toml_value(text: &str) -> Option<toml::Value> {
    format!("v = {text}").parse::<toml::Table>().ok().and_then(|mut table| table.remove("v"))
}

/// The properties declared (those declared wrongly left out) and what is wrong with them and the configs' values.
fn properties_of(declared: &toml::Table, configs: &[Config]) -> (Vec<Property>, Vec<PropertyProblem>) {
    let mut problems = Vec::new();
    let mut properties = Vec::new();
    for (name, value) in declared {
        match property_of(name, value) {
            Ok(property) => {
                if let Some(why) = property.default.as_ref().and_then(|d| property.kind.refuses(d)) {
                    problems.push(PropertyProblem {
                        config: None,
                        property: name.clone(),
                        why: format!("its default: {why}"),
                    });
                }
                properties.push(property);
            }
            Err(why) => problems.push(PropertyProblem { config: None, property: name.clone(), why }),
        }
    }
    for config in configs {
        let problem = |property: &str, why: String| PropertyProblem {
            config: Some(config.name.clone()),
            property: property.to_owned(),
            why,
        };
        for (name, value) in &config.properties {
            match properties.iter().find(|p| p.name == *name) {
                Some(property) => problems.extend(property.kind.refuses(value).map(|why| problem(name, why))),
                None if declared.contains_key(name) => {}
                None => problems.push(problem(name, "the project declares no such property".into())),
            }
        }
        for property in properties.iter().filter(|p| p.default.is_none() && !config.properties.contains_key(&p.name)) {
            problems.push(problem(&property.name, "is not set, and has no default".into()));
        }
    }
    (properties, problems)
}

/// A place `project.toml` or `project.local.toml` names: as it is where it is absolute, else within the project's folder.
pub(crate) fn resolve_path(directory: &HostPath, value: &str) -> Result<HostPath> {
    directory.resolve(Path::new(value)).with_context(|| format!("{value}: the place it names"))
}

/// A layer's or a mount point's name, as the name of its folder or changeset.
fn layer_name(name: &str) -> Result<Arc<Name>> {
    (Name::new(name).with_context(|| format!("the layer {name}"))).map(ToOwned::to_owned)
}

/// That each generated layer fills an asset layer of the project.
fn check_fills(root: &HostPath, layers: &[Layer]) -> Result<()> {
    for layer in layers {
        if let Some(filled) = &layer.fills {
            let found = layers.iter().find(|l| l.name == *filled);
            if !found.is_some_and(|l| l.kind == LayerKind::Assets) {
                bail!("{}: the layer {} fills {filled}, no asset layer of the project", root.join(&FILE), layer.name);
            }
        }
    }
    Ok(())
}

/// What a new project is: a layer of its own, and nothing else.
const EMPTY: &str = "# A project. `res://` paths are absolute from this directory.\n\n[[layer]]\nname = \"project\"\npath = \".layers/project\"\n";

/// Make an empty project in `directory`: its `project.toml`, naming a layer of its own, and that layer's folder. What
/// else a project holds - a game, its layers, extensions - is added to it, or written by whatever makes projects whole.
///
/// # Errors
/// A project is there already, or the folder cannot be written.
pub fn create(directory: &Path, fs: &dyn pe2_io::Filesystem) -> Result<()> {
    const OWN: &Name = Name::constant("project");
    const IGNORE: &Name = Name::constant(".gitignore");
    let directory = HostPath::new(directory).with_context(|| directory.display().to_string())?;
    let file = directory.join(&FILE);
    anyhow::ensure!(!fs.exists(&file), "{directory} holds a project already");
    let own = directory.join(&LAYERS).join(&OWN);
    fs.create_folder_all(&own).with_context(|| directory.to_string())?;
    // Its own layer is records (`records`).
    records::mark(fs, &own, &records::Settings::default())?;
    fs.write(&file, EMPTY.as_bytes()).with_context(|| file.to_string())?;
    // Where it is served is no part of it, nor what was set aside from there.
    let ignore = directory.join(&IGNORE);
    let mut text = fs.read_to_string(&ignore).unwrap_or_default();
    for folder in [SERVED, SET_ASIDE] {
        if !text.lines().any(|l| l.trim() == format!("/{folder}/")) {
            let separator = if text.is_empty() || text.ends_with('\n') { "" } else { "\n" };
            text = format!("{text}{separator}/{folder}/\n");
        }
    }
    fs.write(&ignore, text.as_bytes()).with_context(|| ignore.to_string())
}

/// How a layer's content is stored (`storage` in `project.toml`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Storage {
    /// Its files as files: what a person and version control read.
    #[default]
    Text,
    /// One listing of its files, their chunks in the store (`pack`).
    Packed,
}

/// How a layer says it is stored, its pack opened where it is packed.
fn storage_of(
    root: &HostPath,
    layer: &Name,
    said: Option<&str>,
    generated: bool,
    path: &HostPath,
    io: &Arc<dyn pe2_io::Io>,
) -> Result<Storage> {
    let file = root.join(&FILE);
    let storage = match said {
        None | Some("text") => Storage::Text,
        Some("packed") if generated => Storage::Packed,
        Some("packed") => bail!("{}: the layer {layer}: only a generated layer is packed, so far", file.display()),
        Some(other) => bail!("{}: the layer {layer}: no storage {other} (text, packed)", file.display()),
    };
    if storage == Storage::Packed {
        pack::Pack::open(path, io).with_context(|| format!("{}: its pack", path.display()))?;
    }
    Ok(storage)
}

/// A project's mounts, and its unmanaged ones apart, each where this machine says (`project.local.toml`).
fn mounts_of(root: &HostPath, declared: Vec<MountFile>, local: &LocalFile) -> Result<(Vec<Mount>, Vec<Mount>)> {
    let (mut mounts, mut unmanaged) = (Vec::new(), Vec::new());
    for m in declared {
        let at = TreePath::new(m.at.trim_matches('/')).with_context(|| format!("the mount {}", m.at))?;
        let path = match (m.unmanaged, local.mounts.get(at.as_str()), &m.path) {
            (true, Some(here), _) => resolve_path(root, here)?,
            (_, _, Some(path)) => resolve_path(root, path)?,
            (true, None, None) => root.join(&at),
            (false, _, None) => bail!("{}: the mount {at} says no path", root.join(&FILE)),
        };
        let mount = Mount { at: at.to_owned(), path, read_only: m.read_only || m.unmanaged };
        if m.unmanaged { unmanaged.push(mount) } else { mounts.push(mount) }
    }
    Ok((mounts, unmanaged))
}

impl Project {
    /// The project in `directory`.
    ///
    /// # Errors
    /// Its `project.toml` cannot be read or parsed.
    pub fn load(directory: &Path) -> Result<Project> {
        Project::load_with(directory, pe2_io::system())
    }

    /// As `load`, what works on it reading the outside world through `io`.
    ///
    /// # Errors
    /// As `load`.
    pub fn load_with(directory: &Path, io: Arc<dyn pe2_io::Io>) -> Result<Project> {
        let root = HostPath::new(directory).with_context(|| format!("{}", directory.display()))?;
        let path = root.join(&FILE);
        let text = io.filesystem().read_to_string(&path).with_context(|| format!("{}", path.display()))?;
        let file: ProjectFile = toml::from_str(&text).with_context(|| format!("{}", path.display()))?;
        let local = read_local(io.filesystem(), &root)?;
        let (mounts, unmanaged) = mounts_of(&root, file.mount, &local)?;
        // The mounts are files layers at the bottom of the stack, in their order, each named after its mount point.
        let mut layers: Vec<Layer> = mounts
            .iter()
            .map(|m| -> Result<Layer> {
                Ok(Layer {
                    name: layer_name(m.at.as_str())?,
                    kind: LayerKind::Files,
                    path: m.path.clone(),
                    at: m.at.clone(),
                    changeset: root.join(&LAYERS).join(&layer_name(m.at.as_str())?).with_suffix(CHANGESET_SUFFIX),
                    read_only: m.read_only,
                    changeset_writable: true,
                    mounted: true,
                    fills: None,
                    storage: Storage::Text,
                })
            })
            .collect::<Result<_>>()?;
        for l in file.layer {
            let name = layer_name(&l.name)?;
            let path = match l.path.as_deref() {
                Some(path) => resolve_path(&root, path)?,
                None => root.join(&LAYERS).join(&name),
            };
            let changeset = path.with_suffix(CHANGESET_SUFFIX);
            if layers.iter().any(|m| m.name == name) || unmanaged.iter().any(|m| m.at.parts().eq([&*name])) {
                bail!("{}: the layer {} is named as a mount point is", root.join(&FILE), l.name);
            }
            let kind = match l.kind.as_deref() {
                None | Some("assets") => LayerKind::Assets,
                Some("files") => LayerKind::Files,
                Some("generated") => LayerKind::Generated,
                Some(other) => {
                    bail!("{}: the layer {}: no kind {other} (assets, files, generated)", root.join(&FILE), l.name)
                }
            };
            let generated = kind == LayerKind::Generated;
            if generated != l.fills.is_some() {
                bail!("{}: the layer {}: a generated layer, and it alone, fills another", root.join(&FILE), l.name);
            }
            let read_only = l.read_only || generated;
            let storage = storage_of(&root, &name, l.storage.as_deref(), generated, &path, &io)?;
            let fills = l.fills.as_deref().map(layer_name).transpose()?;
            layers.push(Layer {
                name,
                kind,
                path,
                at: TreePath::root().to_owned(),
                changeset,
                read_only,
                changeset_writable: !read_only,
                mounted: false,
                fills,
                storage,
            });
        }
        check_fills(&root, &layers)?;
        let disabled_extensions = file.extensions.disabled;
        let requires = file.extension;
        let extension_settings = file
            .extensions
            .settings
            .into_iter()
            .filter_map(|(id, value)| match value {
                toml::Value::Table(table) => Some((id, table)),
                _ => None,
            })
            .collect();
        let links = local.build.links;
        let filesystem = Filesystem::of(&root, local.filesystem);
        let (configs, properties, property_problems) =
            configs_of(file.config, &file.property).with_context(|| path.to_string())?;
        Ok(Project {
            root,
            layers,
            mounts,
            unmanaged,
            disabled_extensions,
            requires,
            extension_settings,
            configs,
            properties,
            property_problems,
            links,
            filesystem,
            io,
        })
    }

    /// A config's properties: each the project declares, as the config sets it, else its default.
    #[must_use]
    pub fn properties_of(&self, config: &Config) -> toml::Table {
        self.properties
            .iter()
            .filter_map(|p| {
                let set = config.properties.get(&p.name).filter(|v| p.kind.refuses(v).is_none());
                set.or(p.default.as_ref()).map(|v| (p.name.clone(), v.clone()))
            })
            .collect()
    }

    /// The extensions it holds: those its layers' records name (`records::EXTENSIONS`), each id's from the highest
    /// layer holding it or leaving it off, as the folders their files are read at (`records::held_folder`: on no disk,
    /// read through `content`), in name order. One whose tree is not in the store is said, and left out.
    #[must_use]
    pub fn held_extensions(&self) -> Vec<HostPath> {
        let mut held: std::collections::BTreeMap<String, Option<HostPath>> = std::collections::BTreeMap::new();
        for layer in self.layers.iter().rev().filter(|layer| !layer.mounted) {
            if self.io.filesystem().is_folder(&layer.path.join(&ext::folder::HELD)) {
                eprintln!(
                    "{} holds extensions as folders, as layers held them before records: \
                     `pe2-cli project store convert --layer {}` holds them as records",
                    layer.name, layer.name
                );
            }
            let records = match records::extensions_of(self.io.filesystem(), &layer.path) {
                Ok(records) => records,
                Err(e) => {
                    eprintln!("the extensions {} holds: {e:#}", layer.name);
                    continue;
                }
            };
            for (id, tree) in records {
                if held.contains_key(&id) {
                    continue;
                }
                let folder = tree.and_then(|tree| {
                    let folder = match records::held_folder(&layer.path, &id) {
                        Ok(folder) => folder,
                        Err(why) => {
                            eprintln!("{}: {id}: {why}", layer.name);
                            return None;
                        }
                    };
                    if records::held(&self.io, &folder).is_none() {
                        eprintln!("{}: {id}: its tree {tree} is not in the store", layer.name);
                        return None;
                    }
                    Some(folder)
                });
                held.insert(id, folder);
            }
        }
        held.into_values().flatten().collect()
    }

    /// The layer an extension added to it goes in: its highest that is written.
    #[must_use]
    pub fn extensions_layer(&self) -> Option<&Layer> {
        self.layers.iter().rev().find(|layer| !layer.mounted && !layer.read_only)
    }

    /// A `res://` path's place on disk, through the mounts, unmanaged ones too (the longest mount point first).
    ///
    /// # Errors
    /// `uri` is not a `res://` path.
    pub fn resolve(&self, uri: &str) -> Result<HostPath> {
        let rel = TreePath::from_res(uri)?;
        let mut mounts: Vec<&Mount> = self.mounts.iter().chain(&self.unmanaged).collect();
        mounts.sort_by_key(|m| std::cmp::Reverse(m.at.as_str().len()));
        for m in mounts {
            if let Some(rest) = rel.strip_prefix(&m.at) {
                return Ok(m.path.join(&rest));
            }
        }
        Ok(self.root.join(&rel))
    }

    /// The generated layers, each with the plan that fills it.
    pub fn generated(&self) -> impl Iterator<Item = &Layer> {
        self.layers.iter().filter(|l| l.kind == LayerKind::Generated)
    }

    /// The layer named `name`.
    ///
    /// # Errors
    /// The project has no such layer.
    pub fn layer(&self, name: &Name) -> Result<&Layer> {
        self.layers.iter().find(|l| *l.name == *name).with_context(|| format!("the project has no layer {name}"))
    }
}

/// What tests of a project share.
#[cfg(test)]
#[allow(
    clippy::disallowed_methods,
    reason = "a test's fixtures: the machine's time, places, files and environment, which nothing tested reads"
)]
pub(crate) mod testing {
    /// The system's I/O, as every test that is given none reads and writes with.
    pub(crate) fn io() -> std::sync::Arc<dyn pe2_io::Io> {
        static IO: std::sync::LazyLock<std::sync::Arc<dyn pe2_io::Io>> = std::sync::LazyLock::new(pe2_io::system);
        IO.clone()
    }

    use sfs::HostPath;
    pub use sfs::testing::{At, scratch};
    use std::sync::Arc;

    /// A uid a test writes, checked.
    pub fn uid(text: &str) -> pe2_path::Uid {
        pe2_path::Uid::new(text).expect("a test writes a uid")
    }

    /// A path of the project's tree a test writes, checked.
    pub fn tree(text: &str) -> Arc<crate::TreePath> {
        crate::TreePath::new(text).expect("a test writes a path").to_owned()
    }

    /// A path within something a test writes, checked.
    pub fn rel(text: &str) -> Arc<pe2_path::RelPath> {
        pe2_path::RelPath::new(text).expect("a test writes a path").to_owned()
    }

    /// A name a test writes, checked.
    pub fn name(text: &str) -> Arc<pe2_path::Name> {
        pe2_path::Name::new(text).expect("a test writes a name").to_owned()
    }

    /// Have a file or a folder say it was last changed then.
    pub fn set_modified(path: &HostPath, at: std::time::SystemTime) {
        let mut options = std::fs::OpenOptions::new();
        // What changes a time is allowed to: on Windows asked for by itself, of a folder too.
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt as _;
            options.write(true).access_mode(0x0100).custom_flags(0x0200_0000);
        }
        #[cfg(not(windows))]
        if path.is_dir() {
            options.read(true);
        } else {
            options.write(true);
        }
        options.open(path).unwrap().set_modified(at).unwrap();
    }
}

#[cfg(test)]
#[allow(
    clippy::disallowed_methods,
    reason = "a test's fixtures: the machine's time, places, files and environment, which nothing tested reads"
)]
mod tests {
    #[test]
    fn an_empty_project_loads_and_is_made_once() {
        let dir = crate::testing::scratch("new-project");
        let _ = std::fs::remove_dir_all(&dir);
        super::create(&dir, &pe2_io::SystemFilesystem).unwrap();
        let project = super::Project::load(&dir).unwrap();
        assert_eq!(project.layers.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), ["project"]);
        assert!(super::create(&dir, &pe2_io::SystemFilesystem).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn configs_have_the_properties_declared_and_say_what_is_not_as_declared() {
        let dir = crate::testing::scratch("properties");
        let _ = std::fs::remove_dir_all(&dir);
        super::create(&dir, &pe2_io::SystemFilesystem).unwrap();
        let file = dir.join(&super::FILE);
        let mut text = std::fs::read_to_string(&file).unwrap();
        text.push_str(
            r#"
[property.language]
values = ["en", "fr"]
default = "en"

[property.flags]
type = "texts"
default = []

[property.level]
type = "integer"

[property.odd]
type = "colour"

[config.usa]
release = "USA"
properties = { level = 1 }

[config.usa_fr]
release = "USA"
properties = { language = "fr", level = 2, flags = ["-O2"] }

[config.wrong]
properties = { language = "de", flags = [1], nothing = true }
"#,
        );
        std::fs::write(&file, text).unwrap();
        let project = super::Project::load(&dir).unwrap();
        let names: Vec<&str> = project.properties.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["language", "flags", "level"], "in order, the one declared wrongly left out");
        let config = |name: &str| project.configs.iter().find(|c| *c.name == *name).unwrap();
        let of = |name: &str| project.properties_of(config(name));
        assert_eq!(of("usa").get("language").and_then(toml::Value::as_str), Some("en"), "by default");
        assert_eq!(of("usa_fr").get("language").and_then(toml::Value::as_str), Some("fr"));
        assert_eq!(of("wrong").get("language").and_then(toml::Value::as_str), Some("en"), "a wrong value: its default");
        assert!(!of("wrong").contains_key("level"), "no default, not set");
        let mut said: Vec<String> = project
            .property_problems
            .iter()
            .map(|p| format!("{} {}", p.config.as_ref().map_or("-", |config| config.as_str()), p.property))
            .collect();
        said.sort();
        assert_eq!(said, ["- odd", "wrong flags", "wrong language", "wrong level", "wrong nothing"]);
        let built = crate::build::build_config(&project, config("usa_fr"));
        assert!(built.known.contains(&"language=fr".to_owned()) && built.known.contains(&"language=en".to_owned()));
        assert_eq!(built.properties.get("level").and_then(toml::Value::as_integer), Some(2));
        let index = crate::index::Index::without_store(&project).unwrap();
        assert_eq!(
            index.problems.iter().filter(|p| matches!(p, crate::index::Problem::Property { .. })).count(),
            5,
            "{:?}",
            index.problems
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
