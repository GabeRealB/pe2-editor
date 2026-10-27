//! A project's assets as the editor sees them: every bundle (`<path>.asset/`) of its layers, joined - the game layer
//! at the bottom, the project's own directory on top, a higher layer's file of a bundle winning - with its
//! `asset.toml`, its files, and which assets' slots reference it.

use anyhow::{Context, Result};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use crate::Project;

pub const BUNDLE_SUFFIX: &str = ".asset";
pub const ASSET_FILE: &str = "asset.toml";

/// A file of a bundle, from the highest layer holding it.
#[derive(Clone, Debug)]
pub struct BundleFile {
    pub name: String,
    pub path: PathBuf,
    pub size: u64,
    pub layer: String,
}

/// One asset: its bundle path (`stages/akropolis/rooms/square/folder`), its `asset.toml` and its files.
#[derive(Clone, Debug)]
pub struct Asset {
    pub path: String,
    pub uid: Option<String>,
    pub kind: Option<String>,
    pub fields: toml::Table,
    pub files: Vec<BundleFile>,
}

/// A reference from one asset to another: where it is (the slot's name, or the field holding it) and its target.
#[derive(Clone, Debug)]
pub struct Reference {
    pub from: String,
    pub slot: String,
    pub uid: String,
    pub target: String,
}

pub struct Index {
    pub assets: BTreeMap<String, Asset>,
    pub by_uid: HashMap<String, String>,
    /// For each asset, the references naming it.
    pub referenced_by: HashMap<String, Vec<Reference>>,
    /// References whose uid or path names no asset of the project.
    pub dangling: Vec<Reference>,
}

/// The directories a project's assets come from, bottom to top, with the names shown for them.
fn layer_roots(project: &Project) -> Vec<(String, PathBuf)> {
    let mut roots: Vec<(String, PathBuf)> = project.layers.iter().map(|l| (l.name.clone(), l.path.clone())).collect();
    roots.push(("project".into(), project.root.clone()));
    roots
}

/// The bundles under `root`, by asset path; a project's own directory skips its layers and mounts.
fn bundles(root: &Path, skip: &[PathBuf], out: &mut BTreeMap<String, PathBuf>) -> Result<()> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;
            if !file_type.is_dir() || skip.contains(&path) {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(stem) = name.strip_suffix(BUNDLE_SUFFIX) {
                let rel = path.parent().unwrap_or(root).strip_prefix(root).unwrap_or(Path::new("")).join(stem);
                out.insert(rel.to_string_lossy().replace('\\', "/"), path);
            } else if !name.starts_with('.') || dir != *root {
                stack.push(path);
            }
        }
    }
    Ok(())
}

/// Every reference in a value: a table holding a `uid` and a `res://` `path`, named by the enclosing slot's `name`
/// or else the field it sits in.
fn references(value: &toml::Value, field: &str, out: &mut Vec<(String, String, String)>) {
    match value {
        toml::Value::Table(t) => {
            if let (Some(toml::Value::String(uid)), Some(toml::Value::String(path))) = (t.get("uid"), t.get("path"))
                && let Some(rel) = path.strip_prefix("res://")
            {
                let target = rel.trim_end_matches(BUNDLE_SUFFIX).to_string();
                out.push((field.to_string(), uid.clone(), target));
                return;
            }
            let name = t.get("name").and_then(toml::Value::as_str);
            for (key, v) in t {
                references(v, name.unwrap_or(key), out);
            }
        }
        toml::Value::Array(a) => a.iter().for_each(|v| references(v, field, out)),
        _ => {}
    }
}

impl Index {
    /// Index a project's layers.
    ///
    /// # Errors
    /// A layer cannot be walked, or an `asset.toml` cannot be read or parsed.
    pub fn load(project: &Project) -> Result<Index> {
        let roots = layer_roots(project);
        let mut skip: Vec<PathBuf> = project.layers.iter().map(|l| l.path.clone()).collect();
        skip.extend(project.mounts.iter().map(|m| project.root.join(&m.at)));
        skip.extend(["roms", "build"].iter().map(|d| project.root.join(d)));
        // Bottom to top: a higher layer's file replaces a lower one's.
        let mut files: BTreeMap<String, BTreeMap<String, BundleFile>> = BTreeMap::new();
        for (layer, root) in &roots {
            let mut found = BTreeMap::new();
            bundles(root, if *root == project.root { &skip } else { &[] }, &mut found)?;
            for (asset, dir) in found {
                let entry = files.entry(asset).or_default();
                for file in std::fs::read_dir(&dir).with_context(|| format!("{}", dir.display()))? {
                    let file = file?;
                    if file.file_type()?.is_file() {
                        let name = file.file_name().to_string_lossy().into_owned();
                        let size = file.metadata()?.len();
                        entry.insert(name.clone(), BundleFile { name, path: file.path(), size, layer: layer.clone() });
                    }
                }
            }
        }
        let mut assets = BTreeMap::new();
        for (path, files) in files {
            let fields: toml::Table = match files.get(ASSET_FILE) {
                Some(f) => {
                    let text = std::fs::read_to_string(&f.path)?;
                    toml::from_str(&text).with_context(|| format!("{}", f.path.display()))?
                }
                None => toml::Table::new(),
            };
            let uid = fields.get("uid").and_then(toml::Value::as_str).map(str::to_string);
            let kind = fields.get("kind").and_then(toml::Value::as_str).map(str::to_string);
            assets.insert(path.clone(), Asset { path, uid, kind, fields, files: files.into_values().collect() });
        }
        let by_uid: HashMap<String, String> =
            assets.values().filter_map(|a| a.uid.clone().map(|u| (u, a.path.clone()))).collect();
        let (mut referenced_by, mut dangling) = (HashMap::<String, Vec<Reference>>::new(), Vec::new());
        for asset in assets.values() {
            let mut found = Vec::new();
            references(&toml::Value::Table(asset.fields.clone()), "", &mut found);
            for (slot, uid, target) in found {
                let reference = Reference { from: asset.path.clone(), slot, uid, target };
                // The uid decides; the path is the fallback.
                match by_uid
                    .get(&reference.uid)
                    .or_else(|| assets.contains_key(&reference.target).then_some(&reference.target))
                {
                    Some(resolved) => referenced_by.entry(resolved.clone()).or_default().push(reference),
                    None => dangling.push(reference),
                }
            }
        }
        Ok(Index { assets, by_uid, referenced_by, dangling })
    }

    /// The asset a reference names: by uid, else by path.
    #[must_use]
    pub fn resolve(&self, uid: &str, target: &str) -> Option<&Asset> {
        self.by_uid.get(uid).and_then(|p| self.assets.get(p)).or_else(|| self.assets.get(target))
    }
}
