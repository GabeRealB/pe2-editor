//! The project's model where its processes share it: what the index reads of the project's files, and what it
//! derives of them, in trees of the shared space (`shared::space`) as of the last commit - for whoever holds the
//! project next to read in place, where each built it again from the files. Nothing in it is the only copy of
//! anything: the files are what is so, and a model of another format, or of a space begun anew, is filled again.
//!
//! What a refresh changes of it is gathered (`Delta`) and applied whole, in the commit that says what is served
//! after the refresh: a refresh that fails leaves the model as it was, and nobody waits on a refresh for the
//! writer's lock. Every value begins with the generation of the commit that put it: what a process keeps of a
//! value it decoded is told from another by it.
//!
//! So far it holds each file's record (`Record`), what each layer holds (`Held`, by layer and path: its folders,
//! plain files and bundles, as a walk of it finds them) and the records of the layers stored as records
//! (`records::Record`, by layer and uid, each with its file's time and length): what the index keeps between
//! runs, where it kept a database of its own, walked every layer and read every record's file as it opened. And
//! of what the index derives of those, the stack applied (`Stack`: each entry it resolves, by its path, and what
//! it says of the layers' operations, what does not add up, and what the layers need of each other), which
//! whoever takes the project takes as it is where nothing it is made of differs. An index opens over what the model holds of the files (`known`) - or, where the space was begun anew
//! because a process died or the power went, over what that space kept beside it, each record held to its file -
//! and whoever holds the project says back what the opening read anew (`took`).

use pe2_path::{Name, Uid};
use sfs::HostPath;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use sfs::space::{Pages, Result, Txn};

/// A modification time as seconds and nanoseconds since the epoch.
pub type Stamp = (u64, u32);

/// What the index needs of an `node.toml` without parsing it again.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Declaration {
    /// What of its stack says what it makes.
    pub shape: String,
    /// The uid it names itself, as declarations did before changesets.
    pub uid: Option<String>,
    /// Its `variants`' names, in order.
    pub variants: Vec<String>,
    /// Its references, in the order it writes them.
    pub references: Box<[Declared]>,
    /// Why it cannot be parsed.
    pub error: Option<String>,
    pub text: Arc<str>,
}

/// A reference a declaration makes: the slot or field it is in, the asset it names, and what of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Declared {
    pub slot: Box<str>,
    /// The asset's uid; where what is written there is no uid, that text, kept to say the reference leads nowhere.
    pub uid: std::result::Result<Uid, Box<str>>,
    /// The bundle's path written beside the uid (`{ uid, path = "res://<path>.node" }`), where one is.
    pub target: Option<Arc<crate::TreePath>>,
    /// What of the asset it names, and how that is asked for (`palettes/default`); none for the asset itself.
    pub output: Option<Arc<pe2_path::Asked>>,
}

/// What the index knows of a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub size: u64,
    pub modified: Stamp,
    /// When it was read: a record is trusted only for a file last modified before (git's "racily clean" case).
    pub recorded: Stamp,
    pub sha1: pe2_ext::Sha1,
    pub declaration: Option<Declaration>,
}

/// The tree of what is read: the model's own word of itself, and each file's record and text.
pub const INPUTS: usize = 4;
/// The trees of what is to come: the layers' records, the stack, the assets, their links.
pub const RECORDS: usize = 5;
pub const STACK: usize = 6;
pub const ASSETS: usize = 7;
pub const LINKS: usize = 12;

/// The model's format: a model of another is emptied, and filled again - as is one of another schema of the project's
/// outputs (`local::SCHEMA`), and one whose names were compared by another version of Unicode
/// (`pe2_path::UNICODE_VERSION`), by which names are one name. 1 until the editor's first release: a model of an older
/// shape is made again, never told apart.
const FORMAT: u32 = 1;

/// What a key of `INPUTS` begins with: the model's word of itself; a file's record; a declaration's text; a layer,
/// with what its listing depends on; something a layer holds.
const META: u8 = b'm';
const FILE: u8 = b'f';
const TEXT: u8 = b't';
const LAYER: u8 = b'l';
const HELD: u8 = b's';

/// The longest name a key holds itself: a longer one is in the key as its hash, and in the value whole.
const NAMED: usize = 700;

/// What a file's record says besides its numbers.
mod says {
    /// It is a declaration's.
    pub const DECLARATION: u8 = 1;
    /// Its name is too long for its key, and follows the record.
    pub const NAMED: u8 = 2;
    /// Its hash is twenty bytes, as a SHA-1 is.
    pub const SHA1: u8 = 4;
}

fn invalid(what: &str) -> sfs::space::Error {
    sfs::space::Error::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("the project's model: {what}")))
}

/// A value, written.
pub(crate) struct Out(pub(crate) Vec<u8>);

impl Out {
    /// A value that begins with the generation of the commit that puts it, said as it is applied.
    pub(crate) fn new() -> Out {
        Out(vec![0; 8])
    }

    /// A value's own bytes, with nothing before them: what a change of the model is given, and stamps itself.
    pub(crate) fn bare() -> Out {
        Out(Vec::new())
    }

    pub(crate) fn u8(&mut self, it: u8) {
        self.0.push(it);
    }

    pub(crate) fn u32(&mut self, it: u32) {
        self.0.extend_from_slice(&it.to_le_bytes());
    }

    pub(crate) fn u64(&mut self, it: u64) {
        self.0.extend_from_slice(&it.to_le_bytes());
    }

    pub(crate) fn bytes(&mut self, it: &[u8]) {
        self.u32(u32::try_from(it.len()).unwrap_or(u32::MAX));
        self.0.extend_from_slice(it);
    }

    pub(crate) fn text(&mut self, it: &str) {
        self.bytes(it.as_bytes());
    }

    pub(crate) fn maybe(&mut self, it: Option<&str>) {
        self.u8(u8::from(it.is_some()));
        if let Some(it) = it {
            self.text(it);
        }
    }
}

/// A value, read.
pub(crate) struct In<'a>(pub(crate) &'a [u8]);

impl<'a> In<'a> {
    /// What a value holds after the generation that put it.
    pub(crate) fn after_stamp(value: &'a [u8]) -> Option<In<'a>> {
        value.get(8..).map(In)
    }

    pub(crate) fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let (taken, rest) = self.0.split_at_checked(len)?;
        self.0 = rest;
        Some(taken)
    }

    pub(crate) fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|it| it[0])
    }

    pub(crate) fn u32(&mut self) -> Option<u32> {
        self.take(4).and_then(|it| it.try_into().ok()).map(u32::from_le_bytes)
    }

    pub(crate) fn u64(&mut self) -> Option<u64> {
        self.take(8).and_then(|it| it.try_into().ok()).map(u64::from_le_bytes)
    }

    pub(crate) fn bytes(&mut self) -> Option<&'a [u8]> {
        let len = usize::try_from(self.u32()?).ok()?;
        self.take(len)
    }

    pub(crate) fn text(&mut self) -> Option<String> {
        std::str::from_utf8(self.bytes()?).ok().map(str::to_owned)
    }

    /// A text that may be there, read into what is given: none where what is read is no such thing.
    pub(crate) fn maybe(&mut self, into: &mut Option<String>) -> Option<()> {
        *into = match self.u8()? {
            0 => None,
            _ => Some(self.text()?),
        };
        Some(())
    }
}

/// A file's name in the model: where it is in the project's folder, by its parts - the same whatever the folder is
/// called on this system, or where it is - or, for one elsewhere (a mount's), its whole path.
pub(crate) fn name(root: &HostPath, path: &HostPath) -> String {
    // Whole, where it is not within by names: elsewhere, or within by a name no path takes - the same file all the
    // same (`named`).
    match path.within(root) {
        Ok(within) => format!("/{within}"),
        Err(sfs::NotWithin::Outside | sfs::NotWithin::Refused(_)) => format!("={path}"),
    }
}

