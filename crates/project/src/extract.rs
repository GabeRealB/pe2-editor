//! Extraction: a project's game layer, (re)written from its releases' discs by the declarations
//! alone.
//!
//! Each disc of the layout is found among the project's roms by its size and sha1, and its file system is held to
//! the layout (the same files, in disc order). Every declared asset is written as a bundle, `<group>/<name>.node/`,
//! its `node.toml` the declaration's own fields (its slots filled, a stream's descriptor fields, ...) without what
//! only extraction reads (`from`). An asset located in a disc slot is read from it - an ISO file's bytes (a `mixed`
//! file's as 2336-byte sectors) or the license data -, checked against its sha1 and written as `native.<format>`.
//! An asset located in a file's chunk slot is read from its stage container - `STAGE0.HED`'s or a `STAGE<N>.CDF`'s tables
//! give the file's place, the file asset's slots the chunk's - as the chunk's payload. The layout's discs and stages
//! are written as bundles too; folders, groups, files and streams carry their declarations alone.
//!
//! An asset an extension extracts (`pe2.images@textures`: images with a `mode`) is made by that
//! extractor instead: its session begins with the declarations lent (what it needs of the other assets it reads once)
//! and then makes each asset's native files, reading the chunks it needs from the discs and checking them against the
//! declaration. The host knows no image format.
//!
//! A reference is written as the referenced asset's uid alone (`"uid://..."`, `"uid://...#palettes/default"` for a part
//! of it), where the declarations hold its path beside it: the uid decides, and a path written into every referencing
//! asset would go stale when the asset moved.
//!
//! An asset whose importer writes its source (an image's `pe2.images@ora`) is written as that source alone: exported
//! from the native files read off the disc, and imported again to check that it makes them byte for byte and gives
//! back the settings it derives (an image's size, its cells' rectangles), which its `node.toml` then leaves out -
//! extraction stops at an asset whose source would not. Its import is kept in the project cache, so the project opens
//! without importing (`imports`).
//!
//! A bundle's `node.toml` carries no uid: the layer's changeset (`<layer>.toml` beside it) lists every entry the
//! layer holds - each asset under its declared uid, each folder and `extraction.toml` under a new one - and marks the
//! layer read-only, extraction's alone.

use anyhow::{Context, Result, bail};
use formats::cdf::{self, Source};
use formats::iso::{self, Image};
use sha1::{Digest, Sha1};
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::changeset::Changeset;
use crate::declarations::{At, Declarations, Disc};
use crate::ext::Extensions;
use crate::imports::Imports;
use crate::lock::Lock;
use crate::{DiscImage, Game, Project, uid};

pub struct Options {
    /// The releases to extract; every release of the project when empty.
    pub releases: Vec<String>,
    pub roms: Option<PathBuf>,
    pub declarations: Option<PathBuf>,
}

