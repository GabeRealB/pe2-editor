//! A filesystem in the space's trees: folders and files by number, which knows nothing of projects.
//!
//! - `names`: a folder's number and a name, to the number of what has that name there - a lookup one descent, a
//!   listing one range, a folder renamed one key, whatever is inside it. And the same with the name folded, to
//!   the names written so there, each with its number: what a system that ignores case looks names up through.
//! - `nodes`: a number to what has it - a folder or a file, its length, when it changed, a version that goes up
//!   with every change of its bytes, flags its consumers give meaning to, the folder it is in and its name there,
//!   and where its bytes are: in the node itself, a file on disk, ranges of the log, or what its consumer derives
//!   them from. Numbers are given once and never again: they are what a system knows a file by.
//! - `changes`: what each commit made, changed, moved and took away, with who did it - what tells a kernel, a
//!   watcher or a holder what to look at again without comparing trees. Kept to a length - and of one commit,
//!   to half of it: who has read up to before what it still holds takes the tree whole (`changes` says so).
//!
//! Several such filesystems share a space, each in trees of its own (`Trees`).

use crate::space::{Error, Pages, Result, Txn};
use crate::tree;
use std::io;

/// The root folder's number.
pub const ROOT: u64 = 1;
/// How many changes are kept, at most, before the oldest half is let go of.
const KEPT: u64 = 8192;

/// Which of a space's trees a filesystem is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trees {
    pub names: usize,
    pub folded: usize,
    pub nodes: usize,
    pub changes: usize,
}

/// The tree a project is served as.
pub const SERVED: Trees = Trees { names: 0, folded: 1, nodes: 2, changes: 3 };
/// The files of a project's layers.
pub const STORED: Trees = Trees { names: 4, folded: 5, nodes: 6, changes: 7 };

/// Where a file's bytes are.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Bytes {
    /// A folder has none.
    #[default]
    None,
    /// In the node itself.
    Here(Vec<u8>),
    /// A file on disk, by its path.
    File(String),
    /// Ranges of the log: where each begins, and how long it is.
    Logged(Vec<(u64, u64)>),
    /// What its consumer derives them from, as the consumer wrote it.
    Derived(Vec<u8>),
}

/// A folder or a file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Node {
    pub folder: bool,
    /// What its consumers say of it: nothing here reads them.
    pub flags: u32,
    pub len: u64,
    /// When it last changed, in nanoseconds since 1970.
    pub modified: u64,
    /// Goes up with every change of its bytes: what was read of an earlier one is of that one.
    pub version: u64,
    /// The folder it is in, and its name there.
    pub parent: u64,
    pub name: String,
    pub bytes: Bytes,
}

fn invalid(what: &str) -> Error {
    io::Error::new(io::ErrorKind::InvalidInput, what.to_owned()).into()
}

fn text(out: &mut Vec<u8>, text: &[u8]) {
    out.extend_from_slice(&u32::try_from(text.len()).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(text);
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn byte(&mut self) -> Option<u8> {
        let (byte, rest) = self.0.split_first()?;
        self.0 = rest;
        Some(*byte)
    }

    fn u32(&mut self) -> Option<u32> {
        let (number, rest) = self.0.split_first_chunk::<4>()?;
        self.0 = rest;
        Some(u32::from_le_bytes(*number))
    }

    fn u64(&mut self) -> Option<u64> {
        let (number, rest) = self.0.split_first_chunk::<8>()?;
        self.0 = rest;
        Some(u64::from_le_bytes(*number))
    }

    fn bytes(&mut self) -> Option<&'a [u8]> {
        let len = usize::try_from(self.u32()?).ok()?;
        let (bytes, rest) = self.0.split_at_checked(len)?;
        self.0 = rest;
        Some(bytes)
    }

    fn text(&mut self) -> Option<String> {
        String::from_utf8(self.bytes()?.to_vec()).ok()
    }
}

