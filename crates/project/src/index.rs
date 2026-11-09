//! A project's entries as the editor sees them: its layers applied bottom to top, each by its changeset - what it
//! creates (assets, which are bundles `<path>.node/`, folders and plain files), and what it does to the entries below
//! (overrides, moves, deletes). An asset comes with its `node.toml`, its files, which assets' slots reference it,
//! and what is wrong.
//!
//! The index persists in the project cache's store (`store.rs`, `index.db`): for every file it read, its stamp, the
//! hash of its content, and for an `node.toml` what the index needs of it - its type, variant names and
//! references - besides its text, which an asset parses only when its fields are asked for. Opening a project stats
//! every file and reads again only those whose stamp changed; a file the store knows that no layer holds any more is
//! dropped from it. A file modified no earlier than its record was made is read again whatever its stamp (git's
//! "racily clean" case). While the project is open, `refresh` takes the paths a watcher reports and scans again only
//! what they touch; each writes only the rows it changed.
//!
//! Entries a layer holds but its changeset does not list are unlisted: where the layer can be written the index lists
//! them, a create each with a new uid (an `node.toml`'s own `uid`, as declarations wrote them before changesets, is
//! kept); elsewhere they are reported.

use anyhow::{Context as _, Result};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::cache::Cache;
use crate::changeset::{self, Base, Changeset, Op};
use crate::extract::sha1_hex;
pub use crate::store::Stamp;
use crate::store::{Declaration, Record, Store};
use crate::{FILE, Layer, LayerKind, Project, uid};

pub const BUNDLE_SUFFIX: &str = ".node";
pub const NODE_FILE: &str = "node.toml";
/// A node's settings a generated layer holds for it, made by extraction: beneath what its `node.toml` sets
/// (`graph::with_generated`), so that what the game holds stays out of the node and is still edited as its fields.
pub const SETTINGS_FILE: &str = "settings.toml";

/// What names a version an asset overrides, before its uid: the graph's `below` operation reads it as a node.
pub const BELOW: &str = "below:";

/// Whether a path is a records layer's record (`records`).
fn in_records(path: &Path) -> bool {
    path.ancestors().any(|a| a.file_name().is_some_and(|n| n == crate::records::DIR))
}

/// A file an override merged over the one below, in the cache (`.cache/merged/<sha1>/<name>`), written where it is not
/// yet: its folder is named by its content's SHA-1.
fn merged_file(dir: &Path, name: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
    let at = dir.join(sha1_hex(bytes)).join(name);
    if !at.is_file() {
        std::fs::create_dir_all(at.parent().expect("in a folder"))?;
        crate::cache::write_atomic(&at, bytes).map_err(std::io::Error::other)?;
    }
    Ok(at)
}

/// The name a file of an entry at `at` goes by: a bundle's by its own, a plain file by the entry's.
fn file_name(name: &str, at: &str) -> String {
    if name.is_empty() { at.rsplit('/').next().unwrap_or(at).to_string() } else { name.to_string() }
}

/// A merge remembered, by its inputs: what it made (its SHA-1) and its conflicts.
#[derive(serde::Serialize, serde::Deserialize)]
struct Memo {
    sha1: String,
    conflicts: Vec<crate::merge::Conflict>,
}
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
    /// The importer that made it of the asset's source, kept in the cache (`imports`); none for a file of the bundle.
    pub made_by: Option<String>,
}

/// One asset: its resolved path (`stages/akropolis/rooms/square/folder`), its uid, its `node.toml` and its files.
/// Its fields are parsed from the `node.toml`'s text the first time they are asked for.
#[derive(Clone, Debug)]
pub struct Asset {
    pub path: String,
    pub uid: Option<String>,
    /// What of its stack says what it makes (`graph::shape`): nodes alike share one.
    pub shape: Arc<str>,
    /// Its `variants`' names, in order.
    pub variants: Vec<String>,
    pub files: Vec<BundleFile>,
    /// The layer creating it, then those overriding it, bottom to top.
    pub layers: Vec<String>,
    /// Why its `node.toml` could not be parsed; its fields are then empty.
    pub error: Option<String>,
    text: Arc<str>,
    /// Its `settings.toml`, where a generated layer holds one.
    generated: Option<Arc<str>>,
    fields: OnceLock<toml::Table>,
    settings_text: OnceLock<String>,
    /// Its settings as its import resolves them (`imports`), where its importer derives some from its source.
    pub imported: Option<Arc<Imported>>,
    /// It as its files are on disk, where it is shown with unsaved changes (`Working`): what saving it overwrites.
    pub disk: Option<Arc<Asset>>,
}

/// An asset's unsaved files, as the editor's working copy of it has them (`Index::set_working`): each by its name,
/// its content kept at a path by its hash, or absent. The index shows the asset with them in place of the files on
/// disk, so that what reads the index - outputs, views, previews - follows the edit before it is saved.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Working {
    pub files: BTreeMap<String, Option<WorkingFile>>,
}

/// A file of a working copy: where its content is kept, and its hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkingFile {
    pub path: PathBuf,
    pub sha1: String,
}

/// An asset's settings with what its import derived from its source in their places.
#[derive(Clone, Debug, Default)]
pub struct Imported {
    pub settings: toml::Table,
    /// The settings as TOML, for what reads a declaration's text; written the first time it is asked for.
    pub text: OnceLock<String>,
    /// Settings the declaration gives that the import overrides (`size`, `cells.dot.rect`).
    pub overridden: Vec<String>,
    /// Parts the declaration has that the source no longer does (`cells.dot`).
    pub orphans: Vec<String>,
    /// What the import took otherwise than the source has it, as its importer said.
    pub notes: Vec<String>,
}

impl Asset {
    /// It as its files are on disk: itself, where nothing of it is unsaved.
    #[must_use]
    pub fn on_disk(&self) -> &Asset {
        self.disk.as_deref().unwrap_or(self)
    }

    /// Whether it is shown with unsaved changes.
    #[must_use]
    pub fn unsaved(&self) -> bool {
        self.disk.is_some()
    }

    /// Its `node.toml`, as its working copy has it where it has one.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Its `node.toml`'s fields, over its generated `settings.toml`'s; empty where it cannot be parsed.
    pub fn fields(&self) -> &toml::Table {
        self.fields.get_or_init(|| self.fields_of(&self.text))
    }

    /// A declaration's fields, as this asset's would be: over its generated settings.
    #[must_use]
    pub fn fields_of(&self, text: &str) -> toml::Table {
        let declared = toml::from_str(text).unwrap_or_default();
        let generated = self.generated.as_deref().and_then(|g| toml::from_str::<toml::Table>(g).ok());
        let fields = match generated {
            Some(generated) => crate::graph::with_generated(declared, &generated),
            None => declared,
        };
        crate::graph::with_settings(fields)
    }

    /// Its settings as readers see them: its fields, with what its import derived in their places.
    #[must_use]
    pub fn settings(&self) -> &toml::Table {
        self.imported.as_ref().map_or_else(|| self.fields(), |i| &i.settings)
    }

    /// Its settings as TOML.
    #[must_use]
    pub fn settings_text(&self) -> &str {
        let written = |settings: &toml::Table| {
            let settings = settings.iter().filter(|(key, _)| *key != "stack");
            toml::to_string(&settings.map(|(key, value)| (key.clone(), value.clone())).collect::<toml::Table>())
                .unwrap_or_default()
        };
        match &self.imported {
            Some(imported) => imported.text.get_or_init(|| written(&imported.settings)).as_str(),
            None => self.settings_text.get_or_init(|| written(self.fields())).as_str(),
        }
    }
}

/// What an entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Asset,
    Folder,
    File,
}

/// An entry of the resolved project, by its real path (a bundle's with `.node`): what it is, its uid, the layer
/// creating it and the one whose content wins, and where that content is on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub kind: Kind,
    pub uid: String,
    pub home: String,
    pub layer: String,
    pub disk: PathBuf,
    /// The layers overriding it, bottom to top.
    pub overrides: Vec<String>,
    /// The layer that moved it here, where one did.
    pub moved: Option<String>,
}

/// How an operation of a layer applies to the layers below it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpStatus {
    Applied,
    /// An override made from another version of its entry than the one below now.
    Stale,
    /// Its entry is not created below.
    Unbound,
    /// An override whose own version the layer does not hold.
    Missing,
    /// A move onto a path an entry already resolves to.
    Conflict,
}

/// A layer's operation as the editor shows it: its uid, what it does, the path its entry had where it applied.
pub struct OpView {
    pub uid: String,
    pub op: Op,
    pub path: Option<String>,
    pub status: OpStatus,
}

/// An entry a layer creates: its uid, its path, and what the layer holds there (none: missing).
pub struct CreateView {
    pub uid: String,
    pub path: String,
    pub kind: Option<Kind>,
}

/// A part of a patching override made from another version of it below (`Index::stale_parts`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StalePart {
    /// The file of the bundle.
    pub file: String,
    /// The field, as a dotted path; none for a file held or removed whole.
    pub field: Option<String>,
    /// What the layer has, what it was below when made, and what it is below now: none where absent.
    pub mine: Option<String>,
    pub was: Option<String>,
    pub now: Option<String>,
}

/// An entry as the layers below one see it: what an operation of that layer on it stands on.
#[derive(Clone, Debug)]
pub struct Below {
    pub kind: Kind,
    pub uid: String,
    /// Its resolved path below the layer.
    pub path: String,
    /// Its content's hash: what an override copying it whole records as its base. None for a folder.
    pub hash: Option<String>,
    /// Its files' hashes, by name: what an override records of a file it changes, as what it was made from
    /// (`changeset::Base::was`).
    pub sha1s: BTreeMap<String, String>,
    /// Its content's files: a plain file itself (an empty name), a bundle's files by name.
    pub files: Vec<(String, PathBuf)>,
    /// The layer holding its content.
    pub layer: String,
}

/// A layer in the stack as the layer panel shows it.
#[derive(Clone, Debug)]
pub struct LayerInfo {
    pub name: String,
    pub kind: LayerKind,
    /// A mount's mount point; empty for a layer at the top.
    pub at: String,
    /// Its content is its maker's alone.
    pub read_only: bool,
    /// Its changeset can be written.
    pub writable: bool,
    pub mounted: bool,
    /// The layer a generated layer gives its nodes' files.
    pub fills: Option<String>,
    pub entries: usize,
    pub overrides: usize,
    pub moves: usize,
    pub deletes: usize,
    pub problems: usize,
    /// The layers below it that it acts on or references, which it must stay above.
    pub depends_on: Vec<Dependency>,
}

/// What a layer needs of one below it: operations on its entries, references to its assets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dependency {
    pub layer: String,
    pub operations: usize,
    pub references: usize,
}

impl Dependency {
    /// Why a layer must stay above another, as said to the user.
    #[must_use]
    pub fn describe(&self, from: &str) -> String {
        let mut parts = vec![];
        if self.operations > 0 {
            parts.push(format!("acts on {} entr{} of {}", self.operations, plural_y(self.operations), self.layer));
        }
        if self.references > 0 {
            let times = if self.references == 1 { "once".to_string() } else { format!("{} times", self.references) };
            parts.push(format!("references {}'s assets {times}", self.layer));
        }
        format!("{from} {}", parts.join(" and "))
    }
}

fn plural_y(n: usize) -> &'static str {
    if n == 1 { "y" } else { "ies" }
}

/// A reference from one asset to another: where it is (the slot's name, or the field holding it) and its target.
#[derive(Clone, Debug)]
pub struct Reference {
    pub from: String,
    pub slot: String,
    pub uid: String,
    pub target: String,
    /// The output of the asset it names (`palettes/default`, from `uid://...#palettes/default`); none for the asset itself.
    pub output: Option<String>,
}

/// Something wrong with the project, found by the index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    /// An `node.toml` that cannot be parsed: the asset is listed without its fields.
    Unreadable { asset: String, error: String },
    /// One uid created twice.
    DuplicateUid { uid: String, paths: Vec<String> },
    /// One path in several layers, which no changeset declares an override of.
    Conflict { path: String, layers: Vec<String> },
    /// What a layer holds that its changeset does not list: put in a layer folder by other means than the editor, or in
    /// one that cannot be written. Adopted (listed) or removed by the user.
    Unlisted { layer: String, path: String },
    /// An entry a changeset lists that its layer does not hold; or holds where the index leaves it out (`left_out`:
    /// an extension's folder, an ignored name), which no asset is.
    Missing { layer: String, path: String, left_out: bool },
    /// An operation whose entry no layer below creates.
    Unbound { layer: String, uid: String },
    /// An override whose base changed since it was made.
    Stale { layer: String, path: String },
    /// An asset whose import failed: it has no native files.
    Import { asset: String, error: String },
    /// An extension the project requires and does not have.
    Required { id: String, why: String },
    /// A stack that cannot run as it is written.
    Stack { asset: String, error: String },
    /// A setting the declaration gives that the asset's import derives from its source, which wins.
    Overridden { asset: String, setting: String },
    /// A part's settings the declaration has for a part its source no longer has.
    Orphan { asset: String, part: String },
    /// A reference naming an output (`uid://...#palettes/night`) its asset does not have.
    MissingOutput { asset: String, slot: String, target: String, output: String },
    /// An extension the project holds that is not loaded, by id, and why.
    Extension { id: String, error: String },
    /// What an asset's import took otherwise than its source has it (colors matched to the nearest), as its importer
    /// said: the build goes on with it.
    Taken { asset: String, note: String },
}

