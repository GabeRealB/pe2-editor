//! The extensions the editor ships: part of its binaries, loaded for every project, versioned with them. Each is a
//! crate beside this one, built as a component by `build.rs` and embedded here; a project names those it needs, and
//! never carries a copy.
//!
//! The same crates are linked in too: `PE2_EXTENSIONS=linked` runs them in the host instead of their components, to
//! step through one in a debugger or compare the two.

use project::ext::{Extensions, Loadable, Origin, component};
use std::collections::HashMap;
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
    load_without(&[])
}

/// Load the built-ins but those a project leaves off (`disabled`, by id), and those requiring them.
#[must_use]
pub fn load_without(disabled: &[String]) -> Extensions {
    if std::env::var_os("PE2_EXTENSIONS").is_some_and(|v| v == "linked") {
        return Extensions::load_without(linked().into_iter().map(Into::into).collect(), true, disabled);
    }
    let mut loadable: Vec<Loadable> = vec![];
    let mut failed = vec![];
    for (package, bytes) in COMPONENTS {
        match component::load_bytes(package, bytes, Origin::Builtin) {
            Ok(extension) => loadable.push(extension),
            Err(e) => failed.push(format!("{package}: {e:#}")),
        }
    }
    let mut extensions = Extensions::load_without(loadable, true, disabled);
    extensions.problems.extend(failed);
    extensions
}

/// The built-ins, loaded once a process.
#[must_use]
pub fn shared() -> Arc<Extensions> {
    for_project(&[])
}

/// The built-ins as a project has them - but those it leaves off - loaded once a process for each such set.
#[must_use]
pub fn for_project(disabled: &[String]) -> Arc<Extensions> {
    static LOADED: OnceLock<Mutex<HashMap<Vec<String>, Arc<Extensions>>>> = OnceLock::new();
    let mut key = disabled.to_vec();
    key.sort_unstable();
    key.dedup();
    let loaded = LOADED.get_or_init(Mutex::default);
    let mut loaded = loaded.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    loaded.entry(key.clone()).or_insert_with(|| Arc::new(load_without(&key))).clone()
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
