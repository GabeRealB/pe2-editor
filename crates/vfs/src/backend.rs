//! What serves a tree, chosen by what a job needs of it. A backend says what it can do (`Can`) and whether it is
//! available at a place; a job - the project's folder, written through; a scope, read by path - says what it needs;
//! and the backend best suited is the first of this system's (`all`, best first) that can do all of it and is available
//! there. So a backend is only ever given a job it does whole: the Projected File System, which serves read-only, is
//! never the project's folder, and WebDAV, which serves at an address, never a scope. On Windows that order is Dokany,
//! where it is installed - a real filesystem, written through as FUSE is and leaving nothing on disk - then the
//! Projected File System, then the Cloud Files API, which every Windows has.
//!
//! What is chosen is a setting (`Setting`): `auto`, a backend by name - refused, with why, where it cannot do the job or
//! is not available - or `none`. Whoever asks says it, then the project (`project.local.toml`), then the machine
//! (`setting`).
//!
//! A backend's own ways stay in its module: what it mounts is a `Mount`, told of what is served by path where the
//! backend needs telling (`placed`, `released`), and what a process that ended left of one is the backend's to clear
//! (`clear`).

use std::io;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use project::content::Use;

use crate::Served;
use crate::write::Owner;

/// What a backend can do, and what a job needs of one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Can {
    /// What programs change is applied to the project: files written, made and removed, folders made, renamed and
    /// moved - the whole of what `write::Owner` takes.
    pub write: bool,
    /// Served at a folder, which programs open by path; else at an address.
    pub folder: bool,
}

impl Can {
    /// What the project's folder needs: written through.
    pub const WRITTEN: Can = Can { write: true, folder: false };
    /// What a scope needs: read by path.
    pub const BY_PATH: Can = Can { write: false, folder: true };

    /// Whether this is all a job needs.
    #[must_use]
    pub fn does(self, needs: Can) -> bool {
        self.lacks(needs).is_none()
    }

    /// Why this is not all a job needs, where it is not.
    fn lacks(self, needs: Can) -> Option<&'static str> {
        if needs.write && !self.write {
            Some("it serves read-only, and this is written through")
        } else if needs.folder && !self.folder {
            Some("it serves at an address, and this is opened by path")
        } else {
            None
        }
    }
}

/// What is served, where, and by whom what programs change is applied.
pub struct Job<'a> {
    pub served: &'a Served,
    /// Who applies what programs change; none serves read-only.
    pub owner: Option<Arc<dyn Owner>>,
    /// The folder it is served at, for a backend serving at one.
    pub at: &'a Path,
    /// Where a backend keeps what is being written until it is applied.
    pub staging: &'a Path,
    /// What it is served as, where the system shows or registers a name.
    pub name: &'a str,
    /// The address it is served at, for a backend serving at one; none is the backend's own.
    pub address: Option<SocketAddr>,
}

/// A way to serve a tree.
pub trait Backend: Sync {
    /// Its name, as settings say it.
    fn name(&self) -> &'static str;

    /// What it is called.
    fn label(&self) -> &'static str;

    /// What it can do.
    fn can(&self) -> Can;

    /// Whether what was read through it stays on disk, read again from there: fast from the second read on, at the
    /// price of a copy.
    fn keeps(&self) -> bool {
        false
    }

    /// Whether it prepares something for every file as it starts to serve, read or not.
    fn prepares(&self) -> bool {
        false
    }

    /// Whether it can serve at a folder here.
    ///
    /// # Errors
    /// Why not, as a sentence's end ("the Projected File System is not turned on").
    fn available(&self, at: &Path) -> Result<(), String>;

    /// Serve a job, until what this returns is dropped.
    ///
    /// # Errors
    /// It cannot be served.
    fn mount(&self, job: &Job) -> io::Result<Box<dyn Mount>>;

    /// Clear what a serving that ended - let go of, or its process gone - left at a folder, served there by a name: of
    /// this backend's, or of any where which served it is not known. The folder is then empty, or gone.
    fn clear(&self, _at: &Path, _name: &str) {}

