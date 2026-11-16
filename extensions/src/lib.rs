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
use sfs::HostPath;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use toml::{Table, Value};

/// An extension's tests, in its folder.
const TESTS: &pe2_path::Name = pe2_path::Name::constant(project::ext::unsandboxed::TESTS);
/// The editor's settings, in its data folder.
const SETTINGS: &pe2_path::Name = pe2_path::Name::constant("settings.toml");
/// Extensions kept to be added, in a folder.
const EXTENSIONS: &pe2_path::Name = pe2_path::Name::constant("extensions");

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
#[allow(clippy::disallowed_methods, reason = "the program's own files: plain paths, joined of names it writes")]
pub fn built_apart(file: &str) -> Option<Vec<u8>> {
    let file = pe2_path::Name::new(file).ok()?;
    std::fs::read(Path::new(env!("PE2_BUILT")).join(file.as_str())).ok()
}

/// The tests of the built-ins, each a `tests/` folder, as the build lays them out.
#[must_use]
#[allow(clippy::disallowed_methods, reason = "the program's own files: plain paths, joined of names it writes")]
pub fn built_suites() -> Vec<PathBuf> {
    let Ok(listed) = std::fs::read_dir(env!("PE2_SUITES")) else { return Vec::new() };
    let entries = listed.flatten().map(|entry| entry.path());
    let mut suites: Vec<PathBuf> =
        entries.map(|entry| entry.join(TESTS.as_str())).filter(|tests| tests.is_dir()).collect();
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
    load_without(&[], &[], &Kept::new(), None, &pe2_io::system())
}

/// The settings kept for extensions, by id: each a table by scope (`{ project = { ... }, user = { ... } }`).
pub type Kept = BTreeMap<String, Table>;

/// An extension found beside the built-ins: one the project holds, one kept to be added to it, or tests.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Found {
    pub origin: Origin,
    /// Its component.
    pub file: HostPath,
    /// The folder it is in with what it ships, where it is in one of its own.
    pub folder: Option<HostPath>,
    /// What tells this version of it from another: its files' sizes and times - and what the user enabled, as that
    /// changes what it runs as.
    pub stamp: String,
}

/// Load the extensions the project holds and the built-ins, but those it leaves off (`disabled`, by id) and those
/// requiring them; what is kept to be added (yours, beside the editor) is shown, and never run. `requires`, what the
/// project requires (`[[extension]]`), where there is a project.
#[must_use]
pub fn load_without(
    found: &[Found],
    disabled: &[String],
    kept: &Kept,
    requires: Option<&[Requirement]>,
    io: &Arc<dyn pe2_io::Io>,
) -> Extensions {
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
    // What the unsandboxed ones loaded offer to run, filled once it is known which are loaded.
    let offered = project::ext::unsandboxed::Offered::default();
    let mut unsandboxed_loaded: Vec<Arc<Unsandboxed>> = vec![];
    for found in found {
        // What is kept to be added runs only once a project holds it.
        let kept_apart = matches!(found.origin, Origin::User(_) | Origin::Install(_));
        // An archive of one is read without unpacking it.
        if kept_apart && archive::is_archive(io.filesystem(), &found.file) {
            unloadable.push(match archive::read(io.filesystem(), &found.file) {
                Ok((about, component)) => shown(about, found, &component, Status::Available, None),
                Err(e) => unreadable(io.filesystem(), found, format!("{e:#}")),
            });
            continue;
        }
        // Only its manifest: what it offers to run, nothing registered.
        if found.file.file_name().is_some_and(|name| name == folder::MANIFEST) {
            match manifest_alone(found, kept_apart, &enabled, &offered, io) {
                Ok(Alone::Loads(extension, unsandboxed)) => {
                    unsandboxed_loaded.extend(unsandboxed);
                    loadable.push(*extension);
                }
                Ok(Alone::Shown(present)) => unloadable.push(*present),
                Err(e) => {
                    if !kept_apart {
                        failed.push(format!("{}: {e}", found.origin.said()));
                    }
                    unloadable.push(unreadable(io.filesystem(), found, e));
                }
            }
            continue;
        }
        let loaded =
            project::content::read(io, &found.file).map_err(|e| format!("it cannot be read: {e}")).and_then(|bytes| {
                if !bytes.starts_with(b"\0asm") {
                    return Err("it is not a WebAssembly component".to_string());
                }
                // In a folder of its own, its manifest is its component's: what says whether it needs trusting.
                let about = match found.origin.folder() {
                    Some(held) => Some(folder::checked(held, &bytes, io).map_err(|e| format!("{e:#}"))?),
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
                        let trust = unsandboxed::trust(&found.file, found.folder.as_ref(), &about, io)
                            .map_err(|e| format!("{e:#}"))?;
                        if !enabled.contains_key(&trust) && trust::allowed_by(&found.origin).is_none() {
                            let changed = enabled.values().any(|id| *id == about.id);
                            unloadable.push(shown(about, found, &bytes, Status::NotEnabled { changed }, Some(trust)));
                            return Ok(None);
                        }
                        let unsandboxed = unsandboxed_of(&about, found.folder.as_ref(), trust, &offered, io);
                        unsandboxed_loaded.push(unsandboxed.clone());
                        Some(unsandboxed)
                    }
                    None => None,
                };
                // The first line of what went wrong: the parser's is followed by the bytes it found.
                let loaded = match held {
                    Some(held) => component::load_unsandboxed(&name, &bytes, found.origin.clone(), held, io.clone()),
                    None => component::load_bytes(&name, &bytes, found.origin.clone(), io.clone()),
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
                unloadable.push(unreadable(io.filesystem(), found, e));
            }
        }
    }
    builtins(&mut loadable, &mut failed, io);
    loadable.extend(TEST_HOST.get().map(|host| host().into()));
    let mut extensions = Extensions::load_with(with_settings(loadable), true, disabled, io.clone());
    offering(&mut extensions, offered, unsandboxed_loaded);
    extensions.problems.extend(failed);
    extensions.present.extend(unloadable);
    if let Some(requires) = requires {
        extensions.unmet = unmet(requires, &extensions.present);
    }
    extensions
}

