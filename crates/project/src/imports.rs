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
//! An import is an output of the work graph (`graph`): the asset's stack takes its source - the stored file on top -
//! through its importer to the `native` output, kept in the project's store by content with its trace, which records
//! what the import read (its source's hash, the declaration's). A changed source, a changed setting or a new importer
//! makes it again, and anything else finds it made. Extraction makes the imports it checks through the same engine,
//! so a project opens without importing. Like the rest of the cache, any of it may be deleted at the cost of time only.

use anyhow::{Context, Result};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Mutex;
use std::sync::Arc;
use toml::{Table, Value};

use crate::cache::Cache;
use crate::ext::{Extensions, Origin, Status};
use crate::extract::sha1_hex;
use crate::graph::exec::{Engine, Reads};
use crate::graph::store::{Blob, Kept, Store};
use crate::graph::{self, Plan};
use crate::index::{Asset, BundleFile, Imported, Index, Problem, Report};

/// Where imports were kept before the store, removed on opening.
const OLD: &str = "imports";

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
    store: Store,
    /// What attaching read of each import, and the settings it resolved to, by its output's key, with what it read:
    /// a refresh finds them here rather than on disk while that holds.
    known: Mutex<HashMap<String, (Reads, Arc<Known>)>>,
    /// The addresses each referenced asset has, by its path, with the hash of the settings they were listed from.
    addresses: Mutex<HashMap<String, Listed>>,
}

/// An asset's addresses, with the hash of the settings they were listed from.
type Listed = (String, Arc<HashSet<String>>);

/// An import as attaching uses it.
struct Known {
    importer: String,
    files: Vec<(String, Blob)>,
    /// The settings it derived, the paths it derives them at, and its notes: resolved over the declaration as written
    /// each time it is attached, as its settings may have changed unapplied.
    made: Table,
    derives: Vec<String>,
    notes: Vec<String>,
    /// Its last resolution, with the declaration it was over: resolved again only when that changed.
    resolved: Mutex<Option<(String, Arc<Imported>)>>,
}

impl Known {
    /// Its settings resolved over a declaration (`resolve`): as last resolved, where that was this declaration.
    fn resolved(&self, asset: &Asset) -> Arc<Imported> {
        let mut last = self.resolved.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((text, resolved)) = last.as_ref()
            && text == asset.text()
        {
            return resolved.clone();
        }
        let mut resolved = resolve(asset.fields(), &self.made, &self.derives);
        resolved.notes.clone_from(&self.notes);
        let resolved = Arc::new(resolved);
        *last = Some((asset.text().to_string(), resolved.clone()));
        resolved
    }
}

impl Imports {
    /// The project's imports.
    ///
    /// # Errors
    /// The store cannot be opened.
    pub fn open(cache: &Cache) -> Result<Imports> {
        let old = cache.path(OLD);
        if old.exists() {
            std::fs::remove_dir_all(&old).with_context(|| old.display().to_string())?;
        }
        Ok(Imports { store: Store::open(cache)?, known: Mutex::default(), addresses: Mutex::default() })
    }

    /// Sweep the store of what the project's nodes no longer make, and of what nodes it no longer has made
    /// (`Store::sweep`): what was swept, as (traces, values, blobs). It reads all the store keeps - a third of a
    /// second for the game - so it is run aside, not on the way to anything.
    pub fn sweep(&self, index: &Index) -> (usize, usize, usize) {
        self.store.sweep(&live_nodes(index), &HashSet::new())
    }

    /// The engine imports are made by: the extensions' operations, keeping in the store.
    #[must_use]
    pub fn engine(&self, extensions: &Extensions) -> Engine {
        Engine::new(extensions.operations(), Some(self.store.clone()))
    }

    /// Give every asset whose importer makes files what its import made, made now where it is not yet: the files take
    /// the place of the bundle's of the same names and say which importer made them, the derived settings resolve over
    /// the declaration's. An import that fails, a declared value it overrides, a part it orphans and a reference
    /// naming a part its asset does not have are problems of the index. Imports no asset made are removed.
    pub fn attach(&self, index: &mut Index, extensions: &Extensions) -> Attached {
        self.attach_some(index, extensions, None, &|_, _, _| {})
    }

    /// Attach as `attach` does, saying how many assets of how many it has gone through.
    pub fn attach_reporting(&self, index: &mut Index, extensions: &Extensions, report: &Report<'_>) -> Attached {
        self.attach_some(index, extensions, None, report)
    }

