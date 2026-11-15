//! A name, and names joined into a relative path.

use std::fmt;
use std::sync::Arc;

use crate::error::{PathError, Why};
use crate::fold;

/// One part of a path: portable, so that a project made on one system opens on another. It is not empty, `.` or `..`;
/// it holds no separator, none of `: * ? " < > |` and no control character; it is no name Windows keeps for a device
/// (`CON`, `PRN`, `AUX`, `NUL`, `COM1`-`COM9`, `LPT1`-`LPT9`, with or without an extension, in any case); and it does
/// not end in a dot or a space.
///
/// Unsized, as `str` is: held as `&Name`, a part of a path's text or a constant (`Name::constant`), and owned as
/// `Arc<Name>` (`ToOwned`), whose clone shares its text.
#[repr(transparent)]
pub struct Name(str);

/// Names joined by `/`, relative to what it is taken within; none at all is that itself (`RelPath::root`). Two are
/// ordered and compared as their texts, as the names they were before they were paths.
///
/// Unsized, as `str` is: borrowed as `&RelPath` - what is in it (`parent`) and what is left of it (`strip_prefix`) are
/// slices of its text - and owned as `Arc<RelPath>`.
#[repr(transparent)]
pub struct RelPath(str);

impl Name {
    /// A checked text as a name.
    pub(crate) const fn of(text: &str) -> &Name {
        // SAFETY: `Name` is `str` alone, as `repr(transparent)` lays it out.
        unsafe { &*(std::ptr::from_ref::<str>(text) as *const Name) }
    }
}

impl RelPath {
    /// A checked text as a path.
    pub(crate) const fn of(text: &str) -> &RelPath {
        // SAFETY: `RelPath` is `str` alone, as `repr(transparent)` lays it out.
        unsafe { &*(std::ptr::from_ref::<str>(text) as *const RelPath) }
    }

    /// A checked text as a shared path.
    pub(crate) fn shared(text: &str) -> Arc<RelPath> {
        let shared: Arc<str> = Arc::from(text);
        // SAFETY: `RelPath` is `str` alone (`repr(transparent)`): the pointer is to what the `Arc<str>` held.
        unsafe { Arc::from_raw(Arc::into_raw(shared) as *const RelPath) }
    }
}

/// Why `text` is no portable name, if it is not one.
pub(crate) const fn refused(text: &str) -> Option<Why> {
    refused_in(text.as_bytes(), 0, text.len())
}

/// Why the name in `bytes[start..end]` (whole characters of UTF-8) is not a portable one, if it is not. Written to be
/// run as the program is compiled too, for the names it holds.
const fn refused_in(bytes: &[u8], start: usize, end: usize) -> Option<Why> {
    let len = end - start;
    if len == 0 {
        return Some(Why::Empty);
    }
    if len > 255 {
        return Some(Why::Long);
    }
    if bytes[start] == b'.' && (len == 1 || (len == 2 && bytes[start + 1] == b'.')) {
        return Some(Why::Dot);
    }
    let mut at = start;
    while at < end {
        let b = bytes[at];
        if b < 0x20 || b == 0x7f {
            return Some(Why::Character(b as char));
        }
        if matches!(b, b'/' | b'\\' | b':' | b'*' | b'?' | b'"' | b'<' | b'>' | b'|') {
            return Some(Why::Character(b as char));
        }
        // U+0080 to U+009F, the other control characters: 0xC2 and 0x80 to 0x9F.
        if b == 0xc2 && at + 1 < end && bytes[at + 1] >= 0x80 && bytes[at + 1] <= 0x9f {
            return Some(Why::Character(match char::from_u32(bytes[at + 1] as u32) {
                Some(c) => c,
                None => '?',
            }));
        }
        if let Some(c) = direction(bytes, at, end) {
            return Some(Why::Direction(c));
        }
        at += 1;
    }
    let last = bytes[end - 1];
    if last == b'.' || last == b' ' {
        return Some(Why::Trailing(last as char));
    }
    if reserved(bytes, start, end) { Some(Why::Reserved) } else { None }
}

