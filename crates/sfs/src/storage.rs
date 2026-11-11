//! What whoever holds a project writes to its files, as records of the log: said whole, and had on disk, before any
//! file is touched. A set of changes begins (`Began`), says each change - a file's new bytes, a file gone, a path
//! moved, a folder made or removed - and is whole once it says so (`Whole`): from then on the files can be written
//! at leisure, and written again from the log by whoever finds them not written - a process that died between, a
//! loss of power. A set that never came to be whole touched no file, and is nothing.

use crate::{Log, Record};
use std::io;

mod kind {
    pub const BEGAN: u8 = 32;
    pub const PUT: u8 = 33;
    pub const MORE: u8 = 34;
    pub const DELETE: u8 = 35;
    pub const MOVE: u8 = 36;
    pub const MAKE_DIR: u8 = 37;
    pub const REMOVE_DIR: u8 = 38;
    pub const WHOLE: u8 = 39;
    pub const ABORTED: u8 = 40;
}

/// The most of a file one record holds: a longer one goes on in the records after it (`More`).
const MOST: usize = 4 << 20;

/// One record of a set of changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stored {
    /// A set of changes begins: what was said before it and never came to be whole is nothing.
    Began,
    /// A file is these bytes - or begins with them, where `More` follows.
    Put { path: String, bytes: Vec<u8> },
    /// The file put before goes on with these bytes.
    More { bytes: Vec<u8> },
    /// A file is gone.
    Delete { path: String },
    /// A file or a folder is moved.
    Move { from: String, to: String },
    /// A folder is made.
    MakeDir { path: String },
    /// A folder is removed, where it is empty.
    RemoveDir { path: String },
    /// The set that began at this position is whole: its files are to be as it says. What it is, as a person named
    /// it.
    Whole { began: u64, label: String },
    /// The set that began at this position was given up after it was whole - a file could not be written - and
    /// what it had written was put back: it is not to be written again.
    Aborted { began: u64 },
}

