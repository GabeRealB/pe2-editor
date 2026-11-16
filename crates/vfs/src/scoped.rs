//! Scoped filesystems: the few files a program is given, served for it alone. A program reads files by path, and a
//! layer holds a large file as chunks (`project::chunks`); rather than assemble one into a copy, a scope serves it where
//! it is asked for, at `<project>/.local/processes/scope/<process>/<sha1>/<name>`, read-only, as long as the process runs.
//!
//! `Scopes::install` registers them with `project::content` (`content::expose_with`), so whatever hands a layer's file
//! to a program (`content::for_program`) gets a path in its project's scope; nothing is mounted until the first file
//! is. Where nothing serves one, `content::for_program` falls back on a copy in the project's outputs.
//!
//! What serves a scope is the backend best suited to it (`backend`): one serving at a folder, read by path - as the
//! project says (`[filesystem] scopes` of its `.local/settings.toml`), else the machine, else the best there is here.
//! What a backend leaves at the folder is the backend's to clear, when the scope is let go of and, where its process
//! ended without, by the next to sweep the project's scopes.
//!
//! A scope serves folders too (`content::serve_folders_with`): the files an operation hands a program, each where it is
//! or held in memory, under `<scope>/run-<n>/` for as long as the operation runs - nothing written for them.

use sfs::HostPath;
use std::collections::BTreeMap;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use pe2_path::{Name, RelPath};
use project::content::{Folder, Given, GivenPath, Use};

use crate::Served;
use crate::backend::{self, Backend, Can, Job, Mount};

/// A project's scope: the files it serves, and its mount once it has one.
struct Scope {
    at: HostPath,
    served: Served,
    files: BTreeMap<Arc<RelPath>, Given>,
    mounted: Option<Mounted>,
}

/// What this machine says of scopes where a project does not (its settings' `[filesystem]`): what serves them, and
/// how much laying files out instead may write (`copies`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Machine {
    pub scopes: Option<String>,
    pub copies: Option<String>,
}

impl Scope {
    /// Tell what serves it that what is now served under a path is about to be opened by it.
    fn placed(&self, path: &RelPath) {
        if let Some(Mounted { mount: Some(mount), .. }) = &self.mounted {
            mount.placed(&self.served, path);
        }
    }
}

/// The scopes this process serves, by project, and what the machine says serves them.
struct Held {
    scopes: BTreeMap<HostPath, Scope>,
    machine: Box<dyn Fn() -> Machine + Send>,
}

/// The scopes of the projects this process gives programs files of, by project: registered with `project::content`
/// while this is held, each unmounted when it is dropped.
pub struct Scopes {
    held: Arc<Mutex<Held>>,
    _exposing: project::content::Exposing,
    _serving: project::content::ServingFolders,
}

impl Scopes {
    /// Serve chunked contents to programs in scopes, from now until what this returns is dropped: by what each project
    /// says of its scopes, else what the machine does (`machine`, asked as something is served).
    #[must_use]
    pub fn install(machine: impl Fn() -> Machine + Send + 'static) -> Scopes {
        let scopes = Arc::new(Mutex::new(Held { scopes: BTreeMap::new(), machine: Box::new(machine) }));
        let held = Arc::clone(&scopes);
        let exposing = project::content::expose_with(None, move |io, disk| {
            expose(&mut held.lock().unwrap_or_else(std::sync::PoisonError::into_inner), io, disk)
        });
        let held = Arc::clone(&scopes);
        let serving =
            project::content::serve_folders_with(move |io, root, files, how| serve(&held, io, root, files, how));
        Scopes { held: scopes, _exposing: exposing, _serving: serving }
    }

    /// Where a project's scope is, for this process, mounted or not.
    ///
    /// # Panics
    /// Never: a process's number is a name.
    #[must_use]
    pub fn at(root: &HostPath) -> HostPath {
        const SCOPE: &pe2_path::Name = pe2_path::Name::constant("scope");
        let process = std::process::id().to_string();
        let process = pe2_path::Name::new(&process).expect("a number is a name");
        project::local::part(root, project::local::PROCESSES).join(&SCOPE).join(&process)
    }
}

