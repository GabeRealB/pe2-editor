//! Merges of a project's files as version control asks for them (`pe2-cli project merge %O %A %B %P`): a layer's
//! records merged as what they say - field by field, a file both sides changed by its contents, three ways, as the
//! index merges an override over the version below - `project.toml` and a layer's `layer.toml` by field, any other
//! file of the layers by line.
//!
//! What cannot be merged is a conflict. A record keeps ours, with theirs whole beside it (`[merge] theirs`, which a
//! record's reader keeps: `records::Record::theirs`), for the project to say and the user to settle - keeping ours or
//! taking theirs (`records::settle`); any other file keeps ours where the two disagree. A merge writes nothing but the
//! file merged: one whose result would be a new object in a store - a text too long to be held inline, bytes - is a
//! conflict, as what version control commits of a merge is the files it merged.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};

use crate::records::{Content, File, Record};

/// A merge: the file's text, and whether anything in it conflicts.
#[derive(Debug, PartialEq, Eq)]
pub struct Merged {
    pub text: String,
    pub conflict: bool,
}

/// Merge a file of a project at `path` (as version control names it), `ours` and `theirs` both changed from `base`;
/// `read` reads what a store keeps by hash, for a record's files kept there.
#[must_use]
pub fn merge(
    path: &str,
    base: &str,
    ours: &str,
    theirs: &str,
    read: &dyn Fn(&pe2_ext::Sha1) -> Option<Vec<u8>>,
) -> Merged {
    let name = path.rsplit('/').next().unwrap_or(path);
    if is_record(path) {
        return records(base, ours, theirs, read);
    }
    if std::path::Path::new(name).extension().is_some_and(|e| e.eq_ignore_ascii_case("toml"))
        && let Some((text, conflicts)) = crate::merge::fields::merge(base, theirs, ours)
    {
        return Merged { text, conflict: !conflicts.is_empty() };
    }
    let (text, conflicts) = crate::merge::lines::merge(base, theirs, ours);
    Merged { text, conflict: !conflicts.is_empty() }
}

/// Whether a path is a layer's record: `layers/<layer>/<shard>/<uid>.toml`.
fn is_record(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    parts.len() >= 4
        && parts[parts.len() - 4] == crate::LAYERS.as_str()
        && crate::records::is_shard(std::ffi::OsStr::new(parts[parts.len() - 2]))
        && parts[parts.len() - 1].strip_suffix(".toml").is_some_and(|uid| uid.ends_with(parts[parts.len() - 2]))
}

/// Of three versions of something, the merged one: where one side kept the base, the other's; where both made it the
/// same, that; none where they made it otherwise.
fn pick<T: PartialEq + Clone>(base: &T, ours: &T, theirs: &T) -> Option<T> {
    if ours == theirs || theirs == base {
        Some(ours.clone())
    } else if ours == base {
        Some(theirs.clone())
    } else {
        None
    }
}

/// Merge three versions of a record; a conflict keeps ours, theirs beside it.
fn records(base: &str, ours: &str, theirs: &str, read: &dyn Fn(&pe2_ext::Sha1) -> Option<Vec<u8>>) -> Merged {
    let conflict = || Merged { text: with_theirs(ours, theirs), conflict: true };
    let (Ok(b), Ok(o), Ok(t)) = (Record::parse(base), Record::parse(ours), Record::parse(theirs)) else {
        return conflict();
    };
    match merge_records(&b, &o, &t, read) {
        Some(merged) => Merged { text: merged.render(), conflict: false },
        None => conflict(),
    }
}

/// A record two sides changed, merged: none where they changed the same thing otherwise.
fn merge_records(
    base: &Record,
    ours: &Record,
    theirs: &Record,
    read: &dyn Fn(&pe2_ext::Sha1) -> Option<Vec<u8>>,
) -> Option<Record> {
    let names: BTreeSet<_> = base.files.keys().chain(ours.files.keys()).chain(theirs.files.keys()).cloned().collect();
    let mut files = BTreeMap::new();
    for name in names {
        let (b, o, t) = (base.files.get(&name), ours.files.get(&name), theirs.files.get(&name));
        let merged = match pick(&b, &o, &t) {
            Some(file) => file.cloned(),
            // Both changed it, otherwise: by its contents, where both are a file made of the same base.
            None => Some(merge_file(name.as_str(), b?, o?, t?, read)?),
        };
        if let Some(file) = merged {
            files.insert(name, file);
        }
    }
    Some(Record {
        op: pick(&base.op, &ours.op, &theirs.op)?,
        kind: pick(&base.kind, &ours.kind, &theirs.kind)?,
        parent: pick(&base.parent, &ours.parent, &theirs.parent)?,
        name: pick(&base.name, &ours.name, &theirs.name)?,
        files,
        tree: pick(&base.tree, &ours.tree, &theirs.tree)?,
        local: ours.local,
        theirs: None,
    })
}

