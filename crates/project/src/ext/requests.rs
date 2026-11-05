//! What a view asks of the project without waiting for it (`Project::request`): an output made on threads of its own, the
//! view told it is on its way, and woken when it is there. A view's frame is the window's, and how long an output takes to
//! make is what it reads: nothing says it fits in one. Those asking for the same output share one making, and what was made
//! is kept until what it reads changes, so that the panes showing one scene ask for it once.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

use pe2_ext::Requested;

use super::Extensions;
use crate::index::Index;

/// How long the first to ask waits: what is kept comes back within the frame asking, so that nothing flashes empty.
const BUDGET: Duration = Duration::from_millis(10);
/// When a view waiting says so: sooner, it would only flash.
pub const SAY_AFTER: Duration = Duration::from_millis(100);
/// How many outputs are kept, the oldest going first.
const KEPT: usize = 16;
const THREADS: usize = 2;

type Key = (String, String);
type Wake = Box<dyn Fn(Duration) + Send>;
type Maker = Arc<dyn Fn(&Index, &crate::index::Asset, &str) -> anyhow::Result<Vec<u8>> + Send + Sync>;

enum Slot {
    Making { since: Instant, asked: u64 },
    Made(Arc<Vec<u8>>),
    Failed(String),
}

#[derive(Default)]
struct State {
    slots: HashMap<Key, Slot>,
    kept: VecDeque<Key>,
    asked: u64,
}

/// What the threads and those asking share.
struct Shared {
    state: Mutex<State>,
    answered: Condvar,
    index: Mutex<Arc<Index>>,
    /// What makes an output: the extensions', held weakly, as they hold this.
    maker: Mutex<Option<Maker>>,
    /// Told to show the window again, in this long.
    wake: Mutex<Option<Wake>>,
    finished: AtomicU64,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn woken(&self, after: Duration) {
        if let Some(wake) = &*self.wake.lock().unwrap_or_else(PoisonError::into_inner) {
            wake(after);
        }
    }

    /// Make what was asked, with what is current when its turn comes, and keep it unless it was forgotten meanwhile.
    fn make(&self, key: &Key, asked: u64) {
        let index = self.index.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let maker = self.maker.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let made = match (maker, index.assets.get(&key.0)) {
            (Some(maker), Some(asset)) => maker(&index, asset, &key.1).map_err(|why| format!("{why:#}")),
            (None, _) => Err("nothing makes it".to_string()),
            (_, None) => Err(format!("no asset {}", key.0)),
        };
        let mut state = self.lock();
        if !matches!(state.slots.get(key), Some(Slot::Making { asked: now, .. }) if *now == asked) {
            return;
        }
        state.slots.insert(key.clone(), made.map_or_else(Slot::Failed, |bytes| Slot::Made(Arc::new(bytes))));
        state.kept.push_back(key.clone());
        while state.kept.len() > KEPT {
            if let Some(oldest) = state.kept.pop_front() {
                state.slots.remove(&oldest);
            }
        }
        drop(state);
        self.finished.fetch_add(1, Ordering::Relaxed);
        self.answered.notify_all();
        self.woken(Duration::ZERO);
    }
}

/// What extensions are served by, as they hold what holds them only weakly and neither is to know the other's whole type.
pub trait Asking: Send + Sync {
    /// An output, kept or on its way.
    fn request(&self, path: &str, address: &str) -> Requested;
    /// An output where it is kept.
    fn kept(&self, path: &str, address: &str) -> Option<Arc<Vec<u8>>>;
}

/// The outputs asked for by what shows the project open.
pub struct Requests {
    shared: Arc<Shared>,
    jobs: Mutex<Sender<(Key, u64)>>,
}

impl Requests {
    /// Start the threads, on this project as it is taken in.
    #[must_use]
    pub fn start(index: Arc<Index>) -> Arc<Requests> {
        let shared = Arc::new(Shared {
            state: Mutex::default(),
            answered: Condvar::new(),
            index: Mutex::new(index),
            maker: Mutex::new(None),
            wake: Mutex::new(None),
            finished: AtomicU64::new(0),
        });
        let (jobs, queue) = channel::<(Key, u64)>();
        let queue = Arc::new(Mutex::new(queue));
        for at in 0..THREADS {
            let (shared, queue) = (shared.clone(), queue.clone());
            let _ = std::thread::Builder::new().name(format!("requests {at}")).spawn(move || {
                loop {
                    let job = queue.lock().unwrap_or_else(PoisonError::into_inner).recv();
                    let Ok((key, asked)) = job else { return };
                    shared.make(&key, asked);
                }
            });
        }
        Arc::new(Requests { shared, jobs: Mutex::new(jobs) })
    }

