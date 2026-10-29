//! Imports: what an asset's importer makes of its source, kept in the project cache (`.cache/imports/<key>/`) and seen
//! by every reader as the asset's own - the bundle holds the source, the native form is made from it.
//!
//! An import makes native files, and kind settings it derives from the source (an image's size, its cells'
//! rectangles): those its importer says it `derives`. They take the place of the declaration's - a value the
//! declaration has there is overridden, and said to be - and the parts of a keyed table the declaration has but the
//! source no longer does are orphaned: kept, reported, until removed or their source part returns. A part the source
//! has and the declaration not is there with what the import derives of it. What readers see is the resolved settings
//! (`index::Asset::settings`); the declaration stays as written.
//!
//! An import is kept by content: its key hashes the importer and its extension's version, the source's hash and the
//! asset's declaration (its settings), so a changed source, a changed setting or a new importer makes it again, and
//! anything else finds it made. Beside the files, `made.toml` lists them with their hashes and holds the derived
//! settings, written last: a folder without it is an import that did not finish, made again. Extraction seeds the
//! imports it checked, so a project opens without importing. Like the rest of the cache, any of it may be deleted at
//! the cost of time only.

use anyhow::{Context, Result};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Mutex;
use std::path::PathBuf;
use std::sync::Arc;
use toml::{Table, Value};

use crate::cache::{Cache, write_atomic};
use crate::ext::{Extensions, Origin, Status};
use crate::extract::sha1_hex;
use crate::index::{Asset, BundleFile, Imported, Index, Problem};

const DIR: &str = "imports";
const MADE: &str = "made.toml";

/// What attaching found.
#[derive(Debug, Default)]
pub struct Attached {
    /// Imports made now, and those found made.
    pub made: usize,
    pub kept: usize,
    /// Imports that failed, with why: their assets have no native files.
    pub failed: Vec<(String, String)>,
}

/// The project's imports.
pub struct Imports {
    dir: PathBuf,
    /// What attaching read of each import, and the settings it resolved to, by key: the key covers the declaration,
    /// so a refresh finds them here rather than on disk.
    known: Mutex<HashMap<String, Arc<Known>>>,
    /// The parts each referenced asset has, by its path, with the hash of the settings they were listed from.
    parts: Mutex<HashMap<String, Listed>>,
}

/// An asset's parts, with the hash of the settings they were listed from.
type Listed = (String, Arc<HashSet<String>>);

/// An import as attaching uses it.
struct Known {
    files: Vec<(String, Made)>,
    resolved: Arc<Imported>,
}

/// A file an import made, as `made.toml` lists it.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Made {
    sha1: String,
    size: u64,
}

/// `made.toml`: the files, and the settings derived.
#[derive(serde::Serialize, serde::Deserialize)]
struct Manifest {
    files: BTreeMap<String, Made>,
    #[serde(default)]
    settings: Table,
}

/// The key of an import: its importer (qualified) and its extension's version as loaded, its source's hash and the
/// asset's declaration.
#[must_use]
pub fn key(extensions: &Extensions, importer: &str, source_sha1: &str, declaration: &str) -> String {
    let id = importer.split_once('@').map_or(importer, |(id, _)| id);
    let version = extensions.version_of(id);
    sha1_hex(format!("{importer}\n{version}\n{source_sha1}\n{declaration}").as_bytes())
}

impl Imports {
    /// The project's imports.
    ///
    /// # Errors
    /// Their folder cannot be made.
    pub fn open(cache: &Cache) -> Result<Imports> {
        let dir = cache.path(DIR);
        std::fs::create_dir_all(&dir).with_context(|| dir.display().to_string())?;
        Ok(Imports { dir, known: Mutex::default(), parts: Mutex::default() })
    }

    fn get(&self, key: &str) -> Option<Manifest> {
        let text = std::fs::read_to_string(self.dir.join(key).join(MADE)).ok()?;
        toml::from_str(&text).ok()
    }

    /// Keep what an import made under its key.
    ///
    /// # Errors
    /// It cannot be written.
    pub fn put(&self, key: &str, imported: &pe2_ext::Imported) -> Result<()> {
        let dir = self.dir.join(key);
        std::fs::create_dir_all(&dir).with_context(|| dir.display().to_string())?;
        let mut files = BTreeMap::new();
        for (name, bytes) in &imported.files {
            if name.contains(['/', '\\']) || name == MADE || name.starts_with('.') {
                anyhow::bail!("{name}: not a name an import may make");
            }
            write_atomic(&dir.join(name), bytes)?;
            files.insert(name.clone(), Made { sha1: sha1_hex(bytes), size: bytes.len() as u64 });
        }
        let manifest = Manifest { files, settings: imported.settings.clone() };
        write_atomic(&dir.join(MADE), toml::to_string(&manifest)?.as_bytes())
    }

