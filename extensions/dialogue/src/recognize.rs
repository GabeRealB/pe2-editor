//! Which character each glyph cell of a dialogue draws, read off its atlas: a cell's picture is known by a hash of its
//! 4-bit values (`sha1([w, h] + values)`, its first 16 hex digits; an all-clear cell is the space), which the glyph
//! table names. A picture several characters share (capital I and lowercase l) is one cell each: the dialogue's
//! `glyphs` choices settle them, else a guess from how the cell is used, which a problem reports. What no table names
//! is kept under a placeholder and reported, never stopping an exact rebuild.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use sha1::{Digest, Sha1};

use crate::layout::{Picture, crop};
use crate::markup::{CELL_MASK, draws};

/// A picture's hash; `space` for an all-clear one.
#[must_use]
pub fn hash(picture: &Picture) -> String {
    if picture.values.iter().all(|v| *v == 0) {
        return "space".into();
    }
    let mut sha = Sha1::new();
    sha.update([u8::try_from(picture.width).unwrap_or(0), u8::try_from(picture.height).unwrap_or(0)]);
    sha.update(&picture.values);
    sha.finalize().iter().take(8).fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// What recognition could not settle on its own, a cell's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Note {
    /// A picture no table names.
    Unseen { cell: u16, hash: String },
    /// A picture several characters share, guessed.
    Ambiguous { cell: u16, candidates: Vec<String>, guessed: String },
    /// Two cells that came out as one character.
    Duplicate { cell: u16, char: String, also: u16 },
}

/// A dialogue's glyph cells recognized: each cell's character where it is known, and what was not settled.
pub struct Recognized {
    pub chars: BTreeMap<u16, String>,
    pub notes: Vec<Note>,
}

struct Payload<'a> {
    streams: &'a [Vec<u16>],
    hashes: BTreeMap<u16, String>,
    chars: BTreeMap<u16, Option<String>>,
}

impl Payload<'_> {
    fn uses(&self, cell: u16) -> impl Iterator<Item = (&[u16], usize)> {
        self.streams.iter().flat_map(move |codes| {
            codes.iter().enumerate().filter(move |(_, c)| draws(**c) && **c & CELL_MASK == cell).map(move |(n, _)| (codes.as_slice(), n))
        })
    }

    fn is_space(&self, code: u16) -> bool {
        !draws(code) || self.hashes.get(&(code & CELL_MASK)).is_none_or(|h| h == "space")
    }

    /// The share of a cell's uses that begin a word.
    fn starts_words(&self, cell: u16) -> f64 {
        let (mut uses, mut starts) = (0u32, 0u32);
        for (codes, n) in self.uses(cell) {
            uses += 1;
            if n == 0 || self.is_space(codes[n - 1]) {
                starts += 1;
            }
        }
        if uses == 0 { 0.0 } else { f64::from(starts) / f64::from(uses) }
    }

    fn char_at(&self, codes: &[u16], k: Option<usize>) -> Option<String> {
        let code = *codes.get(k?)?;
        if !draws(code) {
            return None;
        }
        self.chars.get(&(code & CELL_MASK)).cloned().flatten().filter(|c| c != " ")
    }

    /// How strongly a cell's uses read as a capital I rather than a lowercase l: a lowercase letter before it makes it
    /// l, capitals beside it I, standing alone or before an apostrophe I; at a word's start, a vowel after it makes it
    /// l (lever, lock), a consonant I (In, It, Ice).
    fn capital_votes(&self, cell: u16) -> i32 {
        let alpha = |c: &str| c.chars().next().is_some_and(char::is_alphabetic);
        let upper = |c: &str| c.chars().next().is_some_and(char::is_uppercase);
        let mut votes = 0;
        for (codes, n) in self.uses(cell) {
            let before = self.char_at(codes, n.checked_sub(1));
            let after = self.char_at(codes, Some(n + 1));
            match (&before, &after) {
                (Some(b), _) if alpha(b) => {
                    let capital_after = after.as_deref().is_none_or(|a| !alpha(a) || upper(a));
                    votes += if upper(b) && capital_after { 3 } else { -3 };
                }
                (_, None) => votes += 3,
                (_, Some(a)) if a == "'" => votes += 3,
                (_, Some(a)) if alpha(a) && upper(a) => votes += 3,
                (_, Some(a)) if alpha(a) => votes += if "aeiouy".contains(a.to_lowercase().as_str()) { -1 } else { 1 },
                _ => {}
            }
        }
        votes
    }

    fn guess(&self, cell: u16, names: &[String]) -> String {
        if names.iter().collect::<BTreeSet<_>>() == ["I".to_string(), "l".to_string()].iter().collect() {
            return if self.capital_votes(cell) > 0 { "I" } else { "l" }.into();
        }
        let is_upper = |n: &String| n.chars().next().is_some_and(char::is_uppercase);
        let upper: Vec<&String> = names.iter().filter(|n| is_upper(n)).collect();
        if let Some(first) = upper.first() {
            let starts = self.uses(cell).all(|(codes, n)| n == 0 || self.is_space(codes[n - 1]));
            if starts {
                return (*first).clone();
            }
        }
        names.iter().find(|n| !is_upper(n)).unwrap_or(&names[0]).clone()
    }
}

