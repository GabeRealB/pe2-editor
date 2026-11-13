//! The editor's own tasks, run as `cargo xtask <task>`:
//!
//! - `dist [--tests] [--out <folder>]`: the editor built and laid out as it ships, in
//!   `<out>/pe2-<version>-<platform>/` (`target/dist/` where unsaid): `pe2-editor` and `pe2-cli`, what a user reads
//!   of them in `docs/` (the workspace's `docs/`), what they are built with and under which license in
//!   `LICENSES.txt` (`licenses::Listing::document`, each extension's own in its `licenses/`), and beside them in
//!   `extensions/` the extensions built apart from the editor, each a folder as one is kept (`<id>.extension/`): its
//!   component, its manifest and what its crate has to be read (`docs/`, `licenses/`, ...), and what it ships -
//!   what each fetches onto a machine its manifest says (`[[setup]]`). A project adds them from there by id (`pe2-cli project extension add
//!   pe2.psx`), copying them; nothing runs from beside the editor. `--tests` adds `pe2-test-runner`, a build for testing
//!   that what ships has not.
//! - `package [--out <folder>]`: `dist`, then archived: the editor alone, `pe2-<version>-<platform>.zip`, and each
//!   extension beside it in one of its own, `<id>-<version>.extension.zip` holding its folder, every file's Unix
//!   permissions kept and each archive's SHA-256 beside it (`.zip.sha256`, as `sha256sum` writes it).
//! - `hash <manifest> [--write]`: the SHA-256s an extension's `[[setup]]` entries are held to, found by fetching each,
//!   as `pe2-cli extension hash` finds them - which a manifest of the workspace's needs before the workspace builds.
//! - `pins [<manifest>...]`: whether every source each `[[setup]]` entry names still answers - every extension of the
//!   workspace's where none is named - failing where an entry has none left: run now and then, before users find one gone.
//! - `packages [--package <package>]... [--platform <platform>]... [--zig <zig>] [--publish]`: the packages the
//!   extensions download, built from their sources by the zig projects in `packages/` and laid out as their releases,
//!   one a package version - published with `--publish` (`packages.rs`).

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest, Sha256};

mod packages;

const USAGE: &str = "cargo xtask dist [--tests] [--out <folder>]\n\
                     cargo xtask package [--out <folder>]\n\
                     cargo xtask hash <manifest> [--write]\n\
                     cargo xtask pins [<manifest>...]\n\
                     cargo xtask packages [--package <package>]... [--platform <platform>]... [--zig <zig>] [--publish]";

/// What a task is asked to do.
#[derive(Default)]
struct Options {
    tests: bool,
    out: Option<PathBuf>,
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let task = args.next().unwrap_or_default();
    if task == "hash" {
        let manifest = args.next().context(USAGE)?;
        let write = match args.next().as_deref() {
            None => false,
            Some("--write") => true,
            Some(other) => bail!("no option {other}\n{USAGE}"),
        };
        return pe2_setup::hash_manifest(Path::new(&manifest), write);
    }
    if task == "packages" {
        let (workspace, target) = places()?;
        return packages::run(args, &workspace, &target);
    }
    if task == "pins" {
        let named: Vec<PathBuf> = args.map(PathBuf::from).collect();
        return pins(if named.is_empty() { manifests()? } else { named });
    }
    let mut options = Options::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--tests" => options.tests = true,
            "--out" => options.out = Some(args.next().context("--out names a folder")?.into()),
            other => bail!("no option {other}\n{USAGE}"),
        }
    }
    match task.as_str() {
        "dist" => {
            let tree = dist(&options)?;
            println!("{}", tree.display());
        }
        "package" => {
            ensure!(!options.tests, "a package ships no test runner");
            let tree = dist(&options)?;
            for (archive, sum) in package(&tree)? {
                println!("{}\n{sum}", archive.display());
            }
        }
        _ => bail!("no task {task:?}\n{USAGE}"),
    }
    Ok(())
}

