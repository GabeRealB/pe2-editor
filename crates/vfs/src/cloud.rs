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

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use cloud_filter::error::{CResult, CloudErrorKind};
use cloud_filter::filter::{Request, StateWatchEvent, SyncFilter, info, ticket};
use cloud_filter::metadata::Metadata;
use cloud_filter::placeholder::{ConvertOptions, Placeholder, UpdateOptions};
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
}

impl Drop for Cloud {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// A sync root's id: this provider's, the user's, the project's (by its name).
fn id_of(name: &str) -> io::Result<SyncRootId> {
    let account: String = name.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
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
    at: &Path,
    staging: &Path,
    name: &str,
) -> io::Result<Cloud> {
    // Windows names the folder plainly - `D:\...` - in what it asks and registers; not as canonicalized (`\\?\D:\...`).
    let at = &plain(at);
    std::fs::create_dir_all(at)?;
    std::fs::create_dir_all(staging)?;
    let step = |what: &'static str| move |e: windows_error::Error| io::Error::other(format!("{what}: {e}"));
    let id = id_of(name)?;
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
    let worker = Worker { served: served.clone(), owner, root: at.clone(), staging: staging.to_path_buf(), pending };
    let stopping = stopped.clone();
    std::thread::Builder::new().name("cloud".into()).spawn(move || worker.run(&stopping))?;
    Ok(Cloud { _connection: connection, stop: stopped })
}

/// Unregister a project's folder: it is no longer a sync root, and what was not read of it cannot be.
///
/// # Errors
/// It cannot be unregistered.
pub fn forget(name: &str) -> io::Result<()> {
    let id = id_of(name)?;
    if id.is_registered()? {
        id.unregister()?;
    }
    Ok(())
}

/// A path without the verbatim prefix canonicalizing gives it (`\\?\D:\x` as `D:\x`), where it has a drive.
fn plain(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => path.to_path_buf(),
    }
}

/// What waits to be applied: files seen changed, and removals, each by when it was last touched.
#[derive(Default)]
struct Pending {
    changed: HashMap<PathBuf, Instant>,
    removed: HashMap<String, Instant>,
}

/// A path of the tree for a path in the folder: its steps joined by `/`.
fn rel(root: &Path, path: &Path) -> Option<String> {
    let inside = path.strip_prefix(root).ok()?;
    let steps: Option<Vec<&str>> = inside.components().map(|c| c.as_os_str().to_str()).collect();
    Some(steps?.join("/"))
}

fn time(at: SystemTime) -> FileTime {
    FileTime::try_from(at).unwrap_or(FileTime::NT_TIME_EPOCH)
}

/// A node's metadata as Windows keeps it on its placeholder.
fn metadata(node: &Node) -> Metadata {
    let at = time(node.modified);
    let base = if matches!(node.what, What::Folder(_)) { Metadata::directory() } else { Metadata::file() };
    base.size(node.size).created(at).written(at).changed(at).accessed(at)
}

/// A node's placeholder in its folder, by its name: its metadata, its path in the tree as its identity (which Windows
/// requires of a file), in sync.
fn placeholder(name: &str, node: &Node) -> PlaceholderCreateInfo {
    let made =
        PlaceholderCreateInfo::new(name).metadata(metadata(node)).blob(node.path.as_bytes().to_vec()).mark_in_sync();
    if matches!(node.what, What::Folder(_)) { made } else { made.has_no_children() }
}

struct Filter {
    served: Served,
    owner: Option<Arc<dyn Owner>>,
    root: PathBuf,
    pending: Arc<Mutex<Pending>>,
}

