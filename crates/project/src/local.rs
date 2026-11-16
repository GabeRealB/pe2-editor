//! What of a project is this machine's alone: `.local/` beside `project.toml`, never shipped with the project (the
//! `.gitignore` a new project is made with leaves it out). Its parts:
//!
//! - `processes/`: what the processes working on the project find each other by - the project lock (`lock.rs`), the
//!   sockets, locks and logs of its host and its server (`role.rs`), the socket agents reach its editor by
//!   (`SOCKET`), the scopes and access records of what is served (`vfs`).
//! - `filesystem/`: the shared filesystem's log and the space its processes share (`storage.rs`, `shared.rs`).
//! - `history/`: the session's history - its steps, blobs, journal and trash - wiped as a session closes cleanly.
//! - `conflicts/`: what writes that met a change made meanwhile left aside, kept until the user settles them.
//! - `outputs/`: what is derived from the layers - the work graph's results, imports listed, merges, materialized
//!   files. Never the only copy of anything: it may be deleted at any time at the cost of time only, and its
//!   `version` names the schema and the tools that wrote it, so outputs written by others are wiped on opening.
//!   It is the only part a change of schema wipes.
//! - `store/`: content only this machine's records name.
//! - `layers/`: each layer's part only this machine has.
//! - `set-aside/`: what the served folder held that was not the project's, a folder a time.
//! - `settings.toml`: this machine's settings of the project and the editor's view of it.

use anyhow::{Context, Result};
use pe2_path::Name;
use sfs::HostPath;
use std::io;

use crate::Project;
use crate::lock::Lock;

pub const DIR: &Name = Name::constant(".local");
/// What the processes working on the project find each other by.
pub const PROCESSES: &Name = Name::constant("processes");
/// Where what programs write through the filesystem served is staged until it is applied (`vfs::writes`).
pub const STAGING: &Name = Name::constant("filesystem");
/// The session's history.
pub const HISTORY: &Name = Name::constant("history");
/// What writes that met a change made meanwhile left aside.
pub const CONFLICTS: &Name = Name::constant("conflicts");
/// What is derived from the layers: the only part a change of schema wipes.
pub const OUTPUTS: &Name = Name::constant("outputs");
/// Content only this machine's records name.
pub const STORE: &Name = Name::constant("store");
/// Each layer's part only this machine has.
pub const LAYERS: &Name = Name::constant("layers");
/// What the served folder held that was not the project's.
pub const SET_ASIDE: &Name = Name::constant("set-aside");
/// Where a file written whole takes its temporary name (`write_atomic`), on the project's volume, in no tree.
pub const TEMPORARY: &Name = Name::constant("temporary");
/// How long a temporary file is left: longer than any write takes from making it to renaming it into place.
const LEFT_FOR: std::time::Duration = std::time::Duration::from_hours(1);
/// This machine's settings of the project.
pub const SETTINGS: &Name = Name::constant("settings.toml");
/// The socket agents reach the editor holding the project by (`mcp::link`): listening from the moment it holds the
/// project's lock, before it opens the outputs.
pub const SOCKET: &Name = Name::constant("editor.sock");
/// The shape of what the outputs hold: outputs of another are wiped. 1 until the editor's first release: outputs of an
/// older shape are made again, never told apart.
pub(crate) const SCHEMA: u32 = 1;
/// The schema and the tools that wrote the outputs.
const VERSION: &Name = Name::constant("version");
/// What has git ignore what it names.
pub const GITIGNORE: &Name = Name::constant(".gitignore");

/// A part of the project in `root`'s local folder.
#[must_use]
pub fn part(root: &HostPath, name: &Name) -> HostPath {
    root.join(&DIR).join(name)
}

/// Where this machine's settings of the project in `root` are.
#[must_use]
pub fn settings(root: &HostPath) -> HostPath {
    part(root, SETTINGS)
}

#[derive(Clone)]
pub struct Local {
    /// The project's local folder.
    pub dir: HostPath,
    /// The project's I/O, for what the local folder opens (its history, its storage).
    pub io: std::sync::Arc<dyn pe2_io::Io>,
    /// What was written again from the log as the local folder was opened (`storage::redo`), where anything was: changes
    /// whoever held the project before had said whole and may not have had in its files.
    pub recovered: Option<String>,
}

impl Local {
    /// The project's local folder, made if missing, its outputs wiped if another schema or version wrote them. Only
    /// the holder of the project's lock opens it.
    ///
    /// # Errors
    /// The folder cannot be made, its outputs wiped or written.
    pub fn open(project: &Project, _lock: &Lock) -> Result<Local> {
        let fs = project.io.filesystem();
        let dir = project.root.join(&DIR);
        let outputs = dir.join(&OUTPUTS);
        let version = format!("schema {SCHEMA}\nproject {}\n", env!("CARGO_PKG_VERSION"));
        let current = fs.read_to_string(&outputs.join(&VERSION)).ok();
        if current.as_deref() != Some(version.as_str()) {
            if fs.exists(&outputs) {
                fs.remove_all(&outputs).with_context(|| format!("wiping {}", outputs.display()))?;
            }
            fs.create_folder_all(&outputs).with_context(|| format!("{}", outputs.display()))?;
            write_atomic(fs, &outputs.join(&VERSION), version.as_bytes())?;
        }
        // Before anything reads the project's files: they are as the log says they are to be.
        let recovered = crate::storage::redo(&project.root, fs)?;
        sweep_temporary(fs, &project.root, project.io.clock().wall());
        Ok(Local { dir, io: project.io.clone(), recovered })
    }

