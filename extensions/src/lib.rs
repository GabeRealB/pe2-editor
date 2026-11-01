//! The extensions the editor ships: part of its binaries, loaded for every project, versioned with them. Each is a
//! crate beside this one, built as a component by `build.rs` and embedded here; a project names those it needs, and
//! never carries a copy.
//!
//! The same crates are linked in too: `PE2_EXTENSIONS=linked` runs them in the host instead of their components, to
//! step through one in a debugger or compare the two.

use project::{Imported, Manifest, Project};
use project::ext::component::Unsandboxed;
use project::ext::{EXTENSION_COMPONENT, Extensions, Loadable, Origin, Present, Status, component, unsandboxed};
use project::index::Index;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use toml::{Table, Value};
use std::sync::{Arc, Mutex, OnceLock};

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

/// The built-in extensions linked in.
#[must_use]
pub fn linked() -> Vec<pe2_ext::Extension> {
    vec![pe2_render::extension(), pe2_events::extension(), pe2_formats::extension(), pe2_game::extension(), pe2_images::extension(), pe2_movies::extension(), pe2_packages::extension(), pe2_str::extension()]
}

/// Load the built-ins: their components, or with `PE2_EXTENSIONS=linked` their crates.
#[must_use]
pub fn load() -> Extensions {
    load_without(&[], &[], &Kept::new(), None)
}

/// The settings kept for extensions, by id: each a table by scope (`{ project = { ... }, user = { ... } }`).
pub type Kept = BTreeMap<String, Table>;

/// A component found beside the built-ins: in the project (an `extension` asset) or the user's folder.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Found {
    pub origin: Origin,
    pub file: PathBuf,
    /// The folder it is in with what it ships, where it is in one of its own.
    pub folder: Option<PathBuf>,
    /// What tells this version of it from another: its hash where the index knows it, else its size and time - and
    /// what the user enabled, as that changes what it runs as.
    pub stamp: String,
}

