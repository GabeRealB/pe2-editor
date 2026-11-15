//! The tree served on Windows through the Cloud Files API - what `OneDrive` serves its folders by - which every Windows
//! has, with nothing to install and no administrator. The folder (`assets`) is registered as a sync root: what is in a
//! folder is made placeholders when it is first opened, and a file's bytes are given as they are read (`fetch_data`),
//! then kept on disk as a real file - so every program, Explorer and all, sees plain NTFS files, and indexers, thumbnails
//! and antivirus leave unread ones be.
//!
//! What programs change there is applied to the project as the FUSE mount's is (`write::Owner`): a file written or
//! made, a folder made - seen by the sync root's watcher, applied once it has settled (`SETTLE`), then marked in sync -
//! and a removal or a move, each told before it is made. As the project changes, the placeholders made follow (`sync`):
//! one whose file changed is emptied, to be read again; one gone is removed; one changed here and not applied yet is
//! left alone.

use pe2_path::{Name, PathError, RelPath, Why};
use project::TreePath;
use sfs::{HostPath, NotWithin};
use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use cloud_filter::error::{CResult, CloudErrorKind};
use cloud_filter::filter::{Request, StateWatchEvent, SyncFilter, info, ticket};
use cloud_filter::metadata::Metadata;
use cloud_filter::placeholder::{ConvertOptions, Placeholder, PlaceholderInfo, UpdateOptions};
use cloud_filter::placeholder_create_info::PlaceholderCreateInfo;
use cloud_filter::root::{
    Connection, HydrationType, PopulationType, SecurityId, Session, SyncRootId, SyncRootIdBuilder, SyncRootInfo,
};
use cloud_filter::utility::WriteAt;
use nt_time::FileTime;

/// The errors the Cloud Files API gives.
mod windows_error {
    pub type Error = windows_core::Error;
}

use crate::write::{Change, Owner, is_junk};
use crate::{Node, Served, Tree, What};

/// How long a change waits for what follows it before it is applied.
const SETTLE: Duration = Duration::from_millis(1000);

/// How much of a file is given at a time: aligned, but for the last piece, which may end the file.
const PIECE: u64 = 1 << 20;

/// The provider every project's sync root is registered by.
const PROVIDER: &str = "pe2";

/// A project served through the Cloud Files API: disconnected, and its watcher stopped, when dropped. The folder stays
/// registered, its placeholders and what was read kept, until `forget`.
pub struct Cloud {
    _connection: Connection<Filter>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Cloud {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Waited for: it gives the project what still waits to settle, while the folder is still connected.
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
    }
}

/// A sync root's id: this provider's, the user's, and the folder's - its name as served, for a person to know it by,
/// and what tells its place from every other: two projects in folders of one name (`project`, mostly) are two sync
/// roots, and letting go of one's leaves the other's.
fn id_of(name: &str, at: &HostPath) -> io::Result<SyncRootId> {
    let place = project::extract::sha1_hex(at.clone().to_string_lossy().to_lowercase().as_bytes());
    let named = name.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' });
    let account: String = named.chain("_".chars()).chain(place.chars().take(12)).collect();
    let user = SecurityId::current_user().map_err(|e| io::Error::other(format!("the user: {e}")))?;
    Ok(SyncRootIdBuilder::new(PROVIDER).user_security_id(user).account_name(account).build())
}

/// Serve a tree at a folder (an NTFS one), registering it where it is not yet; what programs change applied by `owner`,
/// or refused where there is none.
///
/// # Errors
/// The folder cannot be made or registered, or the filter cannot connect.
pub fn serve(
    served: &Served,
    owner: Option<Arc<dyn Owner>>,
    at: &HostPath,
    staging: &HostPath,
    name: &str,
) -> io::Result<Cloud> {
    // Windows names the folder plainly - `D:\...` - in what it asks and registers; not as canonicalized (`\\?\D:\...`).
    let at = &at.clone();
    std::fs::create_dir_all(at)?;
    std::fs::create_dir_all(staging)?;
    let step = |what: &'static str| move |e: windows_error::Error| io::Error::other(format!("{what}: {e}"));
    let id = id_of(name, at)?;
    if !id.is_registered().map_err(step("asking whether it is registered"))? {
        let info = SyncRootInfo::default()
            .with_display_name(format!("Parasite Eve II project {name}"))
            // Shown beside it in Explorer: a folder, Windows' own, until the editor has one.
            .with_icon(r"%SystemRoot%\system32\imageres.dll,-3")
            .with_version("1")
            .with_hydration_type(HydrationType::Partial)
            .with_population_type(PopulationType::Full)
            .with_path(at)
            .map_err(|e| io::Error::other(format!("its path: {e}")))?;
        id.register(info).map_err(step("registering it"))?;
    }
    let pending = Arc::new(Mutex::new(Pending::default()));
    let filter = Filter { served: served.clone(), owner: owner.clone(), root: at.clone(), pending: pending.clone() };
    let connection = Session::new().connect(at, filter).map_err(step("connecting to it"))?;
    let stopped = Arc::new(AtomicBool::new(false));
    let worker = Worker { served: served.clone(), owner, root: at.clone(), staging: staging.clone(), pending };
    let stopping = stopped.clone();
    let worker = std::thread::Builder::new().name("cloud".into()).spawn(move || worker.run(&stopping))?;
    // Woken when the tree changes: the folder is brought in step at once, not when the worker next looks.
    let waking = worker.thread().clone();
    served.on_change(move || waking.unpark());
    Ok(Cloud { _connection: connection, stop: stopped, worker: Some(worker) })
}

