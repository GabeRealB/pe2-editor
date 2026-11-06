//! What an unsandboxed extension fetches onto a machine before it runs, as its manifest declares it (`[[setup]]`): a
//! package each, by its URL and its SHA-256, and what of it goes where in the extension's tree (`take`). Nothing the
//! extension runs does it: the host downloads what is declared, refuses what is not those bytes, and unpacks what is
//! taken - so trusting an extension is trusting exactly these files, its component carrying the list.
//!
//! What is fetched is the machine's, not a project's: kept in a store of the user's (`<data>/setup/`), one folder an
//! entry, laid out as the extension's tree is (`take`'s places), and shared by every project holding the extension.
//! Each place taken is held to its SHA-256 as well (a folder's, of its files: `hash`), after patching: what a fetch
//! made is checked whole before it appears, and an entry's folder is named by what it holds, so two entries holding
//! the same files - one package from two mirrors - share it. A fetch records each file's size and time; what no longer
//! matches is fetched again. A
//! program the extension starts is looked for in its folder, then in what setup put there for this platform, then on
//! the system (`unsandboxed::program`).
//!
//! What a download is, its bytes say: gzip or xz holding a tar, a zip, or anything else a single file. An archive's symbolic
//! and hard links are taken as the files they name. What is taken may then be patched (`patches`): unified diffs the
//! extension ships, each held to its hash, applied in order to the places `take` put things - exactly, or not at all.
//!
//! Trusting the extension is consenting to what it fetches: what it requires is fetched once it runs, what is optional
//! (`lazy`) when it asks for it (`set-up` of the contract) - unless the user turned that off on this computer, which is
//! theirs to choose, kept with what they trust.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::Deserialize;
use sha2::{Digest, Sha256};

pub mod kept;

/// The platform this runs on, as an extension names one: `<os>-<arch>` (`linux-x86_64`).
#[must_use]
pub fn platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// A package an extension fetches: `[[setup]]` of its manifest.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Setup {
    /// What it is, as it is shown.
    pub name: String,
    /// Its license, as an SPDX expression, shown before it is fetched.
    #[serde(default)]
    pub license: Option<String>,
    /// Who holds its rights, and where it comes from, as it is shown beside its license.
    #[serde(default)]
    pub copyright: Option<String>,
    /// The platform it is for (`linux-x86_64`); none, every platform.
    #[serde(default)]
    pub platform: Option<String>,
    /// Where it is downloaded from: HTTPS.
    pub url: String,
    /// Where else it is, tried in order where `url` fails: the same bytes, held to the same hash.
    #[serde(default)]
    pub mirrors: Vec<String>,
    /// The download's SHA-256: other bytes are refused. Empty only where it is to be found (`hashes`).
    #[serde(default)]
    pub sha256: String,
    /// What of it goes where.
    pub take: Take,
    /// Optional: fetched when the extension asks for it, and only where the user lets it. Otherwise it is required,
    /// and fetched once the extension runs.
    #[serde(default)]
    pub lazy: bool,
    /// What the extension changes of what is taken, in order: patches it ships.
    #[serde(default)]
    pub patches: Vec<Patch>,
}

/// A unified diff an extension ships (`file`, in its folder), applied to what an entry takes: its paths, less their first
/// part (`a/`, `b/`), are places in the extension's tree. Held to its hash, as a download is.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Patch {
    pub file: String,
    pub sha256: String,
}

/// What of a download goes where in the extension's tree, each place with the SHA-256 of what it holds (`hash`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(untagged)]
pub enum Take {
    /// A single file: `{ file, sha256 }`.
    File(One),
    /// An archive: each path in it, a file or a folder whole, and where it goes: `"<path>" = { to, sha256 }`.
    Members(BTreeMap<String, Taken>),
}

/// A download that is a single file, where it goes and its SHA-256.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct One {
    pub file: String,
    /// Empty only where it is to be found (`hashes`): nothing is fetched without it.
    #[serde(default)]
    pub sha256: String,
}

/// Where a member of an archive goes, and the SHA-256 of what it holds there, patched.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Taken {
    pub to: String,
    /// Empty only where it is to be found (`hashes`): nothing is fetched without it.
    #[serde(default)]
    pub sha256: String,
}

impl Take {
    /// Each place taken in the extension's tree, with the SHA-256 it holds to.
    #[must_use]
    pub fn places(&self) -> Vec<(&str, &str)> {
        match self {
            Take::File(one) => vec![(one.file.as_str(), one.sha256.as_str())],
            Take::Members(members) => members.values().map(|t| (t.to.as_str(), t.sha256.as_str())).collect(),
        }
    }
}

impl Setup {
    /// Whether it is for the platform this editor runs on.
    #[must_use]
    pub fn here(&self) -> bool {
        self.platform.as_deref().is_none_or(|named| named == platform())
    }

    /// Where it is downloaded from: its URL, then its mirrors.
    pub fn sources(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.url.as_str()).chain(self.mirrors.iter().map(String::as_str))
    }

    /// The host it is downloaded from, as it is shown.
    #[must_use]
    pub fn host(&self) -> &str {
        let rest = self.url.split_once("://").map_or(self.url.as_str(), |(_, rest)| rest);
        rest.split(['/', '?', '#']).next().unwrap_or(rest)
    }

    /// Its folder in the store: what it holds - each place taken and its hash - and not where it came from, so two
    /// entries holding the same files share it, and two taking otherwise from one package keep apart.
    #[must_use]
    pub fn key(&self) -> String {
        let mut places = self.take.places();
        places.sort_unstable();
        let held = places.iter().fold(String::new(), |mut held, (place, sha256)| {
            let _ = writeln!(held, "{place} {}", sha256.to_ascii_lowercase());
            held
        });
        hex(&Sha256::digest(held.as_bytes())[..20])
    }

    /// Where it is kept in a store, fetched.
    #[must_use]
    pub fn place(&self, store: &Path) -> PathBuf {
        store.join(self.key())
    }

    /// Whether it is fetched into a store, and still as it was: every file the fetch recorded the size and time it
    /// had then (`intact`) - looked at again an hour after it last was, or once it was fetched or removed; `verify`
    /// hashes it. Found intact, it is marked used now (`kept::sweep` removes what was not used for long).
    #[must_use]
    pub fn fetched(&self, store: &Path) -> bool {
        let place = self.place(store);
        if !place.is_dir() {
            return false;
        }
        let mut checked = checked();
        let fresh = checked.get(&place).filter(|(_, at)| at.elapsed() < LOOKED_AT_FOR);
        if let Some((intact, _)) = fresh {
            return *intact;
        }
        let found = intact(self, &place);
        if found {
            kept::used(&place);
        }
        checked.insert(place, (found, std::time::Instant::now()));
        found
    }

    /// Whether every SHA-256 it is held to is declared: nothing is fetched without them.
    fn declared(&self) -> Result<()> {
        ensure!(!self.sha256.is_empty(), "{}: it declares no SHA-256 for its download", self.name);
        let unhashed: Vec<&str> =
            self.take.places().into_iter().filter(|(_, sha256)| sha256.is_empty()).map(|(place, _)| place).collect();
        ensure!(unhashed.is_empty(), "{}: it declares no SHA-256 for {}", self.name, unhashed.join(", "));
        Ok(())
    }
}

