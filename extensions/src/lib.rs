//! The extensions the editor ships: part of its binaries, loaded for every project, versioned with them. Each is a
//! crate beside this one, built as a component by `build.rs` and embedded here; a project names those it needs, and
//! never carries a copy.
//!
//! The same crates are linked in too: `PE2_EXTENSIONS=linked` runs them in the host instead of their components, to
//! step through one in a debugger, profile it, or compare the two; what a project holds still runs as components.

use project::ext::component::{Described, Unsandboxed};
use project::ext::{
    EXTENSION_COMPONENT, Extensions, Loadable, Origin, Present, Status, component, folder, unsandboxed,
};
use project::{Project, Requirement};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use toml::{Table, Value};

pub mod archive;
pub mod trust;

include!(concat!(env!("OUT_DIR"), "/components.rs"));

/// The built-ins' components, by package: what loading reads.
#[must_use]
pub fn components() -> &'static [(&'static str, &'static [u8])] {
    COMPONENTS
}

/// A component built here and not embedded, by its file: what a test needs of one that has to be trusted.
#[must_use]
pub fn built_apart(file: &str) -> Option<Vec<u8>> {
    std::fs::read(Path::new(env!("PE2_BUILT")).join(file)).ok()
}

/// The tests of the built-ins, each a `tests/` folder, as the build lays them out.
#[must_use]
pub fn built_suites() -> Vec<PathBuf> {
    let entries = std::fs::read_dir(env!("PE2_SUITES")).into_iter().flatten().flatten();
    let mut suites: Vec<PathBuf> =
        entries.map(|entry| entry.path().join(unsandboxed::TESTS)).filter(|tests| tests.is_dir()).collect();
    suites.sort();
    suites
}

/// The built-in extensions linked in.
#[must_use]
pub fn linked() -> Vec<pe2_ext::Extension> {
    vec![
        pe2_render::extension(),
        pe2_events::extension(),
        pe2_graph::extension(),
        pe2_flags::extension(),
        pe2_dialogue::extension(),
        pe2_fonts::extension(),
        pe2_formats::extension(),
        pe2_game::extension(),
        pe2_images::extension(),
        pe2_movies::extension(),
        pe2_packages::extension(),
        pe2_str::extension(),
        pe2_sounds::extension(),
    ]
}

/// Load the built-ins: their components, or with `PE2_EXTENSIONS=linked` their crates.
#[must_use]
pub fn load() -> Extensions {
    load_without(&[], &[], &Kept::new(), None)
}

/// The settings kept for extensions, by id: each a table by scope (`{ project = { ... }, user = { ... } }`).
pub type Kept = BTreeMap<String, Table>;

/// An extension found beside the built-ins: one the project holds, one kept to be added to it, or tests.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Found {
    pub origin: Origin,
    /// Its component.
    pub file: PathBuf,
    /// The folder it is in with what it ships, where it is in one of its own.
    pub folder: Option<PathBuf>,
    /// What tells this version of it from another: its files' sizes and times - and what the user enabled, as that
    /// changes what it runs as.
    pub stamp: String,
}

/// Load the extensions the project holds and the built-ins, but those it leaves off (`disabled`, by id) and those
/// requiring them; what is kept to be added (yours, beside the editor) is shown, and never run. `requires`, what the
/// project requires (`[[extension]]`), where there is a project.
#[must_use]
pub fn load_without(found: &[Found], disabled: &[String], kept: &Kept, requires: Option<&[Requirement]>) -> Extensions {
    let with_settings = |mut loadable: Vec<Loadable>| {
        for l in &mut loadable {
            l.settings = kept.get(&l.id).cloned().unwrap_or_default();
        }
        loadable
    };
    let mut loadable: Vec<Loadable> = vec![];
    let mut failed = vec![];
    // Those not run: refused, not trusted, kept apart.
    let mut unloadable = vec![];
    let enabled = enabled_unsandboxed();
    for found in found {
        // What is kept to be added runs only once a project holds it.
        let kept_apart = matches!(found.origin, Origin::User(_) | Origin::Install(_));
        // An archive of one is read without unpacking it.
        if kept_apart && archive::is_archive(&found.file) {
            unloadable.push(match archive::read(&found.file) {
                Ok((about, component)) => shown(about, found, &component, Status::Available, None),
                Err(e) => unreadable(found, format!("{e:#}")),
            });
            continue;
        }
        let loaded =
            project::content::read(&found.file).map_err(|e| format!("it cannot be read: {e}")).and_then(|bytes| {
                if !bytes.starts_with(b"\0asm") {
                    return Err("it is not a WebAssembly component".to_string());
                }
                // In a folder of its own, its manifest is its component's: what says whether it needs trusting.
                let about = match found.origin.folder() {
                    Some(held) => Some(folder::checked(held, &bytes).map_err(|e| format!("{e:#}"))?),
                    None => component::about(&bytes),
                };
                if let Some(why) = about.as_ref().and_then(|a| unsandboxed::elsewhere(&a.platforms)) {
                    return Err(why);
                }
                if let Some(about) = about.as_ref().filter(|_| kept_apart) {
                    unloadable.push(shown(about.clone(), found, &bytes, Status::Available, None));
                    return Ok(None);
                }
                let name = found.origin.said();
                // Unsandboxed, it runs only as the user trusts it; until then, it is read for who it is.
                let held = match about.filter(|a| a.unsandboxed) {
                    Some(about) => {
                        let trust = unsandboxed::trust(&found.file, found.folder.as_deref(), &about)
                            .map_err(|e| format!("{e:#}"))?;
                        if !enabled.contains_key(&trust) && trust::allowed_by(&found.origin).is_none() {
                            let changed = enabled.values().any(|id| *id == about.id);
                            unloadable.push(shown(about, found, &bytes, Status::NotEnabled { changed }, Some(trust)));
                            return Ok(None);
                        }
                        let install = about.described.install.as_ref();
                        let install = install.and_then(|page| Some(found.folder.as_ref()?.join(page)));
                        // What its setup fetches here, where it is kept, and what of it the user turned off.
                        let setup = about.setup.iter().filter(|s| s.here()).cloned().collect();
                        let launch = about.launch.iter().filter(|l| l.here()).cloned().collect();
                        let (store, off) = (setup_folder(), trust::optional_off());
                        Some(Unsandboxed { folder: found.folder.clone(), trust, install, store, setup, off, launch })
                    }
                    None => None,
                };
                // The first line of what went wrong: the parser's is followed by the bytes it found.
                let loaded = match held {
                    Some(held) => component::load_unsandboxed(&name, &bytes, found.origin.clone(), held),
                    None => component::load_bytes(&name, &bytes, found.origin.clone()),
                };
                let loaded = loaded.map_err(|e| {
                    let why = format!("{e:#}");
                    why.lines().next().unwrap_or_default().trim_end_matches(['[', ' ', '=']).to_string()
                });
                loaded.map(Some)
            });
        match loaded {
            Ok(Some(extension)) => loadable.push(extension),
            Ok(None) => {}
            Err(e) => {
                // What is kept apart is no trouble of the project's.
                if !kept_apart {
                    failed.push(format!("{}: {e}", found.origin.said()));
                }
                unloadable.push(unreadable(found, e));
            }
        }
    }
    // The built-ins: their crates, linked in, where asked (a native profiler sees into them); what the project holds
    // runs as ever.
    if std::env::var_os("PE2_EXTENSIONS").is_some_and(|v| v == "linked") {
        loadable.extend(linked().into_iter().map(Into::into));
    } else {
        for (package, bytes) in COMPONENTS {
            match component::load_bytes(package, bytes, Origin::Builtin) {
                Ok(extension) => loadable.push(extension),
                Err(e) => failed.push(format!("{package}: {e:#}")),
            }
        }
    }
    loadable.extend(TEST_HOST.get().map(|host| host().into()));
    let mut extensions = Extensions::load_without(with_settings(loadable), true, disabled);
    extensions.problems.extend(failed);
    extensions.present.extend(unloadable);
    if let Some(requires) = requires {
        extensions.unmet = unmet(requires, &extensions.present);
    }
    extensions
}

