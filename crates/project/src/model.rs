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
//! So far it holds each file's record (`store::Record`), written beside the index's own.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sfs::space::{Pages, Result, Txn};

use crate::store::{Declaration, Record};

/// The tree of what is read: the model's own word of itself, and each file's record and text.
pub const INPUTS: usize = 4;
/// The trees of what is to come: the layers' records, the stack, the assets, their links.
pub const RECORDS: usize = 5;
pub const STACK: usize = 6;
pub const ASSETS: usize = 7;
pub const LINKS: usize = 12;

/// Raised whenever what the trees hold changes shape: a model of another format is emptied, and filled again.
const FORMAT: u32 = 1;

/// What a key of `INPUTS` begins with: the model's word of itself; a file's record; a declaration's text.
const META: u8 = b'm';
const FILE: u8 = b'f';
const TEXT: u8 = b't';

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
struct Out(Vec<u8>);

impl Out {
    /// A value that begins with the generation of the commit that puts it, said as it is applied.
    fn new() -> Out {
        Out(vec![0; 8])
    }

    fn u8(&mut self, it: u8) {
        self.0.push(it);
    }

    fn u32(&mut self, it: u32) {
        self.0.extend_from_slice(&it.to_le_bytes());
    }

    fn u64(&mut self, it: u64) {
        self.0.extend_from_slice(&it.to_le_bytes());
    }

    fn bytes(&mut self, it: &[u8]) {
        self.u32(u32::try_from(it.len()).unwrap_or(u32::MAX));
        self.0.extend_from_slice(it);
    }

    fn text(&mut self, it: &str) {
        self.bytes(it.as_bytes());
    }

    fn maybe(&mut self, it: Option<&str>) {
        self.u8(u8::from(it.is_some()));
        if let Some(it) = it {
            self.text(it);
        }
    }
}

/// A value, read.
struct In<'a>(&'a [u8]);

impl<'a> In<'a> {
    /// What a value holds after the generation that put it.
    fn after_stamp(value: &'a [u8]) -> Option<In<'a>> {
        value.get(8..).map(In)
    }

    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let (taken, rest) = self.0.split_at_checked(len)?;
        self.0 = rest;
        Some(taken)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|it| it[0])
    }

    fn u32(&mut self) -> Option<u32> {
        self.take(4).and_then(|it| it.try_into().ok()).map(u32::from_le_bytes)
    }

    fn u64(&mut self) -> Option<u64> {
        self.take(8).and_then(|it| it.try_into().ok()).map(u64::from_le_bytes)
    }

    fn bytes(&mut self) -> Option<&'a [u8]> {
        let len = usize::try_from(self.u32()?).ok()?;
        self.take(len)
    }

    fn text(&mut self) -> Option<String> {
        std::str::from_utf8(self.bytes()?).ok().map(str::to_owned)
    }

    /// A text that may be there, read into what is given: none where what is read is no such thing.
    fn maybe(&mut self, into: &mut Option<String>) -> Option<()> {
        *into = match self.u8()? {
            0 => None,
            _ => Some(self.text()?),
        };
        Some(())
    }
}

/// A file's name in the model: where it is in the project's folder, by its parts - the same whatever the folder is
/// called on this system, or where it is - or, for one elsewhere (a mount's), its whole path.
fn name(root: &Path, path: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(within) => {
            let parts: Vec<_> = within.components().map(|part| part.as_os_str().to_string_lossy()).collect();
            format!("/{}", parts.join("/"))
        }
        Err(_) => format!("={}", path.to_string_lossy()),
    }
}