/// The character at `bytes[at..]` that turns which way text reads, if it is one: the marks and embeddings, overrides
/// and isolates of bidirectional text (U+061C, U+200E, U+200F, U+202A to U+202E, U+2066 to U+2069).
const fn direction(bytes: &[u8], at: usize, end: usize) -> Option<char> {
    let code = if bytes[at] == 0xd8 && at + 1 < end && bytes[at + 1] == 0x9c {
        0x061c
    } else if bytes[at] == 0xe2 && at + 2 < end && (bytes[at + 1] == 0x80 || bytes[at + 1] == 0x81) {
        0x2000 | ((bytes[at + 1] as u32 & 0x3f) << 6) | (bytes[at + 2] as u32 & 0x3f)
    } else {
        return None;
    };
    if matches!(code, 0x061c | 0x200e | 0x200f | 0x202a..=0x202e | 0x2066..=0x2069) {
        char::from_u32(code)
    } else {
        None
    }
}

/// Whether Windows keeps a name for a device: its part before the first dot, without the spaces ending it - `CON`,
/// `PRN`, `AUX`, `NUL`, `CONIN$`, `CONOUT$`, and `COM` or `LPT` with a digit from 1 to 9 or a superscript one, two or
/// three after it.
const fn reserved(bytes: &[u8], start: usize, end: usize) -> bool {
    let mut stem_end = start;
    while stem_end < end && bytes[stem_end] != b'.' {
        stem_end += 1;
    }
    while stem_end > start && bytes[stem_end - 1] == b' ' {
        stem_end -= 1;
    }
    let len = stem_end - start;
    let stem = (bytes, start, len);
    if is(stem, b"CON") || is(stem, b"PRN") || is(stem, b"AUX") || is(stem, b"NUL") {
        return true;
    }
    if is(stem, b"CONIN$") || is(stem, b"CONOUT$") {
        return true;
    }
    if len < 4 {
        return false;
    }
    let (a, b, c) = (
        bytes[start].to_ascii_uppercase(),
        bytes[start + 1].to_ascii_uppercase(),
        bytes[start + 2].to_ascii_uppercase(),
    );
    if !matches!((a, b, c), (b'C', b'O', b'M') | (b'L', b'P', b'T')) {
        return false;
    }
    let digit = bytes[start + 3];
    // `¹`, `²` and `³`: 0xC2 and 0xB9, 0xB2 or 0xB3.
    (len == 4 && digit >= b'1' && digit <= b'9')
        || (len == 5 && digit == 0xc2 && matches!(bytes[start + 4], 0xb9 | 0xb2 | 0xb3))
}

/// Whether a name's stem (`bytes[start..start + len]`) is `word`, in any case.
const fn is((bytes, start, len): (&[u8], usize, usize), word: &[u8]) -> bool {
    if word.len() != len {
        return false;
    }
    let mut at = 0;
    while at < len {
        if bytes[start + at].to_ascii_uppercase() != word[at] {
            return false;
        }
        at += 1;
    }
    true
}

/// Why `text` is no relative path, if it is not one: none of its parts may be refused, the empty text being the root.
const fn refused_path(text: &str) -> Option<Why> {
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let mut start = 0;
    let mut at = 0;
    while at <= bytes.len() {
        if at == bytes.len() || bytes[at] == b'/' {
            if let Some(why) = refused_in(bytes, start, at) {
                return Some(why);
            }
            start = at + 1;
        }
        at += 1;
    }
    None
}

/// What names a place within another, name by name: a [`Name`], a [`RelPath`], a [`VfsPath`](crate::VfsPath).
pub trait Names {
    /// Its names, in order.
    fn names(&self) -> impl Iterator<Item = &str>;
}

impl Names for Name {
    fn names(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.as_str())
    }
}

impl Names for RelPath {
    fn names(&self) -> impl Iterator<Item = &str> {
        self.parts().map(Name::as_str)
    }
}

impl<T: Names + ?Sized> Names for &T {
    fn names(&self) -> impl Iterator<Item = &str> {
        (**self).names()
    }
}