    /// Attach the imports of these assets alone - those a refresh changed (`Refreshed::assets`) - and of those reading
    /// their outputs (`Extensions::dependents`), the others keeping theirs: their problems made again, and those of the
    /// references from or to them naming outputs. What it costs is what changed; imports no asset makes any more are
    /// removed by the next full attach.
    pub fn attach_changed(&self, index: &mut Index, extensions: &Extensions, changed: &BTreeSet<String>) -> Attached {
        let mut changed = changed.clone();
        changed.extend(extensions.dependents(index, &changed));
        self.attach_some(index, extensions, Some(&changed), &|_, _, _| {})
    }

    fn attach_some(
        &self,
        index: &mut Index,
        extensions: &Extensions,
        only: Option<&BTreeSet<String>>,
        report: &Report<'_>,
    ) -> Attached {
        let mut attached = Attached::default();
        let of = |path: &str| only.is_none_or(|o| o.contains(path));
        // What earlier imports made goes first: an asset may import otherwise now, or not at all.
        // Copied where shared with a snapshot, and only where something goes.
        for asset in index.assets.values_mut().filter(|a| of(&a.path)) {
            if asset.imported.is_some() || asset.files.iter().any(|f| f.made_by.is_some()) {
                let asset = Arc::make_mut(asset);
                asset.files.retain(|f| f.made_by.is_none());
                asset.imported = None;
            }
        }
        let engine = self.engine(extensions);
        let mut known = self.known.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut found: Vec<(String, String, Arc<Known>)> = Vec::new();
        let mut stacks: Vec<Problem> = Vec::new();
        let assets: Vec<&Asset> = match only {
            None => index.assets.values().map(|a| &**a).collect(),
            Some(only) => only.iter().filter_map(|p| index.assets.get(p).map(|a| &**a)).collect(),
        };
        let total = assets.len();
        // What an import reads of other nodes, made in the index as it is.
        let world = crate::ext::Graph { index: &*index, extensions };
        let reach = graph::exec::Reach { engine: &engine, world: &world };
        for (i, asset) in assets.into_iter().enumerate() {
            if i % 256 == 0 {
                report("Attaching what the importers made", i, total);
            }
            let (plan, wrong) = extensions.plan(asset);
            stacks.extend(wrong.into_iter().map(|p| Problem::Import {
                asset: asset.path.clone(),
                error: format!("{}: {}", p.at, p.what),
            }));
            let Some((importer, output)) = import_output(&plan, extensions) else { continue };
            let plan = &plan;
            let key = engine.key(plan, asset, output, None);
            if let Some((_, import)) = known.get(&key).filter(|(reads, _)| reads.hold(asset, Some(reach))) {
                attached.kept += 1;
                found.push((asset.path.clone(), key, import.clone()));
                continue;
            }
            match self.import(asset, extensions, reach, (importer, plan, output), &key) {
                Ok((made, reads, import)) => {
                    if made {
                        attached.made += 1;
                    } else {
                        attached.kept += 1;
                    }
                    known.insert(key.clone(), (reads, import.clone()));
                    found.push((asset.path.clone(), key, import));
                }
                Err(e) => attached.failed.push((asset.path.clone(), format!("{e:#}"))),
            }
        }
        if only.is_none() {
            let live: HashSet<&String> = found.iter().map(|(_, key, _)| key).collect();
            known.retain(|key, _| live.contains(key));
        }
        drop(known);
        let mut problems: Vec<Problem> =
            attached.failed.iter().map(|(asset, error)| Problem::Import { asset: asset.clone(), error: error.clone() }).collect();
        problems.extend(stacks);
        for (path, _, import) in found {
            let Some(asset) = index.assets.get_mut(&path).map(Arc::make_mut) else { continue };
            let layer = source_file(asset).map_or_default(|f| f.layer.clone());
            asset.files.retain(|f| !import.files.iter().any(|(name, _)| *name == f.name));
            for (name, blob) in &import.files {
                asset.files.push(BundleFile {
                    path: self.store.blob_path(&blob.sha1),
                    name: name.clone(),
                    size: blob.size,
                    modified: (0, 0),
                    layer: layer.clone(),
                    sha1: blob.sha1.clone(),
                    made_by: Some(import.importer.clone()),
                });
            }
            let resolved = import.resolved(asset);
            problems.extend(resolved.overridden.iter().map(|setting| Problem::Overridden {
                asset: path.clone(),
                setting: setting.clone(),
            }));
            problems.extend(resolved.orphans.iter().map(|part| Problem::Orphan { asset: path.clone(), part: part.clone() }));
            problems.extend(resolved.notes.iter().map(|note| Problem::Taken { asset: path.clone(), note: note.clone() }));
            asset.imported = Some(resolved);
        }
        problems.extend(self.missing_outputs(index, extensions, only));
        // The project's extensions not loaded, as problems of their assets.
        problems.extend(extensions.present.iter().filter_map(|p| match (&p.origin, &p.status) {
            (Origin::Project { asset, .. }, Status::Refused(why)) => Some(Problem::Extension { asset: asset.clone(), error: why.clone() }),
            _ => None,
        }));
        index.problems.retain(|p| match p {
            Problem::Import { asset, .. }
            | Problem::Overridden { asset, .. }
            | Problem::Orphan { asset, .. }
            | Problem::Taken { asset, .. } => !of(asset),
            Problem::MissingOutput { asset, target, .. } => !of(asset) && !of(target),
            Problem::Extension { .. } => false,
            _ => true,
        });
        index.problems.extend(problems);
        attached
    }

