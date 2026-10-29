//! The extensions the editor ships: part of its install, loaded for every project, versioned with it. Each is a crate
//! beside this one, linked in for now; a project names those it needs, and never carries a copy.

/// The built-in extensions.
#[must_use]
pub fn builtin() -> Vec<pe2_ext::Extension> {
    vec![pe2_images::extension()]
}
