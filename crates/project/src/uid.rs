//! Drawing a new uid (`pe2_path::Uid`): 63 random bits, drawn again until none is taken, as Godot checks its table
//! (`core/io/resource_uid.cpp`). A uid is drawn once, when an asset is first declared, and kept from then on.

pub use pe2_path::Uid;

/// A new uid, one `taken` does not hold.
///
/// # Errors
/// The system's random source fails.
pub fn new(taken: impl Fn(Uid) -> bool) -> Result<Uid, getrandom::Error> {
    loop {
        let drawn = i64::try_from(getrandom::u64()? >> 1).ok().and_then(Uid::from_id);
        if let Some(uid) = drawn.filter(|&uid| !taken(uid)) {
            return Ok(uid);
        }
    }
}
