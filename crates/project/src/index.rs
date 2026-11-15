//! A project's entries as the editor sees them: its layers applied bottom to top, each by its changeset - what it
//! creates (assets, which are bundles `<path>.node/`, folders and plain files), and what it does to the entries below
//! (overrides, moves, deletes). An asset comes with its `node.toml`, its files, which assets' slots reference it,
//! and what is wrong.
//!
//! What the index read of each file is kept in the project's model, where its processes share it (`model`): its
//! stamp, the hash of its content, and for an `node.toml` what the index needs of it - its type, variant names and
//! references - besides its text, which an asset parses only when its fields are asked for. An index opens over
//! what the model holds (`model::known`), and whoever holds the project says back what the opening read anew
//! (`model::took`). Opening stats every file and reads again only those whose stamp changed; a file the model knows
//! that no layer holds any more is dropped from it. A file modified no earlier than its record was made is read
//! again whatever its stamp (git's "racily clean" case). Whoever holds a project to stay opens without that
//! (`open_behind`): the records are taken as the model has them, and the files held to them behind its first
//! answers (`verification`), what differs taken in as a change of files is. While the project is open, `refresh`
//! takes the paths a watcher reports and scans again only what they touch, and says what it changed of the model
//! (`Refreshed::model`).
//!
//! Entries a layer holds but its changeset does not list are unlisted: where the layer can be written the index lists
//! them, a create each with a new uid (an `node.toml`'s own `uid`, as declarations wrote them before changesets, is
//! kept); elsewhere they are reported.

use anyhow::{Context as _, Result};
use pe2_path::{Name, RelPath, Uid};
use sfs::HostPath;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::cache::Cache;
use crate::changeset::{self, Base, Changeset, Op};
use crate::extract::sha1_hex;
pub use crate::model::Stamp;
use crate::model::{Declaration, Declared, Record};
use crate::{FILE, Layer, LayerKind, Project, TreePath, uid};

pub const BUNDLE_SUFFIX: &str = ".node";
pub const NODE_FILE: &Name = Name::constant("node.toml");
/// A node's settings a generated layer holds for it, made by extraction: beneath what its `node.toml` sets
/// (`graph::with_generated`), so that what the game holds stays out of the node and is still edited as its fields.
pub const SETTINGS_FILE: &Name = Name::constant("settings.toml");

/// What names a version an asset overrides, before its uid: the graph's `below` operation reads it as a node.
pub const BELOW: &str = "below:";

/// Whether a path is a records layer's record (`records`).
fn in_records(path: &HostPath) -> bool {
    path.ancestors().any(|a| a.file_name().is_some_and(|n| n == crate::records::DIR))
}

/// Whether a path is a layer's record of an extension it holds: no entry, nothing of the index's.
fn of_extensions(path: &HostPath) -> bool {
    let Some(folder) = path.parent() else { return false };
    folder.file_name().is_some_and(|n| n == crate::records::EXTENSIONS)
        && folder.parent().is_some_and(|records| records.file_name().is_some_and(|n| n == crate::records::DIR))
}

/// A file an override merged over the one below, in the cache (`.cache/merged/<sha1>/<name>`), written where it is not
/// yet: its folder is named by its content's SHA-1.
fn merged_file(fs: &dyn pe2_io::Filesystem, dir: &HostPath, name: &str, bytes: &[u8]) -> std::io::Result<HostPath> {
    let at = dir
        .join(&Name::new(&sha1_hex(bytes)).map_err(std::io::Error::other)?)
        .join(&RelPath::new(name).map_err(std::io::Error::other)?);
    if !fs.is_file(&at) {
        fs.create_folder_all(&at.parent().expect("in a folder"))?;
        crate::cache::write_atomic(fs, &at, bytes).map_err(std::io::Error::other)?;
    }
    Ok(at)
}

/// The name a file of an entry at `at` goes by: a bundle's by its own, a plain file by the entry's.
fn file_name(name: &RelPath, at: &TreePath) -> String {
    if name.is_root() { at.name().unwrap_or_default().to_string() } else { name.to_string() }
}

/// A merge remembered, by its inputs: what it made (its SHA-1) and its conflicts.
#[derive(serde::Serialize, serde::Deserialize)]
struct Memo {
    sha1: pe2_ext::Sha1,
    conflicts: Vec<crate::merge::Conflict>,
}

/// A file of a bundle, from the layer whose version of the asset wins.
#[derive(Clone, Debug)]
pub struct BundleFile {
    pub name: Arc<RelPath>,
    pub path: HostPath,
    pub size: u64,
    pub modified: Stamp,
    pub layer: Arc<Name>,
    /// The hash of its content.
    pub sha1: pe2_ext::Sha1,
    /// The importer that made it of the asset's source, kept in the cache (`imports`); none for a file of the bundle.
    pub made_by: Option<String>,
}

/// One asset: its resolved path (`stages/akropolis/rooms/square/folder`), its uid, its `node.toml` and its files.
/// Its fields are parsed from the `node.toml`'s text the first time they are asked for.
#[derive(Clone, Debug)]
pub struct Asset {
    pub path: Arc<TreePath>,
    pub uid: Option<Uid>,
    /// The name the graph knows it by (`graph::exec::Node::uid`): its uid's text; for a version below an asset,
    /// `below:<uid>`, as many times over as it is below - it is no uid of its own.
    pub named: Option<Arc<str>>,
    /// What of its stack says what it makes (`graph::shape`): nodes alike share one.
    pub shape: Arc<str>,
    /// Its `variants`' names, in order.
    pub variants: Vec<String>,
    pub files: Vec<BundleFile>,
    /// The layer creating it, then those overriding it, bottom to top.
    pub layers: Vec<Arc<Name>>,
    /// Why its `node.toml` could not be parsed; its fields are then empty.
    pub error: Option<String>,
    text: Arc<str>,
    /// The project's I/O, which its files are read through.
    pub(crate) io: Arc<dyn pe2_io::Io>,
    /// Its `settings.toml`, where a generated layer holds one.
    generated: Option<Arc<str>>,
    fields: OnceLock<pe2_value::Table>,
    settings_text: OnceLock<String>,
    /// Its fields and its settings as `toml` holds them, for what reads them so still (the editor's panes, a linked
    /// extension): made when first asked for, of the asset it is asked of alone.
    fields_toml: OnceLock<toml::Table>,
    settings_toml: OnceLock<toml::Table>,
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
    pub files: BTreeMap<Arc<RelPath>, Option<WorkingFile>>,
}

/// A file of a working copy: where its content is kept, and its hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkingFile {
    pub path: HostPath,
    pub sha1: pe2_ext::Sha1,
}

/// An asset's settings with what its import derived from its source in their places.
#[derive(Clone, Debug, Default)]
pub struct Imported {
    pub settings: pe2_value::Table,
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
    /// The project's I/O, which its files are read through.
    #[must_use]
    pub fn io(&self) -> &Arc<dyn pe2_io::Io> {
        &self.io
    }

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
    pub fn fields(&self) -> &pe2_value::Table {
        self.fields.get_or_init(|| self.fields_of(&self.text))
    }

    /// A declaration's fields, as this asset's would be: over its generated settings.
    #[must_use]
    pub fn fields_of(&self, text: &str) -> pe2_value::Table {
        let declared = toml::from_str(text).unwrap_or_default();
        let generated = self.generated.as_deref().and_then(|g| toml::from_str::<pe2_value::Table>(g).ok());
        let fields = match generated {
            Some(generated) => crate::graph::with_generated_values(declared, &generated),
            None => declared,
        };
        crate::graph::with_settings_values(fields)
    }

    /// Its settings as readers see them: its fields, with what its import derived in their places.
    #[must_use]
    pub fn settings(&self) -> &pe2_value::Table {
        self.imported.as_ref().map_or_else(|| self.fields(), |i| &i.settings)
    }

    /// Its fields, as `toml` holds them: made of `fields` when first asked for.
    pub fn fields_toml(&self) -> &toml::Table {
        self.fields_toml.get_or_init(|| self.fields().to_toml())
    }

    /// Its settings, as `toml` holds them: made of `settings` when first asked for.
    pub fn settings_toml(&self) -> &toml::Table {
        self.settings_toml.get_or_init(|| self.settings().to_toml())
    }

    /// Its settings as TOML.
    #[must_use]
    pub fn settings_text(&self) -> &str {
        let written = |settings: &pe2_value::Table| {
            let settings: pe2_value::Table =
                settings.iter().filter(|(key, _)| *key != "stack").map(|(k, v)| (k.into(), v.clone())).collect();
            toml::to_string(&settings).unwrap_or_default()
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
    pub uid: Uid,
    pub home: Arc<Name>,
    pub layer: Arc<Name>,
    pub disk: HostPath,
    /// The layers overriding it, bottom to top.
    pub overrides: Vec<Arc<Name>>,
    /// The layer that moved it here, where one did.
    pub moved: Option<Arc<Name>>,
}

/// How an operation of a layer applies to the layers below it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    pub uid: Uid,
    pub op: Op,
    pub path: Option<Arc<TreePath>>,
    pub status: OpStatus,
}

/// An entry a layer creates: its uid, its path, and what the layer holds there (none: missing).
pub struct CreateView {
    pub uid: Uid,
    pub path: Arc<TreePath>,
    pub kind: Option<Kind>,
}

/// A part of a patching override made from another version of it below (`Index::stale_parts`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StalePart {
    /// The file of the bundle.
    pub file: Arc<RelPath>,
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
    pub uid: Uid,
    /// Its resolved path below the layer.
    pub path: Arc<TreePath>,
    /// Its content's hash: what an override copying it whole records as its base. None for a folder.
    pub hash: Option<String>,
    /// Its files' hashes, by name: what an override records of a file it changes, as what it was made from
    /// (`changeset::Base::was`).
    pub sha1s: BTreeMap<Arc<RelPath>, pe2_ext::Sha1>,
    /// Its content's files: a plain file itself (an empty name), a bundle's files by name.
    pub files: Vec<(Arc<RelPath>, HostPath)>,
    /// The layer holding its content.
    pub layer: Arc<Name>,
}

/// A layer in the stack as the layer panel shows it.
#[derive(Clone, Debug)]
pub struct LayerInfo {
    pub name: Arc<Name>,
    pub kind: LayerKind,
    /// A mount's mount point; empty for a layer at the top.
    pub at: Arc<TreePath>,
    /// Its content is its maker's alone.
    pub read_only: bool,
    /// Its changeset can be written.
    pub writable: bool,
    pub mounted: bool,
    /// The layer a generated layer gives its nodes' files.
    pub fills: Option<Arc<Name>>,
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
    pub layer: Arc<Name>,
    pub operations: usize,
    pub references: usize,
}

impl Dependency {
    /// Why a layer must stay above another, as said to the user.
    #[must_use]
    pub fn describe(&self, from: &Name) -> String {
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reference {
    pub from: Arc<TreePath>,
    pub slot: Box<str>,
    /// The asset's uid; where what is written there is no uid, that text: the reference leads nowhere.
    pub uid: Result<Uid, Box<str>>,
    /// The bundle's path written beside the uid, where one is: what it resolves to where the uid names no asset.
    pub target: Option<Arc<TreePath>>,
    /// What of the asset it names (`palettes/default`, from `uid://.../palettes/default`); none for the asset itself.
    pub output: Option<Arc<pe2_path::Asked>>,
}

impl Reference {
    /// A reference a declaration at `from` makes.
    fn of(from: &TreePath, declared: &Declared) -> Reference {
        Reference {
            from: from.to_owned(),
            slot: declared.slot.clone(),
            uid: declared.uid.clone(),
            target: declared.target.clone(),
            output: declared.output.clone(),
        }
    }

    /// Its uid as written: the uid, or the text that is none.
    #[must_use]
    pub fn uid_text(&self) -> String {
        self.uid.as_ref().map_or_else(ToString::to_string, ToString::to_string)
    }

    /// What it names, as it is written: the path beside its uid and the uid (`a/b (uid://x)`), or its uid alone.
    #[must_use]
    pub fn said(&self) -> String {
        match &self.target {
            Some(target) => format!("{target} ({})", self.uid_text()),
            None => self.uid_text(),
        }
    }
}

/// Something wrong with the project, found by the index.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Problem {
    /// An `node.toml` that cannot be parsed: the asset is listed without its fields.
    Unreadable { asset: String, error: String },
    /// One uid created twice.
    DuplicateUid { uid: String, paths: Vec<String> },
    /// One path in several layers, which no changeset declares an override of.
    Conflict { path: String, layers: Vec<Arc<Name>> },
    /// What a layer holds that its changeset does not list: put in a layer folder by other means than the editor, or in
    /// one that cannot be written. Adopted (listed) or removed by the user.
    Unlisted { layer: Arc<Name>, path: String },
    /// An entry a changeset lists that its layer does not hold; or holds where the index leaves it out (`left_out`:
    /// an extension's folder, an ignored name), which no asset is.
    Missing { layer: Arc<Name>, path: String, left_out: bool },
    /// An operation whose entry no layer below creates.
    Unbound { layer: Arc<Name>, uid: String },
    /// An override whose base changed since it was made.
    Stale { layer: Arc<Name>, path: String },
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
    /// A config's property that is not as the project declares it, or a property declared wrongly (no config).
    Property { config: Option<String>, property: String, why: String },
    /// What a layer holds by a name no path takes - one some system refuses, or that is not text - at its path as it
    /// shows: none of the project's, nor anything in it, until it is renamed.
    Refused { layer: Arc<Name>, path: String, why: String },
}

impl Problem {
    /// Whether it is one the stack says as it is applied, of this layer or at one of these paths: what applying
    /// the stack for some entries alone would have to say again.
    fn of_stack(&self, layer: &Name, paths: &HashSet<&str>) -> bool {
        match self {
            Problem::DuplicateUid { paths: at, .. } => at.iter().any(|path| paths.contains(path.as_str())),
            Problem::Conflict { path, layers } => paths.contains(path.as_str()) || layers.iter().any(|l| l == layer),
            Problem::Unlisted { layer: of, path }
            | Problem::Missing { layer: of, path, .. }
            | Problem::Stale { layer: of, path }
            | Problem::Refused { layer: of, path, .. } => of == layer || paths.contains(path.as_str()),
            Problem::Unbound { layer: of, .. } => of == layer,
            _ => false,
        }
    }

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
            | Problem::Stale { path, .. }
            | Problem::Refused { path, .. } => vec![path],
            Problem::DuplicateUid { paths, .. } => paths.iter().map(String::as_str).collect(),
            Problem::Unbound { .. } | Problem::Property { .. } => vec![],
        }
    }

    /// What it is, in a line.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Problem::Unreadable { error, .. } => format!("node.toml cannot be read: {error}"),
            Problem::DuplicateUid { uid, paths } => format!("{uid} is created {} times", paths.len()),
            Problem::Conflict { layers, .. } => {
                let layers: Vec<&str> = layers.iter().map(|layer| layer.as_str()).collect();
                format!("in layers {} without a declared override", layers.join(", "))
            }
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
            Problem::Property { config: Some(config), property, why } => {
                format!("the config {config}'s property {property}: {why}")
            }
            Problem::Property { config: None, property, why } => format!("the property {property}: {why}"),
            Problem::Refused { layer, why, .. } => {
                format!("held by {layer} by a name no path takes ({why}): none of the project's until it is renamed")
            }
        }
    }

    /// The layers it is about.
    #[must_use]
    pub fn layers(&self) -> Vec<&Name> {
        match self {
            Problem::Conflict { layers, .. } => layers.iter().map(|layer| &**layer).collect(),
            Problem::Unlisted { layer, .. }
            | Problem::Missing { layer, .. }
            | Problem::Unbound { layer, .. }
            | Problem::Stale { layer, .. }
            | Problem::Refused { layer, .. } => vec![&**layer],
            Problem::Unreadable { .. }
            | Problem::DuplicateUid { .. }
            | Problem::Import { .. }
            | Problem::Stack { .. }
            | Problem::Required { .. }
            | Problem::Overridden { .. }
            | Problem::Orphan { .. }
            | Problem::MissingOutput { .. }
            | Problem::Taken { .. }
            | Problem::Property { .. }
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

/// A map of the index: changed in place by whoever has the index, and handed to the snapshots made of it as a
/// copy of its own (`hand_over`) - made when one is handed over after a change, not when the change is made, and
/// handed again as it is for as long as nothing changes. So changing the index costs what the change does,
/// whatever holds a snapshot of it; and a snapshot is never changed under whoever reads it. Cloned, it is shared
/// as it is, and copied where one of the two is changed (`make_mut`), as an `Arc` is.
#[derive(Debug)]
pub struct Shared<T> {
    now: Arc<T>,
    /// What was last handed over, where nothing changed since.
    handed: Option<Arc<T>>,
}

impl<T> Shared<T> {
    #[must_use]
    pub fn new(of: T) -> Shared<T> {
        Shared { now: Arc::new(of), handed: None }
    }

    /// Whether two are one map, shared.
    #[must_use]
    pub fn same(this: &Shared<T>, other: &Shared<T>) -> bool {
        Arc::ptr_eq(&this.now, &other.now)
    }
}

impl<T: Clone> Shared<T> {
    /// The map, to change in place: copied first where it is shared as it is (`clone`), never for what was
    /// handed over.
    pub fn make_mut(this: &mut Shared<T>) -> &mut T {
        this.handed = None;
        Arc::make_mut(&mut this.now)
    }

    /// The map as it is now, for a snapshot: a copy of its own, made here where it changed since the last was
    /// handed over.
    #[must_use]
    pub fn hand_over(&mut self) -> Shared<T> {
        let handed = self.handed.get_or_insert_with(|| Arc::new(T::clone(&self.now))).clone();
        Shared { now: handed, handed: None }
    }
}

impl<T> Clone for Shared<T> {
    fn clone(&self) -> Self {
        Shared { now: self.now.clone(), handed: None }
    }
}

impl<T: Default> Default for Shared<T> {
    fn default() -> Self {
        Shared::new(T::default())
    }
}

impl<T> std::ops::Deref for Shared<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.now
    }
}

/// What a refresh changed: the assets whose index entry changed (added, changed or gone), and whether `project.toml`
/// changed - the layers themselves may have, which a refresh does not follow: the project is read again.
#[derive(Debug, Default)]
pub struct Refreshed {
    pub assets: BTreeSet<Arc<TreePath>>,
    pub project_file: bool,
    /// The stack was applied again: what changed was more than contents.
    pub restacked: bool,
    /// The entries of the whole stack that changed or came to be, by their resolved paths, and those that are no
    /// more: what is served of them is all that changed of what is served (`Index::serve_changed`).
    pub touched: BTreeSet<Arc<TreePath>>,
    pub gone: BTreeSet<Arc<TreePath>>,
    /// What it changed of the project's model, where its processes share it (`model`): applied in the commit that
    /// says what is served after the refresh.
    pub model: crate::model::Delta,
}

impl Refreshed {
    /// This, and what a later refresh changed.
    pub fn and(&mut self, later: Refreshed) {
        self.assets.extend(later.assets);
        // What came back after it was gone is there; what went after it was touched is gone.
        self.gone.retain(|path| !later.touched.contains(path));
        self.touched.retain(|path| !later.gone.contains(path));
        self.touched.extend(later.touched);
        self.gone.extend(later.gone);
        self.project_file |= later.project_file;
        self.restacked |= later.restacked;
        self.model.and(later.model);
    }
}

/// What differs between the records known of a layer stored as records and those read of it now, said of the
/// project's model.
fn records_differ(
    was: &BTreeMap<Uid, crate::model::Recorded>,
    now: &BTreeMap<Uid, crate::records::Recorded>,
    layer: &Name,
    model: &mut crate::model::Delta,
) {
    for uid in was.keys().filter(|uid| !now.contains_key(*uid)) {
        model.record(layer, *uid, None);
    }
    for (uid, (record, modified, len)) in now {
        let modified = crate::model::stamp_of(*modified);
        let same = was.get(uid).is_some_and(|(known, at, of)| {
            *at == modified && of == len && (Arc::ptr_eq(known, record) || known == record)
        });
        if !same {
            model.record(layer, *uid, Some(&(record.clone(), modified, *len)));
        }
    }
}

/// Whether an entry's content - where its layer holds it, or the layer filling that one does - or a file its
/// overrides made lies in what was scanned again: the regions, by layer and path there, and where those are on disk.
fn in_rescanned(node: &Node, regions: &[(usize, Arc<TreePath>)], disks: &[HostPath], filled: &[Option<usize>]) -> bool {
    let (layer, at) = (node.content.0, &node.content.1);
    let of_content = |(scanned, region): &(usize, Arc<TreePath>)| {
        (*scanned == layer || filled.get(layer).copied().flatten() == Some(*scanned))
            && (at.starts_with(region) || region.starts_with(at))
    };
    let through = || node.files.iter().flat_map(|files| files.iter().map(|(_, file, _)| file));
    regions.iter().any(of_content)
        || through().any(|file| disks.iter().any(|r| file.starts_with(r) || r.starts_with(file)))
}

fn stamp(time: SystemTime) -> Stamp {
    time.duration_since(UNIX_EPOCH).map_or((0, 0), |d| (d.as_secs(), d.subsec_nanos()))
}

/// A file to read: its path, its stat and whether it is an `node.toml`.
struct Pending {
    path: HostPath,
    size: u64,
    modified: Stamp,
    declaration: bool,
}

/// A file's record, read now; none when it vanished meanwhile. A file that cannot be parsed is a record with its
/// error, not a failure: a user may save a half-written `node.toml`.
fn read(io: &Arc<dyn pe2_io::Io>, file: &Pending) -> Option<Record> {
    #[allow(
        clippy::disallowed_methods,
        reason = "compared with the times the system stamps files with: the system's own clock"
    )]
    let recorded = stamp(SystemTime::now());
    // A declaration is read whole, to be parsed; any other file is only hashed, a piece at a time.
    let (sha1, declaration) = if file.declaration {
        let bytes = crate::content::read(io, &file.path).ok()?;
        (crate::extract::sha1_of(&bytes), Some(declaration(&bytes)))
    } else {
        (crate::content::sha1(io, &file.path).ok()?, None)
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
            let settings = crate::graph::with_settings(&table);
            out.variants = settings
                .get("variants")
                .and_then(toml::Value::as_table)
                .into_iter()
                .flat_map(|t| t.keys().cloned())
                .collect();
            let name = table.get("name").and_then(toml::Value::as_str);
            // An asset's own `uid` (as declarations named it before changesets) is not a reference.
            let mut made = Vec::new();
            for (key, value) in table.iter().filter(|(key, _)| *key != "uid") {
                references(value, name.unwrap_or(key), &mut made);
            }
            out.references = made.into();
        }
        Err(e) => out.error = Some(e),
    }
    out
}