impl Node {
    fn written(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64 + self.name.len());
        out.push(u8::from(self.folder));
        out.extend_from_slice(&self.flags.to_le_bytes());
        out.extend_from_slice(&self.len.to_le_bytes());
        out.extend_from_slice(&self.modified.to_le_bytes());
        out.extend_from_slice(&self.version.to_le_bytes());
        out.extend_from_slice(&self.parent.to_le_bytes());
        text(&mut out, self.name.as_bytes());
        match &self.bytes {
            Bytes::None => out.push(0),
            Bytes::Here(bytes) => {
                out.push(1);
                text(&mut out, bytes);
            }
            Bytes::File(path) => {
                out.push(2);
                text(&mut out, path.as_bytes());
            }
            Bytes::Logged(ranges) => {
                out.push(3);
                out.extend_from_slice(&u32::try_from(ranges.len()).unwrap_or(u32::MAX).to_le_bytes());
                for (at, len) in ranges {
                    out.extend_from_slice(&at.to_le_bytes());
                    out.extend_from_slice(&len.to_le_bytes());
                }
            }
            Bytes::Derived(from) => {
                out.push(4);
                text(&mut out, from);
            }
        }
        out
    }

    fn read(value: &[u8]) -> Option<Node> {
        let mut value = Reader(value);
        let folder = value.byte()? != 0;
        let (flags, len, modified, version, parent) = (value.u32()?, value.u64()?, value.u64()?, value.u64()?, value.u64()?);
        let name = value.text()?;
        let bytes = match value.byte()? {
            0 => Bytes::None,
            1 => Bytes::Here(value.bytes()?.to_vec()),
            2 => Bytes::File(value.text()?),
            3 => {
                let ranges = (0..value.u32()?).map(|_| Some((value.u64()?, value.u64()?)));
                Bytes::Logged(ranges.collect::<Option<_>>()?)
            }
            4 => Bytes::Derived(value.bytes()?.to_vec()),
            _ => return None,
        };
        Some(Node { folder, flags, len, modified, version, parent, name, bytes })
    }
}

/// What became of something in a commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum What {
    Made,
    /// Its bytes, or what is said of it.
    Changed,
    /// It was in this folder, under this name.
    Moved { from: u64, was: String },
    /// With all that was inside it, of which nothing more is said.
    Gone,
}

/// One thing a commit did: to what, and where that is - or, gone, was.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub what: What,
    pub number: u64,
    pub folder: bool,
    pub parent: u64,
    pub name: String,
    /// Who did it, as its consumers number themselves: a kernel is not told what it did itself.
    pub by: u32,
}

impl Change {
    fn written(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(40 + self.name.len());
        out.extend_from_slice(&self.by.to_le_bytes());
        out.extend_from_slice(&self.number.to_le_bytes());
        out.push(u8::from(self.folder));
        out.extend_from_slice(&self.parent.to_le_bytes());
        text(&mut out, self.name.as_bytes());
        match &self.what {
            What::Made => out.push(0),
            What::Changed => out.push(1),
            What::Moved { from, was } => {
                out.push(2);
                out.extend_from_slice(&from.to_le_bytes());
                text(&mut out, was.as_bytes());
            }
            What::Gone => out.push(3),
        }
        out
    }

    fn read(value: &[u8]) -> Option<Change> {
        let mut value = Reader(value);
        let (by, number, folder, parent, name) = (value.u32()?, value.u64()?, value.byte()? != 0, value.u64()?, value.text()?);
        let what = match value.byte()? {
            0 => What::Made,
            1 => What::Changed,
            2 => What::Moved { from: value.u64()?, was: value.text()? },
            3 => What::Gone,
            _ => return None,
        };
        Some(Change { what, number, folder, parent, name, by })
    }
}

/// What a filesystem keeps of itself, under the number no node has: the next number to give, and of its changes
/// how many it holds, up to which generation some were let go of, and how many the generation being written has.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Kept {
    next: u64,
    changes: u64,
    floor: u64,
    generation: u64,
    noted: u32,
}

impl Kept {
    fn of(pages: &impl Pages, trees: Trees) -> Result<Kept> {
        let Some(value) = tree::get(pages, trees.nodes, &0u64.to_be_bytes())? else {
            return Ok(Kept { next: ROOT + 1, ..Kept::default() });
        };
        let mut value = Reader(&value);
        let kept = (|| Some(Kept { next: value.u64()?, changes: value.u64()?, floor: value.u64()?, generation: value.u64()?, noted: value.u32()? }))();
        kept.ok_or_else(|| pages.damaged("what a filesystem keeps of itself is not whole".into()))
    }

    fn put(&self, txn: &mut Txn<'_>, trees: Trees) -> Result<()> {
        let mut out = Vec::with_capacity(36);
        for number in [self.next, self.changes, self.floor, self.generation] {
            out.extend_from_slice(&number.to_le_bytes());
        }
        out.extend_from_slice(&self.noted.to_le_bytes());
        tree::put(txn, trees.nodes, &0u64.to_be_bytes(), &out)
    }
}

