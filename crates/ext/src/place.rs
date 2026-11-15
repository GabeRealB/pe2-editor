//! Places on disk the host gives an operation - a file for a program to write, a folder served, where a content or a
//! file of the project is - and a program's arguments, which tell it places. An extension reads a place, in its own
//! filesystem's form to open it and in a program's to tell one, and never makes one of text: what a program is told is
//! written by the host, as that program reads it.

use std::any::Any;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use pe2_path::RelPath;

use crate::Result;

/// A place on disk, as the host gave it.
#[derive(Clone)]
pub struct HostPath(Arc<dyn Place>);

/// A content where a program can read it, for as long as this is held (`Call::place_content`): a stored blob's file,
/// or a file written for a content held in itself or in memory, removed when this is dropped. Held until the program
/// it is given to is done with it - waited for, not only started.
pub struct PlacedContent {
    path: HostPath,
    /// What keeps it: in the host, what removes what was written for it; in a component, the host's handle.
    _keeps: Box<dyn Any + Send>,
}

impl PlacedContent {
    /// A place, and what keeps it: what removes it when dropped, or nothing.
    #[doc(hidden)]
    #[must_use]
    pub fn new(path: HostPath, keeps: impl Any + Send) -> PlacedContent {
        PlacedContent { path, _keeps: Box::new(keeps) }
    }

    /// Where it is.
    #[must_use]
    pub fn path(&self) -> &HostPath {
        &self.path
    }
}

impl fmt::Debug for PlacedContent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PlacedContent({})", self.path.native())
    }
}

/// What gives a place its forms: the host's own path, where the host runs the operation itself, or the handle a
/// component holds.
#[doc(hidden)]
pub trait Place: Send + Sync {
    fn guest(&self) -> PathBuf;
    fn native(&self) -> String;
    fn join(&self, within: &RelPath) -> Result<HostPath>;
    fn as_any(&self) -> &dyn Any;
}

impl HostPath {
    /// As this extension's filesystem names it: what it opens, reads or removes it by.
    #[must_use]
    pub fn guest(&self) -> PathBuf {
        self.0.guest()
    }

    /// As a program on this system is told it, and a file written for one holds it (a compilation database). A
    /// program's argument is better told as one (`Arg::Path`): the host writes it so.
    #[must_use]
    pub fn native(&self) -> String {
        self.0.native()
    }

    /// A place within it, by a path within a folder.
    ///
    /// # Errors
    /// The host will not give it.
    pub fn join(&self, within: &RelPath) -> Result<HostPath> {
        self.0.join(within)
    }

    /// A place as something that gives places has it.
    #[doc(hidden)]
    #[must_use]
    pub fn of(place: impl Place + 'static) -> HostPath {
        HostPath(Arc::new(place))
    }

    /// A place of the host's own, where the host runs an operation itself.
    #[doc(hidden)]
    #[must_use]
    pub fn of_host(path: PathBuf) -> HostPath {
        HostPath::of(OnHost(path))
    }

    /// The host's own path of a place it gave itself; none of one a component holds.
    #[doc(hidden)]
    #[must_use]
    pub fn on_host(&self) -> Option<&Path> {
        self.0.as_any().downcast_ref::<OnHost>().map(|on| on.0.as_path())
    }

    /// What gives it its forms.
    #[doc(hidden)]
    #[must_use]
    pub fn place(&self) -> &dyn Place {
        &*self.0
    }
}

impl fmt::Debug for HostPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.native())
    }
}

/// Two places are one where a program is told them alike.
impl PartialEq for HostPath {
    fn eq(&self, other: &HostPath) -> bool {
        self.native() == other.native()
    }
}

impl fmt::Display for HostPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.native())
    }
}

/// A place of the host's own: its forms are its path.
struct OnHost(PathBuf);

impl Place for OnHost {
    fn guest(&self) -> PathBuf {
        self.0.clone()
    }

    fn native(&self) -> String {
        self.0.to_string_lossy().into_owned()
    }

    #[allow(clippy::disallowed_methods, reason = "a place within the host's own, joined of checked names")]
    fn join(&self, within: &RelPath) -> Result<HostPath> {
        Ok(HostPath::of_host(within.parts().fold(self.0.clone(), |at, name| at.join(name.as_str()))))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Part of a program's argument (`Arg::Joined`).
#[derive(Clone, Debug)]
pub enum ArgPart {
    Text(String),
    /// Written as the program reads it.
    Path(HostPath),
}

/// A program's argument: text, a place the host writes as the program reads it, or parts joined into one (`-I` and a
/// folder).
#[derive(Clone, Debug)]
pub enum Arg {
    Text(String),
    Path(HostPath),
    Joined(Vec<ArgPart>),
}

impl Arg {
    /// Parts joined into one argument.
    #[must_use]
    pub fn joined(parts: impl IntoIterator<Item = ArgPart>) -> Arg {
        Arg::Joined(parts.into_iter().collect())
    }
}

impl From<&str> for Arg {
    fn from(text: &str) -> Arg {
        Arg::Text(text.to_string())
    }
}

impl From<String> for Arg {
    fn from(text: String) -> Arg {
        Arg::Text(text)
    }
}

impl From<&String> for Arg {
    fn from(text: &String) -> Arg {
        Arg::Text(text.clone())
    }
}

impl From<HostPath> for Arg {
    fn from(path: HostPath) -> Arg {
        Arg::Path(path)
    }
}

impl From<&HostPath> for Arg {
    fn from(path: &HostPath) -> Arg {
        Arg::Path(path.clone())
    }
}

impl From<&str> for ArgPart {
    fn from(text: &str) -> ArgPart {
        ArgPart::Text(text.to_string())
    }
}

impl From<&HostPath> for ArgPart {
    fn from(path: &HostPath) -> ArgPart {
        ArgPart::Path(path.clone())
    }
}

/// Arguments of text and places, as a program's arguments are written: each converted (`Arg::from`).
#[macro_export]
macro_rules! args {
    ($($arg:expr),* $(,)?) => {
        vec![$($crate::Arg::from($arg)),*]
    };
}