/// What an extension runs outside the sandbox as: what its setup fetches here, where it is kept, and what of it the
/// user turned off; the programs it offers here; its installation notes; and what the others loaded offer.
fn unsandboxed_of(
    about: &component::About,
    folder: Option<&HostPath>,
    trust: String,
    offered: &project::ext::unsandboxed::Offered,
    io: &Arc<dyn pe2_io::Io>,
) -> Arc<Unsandboxed> {
    let install =
        about.described.install.as_ref().and_then(|page| Some(folder?.join(&pe2_path::RelPath::new(page).ok()?)));
    Arc::new(Unsandboxed {
        id: about.id.clone(),
        folder: folder.cloned(),
        trust,
        install,
        store: setup_folder(),
        setup: about.setup.iter().filter(|s| s.here()).cloned().collect(),
        off: trust::optional_off(),
        programs: about.program.iter().filter(|p| p.here()).cloned().collect(),
        offered: offered.clone(),
        io: io.clone(),
    })
}

/// What an extension that is only its manifest comes to: loaded, with what it offers to run where it runs unsandboxed;
/// or shown and not run (kept apart, not trusted).
enum Alone {
    Loads(Box<Loadable>, Option<Arc<Unsandboxed>>),
    Shown(Box<Present>),
}

/// An extension that is only its manifest (`found.file`): checked as one with a component is, trusted as one is - its
/// hash its folder's, its manifest among what counts, as it is what it is.
fn manifest_alone(
    found: &Found,
    kept_apart: bool,
    enabled: &BTreeMap<String, String>,
    offered: &project::ext::unsandboxed::Offered,
    io: &Arc<dyn pe2_io::Io>,
) -> Result<Alone, String> {
    let held = found.folder.as_ref().ok_or("it is a manifest outside a folder of its own")?;
    let about = folder::checked_alone(held, io).map_err(|e| format!("{e:#}"))?;
    if let Some(why) = unsandboxed::elsewhere(&about.platforms) {
        return Err(why);
    }
    let text = project::content::read(io, &found.file).unwrap_or_default();
    if kept_apart {
        return Ok(Alone::Shown(Box::new(shown(about, found, &text, Status::Available, None))));
    }
    if !about.unsandboxed {
        return Ok(Alone::Loads(Box::new(component::load_manifest(about, found.origin.clone(), None)), None));
    }
    let trust = unsandboxed::trust(&found.file, Some(held), &about, io).map_err(|e| format!("{e:#}"))?;
    if !enabled.contains_key(&trust) && trust::allowed_by(&found.origin).is_none() {
        let changed = enabled.values().any(|id| *id == about.id);
        return Ok(Alone::Shown(Box::new(shown(about, found, &text, Status::NotEnabled { changed }, Some(trust)))));
    }
    let unsandboxed = unsandboxed_of(&about, Some(held), trust, offered, io);
    let loadable = component::load_manifest(about, found.origin.clone(), Some(unsandboxed.clone()));
    Ok(Alone::Loads(Box::new(loadable), Some(unsandboxed)))
}

/// The built-ins: their crates, linked in, where asked (a native profiler sees into them), else their components.
fn builtins(loadable: &mut Vec<Loadable>, failed: &mut Vec<String>, io: &Arc<dyn pe2_io::Io>) {
    if io.environment().var("PE2_EXTENSIONS").is_some_and(|v| v == "linked") {
        loadable.extend(linked().into_iter().map(Into::into));
        return;
    }
    for (package, bytes) in COMPONENTS {
        match component::load_bytes(package, bytes, Origin::Builtin, io.clone()) {
            Ok(extension) => loadable.push(extension),
            Err(e) => failed.push(format!("{package}: {e:#}")),
        }
    }
}

