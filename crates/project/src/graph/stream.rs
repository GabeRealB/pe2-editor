//! Streams: an output that is a header and a queue of items, made as they are read. A stream operation is opened, not
//! made (`Engine::open_stream`): what it is given evaluated as any output's inputs, it starts a producer (an
//! extension's generator), whose header says what the stream is. The queue is filled on a thread of its own, a batch at
//! a time, while a reader is near its end: each reader keeps its own place (`Reader::get`), and the queue holds items,
//! in the batches they were made in (a batch shared by handle: `Item` is one of its items),
//! from a little behind the first reader to `AHEAD` past the last, dropping what all have passed. An item asked for far
//! from those held moves the queue there where the producer can go on from elsewhere (`Producer::seek`), else it is
//! read up to there.
//!
//! An item is one of three things to a reader: made (`Poll::Ready`), not yet - the queue fills toward it
//! (`Poll::Pending`) - or past the stream's end. A stream is identified by its trace, as what it read decides what it
//! makes: one whose reads no longer hold is opened again.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use anyhow::Result;
use toml::Table;

use super::Value;

/// How many items past the furthest reader the queue is filled to.
pub const AHEAD: u64 = 50;
/// How many items a producer is asked for at a time.
pub const BATCH: usize = 25;
/// How many it is asked for where it holds none - at its start, or gone on from elsewhere - so the first is read soon.
pub const FIRST: usize = 2;
/// How many items before the nearest reader are kept, for a reader stepping back.
pub const BEHIND: u64 = 10;

/// What makes a stream's items: an extension's generator, as the host drives it.
pub trait Producer: Send {
    /// What the stream is (`Generator::header`).
    fn header(&self) -> Table;
    /// Up to `count` items after the last: fewer, or none, at the end.
    ///
    /// # Errors
    /// They cannot be made.
    fn next(&mut self, count: usize) -> Result<Vec<Value>>;
    /// Go on from the item at `position`, where it can.
    ///
    /// # Errors
    /// It could not get there.
    fn seek(&mut self, position: u64) -> Result<bool>;
}

/// Items made together, from a position: held, and dropped, as one.
struct Batch {
    first: u64,
    items: Vec<Value>,
}

impl Batch {
    fn end(&self) -> u64 {
        self.first + self.items.len() as u64
    }
}

/// An item of a stream: a handle to its batch, and its place there.
#[derive(Clone)]
pub struct Item {
    batch: Arc<Batch>,
    index: usize,
}

impl std::ops::Deref for Item {
    type Target = Value;

    fn deref(&self) -> &Value {
        &self.batch.items[self.index]
    }
}

/// An item as a reader finds it.
#[derive(Clone)]
pub enum Poll {
    Ready(Item),
    /// Not made yet: the queue is filling toward it.
    Pending,
    /// Past the stream's end.
    End,
    Failed(String),
}

#[derive(Default)]
struct State {
    /// The position of the first item held, and the batches held from it.
    base: u64,
    batches: VecDeque<Arc<Batch>>,
    /// Its length, once the producer has come to its end.
    end: Option<u64>,
    /// Where it is to go on from.
    seek: Option<u64>,
    failed: Option<String>,
    /// Each reader's place.
    readers: HashMap<u64, u64>,
    next_reader: u64,
    closed: bool,
}

impl State {
    fn made_to(&self) -> u64 {
        self.batches.back().map_or(self.base, |b| b.end())
    }

    /// The item at a position, where it is held.
    fn item(&self, position: u64) -> Option<Item> {
        let at = self.batches.partition_point(|b| b.end() <= position);
        let batch = self.batches.get(at).filter(|b| b.first <= position)?;
        Some(Item { batch: batch.clone(), index: usize::try_from(position - batch.first).ok()? })
    }

    /// Whether the producer is to make more: a reader is within `AHEAD` of the end of what is made.
    fn wanted(&self) -> bool {
        let furthest = self.readers.values().max().copied();
        self.failed.is_none()
            && self.end.is_none()
            && furthest.is_some_and(|f| self.made_to() < f.saturating_add(AHEAD))
    }

    /// Drop what every reader has passed, but `BEHIND`.
    fn drop_behind(&mut self) {
        let Some(nearest) = self.readers.values().min().copied() else { return };
        while let Some(first) = self.batches.front()
            && first.end() + BEHIND <= nearest
        {
            self.base = first.end();
            self.batches.pop_front();
        }
    }
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    /// Told whenever items are made: a window to wake.
    made: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn tell(&self) {
        self.changed.notify_all();
        let made = self.made.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if let Some(made) = made {
            made();
        }
    }
}

