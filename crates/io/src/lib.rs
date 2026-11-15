//! The host's I/O, given rather than reached for. What code reads of the outside world - the time, randomness, the
//! environment - it reads through an [`Io`] it is given, never from the system directly, so that any part of it can be
//! another: the system's ([`system`]), one a test drives ([`Standing`], [`Seeded`], [`Fixed`]), or any other, mixed
//! ([`Composed`]).
//!
//! Each part is a trait shaped like the WASI interface for it - [`Clock`] like `wasi:clocks`, [`Random`] like
//! `wasi:random`, [`Environment`] like `wasi:cli`'s - its model and operations, not its types: names are this
//! workspace's checked paths, calls are synchronous, errors are `std::io`'s. The traits are object-safe: owners hold an
//! `Arc<dyn Io>`, and a call costs one virtual call.
//!
//! A place on disk is a [`HostPath`].

mod clock;
mod environment;
mod host;
mod random;

pub use clock::{Clock, Receiver, Sender, Signal, Standing, SystemClock, Waiter, channel, sleep};
pub use environment::{Environment, Fixed, SystemEnvironment};
pub use host::{HostPath, NotWithin};
pub use random::{Random, Seeded, SystemRandom};

use std::sync::Arc;

/// The host's I/O, as one: each part given by whatever it is assembled of.
pub trait Io: Send + Sync {
    /// What tells the time, and waits for it.
    fn clock(&self) -> &dyn Clock;
    /// Where randomness comes from.
    fn random(&self) -> &dyn Random;
    /// What the program was started with: its environment, arguments and places.
    fn environment(&self) -> &dyn Environment;
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
}

/// The system's I/O, as owners hold it.
#[must_use]
pub fn system() -> Arc<dyn Io> {
    Arc::new(System)
}

/// I/O of any parts: the system's where unsaid ([`Composed::system`]), each replaced as a test needs.
#[derive(Clone)]
pub struct Composed {
    pub clock: Arc<dyn Clock>,
    pub random: Arc<dyn Random>,
    pub environment: Arc<dyn Environment>,
}

impl Composed {
    /// The system's parts, to replace some of (`Composed { clock: .., ..Composed::system() }`).
    #[must_use]
    pub fn system() -> Composed {
        Composed {
            clock: Arc::new(SystemClock),
            random: Arc::new(SystemRandom),
            environment: Arc::new(SystemEnvironment),
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
}
