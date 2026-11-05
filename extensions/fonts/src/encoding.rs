//! An encoding: which character each code of a text stands for. The game's texts are codes, and a font face draws a
//! code by the glyph of its character; a string table stores its characters as their codes. An encoding is built from
//! a base - Windows-1252, or none - read from `first`, and the characters it sets on top (`characters`, by code), which
//! is how the UI's controller buttons sit at 0x7B-0x7E where Windows-1252 has `{ | } ~`, or a sequence of characters
//! from `first` (`sequence`: the caption font's `A`-`Z`, `a`-`z`, ...).

use std::collections::BTreeMap;

/// Windows-1252's characters at 0x80-0x9F; the five codes it leaves undefined are none. Elsewhere it is Latin-1.
const WINDOWS_1252_HIGH: [Option<char>; 32] = [
    Some('€'),
    None,
    Some('‚'),
    Some('ƒ'),
    Some('„'),
    Some('…'),
    Some('†'),
    Some('‡'),
    Some('ˆ'),
    Some('‰'),
    Some('Š'),
    Some('‹'),
    Some('Œ'),
    None,
    Some('Ž'),
    None,
    None,
    Some('‘'),
    Some('’'),
    Some('“'),
    Some('”'),
    Some('•'),
    Some('–'),
    Some('—'),
    Some('˜'),
    Some('™'),
    Some('š'),
    Some('›'),
    Some('œ'),
    None,
    Some('ž'),
    Some('Ÿ'),
];

/// A Windows-1252 code's character, none for a control code or one it leaves undefined.
#[must_use]
pub fn windows_1252(code: u8) -> Option<char> {
    match code {
        0x00..0x20 | 0x7F => None,
        0x80..0xA0 => WINDOWS_1252_HIGH[usize::from(code - 0x80)],
        _ => Some(char::from(code)),
    }
}

/// An encoding: a character for each code it defines.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Encoding {
    pub chars: BTreeMap<u32, char>,
}

impl Encoding {
    /// The encoding an `encoding` node's settings say: `base` (`windows-1252` or `none`), read from `first` (0 where
    /// unset) to `last` (0xFF), then `sequence` placed from `first`, then `characters` by code (`"0x7B" = "○"`).
    ///
    /// # Errors
    /// A base it does not know, or a code or character it cannot read.
    pub fn of(settings: &toml::Table) -> Result<Encoding, String> {
        let number = |key: &str, default: i64| settings.get(key).and_then(toml::Value::as_integer).unwrap_or(default);
        let first = u32::try_from(number("first", 0)).map_err(|_| "first is negative")?;
        let last = u32::try_from(number("last", 0xFF)).map_err(|_| "last is negative")?;
        let mut chars = BTreeMap::new();
        match settings.get("base").and_then(toml::Value::as_str).unwrap_or("none") {
            "windows-1252" => {
                for code in first..=last.min(0xFF) {
                    if let Some(c) = u8::try_from(code).ok().and_then(windows_1252) {
                        chars.insert(code, c);
                    }
                }
            }
            "none" => {}
            other => return Err(format!("no base {other} (windows-1252, none)")),
        }
        if let Some(sequence) = settings.get("sequence").and_then(toml::Value::as_str) {
            for (code, c) in (first..).zip(sequence.chars()) {
                chars.insert(code, c);
            }
        }
        for (code, c) in settings.get("characters").and_then(toml::Value::as_table).into_iter().flatten() {
            let code = parse_code(code)?;
            let c = single(c.as_str().ok_or_else(|| format!("{code:#X}: not a character"))?)
                .ok_or_else(|| format!("{code:#X}: not one character"))?;
            chars.insert(code, c);
        }
        Ok(Encoding { chars })
    }

    #[must_use]
    pub fn char_of(&self, code: u32) -> Option<char> {
        self.chars.get(&code).copied()
    }

    /// The codes standing for a character, lowest first.
    #[must_use]
    pub fn codes_of(&self, c: char) -> Vec<u32> {
        self.chars.iter().filter(|(_, d)| **d == c).map(|(code, _)| *code).collect()
    }
}

/// A code as a key writes it: `0x7B`, or decimal.
///
/// # Errors
/// Neither.
pub fn parse_code(text: &str) -> Result<u32, String> {
    let parsed = match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(hex) => u32::from_str_radix(hex, 16),
        None => text.parse(),
    };
    parsed.map_err(|_| format!("{text} is not a code"))
}

/// The one character of a text.
#[must_use]
pub fn single(text: &str) -> Option<char> {
    let mut chars = text.chars();
    let c = chars.next()?;
    chars.next().is_none().then_some(c)
}

#[cfg(test)]
mod tests {
    use super::{Encoding, windows_1252};

    #[test]
    fn the_ui_encoding_is_windows_1252_with_the_buttons() {
        let settings: toml::Table = toml::from_str(
            "base = \"windows-1252\"\nfirst = 0x20\n[characters]\n\"0x7B\" = \"○\"\n\"0x7C\" = \"△\"\n\"0x7D\" = \"✕\"\n\"0x7E\" = \"□\"\n",
        )
        .unwrap();
        let ui = Encoding::of(&settings).unwrap();
        assert_eq!(ui.char_of(0x41), Some('A'));
        assert_eq!(ui.char_of(0x7B), Some('○'));
        assert_eq!(ui.char_of(0x8C), Some('Œ'));
        assert_eq!(ui.char_of(0x81), None, "undefined in Windows-1252");
        assert_eq!(ui.char_of(0x1F), None, "before first");
        assert_eq!(ui.codes_of('{'), Vec::<u32>::new(), "the button took its place");
        assert_eq!(windows_1252(0xE9), Some('é'));
    }

    #[test]
    fn a_sequence_places_its_characters_from_first() {
        let caption = Encoding::of(&toml::from_str("sequence = \"ABC\"").unwrap()).unwrap();
        assert_eq!((caption.char_of(0), caption.char_of(2), caption.char_of(3)), (Some('A'), Some('C'), None));
    }
}
