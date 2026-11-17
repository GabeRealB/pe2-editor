//! What a project names an asset by: its uid, how it is asked for, and what of it an address names.

use std::fmt;
use std::sync::Arc;

use crate::error::{PathError, Why};
use crate::vfs::{Tree, VfsPath};

/// An asset's uid: 64 bits, never 0 (none is no uid: `Option<Uid>` is as large as a uid). It is drawn once, when an
/// asset is first declared, and kept from then on, and compared as the number it is - never as text.
///
/// Written `uid://` and its 64 bits in Crockford's base 32, most significant digit first: 13 lowercase characters of
/// `0123456789abcdefghjkmnpqrstvwxyz`, padded with zeros (`uid://000000000000a`) - so that one uid is always written
/// alike, the order of the texts is that of the numbers, and no uid is a name some system keeps for itself. Read, it may
/// be written otherwise, as Crockford's decoding takes it: in either case, without the zeros before it, and with `i`
/// or `l` for `1` and `o` for `0` - `uid://A` is `uid://000000000000a`. What is no digit (`u` among them), more than 13
/// characters, more than 64 bits, and 0, are no uid.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Uid(std::num::NonZeroU64);

/// What a uid's text begins with.
pub(crate) const UID_PREFIX: &str = "uid://";
/// How many characters a uid is written in: 64 bits in fives.
pub const UID_DIGITS: usize = 13;
/// Crockford's base 32, written in lowercase: the characters a uid is written in.
pub const UID_ALPHABET: &str = "0123456789abcdefghjkmnpqrstvwxyz";
const DIGITS: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

/// The value of a character of a uid's digits, as Crockford's decoding reads it.
const fn digit(c: u8) -> Option<u64> {
    Some(match c.to_ascii_lowercase() {
        c @ b'0'..=b'9' => (c - b'0') as u64,
        b'o' => 0,
        b'i' | b'l' => 1,
        c @ b'a'..=b'h' => (c - b'a') as u64 + 10,
        b'j' => 18,
        b'k' => 19,
        b'm' => 20,
        b'n' => 21,
        c @ b'p'..=b't' => (c - b'p') as u64 + 22,
        c @ b'v'..=b'z' => (c - b'v') as u64 + 27,
        _ => return None,
    })
}

impl Uid {
    /// The uid a number is: none for 0.
    #[must_use]
    pub const fn from_u64(n: u64) -> Option<Uid> {
        match std::num::NonZeroU64::new(n) {
            Some(n) => Some(Uid(n)),
            None => None,
        }
    }

    /// The number it is.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }

    /// `text` as a uid.
    ///
    /// # Errors
    /// It is not `uid://` and the digits of one (see `Uid`).
    pub fn new(text: &str) -> Result<Uid, PathError> {
        text.strip_prefix(UID_PREFIX).and_then(Uid::of_digits).ok_or_else(|| PathError::new(text, Why::NoUid))
    }

    /// Its digits alone, without `uid://`, read as `new` reads them.
    #[must_use]
    pub fn of_digits(digits: &str) -> Option<Uid> {
        if digits.is_empty() || digits.len() > UID_DIGITS {
            return None;
        }
        let mut n: u64 = 0;
        for c in digits.bytes() {
            n = n.checked_mul(32)?.checked_add(digit(c)?)?;
        }
        Uid::from_u64(n)
    }

    /// Its digits alone, without `uid://`, as they are written: what names it where it is stored by its uid (a record's
    /// file).
    #[must_use]
    pub fn digits(self) -> UidDigits {
        let mut n = self.get();
        let mut digits = [0u8; UID_DIGITS];
        for at in (0..UID_DIGITS).rev() {
            digits[at] = DIGITS[(n % 32) as usize];
            n /= 32;
        }
        UidDigits(digits)
    }
}

/// A uid's digits as they are written: 13 characters of Crockford's base 32, held without being allocated.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct UidDigits([u8; UID_DIGITS]);

impl UidDigits {
    /// Its text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        // Characters of `DIGITS`: ASCII, so UTF-8.
        std::str::from_utf8(&self.0).unwrap_or_default()
    }
}

