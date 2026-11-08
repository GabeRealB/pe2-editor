//! Types, as registered: what a value is. A type may carry a recognizer - signatures the host matches itself, then,
//! where those leave it open, a sniff of a file's first bytes, and a media type it answers to - and then a stored file
//! is recognized as it, cheaply. One without is only ever made by an operation.

use std::sync::Arc;

use crate::Name;
use crate::registry::{ActOnContents, Field, Icon, ListContents, SheetOfPart, Tint, ViewDef};

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

/// A type as registered: what a value is, recognized or not, and where it says how they show (`shows_nodes`) what a node
/// making one is.
#[derive(Clone)]
#[expect(clippy::struct_excessive_bools, reason = "a type's facts, each independent, each said by its builder")]
pub struct TypeDef {
    pub name: Name,
    pub label: String,
    /// A standard media type (`image/png`) it answers to: a claim about the file, which operations may take it by.
    pub media_type: Option<String>,
    /// Any one matching recognizes the file; none at all leaves it to the sniff.
    pub signatures: Vec<Signature>,
    pub sniff: Option<Arc<Sniff>>,
    /// The bare name declarations wrote before names were qualified (`image`): a built-in's alone.
    pub alias: Option<String>,
    pub icon: Option<(Icon, Tint)>,
    pub fields: Vec<Field>,
    pub views: Vec<ViewDef>,
    /// Views drawn in the inspector, after the host's property sheet: what its custom fields hold.
    pub inspector: Vec<ViewDef>,
    /// Editors of its own, beside the preview.
    pub editors: Vec<ViewDef>,
    /// Views at the head of the contents pane.
    pub contents_views: Vec<ViewDef>,
    /// What is inside its assets.
    pub contents: Option<Arc<ListContents>>,
    /// What comes of something done in its assets' contents.
    pub content_act: Option<Arc<ActOnContents>>,
    /// Its parts' fields, where those are not what their tables hold.
    pub part_sheet: Option<Arc<SheetOfPart>>,
    /// The stack a node of it has where its bundle states none, as TOML (`[[stack]]` tables): extraction writes it.
    pub stack: Option<String>,
    /// The files that make a node of it with that stack, by name (`*.usda`): the stack stores them under its own names.
    pub takes: Vec<String>,
    /// What only extraction makes: a person has no use for a new one.
    pub extracted: bool,
    /// A node of it holds only what is set on it: an operation of its name makes those settings, of this type.
    pub data: bool,
    /// What is set on a node of it is applied as soon as it is taken in: its outputs are made again at once, as what it
    /// is edited for (a script sequenced and played) is hearing or seeing them follow.
    pub applied_at_once: bool,
    /// The value types a node making one is of this type, by their qualified names: a picture stored as `texels` or as
    /// `bs_picture` is an `image`.
    pub values: Vec<String>,
    /// Said by an extension adding to it (`Registry::extend`), of a type of what it requires: nothing here is its label or
    /// its icon, and where it does not know what it adds to, what it adds is not there.
    pub extends: bool,
}

impl TypeDef {
    /// Whether a stored file can be recognized as it.
    #[must_use]
    pub fn recognizable(&self) -> bool {
        !self.signatures.is_empty() || self.sniff.is_some()
    }

    /// Whether a node making a value of it is one of it: it says how such a node shows, rather than only being what flows.
    #[must_use]
    pub fn shows_nodes(&self) -> bool {
        self.data || self.icon.is_some() || self.alias.is_some() || !self.values.is_empty()
    }
}
