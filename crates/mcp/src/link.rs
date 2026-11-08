//! The editor's link: a local socket through which a running editor answers the requests made of the project it has
//! open - a Unix domain socket at `.cache/editor.sock`, a named pipe on Windows. One JSON request a line, one JSON
//! reply a line. Only the process holding the project lock listens, so a socket left by an editor that crashed is
//! taken over, never shared.

use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{ListenerOptions, Name, Stream};
use std::io::{BufRead, BufReader, Write};
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

fn forward_to_holder(root: &Path, request: &Request, patience: Duration) -> Option<Reply> {
    let stream = Stream::connect(name(root).ok()?).ok()?;
    let line = serde_json::to_string(request).ok()?;
    // Read on a thread of its own, so an editor that does not answer is given up on.
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut stream = BufReader::new(stream);
        let result = (|| -> std::io::Result<String> {
            stream.get_mut().write_all(format!("{line}\n").as_bytes())?;
            let mut reply = String::new();
            stream.read_line(&mut reply)?;
            Ok(reply)
        })();
        let _ = send.send(result);
    });
    Some(match receive.recv_timeout(patience) {
        Ok(Ok(reply)) => serde_json::from_str(&reply)
            .unwrap_or_else(|e| Reply::Error(format!("the editor's reply could not be read: {e}"))),
        Ok(Err(e)) => Reply::Error(format!("the editor's link failed: {e}")),
        Err(_) => Reply::Error("the editor did not answer in time (is it minimized?)".into()),
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
        // Wake the accepting thread, which then ends.
        if let Ok(name) = name(&self.root) {
            let _ = Stream::connect(name);
        }
        // Gone once it has stopped answering, for whoever holds the project next to answer - unless the name was taken
        // over meanwhile, and the wake reached the newer listener: then it is left, no longer reached.
        let _ = self.ended.recv_timeout(Duration::from_secs(1));
    }
}

fn serve(stream: Stream, answer: &dyn Fn(Request) -> Reply) {
    let mut stream = BufReader::new(stream);
    let mut line = String::new();
    while matches!(stream.read_line(&mut line), Ok(n) if n > 0) {
        let reply = match serde_json::from_str::<Request>(&line) {
            Ok(request) => answer(request),
            Err(e) => Reply::Error(format!("not a request: {e}")),
        };
        let Ok(text) = serde_json::to_string(&reply) else { break };
        if stream.get_mut().write_all(format!("{text}\n").as_bytes()).is_err() {
            break;
        }
        line.clear();
    }
}
