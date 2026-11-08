//! Projects: what a `project.toml` declares - its layers, among them generated ones, each filled by its plan (`plan`)
//! from the game's discs - and what is made of it.

pub mod build;
pub mod cache;
pub mod changeset;
pub mod discs;
pub mod edit;
pub mod events;
pub mod ext;
pub mod extract;
pub mod graph;
pub mod history;
pub mod imports;
pub mod index;
pub mod lock;
pub mod plan;
pub mod profile;
pub mod schedule;
pub mod setup;
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
/// "<path>"`), and the links it wants to what builds deploy (`[build.links]`). It is kept out of the project's history
/// and of version control.
#[derive(Default, Deserialize)]
struct LocalFile {
    #[serde(default)]
    mounts: BTreeMap<String, String>,
    #[serde(default)]
    build: BuildFile,
}

fn read_local(root: &Path) -> Result<LocalFile> {
    let path = root.join(LOCAL);
    match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).with_context(|| format!("{}", path.display())),
        Err(_) => Ok(LocalFile::default()),
    }
}

/// The file what of a project is this machine's is kept in.
pub const LOCAL: &str = "project.local.toml";

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
    pub name: String,
    pub kind: LayerKind,
    /// Its content's directory: in `.layers/`, or a mounted tree.
    pub path: PathBuf,
    /// Where its content appears in the resolved project: empty for a layer at the top, the mount point for a mount.
    pub at: String,
    /// Its changeset, in `.layers/`: `<path>.toml` beside a layer's directory, `.layers/<name>.toml` for a mount.
    pub changeset: PathBuf,
    /// Its content is written only by its maker: stated in `project.toml`, or a generated layer (its plan's alone).
    pub read_only: bool,
    /// Whether its changeset is the project's to write: a mount's is, though its tree may be read-only.
    pub changeset_writable: bool,
    /// A mount, which `project.toml` declares apart (`[[mount]]`) until mounts move into the stack.
    pub mounted: bool,
    /// The layer a generated layer gives its nodes' files.
    pub fills: Option<String>,
}

#[derive(Clone)]
pub struct Mount {
    pub at: String,
    pub path: PathBuf,
    pub read_only: bool,
}

impl Layer {
    /// A generated layer's plan: `<path>.plan.jsonl`, beside its folder as its changeset would be, so that what fills
    /// the layer travels with the project while what it made does not.
    #[must_use]
    pub fn plan(&self) -> PathBuf {
        let mut plan = self.path.clone().into_os_string();
        plan.push(".plan.jsonl");
        PathBuf::from(plan)
    }

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
    /// Links this machine wants to what is deployed (`project.local.toml`): where each goes, relative to the project,
    /// by the deployed file it stands for.
    pub links: build::Links,
}

fn resolve_path(directory: &Path, value: &str) -> PathBuf {
    let p = Path::new(value);
    if p.is_absolute() { p.to_path_buf() } else { directory.join(p) }
}

/// What a new project is: a layer of its own, and nothing else.
const EMPTY: &str = "# A project. `res://` paths are absolute from this directory.\n\n[[layer]]\nname = \"project\"\npath = \".layers/project\"\n";

/// Make an empty project in `directory`: its `project.toml`, naming a layer of its own, and that layer's folder. What
/// else a project holds - a game, its layers, extensions - is added to it, or written by whatever makes projects whole.
///
/// # Errors
/// A project is there already, or the folder cannot be written.
pub fn create(directory: &Path) -> Result<()> {
    let file = directory.join(FILE);
    anyhow::ensure!(!file.exists(), "{} holds a project already", directory.display());
    std::fs::create_dir_all(directory.join(".layers/project")).with_context(|| directory.display().to_string())?;
    std::fs::write(&file, EMPTY).with_context(|| file.display().to_string())
}