    /// Give every asset whose importer makes files what its import made, made now where it is not yet: the files take
    /// the place of the bundle's of the same names and say which importer made them, the derived settings resolve over
    /// the declaration's. An import that fails, a declared value it overrides, a part it orphans and a reference
    /// naming a part its asset does not have are problems of the index. Imports no asset made are removed.
    pub fn attach(&self, index: &mut Index, extensions: &Extensions) -> Attached {
        let mut attached = Attached::default();
        // What earlier imports made goes first: an asset may import otherwise now, or not at all.
        for asset in index.assets.values_mut() {
            asset.files.retain(|f| f.made_by.is_none());
            asset.imported = None;
        }
        let mut known = self.known.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut found: Vec<(String, String, String, Arc<Known>)> = Vec::new();
        for asset in index.assets.values() {
            let Some((importer, key)) = key_of(asset, extensions) else { continue };
            if let Some(import) = known.get(&key) {
                attached.kept += 1;
                found.push((asset.path.clone(), importer, key, import.clone()));
                continue;
            }
            let manifest = match self.get(&key) {
                Some(manifest) => {
                    attached.kept += 1;
                    manifest
                }
                None => match self.make(asset, extensions, &key) {
                    Ok(manifest) => {
                        attached.made += 1;
                        manifest
                    }
                    Err(e) => {
                        attached.failed.push((asset.path.clone(), format!("{e:#}")));
                        continue;
                    }
                },
            };
            let derives = extensions.importer(&importer).map_or_default(|i| i.derives.clone());
            let resolved = Arc::new(resolve(asset.fields(), &manifest.settings, &derives));
            let import = Arc::new(Known { files: manifest.files.into_iter().collect(), resolved });
            known.insert(key.clone(), import.clone());
            found.push((asset.path.clone(), importer, key, import));
        }
        let live: HashSet<&str> = found.iter().map(|(_, _, key, _)| key.as_str()).collect();
        known.retain(|key, _| live.contains(key.as_str()));
        self.sweep(&live);
        drop(known);
        let mut problems: Vec<Problem> =
            attached.failed.iter().map(|(asset, error)| Problem::Import { asset: asset.clone(), error: error.clone() }).collect();
        for (path, importer, key, import) in found {
            let Some(asset) = index.assets.get_mut(&path) else { continue };
            let layer = source_file(asset).map_or_default(|f| f.layer.clone());
            asset.files.retain(|f| !import.files.iter().any(|(name, _)| *name == f.name));
            for (name, file) in &import.files {
                asset.files.push(BundleFile {
                    path: self.dir.join(&key).join(name),
                    name: name.clone(),
                    size: file.size,
                    modified: (0, 0),
                    layer: layer.clone(),
                    sha1: file.sha1.clone(),
                    made_by: Some(importer.clone()),
                });
            }
            let resolved = &import.resolved;
            problems.extend(resolved.overridden.iter().map(|setting| Problem::Overridden {
                asset: path.clone(),
                setting: setting.clone(),
            }));
            problems.extend(resolved.orphans.iter().map(|part| Problem::Orphan { asset: path.clone(), part: part.clone() }));
            asset.imported = Some(resolved.clone());
        }
        problems.extend(self.missing_parts(index, extensions));
        // The project's extensions not loaded, as problems of their assets.
        problems.extend(extensions.present.iter().filter_map(|p| match (&p.origin, &p.status) {
            (Origin::Project { asset, .. }, Status::Refused(why)) => Some(Problem::Extension { asset: asset.clone(), error: why.clone() }),
            _ => None,
        }));
        index.problems.retain(|p| {
            !matches!(
                p,
                Problem::Import { .. }
                    | Problem::Overridden { .. }
                    | Problem::Orphan { .. }
                    | Problem::MissingPart { .. }
                    | Problem::Extension { .. }
            )
        });
        index.problems.extend(problems);
        attached
    }

