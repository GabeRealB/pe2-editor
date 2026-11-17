//! Drawing a new uid (`pe2_path::Uid`): 64 random bits, drawn again where they are 0 or a uid taken. A uid is drawn
//! once, when an asset is first declared, and kept from then on.

pub use pe2_path::Uid;

/// A new uid, drawn of `random`, one `taken` does not hold.
pub fn new(random: &dyn pe2_io::Random, taken: impl Fn(Uid) -> bool) -> Uid {
    loop {
        let drawn = Uid::from_u64(random.u64());
        if let Some(uid) = drawn.filter(|&uid| !taken(uid)) {
            return uid;
        }
    }
}