/// Stat the files on every core: each one's size (none when it is gone) and modification time.
fn stat_all(io: &Arc<dyn pe2_io::Io>, files: &[HostPath]) -> Vec<(Option<u64>, Stamp)> {
    let stat = |file: &HostPath| match crate::content::stat(io, file) {
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
fn read_all(io: &Arc<dyn pe2_io::Io>, files: &[Pending]) -> Vec<Option<Record>> {
    if files.is_empty() {
        return vec![];
    }
    let threads = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    std::thread::scope(|scope| {
        let workers: Vec<_> = files
            .chunks(files.len().div_ceil(threads))
            .map(|part| scope.spawn(move || part.iter().map(|file| read(io, file)).collect::<Vec<_>>()))
            .collect();
        workers.into_iter().flat_map(|w| w.join().unwrap_or_default()).collect()
    })
}

/// What one layer's directory holds, by its real path relative to the layer (a bundle's with `.node`): its bundles
/// (their files by name), folders and plain files. Names starting with a dot are the tools' bookkeeping, never
/// entries.
#[derive(Clone, Default)]
struct Scan {
    bundles: BTreeMap<Arc<TreePath>, BTreeMap<Arc<RelPath>, HostPath>>,
    folders: BTreeSet<Arc<TreePath>>,
    files: BTreeMap<Arc<TreePath>, HostPath>,
    /// What it holds by a name no path takes, at its path as shown, with why: a problem (`Problem::Refused`), and
    /// nothing of the project's - what is in a folder so named is not looked at.
    refused: BTreeMap<String, Box<str>>,
}

/// Where a layer's content is and how it is read: its directory, the resolved path it appears at (a mount point, or
/// none), the names it leaves out, and whether it holds assets (a files layer's `.node` folders are plain folders).
#[derive(Clone)]
struct Place {
    root: HostPath,
    at: Arc<TreePath>,
    ignore: Vec<String>,
    assets: bool,
    /// The project's I/O, which its layer is read through.
    io: Arc<dyn pe2_io::Io>,
}

impl Place {
    /// What a listing of the layer depends on besides what is on disk: where it is, where it appears, whether it
    /// holds assets, and the names it leaves out. A listing kept is one of the layer as it is where this is the
    /// same.
    fn made_by(&self) -> String {
        format!("{}\n{}\n{}\n{}", self.root.display(), self.at, self.assets, self.ignore.join("\n"))
    }

    /// A resolved path's place on disk: under the layer's folder, or a mount's tree.
    fn disk(&self, path: &TreePath) -> HostPath {
        let within = path.strip_prefix(&self.at).unwrap_or(path.rel());
        self.root.join(&within)
    }

    /// The resolved path of a place on disk within the layer.
    ///
    /// # Errors
    /// It is not in the layer, or it is by a name no path takes: two answers its callers tell apart.
    fn resolved(&self, disk: &HostPath) -> Result<Arc<TreePath>, sfs::NotWithin> {
        Ok(self.at.join_path(&disk.within(&self.root)?))
    }

    /// Where to look again for what changed at a place on disk: its resolved path, or - where it is in the layer by a
    /// name no path takes - that of the nearest folder above it that has one, whose scan finds it refused and says so.
    /// None where it is not in the layer.
    fn looked_at(&self, disk: &HostPath) -> Option<Arc<TreePath>> {
        let mut at = disk.clone();
        loop {
            match self.resolved(&at) {
                Ok(rel) => return Some(rel),
                Err(sfs::NotWithin::Outside) => return None,
                Err(sfs::NotWithin::Refused(_)) => at = at.parent()?,
            }
        }
    }

    /// Whether a resolved path is left out: by its names within the layer.
    fn ignores(&self, path: &TreePath) -> bool {
        let within = path.strip_prefix(&self.at).unwrap_or(path.rel());
        changeset::ignored(&self.ignore, within.as_str()) || self.holds_extension(path)
    }

    /// Whether a path is in the folder of a layer holding its extensions (`extensions/`, `ext::folder`): code, which no
    /// asset is.
    fn holds_extension(&self, path: &TreePath) -> bool {
        self.at.is_root() && path.parts().next() == Some(crate::ext::folder::HELD)
    }
}

impl Scan {
    /// Scan what is at the resolved path `rel` within a layer (all of it at its root) and everything under it,
    /// replacing what was known there. A mount's tree is the folder at its mount point.
    fn rescan(&mut self, place: &Place, rel: &TreePath) {
        let rel: &TreePath = if place.at == *rel { TreePath::root() } else { rel };
        let below = |p: &TreePath| p.starts_with(rel);
        self.bundles.retain(|p, _| !below(p));
        self.folders.retain(|p| !below(p));
        self.files.retain(|p, _| !below(p));
        self.refused.retain(|p, _| !shown_within(p, rel));
        let start = if rel.is_root() { place.root.clone() } else { place.disk(rel) };
        let rel = if rel.is_root() { place.at.clone() } else { rel.to_owned() };
        if rel == place.at {
            if !place.at.is_root() {
                self.folders.insert(place.at.clone());
            }
        } else {
            let name = rel.name().unwrap_or_default();
            if name.starts_with('.') || place.ignores(&rel) {
                return;
            }
            if crate::pack::is_dir(&place.io, &start) {
                if place.assets && name.ends_with(BUNDLE_SUFFIX) {
                    self.bundle(&place.io, &rel, &start);
                    return;
                }
                self.folders.insert(rel.clone());
            } else {
                if crate::pack::is_file(&place.io, &start) {
                    self.files.insert(rel, start);
                }
                return;
            }
        }
        // Each folder directly under the start is walked on a core of its own.
        let mut tops = Vec::new();
        self.walk_one(place, &start, &rel, &mut tops);
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
            // A walker that failed fails the scan: what it walked is never left out unsaid.
            workers.into_iter().map(|w| w.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic))).collect()
        });
        for part in parts {
            let Scan { bundles, folders, files, refused } = part;
            self.bundles.extend(bundles);
            self.folders.extend(folders);
            self.files.extend(files);
            self.refused.extend(refused);
        }
    }

    /// One folder's entries: its bundles and plain files kept, its folders kept and handed back to walk.
    fn walk_one(
        &mut self,
        place: &Place,
        dir: &HostPath,
        dir_rel: &TreePath,
        folders: &mut Vec<(HostPath, Arc<TreePath>)>,
    ) {
        // On disk, or in a packed layer's listing.
        let mut listing = Vec::new();
        for (name, path, is_dir) in crate::pack::read_dir(&place.io, dir) {
            let name = match name {
                Ok(name) if name.as_str().starts_with('.') => continue,
                Ok(name) => name,
                Err(why) if why.text.starts_with('.') => continue,
                // What the folders above leave in, by its own name: refused, unless that is left out.
                Err(why) => {
                    if !changeset::ignored(&place.ignore, &why.text) {
                        let shown = &why.text;
                        let child = if dir_rel.is_root() { shown.to_string() } else { format!("{dir_rel}/{shown}") };
                        self.refused.insert(child, why.to_string().into());
                    }
                    continue;
                }
            };
            if place.ignores(&dir_rel.join(&name)) {
                continue;
            }
            // A large file is held as its manifest: listed as the file it stands for.
            let (shown, path) = if is_dir { (name.clone(), path) } else { listed(&name, path) };
            listing.push(Listed { shown, on_disk: name, held: (path, is_dir) });
        }
        let (listing, alike) = apart(listing);
        for Refusal { text, why } in alike {
            let child = if dir_rel.is_root() { text.into_string() } else { format!("{dir_rel}/{text}") };
            self.refused.insert(child, why);
        }
        for Listed { shown: name, held: (path, is_dir), .. } in listing {
            let child = dir_rel.join(&name);
            if is_dir {
                if place.assets && name.as_str().ends_with(BUNDLE_SUFFIX) {
                    self.bundle(&place.io, &child, &path);
                } else {
                    self.folders.insert(child.clone());
                    folders.push((path, child));
                }
            } else {
                self.files.insert(child, path);
            }
        }
    }

    /// A bundle it holds: its files, and what in it is named so no path takes it. One whose asset's name is none -
    /// `CON.node`, an asset `CON` - is refused whole.
    fn bundle(&mut self, io: &Arc<dyn pe2_io::Io>, at: &TreePath, folder: &HostPath) {
        if let Err(why) = asset_named(at) {
            self.refused.insert(at.to_string(), format!("its asset's name: {why}").into());
            return;
        }
        let (files, refused) = bundle_files(io, folder);
        for refusal in refused {
            self.refused.insert(format!("{at}/{}", refusal.text), refusal.why);
        }
        self.bundles.insert(at.to_owned(), files);
    }

    /// What it holds, by path, as the project's model keeps it: its text.
    fn held(&self) -> BTreeMap<String, crate::model::Held> {
        use crate::model::Held;
        let bundles = self.bundles.iter().map(|(path, files)| (path.to_string(), Held::Bundle(files.clone())));
        let folders = self.folders.iter().map(|path| (path.to_string(), Held::Folder));
        let files = self.files.iter().map(|(path, file)| (path.to_string(), Held::File(file.clone())));
        let refused = self.refused.iter().map(|(path, why)| (path.clone(), Held::Refused(why.clone())));
        bundles.chain(folders).chain(files).chain(refused).collect()
    }

    /// What the project's model keeps of a layer, as a scan of it: each path checked as it is read back - one that is
    /// none is refused, said as the scan says it, never taken for another.
    fn of_held(held: BTreeMap<String, crate::model::Held>) -> Scan {
        use crate::model::Held;
        let mut scan = Scan::default();
        for (path, held) in held {
            let at = match (&held, TreePath::new(&path)) {
                (Held::Refused(_), _) => None,
                (_, Ok(at)) => Some(at),
                (_, Err(why)) => {
                    scan.refused.insert(path, why.to_string().into());
                    continue;
                }
            };
            match (held, at) {
                (Held::Refused(why), _) => drop(scan.refused.insert(path, why)),
                (Held::Folder, Some(at)) => drop(scan.folders.insert(at.to_owned())),
                (Held::File(file), Some(at)) => drop(scan.files.insert(at.to_owned(), file)),
                (Held::Bundle(files), Some(at)) => drop(scan.bundles.insert(at.to_owned(), files)),
                (_, None) => {}
            }
        }
        scan
    }

    /// What differs between what a layer held and what it holds now, said of the project's model.
    fn differs(before: &Scan, now: &Scan, root: &HostPath, layer: &Name, model: &mut crate::model::Delta) {
        let (before, now) = (before.held(), now.held());
        for path in before.keys().filter(|path| !now.contains_key(*path)) {
            model.held(root, layer, path, None);
        }
        for (path, held) in now.iter().filter(|(path, held)| before.get(*path) != Some(held)) {
            model.held(root, layer, path, Some(held));
        }
    }

    /// The places on disk where it differs from another scan of the same layer: what is at a path in one and not
    /// the other, or otherwise in each. What is refused is at no place it reads: it is the problem it is.
    fn differing(&self, other: &Scan, place: &Place) -> Vec<HostPath> {
        let mut paths: BTreeSet<&TreePath> = BTreeSet::new();
        for path in self.bundles.keys().chain(other.bundles.keys()) {
            if self.bundles.get(path) != other.bundles.get(path) {
                paths.insert(path);
            }
        }
        for path in self.folders.iter().chain(other.folders.iter()) {
            if self.folders.contains(path) != other.folders.contains(path) {
                paths.insert(path);
            }
        }
        for path in self.files.keys().chain(other.files.keys()) {
            if self.files.get(path) != other.files.get(path) {
                paths.insert(path);
            }
        }
        paths.into_iter().map(|path| place.disk(path)).collect()
    }

    /// Bring the folders above a path to what the layer has there now, as a walk of all of it would find them:
    /// each there where it is a folder the layer does not leave out, and not where it is none - what scanning a
    /// path again leaves be, which the first thing put in a folder, and the last taken out of one that goes with
    /// it, change. Those that came or went, with whether they are there.
    fn folders_above(&mut self, place: &Place, rel: &TreePath) -> Vec<(Arc<TreePath>, bool)> {
        let mut changed = Vec::new();
        let mut above = rel.parent();
        while let Some(folder) = above.filter(|folder| !folder.is_root() && place.at != **folder) {
            let name = folder.name().unwrap_or_default();
            let there =
                !name.starts_with('.') && !place.ignores(folder) && crate::pack::is_dir(&place.io, &place.disk(folder));
            let was = self.folders.contains(folder);
            if there && !was {
                self.folders.insert(folder.to_owned());
            } else if !there && was {
                self.folders.remove(folder);
            }
            if there != was {
                changed.push((folder.to_owned(), there));
            }
            above = folder.parent();
        }
        changed
    }

    fn kind(&self, path: &TreePath) -> Option<Kind> {
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
    fn paths(&self) -> impl Iterator<Item = &TreePath> {
        self.bundles.keys().chain(self.folders.iter()).chain(self.files.keys()).map(|path| &**path)
    }

    /// Every file it holds: bundles' and plain.
    fn all_files(&self) -> impl Iterator<Item = &HostPath> {
        self.bundles.values().flat_map(BTreeMap::values).chain(self.files.values())
    }

    /// What it holds at the resolved path `rel` and under it (all of it at the root).
    fn under(&self, rel: &TreePath) -> Scan {
        let below = |p: &TreePath| p.starts_with(rel);
        Scan {
            bundles: self.bundles.iter().filter(|(p, _)| below(p)).map(|(p, f)| (p.clone(), f.clone())).collect(),
            folders: self.folders.iter().filter(|p| below(p)).cloned().collect(),
            files: self.files.iter().filter(|(p, _)| below(p)).map(|(p, f)| (p.clone(), f.clone())).collect(),
            refused: self
                .refused
                .iter()
                .filter(|(p, _)| shown_within(p, rel))
                .map(|(p, w)| (p.clone(), w.clone()))
                .collect(),
        }
    }

    /// What it holds by a name no path takes, each the problem it is of `layer`.
    fn refused_problems<'a>(&'a self, layer: &'a Name) -> impl Iterator<Item = Problem> + 'a {
        self.refused.iter().map(move |(path, why)| Problem::Refused {
            layer: layer.to_owned(),
            path: path.clone(),
            why: why.to_string(),
        })
    }

    /// Whether it holds the same paths as `other`: what a change of contents alone leaves.
    fn same_paths(&self, other: &Scan) -> bool {
        self.bundles.keys().eq(other.bundles.keys())
            && self.folders == other.folders
            && self.files.keys().eq(other.files.keys())
            && self.refused == other.refused
    }

    /// Whether it holds this file on disk, of a bundle or plain.
    fn holds(&self, place: &Place, file: &HostPath) -> bool {
        // One in by a name no path takes is held as refused, not as a file.
        let Ok(rel) = place.resolved(file) else { return false };
        if self.files.get(&rel).is_some_and(|f| f == file) {
            return true;
        }
        bundle_at(&rel)
            .is_some_and(|bundle| self.bundles.get(&bundle).is_some_and(|files| files.values().any(|f| f == file)))
    }
}

/// Whether what is shown at a path - refused, so text - is at a path or under it.
fn shown_within(shown: &str, at: &TreePath) -> bool {
    at.is_root() || shown == at.as_str() || shown.strip_prefix(at.as_str()).is_some_and(|rest| rest.starts_with('/'))
}

/// A bundle's files by name: their paths within it, a file in a folder of it (a UFO font's) as `<folder>/<file>`.
/// What is hidden (a name starting with `.`) is left out, folders and all.
///
/// What in it is named so no path takes it is handed back apart, each refused with its path within the bundle as it
/// shows: nothing of the bundle's, for whoever walks it to say so.
pub(crate) fn bundle_files(
    io: &Arc<dyn pe2_io::Io>,
    dir: &HostPath,
) -> (BTreeMap<Arc<RelPath>, HostPath>, Vec<Refusal>) {
    let mut files = BTreeMap::new();
    let mut refused = Vec::new();
    let mut pending = vec![(dir.clone(), RelPath::root().to_owned())];
    while let Some((folder, prefix)) = pending.pop() {
        let within =
            |text: &str| -> Box<str> { if prefix.is_root() { text.into() } else { format!("{prefix}/{text}").into() } };
        // On disk, or in a packed layer's listing.
        let mut listing = Vec::new();
        for (name, path, is_dir) in crate::pack::read_dir(io, &folder) {
            let name = match name {
                Ok(name) if name.as_str().starts_with('.') => continue,
                Ok(name) => name,
                Err(why) if why.text.starts_with('.') => continue,
                Err(why) => {
                    refused.push(Refusal { text: within(&why.text), why: why.to_string().into() });
                    continue;
                }
            };
            // A large file is held as its manifest: listed as the file it stands for.
            let (shown, path) = if is_dir { (name.clone(), path) } else { listed(&name, path) };
            listing.push(Listed { shown, on_disk: name, held: (path, is_dir) });
        }
        let (listing, alike) = apart(listing);
        refused.extend(alike.into_iter().map(|refusal| Refusal { text: within(&refusal.text), why: refusal.why }));
        for Listed { shown, held: (path, is_dir), .. } in listing {
            if is_dir {
                pending.push((path, prefix.join(&shown)));
            } else {
                files.insert(prefix.join(&shown), path);
            }
        }
    }
    (files, refused)
}

/// What a walk refused: its path as it shows, and why - the project's problem to say.
#[derive(Debug)]
pub(crate) struct Refusal {
    pub(crate) text: Box<str>,
    pub(crate) why: Box<str>,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.why)
    }
}

/// An entry of a folder as a walk lists it: by the name it shows as (a manifest as the file it stands for), the name it
/// has on disk, and what of it the walk keeps.
struct Listed<T> {
    shown: Arc<Name>,
    on_disk: Arc<Name>,
    held: T,
}

/// A folder's listing apart from what in it is named alike: entries whose shown names are equal - `Notes` beside
/// `notes`, a name beside the same in another normal form, a file beside its own manifest - are each refused, by its
/// name on disk, whichever the system listed first: no system that does not tell them apart could hold them both.
fn apart<T>(listing: Vec<Listed<T>>) -> (Vec<Listed<T>>, Vec<Refusal>) {
    let mut alike: HashMap<&Name, Vec<usize>> = HashMap::new();
    for (at, entry) in listing.iter().enumerate() {
        alike.entry(&*entry.shown).or_default().push(at);
    }
    let mut refused = Vec::new();
    let mut gone = vec![false; listing.len()];
    for group in alike.values().filter(|group| group.len() > 1) {
        for &at in group {
            let others: Vec<String> = group
                .iter()
                .filter(|&&other| other != at)
                .map(|&other| format!("{:?}", listing[other].on_disk.as_str()))
                .collect();
            let text = listing[at].on_disk.as_str();
            let why = format!(
                "{text:?} is one name with {} beside it to a system that does not tell case or form apart",
                others.join(" and ")
            );
            refused.push(Refusal { text: text.into(), why: why.into() });
            gone[at] = true;
        }
    }
    refused.sort_by(|a, b| a.text.cmp(&b.text));
    let kept = listing.into_iter().zip(gone).filter(|(_, gone)| !gone).map(|(entry, _)| entry).collect();
    (kept, refused)
}

/// A file as the index lists it: a manifest (`<name>.chunks`) as the file it stands for, by that file's name and path.
fn listed(name: &Name, path: HostPath) -> (Arc<Name>, HostPath) {
    let stands_for = name.as_str().strip_suffix(crate::chunks::SUFFIX.as_str()).and_then(|name| Name::new(name).ok());
    match (stands_for, crate::chunks::logical(&path)) {
        (Some(name), Some(logical)) => (name.to_owned(), logical),
        _ => (name.to_owned(), path),
    }
}

/// The asset a bundle's path names: its last name without `.node`.
///
/// # Errors
/// That is no name (`CON.node`, whose asset would be `CON`).
pub fn asset_named(bundle: &TreePath) -> Result<Arc<TreePath>, pe2_path::PathError> {
    let Some(name) = bundle.name().and_then(|name| name.strip_suffix(BUNDLE_SUFFIX)) else {
        return Ok(bundle.to_owned());
    };
    let parent = bundle.parent().unwrap_or_default();
    Ok(parent.join(Name::new(name)?))
}

/// Move an entry and what is under it to another path, by a layer (`i`): each where it goes, but where something is there
/// already, which stays where it was and is said back.
fn move_subtree(
    (resolved, where_is): (&mut Resolved, &mut HashMap<Uid, Arc<TreePath>>),
    from: &TreePath,
    to: &TreePath,
    i: usize,
) -> Vec<Arc<TreePath>> {
    let mut conflicts = Vec::new();
    for p in subtree(resolved, from) {
        let target = to.join_path(p.strip_prefix(from).expect("under it, as subtree lists"));
        // What is there is another entry unless it is this one, spelled otherwise (`n` to `N`): then it takes the
        // spelling it is moved to.
        if *target != *p && resolved.contains_key(&target) {
            conflicts.push(target);
            continue;
        }
        let mut node = resolved.remove(&p).expect("listed by subtree");
        if p == *from {
            node.moved = Some(i);
        }
        where_is.insert(node.uid, target.clone());
        resolved.insert(target, node);
    }
    conflicts
}

/// Whether a map holds a path spelled as it is. Paths compare by their names' folds, so `n` finds `N`; where an entry is
/// spelled anew (`n` renamed `N`), what was is gone and what is came, as a listing by text has them.
fn spelled<V>(map: &BTreeMap<Arc<TreePath>, V>, path: &TreePath) -> bool {
    map.get_key_value(path).is_some_and(|(key, _)| key.as_str() == path.as_str())
}

/// Whether a set of paths holds one spelled as it is (`spelled`).
fn spelled_among(set: &HashSet<&TreePath>, path: &TreePath) -> bool {
    set.get(path).is_some_and(|there| there.as_str() == path.as_str())
}

/// An asset's bundle: its path with `.node` after its last name. The root names no asset, and is itself.
///
/// # Panics
/// Never: a name with `.node` after it is a name.
#[must_use]
pub fn bundle_named(asset: &TreePath) -> Arc<TreePath> {
    let Some(name) = asset.name() else { return asset.to_owned() };
    // A name and `.node` after it is a name.
    let text = format!("{name}{BUNDLE_SUFFIX}");
    let bundle = Name::new(&text).expect("a name with .node after it is a name");
    asset.parent().unwrap_or_default().join(bundle)
}

/// The asset an entry's path names: a bundle's without `.node`, any other its own. The index holds no bundle whose asset
/// is no name (`Scan::bundle`).
///
/// # Panics
/// The path is a bundle whose asset is no name: one the index holds none of.
#[must_use]
pub fn asset_of(path: &TreePath) -> Arc<TreePath> {
    asset_named(path).expect("the index holds no bundle whose asset is no name")
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
    name: Arc<Name>,
    kind: LayerKind,
    place: Place,
    changeset_path: HostPath,
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
    fills: Option<Arc<Name>>,
    /// What it holds, shared with the snapshots made of the index until it is scanned again.
    scan: Arc<Scan>,
}

