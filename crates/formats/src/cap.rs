//! CAP dialogue (`CAP2`): a room's script of lines and scene steps, the data resource of its bundle.
//!
//! A file is its header, its glyph cells, a text stream for each record (and, in about half the retail files, an
//! unused empty one after them), the sequence table - each command's slot, its records' slots and a terminator - and
//! the command index. This module reads a file into what it holds, every byte of a command and a record kept, and lays
//! one out again; what the layout implies (offsets, counts, terminators, the index) is not kept. A file not laid out as
//! `write` lays it out is refused: what is read is what is written back.

use std::fmt::Write as _;

pub const MAGIC: &[u8; 4] = b"CAP2";
/// The header's second word: 8 in every file, never read.
const FIELD_4: i32 = 8;
const HEADER: usize = 0x14;
const SLOT: usize = 12;
/// A terminator's text reference.
const END_REF: u32 = 0xFFFF_FFFF;
/// A text stream's end.
pub const END: u16 = 0xFFFF;

/// A dialogue: its glyph cells, its commands by number, and whether an empty text stream follows the records'.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cap {
    /// `[u, v, w, h]` of each cell in the dialogue's atlas: speaker plates by speaker number, then glyphs.
    pub cells: Vec<[u8; 4]>,
    /// The commands, a command's number its place; none where a number has no command.
    pub commands: Vec<Option<Command>>,
    pub trailing_empty_text: bool,
}

/// A command: its twelve bytes (`CapCommand`) and its records.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Command {
    pub bytes: [u8; 12],
    pub records: Vec<Record>,
}

/// A record: its first eight bytes (`CapSequenceRecord`, its text reference made of where its text is) and its text
/// stream's codes, without the end code.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Record {
    pub bytes: [u8; 8],
    pub text: Vec<u16>,
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// The codes of the text stream at an offset, without its end code.
fn codes_at(data: &[u8], mut at: usize) -> Vec<u16> {
    let mut codes = Vec::new();
    while let Some(code) = u16_at(data, at) {
        if code == END {
            break;
        }
        codes.push(code);
        at += 2;
    }
    codes
}