/// Every extension manifest of the workspace: `extensions/<crate>/extension.toml`, and its test suites'.
fn manifests() -> Result<Vec<PathBuf>> {
    let (workspace, _) = places()?;
    let mut found = Vec::new();
    for entry in std::fs::read_dir(workspace.join("extensions"))?.flatten() {
        for manifest in [entry.path().join("extension.toml"), entry.path().join("tests/extension.toml")] {
            if manifest.is_file() {
                found.push(manifest);
            }
        }
    }
    found.sort();
    Ok(found)
}

/// `pins`: each manifest's sources asked whether they still answer; an error where an entry has none left.
fn pins(manifests: Vec<PathBuf>) -> Result<()> {
    let mut gone = Vec::new();
    for manifest in manifests {
        // What fetches nothing has nothing to check.
        if !std::fs::read_to_string(&manifest).is_ok_and(|text| text.contains("[[setup]]")) {
            continue;
        }
        println!("== {}", manifest.display());
        gone.extend(
            pe2_setup::check_pins(&manifest)?.into_iter().map(|entry| format!("{}: {entry}", manifest.display())),
        );
    }
    ensure!(gone.is_empty(), "no source answers for:\n  {}", gone.join("\n  "));
    Ok(())
}

/// The workspace's folder, and the target folder.
fn places() -> Result<(PathBuf, PathBuf)> {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).parent().context("the workspace holds xtask")?.to_path_buf();
    let target = std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| workspace.join("target"), PathBuf::from);
    Ok((workspace, target))
}

/// The platform built for, as an extension's folders of what it ships name it (`linux-x86_64`).
fn platform() -> Result<String> {
    let os = match std::env::consts::OS {
        os @ ("linux" | "macos" | "windows") => os,
        other => bail!("no platform for {other}"),
    };
    Ok(format!("{os}-{}", std::env::consts::ARCH))
}

/// `cargo` run in the workspace with these arguments.
fn cargo(workspace: &Path, args: &[&str]) -> Result<()> {
    let program = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(program).current_dir(workspace).args(args).status().context("cargo runs")?;
    ensure!(status.success(), "cargo {} failed", args.join(" "));
    Ok(())
}

/// The editor built and laid out as it ships: the tree's folder.
fn dist(options: &Options) -> Result<PathBuf> {
    let (workspace, target) = places()?;
    // The editor and the CLI; their build builds the extensions apart from them too (`extensions/build.rs`).
    cargo(&workspace, &["build", "--release", "--package", "editor", "--package", "cli"])?;
    if options.tests {
        cargo(
            &workspace,
            &["build", "--release", "--package", "editor", "--features", "tests", "--bin", "pe2-test-runner"],
        )?;
    }
    let release = target.join("release");
    let components = target.join("extensions").join("wasm32-wasip3").join("release");
    let name = format!("pe2-{}-{}", env!("CARGO_PKG_VERSION"), platform()?);
    let tree = options.out.clone().unwrap_or_else(|| target.join("dist")).join(name);
    if tree.exists() {
        std::fs::remove_dir_all(&tree).with_context(|| tree.display().to_string())?;
    }
    std::fs::create_dir_all(&tree).with_context(|| tree.display().to_string())?;
    let mut programs = vec!["pe2-editor", "pe2-cli"];
    if options.tests {
        programs.push("pe2-test-runner");
    }
    for program in programs {
        let file = format!("{program}{}", std::env::consts::EXE_SUFFIX);
        copy(&release.join(&file), &tree.join(&file))?;
    }
    // What a user reads of the editor, as an extension ships its own.
    copy(&workspace.join(DOCS), &tree.join(DOCS))?;
    // What the programs are built with, each license's text with it: what they say themselves (`pe2-cli licenses`).
    let listing = licenses::listing();
    let built_for = platform()?;
    ensure!(listing.target == built_for, "the licenses were gathered for {}, not {built_for}", listing.target);
    std::fs::write(tree.join(LICENSES), listing.document(None)).with_context(|| tree.join(LICENSES).display().to_string())?;
    let extensions = tree.join(EXTENSIONS);
    for (source, component) in [("extensions/psx", "pe2_psx.wasm"), ("extensions/ffmpeg", "pe2_ffmpeg.wasm")] {
        let folder = extension(&workspace.join(source), &components.join(component), &extensions)?;
        // What its component links, beside what its crate says of what it ships and fetches.
        let name = folder.file_name().context("a folder has a name")?.to_string_lossy().into_owned();
        let id = name.strip_suffix(".extension").context("an extension's folder")?;
        ensure!(listing.crates_within(Some(id)).next().is_some(), "no crate is listed as linked into {id}");
        let file = folder.join("licenses").join(LINKED);
        std::fs::create_dir_all(folder.join("licenses")).with_context(|| folder.display().to_string())?;
        std::fs::write(&file, listing.document(Some(id))).with_context(|| file.display().to_string())?;
    }
    Ok(tree)
}

