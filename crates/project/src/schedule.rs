//! Who the host's work is for, and what runs next: one pool of threads for all of it, shared fairly between groups.
//!
//! Work enters the host from a few places - a view asking for an output, a command the user ran, what the editor does on
//! its own - and each makes a group (`Group`) of its class (`Class`): everything done for it, on any thread, is the group's,
//! as what it spawns inherits it (`parallel`, which runs under the group of whatever called it). Nothing inside an operation
//! says anything of priority.
//!
//! Groups share the pool as Linux's scheduler shares a CPU between task groups. Each has a virtual runtime: the time its
//! work took, divided by its class's weight. A worker takes the next unit from the group with the least of it, so a group
//! that asks for little - a view's preview - runs ahead of one that asks for much - the attaching of a project's imports -
//! without either saying so, and a heavier class gets more of the pool for the same time. A group coming back from idle
//! starts near the least virtual runtime of those waiting, not below it: it is not owed what it did not ask for.
//!
//! `parallel` is a scope over the pool: its caller works through its items with helpers queued under its group, then waits
//! for the helpers that started, running other queued work meanwhile, so that nested scopes never hold every thread.

use std::any::Any;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

/// Who work is for: what weighs it against the rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// What the user is looking at: a view's output, the selected asset.
    Interactive,
    /// What the user asked for: a build, an extraction, a signal.
    Commanded,
    /// What the host does on its own: opening, indexing, attaching imports, thumbnails, sweeps.
    Background,
}

impl Class {
    /// Its share of the pool against the others, as Linux weighs its nice levels: each step about a third more.
    fn weight(self) -> u64 {
        match self {
            Class::Interactive => 9548,
            Class::Commanded => 3121,
            Class::Background => 1024,
        }
    }
}

/// What a group returning from idle may start below the least virtual runtime of those waiting: a short head start, so
/// that what was just asked for runs soon, but no credit for the time it asked for nothing.
const RETURNING: Duration = Duration::from_millis(6);

/// A group of work: everything done for one origin, its time charged together.
#[derive(Clone)]
pub struct Group(Arc<State>);

struct State {
    class: Class,
    name: String,
    /// The time its work took, in nanoseconds, scaled by `1024 / weight`.
    virtual_runtime: AtomicU64,
}

impl Group {
    /// A group of a class, starting level with those waiting.
    #[must_use]
    pub fn new(class: Class, name: &str) -> Group {
        let start = pool().least();
        Group(Arc::new(State { class, name: name.to_string(), virtual_runtime: AtomicU64::new(start) }))
    }

    /// Do this as the group's: what it runs here and what it spawns is charged to it. With `PE2_SCHEDULE=fifo`, groups
    /// count for nothing - all work one group, the pool first come first served - to compare what they change.
    pub fn enter<R>(&self, work: impl FnOnce() -> R) -> R {
        if unweighed() {
            return work();
        }
        let before = CURRENT.with(|current| current.replace(Some(self.clone())));
        let done = work();
        CURRENT.with(|current| *current.borrow_mut() = before);
        done
    }

    #[must_use]
    pub fn class(&self) -> Class {
        self.0.class
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.0.name
    }

    /// Charge it for time its work took.
    fn charge(&self, took: Duration) {
        let scaled = u64::try_from(took.as_nanos()).unwrap_or(u64::MAX) * 1024 / self.0.class.weight();
        self.0.virtual_runtime.fetch_add(scaled, Ordering::Relaxed);
    }

    fn virtual_runtime(&self) -> u64 {
        self.0.virtual_runtime.load(Ordering::Relaxed)
    }