    /// The references naming an output their asset does not have: with the settings resolved, as members an import makes
    /// are among them. An asset's outputs are listed again only when its settings change.
    fn missing_outputs(&self, index: &Index, extensions: &Extensions, only: Option<&BTreeSet<String>>) -> Vec<Problem> {
        let mut known = self.addresses.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut problems = Vec::new();
        let of = |path: &str| only.is_none_or(|o| o.contains(path));
        for (target, references) in index.referenced_by.iter() {
            // Of the assets attached: the references to them, and from them.
            let named: Vec<_> = references.iter().filter(|r| r.output.is_some() && (of(target) || of(&r.from))).collect();
            let Some(asset) = index.assets.get(target).filter(|a| !named.is_empty() && extensions.kind_of(a).is_some())
            else {
                continue;
            };
            let digest = sha1_hex(asset.settings_text().as_bytes());
            let addresses = if let Some((_, addresses)) = known.get(target).filter(|(d, _)| *d == digest) {
                addresses.clone()
            } else {
                let addresses: Arc<HashSet<String>> = Arc::new(extensions.addresses(asset).into_iter().map(|(address, _)| address).collect());
                known.insert(target.clone(), (digest, addresses.clone()));
                addresses
            };
            for reference in named {
                let Some(output) = reference.output.as_ref().filter(|o| !addresses.contains(crate::ext::listed(o))) else { continue };
                problems.push(Problem::MissingOutput {
                    asset: reference.from.clone(),
                    slot: reference.slot.clone(),
                    target: target.clone(),
                    output: output.clone(),
                });
            }
        }
        problems
    }

}

impl Imports {
    /// An asset's import: found kept where its trace holds, else made now - whether it was made, what it read, and
    /// what attaching uses of it.
    fn import(
        &self,
        asset: &Asset,
        extensions: &Extensions,
        reach: graph::exec::Reach<'_>,
        (importer, plan, output): (String, &Plan, usize),
        key: &str,
    ) -> Result<(bool, Reads, Arc<Known>)> {
        // Found kept and readable as this store reads values, else made now.
        let engine = reach.engine;
        let found = engine.kept_in(Some(reach), plan, asset, output, None).and_then(|trace| Some((self.store.describe(&trace.value)?, trace)));
        let (made, (kept, trace)) = if let Some(found) = found {
            (false, found)
        } else {
            engine.output_in(Some(reach), plan, asset, &plan.outputs[output].name, None, true)?;
            let trace = self.store.trace(key).context("its import was not kept")?;
            (true, (self.store.describe(&trace.value).context("its import was not kept")?, trace))
        };
        let Kept::Typed { files, settings, notes, .. } = kept else {
            anyhow::bail!("its import is kept as something else than an extension's type");
        };
        let derives = extensions.op(&importer).map_or_default(|op| op.derives.clone());
        let files: Vec<(String, Blob)> = files.into_iter().map(|f| (f.name, f.blob)).collect();
        Ok((made, trace.reads, Arc::new(Known { importer, files, made: settings, derives, notes, resolved: Mutex::default() })))
    }
}

/// The project's nodes, as traces name them: what the store keeps for others goes in a sweep.
#[must_use]
pub fn live_nodes(index: &Index) -> HashSet<String> {
    index.assets.values().map(|a| graph::exec::id(&**a).to_string()).collect()
}