impl Drop for Scopes {
    #[allow(
        clippy::disallowed_methods,
        reason = "a scope's mount point, its staging and the marker saying which process serves it: what the system mounts, and how the project's processes find each other, as the system has them"
    )]
    fn drop(&mut self) {
        let scopes = std::mem::take(&mut self.held.lock().unwrap_or_else(std::sync::PoisonError::into_inner).scopes);
        for (_, scope) in scopes {
            let at = scope.at.clone();
            drop(scope);
            let _ = std::fs::remove_dir(at);
        }
    }
}

/// The project whose store holds a layer's file: of those this process opened, the one whose layers it lies in.
fn project_of(disk: &HostPath) -> Option<HostPath> {
    project::holding(disk)
}

/// Serve a layer's file in its project's scope, mounting it first where it is not: its path there.
fn expose(held: &mut Held, io: &std::sync::Arc<dyn pe2_io::Io>, disk: &HostPath) -> io::Result<HostPath> {
    let root =
        project_of(disk).ok_or_else(|| io::Error::other(format!("{}: in no project's layers", disk.display())))?;
    let machine = (held.machine)();
    // One file of a layer given to a program: read in ranges as often as whole, and nothing else of the scope. A
    // small one is as well copied.
    let how = Use { ranges: true, often: false };
    let written = project::content::len(io, disk).unwrap_or(u64::MAX);
    let backend = chosen(io.filesystem(), &root, &machine, &how, written)?;
    let scope = scope_of(&mut held.scopes, &root);
    let sha1 = project::content::sha1(io, disk)?;
    let name = disk.file_name().map_or_else(|| "content".into(), |n| n.to_string_lossy().into_owned());
    let path = RelPath::new(&format!("{sha1}/{name}")).map_err(io::Error::other)?.to_owned();
    if !scope.files.contains_key(&*path) {
        scope.files.insert(path.clone(), Given::Disk(disk.clone()));
        scope.served.set_files(&scope.files, io);
    }
    mount(scope, &root, backend)?;
    scope.placed(&path);
    Ok(scope.at.join(&*path))
}

/// A project's scope, made where it has none yet (what processes that ended left of theirs swept first).
fn scope_of<'a>(scopes: &'a mut BTreeMap<HostPath, Scope>, root: &HostPath) -> &'a mut Scope {
    if !scopes.contains_key(root) {
        sweep(root);
    }
    scopes.entry(root.clone()).or_insert_with(|| Scope {
        at: Scopes::at(root),
        served: Served::default(),
        files: BTreeMap::new(),
        mounted: None,
    })
}

/// Serve a folder of these files in a project's scope, under a folder of its own: where it is, until what this returns
/// is dropped.
fn serve(
    held: &Arc<Mutex<Held>>,
    io: &std::sync::Arc<dyn pe2_io::Io>,
    root: Option<&HostPath>,
    files: &BTreeMap<Arc<GivenPath>, Given>,
    how: &Use,
) -> io::Result<Folder> {
    static RUN: AtomicU64 = AtomicU64::new(0);
    let root = root.ok_or_else(|| io::Error::other("no project to serve a folder in"))?.clone();
    let mut scopes = held.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let machine = (scopes.machine)();
    // Laid out instead where that suits better: nothing is served, and whoever asked lays them out.
    let backend = chosen(io.filesystem(), &root, &machine, how, project::content::written_to_lay_out(io, files))?;
    let run = Name::new(&format!("run-{}", RUN.fetch_add(1, Ordering::Relaxed))).map_err(io::Error::other)?.to_owned();
    let scope = scope_of(&mut scopes.scopes, &root);
    for (name, given) in files {
        scope.files.insert(run.as_rel().join_path(name.rel()), given.clone());
    }
    scope.served.set_files(&scope.files, io);
    let unserve = Unserve { held: Arc::clone(held), root: root.clone(), run: run.clone(), io: io.clone() };
    if let Err(e) = mount(scope, &root, backend) {
        drop(scopes);
        drop(unserve);
        return Err(e);
    }
    scope.placed(run.as_rel());
    let at = scope.at.join(&*run);
    Ok(Folder::new(at, Box::new(unserve)))
}

