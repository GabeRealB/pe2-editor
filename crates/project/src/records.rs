//! A layer stored as records: one per entry the layer creates or acts on, by uid, at `.records/<aa>/<uid>.toml` in the
//! layer's folder - no tree of folders and files, which nobody edits by hand. A record says what the layer does to its entry
//! (creates it, overrides, moves or deletes it), where it is (its parent's uid and its name, so a rename or a move of a
//! folder holding thousands of entries is one record), and its files: text inline whatever its length, so a layer's history reads as
//! text in version control, anything else by its SHA-1 in the store (`chunks::Store::put_file`). An override's file
//! carries what it was made from (`was`), or is removed.
//!
//! ```toml
//! format = 1
//! op = "override"
//! kind = "node"
//!
//! [files."node.toml"]
//! was = "3f2a…"
//! text = """
//! type = "data"
//! """
//!
//! [files."native.texels"]
//! sha1 = "9c1e…"
//! size = 1835008
//! ```
//!
//! A layer stored so says it in its folder (`.records/layer.toml`: its format, and what its changeset said of it - read
//! only, the names it ignores), and every record its format: a program meeting a format it does not know refuses it,
//! and writes nothing.
//!
//! What reads a layer sees it as it would a tree of files: each records layer's files at the paths a layer's tree would
//! have them (`Set`), its entries placed through the layers below, read through `content` as a packed layer's are.

use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// The folder of a layer's records, in its folder: a name a scan of the layer leaves out.
pub const DIR: &str = ".records";
/// The file marking a layer's folder as records: their format, and the layer's settings.
pub const MARKER: &str = "layer.toml";
/// The format of the records this program reads and writes.
pub const FORMAT: i64 = 1;

/// What a layer does to an entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Create,
    Override,
    Move,
    Delete,
}

/// What an entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Node,
    Folder,
    File,
}

/// A file's content in a record: inline text, or a file kept whole in the store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Content {
    Text(String),
    Stored { sha1: String, size: u64 },
}

/// A file of an entry, as a record holds it: its content, none where the layer removes it; and, for an override's,
/// the SHA-1 of the version below it was made from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    pub content: Option<Content>,
    pub was: Option<String>,
}

/// What a layer has of one entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub op: Op,
    pub kind: Kind,
    /// Where it is: its parent's uid (none at the top) and its name - for what the layer creates or moves.
    pub parent: Option<String>,
    pub name: Option<String>,
    /// Its files by name; a plain file's own by an empty name.
    pub files: BTreeMap<String, File>,
}

/// Where a layer keeps the extensions it holds - a records layer, or one stored as a tree, whose scan leaves `.records`
/// out: a record an id, `.records/extensions/<id>.toml`. They are no entries - an extension is not in the tree of what the project holds - and are sealed: kept whole, as one tree of
/// whole objects in the store (`chunks::Store::put_folder`), never merged, a higher layer's in place of a lower one's,
/// and deleted to leave a lower one's off:
///
/// ```toml
/// format = 1
/// op = "create"
/// kind = "extension"
/// tree = "<the SHA-1 of its tree>"
/// ```
pub const EXTENSIONS: &str = "extensions";

/// Where a layer's record of the extension of an id is.
#[must_use]
pub fn extension_path(layer: &Path, id: &str) -> PathBuf {
    layer.join(DIR).join(EXTENSIONS).join(format!("{id}.toml"))
}

/// The text of a layer's record of an extension: holding the tree named, or (none) leaving below's off.
#[must_use]
pub fn extension_record(tree: Option<&str>) -> String {
    let mut doc = toml_edit::DocumentMut::new();
    doc["format"] = toml_edit::value(FORMAT);
    doc["op"] = toml_edit::value(if tree.is_some() { "create" } else { "delete" });
    doc["kind"] = toml_edit::value("extension");
    if let Some(tree) = tree {
        doc["tree"] = toml_edit::value(tree);
    }
    doc.to_string()
}

/// The extensions a layer holds, by id: each its tree, none where the layer leaves below's off.
///
/// # Errors
/// A record cannot be read, or is of a format this program does not know.
pub fn extensions_of(layer: &Path) -> Result<BTreeMap<String, Option<String>>> {
    let mut out = BTreeMap::new();
    for file in std::fs::read_dir(layer.join(DIR).join(EXTENSIONS)).into_iter().flatten().flatten() {
        let path = file.path();
        let Some(id) = path.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".toml")) else {
            continue;
        };
        let text = std::fs::read_to_string(&path)?;
        let table: toml::Table = toml::from_str(&text).with_context(|| path.display().to_string())?;
        match table.get("format").and_then(toml::Value::as_integer) {
            Some(FORMAT) => {}
            Some(other) if other > FORMAT => bail!("{}: of format {other}, written by a newer pe2", path.display()),
            other => bail!("{}: not a record of format {FORMAT} ({other:?})", path.display()),
        }
        let word = |key: &str| table.get(key).and_then(toml::Value::as_str);
        if word("kind") != Some("extension") {
            bail!("{}: not an extension's record", path.display());
        }
        let tree = match word("op") {
            Some("create") => Some(word("tree").with_context(|| format!("{}: no tree", path.display()))?.to_string()),
            Some("delete") => None,
            other => bail!("{}: an extension is created or deleted, not {other:?}", path.display()),
        };
        out.insert(id.to_string(), tree);
    }
    Ok(out)
}

/// An extension a layer holds, as what reads its files sees it: they are at `<layer>/extensions/<id>.extension/<path>`
/// (`held_folder`), where it was a folder before it was a record, read from its tree in the store - a path that is on
/// no disk, read through `content` as a records layer's files are. The tree is the object: what is in it is named by
/// its path within it, never by its own hash.
#[derive(Clone, Debug)]
pub struct HeldExtension {
    pub layer: PathBuf,
    pub id: String,
    /// Its tree's hash.
    pub tree: String,
    pub files: std::sync::Arc<crate::chunks::Tree>,
    pub store: crate::chunks::Store,
    /// When its record was written.
    pub modified: std::time::SystemTime,
}

/// Where the files of the extension of an id a layer holds are read: its folder, as a path on no disk.
#[must_use]
pub fn held_folder(layer: &Path, id: &str) -> PathBuf {
    layer.join(crate::ext::folder::HELD).join(format!("{id}{}", crate::ext::folder::SUFFIX))
}

/// The trees of the extensions read, by their records' files and their stamps: a tree is read once.
type HeldRead = std::collections::HashMap<PathBuf, (std::time::SystemTime, u64, Option<std::sync::Arc<HeldExtension>>)>;
static HELD_TREES: std::sync::RwLock<Option<HeldRead>> = std::sync::RwLock::new(None);

