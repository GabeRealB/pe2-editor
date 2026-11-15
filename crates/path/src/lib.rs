//! Paths as types. Text is never a path: it becomes one through a constructor that checks it, where it comes in - a file
//! read, a frame between processes, a call of an extension, an argument - and past that, what names a file is one of
//! these:
//!
//! - [`Name`]: one part of a path, portable: a name every system the editor runs on can hold as it is.
//! - [`RelPath`]: names joined by `/`, relative to whatever it is taken within.
//! - [`Uid`] and [`ResourcePath`]: what a project names an asset by - its uid, what of it an address names, and how
//!   that is asked for, written as a URL is (`uid://x/objects`, `uid://x/cells/body?palette=night`); [`Address`],
//!   [`Query`] and [`Asked`] are those parts, checked as the program is compiled where it holds them.
//! - [`VfsPath`]: a path within a scope of the project's filesystem, typed by the kind of scope ([`Tree`], [`Run`],
//!   [`Given`]), so that one kind's path is not taken for another's.
//!
//! A place on disk is no path of these: the filesystem's implementations keep it (`sfs::HostPath`).

/// A type's serde as its text: written as `Display` writes it, read through its `FromStr`, which checks it.
#[macro_export]
#[doc(hidden)]
macro_rules! serde_as_text {
    ($type:ty) => {
        impl serde::Serialize for $type {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.collect_str(self)
            }
        }
        impl<'de> serde::Deserialize<'de> for $type {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<$type, D::Error> {
                let text = <std::borrow::Cow<'de, str> as serde::Deserialize>::deserialize(deserializer)?;
                text.parse().map_err(serde::de::Error::custom)
            }
        }
    };
}

mod error;
mod fold;
mod name;
mod resource;
mod vfs;

/// A key and everything after it, as a map of shared paths is ranged over by a borrowed one: `map.range(from(path))`.
/// (`path..` asks for a sized key, which a path is not.)
#[must_use]
pub fn from<T: ?Sized>(start: &T) -> (std::ops::Bound<&T>, std::ops::Bound<&T>) {
    (std::ops::Bound::Included(start), std::ops::Bound::Unbounded)
}

pub use error::{PathError, Why};
pub use fold::{UNICODE_VERSION, folded};
pub use name::{Name, Names, RelPath};
pub use resource::{Address, Asked, Query, Reference, ResourcePath, Uid};
pub use vfs::{Given, Run, Scope, Tree, VfsPath};
