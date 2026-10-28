//! Thumbnails of the asset tree's images, made on a thread of their own and kept as artifacts in the project cache
//! (`project::artifacts`): an image shown in the tree is asked for once, read back from the cache when it was made in
//! an earlier session, made and kept otherwise. The key of an artifact follows its asset's files, so an edit makes a
//! new one; a refreshed asset's thumbnail is forgotten and asked for again.

use eframe::egui;
use project::artifacts::{self, Artifacts};
use project::index::Asset;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, OnceLock};

/// The longer side of a thumbnail, in pixels: twice the tree's row, for scaled displays.
pub const SIDE: usize = 32;
/// The generator, versioned: a change to how thumbnails are made raises it, and every thumbnail is made again.
const GENERATOR: &str = "thumbnail 1";
const NAME: &str = "thumbnail.png";

enum Thumb {
    Pending,
    Ready(egui::TextureHandle),
    /// No picture to make one from.
    None,
}

struct Job {
    path: String,
    uid: String,
    key: String,
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
    /// Start the thread making thumbnails, keeping them in `artifacts`.
    pub fn start(artifacts: Artifacts) -> Thumbs {
        let (jobs, queue) = mpsc::channel::<Job>();
        let (finished, done) = mpsc::channel();
        let ctx: Arc<OnceLock<egui::Context>> = Arc::default();
        let wake = ctx.clone();
        std::thread::spawn(move || {
            for job in queue {
                let image = make(&artifacts, &job);
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
    pub fn get(&self, ctx: &egui::Context, asset: &Asset) -> Option<egui::TextureId> {
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
                let uid = asset.uid.clone()?;
                let key = artifacts::key(asset, GENERATOR, &SIDE.to_string());
                let job = Job { path: asset.path.clone(), uid, key, asset: asset.clone() };
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

/// A thumbnail: read back from the cache, or made from the picture and kept.
fn make(artifacts: &Artifacts, job: &Job) -> Option<egui::ColorImage> {
    let kept = artifacts.get(&job.uid, NAME, &job.key).and_then(|png| preview::Image::from_png(&png).ok());
    let image = if let Some(image) = kept {
        image
    } else {
        let image = preview::picture(&job.asset, 0)?.ok()?.thumbnail(SIDE);
        if let Ok(png) = image.png() {
            let _ = artifacts.put(&job.uid, NAME, &job.key, &png);
        }
        image
    };
    Some(egui::ColorImage::from_rgba_unmultiplied([image.width, image.height], &image.rgba))
}