    /// Leave a folder it served, by a name, to another backend about to serve there: what it left there as it served
    /// it goes - it can be read again - and the folder is a plain one again; what was put or changed there while
    /// nothing served it stays, for whoever serves next to say (`strays`).
    fn retire(&self, _at: &Path, _name: &str) {}

    /// Let go of what it keeps on disk of what was read at a folder it serves, where that is more than a budget of
    /// bytes: what was read longest ago first (`record`), until it fits; what was changed there is never touched.
    /// Nothing, for one that keeps nothing.
    fn collect(&self, _at: &Path, _record: &crate::access::Access, _budget: u64) {}

    /// No longer know a folder it served, by the name it served it as, where the system keeps one registered.
    ///
    /// # Errors
    /// It cannot be forgotten.
    fn forget(&self, _at: &Path, _name: &str) -> io::Result<()> {
        Ok(())
    }

    /// Whether a file at the folder it served is what serving left there, as it left it - not a file put there, or
    /// changed while nothing served it.
    fn left_as_served(&self, _file: &Path) -> bool {
        false
    }
}

/// A tree served: no longer once dropped.
/// A mount written through, which gives the project what still waits to settle as it ends: what a program did a
/// moment before serving stopped is not lost with it. The mount goes first, so that nothing more is written.
pub struct Finishing {
    mount: Option<Box<dyn Mount>>,
    writes: Option<crate::writes::Writes>,
}

impl Finishing {
    #[must_use]
    pub fn of(mount: impl Mount + 'static, writes: Option<crate::writes::Writes>) -> Box<dyn Mount> {
        Box::new(Finishing { mount: Some(Box::new(mount)), writes })
    }

    fn mount(&self) -> &dyn Mount {
        self.mount.as_deref().expect("held until dropped")
    }
}

impl Mount for Finishing {
    fn shown(&self) -> String {
        self.mount().shown()
    }

    fn note(&self) -> Option<String> {
        self.mount().note()
    }

    fn placed(&self, served: &Served, path: &str) {
        self.mount().placed(served, path);
    }

    fn released(&self, path: &str) {
        self.mount().released(path);
    }
}

impl Drop for Finishing {
    fn drop(&mut self) {
        drop(self.mount.take());
        if let Some(writes) = &self.writes {
            writes.finish();
        }
    }
}

pub trait Mount: Send {
    /// Where, as users are told: a folder or an address.
    fn shown(&self) -> String;

    /// What users should know of it, where there is something.
    fn note(&self) -> Option<String> {
        None
    }

    /// What is now served under a path is about to be opened by it.
    fn placed(&self, _served: &Served, _path: &str) {}

    /// What was served under a path no longer is; called while the tree still holds it.
    fn released(&self, _path: &str) {}
}

/// What a setting says serves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Setting {
    /// The backend best suited.
    Auto,
    /// Nothing.
    None,
    /// A backend, by its name.
    Named(String),
}

impl Setting {
    /// A setting as written: `auto`, `none` or a backend's name.
    #[must_use]
    pub fn of(text: &str) -> Setting {
        match text {
            "auto" => Setting::Auto,
            "none" => Setting::None,
            name => Setting::Named(name.to_string()),
        }
    }
}

/// What serves: the first of these that says - whoever asks, then the project, then the machine - or the best suited.
#[must_use]
pub fn setting(says: &[Option<&str>]) -> Setting {
    says.iter().flatten().next().map_or(Setting::Auto, |text| Setting::of(text))
}

/// This system's backends, best first.
#[must_use]
pub fn all() -> &'static [&'static dyn Backend] {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        &[crate::fuse::BACKEND, crate::dav::BACKEND]
    }
    #[cfg(windows)]
    {
        &[crate::dokan::BACKEND, crate::projfs::BACKEND, crate::cloud::BACKEND, crate::dav::BACKEND]
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        &[crate::dav::BACKEND]
    }
}

