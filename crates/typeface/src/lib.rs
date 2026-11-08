//! A font face as the game draws it: glyphs of 4-bit art, the UFO 3 font a font editor opens made of them and read back
//! exactly (`face`), and outlines drawn into art (`raster`). Shared by the extensions whose assets are faces: the fonts
//! (`pe2.fonts`) and the dialogues' (`pe2.dialogue`).

pub mod face;
pub mod raster;

use std::collections::BTreeMap;
use std::sync::OnceLock;

/// A texel no glyph paints.
pub const TRANSPARENT: u8 = 0xFF;

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