impl LayerState {
    /// A layer as the project has it. What it holds is as `known` says - what the project's model kept of it -
    /// where that is to be `taken` so and was made by the same place and names left out; else as a walk of it
    /// finds it now, and what was known is given back with it, to tell what differs.
    fn new(
        layer: &Layer,
        known: Option<(String, Scan)>,
        taken: bool,
        io: &Arc<dyn pe2_io::Io>,
    ) -> Result<(LayerState, Option<Scan>)> {
        let records = crate::records::is_records(io.filesystem(), &layer.path);
        let changeset = LayerState::changeset_of(layer, io.filesystem())?;
        let tree_ignore = if layer.mounted { tree_ignores(io.filesystem(), &layer.path) } else { vec![] };
        let mut ignore = changeset.ignore.clone();
        ignore.extend(tree_ignore.iter().cloned());
        let assets = layer.kind != LayerKind::Files;
        let place = Place { root: layer.path.clone(), at: layer.at.clone(), ignore, assets, io: io.clone() };
        let (scan, walked) = match known {
            Some((made_by, scan)) if taken && made_by == place.made_by() => (scan, None),
            known => {
                let mut scan = Scan::default();
                scan.rescan(&place, TreePath::root());
                (scan, Some(known.map_or_default(|(_, was)| was)))
            }
        };
        let state = LayerState {
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
            } else if records || io.filesystem().exists(&layer.changeset) {
                Listing::Adopted
            } else {
                Listing::Once
            },
            fills: layer.fills.clone(),
            scan: Arc::new(scan),
        };
        Ok((state, walked))
    }

    /// A layer's changeset: as its file says, or, for a records layer, as its records say (`records::Set::changeset`).
    fn changeset_of(layer: &Layer, fs: &dyn pe2_io::Filesystem) -> Result<Changeset> {
        if crate::records::is_records(fs, &layer.path) {
            return crate::records::Set::changeset(&layer.path)
                .with_context(|| format!("{}: its records are not read", layer.name));
        }
        Changeset::load(fs, &layer.changeset)
    }

    /// Its changeset read again, and the names it leaves out with it.
    fn reload_changeset(&mut self) -> Result<()> {
        self.changeset = if crate::records::is_records(self.place.io.filesystem(), &self.place.root) {
            crate::records::Set::changeset(&self.place.root).context("its records are not read")?
        } else {
            Changeset::load(self.place.io.filesystem(), &self.changeset_path)?
        };
        self.place.ignore = self.changeset.ignore.iter().chain(&self.tree_ignore).cloned().collect();
        Ok(())
    }
}

/// The names a mounted tree's own rules leave out: its `.gitignore`, and the one at the top of the repository it lies
/// in (by name only: `changeset::gitignore_names`).
fn tree_ignores(fs: &dyn pe2_io::Filesystem, tree: &HostPath) -> Vec<String> {
    let mut names = Vec::new();
    let mut read = |dir: &HostPath| {
        if let Ok(text) = fs.read_to_string(&dir.join(&crate::cache::GITIGNORE)) {
            names.extend(changeset::gitignore_names(&text));
        }
    };
    read(tree);
    if let Some(repository) = tree.ancestors().skip(1).find(|d| fs.exists(&d.join(&Name::constant(".git")))) {
        read(&repository);
    }
    names
}

/// An entry while the stack is applied: what it is, its uid, the layer creating it, where its winning content is (a
/// layer and the path there), and the layers that overrode it.
#[derive(Clone, PartialEq, Eq)]
struct Node {
    kind: Kind,
    uid: Uid,
    home: usize,
    content: (usize, Arc<TreePath>),
    overrides: Vec<usize>,
    moved: Option<usize>,
    /// Its files, where an override wins: those it changes - as held, or merged over the version below now - and those
    /// below it does not change; a plain file's own by an empty name.
    files: Option<Arc<NodeFiles>>,
    /// The files its overrides' merges read (theirs below, and their own), for a change to any to merge again.
    inputs: Arc<Vec<HostPath>>,
}

impl Node {
    /// An entry as the layer creating it left it.
    fn created(kind: Kind, uid: Uid, home: usize, path: &Arc<TreePath>) -> Node {
        Node {
            kind,
            uid,
            home,
            content: (home, Arc::clone(path)),
            overrides: vec![],
            moved: None,
            files: None,
            inputs: Arc::default(),
        }
    }
}

impl Node {
    /// As the project's model keeps it: its layers by their place in the stack, its files where they are in the
    /// project's folder.
    fn written(&self, root: &HostPath) -> Vec<u8> {
        use crate::model::{Out, name};
        let layer = |out: &mut Out, layer: usize| out.u32(u32::try_from(layer).unwrap_or(u32::MAX));
        let mut out = Out::bare();
        out.u8(match self.kind {
            Kind::Asset => 0,
            Kind::Folder => 1,
            Kind::File => 2,
        });
        out.text(&self.uid.to_string());
        layer(&mut out, self.home);
        layer(&mut out, self.content.0);
        out.text(self.content.1.as_str());
        layer(&mut out, self.overrides.len());
        for over in &self.overrides {
            layer(&mut out, *over);
        }
        // One past the layer that moved it; 0 for none.
        layer(&mut out, self.moved.map_or(0, |moved| moved + 1));
        out.u8(u8::from(self.files.is_some()));
        for (file, path, held) in self.files.iter().flat_map(|files| files.iter()) {
            out.u8(1);
            out.text(file.as_str());
            out.text(&name(root, path));
            layer(&mut out, *held);
        }
        out.u8(0);
        layer(&mut out, self.inputs.len());
        for input in self.inputs.iter() {
            out.text(&name(root, input));
        }
        out.0
    }

    /// As the project's model keeps it, read back, each name checked; none where what is there is no entry.
    fn read(root: &HostPath, bytes: &[u8]) -> Option<Node> {
        use crate::model::{In, named};
        let mut read = In(bytes);
        let layer = |read: &mut In<'_>| read.u32().and_then(|layer| usize::try_from(layer).ok());
        let kind = [Kind::Asset, Kind::Folder, Kind::File].get(usize::from(read.u8()?)).copied()?;
        let uid = Uid::new(&read.text()?).ok()?;
        let home = layer(&mut read)?;
        let content = (layer(&mut read)?, TreePath::new(&read.text()?).ok()?.to_owned());
        let overrides = (0..layer(&mut read)?).map(|_| layer(&mut read)).collect::<Option<Vec<usize>>>()?;
        let moved = layer(&mut read)?.checked_sub(1);
        let has_files = read.u8()? != 0;
        let mut files = Vec::new();
        while read.u8()? != 0 {
            files.push((RelPath::new(&read.text()?).ok()?.to_owned(), named(root, &read.text()?)?, layer(&mut read)?));
        }
        let inputs = (0..layer(&mut read)?).map(|_| named(root, &read.text()?)).collect::<Option<Vec<HostPath>>>()?;
        let files = has_files.then(|| Arc::new(files));
        read.0.is_empty().then_some(Node {
            kind,
            uid,
            home,
            content,
            overrides,
            moved,
            files,
            inputs: Arc::new(inputs),
        })
    }
}

/// What the stack says besides the entries it resolves, as the project's model keeps it by these names: each
/// layer's operations with their status, what does not add up, what the layers need of each other.
const SAID_OPS: &str = "ops";
const SAID_PROBLEMS: &str = "problems";
const SAID_DEPENDENCIES: &str = "dependencies";

/// Something the stack says, written as the model keeps it.
fn said<T: serde::Serialize>(it: &T) -> Vec<u8> {
    let mut bytes = Vec::new();
    let _ = crate::wire::send(&mut bytes, it);
    bytes
}

/// Something the stack says, read back.
fn said_of<T: serde::de::DeserializeOwned>(mut bytes: &[u8]) -> Option<T> {
    crate::wire::receive(&mut bytes).ok().flatten()
}

/// What the stack is made of besides the files, the layers' listings and their records: the layers as the project
/// says them, in their order. A stack kept of other layers is none of this project's.
fn stack_made_of(project: &Project) -> String {
    use std::fmt::Write as _;
    project.layers.iter().fold(String::new(), |mut made_of, layer| {
        let _ = writeln!(
            made_of,
            "{}|{:?}|{}|{}|{}|{}|{}|{}|{:?}|{:?}",
            layer.name,
            layer.kind,
            layer.path.display(),
            layer.at,
            layer.changeset.display(),
            layer.read_only,
            layer.changeset_writable,
            layer.mounted,
            layer.fills,
            layer.storage
        );
        made_of
    })
}

/// The stack applied: the resolved entries, what is wrong, and each layer's operations with their status and the path
/// their entry had.
struct Resolution {
    resolved: Resolved,
    problems: Vec<Problem>,
    ops: Vec<OpStatuses>,
}

/// What the layers below a layer resolve (`Index::under`).
pub struct Under(Option<Arc<Beneath>>);

impl Under {
    /// The uid and kind of the entry at a path.
    #[must_use]
    pub fn at(&self, path: &TreePath) -> Option<(Uid, Kind)> {
        let node = self.0.as_ref()?.resolution.resolved.get(path)?;
        Some((node.uid, node.kind))
    }

    /// The path and kind of the entry of a uid.
    #[must_use]
    pub fn of(&self, uid: Uid) -> Option<(&TreePath, Kind)> {
        let beneath = self.0.as_ref()?;
        let (path, node) = beneath.resolution.resolved.get_key_value(beneath.by_uid.get(&uid)?)?;
        Some((path, node.kind))
    }
}

/// The entries a layer let go of and made, by uid and path, those made each with its kind: what the stack is applied
/// for alone (`Index::restack_entries`).
struct Restacked {
    gone: Vec<(Uid, Arc<TreePath>)>,
    came: Vec<(Uid, Arc<TreePath>)>,
    kinds: Vec<Kind>,
}

/// What the layers below one resolve, with where each uid is.
struct Beneath {
    resolution: Resolution,
    by_uid: HashMap<Uid, Arc<TreePath>>,
}

/// The bundle a path is in or is: the nearest of it and the folders above whose name ends in `.node`.
fn bundle_at(rel: &TreePath) -> Option<Arc<TreePath>> {
    let mut at = Some(rel);
    let mut found = None;
    while let Some(path) = at {
        if path.name().is_some_and(|name| name.ends_with(BUNDLE_SUFFIX)) {
            found = Some(path);
        }
        at = path.parent().filter(|parent| !parent.is_root());
    }
    found.map(ToOwned::to_owned)
}

/// A uid as a reference writes it: the uid, or - where what is written is none - that text, the reference leading
/// nowhere.
pub type WrittenUid = Result<Uid, Box<str>>;

/// The reference a value holds, if it holds one: its asset's uid, and the path it names where it names one. A
/// reference is the asset's uid, `"uid://..."` - with what of it it names after a `/` (`/palettes/default`), which
/// this leaves out (`asked_of` gives it) -, or, as written before uids sufficed, a table of the uid and the bundle's
/// `res://` path.
#[must_use]
pub fn reference(value: &toml::Value) -> Option<(WrittenUid, Option<Arc<TreePath>>)> {
    match value {
        // One naming no node as a uid keeps its text: it resolves to nothing, and is said as a reference leading nowhere.
        toml::Value::String(s) if s.starts_with("uid://") => match pe2_ext::target_of(value) {
            Ok(Some(pe2_path::Reference::Uid(uid))) => Some((Ok(uid), None)),
            _ => Some((Err(s.as_str().into()), None)),
        },
        toml::Value::Table(t) => {
            let uid = t.get("uid")?.as_str()?;
            let written = t.get("path")?.as_str()?;
            let path = TreePath::from_res(written.strip_suffix(BUNDLE_SUFFIX).unwrap_or(written)).ok()?;
            Some((Uid::new(uid).map_err(|_| uid.into()), Some(path.to_owned())))
        }
        _ => None,
    }
}

/// What a reference names of its asset, and how it asks for it (`cells/body?palette=default`): none where it names the
/// asset itself, or what it names is no address - which whoever reads it refuses, saying why.
fn asked(value: &toml::Value) -> Option<Arc<pe2_path::Asked>> {
    pe2_ext::asked_of(value).ok().flatten()
}

/// Every reference in a value (`reference`), named by the enclosing slot's `name` or else the field it sits in.
fn references(value: &toml::Value, field: &str, out: &mut Vec<Declared>) {
    match value {
        toml::Value::Table(t) => {
            if let Some((uid, target)) = reference(value) {
                out.push(Declared { slot: field.into(), uid, target, output: asked(value) });
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
                out.push(Declared { slot: field.into(), uid, target: None, output: asked(value) });
            }
        }
        _ => {}
    }
}

/// The paths at or under `path`.
/// The uids references name, as they are written: one whose text is no uid names no entry, and is none of these - the
/// link says it dangles.
fn named_uids<'a>(references: impl IntoIterator<Item = &'a Reference>) -> Vec<Uid> {
    references.into_iter().filter_map(|r| r.uid.as_ref().ok().copied()).collect()
}

/// A path without what it ends in, where it ends in it and something is before it: `a/b/c` without `b/c` is `a`.
fn without_tail(path: &TreePath, tail: &RelPath) -> Option<Arc<TreePath>> {
    let (parts, tail_parts): (Vec<&Name>, Vec<&Name>) = (path.parts().collect(), tail.parts().collect());
    let keep = parts.len().checked_sub(tail_parts.len()).filter(|keep| *keep > 0)?;
    if parts[keep..] != tail_parts[..] {
        return None;
    }
    let mut at = path;
    for _ in 0..tail_parts.len() {
        at = at.parent()?;
    }
    Some(at.to_owned())
}

fn subtree<V>(map: &BTreeMap<Arc<TreePath>, V>, path: &TreePath) -> Vec<Arc<TreePath>> {
    // Paths order name by name: what is under a path follows it, before anything that is not.
    let after = map.range::<TreePath, _>(pe2_path::from(path)).map(|(p, _)| p);
    after.take_while(|p| p.starts_with(path)).cloned().collect()
}

/// How far a long piece of work is: its stage, and how many of how many it has done (both 0 where it cannot say).
pub type Report<'a> = dyn Fn(&str, usize, usize) + 'a;

/// The entries a stack resolves, by their real paths.
type Resolved = BTreeMap<Arc<TreePath>, Node>;

/// A node's files: each by its name in the node, where it is on disk, and the layer it comes from.
type NodeFiles = Vec<(Arc<RelPath>, HostPath, usize)>;

/// A layer's operations, by uid: how each went, and the path its entry had where it applied.
type OpStatuses = BTreeMap<Uid, (OpStatus, Option<Arc<TreePath>>)>;

/// Which entry's content wins, by the layer and path holding it.
type Winning = HashMap<(usize, Arc<TreePath>), Arc<TreePath>>;

#[derive(Clone)]
pub struct Index {
    /// The resolved assets, by path: shared with the snapshots made of the index, copied where changed.
    /// Shared with the snapshots made of the index, copied where one is changed while they hold it.
    pub assets: Shared<BTreeMap<Arc<TreePath>, Arc<Asset>>>,
    /// The versions overridden assets have below them, by `below:<uid>`, and those below that, by `below:below:<uid>`:
    /// nodes of their own, read through the graph's `below` operation - made when first asked for (`version`),
    /// of the stack as it is then, and let go of whenever an asset is assembled again.
    versions: Arc<OnceLock<HashMap<String, Arc<Asset>>>>,
    /// Every resolved entry - assets, folders, plain files - by path. This and the maps below are shared with the
    /// snapshots made of the index, copied when a take-in changes them.
    pub entries: Shared<BTreeMap<Arc<TreePath>, Entry>>,
    /// How many files the last opening or refresh read, rather than took from what it knew.
    pub read: usize,
    pub by_uid: Shared<HashMap<Uid, Arc<TreePath>>>,
    /// For each asset, the references naming it.
    pub referenced_by: Shared<HashMap<Arc<TreePath>, Vec<Reference>>>,
    /// References whose uid or path names no asset of the project.
    pub dangling: Vec<Reference>,
    pub problems: Vec<Problem>,
    /// What is wrong with the project's config properties, as of its loading: among the problems.
    properties: Vec<Problem>,
    layers: Vec<LayerState>,
    /// Each file's record, shared with the snapshots made of the index.
    records: Shared<HashMap<HostPath, Arc<Record>>>,
    /// What its opening read that differs from what it opened over, of the project's model: for whoever holds the
    /// project to say there (`take_opened`, `model::took`).
    opened: crate::model::Delta,
    /// The project's folder.
    root: HostPath,
    /// The project's I/O.
    io: Arc<dyn pe2_io::Io>,
    /// What does not add up in the stack, as it was last applied: the stack's own among the problems.
    stack_problems: Vec<Problem>,
    /// What the stack is made of besides what the model holds (`stack_made_of`), and what it said of its
    /// operations, its problems and what the layers need of each other when that was last said in the model:
    /// said again where it differs.
    made_of: String,
    stack_said: Arc<[Vec<u8>; 3]>,
    /// Whether its opening took the stack as the model had it, where it applied none.
    stack_taken: bool,
    resolved: Shared<Resolved>,
    /// What the whole stack resolves, hidden layers too: what is served as files (`served`). The same as `resolved`
    /// where nothing is hidden.
    full: Shared<Resolved>,
    /// The layers left out of the view (the editor's eyes); problems and statuses are the full stack's.
    hidden: HashSet<usize>,
    ops: Vec<OpStatuses>,
    dependencies: Vec<BTreeMap<usize, (usize, usize)>>,
    /// The content hash of each entry a writable layer holds, as of the last pass: what tells a move made outside
    /// the editor from a deletion and a new entry.
    hashes: Shared<Vec<HashMap<Arc<TreePath>, String>>>,
    /// The resolved path of each entry by where its winning content is (a layer and its path there): what a change
    /// of a layer's content touches.
    winning: Shared<Winning>,
    /// The files the overrides' merges read (every node's `inputs`), as the stack was last applied: a change to
    /// one merges again. An entry a layer makes by itself reads none, so applying the stack for such alone
    /// (`restack_entries`) leaves it as it is.
    merge_inputs: Shared<HashSet<HostPath>>,
    /// Where each entry is, by its uid: its real path.
    placed: Shared<HashMap<Uid, Arc<TreePath>>>,
    /// How many references name each uid, whatever they resolve to: a uid none names can come or go without any
    /// other asset's links changing.
    named: Shared<HashMap<Uid, usize>>,
    /// The working copies of assets with unsaved changes, by uid: shown in place of their files on disk.
    working: Shared<HashMap<Uid, Working>>,
    /// What the layers below each layer resolve (`below_layer`), as asked since the stack was last applied: made anew,
    /// not cleared, as the stack is applied, so a snapshot keeps what was so of it.
    beneath: Arc<Mutex<HashMap<usize, Arc<Beneath>>>>,
    /// Where what overrides merged is kept (`merged_file`), and what each merge made (`Memo`).
    merged: HostPath,
    /// The store keeping the versions overrides were made from (`changeset::Base`).
    objects: crate::chunks::Store,
}

impl Index {
    /// The project's I/O, as the index was opened with it.
    #[must_use]
    pub fn io(&self) -> &Arc<dyn pe2_io::Io> {
        &self.io
    }

    /// The index as it is now, for a snapshot: its maps handed over as copies of their own (`Shared::hand_over`),
    /// so that this one goes on changing them in place whatever holds the snapshot. What a copy costs is paid
    /// here, for the maps that changed since the last was handed over.
    #[must_use]
    pub fn hand_over(&mut self) -> Index {
        // What is resolved is the whole stack where nothing is hidden: one map, handed over once.
        let one = Shared::same(&self.resolved, &self.full);
        let full = self.full.hand_over();
        let resolved = if one { full.clone() } else { self.resolved.hand_over() };
        let assets = self.assets.hand_over();
        let entries = self.entries.hand_over();
        let by_uid = self.by_uid.hand_over();
        let referenced_by = self.referenced_by.hand_over();
        let records = self.records.hand_over();
        let hashes = self.hashes.hand_over();
        let winning = self.winning.hand_over();
        let merge_inputs = self.merge_inputs.hand_over();
        let placed = self.placed.hand_over();
        let named = self.named.hand_over();
        let working = self.working.hand_over();
        Index {
            assets,
            entries,
            by_uid,
            referenced_by,
            records,
            resolved,
            full,
            hashes,
            winning,
            merge_inputs,
            placed,
            named,
            working,
            versions: self.versions.clone(),
            read: self.read,
            dangling: self.dangling.clone(),
            problems: self.problems.clone(),
            properties: self.properties.clone(),
            layers: self.layers.clone(),
            opened: crate::model::Delta::default(),
            root: self.root.clone(),
            io: self.io.clone(),
            stack_problems: self.stack_problems.clone(),
            made_of: self.made_of.clone(),
            stack_said: self.stack_said.clone(),
            stack_taken: self.stack_taken,
            hidden: self.hidden.clone(),
            ops: self.ops.clone(),
            dependencies: self.dependencies.clone(),
            beneath: self.beneath.clone(),
            merged: self.merged.clone(),
            objects: self.objects.clone(),
        }
    }

    /// Whether a layer leaves a path out (an extension's folder, a name it ignores): no asset, whatever is on disk.
    #[must_use]
    pub fn leaves_out(&self, layer: &Name, path: &TreePath) -> bool {
        self.layers.iter().find(|l| l.name == layer).is_some_and(|l| l.place.ignores(path))
    }

    /// Index a project's layers, over what the project's model holds of their files (`model::known`): only files
    /// whose stamp changed are read. Unlisted entries of the layers that can be written are listed in their
    /// changesets. By whoever holds the project, as its cache says.
    ///
    /// # Errors
    /// A changeset cannot be read or written.
    pub fn open(project: &Project, cache: &Cache) -> Result<Index> {
        Index::open_reporting(project, cache, &|_, _, _| {})
    }

    /// Open the index as `open` does, saying how far it is: a stage, and how many of how many it has done.
    ///
    /// # Errors
    /// As `open`.
    pub fn open_reporting(project: &Project, _cache: &Cache, report: &Report<'_>) -> Result<Index> {
        let _span = tracing::info_span!("index open").entered();
        report("Reading what is known of the files", 0, 0);
        Index::over(crate::model::known(&project.root, project.io.filesystem()), true, project, report)
    }

    /// Open the index as `open_reporting` does, but taking the files to be as the model has them: none is looked
    /// at but those it knows nothing of. For whoever holds the project to stay, who holds the files to their
    /// records behind its first answers (`verification`) and before its first edit.
    ///
    /// # Errors
    /// As `open`.
    pub fn open_behind(project: &Project, _cache: &Cache, report: &Report<'_>) -> Result<Index> {
        let _span = tracing::info_span!("index open").entered();
        report("Reading what is known of the files", 0, 0);
        Index::over(crate::model::known(&project.root, project.io.filesystem()), false, project, report)
    }

    /// Open the index over nothing known: every file read. What an index is held to, and what one opens as where
    /// nothing may be known of the project.
    ///
    /// # Errors
    /// A layer cannot be read.
    pub fn without_store(project: &Project) -> Result<Index> {
        Index::over(crate::model::Known::default(), true, project, &|_, _, _| {})
    }

