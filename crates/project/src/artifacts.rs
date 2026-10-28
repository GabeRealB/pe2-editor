//! Artifacts: what the editor derives from an asset - a thumbnail, a decoded view - kept in the project cache
//! (`.cache/artifacts/<uid>/<name>`), so that it is made once rather than each time it is shown.
//!
//! An artifact is kept in sync by content, not time: beside it, `<name>.key` holds the hash of everything it was made
//! from - its generator and that generator's version, its parameters, and its asset's files (their names and
//! hashes). Asked for under another key (an edit changed a file, the generator changed), it is made again. The
//! artifacts of assets the project no longer has are swept when it is opened. Like the rest of the cache, any of it
//! may be deleted at the cost of time only.

use anyhow::{Context, Result};
use std::collections::HashSet;
use std::path::PathBuf;

use crate::cache::{Cache, write_atomic};
use crate::extract::sha1_hex;
use crate::index::Asset;

const DIR: &str = "artifacts";

pub struct Artifacts {
    dir: PathBuf,
}

/// The key of an artifact of an asset: its generator (a name and a version), its parameters, and the asset's files.
#[must_use]
pub fn key(asset: &Asset, generator: &str, parameters: &str) -> String {
    let mut text = format!("{generator}\n{parameters}\n");
    let mut files: Vec<(&str, &str)> = asset.files.iter().map(|f| (f.name.as_str(), f.sha1.as_str())).collect();
    files.sort_unstable();
    for (name, sha1) in files {
        text.push_str(name);
        text.push(' ');
        text.push_str(sha1);
        text.push('\n');
    }
    sha1_hex(text.as_bytes())
}

/// A uid as a folder's name: without its scheme.
fn folder(uid: &str) -> &str {
    uid.strip_prefix("uid://").unwrap_or(uid)
}

impl Artifacts {
    /// The project's artifacts.
    ///
    /// # Errors
    /// Their folder cannot be made.
    pub fn open(cache: &Cache) -> Result<Artifacts> {
        let dir = cache.path(DIR);
        std::fs::create_dir_all(&dir).with_context(|| dir.display().to_string())?;
        Ok(Artifacts { dir })
    }

    fn path(&self, uid: &str, name: &str) -> PathBuf {
        self.dir.join(folder(uid)).join(name)
    }

    /// An asset's artifact, if it was made under this key.
    #[must_use]
    pub fn get(&self, uid: &str, name: &str, key: &str) -> Option<Vec<u8>> {
        let path = self.path(uid, name);
        let made_under = std::fs::read_to_string(path.with_file_name(format!("{name}.key"))).ok()?;
        if made_under.trim() != key {
            return None;
        }
        std::fs::read(path).ok()
    }

    /// Keep an asset's artifact, made under this key.
    ///
    /// # Errors
    /// It cannot be written.
    pub fn put(&self, uid: &str, name: &str, key: &str, bytes: &[u8]) -> Result<()> {
        let dir = self.dir.join(folder(uid));
        let path = dir.join(name);
        std::fs::create_dir_all(&dir).with_context(|| path.display().to_string())?;
        write_atomic(&path, bytes)?;
        write_atomic(&path.with_file_name(format!("{name}.key")), key.as_bytes())
    }

    /// Remove the artifacts of assets the project no longer has (by their uids). Returns how many assets' went.
    #[must_use = "how many were swept"]
    pub fn sweep(&self, live: &HashSet<&str>) -> usize {
        let live: HashSet<&str> = live.iter().map(|uid| folder(uid)).collect();
        let mut swept = 0;
        for entry in std::fs::read_dir(&self.dir).into_iter().flatten().flatten() {
            let name = entry.file_name();
            if !live.contains(name.to_string_lossy().as_ref()) && std::fs::remove_dir_all(entry.path()).is_ok() {
                swept += 1;
            }
        }
        swept
    }
}

#[cfg(test)]
mod tests {
    use super::{Artifacts, key};
    use crate::Project;
    use crate::cache::Cache;
    use crate::index::Index;
    use crate::lock::Lock;

    #[test]
    fn made_again_when_its_asset_changes() {
        let root = std::env::temp_dir().join(format!("pe2-artifacts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".layers/top/a.asset")).unwrap();
        std::fs::write(root.join("project.toml"), "[[layer]]\nname = \"top\"\n").unwrap();
        std::fs::write(root.join(".layers/top/a.asset/asset.toml"), "kind = \"image\"\n").unwrap();
        let project = Project::load(&root).unwrap();
        let lock = Lock::try_acquire(&root, "a test").unwrap();
        let cache = Cache::open(&project, &lock).unwrap();
        let mut index = Index::open(&project, &cache).unwrap();
        let artifacts = Artifacts::open(&cache).unwrap();
        let asset = &index.assets["a"];
        let uid = asset.uid.clone().unwrap();
        let first = key(asset, "thumbnail 1", "48");
        artifacts.put(&uid, "thumbnail.png", &first, b"png").unwrap();
        assert_eq!(artifacts.get(&uid, "thumbnail.png", &first).as_deref(), Some(&b"png"[..]));
        // Its declaration edited: another key, nothing kept under it.
        let file = root.join(".layers/top/a.asset/asset.toml");
        std::fs::write(&file, "kind = \"image\"\nnote = 1\n").unwrap();
        index.refresh(&project, &[file]).unwrap();
        let second = key(&index.assets["a"], "thumbnail 1", "48");
        assert_ne!(first, second);
        assert!(artifacts.get(&uid, "thumbnail.png", &second).is_none());
        // Its asset gone: swept.
        assert_eq!(artifacts.sweep(&std::collections::HashSet::new()), 1);
        drop(lock);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
