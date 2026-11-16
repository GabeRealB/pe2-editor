//! The host's I/O, given rather than reached for. What code reads of the outside world - the time, randomness, the
//! environment, files - and does in it - watching files, starting programs, talking to other processes and over the
//! network - it does through an [`Io`] it is given, never with the system directly, so that any part of it can be
//! another: the system's ([`system`]), one a test drives ([`Standing`], [`Skipping`], [`Seeded`], [`Fixed`], [`Faulty`], [`Telling`],
//! [`Scripted`], [`InMemory`], [`Served`], [`Closed`]), or any other, mixed ([`Composed`]).
//!
//! Each part is a trait shaped like the WASI interface for it - [`Clock`] like `wasi:clocks`, [`Random`] like
//! `wasi:random`, [`Environment`] like `wasi:cli`'s, [`Filesystem`] and its [`Descriptor`]s like `wasi:filesystem`,
//! [`Http`] like `wasi:http`'s outgoing handler, [`Sockets`] like `wasi:sockets` - its model and operations, not its
//! types: names are this workspace's checked paths, calls are synchronous, errors are `std::io`'s. What WASI has no
//! interface for is the host's own: [`Watch`], [`Processes`], [`Channels`]. The traits are object-safe: owners hold an
//! `Arc<dyn Io>`, and a call costs one virtual call.
//!
//! A place on disk is a [`HostPath`].

mod channels;
mod clock;
mod environment;
mod faulty;
mod filesystem;
mod host;
mod http;
mod processes;
mod random;
mod sockets;
mod watch;

pub use channels::{Channel, ChannelListener, ChannelName, Channels, InMemory, SystemChannels};

pub use clock::{Clock, Receiver, Sender, Signal, Skipping, Standing, SystemClock, Waiter, channel, sleep};
pub use environment::{Environment, Fixed, SystemEnvironment};
pub use faulty::{Fault, Faulty};
pub use filesystem::{
    Appending, Descriptor, DirEntry, Filesystem, Kind, LockKind, Mapped, Mapping, Open, Stat, SystemDescriptor,
    SystemFilesystem, copy_all, create_directory_all_at, is_empty_folder, read_file_at, read_whole, remove_all_at,
    sync_all, sync_file, write_all_at, write_file_at,
};
pub use host::{HostPath, NotWithin};
pub use http::{Http, Request, Response, Served, SystemHttp};
pub use processes::{Exit, Output, Process, Processes, Program, Scripted, Stream, SystemProcesses};
pub use random::{Random, Seeded, SystemRandom};
pub use sockets::{Closed, SocketUse, Sockets, SystemSockets};
pub use watch::{Change, ChangeKind, Depth, SystemWatch, Tell, Telling, Watch, Watching};

use std::sync::Arc;

/// The host's I/O, as one: each part given by whatever it is assembled of.
pub trait Io: Send + Sync + std::fmt::Debug {
    /// What tells the time, and waits for it.
    fn clock(&self) -> &dyn Clock;
    /// Where randomness comes from.
    fn random(&self) -> &dyn Random;
    /// What the program was started with: its environment, arguments and places.
    fn environment(&self) -> &dyn Environment;
    /// Where files are.
    fn filesystem(&self) -> &dyn Filesystem;
    /// What tells of files as they change.
    fn watch(&self) -> &dyn Watch;
    /// What starts programs.
    fn processes(&self) -> &dyn Processes;
    /// What connects the processes of the machine.
    fn channels(&self) -> &dyn Channels;
    /// What asks over HTTP.
    fn http(&self) -> &dyn Http;
    /// What opens sockets, and says where a component's may go.
    fn sockets(&self) -> &dyn Sockets;
}

/// The system's I/O, every part of it.
#[derive(Clone, Copy, Debug, Default)]
pub struct System;

impl Io for System {
    fn clock(&self) -> &dyn Clock {
        &SystemClock
    }

    fn random(&self) -> &dyn Random {
        &SystemRandom
    }

    fn environment(&self) -> &dyn Environment {
        &SystemEnvironment
    }

    fn filesystem(&self) -> &dyn Filesystem {
        &SystemFilesystem
    }

    fn watch(&self) -> &dyn Watch {
        &SystemWatch
    }

    fn processes(&self) -> &dyn Processes {
        &SystemProcesses
    }

    fn channels(&self) -> &dyn Channels {
        &SystemChannels
    }

    fn http(&self) -> &dyn Http {
        &SystemHttp
    }

    fn sockets(&self) -> &dyn Sockets {
        &SystemSockets
    }
}

/// The system's I/O, as owners hold it.
#[must_use]
pub fn system() -> Arc<dyn Io> {
    Arc::new(System)
}

/// I/O held by what is made by `Default`: the system's until one is given (`Held(io)`).
#[derive(Clone, Debug)]
pub struct Held(pub Arc<dyn Io>);

impl Default for Held {
    fn default() -> Held {
        Held(system())
    }
}

impl std::ops::Deref for Held {
    type Target = Arc<dyn Io>;

    fn deref(&self) -> &Arc<dyn Io> {
        &self.0
    }
}

/// I/O of any parts: the system's where unsaid ([`Composed::system`]), each replaced as a test needs.
#[derive(Clone)]
pub struct Composed {
    pub clock: Arc<dyn Clock>,
    pub random: Arc<dyn Random>,
    pub environment: Arc<dyn Environment>,
    pub filesystem: Arc<dyn Filesystem>,
    pub watch: Arc<dyn Watch>,
    pub processes: Arc<dyn Processes>,
    pub channels: Arc<dyn Channels>,
    pub http: Arc<dyn Http>,
    pub sockets: Arc<dyn Sockets>,
}

impl std::fmt::Debug for Composed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("composed I/O")
    }
}

impl Composed {
    /// The system's parts, to replace some of (`Composed { clock: .., ..Composed::system() }`).
    #[must_use]
    pub fn system() -> Composed {
        Composed {
            clock: Arc::new(SystemClock),
            random: Arc::new(SystemRandom),
            environment: Arc::new(SystemEnvironment),
            filesystem: Arc::new(SystemFilesystem),
            watch: Arc::new(SystemWatch),
            processes: Arc::new(SystemProcesses),
            channels: Arc::new(SystemChannels),
            http: Arc::new(SystemHttp),
            sockets: Arc::new(SystemSockets),
        }
    }
}

impl Io for Composed {
    fn clock(&self) -> &dyn Clock {
        &*self.clock
    }

    fn random(&self) -> &dyn Random {
        &*self.random
    }

    fn environment(&self) -> &dyn Environment {
        &*self.environment
    }

    fn filesystem(&self) -> &dyn Filesystem {
        &*self.filesystem
    }

    fn watch(&self) -> &dyn Watch {
        &*self.watch
    }

    fn processes(&self) -> &dyn Processes {
        &*self.processes
    }

    fn channels(&self) -> &dyn Channels {
        &*self.channels
    }

    fn http(&self) -> &dyn Http {
        &*self.http
    }

    fn sockets(&self) -> &dyn Sockets {
        &*self.sockets
    }
}