impl<T: Names + ?Sized> Names for Arc<T> {
    fn names(&self) -> impl Iterator<Item = &str> {
        (**self).names()
    }
}

impl Name {
    /// `text` as a name.
    ///
    /// # Errors
    /// It is no portable name.
    pub fn new(text: &str) -> Result<&Name, PathError> {
        match refused(text) {
            Some(why) => Err(PathError::new(text, why)),
            None => Ok(Name::of(text)),
        }
    }

    /// A name the program holds: checked as it is compiled, where it is a constant (`const X: &Name = ...`), and
    /// refused by failing to.
    ///
    /// # Panics
    /// `text` is no portable name.
    #[must_use]
    pub const fn constant(text: &'static str) -> &'static Name {
        assert!(refused(text).is_none(), "no portable name");
        Name::of(text)
    }

    /// Its text.
    #[must_use]
    pub const fn as_str(&self) -> &str {
        &self.0
    }

    /// It as a path of one name.
    #[must_use]
    pub const fn as_rel(&self) -> &RelPath {
        RelPath::of(&self.0)
    }

    /// A shared name as a shared path of one name.
    #[must_use]
    pub fn into_rel(name: Arc<Name>) -> Arc<RelPath> {
        // SAFETY: `Name` and `RelPath` are both `str` alone (`repr(transparent)`), and a name is a path of one name.
        unsafe { Arc::from_raw(Arc::into_raw(name) as *const RelPath) }
    }
}

impl RelPath {
    /// The path of nothing: what it is taken within itself.
    #[must_use]
    pub const fn root() -> &'static RelPath {
        RelPath::of("")
    }

    /// `text` as a path: names joined by `/`, none for the empty text.
    ///
    /// # Errors
    /// A part of it is no portable name, or empty (`a//b`, a leading or a trailing `/`).
    pub fn new(text: &str) -> Result<&RelPath, PathError> {
        match refused_path(text) {
            Some(why) => Err(PathError::new(text, why)),
            None => Ok(RelPath::of(text)),
        }
    }

    /// A path the program holds: checked as it is compiled, where it is a constant (`const X: &RelPath = ...`), and
    /// refused by failing to.
    ///
    /// # Panics
    /// `text` is no relative path.
    #[must_use]
    pub const fn constant(text: &'static str) -> &'static RelPath {
        assert!(refused_path(text).is_none(), "no relative path of portable names");
        RelPath::of(text)
    }

    /// Where a store keeps what it keeps by a key (a digest): in a folder of the key's first two characters, as
    /// `ab/abcdef`, with `suffix` after the key's name (`.cbor`, or nothing).
    ///
    /// # Errors
    /// The key with its suffix is no portable name.
    pub fn sharded(key: &str, suffix: &str) -> Result<Arc<RelPath>, PathError> {
        let file = format!("{key}{suffix}");
        let shard = key.get(..2).unwrap_or(key);
        for name in [shard, file.as_str()] {
            if let Some(why) = refused(name) {
                return Err(PathError::new(&file, why));
            }
        }
        Ok(RelPath::shared(&format!("{shard}/{file}")))
    }

    /// Whether it names nothing but what it is taken within.
    #[must_use]
    pub const fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    /// Its names, in order: slices of its text, each checked as [`Name`] checks one when it was.
    pub fn parts(&self) -> impl DoubleEndedIterator<Item = &Name> {
        self.0.split('/').filter(|part| !part.is_empty()).map(Name::of)
    }

    /// How many names it has.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.parts().count()
    }

