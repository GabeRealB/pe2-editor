//! What a project names an asset by: its uid, how it is asked for, and what of it an address names.

use std::fmt;
use std::sync::Arc;

use crate::error::{PathError, Why};

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

impl Uid {
    /// Its digits alone, without `uid://`: what names it where it is stored by its uid (a record's file).
    #[must_use]
    pub fn digits(self) -> String {
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
        digits[at..].iter().map(|&d| char::from(d)).collect()
    }
}

impl fmt::Display for Uid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(UID_PREFIX)?;
        f.write_str(&self.digits())
    }
}

/// The checks of what follows a uid, written to run as the program is compiled too, for the addresses it holds.
mod check {
    use crate::error::Why;

    /// The character at `bytes[at]`, of UTF-8.
    const fn at(bytes: &[u8], at: usize) -> char {
        let b = bytes[at] as u32;
        let (code, more) = if b < 0x80 {
            (b, 0)
        } else if b >= 0xf0 {
            (b & 0x07, 3)
        } else if b >= 0xe0 {
            (b & 0x0f, 2)
        } else {
            (b & 0x1f, 1)
        };
        let mut code = code;
        let mut i = 1;
        while i <= more && at + i < bytes.len() {
            code = (code << 6) | (bytes[at + i] as u32 & 0x3f);
            i += 1;
        }
        match char::from_u32(code) {
            Some(c) => c,
            None => char::REPLACEMENT_CHARACTER,
        }
    }

    /// Why `bytes[start..end]` is no part of an address, or key of a query, if it is not one: ASCII letters, digits and
    /// `_`, not starting with `_`.
    pub(super) const fn word(bytes: &[u8], start: usize, end: usize) -> Option<Why> {
        if start == end {
            return Some(Why::Empty);
        }
        let mut i = start;
        while i < end {
            let b = bytes[i];
            if !(b.is_ascii_alphanumeric() || b == b'_') {
                return Some(Why::Word(at(bytes, i)));
            }
            i += 1;
        }
        if bytes[start] == b'_' { Some(Why::Underscore) } else { None }
    }

    /// Why `bytes[start..end]` is no address, if it is not one: parts joined by `/`.
    pub(super) const fn address(bytes: &[u8], start: usize, end: usize) -> Option<Why> {
        let mut from = start;
        let mut i = start;
        while i <= end {
            if i == end || bytes[i] == b'/' {
                if let Some(why) = word(bytes, from, i) {
                    return Some(why);
                }
                from = i + 1;
            }
            i += 1;
        }
        None
    }

    /// Whether `bytes[a..a_end]` and `bytes[b..b_end]` are one text.
    const fn same(bytes: &[u8], a: usize, a_end: usize, b: usize, b_end: usize) -> bool {
        if a_end - a != b_end - b {
            return false;
        }
        let mut i = 0;
        while a + i < a_end {
            if bytes[a + i] != bytes[b + i] {
                return false;
            }
            i += 1;
        }
        true
    }

    /// Where the key of the pair starting at `start` ends: its `=`, or `end` where it has none.
    const fn key_end(bytes: &[u8], start: usize, end: usize) -> usize {
        let mut i = start;
        while i < end && bytes[i] != b'=' && bytes[i] != b'&' {
            i += 1;
        }
        i
    }

    /// Why `bytes[start..end]` is no query, if it is not one: `key=value` pairs joined by `&`, each key once, a value
    /// ASCII letters, digits, `_`, `-`, `.` and `/` (a path within what is asked: a prefab's `member=mist/body`).
    pub(super) const fn query(bytes: &[u8], start: usize, end: usize) -> Option<Why> {
        let mut pair = start;
        while pair <= end {
            let key = key_end(bytes, pair, end);
            if key == end || bytes[key] != b'=' {
                return Some(Why::NoValue);
            }
            if let Some(why) = word(bytes, pair, key) {
                return Some(why);
            }
            let mut value = key + 1;
            while value < end && bytes[value] != b'&' {
                let b = bytes[value];
                if !(b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'/')) {
                    return Some(Why::Word(at(bytes, value)));
                }
                value += 1;
            }
            if value == key + 1 {
                return Some(Why::Empty);
            }
            // A key named before.
            let mut before = start;
            while before < pair {
                let known = key_end(bytes, before, end);
                if same(bytes, before, known, pair, key) {
                    return Some(Why::Repeated);
                }
                while before < end && bytes[before] != b'&' {
                    before += 1;
                }
                before += 1;
            }
            pair = value + 1;
        }
        None
    }

    /// Where `bytes` has its `?`, or its length where it has none.
    pub(super) const fn question(bytes: &[u8]) -> usize {
        let mut i = 0;
        while i < bytes.len() && bytes[i] != b'?' {
            i += 1;
        }
        i
    }

    /// Why `text` is no output asked for, if it is not one: an address, and a query after `?`.
    pub(super) const fn asked(text: &str) -> Option<Why> {
        let bytes = text.as_bytes();
        let q = question(bytes);
        if let Some(why) = address(bytes, 0, q) {
            return Some(why);
        }
        if q == bytes.len() {
            return None;
        }
        if q + 1 == bytes.len() {
            return Some(Why::EmptySuffix);
        }
        query(bytes, q + 1, bytes.len())
    }
}