/// What makes an asset's result - the operation its stack ends with - and the output keeping it, where its result is
/// made: its files are then attached in its bundle's, and its derived settings resolved over its declaration's. (A
/// result that is a stored file is seen as it is.)
fn import_output(plan: &Plan, extensions: &Extensions) -> Option<(String, usize)> {
    let result = plan.result.filter(|_| !plan.result_stored())?;
    let output = plan.result_output()?;
    // A table is read by what asks for it: it has no files to attach.
    if extensions.op(&plan.steps[result].op).is_some_and(|op| op.output == pe2_ext::Type::Table) {
        return None;
    }
    // What events make current is theirs: taking a project in compiles nothing.
    plan.outputs[output].events.pull.is_empty().then(|| (plan.steps[result].op.clone(), output))
}

/// The file an asset's stack takes as its import's source, from its bundle.
fn source_file(asset: &Asset) -> Option<&BundleFile> {
    let name = graph::import_of(graph::exec::Node::fields(asset))?.source;
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
    Imported { settings, text: std::sync::OnceLock::new(), overridden, orphans, notes: vec![] }
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

[[stack]]
op = "file"
file = "source.ora"

[[stack]]
op = "pe2.images@ora"
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
        assert!(asset_text.contains("[[stack]]"));
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
    use pe2_ext::ty::{self, Files, Registered};
    use pe2_ext::{Extension, File, Image, Registry, Result, Retain, Signature, Table, Typed, Value};

    /// A strip's source: a line of cell names, recognized by its name.
    struct Line;

    impl Registered for Line {
        const NAME: &str = "test.strips@text_line";
        const LABEL: &str = "A line of names";

        fn signatures() -> Vec<Signature> {
            vec![Signature::Name("source.txt".into())]
        }
    }

    impl Files for Line {}

    /// A strip's native form.
    struct Strip;

    impl Registered for Strip {
        const NAME: &str = "test.strips@line_native";
        const LABEL: &str = "A strip's native form";
    }

    impl Files for Strip {}
    use std::path::{Path, PathBuf};

    /// A kind whose source is a line of cell names: each a cell one texel wide, its native file the source reversed.
    #[expect(clippy::unnecessary_wraps, reason = "an entry point may fail; this one does not")]
    fn strips(r: &mut Registry) -> Result<()> {
        let _ = r.value_type::<Line>();
        let _ = r.value_type::<Strip>();
        let cell = r.op("cell").input::<Strip>("native").output::<ty::Image>().parameter::<String>("name").run(|_| Ok(Image::default()));
        let _ = r
            .kind("strip")
            .alias("strip")
            .output("cells", &cell, Retain::Transient)
            .members(|asset| asset.parts("cells").into_iter().map(|(name, _)| (name.to_string(), name.to_string())).collect())
            .register();
        let _ = r
            .op("line")
            .input::<Line>("source")
            .output::<Strip>()
            .derives("size")
            .derives("cells.*.rect")
            .run(|call| {
                let source = call.take::<Line>(0)?;
                let source = String::from_utf8(call.read(source.file()?)?)?;
                let names: Vec<&str> = source.split_whitespace().collect();
                let mut cells = Table::new();
                for (i, name) in names.iter().enumerate() {
                    let rect = Value::Array([i, 0, 1, 1].iter().map(|&v| Value::Integer(i64::try_from(v).unwrap())).collect());
                    cells.insert((*name).to_string(), Value::Table(Table::from_iter([("rect".to_string(), rect)])));
                }
                let mut settings = Table::new();
                settings.insert("size".into(), Value::Array(vec![Value::Integer(i64::try_from(names.len())?), Value::Integer(1)]));
                settings.insert("cells".into(), Value::Table(cells));
                let files = vec![File::new("native.bin", source.bytes().rev().collect())];
                Ok(Typed { ty: Strip::NAME.into(), files, settings, ..Typed::default() })
            });
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

    /// A strip's stack: its source through the importer, kept as its native form.
    const STACK: &str = "\n[[stack]]\nop = \"file\"\nfile = \"source.txt\"\n\n[[stack]]\nop = \"test.strips@line\"\n\n\
                         [[stack]]\nop = \"output\"\nname = \"native\"\nretain = \"cache\"\nwhen = \"eager\"\n";

    #[test]
    fn imports_resolve_settings_and_parts() {
        let root: PathBuf = std::env::temp_dir().join(format!("pe2-imports-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write(&root, "project.toml", "[[layer]]\nname = \"base\"\n");
        write(
            &root,
            ".layers/base/s.node/node.toml",
            &format!("uid = \"uid://s\"\nkind = \"strip\"\nsize = [9, 9]\ncells.a = {{ palette = \"warm\" }}\ncells.gone = {{}}\n{STACK}"),
        );
        write(&root, ".layers/base/s.node/source.txt", "a b");
        write(&root, ".layers/base/r.node/node.toml", "kind = \"file\"\nfine = \"uid://s#cells/b\"\nbad = \"uid://s#cells/z\"\n");
        let extensions =
            Extensions::load(vec![Loadable::from(Extension { id: "test.strips", version: "0.1.0", requires: &[], weak: &[], optional: &[], provides: &[], platforms: &[], unsandboxed: false, register: strips })], true);
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
        assert!(matches!(index.problems.iter().find(|p| matches!(p, Problem::MissingOutput { .. })), Some(Problem::MissingOutput { output, .. }) if output == "cells/z"));
        assert!(index.problems.iter().any(|p| matches!(p, Problem::Overridden { setting, .. } if setting == "size")));
        assert!(index.problems.iter().any(|p| matches!(p, Problem::Orphan { part, .. } if part == "cells.gone")));
        // Opened again: found made. The source changed: made again, its old import swept.
        let (_, attached) = open(&root, &extensions);
        assert_eq!((attached.made, attached.kept), (0, 1));
        write(&root, ".layers/base/s.node/source.txt", "a b z");
        let (index, attached) = open(&root, &extensions);
        assert_eq!((attached.made, attached.kept), (1, 0));
        assert!(!index.problems.iter().any(|p| matches!(p, Problem::MissingOutput { .. })));
        // Swept, the old import goes.
        let store = crate::graph::store::Store::at(root.join(".cache/store")).unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
        let _ = store.sweep_older(&super::live_nodes(&index), &std::collections::HashSet::new(), later);
        let kept = |sub: &str| std::fs::read_dir(root.join(".cache/store").join(sub)).unwrap().count();
        assert_eq!((kept("traces"), kept("values")), (1, 1));
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// What an index's imports show: each asset's made files and settings, and the problems (in no order).
    fn shown(index: &Index) -> String {
        let mut out: Vec<String> = index
            .assets
            .values()
            .map(|a| {
                let made: Vec<_> = a.files.iter().filter(|f| f.made_by.is_some()).map(|f| (&f.name, &f.sha1)).collect();
                format!("{} {made:?} {:?}", a.path, a.imported.as_ref().map(|i| &i.settings))
            })
            .collect();
        let mut problems: Vec<String> = index.problems.iter().map(|p| format!("{p:?}")).collect();
        problems.sort();
        out.extend(problems);
        out.join("\n")
    }

    #[test]
    fn attaching_what_changed_shows_what_attaching_all_shows() {
        let root: PathBuf = std::env::temp_dir().join(format!("pe2-imports-changed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write(&root, "project.toml", "[[layer]]\nname = \"base\"\n");
        let declared = format!("kind = \"strip\"\ncells.a = {{}}\n{STACK}");
        write(&root, ".layers/base/s.node/node.toml", &format!("uid = \"uid://s\"\n{declared}"));
        write(&root, ".layers/base/s.node/source.txt", "a b");
        write(&root, ".layers/base/t.node/node.toml", &format!("uid = \"uid://t\"\n{declared}"));
        write(&root, ".layers/base/t.node/source.txt", "a");
        write(&root, ".layers/base/r.node/node.toml", "kind = \"file\"\none = \"uid://s#cells/b\"\ntwo = \"uid://t#cells/b\"\n");
        let extensions =
            Extensions::load(vec![Loadable::from(Extension { id: "test.strips", version: "0.1.0", requires: &[], weak: &[], optional: &[], provides: &[], platforms: &[], unsandboxed: false, register: strips })], true);
        let project = Project::load(&root).unwrap();
        let (mut index, _) = open(&root, &extensions);
        let lock = Lock::try_acquire(&root, "a test").unwrap();
        let imports = Imports::open(&Cache::open(&project, &lock).unwrap()).unwrap();
        drop(lock);
        let base = |p: &str| root.join(".layers/base").join(p);
        // A source repainted (a part gone and one come), a declaration naming another part, both at once.
        for (path, text) in [
            ("s.node/source.txt", "a"),
            ("t.node/source.txt", "a b"),
            ("r.node/node.toml", "kind = \"file\"\none = \"uid://s#cells/a\"\ntwo = \"uid://t#cells/c\"\n"),
        ] {
            write(&root, &format!(".layers/base/{path}"), text);
            let refreshed = index.refresh(&project, &[base(path)]).unwrap();
            imports.attach_changed(&mut index, &extensions, &refreshed.assets);
            let (fresh, _) = open(&root, &extensions);
            assert_eq!(shown(&index), shown(&fresh), "after {path}");
        }
        std::fs::remove_dir_all(&root).unwrap();
    }
}