/// Where what extensions' setup fetches is kept on this machine: `setup` in the editor's data folder.
#[must_use]
pub fn setup_folder() -> Option<PathBuf> {
    Some(data_folder()?.join("setup"))
}

/// What an extension's setup has yet to fetch onto this machine, for this platform, optional or not.
#[must_use]
pub fn pending(present: &Present) -> Vec<&project::ext::setup::Setup> {
    let Some(store) = setup_folder() else { return vec![] };
    present.setup.iter().filter(|setup| setup.here() && !setup.fetched(&store)).collect()
}

/// What an extension requires and has yet to fetch onto this machine: what trusting it fetches unasked, once it runs.
#[must_use]
pub fn required(present: &Present) -> Vec<&project::ext::setup::Setup> {
    let runs = present.status == Status::Loaded && matches!(present.origin, Origin::Project(_));
    if !runs {
        return vec![];
    }
    pending(present).into_iter().filter(|setup| !setup.lazy).collect()
}

/// What the extensions present name of a store, by key: every platform's entries, fetched or not - what a sweep of
/// the store keeps, however long ago it was used.
#[must_use]
pub fn named_in_store(extensions: &Extensions) -> std::collections::HashSet<String> {
    extensions.present.iter().flat_map(|present| &present.setup).map(project::ext::setup::Setup::key).collect()
}

/// Sweep the store of what extensions fetch where a sweep is due (`setup::kept::sweep_if_due`), keeping what the
/// extensions present name: what was removed, and what could not be, with why; None where none was due.
#[must_use]
pub fn sweep_store_if_due(extensions: &Extensions) -> Option<project::ext::setup::kept::Swept> {
    project::ext::setup::kept::sweep_if_due(&setup_folder()?, &named_in_store(extensions))
}

/// Whether the user lets an extension fetch something optional on this computer.
#[must_use]
pub fn optional_on(setup: &project::ext::setup::Setup) -> bool {
    !trust::optional_off().contains(&setup.key())
}

/// How a fetch is told as it goes: the extension, the entry, bytes done, of how many.
pub type Said<'a> = &'a mut dyn FnMut(&str, &project::ext::setup::Setup, u64, Option<u64>);

/// Fetch what the extensions running require and have not fetched yet - trusting them is consenting to it - each told
/// to `said` as it goes (the extension, the entry, bytes done, of how many): what failed, each with why.
pub fn fetch_required(extensions: &Extensions, said: Said) -> Vec<(String, String)> {
    let Some(store) = setup_folder() else { return vec![] };
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let mut failed = vec![];
    for present in &extensions.present {
        for setup in required(present) {
            let mut progress = |done, total| said(&present.id, setup, done, total);
            let folder = present.origin.folder();
            if let Err(e) = project::ext::setup::fetch(setup, &store, folder, &mut progress, &cancel) {
                failed.push((format!("{}: {}", present.id, setup.name), format!("{e:#}")));
            }
        }
    }
    failed
}

/// Test mode, for this process: this extension, built into the host, is there for tests to act through, and the tests of
/// the extensions found load with them.
pub fn test_mode(host: fn() -> pe2_ext::Extension) {
    let _ = TEST_HOST.set(host);
    component::be_patient();
}

static TEST_HOST: OnceLock<fn() -> pe2_ext::Extension> = OnceLock::new();

