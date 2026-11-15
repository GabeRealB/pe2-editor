//! Time, shaped like `wasi:clocks`: a monotonic clock and the wall clock, and - what the host's threads need beside
//! them - waiting for a moment by that clock, or until woken.

#![allow(
    clippy::disallowed_methods,
    reason = "the system's clocks, and the waits on them: what every other reads through"
)]

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime};

/// What tells the time, and waits for it.
pub trait Clock: Send + Sync {
    /// The time now, on a clock that only goes forward: what durations and moments to wait for are measured on.
    fn now(&self) -> Instant;

    /// The time of day: what is written into data (a file's modification time) and compared with what is.
    fn wall(&self) -> SystemTime;

    /// Something to wait on by this clock: until a moment comes, or until it is woken.
    fn waiter(&self) -> Arc<dyn Waiter>;
}

/// A wait by a clock, which another thread may cut short.
pub trait Waiter: Send + Sync {
    /// Wait until woken (true), or until `until` comes by the clock (false); with none, until woken. A wake given while
    /// nothing waits is kept for the next wait, which returns at once.
    fn wait_until(&self, until: Option<Instant>) -> bool;

    /// Wake whatever waits, or the next wait.
    fn wake(&self);
}

/// Wait out a duration, by a clock.
pub fn sleep(clock: &dyn Clock, duration: Duration) {
    let until = clock.now() + duration;
    let waiter = clock.waiter();
    while clock.now() < until {
        waiter.wait_until(Some(until));
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The system's clocks.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn wall(&self) -> SystemTime {
        SystemTime::now()
    }

    fn waiter(&self) -> Arc<dyn Waiter> {
        Arc::new(SystemWaiter::default())
    }
}

/// A wait on the system's clock: a condition woken, waited on for as long as is left.
#[derive(Default)]
struct SystemWaiter {
    woken: Mutex<bool>,
    told: Condvar,
}

impl Waiter for SystemWaiter {
    fn wait_until(&self, until: Option<Instant>) -> bool {
        let mut woken = lock(&self.woken);
        loop {
            if *woken {
                *woken = false;
                return true;
            }
            match until {
                None => woken = self.told.wait(woken).unwrap_or_else(PoisonError::into_inner),
                Some(until) => {
                    let now = Instant::now();
                    if now >= until {
                        return false;
                    }
                    woken = self.told.wait_timeout(woken, until - now).unwrap_or_else(PoisonError::into_inner).0;
                }
            }
        }
    }

    fn wake(&self) {
        *lock(&self.woken) = true;
        self.told.notify_all();
    }
}

/// A clock that stands until it is moved ([`Standing::advance`]): what a test drives, to say how much time has passed
/// rather than waiting for it. What waits by it waits until it is moved past the moment waited for, or woken - never on
/// the system's time.
#[derive(Clone)]
pub struct Standing(Arc<Stands>);

/// Where a standing clock is, and what tells its waits it moved.
struct Stands {
    at: Mutex<(Instant, SystemTime)>,
    moved: Condvar,
    /// Whether the time of day stands too, or is the system's.
    standing_wall: bool,
}

impl Standing {
    /// One standing at the system's time now.
    #[must_use]
    pub fn new() -> Standing {
        Standing::at(SystemTime::now())
    }

    /// One standing at this time of day.
    #[must_use]
    pub fn at(wall: SystemTime) -> Standing {
        Standing(Arc::new(Stands {
            at: Mutex::new((Instant::now(), wall)),
            moved: Condvar::new(),
            standing_wall: true,
        }))
    }

    /// One whose monotonic clock stands - what durations and moments waited for are measured on - while the time of
    /// day is the system's: for what decides by durations, among what tells files apart by the times the system gives
    /// them as they are written (a time that stood would give two writes one time).
    #[must_use]
    pub fn monotonic() -> Standing {
        let at = Mutex::new((Instant::now(), SystemTime::now()));
        Standing(Arc::new(Stands { at, moved: Condvar::new(), standing_wall: false }))
    }

    /// Moved on by this much: what waits for a moment it reaches returns.
    pub fn advance(&self, by: Duration) {
        let mut at = lock(&self.0.at);
        at.0 += by;
        at.1 += by;
        drop(at);
        self.0.moved.notify_all();
    }
}

impl Default for Standing {
    fn default() -> Standing {
        Standing::new()
    }
}

impl Clock for Standing {
    fn now(&self) -> Instant {
        lock(&self.0.at).0
    }

    fn wall(&self) -> SystemTime {
        if self.0.standing_wall { lock(&self.0.at).1 } else { SystemTime::now() }
    }

    fn waiter(&self) -> Arc<dyn Waiter> {
        Arc::new(StandingWaiter { clock: self.0.clone(), woken: Mutex::new(false) })
    }
}

/// A wait by a standing clock: woken, or the clock moved past its moment.
struct StandingWaiter {
    clock: Arc<Stands>,
    woken: Mutex<bool>,
}

