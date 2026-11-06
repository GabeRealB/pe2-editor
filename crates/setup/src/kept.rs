//! What a store keeps, and its sweep. A store is shared by every project on a computer, and no project knows what the
//! others need, so what goes is what was not used for long: an entry's folder is marked used each time a process finds
//! it intact (`Setup::fetched`, an hour apart at most), and one not used for `UNUSED_FOR` goes, unless the project
//! sweeping names it. What a fetch left unfinished - its folder being made (`*.part`), its download - goes a day
//! after, and a folder a fetch did not make whole (no record: an older editor's) too, a day after it was last changed.
//! Nothing goes that was changed or used in the last day, as another process may run a program from it.
//!
//! Everything in a store can be fetched again, and is held to its hashes when it is: a sweep that removes too much
//! costs a download.

use std::collections::HashSet;
use std::hash::BuildHasher;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};

use super::RECORD;

/// How long an entry is kept after it was last used, where the project sweeping does not name it.
pub const UNUSED_FOR: Duration = Duration::from_hours(30 * 24);

/// How long nothing goes after it was changed or used.
const GRACE: Duration = Duration::from_hours(24);

/// How often a store is swept, by any process: the time of `SWEPT`, its stamp, says when it last was.
const EVERY: Duration = Duration::from_mins(30);
const SWEPT: &str = ".swept";

/// What a store holds: an entry's folder, or what a fetch left unfinished.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Kept {
    /// Its name in the store: an entry's key (`Setup::key`).
    pub key: String,
    /// What it is, as its fetch recorded it: the entry's name, and where it came from. None for what a fetch left.
    pub name: Option<(String, String)>,
    /// How many bytes it holds.
    pub bytes: u64,
    /// When it was last used, or, where it was not made whole, last changed.
    pub used: SystemTime,
    /// Whether a fetch made it whole: an entry's folder. Otherwise what a fetch left, or an older editor's folder.
    pub whole: bool,
}

impl Kept {
    /// What it is, as it is said: its entry's name, or what a fetch left.
    #[must_use]
    pub fn said(&self) -> String {
        match &self.name {
            Some((name, _)) => name.clone(),
            None if self.key.starts_with("downloads/") => format!("an unfinished download ({})", self.key),
            None if Path::new(&self.key).extension().is_some_and(|e| e == "part") => {
                format!("an unfinished fetch ({})", self.key)
            }
            None if self.whole => format!("a package fetched by an older editor ({})", self.key),
            None => format!("an older editor's {}", self.key),
        }
    }
}

/// How many bytes, as it is said: in MB to a tenth, in KB below one MB.
#[must_use]
pub fn size(bytes: u64) -> String {
    if bytes < 1 << 20 {
        return format!("{} KB", bytes.div_ceil(1024));
    }
    format!("{}.{} MB", bytes >> 20, ((bytes & 0xF_FFFF) * 10) >> 20)
}

/// How long ago, in hours or days, as it is said ("3 days ago").
#[must_use]
pub fn ago(when: SystemTime) -> String {
    let hours = SystemTime::now().duration_since(when).unwrap_or_default().as_secs() / 3600;
    match hours {
        0 => "within the hour".to_string(),
        1 => "an hour ago".to_string(),
        2..24 => format!("{hours} hours ago"),
        24..48 => "a day ago".to_string(),
        _ => format!("{} days ago", hours / 24),
    }
}

/// Mark an entry's folder used now.
pub(crate) fn used(place: &Path) {
    let record = place.join(RECORD);
    if let Ok(file) = std::fs::File::options().append(true).open(&record) {
        let _ = file.set_modified(SystemTime::now());
    }
}

/// When a file or folder was last changed.
fn changed(path: &Path) -> SystemTime {
    std::fs::symlink_metadata(path).and_then(|meta| meta.modified()).unwrap_or(SystemTime::UNIX_EPOCH)
}

