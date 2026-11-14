//! What tests share: places written in a test, checked as the program's are (the `testing` feature, for
//! dev-dependencies alone).

use crate::{HostPath, RelPath};

/// A place within a folder, by a path written in a test: checked as the program's paths are, joined in the system's
/// own separators.
pub trait At {
    fn at(&self, path: &str) -> HostPath;
}

impl At for HostPath {
    fn at(&self, path: &str) -> HostPath {
        self.join(&RelPath::new(path).unwrap_or_else(|e| panic!("a test's path: {e}")))
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
