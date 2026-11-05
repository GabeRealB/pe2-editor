//! A font atlas: one texture page several faces are drawn from, and each face's glyph table (`_FontGlyph`, 12 bytes a
//! code, `textdraw.c`). The page and the tables are made from the faces' fonts, their bindings and the atlas's layout,
//! and read back into them from the disc's page and tables.
//!
//! Who says what: a face's font holds its glyphs - each a drawing, its metrics and the characters it draws (its name is
//! the font's own business); its binding (`FaceSpec`) which code stands for which character, the codes its table covers,
//! what a character the font lacks draws (`missing`, from a code on) and which codes draw nothing; the atlas's layout
//! where each glyph's cell is, by a character it draws, and the records no glyph makes (`kept`), verbatim.
//!
//! A code's record is, in turn: nothing (zeros) where the binding says so; the kept record; its character's glyph's; the
//! missing character's glyph's, from `missing`'s code on; else nothing. A glyph a code draws, with art, is painted where
//! the layout places its cell - by a row's baseline and the cell's x - which it must: what the layout does not place is
//! an error naming it, which `pack` answers with rows of its own. A glyph without art draws the page's blank 1 x 1 cell
//! (the space); a glyph no code draws is not on the page. A face's band
//! of the page's rows, which its V bias reaches, is filled with the background index before the glyphs are painted, the
//! rows of none with the gap index.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use crate::encoding::Encoding;

/// A texel no glyph paints.
pub const TRANSPARENT: u8 = 0xFF;
/// A `_FontGlyph` record's size.
pub const RECORD: usize = 12;

/// How a face's table writes a glyph's record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Format {
    /// `_FontGlyph` (`textdraw.c`), 12 bytes: the cell (its size less one), its offsets from the pen, the advance past
    /// it, the advance in y, the kerning classes and two bytes no code reads.
    #[default]
    FontGlyph,
    /// `TextGlyphCell` (`text.h`), 4 bytes: the cell alone - drawn from the pen, its top on the baseline - the pen
    /// advancing by its width.
    GlyphCell,
    /// `EndingStfGlyph` (the ending's credits), 4 bytes: the cell - standing on the baseline, the pen advancing by its
    /// width - its height in 7 bits and the eighth its page.
    StfGlyph,
}

impl Format {
    /// A format by its name in a face's binding (`font_glyph`, `glyph_cell`).
    #[must_use]
    pub fn named(name: &str) -> Option<Format> {
        match name {
            "font_glyph" => Some(Format::FontGlyph),
            "glyph_cell" => Some(Format::GlyphCell),
            "stf_glyph" => Some(Format::StfGlyph),
            _ => None,
        }
    }

    /// A record's size.
    #[must_use]
    pub fn size(self) -> usize {
        match self {
            Format::FontGlyph => RECORD,
            Format::GlyphCell | Format::StfGlyph => 4,
        }
    }
}
/// How far past a cell a stroke it cuts off may reach.
const STROKE_REACH: usize = 2;
/// Texels between glyphs the packer places, and between its rows.
const GAP: usize = 1;

/// A glyph's art: 4-bit values, `TRANSPARENT` where it paints nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Art {
    pub width: usize,
    pub height: usize,
    pub texels: Vec<u8>,
}

/// A glyph of a font, as the game draws it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Glyph {
    pub name: String,
    /// The characters it draws.
    pub chars: Vec<char>,
    /// Its drawn cell's size: 1 x 1 for a glyph without art.
    pub width: u8,
    pub height: u8,
    pub x_offset: i8,
    pub y_offset: i8,
    pub advance_extra: i8,
    pub left: u8,
    pub right: u8,
    /// Its art, and where its drawn cell is in it.
    pub art: Option<Art>,
    pub cell_at: (usize, usize),
}

/// A face on an atlas: its binding - the codes its table covers and what they draw - and what the engine's code fixes of
/// it on the page.
#[derive(Clone, Debug)]
pub struct FaceSpec {
    pub name: String,
    pub first: u32,
    pub count: usize,
    /// Added to a record's V, modulo 256: the row it is on.
    pub bias: u8,
    /// Its rows, first and last.
    pub band: (usize, usize),
    /// Its codes' characters: its encoding's, with its own over them.
    pub encoding: Encoding,
    /// What a character the font lacks draws, and from which code on: below it, nothing.
    pub missing: Option<(char, u32)>,
    /// Codes drawing nothing, whatever their characters.
    pub nothing: Vec<u32>,
    /// The characters a generated atlas holds of it - its text's - its other codes drawing nothing; none, every one its
    /// codes draw.
    pub only: Option<BTreeSet<char>>,
    /// How its table writes a record.
    pub format: Format,
}

/// The page: its size in texels, the indices of a band's background and of the rows of none, and the cell a glyph
/// without art draws (as a table has it) - without one, such a glyph has a cell of its own where it is placed, which
/// it paints nothing in (the credits' spaces).
#[derive(Clone, Debug)]
pub struct PageSpec {
    pub width: usize,
    pub height: usize,
    pub background: u8,
    pub gap: u8,
    pub blank: Option<(u8, u8)>,
    /// What lies under the glyphs where no glyph paints: a value a texel, `TRANSPARENT` where nothing does - art no glyph
    /// makes, kept as it is (the Japanese release's caption glyphs under the American caption face's rows).
    pub under: Option<Vec<u8>>,
    /// How many pages it is, one below another: a cell's page its row over the page's height, a record saying it where
    /// its format has the room.
    pub pages: usize,
}

impl PageSpec {
    /// The rows of its pages, one below another.
    #[must_use]
    pub fn rows(&self) -> usize {
        self.height * self.pages.max(1)
    }
}

/// A row of a face's cells: its baseline and each glyph's cell's x, by a character it draws.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub baseline: usize,
    pub cells: Vec<(char, usize)>,
}

/// A face's place on the atlas: its rows, and the records no glyph makes, by code.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FaceLayout {
    pub rows: Vec<Row>,
    pub kept: BTreeMap<u32, Record>,
}

/// Where each face's glyphs are, by face.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Layout {
    pub faces: BTreeMap<String, FaceLayout>,
}

/// A record, unpacked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Record {
    pub u: u8,
    pub v: u8,
    pub width: u8,
    pub height: u8,
    pub x_offset: i8,
    pub y_offset: i8,
    pub advance_extra: i8,
    pub advance_y: i8,
    pub left: u8,
    pub right: u8,
    pub unread: [u8; 2],
    /// The page its cell is on.
    pub page: u8,
}

impl Record {
    /// A record as a format writes it: a glyph cell's has no offsets but its top on the baseline, and advances by its
    /// width.
    #[must_use]
    pub fn read(format: Format, b: &[u8]) -> Record {
        if format == Format::GlyphCell {
            return Record {
                u: b[0],
                v: b[1],
                width: b[2],
                height: b[3],
                x_offset: 0,
                y_offset: i8::try_from(b[3].saturating_sub(1)).unwrap_or(i8::MAX),
                advance_extra: 1,
                advance_y: 0,
                left: 0,
                right: 0,
                unread: [0, 0],
                page: 0,
            };
        }
        if format == Format::StfGlyph {
            return Record {
                u: b[0],
                v: b[1],
                width: b[2],
                height: b[3] & 0x7F,
                x_offset: 0,
                y_offset: 0,
                advance_extra: 1,
                advance_y: 0,
                left: 0,
                right: 0,
                unread: [0, 0],
                page: b[3] >> 7,
            };
        }
        let signed = |x: u8| i8::from_le_bytes([x]);
        Record {
            u: b[0],
            v: b[1],
            width: b[2].wrapping_add(1),
            height: b[3].wrapping_add(1),
            x_offset: signed(b[4]),
            y_offset: signed(b[5]),
            advance_extra: signed(b[6]),
            advance_y: signed(b[7]),
            left: b[8],
            right: b[9],
            unread: [b[10], b[11]],
            page: 0,
        }
    }