/// What the extensions loaded offer to run, known once it is known which loaded: those of the unsandboxed ones that did.
fn offering(
    extensions: &mut Extensions,
    offered: project::ext::unsandboxed::Offered,
    unsandboxed: Vec<Arc<Unsandboxed>>,
) {
    let loaded: std::collections::HashSet<&str> =
        extensions.present.iter().filter(|p| p.status == Status::Loaded).map(|p| p.id.as_str()).collect();
    let by_id = unsandboxed.into_iter().filter(|u| loaded.contains(u.id.as_str())).map(|u| (u.id.clone(), u));
    let _ = offered.set(by_id.collect());
    extensions.offered = offered;
}

/// Where what extensions' setup fetches is kept on this machine: `setup` in the editor's data folder.
#[must_use]
#[allow(clippy::disallowed_methods, reason = "the program's own files: plain paths, joined of names it writes")]
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
            if let Err(e) =
                project::ext::setup::fetch(setup, &store, folder, &mut progress, &cancel, &**extensions.io())
            {
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
fn unreadable(fs: &dyn pe2_io::Filesystem, found: &Found, why: String) -> Present {
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
        size: fs.stat(&found.file).ok().and_then(|stat| usize::try_from(stat.size).ok()),
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
    loaded(&[], &[], &Kept::new(), None, &pe2_io::system())
}

/// The extensions as a project has them: those it holds and the built-ins, but those it leaves off, with what is kept
/// to be added shown - loaded once a process for each such set, so a changed extension loads again.
#[must_use]
pub fn for_project(project: &Project) -> Arc<Extensions> {
    let extensions =
        loaded(&found(project), &project.disabled_extensions, &kept(project), Some(&project.requires), &project.io);
    // Its operations run programs only where the user trusts it as a whole.
    let trusted = trust::allowed_by(&Origin::Project(project.root.clone())).is_some();
    extensions.programs_trusted.store(trusted, std::sync::atomic::Ordering::Relaxed);
    extensions
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
/// each `auto`, `none` or a backend's name; the budgets `copies` and `kept`; and `cache`, where what is kept apart is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FilesystemDefaults {
    pub serve: Option<String>,
    pub scopes: Option<String>,
    /// How much laying files out for a program may write, and how much may be kept of what was read of a project's
    /// folder: budgets of bytes (`16 MiB`).
    pub copies: Option<String>,
    pub kept: Option<String>,
    /// How much memory the pages kept of what was read of a project's files may take (`256 MiB`).
    pub pages: Option<String>,
    /// Where what a backend keeps apart from the folder it serves is kept: a folder.
    pub cache: Option<String>,
}

/// What this machine says serves projects (`FilesystemDefaults`); nothing where the settings say nothing, or cannot be
/// read.
#[must_use]
#[allow(clippy::disallowed_methods, reason = "the program's own files: plain paths, joined of names it writes")]
pub fn filesystem_defaults() -> FilesystemDefaults {
    let read = || {
        let text = std::fs::read_to_string(data_folder()?.join(SETTINGS)).ok()?;
        let Some(Value::Table(filesystem)) = toml::from_str::<Table>(&text).ok()?.remove("filesystem") else {
            return None;
        };
        let said = |key: &str| filesystem.get(key).and_then(Value::as_str).map(str::to_string);
        Some(FilesystemDefaults {
            serve: said("serve"),
            scopes: said("scopes"),
            copies: said("copies"),
            kept: said("kept"),
            pages: said("pages"),
            cache: said("cache"),
        })
    };
    read().unwrap_or_default()
}

/// The user's settings of extensions: `[extensions."<id>"]` in the editor's `settings.toml`, beside its other ones.
#[must_use]
#[allow(clippy::disallowed_methods, reason = "the program's own files: plain paths, joined of names it writes")]
pub fn user_settings() -> BTreeMap<String, Table> {
    let Some(file) = data_folder().map(|d| d.join(SETTINGS)) else { return BTreeMap::new() };
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
fn tests_of(folder: &HostPath, io: &Arc<dyn pe2_io::Io>) -> Option<Found> {
    let file = folder.join(&TESTS).join(&pe2_path::Name::constant("tests.wasm"));
    project::content::is_file(io, &file).then(|| Found {
        origin: Origin::Component(file.clone()),
        stamp: file_stamp(&file, io),
        folder: file.parent(),
        file,
    })
}

/// The extensions a project holds (`Project::held_extensions`).
#[must_use]
pub fn held_components(project: &Project) -> Vec<Found> {
    project.held_extensions().into_iter().filter_map(|held| component_at(held, Origin::Project, &project.io)).collect()
}

/// The extensions found beside the built-ins: those the project holds, those kept to be added (yours, beside the
/// editor). Two findings differ where one came, went or changed, or what the user enabled did: the extensions are
/// loaded again.
#[must_use]
pub fn found(project: &Project) -> Vec<Found> {
    let mut found = held_components(project);
    found.extend(user_components(&project.io));
    found.extend(install_components(&project.io));
    // In test mode, what each held carries of tests.
    if TEST_HOST.get().is_some() {
        let held = found.iter().filter(|found| matches!(found.origin, Origin::Project(_)));
        let tests: Vec<Found> = held.filter_map(|found| tests_of(found.folder.as_ref()?, &project.io)).collect();
        found.extend(tests);
    }
    // What the user enabled - and turned off of what is fetched - changes what each runs as.
    let enabled = trust::file()
        .and_then(|f| HostPath::new(&f).ok())
        .and_then(|f| project.io.filesystem().read_to_string(&f).ok())
        .unwrap_or_default();
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

fn loaded(
    found: &[Found],
    disabled: &[String],
    kept: &Kept,
    requires: Option<&[Requirement]>,
    io: &Arc<dyn pe2_io::Io>,
) -> Arc<Extensions> {
    static LOADED: OnceLock<Mutex<Sets>> = OnceLock::new();
    let mut disabled = disabled.to_vec();
    disabled.sort_unstable();
    disabled.dedup();
    let key = (found.to_vec(), disabled, toml::to_string(kept).unwrap_or_default(), requires.map(<[_]>::to_vec));
    let loaded = LOADED.get_or_init(Mutex::default);
    let mut loaded = loaded.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let extensions = match loaded.iter().position(|(k, _)| *k == key) {
        Some(at) => loaded.remove(at).1,
        None => Arc::new(load_without(&key.0, &key.1, kept, requires, io)),
    };
    loaded.push((key, extensions.clone()));
    if loaded.len() > KEPT {
        loaded.remove(0);
    }
    extensions
}

/// The user's own extensions folder: `extensions` in the editor's data folder.
#[must_use]
#[allow(clippy::disallowed_methods, reason = "the program's own files: plain paths, joined of names it writes")]
pub fn user_folder() -> Option<PathBuf> {
    Some(data_folder()?.join(EXTENSIONS))
}

/// The editor's data folder, as eframe places it: the XDG data folder on Linux, Application Support on macOS, the
/// roaming application data on Windows.
#[must_use]
#[allow(
    clippy::disallowed_methods,
    reason = "the program's own data folder, which eframe finds by the system's environment too: the two agree; what it is given and the program's own files are plain paths, joined of names it writes"
)]
pub fn data_folder() -> Option<PathBuf> {
    if let Some(folder) = DATA_FOLDER.get() {
        return Some(folder.clone());
    }
    // A folder the environment names, where it names one: a relative one names none. Read of the system's
    // environment, as eframe reads it for the same folder: the two agree.
    let place = |path: PathBuf| path.is_absolute().then_some(path);
    let editor = "pe2-editor";
    if cfg!(windows) {
        let roaming = place(std::env::var_os("APPDATA")?.into())?;
        Some(roaming.join(editor).join("data"))
    } else if cfg!(target_os = "macos") {
        let home = place(std::env::home_dir()?)?;
        Some(home.join("Library").join("Application Support").join(editor))
    } else {
        let data = std::env::var_os("XDG_DATA_HOME").and_then(|data| place(data.into()));
        let data = data.or_else(|| Some(place(std::env::home_dir()?)?.join(".local").join("share")));
        Some(data?.join(editor))
    }
}

/// In place of the user's data folder, for this process: what runs the editor in tests keeps away from the real one.
pub fn use_data_folder(folder: PathBuf) {
    let _ = DATA_FOLDER.set(folder);
}

static DATA_FOLDER: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// The extensions folder beside the editor.
#[must_use]
#[allow(
    clippy::disallowed_methods,
    reason = "beside the program as the system started it: the process's own place, no project's"
)]
pub fn install_folder() -> Option<PathBuf> {
    Some(std::env::current_exe().ok()?.with_file_name(EXTENSIONS.as_str()))
}

