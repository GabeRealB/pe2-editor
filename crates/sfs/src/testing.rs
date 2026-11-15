//! What tests share: places written in a test, checked as the program's are (the `testing` feature, for
//! dev-dependencies alone).

#![allow(clippy::disallowed_methods, reason = "a test's plain path is joined of checked names")]

use crate::HostPath;
use pe2_path::RelPath;

/// A place within a folder, by a path written in a test: checked as the program's paths are, joined in the system's
/// own separators - a storage's place within one of its, a plain path within a folder of the program's own.
pub trait At {
    type Place;
    fn at(&self, path: &str) -> Self::Place;
}

/// A test's path, checked.
fn checked(path: &str) -> &RelPath {
    RelPath::new(path).unwrap_or_else(|e| panic!("a test's path: {e}"))
}

impl At for HostPath {
    type Place = HostPath;
    fn at(&self, path: &str) -> HostPath {
        self.join(checked(path))
    }
}

impl At for std::path::Path {
    type Place = std::path::PathBuf;
    fn at(&self, path: &str) -> std::path::PathBuf {
        checked(path).parts().fold(self.to_path_buf(), |place, name| place.join(name))
    }
}

/// A folder of a test's own under the system's temporary folder, `pe2-<name>-<process>`: nothing there yet.
///
/// # Panics
/// The system's temporary folder cannot be found.
#[must_use]
pub fn scratch(name: &str) -> HostPath {
    let temporary = HostPath::new(&std::env::temp_dir()).expect("the system's temporary folder");
    let folder = temporary.at(&format!("pe2-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&folder);
    folder
}