    /// A part of the local folder.
    #[must_use]
    pub fn part(&self, name: &Name) -> HostPath {
        self.dir.join(name)
    }

    /// A file or folder of the outputs.
    #[must_use]
    pub fn output(&self, name: &Name) -> HostPath {
        self.dir.join(&OUTPUTS).join(name)
    }
}

/// A table of this machine's settings of the project in `root`, by its keys (`["editor", "layers"]`), read as `T`:
/// none where the settings hold none, cannot be read, or hold one that does not read as `T`.
#[must_use]
pub fn setting<T: serde::de::DeserializeOwned>(
    fs: &dyn pe2_io::Filesystem,
    root: &HostPath,
    keys: &[&str],
) -> Option<T> {
    let text = fs.read_to_string(&settings(root)).ok()?;
    let mut at = toml::Value::Table(toml::from_str(&text).ok()?);
    for key in keys {
        at = at.as_table_mut()?.remove(*key)?;
    }
    at.try_into().ok()
}

/// Write a table of this machine's settings of the project in `root`, by its keys, the rest of them as they are; the
/// file is written whole, and its folder made where it is missing.
///
/// # Errors
/// The settings cannot be read as TOML, a key names something else than a table, or the file cannot be written.
pub fn set_setting<T: serde::Serialize>(
    fs: &dyn pe2_io::Filesystem,
    root: &HostPath,
    keys: &[&str],
    value: &T,
) -> Result<()> {
    let file = settings(root);
    let text = fs.read_to_string(&file).unwrap_or_default();
    let mut document: toml_edit::DocumentMut = text.parse().with_context(|| file.display().to_string())?;
    let written: toml_edit::DocumentMut = toml::to_string(value)?.parse()?;
    let (last, within) = keys.split_last().context("a setting is named by one key at least")?;
    let mut table = document.as_table_mut();
    for key in within {
        let item = table.entry(key).or_insert_with(|| {
            let mut implicit = toml_edit::Table::new();
            implicit.set_implicit(true);
            toml_edit::Item::Table(implicit)
        });
        table = item.as_table_mut().with_context(|| format!("{}: `{key}` is no table", file.display()))?;
    }
    table.insert(last, toml_edit::Item::Table(written.as_table().clone()));
    write_settings(fs, &file, &document)
}

/// Write this machine's settings of a project whole, its local folder made where it is missing.
///
/// # Errors
/// The folder or the file cannot be written.
pub fn write_settings(fs: &dyn pe2_io::Filesystem, file: &HostPath, document: &toml_edit::DocumentMut) -> Result<()> {
    if let Some(folder) = file.parent() {
        fs.create_folder_all(&folder).with_context(|| folder.display().to_string())?;
    }
    write_atomic(fs, file, document.to_string().as_bytes())
}

/// Make the folder of the processes working on the project in `root`, where it is missing: its lock and their
/// sockets are made there before anything else of the project is opened.
///
/// # Errors
/// The folder cannot be made.
pub fn processes(fs: &dyn pe2_io::Filesystem, root: &HostPath) -> Result<HostPath> {
    let dir = part(root, PROCESSES);
    fs.create_folder_all(&dir).with_context(|| format!("{}", dir.display()))?;
    Ok(dir)
}

/// Write a file whole: under a temporary name, then renamed over it, so no reader ever sees half a file. One that
/// fails leaves nothing under the temporary name.
///
/// # Errors
/// The temporary file cannot be written or renamed.
pub fn write_atomic(fs: &dyn pe2_io::Filesystem, path: &HostPath, bytes: &[u8]) -> Result<()> {
    write_whole(fs, path, bytes, false).with_context(|| path.display().to_string())
}

/// Write `bytes` at `path` whole: under a temporary name - in its project's local folder (`temporary_for`), so that none
/// is ever left in a tree; beside it for a file of no project, or one on another volume than its project's - then
/// renamed over it. Each write has a temporary name of its own: two threads may be writing one file at once. Where
/// the rename fails and the file is there all the same while `already` says that will do - what a store keeps by its
/// hash, put there meanwhile by another - the write's own goes, and what is there stands.
///
/// # Errors
/// The temporary file cannot be written or renamed.
pub(crate) fn write_whole(fs: &dyn pe2_io::Filesystem, path: &HostPath, bytes: &[u8], already: bool) -> io::Result<()> {
    static WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let write = WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let own = format!("{}.{write}.tmp", std::process::id());
    let put = |temporary: &HostPath| -> io::Result<()> {
        fs.write(temporary, bytes)?;
        match fs.rename(temporary, path) {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = fs.remove_file(temporary);
                if already && fs.exists(path) { Ok(()) } else { Err(e) }
            }
        }
    };
    if let Some(folder) = crate::temporary_for(path) {
        let temporary = folder.join(&Name::new(&own).map_err(io::Error::other)?);
        let tried = put(&temporary).or_else(|e| {
            // Its folder made where it is not yet, once.
            if e.kind() == io::ErrorKind::NotFound && !fs.exists(&folder) {
                fs.create_folder_all(&folder)?;
                return put(&temporary);
            }
            Err(e)
        });
        match tried {
            // On another volume: beside it.
            Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {}
            done => return done,
        }
    }
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let beside = format!(".{name}.{own}");
    let beside = Name::new(&beside).map_err(io::Error::other)?;
    put(&path.parent().map(|folder| folder.join(beside)).ok_or_else(|| io::Error::other("no folder"))?)
}