/// The extension a layer holds at a folder (`held_folder`): none where the folder is no held extension's, or the layer
/// leaves it off.
#[must_use]
pub fn held(folder: &Path) -> Option<std::sync::Arc<HeldExtension>> {
    let id = crate::ext::folder::id_of(folder)?;
    let parent = folder.parent()?;
    if parent.file_name()? != crate::ext::folder::HELD {
        return None;
    }
    let layer = parent.parent()?;
    let record = extension_path(layer, id);
    let meta = std::fs::metadata(&record).ok()?;
    let stamp = (meta.modified().ok()?, meta.len());
    if let Some(found) =
        HELD_TREES.read().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref().and_then(|h| h.get(&record))
        && (found.0, found.1) == stamp
    {
        return found.2.clone();
    }
    let read = || -> Option<std::sync::Arc<HeldExtension>> {
        let tree = extensions_of(layer).ok()?.remove(id)??;
        let store = crate::chunks::Store::of(layer)?;
        let files = std::sync::Arc::new(store.get_tree(&tree).ok()?);
        Some(std::sync::Arc::new(HeldExtension {
            layer: layer.to_path_buf(),
            id: id.to_string(),
            tree,
            files,
            store,
            modified: stamp.0,
        }))
    };
    let held = read();
    let mut cache = HELD_TREES.write().unwrap_or_else(std::sync::PoisonError::into_inner);
    cache.get_or_insert_default().insert(record, (stamp.0, stamp.1, held.clone()));
    held
}

/// The held extension a path is in (`held`), and the path within its folder ("" for the folder).
#[must_use]
pub fn held_at(path: &Path) -> Option<(std::sync::Arc<HeldExtension>, String)> {
    // Its folder is the nearest ancestor named `<id>.extension` in a layer's `extensions/`.
    let folder = path.ancestors().find(|a| {
        crate::ext::folder::id_of(a).is_some()
            && a.parent().and_then(Path::file_name) == Some(crate::ext::folder::HELD.as_ref())
    })?;
    let held = held(folder)?;
    let within = path.strip_prefix(folder).ok()?.to_string_lossy().replace('\\', "/");
    Some((held, within))
}

impl HeldExtension {
    /// A file of it by its path within it.
    #[must_use]
    pub fn file(&self, name: &str) -> Option<&crate::chunks::TreeFile> {
        self.files.files.get(name)
    }

    /// Whether a path within it is a folder of it ("" its own).
    #[must_use]
    pub fn is_folder(&self, within: &str) -> bool {
        within.is_empty() || {
            let prefix = format!("{within}/");
            self.files.files.range(prefix.clone()..).next().is_some_and(|(name, _)| name.starts_with(&prefix))
        }
    }

    /// What a folder of it holds: each name, and whether it is a folder.
    #[must_use]
    pub fn list(&self, within: &str) -> Vec<(String, bool)> {
        let prefix = if within.is_empty() { String::new() } else { format!("{within}/") };
        let mut out: Vec<(String, bool)> = Vec::new();
        for name in self.files.files.keys().filter(|n| n.starts_with(&prefix)) {
            let rest = &name[prefix.len()..];
            let (first, folder) = match rest.split_once('/') {
                Some((first, _)) => (first, true),
                None => (rest, false),
            };
            if out.last().is_none_or(|(last, _)| last != first) {
                out.push((first.to_string(), folder));
            }
        }
        out
    }
}

/// Hold the extensions a layer has as folders (`extensions/<id>.extension`, as it held them before records) as
/// records: each folder kept as a tree in the store, its record written, the folder removed. How many there were.
///
/// # Errors
/// A folder cannot be read or kept, or its record written.
pub fn hold_extensions(project: &crate::Project, layer: &Path) -> Result<usize> {
    let held = layer.join(crate::ext::folder::HELD);
    let store = crate::chunks::Store::at(&project.root);
    let mut count = 0;
    for entry in std::fs::read_dir(&held).into_iter().flatten().flatten() {
        let folder = entry.path();
        let Some(id) = crate::ext::folder::id_of(&folder).map(str::to_string) else { continue };
        if !folder.is_dir() {
            continue;
        }
        let (tree, _) = store.put_folder(&folder)?;
        let at = extension_path(layer, &id);
        std::fs::create_dir_all(at.parent().unwrap_or(layer))?;
        crate::cache::write_atomic(&at, extension_record(Some(&tree)).as_bytes())?;
        std::fs::remove_dir_all(&folder)?;
        count += 1;
    }
    if std::fs::read_dir(&held).is_ok_and(|mut d| d.next().is_none()) {
        std::fs::remove_dir(&held)?;
    }
    Ok(count)
}

/// Where a layer's record of an entry is, in its folder.
#[must_use]
pub fn path_of(layer: &Path, uid: &str) -> PathBuf {
    let key = uid.strip_prefix("uid://").unwrap_or(uid);
    layer.join(DIR).join(&key[..2.min(key.len())]).join(format!("{key}.toml"))
}

/// The uid a record's file is of: none for a file that is no record.
#[must_use]
pub fn uid_of(path: &Path) -> Option<String> {
    let key = path.file_name()?.to_str()?.strip_suffix(".toml")?;
    let shard = path.parent()?;
    // Entries' records are sharded by two characters; the extensions' are elsewhere.
    let in_records =
        shard.file_name().is_some_and(|n| n.len() == 2) && shard.parent()?.file_name().is_some_and(|n| n == DIR);
    (in_records && !key.is_empty() && key != "layer").then(|| format!("uid://{key}"))
}

/// Whether a layer's folder is stored as records.
#[must_use]
pub fn is_records(layer: &Path) -> bool {
    layer.join(DIR).join(MARKER).is_file()
}

/// What a records layer says of itself, as a changeset says it of a layer stored as a tree.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    pub read_only: bool,
    pub ignore: Vec<String>,
}

/// Mark a layer's folder as records of this format, with its settings.
///
/// # Errors
/// The marker cannot be written.
pub fn mark(layer: &Path, settings: &Settings) -> Result<()> {
    let mut doc = toml_edit::DocumentMut::new();
    doc["format"] = toml_edit::value(FORMAT);
    if settings.read_only {
        doc["read_only"] = toml_edit::value(true);
    }
    if !settings.ignore.is_empty() {
        doc["ignore"] = toml_edit::value(settings.ignore.iter().map(String::as_str).collect::<toml_edit::Array>());
    }
    std::fs::create_dir_all(layer.join(DIR))?;
    let at = layer.join(DIR).join(MARKER);
    crate::cache::write_atomic(&at, doc.to_string().as_bytes())
}

/// A records layer's settings, its format checked: one this program knows.
///
/// # Errors
/// Another format, or no marker one can read.
pub fn settings(layer: &Path) -> Result<Settings> {
    let at = layer.join(DIR).join(MARKER);
    let text = std::fs::read_to_string(&at).with_context(|| at.display().to_string())?;
    let table: toml::Table = toml::from_str(&text).with_context(|| at.display().to_string())?;
    match table.get("format").and_then(toml::Value::as_integer) {
        Some(FORMAT) => {}
        Some(newer) if newer > FORMAT => {
            bail!("{}: its records are of format {newer}, written by a newer pe2: update to read it", layer.display())
        }
        other => bail!("{}: not records this pe2 reads (format {other:?})", layer.display()),
    }
    let ignore = table.get("ignore").and_then(toml::Value::as_array).into_iter().flatten();
    Ok(Settings {
        read_only: table.get("read_only").and_then(toml::Value::as_bool).unwrap_or(false),
        ignore: ignore.filter_map(|v| v.as_str().map(str::to_string)).collect(),
    })
}

impl Op {
    fn name(self) -> &'static str {
        match self {
            Op::Create => "create",
            Op::Override => "override",
            Op::Move => "move",
            Op::Delete => "delete",
        }
    }
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Node => "node",
            Kind::Folder => "folder",
            Kind::File => "file",
        }
    }
}