/// An extension that cannot be loaded at all, as it is shown: named by where it is - the id its folder is named for,
/// the file's name - as who it says it is may not be readable, and is no claim on an id.
fn unreadable(found: &Found, why: String) -> Present {
    let name = match found.origin.folder().and_then(folder::id_of) {
        Some(id) => id.to_string(),
        None => found.file.file_name().map_or_default(|n| n.to_string_lossy().into_owned()),
    };
    Present {
        id: name,
        version: String::new(),
        origin: found.origin.clone(),
        requires: vec![],
        weak: vec![],
        optional: vec![],
        provides: vec![],
        linked: vec![],
        status: Status::Refused(why),
        size: std::fs::metadata(&found.file).ok().and_then(|m| usize::try_from(m.len()).ok()),
        contract: None,
        load: None,
        registers: vec![],
        trust: None,
        described: Described::default(),
        setup: vec![],
    }
}

/// What a project requires and does not have, and why.
fn unmet(requires: &[Requirement], present: &[Present]) -> Vec<(String, String)> {
    let why = |required: &Requirement| {
        let found = present
            .iter()
            .find(|p| p.id == required.id && !matches!(p.status, Status::Replaced(_) | Status::Available));
        let Some(found) = found else {
            let kept = present.iter().any(|p| p.id == required.id && p.status == Status::Available);
            return Some(if kept { "it is not added to the project" } else { "it was not found" }.to_string());
        };
        match &found.status {
            Status::Loaded => match &required.version {
                Some(version) if !project::ext::satisfies(&found.version, version) => {
                    Some(format!("it is {}, and it asks for {version}", found.version))
                }
                _ => None,
            },
            Status::Disabled => Some("the project leaves it off".to_string()),
            Status::NotEnabled { .. } => Some("it runs outside the sandbox and is not trusted".to_string()),
            Status::Refused(why) => Some(why.clone()),
            Status::Replaced(_) | Status::Available => None,
        }
    };
    requires.iter().filter_map(|required| Some((required.id.clone(), why(required)?))).collect()
}

/// An extension not run, as it is shown: who its manifest says it is, read without running anything.
fn shown(about: component::About, found: &Found, bytes: &[u8], status: Status, trust: Option<String>) -> Present {
    Present {
        status,
        id: about.id,
        version: about.version,
        origin: found.origin.clone(),
        requires: about.requires,
        weak: about.weak,
        optional: about.optional,
        provides: about.provides,
        linked: vec![],
        size: Some(bytes.len()),
        contract: component::contract(bytes),
        load: None,
        registers: vec![],
        trust,
        described: about.described,
        setup: about.setup,
    }
}

/// The unsandboxed extensions the user enabled: each one's hash, with its id. They live with the user, never in a
/// project.
#[must_use]
pub fn enabled_unsandboxed() -> BTreeMap<String, String> {
    let table = trust::read();
    let Some(Value::Table(enabled)) = table.get("enabled") else { return BTreeMap::new() };
    enabled.iter().filter_map(|(trust, id)| Some((trust.clone(), id.as_str()?.to_string()))).collect()
}

/// Enable an unsandboxed extension, by its hash, in place of what was enabled of its id; or not.
///
/// # Errors
/// What is enabled could not be written.
pub fn set_unsandboxed_enabled(trust: &str, id: &str, on: bool) -> std::io::Result<()> {
    let mut enabled = enabled_unsandboxed();
    enabled.retain(|_, of| of != id);
    if on {
        enabled.insert(trust.to_string(), id.to_string());
    }
    let mut table = trust::read();
    table.insert("enabled".into(), Value::Table(enabled.into_iter().map(|(t, id)| (t, Value::String(id))).collect()));
    trust::write(&table)
}

/// The built-ins, loaded once a process.
#[must_use]
pub fn shared() -> Arc<Extensions> {
    loaded(&[], &[], &Kept::new(), None)
}

/// The extensions as a project has them: those it holds and the built-ins, but those it leaves off, with what is kept
/// to be added shown - loaded once a process for each such set, so a changed extension loads again.
#[must_use]
pub fn for_project(project: &Project) -> Arc<Extensions> {
    loaded(&found(project), &project.disabled_extensions, &kept(project), Some(&project.requires))
}

/// The settings kept for a project's extensions: its own (`[extensions."<id>"]` in `project.toml`) and the user's (the
/// same in the editor's `settings.toml`).
#[must_use]
pub fn kept(project: &Project) -> Kept {
    let mut kept = Kept::new();
    let scopes = [("project", project.extension_settings.clone()), ("user", user_settings())];
    for (scope, by_id) in scopes {
        for (id, table) in by_id {
            let by_scope = kept.entry(id).or_default();
            by_scope.insert(scope.to_string(), Value::Table(table));
        }
    }
    kept
}

/// What this machine says serves projects as filesystems where a project does not: `[filesystem]` in the editor's
/// `settings.toml` - `serve`, what serves a project's folder, and `scopes`, what serves the files programs are given,
/// each `auto`, `none` or a backend's name; and the budgets `copies` and `kept`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FilesystemDefaults {
    pub serve: Option<String>,
    pub scopes: Option<String>,
    /// How much laying files out for a program may write, and how much may be kept of what was read of a project's
    /// folder: budgets of bytes (`16 MiB`).
    pub copies: Option<String>,
    pub kept: Option<String>,
}

/// What this machine says serves projects (`FilesystemDefaults`); nothing where the settings say nothing, or cannot be
/// read.
#[must_use]
pub fn filesystem_defaults() -> FilesystemDefaults {
    let read = || {
        let text = std::fs::read_to_string(data_folder()?.join("settings.toml")).ok()?;
        let Some(Value::Table(filesystem)) = toml::from_str::<Table>(&text).ok()?.remove("filesystem") else {
            return None;
        };
        let said = |key: &str| filesystem.get(key).and_then(Value::as_str).map(str::to_string);
        Some(FilesystemDefaults {
            serve: said("serve"),
            scopes: said("scopes"),
            copies: said("copies"),
            kept: said("kept"),
        })
    };
    read().unwrap_or_default()
}

