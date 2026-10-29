//! The extensions the editor ships: part of its binaries, loaded for every project, versioned with them. Each is a
//! crate beside this one, built as a component by `build.rs` and embedded here; a project names those it needs, and
//! never carries a copy.
//!
//! The same crates are linked in too: `PE2_EXTENSIONS=linked` runs them in the host instead of their components, to
//! step through one in a debugger or compare the two.

use project::Project;
use project::ext::{EXTENSION_COMPONENT, EXTENSION_KIND, Extensions, Loadable, Origin, Present, Status, component};
use project::index::Index;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

include!(concat!(env!("OUT_DIR"), "/components.rs"));

/// The built-ins' components, by package: what loading reads.
#[must_use]
pub fn components() -> &'static [(&'static str, &'static [u8])] {
    COMPONENTS
}

/// The built-in extensions linked in.
#[must_use]
pub fn linked() -> Vec<pe2_ext::Extension> {
    vec![pe2_formats::extension(), pe2_game::extension(), pe2_images::extension()]
}

/// Load the built-ins: their components, or with `PE2_EXTENSIONS=linked` their crates.
#[must_use]
pub fn load() -> Extensions {
    load_without(&[], &[])
}

/// A component found beside the built-ins: in the project (an `extension` asset) or the user's folder.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Found {
    pub origin: Origin,
    pub file: PathBuf,
    /// What tells this version of it from another: its hash where the index knows it, else its size and time.
    pub stamp: String,
}

/// Load the components found - the project's first, then the user's: one of an id takes the place of those after it -
/// and the built-ins, but those a project leaves off (`disabled`, by id) and those requiring them.
#[must_use]
pub fn load_without(found: &[Found], disabled: &[String]) -> Extensions {
    if std::env::var_os("PE2_EXTENSIONS").is_some_and(|v| v == "linked") {
        return Extensions::load_without(linked().into_iter().map(Into::into).collect(), true, disabled);
    }
    let mut loadable: Vec<Loadable> = vec![];
    let mut failed = vec![];
    // Those that cannot be loaded at all, present as refused: named by where they are, as who they say they are may
    // not be readable, and is no claim on an id.
    let mut unloadable = vec![];
    for found in found {
        let loaded = std::fs::read(&found.file).map_err(|e| format!("it cannot be read: {e}")).and_then(|bytes| {
            if !bytes.starts_with(b"\0asm") {
                return Err("it is not a WebAssembly component".to_string());
            }
            let name = found.origin.said();
            // The first line of what went wrong: the parser's is followed by the bytes it found.
            component::load_bytes(&name, &bytes, found.origin.clone()).map_err(|e| {
                let why = format!("{e:#}");
                why.lines().next().unwrap_or_default().trim_end_matches(['[', ' ', '=']).to_string()
            })
        });
        match loaded {
            Ok(extension) => loadable.push(extension),
            Err(e) => {
                failed.push(format!("{}: {e}", found.origin.said()));
                // Named by where it is: the asset's path, the file's name.
                let name = match &found.origin {
                    Origin::Project { asset, .. } => asset.clone(),
                    _ => found.file.file_name().map_or_default(|n| n.to_string_lossy().into_owned()),
                };
                unloadable.push(Present {
                    id: name,
                    version: String::new(),
                    origin: found.origin.clone(),
                    requires: vec![],
                    status: Status::Refused(e),
                    size: std::fs::metadata(&found.file).ok().and_then(|m| usize::try_from(m.len()).ok()),
                    load: None,
                    registers: vec![],
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
    let mut extensions = Extensions::load_without(loadable, true, disabled);
    extensions.problems.extend(failed);
    extensions.present.extend(unloadable);
    extensions
}

/// The built-ins, loaded once a process.
#[must_use]
pub fn shared() -> Arc<Extensions> {
    loaded(&[], &[])
}

/// The extensions as a project has them: its own (`extension` assets, where the index is given), the user's, the
/// built-ins, but those it leaves off - loaded once a process for each such set, so a changed component loads again.
#[must_use]
pub fn for_project(project: &Project, index: Option<&Index>) -> Arc<Extensions> {
    loaded(&found(index), &project.disabled_extensions)
}

/// The components found beside the built-ins: the project's (where the index is given), then the user's. Two
/// findings differ where a component came, went or changed: the extensions are loaded again.
#[must_use]
pub fn found(index: Option<&Index>) -> Vec<Found> {
    let mut found = index.map_or_default(project_components);
    found.extend(user_components());
    found
}

/// What a set of extensions is loaded for: the components found and the ids left off.
type Key = (Vec<Found>, Vec<String>);

/// The sets loaded, by what for.
type Sets = Vec<(Key, Arc<Extensions>)>;

/// The sets loaded lately, newest last: one a component replaces is dropped in time, its engines with it.
const KEPT: usize = 4;

fn loaded(found: &[Found], disabled: &[String]) -> Arc<Extensions> {
    static LOADED: OnceLock<Mutex<Sets>> = OnceLock::new();
    let mut disabled = disabled.to_vec();
    disabled.sort_unstable();
    disabled.dedup();
    let key = (found.to_vec(), disabled);
    let loaded = LOADED.get_or_init(Mutex::default);
    let mut loaded = loaded.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let extensions = match loaded.iter().position(|(k, _)| *k == key) {
        Some(at) => loaded.remove(at).1,
        None => Arc::new(load_without(&key.0, &key.1)),
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
        .filter(|a| a.kind.as_deref() == Some(EXTENSION_KIND))
        .filter_map(|a| {
            let file = a.files.iter().find(|f| f.name == EXTENSION_COMPONENT)?;
            let origin = Origin::Project { asset: a.path.clone(), file: file.path.clone() };
            Some(Found { origin, file: file.path.clone(), stamp: file.sha1.clone() })
        })
        .collect()
}

/// The user's own extensions folder: `extensions` in the editor's data folder (as eframe places it: the XDG data folder
/// on Linux, Application Support on macOS, the roaming application data on Windows).
#[must_use]
pub fn user_folder() -> Option<PathBuf> {
    let data = if cfg!(windows) {
        std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join("pe2-editor").join("data"))
    } else if cfg!(target_os = "macos") {
        std::env::home_dir().map(|p| p.join("Library/Application Support/pe2-editor"))
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::home_dir().map(|p| p.join(".local/share")))
            .map(|p| p.join("pe2-editor"))
    };
    Some(data?.join("extensions"))
}

/// The components in the user's folder (`*.wasm`), in name order.
#[must_use]
pub fn user_components() -> Vec<Found> {
    let Some(folder) = user_folder() else { return vec![] };
    let mut files: Vec<PathBuf> = std::fs::read_dir(&folder)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "wasm"))
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|file| {
            let meta = std::fs::metadata(&file).ok();
            let time = meta.as_ref().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
            let stamp = format!("{} {}", meta.map_or(0, |m| m.len()), time.map_or(0, |t| t.as_nanos()));
            Found { origin: Origin::User(file.clone()), file, stamp }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{COMPONENTS, linked};
    use project::ext::component;

    #[test]
    fn each_component_says_who_it_is_without_running() {
        assert_eq!(COMPONENTS.len(), linked().len());
        for (package, bytes) in COMPONENTS {
            let about = component::about(bytes).unwrap_or_else(|| panic!("{package}: no section"));
            let declared = linked().into_iter().find(|e| e.id == about.id).unwrap_or_else(|| panic!("{}: not linked", about.id));
            assert_eq!(about.version, declared.version, "{package}");
            assert_eq!(about.requires, declared.requires, "{package}");
        }
    }
}