/// How much laying files out for a program may write, for a project: as it says, else the machine, else the default.
fn copies(fs: &dyn pe2_io::Filesystem, root: &HostPath, machine: &Machine) -> u64 {
    let says = project::local_filesystem(root, fs).copies;
    backend::budget(&[says.as_deref(), machine.copies.as_deref()], backend::COPIES)
}

/// The backend that serves something in a project's scope: as the project says serves its scopes, else the machine,
/// else the best suited to how it will be used - or none, where laying it out would write little enough (`written`)
/// that copies suit better.
///
/// # Errors
/// Copies suit better, or the backend named cannot serve it: laid out either way, by whoever asked.
fn chosen(
    fs: &dyn pe2_io::Filesystem,
    root: &HostPath,
    machine: &Machine,
    how: &Use,
    written: u64,
) -> io::Result<&'static dyn Backend> {
    let says = project::local_filesystem(root, fs).scopes;
    let setting = backend::setting(&[says.as_deref(), machine.scopes.as_deref()]);
    let light = written <= copies(fs, root, machine);
    backend::choose(&setting, Can::BY_PATH, &Scopes::at(root), how, light)
        .map_err(|why| io::Error::other(format!("a project's scopes: {why}")))?
        .ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, "copies suit this better than a scope"))
}

/// A folder served in a scope: taken out of it, and its inode numbers let go of, once this is dropped.
struct Unserve {
    held: Arc<Mutex<Held>>,
    root: HostPath,
    run: Arc<Name>,
    /// The I/O what is served of it is read through.
    io: std::sync::Arc<dyn pe2_io::Io>,
}

impl Drop for Unserve {
    fn drop(&mut self) {
        let mut held = self.held.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(scope) = held.scopes.get_mut(&self.root) else { return };
        // Told first, while what it lists is still in the tree.
        let run = self.run.as_rel();
        if let Some(Mounted { mount: Some(mount), .. }) = &scope.mounted {
            mount.released(run);
        }
        scope.files.retain(|path, _| !path.starts_with(run));
        scope.served.set_files(&scope.files, &self.io);
        scope.served.forget(run);
    }
}

/// Remove the scopes processes that have ended left in a project's cache: each cleared by the backend that served it,
/// or by every one that could have where nothing says which.
#[allow(
    clippy::disallowed_methods,
    reason = "a scope's mount point, its staging and the marker saying which process serves it: what the system mounts, and how the project's processes find each other, as the system has them"
)]
fn sweep(root: &HostPath) {
    let Some(dir) = Scopes::at(root).parent() else { return };
    let mine = std::process::id().to_string();
    for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let pid = entry.file_name().to_string_lossy().into_owned();
        // A scope is a process's number; beside it, what tells it runs and its staging.
        if pid == mine || !pid.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let Left::By(served_by) = Marker::left(&dir, &pid) else { continue };
        let name = scope_name(root, &pid);
        for backend in backend::all() {
            if backend.can().folder && served_by.as_deref().is_none_or(|by| by == backend.name()) {
                backend.clear(&dir.entry(&entry.file_name()), &name);
            }
        }
        let _ = std::fs::remove_dir_all(dir.entry(std::ffi::OsStr::new(&format!("{pid}.staging"))));
        let _ = std::fs::remove_file(Marker::path(&dir, &pid));
        let _ = std::fs::remove_dir(dir.entry(&entry.file_name()));
    }
}

/// What a scope's process left: nothing to sweep while it runs, else which backend served it, where that is told.
enum Left {
    Running,
    By(Option<String>),
}