/// The user's settings of extensions: `[extensions."<id>"]` in the editor's `settings.toml`, beside its other ones.
#[must_use]
pub fn user_settings() -> BTreeMap<String, Table> {
    let Some(file) = data_folder().map(|d| d.join("settings.toml")) else { return BTreeMap::new() };
    let Ok(text) = std::fs::read_to_string(file) else { return BTreeMap::new() };
    let Ok(mut settings) = toml::from_str::<Table>(&text) else { return BTreeMap::new() };
    let Some(Value::Table(extensions)) = settings.remove("extensions") else { return BTreeMap::new() };
    extensions
        .into_iter()
        .filter_map(|(id, value)| match value {
            Value::Table(table) => Some((id, table)),
            _ => None,
        })
        .collect()
}

/// The tests in an extension's folder.
fn tests_of(folder: &Path) -> Option<Found> {
    let file = folder.join(unsandboxed::TESTS).join("tests.wasm");
    project::pack::is_file(&file).then(|| Found {
        origin: Origin::Component(file.clone()),
        stamp: file_stamp(&file),
        folder: file.parent().map(PathBuf::from),
        file,
    })
}

/// The extensions a project holds (`Project::held_extensions`).
#[must_use]
pub fn held_components(project: &Project) -> Vec<Found> {
    project.held_extensions().into_iter().filter_map(|held| component_at(held, Origin::Project)).collect()
}

/// The extensions found beside the built-ins: those the project holds, those kept to be added (yours, beside the
/// editor). Two findings differ where one came, went or changed, or what the user enabled did: the extensions are
/// loaded again.
#[must_use]
pub fn found(project: &Project) -> Vec<Found> {
    let mut found = held_components(project);
    found.extend(user_components());
    found.extend(install_components());
    // In test mode, what each held carries of tests.
    if TEST_HOST.get().is_some() {
        let held = found.iter().filter(|found| matches!(found.origin, Origin::Project(_)));
        let tests: Vec<Found> = held.filter_map(|found| tests_of(found.folder.as_deref()?)).collect();
        found.extend(tests);
    }
    // What the user enabled - and turned off of what is fetched - changes what each runs as.
    let enabled = trust::file().and_then(|f| std::fs::read_to_string(f).ok()).unwrap_or_default();
    for found in &mut found {
        found.stamp.push_str(&enabled);
    }
    found
}

/// What a set of extensions is loaded for: the extensions found, the ids left off, the settings kept (as TOML) and what
/// the project requires.
type Key = (Vec<Found>, Vec<String>, String, Option<Vec<Requirement>>);

/// The sets loaded, by what for.
type Sets = Vec<(Key, Arc<Extensions>)>;

/// The sets loaded lately, newest last: one an extension replaces is dropped in time, its engines with it.
const KEPT: usize = 4;

fn loaded(found: &[Found], disabled: &[String], kept: &Kept, requires: Option<&[Requirement]>) -> Arc<Extensions> {
    static LOADED: OnceLock<Mutex<Sets>> = OnceLock::new();
    let mut disabled = disabled.to_vec();
    disabled.sort_unstable();
    disabled.dedup();
    let key = (found.to_vec(), disabled, toml::to_string(kept).unwrap_or_default(), requires.map(<[_]>::to_vec));
    let loaded = LOADED.get_or_init(Mutex::default);
    let mut loaded = loaded.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let extensions = match loaded.iter().position(|(k, _)| *k == key) {
        Some(at) => loaded.remove(at).1,
        None => Arc::new(load_without(&key.0, &key.1, kept, requires)),
    };
    loaded.push((key, extensions.clone()));
    if loaded.len() > KEPT {
        loaded.remove(0);
    }
    extensions
}

/// The user's own extensions folder: `extensions` in the editor's data folder.
#[must_use]
pub fn user_folder() -> Option<PathBuf> {
    Some(data_folder()?.join("extensions"))
}

/// The editor's data folder, as eframe places it: the XDG data folder on Linux, Application Support on macOS, the
/// roaming application data on Windows.
#[must_use]
pub fn data_folder() -> Option<PathBuf> {
    if let Some(folder) = DATA_FOLDER.get() {
        return Some(folder.clone());
    }
    if cfg!(windows) {
        std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join("pe2-editor").join("data"))
    } else if cfg!(target_os = "macos") {
        std::env::home_dir().map(|p| p.join("Library/Application Support/pe2-editor"))
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::home_dir().map(|p| p.join(".local/share")))
            .map(|p| p.join("pe2-editor"))
    }
}

/// In place of the user's data folder, for this process: what runs the editor in tests keeps away from the real one.
pub fn use_data_folder(folder: PathBuf) {
    let _ = DATA_FOLDER.set(folder);
}

static DATA_FOLDER: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// The extensions folder beside the editor.
#[must_use]
pub fn install_folder() -> Option<PathBuf> {
    Some(std::env::current_exe().ok()?.parent()?.join("extensions"))
}

/// What tells a file's version from another: its size and time.
fn file_stamp(file: &std::path::Path) -> String {
    let stat = project::content::stat(file).ok();
    let time = stat.and_then(|(_, t)| t.duration_since(std::time::UNIX_EPOCH).ok());
    format!("{} {}", stat.map_or(0, |(len, _)| len), time.map_or(0, |t| t.as_nanos()))
}

