//! Textures and atlases - images by role, declared as the declarations module describes - extracted in their stored form: an image
//! chunk is LZSS without a matching compressor, so an image cannot be packed back exactly and extraction writes no
//! editable form (a full-color picture is an import, for modding). An image's texels are read from the pages its
//! location names - a multi-page image's from as many pages as its width needs, side by side, across the file's image
//! chunks where it holds them in two - and each placed palette's colors from the file slot that uploads them (a
//! palette chunk, a bundle's palette resource, or the image's own pages). Each is checked against its declared sha1
//! and written as those bytes: `native.texels` (a byte a texel at 4 and 8 bits, a little-endian halfword at 16, row by
//! row) and `native.<palette>.colors` (ABGR1555 halfwords).

use anyhow::{Context, Result, bail};
use std::collections::HashMap;

use crate::declarations::{At, Declarations, PaletteEntry, Texture, VramSlot};
use crate::extract::sha1_hex;

/// Where a variant's colors are stored: a file asset's slot, a resource of it, or its image pages.
struct PaletteSource {
    file: String,
    slot: String,
    resource: Option<usize>,
    in_pages: bool,
}

/// Where every palette's colors are, across the declarations: by image and palette name; and the file assets by
/// release and place.
pub struct Index {
    palettes: HashMap<(String, String), PaletteSource>,
    /// The named runs a slot uploads (colors no image owns: the system's text palettes), by VRAM slot and place.
    runs: HashMap<(String, [u32; 2]), PaletteSource>,
    files: HashMap<(String, u32, Option<u32>, u32), String>,
}

impl Index {
    #[must_use]
    pub fn new(decl: &Declarations) -> Index {
        let (mut palettes, mut runs) = (HashMap::new(), HashMap::new());
        let mut note = |file: &str, slot: &str, resource: Option<usize>, in_pages: bool, entries: &[PaletteEntry]| {
            for e in entries {
                let source = || PaletteSource { file: file.to_string(), slot: slot.to_string(), resource, in_pages };
                match (&e.image, &e.palette, &e.slot, e.at) {
                    (Some(image), Some(palette), ..) => {
                        palettes.entry((image.target().to_string(), palette.clone())).or_insert_with(source);
                    }
                    (None, _, Some(vram), Some(at)) => {
                        runs.entry((vram.target().to_string(), at)).or_insert_with(source);
                    }
                    _ => {}
                }
            }
        };
        let mut files = HashMap::new();
        for (path, asset) in &decl.assets {
            for slot in &asset.slots {
                note(path, &slot.name, None, !slot.pages.is_empty(), &slot.palettes);
                for r in &slot.resources {
                    note(path, &slot.name, Some(r.index), false, &r.palettes);
                }
            }
            if asset.kind == "file" {
                for (release, loc) in &asset.from {
                    for at in &loc.at {
                        if let (Some(stage), Some(file)) = (at.stage, at.file) {
                            files.insert((release.clone(), stage, at.folder, file), path.clone());
                        }
                    }
                }
            }
        }
        Index { palettes, runs, files }
    }
}

/// A placement's VRAM halfword.
fn place_of(decl: &Declarations, slot: &str, at: [u32; 2]) -> Result<(usize, usize)> {
    let vram: VramSlot = decl.assets.get(slot).with_context(|| format!("no VRAM slot {slot}"))?.view()?;
    Ok(((vram.rect[0] + at[0]) as usize, (vram.rect[1] + at[1]) as usize))
}

/// What is needed to read a piece's colors.
struct Reader<'a, 'b> {
    decl: &'a Declarations,
    index: &'a Index,
    release: &'a str,
    read: &'b mut dyn FnMut(&At) -> Result<Vec<u8>>,
}

