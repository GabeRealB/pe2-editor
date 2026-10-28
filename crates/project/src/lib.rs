//! Projects: what a `project.toml` declares, the declarations a project is extracted by, and
//! the extraction that fills its game layer with asset bundles.

pub mod cache;
pub mod changeset;
pub mod declarations;
pub mod extract;
pub mod index;
pub mod lock;
pub mod store;
pub mod textures;
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
}

#[derive(Deserialize)]
struct MountFile {
    at: String,
    path: String,
    #[serde(default)]
    read_only: bool,
}

pub struct Game {
    /// The name of the layer extraction fills.
    pub layer: String,
    pub roms: PathBuf,
    /// A `res://` path.
    pub declarations: String,
    pub releases: BTreeMap<String, Vec<DiscImage>>,
}

pub struct Layer {
    pub name: String,
    /// Its directory.
    pub path: PathBuf,
    /// Its changeset, beside its directory: `<path>.toml`.
    pub changeset: PathBuf,
    /// Only its maker writes it: stated in `project.toml`, or the game layer (extraction's alone).
    pub read_only: bool,
}

pub struct Mount {
    pub at: String,
    pub path: PathBuf,
    pub read_only: bool,
}

pub struct Project {
    pub root: PathBuf,
    pub game: Option<Game>,
    pub layers: Vec<Layer>,
    pub mounts: Vec<Mount>,
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
        let layers = file
            .layer
            .into_iter()
            .map(|l| {
                let path = resolve_path(&root, l.path.as_deref().unwrap_or(&format!("{LAYERS}/{}", l.name)));
                let mut changeset = path.clone().into_os_string();
                changeset.push(".toml");
                let read_only = l.read_only || game_layer.as_deref() == Some(l.name.as_str());
                Layer { name: l.name, path, changeset: PathBuf::from(changeset), read_only }
            })
            .collect();
        let mounts = file
            .mount
            .into_iter()
            .map(|m| Mount { at: m.at, path: resolve_path(&root, &m.path), read_only: m.read_only })
            .collect();
        Ok(Project { root, game, layers, mounts })
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