/// What a fetch recorded in an entry's folder: what it is (`# <name>\t<url>`), then each file's size, time and path.
/// Its own time is when the entry was last used.
const RECORD: &str = ".fetched";

/// How long what was found of a place holds in a process before it is looked at again.
const LOOKED_AT_FOR: std::time::Duration = std::time::Duration::from_hours(1);

/// The places in a store found intact or not in this process, and when: forgotten as an entry is fetched or removed.
fn checked() -> std::sync::MutexGuard<'static, HashMap<PathBuf, (bool, std::time::Instant)>> {
    type Checked = std::sync::Mutex<HashMap<PathBuf, (bool, std::time::Instant)>>;
    static CHECKED: std::sync::OnceLock<Checked> = std::sync::OnceLock::new();
    CHECKED.get_or_init(Default::default).lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A file's size and modification time, as a fetch records them.
fn stamp(file: &Path) -> Option<(u64, u128)> {
    let meta = std::fs::metadata(file).ok()?;
    let time = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
    Some((meta.len(), time))
}

/// Every file in a folder, by its path within it (`/` between parts), sorted.
fn files_in(folder: &Path) -> Result<Vec<String>> {
    let mut files = vec![];
    let mut folders = vec![(folder.to_path_buf(), String::new())];
    while let Some((at, within)) = folders.pop() {
        for entry in std::fs::read_dir(&at).with_context(|| at.display().to_string())? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = if within.is_empty() { name } else { format!("{within}/{name}") };
            if entry.file_type()?.is_dir() {
                folders.push((entry.path(), path));
            } else {
                files.push(path);
            }
        }
    }
    files.sort_unstable();
    Ok(files)
}

/// Record what a fetch made in its folder: what it is, and each file's size and time, against which it is found intact.
fn record(setup: &Setup, place: &Path) -> Result<()> {
    let mut record = format!("# {}\t{}\n", setup.name.replace(['\t', '\n'], " "), setup.url);
    for file in files_in(place)? {
        let (size, time) = stamp(&place.join(&file)).with_context(|| file.clone())?;
        let _ = writeln!(record, "{size} {time} {file}");
    }
    let to = place.join(RECORD);
    std::fs::write(&to, record).with_context(|| to.display().to_string())
}

/// Whether an entry's folder is as its fetch left it: each place it takes there, and each file the fetch recorded of
/// the size and time it had. Cheap - nothing is read - so its hashes are `verify`'s.
fn intact(setup: &Setup, place: &Path) -> bool {
    let Ok(record) = std::fs::read_to_string(place.join(RECORD)) else { return false };
    let taken = setup.take.places().into_iter().all(|(taken, _)| place.join(taken).exists());
    taken
        && record.lines().filter(|line| !line.starts_with('#')).all(|line| {
            let mut parts = line.splitn(3, ' ');
            let (Some(size), Some(time), Some(file)) = (parts.next(), parts.next(), parts.next()) else {
                return false;
            };
            stamp(&place.join(file)).is_some_and(|(s, t)| s.to_string() == size && t.to_string() == time)
        })
}

/// The SHA-256 of what a place holds: a file's, of its bytes; a folder's, of the list of its files - each one's path
/// within it (`/` between parts), a tab, its SHA-256, a line each, sorted by path. Neither permissions nor times count,
/// so a tree hashes alike on every platform.
///
/// # Errors
/// It cannot be read.
pub fn hash(place: &Path) -> Result<String> {
    let of_file = |file: &Path| -> Result<String> {
        let mut hasher = Sha256::new();
        std::io::copy(&mut std::fs::File::open(file).with_context(|| file.display().to_string())?, &mut hasher)?;
        Ok(hex(&hasher.finalize()))
    };
    if !place.is_dir() {
        return of_file(place);
    }
    let mut list = String::new();
    for file in files_in(place)? {
        let _ = writeln!(list, "{file}\t{}", of_file(&place.join(&file))?);
    }
    Ok(hex(&Sha256::digest(list.as_bytes())))
}

/// Each place an entry's folder (or a folder it is being made in) takes, held to its hash; and nothing there besides.
fn held(setup: &Setup, folder: &Path) -> Result<()> {
    let places = setup.take.places();
    for (place, declared) in &places {
        let at: PathBuf = parts(place).with_context(|| format!("{place}: not a place within it"))?.iter().collect();
        let got = hash(&folder.join(&at)).with_context(|| format!("{}: {place}", setup.name))?;
        ensure!(
            got.eq_ignore_ascii_case(declared),
            "{}: {place}: its SHA-256 is {got}, not the {declared} its extension declares",
            setup.name
        );
    }
    let places: Vec<String> = places.iter().filter_map(|(place, _)| parts(place)).map(|p| p.join("/")).collect();
    let within = |file: &str| places.iter().any(|place| file == place || file.starts_with(&format!("{place}/")));
    let besides: Vec<String> = files_in(folder)?.into_iter().filter(|f| f != RECORD && !within(f)).collect();
    ensure!(besides.is_empty(), "{}: it holds what it does not take: {}", setup.name, besides.join(", "));
    Ok(())
}

/// Hash what an entry fetched, every place it takes, as a fetch checks it: what is not as declared, removed, to be
/// fetched again.
///
/// # Errors
/// It is not fetched, or not what it declares (and is removed), or cannot be read.
pub fn verify(setup: &Setup, store: &Path) -> Result<()> {
    let place = setup.place(store);
    ensure!(place.is_dir(), "{}: not fetched", setup.name);
    let found = held(setup, &place);
    checked().remove(&place);
    if found.is_err() {
        remove(setup, store)?;
    }
    found
}

/// Lay out what an entry takes, empty, as though fetched: what tests hold, which download nothing.
#[doc(hidden)]
pub fn as_fetched(setup: &Setup, store: &Path) {
    // One at a time, and once: tests that lay out the same entry at once would each record the other's files.
    static LAYING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one = LAYING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let place = setup.place(store);
    if setup.fetched(store) {
        return;
    }
    for (taken, _) in setup.take.places() {
        let at = place.join(taken);
        if let Some(parent) = at.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(at, "");
    }
    let _ = record(setup, &place);
    checked().remove(&place);
}

/// Downloads are off for this process: what tests run, which never reach the network.
static FORBIDDEN: AtomicBool = AtomicBool::new(false);

/// Turn downloads off for this process: a fetch of what is not there fails, saying so.
pub fn forbid_downloads() {
    FORBIDDEN.store(true, Ordering::Relaxed);
}

/// Remove what was fetched of an entry from a store.
///
/// # Errors
/// It cannot be removed.
pub fn remove(setup: &Setup, store: &Path) -> Result<()> {
    let place = setup.place(store);
    checked().remove(&place);
    match std::fs::remove_dir_all(&place) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e).context(place.display().to_string()),
        _ => Ok(()),
    }
}