impl Content {
    /// A file's content as a record keeps it: text inline whatever its length, so a layer's history always reads as
    /// text, anything else (bytes that are not UTF-8, or hold a NUL, as git tells binary files) kept in the store.
    ///
    /// # Errors
    /// It cannot be kept.
    pub fn of(bytes: &[u8], store: &crate::chunks::Store) -> Result<Content> {
        if !bytes.contains(&0)
            && let Ok(text) = std::str::from_utf8(bytes)
        {
            return Ok(Content::Text(text.to_string()));
        }
        let sha1 = store.put_file(bytes)?;
        Ok(Content::Stored { sha1, size: bytes.len() as u64 })
    }

    /// Its bytes.
    ///
    /// # Errors
    /// A file kept in the store is not there.
    pub fn bytes(&self, store: &crate::chunks::Store) -> std::io::Result<Vec<u8>> {
        match self {
            Content::Text(text) => Ok(text.clone().into_bytes()),
            Content::Stored { sha1, .. } => store.get_file(sha1),
        }
    }

    /// Its length.
    #[must_use]
    pub fn size(&self) -> u64 {
        match self {
            Content::Text(text) => text.len() as u64,
            Content::Stored { size, .. } => *size,
        }
    }

    /// Its SHA-1.
    #[must_use]
    pub fn sha1(&self) -> String {
        match self {
            Content::Text(text) => crate::extract::sha1_hex(text.as_bytes()),
            Content::Stored { sha1, .. } => sha1.clone(),
        }
    }
}

impl Record {
    /// A record's text read.
    ///
    /// # Errors
    /// It is no record, or of a format this program does not know.
    pub fn parse(text: &str) -> Result<Record> {
        let table: toml::Table = toml::from_str(text)?;
        match table.get("format").and_then(toml::Value::as_integer) {
            Some(FORMAT) => {}
            Some(other) if other > FORMAT => bail!("a record of format {other}, written by a newer pe2"),
            other => bail!("not a record of format {FORMAT} ({other:?})"),
        }
        let word = |key: &str| table.get(key).and_then(toml::Value::as_str).with_context(|| format!("no `{key}`"));
        let op = match word("op")? {
            "create" => Op::Create,
            "override" => Op::Override,
            "move" => Op::Move,
            "delete" => Op::Delete,
            other => bail!("an op that is none: {other}"),
        };
        let kind = match word("kind")? {
            "node" => Kind::Node,
            "folder" => Kind::Folder,
            "file" => Kind::File,
            other => bail!("a kind that is none: {other}"),
        };
        let string = |key: &str| table.get(key).and_then(toml::Value::as_str).map(str::to_string);
        let mut files = BTreeMap::new();
        for (name, file) in table.get("files").and_then(toml::Value::as_table).into_iter().flatten() {
            let file = file.as_table().with_context(|| format!("{name}: not a table"))?;
            let field = |key: &str| file.get(key).and_then(toml::Value::as_str).map(str::to_string);
            let content = match (field("text"), field("sha1")) {
                (Some(text), None) => Some(Content::Text(text)),
                (None, Some(sha1)) => {
                    let size = file.get("size").and_then(toml::Value::as_integer).context("a stored file's size")?;
                    Some(Content::Stored { sha1, size: u64::try_from(size)? })
                }
                (None, None) if file.get("removed").and_then(toml::Value::as_bool) == Some(true) => None,
                _ => bail!("{name}: neither text, a stored file nor removed"),
            };
            files.insert(name.clone(), File { content, was: field("was") });
        }
        Ok(Record { op, kind, parent: string("parent"), name: string("name"), files })
    }

    /// Its text: its format first, what it does and where, then its files by name.
    #[must_use]
    pub fn render(&self) -> String {
        let mut doc = toml_edit::DocumentMut::new();
        doc["format"] = toml_edit::value(FORMAT);
        doc["op"] = toml_edit::value(self.op.name());
        doc["kind"] = toml_edit::value(self.kind.name());
        if let Some(parent) = &self.parent {
            doc["parent"] = toml_edit::value(parent.as_str());
        }
        if let Some(name) = &self.name {
            doc["name"] = toml_edit::value(name.as_str());
        }
        if !self.files.is_empty() {
            let mut files = toml_edit::Table::new();
            files.set_implicit(true);
            for (name, file) in &self.files {
                let mut table = toml_edit::Table::new();
                if let Some(was) = &file.was {
                    table["was"] = toml_edit::value(was.as_str());
                }
                match &file.content {
                    Some(Content::Text(text)) => table["text"] = toml_edit::value(text.as_str()),
                    Some(Content::Stored { sha1, size }) => {
                        table["sha1"] = toml_edit::value(sha1.as_str());
                        table["size"] = toml_edit::value(i64::try_from(*size).unwrap_or(i64::MAX));
                    }
                    None => table["removed"] = toml_edit::value(true),
                }
                files.insert(name, toml_edit::Item::Table(table));
            }
            doc["files"] = toml_edit::Item::Table(files);
        }
        doc.to_string()
    }
}

/// A records layer as what reads it sees it: its records by uid, each with its file's time, and the tree of files they
/// make at the paths a layer's tree would have them.
#[derive(Default)]
struct View {
    dir: PathBuf,
    settings: Settings,
    records: Read,
    /// Each file by its path in the layer: its entry's uid and its name in the entry ("" for a plain file).
    files: BTreeMap<String, (String, String)>,
    folders: std::collections::BTreeSet<String>,
    /// Where each of its records' entries is.
    placed: std::collections::HashMap<String, String>,
    /// What each folder holds, by its path ("" the layer's top): each name, and whether it is a folder.
    children: std::collections::HashMap<String, Vec<(String, bool)>>,
}

impl View {
    /// What each folder holds, from the files and folders.
    fn list_children(&mut self) {
        let mut children: std::collections::HashMap<String, Vec<(String, bool)>> = std::collections::HashMap::new();
        let parts = |path: &str| match path.rsplit_once('/') {
            Some((parent, name)) => (parent.to_string(), name.to_string()),
            None => (String::new(), path.to_string()),
        };
        for folder in &self.folders {
            let (parent, name) = parts(folder);
            children.entry(parent).or_default().push((name, true));
        }
        for file in self.files.keys() {
            let (parent, name) = parts(file);
            children.entry(parent).or_default().push((name, false));
        }
        self.children = children;
    }
}

/// What differs between two readings of a project's records layers (`Set::differences`).
#[derive(Clone, Debug, Default)]
pub struct Differences {
    /// For each layer (by folder), where the entries whose records came, went or changed were, and are.
    pub paths: Vec<(PathBuf, BTreeSet<String>)>,
    /// Any of them is placed otherwise.
    pub placed: bool,
    /// Any of them is (or was) an override: what it merges is merged again.
    pub overrides: bool,
}

/// A record read, with the stamp of its file: what a reading again finds unchanged it takes as it was.
#[derive(Clone)]
struct Held {
    record: std::sync::Arc<Record>,
    modified: std::time::SystemTime,
    len: u64,
}

/// A layer's records read, by uid.
type Read = BTreeMap<String, Held>;

/// A project's records layers, as seen through its stack: what `content` and a scan read them by. One a project, made
/// again as its records or its changesets change (`Set::open`).
pub struct Set {
    root: PathBuf,
    store: crate::chunks::Store,
    views: Vec<View>,
    /// Records whose place cannot be found, by what: their parent is nowhere below.
    pub problems: Vec<String>,
}

