//! The editors running on this computer, each saying what it is doing: a file of its own in the user's data folder,
//! `editors/<pid>.json`, rewritten as its state changes, beside `<pid>.lock`, which its process holds while it lives.
//! What a lock no process holds stands beside is an editor gone - crashed, killed - and is removed by whoever reads
//! it. Read from the files, an editor is found whatever it is doing: opening a project, busy, minimized, with none open.
//!
//! A process also says whether its program was rebuilt since it started, by the program's time then and now: one that
//! was runs the code it started with, which may read and write a project as that code did.

#![allow(
    clippy::disallowed_methods,
    reason = "how the editors on this computer find each other: files every process reads as the system has them"
)]

use sfs::HostPath;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// What an editor says of itself.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Presence {
    pub pid: u32,
    /// Its program, and the program's modification time as it started (seconds since the epoch).
    pub program: Option<HostPath>,
    pub built: Option<u64>,
    /// When it started (seconds since the epoch).
    pub started: u64,
    /// The project it has open; the one it is opening, and how far.
    pub project: Option<HostPath>,
    pub opening: Option<HostPath>,
    pub stage: Option<String>,
    /// What it is busy with: taking in, an epoch, requests, ...
    pub busy: Vec<String>,
    /// What the project asks whoever opened it, unanswered: which extensions to trust.
    pub asking: bool,
    /// What went wrong, as it shows it.
    pub error: Option<String>,
    /// Whether agents may use its window (click, type, run commands).
    pub agents: bool,
    /// What it is fetching for the project's extensions, and how far; what failed.
    pub fetching: Option<String>,
    /// The last epoch: what it was signaled for, and how it ended.
    pub last_run: Option<String>,
    /// How many problems the project has.
    pub problems: usize,
    /// Its window: focused, in the background, minimized.
    pub window: String,
}

/// Where the editors keep their files.
#[allow(
    clippy::disallowed_methods,
    reason = "the editors' presence is kept in the program's own folder: plain paths, joined of names it writes"
)]
fn folder() -> Option<PathBuf> {
    Some(extensions::data_folder()?.join("editors"))
}

/// What a folder holds, as places: what the system lists, nothing where it lists nothing.
fn entries(folder: &Path) -> impl Iterator<Item = PathBuf> {
    std::fs::read_dir(folder).into_iter().flatten().flatten().map(|entry| entry.path())
}

/// Seconds since the epoch.
fn seconds(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// A program's modification time.
fn built(program: &HostPath) -> Option<u64> {
    std::fs::metadata(program).and_then(|m| m.modified()).ok().map(seconds)
}

/// Whether a program was rebuilt since a process of it started: its file is newer than it was then.
#[must_use]
pub fn rebuilt(program: Option<&HostPath>, then: Option<u64>) -> bool {
    then.zip(program.and_then(built)).is_some_and(|(then, now)| now != then)
}

/// This process's program and its time now: what a presence starts with, and the server says of itself.
#[must_use]
#[allow(
    clippy::disallowed_methods,
    reason = "this process's own program, as the system started it: the process's, no project's"
)]
pub fn this_program() -> (Option<HostPath>, Option<u64>) {
    let program = std::env::current_exe().ok().and_then(|program| HostPath::new(&program).ok());
    let built = program.as_ref().and_then(built);
    (program, built)
}

/// What keeps an editor's presence: its files, held while it lives, and what it last wrote.
pub struct Keeper {
    json: PathBuf,
    lock: PathBuf,
    _held: std::fs::File,
    base: Presence,
    written: Option<(Presence, Instant)>,
    /// What says when it was started, and how lately it wrote.
    io: std::sync::Arc<dyn pe2_io::Io>,
}

/// How often a presence that keeps changing is written at most.
const EVERY: Duration = Duration::from_millis(500);

impl Keeper {
    /// Begin this process's presence: its lock taken and held; none where there is no data folder or no lock.
    #[must_use]
    #[allow(
        clippy::disallowed_methods,
        reason = "the editors' presence is kept in the program's own folder: plain paths, joined of names it writes"
    )]
    pub fn start(io: std::sync::Arc<dyn pe2_io::Io>) -> Option<Keeper> {
        let folder = folder()?;
        std::fs::create_dir_all(&folder).ok()?;
        sweep(&folder);
        let pid = std::process::id();
        let lock = folder.join(format!("{pid}.lock"));
        let held = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&lock).ok()?;
        held.try_lock().ok()?;
        let (program, built) = this_program();
        let base = Presence { pid, program, built, started: seconds(io.clock().wall()), ..Presence::default() };
        let json = folder.join(format!("{pid}.json"));
        Some(Keeper { json, lock, _held: held, base, written: None, io })
    }

    /// Say this now, however lately it last said something: what another process must know before it acts - a
    /// project about to be opened.
    pub fn keep_now(&mut self, presence: Presence) {
        self.written = None;
        self.keep(presence);
    }

    /// Say this, where it changed: written now, or within `EVERY` of the last write where it keeps changing.
    pub fn keep(&mut self, mut presence: Presence) {
        presence.pid = self.base.pid;
        presence.program.clone_from(&self.base.program);
        presence.built = self.base.built;
        presence.started = self.base.started;
        if let Some((last, at)) = &self.written
            && (*last == presence || self.io.clock().now().saturating_duration_since(*at) < EVERY)
        {
            return;
        }
        let Ok(text) = serde_json::to_string_pretty(&presence) else { return };
        let partial = self.json.with_extension("json.part");
        if std::fs::write(&partial, text).is_ok() && std::fs::rename(&partial, &self.json).is_ok() {
            self.written = Some((presence, self.io.clock().now()));
        }
    }
}

