//! A path within a scope of the project's filesystem, typed by the kind of scope it is in.

use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::sync::Arc;

use crate::error::{PathError, Why};
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

/// What starts a path of the project's tree written as a place (`res://packages/aya`), as the files a project
/// writes name one.
pub const RES_PREFIX: &str = "res://";

impl VfsPath<Tree> {
    /// `text` as a path of the project's tree written as a place: `res://` and the path (`res://packages/aya`), a
    /// folder's `/` after it taken.
    ///
    /// # Errors
    /// It does not start with `res://`, or what follows is no path.
    pub fn from_res(text: &str) -> Result<&VfsPath<Tree>, PathError> {
        let path = text.strip_prefix(RES_PREFIX).ok_or_else(|| PathError::new(text, Why::NoRes))?;
        VfsPath::new(path.strip_suffix('/').unwrap_or(path))
    }
}

/// A path within a scope of kind `S`: a [`RelPath`] the compiler will not take for a path of another kind of scope.
///
/// Unsized, as `RelPath` is: borrowed as `&VfsPath<S>`, owned as `Arc<VfsPath<S>>`.
#[repr(transparent)]
pub struct VfsPath<S: Scope> {
    scope: PhantomData<fn() -> S>,
    path: RelPath,
}

impl<S: Scope> VfsPath<S> {
    /// A path's names, within a scope of this kind.
    const fn of(path: &RelPath) -> &VfsPath<S> {
        // SAFETY: `VfsPath` is a `RelPath` alone beside a marker of no size, as `repr(transparent)` lays it out.
        unsafe { &*(std::ptr::from_ref::<RelPath>(path) as *const VfsPath<S>) }
    }

    /// The scope itself.
    #[must_use]
    pub const fn root() -> &'static VfsPath<S> {
        VfsPath::of(RelPath::root())
    }

    /// `text` as a path within a scope of this kind.
    ///
    /// # Errors
    /// It is no path (`RelPath::new`).
    pub fn new(text: &str) -> Result<&VfsPath<S>, PathError> {
        RelPath::new(text).map(VfsPath::of)
    }

    /// A path the program holds, checked as it is compiled (`RelPath::constant`).
    ///
    /// # Panics
    /// `text` is no relative path.
    #[must_use]
    pub const fn constant(text: &'static str) -> &'static VfsPath<S> {
        VfsPath::of(RelPath::constant(text))
    }

    /// A relative path's names, within a scope of this kind: where they are taken within one.
    #[must_use]
    pub const fn within(path: &RelPath) -> &VfsPath<S> {
        VfsPath::of(path)
    }

    /// The same names, within another kind of scope: for where one scope is laid out of another.
    #[must_use]
    pub const fn in_scope<T: Scope>(&self) -> &VfsPath<T> {
        VfsPath::of(&self.path)
    }

    /// Its names, as a relative path.
    #[must_use]
    pub const fn rel(&self) -> &RelPath {
        &self.path
    }

    /// Whether it is the scope itself.
    #[must_use]
    pub const fn is_root(&self) -> bool {
        self.path.is_root()
    }

    /// Its names, in order.
    pub fn parts(&self) -> impl DoubleEndedIterator<Item = &Name> {
        self.path.parts()
    }

    /// Its last name's text, unless it is the scope itself: what it is shown as.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.path.name()
    }

    /// How many names it has: none for the scope itself.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.path.depth()
    }

    /// Its last name, unless it is the scope itself.
    #[must_use]
    pub fn last(&self) -> Option<&Name> {
        self.path.last()
    }

    /// What it is in, unless it is the scope itself.
    #[must_use]
    pub fn parent(&self) -> Option<&VfsPath<S>> {
        self.path.parent().map(VfsPath::of)
    }

    /// A name within it.
    #[must_use]
    pub fn join(&self, name: &Name) -> Arc<VfsPath<S>> {
        VfsPath::shared(self.path.join(name))
    }

    /// A path within it.
    #[must_use]
    pub fn join_path(&self, path: &RelPath) -> Arc<VfsPath<S>> {
        VfsPath::shared(self.path.join_path(path))
    }

    /// Whether it is `prefix` or within it, name by name.
    #[must_use]
    pub fn starts_with(&self, prefix: &VfsPath<S>) -> bool {
        self.path.starts_with(&prefix.path)
    }

    /// What of it is within `prefix`, if it is `prefix` or within it.
    #[must_use]
    pub fn strip_prefix(&self, prefix: &VfsPath<S>) -> Option<&RelPath> {
        self.path.strip_prefix(&prefix.path)
    }

    /// Its text, names joined by `/`: for writing it out where it leaves the program as text.
    #[must_use]
    pub const fn as_str(&self) -> &str {
        self.path.as_str()
    }

    /// A shared path's names, within a scope of this kind.
    #[must_use]
    pub fn shared(path: Arc<RelPath>) -> Arc<VfsPath<S>> {
        // SAFETY: `VfsPath` is a `RelPath` alone beside a marker of no size (`repr(transparent)`).
        unsafe { Arc::from_raw(Arc::into_raw(path) as *const VfsPath<S>) }
    }

    /// A shared path's names, as a relative path.
    #[must_use]
    pub fn into_rel(this: Arc<VfsPath<S>>) -> Arc<RelPath> {
        // SAFETY: `VfsPath` is a `RelPath` alone beside a marker of no size (`repr(transparent)`).
        unsafe { Arc::from_raw(Arc::into_raw(this) as *const RelPath) }
    }

    /// A shared path's names, within another kind of scope.
    #[must_use]
    pub fn into_scope<T: Scope>(this: Arc<VfsPath<S>>) -> Arc<VfsPath<T>> {
        // SAFETY: both are a `RelPath` alone beside a marker of no size (`repr(transparent)`).
        unsafe { Arc::from_raw(Arc::into_raw(this) as *const VfsPath<T>) }
    }
}