/// The entries being fetched in this process, each by its key: a second fetch of one waits for the first.
fn lock(key: &str) -> std::sync::Arc<std::sync::Mutex<()>> {
    static LOCKS: std::sync::OnceLock<std::sync::Mutex<HashMap<String, std::sync::Arc<std::sync::Mutex<()>>>>> =
        std::sync::OnceLock::new();
    let locks = LOCKS.get_or_init(Default::default);
    let mut locks = locks.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    locks.entry(key.to_string()).or_default().clone()
}

/// Bytes as hexadecimal digits.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut hex, b| {
        let _ = write!(hex, "{b:02x}");
        hex
    })
}

/// How a fetch goes: bytes downloaded, of how many where the server said.
pub type Progress<'a> = &'a mut dyn FnMut(u64, Option<u64>);

/// Fetch an entry into a store, where it is not there yet or no longer as it was: downloaded, checked, what it takes
/// unpacked into its folder and patched with what the extension's folder (`folder`) ships, then each place held to its
/// hash - which appears whole or not at all. `cancel` stops it between pieces.
///
/// # Errors
/// It declares no hashes, cannot be downloaded, its bytes are not those declared, it is not what `take` says (an
/// archive or a file), it lacks what is taken, a patch is not what is declared or does not apply, what it made is not
/// what it declares, or the store cannot be written; or it was cancelled.
pub fn fetch(
    setup: &Setup,
    store: &Path,
    folder: Option<&Path>,
    progress: Progress,
    cancel: &AtomicBool,
) -> Result<()> {
    setup.declared()?;
    let key = setup.key();
    let lock = lock(&key);
    let _held = lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let place = setup.place(store);
    if setup.fetched(store) {
        return Ok(());
    }
    ensure!(!FORBIDDEN.load(Ordering::Relaxed), "{}: downloads are off in this run", setup.name);
    // What another process makes at the same time is its own until it is whole.
    let own = std::process::id();
    let partial = store.join(format!("{key}.{own}.part"));
    made(setup, store, &partial, folder, progress, cancel)?;
    let done = held(setup, &partial).and_then(|()| record(setup, &partial));
    if let Err(e) = done {
        let _ = std::fs::remove_dir_all(&partial);
        return Err(e);
    }
    // What was there no longer is as it was: made again.
    checked().remove(&place);
    if place.exists() {
        std::fs::remove_dir_all(&place).with_context(|| place.display().to_string())?;
    }
    if let Err(e) = std::fs::rename(&partial, &place) {
        let _ = std::fs::remove_dir_all(&partial);
        // Another process got there first: the same files.
        ensure!(place.is_dir(), "{}: {e}", place.display());
    }
    Ok(())
}

/// An entry downloaded, unpacked into a folder (`into`) and patched: what it is left there is not checked yet. Its
/// download is held to its hash where it declares one.
fn made(
    setup: &Setup,
    store: &Path,
    into: &Path,
    folder: Option<&Path>,
    progress: Progress,
    cancel: &AtomicBool,
) -> Result<String> {
    let own = std::process::id();
    let downloads = store.join("downloads");
    std::fs::create_dir_all(&downloads).with_context(|| downloads.display().to_string())?;
    let n = DOWNLOADS.fetch_add(1, Ordering::Relaxed);
    let file = downloads.join(format!("{}.{own}.{n}", setup.key()));
    let sha256 = download(setup, &file, progress, cancel)?;
    let _ = std::fs::remove_dir_all(into);
    let unpacked = unpack(setup, &file, into, cancel).and_then(|()| patch_all(setup, folder, into));
    let _ = std::fs::remove_file(&file);
    if let Err(e) = unpacked {
        let _ = std::fs::remove_dir_all(into);
        return Err(e);
    }
    Ok(sha256)
}

/// Downloads under way in this process, numbered so none shares a file.
static DOWNLOADS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// What an entry's hashes are, found by fetching it into a place of its own (a temporary folder in `scratch`) and
/// hashing what it made, as its extension's author declares them: its download's - checked where it declares one - and
/// each place it takes, in `take.places()`'s order. Nothing is kept.
///
/// # Errors
/// It cannot be downloaded, its download is not the bytes it declares, it is not what `take` says, or a patch is not
/// what is declared or does not apply.
pub fn hashes(
    setup: &Setup,
    scratch: &Path,
    folder: Option<&Path>,
    progress: Progress,
    cancel: &AtomicBool,
) -> Result<(String, Vec<String>)> {
    ensure!(!FORBIDDEN.load(Ordering::Relaxed), "{}: downloads are off in this run", setup.name);
    let into = scratch.join(format!("hashing.{}.{}", std::process::id(), DOWNLOADS.load(Ordering::Relaxed)));
    let sha256 = made(setup, scratch, &into, folder, progress, cancel)?;
    let found = setup.take.places().into_iter().map(|(place, _)| {
        let at: PathBuf = parts(place).with_context(|| format!("{place}: not a place within it"))?.iter().collect();
        hash(&into.join(at)).with_context(|| format!("{}: {place}", setup.name))
    });
    let found: Result<Vec<String>> = found.collect();
    let _ = std::fs::remove_dir_all(&into);
    let _ = std::fs::remove_dir(scratch.join("downloads"));
    Ok((sha256, found?))
}

/// A place taken, its hash written in: `"<to>"` becoming `{ to, sha256 }` (or `{ file, sha256 }`), a table's set.
fn hashed(value: &mut toml_edit::Value, key: &str, sha256: &str) {
    if let Some(place) = value.as_str().map(str::to_string) {
        let mut table = toml_edit::InlineTable::new();
        table.insert(key, place.into());
        *value = toml_edit::Value::InlineTable(table);
    }
    if let Some(table) = value.as_inline_table_mut() {
        table.insert("sha256", sha256.into());
    }
}

/// Whether each source of a manifest's `[[setup]]` entries still answers - its URL and its mirrors, every platform's -
/// asked for its first byte alone, as an author checks the pins before users find one gone. Said on stdout: what answers,
/// and what does not, with why. The entries none of whose sources answers.
///
/// # Errors
/// The manifest cannot be read.
pub fn check_pins(manifest: &Path) -> Result<Vec<String>> {
    let text = std::fs::read_to_string(manifest).with_context(|| manifest.display().to_string())?;
    let doc: toml_edit::DocumentMut = text.parse().with_context(|| manifest.display().to_string())?;
    let setups: Setups = toml::from_str(&unhashed(&doc).to_string()).with_context(|| manifest.display().to_string())?;
    let mut gone = Vec::new();
    for setup in &setups.setup {
        let platform = setup.platform.as_deref().unwrap_or("every platform");
        println!("{} ({platform})", setup.name);
        let mut answered = false;
        for url in setup.sources() {
            match answers(url) {
                Ok(()) => {
                    answered = true;
                    println!("  ok    {url}");
                }
                Err(why) => println!("  GONE  {url}: {why}"),
            }
        }
        if !answered {
            gone.push(format!("{} ({platform})", setup.name));
        }
    }
    Ok(gone)
}