/// What tells a file's version from another: its size and time.
fn file_stamp(file: &HostPath, io: &Arc<dyn pe2_io::Io>) -> String {
    let stat = project::content::stat(io, file).ok();
    let time = stat.and_then(|(_, t)| t.duration_since(std::time::UNIX_EPOCH).ok());
    format!("{} {}", stat.map_or(0, |(len, _)| len), time.map_or(0, |t| t.as_nanos()))
}

/// The stamps of what an extension's folder holds: its tests are not it. One a project holds is its tree, whose hash
/// tells its versions apart.
fn own_stamp(folder: &HostPath, io: &Arc<dyn pe2_io::Io>) -> String {
    if let Some((_, tree)) = project::records::Set::sealed_at(folder) {
        return tree.to_string();
    }
    let mut entries: Vec<HostPath> = folder.entries().collect();
    entries.retain(|entry| entry.file_name().is_none_or(|name| name != TESTS));
    entries.sort();
    let stamp = |e: &HostPath| if io.filesystem().is_folder(e) { folder_stamp(e, io) } else { file_stamp(e, io) };
    entries.iter().map(stamp).collect::<Vec<_>>().join(" ")
}

/// The stamps of what a folder holds, below it too.
fn folder_stamp(folder: &HostPath, io: &Arc<dyn pe2_io::Io>) -> String {
    let mut entries: Vec<HostPath> = folder.entries().collect();
    entries.sort();
    let stamp = |e: &HostPath| if io.filesystem().is_folder(e) { folder_stamp(e, io) } else { file_stamp(e, io) };
    entries.iter().map(stamp).collect::<Vec<_>>().join(" ")
}