fn name_key(folder: u64, name: &str) -> Vec<u8> {
    let mut key = Vec::with_capacity(8 + name.len());
    key.extend_from_slice(&folder.to_be_bytes());
    key.extend_from_slice(name.as_bytes());
    key
}

/// A name as systems that ignore case compare it.
#[must_use]
pub fn folded(name: &str) -> String {
    name.chars().flat_map(char::to_lowercase).collect()
}

fn number_of(pages: &impl Pages, value: &[u8]) -> Result<u64> {
    let number = value.try_into().map(u64::from_be_bytes);
    number.map_err(|_| pages.damaged("a name of a filesystem leads to no number".into()))
}

/// The number of what a folder has under a name; none where it has nothing of that name.
///
/// # Errors
/// The space is damaged.
pub fn lookup(pages: &impl Pages, trees: Trees, folder: u64, name: &str) -> Result<Option<u64>> {
    match tree::get(pages, trees.names, &name_key(folder, name))? {
        Some(value) => number_of(pages, &value).map(Some),
        None => Ok(None),
    }
}

/// The names a folder has that fold to one, each with its number, as the folded tree's value holds them.
fn folded_names(pages: &impl Pages, value: &[u8]) -> Result<Vec<(String, u64)>> {
    let mut value = Reader(value);
    let mut names = Vec::new();
    while !value.0.is_empty() {
        let read = (|| Some((value.text()?, value.u64()?)))();
        names.push(read.ok_or_else(|| pages.damaged("the names of a filesystem that fold to one are not whole".into()))?);
    }
    Ok(names)
}

/// What a folder has under a name whatever its case: the name as it is written, and its number - the first of
/// them, where several differ by case alone.
///
/// # Errors
/// The space is damaged.
pub fn lookup_folded(pages: &impl Pages, trees: Trees, folder: u64, name: &str) -> Result<Option<(String, u64)>> {
    let Some(value) = tree::get(pages, trees.folded, &name_key(folder, &folded(name)))? else { return Ok(None) };
    Ok(folded_names(pages, &value)?.into_iter().next())
}

/// What has a number; none where nothing does.
///
/// # Errors
/// The space is damaged.
pub fn node(pages: &impl Pages, trees: Trees, number: u64) -> Result<Option<Node>> {
    if number == 0 {
        return Ok(None);
    }
    if number == ROOT {
        // The root is there in a filesystem nothing was made in yet.
        let root = tree::get(pages, trees.nodes, &number.to_be_bytes())?;
        return match root {
            Some(value) => Node::read(&value).map(Some).ok_or_else(|| pages.damaged("the root of a filesystem is not whole".into())),
            None => Ok(Some(Node { folder: true, ..Node::default() })),
        };
    }
    match tree::get(pages, trees.nodes, &number.to_be_bytes())? {
        Some(value) => Node::read(&value).map(Some).ok_or_else(|| pages.damaged(format!("node {number} of a filesystem is not whole"))),
        None => Ok(None),
    }
}

/// The names in a folder, in order, after one - or from the first - each with its number, given to `each` until it
/// says to stop.
///
/// # Errors
/// The space is damaged.
pub fn list(pages: &impl Pages, trees: Trees, folder: u64, after: Option<&str>, each: &mut dyn FnMut(&str, u64) -> bool) -> Result<()> {
    let prefix = folder.to_be_bytes();
    let from = after.map_or_else(|| prefix.to_vec(), |after| name_key(folder, after));
    let mut torn = false;
    tree::scan(pages, trees.names, &from, &mut |key, value| {
        let Some(name) = key.strip_prefix(&prefix) else { return false };
        if after.is_some_and(|after| after.as_bytes() == name) {
            return true;
        }
        if let (Ok(name), Ok(number)) = (std::str::from_utf8(name), <[u8; 8]>::try_from(value)) {
            each(name, u64::from_be_bytes(number))
        } else {
            torn = true;
            false
        }
    })?;
    if torn {
        return Err(pages.damaged(format!("a name in folder {folder} of a filesystem is not whole")));
    }
    Ok(())
}

/// The path of what has a number, from the root, its parts joined by `/`; none where nothing has it.
///
/// # Errors
/// The space is damaged.
pub fn path(pages: &impl Pages, trees: Trees, number: u64) -> Result<Option<String>> {
    let mut parts = Vec::new();
    let mut at = number;
    while at != ROOT {
        let Some(node) = node(pages, trees, at)? else { return Ok(None) };
        if parts.len() > 4096 {
            return Err(pages.damaged(format!("node {number} of a filesystem is inside itself")));
        }
        parts.push(node.name);
        at = node.parent;
    }
    parts.reverse();
    Ok(Some(parts.join("/")))
}

