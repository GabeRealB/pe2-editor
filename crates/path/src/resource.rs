//! What a project names an asset by: its uid, and what of it a suffix names.

use std::fmt;

use crate::error::{PathError, Why};
use crate::name::RelPath;

/// An asset's uid: 63 bits, written `uid://` and their digits in base 36 (0-9a-z), most significant first - Godot's
/// scheme, with the full alphabet. It is drawn once, when an asset is first declared, and kept from then on.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Uid(i64);

/// What a uid's text begins with.
pub(crate) const UID_PREFIX: &str = "uid://";
const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";

impl Uid {
    /// The uid of an id, if it is one: not negative.
    #[must_use]
    pub fn from_id(id: i64) -> Option<Uid> {
        (id >= 0).then_some(Uid(id))
    }

    /// Its id.
    #[must_use]
    pub fn id(self) -> i64 {
        self.0
    }

    /// `text` as a uid.
    ///
    /// # Errors
    /// It is not `uid://` and at most 13 digits of base 36 within 63 bits.
    pub fn new(text: &str) -> Result<Uid, PathError> {
        Uid::parse(text).ok_or_else(|| PathError::new(text, Why::NoUid))
    }

    fn parse(text: &str) -> Option<Uid> {
        let digits = text.strip_prefix(UID_PREFIX)?;
        if digits.is_empty() || digits.len() > 13 {
            return None;
        }
        let mut n: u64 = 0;
        for c in digits.bytes() {
            let d = DIGITS.iter().position(|&x| x == c)?;
            n = n.checked_mul(36)?.checked_add(d as u64)?;
        }
        i64::try_from(n).ok().map(Uid)
    }
}

impl fmt::Display for Uid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut n = self.0.unsigned_abs();
        let mut digits = [0u8; 13];
        let mut at = digits.len();
        loop {
            at -= 1;
            digits[at] = DIGITS[(n % 36) as usize];
            n /= 36;
            if n == 0 {
                break;
            }
        }
        f.write_str(UID_PREFIX)?;
        f.write_str(std::str::from_utf8(&digits[at..]).map_err(|_| fmt::Error)?)
    }
}

/// An asset, or what of it a suffix names: `uid://x`, `uid://x#objects`, `uid://x#cells/left`.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ResourcePath {
    /// The asset.
    pub uid: Uid,
    /// What of it, after the `#`: an output, and a member of it.
    pub suffix: Option<RelPath>,
}

impl ResourcePath {
    /// An asset itself.
    #[must_use]
    pub fn of(uid: Uid) -> ResourcePath {
        ResourcePath { uid, suffix: None }
    }

    /// What of an asset `suffix` names.
    #[must_use]
    pub fn within(uid: Uid, suffix: RelPath) -> ResourcePath {
        ResourcePath { uid, suffix: (!suffix.is_root()).then_some(suffix) }
    }

    /// `text` as a resource path.
    ///
    /// # Errors
    /// It is no uid, or what follows its `#` is empty or no path.
    pub fn new(text: &str) -> Result<ResourcePath, PathError> {
        let (uid, suffix) = match text.split_once('#') {
            Some((_, "")) => return Err(PathError::new(text, Why::EmptySuffix)),
            Some((uid, suffix)) => (uid, Some(suffix)),
            None => (text, None),
        };
        let uid = Uid::parse(uid).ok_or_else(|| PathError::new(text, Why::NoUid))?;
        let suffix = suffix.map(RelPath::new).transpose().map_err(|e| PathError::new(text, e.why))?;
        Ok(ResourcePath { uid, suffix })
    }
}

impl fmt::Display for ResourcePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.suffix {
            Some(suffix) => write!(f, "{}#{suffix}", self.uid),
            None => write!(f, "{}", self.uid),
        }
    }
}

impl std::str::FromStr for Uid {
    type Err = PathError;
    fn from_str(text: &str) -> Result<Uid, PathError> {
        Uid::new(text)
    }
}

impl std::str::FromStr for ResourcePath {
    type Err = PathError;
    fn from_str(text: &str) -> Result<ResourcePath, PathError> {
        ResourcePath::new(text)
    }
}

crate::serde_as_text!(Uid);
crate::serde_as_text!(ResourcePath);

#[cfg(test)]
mod tests {
    use super::{ResourcePath, Uid};
    use crate::{RelPath, Why};

    #[test]
    fn a_uid_is_its_text_both_ways() {
        for text in ["uid://vdx31ffeypvs", "uid://0", "uid://1y2p0ij32e8e7"] {
            assert_eq!(Uid::new(text).unwrap().to_string(), text);
        }
        assert_eq!(Uid::new("uid://1y2p0ij32e8e7").unwrap().id(), i64::MAX);
        for bad in ["uid://1y2p0ij32e8e8", "uid://ABC", "uid://", "vdx31ffeypvs", "uid://a-b"] {
            assert_eq!(Uid::new(bad).unwrap_err().why, Why::NoUid, "{bad}");
        }
        assert_eq!(Uid::from_id(-1), None);
    }

    #[test]
    fn a_resource_path_is_a_uid_and_what_of_it() {
        let asset = ResourcePath::new("uid://abc").unwrap();
        assert_eq!(asset.suffix, None);
        let member = ResourcePath::new("uid://abc#cells/left").unwrap();
        assert_eq!(member.uid, asset.uid);
        assert_eq!(member.suffix, Some(RelPath::new("cells/left").unwrap()));
        assert_eq!(member.to_string(), "uid://abc#cells/left");
        assert_eq!(ResourcePath::within(asset.uid, RelPath::root()), asset);
        assert_eq!(ResourcePath::new("uid://abc#").unwrap_err().why, Why::EmptySuffix);
        assert_eq!(ResourcePath::new("uid://abc#a/../b").unwrap_err().why, Why::Dot);
        assert_eq!(ResourcePath::new("res://abc#a").unwrap_err().why, Why::NoUid);
        let read: ResourcePath = serde_json::from_str("\"uid://abc#objects\"").unwrap();
        assert_eq!(serde_json::to_string(&read).unwrap(), "\"uid://abc#objects\"");
    }
}
