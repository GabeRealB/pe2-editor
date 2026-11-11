//! What was done to a project's files through the folder it is served at, as records of the log: each operation
//! in the order it was made, and what became of it - so that a process that starts again finds what was in
//! transit, and takes it up where it was. A file in transit is known by the position of the record that began it
//! (`Began`), which no other record has; its bytes are the data records that say that number.

use crate::Log;
use std::io;

/// The kinds of records, after the two of a file's bytes (`content::DATA`, `content::FILL`).
mod kind {
    pub const BEGAN: u8 = 3;
    pub const TRUNCATED: u8 = 4;
    pub const REMOVED: u8 = 5;
    pub const MADE_FOLDER: u8 = 6;
    pub const RENAMED: u8 = 7;
    pub const TAKEN: u8 = 8;
    pub const DONE: u8 = 9;
    pub const REFUSED: u8 = 10;
    pub const ENDED: u8 = 11;
}

/// One thing done, or what became of one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    /// A file in transit begins at a path: the record's position is the file's number. `emptied`, it begins as a
    /// change of its own - a file made, or one written anew; otherwise it begins as the file there is, whose bytes
    /// follow as a fill, and changes by what is written to it after.
    Began { path: String, emptied: bool },
    /// A file in transit was made this long.
    Truncated { file: u64, len: u64 },
    /// A path was removed.
    Removed { path: String },
    /// A folder was made.
    MadeFolder { path: String },
    /// A path was renamed.
    Renamed { from: String, to: String },
    /// The project has a file in transit as it was when the log ended at `upto`: what was written to it after is
    /// not the project's yet.
    Taken { file: u64, upto: u64 },
    /// The project was told what the record at this position says - a removal, a folder, a rename.
    Done { of: u64 },
    /// The project did not take what the record at this position says, and never will.
    Refused { of: u64 },
    /// Serving ended with nothing in transit: what is before this is nobody's.
    Ended,
}

fn text(out: &mut Vec<u8>, text: &str) {
    out.extend_from_slice(&u32::try_from(text.len()).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(text.as_bytes());
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn number(&mut self) -> Option<u64> {
        let (number, rest) = self.0.split_first_chunk::<8>()?;
        self.0 = rest;
        Some(u64::from_le_bytes(*number))
    }

    fn flag(&mut self) -> Option<bool> {
        let (flag, rest) = self.0.split_first()?;
        self.0 = rest;
        Some(*flag != 0)
    }

    fn text(&mut self) -> Option<String> {
        let (len, rest) = self.0.split_first_chunk::<4>()?;
        let len = usize::try_from(u32::from_le_bytes(*len)).ok()?;
        let (text, rest) = rest.split_at_checked(len)?;
        self.0 = rest;
        String::from_utf8(text.to_vec()).ok()
    }
}

impl Op {
    /// Append it to the log: where it is.
    ///
    /// # Errors
    /// The log cannot take it.
    pub fn append(&self, log: &Log) -> io::Result<u64> {
        let mut body = Vec::new();
        let kind = match self {
            Op::Began { path, emptied } => {
                body.push(u8::from(*emptied));
                text(&mut body, path);
                kind::BEGAN
            }
            Op::Truncated { file, len } => {
                body.extend_from_slice(&file.to_le_bytes());
                body.extend_from_slice(&len.to_le_bytes());
                kind::TRUNCATED
            }
            Op::Removed { path } => {
                text(&mut body, path);
                kind::REMOVED
            }
            Op::MadeFolder { path } => {
                text(&mut body, path);
                kind::MADE_FOLDER
            }
            Op::Renamed { from, to } => {
                text(&mut body, from);
                text(&mut body, to);
                kind::RENAMED
            }
            Op::Taken { file, upto } => {
                body.extend_from_slice(&file.to_le_bytes());
                body.extend_from_slice(&upto.to_le_bytes());
                kind::TAKEN
            }
            Op::Done { of } => {
                body.extend_from_slice(&of.to_le_bytes());
                kind::DONE
            }
            Op::Refused { of } => {
                body.extend_from_slice(&of.to_le_bytes());
                kind::REFUSED
            }
            Op::Ended => kind::ENDED,
        };
        log.append(kind, &[&body])
    }

    /// The operation a record is; none where it is of another kind - a file's bytes - or not whole.
    #[must_use]
    pub fn of(kind: u8, body: &[u8]) -> Option<Op> {
        let mut body = Reader(body);
        Some(match kind {
            kind::BEGAN => Op::Began { emptied: body.flag()?, path: body.text()? },
            kind::TRUNCATED => Op::Truncated { file: body.number()?, len: body.number()? },
            kind::REMOVED => Op::Removed { path: body.text()? },
            kind::MADE_FOLDER => Op::MadeFolder { path: body.text()? },
            kind::RENAMED => Op::Renamed { from: body.text()?, to: body.text()? },
            kind::TAKEN => Op::Taken { file: body.number()?, upto: body.number()? },
            kind::DONE => Op::Done { of: body.number()? },
            kind::REFUSED => Op::Refused { of: body.number()? },
            kind::ENDED => Op::Ended,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Op;
    use crate::log::{Config, Log};

    #[test]
    fn what_is_appended_is_read_as_it_was() {
        let dir = std::env::temp_dir().join(format!("pe2-sfs-ops-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let log = Log::open(&dir, Config { segment: 1 << 16, budget: 1 << 20 }).unwrap();
        let ops = [
            Op::Began { path: "a/b.txt".into(), emptied: true },
            Op::Began { path: "c".into(), emptied: false },
            Op::Truncated { file: 7, len: 12 },
            Op::Removed { path: "gone".into() },
            Op::MadeFolder { path: "made/here".into() },
            Op::Renamed { from: "old name".into(), to: "näme".into() },
            Op::Taken { file: 7, upto: 99 },
            Op::Done { of: 3 },
            Op::Refused { of: 4 },
            Op::Ended,
        ];
        for op in &ops {
            op.append(&log).unwrap();
        }
        let read: Vec<Op> = log.records(0).map(|r| r.unwrap()).filter_map(|r| Op::of(r.kind, &r.body)).collect();
        assert_eq!(read, ops);
        assert_eq!(Op::of(1, b"bytes of a file"), None, "a file's bytes are no operation");
        drop(log);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