    /// The record as a format writes it.
    ///
    /// # Errors
    /// The format has no place for what it says: a glyph cell drawn other than from the pen, its top on the baseline,
    /// or an advance past 255.
    pub fn bytes(&self, format: Format) -> Result<Vec<u8>, String> {
        let advance = i16::from(self.x_offset) + i16::from(self.width) - 1 + i16::from(self.advance_extra);
        if format == Format::StfGlyph {
            if self.x_offset != 0 || self.y_offset != 0 {
                return Err(format!(
                    "a credits cell is drawn from the pen, standing on the baseline: this one {} across and {} down",
                    self.x_offset, self.y_offset
                ));
            }
            let advance =
                u8::try_from(advance).map_err(|_| format!("its advance {advance} does not fit a credits cell"))?;
            if self.height > 0x7F || self.page > 1 {
                return Err(format!(
                    "a credits cell is at most 127 rows on one of two pages: this one {} on page {}",
                    self.height, self.page
                ));
            }
            return Ok(vec![self.u, self.v, advance, self.height | (self.page << 7)]);
        }
        if self.page != 0 {
            return Err(format!("its table has one page, and its cell is on page {}", self.page));
        }
        if format == Format::GlyphCell {
            if self.x_offset != 0 || i16::from(self.y_offset) != i16::from(self.height) - 1 {
                return Err(format!(
                    "a glyph cell is drawn from the pen, its top on the baseline: this one {} across and {} down",
                    self.x_offset,
                    i16::from(self.y_offset) - i16::from(self.height) + 1
                ));
            }
            let advance =
                u8::try_from(advance).map_err(|_| format!("its advance {advance} does not fit a glyph cell"))?;
            return Ok(vec![self.u, self.v, advance, self.height]);
        }
        Ok(self.font_glyph().to_vec())
    }

    fn font_glyph(&self) -> [u8; RECORD] {
        let unsigned = |x: i8| x.to_le_bytes()[0];
        [
            self.u,
            self.v,
            self.width.wrapping_sub(1),
            self.height.wrapping_sub(1),
            unsigned(self.x_offset),
            unsigned(self.y_offset),
            unsigned(self.advance_extra),
            unsigned(self.advance_y),
            self.left,
            self.right,
            self.unread[0],
            self.unread[1],
        ]
    }

    fn of(glyph: &Glyph, u: u8, v: u8, page: u8) -> Record {
        Record {
            page,
            u,
            v,
            width: glyph.width,
            height: glyph.height,
            x_offset: glyph.x_offset,
            y_offset: glyph.y_offset,
            advance_extra: glyph.advance_extra,
            advance_y: 0,
            left: glyph.left,
            right: glyph.right,
            unread: [0, 0],
        }
    }
}

/// A rectangle of the page: x, y, width, height.
type Rect = (usize, usize, usize, usize);

/// The texels of strokes running past cells, by the (face, record) whose art they are.
type Strokes = BTreeMap<(usize, usize), Vec<(usize, usize)>>;

fn overlaps(a: Rect, b: Rect) -> bool {
    a.0 < b.0 + b.2 && b.0 < a.0 + a.2 && a.1 < b.1 + b.3 && b.1 < a.1 + a.3
}

/// The Adobe Glyph List For New Fonts (`data/aglfn.txt`): a glyph name by character.
fn aglfn() -> &'static BTreeMap<u32, &'static str> {
    static NAMES: OnceLock<BTreeMap<u32, &'static str>> = OnceLock::new();
    NAMES.get_or_init(|| {
        include_str!("../data/aglfn.txt")
            .lines()
            .filter(|line| !line.starts_with('#'))
            .filter_map(|line| {
                let mut fields = line.split(';');
                Some((u32::from_str_radix(fields.next()?, 16).ok()?, fields.next()?))
            })
            .collect()
    })
}

/// A glyph's name for the character it draws first, as a font names it where nothing else does: the Adobe Glyph List's
/// name, else `uniXXXX` (`uXXXXX` past the Basic Multilingual Plane).
#[must_use]
pub fn glyph_name(c: char) -> String {
    let code = u32::from(c);
    match aglfn().get(&code) {
        Some(name) => (*name).to_string(),
        None if code <= 0xFFFF => format!("uni{code:04X}"),
        None => format!("u{code:05X}"),
    }
}

/// Whether a glyph has a cell of its own on the page: with art, or without where the page has no blank cell.
fn placed_by_layout(page: &PageSpec, glyph: &Glyph) -> bool {
    glyph.art.is_some() || page.blank.is_none()
}

/// A face's codes' characters' glyphs: which glyph draws a character, by its place in the font.
fn char_map(face: &[Glyph]) -> BTreeMap<char, usize> {
    let mut map = BTreeMap::new();
    for (i, glyph) in face.iter().enumerate() {
        for c in &glyph.chars {
            map.entry(*c).or_insert(i);
        }
    }
    map
}

/// What a code draws: nothing, its kept record, or a glyph by its place in the font.
enum Draws<'a> {
    Nothing,
    Kept(&'a Record),
    Glyph(usize),
}

fn draws<'a>(spec: &FaceSpec, layout: Option<&'a FaceLayout>, map: &BTreeMap<char, usize>, code: u32) -> Draws<'a> {
    if spec.nothing.contains(&code) {
        return Draws::Nothing;
    }
    if let Some(kept) = layout.and_then(|l| l.kept.get(&code)) {
        return Draws::Kept(kept);
    }
    let own = spec.encoding.char_of(code).and_then(|c| map.get(&c));
    let missing = spec.missing.filter(|(_, from)| code >= *from).and_then(|(c, _)| map.get(&c));
    own.or(missing).map_or(Draws::Nothing, |&i| Draws::Glyph(i))
}