fn usize_of(value: u32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

/// Read a dialogue.
///
/// # Errors
/// It is no CAP file, it is cut short, or it is not laid out as `write` would lay it out.
pub fn parse(data: &[u8]) -> Result<Cap, String> {
    let short = || "the file is cut short".to_string();
    if data.get(..4) != Some(MAGIC.as_slice()) {
        return Err("not a CAP2 file".into());
    }
    let glyphs = usize_of(u32_at(data, 0x8).ok_or_else(short)?);
    let sequences = usize_of(u32_at(data, 0xC).ok_or_else(short)?);
    let commands_at = usize_of(u32_at(data, 0x10).ok_or_else(short)?);
    let index_count = u32_at(data, commands_at).ok_or_else(short)?;
    let index: Vec<usize> = (0..usize_of(index_count))
        .map(|i| u32_at(data, commands_at + 4 + 4 * i).map(usize_of).ok_or_else(short))
        .collect::<Result<_, _>>()?;
    // The slots: a command's, then its records' until a terminator, then the next command's.
    let mut commands: Vec<(usize, Command)> = Vec::new();
    let mut refs: Vec<u32> = Vec::new();
    let (mut at, mut expect_command) = (sequences + 4, true);
    while at + SLOT <= commands_at {
        let slot = data.get(at..at + SLOT).ok_or_else(short)?;
        if expect_command {
            let mut bytes = [0; 12];
            bytes.copy_from_slice(slot);
            commands.push((at, Command { bytes, records: Vec::new() }));
            expect_command = false;
        } else {
            let reference = u32_at(slot, 8).ok_or_else(short)?;
            if reference == END_REF {
                expect_command = true;
            } else {
                let mut bytes = [0; 8];
                bytes.copy_from_slice(&slot[..8]);
                let (_, command) = commands.last_mut().ok_or("a record before any command")?;
                command.records.push(Record { bytes, text: codes_at(data, usize_of(reference)) });
                refs.push(reference);
            }
        }
        at += SLOT;
    }
    // The cells: from the header to the first text stream.
    let text_start = refs.iter().map(|&r| usize_of(r)).min().unwrap_or(sequences);
    let cells: Vec<[u8; 4]> = (glyphs..text_start)
        .step_by(4)
        .map(|o| data.get(o..o + 4).map(|c| [c[0], c[1], c[2], c[3]]).ok_or_else(short))
        .collect::<Result<_, _>>()?;
    // Each command by its number: the index's entries are their slots' offsets.
    let length = index.iter().rposition(|&e| e != 0).map_or(0, |highest| highest + 1);
    let mut numbered: Vec<Option<Command>> = vec![None; length];
    for (number, &entry) in index.iter().enumerate().filter(|(_, e)| **e != 0) {
        let found = commands.iter().find(|(offset, _)| *offset == entry).ok_or("an index entry names no command")?;
        numbered[number] = Some(found.1.clone());
    }
    let records: usize = commands.iter().map(|(_, c)| c.records.len()).sum();
    // An empty stream after the records' is the one thing the layout cannot imply.
    let streams_end = sequences;
    let after_records = refs
        .iter()
        .max()
        .map_or(text_start, |&last| usize_of(last) + 2 * codes_at(data, usize_of(last)).len() + 2);
    let trailing_empty_text = records > 0 && after_records + 2 == streams_end && u16_at(data, after_records) == Some(END);
    let cap = Cap { cells, commands: numbered, trailing_empty_text };
    let rebuilt = write(&cap);
    if rebuilt != data {
        let at = rebuilt.iter().zip(data).position(|(a, b)| a != b).unwrap_or(rebuilt.len().min(data.len()));
        let mut why = String::new();
        let _ = write!(why, "not laid out as written: they differ from byte {at:#x} ({} written, {} read)", rebuilt.len(), data.len());
        return Err(why);
    }
    Ok(cap)
}

/// Lay a dialogue out: header, cells, a text stream a record (and the trailing empty one), the sequence table with a
/// terminator after each command's records, the command index.
#[must_use]
pub fn write(cap: &Cap) -> Vec<u8> {
    let mut texts: Vec<u8> = Vec::new();
    let text_at = HEADER + 4 * cap.cells.len();
    let mut text_offsets: Vec<u32> = Vec::new();
    let present = || cap.commands.iter().flatten();
    for record in present().flat_map(|c| &c.records) {
        text_offsets.push(u32::try_from(text_at + texts.len()).unwrap_or(u32::MAX));
        for code in &record.text {
            texts.extend(code.to_le_bytes());
        }
        texts.extend(END.to_le_bytes());
    }
    if cap.trailing_empty_text {
        texts.extend(END.to_le_bytes());
    }
    let sequences = text_at + texts.len();
    let mut slots: Vec<u8> = Vec::new();
    let mut index: Vec<u32> = vec![0; cap.commands.len().max(1)];
    let mut count: usize = 0;
    let mut texts_used = text_offsets.iter();
    for (number, command) in cap.commands.iter().enumerate() {
        let Some(command) = command else { continue };
        index[number] = u32::try_from(sequences + 4 + slots.len()).unwrap_or(u32::MAX);
        slots.extend(command.bytes);
        for record in &command.records {
            slots.extend(record.bytes);
            slots.extend(texts_used.next().copied().unwrap_or(END_REF).to_le_bytes());
            count += 1;
        }
        slots.extend([0; 8]);
        slots.extend(END_REF.to_le_bytes());
        count += 1;
    }
    let commands_at = sequences + 4 + slots.len();
    let word = |v: usize| u32::try_from(v).unwrap_or(u32::MAX).to_le_bytes();
    let mut out = Vec::with_capacity(commands_at + 4 + 4 * index.len());
    out.extend(MAGIC);
    out.extend(FIELD_4.to_le_bytes());
    out.extend(word(HEADER));
    out.extend(word(sequences));
    out.extend(word(commands_at));
    for cell in &cap.cells {
        out.extend(cell);
    }
    out.extend(texts);
    out.extend(i16::try_from(count).unwrap_or(i16::MAX).to_le_bytes());
    out.extend(i16::try_from(SLOT).unwrap_or(0).to_le_bytes());
    out.extend(slots);
    out.extend(i32::try_from(index.len()).unwrap_or(i32::MAX).to_le_bytes());
    for entry in index {
        out.extend(entry.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{Cap, Command, Record, parse, write};

    fn line(key: u8, text: &[u16]) -> Record {
        Record { bytes: [1, 0, 30, 0, 1, key, 0, 30], text: text.to_vec() }
    }

    #[test]
    fn a_dialogue_reads_back_as_written() {
        let cap = Cap {
            cells: vec![[0, 0, 24, 11], [24, 0, 7, 12], [31, 0, 6, 12]],
            commands: vec![
                None,
                Some(Command { bytes: [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], records: vec![line(0, &[25, 26, 0xFFFE])] }),
                None,
                Some(Command {
                    bytes: [2, 0, 1, 0x56, 0, 0, 1, 0, 0, 0, 0, 0],
                    records: vec![line(0, &[26]), line(1, &[25]), Record { bytes: [5, 0x80, 0, 0, 1, 1, 0, 0], text: vec![] }],
                }),
            ],
            trailing_empty_text: true,
        };
        let bytes = write(&cap);
        assert_eq!(&bytes[..4], b"CAP2");
        assert_eq!(parse(&bytes), Ok(cap.clone()));
        let without = Cap { trailing_empty_text: false, ..cap };
        assert_eq!(parse(&write(&without)), Ok(without));
    }

    #[test]
    fn an_empty_dialogue_has_an_index_of_one() {
        let empty = Cap::default();
        let bytes = write(&empty);
        assert_eq!(bytes.len(), 0x14 + 4 + 8);
        assert_eq!(parse(&bytes), Ok(empty));
    }

    #[test]
    fn a_file_laid_out_otherwise_is_refused() {
        let mut bytes = write(&Cap { commands: vec![None, Some(Command::default())], ..Cap::default() });
        bytes.push(0);
        assert!(parse(&bytes).is_err());
    }
}