/// Make the placeholders of what is at a path of the tree, all under it and the folders leading to it, where Windows
/// has not: what a program opens by its path finds it, before anything lists the folders it is in - which is when
/// Windows asks for them (`fetch_placeholders`).
pub fn place(served: &Served, root: &HostPath, at: &RelPath, read_only: bool) {
    let tree = served.tree();
    let mut dir = root.clone();
    let mut walked = RelPath::root().to_owned();
    for step in at.parts() {
        walked = walked.join(step);
        let Some((_, node)) = tree.at(&walked) else { return };
        // Named as the tree has it written.
        let name = node.path.last().unwrap_or(step);
        let next = dir.join(name);
        if std::fs::symlink_metadata(&next).is_err() {
            let _ = placeholder(name.as_str(), &node, read_only).create(&dir);
        }
        dir = next;
    }
    fill(&tree, &dir, &walked, read_only);
}

/// The placeholders of all under a folder of the tree, where they are not.
fn fill(tree: &Tree, dir: &HostPath, at: &RelPath, read_only: bool) {
    let Some(children) = tree.entries(at) else { return };
    for (name, _, node) in &children {
        let path = dir.join(&**name);
        if std::fs::symlink_metadata(&path).is_err() {
            let _ = placeholder(name.as_str(), node, read_only).create(dir);
        }
        if matches!(node.what, What::Folder(_)) {
            fill(tree, &path, &at.join(name), read_only);
        }
    }
}

/// Unregister a folder: it is no longer a sync root, and what was not read of it cannot be. One that is not
/// registered is none to forget - asked of Windows as it is unregistered, which whether one is registered
/// (`is_registered`) does not always say.
///
/// # Errors
/// It cannot be unregistered.
pub fn forget(at: &HostPath, name: &str) -> io::Result<()> {
    const NOT_FOUND: i32 = 0x8007_0490_u32.cast_signed();
    match id_of(name, at)?.unregister() {
        Err(e) if e.code().0 != NOT_FOUND => Err(e.into()),
        _ => Ok(()),
    }
}

/// What waits to be applied: files seen changed, and removals, each by when it was last touched.
#[derive(Default)]
struct Pending {
    changed: HashMap<HostPath, Instant>,
    removed: HashMap<Arc<RelPath>, Instant>,
}

/// Remove what serving left at a path the tree no longer has: a file in sync, a folder's files in sync and the
/// folder once it is empty. What a program put there since - in no state, or changed - stays, a stray of its own.
fn remove_in_sync(path: &HostPath) {
    if !path.is_dir() {
        let _ = std::fs::remove_file(path);
        return;
    }
    for inner in path.entries() {
        if inner.is_dir() || in_sync(&inner) {
            remove_in_sync(&inner);
        }
    }
    let _ = std::fs::remove_dir(path);
}

/// A path of the tree for a path in the folder.
///
/// # Errors
/// It is not in the folder, or it is by a name no path takes - which the folder can hold (a program made it) and the
/// project cannot: two answers every caller tells apart.
fn rel(root: &HostPath, path: &HostPath) -> Result<Arc<RelPath>, NotWithin> {
    path.within(root)
}

/// A place Windows says something of, as the system names it: plainly, or resolved where it said it otherwise. One
/// that names no place is refused by that.
fn placed(path: &std::path::Path) -> Result<HostPath, NotWithin> {
    HostPath::from_plain(path.to_path_buf()).map_or_else(
        || {
            HostPath::new(path).map_err(|why| {
                NotWithin::Refused(PathError { text: format!("{} ({why})", path.display()).into(), why: Why::NotText })
            })
        },
        Ok,
    )
}

/// What Windows asks of a path, as a path of the tree.
fn asked_of(root: &HostPath, path: &std::path::Path) -> Result<Arc<RelPath>, NotWithin> {
    rel(root, &placed(path)?)
}

fn time(at: SystemTime) -> FileTime {
    FileTime::try_from(at).unwrap_or(FileTime::NT_TIME_EPOCH)
}

/// A node's metadata as Windows keeps it on its placeholder: a file's read-only where nothing applies what programs
/// change.
fn metadata(node: &Node, read_only: bool) -> Metadata {
    const FILE_ATTRIBUTE_READONLY: u32 = 0x1;
    let at = time(node.modified);
    let folder = matches!(node.what, What::Folder(_));
    let base = if folder { Metadata::directory() } else { Metadata::file() };
    let base = if read_only && !folder { base.attributes(FILE_ATTRIBUTE_READONLY) } else { base };
    base.size(node.size).created(at).written(at).changed(at).accessed(at)
}