/// The faces' fonts' glyphs, the layout and what lies under the glyphs, read from a page (`width` x `height` 4-bit
/// values) and the faces' tables: each record with art its own a glyph, its strokes past its cell with it, each blank one
/// (on the page's blank cell) a glyph without art; each glyph's characters those of the codes drawing it, but a code a
/// character the font lacks draws the missing one for; what that does not make again of the tables, kept, and of the
/// page, under the glyphs.
///
/// # Errors
/// A table is not whole records.
pub fn decode(page: &PageSpec, texels: &[u8], specs: &[FaceSpec], tables: &[&[u8]]) -> Result<Decoded, String> {
    let at = |x: usize, y: usize| texels[y * page.width + x];
    // Each face's distinct records and the codes each stands for.
    let mut faces: Vec<Vec<(Record, Vec<u32>)>> = Vec::new();
    for (spec, table) in specs.iter().zip(tables) {
        let size = spec.format.size();
        if table.len() != spec.count * size {
            return Err(format!("{}: {} bytes, not {} records", spec.name, table.len(), spec.count));
        }
        let mut distinct: Vec<(Record, Vec<u32>)> = Vec::new();
        for (i, bytes) in table.chunks(size).enumerate() {
            if bytes.iter().all(|b| *b == 0) {
                continue;
            }
            let code = spec.first + u32::try_from(i).unwrap_or(u32::MAX);
            let record = Record::read(spec.format, bytes);
            match distinct.iter_mut().find(|(r, _)| *r == record) {
                Some((_, codes)) => codes.push(code),
                None => distinct.push((record, vec![code])),
            }
        }
        faces.push(distinct);
    }
    let own: Vec<Vec<bool>> =
        specs.iter().zip(&faces).map(|(spec, distinct)| own_cells(page, &at, spec, distinct)).collect();
    // The cells with art, every face's, by (face, record).
    let mut cells: Vec<((usize, usize), Rect)> = Vec::new();
    for (f, spec) in specs.iter().enumerate() {
        for (i, (record, _)) in faces[f].iter().enumerate().filter(|(i, _)| own[f][*i]) {
            cells.push(((f, i), cell_of(page, spec, record)));
        }
    }
    let strokes = strokes(page, &at, &cells);
    let mut fonts: Vec<Vec<Glyph>> = Vec::new();
    let mut layout = Layout::default();
    for (f, spec) in specs.iter().enumerate() {
        let missing_record = spec.missing.and_then(|(c, _)| {
            faces[f]
                .iter()
                .find(|(_, codes)| codes.iter().any(|code| spec.encoding.char_of(*code) == Some(c)))
                .map(|(r, _)| *r)
        });
        let mut font = Vec::new();
        let mut claimed: Vec<char> = Vec::new();
        let mut rows: BTreeMap<usize, Vec<(char, usize)>> = BTreeMap::new();
        for (i, (record, codes)) in faces[f].iter().enumerate() {
            // A glyph without art: on the blank cell, or - the page having none - on an empty cell of its own in its band.
            let (x, y, w, h) = cell_of(page, spec, record);
            let in_band = y >= spec.band.0 && y + h <= spec.band.1 + 1 && x + w <= page.width;
            let empty = in_band
                && (y..y + h)
                    .all(|row| (x..x + w).all(|col| at(col, row) == page.background || at(col, row) == page.gap));
            let blank = !own[f][i] && page.blank.map_or(empty, |cell| (record.u, record.v) == cell);
            if !own[f][i] && !blank {
                continue;
            }
            let chars = chars_of(spec, codes, Some(*record) == missing_record, &claimed);
            let Some(&first) = chars.first() else { continue };
            claimed.extend(&chars);
            let mut glyph = Glyph {
                name: glyph_name(first),
                chars,
                width: record.width,
                height: record.height,
                x_offset: record.x_offset,
                y_offset: record.y_offset,
                advance_extra: record.advance_extra,
                left: record.left,
                right: record.right,
                art: None,
                cell_at: (0, 0),
            };
            if blank && page.blank.is_none() {
                rows.entry(baseline_of(y, record.height, record.y_offset)).or_default().push((first, x));
            }
            if own[f][i] {
                let (art, cell_at) = cut(&at, (x, y, w, h), strokes.get(&(f, i)).map_or(&[][..], Vec::as_slice));
                (glyph.art, glyph.cell_at) = (Some(art), cell_at);
                rows.entry(baseline_of(y, record.height, record.y_offset)).or_default().push((first, x));
            }
            font.push(glyph);
        }
        let rows = rows
            .into_iter()
            .map(|(baseline, mut cells)| {
                cells.sort_by_key(|(_, x)| *x);
                Row { baseline, cells }
            })
            .collect();
        layout.faces.insert(spec.name.clone(), FaceLayout { rows, kept: BTreeMap::new() });
        fonts.push(font);
    }
    keep_unmade(page, specs, &fonts, &mut layout, tables)?;
    let under = under_of(page, specs, &fonts, &layout, texels)?;
    Ok((fonts, layout, under))
}

/// What no glyph makes of a page, which lies under them: none where they make all of it.
fn under_of(
    page: &PageSpec,
    specs: &[FaceSpec],
    fonts: &[Vec<Glyph>],
    layout: &Layout,
    texels: &[u8],
) -> Result<Option<Vec<u8>>, String> {
    let (made, _) = generate(page, specs, fonts, layout)?;
    let under: Vec<u8> =
        made.iter().zip(texels).map(|(mine, theirs)| if mine == theirs { TRANSPARENT } else { *theirs }).collect();
    Ok(under.iter().any(|t| *t != TRANSPARENT).then_some(under))
}

/// A glyph's characters: its codes', each once and none an earlier glyph has, but where a code draws it as the missing
/// character's stand-in (`stand_in`: it is the record the missing character's code has).
fn chars_of(spec: &FaceSpec, codes: &[u32], stand_in: bool, claimed: &[char]) -> Vec<char> {
    let mut chars: Vec<char> = Vec::new();
    for code in codes {
        let Some(c) = spec.encoding.char_of(*code) else { continue };
        let standing_in = stand_in && spec.missing.is_some_and(|(missing, from)| *code >= from && c != missing);
        if !standing_in && !chars.contains(&c) && !claimed.contains(&c) {
            chars.push(c);
        }
    }
    chars
}

/// A glyph's art cut from the page: its cell and its strokes past it, and where the cell is in it.
fn cut(at: &dyn Fn(usize, usize) -> u8, (x, y, w, h): Rect, strokes: &[(usize, usize)]) -> (Art, (usize, usize)) {
    let left = strokes.iter().map(|p| p.0).min().unwrap_or(x).min(x);
    let top = strokes.iter().map(|p| p.1).min().unwrap_or(y).min(y);
    let right = strokes.iter().map(|p| p.0 + 1).max().unwrap_or(x + w).max(x + w);
    let bottom = strokes.iter().map(|p| p.1 + 1).max().unwrap_or(y + h).max(y + h);
    let width = right - left;
    let mut texels = vec![TRANSPARENT; width * (bottom - top)];
    let cell = (y..y + h).flat_map(|row| (x..x + w).map(move |col| (col, row)));
    for (col, row) in cell.chain(strokes.iter().copied()) {
        texels[(row - top) * width + col - left] = at(col, row);
    }
    (Art { width, height: bottom - top, texels }, (x - left, y - top))
}

/// What a page and its tables are read into: the faces' fonts, the layout, and what lies under the glyphs.
pub type Decoded = (Vec<Vec<Glyph>>, Layout, Option<Vec<u8>>);

/// Keeps, verbatim, each record of the tables the fonts and their bindings do not make again.
fn keep_unmade(
    page: &PageSpec,
    specs: &[FaceSpec],
    fonts: &[Vec<Glyph>],
    layout: &mut Layout,
    tables: &[&[u8]],
) -> Result<(), String> {
    let (_, made) = generate(page, specs, fonts, layout)?;
    for ((spec, made), table) in specs.iter().zip(&made).zip(tables) {
        let kept = &mut layout.faces.entry(spec.name.clone()).or_default().kept;
        let size = spec.format.size();
        for (i, (mine, theirs)) in made.chunks(size).zip(table.chunks(size)).enumerate() {
            if mine != theirs {
                kept.insert(spec.first + u32::try_from(i).unwrap_or(u32::MAX), Record::read(spec.format, theirs));
            }
        }
    }
    Ok(())
}

/// Which of a face's distinct records hold art of their own: in the face's band, holding more than background, and
/// not lying partly over another such cell. Of cells lying over each other, the one whose edge the most art crosses goes
/// - a glyph's cell is cut to its art, a stale one cuts through another's - until none lies over another.
fn own_cells(
    page: &PageSpec,
    at: &dyn Fn(usize, usize) -> u8,
    spec: &FaceSpec,
    distinct: &[(Record, Vec<u32>)],
) -> Vec<bool> {
    let blank = |value: u8| value == page.background || value == page.gap;
    let rects: Vec<Rect> = distinct.iter().map(|(r, _)| cell_of(page, spec, r)).collect();
    let mut mine: Vec<bool> = rects
        .iter()
        .map(|&(x, y, w, h)| {
            let in_band = y >= spec.band.0 && y + h - 1 <= spec.band.1 && x + w <= page.width;
            in_band && (y..y + h).any(|row| (x..x + w).any(|col| !blank(at(col, row))))
        })
        .collect();
    let crossed: Vec<usize> = rects.iter().map(|&cell| crossings(page, at, cell)).collect();
    loop {
        let over = |i: usize| {
            (0..rects.len())
                .filter(|&j| j != i && mine[j] && rects[j] != rects[i] && overlaps(rects[i], rects[j]))
                .count()
        };
        let worst = (0..rects.len()).filter(|&i| mine[i] && over(i) > 0).max_by_key(|&i| (crossed[i], over(i)));
        match worst {
            Some(i) => mine[i] = false,
            None => break,
        }
    }
    mine
}

/// A record's cell on the page.
fn cell_of(page: &PageSpec, spec: &FaceSpec, r: &Record) -> Rect {
    let row = usize::from(r.page) * page.height + usize::from(r.v.wrapping_add(spec.bias));
    (usize::from(r.u), row, usize::from(r.width), usize::from(r.height))
}

