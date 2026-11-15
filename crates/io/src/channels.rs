//! Channels between the processes of a machine: a name every process agrees on, listened at by one and connected to
//! by others, each connection a stream both ways. The system's are local sockets (`interprocess`): a socket file on
//! Unix, a named pipe on Windows. [`InMemory`] connects within a process, for a test.

use crate::HostPath;
use std::collections::{HashMap, VecDeque};
use std::io::{self, Read, Write};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, PoisonError};

/// Where a channel is found.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ChannelName {
    /// A socket file (Unix).
    File(HostPath),
    /// A name of the machine's (a pipe's, on Windows).
    Named(String),
}

/// A connection: what one side writes, the other reads.
pub trait Channel: Read + Write + Send {}

/// Listening at a name: each connection to it, accepted in turn.
pub trait ChannelListener: Send {
    /// Wait for the next connection.
    ///
    /// # Errors
    /// The connection failed as it was made, or the listener is gone.
    fn accept(&self) -> io::Result<Box<dyn Channel>>;
}

/// What makes channels.
pub trait Channels: Send + Sync {
    /// Listen at a name, taking it over from whoever listened there before - left by a process that ended - and
    /// leaving it as it is when dropped: by then it may be another's.
    ///
    /// # Errors
    /// It cannot be listened at.
    fn listen(&self, name: &ChannelName) -> io::Result<Box<dyn ChannelListener>>;

    /// Connect to what listens at a name.
    ///
    /// # Errors
    /// Nothing listens there.
    fn connect(&self, name: &ChannelName) -> io::Result<Box<dyn Channel>>;
}

/// The system's: local sockets.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemChannels;

impl Channel for interprocess::local_socket::Stream {}

struct SystemListener(interprocess::local_socket::Listener);

impl ChannelListener for SystemListener {
    fn accept(&self) -> io::Result<Box<dyn Channel>> {
        use interprocess::local_socket::traits::Listener as _;
        Ok(Box::new(self.0.accept()?))
    }
}

fn local(name: &ChannelName) -> io::Result<interprocess::local_socket::Name<'static>> {
    use interprocess::local_socket::{GenericFilePath, GenericNamespaced, ToFsName as _, ToNsName as _};
    match name {
        ChannelName::File(path) => path.clone().into_path_buf().to_fs_name::<GenericFilePath>(),
        ChannelName::Named(name) => name.clone().to_ns_name::<GenericNamespaced>(),
    }
}

impl Channels for SystemChannels {
    fn listen(&self, name: &ChannelName) -> io::Result<Box<dyn ChannelListener>> {
        let options = interprocess::local_socket::ListenerOptions::new().name(local(name)?);
        let listener = options.try_overwrite(true).reclaim_name(false).create_sync()?;
        Ok(Box::new(SystemListener(listener)))
    }

    fn connect(&self, name: &ChannelName) -> io::Result<Box<dyn Channel>> {
        use interprocess::local_socket::traits::Stream as _;
        Ok(Box::new(interprocess::local_socket::Stream::connect(local(name)?)?))
    }
}

/// Channels within a process: a name listened at is this one's alone, and a connection a pair of pipes.
#[derive(Clone, Default)]
pub struct InMemory {
    listening: Arc<Mutex<Listening>>,
}

/// Each name listened at, and where a connection to it is handed.
type Listening = HashMap<ChannelName, mpsc::Sender<Box<dyn Channel>>>;

impl std::fmt::Debug for InMemory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("channels within a process")
    }
}

/// Bytes on their way one way, and whether their writer is gone.
#[derive(Default)]
struct Pipe {
    held: Mutex<(VecDeque<u8>, bool)>,
    came: Condvar,
}

impl Pipe {
    fn close(&self) {
        self.held.lock().unwrap_or_else(PoisonError::into_inner).1 = true;
        self.came.notify_all();
    }
}

/// One end of a connection.
struct End {
    read: Arc<Pipe>,
    write: Arc<Pipe>,
}

impl Read for End {
    fn read(&mut self, into: &mut [u8]) -> io::Result<usize> {
        let mut held = self.read.held.lock().unwrap_or_else(PoisonError::into_inner);
        while held.0.is_empty() && !held.1 {
            held = self.read.came.wait(held).unwrap_or_else(PoisonError::into_inner);
        }
        let read = into.len().min(held.0.len());
        for (to, byte) in into.iter_mut().zip(held.0.drain(..read)) {
            *to = byte;
        }
        Ok(read)
    }
}

impl Write for End {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut held = self.write.held.lock().unwrap_or_else(PoisonError::into_inner);
        if held.1 {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        held.0.extend(bytes);
        self.write.came.notify_all();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for End {
    fn drop(&mut self) {
        // The other end reads what is left, then its end; and writes no more.
        self.write.close();
        self.read.close();
    }
}

impl Channel for End {}

struct Accepting(Mutex<mpsc::Receiver<Box<dyn Channel>>>);

impl ChannelListener for Accepting {
    fn accept(&self) -> io::Result<Box<dyn Channel>> {
        let receiver = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        receiver.recv().map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))
    }
}

impl Channels for InMemory {
    fn listen(&self, name: &ChannelName) -> io::Result<Box<dyn ChannelListener>> {
        let (send, receive) = mpsc::channel();
        self.listening.lock().unwrap_or_else(PoisonError::into_inner).insert(name.clone(), send);
        Ok(Box::new(Accepting(Mutex::new(receive))))
    }

    fn connect(&self, name: &ChannelName) -> io::Result<Box<dyn Channel>> {
        let (there, here) = (Arc::new(Pipe::default()), Arc::new(Pipe::default()));
        let theirs: Box<dyn Channel> = Box::new(End { read: there.clone(), write: here.clone() });
        let listening = self.listening.lock().unwrap_or_else(PoisonError::into_inner);
        let listener = listening.get(name).ok_or(io::ErrorKind::ConnectionRefused)?;
        listener.send(theirs).map_err(|_| io::Error::from(io::ErrorKind::ConnectionRefused))?;
        Ok(Box::new(End { read: here, write: there }))
    }
}

#[cfg(test)]
mod tests {
    use super::{ChannelName, Channels, InMemory};
    use std::io::{Read, Write};

    #[test]
    fn what_one_end_writes_the_other_reads_until_it_is_dropped() {
        let channels = InMemory::default();
        let name = ChannelName::Named("a test".into());
        assert!(channels.connect(&name).is_err(), "nothing listens yet");
        let listener = channels.listen(&name).unwrap();
        let mut here = channels.connect(&name).unwrap();
        let mut there = listener.accept().unwrap();
        here.write_all(b"asked").unwrap();
        let mut asked = [0; 5];
        there.read_exact(&mut asked).unwrap();
        assert_eq!(&asked, b"asked");
        there.write_all(b"answered").unwrap();
        drop(there);
        let mut answered = Vec::new();
        here.read_to_end(&mut answered).unwrap();
        assert_eq!(answered, b"answered", "what was written before it went is read, then its end");
        assert!(here.write_all(b"more").is_err(), "nobody reads it any more");
    }
}
