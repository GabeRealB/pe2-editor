//! A name, and names joined into a relative path.

use std::fmt;
use std::sync::Arc;

use crate::error::{PathError, Why};

/// One part of a path: portable, so that a project made on one system opens on another. It is not empty, `.` or `..`;
/// it holds no separator, none of `: * ? " < > |` and no control character; it is no name Windows keeps for a device
/// (`CON`, `PRN`, `AUX`, `NUL`, `COM1`-`COM9`, `LPT1`-`LPT9`, with or without an extension, in any case); and it does
/// not end in a dot or a space. A name the program itself holds is a constant, checked as it is compiled
/// (`Name::constant`).
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Name(Text);

/// Names joined by `/`, relative to what it is taken within; none at all is that itself (`RelPath::root`). Two are
/// ordered and compared as their texts, as the names they were before they were paths.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct RelPath(Text);

/// A checked text: one the program holds from its start, or one made as it runs, shared by whatever holds it.
#[derive(Clone)]
enum Text {
    Static(&'static str),
    Shared(Arc<str>),
}

impl Text {
    fn as_str(&self) -> &str {
        match self {
            Text::Static(text) => text,
            Text::Shared(text) => text,
        }
    }
}

impl From<&str> for Text {
    fn from(text: &str) -> Text {
        Text::Shared(text.into())
    }
}

impl Default for Text {
    fn default() -> Text {
        Text::Static("")
    }
}

impl PartialEq for Text {
    fn eq(&self, other: &Text) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for Text {}

impl PartialOrd for Text {
    fn partial_cmp(&self, other: &Text) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Text {
    fn cmp(&self, other: &Text) -> std::cmp::Ordering {
        self.as_str().cmp(other.as_str())
    }
}

impl std::hash::Hash for Text {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
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
        at += 1;
    }
    let last = bytes[end - 1];
    if last == b'.' || last == b' ' {
        return Some(Why::Trailing(last as char));
    }
    if reserved(bytes, start, end) { Some(Why::Reserved) } else { None }
}

/// Whether Windows keeps a name for a device: its part before the first dot, without the spaces ending it.
const fn reserved(bytes: &[u8], start: usize, end: usize) -> bool {
    let mut stem_end = start;
    while stem_end < end && bytes[stem_end] != b'.' {
        stem_end += 1;
    }
    while stem_end > start && bytes[stem_end - 1] == b' ' {
        stem_end -= 1;
    }
    let len = stem_end - start;
    if len != 3 && len != 4 {
        return false;
    }
    let (a, b, c) = (
        bytes[start].to_ascii_uppercase(),
        bytes[start + 1].to_ascii_uppercase(),
        bytes[start + 2].to_ascii_uppercase(),
    );
    if len == 3 {
        return matches!((a, b, c), (b'C', b'O', b'N') | (b'P', b'R', b'N') | (b'A', b'U', b'X') | (b'N', b'U', b'L'));
    }
    matches!((a, b, c), (b'C', b'O', b'M') | (b'L', b'P', b'T')) && bytes[start + 3] >= b'1' && bytes[start + 3] <= b'9'
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
        self.parts()
    }
}

impl Name {
    /// `text` as a name.
    ///
    /// # Errors
    /// It is no portable name.
    pub fn new(text: &str) -> Result<Name, PathError> {
        match refused(text) {
            Some(why) => Err(PathError::new(text, why)),
            None => Ok(Name(text.into())),
        }
    }

    /// A name the program holds: checked as it is compiled, where it is a constant (`const X: Name = ...`), and
    /// refused by failing to.
    ///
    /// # Panics
    /// `text` is no portable name.
    #[must_use]
    pub const fn constant(text: &'static str) -> Name {
        assert!(refused(text).is_none(), "no portable name");
        Name(Text::Static(text))
    }

    /// Its text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl RelPath {
    /// The path of nothing: what it is taken within itself.
    #[must_use]
    pub fn root() -> RelPath {
        RelPath::default()
    }

    /// `text` as a path: names joined by `/`, none for the empty text.
    ///
    /// # Errors
    /// A part of it is no portable name, or empty (`a//b`, a leading or a trailing `/`).
    pub fn new(text: &str) -> Result<RelPath, PathError> {
        match refused_path(text) {
            Some(why) => Err(PathError::new(text, why)),
            None => Ok(RelPath(text.into())),
        }
    }