/// A texture's or an atlas's stored form for one release: its files, each checked against the sha1s its location
/// declares.
///
/// # Errors
/// A chunk cannot be read or decoded, or texels or colors differ from their sha1.
pub fn extract(
    decl: &Declarations,
    index: &Index,
    path: &str,
    release: &str,
    read: &mut dyn FnMut(&At) -> Result<Vec<u8>>,
) -> Result<Vec<(String, Vec<u8>)>> {
    let asset = &decl.assets[path];
    let loc = asset.from.get(release).context("not in this release")?;
    let at = loc.at.first().context("read from nowhere")?;
    let pages = codecs::texture::pages(&read(at)?)?;
    let first = at.page.unwrap_or(0);
    let mut reader = Reader { decl, index, release, read };
    let tex: Texture = asset.view()?;
    let [w, h] = tex.size.map(|n| n as usize);
    let page_width = 64 * texels_per_halfword(tex.bpp);
    let count = w.div_ceil(page_width).max(1);
    let mut columns = Vec::new();
    for page in image_pages(&mut reader, path, at, pages, first, count)? {
        columns.push((codecs::texture::texels(&page, tex.bpp, (0, 0, page_width, h)), page.rows));
    }
    let mut texels = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            texels.push(columns[x / page_width].0[y * page_width + x % page_width]);
        }
    }
    let stored = h.min(columns[0].1);
    let want = loc.sha1.as_ref().context("no texels sha1")?;
    check(&codecs::texture::texel_bytes(&texels[..w * stored], tex.bpp), want, "texels")?;
    let mut out = vec![("native.texels".to_string(), codecs::texture::texel_bytes(&texels, tex.bpp))];
    for palette in &tex.palettes {
        let (Some(slot), Some(place)) = (&palette.slot, palette.at) else { continue }; // an executable's
        // A slot naming the image's palette uploads it; else a named run at its place (a palette the system uploads
        // for every image drawn with it).
        let source = index
            .palettes
            .get(&(path.to_string(), palette.name.clone()))
            .or_else(|| index.runs.get(&(slot.target().to_string(), place)))
            .with_context(|| format!("{}: no file uploads it", palette.name))?;
        let vram = place_of(decl, slot.target(), place)?;
        let colors = variant_colors(decl, source, release, reader.read, vram, tex.entries(&palette.name))
            .with_context(|| format!("{} ({} {})", palette.name, source.file, source.slot))?;
        let bytes: Vec<u8> = colors.iter().flat_map(|c| c.to_le_bytes()).collect();
        let declared = loc.palettes.get(&palette.name).with_context(|| format!("{}: no sha1", palette.name))?;
        check(&bytes, declared, &palette.name)?;
        out.push((format!("native.{}.colors", palette.name), bytes));
    }
    Ok(out)
}

/// A multi-page image's pages in order: from its location's chunk on, and - where that chunk holds not all of them -
/// from the file's later image chunks that hold the rest (an entry's `first` and `count`).
fn image_pages(
    reader: &mut Reader,
    path: &str,
    at: &At,
    pages: Vec<codecs::texture::Page>,
    first: usize,
    count: usize,
) -> Result<Vec<codecs::texture::Page>> {
    let decl = reader.decl;
    let key = (reader.release.to_string(), at.stage.unwrap_or(0), at.folder, at.file.unwrap_or(0));
    let file = reader.index.files.get(&key).and_then(|f| decl.assets.get(f));
    let holding = |slot: &crate::declarations::ChunkSlot| -> Option<(usize, usize)> {
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
            let payload = (reader.read)(&At { slot: Some(slot.name.clone()), page: None, ..at.clone() })?;
            let stored = codecs::texture::pages(&payload)?;
            let offset: usize = slot
                .pages
                .iter()
                .take_while(|e| e.image.as_ref().is_none_or(|r| r.target() != path))
                .map(|e| entry_pages(decl, e))
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
fn entry_pages(decl: &Declarations, entry: &crate::declarations::PageEntry) -> usize {
    let Some(image) = entry.image.as_ref().and_then(|r| decl.assets.get(r.target())) else { return 1 };
    let total = image
        .view::<Texture>()
        .ok()
        .filter(|_| image.role.as_deref() == Some("texture"))
        .map_or(1, |t| (t.size[0] as usize).div_ceil(64 * texels_per_halfword(t.bpp)).max(1));
    let from = entry.first.unwrap_or(0);
    entry.count.unwrap_or(total - from)
}

fn texels_per_halfword(bpp: u8) -> usize {
    match bpp {
        4 => 4,
        8 => 2,
        _ => 1,
    }
}

fn check(bytes: &[u8], want: &str, what: &str) -> Result<()> {
    let got = sha1_hex(bytes);
    if got != want {
        bail!("{what}: sha1 {got}, declared {want}");
    }
    Ok(())
}

/// The `n` colors at VRAM (x, y) that a palette source stores.
fn variant_colors(
    decl: &Declarations,
    source: &PaletteSource,
    release: &str,
    read: &mut dyn FnMut(&At) -> Result<Vec<u8>>,
    (x, y): (usize, usize),
    n: usize,
) -> Result<Vec<u16>> {
    let file = &decl.assets[&source.file];
    let place = file.from.get(release).and_then(|l| l.at.first()).context("the file is in no place")?;
    let payload = read(&At { slot: Some(source.slot.clone()), ..place.clone() })?;
    if source.in_pages {
        for p in codecs::texture::pages(&payload)? {
            let (px, py) = (usize::from(p.x), usize::from(p.y));
            if (px..px + 64).contains(&x) && (py..py + p.rows).contains(&y) {
                let at = (y - py) * 64 + x - px;
                return p.words.get(at..at + n).map(<[u16]>::to_vec).context("past the page");
            }
        }
        bail!("no page holds ({x}, {y})");
    }
    let bytes = match source.resource {
        Some(r) => formats::bundle::resources(&payload).get(&r).map(|(_, b)| b.to_vec()).context("no such resource")?,
        None => payload,
    };
    let block = codecs::texture::palette(&bytes)?;
    let (bx, by) = (usize::from(block.x), usize::from(block.y));
    block
        .rows
        .get(y.wrapping_sub(by))
        .and_then(|row| row.get(x.wrapping_sub(bx)..x.wrapping_sub(bx).wrapping_add(n)))
        .map(<[u16]>::to_vec)
        .with_context(|| format!("({x}, {y}) is outside the stored palettes"))
}