    /// Its last name's text, unless it is the root: what it is shown as.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.last().map(Name::as_str)
    }

    /// Its last name, unless it is the root.
    #[must_use]
    pub fn last(&self) -> Option<&Name> {
        self.parts().next_back()
    }

    /// What it is in, unless it is the root: the start of its text.
    #[must_use]
    pub fn parent(&self) -> Option<&RelPath> {
        if self.is_root() {
            return None;
        }
        Some(match self.0.rfind('/') {
            Some(at) => RelPath::of(&self.0[..at]),
            None => RelPath::root(),
        })
    }

    /// A name within it.
    #[must_use]
    pub fn join(&self, name: &Name) -> Arc<RelPath> {
        self.join_text(name.as_str())
    }

    /// A path within it.
    #[must_use]
    pub fn join_path(&self, path: &RelPath) -> Arc<RelPath> {
        self.join_text(path.as_str())
    }

    fn join_text(&self, text: &str) -> Arc<RelPath> {
        match (self.is_root(), text.is_empty()) {
            (_, true) => self.to_owned(),
            (true, false) => RelPath::shared(text),
            (false, false) => RelPath::shared(&format!("{}/{text}", self.as_str())),
        }
    }

    /// Whether it is `prefix` or within it, name by name.
    #[must_use]
    pub fn starts_with(&self, prefix: &RelPath) -> bool {
        self.strip_prefix(prefix).is_some()
    }

    /// What of it is within `prefix`, if it is `prefix` or within it - name by name, each as names compare (`fold`):
    /// the end of its text.
    #[must_use]
    pub fn strip_prefix(&self, prefix: &RelPath) -> Option<&RelPath> {
        let mut rest = self.as_str();
        for name in prefix.parts() {
            let (first, after) = rest.split_once('/').unwrap_or((rest, ""));
            if first.is_empty() || !fold::same(first, name.as_str()) {
                return None;
            }
            rest = after;
        }
        Some(RelPath::of(rest))
    }

    /// Its text, names joined by `/`: for writing it out where a path leaves the program as text.
    #[must_use]
    pub const fn as_str(&self) -> &str {
        &self.0
    }
}

/// A name is a place within a folder as any system takes one: joined onto a path of the system's, it stays within it.
impl AsRef<std::path::Path> for Name {
    fn as_ref(&self) -> &std::path::Path {
        std::path::Path::new(&self.0)
    }
}

impl ToOwned for Name {
    type Owned = Arc<Name>;
    fn to_owned(&self) -> Arc<Name> {
        Arc::from(self)
    }
}

impl ToOwned for RelPath {
    type Owned = Arc<RelPath>;
    fn to_owned(&self) -> Arc<RelPath> {
        Arc::from(self)
    }
}

impl From<&Name> for Arc<Name> {
    fn from(name: &Name) -> Arc<Name> {
        let shared = RelPath::shared(name.as_str());
        // SAFETY: `Name` and `RelPath` are both `str` alone (`repr(transparent)`), and the text is a checked name.
        unsafe { Arc::from_raw(Arc::into_raw(shared) as *const Name) }
    }
}

impl From<&RelPath> for Arc<RelPath> {
    fn from(path: &RelPath) -> Arc<RelPath> {
        RelPath::shared(path.as_str())
    }
}

impl<'a> From<&'a Name> for &'a RelPath {
    fn from(name: &'a Name) -> &'a RelPath {
        name.as_rel()
    }
}

impl PartialEq<Name> for RelPath {
    fn eq(&self, other: &Name) -> bool {
        fold::same(self.as_str(), other.as_str())
    }
}

impl PartialEq<&Name> for Arc<Name> {
    fn eq(&self, other: &&Name) -> bool {
        **self == **other
    }
}

impl PartialEq<&RelPath> for Arc<RelPath> {
    fn eq(&self, other: &&RelPath) -> bool {
        **self == **other
    }
}

impl PartialEq<&Name> for Arc<RelPath> {
    fn eq(&self, other: &&Name) -> bool {
        **self == **other
    }
}

impl PartialEq<Name> for Arc<Name> {
    fn eq(&self, other: &Name) -> bool {
        **self == *other
    }
}

impl PartialEq<RelPath> for Arc<RelPath> {
    fn eq(&self, other: &RelPath) -> bool {
        **self == *other
    }
}

impl PartialEq<Name> for Arc<RelPath> {
    fn eq(&self, other: &Name) -> bool {
        **self == *other
    }
}

impl Default for &RelPath {
    fn default() -> Self {
        RelPath::root()
    }
}

/// Two names are one where they fold alike (`fold`): what any system takes as one name.
impl PartialEq for Name {
    fn eq(&self, other: &Name) -> bool {
        fold::same(self.as_str(), other.as_str())
    }
}

