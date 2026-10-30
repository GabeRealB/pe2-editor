//! Pictures drawn off the window's thread: an asset's picture in a look - its kind's `picture/<look>` output, made
//! through the work graph and not kept - drawn once on a few threads of their own (each a job of its extension) from
//! the project as the last take-in left it, and shared by all that show it - the inspector's header, the preview's
//! views, the panels. What its kind marks on it is made with it. Until one is drawn it is not there yet, and the window
//! is woken when it is. Each is kept with what it read: a take-in forgets those whose reads no longer hold.

use pe2_ext::{Image, Marks};
use project::ext::Extensions;
use project::graph::exec::Reads;
use project::index::Index;
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};

/// A picture asked for: the asset, the look, and the project to draw it from.
struct Request {
    path: String,
    look: usize,
    index: Arc<Index>,
    /// Which asking it answers: a picture forgotten meanwhile is asked again, and the older answer dropped.
    asked: u64,
}

/// A picture drawn, or why it could not be.
pub type Drawn = Result<Arc<Image>, String>;

/// What a thread made of a picture asked for: it, what is marked on it, and what they read (none where it failed).
struct Made {
    picture: Drawn,
    marks: Option<Arc<Marks>>,
    reads: Option<Reads>,
}

enum Slot {
    Drawing(u64),
    Drawn(Made),
}

/// A picture as a thread answers: its asset and look, the asking it answers, and what was made.
type Answer = (String, usize, u64, Made);

pub struct Pictures {
    slots: HashMap<(String, usize), Slot>,
    requests: Option<Sender<Request>>,
    results: Receiver<Answer>,
    index: Arc<Index>,
    extensions: Arc<Extensions>,
    asked: u64,
    ctx: Arc<OnceLock<eframe::egui::Context>>,
}

impl Pictures {
    /// Start the threads drawing pictures by `extensions` - half the cores, at most four - from `index`.
    pub fn start(index: Arc<Index>, extensions: Arc<Extensions>) -> Pictures {
        let (requests, queued) = mpsc::channel::<Request>();
        let (drawn, results) = mpsc::channel();
        let queued = Arc::new(Mutex::new(queued));
        let ctx: Arc<OnceLock<eframe::egui::Context>> = Arc::default();
        let threads = std::thread::available_parallelism().map_or(2, |n| (n.get() / 2).clamp(1, 4));
        for i in 0..threads {
            let (queued, drawn, extensions, wake) = (queued.clone(), drawn.clone(), extensions.clone(), ctx.clone());
            let _ = std::thread::Builder::new().name(format!("pictures {i}")).spawn(move || {
                let engine = extensions.engine();
                loop {
                    let request = match queued.lock() {
                        Ok(queue) => queue.recv(),
                        Err(_) => return,
                    };
                    let Ok(request) = request else { return };
                    let made = make(&extensions, &engine, &request);
                    if drawn.send((request.path, request.look, request.asked, made)).is_err() {
                        return;
                    }
                    if let Some(ctx) = wake.get() {
                        ctx.request_repaint();
                    }
                }
            });
        }
        Pictures { slots: HashMap::new(), requests: Some(requests), results, index, extensions, asked: 0, ctx }
    }

    /// The window to wake when a picture is drawn.
    pub fn wake(&self, ctx: &eframe::egui::Context) {
        let _ = self.ctx.set(ctx.clone());
    }

    /// The project to draw from: the last take-in's. A picture whose reads no longer hold in it is forgotten, to be
    /// drawn again when asked; one that failed is asked again.
    pub fn set_index(&mut self, index: Arc<Index>) {
        self.slots.retain(|(path, _), slot| match slot {
            Slot::Drawing(_) => true,
            Slot::Drawn(made) => {
                made.reads.as_ref().is_some_and(|reads| index.assets.get(path).is_some_and(|asset| reads.hold(&**asset, None)))
            }
        });
        self.index = index;
    }

    /// An asset's picture in a look: drawn; or, while it is being drawn (asked now if it was not), none.
    pub fn get(&mut self, path: &str, look: usize) -> Option<Drawn> {
        for (path, look, asked, made) in self.results.try_iter() {
            if let Some(slot) = self.slots.get_mut(&(path, look))
                && matches!(slot, Slot::Drawing(a) if *a == asked)
            {
                *slot = Slot::Drawn(made);
            }
        }
        let key = (path.to_string(), look);
        match self.slots.get(&key) {
            Some(Slot::Drawn(made)) => return Some(made.picture.clone()),
            Some(Slot::Drawing(_)) => return None,
            None => {}
        }
        if !self.index.assets.get(path).is_some_and(|a| self.extensions.has_picture(a)) {
            return Some(Err("it has no picture".into()));
        }
        self.asked += 1;
        let request = Request { path: path.to_string(), look, index: self.index.clone(), asked: self.asked };
        if self.requests.as_ref().is_some_and(|r| r.send(request).is_ok()) {
            self.slots.insert(key, Slot::Drawing(self.asked));
        }
        None
    }

    /// What is marked on a picture drawn.
    pub fn marks(&self, path: &str, look: usize) -> Option<Arc<Marks>> {
        match self.slots.get(&(path.to_string(), look)) {
            Some(Slot::Drawn(made)) => made.marks.clone(),
            _ => None,
        }
    }

    /// Forget every picture.
    pub fn clear(&mut self) {
        self.slots.clear();
    }
}

/// Make a picture asked for, and what its kind marks on it: a failure of those leaves the picture unmarked.
fn make(extensions: &Extensions, engine: &project::graph::exec::Engine, request: &Request) -> Made {
    let index = &request.index;
    let failed = |why: String| Made { picture: Err(why), marks: None, reads: None };
    let Some(asset) = index.assets.get(&request.path) else { return failed("it has no picture".to_string()) };
    let Some(address) = extensions.picture_address(asset, request.look) else { return failed("it has no picture".to_string()) };
    let given = match extensions.output(engine, index, asset, &address, true) {
        Ok(given) => given,
        Err(e) => return failed(format!("{e:#}")),
    };
    let Some(project::graph::Value::Image(image)) = given.value.as_deref() else { return failed("it drew no picture".to_string()) };
    let mut reads = given.reads.clone();
    let marked = extensions.marks_output(asset).and_then(|name| extensions.output(engine, index, asset, &name, true).ok());
    let marks = marked.and_then(|given| {
        reads.merge(&given.reads);
        match given.value.as_deref() {
            Some(project::graph::Value::Marks(marks)) => Some(Arc::new(marks.clone())),
            _ => None,
        }
    });
    Made { picture: Ok(Arc::new(image.clone())), marks, reads: Some(reads) }
}

impl Drop for Pictures {
    /// The threads finish what they are drawing and stop.
    fn drop(&mut self) {
        self.requests = None;
    }
}
