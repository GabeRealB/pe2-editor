//! The editor's link: a local socket through which a running editor answers the requests made of the project it has
//! open - a Unix domain socket at `.cache/editor.sock`, a named pipe on Windows. One CBOR request a frame, one CBOR reply
//! a frame (`project::wire`). Only the process holding the project lock listens, so a socket left by an editor that crashed is
//! taken over, never shared.

use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{ListenerOptions, Name, Stream};
use std::io::BufReader;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::{Reply, Request};

/// How long a request waits for the editor's answer: the editor answers between frames.
const PATIENCE: Duration = Duration::from_secs(20);

fn name(root: &Path) -> std::io::Result<Name<'static>> {
    project::host::local_name(root, project::cache::SOCKET, "pe2-editor")
}

/// Whether anything listens on the holder's link of the project in `root`: an editor or a host that answers.
#[must_use]
pub fn listens(root: &Path) -> bool {
    name(root).ok().and_then(|name| Stream::connect(name).ok()).is_some()
}

/// Hand a request to the editor serving the project in `root`; none when no editor is.
#[must_use]
pub fn forward(root: &Path, request: &Request) -> Option<Reply> {
    forward_within(root, request, PATIENCE)
}

/// Hand a request to the editor serving the project in `root`, waiting this long for its answer; none when no editor is,
/// or the holder declines it (the host answers edits alone).
#[must_use]
pub fn forward_within(root: &Path, request: &Request, patience: Duration) -> Option<Reply> {
    match forward_to_holder(root, request, patience)? {
        Reply::Declined => None,
        reply => Some(reply),
    }
}

/// How long a link that closed without answering is asked again: its holder stopping, the link is gone in a moment.
const LEAVING: Duration = Duration::from_secs(2);

/// Asked of a link: answered, or closed with no answer - by a holder that was stopping, and took the connection as
/// its last act. Such a one has done nothing of what was asked.
enum Asked {
    Answered(Reply),
    Closed,
    Failed(String),
}

/// Ask a link again while it closes without answering, a short while: none once nothing listens there any more -
/// whoever asked goes on as where nothing holds the project - an error where it goes on closing.
fn until_answered(root: &Path, whose: &str, mut ask: impl FnMut() -> Option<Asked>) -> Option<Reply> {
    let asked = std::time::Instant::now();
    loop {
        match ask()? {
            Asked::Answered(Reply::Declined) => return None,
            Asked::Answered(reply) => return Some(reply),
            Asked::Failed(why) => return Some(Reply::Error(why)),
            Asked::Closed if asked.elapsed() < LEAVING => std::thread::sleep(Duration::from_millis(20)),
            Asked::Closed if !listens(root) => return None,
            Asked::Closed => return Some(Reply::Error(format!("{whose} closed its link without answering"))),
        }
    }
}

/// Hand a request to the holder of the project in `root` and wait for its answer for as long as the link lives: for
/// what must not be asked twice - an edit given up on while its holder is still making it would be made again. None
/// where nothing listens, or the holder declines it.
#[must_use]
pub fn forward_patiently(root: &Path, request: &Request) -> Option<Reply> {
    until_answered(root, "the holder", || {
        let mut stream = BufReader::new(Stream::connect(name(root).ok()?).ok()?);
        let answered = (|| -> std::io::Result<Option<Reply>> {
            project::wire::send(stream.get_mut(), request)?;
            project::wire::receive(&mut stream)
        })();
        Some(match answered {
            Ok(Some(reply)) => Asked::Answered(reply),
            Ok(None) => Asked::Closed,
            Err(e) => Asked::Failed(format!("the holder's link failed: {e}")),
        })
    })
}

fn forward_to_holder(root: &Path, request: &Request, patience: Duration) -> Option<Reply> {
    until_answered(root, "the editor", || {
        let stream = Stream::connect(name(root).ok()?).ok()?;
        let request = request.clone();
        // Read on a thread of its own, so an editor that does not answer is given up on.
        let (send, receive) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut stream = BufReader::new(stream);
            let result = (|| -> std::io::Result<Option<Reply>> {
                project::wire::send(stream.get_mut(), &request)?;
                project::wire::receive(&mut stream)
            })();
            let _ = send.send(result);
        });
        Some(match receive.recv_timeout(patience) {
            Ok(Ok(Some(reply))) => Asked::Answered(reply),
            Ok(Ok(None)) => Asked::Closed,
            Ok(Err(e)) => Asked::Failed(format!("the editor's link failed: {e}")),
            Err(_) => Asked::Failed("the editor did not answer in time (is it minimized?)".into()),
        })
    })
}

/// The editor's side: listening while it lives, each request answered by `answer`.
pub struct Listener {
    stop: Arc<AtomicBool>,
    root: std::path::PathBuf,
    /// Told when the accepting thread ends.
    ended: std::sync::mpsc::Receiver<()>,
}

impl Listener {
    /// Listen for the project in `root`, which this process holds; `answer` is called on a link thread.
    ///
    /// # Errors
    /// The socket cannot be made.
    pub fn start(root: &Path, answer: impl Fn(Request) -> Reply + Send + Sync + 'static) -> std::io::Result<Listener> {
        // The name is not removed when this ends: by then it may be a newer holder's, which took it over.
        let listener =
            ListenerOptions::new().name(name(root)?).try_overwrite(true).reclaim_name(false).create_sync()?;
        let (end, ended) = std::sync::mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let answer = Arc::new(answer);
        let stopped = stop.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stopped.load(Ordering::Relaxed) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let answer = answer.clone();
                std::thread::spawn(move || serve(stream, &*answer));
            }
            drop(listener);
            let _ = end.send(());
        });
        Ok(Listener { stop, root: root.to_path_buf(), ended })
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Wake the accepting thread, which then ends - from a thread of its own: where that one has ended already (a
        // request reached it since it was told to stop), nobody answers the name any more, and on Windows connecting
        // to it then waits for as long as a connection of it is still open, which would be this never returning.
        if let Ok(name) = name(&self.root) {
            std::thread::spawn(move || drop(Stream::connect(name)));
        }
        // Gone once it has stopped answering, for whoever holds the project next to answer - unless the name was taken
        // over meanwhile, and the wake reached the newer listener: then it is left, no longer reached.
        let _ = self.ended.recv_timeout(Duration::from_secs(1));
    }
}

fn serve(stream: Stream, answer: &dyn Fn(Request) -> Reply) {
    let mut stream = BufReader::new(stream);
    loop {
        let reply = match project::wire::receive::<Request>(&mut stream) {
            Ok(Some(request)) => answer(request),
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => Reply::Error(format!("not a request: {e}")),
            Ok(None) | Err(_) => break,
        };
        if project::wire::send(stream.get_mut(), &reply).is_err() {
            break;
        }
    }
}