pub struct Report {
    pub layer: PathBuf,
    pub assets: usize,
    pub layouts: usize,
    /// Assets written as their importer's source, each checked to import back into its native files.
    pub sources: usize,
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

#[must_use]
pub fn sha1_hex(bytes: &[u8]) -> String {
    hex(&Sha1::digest(bytes))
}

pub(crate) fn sha1_file(path: &Path) -> Result<String> {
    let mut f = fs::File::open(path)?;
    let (mut h, mut buf) = (Sha1::new(), vec![0; 1 << 22]);
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

/// A disc image in `roms`, by size and sha1 (its name may differ).
fn find_disc(roms: &Path, name: &str, size: u64, sha1: &str) -> Result<PathBuf> {
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

/// A disc opened for extraction: its image, its layout, and its root directory's files by name.
struct OpenDisc {
    image: Image,
    layout: Disc,
    files: BTreeMap<String, iso::Entry>,
    path: PathBuf,
}

impl OpenDisc {
    fn open(roms: &Path, name: &str, layout: &Disc) -> Result<OpenDisc> {
        let i = &layout.image;
        let path = find_disc(roms, &i.name, i.size, &i.sha1)?;
        let mut image = Image::open(&path)?;
        let mut entries = image.root()?;
        if let Some(d) = entries.iter().find(|e| e.directory) {
            bail!("{name}: the directory {} - the layout knows a flat root only", d.name);
        }
        entries.sort_by_key(|e| e.lba);
        let on_disc: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        let declared: Vec<&str> = layout.slots.iter().filter(|s| s.ty != "license").map(|s| s.name.as_str()).collect();
        if on_disc != declared {
            bail!("{name}: the disc's files, in disc order, are {on_disc:?}; the layout says {declared:?}");
        }
        let files = entries.into_iter().map(|e| (e.name.clone(), e)).collect();
        Ok(OpenDisc { image, layout: layout.clone(), files, path })
    }

    /// A slot's bytes: the license data, or an ISO file's - a `mixed` file's as 2336-byte sectors.
    fn slot(&mut self, name: &str) -> Result<Vec<u8>> {
        let slot = self.layout.slots.iter().find(|s| s.name == name).with_context(|| format!("no slot {name}"))?;
        match slot.ty.as_str() {
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
    fn slot_size(&mut self, name: &str) -> Result<u64> {
        let slot = self.layout.slots.iter().find(|s| s.name == name).with_context(|| format!("no slot {name}"))?;
        Ok(match slot.ty.as_str() {
            "data" => u64::from(self.files[name].size),
            "mixed" => u64::from(self.files[name].sectors()) * iso::MODE2 as u64,
            _ => self.slot(name)?.len() as u64,
        })
    }

    /// Up to `len` of a slot's bytes from `offset`, as `slot` gives them, reading the sectors they lie in alone.
    fn slot_range(&mut self, name: &str, offset: u64, len: usize) -> Result<Vec<u8>> {
        let size = self.slot_size(name)?;
        let end = size.min(offset.saturating_add(len as u64));
        if offset >= end {
            return Ok(vec![]);
        }
        let ty = self.layout.slots.iter().find(|s| s.name == name).map_or_default(|s| s.ty.clone());
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
        let bytes = if ty == "data" { self.image.data(e.lba + first, count)? } else { self.image.mode2(e.lba + first, count)? };
        let skip = usize::try_from(offset % sector)?;
        Ok(bytes[skip..skip + usize::try_from(end - offset)?].to_vec())
    }
}

/// An ISO file of an open disc, read sector by sector.
struct IsoFile<'a> {
    image: &'a mut Image,
    lba: u32,
    size: u64,
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
    fn iso_file(&mut self, name: &str) -> Result<IsoFile<'_>> {
        let e = self.files.get(name).with_context(|| format!("no file {name}"))?;
        let (lba, size) = (e.lba, u64::from(e.size));
        Ok(IsoFile { image: &mut self.image, lba, size })
    }
}

/// A stage's container as extraction reads it: where it is (a disc, its CDF) and its files' offsets.
struct Container {
    disc: String,
    cdf: String,
    files: HashMap<(Option<u32>, u32), u64>,
}

/// The stage containers of a release, opened as assets ask for them.
struct Containers {
    open: HashMap<(String, u32), Container>,
}

impl Containers {
    fn get(&mut self, decl: &Declarations, discs: &mut Discs, release: &str, stage: u32) -> Result<&Container> {
        let key = (release.to_string(), stage);
        if !self.open.contains_key(&key) {
            let layout = decl.layouts[release]
                .get(&format!("stage{stage}"))
                .with_context(|| format!("{release}: no stage {stage}"))?;
            let at = layout.at.first().with_context(|| format!("{release} stage {stage}: no disc"))?;
            let disc_name = at.disc.clone().with_context(|| format!("{release} stage {stage}: no disc"))?;
            let disc = discs
                .get_mut(&(release.to_string(), disc_name.clone()))
                .with_context(|| format!("no disc {disc_name}"))?;
            let (cdf_name, entries) = if stage == 0 {
                let hed = disc.slot("STAGE0.HED")?;
                ("STAGE0.CDF".to_string(), cdf::stage0_files(&hed)?)
            } else {
                let name = at.slots.first().cloned().with_context(|| format!("{release} stage {stage}: no CDF"))?;
                let entries = cdf::stage_files(&mut disc.iso_file(&name)?)?;
                (name, entries)
            };
            let files = entries.into_iter().map(|e| ((e.folder, e.id), e.offset)).collect();
            self.open.insert(key.clone(), Container { disc: disc_name, cdf: cdf_name, files });
        }
        Ok(&self.open[&key])
    }
}

/// A chunk slot's payload: the file the `at` names, in its stage's container, and the slot its file asset declares.
fn chunk_payload(
    decl: &Declarations,
    discs: &mut Discs,
    containers: &mut Containers,
    files: &HashMap<(String, u32, Option<u32>, u32), String>,
    release: &str,
    at: &At,
) -> Result<Vec<u8>> {
    let (Some(stage), Some(file), Some(slot)) = (at.stage, at.file, at.slot.as_deref()) else {
        bail!("not a chunk slot");
    };
    let file_asset = files
        .get(&(release.to_string(), stage, at.folder, file))
        .with_context(|| format!("no file asset at stage {stage} folder {:?} file {file}", at.folder))?;
    let index = decl.assets[file_asset]
        .slots
        .iter()
        .position(|s| s.name == slot)
        .with_context(|| format!("{file_asset} has no slot {slot}"))?;
    let container = containers.get(decl, discs, release, stage)?;
    let offset =
        *container.files.get(&(at.folder, file)).with_context(|| format!("stage {stage} has no file {file}"))?;
    let (disc_name, cdf_name) = (container.disc.clone(), container.cdf.clone());
    let disc = discs.get_mut(&(release.to_string(), disc_name)).context("the container's disc")?;
    let mut src = disc.iso_file(&cdf_name)?;
    let chunks = cdf::chunks(&mut src, offset)?;
    let chunk = chunks.get(index).with_context(|| format!("{file_asset}: no chunk {index} on the disc"))?;
    Ok(chunk.payload(&mut src)?)
}

fn quote(s: &str) -> String {
    toml_edit::Value::from(s).to_string().trim().to_string()
}

/// A declaration's text without its uid, which the layer's changeset holds, its references by uid alone.
fn without_uid(text: &str) -> Result<String> {
    let mut doc: toml_edit::DocumentMut = text.parse()?;
    doc.remove("uid");
    uids_alone(doc.as_item_mut());
    Ok(doc.to_string())
}

/// A declaration's references - `{ uid, path = "res://..." }` - written as their uids alone, everywhere in an item.
fn uids_alone(item: &mut toml_edit::Item) {
    match item {
        toml_edit::Item::Table(table) => table.iter_mut().for_each(|(_, item)| uids_alone(item)),
        toml_edit::Item::ArrayOfTables(tables) => {
            tables.iter_mut().for_each(|table| table.iter_mut().for_each(|(_, item)| uids_alone(item)));
        }
        toml_edit::Item::Value(value) => uids_alone_in(value),
        toml_edit::Item::None => {}
    }
}

fn uids_alone_in(value: &mut toml_edit::Value) {
    match value {
        toml_edit::Value::InlineTable(table) => {
            let uid = table.get("uid").and_then(toml_edit::Value::as_str).map(str::to_string);
            let path = table.get("path").and_then(toml_edit::Value::as_str).is_some_and(|p| p.starts_with("res://"));
            // An output of the asset: `uid://...#palettes/default`.
            let output = table.get("output").and_then(toml_edit::Value::as_str).map_or_default(|o| format!("#{o}"));
            if let (Some(uid), true) = (uid, path) {
                let decor = value.decor().clone();
                *value = toml_edit::Value::from(format!("{uid}{output}"));
                *value.decor_mut() = decor;
                return;
            }
            table.iter_mut().for_each(|(_, v)| uids_alone_in(v));
        }
        toml_edit::Value::Array(array) => array.iter_mut().for_each(uids_alone_in),
        _ => {}
    }
}

/// A bundle's `node.toml`: the declaration's own fields, then the native form's file and the originals' sha1s. An
/// asset whose declaration has an `[import]` section names its native file there (`source`), not beside it, nor the
/// format extraction names that file by.
fn asset_toml(body: &str, native: Option<&str>, originals: &[(String, String)]) -> Result<String> {
    let mut body = without_uid(body)?;
    let mut doc: toml_edit::DocumentMut = body.parse()?;
    let imported = doc.contains_table("import");
    if imported && doc.remove("format").is_some() {
        body = doc.to_string();
    }
    let mut lines =
        vec!["# A node, written by extraction.".to_string(), String::new(), body.trim().to_string()];
    if let Some(native) = native.filter(|_| !imported) {
        lines.push(format!("native = {}", quote(native)));
    }
    for (release, sha1) in originals {
        lines.extend([String::new(), format!("[original.{release}]"), format!("sha1 = {}", quote(sha1))]);
    }
    Ok(lines.join("\n") + "\n")
}

/// A bundle's `node.toml` with its declaration's `[import]` written out as its stack, before the originals: its source
/// stored on top, the importer taking it where it names one (with the import's other settings), the native form as the
/// `native` output - kept in the store, made as soon as the source changes, where the importer makes it; where none
/// does (`native`), the stored file as it is, made when asked for.
fn stacked(toml: &str, native: bool) -> Result<String> {
    use std::fmt::Write as _;
    let mut doc: toml_edit::DocumentMut = toml.parse()?;
    let Some(import) = doc.remove("import") else { return Ok(toml.to_string()) };
    let import = import.as_table_like().context("its `import` is not a table")?;
    let text = |key: &str| import.get(key).and_then(toml_edit::Item::as_str);
    let source = text("source").context("its import has no source")?;
    let mut stack = format!("[[stack]]\nop = \"file\"\nfile = {}\n", quote(source));
    // Its importer, where it names one; none, and the stored file is the native form as it is.
    if let Some(importer) = text("importer") {
        let _ = write!(stack, "\n[[stack]]\nop = {}\n", quote(importer));
        for (key, item) in import.iter().filter(|(k, _)| !matches!(*k, "importer" | "source")) {
            let value = item.as_value().with_context(|| format!("its import's {key} is not a value"))?;
            let _ = writeln!(stack, "{key} = {}", value.to_string().trim());
        }
    }
    stack.push_str("\n[[stack]]\nop = \"output\"\nname = \"native\"\n");
    if !native {
        stack.push_str("retain = \"cache\"\nwhen = \"eager\"\n");
    }
    Ok(with_stack(&doc.to_string(), &stack))
}

/// A layout asset's `node.toml`: its declaration without what only extraction reads (`at`).
fn layout_toml(text: &str) -> Result<String> {
    let mut doc: toml_edit::DocumentMut = text.parse()?;
    doc.remove("at");
    doc.remove("uid");
    uids_alone(doc.as_item_mut());
    let body = doc.to_string();
    let body = body.lines().skip_while(|l| l.starts_with('#') || l.is_empty()).collect::<Vec<_>>().join("\n");
    Ok(format!("# A node, written by extraction: the layout and its slots.\n\n{body}\n"))
}

fn write_bundle(layer: &Path, path: &str, files: &[(&str, &[u8])]) -> Result<()> {
    let dir = layer.join(format!("{path}.node"));
    fs::create_dir_all(&dir)?;
    for (name, bytes) in files {
        fs::write(dir.join(name), bytes).with_context(|| format!("{}", dir.join(name).display()))?;
    }
    Ok(())
}

type Discs = BTreeMap<(String, String), OpenDisc>;

/// Every disc of the releases' layouts, opened and held to its layout.
fn open_discs(game: &Game, decl: &Declarations, releases: &[String], roms: &Path) -> Result<Discs> {
    let mut discs = Discs::new();
    for release in releases {
        let layouts =
            decl.layouts.get(release).with_context(|| format!("{release}: the declarations have no layout"))?;
        let wanted: &Vec<DiscImage> = &game.releases[release];
        for (name, layout) in layouts {
            let Some(disc) = &layout.disc else { continue };
            if !wanted.iter().any(|d| d.sha1 == disc.image.sha1) {
                bail!("{release} {name}: the project's discs do not include {} ({})", disc.image.name, disc.image.sha1);
            }
            discs.insert((release.clone(), name.clone()), OpenDisc::open(roms, name, disc)?);
        }
    }
    Ok(discs)
}

/// The declarations as an extractor reads them: each asset's table as declared.
struct Declared<'a>(&'a Declarations);

impl pe2_ext::Declared for Declared<'_> {
    fn declarations(&self, kind: &str) -> Vec<(String, toml::Table)> {
        self.0.assets.iter().filter(|(_, a)| a.kind == kind).map(|(path, a)| (path.clone(), a.table.clone())).collect()
    }
}

/// A release's discs as an extractor reads them: a chunk's payload by its location.
struct Reader<'a> {
    decl: &'a Declarations,
    discs: &'a mut Discs,
    containers: &'a mut Containers,
    files: &'a HashMap<(String, u32, Option<u32>, u32), String>,
    release: &'a str,
}