/// How much art crosses a cell's edge: texels just outside it holding art next to art just inside it.
fn crossings(page: &PageSpec, at: &dyn Fn(usize, usize) -> u8, (left, top, width, height): Rect) -> usize {
    let inked = |col: usize, row: usize| {
        col < page.width && row < page.rows() && at(col, row) != page.background && at(col, row) != page.gap
    };
    let (right, bottom) = (left + width, top + height);
    let mut count = 0;
    for row in top..bottom {
        count += usize::from(left > 0 && inked(left - 1, row) && inked(left, row));
        count += usize::from(inked(right, row) && inked(right - 1, row));
    }
    for col in left..right {
        count += usize::from(top > 0 && inked(col, top - 1) && inked(col, top));
        count += usize::from(inked(col, bottom) && inked(col, bottom - 1));
    }
    count
}

/// A cell's baseline: the row its last row is `y_offset` rows above.
fn baseline_of(y: usize, height: u8, y_offset: i8) -> usize {
    let bottom = i64::try_from(y).unwrap_or(0) + i64::from(height) - 1;
    usize::try_from(bottom - i64::from(y_offset)).unwrap_or(0)
}

/// The page row a cell's top is on, from its baseline.
fn top_of(baseline: usize, height: u8, y_offset: i8) -> usize {
    let top = i64::try_from(baseline).unwrap_or(0) + i64::from(y_offset) - i64::from(height) + 1;
    usize::try_from(top).unwrap_or(0)
}

/// The strokes running past the cells: texels outside every cell, neither background nor gap, in 8-connected runs,
/// each the nearest cell's (within `STROKE_REACH`), the one whose own art it touches where several are as near. A run
/// no cell is near is no glyph's: it lies under them.
fn strokes(page: &PageSpec, at: &dyn Fn(usize, usize) -> u8, cells: &[((usize, usize), Rect)]) -> Strokes {
    let blank = |value: u8| value == page.background || value == page.gap;
    let mut covered = vec![false; page.width * page.rows()];
    for &(_, (x, y, w, h)) in cells {
        for row in y..(y + h).min(page.rows()) {
            for col in x..(x + w).min(page.width) {
                covered[row * page.width + col] = true;
            }
        }
    }
    let stray = |col: usize, row: usize| !covered[row * page.width + col] && !blank(at(col, row));
    let mut seen = vec![false; page.width * page.rows()];
    let mut out = Strokes::new();
    for row in 0..page.rows() {
        for col in 0..page.width {
            if seen[row * page.width + col] || !stray(col, row) {
                continue;
            }
            let mut run = Vec::new();
            let mut todo = vec![(col, row)];
            seen[row * page.width + col] = true;
            while let Some((c, r)) = todo.pop() {
                run.push((c, r));
                for (dc, dr) in [(-1i64, -1i64), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)] {
                    let (Ok(nc), Ok(nr)) = (
                        usize::try_from(i64::try_from(c).unwrap_or(0) + dc),
                        usize::try_from(i64::try_from(r).unwrap_or(0) + dr),
                    ) else {
                        continue;
                    };
                    // A run stays on its page.
                    if nc < page.width
                        && nr < page.rows()
                        && nr / page.height == r / page.height
                        && !seen[nr * page.width + nc]
                        && stray(nc, nr)
                    {
                        seen[nr * page.width + nc] = true;
                        todo.push((nc, nr));
                    }
                }
            }
            let distance = |(x, y, w, h): Rect, (c, r): (usize, usize)| {
                if y / page.height != r / page.height {
                    return usize::MAX;
                }
                let dx = if c < x { x - c } else { (c + 1).saturating_sub(x + w) };
                let dy = if r < y { y - r } else { (r + 1).saturating_sub(y + h) };
                dx.max(dy)
            };
            let reach = |cell: Rect| run.iter().map(|p| distance(cell, *p)).min().unwrap_or(usize::MAX);
            let nearest = cells.iter().map(|(_, cell)| reach(*cell)).min().unwrap_or(usize::MAX);
            if nearest > STROKE_REACH {
                continue;
            }
            let candidates: Vec<&((usize, usize), Rect)> =
                cells.iter().filter(|(_, cell)| reach(*cell) == nearest).collect();
            // The one whose own art the stroke touches.
            let touches = |(x, y, w, h): Rect| {
                run.iter().any(|&(c, r)| {
                    (c.saturating_sub(1)..=c + 1).any(|cc| {
                        (r.saturating_sub(1)..=r + 1)
                            .any(|rr| cc >= x && cc < x + w && rr >= y && rr < y + h && !blank(at(cc, rr)))
                    })
                })
            };
            let owner = candidates.iter().find(|(_, cell)| touches(*cell)).unwrap_or(&candidates[0]).0;
            out.entry(owner).or_default().extend(run);
        }
    }
    out
}

/// The page and the faces' tables, made from their fonts' glyphs where the layout places them, and their bindings.
///
/// # Errors
/// A glyph a code draws, with art, is not placed, lies past the page or over another's art, or a layout places a
/// character the font lacks or no code draws.
pub fn generate(
    page: &PageSpec,
    specs: &[FaceSpec],
    fonts: &[Vec<Glyph>],
    layout: &Layout,
) -> Result<(Vec<u8>, Vec<Vec<u8>>), String> {
    make(page, specs, fonts, layout, true)
}

/// The page and the tables, as `generate` makes them; where art lies over other art, the later's, said only `strict`.
fn make(
    page: &PageSpec,
    specs: &[FaceSpec],
    fonts: &[Vec<Glyph>],
    layout: &Layout,
    strict: bool,
) -> Result<(Vec<u8>, Vec<Vec<u8>>), String> {
    let texels = ground(page, specs)?;
    all_placed(page, specs, fonts, layout)?;
    let mut canvas = Canvas { painted: vec![None; texels.len()], texels, strict };
    let mut tables = Vec::new();
    for (spec, face) in specs.iter().zip(fonts) {
        let map = char_map(face);
        let own = layout.faces.get(&spec.name);
        let drawn = reached(spec, face, own, &map);
        let placed = pins(spec, face, &map, own, &drawn)?;
        // Each drawn glyph's record, by its place in the font: where it is painted, or the blank cell.
        let mut records: Vec<Option<Record>> = vec![None; face.len()];
        for &i in &drawn {
            let glyph = &face[i];
            if let (None, Some((u, v))) = (&glyph.art, page.blank) {
                records[i] = Some(Record::of(glyph, u, v, 0));
                continue;
            }
            let (x, baseline) = placed[&i];
            let top = top_of(baseline, glyph.height, glyph.y_offset);
            canvas.paint(page, spec, glyph, (x, top))?;
            let u = u8::try_from(x).map_err(|_| format!("{}: {} past x 255", spec.name, glyph.name))?;
            let v = u8::try_from(top % page.height % 256).unwrap_or(0).wrapping_sub(spec.bias);
            let sheet =
                u8::try_from(top / page.height).map_err(|_| format!("{}: {} past page 255", spec.name, glyph.name))?;
            records[i] = Some(Record::of(glyph, u, v, sheet));
        }
        tables.push(table_of(spec, own, &map, &records)?);
    }
    Ok((canvas.texels, tables))
}

/// The pages before any glyph: the rows of none the gap index, the faces' bands the background, what lies under the
/// glyphs over them.
fn ground(page: &PageSpec, specs: &[FaceSpec]) -> Result<Vec<u8>, String> {
    let mut texels = vec![page.gap; page.width * page.rows()];
    for row in 0..page.rows() {
        if specs.iter().any(|spec| row >= spec.band.0 && row <= spec.band.1) {
            texels[row * page.width..(row + 1) * page.width].fill(page.background);
        }
    }
    if let Some(under) = &page.under {
        if under.len() != texels.len() {
            return Err(format!("what lies under the glyphs is {} texels, the page {}", under.len(), texels.len()));
        }
        for (texel, value) in texels.iter_mut().zip(under) {
            if *value != TRANSPARENT {
                *texel = *value;
            }
        }
    }
    Ok(texels)
}

