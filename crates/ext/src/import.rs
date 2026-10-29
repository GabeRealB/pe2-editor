//! Formats and importers. A format is what a file is, recognized cheaply: by signatures the host matches itself, then,
//! where those leave it open, by a sniff of the file's first bytes. An importer makes an asset of a kind from a source
//! of the formats it reads, by its settings - the declaration's `[import]` section; a native importer makes nothing,
//! its outputs being its inputs, and its settings say how to read them.

use std::sync::Arc;

use crate::{Asset, Field, Name, ViewDef};

/// How the host recognizes a format without calling the extension.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Signature {
    /// These bytes at this offset.
    Bytes { offset: usize, bytes: Vec<u8> },
    /// A file name: exact (`native.texels`), or a suffix after `*` (`*.png`). A hint, not proof.
    Name(String),
    /// Text: UTF-8 without a NUL byte.
    Text,
}

/// A format recognized in a file, with its version where that matters.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Recognized {
    pub version: Option<String>,
}

/// A sniff: a file's name, size and first bytes to whether it is of the format.
pub type Sniff = dyn Fn(&str, u64, &[u8]) -> Option<Recognized> + Send + Sync;

/// A format as registered.
#[derive(Clone)]
pub struct FormatDef {
    pub name: Name,
    pub label: String,
    /// A standard media type (`image/png`): a claim about the file, which importers may read by.
    pub media_type: Option<String>,
    /// Any one matching recognizes the file; none at all leaves it to the sniff.
    pub signatures: Vec<Signature>,
    pub sniff: Option<Arc<Sniff>>,
}

/// How far an importer takes a source of a format it reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Support {
    Full,
    /// Its format, with contents it cannot (fully) take: a version, a subset. `offer`: still offered, with the reason.
    Partial { reason: String, offer: bool },
}

/// An importer's check of a source it might import.
pub type Check = dyn Fn(&Asset) -> Support + Send + Sync;

/// Files an import made, each by its name in the bundle.
pub type Made = Vec<(String, Vec<u8>)>;

/// What an import made of a source: the native files, and the kind settings it derives from the source (an image's
/// size, its cells' rectangles) - those its importer `derives`, taking the place of the declaration's.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Imported {
    pub files: Made,
    pub settings: crate::Table,
}

/// An import: what it makes of the asset's source (the file its `[import]` section names), by its settings.
pub type Import = dyn Fn(&Asset) -> crate::Result<Imported> + Send + Sync;

/// An export: the source written from an asset's native files and settings - what extraction does where the disc has
/// only the native form. Importing it makes those native files again, byte for byte.
pub type Export = dyn Fn(&Asset) -> crate::Result<Vec<u8>> + Send + Sync;

/// An importer as registered.
#[derive(Clone)]
pub struct ImporterDef {
    pub name: Name,
    pub label: String,
    /// What it reads: formats by name (`pe2.images@texels`) or media types (`image/png`).
    pub reads: Vec<String>,
    /// The kind it makes.
    pub makes: Name,
    /// Its outputs are its inputs: it makes nothing.
    pub native: bool,
    /// Its settings, as the properties pane shows them.
    pub fields: Vec<Field>,
    /// Views drawn in the properties pane with its settings: what its custom settings hold.
    pub inspector: Vec<ViewDef>,
    pub check: Option<Arc<Check>>,
    /// What it makes; a native importer makes nothing.
    pub import: Option<Arc<Import>>,
    /// The kind settings it derives from the source, as paths: `size`, `cells.*.rect` (`*` any part of a keyed table).
    /// A declaration's value there is overridden by the import's; a part the declaration has and the source no longer
    /// does is orphaned.
    pub derives: Vec<String>,
    /// The source it reads, written from the native form.
    pub export: Option<Arc<Export>>,
    /// Which of several fully supporting importers is the default: the highest.
    pub priority: i32,
}

/// An asset's import settings: its declaration's `[import]` section.
impl<'a> Asset<'a> {
    /// A setting of its importer.
    #[must_use]
    pub fn setting(&self, key: &str) -> Option<&'a crate::Value> {
        self.field("import")?.get(key)
    }

    /// Its importer, qualified.
    #[must_use]
    pub fn importer(&self) -> Option<&'a str> {
        self.setting("importer")?.as_str()
    }

    /// The file its importer reads.
    #[must_use]
    pub fn source_file(&self) -> Option<&'a str> {
        self.setting("source")?.as_str()
    }
}