impl Waiter for StandingWaiter {
    fn wait_until(&self, until: Option<Instant>) -> bool {
        let mut at = lock(&self.clock.at);
        loop {
            // Under the clock's lock, which a wake takes before it tells: none is lost between looking and waiting.
            let mut woken = lock(&self.woken);
            if *woken {
                *woken = false;
                return true;
            }
            drop(woken);
            if until.is_some_and(|until| at.0 >= until) {
                return false;
            }
            at = self.clock.moved.wait(at).unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn wake(&self) {
        let at = lock(&self.clock.at);
        *lock(&self.woken) = true;
        drop(at);
        self.clock.moved.notify_all();
    }
}

/// A condition several threads wait for, by a clock: what a `Condvar` is beside a lock, its waits timed by the clock
/// rather than the system's. A wait registers a waiter before it looks at the condition, so that a notice given while
/// it looks is not lost; `notify_all` wakes every wait registered.
pub struct Signal {
    io: Arc<dyn crate::Io>,
    waiting: Mutex<Vec<std::sync::Weak<dyn Waiter>>>,
}

impl Signal {
    /// One timed by the clock of `io`.
    #[must_use]
    pub fn new(io: Arc<dyn crate::Io>) -> Signal {
        Signal { io, waiting: Mutex::default() }
    }

    /// Wait until `done` holds of what `state` guards (true), or until `until` comes by the clock (false); with none,
    /// until it holds. The lock is held while `done` looks, and given back held.
    pub fn wait_until<'a, T>(
        &self,
        state: &'a Mutex<T>,
        until: Option<Instant>,
        mut done: impl FnMut(&mut T) -> bool,
    ) -> (MutexGuard<'a, T>, bool) {
        let waiter = self.io.clock().waiter();
        lock(&self.waiting).push(Arc::downgrade(&waiter));
        let answer = loop {
            let mut guard = lock(state);
            if done(&mut guard) {
                break (guard, true);
            }
            if until.is_some_and(|until| self.io.clock().now() >= until) {
                break (guard, false);
            }
            drop(guard);
            waiter.wait_until(until);
        };
        lock(&self.waiting)
            .retain(|waiting| waiting.strong_count() > 0 && !std::ptr::addr_eq(waiting.as_ptr(), Arc::as_ptr(&waiter)));
        answer
    }

    /// Wake every wait, each to look at its condition again.
    pub fn notify_all(&self) {
        for waiting in lock(&self.waiting).iter().filter_map(std::sync::Weak::upgrade) {
            waiting.wake();
        }
    }
}

/// A channel whose receiver waits by a clock: what `std::sync::mpsc` is, its timed receive (`recv_until`) timed by
/// the clock of `io` rather than the system's. A send wakes the receiver, and so does a sender let go of, so that it
/// sees the channel closed once all are.
#[must_use]
pub fn channel<T>(io: Arc<dyn crate::Io>) -> (Sender<T>, Receiver<T>) {
    let (sent, received) = std::sync::mpsc::channel();
    let waiter = io.clock().waiter();
    (Sender { sent: Some(sent), waiter: waiter.clone() }, Receiver { received, waiter, io })
}

/// What sends on a [`channel`].
pub struct Sender<T> {
    /// None only as it is let go of: dropped before the receiver is woken.
    sent: Option<std::sync::mpsc::Sender<T>>,
    waiter: Arc<dyn Waiter>,
}

impl<T> Sender<T> {
    /// Send a value, waking the receiver.
    ///
    /// # Errors
    /// The receiver is gone: the value is given back.
    pub fn send(&self, value: T) -> Result<(), std::sync::mpsc::SendError<T>> {
        match &self.sent {
            Some(sent) => sent.send(value)?,
            None => return Err(std::sync::mpsc::SendError(value)),
        }
        self.waiter.wake();
        Ok(())
    }
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Sender<T> {
        Sender { sent: self.sent.clone(), waiter: self.waiter.clone() }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        drop(self.sent.take());
        self.waiter.wake();
    }
}

/// What receives on a [`channel`].
pub struct Receiver<T> {
    received: std::sync::mpsc::Receiver<T>,
    waiter: Arc<dyn Waiter>,
    io: Arc<dyn crate::Io>,
}

impl<T> Receiver<T> {
    /// The next value, waiting for it until `until` comes by the clock; with none, for as long as it takes.
    ///
    /// # Errors
    /// The moment came first (`Timeout`), or every sender is gone (`Disconnected`).
    pub fn recv_until(&self, until: Option<Instant>) -> Result<T, std::sync::mpsc::RecvTimeoutError> {
        use std::sync::mpsc::{RecvTimeoutError, TryRecvError};
        loop {
            match self.received.try_recv() {
                Ok(value) => return Ok(value),
                Err(TryRecvError::Disconnected) => return Err(RecvTimeoutError::Disconnected),
                Err(TryRecvError::Empty) => {}
            }
            if until.is_some_and(|until| self.io.clock().now() >= until) {
                return Err(RecvTimeoutError::Timeout);
            }
            self.waiter.wait_until(until);
        }
    }

