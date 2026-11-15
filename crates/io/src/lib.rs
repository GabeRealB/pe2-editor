//! The host's I/O, given rather than reached for. What code reads of the outside world - the time, randomness, the
//! environment - it reads through an [`Io`] it is given, never from the system directly, so that any part of it can be
//! another: the system's ([`system`]), one a test drives ([`Standing`], [`Seeded`], [`Fixed`]), or any other, mixed
//! ([`Composed`]).
//!
//! Each part is a trait shaped like the WASI interface for it - [`Clock`] like `wasi:clocks`, [`Random`] like
//! `wasi:random`, [`Environment`] like `wasi:cli`'s, [`Filesystem`] and its [`Descriptor`]s like `wasi:filesystem` - its model and operations, not its types: names are this
//! workspace's checked paths, calls are synchronous, errors are `std::io`'s. The traits are object-safe: owners hold an
//! `Arc<dyn Io>`, and a call costs one virtual call.
//!
//! A place on disk is a [`HostPath`].

mod clock;
mod environment;
mod filesystem;
mod host;
mod random;

pub use clock::{Clock, Receiver, Sender, Signal, Standing, SystemClock, Waiter, channel, sleep};
pub use environment::{Environment, Fixed, SystemEnvironment};
pub use filesystem::{
    Descriptor, DirEntry, Filesystem, Kind, LockKind, Mapped, Mapping, Open, Stat, SystemDescriptor, SystemFilesystem,
    create_directory_all_at, read_file_at, read_whole, remove_all_at, write_all_at, write_file_at,
};
pub use host::{HostPath, NotWithin};
pub use random::{Random, Seeded, SystemRandom};

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
}