    /// Go on with these extensions.
    pub fn set_extensions(&self, extensions: &Arc<Extensions>) {
        let extensions: Weak<Extensions> = Arc::downgrade(extensions);
        let maker: Maker = Arc::new(move |index, asset, address| {
            let extensions = extensions.upgrade().ok_or_else(|| anyhow::anyhow!("the extensions are gone"))?;
            extensions.bytes(index, asset, address)
        });
        *self.shared.maker.lock().unwrap_or_else(PoisonError::into_inner) = Some(maker);
    }

    /// Go on with the project as now taken in.
    pub fn set_index(&self, index: Arc<Index>) {
        *self.shared.index.lock().unwrap_or_else(PoisonError::into_inner) = index;
    }

    /// Whether anything wakes the window yet.
    #[must_use]
    pub fn wakes(&self) -> bool {
        self.shared.wake.lock().unwrap_or_else(PoisonError::into_inner).is_some()
    }

    /// What wakes the window, told in how long.
    pub fn wake_by(&self, wake: impl Fn(Duration) + Send + 'static) {
        *self.shared.wake.lock().unwrap_or_else(PoisonError::into_inner) = Some(Box::new(wake));
    }

    /// Forget what these assets make: it is made again when it is next asked for. What is on its way is left to arrive to
    /// nobody.
    pub fn forget(&self, paths: &impl Fn(&str) -> bool) {
        let mut state = self.shared.lock();
        state.slots.retain(|(path, _), _| !paths(path));
        state.kept.retain(|(path, _)| !paths(path));
    }

    /// Forget all of it.
    pub fn clear(&self) {
        self.forget(&|_| true);
    }

    /// How many have been answered: what was said while one was on its way is out of date when this moves.
    #[must_use]
    pub fn finished(&self) -> u64 {
        self.shared.finished.load(Ordering::Relaxed)
    }

    /// Whether any is on its way.
    #[must_use]
    pub fn busy(&self) -> bool {
        self.shared.lock().slots.values().any(|slot| matches!(slot, Slot::Making { .. }))
    }
}