impl Filter {
    fn pending(&self) -> std::sync::MutexGuard<'_, Pending> {
        self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl SyncFilter for Filter {
    fn fetch_data(&self, request: Request, ticket: ticket::FetchData, info: info::FetchData) -> CResult<()> {
        let path = rel(&self.root, &request.path()).ok_or(CloudErrorKind::InvalidRequest)?;
        // Being read in: what serving waits for before it stops.
        let _open = self.served.opening();
        let tree = self.served.tree();
        let (node, _) = tree.at(&path).ok_or(CloudErrorKind::NotInSync)?;
        let range = info.required_file_range();
        let mut at = range.start;
        while at < range.end {
            let len = usize::try_from((range.end - at).min(PIECE)).unwrap_or(1 << 20);
            let bytes = tree.read(node, at, len).map_err(|_| CloudErrorKind::Unsuccessful)?;
            if bytes.is_empty() {
                break;
            }
            ticket.write_at(&bytes, at).map_err(|_| CloudErrorKind::Unsuccessful)?;
            at += bytes.len() as u64;
        }
        Ok(())
    }

    fn fetch_placeholders(
        &self,
        request: Request,
        ticket: ticket::FetchPlaceholders,
        _info: info::FetchPlaceholders,
    ) -> CResult<()> {
        let path = rel(&self.root, &request.path()).ok_or(CloudErrorKind::InvalidRequest)?;
        let tree = self.served.tree();
        let Some((_, Node { what: What::Folder(children), .. })) = tree.at(&path) else {
            return Ok(());
        };
        let mut made: Vec<PlaceholderCreateInfo> =
            children.iter().filter_map(|(name, ino)| Some(placeholder(name, tree.node(*ino)?))).collect();
        // One there already (made here meanwhile) is no failure of the rest.
        let _ = ticket.pass_with_placeholders(&mut made);
        Ok(())
    }

    fn delete(&self, request: Request, ticket: ticket::Delete, _info: info::Delete) -> CResult<()> {
        if self.owner.is_none() || rel(&self.root, &request.path()).is_none() {
            return Err(CloudErrorKind::AccessDenied);
        }
        ticket.pass().map_err(|_| CloudErrorKind::Unsuccessful)
    }

    fn deleted(&self, request: Request, _info: info::Deleted) {
        if let Some(path) = rel(&self.root, &request.path()) {
            self.pending().removed.insert(path, Instant::now());
        }
    }

    fn rename(&self, request: Request, ticket: ticket::Rename, _info: info::Rename) -> CResult<()> {
        if self.owner.is_none() || rel(&self.root, &request.path()).is_none() {
            return Err(CloudErrorKind::AccessDenied);
        }
        ticket.pass().map_err(|_| CloudErrorKind::Unsuccessful)
    }

    fn renamed(&self, request: Request, info: info::Renamed) {
        let (to, from) = (rel(&self.root, &request.path()), rel(&self.root, &info.source_path()));
        let Some(owner) = &self.owner else { return };
        let change = match (from, to) {
            (Some(from), Some(to)) => Change::Rename { from, to },
            // Moved out: gone from the project.
            (Some(from), None) => Change::Remove { path: from },
            // Moved in: a new file, which the watcher sees.
            _ => return,
        };
        if let Err(why) = owner.change(change) {
            eprintln!("{why}");
        }
    }

    fn state_changed(&self, event: StateWatchEvent) {
        let StateWatchEvent::Changed(paths) = event else { return };
        let mut pending = self.pending();
        for path in paths {
            pending.changed.insert(path, Instant::now());
        }
    }
}

/// A file or folder opened to change its placeholder: a plain handle that may write (a folder's with backup semantics,
/// as Windows opens one).
fn writable(path: &Path, folder: bool) -> io::Result<Placeholder> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const SHARE_ALL: u32 = 0x7;
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).share_mode(SHARE_ALL);
    if folder {
        options.custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
    }
    Ok(Placeholder::from(options.open(path)?))
}

/// Mark what is on disk in sync with the project: a placeholder updated, anything else made one.
/// Whether a file is a placeholder in sync: one serving made and nothing changed since.
#[must_use]
pub fn in_sync(path: &Path) -> bool {
    let state = Placeholder::open(path).ok().and_then(|p| p.info().ok().flatten());
    state.as_ref().is_some_and(cloud_filter::placeholder::PlaceholderInfo::is_in_sync)
}

fn mark_in_sync(path: &Path, placeholder: bool, folder: bool) -> io::Result<()> {
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
    root: PathBuf,
    staging: PathBuf,
    pending: Arc<Mutex<Pending>>,
}

