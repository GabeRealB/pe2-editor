//! An extension as it is kept, shipped and held by a project: a folder named by its id, `<id>.extension/`.
//!
//! - `extension.toml`, its manifest: who it is, how it depends on others, whether it runs unsandboxed, and what is said
//!   of it (`pe2_ext::extension!`). Its component carries the same, and the two have to agree: what reads the folder
//!   alone - whether it needs trusting, what to show of it - may rely on the file.
//! - `extension.wasm`, its component.
//! - What it ships, by platform (`linux-x86_64/`, `unsandboxed::program`).
//! - What is only read: `docs/` (its guides, `install` among them), `LICENSE`, `licenses/` (those of what it bundles),
//!   `CHANGELOG.md`, its icon, and `tests/`. None of it counts toward trusting it, so a fixed typo is not another
//!   extension.
//!
//! A project holds extensions in its layers, sealed: each one object, a tree in the project's store, named by a record
//! of the layer's (`records::EXTENSIONS`), a higher layer's in place of a lower one's. What is in one is named by its
//! path within it, read where its folder was (`records::held_folder`), through `content`: nothing is laid out, and a
//! program is handed a path through the filesystem the project is served as (`content::for_program`). It loads those
//! and the built-ins, nothing else. Folders elsewhere - yours, those beside the editor - are where it takes them from,
//! kept as a tree when added - or from an archive of a folder (`<id>-<version>.extension.zip`), unpacked.

use std::path::Path;

use anyhow::{Context, Result, bail};

use super::component::{self, About};
use super::unsandboxed::TESTS;

/// What an extension's folder's name ends in.
pub const SUFFIX: &str = ".extension";

/// What an archive of an extension's folder is named with: `<id>-<version>.extension.zip`, its folder whole inside.
pub const ARCHIVE_SUFFIX: &str = ".extension.zip";

/// The folder of a project's layer its extensions' files are read at (`records::held_folder`): where they were folders
/// before they were records (`records::hold_extensions`).
pub const HELD: &str = "extensions";

/// An extension's manifest, in its folder.
pub const MANIFEST: &str = "extension.toml";

/// What of an extension's folder is only read, beside its icon: none of it counts toward trusting it.
const READ_ONLY: &[&str] = &[MANIFEST, "docs", "licenses", "CHANGELOG.md", TESTS];

/// Whether a path within an extension's folder is only read.
#[must_use]
pub fn read_only(name: &Path, about: &About) -> bool {
    let top = name.components().next().map_or_default(|c| c.as_os_str().to_string_lossy().into_owned());
    READ_ONLY.contains(&top.as_str())
        || top.starts_with("LICENSE")
        || about.described.icon.as_deref().is_some_and(|icon| name == Path::new(icon))
}

/// The id a folder is named for: its name without `.extension`.
#[must_use]
pub fn id_of(folder: &Path) -> Option<&str> {
    folder.file_name()?.to_str()?.strip_suffix(SUFFIX).filter(|id| !id.is_empty())
}

/// What a folder's manifest says, read alone.
///
/// # Errors
/// It has none, or it cannot be read.
pub fn manifest(folder: &Path) -> Result<About> {
    let file = folder.join(MANIFEST);
    let text = crate::content::read_to_string(&file).with_context(|| format!("it has no {MANIFEST}"))?;
    toml::from_str(&text).with_context(|| file.display().to_string())
}

/// What a folder holds, checked: its manifest, which is its component's (`bytes`), in a folder named for its id.
///
/// # Errors
/// It has no manifest, its component carries another or none, or its folder is named for another id.
pub fn checked(folder: &Path, bytes: &[u8]) -> Result<About> {
    let text =
        crate::content::read_to_string(&folder.join(MANIFEST)).with_context(|| format!("it has no {MANIFEST}"))?;
    let name = folder.file_name().map_or_default(|n| n.to_string_lossy().into_owned());
    checked_text(&text, &name, bytes)
}

/// An extension's manifest (`text`) checked: its component's (`bytes`), in a folder named (`name`) for its id - in its
/// folder, or in an archive of it.
///
/// # Errors
/// Its component carries another manifest or none, or its folder is named for another id.
pub fn checked_text(text: &str, name: &str, bytes: &[u8]) -> Result<About> {
    let about: About = toml::from_str(text).context(MANIFEST)?;
    component::sandboxed_fetches_nothing(&about)?;
    let held: toml::Table = text.parse().with_context(|| MANIFEST.to_string())?;
    let carried = component::manifest_text(bytes)
        .context("its component carries no manifest: build it with this editor's SDK (`pe2_ext::extension!`)")?;
    let carried: toml::Table = carried.parse().context("its component's manifest")?;
    if held != carried {
        let keys = held.keys().chain(carried.keys());
        let differs = keys.into_iter().find(|key| held.get(*key) != carried.get(*key)).cloned().unwrap_or_default();
        bail!("its {MANIFEST} is not its component's: {differs} differs");
    }
    if id_of(Path::new(name)) != Some(about.id.as_str()) {
        bail!("it is {}, in {name}: its folder is {}{SUFFIX}", about.id, about.id);
    }
    Ok(about)
}

#[cfg(test)]
mod tests {
    use super::{About, checked_text, id_of, read_only};
    use std::path::Path;

    #[test]
    fn what_is_only_read_is_named_by_its_place() {
        let about: About = toml::from_str("id = \"a\"\nversion = \"0.1.0\"\nicon = \"art/icon.png\"\n").unwrap();
        for read in ["extension.toml", "docs/install.md", "licenses/gcc.txt", "LICENSE", "LICENSE-MIT", "art/icon.png"]
        {
            assert!(read_only(Path::new(read), &about), "{read}");
        }
        for run in ["extension.wasm", "linux-x86_64/cc1", "art/other.png", "docs.wasm"] {
            assert!(!read_only(Path::new(run), &about), "{run}");
        }
    }

    #[test]
    fn only_what_runs_unsandboxed_fetches() {
        let setup = "\n[[setup]]\nname = \"a tool\"\nurl = \"https://example.com/t\"\nsha256 = \"00\"\ntake = { file = \"tool\", sha256 = \"00\" }\n";
        let sandboxed = format!("id = \"a\"\nversion = \"0.1.0\"\n{setup}");
        let why = checked_text(&sandboxed, "a.extension", b"").unwrap_err();
        assert!(format!("{why:#}").contains("only an unsandboxed extension fetches"), "{why:#}");
        // Unsandboxed, it gets as far as its component.
        let unsandboxed = format!("id = \"a\"\nversion = \"0.1.0\"\nunsandboxed = true\n{setup}");
        let why = checked_text(&unsandboxed, "a.extension", b"").unwrap_err();
        assert!(format!("{why:#}").contains("carries no manifest"), "{why:#}");
    }

    #[test]
    fn a_folder_is_named_for_its_id() {
        assert_eq!(id_of(Path::new("x/pe2.psx.extension")), Some("pe2.psx"));
        assert_eq!(id_of(Path::new("x/pe2.psx")), None);
        assert_eq!(id_of(Path::new(".extension")), None);
    }
}