    /// The record of every file the layers hold, as it was last read: what the model the project's processes share
    /// holds of files (`model::fill_files`). A file two layers hold is given twice.
    pub fn file_records(&self) -> impl Iterator<Item = (&HostPath, &Record)> {
        let files = self.layers.iter().flat_map(|layer| layer.scan.all_files());
        files.filter_map(|file| Some((file, &**self.records.get(file)?)))
    }

    /// All the layers hold, and what each one's listing was made by, as a change of the project's model: said
    /// where the model holds nothing yet (`model::took`).
    #[must_use]
    pub fn layers_held(&self, root: &HostPath) -> crate::model::Delta {
        let mut model = crate::model::Delta::default();
        for layer in &self.layers {
            model.layer(&layer.name, Some(&layer.place.made_by()));
            Scan::differs(&Scan::default(), &layer.scan, root, &layer.name, &mut model);
        }
        model
    }

    /// The records of the layers stored as records, as they were last read, by the layers' names: what the
    /// project's model keeps of them, and is held to.
    #[must_use]
    pub fn records_known(&self) -> crate::model::Records {
        let Some(set) = crate::records::Set::current(&self.root) else { return crate::model::Records::new() };
        let of_layers = self.layers.iter().filter_map(|layer| {
            let read = set.records_read(&layer.place.root)?;
            let read = read
                .into_iter()
                .map(|(uid, (record, modified, len))| (uid, (record, crate::model::stamp_of(modified), len)));
            // A layer with no record is none the model knows of.
            Some((layer.name.clone(), read.collect::<BTreeMap<_, _>>())).filter(|(_, read)| !read.is_empty())
        });
        of_layers.collect()
    }

    /// All the records of the layers stored as records, as a change of the project's model: said where the model
    /// holds nothing yet (`model::took`).
    #[must_use]
    pub fn records_held(&self) -> crate::model::Delta {
        let mut model = crate::model::Delta::default();
        for (layer, records) in self.records_known() {
            for (uid, recorded) in &records {
                model.record(&layer, *uid, Some(recorded));
            }
        }
        model
    }

    /// What the layers hold, as the project's model keeps it: what the model is held to.
    #[must_use]
    pub fn layers_known(&self) -> crate::model::Layers {
        self.layers.iter().map(|layer| (layer.name.clone(), (layer.place.made_by(), layer.scan.held()))).collect()
    }

    /// Open the index over what is known of the project, taken as it is: what `open_behind` does over what the
    /// project's model holds, for what holds another model to it.
    ///
    /// # Errors
    /// As `open`.
    #[cfg(test)]
    pub(crate) fn open_over(known: crate::model::Known, project: &Project) -> Result<Index> {
        Index::over(known, false, project, &|_, _, _| {})
    }

    /// Whether its opening took the stack as the project's model had it, where it applied none.
    #[must_use]
    pub fn took_stack(&self) -> bool {
        self.stack_taken
    }

    /// What its opening read that differs from what it opened over, taken out: said in the project's model by
    /// whoever holds the project (`model::took`).
    pub fn take_opened(&mut self) -> crate::model::Delta {
        std::mem::take(&mut self.opened)
    }

