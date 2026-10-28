//! A project's entries as the editor sees them: its layers applied bottom to top, each by its changeset - what it
//! creates (assets, which are bundles `<path>.asset/`, folders and plain files), and what it does to the entries below
//! (overrides, moves, deletes). An asset comes with its `asset.toml`, its files, which assets' slots reference it,
//! and what is wrong.
//!
//! The index persists in the project cache's store (`store.rs`, `index.db`): for every file it read, its stamp, the
//! hash of its content, and for an `asset.toml` what the index needs of it - its kind, role, variant names and
//! references - besides its text, which an asset parses only when its fields are asked for. Opening a project stats
//! every file and reads again only those whose stamp changed; a file the store knows that no layer holds any more is
//! dropped from it. A file modified no earlier than its record was made is read again whatever its stamp (git's
//! "racily clean" case). While the project is open, `refresh` takes the paths a watcher reports and scans again only
//! what they touch; each writes only the rows it changed.
//!
//! Entries a layer holds but its changeset does not list are unlisted: where the layer can be written the index lists
//! them, a create each with a new uid (an `asset.toml`'s own `uid`, as declarations wrote them before changesets, is
//! kept); elsewhere they are reported.

use anyhow::Result;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::cache::Cache;
use crate::changeset::{Changeset, Op};
use crate::extract::sha1_hex;
pub use crate::store::Stamp;
use crate::store::{Declaration, Record, Store};
use crate::{FILE, Project, uid};

pub const BUNDLE_SUFFIX: &str = ".asset";
pub const ASSET_FILE: &str = "asset.toml";
const STORE: &str = "index.db";

/// A file of a bundle, from the layer whose version of the asset wins.
#[derive(Clone, Debug)]
pub struct BundleFile {
    pub name: String,
    pub path: PathBuf,
    pub size: u64,
    pub modified: Stamp,
    pub layer: String,
    /// The hash of its content.
    pub sha1: String,
}

/// One asset: its resolved path (`stages/akropolis/rooms/square/folder`), its uid, its `asset.toml` and its files.
/// Its fields are parsed from the `asset.toml`'s text the first time they are asked for.
#[derive(Clone, Debug)]
pub struct Asset {
    pub path: String,
    pub uid: Option<String>,
    pub kind: Option<String>,
    pub role: Option<String>,
    /// Its `variants`' names, in order.
    pub variants: Vec<String>,
    pub files: Vec<BundleFile>,
    /// The layer creating it, then those overriding it, bottom to top.
    pub layers: Vec<String>,
    /// Why its `asset.toml` could not be parsed; its fields are then empty.
    pub error: Option<String>,
    text: Arc<str>,
    fields: OnceLock<toml::Table>,
}

impl Asset {
    /// Its `asset.toml` as written.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Its `asset.toml`'s fields; empty where it cannot be parsed.
    pub fn fields(&self) -> &toml::Table {
        self.fields.get_or_init(|| toml::from_str(&self.text).unwrap_or_default())
    }
}

/// What an entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Asset,
    Folder,
    File,
}

/// An entry of the resolved project, by its real path (a bundle's with `.asset`): what it is, its uid, the layer
/// creating it and the one whose content wins, and where that content is on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub kind: Kind,
    pub uid: String,
    pub home: String,
    pub layer: String,
    pub disk: PathBuf,
}

/// A reference from one asset to another: where it is (the slot's name, or the field holding it) and its target.
#[derive(Clone, Debug)]
pub struct Reference {
    pub from: String,
    pub slot: String,
    pub uid: String,
    pub target: String,
}

/// Something wrong with the project, found by the index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    /// An `asset.toml` that cannot be parsed: the asset is listed without its fields.
    Unreadable { asset: String, error: String },
    /// One uid created twice.
    DuplicateUid { uid: String, paths: Vec<String> },
    /// One path in several layers, which no changeset declares an override of.
    Conflict { path: String, layers: Vec<String> },
    /// An entry a layer holds that its changeset does not list, in a layer that cannot be written.
    Unlisted { layer: String, path: String },
    /// An entry a changeset lists that its layer does not hold.
    Missing { layer: String, path: String },
    /// An operation whose entry no layer below creates.
    Unbound { layer: String, uid: String },
    /// An override whose base changed since it was made.
    Stale { layer: String, path: String },
}

impl Problem {
    /// The paths it is about.
    #[must_use]
    pub fn paths(&self) -> Vec<&str> {
        match self {
            Problem::Unreadable { asset: path, .. }
            | Problem::Conflict { path, .. }
            | Problem::Unlisted { path, .. }
            | Problem::Missing { path, .. }
            | Problem::Stale { path, .. } => vec![path],
            Problem::DuplicateUid { paths, .. } => paths.iter().map(String::as_str).collect(),
            Problem::Unbound { .. } => vec![],
        }
    }

    /// What it is, in a line.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Problem::Unreadable { error, .. } => format!("asset.toml cannot be read: {error}"),
            Problem::DuplicateUid { uid, paths } => format!("{uid} is created {} times", paths.len()),
            Problem::Conflict { layers, .. } => format!("in layers {} without a declared override", layers.join(", ")),
            Problem::Unlisted { layer, .. } => format!("not in the changeset of {layer}, which cannot be written"),
            Problem::Missing { layer, .. } => format!("listed by {layer}, which does not hold it"),
            Problem::Unbound { layer, uid } => format!("{layer} acts on {uid}, which no layer below creates"),
            Problem::Stale { layer, .. } => format!("{layer}'s override was made from another version of it"),
        }
    }

    /// Whether the build can go on with it: an override made from an older base still applies.
    #[must_use]
    pub fn is_warning(&self) -> bool {
        matches!(self, Problem::Stale { .. })
    }
}