impl std::ops::Deref for UidDigits {
    type Target = str;
    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for UidDigits {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
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
        let uid = Uid::of_digits(uid).ok_or_else(|| PathError::new(text, Why::NoUid))?;
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

/// A uid's serde as the number it is, for what only programs read (`#[serde(with = "pe2_path::uid_number")]`): 0 is no
/// uid, and is refused.
pub mod uid_number {
    use super::Uid;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::BTreeMap;

    /// # Errors
    /// The serializer's.
    pub fn serialize<S: Serializer>(uid: &Uid, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(uid.get())
    }

    /// # Errors
    /// What is read is no number, or 0.
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Uid, D::Error> {
        Uid::from_u64(u64::deserialize(deserializer)?).ok_or_else(|| serde::de::Error::custom("0 is no uid"))
    }

    /// A map by uids, its keys as their numbers (`#[serde(with = "pe2_path::uid_number::keys")]`).
    pub mod keys {
        use super::{BTreeMap, Deserialize, Deserializer, Serialize, Serializer, Uid};

        /// # Errors
        /// The serializer's.
        pub fn serialize<V: Serialize, S: Serializer>(
            map: &BTreeMap<Uid, V>,
            serializer: S,
        ) -> Result<S::Ok, S::Error> {
            serializer.collect_map(map.iter().map(|(uid, value)| (uid.get(), value)))
        }

        /// # Errors
        /// A key is no number, or 0; a value is none.
        pub fn deserialize<'de, V: Deserialize<'de>, D: Deserializer<'de>>(
            deserializer: D,
        ) -> Result<BTreeMap<Uid, V>, D::Error> {
            let read = BTreeMap::<u64, V>::deserialize(deserializer)?;
            read.into_iter()
                .map(|(n, value)| Uid::from_u64(n).map(|uid| (uid, value)))
                .collect::<Option<_>>()
                .ok_or_else(|| serde::de::Error::custom("0 is no uid"))
        }
    }
}
crate::serde_as_text!(ResourcePath);

/// A node, as what reads it names it: by its uid, or by its path in the project's tree. Written as either
/// (`uid://abc`, `packages/aya/mist`); what of the node is read is said beside it, never in it.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Reference {
    Uid(Uid),
    Path(Arc<VfsPath<Tree>>),
}

impl Reference {
    /// `text` as a reference: a uid where it starts as one does, else a path in the tree.
    ///
    /// # Errors
    /// It is no uid alone, or no path.
    pub fn new(text: &str) -> Result<Reference, PathError> {
        if text.starts_with(UID_PREFIX) {
            return Uid::new(text).map(Reference::Uid);
        }
        VfsPath::<Tree>::new(text).map(|path| Reference::Path(Arc::from(path)))
    }

    /// The uid it names the node by, where it names it by one.
    #[must_use]
    pub fn uid(&self) -> Option<Uid> {
        match self {
            Reference::Uid(uid) => Some(*uid),
            Reference::Path(_) => None,
        }
    }

    /// The path it names the node by, where it names it by one.
    #[must_use]
    pub fn path(&self) -> Option<&VfsPath<Tree>> {
        match self {
            Reference::Uid(_) => None,
            Reference::Path(path) => Some(path),
        }
    }
}

impl fmt::Display for Reference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Reference::Uid(uid) => write!(f, "{uid}"),
            Reference::Path(path) => f.write_str(path.as_str()),
        }
    }
}

impl std::str::FromStr for Reference {
    type Err = PathError;
    fn from_str(text: &str) -> Result<Reference, PathError> {
        Reference::new(text)
    }
}

impl AsRef<Reference> for Reference {
    fn as_ref(&self) -> &Reference {
        self
    }
}

impl From<Uid> for Reference {
    fn from(uid: Uid) -> Reference {
        Reference::Uid(uid)
    }
}

impl From<&VfsPath<Tree>> for Reference {
    fn from(path: &VfsPath<Tree>) -> Reference {
        Reference::Path(Arc::from(path))
    }
}

impl From<Arc<VfsPath<Tree>>> for Reference {
    fn from(path: Arc<VfsPath<Tree>>) -> Reference {
        Reference::Path(path)
    }
}