    /// A path the program holds: checked as it is compiled, where it is a constant (`const X: RelPath = ...`), and
    /// refused by failing to.
    ///
    /// # Panics
    /// `text` is no relative path.
    #[must_use]
    pub const fn constant(text: &'static str) -> RelPath {
        assert!(refused_path(text).is_none(), "no relative path of portable names");
        RelPath(Text::Static(text))
    }

    /// Where a store keeps what it keeps by a key (a digest): in a folder of the key's first two characters, as
    /// `ab/abcdef`, with `suffix` after the key's name (`.cbor`, or nothing).
    ///
    /// # Errors
    /// The key with its suffix is no portable name.
    pub fn sharded(key: &str, suffix: &str) -> Result<RelPath, PathError> {
        let file = format!("{key}{suffix}");
        let shard = key.get(..2).unwrap_or(key);
        for name in [shard, file.as_str()] {
            if let Some(why) = refused(name) {
                return Err(PathError::new(&file, why));
            }
        }
        Ok(RelPath(format!("{shard}/{file}").as_str().into()))
    }

    /// Whether it names nothing but what it is taken within.
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.0.as_str().is_empty()
    }

    /// Its names, in order; each one checked as [`Name`] checks one.
    pub fn parts(&self) -> impl DoubleEndedIterator<Item = &str> {
        self.0.as_str().split('/').filter(|part| !part.is_empty())
    }

    /// How many names it has.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.parts().count()
    }

    /// Its last name, unless it is the root.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.parts().next_back()
    }

    /// Its last name as a name, unless it is the root: a part of a path is one already, checked as the path was.
    #[must_use]
    pub fn last(&self) -> Option<Name> {
        self.name().map(|name| Name(Text::Shared(name.into())))
    }

    /// What it is in, unless it is the root.
    #[must_use]
    pub fn parent(&self) -> Option<RelPath> {
        if self.is_root() {
            return None;
        }
        let text = self.0.as_str();
        Some(match text.rfind('/') {
            Some(at) => RelPath(text[..at].into()),
            None => RelPath::root(),
        })
    }

    /// A name within it.
    #[must_use]
    pub fn join(&self, name: &Name) -> RelPath {
        self.join_text(name.as_str())
    }

    /// A path within it.
    #[must_use]
    pub fn join_path(&self, path: &RelPath) -> RelPath {
        self.join_text(path.as_str())
    }

    fn join_text(&self, text: &str) -> RelPath {
        match (self.is_root(), text.is_empty()) {
            (_, true) => self.clone(),
            (true, false) => RelPath(text.into()),
            (false, false) => RelPath(format!("{}/{text}", self.as_str()).as_str().into()),
        }
    }

    /// Whether it is `prefix` or within it, name by name.
    #[must_use]
    pub fn starts_with(&self, prefix: &RelPath) -> bool {
        self.strip_prefix(prefix).is_some()
    }

    /// What of it is within `prefix`, if it is `prefix` or within it.
    #[must_use]
    pub fn strip_prefix(&self, prefix: &RelPath) -> Option<RelPath> {
        if prefix.is_root() {
            return Some(self.clone());
        }
        let rest = self.as_str().strip_prefix(prefix.as_str())?;
        match rest.strip_prefix('/') {
            _ if rest.is_empty() => Some(RelPath::root()),
            Some(rest) => Some(RelPath(rest.into())),
            None => None,
        }
    }

    /// Its text, names joined by `/`: for writing it out where a path leaves the program as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl PartialEq<str> for Name {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for Name {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<Name> for str {
    fn eq(&self, other: &Name) -> bool {
        self == other.as_str()
    }
}

impl PartialEq<Name> for &str {
    fn eq(&self, other: &Name) -> bool {
        *self == other.as_str()
    }
}

impl PartialEq<Name> for String {
    fn eq(&self, other: &Name) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<Name> for &String {
    fn eq(&self, other: &Name) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<String> for Name {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<str> for RelPath {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for RelPath {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<Name> for RelPath {
    fn eq(&self, other: &Name) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<Name> for &RelPath {
    fn eq(&self, other: &Name) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<std::ffi::OsStr> for Name {
    fn eq(&self, other: &std::ffi::OsStr) -> bool {
        other == self.as_str()
    }
}

impl PartialEq<Name> for &std::ffi::OsStr {
    fn eq(&self, other: &Name) -> bool {
        *self == other.as_str()
    }
}

impl PartialEq<Name> for std::ffi::OsString {
    fn eq(&self, other: &Name) -> bool {
        self == other.as_str()
    }
}

impl PartialEq<Name> for std::ffi::OsStr {
    fn eq(&self, other: &Name) -> bool {
        self == other.as_str()
    }
}

impl From<Name> for RelPath {
    fn from(name: Name) -> RelPath {
        RelPath(name.0)
    }
}

impl fmt::Display for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.as_str())
    }
}

impl fmt::Debug for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.0.as_str(), f)
    }
}

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.as_str())
    }
}