/// Whether each face's layout places what it must: all it does not, said at once.
fn all_placed(page: &PageSpec, specs: &[FaceSpec], fonts: &[Vec<Glyph>], layout: &Layout) -> Result<(), String> {
    let mut unplaced = Vec::new();
    for (spec, face) in specs.iter().zip(fonts) {
        let (map, own) = (char_map(face), layout.faces.get(&spec.name));
        let drawn = reached(spec, face, own, &map);
        let placed = pins(spec, face, &map, own, &drawn)?;
        let missing: Vec<String> = drawn
            .iter()
            .filter(|i| placed_by_layout(page, &face[**i]) && !placed.contains_key(i))
            .map(|i| format!("{:?}", face[*i].chars[0]))
            .collect();
        if !missing.is_empty() {
            let count = missing.len();
            unplaced.push(format!(
                "{}: its layout does not place {count} of its glyphs: {}",
                spec.name,
                missing.join(" ")
            ));
        }
    }
    if unplaced.is_empty() { Ok(()) } else { Err(unplaced.join("; ")) }
}

/// A face's table: each code's record - nothing, the kept one, or its glyph's - as its format writes it.
fn table_of(
    spec: &FaceSpec,
    own: Option<&FaceLayout>,
    map: &BTreeMap<char, usize>,
    records: &[Option<Record>],
) -> Result<Vec<u8>, String> {
    let size = spec.format.size();
    let mut table = Vec::with_capacity(spec.count * size);
    for i in 0..spec.count {
        let code = spec.first + u32::try_from(i).unwrap_or(u32::MAX);
        let record = match draws(spec, own, map, code) {
            Draws::Nothing => None,
            Draws::Kept(record) => Some(*record),
            Draws::Glyph(g) => records[g],
        };
        match record {
            None => table.extend(std::iter::repeat_n(0, size)),
            Some(record) => {
                table.extend(record.bytes(spec.format).map_err(|e| format!("{}: {code:#X}: {e}", spec.name))?);
            }
        }
    }
    Ok(table)
}

/// The pages being painted, and who painted each texel: two glyphs' art meeting must agree, where it is `strict`.
struct Canvas {
    texels: Vec<u8>,
    painted: Vec<Option<(u8, String)>>,
    strict: bool,
}

impl Canvas {
    /// A glyph's art painted with its cell at (x, top); one without art paints nothing in a cell of its own.
    fn paint(
        &mut self,
        page: &PageSpec,
        spec: &FaceSpec,
        glyph: &Glyph,
        (x, top): (usize, usize),
    ) -> Result<(), String> {
        let (width, height) = (usize::from(glyph.width), usize::from(glyph.height));
        let empty = Art { width, height, texels: vec![TRANSPARENT; width * height] };
        let art = glyph.art.as_ref().unwrap_or(&empty);
        let (Some(left), Some(art_top)) = (x.checked_sub(glyph.cell_at.0), top.checked_sub(glyph.cell_at.1)) else {
            return Err(format!("{}: {} lies past the page", spec.name, glyph.name));
        };
        if left + art.width > page.width || art_top + art.height > page.rows() {
            return Err(format!("{}: {} lies past the page", spec.name, glyph.name));
        }
        if art_top / page.height != (art_top + art.height - 1) / page.height {
            return Err(format!("{}: {} runs from one page onto the next", spec.name, glyph.name));
        }
        for row in 0..art.height {
            for col in 0..art.width {
                let value = art.texels[row * art.width + col];
                if value == TRANSPARENT {
                    continue;
                }
                let (px, py) = (left + col, art_top + row);
                let at = py * page.width + px;
                if let Some((other, by)) = &self.painted[at]
                    && *other != value
                    && self.strict
                {
                    return Err(format!("{}: {}'s art lies over {by}'s at ({px}, {py})", spec.name, glyph.name));
                }
                self.texels[at] = value;
                self.painted[at] = Some((value, format!("{} {}", spec.name, glyph.name)));
            }
        }
        Ok(())
    }
}

/// The glyphs a face's codes draw, by their place in its font.
fn reached(
    spec: &FaceSpec,
    face: &[Glyph],
    layout: Option<&FaceLayout>,
    map: &BTreeMap<char, usize>,
) -> BTreeSet<usize> {
    let held = |g: usize| spec.only.as_ref().is_none_or(|only| face[g].chars.iter().any(|c| only.contains(c)));
    (0..spec.count)
        .filter_map(|i| match draws(spec, layout, map, spec.first + u32::try_from(i).unwrap_or(u32::MAX)) {
            Draws::Glyph(g) if held(g) => Some(g),
            _ => None,
        })
        .collect()
}

/// How a generated atlas lays its faces' glyphs out: the characters of `text`, each face's in the order the text first
/// uses them (without a text, every character a face's codes draw, in code order), left to right in rows - a cell's x the first multiple of `align` at or past the last cell's right edge and
/// `spacing` - each row as tall as `row_height` (the face's tallest cell where it is not set), a cell whose right edge
/// would pass `wrap` starting the next; a cell's top on its row's. The first face starts at `start`, each other on the
/// row below the last one's.
#[derive(Clone, Debug)]
pub struct Flow {
    pub text: Option<String>,
    pub start: (usize, usize),
    pub row_height: Option<usize>,
    pub spacing: usize,
    pub align: usize,
    pub wrap: usize,
}

/// The pins a generated atlas needs to make these tables again: the code whose record the flow makes otherwise, first
/// first, its glyph pinned where `placed` has it, and the flow made again, until none is - the kept records `placed`
/// holds kept.
///
/// # Errors
/// The flow fails, or a record it makes otherwise is no glyph's that `placed` places.
pub fn pins_for(
    page: &PageSpec,
    specs: &[FaceSpec],
    fonts: &[Vec<Glyph>],
    rule: &Flow,
    placed: &Layout,
    tables: &[&[u8]],
) -> Result<Layout, String> {
    let mut pins = Layout::default();
    for (name, face) in &placed.faces {
        pins.faces.insert(name.clone(), FaceLayout { rows: Vec::new(), kept: face.kept.clone() });
    }
    let glyphs: usize = fonts.iter().map(Vec::len).sum();
    for _ in 0..=glyphs {
        // Its tables alone: what the rule places a glyph over is pinned away from.
        let layout = flow(page, specs, fonts, rule, &pins)?;
        let (_, made) = make(page, specs, fonts, &layout, false)?;
        let differs = specs.iter().zip(&made).zip(tables).enumerate().find_map(|(f, ((spec, mine), theirs))| {
            let size = spec.format.size();
            mine.chunks(size)
                .zip(theirs.chunks(size))
                .position(|(a, b)| a != b)
                .map(|i| (f, spec.first + u32::try_from(i).unwrap_or(u32::MAX)))
        });
        let Some((f, code)) = differs else { return Ok(pins) };
        let (spec, face) = (&specs[f], &fonts[f]);
        let map = char_map(face);
        let Draws::Glyph(glyph) = draws(spec, pins.faces.get(&spec.name), &map, code) else {
            return Err(format!("{}: {code:#X} is made otherwise, and is no glyph's", spec.name));
        };
        let at = placed.faces.get(&spec.name).and_then(|l| {
            l.rows.iter().find_map(|row| {
                row.cells.iter().find(|(c, _)| map.get(c) == Some(&glyph)).map(|&(c, x)| (c, x, row.baseline))
            })
        });
        let Some((c, x, baseline)) = at else {
            return Err(format!("{}: {code:#X} is made otherwise, and its glyph is placed nowhere", spec.name));
        };
        let rows = &mut pins.faces.entry(spec.name.clone()).or_default().rows;
        if rows.iter().any(|row| row.cells.iter().any(|(pinned, _)| *pinned == c)) {
            return Err(format!("{}: {c:?} is pinned where it is, and still made otherwise", spec.name));
        }
        match rows.iter_mut().find(|row| row.baseline == baseline) {
            Some(row) => row.cells.push((c, x)),
            None => rows.push(Row { baseline, cells: vec![(c, x)] }),
        }
    }
    Err("its pins do not settle".to_string())
}

