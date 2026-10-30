//! Unsandboxed extensions: extensions like any other - a component, found where components are, its dependencies and
//! services the same - that say they run outside the sandbox (`unsandboxed` in `extension!`). Such a one has all of
//! WASI and may launch programs (the contract's `unsandboxed`): those it ships, or whatever it finds on the system. What
//! uses it depends on it and calls its operations and services as any extension's, not knowing what is behind them.
//!
//! Where it is makes no difference, as none of it runs until the user enables it. It is read for who it is, and its
//! component not even instantiated. What is enabled is this extension by its hash (`trust`), kept with the user and
//! never in a project, so a project cannot enable what it brings, and one that changes is asked about again.
//!
//! An extension may be a component alone, or a folder holding `extension.wasm` and what it ships. A program it launches
//! by a relative name is looked for in `<platform>/` of its folder, then its folder, then on the system: one component
//! serves every platform it ships programs for.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// The platform this editor runs on, as an extension names it.
#[must_use]
pub fn platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// Why an extension does not run here; none where it does: it names no platform, or this one.
#[must_use]
pub fn elsewhere(platforms: &[String]) -> Option<String> {
    let here = platform();
    (!platforms.is_empty() && !platforms.contains(&here)).then(|| format!("it runs on {}, and this editor on {here}", platforms.join(", ")))
}

/// Where an extension's programs are: `tool` in what it ships, for this platform or any; else `tool` as it is, for the
/// system to find.
#[must_use]
pub fn program(folder: Option<&Path>, tool: &str) -> PathBuf {
    let shipped = folder.into_iter().flat_map(|folder| [folder.join(platform()).join(tool), folder.join(tool)]);
    let named = shipped.flat_map(|path| [path.with_added_extension(std::env::consts::EXE_EXTENSION), path]);
    named.into_iter().find(|p| p.is_file()).unwrap_or_else(|| PathBuf::from(tool))
}

/// The files in a folder, by their paths within it, in order.
fn files_in(folder: &Path, within: &Path, files: &mut Vec<(PathBuf, PathBuf)>) -> Result<()> {
    let mut entries = std::fs::read_dir(folder).with_context(|| folder.display().to_string())?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let (path, name) = (entry.path(), within.join(entry.file_name()));
        if path.is_dir() {
            files_in(&path, &name, files)?;
        } else {
            files.push((name, path));
        }
    }
    Ok(())
}

/// The hash of an extension whole, what the user enables and what it makes is kept by: its component, or of one in a
/// folder everything there, so a new program is another extension.
///
/// # Errors
/// What it holds cannot be read.
pub fn trust(file: &Path, folder: Option<&Path>) -> Result<String> {
    let Some(folder) = folder else { return crate::extract::sha1_file(file).with_context(|| file.display().to_string()) };
    let mut files = Vec::new();
    files_in(folder, Path::new(""), &mut files)?;
    let mut hashes = String::new();
    for (name, path) in files {
        let sha1 = crate::extract::sha1_file(&path).with_context(|| path.display().to_string())?;
        let _ = writeln!(hashes, "{} {sha1}", name.display());
    }
    Ok(crate::extract::sha1_hex(hashes.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::{elsewhere, platform, program, trust};

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
        let whole = trust(&component, Some(&dir)).unwrap();
        assert_eq!(trust(&component, Some(&dir)).unwrap(), whole);
        // A program it ships, changed: another extension to the user, and what it made is made again.
        std::fs::write(dir.join("bin/tool"), b"another tool").unwrap();
        assert_ne!(trust(&component, Some(&dir)).unwrap(), whole);
        // Moved, too.
        let changed = trust(&component, Some(&dir)).unwrap();
        std::fs::rename(dir.join("bin/tool"), dir.join("tool")).unwrap();
        assert_ne!(trust(&component, Some(&dir)).unwrap(), changed);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_program_is_looked_for_in_what_it_ships_then_on_the_system() {
        let dir = folder("program");
        assert_eq!(program(None, "tool"), std::path::Path::new("tool"));
        assert_eq!(program(Some(&dir), "ffmpeg"), std::path::Path::new("ffmpeg"));
        assert_eq!(program(Some(&dir), "bin/tool"), dir.join("bin/tool"));
        std::fs::write(dir.join("tool"), b"any platform's").unwrap();
        assert_eq!(program(Some(&dir), "tool"), dir.join("tool"));
        std::fs::create_dir(dir.join(platform())).unwrap();
        std::fs::write(dir.join(platform()).join("tool"), b"this platform's").unwrap();
        assert_eq!(program(Some(&dir), "tool"), dir.join(platform()).join("tool"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