/// A stream, open: its header, and its queue filled by its producer on a thread of its own while it is read.
pub struct Stream {
    header: Table,
    shared: Arc<Shared>,
}

impl Stream {
    /// Open a stream on what its producer makes.
    #[must_use]
    pub fn open(producer: Box<dyn Producer>) -> Arc<Stream> {
        let header = producer.header();
        let shared =
            Arc::new(Shared { state: Mutex::new(State::default()), changed: Condvar::new(), made: Mutex::new(None) });
        let filling = shared.clone();
        let _ = std::thread::Builder::new().name("stream".into()).spawn(move || fill(&filling, producer));
        Arc::new(Stream { header, shared })
    }

    #[must_use]
    pub fn header(&self) -> &Table {
        &self.header
    }

    /// A reader of it, at its start.
    #[must_use]
    pub fn reader(self: &Arc<Stream>) -> Reader {
        let mut state = self.shared.state();
        let id = state.next_reader;
        state.next_reader += 1;
        state.readers.insert(id, 0);
        drop(state);
        self.shared.tell();
        Reader { stream: self.clone(), id }
    }

    /// Call this whenever items are made (to wake a window).
    pub fn on_made(&self, made: Arc<dyn Fn() + Send + Sync>) {
        *self.shared.made.lock().unwrap_or_else(PoisonError::into_inner) = Some(made);
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.shared.state().closed = true;
        self.shared.changed.notify_all();
    }
}

/// A reader of a stream: its own place in it.
pub struct Reader {
    stream: Arc<Stream>,
    id: u64,
}

impl Reader {
    #[must_use]
    pub fn stream(&self) -> &Arc<Stream> {
        &self.stream
    }

    /// The item at `position`, the reader's place now: made, not yet, or past the end. Asked far from what is held,
    /// the queue moves there.
    #[must_use]
    pub fn get(&self, position: u64) -> Poll {
        let shared = &self.stream.shared;
        let mut state = shared.state();
        let moved = state.readers.insert(self.id, position) != Some(position);
        let poll = if let Some(failed) = &state.failed {
            Poll::Failed(failed.clone())
        } else if let Some(item) = state.item(position) {
            Poll::Ready(item)
        } else if state.end.is_some_and(|end| position >= end) {
            Poll::End
        } else {
            // Before what is held, or well past what is made: go on from there.
            if (position < state.base || position > state.made_to() + AHEAD) && state.seek != Some(position) {
                state.seek = Some(position);
            }
            Poll::Pending
        };
        drop(state);
        if moved {
            shared.changed.notify_all();
        }
        poll
    }
}

/// A stream as an operation takes it: its items in turn, from the start or where it is sent, waiting for each.
pub struct InTurn {
    reader: Reader,
    at: std::sync::atomic::AtomicU64,
}

impl InTurn {
    #[must_use]
    pub fn new(stream: &Arc<Stream>) -> InTurn {
        InTurn { reader: stream.reader(), at: std::sync::atomic::AtomicU64::new(0) }
    }
}

impl pe2_ext::StreamSource for InTurn {
    fn header(&self) -> Table {
        self.reader.stream.header.clone()
    }

    fn next(&self) -> Result<Option<Value>> {
        let at = self.at.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let shared = &self.reader.stream.shared;
        loop {
            match self.reader.get(at) {
                Poll::Ready(item) => return Ok(Some((*item).clone())),
                Poll::End => return Ok(None),
                Poll::Failed(e) => anyhow::bail!(e),
                Poll::Pending => {
                    // Woken by what is made. What was made since `get` let go of the state is seen here, under the
                    // lock the producer takes to add it, so no batch goes by unseen: one would cost the whole wait,
                    // and a stream of hundreds of thousands of items as many waits.
                    let state = shared.state();
                    let settled =
                        state.failed.is_some() || state.item(at).is_some() || state.end.is_some_and(|end| at >= end);
                    if !settled {
                        let _ = shared.changed.wait_timeout(state, std::time::Duration::from_millis(50));
                    }
                }
            }
        }
    }

    fn seek(&self, position: u64) {
        self.at.store(position, std::sync::atomic::Ordering::Relaxed);
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        self.stream.shared.state().readers.remove(&self.id);
        self.stream.shared.changed.notify_all();
    }
}

/// What the filling thread does next.
enum Job {
    Seek(u64),
    Next,
}

