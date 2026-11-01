//! Applied settings: what the work graph makes a node's outputs from. An edit to a node's declaration is written, and
//! undoable, as any; but the graph keeps making the node's outputs from the declaration as it was last applied until
//! it is applied (`apply`), so that a setting changed in passing does not remake everything made of the node at once.
//! What is unapplied is kept in the project cache (`applied.toml`): for each node with unapplied changes, by uid, its
//! declaration as last applied - the declaration before its first unapplied change. A node without an entry is
//! applied as written; a change undone (the declaration as applied again) leaves none.
//!
//! Changes are noted as the project takes them in, whoever made them - the editor, a text editor, a version control
//! tool - while it is open; what changed while it was not is applied as it is found. Like the rest of the cache, it
//! may be deleted: every node is then applied as written.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};

use crate::cache::{Cache, write_atomic};
use crate::index::{AppliedDeclaration, Index};

const FILE: &str = "applied.toml";

/// The nodes with unapplied changes, each with its declaration as last applied.
pub struct Applied {
    path: PathBuf,
    held: Mutex<BTreeMap<String, String>>,
}

impl Applied {
    /// The project's: as the cache keeps them.
    ///
    /// # Errors
    /// What is kept cannot be read.
    pub fn open(cache: &Cache) -> Result<Applied> {
        let path = cache.path(FILE);
        let held = match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).with_context(|| path.display().to_string())?,
            Err(_) => BTreeMap::new(),
        };
        Ok(Applied { path, held: Mutex::new(held) })
    }

    fn held(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, String>> {
        self.held.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn save(&self, held: &BTreeMap<String, String>) {
        if let Ok(text) = toml::to_string(held) {
            let _ = write_atomic(&self.path, text.as_bytes());
        }
    }

    /// A node's declaration as last applied, where it has unapplied changes.
    #[must_use]
    pub fn text(&self, uid: &str) -> Option<String> {
        self.held().get(uid).cloned()
    }

    /// The nodes with unapplied changes, by uid.
    #[must_use]
    pub fn pending(&self) -> Vec<String> {
        self.held().keys().cloned().collect()
    }

    /// Note a node's declaration changed from `before` to `now`: its first unapplied change keeps `before` as what is
    /// applied; one bringing it back to what is applied leaves it applied. Whether anything is unapplied changed.
    pub fn changed(&self, uid: &str, before: &str, now: &str) -> bool {
        if before == now {
            return false;
        }
        let mut held = self.held();
        let noted = match held.get(uid) {
            Some(applied) if applied == now => held.remove(uid).is_some(),
            Some(_) => false,
            None => held.insert(uid.to_string(), before.to_string()).is_none(),
        };
        if noted {
            self.save(&held);
        }
        noted
    }

    /// Forget a node: it is gone.
    pub fn forget(&self, uid: &str) {
        let mut held = self.held();
        if held.remove(uid).is_some() {
            self.save(&held);
        }
    }

    /// Apply a node's changes: its outputs are made from its declaration as written. Whether it had any.
    pub fn apply(&self, uid: &str) -> bool {
        let mut held = self.held();
        let applied = held.remove(uid).is_some();
        if applied {
            self.save(&held);
        }
        applied
    }

    /// Apply every node's changes: the nodes that had any.
    pub fn apply_all(&self) -> Vec<String> {
        let mut held = self.held();
        let all: Vec<String> = std::mem::take(&mut *held).into_keys().collect();
        self.save(&held);
        all
    }

    /// Give the index's assets their declarations as applied - of these paths, or every asset: the work graph reads
    /// them as their nodes' (`index::Asset::applied`).
    pub fn mark(&self, index: &mut Index, paths: Option<&std::collections::BTreeSet<String>>) {
        let held = self.held();
        let targets: Vec<String> = match paths {
            Some(paths) => paths.iter().cloned().collect(),
            None => index.assets.keys().cloned().collect(),
        };
        for path in targets {
            let Some(asset) = index.assets.get_mut(&path) else { continue };
            let applied = asset.uid.as_ref().and_then(|uid| held.get(uid));
            let now = asset.applied.as_ref().map(|a| a.text.as_str());
            if applied.map(String::as_str) != now {
                let mut written = (**asset).clone();
                written.applied = None;
                let written = Arc::new(written);
                let declaration = applied.map(|text| {
                    Arc::new(AppliedDeclaration { written: written.clone(), fields: crate::graph::with_settings(toml::from_str(text).unwrap_or_default()), text: text.clone() })
                });
                Arc::make_mut(asset).applied = declaration;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Applied;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    fn applied(dir: &std::path::Path) -> Applied {
        Applied { path: dir.join("applied.toml"), held: Mutex::new(BTreeMap::new()) }
    }

    #[test]
    fn the_first_change_keeps_what_is_applied_and_undoing_it_applies() {
        let dir = std::env::temp_dir().join(format!("pe2-applied-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = applied(&dir);
        assert!(a.changed("uid://x", "mode = 4", "mode = 8"));
        // A second change keeps the first's before as what is applied.
        assert!(!a.changed("uid://x", "mode = 8", "mode = 16"));
        assert_eq!(a.text("uid://x").as_deref(), Some("mode = 4"));
        // Back to what is applied: nothing is unapplied.
        assert!(a.changed("uid://x", "mode = 16", "mode = 4"));
        assert_eq!(a.pending(), Vec::<String>::new());
        // Kept in the cache: opened again, it is as left.
        assert!(a.changed("uid://y", "a = 1", "a = 2"));
        let again: BTreeMap<String, String> = toml::from_str(&std::fs::read_to_string(dir.join("applied.toml")).unwrap()).unwrap();
        assert_eq!(again.get("uid://y").map(String::as_str), Some("a = 1"));
        assert!(a.apply("uid://y") && !a.apply("uid://y"));
        assert!(a.changed("uid://z", "b = 1", "b = 2") && a.apply_all() == ["uid://z"]);
        assert_eq!(a.pending(), Vec::<String>::new());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