/// What tells others a scope's process runs, and which backend serves it: `<pid>.alive` beside it, naming the backend.
/// On Windows the process holds it open, shared with nothing, while it serves - so that nothing can read it until it
/// lets go of it, or ends; elsewhere the system says which processes run.
struct Marker {
    path: HostPath,
    held: Option<std::fs::File>,
}

impl Marker {
    fn path(dir: &HostPath, pid: &str) -> HostPath {
        dir.entry(std::ffi::OsStr::new(&format!("{pid}.alive")))
    }

    /// Say this process serves its scope in `dir`, by a backend, until dropped.
    #[allow(
        clippy::disallowed_methods,
        reason = "a scope's mount point, its staging and the marker saying which process serves it: what the system mounts, and how the project's processes find each other, as the system has them"
    )]
    fn hold(dir: &HostPath, by: &str) -> io::Result<Marker> {
        use std::io::Write;
        let path = Marker::path(dir, &std::process::id().to_string());
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(windows)]
        std::os::windows::fs::OpenOptionsExt::share_mode(&mut options, 0);
        let mut file = options.open(&path)?;
        file.write_all(by.as_bytes())?;
        file.flush()?;
        Ok(Marker { path, held: cfg!(windows).then_some(file) })
    }

    /// What the process a scope is named after left; where whether it runs cannot be told, it is taken to.
    #[allow(
        clippy::disallowed_methods,
        reason = "a scope's mount point, its staging and the marker saying which process serves it: what the system mounts, and how the project's processes find each other, as the system has them"
    )]
    fn left(dir: &HostPath, pid: &str) -> Left {
        let said = std::fs::read_to_string(Marker::path(dir, pid));
        let by = |said: io::Result<String>| said.ok().map(|by| by.trim().to_string()).filter(|by| !by.is_empty());
        if cfg!(windows) {
            return match said {
                Err(e) if e.kind() != io::ErrorKind::NotFound => Left::Running,
                said => Left::By(by(said)),
            };
        }
        let process =
            pe2_path::Name::new(pid).ok().and_then(|pid| Some(HostPath::new(Path::new("/proc")).ok()?.join(&pid)));
        if cfg!(target_os = "linux") && !process.is_some_and(|process| process.exists()) {
            return Left::By(by(said));
        }
        Left::Running
    }
}

impl Drop for Marker {
    #[allow(
        clippy::disallowed_methods,
        reason = "a scope's mount point, its staging and the marker saying which process serves it: what the system mounts, and how the project's processes find each other, as the system has them"
    )]
    fn drop(&mut self) {
        drop(self.held.take());
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The name a scope is served by, where the system shows or registers one: its own, by the project and the process.
fn scope_name(root: &HostPath, pid: &str) -> String {
    let hash = project::extract::sha1_hex(root.to_string().as_bytes());
    format!("scope-{pid}-{}", &hash[..12])
}

/// A scope served: let go of, what its backend left cleared, and its staging and marker removed, once dropped.
struct Mounted {
    mount: Option<Box<dyn Mount>>,
    backend: &'static dyn Backend,
    at: HostPath,
    name: String,
    staging: HostPath,
    _alive: Marker,
}

impl Drop for Mounted {
    #[allow(
        clippy::disallowed_methods,
        reason = "a scope's mount point, its staging and the marker saying which process serves it: what the system mounts, and how the project's processes find each other, as the system has them"
    )]
    fn drop(&mut self) {
        drop(self.mount.take());
        self.backend.clear(&self.at, &self.name);
        let _ = std::fs::remove_dir_all(&self.staging);
    }
}

