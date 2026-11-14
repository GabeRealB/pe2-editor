//! Paths as types. Text is never a path: it becomes one through a constructor that checks it, where it comes in - a file
//! read, a frame between processes, a call of an extension, an argument - and past that, what names a file is one of
//! these:
//!
//! - [`Name`]: one part of a path, portable: a name every system the editor runs on can hold as it is.
//! - [`RelPath`]: names joined by `/`, relative to whatever it is taken within.
//! - [`Uid`] and [`ResourcePath`]: what a project names an asset by - its uid, and what of it a suffix names
//!   (`uid://x#objects`, `uid://x#cells/left`).
//! - [`VfsPath`]: a path within a scope of the project's filesystem, typed by the kind of scope ([`Tree`], [`Run`],
//!   [`Given`]), so that one kind's path is not taken for another's.
//! - [`HostPath`]: a place on disk, for the filesystem's implementations alone: absolute, its links resolved, in the
//!   plain form the system names it by, so that two of the same place are equal on every system.

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
#[cfg(not(target_family = "wasm"))]
mod host;
mod name;
mod resource;
#[cfg(all(feature = "testing", not(target_family = "wasm")))]
pub mod testing;
mod vfs;

pub use error::{NotWithin, PathError, Why};
#[cfg(not(target_family = "wasm"))]
pub use host::HostPath;
pub use name::{Name, Names, RelPath};
pub use resource::{ResourcePath, Uid};
pub use vfs::{Given, Run, Scope, Tree, VfsPath};