crate::serde_as_text!(Reference);

#[cfg(test)]
#[allow(
    clippy::disallowed_methods,
    reason = "a test's fixtures: the machine's time, places, files and environment, which nothing tested reads"
)]
mod tests {
    use super::{Address, Asked, Query, Reference, ResourcePath, Uid};
    use crate::Why;

    #[test]
    fn a_uid_is_written_one_way_and_read_as_crockford_reads_it() {
        assert_eq!(Uid::new("uid://000000000000a").unwrap().get(), 10);
        assert_eq!(Uid::new("uid://zzzzzzzzzzzzz").ok(), None, "past 64 bits");
        assert_eq!(Uid::new("uid://fzzzzzzzzzzzz").unwrap().get(), u64::MAX);
        assert_eq!(Uid::from_u64(u64::MAX).unwrap().to_string(), "uid://fzzzzzzzzzzzz");
        // One uid, however it is written.
        let one = Uid::new("uid://0000000001jk1").unwrap();
        for text in ["uid://1jk1", "uid://1JK1", "uid://IJKL", "uid://ljki", "uid://000000001jK1"] {
            assert_eq!(Uid::new(text).unwrap(), one, "{text}");
        }
        assert_eq!(one.to_string(), "uid://0000000001jk1");
        assert_eq!(Uid::new("uid://o1").unwrap(), Uid::new("uid://01").unwrap());
        for bad in
            ["uid://0", "uid://0000000000000", "uid://u", "uid://", "uid://00000000000001", "a", "UID://a", "uid://a-b"]
        {
            assert_eq!(Uid::new(bad).unwrap_err().why, Why::NoUid, "{bad}");
        }
        assert_eq!(Uid::from_u64(0), None);
        // What none is costs nothing.
        assert_eq!(std::mem::size_of::<Option<Uid>>(), 8);
        // Written as the numbers order.
        let (a, b) = (Uid::from_u64(31).unwrap(), Uid::from_u64(32).unwrap());
        assert!(a < b && a.to_string() < b.to_string());
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
        assert_eq!(asked.to_string(), "uid://0000000000abc/cells/body?palette=default&look=night-2");
        assert_eq!(ResourcePath::at(asset.uid, Asked::constant("objects")).to_string(), "uid://0000000000abc/objects");
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
        let part = ResourcePath::new("uid://0000000000abc/tokens?symbol=gBody#cells/prompt/v").unwrap();
        assert_eq!(part.fragment.as_deref().map(Address::as_str), Some("cells/prompt/v"));
        assert_eq!(part.query().get("symbol"), Some("gBody"));
        assert_eq!(part.to_string(), "uid://0000000000abc/tokens?symbol=gBody#cells/prompt/v");
        let of_asset = ResourcePath::new("uid://abc#joints/3").unwrap();
        assert_eq!((of_asset.asked, of_asset.fragment.as_deref().map(Address::as_str)), (None, Some("joints/3")));
        let read: ResourcePath = serde_json::from_str("\"uid://abc/objects?palette=default\"").unwrap();
        assert_eq!(serde_json::to_string(&read).unwrap(), "\"uid://0000000000abc/objects?palette=default\"");
    }

    #[test]
    fn a_reference_names_a_node_by_its_uid_or_its_path() {
        let by_uid = Reference::new("uid://abc").unwrap();
        assert_eq!((by_uid.uid(), by_uid.path()), (Some(Uid::new("uid://abc").unwrap()), None));
        let by_path = Reference::new("packages/aya/mist").unwrap();
        assert_eq!(by_path.path().map(crate::VfsPath::as_str), Some("packages/aya/mist"));
        assert_eq!(
            (by_uid.to_string(), by_path.to_string()),
            ("uid://0000000000abc".to_string(), "packages/aya/mist".to_string())
        );
        assert_eq!(Reference::new("uid://abc/objects").unwrap_err().why, Why::NoUid);
        assert!(Reference::new("a/../b").is_err());
        let read: Reference = serde_json::from_str("\"uid://abc\"").unwrap();
        assert_eq!(read, by_uid);
    }
}