/// The file a name is of.
pub(crate) fn named(root: &HostPath, name: &str) -> Option<HostPath> {
    if let Some(elsewhere) = name.strip_prefix('=') {
        return HostPath::from_plain(elsewhere.into());
    }
    Some(root.join(&pe2_path::RelPath::new(name.strip_prefix('/')?).ok()?))
}

/// The key of what the model holds of a kind under a name: the name itself, or - too long for a key - its hash.
fn key(kind: u8, name: &str) -> Vec<u8> {
    let mut key = vec![kind];
    if name.len() <= NAMED {
        key.extend_from_slice(name.as_bytes());
    } else {
        // No name begins so: one in the project's folder begins with `/`, one elsewhere with `=`.
        key.push(b'#');
        key.extend_from_slice(crate::extract::sha1_hex(name.as_bytes()).as_bytes());
    }
    key
}

fn stamp(out: &mut Out, (secs, nanos): Stamp) {
    out.u64(secs);
    out.u32(nanos);
}

fn stamped(read: &mut In<'_>) -> Option<Stamp> {
    Some((read.u64()?, read.u32()?))
}

/// A file's record as the model keeps it, under its name: all of it but a declaration's text, which is by itself.
fn record(name: &str, record: &Record) -> Vec<u8> {
    let mut out = Out::new();
    let declaration = record.declaration.as_ref();
    let long = name.len() > NAMED;
    let says = |it: bool, said: u8| if it { said } else { 0 };
    out.u8(says(declaration.is_some(), says::DECLARATION) | says(long, says::NAMED) | says::SHA1);
    out.u64(record.size);
    stamp(&mut out, record.modified);
    stamp(&mut out, record.recorded);
    out.0.extend_from_slice(record.sha1.bytes());
    if let Some(declaration) = declaration {
        out.text(&declaration.shape);
        out.maybe(declaration.uid.as_deref());
        out.u32(u32::try_from(declaration.variants.len()).unwrap_or(u32::MAX));
        for variant in &declaration.variants {
            out.text(variant);
        }
        out.u32(u32::try_from(declaration.references.len()).unwrap_or(u32::MAX));
        // Each as text, as declarations write it: a uid that is none as written, none of the rest as nothing.
        for declared in &declaration.references {
            out.text(&declared.slot);
            out.text(&declared.uid.as_ref().map_or_else(ToString::to_string, ToString::to_string));
            out.text(declared.target.as_deref().map_or("", |target| target.as_str()));
            out.text(&declared.output.as_deref().map_or_default(ToString::to_string));
        }
        out.maybe(declaration.error.as_deref());
    }
    if long {
        out.text(name);
    }
    out.0
}

/// A hash as the model keeps it: its twenty bytes.
fn sha1_in(read: &mut In<'_>) -> Option<pe2_ext::Sha1> {
    Some(pe2_ext::Sha1::from_bytes(read.take(20)?.try_into().ok()?))
}

/// A reference a declaration makes, read back as `record` writes it: one that is not so is no record this reads.
fn declared_in(read: &mut In<'_>) -> Option<Declared> {
    let (slot, uid, target, output) = (read.text()?, read.text()?, read.text()?, read.text()?);
    Some(Declared {
        slot: slot.into(),
        uid: Uid::new(&uid).map_err(|_| uid.into()),
        target: if target.is_empty() { None } else { Some(crate::TreePath::new(&target).ok()?.to_owned()) },
        output: if output.is_empty() { None } else { Some(Arc::from(pe2_path::Asked::new(&output).ok()?)) },
    })
}

/// A file's record read back, with the name it says where its key could not: its declaration's text is to be
/// read by itself.
fn recorded(value: &[u8]) -> Option<(Record, Option<String>)> {
    let mut read = In::after_stamp(value)?;
    let said = read.u8()?;
    let size = read.u64()?;
    let modified = stamped(&mut read)?;
    let recorded = stamped(&mut read)?;
    // A record says its hash in its twenty bytes: one saying it otherwise is of no model this reads.
    if said & says::SHA1 == 0 {
        return None;
    }
    let sha1 = sha1_in(&mut read)?;
    let declaration = if said & says::DECLARATION == 0 {
        None
    } else {
        let shape = read.text()?;
        let (mut uid, mut error) = (None, None);
        read.maybe(&mut uid)?;
        let variants = (0..read.u32()?).map(|_| read.text()).collect::<Option<Vec<_>>>()?;
        let references = (0..read.u32()?).map(|_| declared_in(&mut read)).collect::<Option<Box<[_]>>>()?;
        read.maybe(&mut error)?;
        Some(Declaration { shape, uid, variants, references, error, text: Arc::from("") })
    };
    let name = if said & says::NAMED == 0 { None } else { Some(read.text()?) };
    read.0.is_empty().then_some((Record { size, modified, recorded, sha1, declaration }, name))
}

/// A declaration's text as the model keeps it.
fn text(text: &str) -> Vec<u8> {
    let mut out = Out::new();
    out.0.extend_from_slice(text.as_bytes());
    out.0
}

/// Something a layer holds at a path, as a walk of it finds it: a folder; a plain file, with where it is; a bundle,
/// with its files by name and where each is; or what it holds by a name no path takes (at its path as shown), with
/// why - none of the project's, and a problem until it is renamed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Held {
    Folder,
    File(HostPath),
    Bundle(BTreeMap<Arc<pe2_path::RelPath>, HostPath>),
    Refused(Box<str>),
}

/// What the model holds of the layers: each by its name, with what its listing was made by (its place and the
/// names it leaves out, as the index says them) and what it holds by path.
pub type Layers = BTreeMap<Arc<Name>, (String, BTreeMap<String, Held>)>;

/// A record of a layer stored as records, with its file's time and length as it was read.
pub type Recorded = (Arc<crate::records::Record>, Stamp, u64);
/// What the model holds of the layers stored as records: each layer's records, by its name and their uids.
pub type Records = BTreeMap<Arc<Name>, BTreeMap<Uid, Recorded>>;

/// The stack applied, as the model keeps it: what it was made of besides the files, the layers' listings and
/// their records (the layers as the project says them); each entry it resolves, by its path; and what else it
/// says, by name - all as the index writes them, which the model takes as bytes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stack {
    pub made_of: String,
    pub nodes: BTreeMap<String, Vec<u8>>,
    pub said: BTreeMap<String, Vec<u8>>,
}

/// What is known of a project, for an index to open over (`known`): each file's record, what each layer holds,
/// the records of the layers stored as records, and the stack applied over them.
#[derive(Default)]
pub struct Known {
    pub files: HashMap<HostPath, Record>,
    pub layers: Layers,
    pub records: Records,
    pub stack: Option<Stack>,
}

/// What a key of `STACK` begins with: what the stack was made of; an entry it resolves; what else it says.
const MADE_OF: u8 = b'k';
const NODE: u8 = b'n';
const SAID: u8 = b'b';

/// A value's own bytes as the model keeps them: after the generation of the commit that puts them.
fn after_stamp(bytes: &[u8]) -> Vec<u8> {
    let mut value = vec![0; 8];
    value.extend_from_slice(bytes);
    value
}

/// What a key of `RECORDS` begins with: a record of a layer stored as records.
const RECORD: u8 = b'r';

/// A time as the model keeps it.
#[must_use]
pub fn stamp_of(time: std::time::SystemTime) -> Stamp {
    let since = time.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    (since.as_secs(), since.subsec_nanos())
}

/// The time a stamp is of.
#[must_use]
pub fn time_of((secs, nanos): Stamp) -> std::time::SystemTime {
    std::time::UNIX_EPOCH + std::time::Duration::new(secs, nanos)
}