/// A project's mounts, and its unmanaged ones apart, each where this machine says (`project.local.toml`).
fn mounts_of(root: &Path, declared: Vec<MountFile>, local: &LocalFile) -> Result<(Vec<Mount>, Vec<Mount>)> {
    let (mut mounts, mut unmanaged) = (Vec::new(), Vec::new());
    for m in declared {
        let at = m.at.trim_matches('/').to_string();
        let path = match (m.unmanaged, local.mounts.get(&at), &m.path) {
            (true, Some(here), _) => resolve_path(root, here),
            (_, _, Some(path)) => resolve_path(root, path),
            (true, None, None) => root.join(&at),
            (false, _, None) => bail!("{}: the mount {at} says no path", root.join(FILE).display()),
        };
        let mount = Mount { at, path, read_only: m.read_only || m.unmanaged };
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
        let root = std::fs::canonicalize(directory).with_context(|| format!("{}", directory.display()))?;
        let path = root.join(FILE);
        let text = std::fs::read_to_string(&path).with_context(|| format!("{}", path.display()))?;
        let file: ProjectFile = toml::from_str(&text).with_context(|| format!("{}", path.display()))?;
        let local = read_local(&root)?;
        let (mounts, unmanaged) = mounts_of(&root, file.mount, &local)?;
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
                fills: None,
            })
            .collect();
        for l in file.layer {
            let path = resolve_path(&root, l.path.as_deref().unwrap_or(&format!("{LAYERS}/{}", l.name)));
            let mut changeset = path.clone().into_os_string();
            changeset.push(".toml");
            if layers.iter().any(|m| m.name == l.name) || unmanaged.iter().any(|m| m.at == l.name) {
                bail!("{}: the layer {} is named as a mount point is", root.join(FILE).display(), l.name);
            }
            let kind = match l.kind.as_deref() {
                None | Some("assets") => LayerKind::Assets,
                Some("files") => LayerKind::Files,
                Some("generated") => LayerKind::Generated,
                Some(other) => {
                    bail!(
                        "{}: the layer {}: no kind {other} (assets, files, generated)",
                        root.join(FILE).display(),
                        l.name
                    )
                }
            };
            let generated = kind == LayerKind::Generated;
            if generated != l.fills.is_some() {
                bail!(
                    "{}: the layer {}: a generated layer, and it alone, fills another",
                    root.join(FILE).display(),
                    l.name
                );
            }
            let read_only = l.read_only || generated;
            layers.push(Layer {
                name: l.name,
                kind,
                path,
                at: String::new(),
                changeset: PathBuf::from(changeset),
                read_only,
                changeset_writable: !read_only,
                mounted: false,
                fills: l.fills,
            });
        }
        for layer in &layers {
            if let Some(filled) = &layer.fills {
                let found = layers.iter().find(|l| l.name == *filled);
                if !found.is_some_and(|l| l.kind == LayerKind::Assets) {
                    bail!(
                        "{}: the layer {} fills {filled}, no asset layer of the project",
                        root.join(FILE).display(),
                        layer.name
                    );
                }
            }
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
        let links = local.build.links;
        let configs = file
            .config
            .into_iter()
            .map(|(name, c)| Config { name, release: c.release, target: c.target, disabled: c.disabled })
            .collect();
        Ok(Project {
            root,
            layers,
            mounts,
            unmanaged,
            disabled_extensions,
            requires,
            extension_settings,
            configs,
            links,
        })
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

    /// A `res://` path's place on disk, through the mounts, unmanaged ones too (the longest mount point first).
    ///
    /// # Errors
    /// `uri` is not a `res://` path.
    pub fn resolve(&self, uri: &str) -> Result<PathBuf> {
        let Some(rel) = uri.strip_prefix("res://") else { bail!("{uri}: not a res:// path") };
        let rel = rel.trim_matches('/');
        let mut mounts: Vec<&Mount> = self.mounts.iter().chain(&self.unmanaged).collect();
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

    /// The generated layers, each with the plan that fills it.
    pub fn generated(&self) -> impl Iterator<Item = &Layer> {
        self.layers.iter().filter(|l| l.kind == LayerKind::Generated)
    }

    /// The layer named `name`.
    ///
    /// # Errors
    /// The project has no such layer.
    pub fn layer(&self, name: &str) -> Result<&Layer> {
        self.layers.iter().find(|l| l.name == name).with_context(|| format!("the project has no layer {name}"))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn an_empty_project_loads_and_is_made_once() {
        let dir = std::env::temp_dir().join(format!("pe2-new-project-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        super::create(&dir).unwrap();
        let project = super::Project::load(&dir).unwrap();
        assert_eq!(project.layers.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), ["project"]);
        assert!(super::create(&dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
