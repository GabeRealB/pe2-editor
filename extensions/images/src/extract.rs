//! Extracting textures and atlases (`pe2.images@textures`) in their stored form: an image chunk is LZSS without a
//! matching compressor, so the stored texels and colors are the native form, and the source is written from them on
//! extraction (`ora`). An image's texels are read from the pages its location names - a multi-page image's from as many
//! pages as its width needs, side by side, across the file's image chunks where it holds them in two - and each placed
//! palette's colors from the file slot that uploads them (a palette chunk, a bundle's palette resource, or the image's
//! own pages). Each is checked against its declared sha1 and made as those bytes: `native.texels` (a byte a texel at 4
//! and 8 bits, a little-endian halfword at 16, row by row) and `native.<palette>.colors` (ABGR1555 halfwords).
//!
//! # VRAM and images, as the declarations say them
//!
//! VRAM is 1024 x 512 halfwords. A `vram_slot` is a rectangle of it, `rect = [x, y, w, h]`, that uploads are placed
//! in: a slot `within` another is part of it; one that `borrows` another overwrites it while it is up.
//!
//! A texture is pixels in a `mode` - `indexed_4bpp`, `indexed_8bpp` or `direct_16bpp` - and a `size` in texels,
//! uploaded to a `page` - the VRAM slot and the halfword `at` in it - filling as many 64-halfword VRAM pages, side by
//! side, as its width needs; and its `palettes`, each named, placed where its colors are uploaded (`slot`, `at`), or
//! uploaded by an executable (`executable`), or none known (`no_palette`). Its location lists where its first page is
//! read, and the sha1s of its texels and of each palette's colors. An atlas is a texture whose `cells` are drawn
//! separately, each a named `rect` in texels and the `palette` it is drawn with; a cell of another depth states its
//! `mode`, its rectangle in texels of that depth. A palette holds the colors of the deepest cell drawn with it.
//!
//! A file's image slot lists its `pages` - an `image`, or a place where the page holds palettes only - each image
//! filling the pages its width needs; where a file holds an image in two chunks, each entry says which of its pages
//! that chunk holds (`first`, `count`). A palette slot (or a palette resource of a bundle) lists its `palettes`: an
//! image's palette by reference to its part (`palette = { uid, path, part = "palettes/<name>" }`), or a `name`d run of `entries` colors placed by `slot` and `at`, which
//! no image owns: an image palette no slot names takes its colors from the run uploaded at its place (the system's
//! text palettes, which every dialogue atlas is drawn with).

use pe2_ext::{Declared, Discs, Extraction, Made, Result, Table, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use crate::texels_per_halfword;

/// A reference as declarations write it: a uid and a bundle's `res://` path, and the output of it it names.
#[derive(Clone, Debug, Deserialize)]
struct Reference {
    path: String,
    output: Option<String>,
}

impl Reference {
    /// The declaration path (`group/name`) it names.
    fn target(&self) -> &str {
        self.path.trim_start_matches("res://").trim_end_matches(".node")
    }
}

/// Where extraction finds an asset or a chunk.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct At {
    #[serde(skip_serializing_if = "Option::is_none")]
    slot: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stage: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    folder: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    file: Option<u32>,
    /// An image chunk slot's page.
    #[serde(skip_serializing_if = "Option::is_none")]
    page: Option<usize>,
}

#[derive(Clone, Debug, Deserialize)]
struct Location {
    #[serde(default)]
    at: Vec<At>,
    sha1: Option<String>,
    /// Where in a package its texels are, for one the package holds rather than uploads: they are the bytes there.
    offset: Option<usize>,
    size: Option<usize>,
    #[serde(default)]
    palettes: BTreeMap<String, String>,
}

/// What extraction reads of a declaration: where it is per release and a file's slots.
#[derive(Clone, Debug, Deserialize)]
struct Declaration {
    #[serde(default)]
    from: BTreeMap<String, Location>,
    #[serde(default)]
    slots: Vec<ChunkSlot>,
}

#[derive(Clone, Debug, Deserialize)]
struct ChunkSlot {
    name: String,
    #[serde(default)]
    pages: Vec<PageEntry>,
    #[serde(default)]
    palettes: Vec<PaletteEntry>,
    #[serde(default)]
    resources: Vec<Resource>,
}

#[derive(Clone, Debug, Deserialize)]
struct PageEntry {
    image: Option<Reference>,
    first: Option<usize>,
    count: Option<usize>,
}

/// A palette a slot uploads: an image's, by reference to its part (`palettes/<name>`), or a run of colors placed
/// by `slot` and `at`, which no image owns.
#[derive(Clone, Debug, Deserialize)]
struct PaletteEntry {
    palette: Option<Reference>,
    slot: Option<Reference>,
    at: Option<[u32; 2]>,
}