/// A layer's record as the model keeps it, with its file's time and length.
fn layer_record(name: &str, record: &crate::records::Record, modified: Stamp, len: u64) -> Vec<u8> {
    use crate::records::{Content, Kind, Op};
    let mut out = Out::new();
    let long = name.len() > NAMED;
    out.u8(if long { says::NAMED } else { 0 });
    stamp(&mut out, modified);
    out.u64(len);
    out.u8(match record.op {
        Op::Create => 0,
        Op::Override => 1,
        Op::Move => 2,
        Op::Delete => 3,
    });
    out.u8(match record.kind {
        Kind::Node => 0,
        Kind::Folder => 1,
        Kind::File => 2,
    });
    out.maybe(record.parent.map(|parent| parent.to_string()).as_deref());
    out.maybe(record.name.as_ref().map(|path| path.as_str()));
    out.u32(u32::try_from(record.files.len()).unwrap_or(u32::MAX));
    for (file, held) in &record.files {
        out.text(file.as_str());
        match &held.content {
            None => out.u8(0),
            Some(Content::Text(text)) => {
                out.u8(1);
                out.text(text);
            }
            Some(Content::Stored { sha1, size }) => {
                out.u8(2);
                out.0.extend_from_slice(sha1.bytes());
                out.u64(*size);
            }
            Some(Content::Object { sha1, size }) => {
                out.u8(3);
                out.0.extend_from_slice(sha1.bytes());
                out.u64(*size);
            }
            Some(Content::Delta { sha1, size, base, delta }) => {
                out.u8(4);
                out.0.extend_from_slice(sha1.bytes());
                out.u64(*size);
                out.0.extend_from_slice(base.bytes());
                delta_out(&mut out, delta);
            }
        }
        out.u8(u8::from(held.base.is_some()));
        if let Some(base) = &held.base {
            out.0.extend_from_slice(base.bytes());
        }
    }
    out.u8(u8::from(record.tree.is_some()));
    if let Some(tree) = &record.tree {
        out.0.extend_from_slice(tree.bytes());
    }
    out.u8(u8::from(record.local));
    out.maybe(record.theirs.as_deref());
    if long {
        out.text(name);
    }
    out.0
}

/// What a delta changes, as the model keeps it.
fn delta_out(out: &mut Out, delta: &crate::records::Delta) {
    use crate::merge::fields::Step;
    use crate::records::Delta;
    match delta {
        Delta::Fields(sets) => {
            out.u8(0);
            out.u32(u32::try_from(sets.len()).unwrap_or(u32::MAX));
            for (at, to) in sets {
                out.u32(u32::try_from(at.len()).unwrap_or(u32::MAX));
                for step in at {
                    match step {
                        Step::Key(key) => {
                            out.u8(0);
                            out.text(key);
                        }
                        Step::Id(id) => {
                            out.u8(1);
                            out.text(id);
                        }
                        Step::At(at) => {
                            out.u8(2);
                            out.u64(*at as u64);
                        }
                    }
                }
                out.maybe(to.as_deref());
            }
        }
        Delta::Lines(text) => {
            out.u8(1);
            out.text(text);
        }
        Delta::Bytes(sha1) => {
            out.u8(2);
            out.0.extend_from_slice(sha1.bytes());
        }
    }
}

/// What a delta changes, read back (`delta_out`).
fn delta_in(read: &mut In<'_>) -> Option<crate::records::Delta> {
    use crate::merge::fields::Step;
    use crate::records::Delta;
    Some(match read.u8()? {
        0 => {
            let mut sets = Vec::new();
            for _ in 0..read.u32()? {
                let mut at = Vec::new();
                for _ in 0..read.u32()? {
                    at.push(match read.u8()? {
                        0 => Step::Key(read.text()?),
                        1 => Step::Id(read.text()?),
                        2 => Step::At(usize::try_from(read.u64()?).ok()?),
                        _ => return None,
                    });
                }
                let mut to = None;
                read.maybe(&mut to)?;
                sets.push((at, to));
            }
            Delta::Fields(sets)
        }
        1 => Delta::Lines(read.text()?),
        2 => Delta::Bytes(sha1_in(read)?),
        _ => return None,
    })
}

/// A layer's record read back, with its file's time and length and the name it says where its key could not.
fn layer_recorded(value: &[u8]) -> Option<(Recorded, Option<String>)> {
    use crate::records::{Content, File, Kind, Op, Record};
    let mut read = In::after_stamp(value)?;
    let said = read.u8()?;
    let modified = stamped(&mut read)?;
    let len = read.u64()?;
    let op = [Op::Create, Op::Override, Op::Move, Op::Delete].get(usize::from(read.u8()?)).copied()?;
    let kind = [Kind::Node, Kind::Folder, Kind::File].get(usize::from(read.u8()?)).copied()?;
    // Each name checked as it is read back: a model naming one that is none is no model to open over.
    let (mut parent, mut name): (Option<String>, Option<String>) = (None, None);
    read.maybe(&mut parent)?;
    read.maybe(&mut name)?;
    let parent = match parent {
        Some(parent) => Some(pe2_path::Uid::new(&parent).ok()?),
        None => None,
    };
    let name = match name {
        Some(name) => Some(pe2_path::Name::new(&name).ok()?.to_owned()),
        None => None,
    };
    let mut files = BTreeMap::new();
    for _ in 0..read.u32()? {
        let file = pe2_path::RelPath::new(&read.text()?).ok()?.to_owned();
        let content = match read.u8()? {
            0 => None,
            1 => Some(Content::Text(read.text()?)),
            2 => Some(Content::Stored { sha1: sha1_in(&mut read)?, size: read.u64()? }),
            3 => Some(Content::Object { sha1: sha1_in(&mut read)?, size: read.u64()? }),
            4 => {
                let (sha1, size, base) = (sha1_in(&mut read)?, read.u64()?, sha1_in(&mut read)?);
                Some(Content::Delta { sha1, size, base, delta: delta_in(&mut read)? })
            }
            _ => return None,
        };
        let base = match read.u8()? {
            0 => None,
            _ => Some(sha1_in(&mut read)?),
        };
        files.insert(file, File { content, base });
    }
    let tree = match read.u8()? {
        0 => None,
        _ => Some(sha1_in(&mut read)?),
    };
    let local = read.u8()? != 0;
    let mut theirs = None;
    read.maybe(&mut theirs)?;
    let long = if said & says::NAMED == 0 { None } else { Some(read.text()?) };
    let record = Record { op, kind, parent, name, files, tree, local, theirs };
    read.0.is_empty().then_some(((Arc::new(record), modified, len), long))
}

/// The name of what a layer holds at a path.
fn held_name(layer: &Name, path: &str) -> String {
    format!("{layer}\u{0}{path}")
}

/// Something a layer holds, as the model keeps it under its name.
fn held(root: &HostPath, held_name: &str, held: &Held) -> Vec<u8> {
    let mut out = Out::new();
    let long = if held_name.len() > NAMED { says::NAMED } else { 0 };
    match held {
        Held::Folder => out.u8(long),
        Held::File(path) => {
            out.u8(long | 4);
            out.text(&name(root, path));
        }
        Held::Bundle(files) => {
            out.u8(long | 8);
            out.u32(u32::try_from(files.len()).unwrap_or(u32::MAX));
            for (file, path) in files {
                out.text(file.as_str());
                out.text(&name(root, path));
            }
        }
        Held::Refused(why) => {
            out.u8(long | 0xC);
            out.text(why);
        }
    }
    if long != 0 {
        out.text(held_name);
    }
    out.0
}

/// Something a layer holds read back, with the name it says where its key could not.
fn held_of(root: &HostPath, value: &[u8]) -> Option<(Held, Option<String>)> {
    let mut read = In::after_stamp(value)?;
    let said = read.u8()?;
    let held = match said & !says::NAMED {
        0 => Held::Folder,
        4 => Held::File(named(root, &read.text()?)?),
        8 => {
            // Each name checked as it is read back: a model naming one that is none is no model to open over.
            let file = |read: &mut In| {
                Some((pe2_path::RelPath::new(&read.text()?).ok()?.to_owned(), named(root, &read.text()?)?))
            };
            let files = (0..read.u32()?).map(|_| file(&mut read)).collect::<Option<_>>()?;
            Held::Bundle(files)
        }
        0xC => Held::Refused(read.text()?.into()),
        _ => return None,
    };
    let name = if said & says::NAMED == 0 { None } else { Some(read.text()?) };
    read.0.is_empty().then_some((held, name))
}