    fn same(&self, other: &Group) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Whether groups are set aside (`PE2_SCHEDULE=fifo`), to compare what they change.
fn unweighed() -> bool {
    static FIFO: OnceLock<bool> = OnceLock::new();
    *FIFO.get_or_init(|| std::env::var("PE2_SCHEDULE").is_ok_and(|mode| mode == "fifo"))
}

thread_local! {
    /// The group what runs on this thread is for.
    static CURRENT: RefCell<Option<Group>> = const { RefCell::new(None) };
    /// How deep this thread is in running others' work while it waits: bounded, as each is a frame of its stack.
    static HELPING: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The group what runs here is for: the one entered, else the host's own background.
#[must_use]
pub fn current() -> Group {
    CURRENT.with(|current| current.borrow().clone()).unwrap_or_else(|| {
        static OWN: OnceLock<Group> = OnceLock::new();
        OWN.get_or_init(|| Group::new(Class::Background, "host")).clone()
    })
}

/// A unit of work waiting in the pool.
type Unit = Box<dyn FnOnce() + Send>;

/// The pool: its threads, and the units waiting, by group.
struct Pool {
    queues: Mutex<Queues>,
    ready: Condvar,
    threads: usize,
}

#[derive(Default)]
struct Queues {
    /// Each group with units waiting, and them in order.
    waiting: Vec<(Group, VecDeque<Unit>)>,
    /// The least virtual runtime seen of a group waiting: where a group returning from idle starts.
    least: u64,
}

impl Queues {
    /// Queue a unit for a group: one returning from idle brought up near the least.
    fn push(&mut self, group: &Group, unit: Unit) {
        if let Some((_, units)) = self.waiting.iter_mut().find(|(waiting, _)| waiting.same(group)) {
            units.push_back(unit);
            return;
        }
        let floor = self.least.saturating_sub(u64::try_from(RETURNING.as_nanos()).unwrap_or(0));
        group.0.virtual_runtime.fetch_max(floor, Ordering::Relaxed);
        self.waiting.push((group.clone(), VecDeque::from([unit])));
    }

    /// The next unit of the group owed the pool most before this one, where one is waiting.
    fn pop_owed_before(&mut self, group: &Group) -> Option<Unit> {
        let own = group.virtual_runtime();
        let owed = self.waiting.iter().enumerate().filter(|(_, (waiting, _))| !waiting.same(group));
        let (at, _) = owed
            .filter(|(_, (waiting, _))| waiting.virtual_runtime() < own)
            .min_by_key(|(_, (waiting, _))| waiting.virtual_runtime())?;
        let unit = self.waiting[at].1.pop_front();
        if self.waiting[at].1.is_empty() {
            self.waiting.swap_remove(at);
        }
        unit
    }

    /// The next unit: of the group waiting with the least virtual runtime.
    fn pop(&mut self) -> Option<Unit> {
        let (at, _) = self.waiting.iter().enumerate().min_by_key(|(_, (group, _))| group.virtual_runtime())?;
        self.least = self.least.max(self.waiting[at].0.virtual_runtime());
        let unit = self.waiting[at].1.pop_front();
        if self.waiting[at].1.is_empty() {
            self.waiting.swap_remove(at);
        }
        unit
    }
}

impl Pool {
    fn lock(&self) -> std::sync::MutexGuard<'_, Queues> {
        self.queues.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn push(&self, group: &Group, unit: Unit) {
        self.lock().push(group, unit);
        self.ready.notify_one();
    }

    /// The next unit of a group, where one is waiting: what a caller waiting on it may run itself.
    fn try_pop_of(&self, group: &Group) -> Option<Unit> {
        let mut queues = self.lock();
        let at = queues.waiting.iter().position(|(waiting, _)| waiting.same(group))?;
        let unit = queues.waiting[at].1.pop_front();
        if queues.waiting[at].1.is_empty() {
            queues.waiting.swap_remove(at);
        }
        unit
    }

    /// Where a group starts: level with those waiting.
    fn least(&self) -> u64 {
        self.lock().least
    }

    fn pop_owed_before(&self, group: &Group) -> Option<Unit> {
        self.lock().pop_owed_before(group)
    }

    /// Whether another group is waiting that is owed the pool before this one: one with less virtual runtime.
    fn owed_before(&self, group: &Group) -> bool {
        let own = group.virtual_runtime();
        self.lock().waiting.iter().any(|(waiting, _)| !waiting.same(group) && waiting.virtual_runtime() < own)
    }
}

/// How many threads the pool has: every processor but one, kept for the window and what it does (`PE2_JOBS`, where set).
#[must_use]
pub fn threads() -> usize {
    pool().threads
}

fn threads_wanted() -> usize {
    let cores = std::thread::available_parallelism().map_or(2, std::num::NonZero::get);
    std::env::var("PE2_JOBS").ok().and_then(|jobs| jobs.parse().ok()).unwrap_or(cores.saturating_sub(1)).max(1)
}

fn pool() -> &'static Pool {
    static POOL: OnceLock<&'static Pool> = OnceLock::new();
    POOL.get_or_init(|| {
        let pool: &'static Pool =
            Box::leak(Box::new(Pool { queues: Mutex::default(), ready: Condvar::new(), threads: threads_wanted() }));
        for at in 0..pool.threads {
            let _ = std::thread::Builder::new().name(format!("pool {at}")).spawn(move || {
                loop {
                    let unit = {
                        let mut queues = pool.lock();
                        loop {
                            if let Some(unit) = queues.pop() {
                                break unit;
                            }
                            queues = pool.ready.wait(queues).unwrap_or_else(PoisonError::into_inner);
                        }
                    };
                    unit();
                }
            });
        }
        pool
    })
}

thread_local! {
    /// What the chain of steps running on this thread holds between them: each extension's instance, by a key.
    static CHAIN: RefCell<Option<std::collections::HashMap<String, Box<dyn Any>>>> = const { RefCell::new(None) };
}

/// Run a chain of steps (`pe2_ext::stepped`) on this thread: what one step's run leaves for the next (`keep_held`) is
/// there for it (`take_held`), so that a step whose reads are made goes on in place - an extension's instance not made
/// again - and dropped when the chain ends.
pub fn chain<R>(work: impl FnOnce() -> R) -> R {
    let before = CHAIN.with(|chain| chain.replace(Some(std::collections::HashMap::new())));
    let done = work();
    let held = CHAIN.with(|chain| chain.replace(before));
    drop(held);
    done
}

/// Work outside the chain running here - what is made between its steps, what runs at a yield: it neither takes what the
/// chain holds nor leaves anything for it.
pub fn apart<R>(work: impl FnOnce() -> R) -> R {
    let before = CHAIN.with(std::cell::RefCell::take);
    let done = work();
    CHAIN.with(|chain| *chain.borrow_mut() = before);
    done
}

/// Whether a chain of steps runs here: what a step leaves for the next (`keep_held`) is kept.
#[must_use]
pub fn chained() -> bool {
    CHAIN.with(|chain| chain.borrow().is_some())
}

/// What the chain running here holds by this key, taken from it.
#[must_use]
pub fn take_held<T: 'static>(key: &str) -> Option<T> {
    CHAIN.with(|chain| {
        let held = chain.borrow_mut().as_mut()?.remove(key)?;
        held.downcast::<T>().ok().map(|held| *held)
    })
}