impl Eq for Name {}

impl PartialOrd for Name {
    fn partial_cmp(&self, other: &Name) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Names are ordered as their folds.
impl Ord for Name {
    fn cmp(&self, other: &Name) -> std::cmp::Ordering {
        fold::order(self.as_str(), other.as_str())
    }
}

impl std::hash::Hash for Name {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        fold::hash(self.as_str(), state);
    }
}

/// Two paths are one where their names are, one by one: their texts fold alike, as a fold never makes or takes a `/`.
impl PartialEq for RelPath {
    fn eq(&self, other: &RelPath) -> bool {
        fold::same(self.as_str(), other.as_str())
    }
}

impl Eq for RelPath {}

impl PartialOrd for RelPath {
    fn partial_cmp(&self, other: &RelPath) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Paths are ordered name by name, a path before what is within it: what is within a folder follows it, before
/// anything else is.
impl Ord for RelPath {
    fn cmp(&self, other: &RelPath) -> std::cmp::Ordering {
        self.parts().cmp(other.parts())
    }
}

impl std::hash::Hash for RelPath {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        fold::hash(self.as_str(), state);
    }
}

impl PartialEq<str> for Name {
    fn eq(&self, other: &str) -> bool {
        fold::same(self.as_str(), other)
    }
}

impl PartialEq<&str> for Name {
    fn eq(&self, other: &&str) -> bool {
        fold::same(self.as_str(), other)
    }
}

impl PartialEq<Name> for str {
    fn eq(&self, other: &Name) -> bool {
        fold::same(self, other.as_str())
    }
}

impl PartialEq<Name> for &str {
    fn eq(&self, other: &Name) -> bool {
        fold::same(self, other.as_str())
    }
}

impl PartialEq<Name> for String {
    fn eq(&self, other: &Name) -> bool {
        fold::same(self.as_str(), other.as_str())
    }
}

impl PartialEq<Name> for &String {
    fn eq(&self, other: &Name) -> bool {
        fold::same(self.as_str(), other.as_str())
    }
}

impl PartialEq<String> for Name {
    fn eq(&self, other: &String) -> bool {
        fold::same(self.as_str(), other.as_str())
    }
}

impl PartialEq<str> for RelPath {
    fn eq(&self, other: &str) -> bool {
        fold::same(self.as_str(), other)
    }
}

impl PartialEq<&str> for RelPath {
    fn eq(&self, other: &&str) -> bool {
        fold::same(self.as_str(), other)
    }
}

impl PartialEq<Name> for &RelPath {
    fn eq(&self, other: &Name) -> bool {
        fold::same(self.as_str(), other.as_str())
    }
}

impl PartialEq<std::ffi::OsStr> for Name {
    fn eq(&self, other: &std::ffi::OsStr) -> bool {
        other.to_str().is_some_and(|text| fold::same(self.as_str(), text))
    }
}

impl PartialEq<Name> for &std::ffi::OsStr {
    fn eq(&self, other: &Name) -> bool {
        self.to_str().is_some_and(|text| fold::same(text, other.as_str()))
    }
}

impl PartialEq<Name> for std::ffi::OsString {
    fn eq(&self, other: &Name) -> bool {
        self.to_str().is_some_and(|text| fold::same(text, other.as_str()))
    }
}

impl PartialEq<&Name> for std::ffi::OsStr {
    fn eq(&self, other: &&Name) -> bool {
        self.to_str().is_some_and(|text| fold::same(text, other.as_str()))
    }
}

impl PartialEq<&Name> for std::ffi::OsString {
    fn eq(&self, other: &&Name) -> bool {
        self.to_str().is_some_and(|text| fold::same(text, other.as_str()))
    }
}

impl PartialEq<Name> for std::ffi::OsStr {
    fn eq(&self, other: &Name) -> bool {
        self.to_str().is_some_and(|text| fold::same(text, other.as_str()))
    }
}