impl Reader<'_> {
    /// The disc a location's slot is on, where it names one.
    fn disc_slot(&mut self, at: &toml::Table) -> pe2_ext::Result<Option<(&mut OpenDisc, String)>> {
        let at: At = at.clone().try_into().context("not a location")?;
        let (Some(disc), Some(slot)) = (at.disc, at.slot) else { return Ok(None) };
        let open = self
            .discs
            .get_mut(&(self.release.to_string(), disc.clone()))
            .with_context(|| format!("{} has no disc {disc}", self.release))?;
        Ok(Some((open, slot)))
    }
}

impl pe2_ext::Discs for Reader<'_> {
    fn read(&mut self, at: &toml::Table) -> pe2_ext::Result<Vec<u8>> {
        if let Some((disc, slot)) = self.disc_slot(at)? {
            return disc.slot(&slot);
        }
        let at: At = at.clone().try_into().context("not a location")?;
        chunk_payload(self.decl, self.discs, self.containers, self.files, self.release, &at)
    }

    fn size(&mut self, at: &toml::Table) -> pe2_ext::Result<u64> {
        if let Some((disc, slot)) = self.disc_slot(at)? {
            return disc.slot_size(&slot);
        }
        Ok(self.read(at)?.len() as u64)
    }

    fn read_range(&mut self, at: &toml::Table, offset: u64, len: usize) -> pe2_ext::Result<Vec<u8>> {
        if let Some((disc, slot)) = self.disc_slot(at)? {
            return disc.slot_range(&slot, offset, len);
        }
        let all = self.read(at)?;
        let start = usize::try_from(offset).unwrap_or(usize::MAX).min(all.len());
        Ok(all[start..all.len().min(start.saturating_add(len))].to_vec())
    }
}