impl Problem {
    /// The paths it is about.
    #[must_use]
    pub fn paths(&self) -> Vec<&str> {
        match self {
            Problem::Unreadable { asset: path, .. }
            | Problem::Import { asset: path, .. }
            | Problem::Stack { asset: path, .. }
            | Problem::Required { id: path, .. }
            | Problem::Overridden { asset: path, .. }
            | Problem::Orphan { asset: path, .. }
            | Problem::MissingOutput { asset: path, .. }
            | Problem::Extension { id: path, .. }
            | Problem::Taken { asset: path, .. }
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
            Problem::Unreadable { error, .. } => format!("node.toml cannot be read: {error}"),
            Problem::DuplicateUid { uid, paths } => format!("{uid} is created {} times", paths.len()),
            Problem::Conflict { layers, .. } => format!("in layers {} without a declared override", layers.join(", ")),
            Problem::Unlisted { layer, .. } => {
                format!("held by {layer}, which does not list it: put there by other means than the editor")
            }
            Problem::Missing { layer, left_out: false, .. } => format!("listed by {layer}, which does not hold it"),
            Problem::Missing { layer, left_out: true, .. } => {
                format!("listed by {layer}, which leaves it out: no asset")
            }
            Problem::Unbound { layer, uid } => format!("{layer} acts on {uid}, which no layer below creates"),
            Problem::Stale { layer, .. } => format!("{layer}'s override was made from another version of it"),
            Problem::Import { error, .. } => format!("its import failed: {error}"),
            Problem::Stack { error, .. } => format!("its stack: {error}"),
            Problem::Required { why, .. } => format!("the project requires it: {why}"),
            Problem::Overridden { setting, .. } => {
                format!("{setting} is set, but its import derives it from the source, which wins")
            }
            Problem::Orphan { part, .. } => format!("{part} has settings, but its source has no such part"),
            Problem::MissingOutput { slot, target, output, .. } => {
                format!("{slot} names {target}#{output}, which it does not have")
            }
            Problem::Extension { error, .. } => format!("the project holds it, and it is not loaded: {error}"),
            Problem::Taken { note, .. } => format!("its import: {note}"),
        }
    }

    /// The layers it is about.
    #[must_use]
    pub fn layers(&self) -> Vec<&str> {
        match self {
            Problem::Conflict { layers, .. } => layers.iter().map(String::as_str).collect(),
            Problem::Unlisted { layer, .. }
            | Problem::Missing { layer, .. }
            | Problem::Unbound { layer, .. }
            | Problem::Stale { layer, .. } => vec![layer.as_str()],
            Problem::Unreadable { .. }
            | Problem::DuplicateUid { .. }
            | Problem::Import { .. }
            | Problem::Stack { .. }
            | Problem::Required { .. }
            | Problem::Overridden { .. }
            | Problem::Orphan { .. }
            | Problem::MissingOutput { .. }
            | Problem::Taken { .. }
            | Problem::Extension { .. } => vec![],
        }
    }

    /// Whether the build can go on with it: an override made from an older base still applies.
    #[must_use]
    pub fn is_warning(&self) -> bool {
        matches!(
            self,
            Problem::Stale { .. } | Problem::Overridden { .. } | Problem::Orphan { .. } | Problem::Taken { .. }
        )
    }
}

/// What a refresh changed: the assets whose index entry changed (added, changed or gone), and whether `project.toml`
/// changed - the layers themselves may have, which a refresh does not follow: the project is read again.
#[derive(Debug, Default)]
pub struct Refreshed {
    pub assets: BTreeSet<String>,
    pub project_file: bool,
    /// The stack was applied again: what changed was more than contents.
    pub restacked: bool,
}

fn stamp(time: SystemTime) -> Stamp {
    time.duration_since(UNIX_EPOCH).map_or((0, 0), |d| (d.as_secs(), d.subsec_nanos()))
}

/// A file to read: its path, its stat and whether it is an `node.toml`.
struct Pending {
    path: PathBuf,
    size: u64,
    modified: Stamp,
    declaration: bool,
}

/// A file's record, read now; none when it vanished meanwhile. A file that cannot be parsed is a record with its
/// error, not a failure: a user may save a half-written `node.toml`.
fn read(file: &Pending) -> Option<Record> {
    let recorded = stamp(SystemTime::now());
    // A declaration is read whole, to be parsed; any other file is only hashed, a piece at a time.
    let (sha1, declaration) = if file.declaration {
        let bytes = crate::content::read(&file.path).ok()?;
        (sha1_hex(&bytes), Some(declaration(&bytes)))
    } else {
        (crate::content::sha1(&file.path).ok()?, None)
    };
    Some(Record { size: file.size, modified: file.modified, recorded, sha1, declaration })
}

/// What the index keeps of an `node.toml`: parsed once, here, for its type, uid, variant names and references.
fn declaration(bytes: &[u8]) -> Declaration {
    let parsed: Result<toml::Table, String> = std::str::from_utf8(bytes)
        .map_err(|e| e.to_string())
        .and_then(|t| toml::from_str(t).map_err(|e| e.to_string()));
    let mut out = Declaration { text: String::from_utf8_lossy(bytes).into(), ..Declaration::default() };
    match parsed {
        Ok(table) => {
            let string = |key: &str| table.get(key).and_then(toml::Value::as_str).map(str::to_string);
            out.shape = crate::graph::shape(&table);
            out.uid = string("uid");
            // An image's looks: its variants, keyed by name.
            let settings = crate::graph::with_settings(table.clone());
            out.variants = settings
                .get("variants")
                .and_then(toml::Value::as_table)
                .into_iter()
                .flat_map(|t| t.keys().cloned())
                .collect();
            let name = table.get("name").and_then(toml::Value::as_str);
            // An asset's own `uid` (as declarations named it before changesets) is not a reference.
            for (key, value) in table.iter().filter(|(key, _)| *key != "uid") {
                references(value, name.unwrap_or(key), &mut out.references);
            }
        }
        Err(e) => out.error = Some(e),
    }
    out
}

