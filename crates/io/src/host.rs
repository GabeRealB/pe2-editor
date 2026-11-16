//! A place on disk, for the filesystem's implementations alone: where the project's storage, the shared space and the
//! served folder keep what they keep. What only reads or writes files somewhere - a file the user picked, a program's
//! own folder - takes a `std::path::Path`.

#![allow(clippy::disallowed_methods, reason = "where a place is joined and resolved: the one module that does")]

use std::ffi::OsStr;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use pe2_path::{Name, Names, PathError, RelPath, Why};

/// Why a place on disk is no place within another, by names: it is elsewhere, or a name in its way is refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotWithin {
    /// It is not in the other place at all.
    Outside,
    /// It is in it, but by a name that is no path's (`Why`).
    Refused(PathError),
}

/// A place on disk: absolute, the links that were there when it was made resolved, and in the plain form the system
/// names it by (`C:\a`, `\\server\share\a`; never `\\?\C:\a` where `C:\a` names the same place). Two of the same place
/// are therefore equal, hash alike and give the same text, on every system - whatever form each was given in.
///
/// It is made where a place comes in from outside the program (an argument, a setting, the environment), by
/// [`HostPath::new`], and joined with the project's names part by part ([`HostPath::join`]), so that its separators
/// are always the system's own.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HostPath(Arc<Path>);

impl HostPath {
    /// The place `path` names, made absolute against the working folder, with its links resolved as far as it exists -
    /// so that a link out of a folder is not taken to be in it - and the rest of it joined on.
    ///
    /// # Errors
    /// The working folder cannot be read, or the part of it that exists cannot be resolved.
    pub fn new(path: &Path) -> std::io::Result<HostPath> {
        let absolute = std::path::absolute(path)?;
        Ok(HostPath(plain(&real(&absolute)?).into()))
    }

    /// A place someone wrote, relative to this one or absolute (a setting's `../elsewhere`, `/mnt/roms`): made as
    /// [`HostPath::new`] makes one.
    ///
    /// # Errors
    /// The part of it that exists cannot be resolved.
    #[allow(clippy::disallowed_methods, reason = "a place written by someone, taken as the system takes it")]
    pub fn resolve(&self, given: &Path) -> std::io::Result<HostPath> {
        if given.is_absolute() { HostPath::new(given) } else { HostPath::new(&self.0.join(given)) }
    }

    /// A place already in the form a `HostPath` holds - absolute, plain, with no `.` or `..` - as read back from where
    /// one was written; nothing is read from disk.
    #[must_use]
    pub fn from_plain(path: PathBuf) -> Option<HostPath> {
        let ok = path.is_absolute()
            && plain(&path) == path
            && path.components().all(|c| !matches!(c, Component::CurDir | Component::ParentDir));
        ok.then(|| HostPath(path.into()))
    }

    /// The place, as the system's own type, for what takes one whole.
    #[must_use]
    pub fn into_path_buf(self) -> PathBuf {
        self.0.to_path_buf()
    }

    /// The place, for the system's calls.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// A place within it - a name, a path - joined name by name, in the system's own separators.
    #[must_use]
    pub fn join(&self, within: &(impl Names + ?Sized)) -> HostPath {
        let mut at = self.0.to_path_buf();
        at.extend(within.names());
        HostPath(at.into())
    }

    /// A place within it as a program is given it in an argument: this folder as the system writes it, then the path
    /// within with `/`, which programs read on every system - and which is what a pattern a program matches the names
    /// it is given against is written with (a link script's `*/src/a.o`, matched as the name was given).
    #[must_use]
    pub fn argument(&self, within: &RelPath) -> String {
        if within.is_root() {
            return self.to_string();
        }
        let folder = self.to_string();
        let separator = if folder.ends_with(std::path::MAIN_SEPARATOR) { "" } else { std::path::MAIN_SEPARATOR_STR };
        format!("{folder}{separator}{within}")
    }

    /// The place beside it named as it is with `suffix` after: `layers/top` and `.toml`, `layers/top.toml`.
    #[must_use]
    pub fn with_suffix(&self, suffix: &Name) -> HostPath {
        let mut text = self.0.as_os_str().to_owned();
        text.push(suffix.as_str());
        HostPath(PathBuf::from(text).into())
    }

    /// What a listing of it gives (`std::fs::read_dir`): an entry of it by the name the system gave, whatever that
    /// name is - a place on disk, not a name of the project's.
    #[must_use]
    pub fn entry(&self, name: &OsStr) -> HostPath {
        let mut at = self.0.to_path_buf();
        at.push(name);
        HostPath(at.into())
    }