/// What a refresh changes of the model, gathered as it goes and applied whole (`apply`): each key of a tree given
/// a value, or taken away, in the order it was said.
#[derive(Clone, Debug, Default)]
pub struct Delta {
    changes: Vec<(usize, Vec<u8>, Option<Vec<u8>>)>,
}

impl Delta {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// How many keys it changes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.changes.len()
    }

    /// A file's record is this now - or, with none, the file is no more.
    pub fn file(&mut self, root: &HostPath, path: &HostPath, now: Option<&Record>) {
        let name = name(root, path);
        let (of_record, of_text) = (key(FILE, &name), key(TEXT, &name));
        self.changes.push((INPUTS, of_record, now.map(|now| record(&name, now))));
        let declared = now.and_then(|now| now.declaration.as_ref());
        self.changes.push((INPUTS, of_text, declared.map(|declared| text(&declared.text))));
    }

    /// A layer holds this at a path now - or, with none, nothing there any more.
    pub fn held(&mut self, root: &HostPath, layer: &Name, path: &str, now: Option<&Held>) {
        let name = held_name(layer, path);
        self.changes.push((INPUTS, key(HELD, &name), now.map(|now| held(root, &name, now))));
    }

    /// A record of a layer stored as records is this now, in a file of this time and length - or, with none, is
    /// no more.
    pub fn record(&mut self, layer: &Name, uid: Uid, now: Option<&Recorded>) {
        let name = held_name(layer, &uid.to_string());
        let said = now.map(|(record, modified, len)| layer_record(&name, record, *modified, *len));
        self.changes.push((RECORDS, key(RECORD, &name), said));
    }

    /// The stack resolves this at a path now, as the index writes it - or, with none, nothing there any more.
    pub fn node(&mut self, path: &str, now: Option<&[u8]>) {
        // A path no key holds is said with what is there.
        let said = now.map(|now| {
            let mut out = Out::new();
            out.maybe((path.len() > NAMED).then_some(path));
            out.0.extend_from_slice(now);
            out.0
        });
        self.changes.push((STACK, key(NODE, path), said));
    }

    /// The stack says this under a name now, as the index writes it.
    pub fn said(&mut self, name: &str, now: &[u8]) {
        self.changes.push((STACK, key(SAID, name), Some(after_stamp(now))));
    }

    /// The stack was made of this, besides what the model holds: the layers, as the project says them.
    pub fn stack_made_of(&mut self, made_of: &str) {
        self.changes.push((STACK, vec![MADE_OF], Some(after_stamp(made_of.as_bytes()))));
    }

    /// A layer's listing was made by this (its place and the names it leaves out) - or, with none, the layer is
    /// no more.
    pub fn layer(&mut self, layer: &Name, made_by: Option<&str>) {
        let said = made_by.map(|made_by| {
            let mut out = Out::new();
            out.text(made_by);
            out.0
        });
        self.changes.push((INPUTS, key(LAYER, layer.as_str()), said));
    }

    /// This, and what a later refresh changed.
    pub fn and(&mut self, later: Delta) {
        self.changes.extend(later.changes);
    }

    /// Apply it, as part of a commit: every value put says the commit's generation.
    ///
    /// # Errors
    /// The space cannot be written.
    pub fn apply(&self, txn: &mut Txn<'_>) -> Result<()> {
        // Part of nothing where the trees hold no model: whoever holds the project fills it first (`fill_files`).
        if self.changes.is_empty() || !is_begun(txn)? {
            return Ok(());
        }
        let generation = txn.generation().to_le_bytes();
        let mut put = Vec::new();
        for (tree, key, value) in &self.changes {
            match value {
                Some(value) => {
                    put.clear();
                    put.extend_from_slice(value);
                    put[..8].copy_from_slice(&generation);
                    sfs::tree::put(txn, *tree, key, &put)?;
                }
                None => drop(sfs::tree::remove(txn, *tree, key)?),
            }
        }
        Ok(())
    }
}

/// The model's trees, in the order they are named.
const TREES: [usize; 5] = [INPUTS, RECORDS, STACK, ASSETS, LINKS];

/// Whether the trees hold a model of this format.
fn is_begun(pages: &impl Pages) -> Result<bool> {
    let said = sfs::tree::get_in(pages, INPUTS, &[META])?;
    let said = said.as_deref().and_then(In::after_stamp);
    let said = said.and_then(|mut said| Some((said.u32()?, said.u32()?, said.text()?)));
    Ok(said.is_some_and(|(format, schema, unicode)| {
        (format, schema, unicode.as_str()) == (FORMAT, crate::local::SCHEMA, pe2_path::UNICODE_VERSION)
    }))
}

/// Have the trees hold a model of this format: one of another, or what a space keeps of none, is taken away, for
/// whoever holds the project to fill again. Whether it held one.
fn begin(txn: &mut Txn<'_>) -> Result<bool> {
    if is_begun(txn)? {
        return Ok(true);
    }
    for tree in TREES {
        let mut keys = Vec::new();
        sfs::tree::scan(&*txn, tree, &[], &mut |key, _| {
            keys.push(key.to_vec());
            true
        })?;
        for key in keys {
            sfs::tree::remove(txn, tree, &key)?;
        }
    }
    sfs::tree::put(txn, INPUTS, &[META], &meta(txn.generation()))?;
    Ok(false)
}

fn meta(generation: u64) -> Vec<u8> {
    let mut out = Out::new();
    out.0[..8].copy_from_slice(&generation.to_le_bytes());
    out.u32(FORMAT);
    out.u32(crate::local::SCHEMA);
    out.text(pe2_path::UNICODE_VERSION);
    out.0
}

/// Have the model hold these files' records and no other: all of them loaded whole where it holds nothing, else
/// what differs put and taken away - what whoever takes a project says of what its index read.
///
/// # Errors
/// The space cannot be written.
pub fn fill_files<'a>(
    txn: &mut Txn<'_>,
    root: &HostPath,
    records: impl Iterator<Item = (&'a HostPath, &'a Record)>,
) -> Result<()> {
    let generation = txn.generation().to_le_bytes();
    let mut wanted: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
    for (path, now) in records {
        let name = name(root, path);
        wanted.insert(key(FILE, &name), record(&name, now));
        if let Some(declared) = &now.declaration {
            wanted.insert(key(TEXT, &name), text(&declared.text));
        }
    }
    for value in wanted.values_mut() {
        value[..8].copy_from_slice(&generation);
    }
    if !begin(txn)? && TREES.iter().all(|tree| *tree == INPUTS || txn.root(*tree) == 0) {
        // Nothing but its word of itself: taken away once more, and all of it loaded in the order of its keys.
        sfs::tree::remove(txn, INPUTS, &[META])?;
        wanted.insert(vec![META], meta(txn.generation()));
        return sfs::tree::load(txn, INPUTS, &mut wanted.into_iter());
    }
    // What it holds of files that differs - but by the commit that put it - or is of none of these.
    let mut held: Vec<(Vec<u8>, bool)> = Vec::new();
    for kind in [FILE, TEXT] {
        sfs::tree::scan_prefix(&*txn, INPUTS, &[kind], &mut |key, value| {
            let same = wanted.get(key).is_some_and(|now| now.get(8..) == value.get(8..));
            held.push((key.to_vec(), same));
            true
        })?;
    }
    for (key, same) in &held {
        match (wanted.remove(key), same) {
            (Some(_), true) => {}
            (Some(now), false) => sfs::tree::put(txn, INPUTS, key, &now)?,
            (None, _) => drop(sfs::tree::remove(txn, INPUTS, key)?),
        }
    }
    for (key, now) in wanted {
        sfs::tree::put(txn, INPUTS, &key, &now)?;
    }
    Ok(())
}