/// Leave this for the chain's next step, where a chain runs here: false, and dropped, where none does.
pub fn keep_held<T: 'static>(key: &str, value: T) -> bool {
    CHAIN.with(|chain| match chain.borrow_mut().as_mut() {
        Some(held) => {
            held.insert(key.to_string(), Box::new(value));
            true
        }
        None => false,
    })
}

/// A point where what is owed the pool before the work running here goes first: a step's cut, a scope's item. Where a
/// group owed before this thread's is waiting, its next unit runs here, on this stack, as deep as is safe - as Cilk and
/// Go let what is ready run at a yield - and then this goes on.
pub fn yield_now() {
    if unweighed() || HELPING.with(std::cell::Cell::get) >= MOST_HELPING {
        return;
    }
    let Some(unit) = pool().pop_owed_before(&current()) else { return };
    HELPING.with(|depth| depth.set(depth.get() + 1));
    apart(unit);
    HELPING.with(|depth| depth.set(depth.get() - 1));
}

/// How many of their own work a thread waiting may run on top of its stack: past it, it only waits.
const MOST_HELPING: usize = 4;

/// What a scope's helpers share with it, owned: whether it closed, and how many are inside.
#[derive(Default)]
struct Scope {
    inside: Mutex<(bool, usize)>,
    left: Condvar,
}