/// Recognize the glyph cells a dialogue's texts use, on its atlas page (`width` texels a row).
///
/// # Errors
/// A choice names a cell the text does not use, or a character its picture cannot be.
pub fn recognize(
    streams: &[Vec<u16>],
    cells: &[[u8; 4]],
    page: &[u8],
    width: usize,
    table: &BTreeMap<String, Vec<String>>,
    chosen: &BTreeMap<u16, String>,
) -> Result<Recognized, String> {
    let used: BTreeSet<u16> = streams.iter().flatten().filter(|c| draws(**c)).map(|c| c & CELL_MASK).collect();
    let mut payload = Payload { streams, hashes: BTreeMap::new(), chars: BTreeMap::new() };
    for &cell in &used {
        let rect = cells.get(usize::from(cell)).copied().unwrap_or_default();
        if rect[2] == 0 || rect[3] == 0 {
            continue;
        }
        payload.hashes.insert(cell, hash(&crop(page, width, rect)));
    }
    for cell in chosen.keys() {
        if !payload.hashes.contains_key(cell) {
            return Err(format!("glyph choice for cell {cell}: the text does not use that cell"));
        }
    }
    let mut notes = Vec::new();
    let mut ambiguous: Vec<(u16, Vec<String>)> = Vec::new();
    for (&cell, h) in &payload.hashes {
        let names: Vec<String> = if h == "space" { vec![" ".into()] } else { table.get(h).cloned().unwrap_or_default() };
        let char = match (names.len(), chosen.get(&cell)) {
            (0, _) => {
                notes.push(Note::Unseen { cell, hash: h.clone() });
                None
            }
            (1, Some(c)) if *c != names[0] => {
                return Err(format!("glyph choice for cell {cell}: its picture is only {:?}, not {c:?}", names[0]));
            }
            (1, _) => Some(names[0].clone()),
            (_, Some(c)) if !names.contains(c) => {
                return Err(format!("glyph choice for cell {cell}: {c:?} is not one of {names:?}"));
            }
            (_, Some(c)) => Some(c.clone()),
            (_, None) => {
                ambiguous.push((cell, names));
                None
            }
        };
        payload.chars.insert(cell, char);
    }
    let mut guessed: BTreeMap<u16, String> = BTreeMap::new();
    for (cell, names) in &ambiguous {
        let guess = payload.guess(*cell, names);
        payload.chars.insert(*cell, Some(guess.clone()));
        guessed.insert(*cell, guess);
    }
    // Cells sharing a picture several characters share are those characters, one each: the unsettled ones take what
    // the settled ones leave, the cell whose uses read most as a capital taking it.
    let mut by_hash: BTreeMap<&String, Vec<u16>> = BTreeMap::new();
    for (cell, h) in &payload.hashes {
        if table.get(h).is_some_and(|n| n.len() > 1) {
            by_hash.entry(h).or_default().push(*cell);
        }
    }
    for (h, group) in &by_hash {
        if group.len() < 2 {
            continue;
        }
        let names = &table[*h];
        let taken: BTreeSet<&String> = group.iter().filter_map(|c| chosen.get(c)).collect();
        let mut left: Vec<&String> = names.iter().filter(|n| !taken.contains(n)).collect();
        left.sort_by_key(|n| !n.chars().next().is_some_and(char::is_uppercase));
        let i_or_l = names.iter().collect::<BTreeSet<_>>() == ["I".to_string(), "l".to_string()].iter().collect();
        let mut free: Vec<u16> = group.iter().copied().filter(|c| !chosen.contains_key(c)).collect();
        // Stable, as Python's sort: by the score, highest first.
        let score = |c: u16| if i_or_l { f64::from(payload.capital_votes(c)) } else { payload.starts_words(c) };
        let scores: BTreeMap<u16, f64> = free.iter().map(|c| (*c, score(*c))).collect();
        free.sort_by(|a, b| scores[b].total_cmp(&scores[a]));
        for (cell, char) in free.iter().zip(left) {
            payload.chars.insert(*cell, Some(char.clone()));
            if guessed.contains_key(cell) {
                guessed.insert(*cell, char.clone());
            }
        }
    }
    for (cell, names) in ambiguous {
        notes.push(Note::Ambiguous { cell, candidates: names, guessed: guessed[&cell].clone() });
    }
    let mut seen: BTreeMap<String, u16> = BTreeMap::new();
    let mut chars = BTreeMap::new();
    for (cell, char) in &payload.chars {
        let Some(char) = char else { continue };
        if let Some(also) = seen.get(char) {
            notes.push(Note::Duplicate { cell: *cell, char: char.clone(), also: *also });
        } else {
            seen.insert(char.clone(), *cell);
        }
        chars.insert(*cell, char.clone());
    }
    Ok(Recognized { chars, notes })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{Note, hash, recognize};
    use crate::layout::Picture;

    #[test]
    fn i_and_l_are_told_apart_by_their_uses() {
        // Cells 25 and 26 share a picture; "Il" reads... cell 25 alone before a space, cell 26 after a lowercase letter.
        let page = {
            let mut page = vec![0u8; 256 * 16];
            for y in 0..12 {
                page[y * 256 + 1] = 7;
                page[y * 256 + 10 + 1] = 7;
                page[y * 256 + 20] = 5;
            }
            page
        };
        let cells = {
            let mut c = vec![[0u8; 4]; 28];
            c[25] = [0, 0, 3, 12];
            c[26] = [10, 0, 3, 12];
            c[27] = [20, 0, 1, 12];
            c
        };
        let a = Picture { width: 1, height: 12, values: vec![5; 12] };
        let shared = Picture { width: 3, height: 12, values: (0..36).map(|i| if i % 3 == 1 { 7 } else { 0 }).collect() };
        let table: BTreeMap<String, Vec<String>> =
            [(hash(&shared), vec!["I".into(), "l".into()]), (hash(&a), vec!["a".into()])].into();
        // "I" then "al": cell 25 stands alone, cell 26 follows a lowercase letter.
        let streams = vec![vec![25, 0xFFFE, 27, 26]];
        let found = recognize(&streams, &cells, &page, 256, &table, &BTreeMap::new()).expect("recognized");
        assert_eq!(found.chars[&25], "I");
        assert_eq!(found.chars[&26], "l");
        assert!(found.notes.iter().all(|n| matches!(n, Note::Ambiguous { .. })));
    }
}
