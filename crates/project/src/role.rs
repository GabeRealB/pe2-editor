//! A process of a project that others talk to - the host holding it (`host`), the server serving it as a filesystem
//! (`server`): how one is known to run, started on its own, connected to and listened as. What is said goes over a
//! local socket - a file of the project's cache, a named pipe on Windows - one CBOR message a frame (`wire`).

use anyhow::{Context, Result, bail};
use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{ListenerOptions, Name, Stream};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::cache::DIR;

/// One of a project's processes: what `pe2-cli project` starts it as, and what it is known by.
#[derive(Debug)]
pub struct Role {
    /// The command it runs as (`pe2-cli project <command>`), and what it is called where one is said.
    pub command: &'static str,
    /// Its socket, in the cache (Unix).
    pub socket: &'static str,
    /// What its pipe's name starts with (Windows), and its socket's where the cache's path is too long for one.
    pub pipe: &'static str,
    /// The file it locks for as long as it runs: one a project.
    pub lock: &'static str,
    /// What one started by another process writes of itself.
    pub log: &'static str,
}

/// How long one started is waited for to listen, while it runs (one that ends says why at once): long enough for it
/// to read the project.
const STARTING: Duration = Duration::from_secs(120);

impl Role {
    /// Its socket for the project in `root`.
    ///
    /// # Errors
    /// The name cannot be made.
    pub fn name(&self, root: &Path) -> std::io::Result<Name<'static>> {
        crate::host::local_name(root, self.socket, self.pipe)
    }

    /// Whether one runs for the project in `root`: one has its lock.
    #[must_use]
    pub fn running(&self, root: &Path) -> bool {
        let Ok(lock) = std::fs::OpenOptions::new().write(true).open(root.join(DIR).join(self.lock)) else {
            return false;
        };
        let free = lock.try_lock().is_ok();
        if free {
            let _ = lock.unlock();
        }
        !free
    }

    /// Be the one of the project in `root`, for as long as what this returns is held.
    ///
    /// # Errors
    /// One runs already - a second one started in a race ends here, and its starter finds the first.
    pub fn claim(&self, root: &Path) -> Result<std::fs::File> {
        crate::cache::make(root)?;
        let one =
            std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(root.join(DIR).join(self.lock))?;
        if one.try_lock().is_err() {
            bail!("{}: a {} runs for it already", crate::host::shown(root), self.command);
        }
        Ok(one)
    }

    /// Wait for the one of the project in `root` to be gone, this long at most: whether it is.
    #[must_use]
    pub fn gone_within(&self, root: &Path, limit: Duration) -> bool {
        let told = Instant::now();
        while self.running(root) && told.elapsed() < limit {
            std::thread::sleep(Duration::from_millis(50));
        }
        !self.running(root)
    }

    /// Connect to the one of the project in `root`; none where none listens.
    #[must_use]
    pub fn connect(&self, root: &Path) -> Option<Link> {
        Stream::connect(self.name(root).ok()?).ok().map(|stream| Link { stream: BufReader::new(stream) })
    }

    /// Start one for the project in `root`, on its own - not ended with this process, nor with its terminal - and
    /// connect to it once it listens: `program` being `pe2-cli`, given `args` after `project <command>`. What it says
    /// goes to its log in the cache.
    ///
    /// # Errors
    /// It could not be started, or did not listen in time.
    pub fn start(&self, root: &Path, program: &Path, args: &[String]) -> Result<Link> {
        crate::cache::make(root)?;
        let log = std::fs::File::create(root.join(DIR).join(self.log))?;
        let mut command = std::process::Command::new(program);
        command.args(["project", self.command, "--project"]).arg(root).args(args);
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
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x200;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            // A console of its own, with no window: not its starter's, so it does not end with that one's terminal -
            // and not none at all (`DETACHED_PROCESS`, beside which Windows ignores the other), or every console
            // program it came to start would be given a terminal window of its own.
            command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
            keep_output_to_ourselves();
        }
        let mut child = command.spawn().with_context(|| format!("starting {}", program.display()))?;
        let started = Instant::now();
        loop {
            if let Some(link) = self.connect(root) {
                return Ok(link);
            }
            // Ended at once: another won the race to be the one, or it failed - its log says.
            if let Some(status) = child.try_wait()? {
                if let Some(link) = self.connect(root) {
                    return Ok(link);
                }
                let said = std::fs::read_to_string(root.join(DIR).join(self.log)).unwrap_or_default();
                bail!("the {} ended ({status}): {}", self.command, said.trim());
            }
            if started.elapsed() > STARTING {
                bail!("the {} did not listen within {}s", self.command, STARTING.as_secs());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Listen as the one of the project in `root`, each connection given to `connection` on a thread of its own,
    /// until what this returns is dropped or `stopping` says so.
    ///
    /// # Errors
    /// The socket cannot be made.
    pub fn listen(
        &'static self,
        root: &Path,
        stopping: Arc<AtomicBool>,
        connection: impl Fn(Stream) + Send + Sync + 'static,
    ) -> Result<Listening> {
        let listener =
            ListenerOptions::new().name(self.name(root)?).try_overwrite(true).reclaim_name(false).create_sync()?;
        let connection = Arc::new(connection);
        let stopped = stopping.clone();
        let thread = std::thread::Builder::new().name(self.command.into()).spawn(move || {
            for stream in listener.incoming() {
                if stopped.load(Ordering::Relaxed) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let connection = connection.clone();
                let _ = std::thread::Builder::new().name("connection".into()).spawn(move || connection(stream));
            }
        })?;
        Ok(Listening { role: self, root: root.to_path_buf(), stopping, thread: Some(thread) })
    }
}

/// A connection to one of a project's processes.
pub struct Link {
    stream: BufReader<Stream>,
}

impl std::fmt::Debug for Link {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a connection")
    }
}