/// The characters of a text, each once, in the order it first uses them; what is no character to draw (a line break) is
/// left out.
#[must_use]
pub fn first_use(text: &str) -> Vec<char> {
    let mut seen = BTreeSet::new();
    text.chars().filter(|c| !c.is_control() && seen.insert(*c)).collect()
}

/// A generated atlas's layout: each face's glyphs its text uses, where the flow places them - but a character the
/// layout places (`pins`) where it says, the flow going on from it on its row - and the layout's kept records.
///
/// # Errors
/// A face's font does not draw a character of the text, a pin names one it does not draw, or the text does not fit the
/// page.
pub fn flow(
    page: &PageSpec,
    specs: &[FaceSpec],
    fonts: &[Vec<Glyph>],
    flow: &Flow,
    pins: &Layout,
) -> Result<Layout, String> {
    let align = |x: usize| x.div_ceil(flow.align.max(1)) * flow.align.max(1);
    let mut out = Layout::default();
    let mut top = flow.start.1;
    for (spec, face) in specs.iter().zip(fonts) {
        let map = char_map(face);
        let own = pins.faces.get(&spec.name);
        let mut pinned: BTreeMap<usize, (usize, usize)> = BTreeMap::new();
        for row in own.map_or(&[][..], |l| l.rows.as_slice()) {
            for &(c, x) in &row.cells {
                let glyph = *map
                    .get(&c)
                    .ok_or_else(|| format!("{}: its layout places {c:?}, which its font does not draw", spec.name))?;
                pinned.insert(glyph, (x, row.baseline));
            }
        }
        let coded = || {
            let coded = (0..spec.count)
                .filter_map(|i| spec.encoding.char_of(spec.first + u32::try_from(i).unwrap_or(u32::MAX)));
            first_use(&coded.filter(|c| map.contains_key(c)).collect::<String>())
        };
        let chars: Vec<char> = flow.text.as_deref().map_or_else(coded, first_use);
        let tallest = face.iter().filter(|g| g.art.is_some()).map(|g| usize::from(g.height)).max().unwrap_or(1);
        let height = flow.row_height.unwrap_or(tallest);
        // A row that would run from one page onto the next starts the next.
        let fit = |top: usize| {
            if top / page.height == (top + height - 1) / page.height {
                top
            } else {
                (top / page.height + 1) * page.height
            }
        };
        top = fit(top);
        let mut x = flow.start.0;
        let mut placed: Vec<(char, usize, usize)> = Vec::new();
        let mut done = BTreeSet::new();
        for &c in &chars {
            let glyph = *map
                .get(&c)
                .ok_or_else(|| format!("{}: its font does not draw {c:?}, which its text uses", spec.name))?;
            if !done.insert(glyph) {
                continue;
            }
            let g = &face[glyph];
            let width = usize::from(g.width);
            if let Some(&(at, baseline)) = pinned.get(&glyph) {
                placed.push((c, at, baseline));
                (x, top) = (at + width + flow.spacing, top_of(baseline, g.height, g.y_offset));
                continue;
            }
            // Without art it draws the page's blank cell, and has nothing to place.
            if !placed_by_layout(page, g) {
                continue;
            }
            let mut at = align(x);
            if at + width > flow.wrap {
                (at, top) = (align(flow.start.0), fit(top + height));
            }
            if top + usize::from(g.height) > page.rows() {
                return Err(format!("{}: its text does not fit the page: {c:?} would be at row {top}", spec.name));
            }
            placed.push((c, at, baseline_of(top, g.height, g.y_offset)));
            x = at + width + flow.spacing;
        }
        top += height;
        let mut rows: BTreeMap<usize, Vec<(char, usize)>> = BTreeMap::new();
        for (c, at, baseline) in placed {
            rows.entry(baseline).or_default().push((c, at));
        }
        let rows = rows
            .into_iter()
            .map(|(baseline, mut cells)| {
                cells.sort_by_key(|(_, at)| *at);
                Row { baseline, cells }
            })
            .collect();
        out.faces
            .insert(spec.name.clone(), FaceLayout { rows, kept: own.map_or_else(BTreeMap::new, |l| l.kept.clone()) });
    }
    Ok(out)
}

/// Where a face's layout places its glyphs - each cell's x and its row's baseline - by their place in the font.
fn pins(
    spec: &FaceSpec,
    face: &[Glyph],
    map: &BTreeMap<char, usize>,
    layout: Option<&FaceLayout>,
    drawn: &BTreeSet<usize>,
) -> Result<BTreeMap<usize, (usize, usize)>, String> {
    let mut placed: BTreeMap<usize, (usize, usize)> = BTreeMap::new();
    for row in layout.map_or(&[][..], |l| l.rows.as_slice()) {
        for &(c, x) in &row.cells {
            let glyph = *map
                .get(&c)
                .ok_or_else(|| format!("{}: its layout places {c:?}, which its font does not draw", spec.name))?;
            if !drawn.contains(&glyph) {
                return Err(format!("{}: its layout places {c:?}, which no code of it draws", spec.name));
            }
            if placed.insert(glyph, (x, row.baseline)).is_some_and(|was| was != (x, row.baseline)) {
                return Err(format!("{}: its layout places {} twice", spec.name, face[glyph].name));
            }
        }
    }
    Ok(placed)
}

/// The layout with what it does not place packed, and what it places that no code draws left out: each face's glyphs a
/// code draws, with art, that it does not place, in rows of their own below what it places (from the band's top where it
/// places nothing), tallest first, each glyph after the last with a gap, a row's baseline below its tallest art.
///
/// # Errors
/// What is to be packed does not fit a face's band, or the layout places a character the font lacks or one twice.
pub fn pack(page: &PageSpec, specs: &[FaceSpec], fonts: &[Vec<Glyph>], layout: &Layout) -> Result<Layout, String> {
    let mut out = layout.clone();
    for (spec, face) in specs.iter().zip(fonts) {
        let map = char_map(face);
        let mut entry = layout.faces.get(&spec.name).cloned().unwrap_or_default();
        let drawn = reached(spec, face, Some(&entry), &map);
        for row in &mut entry.rows {
            row.cells.retain(|(c, _)| map.get(c).is_none_or(|g| drawn.contains(g)));
        }
        entry.rows.retain(|row| !row.cells.is_empty());
        let placed = pins(spec, face, &map, Some(&entry), &drawn)?;
        let waiting: Vec<usize> =
            drawn.iter().copied().filter(|i| placed_by_layout(page, &face[*i]) && !placed.contains_key(i)).collect();
        for (baseline, cells) in shelves(page, spec, face, &placed, waiting)? {
            entry.rows.push(Row { baseline, cells: cells.into_iter().map(|(i, x)| (face[i].chars[0], x)).collect() });
        }
        out.faces.insert(spec.name.clone(), entry);
    }
    Ok(out)
}

/// A shelf of glyphs: its baseline, and each glyph's cell's x by its place in the font.
type Shelf = (usize, Vec<(usize, usize)>);