/// What makes an asset's source of its native files: the extensions, and the engine importing them back - which keeps
/// the imports it checked.
struct Sourcing<'a> {
    extensions: &'a Extensions,
    engine: crate::graph::exec::Engine,
    sources: usize,
}

/// An asset as extraction lends it to its importer: its declaration and the files given.
struct Lent<'a> {
    path: &'a str,
    uid: Option<&'a str>,
    fields: toml::Table,
    declaration: &'a str,
    files: BTreeMap<String, Vec<u8>>,
}

impl pe2_ext::Source for Lent<'_> {
    fn path(&self) -> &str {
        self.path
    }

    fn uid(&self) -> Option<&str> {
        None
    }

    fn fields(&self) -> &toml::Table {
        &self.fields
    }

    fn declaration(&self) -> &str {
        self.declaration
    }

    fn has_file(&self, name: &str) -> bool {
        self.files.contains_key(name)
    }

    fn read(&self, name: &str) -> pe2_ext::Result<Vec<u8>> {
        self.files.get(name).cloned().ok_or_else(|| pe2_ext::anyhow!("{}: no file {name}", self.path))
    }
}

impl crate::graph::exec::Node for Lent<'_> {
    fn path(&self) -> &str {
        self.path
    }

    fn uid(&self) -> Option<&str> {
        self.uid
    }

    fn declaration(&self) -> &str {
        self.declaration
    }

    fn fields(&self) -> &toml::Table {
        &self.fields
    }

    fn file_sha1(&self, name: &str) -> Option<String> {
        self.files.get(name).map(|bytes| sha1_hex(bytes))
    }

    fn read(&self, name: &str) -> Result<Vec<u8>> {
        self.files.get(name).cloned().with_context(|| format!("{}: no file {name}", self.path))
    }

    fn read_content(&self, sha1: &str) -> Result<Vec<u8>> {
        let found = self.files.values().find(|bytes| sha1_hex(bytes) == sha1);
        found.cloned().with_context(|| format!("{}: no content {sha1}", self.path))
    }
}

