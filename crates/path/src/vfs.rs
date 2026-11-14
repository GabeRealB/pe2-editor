//! A path within a scope of the project's filesystem, typed by the kind of scope it is in.

use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;

use crate::error::PathError;
use crate::name::{Name, RelPath};

/// A kind of scope of the filesystem: what a [`VfsPath`] is within. Whoever holds a scope knows where it is; a path
/// says only that it is within one of its kind.
pub trait Scope: 'static {
    /// What the kind is called, in messages.
    const KIND: &'static str;
}

/// The project's tree, its layers resolved: what the project serves as its folder, and what its layers name their
/// nodes and files by.
pub enum Tree {}

/// The folder a `run` box lays out for a program: what it takes (`self/src/a.c`, `dep/<name>/...`) and makes.
pub enum Run {}

/// The files a program is given, served for it alone.
pub enum Given {}

impl Scope for Tree {
    const KIND: &'static str = "the project's tree";
}

impl Scope for Run {
    const KIND: &'static str = "a run's folder";
}

impl Scope for Given {
    const KIND: &'static str = "a program's given files";
}

/// A path within a scope of kind `S`: a [`RelPath`] the compiler will not take for a path of another kind of scope.
pub struct VfsPath<S: Scope> {
    path: RelPath,
    scope: PhantomData<fn() -> S>,
}

impl<S: Scope> VfsPath<S> {
    /// The scope itself.
    #[must_use]
    pub fn root() -> VfsPath<S> {
        VfsPath::from(RelPath::root())
    }

    /// `text` as a path within a scope of this kind.
    ///
    /// # Errors
    /// It is no path (`RelPath::new`).
    pub fn new(text: &str) -> Result<VfsPath<S>, PathError> {
        RelPath::new(text).map(VfsPath::from)
    }

    /// The same names, within another kind of scope: for where one scope is laid out of another.
    #[must_use]
    pub fn into_scope<T: Scope>(self) -> VfsPath<T> {
        VfsPath::from(self.path)
    }

    /// Its names, as a relative path.
    #[must_use]
    pub fn rel(&self) -> &RelPath {
        &self.path
    }

    /// Whether it is the scope itself.
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.path.is_root()
    }

    /// Its names, in order.
    pub fn parts(&self) -> impl DoubleEndedIterator<Item = &str> {
        self.path.parts()
    }

    /// Its last name, unless it is the scope itself.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.path.name()
    }

    /// What it is in, unless it is the scope itself.
    #[must_use]
    pub fn parent(&self) -> Option<VfsPath<S>> {
        self.path.parent().map(VfsPath::from)
    }

    /// A name within it.
    #[must_use]
    pub fn join(&self, name: &Name) -> VfsPath<S> {
        VfsPath::from(self.path.join(name))
    }

    /// A path within it.
    #[must_use]
    pub fn join_path(&self, path: &RelPath) -> VfsPath<S> {
        VfsPath::from(self.path.join_path(path))
    }

    /// Whether it is `prefix` or within it, name by name.
    #[must_use]
    pub fn starts_with(&self, prefix: &VfsPath<S>) -> bool {
        self.path.starts_with(&prefix.path)
    }

    /// What of it is within `prefix`, if it is `prefix` or within it.
    #[must_use]
    pub fn strip_prefix(&self, prefix: &VfsPath<S>) -> Option<RelPath> {
        self.path.strip_prefix(&prefix.path)
    }

    /// Its text, names joined by `/`: for writing it out where it leaves the program as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.path.as_str()
    }
}

impl<S: Scope> crate::Names for VfsPath<S> {
    fn names(&self) -> impl Iterator<Item = &str> {
        self.path.parts()
    }
}

impl<S: Scope> From<RelPath> for VfsPath<S> {
    fn from(path: RelPath) -> VfsPath<S> {
        VfsPath { path, scope: PhantomData }
    }
}

impl<S: Scope> From<Name> for VfsPath<S> {
    fn from(name: Name) -> VfsPath<S> {
        VfsPath::from(RelPath::from(name))
    }
}

impl<S: Scope> Clone for VfsPath<S> {
    fn clone(&self) -> Self {
        VfsPath::from(self.path.clone())
    }
}

impl<S: Scope> PartialEq for VfsPath<S> {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path
    }
}

impl<S: Scope> Eq for VfsPath<S> {}

impl<S: Scope> PartialOrd for VfsPath<S> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<S: Scope> Ord for VfsPath<S> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.path.cmp(&other.path)
    }
}

impl<S: Scope> Hash for VfsPath<S> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.path.hash(state);
    }
}

impl<S: Scope> Default for VfsPath<S> {
    fn default() -> Self {
        VfsPath::root()
    }
}

impl<S: Scope> fmt::Display for VfsPath<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.path, f)
    }
}

impl<S: Scope> fmt::Debug for VfsPath<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} (in {})", self.path, S::KIND)
    }
}

impl<S: Scope> std::str::FromStr for VfsPath<S> {
    type Err = PathError;
    fn from_str(text: &str) -> Result<VfsPath<S>, PathError> {
        VfsPath::new(text)
    }
}

impl<S: Scope> serde::Serialize for VfsPath<S> {
    fn serialize<Z: serde::Serializer>(&self, serializer: Z) -> Result<Z::Ok, Z::Error> {
        self.path.serialize(serializer)
    }
}

impl<'de, S: Scope> serde::Deserialize<'de> for VfsPath<S> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<VfsPath<S>, D::Error> {
        RelPath::deserialize(deserializer).map(VfsPath::from)
    }
}

#[cfg(test)]
mod tests {
    use super::{Run, Tree, VfsPath};
    use crate::Name;

    #[test]
    fn a_path_keeps_its_scope_and_goes_by_its_names() {
        let source: VfsPath<Tree> = VfsPath::new("packages/m93r/src/a.c").unwrap();
        let package: VfsPath<Tree> = VfsPath::new("packages/m93r").unwrap();
        let within = source.strip_prefix(&package).unwrap();
        let laid: VfsPath<Run> = VfsPath::<Run>::new("self").unwrap().join_path(&within);
        assert_eq!(laid.as_str(), "self/src/a.c");
        assert_eq!(laid.parent().unwrap().join(&Name::new("b.c").unwrap()).as_str(), "self/src/b.c");
        assert_eq!(format!("{source:?}"), "\"packages/m93r/src/a.c\" (in the project's tree)");
        let read: VfsPath<Tree> = serde_json::from_str("\"a/b\"").unwrap();
        assert_eq!(read.into_scope::<Run>().as_str(), "a/b");
        assert!(serde_json::from_str::<VfsPath<Tree>>("\"a/../b\"").is_err());
    }
}