impl<S: Scope> crate::Names for VfsPath<S> {
    fn names(&self) -> impl Iterator<Item = &str> {
        self.path.parts().map(Name::as_str)
    }
}

impl<S: Scope> ToOwned for VfsPath<S> {
    type Owned = Arc<VfsPath<S>>;
    fn to_owned(&self) -> Arc<VfsPath<S>> {
        VfsPath::shared(self.path.to_owned())
    }
}

impl<S: Scope> From<&VfsPath<S>> for Arc<VfsPath<S>> {
    fn from(path: &VfsPath<S>) -> Arc<VfsPath<S>> {
        path.to_owned()
    }
}

impl<S: Scope> From<&VfsPath<S>> for Box<VfsPath<S>> {
    fn from(path: &VfsPath<S>) -> Box<VfsPath<S>> {
        let boxed: Box<str> = path.as_str().into();
        // SAFETY: `VfsPath` is a `RelPath` alone beside a marker of no size, and `RelPath` is `str` alone (both
        // `repr(transparent)`); the text is a checked path.
        unsafe { Box::from_raw(Box::into_raw(boxed) as *mut VfsPath<S>) }
    }
}

impl<S: Scope> Clone for Box<VfsPath<S>> {
    fn clone(&self) -> Box<VfsPath<S>> {
        Box::from(&**self)
    }
}

impl<S: Scope> Default for &VfsPath<S> {
    fn default() -> Self {
        VfsPath::root()
    }
}

impl<S: Scope> PartialEq for VfsPath<S> {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path
    }
}

impl<S: Scope> Eq for VfsPath<S> {}

impl<S: Scope> PartialEq<VfsPath<S>> for Arc<VfsPath<S>> {
    fn eq(&self, other: &VfsPath<S>) -> bool {
        **self == *other
    }
}

impl<S: Scope> PartialEq<&VfsPath<S>> for Arc<VfsPath<S>> {
    fn eq(&self, other: &&VfsPath<S>) -> bool {
        **self == **other
    }
}

impl<S: Scope> PartialEq<str> for VfsPath<S> {
    fn eq(&self, other: &str) -> bool {
        self.path == *other
    }
}

impl<S: Scope> PartialEq<&str> for VfsPath<S> {
    fn eq(&self, other: &&str) -> bool {
        self.path == **other
    }
}

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

impl<S: Scope> fmt::Display for VfsPath<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.path, f)
    }
}

impl<S: Scope> fmt::Debug for VfsPath<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} (in {})", &self.path, S::KIND)
    }
}

impl<S: Scope> serde::Serialize for VfsPath<S> {
    fn serialize<Z: serde::Serializer>(&self, serializer: Z) -> Result<Z::Ok, Z::Error> {
        self.path.serialize(serializer)
    }
}

impl<'de, S: Scope> serde::Deserialize<'de> for Box<VfsPath<S>> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Box<VfsPath<S>>, D::Error> {
        let path = Box::<RelPath>::deserialize(deserializer)?;
        // SAFETY: `VfsPath` is a `RelPath` alone beside a marker of no size (`repr(transparent)`).
        Ok(unsafe { Box::from_raw(Box::into_raw(path) as *mut VfsPath<S>) })
    }
}

#[cfg(test)]
mod tests {
    use super::{Run, Tree, VfsPath};
    use crate::Name;
    use std::sync::Arc;

    #[test]
    fn a_path_keeps_its_scope_and_goes_by_its_names() {
        let source: &VfsPath<Tree> = VfsPath::new("packages/m93r/src/a.c").unwrap();
        let package: &VfsPath<Tree> = VfsPath::new("packages/m93r").unwrap();
        let within = source.strip_prefix(package).unwrap();
        let laid: Arc<VfsPath<Run>> = VfsPath::<Run>::new("self").unwrap().join_path(within);
        assert_eq!(laid.as_str(), "self/src/a.c");
        assert_eq!(laid.parent().unwrap().join(Name::new("b.c").unwrap()).as_str(), "self/src/b.c");
        assert_eq!(format!("{source:?}"), "\"packages/m93r/src/a.c\" (in the project's tree)");
        let read: Arc<VfsPath<Tree>> = serde_json::from_str("\"a/b\"").unwrap();
        assert_eq!(VfsPath::into_scope::<Run>(read).as_str(), "a/b");
        assert!(serde_json::from_str::<Arc<VfsPath<Tree>>>("\"a/../b\"").is_err());
    }

    #[test]
    fn a_place_of_the_tree_is_res_and_its_path() {
        assert_eq!(VfsPath::from_res("res://roms/a.bin").unwrap().as_str(), "roms/a.bin");
        assert_eq!(VfsPath::from_res("res://roms/").unwrap().as_str(), "roms");
        assert!(VfsPath::from_res("res://").unwrap().is_root());
        let refused = VfsPath::from_res("roms/a.bin").unwrap_err();
        assert_eq!(refused.to_string(), "\"roms/a.bin\" is no place of the project (`res://` and a path)");
        assert!(VfsPath::from_res("res://a/../b").is_err());
    }
}