/// The stamps of what an extension's folder holds: its tests are not it. One a project holds is its tree, whose hash
/// tells its versions apart.
fn own_stamp(folder: &Path) -> String {
    if let Some(held) = project::records::held(folder) {
        return held.tree.clone();
    }
    let mut entries: Vec<PathBuf> =
        std::fs::read_dir(folder).into_iter().flatten().flatten().map(|e| e.path()).collect();
    entries.retain(|entry| entry.file_name().is_none_or(|name| name != unsandboxed::TESTS));
    entries.sort();
    entries.iter().map(|e| if e.is_dir() { folder_stamp(e) } else { file_stamp(e) }).collect::<Vec<_>>().join(" ")
}

/// The stamps of what a folder holds, below it too.
fn folder_stamp(folder: &std::path::Path) -> String {
    let mut entries: Vec<PathBuf> =
        std::fs::read_dir(folder).into_iter().flatten().flatten().map(|e| e.path()).collect();
    entries.sort();
    entries.iter().map(|e| if e.is_dir() { folder_stamp(e) } else { file_stamp(e) }).collect::<Vec<_>>().join(" ")
}

/// The extensions in a folder where they are kept, in name order: each a folder of its own (`<id>.extension`), or an
/// archive of one (`<id>-<version>.extension.zip`).
fn components_in(folder: &std::path::Path, origin: fn(PathBuf) -> Origin) -> Vec<Found> {
    let mut entries: Vec<PathBuf> =
        std::fs::read_dir(folder).into_iter().flatten().flatten().map(|e| e.path()).collect();
    entries.sort();
    let archived = |entry: PathBuf| {
        archive::is_archive(&entry).then(|| Found {
            origin: origin(entry.clone()),
            stamp: file_stamp(&entry),
            file: entry,
            folder: None,
        })
    };
    entries.into_iter().filter_map(|entry| component_at(entry.clone(), origin).or_else(|| archived(entry))).collect()
}

/// The extension in a folder of its own (`<id>.extension`), holding its component and what it ships.
fn component_at(entry: PathBuf, origin: fn(PathBuf) -> Origin) -> Option<Found> {
    if !project::pack::is_dir(&entry) || folder::id_of(&entry).is_none() {
        return None;
    }
    let file = entry.join(EXTENSION_COMPONENT);
    let stamp = own_stamp(&entry);
    project::pack::is_file(&file).then(|| Found { origin: origin(entry.clone()), file, folder: Some(entry), stamp })
}

/// The extensions in the user's folder, to be added to a project.
#[must_use]
pub fn user_components() -> Vec<Found> {
    user_folder().map_or_default(|folder| components_in(&folder, Origin::User))
}

/// The extensions beside the editor, to be added to a project.
#[must_use]
pub fn install_components() -> Vec<Found> {
    install_folder().map_or_default(|folder| components_in(&folder, Origin::Install))
}

/// Where an extension of an id is kept to be added: your own extensions' first, then those beside the editor; in each
/// its folder, else its archive (the last by name, where there are several).
#[must_use]
pub fn kept_apart(id: &str) -> Option<PathBuf> {
    let name = format!("{id}{}", folder::SUFFIX);
    let in_place = |place: PathBuf| {
        let held = place.join(&name);
        if held.is_dir() {
            return Some(held);
        }
        let mut archives: Vec<PathBuf> =
            std::fs::read_dir(&place).into_iter().flatten().flatten().map(|e| e.path()).collect();
        archives.retain(|file| archive::is_archive(file) && archive::id_of(file) == Some(id));
        archives.sort();
        archives.pop()
    };
    [user_folder(), install_folder()].into_iter().flatten().find_map(in_place)
}

#[cfg(test)]
mod tests {
    use super::{COMPONENTS, linked};
    use project::ext::component;

    #[test]
    fn components_are_held_to_the_contract_this_editor_runs() {
        let wit = include_str!("../../crates/ext/wit/extension.wit");
        assert!(
            wit.contains(&format!("package pe2:extension@{};", component::CONTRACT)),
            "the WIT package is the contract"
        );
        for (package, bytes) in COMPONENTS {
            assert_eq!(component::contract(bytes).as_deref(), Some(component::CONTRACT), "{package}");
        }
        assert!(component::runs(component::CONTRACT).is_ok());
        let said = |c: &str| component::runs(c).unwrap_err();
        // Around this editor's line, 0.<minor>: newer within it, the line before, the line after, 1.0.
        let minor: u64 = component::CONTRACT.split('.').nth(1).unwrap().parse().unwrap();
        let (ours, before, after) = (format!("0.{minor}"), format!("0.{}", minor - 1), format!("0.{}", minor + 1));
        let newer = format!("{ours}.99");
        assert!(said(&newer).contains(&format!("newer than this editor's {}: update the editor", component::CONTRACT)));
        let rebuild = format!("built for contract {before}, and this editor runs {ours}: rebuild it");
        assert!(said(&format!("{before}.0")).contains(&rebuild), "{}", said(&format!("{before}.0")));
        let update = format!("built for contract {after}, newer than this editor's {ours}");
        assert!(said(&format!("{after}.0")).contains(&update), "{}", said(&format!("{after}.0")));
        assert!(said("1.0.0").contains("built for contract 1, newer"), "{}", said("1.0.0"));
        // A component of another line is refused before it is compiled: a built-in's export renamed in place.
        let (_, bytes) = COMPONENTS[0];
        let name = format!("pe2:extension/guest@{}", component::CONTRACT);
        let mut older = bytes.to_vec();
        let ats: Vec<usize> =
            bytes.windows(name.len()).enumerate().filter(|(_, w)| *w == name.as_bytes()).map(|(at, _)| at).collect();
        assert!(!ats.is_empty(), "its export's name");
        for at in ats {
            older[at..at + name.len()].copy_from_slice(format!("pe2:extension/guest@{before}.0").as_bytes());
        }
        let refused = component::load_bytes("older", &older, project::ext::Origin::Builtin).err().expect("refused");
        assert!(format!("{refused:#}").contains("rebuild it with this editor's SDK"), "{refused:#}");
    }