/// Whether a source answers with what it holds: its first byte asked for, and nothing more read.
fn answers(url: &str) -> std::result::Result<(), String> {
    if !url.starts_with("https://") {
        return Err("only HTTPS is fetched".to_string());
    }
    match ureq::get(url).set("Range", "bytes=0-0").call() {
        Ok(response) if matches!(response.status(), 200 | 206) => Ok(()),
        Ok(response) => Err(format!("it answers {}", response.status())),
        Err(ureq::Error::Status(code, _)) => Err(format!("it answers {code}")),
        Err(e) => Err(e.to_string()),
    }
}

/// What of a manifest the hashes are found for: its `[[setup]]` entries.
#[derive(Deserialize)]
struct Setups {
    #[serde(default)]
    setup: Vec<Setup>,
}

/// A manifest with each place written bare - a member's `"<path>" = "<to>"`, or `take = "<file>"` - as one hashed by
/// nothing yet, to be found: what `Setup` reads.
fn unhashed(doc: &toml_edit::DocumentMut) -> toml_edit::DocumentMut {
    let mut read = doc.clone();
    let entries = read.get_mut("setup").and_then(toml_edit::Item::as_array_of_tables_mut);
    for entry in entries.into_iter().flat_map(toml_edit::ArrayOfTables::iter_mut) {
        match entry.get_mut("take") {
            Some(toml_edit::Item::Value(value)) if value.is_str() => hashed(value, "file", ""),
            // A single file taken already: `{ file, sha256 }`, its `file` a place, not a member of an archive.
            Some(take) if take.get("file").is_some_and(toml_edit::Item::is_str) => {}
            Some(take) => {
                for (_, member) in take.as_table_like_mut().into_iter().flat_map(|t| t.iter_mut()) {
                    if let Some(value) = member.as_value_mut().filter(|v| v.is_str()) {
                        hashed(value, "to", "");
                    }
                }
            }
            None => {}
        }
    }
    read
}