/// What a refresh changed: the assets whose index entry changed (added, changed or gone), and whether `project.toml`
/// changed - the layers themselves may have, which a refresh does not follow: the project is opened again.
#[derive(Debug, Default)]
pub struct Refreshed {
    pub assets: BTreeSet<String>,
    pub project_file: bool,
}

fn stamp(time: SystemTime) -> Stamp {
    time.duration_since(UNIX_EPOCH).map_or((0, 0), |d| (d.as_secs(), d.subsec_nanos()))
}

/// A file to read: its path, its stat and whether it is an `asset.toml`.
struct Pending {
    path: PathBuf,
    size: u64,
    modified: Stamp,
    declaration: bool,
}

/// A file's record, read now; none when it vanished meanwhile. A file that cannot be parsed is a record with its
/// error, not a failure: a user may save a half-written `asset.toml`.
fn read(file: &Pending) -> Option<Record> {
    let bytes = std::fs::read(&file.path).ok()?;
    let recorded = stamp(SystemTime::now());
    let declaration = file.declaration.then(|| declaration(&bytes));
    Some(Record { size: file.size, modified: file.modified, recorded, sha1: sha1_hex(&bytes), declaration })
}

/// What the index keeps of an `asset.toml`: parsed once, here, for its kind, role, uid, variant names and references.
fn declaration(bytes: &[u8]) -> Declaration {
    let parsed: Result<toml::Table, String> = std::str::from_utf8(bytes)
        .map_err(|e| e.to_string())
        .and_then(|t| toml::from_str(t).map_err(|e| e.to_string()));
    let mut out = Declaration { text: String::from_utf8_lossy(bytes).into(), ..Declaration::default() };
    match parsed {
        Ok(table) => {
            let string = |key: &str| table.get(key).and_then(toml::Value::as_str).map(str::to_string);
            out.kind = string("kind");
            out.role = string("role");
            out.uid = string("uid");
            out.variants = table
                .get("variants")
                .and_then(toml::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|v| v.get("name")?.as_str().map(str::to_string))
                .collect();
            let name = table.get("name").and_then(toml::Value::as_str);
            for (key, value) in &table {
                references(value, name.unwrap_or(key), &mut out.references);
            }
        }
        Err(e) => out.error = Some(e),
    }
    out
}

/// Stat the files on every core: each one's size (none when it is gone) and modification time.
fn stat_all(files: &[PathBuf]) -> Vec<(Option<u64>, Stamp)> {
    let stat = |file: &PathBuf| match std::fs::metadata(file) {
        Ok(meta) => (Some(meta.len()), meta.modified().map_or((0, 0), stamp)),
        Err(_) => (None, (0, 0)),
    };
    if files.len() < 256 {
        return files.iter().map(stat).collect();
    }
    let threads = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    std::thread::scope(|scope| {
        let workers: Vec<_> = files
            .chunks(files.len().div_ceil(threads))
            .map(|part| scope.spawn(move || part.iter().map(stat).collect::<Vec<_>>()))
            .collect();
        workers.into_iter().flat_map(|w| w.join().unwrap_or_default()).collect()
    })
}

/// Read the files on every core.
fn read_all(files: &[Pending]) -> Vec<Option<Record>> {
    if files.is_empty() {
        return vec![];
    }
    let threads = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    std::thread::scope(|scope| {
        let workers: Vec<_> = files
            .chunks(files.len().div_ceil(threads))
            .map(|part| scope.spawn(move || part.iter().map(read).collect::<Vec<_>>()))
            .collect();
        workers.into_iter().flat_map(|w| w.join().unwrap_or_default()).collect()
    })
}

/// What one layer's directory holds, by its real path relative to the layer (a bundle's with `.asset`): its bundles
/// (their files by name), folders and plain files. Names starting with a dot are the tools' bookkeeping, never
/// entries.
#[derive(Default)]
struct Scan {
    bundles: BTreeMap<String, BTreeMap<String, PathBuf>>,
    folders: BTreeSet<String>,
    files: BTreeMap<String, PathBuf>,
}

impl Scan {
    /// Scan what is at `rel` within the layer `root` (all of it when empty) and everything under it, replacing what
    /// was known there.
    fn rescan(&mut self, root: &Path, rel: &str, changeset: &Changeset) {
        let below = |p: &str| rel.is_empty() || p == rel || p.starts_with(&format!("{rel}/"));
        self.bundles.retain(|p, _| !below(p));
        self.folders.retain(|p| !below(p));
        self.files.retain(|p, _| !below(p));
        let start = if rel.is_empty() { root.to_path_buf() } else { root.join(rel) };
        if !rel.is_empty() {
            let name = rel.rsplit('/').next().unwrap_or(rel);
            if name.starts_with('.') || changeset.ignores(rel) {
                return;
            }
            if start.is_dir() {
                if rel.ends_with(BUNDLE_SUFFIX) {
                    self.bundles.insert(rel.to_string(), bundle_files(&start));
                    return;
                }
                self.folders.insert(rel.to_string());
            } else {
                if start.is_file() {
                    self.files.insert(rel.to_string(), start);
                }
                return;
            }
        }
        // Each folder directly under the start is walked on a core of its own.
        let mut tops = Vec::new();
        self.walk_one(&start, rel, changeset, &mut tops);
        let parts: Vec<Scan> = std::thread::scope(|scope| {
            let workers: Vec<_> = tops
                .into_iter()
                .map(|(dir, dir_rel)| {
                    scope.spawn(move || {
                        let mut part = Scan::default();
                        let mut stack = vec![(dir, dir_rel)];
                        while let Some((dir, dir_rel)) = stack.pop() {
                            part.walk_one(&dir, &dir_rel, changeset, &mut stack);
                        }
                        part
                    })
                })
                .collect();
            workers.into_iter().filter_map(|w| w.join().ok()).collect()
        });
        for part in parts {
            self.bundles.extend(part.bundles);
            self.folders.extend(part.folders);
            self.files.extend(part.files);
        }
    }

