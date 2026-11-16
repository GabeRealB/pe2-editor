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

/// A file's content in a record: inline text, or a file kept whole in the store - or, a sealed node's, a whole object
/// of its tree (`Record::tree`) - or, an override's, what it changes of its base (`delta`): the file it makes is `sha1`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Content {
    Text(String),
    Stored { sha1: pe2_ext::Sha1, size: u64 },
    Object { sha1: pe2_ext::Sha1, size: u64 },
    Delta { sha1: pe2_ext::Sha1, size: u64, base: pe2_ext::Sha1, delta: Delta },
}

/// What an override's file changes of its base (`delta`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Delta {
    /// Fields of a TOML file, each set to a value - written as TOML - or removed (none).
    Fields(Vec<(Vec<crate::merge::fields::Step>, Option<String>)>),
    /// Runs of lines of a text (`delta::lines_of`).
    Lines(String),
    /// Bytes (`delta::bytes_of`), kept in the store by this SHA-1.
    Bytes(pe2_ext::Sha1),
}

/// The longest text a record holds inline: longer, it is kept in the store.
pub const INLINE: usize = 64 << 10;

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
    /// Its files by name; a plain file's own by the root. A sealed node's are its tree's: these are only those it
    /// removes of the version below.
    pub files: BTreeMap<Arc<RelPath>, File>,
    /// A sealed node's content: a tree of whole objects in the store (`chunks::Tree`), kept and replaced whole, never
    /// merged or edited - an extension, where it holds a manifest (`ext::folder::MANIFEST`), wherever it is.
    pub tree: Option<pe2_ext::Sha1>,
    /// Where it is kept: in this machine's part of its layer (`.local/layers/<layer>`), never shipped - what its
    /// file's place says, not its text.
    pub local: bool,
    /// A merge that could not be made (`gitmerge`): the other side's record, whole, beside this one (`[merge] theirs`),
    /// until the user keeps this or takes that (`settle`).
    pub theirs: Option<String>,
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
    /// A file's content as a record keeps it: a text inline - so that a layer's history reads as text - where it is
    /// UTF-8, holds no NUL (as git tells binary files), is at most `INLINE` long and is written and read back as it is;
    /// anything else kept in the store.
    ///
    /// # Errors
    /// It cannot be kept.
    pub fn of(bytes: &[u8], store: &crate::chunks::Store) -> Result<Content> {
        if bytes.len() <= INLINE
            && !bytes.contains(&0)
            && let Ok(text) = std::str::from_utf8(bytes)
            && inline_as_it_is(text)
        {
            return Ok(Content::Text(text.to_string()));
        }
        let sha1 = store.put_file(bytes)?;
        Ok(Content::Stored { sha1, size: bytes.len() as u64 })
    }

    /// An override's file as a record keeps it: what it changes of its base (`delta`), where the base is in a store and
    /// what it changes is less than half of it - the file whole (`of`) otherwise. A delta is kept only where it gives
    /// the file back; one of bytes is kept in `store`, as the file would be.
    ///
    /// # Errors
    /// It cannot be kept.
    pub fn over(
        bytes: &[u8],
        base: Option<pe2_ext::Sha1>,
        name: &str,
        store: &crate::chunks::Store,
    ) -> Result<Content> {
        let whole = |store| Content::of(bytes, store);
        let Some(base) = base else { return whole(store) };
        let Ok(from) = store.get_file(&base) else { return whole(store) };
        let (sha1, size) = (crate::extract::sha1_of(bytes), bytes.len() as u64);
        let texts = (std::str::from_utf8(&from), std::str::from_utf8(bytes));
        let delta = if let (Ok(from), Ok(to)) = texts
            && !bytes.contains(&0)
        {
            let toml = std::path::Path::new(name).extension().is_some_and(|e| e == "toml");
            let fields = toml.then(|| crate::delta::fields(from, to)).flatten();
            let fields = fields.map(|sets| {
                Delta::Fields(sets.into_iter().map(|set| (set.at, set.to.map(|value| value.to_string()))).collect())
            });
            let delta = fields.unwrap_or_else(|| Delta::Lines(crate::delta::lines_of(from, to)));
            // As large as the file's half, or not written and read back as it is: the file whole.
            let said = delta_len(&delta);
            if said * 2 >= bytes.len() || matches!(&delta, Delta::Lines(text) if !inline_as_it_is(text)) {
                return whole(store);
            }
            delta
        } else {
            let made = crate::delta::bytes_of(&from, bytes);
            if made.len() * 2 >= bytes.len() {
                return whole(store);
            }
            Delta::Bytes(store.put_file(&made)?)
        };
        let content = Content::Delta { sha1, size, base, delta };
        // Held to giving the file back.
        if content.bytes(store).ok().as_deref() != Some(bytes) {
            return whole(store);
        }
        Ok(content)
    }

    /// Its bytes: a delta's made of its base, which either of the project's stores keeps.
    ///
    /// # Errors
    /// A file kept in the store is not there - a delta's base among them - or a delta is none of its form.
    pub fn bytes(&self, store: &crate::chunks::Store) -> std::io::Result<Vec<u8>> {
        match self {
            Content::Text(text) => Ok(text.clone().into_bytes()),
            Content::Stored { sha1, .. } => store.get_file(sha1),
            Content::Object { sha1, .. } => store.get_object(sha1),
            Content::Delta { base, delta, .. } => {
                let from = store.get_file(base).map_err(|e| {
                    let why = format!(
                        "the version it was made from ({base}) is in no store of the project's - fill the project \
                         from the discs, or take the version below: {e}"
                    );
                    // Not the file gone: it is there, and its base is not.
                    std::io::Error::other(why)
                })?;
                apply(&from, delta, store).map_err(|e| std::io::Error::other(format!("{e:#}")))
            }
        }
    }

    /// Its length.
    #[must_use]
    pub fn size(&self) -> u64 {
        match self {
            Content::Text(text) => text.len() as u64,
            Content::Stored { size, .. } | Content::Object { size, .. } | Content::Delta { size, .. } => *size,
        }
    }

    /// Its SHA-1.
    #[must_use]
    pub fn sha1(&self) -> pe2_ext::Sha1 {
        match self {
            Content::Text(text) => crate::extract::sha1_of(text.as_bytes()),
            Content::Stored { sha1, .. } | Content::Object { sha1, .. } | Content::Delta { sha1, .. } => *sha1,
        }
    }
}