/// A node's placeholder in its folder, by its name: its metadata, its path in the tree as its identity (which Windows
/// requires of a file), in sync.
fn placeholder(name: &str, node: &Node, read_only: bool) -> PlaceholderCreateInfo {
    let made = PlaceholderCreateInfo::new(name)
        .metadata(metadata(node, read_only))
        .blob(node.path.as_str().as_bytes().to_vec())
        .mark_in_sync();
    if matches!(node.what, What::Folder(_)) { made } else { made.has_no_children() }
}

struct Filter {
    served: Served,
    owner: Option<Arc<dyn Owner>>,
    root: HostPath,
    pending: Arc<Mutex<Pending>>,
}

impl Filter {
    fn pending(&self) -> std::sync::MutexGuard<'_, Pending> {
        self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl SyncFilter for Filter {
    fn fetch_data(&self, request: Request, ticket: ticket::FetchData, info: info::FetchData) -> CResult<()> {
        // Only what the tree has is read in: no placeholder is of a name no path takes.
        let path = asked_of(&self.root, &request.path()).map_err(|_| CloudErrorKind::InvalidRequest)?;
        // Being read in: what serving waits for before it stops.
        let _open = self.served.opening();
        let tree = self.served.tree();
        let (node, _) = tree.at(&path).ok_or(CloudErrorKind::NotInSync)?;
        let range = info.required_file_range();
        // Read, and kept from now on as far as it was asked for.
        self.served.seen(&path, crate::Seen::Kept(range.end));
        let mut at = range.start;
        // One piece's room, filled again for each.
        let mut piece = vec![0u8; usize::try_from((range.end - at).min(PIECE)).unwrap_or(1 << 20)];
        while at < range.end {
            let len = usize::try_from((range.end - at).min(PIECE)).unwrap_or(1 << 20);
            let read = tree.read_into(node, at, &mut piece[..len]).map_err(|_| CloudErrorKind::Unsuccessful)?;
            if read == 0 {
                break;
            }
            ticket.write_at(&piece[..read], at).map_err(|_| CloudErrorKind::Unsuccessful)?;
            at += read as u64;
        }
        Ok(())
    }

    fn fetch_placeholders(
        &self,
        request: Request,
        ticket: ticket::FetchPlaceholders,
        _info: info::FetchPlaceholders,
    ) -> CResult<()> {
        let path = asked_of(&self.root, &request.path()).map_err(|_| CloudErrorKind::InvalidRequest)?;
        let tree = self.served.tree();
        let Some(children) = tree.entries(&path) else {
            return Ok(());
        };
        let read_only = self.owner.is_none();
        let mut made: Vec<PlaceholderCreateInfo> =
            children.iter().map(|(name, _, node)| placeholder(name.as_str(), node, read_only)).collect();
        // One there already (made here meanwhile) is no failure of the rest.
        let _ = ticket.pass_with_placeholders(&mut made);
        Ok(())
    }

    fn delete(&self, request: Request, ticket: ticket::Delete, _info: info::Delete) -> CResult<()> {
        // What is named so no path takes it is no project's: a program's own, removed as it likes.
        let outside = matches!(asked_of(&self.root, &request.path()), Err(NotWithin::Outside));
        if self.owner.is_none() || outside {
            return Err(CloudErrorKind::AccessDenied);
        }
        ticket.pass().map_err(|_| CloudErrorKind::Unsuccessful)
    }

    fn deleted(&self, request: Request, _info: info::Deleted) {
        // One named so no path takes it was never taken in: its removal is nothing to the project.
        if let Ok(path) = asked_of(&self.root, &request.path()) {
            self.pending().removed.insert(path, Instant::now());
        }
    }

    fn rename(&self, request: Request, ticket: ticket::Rename, info: info::Rename) -> CResult<()> {
        if self.owner.is_none() || matches!(asked_of(&self.root, &request.path()), Err(NotWithin::Outside)) {
            return Err(CloudErrorKind::AccessDenied);
        }
        // Renamed to what no path takes, it would be none of the project's: refused, as a name Windows refuses is - the
        // program is told, and nothing changes.
        if let Err(NotWithin::Refused(why)) = asked_of(&self.root, &info.target_path()) {
            eprintln!("{why}: a rename to it is refused");
            return Err(CloudErrorKind::InvalidRequest);
        }
        ticket.pass().map_err(|_| CloudErrorKind::Unsuccessful)
    }

    fn renamed(&self, request: Request, info: info::Renamed) {
        let (to, from) = (asked_of(&self.root, &request.path()), asked_of(&self.root, &info.source_path()));
        let Some(owner) = &self.owner else { return };
        let change = match (from, to) {
            (Ok(from), Ok(to)) => Change::Rename { from: TreePath::shared(from), to: TreePath::shared(to) },
            // Moved out: gone from the project.
            (Ok(from), Err(NotWithin::Outside)) => Change::Remove { path: TreePath::shared(from) },
            // Renamed to a name no path takes, which `rename` refuses ahead: where Windows did not ask, the project
            // keeps it as it was, and the file is a stray of the folder's - never removed from the project.
            (Ok(from), Err(NotWithin::Refused(why))) => {
                eprintln!("{why}: {from} was renamed to it; the project keeps it as {from}");
                return;
            }
            // Moved in, or a stray renamed: a new file, which the watcher sees.
            (Err(_), _) => return,
        };
        if let Err(why) = owner.change(change) {
            eprintln!("{why}");
        }
    }

    fn state_changed(&self, event: StateWatchEvent) {
        let StateWatchEvent::Changed(paths) = event else { return };
        let mut pending = self.pending();
        for path in paths {
            match placed(&path) {
                Ok(path) => drop(pending.changed.insert(path, Instant::now())),
                Err(why) => eprintln!("{why}: changed, and not taken into the project"),
            }
        }
    }
}

/// Opening a file so, Windows does not read its content back in where it is not on disk.
const FILE_FLAG_OPEN_NO_RECALL: u32 = 0x0010_0000;
/// A folder is opened so.
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
/// What is opened here is shared with whoever else has it open.
const SHARE_ALL: u32 = 0x7;

/// A file or folder opened to change its placeholder: a plain handle that may write (a folder's with backup semantics,
/// as Windows opens one), which reads nothing in.
fn writable(path: &HostPath, folder: bool) -> io::Result<Placeholder> {
    use std::os::windows::fs::OpenOptionsExt;
    let mut options = std::fs::OpenOptions::new();
    let flags = if folder { FILE_FLAG_BACKUP_SEMANTICS } else { 0 };
    options.read(true).write(true).share_mode(SHARE_ALL).custom_flags(flags | FILE_FLAG_OPEN_NO_RECALL);
    Ok(Placeholder::from(options.open(path)?))
}

/// A file or folder opened to be asked what it is as a placeholder, and for nothing else: for its attributes alone.
/// Opened as the Cloud Files API opens one (`CfOpenFileWithOplock`), Windows fetches a file whole whose content was
/// let go of, so walking a folder to ask would read back in all a collector freed.
fn asked(path: &HostPath) -> io::Result<Placeholder> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_READ_ATTRIBUTES: u32 = 0x80;
    let mut options = std::fs::OpenOptions::new();
    options
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(SHARE_ALL)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_NO_RECALL);
    Ok(Placeholder::from(options.open(path)?))
}

