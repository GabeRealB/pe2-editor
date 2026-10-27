//! Asset uids: `uid://` and 63 random bits in base 36 (0-9a-z), most significant digit first - Godot's scheme
//! (`core/io/resource_uid.cpp`), with the full alphabet where Godot's off-by-one drops `z` and `9`. 63 bits keep an id
//! inside a signed 64-bit integer. A uid is assigned once, when an asset is first declared, and kept from then on; a
//! new one is drawn until no declaration holds it, as Godot checks its table.

pub const PREFIX: &str = "uid://";
const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";

/// The id a uid's text stands for, if it is one.
#[must_use]
pub fn parse(text: &str) -> Option<i64> {
    let digits = text.strip_prefix(PREFIX)?;
    if digits.is_empty() || digits.len() > 13 {
        return None;
    }
    let mut n: u64 = 0;
    for c in digits.bytes() {
        let d = DIGITS.iter().position(|&x| x == c)?;
        n = n.checked_mul(36)?.checked_add(d as u64)?;
    }
    i64::try_from(n).ok()
}

/// A new id, one `taken` does not hold.
///
/// # Errors
/// The system's random source fails.
pub fn new(taken: impl Fn(i64) -> bool) -> Result<i64, getrandom::Error> {
    loop {
        let id = i64::try_from(getrandom::u64()? >> 1).unwrap_or(0);
        if !taken(id) {
            return Ok(id);
        }
    }
}

/// An id's text; a negative id is no uid (`uid://<invalid>`, as Godot writes it).
#[must_use]
pub fn text(id: i64) -> String {
    let Ok(mut n) = u64::try_from(id) else { return format!("{PREFIX}<invalid>") };
    let mut out = String::new();
    loop {
        out.insert(0, char::from(DIGITS[(n % 36) as usize]));
        n /= 36;
        if n == 0 {
            return format!("{PREFIX}{out}");
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn round_trip() {
        for t in ["uid://vdx31ffeypvs", "uid://0", "uid://1y2p0ij32e8e7"] {
            assert_eq!(super::text(super::parse(t).unwrap()), t);
        }
        assert_eq!(super::parse("uid://1y2p0ij32e8e8"), None); // 2^63: past 63 bits
        assert_eq!(super::parse("uid://ABC"), None);
        assert_eq!(super::text(-1), "uid://<invalid>");
    }
}
