//! What takes a while to let go of, let go of away from the window: an index is thousands of records, and the frame that
//! drops the last of it is one that shows late.

use std::any::Any;
use std::sync::OnceLock;
use std::sync::mpsc::{Sender, channel};

/// Drop this on a thread of its own, in time. Without one to go to, it is dropped here.
pub fn drop_later<T: Send + 'static>(value: T) {
    static LATER: OnceLock<Option<Sender<Box<dyn Any + Send>>>> = OnceLock::new();
    let later = LATER.get_or_init(|| {
        let (to, from) = channel::<Box<dyn Any + Send>>();
        std::thread::Builder::new()
            .name("let go".into())
            .spawn(move || from.into_iter().for_each(drop))
            .ok()
            .map(|_| to)
    });
    if let Some(later) = later {
        let _ = later.send(Box::new(value));
    }
}