/// The hashes a manifest's `[[setup]]` entries are held to, for its author (`pe2-cli extension hash`, `cargo xtask
/// hash`): each entry - every platform's - fetched apart and hashed, its download and each place it takes, patched.
/// What differs from what it declares is said, and written into the manifest where asked (`write`), its comments and
/// layout kept. A place may be written bare until then (`"<path>" = "<to>"`, `take = "<file>"`). It goes on stdout,
/// how a download goes on stderr.
///
/// # Errors
/// The manifest cannot be read or written, or an entry cannot be fetched.
pub fn hash_manifest(manifest: &Path, write: bool) -> Result<()> {
    let text = std::fs::read_to_string(manifest).with_context(|| manifest.display().to_string())?;
    let mut doc: toml_edit::DocumentMut = text.parse().with_context(|| manifest.display().to_string())?;
    let read = unhashed(&doc);
    let setups: Setups = toml::from_str(&read.to_string()).with_context(|| manifest.display().to_string())?;
    let folder = manifest.parent().unwrap_or(Path::new("."));
    let scratch = std::env::temp_dir().join(format!("pe2-hash-{}", std::process::id()));
    std::fs::create_dir_all(&scratch)?;
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let mut differs = 0;
    let entries = doc.get_mut("setup").and_then(toml_edit::Item::as_array_of_tables_mut);
    for (setup, entry) in setups.setup.iter().zip(entries.into_iter().flat_map(toml_edit::ArrayOfTables::iter_mut)) {
        println!("{} ({})\n  {}", setup.name, setup.platform.as_deref().unwrap_or("every platform"), setup.url);
        let mut shown = |done: u64, total: Option<u64>| {
            let of = total.map_or_default(|total| format!(" of {}", total >> 20));
            eprint!("\r  {}{of} MB   ", done >> 20);
        };
        let found = hashes(setup, &scratch, Some(folder), &mut shown, &cancel);
        eprint!("\r{}\r", " ".repeat(40));
        let (download, places) = found.with_context(|| setup.name.clone())?;
        let mut said = |what: &str, was: &str, is: &str| {
            if was.eq_ignore_ascii_case(is) {
                println!("  {what}: {is}");
            } else {
                differs += 1;
                let was = if was.is_empty() { "none".to_string() } else { format!("was {was}") };
                println!("  {what}: {is} ({was})");
            }
        };
        said("download", &setup.sha256, &download);
        entry.insert("sha256", toml_edit::value(download.as_str()));
        match &setup.take {
            Take::File(one) => {
                said(&one.file, &one.sha256, &places[0]);
                if let Some(value) = entry.get_mut("take").and_then(toml_edit::Item::as_value_mut) {
                    hashed(value, "file", &places[0]);
                }
            }
            Take::Members(members) => {
                let take = entry.get_mut("take").and_then(toml_edit::Item::as_table_like_mut);
                let take = take.context("take is a table")?;
                for ((from, taken), sha256) in members.iter().zip(&places) {
                    said(&taken.to, &taken.sha256, sha256);
                    if let Some(value) = take.get_mut(from).and_then(toml_edit::Item::as_value_mut) {
                        hashed(value, "to", sha256);
                    }
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    if write && differs > 0 {
        std::fs::write(manifest, doc.to_string()).with_context(|| manifest.display().to_string())?;
        println!("{differs} written into {}", manifest.display());
    } else if differs > 0 {
        println!("{differs} differ from what {} declares: --write writes them in", manifest.display());
    } else {
        println!("Each is as {} declares", manifest.display());
    }
    Ok(())
}

/// Download an entry's package to a file, from its URL or else its mirrors in order, checked against its SHA-256 as it
/// comes where it declares one: its SHA-256. A source that fails, or gives other bytes, gives way to the next; where all
/// do, each says why.
fn download(setup: &Setup, to: &Path, progress: Progress, cancel: &AtomicBool) -> Result<String> {
    let mut failed = Vec::new();
    for url in setup.sources() {
        match download_from(setup, url, to, progress, cancel) {
            Ok(got) => return Ok(got),
            Err(e) if cancel.load(Ordering::Relaxed) => return Err(e),
            Err(e) => failed.push(format!("{e:#}")),
        }
    }
    match failed.as_slice() {
        [one] => bail!("{one}"),
        all => bail!("{}: none of its sources gave it:\n  {}", setup.name, all.join("\n  ")),
    }
}

/// Download from one of an entry's sources, as `download` does.
fn download_from(setup: &Setup, url: &str, to: &Path, progress: Progress, cancel: &AtomicBool) -> Result<String> {
    ensure!(url.starts_with("https://"), "{url}: only HTTPS is fetched");
    let response = ureq::get(url).call().map_err(|e| anyhow!("{url}: {e}"))?;
    let total = response.header("Content-Length").and_then(|n| n.parse().ok());
    let mut reader = response.into_reader();
    let partial = to.with_extension("download");
    let mut out = std::fs::File::create(&partial).with_context(|| partial.display().to_string())?;
    let mut hasher = Sha256::new();
    let mut piece = vec![0; 1 << 16];
    let mut done = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            drop(out);
            let _ = std::fs::remove_file(&partial);
            bail!("cancelled");
        }
        let n = match reader.read(&mut piece) {
            Ok(n) => n,
            Err(e) => {
                drop(out);
                let _ = std::fs::remove_file(&partial);
                bail!("{url}: {e}");
            }
        };
        if n == 0 {
            break;
        }
        hasher.update(&piece[..n]);
        out.write_all(&piece[..n]).with_context(|| partial.display().to_string())?;
        done += n as u64;
        progress(done, total);
    }
    out.flush()?;
    drop(out);
    let got = hex(&hasher.finalize());
    if !setup.sha256.is_empty() && !got.eq_ignore_ascii_case(&setup.sha256) {
        let _ = std::fs::remove_file(&partial);
        bail!("{url}: its SHA-256 is {got}, not the {} its extension declares", setup.sha256);
    }
    std::fs::rename(&partial, to).with_context(|| to.display().to_string())?;
    Ok(got)
}

/// An entry's patches applied to what it took, each checked against its hash first.
fn patch_all(setup: &Setup, folder: Option<&Path>, into: &Path) -> Result<()> {
    for patch in &setup.patches {
        let folder = folder.with_context(|| {
            format!("{}: its patches are in the extension's folder, which it has none of", setup.name)
        })?;
        let file = folder.join(&patch.file);
        let bytes = std::fs::read(&file).with_context(|| file.display().to_string())?;
        let got = hex(&Sha256::digest(&bytes));
        ensure!(
            got.eq_ignore_ascii_case(&patch.sha256),
            "{}: its SHA-256 is {got}, not the {} declared",
            patch.file,
            patch.sha256
        );
        let text = String::from_utf8(bytes).with_context(|| patch.file.clone())?;
        apply(&text, into).with_context(|| format!("{}: {}", setup.name, patch.file))?;
    }
    Ok(())
}

/// A unified diff applied to a tree: each file it names, less the first part of its path, changed hunk by hunk where
/// what it removes and keeps is found - at the line it says, or the nearest where it is. A file it makes from nothing
/// (`--- /dev/null`) is made.
///
/// # Errors
/// It is not a unified diff, names a place outside the tree or a file not there, or a hunk is found nowhere.
pub fn apply(diff: &str, tree: &Path) -> Result<()> {
    let lines: Vec<&str> = diff.lines().collect();
    let mut at = 0;
    let mut files = 0;
    while at < lines.len() {
        let Some(old) = lines[at].strip_prefix("--- ") else {
            at += 1;
            continue;
        };
        let new = lines.get(at + 1).and_then(|line| line.strip_prefix("+++ ")).context("--- without +++")?;
        at += 2;
        let named = |header: &str| header.split('\t').next().unwrap_or(header).trim().to_string();
        let (old, new) = (named(old), named(new));
        let path = new.split_once('/').map(|(_, rest)| rest.to_string()).context("a path with no part to strip")?;
        let place: PathBuf =
            parts(&path).with_context(|| format!("{path}: not a place within the tree"))?.iter().collect();
        let file = tree.join(&place);
        let made = old == "/dev/null";
        let text = if made {
            String::new()
        } else {
            std::fs::read_to_string(&file).with_context(|| format!("{path}: not there to patch"))?
        };
        let ends = text.ends_with('\n') || text.is_empty();
        let mut content: Vec<String> = text.lines().map(str::to_string).collect();
        // Each hunk, moved by what the ones before it added and removed.
        let mut shift: isize = 0;
        while let Some(header) = lines.get(at).and_then(|line| line.strip_prefix("@@ ")) {
            let start: isize = header
                .split_whitespace()
                .next()
                .and_then(|old| old.strip_prefix('-'))
                .and_then(|old| old.split(',').next())
                .and_then(|n| n.parse().ok())
                .with_context(|| format!("{path}: a hunk header that is not one: @@ {header}"))?;
            at += 1;
            let (mut before, mut after) = (Vec::new(), Vec::new());
            while let Some(line) = lines.get(at) {
                match line.chars().next() {
                    Some(' ') => {
                        before.push(line[1..].to_string());
                        after.push(line[1..].to_string());
                    }
                    Some('-') if !line.starts_with("--- ") => before.push(line[1..].to_string()),
                    Some('+') if !line.starts_with("+++ ") => after.push(line[1..].to_string()),
                    Some('\\') => {}
                    None => {
                        before.push(String::new());
                        after.push(String::new());
                    }
                    _ => break,
                }
                at += 1;
            }
            let expected = usize::try_from((start - 1).max(0) + shift).unwrap_or(0);
            let fits =
                |from: usize| content.get(from..from + before.len()).is_some_and(|here| here == before.as_slice());
            let found = (0..=content.len()).filter(|from| fits(*from)).min_by_key(|from| from.abs_diff(expected));
            let from = found.with_context(|| format!("{path}: the hunk at line {start} is found nowhere"))?;
            content.splice(from..from + before.len(), after.iter().cloned());
            shift += isize::try_from(after.len()).unwrap_or(0) - isize::try_from(before.len()).unwrap_or(0);
        }
        let mut out = content.join("\n");
        if ends && !out.is_empty() {
            out.push('\n');
        }
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&file, out).with_context(|| file.display().to_string())?;
        files += 1;
    }
    ensure!(files > 0, "it changes no file: not a unified diff");
    Ok(())
}

/// What a download is, by its first bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    TarGz,
    TarXz,
    Zip,
    File,
}

/// An archive's tar, read through what compresses it.
fn tar_of(file: &Path, kind: Kind) -> Result<tar::Archive<Box<dyn Read>>> {
    let file = std::fs::File::open(file)?;
    let read: Box<dyn Read> = match kind {
        Kind::TarXz => Box::new(lzma_rust2::XzReader::new(std::io::BufReader::new(file), true)),
        _ => Box::new(flate2::read::GzDecoder::new(file)),
    };
    Ok(tar::Archive::new(read))
}

fn kind_of(file: &Path) -> Result<Kind> {
    let mut head = [0u8; 6];
    let read = std::fs::File::open(file)?.read(&mut head)?;
    let head = &head[..read];
    // A gzip or an xz: of a tar, which says `ustar` at 257.
    let compressed = if head.starts_with(&[0x1f, 0x8b]) {
        Some(Kind::TarGz)
    } else if head.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0]) {
        Some(Kind::TarXz)
    } else {
        None
    };
    if let Some(kind) = compressed {
        let mut first = vec![0u8; 512];
        let mut tar = tar_of(file, kind)?.into_inner();
        tar.read_exact(&mut first).ok();
        ensure!(&first[257..262] == b"ustar", "a compressed file other than a tar, which is not read");
        return Ok(kind);
    }
    Ok(if head.starts_with(b"PK\x03\x04") { Kind::Zip } else { Kind::File })
}

