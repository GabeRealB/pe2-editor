//! The server: the process serving a project as a filesystem (`pe2-cli project server`), and nothing else. It reads no
//! index, no records, and loads no extension: whoever holds the project - an editor, a host (`host`) - lists what is
//! served (`Index::served`) into a table both map into memory (`served`), each path with where its content lives,
//! and names it here; the server serves each file from where the table says it is. What programs write through what it serves it sends to whoever holds the project, on the
//! holder's link, as edits; where nothing holds it, a host is started to.
//!
//! It serves from the first table it is named - until then its folder is as it was - and that connection
//! feeds it until it ends: one feeder at a time, as one process holds the project at a time. A connection asking to
//! keep it (`Keep`) is a lease while it stays open: the server stops a while after none is left, once no file is open
//! in what it serves - what only looks at the folder keeps nothing.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::role::{Link, Role};

/// The server, as a process of a project.
pub static ROLE: Role =
    Role { command: "server", socket: "server.sock", pipe: "pe2-server", lock: "server.lock", log: "server.log" };

/// What is served of the project, by each path in the served folder (`Index::served`): what the holder sends the
/// server.
pub type Listing = std::collections::BTreeMap<String, Shown>;

/// A path of what is served.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "is", rename_all = "snake_case")]
pub enum Shown {
    Folder,
    /// A node: a folder of its files, and the entries inside it.
    Node,
    /// A file: where a layer holds it, its length and when it changed (seconds and nanoseconds since the epoch);
    /// whether it is a node's file (the folders its name makes are the node's, not entries).
    File {
        disk: std::path::PathBuf,
        size: u64,
        modified: (u64, u32),
        of_files: bool,
        /// Where its content lives, for the server to read it knowing nothing else of the project.
        #[serde(default)]
        source: Source,
    },
}

/// Where a served file's content lives.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "in", rename_all = "snake_case")]
pub enum Source {
    /// Where a layer holds it, as a file or the manifest of one (chunked, or in a packed layer's listing).
    #[default]
    Disk,
    /// Text a record holds: the record's file, and the file's name in it.
    RecordText { record: std::path::PathBuf, name: String },
    /// Kept in a store for a record, by its SHA-1.
    Stored { sha1: String, store: std::path::PathBuf },
}

/// What a process says to the server.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "say", rename_all = "snake_case")]
pub enum ToServer {
    /// Keep serving while this connection lasts.
    Keep { who: String },
    /// What is served, from whoever holds the project: the table of this generation, which it wrote (`served`) for
    /// the server to map. The first is what it starts serving with; who names one feeds it from then on, until its
    /// connection ends.
    Table { generation: u64 },
    /// These files changed in the layers - an edit made by whoever holds the project - for what is read of them to
    /// follow at once.
    Changed { paths: Vec<std::path::PathBuf> },
    /// What it is doing.
    Status,
    /// Stop serving, now.
    Stop,
}

/// What the server answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "say", rename_all = "snake_case")]
pub enum FromServer {
    /// Kept, for as long as the connection lasts.
    Kept,
    /// What was shown is served: where (a folder, an address), and what who feeds it should know of how (Windows
    /// programs cannot reach a mount, say).
    Serving {
        at: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    /// Not taken: another connection feeds it, whose it says.
    Fed { by: String },
    Status {
        pid: u32,
        /// Where it serves; empty until it was given what to serve.
        at: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        /// Who feeds it, where anyone does.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fed_by: Option<String>,
        leases: usize,
        /// How many files are open in what it serves.
        open: usize,
    },
    Stopping,
    /// What was said is done.
    Done,
    Error { why: String },
}

/// A lease on the server: it serves while this is held.
#[derive(Debug)]
pub struct Lease {
    link: Link,
}

impl Lease {
    /// Say something more on the lease's connection, and read the answer.
    ///
    /// # Errors
    /// The server is gone.
    pub fn tell(&mut self, said: &ToServer) -> Result<FromServer> {
        self.link.ask(said)
    }
}

fn lease(mut link: Link, who: &str) -> Result<Lease> {
    match link.ask(&ToServer::Keep { who: who.to_string() })? {
        FromServer::Kept => Ok(Lease { link }),
        FromServer::Error { why } => bail!(why),
        other => bail!("the server answered {other:?}"),
    }
}

/// Keep the server of the project in `root` for `who`, starting one where none runs - `program` being `pe2-cli`,
/// given `args` after `project server` - and waiting for it to listen.
///
/// # Errors
/// No server could be started (nothing can serve the project's folder here, as its log says), or it would not be
/// kept.
pub fn keep(root: &Path, who: &str, program: &Path, args: &[String]) -> Result<Lease> {
    let link = match ROLE.connect(root) {
        Some(link) => link,
        None => ROLE.start(root, program, args)?,
    };
    lease(link, who)
}

/// Keep the server of the project in `root` for `who`, where one runs.
#[must_use]
pub fn keep_running(root: &Path, who: &str) -> Option<Lease> {
    lease(ROLE.connect(root)?, who).ok()
}

/// Whether a server runs for the project in `root`.
#[must_use]
pub fn running(root: &Path) -> bool {
    ROLE.running(root)
}

/// What the server of the project in `root` says it is doing, where one listens.
#[must_use]
pub fn status(root: &Path) -> Option<FromServer> {
    ROLE.connect(root)?.ask(&ToServer::Status).ok()
}

/// Tell the server of the project in `root` to stop, now: whether one was told.
#[must_use]
pub fn stop(root: &Path) -> bool {
    let Some(mut server) = ROLE.connect(root) else { return false };
    matches!(server.ask(&ToServer::Stop), Ok(FromServer::Stopping))
}

#[cfg(test)]
mod tests {
    use super::{FromServer, ToServer};

    #[test]
    fn messages_are_json_by_what_they_say() {
        let said = serde_json::to_string(&ToServer::Keep { who: "pe2-editor".into() }).unwrap();
        assert_eq!(said, r#"{"say":"keep","who":"pe2-editor"}"#);
        assert_eq!(serde_json::to_string(&FromServer::Kept).unwrap(), r#"{"say":"kept"}"#);
    }
}