/// How many bytes a folder holds, every file in it.
fn bytes_in(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return 0 };
    if !meta.is_dir() {
        return meta.len();
    }
    std::fs::read_dir(path).into_iter().flatten().flatten().map(|entry| bytes_in(&entry.path())).sum()
}

/// One thing in a store, as it is.
fn read(path: &Path, key: String) -> Kept {
    let record = path.join(RECORD);
    let Ok(text) = std::fs::read_to_string(&record) else {
        return Kept { key, name: None, bytes: bytes_in(path), used: changed(path), whole: false };
    };
    let name = text.lines().next().and_then(|head| head.strip_prefix("# ")).and_then(|head| head.split_once('\t'));
    let name = name.map(|(name, url)| (name.to_string(), url.to_string()));
    // The record says each file's size: nothing else is read.
    let files = text.lines().filter(|line| !line.starts_with('#'));
    let bytes = files.filter_map(|line| line.split(' ').next()?.parse::<u64>().ok()).sum();
    Kept { key, name, bytes, used: changed(&record), whole: true }
}

/// What a store holds, most recently used first: each entry's folder, and what fetches left unfinished.
#[must_use]
pub fn list(store: &Path) -> Vec<Kept> {
    let mut kept = vec![];
    for entry in std::fs::read_dir(store).into_iter().flatten().flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "downloads" {
            for download in std::fs::read_dir(&path).into_iter().flatten().flatten() {
                let file = download.path();
                let name = format!("downloads/{}", download.file_name().to_string_lossy());
                kept.push(Kept { key: name, name: None, bytes: bytes_in(&file), used: changed(&file), whole: false });
            }
        } else if path.is_dir() {
            kept.push(read(&path, name));
        }
    }
    kept.sort_by(|a, b| b.used.cmp(&a.used).then_with(|| a.key.cmp(&b.key)));
    kept
}

/// Remove one thing a store holds, by its key (`Kept::key`).
///
/// # Errors
/// It names nothing a store holds, or cannot be removed.
pub fn remove(store: &Path, key: &str) -> Result<()> {
    let within = !key.is_empty() && Path::new(key).components().all(|c| matches!(c, std::path::Component::Normal(_)));
    anyhow::ensure!(within && key != SWEPT, "{key}: nothing a store holds");
    let path = store.join(key);
    super::checked().remove(&path);
    let removed = if path.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
    removed.with_context(|| path.display().to_string())
}

/// Whether a thing a store holds is to go: what is not whole, a day after it last changed; an entry's folder the
/// project does not name (`keep`), `UNUSED_FOR` after it was last used.
fn due<S: BuildHasher>(kept: &Kept, keep: &HashSet<String, S>, now: SystemTime) -> bool {
    let age = now.duration_since(kept.used).unwrap_or_default();
    if age < GRACE {
        return false;
    }
    !kept.whole || (!keep.contains(&kept.key) && age >= UNUSED_FOR)
}

/// What a sweep removed, and what it could not, with why.
pub type Swept = (Vec<Kept>, Vec<(Kept, String)>);

/// Sweep a store now: what is to go, as `due` says, removed - keeping every entry the project sweeping names
/// (`keep`, by key). What was removed, and what could not be, with why.
#[must_use]
pub fn sweep<S: BuildHasher>(store: &Path, keep: &HashSet<String, S>) -> Swept {
    let now = SystemTime::now();
    let (mut removed, mut failed) = (vec![], vec![]);
    for kept in list(store).into_iter().filter(|kept| due(kept, keep, now)) {
        match remove(store, &kept.key) {
            Ok(()) => removed.push(kept),
            Err(e) => failed.push((kept, format!("{e:#}"))),
        }
    }
    (removed, failed)
}