/// The number of what is at a path from the root; none where nothing is.
///
/// # Errors
/// The space is damaged.
pub fn at(pages: &impl Pages, trees: Trees, path: &str) -> Result<Option<u64>> {
    let mut number = ROOT;
    for part in path.split('/').filter(|part| !part.is_empty()) {
        match lookup(pages, trees, number, part)? {
            Some(inside) => number = inside,
            None => return Ok(None),
        }
    }
    Ok(Some(number))
}

/// Say what a commit did.
fn note(txn: &mut Txn<'_>, trees: Trees, kept: &mut Kept, change: &Change) -> Result<()> {
    if kept.generation != txn.generation() {
        kept.generation = txn.generation();
        kept.noted = 0;
    }
    kept.noted += 1;
    if u64::from(kept.noted) > KEPT / 2 {
        // One commit that changes this much is not followed change by change: who is before it takes the tree
        // whole.
        kept.floor = kept.generation;
        return Ok(());
    }
    let mut key = kept.generation.to_be_bytes().to_vec();
    key.extend_from_slice(&kept.noted.to_be_bytes());
    tree::put(txn, trees.changes, &key, &change.written())?;
    kept.changes += 1;
    if kept.changes > KEPT {
        // The oldest half goes, whole generations of it: who had read up to before them takes the tree whole.
        let mut oldest = Vec::new();
        let mut last = 0u64;
        let goes = usize::try_from(KEPT / 2).unwrap_or(usize::MAX);
        tree::scan(txn, trees.changes, b"", &mut |key, _| {
            let generation = key.first_chunk::<8>().map_or(0, |generation| u64::from_be_bytes(*generation));
            if oldest.len() >= goes && generation != last {
                return false;
            }
            last = generation;
            oldest.push(key.to_vec());
            // Never those of the commit being made.
            generation != kept.generation
        })?;
        oldest.retain(|key| key.first_chunk::<8>().is_some_and(|generation| u64::from_be_bytes(*generation) != kept.generation));
        for key in &oldest {
            tree::remove(txn, trees.changes, key)?;
            kept.changes -= 1;
        }
        if let Some(generation) = oldest.last().and_then(|key| key.first_chunk::<8>()) {
            kept.floor = kept.floor.max(u64::from_be_bytes(*generation));
        }
    }
    Ok(())
}

/// What the commits after a generation did, in order, each with its generation, given to `each` until it says to
/// stop. False where some of it is no longer kept: who asks has to take the tree whole.
///
/// # Errors
/// The space is damaged.
pub fn changes(pages: &impl Pages, trees: Trees, after: u64, each: &mut dyn FnMut(u64, Change) -> bool) -> Result<bool> {
    if after < Kept::of(pages, trees)?.floor {
        return Ok(false);
    }
    let mut torn = false;
    tree::scan(pages, trees.changes, &(after + 1).to_be_bytes(), &mut |key, value| {
        if let (Some(generation), Some(change)) = (key.first_chunk::<8>(), Change::read(value)) {
            each(u64::from_be_bytes(*generation), change)
        } else {
            torn = true;
            false
        }
    })?;
    if torn {
        return Err(pages.damaged("a change of a filesystem is not whole".into()));
    }
    Ok(true)
}

fn put_node(txn: &mut Txn<'_>, trees: Trees, number: u64, node: &Node) -> Result<()> {
    tree::put(txn, trees.nodes, &number.to_be_bytes(), &node.written())
}

/// Say, or say no more (`number` none), that a folder has a name, among the names there that fold to the same.
fn fold_name(txn: &mut Txn<'_>, trees: Trees, folder: u64, name: &str, number: Option<u64>) -> Result<()> {
    let key = name_key(folder, &folded(name));
    let mut names = match tree::get(txn, trees.folded, &key)? {
        Some(value) => folded_names(txn, &value)?,
        None => Vec::new(),
    };
    names.retain(|(written, _)| written != name);
    if let Some(number) = number {
        names.push((name.to_owned(), number));
    }
    if names.is_empty() {
        tree::remove(txn, trees.folded, &key)?;
        return Ok(());
    }
    let mut value = Vec::new();
    for (written, number) in &names {
        text(&mut value, written.as_bytes());
        value.extend_from_slice(&number.to_le_bytes());
    }
    tree::put(txn, trees.folded, &key, &value)
}