/// A file both sides changed of a record, merged three ways by its contents - as the index merges an override over
/// the version below - and kept as a record keeps it, over the same base; none where the contents conflict, where
/// one is not to be read, or where what it becomes would be kept in a store.
fn merge_file(
    name: &str,
    base: &File,
    ours: &File,
    theirs: &File,
    read: &dyn Fn(&pe2_ext::Sha1) -> Option<Vec<u8>>,
) -> Option<File> {
    let base_of = pick(&base.base, &ours.base, &theirs.base)?;
    let bytes = |content: Option<&Content>| -> Option<Vec<u8>> {
        match content? {
            Content::Text(text) => Some(text.clone().into_bytes()),
            Content::Stored { sha1, .. } | Content::Object { sha1, .. } => read(sha1),
            Content::Delta { base, delta, .. } => {
                let from = read(base)?;
                crate::records::apply_with(&from, delta, read).ok()
            }
        }
    };
    let (b, o, t) = (bytes(base.content.as_ref())?, bytes(ours.content.as_ref())?, bytes(theirs.content.as_ref())?);
    let merged = crate::merge::merge(name, &b, &t, &o);
    if !merged.conflicts.is_empty() {
        return None;
    }
    // Kept as a record keeps it, over the same base, with nothing new in a store.
    let content = match base_of.and_then(|from| read(&from).map(|bytes| (from, bytes))) {
        Some((from, base_bytes)) => crate::records::delta_without_store(&merged.bytes, from, &base_bytes, name)?,
        None => crate::records::inline(&merged.bytes)?,
    };
    Some(File { content: Some(content), base: base_of })
}

/// Ours, with theirs whole beside it (`[merge] theirs`).
fn with_theirs(ours: &str, theirs: &str) -> String {
    let mut doc: toml_edit::DocumentMut = ours.parse().unwrap_or_default();
    let mut merge = toml_edit::Table::new();
    merge["theirs"] = toml_edit::value(theirs);
    doc["merge"] = toml_edit::Item::Table(merge);
    doc.to_string()
}

/// Run as version control's merge driver: `base`, `ours` and `theirs` are files, `path` the merged file's in the
/// repository; the merge is written to `ours`. Whether it conflicts.
///
/// # Errors
/// A file cannot be read or written.
pub fn driver(
    fs: &dyn pe2_io::Filesystem,
    base: &sfs::HostPath,
    ours: &sfs::HostPath,
    theirs: &sfs::HostPath,
    path: &str,
    stores: &[crate::chunks::Store],
) -> Result<bool> {
    let text = |file: &sfs::HostPath| fs.read_to_string(file).with_context(|| file.display().to_string());
    let read = |sha1: &pe2_ext::Sha1| stores.iter().find_map(|store| store.get_file(sha1).ok());
    let merged = merge(path, &text(base)?, &text(ours)?, &text(theirs)?, &read);
    fs.write(ours, merged.text.as_bytes()).with_context(|| ours.display().to_string())?;
    Ok(merged.conflict)
}

/// What `.gitattributes` says of a project's files: those merged by its driver.
pub const ATTRIBUTES: &str = "layers/** merge=pe2\nproject.toml merge=pe2\n";

/// Have `.gitattributes` in the project's folder say which files its driver merges (`ATTRIBUTES`), beside what it
/// says of others.
///
/// # Errors
/// It cannot be read or written.
pub fn attributes(fs: &dyn pe2_io::Filesystem, root: &sfs::HostPath) -> Result<()> {
    let file = root.join(pe2_path::Name::constant(".gitattributes"));
    let mut text = fs.read_to_string(&file).unwrap_or_default();
    for line in ATTRIBUTES.lines() {
        if !text.lines().any(|l| l.trim() == line) {
            let separator = if text.is_empty() || text.ends_with('\n') { "" } else { "\n" };
            text = format!("{text}{separator}{line}\n");
        }
    }
    fs.write(&file, text.as_bytes()).with_context(|| file.display().to_string())
}

/// `git` run in a folder, what it says; none where it cannot be run or fails.
fn git(io: &dyn pe2_io::Io, folder: &sfs::HostPath, args: &[&str]) -> Option<String> {
    let program = pe2_io::Program::new("git").args(args.iter().copied()).folder(folder.as_path().to_path_buf());
    let done = io.processes().output(&program).ok()?;
    done.exit.success().then(|| String::from_utf8_lossy(&done.stdout).trim().to_string())
}

/// Whether the project's folder is in a git repository.
#[must_use]
pub fn in_git(io: &dyn pe2_io::Io, root: &sfs::HostPath) -> bool {
    git(io, root, &["rev-parse", "--is-inside-work-tree"]).as_deref() == Some("true")
}

/// Whether the project is kept in a git repository - its `project.toml` not one git leaves out - whose merges of its
/// records are not its driver's.
#[must_use]
pub fn unregistered(project: &crate::Project) -> bool {
    let (io, root) = (&*project.io, &project.root);
    in_git(io, root) && git(io, root, &["check-ignore", "-q", crate::FILE.as_str()]).is_none() && !registered(io, root)
}

