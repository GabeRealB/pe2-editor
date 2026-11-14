//! What a build of the editor needs and is no crate (`prepare.toml`: on Windows, the DirectX Shader Compiler), held to
//! its pin: a build for a platform needing something fails, naming the task that prepares it (`cargo xtask prepare`),
//! until it is prepared and as it is pinned. Then it is put beside the programs the build makes, in the profile's folder
//! (`target/<profile>/`), where the editor looks for it - as a release has it beside them (`cargo xtask dist`). Tests,
//! which cargo runs from folders of their own, are given where it was prepared (`PE2_PREPARED`), which only a test
//! build of the editor reads: no program anyone runs carries the path of the machine it was built on.
#![allow(clippy::disallowed_methods, reason = "a build script: it lays out files by the folders cargo gives it")]

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets it"));
    let workspace = manifest_dir.ancestors().nth(2).expect("the workspace holds crates/editor").to_path_buf();
    let listing = workspace.join("prepare.toml");
    println!("cargo:rerun-if-changed={}", listing.display());
    let os = std::env::var("CARGO_CFG_TARGET_OS").expect("cargo sets it");
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").expect("cargo sets it");
    let platform = format!("{os}-{arch}");
    let needed = needed(&listing, &platform);
    if needed.is_empty() {
        return;
    }
    let target = std::env::var("TARGET").expect("cargo sets it");
    let folder = workspace.join("prepared").join(&platform);
    for (file, sha256) in &needed {
        let prepared = folder.join(file);
        println!("cargo:rerun-if-changed={}", prepared.display());
        let held = std::fs::read(&prepared).ok().map(|bytes| hex(&Sha256::digest(&bytes)));
        if held.as_deref() != Some(sha256.as_str()) {
            let why = if held.is_some() { "is not the one pinned" } else { "is not prepared" };
            panic!("{} {why} (prepare.toml): run `cargo xtask prepare --target {target}`", prepared.display());
        }
    }
    // Beside the programs: in the folder holding the `build` folder this runs in (`<profile>/build/<crate>/<hash>/out`,
    // `<profile>/build/<crate>-<hash>/out` before), which cargo does not name to a build script.
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets it"));
    let profile =
        out.ancestors().find(|folder| folder.file_name().is_some_and(|name| name == "build")).and_then(Path::parent);
    match profile {
        Some(profile) => {
            for (file, _) in &needed {
                put(&folder.join(file), &profile.join(file));
            }
        }
        None => println!(
            "cargo:warning=no profile folder holds {}: what the programs need is not put beside them",
            out.display()
        ),
    }
    // For tests: where it was prepared.
    println!("cargo:rustc-env=PE2_PREPARED={}", folder.display());
}

/// A file put in a folder, where it is not there as it is: one a running program holds open is not copied over again,
/// which Windows refuses.
fn put(from: &Path, to: &Path) {
    let same = |a: &Path, b: &Path| {
        let (Ok(a), Ok(b)) = (std::fs::metadata(a), std::fs::metadata(b)) else { return false };
        a.len() == b.len() && b.modified().ok() >= a.modified().ok()
    };
    if same(from, to) {
        return;
    }
    if let Some(folder) = to.parent() {
        std::fs::create_dir_all(folder).unwrap_or_else(|e| panic!("{}: {e}", folder.display()));
    }
    std::fs::copy(from, to).unwrap_or_else(|e| panic!("{} to {}: {e}", from.display(), to.display()));
}

/// What a platform needs, by `prepare.toml`: each file it takes, with its SHA-256.
fn needed(listing: &Path, platform: &str) -> Vec<(String, String)> {
    let text = std::fs::read_to_string(listing).unwrap_or_else(|e| panic!("{}: {e}", listing.display()));
    let said: toml::Table = text.parse().unwrap_or_else(|e| panic!("{}: {e}", listing.display()));
    let entries = said.get("setup").and_then(toml::Value::as_array).cloned().unwrap_or_default();
    let text_of = |table: &toml::Table, key: &str| table.get(key).and_then(toml::Value::as_str).map(str::to_string);
    let mut needed = Vec::new();
    for entry in entries.iter().filter_map(toml::Value::as_table) {
        if text_of(entry, "platform").is_some_and(|named| named != platform) {
            continue;
        }
        let take = entry.get("take").and_then(toml::Value::as_table).expect("an entry says what it takes");
        // A single file (`{ file, sha256 }`), or an archive's members, each `{ to, sha256 }`.
        let places: Vec<(String, String)> = match (text_of(take, "file"), text_of(take, "sha256")) {
            (Some(file), Some(sha256)) => vec![(file, sha256)],
            _ => take
                .values()
                .filter_map(toml::Value::as_table)
                .filter_map(|taken| Some((text_of(taken, "to")?, text_of(taken, "sha256")?)))
                .collect(),
        };
        needed.extend(places.into_iter().map(|(file, sha256)| (file, sha256.to_ascii_lowercase())));
    }
    needed
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut text, b| {
        let _ = write!(text, "{b:02x}");
        text
    })
}
