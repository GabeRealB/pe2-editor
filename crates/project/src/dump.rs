//! What a project resolves to, written so that two of them compare: every entry by its path, what it is, its uid,
//! the layers making it what it is, and its files by hash; and the problems. Nothing of where the project keeps any of
//! it (no place on disk, no kind of layer) is in it, so that it is the same however the project is stored - what a
//! change of storage is checked by (`pe2-cli project dump`).

use crate::index::{Index, Kind};
use serde::Serialize;

/// What a project resolves to.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Dump {
    /// The layers, bottom to top.
    pub layers: Vec<String>,
    pub entries: Vec<Entry>,
    /// Each problem, as it is said, in order.
    pub problems: Vec<String>,
}

/// An entry of the resolved project.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub kind: &'static str,
    pub uid: String,
    /// The layer creating it, the one whose content wins, those overriding it, and the one that moved it.
    pub home: String,
    pub layer: String,
    pub overrides: Vec<String>,
    pub moved: Option<String>,
    /// Its files - a node's, by name; a file's own, by the empty name - each by its hash and size, and the layer
    /// whose it is.
    pub files: Vec<File>,
}

/// A file of an entry.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct File {
    pub name: String,
    pub sha1: String,
    pub size: u64,
    pub layer: String,
}

/// What `index` resolves to.
///
/// # Errors
/// A plain file's content cannot be read for its hash.
pub fn dump(index: &Index) -> std::io::Result<Dump> {
    let layers = index.layers().into_iter().map(|layer| layer.name.to_string()).collect();
    let mut entries = Vec::new();
    for (path, entry) in index.entries.iter() {
        let files = match entry.kind {
            Kind::Asset => index.assets.get(&crate::index::asset_of(path)).map_or_else(Vec::new, |asset| {
                asset
                    .files
                    .iter()
                    .map(|file| File {
                        name: file.name.to_string(),
                        sha1: file.sha1.to_string(),
                        size: file.size,
                        layer: file.layer.to_string(),
                    })
                    .collect()
            }),
            Kind::File => {
                let sha1 = crate::content::sha1(index.io(), &entry.disk)?;
                let size = crate::content::len(index.io(), &entry.disk)?;
                vec![File { name: String::new(), sha1: sha1.to_string(), size, layer: entry.layer.to_string() }]
            }
            Kind::Folder => Vec::new(),
        };
        entries.push(Entry {
            path: path.to_string(),
            kind: match entry.kind {
                Kind::Asset => "node",
                Kind::Folder => "folder",
                Kind::File => "file",
            },
            uid: entry.uid.to_string(),
            home: entry.home.to_string(),
            layer: entry.layer.to_string(),
            overrides: entry.overrides.iter().map(ToString::to_string).collect(),
            moved: entry.moved.as_ref().map(ToString::to_string),
            files,
        });
    }
    let mut problems: Vec<String> = index.problems.iter().map(crate::index::Problem::describe).collect();
    problems.sort();
    Ok(Dump { layers, entries, problems })
}