/// Text checked as one of these, shared or borrowed as `str` is.
macro_rules! over_text {
    ($type:ident, $check:expr, $what:literal) => {
        impl $type {
            /// A checked text as this.
            const fn of(text: &str) -> &$type {
                // SAFETY: the type is `str` alone, as `repr(transparent)` lays it out.
                unsafe { &*(std::ptr::from_ref::<str>(text) as *const $type) }
            }

            /// `text` as this.
            ///
            /// # Errors
            /// It is none, as the type says.
            pub fn new(text: &str) -> Result<&$type, PathError> {
                match $check(text) {
                    Some(why) => Err(PathError::new(text, why)),
                    None => Ok($type::of(text)),
                }
            }

            /// A text the program holds as this, checked as it is compiled.
            ///
            /// # Panics
            /// `text` is none.
            #[must_use]
            pub const fn constant(text: &'static str) -> &'static $type {
                assert!($check(text).is_none(), $what);
                $type::of(text)
            }

            /// Its text.
            #[must_use]
            pub const fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $type {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl fmt::Debug for $type {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Debug::fmt(&self.0, f)
            }
        }

        impl AsRef<$type> for $type {
            fn as_ref(&self) -> &$type {
                self
            }
        }

        impl ToOwned for $type {
            type Owned = Arc<$type>;
            fn to_owned(&self) -> Arc<$type> {
                Arc::from(self)
            }
        }

        impl From<&$type> for Arc<$type> {
            fn from(checked: &$type) -> Arc<$type> {
                let shared: Arc<str> = Arc::from(checked.as_str());
                // SAFETY: the type is `str` alone (`repr(transparent)`): the pointer is to what the `Arc<str>` held.
                unsafe { Arc::from_raw(Arc::into_raw(shared) as *const $type) }
            }
        }

        impl From<&$type> for Box<$type> {
            fn from(checked: &$type) -> Box<$type> {
                let boxed: Box<str> = checked.as_str().into();
                // SAFETY: the type is `str` alone (`repr(transparent)`), and the text is checked.
                unsafe { Box::from_raw(Box::into_raw(boxed) as *mut $type) }
            }
        }

        impl Clone for Box<$type> {
            fn clone(&self) -> Box<$type> {
                Box::from(&**self)
            }
        }

        impl serde::Serialize for $type {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> serde::Deserialize<'de> for Box<$type> {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Box<$type>, D::Error> {
                let text = <std::borrow::Cow<'de, str> as serde::Deserialize>::deserialize(deserializer)?;
                $type::new(&text).map(Box::from).map_err(serde::de::Error::custom)
            }
        }
    };
}

/// What of an asset a reference names, after its uid: an output, and a path within it - `objects`, `cells/left`,
/// `outfit/mist/body`. Each part is ASCII letters, digits and `_`, none starting with `_`: a part reaches C as part of
/// an identifier, the parts joined by `_`, so a leading one could not be told from what joins them. Compared as
/// written: exactly.
#[derive(PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct Address(str);

const fn address_refused(text: &str) -> Option<Why> {
    check::address(text.as_bytes(), 0, text.len())
}

over_text!(Address, address_refused, "no address: parts of ASCII letters, digits and `_` joined by `/`");

impl Address {
    /// The output it names.
    #[must_use]
    pub fn output(&self) -> &str {
        self.0.split_once('/').map_or(&self.0, |(output, _)| output)
    }

    /// The path within the output it names, where it names one: `left` of `cells/left`, `mist/body` of
    /// `outfit/mist/body`.
    #[must_use]
    pub fn member(&self) -> Option<&str> {
        self.0.split_once('/').map(|(_, member)| member)
    }

    /// Its parts, the output first.
    pub fn parts(&self) -> impl Iterator<Item = &str> {
        self.0.split('/')
    }

    /// It asked for as it is.
    #[must_use]
    pub const fn asked(&self) -> &Asked {
        Asked::of(&self.0)
    }
}

/// How an output is asked for, after a reference's `?`: keys with their values, in the order written, each key once -
/// `palette=default`. A key is what an address's part is; a value ASCII letters, digits, `_`, `-`, `.` and `/`. Which keys
/// an output takes, the output declares. Empty: nothing is asked.
#[derive(PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct Query(str);

const fn query_refused(text: &str) -> Option<Why> {
    if text.is_empty() { None } else { check::query(text.as_bytes(), 0, text.len()) }
}

over_text!(Query, query_refused, "no query: `key=value` pairs joined by `&`");

impl Query {
    /// Nothing asked.
    pub const NONE: &Query = Query::of("");

    /// A key's value, where it has one.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.pairs().find(|(known, _)| *known == key).map(|(_, value)| value)
    }

    /// Its keys with their values, in order.
    pub fn pairs(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.split('&').filter(|pair| !pair.is_empty()).filter_map(|pair| pair.split_once('='))
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// An output asked for: its address, and how it is asked for - `cells/body?palette=default`, a reference's path and
/// query without its uid; `native`, an output asked for as it is.
#[derive(PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct Asked(str);

over_text!(Asked, check::asked, "no output asked for: an address, and a query after `?`");

impl Asked {
    /// The output it names, and the path within it.
    #[must_use]
    pub fn address(&self) -> &Address {
        Address::of(self.0.split_once('?').map_or(&self.0, |(address, _)| address))
    }

    /// How it is asked for: empty where nothing is said.
    #[must_use]
    pub fn query(&self) -> &Query {
        Query::of(self.0.split_once('?').map_or("", |(_, query)| query))
    }

    /// An address asked for with a query.
    #[must_use]
    pub fn with(address: &Address, query: &Query) -> Arc<Asked> {
        if query.is_empty() {
            return Arc::from(address.asked());
        }
        Arc::from(Asked::of(&format!("{address}?{query}")))
    }
}

/// What a reference names, written as a URL is: an asset by its uid; an output of it as the URL's path - the groups it is
/// in, then its name; how that output is made as its query; and a part of what it makes as its fragment -
/// `uid://x`, `uid://x/objects`, `uid://x/cells/body?palette=default`, `uid://x/tokens?symbol=gBody#buffer`.
/// A query asks something of an output, which declares its keys: one with no address is none. A fragment is taken by
/// whoever reads what is made, a step at a time (a table's key, a list's index); it never changes what is made.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ResourcePath {
    /// The asset.
    pub uid: Uid,
    /// What of it: an output, and how that is asked for. None: the asset itself.
    pub asked: Option<Arc<Asked>>,
    /// The part of what it names that is read, a step at a time. None: all of it.
    pub fragment: Option<Arc<Address>>,
}

impl ResourcePath {
    /// An asset itself.
    #[must_use]
    pub fn of(uid: Uid) -> ResourcePath {
        ResourcePath { uid, asked: None, fragment: None }
    }

    /// What of an asset an address names, asked for as it is.
    #[must_use]
    pub fn at(uid: Uid, asked: &Asked) -> ResourcePath {
        ResourcePath { uid, asked: Some(Arc::from(asked)), fragment: None }
    }

    /// The output it names, where it names one.
    #[must_use]
    pub fn address(&self) -> Option<&Address> {
        self.asked.as_deref().map(Asked::address)
    }

    /// How it asks for its output: empty where it names none, or asks for it as it is.
    #[must_use]
    pub fn query(&self) -> &Query {
        self.asked.as_deref().map_or(Query::NONE, Asked::query)
    }

    /// `text` as a resource path.
    ///
    /// # Errors
    /// It is no uid; its address, query or fragment is empty, or none as `Address` and `Query` refuse; or it asks with
    /// a query for no output.
    pub fn new(text: &str) -> Result<ResourcePath, PathError> {
        let refused = |e: PathError| PathError::new(text, e.why);
        let (named, fragment) = match text.split_once('#') {
            Some((_, "")) => return Err(PathError::new(text, Why::EmptySuffix)),
            Some((named, fragment)) => (named, Some(Arc::from(Address::new(fragment).map_err(refused)?))),
            None => (text, None),
        };
        let digits = named.strip_prefix(UID_PREFIX).ok_or_else(|| PathError::new(text, Why::NoUid))?;
        let (uid, asked) = match digits.split_once('/') {
            Some((_, "")) => return Err(PathError::new(text, Why::EmptySuffix)),
            Some((uid, asked)) => (uid, Some(Arc::from(Asked::new(asked).map_err(refused)?))),
            None if digits.contains('?') => return Err(PathError::new(text, Why::NoOutput)),
            None => (digits, None),
        };
        let uid = Uid::parse(&format!("{UID_PREFIX}{uid}")).ok_or_else(|| PathError::new(text, Why::NoUid))?;
        Ok(ResourcePath { uid, asked, fragment })
    }
}

impl fmt::Display for ResourcePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.uid)?;
        if let Some(asked) = &self.asked {
            write!(f, "/{asked}")?;
        }
        if let Some(fragment) = &self.fragment {
            write!(f, "#{fragment}")?;
        }
        Ok(())
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
    use super::{Address, Asked, Query, ResourcePath, Uid};
    use crate::Why;

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
    fn a_resource_path_is_a_uid_what_of_it_and_how_that_is_asked_for() {
        let asset = ResourcePath::new("uid://abc").unwrap();
        assert_eq!((asset.address(), asset.query().is_empty()), (None, true));
        let member = ResourcePath::new("uid://abc/outfit/mist/body").unwrap();
        let address = member.address().unwrap();
        assert_eq!((member.uid, address.output(), address.member()), (asset.uid, "outfit", Some("mist/body")));
        let asked = ResourcePath::new("uid://abc/cells/body?palette=default&look=night-2").unwrap();
        let query = asked.query();
        assert_eq!(
            (query.get("palette"), query.get("look"), query.get("none")),
            (Some("default"), Some("night-2"), None)
        );
        assert_eq!(asked.to_string(), "uid://abc/cells/body?palette=default&look=night-2");
        assert_eq!(ResourcePath::at(asset.uid, Asked::constant("objects")).to_string(), "uid://abc/objects");
        assert_eq!(
            Asked::with(Address::constant("cells/body"), Query::new("palette=a").unwrap()).query().get("palette"),
            Some("a")
        );
        assert_eq!(&*Asked::with(Address::constant("native"), Query::NONE), Asked::constant("native"));
        for (bad, why) in [
            ("uid://abc/", Why::EmptySuffix),
            ("uid://abc/a?", Why::EmptySuffix),
            ("uid://abc?palette=default", Why::NoOutput),
            ("uid://abc/a//b", Why::Empty),
            ("uid://abc/_a/b", Why::Underscore),
            ("uid://abc/a/_b", Why::Underscore),
            ("uid://abc/cells/body@default", Why::Word('@')),
            ("uid://abc/palettes/weapon.battery", Why::Word('.')),
            ("uid://abc/a#", Why::EmptySuffix),
            ("uid://abc/a#b/_c", Why::Underscore),
            ("uid://abc/a#b.c", Why::Word('.')),
            ("uid://abc/a?palette", Why::NoValue),
            ("uid://abc/a?palette=", Why::Empty),
            ("uid://abc/a?a=1&a=2", Why::Repeated),
            ("uid://abc/a?a=x y", Why::Word(' ')),
            ("uid://abc/a?a=x?y", Why::Word('?')),
            ("res://abc/a", Why::NoUid),
        ] {
            assert_eq!(ResourcePath::new(bad).unwrap_err().why, why, "{bad}");
        }
        let part = ResourcePath::new("uid://abc/tokens?symbol=gBody#cells/prompt/v").unwrap();
        assert_eq!(part.fragment.as_deref().map(Address::as_str), Some("cells/prompt/v"));
        assert_eq!(part.query().get("symbol"), Some("gBody"));
        assert_eq!(part.to_string(), "uid://abc/tokens?symbol=gBody#cells/prompt/v");
        let of_asset = ResourcePath::new("uid://abc#joints/3").unwrap();
        assert_eq!((of_asset.asked, of_asset.fragment.as_deref().map(Address::as_str)), (None, Some("joints/3")));
        let read: ResourcePath = serde_json::from_str("\"uid://abc/objects?palette=default\"").unwrap();
        assert_eq!(serde_json::to_string(&read).unwrap(), "\"uid://abc/objects?palette=default\"");
    }
}
