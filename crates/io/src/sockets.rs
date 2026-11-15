//! Sockets, as `wasi:sockets` has them: a TCP listener at an address, and whether an address may be bound or connected
//! to - what an unsandboxed component's own sockets are held to. A listener is the system's socket: what serves on it
//! (an HTTP server's runtime) takes one. [`Closed`] is a run that reaches no network.

use std::io;
use std::net::SocketAddr;

/// What a socket is to do at an address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SocketUse {
    /// Be bound there, to listen or to connect from.
    Bind,
    /// Listen there.
    Listen,
    /// Accept a connection from there.
    Accept,
    /// Connect there.
    Connect,
    /// Send there, or receive from there (datagrams).
    Datagram,
}

/// What opens sockets.
pub trait Sockets: Send + Sync {
    /// A TCP listener at an address (its port 0: one the system picks).
    ///
    /// # Errors
    /// It cannot be listened at: taken, refused, or no network here.
    fn listen_tcp(&self, at: SocketAddr) -> io::Result<std::net::TcpListener>;

    /// Whether a socket may be used so at an address.
    fn allows(&self, at: SocketAddr, how: SocketUse) -> bool;
}

/// The system's: any address, as the system allows it.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemSockets;

#[allow(clippy::disallowed_methods, reason = "the system's sockets: what every other opens through")]
impl Sockets for SystemSockets {
    fn listen_tcp(&self, at: SocketAddr) -> io::Result<std::net::TcpListener> {
        std::net::TcpListener::bind(at)
    }

    fn allows(&self, _at: SocketAddr, _how: SocketUse) -> bool {
        true
    }
}

/// No network: nothing listened at, nothing allowed.
#[derive(Clone, Copy, Debug, Default)]
pub struct Closed;

impl Sockets for Closed {
    fn listen_tcp(&self, at: SocketAddr) -> io::Result<std::net::TcpListener> {
        Err(io::Error::new(io::ErrorKind::PermissionDenied, format!("{at}: this run reaches no network")))
    }

    fn allows(&self, _at: SocketAddr, _how: SocketUse) -> bool {
        false
    }
}
