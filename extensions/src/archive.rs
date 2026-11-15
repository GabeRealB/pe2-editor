//! Extensions kept as archives: `<id>-<version>.extension.zip`, as `cargo xtask package` writes them - an extension's
//! folder whole inside, its programs' permissions kept. Where extensions are kept, one is read without unpacking it, for
//! who it is; added to a project, it is unpacked, and its folder copied in as any other.
//!
//! Every entry is in the one folder, `<id>.extension/`, and nothing in it names a place outside it: what an archive
//! holds is checked before anything is written.

use sfs::HostPath;
use std::path::{Component, Path};
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result, anyhow, bail, ensure};
use project::ext::component::About;
use project::ext::{EXTENSION_COMPONENT, folder};

/// Whether a file is an extension's archive, by its name.
#[must_use]
pub fn is_archive(path: &std::path::Path) -> bool {
    path.file_name().and_then(|n| n.to_str()).is_some_and(|name| name.ends_with(folder::ARCHIVE_SUFFIX))
        && path.is_file()
}

/// The id an archive is named for: `<id>` of `<id>-<version>.extension.zip`.
#[must_use]
pub fn id_of(path: &std::path::Path) -> Option<&str> {
    let named = path.file_name()?.to_str()?.strip_suffix(folder::ARCHIVE_SUFFIX)?;
    named.rsplit_once('-').map(|(id, _)| id).filter(|id| !id.is_empty())
}

/// The one folder every entry of an archive is in: `<id>.extension`.
fn top(archive: &codecs::zip::Archive) -> Result<String> {
    let mut top: Option<String> = None;
    for name in archive.names() {
        let path = Path::new(name);
        let within = !name.contains('\\') && path.components().all(|c| matches!(c, Component::Normal(_)));
        ensure!(within, "{name}: not a place within the archive");
        let first = path.components().next().map(|c| c.as_os_str().to_string_lossy().into_owned());
        let first = first.with_context(|| format!("{name}: an entry with no name"))?;
        match &top {
            None => top = Some(first),
            Some(known) if *known == first => {}
            Some(known) => bail!("it holds {known} and {first}: an extension's archive holds its folder alone"),
        }
    }
    let top = top.context("it is empty")?;
    ensure!(
        folder::id_named(&top).is_some(),
        "it holds {top}, which is no extension's folder (<id>{})",
        folder::SUFFIX
    );
    Ok(top)
}

/// An archive's directory, from its bytes.
fn open(bytes: &[u8]) -> Result<codecs::zip::Archive<'_>> {
    codecs::zip::Archive::new(bytes).map_err(|e| anyhow!("{e}"))
}

/// Who an archive's extension is, read without unpacking it and checked as its folder would be; and its component.
///
/// # Errors
/// It cannot be read, is no extension's archive, or its manifest is not its component's.
pub fn read(file: &HostPath) -> Result<(About, Vec<u8>)> {
    let bytes = std::fs::read(file).with_context(|| file.display().to_string())?;
    let archive = open(&bytes)?;
    let top = top(&archive)?;
    let entry = |name: &str| {
        let read = archive.read(&format!("{top}/{name}")).with_context(|| format!("{top} has no {name}"))?;
        read.map_err(|e| anyhow!("{e}"))
    };
    let component = entry(EXTENSION_COMPONENT.as_str())?;
    let manifest = String::from_utf8(entry(folder::MANIFEST.as_str())?).context(folder::MANIFEST.to_string())?;
    let about = folder::checked_text(&manifest, &top, &component)?;
    Ok((about, component))
}

/// An archive unpacked, in a place of its own: gone with this.
pub struct Unpacked {
    place: HostPath,
    /// Its extension's folder.
    pub folder: HostPath,
}

impl Drop for Unpacked {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.place);
    }
}

