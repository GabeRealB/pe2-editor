//! What is served of a project, kept in the filesystem its processes share (`sfs::fs::SERVED`): whoever holds the
//! project says there what it serves - all of it where the shared space holds none of it (`show_whole`), and after
//! an edit what the edit changed (`show`, of `Index::serve_changed`) - for whoever serves the folder to read with
//! no word from the holder but that it did (`server::ToServer::Shown`): the server reads what changed since it
//! last read (`changed`), or all of it (`listed`).
//!
//! A listing's path is a folder or a file of that filesystem. A project node is a folder that says so (`NODE`);
//! a folder a node's files made, no entry of its own, says that (`OF_FILES`) and goes with the last of what is in
//! it; a file says where its content lives as the listing does (`Shown::File`), for the server to read it knowing
//! nothing else of the project.

use crate::server::{Delta, Listing, Shown, Source};
use sfs::fs::{self, Bytes, Node, ROOT, SERVED};
use sfs::space::{Pages, Result, Txn};
use std::path::{Path, PathBuf};

/// A folder that is a project node (an asset): its files in it beside what is inside it.
pub const NODE: u32 = 1;
/// A folder a node's files made, no entry of its own; a file that is a node's.
pub const OF_FILES: u32 = 2;
/// Who says a change: whoever holds the project.
pub const HOLDER: u32 = 1;
/// Whoever serves the folder, changing what is served as a program asked through it - before the project has it
/// so: a folder made, a path moved, a thing removed.
pub const SERVER: u32 = 2;

/// A node flag: not the project's - made through the folder by whoever serves it, the project yet to take it, or
/// never to. No listing of what the project serves has it; shown by whoever holds the project, it is the
/// project's from then on.
pub const NEW: u32 = 4;
/// A node flag: removed through the folder, and shown there no more, while the project still has it - until it
/// takes the removal, or refuses it. Kept whatever the project says of the node meanwhile.
pub const GONE: u32 = 8;
/// A node flag, with `NEW`: a folder made in a node, waiting for what is written in it to say what it is.
pub const WAITS: u32 = 16;
/// A node flag, with `NEW`: never the project's - what an editor leaves beside a file.
pub const JUNK: u32 = 32;
/// What whoever serves the folder says of a node that is not the project's, and goes when the project has it.
const NOT_YET: u32 = NEW | WAITS | JUNK;

/// The tree of what was moved through the folder and the project is yet to take: a number - what the move is due
/// by, in the order moves were made - to where it was and where it is now.
pub const MOVES: usize = 11;

/// The shared space's folder, in the cache's folder of what serving keeps.
pub const SPACE: &str = "space";

/// The space the processes of the project in `root` share, attached here.
///
/// # Errors
/// As `sfs::space::shared`.
pub fn space(root: &Path) -> std::io::Result<std::sync::Arc<sfs::Space>> {
    sfs::space::shared(&root.join(crate::cache::DIR).join(crate::cache::STAGING).join(SPACE))
}

/// The pages kept of what is read of the files of the project in `root` (`sfs::ring`), of about this many bytes:
/// in memory the system keeps for its processes by a name of the project's folder, or - where it has none to
/// name, or cannot give it - in this process's own. Cleared where this process has the shared space alone, a
/// slot some process died writing a slot again.
///
/// # Errors
/// There is no memory for it.
pub fn ring(root: &Path, space: &sfs::Space, bytes: u64) -> std::io::Result<sfs::ring::Ring> {
    // What a process of any project left there as it died goes first: it is taken until it is removed.
    sfs::ring::sweep("pe2-ring-");
    let named = crate::extract::sha1_hex(root.to_string_lossy().as_bytes());
    let ring = match sfs::ring::Ring::shared(&format!("pe2-ring-{}", &named[..16]), bytes) {
        Ok(ring) => ring,
        Err(_) => sfs::ring::Ring::private(bytes)?,
    };
    if space.fresh() {
        ring.clear();
    }
    Ok(ring)
}

/// Leave what this process shares with the others of its projects as whoever comes next, alone, may trust it
/// (`sfs::space::rest_all`): what a process does last as it ends in order.
pub fn rest_all() {
    if let Err(e) = sfs::space::rest_all() {
        eprintln!("the shared space is not left to be trusted: {e}");
    }
}

fn now() -> u64 {
    let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    u64::try_from(since.as_nanos()).unwrap_or(u64::MAX)
}

/// Say all that is served in a space, as one commit (`show_whole`).
///
/// # Errors
/// As `show`.
pub fn say_whole(space: &sfs::Space, listing: &Listing) -> Result<()> {
    let mut txn = space.write()?;
    show_whole(&mut txn, listing, now())?;
    txn.commit()
}

/// Say what changed of what is served in a space, as one commit (`show`).
///
/// # Errors
/// As `show`.
pub fn say(space: &sfs::Space, delta: &Delta) -> Result<()> {
    say_with(space, delta, &mut |_| Ok(()))
}