/// The file a name is of.
fn named(root: &Path, name: &str) -> Option<PathBuf> {
    if let Some(elsewhere) = name.strip_prefix('=') {
        return Some(PathBuf::from(elsewhere));
    }
    let within = name.strip_prefix('/')?;
    let mut path = root.to_path_buf();
    // Each part by itself: a path Windows takes as it is written has no other separator than its own.
    path.extend(within.split('/').filter(|part| !part.is_empty()));
    Some(path)
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

fn stamp(out: &mut Out, (secs, nanos): crate::store::Stamp) {
    out.u64(secs);
    out.u32(nanos);
}

fn stamped(read: &mut In<'_>) -> Option<crate::store::Stamp> {
    Some((read.u64()?, read.u32()?))
}

/// A file's record as the model keeps it, under its name: all of it but a declaration's text, which is by itself.
fn record(name: &str, record: &Record) -> Vec<u8> {
    let mut out = Out::new();
    let sha1 = (record.sha1.len() == 40).then(|| hex(&record.sha1)).flatten();
    let declaration = record.declaration.as_ref();
    let long = name.len() > NAMED;
    let says = |it: bool, said: u8| if it { said } else { 0 };
    out.u8(says(declaration.is_some(), says::DECLARATION) | says(long, says::NAMED) | says(sha1.is_some(), says::SHA1));
    out.u64(record.size);
    stamp(&mut out, record.modified);
    stamp(&mut out, record.recorded);
    match sha1 {
        Some(sha1) => out.0.extend_from_slice(&sha1),
        None => out.text(&record.sha1),
    }
    if let Some(declaration) = declaration {
        out.text(&declaration.shape);
        out.maybe(declaration.uid.as_deref());
        out.u32(u32::try_from(declaration.variants.len()).unwrap_or(u32::MAX));
        for variant in &declaration.variants {
            out.text(variant);
        }
        out.u32(u32::try_from(declaration.references.len()).unwrap_or(u32::MAX));
        for (slot, uid, target, output) in &declaration.references {
            out.text(slot);
            out.text(uid);
            out.text(target);
            out.text(output);
        }
        out.maybe(declaration.error.as_deref());
    }
    if long {
        out.text(name);
    }
    out.0
}

/// A hash written as forty hexadecimal digits, as its twenty bytes; none where it is written otherwise.
fn hex(text: &str) -> Option<[u8; 20]> {
    let mut bytes = [0u8; 20];
    for (byte, digits) in bytes.iter_mut().zip(text.as_bytes().chunks(2)) {
        let digit = |digit: u8| match digit {
            b'0'..=b'9' => Some(digit - b'0'),
            b'a'..=b'f' => Some(digit - b'a' + 10),
            _ => None,
        };
        *byte = digit(digits[0])? << 4 | digit(*digits.get(1)?)?;
    }
    Some(bytes)
}

fn hexed(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
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
    let sha1 = if said & says::SHA1 == 0 { read.text()? } else { hexed(read.take(20)?) };
    let declaration = if said & says::DECLARATION == 0 {
        None
    } else {
        let shape = read.text()?;
        let (mut uid, mut error) = (None, None);
        read.maybe(&mut uid)?;
        let variants = (0..read.u32()?).map(|_| read.text()).collect::<Option<Vec<_>>>()?;
        let references =
            (0..read.u32()?).map(|_| Some((read.text()?, read.text()?, read.text()?, read.text()?))).collect::<Option<Vec<_>>>()?;
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

/// What a refresh changes of the model, gathered as it goes and applied whole (`apply`): each key of a tree given
/// a value, or taken away, in the order it was said.
#[derive(Debug, Default)]
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
    pub fn file(&mut self, root: &Path, path: &Path, now: Option<&Record>) {
        let name = name(root, path);
        let (of_record, of_text) = (key(FILE, &name), key(TEXT, &name));
        self.changes.push((INPUTS, of_record, now.map(|now| record(&name, now))));
        let declared = now.and_then(|now| now.declaration.as_ref());
        self.changes.push((INPUTS, of_text, declared.map(|declared| text(&declared.text))));
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
    Ok(said.is_some_and(|said| In::after_stamp(&said).and_then(|mut said| said.u32()) == Some(FORMAT)))
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
    out.0
}

/// Have the model hold these files' records and no other: all of them loaded whole where it holds nothing, else
/// what differs put and taken away - what whoever takes a project says of what its index read.
///
/// # Errors
/// The space cannot be written.
pub fn fill_files<'a>(txn: &mut Txn<'_>, root: &Path, records: impl Iterator<Item = (&'a Path, &'a Record)>) -> Result<()> {
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

/// A file's record, as the model holds it; none where it holds none of the file.
///
/// # Errors
/// The space is damaged, or holds what is no record.
pub fn file(pages: &impl Pages, root: &Path, path: &Path) -> Result<Option<Record>> {
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
    let value = sfs::tree::get_in(pages, INPUTS, &key(TEXT, name))?.ok_or_else(|| invalid("a declaration with no text"))?;
    let text = value.get(8..).and_then(|text| std::str::from_utf8(text).ok());
    text.map(Arc::from).ok_or_else(|| invalid("a declaration's text that is none"))
}

/// Every file's record the model holds, by the file.
///
/// # Errors
/// The space is damaged, or holds what is no record.
pub fn files(pages: &impl Pages, root: &Path) -> Result<HashMap<PathBuf, Record>> {
    let mut files: HashMap<PathBuf, (String, Record)> = HashMap::new();
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
    let mut read = HashMap::with_capacity(files.len());
    for (path, (name, mut record)) in files {
        if let Some(declaration) = &mut record.declaration {
            declaration.text = declared(pages, &name)?;
        }
        read.insert(path, record);
    }
    Ok(read)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_record(n: u64, declared: bool) -> Record {
        let declaration = declared.then(|| Declaration {
            shape: format!("shape {n}"),
            uid: (n % 2 == 0).then(|| format!("uid://{n:012}")),
            variants: (0..n % 3).map(|v| format!("variant {v}")).collect(),
            references: (0..n % 4).map(|r| (format!("slot{r}"), format!("uid://{r:012}"), format!("a/b{r}"), String::new())).collect(),
            error: (n % 5 == 0).then(|| "it cannot be parsed".to_string()),
            text: Arc::from(format!("uid = \"uid://{n:012}\"\n# {}\n", "x".repeat(usize::try_from(n * 97 % 3000).unwrap()))),
        });
        let sha1 = if n % 11 == 0 { String::new() } else { crate::extract::sha1_hex(&n.to_le_bytes()) };
        Record { size: n * 7, modified: (1_700_000_000 + n, 5), recorded: (1_700_000_100, 9), sha1, declaration }
    }

    fn files_of(root: &Path, count: u64) -> HashMap<PathBuf, Record> {
        let mut files: HashMap<PathBuf, Record> = (0..count)
            .map(|n| (root.join(format!(".layers/game/{}/n{n}.node", n % 17)).join(if n % 3 == 0 { "node.toml" } else { "a.bin" }), a_record(n, n % 3 == 0)))
            .collect();
        // One elsewhere, as a mount's; one whose name no key holds.
        files.insert(PathBuf::from("/somewhere/else/file.c"), a_record(1, false));
        files.insert(root.join(format!("deep/{}/node.toml", "long/".repeat(200))), a_record(3, true));
        files
    }

    #[test]
    fn what_the_model_holds_of_files_is_what_it_was_given_and_what_changed_of_it() {
        let root = Path::new("/a project");
        let space = sfs::Space::private().unwrap();
        assert!(files(&space.read().unwrap(), root).unwrap().is_empty());
        let mut now = files_of(root, 400);
        let mut txn = space.write().unwrap();
        fill_files(&mut txn, root, now.iter().map(|(path, record)| (path.as_path(), record))).unwrap();
        txn.commit().unwrap();
        assert_eq!(files(&space.read().unwrap(), root).unwrap(), now);
        // Filled again with the same, nothing is written: the space is at the commit it was at.
        let generation = space.read().unwrap().commit().generation;
        let mut txn = space.write().unwrap();
        fill_files(&mut txn, root, now.iter().map(|(path, record)| (path.as_path(), record))).unwrap();
        txn.commit().unwrap();
        assert_eq!(space.read().unwrap().commit().generation, generation);
        // What a refresh changes: files read again, one new, some gone - and one of them by a name no key holds.
        let mut delta = Delta::default();
        let long = root.join(format!("deep/{}/node.toml", "long/".repeat(200)));
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
        delta.file(root, &root.join("new/node.toml"), Some(&added));
        now.insert(root.join("new/node.toml"), added);
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
        fill_files(&mut txn, root, other.iter().map(|(path, record)| (path.as_path(), record))).unwrap();
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
        let project = crate::Project::load(Path::new(&root)).unwrap();
        let space = crate::shared::space(&project.root).unwrap();
        assert!(!space.fresh(), "its space was begun anew: nothing left it in order");
        let unread = |mut records: HashMap<PathBuf, Record>| {
            for record in records.values_mut() {
                record.recorded = (0, 0);
            }
            records
        };
        let held = unread(files(&space.read().unwrap(), &project.root).unwrap());
        let index = crate::index::Index::without_store(&project).unwrap();
        let read = unread(index.file_records().map(|(path, record)| (path.to_path_buf(), record.clone())).collect());
        let differing: Vec<_> = held.keys().chain(read.keys()).filter(|path| held.get(*path) != read.get(*path)).collect();
        assert!(differing.is_empty(), "{} of {} files differ: {:#?}", differing.len(), read.len(), &differing[..differing.len().min(10)]);
        let usage = space.usage().unwrap();
        eprintln!("{} files, the same in the model as read afresh; the space holds {} pages, {} of them free", read.len(), usage.pages, usage.free);
    }

    #[test]
    #[ignore = "a measurement"]
    fn bench_fill() {
        let root = Path::new("/home/someone/a project");
        // As a project of 14,000 files: half of them declarations of a kibibyte and a half.
        let now: HashMap<PathBuf, Record> = (0..14_000u64)
            .map(|n| {
                let mut record = a_record(n, n % 2 == 0);
                if let Some(declaration) = &mut record.declaration {
                    declaration.text = Arc::from("x".repeat(1400));
                }
                let folder = format!(".layers/game/stages/stage{}/rooms/room{}/n{n}.node", n % 7, n % 31);
                (root.join(folder).join(if n % 2 == 0 { "node.toml" } else { "native.bin" }), record)
            })
            .collect();
        let space = sfs::Space::private().unwrap();
        for what in ["into nothing", "again, the same", "again, the same"] {
            let started = std::time::Instant::now();
            let mut txn = space.write().unwrap();
            fill_files(&mut txn, root, now.iter().map(|(path, record)| (path.as_path(), record))).unwrap();
            txn.commit().unwrap();
            eprintln!("14,000 files' records filled {what}: {:.1} ms", started.elapsed().as_secs_f64() * 1e3);
        }
        let started = std::time::Instant::now();
        let wanted: usize = now.iter().map(|(path, now)| key(FILE, &name(root, path)).len() + record("x", now).len()).sum();
        eprintln!("named and written as values alone: {:.1} ms ({wanted} bytes)", started.elapsed().as_secs_f64() * 1e3);
        let started = std::time::Instant::now();
        let read = files(&space.read().unwrap(), root).unwrap();
        eprintln!("all read back: {:.1} ms; {} pages", started.elapsed().as_secs_f64() * 1e3, space.usage().unwrap().pages);
        assert_eq!(read.len(), now.len());
    }

    #[test]
    fn a_file_is_named_by_where_it_is_in_the_project_wherever_that_is() {
        let (here, there) = (Path::new("/home/someone/project"), Path::new("/mnt/other/project"));
        let file = here.join(".layers").join("game").join("a.node").join("node.toml");
        assert_eq!(name(here, &file), "/.layers/game/a.node/node.toml");
        assert_eq!(named(there, &name(here, &file)), Some(there.join(".layers/game/a.node/node.toml")));
        assert_eq!(named(here, &name(here, Path::new("/elsewhere/x.c"))), Some(PathBuf::from("/elsewhere/x.c")));
        // The keys of files are apart from those of their texts, and from the model's word of itself.
        assert_ne!(key(FILE, "/a"), key(TEXT, "/a"));
        assert!(key(FILE, &"x".repeat(5000)).len() < 100);
    }

    #[test]
    fn a_model_of_another_format_is_filled_again() {
        let root = Path::new("/a project");
        let space = sfs::Space::private().unwrap();
        let mut txn = space.write().unwrap();
        // As a format before this one left it: its word of itself another, and keys this one knows nothing of.
        let mut said = Out::new();
        said.u32(FORMAT + 1);
        sfs::tree::put(&mut txn, INPUTS, &[META], &said.0).unwrap();
        sfs::tree::put(&mut txn, INPUTS, b"f/old", b"what it held").unwrap();
        sfs::tree::put(&mut txn, ASSETS, b"an asset", b"of then").unwrap();
        txn.commit().unwrap();
        assert!(files(&space.read().unwrap(), root).unwrap().is_empty());
        let now = files_of(root, 20);
        let mut txn = space.write().unwrap();
        fill_files(&mut txn, root, now.iter().map(|(path, record)| (path.as_path(), record))).unwrap();
        txn.commit().unwrap();
        let read = space.read().unwrap();
        assert_eq!(files(&read, root).unwrap(), now);
        assert_eq!(read.root(ASSETS), 0);
    }
}