/// Whether the git repository the project's folder is in has its driver (`merge.pe2.driver`).
#[must_use]
pub fn registered(io: &dyn pe2_io::Io, root: &sfs::HostPath) -> bool {
    git(io, root, &["config", "--get", "merge.pe2.driver"]).is_some_and(|said| !said.is_empty())
}

/// Have the project's merges made by its driver: `.gitattributes`, and - in a git repository, which keeps it in its own
/// configuration, as `.gitattributes` cannot - the driver, run as `cli` (`pe2-cli`). Whether it was registered (not,
/// outside a repository).
///
/// # Errors
/// `.gitattributes` cannot be written, or git refuses the configuration.
pub fn register(io: &dyn pe2_io::Io, root: &sfs::HostPath, cli: &std::path::Path) -> Result<bool> {
    attributes(io.filesystem(), root)?;
    if !in_git(io, root) {
        return Ok(false);
    }
    let driver = format!("\"{}\" project merge %O %A %B %P", cli.display());
    for (key, value) in
        [("merge.pe2.name", "pe2 records, merged by what they say"), ("merge.pe2.driver", driver.as_str())]
    {
        git(io, root, &["config", key, value]).with_context(|| format!("git config {key}"))?;
    }
    Ok(true)
}

/// The project a file version control names is of: the folder above its `layers/`, or holding its `project.toml`, in
/// the repository's folder `at`.
#[must_use]
pub fn root_of(at: &sfs::HostPath, path: &str) -> Option<sfs::HostPath> {
    let parts: Vec<&str> = path.split('/').collect();
    let end = parts
        .iter()
        .position(|part| *part == crate::LAYERS.as_str())
        .or_else(|| (parts.last() == Some(&crate::FILE.as_str())).then(|| parts.len() - 1))?;
    let within = parts[..end].join("/");
    if within.is_empty() { Some(at.clone()) } else { Some(at.join(&pe2_path::RelPath::new(&within).ok()?)) }
}

#[cfg(test)]
mod tests {
    use super::merge;

    const PATH: &str = "layers/top/gh/00000abcdefgh.toml";

    fn record(fields: &str) -> String {
        format!("format = 1\nop = \"override\"\nkind = \"node\"\n{fields}")
    }

    fn none(_: &pe2_ext::Sha1) -> Option<Vec<u8>> {
        None
    }

    #[test]
    fn records_merge_by_what_they_say_and_a_conflict_keeps_ours_with_theirs_beside_it() {
        let text = |text: &str| format!("\n[files.\"node.toml\"]\ntext = \"\"\"\n{text}\"\"\"\n");
        let base = record(&text("size = 2\nmode = \"a\"\n"));
        // Each side a field of the same file: both kept.
        let ours = record(&text("size = 3\nmode = \"a\"\n"));
        let theirs = record(&text("size = 2\nmode = \"b\"\n"));
        let merged = merge(PATH, &base, &ours, &theirs, &none);
        assert!(!merged.conflict, "{}", merged.text);
        assert!(merged.text.contains("size = 3") && merged.text.contains("mode = \"b\""), "{}", merged.text);
        // A file each side added, and one side renamed the entry: all of it.
        let ours2 = record(&format!("name = \"x\"\n{}", text("size = 2\nmode = \"a\"\n")));
        let theirs2 = format!("{base}\n[files.\"more.txt\"]\ntext = \"more\"\n");
        let merged = merge(PATH, &base, &ours2, &theirs2, &none);
        assert!(
            !merged.conflict && merged.text.contains("name = \"x\"") && merged.text.contains("more"),
            "{}",
            merged.text
        );
        // The same field otherwise: ours, theirs beside it.
        let theirs3 = record(&text("size = 4\nmode = \"a\"\n"));
        let merged = merge(PATH, &base, &ours, &theirs3, &none);
        assert!(merged.conflict);
        let read = crate::records::Record::parse(&merged.text).unwrap();
        assert_eq!(read.theirs.as_deref(), Some(theirs3.as_str()));
        assert!(merged.text.contains("size = 3"));
    }

    #[test]
    fn a_project_file_merges_by_field_and_another_by_line() {
        let merged = merge("project.toml", "a = 1\nb = 2\n", "a = 3\nb = 2\n", "a = 1\nb = 4\n", &none);
        assert_eq!(merged, super::Merged { text: "a = 3\nb = 4\n".into(), conflict: false });
        let merged = merge("layers/x.plan.jsonl", "1\n2\n3\n4\n5\n", "1\nB\n3\n4\n5\n", "1\n2\n3\nD\n5\n", &none);
        assert_eq!(merged, super::Merged { text: "1\nB\n3\nD\n5\n".into(), conflict: false });
        assert!(merge("project.toml", "a = 1\n", "a = 2\n", "a = 3\n", &none).conflict);
    }
}