impl fmt::Display for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Debug for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Debug for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl serde::Serialize for Name {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl serde::Serialize for RelPath {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for Box<Name> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Box<Name>, D::Error> {
        let text = <std::borrow::Cow<'de, str> as serde::Deserialize>::deserialize(deserializer)?;
        let name = Name::new(&text).map_err(serde::de::Error::custom)?;
        let boxed: Box<str> = name.as_str().into();
        // SAFETY: `Name` is `str` alone (`repr(transparent)`), and the text is checked.
        Ok(unsafe { Box::from_raw(Box::into_raw(boxed) as *mut Name) })
    }
}

impl<'de> serde::Deserialize<'de> for Box<RelPath> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Box<RelPath>, D::Error> {
        let text = <std::borrow::Cow<'de, str> as serde::Deserialize>::deserialize(deserializer)?;
        let path = RelPath::new(&text).map_err(serde::de::Error::custom)?;
        let boxed: Box<str> = path.as_str().into();
        // SAFETY: `RelPath` is `str` alone (`repr(transparent)`), and the text is checked.
        Ok(unsafe { Box::from_raw(Box::into_raw(boxed) as *mut RelPath) })
    }
}

#[cfg(test)]
#[allow(
    clippy::disallowed_methods,
    reason = "a test's fixtures: the machine's time, places, files and environment, which nothing tested reads"
)]
mod tests {
    use super::{Name, RelPath};
    use crate::Why;
    use std::sync::Arc;

    const LAYERS: &Name = Name::constant(".layers");
    const NESTED: &RelPath = RelPath::constant(".layers/top");

    fn why(text: &str) -> Why {
        Name::new(text).expect_err(text).why
    }

    #[test]
    fn a_name_is_refused_where_some_system_could_not_hold_it() {
        for ok in ["a", "native.bin", "node.toml", "COM0", "LPT", "console", "aux_", "my aux", ".hidden", "a b", "ä"] {
            assert_eq!(Name::new(ok).map(Name::as_str), Ok(ok));
        }
        assert_eq!(why(""), Why::Empty);
        assert_eq!(why("."), Why::Dot);
        assert_eq!(why(".."), Why::Dot);
        for c in ['/', '\\', ':', '*', '?', '"', '<', '>', '|', '\0', '\n', '\u{7f}'] {
            assert_eq!(why(&format!("a{c}b")), Why::Character(c));
        }
        for reserved in ["CON", "con", "Nul.txt", "aux.tar.gz", "COM1", "lpt9.log", "PRN ", "NUL .x"] {
            assert!(matches!(why(reserved), Why::Reserved | Why::Trailing(' ')), "{reserved}");
        }
        assert_eq!(why("NUL .x"), Why::Reserved);
        assert_eq!(why("a."), Why::Trailing('.'));
        assert_eq!(why("a "), Why::Trailing(' '));
        assert_eq!(why("a\u{85}b"), Why::Character('\u{85}'));
        assert_eq!(LAYERS, Name::new(".layers").unwrap());
        assert_eq!(NESTED.parent().unwrap(), LAYERS.as_rel());
        let error = Name::new("a:b").unwrap_err().to_string();
        assert!(error.contains("`:`"), "{error}");
    }