    /// What a folder holds, as places: what the system lists (`std::fs::read_dir`), nothing where it lists nothing.
    pub fn entries(&self) -> impl Iterator<Item = HostPath> {
        let listed = std::fs::read_dir(&self.0).into_iter().flatten().flatten();
        listed.map(|entry| self.entry(&entry.file_name()))
    }

    /// The place beside it named as it is without `suffix`, where its name ends in it: what `with_suffix` was given.
    #[must_use]
    pub fn without_suffix(&self, suffix: &Name) -> Option<HostPath> {
        let name = self.0.file_name()?.to_str()?;
        let stem = name.strip_suffix(suffix.as_str()).filter(|stem| !stem.is_empty())?;
        Some(HostPath(self.0.with_file_name(stem).into()))
    }

    /// It, what it is in, and so on up to its root.
    pub fn ancestors(&self) -> impl Iterator<Item = HostPath> {
        self.0.ancestors().map(|at| HostPath(at.into()))
    }

    /// What it is in, unless it is a root.
    #[must_use]
    pub fn parent(&self) -> Option<HostPath> {
        self.0.parent().map(|parent| HostPath(parent.into()))
    }

    /// Its last part, unless it is a root.
    #[must_use]
    pub fn file_name(&self) -> Option<&OsStr> {
        self.0.file_name()
    }

    /// Whether it is `base` or within it.
    #[must_use]
    pub fn starts_with(&self, base: &HostPath) -> bool {
        self.0.starts_with(&base.0)
    }

    /// Its place within `base` as names.
    ///
    /// # Errors
    /// It is not within `base` ([`NotWithin::Outside`]), or it is by a name that is no path's
    /// ([`NotWithin::Refused`]): two answers a caller has to tell apart - what is elsewhere is no business of its, but
    /// what is within by a name it cannot take is, and is said, never passed over.
    pub fn within(&self, base: &HostPath) -> Result<std::sync::Arc<RelPath>, NotWithin> {
        let rest = self.0.strip_prefix(&base.0).map_err(|_| NotWithin::Outside)?;
        let shown = || rest.to_string_lossy().into_owned();
        let mut names = Vec::new();
        for part in rest.components() {
            let Component::Normal(name) = part else {
                return Err(NotWithin::Refused(PathError { text: shown().into(), why: Why::Dot }));
            };
            let name = name
                .to_str()
                .ok_or_else(|| NotWithin::Refused(PathError { text: shown().into(), why: Why::NotText }))?;
            if let Err(refused) = Name::new(name) {
                return Err(NotWithin::Refused(PathError { text: shown().into(), why: refused.why }));
            }
            names.push(name);
        }
        RelPath::new(&names.join("/")).map(RelPath::to_owned).map_err(NotWithin::Refused)
    }

    /// The place it is at once what `from` holds is moved to `to`: by its own parts, which need be no names; none
    /// where it is not within `from`.
    #[must_use]
    pub fn moved(&self, from: &HostPath, to: &HostPath) -> Option<HostPath> {
        let rest = self.0.strip_prefix(&from.0).ok()?;
        let mut at = to.0.to_path_buf();
        at.push(rest);
        Some(HostPath(at.into()))
    }

    /// The form a WebAssembly extension's view of the disk names it by: `/D:/a/b` for `D:\a\b` on Windows, where
    /// each drive is a folder of the root; as it is elsewhere.
    #[must_use]
    pub fn guest(&self) -> String {
        let text = self.0.to_string_lossy();
        match self.0.components().next() {
            Some(Component::Prefix(prefix)) if matches!(prefix.kind(), std::path::Prefix::Disk(_)) => {
                let (drive, rest) = text.split_at(2);
                format!("/{}{}", drive.to_ascii_uppercase(), rest.replace('\\', "/"))
            }
            _ => text.into_owned(),
        }
    }

    /// The place a guest's form names: what [`HostPath::guest`] made.
    #[must_use]
    pub fn from_guest(text: &str) -> Option<HostPath> {
        let bytes = text.as_bytes();
        let path = if cfg!(windows)
            && bytes.len() >= 3
            && bytes[0] == b'/'
            && bytes[1].is_ascii_alphabetic()
            && bytes[2] == b':'
        {
            let rest = &text[3..];
            PathBuf::from(format!("{}:{}", &text[1..2], if rest.is_empty() { "\\" } else { rest }).replace('/', "\\"))
        } else {
            PathBuf::from(text)
        };
        HostPath::from_plain(path)
    }
}