/// Shelves of glyphs, by their place in the font, below what is placed.
fn shelves(
    page: &PageSpec,
    spec: &FaceSpec,
    face: &[Glyph],
    placed: &BTreeMap<usize, (usize, usize)>,
    mut waiting: Vec<usize>,
) -> Result<Vec<Shelf>, String> {
    let signed = |n: usize| i64::try_from(n).unwrap_or(i64::MAX);
    let art = |i: usize| {
        face[i].art.as_ref().map_or((usize::from(face[i].width), usize::from(face[i].height)), |a| (a.width, a.height))
    };
    // Rows of art above a glyph's baseline (its own row among them), and below it.
    let above =
        |i: usize| signed(usize::from(face[i].height)) - i64::from(face[i].y_offset) + signed(face[i].cell_at.1);
    let bottom_of = |i: usize, baseline: i64| baseline + 1 - above(i) + signed(art(i).1);
    // Tallest first, so that each row holds art of about one height; of one height, in the font's order.
    waiting.sort_by_key(|i| std::cmp::Reverse(art(*i).1));
    let mut top = placed
        .iter()
        .map(|(i, &(_, baseline))| bottom_of(*i, signed(baseline)) + signed(GAP))
        .max()
        .unwrap_or_else(|| signed(spec.band.0));
    let mut out = Vec::new();
    let mut rest = waiting.as_slice();
    while !rest.is_empty() {
        // As many as fit across, left to right.
        let (mut width, mut count) = (0, 0);
        for i in rest {
            let next = width + art(*i).0 + if count == 0 { 0 } else { GAP };
            if next > page.width && count > 0 {
                break;
            }
            (width, count) = (next, count + 1);
        }
        let row = &rest[..count];
        let baseline = top + row.iter().map(|i| above(*i)).max().unwrap_or(1) - 1;
        let bottom = row.iter().map(|i| bottom_of(*i, baseline)).max().unwrap_or(top);
        if baseline < signed(spec.band.0) || bottom - 1 > signed(spec.band.1) {
            return Err(format!(
                "{}: what it does not place does not fit its band, rows {} to {}",
                spec.name, spec.band.0, spec.band.1
            ));
        }
        let mut left = 0;
        let mut cells = Vec::new();
        for i in row {
            cells.push((*i, left + face[*i].cell_at.0));
            left += art(*i).0 + GAP;
        }
        out.push((usize::try_from(baseline).unwrap_or(0), cells));
        top = bottom + signed(GAP);
        rest = &rest[count..];
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{Art, FaceSpec, Flow, Format, Glyph, Layout, PageSpec, RECORD, Row, decode, flow, generate, pack};
    use crate::encoding::Encoding;

    /// A record's bytes: cell, offsets, advance.
    fn record(u: u8, v: u8, w: u8, h: u8, xo: i8, yo: i8, adx: i8) -> [u8; RECORD] {
        [u, v, w - 1, h - 1, xo.to_le_bytes()[0], yo.to_le_bytes()[0], adx.to_le_bytes()[0], 0, 0, 0, 0, 0]
    }

    /// A 16 x 12 page: a band of rows 0-7 (background 5), rows 8-11 none (gap 0); `A` a 3 x 3 cell at (2, 1) with a
    /// stroke past its right edge at (5, 2), `B` a 2 x 3 cell at (8, 1). Codes 0x41 and 0x61 draw `A`, 0x42 `B`, 0x20 the
    /// blank cell (the space), 0xC0 and up what is missing (the space), 0xC2 nothing, 0x43 a copy of a cell below the band.
    fn page() -> (PageSpec, Vec<u8>, FaceSpec, Vec<u8>) {
        let spec =
            PageSpec { width: 16, height: 12, background: 5, gap: 0, blank: Some((0, 0)), under: None, pages: 1 };
        let mut texels = vec![5u8; 16 * 12];
        texels[8 * 16..].fill(0);
        for (x, y) in [(2, 1), (3, 1), (4, 1), (2, 2), (4, 2), (2, 3), (3, 3), (4, 3), (5, 2), (8, 1), (9, 2), (8, 3)] {
            texels[y * 16 + x] = 15;
        }
        let face = FaceSpec {
            name: "face".into(),
            first: 0x20,
            count: 0xE0,
            bias: 0,
            band: (0, 7),
            encoding: Encoding::of(&toml::from_str("base = \"windows-1252\"").unwrap()).unwrap(),
            missing: Some((' ', 0xC0)),
            nothing: vec![0xC2],
            only: None,
            format: Format::FontGlyph,
        };
        let mut table = vec![0u8; 0xE0 * RECORD];
        let put = |table: &mut Vec<u8>, code: usize, r: [u8; RECORD]| {
            table[(code - 0x20) * RECORD..][..RECORD].copy_from_slice(&r);
        };
        put(&mut table, 0x20, record(0, 0, 1, 1, 0, 0, 4));
        put(&mut table, 0x41, record(2, 1, 3, 3, 0, 0, 1));
        put(&mut table, 0x61, record(2, 1, 3, 3, 0, 0, 1));
        put(&mut table, 0x42, record(8, 1, 2, 3, 1, 0, 1));
        put(&mut table, 0x43, record(4, 9, 2, 2, 0, 0, 0));
        for code in (0xC0..0x100).filter(|c| *c != 0xC2) {
            put(&mut table, code, record(0, 0, 1, 1, 0, 0, 4));
        }
        (spec, texels, face, table)
    }

    #[test]
    fn a_page_and_its_table_are_made_again_from_what_they_are_read_into() {
        let (spec, texels, face, table) = page();
        let (fonts, layout, _) = decode(&spec, &texels, std::slice::from_ref(&face), &[&table]).unwrap();
        let names: Vec<&str> = fonts[0].iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["space", "A", "B"]);
        // The space draws itself alone, what is missing being its stand-in; `A` draws A and a, its stroke with it.
        assert_eq!(fonts[0][0].chars, [' ']);
        assert_eq!(fonts[0][1].chars, ['A', 'a']);
        assert_eq!(fonts[0][1].art.as_ref().unwrap().width, 4);
        // Placed by a character; the copy below the band, which no glyph makes, kept.
        let face_layout = &layout.faces["face"];
        assert_eq!(face_layout.rows[0].cells, [('A', 2), ('B', 8)]);
        assert_eq!(face_layout.kept.keys().copied().collect::<Vec<_>>(), [0x43]);
        let (made, tables) = generate(&spec, std::slice::from_ref(&face), &fonts, &layout).unwrap();
        assert_eq!(made, texels);
        assert_eq!(tables[0], table);
    }

    #[test]
    fn what_the_layout_does_not_place_is_said_so_and_packed_on_asking() {
        let (spec, texels, face, table) = page();
        let (fonts, mut layout, _) = decode(&spec, &texels, std::slice::from_ref(&face), &[&table]).unwrap();
        layout.faces.get_mut("face").unwrap().rows.clear();
        let why = generate(&spec, std::slice::from_ref(&face), &fonts, &layout).unwrap_err();
        assert!(why.contains("does not place 2 of its glyphs: 'A' 'B'"), "{why}");
        let packed = pack(&spec, std::slice::from_ref(&face), &fonts, &layout).unwrap();
        let (made, tables) = generate(&spec, std::slice::from_ref(&face), &fonts, &packed).unwrap();
        // Packed from the band's top, left to right a texel apart: A's art (4 wide with its stroke) at 0, B at 5.
        assert_eq!(&tables[0][(0x41 - 0x20) * RECORD..][..2], &[0, 0], "A's cell");
        assert_eq!(tables[0][(0x42 - 0x20) * RECORD], 5, "B's cell's x");
        assert_eq!(made[16 + 3], 15, "A's stroke, at its cell's right");
        // What is placed stays; packing it again changes nothing.
        assert_eq!(pack(&spec, std::slice::from_ref(&face), &fonts, &packed).unwrap(), packed);
    }

    #[test]
    fn what_does_not_fit_its_band_is_said_so() {
        let (spec, texels, mut face, table) = page();
        let (fonts, mut layout, _) = decode(&spec, &texels, std::slice::from_ref(&face), &[&table]).unwrap();
        layout.faces.get_mut("face").unwrap().rows.clear();
        face.band = (0, 1);
        assert!(pack(&spec, &[face], &fonts, &layout).unwrap_err().contains("does not fit its band"));
    }

    #[test]
    fn a_glyph_no_code_draws_is_not_placed_and_its_pin_goes() {
        let (spec, texels, mut face, table) = page();
        let (fonts, layout, _) = decode(&spec, &texels, std::slice::from_ref(&face), &[&table]).unwrap();
        // B's code now draws nothing: its pin is an error, which packing takes out.
        face.nothing.push(0x42);
        assert!(
            generate(&spec, std::slice::from_ref(&face), &fonts, &layout)
                .unwrap_err()
                .contains("which no code of it draws")
        );
        let packed = pack(&spec, std::slice::from_ref(&face), &fonts, &layout).unwrap();
        assert_eq!(packed.faces["face"].rows[0].cells, [('A', 2)]);
        assert!(generate(&spec, &[face], &fonts, &packed).is_ok());
    }

    /// A font of twelve glyphs, A to L, of these widths and 10 rows each, cut to their ink, coded from 0, on a 64 x 64 page.
    fn letters() -> (PageSpec, FaceSpec, Vec<Glyph>) {
        const WIDTHS: [u8; 12] = [10, 9, 8, 9, 8, 8, 9, 9, 6, 8, 9, 8];
        let text = "ABCDEFGHIJKL";
        let page =
            PageSpec { width: 64, height: 64, background: 0, gap: 0, blank: Some((0, 0)), under: None, pages: 1 };
        let encoding = Encoding::of(&toml::from_str(&format!("sequence = {text:?}")).unwrap()).unwrap();
        let spec = FaceSpec {
            name: "letters".into(),
            first: 0,
            count: 12,
            bias: 0,
            band: (0, 63),
            encoding,
            missing: None,
            nothing: vec![],
            only: None,
            format: Format::FontGlyph,
        };
        let glyphs = text
            .chars()
            .zip(WIDTHS)
            .map(|(c, w)| Glyph {
                name: c.to_string(),
                chars: vec![c],
                width: w,
                height: 10,
                x_offset: 0,
                y_offset: 0,
                advance_extra: 0,
                left: 0,
                right: 0,
                art: Some(Art { width: usize::from(w), height: 10, texels: vec![15; usize::from(w) * 10] }),
                cell_at: (0, 0),
            })
            .collect();
        (page, spec, glyphs)
    }

    /// Each code's cell, as the tables made of a flow say.
    fn cells_of(page: &PageSpec, spec: &FaceSpec, glyphs: &[Glyph], rule: &Flow, pins: &Layout) -> Vec<(u8, u8)> {
        let layout = flow(page, std::slice::from_ref(spec), &[glyphs.to_vec()], rule, pins).unwrap();
        let (_, tables) = generate(page, std::slice::from_ref(spec), &[glyphs.to_vec()], &layout).unwrap();
        tables[0].chunks(RECORD).map(|r| (r[0], r[1])).collect()
    }

    #[test]
    fn a_flow_puts_each_cell_on_the_next_column_past_the_last_and_wraps_what_would_pass_the_edge() {
        let (page, spec, glyphs) = letters();
        let rule = Flow {
            text: Some("ABCDEFGHIJKL".into()),
            start: (0, 0),
            row_height: Some(16),
            spacing: 0,
            align: 8,
            wrap: 64,
        };
        let free = [
            (0, 0),
            (16, 0),
            (32, 0),
            (40, 0),
            (56, 0),
            (0, 16),
            (8, 16),
            (24, 16),
            (40, 16),
            (48, 16),
            (0, 32),
            (16, 32),
        ];
        assert_eq!(cells_of(&page, &spec, &glyphs, &rule, &Layout::default()), free);
        // C pinned a texel past its column: the flow goes on from it.
        let mut pins = Layout::default();
        pins.faces.entry("letters".into()).or_default().rows.push(Row { baseline: 9, cells: vec![('C', 33)] });
        let pinned = [
            (0, 0),
            (16, 0),
            (33, 0),
            (48, 0),
            (0, 16),
            (8, 16),
            (16, 16),
            (32, 16),
            (48, 16),
            (56, 16),
            (0, 32),
            (16, 32),
        ];
        assert_eq!(cells_of(&page, &spec, &glyphs, &rule, &pins), pinned);
    }

    #[test]
    fn a_generated_atlas_holds_its_text_alone() {
        let (page, mut spec, glyphs) = letters();
        let rule = Flow {
            text: Some("Bad\nDEAD, CAB".into()),
            start: (1, 2),
            row_height: None,
            spacing: 1,
            align: 1,
            wrap: 64,
        };
        spec.only = Some(super::first_use(rule.text.as_deref().unwrap_or_default()).into_iter().collect());
        let (_, tables) = {
            let layout =
                flow(&page, std::slice::from_ref(&spec), std::slice::from_ref(&glyphs), &rule, &Layout::default());
            // Its font has no lowercase, nor a comma or a space: said, by the first it lacks.
            assert!(layout.unwrap_err().contains("does not draw 'a'"));
            let rule = Flow { text: Some("BAD\nDEADCAB".into()), ..rule.clone() };
            let layout =
                flow(&page, std::slice::from_ref(&spec), std::slice::from_ref(&glyphs), &rule, &Layout::default())
                    .unwrap();
            generate(&page, std::slice::from_ref(&spec), std::slice::from_ref(&glyphs), &layout).unwrap()
        };
        // B, A and D in the order the text first uses them, a texel apart from the start; then E and C; the rest nothing.
        let cell = |code: usize| (tables[0][code * RECORD], tables[0][code * RECORD + 1]);
        assert_eq!([cell(1), cell(0), cell(3), cell(4), cell(2)], [(1, 2), (11, 2), (22, 2), (32, 2), (41, 2)]);
        assert_eq!(&tables[0][5 * RECORD..6 * RECORD], &[0; RECORD], "F");
    }

    #[test]
    fn a_flow_runs_onto_the_next_page_and_a_credits_cell_says_which() {
        let (mut page, mut spec, mut glyphs) = letters();
        // Two pages of 64 x 24: a 16-row row from 16 would run onto the next, which it starts.
        (page.height, page.pages, page.blank) = (24, 2, None);
        spec.format = Format::StfGlyph;
        for g in &mut glyphs {
            (g.y_offset, g.advance_extra) = (0, 1);
        }
        // A space without art, a cell of its own in the flow: B, then 6 texels of nothing, then C.
        glyphs[2].art = None;
        let rule = Flow { text: None, start: (0, 0), row_height: Some(16), spacing: 0, align: 1, wrap: 64 };
        let layout =
            flow(&page, std::slice::from_ref(&spec), std::slice::from_ref(&glyphs), &rule, &Layout::default()).unwrap();
        let (texels, tables) =
            generate(&page, std::slice::from_ref(&spec), std::slice::from_ref(&glyphs), &layout).unwrap();
        assert_eq!(texels.len(), 64 * 48);
        let cell = |code: usize| <[u8; 4]>::try_from(&tables[0][code * 4..code * 4 + 4]).unwrap();
        // A to G on the first row, C painting nothing; H past 64 starts the next, which would run onto page 2.
        assert_eq!(
            [cell(0), cell(1), cell(2), cell(6)],
            [[0, 0, 10, 10], [10, 0, 9, 10], [19, 0, 8, 10], [52, 0, 9, 10]]
        );
        assert_eq!(cell(7), [0, 0, 9, 0x0A | 0x80], "H on page 2");
        assert!(texels[19..27].iter().all(|t| *t == 0), "C paints nothing");
        // Read back, the cells' pages are theirs.
        let (fonts, placed, _) = decode(&page, &texels, std::slice::from_ref(&spec), &[&tables[0]]).unwrap();
        let (_, again) = generate(&page, std::slice::from_ref(&spec), &fonts, &placed).unwrap();
        assert_eq!(again[0], tables[0]);
    }

    #[test]
    fn renaming_a_glyph_moves_nothing() {
        let (spec, texels, face, table) = page();
        let (mut fonts, layout, _) = decode(&spec, &texels, std::slice::from_ref(&face), &[&table]).unwrap();
        fonts[0][1].name = "letter_a".into();
        let (made, tables) = generate(&spec, std::slice::from_ref(&face), &fonts, &layout).unwrap();
        assert_eq!((made, &tables[0]), (texels, &table));
    }
}