/// What laying files out for a program may write where nothing says (`[filesystem] copies`): up to that, what a
/// program is given is laid out - what a layer holds as one file linked, the rest written - and beyond it, served.
pub const COPIES: u64 = 16 << 20;
/// What a backend that keeps what is read may keep of a project's folder where nothing says (`[filesystem] kept`).
pub const KEPT: u64 = 2 << 30;

/// A budget of bytes: the first of these that says one - the project, then the machine - else the default.
#[must_use]
pub fn budget(says: &[Option<&str>], default: u64) -> u64 {
    says.iter().flatten().find_map(|text| crate::access::bytes(text)).unwrap_or(default)
}

/// The backend a setting gives a job at a folder, for how what is served will be used: none where it says none,
/// where `auto` finds none here - or where giving programs plain copies suits better than any (what is read through
/// none is laid out for them: `content::serve_folder`). `light` says laying it all out writes little enough
/// (`COPIES`): then copies it is, for what is only read - a file a layer holds whole is linked, not copied,
/// and read after as any file is, which no backend matches.
///
/// # Errors
/// The backend it names is not one, cannot do what the job needs, or is not available there: which, and why.
pub fn choose(
    setting: &Setting,
    needs: Can,
    at: &Path,
    how: &Use,
    light: bool,
) -> Result<Option<&'static dyn Backend>, String> {
    choose_among(all(), setting, needs, at, how, light)
}

/// How well a way to serve - a backend, or none: copies - suits a use, where laying it all out would write much; the
/// best least. Measured, not reasoned:
///
/// - read often, what was read is best kept on disk - by a backend that keeps, else as copies - and worst asked for
///   again each time;
/// - read once, nothing should be written for it: a backend that neither prepares nor keeps, then one that only
///   keeps, and copies last, which write all of it to have it read back.
fn suits(way: Option<&dyn Backend>, how: &Use) -> u8 {
    let (keeps, lazy) = (way.is_some_and(Backend::keeps), way.is_some_and(|backend| !backend.prepares()));
    let copies = way.is_none();
    // Ranges of large files, however often: keeping one keeps the file, so as what is read once.
    if how.often && !how.ranges {
        return if keeps { 0 } else { 2 - u8::from(copies) };
    }
    match (copies, lazy, keeps) {
        (true, ..) => 3,
        (_, true, false) => 0,
        (_, true, true) => 1,
        _ => 2,
    }
}

fn choose_among<'a>(
    backends: &[&'a dyn Backend],
    setting: &Setting,
    needs: Can,
    at: &Path,
    how: &Use,
    light: bool,
) -> Result<Option<&'a dyn Backend>, String> {
    match setting {
        Setting::None => Ok(None),
        Setting::Auto if light && !needs.write => Ok(None),
        Setting::Auto => {
            let able = backends.iter().copied().filter(|b| b.can().lacks(needs).is_none() && b.available(at).is_ok());
            let mut ways: Vec<Option<&dyn Backend>> = able.map(Some).collect();
            // Copies are a way too, for what is only read.
            if !needs.write {
                ways.push(None);
            }
            // The best suited; among equals, the one first of this system's.
            ways.sort_by_key(|way| suits(*way, how));
            Ok(ways.first().copied().flatten())
        }
        Setting::Named(name) => {
            let Some(backend) = backends.iter().copied().find(|b| b.name() == name) else {
                let names: Vec<&str> = backends.iter().map(|b| b.name()).collect();
                return Err(format!("`{name}` is no backend here: {}, auto or none", names.join(", ")));
            };
            if let Some(why) = backend.can().lacks(needs) {
                return Err(format!("`{name}` cannot serve this: {why}"));
            }
            backend.available(at).map_err(|why| format!("`{name}` cannot serve here: {why}"))?;
            Ok(Some(backend))
        }
    }
}

/// Make a folder ready for a backend to serve at: every other backend of this system that serves at folders retires
/// what it left there (`Backend::retire`), so that serving by one after another finds no leftovers of the other's to
/// take for files that are not the project's.
pub fn hand_over(to: &dyn Backend, at: &Path, name: &str) {
    for other in all().iter().filter(|other| other.can().folder && other.name() != to.name()) {
        other.retire(at, name);
    }
}