    /// The references naming a part their asset does not have, of the assets whose kinds list their parts: with the
    /// settings resolved, as parts an import makes are among them. An asset's parts are listed again only when its
    /// settings change.
    fn missing_parts(&self, index: &Index, extensions: &Extensions) -> Vec<Problem> {
        let mut known = self.parts.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut problems = Vec::new();
        for (target, references) in &index.referenced_by {
            let named: Vec<_> = references.iter().filter(|r| r.part.is_some()).collect();
            let Some(asset) = index.assets.get(target).filter(|a| !named.is_empty() && extensions.lists_parts(a))
            else {
                continue;
            };
            let digest = sha1_hex(asset.settings_text().as_bytes());
            let parts = if let Some((_, parts)) = known.get(target).filter(|(d, _)| *d == digest) {
                parts.clone()
            } else {
                let parts: Arc<HashSet<String>> = Arc::new(extensions.parts(asset).into_iter().map(|p| p.id).collect());
                known.insert(target.clone(), (digest, parts.clone()));
                parts
            };
            for reference in named {
                let Some(part) = reference.part.as_ref().filter(|p| !parts.contains(*p)) else { continue };
                problems.push(Problem::MissingPart {
                    asset: reference.from.clone(),
                    slot: reference.slot.clone(),
                    target: target.clone(),
                    part: part.clone(),
                });
            }
        }
        problems
    }

    /// Import an asset now and keep what it made.
    fn make(&self, asset: &Asset, extensions: &Extensions, key: &str) -> Result<Manifest> {
        let imported = extensions.import(&extensions.lend(asset)).context("its importer makes nothing")??;
        self.put(key, &imported)?;
        self.get(key).context("the import could not be kept")
    }

    /// Remove the imports not in `live`, and those that did not finish.
    fn sweep(&self, live: &HashSet<&str>) -> usize {
        let Ok(entries) = std::fs::read_dir(&self.dir) else { return 0 };
        let mut removed = 0;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if !live.contains(name) && std::fs::remove_dir_all(entry.path()).is_ok() {
                removed += 1;
            }
        }
        removed
    }
}

/// An asset's importer and its import's key, where its importer makes files and its source is in its bundle.
fn key_of(asset: &Asset, extensions: &Extensions) -> Option<(String, String)> {
    let importer = extensions.importer_of(asset)?;
    importer.import.as_ref()?;
    let source = source_file(asset)?;
    let name = importer.name.to_string();
    let key = key(extensions, &name, &source.sha1, asset.text());
    Some((name, key))
}

/// The file an asset's import settings name as its source, from its bundle.
fn source_file(asset: &Asset) -> Option<&BundleFile> {
    let name = asset.fields().get("import")?.get("source")?.as_str()?;
    asset.files.iter().find(|f| f.name == name && f.made_by.is_none())
}

/// A path of `derives`, split: `cells.*.rect` to `["cells", "*", "rect"]`.
fn steps(path: &str) -> Vec<&str> {
    path.split('.').collect()
}

/// The declaration's settings with an import's derived ones in their places: at each path the importer derives, the
/// import's value (a declared one there is overridden); under a `*`, the parts the import has - the declaration's
/// settings of each kept beside what the import derives of it, a part only the import has added after the
/// declaration's, a part only the declaration has kept and orphaned.
#[must_use]
pub fn resolve(declared: &Table, made: &Table, paths: &[String]) -> Imported {
    let mut settings = declared.clone();
    let (mut overridden, mut orphans) = (Vec::new(), Vec::new());
    for path in paths {
        place(&mut settings, made, &steps(path), "", &mut overridden, &mut orphans);
    }
    Imported { settings, text: std::sync::OnceLock::new(), overridden, orphans }
}

fn place(
    into: &mut Table,
    from: &Table,
    steps: &[&str],
    at: &str,
    overridden: &mut Vec<String>,
    orphans: &mut Vec<String>,
) {
    let Some((&step, rest)) = steps.split_first() else { return };
    let join = |key: &str| if at.is_empty() { key.to_string() } else { format!("{at}.{key}") };
    match (step, rest.first()) {
        // The parts of a keyed table: each the import has, and the declaration's others orphaned.
        (_, Some(&"*")) => {
            let Some(Value::Table(parts)) = from.get(step) else { return };
            let table = into.entry(step).or_insert_with(|| Value::Table(Table::new()));
            if !table.is_table() {
                overridden.push(join(step));
                *table = Value::Table(Table::new());
            }
            let Value::Table(table) = table else { return };
            for name in table.keys() {
                if !parts.contains_key(name) {
                    orphans.push(format!("{}.{name}", join(step)));
                }
            }
            for (name, part) in parts {
                let Value::Table(part) = part else { continue };
                let mine = table.entry(name.as_str()).or_insert_with(|| Value::Table(Table::new()));
                if let Value::Table(mine) = mine {
                    place(mine, part, &rest[1..], &format!("{}.{name}", join(step)), overridden, orphans);
                }
            }
        }
        (_, Some(_)) => {
            let Some(Value::Table(from)) = from.get(step) else { return };
            let table = into.entry(step).or_insert_with(|| Value::Table(Table::new()));
            if let Value::Table(table) = table {
                place(table, from, rest, &join(step), overridden, orphans);
            }
        }
        (_, None) => {
            let Some(value) = from.get(step) else { return };
            if into.get(step).is_some_and(|v| v != value) {
                overridden.push(join(step));
            }
            into.insert(step.to_string(), value.clone());
        }
    }
}