impl Sourcing<'_> {
    /// A bundle's `node.toml` with its stack written out: the import its declaration names (`stacked`), or where it
    /// names none, the stack of its kind (`KindBuilder::stack`), where that has one.
    fn stack(&self, path: &str, kind: &str, toml: String) -> Result<String> {
        let declared: toml::Table = toml.parse()?;
        if declared.contains_key("stack") {
            return Ok(toml);
        }
        let Some(import) = declared.get("import") else {
            return Ok(match self.extensions.kind_stack(kind) {
                Some(stack) => with_stack(&toml, stack),
                None => toml,
            });
        };
        // An import undone by another makes its native form from a source: kept. A stored file named without an
        // importer, or one undone by none, is the native form as it is.
        let native = match import.get("importer").and_then(toml::Value::as_str) {
            Some(op) => self.extensions.op(op).with_context(|| format!("{path}: no extension provides {op}"))?.inverse.is_none(),
            None => true,
        };
        stacked(&toml, native).with_context(|| path.to_string())
    }

    /// A content, whole: inline, or kept in the store.
    fn read(&self, content: &crate::graph::Content) -> Result<Vec<u8>> {
        match content {
            crate::graph::Content::Inline(bytes) => Ok(bytes.clone()),
            crate::graph::Content::Stored { sha1, .. } => {
                let path = self.engine.store().context("no store")?.blob_path(sha1);
                std::fs::read(&path).with_context(|| path.display().to_string())
            }
        }
    }

    /// An asset's `node.toml` and files as its bundle holds them: its native files where its import is undone by
    /// none, else its import's source alone - written by the operation undoing it from the native files, imported back
    /// by its stack with the declaration less what the import derives, and compared, files and settings both; its
    /// import kept.
    fn bundle(&mut self, path: &str, asset: &crate::declarations::Asset, toml: String, native: pe2_ext::Made) -> Result<(String, pe2_ext::Made)> {
        let fields: toml::Table = toml.parse()?;
        let Some(import) = crate::graph::import_of(&fields) else { return Ok((toml, native)) };
        let (op, source) = (import.op.to_string(), import.source.to_string());
        let def = self.extensions.op(&op).with_context(|| format!("{path}: no extension provides {op}"))?;
        let Some(inverse) = def.inverse.clone() else { return Ok((toml, native)) };
        let pe2_ext::Type::Named(ty) = &def.output else { bail!("{path}: {op} makes {}, no extension's type", def.output) };
        let lent = Lent { path, uid: Some(&asset.uid), fields: fields.clone(), declaration: &toml, files: BTreeMap::new() };
        let files = native.iter().map(|(name, bytes)| pe2_ext::File::new(name, bytes.clone())).collect();
        let native_form = crate::graph::Value::Typed(pe2_ext::Typed { ty: ty.clone(), files, ..pe2_ext::Typed::default() });
        let written = self.engine.run(&inverse, vec![vec![native_form]], &toml::Table::new(), &lent);
        let bytes = match written.with_context(|| format!("{path}: {inverse} could not write its source"))? {
            crate::graph::Value::Typed(written) => self.read(&written.file()?.content)?,
            other => bail!("{path}: {inverse} wrote {}, not a file", other.ty()),
        };
        // What the bundle says: the declaration without what the source holds. Imported back by its stack, as the
        // project will - which keeps the import.
        let written = crate::imports::without_derived(&toml, &def.derives)?;
        let lent = Lent {
            path,
            uid: Some(&asset.uid),
            fields: written.parse()?,
            declaration: &written,
            files: BTreeMap::from([(source.clone(), bytes.clone())]),
        };
        let (plan, wrong) = crate::graph::Plan::of(&lent.fields, &self.engine.ops, &[]);
        if let Some(wrong) = wrong.first() {
            bail!("{path}: {}: {}", wrong.at, wrong.what);
        }
        let result = plan.result_output().map(|o| plan.outputs[o].name.clone()).with_context(|| format!("{path}: its stack keeps no result"))?;
        let given = self
            .engine
            .output(&plan, &lent, &result, true)
            .with_context(|| format!("{path}: {op} could not import its source"))?;
        let Some(crate::graph::Value::Typed(made)) = given.value.as_deref() else {
            bail!("{path}: its import made no native form");
        };
        let made_map: BTreeMap<String, Vec<u8>> =
            made.files.iter().map(|f| Ok((f.name.clone(), self.read(&f.content)?))).collect::<Result<_>>()?;
        let native_map: BTreeMap<String, Vec<u8>> = native.into_iter().collect();
        if made_map != native_map {
            let differ: Vec<&String> = native_map
                .keys()
                .chain(made_map.keys())
                .filter(|n| native_map.get(*n) != made_map.get(*n))
                .collect();
            bail!("{path}: its {source} does not import back into the disc's files ({differ:?} differ)");
        }
        let resolved = crate::imports::resolve(&lent.fields, &made.settings, &def.derives);
        if resolved.settings != fields || !resolved.orphans.is_empty() {
            bail!("{path}: the settings its {source} gives are not the declaration's (orphaned: {:?})", resolved.orphans);
        }
        self.sources += 1;
        Ok((written, vec![(source, bytes)]))
    }
}

/// A bundle's `node.toml` with a stack added, before the originals.
pub(crate) fn with_stack(toml: &str, stack: &str) -> String {
    let body = toml.find("\n[original.").map_or(toml.len(), |i| i + 1);
    let (body, originals) = toml.split_at(body);
    let separator = if originals.is_empty() { "" } else { "\n" };
    format!("{}\n\n{}\n{separator}{originals}", body.trim_end(), stack.trim())
}

