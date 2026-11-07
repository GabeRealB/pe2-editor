//! What an extension's extraction operations read: a plan (the host's `plan`) reads off the discs what each takes and
//! gives it as its chunks, each with the place it was read at (`Given`), and the declarations it needs as its settings.
//! An extension's code reads them as it would read the discs (`Discs`) and the declarations (`Declared`), so that what
//! reads a texture's pages or an atlas's faces needs no knowledge of where they came from.

use crate::{Result, Table};

/// Files made, each by its name in the bundle.
pub type Made = Vec<(String, Vec<u8>)>;

/// Declarations as an operation was given them: each the table its group file declares it by.
pub trait Declared {
    /// Every declaration of a type given (`file`, `image`, `vram_slot`), by path, in path order.
    fn declarations(&self, node_type: &str) -> Vec<(String, Table)>;
}

/// The discs, as an extension's code reads them: here, what a plan gave it (`Given`).
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

/// The chunks a plan read for an extraction, each by the place it was read at (`{ disc, slot }`, or `{ stage, folder,
/// file, slot }`): what it reads, and nothing else - a read of another place fails, naming it.
pub struct Given {
    chunks: Vec<(Table, Vec<u8>)>,
}

/// What of a place says which chunk it is: an image chunk's `page` is read of it, not where it is.
fn place_of(at: &Table) -> Vec<(&str, &toml::Value)> {
    ["disc", "stage", "folder", "file", "slot"].iter().filter_map(|key| Some((*key, at.get(*key)?))).collect()
}

impl Given {
    /// The chunks, each with the place it was read at, in the same order.
    ///
    /// # Errors
    /// They are not as many as the places, or a place is no table.
    pub fn new(places: &[toml::Value], chunks: Vec<Vec<u8>>) -> Result<Given> {
        if places.len() != chunks.len() {
            crate::bail!("{} chunks given for {} places", chunks.len(), places.len());
        }
        let places = places.iter().map(|at| at.as_table().cloned().ok_or_else(|| crate::anyhow!("a place no table")));
        Ok(Given { chunks: places.zip(chunks).map(|(at, bytes)| Ok((at?, bytes))).collect::<Result<_>>()? })
    }
}

impl Discs for Given {
    fn read(&mut self, at: &Table) -> Result<Vec<u8>> {
        let wanted = place_of(at);
        let found = self.chunks.iter().find(|(given, _)| place_of(given) == wanted);
        found.map(|(_, bytes)| bytes.clone()).ok_or_else(|| crate::anyhow!("it reads {at}, which it was not given"))
    }
}