    /// The next value, waiting for it for as long as it takes.
    ///
    /// # Errors
    /// Every sender is gone.
    pub fn recv(&self) -> Result<T, std::sync::mpsc::RecvError> {
        self.recv_until(None).map_err(|_| std::sync::mpsc::RecvError)
    }

    /// A value that is there, without waiting.
    ///
    /// # Errors
    /// None is there, or every sender is gone.
    pub fn try_recv(&self) -> Result<T, std::sync::mpsc::TryRecvError> {
        self.received.try_recv()
    }

    /// The values that are there, without waiting.
    #[must_use]
    pub fn try_iter(&self) -> std::sync::mpsc::TryIter<'_, T> {
        self.received.try_iter()
    }
}

#[cfg(test)]
#[allow(
    clippy::disallowed_methods,
    reason = "a test's fixtures: the machine's time, places, files and environment, which nothing tested reads"
)]
mod tests {
    use super::{Clock, Signal, Standing, SystemClock};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[test]
    fn a_standing_clock_moves_only_when_moved_and_its_waits_with_it() {
        let clock = Standing::new();
        let (then, wall) = (clock.now(), clock.wall());
        assert_eq!(clock.now(), then, "it stands");
        let waiter = clock.waiter();
        let until = then + Duration::from_secs(60);
        let waiting = {
            let waiter = waiter.clone();
            std::thread::spawn(move || waiter.wait_until(Some(until)))
        };
        clock.advance(Duration::from_secs(30));
        clock.advance(Duration::from_secs(30));
        assert!(!waiting.join().unwrap(), "the moment came, by the clock");
        assert_eq!((clock.now() - then, clock.wall().duration_since(wall).unwrap()), (until - then, until - then));
        // Woken, whether it waits yet or not.
        waiter.wake();
        assert!(waiter.wait_until(None), "a wake given before is kept for the wait");
        let waiting = {
            let waiter = waiter.clone();
            std::thread::spawn(move || waiter.wait_until(None))
        };
        waiter.wake();
        assert!(waiting.join().unwrap(), "woken");
    }

    #[test]
    fn a_signal_wakes_every_wait_and_times_each_by_its_clock() {
        let clock = Standing::new();
        let io = crate::Composed { clock: Arc::new(clock.clone()), ..crate::Composed::system() };
        let signal = Arc::new(Signal::new(Arc::new(io)));
        let state = Arc::new(Mutex::new(0));
        let waits: Vec<_> = (0..3)
            .map(|_| {
                let (signal, state) = (signal.clone(), state.clone());
                std::thread::spawn(move || signal.wait_until(&state, None, |n| *n == 1).1)
            })
            .collect();
        *state.lock().unwrap() = 1;
        signal.notify_all();
        assert!(waits.into_iter().all(|wait| wait.join().unwrap()), "each saw it hold");
        let timed = {
            let (signal, state, until) = (signal.clone(), state.clone(), clock.now() + Duration::from_secs(5));
            std::thread::spawn(move || signal.wait_until(&state, Some(until), |n| *n == 2).1)
        };
        clock.advance(Duration::from_secs(5));
        assert!(!timed.join().unwrap(), "the moment came first, by the clock");
    }

    #[test]
    fn a_channel_waits_by_its_clock_and_sees_its_senders_go() {
        let clock = Standing::new();
        let io: Arc<dyn crate::Io> =
            Arc::new(crate::Composed { clock: Arc::new(clock.clone()), ..crate::Composed::system() });
        let (sender, receiver) = super::channel::<u32>(io);
        let until = clock.now() + Duration::from_secs(1);
        let waiting = std::thread::spawn(move || {
            let timed = receiver.recv_until(Some(until));
            (timed, receiver)
        });
        clock.advance(Duration::from_secs(1));
        let (timed, receiver) = waiting.join().unwrap();
        assert_eq!(timed, Err(std::sync::mpsc::RecvTimeoutError::Timeout), "the moment came, by the clock");
        let waiting = std::thread::spawn(move || (receiver.recv(), receiver.recv()));
        sender.send(7).unwrap();
        drop(sender);
        let (first, then) = waiting.join().unwrap();
        assert_eq!((first, then), (Ok(7), Err(std::sync::mpsc::RecvError)), "sent, then its sender gone");
    }

    #[test]
    fn the_system_clocks_wait_for_a_moment_passed_at_once() {
        let clock = SystemClock;
        assert!(!clock.waiter().wait_until(Some(clock.now())), "the moment has come");
        let waiter = clock.waiter();
        waiter.wake();
        assert!(waiter.wait_until(None));
    }
}