fn put_name(txn: &mut Txn<'_>, trees: Trees, folder: u64, name: &str, number: u64) -> Result<()> {
    tree::put(txn, trees.names, &name_key(folder, name), &number.to_be_bytes())?;
    fold_name(txn, trees, folder, name, Some(number))
}

fn take_name(txn: &mut Txn<'_>, trees: Trees, folder: u64, name: &str) -> Result<()> {
    tree::remove(txn, trees.names, &name_key(folder, name))?;
    fold_name(txn, trees, folder, name, None)
}

/// A folder to put something into under a name: there, a folder, and with nothing of that name.
fn free_in(txn: &Txn<'_>, trees: Trees, folder: u64, name: &str) -> Result<()> {
    if name.is_empty() || name.contains(['/', '\0']) || name.len() > tree::KEY - 8 {
        return Err(invalid("no name a filesystem holds"));
    }
    if !node(txn, trees, folder)?.is_some_and(|node| node.folder) {
        return Err(io::Error::new(io::ErrorKind::NotFound, "no such folder").into());
    }
    if lookup(txn, trees, folder, name)?.is_some() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, "the folder has that name already").into());
    }
    Ok(())
}

/// Make a folder or a file in the folder and under the name it says: its number, given to nothing before.
///
/// # Errors
/// The folder is none, or has that name already; the name is none a filesystem holds; or the space cannot take it.
pub fn make(txn: &mut Txn<'_>, trees: Trees, node: &Node, by: u32) -> Result<u64> {
    free_in(txn, trees, node.parent, &node.name)?;
    let mut kept = Kept::of(txn, trees)?;
    let number = kept.next;
    kept.next += 1;
    put_node(txn, trees, number, node)?;
    put_name(txn, trees, node.parent, &node.name, number)?;
    let change = Change { what: What::Made, number, folder: node.folder, parent: node.parent, name: node.name.clone(), by };
    note(txn, trees, &mut kept, &change)?;
    kept.put(txn, trees)?;
    Ok(number)
}

/// Say anew what has a number: its length, its bytes, when it changed, its flags. Where it is and whether it is a
/// folder stay as they are, whatever is given.
///
/// # Errors
/// Nothing has the number; or the space cannot take it.
pub fn set(txn: &mut Txn<'_>, trees: Trees, number: u64, now: &Node, by: u32) -> Result<()> {
    let Some(was) = node(txn, trees, number)? else {
        return Err(io::Error::new(io::ErrorKind::NotFound, "no such node").into());
    };
    let now = Node { folder: was.folder, parent: was.parent, name: was.name, ..now.clone() };
    put_node(txn, trees, number, &now)?;
    let mut kept = Kept::of(txn, trees)?;
    let change = Change { what: What::Changed, number, folder: now.folder, parent: now.parent, name: now.name, by };
    note(txn, trees, &mut kept, &change)?;
    kept.put(txn, trees)
}

/// Move what has a number into a folder under a name: one key, whatever is inside it.
///
/// # Errors
/// Nothing has the number; the folder is none, is inside what is moved, or has that name already.
pub fn rename(txn: &mut Txn<'_>, trees: Trees, number: u64, folder: u64, name: &str, by: u32) -> Result<()> {
    let Some(mut moved) = node(txn, trees, number).ok().flatten().filter(|_| number != ROOT) else {
        return Err(io::Error::new(io::ErrorKind::NotFound, "no such node").into());
    };
    free_in(txn, trees, folder, name)?;
    // Not into itself.
    let mut above = folder;
    while above != ROOT {
        if above == number {
            return Err(invalid("a folder moved into itself"));
        }
        above = node(txn, trees, above)?.map_or(ROOT, |node| node.parent);
    }
    take_name(txn, trees, moved.parent, &moved.name)?;
    let (from, was) = (moved.parent, std::mem::replace(&mut moved.name, name.to_owned()));
    moved.parent = folder;
    put_node(txn, trees, number, &moved)?;
    put_name(txn, trees, folder, name, number)?;
    let mut kept = Kept::of(txn, trees)?;
    let change = Change { what: What::Moved { from, was }, number, folder: moved.folder, parent: folder, name: moved.name, by };
    note(txn, trees, &mut kept, &change)?;
    kept.put(txn, trees)
}