/// A declaration's text without the settings an importer derives, as extraction writes it for an asset whose source
/// it writes: they are the source's.
///
/// # Errors
/// The text is not TOML.
pub fn without_derived(text: &str, derives: &[String]) -> Result<String> {
    let mut doc: toml_edit::DocumentMut = text.parse()?;
    for path in derives {
        remove(doc.as_item_mut(), &steps(path));
    }
    Ok(doc.to_string())
}

fn remove(item: &mut toml_edit::Item, steps: &[&str]) {
    let Some((&step, rest)) = steps.split_first() else { return };
    let Some(table) = item.as_table_like_mut() else { return };
    if rest.is_empty() {
        table.remove(step);
        return;
    }
    if step == "*" {
        for (_, part) in table.iter_mut() {
            remove(part, rest);
        }
        return;
    }
    if let Some(next) = table.get_mut(step) {
        // An inline table (`cells.dot = { rect = ..., palette = ... }`) is a value, not an item: made an item to walk.
        if let Some(inline) = next.as_inline_table_mut() {
            let mut item = toml_edit::Item::Value(toml_edit::Value::InlineTable(std::mem::take(inline)));
            remove(&mut item, rest);
            if let toml_edit::Item::Value(toml_edit::Value::InlineTable(done)) = item {
                *next.as_inline_table_mut().expect("still inline") = done;
            }
            return;
        }
        remove(next, rest);
    }
}

#[cfg(test)]
mod tests {
    use super::{resolve, without_derived};

    const DECLARED: &str = r#"kind = "image"
mode = "indexed_8bpp"
size = [8, 2]
cells.wide = { rect = [4, 0, 4, 2], palette = "cold" }
cells.gone = { rect = [0, 0, 1, 1], palette = "warm" }

[import]
importer = "pe2.images@ora"
"#;

    #[test]
    fn derived_settings_take_their_places() {
        let derives = ["size".to_string(), "cells.*.rect".to_string()];
        let declared: toml::Table = toml::from_str(DECLARED).unwrap();
        let made: toml::Table =
            toml::from_str("size = [8, 3]\ncells.wide = { rect = [4, 0, 4, 2] }\ncells.new = { rect = [0, 2, 2, 1] }")
                .unwrap();
        let resolved = resolve(&declared, &made, &derives);
        assert_eq!(resolved.settings["size"].as_array().unwrap()[1].as_integer(), Some(3));
        assert_eq!(resolved.settings["cells"]["wide"]["palette"].as_str(), Some("cold"));
        assert_eq!(resolved.settings["cells"]["new"]["rect"].as_array().unwrap().len(), 4);
        let order: Vec<&String> = resolved.settings["cells"].as_table().unwrap().keys().collect();
        assert_eq!(order, ["wide", "gone", "new"]);
        assert_eq!(resolved.overridden, ["size"]);
        assert_eq!(resolved.orphans, ["cells.gone"]);
        let asset_text = toml::to_string(&resolved.settings).unwrap();
        assert!(asset_text.contains("[import]"));
    }

    #[test]
    fn extraction_leaves_the_derived_settings_out() {
        let derives = ["size".to_string(), "cells.*.rect".to_string()];
        let text = without_derived(DECLARED, &derives).unwrap();
        assert!(!text.contains("size"), "{text}");
        assert!(!text.contains("rect"), "{text}");
        assert!(text.contains("cells.wide = { palette = \"cold\" }"), "{text}");
        assert!(text.contains("mode = \"indexed_8bpp\""), "{text}");
    }
}

#[cfg(test)]
mod attaching {
    use super::Imports;
    use crate::Project;
    use crate::cache::Cache;
    use crate::ext::{Extensions, Loadable};
    use crate::index::{Index, Problem};
    use crate::lock::Lock;
    use pe2_ext::{Extension, Imported, Part, Registry, Result, Table, Value};
    use std::path::{Path, PathBuf};