/// Unpack an extension's archive, every file's permissions as it says them: its folder, checked as loading checks it.
///
/// # Errors
/// It cannot be read, is no extension's archive, names a place outside its folder, its manifest is not its
/// component's, or it cannot be written.
pub fn unpack(file: &std::path::Path) -> Result<Unpacked> {
    static UNPACKED: AtomicUsize = AtomicUsize::new(0);
    let bytes = std::fs::read(file).with_context(|| file.display().to_string())?;
    let archive = open(&bytes)?;
    let top = top(&archive)?;
    let n = UNPACKED.fetch_add(1, Ordering::Relaxed);
    let temporary = HostPath::new(&std::env::temp_dir()).context("the system's folder of temporary files")?;
    let place = temporary.join(&pe2_path::Name::new(&format!("pe2-unpacked-{}-{n}", std::process::id()))?);
    let _ = std::fs::remove_dir_all(&place);
    let unpacked = Unpacked { folder: place.join(&pe2_path::Name::new(&top)?), place };
    for name in archive.names() {
        // An entry is a path within the archive, whatever it says: one that is not is refused.
        let within = pe2_path::RelPath::new(name.trim_end_matches('/'))
            .with_context(|| format!("{name}: not a place within the archive"))?;
        let to = unpacked.place.join(&within);
        if name.ends_with('/') {
            std::fs::create_dir_all(&to).with_context(|| to.display().to_string())?;
            continue;
        }
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(&parent).with_context(|| parent.to_string())?;
        }
        let data = archive.read(name).with_context(|| name.to_string())?.map_err(|e| anyhow!("{e}"))?;
        std::fs::write(&to, data).with_context(|| to.display().to_string())?;
        #[cfg(unix)]
        if let Some(mode) = archive.mode(name) {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&to, std::fs::Permissions::from_mode(mode))
                .with_context(|| to.display().to_string())?;
        }
    }
    let component = unpacked.folder.join(&EXTENSION_COMPONENT);
    let component = std::fs::read(&component).with_context(|| format!("{top} has no {EXTENSION_COMPONENT}"))?;
    folder::checked(&unpacked.folder, &component)?;
    Ok(unpacked)
}

#[cfg(test)]
mod tests {
    use super::{id_of, read, unpack};
    use codecs::zip::{Entry, write};
    use sfs::HostPath;
    use sfs::testing::At as _;

    fn archive(dir: &HostPath, name: &str, entries: &[(&str, &[u8], u32)]) -> HostPath {
        let entries: Vec<Entry> =
            entries.iter().map(|(name, data, mode)| Entry { name, data, deflate: true, mode: *mode }).collect();
        let file = dir.at(name);
        std::fs::write(&file, write(&entries)).unwrap();
        file
    }

    #[test]
    fn an_archive_holds_one_folder_and_nothing_outside_it() {
        let dir = sfs::testing::scratch("archive");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let refused = |name: &str, entries: &[(&str, &[u8], u32)]| {
            let file = archive(&dir, name, entries);
            format!("{:#}", unpack(&file).err().expect("refused"))
        };
        let outside = refused("a-0.1.0.extension.zip", &[("a.extension/../../escaped", b"x", 0)]);
        assert!(outside.contains("not a place within the archive"), "{outside}");
        assert!(!dir.at("escaped").exists() && !HostPath::new(&std::env::temp_dir()).unwrap().at("escaped").exists());
        let rooted = refused("a-0.1.0.extension.zip", &[("/etc/escaped", b"x", 0)]);
        assert!(rooted.contains("not a place within the archive"), "{rooted}");
        let two = refused("a-0.1.0.extension.zip", &[("a.extension/x", b"x", 0), ("b.extension/x", b"x", 0)]);
        assert!(two.contains("holds its folder alone"), "{two}");
        let plain = refused("a-0.1.0.extension.zip", &[("a/x", b"x", 0)]);
        assert!(plain.contains("no extension's folder"), "{plain}");
        let empty = refused("a-0.1.0.extension.zip", &[("a.extension/x", b"x", 0)]);
        assert!(empty.contains("has no extension.wasm"), "{empty}");
        assert_eq!(id_of(&dir.at("pe2.psx-0.1.0.extension.zip")), Some("pe2.psx"));
        assert_eq!(id_of(&dir.at("pe2.psx.zip")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_archive_unpacks_as_its_folder_was() {
        let dir = sfs::testing::scratch("archive-folder");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let bytes = crate::components().iter().find(|(p, _)| *p == "pe2-events").unwrap().1;
        let manifest = project::ext::component::manifest_text(bytes).unwrap();
        let file = archive(
            &dir,
            "pe2.events-0.1.0.extension.zip",
            &[
                ("pe2.events.extension/extension.wasm", bytes, 0o644),
                ("pe2.events.extension/extension.toml", manifest.as_bytes(), 0o644),
                ("pe2.events.extension/linux-x86_64/tool", b"#!/bin/sh\n", 0o755),
            ],
        );
        let (about, component) = read(&file).unwrap();
        assert_eq!((about.id.as_str(), component.as_slice()), ("pe2.events", bytes));
        let unpacked = unpack(&file).unwrap();
        assert!(unpacked.folder.ends_with("pe2.events.extension"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let tool = std::fs::metadata(unpacked.folder.at("linux-x86_64/tool")).unwrap();
            assert_eq!(tool.permissions().mode() & 0o777, 0o755);
        }
        let place = unpacked.folder.parent().unwrap().clone();
        drop(unpacked);
        assert!(!place.exists(), "gone with it");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
