//! What was read of what is served, remembered: by path, when it was last read and how much of it a backend that
//! keeps what is read holds on disk. One record by name - the project's folder has one - kept in the project's cache
//! (`.cache/access/`) from one serving to the next.
//!
//! It answers what to let go of where a backend keeps more than it may (`Access::oldest`): what was read longest ago.
//!
//! It does not say what to have ready before it is asked for. That was tried, and measured: filling a file ahead
//! costs nearly what its first read does, so for what one run of a program is given nothing is gained, and for a
//! folder worked in, what was read last time is still kept - while filling in the background slows what is read
//! meanwhile.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// A file read: when last (seconds since the epoch), and how many bytes of it are kept on disk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Read {
    last: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    kept: u64,
}

#[expect(clippy::trivially_copy_pass_by_ref, reason = "as serde calls it")]
fn is_zero(bytes: &u64) -> bool {
    *bytes == 0
}

/// What was read of something served, and where that is remembered.
#[derive(Debug)]
pub struct Access {
    file: Option<PathBuf>,
    read: Mutex<(BTreeMap<String, Read>, bool)>,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs())
}

impl Access {
    /// What was read of a use of a project, by its name: as remembered from the last time, nothing where it is new.
    #[must_use]
    pub fn of(project: &Path, what: &str) -> Access {
        let name = project::extract::sha1_hex(what.as_bytes());
        let file = project.join(project::cache::DIR).join("access").join(format!("{name}.json"));
        let read = std::fs::read(&file).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
        Access { file: Some(file), read: Mutex::new((read, false)) }
    }

    /// A record remembered nowhere: of what is served for no project.
    #[must_use]
    pub fn unkept() -> Access {
        Access { file: None, read: Mutex::default() }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, (BTreeMap<String, Read>, bool)> {
        self.read.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// A file was read, now.
    pub fn read(&self, path: &str) {
        let mut state = self.lock();
        state.0.entry(path.to_string()).or_default().last = now();
        state.1 = true;
    }

    /// A file's content is kept on disk, this much of it: read now, as far as letting go of it goes.
    pub fn kept(&self, path: &str, bytes: u64) {
        let mut state = self.lock();
        let read = state.0.entry(path.to_string()).or_default();
        (read.last, read.kept) = (now(), read.kept.max(bytes));
        state.1 = true;
    }

    /// A file's content is no longer kept on disk.
    pub fn dropped(&self, path: &str) {
        let mut state = self.lock();
        if let Some(read) = state.0.get_mut(path) {
            read.kept = 0;
            state.1 = true;
        }
    }

    /// Nothing is kept on disk any more: what served it before, keeping what was read, has let go of all of it.
    pub fn nothing_kept(&self) {
        let mut state = self.lock();
        let (read, changed) = &mut *state;
        for read in read.values_mut().filter(|read| read.kept > 0) {
            (read.kept, *changed) = (0, true);
        }
    }

    /// How much is kept on disk in all.
    #[must_use]
    pub fn kept_in_all(&self) -> u64 {
        self.lock().0.values().map(|read| read.kept).sum()
    }

    /// What is kept on disk, read longest ago first, each with how much of it is: what to let go of until what is kept
    /// fits a budget.
    #[must_use]
    pub fn oldest(&self) -> Vec<(String, u64)> {
        let state = self.lock();
        let mut kept: Vec<(&String, &Read)> = state.0.iter().filter(|(_, read)| read.kept > 0).collect();
        kept.sort_by_key(|(path, read)| (read.last, *path));
        kept.into_iter().map(|(path, read)| (path.clone(), read.kept)).collect()
    }

    /// Remember it, where it is remembered and something was read since it last was.
    pub fn save(&self) {
        let Some(file) = &self.file else { return };
        let mut state = self.lock();
        if !std::mem::take(&mut state.1) {
            return;
        }
        // What was read long ago and is not kept says nothing any more: a record does not grow without end.
        let old = now().saturating_sub(90 * 24 * 3600);
        state.0.retain(|_, read| read.kept > 0 || read.last >= old);
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(text) = serde_json::to_vec(&state.0) {
            let _ = std::fs::write(file, text);
        }
    }
}

impl Drop for Access {
    fn drop(&mut self) {
        self.save();
    }
}

/// A budget of bytes as a setting writes it: a number with `KiB`, `MiB` or `GiB` (or `K`, `M`, `G`), or `0`.
#[must_use]
pub fn bytes(text: &str) -> Option<u64> {
    let text = text.trim();
    let digits = text.find(|c: char| !c.is_ascii_digit()).unwrap_or(text.len());
    let count: u64 = text[..digits].parse().ok()?;
    let unit = match text[digits..].trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" | "kib" => 1 << 10,
        "m" | "mb" | "mib" => 1 << 20,
        "g" | "gb" | "gib" => 1 << 30,
        _ => return None,
    };
    count.checked_mul(unit)
}

#[cfg(test)]
mod tests {
    use super::{Access, bytes};

    #[test]
    fn what_is_let_go_of_first_is_what_was_read_longest_ago_and_is_kept() {
        let access = Access::unkept();
        access.kept("old.bin", 100);
        access.kept("new.bin", 50);
        access.read("only-read.h");
        // Made older by hand: both were kept this second.
        access.lock().0.get_mut("old.bin").unwrap().last -= 60;
        assert_eq!(access.oldest(), [("old.bin".to_string(), 100), ("new.bin".to_string(), 50)]);
        assert_eq!(access.kept_in_all(), 150);
        access.dropped("old.bin");
        assert_eq!(access.oldest(), [("new.bin".to_string(), 50)]);
        access.nothing_kept();
        assert_eq!((access.oldest(), access.kept_in_all()), (Vec::new(), 0));
    }

    #[test]
    fn a_budget_is_read_as_a_setting_writes_it() {
        assert_eq!(bytes("16 MiB"), Some(16 << 20));
        assert_eq!(bytes("2GiB"), Some(2 << 30));
        assert_eq!(bytes("0"), Some(0));
        assert_eq!(bytes("512 k"), Some(512 << 10));
        assert_eq!(bytes("lots"), None);
    }
}
