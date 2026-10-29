//! Thumbnails of the asset tree's assets: each its kind's `thumbnail/<side>` output, made on a thread of their own
//! through the work graph and kept in the project's store - an asset shown in the tree is asked for once, read back
//! from the store where what it read still holds (an earlier session's), made and kept otherwise. A refreshed asset's
//! thumbnail is forgotten and asked for again.

use eframe::egui;
use project::ext::Extensions;
use project::graph::exec::Engine;
use project::graph::store::Store;
use project::index::Asset;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, OnceLock};

enum Thumb {
    Pending,
    Ready(egui::TextureHandle),
    /// No picture to make one from.
    None,
}

struct Job {
    path: String,
    asset: Asset,
}

struct Done {
    path: String,
    image: Option<egui::ColorImage>,
}

pub struct Thumbs {
    jobs: mpsc::Sender<Job>,
    done: mpsc::Receiver<Done>,
    /// The window to wake when a thumbnail is ready, once there is one.
    ctx: Arc<OnceLock<egui::Context>>,
    shown: RefCell<HashMap<String, Thumb>>,
}

impl Thumbs {
    /// Start the thread making thumbnails by the `extensions`, keeping them in `store`.
    pub fn start(extensions: Arc<Extensions>, store: Store) -> Thumbs {
        let (jobs, queue) = mpsc::channel::<Job>();
        let (finished, done) = mpsc::channel();
        let ctx: Arc<OnceLock<egui::Context>> = Arc::default();
        let wake = ctx.clone();
        std::thread::spawn(move || {
            let engine = Engine::new(extensions.operations(), Some(store));
            for job in queue {
                let image = make(&extensions, &engine, &job.asset);
                if finished.send(Done { path: job.path, image }).is_err() {
                    break;
                }
                if let Some(ctx) = wake.get() {
                    ctx.request_repaint();
                }
            }
        });
        Thumbs { jobs, done, ctx, shown: RefCell::default() }
    }

    /// An asset's thumbnail, if it is ready; asked for when it is not.
    pub fn get(&self, ctx: &egui::Context, extensions: &Extensions, asset: &Asset) -> Option<egui::TextureId> {
        let _ = self.ctx.set(ctx.clone());
        let mut shown = self.shown.borrow_mut();
        for done in self.done.try_iter() {
            let thumb = match done.image {
                Some(image) => {
                    let options = egui::TextureOptions::LINEAR;
                    Thumb::Ready(ctx.load_texture(format!("thumbnail {}", done.path), image, options))
                }
                None => Thumb::None,
            };
            shown.insert(done.path, thumb);
        }
        match shown.get(&asset.path) {
            Some(Thumb::Ready(texture)) => Some(texture.id()),
            Some(Thumb::Pending | Thumb::None) => None,
            None => {
                if !extensions.has_thumbnail(asset) {
                    return None;
                }
                let job = Job { path: asset.path.clone(), asset: asset.clone() };
                if self.jobs.send(job).is_ok() {
                    shown.insert(asset.path.clone(), Thumb::Pending);
                }
                None
            }
        }
    }

    /// Forget the thumbnails of assets that changed: they are asked for again.
    pub fn forget<'a>(&self, paths: impl IntoIterator<Item = &'a String>) {
        let mut shown = self.shown.borrow_mut();
        for path in paths {
            shown.remove(path);
        }
    }

    /// Forget every thumbnail: the view changed (an eye, a solo).
    pub fn clear(&self) {
        self.shown.borrow_mut().clear();
    }
}

/// A thumbnail: read back from the store, or made by its extension and kept.
fn make(extensions: &Extensions, engine: &Engine, asset: &Asset) -> Option<egui::ColorImage> {
    let given = extensions.output(engine, asset, project::graph::THUMBNAIL, true).ok()?;
    let Some(project::graph::Value::Image(image)) = given.value.as_deref() else { return None };
    Some(egui::ColorImage::from_rgba_unmultiplied([image.width, image.height], &image.rgba))
}