    /// One folder's entries: its bundles and plain files kept, its folders kept and handed back to walk.
    fn walk_one(&mut self, dir: &Path, dir_rel: &str, changeset: &Changeset, folders: &mut Vec<(PathBuf, String)>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let child = if dir_rel.is_empty() { name.clone() } else { format!("{dir_rel}/{name}") };
            if name.starts_with('.') || changeset.ignores(&child) {
                continue;
            }
            let Ok(file_type) = entry.file_type() else { continue };
            if file_type.is_dir() {
                if child.ends_with(BUNDLE_SUFFIX) {
                    self.bundles.insert(child, bundle_files(&entry.path()));
                } else {
                    self.folders.insert(child.clone());
                    folders.push((entry.path(), child));
                }
            } else if file_type.is_file() {
                self.files.insert(child, entry.path());
            }
        }
    }

    fn kind(&self, path: &str) -> Option<Kind> {
        if self.bundles.contains_key(path) {
            Some(Kind::Asset)
        } else if self.folders.contains(path) {
            Some(Kind::Folder)
        } else if self.files.contains_key(path) {
            Some(Kind::File)
        } else {
            None
        }
    }

    /// Every path it holds.
    fn paths(&self) -> impl Iterator<Item = &String> {
        self.bundles.keys().chain(self.folders.iter()).chain(self.files.keys())
    }

    /// Every file it holds: bundles' and plain.
    fn all_files(&self) -> impl Iterator<Item = &PathBuf> {
        self.bundles.values().flat_map(BTreeMap::values).chain(self.files.values())
    }
}

/// A bundle's files by name.
fn bundle_files(dir: &Path) -> BTreeMap<String, PathBuf> {
    let mut files = BTreeMap::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with('.') && entry.file_type().is_ok_and(|t| t.is_file()) {
            files.insert(name, entry.path());
        }
    }
    files
}

/// The asset an asset entry's real path names: without `.asset`.
fn asset_of(path: &str) -> &str {
    path.strip_suffix(BUNDLE_SUFFIX).unwrap_or(path)
}

/// A layer as the index holds it.
struct LayerState {
    name: String,
    root: PathBuf,
    changeset_path: PathBuf,
    changeset: Changeset,
    writable: bool,
    scan: Scan,
}

/// An entry while the stack is applied: what it is, its uid, the layer creating it, where its winning content is (a
/// layer and the path there), and the layers that overrode it.
#[derive(Clone, PartialEq, Eq)]
struct Node {
    kind: Kind,
    uid: String,
    home: usize,
    content: (usize, String),
    overrides: Vec<usize>,
}

