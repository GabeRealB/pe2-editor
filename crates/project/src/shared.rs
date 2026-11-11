//! What is served of a project, kept in the filesystem its processes share (`sfs::fs::SERVED`): whoever holds the
//! project says there what it serves - all of it where the shared space holds none of it (`show_whole`), and after
//! an edit what the edit changed (`show`, of `Index::serve_changed`) - for whoever serves the folder to read with
//! no word from the holder. Nothing reads it there yet: the server is still told by the holder what it serves
//! (`server::ToServer`), and this is said beside that.
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

/// The shared space's folder, in the cache's folder of what serving keeps.
pub const SPACE: &str = "space";

/// The space the processes of the project in `root` share, attached here.
///
/// # Errors
/// As `sfs::space::shared`.
pub fn space(root: &Path) -> std::io::Result<std::sync::Arc<sfs::Space>> {
    sfs::space::shared(&root.join(crate::cache::DIR).join(crate::cache::STAGING).join(SPACE))
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
    let mut txn = space.write()?;
    show(&mut txn, delta, now())?;
    txn.commit()
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
    match fs::lookup(txn, SERVED, at, name)? {
        Some(there) if fs::node(txn, SERVED, there)?.is_some_and(|node| node.folder) => return Ok(there),
        Some(file) => {
            fs::remove(txn, SERVED, file, HOLDER)?;
            *last = None;
        }
        None => {}
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
    if node.folder && !is_empty(txn, number)? {
        if node.flags != OF_FILES {
            fs::set(txn, SERVED, number, &Node { flags: OF_FILES, ..node }, HOLDER)?;
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
            if let Some(node) = fs::node(txn, SERVED, number)?.filter(|node| node.flags != flags && number != ROOT) {
                fs::set(txn, SERVED, number, &Node { flags, ..node }, HOLDER)?;
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
                        // Said only where something of it differs.
                        file.version = was.version;
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
    show(txn, &Delta::between(&held, listing), now)
}

/// What the shared filesystem says is served, as a listing: each folder that is an entry, each node, each file
/// with where its content lives - its time as the filesystem has it, which never goes back.
///
/// # Errors
/// The shared space is damaged.
pub fn listed(pages: &impl Pages) -> Result<Listing> {
    let mut listing = Listing::new();
    let mut folders = vec![(ROOT, String::new())];
    while let Some((number, path)) = folders.pop() {
        let mut inside = Vec::new();
        fs::list(pages, SERVED, number, None, &mut |name, number| {
            inside.push((name.to_owned(), number));
            true
        })?;
        for (name, number) in inside {
            let Some(node) = fs::node(pages, SERVED, number)? else { continue };
            let path = if path.is_empty() { name } else { format!("{path}/{name}") };
            if node.folder {
                if node.flags & NODE != 0 {
                    listing.insert(path.clone(), Shown::Node);
                } else if node.flags & OF_FILES == 0 {
                    listing.insert(path.clone(), Shown::Folder);
                }
                folders.push((number, path));
            } else if let Bytes::Derived(said) = &node.bytes
                && let Some((disk, source)) = placed(said)
            {
                let modified = (node.modified / 1_000_000_000, u32::try_from(node.modified % 1_000_000_000).unwrap_or(0));
                listing.insert(path, Shown::File { disk, size: node.len, modified, of_files: node.flags & OF_FILES != 0, source });
            }
        }
    }
    Ok(listing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sfs::Space;

    fn file(size: u64, at: u64, of_files: bool, source: Source) -> Shown {
        Shown::File { disk: PathBuf::from("/layers/game/x"), size, modified: (at, 0), of_files, source }
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

    fn told(space: &Space, before: &Listing, now: &Listing, at: u64) {
        let mut txn = space.write().unwrap();
        show(&mut txn, &Delta::between(before, now), at).unwrap();
        txn.commit().unwrap();
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