impl fmt::Debug for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.0.as_str(), f)
    }
}

impl std::str::FromStr for Name {
    type Err = PathError;
    fn from_str(text: &str) -> Result<Name, PathError> {
        Name::new(text)
    }
}

impl std::str::FromStr for RelPath {
    type Err = PathError;
    fn from_str(text: &str) -> Result<RelPath, PathError> {
        RelPath::new(text)
    }
}

crate::serde_as_text!(Name);
crate::serde_as_text!(RelPath);

#[cfg(test)]
mod tests {
    use super::{Name, RelPath};
    use crate::Why;

    const LAYERS: Name = Name::constant(".layers");
    const NESTED: RelPath = RelPath::constant(".layers/top");

    fn why(text: &str) -> Why {
        Name::new(text).expect_err(text).why
    }

    #[test]
    fn a_name_is_refused_where_some_system_could_not_hold_it() {
        for ok in ["a", "native.bin", "node.toml", "COM0", "LPT", "console", "aux_", "my aux", ".hidden", "a b", "ä"] {
            assert_eq!(Name::new(ok).map(|n| n.to_string()).as_deref(), Ok(ok));
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
        assert_eq!(NESTED.parent().unwrap(), RelPath::from(LAYERS));
        let error = Name::new("a:b").unwrap_err().to_string();
        assert!(error.contains("`:`"), "{error}");
    }

    #[test]
    fn a_path_is_its_names_and_goes_by_them() {
        let path = RelPath::new("packages/weapons/m93r").unwrap();
        assert_eq!(path.parts().collect::<Vec<_>>(), ["packages", "weapons", "m93r"]);
        assert_eq!(path.name(), Some("m93r"));
        assert_eq!(path.depth(), 3);
        assert_eq!(path.parent().unwrap().as_str(), "packages/weapons");
        assert_eq!(RelPath::new("a").unwrap().parent(), Some(RelPath::root()));
        assert_eq!(RelPath::root().parent(), None);
        assert!(RelPath::new("").unwrap().is_root());
        let weapons = RelPath::new("packages/weapons").unwrap();
        assert!(path.starts_with(&weapons) && path.starts_with(&RelPath::root()) && path.starts_with(&path));
        assert!(!RelPath::new("packages/weaponsx").unwrap().starts_with(&weapons));
        assert_eq!(path.strip_prefix(&weapons).unwrap().as_str(), "m93r");
        assert_eq!(path.strip_prefix(&path), Some(RelPath::root()));
        assert_eq!(weapons.join(&Name::new("m93r").unwrap()), path);
        assert_eq!(RelPath::root().join_path(&path), path);
        assert_eq!(path.join_path(&RelPath::root()), path);
        for bad in ["/a", "a/", "a//b", "a/../b", "a/./b", "a\\b"] {
            assert!(RelPath::new(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn paths_order_as_their_texts() {
        let texts = ["a", "a b", "a-b", "a/b", "a/b/c", "ab", "b"];
        let mut paths: Vec<RelPath> = texts.iter().rev().map(|t| RelPath::new(t).unwrap()).collect();
        paths.sort();
        let mut sorted = texts.to_vec();
        sorted.sort_unstable();
        assert_eq!(paths.iter().map(RelPath::as_str).collect::<Vec<_>>(), sorted);
    }

    #[test]
    fn as_text_it_is_checked_when_read() {
        let path = RelPath::new("a/b").unwrap();
        assert_eq!(serde_json::to_string(&path).unwrap(), "\"a/b\"");
        assert_eq!(serde_json::from_str::<RelPath>("\"a/b\"").unwrap(), path);
        assert!(serde_json::from_str::<RelPath>("\"a/../b\"").is_err());
        assert!(serde_json::from_str::<Name>("\"a/b\"").is_err());
        assert_eq!(RelPath::sharded("abcdef", ".cbor").unwrap().as_str(), "ab/abcdef.cbor");
        assert!(RelPath::sharded("../x", "").is_err());
    }
}