/// What the project's processes last left known of it, for an index to open over - the files' records, and what
/// each layer holds: what the model holds, where the shared space holds one; else what a space that was begun
/// anew - a process died, the power went - kept beside it (`sfs::Space::left_behind`), where that reads whole;
/// else nothing. Whoever opens over it holds it to the files.
#[must_use]
pub fn known(root: &HostPath, filesystem: &dyn pe2_io::Filesystem) -> Known {
    let _span = tracing::info_span!("model: what is known of the files read").entered();
    // All of it at one commit, each part read on a thread of its own.
    let all = |space: &sfs::Space| -> Option<Known> {
        let read = space.read().ok()?;
        if !is_begun(&read).ok()? {
            return None;
        }
        let read = &read;
        std::thread::scope(|scope| {
            let (layers, records) = (scope.spawn(|| layers(read, root)), scope.spawn(|| records(read)));
            let stack = scope.spawn(|| stack(read));
            let files = files(read, root);
            let (layers, records, stack) = (layers.join().ok()?, records.join().ok()?, stack.join().ok()?);
            Some(Known { files: files.ok()?, layers: layers.ok()?, records: records.ok()?, stack: stack.ok()? })
        })
    };
    let Ok(space) = crate::shared::space(root, filesystem) else { return Known::default() };
    if let Some(held) = all(&space) {
        return held;
    }
    let left = sfs::Space::left_behind(&crate::shared::space_at(root), filesystem).ok().flatten();
    left.and_then(|left| all(&left)).unwrap_or_default()
}

/// The stack applied, as the model holds it; none where it holds none - nothing says what one was made of.
///
/// # Errors
/// The space is damaged, or holds what is none of it.
pub fn stack(pages: &impl Pages) -> Result<Option<Stack>> {
    if !is_begun(pages)? {
        return Ok(None);
    }
    let Some(made_of) = sfs::tree::get_in(pages, STACK, &[MADE_OF])? else { return Ok(None) };
    let made_of = made_of
        .get(8..)
        .and_then(|text| std::str::from_utf8(text).ok())
        .ok_or_else(|| invalid("a stack made of nothing said"))?;
    let mut stack = Stack { made_of: made_of.to_owned(), ..Stack::default() };
    let mut failed = None;
    sfs::tree::scan_prefix(pages, STACK, &[NODE], &mut |key, value| {
        let read = In::after_stamp(value).and_then(|mut read| {
            let mut long = None;
            read.maybe(&mut long)?;
            let path = match long {
                Some(path) => path,
                None => std::str::from_utf8(&key[1..]).ok()?.to_owned(),
            };
            Some((path, read.0.to_vec()))
        });
        match read {
            Some((path, bytes)) => drop(stack.nodes.insert(path, bytes)),
            None => failed = Some(invalid("an entry of the stack that is none")),
        }
        failed.is_none()
    })?;
    sfs::tree::scan_prefix(pages, STACK, &[SAID], &mut |key, value| {
        match std::str::from_utf8(&key[1..]).ok().zip(value.get(8..)) {
            Some((name, bytes)) => drop(stack.said.insert(name.to_owned(), bytes.to_vec())),
            None => failed = Some(invalid("what the stack says that is none of it")),
        }
        failed.is_none()
    })?;
    failed.map_or(Ok(Some(stack)), Err)
}

/// What the model holds of the layers stored as records: each layer's records by uid.
///
/// # Errors
/// The space is damaged, or holds what is no record.
pub fn records(pages: &impl Pages) -> Result<Records> {
    let mut records = Records::new();
    if !is_begun(pages)? {
        return Ok(records);
    }
    let mut failed = None;
    sfs::tree::scan_prefix(pages, RECORDS, &[RECORD], &mut |key, value| {
        let read = layer_recorded(value).and_then(|(recorded, long)| {
            let name = match long {
                Some(name) => name,
                None => std::str::from_utf8(&key[1..]).ok()?.to_owned(),
            };
            let (layer, uid) = name.split_once('\u{0}')?;
            Some((Name::new(layer).ok()?.to_owned(), Uid::new(uid).ok()?, recorded))
        });
        match read {
            Some((layer, uid, recorded)) => drop(records.entry(layer).or_default().insert(uid, recorded)),
            None => failed = Some(invalid("a layer's record that is none")),
        }
        failed.is_none()
    })?;
    failed.map_or(Ok(records), Err)
}

/// What the model holds of the layers: each with what its listing was made by, and what it holds.
///
/// # Errors
/// The space is damaged, or holds what is none of that.
pub fn layers(pages: &impl Pages, root: &HostPath) -> Result<Layers> {
    let mut layers = Layers::new();
    if !is_begun(pages)? {
        return Ok(layers);
    }
    let mut failed = None;
    sfs::tree::scan_prefix(pages, INPUTS, &[LAYER], &mut |key, value| {
        let read = std::str::from_utf8(&key[1..]).ok().zip(In::after_stamp(value).and_then(|mut said| said.text()));
        match read {
            Some((layer, made_by)) => match Name::new(layer) {
                Ok(layer) => drop(layers.insert(layer.to_owned(), (made_by, BTreeMap::new()))),
                Err(_) => failed = Some(invalid("a layer named as none is")),
            },
            None => failed = Some(invalid("a layer that is none")),
        }
        failed.is_none()
    })?;
    sfs::tree::scan_prefix(pages, INPUTS, &[HELD], &mut |key, value| {
        let read = held_of(root, value).and_then(|(held, long)| {
            let name = match long {
                Some(name) => name,
                None => std::str::from_utf8(&key[1..]).ok()?.to_owned(),
            };
            let (layer, path) = name.split_once('\u{0}')?;
            Some((Name::new(layer).ok()?.to_owned(), path.to_owned(), held))
        });
        match read {
            // What is held by a layer the model does not name is nothing it holds.
            Some((layer, path, held)) => drop(layers.get_mut(&layer).map(|(_, holds)| holds.insert(path, held))),
            None => failed = Some(invalid("what a layer holds that is none of it")),
        }
        failed.is_none()
    })?;
    failed.map_or(Ok(layers), Err)
}

/// Say in the model what an index just opened read: what differs from what it opened over (`Index::take_opened`)
/// where the model holds that, else all it read. What whoever holds the project does as it takes it.
///
/// # Errors
/// The shared space is not there, or cannot be written.
pub fn took(root: &HostPath, index: &mut crate::index::Index) -> std::io::Result<()> {
    let _span = tracing::info_span!("model: what the opening read said").entered();
    let opened = index.take_opened();
    let space = crate::shared::space(root, index.io().filesystem())?;
    let mut txn = space.write()?;
    if is_begun(&txn)? {
        opened.apply(&mut txn)?;
    } else {
        fill_files(&mut txn, root, index.file_records())?;
        index.layers_held(root).apply(&mut txn)?;
        index.records_held().apply(&mut txn)?;
        index.stack_held().apply(&mut txn)?;
    }
    txn.commit()?;
    // What a space begun anew kept beside it is no longer what is known of the files.
    sfs::Space::forget_left(&crate::shared::space_at(root), index.io().filesystem());
    Ok(())
}

/// A file's record, as the model holds it; none where it holds none of the file.
///
/// # Errors
/// The space is damaged, or holds what is no record.
pub fn file(pages: &impl Pages, root: &HostPath, path: &HostPath) -> Result<Option<Record>> {
    let name = name(root, path);
    let Some(value) = sfs::tree::get_in(pages, INPUTS, &key(FILE, &name))? else { return Ok(None) };
    let (mut record, _) = recorded(&value).ok_or_else(|| invalid("a file's record that is none"))?;
    if let Some(declaration) = &mut record.declaration {
        declaration.text = declared(pages, &name)?;
    }
    Ok(Some(record))
}

/// A declaration's text, by its file's name.
fn declared(pages: &impl Pages, name: &str) -> Result<Arc<str>> {
    let value =
        sfs::tree::get_in(pages, INPUTS, &key(TEXT, name))?.ok_or_else(|| invalid("a declaration with no text"))?;
    let text = value.get(8..).and_then(|text| std::str::from_utf8(text).ok());
    text.map(Arc::from).ok_or_else(|| invalid("a declaration's text that is none"))
}