/// As `say`, with what else its commit says: what whoever says it has to say of the same moment - what became of
/// what was due (`vfs::due`), which is so the moment the folder shows it.
///
/// # Errors
/// As `show`, and what `also` could not do.
pub fn say_with(space: &sfs::Space, delta: &Delta, also: &mut dyn FnMut(&mut Txn<'_>) -> Result<()>) -> Result<()> {
    let mut txn = space.write()?;
    // First: a move the project took is no longer one its paths are read through (`moved`), and what is said
    // here is said of the project with that move made.
    also(&mut txn)?;
    show(&mut txn, delta, now())?;
    txn.commit()
}

/// Something was moved through the folder, in what is served, and the project is yet to take the move: from here
/// on what whoever holds the project says of a path at or under where it was is said of where it is now - until
/// the project took the move (`unmoved`, in the commit that says so) or refused it.
///
/// # Errors
/// The shared space cannot take it.
pub fn moved(txn: &mut Txn<'_>, due: u64, from: &str, to: &str) -> Result<()> {
    let mut said = Vec::new();
    text(&mut said, from);
    text(&mut said, to);
    sfs::tree::put(txn, MOVES, &due.to_be_bytes(), &said)
}

/// A move made through the folder is one the project has, or one it refused and that was put back: its paths are
/// read through it no more. Whether it was one.
///
/// # Errors
/// The shared space cannot take it.
pub fn unmoved(txn: &mut Txn<'_>, due: u64) -> Result<bool> {
    sfs::tree::remove(txn, MOVES, &due.to_be_bytes())
}

/// The moves the project is yet to take, in the order they were made.
fn moves(pages: &impl Pages) -> Result<Vec<(String, String)>> {
    Ok(moves_said(pages)?.into_iter().map(|(_, from, to)| (from, to)).collect())
}

/// The moves the project is yet to take, in the order they were made: what each is known by, where it was, where
/// it is.
///
/// # Errors
/// The shared space is damaged.
pub fn moves_said(pages: &impl Pages) -> Result<Vec<(u64, String, String)>> {
    let mut moves = Vec::new();
    if pages.root(MOVES) == 0 {
        return Ok(moves);
    }
    sfs::tree::scan(pages, MOVES, &[], &mut |key, mut said| {
        let due = <[u8; 8]>::try_from(key).map_or(0, u64::from_be_bytes);
        let mut text = || {
            let len = usize::try_from(u32::from_le_bytes(said.get(..4)?.try_into().ok()?)).ok()?;
            let text = std::str::from_utf8(said.get(4..4 + len)?).ok()?.to_owned();
            said = &said[4 + len..];
            Some(text)
        };
        if let (Some(from), Some(to)) = (text(), text()) {
            moves.push((due, from, to));
        }
        true
    })?;
    Ok(moves)
}

/// A path as the project has it, where it is in what is served: through the moves the project is yet to take.
fn through(moves: &[(String, String)], path: &str) -> String {
    let mut path = std::borrow::Cow::Borrowed(path);
    for (from, to) in moves {
        if let Some(rest) = path.strip_prefix(from.as_str()).filter(|rest| rest.is_empty() || rest.starts_with('/')) {
            path = std::borrow::Cow::Owned(format!("{to}{rest}"));
        }
    }
    path.into_owned()
}

fn nanos((secs, nanos): (u64, u32)) -> u64 {
    secs.saturating_mul(1_000_000_000).saturating_add(u64::from(nanos))
}

fn text(out: &mut Vec<u8>, text: &str) {
    out.extend_from_slice(&u32::try_from(text.len()).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(text.as_bytes());
}

/// Where a file's content lives, as a node of the shared filesystem says it.
fn place(disk: &Path, source: &Source) -> Vec<u8> {
    let mut out = Vec::new();
    match source {
        Source::Disk => {
            out.push(0);
            text(&mut out, &disk.to_string_lossy());
        }
        Source::RecordText { record, name } => {
            out.push(1);
            text(&mut out, &disk.to_string_lossy());
            text(&mut out, &record.to_string_lossy());
            text(&mut out, name);
        }
        Source::Stored { sha1, store } => {
            out.push(2);
            text(&mut out, &disk.to_string_lossy());
            text(&mut out, sha1);
            text(&mut out, &store.to_string_lossy());
        }
    }
    out
}

/// Where a file's content lives, read back: where a layer holds it, and its source.
#[must_use]
pub fn placed(said: &[u8]) -> Option<(PathBuf, Source)> {
    let (kind, mut rest) = said.split_first()?;
    let mut text = || {
        let (len, after) = rest.split_first_chunk::<4>()?;
        let (text, after) = after.split_at_checked(usize::try_from(u32::from_le_bytes(*len)).ok()?)?;
        rest = after;
        String::from_utf8(text.to_vec()).ok()
    };
    let disk = PathBuf::from(text()?);
    let source = match kind {
        0 => Source::Disk,
        1 => Source::RecordText { record: PathBuf::from(text()?), name: text()? },
        2 => Source::Stored { sha1: text()?, store: PathBuf::from(text()?) },
        _ => return None,
    };
    Some((disk, source))
}

fn above(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}

/// The folder the path before was in: paths are said in order, and the next is mostly in the same. None after
/// anything was taken away.
type Last = Option<(String, u64)>;

/// The folder a folder has under a name, made where there is none - of a node's files, where it is made for one.
/// What a file holds the place of gives way.
fn folder_in(txn: &mut Txn<'_>, at: u64, name: &str, of_files: bool, last: &mut Last) -> Result<u64> {
    match (fs::lookup(txn, SERVED, at, name)?, of_files) {
        (Some(there), _) if fs::node(txn, SERVED, there)?.is_some_and(|node| node.folder) => {
            // One made through the folder, in a node: what a file of the node's was written in it makes it.
            if of_files && let Some(node) = fs::node(txn, SERVED, there)?.filter(|node| node.flags & NOT_YET != 0) {
                fs::set(txn, SERVED, there, &Node { flags: OF_FILES | (node.flags & GONE), ..node }, HOLDER)?;
            }
            return Ok(there);
        }
        (Some(file), _) => {
            fs::remove(txn, SERVED, file, HOLDER)?;
            *last = None;
        }
        (None, _) => {}
    }
    let flags = if of_files { OF_FILES } else { 0 };
    fs::make(txn, SERVED, &Node { folder: true, flags, parent: at, name: name.to_owned(), ..Node::default() }, HOLDER)
}

/// The folder at a path, made with those above it where there is none.
fn folder(txn: &mut Txn<'_>, path: &str, of_files: bool, last: &mut Last) -> Result<u64> {
    if let Some((_, number)) = last.as_ref().filter(|(at, _)| at == path) {
        return Ok(*number);
    }
    let mut at = ROOT;
    for name in path.split('/').filter(|name| !name.is_empty()) {
        at = folder_in(txn, at, name, of_files, last)?;
    }
    *last = Some((path.to_owned(), at));
    Ok(at)
}

fn is_empty(pages: &impl Pages, folder: u64) -> Result<bool> {
    let mut empty = true;
    fs::list(pages, SERVED, folder, None, &mut |_, _| {
        empty = false;
        false
    })?;
    Ok(empty)
}

/// A folder a node's files made, left with nothing in it, is no entry of its own: it goes, and so the one above it.
fn prune(txn: &mut Txn<'_>, mut at: u64) -> Result<()> {
    while at != ROOT {
        let Some(node) = fs::node(txn, SERVED, at)? else { return Ok(()) };
        if !node.folder || node.flags & OF_FILES == 0 || node.flags & NODE != 0 || !is_empty(txn, at)? {
            return Ok(());
        }
        fs::remove(txn, SERVED, at, HOLDER)?;
        at = node.parent;
    }
    Ok(())
}

/// A path is listed no more: a file goes; a folder goes where nothing is in it, and is otherwise what the files
/// still in it made - each of which the listing says for itself, shown or gone.
fn gone(txn: &mut Txn<'_>, path: &str, last: &mut Last) -> Result<()> {
    let (folder_path, name) = above(path);
    let parent = match last.as_ref().filter(|(at, _)| at == folder_path) {
        Some((_, number)) => Some(*number),
        None => fs::at(txn, SERVED, folder_path)?,
    };
    let Some(parent) = parent else { return Ok(()) };
    let Some(number) = fs::lookup(txn, SERVED, parent, name)? else { return Ok(()) };
    let Some(node) = fs::node(txn, SERVED, number)? else { return Ok(()) };
    // Never the project's: made through the folder, and whoever serves it has it there.
    if node.flags & NEW != 0 {
        return Ok(());
    }
    if node.folder && !is_empty(txn, number)? {
        if node.flags & !GONE != OF_FILES {
            fs::set(txn, SERVED, number, &Node { flags: OF_FILES | (node.flags & GONE), ..node }, HOLDER)?;
        }
        return Ok(());
    }
    fs::remove(txn, SERVED, number, HOLDER)?;
    // The folder it was in stays the one to look in next - unless it went with it.
    *last = Some((folder_path.to_owned(), parent));
    if fs::node(txn, SERVED, parent)?.is_some_and(|folder| folder.flags & OF_FILES != 0) {
        *last = None;
        prune(txn, parent)?;
    }
    Ok(())
}

/// A path is listed, anew or otherwise than it was.
fn shown(txn: &mut Txn<'_>, path: &str, shown: &Shown, now: u64, last: &mut Last) -> Result<()> {
    match shown {
        Shown::Folder | Shown::Node => {
            let (folder_path, name) = above(path);
            let parent = folder(txn, folder_path, false, last)?;
            let number = folder_in(txn, parent, name, false, last)?;
            let flags = if matches!(shown, Shown::Node) { NODE } else { 0 };
            // The project's from here on, whoever made it; removed through the folder, it stays so.
            if let Some(node) = fs::node(txn, SERVED, number)?.filter(|node| node.flags & !GONE != flags && number != ROOT) {
                fs::set(txn, SERVED, number, &Node { flags: flags | (node.flags & GONE), ..node }, HOLDER)?;
            }
        }
        Shown::File { disk, size, modified, of_files, source } => {
            let (folder_path, name) = above(path);
            let parent = folder(txn, folder_path, *of_files, last)?;
            let bytes = Bytes::Derived(place(disk, source));
            let flags = if *of_files { OF_FILES } else { 0 };
            let mut file = Node { flags, len: *size, modified: nanos(*modified), parent, name: name.to_owned(), bytes, ..Node::default() };
            match fs::lookup(txn, SERVED, parent, name)? {
                Some(number) => match fs::node(txn, SERVED, number)? {
                    Some(was) if !was.folder => {
                        // A second on at least, where it changed and its time did not go on: what polls compares
                        // whole seconds.
                        if file.modified <= was.modified {
                            let other = was.bytes != file.bytes || was.len != file.len;
                            file.modified = if other { now.max(was.modified + 1_000_000_000) } else { was.modified };
                        }
                        // Said only where something of it differs. Removed through the folder, it stays so.
                        file.version = was.version;
                        file.flags |= was.flags & GONE;
                        if file != was {
                            file.version += 1;
                            fs::set(txn, SERVED, number, &file, HOLDER)?;
                        }
                    }
                    // A folder held the place: it gives way, with what was in it.
                    _ => {
                        fs::remove(txn, SERVED, number, HOLDER)?;
                        fs::make(txn, SERVED, &file, HOLDER)?;
                        *last = None;
                    }
                },
                None => drop(fs::make(txn, SERVED, &file, HOLDER)?),
            }
        }
    }
    Ok(())
}

/// Say what changed of what is served: the paths gone, then the paths shown anew or otherwise - the entries first,
/// then the nodes' files, so that a folder an entry is is not taken for one a node's files made.
///
/// # Errors
/// The shared space cannot take it, or is damaged.
pub fn show(txn: &mut Txn<'_>, delta: &Delta, now: u64) -> Result<()> {
    // Said of the project as it is; where something was moved through the folder and the project is yet to take
    // the move, of where each path is by now.
    let moves = moves(txn)?;
    if moves.is_empty() {
        return show_there(txn, delta, now);
    }
    let there = Delta {
        shown: delta.shown.iter().map(|(path, it)| (through(&moves, path), it.clone())).collect(),
        gone: delta.gone.iter().map(|path| through(&moves, path)).collect(),
    };
    show_there(txn, &there, now)
}

/// As `show`, of paths as they are in what is served.
fn show_there(txn: &mut Txn<'_>, delta: &Delta, now: u64) -> Result<()> {
    let mut last = None;
    for path in &delta.gone {
        gone(txn, path, &mut last)?;
    }
    let of_files = |shown: &Shown| matches!(shown, Shown::File { of_files: true, .. });
    for (path, it) in delta.shown.iter().filter(|(_, it)| !of_files(it)) {
        shown(txn, path, it, now, &mut last)?;
    }
    for (path, it) in delta.shown.iter().filter(|(_, it)| of_files(it)) {
        shown(txn, path, it, now, &mut last)?;
    }
    Ok(())
}

/// Say all that is served: what the shared filesystem holds is brought to it, by what differs - everything, where
/// it holds nothing yet.
///
/// # Errors
/// As `show`.
pub fn show_whole(txn: &mut Txn<'_>, listing: &Listing, now: u64) -> Result<()> {
    let held = listed(txn)?;
    let moves = moves(txn)?;
    if moves.is_empty() {
        return show_there(txn, &Delta::between(&held, listing), now);
    }
    let there: Listing = listing.iter().map(|(path, it)| (through(&moves, path), it.clone())).collect();
    show_there(txn, &Delta::between(&held, &there), now)
}

/// What a node of the shared filesystem is to a listing: an entry, or - a folder a node's files made - none.
fn entry(node: &Node) -> Option<Shown> {
    // Made through the folder, and not the project's.
    if node.flags & NEW != 0 {
        return None;
    }
    if node.folder {
        return match node.flags {
            flags if flags & NODE != 0 => Some(Shown::Node),
            flags if flags & OF_FILES != 0 => None,
            _ => Some(Shown::Folder),
        };
    }
    let Bytes::Derived(said) = &node.bytes else { return None };
    let (disk, source) = placed(said)?;
    let modified = (node.modified / 1_000_000_000, u32::try_from(node.modified % 1_000_000_000).unwrap_or(0));
    Some(Shown::File { disk, size: node.len, modified, of_files: node.flags & OF_FILES != 0, source })
}

/// What the shared filesystem says is served, as a listing: each folder that is an entry, each node, each file
/// with where its content lives - its time as the filesystem has it, which never goes back.
///
/// # Errors
/// The shared space is damaged.
pub fn listed(pages: &impl Pages) -> Result<Listing> {
    let mut listing = Listing::new();
    under(pages, ROOT, "", &mut |path, shown| drop(listing.insert(path, shown)))?;
    Ok(listing)
}

/// Each entry under a folder of the shared filesystem, by its path.
fn under(pages: &impl Pages, folder: u64, path: &str, each: &mut dyn FnMut(String, Shown)) -> Result<()> {
    let mut folders = vec![(folder, path.to_owned())];
    while let Some((number, path)) = folders.pop() {
        let mut inside = Vec::new();
        fs::list(pages, SERVED, number, None, &mut |name, number| {
            inside.push((name.to_owned(), number));
            true
        })?;
        for (name, number) in inside {
            let Some(node) = fs::node(pages, SERVED, number)? else { continue };
            let path = if path.is_empty() { name } else { format!("{path}/{name}") };
            if let Some(shown) = entry(&node) {
                each(path.clone(), shown);
            }
            if node.folder {
                folders.push((number, path));
            }
        }
    }
    Ok(())
}

/// What differs of what is served from a listing of it read at an earlier commit, by what the commits after that
/// one did (`fs::changes`): what it costs follows what changed. None where those changes are no longer kept - who
/// asks lists it whole (`listed`).
///
/// # Errors
/// The shared space is damaged.
pub fn changed(pages: &impl Pages, after: u64, listing: &Listing) -> Result<Option<Delta>> {
    let mut since = Vec::new();
    if !fs::changes(pages, SERVED, after, &mut |_, change| {
        since.push(change);
        true
    })? {
        return Ok(None);
    }
    let mut delta = Delta::default();
    let join = |folder: &str, name: &str| if folder.is_empty() { name.to_owned() } else { format!("{folder}/{name}") };
    // A path is listed no more, with what the listing - and what was shown since - has under it.
    let gone = |delta: &mut Delta, path: &str| {
        let inside = format!("{path}/");
        let listed = listing.range(inside.clone()..).take_while(|(under, _)| under.starts_with(&inside));
        delta.gone.extend(listed.map(|(under, _)| under.clone()));
        delta.gone.insert(path.to_owned());
        delta.shown.retain(|shown, _| shown != path && !shown.starts_with(&inside));
    };
    for change in since {
        let now = match change.what {
            fs::What::Gone => None,
            fs::What::Made | fs::What::Changed | fs::What::Moved { .. } => fs::node(pages, SERVED, change.number)?,
        };
        if let fs::What::Moved { from, was } = &change.what
            && let Some(folder) = fs::path(pages, SERVED, *from)?
        {
            gone(&mut delta, &join(&folder, was));
        }
        // Where it is now - or, gone, was: what is gone with the folder it was in is said by that folder's going.
        let Some(folder) = fs::path(pages, SERVED, now.as_ref().map_or(change.parent, |node| node.parent))? else { continue };
        let Some(node) = now else {
            gone(&mut delta, &join(&folder, &change.name));
            continue;
        };
        let path = join(&folder, &node.name);
        match entry(&node) {
            Some(shown) => {
                delta.gone.remove(&path);
                if matches!(change.what, fs::What::Moved { .. }) && node.folder {
                    under(pages, change.number, &path, &mut |path, shown| {
                        delta.gone.remove(&path);
                        delta.shown.insert(path, shown);
                    })?;
                }
                delta.shown.insert(path, shown);
            }
            // An entry no more, but there for the files still in it: those are shown as they are, after it went.
            None if listing.contains_key(&path) || delta.shown.contains_key(&path) => {
                gone(&mut delta, &path);
                under(pages, change.number, &path, &mut |path, shown| {
                    delta.gone.remove(&path);
                    delta.shown.insert(path, shown);
                })?;
            }
            None => {}
        }
    }
    // What is as the listing has it already is no change - but where it, or a folder above it, went first.
    let went = |path: &str| {
        let mut at = Some(path);
        std::iter::from_fn(|| {
            let now = at?;
            at = now.rsplit_once('/').map(|(above, _)| above);
            Some(now)
        })
        .any(|path| delta.gone.contains(path))
    };
    delta.shown.retain(|path, shown| went(path) || listing.get(path) != Some(&*shown));
    Ok(Some(delta))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sfs::Space;

    fn file(size: u64, at: u64, of_files: bool, source: Source) -> Shown {
        Shown::File { disk: PathBuf::from("/layers/game/x"), size, modified: (at, 0), of_files, source }
    }

    /// A project of two folders, a file in each.
    fn small() -> Listing {
        Listing::from([
            ("rooms".to_owned(), Shown::Folder),
            ("rooms/a.txt".to_owned(), file(1, 10, false, Source::Disk)),
            ("props".to_owned(), Shown::Folder),
            ("props/b.txt".to_owned(), file(2, 10, false, Source::Disk)),
        ])
    }

    fn flags_at(space: &Space, path: &str) -> Option<u32> {
        let pages = space.read().unwrap();
        let number = fs::at(&pages, SERVED, path).unwrap()?;
        fs::node(&pages, SERVED, number).unwrap().map(|node| node.flags)
    }

    #[test]
    fn what_is_made_through_the_folder_is_not_what_the_project_serves_until_the_project_has_it() {
        let space = Space::private().unwrap();
        let before = small();
        say_whole(&space, &before).unwrap();
        // Whoever serves the folder makes a folder in it, and one in that, a program having asked.
        let mut txn = space.write().unwrap();
        let rooms = fs::at(&txn, SERVED, "rooms").unwrap().unwrap();
        let folder = |parent: u64, name: &str| Node { folder: true, flags: NEW, parent, name: name.into(), ..Node::default() };
        let made = fs::make(&mut txn, SERVED, &folder(rooms, "made"), SERVER).unwrap();
        fs::make(&mut txn, SERVED, &folder(made, "inside"), SERVER).unwrap();
        let seen = txn.generation();
        txn.commit().unwrap();
        assert_eq!(listed(&space.read().unwrap()).unwrap(), before, "no part of what the project serves");
        assert_eq!(changed(&space.read().unwrap(), seen - 1, &before).unwrap(), Some(Delta::default()), "nor a change of it");
        // Said whole again, or a path beside it said gone, it stays; said gone itself - which the project never had
        // - it stays too.
        say_whole(&space, &before).unwrap();
        say(&space, &Delta { gone: ["rooms/made".to_owned()].into(), ..Delta::default() }).unwrap();
        assert_eq!(flags_at(&space, "rooms/made/inside"), Some(NEW));
        // The project has the first: listed from then on, as the node it was made as; the one in it not yet.
        let mut now = before.clone();
        now.insert("rooms/made".to_owned(), Shown::Folder);
        say(&space, &Delta::between(&before, &now)).unwrap();
        assert_eq!(listed(&space.read().unwrap()).unwrap(), now);
        assert_eq!(fs::at(&space.read().unwrap(), SERVED, "rooms/made").unwrap(), Some(made), "under the number it had");
        assert_eq!(flags_at(&space, "rooms/made/inside"), Some(NEW));
    }

    #[test]
    fn what_was_removed_through_the_folder_stays_so_whatever_the_project_says_of_it_meanwhile() {
        let space = Space::private().unwrap();
        let before = small();
        say_whole(&space, &before).unwrap();
        let mut txn = space.write().unwrap();
        let number = fs::at(&txn, SERVED, "rooms/a.txt").unwrap().unwrap();
        let node = fs::node(&txn, SERVED, number).unwrap().unwrap();
        fs::set(&mut txn, SERVED, number, &Node { flags: node.flags | GONE, ..node }, SERVER).unwrap();
        txn.commit().unwrap();
        assert_eq!(listed(&space.read().unwrap()).unwrap(), before, "the project has it still");
        // The project changes it: removed through the folder all the same.
        let mut now = before.clone();
        now.insert("rooms/a.txt".to_owned(), file(5, 20, false, Source::Disk));
        say(&space, &Delta::between(&before, &now)).unwrap();
        assert_eq!(flags_at(&space, "rooms/a.txt"), Some(GONE));
        assert_eq!(listed(&space.read().unwrap()).unwrap(), now);
        // The project takes the removal: gone.
        say(&space, &Delta { gone: ["rooms/a.txt".to_owned()].into(), ..Delta::default() }).unwrap();
        assert_eq!(flags_at(&space, "rooms/a.txt"), None);
    }

    #[test]
    fn what_the_project_says_of_a_path_moved_through_the_folder_is_said_where_it_is_now() {
        let space = Space::private().unwrap();
        let before = small();
        say_whole(&space, &before).unwrap();
        // A program renames a folder; whoever serves it moves it at once, and says the project is yet to.
        let mut txn = space.write().unwrap();
        let rooms = fs::at(&txn, SERVED, "rooms").unwrap().unwrap();
        fs::rename(&mut txn, SERVED, rooms, fs::ROOT, "halls", SERVER).unwrap();
        moved(&mut txn, 7, "rooms", "halls").unwrap();
        txn.commit().unwrap();
        let there = |space: &Space| listed(&space.read().unwrap()).unwrap().into_keys().collect::<Vec<_>>();
        assert_eq!(there(&space), ["halls", "halls/a.txt", "props", "props/b.txt"]);
        // The project, which has it where it was, says its file changed and another came beside it: said where
        // they are now. Said whole, nothing comes back where it was.
        let mut now = before.clone();
        now.insert("rooms/a.txt".to_owned(), file(5, 20, false, Source::Disk));
        now.insert("rooms/c.txt".to_owned(), file(3, 20, false, Source::Disk));
        say(&space, &Delta::between(&before, &now)).unwrap();
        say_whole(&space, &now).unwrap();
        assert_eq!(there(&space), ["halls", "halls/a.txt", "halls/c.txt", "props", "props/b.txt"]);
        let pages = space.read().unwrap();
        let a = fs::at(&pages, SERVED, "halls/a.txt").unwrap().and_then(|number| fs::node(&pages, SERVED, number).unwrap());
        assert_eq!(a.map(|node| node.len), Some(5));
        drop(pages);
        // The project takes the move, and says so in the commit that says what it serves after it: where it was
        // gone, where it is shown - and nothing of what is there taken away for it.
        let moved_listing: Listing = now.iter().map(|(path, it)| (path.replacen("rooms", "halls", 1), it.clone())).collect();
        say_with(&space, &Delta::between(&now, &moved_listing), &mut |txn| unmoved(txn, 7).map(drop)).unwrap();
        assert_eq!(there(&space), ["halls", "halls/a.txt", "halls/c.txt", "props", "props/b.txt"]);
        assert_eq!(listed(&space.read().unwrap()).unwrap(), moved_listing);
        assert_eq!(fs::at(&space.read().unwrap(), SERVED, "halls").unwrap(), Some(rooms), "the folder it was");
        // From then on its paths are the project's own again.
        let mut last = moved_listing.clone();
        last.insert("rooms".to_owned(), Shown::Folder);
        say(&space, &Delta::between(&moved_listing, &last)).unwrap();
        assert_eq!(there(&space), ["halls", "halls/a.txt", "halls/c.txt", "props", "props/b.txt", "rooms"]);
    }

    fn listing() -> Listing {
        let record = Source::RecordText { record: PathBuf::from("/layers/top/.records/ab/uid.toml"), name: "node.toml".into() };
        let stored = Source::Stored { sha1: "da39a3ee".into(), store: PathBuf::from("/layers/.store") };
        Listing::from([
            ("rooms".to_owned(), Shown::Folder),
            ("rooms/patio".to_owned(), Shown::Node),
            ("rooms/patio/node.toml".to_owned(), file(12, 100, true, record)),
            ("rooms/patio/glyphs/a.glif".to_owned(), file(30, 100, true, Source::Disk)),
            ("rooms/patio/glyphs/b.glif".to_owned(), file(31, 100, true, stored)),
            ("rooms/patio/props".to_owned(), Shown::Folder),
            ("rooms/patio/props/lamp".to_owned(), Shown::Node),
            ("readme.txt".to_owned(), file(5, 100, false, Source::Disk)),
        ])
    }

    /// Say what differs between two listings - and hold who follows by change to it: a listing read before,
    /// brought on by what the commits since did, is what is listed whole.
    fn told(space: &Space, before: &Listing, now: &Listing, at: u64) {
        let (seen, mut follows) = {
            let read = space.read().unwrap();
            (read.commit().generation, listed(&read).unwrap())
        };
        let mut txn = space.write().unwrap();
        show(&mut txn, &Delta::between(before, now), at).unwrap();
        txn.commit().unwrap();
        let read = space.read().unwrap();
        let delta = changed(&read, seen, &follows).unwrap().expect("the changes are kept");
        // As the server's tree takes it: what is gone goes with what is under it, then what is shown is put.
        for path in &delta.gone {
            let inside = format!("{path}/");
            follows.retain(|under, _| under != path && !under.starts_with(&inside));
        }
        follows.extend(delta.shown.clone());
        assert_eq!(follows, listed(&read).unwrap(), "followed by change: {delta:?}");
    }

    #[test]
    fn what_is_said_whole_is_listed_as_it_was_said() {
        let space = Space::private().unwrap();
        let mut txn = space.write().unwrap();
        show_whole(&mut txn, &listing(), 0).unwrap();
        txn.commit().unwrap();
        let read = space.read().unwrap();
        assert_eq!(listed(&read).unwrap(), listing());
        // The folder a node's files made is there, and no entry.
        let glyphs = fs::at(&read, SERVED, "rooms/patio/glyphs").unwrap().unwrap();
        assert_eq!(fs::node(&read, SERVED, glyphs).unwrap().unwrap().flags, OF_FILES);
        drop(read);
        // Said again where all of it is held: nothing changes.
        let before = space.usage().unwrap().generation;
        let mut txn = space.write().unwrap();
        show_whole(&mut txn, &listing(), 0).unwrap();
        txn.commit().unwrap();
        assert_eq!(space.usage().unwrap().generation, before);
        // Said whole where something else is held: brought to it.
        let mut other = listing();
        other.remove("readme.txt");
        other.insert("rooms/yard".into(), Shown::Node);
        let mut txn = space.write().unwrap();
        show_whole(&mut txn, &other, 0).unwrap();
        txn.commit().unwrap();
        assert_eq!(listed(&space.read().unwrap()).unwrap(), other);
    }

    #[test]
    fn a_folder_a_nodes_files_made_goes_with_the_last_of_them_and_is_an_entry_once_one_is_there() {
        let space = Space::private().unwrap();
        let mut now = listing();
        told(&space, &Listing::new(), &now, 0);
        // An entry of the folder's name comes: an entry now, with the files still in it.
        let mut next = now.clone();
        next.insert("rooms/patio/glyphs".into(), Shown::Folder);
        told(&space, &now, &next, 0);
        now = next;
        assert_eq!(listed(&space.read().unwrap()).unwrap(), now);
        // And goes: the files' own again.
        let mut next = now.clone();
        next.remove("rooms/patio/glyphs");
        told(&space, &now, &next, 0);
        now = next;
        assert_eq!(listed(&space.read().unwrap()).unwrap(), now);
        // The files go, one and then the other: the folder with the last.
        for name in ["rooms/patio/glyphs/a.glif", "rooms/patio/glyphs/b.glif"] {
            assert!(fs::at(&space.read().unwrap(), SERVED, "rooms/patio/glyphs").unwrap().is_some());
            let mut next = now.clone();
            next.remove(name);
            told(&space, &now, &next, 0);
            now = next;
        }
        let read = space.read().unwrap();
        assert_eq!(fs::at(&read, SERVED, "rooms/patio/glyphs").unwrap(), None);
        assert_eq!(listed(&read).unwrap(), now);
        drop(read);
        // A node gone with all in it; a file where a folder was, and a folder where a file was.
        let mut next = now.clone();
        next.retain(|path, _| !path.starts_with("rooms/patio"));
        next.insert("rooms/patio".into(), file(1, 100, false, Source::Disk));
        next.remove("readme.txt");
        next.insert("readme.txt".into(), Shown::Folder);
        next.insert("readme.txt/inside".into(), file(2, 100, false, Source::Disk));
        told(&space, &now, &next, 0);
        assert_eq!(listed(&space.read().unwrap()).unwrap(), next);
    }

    #[test]
    #[ignore = "a measurement, of the project PE2_BENCH_PROJECT names"]
    fn bench_saying_what_is_served() {
        let Ok(root) = std::env::var("PE2_BENCH_PROJECT") else { return };
        let index = crate::index::Index::without_store(&crate::Project::load(Path::new(&root)).unwrap()).unwrap();
        let listing = index.served();
        let dir = std::env::temp_dir().join(format!("pe2-shared-bench-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (name, space) in [("memory", Space::private().unwrap()), ("files", Space::open(&dir).unwrap())] {
            let whole = std::time::Instant::now();
            say_whole(&space, &listing).unwrap();
            let whole = whole.elapsed();
            let again = std::time::Instant::now();
            say_whole(&space, &listing).unwrap();
            let again = again.elapsed();
            let read = std::time::Instant::now();
            let said = listed(&space.read().unwrap()).unwrap();
            let read = read.elapsed();
            assert_eq!(said.len(), listing.len());
            // One file of a node changed, then a file added and taken away again, a hundred times each.
            let (path, shown) = listing.iter().find(|(_, shown)| matches!(shown, Shown::File { .. })).unwrap();
            let Shown::File { disk, size, modified, of_files, source } = shown.clone() else { unreachable!() };
            let changed = std::time::Instant::now();
            for n in 1..=100 {
                let other = Shown::File { disk: disk.clone(), size: size + n, modified, of_files, source: source.clone() };
                say(&space, &Delta { shown: [(path.clone(), other)].into(), gone: [].into() }).unwrap();
            }
            let changed = changed.elapsed();
            let added = std::time::Instant::now();
            for n in 0..100 {
                let new = format!("zz_bench/file-{n}.txt");
                say(&space, &Delta { shown: [(new.clone(), shown.clone())].into(), gone: [].into() }).unwrap();
                say(&space, &Delta { shown: [].into(), gone: [new].into() }).unwrap();
            }
            let added = added.elapsed();
            let usage = space.usage().unwrap();
            eprintln!(
                "{name}: {} paths said whole {:.1} ms, said again with nothing differing {:.1} ms, listed back {:.1} ms; a file changed \
                 {:.1} us; a file added or taken away {:.1} us; {} pages of 4 KiB, {} free",
                listing.len(),
                whole.as_secs_f64() * 1e3,
                again.as_secs_f64() * 1e3,
                read.as_secs_f64() * 1e3,
                changed.as_secs_f64() * 1e6 / 100.0,
                added.as_secs_f64() * 1e6 / 200.0,
                usage.pages,
                usage.free
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_files_time_never_goes_back_and_its_version_goes_up_with_it() {
        let space = Space::private().unwrap();
        let mut now = Listing::from([("a.txt".to_owned(), file(5, 100, false, Source::Disk))]);
        told(&space, &Listing::new(), &now, 0);
        let node = |space: &Space| {
            let read = space.read().unwrap();
            let number = fs::at(&read, SERVED, "a.txt").unwrap().unwrap();
            (number, fs::node(&read, SERVED, number).unwrap().unwrap())
        };
        let (number, first) = node(&space);
        assert_eq!((first.modified, first.version, first.len), (100_000_000_000, 0, 5));
        // Changed, its time said the same: a second on at least - the time it is said at, where that is later.
        let mut next = now.clone();
        next.insert("a.txt".into(), file(6, 100, false, Source::Disk));
        told(&space, &now, &next, 100_500_000_000);
        now = next;
        let (same, second) = node(&space);
        assert_eq!((same, second.modified, second.version, second.len), (number, 101_000_000_000, 1, 6));
        // Changed, its time said earlier: the time it is said at.
        let mut next = now.clone();
        next.insert("a.txt".into(), file(7, 50, false, Source::Disk));
        told(&space, &now, &next, 300_000_000_000);
        assert_eq!(node(&space).1.modified, 300_000_000_000);
        // What each change was is said for who reads them after: made, then changed twice, by the holder.
        let mut said = Vec::new();
        assert!(fs::changes(&space.read().unwrap(), SERVED, 0, &mut |_, change| {
            said.push((change.what, change.number, change.by));
            true
        })
        .unwrap());
        assert_eq!(said, [(fs::What::Made, number, HOLDER), (fs::What::Changed, number, HOLDER), (fs::What::Changed, number, HOLDER)]);
    }
}