/// Every asset located in the releases, read from its slots, checked and written as a bundle. Returns their count,
/// and how many were written as their sources.
fn write_assets(
    layer: &Path,
    decl: &Declarations,
    releases: &[String],
    discs: &mut Discs,
    mut sourcing: Sourcing,
) -> Result<(usize, usize)> {
    // Where each file asset is, to find a chunk slot's file.
    let mut files = HashMap::new();
    for (path, asset) in &decl.assets {
        for (release, loc) in &asset.from {
            for at in loc.at.iter().filter(|a| a.disc.is_none() && a.slot.is_none()) {
                if let (Some(stage), Some(file)) = (at.stage, at.file) {
                    files.insert((release.clone(), stage, at.folder, file), path.clone());
                }
            }
        }
    }
    let mut containers = Containers { open: HashMap::new() };
    let extensions = sourcing.extensions;
    // An extractor's session, begun with the first of its assets.
    let mut sessions: HashMap<String, Box<dyn pe2_ext::Extraction>> = HashMap::new();
    let mut written = 0;
    for (path, asset) in &decl.assets {
        if let Some(extractor) = extensions.extractor_for(&asset.kind, |field| asset.table.contains_key(field)) {
            let name = extractor.name.to_string();
            if !sessions.contains_key(&name) {
                let session = (extractor.begin)(&Declared(decl)).with_context(|| format!("{name} could not begin"))?;
                sessions.insert(name.clone(), session);
            }
            let session = sessions.get_mut(&name).context("begun")?;
            let mut made = Vec::new();
            let mut originals = vec![];
            for release in releases.iter().filter(|r| asset.from.contains_key(*r)) {
                let mut reader = Reader { decl, discs, containers: &mut containers, files: &files, release };
                let got = session.extract(path, release, &mut reader).with_context(|| format!("{path}: {release}"))?;
                if made.is_empty() {
                    made = got;
                }
                originals.extend(asset.from[release].sha1.iter().map(|s| (release.clone(), s.clone())));
            }
            let toml = sourcing.stack(path, &asset.kind, asset_toml(&asset.body, None, &originals)?)?;
            let (toml, made) = sourcing.bundle(path, asset, toml, made)?;
            let mut files: Vec<(&str, &[u8])> = vec![("node.toml", toml.as_bytes())];
            files.extend(made.iter().map(|(n, b)| (n.as_str(), b.as_slice())));
            write_bundle(layer, path, &files)?;
            written += 1;
            continue;
        }
        // A container assembled from its movies (a disc's movie file) has no bytes of its own: its stack joins its
        // entries' movies, which `make_assembled` holds to the disc's.
        if asset.table.contains_key("entries") && asset.kind == "stream" {
            let originals: Vec<(String, String)> = releases
                .iter()
                .filter_map(|r| Some((r.clone(), asset.from.get(r)?.sha1.clone()?)))
                .collect();
            let toml = sourcing.stack(path, &asset.kind, asset_toml(&asset.body, None, &originals)?)?;
            write_bundle(layer, path, &[("node.toml", toml.as_bytes())])?;
            written += 1;
            continue;
        }
        let mut native: Option<Vec<u8>> = None;
        let mut originals = vec![];
        for release in releases {
            let Some(loc) = asset.from.get(release) else { continue };
            for at in &loc.at {
                if at.disc.is_none() && at.slot.is_some() {
                    let want = loc.sha1.as_deref().with_context(|| format!("{path}: {release} gives no sha1"))?;
                    let bytes = chunk_payload(decl, discs, &mut containers, &files, release, at)
                        .with_context(|| format!("{path}: {release}"))?;
                    let got = sha1_hex(&bytes);
                    if got != want {
                        bail!("{path}: {release} {at:?} has sha1 {got}, the declaration {want}");
                    }
                    native.get_or_insert(bytes);
                    continue;
                }
                let Some(disc_name) = &at.disc else { continue };
                let want = loc.sha1.as_deref().with_context(|| format!("{path}: {release} gives no sha1"))?;
                for slot in at.slots() {
                    let disc = discs
                        .get_mut(&(release.clone(), disc_name.clone()))
                        .with_context(|| format!("{path}: {release} has no disc {disc_name}"))?;
                    let bytes = disc.slot(slot).with_context(|| format!("{path}: {disc_name} {slot}"))?;
                    let got = sha1_hex(&bytes);
                    if got != want {
                        bail!("{path}: {release} {disc_name} {slot} has sha1 {got}, the declaration {want}");
                    }
                    native.get_or_insert(bytes);
                }
            }
            if let Some(sha1) = &loc.sha1 {
                originals.push((release.clone(), sha1.clone()));
            }
        }
        let name = format!("native.{}", asset.format.as_deref().unwrap_or("bin"));
        let toml = sourcing.stack(path, &asset.kind, asset_toml(&asset.body, native.as_ref().map(|_| name.as_str()), &originals)?)?;
        let mut files: Vec<(&str, &[u8])> = vec![("node.toml", toml.as_bytes())];
        if let Some(native) = &native {
            files.push((&name, native));
        }
        write_bundle(layer, path, &files)?;
        written += 1;
    }
    Ok((written, sourcing.sources))
}

/// A bundle of the layer as written, as a node of the work graph: its declaration and stored files as on disk, each
/// file hashed once, when first asked.
struct Written {
    path: String,
    uid: String,
    dir: PathBuf,
    declaration: String,
    fields: toml::Table,
    sha1s: std::sync::Mutex<HashMap<String, String>>,
}

impl Written {
    fn open(layer: &Path, path: &str, uid: &str) -> Result<Written> {
        let dir = layer.join(format!("{path}{}", crate::index::BUNDLE_SUFFIX));
        let declaration = fs::read_to_string(dir.join("node.toml")).with_context(|| format!("{path}: its node.toml"))?;
        let fields = declaration.parse().with_context(|| format!("{path}: its node.toml"))?;
        Ok(Written { path: path.to_string(), uid: uid.to_string(), dir, declaration, fields, sha1s: std::sync::Mutex::default() })
    }
}

