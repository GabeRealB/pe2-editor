//! Unsandboxed extensions: extensions like any other - a component, found where components are, its dependencies and
//! services the same - that say they run outside the sandbox (`unsandboxed` in `extension!`). Such a one has all of
//! WASI and may launch programs (the contract's `unsandboxed`): those it ships, or whatever it finds on the system. What
//! uses it depends on it and calls its operations and services as any extension's, not knowing what is behind them.
//!
//! None of it runs until the user trusts it: it is read for who it is, and its component not even instantiated. What is
//! enabled is this extension by its hash (`trust`), or the place it is in, kept with the user and never in a project, so
//! a project cannot enable what it brings. One enabled by its hash that changes is asked about again.
//!
//! Its folder (`folder`) holds its component and what it ships. A program it launches by a relative name is looked for
//! in `<platform>/` of its folder, then its folder, then on the system: one component serves every platform it ships
//! programs for. What the system has to provide its installation notes say (`install` in its manifest), which a program
//! it cannot start points to.

use pe2_path::{Name, RelPath};
use sfs::HostPath;
use std::fmt::Write as _;
use std::sync::Arc;

use anyhow::{Context, Result};

/// The platform this editor runs on, as an extension names it.
#[must_use]
pub fn platform() -> String {
    pe2_setup::platform()
}

/// Why an extension does not run here; none where it does: it names no platform, or this one.
#[must_use]
pub fn elsewhere(platforms: &[String]) -> Option<String> {
    let here = platform();
    (!platforms.is_empty() && !platforms.contains(&here))
        .then(|| format!("it runs on {}, and this editor on {here}", platforms.join(", ")))
}

/// Where an extension's programs are: `tool` in what it ships, for this platform or any, then in what its setup fetched
/// (`fetched`, each folder laid out as its tree is); else `tool` as it is, for the system to find.
#[must_use]
pub fn program(folder: Option<&HostPath>, fetched: &[HostPath], tool: &str, io: &Arc<dyn pe2_io::Io>) -> Runnable {
    let Ok(within) = RelPath::new(tool) else { return Runnable::Named(tool.to_owned()) };
    let exe = format!("{}.{}", within.name().unwrap_or_default(), std::env::consts::EXE_EXTENSION);
    let exe = Name::new(&exe).ok();
    let with_exe = exe
        .filter(|_| !std::env::consts::EXE_EXTENSION.is_empty())
        .map(|exe| within.parent().unwrap_or_default().join(exe));
    let named: Vec<Arc<RelPath>> = with_exe.into_iter().chain([within.to_owned()]).collect();
    let found =
        named.iter().flat_map(|name| places(folder, fetched, name)).find(|path| crate::content::is_file(io, path));
    found.map_or_else(|| Runnable::Named(tool.to_owned()), |p| Runnable::At(runnable(&p, io)))
}

/// Where a place in an extension's tree may be, in order: in what it ships, for this platform then for any, then in
/// what its setup fetched.
fn places(folder: Option<&HostPath>, fetched: &[HostPath], within: &RelPath) -> Vec<HostPath> {
    let platform = platform();
    let platform = Name::new(&platform).ok();
    let shipped = folder.into_iter().flat_map(|folder| {
        let own = platform.as_ref().map(|platform| folder.join(platform).join(within));
        own.into_iter().chain([folder.join(within)])
    });
    shipped.chain(fetched.iter().map(|at| at.join(within))).collect()
}

/// What a program is started as: a place on disk, or a name the system looks for where it looks for programs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Runnable {
    At(HostPath),
    Named(String),
}

impl Runnable {
    /// What the system is given to start it.
    #[must_use]
    pub fn as_os_str(&self) -> &std::ffi::OsStr {
        match self {
            Runnable::At(path) => path.as_os_str(),
            Runnable::Named(name) => std::ffi::OsStr::new(name),
        }
    }
}

impl std::fmt::Display for Runnable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Runnable::At(path) => write!(f, "{path}"),
            Runnable::Named(name) => f.write_str(name),
        }
    }
}