#[derive(Clone, Debug, Deserialize)]
struct Resource {
    index: usize,
    #[serde(default)]
    palettes: Vec<PaletteEntry>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
enum Mode {
    #[serde(rename = "indexed_4bpp")]
    Indexed4,
    #[serde(rename = "indexed_8bpp")]
    Indexed8,
    #[serde(rename = "direct_16bpp")]
    Direct16,
}

impl Mode {
    fn bpp(self) -> u8 {
        match self {
            Mode::Indexed4 => 4,
            Mode::Indexed8 => 8,
            Mode::Direct16 => 16,
        }
    }
}

/// A texture's or an atlas's settings, as extraction reads them.
#[derive(Clone, Debug, Deserialize)]
struct Texture {
    mode: Mode,
    size: [u32; 2],
    #[serde(default)]
    palettes: Table,
    #[serde(default)]
    cells: Table,
}

#[derive(Clone, Debug, Deserialize)]
struct Palette {
    slot: Option<Reference>,
    at: Option<[u32; 2]>,
}

#[derive(Clone, Debug, Deserialize)]
struct Cell {
    palette: Option<String>,
    mode: Option<Mode>,
}

#[derive(Clone, Debug, Deserialize)]
struct VramSlot {
    rect: [u32; 4],
}

impl Texture {
    /// Its palettes, in the order declared.
    fn palettes(&self) -> Result<Vec<(String, Palette)>> {
        self.palettes.iter().map(|(name, p)| Ok((name.clone(), p.clone().try_into()?))).collect()
    }

