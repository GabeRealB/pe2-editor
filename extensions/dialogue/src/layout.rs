//! A dialogue's cells and atlas from its text (`doc/CAP_FORMAT.md` §9): glyph cells are numbered from 25 - from
//! `first_glyph` for the other layout tool - in the order the text first uses each character, a speaker plate's cell is
//! its speaker's number, and the atlas places every item in that first-use order, a plate after the glyphs of the first
//! line shown with it: left to right, a new 14-texel row where one would pass x = 256.
//!
//! What the rule does not imply is the dialogue's `atlas` table: `placed`, cells the atlas holds elsewhere
//! (`{ "37" = [x, y] }`); `cast`, the cells of speakers no record shows (`[x, y, w, h]` as stored); `drawn`, those of
//! them whose plate is on the atlas, by plate; `marks`, glyphs of the face no cell reaches, by position; `cells`, a table
//! longer than its cells need; `stored_cells`, the cells of a page the dialogue does not lay out, as stored; `face` and
//! `first_glyph`, the other tool's.

use std::collections::BTreeMap;

use pe2_ext::{Table, Value};

use crate::markup::{self, Item};

pub const FIRST_GLYPH_CELL: u16 = 25;
pub const ATLAS_SIZE: usize = 256;
pub const ROW_HEIGHT: usize = 14;

/// A glyph's picture: its 4-bit values, row by row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picture {
    pub width: usize,
    pub height: usize,
    pub values: Vec<u8>,
}

/// The dialogue faces' pictures, by face and token: a character, or a plate's name.
#[derive(Clone, Debug, Default)]
pub struct Font {
    pub faces: BTreeMap<String, BTreeMap<String, Picture>>,
}

impl Font {
    /// A picture of a face.
    ///
    /// # Errors
    /// The face has no glyph of that token.
    pub fn picture(&self, face: &str, token: &str) -> Result<&Picture, String> {
        self.faces.get(face).and_then(|f| f.get(token)).ok_or_else(|| format!("the {face} face has no {token:?}"))
    }
}

/// A dialogue laid out: its cell table, each text's codes, and its atlas page (`ATLAS_SIZE` square, a value a texel).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Laid {
    pub cells: Vec<[u8; 4]>,
    pub texts: Vec<Vec<u16>>,
    pub atlas: Vec<u8>,
}

/// What a text is shown with: the speaker number of the first record showing it, where that record names one.
pub type Titles = Vec<Option<u8>>;

fn number_key(key: &str) -> Result<u16, String> {
    key.parse().map_err(|_| format!("atlas: {key:?} is no cell number"))
}

fn pair(value: &Value) -> Option<(usize, usize)> {
    let items = value.as_array()?;
    Some((usize::try_from(items.first()?.as_integer()?).ok()?, usize::try_from(items.get(1)?.as_integer()?).ok()?))
}

fn rect(value: &Value) -> Result<[u8; 4], String> {
    let items = value.as_array().ok_or("a cell is no list")?;
    let mut out = [0u8; 4];
    for (n, slot) in out.iter_mut().enumerate() {
        let v = items.get(n).and_then(Value::as_integer).ok_or("a cell is four numbers")?;
        *slot = u8::try_from(v).map_err(|_| format!("a cell's {v} is out of range"))?;
    }
    Ok(out)
}

fn by_number<'a>(atlas: &'a Table, key: &str) -> Result<Vec<(u16, &'a Value)>, String> {
    let table = atlas.get(key).and_then(Value::as_table);
    table.into_iter().flatten().map(|(k, v)| Ok((number_key(k)?, v))).collect()
}

fn to_u8(value: usize) -> u8 {
    u8::try_from(value).unwrap_or(u8::MAX)
}