fn text(out: &mut Vec<u8>, text: &str) {
    out.extend_from_slice(&u32::try_from(text.len()).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(text.as_bytes());
}

fn read_text(body: &mut &[u8]) -> Option<String> {
    let (len, rest) = body.split_first_chunk::<4>()?;
    let (text, rest) = rest.split_at_checked(usize::try_from(u32::from_le_bytes(*len)).ok()?)?;
    *body = rest;
    String::from_utf8(text.to_vec()).ok()
}

fn read_number(body: &mut &[u8]) -> Option<u64> {
    let (number, rest) = body.split_first_chunk::<8>()?;
    *body = rest;
    Some(u64::from_le_bytes(*number))
}

/// Say that a set of changes begins: where.
///
/// # Errors
/// The log cannot take it.
pub fn began(log: &Log) -> io::Result<u64> {
    log.append(kind::BEGAN, &[])
}

/// Say that a file is these bytes, in as many records as they take.
///
/// # Errors
/// The log cannot take them.
pub fn put(log: &Log, path: &str, bytes: &[u8]) -> io::Result<()> {
    let mut head = Vec::new();
    text(&mut head, path);
    let most = MOST.min(usize::try_from(log.config().segment / 4).unwrap_or(MOST));
    let mut parts = bytes.chunks(most);
    log.append(kind::PUT, &[&head, parts.next().unwrap_or_default()])?;
    for part in parts {
        log.append(kind::MORE, &[part])?;
    }
    Ok(())
}

impl Stored {
    /// Append it to the log: where it is. A file's bytes are said with `put`, which this is for one that fits a
    /// record.
    ///
    /// # Errors
    /// The log cannot take it.
    pub fn append(&self, log: &Log) -> io::Result<u64> {
        let mut body = Vec::new();
        let kind = match self {
            Stored::Began => kind::BEGAN,
            Stored::Put { path, bytes } => {
                text(&mut body, path);
                return log.append(kind::PUT, &[&body, bytes]);
            }
            Stored::More { bytes } => return log.append(kind::MORE, &[bytes]),
            Stored::Delete { path } => {
                text(&mut body, path);
                kind::DELETE
            }
            Stored::Move { from, to } => {
                text(&mut body, from);
                text(&mut body, to);
                kind::MOVE
            }
            Stored::MakeDir { path } => {
                text(&mut body, path);
                kind::MAKE_DIR
            }
            Stored::RemoveDir { path } => {
                text(&mut body, path);
                kind::REMOVE_DIR
            }
            Stored::Whole { began, label } => {
                body.extend_from_slice(&began.to_le_bytes());
                text(&mut body, label);
                kind::WHOLE
            }
            Stored::Aborted { began } => {
                body.extend_from_slice(&began.to_le_bytes());
                kind::ABORTED
            }
        };
        log.append(kind, &[&body])
    }

    /// What a record says of a set of changes; none where it is of another kind, or not whole.
    #[must_use]
    pub fn of(record: &Record) -> Option<Stored> {
        let mut body: &[u8] = &record.body;
        Some(match record.kind {
            kind::BEGAN => Stored::Began,
            kind::PUT => Stored::Put { path: read_text(&mut body)?, bytes: body.to_vec() },
            kind::MORE => Stored::More { bytes: body.to_vec() },
            kind::DELETE => Stored::Delete { path: read_text(&mut body)? },
            kind::MOVE => Stored::Move { from: read_text(&mut body)?, to: read_text(&mut body)? },
            kind::MAKE_DIR => Stored::MakeDir { path: read_text(&mut body)? },
            kind::REMOVE_DIR => Stored::RemoveDir { path: read_text(&mut body)? },
            kind::WHOLE => Stored::Whole { began: read_number(&mut body)?, label: read_text(&mut body)? },
            kind::ABORTED => Stored::Aborted { began: read_number(&mut body)? },
            _ => return None,
        })
    }
}

/// The sets of changes the log holds from a position on that are whole and were not given up, each as what it is
/// named and its changes in order, a file's bytes joined: what is to be written again where it may not have been.
///
/// # Errors
/// The log cannot be read.
pub fn whole(log: &Log, from: u64) -> io::Result<Vec<(String, Vec<Stored>)>> {
    let mut sets: Vec<(u64, String, Vec<Stored>)> = Vec::new();
    let mut open: Option<(u64, Vec<Stored>)> = None;
    for record in log.records(log.from(from)) {
        let record = record?;
        let Some(stored) = Stored::of(&record) else { continue };
        match stored {
            Stored::Began => open = Some((record.position, Vec::new())),
            Stored::Whole { began, label } => {
                if let Some((at, changes)) = open.take().filter(|(at, _)| *at == began) {
                    sets.push((at, label, changes));
                }
            }
            Stored::Aborted { began } => sets.retain(|(at, ..)| *at != began),
            Stored::More { bytes } => {
                if let Some((_, changes)) = &mut open
                    && let Some(Stored::Put { bytes: so_far, .. }) = changes.last_mut()
                {
                    so_far.extend_from_slice(&bytes);
                }
            }
            change => {
                if let Some((_, changes)) = &mut open {
                    changes.push(change);
                }
            }
        }
    }
    Ok(sets.into_iter().map(|(_, label, changes)| (label, changes)).collect())
}

#[cfg(test)]
mod tests {
    use super::{Stored, began, put, whole};
    use crate::log::{Config, Log};

    #[test]
    fn sets_that_are_whole_are_found_again_and_the_others_are_nothing() {
        let dir = std::env::temp_dir().join(format!("pe2-sfs-storage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let log = Log::open(&dir, Config { segment: 1 << 16, budget: 1 << 22 }).unwrap();
        // One whole, with a file longer than a record and a record of another kind among it.
        let lengthy: Vec<u8> = (0..70_000u32).map(|n| (n % 251) as u8).collect();
        let first = began(&log).unwrap();
        put(&log, "a/long.bin", &lengthy).unwrap();
        log.append(1, &[b"bytes a program wrote, said by another process meanwhile"]).unwrap();
        Stored::Move { from: "a".into(), to: "b".into() }.append(&log).unwrap();
        Stored::Whole { began: first, label: "the first".into() }.append(&log).unwrap();
        // One cut short: its process gone before it was whole.
        began(&log).unwrap();
        put(&log, "never.txt", b"x").unwrap();
        // One whole and given up; one whole after it.
        let third = began(&log).unwrap();
        Stored::Delete { path: "gone".into() }.append(&log).unwrap();
        Stored::Whole { began: third, label: "given up".into() }.append(&log).unwrap();
        Stored::Aborted { began: third }.append(&log).unwrap();
        let fourth = began(&log).unwrap();
        Stored::MakeDir { path: "made".into() }.append(&log).unwrap();
        Stored::RemoveDir { path: "emptied".into() }.append(&log).unwrap();
        Stored::Whole { began: fourth, label: "the last".into() }.append(&log).unwrap();
        let found = whole(&log, 0).unwrap();
        assert_eq!(
            found,
            [
                (
                    "the first".to_string(),
                    vec![
                        Stored::Put { path: "a/long.bin".into(), bytes: lengthy },
                        Stored::Move { from: "a".into(), to: "b".into() }
                    ]
                ),
                (
                    "the last".to_string(),
                    vec![Stored::MakeDir { path: "made".into() }, Stored::RemoveDir { path: "emptied".into() }]
                ),
            ]
        );
        assert_eq!(whole(&log, fourth).unwrap().len(), 1, "from a position on");
        drop(log);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