    /// How many colors a palette holds: the deepest of the cells drawn with it, or the image's depth.
    fn entries(&self, palette: &str) -> usize {
        let cells = self.cells.values().filter_map(|c| c.clone().try_into::<Cell>().ok());
        let drawn = cells.filter(|c| c.palette.as_deref() == Some(palette));
        drawn.map(|c| entries_at(c.mode.unwrap_or(self.mode).bpp())).max().unwrap_or(entries_at(self.mode.bpp()))
    }
}

fn entries_at(bpp: u8) -> usize {
    codecs::texture::entries(bpp)
}

/// Where a palette's colors are stored: a file asset's slot, a resource of it, or its image pages.
struct PaletteSource {
    file: String,
    slot: String,
    resource: Option<usize>,
    in_pages: bool,
}

/// A session: what extraction needs of the other declarations, read once.
struct Textures {
    /// The images, by path.
    images: BTreeMap<String, (Declaration, Table)>,
    /// The file assets, by path.
    files: BTreeMap<String, Declaration>,
    vram: HashMap<String, VramSlot>,
    /// Where every image palette's colors are, by image and palette name.
    palettes: HashMap<(String, String), PaletteSource>,
    /// The named runs a slot uploads (colors no image owns: the system's text palettes), by VRAM slot and place.
    runs: HashMap<(String, [u32; 2]), PaletteSource>,
    /// The file assets by release and place.
    places: HashMap<(String, u32, Option<u32>, u32), String>,
}

/// Begin a session: the images, the files and the VRAM slots declared.
///
/// # Errors
/// A declaration has not the shape its kind gives it.
pub fn begin(declared: &dyn Declared) -> Result<Box<dyn Extraction>> {
    let parse = |path: &str, table: Table| -> Result<Declaration> {
        Table::try_into(table).map_err(|e| anyhow!("{path}: {e}"))
    };
    let mut images = BTreeMap::new();
    for (path, table) in declared.declarations("image") {
        let declaration = parse(&path, table.clone())?;
        images.insert(path, (declaration, table));
    }
    let mut files = BTreeMap::new();
    for (path, table) in declared.declarations("file") {
        let declaration = parse(&path, table)?;
        files.insert(path, declaration);
    }
    let mut vram = HashMap::new();
    for (path, table) in declared.declarations("vram_slot") {
        let slot: VramSlot = table.try_into().map_err(|e| anyhow!("{path}: {e}"))?;
        vram.insert(path, slot);
    }
    let (mut palettes, mut runs, mut places) = (HashMap::new(), HashMap::new(), HashMap::new());
    for (path, file) in &files {
        let mut note = |slot: &str, resource: Option<usize>, in_pages: bool, entries: &[PaletteEntry]| {
            for e in entries {
                let source = || PaletteSource { file: path.clone(), slot: slot.to_string(), resource, in_pages };
                let named = e.palette.as_ref().and_then(|p| Some((p.target(), p.output.as_deref()?.strip_prefix("palettes/")?)));
                match (named, &e.slot, e.at) {
                    (Some((image, palette)), ..) => {
                        palettes.entry((image.to_string(), palette.to_string())).or_insert_with(source);
                    }
                    (None, Some(vram), Some(at)) => {
                        runs.entry((vram.target().to_string(), at)).or_insert_with(source);
                    }
                    _ => {}
                }
            }
        };
        for slot in &file.slots {
            note(&slot.name, None, !slot.pages.is_empty(), &slot.palettes);
            for r in &slot.resources {
                note(&slot.name, Some(r.index), false, &r.palettes);
            }
        }
        for (release, loc) in &file.from {
            for at in &loc.at {
                if let (Some(stage), Some(number)) = (at.stage, at.file) {
                    places.insert((release.clone(), stage, at.folder, number), path.clone());
                }
            }
        }
    }
    Ok(Box::new(Textures { images, files, vram, palettes, runs, places }))
}

fn read(discs: &mut dyn Discs, at: &At) -> Result<Vec<u8>> {
    discs.read(&Table::try_from(at)?)
}

impl Extraction for Textures {
    fn extract(&mut self, path: &str, release: &str, discs: &mut dyn Discs) -> Result<Made> {
        let (asset, table) = self.images.get(path).ok_or_else(|| anyhow!("{path}: no such image"))?;
        let loc = asset.from.get(release).ok_or_else(|| anyhow!("not in this release"))?;
        let at = loc.at.first().ok_or_else(|| anyhow!("read from nowhere"))?;
        if let (Some(offset), Some(size)) = (loc.offset, loc.size) {
            let inflated = formats::lzss::decode_stream(&read(discs, at)?, 0).0;
            let held = inflated.get(offset..offset + size).ok_or_else(|| anyhow!("past the end of its package"))?;
            check(held, loc.sha1.as_ref().ok_or_else(|| anyhow!("no texels sha1"))?, "texels")?;
            return Ok(vec![("native.texels".to_string(), held.to_vec())]);
        }
        let pages = codecs::texture::pages(&read(discs, at)?)?;
        let first = at.page.unwrap_or(0);
        let tex: Texture = table.clone().try_into()?;
        let [w, h] = tex.size.map(|n| n as usize);
        let bpp = tex.mode.bpp();
        let page_width = 64 * texels_per_halfword(bpp);
        let count = w.div_ceil(page_width).max(1);
        let mut columns = Vec::new();
        for page in self.image_pages(discs, path, release, at, pages, first, count)? {
            columns.push((codecs::texture::texels(&page, bpp, (0, 0, page_width, h)), page.rows));
        }
        let mut texels = Vec::with_capacity(w * h);
        for y in 0..h {
            for x in 0..w {
                texels.push(columns[x / page_width].0[y * page_width + x % page_width]);
            }
        }
        let stored = h.min(columns[0].1);
        let want = loc.sha1.as_ref().ok_or_else(|| anyhow!("no texels sha1"))?;
        check(&codecs::texture::texel_bytes(&texels[..w * stored], bpp), want, "texels")?;
        let mut out = vec![("native.texels".to_string(), codecs::texture::texel_bytes(&texels, bpp))];
        for (name, palette) in tex.palettes()? {
            let (Some(slot), Some(place)) = (&palette.slot, palette.at) else { continue }; // an executable's
            // A slot naming the image's palette uploads it; else a named run at its place (a palette the system
            // uploads for every image drawn with it).
            let source = self
                .palettes
                .get(&(path.to_string(), name.clone()))
                .or_else(|| self.runs.get(&(slot.target().to_string(), place)))
                .ok_or_else(|| anyhow!("{name}: no file uploads it"))?;
            let vram = self.place_of(slot.target(), place)?;
            let colors = self
                .colors(source, release, discs, vram, tex.entries(&name))
                .map_err(|e| anyhow!("{name} ({} {}): {e:#}", source.file, source.slot))?;
            let bytes: Vec<u8> = colors.iter().flat_map(|c| c.to_le_bytes()).collect();
            let declared = loc.palettes.get(&name).ok_or_else(|| anyhow!("{name}: no sha1"))?;
            check(&bytes, declared, &name)?;
            out.push((format!("native.{name}.colors"), bytes));
        }
        Ok(out)
    }
}

impl Textures {
    /// A placement's VRAM halfword.
    fn place_of(&self, slot: &str, at: [u32; 2]) -> Result<(usize, usize)> {
        let vram = self.vram.get(slot).ok_or_else(|| anyhow!("no VRAM slot {slot}"))?;
        Ok(((vram.rect[0] + at[0]) as usize, (vram.rect[1] + at[1]) as usize))
    }