/// The extensions in a folder where they are kept, in name order: each a folder of its own (`<id>.extension`), or an
/// archive of one (`<id>-<version>.extension.zip`).
fn components_in(folder: &Path, origin: fn(HostPath) -> Origin, io: &Arc<dyn pe2_io::Io>) -> Vec<Found> {
    // What it holds, as places: the extensions' folders and archives are where their programs are found and what is
    // trusted of them. A folder the system cannot name holds none.
    let Ok(folder) = HostPath::new(folder) else { return Vec::new() };
    let mut entries: Vec<HostPath> = folder.entries().collect();
    entries.sort();
    let archived = |entry: HostPath| {
        archive::is_archive(io.filesystem(), &entry).then(|| Found {
            origin: origin(entry.clone()),
            stamp: file_stamp(&entry, io),
            file: entry,
            folder: None,
        })
    };
    entries
        .into_iter()
        .filter_map(|entry| component_at(entry.clone(), origin, io).or_else(|| archived(entry)))
        .collect()
}

/// The extension in a folder of its own, holding its manifest - whatever the folder is named - its component and what
/// it ships.
fn component_at(entry: HostPath, origin: fn(HostPath) -> Origin, io: &Arc<dyn pe2_io::Io>) -> Option<Found> {
    if !project::content::is_file(io, &entry.join(&folder::MANIFEST)) {
        return None;
    }
    // Its component; or, where it is only its manifest (programs and what they are fetched from), that.
    let file = if folder::alone(&entry, io) { entry.join(&folder::MANIFEST) } else { entry.join(&EXTENSION_COMPONENT) };
    let stamp = own_stamp(&entry, io);
    project::content::is_file(io, &file).then(|| Found {
        origin: origin(entry.clone()),
        file,
        folder: Some(entry),
        stamp,
    })
}

/// The extensions in the user's folder, to be added to a project.
#[must_use]
pub fn user_components(io: &Arc<dyn pe2_io::Io>) -> Vec<Found> {
    user_folder().map_or_default(|folder| components_in(&folder, Origin::User, io))
}

/// The extensions beside the editor, to be added to a project.
#[must_use]
pub fn install_components(io: &Arc<dyn pe2_io::Io>) -> Vec<Found> {
    install_folder().map_or_default(|folder| components_in(&folder, Origin::Install, io))
}

/// Where an extension of an id is kept to be added: your own extensions' first, then those beside the editor; in each
/// its folder, else its archive (the last by name, where there are several).
#[must_use]
#[allow(clippy::disallowed_methods, reason = "the program's own files: plain paths, joined of names it writes")]
pub fn kept_apart(id: &str) -> Option<PathBuf> {
    let name = format!("{id}{}", folder::SUFFIX);
    let name = pe2_path::Name::new(&name).ok()?;
    let in_place = |place: PathBuf| {
        let held = place.join(name);
        if held.is_dir() {
            return Some(held);
        }
        let listed = std::fs::read_dir(&place).into_iter().flatten().flatten();
        let mut archives: Vec<PathBuf> = listed.map(|entry| entry.path()).collect();
        archives
            .retain(|file| archive::is_archive(&pe2_io::SystemFilesystem, file) && archive::id_of(file) == Some(id));
        archives.sort();
        archives.pop()
    };
    [user_folder(), install_folder()].into_iter().flatten().find_map(in_place)
}