static SETS: std::sync::RwLock<Vec<std::sync::Arc<Set>>> = std::sync::RwLock::new(Vec::new());

/// The path of an entry named so in a folder: a node's with `.node`.
fn joined(parent: Option<&str>, name: &str, kind: Kind) -> String {
    let name = if kind == Kind::Node { format!("{name}{}", crate::index::BUNDLE_SUFFIX) } else { name.to_string() };
    match parent {
        Some(parent) if !parent.is_empty() => format!("{parent}/{name}"),
        _ => name,
    }
}

/// Where a record's entry is - through its parent, in its layer or below, or where it is below - remembered in
/// `placed`; none where its parent is nowhere.
fn place(
    uid: &str,
    records: &Read,
    below: &std::collections::HashMap<String, String>,
    placed: &mut std::collections::HashMap<String, String>,
    depth: usize,
) -> Option<String> {
    if let Some(at) = placed.get(uid) {
        return Some(at.clone());
    }
    let at = match records.get(uid) {
        Some(Held { record, .. }) if depth < 256 && record.name.is_some() => {
            let parent = match &record.parent {
                Some(parent) => Some(place(parent, records, below, placed, depth + 1)?),
                None => None,
            };
            joined(parent.as_deref(), record.name.as_deref()?, record.kind)
        }
        _ => below.get(uid)?.clone(),
    };
    placed.insert(uid.to_string(), at.clone());
    Some(at)
}

/// A records layer's records, read on every core: those whose file is as it was when `before` read it taken from it.
fn read_records(dir: &Path, before: Option<&Read>) -> Result<Read> {
    let shards: Vec<PathBuf> = std::fs::read_dir(dir.join(DIR))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|shard| shard.file_type().is_ok_and(|t| t.is_dir()) && shard.file_name().len() == 2)
        .map(|shard| shard.path())
        .collect();
    let read_shard = |shard: &PathBuf| -> Result<Vec<(String, Held)>> {
        let mut out = Vec::new();
        for file in std::fs::read_dir(shard)?.flatten() {
            let path = file.path();
            let Some(uid) = uid_of(&path) else { continue };
            let meta = file.metadata()?;
            let (modified, len) = (meta.modified()?, meta.len());
            if let Some(held) = before.and_then(|b| b.get(&uid)).filter(|h| h.modified == modified && h.len == len) {
                out.push((uid, held.clone()));
                continue;
            }
            let text = std::fs::read_to_string(&path)?;
            let record = Record::parse(&text).with_context(|| path.display().to_string())?;
            out.push((uid, Held { record: std::sync::Arc::new(record), modified, len }));
        }
        Ok(out)
    };
    if shards.is_empty() {
        return Ok(BTreeMap::new());
    }
    let threads = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    let parts: Vec<Result<Vec<(String, Held)>>> = std::thread::scope(|scope| {
        let workers: Vec<_> = shards
            .chunks(shards.len().div_ceil(threads))
            .map(|part| {
                scope.spawn(move || {
                    part.iter().map(read_shard).collect::<Result<Vec<_>>>().map(|v| v.into_iter().flatten().collect())
                })
            })
            .collect();
        workers.into_iter().map(|w| w.join().unwrap_or_else(|_| Err(anyhow::anyhow!("a reader failed")))).collect()
    });
    let mut out = BTreeMap::new();
    for part in parts {
        out.extend(part?);
    }
    Ok(out)
}

impl Set {
    /// Read a project's records layers, placing their entries through its stack, and serve them to what reads its files
    /// from now on (in the place of what was read of the project before).
    ///
    /// # Errors
    /// A records layer cannot be read, or is of a format this program does not know.
    pub fn open(project: &crate::Project) -> Result<std::sync::Arc<Set>> {
        let _span = tracing::info_span!("records open").entered();
        // What was read of it before: a record whose file is as it was is not read again.
        let previous = Set::current(&project.root);
        let mut places: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        let (mut views, mut problems) = (Vec::new(), Vec::new());
        for layer in &project.layers {
            if !is_records(&layer.path) {
                let changeset = crate::changeset::Changeset::load(&layer.changeset).unwrap_or_default();
                places.extend(changeset.entries);
                for (uid, op) in changeset.ops {
                    if let crate::changeset::Op::Move { to } | crate::changeset::Op::Override { to: Some(to), .. } = op
                    {
                        places.insert(uid, to);
                    }
                }
                continue;
            }
            let settings = settings(&layer.path)?;
            let before = previous.as_ref().and_then(|p| p.views.iter().find(|v| v.dir == layer.path));
            let records = read_records(&layer.path, before.map(|v| &v.records))?;
            let mut view = View { dir: layer.path.clone(), settings, ..View::default() };
            // Each record's place (`place`).
            let mut located: std::collections::HashMap<String, String> = std::collections::HashMap::new();
            for uid in records.keys() {
                if place(uid, &records, &places, &mut located, 0).is_none() {
                    problems.push(format!("{}: {uid} has no place: what it is in is nowhere below", layer.name));
                }
            }
            for (uid, Held { record, .. }) in &records {
                let Some(at) = located.get(uid) else { continue };
                // What it places (creates, moves) is there for the layers above.
                if record.name.is_some() {
                    places.insert(uid.clone(), at.clone());
                }
                if record.op == Op::Delete || record.op == Op::Move {
                    continue;
                }
                let mut folder = Path::new(at).parent();
                while let Some(f) = folder.filter(|f| !f.as_os_str().is_empty()) {
                    view.folders.insert(f.to_string_lossy().replace('\\', "/"));
                    folder = f.parent();
                }
                match record.kind {
                    Kind::Folder => {
                        view.folders.insert(at.clone());
                    }
                    Kind::File => {
                        if record.files.get("").is_some_and(|f| f.content.is_some()) {
                            view.files.insert(at.clone(), (uid.clone(), String::new()));
                        }
                    }
                    Kind::Node => {
                        view.folders.insert(at.clone());
                        for (name, file) in &record.files {
                            if file.content.is_some() {
                                view.files.insert(format!("{at}/{name}"), (uid.clone(), name.clone()));
                                let mut folder = Path::new(name).parent();
                                while let Some(f) = folder.filter(|f| !f.as_os_str().is_empty()) {
                                    view.folders.insert(format!("{at}/{}", f.to_string_lossy().replace('\\', "/")));
                                    folder = f.parent();
                                }
                            }
                        }
                    }
                }
            }
            view.records = records;
            view.placed = located;
            view.list_children();
            views.push(view);
        }
        let set = std::sync::Arc::new(Set {
            root: project.root.clone(),
            store: crate::chunks::Store::at(&project.root),
            views,
            problems,
        });
        let mut sets = SETS.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        sets.retain(|s| s.root != project.root);
        if !set.views.is_empty() {
            sets.push(set.clone());
        }
        Ok(set)
    }

    /// What is read of a project's records layers now, where anything is.
    #[must_use]
    pub fn current(root: &Path) -> Option<std::sync::Arc<Set>> {
        let sets = SETS.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        sets.iter().find(|s| s.root == root).cloned()
    }