/// The folder of what a user reads of the editor: the workspace's, and the one beside the editor.
const DOCS: &str = "docs";

/// The file beside the editor saying what it is built with, each license's text with it.
const LICENSES: &str = "LICENSES.txt";

/// The same of an extension's component, in its folder's `licenses/`.
const LINKED: &str = "LINKED.txt";

/// The folder beside the editor holding the extensions built apart from it.
const EXTENSIONS: &str = "extensions";

/// The file an extension's folder holds its component in.
const COMPONENT: &str = "extension.wasm";

/// An extension's manifest, beside its crate's `Cargo.toml` and in its folder.
const MANIFEST: &str = "extension.toml";

/// What of an extension's crate is to be read, copied into its folder where it is there.
const READ: &[&str] = &["docs", "licenses", "CHANGELOG.md"];

/// An extension's folder, as one is kept (`<id>.extension`), in `to`: its component, its manifest and what its crate
/// (`source`) has to be read - and the files its manifest names, its setup's patches among them. The folder.
fn extension(source: &Path, component: &Path, to: &Path) -> Result<PathBuf> {
    let text =
        std::fs::read_to_string(source.join(MANIFEST)).with_context(|| source.join(MANIFEST).display().to_string())?;
    let manifest: toml::Table = text.parse().with_context(|| source.join(MANIFEST).display().to_string())?;
    let id = manifest.get("id").and_then(toml::Value::as_str).context("its manifest has no id")?;
    let folder = to.join(format!("{id}.extension"));
    copy(component, &folder.join(COMPONENT))?;
    copy(&source.join(MANIFEST), &folder.join(MANIFEST))?;
    let mut read: Vec<String> = READ.iter().map(ToString::to_string).collect();
    let licenses = std::fs::read_dir(source).with_context(|| source.display().to_string())?.flatten();
    read.extend(licenses.map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with("LICENSE")));
    let named = ["docs", "install", "icon"].iter().filter_map(|key| manifest.get(*key)?.as_str());
    read.extend(named.map(ToString::to_string));
    let bundled = manifest.get("bundled").and_then(toml::Value::as_array).map_or(&[][..], Vec::as_slice);
    read.extend(bundled.iter().filter_map(|b| b.get("text")?.as_str()).map(ToString::to_string));
    // What its setup patches what it fetches with: shipped, as part of it.
    let setup = manifest.get("setup").and_then(toml::Value::as_array).map_or(&[][..], Vec::as_slice);
    let patches = setup.iter().filter_map(|s| s.get("patches")?.as_array()).flatten();
    read.extend(patches.filter_map(|patch| patch.get("file")?.as_str()).map(ToString::to_string));
    for path in read {
        if source.join(&path).exists() {
            copy(&source.join(&path), &folder.join(&path))?;
        }
    }
    Ok(folder)
}

/// A file or a folder copied, a folder whole; a file keeps its permissions.
fn copy(from: &Path, to: &Path) -> Result<()> {
    if from.is_dir() {
        for entry in std::fs::read_dir(from).with_context(|| from.display().to_string())? {
            let entry = entry?;
            copy(&entry.path(), &to.join(entry.file_name()))?;
        }
        return Ok(());
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).with_context(|| parent.display().to_string())?;
    }
    std::fs::copy(from, to).with_context(|| format!("{} to {}", from.display(), to.display()))?;
    Ok(())
}

