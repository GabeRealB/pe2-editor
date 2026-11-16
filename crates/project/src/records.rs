//! A layer stored as records: one per entry the layer creates or acts on, by uid, at `<aa>/<uid>.toml` in the layer's
//! folder (`layers/<name>/`) - no tree of folders and files, which nobody edits by hand. A record says what the layer does
//! to its entry (creates it, overrides, moves or deletes it), where it is (its parent's uid and its name, so a rename or
//! a move of a folder holding thousands of entries is one record), and its files: text inline whatever its length, so
//! a layer's history reads as text in version control, anything else by its SHA-1 in the store
//! (`chunks::Store::put_file`). An override's file carries what it was made from (its `base`), or is removed.
//!
//! ```toml
//! format = 1
//! op = "override"
//! kind = "node"
//!
//! [files."node.toml"]
//! base = "3f2a…"
//! text = """
//! type = "data"
//! """
//!
//! [files."native.texels"]
//! sha1 = "9c1e…"
//! size = 1835008
//! ```
//!
//! A layer stored so says it in its folder (`layer.toml`: its format, and its settings - read only, the names it
//! ignores), and every record its format: a program meeting a format it does not know refuses it, and writes nothing.
//!
//! What reads a layer sees it as it would a tree of files: each records layer's files at the paths a layer's tree would
//! have them, under a root of its own on no disk (`<its folder>/tree`, `TREE`), its entries placed through the layers
//! below (`Set`), read through `content` as a packed layer's are.

use anyhow::{Context, Result, bail};
use pe2_path::{Name, RelPath, Uid};
use sfs::HostPath;
use std::sync::Arc;

use crate::TreePath;
use std::collections::{BTreeMap, BTreeSet};

/// The file marking a layer's folder as records: their format, and the layer's settings.
pub const MARKER: &Name = Name::constant("layer.toml");
/// Where a records layer's tree is addressed - what reads its files reads them at `<its folder>/tree/<path>`: a folder
/// of its own in the layer's, never made on disk, so that no folder of the tree meets one of its records' shards (two
/// letters) or its marker.
pub const TREE: &Name = Name::constant("tree");

/// The folder of a records layer's records, of the root its tree is addressed at (`TREE`): none for another path.
#[must_use]
pub fn shipped_of(tree: &HostPath) -> Option<HostPath> {
    (tree.file_name()? == TREE).then(|| tree.parent()).flatten()
}
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
    Stored { sha1: pe2_ext::Sha1, size: u64 },
}

/// A file of an entry, as a record holds it: its content, none where the layer removes it; and, for an override's,
/// the SHA-1 of the version below it was made from (its base).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    pub content: Option<Content>,
    pub base: Option<pe2_ext::Sha1>,
}

/// What a layer has of one entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub op: Op,
    pub kind: Kind,
    /// Where it is: its parent's uid (none at the top) and its name - for what the layer creates or moves.
    pub parent: Option<Uid>,
    pub name: Option<Arc<Name>>,
    /// Its files by name; a plain file's own by the root.
    pub files: BTreeMap<Arc<RelPath>, File>,
}

/// Where a layer keeps the extensions it holds, in its folder: a record an id, `extensions/<id>.toml`. They are no
/// entries - an extension is not in the tree of what the project holds - and are sealed: kept whole, as one tree of
/// whole objects in the store (`chunks::Store::put_folder`), never merged, a higher layer's in place of a lower one's,
/// and deleted to leave a lower one's off:
///
/// ```toml
/// format = 1
/// op = "create"
/// kind = "extension"
/// tree = "<the SHA-1 of its tree>"
/// ```
pub const EXTENSIONS: &Name = Name::constant("extensions");

/// An extension's id with what follows it in a file's name.
fn id_name(id: &str, suffix: &str) -> Result<Arc<Name>, pe2_path::PathError> {
    (Name::new(&format!("{id}{suffix}"))).map(ToOwned::to_owned)
}

/// Where a layer's record of the extension of an id is.
///
/// # Errors
/// The id makes no file's name on every system: no layer holds it.
pub fn extension_path(layer: &HostPath, id: &str) -> Result<HostPath, pe2_path::PathError> {
    Ok(layer.join(&EXTENSIONS).join(&id_name(id, ".toml")?))
}

/// The text of a layer's record of an extension: holding the tree named, or (none) leaving below's off.
#[must_use]
pub fn extension_record(tree: Option<&pe2_ext::Sha1>) -> String {
    let mut doc = toml_edit::DocumentMut::new();
    doc["format"] = toml_edit::value(FORMAT);
    doc["op"] = toml_edit::value(if tree.is_some() { "create" } else { "delete" });
    doc["kind"] = toml_edit::value("extension");
    if let Some(tree) = tree {
        doc["tree"] = toml_edit::value(tree.to_string());
    }
    doc.to_string()
}

/// The extensions a layer holds, by id: each its tree, none where the layer leaves below's off.
///
/// # Errors
/// A record cannot be read, or is of a format this program does not know.
pub fn extensions_of(fs: &dyn pe2_io::Filesystem, layer: &HostPath) -> Result<BTreeMap<String, Option<pe2_ext::Sha1>>> {
    let mut out = BTreeMap::new();
    let folder = layer.join(&EXTENSIONS);
    for file in fs.read_directory(&folder).unwrap_or_default() {
        let path = folder.entry(&file.name);
        let Some(id) = path.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".toml")) else {
            continue;
        };
        let text = fs.read_to_string(&path)?;
        out.insert(id.to_string(), extension_tree(&text).with_context(|| path.display().to_string())?);
    }
    Ok(out)
}

/// What a record of an extension says: the tree it holds, or (none) that it leaves below's off.
///
/// # Errors
/// It is no extension's record of a format this program knows.
pub fn extension_tree(text: &str) -> Result<Option<pe2_ext::Sha1>> {
    let table: toml::Table = toml::from_str(text)?;
    match table.get("format").and_then(toml::Value::as_integer) {
        Some(FORMAT) => {}
        Some(other) if other > FORMAT => bail!("of format {other}, written by a newer pe2"),
        other => bail!("not a record of format {FORMAT} ({other:?})"),
    }
    let word = |key: &str| table.get(key).and_then(toml::Value::as_str);
    if word("kind") != Some("extension") {
        bail!("not an extension's record");
    }
    match word("op") {
        Some("create") => Ok(Some(pe2_ext::Sha1::from_hex(word("tree").context("no tree")?).context("its tree")?)),
        Some("delete") => Ok(None),
        other => bail!("an extension is created or deleted, not {other:?}"),
    }
}

/// An extension a layer holds, as what reads its files sees it: they are at `<layer>/extensions/<id>.extension/<path>`
/// (`held_folder`), where it was a folder before it was a record, read from its tree in the store - a path that is on
/// no disk, read through `content` as a records layer's files are. The tree is the object: what is in it is named by
/// its path within it, never by its own hash.
#[derive(Clone, Debug)]
pub struct HeldExtension {
    pub layer: HostPath,
    pub id: String,
    /// Its tree's hash.
    pub tree: pe2_ext::Sha1,
    pub files: std::sync::Arc<crate::chunks::Tree>,
    pub store: crate::chunks::Store,
    /// When its record was written.
    pub modified: std::time::SystemTime,
}

/// Where the files of the extension of an id a layer holds are read: its folder, as a path on no disk.
///
/// # Errors
/// The id makes no file's name on every system: no layer holds it.
pub fn held_folder(layer: &HostPath, id: &str) -> Result<HostPath, pe2_path::PathError> {
    Ok(layer.join(&crate::ext::folder::HELD).join(&id_name(id, crate::ext::folder::SUFFIX)?))
}

/// The trees of the extensions read, by their records' files and their stamps: a tree is read once.
type HeldRead =
    std::collections::HashMap<HostPath, (std::time::SystemTime, u64, Option<std::sync::Arc<HeldExtension>>)>;
static HELD_TREES: std::sync::RwLock<Option<HeldRead>> = std::sync::RwLock::new(None);