/// A file's or folder's state as a placeholder; none where it is a plain one. Asked of the file itself, opened: how
/// a folder lists it does not say, since Windows shows placeholders to other programs as plain files.
///
/// # Errors
/// It cannot be opened or asked: which says nothing of its state.
fn state(path: &HostPath) -> io::Result<Option<PlaceholderInfo>> {
    asked(path)?.info().map_err(io::Error::other)
}

/// Whether a file is a placeholder in sync: one serving made and nothing changed since.
#[must_use]
pub fn in_sync(path: &HostPath) -> bool {
    state(path).ok().flatten().is_some_and(|state| state.is_in_sync())
}

/// Mark what is on disk in sync with the project: a placeholder updated, anything else made one.
fn mark_in_sync(path: &HostPath, placeholder: bool, folder: bool) -> io::Result<()> {
    let mut opened = writable(path, folder)?;
    if placeholder {
        opened.update(UpdateOptions::default().mark_in_sync(), None)?;
    } else {
        opened.convert_to_placeholder(ConvertOptions::default().mark_in_sync(), None)?;
    }
    Ok(())
}

/// What applies what has settled, and keeps the placeholders made in step with the tree served.
struct Worker {
    served: Served,
    owner: Option<Arc<dyn Owner>>,
    root: HostPath,
    staging: HostPath,
    pending: Arc<Mutex<Pending>>,
}

impl Worker {
    fn run(&self, stop: &AtomicBool) {
        let mut last = self.served.tree();
        // What was left of an earlier serving, brought in step: the tree may have changed while nothing served it.
        self.sync(&last, &self.root, RelPath::root());
        while !stop.load(Ordering::Relaxed) {
            // Woken sooner by what stops it.
            std::thread::park_timeout(Duration::from_millis(250));
            self.settle(false);
            let now = self.served.tree();
            if !Arc::ptr_eq(&last, &now) {
                self.sync(&now, &self.root, RelPath::root());
                last = now;
            }
        }
        // As serving ends, what a program did a moment before is the project's too.
        self.settle(true);
    }