/// A path within an archive or a tree, as its parts: none of them leading out of it.
fn parts(path: &str) -> Option<Vec<String>> {
    let parts: Vec<String> = Path::new(path)
        .components()
        .map(|c| match c {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            Component::CurDir => Some(String::new()),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    let parts: Vec<String> = parts.into_iter().filter(|p| !p.is_empty()).collect();
    (!parts.is_empty()).then_some(parts)
}

/// Where an archive's entry goes, where it is taken: below the folder, by the place its member goes to.
fn taken(members: &BTreeMap<String, String>, entry: &[String]) -> Option<(String, PathBuf)> {
    members.iter().find_map(|(from, to)| {
        let from_parts = parts(from)?;
        let rest = entry.strip_prefix(from_parts.as_slice())?;
        let mut place: PathBuf = parts(to)?.iter().collect();
        place.extend(rest);
        Some((from.clone(), place))
    })
}

/// Unpack what an entry takes of its download into a folder.
fn unpack(setup: &Setup, file: &Path, into: &Path, cancel: &AtomicBool) -> Result<()> {
    let kind = kind_of(file)?;
    match (&setup.take, kind) {
        (Take::File(One { file: place, .. }), Kind::File) => {
            let place: PathBuf =
                parts(place).with_context(|| format!("{place}: not a place within it"))?.iter().collect();
            let to = into.join(place);
            write(&to, &mut std::fs::File::open(file)?, Some(0o755))
        }
        (Take::File(_), _) => bail!("{}: an archive, which `take` names as a single file", setup.url),
        (Take::Members(_), Kind::File) => bail!("{}: a single file, which `take` names as an archive", setup.url),
        (Take::Members(members), kind @ (Kind::TarGz | Kind::TarXz)) => {
            unpack_tar(setup, &goes(members), file, kind, into, cancel)
        }
        (Take::Members(members), Kind::Zip) => unpack_zip(setup, &goes(members), file, into),
    }
}

/// Where each member of an archive goes.
fn goes(members: &BTreeMap<String, Taken>) -> BTreeMap<String, String> {
    members.iter().map(|(from, taken)| (from.clone(), taken.to.clone())).collect()
}

/// A file written, its folder made first, with its mode where it says one.
fn write(to: &Path, from: &mut dyn Read, mode: Option<u32>) -> Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).with_context(|| parent.display().to_string())?;
    }
    let mut out = std::fs::File::create(to).with_context(|| to.display().to_string())?;
    std::io::copy(from, &mut out).with_context(|| to.display().to_string())?;
    #[cfg(unix)]
    if let Some(mode) = mode {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(to, std::fs::Permissions::from_mode(mode & 0o7777))?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    Ok(())
}

/// Every member `take` names, found: what was not is an error.
fn all_found(
    setup: &Setup,
    members: &BTreeMap<String, String>,
    found: &std::collections::HashSet<String>,
) -> Result<()> {
    let missing: Vec<&str> = members.keys().filter(|m| !found.contains(*m)).map(String::as_str).collect();
    ensure!(missing.is_empty(), "{}: it has no {}", setup.url, missing.join(", "));
    Ok(())
}

fn unpack_tar(
    setup: &Setup,
    members: &BTreeMap<String, String>,
    file: &Path,
    kind: Kind,
    into: &Path,
    cancel: &AtomicBool,
) -> Result<()> {
    let open = || tar_of(file, kind);
    let mut found = std::collections::HashSet::new();
    // Where each archive path taken went; links taken, by the archive path they name.
    let mut written: HashMap<Vec<String>, PathBuf> = HashMap::new();
    let mut links: Vec<(Vec<String>, PathBuf)> = Vec::new();
    let mut archive = open()?;
    for entry in archive.entries().with_context(|| setup.url.clone())? {
        ensure!(!cancel.load(Ordering::Relaxed), "cancelled");
        let mut entry = entry.with_context(|| setup.url.clone())?;
        let path = entry.path()?.to_string_lossy().into_owned();
        let Some(path_parts) = parts(&path) else { continue };
        let Some((member, place)) = taken(members, &path_parts) else { continue };
        found.insert(member);
        let to = into.join(&place);
        let header = entry.header();
        match header.entry_type() {
            tar::EntryType::Regular | tar::EntryType::Continuous => {
                let mode = header.mode().ok();
                write(&to, &mut entry, mode)?;
                written.insert(path_parts, to);
            }
            tar::EntryType::Directory => std::fs::create_dir_all(&to)?,
            tar::EntryType::Symlink => {
                let target = entry.link_name()?.context("a link naming nothing")?.to_string_lossy().into_owned();
                // Named from the link's own folder.
                let mut named: Vec<String> = path_parts[..path_parts.len() - 1].to_vec();
                for part in Path::new(&target).components() {
                    match part {
                        Component::Normal(part) => named.push(part.to_string_lossy().into_owned()),
                        Component::ParentDir => {
                            named.pop().with_context(|| format!("{path}: a link out of the archive"))?;
                        }
                        Component::CurDir => {}
                        _ => bail!("{path}: a link out of the archive"),
                    }
                }
                links.push((named, to));
            }
            tar::EntryType::Link => {
                let target = entry.link_name()?.context("a link naming nothing")?.to_string_lossy().into_owned();
                links.push((parts(&target).with_context(|| format!("{path}: a link out of the archive"))?, to));
            }
            _ => {}
        }
    }
    all_found(setup, members, &found)?;
    // A link is the file it names: copied where that was taken, read from the archive again where not.
    let mut waiting: HashMap<Vec<String>, Vec<PathBuf>> = HashMap::new();
    for (named, to) in links {
        match written.get(&named) {
            Some(from) => {
                if let Some(parent) = to.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::copy(from, &to).with_context(|| to.display().to_string())?;
            }
            None => waiting.entry(named).or_default().push(to),
        }
    }
    if waiting.is_empty() {
        return Ok(());
    }
    let mut archive = open()?;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_string_lossy().into_owned();
        let Some(named) = parts(&path) else { continue };
        let Some(places) = waiting.remove(&named) else { continue };
        let mode = entry.header().mode().ok();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes)?;
        for to in places {
            write(&to, &mut bytes.as_slice(), mode)?;
        }
    }
    let unresolved: Vec<String> = waiting.keys().map(|named| named.join("/")).collect();
    ensure!(unresolved.is_empty(), "{}: links to what it does not hold: {}", setup.url, unresolved.join(", "));
    Ok(())
}

fn unpack_zip(setup: &Setup, members: &BTreeMap<String, String>, file: &Path, into: &Path) -> Result<()> {
    let bytes = std::fs::read(file)?;
    let archive = codecs::zip::Archive::new(&bytes).map_err(|e| anyhow!("{}: {e}", setup.url))?;
    let mut found = std::collections::HashSet::new();
    for name in archive.names() {
        let Some(path_parts) = parts(name) else { continue };
        let Some((member, place)) = taken(members, &path_parts) else { continue };
        found.insert(member);
        let to = into.join(place);
        if name.ends_with('/') {
            std::fs::create_dir_all(&to)?;
            continue;
        }
        let data = archive.read(name).context(name.to_string())?.map_err(|e| anyhow!("{name}: {e}"))?;
        write(&to, &mut data.as_slice(), archive.mode(name))?;
    }
    all_found(setup, members, &found)
}