/// Fill a stream's queue while it is open: going on from where it is asked to, and making a batch whenever a reader is
/// near the end of what is made.
fn fill(shared: &Shared, mut producer: Box<dyn Producer>) {
    loop {
        let job = {
            let mut state = shared.state();
            loop {
                if state.closed {
                    return;
                }
                if let Some(position) = state.seek.take() {
                    break Job::Seek(position);
                }
                if state.wanted() {
                    break Job::Next;
                }
                state = shared.changed.wait(state).unwrap_or_else(PoisonError::into_inner);
            }
        };
        match job {
            Job::Seek(position) => {
                let moved = producer.seek(position);
                let mut state = shared.state();
                match moved {
                    Ok(true) => {
                        state.base = position;
                        state.batches.clear();
                        state.end = None;
                    }
                    // It cannot: it is read up to there, or - before what is held - not at all.
                    Ok(false) if position < state.base => {
                        state.failed = Some(format!("it cannot go back to item {position}"));
                    }
                    Ok(false) => {}
                    Err(e) => state.failed = Some(format!("{e:#}")),
                }
            }
            Job::Next => {
                let batch = if shared.state().batches.is_empty() { FIRST } else { BATCH };
                let made = producer.next(batch);
                let mut state = shared.state();
                match made {
                    Ok(items) => {
                        let short = items.len() < batch;
                        if !items.is_empty() {
                            let first = state.made_to();
                            state.batches.push_back(Arc::new(Batch { first, items }));
                        }
                        if short {
                            state.end = Some(state.made_to());
                        }
                        state.drop_behind();
                    }
                    Err(e) => state.failed = Some(format!("{e:#}")),
                }
            }
        }
        shared.tell();
    }
}

#[cfg(test)]
mod tests {
    use super::{AHEAD, BATCH, Poll, Producer, Stream};
    use crate::graph::Value;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Numbers from where it is, as tables, up to a length; seeking where asked.
    struct Counting {
        at: u64,
        len: u64,
        made: Arc<AtomicUsize>,
    }

    impl Producer for Counting {
        fn header(&self) -> toml::Table {
            toml::from_str(&format!("count = {}", self.len)).unwrap()
        }

        fn next(&mut self, count: usize) -> anyhow::Result<Vec<Value>> {
            let to = (self.at + count as u64).min(self.len);
            let items = (self.at..to).map(|n| Value::Table(toml::from_str(&format!("n = {n}")).unwrap())).collect();
            self.made.fetch_add(usize::try_from(to - self.at).unwrap(), Ordering::Relaxed);
            self.at = to;
            Ok(items)
        }

        fn seek(&mut self, position: u64) -> anyhow::Result<bool> {
            self.at = position;
            Ok(true)
        }
    }

    fn wait(reader: &super::Reader, position: u64) -> Poll {
        for _ in 0..2000 {
            match reader.get(position) {
                Poll::Pending => std::thread::sleep(std::time::Duration::from_millis(1)),
                other => return other,
            }
        }
        Poll::Pending
    }

    fn number(poll: &Poll) -> i64 {
        match poll {
            Poll::Ready(value) => match &**value {
                Value::Table(t) => t["n"].as_integer().unwrap(),
                _ => panic!("not a table"),
            },
            _ => panic!("not made"),
        }
    }

    #[test]
    fn a_stream_fills_ahead_of_its_reader_and_moves_where_asked() {
        let made = Arc::new(AtomicUsize::new(0));
        let stream = Stream::open(Box::new(Counting { at: 0, len: 1000, made: made.clone() }));
        assert_eq!(stream.header()["count"].as_integer(), Some(1000));
        // Nothing is made before it is read.
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert_eq!(made.load(Ordering::Relaxed), 0);
        let reader = stream.reader();
        assert_eq!(number(&wait(&reader, 0)), 0);
        // Filled to AHEAD past the reader, a batch at a time - no further.
        std::thread::sleep(std::time::Duration::from_millis(20));
        let ahead = made.load(Ordering::Relaxed) as u64;
        assert!((AHEAD..AHEAD + BATCH as u64).contains(&ahead), "{ahead}");
        assert_eq!(number(&wait(&reader, 30)), 30);
        // Far ahead: it goes on from there rather than making what is between.
        assert_eq!(number(&wait(&reader, 900)), 900);
        assert!((made.load(Ordering::Relaxed) as u64) < 900);
        // And back.
        assert_eq!(number(&wait(&reader, 5)), 5);
        assert!(matches!(wait(&reader, 1000), Poll::End));
    }
}
