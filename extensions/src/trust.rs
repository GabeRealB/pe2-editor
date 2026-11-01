//! Which unsandboxed extensions may run. What is enabled is written with the user and never in a project, so that a project
//! cannot trust what it brings: `unsandboxed.toml` holds the extensions enabled one by one, by their hashes, and the places
//! the user trusts, by their real paths. An extension runs where it is enabled, or where it is:
//!
//! - in a place trusted (`pe2-cli project trust`): a project, a layer of one, a mount. What is there may change and goes on
//!   running, as what `make` runs does after a pull; a layer that moves is another place, and is asked about again.
//! - in the user's own extensions folder or beside the editor: what can write there can already run as the user.
//! - anywhere, for a run asked to trust everything (`--trust-everything`), which is written nowhere.
//!
//! A place is compared with where the file really is: what a layer above overrides is the file of that layer, and a link
//! out of a place is not in it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use project::ext::Origin;
use toml::{Table, Value};

use crate::data_folder;

/// This run trusts every extension.
static EVERYTHING: AtomicBool = AtomicBool::new(false);

/// Trust every extension for the rest of this process: nothing is kept.
pub fn trust_everything() {
    EVERYTHING.store(true, Ordering::Relaxed);
}

/// Whether this run trusts every extension.
#[must_use]
pub fn trusts_everything() -> bool {
    EVERYTHING.load(Ordering::Relaxed)
}

/// Places trusted for a project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trusted {
    /// The project, by its real path.
    pub project: PathBuf,
    /// Real paths.
    pub places: Vec<PathBuf>,
}

/// The file holding what the user enabled and trusts.
pub(crate) fn file() -> Option<PathBuf> {
    Some(data_folder()?.join("unsandboxed.toml"))
}

/// What the file holds.
pub(crate) fn read() -> Table {
    let text = file().and_then(|f| std::fs::read_to_string(f).ok()).unwrap_or_default();
    toml::from_str(&text).unwrap_or_default()
}

/// Write what the file holds.
pub(crate) fn write(table: &Table) -> std::io::Result<()> {
    let file = file().ok_or_else(|| std::io::Error::other("no data folder"))?;
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(file, toml::to_string(table).unwrap_or_default())
}

/// The places the user trusts.
#[must_use]
pub fn trusted() -> Vec<Trusted> {
    let table = read();
    let held = table.get("trusted").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    let path = |value: &Value| value.as_str().map(PathBuf::from);
    held.iter()
        .filter_map(|entry| {
            let project = entry.get("project").and_then(path)?;
            let places = entry.get("places").and_then(Value::as_array)?.iter().filter_map(path).collect();
            Some(Trusted { project, places })
        })
        .collect()
}

fn store(all: &[Trusted]) -> std::io::Result<()> {
    let text = |path: &Path| Value::String(path.display().to_string());
    let entry = |trusted: &Trusted| {
        let places = Value::Array(trusted.places.iter().map(|place| text(place)).collect());
        Value::Table(Table::from_iter([("project".to_string(), text(&trusted.project)), ("places".to_string(), places)]))
    };
    let mut table = read();
    table.remove("trusted");
    if !all.is_empty() {
        table.insert("trusted".to_string(), Value::Array(all.iter().map(entry).collect()));
    }
    write(&table)
}

/// Trust places for a project: what is in them runs from now on.
///
/// # Errors
/// One is not there, or what is trusted cannot be written.
pub fn trust(project: &Path, places: &[PathBuf]) -> std::io::Result<()> {
    let project = project.canonicalize()?;
    let places = places.iter().map(|place| place.canonicalize()).collect::<std::io::Result<Vec<_>>>()?;
    let mut all = trusted();
    let at = all.iter().position(|known| known.project == project).unwrap_or_else(|| {
        all.push(Trusted { project: project.clone(), places: Vec::new() });
        all.len() - 1
    });
    for place in places {
        if !all[at].places.contains(&place) {
            all[at].places.push(place);
        }
    }
    store(&all)
}

/// Stop trusting these places of a project, or, of none named, all of its.
///
/// # Errors
/// What is trusted cannot be written.
pub fn distrust(project: &Path, places: &[PathBuf]) -> std::io::Result<()> {
    let project = project.canonicalize().unwrap_or_else(|_| project.to_path_buf());
    let places: Vec<PathBuf> = places.iter().map(|place| place.canonicalize().unwrap_or_else(|_| place.clone())).collect();
    let mut all = trusted();
    for known in all.iter_mut().filter(|known| known.project == project) {
        known.places.retain(|place| !places.is_empty() && !places.contains(place));
    }
    all.retain(|known| !known.places.is_empty());
    store(&all)
}