    /// What differs between two readings of a project's records layers: for each layer (by folder), the paths of the
    /// entries whose records came, went or changed - where each was and where each is - and whether any of them is
    /// placed otherwise (moved, renamed, made or deleted), which moves what is placed through it, in higher layers too.
    #[must_use]
    pub fn differences(before: Option<&Set>, now: &Set) -> Differences {
        let mut out = Vec::new();
        let (mut placed, mut overrides) = (false, false);
        for view in &now.views {
            let old = before.and_then(|b| b.views.iter().find(|v| v.dir == view.dir));
            let empty = BTreeMap::new();
            let old_records = old.map_or(&empty, |o| &o.records);
            let mut paths = BTreeSet::new();
            let uids: BTreeSet<&String> = view.records.keys().chain(old_records.keys()).collect();
            for uid in uids {
                let (was, is) = (old_records.get(uid), view.records.get(uid));
                if let (Some(was), Some(is)) = (was, is)
                    && std::sync::Arc::ptr_eq(&was.record, &is.record)
                {
                    continue;
                }
                overrides |= [was, is].into_iter().flatten().any(|held| held.record.op == Op::Override);
                let at_before = old.and_then(|o| o.placed.get(uid));
                let at_now = view.placed.get(uid);
                paths.extend(at_before.cloned());
                paths.extend(at_now.cloned());
                let same_place = match (was, is) {
                    (Some(was), Some(is)) => {
                        at_before == at_now
                            && was.record.op == is.record.op
                            && was.record.kind == is.record.kind
                            && was.record.parent == is.record.parent
                            && was.record.name == is.record.name
                    }
                    _ => false,
                };
                placed |= !same_place;
            }
            if old.is_none() {
                placed = true;
            }
            if !paths.is_empty() {
                out.push((view.dir.clone(), paths));
            }
        }
        // A layer read before and no more.
        if let Some(before) = before {
            placed |= before.views.iter().any(|o| !now.views.iter().any(|v| v.dir == o.dir));
        }
        Differences { paths: out, placed, overrides }
    }

    /// The records layer a path is in, and the path within it.
    fn view_of(path: &Path) -> Option<(std::sync::Arc<Set>, usize, String)> {
        let sets = SETS.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        for set in sets.iter() {
            for (i, view) in set.views.iter().enumerate() {
                if let Ok(rel) = path.strip_prefix(&view.dir) {
                    return Some((set.clone(), i, rel.to_string_lossy().replace('\\', "/")));
                }
            }
        }
        None
    }

    /// A file of a records layer by its path: its content, and when its record was written.
    #[must_use]
    pub fn file(path: &Path) -> Option<(Content, std::time::SystemTime, crate::chunks::Store)> {
        let (set, i, rel) = Set::view_of(path)?;
        let view = &set.views[i];
        let (uid, name) = view.files.get(&rel)?;
        let held = view.records.get(uid)?;
        Some((held.record.files.get(name)?.content.clone()?, held.modified, set.store.clone()))
    }

    /// Where the content of a file of a records layer lives, for what serves it to read it without the records: the
    /// record holding its text, or the store keeping it.
    #[must_use]
    pub fn source(path: &Path) -> Option<crate::server::Source> {
        use crate::server::Source;
        let (set, i, rel) = Set::view_of(path)?;
        let view = &set.views[i];
        let (uid, name) = view.files.get(&rel)?;
        let held = view.records.get(uid)?;
        Some(match held.record.files.get(name)?.content.as_ref()? {
            Content::Text(_) => Source::RecordText { record: path_of(&view.dir, uid), name: name.clone() },
            Content::Stored { sha1, .. } => Source::Stored { sha1: sha1.clone(), store: set.store.dir.clone() },
        })
    }

    /// A file of a records layer's length and when its record was written, without its content.
    #[must_use]
    pub fn stat(path: &Path) -> Option<(u64, std::time::SystemTime)> {
        let (set, i, rel) = Set::view_of(path)?;
        let view = &set.views[i];
        let (uid, name) = view.files.get(&rel)?;
        let held = view.records.get(uid)?;
        Some((held.record.files.get(name)?.content.as_ref()?.size(), held.modified))
    }

    /// What a folder of a records layer holds: each name, and whether it is a folder. None for a path in no records layer.
    #[must_use]
    pub fn list(dir: &Path) -> Option<Vec<(String, bool)>> {
        let (set, i, rel) = Set::view_of(dir)?;
        let view = &set.views[i];
        let out = view.children.get(&rel).cloned().unwrap_or_default();
        Some(out)
    }

    /// A records layer's records, by uid: none where the folder is no records layer read.
    #[must_use]
    pub fn records_of(dir: &Path) -> Option<BTreeMap<String, Record>> {
        let (set, i, _) = Set::view_of(&dir.join(DIR))?;
        Some(set.views[i].records.iter().map(|(uid, held)| (uid.clone(), (*held.record).clone())).collect())
    }

    /// Whether a path is a folder of a records layer: none where it is in none.
    #[must_use]
    pub fn is_folder(path: &Path) -> Option<bool> {
        let (set, i, rel) = Set::view_of(path)?;
        Some(rel.is_empty() || set.views[i].folders.contains(&rel))
    }

    /// Whether a path is a file of a records layer: none where it is in none.
    #[must_use]
    pub fn is_file(path: &Path) -> Option<bool> {
        let (set, i, rel) = Set::view_of(path)?;
        Some(set.views[i].files.contains_key(&rel))
    }

    /// What a records layer's records say as a changeset says it of a layer stored as a tree: what it creates, by uid
    /// and path; what it does to entries from below; whether it is read only, what it ignores.
    #[must_use]
    pub fn changeset(dir: &Path) -> Option<crate::changeset::Changeset> {
        let (set, i, _) = Set::view_of(&dir.join(DIR))?;
        let view = &set.views[i];
        let mut changeset = crate::changeset::Changeset {
            read_only: view.settings.read_only,
            ignore: view.settings.ignore.clone(),
            ..crate::changeset::Changeset::default()
        };
        let path_of = |uid: &str| view.placed.get(uid).cloned();
        for (uid, Held { record, .. }) in &view.records {
            let op = match record.op {
                Op::Create => {
                    if let Some(at) = path_of(uid) {
                        changeset.entries.insert(uid.clone(), at);
                    }
                    continue;
                }
                Op::Override => {
                    let files = record
                        .files
                        .iter()
                        .map(|(name, f)| {
                            let base = crate::changeset::Base { was: f.was.clone(), removed: f.content.is_none() };
                            (name.clone(), base)
                        })
                        .collect();
                    let to = if record.name.is_some() { path_of(uid) } else { None };
                    crate::changeset::Op::Override { files, to }
                }
                Op::Move => match path_of(uid) {
                    Some(to) => crate::changeset::Op::Move { to },
                    None => continue,
                },
                Op::Delete => crate::changeset::Op::Delete,
            };
            changeset.ops.insert(uid.clone(), op);
        }
        Some(changeset)
    }
}

/// What a layer stored as a tree holds, as records: one per entry it creates or acts on, read from its changeset and its
/// files (by the index, which places them), each file's content as a record keeps it.
///
/// # Errors
/// A file cannot be read or kept, or an entry's place has no uid to name it by.
pub fn of_tree(
    project: &crate::Project,
    index: &crate::index::Index,
    layer: &crate::Layer,
) -> Result<(Settings, BTreeMap<String, Record>)> {
    let changeset = index.changeset(&layer.name).with_context(|| format!("the index has no layer {}", layer.name))?;
    let records = records_of_tree(project, index, &layer.name, &|path| layer.disk(path), changeset)?;
    Ok((Settings { read_only: changeset.read_only, ignore: changeset.ignore.clone() }, records))
}