/// Where the file a content is can be read whole, by its SHA-1, for one that is a delta (`Content::Delta`): made of
/// its base once, and kept in this machine's store (`chunks::Store::local_one`) as any file kept whole - read by range
/// as any is. Of another content, none.
///
/// # Errors
/// Its base is in no store of the project's, or what it makes is not the file it says.
pub fn made(content: &Content, store: &crate::chunks::Store) -> std::io::Result<Option<pe2_ext::Sha1>> {
    let Content::Delta { sha1, .. } = content else { return Ok(None) };
    if store.holding_file(sha1).is_none() {
        let bytes = content.bytes(store)?;
        if crate::extract::sha1_of(&bytes) != *sha1 {
            return Err(std::io::Error::other(format!("what its delta makes is not the file {sha1}")));
        }
        store.local_one().put_file(&bytes)?;
    }
    Ok(Some(*sha1))
}

/// A delta applied to its base's bytes.
///
/// # Errors
/// It is none of its form, a field's value is no TOML, or a bytes delta is in no store.
fn apply(base: &[u8], delta: &Delta, store: &crate::chunks::Store) -> Result<Vec<u8>> {
    apply_with(base, delta, &|sha1| store.get_file(sha1).ok())
}

/// A delta applied to its base's bytes, a bytes delta read by `read`.
///
/// # Errors
/// It is none of its form, a field's value is no TOML, or a bytes delta is not to be read.
pub(crate) fn apply_with(
    base: &[u8],
    delta: &Delta,
    read: &dyn Fn(&pe2_ext::Sha1) -> Option<Vec<u8>>,
) -> Result<Vec<u8>> {
    match delta {
        Delta::Fields(sets) => {
            let base = std::str::from_utf8(base).context("a base that is no text")?;
            let sets: Vec<crate::delta::Set> = sets
                .iter()
                .map(|(at, to)| -> Result<crate::delta::Set> {
                    let to = to.as_deref().map(value_of).transpose()?;
                    Ok(crate::delta::Set { at: at.clone(), to })
                })
                .collect::<Result<_>>()?;
            Ok(crate::delta::apply_fields(base, &sets).context("a base that is no TOML")?.into_bytes())
        }
        Delta::Lines(text) => {
            let base = std::str::from_utf8(base).context("a base that is no text")?;
            Ok(crate::delta::apply_lines(base, text)?.into_bytes())
        }
        Delta::Bytes(sha1) => {
            crate::delta::apply_bytes(base, &read(sha1).with_context(|| format!("its delta {sha1}"))?)
        }
    }
}

/// A file as a record holds it inline (`Content::of`), where it can be: none where it would be kept in a store.
pub(crate) fn inline(bytes: &[u8]) -> Option<Content> {
    let text = std::str::from_utf8(bytes)
        .ok()
        .filter(|text| bytes.len() <= INLINE && !bytes.contains(&0) && inline_as_it_is(text))?;
    Some(Content::Text(text.to_string()))
}

/// What a file changes of its base (`base`, whose bytes these are), kept as a record keeps it with nothing new in a
/// store - its fields or lines, or the file inline (`Content::over`); none where it would be kept in a store.
pub(crate) fn delta_without_store(bytes: &[u8], base: pe2_ext::Sha1, from: &[u8], name: &str) -> Option<Content> {
    let (Ok(from), Ok(to)) = (std::str::from_utf8(from), std::str::from_utf8(bytes)) else { return None };
    let toml = std::path::Path::new(name).extension().is_some_and(|e| e == "toml");
    let fields = toml.then(|| crate::delta::fields(from, to)).flatten().map(|sets| {
        Delta::Fields(sets.into_iter().map(|set| (set.at, set.to.map(|value| value.to_string()))).collect())
    });
    let delta = fields.unwrap_or_else(|| Delta::Lines(crate::delta::lines_of(from, to)));
    if delta_len(&delta) * 2 >= bytes.len() || matches!(&delta, Delta::Lines(text) if !inline_as_it_is(text)) {
        return inline(bytes);
    }
    let (sha1, size) = (crate::extract::sha1_of(bytes), bytes.len() as u64);
    Some(Content::Delta { sha1, size, base, delta })
}

/// A value, from the TOML it is written as.
fn value_of(text: &str) -> Result<toml::Value> {
    let mut table: toml::Table = toml::from_str(&format!("v = {text}")).with_context(|| format!("a value: {text}"))?;
    table.remove("v").context("a value")
}

/// How long a delta is, as a record holds it (a bytes delta: its key alone).
fn delta_len(delta: &Delta) -> usize {
    match delta {
        Delta::Fields(sets) => sets.iter().map(|(at, to)| at.len() * 8 + to.as_ref().map_or(0, String::len)).sum(),
        Delta::Lines(text) => text.len(),
        Delta::Bytes(_) => 40,
    }
}

/// Whether a text is written in a record and read back as it is.
fn inline_as_it_is(text: &str) -> bool {
    let mut doc = toml_edit::DocumentMut::new();
    doc["t"] = toml_edit::value(text);
    toml::from_str::<toml::Table>(&doc.to_string())
        .ok()
        .and_then(|t| t.get("t").cloned())
        .as_ref()
        .and_then(toml::Value::as_str)
        == Some(text)
}

/// What a file's table says it changes of its base: its fields set (`set`), its lines (`lines`) or a bytes delta
/// (`delta`); none where it says none.
///
/// # Errors
/// What it says is none of those forms.
fn delta_in(file: &toml::Table) -> Result<Option<Delta>> {
    use crate::merge::fields::Step;
    if let Some(text) = file.get("lines") {
        return Ok(Some(Delta::Lines(text.as_str().context("lines that are no text")?.to_string())));
    }
    if let Some(sha1) = file.get("delta") {
        return Ok(Some(Delta::Bytes(pe2_ext::Sha1::from_hex(sha1.as_str().context("a delta that is no SHA-1")?)?)));
    }
    let Some(sets) = file.get("set") else { return Ok(None) };
    let mut out = Vec::new();
    for set in sets.as_array().context("fields set that are no list")? {
        let at = set.get("at").and_then(toml::Value::as_array).context("a field set at no path")?;
        let at = at
            .iter()
            .map(|step| match step {
                toml::Value::String(key) => Ok(Step::Key(key.clone())),
                toml::Value::Integer(at) => Ok(Step::At(usize::try_from(*at)?)),
                toml::Value::Table(id) => {
                    Ok(Step::Id(id.get("id").and_then(toml::Value::as_str).context("a step by no id")?.to_string()))
                }
                other => bail!("a step that is none: {other}"),
            })
            .collect::<Result<Vec<Step>>>()?;
        out.push((at, set.get("to").map(ToString::to_string)));
    }
    Ok(Some(Delta::Fields(out)))
}

