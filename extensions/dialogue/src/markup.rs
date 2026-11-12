//! A dialogue's text stream as text (`formats::cap`): its glyphs as the characters they draw, its controls as tokens in
//! braces. Palette and reveal delay are states, written where they change.
//!
//! - a character, or `{glyph <token>}` for a glyph whose token is no single character (or a brace);
//! - `{cell N}`: a cell drawn by its number, where no character is known for it;
//! - `\n` ends a line (`FFFE`), `{spacer}` (`FFFD`);
//! - `{view N}`, `{view N logical}`, `{view N hide}`: the view changes while the line is drawn;
//! - `{choice confirm|silent|cursor N}`: the line ends in a reply; `{icon N}`; `{code XXXX}` any other control;
//! - `{palette N}`, `{delay N}`: the palette and reveal delay of the glyphs after it.

use std::fmt::Write as _;

pub const LINE: u16 = 0xFFFE;
pub const SPACER: u16 = 0xFFFD;
const VIEW_MASK: u16 = 0x9F00;
const VIEW: u16 = 0x8000;
const VIEW_LOGICAL: u16 = 0x2000;
const VIEW_HIDE_ACTORS: u16 = 0x4000;
const ICON: u16 = 0x8400;
const CHOICES: [(u16, &str); 3] = [(0x8100, "confirm"), (0x8200, "silent"), (0x8300, "cursor")];
pub const CELL_MASK: u16 = 0x3FF;
const PALETTE_SHIFT: u16 = 10;
const DELAY_SHIFT: u16 = 12;

/// What markup holds: a glyph by its token or a cell by its number, each with its palette and delay, or a control code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    Glyph { token: String, palette: u16, delay: u16 },
    Cell { cell: u16, palette: u16, delay: u16 },
    Code(u16),
}

impl Item {
    /// Its code, given the cell its glyph is drawn from.
    #[must_use]
    pub fn code(&self, cell: u16) -> u16 {
        match self {
            Item::Glyph { palette, delay, .. } | Item::Cell { palette, delay, .. } => {
                cell | palette << PALETTE_SHIFT | delay << DELAY_SHIFT
            }
            Item::Code(code) => *code,
        }
    }
}

/// Whether a code draws a cell.
#[must_use]
pub fn draws(code: u16) -> bool {
    code < 0x8000
}

/// A stream's codes as markup, a cell's token by `token_of` (none: `{cell N}`).
#[must_use]
pub fn print(codes: &[u16], token_of: &dyn Fn(u16) -> Option<String>) -> String {
    let (mut out, mut palette, mut delay) = (String::new(), 0, 0);
    for &code in codes {
        if code == LINE {
            out.push('\n');
        } else if code == SPACER {
            out.push_str("{spacer}");
        } else if !draws(code) {
            let low = code & 0xFF;
            if code & VIEW_MASK == VIEW {
                let logical = if code & VIEW_LOGICAL != 0 { " logical" } else { "" };
                let hide = if code & VIEW_HIDE_ACTORS != 0 { " hide" } else { "" };
                { let _ = write!(out, "{{view {low}{logical}{hide}}}"); };
            } else if let Some((_, name)) = CHOICES.iter().find(|(family, _)| code & 0xFF00 == *family) {
                { let _ = write!(out, "{{choice {name} {low}}}"); };
            } else if code & 0xFF00 == ICON {
                { let _ = write!(out, "{{icon {low}}}"); };
            } else {
                { let _ = write!(out, "{{code {code:04X}}}"); };
            }
        } else {
            let (cell, p, d) = (code & CELL_MASK, (code >> PALETTE_SHIFT) & 3, (code >> DELAY_SHIFT) & 7);
            if p != palette {
                { let _ = write!(out, "{{palette {p}}}"); };
                palette = p;
            }
            if d != delay {
                { let _ = write!(out, "{{delay {d}}}"); };
                delay = d;
            }
            match token_of(cell) {
                None => { let _ = write!(out, "{{cell {cell}}}"); },
                Some(token) => {
                    let mut chars = token.chars();
                    match (chars.next(), chars.next()) {
                        (Some(ch), None) if ch != '{' && ch != '}' => out.push(ch),
                        _ => { let _ = write!(out, "{{glyph {token}}}"); },
                    }
                }
            }
        }
    }
    out
}