/// The records a tree of files and its changeset make, as a layer's (`layer`, which places what it names below it):
/// `disk` says where a path of the tree is.
fn records_of_tree(
    project: &crate::Project,
    index: &crate::index::Index,
    layer: &str,
    disk: &dyn Fn(&str) -> PathBuf,
    changeset: &crate::changeset::Changeset,
) -> Result<BTreeMap<String, Record>> {
    use crate::changeset::Op as Did;
    let store = crate::chunks::Store::at(&project.root);
    let below = index.entries_below(layer);
    // Who is where: what the layer places, then what is below it.
    let mut uid_at: std::collections::HashMap<String, String> =
        below.iter().map(|(path, (uid, _))| (path.clone(), uid.clone())).collect();
    for (uid, path) in &changeset.entries {
        uid_at.insert(path.clone(), uid.clone());
    }
    for (uid, op) in &changeset.ops {
        if let Did::Move { to } | Did::Override { to: Some(to), .. } = op {
            uid_at.insert(to.clone(), uid.clone());
        }
    }
    let place = |path: &str| -> Result<(Option<String>, String)> {
        let (parent, name) = match path.rsplit_once('/') {
            Some((parent, name)) => (Some(parent), name),
            None => (None, path),
        };
        let parent = match parent {
            Some(parent) => {
                Some(uid_at.get(parent).cloned().with_context(|| format!("{path}: nothing names {parent}"))?)
            }
            None => None,
        };
        Ok((parent, name.strip_suffix(crate::index::BUNDLE_SUFFIX).unwrap_or(name).to_string()))
    };
    let kind_at = |path: &str| {
        if path.ends_with(crate::index::BUNDLE_SUFFIX) {
            Kind::Node
        } else if crate::pack::is_dir(&disk(path)) {
            Kind::Folder
        } else {
            Kind::File
        }
    };
    let content = |disk: &Path| -> Result<Content> {
        let bytes = crate::content::read(disk).with_context(|| disk.display().to_string())?;
        Content::of(&bytes, &store)
    };
    // A bundle's files, by name, as its folder holds them.
    let bundle = |path: &str| -> Result<BTreeMap<String, File>> {
        let mut files = BTreeMap::new();
        for (name, file) in crate::index::bundle_files(&disk(path)) {
            files.insert(name, File { content: Some(content(&file)?), was: None });
        }
        Ok(files)
    };
    let mut records = BTreeMap::new();
    for (uid, path) in &changeset.entries {
        let kind = kind_at(path);
        let files = match kind {
            Kind::Node => bundle(path)?,
            Kind::File => BTreeMap::from([(String::new(), File { content: Some(content(&disk(path))?), was: None })]),
            Kind::Folder => BTreeMap::new(),
        };
        let (parent, name) = place(path)?;
        records.insert(uid.clone(), Record { op: Op::Create, kind, parent, name: Some(name), files });
    }
    for (uid, op) in &changeset.ops {
        let from = below.iter().find(|(_, (u, _))| u == uid);
        let kind = from.map_or(Kind::File, |(_, (_, kind))| match kind {
            crate::index::Kind::Asset => Kind::Node,
            crate::index::Kind::Folder => Kind::Folder,
            crate::index::Kind::File => Kind::File,
        });
        let record = match op {
            Did::Delete => Record { op: Op::Delete, kind, parent: None, name: None, files: BTreeMap::new() },
            Did::Move { to } => {
                let (parent, name) = place(to)?;
                Record { op: Op::Move, kind, parent, name: Some(name), files: BTreeMap::new() }
            }
            Did::Override { files: bases, to } => {
                let at = to.clone().or_else(|| from.map(|(path, _)| path.clone())).context("an override of nothing")?;
                let (parent, name) = match to {
                    Some(to) => {
                        let (parent, name) = place(to)?;
                        (parent, Some(name))
                    }
                    None => (None, None),
                };
                let mut files = BTreeMap::new();
                for (file, base) in bases {
                    let held_at = if file.is_empty() { disk(&at) } else { disk(&at).join(file) };
                    let held = if base.removed { None } else { Some(content(&held_at)?) };
                    files.insert(file.clone(), File { content: held, was: base.was.clone() });
                }
                Record { op: Op::Override, kind, parent, name, files }
            }
        };
        records.insert(uid.clone(), record);
    }
    Ok(records)
}

/// Make a layer's records those of a tree of files and its changeset (`<folder>.toml`, beside it) - what a tool writes
/// as a tree (`tools/editor_project/init.py`) - in place of all it had but the extensions it holds: a records layer from
/// now on, its tree, where it was one, gone. How many records it holds.
///
/// # Errors
/// The tree or its changeset cannot be read, or the records cannot be written.
pub fn import(project: &crate::Project, cache: &crate::cache::Cache, name: &str, from: &Path) -> Result<usize> {
    let layer = project.layer(name)?;
    if layer.mounted || layer.kind == crate::LayerKind::Generated {
        bail!("{name}: a mounted or generated layer is no records layer");
    }
    let changeset = crate::changeset::Changeset::load(&from.with_extension("toml"))?;
    let records = {
        let index = crate::index::Index::open(project, cache)?;
        records_of_tree(project, &index, name, &|path| from.join(path), &changeset)?
    };
    let settings = Settings { read_only: changeset.read_only, ignore: changeset.ignore.clone() };
    // Written beside, as a layer of its own, then in the place of what was there.
    let fresh = layer.path.join(".import");
    let _ = std::fs::remove_dir_all(&fresh);
    write_all(&fresh, &settings, &records)?;
    // The extensions it holds are no entries of the tree: kept as they are.
    let held = layer.path.join(DIR).join(EXTENSIONS);
    if held.is_dir() {
        std::fs::rename(&held, fresh.join(DIR).join(EXTENSIONS))?;
    }
    let old = layer.path.join(".records.old");
    let _ = std::fs::remove_dir_all(&old);
    if layer.path.join(DIR).exists() {
        std::fs::rename(layer.path.join(DIR), &old)?;
    }
    std::fs::rename(fresh.join(DIR), layer.path.join(DIR))?;
    let _ = std::fs::remove_dir_all(&old);
    std::fs::remove_dir_all(&fresh)?;
    // A tree it was: its files and changeset go.
    for entry in std::fs::read_dir(&layer.path)?.flatten() {
        if of_the_tree(&entry.file_name()) {
            let path = entry.path();
            if path.is_dir() {
                std::fs::remove_dir_all(&path)?;
            } else {
                std::fs::remove_file(&path)?;
            }
        }
    }
    if layer.changeset.is_file() {
        std::fs::remove_file(&layer.changeset)?;
    }
    // What it held as folders of extensions, held as records.
    Ok(records.len() + hold_extensions(project, &layer.path)?)
}

