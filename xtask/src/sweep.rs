//! `sweep`: what the build folder keeps of builds no longer made, removed. Cargo keeps every unit it ever built - a
//! crate built with other features, for another profile, or before what it depends on changed - each in a folder of
//! its own (`target/debug/build/<package>/<hash>`), and the workspace's crates pile up dozens of them. What the
//! checks build now is asked of cargo itself: it says each unit it made or found fresh. Every other unit's folder
//! goes, and so do incremental caches rustc has not touched in a week, which no unit names.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, ensure};

/// What the checks build: what is kept.
const BUILDS: [&[&str]; 2] = [&["clippy", "--workspace", "--all-targets"], &["test", "--workspace", "--no-run"]];

/// How long an incremental cache rustc has not touched is kept.
const INCREMENTAL_KEPT: Duration = Duration::from_hours(7 * 24);

/// Sweep the build folder of the workspace's debug builds; say what was removed.
///
/// # Errors
/// A build fails - what it would have named is not known, so nothing is removed - or a folder cannot be removed.
pub fn run(workspace: &Path, target: &Path) -> Result<()> {
    let debug = target.join("debug");
    let units = debug.join("build");
    let mut kept = BTreeSet::new();
    for args in BUILDS {
        kept.extend(named(workspace, &units, args)?);
    }
    let mut freed = 0;
    let mut removed = 0;
    for package in std::fs::read_dir(&units).with_context(|| units.display().to_string())?.flatten() {
        for unit in std::fs::read_dir(package.path())?.flatten() {
            let unit = unit.path();
            if !kept.contains(&unit) {
                freed += size_of(&unit);
                removed += 1;
                std::fs::remove_dir_all(&unit).with_context(|| unit.display().to_string())?;
            }
        }
        // A package none of whose units is kept is none of the builds'.
        let _ = std::fs::remove_dir(package.path());
    }
    let mut caches = 0;
    let incremental = debug.join("incremental");
    let now = SystemTime::now();
    for cache in std::fs::read_dir(&incremental).into_iter().flatten().flatten() {
        let touched = last_touched(&cache.path());
        if touched.is_some_and(|touched| now.duration_since(touched).unwrap_or_default() > INCREMENTAL_KEPT) {
            freed += size_of(&cache.path());
            caches += 1;
            std::fs::remove_dir_all(cache.path()).with_context(|| cache.path().display().to_string())?;
        }
    }
    println!("{removed} old units and {caches} incremental caches removed: {} MiB", freed >> 20);
    Ok(())
}

/// The units' folders a build makes or finds fresh, as cargo says them.
fn named(workspace: &Path, units: &Path, args: &[&str]) -> Result<BTreeSet<PathBuf>> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .args(args)
        .arg("--message-format=json")
        .current_dir(workspace)
        .stderr(Stdio::inherit())
        .output()
        .with_context(|| format!("cargo {}", args.join(" ")))?;
    ensure!(output.status.success(), "cargo {} failed: nothing is removed", args.join(" "));
    let mut kept = BTreeSet::new();
    // A program is said by its copy beside the units (`target/debug/pe2-cli`): its unit is found by that copy.
    let mut programs: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Ok(message) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let said: Vec<PathBuf> = match message["reason"].as_str() {
            Some("compiler-artifact") => message["filenames"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|file| file.as_str().map(PathBuf::from))
                .collect(),
            Some("build-script-executed") => message["out_dir"].as_str().map(PathBuf::from).into_iter().collect(),
            _ => continue,
        };
        for file in said {
            if let Some(unit) = unit_of(units, &file) {
                kept.insert(unit);
            } else {
                let package = package_of(&message).context("a message names no package")?;
                programs.entry(package).or_default().push(file);
            }
        }
    }
    for (package, copies) in programs {
        for copy in copies {
            kept.extend(program_units(&units.join(&package), &copy));
        }
    }
    Ok(kept)
}

/// The unit's folder a file is in: `<units>/<package>/<hash>`.
fn unit_of(units: &Path, file: &Path) -> Option<PathBuf> {
    let within = file.strip_prefix(units).ok()?;
    let mut parts = within.components();
    let (package, hash) = (parts.next()?, parts.next()?);
    Some(units.join(package).join(hash))
}

/// A package's name, as its units' folder is named: what follows `#` in its id, before `@`
/// (`path+file:///.../crates/cli#cli@0.1.0`, `registry+...#png@0.18.0`), or, where the id names it by its folder
/// alone (`path+file:///.../crates/cli#0.1.0`), that folder's.
fn package_of(message: &serde_json::Value) -> Option<String> {
    let id = message["package_id"].as_str()?;
    let (place, named) = id.rsplit_once('#')?;
    match named.split_once('@') {
        Some((name, _)) => Some(name.to_string()),
        None => place.rsplit('/').next().map(str::to_string),
    }
}

/// A program's units: those holding a file it is a copy of - the same length and time - or, where none does, every
/// unit of its package that holds one of its name, so that nothing live goes.
fn program_units(package: &Path, copy: &Path) -> Vec<PathBuf> {
    let Some(stem) = copy.file_stem().and_then(|s| s.to_str()) else { return Vec::new() };
    let stem = stem.replace('-', "_");
    let copied = std::fs::metadata(copy).ok().map(|m| (m.len(), m.modified().ok()));
    let mut named = Vec::new();
    let mut same = Vec::new();
    for unit in std::fs::read_dir(package).into_iter().flatten().flatten() {
        for file in std::fs::read_dir(unit.path().join("out")).into_iter().flatten().flatten() {
            let name = file.file_name();
            let Some(name) = name.to_str() else { continue };
            let file_stem = Path::new(name).file_stem().and_then(|s| s.to_str()).unwrap_or(name);
            let Some((built, _)) = file_stem.rsplit_once('-') else { continue };
            if built != stem || Path::new(name).extension() != copy.extension() {
                continue;
            }
            named.push(unit.path());
            let held = file.metadata().ok().map(|m| (m.len(), m.modified().ok()));
            if held.is_some() && held == copied {
                same.push(unit.path());
            }
        }
    }
    if same.is_empty() { named } else { same }
}

/// The last time anything in a folder changed.
fn last_touched(folder: &Path) -> Option<SystemTime> {
    let mut last = std::fs::metadata(folder).and_then(|m| m.modified()).ok();
    for entry in std::fs::read_dir(folder).into_iter().flatten().flatten() {
        let touched =
            if entry.path().is_dir() { last_touched(&entry.path()) } else { entry.metadata().ok()?.modified().ok() };
        last = last.max(touched);
    }
    last
}

/// How many bytes a folder holds.
fn size_of(folder: &Path) -> u64 {
    std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .map(
            |entry| {
                if entry.path().is_dir() { size_of(&entry.path()) } else { entry.metadata().map_or(0, |m| m.len()) }
            },
        )
        .sum()
}
