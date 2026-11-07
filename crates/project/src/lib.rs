//! Projects: what a `project.toml` declares, the declarations a project is extracted by, and
//! the extraction that fills its game layer with asset bundles.

pub mod applied;
pub mod build;
pub mod cache;
pub mod changeset;
pub mod declarations;
pub mod edit;
pub mod events;
pub mod ext;
pub mod extract;
pub mod graph;
pub mod history;
pub mod imports;
pub mod index;
pub mod lock;
pub mod profile;
pub mod schedule;
pub mod stack;
pub mod store;
pub mod switches;
pub mod uid;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const FILE: &str = "project.toml";
/// Where a project's layers live, each a directory with its changeset beside it.
pub const LAYERS: &str = ".layers";

#[derive(Deserialize)]
struct ProjectFile {
    game: Option<GameFile>,
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
    #[serde(default)]
    build: BuildFile,
}

/// A build the project can be: `[config.<name>]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub name: String,
    pub release: Option<String>,
    /// The target building it, what its `target.<name>` sections are for.
    pub target: Option<String>,
    /// The assets left out of it, by path: no event reaches them, and looking for a type's assets does not find them.
    pub disabled: Vec<String>,
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
}

/// `[build]`: where a build's results are also wanted.
#[derive(Default, Deserialize)]
struct BuildFile {
    /// `[build.links]`: a link, relative to the project, for a deployed file: where a language server finds it.
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
struct GameFile {
    layer: String,
    #[serde(default = "default_roms")]
    roms: String,
    declarations: String,
    #[serde(default)]
    releases: BTreeMap<String, ReleaseFile>,
}

fn default_roms() -> String {
    "roms".into()
}

#[derive(Deserialize)]
struct ReleaseFile {
    #[serde(default)]
    discs: Vec<DiscImage>,
}

/// A disc image a release needs: its name, size and sha1.
#[derive(Clone, Debug, Deserialize)]
pub struct DiscImage {
    pub name: String,
    pub size: u64,
    pub sha1: String,
}

#[derive(Deserialize)]
struct LayerFile {
    name: String,
    /// The layer's directory; by default `.layers/<name>`.
    path: Option<String>,
    #[serde(default)]
    read_only: bool,
    /// `files` for a layer of plain files only (code); an asset layer by default.
    kind: Option<String>,
}

#[derive(Deserialize)]
struct MountFile {
    at: String,
    path: String,
    #[serde(default)]
    read_only: bool,
}

#[derive(Clone)]
pub struct Game {
    /// The name of the layer extraction fills.
    pub layer: String,
    pub roms: PathBuf,
    /// A `res://` path.
    pub declarations: String,
    pub releases: BTreeMap<String, Vec<DiscImage>>,
}

/// What a layer may hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayerKind {
    /// Assets and plain files: the game layer, a mod's assets, the project's own.
    Assets,
    /// Plain files only - code. A mount is one whose content is an external tree left where it is.
    Files,
}

pub(crate) use schedule::parallel;

#[derive(Clone, PartialEq)]
pub struct Layer {
    pub name: String,
    pub kind: LayerKind,
    /// Its content's directory: in `.layers/`, or a mounted tree.
    pub path: PathBuf,
    /// Where its content appears in the resolved project: empty for a layer at the top, the mount point for a mount.
    pub at: String,
    /// Its changeset, in `.layers/`: `<path>.toml` beside a layer's directory, `.layers/<name>.toml` for a mount.
    pub changeset: PathBuf,
    /// Its content is written only by its maker: stated in `project.toml`, or the game layer (extraction's alone).
    pub read_only: bool,
    /// Whether its changeset is the project's to write: a mount's is, though its tree may be read-only.
    pub changeset_writable: bool,
    /// A mount, which `project.toml` declares apart (`[[mount]]`) until mounts move into the stack.
    pub mounted: bool,
}

#[derive(Clone)]
pub struct Mount {
    pub at: String,
    pub path: PathBuf,
    pub read_only: bool,
}

impl Layer {
    /// A resolved path's place on disk in this layer: under its directory, or, for a mount, under its tree.
    #[must_use]
    pub fn disk(&self, path: &str) -> PathBuf {
        if self.at.is_empty() {
            return self.path.join(path);
        }
        match path.strip_prefix(&self.at) {
            Some("") => self.path.clone(),
            Some(rest) => self.path.join(rest.trim_start_matches('/')),
            None => self.path.join(path),
        }
    }
}

#[derive(Clone)]
pub struct Project {
    pub root: PathBuf,
    pub game: Option<Game>,
    pub layers: Vec<Layer>,
    pub mounts: Vec<Mount>,
    /// The extensions it leaves off, by id (`[extensions] disabled`): everyone on it reads its assets the same way.
    pub disabled_extensions: Vec<String>,
    /// The extensions it requires (`[[extension]]`).
    pub requires: Vec<Requirement>,
    /// Its extensions' settings, by id (`[extensions."<id>"]`): the same for everyone on it.
    pub extension_settings: std::collections::BTreeMap<String, toml::Table>,
    /// The builds it can be, by name, in order.
    pub configs: Vec<Config>,
    /// Links wanted to what is deployed: where each goes, relative to the project, by the deployed file it stands for.
    pub links: build::Links,
}

