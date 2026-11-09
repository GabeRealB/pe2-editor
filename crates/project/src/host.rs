//! The host: the process serving a project as a filesystem (`pe2-cli project host`), which keeps serving it for as long
//! as anyone wants it - an editor, a `project serve`, a program still using what is served - whoever holds the project
//! meanwhile. It holds the project itself while nothing else does; a process taking the project lock while the host
//! holds it borrows it (`Lock::try_acquire`): the host lets go of the lock, its history and the cache's record store, and
//! serves on from what it reads of the layers, sending what programs write to whoever holds the project, until the
//! borrower lets go and it takes the project back. The mount never goes away in between.
//!
//! What a process says to the host goes over a local socket - `.cache/host.sock`, a named pipe on Windows - one CBOR
//! message a frame (`ToHost`, `FromHost`, `wire`). A connection asking to keep it (`Keep`) or borrowing the project (`Lend`) is
//! a lease while it stays open: the host stops a while after none is left, once no file is open in what it serves -
//! what only looks at it keeps nothing - unless the machine says to keep it (`[filesystem] keep`).

use anyhow::{Context, Result, bail};
use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{Name, Stream};
use serde::{Deserialize, Serialize};
use std::io::BufReader;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::cache::DIR;

/// The host's socket, in the cache (Unix).
pub const SOCKET: &str = "host.sock";
/// The file a host locks for as long as it runs: one host a project.
pub const LOCK: &str = "host.lock";
/// What a host started by another process writes of itself.
pub const LOG: &str = "host.log";

/// What is served of the project, by each path in the served folder (`Index::served`): what the holder sends the host,
/// which serves it while it does not hold the project itself.
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
    },
}

/// What changed from one listing to another: what is new or other now, and what is gone.
#[must_use]
pub fn changes(before: &Listing, now: &Listing) -> (Vec<(String, Shown)>, Vec<String>) {
    let shown = now.iter().filter(|(path, it)| before.get(*path) != Some(*it)).map(|(p, it)| (p.clone(), it.clone()));
    let gone = before.keys().filter(|path| !now.contains_key(*path)).cloned();
    (shown.collect(), gone.collect())
}

/// What a process says to the host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "say", rename_all = "snake_case")]
pub enum ToHost {
    /// Keep serving while this connection lasts.
    Keep { who: String },
    /// Let go of the project for this connection's process, until the connection ends.
    Lend { who: String },
    /// These files changed - an edit made by whoever holds the project - for what is served to follow at once.
    Changed { paths: Vec<std::path::PathBuf> },
    /// What is served, from whoever holds the project: the whole listing (`whole`), or what changed since the last.
    Shown { whole: bool, shown: Vec<(String, Shown)>, gone: Vec<String> },
    /// What it is doing.
    Status,
    /// Stop serving, now.
    Stop,
}

/// What the host answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "say", rename_all = "snake_case")]
pub enum FromHost {
    /// Serving, where (a folder, an address), for as long as the connection lasts.
    Kept {
        at: String,
        /// What who keeps it should know of how it serves (Windows programs cannot reach a mount, say).
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
        at: String,
        holder: String,
        leases: usize,
        /// How many files are open in what it serves.
        open: usize,
    },
    Stopping,
    /// What was said is done.
    Done,
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
        let path = root.join(DIR).join(file);
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

/// The host's socket for the project in `root`.
///
/// # Errors
/// The name cannot be made.
pub fn name(root: &Path) -> std::io::Result<Name<'static>> {
    local_name(root, SOCKET, "pe2-host")
}

/// A connection to the host.
pub struct Connection {
    stream: BufReader<Stream>,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("host connection")
    }
}

impl Connection {
    /// Connect to the host serving the project in `root`; none where none does.
    #[must_use]
    pub fn connect(root: &Path) -> Option<Connection> {
        Stream::connect(name(root).ok()?).ok().map(|stream| Connection { stream: BufReader::new(stream) })
    }

