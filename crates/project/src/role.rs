//! A process of a project that others talk to - the host holding it (`host`), the server serving it as a filesystem
//! (`server`): how one is known to run, started on its own, connected to and listened as. What is said goes over a
//! local socket - a file of the project's cache, a named pipe on Windows - one CBOR message a frame (`wire`).

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde::de::DeserializeOwned;
use sfs::HostPath;
use std::io::BufReader;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::cache::DIR;

/// One of a project's processes: what `pe2-cli project` starts it as, and what it is known by.
#[derive(Debug)]
pub struct Role {
    /// The command it runs as (`pe2-cli project <command>`), and what it is called where one is said.
    pub command: &'static str,
    /// Its socket, in the cache (Unix).
    pub socket: &'static pe2_path::Name,
    /// What its pipe's name starts with (Windows), and its socket's where the cache's path is too long for one.
    pub pipe: &'static str,
    /// The file it locks for as long as it runs: one a project.
    pub lock: &'static pe2_path::Name,
    /// What one started by another process writes of itself.
    pub log: &'static pe2_path::Name,
}

/// How long one started is waited for to listen, while it runs (one that ends says why at once): long enough for it
/// to read the project.
const STARTING: Duration = Duration::from_secs(120);

impl Role {
    /// Its socket for the project in `root`.
    ///
    /// # Errors
    /// The name cannot be made.
    pub fn name(&self, root: &HostPath) -> std::io::Result<pe2_io::ChannelName> {
        crate::host::local_name(root, self.socket, self.pipe)
    }

    /// Whether one runs for the project in `root`: one has its lock.
    #[must_use]
    #[allow(
        clippy::disallowed_methods,
        reason = "how this project's processes find each other - its lock, its roles and their logs - which every process reads as the system has it"
    )]
    pub fn running(&self, root: &HostPath) -> bool {
        let Ok(lock) = std::fs::OpenOptions::new().write(true).open(root.join(&DIR).join(&self.lock)) else {
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
    #[allow(
        clippy::disallowed_methods,
        reason = "how this project's processes find each other - its lock, its roles and their logs - which every process reads as the system has it"
    )]
    pub fn claim(&self, root: &HostPath) -> Result<std::fs::File> {
        crate::cache::make(&pe2_io::SystemFilesystem, root)?;
        let one = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(root.join(&DIR).join(&self.lock))?;
        if one.try_lock().is_err() {
            bail!("{root}: a {} runs for it already", self.command);
        }
        Ok(one)
    }

    /// Wait for the one of the project in `root` to be gone, this long at most: whether it is.
    #[must_use]
    pub fn gone_within(&self, root: &HostPath, limit: Duration, clock: &dyn pe2_io::Clock) -> bool {
        let told = clock.now();
        while self.running(root) && clock.now().saturating_duration_since(told) < limit {
            pe2_io::sleep(clock, Duration::from_millis(50));
        }
        !self.running(root)
    }

    /// Connect to the one of the project in `root`; none where none listens.
    #[must_use]
    pub fn connect(&self, root: &HostPath, io: &dyn pe2_io::Io) -> Option<Link> {
        io.channels().connect(&self.name(root).ok()?).ok().map(|stream| Link { stream: BufReader::new(stream) })
    }

    /// Start one for the project in `root`, on its own - not ended with this process, nor with its terminal - and
    /// connect to it once it listens: `program` being `pe2-cli`, given `args` after `project <command>`. What it says
    /// goes to its log in the cache.
    ///
    /// # Errors
    /// It could not be started, or did not listen in time.
    #[allow(
        clippy::disallowed_methods,
        reason = "how this project's processes find each other - its lock, its roles and their logs - which every process reads as the system has it"
    )]
    pub fn start(
        &self,
        root: &HostPath,
        program: &std::path::Path,
        args: &[String],
        io: &dyn pe2_io::Io,
    ) -> Result<Link> {
        crate::cache::make(&pe2_io::SystemFilesystem, root)?;
        let log = root.join(&DIR).join(&self.log);
        // A group of its own: a Ctrl-C in the terminal that started it is not its, nor is that terminal's closing.
        let started = pe2_io::Program::new(program)
            .args(["project", self.command, "--project"])
            .arg(root.as_os_str())
            .args(args)
            .streams(pe2_io::Stream::Null, pe2_io::Stream::Into(log.clone()), pe2_io::Stream::Into(log))
            .own_group();
        #[cfg(windows)]
        keep_output_to_ourselves();
        let mut child = io.processes().spawn(&started).with_context(|| format!("starting {}", program.display()))?;
        let clock = io.clock();
        let started = clock.now();
        loop {
            if let Some(link) = self.connect(root, io) {
                return Ok(link);
            }
            // Ended at once: another won the race to be the one, or it failed - its log says.
            if let Some(status) = child.try_wait()? {
                if let Some(link) = self.connect(root, io) {
                    return Ok(link);
                }
                let said = std::fs::read_to_string(root.join(&DIR).join(&self.log)).unwrap_or_default();
                bail!("the {} ended ({status}): {}", self.command, said.trim());
            }
            if clock.now().saturating_duration_since(started) > STARTING {
                bail!("the {} did not listen within {}s", self.command, STARTING.as_secs());
            }
            pe2_io::sleep(clock, Duration::from_millis(50));
        }
    }

    /// Listen as the one of the project in `root`, each connection given to `connection` on a thread of its own,
    /// until what this returns is dropped or `stopping` says so.
    ///
    /// # Errors
    /// The socket cannot be made.
    pub fn listen(
        &'static self,
        root: &HostPath,
        stopping: Arc<AtomicBool>,
        io: Arc<dyn pe2_io::Io>,
        connection: impl Fn(Box<dyn pe2_io::Channel>) + Send + Sync + 'static,
    ) -> Result<Listening> {
        let listener = io.channels().listen(&self.name(root)?)?;
        let connection = Arc::new(connection);
        let stopped = stopping.clone();
        let thread = std::thread::Builder::new().name(self.command.into()).spawn(move || {
            loop {
                let stream = listener.accept();
                if stopped.load(Ordering::Relaxed) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let connection = connection.clone();
                let _ = std::thread::Builder::new().name("connection".into()).spawn(move || connection(stream));
            }
        })?;
        Ok(Listening { role: self, root: root.clone(), stopping, io, thread: Some(thread) })
    }
}

/// A connection to one of a project's processes.
pub struct Link {
    stream: BufReader<Box<dyn pe2_io::Channel>>,
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
pub struct Listening {
    role: &'static Role,
    root: HostPath,
    stopping: Arc<AtomicBool>,
    /// What its waits are timed by.
    io: Arc<dyn pe2_io::Io>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl std::fmt::Debug for Listening {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Listening").field("role", &self.role).field("root", &self.root).finish_non_exhaustive()
    }
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
            let io = self.io.clone();
            let wake = move || drop(io.channels().connect(&name));
            let _ = std::thread::Builder::new().name("wake".into()).spawn(wake);
        }
        // Waited for a moment only: where its socket is gone - the project's cache removed under it - nothing wakes
        // it, and it is left to end with the process.
        let clock = self.io.clock();
        let asked = clock.now();
        while !thread.is_finished() && clock.now().saturating_duration_since(asked) < Duration::from_secs(2) {
            pe2_io::sleep(clock, Duration::from_millis(5));
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
