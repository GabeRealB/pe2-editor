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

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

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
pub fn program(folder: Option<&Path>, fetched: &[PathBuf], tool: &str) -> PathBuf {
    let shipped = folder.into_iter().flat_map(|folder| [folder.join(platform()).join(tool), folder.join(tool)]);
    let shipped = shipped.chain(fetched.iter().map(|place| place.join(tool)));
    let named = shipped.flat_map(|path| [path.with_added_extension(std::env::consts::EXE_EXTENSION), path]);
    named.into_iter().find(|p| crate::pack::is_file(p)).map_or_else(|| PathBuf::from(tool), |p| runnable(&p))
}

/// A path a program can be handed for a place in an extension's tree: itself on disk; one a project holds, through the
/// filesystem the project is served as (`content::for_program`), its tree laid out nowhere.
fn runnable(path: &Path) -> PathBuf {
    if crate::records::held_at(path).is_some() {
        return crate::content::for_program(path).unwrap_or_else(|_| path.to_path_buf());
    }
    path.to_path_buf()
}

/// A program an extension starts by name that is another program with arguments (`[[launch]]` of its manifest): a
/// script with the interpreter its setup fetches, say. `program` and each argument starting `@` are places in its tree,
/// found as a program is (`program`); what the extension passes follows.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Launch {
    /// What the extension starts it as.
    pub name: String,
    /// The platform it is for; none, every platform.
    #[serde(default)]
    pub platform: Option<String>,
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
}

impl Launch {
    /// Whether it is for the platform this editor runs on.
    #[must_use]
    pub fn here(&self) -> bool {
        self.platform.as_deref().is_none_or(|named| named == platform())
    }

    /// What it starts, and with what before what the extension passes: its places in the extension's tree, found.
    #[must_use]
    pub fn resolved(&self, folder: Option<&Path>, fetched: &[PathBuf]) -> (PathBuf, Vec<String>) {
        let args = self.args.iter().map(|arg| match arg.strip_prefix('@') {
            Some(place) => {
                in_tree(folder, fetched, place).map_or_else(|| place.to_string(), |p| p.display().to_string())
            }
            None => arg.clone(),
        });
        (program(folder, fetched, &self.program), args.collect())
    }
}

/// A place in an extension's tree, where it is: in what it ships, for this platform or any, then in what its setup
/// fetched.
#[must_use]
pub fn in_tree(folder: Option<&Path>, fetched: &[PathBuf], place: &str) -> Option<PathBuf> {
    let shipped = folder.into_iter().flat_map(|folder| [folder.join(platform()).join(place), folder.join(place)]);
    let found = shipped.chain(fetched.iter().map(|at| at.join(place)));
    found.into_iter().find(|path| crate::pack::is_file(path) || crate::pack::is_dir(path)).map(|path| runnable(&path))
}