/// Stop trusting a place, of whichever project: what the Extensions pane asks.
///
/// # Errors
/// What is trusted cannot be written.
pub fn distrust_place(place: &Path) -> std::io::Result<()> {
    let mut all = trusted();
    for known in &mut all {
        known.places.retain(|held| held != place);
    }
    all.retain(|known| !known.places.is_empty());
    store(&all)
}

/// Why an extension may run where it is, without being enabled by its hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Allowed {
    /// This run trusts everything.
    Everything,
    /// It is in the user's folder or beside the editor.
    Own,
    /// It is in a place trusted.
    Place(PathBuf),
}

impl Allowed {
    /// What the user is told.
    #[must_use]
    pub fn said(&self) -> String {
        match self {
            Allowed::Everything => "It runs because this run trusts everything (--trust-everything).".to_string(),
            Allowed::Own => "It runs because it is in your own extensions, or beside the editor.".to_string(),
            Allowed::Place(place) => format!("It runs because you trust {}.", place.display()),
        }
    }
}

/// Which of these places holds a file, where it really is.
fn holding<'a>(file: &Path, mut places: impl Iterator<Item = &'a PathBuf>) -> Option<PathBuf> {
    let real = file.canonicalize().ok()?;
    places.find(|place| real.starts_with(place)).cloned()
}

fn allowed(origin: &Origin, everything: bool, trusted: &[Trusted]) -> Option<Allowed> {
    if everything {
        return Some(Allowed::Everything);
    }
    match origin {
        Origin::User(_) | Origin::Install(_) => Some(Allowed::Own),
        Origin::Project { file, .. } | Origin::Component(file) => holding(file, trusted.iter().flat_map(|known| &known.places)).map(Allowed::Place),
        _ => None,
    }
}

/// Why an extension from here may run; none where it has to have been enabled.
#[must_use]
pub fn allowed_by(origin: &Origin) -> Option<Allowed> {
    allowed(origin, trusts_everything(), &trusted())
}

#[cfg(test)]
mod tests {
    use super::{Allowed, Origin, Trusted, allowed};
    use std::path::PathBuf;

    /// A project's layers in a folder, one holding an extension, with its real paths.
    fn places(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("pe2-trust-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for layer in ["mine", "mine_too", "theirs"] {
            std::fs::create_dir_all(root.join(layer).join("x.node")).unwrap();
            std::fs::write(root.join(layer).join("x.node/extension.wasm"), "").unwrap();
        }
        let root = root.canonicalize().unwrap();
        (root.clone(), root.join("mine"), root.join("theirs/x.node/extension.wasm"))
    }

    fn in_project(file: PathBuf) -> Origin {
        Origin::Project { asset: "x".to_string(), file }
    }

    #[test]
    fn a_place_holds_what_is_really_in_it() {
        let (root, mine, theirs) = places("place");
        let trusted = [Trusted { project: root.clone(), places: vec![mine.clone()] }];
        let there = allowed(&in_project(mine.join("x.node/extension.wasm")), false, &trusted);
        assert_eq!(there, Some(Allowed::Place(mine.clone())));
        // Another layer, and one whose name begins alike.
        assert_eq!(allowed(&in_project(theirs.clone()), false, &trusted), None);
        assert_eq!(allowed(&in_project(root.join("mine_too/x.node/extension.wasm")), false, &trusted), None);
        // A link in a place trusted, to what is not.
        #[cfg(unix)]
        {
            let link = mine.join("elsewhere.wasm");
            std::os::unix::fs::symlink(&theirs, &link).unwrap();
            assert_eq!(allowed(&in_project(link), false, &trusted), None);
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn what_is_the_users_own_needs_no_place_and_a_run_may_trust_everything() {
        let file = PathBuf::from("/nowhere/x.wasm");
        assert_eq!(allowed(&Origin::User(file.clone()), false, &[]), Some(Allowed::Own));
        assert_eq!(allowed(&Origin::Install(file.clone()), false, &[]), Some(Allowed::Own));
        assert_eq!(allowed(&in_project(file.clone()), false, &[]), None);
        assert_eq!(allowed(&in_project(file), true, &[]), Some(Allowed::Everything));
    }
}