    /// A multi-page image's pages in order: from its location's chunk on, and - where that chunk holds not all of
    /// them - from the file's later image chunks that hold the rest (an entry's `first` and `count`).
    #[expect(clippy::too_many_arguments, reason = "where the pages are, and how many")]
    fn image_pages(
        &self,
        discs: &mut dyn Discs,
        path: &str,
        release: &str,
        at: &At,
        pages: Vec<codecs::texture::Page>,
        first: usize,
        count: usize,
    ) -> Result<Vec<codecs::texture::Page>> {
        let key = (release.to_string(), at.stage.unwrap_or(0), at.folder, at.file.unwrap_or(0));
        let file = self.places.get(&key).and_then(|f| self.files.get(f));
        let holding = |slot: &ChunkSlot| -> Option<(usize, usize)> {
            let entry = slot.pages.iter().find(|e| e.image.as_ref().is_some_and(|r| r.target() == path))?;
            let from = entry.first.unwrap_or(0);
            Some((from, entry.count.unwrap_or(count - from)))
        };
        let held_here = file
            .and_then(|f| f.slots.iter().find(|s| Some(s.name.as_str()) == at.slot.as_deref()))
            .and_then(holding)
            .map_or(count, |(_, n)| n);
        let mut out: Vec<codecs::texture::Page> = pages.into_iter().skip(first).take(held_here).collect();
        if out.len() < count
            && let Some(file) = file
        {
            let after = file.slots.iter().skip_while(|s| Some(s.name.as_str()) != at.slot.as_deref()).skip(1);
            for slot in after {
                let Some((from, n)) = holding(slot) else { continue };
                if from != out.len() {
                    continue;
                }
                let payload = read(discs, &At { slot: Some(slot.name.clone()), page: None, ..at.clone() })?;
                let stored = codecs::texture::pages(&payload)?;
                let offset: usize = slot
                    .pages
                    .iter()
                    .take_while(|e| e.image.as_ref().is_none_or(|r| r.target() != path))
                    .map(|e| self.entry_pages(e))
                    .sum();
                out.extend(stored.into_iter().skip(offset).take(n));
                if out.len() >= count {
                    break;
                }
            }
        }
        if out.len() < count {
            bail!("{} of its {count} pages stored", out.len());
        }
        Ok(out)
    }

    /// How many stored pages an image slot's entry fills.
    fn entry_pages(&self, entry: &PageEntry) -> usize {
        let Some((_, table)) = entry.image.as_ref().and_then(|r| self.images.get(r.target())) else { return 1 };
        let total = table
            .clone()
            .try_into::<Texture>()
            .ok()
            .filter(|_| !table.contains_key("cells"))
            .map_or(1, |t| (t.size[0] as usize).div_ceil(64 * texels_per_halfword(t.mode.bpp())).max(1));
        let from = entry.first.unwrap_or(0);
        entry.count.unwrap_or(total - from)
    }

    /// The `n` colors at VRAM (x, y) that a palette source stores.
    fn colors(
        &self,
        source: &PaletteSource,
        release: &str,
        discs: &mut dyn Discs,
        (x, y): (usize, usize),
        n: usize,
    ) -> Result<Vec<u16>> {
        let file = self.files.get(&source.file).ok_or_else(|| anyhow!("no file {}", source.file))?;
        let place = file.from.get(release).and_then(|l| l.at.first()).ok_or_else(|| anyhow!("the file is in no place"))?;
        let payload = read(discs, &At { slot: Some(source.slot.clone()), ..place.clone() })?;
        if source.in_pages {
            for p in codecs::texture::pages(&payload)? {
                let (px, py) = (usize::from(p.x), usize::from(p.y));
                if (px..px + 64).contains(&x) && (py..py + p.rows).contains(&y) {
                    let at = (y - py) * 64 + x - px;
                    return p.words.get(at..at + n).map(<[u16]>::to_vec).ok_or_else(|| anyhow!("past the page"));
                }
            }
            bail!("no page holds ({x}, {y})");
        }
        let bytes = match source.resource {
            Some(r) => formats::bundle::resources(&payload)
                .get(&r)
                .map(|(_, b)| b.to_vec())
                .ok_or_else(|| anyhow!("no such resource"))?,
            None => payload,
        };
        let block = codecs::texture::palette(&bytes)?;
        let (bx, by) = (usize::from(block.x), usize::from(block.y));
        block
            .rows
            .get(y.wrapping_sub(by))
            .and_then(|row| row.get(x.wrapping_sub(bx)..x.wrapping_sub(bx).wrapping_add(n)))
            .map(<[u16]>::to_vec)
            .ok_or_else(|| anyhow!("({x}, {y}) is outside the stored palettes"))
    }
}

fn check(bytes: &[u8], want: &str, what: &str) -> Result<()> {
    let got = Sha1::digest(bytes).iter().fold(String::new(), |mut hex, b| {
        let _ = write!(hex, "{b:02x}");
        hex
    });
    if got != want {
        bail!("{what}: sha1 {got}, declared {want}");
    }
    Ok(())
}