/// Why no backend serves a job at a folder: each, and what it lacks or why it is not available.
#[must_use]
pub fn why_none(needs: Can, at: &Path) -> String {
    let whys: Vec<String> = all()
        .iter()
        .filter_map(|b| {
            let why = b.can().lacks(needs).map(str::to_string).or_else(|| b.available(at).err())?;
            Some(format!("`{}`: {why}", b.name()))
        })
        .collect();
    whys.join("; ")
}

/// A path without the verbatim prefix canonicalizing gives it (`\\?\D:\x` as `D:\x`), where it has a drive: as Windows
/// names a folder in what it asks and registers, and as what mounts on one takes it.
#[cfg(windows)]
pub(crate) fn plain(path: &Path) -> std::path::PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => std::path::PathBuf::from(rest),
        _ => path.to_path_buf(),
    }
}

/// The filesystem of the volume a path is on (`NTFS`, `ReFS`), where Windows says: the path need not be there yet.
#[cfg(windows)]
pub(crate) fn volume_filesystem(at: &Path) -> Option<String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::{GetVolumeInformationW, GetVolumePathNameW};
    let at = std::path::absolute(at).ok()?;
    let there = at.ancestors().find(|a| a.exists())?;
    let path: Vec<u16> = there.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let (mut root, mut name) = ([0u16; 262], [0u16; 64]);
    // SAFETY: a path ended by a zero, and buffers of the lengths given with them.
    unsafe {
        GetVolumePathNameW(windows_core::PCWSTR(path.as_ptr()), &mut root).ok()?;
        GetVolumeInformationW(windows_core::PCWSTR(root.as_ptr()), None, None, None, None, Some(&mut name)).ok()?;
    }
    let len = name.iter().position(|c| *c == 0).unwrap_or(name.len());
    Some(String::from_utf16_lossy(&name[..len]))
}

/// Let go, at once, of every mount this process made that would outlast it: as it is about to end without dropping them.
pub fn unmount_all() {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    crate::fuse::unmount_all();
}

#[cfg(test)]
mod tests {
    use super::{Backend, Can, Job, Mount, Setting, Use, choose_among, setting};
    use std::path::Path;

    /// A backend that only says what it is.
    struct Said {
        name: &'static str,
        can: Can,
        here: bool,
        keeps: bool,
        prepares: bool,
    }

    const fn said(name: &'static str, can: Can, here: bool) -> Said {
        Said { name, can, here, keeps: false, prepares: false }
    }