    /// Apply what has settled - or `all` that waits, settled or not: removals first - what is in a folder removed
    /// going with it - then changed files.
    fn settle(&self, all: bool) {
        // While nobody takes changes - a tool has the project - what waits goes on waiting, as it is in the folder:
        // given once someone does (asked only where something is due). As serving ends it is tried all the same, and
        // what is not taken stays a file there.
        let due = |at: &Instant| all || at.elapsed() >= SETTLE;
        let any = {
            let pending = self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            pending.removed.values().chain(pending.changed.values()).any(&due)
        };
        if !any || (!all && self.owner.as_ref().is_some_and(|owner| owner.ready().is_err())) {
            return;
        }
        let (mut removed, changed, waiting) = {
            let mut pending = self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let removed: Vec<Arc<RelPath>> =
                pending.removed.iter().filter(|(_, at)| due(at)).map(|(p, _)| p.clone()).collect();
            let changed: Vec<HostPath> =
                pending.changed.iter().filter(|(_, at)| due(at)).map(|(p, _)| p.clone()).collect();
            for path in &removed {
                pending.removed.remove(&**path);
            }
            for path in &changed {
                pending.changed.remove(path);
            }
            (removed, changed, pending.removed.keys().cloned().collect::<Vec<_>>())
        };
        let Some(owner) = &self.owner else { return };
        // What changed and is no longer there was removed. Windows tells of the removal of a placeholder alone
        // (`deleted`): what a program made a moment ago, and the project was given before it was one, goes without.
        let (gone, mut changed): (Vec<HostPath>, Vec<HostPath>) =
            changed.into_iter().partition(|path| std::fs::symlink_metadata(path).is_err());
        // What was named so no path takes it was never taken in: its going is nothing to the project.
        removed.extend(gone.iter().filter_map(|path| rel(&self.root, path).ok()).filter(|path| !path.is_root()));
        removed.sort();
        removed.dedup();
        // A removal inside one made with it, or still waiting, goes with that one.
        let inside = |path: &RelPath, other: &RelPath| path != other && path.starts_with(other);
        let covered = |path: &RelPath| removed.iter().chain(&waiting).any(|other| inside(path, other));
        let tree = self.served.tree();
        let asked: Vec<Change> = removed
            .iter()
            .filter(|path| !covered(path) && tree.at(path).is_some())
            .map(|path| Change::Remove { path: TreePath::shared(path.clone()) })
            .collect();
        if !asked.is_empty() {
            // Removed from the project together.
            for why in owner.changes(asked).into_iter().filter_map(Result::err) {
                eprintln!("{why}");
            }
        }
        // Declarations first; a file beside a new one waits for the tree to show its folder a node, so it is its file.
        changed.sort_by_key(|p| p.file_name().is_none_or(|n| n != project::index::NODE_FILE));
        for path in changed {
            if self.waits_for_its_node(&path) {
                let mut pending = self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                pending
                    .changed
                    .entry(path)
                    .or_insert_with(|| Instant::now().checked_sub(SETTLE).unwrap_or_else(Instant::now));
                continue;
            }
            if let Err(why) = self.take_in(owner.as_ref(), &path) {
                eprintln!("{}: {why}", path.display());
            }
        }
    }

    /// Whether a file waits for the `node.toml` beside it to make its folder a node.
    fn waits_for_its_node(&self, path: &HostPath) -> bool {
        let Some(folder) = path.parent() else { return false };
        if path.file_name().is_some_and(|n| n == project::index::NODE_FILE)
            || !folder.join(&project::index::NODE_FILE).is_file()
        {
            return false;
        }
        let tree = self.served.tree();
        rel(&self.root, &folder).is_ok_and(|at| !tree.at(&at).is_some_and(|(_, n)| n.asset))
    }

    /// A path seen changed: applied where it is a new folder, or a file new or changed here, then marked in sync.
    fn take_in(&self, owner: &dyn Owner, path: &HostPath) -> io::Result<()> {
        let tree_path = match rel(&self.root, path) {
            Ok(tree_path) if !tree_path.is_root() && !is_junk(&tree_path) => tree_path,
            Ok(_) | Err(NotWithin::Outside) => return Ok(()),
            // Written here by a name no path takes: it stays a file of the folder's, and the project is not given it.
            Err(NotWithin::Refused(why)) => {
                eprintln!("{why}: written in the folder, and not taken into the project until it is renamed");
                return Ok(());
            }
        };
        let Ok(meta) = std::fs::metadata(path) else { return Ok(()) };
        // Only a placeholder has a placeholder's state. Where it cannot be asked, nothing is known of the file - it
        // is not taken for changed, which would read it whole and write it to the project: asked again shortly.
        let Ok(state) = state(path) else {
            self.pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .changed
                .insert(path.clone(), Instant::now());
            return Ok(());
        };
        if state.as_ref().is_some_and(PlaceholderInfo::is_in_sync) {
            return Ok(());
        }
        let change = if meta.is_dir() {
            // One the tree has (moved here, which Windows marks changed) has nothing to apply: it is only marked.
            if self.served.tree().at(&tree_path).is_some() {
                return mark_in_sync(path, state.is_some(), true);
            }
            Change::Folder { path: TreePath::shared(tree_path) }
        } else {
            let staged =
                self.staging.join(&Name::new(&format!("cloud-{}", std::process::id())).map_err(io::Error::other)?);
            std::fs::copy(path, &staged)?;
            Change::Write { path: TreePath::shared(tree_path), staged: staged.into() }
        };
        owner.change(change).map_err(|why| io::Error::other(why.to_string()))?;
        mark_in_sync(path, state.is_some(), meta.is_dir())
    }

