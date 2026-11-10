//! The host: the process holding a project while no editor does (`pe2-cli project host`) - its lock, its index and a
//! history of its own - which answers what is asked of the project on the holder's link (`mcp::link`): the edits the
//! command line and agents make, what programs write through the folder served, the reads, the builds. One is
//! started by whoever asks something of a project nothing holds (`ensure`), and by `pe2-cli project serve`; it stops a
//! while after nothing was asked of it and nothing keeps it. Where it serves the project's folder it keeps the server
//! doing so (`server`) and feeds it what is served, as an editor does.
//!
//! A process taking the project lock while the host holds it borrows it (`Lock::try_acquire`): the host lets go of
//! the lock, its history, its index and its link, and no longer feeds the server - whoever holds the project does -
//! until the borrower lets go and it takes the project back.
//!
//! What a process says to the host goes over a local socket - `.cache/host.sock`, a named pipe on Windows - one CBOR
//! message a frame (`ToHost`, `FromHost`, `wire`). A connection asking to keep the folder served (`Keep`) or borrowing
//! the project (`Lend`) is a lease while it stays open.

use anyhow::{Context, Result};
use interprocess::local_socket::Name;
use interprocess::local_socket::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

use crate::role::{Link, Role};

/// The host, as a process of a project.
pub static ROLE: Role = Role { command: "host", socket: "host.sock", pipe: "pe2-host", lock: "host.lock", log: "host.log" };

/// What a process says to the host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "say", rename_all = "snake_case")]
pub enum ToHost {
    /// Keep the project's folder served while this connection lasts.
    Keep { who: String },
    /// Let go of the project for this connection's process, until the connection ends.
    Lend { who: String },
    /// What it is doing.
    Status,
    /// Stop, now.
    Stop,
}

/// What the host answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "say", rename_all = "snake_case")]
pub enum FromHost {
    /// The folder is served, where (a folder, an address), for as long as the connection lasts.
    Kept {
        at: String,
        /// What who keeps it should know of how it is served (Windows programs cannot reach a mount, say).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    /// The project's lock let go of: take it.
    Lent,
    /// It does not hold the project: who does, as the lock says.
    NotHolding {
        holder: String,
    },
    Status {
        pid: u32,
        /// Whether it keeps the project's folder served.
        #[serde(default)]
        serves: bool,
        holder: String,
        leases: usize,
    },
    Stopping,
    Error {
        why: String,
    },
}

/// The project in `root`'s path, hashed: what keeps a name global to the machine to that project.
fn hashed(root: &Path) -> String {
    use sha1::Digest;
    use std::fmt::Write;
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let digest = sha1::Sha1::digest(root.to_string_lossy().to_lowercase().as_bytes());
    digest.iter().take(8).fold(String::new(), |mut hex, b| {
        let _ = write!(hex, "{b:02x}");
        hex
    })
}

/// A name for a local socket of the project in `root`: a file of its cache on Unix - or of the temporary folder, named by
/// `prefix` and its path, hashed, where the cache's is longer than a socket's path can be - and a pipe named so on
/// Windows, where a pipe's name is global.
///
/// # Errors
/// The name cannot be made.
pub fn local_name(root: &Path, file: &str, prefix: &str) -> std::io::Result<Name<'static>> {
    #[cfg(unix)]
    {
        use interprocess::local_socket::GenericFilePath;
        // `sun_path`: 108 bytes on Linux, 104 on macOS, its terminating zero among them.
        let path = root.join(crate::cache::DIR).join(file);
        let path = if path.as_os_str().len() < 100 {
            path
        } else {
            std::env::temp_dir().join(format!("{prefix}-{}.sock", hashed(root)))
        };
        path.to_fs_name::<GenericFilePath>()
    }
    #[cfg(windows)]
    {
        use interprocess::local_socket::GenericNamespaced;
        let _ = file;
        format!("{prefix}-{}", hashed(root)).to_ns_name::<GenericNamespaced>()
    }
}

/// A connection to the host.
#[derive(Debug)]
pub struct Connection {
    link: Link,
}

impl Connection {
    /// Connect to the host of the project in `root`; none where none listens.
    #[must_use]
    pub fn connect(root: &Path) -> Option<Connection> {
        ROLE.connect(root).map(|link| Connection { link })
    }

    /// Say something to the host, and read its answer.
    ///
    /// # Errors
    /// The host is gone, or answered with what is not an answer.
    pub fn ask(&mut self, said: &ToHost) -> Result<FromHost> {
        self.link.ask(said).context("the host")
    }

    /// Wait for the host to close the connection (it stops).
    pub fn closed(self) {
        self.link.closed();
    }
}

/// The project borrowed from the host: it takes it back once this is dropped.
#[derive(Debug)]
pub struct Lease {
    _connection: Connection,
}

/// Borrow the project from the host holding it, for `who`: the host lets go of the lock, and takes it back once the
/// lease is dropped. None where no host holds it.
#[must_use]
pub fn borrow(root: &Path, who: &str) -> Option<Lease> {
    let mut connection = Connection::connect(root)?;
    match connection.ask(&ToHost::Lend { who: who.to_string() }) {
        Ok(FromHost::Lent) => Some(Lease { _connection: connection }),
        _ => None,
    }
}

/// How long a host told to stop is waited for to be gone.
const STOPPING: Duration = Duration::from_secs(30);

/// Whether a host runs for the project in `root`.
#[must_use]
pub fn running(root: &Path) -> bool {
    ROLE.running(root)
}