/// Write what a file changes of its base in its table (`delta_in`).
fn render_delta(table: &mut toml_edit::Table, delta: &Delta) {
    use crate::merge::fields::Step;
    match delta {
        Delta::Lines(text) => table["lines"] = toml_edit::value(text.as_str()),
        Delta::Bytes(sha1) => table["delta"] = toml_edit::value(sha1.to_string()),
        Delta::Fields(sets) => {
            let mut array = toml_edit::ArrayOfTables::new();
            for (at, to) in sets {
                let mut set = toml_edit::Table::new();
                let mut path = toml_edit::Array::new();
                for step in at {
                    match step {
                        Step::Key(key) => path.push(key.as_str()),
                        Step::At(at) => path.push(i64::try_from(*at).unwrap_or(i64::MAX)),
                        Step::Id(id) => {
                            let mut by = toml_edit::InlineTable::new();
                            by.insert("id", id.as_str().into());
                            path.push(by);
                        }
                    }
                }
                set["at"] = toml_edit::value(path);
                if let Some(value) = to.as_deref().and_then(|text| text.parse::<toml_edit::Value>().ok()) {
                    set["to"] = toml_edit::Item::Value(value);
                }
                array.push(set);
            }
            table["set"] = toml_edit::Item::ArrayOfTables(array);
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
            let base = field("base").map(|base| pe2_ext::Sha1::from_hex(&base)).transpose();
            let base = base.with_context(|| format!("{name}: its base"))?;
            let content = match (field("text"), field("sha1")) {
                (Some(text), None) => Some(Content::Text(text)),
                (None, Some(sha1)) => {
                    let size = file.get("size").and_then(toml::Value::as_integer).context("a stored file's size")?;
                    let (sha1, size) = (
                        pe2_ext::Sha1::from_hex(&sha1).with_context(|| format!("{name}: its sha1"))?,
                        u64::try_from(size)?,
                    );
                    match delta_in(file).with_context(|| format!("{name}: what it changes"))? {
                        Some(delta) => {
                            let base = base.with_context(|| format!("{name}: what it changes, of no base"))?;
                            Some(Content::Delta { sha1, size, base, delta })
                        }
                        None => Some(Content::Stored { sha1, size }),
                    }
                }
                (None, None) if file.get("removed").and_then(toml::Value::as_bool) == Some(true) => None,
                _ => bail!("{name}: neither text, a stored file nor removed"),
            };
            let at = RelPath::new(name).with_context(|| format!("its file {name}"))?.to_owned();
            // Names compare as a system that does not tell case or form apart takes them: two alike are one file.
            if let Some((other, _)) = files.get_key_value(&*at) {
                bail!("its files {other} and {name} are one name to a system that does not tell case or form apart");
            }
            files.insert(at, File { content, base });
        }
        let tree = string("tree").map(|text| pe2_ext::Sha1::from_hex(&text)).transpose().context("its tree")?;
        if tree.is_some() && kind != Kind::Node {
            bail!("a tree is a node's, not a {}'s", kind.name());
        }
        let theirs = table.get("merge").and_then(|merge| merge.get("theirs")).and_then(toml::Value::as_str);
        Ok(Record { op, kind, parent, name, files, tree, local: false, theirs: theirs.map(str::to_string) })
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
        if let Some(tree) = &self.tree {
            doc["tree"] = toml_edit::value(tree.to_string());
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
                    Some(Content::Stored { sha1, size } | Content::Object { sha1, size }) => {
                        table["sha1"] = toml_edit::value(sha1.to_string());
                        table["size"] = toml_edit::value(i64::try_from(*size).unwrap_or(i64::MAX));
                    }
                    Some(Content::Delta { sha1, size, delta, .. }) => {
                        table["sha1"] = toml_edit::value(sha1.to_string());
                        table["size"] = toml_edit::value(i64::try_from(*size).unwrap_or(i64::MAX));
                        render_delta(&mut table, delta);
                    }
                    None => table["removed"] = toml_edit::value(true),
                }
                files.insert(name.as_str(), toml_edit::Item::Table(table));
            }
            doc["files"] = toml_edit::Item::Table(files);
        }
        if let Some(theirs) = &self.theirs {
            let mut merge = toml_edit::Table::new();
            merge["theirs"] = toml_edit::value(theirs.as_str());
            doc["merge"] = toml_edit::Item::Table(merge);
        }
        doc.to_string()
    }
}