/// The tree archived beside it: the editor alone, and each extension beside it in one of its own. Each archive's path,
/// and its checksum's line.
fn package(tree: &Path) -> Result<Vec<(PathBuf, String)>> {
    let top = tree.file_name().context("the tree has a name")?.to_string_lossy().into_owned();
    let mut files = Vec::new();
    gather(tree, &top, &mut files)?;
    let extensions = format!("{top}/{EXTENSIONS}/");
    files.retain(|(name, _, _)| !name.starts_with(&extensions));
    let mut archives = vec![archive(files, &tree.with_file_name(format!("{top}.zip")))?];
    let mut folders: Vec<PathBuf> =
        std::fs::read_dir(tree.join(EXTENSIONS)).into_iter().flatten().flatten().map(|e| e.path()).collect();
    folders.sort();
    for folder in folders {
        let name = folder.file_name().context("a folder has a name")?.to_string_lossy().into_owned();
        let manifest: toml::Table = std::fs::read_to_string(folder.join(MANIFEST))?.parse()?;
        let version = manifest.get("version").and_then(toml::Value::as_str).context("its manifest has no version")?;
        let id = name.strip_suffix(".extension").context("an extension's folder")?;
        let mut files = Vec::new();
        gather(&folder, &name, &mut files)?;
        archives.push(archive(files, &tree.with_file_name(format!("{id}-{version}.extension.zip")))?);
    }
    Ok(archives)
}

/// Files archived, with the archive's SHA-256 beside it: the archive's path and the checksum's line.
fn archive(mut files: Vec<(String, Vec<u8>, u32)>, to: &Path) -> Result<(PathBuf, String)> {
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let entries: Vec<codecs::zip::Entry> =
        files.iter().map(|(name, data, mode)| codecs::zip::Entry { name, data, deflate: true, mode: *mode }).collect();
    let bytes = codecs::zip::write(&entries);
    std::fs::write(to, &bytes).with_context(|| to.display().to_string())?;
    let digest = Sha256::digest(&bytes);
    let hex = digest.iter().fold(String::new(), |mut hex, b| {
        use std::fmt::Write as _;
        let _ = write!(hex, "{b:02x}");
        hex
    });
    let name = to.file_name().context("an archive has a name")?.to_string_lossy().into_owned();
    let line = format!("{hex}  {name}");
    let sum = to.with_file_name(format!("{name}.sha256"));
    std::fs::write(&sum, format!("{line}\n")).with_context(|| sum.display().to_string())?;
    Ok((to.to_path_buf(), line))
}

/// Every file under a folder, by its name in the archive (`<within>/...`), with its bytes and its permissions.
fn gather(folder: &Path, within: &str, out: &mut Vec<(String, Vec<u8>, u32)>) -> Result<()> {
    for entry in std::fs::read_dir(folder).with_context(|| folder.display().to_string())? {
        let entry = entry?;
        let (path, name) = (entry.path(), format!("{within}/{}", entry.file_name().to_string_lossy()));
        if path.is_dir() {
            gather(&path, &name, out)?;
        } else {
            let bytes = std::fs::read(&path).with_context(|| path.display().to_string())?;
            out.push((name, bytes, mode(&path)?));
        }
    }
    Ok(())
}

/// A file's permissions, as the archive keeps them.
#[cfg(unix)]
fn mode(file: &Path) -> Result<u32> {
    use std::os::unix::fs::PermissionsExt;
    Ok(std::fs::metadata(file).with_context(|| file.display().to_string())?.permissions().mode() & 0o777)
}

/// A file's permissions, as the archive keeps them: a program's anyone may run, else readable.
#[cfg(not(unix))]
#[expect(clippy::unnecessary_wraps, reason = "as it is on Unix")]
fn mode(file: &Path) -> Result<u32> {
    Ok(if file.extension().is_some_and(|e| e == "exe") { 0o755 } else { 0o644 })
}