    /// Whether something changed here at a path - or in it, or above it - that the project has yet to be told: a
    /// file written or renamed, a removal waiting to settle.
    fn waits(&self, path: &HostPath, path_in_tree: &RelPath) -> bool {
        let pending = self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        pending.changed.keys().any(|changed| changed.starts_with(path) || path.starts_with(changed))
            || pending
                .removed
                .keys()
                .any(|removed| path_in_tree.starts_with(removed) || removed.starts_with(path_in_tree))
    }

    /// Bring what is on disk under a folder in step with the tree: a file in sync whose node changed emptied, to be
    /// read again; one no longer in the tree removed; a folder recursed into, and given placeholders of the nodes new
    /// in it where Windows filled it already (one it has not is filled when read, as the tree is then). What changed
    /// here, and is not applied yet, is left as it is.
    fn sync(&self, tree: &Tree, dir: &HostPath, at: &RelPath) {
        let read_only = self.owner.is_none();
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let mut here: Vec<Arc<Name>> = Vec::new();
        for entry in entries.flatten() {
            // Named so no path takes it: a program's own, none of the tree's - left as it is.
            let Some(name) = entry.file_name().to_str().and_then(|name| Name::new(name).ok()).map(Name::to_owned)
            else {
                continue;
            };
            let path_in_tree = at.join(&name);
            let path = dir.entry(&entry.file_name());
            here.push(name);
            let in_sync = in_sync(&path);
            let Some((_, node)) = tree.at(&path_in_tree) else {
                // What the tree no longer has goes - but not what a program changed here a moment ago and the
                // project has yet to be told: renamed, a file or a folder keeps the state it had, and the tree has
                // it under its new name only once the renaming is applied.
                if in_sync && !self.waits(&path, &path_in_tree) {
                    remove_in_sync(&path);
                }
                continue;
            };
            if matches!(node.what, What::Folder(_)) {
                self.sync(tree, &path, &path_in_tree);
                continue;
            }
            let stale = std::fs::metadata(&path).is_ok_and(|m| m.len() != node.size);
            if in_sync && stale {
                let options = UpdateOptions::default().metadata(metadata(&node, read_only)).dehydrate().mark_in_sync();
                if let Ok(mut placeholder) = writable(&path, false) {
                    let _ = placeholder.update(options, None);
                }
            }
        }
        // A folder filled shows what is new in it - filled empty too: a project's top, served before it held anything.
        let state = asked(dir).ok().and_then(|folder| folder.state().ok().flatten());
        let filled = state.map_or(!here.is_empty(), |state| !state.partial());
        if filled && let Some(children) = tree.entries(at) {
            for (name, _, node) in children.iter().filter(|(name, _, _)| !here.contains(name)) {
                // What a program removed or renamed away here a moment ago is not put back: the tree has it until
                // the project is told.
                if self.waits(&dir.join(&**name), &at.join(name)) {
                    continue;
                }
                let _ = placeholder(name.as_str(), node, read_only).create(dir);
            }
        }
    }
}

/// The Cloud Files API as a backend: the tree as placeholders at a folder of an NTFS volume, registered as a sync
/// root, written through where it has an owner.
struct AsBackend;

/// The Cloud Files API, among Windows' backends.
pub const BACKEND: &dyn crate::backend::Backend = &AsBackend;

impl crate::backend::Backend for AsBackend {
    fn name(&self) -> &'static str {
        "cloud"
    }

    fn label(&self) -> &'static str {
        "Cloud Files"
    }

    fn can(&self) -> crate::backend::Can {
        crate::backend::Can { write: true, folder: true }
    }

    fn keeps(&self) -> bool {
        true
    }

    fn prepares(&self) -> bool {
        true
    }

    fn available(&self, at: &HostPath) -> Result<(), String> {
        match crate::backend::volume_filesystem(at) {
            Some(filesystem) if !filesystem.eq_ignore_ascii_case("NTFS") => {
                Err(format!("the Cloud Files API serves on NTFS volumes, and this one is {filesystem}"))
            }
            _ => Ok(()),
        }
    }

    fn mount(&self, job: &crate::backend::Job) -> io::Result<Box<dyn crate::backend::Mount>> {
        let read_only = job.owner.is_none();
        let cloud = serve(job.served, job.owner.clone(), job.at, job.staging, job.name)?;
        Ok(Box::new(Kept { _cloud: cloud, root: job.at.clone(), shown: job.at.clone(), read_only }))
    }

    fn clear(&self, at: &HostPath, name: &str) {
        // Unregistered first, or Windows keeps what was not read of it from being removed.
        let _ = forget(at, name);
        let _ = std::fs::remove_dir_all(at);
    }

    fn retire(&self, at: &HostPath, name: &str) {
        // Told apart while Windows still knows the folder as a sync root - what is in sync is what it left, and can be
        // read again; anything else was put or changed there - then no longer one, which takes with it the placeholders
        // never read. Each file is opened to be asked: some seconds for a project listed whole, once, as another
        // backend takes over. A folder that leads elsewhere holds nothing of this one's: it is kept apart (`Apart`).
        if crate::backend::joined(at) {
            return;
        }
        let mut left = Vec::new();
        placeholders_in_sync(at, &mut left);
        let _ = forget(at, name);
        for path in left.iter().rev() {
            // Deepest first: a folder goes once what it held has, and stays where something was kept in it.
            let _ = if path.is_dir() { std::fs::remove_dir(path) } else { std::fs::remove_file(path) };
        }
    }

    fn collect(&self, at: &HostPath, record: &crate::access::Access, budget: u64) {
        let root = at.clone();
        let mut kept = record.kept_in_all();
        for (path, bytes) in record.oldest() {
            if kept <= budget {
                break;
            }
            let file = root.join(&*path);
            // Only what is as serving left it: what was changed there is someone's work, and stays.
            if in_sync(&file) {
                let let_go = writable(&file, false).is_ok_and(|mut placeholder| {
                    placeholder.update(UpdateOptions::default().dehydrate().mark_in_sync(), None).is_ok()
                });
                // In use, or refused: still kept, and asked again the next time.
                if !let_go {
                    continue;
                }
            }
            // Let go of, changed since or gone: none of it counts as kept any more.
            record.dropped(&path);
            kept = kept.saturating_sub(bytes);
        }
    }

    fn forget(&self, at: &HostPath, name: &str) -> io::Result<()> {
        forget(at, name)
    }

    fn left_as_served(&self, file: &HostPath) -> bool {
        in_sync(file)
    }
}