impl Worker {
    fn run(&self, stop: &AtomicBool) {
        let mut last = self.served.tree();
        // What was left of an earlier serving, brought in step: the tree may have changed while nothing served it.
        Self::sync(&last, &self.root, "");
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(250));
            self.settle();
            let now = self.served.tree();
            if !Arc::ptr_eq(&last, &now) {
                Self::sync(&now, &self.root, "");
                last = now;
            }
        }
    }

    /// Apply what has settled: removals first - what is in a folder removed going with it - then changed files.
    fn settle(&self) {
        let due = |at: &Instant| at.elapsed() >= SETTLE;
        let (removed, changed) = {
            let mut pending = self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let removed: Vec<String> =
                pending.removed.iter().filter(|(_, at)| due(at)).map(|(p, _)| p.clone()).collect();
            let changed: Vec<PathBuf> =
                pending.changed.iter().filter(|(_, at)| due(at)).map(|(p, _)| p.clone()).collect();
            let all = pending.removed.keys().cloned().collect::<Vec<_>>();
            for path in &removed {
                pending.removed.remove(path);
            }
            for path in &changed {
                pending.changed.remove(path);
            }
            // A removal inside one still waiting goes with it.
            let covered = |path: &str| all.iter().any(|other| path.starts_with(&format!("{other}/")));
            (removed.into_iter().filter(|p| !covered(p)).collect::<Vec<_>>(), changed)
        };
        let Some(owner) = &self.owner else { return };
        for path in removed {
            if self.served.tree().at(&path).is_some()
                && let Err(why) = owner.change(Change::Remove { path })
            {
                eprintln!("{why}");
            }
        }
        // Declarations first; a file beside a new one waits for the tree to show its folder a node, so it is its file.
        let mut changed = changed;
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
    fn waits_for_its_node(&self, path: &Path) -> bool {
        let Some(folder) = path.parent() else { return false };
        if path.file_name().is_some_and(|n| n == project::index::NODE_FILE)
            || !folder.join(project::index::NODE_FILE).is_file()
        {
            return false;
        }
        let tree = self.served.tree();
        rel(&self.root, folder).is_some_and(|at| !tree.at(&at).is_some_and(|(_, n)| n.asset))
    }

    /// A path seen changed: applied where it is a new folder, or a file new or changed here, then marked in sync.
    fn take_in(&self, owner: &dyn Owner, path: &Path) -> io::Result<()> {
        let Some(tree_path) = rel(&self.root, path).filter(|p| !p.is_empty() && !is_junk(p)) else { return Ok(()) };
        let Ok(meta) = std::fs::metadata(path) else { return Ok(()) };
        // Any file opens as one; only a placeholder has a placeholder's state.
        let state = Placeholder::open(path).ok().and_then(|p| p.info().ok().flatten());
        if state.as_ref().is_some_and(cloud_filter::placeholder::PlaceholderInfo::is_in_sync) {
            return Ok(());
        }
        let change = if meta.is_dir() {
            // One the tree has (moved here, which Windows marks changed) has nothing to apply: it is only marked.
            if self.served.tree().at(&tree_path).is_some() {
                return mark_in_sync(path, state.is_some(), true);
            }
            Change::Folder { path: tree_path }
        } else {
            let staged = self.staging.join(format!("cloud-{}", std::process::id()));
            std::fs::copy(path, &staged)?;
            Change::Write { path: tree_path, staged }
        };
        owner.change(change).map_err(io::Error::other)?;
        mark_in_sync(path, state.is_some(), meta.is_dir())
    }

    /// Bring what is on disk under a folder in step with the tree: a file in sync whose node changed emptied, to be
    /// read again; one no longer in the tree removed; a folder recursed into, and given placeholders of the nodes new
    /// in it where Windows filled it already (one it has not is filled when read, as the tree is then). What changed
    /// here, and is not applied yet, is left as it is.
    fn sync(tree: &Tree, dir: &Path, at: &str) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let mut here: Vec<String> = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path_in_tree = if at.is_empty() { name.clone() } else { format!("{at}/{name}") };
            let path = entry.path();
            here.push(name);
            let in_sync =
                Placeholder::open(&path).ok().and_then(|p| p.info().ok().flatten()).is_some_and(|i| i.is_in_sync());
            let Some((_, node)) = tree.at(&path_in_tree) else {
                if in_sync {
                    let _ = if path.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
                }
                continue;
            };
            if matches!(node.what, What::Folder(_)) {
                Self::sync(tree, &path, &path_in_tree);
                continue;
            }
            let stale = std::fs::metadata(&path).is_ok_and(|m| m.len() != node.size);
            if in_sync && stale {
                let options = UpdateOptions::default().metadata(metadata(node)).dehydrate().mark_in_sync();
                if let Ok(mut placeholder) = writable(&path, false) {
                    let _ = placeholder.update(options, None);
                }
            }
        }
        // A folder filled shows what is new in it - filled empty too: a project's top, served before it held anything.
        let state = Placeholder::open(dir).ok().and_then(|p| p.state().ok().flatten());
        let filled = state.map_or(!here.is_empty(), |state| !state.partial());
        if let Some((_, Node { what: What::Folder(children), .. })) = tree.at(at)
            && filled
        {
            for (name, ino) in children.iter().filter(|(name, _)| !here.contains(name)) {
                let Some(node) = tree.node(*ino) else { continue };
                let _ = placeholder(name, node).create(dir);
            }
        }
    }
}
