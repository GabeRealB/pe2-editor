//! A release's discs, read: each disc image found by its size and sha1 and held to its layout, its slots read whole or by
//! range, and a stage's container - `STAGE0.HED` and `STAGE0.CDF`, or a `STAGE<N>.CDF` - read for where its files'
//! chunks and its stored streams are. What reads the discs - extraction, a plan's operations - reads them through this,
//! by any number of threads at once.

use anyhow::{Context, Result, bail};
use formats::cdf::{self, Source};
use formats::iso::{self, Image};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use crate::extract::sha1_file;

/// A disc image in `roms`, by size and sha1 (its name may differ).
pub(crate) fn find_disc(roms: &Path, name: &str, size: u64, sha1: &str) -> Result<PathBuf> {
    let named = roms.join(name);
    let mut candidates = vec![];
    if named.is_file() {
        candidates.push(named.clone());
    }
    let mut others: Vec<PathBuf> = fs::read_dir(roms)
        .with_context(|| format!("{}", roms.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && *p != named)
        .collect();
    others.sort();
    candidates.extend(others);
    for p in candidates {
        if fs::metadata(&p)?.len() == size && sha1_file(&p)? == sha1 {
            return Ok(p);
        }
    }
    bail!("{name} (sha1 {sha1}) is not in {}", roms.display())
}

/// A disc's layout: its image, found by its size and sha1, and its slots in disc order, each by name and type
/// (`license`, `data`, `mixed`).
#[derive(Clone, Debug)]
pub struct Layout {
    pub image: String,
    pub size: u64,
    pub sha1: String,
    pub slots: Vec<(String, String)>,
}

/// A disc opened: its image, its layout, and its root directory's files by name.
pub struct OpenDisc {
    image: Image,
    pub layout: Layout,
    pub files: BTreeMap<String, iso::Entry>,
    pub path: PathBuf,
}

impl OpenDisc {
    /// Found in `roms` and held to its layout: the same files, in disc order.
    ///
    /// # Errors
    /// It is not there, or it holds other files.
    pub fn open(roms: &Path, name: &str, layout: &Layout) -> Result<OpenDisc> {
        Self::open_at(find_disc(roms, &layout.image, layout.size, &layout.sha1)?, name, layout)
    }

    /// A disc image found already (`find_disc`), held to its layout.
    ///
    /// # Errors
    /// It cannot be read, or its files are not the layout's.
    pub fn open_at(path: PathBuf, name: &str, layout: &Layout) -> Result<OpenDisc> {
        let image = Image::open(&path)?;
        let mut entries = image.root()?;
        if let Some(d) = entries.iter().find(|e| e.directory) {
            bail!("{name}: the directory {} - the layout knows a flat root only", d.name);
        }
        entries.sort_by_key(|e| e.lba);
        let on_disc: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        let declared: Vec<&str> =
            layout.slots.iter().filter(|(_, ty)| ty != "license").map(|(name, _)| name.as_str()).collect();
        if on_disc != declared {
            bail!("{name}: the disc's files, in disc order, are {on_disc:?}; the layout says {declared:?}");
        }
        let files = entries.into_iter().map(|e| (e.name.clone(), e)).collect();
        Ok(OpenDisc { image, layout: layout.clone(), files, path })
    }

    /// A slot's bytes: the license data, or an ISO file's - a `mixed` file's as 2336-byte sectors.
    pub(crate) fn slot(&self, name: &str) -> Result<Vec<u8>> {
        let ty = self.slot_type(name)?;
        match ty {
            "license" => Ok(self.image.license()?),
            "data" => {
                let e = &self.files[name];
                let mut bytes = self.image.data(e.lba, e.sectors())?;
                bytes.truncate(e.size as usize);
                Ok(bytes)
            }
            "mixed" => {
                let e = &self.files[name];
                Ok(self.image.mode2(e.lba, e.sectors())?)
            }
            other => bail!("slot {name}: no reading for type {other}"),
        }
    }

    /// The size of a slot's bytes, as `slot` gives them.
    pub(crate) fn slot_size(&self, name: &str) -> Result<u64> {
        Ok(match self.slot_type(name)? {
            "data" => u64::from(self.files[name].size),
            "mixed" => u64::from(self.files[name].sectors()) * iso::MODE2 as u64,
            _ => self.slot(name)?.len() as u64,
        })
    }

    /// Up to `len` of a slot's bytes from `offset`, as `slot` gives them, reading the sectors they lie in alone.
    pub(crate) fn slot_range(&self, name: &str, offset: u64, len: usize) -> Result<Vec<u8>> {
        let size = self.slot_size(name)?;
        let end = size.min(offset.saturating_add(len as u64));
        if offset >= end {
            return Ok(vec![]);
        }
        let ty = self.slot_type(name).unwrap_or_default().to_string();
        let sector = match ty.as_str() {
            "data" => cdf::SECTOR,
            "mixed" => iso::MODE2 as u64,
            _ => {
                let all = self.slot(name)?;
                return Ok(all[usize::try_from(offset)?..usize::try_from(end)?].to_vec());
            }
        };
        let e = &self.files[name];
        let first = u32::try_from(offset / sector)?;
        let count = u32::try_from(end.div_ceil(sector))? - first;
        let bytes = if ty == "data" {
            self.image.data(e.lba + first, count)?
        } else {
            self.image.mode2(e.lba + first, count)?
        };
        let skip = usize::try_from(offset % sector)?;
        Ok(bytes[skip..skip + usize::try_from(end - offset)?].to_vec())
    }
}

/// An ISO file of an open disc, read sector by sector.
pub(crate) struct IsoFile<'a> {
    image: &'a Image,
    lba: u32,
    pub size: u64,
}