impl Link {
    /// Say something, and read the answer.
    ///
    /// # Errors
    /// The process is gone, or answered with what is not an answer.
    pub fn ask<Said: Serialize, Answer: DeserializeOwned>(&mut self, said: &Said) -> Result<Answer> {
        crate::wire::send(self.stream.get_mut(), said)?;
        crate::wire::receive(&mut self.stream)?.context("the connection was closed")
    }

    /// Wait for the process to close the connection (it stops).
    pub fn closed(mut self) {
        let mut byte = [0u8; 1];
        while matches!(std::io::Read::read(&mut self.stream, &mut byte), Ok(read) if read > 0) {}
    }
}

/// Listening for what processes say: stopped when dropped.
#[derive(Debug)]
pub struct Listening {
    role: &'static Role,
    root: PathBuf,
    stopping: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Listening {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Relaxed);
        let Some(thread) = self.thread.take() else { return };
        // Woken, it finds itself stopping and lets go of the name. Where it has left already - whoever spoke to it
        // while it was stopping woke it - nobody answers the name any more, and on Windows connecting to it then
        // waits for as long as a connection of it is still open, a keeper's: so never on this thread, which would be
        // the process never stopping.
        if !thread.is_finished()
            && let Ok(name) = self.role.name(&self.root)
        {
            let wake = move || drop(Stream::connect(name));
            let _ = std::thread::Builder::new().name("wake".into()).spawn(wake);
        }
        // Waited for a moment only: where its socket is gone - the project's cache removed under it - nothing wakes
        // it, and it is left to end with the process.
        let asked = Instant::now();
        while !thread.is_finished() && asked.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(5));
        }
        if thread.is_finished() {
            let _ = thread.join();
        }
    }
}

/// Keep what this process reads and writes - its standard input, output and error - from the processes it starts
/// from now on, but those given them. Windows hands a process started every handle of its starter that may be
/// inherited, whatever it is given as its own: a host started by a command would hold on to what the command writes
/// to, and whoever reads that - a script, a test, a terminal's pipe - would wait for the host to end.
#[cfg(windows)]
fn keep_output_to_ourselves() {
    use std::os::windows::io::AsRawHandle;
    unsafe extern "system" {
        fn SetHandleInformation(handle: *mut std::ffi::c_void, mask: u32, flags: u32) -> i32;
    }
    const HANDLE_FLAG_INHERIT: u32 = 1;
    let ours = [std::io::stdin().as_raw_handle(), std::io::stdout().as_raw_handle(), std::io::stderr().as_raw_handle()];
    for handle in ours.into_iter().filter(|handle| !handle.is_null()) {
        // SAFETY: a handle of this process, open for as long as it runs; only whether it is inherited changes.
        unsafe { SetHandleInformation(handle.cast(), HANDLE_FLAG_INHERIT, 0) };
    }
}
