//! Pictures drawn off the window's thread: an asset's picture in a look - its kind's `picture/<look>` output, made
//! through the work graph and not kept - drawn once on a few threads of their own (each a job of its extension) from
//! the project as the last take-in left it, and shared by all that show it - the inspector's header, the preview's
//! views, the panels. Until one is drawn it is not there yet, and the window is woken when it is. Each is kept with
//! what it read: a take-in forgets those whose reads no longer hold.

use pe2_ext::Image;
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

enum Slot {
    Drawing(u64),
    /// Drawn, with what it read; failed, with nothing.
    Drawn(Drawn, Option<Reads>),
}

/// A picture as a thread answers: its asset and look, the asking it answers, the picture and what it read.
type Answer = (String, usize, u64, Drawn, Option<Reads>);

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
                    let made = request.index.assets.get(&request.path).and_then(|asset| {
                        let address = extensions.picture_address(asset, request.look)?;
                        Some(extensions.output(&engine, asset, &address, true))
                    });
                    let (picture, reads) = match made {
                        Some(Ok(given)) => match given.value.as_deref() {
                            Some(project::graph::Value::Image(image)) => (Ok(Arc::new(image.clone())), Some(given.reads)),
                            _ => (Err("it drew no picture".to_string()), None),
                        },
                        Some(Err(e)) => (Err(format!("{e:#}")), None),
                        None => (Err("it has no picture".to_string()), None),
                    };
                    if drawn.send((request.path, request.look, request.asked, picture, reads)).is_err() {
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
            Slot::Drawn(_, reads) => {
                reads.as_ref().is_some_and(|reads| index.assets.get(path).is_some_and(|asset| reads.hold(&**asset)))
            }
        });
        self.index = index;
    }

    /// An asset's picture in a look: drawn; or, while it is being drawn (asked now if it was not), none.
    pub fn get(&mut self, path: &str, look: usize) -> Option<Drawn> {
        for (path, look, asked, picture, reads) in self.results.try_iter() {
            if let Some(slot) = self.slots.get_mut(&(path, look))
                && matches!(slot, Slot::Drawing(a) if *a == asked)
            {
                *slot = Slot::Drawn(picture, reads);
            }
        }
        let key = (path.to_string(), look);
        match self.slots.get(&key) {
            Some(Slot::Drawn(picture, _)) => return Some(picture.clone()),
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

    /// Forget every picture.
    pub fn clear(&mut self) {
        self.slots.clear();
    }
}

impl Drop for Pictures {
    /// The threads finish what they are drawing and stop.
    fn drop(&mut self) {
        self.requests = None;
    }
}
