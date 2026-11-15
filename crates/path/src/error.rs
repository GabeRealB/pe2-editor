//! Why a text is no path of the kind asked for.

use std::fmt;

/// A text refused as a path, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathError {
    /// The text refused.
    pub text: Box<str>,
    /// What of it is refused.
    pub why: Why,
}

/// What makes a text no path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    /// A name with nothing in it, or a path with an empty part (`a//b`, a leading or trailing `/`).
    Empty,
    /// `.` or `..`, which name no file of their own.
    Dot,
    /// A character some system does not take in a name: a separator, `: * ? " < > |`, or a control character.
    Character(char),
    /// A name Windows keeps for a device (`CON`, `NUL`, `COM1`, ...), with or without an extension.
    Reserved,
    /// A name ending in a dot or a space, which Windows drops.
    Trailing(char),
    /// No `uid://` and a uid's digits.
    NoUid,
    /// A suffix after `#` with nothing in it.
    EmptySuffix,
    /// A name the system gave that is not text (on Windows, unpaired UTF-16; elsewhere, bytes that are not UTF-8).
    NotText,
    /// A name with a character that turns how text around it reads (U+202E and its kind): shown as what it is not.
    Direction(char),
    /// A name longer than a name may be on every system: 255 bytes.
    Long,
}

impl PathError {
    pub(crate) fn new(text: &str, why: Why) -> PathError {
        PathError { text: text.into(), why }
    }
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = &self.text;
        match self.why {
            Why::Empty => write!(f, "{text:?} has an empty name in it"),
            Why::Dot => write!(f, "{text:?} has a `.` or `..` in it, which names no file of its own"),
            Why::Character(c) if c.is_control() => {
                write!(f, "{text:?} has a control character ({:#04x}) in it", u32::from(c))
            }
            Why::Character(c) => write!(f, "{text:?} has a `{c}` in a name, which not every system takes"),
            Why::Reserved => write!(f, "{text:?} has a name Windows keeps for a device"),
            Why::Trailing(' ') => write!(f, "{text:?} has a name ending in a space, which Windows drops"),
            Why::Trailing(c) => write!(f, "{text:?} has a name ending in `{c}`, which Windows drops"),
            Why::NoUid => write!(f, "{text:?} is no uid (`uid://` and its digits)"),
            Why::EmptySuffix => write!(f, "{text:?} has nothing after its `#`"),
            Why::NotText => write!(f, "{text:?} has a name that is not text"),
            Why::Direction(c) => {
                write!(
                    f,
                    "{text:?} has a character turning which way text reads (U+{:04X}), shown as what it is not",
                    u32::from(c)
                )
            }
            Why::Long => write!(f, "{text:?} has a name longer than 255 bytes, which not every system takes"),
        }
    }
}

impl std::error::Error for PathError {}