impl Drop for Keeper {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.json);
        let _ = std::fs::remove_file(&self.lock);
    }
}

/// Remove what editors gone left: a presence whose lock nobody holds.
fn sweep(folder: &Path) {
    for path in entries(folder) {
        if path.extension().is_none_or(|e| e != "lock") {
            continue;
        }
        let Ok(file) = std::fs::OpenOptions::new().write(true).open(&path) else { continue };
        if file.try_lock().is_ok() {
            drop(file);
            let _ = std::fs::remove_file(path.with_extension("json"));
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// The editors running on this computer, each as it last said, by process.
#[must_use]
pub fn running() -> BTreeMap<u32, Presence> {
    let Some(folder) = folder() else { return BTreeMap::new() };
    sweep(&folder);
    let read = |path: PathBuf| serde_json::from_str::<Presence>(&std::fs::read_to_string(path).ok()?).ok();
    entries(&folder)
        .filter(|path| path.extension().is_some_and(|e| e == "json"))
        .filter_map(read)
        .map(|presence| (presence.pid, presence))
        .collect()
}

/// The editor that has the project in `root` open, or is opening it: its process.
#[must_use]
pub fn claimed_by(root: &HostPath) -> Option<u32> {
    running().into_values().find_map(|p| {
        let claims = [p.project.as_ref(), p.opening.as_ref()].into_iter().flatten();
        claims.into_iter().any(|path| path == root).then_some(p.pid)
    })
}

/// What `editors` answers: each editor running on this computer and its state, and of the project this server serves,
/// who holds it now; and this server, its program rebuilt since it started or not.
#[must_use]
pub fn report(root: &HostPath) -> serde_json::Value {
    let editors: Vec<serde_json::Value> = running()
        .into_values()
        .map(|p| {
            let mut value = serde_json::to_value(&p).unwrap_or_default();
            if let Some(table) = value.as_object_mut() {
                table.insert("rebuilt_since_started".into(), rebuilt(p.program.as_ref(), p.built).into());
                table.insert("this_project".into(), p.project.as_ref().is_some_and(|project| project == root).into());
            }
            value
        })
        .collect();
    let (program, _) = this_program();
    serde_json::json!({
        "editors": editors,
        "this_server": {
            "pid": std::process::id(),
            "program": program,
            "rebuilt_since_started": rebuilt(program.as_ref(), *STARTED_BUILT),
            "project": root.to_string(),
            "project_held_by": project::lock::holder(root),
        },
    })
}

/// This process's program's time as it started: what it is compared with, to know it was rebuilt since.
static STARTED_BUILT: std::sync::LazyLock<Option<u64>> = std::sync::LazyLock::new(|| this_program().1);

/// This process's program, and its time as this process started (`note_start`).
#[must_use]
pub fn started() -> (Option<HostPath>, Option<u64>) {
    (this_program().0, *STARTED_BUILT)
}

/// Note this process's program's time now, as it starts: asked first later, it would be the rebuilt program's.
pub fn note_start() {
    std::sync::LazyLock::force(&STARTED_BUILT);
}

#[cfg(test)]
#[allow(
    clippy::disallowed_methods,
    reason = "a test's fixtures: the machine's time, places, files and environment, which nothing tested reads"
)]
mod tests {
    use super::{Keeper, Presence, rebuilt, running, sweep};
    use sfs::testing::At as _;

    #[test]
    fn an_editor_is_found_while_it_lives_and_gone_with_it() {
        let data = sfs::testing::scratch("presence");
        let _ = std::fs::remove_dir_all(&data);
        extensions::use_data_folder(data.to_path_buf());
        // What a crashed editor left: a lock nobody holds.
        let folder = data.at("editors");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.at("1.lock"), "").unwrap();
        std::fs::write(folder.at("1.json"), "{}").unwrap();
        let mut keeper = Keeper::start(pe2_io::system()).expect("a presence");
        assert!(!folder.at("1.json").exists(), "what a gone editor left is swept");
        let project = data.at("a project");
        keeper.keep(Presence { project: Some(project.clone()), window: "focused".into(), ..Presence::default() });
        let found = running();
        let me = found.get(&std::process::id()).expect("found");
        assert_eq!((me.project.as_ref(), me.window.as_str()), (Some(&project), "focused"));
        assert!(!rebuilt(me.program.as_ref(), me.built), "its program is as it started");
        drop(keeper);
        sweep(&folder);
        assert!(running().is_empty(), "gone with it");
        let _ = std::fs::remove_dir_all(&data);
    }
}