/// A path a program can be handed for a place in an extension's tree: itself on disk; one a project holds, through the
/// filesystem the project is served as (`content::for_program`), its tree laid out nowhere.
fn runnable(path: &HostPath, io: &Arc<dyn pe2_io::Io>) -> HostPath {
    if crate::records::held_at(io, path).is_some() {
        return crate::content::for_program(io, path).unwrap_or_else(|_| path.clone());
    }
    path.clone()
}

/// A program an extension offers by name (`[[program]]` of its manifest): it starts it by that name, and so do others,
/// as `<extension>:<name>`. `program` is a place in its tree (shipped or fetched, found as `program` finds one) or
/// another extension's program (`@<extension>:<name>`); each argument starting `@` a place in its tree, or another
/// extension's program, its path; what the one starting it passes follows.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Program {
    /// What it is started as.
    pub name: String,
    /// The platform it is for; none, every platform.
    #[serde(default)]
    pub platform: Option<String>,
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
}

impl Program {
    /// Whether it is for the platform this editor runs on.
    #[must_use]
    pub fn here(&self) -> bool {
        self.platform.as_deref().is_none_or(|named| named == platform())
    }
}

/// What the extensions loaded offer to run, by id: each unsandboxed one, filled once all are loaded. What starts
/// `<extension>:<name>` - an extension, a project's operation - finds it here.
pub type Offered = std::sync::Arc<
    std::sync::OnceLock<std::collections::BTreeMap<String, std::sync::Arc<super::component::Unsandboxed>>>,
>;

/// A place in an extension's tree, where it is: in what it ships, for this platform or any, then in what its setup
/// fetched.
#[must_use]
pub fn in_tree(
    folder: Option<&HostPath>,
    fetched: &[HostPath],
    place: &str,
    io: &Arc<dyn pe2_io::Io>,
) -> Option<HostPath> {
    let within = RelPath::new(place).ok()?;
    let found = places(folder, fetched, within).into_iter();
    found
        .into_iter()
        .find(|path| crate::content::is_file(io, path) || crate::content::is_dir(io, path))
        .map(|path| runnable(&path, io))
}

/// The files in a folder - on disk, or an extension's a project holds (`records::held_at`) - by their paths within it,
/// in order.
fn files_in(
    io: &Arc<dyn pe2_io::Io>,
    folder: &HostPath,
    within: &RelPath,
    files: &mut Vec<(Arc<RelPath>, HostPath)>,
) -> Result<()> {
    if !crate::content::is_dir(io, folder) {
        anyhow::bail!("{}: no such folder", folder.display());
    }
    let mut entries = crate::content::read_dir(io, folder);
    entries.sort_by(|a, b| crate::content::shown(&a.0).cmp(crate::content::shown(&b.0)));
    for (name, path, is_folder) in entries {
        let name = within.join(&name.with_context(|| format!("{}: what it holds", folder.display()))?);
        if is_folder {
            files_in(io, &path, &name, files)?;
        } else {
            files.push((name, path));
        }
    }
    Ok(())
}

/// What of an extension's folder never counts as it: its tests, which what ships may not have.
pub const TESTS: &str = "tests";

/// The hash of an extension, what the user enables: its component, or of one in a folder of its own everything there
/// that runs, so that a new program is another extension - but what is only read (`folder::read_only`: its manifest,
/// which its component carries, its guides, licenses, changelog, icon and tests). What its setup fetches is named by its
/// manifest, with its hashes: trusting it covers that too.
///
/// # Errors
/// What it holds cannot be read.
pub fn trust(
    file: &HostPath,
    folder: Option<&HostPath>,
    about: &super::component::About,
    io: &Arc<dyn pe2_io::Io>,
) -> Result<String> {
    let Some(folder) = folder else {
        // What trust is kept as: the hash's text.
        return crate::content::sha1(io, file).map(|sha1| sha1.to_string()).with_context(|| file.display().to_string());
    };
    let mut files = Vec::new();
    files_in(io, folder, RelPath::root(), &mut files)?;
    let counts = |name: &RelPath| {
        file.file_name().is_some_and(|own| own == name.as_str()) || !super::folder::read_only(name, about)
    };
    let mut listing = String::new();
    for (name, path) in files.into_iter().filter(|(name, _)| counts(name)) {
        // A held one's from its tree, read from nothing.
        let sha1 = crate::content::sha1(io, &path).with_context(|| path.display().to_string())?;
        let _ = writeln!(listing, "{name} {sha1}");
    }
    Ok(crate::extract::sha1_hex(listing.as_bytes()))
}