/// Remove what writes left in the project's folder of temporary names (`write_whole`) longer ago than any write takes:
/// what a process dying mid-write left there.
fn sweep_temporary(fs: &dyn pe2_io::Filesystem, root: &HostPath, now: std::time::SystemTime) {
    let folder = part(root, TEMPORARY);
    for entry in fs.read_directory(&folder).unwrap_or_default() {
        let path = folder.entry(&entry.name);
        let modified = fs.stat(&path).ok().and_then(|stat| stat.modified);
        if modified.and_then(|at| now.duration_since(at).ok()).is_some_and(|age| age >= LEFT_FOR) {
            let _ = fs.remove_file(&path);
        }
    }
}

/// Remove a test's project, whatever a history still alive in it is writing behind this moment: tried again
/// until nothing is put there meanwhile.
#[cfg(test)]
#[allow(
    clippy::disallowed_methods,
    reason = "a test's fixtures: the machine's time, places and environment, which nothing tested reads"
)]
pub(crate) fn remove_all(root: &HostPath) {
    for _ in 0..200 {
        match std::fs::remove_dir_all(root) {
            Ok(()) => return,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(5)),
        }
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(test)]
#[allow(
    clippy::disallowed_methods,
    reason = "a test's fixtures: the machine's time, places, files and environment, which nothing tested reads"
)]
mod tests {
    use crate::testing::At as _;
    #[test]
    fn a_wipe_takes_the_outputs_alone() {
        let root = crate::testing::scratch("local-wipe");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.at("project.toml"), "").unwrap();
        let project = crate::Project::load(&root).unwrap();
        let lock = crate::lock::Lock::try_acquire(&root, "a test", &pe2_io::System).unwrap();
        let (output, kept) = (root.at(".local/outputs/left.before"), root.at(".local/history/a step"));
        let conflict = root.at(".local/conflicts/layers/top/a.toml");
        for file in
            [&output, &kept, &conflict, &root.at(".local/processes/editor.sock"), &root.at(".local/settings.toml")]
        {
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, "").unwrap();
        }
        // No version, as outputs another wrote: they are wiped, and nothing else is.
        let local = super::Local::open(&project, &lock).unwrap();
        assert!(!output.exists());
        assert!(local.output(pe2_path::Name::constant("version")).exists());
        for file in [kept, conflict, root.at(".local/processes/editor.sock"), root.at(".local/settings.toml")] {
            assert!(file.exists(), "{}", file.display());
        }
        // Opened again by the same tools, nothing is wiped.
        std::fs::write(&output, "").unwrap();
        drop(super::Local::open(&project, &lock).unwrap());
        assert!(output.exists());
        drop(lock);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_setting_is_written_with_the_rest_kept() {
        #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        struct View {
            hidden: Vec<String>,
        }
        let root = crate::testing::scratch("settings");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let fs = &pe2_io::SystemFilesystem;
        let keys = &["editor", "layers"];
        assert_eq!(super::setting::<View>(fs, &root, keys), None, "none before the folder is made");
        std::fs::create_dir_all(root.at(".local")).unwrap();
        std::fs::write(root.at(".local/settings.toml"), "# mine\n[mounts]\nroms = \"/discs\"\n").unwrap();
        let view = View { hidden: vec!["game".into()] };
        super::set_setting(fs, &root, keys, &view).unwrap();
        assert_eq!(super::setting::<View>(fs, &root, keys).as_ref(), Some(&view));
        let text = std::fs::read_to_string(root.at(".local/settings.toml")).unwrap();
        assert!(text.starts_with("# mine\n[mounts]\nroms = \"/discs\"\n"), "{text}");
        // A key naming something else than a table is refused, and the file left as it was.
        assert!(super::set_setting(fs, &root, &["mounts", "roms", "x"], &view).is_err());
        assert_eq!(std::fs::read_to_string(root.at(".local/settings.toml")).unwrap(), text);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn writers_of_one_file_do_not_meet() {
        let dir = crate::testing::scratch("write-atomic");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.at("value");
        let writers: Vec<_> = (0..8)
            .map(|_| {
                let file = file.clone();
                std::thread::spawn(move || {
                    (0..200).try_for_each(|_| super::write_atomic(&pe2_io::SystemFilesystem, &file, b"held"))
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap().unwrap();
        }
        assert_eq!(std::fs::read(&file).unwrap(), b"held");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