/// Take away what has a number, with all that is inside it: how many nodes went.
///
/// # Errors
/// Nothing has the number, or it is the root; or the space is damaged.
pub fn remove(txn: &mut Txn<'_>, trees: Trees, number: u64, by: u32) -> Result<usize> {
    let Some(gone) = node(txn, trees, number).ok().flatten().filter(|_| number != ROOT) else {
        return Err(io::Error::new(io::ErrorKind::NotFound, "no such node").into());
    };
    take_name(txn, trees, gone.parent, &gone.name)?;
    // It and what is inside it, folder by folder.
    let mut went = 0;
    let mut folders = vec![(number, gone.folder)];
    while let Some((number, folder)) = folders.pop() {
        if folder {
            let mut inside = Vec::new();
            list(txn, trees, number, None, &mut |name, number| {
                inside.push((name.to_owned(), number));
                true
            })?;
            for (name, inner) in inside {
                tree::remove(txn, trees.names, &name_key(number, &name))?;
                tree::remove(txn, trees.folded, &name_key(number, &folded(&name)))?;
                folders.push((inner, node(txn, trees, inner)?.is_some_and(|node| node.folder)));
            }
        }
        tree::remove(txn, trees.nodes, &number.to_be_bytes())?;
        went += 1;
    }
    let mut kept = Kept::of(txn, trees)?;
    let change = Change { what: What::Gone, number, folder: gone.folder, parent: gone.parent, name: gone.name, by };
    note(txn, trees, &mut kept, &change)?;
    kept.put(txn, trees)?;
    Ok(went)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::space::Space;
    use std::collections::BTreeMap;

    fn file(parent: u64, name: &str, bytes: &[u8]) -> Node {
        Node { parent, name: name.into(), len: bytes.len() as u64, bytes: Bytes::Here(bytes.to_vec()), ..Node::default() }
    }

    fn folder(parent: u64, name: &str) -> Node {
        Node { folder: true, parent, name: name.into(), ..Node::default() }
    }

    /// All a filesystem holds, by path: a folder none, a file its bytes.
    fn all(pages: &impl Pages, trees: Trees) -> BTreeMap<String, Option<Vec<u8>>> {
        let mut out = BTreeMap::new();
        let mut folders = vec![ROOT];
        while let Some(number) = folders.pop() {
            list(pages, trees, number, None, &mut |_, inside| {
                let node = node(pages, trees, inside).unwrap().unwrap();
                let path = path(pages, trees, inside).unwrap().unwrap();
                assert_eq!(at(pages, trees, &path).unwrap(), Some(inside));
                if let Bytes::Here(bytes) = node.bytes {
                    out.insert(path, Some(bytes));
                } else {
                    folders.push(inside);
                    out.insert(path, None);
                }
                true
            })
            .unwrap();
        }
        out
    }

    #[test]
    fn names_lead_to_numbers_and_numbers_to_what_has_them() {
        let space = Space::private().unwrap();
        let mut txn = space.write().unwrap();
        let rooms = make(&mut txn, SERVED, &folder(ROOT, "Rooms"), 1).unwrap();
        let patio = make(&mut txn, SERVED, &folder(rooms, "patio"), 1).unwrap();
        let toml = make(&mut txn, SERVED, &file(patio, "node.toml", b"type = 'room'"), 1).unwrap();
        make(&mut txn, SERVED, &file(patio, "Room.toml", b"x"), 1).unwrap();
        // Another filesystem of the same space knows nothing of it.
        make(&mut txn, STORED, &file(ROOT, "record.toml", b"y"), 1).unwrap();
        assert!(matches!(make(&mut txn, SERVED, &file(patio, "node.toml", b""), 1), Err(Error::Io(e)) if e.kind() == io::ErrorKind::AlreadyExists));
        assert!(matches!(make(&mut txn, SERVED, &file(toml, "in a file", b""), 1), Err(Error::Io(e)) if e.kind() == io::ErrorKind::NotFound));
        assert!(make(&mut txn, SERVED, &file(patio, "a/b", b""), 1).is_err());
        txn.commit().unwrap();

        let read = space.read().unwrap();
        assert_eq!(lookup(&read, SERVED, ROOT, "Rooms").unwrap(), Some(rooms));
        assert_eq!(lookup(&read, SERVED, ROOT, "rooms").unwrap(), None);
        assert_eq!(lookup_folded(&read, SERVED, ROOT, "ROOMS").unwrap(), Some(("Rooms".into(), rooms)));
        assert_eq!(at(&read, SERVED, "Rooms/patio/node.toml").unwrap(), Some(toml));
        assert_eq!(at(&read, SERVED, "Rooms/none").unwrap(), None);
        assert_eq!(path(&read, SERVED, toml).unwrap().as_deref(), Some("Rooms/patio/node.toml"));
        assert_eq!(node(&read, SERVED, toml).unwrap().unwrap().bytes, Bytes::Here(b"type = 'room'".to_vec()));
        assert!(node(&read, SERVED, ROOT).unwrap().unwrap().folder);
        let mut names = Vec::new();
        list(&read, SERVED, patio, None, &mut |name, _| {
            names.push(name.to_owned());
            true
        })
        .unwrap();
        assert_eq!(names, ["Room.toml", "node.toml"]);
        names.clear();
        list(&read, SERVED, patio, Some("Room.toml"), &mut |name, _| {
            names.push(name.to_owned());
            true
        })
        .unwrap();
        assert_eq!(names, ["node.toml"], "from after a name on");
        assert_eq!(all(&read, STORED).keys().collect::<Vec<_>>(), ["record.toml"]);
    }

    #[test]
    fn a_folder_renamed_is_one_key_and_one_removed_goes_with_what_is_in_it() {
        let space = Space::private().unwrap();
        let mut txn = space.write().unwrap();
        let a = make(&mut txn, SERVED, &folder(ROOT, "a"), 1).unwrap();
        let b = make(&mut txn, SERVED, &folder(a, "b"), 1).unwrap();
        let mut files = Vec::new();
        for n in 0..300 {
            files.push(make(&mut txn, SERVED, &file(b, &format!("file-{n:03}.txt"), format!("{n}").as_bytes()), 1).unwrap());
        }
        let other = make(&mut txn, SERVED, &folder(ROOT, "other"), 1).unwrap();
        txn.commit().unwrap();
        let before = space.usage().unwrap();

        let mut txn = space.write().unwrap();
        assert!(rename(&mut txn, SERVED, a, b, "inside itself", 1).is_err());
        assert!(rename(&mut txn, SERVED, a, ROOT, "other", 1).is_err(), "onto a name the folder has");
        rename(&mut txn, SERVED, a, other, "A", 1).unwrap();
        txn.commit().unwrap();
        let read = space.read().unwrap();
        // Every number as it was, at its new path.
        assert_eq!(path(&read, SERVED, files[7]).unwrap().as_deref(), Some("other/A/b/file-007.txt"));
        assert_eq!(at(&read, SERVED, "a").unwrap(), None);
        assert_eq!(lookup_folded(&read, SERVED, other, "a").unwrap(), Some(("A".into(), a)));
        assert_eq!(all(&read, SERVED).len(), 303);
        drop(read);
        // Whatever is inside it, a handful of pages changed.
        let after = space.usage().unwrap();
        assert!(after.pages - before.pages < 16, "a rename wrote {} pages", after.pages - before.pages);

        let mut txn = space.write().unwrap();
        assert_eq!(remove(&mut txn, SERVED, a, 1).unwrap(), 302);
        assert!(remove(&mut txn, SERVED, ROOT, 1).is_err());
        let again = make(&mut txn, SERVED, &folder(ROOT, "a"), 1).unwrap();
        txn.commit().unwrap();
        let read = space.read().unwrap();
        assert!(again > *files.last().unwrap() && again > other, "a number given again");
        assert_eq!(node(&read, SERVED, files[7]).unwrap(), None);
        assert_eq!(all(&read, SERVED).keys().collect::<Vec<_>>(), ["a", "other"]);
    }

    #[test]
    fn names_that_differ_by_case_alone_are_each_found() {
        let space = Space::private().unwrap();
        let mut txn = space.write().unwrap();
        let lower = make(&mut txn, SERVED, &file(ROOT, "readme.md", b"l"), 1).unwrap();
        let upper = make(&mut txn, SERVED, &file(ROOT, "README.md", b"u"), 1).unwrap();
        assert_eq!(lookup(&txn, SERVED, ROOT, "readme.md").unwrap(), Some(lower));
        assert_eq!(lookup(&txn, SERVED, ROOT, "README.md").unwrap(), Some(upper));
        // Whatever its case, one of them; and once that one is gone, the other.
        let (found, _) = lookup_folded(&txn, SERVED, ROOT, "ReadMe.MD").unwrap().unwrap();
        let (gone, stays) = if found == "README.md" { (upper, lower) } else { (lower, upper) };
        remove(&mut txn, SERVED, gone, 1).unwrap();
        assert_eq!(lookup_folded(&txn, SERVED, ROOT, "ReadMe.MD").unwrap().map(|(_, number)| number), Some(stays));
        remove(&mut txn, SERVED, stays, 1).unwrap();
        assert_eq!(lookup_folded(&txn, SERVED, ROOT, "ReadMe.MD").unwrap(), None);
    }

    #[test]
    fn what_each_commit_did_is_read_after_it_by_who_did_not_do_it() {
        let space = Space::private().unwrap();
        let mut txn = space.write().unwrap();
        let a = make(&mut txn, SERVED, &folder(ROOT, "a"), 7).unwrap();
        let f = make(&mut txn, SERVED, &file(a, "f", b"1"), 7).unwrap();
        txn.commit().unwrap();
        let first = space.read().unwrap().commit().generation;
        let mut txn = space.write().unwrap();
        set(&mut txn, SERVED, f, &Node { len: 2, version: 1, bytes: Bytes::Here(b"22".to_vec()), ..Node::default() }, 9).unwrap();
        rename(&mut txn, SERVED, f, ROOT, "g", 9).unwrap();
        remove(&mut txn, SERVED, a, 7).unwrap();
        txn.commit().unwrap();

        let read = space.read().unwrap();
        // What `set` leaves be: where it is.
        let now = node(&read, SERVED, f).unwrap().unwrap();
        assert_eq!((now.parent, now.name.as_str(), now.version, &now.bytes), (ROOT, "g", 1, &Bytes::Here(b"22".to_vec())));
        let mut since = Vec::new();
        assert!(changes(&read, SERVED, first, &mut |_, change| {
            since.push((change.what, change.number, change.parent, change.name, change.by));
            true
        })
        .unwrap());
        assert_eq!(
            since,
            [
                (What::Changed, f, a, "f".to_owned(), 9),
                (What::Moved { from: a, was: "f".into() }, f, ROOT, "g".to_owned(), 9),
                (What::Gone, a, ROOT, "a".to_owned(), 7),
            ]
        );
        let mut all = 0;
        assert!(changes(&read, SERVED, 0, &mut |_, _| {
            all += 1;
            true
        })
        .unwrap());
        assert_eq!(all, 5);
    }

    #[test]
    fn a_commit_that_changes_very_much_is_not_followed_change_by_change() {
        let space = Space::private().unwrap();
        let mut txn = space.write().unwrap();
        let first = make(&mut txn, SERVED, &folder(ROOT, "first"), 1).unwrap();
        txn.commit().unwrap();
        let before = space.read().unwrap().commit().generation;
        let mut txn = space.write().unwrap();
        for n in 0..6000 {
            make(&mut txn, SERVED, &file(first, &format!("{n}"), b""), 1).unwrap();
        }
        txn.commit().unwrap();
        let after = space.read().unwrap().commit().generation;
        let mut txn = space.write().unwrap();
        make(&mut txn, SERVED, &folder(ROOT, "later"), 1).unwrap();
        txn.commit().unwrap();
        let read = space.read().unwrap();
        let mut count = 0;
        // Before it: the tree whole. After it: what came after.
        assert!(!changes(&read, SERVED, before, &mut |_, _| true).unwrap());
        assert!(changes(&read, SERVED, after, &mut |_, _| {
            count += 1;
            true
        })
        .unwrap());
        assert_eq!(count, 1);
        assert!(Kept::of(&read, SERVED).unwrap().changes <= KEPT / 2 + 2);
    }

    #[test]
    fn changes_are_kept_to_a_length_and_who_is_behind_them_is_told() {
        let space = Space::private().unwrap();
        let mut generations = Vec::new();
        for round in 0..40u64 {
            let mut txn = space.write().unwrap();
            for n in 0..300 {
                make(&mut txn, SERVED, &file(ROOT, &format!("{round}-{n}"), b""), 1).unwrap();
            }
            txn.commit().unwrap();
            generations.push(space.read().unwrap().commit().generation);
        }
        let read = space.read().unwrap();
        let count = |after: u64| {
            let mut count = 0u64;
            let whole = changes(&read, SERVED, after, &mut |_, _| {
                count += 1;
                true
            })
            .unwrap();
            (whole, count)
        };
        // Behind what is kept: told, with nothing read as if it were all.
        assert!(!count(0).0 && !count(generations[3]).0);
        // Within it: every change since, none missing.
        assert_eq!(count(generations[38]), (true, 300));
        assert_eq!(count(generations[30]), (true, 9 * 300));
        let kept = Kept::of(&read, SERVED).unwrap();
        assert!(kept.changes <= KEPT && kept.changes >= KEPT / 2, "{kept:?}");
        assert_eq!(count(kept.floor), (true, kept.changes));
        assert!(!count(kept.floor - 1).0);
    }
}