/// Lay a dialogue out: number its cells from its texts, place them, draw its atlas.
///
/// # Errors
/// A text's markup is not valid, a face lacks a glyph it uses, or the atlas table is not as described.
pub fn lay_out(
    texts: &[String],
    titles: &Titles,
    speakers: &BTreeMap<u8, String>,
    plate_of: &dyn Fn(u8) -> Option<String>,
    atlas: &Table,
    font: &Font,
) -> Result<Laid, String> {
    let face = atlas.get("face").and_then(Value::as_str).unwrap_or("glyphs");
    let first_glyph = match atlas.get("first_glyph").and_then(Value::as_integer) {
        Some(n) => u16::try_from(n).map_err(|_| "first_glyph is out of range")?,
        None => FIRST_GLYPH_CELL,
    };
    let titles_are_cells = atlas.contains_key("first_glyph");
    // Each cell's item, in the order the text first uses them.
    let mut cells: BTreeMap<u16, (String, String)> = BTreeMap::new();
    let mut glyph_cell: BTreeMap<String, u16> = BTreeMap::new();
    let mut order: Vec<(String, String)> = Vec::new();
    let mut codes_of = Vec::with_capacity(texts.len());
    for (n, text) in texts.iter().enumerate() {
        let mut codes = Vec::new();
        for item in markup::parse(text)? {
            let cell = match &item {
                Item::Glyph { token, .. } => {
                    if let Some(&cell) = glyph_cell.get(token) {
                        cell
                    } else {
                        let cell = first_glyph + u16::try_from(glyph_cell.len()).map_err(|_| "too many glyphs")?;
                        glyph_cell.insert(token.clone(), cell);
                        cells.insert(cell, (face.to_string(), token.clone()));
                        order.push((face.to_string(), token.clone()));
                        cell
                    }
                }
                Item::Cell { cell, .. } => *cell,
                Item::Code(_) => 0,
            };
            codes.push(item.code(cell));
        }
        codes_of.push(codes);
        let number = if titles_are_cells { None } else { titles.get(n).copied().flatten() };
        if let Some(number) = number {
            let speaker = speakers.get(&number).cloned().or_else(|| plate_of(number));
            if let Some(speaker) = speaker
                && !cells.contains_key(&u16::from(number))
            {
                cells.insert(u16::from(number), ("speakers".into(), speaker.clone()));
                order.push(("speakers".into(), speaker));
            }
        }
    }
    // Where the rule puts each item, the last placement of one placed twice.
    let mut ruled: BTreeMap<(String, String), (usize, usize)> = BTreeMap::new();
    let (mut x, mut y) = (0usize, 0usize);
    for item in &order {
        let picture = font.picture(&item.0, &item.1)?;
        if x + picture.width > ATLAS_SIZE {
            (x, y) = (0, y + ROW_HEIGHT);
        }
        ruled.insert(item.clone(), (x, y));
        x += picture.width;
    }
    let mut moved: BTreeMap<u16, (usize, usize)> = BTreeMap::new();
    for (number, at) in by_number(atlas, "placed")? {
        moved.insert(number, pair(at).ok_or("placed: a cell's place is two numbers")?);
    }
    let at: BTreeMap<u16, (usize, usize)> =
        cells.iter().map(|(number, item)| (*number, moved.get(number).copied().unwrap_or(ruled[item]))).collect();
    let cast: Vec<(u16, [u8; 4])> =
        by_number(atlas, "cast")?.into_iter().map(|(n, v)| Ok((n, rect(v)?))).collect::<Result<_, String>>()?;
    let highest = cells.keys().chain(cast.iter().map(|(n, _)| n)).max().map_or(0, |n| usize::from(*n) + 1);
    let length = highest.max(atlas.get("cells").and_then(Value::as_integer).and_then(|n| usize::try_from(n).ok()).unwrap_or(0));
    let mut table: Vec<[u8; 4]> = vec![[0; 4]; length];
    if let Some(stored) = atlas.get("stored_cells").and_then(Value::as_array) {
        table = stored.iter().map(rect).collect::<Result<_, _>>()?;
    }
    for (number, (kind, token)) in &cells {
        let picture = font.picture(kind, token)?;
        let (x, y) = at[number];
        let slot = table.get_mut(usize::from(*number)).ok_or("a cell past the table")?;
        *slot = [to_u8(x), to_u8(y), to_u8(picture.width), to_u8(picture.height)];
    }
    for (number, cell) in &cast {
        *table.get_mut(usize::from(*number)).ok_or("a cast cell past the table")? = *cell;
    }
    let page = draw(&cells, &order, &at, &cast, atlas, face, font)?;
    Ok(Laid { cells: table, texts: codes_of, atlas: page })
}

