//! The fold names are compared by (`fold.rs`), made from the Unicode Character Database `cargo xtask prepare` fetches
//! into `prepared/common/ucd/` (`prepare.toml`), held to its pins: a build fails, naming that task, until it is
//! prepared and as it is pinned.
//!
//! A character folds to the full case folding (`CaseFolding.txt`, its common and full mappings) of its simple uppercase
//! (`UnicodeData.txt`): what full case folding takes as alike stays alike, and what NTFS takes as alike - characters of
//! one uppercase, `ı` and `i` among them - becomes so. What is written is each character the fold changes, by code
//! point, with what it folds to, and the database's version.
#![allow(clippy::disallowed_methods, reason = "a build script: it reads files by the folders cargo gives it")]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

const UNICODE_DATA: &str = "ucd/UnicodeData.txt";
const CASE_FOLDING: &str = "ucd/CaseFolding.txt";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets it"));
    let workspace = manifest_dir.ancestors().nth(2).expect("the workspace holds crates/path").to_path_buf();
    let listing = workspace.join("prepare.toml");
    println!("cargo:rerun-if-changed={}", listing.display());
    let pins = pins(&listing);
    let common = workspace.join("prepared").join("common");
    let read = |file: &str| {
        let prepared = common.join(file);
        println!("cargo:rerun-if-changed={}", prepared.display());
        let pin = pins.get(file).unwrap_or_else(|| panic!("prepare.toml prepares no {file} for every platform"));
        let bytes = std::fs::read(&prepared).ok();
        let held = bytes.as_deref().map(|bytes| hex(&Sha256::digest(bytes)));
        if held.as_deref() != Some(pin.as_str()) {
            let why = if held.is_some() { "is not the one pinned" } else { "is not prepared" };
            panic!("{} {why} (prepare.toml): run `cargo xtask prepare`", prepared.display());
        }
        String::from_utf8(bytes.unwrap_or_default()).unwrap_or_else(|e| panic!("{}: {e}", prepared.display()))
    };
    let (data, folding) = (read(UNICODE_DATA), read(CASE_FOLDING));
    let version = folding
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("# CaseFolding-"))
        .and_then(|rest| rest.strip_suffix(".txt"))
        .expect("CaseFolding.txt names its version in its first line");
    let uppercase = simple_uppercase(&data);
    let case_folds = case_folding(&folding);
    let mut table = String::new();
    let _ = writeln!(table, "/// The version of the Unicode Character Database the fold is made of.");
    let _ = writeln!(table, "pub const UNICODE_VERSION: &str = {version:?};");
    let _ = writeln!(table, "/// Each character the fold changes, by code point, with what it folds to: in order.");
    let _ = writeln!(table, "pub(crate) static FOLDS: &[(u32, &str)] = &[");
    for code in (0..=0x10_FFFF_u32).filter(|code| !(0xD800..=0xDFFF).contains(code)) {
        let simple = uppercase.get(&code).copied().unwrap_or(code);
        let folded: Vec<u32> = case_folds.get(&simple).cloned().unwrap_or_else(|| vec![simple]);
        if folded != [code] {
            // Escaped: what a fold gives need not be in the form text is written in.
            let text = folded.iter().fold(String::new(), |mut text, c| {
                let _ = write!(text, "\\u{{{c:x}}}");
                text
            });
            let _ = writeln!(table, "    ({code:#x}, \"{text}\"),");
        }
    }
    let _ = writeln!(table, "];");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets it"));
    std::fs::write(out.join("fold.rs"), table).expect("the fold's table written");
}

/// Each character's simple uppercase, where it has one: `UnicodeData.txt`'s thirteenth field.
fn simple_uppercase(data: &str) -> BTreeMap<u32, u32> {
    let code = |text: &str| u32::from_str_radix(text, 16).unwrap_or_else(|e| panic!("{text}: {e}"));
    data.lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(';').collect();
            let upper = fields.get(12).filter(|upper| !upper.is_empty())?;
            Some((code(fields[0]), code(upper)))
        })
        .collect()
}

/// Each character's full case folding: its common mapping, or its full one where it has one (`C` and `F`).
fn case_folding(folding: &str) -> BTreeMap<u32, Vec<u32>> {
    let code = |text: &str| u32::from_str_radix(text, 16).unwrap_or_else(|e| panic!("{text}: {e}"));
    let mut fold = BTreeMap::new();
    for line in folding.lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        let fields: Vec<&str> = line.split(';').map(str::trim).collect();
        let [from, status, to, ..] = fields[..] else { continue };
        let to: Vec<u32> = to.split_whitespace().map(code).collect();
        match status {
            "F" => drop(fold.insert(code(from), to)),
            "C" => drop(fold.entry(code(from)).or_insert(to)),
            _ => {}
        }
    }
    fold
}

/// What `prepare.toml` pins for every platform: each file it takes, with its SHA-256.
fn pins(listing: &Path) -> BTreeMap<String, String> {
    let text = std::fs::read_to_string(listing).unwrap_or_else(|e| panic!("{}: {e}", listing.display()));
    let said: toml::Table = text.parse().unwrap_or_else(|e| panic!("{}: {e}", listing.display()));
    let entries = said.get("setup").and_then(toml::Value::as_array).cloned().unwrap_or_default();
    let text_of = |table: &toml::Table, key: &str| table.get(key).and_then(toml::Value::as_str).map(str::to_string);
    entries
        .iter()
        .filter_map(toml::Value::as_table)
        .filter(|entry| entry.get("platform").is_none())
        .filter_map(|entry| {
            let take = entry.get("take").and_then(toml::Value::as_table)?;
            Some((text_of(take, "file")?, text_of(take, "sha256")?.to_ascii_lowercase()))
        })
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut text, b| {
        let _ = write!(text, "{b:02x}");
        text
    })
}