#[cfg(test)]
mod tests {
    use super::{One, Setup, Take, Taken, fetch, unpack};
    use std::path::Path;
    use std::sync::atomic::AtomicBool;

    fn setup(take: Take) -> Setup {
        Setup {
            name: "a tool".into(),
            license: None,
            copyright: None,
            platform: None,
            url: "https://example.com/tool.tar.gz".into(),
            sha256: "0".repeat(64),
            take,
            lazy: false,
            patches: vec![],
            mirrors: vec![],
        }
    }

    /// A gzip of a tar: a program, a library and a link to it, and what is not taken.
    fn tarball(dir: &Path) -> std::path::PathBuf {
        let file = dir.join("tool.tar.gz");
        let gz = flate2::write::GzEncoder::new(std::fs::File::create(&file).unwrap(), flate2::Compression::fast());
        let mut tar = tar::Builder::new(gz);
        let mut add = |path: &str, data: &[u8], mode: u32| {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(mode);
            header.set_cksum();
            tar.append_data(&mut header, path, data).unwrap();
        };
        add("pkg/libexec/gcc/x86_64/14/cc1", b"a compiler", 0o755);
        add("pkg/libexec/libz.so.1.3", b"a library", 0o644);
        add("pkg/share/doc", b"left", 0o644);
        let mut link = tar::Header::new_gnu();
        link.set_entry_type(tar::EntryType::Symlink);
        link.set_size(0);
        link.set_mode(0o777);
        link.set_cksum();
        tar.append_link(&mut link, "pkg/libexec/libz.so.1", "libz.so.1.3").unwrap();
        tar.into_inner().unwrap().finish().unwrap();
        file
    }

    /// A single file taken, held to a hash.
    fn one(file: &str, sha256: &str) -> Take {
        Take::File(One { file: file.into(), sha256: sha256.into() })
    }