    /// A kind whose source is a line of cell names: each a cell one texel wide, its native file the source reversed.
    #[expect(clippy::unnecessary_wraps, reason = "an entry point may fail; this one does not")]
    fn strips(r: &mut Registry) -> Result<()> {
        let strip = r
            .kind("strip")
            .alias("strip")
            .parts(|asset| {
                asset.parts("cells").into_iter().map(|(name, _)| Part { id: format!("cells/{name}"), label: name.into(), made_of: vec![] }).collect()
            })
            .register();
        let text = r.format("text_line").named("source.txt").register();
        r.importer("line", &strip)
            .reads(&text)
            .derives("size")
            .derives("cells.*.rect")
            .import(|asset| {
                let source = String::from_utf8(asset.read("source.txt")?)?;
                let names: Vec<&str> = source.split_whitespace().collect();
                let mut cells = Table::new();
                for (i, name) in names.iter().enumerate() {
                    let rect = Value::Array([i, 0, 1, 1].iter().map(|&v| Value::Integer(i64::try_from(v).unwrap())).collect());
                    cells.insert((*name).to_string(), Value::Table(Table::from_iter([("rect".to_string(), rect)])));
                }
                let mut settings = Table::new();
                settings.insert("size".into(), Value::Array(vec![Value::Integer(i64::try_from(names.len())?), Value::Integer(1)]));
                settings.insert("cells".into(), Value::Table(cells));
                Ok(Imported { files: vec![("native.bin".into(), source.bytes().rev().collect())], settings })
            })
            .register();
        Ok(())
    }

    fn write(root: &Path, path: &str, text: &str) {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
    }

    fn open(root: &Path, extensions: &Extensions) -> (Index, super::Attached) {
        let project = Project::load(root).unwrap();
        let lock = Lock::try_acquire(root, "a test").unwrap();
        let cache = Cache::open(&project, &lock).unwrap();
        let mut index = Index::open(&project, &cache).unwrap();
        let attached = Imports::open(&cache).unwrap().attach(&mut index, extensions);
        (index, attached)
    }

    #[test]
    fn imports_resolve_settings_and_parts() {
        let root: PathBuf = std::env::temp_dir().join(format!("pe2-imports-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write(&root, "project.toml", "[[layer]]\nname = \"base\"\n");
        write(
            &root,
            ".layers/base/s.asset/asset.toml",
            "uid = \"uid://s\"\nkind = \"strip\"\nsize = [9, 9]\ncells.a = { palette = \"warm\" }\ncells.gone = {}\n\n\
             [import]\nimporter = \"test.strips@line\"\nsource = \"source.txt\"\n",
        );
        write(&root, ".layers/base/s.asset/source.txt", "a b");
        write(&root, ".layers/base/r.asset/asset.toml", "kind = \"file\"\nfine = \"uid://s#cells/b\"\nbad = \"uid://s#cells/z\"\n");
        let extensions =
            Extensions::load(vec![Loadable::from(Extension { id: "test.strips", version: "0.1.0", requires: &[], register: strips })], true);
        assert!(extensions.problems.is_empty(), "{:?}", extensions.problems);
        let (index, attached) = open(&root, &extensions);
        assert_eq!((attached.made, attached.kept, attached.failed.len()), (1, 0, 0));
        let s = &index.assets["s"];
        let made = s.files.iter().find(|f| f.name == "native.bin").unwrap();
        assert_eq!(made.made_by.as_deref(), Some("test.strips@line"));
        assert_eq!(std::fs::read(&made.path).unwrap(), b"b a");
        let settings = s.settings();
        assert_eq!(settings["size"].as_array().unwrap()[0].as_integer(), Some(2));
        assert_eq!(settings["cells"]["a"]["palette"].as_str(), Some("warm"));
        assert!(settings["cells"]["b"]["rect"].is_array());
        let mut problems: Vec<String> = index.problems.iter().map(|p| format!("{p:?}")).collect();
        problems.sort();
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert!(matches!(index.problems.iter().find(|p| matches!(p, Problem::MissingPart { .. })), Some(Problem::MissingPart { part, .. }) if part == "cells/z"));
        assert!(index.problems.iter().any(|p| matches!(p, Problem::Overridden { setting, .. } if setting == "size")));
        assert!(index.problems.iter().any(|p| matches!(p, Problem::Orphan { part, .. } if part == "cells.gone")));
        // Opened again: found made. The source changed: made again, its old import swept.
        let (_, attached) = open(&root, &extensions);
        assert_eq!((attached.made, attached.kept), (0, 1));
        write(&root, ".layers/base/s.asset/source.txt", "a b z");
        let (index, attached) = open(&root, &extensions);
        assert_eq!((attached.made, attached.kept), (1, 0));
        assert!(!index.problems.iter().any(|p| matches!(p, Problem::MissingPart { .. })));
        let kept = std::fs::read_dir(root.join(".cache/imports")).unwrap().count();
        assert_eq!(kept, 1);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