impl Source for IsoFile<'_> {
    fn read_at(&mut self, offset: u64, len: usize) -> std::io::Result<Vec<u8>> {
        let end = offset + len as u64;
        if end > self.size {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{len} bytes at {offset:#x} past the file"),
            ));
        }
        let first = u32::try_from(offset / cdf::SECTOR).unwrap_or(u32::MAX);
        let count = u32::try_from(end.div_ceil(cdf::SECTOR)).unwrap_or(u32::MAX) - first;
        let bytes = self.image.data(self.lba + first, count)?;
        let skip = usize::try_from(offset % cdf::SECTOR).unwrap_or(0);
        Ok(bytes[skip..skip + len].to_vec())
    }
}

impl OpenDisc {
    pub(crate) fn iso_file(&self, name: &str) -> Result<IsoFile<'_>> {
        let e = self.files.get(name).with_context(|| format!("no file {name}"))?;
        let (lba, size) = (e.lba, u64::from(e.size));
        Ok(IsoFile { image: &self.image, lba, size })
    }
}

/// A stage's container as extraction reads it: where it is (a disc, its CDF), its files' offsets, and where each stream
/// it stores is, by its folder (none in stage 0) and its place in the table: `[start, end)`.
pub struct Container {
    pub disc: String,
    pub cdf: String,
    pub files: HashMap<(Option<u32>, u32), u64>,
    pub streams: HashMap<(Option<u32>, usize), (u64, u64)>,
}

/// Whether a stream record's data is stored in the stage: a scene's always, a movie's where it has no volume (a movie
/// with a volume plays from the disc's movie file).
fn stored_in_stage(record: &cdf::StreamRecord) -> bool {
    record.kind == 2 || (record.kind == 1 && record.data[9] == 0)
}

impl OpenDisc {
    fn slot_type(&self, name: &str) -> Result<&str> {
        let found = self.layout.slots.iter().find(|(slot, _)| slot == name);
        found.map(|(_, ty)| ty.as_str()).with_context(|| format!("no slot {name}"))
    }
}

/// A stage's container, read off its disc: stage 0's by `STAGE0.HED` and `STAGE0.CDF`, another's by its CDF.
///
/// # Errors
/// Its tables cannot be read.
pub fn container(disc: &OpenDisc, disc_name: &str, stage: u32, cdf_name: &str) -> Result<Container> {
    let mut streams = HashMap::new();
    let entries = if stage == 0 {
        // A stored stream runs to the next file.
        let hed = disc.slot("STAGE0.HED")?;
        let entries = cdf::stage0_files(&hed)?;
        let end = disc.iso_file("STAGE0.CDF")?.size;
        for (index, record) in cdf::stage0_streams(&hed)?.iter().enumerate().filter(|(_, r)| stored_in_stage(r)) {
            let start = u64::from(record.start) * cdf::SECTOR;
            let next = entries.iter().map(|e| e.offset).filter(|&o| o > start).min().unwrap_or(end);
            streams.insert((None, index), (start, next));
        }
        entries
    } else {
        // A folder's stored streams run each to the next, the last to the folder's end.
        let mut file = disc.iso_file(cdf_name)?;
        let entries = cdf::stage_files(&mut file)?;
        for folder in cdf::stage_folders(&mut file)? {
            let records = cdf::folder_streams(&mut file, &folder)?;
            let starts: Vec<u64> = records
                .iter()
                .filter(|r| stored_in_stage(r))
                .map(|r| folder.offset + u64::from(r.start) * cdf::SECTOR)
                .collect();
            for (index, record) in records.iter().enumerate().filter(|(_, r)| stored_in_stage(r)) {
                let start = folder.offset + u64::from(record.start) * cdf::SECTOR;
                let next = starts.iter().copied().filter(|&s| s > start).min().unwrap_or(folder.offset + folder.size);
                streams.insert((Some(folder.id), index), (start, next));
            }
        }
        entries
    };
    let files = entries.into_iter().map(|e| ((e.folder, e.id), e.offset)).collect();
    Ok(Container { disc: disc_name.to_string(), cdf: cdf_name.to_string(), files, streams })
}

/// A file's chunk, by its place among the file's chunks: its payload.
///
/// # Errors
/// The container holds no such file, or it has no such chunk.
pub fn chunk(disc: &OpenDisc, container: &Container, folder: Option<u32>, file: u32, index: usize) -> Result<Vec<u8>> {
    let offset =
        *container.files.get(&(folder, file)).with_context(|| format!("no file {file} in folder {folder:?}"))?;
    let mut src = disc.iso_file(&container.cdf)?;
    let chunks = cdf::chunks(&mut src, offset)?;
    let chunk = chunks.get(index).with_context(|| format!("file {file} has no chunk {index}"))?;
    Ok(chunk.payload(&mut src)?)
}

/// A stream stored in a stage, by its folder (none in stage 0) and its place in the table.
///
/// # Errors
/// The container stores no such stream.
pub fn stream(disc: &OpenDisc, container: &Container, folder: Option<u32>, index: usize) -> Result<Vec<u8>> {
    let &(start, end) =
        container.streams.get(&(folder, index)).with_context(|| format!("no stored stream {index} in {folder:?}"))?;
    Ok(disc.iso_file(&container.cdf)?.read_at(start, usize::try_from(end - start)?)?)
}