/// The files in a folder - on disk, or an extension's a project holds (`records::held_at`) - by their paths within it,
/// in order.
fn files_in(folder: &Path, within: &Path, files: &mut Vec<(PathBuf, PathBuf)>) -> Result<()> {
    if !crate::pack::is_dir(folder) {
        anyhow::bail!("{}: no such folder", folder.display());
    }
    let mut entries = crate::pack::read_dir(folder);
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, path, is_folder) in entries {
        let name = within.join(name);
        if is_folder {
            files_in(&path, &name, files)?;
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
pub fn trust(file: &Path, folder: Option<&Path>, about: &super::component::About) -> Result<String> {
    let Some(folder) = folder else {
        return crate::content::sha1(file).with_context(|| file.display().to_string());
    };
    let mut files = Vec::new();
    files_in(folder, Path::new(""), &mut files)?;
    let counts = |name: &Path| {
        file.file_name().is_some_and(|own| name.as_os_str() == own) || !super::folder::read_only(name, about)
    };
    let mut listing = String::new();
    for (name, path) in files.into_iter().filter(|(name, _)| counts(name)) {
        // A held one's from its tree, read from nothing.
        let sha1 = crate::content::sha1(&path).with_context(|| path.display().to_string())?;
        let _ = writeln!(listing, "{} {sha1}", name.display());
    }
    Ok(crate::extract::sha1_hex(listing.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::{elsewhere, platform, program, trust};
    use crate::ext::component::About;

    fn about() -> About {
        About::default()
    }

    fn folder(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("pe2-unsandboxed-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("extension.wasm"), b"").unwrap();
        std::fs::write(dir.join("bin/tool"), b"a tool").unwrap();
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
        let component = dir.join("extension.wasm");
        let whole = trust(&component, Some(&dir), &about()).unwrap();
        assert_eq!(trust(&component, Some(&dir), &about()).unwrap(), whole);
        // A program it ships, changed: another extension to the user, and what it made is made again.
        std::fs::write(dir.join("bin/tool"), b"another tool").unwrap();
        assert_ne!(trust(&component, Some(&dir), &about()).unwrap(), whole);
        // Moved, too.
        let changed = trust(&component, Some(&dir), &about()).unwrap();
        std::fs::rename(dir.join("bin/tool"), dir.join("tool")).unwrap();
        assert_ne!(trust(&component, Some(&dir), &about()).unwrap(), changed);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_program_is_looked_for_in_what_it_ships_then_on_the_system() {
        let dir = folder("program");
        assert_eq!(program(None, &[], "tool"), std::path::Path::new("tool"));
        assert_eq!(program(Some(&dir), &[], "ffmpeg"), std::path::Path::new("ffmpeg"));
        assert_eq!(program(Some(&dir), &[], "bin/tool"), dir.join("bin/tool"));
        // What setup fetched, after what it ships.
        let fetched = dir.join("fetched");
        std::fs::create_dir_all(&fetched).unwrap();
        std::fs::write(fetched.join("tool"), b"fetched").unwrap();
        assert_eq!(program(Some(&dir), std::slice::from_ref(&fetched), "tool"), fetched.join("tool"));
        std::fs::write(dir.join("tool"), b"any platform's").unwrap();
        assert_eq!(program(Some(&dir), std::slice::from_ref(&fetched), "tool"), dir.join("tool"));
        std::fs::create_dir(dir.join(platform())).unwrap();
        std::fs::write(dir.join(platform()).join("tool"), b"this platform's").unwrap();
        assert_eq!(program(Some(&dir), &[], "tool"), dir.join(platform()).join("tool"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_launch_is_a_program_of_its_tree_with_places_of_it_for_arguments() {
        let dir = folder("launch");
        let fetched = dir.join("fetched");
        std::fs::create_dir_all(fetched.join("python/bin")).unwrap();
        std::fs::write(fetched.join("python/bin/python3.13"), b"an interpreter").unwrap();
        std::fs::create_dir_all(fetched.join("tool")).unwrap();
        std::fs::write(fetched.join("tool/script.py"), b"a script").unwrap();
        let launch: super::Launch = toml::from_str(
            "name = \"tool\"\nprogram = \"python/bin/python3.13\"\nargs = [\"-B\", \"@tool/script.py\", \"@nowhere\"]\n",
        )
        .unwrap();
        assert!(launch.here());
        let (program, args) = launch.resolved(Some(&dir), std::slice::from_ref(&fetched));
        assert_eq!(program, fetched.join("python/bin/python3.13"));
        let script = fetched.join("tool/script.py").display().to_string();
        assert_eq!(args, ["-B".to_string(), script, "nowhere".to_string()]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn what_is_only_read_is_not_the_extension() {
        let dir = std::env::temp_dir().join(format!("pe2-read-only-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("tests")).unwrap();
        let component = dir.join("extension.wasm");
        std::fs::write(&component, "component").unwrap();
        std::fs::write(dir.join("program"), "one").unwrap();
        let whole = trust(&component, Some(&dir), &about()).unwrap();
        // Tests come and go; guides, licenses and the manifest the component carries are only read.
        std::fs::write(dir.join("tests/tests.wasm"), "tests").unwrap();
        std::fs::create_dir_all(dir.join("docs")).unwrap();
        std::fs::write(dir.join("docs/install.md"), "install").unwrap();
        std::fs::write(dir.join("extension.toml"), "id = \"x\"").unwrap();
        std::fs::write(dir.join("LICENSE"), "free").unwrap();
        assert_eq!(trust(&component, Some(&dir), &about()).unwrap(), whole);
        // A program is the extension, wherever it is.
        std::fs::write(dir.join("program"), "two").unwrap();
        assert_ne!(trust(&component, Some(&dir), &about()).unwrap(), whole);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