/// Sweep a store where a sweep is due - none for `EVERY`, by any process - as `sweep` does: what was removed and what
/// could not be, or None where it was not due.
#[must_use]
pub fn sweep_if_due<S: BuildHasher>(store: &Path, keep: &HashSet<String, S>) -> Option<Swept> {
    let stamp: PathBuf = store.join(SWEPT);
    let last = std::fs::metadata(&stamp).and_then(|meta| meta.modified()).ok();
    if last.is_some_and(|last| SystemTime::now().duration_since(last).unwrap_or_default() < EVERY) || !store.is_dir() {
        return None;
    }
    // Stamped first: another process finds it swept.
    std::fs::write(&stamp, "").ok()?;
    Some(sweep(store, keep))
}

#[cfg(test)]
mod tests {
    use super::{GRACE, UNUSED_FOR, list, sweep, sweep_if_due};
    use crate::{One, Setup, Take};
    use std::collections::HashSet;
    use std::time::SystemTime;

    fn entry(name: &str) -> Setup {
        Setup {
            name: name.into(),
            license: None,
            copyright: None,
            platform: None,
            url: format!("https://example.com/{name}"),
            sha256: "0".repeat(64),
            take: Take::File(One { file: name.into(), sha256: format!("{:0>64}", name.len()) }),
            lazy: false,
            patches: vec![],
            mirrors: vec![],
        }
    }

    /// Set when a thing in a store was last used or changed: `ago` before now.
    fn aged(path: &std::path::Path, ago: std::time::Duration) {
        let file = std::fs::File::options().append(true).open(path).or_else(|_| std::fs::File::open(path)).unwrap();
        file.set_modified(SystemTime::now() - ago).unwrap();
    }

    #[test]
    fn what_was_not_used_for_long_goes_unless_the_project_names_it() {
        let store = std::env::temp_dir().join(format!("pe2-kept-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&store);
        std::fs::create_dir_all(&store).unwrap();
        let (fresh, old, named) = (entry("fresh"), entry("old-one"), entry("named-one"));
        for setup in [&fresh, &old, &named] {
            crate::as_fetched(setup, &store);
            assert!(setup.fetched(&store), "marked used as it is found");
        }
        let long = UNUSED_FOR + GRACE;
        aged(&old.place(&store).join(".fetched"), long);
        aged(&named.place(&store).join(".fetched"), long);
        // What fetches left: a folder being made and a download, a day old and just now; an older editor's folder.
        for (name, ago) in
            [("x.1.part", GRACE * 2), ("y.2.part", GRACE / 2), ("f6f6e883942d4d32-9872f330edb1", GRACE * 2)]
        {
            std::fs::create_dir_all(store.join(name)).unwrap();
            std::fs::write(store.join(name).join("cc1"), "x").unwrap();
            aged(&store.join(name), ago);
        }
        std::fs::create_dir_all(store.join("downloads")).unwrap();
        std::fs::write(store.join("downloads/abc.1.0"), "partial").unwrap();
        aged(&store.join("downloads/abc.1.0"), GRACE * 2);
        let listed = list(&store);
        assert_eq!(listed.len(), 7);
        let fresh_kept = listed.iter().find(|k| k.key == fresh.key()).unwrap();
        assert_eq!(fresh_kept.name.as_ref().map(|(n, _)| n.as_str()), Some("fresh"));
        assert!(fresh_kept.whole && fresh_kept.bytes == 0);
        let keep: HashSet<String> = [named.key()].into_iter().collect();
        let (removed, failed) = sweep(&store, &keep);
        assert!(failed.is_empty(), "{failed:?}");
        let mut gone: Vec<String> = removed.into_iter().map(|k| k.key).collect();
        gone.sort();
        let mut expected =
            vec![old.key(), "x.1.part".into(), "f6f6e883942d4d32-9872f330edb1".into(), "downloads/abc.1.0".into()];
        expected.sort();
        assert_eq!(gone, expected);
        assert!(fresh.fetched(&store) && named.fetched(&store) && store.join("y.2.part").exists());
        assert!(!old.fetched(&store));
        // Swept: not again for a while, whatever process asks.
        assert!(sweep_if_due(&store, &keep).is_some());
        assert!(sweep_if_due(&store, &keep).is_none());
        let _ = std::fs::remove_dir_all(&store);
    }
}