/// Write a records layer's entries out as a tree of files, with its changeset beside it (`<folder>.toml`): what it
/// creates, for tools that read a layer as files (`init.py compare`). How many entries.
///
/// # Errors
/// The layer is no records layer read, or a file cannot be read or written.
pub fn export(project: &crate::Project, name: &str, to: &Path) -> Result<usize> {
    let layer = project.layer(name)?;
    Set::open(project)?;
    let mut changeset = Set::changeset(&layer.path).with_context(|| format!("{name} is no records layer"))?;
    changeset.ops.clear();
    for path in changeset.entries.values() {
        let (from, out) = (layer.path.join(path), to.join(path));
        if path.ends_with(crate::index::BUNDLE_SUFFIX) {
            std::fs::create_dir_all(&out)?;
            for (file, at) in crate::index::bundle_files(&from) {
                let target = out.join(&file);
                if let Some(folder) = target.parent() {
                    std::fs::create_dir_all(folder)?;
                }
                std::fs::write(&target, crate::content::read(&at)?)?;
            }
        } else if crate::content::exists(&from) {
            if let Some(folder) = out.parent() {
                std::fs::create_dir_all(folder)?;
            }
            std::fs::write(&out, crate::content::read(&from)?)?;
        } else {
            std::fs::create_dir_all(&out)?;
        }
    }
    changeset.save(&to.with_extension("toml"))?;
    Ok(changeset.entries.len())
}

/// Write a layer's records into its folder, marked with its settings.
///
/// # Errors
/// A record cannot be written.
pub fn write_all(dir: &Path, settings: &Settings, records: &BTreeMap<String, Record>) -> Result<()> {
    for (uid, record) in records {
        let at = path_of(dir, uid);
        if let Some(shard) = at.parent() {
            std::fs::create_dir_all(shard)?;
        }
        crate::cache::write_atomic(&at, record.render().as_bytes())?;
    }
    mark(dir, settings)
}

/// The records a layer's changeset makes, with what an edit wrote of its files (`written`: by their path in the layer,
/// their content or, none, removed), over the records it has (`old`): each record new or changed, and the uids whose
/// records go.
///
/// # Errors
/// A file was written where no entry of the layer is, an entry's place names nothing, or content cannot be kept.
pub fn records_for(
    project: &crate::Project,
    index: &crate::index::Index,
    layer: &crate::Layer,
    changeset: &crate::changeset::Changeset,
    old: &BTreeMap<String, Record>,
    written: &BTreeMap<String, Option<Vec<u8>>>,
) -> Result<(BTreeMap<String, Record>, Vec<String>)> {
    use crate::changeset::Op as Did;
    let store = crate::chunks::Store::at(&project.root);
    let below = index.entries_below(&layer.name);
    let mut uid_at: std::collections::HashMap<String, String> =
        below.iter().map(|(path, (uid, _))| (path.clone(), uid.clone())).collect();
    let mut path_below: std::collections::HashMap<&String, (&String, crate::index::Kind)> =
        below.iter().map(|(path, (uid, kind))| (uid, (path, *kind))).collect();
    for (uid, path) in &changeset.entries {
        uid_at.insert(path.clone(), uid.clone());
    }
    for (uid, op) in &changeset.ops {
        if let Did::Move { to } | Did::Override { to: Some(to), .. } = op {
            uid_at.insert(to.clone(), uid.clone());
        }
    }
    let place = |path: &str| -> Result<(Option<String>, String)> {
        let (parent, name) = path.rsplit_once('/').map_or((None, path), |(p, n)| (Some(p), n));
        let parent = match parent {
            Some(p) => Some(uid_at.get(p).cloned().with_context(|| format!("{path}: nothing names {p}"))?),
            None => None,
        };
        Ok((parent, name.strip_suffix(crate::index::BUNDLE_SUFFIX).unwrap_or(name).to_string()))
    };
    let mut claimed = std::collections::HashSet::new();
    // What was written of an entry at a path: its files by name, as a node's are inside its folder, a file's itself.
    let mut written_at = |path: &str, kind: Kind| -> Vec<(String, Option<Vec<u8>>)> {
        let inside = format!("{path}/");
        let found: Vec<(String, Option<Vec<u8>>)> = written
            .iter()
            .filter_map(|(at, bytes)| match kind {
                Kind::Node => at.strip_prefix(&inside).map(|name| (name.to_string(), bytes.clone())),
                Kind::File => (at == path).then(|| (String::new(), bytes.clone())),
                Kind::Folder => None,
            })
            .collect();
        claimed.extend(written.keys().filter(|at| *at == path || at.starts_with(&inside)).cloned());
        found
    };
    let of_kind = |kind: crate::index::Kind| match kind {
        crate::index::Kind::Asset => Kind::Node,
        crate::index::Kind::Folder => Kind::Folder,
        crate::index::Kind::File => Kind::File,
    };
    let mut new = BTreeMap::new();
    for (uid, path) in &changeset.entries {
        let before = old.get(uid).filter(|r| r.op == Op::Create);
        let kind = before.map_or_else(
            || {
                if path.ends_with(crate::index::BUNDLE_SUFFIX) {
                    Kind::Node
                } else if written.get(path).is_some_and(Option::is_some) {
                    Kind::File
                } else {
                    Kind::Folder
                }
            },
            |r| r.kind,
        );
        let mut files = before.map_or_else(BTreeMap::new, |r| r.files.clone());
        for (name, bytes) in written_at(path, kind) {
            match bytes {
                Some(bytes) => files.insert(name, File { content: Some(Content::of(&bytes, &store)?), was: None }),
                None => files.remove(&name),
            };
        }
        let (parent, name) = place(path)?;
        new.insert(uid.clone(), Record { op: Op::Create, kind, parent, name: Some(name), files });
    }
    for (uid, op) in &changeset.ops {
        let from = path_below.remove(uid);
        let kind = old.get(uid).map(|r| r.kind).or_else(|| from.map(|(_, k)| of_kind(k))).unwrap_or(Kind::File);
        let record = match op {
            Did::Delete => Record { op: Op::Delete, kind, parent: None, name: None, files: BTreeMap::new() },
            Did::Move { to } => {
                let (parent, name) = place(to)?;
                Record { op: Op::Move, kind, parent, name: Some(name), files: BTreeMap::new() }
            }
            Did::Override { files: bases, to } => {
                let at = to.clone().or_else(|| from.map(|(p, _)| p.clone())).context("an override of nothing")?;
                let (parent, name) = match to {
                    Some(to) => place(to).map(|(p, n)| (p, Some(n)))?,
                    None => (None, None),
                };
                let now: BTreeMap<String, Option<Vec<u8>>> = written_at(&at, kind).into_iter().collect();
                let held = old.get(uid).filter(|r| r.op == Op::Override).map(|r| &r.files);
                Record { op: Op::Override, kind, parent, name, files: override_files(bases, now, held, &store)? }
            }
        };
        new.insert(uid.clone(), record);
    }
    if let Some(stray) = written.iter().find(|(at, bytes)| bytes.is_some() && !claimed.contains(*at)) {
        bail!("{}: no entry of {} is there to hold it", stray.0, layer.name);
    }
    let gone = old.keys().filter(|uid| !new.contains_key(*uid)).cloned().collect();
    new.retain(|uid, record| old.get(uid) != Some(record));
    Ok((new, gone))
}

/// An override's files in its record: each it holds as written now, else as it held it; each it removes, removed;
/// each with what it was made from.
fn override_files(
    bases: &BTreeMap<String, crate::changeset::Base>,
    mut now: BTreeMap<String, Option<Vec<u8>>>,
    held: Option<&BTreeMap<String, File>>,
    store: &crate::chunks::Store,
) -> Result<BTreeMap<String, File>> {
    let mut files = BTreeMap::new();
    for (file, base) in bases {
        let content = if base.removed {
            None
        } else {
            match now.remove(file) {
                Some(Some(bytes)) => Some(Content::of(&bytes, store)?),
                _ => held.and_then(|h| h.get(file)).and_then(|f| f.content.clone()),
            }
        };
        files.insert(file.clone(), File { content, was: base.was.clone() });
    }
    Ok(files)
}