/// The declarations' texts, all of them in one pass over their keys: each by the key its file's name makes.
fn texts(pages: &impl Pages) -> Result<HashMap<Vec<u8>, Arc<str>>> {
    let mut texts: HashMap<Vec<u8>, Arc<str>> = HashMap::new();
    let mut failed = None;
    sfs::tree::scan_prefix(pages, INPUTS, &[TEXT], &mut |key, value| {
        match value.get(8..).and_then(|text| std::str::from_utf8(text).ok()) {
            Some(text) => drop(texts.insert(key.to_vec(), Arc::from(text))),
            None => failed = Some(invalid("a declaration's text that is none")),
        }
        failed.is_none()
    })?;
    failed.map_or(Ok(texts), Err)
}

/// Every file's record the model holds, by the file.
///
/// # Errors
/// The space is damaged, or holds what is no record.
pub fn files(pages: &impl Pages, root: &HostPath) -> Result<HashMap<HostPath, Record>> {
    let mut files: HashMap<HostPath, (String, Record)> = HashMap::new();
    if !is_begun(pages)? {
        return Ok(HashMap::new());
    }
    let mut failed = None;
    sfs::tree::scan_prefix(pages, INPUTS, &[FILE], &mut |key, value| {
        let read = recorded(value).and_then(|(record, long)| {
            let name = match long {
                Some(name) => name,
                None => std::str::from_utf8(&key[1..]).ok()?.to_owned(),
            };
            Some((named(root, &name)?, name, record))
        });
        match read {
            Some((path, name, record)) => drop(files.insert(path, (name, record))),
            None => failed = Some(invalid("a file's record that is none")),
        }
        failed.is_none()
    })?;
    if let Some(failed) = failed {
        return Err(failed);
    }
    let mut texts = texts(pages)?;
    let mut read = HashMap::with_capacity(files.len());
    for (path, (name, mut record)) in files {
        if let Some(declaration) = &mut record.declaration {
            declaration.text = texts.remove(&key(TEXT, &name)).ok_or_else(|| invalid("a declaration with no text"))?;
        }
        read.insert(path, record);
    }
    Ok(read)
}

#[cfg(test)]
#[allow(
    clippy::disallowed_methods,
    reason = "a test's fixtures: the machine's time, places and environment, which nothing tested reads"
)]
mod tests {

    use super::*;
    use crate::testing::At as _;
    use crate::testing::{rel, uid};
    use sfs::HostPath;

    fn a_record(n: u64, declared: bool) -> Record {
        let declaration = declared.then(|| Declaration {
            shape: format!("shape {n}"),
            uid: (n % 2 == 0).then(|| format!("uid://{n:012}")),
            variants: (0..n % 3).map(|v| format!("variant {v}")).collect(),
            references: (0..n % 4)
                .map(|r| Declared {
                    slot: format!("slot{r}").into(),
                    uid: if r == 3 { Err("uid://no uid".into()) } else { Ok(uid(&format!("uid://{r:012}"))) },
                    target: (r % 2 == 0).then(|| crate::testing::tree(&format!("a/b{r}"))),
                    output: (r == 1).then(|| Arc::from(pe2_path::Asked::new("palettes/night").unwrap())),
                })
                .collect(),
            error: (n % 5 == 0).then(|| "it cannot be parsed".to_string()),
            text: Arc::from(format!(
                "uid = \"uid://{n:012}\"\n# {}\n",
                "x".repeat(usize::try_from(n * 97 % 3000).unwrap())
            )),
        });
        let sha1 = crate::extract::sha1_of(&n.to_le_bytes());
        Record { size: n * 7, modified: (1_700_000_000 + n, 5), recorded: (1_700_000_100, 9), sha1, declaration }
    }

    fn files_of(root: &HostPath, count: u64) -> HashMap<HostPath, Record> {
        let mut files: HashMap<HostPath, Record> = (0..count)
            .map(|n| {
                (
                    root.at(&format!("layers/game/{}/n{n}.node", n % 17)).at(if n % 3 == 0 {
                        "node.toml"
                    } else {
                        "a.bin"
                    }),
                    a_record(n, n % 3 == 0),
                )
            })
            .collect();
        // One elsewhere, as a mount's; one whose name no key holds.
        files.insert(crate::testing::scratch("model-elsewhere").at("file.c"), a_record(1, false));
        files.insert(root.at(&format!("deep/{}node.toml", "long/".repeat(200))), a_record(3, true));
        files
    }

    /// What two layers hold, as a walk of them finds it: folders, plain files, bundles - one of a name no key holds.
    fn held_by(root: &HostPath) -> Layers {
        let game = root.at("layers/game");
        let bundle = |n: u64| -> BTreeMap<Arc<pe2_path::RelPath>, HostPath> {
            let folder = game.at(&format!("rooms/r{n}.node"));
            (0..=n % 3).map(|f| (rel(&format!("file{f}.bin")), folder.at(&format!("file{f}.bin")))).collect()
        };
        let mut holds: BTreeMap<String, Held> =
            (0..200u64).map(|n| (format!("rooms/r{n}.node"), Held::Bundle(bundle(n)))).collect();
        holds.insert("rooms".into(), Held::Folder);
        holds.insert("rooms/notes.txt".into(), Held::File(game.at("rooms/notes.txt")));
        holds.insert(format!("deep/{}", "long/".repeat(200)), Held::Folder);
        let code: BTreeMap<String, Held> = [
            ("src".to_string(), Held::Folder),
            ("src/a.c".to_string(), Held::File(crate::testing::scratch("model-code").at("src/a.c"))),
        ]
        .into();
        [
            (crate::testing::name("game"), ("made by this".to_string(), holds)),
            (crate::testing::name("code"), ("and that".to_string(), code)),
        ]
        .into()
    }

    /// What `held_by` holds, said as a change of the model.
    fn holds(root: &HostPath) -> Delta {
        let mut delta = Delta::default();
        for (layer, (made_by, holds)) in held_by(root) {
            delta.layer(&layer, Some(&made_by));
            for (path, held) in &holds {
                delta.held(root, &layer, path, Some(held));
            }
        }
        delta
    }

    #[test]
    fn what_the_model_holds_of_layers_is_what_was_said_of_them() {
        let root = &crate::testing::scratch("model-root-1");
        let space = sfs::Space::private().unwrap();
        let mut txn = space.write().unwrap();
        fill_files(&mut txn, root, std::iter::empty()).unwrap();
        holds(root).apply(&mut txn).unwrap();
        txn.commit().unwrap();
        let mut now = held_by(root);
        assert_eq!(layers(&space.read().unwrap(), root).unwrap(), now);
        // A bundle's files change, a folder goes, a layer's listing is made by something else, and a layer is no more.
        let mut delta = Delta::default();
        let changed = Held::Bundle([(rel("only.bin"), root.at("layers/game/rooms/r7.node/only.bin"))].into());
        delta.held(root, pe2_path::Name::constant("game"), "rooms/r7.node", Some(&changed));
        delta.held(root, pe2_path::Name::constant("game"), &format!("deep/{}", "long/".repeat(200)), None);
        delta.layer(pe2_path::Name::constant("game"), Some("made by another"));
        delta.held(root, pe2_path::Name::constant("code"), "src", None);
        delta.held(root, pe2_path::Name::constant("code"), "src/a.c", None);
        delta.layer(pe2_path::Name::constant("code"), None);
        let mut txn = space.write().unwrap();
        delta.apply(&mut txn).unwrap();
        txn.commit().unwrap();
        now.remove(pe2_path::Name::constant("code"));
        let game = now.get_mut(pe2_path::Name::constant("game")).unwrap();
        game.0 = "made by another".into();
        game.1.insert("rooms/r7.node".into(), changed);
        game.1.remove(&format!("deep/{}", "long/".repeat(200)));
        assert_eq!(layers(&space.read().unwrap(), root).unwrap(), now);
        // And the files' records are apart from all of it.
        assert!(files(&space.read().unwrap(), root).unwrap().is_empty());
    }