fn resolve_path(directory: &Path, value: &str) -> PathBuf {
    let p = Path::new(value);
    if p.is_absolute() { p.to_path_buf() } else { directory.join(p) }
}

impl Project {
    /// The project in `directory`.
    ///
    /// # Errors
    /// Its `project.toml` cannot be read or parsed.
    pub fn load(directory: &Path) -> Result<Project> {
        let root = std::fs::canonicalize(directory).with_context(|| format!("{}", directory.display()))?;
        let path = root.join(FILE);
        let text = std::fs::read_to_string(&path).with_context(|| format!("{}", path.display()))?;
        let file: ProjectFile = toml::from_str(&text).with_context(|| format!("{}", path.display()))?;
        let game = file.game.map(|g| Game {
            layer: g.layer,
            roms: resolve_path(&root, &g.roms),
            declarations: g.declarations,
            releases: g.releases.into_iter().map(|(r, spec)| (r, spec.discs)).collect(),
        });
        let game_layer = game.as_ref().map(|g| g.layer.clone());
        let mounts: Vec<Mount> = file
            .mount
            .into_iter()
            .map(|m| Mount {
                at: m.at.trim_matches('/').to_string(),
                path: resolve_path(&root, &m.path),
                read_only: m.read_only,
            })
            .collect();
        // The mounts are files layers at the bottom of the stack, in their order, each named after its mount point.
        let mut layers: Vec<Layer> = mounts
            .iter()
            .map(|m| Layer {
                name: m.at.clone(),
                kind: LayerKind::Files,
                path: std::fs::canonicalize(&m.path).unwrap_or_else(|_| m.path.clone()),
                at: m.at.clone(),
                changeset: root.join(LAYERS).join(format!("{}.toml", m.at)),
                read_only: m.read_only,
                changeset_writable: true,
                mounted: true,
            })
            .collect();
        for l in file.layer {
            let path = resolve_path(&root, l.path.as_deref().unwrap_or(&format!("{LAYERS}/{}", l.name)));
            let mut changeset = path.clone().into_os_string();
            changeset.push(".toml");
            let read_only = l.read_only || game_layer.as_deref() == Some(l.name.as_str());
            if layers.iter().any(|m| m.name == l.name) {
                bail!("{}: the layer {} is named as a mount point is", root.join(FILE).display(), l.name);
            }
            let kind = match l.kind.as_deref() {
                None | Some("assets") => LayerKind::Assets,
                Some("files") => LayerKind::Files,
                Some(other) => {
                    bail!("{}: the layer {}: no kind {other} (assets, files)", root.join(FILE).display(), l.name)
                }
            };
            layers.push(Layer {
                name: l.name,
                kind,
                path,
                at: String::new(),
                changeset: PathBuf::from(changeset),
                read_only,
                changeset_writable: !read_only,
                mounted: false,
            });
        }
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
        let links = file.build.links;
        let configs = file
            .config
            .into_iter()
            .map(|(name, c)| Config { name, release: c.release, target: c.target, disabled: c.disabled })
            .collect();
        Ok(Project { root, game, layers, mounts, disabled_extensions, requires, extension_settings, configs, links })
    }

    /// The extensions it holds: the folders in `extensions/` of its layers (`ext::folder`), each id's from the highest
    /// layer holding it, in name order.
    #[must_use]
    pub fn held_extensions(&self) -> Vec<PathBuf> {
        let mut held: std::collections::BTreeMap<std::ffi::OsString, PathBuf> = std::collections::BTreeMap::new();
        for layer in self.layers.iter().rev().filter(|layer| !layer.mounted) {
            let entries = std::fs::read_dir(layer.path.join(ext::folder::HELD)).into_iter().flatten().flatten();
            for entry in entries {
                let path = entry.path();
                if path.is_dir() && ext::folder::id_of(&path).is_some() {
                    held.entry(entry.file_name()).or_insert(path);
                }
            }
        }
        held.into_values().collect()
    }

    /// The layer an extension added to it goes in: its highest that is written.
    #[must_use]
    pub fn extensions_layer(&self) -> Option<&Layer> {
        self.layers.iter().rev().find(|layer| !layer.mounted && !layer.read_only)
    }

    /// A `res://` path's place on disk, through the mounts (the longest mount point first).
    ///
    /// # Errors
    /// `uri` is not a `res://` path.
    pub fn resolve(&self, uri: &str) -> Result<PathBuf> {
        let Some(rel) = uri.strip_prefix("res://") else { bail!("{uri}: not a res:// path") };
        let rel = rel.trim_matches('/');
        let mut mounts: Vec<&Mount> = self.mounts.iter().collect();
        mounts.sort_by_key(|m| std::cmp::Reverse(m.at.len()));
        for m in mounts {
            if rel == m.at {
                return Ok(m.path.clone());
            }
            if let Some(rest) = rel.strip_prefix(&format!("{}/", m.at)) {
                return Ok(m.path.join(rest));
            }
        }
        Ok(self.root.join(rel))
    }

    /// The layer named `name`.
    ///
    /// # Errors
    /// The project has no such layer.
    pub fn layer(&self, name: &str) -> Result<&Layer> {
        self.layers.iter().find(|l| l.name == name).with_context(|| format!("the project has no layer {name}"))
    }
}