/// A scope's mount, made where it has none, by a backend; one it has already serves on, whichever it is by.
#[allow(
    clippy::disallowed_methods,
    reason = "a scope's mount point, its staging and the marker saying which process serves it: what the system mounts, and how the project's processes find each other, as the system has them"
)]
fn mount(scope: &mut Scope, root: &HostPath, backend: &'static dyn Backend) -> io::Result<()> {
    if scope.mounted.is_none() {
        let pid = std::process::id().to_string();
        let dir = scope.at.parent().unwrap_or_else(|| scope.at.clone());
        std::fs::create_dir_all(&dir)?;
        let alive = Marker::hold(&dir, backend.name())?;
        let (name, staging) = (scope_name(root, &pid), dir.entry(std::ffi::OsStr::new(&format!("{pid}.staging"))));
        // Held from before it serves: what serving leaves, should it fail, is cleared as it is dropped.
        let mut mounted =
            Mounted { mount: None, backend, at: scope.at.clone(), name: name.clone(), staging, _alive: alive };
        let job = Job {
            served: &scope.served,
            owner: None,
            at: &scope.at,
            staging: &mounted.staging,
            name: &name,
            address: None,
            // Read-only, it applies nothing: no time of its decides anything.
            io: pe2_io::system(),
        };
        mounted.mount = Some(backend.mount(&job)?);
        scope.mounted = Some(mounted);
    }
    Ok(())
}

#[cfg(all(test, any(target_os = "linux", windows)))]
#[allow(
    clippy::disallowed_methods,
    reason = "a test's fixtures: the machine's time, places and environment, which nothing tested reads"
)]
mod tests {
    use super::Scopes;
    use sfs::HostPath;
    use sfs::testing::At as _;

    /// Scopes served by the backend `PE2_TEST_SCOPES` names (how the tests are run through each backend of a system),
    /// else by the first of this system's that serves at a folder here: named, as what is served here is small
    /// enough that it would be laid out otherwise.
    fn scopes() -> Scopes {
        Scopes::install(|| {
            let here = HostPath::new(&std::env::temp_dir()).unwrap();
            let first = || crate::backend::all().iter().find(|b| b.can().folder && b.available(&here).is_ok());
            let named = std::env::var("PE2_TEST_SCOPES").ok().or_else(|| first().map(|b| b.name().to_string()));
            super::Machine { scopes: named, copies: None }
        })
    }

    /// Few of the files read, once.
    fn once() -> project::content::Use {
        project::content::Use::once()
    }