/// What a helper works through: items, until none is left (false) or another group is owed the pool first (true).
type Drain<'a> = dyn Fn(&dyn Fn() -> bool) -> bool + Sync + 'a;

/// A helper of a scope, queued under its group: inside while the scope is open, it works through the items; owed to
/// another group, it steps out and queues itself again, so that the worker goes to what is owed it - each item a point
/// where what was asked since can go first.
fn helper(scope: Arc<Scope>, group: Group, drain: &'static Drain<'static>) -> Unit {
    Box::new(move || {
        {
            let mut inside = scope.inside.lock().unwrap_or_else(PoisonError::into_inner);
            if inside.0 {
                return;
            }
            inside.1 += 1;
        }
        let pool = pool();
        let stepped_out = group.enter(|| drain(&|| pool.owed_before(&group)));
        scope.inside.lock().unwrap_or_else(PoisonError::into_inner).1 -= 1;
        scope.left.notify_all();
        if stepped_out {
            pool.push(&group.clone(), helper(scope, group, drain));
        }
    })
}

/// Close a scope, and wait until no helper is inside it.
fn close(scope: &Scope, group: &Group) {
    scope.inside.lock().unwrap_or_else(PoisonError::into_inner).0 = true;
    wait_helping(group, &scope.inside, &scope.left, |inside| inside.1 == 0);
}

/// Wait until `done` holds of what `lock` guards, told by `told`: meanwhile running what else its group has queued, as
/// deep as is safe. Only its own: run on its stack, another group's work would hold it as long as that work goes on.
fn wait_helping<S>(group: &Group, lock: &Mutex<S>, told: &Condvar, done: impl Fn(&S) -> bool) {
    let pool = pool();
    loop {
        let state = lock.lock().unwrap_or_else(PoisonError::into_inner);
        if done(&state) {
            return;
        }
        if HELPING.with(std::cell::Cell::get) < MOST_HELPING {
            drop(state);
            if let Some(unit) = pool.try_pop_of(group) {
                HELPING.with(|depth| depth.set(depth.get() + 1));
                apart(unit);
                HELPING.with(|depth| depth.set(depth.get() - 1));
            } else {
                let state = lock.lock().unwrap_or_else(PoisonError::into_inner);
                if !done(&state) {
                    drop(told.wait_timeout(state, Duration::from_millis(1)));
                }
            }
            continue;
        }
        drop(told.wait_timeout(state, Duration::from_millis(1)));
    }
}

/// What a work list holds: the items waiting, how many are being worked, and how many helpers are queued or working.
struct List<T> {
    waiting: VecDeque<T>,
    busy: usize,
    helpers: usize,
}

/// What works an item of a work list, given what adds another.
type Worker<'a, T> = dyn Fn(T, &dyn Fn(T)) + Sync + 'a;

/// A work list as its caller and helpers share it.
struct Work<'a, T> {
    list: Mutex<List<T>>,
    changed: Condvar,
    worker: &'a Worker<'a, T>,
    ahead: &'a (dyn Fn(&T) -> bool + Sync),
    group: Group,
    scope: Arc<Scope>,
    panicked: Mutex<Option<Box<dyn Any + Send>>>,
    /// What a helper runs, once made.
    drain: OnceLock<&'static Drain<'static>>,
}