/// Stat the files on every core: each one's size (none when it is gone) and modification time.
fn stat_all(files: &[PathBuf]) -> Vec<(Option<u64>, Stamp)> {
    let stat = |file: &PathBuf| match crate::content::stat(file) {
        Ok((len, modified)) => (Some(len), stamp(modified)),
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

/// What one layer's directory holds, by its real path relative to the layer (a bundle's with `.node`): its bundles
/// (their files by name), folders and plain files. Names starting with a dot are the tools' bookkeeping, never
/// entries.
#[derive(Clone, Default)]
struct Scan {
    bundles: BTreeMap<String, BTreeMap<String, PathBuf>>,
    folders: BTreeSet<String>,
    files: BTreeMap<String, PathBuf>,
}

/// Where a layer's content is and how it is read: its directory, the resolved path it appears at (a mount point, or
/// none), the names it leaves out, and whether it holds assets (a files layer's `.node` folders are plain folders).
#[derive(Clone)]
struct Place {
    root: PathBuf,
    at: String,
    ignore: Vec<String>,
    assets: bool,
}

impl Place {
    /// A resolved path's place on disk.
    fn disk(&self, path: &str) -> PathBuf {
        if self.at.is_empty() {
            return self.root.join(path);
        }
        match path.strip_prefix(&self.at) {
            Some("") => self.root.clone(),
            Some(rest) => self.root.join(rest.trim_start_matches('/')),
            None => self.root.join(path),
        }
    }

    /// The resolved path of a place on disk within the layer.
    fn resolved(&self, disk: &Path) -> Option<String> {
        let rel = disk.strip_prefix(&self.root).ok()?.to_string_lossy().replace('\\', "/");
        Some(match (self.at.is_empty(), rel.is_empty()) {
            (true, _) => rel,
            (false, true) => self.at.clone(),
            (false, false) => format!("{}/{rel}", self.at),
        })
    }

    /// Whether a resolved path is left out: by its names within the layer.
    fn ignores(&self, path: &str) -> bool {
        let within = if self.at.is_empty() { path } else { path.strip_prefix(&self.at).unwrap_or(path) };
        changeset::ignored(&self.ignore, within.trim_start_matches('/')) || self.holds_extension(path)
    }

    /// Whether a path is in the folder of a layer holding its extensions (`extensions/`, `ext::folder`): code, which no
    /// asset is.
    fn holds_extension(&self, path: &str) -> bool {
        let held = crate::ext::folder::HELD;
        self.at.is_empty() && path.strip_prefix(held).is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    }
}

impl Scan {
    /// Scan what is at the resolved path `rel` within a layer (all of it when empty) and everything under it,
    /// replacing what was known there. A mount's tree is the folder at its mount point.
    fn rescan(&mut self, place: &Place, rel: &str) {
        let rel = if rel == place.at { "" } else { rel };
        let below = |p: &str| rel.is_empty() || p == rel || p.starts_with(&format!("{rel}/"));
        self.bundles.retain(|p, _| !below(p));
        self.folders.retain(|p| !below(p));
        self.files.retain(|p, _| !below(p));
        let start = if rel.is_empty() { place.root.clone() } else { place.disk(rel) };
        let rel = if rel.is_empty() { place.at.as_str() } else { rel };
        if rel == place.at {
            if !place.at.is_empty() {
                self.folders.insert(place.at.clone());
            }
        } else {
            let name = rel.rsplit('/').next().unwrap_or(rel);
            if name.starts_with('.') || place.ignores(rel) {
                return;
            }
            if crate::pack::is_dir(&start) {
                if place.assets && rel.ends_with(BUNDLE_SUFFIX) {
                    self.bundles.insert(rel.to_string(), bundle_files(&start));
                    return;
                }
                self.folders.insert(rel.to_string());
            } else {
                if crate::pack::is_file(&start) {
                    self.files.insert(rel.to_string(), start);
                }
                return;
            }
        }
        // Each folder directly under the start is walked on a core of its own.
        let mut tops = Vec::new();
        self.walk_one(place, &start, rel, &mut tops);
        let parts: Vec<Scan> = std::thread::scope(|scope| {
            let workers: Vec<_> = tops
                .into_iter()
                .map(|(dir, dir_rel)| {
                    scope.spawn(move || {
                        let mut part = Scan::default();
                        let mut stack = vec![(dir, dir_rel)];
                        while let Some((dir, dir_rel)) = stack.pop() {
                            part.walk_one(place, &dir, &dir_rel, &mut stack);
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
    fn walk_one(&mut self, place: &Place, dir: &Path, dir_rel: &str, folders: &mut Vec<(PathBuf, String)>) {
        // On disk, or in a packed layer's listing.
        for (name, path, is_dir) in crate::pack::read_dir(dir) {
            let child = if dir_rel.is_empty() { name.clone() } else { format!("{dir_rel}/{name}") };
            if name.starts_with('.') || place.ignores(&child) {
                continue;
            }
            if is_dir {
                if place.assets && child.ends_with(BUNDLE_SUFFIX) {
                    self.bundles.insert(child, bundle_files(&path));
                } else {
                    self.folders.insert(child.clone());
                    folders.push((path, child));
                }
            } else {
                let (name, path) = listed(&name, path);
                let child = if dir_rel.is_empty() { name } else { format!("{dir_rel}/{name}") };
                self.files.insert(child, path);
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

    /// What it holds at the resolved path `rel` and under it (all of it when empty).
    fn under(&self, rel: &str) -> Scan {
        let prefix = format!("{rel}/");
        let below = |p: &String| rel.is_empty() || p == rel || p.starts_with(&prefix);
        Scan {
            bundles: self.bundles.iter().filter(|(p, _)| below(p)).map(|(p, f)| (p.clone(), f.clone())).collect(),
            folders: self.folders.iter().filter(|p| below(p)).cloned().collect(),
            files: self.files.iter().filter(|(p, _)| below(p)).map(|(p, f)| (p.clone(), f.clone())).collect(),
        }
    }

    /// Whether it holds the same paths as `other`: what a change of contents alone leaves.
    fn same_paths(&self, other: &Scan) -> bool {
        self.bundles.keys().eq(other.bundles.keys())
            && self.folders == other.folders
            && self.files.keys().eq(other.files.keys())
    }

    /// Whether it holds this file on disk, of a bundle or plain.
    fn holds(&self, place: &Place, file: &Path) -> bool {
        let Some(rel) = place.resolved(file) else { return false };
        if self.files.get(&rel).is_some_and(|f| f == file) {
            return true;
        }
        bundle_of(&rel).is_some_and(|asset| {
            self.bundles.get(&format!("{asset}{BUNDLE_SUFFIX}")).is_some_and(|files| files.values().any(|f| f == file))
        })
    }
}

/// A bundle's files by name: their paths within it, a file in a folder of it (a UFO font's) as `<folder>/<file>`.
/// What is hidden (a name starting with `.`) is left out, folders and all.
pub(crate) fn bundle_files(dir: &Path) -> BTreeMap<String, PathBuf> {
    let mut files = BTreeMap::new();
    let mut pending = vec![(dir.to_path_buf(), String::new())];
    while let Some((folder, prefix)) = pending.pop() {
        // On disk, or in a packed layer's listing.
        for (name, path, is_dir) in crate::pack::read_dir(&folder) {
            if name.starts_with('.') {
                continue;
            }
            if is_dir {
                pending.push((path, format!("{prefix}{name}/")));
            } else {
                // A large file is held as its manifest: listed as the file it stands for.
                let (name, path) = listed(&name, path);
                files.insert(format!("{prefix}{name}"), path);
            }
        }
    }
    files
}

/// A file as the index lists it: a manifest (`<name>.chunks`) as the file it stands for, by that file's name and path.
fn listed(name: &str, path: PathBuf) -> (String, PathBuf) {
    match (name.strip_suffix(crate::chunks::SUFFIX), crate::chunks::logical(&path)) {
        (Some(name), Some(logical)) => (name.to_string(), logical),
        _ => (name.to_string(), path),
    }
}

/// The asset an asset entry's real path names: without `.node`.
fn asset_of(path: &str) -> &str {
    path.strip_suffix(BUNDLE_SUFFIX).unwrap_or(path)
}

/// How what a layer holds unlisted gets listed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Listing {
    /// As it is found: a mounted tree, worked on with its own tools.
    Always,
    /// The first time it is indexed: a layer folder with no changeset yet.
    Once,
    /// Only as the user adopts it (`edit::adopt`): a layer folder, the editor's storage.
    Adopted,
}

/// A layer as the index holds it.
#[derive(Clone)]
struct LayerState {
    name: String,
    kind: LayerKind,
    place: Place,
    changeset_path: PathBuf,
    changeset: Changeset,
    /// Names its tree's own rules leave out (a mount's `.gitignore`s), beside its changeset's.
    tree_ignore: Vec<String>,
    /// Its content is its maker's alone.
    read_only: bool,
    /// Its changeset can be written: unlisted entries are listed in it.
    writable: bool,
    mounted: bool,
    /// How what it holds unlisted gets listed (`list_unlisted`).
    listing: Listing,
    /// The layer whose nodes a generated layer gives their files: taking no part in the stack, what it holds at a path
    /// is that layer's bundle's too.
    fills: Option<String>,
    /// What it holds, shared with the snapshots made of the index until it is scanned again.
    scan: Arc<Scan>,
}

impl LayerState {
    fn new(layer: &Layer) -> Result<LayerState> {
        let records = crate::records::is_records(&layer.path);
        let changeset = LayerState::changeset_of(layer)?;
        let tree_ignore = if layer.mounted { tree_ignores(&layer.path) } else { vec![] };
        let mut ignore = changeset.ignore.clone();
        ignore.extend(tree_ignore.iter().cloned());
        let assets = layer.kind != LayerKind::Files;
        let place = Place { root: layer.path.clone(), at: layer.at.clone(), ignore, assets };
        let mut scan = Scan::default();
        scan.rescan(&place, "");
        Ok(LayerState {
            name: layer.name.clone(),
            kind: layer.kind,
            place,
            changeset_path: layer.changeset.clone(),
            writable: layer.changeset_writable && !changeset.read_only,
            changeset,
            tree_ignore,
            read_only: layer.read_only,
            mounted: layer.mounted,
            listing: if layer.mounted {
                Listing::Always
            } else if records || layer.changeset.exists() {
                Listing::Adopted
            } else {
                Listing::Once
            },
            fills: layer.fills.clone(),
            scan: Arc::new(scan),
        })
    }

    /// A layer's changeset: as its file says, or, for a records layer, as its records say (`records::Set::changeset`).
    fn changeset_of(layer: &Layer) -> Result<Changeset> {
        if crate::records::is_records(&layer.path) {
            return crate::records::Set::changeset(&layer.path)
                .with_context(|| format!("{}: its records are not read", layer.name));
        }
        Changeset::load(&layer.changeset)
    }

    /// Its changeset read again, and the names it leaves out with it.
    fn reload_changeset(&mut self) -> Result<()> {
        self.changeset = if crate::records::is_records(&self.place.root) {
            crate::records::Set::changeset(&self.place.root).context("its records are not read")?
        } else {
            Changeset::load(&self.changeset_path)?
        };
        self.place.ignore = self.changeset.ignore.iter().chain(&self.tree_ignore).cloned().collect();
        Ok(())
    }
}

/// The names a mounted tree's own rules leave out: its `.gitignore`, and the one at the top of the repository it lies
/// in (by name only: `changeset::gitignore_names`).
fn tree_ignores(tree: &Path) -> Vec<String> {
    let mut names = Vec::new();
    let mut read = |dir: &Path| {
        if let Ok(text) = std::fs::read_to_string(dir.join(".gitignore")) {
            names.extend(changeset::gitignore_names(&text));
        }
    };
    read(tree);
    if let Some(repository) = tree.ancestors().skip(1).find(|d| d.join(".git").exists()) {
        read(repository);
    }
    names
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
    moved: Option<usize>,
    /// Its files, where an override wins: those it changes - as held, or merged over the version below now - and those
    /// below it does not change; a plain file's own by an empty name.
    files: Option<Arc<Vec<(String, PathBuf, usize)>>>,
    /// The files its overrides' merges read (theirs below, and their own), for a change to any to merge again.
    inputs: Arc<Vec<PathBuf>>,
}

impl Node {
    /// An entry as the layer creating it left it.
    fn created(kind: Kind, uid: &str, home: usize, path: &str) -> Node {
        Node {
            kind,
            uid: uid.to_string(),
            home,
            content: (home, path.to_string()),
            overrides: vec![],
            moved: None,
            files: None,
            inputs: Arc::default(),
        }
    }
}

/// The stack applied: the resolved entries, what is wrong, and each layer's operations with their status and the path
/// their entry had.
struct Resolution {
    resolved: BTreeMap<String, Node>,
    problems: Vec<Problem>,
    ops: Vec<BTreeMap<String, (OpStatus, Option<String>)>>,
}

/// The asset a path relative to a layer's root lies in: the path up to its first `.node` component, without the
/// suffix. None outside any bundle.
fn bundle_of(rel: &str) -> Option<String> {
    let parts: Vec<&str> = rel.split('/').collect();
    let at = parts.iter().position(|p| p.ends_with(BUNDLE_SUFFIX))?;
    let mut asset = parts[..=at].join("/");
    asset.truncate(asset.len() - BUNDLE_SUFFIX.len());
    Some(asset)
}

/// The reference a value holds, if it holds one: its asset's uid, and the path it names where it names one. A
/// reference is the asset's uid, `"uid://..."` - with an output of it after a `#` (`#palettes/default`), which
/// this leaves out (`part_of` gives it) -, or, as written before uids sufficed, a table of the uid and the bundle's
/// `res://` path.
#[must_use]
pub fn reference(value: &toml::Value) -> Option<(String, Option<String>)> {
    match value {
        toml::Value::String(s) if s.starts_with("uid://") => {
            Some((s.split_once('#').map_or(s.as_str(), |(uid, _)| uid).to_string(), None))
        }
        toml::Value::Table(t) => {
            let uid = t.get("uid")?.as_str()?;
            let rel = t.get("path")?.as_str()?.strip_prefix("res://")?;
            Some((uid.to_string(), Some(rel.trim_end_matches(BUNDLE_SUFFIX).to_string())))
        }
        _ => None,
    }
}

/// Every reference in a value (`reference`), named by the enclosing slot's `name` or else the field it sits in.
fn references(value: &toml::Value, field: &str, out: &mut Vec<(String, String, String, String)>) {
    match value {
        toml::Value::Table(t) => {
            if let Some((uid, path)) = reference(value) {
                out.push((
                    field.to_string(),
                    uid,
                    path.unwrap_or_default(),
                    pe2_ext::output_of(value).unwrap_or_default().to_string(),
                ));
                return;
            }
            let name = t.get("name").and_then(toml::Value::as_str);
            for (key, v) in t.iter().filter(|(key, _)| *key != "uid") {
                references(v, name.unwrap_or(key), out);
            }
        }
        toml::Value::Array(a) => a.iter().for_each(|v| references(v, field, out)),
        toml::Value::String(_) => {
            if let Some((uid, _)) = reference(value) {
                out.push((
                    field.to_string(),
                    uid,
                    String::new(),
                    pe2_ext::output_of(value).unwrap_or_default().to_string(),
                ));
            }
        }
        _ => {}
    }
}

/// The paths at or under `path`.
fn subtree<V>(map: &BTreeMap<String, V>, path: &str) -> Vec<String> {
    let prefix = format!("{path}/");
    map.keys().filter(|p| *p == path || p.starts_with(&prefix)).cloned().collect()
}

/// How far a long piece of work is: its stage, and how many of how many it has done (both 0 where it cannot say).
pub type Report<'a> = dyn Fn(&str, usize, usize) + 'a;

#[derive(Clone)]
pub struct Index {
    /// The resolved assets, by path: shared with the snapshots made of the index, copied where changed.
    pub assets: BTreeMap<String, Arc<Asset>>,
    /// The versions overridden assets have below them, by `below:<uid>`, and those below that, by `below:below:<uid>`:
    /// nodes of their own, read through the graph's `below` operation.
    versions: HashMap<String, Arc<Asset>>,
    /// Every resolved entry - assets, folders, plain files - by path. This and the maps below are shared with the
    /// snapshots made of the index, copied when a take-in changes them.
    pub entries: Arc<BTreeMap<String, Entry>>,
    /// How many files the last opening or refresh read, rather than took from what it knew.
    pub read: usize,
    pub by_uid: Arc<HashMap<String, String>>,
    /// For each asset, the references naming it.
    pub referenced_by: Arc<HashMap<String, Vec<Reference>>>,
    /// References whose uid or path names no asset of the project.
    pub dangling: Vec<Reference>,
    pub problems: Vec<Problem>,
    layers: Vec<LayerState>,
    /// Each file's record, shared with the snapshots made of the index.
    records: HashMap<PathBuf, Arc<Record>>,
    /// Where the records are kept between runs: none while another process has it (`let_go_of_store`).
    store: Option<Arc<Store>>,
    resolved: Arc<BTreeMap<String, Node>>,
    /// The layers left out of the view (the editor's eyes); problems and statuses are the full stack's.
    hidden: HashSet<usize>,
    ops: Vec<BTreeMap<String, (OpStatus, Option<String>)>>,
    dependencies: Vec<BTreeMap<usize, (usize, usize)>>,
    /// The content hash of each entry a writable layer holds, as of the last pass: what tells a move made outside
    /// the editor from a deletion and a new entry.
    hashes: Arc<Vec<HashMap<String, String>>>,
    /// The resolved path of each entry by where its winning content is (a layer and its path there): what a change
    /// of a layer's content touches.
    winning: Arc<HashMap<(usize, String), String>>,
    /// The working copies of assets with unsaved changes, by uid: shown in place of their files on disk.
    working: Arc<HashMap<String, Working>>,
    /// Where what overrides merged is kept (`merged_file`), and what each merge made (`Memo`).
    merged: PathBuf,
    /// The store keeping the versions overrides were made from (`changeset::Base`).
    objects: crate::chunks::Store,
}

impl Index {
    /// Whether a layer leaves a path out (an extension's folder, a name it ignores): no asset, whatever is on disk.
    #[must_use]
    pub fn leaves_out(&self, layer: &str, path: &str) -> bool {
        self.layers.iter().find(|l| l.name == layer).is_some_and(|l| l.place.ignores(path))
    }

    /// Index a project's layers, through its cache: only files whose stamp changed are read. Unlisted entries of the
    /// layers that can be written are listed in their changesets.
    ///
    /// # Errors
    /// A changeset cannot be read or written, or the cache cannot be written.
    pub fn open(project: &Project, cache: &Cache) -> Result<Index> {
        Index::open_reporting(project, cache, &|_, _, _| {})
    }

    /// Open the index as `open` does, saying how far it is: a stage, and how many of how many it has done.
    ///
    /// # Errors
    /// As `open`.
    pub fn open_reporting(project: &Project, cache: &Cache, report: &Report<'_>) -> Result<Index> {
        let _span = tracing::info_span!("index open").entered();
        report("Reading what the cache knows", 0, 0);
        Index::over(Some(Arc::new(Store::open(&cache.path(STORE))?)), project, report)
    }

    /// Open the index without the cache's store, which another process has: every file read, and its records kept in
    /// memory alone (until `keep_records`).
    ///
    /// # Errors
    /// A layer cannot be read.
    pub fn without_store(project: &Project) -> Result<Index> {
        Index::over(None, project, &|_, _, _| {})
    }

    /// Stop keeping the records in the cache's store, and close it, for another process to open: what is read from now
    /// on is kept in memory alone, until `keep_records`.
    pub fn let_go_of_store(&mut self) {
        self.store = None;
    }

    /// Keep the records in the cache's store again, opened anew: what it has is the last holder's, and what changes
    /// from now on is written to it.
    ///
    /// # Errors
    /// The store cannot be opened.
    pub fn keep_records(&mut self, cache: &Cache) -> Result<()> {
        self.store = Some(Arc::new(Store::open(&cache.path(STORE))?));
        Ok(())
    }

    /// The index of the project as `project.toml` has it now, opened over this one's store, which stays open; this
    /// one goes on answering until it is let go.
    ///
    /// # Errors
    /// A layer or the store cannot be read.
    pub fn again(&self, project: &Project, report: &Report<'_>) -> Result<Index> {
        let mut index = Index::over(self.store.clone(), project, report)?;
        index.set_working(self.working.as_ref().clone());
        Ok(index)
    }

    fn over(store: Option<Arc<Store>>, project: &Project, report: &Report<'_>) -> Result<Index> {
        let mut known = store.as_ref().map(|s| s.load()).transpose()?.unwrap_or_default();
        // Its records layers, placed through the stack: what their scans and changesets are read from.
        crate::records::Set::open(project)?;
        let mut layers = Vec::new();
        for (i, layer) in project.layers.iter().enumerate() {
            report(&format!("Scanning {}", layer.name), i, project.layers.len());
            layers.push(LayerState::new(layer)?);
        }
        let mut index = Index {
            assets: BTreeMap::new(),
            versions: HashMap::new(),
            entries: Arc::default(),
            read: 0,
            by_uid: Arc::default(),
            referenced_by: Arc::default(),
            dangling: vec![],
            problems: vec![],
            layers,
            records: HashMap::new(),
            merged: project.root.join(crate::cache::DIR).join("merged"),
            objects: crate::chunks::Store::at(&project.root),
            store,
            resolved: Arc::default(),
            hidden: HashSet::new(),
            ops: vec![],
            dependencies: vec![],
            hashes: Arc::default(),
            winning: Arc::default(),
            working: Arc::default(),
        };
        let files: Vec<PathBuf> = index.layers.iter().flat_map(|l| l.scan.all_files().cloned()).collect();
        report("Reading the files that changed", 0, files.len());
        let read = index.load_records(&files, |path, size, modified| {
            known.remove(path).filter(|r| r.size == size && r.modified == modified && modified < r.recorded)
        });
        index.read = read.len();
        // What is left of what the store knew no layer holds any more.
        let gone: Vec<&Path> = known.keys().map(PathBuf::as_path).collect();
        if let Some(store) = &index.store {
            store.write(read.iter().map(|p| (p.as_path(), &*index.records[p])), gone)?;
        }
        report("Resolving the layers", 0, 0);
        index.apply_stack()?;
        report("Assembling the assets", 0, 0);
        let paths: Vec<String> = index.resolved.keys().cloned().collect();
        index.assemble(&paths);
        index.link();
        Ok(index)
    }

    /// Bring the index up to date with paths a watcher reports changed: what they lie in is scanned again (a file
    /// reported is read again whatever its stamp), a changeset reported is read again, and the assets whose entry or
    /// content changed are rebuilt. It costs what changed: where only contents did - the layers hold the same paths,
    /// no changeset was read again, and each content changed is its entry's winning one - the stack is not applied
    /// again, and only the changed assets are assembled and linked; otherwise the stack is applied anew.
    /// `project.toml` is only noticed: a change to the layers themselves needs an index made again (`again`).
    ///
    /// # Errors
    /// A changeset cannot be read or written.
    pub fn refresh(&mut self, project: &Project, changed: &[PathBuf]) -> Result<Refreshed> {
        // A manifest changing is the file it stands for changing; a packed layer's listing, its whole layer.
        let changed: Vec<PathBuf> = changed
            .iter()
            .map(|p| match p.file_name() {
                Some(name) if name == crate::pack::FILE => p.parent().map_or_else(|| p.clone(), Path::to_path_buf),
                _ => crate::chunks::logical(p).unwrap_or_else(|| p.clone()),
            })
            .collect();
        self.refresh_files(project, &changed)
    }

    /// Records changed: the project's are read again, and each records layer as they make it now, rescanned whole.
    /// Whether any records layer was.
    fn take_in_records(&mut self, project: &Project, wanted: &mut Vec<(usize, String)>) -> Result<bool> {
        crate::records::Set::open(project)?;
        let mut any = false;
        for (i, layer) in self.layers.iter_mut().enumerate() {
            if crate::records::is_records(&layer.place.root) {
                layer.reload_changeset()?;
                any = true;
                wanted.push((i, String::new()));
            }
        }
        Ok(any)
    }

    fn refresh_files(&mut self, project: &Project, changed: &[PathBuf]) -> Result<Refreshed> {
        let mut refreshed = Refreshed::default();
        let mut changesets = false;
        let mut wanted: Vec<(usize, String)> = Vec::new();
        if changed.iter().any(|p| in_records(p)) {
            changesets |= self.take_in_records(project, &mut wanted)?;
        }
        for path in changed {
            // `project.toml` is read again whole, records with their layers (above).
            if *path == project.root.join(FILE) || in_records(path) {
                refreshed.project_file |= *path == project.root.join(FILE);
                continue;
            }
            for (i, layer) in self.layers.iter_mut().enumerate() {
                if *path == layer.changeset_path {
                    layer.reload_changeset()?;
                    changesets = true;
                    continue;
                }
                let Some(rel) = layer.place.resolved(path) else { continue };
                // A change inside a bundle rescans the bundle; anything else, what is at the path (the whole layer at
                // a mount's point).
                let bundle = if layer.place.assets { bundle_of(&rel) } else { None };
                let at = bundle.map_or(rel, |asset| format!("{asset}{BUNDLE_SUFFIX}"));
                wanted.push((i, if at == layer.place.at { String::new() } else { at }));
            }
        }
        // Each region once: one within another of its layer is scanned with it.
        let within = |a: &str, b: &str| b.is_empty() || a == b || a.starts_with(&format!("{b}/"));
        wanted.sort();
        wanted.dedup();
        let regions: Vec<(usize, String)> = wanted
            .iter()
            .filter(|(i, a)| !wanted.iter().any(|(j, b)| j == i && b != a && within(a, b)))
            .cloned()
            .collect();
        // What the entries changed declare now, before their records are read again: what a link takes back.
        let mut before_references = HashMap::new();
        for (i, at) in &regions {
            for path in self.layers[*i].scan.under(at).bundles.keys() {
                if let Some(resolved) = self.winning.get(&(self.entries_of(*i), path.clone())) {
                    let asset = asset_of(resolved).to_string();
                    before_references.insert(asset.clone(), self.references_from(&asset));
                }
            }
        }
        let mut gone = Vec::new();
        let mut files = Vec::new();
        let mut paths_same = true;
        for (i, at) in &regions {
            let layer = &mut self.layers[*i];
            let before = layer.scan.under(at);
            Arc::make_mut(&mut layer.scan).rescan(&layer.place, at);
            let after = layer.scan.under(at);
            paths_same &= before.same_paths(&after);
            let now: HashSet<&PathBuf> = after.all_files().collect();
            for file in before.all_files().filter(|f| !now.contains(f)) {
                let elsewhere = self.layers.iter().enumerate().any(|(j, l)| j != *i && l.scan.holds(&l.place, file));
                if !elsewhere {
                    gone.push(file.clone());
                }
            }
            files.extend(after.all_files().cloned());
        }
        for file in &gone {
            self.records.remove(file);
        }
        let reported: HashSet<&PathBuf> = changed.iter().collect();
        let (read, vanished) = self.reload(&files, &reported);
        gone.extend(vanished);
        self.read = read.len();
        let records = read.iter().filter_map(|p| Some((p.as_path(), &**self.records.get(p)?)));
        if let Some(store) = &self.store {
            store.write(records, gone.iter().map(PathBuf::as_path))?;
        }
        // An override's files are merged as the stack is applied: one of them, or what is below it, changed applies it
        // again.
        let inputs: HashSet<&PathBuf> = self.resolved.values().flat_map(|n| n.inputs.iter()).collect();
        let patched = changed.iter().any(|p| inputs.contains(p));
        if paths_same && !changesets && !patched && self.contents_only(&regions) {
            self.refresh_contents(&regions, &before_references, &mut refreshed);
            return Ok(refreshed);
        }
        let rescanned: Vec<PathBuf> = regions.iter().map(|(i, at)| self.layers[*i].place.disk(at)).collect();
        refreshed.restacked = true;
        let before = std::mem::take(&mut self.resolved);
        self.apply_stack()?;
        // An asset changed when its entry did, or when its content - or what a generated layer gives it - lies in what
        // was rescanned.
        for (path, node) in self.resolved.iter() {
            let content = self.layers[node.content.0].place.disk(&node.content.1);
            let filled = self.filled_by(node.content.0).map(|g| self.layers[g].place.disk(&node.content.1));
            let through = node.files.iter().flat_map(|f| f.iter().map(|(_, file, _)| file));
            let touched = std::iter::once(&content)
                .chain(filled.as_ref())
                .chain(through)
                .any(|c| rescanned.iter().any(|r| c.starts_with(r) || r.starts_with(c)));
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

    /// A refresh where only contents changed, each its entry's winning one: the entries stand; the changed assets
    /// are assembled and linked again, the hashes of their writable layers kept, the dependencies counted again only
    /// where their references changed.
    fn refresh_contents(
        &mut self,
        regions: &[(usize, String)],
        before: &HashMap<String, Vec<Reference>>,
        refreshed: &mut Refreshed,
    ) {
        let mut paths = Vec::new();
        for (i, at) in regions {
            let scan = self.layers[*i].scan.under(at);
            if self.layers[*i].writable {
                for (path, kind) in
                    scan.bundles.keys().map(|p| (p, Kind::Asset)).chain(scan.files.keys().map(|p| (p, Kind::File)))
                {
                    if let Some(hash) = self.content_hash(*i, path, kind) {
                        Arc::make_mut(&mut self.hashes)[*i].insert(path.clone(), hash);
                    }
                }
            }
            let entries = self.entries_of(*i);
            for path in scan.bundles.keys() {
                if let Some(resolved) = self.winning.get(&(entries, path.clone())) {
                    paths.push(resolved.clone());
                }
            }
        }
        self.assemble_assets(&paths);
        let assets: Vec<String> = paths.iter().map(|p| asset_of(p).to_string()).collect();
        let referring = |r: &Reference| (r.slot.clone(), r.uid.clone(), r.target.clone(), r.output.clone());
        let references_changed = assets.iter().any(|a| {
            let now: Vec<_> = self.references_from(a).iter().map(referring).collect();
            before.get(a).is_none_or(|b| b.iter().map(referring).collect::<Vec<_>>() != now)
        });
        self.relink(&assets, before);
        if references_changed {
            self.dependencies = self.dependencies();
        }
        refreshed.assets.extend(assets);
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
                self.records.insert(file.clone(), Arc::new(record));
            } else {
                let declaration = file.file_name().is_some_and(|n| n == NODE_FILE || n == SETTINGS_FILE)
                    && file.parent().is_some_and(|d| d.to_string_lossy().ends_with(BUNDLE_SUFFIX));
                pending.push(Pending { path: file.clone(), size, modified, declaration });
            }
        }
        let mut read = Vec::new();
        for (file, record) in pending.iter().zip(read_all(&pending)) {
            if let Some(record) = record {
                self.records.insert(file.path.clone(), Arc::new(record));
                read.push(file.path.clone());
            }
        }
        read
    }

    /// Stat the files and read again those whose stamp changed or that were reported; returns those read, and those
    /// gone since they were scanned.
    fn reload(&mut self, files: &[PathBuf], reported: &HashSet<&PathBuf>) -> (Vec<PathBuf>, Vec<PathBuf>) {
        let (mut pending, mut vanished) = (Vec::new(), Vec::new());
        for (file, (size, modified)) in files.iter().zip(stat_all(files)) {
            let Some(size) = size else {
                self.records.remove(file);
                vanished.push(file.clone());
                continue;
            };
            let kept = !reported.contains(file)
                && self.records.get(file).is_some_and(|r| r.size == size && r.modified == modified);
            if !kept {
                let declaration = file.file_name().is_some_and(|n| n == NODE_FILE || n == SETTINGS_FILE)
                    && file.parent().is_some_and(|d| d.to_string_lossy().ends_with(BUNDLE_SUFFIX));
                pending.push(Pending { path: file.clone(), size, modified, declaration });
            }
        }
        let mut read = Vec::new();
        for (file, record) in pending.iter().zip(read_all(&pending)) {
            if let Some(record) = record {
                self.records.insert(file.path.clone(), Arc::new(record));
                read.push(file.path.clone());
            }
        }
        (read, vanished)
    }

    /// The generated layer filling a layer's nodes, where one does.
    fn filled_by(&self, layer: usize) -> Option<usize> {
        let name = &self.layers[layer].name;
        self.layers.iter().position(|l| l.fills.as_ref() == Some(name))
    }

    /// The layer a layer's bundles are entries of: a generated layer's, the layer it fills; any other's, its own.
    fn entries_of(&self, layer: usize) -> usize {
        let fills = self.layers[layer].fills.as_ref();
        fills.and_then(|f| self.layers.iter().position(|l| l.name == *f)).unwrap_or(layer)
    }

    /// A bundle's files in a layer, each with the layer holding it: its own, then what the generated layer filling the
    /// layer holds at its path - a node's declaration in one, its files in the other. None where the layer holds no
    /// bundle there.
    fn bundle_files(&self, layer: usize, path: &str) -> Option<Vec<(String, PathBuf, usize)>> {
        let own = self.layers[layer].scan.bundles.get(path)?;
        let mut files: Vec<(String, PathBuf, usize)> = own.iter().map(|(n, f)| (n.clone(), f.clone(), layer)).collect();
        if let Some(filler) = self.filled_by(layer)
            && let Some(filled) = self.layers[filler].scan.bundles.get(path)
        {
            for (name, file) in filled {
                if !own.contains_key(name) {
                    files.push((name.clone(), file.clone(), filler));
                }
            }
            files.sort_by(|a, b| a.0.cmp(&b.0));
        }
        Some(files)
    }

    /// The hash of an entry's content in a layer: a plain file's own, a bundle's over its files' names and hashes - what
    /// a generated layer gives it with them; a folder has none.
    fn content_hash(&self, layer: usize, path: &str, kind: Kind) -> Option<String> {
        let scan = &self.layers[layer].scan;
        match kind {
            Kind::File => scan.files.get(path).and_then(|f| self.records.get(f)).map(|r| format!("sha1:{}", r.sha1)),
            Kind::Asset => {
                let files = self.bundle_files(layer, path)?;
                let mut listing = String::new();
                for (name, f, _) in &files {
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

    /// A resolved entry's files: as its overrides make them, else its content's - a plain file's own by an empty name.
    fn node_files(&self, node: &Node) -> Vec<(String, PathBuf, usize)> {
        match (&node.files, node.kind) {
            (Some(files), _) => files.as_ref().clone(),
            (None, Kind::Asset) => self.bundle_files(node.content.0, &node.content.1).unwrap_or_default(),
            (None, Kind::File) => {
                let file = self.layers[node.content.0].scan.files.get(&node.content.1);
                file.map_or_else(Vec::new, |f| vec![(String::new(), f.clone(), node.content.0)])
            }
            (None, Kind::Folder) => vec![],
        }
    }

    /// A file's SHA-1: as recorded, or, for one merged in the cache, the name of its folder.
    fn sha1_of(&self, path: &Path) -> Option<String> {
        if let Some(record) = self.records.get(path) {
            return Some(record.sha1.clone());
        }
        let folder = path.parent().filter(|f| f.parent() == Some(self.merged.as_path()))?;
        folder.file_name()?.to_str().map(str::to_string)
    }

    /// The hash of a resolved entry's content: a plain file's own, a bundle's over its files' names and hashes.
    fn node_hash(&self, node: &Node) -> Option<String> {
        match node.kind {
            Kind::Asset => {
                let mut listing = String::new();
                for (name, f, _) in &self.node_files(node) {
                    let sha1 = self.sha1_of(f).unwrap_or_default();
                    listing.push_str(name);
                    listing.push(' ');
                    listing.push_str(&sha1);
                    listing.push('\n');
                }
                Some(format!("sha1:{}", sha1_hex(listing.as_bytes())))
            }
            Kind::File => {
                let files = self.node_files(node);
                files.first().and_then(|(_, f, _)| self.sha1_of(f)).map(|sha1| format!("sha1:{sha1}"))
            }
            Kind::Folder => None,
        }
    }

    /// An entry's content as a layer's override of it at `at` makes it: the files it changes (`changed`) as it holds them,
    /// merged over the version below now where that is no longer the one it was made from, or removed; the rest as below.
    /// Whether it is stale (something it changes changed below, otherwise or since), or none where a file it holds is
    /// missing.
    fn override_with(&self, node: &mut Node, layer: usize, at: &str, changed: &BTreeMap<String, Base>) -> Option<bool> {
        let below = self.node_files(node);
        let own: Vec<(String, PathBuf, usize)> = match node.kind {
            Kind::Asset => self.bundle_files(layer, at).unwrap_or_default(),
            Kind::File => self.layers[layer].scan.files.get(at).map(|f| vec![(String::new(), f.clone(), layer)])?,
            Kind::Folder => return None,
        };
        let mut files: BTreeMap<String, (PathBuf, usize)> =
            below.iter().map(|(name, file, held)| (name.clone(), (file.clone(), *held))).collect();
        let (mut inputs, mut stale) = (Vec::new(), false);
        for (name, base) in changed {
            let now = below.iter().find(|(n, ..)| n == name).map(|(_, file, _)| file);
            let now_sha1 = now.and_then(|f| self.sha1_of(f));
            if base.removed {
                files.remove(name);
                stale |= now_sha1 != base.was;
                continue;
            }
            let (_, mine, held) = own.iter().find(|(n, ..)| n == name)?;
            inputs.push(mine.clone());
            inputs.extend(now.cloned());
            let made = match (&base.was, now) {
                (was, _) if *was == now_sha1 => mine.clone(),
                (Some(was), Some(now)) => {
                    if let Some((merged, conflicts)) = self.merged(&file_name(name, at), was, now, mine) {
                        stale |= !conflicts.is_empty();
                        merged
                    } else {
                        // What it was made from is not kept: it is stale, as it holds it.
                        stale = true;
                        mine.clone()
                    }
                }
                // Added below since, or removed there: as it holds it, stale.
                _ => {
                    stale = true;
                    mine.clone()
                }
            };
            files.insert(name.clone(), (made, *held));
        }
        // What it holds and does not list it holds whole, as it is.
        for (name, file, held) in &own {
            if !changed.contains_key(name) {
                files.insert(name.clone(), (file.clone(), *held));
            }
        }
        node.files = Some(Arc::new(files.into_iter().map(|(name, (file, held))| (name, file, held)).collect()));
        node.inputs = Arc::new(inputs);
        Some(stale)
    }

    /// Apply a layer's override of an entry at `at` (`override_with`), the layer holding its content where it holds any
    /// of it: whether it is stale, or none where what it holds is missing, or is no such entry.
    fn apply_override(
        &self,
        node: &mut Node,
        layer: usize,
        at: &str,
        changed: &BTreeMap<String, Base>,
    ) -> Option<bool> {
        let held = self.layers[layer].scan.kind(at);
        if held.is_some_and(|k| k != node.kind) {
            return None;
        }
        let stale = self.override_with(node, layer, at, changed)?;
        if held.is_some() {
            node.content = (layer, at.to_string());
        }
        node.overrides.push(layer);
        Some(stale)
    }

    /// `mine`, made of the version `was` (kept in the store), merged over `now`: where the result is - `mine` itself
    /// where it is unchanged, else in the cache - and its conflicts; none where `was` is not kept, or a file cannot be
    /// read. A merge is remembered by its inputs, so the stack is applied again without merging again.
    fn merged(&self, name: &str, was: &str, now: &Path, mine: &Path) -> Option<(PathBuf, Vec<crate::merge::Conflict>)> {
        let (now_sha1, mine_sha1) = (self.sha1_of(now)?, self.sha1_of(mine)?);
        let key = sha1_hex(format!("{name}\n{was}\n{now_sha1}\n{mine_sha1}").as_bytes());
        let memo = self.merged.join("memo").join(&key);
        let at = |sha1: &str| {
            if sha1 == mine_sha1 { mine.to_path_buf() } else { self.merged.join(sha1).join(name) }
        };
        if let Ok(text) = std::fs::read_to_string(&memo)
            && let Ok(remembered) = serde_json::from_str::<Memo>(&text)
            && (remembered.sha1 == mine_sha1 || at(&remembered.sha1).is_file())
        {
            return Some((at(&remembered.sha1), remembered.conflicts));
        }
        let base = self.objects.get_file(was).ok()?;
        let theirs = crate::content::read(now).ok()?;
        let ours = crate::content::read(mine).ok()?;
        let merged = crate::merge::merge(name, &base, &theirs, &ours);
        let sha1 = sha1_hex(&merged.bytes);
        let path =
            if sha1 == mine_sha1 { mine.to_path_buf() } else { merged_file(&self.merged, name, &merged.bytes).ok()? };
        let remembered = Memo { sha1, conflicts: merged.conflicts };
        if let Ok(text) = serde_json::to_string(&remembered) {
            let _ = std::fs::create_dir_all(memo.parent().expect("in a folder"));
            let _ = crate::cache::write_atomic(&memo, text.as_bytes());
        }
        Some((path, remembered.conflicts))
    }

    /// The hash an override of an entry (by its real path) records as its base: the content it is made from, the
    /// version that wins directly below the layer making it.
    #[must_use]
    pub fn base_hash(&self, path: &str) -> Option<String> {
        self.node_hash(self.resolved.get(path)?)
    }

    /// Apply the stack: each layer's creates, then its operations, bottom to top. What a writable layer holds unlisted
    /// is listed in its changeset first (written), so applying never meets it again. Problems, statuses and
    /// dependencies are the whole stack's; the view leaves out the hidden layers.
    fn apply_stack(&mut self) -> Result<()> {
        self.list_unlisted()?;
        let full = self.resolve_stack(&HashSet::new());
        self.problems = full.problems;
        self.ops = full.ops;
        self.resolved =
            Arc::new(if self.hidden.is_empty() { full.resolved } else { self.resolve_stack(&self.hidden).resolved });
        self.dependencies = self.dependencies();
        self.remember_hashes();
        self.winning =
            Arc::new(self.resolved.iter().map(|(path, node)| (node.content.clone(), path.clone())).collect());
        Ok(())
    }

    /// Whether what changed in these regions is contents alone: every bundle and plain file of them is its entry's
    /// winning content, and no layer is hidden.
    fn contents_only(&self, regions: &[(usize, String)]) -> bool {
        self.hidden.is_empty()
            && regions.iter().all(|(i, at)| {
                let scan = self.layers[*i].scan.under(at);
                let entries = self.entries_of(*i);
                scan.bundles.keys().all(|p| self.winning.contains_key(&(entries, p.clone())))
                    && (scan.files.is_empty() || entries == *i)
                    && scan.files.keys().all(|p| self.winning.contains_key(&(*i, p.clone())))
            })
    }

    /// The stack applied without the layers in `skip`.
    fn resolve_stack(&self, skip: &HashSet<usize>) -> Resolution {
        let mut resolved: BTreeMap<String, Node> = BTreeMap::new();
        let mut where_is: HashMap<String, String> = HashMap::new();
        let mut problems = Vec::new();
        let mut statuses = vec![BTreeMap::new(); self.layers.len()];
        for (i, layer) in self.layers.iter().enumerate() {
            // A generated layer makes no entries: what it holds is the files of another's.
            if skip.contains(&i) || layer.kind == LayerKind::Generated {
                continue;
            }
            let status = &mut statuses[i];
            let mut accounted: HashSet<String> = HashSet::new();
            for (uid, path) in &layer.changeset.entries {
                let Some(kind) = layer.scan.kind(path) else {
                    let left_out = layer.place.ignores(path);
                    problems.push(Problem::Missing { layer: layer.name.clone(), path: path.clone(), left_out });
                    continue;
                };
                accounted.insert(path.clone());
                if let Some(other) = resolved.get(path) {
                    // A folder has no content: two layers making one are adding to the same place, its first uid and its
                    // second naming it alike.
                    if kind == Kind::Folder && other.kind == Kind::Folder {
                        where_is.insert(uid.clone(), path.clone());
                        continue;
                    }
                    let layers = vec![self.layers[other.home].name.clone(), layer.name.clone()];
                    problems.push(Problem::Conflict { path: path.clone(), layers });
                    continue;
                }
                if let Some(other) = where_is.get(uid) {
                    problems.push(Problem::DuplicateUid { uid: uid.clone(), paths: vec![other.clone(), path.clone()] });
                    continue;
                }
                resolved.insert(path.clone(), Node::created(kind, uid, i, path));
                where_is.insert(uid.clone(), path.clone());
            }
            for (uid, op) in &layer.changeset.ops {
                let Some(path) = where_is.get(uid).cloned() else {
                    problems.push(Problem::Unbound { layer: layer.name.clone(), uid: uid.clone() });
                    status.insert(uid.clone(), (OpStatus::Unbound, None));
                    continue;
                };
                let mut outcome = OpStatus::Applied;
                let to = match op {
                    Op::Delete => {
                        for p in subtree(&resolved, &path) {
                            if let Some(node) = resolved.remove(&p) {
                                where_is.remove(&node.uid);
                            }
                        }
                        status.insert(uid.clone(), (outcome, Some(path)));
                        continue;
                    }
                    Op::Move { to } | Op::Override { to: Some(to), .. } => Some(to),
                    Op::Override { to: None, .. } => None,
                };
                let at = match to {
                    Some(to) => {
                        for p in subtree(&resolved, &path) {
                            let target = format!("{to}{}", &p[path.len()..]);
                            if resolved.contains_key(&target) {
                                problems.push(Problem::Conflict { path: target, layers: vec![layer.name.clone()] });
                                outcome = OpStatus::Conflict;
                                continue;
                            }
                            let mut node = resolved.remove(&p).expect("listed by subtree");
                            if p == path {
                                node.moved = Some(i);
                            }
                            where_is.insert(node.uid.clone(), target.clone());
                            resolved.insert(target, node);
                        }
                        to.clone()
                    }
                    None => path.clone(),
                };
                if let Op::Override { files: changed, .. } = op
                    && let Some(node) = resolved.get_mut(&at)
                {
                    let Some(stale) = self.apply_override(node, i, &at, changed) else {
                        problems.push(Problem::Missing {
                            layer: layer.name.clone(),
                            path: at.clone(),
                            left_out: false,
                        });
                        status.insert(uid.clone(), (OpStatus::Missing, Some(path)));
                        continue;
                    };
                    accounted.insert(at.clone());
                    if stale {
                        problems.push(Problem::Stale { layer: layer.name.clone(), path: at.clone() });
                        outcome = OpStatus::Stale;
                    }
                }
                status.insert(uid.clone(), (outcome, Some(path)));
            }
            self.check_held(i, &accounted, &resolved, &mut problems);
        }
        Resolution { resolved, problems, ops: statuses }
    }

    /// What a layer holds that its creates and overrides do not account for: content at a path an entry from below
    /// resolves to, without an override, is a conflict (a folder there is only the structure its own entries need);
    /// anything else is unlisted.
    fn check_held(
        &self,
        i: usize,
        accounted: &HashSet<String>,
        resolved: &BTreeMap<String, Node>,
        problems: &mut Vec<Problem>,
    ) {
        let layer = &self.layers[i];
        for path in layer.scan.bundles.keys().chain(layer.scan.files.keys()) {
            if !accounted.contains(path)
                && let Some(node) = resolved.get(path)
                && node.home != i
            {
                let layers = vec![self.layers[node.home].name.clone(), layer.name.clone()];
                problems.push(Problem::Conflict { path: path.clone(), layers });
            }
        }
        // What it holds unlisted, by the topmost path of each tree of it: a folder of a hundred files is one problem.
        let unlisted: BTreeSet<&String> =
            layer.scan.paths().filter(|p| !accounted.contains(*p) && !resolved.contains_key(*p)).collect();
        for path in &unlisted {
            let parent = path.rsplit_once('/').map(|(p, _)| p.to_string());
            if !parent.is_some_and(|p| unlisted.contains(&p)) {
                problems.push(Problem::Unlisted { layer: layer.name.clone(), path: (*path).clone() });
            }
        }
    }

    /// What each layer needs of the layers it is not: operations on the entries they create, references to their
    /// assets - by the creating layer, with how many of each.
    fn dependencies(&self) -> Vec<BTreeMap<usize, (usize, usize)>> {
        let mut creator: HashMap<&str, usize> = HashMap::new();
        for (i, layer) in self.layers.iter().enumerate() {
            for uid in layer.changeset.entries.keys() {
                creator.entry(uid.as_str()).or_insert(i);
            }
        }
        let mut out = vec![BTreeMap::new(); self.layers.len()];
        for (i, layer) in self.layers.iter().enumerate() {
            for uid in layer.changeset.ops.keys() {
                if let Some(&j) = creator.get(uid.as_str()).filter(|&&j| j != i) {
                    out[i].entry(j).or_insert((0, 0)).0 += 1;
                }
            }
            for files in layer.scan.bundles.values() {
                let declaration =
                    files.get(NODE_FILE).and_then(|f| self.records.get(f)).and_then(|r| r.declaration.as_ref());
                for (_, uid, _, _) in declaration.map_or(&[][..], |d| d.references.as_slice()) {
                    if let Some(&j) = creator.get(uid.as_str()).filter(|&&j| j != i) {
                        out[i].entry(j).or_insert((0, 0)).1 += 1;
                    }
                }
            }
        }
        out
    }

    /// A layer's changeset as the index read it.
    #[must_use]
    pub fn changeset(&self, layer: &str) -> Option<&Changeset> {
        self.layers.iter().find(|l| l.name == layer).map(|l| &l.changeset)
    }

    /// The stack resolved without a layer and those above it: what that layer stands on.
    fn below_layer(&self, layer: &str) -> Option<Resolution> {
        let at = self.layers.iter().position(|l| l.name == layer)?;
        Some(self.resolve_stack(&(at..self.layers.len()).collect()))
    }

    /// What the layers below `layer` resolve, by path: each entry's uid and kind.
    #[must_use]
    pub fn entries_below(&self, layer: &str) -> HashMap<String, (String, Kind)> {
        let Some(resolution) = self.below_layer(layer) else { return HashMap::new() };
        resolution.resolved.iter().map(|(path, node)| (path.clone(), (node.uid.clone(), node.kind))).collect()
    }

    /// An entry as the layers below `layer` resolve it, by its uid.
    #[must_use]
    pub fn below(&self, layer: &str, uid: &str) -> Option<Below> {
        let resolution = self.below_layer(layer)?;
        let (path, node) = resolution.resolved.iter().find(|(_, n)| n.uid == uid)?;
        let source = &self.layers[node.content.0];
        let files: Vec<(String, PathBuf)> =
            self.node_files(node).into_iter().map(|(name, file, _)| (name, file)).collect();
        Some(Below {
            kind: node.kind,
            uid: uid.to_string(),
            path: path.clone(),
            hash: self.node_hash(node),
            sha1s: files.iter().filter_map(|(name, f)| Some((name.clone(), self.sha1_of(f)?))).collect(),
            files,
            layer: source.name.clone(),
        })
    }

    /// The entries the layers below `layer` resolve inside a folder (its path), by uid.
    #[must_use]
    pub fn below_inside(&self, layer: &str, folder: &str) -> Vec<(String, String)> {
        let Some(resolution) = self.below_layer(layer) else { return vec![] };
        let prefix = format!("{folder}/");
        resolution
            .resolved
            .iter()
            .filter(|(path, _)| path.starts_with(&prefix))
            .map(|(path, node)| (node.uid.clone(), path.clone()))
            .collect()
    }

    /// The stack, bottom to top, as the layer panel shows it.
    #[must_use]
    pub fn layers(&self) -> Vec<LayerInfo> {
        self.layers
            .iter()
            .enumerate()
            .map(|(i, l)| {
                let count = |f: fn(&Op) -> bool| l.changeset.ops.values().filter(|o| f(o)).count();
                let depends_on =
                    self.dependencies.get(i).into_iter().flatten().map(|(&j, &(operations, references))| Dependency {
                        layer: self.layers[j].name.clone(),
                        operations,
                        references,
                    });
                LayerInfo {
                    name: l.name.clone(),
                    kind: l.kind,
                    at: l.place.at.clone(),
                    read_only: l.read_only,
                    writable: l.writable,
                    mounted: l.mounted,
                    fills: l.fills.clone(),
                    entries: l.changeset.entries.len(),
                    overrides: count(|o| matches!(o, Op::Override { .. })),
                    moves: count(|o| matches!(o, Op::Move { .. })),
                    deletes: count(|o| matches!(o, Op::Delete)),
                    problems: self.problems.iter().filter(|p| p.layers().contains(&l.name.as_str())).count(),
                    depends_on: depends_on.collect(),
                }
            })
            .collect()
    }

    /// A layer's creates, in path order.
    #[must_use]
    pub fn creates(&self, layer: &str) -> Vec<CreateView> {
        let Some(l) = self.layers.iter().find(|l| l.name == layer) else { return vec![] };
        let mut out: Vec<CreateView> = l
            .changeset
            .entries
            .iter()
            .map(|(uid, path)| CreateView { uid: uid.clone(), path: path.clone(), kind: l.scan.kind(path) })
            .collect();
        out.sort_by(|a, b| a.path.cmp(&b.path));
        out
    }

    /// What makes a layer's override of an entry (by uid) stale: what each file it changes conflicts with below - a
    /// field, lines, a whole file - and each it removes, or holds, that changed below since otherwise.
    #[must_use]
    pub fn stale_parts(&self, layer: &str, uid: &str) -> Vec<StalePart> {
        let Some(i) = self.layers.iter().position(|l| l.name == layer) else { return vec![] };
        let Some(Op::Override { files, to }) = self.layers[i].changeset.ops.get(uid) else { return vec![] };
        let Some(below) = self.below(layer, uid) else { return vec![] };
        let at = to.clone().unwrap_or_else(|| below.path.clone());
        let mine_of = |name: &str| match below.kind {
            Kind::File => self.layers[i].scan.files.get(&at).cloned(),
            _ => self.layers[i].scan.bundles.get(&at).and_then(|b| b.get(name)).cloned(),
        };
        let whole = |file: &str, mine: &str, was: &Option<String>, now: Option<&String>| StalePart {
            file: file.to_string(),
            field: None,
            mine: Some(mine.to_string()),
            was: Some(if was.is_some() { "a version" } else { "absent" }.to_string()),
            now: Some(if now.is_some() { "another version" } else { "absent" }.to_string()),
        };
        let mut parts = Vec::new();
        for (name, base) in files {
            let shown = file_name(name, &at);
            let now = below.files.iter().find(|(n, _)| n == name).map(|(_, f)| f);
            let now_sha1 = below.sha1s.get(name);
            if now_sha1 == base.was.as_ref() {
                continue;
            }
            if base.removed {
                parts.push(whole(&shown, "removed", &base.was, now_sha1));
                continue;
            }
            let merged = base
                .was
                .as_ref()
                .zip(now)
                .zip(mine_of(name))
                .and_then(|((was, now), mine)| self.merged(&shown, was, now, &mine));
            match merged {
                Some((_, conflicts)) => parts.extend(conflicts.into_iter().map(|c| StalePart {
                    file: shown.clone(),
                    field: c.at,
                    mine: c.mine,
                    was: c.base,
                    now: c.theirs,
                })),
                None => parts.push(whole(&shown, "its own", &base.was, now_sha1)),
            }
        }
        parts
    }

    /// A layer's operations with their status, in the order of the paths they act on.
    #[must_use]
    pub fn operations(&self, layer: &str) -> Vec<OpView> {
        let Some(i) = self.layers.iter().position(|l| l.name == layer) else { return vec![] };
        let statuses = self.ops.get(i);
        let mut out: Vec<OpView> = self.layers[i]
            .changeset
            .ops
            .iter()
            .map(|(uid, op)| {
                let (status, path) = statuses.and_then(|s| s.get(uid)).cloned().unwrap_or((OpStatus::Unbound, None));
                OpView { uid: uid.clone(), op: op.clone(), path, status }
            })
            .collect();
        out.sort_by(|a, b| a.path.cmp(&b.path));
        out
    }

    /// Leave layers out of the view (the editor's eyes), or bring them back: the resolved entries and assets follow;
    /// problems and statuses stay the whole stack's. Returns whether the view changed.
    pub fn set_hidden(&mut self, names: &BTreeSet<String>) -> bool {
        let hidden: HashSet<usize> =
            self.layers.iter().enumerate().filter(|(_, l)| names.contains(&l.name)).map(|(i, _)| i).collect();
        if hidden == self.hidden {
            return false;
        }
        self.hidden = hidden;
        self.resolved = Arc::new(self.resolve_stack(&self.hidden).resolved);
        self.assets.clear();
        let paths: Vec<String> = self.resolved.keys().cloned().collect();
        self.assemble(&paths);
        self.link();
        true
    }

    /// Show assets with their working copies' files in place of those on disk - these, by uid; the others as on
    /// disk. The assets whose working copy changed are assembled and linked again; returns their paths.
    pub fn set_working(&mut self, working: HashMap<String, Working>) -> BTreeSet<String> {
        let mut uids: BTreeSet<&String> = working.keys().collect();
        uids.extend(self.working.keys());
        let changed: Vec<String> = uids
            .into_iter()
            .filter(|uid| working.get(*uid) != self.working.get(*uid))
            .filter_map(|uid| self.by_uid.get(uid).cloned())
            .collect();
        // A working copy's files are kept by their hash: read once, as any file the index records.
        let unread: Vec<Pending> = working
            .values()
            .flat_map(|w| w.files.iter())
            .filter_map(|(name, file)| Some((name, file.as_ref()?)))
            .filter(|(_, file)| !self.records.contains_key(&file.path))
            .filter_map(|(name, file)| {
                let (size, modified) = crate::content::stat(&file.path).ok()?;
                let declaration = name == NODE_FILE || name == SETTINGS_FILE;
                Some(Pending { path: file.path.clone(), size, modified: stamp(modified), declaration })
            })
            .collect();
        for (file, record) in unread.iter().zip(read_all(&unread)) {
            if let Some(record) = record {
                self.records.insert(file.path.clone(), Arc::new(record));
            }
        }
        self.working = Arc::new(working);
        if changed.is_empty() {
            return BTreeSet::new();
        }
        let before: HashMap<String, Vec<Reference>> =
            changed.iter().map(|a| (a.clone(), self.references_from(a))).collect();
        let paths: Vec<String> = changed.iter().map(|a| format!("{a}{BUNDLE_SUFFIX}")).collect();
        self.assemble_assets(&paths);
        let referring = |r: &Reference| (r.slot.clone(), r.uid.clone(), r.target.clone(), r.output.clone());
        let references_changed = changed.iter().any(|a| {
            let now: Vec<_> = self.references_from(a).iter().map(referring).collect();
            before.get(a).is_none_or(|b| b.iter().map(referring).collect::<Vec<_>>() != now)
        });
        self.relink(&changed, &before);
        if references_changed {
            self.dependencies = self.dependencies();
        }
        changed.into_iter().collect()
    }

    /// An asset as assembled from disk, with its working copy's files in their places, where it has one.
    fn with_working(&self, asset: Asset) -> Asset {
        let Some(working) = asset.uid.as_ref().and_then(|uid| self.working.get(uid)) else { return asset };
        let mut shown = asset.clone();
        for (name, file) in &working.files {
            let at = shown.files.iter().position(|f| f.name == *name);
            let layer = at.map(|i| shown.files[i].layer.clone()).or_else(|| shown.layers.last().cloned());
            if let Some(i) = at {
                shown.files.remove(i);
            }
            let Some(file) = file else { continue };
            let Some(record) = self.records.get(&file.path) else { continue };
            if name == NODE_FILE
                && let Some(d) = &record.declaration
            {
                shown.shape = Arc::from(d.shape.as_str());
                shown.variants.clone_from(&d.variants);
                shown.error.clone_from(&d.error);
                shown.text = d.text.clone();
            }
            if name == SETTINGS_FILE
                && let Some(d) = &record.declaration
            {
                shown.generated = Some(d.text.clone());
            }
            shown.files.push(BundleFile {
                name: name.clone(),
                path: file.path.clone(),
                size: record.size,
                modified: record.modified,
                layer: layer.unwrap_or_default(),
                sha1: record.sha1.clone(),
                made_by: None,
            });
        }
        shown.files.sort_by(|a, b| a.name.cmp(&b.name));
        shown.disk = Some(Arc::new(asset));
        shown
    }

    /// Keep the content hash of each entry a writable layer holds, for a later pass to tell moves - and of each it lists
    /// and no longer holds, as last known, until it is adopted somewhere else (`adoption`) or forgotten.
    fn remember_hashes(&mut self) {
        let before = self.hashes.clone();
        self.hashes = Arc::new(
            (0..self.layers.len())
                .map(|i| {
                    let layer = &self.layers[i];
                    if !layer.writable {
                        return HashMap::new();
                    }
                    let held = layer.scan.bundles.keys().map(|p| (p, Kind::Asset));
                    let held = held.chain(layer.scan.files.keys().map(|p| (p, Kind::File)));
                    let mut hashes: HashMap<String, String> =
                        held.filter_map(|(p, k)| Some((p.clone(), self.content_hash(i, p, k)?))).collect();
                    let last = before.get(i);
                    for path in layer.changeset.entries.values().filter(|p| layer.scan.kind(p).is_none()) {
                        if let Some(hash) = last.and_then(|last| last.get(path)) {
                            hashes.insert(path.clone(), hash.clone());
                        }
                    }
                    hashes
                })
                .collect(),
        );
    }

    /// The uid a layer lists an entry under, by its path in the layer.
    #[must_use]
    pub fn listed(&self, layer: &str, path: &str) -> Option<&str> {
        let layer = self.layers.iter().find(|known| known.name == layer)?;
        layer.changeset.entries.iter().find(|(_, listed)| *listed == path).map(|(uid, _)| uid.as_str())
    }

    /// List what writable layers hold unlisted, where that is how they are edited: a mounted tree (source code, worked on
    /// with its own tools), and a layer folder the first time it is indexed, with no changeset yet. A layer folder is
    /// otherwise storage, written by the editor: what it holds unlisted is a problem (`Problem::Unlisted`), which the
    /// user adopts or removes (`edit::adopt`, `edit::remove_unlisted`) - never taken as an edit. The changesets of what
    /// is listed are written.
    fn list_unlisted(&mut self) -> Result<()> {
        for i in 0..self.layers.len() {
            let layer = &self.layers[i];
            if !layer.writable || layer.listing == Listing::Adopted {
                continue;
            }
            let listing = self.listing(i, None)?;
            let layer = &mut self.layers[i];
            if layer.listing == Listing::Once {
                layer.listing = Listing::Adopted;
            }
            if listing.is_empty() {
                continue;
            }
            layer.changeset.entries.extend(listing);
            layer.changeset.save(&layer.changeset_path)?;
        }
        Ok(())
    }

    /// How a layer would list what it holds unlisted - all of it, or what is at and under the paths given - each by its
    /// uid and path. A listed entry gone from its path while an unlisted one holds the same content (by the hashes of the
    /// last pass) moved there, and keeps its uid; a folder follows when what it held moved under a new name. What is
    /// left gets a new entry, under the uid its `node.toml` names where it names one not taken, a new one otherwise. What
    /// a layer holds at a path a lower layer creates is no entry of its own (an override, or a conflict).
    ///
    /// # Errors
    /// A new uid cannot be made.
    fn listing(&self, i: usize, only: Option<&[String]>) -> Result<Vec<(String, String)>> {
        let mut taken: HashSet<String> = HashSet::new();
        for layer in &self.layers {
            taken.extend(layer.changeset.entries.keys().cloned());
        }
        let below: HashSet<&String> = self.layers[..i].iter().flat_map(|l| l.changeset.entries.values()).collect();
        let layer = &self.layers[i];
        let listed: HashSet<&String> = layer.changeset.entries.values().collect();
        let copies = self.override_targets(i);
        let wanted = |p: &str| only.is_none_or(|only| only.iter().any(|o| p == o || p.starts_with(&format!("{o}/"))));
        let mut unlisted: BTreeSet<String> = layer
            .scan
            .paths()
            .filter(|p| !listed.contains(p) && !below.contains(p) && !copies.contains(*p) && wanted(p))
            .cloned()
            .collect();
        if unlisted.is_empty() {
            return Ok(vec![]);
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
            if let Some(new) = last.get(old).and_then(|h| by_hash.get(h)).filter(|c| c.len() == 1).map(|c| c[0].clone())
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
        let mut out: Vec<(String, String)> = moves.into_iter().map(|(id, _, new)| (id, new)).collect();
        for path in unlisted {
            let declared = layer
                .scan
                .bundles
                .get(&path)
                .and_then(|files| files.get(NODE_FILE))
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
            out.push((id, path));
        }
        Ok(out)
    }

    /// How a layer would list what it holds unlisted at and under a path, each by its uid and path: its moves (a gone
    /// entry's uid, at its new path) and its new entries (`listing`).
    ///
    /// # Errors
    /// There is no such layer, or a new uid cannot be made.
    pub fn adoption(&self, layer: &str, path: &str) -> Result<Vec<(String, String)>> {
        let i = anyhow::Context::with_context(self.layers.iter().position(|l| l.name == layer), || {
            format!("no layer {layer}")
        })?;
        self.listing(i, Some(&[path.to_string()]))
    }

    /// Where a layer's overrides put their copies: each override's move target, else its entry's path below the
    /// layer. Not unlisted content: the changeset accounts for them.
    fn override_targets(&self, layer: usize) -> HashSet<String> {
        let ops = &self.layers[layer].changeset.ops;
        if !ops.values().any(|op| matches!(op, Op::Override { .. })) {
            return HashSet::new();
        }
        let below = self.resolve_stack(&(layer..self.layers.len()).collect());
        let path_of: HashMap<&str, &String> = below.resolved.iter().map(|(p, n)| (n.uid.as_str(), p)).collect();
        ops.iter()
            .filter_map(|(uid, op)| match op {
                Op::Override { to: Some(to), .. } => Some(to.clone()),
                Op::Override { to: None, .. } => path_of.get(uid.as_str()).map(|p| (*p).clone()),
                _ => None,
            })
            .collect()
    }

    /// Build the assets at these real paths from the resolved stack and the records: their uid, the winning layer's
    /// bundle, its `node.toml`. A path that no longer resolves to an asset loses its asset; `entries` follows the
    /// whole stack.
    fn assemble(&mut self, paths: &[String]) {
        self.assemble_entries();
        self.assemble_assets(paths);
    }

    /// The entries, from the resolved stack.
    fn assemble_entries(&mut self) {
        self.entries = Arc::new(
            self.resolved
                .iter()
                .map(|(path, node)| {
                    let layer = &self.layers[node.content.0];
                    let entry = Entry {
                        kind: node.kind,
                        uid: node.uid.clone(),
                        home: self.layers[node.home].name.clone(),
                        layer: layer.name.clone(),
                        disk: layer.place.disk(&node.content.1),
                        overrides: node.overrides.iter().map(|&i| self.layers[i].name.clone()).collect(),
                        moved: node.moved.map(|i| self.layers[i].name.clone()),
                    };
                    (path.clone(), entry)
                })
                .collect(),
        );
    }

    /// The assets at these real paths, from the resolved stack and the records; a path that no longer resolves to an
    /// asset loses its asset.
    fn assemble_assets(&mut self, paths: &[String]) {
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
                shape: Arc::from(""),
                variants: vec![],
                files: vec![],
                layers: layers.map(|l| self.layers[l].name.clone()).collect(),
                error: None,
                text: Arc::from(""),
                generated: None,
                fields: OnceLock::new(),
                settings_text: OnceLock::new(),
                imported: None,
                disk: None,
            };
            let files = self.node_files(node);
            // What overrides merged, in the cache: recorded as any file is, read once.
            let unread: Vec<Pending> = files
                .iter()
                .filter(|(_, file, _)| !self.records.contains_key(file))
                .filter_map(|(name, file, _)| {
                    let (size, modified) = crate::content::stat(file).ok()?;
                    let declaration = name == NODE_FILE || name == SETTINGS_FILE;
                    Some(Pending { path: file.clone(), size, modified: stamp(modified), declaration })
                })
                .collect();
            for (file, record) in unread.iter().zip(read_all(&unread)) {
                if let Some(record) = record {
                    self.records.insert(file.path.clone(), Arc::new(record));
                }
            }
            for (name, file, held) in files {
                let Some(record) = self.records.get(&file) else { continue };
                if name == NODE_FILE
                    && let Some(d) = &record.declaration
                {
                    asset.shape = Arc::from(d.shape.as_str());
                    asset.variants.clone_from(&d.variants);
                    asset.error.clone_from(&d.error);
                    asset.text = d.text.clone();
                }
                if name == SETTINGS_FILE
                    && let Some(d) = &record.declaration
                {
                    asset.generated = Some(d.text.clone());
                }
                asset.files.push(BundleFile {
                    name: name.clone(),
                    path: file.clone(),
                    size: record.size,
                    modified: record.modified,
                    layer: self.layers[held].name.clone(),
                    sha1: record.sha1.clone(),
                    made_by: None,
                });
            }
            let (uid, top, overridden) = (node.uid.clone(), layer.name.clone(), !node.overrides.is_empty());
            let at = asset.path.clone();
            let asset = self.with_working(asset);
            self.assets.insert(asset.path.clone(), Arc::new(asset));
            self.versions.retain(|version, _| version.trim_start_matches(BELOW) != uid);
            if overridden {
                self.assemble_versions(&at, &uid, top);
            }
        }
    }

    /// The versions an asset overrides, each below the last, from the layer holding it.
    fn assemble_versions(&mut self, at: &str, uid: &str, mut layer: String) {
        let mut name = format!("{BELOW}{uid}");
        while let Some(below) = self.below(&layer, uid) {
            let mut asset = Asset {
                path: at.to_string(),
                uid: Some(name.clone()),
                shape: Arc::from(""),
                variants: vec![],
                files: vec![],
                layers: vec![below.layer.clone()],
                error: None,
                text: Arc::from(""),
                generated: None,
                fields: OnceLock::new(),
                settings_text: OnceLock::new(),
                imported: None,
                disk: None,
            };
            for (file_name, file) in &below.files {
                let Some(record) = self.records.get(file) else { continue };
                if file_name == NODE_FILE
                    && let Some(d) = &record.declaration
                {
                    asset.shape = Arc::from(d.shape.as_str());
                    asset.text = d.text.clone();
                }
                if file_name == SETTINGS_FILE
                    && let Some(d) = &record.declaration
                {
                    asset.generated = Some(d.text.clone());
                }
                asset.files.push(BundleFile {
                    name: file_name.clone(),
                    path: file.clone(),
                    size: record.size,
                    modified: record.modified,
                    layer: below.layer.clone(),
                    sha1: record.sha1.clone(),
                    made_by: None,
                });
            }
            self.versions.insert(name.clone(), Arc::new(asset));
            name.insert_str(0, BELOW);
            layer = below.layer;
        }
    }

    /// A version an asset overrides, by its name: `below:<uid>`.
    #[must_use]
    pub fn version(&self, name: &str) -> Option<&Asset> {
        self.versions.get(name).map(Arc::as_ref)
    }

    /// Resolve the uids and the references, and add what is wrong with the assets' declarations.
    fn link(&mut self) {
        self.by_uid = Arc::new(self.assets.values().filter_map(|a| Some((a.uid.clone()?, a.path.clone()))).collect());
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
                .find(|f| f.name == NODE_FILE)
                .and_then(|f| self.records.get(&f.path)?.declaration.as_ref());
            for (slot, uid, target, output) in declaration.map_or(&[][..], |d| d.references.as_slice()).iter().cloned()
            {
                let output = (!output.is_empty()).then_some(output);
                let reference = Reference { from: asset.path.clone(), slot, uid, target, output };
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
        self.referenced_by = Arc::new(referenced_by);
        self.dangling = dangling;
    }

    /// Link again what these assets declare, which declared `before` (by asset): their unreadable declarations, and
    /// their references taken back and made again where a full link puts them (in asset order, then declaration
    /// order). The uids and the assets stand: other assets' references resolve as they did.
    fn relink(&mut self, assets: &[String], before: &HashMap<String, Vec<Reference>>) {
        let changed: HashSet<&str> = assets.iter().map(String::as_str).collect();
        self.problems.retain(|p| !matches!(p, Problem::Unreadable { asset, .. } if changed.contains(asset.as_str())));
        for path in assets {
            if let Some(error) = self.assets.get(path).and_then(|a| a.error.clone()) {
                self.problems.push(Problem::Unreadable { asset: path.clone(), error });
            }
        }
        let resolved = |index: &Index, r: &Reference| {
            index.by_uid.get(&r.uid).cloned().or_else(|| index.assets.contains_key(&r.target).then(|| r.target.clone()))
        };
        for (from, references) in before.iter().filter(|(a, _)| changed.contains(a.as_str())) {
            let targets: Vec<String> = references.iter().filter_map(|r| resolved(self, r)).collect();
            for target in targets {
                let referenced_by = Arc::make_mut(&mut self.referenced_by);
                if let Some(list) = referenced_by.get_mut(&target) {
                    list.retain(|r| r.from != *from);
                    if list.is_empty() {
                        referenced_by.remove(&target);
                    }
                }
            }
        }
        self.dangling.retain(|r| !changed.contains(r.from.as_str()));
        let mut sorted: Vec<&String> = assets.iter().collect();
        sorted.sort();
        for from in sorted {
            let mut placed: HashMap<String, usize> = HashMap::new();
            let mut dangling_at = self.dangling.partition_point(|r| r.from < *from);
            for reference in self.references_from(from) {
                if let Some(target) = resolved(self, &reference) {
                    let list = Arc::make_mut(&mut self.referenced_by).entry(target.clone()).or_default();
                    // After the references of the assets before it, then each of its own after the last.
                    let at = placed.entry(target).or_insert_with(|| list.partition_point(|r| r.from < *from));
                    list.insert(*at, reference);
                    *at += 1;
                } else {
                    self.dangling.insert(dangling_at, reference);
                    dangling_at += 1;
                }
            }
        }
    }

    /// The references an asset's declaration makes, in its order.
    #[must_use]
    pub fn references_from(&self, path: &str) -> Vec<Reference> {
        let Some(asset) = self.assets.get(path) else { return vec![] };
        let declaration = asset
            .files
            .iter()
            .find(|f| f.name == NODE_FILE)
            .and_then(|f| self.records.get(&f.path)?.declaration.as_ref());
        declaration
            .map_or(&[][..], |d| d.references.as_slice())
            .iter()
            .map(|(slot, uid, target, output)| Reference {
                from: path.to_string(),
                slot: slot.clone(),
                uid: uid.clone(),
                target: target.clone(),
                output: (!output.is_empty()).then(|| output.clone()),
            })
            .collect()
    }

    /// The asset a reference names: by uid, else by path.
    #[must_use]
    pub fn resolve(&self, uid: &str, target: &str) -> Option<&Asset> {
        self.by_uid.get(uid).and_then(|p| self.assets.get(p)).or_else(|| self.assets.get(target)).map(|a| &**a)
    }
}

#[cfg(test)]
mod tests {
    use super::{Index, Problem, STORE};
    use crate::Project;
    use crate::cache::Cache;
    use crate::changeset::{Base, Changeset, Op};

    /// The top layer's override of an entry, changing these files of it: each made from the version below now, kept.
    fn overriding(index: &Index, uid: &str, files: &[&str]) -> Op {
        let below = index.below("top", uid).expect("below top");
        let store = crate::chunks::Store::at(index.merged.parent().and_then(Path::parent).expect("a project"));
        let files = files.iter().map(|name| {
            let (_, file) = below.files.iter().find(|(n, _)| n == name).expect("a file below");
            store.put_file(&crate::content::read(file).expect("read")).expect("kept");
            ((*name).to_string(), Base { was: below.sha1s.get(*name).cloned(), removed: false })
        });
        Op::Override { files: files.collect(), to: None }
    }

    use crate::lock::Lock;
    use std::path::{Path, PathBuf};

    /// A project of two layers, `base` and `top`, in `.layers/`; `base` holds the bundle `a` (its `node.toml` naming
    /// `uid://a`, as declarations did before changesets) in the folder `g`, and `b` and `f`.
    fn project(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("pe2-index-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write(&root, "project.toml", "[[layer]]\nname = \"base\"\n\n[[layer]]\nname = \"top\"\n");
        write(&root, ".layers/base/g/a.node/node.toml", "uid = \"uid://a\"\nstack = [{ op = \"t.image\" }]\n");
        write(&root, ".layers/base/g/a.node/native.texels", "texels");
        write(
            &root,
            ".layers/base/b.node/node.toml",
            "stack = [{ op = \"t.file\" }]\nslot = \"uid://a#palettes/default\"\n",
        );
        write(
            &root,
            ".layers/base/f.node/node.toml",
            "stack = [{ op = \"t.file\" }]\nold = { uid = \"uid://a\", path = \"res://g/a.node\" }\n",
        );
        std::fs::create_dir_all(root.join(".layers/top")).unwrap();
        root
    }

    #[test]
    fn what_a_layer_holds_of_extensions_is_no_asset() {
        let root = project("extensions");
        write(&root, ".layers/top/extensions/my.tool.extension/extension.wasm", "code");
        write(&root, ".layers/top/extensions/old.node/node.toml", "stack = [{ op = \"t.file\" }]\n");
        let (_, index) = open(&root);
        assert!(index.assets.keys().all(|path| !path.starts_with("extensions")), "{:?}", index.assets.keys());
        assert!(index.base_hash("extensions").is_none(), "not even a folder");
        let _ = std::fs::remove_dir_all(&root);
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
        assert_eq!(base.entries.get("uid://a").map(String::as_str), Some("g/a.node"));
        assert_eq!(base.entries.len(), 4, "{base:?}");
        assert_eq!(index.assets["g/a"].uid.as_deref(), Some("uid://a"));
        // By uid alone, and in the older form with a path.
        assert_eq!(index.referenced_by["g/a"].len(), 2);
        assert!(index.problems.is_empty(), "{:?}", index.problems);
        // Listed now, and read: nothing is listed or read again.
        let (_, again) = open(&root);
        assert_eq!((again.read, changeset(&root, "base")), (0, base));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_bundle_holds_folders() {
        let root = project("folders");
        write(&root, ".layers/base/f.node/node.toml", "stack = [{ op = \"file\", file = \"face.ufo\" }]\n");
        write(&root, ".layers/base/f.node/face.ufo/metainfo.plist", "<plist/>");
        write(&root, ".layers/base/f.node/face.ufo/glyphs/A_.glif", "<glyph/>");
        write(&root, ".layers/base/f.node/face.ufo/.hidden/x", "left out");
        let (_, index) = open(&root);
        let names: Vec<&str> = index.assets["f"].files.iter().map(|file| file.name.as_str()).collect();
        assert_eq!(names, ["face.ufo/glyphs/A_.glif", "face.ufo/metainfo.plist", "node.toml"], "{names:?}");
        // What a stored-file layer naming the folder reads.
        let node: &dyn crate::graph::exec::Node = &*index.assets["f"];
        assert_eq!(node.file_names().len(), 3);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn applies_overrides_moves_and_deletes() {
        let root = project("ops");
        let (_, index) = open(&root);
        let base = changeset(&root, "base");
        let uid_of = |path: &str| base.entries.iter().find(|(_, p)| *p == path).unwrap().0.clone();
        let mut top = Changeset::default();
        top.ops.insert("uid://a".into(), overriding(&index, "uid://a", &["node.toml", "native.texels"]));
        top.ops.insert(uid_of("b.node"), Op::Move { to: "c.node".into() });
        top.ops.insert(uid_of("f.node"), Op::Delete);
        top.save(&root.join(".layers/top.toml")).unwrap();
        write(&root, ".layers/top/g/a.node/node.toml", "stack = [{ op = \"t.image\" }]\n");
        write(&root, ".layers/top/g/a.node/native.texels", "repainted");
        let (_, index) = open(&root);
        assert_eq!(index.assets.keys().collect::<Vec<_>>(), ["c", "g/a"]);
        assert_eq!(index.assets["g/a"].layers, ["base", "top"]);
        assert!(index.assets["g/a"].files.iter().all(|f| f.layer == "top"));
        assert_eq!(index.assets["c"].uid, Some(uid_of("b.node")));
        assert!(index.problems.is_empty(), "{:?}", index.problems);
        // What it overrides is there, a node of its own.
        let below = index.version("below:uid://a").unwrap();
        assert_eq!(
            (below.uid.as_deref(), below.layers.as_slice()),
            (Some("below:uid://a"), ["base".to_string()].as_slice())
        );
        assert!(below.files.iter().all(|f| f.layer == "base"));
        assert!(index.version("below:below:uid://a").is_none());
        // The base changes under the override: stale.
        write(&root, ".layers/base/g/a.node/native.texels", "retouched");
        let (_, index) = open(&root);
        assert_eq!(index.problems, [Problem::Stale { layer: "top".into(), path: "g/a.node".into() }]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn two_layers_making_one_folder_add_to_it() {
        let root = project("one-folder");
        open(&root);
        let mut top = Changeset::default();
        top.entries.insert("uid://again".into(), "g".into());
        top.entries.insert("uid://beside".into(), "g/beside.node".into());
        top.save(&root.join(".layers/top.toml")).unwrap();
        write(&root, ".layers/top/g/beside.node/node.toml", "stack = [{ op = \"t.file\" }]\n");
        let (_, index) = open(&root);
        assert!(!index.problems.iter().any(|p| matches!(p, Problem::Conflict { .. })), "{:?}", index.problems);
        assert!(index.assets.contains_key("g/a") && index.assets.contains_key("g/beside"));
    }

    #[test]
    fn reports_what_does_not_add_up() {
        let root = project("problems");
        open(&root);
        let mut top = Changeset { read_only: true, ..Changeset::default() };
        top.entries.insert("uid://gone".into(), "nowhere".into());
        top.ops.insert("uid://unknown".into(), Op::Delete);
        top.save(&root.join(".layers/top.toml")).unwrap();
        write(&root, ".layers/top/b.node/node.toml", "stack = [{ op = \"t.file\" }]\n");
        write(&root, ".layers/top/new.node/node.toml", "kind = [unclosed\n");
        let (_, index) = open(&root);
        let has = |p: &Problem| index.problems.contains(p);
        assert!(has(&Problem::Conflict { path: "b.node".into(), layers: vec!["base".into(), "top".into()] }));
        assert!(has(&Problem::Unlisted { layer: "top".into(), path: "new.node".into() }));
        assert!(has(&Problem::Missing { layer: "top".into(), path: "nowhere".into(), left_out: false }));
        assert!(has(&Problem::Unbound { layer: "top".into(), uid: "uid://unknown".into() }));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn follows_the_watcher() {
        let root = project("refresh");
        let (project, mut index) = open(&root);
        let folder = changeset(&root, "base").entries.iter().find(|(_, p)| *p == "g").unwrap().0.clone();
        // A file of a bundle changed: that bundle, its file read again.
        let texels = write(&root, ".layers/base/g/a.node/native.texels", "repainted");
        let refreshed = index.refresh(&project, &[texels]).unwrap();
        assert_eq!(refreshed.assets.into_iter().collect::<Vec<_>>(), ["g/a"]);
        assert_eq!(index.read, 1);
        // A folder renamed in the layer's folder, by other means than the editor: no edit, a problem - what it holds now
        // is unlisted, what it listed missing - until it is adopted, which follows the move: the uids are kept.
        std::fs::rename(root.join(".layers/base/g"), root.join(".layers/base/h")).unwrap();
        index.refresh(&project, &[root.join(".layers/base/g"), root.join(".layers/base/h")]).unwrap();
        assert!(!index.assets.contains_key("h/a"));
        assert!(index.problems.contains(&Problem::Unlisted { layer: "base".into(), path: "h".into() }));
        let adopted = index.adoption("base", "h").unwrap();
        assert!(adopted.contains(&("uid://a".to_string(), "h/a.node".to_string())), "{adopted:?}");
        assert!(adopted.contains(&(folder.clone(), "h".to_string())), "{adopted:?}");
        let mut base = changeset(&root, "base");
        base.entries.extend(adopted);
        base.save(&root.join(".layers/base.toml")).unwrap();
        index.refresh(&project, &[root.join(".layers/base.toml")]).unwrap();
        assert_eq!(index.assets["h/a"].uid.as_deref(), Some("uid://a"));
        assert!(index.problems.is_empty(), "{:?}", index.problems);
        // A bundle removed: its entry is missing until it is dropped from the changeset.
        std::fs::remove_dir_all(root.join(".layers/base/f.node")).unwrap();
        index.refresh(&project, &[root.join(".layers/base/f.node")]).unwrap();
        assert!(!index.assets.contains_key("f"));
        assert!(index.problems.contains(&Problem::Missing {
            layer: "base".into(),
            path: "f.node".into(),
            left_out: false
        }));
        assert!(index.refresh(&project, &[root.join("project.toml")]).unwrap().project_file);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// What an index shows, in a form to compare: its assets (their declarations and files by name, hash and layer),
    /// entries, uids, references both ways and problems (in no particular order), stack and statuses.
    fn shown(index: &Index) -> String {
        use std::fmt::Write;
        let mut out = String::new();
        for (path, a) in &index.assets {
            let files: Vec<_> = a.files.iter().map(|f| (&f.name, &f.sha1, &f.layer, f.size)).collect();
            let _ = writeln!(
                out,
                "asset {path} {:?} {:?} {:?} {:?} {:?} {:?} {:?}",
                a.uid, a.shape, a.variants, a.layers, a.error, a.text, files
            );
        }
        for (path, e) in index.entries.iter() {
            let _ = writeln!(out, "entry {path} {e:?}");
        }
        let by_uid: std::collections::BTreeMap<_, _> = index.by_uid.iter().collect();
        let _ = writeln!(out, "uids {by_uid:?}");
        let referenced: std::collections::BTreeMap<_, _> = index.referenced_by.iter().collect();
        let _ = writeln!(out, "referenced {referenced:?}\ndangling {:?}", index.dangling);
        let mut problems: Vec<String> = index.problems.iter().map(|p| format!("{p:?}")).collect();
        problems.sort();
        let _ = writeln!(out, "problems {problems:?}\nops {:?}\ndependencies {:?}", index.ops, index.dependencies);
        let mut winning: Vec<_> = index.winning.iter().collect();
        winning.sort();
        let _ = writeln!(out, "winning {winning:?}");
        out
    }

    #[test]
    fn a_generated_layer_gives_the_nodes_it_fills_their_files() {
        let root = project("generated");
        // The nodes are base's; what their plan made, base-files' - without a node of its own, and its record beside.
        let text = "[[layer]]\nname = \"base\"\n\n[[layer]]\nname = \"base-files\"\nkind = \"generated\"\nfills = \"base\"\n\n[[layer]]\nname = \"top\"\n";
        write(&root, "project.toml", text);
        std::fs::remove_file(root.join(".layers/base/g/a.node/native.texels")).unwrap();
        write(&root, ".layers/base-files/g/a.node/native.texels", "texels");
        write(&root, ".layers/base-files/extraction.record.jsonl", "{}");
        let (project, mut index) = open(&root);
        let a = &index.assets["g/a"];
        let held: Vec<(&str, &str)> = a.files.iter().map(|f| (f.name.as_str(), f.layer.as_str())).collect();
        assert_eq!(held, [("native.texels", "base-files"), ("node.toml", "base")]);
        assert!(index.problems.is_empty(), "{:?}", index.problems);
        assert!(index.layers().iter().any(|l| l.name == "base-files" && l.fills.as_deref() == Some("base")));
        // What the plan writes again is taken in as the node's.
        let (_, fresh) = open(&root);
        let texels = write(&root, ".layers/base-files/g/a.node/native.texels", "repainted");
        index.refresh(&project, std::slice::from_ref(&texels)).unwrap();
        assert_ne!(shown(&index), shown(&fresh));
        let (_, again) = open(&root);
        assert_eq!(shown(&index), shown(&again));
        // An override of the node's declaration sees the files the plan makes, and what it makes otherwise reaches it.
        let mut top = Changeset::default();
        top.ops.insert("uid://a".into(), overriding(&index, "uid://a", &["node.toml"]));
        top.save(&root.join(".layers/top.toml")).unwrap();
        write(&root, ".layers/top/g/a.node/node.toml", "stack = [{ op = \"t.image\" }]\n");
        let below = index.below("top", "uid://a").unwrap();
        assert!(below.files.iter().any(|(name, _)| name == "native.texels"), "{:?}", below.files);
        write(&root, ".layers/base-files/g/a.node/native.texels", "extracted otherwise");
        let (_, after) = open(&root);
        assert!(!after.problems.iter().any(|p| matches!(p, Problem::Stale { .. })), "{:?}", after.problems);
        let texels = after.assets["g/a"].files.iter().find(|f| f.name == "native.texels").unwrap();
        assert_eq!(std::fs::read_to_string(&texels.path).unwrap(), "extracted otherwise");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_refresh_shows_what_opening_afresh_shows() {
        let root = project("afresh");
        let (project, mut index) = open(&root);
        // Each change refreshed, the stack applied again or not as expected, then held to an opening afresh.
        let check = |index: &mut Index, project: &Project, changed: &[PathBuf], what: &str, restacked: bool| {
            assert_eq!(index.refresh(project, changed).unwrap().restacked, restacked, "{what}");
            let (_, fresh) = open(&root);
            assert_eq!(shown(index), shown(&fresh), "after {what}");
        };
        let base = |p: &str| root.join(".layers/base").join(p);
        // Contents alone: a bundle's file, its declaration (a reference added, one made dangling), unreadable.
        write(&root, ".layers/base/g/a.node/native.texels", "repainted");
        check(&mut index, &project, &[base("g/a.node/native.texels")], "a file of a bundle", false);
        write(
            &root,
            ".layers/base/b.node/node.toml",
            "stack = [{ op = \"t.file\" }]\nslot = \"uid://nowhere\"\nmore = \"uid://a\"\n",
        );
        check(&mut index, &project, &[base("b.node/node.toml")], "references changed", false);
        write(&root, ".layers/base/f.node/node.toml", "kind = [unclosed\n");
        check(&mut index, &project, &[base("f.node/node.toml")], "an unreadable declaration", false);
        write(&root, ".layers/base/f.node/node.toml", "stack = [{ op = \"t.file\" }]\nold = { uid = \"uid://a\" }\n");
        check(&mut index, &project, &[base("f.node/node.toml")], "readable again", false);
        // A file added to a bundle: its content changes, its paths stay.
        write(&root, ".layers/base/g/a.node/extra.bin", "more");
        check(&mut index, &project, &[base("g/a.node/extra.bin")], "a file added to a bundle", false);
        // Structure: a new bundle, listed in its changeset; one removed.
        write(&root, ".layers/base/n.node/node.toml", "stack = [{ op = \"t.file\" }]\nto = \"uid://a\"\n");
        check(&mut index, &project, &[base("n.node/node.toml")], "a new bundle", true);
        std::fs::remove_dir_all(base("n.node")).unwrap();
        check(&mut index, &project, &[base("n.node")], "a bundle removed", true);
        // An override in top, then what is below it changed: merged again, though only contents changed.
        let mut top = Changeset::default();
        top.ops.insert("uid://a".into(), overriding(&index, "uid://a", &["node.toml"]));
        top.save(&root.join(".layers/top.toml")).unwrap();
        write(&root, ".layers/top/g/a.node/node.toml", "stack = [{ op = \"t.image\" }]\n");
        check(
            &mut index,
            &project,
            &[root.join(".layers/top.toml"), root.join(".layers/top/g/a.node/node.toml")],
            "an override",
            true,
        );
        write(&root, ".layers/base/g/a.node/native.texels", "retouched");
        check(&mut index, &project, &[base("g/a.node/native.texels")], "an override's base changed", true);
        write(&root, ".layers/top/g/a.node/node.toml", "stack = [{ op = \"t.image\" }]\nnote = \"uid://a\"\n");
        check(
            &mut index,
            &project,
            &[root.join(".layers/top/g/a.node/node.toml")],
            "the override's own content, merged again",
            true,
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn mounts_a_tree_as_a_files_layer() {
        let root = project("mount");
        // A tree outside the project, mounted at `code`: its own `.gitignore` leaves out `*.o`; `x.node` in it is a
        // plain folder, not a bundle.
        let tree = root.with_extension("tree");
        let _ = std::fs::remove_dir_all(&tree);
        write(&tree, "main.c", "int main;");
        write(&tree, "main.o", "object");
        write(&tree, "x.node/node.toml", "stack = [{ op = \"t.file\" }]\n");
        write(&tree, ".gitignore", "*.o\n");
        let text = std::fs::read_to_string(root.join("project.toml")).unwrap();
        let mount = format!("[[mount]]\nat = \"code\"\npath = \"{}\"\nread_only = true\n", tree.display());
        std::fs::write(root.join("project.toml"), format!("{text}\n{mount}")).unwrap();
        let (project, index) = open(&root);
        assert_eq!(project.layers.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), ["code", "base", "top"]);
        let mut listed: Vec<String> = changeset(&root, "code").entries.into_values().collect();
        listed.sort();
        assert_eq!(listed, ["code", "code/main.c", "code/x.node", "code/x.node/node.toml"]);
        assert_eq!(index.entries["code/main.c"].disk, tree.join("main.c"));
        assert!(!index.assets.contains_key("code/x"));
        assert!(index.problems.is_empty(), "{:?}", index.problems);
        std::fs::remove_dir_all(&root).unwrap();
        std::fs::remove_dir_all(&tree).unwrap();
    }

    #[test]
    fn hides_layers_from_the_view_only() {
        let root = project("hidden");
        let (_, index) = open(&root);
        let mut top = Changeset::default();
        top.ops.insert("uid://a".into(), overriding(&index, "uid://a", &["node.toml"]));
        top.ops.insert("uid://nothing".into(), Op::Delete);
        top.save(&root.join(".layers/top.toml")).unwrap();
        write(&root, ".layers/top/g/a.node/node.toml", "stack = [{ op = \"t.image\" }]\n");
        let (_, mut index) = open(&root);
        let info = index.layers();
        assert_eq!((info[1].overrides, info[1].deletes, info[1].problems), (1, 1, 1));
        assert_eq!(info[1].depends_on, [super::Dependency { layer: "base".into(), operations: 1, references: 0 }]);
        let statuses: Vec<_> = index.operations("top").iter().map(|o| o.status).collect();
        assert!(statuses.contains(&super::OpStatus::Applied) && statuses.contains(&super::OpStatus::Unbound));
        assert_eq!(index.assets["g/a"].layers, ["base", "top"]);
        // Hiding the top layer shows the base's version; the problems stay the whole stack's.
        let problems = index.problems.clone();
        assert!(index.set_hidden(&["top".to_string()].into()));
        assert_eq!(index.assets["g/a"].layers, ["base"]);
        assert_eq!(index.problems, problems);
        assert!(index.set_hidden(&std::collections::BTreeSet::new()));
        assert_eq!(index.assets["g/a"].layers, ["base", "top"]);
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