/// The place `path` (absolute) names, its links resolved through the deepest part of it that exists, the rest joined
/// on with `.` dropped and `..` taking the part before away.
#[allow(clippy::disallowed_methods, reason = "the one place a path is resolved: here")]
fn real(path: &Path) -> std::io::Result<PathBuf> {
    let mut there = path.to_path_buf();
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    loop {
        match std::fs::canonicalize(&there) {
            Ok(mut real) => {
                for part in rest.iter().rev() {
                    match Path::new(part).components().next() {
                        Some(Component::ParentDir) => {
                            real.pop();
                        }
                        Some(Component::CurDir) | None => {}
                        Some(_) => real.push(part),
                    }
                }
                return Ok(real);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound || e.kind() == std::io::ErrorKind::NotADirectory => {
                let Some(name) = there.components().next_back() else { return Ok(path.to_path_buf()) };
                if matches!(name, Component::Prefix(_) | Component::RootDir) {
                    return Ok(path.to_path_buf());
                }
                rest.push(name.as_os_str().to_owned());
                there.pop();
            }
            Err(e) => return Err(e),
        }
    }
}

/// `path` in the plain form, where that names the same place: Windows' verbatim forms (`\\?\C:\a`, `\\?\UNC\s\x\a`)
/// as `C:\a` and `\\s\x\a`, unless a part of them is a name the plain form would take otherwise (a trailing dot, a
/// device's name). Elsewhere, as it is.
fn plain(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        use std::path::Prefix;
        let mut parts = path.components();
        let Some(Component::Prefix(prefix)) = parts.next() else { return path.to_path_buf() };
        let rest: Vec<_> = parts.collect();
        let safe = rest.iter().all(|part| match part {
            Component::RootDir => true,
            Component::Normal(name) => name.to_str().is_some_and(|name| Name::new(name).is_ok()),
            _ => false,
        });
        let start = match prefix.kind() {
            Prefix::VerbatimDisk(drive) if safe => format!("{}:\\", char::from(drive).to_ascii_uppercase()),
            Prefix::VerbatimUNC(server, share) if safe => {
                format!("\\\\{}\\{}\\", server.to_string_lossy(), share.to_string_lossy())
            }
            Prefix::Disk(drive) => format!("{}:\\", char::from(drive).to_ascii_uppercase()),
            _ => return path.to_path_buf(),
        };
        let mut plain = PathBuf::from(start);
        plain.extend(rest.iter().filter(|part| !matches!(part, Component::RootDir)));
        plain
    }
    #[cfg(not(windows))]
    {
        path.to_path_buf()
    }
}

impl fmt::Display for HostPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0.display(), f)
    }
}

impl fmt::Debug for HostPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

/// Read as a place on disk, by whatever reads one (`exists`, `std::fs`). Joining text onto it is refused by the build
/// (`clippy.toml`'s `disallowed-methods`): what is within it is reached by [`HostPath::join`] alone.
impl std::ops::Deref for HostPath {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for HostPath {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

/// A place written as the system writes one - a command line's argument - made as [`HostPath::new`] makes one.
impl std::str::FromStr for HostPath {
    type Err = std::io::Error;
    fn from_str(text: &str) -> std::io::Result<HostPath> {
        HostPath::new(Path::new(text))
    }
}

impl serde::Serialize for HostPath {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0.to_str() {
            Some(text) => serializer.serialize_str(text),
            None => Err(serde::ser::Error::custom(format!("{} is not UTF-8", self.0.display()))),
        }
    }
}

impl<'de> serde::Deserialize<'de> for HostPath {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<HostPath, D::Error> {
        let text = <std::borrow::Cow<'de, str> as serde::Deserialize>::deserialize(deserializer)?;
        HostPath::from_plain(PathBuf::from(&*text))
            .ok_or_else(|| serde::de::Error::custom(format!("{text:?} is no absolute place in its plain form")))
    }
}

impl fmt::Display for NotWithin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NotWithin::Outside => f.write_str("it is elsewhere"),
            NotWithin::Refused(why) => why.fmt(f),
        }
    }
}

impl std::error::Error for NotWithin {}

#[cfg(test)]
#[allow(
    clippy::disallowed_methods,
    reason = "a test's fixtures: the machine's time, places, files and environment, which nothing tested reads"
)]
mod tests {
    use super::HostPath;
    use pe2_path::RelPath;
    use std::path::Path;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("pe2-path-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("a")).unwrap();
        dir
    }

    #[test]
    fn a_place_within_is_given_to_a_program_with_its_path_within_written_with_slashes() {
        let dir = HostPath::new(&scratch("argument")).unwrap();
        let sep = std::path::MAIN_SEPARATOR;
        assert_eq!(dir.argument(RelPath::new("self/src/a.o").unwrap()), format!("{dir}{sep}self/src/a.o"));
        assert_eq!(dir.argument(RelPath::root()), dir.to_string());
    }