    #[test]
    fn a_chunked_file_is_given_to_programs_in_its_scope() {
        let root = sfs::testing::scratch("scope");
        let _ = std::fs::remove_dir_all(&root);
        // A file of a project's layers, its content in that project's store.
        project::create(&root, &pe2_io::SystemFilesystem).expect("made");
        project::Project::load(&root).expect("opened");
        let file = root.at("layers/top/a.node/movie.str");
        let bytes: Vec<u8> = (0..(3u32 << 20)).map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8).collect();
        project::content::write(&pe2_io::system(), &file, &bytes).expect("written");
        let scopes = scopes();
        let given = project::content::for_program(&pe2_io::system(), &file).expect("given");
        // A backend here: FUSE where the kernel has it, the Projected File System or the Cloud Files API on Windows.
        let fuse = cfg!(windows) || std::path::Path::new("/dev/fuse").exists();
        assert_eq!(given.starts_with(&Scopes::at(&root)), fuse, "{}", given.display());
        if fuse {
            // As a program reads it: a file, by path, its name kept.
            assert_eq!(given.file_name().unwrap(), "movie.str");
            assert_eq!(std::fs::read(&given).expect("read through the scope"), bytes);
            assert!(!root.at(".local/outputs/materialized").exists(), "nothing copied");
        } else {
            // No FUSE here: a copy in the outputs.
            assert!(given.starts_with(&root.at(".local/outputs/materialized")));
        }
        drop(scopes);
        assert!(!Scopes::at(&root).exists(), "unmounted and gone");
        std::fs::remove_dir_all(&root).unwrap();
    }
    #[test]
    fn what_a_scope_serves_no_program_changes() {
        use project::content::Given;
        let root = sfs::testing::scratch("scope-read-only");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let files = std::collections::BTreeMap::from([
            (
                project::content::GivenPath::new("kept.h").unwrap().to_owned(),
                Given::Bytes(std::sync::Arc::from(&b"#define A 1"[..])),
            ),
            (
                project::content::GivenPath::new("sub/other.h").unwrap().to_owned(),
                Given::Bytes(std::sync::Arc::from(&b"#define B 2"[..])),
            ),
        ]);
        let scopes = scopes();
        let folder =
            project::content::serve_folder(&pe2_io::system(), Some(&root), &files, &once(), &root.at("fallback"))
                .expect("served");
        if folder.at.starts_with(&Scopes::at(&root)) {
            // As another program would: this process's own removals are how a scope lets go of what it served.
            let run = |command: String| {
                let (shell, flag) = if cfg!(windows) { ("cmd", "/C") } else { ("sh", "-c") };
                let _ = std::process::Command::new(shell).arg(flag).arg(&command).current_dir(&folder.at).output();
            };
            if cfg!(windows) {
                run("echo changed> kept.h".into());
                run("ren kept.h moved.h".into());
                run("del /q kept.h".into());
                run("ren sub moved".into());
                run("rmdir /s /q sub".into());
            } else {
                run("echo changed > kept.h".into());
                run("mv kept.h moved.h".into());
                run("rm -f kept.h".into());
                run("mv sub moved".into());
                run("rm -rf sub".into());
            }
            assert_eq!(std::fs::read(folder.at.at("kept.h")).unwrap(), b"#define A 1", "not written or removed");
            assert!(!folder.at.at("moved.h").exists(), "not renamed");
            assert_eq!(std::fs::read(folder.at.at("sub/other.h")).unwrap(), b"#define B 2", "its folder kept");
            assert!(!folder.at.at("moved").exists(), "its folder not renamed");
        }
        drop(folder);
        drop(scopes);
        assert!(!Scopes::at(&root).exists(), "let go of, and gone");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_folder_is_served_to_programs_where_its_files_are_until_it_is_let_go_of() {
        use project::content::Given;
        let root = sfs::testing::scratch("scope-folder");
        let _ = std::fs::remove_dir_all(&root);
        let chunked = root.at("layers/top/a.node/big.bin");
        let big: Vec<u8> = (0..(2u32 << 20)).map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8).collect();
        project::content::write(&pe2_io::system(), &chunked, &big).expect("written");
        let plain = root.at("plain.h");
        std::fs::write(&plain, "#define A 1\n").unwrap();
        let files = std::collections::BTreeMap::from([
            (
                project::content::GivenPath::new("units").unwrap().to_owned(),
                Given::Bytes(std::sync::Arc::from(&b"self/a.c"[..])),
            ),
            (project::content::GivenPath::new("self/include/a.h").unwrap().to_owned(), Given::Disk(plain.clone())),
            (project::content::GivenPath::new("dep/big.bin").unwrap().to_owned(), Given::Disk(chunked)),
        ]);
        let scopes = scopes();
        let fallback = root.at("fallback");
        let folder =
            project::content::serve_folder(&pe2_io::system(), Some(&root), &files, &once(), &fallback).expect("served");
        // A backend here: FUSE where the kernel has it, the Projected File System or the Cloud Files API on Windows.
        let fuse = cfg!(windows) || std::path::Path::new("/dev/fuse").exists();
        assert_eq!(folder.at.starts_with(&Scopes::at(&root)), fuse, "{}", folder.at.display());
        assert_eq!(std::fs::read(folder.at.at("units")).unwrap(), b"self/a.c");
        assert_eq!(std::fs::read_to_string(folder.at.at("self/include/a.h")).unwrap(), "#define A 1\n");
        assert_eq!(std::fs::read(folder.at.at("dep/big.bin")).unwrap(), big);
        let listed: Vec<String> = std::fs::read_dir(folder.at.at("self"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(listed, ["include"]);
        if fuse {
            assert!(!fallback.exists(), "nothing written");
        }
        let at = folder.at.clone();
        drop(folder);
        // As the kernel is told: what it looked up it holds a moment.
        let gone = (0..40).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(50));
            !at.exists()
        });
        assert!(gone, "let go of");
        drop(scopes);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