    impl Backend for Said {
        fn name(&self) -> &'static str {
            self.name
        }
        fn label(&self) -> &'static str {
            self.name
        }
        fn can(&self) -> Can {
            self.can
        }
        fn keeps(&self) -> bool {
            self.keeps
        }
        fn prepares(&self) -> bool {
            self.prepares
        }
        fn available(&self, _: &Path) -> Result<(), String> {
            if self.here { Ok(()) } else { Err("it is not turned on".into()) }
        }
        fn mount(&self, _: &Job) -> std::io::Result<Box<dyn Mount>> {
            Err(std::io::Error::other("only said"))
        }
    }

    const BOTH: Can = Can { write: true, folder: true };

    #[test]
    fn the_first_backend_that_can_do_a_job_and_is_available_is_chosen_and_a_named_one_refused_with_why() {
        let light = said("light", Can::BY_PATH, true);
        let off = said("off", BOTH, false);
        let full = said("full", BOTH, true);
        let wire = said("wire", Can::WRITTEN, true);
        let all: [&dyn Backend; 4] = [&light, &off, &full, &wire];
        let at = Path::new("x");
        // Read once, and much to write to lay it out: a backend suits better than copies.
        let how = Use::once();
        let chosen = |setting: &Setting, needs: Can| {
            choose_among(&all, setting, needs, at, &how, false).map(|b| b.map(Backend::name))
        };
        // Best suited: what cannot do the job, or is not here, is passed over.
        assert_eq!(chosen(&Setting::Auto, Can::BY_PATH), Ok(Some("light")));
        assert_eq!(chosen(&Setting::Auto, Can::WRITTEN), Ok(Some("full")));
        assert_eq!(chosen(&Setting::None, Can::WRITTEN), Ok(None));
        // By name: that one, or why not.
        assert_eq!(chosen(&Setting::Named("wire".into()), Can::WRITTEN), Ok(Some("wire")));
        let refused = |name: &str, needs: Can| chosen(&Setting::Named(name.into()), needs).unwrap_err();
        assert_eq!(
            refused("light", Can::WRITTEN),
            "`light` cannot serve this: it serves read-only, and this is written through"
        );
        assert_eq!(
            refused("wire", Can::BY_PATH),
            "`wire` cannot serve this: it serves at an address, and this is opened by path"
        );
        assert_eq!(refused("off", Can::WRITTEN), "`off` cannot serve here: it is not turned on");
        assert_eq!(refused("nfs", Can::WRITTEN), "`nfs` is no backend here: light, off, full, wire, auto or none");
        // None suited: nothing serves.
        let none: [&dyn Backend; 2] = [&off, &wire];
        assert!(choose_among(&none, &Setting::Auto, Can::BY_PATH, at, &how, false).unwrap().is_none());
    }

    #[test]
    fn what_serves_is_what_suits_how_it_will_be_used() {
        // As this system's would be ordered: one that keeps nothing, one that keeps what was read, one that keeps and
        // prepares every file up front.
        let plain = said("plain", BOTH, true);
        let keeper = Said { keeps: true, ..said("keeper", Can::BY_PATH, true) };
        let eager = Said { keeps: true, prepares: true, ..said("eager", BOTH, true) };
        let all: [&dyn Backend; 3] = [&plain, &keeper, &eager];
        let at = Path::new("x");
        let chosen = |needs: Can, how: &Use, light: bool| {
            choose_among(&all, &Setting::Auto, needs, at, how, light).unwrap().map_or("copies", Backend::name)
        };
        // Little to write to lay it all out: laid out, however it is read.
        assert_eq!(chosen(Can::BY_PATH, &Use::once(), true), "copies");
        assert_eq!(chosen(Can::BY_PATH, &Use::general(), true), "copies");
        // Much to write, read once: nothing written or kept for it.
        assert_eq!(chosen(Can::BY_PATH, &Use::once(), false), "plain");
        assert_eq!(chosen(Can::BY_PATH, &Use { ranges: true, often: false }, false), "plain");
        assert_eq!(chosen(Can::BY_PATH, &Use { ranges: true, often: true }, false), "plain");
        // Much to write, read again and again: kept on disk.
        assert_eq!(chosen(Can::BY_PATH, &Use::general(), false), "keeper");
        // Written through, copies are no way, nor is what serves read-only: the one that keeps, then the others.
        assert_eq!(chosen(Can::WRITTEN, &Use::general(), true), "eager");
        let without: [&dyn Backend; 2] = [&plain, &keeper];
        let written = choose_among(&without, &Setting::Auto, Can::WRITTEN, at, &Use::general(), false).unwrap();
        assert_eq!(written.map(Backend::name), Some("plain"));
        // A setting naming one is heard whatever the use; `none` is copies.
        let named =
            choose_among(&all, &Setting::Named("eager".into()), Can::BY_PATH, at, &Use::default(), true).unwrap();
        assert_eq!(named.map(Backend::name), Some("eager"));
        assert!(choose_among(&all, &Setting::None, Can::BY_PATH, at, &Use::general(), false).unwrap().is_none());
    }

    #[test]
    fn who_asks_is_heard_before_the_project_and_the_project_before_the_machine() {
        assert_eq!(setting(&[None, None, None]), Setting::Auto);
        assert_eq!(setting(&[None, None, Some("cloud")]), Setting::Named("cloud".into()));
        assert_eq!(setting(&[None, Some("none"), Some("cloud")]), Setting::None);
        assert_eq!(setting(&[Some("auto"), Some("none"), Some("cloud")]), Setting::Auto);
    }
}