    /// A built-in kept as an extension is, in a folder of its own (`<id>.extension`) holding its component, the manifest
    /// it carries and a tool: the folder.
    fn kept_as(dir: &std::path::Path, package: &str) -> std::path::PathBuf {
        let bytes = COMPONENTS.iter().find(|(p, _)| *p == package).expect("a built-in").1;
        let about = component::about(bytes).expect("its manifest");
        let folder = dir.join(format!("{}.extension", about.id));
        std::fs::create_dir_all(folder.join("linux-x86_64")).unwrap();
        std::fs::write(folder.join("extension.wasm"), bytes).unwrap();
        std::fs::write(folder.join("extension.toml"), component::manifest_text(bytes).unwrap()).unwrap();
        let tool = folder.join("linux-x86_64/tool");
        std::fs::write(&tool, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        folder
    }

    fn found(folder: &std::path::Path, origin: fn(std::path::PathBuf) -> super::Origin) -> super::Found {
        let file = folder.join("extension.wasm");
        super::Found {
            origin: origin(folder.to_path_buf()),
            file,
            folder: Some(folder.to_path_buf()),
            stamp: String::new(),
        }
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("pe2-ext-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn what_a_project_holds_runs_and_what_is_kept_apart_is_shown() {
        use project::Requirement;
        use project::ext::{Origin, Status};
        let dir = scratch("held");
        let folder = kept_as(&dir, "pe2-events");
        let status = |extensions: &project::ext::Extensions, origin: fn(&Origin) -> bool| {
            extensions.present.iter().find(|p| origin(&p.origin)).map(|p| p.status.clone())
        };
        let held = |o: &Origin| matches!(o, Origin::Project(_));
        let kept = |o: &Origin| matches!(o, Origin::User(_));
        // Held, in place of the built-in of its id.
        let extensions = super::load_without(&[found(&folder, Origin::Project)], &[], &super::Kept::new(), Some(&[]));
        assert_eq!(status(&extensions, held), Some(Status::Loaded), "{:?}", extensions.problems);
        assert_eq!(
            extensions.present.iter().find(|p| p.origin == Origin::Builtin && p.id == "pe2.events").map(|p| &p.status),
            Some(&Status::Replaced("the project's pe2.events.extension".into()))
        );
        // Kept apart: shown, never run - and required, it is to be added.
        let required = [Requirement { id: "pe2.events".to_string(), version: Some("^7.0".to_string()) }];
        let extensions =
            super::load_without(&[found(&folder, Origin::User)], &[], &super::Kept::new(), Some(&required));
        assert_eq!(status(&extensions, kept), Some(Status::Available));
        assert!(extensions.unmet[0].1.contains("asks for ^7.0"), "the built-in: {:?}", extensions.unmet);
        let required = [Requirement { id: "my.tool".to_string(), version: None }];
        let extensions = super::load_without(&[], &[], &super::Kept::new(), Some(&required));
        assert_eq!(extensions.unmet, [("my.tool".to_string(), "it was not found".to_string())]);
        // A manifest that is not its component's, and a folder named for another id: refused, problems of the project.
        std::fs::write(folder.join("extension.toml"), "id = \"pe2.events\"\nversion = \"0.1.0\"\nunsandboxed = true\n")
            .unwrap();
        let extensions = super::load_without(&[found(&folder, Origin::Project)], &[], &super::Kept::new(), Some(&[]));
        let Some(Status::Refused(why)) = status(&extensions, held) else { panic!("{:?}", extensions.present) };
        assert!(why.contains("is not its component's"), "{why}");
        assert!(extensions.problems.iter().any(|p| p.contains("is not its component's")), "{:?}", extensions.problems);
        let renamed = dir.join("other.extension");
        std::fs::rename(&folder, &renamed).unwrap();
        let folder = kept_as(&dir, "pe2-events");
        std::fs::remove_dir_all(&renamed).unwrap();
        std::fs::rename(&folder, &renamed).unwrap();
        let extensions = super::load_without(&[found(&renamed, Origin::Project)], &[], &super::Kept::new(), Some(&[]));
        let Some(Status::Refused(why)) = status(&extensions, held) else { panic!("{:?}", extensions.present) };
        assert!(why.contains("its folder is pe2.events.extension"), "{why}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_extension_is_added_whole_updated_and_removed_as_steps() {
        use project::cache::Cache;
        use project::history::History;
        use project::lock::Lock;
        use project::switches::{add_extension, remove_extension};
        let dir = scratch("add");
        let source = kept_as(&dir.join("kept"), "pe2-events");
        let root = dir.join("project");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("project.toml"), "[[layer]]\nname = \"game\"\n\n[[layer]]\nname = \"mine\"\n")
            .unwrap();
        for layer in ["game", "mine"] {
            project::records::mark(&root.join(".layers").join(layer), &project::records::Settings::default()).unwrap();
        }
        let project = project::Project::load(&root).unwrap();
        let lock = Lock::try_acquire(&root, "a test").unwrap();
        let (mut history, _) = History::open(&Cache::open(&project, &lock).unwrap()).unwrap();
        let index = project::index::Index::open(&project, &Cache::open(&project, &lock).unwrap()).unwrap();
        let about = add_extension(&project, &source, None, &mut history, "add".into()).unwrap();
        assert_eq!(about.id, "pe2.events");
        let project = project::Project::load(&root).unwrap();
        // Held by a record of the layer's: its files read where its folder was, from its tree, on no disk.
        let record = root.join(".layers/mine/.records/extensions/pe2.events.toml");
        let [held] = project.held_extensions().try_into().unwrap();
        assert_eq!(held, root.join(".layers/mine/extensions/pe2.events.extension"));
        assert!(record.is_file() && !held.exists() && !root.join(".cache/extensions").exists());
        assert_eq!(project::records::held(&held).unwrap().layer, root.join(".layers/mine"));
        assert_eq!(project.requires, [project::Requirement { id: "pe2.events".into(), version: Some("^0.1".into()) }]);
        // Trusted as the folder it came from was: the hash read from its tree is the folder's.
        let trust = |folder: &std::path::Path| {
            project::ext::unsandboxed::trust(&folder.join("extension.wasm"), Some(folder), &about).unwrap()
        };
        assert_eq!(trust(&held), trust(&source));
        // A program it ships, handed over in place: its object, read-only, still a program.
        let tool = project::ext::unsandboxed::program(Some(&held), &[], "tool");
        assert!(tool.starts_with(root.join(".layers/.store/objects")), "{}", tool.display());
        assert!(std::fs::metadata(&tool).unwrap().permissions().readonly());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&tool).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "a program is still one");
        }
        let extensions =
            super::load_without(&super::found(&project), &[], &super::Kept::new(), Some(&project.requires));
        let loaded = extensions.loaded.iter().find(|l| l.id == "pe2.events").unwrap();
        assert_eq!(loaded.origin, project::ext::Origin::Project(held.clone()));
        // Updated: added again, in place of what was there; undone, what was there is back. As it is, it is no step.
        let tool = |project: &project::Project| {
            project::content::read_to_string(&project.held_extensions()[0].join("linux-x86_64/tool")).unwrap()
        };
        let steps = history.done().len();
        add_extension(&project, &source, None, &mut history, "again".into()).unwrap();
        assert_eq!(history.done().len(), steps, "added as it is, nothing was done");
        std::fs::write(source.join("linux-x86_64/tool"), "#!/bin/sh\necho new\n").unwrap();
        add_extension(&project, &source, None, &mut history, "update".into()).unwrap();
        assert_eq!(tool(&project), "#!/bin/sh\necho new\n");
        history.undo(&index, None, false).unwrap();
        assert_eq!(tool(&project), "#!/bin/sh\n");
        // Removed: its record and what was required of it.
        remove_extension(&project, "pe2.events", &mut history, "remove".into()).unwrap();
        let project = project::Project::load(&root).unwrap();
        assert!(project.held_extensions().is_empty() && project.requires.is_empty());
        history.undo(&index, None, false).unwrap();
        history.undo(&index, None, false).unwrap();
        assert!(!record.exists(), "undone, it was never added");
        // Added to a layer named, then updated where it is, though a higher layer is written.
        add_extension(&project, &source, Some("game"), &mut history, "add".into()).unwrap();
        let in_game = root.join(".layers/game/.records/extensions/pe2.events.toml");
        assert!(in_game.is_file());
        let project = project::Project::load(&root).unwrap();
        std::fs::write(source.join("linux-x86_64/tool"), "#!/bin/sh\necho newer\n").unwrap();
        add_extension(&project, &source, None, &mut history, "update".into()).unwrap();
        assert!(!record.exists());
        assert_eq!(tool(&project), "#!/bin/sh\necho newer\n");
        assert!(add_extension(&project, &source, Some("nowhere"), &mut history, "add".into()).is_err());
        // Held by a layer the project does not write: removed, the layer written above leaves it off.
        let text = std::fs::read_to_string(root.join("project.toml")).unwrap();
        std::fs::write(
            root.join("project.toml"),
            text.replacen("name = \"game\"", "name = \"game\"\nread_only = true", 1),
        )
        .unwrap();
        let project = project::Project::load(&root).unwrap();
        remove_extension(&project, "pe2.events", &mut history, "remove".into()).unwrap();
        assert!(in_game.is_file() && record.is_file() && project.held_extensions().is_empty());
        drop(lock);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A texture lent as a node: its declaration alone.
    struct Texture {
        declaration: String,
        fields: toml::Table,
    }