#[cfg(test)]
#[allow(
    clippy::disallowed_methods,
    reason = "a test's fixtures: the machine's time, places, files and environment, which nothing tested reads"
)]
mod tests {
    use super::{COMPONENTS, linked};
    use project::ext::component;
    use sfs::HostPath;
    use sfs::testing::At as _;

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
        let refused = component::load_bytes("older", &older, project::ext::Origin::Builtin, pe2_io::system())
            .err()
            .expect("refused");
        assert!(format!("{refused:#}").contains("rebuild it with this editor's SDK"), "{refused:#}");
    }

    /// A built-in kept as an extension is, in a folder of its own (`<id>.extension`) holding its component, the manifest
    /// it carries and a tool: the folder.
    fn kept_as(dir: &HostPath, package: &str) -> HostPath {
        let bytes = COMPONENTS.iter().find(|(p, _)| *p == package).expect("a built-in").1;
        let about = component::about(bytes).expect("its manifest");
        let folder = dir.at(&format!("{}.extension", about.id));
        std::fs::create_dir_all(folder.at(&project::ext::unsandboxed::platform())).unwrap();
        std::fs::write(folder.at("extension.wasm"), bytes).unwrap();
        std::fs::write(folder.at("extension.toml"), component::manifest_text(bytes).unwrap()).unwrap();
        let tool = folder.at(&shipped_tool());
        std::fs::write(&tool, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        folder
    }

    /// Where the tool `kept_as` ships is in its folder: for the platform the test runs on, as a release for it is.
    fn shipped_tool() -> String {
        format!("{}/tool{}", project::ext::unsandboxed::platform(), std::env::consts::EXE_SUFFIX)
    }

    fn found(folder: &HostPath, origin: fn(HostPath) -> super::Origin) -> super::Found {
        let file = folder.at("extension.wasm");
        super::Found { origin: origin(folder.clone()), file, folder: Some(folder.clone()), stamp: String::new() }
    }

    fn scratch(name: &str) -> HostPath {
        let dir = sfs::testing::scratch(&format!("ext-{name}"));
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
        let extensions = super::load_without(
            &[found(&folder, Origin::Project)],
            &[],
            &super::Kept::new(),
            Some(&[]),
            &pe2_io::system(),
        );
        assert_eq!(status(&extensions, held), Some(Status::Loaded), "{:?}", extensions.problems);
        assert_eq!(
            extensions.present.iter().find(|p| p.origin == Origin::Builtin && p.id == "pe2.events").map(|p| &p.status),
            Some(&Status::Replaced("the project's pe2.events.extension".into()))
        );
        // Kept apart: shown, never run - and required, it is to be added.
        let required = [Requirement { id: "pe2.events".to_string(), version: Some("^7.0".to_string()) }];
        let extensions = super::load_without(
            &[found(&folder, Origin::User)],
            &[],
            &super::Kept::new(),
            Some(&required),
            &pe2_io::system(),
        );
        assert_eq!(status(&extensions, kept), Some(Status::Available));
        assert!(extensions.unmet[0].1.contains("asks for ^7.0"), "the built-in: {:?}", extensions.unmet);
        let required = [Requirement { id: "my.tool".to_string(), version: None }];
        let extensions = super::load_without(&[], &[], &super::Kept::new(), Some(&required), &pe2_io::system());
        assert_eq!(extensions.unmet, [("my.tool".to_string(), "it was not found".to_string())]);
        // A manifest that is not its component's, and a folder named for another id: refused, problems of the project.
        std::fs::write(folder.at("extension.toml"), "id = \"pe2.events\"\nversion = \"0.1.0\"\nunsandboxed = true\n")
            .unwrap();
        let extensions = super::load_without(
            &[found(&folder, Origin::Project)],
            &[],
            &super::Kept::new(),
            Some(&[]),
            &pe2_io::system(),
        );
        let Some(Status::Refused(why)) = status(&extensions, held) else { panic!("{:?}", extensions.present) };
        assert!(why.contains("is not its component's"), "{why}");
        assert!(extensions.problems.iter().any(|p| p.contains("is not its component's")), "{:?}", extensions.problems);
        // A folder named anything: an extension by what it holds.
        let renamed = dir.at("anything");
        std::fs::rename(&folder, &renamed).unwrap();
        let folder = kept_as(&dir, "pe2-events");
        std::fs::remove_dir_all(&renamed).unwrap();
        std::fs::rename(&folder, &renamed).unwrap();
        let extensions = super::load_without(
            &[found(&renamed, Origin::Project)],
            &[],
            &super::Kept::new(),
            Some(&[]),
            &pe2_io::system(),
        );
        let status = status(&extensions, held);
        assert!(status.is_some() && !matches!(status, Some(Status::Refused(_))), "{:?}", extensions.present);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[expect(clippy::too_many_lines, reason = "one extension's life in a project, a step after another")]
    fn an_extension_is_added_whole_updated_and_removed_as_steps() {
        use project::history::History;
        use project::local::Local;
        use project::lock::Lock;
        use project::switches::{add_extension, remove_extension};
        let dir = scratch("add");
        let source = kept_as(&dir.at("kept"), "pe2-events");
        let root = dir.at("project");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.at("project.toml"), "[[layer]]\nname = \"game\"\n\n[[layer]]\nname = \"mine\"\n").unwrap();
        for layer in ["game", "mine"] {
            project::records::mark(
                &pe2_io::SystemFilesystem,
                &root.at("layers").at(layer),
                &project::records::Settings::default(),
            )
            .unwrap();
        }
        let project = project::Project::load(&root).unwrap();
        let lock = Lock::try_acquire(&root, "a test", &pe2_io::System).unwrap();
        let (mut history, _) = History::open(&Local::open(&project, &lock).unwrap()).unwrap();
        let index = project::index::Index::open(&project, &Local::open(&project, &lock).unwrap()).unwrap();
        // What a step wrote read again, as the index taking it in would.
        let read = |root: &HostPath| {
            let project = project::Project::load(root).unwrap();
            project::records::Set::open(&project).unwrap();
            project
        };
        let about = add_extension(&project, &source, None, &mut history, "add".into()).unwrap();
        assert_eq!(about.id, "pe2.events");
        let project = read(&root);
        // A sealed node of the layer's, placed in `extensions/`: its files read from its tree, on no disk.
        let [held] = project.held_extensions().try_into().unwrap();
        assert_eq!(held, root.at("layers/mine/tree/extensions/pe2.events.node"));
        assert!(!held.exists() && !root.at(".local/outputs/extensions").exists());
        let (layer, _) = project::records::Set::sealed_at(&held).unwrap();
        assert_eq!(layer, root.at("layers/mine"));
        // Its records: the folder it is placed in, and it.
        let records = |layer: &str| {
            read(&root);
            project::records::Set::records_of(&root.at("layers").at(layer).at("tree"))
        };
        assert_eq!(records("mine").unwrap().len(), 2);
        assert_eq!(project.requires, [project::Requirement { id: "pe2.events".into(), version: Some("^0.1".into()) }]);
        // Trusted as the folder it came from was: the hash read from its tree is the folder's.
        let trust = |folder: &HostPath| {
            project::ext::unsandboxed::trust(&folder.at("extension.wasm"), Some(folder), &about, &pe2_io::system())
                .unwrap()
        };
        assert_eq!(trust(&held), trust(&source));
        // A program it ships, handed over in place: its object, read-only, still a program.
        let project::ext::unsandboxed::Runnable::At(tool) =
            project::ext::unsandboxed::program(Some(&held), &[], "tool", &pe2_io::system())
        else {
            panic!("no program found")
        };
        assert!(tool.starts_with(&root.at("store/objects")), "{tool}");
        assert!(std::fs::metadata(&tool).unwrap().permissions().readonly());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&tool).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "a program is still one");
        }
        let extensions = super::load_without(
            &super::found(&project),
            &[],
            &super::Kept::new(),
            Some(&project.requires),
            &pe2_io::system(),
        );
        let loaded = extensions.loaded.iter().find(|l| l.id == "pe2.events").unwrap();
        assert_eq!(loaded.origin, project::ext::Origin::Project(held.clone()));
        // Updated: added again, in place of what was there; undone, what was there is back. As it is, it is no step.
        let tool = |root: &HostPath| {
            let project = read(root);
            project::content::read_to_string(&project.io, &project.held_extensions()[0].at(&shipped_tool())).unwrap()
        };
        let steps = history.done().len();
        add_extension(&project, &source, None, &mut history, "again".into()).unwrap();
        assert_eq!(history.done().len(), steps, "added as it is, nothing was done");
        std::fs::write(source.at(&shipped_tool()), "#!/bin/sh\necho new\n").unwrap();
        add_extension(&project, &source, None, &mut history, "update".into()).unwrap();
        assert_eq!(tool(&root), "#!/bin/sh\necho new\n");
        assert_eq!(records("mine").unwrap().len(), 2, "updated in its record");
        history.undo(&index, None, false).unwrap();
        assert_eq!(tool(&root), "#!/bin/sh\n");
        // Removed: its record and what was required of it.
        let project = read(&root);
        remove_extension(&project, "pe2.events", &mut history, "remove".into()).unwrap();
        let project = read(&root);
        assert!(project.held_extensions().is_empty() && project.requires.is_empty());
        history.undo(&index, None, false).unwrap();
        history.undo(&index, None, false).unwrap();
        assert!(records("mine").unwrap_or_default().is_empty(), "undone, it was never added");
        // Added to a layer named, then updated where it is, though a higher layer is written.
        let project = read(&root);
        add_extension(&project, &source, Some(pe2_path::Name::constant("game")), &mut history, "add".into()).unwrap();
        assert_eq!(records("game").unwrap().len(), 2);
        let project = read(&root);
        std::fs::write(source.at(&shipped_tool()), "#!/bin/sh\necho newer\n").unwrap();
        add_extension(&project, &source, None, &mut history, "update".into()).unwrap();
        assert!(records("mine").unwrap_or_default().is_empty());
        assert_eq!(tool(&root), "#!/bin/sh\necho newer\n");
        assert!(
            add_extension(&project, &source, Some(pe2_path::Name::constant("nowhere")), &mut history, "add".into())
                .is_err()
        );
        // Held by a layer the project does not write: removed, the layer written above leaves it off.
        let text = std::fs::read_to_string(root.at("project.toml")).unwrap();
        std::fs::write(
            root.at("project.toml"),
            text.replacen("name = \"game\"", "name = \"game\"\nread_only = true", 1),
        )
        .unwrap();
        let project = read(&root);
        remove_extension(&project, "pe2.events", &mut history, "remove".into()).unwrap();
        let project = read(&root);
        assert_eq!(records("game").unwrap().len(), 2, "what it does not write is as it was");
        let deleted = records("mine").unwrap();
        assert!(deleted.values().all(|record| record.op == project::records::Op::Delete), "{deleted:?}");
        assert_eq!(project.held_extensions(), Vec::<HostPath>::new());
        drop(lock);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A texture lent as a node: its declaration alone.
    struct Texture {
        declaration: String,
        fields: toml::Table,
    }

    impl project::graph::exec::Node for Texture {
        fn path(&self) -> &'static project::TreePath {
            project::TreePath::constant("test/texture")
        }

        fn uid(&self) -> Option<pe2_path::Uid> {
            None
        }

        fn declaration(&self) -> &str {
            &self.declaration
        }

        fn fields(&self) -> &toml::Table {
            &self.fields
        }

        fn file_sha1(&self, _: &pe2_ext::RelPath) -> Option<pe2_ext::Sha1> {
            None
        }

        fn read(&self, name: &pe2_ext::RelPath) -> pe2_ext::Result<Vec<u8>> {
            Err(pe2_ext::anyhow!("no file {name}"))
        }
    }

    #[test]
    fn a_component_writes_a_large_file_through_the_host() {
        use project::graph::{Content, File, Typed, Value};
        let dir = sfs::testing::scratch("ext-writer");
        let _ = std::fs::remove_dir_all(&dir);
        let store = project::graph::store::Store::at(dir.clone(), pe2_io::system()).unwrap();
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
            files: vec![File::new(pe2_ext::RelPath::constant("native.texels"), texels)],
            settings: node.fields.clone(),
            ..Typed::default()
        });
        let made = engine.run("pe2.images@ora_source", vec![vec![native]], &toml::Table::new(), &node).unwrap();
        let Value::Typed(made) = made else { panic!("not a file") };
        let Content::Stored { sha1, size } = &made.file().unwrap().content else { panic!("held in memory") };
        assert!(*size > project::graph::exec::SPILL as u64, "{size} bytes");
        let kept = std::fs::read(store.blob_path(sha1).unwrap()).unwrap();
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
                files: vec![File::new(pe2_ext::RelPath::constant("native.texels"), vec![0; 4])],
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
        let dir = sfs::testing::scratch("ext-project-template");
        let _ = std::fs::remove_dir_all(&dir);
        project::create(&dir, &pe2_io::SystemFilesystem).unwrap();
        let p = super::Project::load(&dir).unwrap();
        let lock = project::lock::Lock::try_acquire(&p.root, "a test", &pe2_io::System).unwrap();
        let local = project::local::Local::open(&p, &lock).unwrap();
        let (mut history, _) = project::history::History::open(&local).unwrap();
        let index = project::index::Index::open(&p, &local).unwrap();
        // An operation passing on what it is given, and a template importing a text file through it.
        let group = "uid = \"uid://group\"\n\n[[stack]]\nop = \"pe2.graph@operation\"\nlabel = \"Passed\"\n\
                     ops.given = { op = \"input\", name = \"text\" }\nops.out = { op = \"output\", name = \"text\", from = { value = \"given\" } }\n";
        let template = "uid = \"uid://template\"\n\n[[stack]]\nop = \"pe2.graph@template\"\nlabel = \"Passed text\"\nfiles = [\"*.txt\"]\n\
                        makes = [{ op = \"file\", file = \"{file}\" }, { op = \"uid://group\" }, { op = \"output\", name = \"text\" }]\n";
        let (uid, tree) = (|text| pe2_path::Uid::new(text).unwrap(), |text| project::TreePath::new(text).unwrap());
        project::edit::create(
            &p,
            &index,
            pe2_path::Name::constant("project"),
            uid("uid://group"),
            tree("graph/passed"),
            (group, &[]),
            &mut history,
        )
        .unwrap();
        project::edit::create(
            &p,
            &index,
            pe2_path::Name::constant("project"),
            uid("uid://template"),
            tree("graph/template"),
            (template, &[]),
            &mut history,
        )
        .unwrap();
        let index = project::index::Index::open(&p, &local).unwrap();
        let extensions = super::for_project(&p);
        assert_eq!(extensions.take_definitions(&index), []);
        let offered: Vec<String> = extensions.templates_for("notes.TXT").into_iter().map(|t| t.label).collect();
        assert_eq!(offered, ["Passed text"]);
        let found = extensions.template("uid://template").unwrap();
        let stack = extensions.stamp(&found, "notes.txt").unwrap();
        assert!(stack.contains("file = \"notes.txt\""), "{stack}");
        let made = format!("uid = \"uid://made\"\n\n{stack}");
        let notes = pe2_path::RelPath::new("notes.txt").unwrap();
        let files: [(&pe2_path::RelPath, &[u8]); 1] = [(notes, b"hello")];
        project::edit::create(
            &p,
            &index,
            pe2_path::Name::constant("project"),
            uid("uid://made"),
            tree("notes"),
            (&made, &files),
            &mut history,
        )
        .unwrap();
        let index = project::index::Index::open(&p, &local).unwrap();
        let _ = extensions.take_definitions(&index);
        let engine = extensions.engine();
        let given = extensions
            .output(&engine, &index, &index.assets[tree("notes")], pe2_path::Asked::constant("text"), true)
            .unwrap();
        let project::graph::Value::Typed(typed) = &*given.value.unwrap() else { panic!("no file") };
        assert_eq!(typed.files[0].name.as_str(), "notes.txt");
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