/// Whether what a layer's folder holds by this name is of its tree of entries: not its records, nor anything hidden,
/// nor the extensions it holds (`ext::folder::HELD`), which are no entries: held as records of their own
/// (`hold_extensions`).
fn of_the_tree(name: &std::ffi::OsStr) -> bool {
    let name = name.to_string_lossy();
    !name.starts_with('.') && name != crate::ext::folder::HELD
}

/// What a project's index shows, for comparing: each entry by path - its kind, uid, and files by name and SHA-1.
fn shown(index: &crate::index::Index) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (path, entry) in index.entries.iter() {
        let mut said = format!("{:?} {}", entry.kind, entry.uid);
        match entry.kind {
            crate::index::Kind::Asset => {
                let asset = path.strip_suffix(crate::index::BUNDLE_SUFFIX).unwrap_or(path);
                for file in index.assets.get(asset).map_or_else(Vec::new, |a| a.files.clone()) {
                    let _ = write!(said, " {}={}", file.name, file.sha1);
                }
            }
            crate::index::Kind::File => {
                let _ = write!(said, " ={}", crate::content::sha1(&entry.disk).unwrap_or_default());
            }
            crate::index::Kind::Folder => {}
        }
        out.insert(path.clone(), said);
    }
    out
}

/// Store a layer stored as a tree as records: its records written, its tree and changeset set aside while the project
/// is read again and shows the same - each entry, its uid, its files and their hashes - then removed; where it shows
/// otherwise, the tree is put back and what differs is said. The extensions it holds as folders are held as records
/// (`hold_extensions`) - of a records layer, those alone. What was made.
///
/// # Errors
/// The layer is a records layer holding no extensions as folders, mounted or generated; what it holds cannot be read;
/// or the project shows otherwise once converted.
pub fn convert(project: &crate::Project, cache: &crate::cache::Cache, name: &str) -> Result<Converted> {
    let layer = project.layer(name)?;
    if is_records(&layer.path) {
        // What it still holds as folders, of what it held before records: its extensions.
        let extensions = hold_extensions(project, &layer.path)?;
        if extensions == 0 {
            bail!("{name} is stored as records already");
        }
        return Ok(Converted { records: 0, extensions });
    }
    if layer.mounted || layer.kind == crate::LayerKind::Generated {
        bail!("{name}: a mounted or generated layer stays a tree");
    }
    let (before, settings, records) = {
        let index = crate::index::Index::open(project, cache)?;
        let (settings, records) = of_tree(project, &index, layer)?;
        (shown(&index), settings, records)
    };
    // The tree aside, where nothing reads it: a dot-folder of the layer's.
    let aside = layer.path.join(".tree");
    std::fs::create_dir_all(&aside)?;
    let mut moved = Vec::new();
    for entry in std::fs::read_dir(&layer.path)?.flatten() {
        let file_name = entry.file_name();
        if !of_the_tree(&file_name) {
            continue;
        }
        std::fs::rename(entry.path(), aside.join(&file_name))?;
        moved.push(file_name);
    }
    let changeset_aside = aside.join(".changeset.toml");
    let had_changeset = layer.changeset.is_file();
    if had_changeset {
        std::fs::rename(&layer.changeset, &changeset_aside)?;
    }
    let put_back = || -> Result<()> {
        let _ = std::fs::remove_dir_all(layer.path.join(DIR));
        for file_name in &moved {
            std::fs::rename(aside.join(file_name), layer.path.join(file_name))?;
        }
        if had_changeset {
            std::fs::rename(&changeset_aside, &layer.changeset)?;
        }
        std::fs::remove_dir_all(&aside)?;
        Ok(())
    };
    let written = write_all(&layer.path, &settings, &records)
        .and_then(|()| Ok(shown(&crate::index::Index::open(project, cache)?)));
    let after = match written {
        Ok(after) => after,
        Err(e) => {
            put_back()?;
            Set::open(project)?;
            return Err(e.context(format!("{name} is left as it was")));
        }
    };
    if after != before {
        put_back()?;
        Set::open(project)?;
        let differ: Vec<String> = before
            .iter()
            .filter(|(path, said)| after.get(*path) != Some(*said))
            .map(|(path, said)| format!("{path}: {said} / {}", after.get(path).map_or("gone", String::as_str)))
            .chain(after.keys().filter(|p| !before.contains_key(*p)).map(|p| format!("{p}: new")))
            .take(10)
            .collect();
        bail!("{name} would show otherwise as records, and is left as it was:\n{}", differ.join("\n"));
    }
    std::fs::remove_dir_all(&aside)?;
    Ok(Converted { records: records.len(), extensions: hold_extensions(project, &layer.path)? })
}

/// What a conversion made: the layer's records, and the extensions it held as folders, held as records now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Converted {
    pub records: usize,
    pub extensions: usize,
}

#[cfg(test)]
mod tests {
    use super::{Content, File, Kind, Op, Record};
    use std::collections::BTreeMap;

    #[test]
    fn a_record_round_trips_with_its_text_as_written() {
        let text = |t: &str| Some(Content::Text(t.to_string()));
        let mut record = Record {
            op: Op::Override,
            kind: Kind::Node,
            parent: Some("uid://1izcb2mud6bit".into()),
            name: Some("box".into()),
            files: BTreeMap::default(),
        };
        for (name, content) in [
            ("node.toml", text("type = \"data\"\n[settings]\nsize = 3\n")),
            ("odd.txt", text("\nleading, ''' and \"\"\", tab\there\r\nunterminated")),
            ("native.texels", Some(Content::Stored { sha1: "9c1e".into(), size: 1_835_008 })),
            ("old.bin", None),
        ] {
            record.files.insert(name.into(), File { content, was: Some("3f2a".into()) });
        }
        let written = record.render();
        assert!(written.starts_with("format = 1\nop = \"override\"\n"), "{written}");
        assert!(
            written.contains("[files.\"node.toml\"]\nwas = \"3f2a\"\ntext = \"\"\"\ntype = \"data\"\n"),
            "{written}"
        );
        assert_eq!(Record::parse(&written).unwrap(), record);
    }

    #[test]
    fn a_record_of_another_format_is_refused() {
        let error = Record::parse("format = 2\nop = \"create\"\nkind = \"node\"\n").unwrap_err();
        assert!(error.to_string().contains("newer pe2"), "{error}");
        assert!(Record::parse("op = \"create\"\nkind = \"node\"\n").is_err());
    }

    #[test]
    fn a_record_is_placed_by_its_uid() {
        let layer = std::path::Path::new("/p/.layers/top");
        let at = super::path_of(layer, "uid://1hwweozpa40d9");
        assert_eq!(at, layer.join(".records/1h/1hwweozpa40d9.toml"));
        assert_eq!(super::uid_of(&at).as_deref(), Some("uid://1hwweozpa40d9"));
        assert_eq!(super::uid_of(&layer.join(".records").join(super::MARKER)), None);
        assert_eq!(super::uid_of(&layer.join("1h/1hwweozpa40d9.toml")), None);
    }
}