impl Asking for Requests {
    fn request(&self, path: &str, address: &str) -> Requested {
        let key = (path.to_string(), address.to_string());
        let mut state = self.shared.lock();
        if !state.slots.contains_key(&key) {
            state.asked += 1;
            let asked = state.asked;
            state.slots.insert(key.clone(), Slot::Making { since: Instant::now(), asked });
            let _ = self.jobs.lock().unwrap_or_else(PoisonError::into_inner).send((key.clone(), asked));
            let making = |state: &mut State| matches!(state.slots.get(&key), Some(Slot::Making { .. }));
            state = self
                .shared
                .answered
                .wait_timeout_while(state, BUDGET, making)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        let answer = match state.slots.get(&key) {
            Some(Slot::Made(bytes)) => Requested::Ready(bytes.to_vec()),
            Some(Slot::Failed(why)) => Requested::Failed(why.clone()),
            Some(Slot::Making { since, .. }) => {
                Requested::Pending { waited: u32::try_from(since.elapsed().as_millis()).unwrap_or(u32::MAX) }
            }
            None => Requested::Pending { waited: 0 },
        };
        drop(state);
        // What is waiting says so in time, whether or not anything else shows the window.
        if let Requested::Pending { waited } = answer
            && let Some(left) = SAY_AFTER.checked_sub(Duration::from_millis(u64::from(waited)))
        {
            self.shared.woken(left);
        }
        answer
    }

    fn kept(&self, path: &str, address: &str) -> Option<Arc<Vec<u8>>> {
        match self.shared.lock().slots.get(&(path.to_string(), address.to_string())) {
            Some(Slot::Made(bytes)) => Some(bytes.clone()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Project;
    use crate::cache::Cache;
    use crate::ext::Loadable;
    use crate::lock::Lock;
    use pe2_ext::ty::Registered as _;
    use pe2_ext::{Extension, File, Registry, Result, Typed, ty};
    use std::path::PathBuf;
    use std::sync::atomic::AtomicUsize;

    static MADE: AtomicUsize = AtomicUsize::new(0);
    /// How long the operation below takes, in milliseconds.
    static TAKES: AtomicU64 = AtomicU64::new(0);

    struct Blob;

    impl ty::Registered for Blob {
        const NAME: &str = "test.requested@blob";
        const LABEL: &str = "Bytes";
    }

    impl ty::Files for Blob {}

    #[expect(clippy::unnecessary_wraps, reason = "an entry point may fail; this one does not")]
    fn register(r: &mut Registry) -> Result<()> {
        let _ = r.value_type::<Blob>();
        let _ = r.op("slow").output::<Blob>().run(|_| {
            MADE.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(TAKES.load(Ordering::SeqCst)));
            Ok(Typed::new(Blob::NAME, vec![File::new("made.bin", b"made".to_vec())]))
        });
        Ok(())
    }

    fn opened(name: &str) -> (PathBuf, Lock, Arc<Requests>, Arc<Extensions>) {
        let root = std::env::temp_dir().join(format!("pe2-requests-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let write = |path: &str, text: &str| {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write("project.toml", "[[layer]]\nname = \"base\"\n");
        write(
            ".layers/base/n.node/node.toml",
            "[[stack]]\nop = \"test.requested@slow\"\n\n[[stack]]\nop = \"output\"\nname = \"native\"\nretain = \"transient\"\n",
        );
        let project = Project::load(&root).unwrap();
        let lock = Lock::try_acquire(&root, "a test").unwrap();
        let cache = Cache::open(&project, &lock).unwrap();
        let index = crate::index::Index::open(&project, &cache).unwrap();
        let extension = Extension {
            id: "test.requested",
            version: "0.1.0",
            requires: &[],
            weak: &[],
            optional: &[],
            provides: &[],
            platforms: &[],
            unsandboxed: false,
            register,
        };
        let extensions = Arc::new(Extensions::load(vec![Loadable::from(extension)], true));
        extensions.keep_in(crate::graph::store::Store::open(&cache).unwrap());
        let requests = Requests::start(Arc::new(index));
        requests.set_extensions(&extensions);
        (root, lock, requests, extensions)
    }

    fn ready(requests: &Requests, address: &str) -> Requested {
        let started = Instant::now();
        loop {
            match requests.request("n", address) {
                Requested::Pending { .. } => assert!(started.elapsed() < Duration::from_secs(10), "it never came"),
                answer => return answer,
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    // One test, as the operation's counts are one process's.
    #[test]
    fn what_is_asked_is_made_once_off_the_asking_thread_and_kept_until_forgotten() {
        let (root, _lock, requests, _extensions) = opened("once");
        TAKES.store(150, Ordering::SeqCst);
        let started = Instant::now();
        assert!(matches!(requests.request("n", "native"), Requested::Pending { .. }), "too long to wait for");
        assert!(started.elapsed() < Duration::from_millis(120), "whoever asked went on");
        // Others asking share it.
        assert!(matches!(requests.request("n", "native"), Requested::Pending { .. }));
        assert_eq!(ready(&requests, "native"), Requested::Ready(b"made".to_vec()));
        assert_eq!(MADE.load(Ordering::SeqCst), 1);
        drop(ready(&requests, "native"));
        assert_eq!(MADE.load(Ordering::SeqCst), 1, "kept");
        assert_eq!(requests.finished(), 1);
        // Made quickly, it is there for the one asking.
        TAKES.store(0, Ordering::SeqCst);
        requests.clear();
        assert_eq!(
            requests.request("n", "native"),
            Requested::Ready(b"made".to_vec()),
            "within what a frame can spare"
        );
        assert_eq!(MADE.load(Ordering::SeqCst), 2);
        // What was forgotten while on its way arrives to nobody.
        TAKES.store(60, Ordering::SeqCst);
        requests.clear();
        assert!(matches!(requests.request("n", "native"), Requested::Pending { .. }));
        requests.clear();
        TAKES.store(0, Ordering::SeqCst);
        assert_eq!(ready(&requests, "native"), Requested::Ready(b"made".to_vec()));
        // Why it could not be is said, and kept with the rest.
        assert!(matches!(ready(&requests, "no such output"), Requested::Failed(_)));
        assert!(!requests.busy());
        let _ = std::fs::remove_dir_all(&root);
    }
}