/// What a layer records of a sealed node (`Set::sealed_records`).
#[derive(Clone, Debug)]
pub struct SealedRecord {
    /// The layer's records' folder.
    pub folder: HostPath,
    pub uid: Uid,
    pub record: std::sync::Arc<Record>,
    /// Its tree, where the record has one and a store keeps it.
    pub tree: Option<std::sync::Arc<crate::chunks::Tree>>,
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
    /// The trees of its sealed nodes (`Record::tree`), by hash, read once.
    trees: std::collections::HashMap<pe2_ext::Sha1, Arc<crate::chunks::Tree>>,
    /// Where this machine's part of its records is (`Record::local`): none for a layer that has no such part.
    local: Option<HostPath>,
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
                let mut names: Vec<Arc<RelPath>> =
                    record.files.iter().filter(|(_, file)| file.content.is_some()).map(|(n, _)| n.clone()).collect();
                if let Some(tree) = &record.tree {
                    names.extend(self.tree_names(at, tree));
                }
                for name in names {
                    let mut folder = name.parent();
                    while let Some(f) = folder.filter(|f| !f.is_root()) {
                        self.folders.insert(at.join_path(f));
                        folder = f.parent();
                    }
                    self.files.insert(at.join_path(&name), (uid, name));
                }
            }
        }
    }

    /// The names of a sealed node's files, its tree read where it is not yet: none, and a problem, where neither of the
    /// project's stores keeps it or a name in it is none a path takes.
    fn tree_names(&mut self, at: &TreePath, sha1: &pe2_ext::Sha1) -> Vec<Arc<RelPath>> {
        if !self.trees.contains_key(sha1) {
            let Some((_, tree)) = self.store.holding_tree(sha1) else {
                self.problems.push(format!("{at}: its tree {sha1} is in neither of the project's stores"));
                return Vec::new();
            };
            self.trees.insert(*sha1, Arc::new(tree));
        }
        let mut names = Vec::new();
        for name in self.trees[sha1].files.keys() {
            match RelPath::new(name) {
                Ok(name) => names.push(name.to_owned()),
                Err(e) => self.problems.push(format!("{at}: its tree holds {name}, which no path takes: {e}")),
            }
        }
        names
    }

    /// A file's content in an entry: its record's - none where it removes it - or, a sealed node's, its tree's.
    fn content(&self, uid: Uid, name: &RelPath) -> Option<Content> {
        let held = self.records.get(&uid)?;
        if let Some(file) = held.record.files.get(name) {
            return file.content.clone();
        }
        let file = self.trees.get(held.record.tree.as_ref()?)?.files.get(name.as_str())?;
        Some(Content::Object { sha1: file.sha1, size: file.size })
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
            trees: std::collections::HashMap::new(),
            local: None,
        }
    }

    /// Where a record of it is kept: in its part (`Record::local`).
    fn record_path(&self, uid: Uid, local: bool) -> Option<HostPath> {
        let folder = if local { self.local.as_ref()? } else { &self.folder };
        path_of(folder, uid).ok()
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
    local: Option<&HostPath>,
    before: &Read,
    changed: &[HostPath],
) -> Option<Result<Read>> {
    if local.is_some_and(|local| changed.iter().any(|path| path.starts_with(local))) {
        return None;
    }
    let ours: Vec<&HostPath> = changed.iter().filter(|path| path.starts_with(dir)).collect();
    // What changed of a record a local one hides, or of the local part, is read with the layer whole.
    if ours.iter().any(|path| uid_of(dir, path).is_some_and(|uid| before.get(&uid).is_some_and(|h| h.record.local))) {
        return None;
    }
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
                    waits(&mut read, uid, &text, at, false).with_context(|| path.display().to_string())?;
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
fn waits(read: &mut Read, uid: Uid, text: &str, at: std::time::SystemTime, local: bool) -> Result<()> {
    let record = Record { local, ..Record::parse(text)? };
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
    read_part(fs, dir, before, false)
}

/// A layer's records, both its parts (`read_part`): the shipped one in `dir`, and this machine's in `local`, its records
/// over the shipped one's of their uids.
///
/// # Errors
/// As `read_part`.
pub(crate) fn read_layer(
    fs: &dyn pe2_io::Filesystem,
    dir: &HostPath,
    local: Option<&HostPath>,
    before: Option<&Read>,
) -> Result<Read> {
    let mut read = read_part(fs, dir, before, false)?;
    if let Some(local) = local {
        read.extend(read_part(fs, local, before, true)?);
    }
    Ok(read)
}

/// One part of a layer's records (`local`, this machine's, or the shipped one), read on every core: those whose file
/// is as it was when `before` read it, from the same part, taken from it.
fn read_part(fs: &dyn pe2_io::Filesystem, dir: &HostPath, before: Option<&Read>, local: bool) -> Result<Read> {
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
    // What waits is taken as it waits, never from its file - which the storage may be replacing as it is read, and
    // which, on Windows, cannot be opened meanwhile.
    let waiting_at: std::collections::HashSet<HostPath> = waiting.iter().map(|(path, ..)| path.clone()).collect();
    let read_shard = |shard: &HostPath| -> Result<Vec<(Uid, Held)>> {
        let mut out = Vec::new();
        for file in fs.read_directory(shard)? {
            let path = shard.entry(&file.name);
            let Some(uid) = uid_of(dir, &path) else { continue };
            if waiting_at.contains(&path) {
                continue;
            }
            let stat = fs.stat_link(&path)?;
            let (modified, len) = (stat.modified.ok_or_else(|| no_time(&path))?, stat.size);
            let same = |h: &&Held| h.modified == modified && h.len == len && h.record.local == local;
            if let Some(held) = before.and_then(|b| b.get(&uid)).filter(same) {
                out.push((uid, held.clone()));
                continue;
            }
            let text = fs.read_to_string(&path)?;
            let record = Record::parse(&text).with_context(|| path.display().to_string())?;
            out.push((uid, Held { record: std::sync::Arc::new(Record { local, ..record }), modified, len }));
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
                waits(&mut out, uid, &text, at, local).with_context(|| path.display().to_string())?;
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
            let (folder, local) = (&layer.records, layer.local.as_ref());
            let before = previous.as_ref().and_then(|p| p.views.iter().find(|v| v.dir == layer.path));
            let layer_touched = touched(folder) || local.is_some_and(touched);
            if let Some(before) = before.filter(|_| !below_changed && !layer_touched) {
                // As it was read: what it places is there for the layers above, as it was.
                places.layers.push(before.named.clone());
                views.push(before.clone());
                continue;
            }
            below_changed = true;
            // A layer whose records are none yet - a generated one before its plan first ran - holds nothing.
            let settings = if fs.exists(folder) { settings(fs, folder)? } else { Settings::default() };
            // The records that changed read again, where the paths say which; else all of them looked at.
            let after =
                before.zip(changed).and_then(|(view, paths)| read_changed(fs, folder, local, &view.records, paths));
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
                (None, Some(known)) => read_layer(fs, folder, local, Some(&known))?,
                (None, None) => read_layer(fs, folder, local, before.map(|v| &v.records))?,
            };
            // This machine's alone, where its records are in the local folder: what they keep is in the local store.
            let store = if folder.starts_with(&crate::local::part(&project.root, crate::local::LAYERS)) {
                crate::chunks::Store::local(&project.root, project.io.clone())
            } else {
                crate::chunks::Store::at(&project.root, project.io.clone())
            };
            let mut view = View {
                settings,
                external: layer.mounted,
                local: layer.local.clone(),
                ..View::new(layer.path.clone(), folder.clone(), store)
            };
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

    /// What each layer records of the sealed nodes (`Record::tree`), bottom to top: each record of one, the records
    /// deleting one among them, with the layer's records' folder and the node's tree where the record has one.
    #[must_use]
    pub fn sealed_records(&self) -> Vec<SealedRecord> {
        let sealed: std::collections::HashSet<Uid> = self
            .views
            .iter()
            .flat_map(|view| view.records.iter())
            .filter(|(_, held)| held.record.tree.is_some())
            .map(|(uid, _)| *uid)
            .collect();
        let mut out = Vec::new();
        for view in self.views.iter().filter(|view| !view.external) {
            for (uid, held) in view.records.iter().filter(|(uid, _)| sealed.contains(*uid)) {
                let tree = held.record.tree.as_ref().and_then(|tree| view.trees.get(tree)).cloned();
                out.push(SealedRecord { folder: view.folder.clone(), uid: *uid, record: held.record.clone(), tree });
            }
        }
        out
    }

    /// The entry the stack places at a path, as the highest layer placing one there has it.
    #[must_use]
    pub fn placed_at(&self, path: &TreePath) -> Option<Uid> {
        let mut found = None;
        for view in self.views.iter().filter(|view| !view.external) {
            for (uid, held) in &view.records {
                if held.record.op == Op::Delete && found == Some(*uid) {
                    found = None;
                } else if view.placed.get(uid).is_some_and(|at| **at == *path) {
                    found = Some(*uid);
                }
            }
        }
        found
    }

    /// The records of the layer whose tree is at `dir` that a merge left with both sides (`Record::theirs`), each with
    /// where its entry is.
    #[must_use]
    pub fn merging(&self, dir: &HostPath) -> Vec<(Uid, Option<Arc<TreePath>>)> {
        let Some(view) = self.views.iter().find(|view| view.dir == *dir) else { return Vec::new() };
        let merging = view.records.iter().filter(|(_, held)| held.record.theirs.is_some());
        merging.map(|(uid, _)| (*uid, view.placed.get(uid).cloned())).collect()
    }

    /// Whether a layer keeps an entry in this machine's part of it (`Record::local`).
    #[must_use]
    pub fn is_local(&self, uid: Uid) -> bool {
        self.views.iter().any(|view| view.records.get(&uid).is_some_and(|held| held.record.local))
    }

    /// Whether an entry is a sealed node (`Record::tree`): kept whole, replaced or deleted, never written into.
    #[must_use]
    pub fn is_sealed(&self, uid: Uid) -> bool {
        self.views.iter().any(|view| view.records.get(&uid).is_some_and(|held| held.record.tree.is_some()))
    }

    /// Every uid a layer's records name.
    #[must_use]
    pub fn uids(&self) -> std::collections::HashSet<Uid> {
        self.views.iter().flat_map(|view| view.records.keys().copied()).collect()
    }

    /// The sealed nodes the stack places (`Record::tree`), each as the highest layer recording it has it - none where
    /// it deletes it - by where its files are read (its view's tree, on no disk), with its tree.
    #[must_use]
    pub fn sealed(&self) -> Vec<(HostPath, std::sync::Arc<crate::chunks::Tree>)> {
        let mut found: BTreeMap<Uid, Option<(HostPath, std::sync::Arc<crate::chunks::Tree>)>> = BTreeMap::new();
        for view in self.views.iter().filter(|view| !view.external) {
            for (uid, held) in &view.records {
                match (held.record.op, &held.record.tree) {
                    (Op::Delete, _) => drop(found.insert(*uid, None)),
                    (Op::Create | Op::Override, Some(tree)) => {
                        let at = view.placed.get(uid).zip(view.trees.get(tree));
                        let sealed = at.map(|(at, tree)| (view.dir.join(&at.rel()), tree.clone()));
                        found.insert(*uid, sealed);
                    }
                    _ => {}
                }
            }
        }
        found.into_values().flatten().collect()
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
            // Each part of its records: the shipped one, and this machine's.
            let parts = std::iter::once((&view.folder, false)).chain(view.local.iter().map(|local| (local, true)));
            for (records_dir, local) in parts {
                let mut found = BTreeSet::new();
                let fs = self.store.io.filesystem();
                let shards = fs.read_directory(records_dir).unwrap_or_default();
                for shard in shards.iter().filter(|shard| shard.kind == pe2_io::Kind::Folder && is_shard(&shard.name)) {
                    let shard = records_dir.entry(&shard.name);
                    for file in fs.read_directory(&shard).unwrap_or_default() {
                        let path = shard.entry(&file.name);
                        let Some(uid) = uid_of(records_dir, &path) else { continue };
                        let stamp = fs.stat_link(&path).ok().and_then(|stat| Some((stat.modified?, stat.size)));
                        let same = view.records.get(&uid).zip(stamp).is_some_and(|(held, (modified, len))| {
                            held.record.local == local && held.modified == modified && held.len == len
                        });
                        // A shipped record a local one hides is not what was read, and says nothing.
                        let hidden = !local && view.records.get(&uid).is_some_and(|held| held.record.local);
                        if !same && !hidden {
                            differing.push(path);
                        }
                        found.insert(uid);
                    }
                }
                // A record read is of a uid whose file can be named: written so, read back so.
                let gone =
                    view.records.iter().filter(|(uid, held)| held.record.local == local && !found.contains(*uid));
                differing.extend(gone.filter_map(|(uid, _)| path_of(records_dir, *uid).ok()));
            }
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
        Some((view.content(*uid, name)?, held.modified, view.store.clone()))
    }

    /// Where the content of a file of a records layer lives, for what serves it to read it without the records: the
    /// record holding its text, or the store keeping it.
    #[must_use]
    pub fn source(path: &HostPath) -> Option<crate::server::Source> {
        use crate::server::Source;
        let (set, i, rel) = Set::view_of(path)?;
        let view = &set.views[i];
        let (uid, name) = view.files.get(&rel)?;
        Some(match view.content(*uid, name)? {
            Content::Text(_) => {
                let local = view.records.get(uid).is_some_and(|held| held.record.local);
                Source::RecordText { record: view.record_path(*uid, local)?, name: name.to_string() }
            }
            Content::Stored { sha1, .. } => Source::Stored { sha1, store: view.store.dir.clone() },
            Content::Object { sha1, size } => Source::Object { sha1, size, store: view.store.dir.clone() },
            // Made whole once, and read so.
            delta @ Content::Delta { .. } => {
                let sha1 = made(&delta, &view.store).ok()??;
                Source::Stored { sha1, store: view.store.holding_file(&sha1)?.dir }
            }
        })
    }

    /// The sealed node a records layer shows at a path (`Record::tree`): the layer's records' folder and its tree. None
    /// where the path is no sealed node's.
    #[must_use]
    pub fn sealed_at(path: &HostPath) -> Option<(HostPath, pe2_ext::Sha1)> {
        let (set, i, rel) = Set::view_of(path)?;
        let view = &set.views[i];
        let (uid, _) = view.placed.iter().find(|(_, at)| ***at == *rel)?;
        Some((view.folder.clone(), view.records.get(uid)?.record.tree?))
    }

    /// A file of a records layer's length and when its record was written, without its content.
    #[must_use]
    pub fn stat(path: &HostPath) -> Option<(u64, std::time::SystemTime)> {
        let (set, i, rel) = Set::view_of(path)?;
        let view = &set.views[i];
        let (uid, name) = view.files.get(&rel)?;
        let held = view.records.get(uid)?;
        Some((view.content(*uid, name)?.size(), held.modified))
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
                    let mut files: BTreeMap<_, _> = record
                        .files
                        .iter()
                        .map(|(name, f)| {
                            let base = crate::changeset::Base { was: f.base, removed: f.content.is_none() };
                            (name.clone(), base)
                        })
                        .collect();
                    // A sealed node's tree replaces the version below's files whole: each of its own is the layer's.
                    let tree = record.tree.as_ref().and_then(|tree| view.trees.get(tree));
                    for name in tree.into_iter().flat_map(|tree| tree.files.keys()) {
                        if let Ok(name) = RelPath::new(name) {
                            files
                                .entry(name.to_owned())
                                .or_insert(crate::changeset::Base { was: None, removed: false });
                        }
                    }
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
    // The extensions added to the layer are no part of the tree a tool hands over: kept, as the layer holds them.
    let records = with_sealed(&read_records(fs, &layer.records, None)?, tree.records);
    let said = Settings { read_only: listing.read_only, ignore: listing.ignore.clone() };
    // What changes, and only that: each record that differs from what the layer has, those it no longer has gone, its
    // settings where they differ.
    replace(project, &layer.records, &said, &records, &format!("Import the layer {name}"))?;
    Ok(records.len())
}

/// Settle a record a merge left with both sides (`Record::theirs`): keep this one, or take the other (`theirs`), whole.
/// One step of the history; the record's path.
///
/// # Errors
/// The layer has no such record, or none a merge left so, or the other side is no record this program reads.
pub fn settle(
    project: &crate::Project,
    layer: &crate::Layer,
    uid: Uid,
    theirs: bool,
    history: &mut crate::history::History,
) -> Result<Vec<HostPath>> {
    let set = Set::current(&project.root).map_or_else(|| Set::open(project), Ok)?;
    let view =
        set.views.iter().find(|view| view.dir == layer.path).with_context(|| format!("{} is read", layer.name))?;
    let held = view.records.get(&uid).with_context(|| format!("{} has no record of {uid}", layer.name))?;
    let other = held.record.theirs.as_deref().with_context(|| format!("{uid}: no merge left both sides of it"))?;
    let text = if theirs {
        // Read as a record is, or refused: what is written is one.
        Record::parse(other).with_context(|| format!("{uid}: the other side"))?;
        other.to_string()
    } else {
        Record { theirs: None, ..(*held.record).clone() }.render()
    };
    let path = layer.record_path(uid, held.record.local)?;
    let label = if theirs { format!("Take the other side of {uid}") } else { format!("Keep this side of {uid}") };
    let mut tx = history.begin(label);
    tx.write(&path, text.as_bytes())?;
    tx.commit()?;
    Ok(vec![path])
}

/// Keep an entry of a layer in this machine's part of it (`local`), never shipped, with all that is in it - or share it
/// (`!local`), with the folders above it the layer keeps this machine's: a shipped entry is never in one that is not.
/// What a shared record keeps by hash is put in the shipped store. One step of the history; the paths written.
///
/// # Errors
/// The layer has no part this machine's alone or no record of the entry, a folder above it is another layer's and this
/// machine's, or the records cannot be written.
pub fn set_local(
    project: &crate::Project,
    layer: &crate::Layer,
    path: &TreePath,
    local: bool,
    history: &mut crate::history::History,
) -> Result<Vec<HostPath>> {
    let set = Set::current(&project.root).map_or_else(|| Set::open(project), Ok)?;
    let view =
        set.views.iter().find(|view| view.dir == layer.path).with_context(|| format!("{} is read", layer.name))?;
    let at_path = |at: &TreePath| view.placed.iter().find(|(_, placed)| ***placed == *at).map(|(uid, _)| *uid);
    let uid = at_path(path).with_context(|| format!("{} records nothing at {path}", layer.name))?;
    let mut moved: Vec<Uid> = Vec::new();
    if local {
        // It, and all in it.
        moved.extend(view.placed.iter().filter(|(_, placed)| placed.starts_with(path)).map(|(uid, _)| *uid));
    } else {
        moved.push(uid);
        // The folders above it this machine's: the layer's, shared with it; another layer's cannot be.
        let mut above = path.parent();
        while let Some(folder) = above.filter(|folder| !folder.is_root()) {
            match at_path(folder) {
                Some(uid) if view.records.get(&uid).is_some_and(|held| held.record.local) => moved.push(uid),
                None if set.placed_at(folder).is_some_and(|uid| set.is_local(uid)) => {
                    bail!("{folder} is another layer's and this machine's alone: share it there first")
                }
                _ => {}
            }
            above = folder.parent();
        }
    }
    let shipped = crate::chunks::Store::at(&project.root, project.io.clone());
    let local_store = crate::chunks::Store::local(&project.root, project.io.clone());
    let label = if local { format!("Keep {path} on this computer only") } else { format!("Share {path}") };
    let mut tx = history.begin(label);
    let mut written = Vec::new();
    for uid in moved {
        let Some(held) = view.records.get(&uid).filter(|held| held.record.local != local) else { continue };
        if !local {
            // What it keeps by hash, where it is shipped.
            for file in held.record.files.values() {
                if let Some(Content::Stored { sha1, .. }) = &file.content
                    && !shipped.has_file(sha1)
                {
                    local_store.copy_file(sha1, &shipped)?;
                }
            }
        }
        let (from, to) = (layer.record_path(uid, !local)?, layer.record_path(uid, local)?);
        tx.write(&to, Record { local, ..(*held.record).clone() }.render().as_bytes())?;
        tx.remove(&from)?;
        written.extend([from, to]);
    }
    tx.commit()?;
    Ok(written)
}

/// What an import makes (`records`), with the sealed nodes the layer holds (`old`) that it does not make - an extension
/// added to the layer - and the folders each is placed in: one the import has at the same place is the import's.
fn with_sealed(old: &Read, mut records: BTreeMap<Uid, Record>) -> BTreeMap<Uid, Record> {
    let mut made: std::collections::HashMap<(Option<Uid>, Arc<Name>), Uid> =
        records.iter().filter_map(|(uid, r)| Some(((r.parent, r.name.clone()?), *uid))).collect();
    // What each folder kept above a sealed node is now: the import's at its place, else itself.
    let mut now: std::collections::HashMap<Uid, Option<Uid>> = std::collections::HashMap::new();
    let sealed: Vec<Uid> = old
        .iter()
        .filter(|(uid, held)| held.record.tree.is_some() && !records.contains_key(*uid))
        .map(|(uid, _)| *uid)
        .collect();
    for uid in sealed {
        // Its folders, from the top down.
        let mut chain = Vec::new();
        let mut at = old[&uid].record.parent;
        while let Some(parent) = at.filter(|parent| !chain.contains(parent)) {
            chain.push(parent);
            at = old.get(&parent).and_then(|held| held.record.parent);
        }
        let mut parent = None;
        for folder in chain.into_iter().rev() {
            if let Some(known) = now.get(&folder) {
                parent = *known;
                continue;
            }
            let Some(held) = old.get(&folder) else { break };
            let placed = held.record.name.clone().map(|name| (parent, name));
            let is = if let Some(theirs) = placed.as_ref().and_then(|place| made.get(place)) {
                *theirs
            } else {
                records.insert(folder, Record { parent, ..(*held.record).clone() });
                if let Some(place) = placed {
                    made.insert(place, folder);
                }
                folder
            };
            now.insert(folder, Some(is));
            parent = Some(is);
        }
        let record = &old[&uid].record;
        let parent = if record.parent.is_some() { parent } else { None };
        records.insert(uid, Record { parent, ..(**record).clone() });
    }
    records
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
        records.insert(
            *uid,
            Record {
                op: Op::Create,
                kind,
                parent,
                name,
                files: BTreeMap::new(),
                tree: None,
                local: false,
                theirs: None,
            },
        );
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
            // A folder holding an extension's manifest is a sealed node, kept whole as its tree: an extension, by what
            // it holds, wherever it is.
            let sealed =
                entry.kind == pe2_io::Kind::Folder && self.fs.is_file(&disk.join(&crate::ext::folder::MANIFEST));
            let record = match entry.kind {
                pe2_io::Kind::Folder if sealed => {
                    let (tree, _) = self.store.put_folder(&disk).with_context(|| disk.display().to_string())?;
                    // Named as a node's folder is, without `.node`; any other folder by its name.
                    let named = if node { crate::index::asset_named(&at)? } else { at.clone() };
                    let name = named.last().with_context(|| format!("{at}: no name"))?.to_owned();
                    let files = BTreeMap::new();
                    Record {
                        op: Op::Create,
                        kind: Kind::Node,
                        parent,
                        name: Some(name),
                        files,
                        tree: Some(tree),
                        local: false,
                        theirs: None,
                    }
                }
                pe2_io::Kind::Folder if at.as_str().ends_with(crate::index::BUNDLE_SUFFIX) => {
                    let mut files = BTreeMap::new();
                    self.files(&disk, RelPath::root(), &mut files)?;
                    let named = crate::index::asset_named(&at)?;
                    let name = named.last().with_context(|| format!("{at}: no name"))?.to_owned();
                    Record {
                        op: Op::Create,
                        kind: Kind::Node,
                        parent,
                        name: Some(name),
                        files,
                        tree: None,
                        local: false,
                        theirs: None,
                    }
                }
                pe2_io::Kind::Folder => {
                    self.folder(&disk, &at, Some(uid))?;
                    Record {
                        op: Op::Create,
                        kind: Kind::Folder,
                        parent,
                        name: Some(name.to_owned()),
                        files: BTreeMap::new(),
                        tree: None,
                        local: false,
                        theirs: None,
                    }
                }
                _ => {
                    let content =
                        Content::of(&self.fs.read(&disk).with_context(|| disk.display().to_string())?, self.store)?;
                    let files =
                        BTreeMap::from([(RelPath::root().to_owned(), File { content: Some(content), base: None })]);
                    Record {
                        op: Op::Create,
                        kind: Kind::File,
                        parent,
                        name: Some(name.to_owned()),
                        files,
                        tree: None,
                        local: false,
                        theirs: None,
                    }
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
    // The extensions added to the layer are no part of the tree it hands over, as an import keeps them: its sealed
    // nodes, and the folders that hold nothing else, are left out.
    let records = Set::records_of(&layer.path).unwrap_or_default();
    let sealed: Vec<Arc<TreePath>> = records
        .iter()
        .filter(|(_, record)| record.tree.is_some())
        .filter_map(|(uid, _)| changeset.entries.get(uid).cloned())
        .collect();
    let paths: Vec<Arc<TreePath>> = changeset.entries.values().cloned().collect();
    changeset.entries.retain(|_, path| {
        let within = |p: &Arc<TreePath>| p.starts_with(path) && **p != **path;
        let below: Vec<&Arc<TreePath>> = paths.iter().filter(|p| within(p)).collect();
        let left = sealed.iter().any(|s| path.starts_with(s))
            || (!below.is_empty() && below.iter().all(|p| sealed.iter().any(|s| p.starts_with(s))));
        !left
    });
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
#[expect(clippy::too_many_lines, reason = "each kind of record an edit makes, in its part, in turn")]
pub fn records_for(
    project: &crate::Project,
    index: &crate::index::Index,
    layer: &crate::Layer,
    changeset: &crate::changeset::Changeset,
    old: &BTreeMap<Uid, Record>,
    written: &BTreeMap<Arc<TreePath>, Option<Vec<u8>>>,
) -> Result<(BTreeMap<Uid, Record>, Vec<Uid>)> {
    use crate::changeset::Op as Did;
    let shipped = crate::chunks::Store::at(&project.root, project.io.clone());
    let local_store = crate::chunks::Store::local(&project.root, project.io.clone());
    // What each record's content is kept in: the store of its part.
    let store_of = |local: bool| if local { &local_store } else { &shipped };
    // Whether an entry another layer has is this machine's.
    let others = Set::current(&project.root);
    let local_elsewhere = |uid: Uid| others.as_ref().is_some_and(|set| set.is_local(uid));
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
    let mut new: BTreeMap<Uid, Record> = BTreeMap::new();
    // Whether an entry is this machine's by where it is: under one that is, or - made or moved there - named by a
    // pattern of the project's `[local]`.
    let local_at = |new: &BTreeMap<Uid, Record>, path: &TreePath, parent: Option<Uid>, placed: bool| {
        let parent_local = parent.is_some_and(|parent| {
            new.get(&parent).or_else(|| old.get(&parent)).map_or_else(|| local_elsewhere(parent), |r| r.local)
        });
        parent_local || (placed && crate::changeset::ignored(&project.local, path.as_str()))
    };
    // The layer's own entries from the top down: a folder's part is known before what is in it.
    let mut entries: Vec<(&Uid, &Arc<TreePath>)> = changeset.entries.iter().collect();
    entries.sort_by_key(|(_, path)| path.parts().count());
    for (uid, path) in entries {
        let before = old.get(uid).filter(|r| r.op == Op::Create);
        let kind = before.map_or_else(|| new_kind(path, written), |r| r.kind);
        let (parent, name) = place(path)?;
        let placed = before.is_none_or(|r| (r.parent, r.name.as_ref()) != (parent, Some(&name)));
        let local = before.is_some_and(|r| r.local) || local_at(&new, path, parent, placed);
        let mut files = before.map_or_else(BTreeMap::new, |r| r.files.clone());
        let tree = before.and_then(|r| r.tree);
        for (file, bytes) in written_at(path, kind) {
            // A sealed node is replaced whole, never written into.
            anyhow::ensure!(tree.is_none(), "{path} is sealed - kept whole, as an extension is - and only read");
            match bytes {
                Some(bytes) => {
                    let content = Content::of(&bytes, store_of(local))?;
                    files.insert(file, File { content: Some(content), base: None })
                }
                None => files.remove(&file),
            };
        }
        new.insert(*uid, Record { op: Op::Create, kind, parent, name: Some(name), files, tree, local, theirs: None });
    }
    for (uid, op) in &changeset.ops {
        let from = below.of(*uid);
        let kind = old.get(uid).map(|r| r.kind).or_else(|| from.map(|(_, k)| of_kind(k))).unwrap_or(Kind::File);
        // What a layer does to an entry this machine's alone is this machine's too: shipped, it would name what others
        // have not.
        let theirs_local = old.get(uid).is_some_and(|r| r.local) || local_elsewhere(*uid);
        let record = match op {
            Did::Delete => {
                let files = BTreeMap::new();
                Record {
                    op: Op::Delete,
                    kind,
                    parent: None,
                    name: None,
                    files,
                    tree: None,
                    local: theirs_local,
                    theirs: None,
                }
            }
            Did::Move { to } => {
                let (parent, name) = place(to)?;
                let local = theirs_local || local_at(&new, to, parent, true);
                Record {
                    op: Op::Move,
                    kind,
                    parent,
                    name: Some(name),
                    files: BTreeMap::new(),
                    tree: None,
                    local,
                    theirs: None,
                }
            }
            Did::Override { files: bases, to } => {
                let at = to.as_deref().or(from.map(|(p, _)| p)).context("an override of nothing")?.to_owned();
                let (parent, name) = match to {
                    Some(to) => place(to).map(|(p, n)| (p, Some(n)))?,
                    None => (None, None),
                };
                let now: BTreeMap<Arc<RelPath>, Option<Vec<u8>>> = written_at(&at, kind).into_iter().collect();
                let held = old.get(uid).filter(|r| r.op == Op::Override);
                let local = theirs_local || to.as_deref().is_some_and(|to| local_at(&new, to, parent, true));
                // A sealed node's override keeps its tree, and what it removes of below's, as they were.
                let tree = held.and_then(|r| r.tree);
                anyhow::ensure!(tree.is_none() || now.is_empty(), "{at} is sealed - kept whole - and only read");
                let files = match held.filter(|_| tree.is_some()) {
                    Some(sealed) => sealed.files.clone(),
                    None => override_files(bases, now, held.map(|r| &r.files), store_of(local))?,
                };
                Record { op: Op::Override, kind, parent, name, files, tree, local, theirs: None }
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

/// What an entry the layer makes anew is: a node by its folder's name, a file where one was written at its path, else
/// a folder.
fn new_kind(path: &TreePath, written: &BTreeMap<Arc<TreePath>, Option<Vec<u8>>>) -> Kind {
    if path.as_str().ends_with(crate::index::BUNDLE_SUFFIX) {
        Kind::Node
    } else if written.get(path).is_some_and(Option::is_some) {
        Kind::File
    } else {
        Kind::Folder
    }
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
                // What it changes of the version below it was made from.
                Some(Some(bytes)) => Some(Content::over(&bytes, base.was, file.as_str(), store)?),
                _ => match held.and_then(|h| h.get(file)).and_then(|f| f.content.clone()) {
                    // Made from another version than now: what it changes said again of this one.
                    Some(delta @ Content::Delta { base: from, .. }) if Some(from) != base.was => {
                        Some(Content::over(&delta.bytes(store)?, base.was, file.as_str(), store)?)
                    }
                    content => content,
                },
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
            tree: None,
            local: false,
            theirs: None,
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

    /// Every file under a folder, by its path within it, with its bytes.
    fn files(dir: &std::path::Path) -> std::collections::BTreeMap<String, Vec<u8>> {
        let mut out = std::collections::BTreeMap::new();
        let mut pending = vec![(dir.to_path_buf(), String::new())];
        while let Some((at, within)) = pending.pop() {
            for entry in std::fs::read_dir(&at).into_iter().flatten().flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
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
    fn an_import_keeps_the_extensions_added_to_its_layer_and_an_export_leaves_them_out() {
        let (root, tree) = staged("import-sealed");
        let layer = crate::testing::name("project");
        let project = crate::Project::load(&root).unwrap();
        super::import(&project, &layer, tree.as_path()).unwrap();
        // An extension added to the layer, in a folder of its own and in one the tree has.
        let folder = root.at("kept/my.tool");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.at("extension.toml"), "id = \"my.tool\"\nversion = \"0.1.0\"\n").unwrap();
        let store = crate::chunks::Store::at(&root, crate::testing::io());
        let (sealed, _) = store.put_folder(&sfs::HostPath::new(folder.as_path()).unwrap()).unwrap();
        let mine = |name: &str, parent: Option<pe2_path::Uid>| super::Record {
            op: super::Op::Create,
            kind: if parent.is_some() { super::Kind::Node } else { super::Kind::Folder },
            parent,
            name: Some(pe2_path::Name::new(name).unwrap().to_owned()),
            files: std::collections::BTreeMap::new(),
            tree: parent.map(|_| sealed),
            local: false,
            theirs: None,
        };
        let records = root.at("layers/project");
        let write = |uid: &str, record: &super::Record| {
            let at = super::path_of(&records, crate::testing::uid(uid)).unwrap();
            std::fs::create_dir_all(at.parent().unwrap()).unwrap();
            std::fs::write(at, record.render()).unwrap();
        };
        write("uid://e1", &mine("extensions", None));
        write("uid://e2", &mine("my.tool", Some(crate::testing::uid("uid://e1"))));
        write("uid://e3", &mine("tool", Some(crate::testing::uid("uid://3g"))));
        // Imported again: what the tree has is the layer's, its extensions kept where they were.
        super::import(&project, &layer, tree.as_path()).unwrap();
        let project = crate::Project::load(&root).unwrap();
        super::Set::open(&project).unwrap();
        let held = project.held_extensions();
        let at = |path: &str| root.at("layers/project/tree").at(path);
        assert_eq!(held, [at("extensions/my.tool.node"), at("g/tool.node")]);
        // Exported: the tree as it was handed over, with no extension in it, nor the folder holding nothing else.
        let out = root.at("staging/out");
        super::export(&project, &layer, out.as_path()).unwrap();
        assert_eq!(files(out.as_path()), files(tree.as_path()));
        crate::local::remove_all(&root);
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