    /// The records of two layers stored as records: creates, an override with what it was made from and a file
    /// it removes, a move, a delete; texts long and short, a file kept in the store.
    fn recorded() -> Records {
        use crate::records::{Content, File, Kind, Op, Record};
        let record = |n: u64| {
            let (op, kind) = match n % 5 {
                0 => (Op::Create, Kind::Node),
                1 => (Op::Override, Kind::Node),
                2 => (Op::Move, Kind::Folder),
                3 => (Op::Delete, Kind::File),
                _ => (Op::Create, Kind::File),
            };
            let mut files = BTreeMap::new();
            if kind == Kind::Node {
                let text = format!("type = \"data\"\n# {}\n", "y".repeat(usize::try_from(n * 131 % 4000).unwrap()));
                files.insert(
                    rel("node.toml"),
                    File {
                        content: Some(Content::Text(text)),
                        base: (op == Op::Override).then(|| crate::extract::sha1_of(b"was")),
                    },
                );
                files.insert(
                    rel("native.bin"),
                    File {
                        content: Some(Content::Stored { sha1: crate::extract::sha1_of(b"native"), size: n * 1000 }),
                        base: None,
                    },
                );
                files.insert(rel("gone.bin"), File { content: None, base: Some(crate::extract::sha1_of(b"gone")) });
            } else if op == Op::Create {
                files.insert(rel(""), File { content: Some(Content::Text(String::new())), base: None });
            }
            let placed = matches!(op, Op::Create | Op::Move);
            let record = Record {
                op,
                kind,
                parent: (placed && n % 2 == 0).then(|| uid(&format!("uid://parent{n}"))),
                name: placed.then(|| pe2_path::Name::new(&format!("name {n}")).unwrap().to_owned()),
                files,
                tree: None,
                local: false,
                theirs: None,
            };
            (uid(&format!("uid://{n:012}")), (Arc::new(record), (1_700_000_000 + n, 7), 100 + n))
        };
        [
            (crate::testing::name("game"), (0..300).map(record).collect()),
            (crate::testing::name("project"), (300..320).map(record).collect()),
        ]
        .into()
    }

    #[test]
    fn what_the_model_holds_of_the_layers_records_is_what_was_said_of_them() {
        let space = sfs::Space::private().unwrap();
        let mut now = recorded();
        let mut delta = Delta::default();
        for (layer, records) in &now {
            for (uid, recorded) in records {
                delta.record(layer, *uid, Some(recorded));
            }
        }
        let mut txn = space.write().unwrap();
        fill_files(&mut txn, &crate::testing::scratch("model-root-fill"), std::iter::empty()).unwrap();
        delta.apply(&mut txn).unwrap();
        txn.commit().unwrap();
        assert_eq!(records(&space.read().unwrap()).unwrap(), now);
        // One changed, one in a file of another time, one gone, and a layer's last.
        let mut delta = Delta::default();
        let (game_layer, project_layer) = (pe2_path::Name::constant("game"), pe2_path::Name::constant("project"));
        let (first, second) = (uid("uid://000000000001"), uid("uid://000000000002"));
        let game = now.get_mut(game_layer).unwrap();
        let changed = (game[&first].0.clone(), (1_800_000_000, 0), 5);
        delta.record(game_layer, first, Some(&changed));
        game.insert(first, changed);
        delta.record(game_layer, second, None);
        game.remove(&second);
        for uid in now.remove(project_layer).unwrap().keys() {
            delta.record(project_layer, *uid, None);
        }
        let mut txn = space.write().unwrap();
        delta.apply(&mut txn).unwrap();
        txn.commit().unwrap();
        assert_eq!(records(&space.read().unwrap()).unwrap(), now);
        // A time is kept to what a file has of it.
        let time = std::time::UNIX_EPOCH + std::time::Duration::new(1_794_000_000, 123_456_700);
        assert_eq!(time_of(stamp_of(time)), time);
    }

    #[test]
    fn what_the_model_holds_of_files_is_what_it_was_given_and_what_changed_of_it() {
        let root = &crate::testing::scratch("model-root-2");
        let space = sfs::Space::private().unwrap();
        assert!(files(&space.read().unwrap(), root).unwrap().is_empty());
        let mut now = files_of(root, 400);
        let mut txn = space.write().unwrap();
        fill_files(&mut txn, root, now.iter()).unwrap();
        txn.commit().unwrap();
        assert_eq!(files(&space.read().unwrap(), root).unwrap(), now);
        // Filled again with the same, nothing is written: the space is at the commit it was at.
        let generation = space.read().unwrap().commit().generation;
        let mut txn = space.write().unwrap();
        fill_files(&mut txn, root, now.iter()).unwrap();
        txn.commit().unwrap();
        assert_eq!(space.read().unwrap().commit().generation, generation);
        // What a refresh changes: files read again, one new, some gone - and one of them by a name no key holds.
        let mut delta = Delta::default();
        let long = root.at(&format!("deep/{}node.toml", "long/".repeat(200)));
        for (n, path) in now.keys().cloned().collect::<Vec<_>>().into_iter().enumerate() {
            match n % 5 {
                0 => {
                    now.remove(&path);
                    delta.file(root, &path, None);
                }
                1 => {
                    let changed = a_record(n as u64 + 1000, n % 2 == 0);
                    delta.file(root, &path, Some(&changed));
                    now.insert(path, changed);
                }
                _ => {}
            }
        }
        let added = a_record(77, true);
        delta.file(root, &root.at("new/node.toml"), Some(&added));
        now.insert(root.at("new/node.toml"), added);
        delta.file(root, &long, None);
        now.remove(&long);
        // Gathered and not applied, nothing changed; applied and given up, nothing either.
        assert_ne!(files(&space.read().unwrap(), root).unwrap(), now);
        let mut txn = space.write().unwrap();
        delta.apply(&mut txn).unwrap();
        drop(txn);
        assert_ne!(files(&space.read().unwrap(), root).unwrap(), now);
        let mut txn = space.write().unwrap();
        delta.apply(&mut txn).unwrap();
        txn.commit().unwrap();
        let read = space.read().unwrap();
        assert_eq!(files(&read, root).unwrap(), now);
        for (path, record) in &now {
            assert_eq!(file(&read, root, path).unwrap().as_ref(), Some(record));
        }
        assert_eq!(file(&read, root, &long).unwrap(), None);
        drop(read);
        // Brought to another set of files by what differs: the same as filled with them from nothing.
        let other = files_of(root, 300);
        let mut txn = space.write().unwrap();
        fill_files(&mut txn, root, other.iter()).unwrap();
        txn.commit().unwrap();
        assert_eq!(files(&space.read().unwrap(), root).unwrap(), other);
    }