    /// Say something to the host, and read its answer.
    ///
    /// # Errors
    /// The host is gone, or answered with what is not an answer.
    pub fn ask(&mut self, said: &ToHost) -> Result<FromHost> {
        crate::wire::send(self.stream.get_mut(), said)?;
        crate::wire::receive(&mut self.stream)?.context("the host closed the connection")
    }

    /// Wait for the host to close the connection (it stops).
    pub fn closed(mut self) {
        while matches!(crate::wire::receive::<FromHost>(&mut self.stream), Ok(Some(_))) {}
    }
}

/// A lease on the host: it serves while this is held.
#[derive(Debug)]
pub struct Lease {
    connection: Connection,
    /// Where it serves.
    pub at: String,
    /// What the host says of how it serves.
    pub note: Option<String>,
}

impl Lease {
    /// Say something more on the lease's connection, and read the answer.
    ///
    /// # Errors
    /// The host is gone.
    pub fn tell(&mut self, said: &ToHost) -> Result<FromHost> {
        self.connection.ask(said)
    }
}

/// Borrow the project from the host holding it, for `who`: the host lets go of the lock, and takes it back once the
/// lease is dropped. None where no host holds it.
#[must_use]
pub fn borrow(root: &Path, who: &str) -> Option<Lease> {
    let mut connection = Connection::connect(root)?;
    match connection.ask(&ToHost::Lend { who: who.to_string() }) {
        Ok(FromHost::Lent) => Some(Lease { connection, at: String::new(), note: None }),
        _ => None,
    }
}

/// Keep the host serving the project in `root` for `who`, starting one where none runs - `program` being `pe2-cli`,
/// given `args` after `project host` - and waiting for it to listen.
///
/// # Errors
/// No host could be started, or it would not keep serving.
pub fn keep(root: &Path, who: &str, program: &Path, args: &[String]) -> Result<Lease> {
    let mut connection = match Connection::connect(root) {
        Some(connection) => connection,
        None => start(root, program, args)?,
    };
    match connection.ask(&ToHost::Keep { who: who.to_string() })? {
        FromHost::Kept { at, note } => Ok(Lease { connection, at, note }),
        FromHost::Error { why } => bail!(why),
        other => bail!("the host answered {other:?}"),
    }
}

/// How long a host started is waited for.
const STARTING: Duration = Duration::from_secs(30);

/// Start a host for the project in `root`, on its own - not ended with this process, nor with its terminal - and connect
/// to it once it listens. What it says goes to its log in the cache.
///
/// # Errors
/// It could not be started, or did not listen in time.
pub fn start(root: &Path, program: &Path, args: &[String]) -> Result<Connection> {
    crate::cache::make(root)?;
    let log = std::fs::File::create(root.join(DIR).join(LOG))?;
    let mut command = std::process::Command::new(program);
    command.args(["project", "host", "--project"]).arg(root).args(args);
    command.stdin(std::process::Stdio::null()).stdout(log.try_clone()?).stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // A group of its own: a Ctrl-C in the terminal that started it is not its.
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x8;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
    let mut child = command.spawn().with_context(|| format!("starting {}", program.display()))?;
    let started = Instant::now();
    loop {
        if let Some(connection) = Connection::connect(root) {
            return Ok(connection);
        }
        // Ended at once: another host won the race to serve, or it failed - its log says.
        if let Some(status) = child.try_wait()? {
            if let Some(connection) = Connection::connect(root) {
                return Ok(connection);
            }
            let said = std::fs::read_to_string(root.join(DIR).join(LOG)).unwrap_or_default();
            bail!("the host ended ({status}): {}", said.trim());
        }
        if started.elapsed() > STARTING {
            bail!("the host did not listen within {}s", STARTING.as_secs());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Tell the host serving the project in `root`, where one does, that these files changed.
pub fn changed(root: &Path, paths: Vec<std::path::PathBuf>) {
    if let Some(mut host) = Connection::connect(root) {
        let _ = host.ask(&ToHost::Changed { paths });
    }
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