    #[test]
    fn one_place_named_any_way_is_one_host_path() {
        let dir = scratch("same");
        let made = HostPath::new(&dir).unwrap();
        assert_eq!(HostPath::new(&dir.join("a").join("..")).unwrap(), made);
        assert_eq!(HostPath::new(&dir.join(".")).unwrap(), made);
        let missing = HostPath::new(&dir.join("x").join("..").join("y").join("z")).unwrap();
        assert_eq!(missing, made.join(&RelPath::new("y/z").unwrap()));
        assert_eq!(missing.within(&made).as_deref(), Ok(RelPath::new("y/z").unwrap()));
        assert_eq!(made.within(&missing), Err(super::NotWithin::Outside));
        assert_eq!(missing.moved(&made, &missing), Some(missing.join(&RelPath::new("y/z").unwrap())));
        assert_eq!(made.moved(&missing, &made), None);
        // Within, but by a name no path takes: said so, and why - never taken for elsewhere. (Where Windows names
        // `CON` a device, it is one whatever folder it is written in.)
        #[cfg(unix)]
        {
            let refused = HostPath::new(&dir.join("CON")).unwrap();
            assert!(matches!(
                refused.within(&made),
                Err(super::NotWithin::Refused(pe2_path::PathError { why: pe2_path::Why::Reserved, .. }))
            ));
        }
        assert!(missing.starts_with(&made));
        let written = serde_json::to_string(&missing).unwrap();
        assert_eq!(serde_json::from_str::<HostPath>(&written).unwrap(), missing);
        assert!(serde_json::from_str::<HostPath>("\"relative/place\"").is_err());
        assert_eq!(HostPath::from_guest(&missing.guest()), Some(missing));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_path_within_is_joined_in_the_systems_own_separators() {
        let dir = scratch("join");
        let made = HostPath::new(&dir).unwrap();
        let joined = made.join(&RelPath::new("a/b/c.txt").unwrap());
        assert_eq!(joined.as_path(), made.as_path().join("a").join("b").join("c.txt"));
        let beside = joined.parent().unwrap().with_suffix(pe2_path::Name::constant(".toml"));
        assert_eq!(beside.as_path(), made.as_path().join("a").join("b.toml"));
        assert_eq!(beside.without_suffix(pe2_path::Name::constant(".toml")), joined.parent());
        assert_eq!(made.entry(std::ffi::OsStr::new("a")), made.join(&pe2_path::Name::constant("a")));
        assert_eq!(joined.ancestors().nth(3), Some(made.clone()));
        assert!(!joined.to_string().contains(if cfg!(windows) { '/' } else { '\\' }));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_link_out_of_a_folder_is_not_in_it() {
        let dir = scratch("link");
        std::fs::create_dir_all(dir.join("elsewhere")).unwrap();
        std::os::unix::fs::symlink(dir.join("elsewhere"), dir.join("a").join("out")).unwrap();
        let folder = HostPath::new(&dir.join("a")).unwrap();
        let through = HostPath::new(&dir.join("a").join("out").join("file")).unwrap();
        assert!(!through.starts_with(&folder));
        assert_eq!(through, HostPath::new(&dir.join("elsewhere").join("file")).unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_verbatim_forms_are_the_plain_ones() {
        let dir = scratch("verbatim");
        let plain = HostPath::new(&dir).unwrap();
        let verbatim = Path::new(r"\\?\").join(plain.as_path());
        assert_eq!(HostPath::new(&verbatim).unwrap(), plain);
        assert!(!plain.to_string().starts_with(r"\\?\"));
        assert_eq!(super::plain(Path::new(r"\\?\UNC\server\share\a")), Path::new(r"\\server\share\a"));
        assert_eq!(super::plain(Path::new(r"\\?\c:\a\nul")), Path::new(r"\\?\c:\a\nul"));
        assert_eq!(super::plain(Path::new(r"\\?\c:\a\b.")), Path::new(r"\\?\c:\a\b."));
        assert_eq!(super::plain(Path::new(r"\\?\c:\a")), Path::new(r"C:\a"));
        let guest = HostPath::from_plain(r"D:\pe2\a".into()).unwrap();
        assert_eq!(guest.guest(), "/D:/pe2/a");
        assert_eq!(HostPath::from_guest("/D:/pe2/a"), Some(guest));
        assert_eq!(HostPath::from_guest("/D:").unwrap().as_path(), Path::new(r"D:\"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(not(windows))]
    #[test]
    fn elsewhere_a_place_is_as_it_is_written() {
        assert_eq!(super::plain(Path::new("/a/b")), Path::new("/a/b"));
        assert_eq!(HostPath::from_plain("/a/b".into()).unwrap().guest(), "/a/b");
        assert_eq!(HostPath::from_plain("/a/../b".into()), None);
    }
}