    /// The model a project's processes left in its shared space, against what an index built from its files
    /// reads: `PE2_MODEL_OF=<project> cargo test -p project --lib the_model_a_project_was_left -- --ignored`,
    /// after whatever held it ended in order.
    #[test]
    #[ignore = "of a project given"]
    fn the_model_a_project_was_left_is_what_its_files_say() {
        let Ok(root) = std::env::var("PE2_MODEL_OF") else { return };
        let project = crate::Project::load(std::path::Path::new(&root)).unwrap();
        let space = crate::shared::space(&project.root, project.io.filesystem()).unwrap();
        assert!(!space.fresh(), "its space was begun anew: nothing left it in order");
        let unread = |mut records: HashMap<HostPath, Record>| {
            for record in records.values_mut() {
                record.recorded = (0, 0);
            }
            records
        };
        let held = unread(files(&space.read().unwrap(), &project.root).unwrap());
        let index = crate::index::Index::without_store(&project).unwrap();
        let read = unread(index.file_records().map(|(path, record)| (path.clone(), record.clone())).collect());
        let differing: Vec<_> =
            held.keys().chain(read.keys()).filter(|path| held.get(*path) != read.get(*path)).collect();
        assert!(
            differing.is_empty(),
            "{} of {} files differ: {:#?}",
            differing.len(),
            read.len(),
            &differing[..differing.len().min(10)]
        );
        // And of every layer what a walk of it finds.
        let (held_by, walked) = (layers(&space.read().unwrap(), &project.root).unwrap(), index.layers_known());
        for (layer, (made_by, holds)) in &walked {
            let (was_made_by, held) =
                held_by.get(layer).unwrap_or_else(|| panic!("the model knows nothing of the layer {layer}"));
            assert_eq!(was_made_by, made_by, "{layer}: what its listing was made by");
            let differing: Vec<_> =
                held.keys().chain(holds.keys()).filter(|path| held.get(*path) != holds.get(*path)).collect();
            assert!(
                differing.is_empty(),
                "{layer}: {} paths differ: {:#?}",
                differing.len(),
                &differing[..differing.len().min(10)]
            );
        }
        assert_eq!(held_by.len(), walked.len());
        // And of every layer stored as records the records an index built afresh reads.
        let (recorded, read_now) = (records(&space.read().unwrap()).unwrap(), index.records_known());
        for (layer, now) in &read_now {
            let was = recorded.get(layer).unwrap_or_else(|| panic!("the model has no record of the layer {layer}"));
            let differing: Vec<_> = was.keys().chain(now.keys()).filter(|uid| was.get(*uid) != now.get(*uid)).collect();
            assert!(
                differing.is_empty(),
                "{layer}: {} records differ: {:#?}",
                differing.len(),
                &differing[..differing.len().min(10)]
            );
        }
        assert_eq!(recorded.len(), read_now.len());
        // And the stack applied, as one applied afresh over them has it.
        let (kept, applied) =
            (stack(&space.read().unwrap()).unwrap().expect("the model holds no stack"), index.stack_known());
        let differing: Vec<_> = kept
            .nodes
            .keys()
            .chain(applied.nodes.keys())
            .filter(|path| kept.nodes.get(*path) != applied.nodes.get(*path))
            .collect();
        assert!(
            differing.is_empty(),
            "{} entries of the stack differ: {:#?}",
            differing.len(),
            &differing[..differing.len().min(10)]
        );
        assert!(kept == applied, "what the stack says besides its entries, or what it was made of, differs");
        let entries = applied.nodes.len();
        let of_records: usize = read_now.values().map(BTreeMap::len).sum();
        let holds: usize = walked.values().map(|(_, holds)| holds.len()).sum();
        let usage = space.usage().unwrap();
        eprintln!(
            "{} files, what {} layers hold at {holds} paths, {of_records} records and the stack's {entries} entries, the same in the model as read afresh; the space holds {} pages, {} of them free",
            read.len(),
            walked.len(),
            usage.pages,
            usage.free
        );
    }

    #[test]
    #[ignore = "a measurement"]
    fn bench_fill() {
        let root = &crate::testing::scratch("model-bench");
        // As a project of 14,000 files: half of them declarations of a kibibyte and a half.
        let now: HashMap<HostPath, Record> = (0..14_000u64)
            .map(|n| {
                let mut record = a_record(n, n % 2 == 0);
                if let Some(declaration) = &mut record.declaration {
                    declaration.text = Arc::from("x".repeat(1400));
                }
                let folder = format!("layers/game/stages/stage{}/rooms/room{}/n{n}.node", n % 7, n % 31);
                (root.at(&folder).at(if n % 2 == 0 { "node.toml" } else { "native.bin" }), record)
            })
            .collect();
        let space = sfs::Space::private().unwrap();
        for what in ["into nothing", "again, the same", "again, the same"] {
            let started = std::time::Instant::now();
            let mut txn = space.write().unwrap();
            fill_files(&mut txn, root, now.iter()).unwrap();
            txn.commit().unwrap();
            eprintln!("14,000 files' records filled {what}: {:.1} ms", started.elapsed().as_secs_f64() * 1e3);
        }
        let started = std::time::Instant::now();
        let wanted: usize =
            now.iter().map(|(path, now)| key(FILE, &name(root, path)).len() + record("x", now).len()).sum();
        eprintln!(
            "named and written as values alone: {:.1} ms ({wanted} bytes)",
            started.elapsed().as_secs_f64() * 1e3
        );
        let started = std::time::Instant::now();
        let read = files(&space.read().unwrap(), root).unwrap();
        eprintln!(
            "all read back: {:.1} ms; {} pages",
            started.elapsed().as_secs_f64() * 1e3,
            space.usage().unwrap().pages
        );
        assert_eq!(read.len(), now.len());
    }

    #[test]
    fn what_processes_that_died_left_of_the_files_is_known_to_who_comes_next() {
        let root = crate::testing::scratch("model-left");
        let _ = std::fs::remove_dir_all(&root);
        let at = crate::shared::space_at(&root);
        std::fs::create_dir_all(&at).unwrap();
        let now = files_of(&root, 300);
        {
            // As a holder that is killed: its model said, the space never left in order.
            let space = sfs::Space::open(&at, &pe2_io::SystemFilesystem).unwrap();
            let mut txn = space.write().unwrap();
            fill_files(&mut txn, &root, now.iter()).unwrap();
            holds(&root).apply(&mut txn).unwrap();
            txn.commit().unwrap();
        }
        // Who attaches next finds the space begun anew - and what it held beside it, read whole.
        let left = known(&root, &pe2_io::SystemFilesystem);
        assert_eq!(left.files, now);
        assert_eq!(left.layers, held_by(&root));
        let space = crate::shared::space(&root, &pe2_io::SystemFilesystem).unwrap();
        assert!(space.fresh() && at.at("left").is_dir());
        assert!(files(&space.read().unwrap(), &root).unwrap().is_empty());
        // Said again in the space as it is now, it is known from there, and what was kept beside it goes.
        let mut txn = space.write().unwrap();
        fill_files(&mut txn, &root, now.iter()).unwrap();
        txn.commit().unwrap();
        sfs::Space::forget_left(&at, &pe2_io::SystemFilesystem);
        let said = known(&root, &pe2_io::SystemFilesystem);
        assert_eq!(said.files, now);
        assert!(said.layers.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_file_is_named_by_where_it_is_in_the_project_wherever_that_is() {
        let (here, there) = (crate::testing::scratch("model-here"), crate::testing::scratch("model-there"));
        let file = here.at("layers/game/a.node/node.toml");
        assert_eq!(name(&here, &file), "/layers/game/a.node/node.toml");
        assert_eq!(named(&there, &name(&here, &file)), Some(there.at("layers/game/a.node/node.toml")));
        let elsewhere = crate::testing::scratch("model-elsewhere").at("x.c");
        assert_eq!(named(&here, &name(&here, &elsewhere)), Some(elsewhere));
        // The keys of files are apart from those of their texts, and from the model's word of itself.
        assert_ne!(key(FILE, "/a"), key(TEXT, "/a"));
        assert!(key(FILE, &"x".repeat(5000)).len() < 100);
    }

    #[test]
    fn a_model_of_another_format_is_filled_again() {
        let root = &crate::testing::scratch("model-root-3");
        let space = sfs::Space::private().unwrap();
        let mut txn = space.write().unwrap();
        // As a format before this one left it: its word of itself another, and keys this one knows nothing of.
        let mut said = Out::new();
        said.u32(FORMAT + 1);
        said.u32(crate::local::SCHEMA);
        said.text(pe2_path::UNICODE_VERSION);
        sfs::tree::put(&mut txn, INPUTS, &[META], &said.0).unwrap();
        sfs::tree::put(&mut txn, INPUTS, b"f/old", b"what it held").unwrap();
        sfs::tree::put(&mut txn, ASSETS, b"an asset", b"of then").unwrap();
        txn.commit().unwrap();
        assert!(files(&space.read().unwrap(), root).unwrap().is_empty());
        let now = files_of(root, 20);
        let mut txn = space.write().unwrap();
        fill_files(&mut txn, root, now.iter()).unwrap();
        txn.commit().unwrap();
        let read = space.read().unwrap();
        assert_eq!(files(&read, root).unwrap(), now);
        assert_eq!(read.root(ASSETS), 0);
    }
}