/// The Cloud Files API serving a folder whose content it keeps apart: the sync root is a folder of its own, where such
/// things are kept on this machine (`backend::apart_for`: an NTFS volume), and the folder served is a junction to it.
/// What a project on a volume the API cannot serve - `ReFS` - is served by all the same; and a choice of its own where
/// it can, for a project's folder that is to hold nothing of what was read.
struct Apart;

/// The Cloud Files API keeping apart what it serves, among Windows' backends.
pub const APART: &dyn crate::backend::Backend = &Apart;

/// Beside a tree kept apart, what says whose it is: the folder it is served at, then the name it is served by.
const WHOSE: &Name = Name::constant("project");
/// The tree kept apart, in the folder that is its own.
const TREE: &Name = Name::constant("tree");

/// The folder of what is kept apart of a folder served: its own, by the place of the one served.
fn apart(at: &HostPath) -> Option<HostPath> {
    let place = project::extract::sha1_hex(at.to_string().to_lowercase().as_bytes());
    Some(crate::backend::apart_for(at)?.join(&Name::new(&place[..16]).ok()?))
}

/// Whether two paths name one folder, as Windows compares them.
fn same(a: &HostPath, b: &HostPath) -> bool {
    let written = |path: &HostPath| path.to_string().trim_end_matches('\\').to_lowercase();
    written(a) == written(b)
}

/// The tree a folder leads to, where that is the one kept apart for it.
fn apart_tree(at: &HostPath) -> Option<HostPath> {
    let (to, tree) = (crate::backend::joined_to(at)?, apart(at)?.join(&TREE));
    same(&to, &tree).then_some(tree)
}

/// Whether a folder holds a file, anywhere in it.
fn holds_files(dir: &HostPath) -> bool {
    dir.entries().any(|entry| !entry.is_dir() || holds_files(&entry))
}

/// Make a folder the junction to its tree kept apart. One that leads elsewhere is made again - another project's
/// tree, where this project is a copy of that one; a folder of its own has to hold no file.
fn link(at: &HostPath, tree: &HostPath) -> io::Result<()> {
    if crate::backend::joined_to(at).is_some_and(|to| same(&to, tree)) {
        return Ok(());
    }
    crate::backend::unjoin(at);
    if at.exists() {
        if holds_files(at) {
            return Err(io::Error::other(format!("{} holds files: it cannot lead elsewhere", at.display())));
        }
        std::fs::remove_dir_all(at)?;
    }
    crate::backend::join_to(at, tree)
}

/// Let go of the trees kept apart, beside this one, whose folder no longer leads to them: a project removed, moved,
/// or served otherwise since.
fn sweep_apart(all: &HostPath, but: &HostPath) {
    for kept in all.entries() {
        let Ok(whose) = std::fs::read_to_string(kept.join(&WHOSE)) else { continue };
        let mut said = whose.lines();
        let (Some(at), Some(name)) = (said.next(), said.next()) else { continue };
        let tree = kept.join(&TREE);
        let led = placed(std::path::Path::new(at)).ok().and_then(|at| crate::backend::joined_to(&at));
        if same(&kept, but) || led.is_some_and(|to| same(&to, &tree)) {
            continue;
        }
        let _ = forget(&tree, name);
        let _ = std::fs::remove_dir_all(&kept);
    }
}

/// Move what a folder holds into another, folder by folder: a move where it can be one, else a copy (another volume).
fn move_into(from: &HostPath, to: &HostPath) {
    for entry in std::fs::read_dir(from).into_iter().flatten().flatten() {
        let (path, into) = (from.entry(&entry.file_name()), to.entry(&entry.file_name()));
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            if std::fs::create_dir_all(&into).is_ok() {
                move_into(&path, &into);
            }
        } else if std::fs::rename(&path, &into).is_err() && std::fs::copy(&path, &into).is_ok() {
            let _ = std::fs::remove_file(&path);
        }
    }
}