/// The atlas: each item where its first cell is, then a plate's other cells, the cast's drawn plates, the marks.
fn draw(
    cells: &BTreeMap<u16, (String, String)>,
    order: &[(String, String)],
    at: &BTreeMap<u16, (usize, usize)>,
    cast: &[(u16, [u8; 4])],
    atlas: &Table,
    face: &str,
    font: &Font,
) -> Result<Vec<u8>, String> {
    let mut first: BTreeMap<&(String, String), u16> = BTreeMap::new();
    let mut by_order: Vec<(&u16, &(String, String))> = cells.iter().collect();
    by_order.sort_by_key(|(_, item)| order.iter().position(|o| o == *item));
    for (number, item) in by_order {
        first.entry(item).or_insert(*number);
    }
    let mut items: Vec<(&str, &str, (usize, usize))> =
        order.iter().map(|item| (item.0.as_str(), item.1.as_str(), at[&first[item]])).collect();
    for (number, item) in cells {
        if first[item] != *number {
            items.push((item.0.as_str(), item.1.as_str(), at[number]));
        }
    }
    let drawn = by_number(atlas, "drawn")?;
    for (number, token) in &drawn {
        let cell = cast.iter().find(|(n, _)| n == number).map(|(_, c)| *c).ok_or("drawn: a plate of no cast cell")?;
        let token = token.as_str().ok_or("drawn: a plate is no name")?;
        items.push(("speakers", token, (usize::from(cell[0]), usize::from(cell[1]))));
    }
    let marks = atlas.get("marks").and_then(Value::as_table);
    for (token, place) in marks.into_iter().flatten() {
        items.push((face, token.as_str(), pair(place).ok_or("marks: a place is two numbers")?));
    }
    let mut page = vec![0u8; ATLAS_SIZE * ATLAS_SIZE];
    for (kind, token, (x, y)) in items {
        paste(&mut page, font.picture(kind, token)?, x, y);
    }
    Ok(page)
}

/// A picture pasted on the page, every value of it, clipped at the page's edges.
fn paste(page: &mut [u8], picture: &Picture, x: usize, y: usize) {
    for row in 0..picture.height {
        for column in 0..picture.width {
            let (px, py) = (x + column, y + row);
            if px < ATLAS_SIZE && py < ATLAS_SIZE {
                page[py * ATLAS_SIZE + px] = picture.values[row * picture.width + column];
            }
        }
    }
}

/// A cell's values on a page.
#[must_use]
pub fn crop(page: &[u8], width: usize, cell: [u8; 4]) -> Picture {
    let [u, v, w, h] = cell.map(usize::from);
    let rows = page.len() / width.max(1);
    let values = (v..v + h)
        .flat_map(|y| (u..u + w).map(move |x| (x, y)))
        .map(|(x, y)| if x < width && y < rows { page[y * width + x] } else { 0 })
        .collect();
    Picture { width: w, height: h, values }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{Font, Picture, lay_out};

    fn glyph(width: usize, value: u8) -> Picture {
        Picture { width, height: 12, values: vec![value; width * 12] }
    }

    #[test]
    fn cells_follow_first_use_and_plates_follow_their_line() {
        let mut font = Font::default();
        let glyphs = font.faces.entry("glyphs".into()).or_default();
        glyphs.insert("H".into(), glyph(8, 3));
        glyphs.insert("i".into(), glyph(250, 4));
        font.faces.entry("speakers".into()).or_default().insert("Aya".into(), glyph(20, 9));
        let texts = vec!["Hi".to_string(), "iH".to_string()];
        let speakers: BTreeMap<u8, String> = [(0, "Aya".to_string())].into();
        let laid = lay_out(&texts, &vec![Some(0), None], &speakers, &|_| None, &pe2_ext::Table::new(), &font).expect("laid");
        assert_eq!(laid.texts, vec![vec![25, 26], vec![26, 25]]);
        assert_eq!(laid.cells[25], [0, 0, 8, 12]);
        assert_eq!(laid.cells[26], [0, 14, 250, 12], "a glyph past the edge starts a row");
        assert_eq!(laid.cells[0], [0, 28, 20, 12], "the plate after its line's glyphs, its number its cell");
        assert_eq!(laid.atlas[28 * 256], 9);
    }
}