impl<T: Send> Work<'_, T> {
    fn lock(&self) -> std::sync::MutexGuard<'_, List<T>> {
        self.list.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Work through what waits until nothing does (false) or another group is owed this thread first (true). A helper
    /// finding nothing leaves, counted out under the lock that found it so: what is added after starts another.
    fn drain(&self, owed: &dyn Fn() -> bool, helper: bool) -> bool {
        loop {
            let item = {
                let mut list = self.lock();
                if list.waiting.is_empty() {
                    if helper {
                        list.helpers -= 1;
                    }
                    return false;
                }
                if owed() {
                    return true;
                }
                list.busy += 1;
                list.waiting.pop_front()
            };
            let Some(item) = item else { continue };
            let started = Instant::now();
            let done = std::panic::catch_unwind(AssertUnwindSafe(|| (self.worker)(item, &|more| self.add(more))));
            self.group.charge(started.elapsed());
            if let Err(panic) = done {
                self.panicked.lock().unwrap_or_else(PoisonError::into_inner).get_or_insert(panic);
            }
            self.lock().busy -= 1;
            self.changed.notify_all();
        }
    }

    /// Another item, in line or ahead of what waits: a helper started for it where fewer are out than what waits and is
    /// worked could use.
    fn add(&self, item: T) {
        let pool = pool();
        let ahead = (self.ahead)(&item);
        let mut list = self.lock();
        if ahead {
            list.waiting.push_front(item);
        } else {
            list.waiting.push_back(item);
        }
        let spawn = list.helpers < (list.busy + list.waiting.len()).min(pool.threads);
        if spawn {
            list.helpers += 1;
        }
        drop(list);
        self.changed.notify_all();
        if spawn && let Some(&drain) = self.drain.get() {
            pool.push(&self.group, helper(self.scope.clone(), self.group.clone(), drain));
        }
    }
}

/// Each of these worked out, and each item it adds as it goes (`work`'s second argument), until none is left: by this
/// thread and helpers queued in the pool under its group, as `parallel`, started as items are added - a pipeline whose
/// stages feed the next, as an extraction's reading feeds what writes each asset. Items are taken in the order added,
/// but for those `ahead` puts before all that waits: what feeds the rest.
///
/// # Panics
/// Where work for an item panicked: after the rest is done, as that panic.
pub(crate) fn worklist<T: Send>(items: Vec<T>, ahead: impl Fn(&T) -> bool + Sync, work: impl Fn(T, &dyn Fn(T)) + Sync) {
    let group = current();
    let work = Work {
        list: Mutex::new(List { waiting: VecDeque::new(), busy: 0, helpers: 0 }),
        changed: Condvar::new(),
        worker: &work,
        ahead: &ahead,
        group: group.clone(),
        scope: Arc::new(Scope::default()),
        panicked: Mutex::default(),
        drain: OnceLock::new(),
    };
    let drain = |owed: &dyn Fn() -> bool| work.drain(owed, true);
    let drain: &Drain<'_> = &drain;
    // SAFETY: as `parallel`'s: a helper calls it only while the scope is open, counted inside, and the scope closes and
    // waits until none is inside before this returns.
    let drain: &'static Drain<'static> = unsafe { std::mem::transmute(drain) };
    let _ = work.drain.set(drain);
    for item in items {
        work.add(item);
    }
    loop {
        work.drain(
            &|| {
                yield_now();
                false
            },
            false,
        );
        // Others are at work, which may add more: helped, or waited on.
        let finished = |list: &List<T>| !list.waiting.is_empty() || list.busy == 0;
        wait_helping(&group, &work.list, &work.changed, finished);
        let list = work.lock();
        if list.waiting.is_empty() && list.busy == 0 {
            break;
        }
    }
    close(&work.scope, &group);
    if let Some(panic) = work.panicked.into_inner().unwrap_or_else(PoisonError::into_inner) {
        std::panic::resume_unwind(panic);
    }
}

