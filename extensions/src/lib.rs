//! The extensions the editor ships: part of its binaries, loaded for every project, versioned with them. Each is a
//! crate beside this one, built as a component by `build.rs` and embedded here; a project names those it needs, and
//! never carries a copy.
//!
//! The same crates are linked in too: `PE2_EXTENSIONS=linked` runs them in the host instead of their components, to
//! step through one in a debugger or compare the two.

use project::ext::{Extensions, Loadable, Origin, component};
use std::sync::{Arc, OnceLock};

include!(concat!(env!("OUT_DIR"), "/components.rs"));

/// The built-in extensions linked in.
#[must_use]
pub fn linked() -> Vec<pe2_ext::Extension> {
    vec![pe2_game::extension(), pe2_images::extension()]
}

/// Load the built-ins: their components, or with `PE2_EXTENSIONS=linked` their crates.
#[must_use]
pub fn load() -> Extensions {
    if std::env::var_os("PE2_EXTENSIONS").is_some_and(|v| v == "linked") {
        return Extensions::load(linked().into_iter().map(Into::into).collect(), true);
    }
    let mut loadable: Vec<Loadable> = vec![];
    let mut failed = vec![];
    for (package, bytes) in COMPONENTS {
        match component::load_bytes(package, bytes, Origin::Builtin) {
            Ok(extension) => loadable.push(extension),
            Err(e) => failed.push(format!("{package}: {e:#}")),
        }
    }
    let mut extensions = Extensions::load(loadable, true);
    extensions.problems.extend(failed);
    extensions
}

/// The built-ins, loaded once a process.
pub fn shared() -> Arc<Extensions> {
    static SHARED: OnceLock<Arc<Extensions>> = OnceLock::new();
    SHARED.get_or_init(|| Arc::new(load())).clone()
}
