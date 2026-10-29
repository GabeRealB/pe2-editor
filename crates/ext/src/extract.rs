//! Extractors: how an extension makes its assets' native files from a release's discs. Extraction asks the extractor
//! registered for an asset's kind and role, if any (the rest it writes as their slots' bytes): it begins a session
//! with the declarations - what the extractor needs of the other assets (which file uploads a palette, where a VRAM
//! slot is) it reads once, there - and then extracts asset after asset, reading what chunks it needs from the discs.

use std::sync::Arc;

use crate::{Name, Result, Table};

/// Files made, each by its name in the bundle.
pub type Made = Vec<(String, Vec<u8>)>;

/// The declarations, as extraction holds them: each the table its group file declares it by (with `from`, where
/// extraction finds it per release).
pub trait Declared {
    /// Every declaration of a kind, as declared (`file`, `image`, `vram_slot`), by path, in path order.
    fn declarations(&self, kind: &str) -> Vec<(String, Table)>;
}

/// A release's discs, as an extraction reads them.
pub trait Discs {
    /// A chunk's payload, by a location as a declaration's `from` gives it (`{ stage, folder, file, slot }`).
    ///
    /// # Errors
    /// No such chunk, or it cannot be read.
    fn read(&mut self, at: &Table) -> Result<Vec<u8>>;

    /// The size of what `read` gives at a location, without reading it (where the discs can; else read).
    ///
    /// # Errors
    /// As `read`.
    fn size(&mut self, at: &Table) -> Result<u64> {
        Ok(self.read(at)?.len() as u64)
    }

    /// Up to `len` bytes of what `read` gives at a location, from `offset`: fewer at its end. A disc slot of hundreds of
    /// megabytes - a movie file - is read by range, never whole (where the discs can; else read and cut).
    ///
    /// # Errors
    /// As `read`.
    fn read_range(&mut self, at: &Table, offset: u64, len: usize) -> Result<Vec<u8>> {
        let all = self.read(at)?;
        let start = usize::try_from(offset).unwrap_or(usize::MAX).min(all.len());
        Ok(all[start..all.len().min(start.saturating_add(len))].to_vec())
    }
}

/// An extraction session: the assets of the kind, one after another.
pub trait Extraction: Send {
    /// An asset's native files for one release, each checked against what its declaration says of it.
    ///
    /// # Errors
    /// Its chunks cannot be read or decoded, or what they hold is not what the declaration says.
    fn extract(&mut self, path: &str, release: &str, discs: &mut dyn Discs) -> Result<Made>;
}

/// Begin a session.
pub type Begin = dyn Fn(&dyn Declared) -> Result<Box<dyn Extraction>> + Send + Sync;

/// An extractor as registered: for a kind, and for some of its roles (all of them where none is named).
#[derive(Clone)]
pub struct ExtractorDef {
    pub name: Name,
    pub kind: Name,
    /// The roles it extracts, qualified; every role of the kind where empty.
    pub roles: Vec<Name>,
    pub begin: Arc<Begin>,
}

impl ExtractorDef {
    /// Whether it extracts assets of this kind and role (qualified).
    #[must_use]
    pub fn takes(&self, kind: &str, role: Option<&str>) -> bool {
        self.kind.as_str() == kind && (self.roles.is_empty() || role.is_some_and(|r| self.roles.iter().any(|n| n.as_str() == r)))
    }
}
