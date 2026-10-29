//! Types, as registered: what a value is. A type may carry a recognizer - signatures the host matches itself, then,
//! where those leave it open, a sniff of a file's first bytes, and a media type it answers to - and then a stored file
//! is recognized as it, cheaply. One without is only ever made by an operation.

use std::sync::Arc;

use crate::Name;

/// How the host recognizes a type in a file without calling the extension.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Signature {
    /// These bytes at this offset.
    Bytes { offset: usize, bytes: Vec<u8> },
    /// A file name: exact (`native.texels`), or a suffix after `*` (`*.png`). A hint, not proof.
    Name(String),
    /// Text: UTF-8 without a NUL byte.
    Text,
}

/// A type recognized in a file, with its version where that matters.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Recognized {
    pub version: Option<String>,
}

/// A sniff: a file's name, size and first bytes to whether it is of the type.
pub type Sniff = dyn Fn(&str, u64, &[u8]) -> Option<Recognized> + Send + Sync;

/// A type as registered.
#[derive(Clone)]
pub struct TypeDef {
    pub name: Name,
    pub label: String,
    /// A standard media type (`image/png`) it answers to: a claim about the file, which operations may take it by.
    pub media_type: Option<String>,
    /// Any one matching recognizes the file; none at all leaves it to the sniff.
    pub signatures: Vec<Signature>,
    pub sniff: Option<Arc<Sniff>>,
}

impl TypeDef {
    /// Whether a stored file can be recognized as it.
    #[must_use]
    pub fn recognizable(&self) -> bool {
        !self.signatures.is_empty() || self.sniff.is_some()
    }
}