impl crate::backend::Backend for Apart {
    fn name(&self) -> &'static str {
        "cloud-cache"
    }

    fn label(&self) -> &'static str {
        "Cloud Files, kept apart"
    }

    fn can(&self) -> crate::backend::Can {
        crate::backend::Can { write: true, folder: true }
    }

    fn keeps(&self) -> bool {
        true
    }

    fn prepares(&self) -> bool {
        true
    }

    fn available(&self, at: &HostPath) -> Result<(), String> {
        let Some(kept) = apart(at) else {
            return Err("where to keep it apart is not known: `[filesystem] cache` says".into());
        };
        let filesystem = crate::backend::volume_filesystem;
        if let Some(theirs) = filesystem(&kept).filter(|name| !name.eq_ignore_ascii_case("NTFS")) {
            return Err(format!(
                "the Cloud Files API serves on NTFS volumes, and it would be kept apart at {}, on {theirs}",
                kept.display()
            ));
        }
        match filesystem(at) {
            Some(ours) if !["NTFS", "ReFS"].iter().any(|known| ours.eq_ignore_ascii_case(known)) => {
                Err(format!("a folder on {ours} cannot lead to one elsewhere"))
            }
            _ => Ok(()),
        }
    }

    fn mount(&self, job: &crate::backend::Job) -> io::Result<Box<dyn crate::backend::Mount>> {
        let at = job.at.clone();
        let kept = apart(&at).ok_or_else(|| io::Error::other("where to keep it apart is not known"))?;
        let tree = kept.join(&TREE);
        std::fs::create_dir_all(&tree)?;
        if let Some(all) = kept.parent() {
            sweep_apart(&all, &kept);
        }
        link(&at, &tree)?;
        // Said once the folder leads to it: one that says so and is led to by nothing is swept.
        std::fs::write(kept.join(&WHOSE), format!("{}\n{}\n", at.display(), job.name))?;
        let read_only = job.owner.is_none();
        let cloud = serve(job.served, job.owner.clone(), &tree, job.staging, job.name)?;
        Ok(Box::new(Kept { _cloud: cloud, root: tree, shown: at, read_only }))
    }

    fn clear(&self, at: &HostPath, name: &str) {
        let Some(kept) = apart(at) else { return };
        let tree = kept.join(&TREE);
        // Unregistered first, or Windows keeps what was not read of it from being removed.
        let _ = forget(&tree, name);
        let _ = std::fs::remove_dir_all(&kept);
        if crate::backend::joined_to(at).is_some_and(|to| same(&to, &tree)) {
            let _ = std::fs::remove_dir(at);
        }
    }

    fn retire(&self, at: &HostPath, name: &str) {
        let Some(tree) = apart_tree(at) else { return };
        // As the API serving in place retires: what is in sync is what it left, and goes; anything else was put or
        // changed there, and stays - in the folder itself, which is one of its own again.
        let mut left = Vec::new();
        placeholders_in_sync(&tree, &mut left);
        let _ = forget(&tree, name);
        for path in left.iter().rev() {
            let _ = if path.is_dir() { std::fs::remove_dir(path) } else { std::fs::remove_file(path) };
        }
        crate::backend::unjoin(at);
        move_into(&tree, at);
        // Gone with what it kept, unless something of it could not be moved.
        if let Some(kept) = tree.parent().filter(|_| !holds_files(&tree)) {
            let _ = std::fs::remove_dir_all(kept);
        }
    }

    fn collect(&self, at: &HostPath, record: &crate::access::Access, budget: u64) {
        // By the folder served: what is opened through it is in the tree kept apart.
        AsBackend.collect(at, record, budget);
    }

    fn forget(&self, at: &HostPath, name: &str) -> io::Result<()> {
        apart(at).map_or(Ok(()), |kept| forget(&kept.join(&TREE), name))
    }

    fn left_as_served(&self, file: &HostPath) -> bool {
        in_sync(file)
    }
}

/// What a folder holds that serving left there as it was - the files that are placeholders in sync - and every folder
/// in it, each before what is in it: a folder holds nothing of its own, and goes where nothing is kept in it.
fn placeholders_in_sync(dir: &HostPath, found: &mut Vec<HostPath>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = dir.entry(&entry.file_name());
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            found.push(path.clone());
            placeholders_in_sync(&path, found);
        } else if in_sync(&path) {
            found.push(path);
        }
    }
}

/// A folder served through the Cloud Files API, as a mount: where its placeholders are, and where it is shown - the
/// same folder, but where it is kept apart.
struct Kept {
    _cloud: Cloud,
    root: HostPath,
    shown: HostPath,
    read_only: bool,
}

impl crate::backend::Mount for Kept {
    fn shown(&self) -> String {
        self.shown.to_string()
    }

    fn placed(&self, served: &Served, path: &RelPath) {
        place(served, &self.root, path, self.read_only);
    }
}