/// A connection to a host keeping the project's folder served, and whether it was started for this: the one running,
/// or one started where none runs - `program` being `pe2-cli`, given `args` after `project host`. A host that only
/// holds the project, started to answer what commands ask (`ensure`), serves nowhere: it is told to stop, and one
/// that serves started in its place.
///
/// # Errors
/// No host could be started.
pub fn serving(root: &Path, program: &Path, args: &[String]) -> Result<(Connection, bool)> {
    if let Some(mut host) = Connection::connect(root) {
        match host.ask(&ToHost::Status) {
            Ok(FromHost::Status { serves: false, .. }) => {
                let _ = host.ask(&ToHost::Stop);
                drop(host);
                let _ = ROLE.gone_within(root, STOPPING);
            }
            _ => return Ok((host, false)),
        }
    }
    Ok((start(root, program, args)?, true))
}

/// The program a host is started as, where this one starts hosts for what is asked of a project (`ensure`).
static STARTER: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// Start hosts as this program (`pe2-cli`) from now on, where something is asked of a project nothing holds.
pub fn start_hosts_with(program: std::path::PathBuf) {
    let _ = STARTER.set(program);
}

/// Have a host run for the project in `root`, to hold it and answer what is asked of it: started where none runs,
/// serving its folder nowhere. Whether one runs. None is started by a program that was not told what a host is
/// started as (`start_hosts_with`), nor by one working under another's lock - which then does the work itself.
#[must_use]
pub fn ensure(root: &Path) -> bool {
    let Some(program) = STARTER.get() else { return false };
    if crate::lock::inherited(root) {
        return false;
    }
    if Connection::connect(root).is_some() {
        return true;
    }
    match start(root, program, &["--backend".to_string(), "none".to_string()]) {
        Ok(_) => true,
        Err(e) => {
            static SAID: std::sync::Once = std::sync::Once::new();
            SAID.call_once(|| eprintln!("no host could be started for {}: {e:#}; the work is done here", shown(root)));
            false
        }
    }
}

/// Have a host stand by for the project in `root`, which this process holds: the one running, or one started -
/// `program` being `pe2-cli` - which holds nothing while this process has the project and takes it once let go, to
/// keep its folder served and answer what is asked of it from then on.
///
/// # Errors
/// None runs, and none could be started.
pub fn stand_by(root: &Path, program: &Path) -> Result<()> {
    if Connection::connect(root).is_some() {
        return Ok(());
    }
    start(root, program, &["--backend".to_string(), "none".to_string()]).map(drop)
}

/// Tell the host of the project in `root` to stop, where nobody keeps it, and wait for it to be gone: whether it is.
/// One something keeps - an editor it serves the folder for, `project serve` - is left.
#[must_use]
pub fn replace(root: &Path) -> bool {
    let Some(mut host) = Connection::connect(root) else { return !running(root) };
    if !matches!(host.ask(&ToHost::Status), Ok(FromHost::Status { leases: 0, .. })) {
        return false;
    }
    let _ = host.ask(&ToHost::Stop);
    drop(host);
    ROLE.gone_within(root, STOPPING)
}

/// Whether the host of the project in `root` holds it, or is about to (nobody does): its link answers, or will. Not
/// where it lent the project, or something else holds it.
#[must_use]
pub fn answers(root: &Path) -> bool {
    let status = Connection::connect(root).and_then(|mut host| host.ask(&ToHost::Status).ok());
    matches!(status, Some(FromHost::Status { holder, .. }) if holder == "the host" || holder == "nobody")
}

/// Start a host for the project in `root`, on its own - not ended with this process, nor with its terminal - and
/// connect to it once it listens. What it says goes to its log in the cache.
///
/// # Errors
/// It could not be started, or did not listen in time.
pub fn start(root: &Path, program: &Path, args: &[String]) -> Result<Connection> {
    ROLE.start(root, program, args).map(|link| Connection { link })
}

/// A path as a person reads it: on Windows, without the verbatim prefix a canonical path has (`\\?\D:\...`).
#[must_use]
pub fn shown(path: &Path) -> String {
    let text = path.display().to_string();
    match text.strip_prefix(r"\\?\") {
        Some(rest) => rest.strip_prefix("UNC\\").map_or_else(|| rest.to_string(), |share| format!(r"\\{share}")),
        None => text,
    }
}

/// Where `pe2-cli` is for a program beside it (the editor): in the same folder.
#[must_use]
pub fn cli_beside(program: &Path) -> std::path::PathBuf {
    program.with_file_name(format!("pe2-cli{}", std::env::consts::EXE_SUFFIX))
}

#[cfg(test)]
mod tests {
    use super::{FromHost, ToHost};

    #[test]
    fn a_path_is_shown_without_its_verbatim_prefix() {
        use std::path::Path;
        assert_eq!(super::shown(Path::new(r"\\?\D:\p\assets")), r"D:\p\assets");
        assert_eq!(super::shown(Path::new(r"\\?\UNC\wsl.localhost\p")), r"\\wsl.localhost\p");
        assert_eq!(super::shown(Path::new("/home/p")), "/home/p");
    }

    #[test]
    fn messages_are_json_by_what_they_say() {
        let said = serde_json::to_string(&ToHost::Lend { who: "pe2-editor".into() }).unwrap();
        assert_eq!(said, r#"{"say":"lend","who":"pe2-editor"}"#);
        assert_eq!(serde_json::to_string(&FromHost::Lent).unwrap(), r#"{"say":"lent"}"#);
    }
}