    /// Members of an archive taken, each to its place, none held to a hash.
    fn members(pairs: &[(&str, &str)]) -> Take {
        Take::Members(
            pairs
                .iter()
                .map(|(from, to)| (from.to_string(), Taken { to: to.to_string(), sha256: String::new() }))
                .collect(),
        )
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("pe2-setup-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn what_is_taken_goes_where_it_says_links_as_their_files() {
        let dir = scratch("take");
        let file = tarball(&dir);
        let taken = members(&[
            ("pkg/libexec/gcc/x86_64/14/cc1", "cpp/libexec/gcc/host/14/cc1"),
            ("pkg/libexec/libz.so.1", "cpp/libexec/libz.so.1"),
        ]);
        let into = dir.join("out");
        unpack(&setup(taken), &file, &into, &AtomicBool::new(false)).unwrap();
        let cc1 = into.join("cpp/libexec/gcc/host/14/cc1");
        assert_eq!(std::fs::read(&cc1).unwrap(), b"a compiler");
        assert_eq!(std::fs::read(into.join("cpp/libexec/libz.so.1")).unwrap(), b"a library");
        assert!(!into.join("share").exists() && !into.join("pkg").exists(), "only what is taken");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&cc1).unwrap().permissions().mode() & 0o777, 0o755);
        }
        // What it does not have, and a file named as an archive.
        let missing = members(&[("pkg/bin/as", "as")]);
        let why = unpack(&setup(missing), &file, &dir.join("no"), &AtomicBool::new(false));
        assert!(format!("{:#}", why.unwrap_err()).contains("it has no pkg/bin/as"));
        let why = unpack(&setup(one("cc1", "")), &file, &dir.join("no"), &AtomicBool::new(false));
        assert!(format!("{:#}", why.unwrap_err()).contains("an archive, which `take` names as a single file"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_xz_tar_is_taken_as_a_gzip_one_its_hard_links_as_their_files() {
        let dir = scratch("xz");
        let mut tar = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(10);
        header.set_mode(0o755);
        header.set_cksum();
        tar.append_data(&mut header, "tools/bin/mipsel-ld.bfd", &b"a linker.."[..]).unwrap();
        // The release links one name of the linker to the other.
        let mut link = tar::Header::new_gnu();
        link.set_entry_type(tar::EntryType::Link);
        link.set_size(0);
        link.set_cksum();
        tar.append_link(&mut link, "tools/bin/mipsel-ld", "tools/bin/mipsel-ld.bfd").unwrap();
        let mut xz = lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::with_preset(1)).unwrap();
        std::io::Write::write_all(&mut xz, &tar.into_inner().unwrap()).unwrap();
        let file = dir.join("tools.tar.xz");
        std::fs::write(&file, xz.finish().unwrap()).unwrap();
        let into = dir.join("out");
        unpack(&setup(members(&[("tools/bin/mipsel-ld", "mips-ld")])), &file, &into, &AtomicBool::new(false)).unwrap();
        assert_eq!(std::fs::read(into.join("mips-ld")).unwrap(), b"a linker..");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn places_written_bare_are_read_as_unhashed_and_those_hashed_as_they_are() {
        let sha = "a".repeat(64);
        let text = format!(
            "[[setup]]\nname = \"bare file\"\nurl = \"https://x\"\ntake = \"cc1\"\n\n\
             [[setup]]\nname = \"hashed file\"\nurl = \"https://x\"\ntake = {{ file = \"cc1.exe\", sha256 = \"{sha}\" }}\n\n\
             [[setup]]\nname = \"members\"\nurl = \"https://x\"\n[setup.take]\n\"a/as\" = \"as\"\n\"a/ld\" = {{ to = \"ld\", sha256 = \"{sha}\" }}\n"
        );
        let read = super::unhashed(&text.parse().unwrap());
        let setups: super::Setups = toml::from_str(&read.to_string()).unwrap();
        let places: Vec<Vec<(&str, &str)>> = setups.setup.iter().map(|s| s.take.places()).collect();
        assert_eq!(places[0], [("cc1", "")]);
        assert_eq!(places[1], [("cc1.exe", sha.as_str())]);
        assert_eq!(places[2], [("as", ""), ("ld", sha.as_str())]);
    }

    #[test]
    fn each_source_is_tried_in_turn_and_each_failure_said() {
        let dir = scratch("sources");
        let mut entry = setup(one("cc1", ""));
        // None reachable: nothing here answers, and no network is reached.
        entry.url = "https://127.0.0.1:1/first".into();
        entry.mirrors = vec!["https://127.0.0.1:1/second".into(), "http://127.0.0.1:1/plain".into()];
        assert_eq!(entry.sources().collect::<Vec<_>>().len(), 3);
        let why = format!(
            "{:#}",
            super::download(&entry, &dir.join("got"), &mut |_, _| {}, &AtomicBool::new(false)).unwrap_err()
        );
        assert!(why.contains("none of its sources gave it"), "{why}");
        let said: Vec<&str> = ["first", "second", "only HTTPS"].into_iter().filter(|s| why.contains(s)).collect();
        assert_eq!(said.len(), 3, "{why}");
        assert!(!dir.join("got").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_zip_is_taken_as_a_tar_is_its_programs_renamed() {
        let dir = scratch("zip");
        let file = dir.join("tools.zip");
        let entries = [
            codecs::zip::Entry { name: "bin/mipsel-as.exe", data: b"an assembler", deflate: true, mode: 0o755 },
            codecs::zip::Entry { name: "bin/libzstd.dll", data: b"a library", deflate: true, mode: 0o644 },
            codecs::zip::Entry { name: "share/doc", data: b"left", deflate: true, mode: 0o644 },
        ];
        std::fs::write(&file, codecs::zip::write(&entries)).unwrap();
        let taken = members(&[("bin/mipsel-as.exe", "mips-as.exe"), ("bin/libzstd.dll", "libzstd.dll")]);
        let into = dir.join("out");
        unpack(&setup(taken), &file, &into, &AtomicBool::new(false)).unwrap();
        assert_eq!(std::fs::read(into.join("mips-as.exe")).unwrap(), b"an assembler");
        assert_eq!(std::fs::read(into.join("libzstd.dll")).unwrap(), b"a library");
        assert!(!into.join("share").exists() && !into.join("bin").exists(), "only what is taken, where it goes");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_patch_changes_what_was_taken_where_it_says_or_nothing() {
        let dir = scratch("patch");
        std::fs::create_dir_all(dir.join("tool/lib")).unwrap();
        std::fs::write(dir.join("tool/lib/a.py"), "one\ntwo\nthree\nfour\nfive\n").unwrap();
        // Its line is off by one: found nearby, as `patch` finds it.
        let diff =
            "--- a/tool/lib/a.py\n+++ b/tool/lib/a.py\n@@ -3,3 +3,4 @@\n two\n three\n+three and a half\n four\n";
        super::apply(diff, &dir).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("tool/lib/a.py")).unwrap(),
            "one\ntwo\nthree\nthree and a half\nfour\nfive\n"
        );
        // What it removes is not there: refused, the file as it was.
        let wrong = "--- a/tool/lib/a.py\n+++ b/tool/lib/a.py\n@@ -1,2 +1,1 @@\n-zero\n one\n";
        let why = super::apply(wrong, &dir).unwrap_err();
        assert!(format!("{why:#}").contains("found nowhere"), "{why:#}");
        let outside = "--- a/../escaped\n+++ b/../escaped\n@@ -0,0 +1,1 @@\n+x\n";
        assert!(super::apply(outside, &dir).is_err());
        assert!(!dir.parent().unwrap().join("escaped").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_entry_is_kept_by_what_it_holds_and_fetched_once() {
        let a = setup(one("cc1", &"a".repeat(64)));
        let b = setup(one("bin/cc1", &"a".repeat(64)));
        assert_ne!(a.key(), b.key(), "one file in two places");
        let mut mirrored = a.clone();
        mirrored.url = "https://mirror.example.com/tool".into();
        mirrored.sha256 = "1".repeat(64);
        assert_eq!(a.key(), mirrored.key(), "the same file from elsewhere");
        assert_ne!(a.key(), setup(one("cc1", &"b".repeat(64))).key(), "another file");
        assert_eq!(a.host(), "example.com");
        // Already there: nothing is downloaded.
        let store = scratch("store");
        super::as_fetched(&a, &store);
        fetch(&a, &store, None, &mut |_, _| panic!("downloaded"), &AtomicBool::new(false)).unwrap();
        // Removed, it is to fetch again: here, where downloads are off, it says so.
        super::remove(&a, &store).unwrap();
        assert!(!a.fetched(&store));
        super::forbid_downloads();
        let why = fetch(&a, &store, None, &mut |_, _| panic!("downloaded"), &AtomicBool::new(false)).unwrap_err();
        assert!(format!("{why:#}").contains("downloads are off"), "{why:#}");
        // What declares no hash is never fetched.
        let unhashed = setup(one("cc1", ""));
        let why = fetch(&unhashed, &store, None, &mut |_, _| {}, &AtomicBool::new(false)).unwrap_err();
        assert!(format!("{why:#}").contains("declares no SHA-256 for cc1"), "{why:#}");
        let _ = std::fs::remove_dir_all(&store);
    }

    #[test]
    fn what_was_fetched_is_held_to_its_hashes_and_found_changed() {
        let dir = scratch("held");
        let made = dir.join("made");
        std::fs::create_dir_all(made.join("python/lib/sub")).unwrap();
        std::fs::write(made.join("python/lib/a.py"), "a").unwrap();
        std::fs::write(made.join("python/lib/sub/b.py"), "b").unwrap();
        std::fs::write(made.join("cc1"), "a compiler").unwrap();
        // A folder's hash is its list of files and theirs.
        let sha = |bytes: &[u8]| super::hex(&<sha2::Sha256 as sha2::Digest>::digest(bytes));
        let list = format!("a.py\t{}\nsub/b.py\t{}\n", sha(b"a"), sha(b"b"));
        let lib = super::hash(&made.join("python/lib")).unwrap();
        assert_eq!(lib, sha(list.as_bytes()));
        assert_eq!(super::hash(&made.join("cc1")).unwrap(), sha(b"a compiler"));
        let mut taken = members(&[("pkg/lib", "python/lib"), ("pkg/cc1", "cc1")]);
        let Take::Members(places) = &mut taken else { unreachable!() };
        places.get_mut("pkg/lib").unwrap().sha256.clone_from(&lib);
        places.get_mut("pkg/cc1").unwrap().sha256 = sha(b"a compiler");
        let entry = setup(taken);
        super::held(&entry, &made).unwrap();
        // A file changed, and one that is nothing taken: each refused by name.
        std::fs::write(made.join("python/lib/a.py"), "changed").unwrap();
        let why = format!("{:#}", super::held(&entry, &made).unwrap_err());
        assert!(why.contains("python/lib: its SHA-256 is"), "{why}");
        std::fs::write(made.join("python/lib/a.py"), "a").unwrap();
        std::fs::write(made.join("stray"), "x").unwrap();
        let why = format!("{:#}", super::held(&entry, &made).unwrap_err());
        assert!(why.contains("what it does not take: stray"), "{why}");
        std::fs::remove_file(made.join("stray")).unwrap();
        // In the store, recorded: intact until a file is removed or changed, which verify hashes.
        let store = dir.join("store");
        std::fs::create_dir_all(&store).unwrap();
        super::record(&entry, &made).unwrap();
        std::fs::rename(&made, entry.place(&store)).unwrap();
        assert!(entry.fetched(&store));
        super::verify(&entry, &store).unwrap();
        let place = entry.place(&store);
        std::fs::remove_file(place.join("python/lib/sub/b.py")).unwrap();
        assert!(!super::intact(&entry, &place), "a file gone");
        assert!(super::verify(&entry, &store).is_err());
        assert!(!place.exists(), "removed, to be fetched again");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