impl crate::graph::exec::Node for Written {
    fn path(&self) -> &str {
        &self.path
    }

    fn uid(&self) -> Option<&str> {
        Some(&self.uid)
    }

    fn declaration(&self) -> &str {
        &self.declaration
    }

    fn fields(&self) -> &toml::Table {
        &self.fields
    }

    fn file_sha1(&self, name: &str) -> Option<String> {
        let mut sha1s = self.sha1s.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(sha1) = sha1s.get(name) {
            return Some(sha1.clone());
        }
        let sha1 = sha1_file(&self.dir.join(name)).ok()?;
        sha1s.insert(name.to_string(), sha1.clone());
        Some(sha1)
    }

    fn read(&self, name: &str) -> Result<Vec<u8>> {
        let path = self.dir.join(name);
        fs::read(&path).with_context(|| path.display().to_string())
    }

    fn file_size(&self, name: &str) -> Option<u64> {
        fs::metadata(self.dir.join(name)).ok().map(|m| m.len())
    }

    fn read_head(&self, name: &str, len: usize) -> Vec<u8> {
        use std::io::Read;
        let mut head = Vec::new();
        if let Ok(file) = fs::File::open(self.dir.join(name)) {
            let _ = file.take(len as u64).read_to_end(&mut head);
        }
        head
    }

    fn content_path(&self, sha1: &str) -> Option<PathBuf> {
        let sha1s = self.sha1s.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        sha1s.iter().find(|(_, s)| *s == sha1).map(|(name, _)| self.dir.join(name))
    }
}

/// The layer's bundles as the work graph reaches them, by uid or path: each read from disk when first asked for.
struct Layer<'a> {
    root: &'a Path,
    uids: HashMap<&'a str, &'a str>,
    /// By path: its uid, and the node once read.
    nodes: HashMap<&'a str, (&'a str, std::sync::OnceLock<Option<Written>>)>,
    ops: &'a crate::graph::Ops,
}

impl<'a> Layer<'a> {
    fn new(root: &'a Path, decl: &'a Declarations, ops: &'a crate::graph::Ops) -> Layer<'a> {
        let uids = decl.assets.iter().map(|(path, a)| (a.uid.as_str(), path.as_str())).collect();
        let nodes = decl.assets.iter().map(|(path, a)| (path.as_str(), (a.uid.as_str(), std::sync::OnceLock::new()))).collect();
        Layer { root, uids, nodes, ops }
    }
}

impl crate::graph::exec::World for Layer<'_> {
    fn node(&self, reference: &toml::Value) -> Option<(&dyn crate::graph::exec::Node, crate::graph::Plan)> {
        let path = match reference.as_str().filter(|r| !r.starts_with("uid://")) {
            Some(path) => path,
            None => self.uids.get(crate::index::reference(reference)?.0.as_str())?,
        };
        let (path, (uid, slot)) = self.nodes.get_key_value(path)?;
        let node = slot.get_or_init(|| Written::open(self.root, path, uid).ok()).as_ref()?;
        let (plan, _) = crate::graph::Plan::of(&node.fields, self.ops, &[]);
        Some((node as &dyn crate::graph::exec::Node, plan))
    }
}

/// Make each container assembled from its movies (a disc's movie file: its `entries`, each naming a movie) by its
/// stack, as the project will - its entries' movies, as written, joined - holding it to the disc's file, and keeping
/// it in the store. Returns how many were made.
fn make_assembled(layer: &Path, decl: &Declarations, releases: &[String], engine: &crate::graph::exec::Engine) -> Result<usize> {
    let world = Layer::new(layer, decl, &engine.ops);
    let reach = crate::graph::exec::Reach { engine, world: &world };
    let mut made = 0;
    for (path, asset) in &decl.assets {
        if asset.kind != "stream" || !asset.table.contains_key("entries") {
            continue;
        }
        let (node, plan) =
            crate::graph::exec::World::node(&world, &toml::Value::String(path.clone())).with_context(|| format!("{path}: not written"))?;
        let result = plan.result_output().map(|o| plan.outputs[o].name.clone()).with_context(|| format!("{path}: its stack keeps no result"))?;
        let given = engine.output_in(Some(reach), &plan, node, &result, None, true).with_context(|| format!("{path}: its movies could not be joined"))?;
        let got = match given.value.as_deref() {
            Some(crate::graph::Value::Typed(typed)) => match &typed.file()?.content {
                crate::graph::Content::Stored { sha1, .. } => sha1.clone(),
                crate::graph::Content::Inline(bytes) => sha1_hex(bytes),
            },
            _ => bail!("{path}: its stack made no file"),
        };
        for release in releases {
            let Some(want) = asset.from.get(release).and_then(|l| l.sha1.as_deref()) else { continue };
            if got != want {
                bail!("{path}: its movies, joined in order, have sha1 {got}; the disc's file {want}");
            }
        }
        made += 1;
    }
    Ok(made)
}