/// The asset a path relative to a layer's root lies in: the path up to its first `.asset` component, without the
/// suffix. None outside any bundle.
fn bundle_of(rel: &str) -> Option<String> {
    let parts: Vec<&str> = rel.split('/').collect();
    let at = parts.iter().position(|p| p.ends_with(BUNDLE_SUFFIX))?;
    let mut asset = parts[..=at].join("/");
    asset.truncate(asset.len() - BUNDLE_SUFFIX.len());
    Some(asset)
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

/// The paths at or under `path`.
fn subtree<V>(map: &BTreeMap<String, V>, path: &str) -> Vec<String> {
    let prefix = format!("{path}/");
    map.keys().filter(|p| *p == path || p.starts_with(&prefix)).cloned().collect()
}

pub struct Index {
    /// The resolved assets, by path.
    pub assets: BTreeMap<String, Asset>,
    /// Every resolved entry - assets, folders, plain files - by path.
    pub entries: BTreeMap<String, Entry>,
    /// How many files the last opening or refresh read, rather than took from what it knew.
    pub read: usize,
    pub by_uid: HashMap<String, String>,
    /// For each asset, the references naming it.
    pub referenced_by: HashMap<String, Vec<Reference>>,
    /// References whose uid or path names no asset of the project.
    pub dangling: Vec<Reference>,
    pub problems: Vec<Problem>,
    layers: Vec<LayerState>,
    records: HashMap<PathBuf, Record>,
    store: Store,
    resolved: BTreeMap<String, Node>,
    /// The content hash of each entry a writable layer holds, as of the last pass: what tells a move made outside
    /// the editor from a deletion and a new entry.
    hashes: Vec<HashMap<String, String>>,
}

impl Index {
    /// Index a project's layers, through its cache: only files whose stamp changed are read. Unlisted entries of the
    /// layers that can be written are listed in their changesets.
    ///
    /// # Errors
    /// A changeset cannot be read or written, or the cache cannot be written.
    pub fn open(project: &Project, cache: &Cache) -> Result<Index> {
        let store = Store::open(&cache.path(STORE))?;
        let mut known = store.load()?;
        let mut layers = Vec::new();
        for layer in &project.layers {
            let changeset = Changeset::load(&layer.changeset)?;
            let mut scan = Scan::default();
            scan.rescan(&layer.path, "", &changeset);
            layers.push(LayerState {
                name: layer.name.clone(),
                root: layer.path.clone(),
                changeset_path: layer.changeset.clone(),
                writable: !layer.read_only && !changeset.read_only,
                changeset,
                scan,
            });
        }
        let mut index = Index {
            assets: BTreeMap::new(),
            entries: BTreeMap::new(),
            read: 0,
            by_uid: HashMap::new(),
            referenced_by: HashMap::new(),
            dangling: vec![],
            problems: vec![],
            layers,
            records: HashMap::new(),
            store,
            resolved: BTreeMap::new(),
            hashes: vec![],
        };
        let files: Vec<PathBuf> = index.layers.iter().flat_map(|l| l.scan.all_files().cloned()).collect();
        let read = index.load_records(&files, |path, size, modified| {
            known.remove(path).filter(|r| r.size == size && r.modified == modified && modified < r.recorded)
        });
        index.read = read.len();
        // What is left of what the store knew no layer holds any more.
        let gone: Vec<&Path> = known.keys().map(PathBuf::as_path).collect();
        index.store.write(read.iter().map(|p| (p.as_path(), &index.records[p])), gone)?;
        index.apply_stack()?;
        let paths: Vec<String> = index.resolved.keys().cloned().collect();
        index.assemble(&paths);
        index.link();
        Ok(index)
    }

    /// Bring the index up to date with paths a watcher reports changed: what they lie in is scanned again (a file
    /// reported is read again whatever its stamp), a changeset reported is read again, the stack is applied anew, and
    /// the assets whose entry changed are rebuilt. `project.toml` is only noticed: a change to the layers themselves
    /// needs the project opened again.
    ///
    /// # Errors
    /// A changeset cannot be read or written.
    pub fn refresh(&mut self, project: &Project, changed: &[PathBuf]) -> Result<Refreshed> {
        let mut refreshed = Refreshed::default();
        let mut rescanned: Vec<PathBuf> = Vec::new();
        for path in changed {
            if *path == project.root.join(FILE) {
                refreshed.project_file = true;
                continue;
            }
            for layer in &mut self.layers {
                if *path == layer.changeset_path {
                    layer.changeset = Changeset::load(path)?;
                    continue;
                }
                let Ok(rel) = path.strip_prefix(&layer.root) else { continue };
                let rel = rel.to_string_lossy().replace('\\', "/");
                // A change inside a bundle rescans the bundle; anything else, what is at the path.
                let at = bundle_of(&rel).map_or(rel, |asset| format!("{asset}{BUNDLE_SUFFIX}"));
                layer.scan.rescan(&layer.root, &at, &layer.changeset);
                rescanned.push(layer.root.join(&at));
            }
        }
        // Records of files no layer holds any more go; files under what was rescanned are stat'ed again.
        let held: HashSet<PathBuf> = self.layers.iter().flat_map(|l| l.scan.all_files().cloned()).collect();
        let gone: Vec<PathBuf> = self.records.keys().filter(|p| !held.contains(*p)).cloned().collect();
        self.records.retain(|p, _| held.contains(p));
        let files: Vec<PathBuf> = held.iter().filter(|f| rescanned.iter().any(|r| f.starts_with(r))).cloned().collect();
        let reported: HashSet<&PathBuf> = changed.iter().collect();
        let known = std::mem::take(&mut self.records);
        let read = self.load_records(&files, |path, size, modified| {
            known
                .get(path)
                .filter(|r| !reported.contains(&path.to_path_buf()) && r.size == size && r.modified == modified)
                .cloned()
        });
        self.read = read.len();
        for (path, record) in known {
            self.records.entry(path).or_insert(record);
        }
        let records = read.iter().filter_map(|p| Some((p.as_path(), self.records.get(p)?)));
        self.store.write(records, gone.iter().map(PathBuf::as_path))?;
        let before = std::mem::take(&mut self.resolved);
        self.apply_stack()?;
        // An asset changed when its entry did, or when its content lies in what was rescanned.
        for (path, node) in &self.resolved {
            let content = self.layers[node.content.0].root.join(&node.content.1);
            let touched = rescanned.iter().any(|r| content.starts_with(r) || r.starts_with(&content));
            if node.kind == Kind::Asset && (before.get(path) != Some(node) || touched) {
                refreshed.assets.insert(asset_of(path).to_string());
            }
        }
        refreshed.assets.extend(
            self.assets.keys().filter(|a| !self.resolved.contains_key(&format!("{a}{BUNDLE_SUFFIX}"))).cloned(),
        );
        let paths: Vec<String> = refreshed.assets.iter().map(|a| format!("{a}{BUNDLE_SUFFIX}")).collect();
        self.assemble(&paths);
        self.link();
        Ok(refreshed)
    }

    /// Stat the files; take the records `known` still answers for, read the rest on every core. Returns those read.
    fn load_records(
        &mut self,
        files: &[PathBuf],
        mut known: impl FnMut(&Path, u64, Stamp) -> Option<Record>,
    ) -> Vec<PathBuf> {
        let mut pending = Vec::new();
        for (file, (size, modified)) in files.iter().zip(stat_all(files)) {
            let Some(size) = size else { continue };
            if let Some(record) = known(file, size, modified) {
                self.records.insert(file.clone(), record);
            } else {
                let declaration = file.file_name().is_some_and(|n| n == ASSET_FILE)
                    && file.parent().is_some_and(|d| d.to_string_lossy().ends_with(BUNDLE_SUFFIX));
                pending.push(Pending { path: file.clone(), size, modified, declaration });
            }
        }
        let mut read = Vec::new();
        for (file, record) in pending.iter().zip(read_all(&pending)) {
            if let Some(record) = record {
                self.records.insert(file.path.clone(), record);
                read.push(file.path.clone());
            }
        }
        read
    }

    /// The hash of an entry's content in a layer: a plain file's own, a bundle's over its files' names and hashes;
    /// a folder has none.
    fn content_hash(&self, layer: usize, path: &str, kind: Kind) -> Option<String> {
        let scan = &self.layers[layer].scan;
        match kind {
            Kind::File => scan.files.get(path).and_then(|f| self.records.get(f)).map(|r| format!("sha1:{}", r.sha1)),
            Kind::Asset => {
                let files = scan.bundles.get(path)?;
                let mut listing = String::new();
                for (name, f) in files {
                    let sha1 = self.records.get(f).map_or("", |r| r.sha1.as_str());
                    listing.push_str(name);
                    listing.push(' ');
                    listing.push_str(sha1);
                    listing.push('\n');
                }
                Some(format!("sha1:{}", sha1_hex(listing.as_bytes())))
            }
            Kind::Folder => None,
        }
    }

    /// The hash an override of an entry (by its real path) records as its base: the content it is made from, the
    /// version that wins directly below the layer making it.
    #[must_use]
    pub fn base_hash(&self, path: &str) -> Option<String> {
        let node = self.resolved.get(path)?;
        self.content_hash(node.content.0, &node.content.1, node.kind)
    }

    /// Apply the stack: each layer's creates, then its operations, bottom to top. What a writable layer holds unlisted
    /// is listed in its changeset first (written), so applying never meets it again.
    fn apply_stack(&mut self) -> Result<()> {
        self.list_unlisted()?;
        let mut resolved: BTreeMap<String, Node> = BTreeMap::new();
        let mut where_is: HashMap<String, String> = HashMap::new();
        let mut problems = Vec::new();
        for (i, layer) in self.layers.iter().enumerate() {
            let mut accounted: HashSet<String> = HashSet::new();
            for (uid, path) in &layer.changeset.entries {
                let Some(kind) = layer.scan.kind(path) else {
                    problems.push(Problem::Missing { layer: layer.name.clone(), path: path.clone() });
                    continue;
                };
                accounted.insert(path.clone());
                if let Some(other) = resolved.get(path) {
                    let layers = vec![self.layers[other.home].name.clone(), layer.name.clone()];
                    problems.push(Problem::Conflict { path: path.clone(), layers });
                    continue;
                }
                if let Some(other) = where_is.get(uid) {
                    problems.push(Problem::DuplicateUid { uid: uid.clone(), paths: vec![other.clone(), path.clone()] });
                    continue;
                }
                let node = Node { kind, uid: uid.clone(), home: i, content: (i, path.clone()), overrides: vec![] };
                resolved.insert(path.clone(), node);
                where_is.insert(uid.clone(), path.clone());
            }
            for (uid, op) in &layer.changeset.ops {
                let Some(path) = where_is.get(uid).cloned() else {
                    problems.push(Problem::Unbound { layer: layer.name.clone(), uid: uid.clone() });
                    continue;
                };
                let to = match op {
                    Op::Delete => {
                        for p in subtree(&resolved, &path) {
                            if let Some(node) = resolved.remove(&p) {
                                where_is.remove(&node.uid);
                            }
                        }
                        continue;
                    }
                    Op::Move { to } | Op::Override { to: Some(to), .. } => Some(to),
                    Op::Override { to: None, .. } => None,
                };
                let path = match to {
                    Some(to) => {
                        for p in subtree(&resolved, &path) {
                            let target = format!("{to}{}", &p[path.len()..]);
                            if resolved.contains_key(&target) {
                                problems.push(Problem::Conflict { path: target, layers: vec![layer.name.clone()] });
                                continue;
                            }
                            let node = resolved.remove(&p).expect("listed by subtree");
                            where_is.insert(node.uid.clone(), target.clone());
                            resolved.insert(target, node);
                        }
                        to.clone()
                    }
                    None => path,
                };
                if let Op::Override { base, .. } = op {
                    let Some(node) = resolved.get_mut(&path) else { continue };
                    if node.kind == Kind::Folder || layer.scan.kind(&path) != Some(node.kind) {
                        problems.push(Problem::Missing { layer: layer.name.clone(), path: path.clone() });
                        continue;
                    }
                    accounted.insert(path.clone());
                    let (below, below_path) = node.content.clone();
                    if self.content_hash(below, &below_path, node.kind).is_some_and(|h| h != *base) {
                        problems.push(Problem::Stale { layer: layer.name.clone(), path: path.clone() });
                    }
                    node.content = (i, path.clone());
                    node.overrides.push(i);
                }
            }
            // Content the layer holds at a path an entry from below resolves to, without an override: a conflict. A
            // folder there is only the structure its own entries need.
            for path in layer.scan.bundles.keys().chain(layer.scan.files.keys()) {
                if !accounted.contains(path)
                    && let Some(node) = resolved.get(path)
                    && node.home != i
                {
                    let layers = vec![self.layers[node.home].name.clone(), layer.name.clone()];
                    problems.push(Problem::Conflict { path: path.clone(), layers });
                }
            }
            for path in layer.scan.paths() {
                if !accounted.contains(path) && !resolved.contains_key(path) {
                    problems.push(Problem::Unlisted { layer: layer.name.clone(), path: path.clone() });
                }
            }
        }
        self.problems = problems;
        self.resolved = resolved;
        self.remember_hashes();
        Ok(())
    }

    /// Keep the content hash of each entry a writable layer holds, for the next pass to tell moves.
    fn remember_hashes(&mut self) {
        self.hashes = (0..self.layers.len())
            .map(|i| {
                let layer = &self.layers[i];
                if !layer.writable {
                    return HashMap::new();
                }
                let held = layer.scan.bundles.keys().map(|p| (p, Kind::Asset));
                let held = held.chain(layer.scan.files.keys().map(|p| (p, Kind::File)));
                held.filter_map(|(p, k)| Some((p.clone(), self.content_hash(i, p, k)?))).collect()
            })
            .collect();
    }

    /// List what writable layers hold unlisted, and follow what moved outside the editor. A listed entry gone from
    /// its path while an unlisted one holds the same content (by the hashes of the last pass) moved there, and keeps
    /// its uid; a folder follows when what it held moved under a new name. What is left gets a create, under the uid
    /// its `asset.toml` names where it names one not taken, a new one otherwise; the changesets are written. What a
    /// layer holds at a path a lower layer creates is not a create (an override, or a conflict).
    fn list_unlisted(&mut self) -> Result<()> {
        let mut taken: HashSet<String> = HashSet::new();
        for layer in &self.layers {
            taken.extend(layer.changeset.entries.keys().cloned());
        }
        let mut below: HashSet<String> = HashSet::new();
        for i in 0..self.layers.len() {
            let layer = &self.layers[i];
            let listed: HashSet<&String> = layer.changeset.entries.values().collect();
            let mut unlisted: BTreeSet<String> =
                layer.scan.paths().filter(|p| !listed.contains(p) && !below.contains(*p)).cloned().collect();
            if !layer.writable || unlisted.is_empty() {
                below.extend(layer.changeset.entries.values().cloned());
                continue;
            }
            // Moves: a gone entry's last hash found again at an unlisted path, once.
            let gone: Vec<(String, String)> = layer
                .changeset
                .entries
                .iter()
                .filter(|(_, p)| layer.scan.kind(p).is_none())
                .map(|(u, p)| (u.clone(), p.clone()))
                .collect();
            let mut by_hash: HashMap<String, Vec<String>> = HashMap::new();
            for path in &unlisted {
                if let Some(kind) = layer.scan.kind(path).filter(|k| *k != Kind::Folder)
                    && let Some(hash) = self.content_hash(i, path, kind)
                {
                    by_hash.entry(hash).or_default().push(path.clone());
                }
            }
            let last = self.hashes.get(i).cloned().unwrap_or_default();
            let mut moves: Vec<(String, String, String)> = Vec::new();
            for (id, old) in &gone {
                if let Some(new) =
                    last.get(old).and_then(|h| by_hash.get(h)).filter(|c| c.len() == 1).map(|c| c[0].clone())
                    && unlisted.remove(&new)
                {
                    moves.push((id.clone(), old.clone(), new));
                }
            }
            // A gone folder whose content moved from under it to under one new name moved there.
            for (id, old) in &gone {
                if layer.scan.kind(old).is_some() || moves.iter().any(|(u, _, _)| u == id) {
                    continue;
                }
                let prefix = format!("{old}/");
                let targets: BTreeSet<String> = moves
                    .iter()
                    .filter_map(|(_, from, to)| {
                        let rest = from.strip_prefix(&prefix)?;
                        to.strip_suffix(&format!("/{rest}")).map(str::to_string)
                    })
                    .collect();
                if targets.len() == 1
                    && let Some(new) = targets.into_iter().next()
                    && unlisted.remove(&new)
                {
                    moves.push((id.clone(), old.clone(), new));
                }
            }
            let mut creates = Vec::new();
            for path in unlisted {
                let declared = layer
                    .scan
                    .bundles
                    .get(&path)
                    .and_then(|files| files.get(ASSET_FILE))
                    .and_then(|f| self.records.get(f))
                    .and_then(|r| r.declaration.as_ref()?.uid.clone())
                    .filter(|u| uid::parse(u).is_some() && !taken.contains(u));
                let id = match declared {
                    Some(id) => id,
                    None => uid::text(
                        uid::new(|id| taken.contains(&uid::text(id))).map_err(|e| anyhow::anyhow!("a new uid: {e}"))?,
                    ),
                };
                taken.insert(id.clone());
                creates.push((id, path));
            }
            let layer = &mut self.layers[i];
            for (id, _, new) in moves {
                layer.changeset.entries.insert(id, new);
            }
            layer.changeset.entries.extend(creates);
            layer.changeset.save(&layer.changeset_path)?;
            below.extend(layer.changeset.entries.values().cloned());
        }
        Ok(())
    }

    /// Build the assets at these real paths from the resolved stack and the records: their uid, the winning layer's
    /// bundle, its `asset.toml`. A path that no longer resolves to an asset loses its asset; `entries` follows the
    /// whole stack.
    fn assemble(&mut self, paths: &[String]) {
        self.entries = self
            .resolved
            .iter()
            .map(|(path, node)| {
                let layer = &self.layers[node.content.0];
                let entry = Entry {
                    kind: node.kind,
                    uid: node.uid.clone(),
                    home: self.layers[node.home].name.clone(),
                    layer: layer.name.clone(),
                    disk: layer.root.join(&node.content.1),
                };
                (path.clone(), entry)
            })
            .collect();
        for path in paths {
            let Some(node) = self.resolved.get(path).filter(|n| n.kind == Kind::Asset) else {
                self.assets.remove(asset_of(path));
                continue;
            };
            let layer = &self.layers[node.content.0];
            let layers = std::iter::once(node.home).chain(node.overrides.iter().copied());
            let mut asset = Asset {
                path: asset_of(path).to_string(),
                uid: Some(node.uid.clone()),
                kind: None,
                role: None,
                variants: vec![],
                files: vec![],
                layers: layers.map(|l| self.layers[l].name.clone()).collect(),
                error: None,
                text: Arc::from(""),
                fields: OnceLock::new(),
            };
            for (name, file) in layer.scan.bundles.get(&node.content.1).into_iter().flatten() {
                let Some(record) = self.records.get(file) else { continue };
                if name == ASSET_FILE
                    && let Some(d) = &record.declaration
                {
                    asset.kind.clone_from(&d.kind);
                    asset.role.clone_from(&d.role);
                    asset.variants.clone_from(&d.variants);
                    asset.error.clone_from(&d.error);
                    asset.text = d.text.clone();
                }
                asset.files.push(BundleFile {
                    name: name.clone(),
                    path: file.clone(),
                    size: record.size,
                    modified: record.modified,
                    layer: layer.name.clone(),
                    sha1: record.sha1.clone(),
                });
            }
            self.assets.insert(asset.path.clone(), asset);
        }
    }

    /// Resolve the uids and the references, and add what is wrong with the assets' declarations.
    fn link(&mut self) {
        self.by_uid = self.assets.values().filter_map(|a| Some((a.uid.clone()?, a.path.clone()))).collect();
        self.problems.retain(|p| !matches!(p, Problem::Unreadable { .. }));
        for asset in self.assets.values() {
            if let Some(error) = &asset.error {
                self.problems.push(Problem::Unreadable { asset: asset.path.clone(), error: error.clone() });
            }
        }
        let (mut referenced_by, mut dangling) = (HashMap::<String, Vec<Reference>>::new(), Vec::new());
        for asset in self.assets.values() {
            let declaration = asset
                .files
                .iter()
                .find(|f| f.name == ASSET_FILE)
                .and_then(|f| self.records.get(&f.path)?.declaration.as_ref());
            for (slot, uid, target) in declaration.map_or(&[][..], |d| d.references.as_slice()).iter().cloned() {
                let reference = Reference { from: asset.path.clone(), slot, uid, target };
                // The uid decides; the path is the fallback.
                match self
                    .by_uid
                    .get(&reference.uid)
                    .or_else(|| self.assets.contains_key(&reference.target).then_some(&reference.target))
                {
                    Some(resolved) => referenced_by.entry(resolved.clone()).or_default().push(reference),
                    None => dangling.push(reference),
                }
            }
        }
        self.referenced_by = referenced_by;
        self.dangling = dangling;
    }

    /// The references an asset's declaration makes, in its order.
    #[must_use]
    pub fn references_from(&self, path: &str) -> Vec<Reference> {
        let Some(asset) = self.assets.get(path) else { return vec![] };
        let declaration = asset
            .files
            .iter()
            .find(|f| f.name == ASSET_FILE)
            .and_then(|f| self.records.get(&f.path)?.declaration.as_ref());
        declaration
            .map_or(&[][..], |d| d.references.as_slice())
            .iter()
            .map(|(slot, uid, target)| Reference {
                from: path.to_string(),
                slot: slot.clone(),
                uid: uid.clone(),
                target: target.clone(),
            })
            .collect()
    }

    /// The asset a reference names: by uid, else by path.
    #[must_use]
    pub fn resolve(&self, uid: &str, target: &str) -> Option<&Asset> {
        self.by_uid.get(uid).and_then(|p| self.assets.get(p)).or_else(|| self.assets.get(target))
    }
}

#[cfg(test)]
mod tests {
    use super::{Index, Problem, STORE};
    use crate::Project;
    use crate::cache::Cache;
    use crate::changeset::{Changeset, Op};
    use crate::lock::Lock;
    use std::path::{Path, PathBuf};

    /// A project of two layers, `base` and `top`, in `.layers/`; `base` holds the bundle `a` (its `asset.toml` naming
    /// `uid://a`, as declarations did before changesets) in the folder `g`, and `b` and `f`.
    fn project(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("pe2-index-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write(&root, "project.toml", "[[layer]]\nname = \"base\"\n\n[[layer]]\nname = \"top\"\n");
        write(&root, ".layers/base/g/a.asset/asset.toml", "uid = \"uid://a\"\nkind = \"image\"\n");
        write(&root, ".layers/base/g/a.asset/native.texels", "texels");
        write(
            &root,
            ".layers/base/b.asset/asset.toml",
            "kind = \"file\"\nslot = { uid = \"uid://a\", path = \"res://g/a.asset\" }\n",
        );
        write(&root, ".layers/base/f.asset/asset.toml", "kind = \"file\"\n");
        std::fs::create_dir_all(root.join(".layers/top")).unwrap();
        root
    }

    fn write(root: &Path, path: &str, text: &str) -> PathBuf {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        path
    }

    fn open(root: &Path) -> (Project, Index) {
        let project = Project::load(root).unwrap();
        // Held while opening; released for the test's next opening.
        let lock = Lock::try_acquire(root, "a test").unwrap();
        let index = Index::open(&project, &Cache::open(&project, &lock).unwrap()).unwrap();
        (project, index)
    }

    fn changeset(root: &Path, layer: &str) -> Changeset {
        Changeset::load(&root.join(format!(".layers/{layer}.toml"))).unwrap()
    }

    #[test]
    fn lists_what_a_writable_layer_holds() {
        let root = project("lists");
        let (_, index) = open(&root);
        let base = changeset(&root, "base");
        assert_eq!(base.entries.get("uid://a").map(String::as_str), Some("g/a.asset"));
        assert_eq!(base.entries.len(), 4, "{base:?}");
        assert_eq!(index.assets["g/a"].uid.as_deref(), Some("uid://a"));
        assert_eq!(index.referenced_by["g/a"].len(), 1);
        assert!(index.problems.is_empty(), "{:?}", index.problems);
        // Listed now, and read: nothing is listed or read again.
        let (_, again) = open(&root);
        assert_eq!((again.read, changeset(&root, "base")), (0, base));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn applies_overrides_moves_and_deletes() {
        let root = project("ops");
        let (_, index) = open(&root);
        let base = changeset(&root, "base");
        let uid_of = |path: &str| base.entries.iter().find(|(_, p)| *p == path).unwrap().0.clone();
        let mut top = Changeset::default();
        top.ops.insert("uid://a".into(), Op::Override { base: index.base_hash("g/a.asset").unwrap(), to: None });
        top.ops.insert(uid_of("b.asset"), Op::Move { to: "c.asset".into() });
        top.ops.insert(uid_of("f.asset"), Op::Delete);
        top.save(&root.join(".layers/top.toml")).unwrap();
        write(&root, ".layers/top/g/a.asset/asset.toml", "kind = \"image\"\n");
        write(&root, ".layers/top/g/a.asset/native.texels", "repainted");
        let (_, index) = open(&root);
        assert_eq!(index.assets.keys().collect::<Vec<_>>(), ["c", "g/a"]);
        assert_eq!(index.assets["g/a"].layers, ["base", "top"]);
        assert!(index.assets["g/a"].files.iter().all(|f| f.layer == "top"));
        assert_eq!(index.assets["c"].uid, Some(uid_of("b.asset")));
        assert!(index.problems.is_empty(), "{:?}", index.problems);
        // The base changes under the override: stale.
        write(&root, ".layers/base/g/a.asset/native.texels", "retouched");
        let (_, index) = open(&root);
        assert_eq!(index.problems, [Problem::Stale { layer: "top".into(), path: "g/a.asset".into() }]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn reports_what_does_not_add_up() {
        let root = project("problems");
        open(&root);
        let mut top = Changeset { read_only: true, ..Changeset::default() };
        top.entries.insert("uid://gone".into(), "nowhere".into());
        top.ops.insert("uid://unknown".into(), Op::Delete);
        top.save(&root.join(".layers/top.toml")).unwrap();
        write(&root, ".layers/top/b.asset/asset.toml", "kind = \"file\"\n");
        write(&root, ".layers/top/new.asset/asset.toml", "kind = [unclosed\n");
        let (_, index) = open(&root);
        let has = |p: &Problem| index.problems.contains(p);
        assert!(has(&Problem::Conflict { path: "b.asset".into(), layers: vec!["base".into(), "top".into()] }));
        assert!(has(&Problem::Unlisted { layer: "top".into(), path: "new.asset".into() }));
        assert!(has(&Problem::Missing { layer: "top".into(), path: "nowhere".into() }));
        assert!(has(&Problem::Unbound { layer: "top".into(), uid: "uid://unknown".into() }));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn follows_the_watcher() {
        let root = project("refresh");
        let (project, mut index) = open(&root);
        let folder = changeset(&root, "base").entries.iter().find(|(_, p)| *p == "g").unwrap().0.clone();
        // A file of a bundle changed: that bundle, its file read again.
        let texels = write(&root, ".layers/base/g/a.asset/native.texels", "repainted");
        let refreshed = index.refresh(&project, &[texels]).unwrap();
        assert_eq!(refreshed.assets.into_iter().collect::<Vec<_>>(), ["g/a"]);
        assert_eq!(index.read, 1);
        // A folder renamed outside the editor: what it holds keeps its uids, and so does the folder.
        std::fs::rename(root.join(".layers/base/g"), root.join(".layers/base/h")).unwrap();
        index.refresh(&project, &[root.join(".layers/base/g"), root.join(".layers/base/h")]).unwrap();
        assert_eq!(index.assets["h/a"].uid.as_deref(), Some("uid://a"));
        assert_eq!(changeset(&root, "base").entries.get(&folder).map(String::as_str), Some("h"));
        assert!(index.problems.is_empty(), "{:?}", index.problems);
        // A bundle removed: its entry is missing until it is dropped from the changeset.
        std::fs::remove_dir_all(root.join(".layers/base/f.asset")).unwrap();
        index.refresh(&project, &[root.join(".layers/base/f.asset")]).unwrap();
        assert!(!index.assets.contains_key("f"));
        assert!(index.problems.contains(&Problem::Missing { layer: "base".into(), path: "f.asset".into() }));
        assert!(index.refresh(&project, &[root.join("project.toml")]).unwrap().project_file);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn wipes_a_foreign_cache() {
        let root = project("foreign");
        open(&root);
        assert!(root.join(".cache").join(STORE).exists());
        std::fs::write(root.join(".cache/version"), "schema 0\n").unwrap();
        assert_eq!(open(&root).1.read, 4);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