/// Markup as its items.
///
/// # Errors
/// A brace is not closed, or holds no token markup knows.
pub fn parse(text: &str) -> Result<Vec<Item>, String> {
    let (mut items, mut palette, mut delay) = (Vec::new(), 0u16, 0u16);
    let mut rest = text;
    let number = |s: &str, what: &str| s.trim().parse::<u16>().map_err(|_| format!("{{{what}}}: {s:?} is no number"));
    while let Some(ch) = rest.chars().next() {
        if ch == '\n' {
            items.push(Item::Code(LINE));
            rest = &rest[1..];
            continue;
        }
        if ch != '{' {
            items.push(Item::Glyph { token: ch.to_string(), palette, delay });
            rest = &rest[ch.len_utf8()..];
            continue;
        }
        let end = rest.find('}').ok_or_else(|| format!("an unclosed brace in {text:?}"))?;
        let inner = &rest[1..end];
        rest = &rest[end + 1..];
        let (word, arg) = inner.split_once(' ').unwrap_or((inner, ""));
        match word {
            "spacer" => items.push(Item::Code(SPACER)),
            "view" => {
                let mut words = arg.split_whitespace();
                let mut code = VIEW | number(words.next().unwrap_or(""), "view")?;
                for flag in words {
                    code |= match flag {
                        "logical" => VIEW_LOGICAL,
                        "hide" => VIEW_HIDE_ACTORS,
                        other => return Err(format!("{{view}}: no flag {other:?}")),
                    };
                }
                items.push(Item::Code(code));
            }
            "choice" => {
                let (family, key) = arg.split_once(' ').ok_or("{choice}: a family and a key")?;
                let found = CHOICES.iter().find(|(_, name)| *name == family).ok_or("{choice}: no such family")?;
                items.push(Item::Code(found.0 | number(key, "choice")?));
            }
            "icon" => items.push(Item::Code(ICON | number(arg, "icon")?)),
            "code" => items.push(Item::Code(u16::from_str_radix(arg, 16).map_err(|_| "{code}: no hex number")?)),
            "palette" => palette = number(arg, "palette")?,
            "delay" => delay = number(arg, "delay")?,
            "glyph" => items.push(Item::Glyph { token: arg.to_string(), palette, delay }),
            "cell" => items.push(Item::Cell { cell: number(arg, "cell")?, palette, delay }),
            other => return Err(format!("no markup {{{other}}}")),
        }
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::{Item, parse, print};

    #[test]
    fn markup_reads_back_as_written() {
        let codes = [0x19, 0x1A, 0xFFFE, 0x1B | 1 << 10, 0x8005 | 0x2000, 0xFFFD, 0x8203, 0x8401, 0x8800, 0x1E, 0x19 | 2 << 12];
        let token_of = |cell: u16| match cell {
            25 => Some("A".to_string()),
            26 => Some("{".to_string()),
            27 => Some("Aya".to_string()),
            _ => None,
        };
        let text = print(&codes, &token_of);
        assert_eq!(
            text,
            "A{glyph {}\n{palette 1}{glyph Aya}{view 5 logical}{spacer}{choice silent 3}{icon 1}{code 8800}{palette 0}{cell 30}{delay 2}A"
        );
        let items = parse(&text).expect("markup");
        let cell_of = |item: &Item| match item {
            Item::Glyph { token, .. } => match token.as_str() {
                "A" => 25,
                "{" => 26,
                _ => 27,
            },
            Item::Cell { cell, .. } => *cell,
            Item::Code(_) => 0,
        };
        let back: Vec<u16> = items.iter().map(|i| i.code(cell_of(i))).collect();
        assert_eq!(back, codes);
    }
}