/// Load the components found - the project's first, then the user's: one of an id takes the place of those after it -
/// and the built-ins, but those a project leaves off (`disabled`, by id) and those requiring them.
#[must_use]
pub fn load_without(found: &[Found], disabled: &[String], kept: &Kept, manifest: Option<&Manifest>) -> Extensions {
    let with_settings = |mut loadable: Vec<Loadable>| {
        for l in &mut loadable {
            l.settings = kept.get(&l.id).cloned().unwrap_or_default();
        }
        loadable
    };
    if std::env::var_os("PE2_EXTENSIONS").is_some_and(|v| v == "linked") {
        return Extensions::load_without(with_settings(linked().into_iter().map(Into::into).collect()), true, disabled);
    }
    let mut loadable: Vec<Loadable> = vec![];
    let mut failed = vec![];
    // Those that cannot be loaded at all, present as refused: named by where they are, as who they say they are may
    // not be readable, and is no claim on an id.
    let mut unloadable = vec![];
    let enabled = enabled_unsandboxed();
    for found in found {
        let loaded = std::fs::read(&found.file).map_err(|e| format!("it cannot be read: {e}")).and_then(|bytes| {
            if !bytes.starts_with(b"\0asm") {
                return Err("it is not a WebAssembly component".to_string());
            }
            let about = component::about(&bytes);
            if let Some(why) = about.as_ref().and_then(|a| unsandboxed::elsewhere(&a.platforms)) {
                return Err(why);
            }
            // What is kept apart from a project is for it to take in.
            if let Some(about) = about.as_ref().filter(|about| manifest.is_some_and(|m| !taken_in(m, &about.id, &found.origin))) {
                unloadable.push(available(about.clone(), found, &bytes));
                return Ok(None);
            }
            let name = found.origin.said();
            // Unsandboxed, it runs only as the user trusts it; until then, it is read for who it is.
            let held = match about.filter(|a| a.unsandboxed) {
                Some(about) => {
                    let trust = unsandboxed::trust(&found.file, found.folder.as_deref()).map_err(|e| format!("{e:#}"))?;
                    if !enabled.contains_key(&trust) && trust::allowed_by(&found.origin).is_none() {
                        unloadable.push(not_enabled(about, found, &bytes, trust, &enabled));
                        return Ok(None);
                    }
                    Some(Unsandboxed { folder: found.folder.clone(), trust })
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
                failed.push(format!("{}: {e}", found.origin.said()));
                // Named by where it is: the asset's path, the folder's name, the file's.
                let name = match (&found.origin, &found.folder) {
                    (Origin::Project { asset, .. }, _) => asset.clone(),
                    (_, Some(folder)) => folder.file_name().map_or_default(|n| n.to_string_lossy().into_owned()),
                    _ => found.file.file_name().map_or_default(|n| n.to_string_lossy().into_owned()),
                };
                unloadable.push(Present {
                    id: name,
                    version: String::new(),
                    origin: found.origin.clone(),
                    requires: vec![],
                    weak: vec![],
                    optional: vec![],
                    provides: vec![],
                    linked: vec![],
                    status: Status::Refused(e),
                    size: std::fs::metadata(&found.file).ok().and_then(|m| usize::try_from(m.len()).ok()),
                    contract: None,
                    load: None,
                    registers: vec![],
                    trust: None,
                });
            }
        }
    }
    for (package, bytes) in COMPONENTS {
        match component::load_bytes(package, bytes, Origin::Builtin) {
            Ok(extension) => loadable.push(extension),
            Err(e) => failed.push(format!("{package}: {e:#}")),
        }
    }
    let mut extensions = Extensions::load_without(with_settings(loadable), true, disabled);
    extensions.problems.extend(failed);
    extensions.present.extend(unloadable);
    if let Some(manifest) = manifest {
        extensions.unmet = unmet(manifest, &extensions.present);
        extensions.unlocked = unlocked(manifest, found);
    }
    extensions
}

/// Whether a project takes in an extension found here: what it holds itself, by being there; from elsewhere, as it says.
fn taken_in(manifest: &Manifest, id: &str, origin: &Origin) -> bool {
    match origin {
        Origin::User(_) | Origin::Install(_) => manifest.imported.iter().any(|imported| imported.id == id && imported.from.is_none()),
        _ => true,
    }
}

/// What a project requires and does not have, and why.
fn unmet(manifest: &Manifest, present: &[Present]) -> Vec<(String, String)> {
    let required = manifest.imported.iter().filter(|imported| imported.required);
    let why = |imported: &Imported| {
        let found = present.iter().find(|p| p.id == imported.id && !matches!(p.status, Status::Replaced(_) | Status::Available));
        let Some(found) = found else { return Some("it was not found".to_string()) };
        match &found.status {
            Status::Loaded => match &imported.version {
                Some(version) if !project::ext::satisfies(&found.version, version) => Some(format!("it is {}, and it asks for {version}", found.version)),
                _ => None,
            },
            Status::Disabled => Some("the project leaves it off".to_string()),
            Status::NotEnabled { .. } => Some("it runs outside the sandbox and is not trusted".to_string()),
            Status::Refused(why) => Some(why.clone()),
            Status::Replaced(_) | Status::Available => None,
        }
    };
    required.filter_map(|imported| Some((imported.id.clone(), why(imported)?))).collect()
}

/// What is no longer what the project took in, of what it took in from outside itself.
fn unlocked(manifest: &Manifest, found: &[Found]) -> Vec<String> {
    let differs = |(id, locked): (&String, &String)| {
        let now = found.iter().find_map(|found| {
            let bytes = std::fs::read(&found.file).ok()?;
            let about = component::about(&bytes)?;
            (about.id == *id && !matches!(found.origin, Origin::Project { .. })).then(|| unsandboxed::trust(&found.file, found.folder.as_deref()).ok())?
        });
        now.is_some_and(|now| now != *locked).then(|| id.clone())
    };
    manifest.locked.iter().filter_map(differs).collect()
}

/// What each extension from outside a project is now, by id, as `extensions.lock` keeps it.
#[must_use]
pub fn locked(project: &Project, index: Option<&Index>) -> BTreeMap<String, String> {
    let found = found(project, index);
    let of = |found: &Found| {
        if matches!(found.origin, Origin::Project { .. }) {
            return None;
        }
        let about = component::about(&std::fs::read(&found.file).ok()?)?;
        taken_in(&project.manifest, &about.id, &found.origin).then_some(())?;
        project.manifest.imported.iter().any(|imported| imported.id == about.id).then_some(())?;
        Some((about.id, unsandboxed::trust(&found.file, found.folder.as_deref()).ok()?))
    };
    found.iter().filter_map(of).collect()
}

/// An extension the project has not taken in, as it is shown: read without running anything.
fn available(about: component::About, found: &Found, bytes: &[u8]) -> Present {
    Present {
        status: Status::Available,
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
        trust: None,
    }
}

/// An unsandboxed extension not run, as it is shown: who it says it is, read without running anything.
fn not_enabled(about: component::About, found: &Found, bytes: &[u8], trust: String, enabled: &BTreeMap<String, String>) -> Present {
    Present {
        status: Status::NotEnabled { changed: enabled.values().any(|id| *id == about.id) },
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
        trust: Some(trust),
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

/// The extensions as a project has them: its own (`extension` assets, where the index is given), the user's, the
/// built-ins, but those it leaves off - loaded once a process for each such set, so a changed component loads again.
#[must_use]
pub fn for_project(project: &Project, index: Option<&Index>) -> Arc<Extensions> {
    loaded(&found(project, index), &project.disabled_extensions, &kept(project), Some(&project.manifest))
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

/// The components a project takes in from places of its own choosing.
#[must_use]
pub fn imported_components(project: &Project) -> Vec<Found> {
    let placed = project.manifest.imported.iter().filter_map(|imported| Some(project.imported_from(imported.from.as_ref()?)));
    placed.filter_map(|place| component_at(place, Origin::Component)).collect()
}

/// The components found beside the built-ins: the project's (where the index is given), the user's, those beside the
/// editor. Two findings differ where a component came, went or changed, or what the user enabled did: the extensions
/// are loaded again.
#[must_use]
pub fn found(project: &Project, index: Option<&Index>) -> Vec<Found> {
    let mut found = index.map_or_default(project_components);
    found.extend(imported_components(project));
    found.extend(user_components());
    found.extend(install_components());
    let enabled = trust::file().and_then(|f| std::fs::read_to_string(f).ok()).unwrap_or_default();
    for found in &mut found {
        found.stamp.push_str(&enabled);
    }
    found
}

/// What a set of extensions is loaded for: the components found, the ids left off and the settings kept (as TOML).
type Key = (Vec<Found>, Vec<String>, String, Option<Manifest>);

/// The sets loaded, by what for.
type Sets = Vec<(Key, Arc<Extensions>)>;

/// The sets loaded lately, newest last: one a component replaces is dropped in time, its engines with it.
const KEPT: usize = 4;

fn loaded(found: &[Found], disabled: &[String], kept: &Kept, manifest: Option<&Manifest>) -> Arc<Extensions> {
    static LOADED: OnceLock<Mutex<Sets>> = OnceLock::new();
    let mut disabled = disabled.to_vec();
    disabled.sort_unstable();
    disabled.dedup();
    let key = (found.to_vec(), disabled, toml::to_string(kept).unwrap_or_default(), manifest.cloned());
    let loaded = LOADED.get_or_init(Mutex::default);
    let mut loaded = loaded.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let extensions = match loaded.iter().position(|(k, _)| *k == key) {
        Some(at) => loaded.remove(at).1,
        None => Arc::new(load_without(&key.0, &key.1, kept, manifest)),
    };
    loaded.push((key, extensions.clone()));
    if loaded.len() > KEPT {
        loaded.remove(0);
    }
    extensions
}

/// The project's extensions: its `extension` assets' components, in path order.
#[must_use]
pub fn project_components(index: &Index) -> Vec<Found> {
    index
        .assets
        .values()
        .filter(|a| a.files.iter().any(|file| file.name == EXTENSION_COMPONENT))
        .filter_map(|a| {
            let file = a.files.iter().find(|f| f.name == EXTENSION_COMPONENT)?;
            let origin = Origin::Project { asset: a.path.clone(), file: file.path.clone() };
            let stamp = a.files.iter().map(|f| f.sha1.as_str()).collect::<Vec<_>>().join(" ");
            Some(Found { origin, file: file.path.clone(), folder: file.path.parent().map(PathBuf::from), stamp })
        })
        .collect()
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
    let meta = std::fs::metadata(file).ok();
    let time = meta.as_ref().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
    format!("{} {}", meta.map_or(0, |m| m.len()), time.map_or(0, |t| t.as_nanos()))
}

/// The stamps of what a folder holds, below it too.
fn folder_stamp(folder: &std::path::Path) -> String {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(folder).into_iter().flatten().flatten().map(|e| e.path()).collect();
    entries.sort();
    entries.iter().map(|e| if e.is_dir() { folder_stamp(e) } else { file_stamp(e) }).collect::<Vec<_>>().join(" ")
}

/// The components in an extensions folder, in name order: each a `*.wasm`, or a folder holding an `extension.wasm`
/// and what it ships.
fn components_in(folder: &std::path::Path, origin: fn(PathBuf) -> Origin) -> Vec<Found> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(folder).into_iter().flatten().flatten().map(|e| e.path()).collect();
    entries.sort();
    entries.into_iter().filter_map(|entry| component_at(entry, origin)).collect()
}

/// The component at a place: a `*.wasm`, or a folder holding an `extension.wasm` and what it ships.
fn component_at(entry: PathBuf, origin: fn(PathBuf) -> Origin) -> Option<Found> {
    if entry.is_dir() {
        let file = entry.join(EXTENSION_COMPONENT);
        let stamp = folder_stamp(&entry);
        return file.is_file().then(|| Found { origin: origin(file.clone()), file, folder: Some(entry), stamp });
    }
    let wasm = entry.extension().is_some_and(|e| e == "wasm");
    wasm.then(|| Found { origin: origin(entry.clone()), stamp: file_stamp(&entry), file: entry, folder: None })
}

/// The components in the user's folder.
#[must_use]
pub fn user_components() -> Vec<Found> {
    user_folder().map_or_default(|folder| components_in(&folder, Origin::User))
}

/// The components beside the editor.
#[must_use]
pub fn install_components() -> Vec<Found> {
    install_folder().map_or_default(|folder| components_in(&folder, Origin::Install))
}

#[cfg(test)]
mod tests {
    use super::{COMPONENTS, linked};
    use project::ext::component;

    #[test]
    fn components_are_held_to_the_contract_this_editor_runs() {
        let wit = include_str!("../../crates/ext/wit/extension.wit");
        assert!(wit.contains(&format!("package pe2:extension@{};", component::CONTRACT)), "the WIT package is the contract");
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
        let ats: Vec<usize> = bytes.windows(name.len()).enumerate().filter(|(_, w)| *w == name.as_bytes()).map(|(at, _)| at).collect();
        assert!(!ats.is_empty(), "its export's name");
        for at in ats {
            older[at..at + name.len()].copy_from_slice(format!("pe2:extension/guest@{before}.0").as_bytes());
        }
        let refused = component::load_bytes("older", &older, project::ext::Origin::Builtin).err().expect("refused");
        assert!(format!("{refused:#}").contains("rebuild it with this editor's SDK"), "{refused:#}");
    }

    /// A built-in's component, as a file in a folder of its own, found as being in a user's extensions.
    fn users(id: &str, name: &str) -> (std::path::PathBuf, super::Found) {
        let dir = std::env::temp_dir().join(format!("pe2-ext-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let bytes = COMPONENTS.iter().find(|(package, _)| *package == id).expect("a built-in").1;
        let file = dir.join("x.wasm");
        std::fs::write(&file, bytes).unwrap();
        let found = super::Found { origin: super::Origin::User(file.clone()), file, folder: None, stamp: String::new() };
        (dir, found)
    }

    #[test]
    fn what_is_kept_apart_loads_only_as_a_project_takes_it_in() {
        use project::{Imported, Manifest};
        use project::ext::{Origin, Status};
        let (dir, found) = users("pe2-events", "takes");
        let id = component::about(&std::fs::read(&found.file).unwrap()).unwrap().id;
        let status = |extensions: &project::ext::Extensions| {
            let user = |p: &&project::ext::Present| matches!(p.origin, Origin::User(_));
            extensions.present.iter().find(user).map(|p| p.status.clone())
        };
        let found = [found];
        // No project: what is the user's is there.
        assert_eq!(status(&super::load_without(&found, &[], &super::Kept::new(), None)), Some(Status::Loaded));
        // Not taken in.
        let bare = Manifest::default();
        assert_eq!(status(&super::load_without(&found, &[], &super::Kept::new(), Some(&bare))), Some(Status::Available));
        // Taken in, in place of the built-in of its id; asked for at another version; locked to something else.
        let imported = Imported { id: id.clone(), required: true, version: Some("^7.0".to_string()), from: None };
        let locked = [(id.clone(), "another".to_string())].into();
        let manifest = Manifest { imported: vec![imported], locked };
        let extensions = super::load_without(&found, &[], &super::Kept::new(), Some(&manifest));
        assert_eq!(status(&extensions), Some(Status::Loaded));
        assert_eq!(extensions.unmet.len(), 1, "{:?}", extensions.unmet);
        assert!(extensions.unmet[0].1.contains("asks for ^7.0"), "{:?}", extensions.unmet);
        assert_eq!(extensions.unlocked, std::slice::from_ref(&id));
        // Required and found nowhere.
        let manifest = Manifest { imported: vec![Imported { id: "my.tool".to_string(), required: true, ..Imported::default() }], ..Manifest::default() };
        let extensions = super::load_without(&[], &[], &super::Kept::new(), Some(&manifest));
        assert_eq!(extensions.unmet, [("my.tool".to_string(), "it was not found".to_string())]);
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
    fn each_component_says_who_it_is_without_running() {
        assert_eq!(COMPONENTS.len(), linked().len());
        for (package, bytes) in COMPONENTS {
            let about = component::about(bytes).unwrap_or_else(|| panic!("{package}: no section"));
            let declared = linked().into_iter().find(|e| e.id == about.id).unwrap_or_else(|| panic!("{}: not linked", about.id));
            assert_eq!(about.version, declared.version, "{package}");
            assert_eq!(about.requires, declared.requires, "{package}");
            assert_eq!(about.weak, declared.weak, "{package}");
            assert_eq!(about.optional, declared.optional, "{package}");
        }
    }
}