/// Each of these worked out at the same time, each result in its item's place: by this thread and by helpers queued in
/// the pool under its group, each taking the next item until none is left. It returns once every helper that started is
/// done, having run other queued work while it waited; a helper starting after that does nothing.
///
/// # Panics
/// Where work for an item panicked: after the rest is done, as that panic.
pub(crate) fn parallel<T: Sync, R: Send>(items: &[T], work: impl Fn(&T) -> R + Sync) -> Vec<R> {
    // What one edit changed is one asset, and is not worth a helper.
    if items.len() < 2 {
        return items.iter().map(work).collect();
    }
    let group = current();
    let pool = pool();
    let next = AtomicUsize::new(0);
    let made: Mutex<Vec<Option<R>>> = Mutex::new((0..items.len()).map(|_| None).collect());
    let panicked: Mutex<Option<Box<dyn Any + Send>>> = Mutex::default();
    let drain = |owed: &dyn Fn() -> bool| {
        loop {
            if next.load(Ordering::Relaxed) >= items.len() {
                return false;
            }
            if owed() {
                return true;
            }
            let at = next.fetch_add(1, Ordering::Relaxed);
            let Some(item) = items.get(at) else { return false };
            let started = Instant::now();
            let done = std::panic::catch_unwind(AssertUnwindSafe(|| work(item)));
            group.charge(started.elapsed());
            match done {
                Ok(done) => made.lock().unwrap_or_else(PoisonError::into_inner)[at] = Some(done),
                Err(panic) => {
                    panicked.lock().unwrap_or_else(PoisonError::into_inner).get_or_insert(panic);
                }
            }
        }
    };
    let drain: &Drain<'_> = &drain;
    // SAFETY: a helper calls it only while the scope is open, counted inside; the scope closes and waits until none is
    // inside before this function returns, so it is called only while what it borrows lives.
    let drain: &'static Drain<'static> = unsafe { std::mem::transmute(drain) };
    let scope = Arc::new(Scope::default());
    for _ in 0..(items.len() - 1).min(pool.threads) {
        pool.push(&group, helper(scope.clone(), group.clone(), drain));
    }
    // Its caller is no worker of the pool, which it does not hand back: it works through what is left, letting what is
    // owed before it go first between items.
    drain(&|| {
        yield_now();
        false
    });
    close(&scope, &group);
    if let Some(panic) = panicked.into_inner().unwrap_or_else(PoisonError::into_inner) {
        std::panic::resume_unwind(panic);
    }
    made.into_inner()
        .unwrap_or_else(PoisonError::into_inner)
        .into_iter()
        .map(|made| made.expect("each item made"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_is_worked_out_at_once_comes_back_in_its_order() {
        let items: Vec<usize> = (0..500).collect();
        assert_eq!(parallel(&items, |item| item * 2), items.iter().map(|item| item * 2).collect::<Vec<_>>());
        assert_eq!(parallel(&[] as &[usize], |item| *item), Vec::<usize>::new());
    }

    #[test]
    fn scopes_within_scopes_finish() {
        let outer: Vec<usize> = (0..64).collect();
        let sums = parallel(&outer, |at| {
            let inner: Vec<usize> = (0..64).map(|i| i + at).collect();
            parallel(&inner, |i| parallel(&[*i, 1], |j| *j).iter().sum::<usize>()).iter().sum::<usize>()
        });
        assert_eq!(sums[0], (0..64).map(|i| i + 1).sum::<usize>());
    }

    #[test]
    fn a_panic_comes_back_after_the_rest() {
        let items: Vec<usize> = (0..32).collect();
        let done = std::panic::catch_unwind(|| {
            parallel(&items, |item| {
                assert_ne!(*item, 7, "seven");
                *item
            })
        });
        assert!(done.is_err());
    }

    #[test]
    fn the_group_that_asked_least_goes_first_and_a_returning_one_is_owed_nothing() {
        let mut queues = Queues::default();
        let (heavy, light) = (Group::new(Class::Background, "heavy"), Group::new(Class::Interactive, "light"));
        heavy.charge(Duration::from_millis(500));
        light.charge(Duration::from_millis(5));
        let order = Arc::new(Mutex::new(Vec::new()));
        for (group, name) in [(&heavy, "heavy"), (&light, "light"), (&heavy, "heavy")] {
            let order = order.clone();
            queues.push(group, Box::new(move || order.lock().unwrap().push(name)));
        }
        while let Some(unit) = queues.pop() {
            unit();
        }
        assert_eq!(*order.lock().unwrap(), ["light", "heavy", "heavy"]);
        // Idle a long while, it comes back level with those waiting, less the head start.
        let idle = Group::new(Class::Background, "idle");
        queues.least = 1_000_000_000;
        queues.push(&idle, Box::new(|| {}));
        assert!(idle.virtual_runtime() >= 1_000_000_000 - u64::try_from(RETURNING.as_nanos()).unwrap());
    }

    #[test]
    fn a_work_list_works_what_its_items_add_until_none_is_left() {
        // Each number adds its halves: a tree of 2^10 leaves, its stages feeding the next.
        let leaves = AtomicUsize::new(0);
        let threads = Mutex::new(std::collections::HashSet::new());
        worklist(
            vec![1024usize, 1024],
            |_| false,
            |n, add| {
                threads.lock().unwrap().insert(std::thread::current().id());
                if n == 1 {
                    leaves.fetch_add(1, Ordering::Relaxed);
                    std::thread::sleep(Duration::from_micros(50));
                } else {
                    add(n / 2);
                    add(n - n / 2);
                }
            },
        );
        assert_eq!(leaves.into_inner(), 2048);
        assert!(pool().threads < 2 || threads.into_inner().unwrap().len() > 1);
        // Nested in a scope, and empty.
        let sums = parallel(&[3usize, 5], |&n| {
            let sum = AtomicUsize::new(0);
            worklist(
                (0..n).collect(),
                |_| false,
                |i, _| {
                    sum.fetch_add(i, Ordering::Relaxed);
                },
            );
            sum.into_inner()
        });
        assert_eq!(sums, vec![3, 10]);
        worklist(Vec::<u8>::new(), |_| false, |_, _| unreachable!());
    }

    #[test]
    fn a_work_list_panics_as_its_item_did_once_the_rest_is_done() {
        let done = AtomicUsize::new(0);
        let panicked = std::panic::catch_unwind(AssertUnwindSafe(|| {
            worklist(
                (0..64).collect(),
                |_| false,
                |n: usize, _| {
                    assert!(n != 7, "seven");
                    done.fetch_add(1, Ordering::Relaxed);
                },
            );
        }));
        assert!(panicked.is_err());
        assert_eq!(done.into_inner(), 63);
    }

    #[test]
    fn at_a_yield_only_what_is_owed_before_goes_first() {
        let mut queues = Queues::default();
        let (long, asked) = (Group::new(Class::Background, "long"), Group::new(Class::Interactive, "asked"));
        long.charge(Duration::from_secs(1));
        queues.push(&long, Box::new(|| {}));
        // Nothing owed before the asked group: what runs long waits.
        assert!(queues.pop_owed_before(&asked).is_none());
        queues.push(&asked, Box::new(|| {}));
        // The long one yields to it, once.
        assert!(queues.pop_owed_before(&long).is_some());
        assert!(queues.pop_owed_before(&long).is_none());
    }

    #[test]
    fn work_spawned_is_charged_to_the_group_that_spawned_it() {
        let group = Group::new(Class::Commanded, "charged");
        let before = group.virtual_runtime();
        let items: Vec<u64> = (0..16).collect();
        group.enter(|| {
            parallel(&items, |_| std::thread::sleep(Duration::from_millis(2)));
        });
        assert!(group.virtual_runtime() > before);
    }
}