    impl project::graph::exec::Node for Texture {
        fn path(&self) -> &'static str {
            "test/texture"
        }

        fn uid(&self) -> Option<&str> {
            None
        }

        fn declaration(&self) -> &str {
            &self.declaration
        }

        fn fields(&self) -> &toml::Table {
            &self.fields
        }

        fn file_sha1(&self, _: &str) -> Option<String> {
            None
        }

        fn read(&self, name: &str) -> pe2_ext::Result<Vec<u8>> {
            Err(pe2_ext::anyhow!("no file {name}"))
        }
    }

    #[test]
    fn a_component_writes_a_large_file_through_the_host() {
        use project::graph::{Content, File, Typed, Value};
        let dir = std::env::temp_dir().join(format!("pe2-ext-writer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = project::graph::store::Store::at(dir.clone()).unwrap();
        let extensions = super::load();
        let engine = project::graph::exec::Engine::new(extensions.operations(), Some(store.clone()));
        // Noise, which a PNG cannot make small: the layers written are past what the host holds in memory.
        let (width, height) = (1024, 512);
        let mut x: u32 = 1;
        let texels: Vec<u8> = (0..width * height * 2)
            .map(|_| {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                x.to_be_bytes()[0]
            })
            .collect();
        let declaration = format!("mode = \"direct_16bpp\"\nsize = [{width}, {height}]\n");
        let node = Texture { fields: declaration.parse().unwrap(), declaration };
        let native = Value::Typed(Typed {
            ty: "pe2.images@texels".into(),
            files: vec![File::new("native.texels", texels)],
            settings: node.fields.clone(),
            ..Typed::default()
        });
        let made = engine.run("pe2.images@ora_source", vec![vec![native]], &toml::Table::new(), &node).unwrap();
        let Value::Typed(made) = made else { panic!("not a file") };
        let Content::Stored { sha1, size } = &made.file().unwrap().content else { panic!("held in memory") };
        assert!(*size > project::graph::exec::SPILL as u64, "{size} bytes");
        let kept = std::fs::read(store.blob_path(sha1)).unwrap();
        assert_eq!(kept.len() as u64, *size);
        assert_eq!(&kept[30..54], b"mimetypeimage/openraster");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_atlas_is_marked_by_its_cells_and_a_wide_texture_by_its_pages() {
        use project::graph::{File, Typed, Value};
        let extensions = super::load();
        let engine = project::graph::exec::Engine::new(extensions.operations(), None);
        let marks = |declaration: &str| {
            let node = Texture { fields: declaration.parse().unwrap(), declaration: declaration.to_string() };
            let native = Value::Typed(Typed {
                ty: "pe2.images@texels".into(),
                files: vec![File::new("native.texels", vec![0; 4])],
                settings: node.fields.clone(),
                ..Typed::default()
            });
            match engine.run("pe2.images@marks", vec![vec![native]], &toml::Table::new(), &node).unwrap() {
                Value::Marks(marks) => marks,
                other => panic!("made {}", other.ty()),
            }
        };
        let atlas = marks(
            "mode = \"indexed_8bpp\"\nsize = [16, 16]\n\
             cells.a = { rect = [0, 0, 8, 16] }\ncells.b = { rect = [8, 0, 8, 16] }\n",
        );
        assert_eq!(atlas.boxes.iter().map(|b| b.name.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!((atlas.summary.as_str(), atlas.toggle.as_str(), atlas.legend), ("2 cells", "Cells", true));
        let wide = marks("mode = \"indexed_8bpp\"\nsize = [300, 8]\n");
        assert_eq!(wide.boxes.len(), 3);
        assert_eq!((wide.boxes[2].rect.x, wide.boxes[2].rect.width), (256.0, 44.0));
        assert_eq!(wide.toggle, "Pages");
        let plain = marks("mode = \"indexed_8bpp\"\nsize = [64, 8]\n");
        assert_eq!(plain, pe2_ext::Marks::default());
    }

    #[test]
    fn a_template_the_project_defines_is_offered_for_its_files_and_makes_a_node_using_its_operation() {
        let dir = std::env::temp_dir().join(format!("pe2-ext-project-template-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        project::create(&dir).unwrap();
        let p = super::Project::load(&dir).unwrap();
        let lock = project::lock::Lock::try_acquire(&p.root, "a test").unwrap();
        let cache = project::cache::Cache::open(&p, &lock).unwrap();
        let (mut history, _) = project::history::History::open(&cache).unwrap();
        let index = project::index::Index::open(&p, &cache).unwrap();
        // An operation passing on what it is given, and a template importing a text file through it.
        let group = "uid = \"uid://group\"\n\n[[stack]]\nop = \"pe2.graph@operation\"\nlabel = \"Passed\"\n\
                     ops.given = { op = \"input\", name = \"text\" }\nops.out = { op = \"output\", name = \"text\", from = { value = \"given\" } }\n";
        let template = "uid = \"uid://template\"\n\n[[stack]]\nop = \"pe2.graph@template\"\nlabel = \"Passed text\"\nfiles = [\"*.txt\"]\n\
                        makes = [{ op = \"file\", file = \"{file}\" }, { op = \"uid://group\" }, { op = \"output\", name = \"text\" }]\n";
        project::edit::create(&p, &index, "project", "uid://group", "graph/passed", (group, &[]), &mut history).unwrap();
        project::edit::create(&p, &index, "project", "uid://template", "graph/template", (template, &[]), &mut history)
            .unwrap();
        let index = project::index::Index::open(&p, &cache).unwrap();
        let extensions = super::for_project(&p);
        assert_eq!(extensions.take_definitions(&index), []);
        let offered: Vec<String> = extensions.templates_for("notes.TXT").into_iter().map(|t| t.label).collect();
        assert_eq!(offered, ["Passed text"]);
        let found = extensions.template("uid://template").unwrap();
        let stack = extensions.stamp(&found, "notes.txt").unwrap();
        assert!(stack.contains("file = \"notes.txt\""), "{stack}");
        let made = format!("uid = \"uid://made\"\n\n{stack}");
        let files: [(&str, &[u8]); 1] = [("notes.txt", b"hello")];
        project::edit::create(&p, &index, "project", "uid://made", "notes", (&made, &files), &mut history).unwrap();
        let index = project::index::Index::open(&p, &cache).unwrap();
        let _ = extensions.take_definitions(&index);
        let engine = extensions.engine();
        let given = extensions.output(&engine, &index, &index.assets["notes"], "text", true).unwrap();
        let project::graph::Value::Typed(typed) = &*given.value.unwrap() else { panic!("no file") };
        assert_eq!(typed.files[0].name, "notes.txt");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn each_component_says_who_it_is_without_running() {
        assert_eq!(COMPONENTS.len(), linked().len());
        for (package, bytes) in COMPONENTS {
            let about = component::about(bytes).unwrap_or_else(|| panic!("{package}: no section"));
            let declared =
                linked().into_iter().find(|e| e.id == about.id).unwrap_or_else(|| panic!("{}: not linked", about.id));
            assert_eq!(about.version, declared.version, "{package}");
            assert_eq!(about.requires, declared.requires, "{package}");
            assert_eq!(about.weak, declared.weak, "{package}");
            assert_eq!(about.optional, declared.optional, "{package}");
            assert_eq!(about.unsandboxed, declared.unsandboxed, "{package}");
            assert!(about.described.description.is_some(), "{package}: its manifest says what it is");
        }
    }
}