/// The extension a layer holds at a folder (`held_folder`): none where the folder is no held extension's, or the layer
/// leaves it off.
#[must_use]
pub fn held(io: &Arc<dyn pe2_io::Io>, folder: &HostPath) -> Option<std::sync::Arc<HeldExtension>> {
    let id = crate::ext::folder::id_of(folder)?;
    let parent = folder.parent()?;
    if parent.file_name()? != crate::ext::folder::HELD {
        return None;
    }
    let layer = parent.parent()?;
    let folder = shipped_of(&layer)?;
    // Its id is a folder's name: the name of its record too.
    let record = extension_path(&folder, id).ok()?;
    let stat = io.filesystem().stat(&record).ok()?;
    let stamp = (stat.modified?, stat.size);
    if let Some(found) =
        HELD_TREES.read().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref().and_then(|h| h.get(&record))
        && (found.0, found.1) == stamp
    {
        return found.2.clone();
    }
    let read = || -> Option<std::sync::Arc<HeldExtension>> {
        let tree = extensions_of(io.filesystem(), &folder).ok()?.remove(id)??;
        // Its tree is the layer's store's, or - until a sweep shares it again - this machine's.
        let (store, files) = crate::chunks::Store::of(&layer, io)?.holding_tree(&tree)?;
        let files = std::sync::Arc::new(files);
        Some(std::sync::Arc::new(HeldExtension {
            layer: layer.clone(),
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
pub fn held_at(io: &Arc<dyn pe2_io::Io>, path: &HostPath) -> Option<(std::sync::Arc<HeldExtension>, String)> {
    // Its folder is the nearest ancestor named `<id>.extension` in a layer's `extensions/`.
    let folder = path.ancestors().find(|a| {
        crate::ext::folder::id_of(a).is_some()
            && a.parent().is_some_and(|layer| layer.file_name().is_some_and(|name| name == crate::ext::folder::HELD))
    })?;
    let held = held(io, &folder)?;
    // Its files are its tree's, held by names: a place within by one no path takes is none of them.
    let within = path.within(&folder).ok()?.as_str().to_owned();
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
pub fn hold_extensions(project: &crate::Project, layer: &HostPath) -> Result<usize> {
    let fs = project.io.filesystem();
    let held = layer.join(&crate::ext::folder::HELD);
    let store = crate::chunks::Store::at(&project.root, project.io.clone());
    let mut count = 0;
    for entry in fs.read_directory(&held).unwrap_or_default() {
        let folder = held.entry(&entry.name);
        let Some(id) = crate::ext::folder::id_of(&folder).map(str::to_string) else { continue };
        if entry.kind != pe2_io::Kind::Folder {
            continue;
        }
        let (tree, _) = store.put_folder(&folder)?;
        let at = extension_path(layer, &id)?;
        fs.create_folder_all(&layer.join(&EXTENSIONS))?;
        crate::local::write_atomic(fs, &at, extension_record(Some(&tree)).as_bytes())?;
        project.io.filesystem().remove_all(&folder)?;
        count += 1;
    }
    if pe2_io::is_empty_folder(fs, &held) {
        project.io.filesystem().remove_folder(&held)?;
    }
    Ok(count)
}

/// Whether a folder's name is a shard's, as a uid's digits make them (`RelPath::sharded`): their first two, or all
/// of them where there are fewer.
#[must_use]
pub fn is_shard(name: &std::ffi::OsStr) -> bool {
    (1..=2).contains(&name.len())
}

/// Where a layer's record of an entry is, in its folder.
///
/// # Errors
/// The uid's digits make no file's name on every system (`con`, as Windows reserves it).
pub fn path_of(layer: &HostPath, uid: Uid) -> Result<HostPath, pe2_path::PathError> {
    Ok(layer.join(&RelPath::sharded(&uid.digits(), ".toml")?))
}

/// The uid a record's file in a folder of records is of: none for a file that is no record of the folder's.
#[must_use]
pub fn uid_of(folder: &HostPath, path: &HostPath) -> Option<Uid> {
    let key = path.file_name()?.to_str()?.strip_suffix(".toml")?;
    let shard = path.parent()?;
    // Entries' records are sharded by the first two characters of their uid's digits (the digits, where there are
    // fewer); the extensions' and the marker are elsewhere.
    let in_records = shard.file_name().is_some_and(|n| n.to_str() == Some(key.get(..2).unwrap_or(key)))
        && shard.parent().is_some_and(|records| records == *folder);
    if !in_records || key.is_empty() {
        return None;
    }
    Uid::new(&format!("uid://{key}")).ok()
}

/// Whether a layer's folder is stored as records.
#[must_use]
pub fn is_records(fs: &dyn pe2_io::Filesystem, layer: &HostPath) -> bool {
    fs.is_file(&layer.join(&MARKER))
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
pub fn mark(fs: &dyn pe2_io::Filesystem, layer: &HostPath, settings: &Settings) -> Result<()> {
    fs.create_folder_all(layer)?;
    let at = layer.join(&MARKER);
    crate::local::write_atomic(fs, &at, marker(settings).as_bytes())
}

/// The text of a layer's marker: the format of its records, and its settings.
pub(crate) fn marker(settings: &Settings) -> String {
    let mut doc = toml_edit::DocumentMut::new();
    doc["format"] = toml_edit::value(FORMAT);
    if settings.read_only {
        doc["read_only"] = toml_edit::value(true);
    }
    if !settings.ignore.is_empty() {
        doc["ignore"] = toml_edit::value(settings.ignore.iter().map(String::as_str).collect::<toml_edit::Array>());
    }
    doc.to_string()
}

/// A records layer's settings, its format checked: one this program knows.
///
/// # Errors
/// Another format, or no marker one can read.
pub fn settings(fs: &dyn pe2_io::Filesystem, layer: &HostPath) -> Result<Settings> {
    let at = layer.join(&MARKER);
    let text = fs.read_to_string(&at).with_context(|| at.display().to_string())?;
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
    pub fn sha1(&self) -> pe2_ext::Sha1 {
        match self {
            Content::Text(text) => crate::extract::sha1_of(text.as_bytes()),
            Content::Stored { sha1, .. } => *sha1,
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
        // Where it is, checked as it is read: a parent that is no uid, a name that is none, is no record.
        let parent = string("parent").map(|text| Uid::new(&text)).transpose().context("its parent")?;
        let name =
            string("name").map(|text| Name::new(&text).map(ToOwned::to_owned)).transpose().context("its name")?;
        let mut files = BTreeMap::new();
        for (name, file) in table.get("files").and_then(toml::Value::as_table).into_iter().flatten() {
            let file = file.as_table().with_context(|| format!("{name}: not a table"))?;
            let field = |key: &str| file.get(key).and_then(toml::Value::as_str).map(str::to_string);
            let content = match (field("text"), field("sha1")) {
                (Some(text), None) => Some(Content::Text(text)),
                (None, Some(sha1)) => {
                    let size = file.get("size").and_then(toml::Value::as_integer).context("a stored file's size")?;
                    let sha1 = pe2_ext::Sha1::from_hex(&sha1).with_context(|| format!("{name}: its sha1"))?;
                    Some(Content::Stored { sha1, size: u64::try_from(size)? })
                }
                (None, None) if file.get("removed").and_then(toml::Value::as_bool) == Some(true) => None,
                _ => bail!("{name}: neither text, a stored file nor removed"),
            };
            let at = RelPath::new(name).with_context(|| format!("its file {name}"))?.to_owned();
            // Names compare as a system that does not tell case or form apart takes them: two alike are one file.
            if let Some((other, _)) = files.get_key_value(&*at) {
                bail!("its files {other} and {name} are one name to a system that does not tell case or form apart");
            }
            let base = field("base").map(|base| pe2_ext::Sha1::from_hex(&base)).transpose();
            files.insert(at, File { content, base: base.with_context(|| format!("{name}: its base"))? });
        }
        Ok(Record { op, kind, parent, name, files })
    }

    /// Its text: its format first, what it does and where, then its files by name.
    #[must_use]
    pub fn render(&self) -> String {
        let mut doc = toml_edit::DocumentMut::new();
        doc["format"] = toml_edit::value(FORMAT);
        doc["op"] = toml_edit::value(self.op.name());
        doc["kind"] = toml_edit::value(self.kind.name());
        if let Some(parent) = &self.parent {
            doc["parent"] = toml_edit::value(parent.to_string());
        }
        if let Some(name) = &self.name {
            doc["name"] = toml_edit::value(name.as_str());
        }
        if !self.files.is_empty() {
            let mut files = toml_edit::Table::new();
            files.set_implicit(true);
            for (name, file) in &self.files {
                let mut table = toml_edit::Table::new();
                if let Some(base) = &file.base {
                    table["base"] = toml_edit::value(base.to_string());
                }
                match &file.content {
                    Some(Content::Text(text)) => table["text"] = toml_edit::value(text.as_str()),
                    Some(Content::Stored { sha1, size }) => {
                        table["sha1"] = toml_edit::value(sha1.to_string());
                        table["size"] = toml_edit::value(i64::try_from(*size).unwrap_or(i64::MAX));
                    }
                    None => table["removed"] = toml_edit::value(true),
                }
                files.insert(name.as_str(), toml_edit::Item::Table(table));
            }
            doc["files"] = toml_edit::Item::Table(files);
        }
        doc.to_string()
    }
}

/// A records layer as what reads it sees it: its records by uid, each with its file's time, and the tree of files they
/// make at the paths a layer's tree would have them.
struct View {
    /// Where its tree is addressed (`TREE`).
    dir: HostPath,
    /// Where its records are.
    folder: HostPath,
    /// Where what they keep by hash is: the project's store, or - for records this machine's alone - its local one.
    store: crate::chunks::Store,
    /// A mount's: its records list its tree - each entry's uid, where it is - and hold none of its files, which are read
    /// where the tree is, never through the records.
    external: bool,
    settings: Settings,
    records: Read,
    /// Each file by its path in the layer: its entry's uid and its name in the entry (the root for a plain file).
    files: BTreeMap<Arc<TreePath>, (Uid, Arc<RelPath>)>,
    folders: std::collections::BTreeSet<Arc<TreePath>>,
    /// The nodes it shows, by their paths (`<name>.node`).
    nodes: BTreeSet<Arc<TreePath>>,
    /// Where each of its records' entries is.
    placed: std::collections::HashMap<Uid, Arc<TreePath>>,
    /// Where the entries its records name are - those it makes and those it moves: what the layers above place
    /// theirs through. Shared with each reading that takes this view as it is.
    named: std::sync::Arc<std::collections::HashMap<Uid, Arc<TreePath>>>,
    /// What each folder holds, by its path (the root, the layer's top): each name, and whether it is a folder.
    children: std::collections::HashMap<Arc<TreePath>, Vec<(Arc<Name>, bool)>>,
    /// What is wrong with its records: those with no place.
    problems: Vec<String>,
}

impl View {
    /// What a record shows in the tree, placed at `at`: it, its files and the folders they are in.
    fn shows(&mut self, uid: Uid, at: &Arc<TreePath>, record: &Record) {
        let mut folder = at.parent();
        while let Some(f) = folder.filter(|f| !f.is_root()) {
            folder = f.parent();
            self.folders.insert(f.to_owned());
        }
        match record.kind {
            Kind::Folder => {
                self.folders.insert(at.clone());
            }
            Kind::File => {
                if record.files.get(RelPath::root()).is_some_and(|f| f.content.is_some()) {
                    self.files.insert(at.clone(), (uid, RelPath::root().to_owned()));
                }
            }
            Kind::Node => {
                self.folders.insert(at.clone());
                self.nodes.insert(at.clone());
                for (name, file) in &record.files {
                    if file.content.is_some() {
                        self.files.insert(at.join_path(name), (uid, name.clone()));
                        let mut folder = name.parent();
                        while let Some(f) = folder.filter(|f| !f.is_root()) {
                            self.folders.insert(at.join_path(f));
                            folder = f.parent();
                        }
                    }
                }
            }
        }
    }

    /// What a records layer whose tree is at `dir` and records in `folder` is seen as before its records are read.
    fn new(dir: HostPath, folder: HostPath, store: crate::chunks::Store) -> View {
        View {
            dir,
            folder,
            store,
            external: false,
            settings: Settings::default(),
            records: Read::default(),
            files: BTreeMap::new(),
            folders: std::collections::BTreeSet::new(),
            nodes: BTreeSet::new(),
            placed: std::collections::HashMap::new(),
            named: std::sync::Arc::default(),
            children: std::collections::HashMap::new(),
            problems: Vec::new(),
        }
    }
}

impl View {
    /// What each folder holds, from the files and folders.
    fn list_children(&mut self) {
        let mut children: std::collections::HashMap<Arc<TreePath>, Vec<(Arc<Name>, bool)>> =
            std::collections::HashMap::new();
        let entries =
            self.folders.iter().map(|folder| (folder, true)).chain(self.files.keys().map(|file| (file, false)));
        for (path, folder) in entries {
            if let (Some(parent), Some(name)) = (path.parent(), path.last()) {
                children.entry(parent.to_owned()).or_default().push((name.to_owned(), folder));
            }
        }
        self.children = children;
    }
}

/// What a records layer shows (`Set::shown`).
pub struct Shown {
    pub nodes: BTreeSet<Arc<TreePath>>,
    pub folders: Vec<Arc<TreePath>>,
    pub files: Vec<Arc<TreePath>>,
}

/// What is known of a project's records layers, for a set to open over (`Set::open_over`): each layer's records by
/// uid, each with its file's time and length, by the layer's folder.
pub type Known = std::collections::HashMap<HostPath, BTreeMap<Uid, Recorded>>;
/// A record as it was read, with its file's time and length.
pub type Recorded = (std::sync::Arc<Record>, std::time::SystemTime, u64);

/// What differs between two readings of a project's records layers (`Set::differences`).
#[derive(Clone, Debug, Default)]
pub struct Differences {
    /// The records that came, went, changed, or whose file is of another time or length, by layer (its folder)
    /// and uid: what the project's model is told of.
    pub records: Vec<(HostPath, Uid)>,
    /// For each layer (by folder), where the entries whose records came, went or changed were, and are.
    pub paths: Vec<(HostPath, BTreeSet<Arc<TreePath>>)>,
    /// Any of them is placed otherwise.
    pub placed: bool,
    /// Any of them is (or was) an override: what it merges is merged again.
    pub overrides: bool,
    /// The layers (by folder) whose records are as they were, taken as read before: nothing of them is to be
    /// derived again.
    pub same: BTreeSet<HostPath>,
}

/// A record read, with the stamp of its file: what a reading again finds unchanged it takes as it was.
#[derive(Clone)]
pub(crate) struct Held {
    pub(crate) record: std::sync::Arc<Record>,
    modified: std::time::SystemTime,
    len: u64,
}

/// A layer's records read, by uid.
pub(crate) type Read = BTreeMap<Uid, Held>;

/// A project's records layers, as seen through its stack: what `content` and a scan read them by. One a project, made
/// again as its records or its changesets change (`Set::open`).
pub struct Set {
    root: HostPath,
    store: crate::chunks::Store,
    views: Vec<std::sync::Arc<View>>,
    /// Records whose place cannot be found, by what: their parent is nowhere below.
    pub problems: Vec<String>,
}

static SETS: std::sync::RwLock<Vec<std::sync::Arc<Set>>> = std::sync::RwLock::new(Vec::new());

/// The records placing entries at one name - alike but for case or form, which not every system tells apart - each
/// said, in order, as a problem of the layer's: to be left out, all of them, whichever was written first.
fn placed_alike(
    records: &Read,
    located: &std::collections::HashMap<Uid, Arc<TreePath>>,
    layer: &Name,
    problems: &mut Vec<String>,
) -> std::collections::HashSet<Uid> {
    let mut placing: std::collections::HashMap<&TreePath, Vec<Uid>> = std::collections::HashMap::new();
    for (uid, Held { record, .. }) in records {
        if let (Some(_), Some(at)) = (&record.name, located.get(uid)) {
            placing.entry(&**at).or_default().push(*uid);
        }
    }
    let (mut alike, mut said) = (std::collections::HashSet::new(), Vec::new());
    for group in placing.values().filter(|group| group.len() > 1) {
        for uid in group {
            let others: Vec<String> = group.iter().filter(|other| *other != uid).map(ToString::to_string).collect();
            let at = &located[uid];
            let others = others.join(" and ");
            said.push(format!(
                "{layer}: {uid} at {at} is placed at one name with {others} to a system that does not tell case or form \
                 apart"
            ));
            alike.insert(*uid);
        }
    }
    said.sort();
    problems.extend(said);
    alike
}

/// The path of an entry named so in a folder: a node's with `.node`.
fn joined(parent: Option<&TreePath>, name: &Name, kind: Kind) -> Arc<TreePath> {
    let at = parent.unwrap_or_default().join(name);
    if kind == Kind::Node { crate::index::bundle_named(&at) } else { at }
}

/// Where the layers below one place the entries they name, each layer's own map, bottom to top: asked from the top,
/// the highest that names an entry says where it is. Nothing of them is copied to ask.
#[derive(Default)]
struct Below {
    layers: Vec<std::sync::Arc<std::collections::HashMap<Uid, Arc<TreePath>>>>,
}

impl Below {
    fn get(&self, uid: Uid) -> Option<&TreePath> {
        self.layers.iter().rev().find_map(|layer| layer.get(&uid)).map(|path| &**path)
    }
}

/// Where a record's entry is - through its parent, in its layer or below, or where it is below - remembered in
/// `placed`; none where its parent is nowhere.
fn place(
    uid: Uid,
    records: &Read,
    below: &Below,
    placed: &mut std::collections::HashMap<Uid, Arc<TreePath>>,
    depth: usize,
) -> Option<Arc<TreePath>> {
    if let Some(at) = placed.get(&uid) {
        return Some(at.clone());
    }
    let at = match records.get(&uid) {
        Some(Held { record, .. }) if depth < 256 && record.name.is_some() => {
            let parent = match record.parent {
                Some(parent) => Some(place(parent, records, below, placed, depth + 1)?),
                None => None,
            };
            joined(parent.as_deref(), record.name.as_ref()?, record.kind)
        }
        _ => below.get(uid)?.to_owned(),
    };
    placed.insert(uid, at.clone());
    Some(at)
}

/// A records layer's records after these files changed, from what was read of it before: each record among them
/// read again, or gone with its file - the others neither listed nor looked at. None where that cannot be told from
/// the paths: a folder of records is among them, or anything of the layer's that is no record.
fn read_changed(
    fs: &dyn pe2_io::Filesystem,
    dir: &HostPath,
    before: &Read,
    changed: &[HostPath],
) -> Option<Result<Read>> {
    let ours: Vec<&HostPath> = changed.iter().filter(|path| path.starts_with(dir)).collect();
    let records: Option<Vec<(Uid, &HostPath)>> = ours
        .iter()
        .map(|path| {
            // A record is `<shard>/<uid>.toml`, its shard a folder of the records' own named by two letters -
            // what is beside the shards (the extensions the layer holds, its settings) is no record of an entry.
            let shard = path.parent().filter(|shard| shard.file_name().is_some_and(is_shard));
            let in_shard = shard.and_then(|shard| shard.parent()).is_some_and(|records| records == *dir);
            Some((uid_of(dir, path).filter(|_| in_shard)?, *path))
        })
        .collect();
    let records = records?;
    Some((|| {
        let mut read = before.clone();
        for (uid, path) in records {
            // One that waits to be written is as that leaves it, whatever the disk has yet.
            match crate::storage::waiting_since(path) {
                Some((at, Some(bytes))) => {
                    let text = String::from_utf8(bytes).with_context(|| path.display().to_string())?;
                    waits(&mut read, uid, &text, at).with_context(|| path.display().to_string())?;
                    continue;
                }
                Some((_, None)) => {
                    read.remove(&uid);
                    continue;
                }
                None => {}
            }
            match fs.stat(path) {
                Ok(stat) => {
                    let (modified, len) = (stat.modified.ok_or_else(|| no_time(path))?, stat.size);
                    if read.get(&uid).is_some_and(|held| held.modified == modified && held.len == len) {
                        continue;
                    }
                    let text = fs.read_to_string(path)?;
                    let record = Record::parse(&text).with_context(|| path.display().to_string())?;
                    // As it was read before - what was written behind, read again once it is written - it is the
                    // same record to whoever compares the two readings: only its file's stamp is new.
                    let record = match read.get(&uid) {
                        Some(held) if *held.record == record => held.record.clone(),
                        _ => std::sync::Arc::new(record),
                    };
                    read.insert(uid, Held { record, modified, len });
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => drop(read.remove(&uid)),
                Err(e) => return Err(e).with_context(|| path.display().to_string()),
            }
        }
        Ok(read)
    })())
}

/// A record that waits to be written, among those read: as its text is, stamped as its file will be once written -
/// its length, and the time it was said, which the file is given - so that the file written is the one read here.
fn waits(read: &mut Read, uid: Uid, text: &str, at: std::time::SystemTime) -> Result<()> {
    let record = Record::parse(text)?;
    let record = match read.get(&uid) {
        Some(held) if *held.record == record => held.record.clone(),
        _ => std::sync::Arc::new(record),
    };
    read.insert(uid, Held { record, modified: at, len: text.len() as u64 });
    Ok(())
}

/// A records layer's records, read on every core: those whose file is as it was when `before` read it taken from it.
/// What waits to be written of them is as that leaves it.
/// What is said where the system gives no time of a record's file.
fn no_time(path: &HostPath) -> std::io::Error {
    std::io::Error::other(format!("{}: the system gives no time of it", path.display()))
}

pub(crate) fn read_records(fs: &dyn pe2_io::Filesystem, dir: &HostPath, before: Option<&Read>) -> Result<Read> {
    // What waits is asked first: a record not waiting by now is in its file already, as it was last said - where
    // one read from its file first, and found waiting no more after, would be missed in both.
    let waiting = crate::storage::waiting_under(dir);
    let records_dir = dir;
    let shards: Vec<HostPath> = fs
        .read_directory(records_dir)
        .unwrap_or_default()
        .iter()
        .filter(|shard| shard.kind == pe2_io::Kind::Folder && is_shard(&shard.name))
        .map(|shard| records_dir.entry(&shard.name))
        .collect();
    let read_shard = |shard: &HostPath| -> Result<Vec<(Uid, Held)>> {
        let mut out = Vec::new();
        for file in fs.read_directory(shard)? {
            let path = shard.entry(&file.name);
            let Some(uid) = uid_of(dir, &path) else { continue };
            let stat = fs.stat_link(&path)?;
            let (modified, len) = (stat.modified.ok_or_else(|| no_time(&path))?, stat.size);
            if let Some(held) = before.and_then(|b| b.get(&uid)).filter(|h| h.modified == modified && h.len == len) {
                out.push((uid, held.clone()));
                continue;
            }
            let text = fs.read_to_string(&path)?;
            let record = Record::parse(&text).with_context(|| path.display().to_string())?;
            out.push((uid, Held { record: std::sync::Arc::new(record), modified, len }));
        }
        Ok(out)
    };
    let threads = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    let parts: Vec<Result<Vec<(Uid, Held)>>> = std::thread::scope(|scope| {
        let workers: Vec<_> = shards
            .chunks(shards.len().div_ceil(threads).max(1))
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
    for (path, at, bytes) in waiting {
        let Some(uid) = uid_of(dir, &path) else { continue };
        match bytes {
            Some(bytes) => {
                let text = String::from_utf8(bytes).with_context(|| path.display().to_string())?;
                waits(&mut out, uid, &text, at).with_context(|| path.display().to_string())?;
            }
            None => drop(out.remove(&uid)),
        }
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
        Set::opened(project, None, None)
    }

    /// Read a project's records layers as `open` does, over what is known of them (the project's model's,
    /// `model::known`): a record whose file is as it was known is not read again - or, where what is known is
    /// `taken` as it is, no record's file is looked at, and whoever opens so holds the files to it after
    /// (`differing`).
    ///
    /// # Errors
    /// As `open`.
    pub fn open_over(project: &crate::Project, known: Known, taken: bool) -> Result<std::sync::Arc<Set>> {
        Set::opened(project, None, Some((known, taken)))
    }

    /// Read a project's records layers again after these files changed, as `open` does - but a layer none of them is
    /// a record of, with nothing changed below it, is taken as it was read before: its records neither looked at nor
    /// placed again. What an edit costs then follows what it changed, not what the project holds.
    ///
    /// # Errors
    /// As `open`.
    pub fn open_after(project: &crate::Project, changed: &[HostPath]) -> Result<std::sync::Arc<Set>> {
        Set::opened(project, Some(changed), None)
    }

    fn opened(
        project: &crate::Project,
        changed: Option<&[HostPath]>,
        known: Option<(Known, bool)>,
    ) -> Result<std::sync::Arc<Set>> {
        let (mut known, taken) = known.unwrap_or_default();
        let _span = tracing::info_span!("records open").entered();
        // What was read of it before: a record whose file is as it was is not read again.
        let previous = Set::current(&project.root);
        let fs = project.io.filesystem();
        let mut places = Below::default();
        let mut views: Vec<std::sync::Arc<View>> = Vec::new();
        // Whether anything below the layer at hand may place its entries otherwise than before: all of it read anew,
        // or a layer below changed.
        let mut below_changed = changed.is_none();
        let touched = |within: &HostPath| changed.is_none_or(|paths| paths.iter().any(|path| path.starts_with(within)));
        for layer in &project.layers {
            let folder = &layer.records;
            let before = previous.as_ref().and_then(|p| p.views.iter().find(|v| v.dir == layer.path));
            if let Some(before) = before.filter(|_| !below_changed && !touched(folder)) {
                // As it was read: what it places is there for the layers above, as it was.
                places.layers.push(before.named.clone());
                views.push(before.clone());
                continue;
            }
            below_changed = true;
            // A layer whose records are none yet - a generated one before its plan first ran - holds nothing.
            let settings = if fs.exists(folder) { settings(fs, folder)? } else { Settings::default() };
            // The records that changed read again, where the paths say which; else all of them looked at.
            let after = before.zip(changed).and_then(|(view, paths)| read_changed(fs, folder, &view.records, paths));
            // What is known of the layer, as records read: what was read here before, where nothing else is.
            let known: Option<Read> = known.remove(&layer.path).map(|known| {
                let held =
                    known.into_iter().map(|(uid, (record, modified, len))| (uid, Held { record, modified, len }));
                held.collect()
            });
            let records = match (after, known) {
                (Some(records), _) => records?,
                // As known, each: no file is looked at.
                (None, Some(known)) if taken => known,
                (None, Some(known)) => read_records(fs, folder, Some(&known))?,
                (None, None) => read_records(fs, folder, before.map(|v| &v.records))?,
            };
            // This machine's alone, where its records are in the local folder: what they keep is in the local store.
            let store = if folder.starts_with(&crate::local::part(&project.root, crate::local::LAYERS)) {
                crate::chunks::Store::local(&project.root, project.io.clone())
            } else {
                crate::chunks::Store::at(&project.root, project.io.clone())
            };
            let mut view =
                View { settings, external: layer.mounted, ..View::new(layer.path.clone(), folder.clone(), store) };
            let mut named = std::collections::HashMap::new();
            // Each record's place (`place`).
            let mut located: std::collections::HashMap<Uid, Arc<TreePath>> = std::collections::HashMap::new();
            for uid in records.keys() {
                if place(*uid, &records, &places, &mut located, 0).is_none() {
                    view.problems.push(format!("{}: {uid} has no place: what it is in is nowhere below", layer.name));
                }
            }
            let alike = placed_alike(&records, &located, &layer.name, &mut view.problems);
            for (uid, Held { record, .. }) in records.iter().filter(|(uid, _)| !alike.contains(*uid)) {
                let Some(at) = located.get(uid) else { continue };
                // What it places (creates, moves) is there for the layers above.
                if record.name.is_some() {
                    named.insert(*uid, at.clone());
                }
                if record.op == Op::Delete || record.op == Op::Move || view.external {
                    continue;
                }
                view.shows(*uid, at, record);
            }
            view.records = records;
            view.placed = located;
            view.named = std::sync::Arc::new(named);
            places.layers.push(view.named.clone());
            view.list_children();
            views.push(std::sync::Arc::new(view));
        }
        let problems = views.iter().flat_map(|view| view.problems.iter().cloned()).collect();
        let set = std::sync::Arc::new(Set {
            root: project.root.clone(),
            store: crate::chunks::Store::at(&project.root, project.io.clone()),
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
    pub fn current(root: &HostPath) -> Option<std::sync::Arc<Set>> {
        let sets = SETS.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        sets.iter().find(|s| s.root == *root).cloned()
    }

    /// What differs between two readings of a project's records layers: for each layer (by folder), the paths of the
    /// entries whose records came, went or changed - where each was and where each is - and whether any of them is
    /// placed otherwise (moved, renamed, made or deleted), which moves what is placed through it, in higher layers too.
    #[must_use]
    pub fn differences(before: Option<&Set>, now: &Set) -> Differences {
        let mut out = Vec::new();
        let mut records = Vec::new();
        let (mut placed, mut overrides) = (false, false);
        let mut same = BTreeSet::new();
        for view in &now.views {
            let old = before.and_then(|b| b.views.iter().find(|v| v.dir == view.dir));
            // Taken as it was read before: nothing of it differs.
            if old.is_some_and(|old| std::sync::Arc::ptr_eq(old, view)) {
                same.insert(view.dir.clone());
                continue;
            }
            let empty = BTreeMap::new();
            let old_records = old.map_or(&empty, |o| &o.records);
            let mut paths = BTreeSet::new();
            let uids: BTreeSet<&Uid> = view.records.keys().chain(old_records.keys()).collect();
            for uid in uids {
                let (was, is) = (old_records.get(uid), view.records.get(uid));
                if let (Some(was), Some(is)) = (was, is)
                    && std::sync::Arc::ptr_eq(&was.record, &is.record)
                {
                    // The same record, in a file of another time or length: told to the model, and its entry's files -
                    // whose times are its file's - scanned again; placed as it was.
                    if was.modified != is.modified || was.len != is.len {
                        records.push((view.dir.clone(), *uid));
                        paths.extend(view.placed.get(uid).cloned());
                    }
                    continue;
                }
                records.push((view.dir.clone(), *uid));
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
        Differences { records, paths: out, placed, overrides, same }
    }

    /// A layer's record as it was read, by the layer's folder and its uid.
    #[must_use]
    pub fn recorded(&self, layer: &HostPath, uid: Uid) -> Option<Recorded> {
        let view = self.views.iter().find(|view| view.dir == *layer)?;
        view.records.get(&uid).map(|held| (held.record.clone(), held.modified, held.len))
    }

    /// Every record of a layer as it was read, by the layer's folder; none where it is no records layer.
    #[must_use]
    pub fn records_read(&self, layer: &HostPath) -> Option<BTreeMap<Uid, Recorded>> {
        let view = self.views.iter().find(|view| view.dir == *layer)?;
        Some(view.records.iter().map(|(uid, held)| (*uid, (held.record.clone(), held.modified, held.len))).collect())
    }

    /// The records' files that are not as they were read - of another time or length, gone, or there with nothing
    /// read of them: what a set opened over what was known, with no file looked at (`open_over`), is held to, on
    /// a thread of its own. Taken in as a change of those files, each is read again.
    #[must_use]
    pub fn differing(&self) -> Vec<HostPath> {
        let mut differing = Vec::new();
        for view in &self.views {
            let mut found = BTreeSet::new();
            let records_dir = &view.folder;
            let fs = self.store.io.filesystem();
            let shards = fs.read_directory(records_dir).unwrap_or_default();
            for shard in shards.iter().filter(|shard| shard.kind == pe2_io::Kind::Folder && is_shard(&shard.name)) {
                let shard = records_dir.entry(&shard.name);
                for file in fs.read_directory(&shard).unwrap_or_default() {
                    let path = shard.entry(&file.name);
                    let Some(uid) = uid_of(records_dir, &path) else { continue };
                    let stamp = fs.stat_link(&path).ok().and_then(|stat| Some((stat.modified?, stat.size)));
                    let same = view
                        .records
                        .get(&uid)
                        .zip(stamp)
                        .is_some_and(|(held, (modified, len))| held.modified == modified && held.len == len);
                    if !same {
                        differing.push(path);
                    }
                    found.insert(uid);
                }
            }
            // A record read is of a uid whose file can be named: written so, read back so.
            let gone = view.records.keys().filter(|uid| !found.contains(*uid));
            differing.extend(gone.filter_map(|uid| path_of(&view.folder, *uid).ok()));
        }
        differing
    }

    /// What the records layer whose tree is addressed at `dir` shows, as a scan takes it whole rather than walking it:
    /// its nodes, its folders (a node's among them, and those inside one) and its files (a node's among them), each by
    /// its path in the layer. None where `dir` is no records layer's.
    #[must_use]
    pub fn shown(dir: &HostPath) -> Option<Shown> {
        let (set, i) = Set::view_at(dir)?;
        let view = &set.views[i];
        if view.external {
            return None;
        }
        Some(Shown {
            nodes: view.nodes.clone(),
            folders: view.folders.iter().cloned().collect(),
            files: view.files.keys().cloned().collect(),
        })
    }

    /// The records layer whose tree is addressed at `dir`.
    fn view_at(dir: &HostPath) -> Option<(std::sync::Arc<Set>, usize)> {
        let sets = SETS.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        sets.iter().find_map(|set| Some((set.clone(), set.views.iter().position(|view| view.dir == *dir)?)))
    }

    /// The records layer a path is in, and the path within it.
    fn view_of(path: &HostPath) -> Option<(std::sync::Arc<Set>, usize, Arc<TreePath>)> {
        let sets = SETS.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        for set in sets.iter() {
            for (i, view) in set.views.iter().enumerate().filter(|(_, view)| !view.external) {
                match path.within(&view.dir) {
                    Ok(rel) => return Some((set.clone(), i, TreePath::shared(rel))),
                    Err(sfs::NotWithin::Outside) => {}
                    // In the layer by a name no path takes: none of its records' files, as the index's scan says.
                    Err(sfs::NotWithin::Refused(_)) => return None,
                }
            }
        }
        None
    }

    /// A file of a records layer by its path: its content, and when its record was written.
    #[must_use]
    pub fn file(path: &HostPath) -> Option<(Content, std::time::SystemTime, crate::chunks::Store)> {
        let (set, i, rel) = Set::view_of(path)?;
        let view = &set.views[i];
        let (uid, name) = view.files.get(&rel)?;
        let held = view.records.get(uid)?;
        Some((held.record.files.get(name)?.content.clone()?, held.modified, view.store.clone()))
    }

    /// Where the content of a file of a records layer lives, for what serves it to read it without the records: the
    /// record holding its text, or the store keeping it.
    #[must_use]
    pub fn source(path: &HostPath) -> Option<crate::server::Source> {
        use crate::server::Source;
        let (set, i, rel) = Set::view_of(path)?;
        let view = &set.views[i];
        let (uid, name) = view.files.get(&rel)?;
        let held = view.records.get(uid)?;
        Some(match held.record.files.get(name)?.content.as_ref()? {
            Content::Text(_) => {
                Source::RecordText { record: path_of(&view.folder, *uid).ok()?, name: name.to_string() }
            }
            Content::Stored { sha1, .. } => Source::Stored { sha1: *sha1, store: view.store.dir.clone() },
        })
    }

    /// A file of a records layer's length and when its record was written, without its content.
    #[must_use]
    pub fn stat(path: &HostPath) -> Option<(u64, std::time::SystemTime)> {
        let (set, i, rel) = Set::view_of(path)?;
        let view = &set.views[i];
        let (uid, name) = view.files.get(&rel)?;
        let held = view.records.get(uid)?;
        Some((held.record.files.get(name)?.content.as_ref()?.size(), held.modified))
    }

    /// What a folder of a records layer holds: each name, and whether it is a folder. None for a path in no records layer.
    #[must_use]
    pub fn list(dir: &HostPath) -> Option<Vec<(Arc<Name>, bool)>> {
        let (set, i, rel) = Set::view_of(dir)?;
        let view = &set.views[i];
        let out = view.children.get(&rel).cloned().unwrap_or_default();
        Some(out)
    }

    /// A records layer's records, by uid: none where the folder is no records layer read.
    #[must_use]
    pub fn records_of(dir: &HostPath) -> Option<BTreeMap<Uid, Record>> {
        let (set, i) = Set::view_at(dir)?;
        Some(set.views[i].records.iter().map(|(uid, held)| (*uid, (*held.record).clone())).collect())
    }

    /// Whether a path is a folder of a records layer: none where it is in none.
    #[must_use]
    pub fn is_folder(path: &HostPath) -> Option<bool> {
        let (set, i, rel) = Set::view_of(path)?;
        Some(rel.is_root() || set.views[i].folders.contains(&rel))
    }

    /// Whether a path is a file of a records layer: none where it is in none.
    #[must_use]
    pub fn is_file(path: &HostPath) -> Option<bool> {
        let (set, i, rel) = Set::view_of(path)?;
        Some(set.views[i].files.contains_key(&rel))
    }

    /// What a records layer's records say as a changeset says it of a layer stored as a tree: what it creates, by uid
    /// and path; what it does to entries from below; whether it is read only, what it ignores.
    #[must_use]
    pub fn changeset(dir: &HostPath) -> Option<crate::changeset::Changeset> {
        let (set, i) = Set::view_at(dir)?;
        let view = &set.views[i];
        let mut changeset = crate::changeset::Changeset {
            read_only: view.settings.read_only,
            ignore: view.settings.ignore.clone(),
            ..crate::changeset::Changeset::default()
        };
        let path_of = |uid: &Uid| view.placed.get(uid).cloned();
        for (uid, Held { record, .. }) in &view.records {
            let op = match record.op {
                Op::Create => {
                    if let Some(at) = path_of(uid) {
                        changeset.entries.insert(*uid, at);
                    }
                    continue;
                }
                Op::Override => {
                    let files = record
                        .files
                        .iter()
                        .map(|(name, f)| {
                            let base = crate::changeset::Base { was: f.base, removed: f.content.is_none() };
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
            changeset.ops.insert(*uid, op);
        }
        Some(changeset)
    }
}

/// Make a layer's records those of a tree of files and its listing (`<folder>.toml` beside it, a changeset of
/// `[entries]` alone) - what a tool writes as a tree - in place of all it had but the extensions it holds. How many
/// records it holds. What the tree holds is read as it is, by no index: a `<name>.node` folder a node of every file in
/// it, another folder a folder, another file a plain file; each by the uid its listing gives its path, else one drawn
/// anew; each placed in the folder above it.
///
/// # Errors
/// The layer is no records layer; the listing does something else than create (an import makes, and only that); the
/// tree or its listing cannot be read; or the records cannot be written.
pub fn import(project: &crate::Project, name: &Name, from: &std::path::Path) -> Result<usize> {
    let from = &HostPath::new(from).with_context(|| from.display().to_string())?;
    let layer = project.layer(name)?;
    if layer.mounted || layer.kind == crate::LayerKind::Generated {
        bail!("{name}: a mounted or generated layer is no records layer");
    }
    let fs = project.io.filesystem();
    let listing = crate::changeset::Changeset::load(fs, &from.with_suffix(crate::CHANGESET_SUFFIX))?;
    if let Some(uid) = listing.ops.keys().next() {
        bail!("{name}: the listing does something to {uid} from below: an import only makes what it lists");
    }
    // The uids taken: by every layer's records, and by the listing.
    let mut taken: std::collections::HashSet<Uid> = listing.entries.keys().copied().collect();
    for view in &Set::open(project)?.views {
        taken.extend(view.records.keys().copied());
    }
    let store = crate::chunks::Store::at(&project.root, project.io.clone());
    let mut tree =
        Staging { fs, store: &store, listing: &listing, random: project.io.random(), taken, records: BTreeMap::new() };
    tree.folder(from, TreePath::root(), None)?;
    let records = tree.records;
    let said = Settings { read_only: listing.read_only, ignore: listing.ignore.clone() };
    // What changes, and only that: each record that differs from what the layer has, those it no longer has gone, its
    // settings where they differ.
    replace(project, &layer.records, &said, &records, &format!("Import the layer {name}"))?;
    Ok(records.len())
}

/// A mount's records, as its listing makes them: each entry it lists by its uid, where it is - its parent's uid, its
/// name - and what it is (`kind_of` its path; one that is nothing there is left out), holding none of its files, which
/// are the mount's tree's.
#[must_use]
pub fn listed(
    entries: &BTreeMap<Uid, Arc<TreePath>>,
    kind_of: &dyn Fn(&TreePath) -> Option<Kind>,
) -> BTreeMap<Uid, Record> {
    let uid_at: std::collections::HashMap<&TreePath, Uid> = entries.iter().map(|(uid, at)| (&**at, *uid)).collect();
    let mut records = BTreeMap::new();
    for (uid, path) in entries {
        let (Some(kind), Some(name)) = (kind_of(path), path.last()) else { continue };
        let parent = path.parent().filter(|parent| !parent.is_root()).and_then(|parent| uid_at.get(parent).copied());
        let name = Some(name.to_owned());
        records.insert(*uid, Record { op: Op::Create, kind, parent, name, files: BTreeMap::new() });
    }
    records
}

/// Make the records in `folder` these, with these settings: each record that differs from what the folder has
/// written, those it no longer has removed, its marker where its settings differ - as one set said in the log
/// (`write_set`), only what differs.
///
/// # Errors
/// The folder's records cannot be read, or the set cannot be written.
pub fn replace(
    project: &crate::Project,
    folder: &HostPath,
    settings_now: &Settings,
    records: &BTreeMap<Uid, Record>,
    label: &str,
) -> Result<()> {
    let fs = project.io.filesystem();
    let old = read_records(fs, folder, None)?;
    let mut writes: Vec<(HostPath, Option<String>)> = Vec::new();
    for (uid, record) in records {
        if old.get(uid).is_none_or(|held| *held.record != *record) {
            writes.push((path_of(folder, *uid)?, Some(record.render())));
        }
    }
    for uid in old.keys().filter(|uid| !records.contains_key(*uid)) {
        writes.push((path_of(folder, *uid)?, None));
    }
    if settings(fs, folder).ok().as_ref() != Some(settings_now) {
        writes.push((folder.join(&MARKER), Some(marker(settings_now))));
    }
    write_set(project, label, &writes)
}

/// Write a set of files of a project whole: said in its log before any is touched (`storage::Storage::say`), so that
/// one cut short is made whole as the project is next opened; each written whole, or (none) removed; the folders they
/// are in made.
fn write_set(project: &crate::Project, label: &str, writes: &[(HostPath, Option<String>)]) -> Result<()> {
    use crate::storage::Stored;
    let fs = project.io.filesystem();
    let mut storage = crate::storage::Storage::open(&project.root, project.io.clone());
    if let Some(storage) = &mut storage {
        let mut folders = BTreeSet::new();
        let mut said = Vec::new();
        for (path, text) in writes {
            match text {
                Some(text) => {
                    for folder in path.ancestors().skip(1).take_while(|f| *f != project.root && !fs.exists(f)) {
                        folders.insert(folder);
                    }
                    said.push(Stored::Put { path: storage.name(path), bytes: text.as_bytes().to_vec() });
                }
                None => said.push(Stored::Delete { path: storage.name(path) }),
            }
        }
        // The folders first, outermost first.
        let made = folders.iter().map(|folder| Stored::MakeDir { path: storage.name(folder) });
        let said: Vec<Stored> = made.chain(said).collect();
        storage.say(label, &said)?;
    }
    for (path, text) in writes {
        match text {
            Some(text) => {
                if let Some(folder) = path.parent() {
                    fs.create_folder_all(&folder)?;
                }
                crate::local::write_atomic(fs, path, text.as_bytes())?;
            }
            None => fs.remove_file(path)?,
        }
    }
    if let Some(mut storage) = storage {
        storage.wrote(writes.iter().map(|(path, _)| path.clone()));
        storage.checkpoint();
    }
    Ok(())
}

/// Make the layers of the project in `root` written as trees of files - `layers/<name>/`, with a listing beside where
/// one was written (`layers/<name>.toml`) - records layers, each imported as a tool's tree is (`import`): every layer
/// but a generated one, not records yet. What makes a project of trees written by hand, as a test or a scenario writes
/// one.
///
/// # Errors
/// `project.toml` cannot be read, a tree cannot be moved aside, or one cannot be imported.
pub fn import_trees(root: &HostPath, io: &Arc<dyn pe2_io::Io>) -> Result<()> {
    let fs = io.filesystem();
    let file = root.join(&crate::FILE);
    let said: toml::Table = fs.read_to_string(&file)?.parse().with_context(|| file.display().to_string())?;
    let layers = said.get("layer").and_then(toml::Value::as_array).cloned().unwrap_or_default();
    let staging = root.join(&Name::constant("staging"));
    let mut staged = Vec::new();
    // A generated layer is its plan's to fill; a mount's files are its tree.
    let made_by_hand =
        |l: &&toml::Value| l.get("kind").and_then(toml::Value::as_str) != Some("generated") && l.get("mount").is_none();
    for layer in layers.iter().filter(made_by_hand) {
        let name = layer.get("name").and_then(toml::Value::as_str).context("a layer with no name")?;
        let name = Name::new(name)?.to_owned();
        let folder = root.join(&crate::LAYERS).join(&name);
        if is_records(fs, &folder) {
            continue;
        }
        let (tree, listing) = (staging.join(&name), folder.with_suffix(crate::CHANGESET_SUFFIX));
        fs.create_folder_all(&staging)?;
        if fs.exists(&folder) {
            fs.rename(&folder, &tree)?;
        } else {
            fs.create_folder_all(&tree)?;
        }
        if fs.exists(&listing) {
            fs.rename(&listing, &tree.with_suffix(crate::CHANGESET_SUFFIX))?;
        }
        fs.create_folder_all(&folder)?;
        mark(fs, &folder, &Settings::default())?;
        staged.push((name, tree));
    }
    if staged.is_empty() {
        return Ok(());
    }
    let project = crate::Project::load_with(root.as_path(), io.clone())?;
    for (name, tree) in staged {
        import(&project, &name, tree.as_path())?;
    }
    fs.remove_all(&staging)?;
    Ok(())
}

/// A tree of files read as a layer's records (`import`).
struct Staging<'a> {
    fs: &'a dyn pe2_io::Filesystem,
    store: &'a crate::chunks::Store,
    listing: &'a crate::changeset::Changeset,
    random: &'a dyn pe2_io::Random,
    /// The uids given, or listed: a uid drawn anew is none of them.
    taken: std::collections::HashSet<Uid>,
    records: BTreeMap<Uid, Record>,
}

impl Staging<'_> {
    /// The uid of what is at a path: the listing's; else, for a node, the one its `node.toml` names where none has it,
    /// as the index lists what a layer holds unlisted; else one drawn anew.
    fn uid(&mut self, path: &TreePath, node_toml: Option<&HostPath>) -> Uid {
        if let Some((uid, _)) = self.listing.entries.iter().find(|(_, at)| ***at == *path) {
            return *uid;
        }
        let declared = node_toml
            .and_then(|file| self.fs.read_to_string(file).ok())
            .and_then(|text| text.parse::<toml::Table>().ok())
            .and_then(|table| Uid::new(table.get("uid")?.as_str()?).ok())
            .filter(|uid| !self.taken.contains(uid));
        if let Some(uid) = declared {
            self.taken.insert(uid);
            return uid;
        }
        let taken = &self.taken;
        let uid = crate::uid::new(self.random, |uid| taken.contains(&uid));
        self.taken.insert(uid);
        uid
    }

    /// What a folder of the tree holds, at `path`, in the entry `parent`.
    fn folder(&mut self, dir: &HostPath, path: &TreePath, parent: Option<Uid>) -> Result<()> {
        let mut entries = self.fs.read_directory(dir).with_context(|| dir.display().to_string())?;
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        for entry in entries {
            let name =
                Name::new(entry.name.to_str().with_context(|| format!("{}: a name that is no text", dir.display()))?)
                    .with_context(|| format!("{}: {}", dir.display(), entry.name.to_string_lossy()))?;
            let (disk, at) = (dir.join(name), path.join(name));
            let node = entry.kind == pe2_io::Kind::Folder && at.as_str().ends_with(crate::index::BUNDLE_SUFFIX);
            let uid = self.uid(&at, node.then(|| disk.join(crate::index::NODE_FILE)).as_ref());
            let record = match entry.kind {
                pe2_io::Kind::Folder if at.as_str().ends_with(crate::index::BUNDLE_SUFFIX) => {
                    let mut files = BTreeMap::new();
                    self.files(&disk, RelPath::root(), &mut files)?;
                    let named = crate::index::asset_named(&at)?;
                    let name = named.last().with_context(|| format!("{at}: no name"))?.to_owned();
                    Record { op: Op::Create, kind: Kind::Node, parent, name: Some(name), files }
                }
                pe2_io::Kind::Folder => {
                    self.folder(&disk, &at, Some(uid))?;
                    Record {
                        op: Op::Create,
                        kind: Kind::Folder,
                        parent,
                        name: Some(name.to_owned()),
                        files: BTreeMap::new(),
                    }
                }
                _ => {
                    let content =
                        Content::of(&self.fs.read(&disk).with_context(|| disk.display().to_string())?, self.store)?;
                    let files =
                        BTreeMap::from([(RelPath::root().to_owned(), File { content: Some(content), base: None })]);
                    Record { op: Op::Create, kind: Kind::File, parent, name: Some(name.to_owned()), files }
                }
            };
            self.records.insert(uid, record);
        }
        Ok(())
    }

    /// A node's files, by their names in it.
    fn files(&self, dir: &HostPath, within: &RelPath, out: &mut BTreeMap<Arc<RelPath>, File>) -> Result<()> {
        for entry in self.fs.read_directory(dir).with_context(|| dir.display().to_string())? {
            let name =
                Name::new(entry.name.to_str().with_context(|| format!("{}: a name that is no text", dir.display()))?)
                    .with_context(|| format!("{}: {}", dir.display(), entry.name.to_string_lossy()))?;
            let (disk, at) = (dir.join(name), within.join(name));
            if entry.kind == pe2_io::Kind::Folder {
                self.files(&disk, &at, out)?;
            } else {
                let content =
                    Content::of(&self.fs.read(&disk).with_context(|| disk.display().to_string())?, self.store)?;
                out.insert(at, File { content: Some(content), base: None });
            }
        }
        Ok(())
    }
}

/// Write a records layer's entries out as a tree of files, with its changeset beside it (`<folder>.toml`): what it
/// creates, for tools that read a layer as files (`init.py compare`). How many entries.
///
/// # Errors
/// The layer is no records layer read, or a file cannot be read or written.
pub fn export(project: &crate::Project, name: &Name, to: &std::path::Path) -> Result<usize> {
    let fs = project.io.filesystem();
    let to = &HostPath::new(to).with_context(|| to.display().to_string())?;
    let layer = project.layer(name)?;
    Set::open(project)?;
    let mut changeset = Set::changeset(&layer.path).with_context(|| format!("{name} is no records layer"))?;
    changeset.ops.clear();
    for path in changeset.entries.values() {
        let (from, out) = (layer.path.join(path), to.join(path));
        if path.as_str().ends_with(crate::index::BUNDLE_SUFFIX) {
            fs.create_folder_all(&out)?;
            let (held, refused) = crate::index::bundle_files(&project.io, &from);
            // Every file of it goes, or none does: one named so no path takes it is renamed first.
            if let Some(why) = refused.first() {
                bail!("{path}: {why}");
            }
            for (file, at) in held {
                let target = out.join(&file);
                if let Some(folder) = target.parent() {
                    fs.create_folder_all(&folder)?;
                }
                fs.write(&target, &crate::content::read(&project.io, &at)?)?;
            }
        } else if crate::content::exists(&project.io, &from) {
            if let Some(folder) = out.parent() {
                fs.create_folder_all(&folder)?;
            }
            fs.write(&out, &crate::content::read(&project.io, &from)?)?;
        } else {
            fs.create_folder_all(&out)?;
        }
    }
    changeset.save(fs, &to.with_suffix(crate::CHANGESET_SUFFIX))?;
    Ok(changeset.entries.len())
}

/// Write a layer's records into its folder, marked with its settings.
///
/// # Errors
/// A record cannot be written.
pub fn write_all(
    fs: &dyn pe2_io::Filesystem,
    dir: &HostPath,
    settings: &Settings,
    records: &BTreeMap<Uid, Record>,
) -> Result<()> {
    for (uid, record) in records {
        let at = path_of(dir, *uid).with_context(|| format!("{uid}: its record"))?;
        if let Some(shard) = at.parent() {
            fs.create_folder_all(&shard)?;
        }
        crate::local::write_atomic(fs, &at, record.render().as_bytes())?;
    }
    mark(fs, dir, settings)
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
    old: &BTreeMap<Uid, Record>,
    written: &BTreeMap<Arc<TreePath>, Option<Vec<u8>>>,
) -> Result<(BTreeMap<Uid, Record>, Vec<Uid>)> {
    use crate::changeset::Op as Did;
    let store = crate::chunks::Store::at(&project.root, project.io.clone());
    // What the layers below resolve is asked, entry by entry: the layer's own are few beside it.
    let below = index.under(&layer.name);
    let mut uid_at: std::collections::HashMap<Arc<TreePath>, Uid> = std::collections::HashMap::new();
    for (uid, path) in &changeset.entries {
        uid_at.insert(path.clone(), *uid);
    }
    for (uid, op) in &changeset.ops {
        if let Did::Move { to } | Did::Override { to: Some(to), .. } = op {
            uid_at.insert(to.clone(), *uid);
        }
    }
    let place = |path: &TreePath| -> Result<(Option<Uid>, Arc<Name>)> {
        let named = |p: &TreePath| uid_at.get(p).copied().or_else(|| below.at(p).map(|(uid, _)| uid));
        let parent = match path.parent().filter(|parent| !parent.is_root()) {
            Some(p) => Some(named(p).with_context(|| format!("{path}: nothing names {p}"))?),
            None => None,
        };
        let asset = crate::index::asset_named(path)?;
        let name = asset.last().with_context(|| format!("{path}: no name"))?;
        Ok((parent, name.to_owned()))
    };
    let mut claimed = std::collections::HashSet::new();
    // What was written of an entry at a path: its files by name, as a node's are inside its folder, a file's itself.
    let mut written_at = |path: &TreePath, kind: Kind| -> Vec<(Arc<RelPath>, Option<Vec<u8>>)> {
        let found: Vec<(Arc<RelPath>, Option<Vec<u8>>)> = written
            .iter()
            .filter_map(|(at, bytes)| match kind {
                Kind::Node => {
                    at.strip_prefix(path).filter(|name| !name.is_root()).map(|name| (name.to_owned(), bytes.clone()))
                }
                Kind::File => (**at == *path).then(|| (RelPath::root().to_owned(), bytes.clone())),
                Kind::Folder => None,
            })
            .collect();
        claimed.extend(written.keys().filter(|at| at.starts_with(path)).cloned());
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
                if path.as_str().ends_with(crate::index::BUNDLE_SUFFIX) {
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
                Some(bytes) => files.insert(name, File { content: Some(Content::of(&bytes, &store)?), base: None }),
                None => files.remove(&name),
            };
        }
        let (parent, name) = place(path)?;
        new.insert(*uid, Record { op: Op::Create, kind, parent, name: Some(name), files });
    }
    for (uid, op) in &changeset.ops {
        let from = below.of(*uid);
        let kind = old.get(uid).map(|r| r.kind).or_else(|| from.map(|(_, k)| of_kind(k))).unwrap_or(Kind::File);
        let record = match op {
            Did::Delete => Record { op: Op::Delete, kind, parent: None, name: None, files: BTreeMap::new() },
            Did::Move { to } => {
                let (parent, name) = place(to)?;
                Record { op: Op::Move, kind, parent, name: Some(name), files: BTreeMap::new() }
            }
            Did::Override { files: bases, to } => {
                let at = to.as_deref().or(from.map(|(p, _)| p)).context("an override of nothing")?.to_owned();
                let (parent, name) = match to {
                    Some(to) => place(to).map(|(p, n)| (p, Some(n)))?,
                    None => (None, None),
                };
                let now: BTreeMap<Arc<RelPath>, Option<Vec<u8>>> = written_at(&at, kind).into_iter().collect();
                let held = old.get(uid).filter(|r| r.op == Op::Override).map(|r| &r.files);
                Record { op: Op::Override, kind, parent, name, files: override_files(bases, now, held, &store)? }
            }
        };
        new.insert(*uid, record);
    }
    if let Some(stray) = written.iter().find(|(at, bytes)| bytes.is_some() && !claimed.contains(*at)) {
        bail!("{}: no entry of {} is there to hold it", stray.0, layer.name);
    }
    let gone = old.keys().filter(|uid| !new.contains_key(*uid)).copied().collect();
    new.retain(|uid, record| old.get(uid) != Some(record));
    Ok((new, gone))
}

/// An override's files in its record: each it holds as written now, else as it held it; each it removes, removed;
/// each with what it was made from.
fn override_files(
    bases: &BTreeMap<Arc<RelPath>, crate::changeset::Base>,
    mut now: BTreeMap<Arc<RelPath>, Option<Vec<u8>>>,
    held: Option<&BTreeMap<Arc<RelPath>, File>>,
    store: &crate::chunks::Store,
) -> Result<BTreeMap<Arc<RelPath>, File>> {
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
        files.insert(file.clone(), File { content, base: base.was });
    }
    Ok(files)
}

#[cfg(test)]
#[allow(
    clippy::disallowed_methods,
    reason = "a test's fixtures: the machine's time, places, files and environment, which nothing tested reads"
)]
mod tests {
    use super::{Content, File, Kind, Op, Record};
    use crate::testing::At as _;
    use crate::testing::{name, rel, uid};
    use std::collections::BTreeMap;

    #[test]
    fn a_record_round_trips_with_its_text_as_written() {
        let text = |t: &str| Some(Content::Text(t.to_string()));
        let mut record = Record {
            op: Op::Override,
            kind: Kind::Node,
            parent: Some(uid("uid://1izcb2mud6bit")),
            name: Some(name("box")),
            files: BTreeMap::default(),
        };
        for (name, content) in [
            ("node.toml", text("type = \"data\"\n[settings]\nsize = 3\n")),
            ("odd.txt", text("\nleading, ''' and \"\"\", tab\there\r\nunterminated")),
            ("native.texels", Some(Content::Stored { sha1: crate::extract::sha1_of(b"texels"), size: 1_835_008 })),
            ("old.bin", None),
        ] {
            record.files.insert(rel(name), File { content, base: Some(crate::extract::sha1_of(b"base")) });
        }
        let written = record.render();
        assert!(written.starts_with("format = 1\nop = \"override\"\n"), "{written}");
        let base = crate::extract::sha1_of(b"base");
        assert!(
            written.contains(&format!("[files.\"node.toml\"]\nbase = \"{base}\"\ntext = \"\"\"\ntype = \"data\"\n")),
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
    fn a_record_naming_two_files_alike_is_refused() {
        let text = |a: &str, b: &str| {
            format!(
                "format = {}\nop = \"create\"\nkind = \"node\"\nname = \"n\"\n[files.{a:?}]\ntext = \"1\"\n\
                 [files.{b:?}]\ntext = \"2\"\n",
                super::FORMAT
            )
        };
        let error = Record::parse(&text("Notes.txt", "notes.txt")).unwrap_err();
        assert!(error.to_string().contains("one name"), "{error}");
        assert!(Record::parse(&text("caf\u{e9}.txt", "cafe\u{301}.txt")).is_err());
        assert_eq!(Record::parse(&text("a.txt", "b.txt")).unwrap().files.len(), 2);
    }

    /// A project of its own layer alone, and a tool's tree beside it to import: a node, one holding bytes no text is,
    /// a folder with a node and a plain file in it, and an empty folder.
    fn staged(name: &str) -> (sfs::HostPath, sfs::HostPath) {
        let root = crate::testing::scratch(name);
        let _ = std::fs::remove_dir_all(&root);
        crate::create(&root, &pe2_io::SystemFilesystem).unwrap();
        let tree = root.at("staging/tree");
        for (path, bytes) in [
            ("a.node/node.toml", &b"stack = [{ op = \"t.file\" }]\n"[..]),
            ("b.node/node.toml", b"stack = [{ op = \"t.file\" }]\n"),
            ("b.node/native.bin", b"\0\x01\x02"),
            ("g/c.node/node.toml", b"x = 1\n"),
            ("g/notes", b"notes"),
        ] {
            std::fs::create_dir_all(tree.at(path).parent().unwrap()).unwrap();
            std::fs::write(tree.at(path), bytes).unwrap();
        }
        std::fs::create_dir_all(tree.at("empty")).unwrap();
        // Each listed by a uid of its own - but the empty folder, given one as it is imported.
        let listing = "[entries]\n\"uid://1a2b3c4d5e6f7\" = \"a.node\"\n\"uid://2b\" = \"b.node\"\n\"uid://3g\" = \"g\"\n\
                       \"uid://4c\" = \"g/c.node\"\n\"uid://5n\" = \"g/notes\"\n";
        std::fs::write(root.at("staging/tree.toml"), listing).unwrap();
        (root, tree)
    }

    /// Every file under a folder, by its path within it, with its bytes - but what an atomic write left beside its file
    /// as the disk failed (`.<name>.<process>.<n>.tmp`), until storage writes its temporary files out of the trees.
    fn files(dir: &std::path::Path) -> std::collections::BTreeMap<String, Vec<u8>> {
        let mut out = std::collections::BTreeMap::new();
        let mut pending = vec![(dir.to_path_buf(), String::new())];
        while let Some((at, within)) = pending.pop() {
            for entry in std::fs::read_dir(&at).into_iter().flatten().flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') && std::path::Path::new(&name).extension().is_some_and(|e| e == "tmp") {
                    continue;
                }
                let path = if within.is_empty() { name } else { format!("{within}/{name}") };
                if entry.file_type().unwrap().is_dir() {
                    out.insert(format!("{path}/"), Vec::new());
                    pending.push((entry.path(), path));
                } else {
                    out.insert(path, std::fs::read(entry.path()).unwrap());
                }
            }
        }
        out
    }

    #[test]
    fn an_import_exported_again_is_the_tree_it_was() {
        let (root, tree) = staged("import-back");
        let project = crate::Project::load(&root).unwrap();
        let layer = crate::testing::name("project");
        assert_eq!(super::import(&project, &layer, tree.as_path()).unwrap(), 6);
        let out = root.at("staging/out");
        assert_eq!(super::export(&project, &layer, out.as_path()).unwrap(), 6);
        assert_eq!(files(out.as_path()), files(tree.as_path()));
        // Each by the uid the listing gave it; what it listed not, by one drawn for it.
        let listed =
            crate::changeset::Changeset::load(&pe2_io::SystemFilesystem, &root.at("staging/out.toml")).unwrap();
        assert_eq!(listed.entries.get(&uid("uid://1a2b3c4d5e6f7")).map(|p| p.as_str()), Some("a.node"));
        assert_eq!(listed.entries.len(), 6);
        // Imported again as it is: nothing differs, nothing is written.
        let before = files(root.at("layers/project").as_path());
        std::fs::rename(root.at("staging/out.toml"), root.at("staging/tree.toml")).unwrap();
        super::import(&project, &layer, tree.as_path()).unwrap();
        assert_eq!(files(root.at("layers/project").as_path()), before);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_import_only_makes() {
        let (root, tree) = staged("import-makes");
        std::fs::write(root.at("staging/tree.toml"), "[ops]\n\"uid://1a2b3c4d5e6f7\" = { delete = true }\n").unwrap();
        let project = crate::Project::load(&root).unwrap();
        let refused = super::import(&project, &crate::testing::name("project"), tree.as_path()).unwrap_err();
        assert!(refused.to_string().contains("only makes"), "{refused:#}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_import_cut_short_at_any_change_is_whole_or_none_once_opened_again() {
        let faulty = |root: &sfs::HostPath| {
            let fs = pe2_io::Faulty::new(std::sync::Arc::new(pe2_io::SystemFilesystem));
            let io = pe2_io::Composed { filesystem: std::sync::Arc::new(fs.clone()), ..pe2_io::Composed::system() };
            (crate::Project::load_with(root.as_path(), std::sync::Arc::new(io)).unwrap(), fs)
        };
        let layer = crate::testing::name("project");
        // How many changes it makes, and what it leaves - unfailed.
        let staged = |name: &str| {
            let (root, tree) = staged(name);
            let listed = std::fs::read_to_string(root.at("staging/tree.toml")).unwrap();
            std::fs::write(root.at("staging/tree.toml"), format!("{listed}\"uid://6e\" = \"empty\"\n")).unwrap();
            (root, tree)
        };
        let (root, tree) = staged("import-counted");
        let before = files(root.at("layers/project").as_path());
        let (project, fs) = faulty(&root);
        let counted = fs.changes();
        super::import(&project, &layer, tree.as_path()).unwrap();
        let (changes, after) = (fs.changes() - counted, files(root.at("layers/project").as_path()));
        assert!(changes > 6 && before != after, "{changes} changes");
        std::fs::remove_dir_all(&root).unwrap();
        for how in [pe2_io::Fault::Once, pe2_io::Fault::From] {
            for nth in 1..=changes + 1 {
                let (root, tree) = staged(&format!("import-{how:?}-{nth}"));
                let (project, fs) = faulty(&root);
                fs.fail(nth, how);
                let imported = super::import(&project, &layer, tree.as_path());
                drop(project);
                fs.heal();
                // Opened again, with nothing failing: what was said whole is made whole, what was not is none.
                let project = crate::Project::load(&root).unwrap();
                let lock = crate::lock::Lock::try_acquire(&root, "a test", &pe2_io::System).unwrap();
                crate::local::Local::open(&project, &lock)
                    .unwrap_or_else(|e| panic!("{how:?} at change {nth}: opened again: {e:#}"));
                let opened = files(root.at("layers/project").as_path());
                assert!(
                    opened == before || opened == after,
                    "{how:?} at change {nth} ({imported:?}): half of it, once opened again: {:?}",
                    opened.keys().collect::<Vec<_>>()
                );
                drop(lock);
                crate::local::remove_all(&root);
            }
        }
    }

    #[test]
    fn a_record_is_placed_by_its_uid() {
        let layer = &crate::testing::scratch("record-place").at("layers/top");
        let at = super::path_of(layer, uid("uid://1hwweozpa40d9")).unwrap();
        assert_eq!(at, layer.at("1h/1hwweozpa40d9.toml"));
        assert_eq!(super::uid_of(layer, &at), Some(uid("uid://1hwweozpa40d9")));
        assert_eq!(super::uid_of(layer, &layer.join(&super::MARKER)), None);
        assert_eq!(super::uid_of(layer, &layer.at("extensions/1hwweozpa40d9.toml")), None);
        assert_eq!(super::uid_of(&layer.at("1h"), &at), None, "of another folder");
    }
}
