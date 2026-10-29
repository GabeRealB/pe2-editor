//! Thumbnails of the asset tree's assets, made by their kinds' extensions on a thread of their own and kept as artifacts
//! in the project cache (`project::artifacts`), named in the extension's namespace (`pe2.images@thumbnail.png`): an
//! asset shown in the tree is asked for once, read back from the cache when it was made in an earlier session, made
//! and kept otherwise. The key of an artifact follows its asset's files and its extension's version, so an edit or a
//! new version makes a new one; a refreshed asset's thumbnail is forgotten and asked for again.

use eframe::egui;
use project::artifacts::{self, Artifacts};
use project::ext::Extensions;
use project::index::Asset;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, OnceLock};

/// The longer side of a thumbnail, in pixels: twice the tree's row, for scaled displays.
pub const SIDE: usize = 32;

enum Thumb {
    Pending,
    Ready(egui::TextureHandle),
    /// No picture to make one from.
    None,
}

struct Job {
    path: String,
    uid: String,
    /// The artifact it is kept as, and the key it is made under.
    name: String,
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
    /// Start the thread making thumbnails by the `extensions`, keeping them in `artifacts`.
    pub fn start(extensions: Arc<Extensions>, artifacts: Artifacts) -> Thumbs {
        let (jobs, queue) = mpsc::channel::<Job>();
        let (finished, done) = mpsc::channel();
        let ctx: Arc<OnceLock<egui::Context>> = Arc::default();
        let wake = ctx.clone();
        std::thread::spawn(move || {
            for job in queue {
                let image = make(&extensions, &artifacts, &job);
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
                let uid = asset.uid.clone()?;
                let name = extensions.thumbnail_name(asset)?;
                let key = artifacts::key(asset, &extensions.thumbnail_generator(asset)?, &SIDE.to_string());
                let job = Job { path: asset.path.clone(), uid, name, key, asset: asset.clone() };
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

/// A thumbnail: read back from the cache, or made by its extension and kept.
fn make(extensions: &Extensions, artifacts: &Artifacts, job: &Job) -> Option<egui::ColorImage> {
    let kept = artifacts.get(&job.uid, &job.name, &job.key).and_then(|png| project::ext::from_png(&png).ok());
    let image = if let Some(image) = kept {
        image
    } else {
        let image = extensions.thumbnail(&job.asset, SIDE)?.ok()?;
        if let Ok(png) = project::ext::png(&image) {
            let _ = artifacts.put(&job.uid, &job.name, &job.key, &png);
        }
        image
    };
    Some(egui::ColorImage::from_rgba_unmultiplied([image.width, image.height], &image.rgba))
}