    #[test]
    fn a_path_is_its_names_and_goes_by_them() {
        let path = RelPath::new("packages/weapons/m93r").unwrap();
        assert_eq!(path.parts().map(Name::as_str).collect::<Vec<_>>(), ["packages", "weapons", "m93r"]);
        assert_eq!(path.name(), Some("m93r"));
        assert_eq!(path.last(), Some(Name::new("m93r").unwrap()));
        assert_eq!(path.depth(), 3);
        assert_eq!(path.parent().unwrap().as_str(), "packages/weapons");
        assert_eq!(RelPath::new("a").unwrap().parent(), Some(RelPath::root()));
        assert_eq!(RelPath::root().parent(), None);
        assert!(RelPath::new("").unwrap().is_root());
        let weapons = RelPath::new("packages/weapons").unwrap();
        assert!(path.starts_with(weapons) && path.starts_with(RelPath::root()) && path.starts_with(path));
        assert!(!RelPath::new("packages/weaponsx").unwrap().starts_with(weapons));
        assert_eq!(path.strip_prefix(weapons).unwrap().as_str(), "m93r");
        assert_eq!(path.strip_prefix(path), Some(RelPath::root()));
        assert_eq!(&*weapons.join(Name::new("m93r").unwrap()), path);
        assert_eq!(&*RelPath::root().join_path(path), path);
        assert_eq!(&*path.join_path(RelPath::root()), path);
        for bad in ["/a", "a/", "a//b", "a/../b", "a/./b", "a\\b"] {
            assert!(RelPath::new(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn owned_it_shares_its_text() {
        let path: Arc<RelPath> = RelPath::new("a/b").unwrap().to_owned();
        let again = Arc::clone(&path);
        assert!(std::ptr::eq(path.as_str(), again.as_str()));
        let name: Arc<Name> = Name::new("b").unwrap().to_owned();
        assert_eq!(&*Name::into_rel(name), RelPath::new("b").unwrap());
        // What is in it, and what is left of it, are slices of its text: nothing is made.
        assert!(std::ptr::eq(path.parent().unwrap().as_str().as_ptr(), path.as_str().as_ptr()));
    }

    #[test]
    fn paths_order_name_by_name_what_is_within_a_folder_after_it() {
        let texts = ["a", "a/b", "a/b/c", "a b", "a-b", "ab", "b"];
        let mut paths: Vec<&RelPath> = texts.iter().rev().map(|t| RelPath::new(t).unwrap()).collect();
        paths.sort();
        assert_eq!(paths.iter().map(|p| p.as_str()).collect::<Vec<_>>(), texts);
        // By their folds: a name of another case is no other name.
        assert!(RelPath::new("B").unwrap() > RelPath::new("a/z").unwrap());
        assert_eq!(RelPath::new("A/B").unwrap(), RelPath::new("a/b").unwrap());
    }

    #[test]
    fn a_path_is_within_another_by_names_alike() {
        let path = RelPath::new("Rooms/Cafe\u{301}/a.txt").unwrap();
        let rest = path.strip_prefix(RelPath::new("rooms/CAFÉ").unwrap()).unwrap();
        assert_eq!(rest.as_str(), "a.txt");
        assert!(path.starts_with(RelPath::new("ROOMS").unwrap()));
        assert!(!path.starts_with(RelPath::new("room").unwrap()));
        assert_eq!(*Name::new("Notes").unwrap(), *"notes");
    }

    #[test]
    fn what_windows_keeps_and_what_reads_otherwise_than_it_is_are_refused() {
        for reserved in ["CONIN$", "conout$.txt", "COM\u{b9}", "lpt\u{b3}.log"] {
            assert_eq!(why(reserved), Why::Reserved, "{reserved}");
        }
        for ok in ["COM\u{b9}x", "CONIN", "console$"] {
            assert!(Name::new(ok).is_ok(), "{ok}");
        }
        for c in ['\u{202e}', '\u{200f}', '\u{2066}', '\u{61c}'] {
            assert_eq!(why(&format!("a{c}gnp.exe")), Why::Direction(c));
        }
        assert_eq!(why(&"a".repeat(256)), Why::Long);
        assert!(Name::new(&"\u{e9}".repeat(127)).is_ok());
    }

    #[test]
    fn as_text_it_is_checked_when_read() {
        let path = RelPath::new("a/b").unwrap();
        assert_eq!(serde_json::to_string(path).unwrap(), "\"a/b\"");
        assert_eq!(&*serde_json::from_str::<Box<RelPath>>("\"a/b\"").unwrap(), path);
        assert_eq!(&*serde_json::from_str::<Arc<RelPath>>("\"a/b\"").unwrap(), path);
        assert!(serde_json::from_str::<Box<RelPath>>("\"a/../b\"").is_err());
        assert!(serde_json::from_str::<Box<Name>>("\"a/b\"").is_err());
        assert_eq!(RelPath::sharded("abcdef", ".cbor").unwrap().as_str(), "ab/abcdef.cbor");
        assert!(RelPath::sharded("../x", "").is_err());
    }
}