    /// What tells which files are not as their records say - changed, gone, or read in the moment they were
    /// changed in - for a thread of its own: what an index opened behind (`open_behind`) is held to. What it
    /// returns is taken in as a change of those files (`refresh`).
    pub fn verification(&self) -> impl FnOnce() -> Vec<HostPath> + Send + 'static {
        let records = self.records.clone();
        let io = self.io.clone();
        let files: Vec<HostPath> = self.layers.iter().flat_map(|layer| layer.scan.all_files().cloned()).collect();
        let layers: Vec<(Place, Arc<Scan>)> =
            self.layers.iter().map(|layer| (layer.place.clone(), layer.scan.clone())).collect();
        let set = crate::records::Set::current(&self.root);
        // What the layer edits go to stands on - the highest that can be written, no mounted tree - made beside it:
        // the first edit asks for it, and waits for all of this. Kept where this index and every one made of it
        // finds it.
        let stands_on =
            self.layers.iter().rev().find(|layer| layer.writable && !layer.mounted).map(|layer| layer.name.clone());
        let beneath = stands_on.map(|layer| (self.clone(), layer));
        move || {
            // Each on a thread of its own: an edit waits for all of it.
            let (of_records, walked, mut stale) = std::thread::scope(|scope| {
                if let Some((index, layer)) = &beneath {
                    scope.spawn(move || {
                        let _span = tracing::info_span!("index: what the layer edits go to stands on").entered();
                        drop(index.below_layer(layer));
                    });
                }
                // The records' files, against what was taken as known of each.
                let of_records = scope.spawn(|| {
                    let _span = tracing::info_span!("index: the records held to their files").entered();
                    set.map_or_else(Vec::new, |set| set.differing())
                });
                // What each layer holds, walked now, against what was taken as known of it: where they differ.
                let walked = scope.spawn(|| {
                    let _span = tracing::info_span!("index: the layers held to what was known of them").entered();
                    let differing = layers.iter().flat_map(|(place, known)| {
                        let mut now = Scan::default();
                        now.rescan(place, TreePath::root());
                        now.differing(known, place)
                    });
                    differing.collect::<Vec<HostPath>>()
                });
                let _span = tracing::info_span!("index: the files held to their records").entered();
                let stale = files.iter().zip(stat_all(&io, &files)).filter(|(file, (size, modified))| {
                    match (size, records.get(*file)) {
                        (Some(size), Some(record)) => {
                            !(record.size == *size && record.modified == *modified && *modified < record.recorded)
                        }
                        (None, None) => false,
                        _ => true,
                    }
                });
                let stale: Vec<HostPath> = stale.map(|(file, _)| file.clone()).collect();
                (of_records.join().unwrap_or_default(), walked.join().unwrap_or_default(), stale)
            });
            stale.extend(walked);
            stale.extend(of_records);
            stale.sort();
            stale.dedup();
            stale
        }
    }

    /// The index of the project as `project.toml` has it now, opened over what this one knows of the files; this
    /// one goes on answering until it is let go.
    ///
    /// # Errors
    /// A layer cannot be read.
    pub fn again(&self, project: &Project, report: &Report<'_>) -> Result<Index> {
        let files = self.records.iter().map(|(file, record)| (file.clone(), (**record).clone())).collect();
        let layers =
            self.layers.iter().map(|layer| (layer.name.clone(), (layer.place.made_by(), layer.scan.held()))).collect();
        let known =
            crate::model::Known { files, layers, records: self.records_known(), stack: Some(self.stack_known()) };
        let mut index = Index::over(known, true, project, report)?;
        index.set_working(HashMap::clone(&self.working));
        Ok(index)
    }

    /// Over what is `known` of the files: each held to its file where `held`, else taken as it is.
    #[expect(clippy::too_many_lines, reason = "the index's fields, one line each")]
    fn over(known: crate::model::Known, held: bool, project: &Project, report: &Report<'_>) -> Result<Index> {
        let crate::model::Known {
            files: mut known,
            layers: mut known_layers,
            records: mut known_records,
            stack: known_stack,
        } = known;
        // What differs of the project's model from what was known, for whoever holds the project to say there.
        let mut opened = crate::model::Delta::default();
        // Its records layers, placed through the stack: what their scans and changesets are read from. Over what
        // is known of their records - held to their files, or taken as it is.
        let of_layers = project.layers.iter().filter_map(|layer| {
            let known = known_records.get(&layer.name)?;
            let read = known
                .iter()
                .map(|(uid, (record, modified, len))| (*uid, (record.clone(), crate::model::time_of(*modified), *len)));
            Some((layer.path.clone(), read.collect()))
        });
        let set = crate::records::Set::open_over(project, of_layers.collect(), !held)?;
        for layer in &project.layers {
            let was = known_records.remove(&layer.name).unwrap_or_default();
            if let Some(now) = set.records_read(&layer.path) {
                records_differ(&was, &now, &layer.name, &mut opened);
            }
        }
        for (layer, gone) in known_records {
            for uid in gone.keys() {
                opened.record(&layer, *uid, None);
            }
        }
        let mut layers = Vec::new();
        for (i, layer) in project.layers.iter().enumerate() {
            report(&format!("Scanning {}", layer.name), i, project.layers.len());
            let _span = tracing::info_span!("layer scan").entered();
            let was = known_layers.remove(&layer.name).map(|(made_by, holds)| (made_by, Scan::of_held(holds)));
            let was_made_by = was.as_ref().map(|(made_by, _)| made_by.clone());
            // Held to the layer, it is walked whatever is known of it; else what is known is what it holds.
            let (state, walked) = LayerState::new(layer, was, !held, &project.io)?;
            let made_by = state.place.made_by();
            if was_made_by.as_deref() != Some(made_by.as_str()) {
                opened.layer(&layer.name, Some(&made_by));
            }
            if let Some(before) = walked {
                Scan::differs(&before, &state.scan, &project.root, &layer.name, &mut opened);
            }
            layers.push(state);
        }
        // What was known of layers the project has no more.
        for (layer, (_, holds)) in known_layers {
            for path in holds.keys() {
                opened.held(&project.root, &layer, path, None);
            }
            opened.layer(&layer, None);
        }
        let mut index = Index {
            assets: Shared::default(),
            versions: Arc::default(),
            entries: Shared::default(),
            read: 0,
            by_uid: Shared::default(),
            referenced_by: Shared::default(),
            dangling: vec![],
            problems: vec![],
            properties: project
                .property_problems
                .iter()
                .map(|p| Problem::Property {
                    config: p.config.as_ref().map(ToString::to_string),
                    property: p.property.clone(),
                    why: p.why.clone(),
                })
                .collect(),
            layers,
            records: Shared::default(),
            merged: project.root.join(&crate::cache::DIR).join(&Name::constant("merged")),
            objects: crate::chunks::Store::at(&project.root, project.io.clone()),
            opened,
            root: project.root.clone(),
            io: project.io.clone(),
            stack_problems: vec![],
            made_of: stack_made_of(project),
            stack_said: Arc::default(),
            stack_taken: false,
            resolved: Shared::default(),
            hidden: HashSet::new(),
            ops: vec![],
            dependencies: vec![],
            hashes: Shared::default(),
            winning: Shared::default(),
            merge_inputs: Shared::default(),
            placed: Shared::default(),
            named: Shared::default(),
            working: Shared::default(),
            beneath: Arc::default(),
            full: Shared::default(),
        };
        let files: Vec<HostPath> = index.layers.iter().flat_map(|l| l.scan.all_files().cloned()).collect();
        report("Reading the files that changed", 0, files.len());
        let read = if held {
            let _span = tracing::info_span!("index stat and read").entered();
            index.load_records(&files, |path, size, modified| {
                known.remove(path).filter(|r| r.size == size && r.modified == modified && modified < r.recorded)
            })
        } else {
            let _span = tracing::info_span!("index: records taken as known").entered();
            // As known, each: only those nothing is known of are looked at, and read.
            let records = Shared::make_mut(&mut index.records);
            let mut unknown = Vec::new();
            for file in &files {
                match known.remove(file) {
                    Some(record) => drop(records.insert(file.clone(), Arc::new(record))),
                    // One two layers hold is known once.
                    None if records.contains_key(file) => {}
                    None => unknown.push(file.clone()),
                }
            }
            index.load_records(&unknown, |_, _, _| None)
        };
        index.read = read.len();
        // What it read anew, and what is left of what was known and no layer holds any more: what differs of the
        // project's model, for whoever holds the project to say there.
        for file in known.keys() {
            index.opened.file(&project.root, file, None);
        }
        for file in &read {
            index.opened.file(&project.root, file, index.records.get(file).map(|record| &**record));
        }
        report("Resolving the layers", 0, 0);
        // The stack as the model has it, where it was made of these layers and nothing it is made of was found
        // otherwise than known - by an opening that takes what is known as it is; else applied.
        let taken = !held && index.opened.is_empty() && {
            let _span = tracing::info_span!("index: the stack taken as known").entered();
            known_stack.as_ref().is_some_and(|stack| index.take_stack(stack))
        };
        index.stack_taken = taken;
        if !taken {
            {
                let _span = tracing::info_span!("index apply stack").entered();
                index.apply_stack()?;
            }
            // And what differs of it from what was known is said.
            let mut differs = std::mem::take(&mut index.opened);
            index.stack_differs(known_stack.as_ref(), &mut differs);
            index.opened = differs;
        }
        report("Assembling the assets", 0, 0);
        let paths: Vec<Arc<TreePath>> = index.resolved.keys().cloned().collect();
        {
            let _span = tracing::info_span!("index assemble").entered();
            index.assemble(&paths);
        }
        {
            let _span = tracing::info_span!("index link").entered();
            index.link();
        }
        Ok(index)
    }

    /// What is served as files of the project (`vfs`): the whole stack - whatever the view hides - each folder, plain
    /// file and node by its path (a node's without `.node`), then each node's files in its folder, but where an entry of
    /// the same name holds that place. Files as they are on disk, never an unsaved working copy.
    #[must_use]
    pub fn served(&self) -> crate::server::Listing {
        let _span = tracing::info_span!("served: listing").entered();
        let mut listing = crate::server::Listing::new();
        for (path, node) in self.full.iter() {
            self.serve_entry(path, node, &mut |path, shown| drop(listing.insert(path, shown)));
        }
        for (path, node) in self.full.iter().filter(|(_, n)| n.kind == Kind::Asset) {
            self.serve_files(path, node, &mut |path, shown| drop(listing.insert(path, shown)));
        }
        listing
    }

    /// What follows the stack applied again, by what differs from before: which assets changed - their entries, or
    /// their content, in what was rescanned - assembled and linked again; and which entries of the whole stack
    /// differ, for what is served (`served_differences`). Where no asset changed, the links stand as they were.
    fn after_restack(
        &mut self,
        regions: &[(usize, Arc<TreePath>)],
        (before, whole_before): (&Resolved, &Resolved),
        unreadable: Vec<Problem>,
        refreshed: &mut Refreshed,
    ) {
        let rescanned: Vec<HostPath> = regions.iter().map(|(i, at)| self.layers[*i].place.disk(at)).collect();
        // The layer filling each layer, where one does.
        let filled: Vec<Option<usize>> = (0..self.layers.len()).map(|layer| self.filled_by(layer)).collect();
        let touched = |node: &Node| in_rescanned(node, regions, &rescanned, &filled);
        tracing::info_span!("after: what is served compared")
            .in_scope(|| self.served_differences(whole_before, &touched, refreshed));
        let scanning = tracing::info_span!("after: every asset looked at").entered();
        // An asset changed when its entry did, or when its content - or what a generated layer gives it - lies in what
        // was rescanned.
        for (path, node) in self.resolved.iter().filter(|(_, node)| node.kind == Kind::Asset) {
            if before.get(path) != Some(node) || touched(node) {
                refreshed.assets.insert(asset_of(path));
            }
        }
        refreshed.assets.extend(self.assets.keys().filter(|a| !self.resolved.contains_key(&bundle_named(a))).cloned());
        drop(scanning);
        let paths: Vec<Arc<TreePath>> = refreshed.assets.iter().map(|asset| bundle_named(asset)).collect();
        tracing::info_span!("after: entries and assets assembled").in_scope(|| self.assemble(&paths));
        if refreshed.assets.is_empty() {
            self.problems.extend(unreadable);
        } else {
            tracing::info_span!("after: linked").in_scope(|| self.link());
        }
    }

    /// Which entries of the whole stack differ after the stack was applied again - changed, new, or with content in
    /// what was rescanned - and which are no more: what is served may differ of those alone. A node an entry came
    /// or went in is among them: its files show where no entry of the same name holds the place.
    fn served_differences(&self, before: &Resolved, touched: &impl Fn(&Node) -> bool, refreshed: &mut Refreshed) {
        let _span = tracing::info_span!("refresh: what is served differs").entered();
        for (path, node) in self.full.iter() {
            if before.get(path) != Some(node) || touched(node) {
                refreshed.touched.insert(path.clone());
            }
        }
        refreshed.gone.extend(before.keys().filter(|path| !spelled(&self.full, path)).cloned());
        refreshed.touched.retain(|path| self.full.contains_key(path));
        // The node an entry came or went in.
        let around: Vec<Arc<TreePath>> = refreshed
            .touched
            .iter()
            .chain(&refreshed.gone)
            .filter(|path| spelled(before, path) != spelled(&self.full, path))
            .filter_map(|path| asset_of(path).parent().filter(|parent| !parent.is_root()).map(TreePath::to_owned))
            .map(|parent| bundle_named(&parent))
            .filter(|parent| self.full.contains_key(parent))
            .collect();
        refreshed.touched.extend(around);
    }

    /// Bring a listing of what is served (`served`) up to what a refresh changed - the entries it touched, those
    /// that are gone - and say what of it differs now: what it costs follows what changed, not what the project
    /// holds.
    pub fn serve_changed(&self, listing: &mut crate::server::Listing, refreshed: &Refreshed) -> crate::server::Delta {
        use crate::server::{Listing, Shown};
        let mut delta = crate::server::Delta::default();
        // The files a node has in a listing: those in its folder that are its own, not those of a node inside it.
        // Paths order name by name: what is in its folder follows it.
        let files_of = |listing: &Listing, name: &TreePath| -> Vec<Arc<TreePath>> {
            let inside =
                listing.range::<TreePath, _>(pe2_path::from(name)).take_while(|(path, _)| path.starts_with(name));
            let own = |path: &TreePath| {
                let mut at = path.parent();
                while let Some(above) = at.filter(|above| !above.is_root()) {
                    if matches!(listing.get(above), Some(Shown::Node)) {
                        return above == name;
                    }
                    at = above.parent();
                }
                false
            };
            let files = inside.filter(|(path, it)| matches!(it, Shown::File { of_files: true, .. }) && own(path));
            files.map(|(path, _)| path.clone()).collect()
        };
        for path in &refreshed.gone {
            let name = asset_of(path);
            let mut going = files_of(listing, &name);
            going.push(name);
            for path in going {
                if let Some((path, _)) = listing.remove_entry(&*path) {
                    delta.gone.insert(path);
                }
            }
        }
        for path in &refreshed.touched {
            let Some(node) = self.full.get(path) else { continue };
            let name = asset_of(path);
            // What it had, to tell what of it differs: its own entry, and a node's files - each as it was written.
            let mut had = Listing::new();
            let mut held = if node.kind == Kind::Asset { files_of(listing, &name) } else { Vec::new() };
            held.push(name);
            for path in held {
                if let Some((path, it)) = listing.remove_entry(&*path) {
                    had.insert(path, it);
                }
            }
            let mut put = |path: Arc<TreePath>, it: Shown| {
                let was = had.remove_entry(&*path);
                if was.is_none_or(|(was, then)| then != it || was.as_str() != path.as_str()) {
                    delta.gone.remove(&*path);
                    crate::server::put(&mut delta.shown, &path, &it);
                }
                crate::server::put(listing, &path, &it);
            };
            self.serve_entry(path, node, &mut put);
            if node.kind == Kind::Asset {
                self.serve_files(path, node, &mut put);
            }
            delta.gone.extend(had.into_keys().filter(|path| !listing.contains_key(&**path)));
        }
        delta
    }

    /// A file as it is served: where a layer holds it, its length, when it changed, and where its content lives.
    fn serve_file(&self, disk: HostPath, of_files: bool) -> crate::server::Shown {
        let (size, modified) =
            self.file_stat(&disk).or_else(|| crate::content::stat(&self.io, &disk).ok()).unwrap_or((0, UNIX_EPOCH));
        let time = modified.duration_since(UNIX_EPOCH).unwrap_or_default();
        let source = crate::records::Set::source(&disk).unwrap_or_default();
        let modified = (time.as_secs(), time.subsec_nanos());
        crate::server::Shown::File { disk, size, modified, of_files, source }
    }

    /// An entry of the stack as it is served: a folder, a plain file, or a node - the folder of its name.
    fn serve_entry(&self, path: &TreePath, node: &Node, list: &mut impl FnMut(Arc<TreePath>, crate::server::Shown)) {
        use crate::server::Shown;
        match node.kind {
            Kind::Folder => list(path.to_owned(), Shown::Folder),
            Kind::File => {
                if let Some((_, disk, _)) = self.node_files(node).into_iter().next() {
                    list(path.to_owned(), self.serve_file(disk, false));
                }
            }
            Kind::Asset => list(asset_of(path), Shown::Node),
        }
    }

    /// A node's files as they are served, in its folder - but where an entry of the same name holds that place.
    fn serve_files(&self, path: &TreePath, node: &Node, list: &mut impl FnMut(Arc<TreePath>, crate::server::Shown)) {
        let name = asset_of(path);
        for (file, disk, _) in self.node_files(node) {
            // Where an entry of the name of its first part holds that place, it is not shown.
            if let Some(first) = file.parts().next() {
                let child = name.join(first);
                if self.full.contains_key(&child) || self.full.contains_key(&bundle_named(&child)) {
                    continue;
                }
            }
            list(name.join_path(&file), self.serve_file(disk, true));
        }
    }

    /// A file's length and time as the index read them; none for a file it has not read.
    #[must_use]
    pub fn file_stat(&self, path: &HostPath) -> Option<(u64, SystemTime)> {
        let record = self.records.get(path)?;
        Some((record.size, UNIX_EPOCH + std::time::Duration::new(record.modified.0, record.modified.1)))
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
    pub fn refresh(&mut self, project: &Project, changed: &[HostPath]) -> Result<Refreshed> {
        let _span = tracing::info_span!("index refresh").entered();
        // A packed layer's listing changed (written by another process, or this one): read again.
        crate::pack::reread_changed(changed);
        // A manifest changing is the file it stands for changing; a packed layer's listing, its whole layer.
        let changed: Vec<HostPath> = changed
            .iter()
            .map(|p| match p.file_name() {
                Some(name) if name == crate::pack::FILE => p.parent().unwrap_or_else(|| p.clone()),
                _ => crate::chunks::logical(p).unwrap_or_else(|| p.clone()),
            })
            .collect();
        let mut refreshed = self.refresh_files(project, &changed)?;
        self.say_stack(&mut refreshed);
        Ok(refreshed)
    }

    /// What a refresh changed of the stack, said of the project's model: the entries it touched, as they are now,
    /// and those gone; and what the stack says besides, where that differs from what was last said.
    fn say_stack(&mut self, refreshed: &mut Refreshed) {
        for path in &refreshed.touched {
            let node = self.full.get(path).map(|node| node.written(&self.root));
            refreshed.model.node(path.as_str(), node.as_deref());
        }
        for path in refreshed.gone.iter().filter(|path| !spelled(&self.full, path)) {
            refreshed.model.node(path.as_str(), None);
        }
        let says = self.stack_says();
        for ((name, now), was) in
            [SAID_OPS, SAID_PROBLEMS, SAID_DEPENDENCIES].iter().zip(&says).zip(self.stack_said.iter())
        {
            if now != was {
                refreshed.model.said(name, now);
            }
        }
        if says != *self.stack_said {
            self.stack_said = Arc::new(says);
        }
    }

    /// What the stack says of its operations, its problems and what the layers need of each other, written.
    fn stack_says(&self) -> [Vec<u8>; 3] {
        [said(&self.ops), said(&self.stack_problems), said(&self.dependencies)]
    }

    /// The stack applied, as the project's model keeps it: what the model is held to, and what a model that holds
    /// none is given.
    #[must_use]
    pub fn stack_known(&self) -> crate::model::Stack {
        let nodes = self.full.iter().map(|(path, node)| (path.to_string(), node.written(&self.root)));
        let says = [SAID_OPS, SAID_PROBLEMS, SAID_DEPENDENCIES].iter().zip(self.stack_says());
        crate::model::Stack {
            made_of: self.made_of.clone(),
            nodes: nodes.collect(),
            said: says.map(|(name, said)| ((*name).to_string(), said)).collect(),
        }
    }

    /// The stack applied, whole, as a change of the project's model: said where the model holds nothing yet
    /// (`model::took`).
    #[must_use]
    pub fn stack_held(&self) -> crate::model::Delta {
        let mut model = crate::model::Delta::default();
        self.stack_differs(None, &mut model);
        model
    }

    /// What differs between the stack known and the stack as it is, said of the project's model.
    fn stack_differs(&self, known: Option<&crate::model::Stack>, model: &mut crate::model::Delta) {
        let now = self.stack_known();
        let empty = crate::model::Stack::default();
        let was = known.unwrap_or(&empty);
        if known.is_none_or(|known| known.made_of != now.made_of) {
            model.stack_made_of(&now.made_of);
        }
        for path in was.nodes.keys().filter(|path| !now.nodes.contains_key(*path)) {
            model.node(path, None);
        }
        for (path, node) in now.nodes.iter().filter(|(path, node)| was.nodes.get(*path) != Some(node)) {
            model.node(path, Some(node));
        }
        for (name, said) in now.said.iter().filter(|(name, said)| was.said.get(*name) != Some(said)) {
            model.said(name, said);
        }
    }

    /// Take the stack as the model has it, where it was made of this project's layers and all of it reads: what
    /// applying the stack would come to, the files, the layers' listings and their records being as known. False
    /// where it cannot be taken - nothing is changed then, and the stack is applied.
    fn take_stack(&mut self, stack: &crate::model::Stack) -> bool {
        if stack.made_of != self.made_of {
            return false;
        }
        // Each path checked as it is read back: a stack naming one that is none is not taken, and is applied anew.
        let node = |(path, bytes): (&String, &Vec<u8>)| {
            Some((TreePath::new(path).ok()?.to_owned(), Node::read(&self.root, bytes)?))
        };
        let nodes: Option<Resolved> = stack.nodes.iter().map(node).collect();
        let layers = self.layers.len();
        let within = |node: &Node| {
            let layers_of =
                [node.home, node.content.0].into_iter().chain(node.overrides.iter().copied()).chain(node.moved);
            layers_of
                .chain(node.files.iter().flat_map(|files| files.iter().map(|(_, _, held)| *held)))
                .all(|layer| layer < layers)
        };
        let Some(nodes) = nodes.filter(|nodes| nodes.values().all(within)) else { return false };
        let says = [SAID_OPS, SAID_PROBLEMS, SAID_DEPENDENCIES].map(|name| stack.said.get(name));
        let [Some(ops), Some(problems), Some(dependencies)] = says else { return false };
        let (Some(ops), Some(problems), Some(dependencies)) =
            (said_of::<Vec<OpStatuses>>(ops), said_of::<Vec<Problem>>(problems), said_of(dependencies))
        else {
            return false;
        };
        if ops.len() != layers {
            return false;
        }
        self.beneath = Arc::default();
        self.stack_said = Arc::new([said(&ops), said(&problems), said(&dependencies)]);
        self.ops = ops;
        self.stack_problems.clone_from(&problems);
        self.problems = problems;
        self.problems.extend(self.properties.iter().cloned());
        self.dependencies = dependencies;
        self.full = Shared::new(nodes);
        self.resolved = self.full.clone();
        self.remember_hashes();
        self.winning =
            Shared::new(self.resolved.iter().map(|(path, node)| (node.content.clone(), path.clone())).collect());
        self.merge_inputs = Shared::new(self.resolved.values().flat_map(|node| node.inputs.iter().cloned()).collect());
        true
    }

    /// Records changed: the project's are read again, and each records layer as they make it now, rescanned whole.
    /// Whether any records layer was.
    /// Take in what changed of the records layers: their records read again (those whose files are as they were taken
    /// as they were), their changesets made again - each changed where it says otherwise - and the entries whose records
    /// changed scanned again where they were and where they are, in their layer and, where any is placed otherwise, in
    /// the records layers above it, whose entries may be placed through it. Whether a changeset changed.
    fn take_in_records(
        &mut self,
        project: &Project,
        changed: &[HostPath],
        wanted: &mut Vec<(usize, Arc<TreePath>)>,
        were: &mut Vec<(usize, Changeset)>,
        model: &mut crate::model::Delta,
    ) -> Result<bool> {
        let before = crate::records::Set::current(&project.root);
        let now = crate::records::Set::open_after(project, changed)?;
        let differences = tracing::info_span!("refresh: the records compared")
            .in_scope(|| crate::records::Set::differences(before.as_deref(), &now));
        // What that changed of the project's model: each record that came, went or changed, as it is now.
        for (dir, uid) in &differences.records {
            if let Some(layer) = project.layers.iter().find(|layer| layer.path == *dir) {
                let recorded = now
                    .recorded(dir, *uid)
                    .map(|(record, modified, len)| (record, crate::model::stamp_of(modified), len));
                model.record(&layer.name, *uid, recorded.as_ref());
            }
        }
        let _span = tracing::info_span!("refresh: the changesets made again").entered();
        let (changed, placed) = (&differences.paths, differences.placed);
        // An override changed merges again as the stack is applied.
        let any = differences.overrides;
        let mut lowest_changed = None;
        for (i, layer) in self.layers.iter_mut().enumerate() {
            // A layer whose records are as they were says of itself what it said.
            if !crate::records::is_records(self.io.filesystem(), &layer.place.root)
                || differences.same.contains(&layer.place.root)
            {
                continue;
            }
            let was = layer.changeset.clone();
            layer.reload_changeset()?;
            if layer.changeset != was {
                were.push((i, was));
            }
            if let Some((_, paths)) = changed.iter().find(|(dir, _)| *dir == layer.place.root) {
                lowest_changed.get_or_insert(i);
                wanted.extend(paths.iter().map(|path| (i, path.clone())));
            }
        }
        if placed && let Some(lowest) = lowest_changed {
            let paths: BTreeSet<&TreePath> = changed.iter().flat_map(|(_, paths)| paths).map(|path| &**path).collect();
            for (i, layer) in self.layers.iter().enumerate().skip(lowest + 1) {
                if crate::records::is_records(self.io.filesystem(), &layer.place.root) {
                    wanted.extend(paths.iter().map(|path| (i, (*path).to_owned())));
                }
            }
        }
        Ok(any)
    }

    fn refresh_files(&mut self, project: &Project, changed: &[HostPath]) -> Result<Refreshed> {
        let mut refreshed = Refreshed::default();
        let mut changesets = false;
        let mut wanted: Vec<(usize, Arc<TreePath>)> = Vec::new();
        // The records layers whose changesets changed, with what each said before.
        let mut were: Vec<(usize, Changeset)> = Vec::new();
        if changed.iter().any(|p| in_records(p) && !of_extensions(p)) {
            changesets |= self.take_in_records(project, changed, &mut wanted, &mut were, &mut refreshed.model)?;
        }
        for path in changed {
            // `project.toml` is read again whole, records with their layers (above).
            if *path == project.root.join(&FILE) || in_records(path) {
                refreshed.project_file |= *path == project.root.join(&FILE);
                continue;
            }
            for (i, layer) in self.layers.iter_mut().enumerate() {
                if *path == layer.changeset_path {
                    layer.reload_changeset()?;
                    changesets = true;
                    continue;
                }
                let Some(rel) = layer.place.looked_at(path) else { continue };
                // A change inside a bundle rescans the bundle; anything else, what is at the path (the whole layer at
                // a mount's point).
                let bundle = if layer.place.assets { bundle_at(&rel) } else { None };
                let at = bundle.unwrap_or(rel);
                wanted.push((i, if at == layer.place.at { TreePath::root().to_owned() } else { at }));
            }
        }
        // Each region once: one within another of its layer is scanned with it.
        let within = |a: &TreePath, b: &TreePath| a.starts_with(b);
        wanted.sort();
        wanted.dedup();
        let regions: Vec<(usize, Arc<TreePath>)> = wanted
            .iter()
            .filter(|(i, a)| !wanted.iter().any(|(j, b)| j == i && b != a && within(a, b)))
            .cloned()
            .collect();
        // What the entries changed declare now, before their records are read again: what a link takes back.
        let mut before_references = HashMap::new();
        for (i, at) in &regions {
            for path in self.layers[*i].scan.under(at).bundles.keys() {
                if let Some(resolved) = self.winning.get(&(self.entries_of(*i), path.clone())) {
                    let asset = asset_of(resolved);
                    before_references.insert(asset.clone(), self.references_from(&asset));
                }
            }
        }
        let mut gone = Vec::new();
        let mut files = Vec::new();
        let mut paths_same = true;
        let rescanning = tracing::info_span!("refresh: regions scanned and read").entered();
        for (i, at) in &regions {
            let layer = &mut self.layers[*i];
            let before = layer.scan.under(at);
            Arc::make_mut(&mut layer.scan).rescan(&layer.place, at);
            let after = layer.scan.under(at);
            Scan::differs(&before, &after, &project.root, &layer.name, &mut refreshed.model);
            // And the folders above it, which what came or went there may have made or taken away.
            for (folder, there) in Arc::make_mut(&mut layer.scan).folders_above(&layer.place, at) {
                let held = there.then_some(&crate::model::Held::Folder);
                refreshed.model.held(&project.root, &layer.name, folder.as_str(), held);
                paths_same = false;
            }
            paths_same &= before.same_paths(&after);
            let now: HashSet<&HostPath> = after.all_files().collect();
            for file in before.all_files().filter(|f| !now.contains(f)) {
                let elsewhere = self.layers.iter().enumerate().any(|(j, l)| j != *i && l.scan.holds(&l.place, file));
                if !elsewhere {
                    gone.push(file.clone());
                }
            }
            files.extend(after.all_files().cloned());
        }
        for file in &gone {
            Shared::make_mut(&mut self.records).remove(file);
        }
        let reported: HashSet<&HostPath> = changed.iter().collect();
        let (read, vanished) =
            tracing::info_span!("refresh: files looked at and read").in_scope(|| self.reload(&files, &reported));
        gone.extend(vanished);
        self.read = read.len();
        // What that changed of the model the project's processes share: those gone first, one of them may have
        // come back.
        for file in &gone {
            refreshed.model.file(&project.root, file, None);
        }
        for file in &read {
            refreshed.model.file(&project.root, file, self.records.get(file).map(|record| &**record));
        }
        drop(rescanning);
        // An override's files are merged as the stack is applied: one of them, or what is below it, changed applies it
        // again.
        let patched = changed.iter().any(|p| self.merge_inputs.contains(p));
        if paths_same && !changesets && were.is_empty() && !patched && self.contents_only(&regions) {
            self.refresh_contents(&regions, &before_references, &mut refreshed);
            return Ok(refreshed);
        }
        refreshed.restacked = true;
        // One layer's entries came, went or moved, and nothing else: the stack is as it was but for those.
        if let [(layer, was)] = were.as_slice()
            && !changesets
            && !patched
            && self.restack_entries(*layer, was, &regions, &before_references, &mut refreshed)
        {
            return Ok(refreshed);
        }
        let before = std::mem::take(&mut self.resolved);
        let whole_before = self.full.clone();
        // What says an asset cannot be read is the link's to say; where no asset changed, it says what it said.
        let unreadable = self.problems.iter().filter(|p| matches!(p, Problem::Unreadable { .. })).cloned().collect();
        tracing::info_span!("index apply stack").in_scope(|| self.apply_stack())?;
        self.after_restack(&regions, (&before, &whole_before), unreadable, &mut refreshed);
        Ok(refreshed)
    }

    /// The stack applied for the entries one layer made, moved or let go of, where its changeset changed in nothing
    /// else and nothing could say otherwise of them: what applying the whole stack again comes to, at the cost of
    /// those entries. False where that cannot be told - nothing is changed then, and the stack is applied whole.
    ///
    /// It can be told where: every layer is shown; the layer is a records layer nothing fills, with no layer above
    /// it creating or doing anything; none of the stack's problems is this layer's or at these paths; each entry gone
    /// was the layer's own, as it made it, and nothing is held where it was; each entry come is held, at a path
    /// nothing resolves to, under a uid no layer lists.
    fn restack_entries(
        &mut self,
        at: usize,
        was: &Changeset,
        regions: &[(usize, Arc<TreePath>)],
        before_references: &HashMap<Arc<TreePath>, Vec<Reference>>,
        refreshed: &mut Refreshed,
    ) -> bool {
        let Some(entries) = self.entries_alone(at, was) else { return false };
        self.restack(at, &entries, regions, before_references, refreshed);
        true
    }

    /// The entries a layer's changeset lets go of and makes, against what it said before, each made with its kind -
    /// where those are all that changed and the stack can be applied for them alone (`restack_entries`).
    fn entries_alone(&self, at: usize, was: &Changeset) -> Option<Restacked> {
        let layer = &self.layers[at];
        let now = &layer.changeset;
        let silent = |l: &LayerState| {
            l.kind == LayerKind::Generated || (l.changeset.entries.is_empty() && l.changeset.ops.is_empty())
        };
        if !self.hidden.is_empty()
            || was.ops != now.ops
            || was.read_only != now.read_only
            || was.ignore != now.ignore
            || layer.kind == LayerKind::Generated
            || !crate::records::is_records(self.io.filesystem(), &layer.place.root)
            || self.filled_by(at).is_some()
            || !self.layers[at + 1..].iter().all(silent)
        {
            return None;
        }
        let differs = |a: &Changeset, b: &Changeset| -> Vec<(Uid, Arc<TreePath>)> {
            let other = a.entries.iter().filter(|(uid, path)| b.entries.get(*uid) != Some(*path));
            other.map(|(uid, path)| (*uid, path.clone())).collect()
        };
        let (gone, came) = (differs(was, now), differs(now, was));
        let uids: HashSet<Uid> = gone.iter().chain(&came).map(|(uid, _)| *uid).collect();
        let paths: HashSet<&str> = gone.iter().chain(&came).map(|(_, path)| path.as_str()).collect();
        let came_at: HashSet<&TreePath> = came.iter().map(|(_, path)| &**path).collect();
        if uids.is_empty()
            || came_at.len() != came.len()
            || uids.iter().any(|uid| now.ops.contains_key(uid))
            || self.problems.iter().any(|problem| problem.of_stack(&layer.name, &paths))
        {
            return None;
        }
        for (uid, path) in &gone {
            let own = self.full.get(path).is_some_and(|node| {
                node.uid == *uid
                    && node.home == at
                    && node.content.0 == at
                    && node.content.1 == *path
                    && node.overrides.is_empty()
                    && node.moved.is_none()
                    && node.files.is_none()
            });
            if !own || (!came_at.contains(&**path) && layer.scan.kind(path).is_some()) {
                return None;
            }
        }
        let gone_at: HashSet<&TreePath> = gone.iter().map(|(_, path)| &**path).collect();
        let gone_uids: HashSet<Uid> = gone.iter().map(|(uid, _)| *uid).collect();
        let mut kinds = Vec::with_capacity(came.len());
        for (uid, path) in &came {
            let kind = layer.scan.kind(path)?;
            let free = !self.full.contains_key(path) || gone_at.contains(&**path);
            let listed = |l: &LayerState| l.changeset.entries.contains_key(uid);
            let elsewhere = self.layers.iter().enumerate().any(|(i, l)| i != at && listed(l));
            if !free || elsewhere || (was.entries.contains_key(uid) && !gone_uids.contains(uid)) {
                return None;
            }
            kinds.push(kind);
        }
        Some(Restacked { gone, came, kinds })
    }

    /// Change the stack for entries a layer let go of and made (`entries_alone`).
    fn restack(
        &mut self,
        at: usize,
        Restacked { gone, came, kinds }: &Restacked,
        regions: &[(usize, Arc<TreePath>)],
        before_references: &HashMap<Arc<TreePath>, Vec<Reference>>,
        refreshed: &mut Refreshed,
    ) {
        let uids: HashSet<Uid> = gone.iter().chain(came).map(|(uid, _)| *uid).collect();
        let paths: HashSet<&TreePath> = gone.iter().chain(came).map(|(_, path)| &**path).collect();
        let came_at: HashSet<&TreePath> = came.iter().map(|(_, path)| &**path).collect();
        let gone_at: HashSet<&TreePath> = gone.iter().map(|(_, path)| &**path).collect();
        let layer = &self.layers[at];
        let (name, writable) = (layer.name.clone(), layer.writable);
        let disk = |path: &TreePath| self.layers[at].place.disk(path);
        let entries: Vec<Entry> = came
            .iter()
            .zip(kinds)
            .map(|((uid, path), kind)| Entry {
                kind: *kind,
                uid: *uid,
                home: name.clone(),
                layer: name.clone(),
                disk: disk(path),
                overrides: vec![],
                moved: None,
            })
            .collect();
        // What stood on the stack as it was, above this layer, stands on another now.
        self.beneath.lock().unwrap_or_else(std::sync::PoisonError::into_inner).retain(|above, _| *above <= at);
        // With every layer shown, what is resolved is the whole stack: let go of here, so that the stack is changed
        // where it is and not copied to be.
        self.resolved = Shared::default();
        {
            let full = Shared::make_mut(&mut self.full);
            let listed = Shared::make_mut(&mut self.entries);
            let winning = Shared::make_mut(&mut self.winning);
            let placed = Shared::make_mut(&mut self.placed);
            let hashes = &mut Shared::make_mut(&mut self.hashes)[at];
            for (uid, path) in gone {
                placed.remove(uid);
                full.remove(path);
                listed.remove(path);
                winning.remove(&(at, path.clone()));
                hashes.remove(path);
            }
            for (((uid, path), kind), entry) in came.iter().zip(kinds).zip(entries) {
                placed.insert(*uid, path.clone());
                full.insert(path.clone(), Node::created(*kind, *uid, at, path));
                listed.insert(path.clone(), entry);
                winning.insert((at, path.clone()), path.clone());
            }
        }
        self.resolved = self.full.clone();
        if writable {
            for ((_, path), kind) in came.iter().zip(kinds) {
                if let Some(hash) = self.content_hash(at, path, *kind) {
                    Shared::make_mut(&mut self.hashes)[at].insert(path.clone(), hash);
                }
            }
        }
        // What is served differs of these, and of a node an entry came or went in.
        refreshed.gone.extend(gone.iter().map(|(_, path)| path).filter(|path| !spelled_among(&came_at, path)).cloned());
        refreshed.touched.extend(came.iter().map(|(_, path)| path.clone()));
        let around: Vec<Arc<TreePath>> = paths
            .iter()
            .filter(|path| spelled_among(&gone_at, path) != spelled_among(&came_at, path))
            .filter_map(|path| asset_of(path).parent().filter(|parent| !parent.is_root()).map(TreePath::to_owned))
            .map(|parent| bundle_named(&parent))
            .filter(|parent| self.full.contains_key(parent))
            .collect();
        refreshed.touched.extend(around);
        // The assets come and gone: each known by its uid from now on, or no more. Where nothing names their uids
        // and nothing refers to one that goes, the other assets' links stand, and so does what the layers need of
        // each other but for these; else all are linked, and counted, again.
        let asset =
            |(uid, path): &(Uid, Arc<TreePath>)| path.as_str().ends_with(BUNDLE_SUFFIX).then(|| (*uid, asset_of(path)));
        let went: Vec<(Uid, Arc<TreePath>)> = gone.iter().filter_map(asset).collect();
        let come: Vec<(Uid, Arc<TreePath>)> = came.iter().filter_map(asset).collect();
        // References are what declarations say: text.
        let others_refer =
            |asset: &TreePath| self.referenced_by.get(asset).is_some_and(|refs| refs.iter().any(|r| r.from != *asset));
        let whole = uids.iter().any(|uid| self.named.contains_key(uid))
            || self.layers.iter().any(|l| uids.iter().any(|uid| l.changeset.ops.contains_key(uid)))
            || come.iter().any(|(_, asset)| self.dangling.iter().any(|r| r.target.as_deref() == Some(&**asset)))
            || went.iter().any(|(_, asset)| others_refer(asset));
        {
            let by_uid = Shared::make_mut(&mut self.by_uid);
            for (uid, asset) in &went {
                if by_uid.get(uid) == Some(asset) {
                    by_uid.remove(uid);
                }
            }
            by_uid.extend(come.iter().cloned());
        }
        let left: Vec<Arc<TreePath>> =
            gone.iter().map(|(_, path)| path).filter(|path| !came_at.contains(&***path)).cloned().collect();
        self.assemble_assets(&left);
        let left: Vec<Arc<TreePath>> =
            went.iter().map(|(_, asset)| asset.clone()).filter(|a| !self.assets.contains_key(a)).collect();
        if !whole {
            for asset in &left {
                let was = named_uids(before_references.get(asset).into_iter().flatten());
                self.recount(at, &was, &[]);
            }
            self.relink(&left, before_references);
        }
        refreshed.assets.extend(left);
        // What the entries that are there hold - those come, and those that stay - assembled and linked, as where
        // contents alone change.
        self.refresh_contents(regions, before_references, refreshed);
        if whole {
            self.link();
            self.dependencies = self.dependencies();
        }
    }

    /// Count what a layer needs of the others for an asset of its own whose references named `was` and name `now`:
    /// each uid another layer makes is one reference to that layer, more or less.
    fn recount(&mut self, layer: usize, was: &[Uid], now: &[Uid]) {
        let creator = |uid: &Uid| self.layers.iter().position(|l| l.changeset.entries.contains_key(uid));
        let less: Vec<usize> = was.iter().filter_map(creator).filter(|other| *other != layer).collect();
        let more: Vec<usize> = now.iter().filter_map(creator).filter(|other| *other != layer).collect();
        let needs = &mut self.dependencies[layer];
        for other in more {
            needs.entry(other).or_insert((0, 0)).1 += 1;
        }
        for other in less {
            if let Some(count) = needs.get_mut(&other) {
                count.1 = count.1.saturating_sub(1);
                if *count == (0, 0) {
                    needs.remove(&other);
                }
            }
        }
    }

    /// A refresh where only contents changed, each its entry's winning one: the entries stand; the changed assets
    /// are assembled and linked again, the hashes of their writable layers kept, the dependencies counted again only
    /// where their references changed.
    fn refresh_contents(
        &mut self,
        regions: &[(usize, Arc<TreePath>)],
        before: &HashMap<Arc<TreePath>, Vec<Reference>>,
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
                        Shared::make_mut(&mut self.hashes)[*i].insert(path.clone(), hash);
                    }
                }
            }
            let entries = self.entries_of(*i);
            for path in scan.bundles.keys() {
                if let Some(resolved) = self.winning.get(&(entries, path.clone())) {
                    paths.push(resolved.clone());
                }
            }
            // What is served of these may differ: the nodes, and the plain files, by where the stack resolves them.
            let plain = scan.files.keys().filter_map(|path| self.winning.get(&(entries, path.clone())));
            refreshed.touched.extend(plain.cloned());
        }
        refreshed.touched.extend(paths.iter().cloned());
        self.assemble_assets(&paths);
        let assets: Vec<Arc<TreePath>> = paths.iter().map(|path| asset_of(path)).collect();
        let referring = |r: &Reference| (r.slot.clone(), r.uid.clone(), r.target.clone(), r.output.clone());
        let references_changed = assets.iter().any(|a| {
            let now: Vec<_> = self.references_from(a).iter().map(referring).collect();
            before.get(a).is_none_or(|b| b.iter().map(referring).collect::<Vec<_>>() != now)
        });
        self.relink(&assets, before);
        if references_changed {
            // Each asset's own layer needs what its references name now, where it needed what they named: counted
            // for those, where every asset's layer can be told; else for all, as the stack applied does.
            let layers: Option<Vec<usize>> =
                paths.iter().map(|path| self.resolved.get(path).map(|n| n.content.0)).collect();
            let plain = |layer: &usize| self.entries_of(*layer) == *layer && self.filled_by(*layer).is_none();
            match layers.filter(|layers| layers.iter().all(plain)) {
                Some(layers) => {
                    for (asset, layer) in assets.iter().zip(layers) {
                        let was = named_uids(before.get(asset).into_iter().flatten());
                        let now = named_uids(&self.references_from(asset));
                        self.recount(layer, &was, &now);
                    }
                }
                None => self.dependencies = self.dependencies(),
            }
        }
        refreshed.assets.extend(assets);
    }

    /// Stat the files; take the records `known` still answers for, read the rest on every core. Returns those read.
    fn load_records(
        &mut self,
        files: &[HostPath],
        mut known: impl FnMut(&HostPath, u64, Stamp) -> Option<Record>,
    ) -> Vec<HostPath> {
        let mut pending = Vec::new();
        for (file, (size, modified)) in files.iter().zip(stat_all(&self.io, files)) {
            let Some(size) = size else { continue };
            if let Some(record) = known(file, size, modified) {
                Shared::make_mut(&mut self.records).insert(file.clone(), Arc::new(record));
            } else {
                let declaration = file.file_name().is_some_and(|n| n == NODE_FILE || n == SETTINGS_FILE)
                    && file.parent().is_some_and(|d| d.to_string_lossy().ends_with(BUNDLE_SUFFIX));
                pending.push(Pending { path: file.clone(), size, modified, declaration });
            }
        }
        let mut read = Vec::new();
        for (file, record) in pending.iter().zip(read_all(&self.io, &pending)) {
            if let Some(record) = record {
                Shared::make_mut(&mut self.records).insert(file.path.clone(), Arc::new(record));
                read.push(file.path.clone());
            }
        }
        read
    }

    /// Stat the files and read again those whose stamp changed or that were reported; returns those read, and those
    /// gone since they were scanned.
    fn reload(&mut self, files: &[HostPath], reported: &HashSet<&HostPath>) -> (Vec<HostPath>, Vec<HostPath>) {
        let (mut pending, mut vanished) = (Vec::new(), Vec::new());
        for (file, (size, modified)) in files.iter().zip(stat_all(&self.io, files)) {
            let Some(size) = size else {
                Shared::make_mut(&mut self.records).remove(file);
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
        for (file, record) in pending.iter().zip(read_all(&self.io, &pending)) {
            if let Some(record) = record {
                Shared::make_mut(&mut self.records).insert(file.path.clone(), Arc::new(record));
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
    fn bundle_files(&self, layer: usize, path: &TreePath) -> Option<NodeFiles> {
        let own = self.layers[layer].scan.bundles.get(path)?;
        let mut files: NodeFiles = own.iter().map(|(n, f)| (n.clone(), f.clone(), layer)).collect();
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
    fn content_hash(&self, layer: usize, path: &TreePath, kind: Kind) -> Option<String> {
        let scan = &self.layers[layer].scan;
        match kind {
            Kind::File => scan.files.get(path).and_then(|f| self.records.get(f)).map(|r| format!("sha1:{}", r.sha1)),
            Kind::Asset => {
                let files = self.bundle_files(layer, path)?;
                let mut listing = String::new();
                for (name, f, _) in &files {
                    let sha1 = self.records.get(f).map_or_default(|r| r.sha1.to_string());
                    listing.push_str(name.as_str());
                    listing.push(' ');
                    listing.push_str(&sha1);
                    listing.push('\n');
                }
                Some(format!("sha1:{}", sha1_hex(listing.as_bytes())))
            }
            Kind::Folder => None,
        }
    }

    /// A resolved entry's files: as its overrides make them, else its content's - a plain file's own by an empty name.
    fn node_files(&self, node: &Node) -> NodeFiles {
        match (&node.files, node.kind) {
            (Some(files), _) => files.as_ref().clone(),
            (None, Kind::Asset) => self.bundle_files(node.content.0, &node.content.1).unwrap_or_default(),
            (None, Kind::File) => {
                let file = self.layers[node.content.0].scan.files.get(&node.content.1);
                file.map_or_else(Vec::new, |f| vec![(RelPath::root().to_owned(), f.clone(), node.content.0)])
            }
            (None, Kind::Folder) => vec![],
        }
    }

    /// A file's SHA-1: as recorded, or, for one merged in the cache, the name of its folder.
    fn sha1_of(&self, path: &HostPath) -> Option<pe2_ext::Sha1> {
        if let Some(record) = self.records.get(path) {
            return Some(record.sha1);
        }
        // A folder named by no hash holds no merged file.
        let folder = path.parent().filter(|f| f.parent().is_some_and(|merged| merged == self.merged))?;
        pe2_ext::Sha1::from_hex(folder.file_name()?.to_str()?).ok()
    }

    /// The hash of a resolved entry's content: a plain file's own, a bundle's over its files' names and hashes.
    fn node_hash(&self, node: &Node) -> Option<String> {
        match node.kind {
            Kind::Asset => {
                let mut listing = String::new();
                for (name, f, _) in &self.node_files(node) {
                    let sha1 = self.sha1_of(f).map_or_default(|sha1| sha1.to_string());
                    listing.push_str(name.as_str());
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
    fn override_with(
        &self,
        node: &mut Node,
        layer: usize,
        at: &TreePath,
        changed: &BTreeMap<Arc<RelPath>, Base>,
    ) -> Option<bool> {
        let below = self.node_files(node);
        let own: NodeFiles = match node.kind {
            Kind::Asset => self.bundle_files(layer, at).unwrap_or_default(),
            Kind::File => {
                self.layers[layer].scan.files.get(at).map(|f| vec![(RelPath::root().to_owned(), f.clone(), layer)])?
            }
            Kind::Folder => return None,
        };
        let mut files: BTreeMap<Arc<RelPath>, (HostPath, usize)> =
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
        at: &TreePath,
        changed: &BTreeMap<Arc<RelPath>, Base>,
    ) -> Option<bool> {
        let held = self.layers[layer].scan.kind(at);
        if held.is_some_and(|k| k != node.kind) {
            return None;
        }
        let stale = self.override_with(node, layer, at, changed)?;
        if held.is_some() {
            node.content = (layer, at.to_owned());
        }
        node.overrides.push(layer);
        Some(stale)
    }

    /// `mine`, made of the version `was` (kept in the store), merged over `now`: where the result is - `mine` itself
    /// where it is unchanged, else in the cache - and its conflicts; none where `was` is not kept, or a file cannot be
    /// read. A merge is remembered by its inputs, so the stack is applied again without merging again.
    fn merged(
        &self,
        name: &str,
        was: &pe2_ext::Sha1,
        now: &HostPath,
        mine: &HostPath,
    ) -> Option<(HostPath, Vec<crate::merge::Conflict>)> {
        let (now_sha1, mine_sha1) = (self.sha1_of(now)?, self.sha1_of(mine)?);
        let key = sha1_hex(format!("{name}\n{was}\n{now_sha1}\n{mine_sha1}").as_bytes());
        let memo = self.merged.join(&Name::constant("memo")).join(&Name::new(&key).ok()?);
        let at = |sha1: &pe2_ext::Sha1| {
            if *sha1 == mine_sha1 {
                mine.clone()
            } else {
                match (Name::new(&sha1.to_string()), RelPath::new(name)) {
                    (Ok(sha1), Ok(name)) => self.merged.join(&sha1).join(&name),
                    _ => self.merged.clone(),
                }
            }
        };
        if let Ok(text) = self.io.filesystem().read_to_string(&memo)
            && let Ok(remembered) = serde_json::from_str::<Memo>(&text)
            && (remembered.sha1 == mine_sha1 || self.io.filesystem().is_file(&at(&remembered.sha1)))
        {
            return Some((at(&remembered.sha1), remembered.conflicts));
        }
        let base = self.objects.get_file(was).ok()?;
        let theirs = crate::content::read(&self.io, now).ok()?;
        let ours = crate::content::read(&self.io, mine).ok()?;
        let merged = crate::merge::merge(name, &base, &theirs, &ours);
        let sha1 = crate::extract::sha1_of(&merged.bytes);
        let path = if sha1 == mine_sha1 {
            mine.clone()
        } else {
            merged_file(self.io.filesystem(), &self.merged, name, &merged.bytes).ok()?
        };
        let remembered = Memo { sha1, conflicts: merged.conflicts };
        if let Ok(text) = serde_json::to_string(&remembered) {
            let _ = self.io.filesystem().create_folder_all(&memo.parent().expect("in a folder"));
            let _ = crate::cache::write_atomic(self.io.filesystem(), &memo, text.as_bytes());
        }
        Some((path, remembered.conflicts))
    }

    /// The hash an override of an entry (by its real path) records as its base: the content it is made from, the
    /// version that wins directly below the layer making it.
    #[must_use]
    pub fn base_hash(&self, path: &TreePath) -> Option<String> {
        self.node_hash(self.resolved.get(path)?)
    }

    /// Apply the stack: each layer's creates, then its operations, bottom to top. What a writable layer holds unlisted
    /// is listed in its changeset first (written), so applying never meets it again. Problems, statuses and
    /// dependencies are the whole stack's; the view leaves out the hidden layers.
    fn apply_stack(&mut self) -> Result<()> {
        self.beneath = Arc::default();
        tracing::info_span!("stack: unlisted").in_scope(|| self.list_unlisted())?;
        let full = tracing::info_span!("stack: resolved").in_scope(|| self.resolve_stack(&HashSet::new()));
        self.stack_problems.clone_from(&full.problems);
        self.problems = full.problems;
        self.problems.extend(self.properties.iter().cloned());
        self.ops = full.ops;
        self.full = Shared::new(full.resolved);
        self.resolved = if self.hidden.is_empty() {
            self.full.clone()
        } else {
            Shared::new(self.resolve_stack(&self.hidden).resolved)
        };
        self.dependencies = tracing::info_span!("stack: dependencies").in_scope(|| self.dependencies());
        tracing::info_span!("stack: hashes").in_scope(|| self.remember_hashes());
        let _span = tracing::info_span!("stack: winning").entered();
        self.winning =
            Shared::new(self.resolved.iter().map(|(path, node)| (node.content.clone(), path.clone())).collect());
        self.merge_inputs = Shared::new(self.resolved.values().flat_map(|node| node.inputs.iter().cloned()).collect());
        Ok(())
    }

    /// Whether what changed in these regions is contents alone: every bundle and plain file of them is its entry's
    /// winning content, and no layer is hidden.
    fn contents_only(&self, regions: &[(usize, Arc<TreePath>)]) -> bool {
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
        let mut resolved: Resolved = BTreeMap::new();
        let mut where_is: HashMap<Uid, Arc<TreePath>> = HashMap::new();
        let mut problems = Vec::new();
        let mut statuses = vec![BTreeMap::new(); self.layers.len()];
        for (i, layer) in self.layers.iter().enumerate() {
            if skip.contains(&i) {
                continue;
            }
            // What it holds by a name no path takes, whatever it is: none of the project's until it is renamed.
            problems.extend(layer.scan.refused_problems(&layer.name));
            // A generated layer makes no entries: what it holds is the files of another's.
            if layer.kind == LayerKind::Generated {
                continue;
            }
            let status = &mut statuses[i];
            let mut accounted: HashSet<Arc<TreePath>> = HashSet::new();
            for (uid, path) in &layer.changeset.entries {
                let Some(kind) = layer.scan.kind(path) else {
                    let left_out = layer.place.ignores(path);
                    problems.push(Problem::Missing { layer: layer.name.clone(), path: path.to_string(), left_out });
                    continue;
                };
                accounted.insert(path.clone());
                if let Some(other) = resolved.get(path) {
                    // A folder has no content: two layers making one are adding to the same place, its first uid and its
                    // second naming it alike.
                    if kind == Kind::Folder && other.kind == Kind::Folder {
                        where_is.insert(*uid, path.clone());
                        continue;
                    }
                    let layers = vec![self.layers[other.home].name.clone(), layer.name.clone()];
                    problems.push(Problem::Conflict { path: path.to_string(), layers });
                    continue;
                }
                if let Some(other) = where_is.get(uid) {
                    let paths = vec![other.to_string(), path.to_string()];
                    problems.push(Problem::DuplicateUid { uid: uid.to_string(), paths });
                    continue;
                }
                resolved.insert(path.clone(), Node::created(kind, *uid, i, path));
                where_is.insert(*uid, path.clone());
            }
            for (uid, op) in &layer.changeset.ops {
                let Some(path) = where_is.get(uid).cloned() else {
                    problems.push(Problem::Unbound { layer: layer.name.clone(), uid: uid.to_string() });
                    status.insert(*uid, (OpStatus::Unbound, None));
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
                        status.insert(*uid, (outcome, Some(path)));
                        continue;
                    }
                    Op::Move { to } | Op::Override { to: Some(to), .. } => Some(to),
                    Op::Override { to: None, .. } => None,
                };
                let at = match to {
                    Some(to) => {
                        let conflicts = move_subtree((&mut resolved, &mut where_is), &path, to, i);
                        if !conflicts.is_empty() {
                            outcome = OpStatus::Conflict;
                        }
                        problems.extend(conflicts.into_iter().map(|target| Problem::Conflict {
                            path: target.to_string(),
                            layers: vec![layer.name.clone()],
                        }));
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
                            path: at.to_string(),
                            left_out: false,
                        });
                        status.insert(*uid, (OpStatus::Missing, Some(path)));
                        continue;
                    };
                    accounted.insert(at.clone());
                    if stale {
                        problems.push(Problem::Stale { layer: layer.name.clone(), path: at.to_string() });
                        outcome = OpStatus::Stale;
                    }
                }
                status.insert(*uid, (outcome, Some(path)));
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
        accounted: &HashSet<Arc<TreePath>>,
        resolved: &Resolved,
        problems: &mut Vec<Problem>,
    ) {
        let layer = &self.layers[i];
        for path in layer.scan.bundles.keys().chain(layer.scan.files.keys()) {
            if !accounted.contains(path)
                && let Some(node) = resolved.get(path)
                && node.home != i
            {
                let layers = vec![self.layers[node.home].name.clone(), layer.name.clone()];
                problems.push(Problem::Conflict { path: path.to_string(), layers });
            }
        }
        // What it holds unlisted, by the topmost path of each tree of it: a folder of a hundred files is one problem.
        let unlisted: BTreeSet<&TreePath> =
            layer.scan.paths().filter(|p| !accounted.contains(*p) && !resolved.contains_key(*p)).collect();
        for path in &unlisted {
            let parent = path.parent().filter(|parent| !parent.is_root());
            if !parent.is_some_and(|parent| unlisted.contains(&parent)) {
                problems.push(Problem::Unlisted { layer: layer.name.clone(), path: path.to_string() });
            }
        }
    }

    /// What each layer needs of the layers it is not: operations on the entries they create, references to their
    /// assets - by the creating layer, with how many of each.
    fn dependencies(&self) -> Vec<BTreeMap<usize, (usize, usize)>> {
        let mut creator: HashMap<Uid, usize> = HashMap::new();
        for (i, layer) in self.layers.iter().enumerate() {
            for uid in layer.changeset.entries.keys() {
                creator.entry(*uid).or_insert(i);
            }
        }
        let mut out = vec![BTreeMap::new(); self.layers.len()];
        for (i, layer) in self.layers.iter().enumerate() {
            for uid in layer.changeset.ops.keys() {
                if let Some(&j) = creator.get(uid).filter(|&&j| j != i) {
                    out[i].entry(j).or_insert((0, 0)).0 += 1;
                }
            }
            for files in layer.scan.bundles.values() {
                let declaration = files
                    .get(NODE_FILE.as_rel())
                    .and_then(|f| self.records.get(f))
                    .and_then(|r| r.declaration.as_ref());
                // What a declaration names as it writes it: a text that is no uid names no layer's entry.
                for declared in declaration.map_or(&[][..], |d| &d.references) {
                    let creating = declared.uid.as_ref().ok().and_then(|uid| creator.get(uid));
                    if let Some(&j) = creating.filter(|&&j| j != i) {
                        out[i].entry(j).or_insert((0, 0)).1 += 1;
                    }
                }
            }
        }
        out
    }

    /// All that the index derives, as text: what a test holds an index changed in place to, against one opened
    /// afresh.
    #[cfg(test)]
    pub(crate) fn shown_whole(&self) -> String {
        use std::fmt::Write;
        let index = self;
        let mut out = String::new();
        for (path, a) in index.assets.iter() {
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
        let placed: std::collections::BTreeMap<_, _> = index.placed.iter().collect();
        let named: std::collections::BTreeMap<_, _> = index.named.iter().collect();
        let _ = writeln!(out, "placed {placed:?}\nnamed {named:?}");
        out
    }

    /// An asset by its path as text - what an extension, a request or another program names it by: checked here; text
    /// that is no path names no asset.
    #[must_use]
    pub fn asset_at(&self, path: &str) -> Option<&Arc<Asset>> {
        self.assets.get(TreePath::new(path).ok()?)
    }

    /// An entry by its path as text, as `asset_at` takes an asset's.
    #[must_use]
    pub fn entry_at(&self, path: &str) -> Option<&Entry> {
        self.entries.get(TreePath::new(path).ok()?)
    }

    /// The real path an entry resolves at, by its uid.
    #[must_use]
    pub fn real_path(&self, uid: Uid) -> Option<&TreePath> {
        self.placed.get(&uid).map(|path| &**path)
    }

    /// A layer's changeset as the index read it.
    #[must_use]
    pub fn changeset(&self, layer: &Name) -> Option<&Changeset> {
        self.layers.iter().find(|l| l.name == layer).map(|l| &l.changeset)
    }

    /// The stack resolved without a layer and those above it: what that layer stands on.
    fn below_layer(&self, layer: &Name) -> Option<Arc<Beneath>> {
        let at = self.layers.iter().position(|l| l.name == layer)?;
        let mut beneath = self.beneath.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(found) = beneath.get(&at) {
            return Some(found.clone());
        }
        let resolution = self.resolve_stack(&(at..self.layers.len()).collect());
        let by_uid = resolution.resolved.iter().map(|(path, node)| (node.uid, path.clone())).collect();
        let found = Arc::new(Beneath { resolution, by_uid });
        beneath.insert(at, found.clone());
        Some(found)
    }

    /// What the layers below `layer` resolve, to be asked by path and by uid: nothing is copied of it.
    #[must_use]
    pub fn under(&self, layer: &Name) -> Under {
        Under(self.below_layer(layer))
    }

    /// What the layers below `layer` resolve, by path: each entry's uid and kind.
    #[must_use]
    pub fn entries_below(&self, layer: &Name) -> HashMap<Arc<TreePath>, (Uid, Kind)> {
        let Some(beneath) = self.below_layer(layer) else { return HashMap::new() };
        beneath.resolution.resolved.iter().map(|(path, node)| (path.clone(), (node.uid, node.kind))).collect()
    }

    /// An entry as the layers below `layer` resolve it, by its uid.
    #[must_use]
    pub fn below(&self, layer: &Name, uid: Uid) -> Option<Below> {
        let beneath = self.below_layer(layer)?;
        let (path, node) = beneath.resolution.resolved.get_key_value(beneath.by_uid.get(&uid)?)?;
        let source = &self.layers[node.content.0];
        let files: Vec<(Arc<RelPath>, HostPath)> =
            self.node_files(node).into_iter().map(|(name, file, _)| (name, file)).collect();
        Some(Below {
            kind: node.kind,
            uid,
            path: path.clone(),
            hash: self.node_hash(node),
            sha1s: files.iter().filter_map(|(name, f)| Some((name.clone(), self.sha1_of(f)?))).collect(),
            files,
            layer: source.name.clone(),
        })
    }

    /// The entries the layers below `layer` resolve inside a folder (its path), by uid.
    #[must_use]
    pub fn below_inside(&self, layer: &Name, folder: &TreePath) -> Vec<(Uid, Arc<TreePath>)> {
        let Some(beneath) = self.below_layer(layer) else { return vec![] };
        let resolved = &beneath.resolution.resolved;
        let inside = subtree(resolved, folder).into_iter().filter(|path| path != folder);
        inside.filter_map(|path| Some((resolved.get(&path)?.uid, path))).collect()
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
                    problems: self.problems.iter().filter(|p| p.layers().contains(&&*l.name)).count(),
                    depends_on: depends_on.collect(),
                }
            })
            .collect()
    }

    /// A layer's creates, in path order.
    #[must_use]
    pub fn creates(&self, layer: &Name) -> Vec<CreateView> {
        let Some(l) = self.layers.iter().find(|l| l.name == layer) else { return vec![] };
        let mut out: Vec<CreateView> = l
            .changeset
            .entries
            .iter()
            .map(|(uid, path)| CreateView { uid: *uid, path: path.clone(), kind: l.scan.kind(path) })
            .collect();
        out.sort_by(|a, b| a.path.cmp(&b.path));
        out
    }

    /// What makes a layer's override of an entry (by uid) stale: what each file it changes conflicts with below - a
    /// field, lines, a whole file - and each it removes, or holds, that changed below since otherwise.
    #[must_use]
    pub fn stale_parts(&self, layer: &Name, uid: Uid) -> Vec<StalePart> {
        let Some(i) = self.layers.iter().position(|l| l.name == layer) else { return vec![] };
        let Some(Op::Override { files, to }) = self.layers[i].changeset.ops.get(&uid) else { return vec![] };
        let Some(below) = self.below(layer, uid) else { return vec![] };
        let at = to.clone().unwrap_or_else(|| below.path.clone());
        let mine_of = |name: &RelPath| match below.kind {
            Kind::File => self.layers[i].scan.files.get(&at).cloned(),
            _ => self.layers[i].scan.bundles.get(&at).and_then(|b| b.get(name)).cloned(),
        };
        let whole = |file: &RelPath, mine: &str, was: &Option<pe2_ext::Sha1>, now: Option<&pe2_ext::Sha1>| StalePart {
            file: file.to_owned(),
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
                parts.push(whole(name, "removed", &base.was, now_sha1));
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
                    file: name.clone(),
                    field: c.at,
                    mine: c.mine,
                    was: c.base,
                    now: c.theirs,
                })),
                None => parts.push(whole(name, "its own", &base.was, now_sha1)),
            }
        }
        parts
    }

    /// A layer's operations with their status, in the order of the paths they act on.
    #[must_use]
    pub fn operations(&self, layer: &Name) -> Vec<OpView> {
        let Some(i) = self.layers.iter().position(|l| l.name == layer) else { return vec![] };
        let statuses = self.ops.get(i);
        let mut out: Vec<OpView> = self.layers[i]
            .changeset
            .ops
            .iter()
            .map(|(uid, op)| {
                let (status, path) = statuses.and_then(|s| s.get(uid)).cloned().unwrap_or((OpStatus::Unbound, None));
                OpView { uid: *uid, op: op.clone(), path, status }
            })
            .collect();
        out.sort_by(|a, b| a.path.cmp(&b.path));
        out
    }

    /// Leave layers out of the view (the editor's eyes), or bring them back: the resolved entries and assets follow;
    /// problems and statuses stay the whole stack's. Returns whether the view changed.
    pub fn set_hidden(&mut self, names: &BTreeSet<Arc<Name>>) -> bool {
        let hidden: HashSet<usize> =
            self.layers.iter().enumerate().filter(|(_, l)| names.contains(&l.name)).map(|(i, _)| i).collect();
        if hidden == self.hidden {
            return false;
        }
        self.hidden = hidden;
        self.resolved = Shared::new(self.resolve_stack(&self.hidden).resolved);
        Shared::make_mut(&mut self.assets).clear();
        let paths: Vec<Arc<TreePath>> = self.resolved.keys().cloned().collect();
        self.assemble(&paths);
        self.link();
        true
    }

    /// Show assets with their working copies' files in place of those on disk - these, by uid; the others as on
    /// disk. The assets whose working copy changed are assembled and linked again; returns their paths.
    pub fn set_working(&mut self, working: HashMap<Uid, Working>) -> BTreeSet<Arc<TreePath>> {
        let mut uids: BTreeSet<&Uid> = working.keys().collect();
        uids.extend(self.working.keys());
        let changed: Vec<Arc<TreePath>> = uids
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
                let (size, modified) = crate::content::stat(&self.io, &file.path).ok()?;
                let declaration = name == NODE_FILE || name == SETTINGS_FILE;
                Some(Pending { path: file.path.clone(), size, modified: stamp(modified), declaration })
            })
            .collect();
        for (file, record) in unread.iter().zip(read_all(&self.io, &unread)) {
            if let Some(record) = record {
                Shared::make_mut(&mut self.records).insert(file.path.clone(), Arc::new(record));
            }
        }
        self.working = Shared::new(working);
        if changed.is_empty() {
            return BTreeSet::new();
        }
        let before: HashMap<Arc<TreePath>, Vec<Reference>> =
            changed.iter().map(|a| (a.clone(), self.references_from(a))).collect();
        let paths: Vec<Arc<TreePath>> = changed.iter().map(|asset| bundle_named(asset)).collect();
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
        // What was read of its files is read again of the working copy's.
        shown.fields = OnceLock::new();
        shown.settings_text = OnceLock::new();
        shown.fields_toml = OnceLock::new();
        shown.settings_toml = OnceLock::new();
        for (name, file) in &working.files {
            let at = shown.files.iter().position(|f| f.name == *name);
            let layer = at.map(|i| shown.files[i].layer.clone()).or_else(|| shown.layers.last().cloned());
            if let Some(i) = at {
                shown.files.remove(i);
            }
            let Some(file) = file else { continue };
            let Some(record) = self.records.get(&file.path) else { continue };
            // An asset is a layer's, the one creating it at least: none is a layer's file.
            let Some(layer) = layer.clone() else { continue };
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
                layer,
                sha1: record.sha1,
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
        self.hashes = Shared::new(
            (0..self.layers.len())
                .map(|i| {
                    let layer = &self.layers[i];
                    if !layer.writable {
                        return HashMap::new();
                    }
                    let held = layer.scan.bundles.keys().map(|p| (p, Kind::Asset));
                    let held = held.chain(layer.scan.files.keys().map(|p| (p, Kind::File)));
                    let mut hashes: HashMap<Arc<TreePath>, String> =
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
    pub fn listed(&self, layer: &Name, path: &TreePath) -> Option<Uid> {
        let layer = self.layers.iter().find(|known| known.name == layer)?;
        layer.changeset.entries.iter().find(|(_, listed)| *listed == path).map(|(uid, _)| *uid)
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
            let listing = self.listing(i, None);
            let layer = &mut self.layers[i];
            if layer.listing == Listing::Once {
                layer.listing = Listing::Adopted;
            }
            if listing.is_empty() {
                continue;
            }
            layer.changeset.entries.extend(listing);
            layer.changeset.save(self.io.filesystem(), &layer.changeset_path)?;
        }
        Ok(())
    }

    /// How a layer would list what it holds unlisted - all of it, or what is at and under the paths given - each by its
    /// uid and path. A listed entry gone from its path while an unlisted one holds the same content (by the hashes of the
    /// last pass) moved there, and keeps its uid; a folder follows when what it held moved under a new name. What is
    /// left gets a new entry, under the uid its `node.toml` names where it names one not taken, a new one otherwise. What
    /// a layer holds at a path a lower layer creates is no entry of its own (an override, or a conflict).
    fn listing(&self, i: usize, only: Option<&[Arc<TreePath>]>) -> Vec<(Uid, Arc<TreePath>)> {
        let mut taken: HashSet<Uid> = HashSet::new();
        for layer in &self.layers {
            taken.extend(layer.changeset.entries.keys().copied());
        }
        let below: HashSet<&TreePath> =
            self.layers[..i].iter().flat_map(|l| l.changeset.entries.values()).map(|path| &**path).collect();
        let layer = &self.layers[i];
        let listed: HashSet<&TreePath> = layer.changeset.entries.values().map(|path| &**path).collect();
        let copies = self.override_targets(i);
        let wanted = |p: &TreePath| only.is_none_or(|only| only.iter().any(|o| p.starts_with(o)));
        let mut unlisted: BTreeSet<Arc<TreePath>> = layer
            .scan
            .paths()
            .filter(|p| !listed.contains(p) && !below.contains(p) && !copies.contains(*p) && wanted(p))
            .map(ToOwned::to_owned)
            .collect();
        if unlisted.is_empty() {
            return vec![];
        }
        // Moves: a gone entry's last hash found again at an unlisted path, once.
        let gone: Vec<(Uid, Arc<TreePath>)> = layer
            .changeset
            .entries
            .iter()
            .filter(|(_, p)| layer.scan.kind(p).is_none())
            .map(|(u, p)| (*u, p.clone()))
            .collect();
        let mut by_hash: HashMap<String, Vec<Arc<TreePath>>> = HashMap::new();
        for path in &unlisted {
            if let Some(kind) = layer.scan.kind(path).filter(|k| *k != Kind::Folder)
                && let Some(hash) = self.content_hash(i, path, kind)
            {
                by_hash.entry(hash).or_default().push(path.to_owned());
            }
        }
        let last = self.hashes.get(i).cloned().unwrap_or_default();
        let mut moves: Vec<(Uid, Arc<TreePath>, Arc<TreePath>)> = Vec::new();
        for (id, old) in &gone {
            if let Some(new) = last.get(old).and_then(|h| by_hash.get(h)).filter(|c| c.len() == 1).map(|c| c[0].clone())
                && unlisted.remove(&new)
            {
                moves.push((*id, old.clone(), new));
            }
        }
        // A gone folder whose content moved from under it to under one new name moved there.
        for (id, old) in &gone {
            if layer.scan.kind(old).is_some() || moves.iter().any(|(u, _, _)| u == id) {
                continue;
            }
            let targets: BTreeSet<Arc<TreePath>> = moves
                .iter()
                .filter_map(|(_, from, to)| {
                    let rest = from.strip_prefix(old).filter(|rest| !rest.is_root())?;
                    without_tail(to, rest)
                })
                .collect();
            if targets.len() == 1
                && let Some(new) = targets.into_iter().next()
                && unlisted.remove(&new)
            {
                moves.push((*id, old.clone(), new));
            }
        }
        let mut out: Vec<(Uid, Arc<TreePath>)> = moves.into_iter().map(|(id, _, new)| (id, new)).collect();
        for path in unlisted {
            // The uid its declaration names, where that is one and not taken: what a node written by hand keeps.
            let declared = layer
                .scan
                .bundles
                .get(&path)
                .and_then(|files| files.get(NODE_FILE.as_rel()))
                .and_then(|f| self.records.get(f))
                .and_then(|r| Uid::new(r.declaration.as_ref()?.uid.as_deref()?).ok())
                .filter(|u| !taken.contains(u));
            let id = match declared {
                Some(id) => id,
                None => uid::new(self.io.random(), |uid| taken.contains(&uid)),
            };
            taken.insert(id);
            out.push((id, path));
        }
        out
    }

    /// How a layer would list what it holds unlisted at and under a path, each by its uid and path: its moves (a gone
    /// entry's uid, at its new path) and its new entries (`listing`).
    ///
    /// # Errors
    /// There is no such layer.
    pub fn adoption(&self, layer: &Name, path: &TreePath) -> Result<Vec<(Uid, Arc<TreePath>)>> {
        let i = anyhow::Context::with_context(self.layers.iter().position(|l| l.name == layer), || {
            format!("no layer {layer}")
        })?;
        Ok(self.listing(i, Some(&[path.to_owned()])))
    }

    /// Where a layer's overrides put their copies: each override's move target, else its entry's path below the
    /// layer. Not unlisted content: the changeset accounts for them.
    fn override_targets(&self, layer: usize) -> HashSet<Arc<TreePath>> {
        let ops = &self.layers[layer].changeset.ops;
        if !ops.values().any(|op| matches!(op, Op::Override { .. })) {
            return HashSet::new();
        }
        let below = self.resolve_stack(&(layer..self.layers.len()).collect());
        let path_of: HashMap<Uid, &TreePath> = below.resolved.iter().map(|(p, n)| (n.uid, &**p)).collect();
        ops.iter()
            .filter_map(|(uid, op)| match op {
                Op::Override { to: Some(to), .. } => Some(to.clone()),
                Op::Override { to: None, .. } => path_of.get(uid).map(|p| (*p).to_owned()),
                _ => None,
            })
            .collect()
    }

    /// Build the assets at these real paths from the resolved stack and the records: their uid, the winning layer's
    /// bundle, its `node.toml`. A path that no longer resolves to an asset loses its asset; `entries` follows the
    /// whole stack.
    fn assemble(&mut self, paths: &[Arc<TreePath>]) {
        tracing::info_span!("assemble: entries").in_scope(|| self.assemble_entries());
        self.assemble_assets(paths);
    }

    /// The entries, from the resolved stack.
    fn assemble_entries(&mut self) {
        self.placed = Shared::new(self.resolved.iter().map(|(path, node)| (node.uid, path.clone())).collect());
        self.entries = Shared::new(
            self.resolved
                .iter()
                .map(|(path, node)| {
                    let layer = &self.layers[node.content.0];
                    let entry = Entry {
                        kind: node.kind,
                        uid: node.uid,
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
    /// asset loses its asset. Many at once - a project opened, the stack applied again - are made on every core:
    /// each is made of its own entry and files alone.
    fn assemble_assets(&mut self, paths: &[Arc<TreePath>]) {
        // Enough of them to be worth helpers: what an edit changed is made here.
        const MANY: usize = 64;
        fn each<T: Sync, R: Send>(items: &[T], work: impl Fn(&T) -> R + Sync) -> Vec<R> {
            if items.len() < MANY { items.iter().map(work).collect() } else { crate::parallel(items, work) }
        }
        // What the assets override is made again when next asked for.
        if !paths.is_empty() {
            self.versions = Arc::default();
        }
        // Each asset's files, and those of them nothing was read of yet: what overrides merged, in the cache.
        let listing = tracing::info_span!("assemble: the assets' files").entered();
        let files = each(paths, |path| {
            let node = self.resolved.get(path).filter(|n| n.kind == Kind::Asset)?;
            let files = self.node_files(node);
            let unread: Vec<Pending> = files
                .iter()
                .filter(|(_, file, _)| !self.records.contains_key(file))
                .filter_map(|(name, file, _)| {
                    let (size, modified) = crate::content::stat(&self.io, file).ok()?;
                    let declaration = name == NODE_FILE || name == SETTINGS_FILE;
                    Some(Pending { path: file.clone(), size, modified: stamp(modified), declaration })
                })
                .collect();
            Some((files, unread))
        });
        drop(listing);
        // Recorded as any file is, read once.
        let unread: Vec<&Pending> = files.iter().flatten().flat_map(|(_, unread)| unread).collect();
        if !unread.is_empty() {
            let unread: Vec<Pending> =
                unread.into_iter().map(|file| Pending { path: file.path.clone(), ..*file }).collect();
            for (file, record) in unread.iter().zip(read_all(&self.io, &unread)) {
                if let Some(record) = record {
                    Shared::make_mut(&mut self.records).insert(file.path.clone(), Arc::new(record));
                }
            }
        }
        let with_files: Vec<_> =
            paths.iter().zip(files).map(|(path, files)| (path, files.map(|(files, _)| files))).collect();
        let assets = tracing::info_span!("assemble: the assets made").in_scope(|| {
            each(&with_files, |(path, files)| {
                let (node, files) = (self.resolved.get(*path)?, files.as_ref()?);
                Some(self.with_working(self.asset_made(path, node, files)))
            })
        });
        let _span = tracing::info_span!("assemble: the assets kept").entered();
        for (path, asset) in paths.iter().zip(assets) {
            let Some(asset) = asset else {
                Shared::make_mut(&mut self.assets).remove(&asset_of(path));
                continue;
            };
            Shared::make_mut(&mut self.assets).insert(asset.path.clone(), Arc::new(asset));
        }
    }

    /// The asset an entry of the resolved stack is, of its files as they were read: as it is on disk.
    fn asset_made(&self, path: &TreePath, node: &Node, files: &[(Arc<RelPath>, HostPath, usize)]) -> Asset {
        let layers = std::iter::once(node.home).chain(node.overrides.iter().copied());
        let mut asset = Asset {
            io: self.io.clone(),
            path: asset_of(path),
            uid: Some(node.uid),
            named: Some(node.uid.to_string().into()),
            shape: Arc::from(""),
            variants: vec![],
            files: vec![],
            layers: layers.map(|l| self.layers[l].name.clone()).collect(),
            error: None,
            text: Arc::from(""),
            generated: None,
            fields: OnceLock::new(),
            settings_text: OnceLock::new(),
            fields_toml: OnceLock::new(),
            settings_toml: OnceLock::new(),
            imported: None,
            disk: None,
        };
        for (name, file, held) in files {
            let Some(record) = self.records.get(file) else { continue };
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
                layer: self.layers[*held].name.clone(),
                sha1: record.sha1,
                made_by: None,
            });
        }
        asset
    }

    /// The versions every overridden asset has below it: `below:<uid>`, `below:below:<uid>`, ..., each below the
    /// last, from the layer holding the asset.
    fn versions_made(&self) -> HashMap<String, Arc<Asset>> {
        let mut versions = HashMap::new();
        let overridden =
            self.resolved.iter().filter(|(_, node)| node.kind == Kind::Asset && !node.overrides.is_empty());
        for (path, node) in overridden {
            let (at, uid) = (asset_of(path), node.uid);
            let mut layer = self.layers[node.content.0].name.clone();
            let mut name = format!("{BELOW}{uid}");
            while let Some(below) = self.below(&layer, uid) {
                let mut asset = Asset {
                    io: self.io.clone(),
                    path: at.clone(),
                    uid: None,
                    named: Some(name.as_str().into()),
                    shape: Arc::from(""),
                    variants: vec![],
                    files: vec![],
                    layers: vec![below.layer.clone()],
                    error: None,
                    text: Arc::from(""),
                    generated: None,
                    fields: OnceLock::new(),
                    settings_text: OnceLock::new(),
                    fields_toml: OnceLock::new(),
                    settings_toml: OnceLock::new(),
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
                        sha1: record.sha1,
                        made_by: None,
                    });
                }
                versions.insert(name.clone(), Arc::new(asset));
                name.insert_str(0, BELOW);
                layer = below.layer;
            }
        }
        versions
    }

    /// A version an asset overrides, by its name: `below:<uid>`.
    #[must_use]
    pub fn version(&self, name: &str) -> Option<&Asset> {
        self.versions.get_or_init(|| self.versions_made()).get(name).map(Arc::as_ref)
    }

    /// Resolve the uids and the references, and add what is wrong with the assets' declarations.
    fn link(&mut self) {
        self.by_uid = Shared::new(self.assets.values().filter_map(|a| Some((a.uid?, a.path.clone()))).collect());
        self.problems.retain(|p| !matches!(p, Problem::Unreadable { .. }));
        for asset in self.assets.values() {
            if let Some(error) = &asset.error {
                self.problems.push(Problem::Unreadable { asset: asset.path.to_string(), error: error.clone() });
            }
        }
        let (mut referenced_by, mut dangling) = (HashMap::<Arc<TreePath>, Vec<Reference>>::new(), Vec::new());
        let mut named = HashMap::<Uid, usize>::new();
        for asset in self.assets.values() {
            let declaration = asset
                .files
                .iter()
                .find(|f| f.name == NODE_FILE)
                .and_then(|f| self.records.get(&f.path)?.declaration.as_ref());
            for declared in declaration.map_or(&[][..], |d| &d.references) {
                let reference = Reference::of(&asset.path, declared);
                if let Ok(uid) = reference.uid {
                    *named.entry(uid).or_default() += 1;
                }
                match self.target_of(&reference) {
                    Some(resolved) => referenced_by.entry(resolved).or_default().push(reference),
                    None => dangling.push(reference),
                }
            }
        }
        self.referenced_by = Shared::new(referenced_by);
        self.dangling = dangling;
        self.named = Shared::new(named);
    }

    /// Link again what these assets declare, which declared `before` (by asset): their unreadable declarations, and
    /// their references taken back and made again where a full link puts them (in asset order, then declaration
    /// order). The uids and the assets stand: other assets' references resolve as they did.
    fn relink(&mut self, assets: &[Arc<TreePath>], before: &HashMap<Arc<TreePath>, Vec<Reference>>) {
        let changed: HashSet<&TreePath> = assets.iter().map(|asset| &**asset).collect();
        let shown: HashSet<&str> = assets.iter().map(|asset| asset.as_str()).collect();
        self.problems.retain(|p| !matches!(p, Problem::Unreadable { asset, .. } if shown.contains(asset.as_str())));
        for path in assets {
            if let Some(error) = self.assets.get(path).and_then(|a| a.error.clone()) {
                self.problems.push(Problem::Unreadable { asset: path.to_string(), error });
            }
        }
        for (from, references) in before.iter().filter(|(a, _)| changed.contains(&***a)) {
            let named = Shared::make_mut(&mut self.named);
            for uid in references.iter().filter_map(|r| r.uid.as_ref().ok()) {
                if let Some(count) = named.get_mut(uid) {
                    *count -= 1;
                    if *count == 0 {
                        named.remove(uid);
                    }
                }
            }
            let targets: Vec<Arc<TreePath>> = references.iter().filter_map(|r| self.target_of(r)).collect();
            for target in targets {
                let referenced_by = Shared::make_mut(&mut self.referenced_by);
                if let Some(list) = referenced_by.get_mut(&target) {
                    list.retain(|r| r.from != *from);
                    if list.is_empty() {
                        referenced_by.remove(&target);
                    }
                }
            }
        }
        self.dangling.retain(|r| !changed.contains(&*r.from));
        let mut sorted: Vec<&TreePath> = assets.iter().map(|asset| &**asset).collect();
        sorted.sort();
        for from in sorted {
            let mut placed: HashMap<Arc<TreePath>, usize> = HashMap::new();
            let mut dangling_at = self.dangling.partition_point(|r| *r.from < *from);
            for reference in self.references_from(from) {
                if let Ok(uid) = reference.uid {
                    *Shared::make_mut(&mut self.named).entry(uid).or_default() += 1;
                }
                if let Some(target) = self.target_of(&reference) {
                    let list = Shared::make_mut(&mut self.referenced_by).entry(target.clone()).or_default();
                    // After the references of the assets before it, then each of its own after the last.
                    let at = placed.entry(target).or_insert_with(|| list.partition_point(|r| *r.from < *from));
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
    pub fn references_from(&self, path: &TreePath) -> Vec<Reference> {
        let Some(asset) = self.assets.get(path) else { return vec![] };
        let declaration = asset
            .files
            .iter()
            .find(|f| f.name == NODE_FILE)
            .and_then(|f| self.records.get(&f.path)?.declaration.as_ref());
        declaration.map_or(&[][..], |d| &d.references).iter().map(|declared| Reference::of(path, declared)).collect()
    }

    /// The asset a reference names, as a declaration writes it: by its uid, else by the path beside it; none where
    /// neither names an asset.
    #[must_use]
    pub fn resolve(&self, uid: &WrittenUid, target: Option<&TreePath>) -> Option<&Asset> {
        let by_uid = uid.as_ref().ok().and_then(|uid| self.by_uid.get(uid)).and_then(|p| self.assets.get(p));
        by_uid.or_else(|| self.assets.get(target?)).map(|a| &**a)
    }

    /// The asset path a reference resolves to: the uid decides, the path is the fallback; none where neither names
    /// an asset.
    fn target_of(&self, reference: &Reference) -> Option<Arc<TreePath>> {
        self.resolve(&reference.uid, reference.target.as_deref()).map(|asset| asset.path.clone())
    }
}

#[cfg(test)]
#[allow(
    clippy::disallowed_methods,
    reason = "a test's fixtures: the machine's time, places, files and environment, which nothing tested reads"
)]
mod tests {
    use super::{Index, Problem};
    use crate::Project;
    use crate::cache::Cache;
    use crate::changeset::{Base, Changeset, Op};
    use crate::testing::At as _;
    use crate::testing::{rel, tree, uid};
    use sfs::HostPath;

    /// The top layer's override of an entry, changing these files of it: each made from the version below now, kept.
    fn overriding(index: &Index, uid: &str, files: &[&str]) -> Op {
        let below = index.below(pe2_path::Name::constant("top"), crate::testing::uid(uid)).expect("below top");
        let store = crate::chunks::Store::at(
            &index.merged.parent().and_then(|cache| cache.parent()).expect("a project"),
            crate::testing::io(),
        );
        let files = files.iter().map(|name| {
            let name = rel(name);
            let (_, file) = below.files.iter().find(|(n, _)| *n == name).expect("a file below");
            store.put_file(&crate::content::read(&crate::testing::io(), file).expect("read")).expect("kept");
            let was = below.sha1s.get(&name).copied();
            (name, Base { was, removed: false })
        });
        Op::Override { files: files.collect(), to: None }
    }

    use crate::lock::Lock;

    /// A project of two layers, `base` and `top`, in `.layers/`; `base` holds the bundle `a` (its `node.toml` naming
    /// `uid://a`, as declarations did before changesets) in the folder `g`, and `b` and `f`.
    fn project(name: &str) -> HostPath {
        let root = crate::testing::scratch(&format!("index-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        write(&root, "project.toml", "[[layer]]\nname = \"base\"\n\n[[layer]]\nname = \"top\"\n");
        write(&root, ".layers/base/g/a.node/node.toml", "uid = \"uid://a\"\nstack = [{ op = \"t.image\" }]\n");
        write(&root, ".layers/base/g/a.node/native.texels", "texels");
        write(
            &root,
            ".layers/base/b.node/node.toml",
            "stack = [{ op = \"t.file\" }]\nslot = \"uid://a/palettes/default\"\n",
        );
        write(
            &root,
            ".layers/base/f.node/node.toml",
            "stack = [{ op = \"t.file\" }]\nold = { uid = \"uid://a\", path = \"res://g/a.node\" }\n",
        );
        std::fs::create_dir_all(root.at(".layers/top")).unwrap();
        root
    }

    #[test]
    fn what_a_layer_holds_of_extensions_is_no_asset() {
        let root = project("extensions");
        write(&root, ".layers/top/extensions/my.tool.extension/extension.wasm", "code");
        write(&root, ".layers/top/extensions/old.node/node.toml", "stack = [{ op = \"t.file\" }]\n");
        let (_, index) = open(&root);
        assert!(index.assets.keys().all(|path| !path.starts_with(&tree("extensions"))), "{:?}", index.assets.keys());
        assert!(index.base_hash(&tree("extensions")).is_none(), "not even a folder");
        let _ = std::fs::remove_dir_all(&root);
    }

    fn write(root: &HostPath, path: &str, text: &str) -> HostPath {
        let path = root.at(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        path
    }

    fn open(root: &HostPath) -> (Project, Index) {
        let project = Project::load(root).unwrap();
        // Held while opening; released for the test's next opening.
        let lock = Lock::try_acquire(root, "a test").unwrap();
        let mut index = Index::open(&project, &Cache::open(&project, &lock).unwrap()).unwrap();
        // As whoever holds a project does: what the opening read is said in the project's model, and the space
        // it is in left in order.
        crate::model::took(root, &mut index).unwrap();
        crate::shared::space(root, &pe2_io::SystemFilesystem).unwrap().rest().unwrap();
        (project, index)
    }

    fn changeset(root: &HostPath, layer: &str) -> Changeset {
        Changeset::load(&pe2_io::SystemFilesystem, &root.at(&format!(".layers/{layer}.toml"))).unwrap()
    }

    #[test]
    fn lists_what_a_writable_layer_holds() {
        let root = project("lists");
        let (_, index) = open(&root);
        let base = changeset(&root, "base");
        assert_eq!(base.entries.get(&uid("uid://a")), Some(&tree("g/a.node")));
        assert_eq!(base.entries.len(), 4, "{base:?}");
        assert_eq!(index.assets[&tree("g/a")].uid, Some(uid("uid://a")));
        // By uid alone, and in the older form with a path.
        assert_eq!(index.referenced_by[&tree("g/a")].len(), 2);
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
        let names: Vec<&str> = index.assets[&tree("f")].files.iter().map(|file| file.name.as_str()).collect();
        assert_eq!(names, ["face.ufo/glyphs/A_.glif", "face.ufo/metainfo.plist", "node.toml"], "{names:?}");
        // What a stored-file layer naming the folder reads.
        let node: &dyn crate::graph::exec::Node = &*index.assets[&tree("f")];
        assert_eq!(node.file_names().len(), 3);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn applies_overrides_moves_and_deletes() {
        let root = project("ops");
        let (_, index) = open(&root);
        let base = changeset(&root, "base");
        let uid_of = |path: &str| *base.entries.iter().find(|(_, p)| ***p == *path).unwrap().0;
        let mut top = Changeset::default();
        top.ops.insert(uid("uid://a"), overriding(&index, "uid://a", &["node.toml", "native.texels"]));
        top.ops.insert(uid_of("b.node"), Op::Move { to: tree("c.node") });
        top.ops.insert(uid_of("f.node"), Op::Delete);
        top.save(&pe2_io::SystemFilesystem, &root.at(".layers/top.toml")).unwrap();
        write(&root, ".layers/top/g/a.node/node.toml", "stack = [{ op = \"t.image\" }]\n");
        write(&root, ".layers/top/g/a.node/native.texels", "repainted");
        let (_, index) = open(&root);
        assert_eq!(index.assets.keys().map(|asset| asset.as_str()).collect::<Vec<_>>(), ["c", "g/a"]);
        assert_eq!(
            index.assets[&tree("g/a")].layers.iter().map(|layer| layer.as_str()).collect::<Vec<_>>(),
            ["base", "top"]
        );
        assert!(index.assets[&tree("g/a")].files.iter().all(|f| *f.layer == "top"));
        assert_eq!(index.assets[&tree("c")].uid, Some(uid_of("b.node")));
        assert!(index.problems.is_empty(), "{:?}", index.problems);
        // What it overrides is there, a node of its own.
        let below = index.version("below:uid://a").unwrap();
        assert_eq!(
            (below.named.as_deref(), below.uid, below.layers.as_slice()),
            (Some("below:uid://a"), None, [crate::testing::name("base")].as_slice())
        );
        assert!(below.files.iter().all(|f| *f.layer == "base"));
        assert!(index.version("below:below:uid://a").is_none());
        // The base changes under the override: stale.
        write(&root, ".layers/base/g/a.node/native.texels", "retouched");
        let (_, index) = open(&root);
        assert_eq!(index.problems, [Problem::Stale { layer: crate::testing::name("top"), path: "g/a.node".into() }]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn two_layers_making_one_folder_add_to_it() {
        let root = project("one-folder");
        open(&root);
        let mut top = Changeset::default();
        top.entries.insert(uid("uid://again"), tree("g"));
        top.entries.insert(uid("uid://beside"), tree("g/beside.node"));
        top.save(&pe2_io::SystemFilesystem, &root.at(".layers/top.toml")).unwrap();
        write(&root, ".layers/top/g/beside.node/node.toml", "stack = [{ op = \"t.file\" }]\n");
        let (_, index) = open(&root);
        assert!(!index.problems.iter().any(|p| matches!(p, Problem::Conflict { .. })), "{:?}", index.problems);
        assert!(index.assets.contains_key(&tree("g/a")) && index.assets.contains_key(&tree("g/beside")));
        drop(index);
        crate::cache::remove_all(&root);
    }

    /// What a layer holds by a name no path takes is a problem of its - scanned, opened again from the model, and come
    /// since - and none of the project's: never passed over, and never taken for anything else.
    #[cfg(unix)]
    #[test]
    fn a_name_no_path_takes_is_a_problem_and_nothing_of_the_projects() {
        let root = project("refused");
        let refused = |path: &str| Problem::Refused {
            layer: crate::testing::name("base"),
            path: path.into(),
            why: index_why(path),
        };
        // `:` is a name Windows takes for something else: made here, where the system lets it be.
        std::fs::write(root.at(".layers/base/g").entry(std::ffi::OsStr::new("a:b.txt")), "x").unwrap();
        let (project, index) = open(&root);
        assert!(index.problems.contains(&refused("g/a:b.txt")), "{:?}", index.problems);
        assert!(index.assets.contains_key(&tree("g/a")), "what is beside it is as it was");
        drop(index);
        // Opened again from what the model keeps: still said.
        let (_, index) = open(&root);
        assert!(index.problems.contains(&refused("g/a:b.txt")), "{:?}", index.problems);
        drop(index);
        // Come since, in a bundle: its bundle is the project's as it was, the file none of it.
        let (_, mut index) = open(&root);
        let file = root.at(".layers/base/g/a.node").entry(std::ffi::OsStr::new("x:y.bin"));
        std::fs::write(&file, "y").unwrap();
        index.refresh(&project, &[file]).unwrap();
        assert!(index.problems.contains(&refused("g/a.node/x:y.bin")), "{:?}", index.problems);
        assert!(!index.assets[&tree("g/a")].files.iter().any(|file| file.name.as_str().contains(':')));
        drop(index);
        crate::cache::remove_all(&root);
    }

    /// Two of a folder named alike - one name in both its normal forms, as a system keeping either writes them - are
    /// each a problem of the layer's, and neither is the project's: in a folder as in a bundle, scanned and opened again
    /// from the model. (Not where the system itself takes them as one.)
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn names_alike_beside_each_other_are_each_a_problem() {
        let root = project("alike");
        let (composed, decomposed) = ("caf\u{e9}.txt", "cafe\u{301}.txt");
        for folder in [".layers/base/g", ".layers/base/g/a.node"] {
            for (name, text) in [(composed, "a"), (decomposed, "b")] {
                std::fs::write(root.at(folder).entry(std::ffi::OsStr::new(name)), text).unwrap();
            }
        }
        let paths = [composed, decomposed].map(|name| [format!("g/{name}"), format!("g/a.node/{name}")]);
        for _ in 0..2 {
            let (_, index) = open(&root);
            let said = |path: &str| {
                index.problems.iter().any(|p| {
                    matches!(p, Problem::Refused { path: at, why, .. } if at == path && why.contains("one name with"))
                })
            };
            for path in paths.iter().flatten() {
                assert!(said(path), "{path}: {:?}", index.problems);
            }
            assert!(!index.entries.contains_key(&*tree(&format!("g/{composed}"))), "neither is taken");
            assert!(!index.assets[&tree("g/a")].files.iter().any(|file| file.name.as_str().starts_with("caf")));
        }
        crate::cache::remove_all(&root);
    }

    /// Why a path the tests make is refused, as the index says it.
    #[cfg(unix)]
    fn index_why(path: &str) -> String {
        let name = path.rsplit('/').next().unwrap_or(path);
        pe2_path::Name::new(name).expect_err("a refused name").to_string()
    }

    #[test]
    fn reports_what_does_not_add_up() {
        let root = project("problems");
        open(&root);
        let mut top = Changeset { read_only: true, ..Changeset::default() };
        top.entries.insert(uid("uid://gone"), tree("nowhere"));
        top.ops.insert(uid("uid://unknown"), Op::Delete);
        top.save(&pe2_io::SystemFilesystem, &root.at(".layers/top.toml")).unwrap();
        write(&root, ".layers/top/b.node/node.toml", "stack = [{ op = \"t.file\" }]\n");
        write(&root, ".layers/top/new.node/node.toml", "kind = [unclosed\n");
        let (_, index) = open(&root);
        let has = |p: &Problem| index.problems.contains(p);
        assert!(has(&Problem::Conflict {
            path: "b.node".into(),
            layers: vec![crate::testing::name("base"), crate::testing::name("top")]
        }));
        assert!(has(&Problem::Unlisted { layer: crate::testing::name("top"), path: "new.node".into() }));
        assert!(has(&Problem::Missing { layer: crate::testing::name("top"), path: "nowhere".into(), left_out: false }));
        assert!(has(&Problem::Unbound { layer: crate::testing::name("top"), uid: "uid://unknown".into() }));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn follows_the_watcher() {
        let root = project("refresh");
        let (project, mut index) = open(&root);
        let folder = *changeset(&root, "base").entries.iter().find(|(_, p)| ***p == *"g").unwrap().0;
        // A file of a bundle changed: that bundle, its file read again.
        let texels = write(&root, ".layers/base/g/a.node/native.texels", "repainted");
        let refreshed = index.refresh(&project, &[texels]).unwrap();
        assert_eq!(refreshed.assets.iter().map(|asset| asset.as_str()).collect::<Vec<_>>(), ["g/a"]);
        assert_eq!(index.read, 1);
        // A folder renamed in the layer's folder, by other means than the editor: no edit, a problem - what it holds now
        // is unlisted, what it listed missing - until it is adopted, which follows the move: the uids are kept.
        std::fs::rename(root.at(".layers/base/g"), root.at(".layers/base/h")).unwrap();
        index.refresh(&project, &[root.at(".layers/base/g"), root.at(".layers/base/h")]).unwrap();
        assert!(!index.assets.contains_key(&tree("h/a")));
        assert!(index.problems.contains(&Problem::Unlisted { layer: crate::testing::name("base"), path: "h".into() }));
        let adopted = index.adoption(pe2_path::Name::constant("base"), &tree("h")).unwrap();
        assert!(adopted.contains(&(uid("uid://a"), tree("h/a.node"))), "{adopted:?}");
        assert!(adopted.contains(&(folder, tree("h"))), "{adopted:?}");
        let mut base = changeset(&root, "base");
        base.entries.extend(adopted);
        base.save(&pe2_io::SystemFilesystem, &root.at(".layers/base.toml")).unwrap();
        index.refresh(&project, &[root.at(".layers/base.toml")]).unwrap();
        assert_eq!(index.assets[&tree("h/a")].uid, Some(uid("uid://a")));
        assert!(index.problems.is_empty(), "{:?}", index.problems);
        // A bundle removed: its entry is missing until it is dropped from the changeset.
        std::fs::remove_dir_all(root.at(".layers/base/f.node")).unwrap();
        index.refresh(&project, &[root.at(".layers/base/f.node")]).unwrap();
        assert!(!index.assets.contains_key(&tree("f")));
        assert!(index.problems.contains(&Problem::Missing {
            layer: crate::testing::name("base"),
            path: "f.node".into(),
            left_out: false
        }));
        assert!(index.refresh(&project, &[root.at("project.toml")]).unwrap().project_file);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// What an index shows, in a form to compare: its assets (their declarations and files by name, hash and layer),
    /// entries, uids, references both ways and problems (in no particular order), stack and statuses.
    fn shown(index: &Index) -> String {
        index.shown_whole()
    }

    #[test]
    fn a_generated_layer_gives_the_nodes_it_fills_their_files() {
        let root = project("generated");
        // The nodes are base's; what their plan made, base-files' - without a node of its own, and its record beside.
        let text = "[[layer]]\nname = \"base\"\n\n[[layer]]\nname = \"base-files\"\nkind = \"generated\"\nfills = \"base\"\n\n[[layer]]\nname = \"top\"\n";
        write(&root, "project.toml", text);
        std::fs::remove_file(root.at(".layers/base/g/a.node/native.texels")).unwrap();
        write(&root, ".layers/base-files/g/a.node/native.texels", "texels");
        write(&root, ".layers/base-files/extraction.record.jsonl", "{}");
        let (project, mut index) = open(&root);
        let a = &index.assets[&tree("g/a")];
        let held: Vec<(&str, &str)> = a.files.iter().map(|f| (f.name.as_str(), f.layer.as_str())).collect();
        assert_eq!(held, [("native.texels", "base-files"), ("node.toml", "base")]);
        assert!(index.problems.is_empty(), "{:?}", index.problems);
        assert!(
            index
                .layers()
                .iter()
                .any(|l| *l.name == "base-files" && l.fills.as_deref() == Some(pe2_path::Name::constant("base")))
        );
        // What the plan writes again is taken in as the node's.
        let (_, fresh) = open(&root);
        let texels = write(&root, ".layers/base-files/g/a.node/native.texels", "repainted");
        index.refresh(&project, std::slice::from_ref(&texels)).unwrap();
        assert_ne!(shown(&index), shown(&fresh));
        let (_, again) = open(&root);
        assert_eq!(shown(&index), shown(&again));
        // An override of the node's declaration sees the files the plan makes, and what it makes otherwise reaches it.
        let mut top = Changeset::default();
        top.ops.insert(uid("uid://a"), overriding(&index, "uid://a", &["node.toml"]));
        top.save(&pe2_io::SystemFilesystem, &root.at(".layers/top.toml")).unwrap();
        write(&root, ".layers/top/g/a.node/node.toml", "stack = [{ op = \"t.image\" }]\n");
        let below = index.below(pe2_path::Name::constant("top"), uid("uid://a")).unwrap();
        assert!(below.files.iter().any(|(name, _)| **name == *"native.texels"), "{:?}", below.files);
        write(&root, ".layers/base-files/g/a.node/native.texels", "extracted otherwise");
        let (_, after) = open(&root);
        assert!(!after.problems.iter().any(|p| matches!(p, Problem::Stale { .. })), "{:?}", after.problems);
        let texels = after.assets[&tree("g/a")].files.iter().find(|f| *f.name == *"native.texels").unwrap();
        assert_eq!(std::fs::read_to_string(&texels.path).unwrap(), "extracted otherwise");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_refresh_shows_what_opening_afresh_shows() {
        let root = project("afresh");
        let (project, mut index) = open(&root);
        // Each change refreshed, the stack applied again or not as expected, then held to an opening afresh.
        let check = |index: &mut Index, project: &Project, changed: &[HostPath], what: &str, restacked: bool| {
            assert_eq!(index.refresh(project, changed).unwrap().restacked, restacked, "{what}");
            let (_, fresh) = open(&root);
            assert_eq!(shown(index), shown(&fresh), "after {what}");
        };
        let base = |p: &str| root.at(".layers/base").at(p);
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
        top.ops.insert(uid("uid://a"), overriding(&index, "uid://a", &["node.toml"]));
        top.save(&pe2_io::SystemFilesystem, &root.at(".layers/top.toml")).unwrap();
        write(&root, ".layers/top/g/a.node/node.toml", "stack = [{ op = \"t.image\" }]\n");
        check(
            &mut index,
            &project,
            &[root.at(".layers/top.toml"), root.at(".layers/top/g/a.node/node.toml")],
            "an override",
            true,
        );
        write(&root, ".layers/base/g/a.node/native.texels", "retouched");
        check(&mut index, &project, &[base("g/a.node/native.texels")], "an override's base changed", true);
        write(&root, ".layers/top/g/a.node/node.toml", "stack = [{ op = \"t.image\" }]\nnote = \"uid://a\"\n");
        check(
            &mut index,
            &project,
            &[root.at(".layers/top/g/a.node/node.toml")],
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
        let tree = root.with_suffix(pe2_path::Name::constant(".tree"));
        let _ = std::fs::remove_dir_all(&tree);
        write(&tree, "main.c", "int main;");
        write(&tree, "main.o", "object");
        write(&tree, "x.node/node.toml", "stack = [{ op = \"t.file\" }]\n");
        write(&tree, ".gitignore", "*.o\n");
        let text = std::fs::read_to_string(root.at("project.toml")).unwrap();
        // Its place as it is written on this system, taken as it is: no character of it escapes another.
        let mount = format!("[[mount]]\nat = \"code\"\npath = '{}'\nread_only = true\n", tree.display());
        std::fs::write(root.at("project.toml"), format!("{text}\n{mount}")).unwrap();
        let (project, index) = open(&root);
        assert_eq!(project.layers.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), ["code", "base", "top"]);
        let mut listed: Vec<String> = changeset(&root, "code").entries.into_values().map(|p| p.to_string()).collect();
        listed.sort();
        assert_eq!(listed, ["code", "code/main.c", "code/x.node", "code/x.node/node.toml"]);
        assert_eq!(index.entries[&crate::testing::tree("code/main.c")].disk, tree.at("main.c"));
        assert!(!index.assets.contains_key(&crate::testing::tree("code/x")));
        assert!(index.problems.is_empty(), "{:?}", index.problems);
        std::fs::remove_dir_all(&root).unwrap();
        std::fs::remove_dir_all(&tree).unwrap();
    }

    #[test]
    fn hides_layers_from_the_view_only() {
        let root = project("hidden");
        let (_, index) = open(&root);
        let mut top = Changeset::default();
        top.ops.insert(uid("uid://a"), overriding(&index, "uid://a", &["node.toml"]));
        top.ops.insert(uid("uid://nothing"), Op::Delete);
        top.save(&pe2_io::SystemFilesystem, &root.at(".layers/top.toml")).unwrap();
        write(&root, ".layers/top/g/a.node/node.toml", "stack = [{ op = \"t.image\" }]\n");
        let (_, mut index) = open(&root);
        let info = index.layers();
        assert_eq!((info[1].overrides, info[1].deletes, info[1].problems), (1, 1, 1));
        assert_eq!(
            info[1].depends_on,
            [super::Dependency { layer: crate::testing::name("base"), operations: 1, references: 0 }]
        );
        let statuses: Vec<_> = index.operations(pe2_path::Name::constant("top")).iter().map(|o| o.status).collect();
        assert!(statuses.contains(&super::OpStatus::Applied) && statuses.contains(&super::OpStatus::Unbound));
        assert_eq!(
            index.assets[&tree("g/a")].layers.iter().map(|layer| layer.as_str()).collect::<Vec<_>>(),
            ["base", "top"]
        );
        // Hiding the top layer shows the base's version; the problems stay the whole stack's.
        let problems = index.problems.clone();
        assert!(index.set_hidden(&[crate::testing::name("top")].into()));
        assert_eq!(index.assets[&tree("g/a")].layers.iter().map(|layer| layer.as_str()).collect::<Vec<_>>(), ["base"]);
        assert_eq!(index.problems, problems);
        assert!(index.set_hidden(&std::collections::BTreeSet::new()));
        assert_eq!(
            index.assets[&tree("g/a")].layers.iter().map(|layer| layer.as_str()).collect::<Vec<_>>(),
            ["base", "top"]
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn what_is_known_of_the_files_outlasts_a_cache_wiped() {
        let root = project("foreign");
        assert_eq!(open(&root).1.read, 4);
        assert!(crate::shared::space_at(&root).at("clean").exists());
        assert_eq!(open(&root).1.read, 0);
        // A cache another wrote is wiped, but for what the project's processes share - which says its own format,
        // and of which schema of the cache it is: what is known of the files is known still.
        std::fs::write(root.at(".cache/version"), "schema 0\n").unwrap();
        std::fs::write(root.at(".cache/left.before"), "").unwrap();
        assert_eq!(open(&root).1.read, 0);
        assert!(!root.at(".cache/left.before").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