/// The releases' discs and containers as bundles, their slots filled. Returns their count.
fn write_layouts(layer: &Path, decl: &Declarations, releases: &[String], discs: &Discs) -> Result<usize> {
    let mut written = 0;
    for release in releases {
        for (name, layout) in &decl.layouts[release] {
            for at in &layout.at {
                let disc_name =
                    at.disc.as_deref().with_context(|| format!("disc/{release}/{name}: an at with no disc"))?;
                let disc = discs
                    .get(&(release.clone(), disc_name.to_string()))
                    .with_context(|| format!("disc/{release}/{name}: {release} has no disc {disc_name}"))?;
                for slot in at.slots() {
                    if !disc.files.contains_key(slot) {
                        bail!("disc/{release}/{name}: {disc_name} has no file {slot}");
                    }
                }
            }
            let toml = layout_toml(&layout.text)?;
            write_bundle(layer, &format!("disc/{release}/{name}"), &[("node.toml", toml.as_bytes())])?;
            written += 1;
        }
    }
    Ok(written)
}

/// What made the layer: the declarations and the images, in the layer's `extraction.toml`.
fn write_record(layer: &Path, decl_root: &Path, releases: &[String], discs: &Discs) -> Result<()> {
    let mut lines = vec![
        "# What made this layer (pe2-cli project extract): this machine's paths, so it stays in the layer.".to_string(),
        String::new(),
        format!("declarations = {}", quote(&fs::canonicalize(decl_root)?.to_string_lossy())),
    ];
    for release in releases {
        lines.extend([String::new(), format!("[releases.{release}]"), "images = [".into()]);
        for ((r, _), disc) in discs {
            if r == release {
                lines.push(format!(
                    "    {{ path = {}, sha1 = {} }},",
                    quote(&disc.path.to_string_lossy()),
                    quote(&disc.layout.image.sha1)
                ));
            }
        }
        lines.push("]".into());
    }
    fs::write(layer.join("extraction.toml"), lines.join("\n") + "\n")?;
    Ok(())
}

/// The game layer's changeset, by real paths (a bundle's with `.node`): every asset under its declared uid, every
/// folder and the record under new ones, the layer read-only.
fn write_changeset(project: &Project, layer: &str, decl: &Declarations, releases: &[String]) -> Result<()> {
    let mut changeset = Changeset { read_only: true, ..Changeset::default() };
    for (path, asset) in &decl.assets {
        changeset.entries.insert(asset.uid.clone(), format!("{path}{}", crate::index::BUNDLE_SUFFIX));
    }
    for release in releases {
        for (name, layout) in &decl.layouts[release] {
            changeset
                .entries
                .insert(layout.uid.clone(), format!("disc/{release}/{name}{}", crate::index::BUNDLE_SUFFIX));
        }
    }
    let mut folders: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for path in changeset.entries.values() {
        let mut at = path.as_str();
        while let Some((parent, _)) = at.rsplit_once('/') {
            folders.insert(parent.to_string());
            at = parent;
        }
    }
    let mut taken: std::collections::HashSet<String> = changeset.entries.keys().cloned().collect();
    for path in folders.into_iter().chain(["extraction.toml".to_string()]) {
        let id =
            uid::text(uid::new(|id| taken.contains(&uid::text(id))).map_err(|e| anyhow::anyhow!("a new uid: {e}"))?);
        taken.insert(id.clone());
        changeset.entries.insert(id, path);
    }
    changeset.save(&project.layer(layer)?.changeset)
}

/// (Re)write a project's game layer from its releases' discs, holding the project's lock.
///
/// # Errors
/// The project names no game layer or an unknown release, the declarations do not load, a disc is missing or differs
/// from its layout, an asset's bytes differ from its sha1, or a file cannot be written.
pub fn extract(project: &Project, options: &Options, lock: &Lock, extensions: &Extensions) -> Result<Report> {
    let game = project.game.as_ref().context("the project has no [game]")?;
    let releases: Vec<String> =
        if options.releases.is_empty() { game.releases.keys().cloned().collect() } else { options.releases.clone() };
    for r in &releases {
        if !game.releases.contains_key(r) {
            bail!("{r}: not a release of this project ([game.releases])");
        }
    }
    let decl_root = match &options.declarations {
        Some(p) => p.clone(),
        None => project.resolve(&game.declarations)?,
    };
    let decl = Declarations::load(&decl_root)?;
    let roms = options.roms.clone().unwrap_or_else(|| game.roms.clone());
    let layer = project.layer(&game.layer)?.path.clone();
    let (root, abs_layer) = (fs::canonicalize(&project.root)?, std::path::absolute(&layer)?);
    if abs_layer == root || !abs_layer.starts_with(&root) {
        bail!("{}: the game layer must be a folder inside the project", layer.display());
    }
    if fs::symlink_metadata(&layer).is_ok_and(|m| m.file_type().is_symlink()) {
        bail!("{} is a link: the game layer is the extraction's own folder", layer.display());
    }
    // Every disc is opened first: a missing or foreign image stops extraction before the layer is touched.
    let mut discs = open_discs(game, &decl, &releases, &roms)?;
    if layer.exists() {
        fs::remove_dir_all(&layer)?; // the layer is the extraction's alone
    }
    fs::create_dir_all(&layer)?;
    let cache = crate::cache::Cache::open(project, lock)?;
    let sourcing = Sourcing { extensions, engine: Imports::open(&cache)?.engine(extensions), sources: 0 };
    let (assets, sources) = write_assets(&layer, &decl, &releases, &mut discs, sourcing)?;
    let layouts = write_layouts(&layer, &decl, &releases, &discs)?;
    make_assembled(&layer, &decl, &releases, &Imports::open(&cache)?.engine(extensions))?;
    write_record(&layer, &decl_root, &releases, &discs)?;
    write_changeset(project, &game.layer, &decl, &releases)?;
    Ok(Report { layer, assets, layouts, sources })
}