#[cfg(test)]
#[allow(
    clippy::disallowed_methods,
    reason = "a test's fixtures: the machine's time, places, files and environment, which nothing tested reads"
)]
mod tests {
    use super::{elsewhere, platform, program, trust};
    use crate::ext::component::About;
    use crate::testing::At as _;
    use sfs::HostPath;

    fn about() -> About {
        About::default()
    }

    fn folder(name: &str) -> HostPath {
        let dir = crate::testing::scratch(&format!("unsandboxed-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.at("bin")).unwrap();
        std::fs::write(dir.at("extension.wasm"), b"").unwrap();
        std::fs::write(dir.at("bin/tool"), b"a tool").unwrap();
        dir
    }

    #[test]
    fn a_platform_is_named_or_any() {
        assert_eq!(elsewhere(&[]), None);
        assert_eq!(elsewhere(&["another-one".to_string(), platform()]), None);
        let why = elsewhere(&["another-one".to_string()]).unwrap();
        assert!(why.contains("it runs on another-one"), "{why}");
    }

    #[test]
    fn what_is_enabled_is_all_it_ships() {
        let dir = folder("trust");
        let component = dir.at("extension.wasm");
        let whole = trust(&component, Some(&dir), &about(), &pe2_io::system()).unwrap();
        assert_eq!(trust(&component, Some(&dir), &about(), &pe2_io::system()).unwrap(), whole);
        // A program it ships, changed: another extension to the user, and what it made is made again.
        std::fs::write(dir.at("bin/tool"), b"another tool").unwrap();
        assert_ne!(trust(&component, Some(&dir), &about(), &pe2_io::system()).unwrap(), whole);
        // Moved, too.
        let changed = trust(&component, Some(&dir), &about(), &pe2_io::system()).unwrap();
        std::fs::rename(dir.at("bin/tool"), dir.at("tool")).unwrap();
        assert_ne!(trust(&component, Some(&dir), &about(), &pe2_io::system()).unwrap(), changed);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_program_is_looked_for_in_what_it_ships_then_on_the_system() {
        let dir = folder("program");
        let named = |name: &str| super::Runnable::Named(name.to_string());
        assert_eq!(program(None, &[], "tool", &crate::testing::io()), named("tool"));
        assert_eq!(program(Some(&dir), &[], "ffmpeg", &crate::testing::io()), named("ffmpeg"));
        assert_eq!(
            program(Some(&dir), &[], "bin/tool", &crate::testing::io()),
            super::Runnable::At(dir.at("bin/tool"))
        );
        // What setup fetched, after what it ships.
        let fetched = dir.at("fetched");
        std::fs::create_dir_all(&fetched).unwrap();
        std::fs::write(fetched.at("tool"), b"fetched").unwrap();
        let at = super::Runnable::At;
        assert_eq!(
            program(Some(&dir), std::slice::from_ref(&fetched), "tool", &crate::testing::io()),
            at(fetched.at("tool"))
        );
        std::fs::write(dir.at("tool"), b"any platform's").unwrap();
        assert_eq!(
            program(Some(&dir), std::slice::from_ref(&fetched), "tool", &crate::testing::io()),
            at(dir.at("tool"))
        );
        std::fs::create_dir(dir.at(&platform())).unwrap();
        std::fs::write(dir.at(&platform()).at("tool"), b"this platform's").unwrap();
        assert_eq!(program(Some(&dir), &[], "tool", &crate::testing::io()), at(dir.at(&platform()).at("tool")));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// An unsandboxed extension in a folder, offering these programs, seeing those offered beside it.
    fn offering(
        id: &str,
        dir: &HostPath,
        programs: &str,
        offered: &super::Offered,
    ) -> std::sync::Arc<crate::ext::component::Unsandboxed> {
        let programs: toml::Table = toml::from_str(programs).unwrap();
        let programs = programs.get("program").cloned().map_or_default(|p| p.try_into().unwrap());
        std::sync::Arc::new(crate::ext::component::Unsandboxed {
            id: id.to_string(),
            folder: Some(dir.clone()),
            trust: format!("{id}-hash"),
            install: None,
            store: None,
            setup: vec![],
            off: std::collections::BTreeSet::new(),
            programs,
            offered: offered.clone(),
            io: pe2_io::system(),
        })
    }

    #[test]
    fn a_program_is_started_through_the_extensions_that_offer_what_it_names() {
        let root = crate::testing::scratch("programs");
        let _ = std::fs::remove_dir_all(&root);
        let (python, binutils, maspsx) = (root.at("python"), root.at("binutils"), root.at("maspsx"));
        for (dir, file) in [(&python, "bin/python3"), (&binutils, "as"), (&maspsx, "maspsx/maspsx.py")] {
            std::fs::create_dir_all(dir.at(file).parent().unwrap()).unwrap();
            std::fs::write(dir.at(file), b"a program").unwrap();
        }
        let offered = super::Offered::default();
        let python_ext =
            offering("pe2.python", &python, "[[program]]\nname = \"python3\"\nprogram = \"bin/python3\"\n", &offered);
        let binutils_ext =
            offering("pe2.binutils", &binutils, "[[program]]\nname = \"as\"\nprogram = \"as\"\n", &offered);
        let maspsx_ext = offering(
            "pe2.maspsx",
            &maspsx,
            "[[program]]\nname = \"as\"\nprogram = \"@pe2.python:python3\"\n\
             args = [\"-B\", \"@maspsx/maspsx.py\", \"--gnu-as-path\", \"@pe2.binutils:as\"]\n",
            &offered,
        );
        let all = [&python_ext, &binutils_ext, &maspsx_ext].map(|u| (u.id.clone(), (*u).clone()));
        offered.set(all.into_iter().collect()).unwrap();
        // Named from anywhere: the interpreter another offers, the script in its own tree, a third's assembler.
        let (program, args) = maspsx_ext.start("pe2.maspsx:as").unwrap();
        assert_eq!(program, super::Runnable::At(python.at("bin/python3")));
        let (script, gnu_as) = (maspsx.at("maspsx/maspsx.py"), binutils.at("as"));
        assert_eq!(args, ["-B", &script.to_string(), "--gnu-as-path", &gnu_as.to_string()]);
        // By its own name, from itself.
        assert_eq!(maspsx_ext.start("as").unwrap().0, super::Runnable::At(python.at("bin/python3")));
        // What is not there is said by name.
        let missing = maspsx_ext.start("pe2.nothing:cc1").unwrap_err();
        assert!(format!("{missing:#}").contains("no extension pe2.nothing offering programs runs here"), "{missing:#}");
        let unoffered = binutils_ext.start("pe2.binutils:ld").unwrap_err();
        assert!(format!("{unoffered:#}").contains("offers no program ld"), "{unoffered:#}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn what_is_only_read_is_not_the_extension() {
        let dir = crate::testing::scratch("read-only");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.at("tests")).unwrap();
        let component = dir.at("extension.wasm");
        std::fs::write(&component, "component").unwrap();
        std::fs::write(dir.at("program"), "one").unwrap();
        let whole = trust(&component, Some(&dir), &about(), &pe2_io::system()).unwrap();
        // Tests come and go; guides, licenses and the manifest the component carries are only read.
        std::fs::write(dir.at("tests/tests.wasm"), "tests").unwrap();
        std::fs::create_dir_all(dir.at("docs")).unwrap();
        std::fs::write(dir.at("docs/install.md"), "install").unwrap();
        std::fs::write(dir.at("extension.toml"), "id = \"x\"").unwrap();
        std::fs::write(dir.at("LICENSE"), "free").unwrap();
        assert_eq!(trust(&component, Some(&dir), &about(), &pe2_io::system()).unwrap(), whole);
        // A program is the extension, wherever it is.
        std::fs::write(dir.at("program"), "two").unwrap();
        assert_ne!(trust(&component, Some(&dir), &about(), &pe2_io::system()).unwrap(), whole);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
